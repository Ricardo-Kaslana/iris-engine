//! 验收工具：R53「遮挡剔除 + FXAA」两件套的目检图输出。
//!
//! **单一 world、两个开关资源按阶段改字段**——渲染器 GPU 缓存按
//! (网格/材质/纹理/图集) 句柄键控、假设目录跨帧稳定（引擎语义），
//! 多 world 换场会让后一个 world 的句柄命中前一个的缓存（本示例
//! 初版踩过：斜条被画成了上一场景的墙面色）。
//!
//! 场景：12×12 室内盒 + 3 根立柱（每柱沿「相机→柱心」射线后方 2.6 m
//! 一组**完全被挡**彩盒）+ 半露彩盒/矮桌（反误杀对照）+ 左前景一条
//! 45° 近黑斜条（FXAA 主体：与亮天空/亮墙形成强对比斜边缘）+ 左上角
//! 不透明 HUD 文字。
//!
//! 四面板（2×2）：
//! - 上排 = 遮挡 关 | 开（稳定 3 帧后）：两帧**逐位一致**（headless
//!   硬断言，保守性），控制台打印 `occlusion_culled` / `triangles_drawn`
//!   与两模式 `Renderer::draw` 平均 CPU 墙钟对比；
//! - 下排 = FXAA 关 | 开：斜条边缘 1–2 像素硬跳变 → 2–3 像素缓坡，
//!   HUD 文字两侧同样锐利（UI 在 FXAA 之后）。
//!
//! ```sh
//! cargo run -p iris-render --example occlusion_fxaa_snapshot --release
//! ffmpeg -y -i iris_occlusion_fxaa.ppm docs/occlusion_fxaa.png
//! ```
//!
//! 读回节拍：遮挡稳态每帧 `render + read_pixels`（阻塞 poll 派发
//! map 回调，等价 60fps 事件循环的帧间隔，见 occlusion.rs）。

use std::path::Path;
use std::time::Instant;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Quat, Transform, World};
use iris_render::prelude::*;

/// 单面板分辨率（与 R52c 验收图同档）。
const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
/// 面板间分隔条宽度。
const GAP: u32 = 6;
/// CPU 墙钟计时帧数。
const TIMED_FRAMES: u32 = 30;

/// 全帧像素 FNV-1a 64（打印用）。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// 写双排四面板 PPM P6（上排遮挡 off|on、下排 FXAA off|on）。
fn write_grid(
    path: &Path,
    occ_off: &[u8],
    occ_on: &[u8],
    fxaa_off: &[u8],
    fxaa_on: &[u8],
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
        write_row(&mut row, y, occ_off, occ_on);
        out.write_all(&row)?;
    }
    for _ in 0..GAP {
        out.write_all(&vec![90u8; (total_w * 3) as usize])?;
    }
    for y in 0..HEIGHT {
        write_row(&mut row, y, fxaa_off, fxaa_on);
        out.write_all(&row)?;
    }
    out.flush()
}

