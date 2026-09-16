//! 集成测试：虚拟几何体 V2 渲染（R85，compute 簇剔除 + 前缀和紧凑绘制）。
//!
//! 核心断言（本轮最强形态）：**剔除只删不可见几何 ⇒ 同一管线同一场景
//! 下，「全量绘制帧」与「GPU 剔除帧」像素逐字节恒等**——视锥剔除的
//! 包围球判定是保守界（球外 ⇒ 三角全在视锥外）、背面锥判定按
//! `cluster_is_backfacing` 的推导保守（锥内法线最大前途 ≤ 0 才剔）、
//! 被剔簇在光栅化阶段本就零贡献（视锥外 / 背面三角无片元）——任何
//! 像素差异都是真 bug（谓词非保守 / 紧凑列表错位 / vertex-pull 取数
//! 错误），按「偶发失败 = 真 bug」追查，无阈值豁免。
//!
//! 测试面：
//! 1. **剔除正确性锚点**：三机位（正面 3/4 / 侧后 / 远距）全量帧 vs
//!    剔除帧逐字节相等（每机位独立渲染器——首帧恒全量兜底，等待读回
//!    后第二帧恒剔除帧，两帧之间只有紧凑列表不同）；
//! 2. **紧凑序确定性**：消费到的紧凑列表恒严格升序（= 全局簇号升序，
//!    stable compaction；packed (实例槽<<16|局部号) 与全局簇号同序）；
//! 3. **剔除有效性**：`FrameStats.meshlet_surviving` 口径——正面近景
//!    （球背半球被锥剔 + 部分簇出视锥）显著小于簇总数且非零；背对场景
//!    （实例级视锥全剔）= 0（口径：全部实例连包围球都进不了视锥 ⇒
//!    本帧枚举为空，是集群剔除的上界极端）；
//! 4. **缺席语义**：无 `MeshletInstance` 的世界 FNV 与有者不同（生效反
//!    证）；空场景双帧 + 双渲染器逐位一致（命令流与引入前逐位不变——
//!    `MeshletMeshStore` 仅在库不挂实例同样零改动）；
//! 5. **确定性**：双会话（两台渲染器同一世界）逐字节一致；旋转实体一帧
//!    哈希改变。
//!
//! 环境无 GPU 适配器时优雅跳过（打印提示，测试仍算通过）。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 屏幕尺寸（测试统一口径）。
const SIZE: u32 = 256;

/// 本套件串行化（ssr / shadow_v2 / probe / meshlet 同款纪律）：同文件多
/// 测试并行共用单适配器会挂死——文件内互斥锁把并发度压到 1。
static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 场景几何：互不接触、无共面重叠面（深度并列会让绘制序影响像素）。
/// 球体 2976 三角 ⇒ 数十簇（背面锥剔除的主力样本）；立方 12 三角 ⇒
/// 1–2 簇（六向法线 ⇒ 锥退化全向，**永不锥剔**——对照组）。
fn scene_meshes() -> Vec<MeshData> {
    vec![
        MeshData::plane(10.0),
        MeshData::uv_sphere(0.7, 48, 32),
        MeshData::cube(),
    ]
}

/// meshlet 簇表示（同几何的 V2 形态）。
fn meshlet_meshes(meshes: &[MeshData]) -> Vec<MeshletMesh> {
    meshes
        .iter()
        .map(iris_engine::meshlet_builder::build_meshlets_from_mesh_data)
        .collect()
}

/// meshlet 场景世界：三件几何 + 各自 `MeshletInstance`。材质恒用默认
/// （句柄 0——meshlet 路径 v1 单材质口径，见 §8.54）。
fn meshlet_world(camera: Camera, meshes: &[MeshletMesh]) -> (World, usize) {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(camera);
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.4, -1.0, -0.3).normalize(),
        intensity: 1.4,
        ambient: Vec3::new(0.10, 0.11, 0.13),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(EnvironmentSettings { intensity: 0.0 });
    world.insert_resource(BloomSettings::disabled());

    let mut store = MeshletMeshStore::new();
    let mut total_clusters = 0usize;
    for mesh in meshes {
        total_clusters += mesh.clusters.len();
        store.add(mesh.clone());
    }
    world.insert_resource(store);
    let placements = [
        Vec3::ZERO,
        Vec3::new(0.0, 1.5, 0.0),
        Vec3::new(1.6, 0.5, -0.6),
    ];
    for (i, position) in placements.into_iter().enumerate() {
        world.spawn((
            Transform {
                position,
                ..Transform::IDENTITY
            },
            MeshletInstance {
                mesh: MeshletMeshHandle(i as u32),
            },
        ));
    }
    propagate_transforms(&mut world);
    (world, total_clusters)
}

