//! 昼夜循环验收图（R80）：2×2 四相位面板，输出 `iris_daynight.ppm`
//! → `docs/daynight.png`。
//!
//! 面板布局（2 排 × 2 列，同一山野场景——地面 + 环形箱体遮挡物 +
//! 大气天空 + 适度体积雾）：**日出 0.04 | 正午 0.25 / 黄昏 0.46 |
//! 午夜 0.75**。每格相机朝向该相位太阳方位（昼侧俯仰 24°、夜间俯视
//! −6°——见 `camera_for`），太阳位置沿弧线在四格间移动（日出低角度
//! 暖光 → 正午高角度白光 → 黄昏反向低角暖光 → 午夜无日光只剩环境下
//! 限）。灯光不经插件——example 内直接按 `iris_core::daynight` 纯函数
//! 算好 `DirectionalLight` 插入（与插件写入路径同源的四个字段）。
//!
//! 每格带硬断言（正午地面亮度 ≫ 午夜 / 黄昏太阳侧 R>B 色温可判别 /
//! 午夜不黑屏——环境下限生效；不通过直接 panic、不写文件）；跨运行
//! 逐字节一致（同输入两次渲染 FNV-1a 全帧哈希相同，打印供比对）。
//!
//! ```sh
//! cargo run -p iris-render --example daynight_snapshot --release
//! ffmpeg -y -i iris_daynight.ppm docs/daynight.png
//! ```

use std::path::Path;

use glam::Vec3;
use iris_core::daynight::{ambient_envelope, sun_direction, sun_envelope};
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
const GAP: u32 = 6;

/// 四相位（与面板序一致）：日出 / 正午 / 黄昏 / 午夜。
const PHASES: [f64; 4] = [0.04, 0.25, 0.46, 0.75];

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

/// 相位 → 相机：眼点固定，视线朝该相位太阳的水平方位。昼侧俯仰 24°
/// （低相位太阳入画，正午太阳位于画面上缘附近——弧线在四格间可见）；
/// 夜间太阳在地平线下（无月亮 / 星空，§8.49），改俯视 −6° 让环境下限
/// 照亮的地面入画——「午夜不黑屏」目检的直接证据。
fn camera_for(phase: f64) -> Camera {
    let sun = sun_direction(phase);
    let horiz = Vec3::new(sun.x, 0.0, sun.z);
    let horiz = if horiz.length() < 1e-4 {
        Vec3::NEG_Z
    } else {
        horiz.normalize()
    };
    let eye = Vec3::new(0.0, 2.6, 0.0);
    let pitch = if iris_core::daynight::is_night(phase) {
        -6.0f32.to_radians()
    } else {
        24.0f32.to_radians()
    };
    let dir = (horiz * pitch.cos() + Vec3::Y * pitch.sin()).normalize();
    Camera {
        eye,
        target: eye + dir * 40.0,
        fov_y_radians: 64f32.to_radians(),
        ..Camera::default()
    }
}

