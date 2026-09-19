//! R92 · 降水验收图：三面板（雨+湿地+涟漪 | 积雪+降雪 | 干燥对照），
//! 输出 `iris_precipitation.ppm` → `docs/precipitation.png`。
//!
//! 左面板：雨（`ParticleEmitterV2::rain` 预设原值——**相机位于降水体积
//! 内部**，游戏相机的真实位形）+ 湿表面（wetness 0.85）+ 带水面的地形
//!（涟漪强度 1.0——雨打水面的法线扰动）+ 一根立柱（湿暗 + 坡度积雪
//! 语义的对照物）。相机在体积内平视。
//!
//! 中面板：同地形 + 积雪（snow 1.0——坡度遮罩：地面满雪、柱身露原色）
//! + 降雪（`snow` 预设原值）。
//!
//! 右面板：同地形**无天气资源**（干燥对照——缺席 = R92 之前的渲染本身，
//! 与左/中的湿暗 / 雪白对照即肉眼差分）。
//!
//! 硬断言：三面板各自双会话逐位一致（确定性文化）+ 雨面板 ≠ 干燥面板
//! + 雪面板 ≠ 干燥面板（天气真实生效的 FNV 差分旁证）。
//!
//! ```sh
//! cargo run -p iris-render --example precipitation_snapshot --release
//! ffmpeg -y -i iris_precipitation.ppm docs/precipitation.png
//! ```

use glam::{Vec2, Vec3};
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Time, Transform, World};
use iris_render::prelude::*;

const WIDTH: u32 = 960;
const HEIGHT: u32 = 540;
const GAP: u32 = 6;
const STEP: std::time::Duration = std::time::Duration::from_nanos(1_000_000_000 / 60);

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 降水地形：灰褐地面（y = 0，40 m）+ 蓝灰立柱（z = −3，高 5 m，坡度
/// 遮罩的对照物）+ 水面（右侧 z < −6 半场，半边长 14 m——相机平视时
/// 水面占画面上半远景）。相机位于降水体积内部平视略仰（雨丝暗剪影
/// 衬亮天空）。`kind` 决定天气资源与发射器。
fn terrain_scene(kind: WeatherKind) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 2.0, 4.5);
        camera.target = Vec3::new(0.0, 1.9, -8.0);
    }
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.35, -1.0, -0.25).normalize(),
        intensity: 1.6,
        ambient: Vec3::new(0.12, 0.13, 0.15),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    world.insert_resource(Time::default());
    let ground_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let ground_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.34, 0.30, 0.26, 1.0));
    let pillar_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.24, 0.30, 0.42, 1.0));
    world.spawn((Transform::IDENTITY, ground_mesh, ground_mat));
    world.spawn((
        Transform {
            position: Vec3::new(2.2, 2.5, -3.0),
            scale: Vec3::new(1.2, 5.0, 1.2),
            ..Transform::IDENTITY
        },
        cube,
        pillar_mat,
    ));
    match kind {
        WeatherKind::Dry => {}
        WeatherKind::Rain => {
            world.insert_resource(Weather {
                wetness: 0.85,
                ripple: 1.0,
                ..Weather::default()
            });
            world.spawn((
                Transform::from_xyz(0.0, 5.0, 0.0),
                ParticleEmitterV2::rain(),
            ));
        }
        WeatherKind::Snow => {
            world.insert_resource(Weather {
                snow: 1.0,
                ..Weather::default()
            });
            world.spawn((
                Transform::from_xyz(0.0, 5.0, 0.0),
                ParticleEmitterV2::snow(),
            ));
        }
    }
    // 水面（雨面板才有涟漪可观；雪面板水面照常波形——snow 无涟漪门）
    world.spawn((WaterSurface {
        height: -0.15,
        half_extent: Vec2::new(14.0, 8.0),
        wave_speed: if matches!(kind, WeatherKind::Dry) {
            0.0
        } else {
            0.8
        },
        ..WaterSurface::default()
    },));
    propagate_transforms(&mut world);
    // 降水推进 75 帧（1.25 s——雨丝铺满视锥、雪花进入视野；Time 已推
    // 进使涟漪相位展开）
    if !matches!(kind, WeatherKind::Dry) {
        for _ in 0..75 {
            world.resource_mut::<Time>().advance(STEP);
            propagate_transforms(&mut world);
            update_particles_v2(&world);
        }
    } else {
        for _ in 0..75 {
            world.resource_mut::<Time>().advance(STEP);
        }
    }
    world
}

enum WeatherKind {
    Dry,
    Rain,
    Snow,
}

fn render(world: &World) -> Vec<u8> {
    let mut renderer = OffscreenRenderer::new(WIDTH, HEIGHT).expect("离屏渲染器");
    renderer.render(world);
    renderer.read_pixels()
}

fn main() {
    let rain = render(&terrain_scene(WeatherKind::Rain));
    let snow = render(&terrain_scene(WeatherKind::Snow));
    let dry = render(&terrain_scene(WeatherKind::Dry));
    let (hr, hn, hd) = (fnv1a(&rain), fnv1a(&snow), fnv1a(&dry));
    assert_eq!(
        hr,
        fnv1a(&render(&terrain_scene(WeatherKind::Rain))),
        "雨面板双会话逐位一致"
    );
    assert_eq!(
        hn,
        fnv1a(&render(&terrain_scene(WeatherKind::Snow))),
        "雪面板双会话逐位一致"
    );
    assert_eq!(
        hd,
        fnv1a(&render(&terrain_scene(WeatherKind::Dry))),
        "干燥面板双会话逐位一致"
    );
    assert_ne!(hr, hd, "雨面板应不同于干燥面板（湿表面 + 涟漪 + 雨丝生效）");
    assert_ne!(hn, hd, "雪面板应不同于干燥面板（积雪遮罩 + 雪花生效）");

    // 合成三面板（RGBA → P6 RGB，两条白缝分隔 + 顶部白边——每行 =
    // panel GAP panel GAP panel，与头声明宽度逐字节一致）
    let panels = [&rain, &snow, &dry];
    let mut ppm = format!("P6\n{} {}\n255\n", WIDTH * 3 + GAP * 2, HEIGHT + GAP).into_bytes();
    let gap_row = vec![255u8; (WIDTH as usize * 3 + GAP as usize * 2) * 3];
    for _ in 0..GAP {
        ppm.extend_from_slice(&gap_row);
    }
    for y in 0..HEIGHT {
        for (pi, panel) in panels.iter().enumerate() {
            if pi > 0 {
                for _ in 0..GAP {
                    ppm.extend_from_slice(&[255, 255, 255]);
                }
            }
            for chunk in panel
                [(y as usize * WIDTH as usize) * 4..((y + 1) as usize * WIDTH as usize) * 4]
                .chunks_exact(4)
            {
                ppm.extend_from_slice(&chunk[..3]);
            }
        }
    }
    std::fs::write("iris_precipitation.ppm", &ppm).expect("写 iris_precipitation.ppm");
    println!("iris_precipitation.ppm 已写出（{} 字节）", ppm.len());
    println!("左 = 雨 + 湿表面 + 水面涟漪   FNV-1a = {hr:#018x}");
    println!("中 = 积雪坡度遮罩 + 降雪     FNV-1a = {hn:#018x}");
    println!("右 = 干燥对照（无天气资源） FNV-1a = {hd:#018x}");
}
