//! C7b · Weighted Blended OIT 验收图：左右双面板（painter | OIT），
//! 输出 `iris_oit.ppm` → `docs/oit.png`。
//!
//! 场景：三块 α=0.45 自发光滤色板（红 / 绿 / 蓝）绕 Y 轴以 +60 / 0 /
//! −60° 交叉叠放，背后垫一块深色不透明板。三板板心同在 z=0 ⇒
//! 视深并列，painter 的先后完全由实体序决定——交叠区里「谁压谁」是
//! 任意的全局决定，而逐像素正确的遮挡（谁离相机近）在十字带两侧
//! 不断翻转 ⇒ **左右面板肉眼可见地不同**：painter 侧叠色按实体序
//! 一刀切，OIT 侧逐像素顺序无关（加权平均，§8.68① 声明其为近似
//! 算法——本图展示的是「顺序无关」性质，不是精确合成）。
//!
//! 硬断言：两面板画面必须不同（证明逐像素路径真的改变交叠结果）+
//! 各自双会话逐位一致（确定性文化）。
//!
//! ```sh
//! cargo run -p iris-render --example oit_snapshot --release
//! ffmpeg -y -i iris_oit.ppm docs/oit.png
//! ```

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
const GAP: u32 = 6;
/// 滤色板混合系数。
const ALPHA: f32 = 0.45;
/// 自发光强度（ACES 后各通道可分辨）。
const E: f32 = 2.0;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 自发光透明材质（albedo 黑，混合系数 α）。
fn tint(r: f32, g: f32, b: f32) -> MaterialDef {
    MaterialDef {
        base_color: [0.0, 0.0, 0.0, ALPHA],
        emissive: [r, g, b, 1.0],
        alpha_mode: AlphaMode::Blend,
        ..Default::default()
    }
}

fn scene(oit: bool) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 0.0, 12.0);
        camera.target = Vec3::ZERO;
    }
    world.insert_resource(DirectionalLight {
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    // 背景垫板：深色不透明（自发光近黑，让滤色板叠色主导画面）
    let yellow = world.resource_mut::<MaterialStore>().add(MaterialDef {
        emissive: [0.05, 0.06, 0.08, 1.0],
        ..MaterialDef::solid(0.0, 0.0, 0.0, 1.0)
    });
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 0.0, -5.0),
            scale: Vec3::new(8.0, 4.6, 0.1),
            ..Transform::IDENTITY
        },
        cube,
        yellow,
    ));
    let red = world.resource_mut::<MaterialStore>().add(tint(E, 0.0, 0.0));
    let green = world.resource_mut::<MaterialStore>().add(tint(0.0, E, 0.0));
    let blue = world.resource_mut::<MaterialStore>().add(tint(0.0, 0.0, E));
    for (deg, mat) in [(35.0f32, red), (0.0, green), (-35.0, blue)] {
        world.spawn((
            Transform {
                position: Vec3::ZERO,
                rotation: glam::Quat::from_rotation_y(deg.to_radians()),
                scale: Vec3::new(6.5, 3.4, 0.08),
            },
            cube,
            mat,
        ));
    }
    propagate_transforms(&mut world);
    if oit {
        world.insert_resource(OrderedIndependentTransparency);
    }
    world
}

fn render(world: &World) -> Vec<u8> {
    let mut renderer = OffscreenRenderer::new(WIDTH, HEIGHT).expect("离屏渲染器");
    renderer.render(world);
    renderer.read_pixels()
}

fn main() {
    let painter = render(&scene(false));
    let oited = render(&scene(true));
    let (hp, ho) = (fnv1a(&painter), fnv1a(&oited));
    assert_ne!(hp, ho, "双面板必须不同（逐像素路径真实改变交叠合成）");
    assert_eq!(hp, fnv1a(&render(&scene(false))), "painter 双会话逐位");
    assert_eq!(ho, fnv1a(&render(&scene(true))), "OIT 双会话逐位");

    // 合成左右面板（RGBA → P6 RGB）
    let mut ppm = format!("P6\n{} {}\n255\n", WIDTH * 2 + GAP, HEIGHT + GAP).into_bytes();
    let gap_row = vec![255u8; (WIDTH as usize * 2 + GAP as usize) * 3];
    for _ in 0..GAP {
        ppm.extend_from_slice(&gap_row);
    }
    for y in 0..HEIGHT {
        for _ in 0..GAP {
            ppm.extend_from_slice(&[255, 255, 255]);
        }
        for chunk in painter
            [(y as usize * WIDTH as usize) * 4..((y + 1) as usize * WIDTH as usize) * 4]
            .chunks_exact(4)
        {
            ppm.extend_from_slice(&chunk[..3]);
        }
        for chunk in oited
            [(y as usize * WIDTH as usize) * 4..((y + 1) as usize * WIDTH as usize) * 4]
            .chunks_exact(4)
        {
            ppm.extend_from_slice(&chunk[..3]);
        }
    }
    std::fs::write("iris_oit.ppm", &ppm).expect("写 iris_oit.ppm");
    println!("iris_oit.ppm 已写出（{} 字节）", ppm.len());
    println!("左 = painter（实体序一刀切）FNV-1a = {hp:#018x}");
    println!("右 = OIT（逐像素顺序无关）  FNV-1a = {ho:#018x}");
}
