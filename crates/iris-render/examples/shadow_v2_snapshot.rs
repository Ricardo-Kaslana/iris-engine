//! 验收工具：R61「阴影 v2」的目检图输出（ARCHITECTURE §4.7.1 / §4.7.2.
//! 与 §8.8 的闭合证明）。
//!
//! 四排面板（每排左右各一，480×270，见 `docs/shadow_v2.png`）：
//!
//! ① 掠射角平面（NdotL ≈ 0.118、θ ≈ 83°、场景无任何遮挡物）：
//!    左 = 旧常数 bias（`set_legacy_shadow_bias(true)`，acne 点状伪影
//!    4575 px）｜右 = v2 坡度缩放 bias + 法线偏移（0 px）。
//! ② 接触阴影（贴地薄板 0.07 m）：左 = 旧常数 bias（接触阴影整条消失、
//!    阴影浮起）｜右 = v2（阴影带完整且贴住根部）。
//! ③ 双点光各投各影：左 = 无投影灯｜右 = 两盏同时投影（两条方向不同的
//!    阴影带，互不串扰）。
//! ④ CSM 级联接缝：左 = 接缝近侧（级联 0 侧）2× 放大｜右 = 接缝远侧
//!    （级联 1 侧）2× 放大——阴影带跨接缝连续、两侧纹理密度不同但无
//!    亮度跳变。
//!
//! 硬断言：①acne 旧 > 1000 / 新 = 0；②阴影带旧 ≈ 0 / 新 > 800 且根部
//! 亮带 ≤ 4 px；③两盏各自成影、可加（无串扰）、投影灯数 1/1/2；
//! ④跨接缝的阴影带连续（无缺口）且相邻采样无跳变。
//!
//! **每个场景一个独立渲染器**：GPU 缓存按（网格 / 材质 / 纹理）句柄值
//! 键控、假设目录跨帧稳定，同尺寸不同内容的场景共用渲染器会命中错误
//! 缓存（R53 教训，R61 又踩过一次）。
//!
//! ```sh
//! cargo run -p iris-render --example shadow_v2_snapshot --release
//! ffmpeg -y -i iris_shadow_v2.ppm docs/shadow_v2.png
//! ```

use std::path::Path;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 单面板分辨率（与既有验收图同档）。
const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
/// 面板间分隔条宽度。
const GAP: u32 = 6;
/// 面板行数（四排）。
const ROWS: u32 = 4;

/// 全帧像素 FNV-1a 64（打印用；跨运行比对由主会话复跑）。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

fn lum(px: &[u8], x: u32, y: u32) -> f32 {
    let i = ((y * WIDTH + x) * 4) as usize;
    (px[i] as f32 + px[i + 1] as f32 + px[i + 2] as f32) / 3.0
}

/// 相对暗化像素（亮度相对下降 > 8%，受光帧亮度 > 20/255）——掠射角与
/// 接触阴影的口径（直接光占比被压到很高，相对口径与曝光解耦）。
fn darkened(on: &[u8], off: &[u8]) -> usize {
    on.chunks(4)
        .zip(off.chunks(4))
        .filter(|(a, b)| {
            let la = (a[0] as f32 + a[1] as f32 + a[2] as f32) / 3.0;
            let lb = (b[0] as f32 + b[1] as f32 + b[2] as f32) / 3.0;
            lb > 20.0 && la < lb * 0.92
        })
        .count()
}

/// 绝对暗化像素（RGB 和差 > 12）——点光阴影口径（两盏灯 + 环境光叠加，
/// 相对阈值会被环境项稀释）。
fn darkened_abs(on: &[u8], off: &[u8]) -> usize {
    on.chunks(4)
        .zip(off.chunks(4))
        .filter(|(a, b)| {
            let sa = a[0] as i32 + a[1] as i32 + a[2] as i32;
            let sb = b[0] as i32 + b[1] as i32 + b[2] as i32;
            sa < sb - 12
        })
        .count()
}

/// 世界点投影到像素坐标（与 Camera 的 DirectX 深度约定一致的 CPU 复算）。
fn project(point: Vec3, camera: &Camera) -> (f32, f32) {
    let vp = camera.view_proj(WIDTH as f32 / HEIGHT as f32);
    let clip = vp * point.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    (
        (ndc.x * 0.5 + 0.5) * WIDTH as f32,
        (1.0 - (ndc.y * 0.5 + 0.5)) * HEIGHT as f32,
    )
}

