//! 集成测试：虚拟几何体 V1 渲染等价性（R84，`meshlet` + `meshlet_builder`）。
//!
//! V1 的正确性锚点：meshlet 构建 = 「顶点重排 + 索引重排」，不透明几何
//! 的覆盖集与逐角插值不变 ⇒ 同一几何「常规网格路径 vs meshlet 构建后
//! 展开路径」渲染**逐字节一致**（MSAA 采样与深度逐样本同值——顶点
//! 属性值逐角相同，插值位级相同；场景无共面重叠面，绘制序不影响
//! 深度裁决）。此断言失败即真 bug（顶点重映射 / 展开错位），按
//! 「偶发失败 = 真 bug」追查，无阈值豁免。
//!
//! 测试面：
//! 1. **视角 A / 视角 B 逐字节相等**（两台独立渲染器分别服务常规 /
//!    meshlet 世界——渲染器按句柄值缓存 GPU 资源，同渲染器跨世界会
//!    串味，R53/R62 教训；不同相机角度两帧都等价）；
//! 2. **反空洞**：两角度全帧 FNV 必须不同（等价断言不是恒真比较）；
//! 3. **确定性**：同一世界重渲逐字节一致。
//!
//! 环境无 GPU 适配器时优雅跳过（打印提示，测试仍算通过）。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 屏幕尺寸（测试统一口径）。
const SIZE: u32 = 256;

/// 本套件串行化（ssr / shadow_v2 / probe 同款纪律）：同文件多测试并行
/// 共用单适配器会挂死——文件内互斥锁把并发度压到 1。
static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 场景几何：互不接触、无共面重叠面（深度并列会让绘制序影响像素，
/// 破坏「逐字节一致」断言的前提）。球体 2976 三角 ⇒ 数十簇。
fn scene_meshes() -> Vec<MeshData> {
    vec![
        MeshData::plane(10.0),            // 地板
        MeshData::uv_sphere(0.7, 48, 32), // 高细分球（多簇主角）
        MeshData::cube(),                 // 立方（6 簇）
        MeshData::cylinder(0.4, 1.0, 32), // 圆柱（侧面 + 双帽多簇）
    ]
}

/// meshlet 构建 + 展开（同几何的 meshlet 路径形态）。
fn expanded_meshes(meshes: &[MeshData]) -> Vec<MeshData> {
    meshes
        .iter()
        .map(|data| {
            let meshlet = iris_engine::meshlet_builder::build_meshlets_from_mesh_data(data);
            meshlet.expand()
        })
        .collect()
}

/// 相机 A：右前上方 3/4 视角。
fn camera_a() -> Camera {
    Camera {
        eye: Vec3::new(3.2, 2.0, 3.6),
        target: Vec3::new(0.0, 0.7, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    }
}

/// 相机 B：左后上方（与 A 差异显著——反空洞断言的支点）。
fn camera_b() -> Camera {
    Camera {
        eye: Vec3::new(-2.6, 3.4, -2.4),
        target: Vec3::new(0.0, 0.6, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    }
}

/// 场景世界：四件几何摆位互不接触（球心 y = 1.5 悬空、立方 / 圆柱底部
/// 高于地板 0.05+）；同一材质序 + 同一网格序——两世界仅网格内容不同。
fn scene_world(camera: Camera, meshes: &[MeshData]) -> World {
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

    for mesh in meshes {
        world.resource_mut::<MeshStore>().add(mesh.clone());
    }
    // 材质顺序固定：地板 / 球 / 立方 / 圆柱（逐语句借还——resource_mut
    // 的守卫存活到语句尾，数组字面量里四把 &mut 会撞重复借用）
    let mut floor_mat = MaterialDef::solid(0.30, 0.32, 0.35, 1.0);
    floor_mat.roughness = 0.95;
    let sphere_mat = MaterialDef::solid(0.75, 0.72, 0.68, 1.0);
    let cube_mat = MaterialDef::solid(0.80, 0.25, 0.20, 1.0);
    let cyl_mat = MaterialDef::solid(0.25, 0.45, 0.80, 1.0);
    let mat_floor = world.resource_mut::<MaterialStore>().add(floor_mat);
    let mat_sphere = world.resource_mut::<MaterialStore>().add(sphere_mat);
    let mat_cube = world.resource_mut::<MaterialStore>().add(cube_mat);
    let mat_cyl = world.resource_mut::<MaterialStore>().add(cyl_mat);
    let mats = [mat_floor, mat_sphere, mat_cube, mat_cyl];
    let placements: [(Vec3, Vec3); 4] = [
        // (位置, 缩放)
        (Vec3::ZERO, Vec3::ONE),
        (Vec3::new(0.0, 1.5, 0.0), Vec3::ONE),
        (Vec3::new(1.6, 0.7, -0.6), Vec3::new(1.2, 1.2, 1.2)),
        (Vec3::new(-1.5, 0.55, 0.4), Vec3::ONE),
    ];
    for (i, (position, scale)) in placements.into_iter().enumerate() {
        world.spawn((
            Transform {
                position,
                scale,
                ..Transform::IDENTITY
            },
            MeshHandle(i as u32),
            mats[i],
        ));
    }
    propagate_transforms(&mut world);
    world
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 逐字节相等断言（带首个差异定位——直接 assert_eq! 两个 256 KB 向量
/// 会把日志刷爆，这里给出可判别的最小信息）。
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
         左 {} vs 右 {}；全帧 FNV {:016x} vs {:016x})",
        diff % 4,
        a[diff],
        b[diff],
        fnv1a(a),
        fnv1a(b)
    );
}

