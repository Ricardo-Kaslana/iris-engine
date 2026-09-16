//! GI 探针三阶 SH 逐片元化验收图（R83）：两组面板对照，输出
//! `iris_gi_sh.ppm` → `docs/gi_sh.png`。
//!
//! 面板布局（2 排 × 2 列，每排「v1 逐实体 | SH 逐片元」左右并排）：
//! 1. **空间梯度（§8.40①）**：红地板 + 绿天花板的房间，一根 3.2 m 高的
//!    圆柱（角色 / 大体积动态物体的替身）横跨两层探针——左格是 v1 的
//!    逐实体取值（整根柱子同一份辐照，头脚无差），右格是逐片元 SH
//!    （柱脚偏红、柱头偏绿，中段连续过渡）；
//! 2. **方向性（§8.40②）**：暗地板 + 强亮天花板的房间，一颗球体，四个
//!    探针**同高**（空间上几乎无差异 ⇒ 信号只能来自方向）——左格 v1 的
//!    六轴平均把上下半球摊成同一个值（球面均匀），右格 SH 的 L1/L2 项让
//!    球顶（法线朝亮天花板）显著亮于球底（法线朝暗地板）。
//!
//! 两排都用**真实烘焙**（`bake_irradiance_probes_sh` vs `bake_irradiance_probes`，
//! 同一世界同一参数），不是手工构造的系数——验收图必须证明「烘焙 → 装配 →
//! 渲染」全链贯通。运行时直射光强度 0 ⇒ 画面里的照明全部来自间接项（GI
//! 探针），信号不被直射光淹没；烘焙参数里的方向光是独立的（第一排朝下照亮
//! 红地板、第二排朝上照亮天花板）。
//!
//! 每格带硬断言（跨模式同像素作差 ⇒ 镜面 / 天空 / 色调映射项抵消；不通过
//! 直接 panic、不写文件）；跨运行逐字节一致（同输入两次渲染的 FNV-1a 全帧
//! 哈希相同，本示例打印四格哈希供比对）。
//!
//! ```sh
//! cargo run -p iris-render --example gi_sh_snapshot --release
//! ffmpeg -y -i iris_gi_sh.ppm docs/gi_sh.png
//! ```

use std::path::Path;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
const GAP: u32 = 6;
/// 面板行数（每组一行，行内两格）。
const ROWS: u32 = 2;

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

fn sample_lum(px: &[u8], camera: &Camera, at: Vec3) -> i32 {
    let (fx, fy) = project_to_pixel(camera, at, WIDTH, HEIGHT);
    let (x, y) = (fx as u32, fy as u32);
    assert!(x < WIDTH && y < HEIGHT, "采样点 ({fx},{fy}) 越界");
    luminance(pixel(px, WIDTH, x, y))
}

fn sample_rgb(px: &[u8], camera: &Camera, at: Vec3) -> [i32; 3] {
    let (fx, fy) = project_to_pixel(camera, at, WIDTH, HEIGHT);
    let (x, y) = (fx as u32, fy as u32);
    assert!(x < WIDTH && y < HEIGHT, "采样点 ({fx},{fy}) 越界");
    pixel(px, WIDTH, x, y)
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

/// 公共世界骨架：相机 + 零直射光 + 近零环境常量 + 关泛光 ⇒ 画面里的照明
/// 几乎全部来自间接项（GI 探针），面板间的差异只由探针路径决定。
fn bare_world(camera: Camera) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(camera);
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.4, -1.0, -0.3).normalize(),
        intensity: 0.0,
        ambient: Vec3::new(0.012, 0.013, 0.016),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    world
}

fn solid(world: &mut World, r: f32, g: f32, b: f32, roughness: f32) -> MaterialHandle {
    let mut mat = MaterialDef::solid(r, g, b, 1.0);
    mat.roughness = roughness;
    mat.metallic = 0.0;
    world.resource_mut::<MaterialStore>().add(mat)
}

/// 一块轴对齐板（立方体缩放）。
fn slab(world: &mut World, mesh: MeshHandle, mat: MaterialHandle, center: Vec3, size: Vec3) {
    world.spawn((
        Transform {
            position: center,
            scale: size,
            ..Transform::IDENTITY
        },
        mesh,
        mat,
    ));
}

