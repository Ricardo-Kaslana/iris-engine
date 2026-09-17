//! 集成测试：虚拟几何体 V3 渲染（R86，层级 LOD——簇 DAG + 屏幕空间
//! 误差选择）。
//!
//! 核心断言（本轮最强形态）：**threshold 0 恒细化到叶 ⇒ LOD 启用帧与
//! R85 全叶路径像素逐字节相等**——叶组恒选自身 ⇒ 选中集 = 全部叶簇 =
//! 恒等 run 表，选择只是换了等价的输入集合（层级存在但未启用的零成本
//! 回归）。其余断言：距离单调（远选粗、近选细、物体不消失）、双会话
//! 逐字节、阈值变化改变像素流。
//!
//! 环境无 GPU 适配器时优雅跳过（打印提示，测试仍算通过）。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 屏幕尺寸（测试统一口径）。
const SIZE: u32 = 256;

/// 本套件串行化（meshlet v2 / probe / post 同款纪律）：同文件多测试
/// 并行共用单适配器会挂死——文件内互斥锁把并发度压到 1。
static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 场景几何：互不接触、无共面重叠面。球体 ~6000 三角 ⇒ 数十叶簇 +
/// 多层父簇（LOD 主力样本）；立方 6 叶簇 ⇒ 一层合并（DAG 形态对照）；
/// 地面 2 三角 ⇒ 单叶簇自为根（平凡层级对照）。
fn scene_meshes() -> Vec<MeshData> {
    vec![
        MeshData::plane(10.0),
        MeshData::uv_sphere(0.7, 64, 48),
        MeshData::cube(),
    ]
}

/// 层级簇表示（同几何的 V3 形态）。
fn hierarchy_meshes(meshes: &[MeshData]) -> Vec<MeshletMesh> {
    meshes
        .iter()
        .map(iris_engine::meshlet_builder::build_meshlet_hierarchy_from_mesh_data)
        .collect()
}

/// R85 平面簇表示（对照组：全叶路径）。
fn flat_meshes(meshes: &[MeshData]) -> Vec<MeshletMesh> {
    meshes
        .iter()
        .map(iris_engine::meshlet_builder::build_meshlets_from_mesh_data)
        .collect()
}

fn base_world(camera: Camera) -> World {
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
    world
}

/// meshlet 场景世界：三件层级几何 + 各自 `MeshletInstance`；`lod` 在场
/// 才挂 [`MeshletLodSettings`]（缺席语义对照由调用方构造）。
fn lod_world(camera: Camera, meshes: &[MeshletMesh], lod: Option<MeshletLodSettings>) -> World {
    let mut world = base_world(camera);
    let mut store = MeshletMeshStore::new();
    for mesh in meshes {
        store.add(mesh.clone());
    }
    world.insert_resource(store);
    if let Some(lod) = lod {
        world.insert_resource(lod);
    }
    for (i, position) in placements().into_iter().enumerate() {
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
    world
}

fn placements() -> [Vec3; 3] {
    [
        Vec3::ZERO,
        Vec3::new(0.0, 1.5, 0.0),
        Vec3::new(1.6, 0.5, -0.6),
    ]
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
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
         左 {} vs 右 {}；全帧 FNV {:016x} vs {:016x}）",
        diff % 4,
        a[diff],
        b[diff],
        fnv1a(a),
        fnv1a(b)
    );
}

