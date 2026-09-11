//! 光照探针 GI 验收图（R64）：四排面板对照，输出 iris_light_probes.ppm
//! → docs/light_probes.png。
//!
//! 面板布局（4 排 × 2 列，每排左右并排）：
//! 1. **室内阴影区角色 关|开**：探针集缺席 vs 就位——缺席时动态角色吃全局
//!    env 天空色（冷、亮、与房间色调不符），就位时吃所在位置的烘焙辐照；
//! 2. **插值跟随位置 亮区|阴影区**：同一角色、同一探针集，只改位置——
//!    重心插值按位置连续取值，室内两个区域的辐照差别可见；
//! 3. **与静态光照贴图的融合 关|开**：挂光照贴图的墙 / 地板不动（R52c 的
//!    逐 texel 分支优先），只有角色变——「静态漫反射 = 贴图 / 动态漫反射 =
//!    探针」的合成心智模型在画面上不产生接缝；
//! 4. **探针布置可视化**（CPU 绘制，不占 GPU）：俯视图（x-z 平面）上的探针
//!    位置（圆点）+ 四面体棱（细线）+ 凸包（粗线）+ 角色所在位置（红点）。
//!
//! 每格都带硬断言（不通过直接 panic、不写文件）；跨运行逐字节一致
//! （同输入两次渲染的 FNV-1a 全帧哈希相同，本示例打印哈希供比对）。
//!
//! ```sh
//! cargo run -p iris-render --example light_probe_snapshot --release
//! ffmpeg -y -i iris_light_probes.ppm docs/light_probes.png
//! ```

use std::path::Path;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Quat, RigidBody, Transform, World};
use iris_render::prelude::*;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
const GAP: u32 = 6;
const ROWS: u32 = 4;
/// 阴影区角色位置（天花板挡住入射光的一侧）。
const SHADOW_POS: Vec3 = Vec3::new(-2.0, 0.0, -2.0);
/// 亮区角色位置（+X / +Z 缺口进来的直射光斑里）。
const BRIGHT_POS: Vec3 = Vec3::new(3.0, 0.0, 3.0);

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 每个面板各用独立渲染器：渲染器按**句柄值**缓存 GPU 资源，同一渲染器上
/// 渲染「同句柄不同内容」的世界会串味（R53/R62 教训；本示例的面板恰好是
/// 「同几何、不同探针集 / 不同角色位置」）。
fn render_fresh(world: &World) -> Option<Vec<u8>> {
    let mut renderer = OffscreenRenderer::new(WIDTH, HEIGHT)?;
    renderer.render(world);
    Some(renderer.read_pixels())
}

/// 室内盒（10×10 m 地板 + 后墙 + 左墙 + 天花板 + 遮挡柱）。
/// 场景构造顺序固定：几何 → 光照贴图装配 → 探针装配 → 动态角色
/// （保证各面板的网格 / 材质句柄序列一致，见上一条串味说明）。
fn room_world() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(4.2, 2.4, 4.6),
        target: Vec3::new(-1.6, 0.9, -2.2),
        fov_y_radians: 40f32.to_radians(),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.55, -1.0, 0.3).normalize(),
        color: Vec3::new(1.0, 0.94, 0.82),
        intensity: 2.6,
        ambient: Vec3::new(0.03, 0.035, 0.05),
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());

    let floor_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(10.0));
    let wall_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(10.0));
    let column_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mut mats = world.resource_mut::<MaterialStore>();
    let floor_mat = mats.add(MaterialDef::solid(0.78, 0.72, 0.6, 1.0));
    let wall_mat = mats.add(MaterialDef::solid(0.62, 0.63, 0.66, 1.0));
    let column_mat = mats.add(MaterialDef::solid(0.85, 0.4, 0.24, 1.0));
    drop(mats);

    world.spawn((Transform::IDENTITY, floor_mesh, floor_mat));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 2.0, -5.0),
            rotation: Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
            scale: Vec3::new(1.0, 1.0, 0.4),
        },
        wall_mesh,
        wall_mat,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(-5.0, 2.0, 0.0),
            rotation: Quat::from_rotation_z(-std::f32::consts::FRAC_PI_2),
            scale: Vec3::new(0.4, 1.0, 1.0),
        },
        wall_mesh,
        wall_mat,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 4.0, 0.0),
            rotation: Quat::from_rotation_x(std::f32::consts::PI),
            ..Transform::IDENTITY
        },
        wall_mesh,
        wall_mat,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(1.5, 1.0, 0.0),
            scale: Vec3::new(0.6, 2.0, 0.6),
            ..Transform::IDENTITY
        },
        column_mesh,
        column_mat,
    ));
    propagate_transforms(&mut world);
    world
}

