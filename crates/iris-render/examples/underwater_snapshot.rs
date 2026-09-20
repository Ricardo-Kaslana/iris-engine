//! R94 · 水下验收图：三面板（水下看海床 + 焦散 | 同机位资源关闭对照 |
//! 水下仰望 Snell 窗），输出 `iris_underwater.ppm` → `docs/underwater.png`。
//!
//! 左面板：相机在水面下、俯角朝海床——海床（y = −4）上的**焦散亮筋**，
//! 叠水体吸收（远处向吸收色收敛）与太阳内散射。
//! 中面板：**同机位同钟，`UnderwaterFog` 资源缺席**——即 R93 之前的渲染
//! 本身（水下列吸收 / 焦散 / 抖动整段不入链），左右并置即水下效果差分。
//! 右面板：相机下潜更深、仰头看水面——检验裁定 1 的**解析列长钳制**：
//! 视线穿出波面后水下列长只算到水面（不是算到水面之后的远平面），
//! 因此 Snell 窗与从下方透亮的波面可见，而不是被整段深水吞成一片暗色。
//!
//! 硬断言：三面板各自双会话逐位一致（确定性文化）+ 左 ≠ 中（水下真实生效）
//! + 右面板上部（朝上视线）≠ 中面板同点（列长钳制改变了水面像素的呈现）
//! + 焦散开启档的海床条带差分像素数过阈值（焦散不是死项）。
//!
//! ```sh
//! cargo run -p iris-render --example underwater_snapshot --release
//! ffmpeg -y -i iris_underwater.ppm docs/underwater.png
//! ```

use std::time::Duration;

use glam::{Vec2, Vec3};
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Time, Transform, World};
use iris_render::prelude::*;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 320;
const GAP: u32 = 6;
const STEP: Duration = Duration::from_nanos(1_000_000_000 / 60);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    /// 水下看海床（资源在场）。
    Under,
    /// 同机位、资源缺席（对照 = R93 现状）。
    Dry,
    /// 水下仰望水面（资源在场）。
    Window,
}

fn camera(panel: Panel) -> Camera {
    let (eye, target) = match panel {
        Panel::Under | Panel::Dry => (Vec3::new(0.0, -1.8, 5.0), Vec3::new(0.0, -4.2, -11.0)),
        Panel::Window => (Vec3::new(0.0, -2.5, 6.0), Vec3::new(0.0, 4.0, -10.0)),
    };
    Camera {
        eye,
        target,
        fov_y_radians: 60f32.to_radians(),
        ..Camera::default()
    }
}

fn scene(panel: Panel) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(camera(panel));
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.25, -1.0, -0.2).normalize(),
        color: Vec3::new(1.0, 0.97, 0.9),
        intensity: 1.8,
        ambient: Vec3::new(0.14, 0.16, 0.19),
        shadows: false,
        shadow_range: 60.0,
    });
    world.insert_resource(BloomSettings::disabled());
    if panel != Panel::Dry {
        world.insert_resource(UnderwaterFog {
            // 验收档：吸收强一点、焦散亮一点，让三处效果在 480×320 下可辨
            absorb_depth: 14.0,
            sun_inscatter: 2.2,
            sky_window: 5.0,
            caustic_strength: 0.8,
            wobble: 0.003,
            ..UnderwaterFog::default()
        });
    }
    let bed_mesh = world
        .resource_mut::<MeshStore>()
        .add(MeshData::plane(160.0));
    let cube_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let ball_mesh = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(0.7, 20, 14));
    let bed_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.30, 0.27, 0.22, 1.0));
    let crate_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.55, 0.33, 0.12, 1.0));
    let buoy_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.8, 0.72, 0.25, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, -4.0, -12.0),
            ..Transform::IDENTITY
        },
        bed_mesh,
        bed_mat,
    ));
    // 海床上的两只箱子 + 一只浮球（焦散与吸收的参照物，也给出尺度感）
    world.spawn((
        Transform {
            position: Vec3::new(-2.2, -3.3, -9.0),
            scale: Vec3::splat(0.7),
            ..Transform::IDENTITY
        },
        cube_mesh,
        crate_mat,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(2.6, -3.5, -13.0),
            scale: Vec3::new(1.1, 0.5, 0.8),
            rotation: glam::Quat::from_rotation_y(0.6),
        },
        cube_mesh,
        crate_mat,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(0.8, -1.2, -7.0),
            ..Transform::IDENTITY
        },
        ball_mesh,
        buoy_mat,
    ));
    // 水面（±24 m 矩形，y = 0）：右面板从下方看的就是它
    world.spawn((WaterSurface::new(0.0, Vec2::splat(24.0)),));
    let mut time = Time::default();
    for _ in 0..120 {
        time.advance(STEP);
    }
    world.insert_resource(time);
    propagate_transforms(&mut world);
    world
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