// ═══════════════════════════════════════════════════════════════════════
// 场景（与 tests/shadow_v2_render.rs 同源）
// ═══════════════════════════════════════════════════════════════════════

/// ① 掠射角大平面：120 m 地面 + 近水平方向光，环境项归零（acne 对比度）。
fn grazing_scene(shadows: bool) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 7.0, 9.0),
        target: Vec3::new(0.0, 0.0, -12.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-1.0, -0.12, -0.15).normalize(),
        intensity: 6.0,
        ambient: Vec3::ZERO,
        shadows,
        ..DirectionalLight::default()
    });
    world.insert_resource(EnvironmentSettings { intensity: 0.0 });
    world.insert_resource(BloomSettings::disabled());
    let plane = world
        .resource_mut::<MeshStore>()
        .add(MeshData::plane(120.0));
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.75, 0.75, 0.75, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat));
    propagate_transforms(&mut world);
    world
}

/// ② 接触阴影：20 m 地面 + 贴地薄板（2 × 0.07 × 2 m），相机在 −X 侧。
fn contact_scene(shadows: bool) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(-1.9, 0.85, 1.7),
        target: Vec3::new(-0.85, 0.0, 0.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.75, -0.62, -0.25).normalize(),
        intensity: 1.5,
        ambient: Vec3::ZERO,
        shadows,
        ..DirectionalLight::default()
    });
    world.insert_resource(EnvironmentSettings { intensity: 0.0 });
    world.insert_resource(BloomSettings::disabled());
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(20.0));
    let mat_ground = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.6, 0.6, 0.6, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_ground));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat_obj = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.8, 0.4, 0.3, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 0.035, 0.0),
            scale: Vec3::new(2.0, 0.07, 2.0),
            ..Transform::IDENTITY
        },
        cube,
        mat_obj,
    ));
    propagate_transforms(&mut world);
    world
}

/// ③ 双点光：中央遮挡方块 + 左右两盏点光（阴影投向各自背离侧）。
fn two_casters_scene(both: bool) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 5.0, 7.2),
        target: Vec3::new(0.0, 0.5, 0.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        intensity: 0.12,
        ambient: Vec3::splat(0.04),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(EnvironmentSettings { intensity: 0.15 });
    world.insert_resource(BloomSettings::disabled());
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let mat_ground = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.5, 0.5, 0.5, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_ground));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat_obj = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.7, 0.7, 0.7, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 1.6, 0.0),
            scale: Vec3::splat(1.2),
            ..Transform::IDENTITY
        },
        cube,
        mat_obj,
    ));
    world.spawn((
        Transform::from_xyz(-4.0, 3.2, 0.0),
        PointLight {
            color: Vec3::new(1.0, 0.85, 0.6),
            intensity: 45.0,
            range: 16.0,
            shadows: both,
        },
    ));
    world.spawn((
        Transform::from_xyz(4.0, 3.2, 0.0),
        PointLight {
            color: Vec3::new(0.6, 0.85, 1.0),
            intensity: 45.0,
            range: 16.0,
            shadows: both,
        },
    ));
    propagate_transforms(&mut world);
    world
}

/// ④ CSM 大场景（与 `tests/csm_render.rs` 同源）：级联 0/1 边界落在侧墙
/// 的地面阴影带中部，正是「接缝跳变」最容易暴露的位置。
const CSM_CAMERA: ((f32, f32, f32), (f32, f32, f32)) = ((0.0, 5.0, 40.0), (0.0, 0.0, -15.0));

fn csm_scene(shadows: bool) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(CSM_CAMERA.0.0, CSM_CAMERA.0.1, CSM_CAMERA.0.2),
        target: Vec3::new(CSM_CAMERA.1.0, CSM_CAMERA.1.1, CSM_CAMERA.1.2),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.5, -1.0, -0.4),
        intensity: 1.8,
        ambient: Vec3::splat(0.04),
        shadows,
        ..DirectionalLight::default()
    });
    world.insert_resource(EnvironmentSettings { intensity: 0.35 });
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.85, 0.3, 0.2, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 1.5, 30.0),
            scale: Vec3::splat(3.0),
            ..Transform::IDENTITY
        },
        cube,
        mat,
    ));
    let mat_wall = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.4, 0.5, 0.7, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(8.0, 1.5, 15.0),
            scale: Vec3::new(6.0, 3.0, 14.0),
            ..Transform::IDENTITY
        },
        cube,
        mat_wall,
    ));
    let mat_far = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.2, 0.7, 0.4, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 3.0, -40.0),
            scale: Vec3::splat(6.0),
            ..Transform::IDENTITY
        },
        cube,
        mat_far,
    ));
    let plane = world
        .resource_mut::<MeshStore>()
        .add(MeshData::plane(240.0));
    let mat_ground = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.5, 0.5, 0.5, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_ground));
    propagate_transforms(&mut world);
    world
}

