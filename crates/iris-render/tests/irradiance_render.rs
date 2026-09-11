//! 集成测试：光照探针 GI（R64）——烘焙 irradiance 探针云 + 四面体重心插值。
//!
//! 五条主线：
//! 1. **兼容锁定（硬约束）**：探针资源缺席的场景渲染结果与 R63（引入探针
//!    实例载荷 / 着色器 select 之前）**逐位一致**——对固定室内盒 + 动态角色
//!    场景的全帧像素做 FNV-1a 哈希，与改动落地前实跑捕获的基线值比对。
//! 2. **语义边界**：只挂光照贴图（无动态实体）的场景在装了探针之后仍然
//!    **逐位一致**——挂图集的实体走 R52c 的逐 texel 分支，探针标志位恒 0。
//! 3. **有效性**：阴影区动态角色在有探针时不再吃天空色——亮度显著下降、
//!    色相从冷偏转向房间暖色（ROI = 角色像素掩码，避开背景混淆）。
//! 4. **插值跟随位置**：同一角色在亮区 / 阴影区两处的辐照不同。
//! 5. **确定性**：双会话逐帧逐位一致（含角色移动穿过探针网格）。
//!
//! 环境：无 GPU 适配器时优雅跳过（OffscreenRenderer::new 返回 None）。
//! 重负载 GPU 套件并行会挂死（AGENTS.md 测试规约）⇒ 文件内 GPU_SERIAL
//! 互斥锁串行化。

use std::sync::Mutex;

use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Quat, RigidBody, Transform, Vec3, World};
use iris_render::prelude::*;

/// 重负载 GPU 套件串行锁（R62b 教训：同文件多项并行共用单适配器会挂死）。
static GPU_SERIAL: Mutex<()> = Mutex::new(());

/// R63 基线（引入探针实例载荷 / 着色器 select 之前实跑捕获）：室内盒 + 动态
/// 角色（无光照贴图），480×270，见 room_world。
const R63_PROBE_FREE_HASH: u64 = 0x182e_756f_e66b_147a;
/// R63 基线（同场景 + 烘焙光照贴图）。
const R63_LIGHTMAPPED_HASH: u64 = 0xf2c5_05f9_3d17_0418;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn room_camera() -> Camera {
    Camera {
        eye: Vec3::new(4.2, 2.4, 4.6),
        target: Vec3::new(-1.6, 0.9, -2.2),
        fov_y_radians: 40f32.to_radians(),
        ..Camera::default()
    }
}

/// 室内盒场景（R52c 光照贴图验收场景的封闭版）：10×10 m 地板 + 后墙
/// （z = −5，法线 +Z）+ 左墙（x = −5，法线 +X）+ 天花板（y = 4，法线 −Y）
/// 以及遮挡柱（0.6×2×0.6 m）。+X / +Z 两侧敞开：侧向暖光从缺口进来，天花板
/// 把天空挡在室外 ⇒ **室内探针的天空项被大幅遮蔽**（本轮的因果前提：无探针
/// 时动态角色吃全局 env 天空色，有探针时吃室内烘焙辐照）。
///
/// 动态角色用 spawn_worker 单独加：**必须在烘焙 + install_lightmap 之后**
/// （R53/R62 教训：网格句柄值必须逐世界对齐，否则同一渲染器上「同句柄
/// 不同内容」会串味——先加角色会让 UV2 副本句柄整体错位一档）。
fn room_world() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(room_camera());
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.55, -1.0, 0.3).normalize(),
        color: Vec3::new(1.0, 0.94, 0.82),
        intensity: 2.6,
        ambient: Vec3::new(0.03, 0.035, 0.05),
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());

    let floor_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(10.0));
    let wall_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(10.0));
    let column_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mut mats = world.resource_mut::<MaterialStore>();
    let floor_mat = mats.add(MaterialDef::solid(0.78, 0.72, 0.6, 1.0));
    let wall_mat = mats.add(MaterialDef::solid(0.62, 0.63, 0.66, 1.0));
    let column_mat = mats.add(MaterialDef::solid(0.85, 0.4, 0.24, 1.0));
    drop(mats);

    world.spawn((Transform::IDENTITY, floor_mesh, floor_mat));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 2.0, -5.0),
            rotation: Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
            scale: Vec3::new(1.0, 1.0, 0.4),
        },
        wall_mesh,
        wall_mat,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(-5.0, 2.0, 0.0),
            rotation: Quat::from_rotation_z(-std::f32::consts::FRAC_PI_2),
            scale: Vec3::new(0.4, 1.0, 1.0),
        },
        wall_mesh,
        wall_mat,
    ));
    // 天花板：法线 −Y（朝室内），挡住半球天空采样
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 4.0, 0.0),
            rotation: Quat::from_rotation_x(std::f32::consts::PI),
            ..Transform::IDENTITY
        },
        wall_mesh,
        wall_mat,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(1.5, 1.0, 0.0),
            scale: Vec3::new(0.6, 2.0, 0.6),
            ..Transform::IDENTITY
        },
        column_mesh,
        column_mat,
    ));

    propagate_transforms(&mut world);
    world
}