fn pixel(px: &[u8], x: u32, y: u32) -> [i32; 3] {
    let i = ((y * WIDTH + x) * 4) as usize;
    [px[i] as i32, px[i + 1] as i32, px[i + 2] as i32]
}

/// 海床条带（画面下 1/5）的差分像素数——焦散亮筋的判别区。
fn band_changed(a: &[u8], b: &[u8]) -> usize {
    let mut n = 0;
    for y in (HEIGHT * 4 / 5)..HEIGHT {
        for x in 0..WIDTH {
            let p = pixel(a, x, y);
            let q = pixel(b, x, y);
            if (0..3).any(|i| (p[i] - q[i]).abs() >= 4) {
                n += 1;
            }
        }
    }
    n
}

fn render(panel: Panel, gpu: &mut OffscreenRenderer) -> Vec<u8> {
    let world = scene(panel);
    for _ in 0..2 {
        gpu.render(&world);
    }
    gpu.read_pixels()
}

fn main() {
    let Some(mut gpu) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let under = render(Panel::Under, &mut gpu);
    let dry = render(Panel::Dry, &mut gpu);
    let window = render(Panel::Window, &mut gpu);
    let (hu, hd, hw) = (fnv1a(&under), fnv1a(&dry), fnv1a(&window));
    // 双会话逐位一致（同一世界序列第二次跑）
    assert_eq!(
        hu,
        fnv1a(&render(Panel::Under, &mut gpu)),
        "水下面板双会话逐位一致"
    );
    assert_eq!(
        hd,
        fnv1a(&render(Panel::Dry, &mut gpu)),
        "对照面板双会话逐位一致"
    );
    assert_eq!(
        hw,
        fnv1a(&render(Panel::Window, &mut gpu)),
        "Snell 窗面板双会话逐位一致"
    );
    assert_ne!(hu, hd, "水下效果必须真实改变画面（资源在场 vs 缺席）");
    assert_ne!(
        hw, hd,
        "仰望水面的列长钳制必须改变呈现（否则裁定 1 的解析钳制没生效）"
    );
    // 焦散不是死项：海床条带在开 / 关两档间应有大量像素不同
    let caustic_on = fnv1a(&under);
    let mut g2 = OffscreenRenderer::new(WIDTH, HEIGHT).expect("环境应有 GPU 适配器");
    let no_caustic = {
        let mut world = scene(Panel::Under);
        let mut u = world
            .try_resource::<UnderwaterFog>()
            .map(|f| *f)
            .expect("水下资源在场");
        u.caustic_strength = 0.0;
        world.insert_resource(u);
        for _ in 0..2 {
            g2.render(&world);
        }
        g2.read_pixels()
    };
    assert_ne!(
        caustic_on,
        fnv1a(&no_caustic),
        "caustic_strength 须是活旋钮"
    );
    let caustic_band = band_changed(&under, &no_caustic);
    assert!(
        caustic_band > 1500,
        "焦散应在海床条带留下可辨亮筋（差分像素数），实得 {caustic_band}"
    );
    // 朝上视线的水面像素：右面板上部与对照同点必须不同（列长钳制的直接体现）
    let top = (WIDTH / 2, HEIGHT / 6);
    assert_ne!(
        pixel(&window, top.0, top.1),
        pixel(&dry, top.0, top.1),
        "仰望方向的同点像素须被水下 pass 改写"
    );

    // 三面板合成（P6 RGB，白缝 + 顶边，与 R92/R93 同款口径）
    let panels = [&under, &dry, &window];
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
    std::fs::write("iris_underwater.ppm", &ppm).expect("写 iris_underwater.ppm");
    println!("iris_underwater.ppm 已写出（{} 字节）", ppm.len());
    println!("左 = 水下海床 + 焦散       FNV-1a = {hu:#018x}");
    println!("中 = 同机位资源关闭对照   FNV-1a = {hd:#018x}");
    println!("右 = 水下仰望 Snell 窗     FNV-1a = {hw:#018x}");
    println!("焦散海床条带差分像素数 = {caustic_band}");
}