// ---------------------------------------------------------------------------
// 面板 1：空间梯度（红区 / 绿区 + 跨两区的高圆柱）
// ---------------------------------------------------------------------------

/// 第一排的相机（正对圆柱，略微偏高——柱脚与柱头都在画面内，视线从两块
/// 色板之间的缝隙穿过）。
fn gradient_camera() -> Camera {
    Camera {
        eye: Vec3::new(0.7, 2.3, 7.2),
        target: Vec3::new(0.0, 2.0, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_4,
        ..Camera::default()
    }
}

/// 圆柱的柱脚 / 中段 / 柱头采样点（正面 z = +0.52，法线近 +Z）。
const COLUMN_BOTTOM: Vec3 = Vec3::new(0.0, 0.50, 0.52);
const COLUMN_MID: Vec3 = Vec3::new(0.0, 2.00, 0.52);
const COLUMN_TOP: Vec3 = Vec3::new(0.0, 3.50, 0.52);

/// 红区 / 绿区房间 + 4 m 高圆柱 + 两层探针（**未烘焙**）。
///
/// **场景设计的要点 = 余弦权重**：圆柱正面（法线 +Z）的 `E(n)` 只对 **+Z
/// 半球**积分（`(ω·n)⁺`），所以「地板红 / 天花板绿」那种上下分色对正面几乎
/// 无信号（水平面与 +Z 正交，cos ≈ 0）。要让柱脚与柱头吃到不同颜色，异色
/// 表面必须**在圆柱前方**：于是用两块竖直色板（下层红 / 上层绿，x ∈ ±[0.9,
/// 3.2]、z = 1.5）夹住圆柱，中间留 1.8 m 的缝给相机视线。色板距探针仅
/// 0.7 m ⇒ 张角极大 ⇒ 下层探针的 SH 近乎纯红、上层近乎纯绿。
///
/// 探针 8 个（每层 4 个：前 2 后 2）——前后错开是为了四面体化非共面（v1
/// 对照格需要合法四面体）；后面的两个探针距柱面 > 半径 ⇒ 不参与正面混合
/// （自动半径 = 层内最近邻 1.2 × 1.5 = 1.8，恰好把「同层前排 / 异层 /
/// 同层后排」都切开——**本排不用半径覆盖组件**，纯自动派生）。
fn gradient_world() -> World {
    let mut world = bare_world(gradient_camera());
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let red = solid(&mut world, 0.95, 0.10, 0.08, 0.9);
    let green = solid(&mut world, 0.10, 0.92, 0.25, 0.9);
    let dark = solid(&mut world, 0.18, 0.18, 0.20, 0.95);
    let neutral = solid(&mut world, 0.32, 0.32, 0.35, 0.95);
    // 地板 / 背墙（暗中性，只给画面纵深，不参与色相）
    slab(
        &mut world,
        cube,
        dark,
        Vec3::new(0.0, -0.1, 0.0),
        Vec3::new(20.0, 0.2, 20.0),
    );
    slab(
        &mut world,
        cube,
        neutral,
        Vec3::new(0.0, 2.5, -3.5),
        Vec3::new(14.0, 5.0, 0.2),
    );
    // 两块竖直色板（下层红 y ∈ [0, 2.2]、上层绿 y ∈ [2.2, 4.4]），各分左右
    for &sx in &[-1.0f32, 1.0] {
        slab(
            &mut world,
            cube,
            red,
            Vec3::new(sx * 2.05, 1.1, 1.5),
            Vec3::new(2.3, 2.2, 0.2),
        );
        slab(
            &mut world,
            cube,
            green,
            Vec3::new(sx * 2.05, 3.3, 1.5),
            Vec3::new(2.3, 2.2, 0.2),
        );
    }
    // 高圆柱（动态物体：不进烘焙 ⇒ 吃探针 GI），y ∈ [0, 4.0]
    let column = world
        .resource_mut::<MeshStore>()
        .add(MeshData::cylinder(0.5, 4.0, 40));
    let skin = solid(&mut world, 0.86, 0.84, 0.80, 0.85);
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 2.0, 0.0),
            ..Transform::IDENTITY
        },
        column,
        skin,
    ));
    // 两层探针（每层前 2 后 2；半径走自动派生）
    for &y in &[1.1f32, 3.3] {
        for &x in &[-0.6f32, 0.6] {
            for &z in &[0.8f32, -1.4] {
                world.spawn((Transform::from_xyz(x, y, z), IrradianceProbe));
            }
        }
    }
    propagate_transforms(&mut world);
    world
}

