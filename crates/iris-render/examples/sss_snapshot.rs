//! 预积分皮肤次表面散射验收图（R63）：五排面板对照，输出 `iris_sss.ppm`
//! → `docs/sss.png`。
//!
//! 面板布局（5 排 × 2 列，每排左右并排）：
//! 1. **终结线 关|开**：单位球 + 侧向方向光（无环境项，硬边 vs 包裹软坡的
//!    最干净对照）；
//! 2. **曲率尺度梯度（强 0.05 | 缺省 0.10）**：strength 同为 1，
//!    scale = 0.05 m（t = 1，α = 90°，强烈包裹）与缺省档（面板 1 的「开」，
//!    同帧复用）；
//! 3. **弱档 0.30 | LUT 可视化**：三档尺度按阅读顺序连排（0.05 → 0.10 →
//!    0.30，软化程度单调递减）；右格是程序化生成的 64×64 预积分 LUT 本体
//!    （横轴 NdotL 从 −1 到 +1、纵轴曲率包裹参数 t 从 0 到 1，越亮 =
//!    漫反射响应越强，t = 0.5 处一条暗线标出缺省档）；
//! 4. **阴影区对照 关|开**：点光 cubemap 投影阴影 + 球体与地面同为皮肤
//!    材质——阴影内直射辐亮度恒 0（`shadow = 0` 硬零），SSS 补项为 0，
//!    背光侧的包裹晒不穿投影阴影（关|开在阴影区**逐字节一致**）；
//! 5. **人形角色实拍 关|开**：`assets/assault_bot.glb`（现有 GLB 角色）——
//!    绝缘体材质（metallic < 0.2，躯干 / 四肢 / 布面）做**皮肤化覆写**：
//!    基色改肤色 + 开启 SSS，保留原有法线 / ORM 通道；金属装备（头盔 /
//!    护木 / 刃）原样保留（金属无漫反射 ⇒ 无 SSS，正好当对照）。
//!
//! 每格都带硬断言（不通过直接 panic、不写文件）；跨运行逐字节一致
//! （同输入两次渲染的 FNV-1a 全帧哈希相同，本示例打印哈希供比对）。
//!
//! ```sh
//! cargo run -p iris-render --example sss_snapshot --release
//! ffmpeg -y -i iris_sss.ppm docs/sss.png
//! ```

use std::path::Path;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Entity, Transform, World};
use iris_render::prelude::*;
use iris_render::sss;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
const GAP: u32 = 6;
/// 面板行数（每组一行，行内两格）。
const ROWS: u32 = 5;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 每个面板各用独立渲染器：渲染器按**句柄值**缓存材质绑定组，同一渲染器上
/// 渲染「同句柄不同内容」的世界会串味（R53/R62 教训；本轮的 关|开 对照
/// 恰好是「1 号材质、不同 SSS 参数」，实测复用渲染器时差异为 0 字节）。
fn render_fresh(world: &World) -> Option<Vec<u8>> {
    let mut renderer = OffscreenRenderer::new(WIDTH, HEIGHT)?;
    renderer.render(world);
    Some(renderer.read_pixels())
}

/// 球体场景：除方向光外无任何环境项（暗面恰为全黑，包裹的边界一眼可辨）。
fn sphere_world(strength: f32, scale: f32, camera: Camera) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(camera);
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-1.0, -0.25, 0.0).normalize(),
        color: Vec3::new(1.0, 0.97, 0.93),
        intensity: 3.2,
        ambient: Vec3::ZERO,
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(EnvironmentSettings { intensity: 0.0 });
    world.insert_resource(BloomSettings::disabled());
    let mesh = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, 128, 64));
    let mut mat = MaterialDef::solid(0.92, 0.72, 0.62, 1.0);
    mat.roughness = 1.0;
    mat.subsurface_strength = strength;
    mat.subsurface_scale = scale;
    let handle = world.resource_mut::<MaterialStore>().add(mat);
    world.spawn((Transform::IDENTITY, mesh, handle));
    propagate_transforms(&mut world);
    world
}

fn sphere_camera() -> Camera {
    Camera {
        eye: Vec3::new(0.0, 0.0, 8.5),
        target: Vec3::ZERO,
        fov_y_radians: 15f32.to_radians(),
        ..Camera::default()
    }
}

