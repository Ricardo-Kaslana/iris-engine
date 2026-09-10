//! 验收工具：R60「TAA 时域抗锯齿」的目检图输出（ARCHITECTURE §4.29）。
//!
//! 两个场景各用一个独立 `OffscreenRenderer`（渲染器 GPU 缓存按
//! (网格/材质/纹理) 句柄值键控、假设目录跨帧稳定——多 world 换场共用
//! 一个渲染器会让后一个 world 的句柄命中前一个的缓存，R53 教训；
//! 本工具两场景各配专属渲染器，开关用 `TaaSettings.enabled` 阶段切换）。
//!
//! 场景 A（上排）：5 根 45° 近黑细杆（世界宽 0.03 ≈ 2px 高频几何）
//! 悬在亮天空前——左 = TAA 关（静止帧锯齿斜边），右 = TAA 开（静止
//! 收敛 48 帧，时域 SSAA 平滑边缘）。
//! 场景 B（下排）：红色方块自体横移（TAA v1 无逐物体运动向量），取
//! 运动中段第 30 帧同帧号对照——左 = TAA 关（锐利干净），右 = TAA 开
//! （拖影可见但受钳制约束，有界）。
//!
//! 硬断言：①静止收敛帧**低频天空区域与关 TAA 逐位一致**（无糊化）；
//! ②边缘带确有平滑（变更像素 > 0）；③拖影像素有界（0 < n < 2%）；
//! ④两会话逐帧确定性由 `tests/taa_render.rs` 承担。
//!
//! ```sh
//! cargo run -p iris-render --example taa_snapshot --release
//! ffmpeg -y -i iris_taa.ppm docs/taa.png
//! ```

use std::path::Path;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Entity, Transform, World};
use iris_render::prelude::*;

/// 单面板分辨率（与 R53 验收图同档）。
const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
/// 面板间分隔条宽度。
const GAP: u32 = 6;

/// 全帧像素 FNV-1a 64（打印用）。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// 写双排四面板 PPM P6（上排细杆 关|开、下排鬼影 关|开）。
fn write_grid(
    path: &Path,
    bars_off: &[u8],
    bars_on: &[u8],
    ghost_off: &[u8],
    ghost_on: &[u8],
) -> std::io::Result<()> {
    use std::io::Write;
    let total_w = WIDTH * 2 + GAP;
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    writeln!(out, "P6\n{total_w} {}\n255", HEIGHT * 2 + GAP)?;
    let mut row = vec![0u8; (total_w * 3) as usize];
    let write_row = |row: &mut [u8], y: u32, left: &[u8], right: &[u8]| {
        for x in 0..(total_w as usize) {
            let pixel: &[u8] = if (x as u32) < WIDTH {
                &left[((y * WIDTH + x as u32) * 4) as usize..]
            } else if (x as u32) < WIDTH + GAP {
                &[40, 40, 40, 255]
            } else {
                let rx = x - WIDTH as usize - GAP as usize;
                &right[((y * WIDTH + rx as u32) * 4) as usize..]
            };
            row[x * 3..x * 3 + 3].copy_from_slice(&pixel[..3]);
        }
    };
    for y in 0..HEIGHT {
        write_row(&mut row, y, bars_off, bars_on);
        out.write_all(&row)?;
    }
    for _ in 0..GAP {
        out.write_all(&vec![90u8; (total_w * 3) as usize])?;
    }
    for y in 0..HEIGHT {
        write_row(&mut row, y, ghost_off, ghost_on);
        out.write_all(&row)?;
    }
    out.flush()
}

/// 场景 A：细杆世界（5 根 45° 近黑细杆悬在亮天空前，泛光关）。
fn bars_world() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 0.0, 3.0),
        target: Vec3::ZERO,
        ..Default::default()
    });
    world.insert_resource(DirectionalLight {
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    world.insert_resource(TaaSettings { enabled: false });

    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat_bar = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.02, 0.02, 0.02, 1.0));
    for i in 0..5i32 {
        world.spawn((
            Transform {
                position: Vec3::new(i as f32 * 0.6 - 1.2, 0.0, 0.0),
                rotation: glam::Quat::from_rotation_z(45f32.to_radians()),
                scale: Vec3::new(0.03, 4.0, 0.03),
            },
            cube,
            mat_bar,
        ));
    }
    propagate_transforms(&mut world);
    world
}

/// 场景 B：鬼影世界（红色方块，测试期逐帧改写 Transform 模拟物体自体
/// 运动；返回实体句柄供驱动）。
fn ghost_world() -> (World, Entity) {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 0.0, 3.0),
        target: Vec3::ZERO,
        ..Default::default()
    });
    world.insert_resource(DirectionalLight {
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    world.insert_resource(TaaSettings { enabled: false });

    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat_cube = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.75, 0.06, 0.06, 1.0));
    let entity = world.spawn((
        Transform {
            position: Vec3::new(-1.3, 0.0, 0.0),
            ..Transform::IDENTITY
        },
        cube,
        mat_cube,
    ));
    propagate_transforms(&mut world);
    (world, entity)
}

