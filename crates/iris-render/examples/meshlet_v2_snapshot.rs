//! 虚拟几何体 V2 验收图（R85）：同一几何「expand 常规路径 | compute 剔除
//! 路径」并排渲染，1×2 输出 `iris_meshlet_v2.ppm` → `docs/meshlet_v2.png`。
//!
//! 面板布局（每格 480×270，同一场景：地面 + 悬空高细分球 + 立方，全部
//! 默认材质——meshlet 路径 v1 单材质口径；两台独立渲染器分别服务两路，
//! 渲染器按句柄值缓存 GPU 资源，跨世界复用会串味）：
//!
//! ```text
//! ┌──────────────────┬──────────────────┐
//! │ expand 常规路径 @A │ compute 剔除路径 @A │  ← 两格全帧逐字节一致
//! └──────────────────┴──────────────────┘
//! ```
//!
//! 断言（不通过直接 panic、不写文件）：
//! - **全帧 FNV-1a 相等**（R85 最强断言的跨管线形态）：expand 路径经
//!   standard.wgsl 完整管线；compute 路径 = vertex-pull 顶点阶段 +
//!   **同一个 fs_main 片元阶段** + GPU 簇剔除紧凑列表——剔除只删不可见
//!   几何、顶点变换逐式同构 ⇒ 像素必须恒等；
//! - 反空洞：compute 路径两机位 FNV 必须不同；
//! - `FrameStats.meshlet_surviving` ∈ (0, 簇总数]（剔除生效且非全剔）；
//! - 紧凑列表严格升序（stable compaction，绘制序确定性）；
//! - 第二台 compute 渲染器重渲跨运行逐位一致（确定性）。
//!
//! ```sh
//! cargo run -p iris-render --example meshlet_v2_snapshot --release
//! ffmpeg -y -i iris_meshlet_v2.ppm docs/meshlet_v2.png
//! ```

use std::path::Path;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
const GAP: u32 = 6;

/// 场景几何：互不接触、无共面重叠面。球体 2976 三角 ⇒ 数十簇（剔除
/// 主力）；立方全向锥永不锥剔（对照组）；地面单簇大球（保守剔除样本）。
fn scene_meshes() -> Vec<MeshData> {
    vec![
        MeshData::plane(10.0),
        MeshData::uv_sphere(0.7, 48, 32),
        MeshData::cube(),
    ]
}