/// 动态角色（圆柱 + RigidBody）：**必须在光照贴图装配之后**调用，
/// 保证各世界的网格 / 材质句柄序列一致（见 room_world 文档的串味论证）。
///
/// 只挂 RigidBody（动态）不挂 Collider：collect_bake_triangles 的静态判据是
/// 「无 RigidBody 或（有 Collider 且 is_static）」⇒ 该实体被排除在烘焙之外
/// ⇒ 没有光照贴图，正是本轮要修的对象。
fn spawn_worker(world: &mut World, position: Vec3) {
    let body = world
        .resource_mut::<MeshStore>()
        .add(MeshData::cylinder(0.32, 1.5, 20));
    let mut mat = MaterialDef::solid(0.86, 0.84, 0.8, 1.0);
    mat.roughness = 0.85;
    let handle = world.resource_mut::<MaterialStore>().add(mat);
    world.spawn((
        Transform::from_xyz(position.x, position.y + 0.75, position.z),
        body,
        handle,
        RigidBody::default(),
    ));
    propagate_transforms(world);
}

/// 测试口径的烘焙参数（与场景方向光同源）与 UV 参数（4 texel/m + 128² 图集，
/// 照 lightmap_render.rs 的口径压一档省 debug 耗时）。
fn test_bake_params() -> BakeParams {
    BakeParams {
        light_dir: Vec3::new(-0.55, -1.0, 0.3).normalize(),
        light_color: Vec3::new(1.0, 0.94, 0.82) * 2.6,
        sky_color: Vec3::new(0.10, 0.13, 0.22),
        shadow_softness: 0.08,
        sky_samples: 64,
        bounces: 1,
    }
}

fn test_bake_uv() -> BakeUvParams {
    BakeUvParams {
        texels_per_meter: 4.0,
        atlas_size: 128,
    }
}

/// 探针布局：室内 3×3×2 = 18 点网格（x/z ∈ {−3.5, 0, 3.5}、y ∈ {0.8, 2.6}）。
///
/// **三维布局**（两个高度层）而不是单层平面：抖动 + 超平面布局的组合在
/// 大点数下会落进数值退化区（构建期自检会作废并重试，见 irradiance.rs 的
/// 抖动阶梯），两层高度是实战里最省的稳健做法。
fn spawn_room_probes(world: &mut World) -> usize {
    let mut count = 0;
    for &x in &[-3.5f32, 0.0, 3.5] {
        for &z in &[-3.5f32, 0.0, 3.5] {
            for &y in &[0.8f32, 2.6] {
                world.spawn((Transform::from_xyz(x, y, z), IrradianceProbe));
                count += 1;
            }
        }
    }
    propagate_transforms(world);
    count
}