/// 验收场景（单 world）：室内壳 + 立柱×3 + 柱后隐藏盒×3 + 半露对照
/// ×2 + 45° 斜条 + HUD 文字；两个开关资源初始均为关。
fn acceptance_world() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(DirectionalLight {
        shadows: false,
        intensity: 1.8,
        ambient: Vec3::splat(0.12),
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    world.insert_resource(OcclusionCulling { enabled: false });
    world.insert_resource(FxaaSettings {
        enabled: false,
        ..Default::default()
    });

    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(14.0));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat_wall = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.72, 0.7, 0.64, 1.0));
    let mat_col = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.3, 0.3, 0.36, 1.0));
    let mat_red = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.85, 0.12, 0.1, 1.0));
    let mat_green = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.15, 0.7, 0.25, 1.0));
    let mat_blue = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.15, 0.25, 0.85, 1.0));
    let mat_dark = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.02, 0.02, 0.02, 1.0));

    // 室内壳：地板 + 后墙 + 左右墙
    world.spawn((Transform::IDENTITY, plane, mat_wall));
    world.spawn((
        Transform {
            rotation: Quat::from_rotation_x(-90f32.to_radians()),
            position: Vec3::new(0.0, 3.0, -7.0),
            scale: Vec3::ONE,
        },
        plane,
        mat_wall,
    ));
    for (x, ry) in [(-7.0f32, 90.0f32), (7.0, -90.0)] {
        world.spawn((
            Transform {
                rotation: Quat::from_rotation_y(ry.to_radians()),
                position: Vec3::new(x, 3.0, 0.0),
                scale: Vec3::ONE,
            },
            plane,
            mat_wall,
        ));
    }

    // 立柱 + 柱后隐藏盒（沿「相机→柱心」射线，柱后 2.6 m，角半径
    // 余量 ≥1.4× ⇒ 屏幕投影严格落在柱轮廓内）
    let eye = Vec3::new(0.0, 1.7, 6.0);
    let hidden_mats = [mat_red, mat_green, mat_blue];
    for (ci, col) in [(0.0f32, 0.0f32), (-3.0, -1.5), (3.0, -1.5)]
        .iter()
        .enumerate()
    {
        let center = Vec3::new(col.0, 1.3, col.1);
        world.spawn((
            Transform {
                position: center,
                scale: Vec3::new(1.1, 2.6, 1.1),
                ..Transform::IDENTITY
            },
            cube,
            mat_col,
        ));
        let dir = (center - eye).normalize();
        world.spawn((
            Transform {
                position: eye + dir * (eye.distance(center) + 2.6),
                scale: Vec3::splat(0.7),
                ..Transform::IDENTITY
            },
            cube,
            hidden_mats[ci],
        ));
    }
    // 反误杀对照：半露彩盒与矮桌（部分可见 ⇒ 查询计 >0，不得剔）
    world.spawn((
        Transform {
            position: Vec3::new(1.6, 0.45, -0.6),
            scale: Vec3::splat(0.9),
            ..Transform::IDENTITY
        },
        cube,
        mat_green,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(-1.8, 0.35, 2.5),
            scale: Vec3::new(1.4, 0.7, 0.8),
            ..Transform::IDENTITY
        },
        cube,
        mat_blue,
    ));
    // FXAA 主体：左前景 45° 近黑斜条（与亮天空/亮墙强对比，恒可见）
    world.spawn((
        Transform {
            position: Vec3::new(-2.6, 1.6, 3.0),
            rotation: Quat::from_rotation_z(45f32.to_radians()),
            scale: Vec3::new(0.1, 2.6, 0.1),
        },
        cube,
        mat_dark,
    ));

    // HUD：不透明文字（FXAA 之后绘制，必须保持锐利）
    world.spawn((
        UiRect {
            position: [10.0, 10.0],
            size: [200.0, 32.0],
        },
        UiText {
            string: "HUD SHARP".to_string(),
            scale: 2.0,
        },
        UiColor([1.0, 1.0, 1.0, 1.0]),
    ));

    world.insert_resource(Camera {
        eye,
        target: Vec3::new(0.0, 1.2, -3.0),
        ..Default::default()
    });
    propagate_transforms(&mut world);
    world
}

/// 稳态帧：render + 阻塞 read_pixels（派发遮挡读回回调，等价真实帧节拍）。
fn settle(renderer: &mut OffscreenRenderer, world: &World, frames: u32) {
    for _ in 0..frames {
        renderer.render(world);
        let _ = renderer.read_pixels();
    }
}

/// 平均 `draw_frame` 的 CPU 墙钟耗时（毫秒）。GPU 异步：本数字反映
/// 每帧 CPU 侧收集/编码成本——遮挡剔除的收益口径。
fn cpu_draw_ms(renderer: &mut OffscreenRenderer, world: &World) -> f64 {
    for _ in 0..3 {
        renderer.draw_frame(world);
    }
    let started = Instant::now();
    for _ in 0..TIMED_FRAMES {
        renderer.draw_frame(world);
    }
    started.elapsed().as_secs_f64() * 1000.0 / TIMED_FRAMES as f64
}

