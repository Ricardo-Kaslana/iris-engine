//! 天空 v2（大气散射）与体积雾验收图（R79）：2×2 面板对照，输出
//! `iris_sky_fog.ppm` → `docs/sky_fog.png`。
//!
//! 面板布局（2 排 × 2 列，每排「左 | 右」并排，同一山野场景）：
//! 1. **旧渐变天空 | 大气天空**：同一场景同一相机（太阳入画）——左格
//!    是 R78 前的 env cubemap 程序化渐变，右格是片元内解析单次散射
//!    （Rayleigh 蓝天 + Mie 太阳周向光晕 + HDR 太阳盘 + 地平线雾色）；
//! 2. **大气+雾关 | 大气+雾开**：雾海展示——开启后地面与远处箱体按
//!    距离与高度被雾吞没（近处天线杆保持清晰——雾的距离单调性同时
//!    可见），天空被内散射染出太阳周向亮区。
//!
//! 每格带硬断言（开关必须改变全帧 / 天空采样点色移可判别 / 远近物体
//! 雾化差可判别；不通过直接 panic、不写文件）；跨运行逐字节一致（同
//! 输入两次渲染的 FNV-1a 全帧哈希相同，本示例打印哈希供比对）。
//!
//! ```sh
//! cargo run -p iris-render --example sky_fog_snapshot --release
//! ffmpeg -y -i iris_sky_fog.ppm docs/sky_fog.png
//! ```

use std::path::Path;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
const GAP: u32 = 6;

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

/// 山野场景公共参数。
struct Scene {
    /// 第 1 排相机（抬头构图：蓝天渐变 + 太阳盘，天空对比的判别处）。
    sky_cam: Camera,
    /// 第 2 排相机（平视构图：地面雾海 + 远近物体层次）。
    fog_cam: Camera,
    /// 天空采样点（第 1 排画面内的高仰角天空）。
    sky_point: Vec3,
    /// 近处天线杆采样点（雾开应保持相对清晰）。
    near_point: Vec3,
    /// 远处箱体采样点（雾开应显著雾化）。
    far_point: Vec3,
}

fn scene() -> Scene {
    Scene {
        // 抬头 35°：画面主体是天空（高仰角的饱和蓝只有抬头才看得到——
        // 低仰角天空物理上就是发白的）
        sky_cam: Camera {
            eye: Vec3::new(0.0, 2.4, 14.0),
            target: Vec3::new(0.0, 38.0, -30.0),
            fov_y_radians: 55f32.to_radians(),
            ..Camera::default()
        },
        // 平视：地平线在画面中部，雾海与远近箱体同框
        fog_cam: Camera {
            eye: Vec3::new(0.0, 2.4, 14.0),
            target: Vec3::new(0.0, 3.4, -30.0),
            fov_y_radians: 55f32.to_radians(),
            ..Camera::default()
        },
        // 第 1 排画面内的高仰角（~55°）天空点
        sky_point: Vec3::new(-11.8, 51.0, -18.5),
        near_point: Vec3::new(-3.0, 1.6, -6.5),
        far_point: Vec3::new(5.5, 1.0, -44.0),
    }
}

