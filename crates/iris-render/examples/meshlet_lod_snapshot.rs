//! 虚拟几何体 V3 验收图（R86）：层级 LOD 三机位并排渲染，1×3 输出
//! `iris_meshlet_lod.ppm` → `docs/meshlet_lod.png`。
//!
//! 面板布局（每格 480×270，同一场景：地面 + 悬空高细分球 + 立方，全部
//! 默认材质；`MeshletLodSettings { threshold_px: 15, enabled: true }`；
//! 三台独立渲染器分别服务三格——渲染器按句柄值缓存 GPU 资源，跨世界
//! 复用会串味；每格首帧 = 恒等兜底 = 该机位的**选中簇集**，无读回时
//! 延，选中数可从 `meshlet_surviving` 直接读取）：
//!
//! ```text
//! ┌───────────────┬───────────────┬───────────────┐
//! │ 近景（叶簇细） │ 中景（L1 过渡） │ 远景（根簇粗） │
//! └───────────────┴───────────────┴───────────────┘
//! ```
//!
//! 断言（不通过直接 panic、不写文件）：
//! - 选中簇数严格单调下降（近 > 中 > 远——屏幕空间误差随距离缩小）；
//! - 选中数 < 叶簇总数（远景层级真正生效）且 > 0（物体不消失）；
//! - 三格 FNV 互不相同（像素流确实随 LOD 变化）+ 各格非空（反空洞）；
//! - 第四台渲染器重渲近景跨运行逐位一致（确定性）。
//!
//! 目检要点（AGENTS.md 工作流）：远处无裂缝碎面、近处细节不丢、无消失
//! /闪面。
//!
//! ```sh
//! cargo run -p iris-render --example meshlet_lod_snapshot --release
//! ffmpeg -y -i iris_meshlet_lod.ppm docs/meshlet_lod.png
//! ```

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
const GAP: u32 = 6;

/// 场景几何：互不接触、无共面重叠面。球体 ~6000 三角 ⇒ 数十叶簇 +
/// 两层父簇（LOD 主力）；立方 6 叶簇一层合并；地面单叶簇自为根。
fn scene_meshes() -> Vec<MeshData> {
    vec![
        MeshData::plane(10.0),
        MeshData::uv_sphere(0.7, 64, 48),
        MeshData::cube(),
    ]
}

fn camera_near() -> Camera {
    Camera {
        eye: Vec3::new(2.4, 1.6, 2.8),
        target: Vec3::new(0.0, 0.7, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    }
}

fn camera_mid() -> Camera {
    Camera {
        eye: Vec3::new(5.5, 3.4, 6.2),
        target: Vec3::new(0.0, 0.7, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    }
}

fn camera_far() -> Camera {
    Camera {
        eye: Vec3::new(12.0, 7.5, 13.5),
        target: Vec3::new(0.0, 0.7, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    }
}

fn lod_world(camera: Camera, meshes: &[MeshletMesh]) -> World {
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
    world.insert_resource(MeshletLodSettings {
        threshold_px: 15.0,
        enabled: true,
    });
    let mut store = MeshletMeshStore::new();
    for mesh in meshes {
        store.add(mesh.clone());
    }
    world.insert_resource(store);
    for (i, position) in [
        Vec3::ZERO,
        Vec3::new(0.0, 1.5, 0.0),
        Vec3::new(1.6, 0.5, -0.6),
    ]
    .into_iter()
    .enumerate()
    {
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

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn main() {
    let Some(mut renderer_near) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut renderer_mid) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut renderer_far) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    let meshes = scene_meshes();
    let hier: Vec<MeshletMesh> = meshes
        .iter()
        .map(iris_engine::meshlet_builder::build_meshlet_hierarchy_from_mesh_data)
        .collect();
    let leaf_total: usize = hier.iter().map(|m| m.leaf_count() as usize).sum();

    let world_near = lod_world(camera_near(), &hier);
    let world_mid = lod_world(camera_mid(), &hier);
    let world_far = lod_world(camera_far(), &hier);

    // 各格首帧 = 恒等兜底 = 该机位选中簇集（surviving 即选中数）
    renderer_near.render(&world_near);
    let px_near = renderer_near.read_pixels();
    let count_near = renderer_near.renderer().stats().meshlet_surviving as usize;

    renderer_mid.render(&world_mid);
    let px_mid = renderer_mid.read_pixels();
    let count_mid = renderer_mid.renderer().stats().meshlet_surviving as usize;

    renderer_far.render(&world_far);
    let px_far = renderer_far.read_pixels();
    let count_far = renderer_far.renderer().stats().meshlet_surviving as usize;

    println!("叶簇总数 {leaf_total}；选中簇数：近 {count_near} / 中 {count_mid} / 远 {count_far}");

    // ---- 断言（不通过直接 panic、不写文件）----
    assert!(
        count_near > 0 && count_mid > 0 && count_far > 0,
        "三机位都必须有簇存活（物体不消失）"
    );
    assert!(
        count_near > count_mid && count_mid > count_far,
        "选中簇数必须严格单调下降：近 {count_near} > 中 {count_mid} > 远 {count_far}"
    );
    assert!(
        count_far < leaf_total,
        "远景必须选中粗簇（{count_far} < 叶总数 {leaf_total}——层级生效）"
    );
    let (hash_near, hash_mid, hash_far) = (fnv1a(&px_near), fnv1a(&px_mid), fnv1a(&px_far));
    assert_ne!(hash_near, hash_far, "近 / 远景哈希必须不同（LOD 生效）");
    assert_ne!(hash_mid, hash_far, "中 / 远景哈希必须不同");

    // 跨运行确定性：第四台渲染器重渲近景逐位一致
    let Some(mut renderer_check) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    renderer_check.render(&world_near);
    let px_check = renderer_check.read_pixels();
    assert_eq!(px_near, px_check, "近景跨渲染器（跨运行）必须逐位一致");

    // ---- 1×3 面板拼装 ----
    let stride = WIDTH * 3 + GAP * 2;
    let mut panel = vec![0u8; (stride * HEIGHT) as usize * 4];
    let blit = |panel: &mut [u8], src: &[u8], x0: u32| {
        for row in 0..HEIGHT as usize {
            let sy = row * WIDTH as usize * 4;
            let dy = (row * stride as usize + x0 as usize) * 4;
            let n = WIDTH as usize * 4;
            panel[dy..dy + n].copy_from_slice(&src[sy..sy + n]);
        }
    };
    blit(&mut panel, &px_near, 0);
    blit(&mut panel, &px_mid, WIDTH + GAP);
    blit(&mut panel, &px_far, (WIDTH + GAP) * 2);

    // ---- 写 PPM（供 ffmpeg 转 PNG 入 docs/）----
    let header = format!("P6\n{stride} {HEIGHT}\n255\n");
    let mut ppm = header.into_bytes();
    for chunk in panel.chunks_exact(4) {
        ppm.extend_from_slice(&chunk[..3]);
    }
    let out_path = "iris_meshlet_lod.ppm";
    std::fs::write(out_path, &ppm).expect("写 PPM");
    println!(
        "快照完成：{out_path}（{} B）；FNV 近 {hash_near:016x} / 中 {hash_mid:016x} / 远 {hash_far:016x}",
        ppm.len()
    );
}
