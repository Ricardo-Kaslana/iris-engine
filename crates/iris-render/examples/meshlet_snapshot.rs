//! 虚拟几何体 V1 验收图（R84）：同一几何「常规网格 | meshlet 构建后展开」
//! 并排渲染，2×2 输出 `iris_meshlet.ppm` → `docs/meshlet.png`。
//!
//! 面板布局（每格 480×270，同一场景：悬空高细分球 2976 三角 / 立方 /
//! 圆柱 + 地板；两台独立渲染器分别服务常规与 meshlet 世界——渲染器按
//! 句柄值缓存 GPU 资源，跨世界复用会串味）：
//!
//! ```text
//! ┌─────────────┬─────────────┐
//! │ 常规 @ 机位A │ meshlet @ A │   ← 两格全帧逐字节一致（FNV 断言）
//! ├─────────────┼─────────────┤
//! │ 常规 @ 机位B │ meshlet @ B │   ← 两格全帧逐字节一致
//! └─────────────┴─────────────┘
//! ```
//!
//! 断言（不通过直接 panic、不写文件）：
//! - 同机位常规 vs meshlet 全帧 FNV-1a **相等**（V1 正确性锚点：顶点
//!   重排不改像素——覆盖集与逐角插值不变）；
//! - 两机位 FNV **不等**（反空洞：等价断言不是恒真比较）；
//! - 球体采样点亮于地板（画面有效性）；
//! - 第二台 meshlet 渲染器重渲跨运行逐字节一致（确定性）。
//!
//! ```sh
//! cargo run -p iris-render --example meshlet_snapshot --release
//! ffmpeg -y -i iris_meshlet.ppm docs/meshlet.png
//! ```

use std::path::Path;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
const GAP: u32 = 6;

