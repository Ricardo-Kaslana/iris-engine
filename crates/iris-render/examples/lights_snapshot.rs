//! 前向光源分箱 + SpotLight 验收图（R78）：2×2 网格，输出 `iris_lights.ppm`
//! → `docs/spotlight.png`。
//!
//! 面板布局（每格 480×270，同一地板场景、IBL 关零隔离灯光信号）：
//! 1. **左上：8 点光基线**——8 盏聚簇点光照亮画面右侧，左侧角落落在
//!    range 并集之外（Globals 均摊路径，分箱关闭）；
//! 2. **右上：9+ 点光（分箱生效）**——同场景追加 4 盏照向左侧暗角
//!    （12 盏点光 > `MAX_POINT_LIGHTS`，自动阈值翻到分箱路径，暗角
//!    被点亮——突破 8 盏均摊的直接证据）；
//! 3. **左下：SpotLight 关**——8 点光场景的暗角原样；
//! 4. **右下：SpotLight 开**——同场景加一盏锥光照向暗角（锥内地面
//!    亮、锥外不受影响——软边锥形可见）。
//!
//! 每格带硬断言（暗角采样点显著变亮；不通过直接 panic、不写文件）；
//! 跨运行逐字节一致（同输入两次渲染的 FNV-1a 全帧哈希相同，本示例打印
//! 哈希供比对）。
//!
//! ```sh
//! cargo run -p iris-render --example lights_snapshot --release
//! ffmpeg -y -i iris_lights.ppm docs/spotlight.png
//! ```

use std::path::Path;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
const GAP: u32 = 6;

/// 公共相机（高角度俯视，左角与右中地面都在画面内）。
fn scene_camera() -> Camera {
    Camera {
        eye: Vec3::new(2.4, 2.6, 2.8),
        target: Vec3::new(-1.2, 0.4, -1.2),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    }
}

/// 地板世界：单面地板 + 近零环境光 + 零直射光 + IBL 关零（隔离灯光）。
fn floor_world() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(scene_camera());
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.4, -1.0, -0.3).normalize(),
        intensity: 0.0,
        ambient: Vec3::new(0.02, 0.02, 0.025),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(EnvironmentSettings { intensity: 0.0 });
    world.insert_resource(BloomSettings::disabled());
    let mut solid = MaterialDef::solid(0.6, 0.6, 0.62, 1.0);
    solid.roughness = 0.9;
    let mat = world.resource_mut::<MaterialStore>().add(solid);
    let floor_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(12.0));
    world.spawn((Transform::IDENTITY, floor_mesh, mat));
    propagate_transforms(&mut world);
    world
}

fn add_point_light(world: &mut World, position: Vec3, range: f32, intensity: f32) {
    world.spawn((
        Transform {
            position,
            ..Transform::IDENTITY
        },
        PointLight::new(Vec3::ONE, intensity, range),
    ));
}

/// 8 盏聚簇点光（画面右侧，Globals 均摊路径的临界盏数；左侧暗角在
/// range 并集之外）。
fn cluster_world() -> World {
    let mut world = floor_world();
    for x in [0.5f32, 1.5] {
        for z in [0.2f32, 1.2, 2.2, 3.2] {
            add_point_light(&mut world, Vec3::new(x, 1.2, z), 2.5, 12.0);
        }
    }
    propagate_transforms(&mut world);
    world
}

/// 左侧暗角地面采样点（8 盏聚簇灯的 range 并集之外）。
const DARK_CORNER: Vec3 = Vec3::new(-2.3, 0.0, -2.3);

/// 追加 4 盏照向暗角的点光（总 12 盏 > 8 ⇒ 分箱自动生效）。
fn with_corner_lights(world: &mut World) {
    for p in [
        Vec3::new(-1.2, 1.0, -2.2),
        Vec3::new(-2.2, 1.0, -1.2),
        Vec3::new(-1.2, 1.0, -0.2),
        Vec3::new(-2.2, 1.0, -2.2),
    ] {
        add_point_light(world, p, 2.2, 10.0);
    }
    propagate_transforms(world);
}