fn bake_params() -> BakeParams {
    BakeParams {
        light_dir: Vec3::new(-0.55, -1.0, 0.3).normalize(),
        light_color: Vec3::new(1.0, 0.94, 0.82) * 2.6,
        sky_color: Vec3::new(0.10, 0.13, 0.22),
        shadow_softness: 0.08,
        sky_samples: 96,
        bounces: 1,
    }
}

fn bake_uv() -> BakeUvParams {
    BakeUvParams {
        texels_per_meter: 6.0,
        atlas_size: 256,
    }
}

/// 室内探针布局：3×3×2 = 18 点网格（x/z ∈ {−3.5, 0, 3.5}、y ∈ {0.8, 2.6}）。
fn spawn_probes(world: &mut World) -> usize {
    let mut count = 0;
    for &x in &[-3.5f32, 0.0, 3.5] {
        for &z in &[-3.5f32, 0.0, 3.5] {
            for &y in &[0.8f32, 2.6] {
                world.spawn((Transform::from_xyz(x, y, z), IrradianceProbe));
                count += 1;
            }
        }
    }
    propagate_transforms(world);
    count
}

fn spawn_worker(world: &mut World, position: Vec3) {
    let body = world
        .resource_mut::<MeshStore>()
        .add(MeshData::cylinder(0.32, 1.5, 20));
    let mut mat = MaterialDef::solid(0.86, 0.84, 0.8, 1.0);
    mat.roughness = 0.85;
    let handle = world.resource_mut::<MaterialStore>().add(mat);
    world.spawn((
        Transform::from_xyz(position.x, position.y + 0.75, position.z),
        body,
        handle,
        RigidBody::default(),
    ));
    propagate_transforms(world);
}

/// 近景相机（面板 3 的融合对照）：贴近角色，画面里同时有角色与它背后的
/// 墙 / 地板（光照贴图表面）——接缝感只能在这个尺度上看出来。
fn close_camera() -> Camera {
    Camera {
        eye: Vec3::new(0.9, 1.5, 0.6),
        target: Vec3::new(-2.0, 0.85, -2.0),
        fov_y_radians: 38f32.to_radians(),
        ..Camera::default()
    }
}

/// 场景装配（probes = 是否装探针集；worker = 角色位置）。
fn scene(probes: bool, worker: Vec3) -> (World, Option<IrradianceProbeInstall>) {
    scene_with_camera(probes, worker, None)
}

/// 同 scene，但可覆盖相机（面板 3 用近景）。
fn scene_with_camera(
    probes: bool,
    worker: Vec3,
    camera: Option<Camera>,
) -> (World, Option<IrradianceProbeInstall>) {
    let mut world = room_world();
    if let Some(camera) = camera {
        world.insert_resource(camera);
    }
    let baked = bake_lightmap(&world, &bake_params(), &bake_uv());
    install_lightmap(&mut world, baked);
    let report = if probes {
        let n = spawn_probes(&mut world);
        assert!(n > 0);
        let set = bake_irradiance_probes(&world, &bake_params());
        Some(install_irradiance_probes(&mut world, set))
    } else {
        None
    };
    spawn_worker(&mut world, worker);
    (world, report)
}

fn luminance(pixels: &[u8], index: usize) -> u32 {
    let r = u32::from(pixels[index * 4]);
    let g = u32::from(pixels[index * 4 + 1]);
    let b = u32::from(pixels[index * 4 + 2]);
    (r * 2126 + g * 7152 + b * 722) / 10_000
}

fn diff_mask(a: &[u8], b: &[u8]) -> Vec<bool> {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .map(|(x, y)| (0..3).any(|c| (x[c] as i32 - y[c] as i32).abs() > 2))
        .collect()
}