/// 第一排的烘焙参数：光斜向上朝 **−Z** 传来（`to_light = (0, 0.7, −0.7)`）⇒
/// 两块色板的朝柱面（法线 −Z）吃到 `NdotL ≈ 0.7`，而阴影射线向上走 ⇒ 不被
/// 地板 / 圆柱挡、也从 4 m 高的背墙**上方**掠过（背墙若高过 y ≈ 5 会把色板
/// 整片遮黑——第一版就栽在这里：色板只剩天空项，色相分离只有 1/3）。
/// 天空压暗（0.12）让逃逸方向不稀释色相。
fn gradient_bake_params() -> BakeParams {
    BakeParams {
        light_dir: Vec3::new(0.0, -0.7, 0.71).normalize(),
        light_color: Vec3::new(1.0, 0.96, 0.92) * 6.5,
        sky_color: Vec3::splat(0.12),
        shadow_softness: 0.0,
        sky_samples: 48,
        bounces: 1,
    }
}

// ---------------------------------------------------------------------------
// 面板 2：方向性（暗地板 / 强亮天花板 + 球体，四探针同高）
// ---------------------------------------------------------------------------

fn directional_camera() -> Camera {
    Camera {
        // 相机在**室内**（+Z 墙身后）：房间只有四面墙 + 带天窗的天花板，
        // 从外面看只能看到墙背
        eye: Vec3::new(0.0, 1.6, 2.8),
        target: Vec3::new(0.0, 1.4, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    }
}

const SPHERE_CENTER: Vec3 = Vec3::new(0.0, 1.4, 0.0);

/// 球面上绕赤道镜像对称的两个采样点（⇒ 镜面项 / NdotV 对称，唯一差异是
/// 法线的 y 分量：上点 n_y = +sin45°、下点 n_y = −sin45°）。纬度取 45° 是
/// **可见性上界**：再靠极的点背对相机被剔除，那个像素其实是射线在球面前侧
/// 的入口点（CPU 对照接不上）。
fn sphere_samples() -> (Vec3, Vec3) {
    let tilt = 45.0f32.to_radians();
    let offset = Vec3::new(0.0, 1.0 * tilt.sin(), 1.0 * tilt.cos());
    (SPHERE_CENTER + offset, SPHERE_CENTER - offset)
}

/// **天窗房间** + 球体 + 同高四探针（空间上几乎无差异 ⇒ 面板间的差异只能
/// 来自方向性）。
///
/// 为什么是天窗而不是「亮天花板」：天花板被球体看见的是它的**下表面**，而
/// 下表面只能被「从下往上传播的光」照亮——那条阴影射线必然被地板挡住 ⇒
/// 烘出来永远是暗的（第一版栽在这里：CPU 侧 3:1 的方向性、GPU 上只剩镜面
/// 项的 11 级差）。天窗绕开了这个死结：**未遮挡方向的辐射率 = `sky_color`**，
/// 不需要任何光源，也不需要可见性技巧。
fn directional_world() -> World {
    let mut world = bare_world(directional_camera());
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let floor_mat = solid(&mut world, 0.05, 0.05, 0.06, 0.95);
    let wall_mat = solid(&mut world, 0.26, 0.26, 0.29, 0.95);
    // 暗地板 / 四面暗墙（房间内壁 ⇒ 水平方向都是暗的）
    slab(
        &mut world,
        cube,
        floor_mat,
        Vec3::new(0.0, -0.1, 0.0),
        Vec3::new(12.4, 0.2, 12.4),
    );
    for &sx in &[-1.0f32, 1.0] {
        slab(
            &mut world,
            cube,
            wall_mat,
            Vec3::new(sx * 3.1, 1.6, 0.0),
            Vec3::new(0.2, 3.4, 12.4),
        );
        slab(
            &mut world,
            cube,
            wall_mat,
            Vec3::new(0.0, 1.6, sx * 3.1),
            Vec3::new(12.4, 3.4, 0.2),
        );
    }
    // 带 4×4 天窗的天花板（四块拼出中间的洞）
    for &sx in &[-1.0f32, 1.0] {
        slab(
            &mut world,
            cube,
            wall_mat,
            Vec3::new(sx * 4.1, 3.3, 0.0),
            Vec3::new(4.2, 0.2, 12.4),
        );
    }
    for &sz in &[-1.0f32, 1.0] {
        slab(
            &mut world,
            cube,
            wall_mat,
            Vec3::new(0.0, 3.3, sz * 4.1),
            Vec3::new(4.0, 0.2, 4.2),
        );
    }
    // 球体（动态物体：不进烘焙 ⇒ 吃探针 GI）
    let sphere = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, 64, 40));
    let skin = solid(&mut world, 0.86, 0.84, 0.80, 0.9);
    world.spawn((
        Transform {
            position: SPHERE_CENTER,
            ..Transform::IDENTITY
        },
        sphere,
        skin,
    ));
    // 同高四探针（自动半径 = 最近邻 2.4 × 1.5 = 3.6，覆盖整颗球）
    for &x in &[-1.2f32, 1.2] {
        for &z in &[-1.2f32, 1.2] {
            world.spawn((Transform::from_xyz(x, 1.4, z), IrradianceProbe));
        }
    }
    propagate_transforms(&mut world);
    world
}