/// 全量兜底帧（首帧）与剔除帧（第二帧）逐字节相等断言——三机位共用。
///
/// 帧 1 = 恒等列表全量绘制（无有效读回）；显式推进设备让紧凑读回完成；
/// 帧 2 = GPU 剔除路径（消费上帧紧凑列表）。两帧之间只有紧凑列表不同
/// ⇒ 像素必须逐字节恒等。正面全景下背面锥仍会剔球体背半球簇（被剔簇
/// 在两帧中都零贡献），故存活数 ≤ 总数且严格升序。
fn assert_full_frame_equals_culled_frame(camera: Camera, context: &str) {
    let meshes = scene_meshes();
    let meshlets = meshlet_meshes(&meshes);
    let (world, total_clusters) = meshlet_world(camera, &meshlets);
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };

    // 帧 1：无有效读回 ⇒ 恒等列表全量绘制（surviving = 簇总数）
    renderer.render(&world);
    let px_full = renderer.read_pixels();
    assert_eq!(
        renderer.renderer().stats().meshlet_surviving as usize,
        total_clusters,
        "{context}：首帧应为全量兜底（surviving = 簇总数）"
    );
    // 等待紧凑结果读回的 map_async 回调完成（read_pixels 内已推进设备，
    // 此处兜底再推进一次）
    renderer
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("读回等待失败");

    // 帧 2：GPU 剔除路径（draw 开头消费紧凑列表，其余全部相同）
    renderer.render(&world);
    let surviving = renderer.renderer().stats().meshlet_surviving as usize;
    assert!(
        surviving > 0 && surviving <= total_clusters,
        "{context}：剔除帧存活数 {surviving} 应落在 (0, {total_clusters}]"
    );
    let list = renderer
        .renderer()
        .meshlet_consumed_list()
        .unwrap_or_else(|| panic!("{context}：剔除帧应已消费到紧凑列表"));
    assert_eq!(list.len(), surviving, "{context}：列表长度应与存活数一致");
    assert!(
        list.windows(2).all(|w| w[0] < w[1]),
        "{context}：紧凑列表必须严格升序（stable compaction）"
    );
    let px_culled = renderer.read_pixels();
    assert_pixels_byte_equal(&px_full, &px_culled, context);
}