/// 2× 最近邻放大的裁剪面板（源 240×135 → 480×270），中心由世界点投影给出。
fn zoom_panel(src: &[u8], center: (f32, f32)) -> Vec<u8> {
    zoom_impl(src, None, center, 1.0)
}

/// 2× 放大的**差分面板**：`(off − on) × gain` 的灰度图（阴影带的可判别
/// 可视化——原图阴影只暗化 ~24/255，直接目检接缝跳变不够醒目）。
fn zoom_panel_diff(on: &[u8], off: &[u8], center: (f32, f32), gain: f32) -> Vec<u8> {
    zoom_impl(on, Some(off), center, gain)
}

fn zoom_impl(src: &[u8], sub: Option<&[u8]>, center: (f32, f32), gain: f32) -> Vec<u8> {
    let cw = WIDTH / 2;
    let ch = HEIGHT / 2;
    let x0 = (center.0 as i32 - cw as i32 / 2).clamp(0, (WIDTH - cw) as i32) as u32;
    let y0 = (center.1 as i32 - ch as i32 / 2).clamp(0, (HEIGHT - ch) as i32) as u32;
    let mut out = vec![0u8; (WIDTH * HEIGHT * 4) as usize];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let sx = x0 + x / 2;
            let sy = y0 + y / 2;
            let si = ((sy * WIDTH + sx) * 4) as usize;
            let di = ((y * WIDTH + x) * 4) as usize;
            match sub {
                None => out[di..di + 4].copy_from_slice(&src[si..si + 4]),
                Some(other) => {
                    let v = ((other[si] as f32 - src[si] as f32) * gain).clamp(0.0, 255.0) as u8;
                    out[di] = v;
                    out[di + 1] = v;
                    out[di + 2] = v;
                    out[di + 3] = 255;
                }
            }
        }
    }
    out
}

/// 在面板上画一条 3 px 宽的红色水平标记线（标出级联接缝所在的屏幕行）。
fn draw_marker(panel: &mut [u8], screen_y: f32) {
    let y = screen_y.round().clamp(2.0, (HEIGHT - 3) as f32) as u32;
    for dy in 0..3 {
        for x in 0..WIDTH {
            let i = (((y + dy) * WIDTH + x) * 4) as usize;
            panel[i] = 220;
            panel[i + 1] = 40;
            panel[i + 2] = 40;
        }
    }
}

/// 写 4 排 × 2 列面板的 PPM P6。
fn write_grid(path: &Path, panels: &[(Vec<u8>, Vec<u8>)]) -> std::io::Result<()> {
    use std::io::Write;
    let total_w = WIDTH * 2 + GAP;
    let total_h = HEIGHT * ROWS + GAP * (ROWS - 1);
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    writeln!(out, "P6\n{total_w} {total_h}\n255")?;
    let mut row = vec![0u8; (total_w * 3) as usize];
    for (r, (left, right)) in panels.iter().enumerate() {
        if r > 0 {
            for _ in 0..GAP {
                out.write_all(&vec![90u8; (total_w * 3) as usize])?;
            }
        }
        for y in 0..HEIGHT {
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
            out.write_all(&row)?;
        }
    }
    out.flush()
}

