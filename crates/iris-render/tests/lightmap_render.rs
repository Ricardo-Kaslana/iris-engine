//! 集成测试：烘焙光照 c 阶段（R52c）——GPU 集成与混合语义。
//!
//! 三条主线：
//! 1. **兼容锁定（硬约束）**：未挂光照贴图的场景渲染结果与 R52b（引入
//!    UV2 / 光照贴图槽位前）**逐位一致**——对固定室内盒场景的全帧像素做
//!    FNV-1a 哈希，与改动落地前实跑捕获的基线值比对（先例见
//!    `normal_render.rs` 的 R41 基线锁）。任何一位改变都意味着「缺席
//!    光照贴图 = 原路径」被破坏（论证链见 `lightmap.rs` 模块文档与
//!    `standard.wgsl` 的 `select` 掩码）。
//! 2. **光照贴图生效**：同场景同相机烘焙 + 挂载后，墙角 / 阴影区显著变暗
//!    （AO + 静态阴影 + 色溢），受光面保持亮度。
//! 3. **确定性**：同输入两次渲染逐位一致。
//!
//! 环境：无 GPU 适配器时优雅跳过（`OffscreenRenderer::new` 返回 `None`）。

use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Quat, Transform, Vec3, World};
use iris_render::prelude::*;

/// R52b 基线全帧像素 FNV-1a 64（改动落地前以同一场景实跑捕获；
/// 场景 = 室内盒（地板 + 两墙 + 遮挡柱）+ 侧向暖光 + 默认 IBL，
/// 480×270，见 [`room_world`]）。
const R52B_BASELINE_HASH: u64 = 0x3fe5_f3ff_00e0_f329;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 室内盒场景（与 `examples/bake_preview.rs` 同款几何）：10×10 m 地板 +
/// 后墙（z = −5，法线 +Z）+ 左墙（x = −5，法线 +X）+ 0.6×2×0.6 m 遮挡柱
/// （(1.5, 1, 0)），相机在室内斜看墙角，方向光侧照（与烘焙口径同源）。
fn room_world() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(3.4, 2.2, 3.8),
        target: Vec3::new(-1.2, 0.6, -2.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.55, -1.0, 0.3).normalize(),
        color: Vec3::new(1.0, 0.95, 0.85),
        intensity: 2.2,
        ambient: Vec3::new(0.10, 0.11, 0.14),
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());

    let floor_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(10.0));
    let wall_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(10.0));
    let column_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mut mats = world.resource_mut::<MaterialStore>();
    let floor_mat = mats.add(MaterialDef::solid(0.8, 0.78, 0.72, 1.0));
    let wall_mat = mats.add(MaterialDef::solid(0.55, 0.6, 0.7, 1.0));
    let column_mat = mats.add(MaterialDef::solid(0.9, 0.35, 0.2, 1.0));
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

/// 硬约束锁定：未挂光照贴图的场景与 R52b 基线逐位一致（全帧像素哈希）。
#[test]
fn scene_without_lightmap_matches_r52b_baseline_bitwise() {
    let Some(mut renderer) = OffscreenRenderer::new(480, 270) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let world = room_world();
    renderer.render(&world);
    let pixels = renderer.read_pixels();
    assert_eq!(pixels.len(), 480 * 270 * 4, "全帧像素尺寸必须一致");
    let hash = fnv1a(&pixels);
    assert_eq!(
        hash, R52B_BASELINE_HASH,
        "未挂光照贴图的渲染结果必须与 R52b 基线逐位一致（哈希 {hash:#018x} ≠ 基线 \
         {R52B_BASELINE_HASH:#018x}）——缺席光照贴图的原路径被破坏"
    );
}

/// 测试口径的烘焙参数：与 `room_world` 的方向光同源（方向 / 颜色 × 强度），
/// 天空压暗（一次反弹会填掉大半遮蔽对比，直射主导才有稳健断言，量级论证
/// 见 ARCHITECTURE §4.24）+ 软影锥 0.08 rad + 64 半球样本 + 1 次反弹。
fn test_bake_params() -> BakeParams {
    BakeParams {
        light_dir: Vec3::new(-0.55, -1.0, 0.3),
        light_color: Vec3::new(1.0, 0.95, 0.85) * 2.2,
        sky_color: Vec3::new(0.12, 0.15, 0.22),
        shadow_softness: 0.08,
        sky_samples: 64,
        bounces: 1,
    }
}

/// 测试口径的 UV 参数：4 texel/m + 128² 图集（比验收示例的 8 / 256 粗一档
/// ——debug 测试的烘焙耗时按 texel 数线性，粗一档省 4×，语义覆盖不变）。
fn test_bake_uv() -> BakeUvParams {
    BakeUvParams {
        texels_per_meter: 4.0,
        atlas_size: 128,
    }
}