fn camera_a() -> Camera {
    Camera {
        eye: Vec3::new(3.4, 2.2, 3.8),
        target: Vec3::new(0.0, 0.7, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    }
}

fn camera_b() -> Camera {
    Camera {
        eye: Vec3::new(-2.8, 3.6, -2.6),
        target: Vec3::new(0.0, 0.6, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    }
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

/// expand 常规路径世界：簇展开为常规网格 + 默认材质（与 meshlet 路径
/// 消费的材质逐字段一致）。
fn expand_world(camera: Camera, meshes: &[MeshData]) -> World {
    let mut world = base_world(camera);
    for mesh in meshes {
        let cluster_mesh = iris_engine::meshlet_builder::build_meshlets_from_mesh_data(mesh);
        world.resource_mut::<MeshStore>().add(cluster_mesh.expand());
    }
    for (i, position) in placements().into_iter().enumerate() {
        world.spawn((
            Transform {
                position,
                ..Transform::IDENTITY
            },
            MeshHandle(i as u32),
            MaterialHandle(0), // 默认材质——meshlet 路径 v1 单材质口径
        ));
    }
    propagate_transforms(&mut world);
    world
}

/// compute 剔除路径世界：`MeshletInstance` 实体（材质 = 默认句柄 0）。
fn compute_world(camera: Camera, meshes: &[MeshletMesh]) -> (World, usize) {
    let mut world = base_world(camera);
    let mut store = MeshletMeshStore::new();
    let mut total_clusters = 0usize;
    for mesh in meshes {
        total_clusters += mesh.clusters.len();
        store.add(mesh.clone());
    }
    world.insert_resource(store);
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
    (world, total_clusters)
}

fn placements() -> [Vec3; 3] {
    [
        Vec3::ZERO,
        Vec3::new(0.0, 1.5, 0.0),
        Vec3::new(1.7, 0.5, -0.7),
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

fn main() {
    let Some(mut renderer_expand) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut renderer_compute) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    let meshes = scene_meshes();
    let meshlets: Vec<MeshletMesh> = meshes
        .iter()
        .map(iris_engine::meshlet_builder::build_meshlets_from_mesh_data)
        .collect();
    let total_clusters = meshlets.iter().map(|m| m.clusters.len()).sum::<usize>();

    let world_expand_a = expand_world(camera_a(), &meshes);
    let (world_compute_a, _) = compute_world(camera_a(), &meshlets);
    let (world_compute_b, _) = compute_world(camera_b(), &meshlets);

    // expand 路径（常规管线，1 帧即稳态）
    renderer_expand.render(&world_expand_a);
    let px_expand = renderer_expand.read_pixels();
    let hash_expand = fnv1a(&px_expand);

    // compute 路径：帧 1 = 恒等全量兜底，等待读回，帧 2 = GPU 剔除
    renderer_compute.render(&world_compute_a);
    let _ = renderer_compute.read_pixels(); // 帧 1 只用于建立紧凑回读
    renderer_compute
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("读回等待失败");
    renderer_compute.render(&world_compute_a);
    let surviving_a = renderer_compute.renderer().stats().meshlet_surviving as usize;
    let px_compute = renderer_compute.read_pixels();
    let hash_compute_a = fnv1a(&px_compute);

    // ---- 断言（不通过直接 panic、不写文件）----
    assert_eq!(
        hash_expand, hash_compute_a,
        "expand 常规路径与 compute 剔除路径全帧 FNV 必须相等\
         （剔除只删不可见几何 + 顶点变换逐式同构 + 同一 fs_main）"
    );
    assert!(
        surviving_a > 0 && surviving_a <= total_clusters,
        "meshlet_surviving = {surviving_a} 应落在 (0, {total_clusters}]"
    );
    let list = renderer_compute
        .renderer()
        .meshlet_consumed_list()
        .expect("compute 帧应已消费到紧凑列表");
    assert_eq!(list.len(), surviving_a, "列表长度应与存活数一致");
    assert!(
        list.windows(2).all(|w| w[0] < w[1]),
        "紧凑列表必须严格升序（stable compaction）"
    );

    // 反空洞：compute 路径两机位 FNV 必须不同
    renderer_compute.render(&world_compute_b);
    renderer_compute
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("读回等待失败");
    renderer_compute.render(&world_compute_b);
    let px_compute_b = renderer_compute.read_pixels();
    let hash_compute_b = fnv1a(&px_compute_b);
    assert_ne!(
        hash_compute_a, hash_compute_b,
        "compute 路径两机位全帧哈希必须不同（反空洞）"
    );

    // 跨运行确定性：第二台 compute 渲染器重渲机位 A（两帧）逐位一致
    let Some(mut renderer_compute2) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    renderer_compute2.render(&world_compute_a);
    let _ = renderer_compute2.read_pixels();
    renderer_compute2
        .gpu()
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("读回等待失败");
    renderer_compute2.render(&world_compute_a);
    let hash_compute_a2 = fnv1a(&renderer_compute2.read_pixels());
    assert_eq!(
        hash_compute_a, hash_compute_a2,
        "compute 路径跨渲染器（跨运行）必须逐位一致"
    );

    // ---- 1×2 面板拼装 ----
    let stride = WIDTH * 2 + GAP;
    let mut panel = vec![0u8; (stride * HEIGHT) as usize * 4];
    let blit = |panel: &mut [u8], src: &[u8], x0: u32| {
        for row in 0..HEIGHT as usize {
            let sy = row * WIDTH as usize * 4;
            let dy = (row * stride as usize + x0 as usize) * 4;
            let n = WIDTH as usize * 4;
            panel[dy..dy + n].copy_from_slice(&src[sy..sy + n]);
        }
    };
    blit(&mut panel, &px_expand, 0);
    blit(&mut panel, &px_compute, WIDTH + GAP);

    // ---- 写 PPM（供 ffmpeg 转 PNG 入 docs/）----
    let header = format!("P6\n{stride} {HEIGHT}\n255\n");
    let mut ppm = header.into_bytes();
    for chunk in panel.chunks_exact(4) {
        ppm.extend_from_slice(&chunk[..3]);
    }
    std::fs::write(Path::new("iris_meshlet_v2.ppm"), &ppm).expect("写 iris_meshlet_v2.ppm");
    println!("iris_meshlet_v2.ppm 已写出（左 = expand 常规路径，右 = compute 剔除路径）");
    println!("FNV-1a expand @A   = {hash_expand:#018x}");
    println!("FNV-1a compute @A  = {hash_compute_a:#018x}（与 expand 必须相同）");
    println!("FNV-1a compute @B  = {hash_compute_b:#018x}（反空洞）");
    println!("meshlet_surviving = {surviving_a}/{total_clusters}（剔除生效口径）");
}