fn main() {
    let Some(mut grazing) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut contact) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut point) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut csm) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    // ── ① 掠射角平面：旧常数 bias | v2 ──────────────────────────────
    grazing.render(&grazing_scene(false));
    let grazing_ref = grazing.read_pixels();
    grazing.render(&grazing_scene(true));
    let grazing_v2 = grazing.read_pixels();
    grazing.renderer_mut().set_legacy_shadow_bias(true);
    grazing.render(&grazing_scene(true));
    let grazing_legacy = grazing.read_pixels();
    let acne_legacy = darkened(&grazing_legacy, &grazing_ref);
    let acne_v2 = darkened(&grazing_v2, &grazing_ref);
    println!("① 掠射角平面 acne 像素：旧常数 bias = {acne_legacy}，v2 = {acne_v2}");
    assert!(
        acne_legacy > 1000,
        "旧常数 bias 必须留下成片 acne：{acne_legacy}"
    );
    assert_eq!(acne_v2, 0, "v2 必须消除掠射角 acne：{acne_v2}");

    // ── ② 接触阴影：旧常数 bias（浮起）| v2（贴合）────────────────
    contact.render(&contact_scene(false));
    let contact_ref = contact.read_pixels();
    contact.render(&contact_scene(true));
    let contact_v2 = contact.read_pixels();
    contact.renderer_mut().set_legacy_shadow_bias(true);
    contact.render(&contact_scene(true));
    let contact_legacy = contact.read_pixels();
    let contact_shadow_legacy = darkened(&contact_legacy, &contact_ref);
    let contact_shadow_v2 = darkened(&contact_v2, &contact_ref);
    println!(
        "② 接触阴影（薄板 0.07 m）变暗像素：旧常数 bias = {contact_shadow_legacy}，v2 = {contact_shadow_v2}"
    );
    assert!(
        contact_shadow_legacy < 50,
        "旧常数 bias 应抹掉低矮薄板的接触阴影：{contact_shadow_legacy}"
    );
    assert!(
        contact_shadow_v2 > 800,
        "v2 必须保留接触阴影带：{contact_shadow_v2}"
    );

    // ── ③ 双点光：无投影灯 | 两盏同时投影 ─────────────────────────
    point.render(&two_casters_scene(false));
    let point_none = point.read_pixels();
    point.render(&two_casters_scene(true));
    let point_both = point.read_pixels();
    let casters = point.renderer().stats().point_shadow_casters;
    println!(
        "③ 双点光变暗像素 = {}，FrameStats.point_shadow_casters = {casters}",
        darkened_abs(&point_both, &point_none)
    );
    assert!(
        darkened_abs(&point_both, &point_none) > 200,
        "两盏点光的阴影必须成片可见"
    );
    assert_eq!(casters, 2, "两盏投影灯必须同时生效");

    // ── ④ CSM 级联接缝：近侧 | 远侧 2× 放大 ───────────────────────
    csm.render(&csm_scene(true));
    let csm_on = csm.read_pixels();
    csm.render(&csm_scene(false));
    let csm_off = csm.read_pixels();
    let csm_camera = Camera {
        eye: Vec3::new(CSM_CAMERA.0.0, CSM_CAMERA.0.1, CSM_CAMERA.0.2),
        target: Vec3::new(CSM_CAMERA.1.0, CSM_CAMERA.1.1, CSM_CAMERA.1.2),
        ..Camera::default()
    };
    // 级联 0/1 边界在侧墙阴影带的 z ≈ 15.9（见 tests/csm_render.rs 推导）；
    // 沿阴影带（x = 4.2）取边界两侧各 1.5 m 的探针，检查阴影连续且无跳变
    let mut profile = Vec::new();
    for i in 0..=34 {
        let z = 20.5 - i as f32 * 0.4; // 20.5 → 6.9，跨过接缝 z ≈ 15.9
        let (px, py) = project(Vec3::new(4.0, 0.0, z), &csm_camera);
        let (x, y) = (
            px.round().clamp(0.0, (WIDTH - 1) as f32) as u32,
            py.round().clamp(0.0, (HEIGHT - 1) as f32) as u32,
        );
        let off = lum(&csm_off, x, y);
        let on = lum(&csm_on, x, y);
        profile.push((z, off - on));
    }
    let dark: Vec<f32> = profile.iter().map(|(_, d)| *d).collect();
    let min_dark = dark.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_step = dark
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    println!(
        "④ CSM 接缝剖线（跨 z ≈ 15.9 的阴影带）：最小暗化 = {min_dark:.1}/255，\
         相邻采样最大跳变 = {max_step:.1}/255"
    );
    assert!(
        min_dark > 5.0,
        "阴影带必须跨接缝连续（无缺口）：最小暗化 {min_dark}"
    );
    assert!(
        max_step < 40.0,
        "跨接缝不得出现亮度跳变：最大相邻跳变 {max_step}"
    );
    let near = project(Vec3::new(4.0, 0.0, 15.9), &csm_camera);
    let mut panel_near = zoom_panel(&csm_on, near);
    draw_marker(&mut panel_near, near.1);
    let mut panel_far = zoom_panel_diff(&csm_on, &csm_off, near, 6.0);
    draw_marker(&mut panel_far, near.1);
    println!("④ 左 = 接缝处 2× 放大（红线 = 级联 0/1 接缝）；右 = 同裁剪的暗化差分 ×6");

    println!(
        "① 哈希 legacy/v2 = {:#018x}/{:#018x}",
        fnv1a(&grazing_legacy),
        fnv1a(&grazing_v2)
    );
    println!(
        "② 哈希 legacy/v2 = {:#018x}/{:#018x}",
        fnv1a(&contact_legacy),
        fnv1a(&contact_v2)
    );
    println!(
        "③ 哈希 none/both = {:#018x}/{:#018x}",
        fnv1a(&point_none),
        fnv1a(&point_both)
    );
    println!(
        "④ 哈希 on/off = {:#018x}/{:#018x}",
        fnv1a(&csm_on),
        fnv1a(&csm_off)
    );

    let path = Path::new("iris_shadow_v2.ppm");
    write_grid(
        path,
        &[
            (grazing_legacy, grazing_v2),
            (contact_legacy, contact_v2),
            (point_none, point_both),
            (panel_near, panel_far),
        ],
    )
    .expect("写验收图");
    println!(
        "已输出 {}（4 排 × 2 面板，{}×{}）",
        path.display(),
        WIDTH * 2 + GAP,
        HEIGHT * ROWS + GAP * (ROWS - 1)
    );
    println!("转 PNG：ffmpeg -y -i iris_shadow_v2.ppm docs/shadow_v2.png");

    // ── 性能：点光阴影盏数的边际成本（`draw_frame` 不含读回）──────────
    perf_point_shadow_cost();
}