/// 场景几何：互不接触、无共面重叠面（深度并列会让绘制序影响像素）。
fn scene_meshes() -> Vec<MeshData> {
    vec![
        MeshData::plane(12.0),
        MeshData::uv_sphere(0.7, 48, 32), // 2976 三角 → 24 簇（多簇主角）
        MeshData::cube(),
        MeshData::cylinder(0.4, 1.0, 32),
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

/// 场景世界（meshlet 版传展开后的网格，摆位 / 材质 / 光照完全一致）。
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
    let placements = [
        (Vec3::ZERO, Vec3::ONE),
        (Vec3::new(0.0, 1.5, 0.0), Vec3::ONE),
        (Vec3::new(1.7, 0.7, -0.7), Vec3::new(1.2, 1.2, 1.2)),
        (Vec3::new(-1.6, 0.55, 0.4), Vec3::ONE),
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

/// 世界坐标 → 像素坐标（与渲染器同一条相机路径）。
fn project_to_pixel(camera: &Camera, p: Vec3, w: u32, h: u32) -> (f32, f32) {
    let aspect = w as f32 / h as f32;
    let view = camera.view_matrix();
    let proj = glam::camera::rh::proj::directx::perspective(
        camera.fov_y_radians,
        aspect,
        camera.z_near,
        camera.z_far,
    );
    let clip = proj * view * p.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    (
        (ndc.x * 0.5 + 0.5) * w as f32,
        (0.5 - ndc.y * 0.5) * h as f32,
    )
}

fn sample_lum(px: &[u8], camera: &Camera, at: Vec3, w: u32, h: u32) -> i32 {
    let (fx, fy) = project_to_pixel(camera, at, w, h);
    let (x, y) = (fx as u32, fy as u32);
    assert!(x < w && y < h, "采样点 ({fx},{fy}) 越界 {w}x{h}");
    let i = ((y * w + x) * 4) as usize;
    px[i] as i32 + px[i + 1] as i32 + px[i + 2] as i32
}

fn render_frame(renderer: &mut OffscreenRenderer, world: &World) -> (Vec<u8>, u64) {
    renderer.render(world);
    let px = renderer.read_pixels();
    let hash = fnv1a(&px);
    (px, hash)
}

fn main() {
    let Some(mut renderer_regular) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut renderer_meshlet) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    let meshes = scene_meshes();
    let meshlet_meshes: Vec<MeshData> = meshes
        .iter()
        .map(|data| iris_engine::meshlet_builder::build_meshlets_from_mesh_data(data).expand())
        .collect();

    // ---- 四帧：{常规, meshlet} × {机位A, 机位B} ----
    let world_reg_a = scene_world(camera_a(), &meshes);
    let world_reg_b = scene_world(camera_b(), &meshes);
    let world_mlt_a = scene_world(camera_a(), &meshlet_meshes);
    let world_mlt_b = scene_world(camera_b(), &meshlet_meshes);

    let (reg_a, hash_reg_a) = render_frame(&mut renderer_regular, &world_reg_a);
    let (reg_b, hash_reg_b) = render_frame(&mut renderer_regular, &world_reg_b);
    let (mlt_a, hash_mlt_a) = render_frame(&mut renderer_meshlet, &world_mlt_a);
    let (mlt_b, hash_mlt_b) = render_frame(&mut renderer_meshlet, &world_mlt_b);

    // ---- 等价断言（V1 正确性锚点）----
    assert_eq!(
        hash_mlt_a, hash_reg_a,
        "机位 A：meshlet 展开路径必须与常规路径全帧逐字节一致"
    );
    assert_eq!(
        hash_mlt_b, hash_reg_b,
        "机位 B：meshlet 展开路径必须与常规路径全帧逐字节一致"
    );
    assert_ne!(hash_reg_a, hash_reg_b, "两机位全帧哈希必须不同（反空洞）");
    assert_ne!(hash_mlt_a, hash_mlt_b, "meshlet 路径两机位全帧哈希必须不同");

    // ---- 画面有效性：球体（网格中心）显著亮于地板远角 ----
    let sphere_at = Vec3::new(0.0, 1.6, 0.0);
    let floor_at = Vec3::new(-2.2, 0.0, -2.2);
    let lum_sphere = sample_lum(&reg_a, &camera_a(), sphere_at, WIDTH, HEIGHT);
    let lum_floor = sample_lum(&reg_a, &camera_a(), floor_at, WIDTH, HEIGHT);
    println!("亮度采样：球面 = {lum_sphere}，地板 = {lum_floor}");
    assert!(
        lum_sphere > lum_floor + 60,
        "球面采样点必须显著亮于地板（{lum_sphere} vs {lum_floor}）"
    );

    // ---- 跨运行确定性：第二台 meshlet 渲染器重渲，逐字节一致 ----
    let Some(mut renderer_meshlet2) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let (_, h2a) = render_frame(&mut renderer_meshlet2, &world_mlt_a);
    let (_, h2b) = render_frame(&mut renderer_meshlet2, &world_mlt_b);
    assert_eq!(h2a, hash_mlt_a, "机位 A meshlet 帧跨运行必须逐位一致");
    assert_eq!(h2b, hash_mlt_b, "机位 B meshlet 帧跨运行必须逐位一致");

    // ---- 2×2 面板拼装 ----
    let stride = WIDTH * 2 + GAP;
    let total_h = HEIGHT * 2 + GAP;
    let mut panel = vec![0u8; (stride * total_h) as usize * 4];
    let blit = |panel: &mut [u8], src: &[u8], x0: u32, y0: u32| {
        for row in 0..HEIGHT as usize {
            let sy = row * WIDTH as usize * 4;
            let dy = ((y0 as usize + row) * stride as usize + x0 as usize) * 4;
            let n = WIDTH as usize * 4;
            panel[dy..dy + n].copy_from_slice(&src[sy..sy + n]);
        }
    };
    blit(&mut panel, &reg_a, 0, 0);
    blit(&mut panel, &mlt_a, WIDTH + GAP, 0);
    blit(&mut panel, &reg_b, 0, HEIGHT + GAP);
    blit(&mut panel, &mlt_b, WIDTH + GAP, HEIGHT + GAP);

    // ---- 写 PPM（供 ffmpeg 转 PNG 入 docs/）----
    let header = format!("P6\n{stride} {total_h}\n255\n");
    let mut ppm = header.into_bytes();
    for chunk in panel.chunks_exact(4) {
        ppm.extend_from_slice(&chunk[..3]);
    }
    std::fs::write(Path::new("iris_meshlet.ppm"), &ppm).expect("写 iris_meshlet.ppm");
    println!(
        "iris_meshlet.ppm 已写出（左列 = 常规路径，右列 = meshlet 展开路径；上行 = 机位 A，下行 = 机位 B）"
    );
    println!("FNV-1a 常规@A = {hash_reg_a:#018x}");
    println!("FNV-1a meshlet@A = {hash_mlt_a:#018x}（与常规@A 必须相同）");
    println!("FNV-1a 常规@B = {hash_reg_b:#018x}");
    println!("FNV-1a meshlet@B = {hash_mlt_b:#018x}（与常规@B 必须相同）");
}