/// 追加一盏照向暗角的锥光（斜轴软边，锥外地面不受影响）。
fn with_corner_spot(world: &mut World) {
    world.spawn((
        Transform {
            position: Vec3::new(-1.5, 2.8, -1.7),
            ..Transform::IDENTITY
        },
        SpotLight::new(
            Vec3::ONE,
            80.0,
            10.0,
            Vec3::new(-0.3, -1.0, 0.0).normalize(),
            25.0f32.to_radians(),
            40.0f32.to_radians(),
        ),
    ));
    propagate_transforms(world);
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn pixel(px: &[u8], w: u32, x: u32, y: u32) -> [i32; 3] {
    let i = ((y * w + x) * 4) as usize;
    [px[i] as i32, px[i + 1] as i32, px[i + 2] as i32]
}

fn luminance(p: [i32; 3]) -> i32 {
    p[0] + p[1] + p[2]
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

fn sample_lum(px: &[u8], at: Vec3, w: u32, h: u32) -> i32 {
    let (fx, fy) = project_to_pixel(&scene_camera(), at, w, h);
    let (x, y) = (fx as u32, fy as u32);
    assert!(x < w && y < h, "采样点 ({fx},{fy}) 越界 {w}x{h}");
    luminance(pixel(px, w, x, y))
}

/// 渲染一帧（两遍走同一条路径——分箱无「首帧中性」语义，单帧即生效）。
fn render_frame(renderer: &mut OffscreenRenderer, world: &World) -> (Vec<u8>, u64) {
    renderer.render(world);
    let px = renderer.read_pixels();
    let hash = fnv1a(&px);
    (px, hash)
}

fn main() {
    let Some(mut renderer) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    // ---- 四个世界 ----
    let world_base8 = cluster_world(); // 左上：8 点光基线
    let mut world_12 = cluster_world(); // 右上：12 点光（分箱生效）
    with_corner_lights(&mut world_12);
    let world_spot_off = cluster_world(); // 左下：spot 关
    let mut world_spot_on = cluster_world(); // 右下：spot 开
    with_corner_spot(&mut world_spot_on);

    let (base8, hash_base8) = render_frame(&mut renderer, &world_base8);
    let (p12, hash_12) = render_frame(&mut renderer, &world_12);
    let (spot_off, hash_spot_off) = render_frame(&mut renderer, &world_spot_off);
    let (spot_on, hash_spot_on) = render_frame(&mut renderer, &world_spot_on);

    // ---- 面板拼装（2×2）----
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
    blit(&mut panel, &base8, 0, 0);
    blit(&mut panel, &p12, WIDTH + GAP, 0);
    blit(&mut panel, &spot_off, 0, HEIGHT + GAP);
    blit(&mut panel, &spot_on, WIDTH + GAP, HEIGHT + GAP);

    // ---- 有效性断言（暗角采样点：分箱让第 9+ 盏生效；锥光照亮暗角）----
    let corner_tl = sample_lum(&base8, DARK_CORNER, WIDTH, HEIGHT);
    let corner_tr = sample_lum(&p12, DARK_CORNER, WIDTH, HEIGHT);
    let corner_bl = sample_lum(&spot_off, DARK_CORNER, WIDTH, HEIGHT);
    let corner_br = sample_lum(&spot_on, DARK_CORNER, WIDTH, HEIGHT);
    println!(
        "暗角：TL(8灯)={corner_tl} TR(12灯)={corner_tr} BL(spot关)={corner_bl} BR(spot开)={corner_br}"
    );
    assert!(
        corner_tr > corner_tl + 24,
        "12 盏点光的暗角必须显著亮于 8 盏基线（分箱未生效？TL = {corner_tl}，TR = {corner_tr}）"
    );
    assert!(
        corner_br > corner_bl + 24,
        "SpotLight 开启后暗角必须显著变亮（BL = {corner_bl}，BR = {corner_br}）"
    );
    assert_ne!(hash_spot_on, hash_spot_off, "锥光开关必须改变全帧");

    // ---- 跨运行确定性：独立渲染器重渲四帧，逐字节一致 ----
    let mut renderer2 = OffscreenRenderer::new(WIDTH, HEIGHT).expect("确定性渲染器");
    let (_, h2) = render_frame(&mut renderer2, &world_base8);
    assert_eq!(h2, hash_base8, "8 点光基线跨运行必须逐位一致");
    let (_, h2) = render_frame(&mut renderer2, &world_12);
    assert_eq!(h2, hash_12, "12 点光（分箱）跨运行必须逐位一致");
    let (_, h2) = render_frame(&mut renderer2, &world_spot_off);
    assert_eq!(h2, hash_spot_off, "spot 关跨运行必须逐位一致");
    let (_, h2) = render_frame(&mut renderer2, &world_spot_on);
    assert_eq!(h2, hash_spot_on, "spot 开跨运行必须逐位一致");

    // ---- 写 PPM（供 ffmpeg 转 PNG 入 docs/）----
    let header = format!("P6\n{stride} {total_h}\n255\n");
    let mut ppm = header.into_bytes();
    for chunk in panel.chunks_exact(4) {
        ppm.extend_from_slice(&chunk[..3]);
    }
    std::fs::write(Path::new("iris_lights.ppm"), &ppm).expect("写 iris_lights.ppm");
    println!("iris_lights.ppm 已写出");
    println!("FNV-1a  8灯基线 = {hash_base8:#018x}");
    println!("FNV-1a 12灯分箱 = {hash_12:#018x}");
    println!("FNV-1a spot 关  = {hash_spot_off:#018x}");
    println!("FNV-1a spot 开  = {hash_spot_on:#018x}");
}