/// 变更像素数。
fn changed_pixels(a: &[u8], b: &[u8]) -> usize {
    (0..(WIDTH * HEIGHT) as usize)
        .filter(|&i| a[i * 4..i * 4 + 4] != b[i * 4..i * 4 + 4])
        .count()
}

fn main() {
    let Some(mut renderer) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let world = acceptance_world();

    // ---- 阶段 A：全关基线（= 遮挡 off 帧 = FXAA off 帧）----
    settle(&mut renderer, &world, 2);
    let off_cpu = cpu_draw_ms(&mut renderer, &world);
    settle(&mut renderer, &world, 1);
    let stats_off = renderer.renderer().stats();
    let px_off = renderer.read_pixels();

    // ---- 阶段 B：遮挡开（帧 1 建查询、帧 2 起有结果可剔）----
    world.resource_mut::<OcclusionCulling>().enabled = true;
    settle(&mut renderer, &world, 3);
    let on_cpu = cpu_draw_ms(&mut renderer, &world);
    // 计时循环零间隔连跑会让读回迟到被丢弃、剔除暂停——再稳 3 帧重建
    // 新鲜链后取稳态。
    settle(&mut renderer, &world, 3);
    let stats_on = renderer.renderer().stats();
    let px_on = renderer.read_pixels();

    let diff = changed_pixels(&px_off, &px_on);
    println!(
        "遮挡：关闭 instances={} culled={} draws={} tris={}",
        stats_off.instances,
        stats_off.culled_instances,
        stats_off.draw_calls,
        stats_off.triangles_drawn
    );
    println!(
        "遮挡：开启 occlusion_culled={} culled={} instances={} tris={}",
        stats_on.occlusion_culled,
        stats_on.culled_instances,
        stats_on.instances,
        stats_on.triangles_drawn
    );
    println!(
        "遮挡：两帧差异字节 = {}（期望 0 = 保守性）｜哈希 off/on = {:#018x}/{:#018x}",
        diff,
        fnv1a(&px_off),
        fnv1a(&px_on)
    );
    println!("遮挡：draw CPU 墙钟 {off_cpu:.3} ms → {on_cpu:.3} ms 每帧");
    assert!(
        stats_on.occlusion_culled >= 3,
        "3 个全遮挡彩盒应被剔除，实际 {}",
        stats_on.occlusion_culled
    );
    assert_eq!(diff, 0, "保守性硬校验：开启遮挡后画面必须逐位一致");
    assert!(
        stats_on.triangles_drawn < stats_off.triangles_drawn,
        "开启后主 pass 三角形必须减少"
    );
    // 关闭遮挡，进入 FXAA 阶段（px_off 复用为下排左面板）
    world.resource_mut::<OcclusionCulling>().enabled = false;

    // ---- 阶段 C：FXAA 开（遮挡关 ⇒ 与阶段 A 同管线只差 FXAA）----
    world.resource_mut::<FxaaSettings>().enabled = true;
    renderer.render(&world);
    let px_fxaa = renderer.read_pixels();
    let changed = changed_pixels(&px_off, &px_fxaa);
    println!(
        "FXAA：变更像素 {changed}（{:.1}%）｜哈希 off/on = {:#018x}/{:#018x}",
        changed as f64 * 100.0 / (WIDTH * HEIGHT) as f64,
        fnv1a(&px_off),
        fnv1a(&px_fxaa)
    );
    assert!(changed > 1000, "FXAA 必须实际处理边缘带，变更 {changed}");
    assert!(
        changed * 100 < (WIDTH * HEIGHT) as usize * 30,
        "非边缘必须早退透传：变更 {changed} 占比过大"
    );

    let path = Path::new("iris_occlusion_fxaa.ppm");
    write_grid(path, &px_off, &px_on, &px_off, &px_fxaa).expect("写验收图");
    println!(
        "输出 {}（上排=遮挡 关|开，下排=FXAA 关|开，{}×{}）",
        path.display(),
        WIDTH * 2 + GAP,
        HEIGHT * 2 + GAP
    );
    println!("转 PNG：ffmpeg -y -i iris_occlusion_fxaa.ppm docs/occlusion_fxaa.png");
}