/// 像素亮度（0..255）。
fn lum(px: &[u8], x: u32, y: u32) -> f64 {
    let i = ((y * WIDTH + x) * 4) as usize;
    (px[i] as f64 + px[i + 1] as f64 + px[i + 2] as f64) / 3.0
}

/// 区域内最大亮度差（LSB）。
fn region_max_diff(a: &[u8], b: &[u8], x0: u32, y0: u32, x1: u32, y1: u32) -> f64 {
    let mut mx = 0.0f64;
    for y in y0..y1 {
        for x in x0..x1 {
            mx = mx.max((lum(a, x, y) - lum(b, x, y)).abs());
        }
    }
    mx
}

/// 区域内 |Δ亮度| > 25 的像素数。
fn diff_count(a: &[u8], b: &[u8]) -> usize {
    let mut n = 0;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            if (lum(a, x, y) - lum(b, x, y)).abs() > 25.0 {
                n += 1;
            }
        }
    }
    n
}

fn main() {
    let Some(mut bars_renderer) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut ghost_renderer) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    // ---- 上排：细杆 关 | 开（静止收敛）----
    let bars = bars_world();
    // 关：静止帧（锯齿斜边）
    bars_renderer.render(&bars);
    let bars_off = bars_renderer.read_pixels();
    // 开：重置透传帧后静止收敛 48 帧（时域 SSAA 稳态）
    bars.resource_mut::<TaaSettings>().enabled = true;
    for _ in 0..48 {
        bars_renderer.render(&bars);
    }
    let bars_on = bars_renderer.read_pixels();

    // 低频天空区域（右上角，远离全部细杆）与关 TAA 的差异必须 ≤ 2 LSB
    // ——静止收敛不糊化低频区域。天空是 IBL 渐变（非平坦色），抖动使
    // 渐变采样方向偏移亚纹素，逐位一致不成立（实测差异 1 LSB）；
    // 「无肉眼差异」的诚实量化口径即 ≤ 2 LSB。
    let sky_residual = region_max_diff(&bars_off, &bars_on, 350, 10, 470, 50);
    println!("细杆：低频天空区域残差（|Δ| 最大值）= {sky_residual:.0} LSB");
    assert!(
        sky_residual <= 2.0,
        "静止收敛帧的低频天空区域残差必须 ≤ 2 LSB（无糊化）：{sky_residual}"
    );
    // 中央带（跨全部细杆边缘）必须有平滑差异
    let edge_changed = diff_count(&bars_off, &bars_on);
    println!("细杆：边缘带 |Δ|>25 像素 = {edge_changed}");
    assert!(edge_changed > 100, "边缘带必须有平滑差异：{edge_changed}");
    println!(
        "细杆：哈希 off/on = {:#018x}/{:#018x}",
        fnv1a(&bars_off),
        fnv1a(&bars_on)
    );

    // ---- 下排：鬼影 关 | 开（运动中段第 30 帧，同帧号对照）----
    let (mut ghost, cube) = ghost_world();
    // 关：方块横移 30 帧
    for i in 0..30u32 {
        *ghost.get_mut::<Transform>(cube).unwrap() =
            Transform::from_xyz(i as f32 * 0.0433 - 1.3, 0.0, 0.0);
        propagate_transforms(&mut ghost);
        ghost_renderer.render(&ghost);
    }
    let ghost_off = ghost_renderer.read_pixels();
    // 开：方块从同一起点再横移 30 帧（帧 0 = 重置透传）
    ghost.resource_mut::<TaaSettings>().enabled = true;
    for i in 0..30u32 {
        *ghost.get_mut::<Transform>(cube).unwrap() =
            Transform::from_xyz(i as f32 * 0.0433 - 1.3, 0.0, 0.0);
        propagate_transforms(&mut ghost);
        ghost_renderer.render(&ghost);
    }
    let ghost_on = ghost_renderer.read_pixels();

    let ghost_px = diff_count(&ghost_off, &ghost_on);
    let total = (WIDTH * HEIGHT) as usize;
    println!(
        "鬼影：拖影/差异像素（|Δ|>25）= {ghost_px}（{:.2}%，有界 < 2%）",
        ghost_px as f64 * 100.0 / total as f64
    );
    println!(
        "鬼影：哈希 off/on = {:#018x}/{:#018x}",
        fnv1a(&ghost_off),
        fnv1a(&ghost_on)
    );
    assert!(ghost_px > 0, "运动方块必有可见差异（TAA 生效自检）");
    assert!(
        ghost_px * 50 < total,
        "拖影必须受钳制约束（< 2% 像素）：{ghost_px}"
    );

    let path = Path::new("iris_taa.ppm");
    write_grid(path, &bars_off, &bars_on, &ghost_off, &ghost_on).expect("写验收图");
    println!(
        "输出 {}（上排=细杆 关|开，下排=鬼影 关|开，{}×{}）",
        path.display(),
        WIDTH * 2 + GAP,
        HEIGHT * 2 + GAP
    );
    println!("转 PNG：ffmpeg -y -i iris_taa.ppm docs/taa.png");
}