/// 常规路径 vs meshlet 展开路径：视角 A 逐字节相等。
#[test]
fn meshlet_expanded_renders_byte_identical_view_a() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer_regular) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let Some(mut renderer_meshlet) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };

    let meshes = scene_meshes();
    let world_regular = scene_world(camera_a(), &meshes);
    let world_meshlet = scene_world(camera_a(), &expanded_meshes(&meshes));

    renderer_regular.render(&world_regular);
    let px_regular = renderer_regular.read_pixels();
    renderer_meshlet.render(&world_meshlet);
    let px_meshlet = renderer_meshlet.read_pixels();
    assert_pixels_byte_equal(
        &px_regular,
        &px_meshlet,
        "视角 A：meshlet 展开必须逐字节复现常规路径",
    );
}

/// 常规路径 vs meshlet 展开路径：视角 B（另一机位）逐字节相等。
#[test]
fn meshlet_expanded_renders_byte_identical_view_b() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer_regular) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let Some(mut renderer_meshlet) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };

    let meshes = scene_meshes();
    let world_regular = scene_world(camera_b(), &meshes);
    let world_meshlet = scene_world(camera_b(), &expanded_meshes(&meshes));

    renderer_regular.render(&world_regular);
    let px_regular = renderer_regular.read_pixels();
    renderer_meshlet.render(&world_meshlet);
    let px_meshlet = renderer_meshlet.read_pixels();
    assert_pixels_byte_equal(
        &px_regular,
        &px_meshlet,
        "视角 B：meshlet 展开必须逐字节复现常规路径",
    );
}

/// 反空洞：两角度全帧 FNV 必须不同（等价断言不是恒真比较——角度变化
/// 若不改像素，前两条测试的「相等」毫无证据力）。
#[test]
fn two_views_produce_different_frames() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };

    let meshes = scene_meshes();
    let world_a = scene_world(camera_a(), &meshes);
    let world_b = scene_world(camera_b(), &meshes);
    renderer.render(&world_a);
    let hash_a = fnv1a(&renderer.read_pixels());
    renderer.render(&world_b);
    let hash_b = fnv1a(&renderer.read_pixels());
    assert_ne!(hash_a, hash_b, "两个相机角度的全帧哈希必须不同");
}

/// 确定性：同一世界经同一渲染器重渲逐字节一致（V1 展开不引入时变性）。
#[test]
fn meshlet_render_is_deterministic_across_runs() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };

    let meshes = scene_meshes();
    let world = scene_world(camera_a(), &expanded_meshes(&meshes));
    renderer.render(&world);
    let first = renderer.read_pixels();
    let hash_first = fnv1a(&first);
    renderer.render(&world);
    let second = renderer.read_pixels();
    assert_eq!(hash_first, fnv1a(&second), "同世界重渲全帧 FNV 必须一致");
    assert_pixels_byte_equal(&first, &second, "同世界重渲必须逐字节一致");
}