/// 逐字节相等断言（带首个差异定位）。
fn assert_pixels_byte_equal(a: &[u8], b: &[u8], context: &str) {
    assert_eq!(a.len(), b.len(), "{context}：像素缓冲长度不同");
    if a == b {
        return;
    }
    let diff = a
        .iter()
        .zip(b.iter())
        .position(|(x, y)| x != y)
        .expect("长度相等且不相等 ⇒ 必有差异");
    let pixel = diff / 4;
    let (x, y) = (pixel % SIZE as usize, pixel / SIZE as usize);
    panic!(
        "{context}：像素逐字节不等（第 {diff} 字节 = 像素 ({x},{y}) 通道 {}，\
         左 {} vs 右 {}；全帧 FNV {:016x} vs {:016x}）——剔除谓词非保守或 \
         vertex-pull 取数错位",
        diff % 4,
        a[diff],
        b[diff],
        fnv1a(a),
        fnv1a(b)
    );
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 机位 A：右前上方 3/4 视角（全景）。
fn camera_front() -> Camera {
    Camera {
        eye: Vec3::new(3.2, 2.0, 3.6),
        target: Vec3::new(0.0, 0.7, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    }
}

/// 机位 B：左后上方（与 A 视角互补——锥剔除的另一侧）。
fn camera_back_side() -> Camera {
    Camera {
        eye: Vec3::new(-2.6, 3.4, -2.4),
        target: Vec3::new(0.0, 0.6, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    }
}

/// 机位 C：远距（全部几何仅占数像素——远距端等价锚点）。
fn camera_far() -> Camera {
    Camera {
        eye: Vec3::new(8.0, 5.0, 9.0),
        target: Vec3::new(0.0, 0.7, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    }
}

/// 剔除正确性锚点 × 三机位（含背面 / 侧面 / 远距）：全量帧 vs 剔除帧
/// 逐字节相等。
#[test]
fn culled_frame_matches_full_draw_byte_identical_front() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    assert_full_frame_equals_culled_frame(camera_front(), "机位 A（正面 3/4）");
}

#[test]
fn culled_frame_matches_full_draw_byte_identical_back_side() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    assert_full_frame_equals_culled_frame(camera_back_side(), "机位 B（侧后）");
}

#[test]
fn culled_frame_matches_full_draw_byte_identical_far() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    assert_full_frame_equals_culled_frame(camera_far(), "机位 C（远距）");
}

/// 剔除有效性：`meshlet_surviving` 口径。
///
/// 依据：球体簇的锥半角约等于其角跨度（cutoff = min 轴·法线）——正面
/// 近景下背半球簇的「簇心→眼」方向越出 90° + θc 锥 ⇒ 锥剔；再加部分簇
/// 整体出视锥。口径带 (0, 0.85·总数)：下界 > 0 排除「全剔坏路径」，上界
/// < 0.85 排除「没剔」；立方簇（全向退化锥，永不锥剔）保证正面帧恒有
/// 存活者。背对场景（相机 180° 转离）= 全部实例在实例级视锥剔除 ⇒ 本帧
/// 簇枚举为空 ⇒ surviving = 0（集群剔除的极端上界）。
#[test]
fn culling_effectively_prunes() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let meshes = scene_meshes();
    let meshlets = meshlet_meshes(&meshes);
    let total_clusters = meshlets.iter().map(|m| m.clusters.len()).sum::<usize>();

    // 正面近景：部分簇存活、部分被剔
    let (world_front, _) = meshlet_world(camera_front(), &meshlets);
    renderer.render(&world_front); // 帧 1：全量兜底
    assert_eq!(
        renderer.renderer().stats().meshlet_surviving as usize,
        total_clusters,
        "首帧应全量兜底"
    );
    renderer.render(&world_front); // 帧 2：剔除生效
    let front_surviving = renderer.renderer().stats().meshlet_surviving as usize;
    assert!(
        front_surviving > 0,
        "正面近景必须有簇存活（立方全向锥簇永不锥剔）"
    );
    assert!(
        front_surviving * 10 < total_clusters * 85,
        "正面近景应显著剔除（{front_surviving}/{total_clusters} ≥ 85% 说明剔除未生效）"
    );

    // 背对场景：相机 180° 转离。球 / 立方实例在实例级视锥全剔；地面
    // 平面的簇包围球（10×10 单 quad ⇒ 半径 ≈ 7.07 m 的均值球）仍与视锥
    // 相交 ⇒ 保守存活（判定半径放大是刻意的保守方向，见
    // cluster_bounding_sphere）。口径：显著小于簇总数（≤ 25%）。
    let camera_away = Camera {
        eye: Vec3::new(0.0, 0.8, 5.0),
        target: Vec3::new(0.0, 0.8, 40.0),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    };
    let (world_away, _) = meshlet_world(camera_away, &meshlets);
    renderer.render(&world_away); // 换世界首帧：消费旧列表（上一世界的）
    // 显式等待：让换世界首帧的紧凑读回完成，保证下一帧消费的是
    // 「背对场景」自身的剔除结果（帧配对契约——见 prepare_meshlet_frame）
    renderer
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("读回等待失败");
    renderer.render(&world_away); // 该帧只剩保守存活的地面簇
    let away_surviving = renderer.renderer().stats().meshlet_surviving as usize;
    assert!(
        away_surviving * 4 <= total_clusters,
        "相机背对场景时 surviving 应显著小于簇总数（{away_surviving}/{total_clusters}）"
    );
    assert!(
        away_surviving < front_surviving,
        "背对场景的存活数必须少于正面（{away_surviving} vs {front_surviving}）"
    );
}