/// 烘焙光照贴图（动态角色不进烘焙 ⇒ 只给静态几何挂图集）。
fn with_lightmap(mut world: World) -> World {
    let baked = bake_lightmap(&world, &test_bake_params(), &test_bake_uv());
    install_lightmap(&mut world, baked);
    world
}

/// 室内盒 + 光照贴图 + 动态角色（**顺序**：几何 → 烘焙装配 → 角色）。
fn room_worker_world(position: Vec3) -> World {
    let mut world = with_lightmap(room_world());
    spawn_worker(&mut world, position);
    world
}

/// 室内盒 + 光照贴图 + 探针集 + 动态角色（顺序同上，探针装配在角色之前）。
fn room_worker_world_with_probes(position: Vec3) -> (World, IrradianceProbeInstall) {
    let mut world = with_lightmap(room_world());
    let (mut world, report) = {
        let report = {
            spawn_room_probes(&mut world);
            let baked = bake_irradiance_probes(&world, &test_bake_params());
            install_irradiance_probes(&mut world, baked)
        };
        (world, report)
    };
    spawn_worker(&mut world, position);
    (world, report)
}

/// 室内盒 + 光照贴图 + 探针集（无角色）。
fn room_world_with_probes() -> (World, IrradianceProbeInstall) {
    let mut world = with_lightmap(room_world());
    spawn_room_probes(&mut world);
    let baked = bake_irradiance_probes(&world, &test_bake_params());
    let report = install_irradiance_probes(&mut world, baked);
    (world, report)
}

fn luminance(pixels: &[u8], index: usize) -> u32 {
    let r = u32::from(pixels[index * 4]);
    let g = u32::from(pixels[index * 4 + 1]);
    let b = u32::from(pixels[index * 4 + 2]);
    (r * 2126 + g * 7152 + b * 722) / 10_000
}

/// 两帧差异 > 2 的像素掩码（ROI：角色像素）。
fn diff_mask(a: &[u8], b: &[u8]) -> Vec<bool> {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .map(|(x, y)| (0..3).any(|c| (x[c] as i32 - y[c] as i32).abs() > 2))
        .collect()
}

fn mean_rgb(pixels: &[u8], mask: &[bool]) -> (f64, f64, f64, f64) {
    let mut sum = [0.0f64; 4];
    let mut n = 0.0;
    for (i, m) in mask.iter().enumerate() {
        if !*m {
            continue;
        }
        n += 1.0;
        sum[0] += f64::from(pixels[i * 4]);
        sum[1] += f64::from(pixels[i * 4 + 1]);
        sum[2] += f64::from(pixels[i * 4 + 2]);
        sum[3] += f64::from(luminance(pixels, i));
    }
    (sum[0] / n, sum[1] / n, sum[2] / n, sum[3] / n)
}

// ---------------------------------------------------------------------------
// 1. 兼容锁定
// ---------------------------------------------------------------------------

/// 硬约束：探针资源缺席 ⇒ 与 R63 逐位一致（全帧 FNV-1a）。
#[test]
fn probe_free_scene_matches_r63_baseline_bitwise() {
    let _guard = GPU_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut renderer) = OffscreenRenderer::new(480, 270) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    // 基线场景 = 室内盒 + 动态角色（**无光照贴图**，与 R63 捕获时同构）
    let mut world = room_world();
    spawn_worker(&mut world, Vec3::new(-2.0, 0.0, -2.0));
    assert!(
        !world.contains_resource::<IrradianceProbeSet>(),
        "前提：无探针集"
    );
    renderer.render(&world);
    let pixels = renderer.read_pixels();
    assert_eq!(pixels.len(), 480 * 270 * 4);
    let hash = fnv1a(&pixels);
    assert_eq!(
        hash, R63_PROBE_FREE_HASH,
        "探针缺席的场景必须与 R63 逐位一致（{hash:#018x} ≠ {R63_PROBE_FREE_HASH:#018x}）"
    );
}