/// 阴影场景：地面 + 悬空球体（**同一种皮肤材质**）+ 头顶投影点光。
/// 无方向光、无环境项 ⇒ 阴影内只剩 0。
fn shadow_world(strength: f32) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(3.0, 3.4, 6.2),
        target: Vec3::new(0.0, 0.9, 0.0),
        fov_y_radians: 40f32.to_radians(),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        intensity: 0.0,
        ambient: Vec3::ZERO,
        ..DirectionalLight::default()
    });
    world.insert_resource(EnvironmentSettings { intensity: 0.0 });
    world.insert_resource(BloomSettings::disabled());
    let mut skin = MaterialDef::solid(0.92, 0.72, 0.62, 1.0);
    skin.roughness = 0.9;
    skin.subsurface_strength = strength;
    skin.subsurface_scale = 0.1;
    let skin_mat = world.resource_mut::<MaterialStore>().add(skin);
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(22.0));
    world.spawn((Transform::IDENTITY, plane, skin_mat));
    let ball = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(0.85, 96, 48));
    world.spawn((Transform::from_xyz(0.0, 1.75, 0.0), ball, skin_mat));
    world.spawn((
        Transform::from_xyz(0.0, 5.6, 0.0),
        PointLight {
            intensity: 90.0,
            range: 18.0,
            shadows: true,
            ..PointLight::default()
        },
    ));
    propagate_transforms(&mut world);
    world
}

/// 角色场景：现有 GLB（`assault_bot.glb`）+ 皮肤化材质覆写。
/// 覆写规则：**绝缘体材质**（metallic < 0.2，即躯干 / 四肢 / 布面）基色改
/// 肤色并开启 SSS，保留原有法线 / ORM 通道；金属装备原样保留。
fn character_world(strength: f32) -> (World, usize) {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(2.30, 1.62, 3.75),
        target: Vec3::new(0.0, 0.92, 0.0),
        fov_y_radians: 38f32.to_radians(),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-1.0, -0.22, 0.45).normalize(),
        color: Vec3::new(1.0, 0.96, 0.9),
        intensity: 3.4,
        ambient: Vec3::new(0.05, 0.055, 0.07),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(EnvironmentSettings { intensity: 0.3 });
    world.insert_resource(BloomSettings::disabled());
    spawn_gltf_instance(&mut world, "assets/assault_bot.glb").expect("加载 assets/assault_bot.glb");

    // 先登记皮肤材质，再逐实体替换（收集 → 写回两阶段，避免借用冲突）
    let mut skin = MaterialDef::solid(0.92, 0.73, 0.63, 1.0);
    skin.roughness = 0.55;
    skin.subsurface_strength = strength;
    skin.subsurface_scale = 0.1;
    // 角色皮肤用满强度（面板语义 = 「开」），尺度取缺省档（头 / 前臂量级）
    let skin_mat = world.resource_mut::<MaterialStore>().add(skin);
    let mut targets: Vec<Entity> = Vec::new();
    for e in world.entities() {
        // 只认「有网格 + 有材质」的图元节点（材质句柄解引用成值再查定义）
        if world.get::<MeshHandle>(e).is_none() {
            continue;
        }
        let Some(current) = world.get::<MaterialHandle>(e).map(|m| *m) else {
            continue;
        };
        let is_skin = world
            .resource::<MaterialStore>()
            .get(current)
            .is_some_and(|d| d.metallic < 0.2);
        if is_skin {
            targets.push(e);
        }
    }
    for e in &targets {
        world.insert(*e, skin_mat);
    }
    propagate_transforms(&mut world);
    (world, targets.len())
}

/// LUT 本体的可视化面板（CPU 生成，不占 GPU）：横轴 NdotL（−1 → +1）、
/// 纵轴曲率包裹参数 t（0 → 1），灰度 = 原始响应值的 1/2.2 幂（显示用）；
/// 四周留 1 px 边框，另在 t = 0.5（缺省档）处画一条暗线。
fn lut_panel() -> Vec<u8> {
    let values = sss::lut_values(sss::SSS_LUT_SIZE);
    let mut out = vec![0u8; (WIDTH * HEIGHT * 4) as usize];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let i = ((y * WIDTH + x) * 4) as usize;
            let border = x < 2 || y < 2 || x >= WIDTH - 2 || y >= HEIGHT - 2;
            let v = if border {
                0.08
            } else {
                let ndotl = (x as f32 / (WIDTH - 1) as f32) * 2.0 - 1.0;
                let t = y as f32 / (HEIGHT - 1) as f32;
                let raw = sss::lut_lookup(&values, sss::SSS_LUT_SIZE, ndotl, t);
                let marker = (t - 0.5).abs() < 0.006;
                if marker { raw * 0.35 } else { raw }
            };
            let byte = (v.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0).round() as u8;
            out[i] = byte;
            out[i + 1] = byte;
            out[i + 2] = byte;
            out[i + 3] = 255;
        }
    }
    out
}

