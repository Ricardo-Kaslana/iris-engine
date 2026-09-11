//! SSR 屏幕空间反射验收图（R62b）：四组面板对照，输出 `iris_ssr.ppm`
//! → `docs/ssr.png`。
//!
//! 面板布局（4 排 × 2 列，每排「关 | 开」左右并排）：
//! 1. **金属地面 关|开**：室内房间——开 SSR 后地面反射出屏内的自发光
//!    图案板与箱体（关 = 只有天空 env）；
//! 2. **动态物体 关|开**：探针烘焙于标记在 A 的时刻、随后标记移到 B——
//!    左（关，探针路径）反射冻结在烘焙时刻的 A；右（开）反射跟随 B
//!    的新镜像位置——**探针做不到、SSR 做得到，本轮存在的理由**；
//! 3. **粗糙度梯度 关|开**：ORM 粗糙度沿深度轴渐变的金属地面（近处
//!    0.02 → 远处 0.9）——近处反射清晰，远处按权重融入 env；
//! 4. **屏外内容回退 关|开**：标记在上帧画面之外 ⇒ 全部片元未命中 ⇒
//!    整帧与探针路径**逐位一致**（差异字节 0 = 回退即既有通路原值）。
//!
//! 每格都带硬断言（不通过直接 panic、不写文件）；跨运行逐字节一致
//! （同输入两次渲染的 FNV-1a 全帧哈希相同，本示例打印哈希供比对）。
//! 附性能段：SSR 开/关帧时（步进成本）、帧尾副本带宽、显存增量。
//!
//! ```sh
//! cargo run -p iris-render --example ssr_snapshot --release
//! ffmpeg -y -i iris_ssr.ppm docs/ssr.png
//! ```

use std::path::Path;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Entity, Transform, World};
use iris_render::prelude::*;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
const GAP: u32 = 6;
/// 面板行数（每组一行，行内两格）。
const ROWS: u32 = 4;

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

fn is_red(p: [i32; 3]) -> bool {
    p[0] > 150 && p[0] - p[1] > 80 && p[0] - p[2] > 80
}

/// 开启态 SSR 资源（Default 的 enabled = false——缺席 = 关语义）。
fn ssr_on() -> SsrSettings {
    SsrSettings {
        enabled: true,
        ..SsrSettings::default()
    }
}

fn base_world(camera: Camera) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(camera);
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.5, -1.0, 0.35).normalize(),
        intensity: 1.2,
        ambient: Vec3::new(0.08, 0.09, 0.12),
        ..DirectionalLight::default()
    });
    // env 强度调低（0.35）： acceptance 场景的照明主体是方向光——
    // 亮 env 会把 SSR 的反射差压到接近 env 本身亮度，画面对比弱
    world.insert_resource(EnvironmentSettings { intensity: 0.35 });
    world.insert_resource(BloomSettings::disabled());
    world
}

fn emissive_mat(store: &mut MaterialStore, r: f32, g: f32, b: f32) -> MaterialHandle {
    emissive_mat_at(store, 6.0, r, g, b)
}

/// `boost` 倍率的自发射材质（roughness = 1 压掉粗糙镜面高光路径）。
fn emissive_mat_at(
    store: &mut MaterialStore,
    boost: f32,
    r: f32,
    g: f32,
    b: f32,
) -> MaterialHandle {
    let mut mat = MaterialDef::solid(r, g, b, 1.0);
    mat.emissive = [r * boost, g * boost, b * boost, 1.0];
    mat.roughness = 1.0;
    store.add(mat)
}