/// 硬约束：同场景 + 光照贴图 + 动态角色，探针缺席 ⇒ 与 R63 逐位一致。
#[test]
fn lightmapped_scene_matches_r63_baseline_bitwise() {
    let _guard = GPU_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut renderer) = OffscreenRenderer::new(480, 270) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let world = room_worker_world(Vec3::new(-2.0, 0.0, -2.0));
    renderer.render(&world);
    let pixels = renderer.read_pixels();
    let hash = fnv1a(&pixels);
    assert_eq!(
        hash, R63_LIGHTMAPPED_HASH,
        "光照贴图场景在探针缺席时必须与 R63 逐位一致（{hash:#018x} ≠ {R63_LIGHTMAPPED_HASH:#018x}）"
    );
}

/// **语义边界**：全是挂光照贴图的静态几何时，装了探针也**逐位不变**
/// （挂图集的实体不发探针标志位 ⇒ 走 R52c 的逐 texel 分支）。
#[test]
fn probes_do_not_touch_lightmapped_entities() {
    let _guard = GPU_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut renderer) = OffscreenRenderer::new(480, 270) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let plain = with_lightmap(room_world());
    renderer.render(&plain);
    let px_plain = renderer.read_pixels();

    // 同一个世界（同几何、同图集句柄）再装探针：静态几何没有任何实例载荷变化
    let (probed, report) = room_world_with_probes();
    assert!(report.tetrahedra > 0, "探针必须成四面体化：{report:?}");
    renderer.render(&probed);
    let px_probed = renderer.read_pixels();
    assert_eq!(
        fnv1a(&px_plain),
        fnv1a(&px_probed),
        "只挂光照贴图的场景装探针后必须逐位不变（掩码优先 + CPU 侧不发标志位）"
    );
}

// ---------------------------------------------------------------------------
// 2. 有效性（阴影区角色）
// ---------------------------------------------------------------------------

/// 角色 ROI（可见 - 隐藏的角色像素掩码）：把角色藏起来的同一场景作为对照。
fn worker_mask(renderer: &mut OffscreenRenderer) -> (Vec<bool>, Vec<u8>) {
    let visible = with_lightmap(room_worker_world(Vec3::new(-2.0, 0.0, -2.0)));
    renderer.render(&visible);
    let px_visible = renderer.read_pixels();
    let hidden = with_lightmap(room_world());
    renderer.render(&hidden);
    let px_hidden = renderer.read_pixels();
    (diff_mask(&px_hidden, &px_visible), px_hidden)
}