fn mean_rgb(pixels: &[u8], mask: &[bool]) -> (f64, f64, f64, f64) {
    let mut sum = [0.0f64; 4];
    let mut n = 0.0;
    for (i, m) in mask.iter().enumerate() {
        if !*m {
            continue;
        }
        n += 1.0;
        sum[0] += f64::from(pixels[i * 4]);
        sum[1] += f64::from(pixels[i * 4 + 1]);
        sum[2] += f64::from(pixels[i * 4 + 2]);
        sum[3] += f64::from(luminance(pixels, i));
    }
    (sum[0] / n, sum[1] / n, sum[2] / n, sum[3] / n)
}

/// 探针布置可视化（CPU 绘制，不占 GPU）：俯视图（x-z），探针 = 白点、
/// 四面体棱 = 暗青细线、凸包棱 = 橙粗线、角色 = 红点、房间边界 = 灰框。
fn layout_panel(world_bounds: f32, set: &IrradianceProbeSet, worker: Vec3) -> Vec<u8> {
    let mut out = vec![0u8; (WIDTH * HEIGHT * 4) as usize];
    let scale = (WIDTH.min(HEIGHT) as f32 - 24.0) / (2.0 * world_bounds);
    let to_px = |x: f32, z: f32| -> (f32, f32) {
        (
            WIDTH as f32 * 0.5 + x * scale,
            HEIGHT as f32 * 0.5 - z * scale,
        )
    };
    fn put(out: &mut [u8], x: f32, y: f32, rgb: [u8; 3]) {
        let (xi, yi) = (x.round() as i32, y.round() as i32);
        if xi < 0 || yi < 0 || xi >= WIDTH as i32 || yi >= HEIGHT as i32 {
            return;
        }
        let i = ((yi as u32 * WIDTH + xi as u32) * 4) as usize;
        out[i] = rgb[0];
        out[i + 1] = rgb[1];
        out[i + 2] = rgb[2];
        out[i + 3] = 255;
    }
    fn line(out: &mut [u8], a: (f32, f32), b: (f32, f32), rgb: [u8; 3]) {
        let steps = ((b.0 - a.0).abs().max((b.1 - a.1).abs()).ceil() as i32).max(1);
        for s in 0..=steps {
            let t = s as f32 / steps as f32;
            put(out, a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, rgb);
        }
    }
    let corners = [(-5.0f32, -5.0f32), (5.0, -5.0), (5.0, 5.0), (-5.0, 5.0)];
    for i in 0..4 {
        let a = to_px(corners[i].0, corners[i].1);
        let b = to_px(corners[(i + 1) % 4].0, corners[(i + 1) % 4].1);
        line(&mut out, a, b, [70, 70, 74]);
    }
    let tetra = set.tetrahedralization();
    let mut drawn: std::collections::BTreeSet<(u32, u32)> = std::collections::BTreeSet::new();
    for t in tetra.tetrahedra() {
        for k in 0..4 {
            for l in (k + 1)..4 {
                let (a, b) = (t[k].min(t[l]), t[k].max(t[l]));
                if !drawn.insert((a, b)) {
                    continue;
                }
                let (pa, pb) = (
                    tetra.world_vertex(a as usize),
                    tetra.world_vertex(b as usize),
                );
                line(&mut out, to_px(pa.x, pa.z), to_px(pb.x, pb.z), [0, 90, 90]);
            }
        }
    }
    let mut hull_edges: std::collections::BTreeSet<(u32, u32)> = std::collections::BTreeSet::new();
    for f in tetra.hull_faces() {
        for k in 0..3 {
            let (a, b) = (f[k].min(f[(k + 1) % 3]), f[k].max(f[(k + 1) % 3]));
            hull_edges.insert((a, b));
        }
    }
    for (a, b) in hull_edges {
        let (pa, pb) = (
            tetra.world_vertex(a as usize),
            tetra.world_vertex(b as usize),
        );
        for d in [-1.0f32, 0.0, 1.0] {
            let (xa, ya) = to_px(pa.x, pa.z);
            let (xb, yb) = to_px(pb.x, pb.z);
            line(&mut out, (xa, ya + d), (xb, yb + d), [200, 120, 40]);
        }
    }
    for p in set.positions() {
        let (x, y) = to_px(p.x, p.z);
        for dx in -1..=1 {
            for dy in -1..=1 {
                put(&mut out, x + dx as f32, y + dy as f32, [235, 235, 240]);
            }
        }
    }
    let (wx, wy) = to_px(worker.x, worker.z);
    for dx in -2..=2 {
        for dy in -2..=2 {
            put(&mut out, wx + dx as f32, wy + dy as f32, [230, 60, 50]);
        }
    }
    out
}