/// 山野世界：地面 + 环形箱体（任意方位角都有遮挡物与光影）、大气天空
/// 与适度雾；方向光按纯函数从相位算出（不经插件——快照钉的是包络
/// 本体，插件接线由 iris-engine headless 测试守护）。
fn build_world(phase: f64) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(camera_for(phase));
    // 方向光 = 昼夜纯函数的直接换算（direction = −sun，光照语义）
    let sun = sun_direction(phase);
    let (color, intensity) = sun_envelope(phase);
    world.insert_resource(DirectionalLight {
        direction: -sun,
        color,
        intensity,
        ambient: ambient_envelope(phase),
        shadows: true,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    // IBL 整帧关闭（资源自述的测试用途）：否则常亮 env cubemap 辐照
    // 会在夜间盖过环境下限，四相位的光照包络差异无从判别——本 example
    // 要钉的就是「环境光包络 + 太阳包络」这一条链路
    world.insert_resource(EnvironmentSettings { intensity: 0.0 });
    world.insert_resource(SkySettings {
        atmosphere: true,
        ..SkySettings::default()
    });
    world.insert_resource(FogSettings {
        enabled: true,
        density: 0.03,
        ..FogSettings::default()
    });
    let mut ground_mat = MaterialDef::solid(0.32, 0.40, 0.28, 1.0);
    ground_mat.roughness = 0.95;
    let rock_mat = MaterialDef::solid(0.52, 0.50, 0.47, 1.0);
    let ground_mesh = world
        .resource_mut::<MeshStore>()
        .add(MeshData::plane(400.0));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let ground = world.resource_mut::<MaterialStore>().add(ground_mat);
    let rock = world.resource_mut::<MaterialStore>().add(rock_mat);
    world.spawn((Transform::IDENTITY, ground_mesh, ground));
    // 环形箱体：8 方位 × 两种半径——四格相机的视野里都有遮挡物与
    // 投射阴影，太阳方位随相位转时光影随之转
    for i in 0..8 {
        let azimuth = i as f32 / 8.0 * std::f32::consts::TAU;
        let radius = if i % 2 == 0 { 12.0 } else { 22.0 };
        let s = 1.2 + (i % 3) as f32 * 0.7;
        world.spawn((
            Transform {
                position: Vec3::new(azimuth.cos() * radius, s * 0.5, azimuth.sin() * radius),
                scale: Vec3::splat(s),
                ..Transform::IDENTITY
            },
            cube,
            rock,
        ));
    }
    propagate_transforms(&mut world);
    world
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

fn sample_pixel(px: &[u8], camera: &Camera, at: Vec3, w: u32, h: u32) -> [i32; 3] {
    let (fx, fy) = project_to_pixel(camera, at, w, h);
    let (x, y) = (fx as u32, fy as u32);
    assert!(x < w && y < h, "采样点 ({fx},{fy}) 越界 {w}x{h}");
    pixel(px, w, x, y)
}

/// 把一格画面拷进面板缓冲。
fn blit(panel: &mut [u8], src: &[u8], x0: u32, y0: u32) {
    let stride = (WIDTH * 2 + GAP) as usize;
    for row in 0..HEIGHT as usize {
        let sy = row * WIDTH as usize * 4;
        let dy = ((y0 as usize + row) * stride + x0 as usize) * 4;
        let n = WIDTH as usize * 4;
        panel[dy..dy + n].copy_from_slice(&src[sy..sy + n]);
    }
}

fn main() {
    let Some(mut renderer) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut panel = vec![0u8; ((WIDTH * 2 + GAP) * (HEIGHT * 2 + GAP)) as usize * 4];
    let col_x = |c: u32| c * (WIDTH + GAP);
    let row_y = |r: u32| r * (HEIGHT + GAP);

    // 渲染四相位并取采样（面板序：左上日出 / 右上正午 / 左下黄昏 /
    // 右下午夜）。地面采样点 = 各相机视线前方 20 m 的地面（画面下部
    // ——相位间同一点同口径）；黄昏太阳侧采样点 = 太阳方向 80 m 处
    // 向下偏 6 m（避开 HDR 太阳盘，落在 Mie 晕与内散射的暖区）。
    let mut frames = Vec::new();
    let mut hashes = [0u64; 4];
    for (i, &phase) in PHASES.iter().enumerate() {
        let world = build_world(phase);
        let camera = camera_for(phase);
        let frame = {
            renderer.render(&world);
            renderer.read_pixels()
        };
        hashes[i] = fnv1a(&frame);
        frames.push((phase, camera, frame));
    }
    blit(&mut panel, &frames[0].2, 0, row_y(0));
    blit(&mut panel, &frames[1].2, col_x(1), row_y(0));
    blit(&mut panel, &frames[2].2, 0, row_y(1));
    blit(&mut panel, &frames[3].2, col_x(1), row_y(1));

    // ---- 断言 1：正午地面 ≫ 午夜地面（直射 + 全昼环境 vs 下限底光） ----
    let ground_point = |camera: &Camera| {
        let horiz = Vec3::new(
            camera.target.x - camera.eye.x,
            0.0,
            camera.target.z - camera.eye.z,
        )
        .normalize();
        camera.eye + horiz * 20.0 - Vec3::Y * camera.eye.y
    };
    let noon_ground = luminance(sample_pixel(
        &frames[1].2,
        &frames[1].1,
        ground_point(&frames[1].1),
        WIDTH,
        HEIGHT,
    ));
    let midnight_ground = luminance(sample_pixel(
        &frames[3].2,
        &frames[3].1,
        ground_point(&frames[3].1),
        WIDTH,
        HEIGHT,
    ));
    let ground_delta = noon_ground - midnight_ground;
    println!("正午地面={noon_ground} 午夜地面={midnight_ground}（Δ={ground_delta}）");
    assert!(
        ground_delta > 40,
        "正午地面亮度 {noon_ground} 必须显著高于午夜 {midnight_ground}（阈值 40）"
    );

    // ---- 断言 2：午夜不黑屏（ambient 下限生效） ----
    assert!(
        midnight_ground >= 8,
        "午夜地面 {midnight_ground} 不得黑屏（NIGHT_AMBIENT 下限应可辨）"
    );

    // ---- 断言 3：黄昏太阳侧 R > B（暖色温可判别） ----
    let dusk_cam = &frames[2].1;
    let sun = sun_direction(PHASES[2]);
    let sun_side = Vec3::new(0.0, 2.6, 0.0) + sun * 80.0 - Vec3::Y * 6.0;
    let dusk_rgb = sample_pixel(&frames[2].2, dusk_cam, sun_side, WIDTH, HEIGHT);
    println!(
        "黄昏太阳侧 RGB=({},{},{})",
        dusk_rgb[0], dusk_rgb[1], dusk_rgb[2]
    );
    assert!(
        dusk_rgb[0] > dusk_rgb[2] + 15,
        "黄昏太阳侧必须偏暖（R={} vs B={}，阈值 15）",
        dusk_rgb[0],
        dusk_rgb[2]
    );
    // 午夜同视角对照（打印用，无硬断言）：沿午夜相机视线 80 m 下偏
    // 6 m 的天空点——构造上必在画面内（太阳在地平线下，无法按太阳
    // 方向取点）
    let night_cam = &frames[3].1;
    let night_forward = (night_cam.target - night_cam.eye).normalize();
    let night_side = night_cam.eye + night_forward * 80.0 - Vec3::Y * 6.0;
    let night_rgb = sample_pixel(&frames[3].2, night_cam, night_side, WIDTH, HEIGHT);
    println!(
        "午夜太阳侧 RGB=({},{},{})",
        night_rgb[0], night_rgb[1], night_rgb[2]
    );

    // ---- 断言 4：跨运行确定性——同输入第二会话逐位一致 ----
    let mut renderer_b = OffscreenRenderer::new(WIDTH, HEIGHT).expect("确定性渲染器 B");
    for (i, &phase) in PHASES.iter().enumerate() {
        let world = build_world(phase);
        renderer_b.render(&world);
        assert_eq!(
            fnv1a(&renderer_b.read_pixels()),
            hashes[i],
            "相位 {phase} 双会话必须逐位一致"
        );
    }

    // ---- 写 PPM（供 ffmpeg 转 PNG 入 docs/）----
    let header = format!("P6\n{} {}\n255\n", WIDTH * 2 + GAP, HEIGHT * 2 + GAP);
    let mut ppm = header.into_bytes();
    for chunk in panel.chunks_exact(4) {
        ppm.extend_from_slice(&chunk[..3]);
    }
    std::fs::write(Path::new("iris_daynight.ppm"), &ppm).expect("写 iris_daynight.ppm");
    println!("iris_daynight.ppm 已写出");
    for (i, name) in ["日出", "正午", "黄昏", "午夜"].iter().enumerate() {
        println!("{name} FNV-1a = {:#018x}", hashes[i]);
    }
}