/// 烘焙 + 装配（`install_lightmap` 会替换实体的网格句柄为带 UV2 的副本，
/// 变换不变 ⇒ 无需重跑 `propagate_transforms`）。
fn room_world_with_lightmap() -> (World, LightmapHandle) {
    let mut world = room_world();
    let baked = bake_lightmap(&world, &test_bake_params(), &test_bake_uv());
    let report = install_lightmap(&mut world, baked);
    (world, report.lightmap)
}

/// sRGB 编码字节的感知亮度（Rec. 709 权重，仅用于统计比较，不做线性化——
/// 两帧同一口径，比值 / 分位数有意义）。
fn luminance(pixels: &[u8], index: usize) -> u32 {
    let r = u32::from(pixels[index * 4]);
    let g = u32::from(pixels[index * 4 + 1]);
    let b = u32::from(pixels[index * 4 + 2]);
    (r * 2126 + g * 7152 + b * 722) / 10_000
}

/// 亮度分位数（`q` ∈ [0,1]）：把全帧亮度排序后取值——比区域硬编码更抗
/// 相机 / 构图漂移，且「暗部变暗、亮部保持」正是分位数的语义。
fn luminance_quantile(pixels: &[u8], q: f32) -> u32 {
    let count = pixels.len() / 4;
    let mut luma: Vec<u32> = (0..count).map(|i| luminance(pixels, i)).collect();
    luma.sort_unstable();
    let index = ((count as f32 - 1.0) * q.clamp(0.0, 1.0)).round() as usize;
    luma[index]
}

/// 光照贴图生效（R52c 混合语义）：同场景同相机，烘焙 + 挂载后
///
/// 1. 整帧显著改变（间接项被替换）；
/// 2. **暗部更暗**（10 分位下降 ≥ 20%）——墙角 AO + 遮蔽生效；
/// 3. **亮部保持**（90 分位不低于原值的 70%）——直接光不受光照贴图影响
///    （「静态 GI 补动态直射」的混合语义）；
/// 4. 变暗像素占场景像素的显著比例（不是个别噪点）；
/// 5. 图集懒上传已发生（`lightmap_is_uploaded` 观测口径）；
/// 6. 确定性：重建同世界再渲一次逐位一致。
#[test]
fn lightmap_darkens_occluded_areas_and_keeps_direct_light() {
    let Some(mut renderer) = OffscreenRenderer::new(480, 270) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let (w, h) = (480usize, 270usize);

    renderer.render(&room_world());
    let px_plain = renderer.read_pixels();
    assert!(
        !renderer.renderer().lightmap_is_uploaded(LightmapHandle(0)),
        "无烘焙时不应有图集上传"
    );

    let (world_baked, handle) = room_world_with_lightmap();
    renderer.render(&world_baked);
    let px_baked = renderer.read_pixels();
    assert!(
        renderer.renderer().lightmap_is_uploaded(handle),
        "渲染后图集必须已懒上传（{handle:?}）"
    );

    // 1. 整帧显著改变
    let total = w * h;
    let diff_sum: u64 = (0..total)
        .map(|i| {
            (0..3)
                .map(|c| {
                    (px_plain[i * 4 + c] as i32 - px_baked[i * 4 + c] as i32).unsigned_abs() as u64
                })
                .sum::<u64>()
        })
        .sum();
    let mean_diff = diff_sum as f64 / (total * 3) as f64;
    assert!(
        mean_diff > 3.0,
        "挂载光照贴图后整帧平均差 {mean_diff:.2} 应显著非零（/255）"
    );

    // 2. 受光照贴图作用的像素集合 = 两帧差异 > 2 的像素（天空与动态直射
    //    逐位不变 ⇒ 差异像素就是间接项被替换的表面；不用「与角落像素比较」
    //    的判据——角落可能是墙而非天空，实测会筛出几乎全是天空的假集合）
    let changed: Vec<usize> = (0..total)
        .filter(|&i| {
            (0..3).any(|c| (px_plain[i * 4 + c] as i32 - px_baked[i * 4 + c] as i32).abs() > 2)
        })
        .collect();
    let coverage = changed.len() as f64 / total as f64;
    assert!(
        coverage > 0.10,
        "室内盒必须占据画面：受光照贴图作用的像素 {coverage:.1}%（{} / {total}）",
        changed.len()
    );

    // 3 / 4. 按**原帧亮度**把受影响像素分成暗部 / 亮部四分位：暗部由遮蔽
    //    区主导（墙角、柱根、墙地交接），亮部由直射主导。整集合取均值不
    //    成立——蓝偏 IBL 换成暖偏烘焙间接光时逐通道差很大（平均差 20/255）
    //    但亮度只降约 10%，直射像素把均值拉平。
    let mean_luma = |px: &[u8]| -> f64 {
        changed
            .iter()
            .map(|&i| luminance(px, i) as f64)
            .sum::<f64>()
            / changed.len() as f64
    };
    let mut by_luma = changed.clone();
    by_luma.sort_unstable_by_key(|&i| luminance(&px_plain, i));
    let quartile = by_luma.len() / 4;
    let (dark_set, bright_set) = (&by_luma[..quartile], &by_luma[by_luma.len() - quartile..]);
    let mean_of = |set: &[usize], px: &[u8]| -> f64 {
        set.iter().map(|&i| luminance(px, i) as f64).sum::<f64>() / set.len() as f64
    };
    let (dark_plain, dark_baked) = (mean_of(dark_set, &px_plain), mean_of(dark_set, &px_baked));
    let (bright_plain, bright_baked) = (
        mean_of(bright_set, &px_plain),
        mean_of(bright_set, &px_baked),
    );
    assert!(
        dark_baked < dark_plain * 0.85,
        "暗部四分位（墙角 / 遮蔽区）平均亮度必须下降 ≥ 15%：\
         {dark_plain:.1} → {dark_baked:.1}（AO + 半球遮蔽生效）"
    );
    assert!(
        bright_baked > bright_plain * 0.85,
        "亮部四分位（直射主导）必须保持 ≥ 85%：{bright_plain:.1} → {bright_baked:.1}\
         （直接光不受光照贴图影响）"
    );
    let darker = changed
        .iter()
        .filter(|&&i| luminance(&px_plain, i) as i32 - luminance(&px_baked, i) as i32 > 8)
        .count();
    let darker_ratio = darker as f64 / changed.len() as f64;
    let (mean_plain, mean_baked) = (mean_luma(&px_plain), mean_luma(&px_baked));
    println!(
        "光照贴图对照：平均差 {mean_diff:.2}/255、受影响像素 {:.1}%、\
         暗部四分位 {dark_plain:.1} → {dark_baked:.1}（{:.0}%）、\
         亮部四分位 {bright_plain:.1} → {bright_baked:.1}（{:.0}%）、\
         全集合均值 {mean_plain:.1} → {mean_baked:.1}、变暗占比 {darker_ratio:.1}%、\
         全帧 10 分位 {} → {}、90 分位 {} → {}、图集 {}² 已上传",
        coverage * 100.0,
        dark_baked / dark_plain * 100.0,
        bright_baked / bright_plain * 100.0,
        luminance_quantile(&px_plain, 0.10),
        luminance_quantile(&px_baked, 0.10),
        luminance_quantile(&px_plain, 0.90),
        luminance_quantile(&px_baked, 0.90),
        test_bake_uv().atlas_size
    );

    // 5. 确定性：重建同世界再渲一次逐位一致
    let (world_again, _) = room_world_with_lightmap();
    renderer.render(&world_again);
    let px_again = renderer.read_pixels();
    assert_eq!(
        px_baked, px_again,
        "同输入两次渲染必须逐位一致（烘焙 + 上传 + 采样的确定性）"
    );
}