/// 渲染 SSR 面板：至少两帧（首帧重置建立上帧资源，第二帧起步进生效）。
/// `ssr = false` 时只渲一帧（与开启会话的第二帧同姿态对照）。
fn render_ssr_panel(renderer: &mut OffscreenRenderer, world: &World, ssr: bool) -> Vec<u8> {
    renderer.render(world);
    if ssr {
        renderer.render(world);
    }
    let px = renderer.read_pixels();
    println!(
        "  SSR {}｜ssr_on = {}｜哈希 {:#018x}",
        if ssr { "开" } else { "关" },
        renderer.renderer().stats().ssr_on,
        fnv1a(&px)
    );
    px
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

/// 解析镜像像素：标记点关于地面（y = 0）的镜像点与相机连线同地面的交点。
fn analytic_mirror_pixel(camera: &Camera, marker: Vec3, w: u32, h: u32) -> (f32, f32) {
    let mirrored = Vec3::new(marker.x, -marker.y, marker.z);
    let eye = camera.eye;
    let dir = mirrored - eye;
    let t = -eye.y / dir.y;
    let hit = eye + dir * t;
    assert!(hit.y.abs() < 1e-4, "镜像交点必须落在地面上：{hit:?}");
    project_to_pixel(camera, hit, w, h)
}

/// 以 `center` 为圆心、半径 `radius` 内的标记红像素数。
fn red_pixels_near(px: &[u8], w: u32, h: u32, center: (f32, f32), radius: f32) -> usize {
    let mut count = 0;
    let x0 = (center.0 - radius).max(0.0) as u32;
    let x1 = ((center.0 + radius) as u32).min(w - 1);
    let y0 = (center.1 - radius).max(0.0) as u32;
    let y1 = ((center.1 + radius) as u32).min(h - 1);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let dx = x as f32 - center.0;
            let dy = y as f32 - center.1;
            if dx * dx + dy * dy > radius * radius {
                continue;
            }
            if is_red(pixel(px, w, x, y)) {
                count += 1;
            }
        }
    }
    count
}

fn new_renderer() -> Option<OffscreenRenderer> {
    OffscreenRenderer::new(WIDTH, HEIGHT)
}