/// 第二排的烘焙参数：**天空驱动**（`sky_color = 4.0` 是天窗的亮度来源；
/// 1.45 是实测的甜点：太低（≤ 1）时程序化天空的 env 镜面项（预滤波辐射率
/// 很高，粗糙度 0.9 也有百级）会把 GI 信号淹成 11 级差；太高（4.0）时球面
/// 整体冲进 ACES 的高光压缩段（上下都贴到 94% 亮度），差值同样被压回 46 级），
/// 方向光强度归零（房间封闭，任何方向的光都会被墙 / 地板挡成阴影，留着只是
/// 噪声）。运行时直射光同样为 0 ⇒ 画面里的照明全部来自间接项。
fn directional_bake_params() -> BakeParams {
    BakeParams {
        light_dir: Vec3::new(-0.4, -1.0, 0.2).normalize(),
        light_color: Vec3::ZERO,
        sky_color: Vec3::splat(1.45),
        shadow_softness: 0.0,
        sky_samples: 48,
        bounces: 1,
    }
}

/// 渲染一个世界并取回像素——**每个世界一个全新渲染器**。
///
/// 渲染器按**句柄值**缓存网格 / 材质的 GPU 资源（AGENTS.md 的已知陷阱：
/// 「同一渲染器上渲染同句柄不同内容的多个世界会串味」）。本示例的两排场景
/// 是两套完全不同的几何 / 材质，却都从 `MeshHandle(0)` / `MaterialHandle(0)`
/// 起编号 ⇒ 复用渲染器会让第二排画出第一排的缓存内容（实测症状：球体整片
/// 变成平坦偏蓝的 161 级，方向性信号归零）。跨世界对照必须各用独立渲染器。
fn render_world(world: &World) -> Vec<u8> {
    let mut renderer =
        OffscreenRenderer::new(WIDTH, HEIGHT).expect("离屏渲染器（每世界独立，见函数文档）");
    renderer.render(world);
    let px = renderer.read_pixels();
    assert_eq!(px.len(), (WIDTH * HEIGHT * 4) as usize);
    px
}