/// 差异像素数（RGB 任一通道不同即计入）。
fn diff_pixels(a: &[u8], b: &[u8]) -> usize {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .filter(|(x, y)| x != y)
        .count()
}

/// 「变亮像素」数（开帧比关帧亮的像素，取 RGB 之和）。
fn brightened_pixels(off: &[u8], on: &[u8]) -> usize {
    off.chunks_exact(4)
        .zip(on.chunks_exact(4))
        .filter(|(a, b)| {
            let sa = u32::from(a[0]) + u32::from(a[1]) + u32::from(a[2]);
            let sb = u32::from(b[0]) + u32::from(b[1]) + u32::from(b[2]);
            sb > sa
        })
        .count()
}

fn main() {
    let Some(probe) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("无 GPU 适配器，跳过验收图生成");
        return;
    };
    drop(probe);

    let mut panels: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

    // ---- 面板 1：终结线 关 | 开 ----
    println!("面板 1：终结线 关|开（scale = 0.10，缺省档）");
    let cam = sphere_camera();
    let p1_off = render_fresh(&sphere_world(0.0, 0.10, cam.clone())).expect("面板 1 关");
    let p1_on = render_fresh(&sphere_world(1.0, 0.10, cam)).expect("面板 1 开");
    let p1_diff = diff_pixels(&p1_off, &p1_on);
    let p1_bright = brightened_pixels(&p1_off, &p1_on);
    println!(
        "  关 {:#018x}｜开 {:#018x}｜差异像素 {p1_diff}｜变亮像素 {p1_bright}",
        fnv1a(&p1_off),
        fnv1a(&p1_on)
    );
    assert!(
        p1_bright > 3000,
        "包裹必须点亮终结线背光侧的大片像素（实测 {p1_bright}）"
    );
    panels.push((p1_off, p1_on));

    // ---- 面板 2：曲率尺度梯度 强(0.05) | 弱(0.30) ----
    println!("面板 2：曲率尺度 强 scale=0.05 | 缺省 scale=0.10");
    let cam = sphere_camera();
    let p2_strong = render_fresh(&sphere_world(1.0, 0.05, cam.clone())).expect("面板 2 强");
    let p2_weak = render_fresh(&sphere_world(1.0, 0.30, cam)).expect("面板 2 弱");
    let baseline = &panels[0].0;
    let strong_bright = brightened_pixels(baseline, &p2_strong);
    let weak_bright = brightened_pixels(baseline, &p2_weak);
    println!(
        "  强 {:#018x}（变亮 {strong_bright}）｜弱 {:#018x}（变亮 {weak_bright}）｜缺省档变亮 {p1_bright}",
        fnv1a(&p2_strong),
        fnv1a(&p2_weak)
    );
    assert!(
        strong_bright > p1_bright && p1_bright > weak_bright,
        "软化程度必须随曲率尺度单调：0.05 ({strong_bright}) > 0.10 ({p1_bright}) > 0.30 ({weak_bright})"
    );

    // ---- 面板 3：弱档 | LUT 本体 ----
    println!(
        "面板 3：弱档 scale=0.30 | 预积分 LUT 本体（64×64 放大）——三档尺度连排 0.05 → 0.10 → 0.30"
    );
    let p3_lut = lut_panel();
    println!(
        "  LUT 面板 {:#018x}（横轴 NdotL、纵轴曲率 t）",
        fnv1a(&p3_lut)
    );
    // 三档尺度按阅读顺序连排：面板 2 左（0.05）→ 面板 2 右（0.10，与面板 1 开
    // 同帧，复用）→ 面板 3 左（0.30）
    panels.push((p2_strong, panels[0].1.clone()));
    panels.push((p2_weak, p3_lut));

    // ---- 面板 4：阴影区对照 关 | 开 ----
    println!("面板 4：点光投影阴影 关|开（球体与地面同为皮肤材质）");
    let no_shadow = {
        let w = shadow_world(0.0);
        for e in w.entities().collect::<Vec<_>>() {
            if let Some(mut light) = w.get_mut::<PointLight>(e) {
                light.shadows = false;
            }
        }
        render_fresh(&w).expect("面板 4 无影对照")
    };
    let p4_off = render_fresh(&shadow_world(0.0)).expect("面板 4 关");
    let p4_on = render_fresh(&shadow_world(1.0)).expect("面板 4 开");
    let mask: Vec<bool> = no_shadow
        .chunks_exact(4)
        .zip(p4_off.chunks_exact(4))
        .map(|(a, b)| b[0] < a[0] || b[1] < a[1] || b[2] < a[2])
        .collect();
    let masked = mask.iter().filter(|m| **m).count();
    let mut inside = 0usize;
    let mut outside = 0usize;
    for (i, m) in mask.iter().enumerate() {
        let a = &p4_off[i * 4..i * 4 + 3];
        let b = &p4_on[i * 4..i * 4 + 3];
        if a != b {
            if *m {
                inside += 1;
            } else {
                outside += 1;
            }
        }
    }
    println!(
        "  阴影掩码 {masked} px｜关 {:#018x}｜开 {:#018x}｜阴影内差异 {inside}｜阴影外差异 {outside}",
        fnv1a(&p4_off),
        fnv1a(&p4_on)
    );
    assert!(masked > 2000, "阴影掩码过小（{masked} px），投影没生效");
    assert_eq!(
        inside, 0,
        "投影阴影内的像素必须与 SSS 强度逐位无关（{inside} 个像素被改变）——包裹晒穿了阴影"
    );
    assert!(
        outside > 3000,
        "阴影外必须有可见的包裹（仅 {outside} 个像素不同）"
    );
    panels.push((p4_off, p4_on));

    // ---- 面板 5：人形角色实拍 关 | 开 ----
    println!("面板 5：人形角色（assault_bot.glb，皮肤化覆写）关|开");
    let (world_off, swapped) = character_world(0.0);
    let (world_on, _) = character_world(1.0);
    let p5_off = render_fresh(&world_off).expect("面板 5 关");
    let p5_on = render_fresh(&world_on).expect("面板 5 开");
    let p5_diff = diff_pixels(&p5_off, &p5_on);
    let p5_bright = brightened_pixels(&p5_off, &p5_on);
    println!(
        "  皮肤化材质覆写 {swapped} 个网格节点｜关 {:#018x}｜开 {:#018x}｜差异像素 {p5_diff}｜变亮 {p5_bright}",
        fnv1a(&p5_off),
        fnv1a(&p5_on)
    );
    assert!(
        swapped > 3,
        "角色必须至少有若干皮肤网格节点（实测 {swapped}）"
    );
    assert!(
        p5_bright > 1500,
        "角色上的 SSS 必须有可见效果（变亮 {p5_bright} 像素）"
    );
    panels.push((p5_off, p5_on));

    // ---- 跨运行逐字节一致：同输入再渲一帧，哈希必须相同 ----
    let cam = sphere_camera();
    let again = render_fresh(&sphere_world(1.0, 0.10, cam)).expect("复跑");
    assert_eq!(
        fnv1a(&again),
        fnv1a(&panels[0].1),
        "同输入两次渲染必须逐字节一致（验收图跨运行稳定）"
    );
    println!(
        "复跑一致性：面板 1 开 两次渲染哈希相同（{:#018x}）",
        fnv1a(&again)
    );

    let path = Path::new("iris_sss.ppm");
    write_grid(path, &panels).expect("写验收图");
    println!(
        "输出 {}（5 排 × 2 列：①终结线 关|开（scale 0.10）②尺度 0.05|0.10 \
         ③尺度 0.30|LUT ④阴影区 关|开 ⑤角色 关|开，{}×{}）",
        path.display(),
        WIDTH * 2 + GAP,
        HEIGHT * ROWS + GAP * (ROWS - 1)
    );
    println!("转 PNG：ffmpeg -y -i iris_sss.ppm docs/sss.png");
}

/// 写 5 排 × 2 列的 PPM P6（与既有验收图示例同款面板拼装；纯字节输出，
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