/// 山野世界：大地 + 散布箱体 + 近处细天线杆。`camera` 逐排给定，
/// `atmosphere` = 大气天空，`fog` = 体积雾设置（None = 缺席）。
fn build_world(camera: &Camera, atmosphere: bool, fog: Option<FogSettings>) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(camera.clone());
    // 太阳仰角 ~43°（白日位形——蓝天判别需要太阳路径透射不过分红化）
    let sun_dir = Vec3::new(0.45, 1.05, -1.0).normalize();
    world.insert_resource(DirectionalLight {
        direction: -sun_dir,
        intensity: 1.2,
        ambient: Vec3::new(0.16, 0.18, 0.22),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    if atmosphere {
        world.insert_resource(SkySettings {
            atmosphere: true,
            ..SkySettings::default()
        });
    }
    if let Some(fog) = fog {
        world.insert_resource(fog);
    }
    let mut ground_mat = MaterialDef::solid(0.32, 0.40, 0.28, 1.0);
    ground_mat.roughness = 0.95;
    let mut rock_mat = MaterialDef::solid(0.52, 0.50, 0.47, 1.0);
    rock_mat.roughness = 0.9;
    let pole_mat = MaterialDef::solid(0.25, 0.22, 0.20, 1.0);
    let ground_mesh = world
        .resource_mut::<MeshStore>()
        .add(MeshData::plane(400.0));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let ground = world.resource_mut::<MaterialStore>().add(ground_mat);
    let rock = world.resource_mut::<MaterialStore>().add(rock_mat);
    let pole = world.resource_mut::<MaterialStore>().add(pole_mat);
    world.spawn((Transform::IDENTITY, ground_mesh, ground));
    // 远中近三组箱体（雾的距离层次）
    for (x, z, s) in [
        (5.5, -44.0, 2.0),
        (-6.0, -30.0, 1.6),
        (8.0, -18.0, 1.2),
        (-10.0, -22.0, 1.4),
        (2.5, -10.0, 0.9),
    ] {
        world.spawn((
            Transform {
                position: Vec3::new(x, s * 0.5, z),
                scale: Vec3::splat(s),
                ..Transform::IDENTITY
            },
            cube,
            rock,
        ));
    }
    // 近处细天线杆（雾开仍应清晰可辨——检验雾不吃近物）
    world.spawn((
        Transform {
            position: Vec3::new(-3.0, 3.0, -6.5),
            scale: Vec3::new(0.12, 6.0, 0.12),
            ..Transform::IDENTITY
        },
        cube,
        pole,
    ));
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

fn sample_lum(px: &[u8], camera: &Camera, at: Vec3, w: u32, h: u32) -> i32 {
    let (fx, fy) = project_to_pixel(camera, at, w, h);
    let (x, y) = (fx as u32, fy as u32);
    assert!(x < w && y < h, "采样点 ({fx},{fy}) 越界 {w}x{h}");
    luminance(pixel(px, w, x, y))
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
    let scene = scene();
    let mut panel = vec![0u8; ((WIDTH * 2 + GAP) * (HEIGHT * 2 + GAP)) as usize * 4];
    let col_x = |c: u32| c * (WIDTH + GAP);
    let row_y = |r: u32| r * (HEIGHT + GAP);

    // ---- 面板 1（左上）：旧渐变天空（R78 前基线，抬头构图） ----
    let world_legacy = build_world(&scene.sky_cam, false, None);
    let legacy = {
        renderer.render(&world_legacy);
        renderer.read_pixels()
    };
    // ---- 面板 2（右上）：大气天空（同一抬头构图） ----
    let world_atmo = build_world(&scene.sky_cam, true, None);
    let atmo = {
        renderer.render(&world_atmo);
        renderer.read_pixels()
    };
    blit(&mut panel, &legacy, 0, row_y(0));
    blit(&mut panel, &atmo, col_x(1), row_y(0));
    let legacy_hash = fnv1a(&legacy);
    let atmo_hash = fnv1a(&atmo);
    assert_ne!(legacy_hash, atmo_hash, "大气开关必须改变全帧");

    // 天空采样点色移（旧渐变 vs 解析大气）
    let sky_legacy = sample_lum(&legacy, &scene.sky_cam, scene.sky_point, WIDTH, HEIGHT);
    let sky_atmo = sample_lum(&atmo, &scene.sky_cam, scene.sky_point, WIDTH, HEIGHT);
    println!(
        "面板1 天空点 legacy={sky_legacy} atmo={sky_atmo}（Δ={}）",
        sky_atmo - sky_legacy
    );
    assert!(
        (sky_atmo - sky_legacy).abs() >= 20,
        "大气天空必须显著改变天空采样点（Δ = {}）",
        sky_atmo - sky_legacy
    );

    // ---- 面板 3（左下）：大气 + 雾关（平视构图） ----
    let world_fog_off = build_world(&scene.fog_cam, true, None);
    let fog_off = {
        renderer.render(&world_fog_off);
        renderer.read_pixels()
    };
    // ---- 面板 4（右下）：大气 + 雾开（雾海） ----
    let world_fog_on = build_world(
        &scene.fog_cam,
        true,
        Some(FogSettings {
            enabled: true,
            density: 0.055,
            ..FogSettings::default()
        }),
    );
    let fog_on = {
        renderer.render(&world_fog_on);
        renderer.read_pixels()
    };
    blit(&mut panel, &fog_off, 0, row_y(1));
    blit(&mut panel, &fog_on, col_x(1), row_y(1));
    let fog_off_hash = fnv1a(&fog_off);
    let fog_on_hash = fnv1a(&fog_on);
    assert_ne!(fog_off_hash, fog_on_hash, "雾开关必须改变全帧");

    // 近处天线杆 vs 远处箱体：远物的雾化偏移必须显著大于近物
    let near_off = sample_lum(&fog_off, &scene.fog_cam, scene.near_point, WIDTH, HEIGHT);
    let near_on = sample_lum(&fog_on, &scene.fog_cam, scene.near_point, WIDTH, HEIGHT);
    let far_off = sample_lum(&fog_off, &scene.fog_cam, scene.far_point, WIDTH, HEIGHT);
    let far_on = sample_lum(&fog_on, &scene.fog_cam, scene.far_point, WIDTH, HEIGHT);
    let near_delta = (near_on - near_off).abs();
    let far_delta = (far_on - far_off).abs();
    println!(
        "面板2 近杆 off={near_off} on={near_on}（Δ={near_delta}），远箱 off={far_off} on={far_on}（Δ={far_delta}）"
    );
    assert!(
        far_delta >= 20,
        "远处箱体必须显著雾化（off={far_off} on={far_on}）"
    );
    assert!(
        near_delta >= 15,
        "近处天线杆也应有可感知雾变（off={near_off} on={near_on}）"
    );

    // ---- 跨运行确定性：面板 4 世界双会话逐字节一致 ----
    let mut renderer_b = OffscreenRenderer::new(WIDTH, HEIGHT).expect("确定性渲染器 B");
    renderer_b.render(&world_fog_on);
    renderer_b.render(&world_fog_on);
    assert_eq!(
        fnv1a(&renderer_b.read_pixels()),
        fog_on_hash,
        "同输入双会话必须逐位一致"
    );

    // ---- 写 PPM（供 ffmpeg 转 PNG 入 docs/）----
    let header = format!("P6\n{} {}\n255\n", WIDTH * 2 + GAP, HEIGHT * 2 + GAP);
    let mut ppm = header.into_bytes();
    for chunk in panel.chunks_exact(4) {
        ppm.extend_from_slice(&chunk[..3]);
    }
    std::fs::write(Path::new("iris_sky_fog.ppm"), &ppm).expect("写 iris_sky_fog.ppm");
    println!("iris_sky_fog.ppm 已写出");
    println!("旧渐变天空 FNV-1a = {legacy_hash:#018x}");
    println!("大气天空   FNV-1a = {atmo_hash:#018x}");
    println!("雾关       FNV-1a = {fog_off_hash:#018x}");
    println!("雾开       FNV-1a = {fog_on_hash:#018x}");
}