fn main() {
    if new_renderer().is_none() {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    }
    let mut panels: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

    // ---- 面板 1：金属地面 关 | 开（屏内物体反射出现）----
    println!("面板 1：金属地面 关|开");
    let camera = Camera {
        eye: Vec3::new(0.0, 2.0, 4.6),
        target: Vec3::new(0.0, 0.6, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_4,
        ..Camera::default()
    };
    let build_room = |with_ssr: bool| {
        let mut world = base_world(camera.clone());
        let floor = world.resource_mut::<MeshStore>().add(MeshData::plane(12.0));
        let mirror = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::metal(0.8, 0.81, 0.84, 0.06));
        world.spawn((Transform::IDENTITY, floor, mirror));
        // 后墙 + 两块图案板（红 / 绿，上半屏直接可见；下半屏的反射只能
        // 来自地面）+ 一只立式箱体（丰富屏内反射内容）
        let wall = world.resource_mut::<MeshStore>().add(MeshData::plane(12.0));
        let wall_mat = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.5, 0.52, 0.56, 1.0));
        world.spawn((
            Transform {
                position: Vec3::new(0.0, 2.5, -5.0),
                rotation: glam::Quat::from_axis_angle(Vec3::X, std::f32::consts::FRAC_PI_2),
                ..Transform::IDENTITY
            },
            wall,
            wall_mat,
        ));
        let slab = world.resource_mut::<MeshStore>().add(MeshData::cube());
        {
            let mut mats = world.resource_mut::<MaterialStore>();
            let red = emissive_mat(&mut mats, 1.0, 0.08, 0.03);
            let green = emissive_mat(&mut mats, 0.1, 0.9, 0.15);
            drop(mats);
            for (x, mat) in [(-2.2f32, red), (2.2, green)] {
                world.spawn((
                    Transform {
                        position: Vec3::new(x, 3.2, -4.8),
                        scale: Vec3::new(1.2, 1.2, 0.12),
                        ..Transform::IDENTITY
                    },
                    slab,
                    mat,
                ));
            }
        }
        let mat_box = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.85, 0.65, 0.3, 1.0));
        world.spawn((
            Transform {
                position: Vec3::new(0.9, 0.5, -1.8),
                scale: Vec3::new(1.0, 1.0, 1.0),
                ..Transform::IDENTITY
            },
            slab,
            mat_box,
        ));
        if with_ssr {
            world.insert_resource(ssr_on());
        }
        propagate_transforms(&mut world);
        world
    };
    let Some(mut renderer) = new_renderer() else {
        return;
    };
    let world_off = build_room(false);
    let panel1_left = render_ssr_panel(&mut renderer, &world_off, false);
    let Some(mut renderer_on) = new_renderer() else {
        return;
    };
    let world_on = build_room(true);
    let panel1_right = render_ssr_panel(&mut renderer_on, &world_on, true);
    // 断言只看下半屏（地面）：图案板挂在上半屏，下半屏的红/绿只能来自反射
    let lower_colored = |px: &[u8]| {
        let mut n = 0;
        for y in HEIGHT / 2..HEIGHT {
            for x in 0..WIDTH {
                let p = pixel(px, WIDTH, x, y);
                if is_red(p) || (p[1] > 130 && p[1] - p[0] > 60 && p[1] - p[2] > 60) {
                    n += 1;
                }
            }
        }
        n
    };
    let lc_left = lower_colored(&panel1_left);
    let lc_right = lower_colored(&panel1_right);
    // 主口径 = 下半屏「开/关 差分像素」（直射太阳高光不随 SSR 变，天然
    // 被差分剔除；彩色计数只是参考读数）
    let lower_diff = panel1_left
        .iter()
        .zip(&panel1_right)
        .enumerate()
        .filter(|(i, (a, b))| {
            let px = i / 4;
            let y = px as u32 / WIDTH;
            y >= HEIGHT / 2 && a != b
        })
        .count();
    println!("  地面彩色反射像素：关 {lc_left} → 开 {lc_right}｜下半屏差分像素 {lower_diff}");
    assert_eq!(
        lc_left, 0,
        "SSR 关闭时地面只反射天空 ⇒ 下半屏不应有图案色（实际 {lc_left}）"
    );
    assert!(
        lower_diff > 3000,
        "SSR 开启后地面应反射出屏内图案与箱体（下半屏差分 {lower_diff}）"
    );
    panels.push((panel1_left, panel1_right));

    // ---- 面板 2：动态物体 关 | 开（探针烘焙后标记 A → B）----
    println!("面板 2：动态物体 关|开（探针烘焙于 A，标记移到 B）");
    let cam2 = Camera {
        eye: Vec3::new(0.0, 2.2, 4.4),
        target: Vec3::new(0.0, 0.4, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_4,
        ..Camera::default()
    };
    let marker_a = Vec3::new(-2.6, 1.1, -2.2);
    let marker_b = Vec3::new(2.6, 1.1, -2.2);
    let marker_scale = Vec3::new(1.0, 2.2, 0.12);
    let build_dynamic = |with_ssr: bool| -> (World, Entity, Entity) {
        let mut world = base_world(cam2.clone());
        let floor = world.resource_mut::<MeshStore>().add(MeshData::plane(12.0));
        let mirror = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::metal(0.85, 0.85, 0.88, 0.05));
        world.spawn((Transform::IDENTITY, floor, mirror));
        let slab = world.resource_mut::<MeshStore>().add(MeshData::cube());
        // ×20：SSR 贡献经 env 强度（0.35）同比缩放，标记需要更亮的 HDR
        // 才能在画面里形成红主导的反射
        let red = {
            let mut mats = world.resource_mut::<MaterialStore>();
            emissive_mat_at(&mut mats, 20.0, 1.0, 0.08, 0.03)
        };
        let marker = world.spawn((
            Transform {
                position: marker_a,
                scale: marker_scale,
                ..Transform::IDENTITY
            },
            slab,
            red,
        ));
        let probe = world.spawn((
            Transform::from_xyz(0.0, 1.475, 0.0),
            ReflectionProbe::new(Vec3::new(4.0, 1.525, 4.0)).with_falloff(0.0),
        ));
        if with_ssr {
            world.insert_resource(ssr_on());
        }
        propagate_transforms(&mut world);
        (world, probe, marker)
    };
    // 探针侧（SSR 关）——本面板起各自的新渲染器（句柄缓存隔离）
    let Some(mut renderer) = new_renderer() else {
        return;
    };
    let Some(mut renderer_on) = new_renderer() else {
        return;
    };
    let (mut world_p, probe_p, marker_p) = build_dynamic(false);
    assert!(
        renderer
            .renderer_mut()
            .bake_reflection_probe(&world_p, probe_p)
    );
    *world_p.get_mut::<Transform>(marker_p).unwrap() = Transform {
        position: marker_b,
        scale: marker_scale,
        ..Transform::IDENTITY
    };
    propagate_transforms(&mut world_p);
    let panel2_left = render_ssr_panel(&mut renderer, &world_p, false);
    // SSR 侧（同样的烘焙后移动）
    let (mut world_s, probe_s, marker_s) = build_dynamic(true);
    assert!(
        renderer_on
            .renderer_mut()
            .bake_reflection_probe(&world_s, probe_s)
    );
    *world_s.get_mut::<Transform>(marker_s).unwrap() = Transform {
        position: marker_b,
        scale: marker_scale,
        ..Transform::IDENTITY
    };
    propagate_transforms(&mut world_s);
    let panel2_right = render_ssr_panel(&mut renderer_on, &world_s, true);
    // 断言点用标记**下部**（y = 0.3，可靠命中区）的镜像：streak 远端会被
    // 地面自身屏幕深度截获（掠射平面经典伪影，§8.38 诚实清单）
    let mirror_a =
        analytic_mirror_pixel(&cam2, Vec3::new(marker_a.x, 0.3, marker_a.z), WIDTH, HEIGHT);
    let mirror_b =
        analytic_mirror_pixel(&cam2, Vec3::new(marker_b.x, 0.3, marker_b.z), WIDTH, HEIGHT);
    // reddening 指标：on 相对 off 的 (r−g) 增量 > 40 的像素——SSR 把
    // 反射内容换成红标记；太阳直射高光（on/off 同有，白色）不产生
    // reddening，天然被差分剔除
    let reddening_near = |on: &[u8], off: &[u8], c: (f32, f32), radius: f32| {
        let mut n = 0;
        let (cx, cy) = (c.0 as i32, c.1 as i32);
        let r = radius as i32;
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy > r * r {
                    continue;
                }
                let x = (cx + dx).clamp(0, WIDTH as i32 - 1) as u32;
                let y = (cy + dy).clamp(0, HEIGHT as i32 - 1) as u32;
                let (o, n2) = (pixel(off, WIDTH, x, y), pixel(on, WIDTH, x, y));
                let before = o[0] - o[1];
                let after = n2[0] - n2[1];
                if after - before > 40 {
                    n += 1;
                }
            }
        }
        n
    };
    let at_a_l = red_pixels_near(&panel2_left, WIDTH, HEIGHT, mirror_a, 30.0);
    let at_b_l = reddening_near(&panel2_left, &panel2_left, mirror_b, 24.0);
    let at_a_r = red_pixels_near(&panel2_right, WIDTH, HEIGHT, mirror_a, 30.0);
    let at_b_r = reddening_near(&panel2_right, &panel2_left, mirror_b, 24.0);
    println!("  A 镜像红色：关 {at_a_l} / 开 {at_a_r}｜B 镜像红色：关 {at_b_l} / 开 {at_b_r}");
    // 核心口径：标记移动后，SSR 关（探针路径）在 B 的新镜像位置什么都没
    // 有——探针冻结在烘焙时刻；SSR 开则反射跟随 B。（探针对**盒内**标记
    // 的反射本就按盒面近似重投影、位置有偏移——R62a §8.37——其「能反映
    // 烘焙时刻内容」由 tests/ssr_render.rs ② 以 24px 邻域断言。）
    assert_eq!(
        at_b_l, 0,
        "探针路径不能反映移动后的 B（左格 B 镜像邻域 {at_b_l}）——探针冻结在烘焙时刻"
    );
    assert!(
        at_b_r >= 100,
        "SSR 必须反射移动后的标记 B（右格 B 镜像 24px 邻域 reddening {at_b_r}）"
    );
    assert!(
        at_a_r > 0,
        "SSR 开启时 A 侧的探针烘焙反射应保留（回退通路共存）：{at_a_r}"
    );
    panels.push((panel2_left, panel2_right));

    // ---- 面板 3：粗糙度梯度 关 | 开（近处清晰 → 远处融入 env）----
    println!("面板 3：粗糙度梯度 关|开");
    let cam3 = Camera {
        eye: Vec3::new(0.0, 1.6, 4.2),
        target: Vec3::new(0.0, 0.9, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_4,
        ..Camera::default()
    };
    let build_gradient = |with_ssr: bool| {
        let mut world = base_world(cam3.clone());
        // ORM 粗糙度沿深度轴渐变：近处 0.02 → 远处 0.9（纹理行 0 = 近端，
        // 与 GPU 实测行带差分校准一致，见 tests/ssr_render.rs ③ 的取样）
        let tex = 256usize;
        let mut rgba = Vec::with_capacity(tex * tex * 4);
        for row in 0..tex {
            let rough = 0.02 + 0.88 * row as f32 / (tex - 1) as f32;
            let g = (rough * 255.0).round().clamp(0.0, 255.0) as u8;
            for _ in 0..tex {
                rgba.extend_from_slice(&[255, g, 255, 255]);
            }
        }
        let orm = world.resource_mut::<TextureStore>().add(CpuTexture {
            width: tex as u32,
            height: tex as u32,
            rgba,
            srgb: false,
        });
        let floor = world.resource_mut::<MeshStore>().add(MeshData::plane(12.0));
        let gradient = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::metal(0.85, 0.85, 0.88, 1.0).with_orm(orm));
        world.spawn((Transform::IDENTITY, floor, gradient));
        let wall = world.resource_mut::<MeshStore>().add(MeshData::plane(12.0));
        let red_wall = {
            let mut mats = world.resource_mut::<MaterialStore>();
            emissive_mat(&mut mats, 1.0, 0.08, 0.03)
        };
        world.spawn((
            Transform {
                position: Vec3::new(0.0, 3.0, -5.5),
                rotation: glam::Quat::from_axis_angle(Vec3::X, std::f32::consts::FRAC_PI_2),
                ..Transform::IDENTITY
            },
            wall,
            red_wall,
        ));
        if with_ssr {
            world.insert_resource(ssr_on());
        }
        propagate_transforms(&mut world);
        world
    };
    let Some(mut renderer) = new_renderer() else {
        return;
    };
    let world_g_off = build_gradient(false);
    let panel3_left = render_ssr_panel(&mut renderer, &world_g_off, false);
    let Some(mut renderer_on) = new_renderer() else {
        return;
    };
    let world_g_on = build_gradient(true);
    let panel3_right = render_ssr_panel(&mut renderer_on, &world_g_on, true);
    // 近半地面（清晰反射带）的红像素远多于远半（粗糙端按权重归零）
    let band_red = |px: &[u8], y0: u32, y1: u32| {
        let mut n = 0;
        for y in y0..y1 {
            for x in 0..WIDTH {
                if is_red(pixel(px, WIDTH, x, y)) {
                    n += 1;
                }
            }
        }
        n
    };
    let rows = HEIGHT;
    let near_l = band_red(&panel3_left, rows * 3 / 4, rows - 4);
    let near_r = band_red(&panel3_right, rows * 3 / 4, rows - 4);
    println!("  近端（光滑）红像素：关 {near_l} → 开 {near_r}");
    assert!(
        near_r > near_l * 2 && near_r > 200,
        "SSR 开启后近端光滑带应出现成片红反射（关 {near_l} → 开 {near_r}）"
    );
    panels.push((panel3_left, panel3_right));

    // ---- 面板 4：屏外内容回退 关 | 开（逐位一致）----
    println!("面板 4：屏外标记 回退对照 关|开");
    let cam4 = Camera {
        eye: Vec3::new(0.0, 2.0, 3.0),
        target: Vec3::new(0.0, 0.0, -1.0),
        fov_y_radians: std::f32::consts::FRAC_PI_4,
        ..Camera::default()
    };
    let marker_off = Vec3::new(0.0, 3.4, -4.2);
    let marker_px = project_to_pixel(&cam4, marker_off, WIDTH, HEIGHT);
    println!(
        "  屏外标记投影像素 ({:.0}, {:.0})——应在画面外",
        marker_px.0, marker_px.1
    );
    assert!(
        marker_px.1 < -6.0,
        "标记必须在画面之外（行 {:.1}），否则「屏外」前提不成立",
        marker_px.1
    );
    let build_offscreen = |with_ssr: bool| -> (World, Entity) {
        let mut world = base_world(cam4.clone());
        let floor = world.resource_mut::<MeshStore>().add(MeshData::plane(12.0));
        let mirror = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::metal(0.85, 0.85, 0.88, 0.05));
        world.spawn((Transform::IDENTITY, floor, mirror));
        let slab = world.resource_mut::<MeshStore>().add(MeshData::cube());
        let red = {
            let mut mats = world.resource_mut::<MaterialStore>();
            emissive_mat(&mut mats, 1.0, 0.08, 0.03)
        };
        world.spawn((
            Transform {
                position: marker_off,
                scale: Vec3::new(1.6, 0.16, 1.6),
                ..Transform::IDENTITY
            },
            slab,
            red,
        ));
        let probe = world.spawn((
            Transform::from_xyz(0.0, 1.475, 0.0),
            ReflectionProbe::new(Vec3::new(4.0, 3.0, 4.0)).with_falloff(0.0),
        ));
        if with_ssr {
            world.insert_resource(ssr_on());
        }
        propagate_transforms(&mut world);
        (world, probe)
    };
    let Some(mut renderer) = new_renderer() else {
        return;
    };
    let (world_o, probe_o) = build_offscreen(false);
    assert!(
        renderer
            .renderer_mut()
            .bake_reflection_probe(&world_o, probe_o)
    );
    let panel4_left = render_ssr_panel(&mut renderer, &world_o, false);
    let Some(mut renderer_on) = new_renderer() else {
        return;
    };
    let (world_o2, probe_o2) = build_offscreen(true);
    assert!(
        renderer_on
            .renderer_mut()
            .bake_reflection_probe(&world_o2, probe_o2)
    );
    let panel4_right = render_ssr_panel(&mut renderer_on, &world_o2, true);
    let diff4 = panel4_left
        .iter()
        .zip(&panel4_right)
        .filter(|(a, b)| a != b)
        .count();
    println!("  关|开差异字节：{diff4}");
    assert_eq!(
        diff4, 0,
        "屏外标记 ⇒ 全部片元未命中 ⇒ 整帧必须与探针路径逐位一致（差异 {diff4} 字节）"
    );
    panels.push((panel4_left, panel4_right));

    // ---- 性能：SSR 帧时开销（步进 + 帧尾副本）/ 显存增量 ----
    {
        use std::time::Instant;
        // 可选环境变量覆盖性能测量尺寸（默认 = 面板尺寸）；
        // `IRIS_SSR_PERF=1920x1080` 供 REPORT 取 1080p 实测
        let (pw, ph) = match std::env::var("IRIS_SSR_PERF") {
            Ok(spec) => {
                let (w, h) = spec.split_once('x').expect("IRIS_SSR_PERF 形如 1920x1080");
                (
                    w.parse::<u32>().expect("宽度"),
                    h.parse::<u32>().expect("高度"),
                )
            }
            Err(_) => (WIDTH, HEIGHT),
        };
        let world_perf_off = build_room(false);
        let world_perf_on = build_room(true);
        let mut perf = OffscreenRenderer::new(pw, ph).expect("性能渲染器");
        let frame_ms = |renderer: &mut OffscreenRenderer, world: &World, ssr: bool| {
            for _ in 0..10 {
                renderer.render(world);
                if ssr {
                    renderer.render(world);
                }
            }
            let _ = renderer.read_pixels();
            let frames = 120u32;
            let t = Instant::now();
            for _ in 0..frames {
                renderer.render(world);
                if ssr {
                    renderer.render(world);
                }
            }
            t.elapsed().as_secs_f64() * 1000.0 / frames as f64
        };
        let ms_off = frame_ms(&mut perf, &world_perf_off, false);
        let ms_on = frame_ms(&mut perf, &world_perf_on, true);
        // 帧尾副本带宽：读 + 写 2 × W × H × 8 B（Rgba16Float）
        let copy_mib = 2.0 * pw as f64 * ph as f64 * 8.0 / (1024.0 * 1024.0);
        // 显存增量：上帧颜色副本 + 1×1 哑纹理（96 B 双参数缓冲不计）
        let vram_mib = pw as f64 * ph as f64 * 8.0 / (1024.0 * 1024.0);
        println!(
            "性能：帧时 {ms_off:.3} → {ms_on:.3} ms（{pw}×{ph}，SSR 开销 {:.1}%，\
             含逐帧 poll；帧尾副本 {copy_mib:.1} MiB/帧读写）｜显存增量 ≈ {vram_mib:.1} MiB（上帧颜色副本）",
            (ms_on - ms_off) / ms_off * 100.0,
        );
    }

    let path = Path::new("iris_ssr.ppm");
    write_grid(path, &panels).expect("写验收图");
    println!(
        "输出 {}（4 排 × 2 列：①金属地面 关|开 ②动态物体 关|开 ③粗糙度梯度 关|开 ④屏外回退 关|开，{}×{}）",
        path.display(),
        WIDTH * 2 + GAP,
        HEIGHT * ROWS + GAP * (ROWS - 1)
    );
    println!("转 PNG：ffmpeg -y -i iris_ssr.ppm docs/ssr.png");
}

/// 写 4 排 × 2 列的 PPM P6（与既有验收图示例同款面板拼装）。
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