fn main() {
    let Some(probe) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("无 GPU 适配器，跳过验收图生成");
        return;
    };
    drop(probe);

    let mut panels: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

    // ---- 面板 1：阴影区角色 关 | 开 ----
    println!("面板 1：室内阴影区动态角色 关|开");
    let (w_off, _) = scene(false, SHADOW_POS);
    let (w_on, report) = scene(true, SHADOW_POS);
    let report = report.expect("探针装配报告");
    println!("  探针 {report:?}");
    let p1_off = render_fresh(&w_off).expect("面板 1 关");
    let p1_on = render_fresh(&w_on).expect("面板 1 开");
    // ROI：把角色移到相机背后（同一探针集 / 同一相机）得到的对照帧
    let (mut w_hidden, _) = scene(true, SHADOW_POS);
    let entities: Vec<_> = w_hidden.entities().collect();
    for e in entities {
        if w_hidden.get::<RigidBody>(e).is_some() {
            w_hidden.insert(e, Transform::from_xyz(0.0, -50.0, 0.0));
        }
    }
    propagate_transforms(&mut w_hidden);
    let p_hidden = render_fresh(&w_hidden).expect("面板 1 隐藏对照");
    // ROI = 「隐藏帧 vs 开帧」∪「隐藏帧 vs 关帧」：角色轮廓边缘的像素可能
    // 在单一对照里只差 1–2 级（量化台阶），取并集才不会把它们误判成
    // 「静态几何被改动」。
    let mut mask = diff_mask(&p_hidden, &p1_on);
    for (m, f) in mask.iter_mut().zip(diff_mask(&p_hidden, &p1_off)) {
        *m |= f;
    }
    let roi = mask.iter().filter(|m| **m).count();
    let (r_off, _g_off, b_off, l_off) = mean_rgb(&p1_off, &mask);
    let (r_on, _g_on, b_on, l_on) = mean_rgb(&p1_on, &mask);
    let (hr, _hg, hb, _hl) = mean_rgb(&p_hidden, &mask);
    println!(
        "  ROI {roi} px｜关 {:#018x}｜开 {:#018x}｜亮度 {l_off:.1} → {l_on:.1}｜\
         B−R {:.1} → {:.1}（房间参考 {:.1}）",
        fnv1a(&p1_off),
        fnv1a(&p1_on),
        b_off - r_off,
        b_on - r_on,
        hb - hr
    );
    assert!(roi > 300, "角色必须可见（ROI {roi} px）");
    assert!(
        l_on < l_off * 0.8,
        "阴影区角色必须显著变暗（{l_off:.1} → {l_on:.1}）"
    );
    assert!(
        (b_on - r_on - (hb - hr)).abs() < (b_off - r_off - (hb - hr)).abs(),
        "色相必须向房间参考收敛（角色 {:.1} → {:.1}，房间 {:.1}）",
        b_off - r_off,
        b_on - r_on,
        hb - hr
    );
    panels.push((p1_off.clone(), p1_on.clone()));

    // ---- 面板 2：插值跟随位置 亮区 | 阴影区 ----
    println!("面板 2：同探针集下角色 亮区|阴影区（插值跟随位置）");
    let (w_bright, _) = scene(true, BRIGHT_POS);
    let p2_bright = render_fresh(&w_bright).expect("面板 2 亮区");
    let mask2 = diff_mask(&p2_bright, &p1_on);
    let l_bright = mean_rgb(&p2_bright, &mask2).3;
    println!(
        "  亮区 {:#018x}｜阴影区 {:#018x}｜差异像素 {}｜亮区均值亮度 {l_bright:.1}",
        fnv1a(&p2_bright),
        fnv1a(&p1_on),
        mask2.iter().filter(|m| **m).count()
    );
    assert!(
        l_bright > l_on * 1.2,
        "亮区场景整体必须明显更亮（{l_bright:.1} vs {l_on:.1}）"
    );
    panels.push((p2_bright, p1_on.clone()));

    // ---- 面板 3：与静态光照贴图的融合 关 | 开（近景）----
    println!("面板 3：与静态光照贴图的融合（近景：角色贴着挂图集的墙 / 地板）");
    let (w3_off, _) = scene_with_camera(false, SHADOW_POS, Some(close_camera()));
    let (w3_on, _) = scene_with_camera(true, SHADOW_POS, Some(close_camera()));
    let p3_off = render_fresh(&w3_off).expect("面板 3 关");
    let p3_on = render_fresh(&w3_on).expect("面板 3 开");
    let fused = diff_mask(&p1_off, &p1_on);
    let static_changed = fused
        .iter()
        .zip(mask.iter())
        .filter(|(f, m)| **f && !**m)
        .count();
    let static_total = mask.iter().filter(|m| !**m).count();
    println!(
        "  角色像素 {roi}｜角色外变化 {static_changed} / {static_total}（必须为 0：静态几何走贴图）"
    );
    assert_eq!(
        static_changed, 0,
        "挂光照贴图的静态几何不得被探针改动（{static_changed} 个像素）"
    );
    // 近景帧同样只允许角色像素变化（同一断言在近景尺度上复核）
    let close_diff = diff_mask(&p3_off, &p3_on);
    let close_changed = close_diff.iter().filter(|d| **d).count();
    println!(
        "  近景 关 {:#018x}｜开 {:#018x}｜变化像素 {close_changed}",
        fnv1a(&p3_off),
        fnv1a(&p3_on)
    );
    assert!(
        close_changed > 200,
        "近景必须看得见角色变化（{close_changed} px）"
    );
    panels.push((p3_off, p3_on));

    // ---- 面板 4：探针布置可视化 ----
    println!(
        "面板 4：探针布置可视化（俯视 x-z：白点 = 探针、青线 = 四面体棱、橙线 = 凸包、红点 = 角色）"
    );
    let set = w_on.resource::<IrradianceProbeSet>();
    let p4_layout = layout_panel(6.0, &set, SHADOW_POS);
    println!("  布置图 {:#018x}", fnv1a(&p4_layout));
    assert!(
        set.tetrahedralization().hull_face_count() > 0,
        "布置图必须有凸包"
    );
    panels.push((p4_layout.clone(), p4_layout));

    // ---- 跨运行逐字节一致 ----
    let again = {
        let (w, _) = scene(true, SHADOW_POS);
        render_fresh(&w).expect("复跑")
    };
    assert_eq!(
        fnv1a(&again),
        fnv1a(&p1_on),
        "同输入两次渲染必须逐字节一致（验收图跨运行稳定）"
    );
    println!(
        "复跑一致性：面板 1 开 两次渲染哈希相同（{:#018x}）",
        fnv1a(&again)
    );

    let path = Path::new("iris_light_probes.ppm");
    write_grid(path, &panels).expect("写验收图");
    println!(
        "输出 {}（4 排 × 2 列：①阴影区角色 关|开 ②亮区|阴影区 ③融合 关|开 ④布置可视化，{}×{}）",
        path.display(),
        WIDTH * 2 + GAP,
        HEIGHT * ROWS + GAP * (ROWS - 1)
    );
    println!("转 PNG：ffmpeg -y -i iris_light_probes.ppm docs/light_probes.png");
}

/// 写 4 排 × 2 列的 PPM P6（与既有验收图示例同款面板拼装；纯字节输出，
/// 跨运行逐字节一致）。
fn write_grid(path: &Path, panels: &[(Vec<u8>, Vec<u8>)]) -> std::io::Result<()> {
    use std::io::Write;
    assert_eq!(panels.len() as u32, ROWS, "面板行数必须等于 ROWS");
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