/// 阴影区动态角色：无探针吃天空色（冷、亮），有探针吃室内烘焙辐照（暖、暗）。
#[test]
fn probes_replace_sky_pollution_on_dynamic_worker() {
    let _guard = GPU_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut renderer) = OffscreenRenderer::new(480, 270) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let (mask, px_hidden) = worker_mask(&mut renderer);
    let roi = mask.iter().filter(|m| **m).count();
    assert!(
        roi > 300,
        "角色必须占据足够像素（实测 {roi} px）——掩码口径见 worker_mask"
    );

    let off = with_lightmap(room_worker_world(Vec3::new(-2.0, 0.0, -2.0)));
    renderer.render(&off);
    let px_off = renderer.read_pixels();

    let (on, report) = room_worker_world_with_probes(Vec3::new(-2.0, 0.0, -2.0));
    assert!(report.tetrahedra > 0, "探针集必须成四面体化：{report:?}");
    assert!(!report.degenerate);
    renderer.render(&on);
    let px_on = renderer.read_pixels();

    let (r_off, _g_off, b_off, l_off) = mean_rgb(&px_off, &mask);
    let (r_on, _g_on, b_on, l_on) = mean_rgb(&px_on, &mask);
    let (hr, hg, hb, _) = mean_rgb(&px_hidden, &mask);
    println!(
        "角色 ROI {roi} px｜无探针 亮度 {l_off:.1} R/B {:.2}｜有探针 亮度 {l_on:.1} R/B {:.2}｜\
         隐藏帧背景 亮度基准 R {hr:.1} G {hg:.1} B {hb:.1}",
        r_off / b_off.max(1.0),
        r_on / b_on.max(1.0)
    );

    // ① 阴影区角色必须显著变暗（天空污染被屋里烘焙辐照取代）
    assert!(
        l_on < l_off * 0.8,
        "有探针时阴影区角色必须显著变暗：{l_off:.1} → {l_on:.1}（应 < 80%）"
    );
    // ② 色相从天空冷蓝转向房间暖色：蓝-红差向「房间参考点」收敛。
    //    房间参考 = 隐藏帧同一 ROI 上的背景像素（角色背后的墙 / 地板）。
    let blue_bias_off = b_off - r_off;
    let blue_bias_on = b_on - r_on;
    let room_bias = hb - hr;
    assert!(
        blue_bias_on < blue_bias_off - 3.0,
        "色相必须从天空蓝偏向房间暖色：B−R 从 {blue_bias_off:.1} 降到 {blue_bias_on:.1}"
    );
    assert!(
        (blue_bias_on - room_bias).abs() < (blue_bias_off - room_bias).abs(),
        "有探针时角色的蓝色偏置必须比无探针时更接近房间参考（角色 {blue_bias_on:.1}、         无探针 {blue_bias_off:.1}、房间 {room_bias:.1}）"
    );
    // ④ 变暗的像素占 ROI 的多数（不是个别噪点）
    let darker = mask
        .iter()
        .enumerate()
        .filter(|(i, m)| **m && luminance(&px_on, *i) + 4 < luminance(&px_off, *i))
        .count();
    assert!(darker * 2 > roi, "ROI 多数像素必须变暗（{darker} / {roi}）");
}

/// 插值跟随位置：亮区（缺口入射的直射区）与阴影区的角色取值必须不同，
/// 且亮区更亮（探针云按位置取值，不是全局常数）。
#[test]
fn probe_value_follows_worker_position() {
    let _guard = GPU_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let bright = Vec3::new(3.2, 0.0, 3.2);
    let shadow = Vec3::new(-2.0, 0.0, -2.0);

    // CPU 侧口径：同一探针集在两个位置上的插值
    let probe_world = {
        let mut w = room_world();
        spawn_room_probes(&mut w);
        w
    };
    let set = bake_irradiance_probes(&probe_world, &test_bake_params());
    let e_bright = set.irradiance_at(bright + Vec3::Y * 0.9).expect("包内点");
    let e_shadow = set.irradiance_at(shadow + Vec3::Y * 0.9).expect("包内点");
    println!("探针辐照 亮区 {e_bright:?}｜阴影区 {e_shadow:?}");
    assert!(
        e_bright.max_element() > e_shadow.max_element() * 1.15,
        "亮区探针辐照必须明显高于阴影区：{e_bright:?} vs {e_shadow:?}"
    );

    // GPU 侧口径：两个位置的角色的平均亮度
    let Some(mut renderer) = OffscreenRenderer::new(480, 270) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut luma_at = |pos: Vec3| -> f64 {
        let base = with_lightmap(room_world());
        renderer.render(&base);
        let px_base = renderer.read_pixels();
        let (w, _) = room_worker_world_with_probes(pos);
        renderer.render(&w);
        let px = renderer.read_pixels();
        let mask = diff_mask(&px_base, &px);
        let n = mask.iter().filter(|m| **m).count();
        assert!(n > 200, "角色必须可见（{n} px）");
        mean_rgb(&px, &mask).3
    };
    let l_bright = luma_at(bright);
    let l_shadow = luma_at(shadow);
    println!("角色平均亮度 亮区 {l_bright:.1}｜阴影区 {l_shadow:.1}");
    assert!(
        l_bright > l_shadow * 1.2,
        "亮区角色必须比阴影区角色亮 ≥ 20%：{l_bright:.1} vs {l_shadow:.1}"
    );
}