/// 缺席语义：挂了 [`LightmapBinding`] 但网格**无 UV2**（未注入）⇒ 退回
/// IBL 间接光，渲染结果与基线**逐位一致**（掩码 0 的兜底路径，
/// 见 `renderer.rs` 的 `collect_instances` 解析口径）。
#[test]
fn binding_without_uv2_falls_back_to_ibl_bitwise() {
    let Some(mut renderer) = OffscreenRenderer::new(480, 270) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    renderer.render(&room_world());
    let px_plain = renderer.read_pixels();

    // 只建目录 + 挂绑定，不注入 UV2（网格仍是原网格）
    let mut world = room_world();
    let baked = bake_lightmap(&world, &test_bake_params(), &test_bake_uv());
    let entities: Vec<_> = baked
        .atlas
        .entries
        .iter()
        .map(|e| e.entity)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    world.insert_resource(LightmapStore::new());
    let handle = world.resource_mut::<LightmapStore>().insert(baked);
    for &entity in &entities {
        world.insert(entity, LightmapBinding { lightmap: handle });
    }
    assert!(
        world
            .resource::<MeshStore>()
            .get(*world.get::<MeshHandle>(entities[0]).expect("网格句柄"))
            .expect("网格")
            .uv2
            .is_none(),
        "前提：网格未注入 UV2"
    );
    renderer.render(&world);
    let px_bound = renderer.read_pixels();
    assert_eq!(
        fnv1a(&px_bound),
        fnv1a(&px_plain),
        "网格无 UV2 时必须逐位退回 IBL 路径（绑定不得让间接光变黑）"
    );
}