/// 缺席语义：有 / 无 meshlet 实体的世界 FNV 不同（生效反证）；空场景
/// 双帧 + 双渲染器逐位一致；仅挂 MeshletMeshStore 不挂实例 = 与完全
/// 无 store 的世界逐位一致（库存在本身零开销）。
#[test]
fn absence_semantics_bit_stable() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let meshes = scene_meshes();
    let meshlets = meshlet_meshes(&meshes);
    let (world_with, _) = meshlet_world(camera_front(), &meshlets);

    // 无实例世界：同摆位的常规网格实体（常规路径渲染——内容不同自然
    // FNV 不同，这里反证的是 meshlet 路径确实改变像素流）
    let mut world_without = World::new();
    world_without.insert_resource(MeshStore::new());
    world_without.insert_resource(MaterialStore::new());
    world_without.insert_resource(TextureStore::new());
    world_without.insert_resource(camera_front());
    world_without.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.4, -1.0, -0.3).normalize(),
        intensity: 1.4,
        ambient: Vec3::new(0.10, 0.11, 0.13),
        shadows: false,
        ..DirectionalLight::default()
    });
    world_without.insert_resource(EnvironmentSettings { intensity: 0.0 });
    world_without.insert_resource(BloomSettings::disabled());
    for mesh in &meshes {
        world_without.resource_mut::<MeshStore>().add(mesh.clone());
    }
    for position in [
        Vec3::ZERO,
        Vec3::new(0.0, 1.5, 0.0),
        Vec3::new(1.6, 0.5, -0.6),
    ] {
        world_without.spawn((
            Transform {
                position,
                ..Transform::IDENTITY
            },
            MeshHandle(0),
        ));
    }
    propagate_transforms(&mut world_without);

    renderer.render(&world_with);
    renderer.render(&world_with); // 帧 2 = 剔除路径
    let hash_with = fnv1a(&renderer.read_pixels());
    renderer.render(&world_without);
    let hash_without = fnv1a(&renderer.read_pixels());
    assert_ne!(
        hash_with, hash_without,
        "meshlet 世界与空场景的 FNV 必须不同（生效反证）"
    );

    // 空场景（有 MeshStore 但零实体、无 meshlet）：双帧稳定
    let mut empty_a = World::new();
    empty_a.insert_resource(MeshStore::new());
    empty_a.insert_resource(MaterialStore::new());
    empty_a.insert_resource(TextureStore::new());
    empty_a.insert_resource(camera_front());
    empty_a.insert_resource(DirectionalLight::default());
    empty_a.insert_resource(EnvironmentSettings { intensity: 0.0 });
    empty_a.insert_resource(BloomSettings::disabled());
    renderer.render(&empty_a);
    let empty_1 = renderer.read_pixels();
    renderer.render(&empty_a);
    let empty_2 = renderer.read_pixels();
    assert_pixels_byte_equal(&empty_1, &empty_2, "空场景双帧");
    // 仅挂 store 不挂实例：与空场景逐位一致（库存在本身不产生命令流）
    let mut empty_b = World::new();
    empty_b.insert_resource(MeshStore::new());
    empty_b.insert_resource(MaterialStore::new());
    empty_b.insert_resource(TextureStore::new());
    empty_b.insert_resource(MeshletMeshStore::new());
    empty_b.insert_resource(camera_front());
    empty_b.insert_resource(DirectionalLight::default());
    empty_b.insert_resource(EnvironmentSettings { intensity: 0.0 });
    empty_b.insert_resource(BloomSettings::disabled());
    renderer.render(&empty_b);
    let empty_store = renderer.read_pixels();
    assert_pixels_byte_equal(&empty_1, &empty_store, "仅挂 store 的空场景");
    // 双渲染器空场景逐位一致（空场景既有 FNV 不变的会话无关形态）
    let Some(mut renderer2) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    renderer2.render(&empty_a);
    let empty_3 = renderer2.read_pixels();
    assert_pixels_byte_equal(&empty_1, &empty_3, "空场景跨渲染器");
}

/// 确定性：双会话（两台渲染器服务同一世界）逐字节一致；旋转实体一帧
/// （推进变换 → 重新传播）后哈希改变（几何确实动了）。
#[test]
fn deterministic_across_sessions_and_rotation_changes_frame() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer_a) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let Some(mut renderer_b) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let meshes = scene_meshes();
    let meshlets = meshlet_meshes(&meshes);
    let (world, _) = meshlet_world(camera_front(), &meshlets);

    // 双会话 × 两帧（全量 + 剔除）逐字节一致
    renderer_a.render(&world);
    let a1 = renderer_a.read_pixels();
    renderer_b.render(&world);
    let b1 = renderer_b.read_pixels();
    assert_pixels_byte_equal(&a1, &b1, "双会话帧 1（全量兜底）");
    renderer_a
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("读回等待失败");
    renderer_b
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("读回等待失败");
    renderer_a.render(&world);
    let a2 = renderer_a.read_pixels();
    renderer_b.render(&world);
    let b2 = renderer_b.read_pixels();
    assert_pixels_byte_equal(&a2, &b2, "双会话帧 2（剔除）");

    // 旋转一帧：实体绕 Z 转 90° 后重传播 → 像素必须改变（绕 Z 旋转
    // 会把地面平面立起来——绕 Y 的 90° 对本场景是自对称变换，哈希不变
    // 才是正确行为）
    let mut rotated = meshlet_world(camera_front(), &meshlets).0;
    rotated.query::<&mut Transform>(|t| {
        t.rotation = glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
    });
    propagate_transforms(&mut rotated);
    renderer_a.render(&rotated);
    renderer_a.render(&rotated);
    let hash_rotated = fnv1a(&renderer_a.read_pixels());
    assert_ne!(
        hash_rotated,
        fnv1a(&a2),
        "旋转实体后全帧哈希必须改变（几何确实动了）"
    );
}