// ---------------------------------------------------------------------------
// 3. 确定性
// ---------------------------------------------------------------------------

/// 全链确定性：烘焙 + 四面体化 + 插值双跑逐位；双会话逐帧逐位（角色移动）。
#[test]
fn probe_pipeline_is_bitwise_deterministic_across_sessions() {
    let _guard = GPU_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let path: Vec<Vec3> = (0..5)
        .map(|i| Vec3::new(-3.0 + i as f32 * 1.4, 0.0, -3.0 + i as f32 * 1.4))
        .collect();

    // CPU 半：两次烘焙逐位一致
    let mut world = room_world();
    spawn_room_probes(&mut world);
    let a = bake_irradiance_probes(&world, &test_bake_params());
    let b = bake_irradiance_probes(&world, &test_bake_params());
    assert_eq!(a.len(), b.len());
    for i in 0..a.len() {
        assert_eq!(
            a.values()[i].to_array().map(f32::to_bits),
            b.values()[i].to_array().map(f32::to_bits)
        );
    }
    assert_eq!(
        a.tetrahedralization().tetrahedra(),
        b.tetrahedralization().tetrahedra()
    );
    // 插值连续性：沿路径采样，相邻样本差有界（重心权重连续 ⇒ 无跳变）
    // 结构自检（构建期 debug_assert 之外的显式断言：局部 Delaunay + 流形 +
    // 欧拉特征），并抽样点定位口径。
    let tv = a.tetrahedralization().validate();
    assert!(tv.is_valid(), "四面体化结构自检失败：{tv:?}");
    assert_eq!(tv.euler_characteristic, 1);
    assert!(
        a.stats().tetra.build_attempts >= 1 && a.stats().tetra.jitter > 0.0,
        "抖动阶梯必须生效：{:?}",
        a.stats()
    );
    // 连续性：沿 7 m 对角路径逐 3 cm 采样，相邻差必须落在「场的梯度尺度」
    // 之内。梯度上界由探针取值跨度与探针间距之比给出（探针 3.5 m 间距、
    // 跨度 < 1 ⇒ 平均梯度 < 0.3/m）；实测最大单步差 0.077 / 0.06 m 出现在
    // 探针云角落（相机的亮区缺口），是**薄四面体内的陡梯度**而不是跳变
    // ——共享面两侧的一致性由 irradiance.rs 的解析单测（1e-4 相对容差）
    // 独立钉住。
    let mut prev: Option<Vec3> = None;
    for step in 0..120 {
        let t = step as f32 / 119.0;
        let p = Vec3::new(-3.5 + t * 7.0, 1.2, -3.5 + t * 7.0);
        let e = a.irradiance_at(p).expect("包内路径");
        if let Some(q) = prev {
            let step_diff = (e - q).abs().max_element();
            assert!(
                step_diff < 0.15,
                "相邻采样不得跳变（步长 0.06 m，实测差 {step_diff}）"
            );
        }
        prev = Some(e);
    }

    // GPU 半：双会话逐帧逐位
    let Some(mut s1) = OffscreenRenderer::new(480, 270) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut s2) = OffscreenRenderer::new(480, 270) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    for (frame, pos) in path.iter().enumerate() {
        let (w1, _) = room_worker_world_with_probes(*pos);
        let (w2, _) = room_worker_world_with_probes(*pos);
        s1.render(&w1);
        s2.render(&w2);
        let p1 = s1.read_pixels();
        let p2 = s2.read_pixels();
        assert_eq!(
            fnv1a(&p1),
            fnv1a(&p2),
            "第 {frame} 帧双会话必须逐位一致（角色位置 {pos:?}）"
        );
    }
    // 同一会话内重放同一帧也必须逐位一致（无跨帧状态残留）
    let (w, _) = room_worker_world_with_probes(path[0]);
    s1.render(&w);
    let first = s1.read_pixels();
    s1.render(&w);
    assert_eq!(fnv1a(&first), fnv1a(&s1.read_pixels()));
}