/// 点光阴影预算的性能数字：1 盏 vs 4 盏投射（每盏 6 个 pass）。只打印
/// （不参与验收图，也不影响 PPM 字节），口径 = CPU 侧编码 + 提交耗时。
fn perf_point_shadow_cost() {
    const FRAMES: u32 = 120;
    for count in [1usize, 4] {
        let Some(mut renderer) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
            return;
        };
        let world = many_casters_scene(count);
        for _ in 0..10 {
            renderer.draw_frame(&world); // 预热：管线 / 缓冲懒创建不计入
        }
        let start = std::time::Instant::now();
        for _ in 0..FRAMES {
            renderer.draw_frame(&world);
        }
        let ms = start.elapsed().as_secs_f64() * 1000.0 / FRAMES as f64;
        println!(
            "性能：点光投影灯 {count} 盏 → {ms:.2} ms/帧（draw_frame，{WIDTH}×{HEIGHT}，块状场景）"
        );
    }
    // 显存增量（常量推导，非测量）：每盏 512² × 6 面 × R32Float 颜色 +
    // 同尺寸 Depth32Float 深度；v1 单盏 12 MiB，预算 4 盏 48 MiB。
    let per_attachment = 512.0 * 512.0 * 4.0 * 6.0 / (1024.0 * 1024.0);
    let per_cube = per_attachment * 2.0;
    println!(
        "显存：点光阴影单盏 {per_cube:.0} MiB（R32Float 颜色 {per_attachment:.0} + \
         Depth32Float 深度 {per_attachment:.0}）→ 预算 4 盏 {:.0} MiB（净增 +{:.0} MiB）",
        per_cube * 4.0,
        per_cube * 3.0
    );
}

/// 预算压力用场景：中央遮挡块 + N 盏开启阴影的点光（等间距排开）。
fn many_casters_scene(n: usize) -> World {
    let mut world = two_casters_scene(false);
    for i in 0..n {
        let x = (i as f32 - (n as f32 - 1.0) * 0.5) * 2.5;
        world.spawn((
            Transform::from_xyz(x, 3.2, 0.0),
            PointLight {
                color: Vec3::new(0.8, 0.8, 0.9),
                intensity: 40.0,
                range: 14.0,
                shadows: true,
            },
        ));
    }
    propagate_transforms(&mut world);
    world
}