/// 近景机位（球 L1 组误差 ≈ 31 px @ 256p——阈值的「细化侧」）。
fn camera_near() -> Camera {
    Camera {
        eye: Vec3::new(2.4, 1.6, 2.8),
        target: Vec3::new(0.0, 0.7, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    }
}

/// 中景机位（球 L1 组误差 ≈ 13 px——阈值 15 的过渡带）。
fn camera_mid() -> Camera {
    Camera {
        eye: Vec3::new(5.5, 3.4, 6.2),
        target: Vec3::new(0.0, 0.7, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    }
}

/// 远景机位（球 L1 组误差 ≈ 6 px、根误差 ≈ 12 px——粗化侧）。
fn camera_far() -> Camera {
    Camera {
        eye: Vec3::new(12.0, 7.5, 13.5),
        target: Vec3::new(0.0, 0.7, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    }
}

/// τ→全叶等价（本轮最强断言）：threshold 0 恒细化到叶 ⇒ 层级 mesh +
/// LOD 启用的首帧与 R85 全叶路径（平面 mesh、无设置）逐字节相等，且
/// `meshlet_surviving` 同为叶簇总数——层级存在但选择未生效时零成本。
#[test]
fn lod_tau_zero_matches_r85_full_leaf_byte_identical() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer_lod) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let Some(mut renderer_r85) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let meshes = scene_meshes();
    let hier = hierarchy_meshes(&meshes);
    let flat = flat_meshes(&meshes);
    let leaf_total: usize = flat.iter().map(|m| m.clusters.len()).sum();

    // LOD 启用 + threshold 0：选择必下钻到叶 ⇒ 恒等 run 表
    let world_lod = lod_world(
        camera_near(),
        &hier,
        Some(MeshletLodSettings {
            threshold_px: 0.0,
            enabled: true,
        }),
    );
    renderer_lod.render(&world_lod); // 首 = 恒等兜底（选中集 = 全叶）
    let px_lod = renderer_lod.read_pixels();
    let surviving_lod = renderer_lod.renderer().stats().meshlet_surviving as usize;

    // R85 路径：平面 mesh、无 LOD 设置
    let world_r85 = lod_world(camera_near(), &flat, None);
    renderer_r85.render(&world_r85);
    let px_r85 = renderer_r85.read_pixels();
    let surviving_r85 = renderer_r85.renderer().stats().meshlet_surviving as usize;

    assert_eq!(
        surviving_lod, leaf_total,
        "threshold 0 下选中数 = 叶簇总数（首帧恒等兜底口径）"
    );
    assert_eq!(surviving_r85, leaf_total, "R85 全叶路径 surviving = 簇总数");
    assert_pixels_byte_equal(&px_lod, &px_r85, "τ→全叶等价：LOD 启用帧 vs R85 全叶帧");
}

/// LOD 生效：相机拉远 ⇒ 选中簇数单调下降且画面仍完整（物体不消失——
/// 远景 FNV ≠ 空场景 FNV，且 ≠ 近景 FNV）。首帧 = 恒等兜底，surviving
/// 即「本机位选中的簇数」（无读回时延，逐机位独立渲染器保证纯净）。
#[test]
fn lod_effective_with_distance_monotone() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer_near) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let Some(mut renderer_mid) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let Some(mut renderer_far) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let meshes = scene_meshes();
    let hier = hierarchy_meshes(&meshes);
    // 阈值定标（球 L1 own_error ≈ 0.52、根 ≈ 1.03，系数 221.7/dist）：
    // 近 L1 ≈ 31 px（细）、中 L1 ≈ 13 px（选中 L1）、远根 ≈ 12 px（选中根）。
    let settings = MeshletLodSettings {
        threshold_px: 15.0,
        enabled: true,
    };

    let world_near = lod_world(camera_near(), &hier, Some(settings));
    let world_mid = lod_world(camera_mid(), &hier, Some(settings));
    let world_far = lod_world(camera_far(), &hier, Some(settings));

    renderer_near.render(&world_near);
    let px_near = renderer_near.read_pixels();
    let near = renderer_near.renderer().stats().meshlet_surviving as usize;

    renderer_mid.render(&world_mid);
    let px_mid = renderer_mid.read_pixels();
    let mid = renderer_mid.renderer().stats().meshlet_surviving as usize;

    renderer_far.render(&world_far);
    let px_far = renderer_far.read_pixels();
    let far = renderer_far.renderer().stats().meshlet_surviving as usize;

    println!(
        "LOD 选中簇数：近 {near} / 中 {mid} / 远 {far}（叶总数 = {}）",
        hier.iter().map(|m| m.leaf_count() as usize).sum::<usize>()
    );
    assert!(near > 0 && mid > 0 && far > 0, "三机位都必须有簇存活");
    assert!(
        near > mid && mid > far,
        "距离单调：远选粗、近选细（近 {near} > 中 {mid} > 远 {far}）"
    );
    // 物体不消失：远景与空场景（同机位、零 meshlet 实体）必不同
    let empty = base_world(camera_far());
    renderer_far.render(&empty);
    let px_empty = renderer_far.read_pixels();
    assert_ne!(
        fnv1a(&px_far),
        fnv1a(&px_empty),
        "远景画面必须非空（物体未消失）"
    );
    // 画面确实随距离换粗细（像素流变化断言——轮廓近似保持由目检兜底）
    assert_ne!(
        fnv1a(&px_near),
        fnv1a(&px_far),
        "近 / 远景像素哈希必须不同（LOD 生效反证）"
    );
    assert_ne!(fnv1a(&px_mid), fnv1a(&px_empty), "中景画面必须非空");
}

/// 确定性：同机位同设置双会话逐字节一致；threshold 变化 ⇒ 哈希与选中
/// 数变化（近景下 τ=2 细化到叶、τ=60 粗化——两者的选择集必然不同）。
#[test]
fn lod_deterministic_across_sessions_and_threshold_changes_hash() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer_a) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let Some(mut renderer_b) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let Some(mut renderer_c) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let meshes = scene_meshes();
    let hier = hierarchy_meshes(&meshes);
    let fine = MeshletLodSettings {
        threshold_px: 2.0,
        enabled: true,
    };
    let coarse = MeshletLodSettings {
        threshold_px: 60.0,
        enabled: true,
    };

    let world = lod_world(camera_near(), &hier, Some(fine));
    renderer_a.render(&world);
    let px_a = renderer_a.read_pixels();
    renderer_b.render(&world);
    let px_b = renderer_b.read_pixels();
    assert_pixels_byte_equal(&px_a, &px_b, "双会话逐字节一致");

    // 阈值变粗：选择集收缩 ⇒ surviving 下降 + 哈希变化
    let world_coarse = lod_world(camera_near(), &hier, Some(coarse));
    renderer_c.render(&world_coarse);
    let px_c = renderer_c.read_pixels();
    let fine_count = renderer_a.renderer().stats().meshlet_surviving as usize;
    let coarse_count = renderer_c.renderer().stats().meshlet_surviving as usize;
    println!("近景阈值扫描：τ=2 → {fine_count} 簇 / τ=60 → {coarse_count} 簇");
    assert!(
        coarse_count < fine_count,
        "阈值变粗必须减少选中簇（{coarse_count} < {fine_count}）"
    );
    assert_ne!(fnv1a(&px_a), fnv1a(&px_c), "阈值变化必须改变像素流");
}