/// 烘 v1 集（无 SH ⇒ mode = 0，逐实体）或 SH 集（mode = 1，逐片元）。
fn bake_into(mut world: World, params: &BakeParams, with_sh: bool) -> World {
    let set = if with_sh {
        bake_irradiance_probes_sh(&world, params)
    } else {
        bake_irradiance_probes(&world, params)
    };
    assert_eq!(set.has_sh(), with_sh, "烘焙产物的 SH 形态必须与入口一致");
    install_irradiance_probes(&mut world, set);
    world
}

fn main() {
    if OffscreenRenderer::new(64, 64).is_none() {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    }
    let mut panel =
        vec![0u8; ((WIDTH * 2 + GAP) * (HEIGHT * ROWS + GAP * (ROWS - 1))) as usize * 4];
    let row_y = |r: u32| r * (HEIGHT + GAP);

    // ---- 面板 1：空间梯度 v1 | SH ----
    let cam1 = gradient_camera();
    let px_v1 = render_world(&bake_into(gradient_world(), &gradient_bake_params(), false));
    let px_sh = render_world(&bake_into(gradient_world(), &gradient_bake_params(), true));
    blit(&mut panel, &px_v1, 0, row_y(0));
    blit(&mut panel, &px_sh, WIDTH + GAP, row_y(0));

    let hue = |p: [i32; 3]| p[0] - p[1];
    let (bot_v1, mid_v1, top_v1) = (
        sample_rgb(&px_v1, &cam1, COLUMN_BOTTOM),
        sample_rgb(&px_v1, &cam1, COLUMN_MID),
        sample_rgb(&px_v1, &cam1, COLUMN_TOP),
    );
    let (bot_sh, mid_sh, top_sh) = (
        sample_rgb(&px_sh, &cam1, COLUMN_BOTTOM),
        sample_rgb(&px_sh, &cam1, COLUMN_MID),
        sample_rgb(&px_sh, &cam1, COLUMN_TOP),
    );
    println!("面板1 v1：柱脚 {bot_v1:?} 中段 {mid_v1:?} 柱头 {top_v1:?}");
    println!("面板1 SH：柱脚 {bot_sh:?} 中段 {mid_sh:?} 柱头 {top_sh:?}");
    // v1：整根柱子同一份辐照 ⇒ 上下的色相指标接近
    assert!(
        (hue(top_v1) - hue(bot_v1)).abs() < 24,
        "v1 逐实体：柱脚与柱头必须同色（无空间梯度）：{bot_v1:?} vs {top_v1:?}"
    );
    // SH：柱脚偏红、柱头偏绿，且中段落在两者之间（连续过渡，不是硬切）
    assert!(
        hue(bot_sh) > hue(top_sh) + 60,
        "SH 逐片元：柱脚必须比柱头显著偏红：{bot_sh:?} vs {top_sh:?}"
    );
    assert!(
        hue(bot_sh) > hue(mid_sh) && hue(mid_sh) > hue(top_sh),
        "SH 逐片元：中段必须单调落在柱脚与柱头之间（连续梯度）：{bot_sh:?} / {mid_sh:?} / {top_sh:?}"
    );
    // 跨模式作差（镜面 / 天空 / 色调映射抵消）：柱脚往红走、柱头往绿走
    let d_bot = hue(bot_sh) - hue(bot_v1);
    let d_top = hue(top_sh) - hue(top_v1);
    println!("面板1 跨模式差：柱脚 {d_bot}，柱头 {d_top}");
    assert!(d_bot > 30, "柱脚必须因逐片元 SH 显著变红：{d_bot}");
    assert!(d_top < -30, "柱头必须因逐片元 SH 显著变绿：{d_top}");

    // ---- 面板 2：方向性 v1 | SH ----
    let cam2 = directional_camera();
    let (up_point, down_point) = sphere_samples();
    let px_dv1 = render_world(&bake_into(
        directional_world(),
        &directional_bake_params(),
        false,
    ));
    let dir_sh = bake_into(directional_world(), &directional_bake_params(), true);
    let px_dsh = render_world(&dir_sh);
    blit(&mut panel, &px_dv1, 0, row_y(1));
    blit(&mut panel, &px_dsh, WIDTH + GAP, row_y(1));

    let (u_v1, d_v1) = (
        sample_lum(&px_dv1, &cam2, up_point),
        sample_lum(&px_dv1, &cam2, down_point),
    );
    let (u_sh, d_sh) = (
        sample_lum(&px_dsh, &cam2, up_point),
        sample_lum(&px_dsh, &cam2, down_point),
    );
    println!("面板2 v1：球顶 {u_v1} 球底 {d_v1}（差 {}）", u_v1 - d_v1);
    println!("面板2 SH：球顶 {u_sh} 球底 {d_sh}（差 {}）", u_sh - d_sh);
    // CPU 对照：同一份系数在 CPU 侧求值也必须上下分离（GPU/CPU 双轨互证）
    let set = dir_sh.resource::<IrradianceProbeSet>();
    let tilt = 45.0f32.to_radians();
    let n_up = Vec3::new(0.0, tilt.sin(), tilt.cos());
    let cpu_up = set.sh_irradiance_at(up_point, n_up).unwrap();
    let cpu_down = set.sh_irradiance_at(down_point, -n_up).unwrap();
    println!("面板2 CPU：球顶 {cpu_up:?} 球底 {cpu_down:?}");
    assert!(
        cpu_up.x > cpu_down.x * 1.5,
        "CPU 侧 SH 求值必须上亮下暗（烘焙出的方向性）：{cpu_up:?} vs {cpu_down:?}"
    );
    // v1：六轴平均摊平方向性 ⇒ 上下近乎同值
    assert!(
        (u_v1 - d_v1).abs() < 24,
        "v1 六轴平均：球顶与球底必须近乎同值（方向性被摊平）：{u_v1} vs {d_v1}"
    );
    // SH：球顶（法线朝天窗）显著亮于球底（法线朝暗地板）。阈值 60 的依据：
    // CPU 侧 E/π 上下比 ≈ 2.6（0.68 / 0.26），经 albedo × kd_amb 与 ACES 压缩
    // 到 8-bit 后实测差 100（635 vs 535），60 留出余量又远高于量化噪声。
    assert!(
        u_sh - d_sh > 60,
        "SH 逐片元：球顶必须显著亮于球底（同一探针内的方向性）：{u_sh} vs {d_sh}"
    );
    assert!(
        (u_sh - u_v1) > 24 && (d_sh - d_v1) < -24,
        "跨模式：球顶变亮、球底变暗（v1 的均值被 SH 拆成两端）：         顶 {} 底 {}",
        u_sh - u_v1,
        d_sh - d_v1
    );

    // ---- 跨运行确定性：两格 SH 画面二次渲染逐字节一致 ----
    assert_eq!(
        fnv1a(&render_world(&bake_into(
            gradient_world(),
            &gradient_bake_params(),
            true
        ))),
        fnv1a(&px_sh),
        "空间梯度格：两次独立会话必须逐位一致"
    );
    assert_eq!(
        fnv1a(&render_world(&bake_into(
            directional_world(),
            &directional_bake_params(),
            true
        ))),
        fnv1a(&px_dsh),
        "方向性格：两次独立会话必须逐位一致"
    );

    // ---- 写 PPM（供 ffmpeg 转 PNG 入 docs/）----
    let header = format!(
        "P6\n{} {}\n255\n",
        panel.len() as u32 / 4 / (HEIGHT * ROWS + GAP * (ROWS - 1)),
        HEIGHT * ROWS + GAP * (ROWS - 1)
    );
    let mut ppm = header.into_bytes();
    for chunk in panel.chunks_exact(4) {
        ppm.extend_from_slice(&chunk[..3]);
    }
    std::fs::write(Path::new("iris_gi_sh.ppm"), &ppm).expect("写 iris_gi_sh.ppm");
    println!("iris_gi_sh.ppm 已写出");
    println!("面板1 v1 FNV-1a = {:#018x}", fnv1a(&px_v1));
    println!("面板1 SH FNV-1a = {:#018x}", fnv1a(&px_sh));
    println!("面板2 v1 FNV-1a = {:#018x}", fnv1a(&px_dv1));
    println!("面板2 SH FNV-1a = {:#018x}", fnv1a(&px_dsh));
}
