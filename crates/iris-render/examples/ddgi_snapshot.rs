//! DDGI 动态全局光照验收图（R88）：2×2 面板对照，输出 `iris_ddgi.ppm` →
//! `docs/ddgi.png`。
//!
//! 场景：**一墙分隔亮 / 暗两室，门口是唯一连通**——40 m 见方地板 +
//! x = 0 的共享墙（门口 |z| < 0.75）+ 暗室（x ∈ [−8, 0]）的三面墙与天花板
//! 全封闭。太阳从东侧斜上方照进来 ⇒ 东侧亮场、暗室只有门口那一束。相机
//! 在暗室内、朝共享墙与门口方向看。
//!
//! 面板布局（2 排 × 2 列）：
//! 1. **DDGI 关**（左上）：`DdgiGrid` 缺席 ⇒ mode 回落 R83 裁决（这里 = 0）⇒
//!    间接漫反射吃 `irradiance_map` 的**无脑天空常量**——全封闭的暗室墙面
//!    照样被照成开阔天空的亮度，这就是 §8.52① 的病灶；
//! 2. **DDGI 开**（右上）：mode = 2，逐片元 SH × chebyshev 深度项 ⇒ 暗室
//!    墙面只剩门口那束光经反弹传进来的**梯度**（近门口亮、远端暗）；
//! 3. **深度项关（漏光对照）**（左下）：`set_ddgi_depth_weight_enabled(false)`
//!    ⇒ 混合退回 R83 的纯径向窗，墙东侧的亮探针**穿墙串味**把暗室近墙墙面
//!    照亮（与面板 2 同世界同渲染参数，唯一差别是这个开关 ⇒ 像素差只能
//!    来自 chebyshev 项）；
//! 4. **昼夜另一相位**（右下）：`DirectionalLight` 换成夜间档（太阳强度 0 +
//!    `NIGHT_AMBIENT` 暗蓝底光）⇒ 全表重照明跟随（`sky_color` 默认取
//!    `ambient`，裁定 3 的 R80 免费联动），暗室由暖白门口光变成暗蓝。
//!
//! 每格带硬断言（漏光抑制比 / 门口梯度 / 昼夜跟随；不通过直接 panic、不写
//! 文件）；跨运行逐字节一致（同输入两次渲染的 FNV-1a 全帧哈希相同，打印四
//! 格哈希供比对）。滚动重照明按**生产参数** k = 2 跑满 `⌈n/k⌉ + 1` 帧再渲染，
//! 并在 example 内断言已收敛（`dirty_count == 0`）。
//!
//! ```sh
//! cargo run -p iris-render --example ddgi_snapshot --release
//! ffmpeg -y -i iris_ddgi.ppm docs/ddgi.png
//! ```

use std::path::Path;

use glam::{Quat, Vec3};
use iris_core::daynight::{NIGHT_AMBIENT, ambient_envelope, sun_direction, sun_envelope};
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
const GAP: u32 = 6;

/// 面板 1–3 的昼间相位（太阳在东侧、低仰角 ⇒ 共享墙把暗室挡在阴影里）。
const DAY_PHASE: f64 = 0.12;
/// 面板 4 的夜间相位（太阳在地平线下 ⇒ 强度 0，只剩 NIGHT_AMBIENT 底光）。
const NIGHT_PHASE: f64 = 0.75;

/// 生产参数的滚动窗口宽度（默认值，`DdgiSettings::default`）⇒ 收敛帧数
/// `⌈100/2⌉ = 50`。
const PROBES_PER_FRAME: usize = 2;

/// 两个采样点都取在**共享墙的西侧面**（x = −0.15，法线 −X）：墙面法线与
/// 「指向太阳」的 +X 侧夹角 > 90° ⇒ `NdotL ≤ 0` ⇒ **直射项结构性为零**
/// （不依赖阴影贴图的覆盖范围），像素里除常量 ambient 外只剩间接漫反射
/// ⇒ 漏光对照的信号不被直射光淹没（地板 ROI 做不到这点，见 `room_camera`）。
/// `WALL_FAR` 远离门口（漏光归因点）、`WALL_NEAR_DOOR` 紧贴门口边缘
/// （梯度上端）。
///
/// **两点都必须在墙面上**：门口是 |z| < 0.75 的**洞**，取 z = 0 会采到「透过
/// 门洞看见的、被直射饱和的东侧场地」而不是暗室墙面——梯度与漏光两条断言
/// 都会量到错误的表面（R88 首稿的实际缺陷，验收轮修正）。
const WALL_FAR: Vec3 = Vec3::new(-0.15, 1.2, 4.5);
const WALL_NEAR_DOOR: Vec3 = Vec3::new(-0.15, 1.2, 0.9);

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

/// 采样点周围 `radius` 像素方窗的平均值（单点会被 MSAA 边缘与量化噪声支配）。
fn sample_roi(px: &[u8], camera: &Camera, at: Vec3, radius: i32) -> [i32; 3] {
    let (fx, fy) = project_to_pixel(camera, at, WIDTH, HEIGHT);
    let (cx, cy) = (fx.round() as i32, fy.round() as i32);
    let mut sum = [0i64; 3];
    let mut count = 0i64;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let (x, y) = (cx + dx, cy + dy);
            if x < 0 || y < 0 || x >= WIDTH as i32 || y >= HEIGHT as i32 {
                continue;
            }
            let p = pixel(px, WIDTH, x as u32, y as u32);
            for c in 0..3 {
                sum[c] += i64::from(p[c]);
            }
            count += 1;
        }
    }
    assert!(count > 0, "采样点 ({fx},{fy}) 完全越界");
    [
        (sum[0] / count) as i32,
        (sum[1] / count) as i32,
        (sum[2] / count) as i32,
    ]
}

fn roi_lum(px: &[u8], camera: &Camera, at: Vec3) -> i32 {
    luminance(sample_roi(px, camera, at, 4))
}

/// 相机在暗室内（天花板 y ∈ [4, 4.3]），朝共享墙与门口方向看、**略微仰视**：
/// 门口（画面中部）与近墙墙面（画面中部偏右）同时入画 ⇒ 漏光对照与门口
/// 梯度在同一格里可判读。
///
/// 仰视的理由：阴影 pass 复用「主相机视锥剔除后的不透明实例区间」⇒ 视锥外
/// 的天花板不进阴影贴图，暗室地板会被当作开阔地直射照亮。抬高眼点 + 仰视
/// 让天花板落进视锥 ⇒ 级联拟合覆盖它 ⇒ 暗室真的暗。
fn room_camera() -> Camera {
    Camera {
        eye: Vec3::new(-6.5, 1.5, 1.0),
        target: Vec3::new(-0.5, 2.2, 1.0),
        fov_y_radians: 70.0f32.to_radians(),
        ..Camera::default()
    }
}

fn scaled(position: Vec3, scale: Vec3) -> Transform {
    Transform {
        position,
        rotation: Quat::IDENTITY,
        scale,
    }
}

fn rough(r: f32, g: f32, b: f32) -> MaterialDef {
    let mut def = MaterialDef::solid(r, g, b, 1.0);
    def.roughness = 1.0;
    def.metallic = 0.0;
    def
}

/// 相位 → `DirectionalLight`（与 `iris_engine::daynight::update_day_night`
/// 同源的四字段写入：direction = −sun、color/intensity = `sun_envelope`、
/// ambient = `ambient_envelope`）。DDGI 的 `sky_color` 默认取 ambient ⇒
/// 昼夜相位一变，全表重照明跟着变（R80 免费联动）。
fn light_at(phase: f64) -> DirectionalLight {
    let (color, intensity) = sun_envelope(phase);
    DirectionalLight {
        direction: -sun_direction(phase),
        color,
        // 强度 ×6（场景曝光补偿）：暗室的 GI 量级只有 ~0.02，而片元里叠着一层
        // 与 DDGI 无关的 env 镜面项（≈0.45 线性）——漏光的**绝对**幅度 ∝ 太阳
        // 强度，不拉高就压到 8-bit 量化级以下、验收图看不出对照。
        intensity: intensity * 6.0,
        ambient: ambient_envelope(phase),
        shadows: true,
        ..DirectionalLight::default()
    }
}

/// 场景世界：40 m 见方地板 + x = 0 的共享墙（门口 |z| < 0.75）+ 暗室的西墙 /
/// 南北墙 / 天花板（x ∈ [−8, 0]、z ∈ [−8, 8]、y ∈ [0, 4] 全封闭）。
fn scene_world(phase: f64) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(room_camera());
    world.insert_resource(light_at(phase));
    // 泛光关掉：东侧过曝会往暗室 ROI 溢光，污染漏光归因
    world.insert_resource(BloomSettings::disabled());

    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let floor_mat = world
        .resource_mut::<MaterialStore>()
        .add(rough(0.78, 0.76, 0.72));
    let wall_mat = world
        .resource_mut::<MaterialStore>()
        .add(rough(0.62, 0.6, 0.58));
    world.spawn((Transform::IDENTITY, plane, floor_mat));
    // 共享墙（x ∈ [−0.15, 0.15]）：两段，中间 |z| < 0.75 是门口
    for z in [-4.375f32, 4.375] {
        world.spawn((
            scaled(Vec3::new(0.0, 2.0, z), Vec3::new(0.3, 4.0, 7.25)),
            cube,
            wall_mat,
        ));
    }
    // 暗室的西墙（x = −8）与南北墙（z = ±8）
    world.spawn((
        scaled(Vec3::new(-8.0, 2.0, 0.0), Vec3::new(0.3, 4.0, 16.3)),
        cube,
        wall_mat,
    ));
    for z in [-8.0f32, 8.0] {
        world.spawn((
            scaled(Vec3::new(-4.0, 2.0, z), Vec3::new(8.3, 4.0, 0.3)),
            cube,
            wall_mat,
        ));
    }
    // 暗室天花板（y ∈ [4, 4.3]）：把直射光与天空彻底挡在外面
    world.spawn((
        scaled(Vec3::new(-4.0, 4.15, 0.0), Vec3::new(8.3, 0.3, 16.3)),
        cube,
        wall_mat,
    ));
    propagate_transforms(&mut world);
    world
}

/// DDGI 设置（**生产默认口径**：k = 2、`rays_per_face = 16`）。间距 1.6 m、
/// bounds 覆盖暗室与东侧近墙一带 ⇒ 10×1×10 = 100 探针（≤ 256 ⇒ 不抽稀）、
/// 影响半径 2.4 m、探针高 y = 1.3；x = ±0.8 两列紧贴共享墙两侧 ⇒ 穿墙串味
/// 有最大的径向权重（漏光对照最可判读）。
fn ddgi_settings() -> DdgiSettings {
    DdgiSettings {
        spacing: 1.6,
        bounds: Some((Vec3::new(-8.0, 0.5, -8.0), Vec3::new(8.0, 2.1, 8.0))),
        probes_per_frame: PROBES_PER_FRAME,
        rays_per_face: 16,
        bounces: 1,
        ..DdgiSettings::default()
    }
}

/// 与 `iris_engine::ddgi::ddgi_bake_params` 同口径的组装（iris-render 的
/// example 不依赖 iris-engine，故在此复刻；依据见 `iris_render::ddgi` 模块文档）。
fn bake_params(world: &World, settings: &DdgiSettings) -> BakeParams {
    let light = world
        .try_resource::<DirectionalLight>()
        .map(|l| l.clone())
        .unwrap_or_default();
    BakeParams {
        light_dir: light.direction,
        light_color: light.color * light.intensity,
        sky_color: settings.sky_color.unwrap_or(light.ambient),
        shadow_softness: 0.0,
        sky_samples: 1,
        bounces: settings.bounces,
    }
}

fn tick(world: &World, settings: &DdgiSettings) {
    let params = bake_params(world, settings);
    world.resource_mut::<DdgiGrid>().tick(settings, &params);
}

/// 装网格 + 按生产参数滚动到收敛（**warmup = ⌈n/k⌉ + 1 帧**，并在返回前
/// 断言已收敛——设计稿 §15.3 的硬要求：不许拿未收敛的表出验收图）。
fn install_and_converge(world: &mut World, settings: &DdgiSettings) -> usize {
    let params = bake_params(world, settings);
    let mut grid = DdgiGrid::new();
    grid.install(settings, &params, collect_bake_triangles(world));
    world.insert_resource(grid);
    let n = world
        .try_resource::<DdgiGrid>()
        .map(|g| g.probe_count())
        .unwrap();
    let frames = n.div_ceil(settings.probes_per_frame) + 1;
    for _ in 0..frames {
        tick(world, settings);
    }
    let stats = world.try_resource::<DdgiGrid>().map(|g| g.stats()).unwrap();
    assert_eq!(
        stats.dirty_count, 0,
        "warmup {frames} 帧后必须已收敛（n = {n}、k = {}），不许拿未收敛的表出验收图",
        settings.probes_per_frame
    );
    assert_eq!(stats.probe_count as usize, n);
    assert_eq!(stats.stride, 1, "100 探针 ≤ 256 ⇒ 不该抽稀");
    frames
}

/// 单格渲染（每格独立渲染器——渲染器按句柄值缓存 GPU 资源，跨世界对照
/// 必须各用独立渲染器，R53/R62 教训）。
fn render(world: &World, depth_weight: bool) -> Vec<u8> {
    let mut renderer = OffscreenRenderer::new(WIDTH, HEIGHT).expect("离屏渲染器");
    renderer
        .renderer_mut()
        .set_ddgi_depth_weight_enabled(depth_weight);
    renderer.render(world);
    let px = renderer.read_pixels();
    assert_eq!(px.len(), (WIDTH * HEIGHT * 4) as usize);
    px
}

fn blit(panel: &mut [u8], src: &[u8], x0: u32, y0: u32) {
    let stride = (WIDTH * 2 + GAP) as usize;
    for row in 0..HEIGHT as usize {
        let sy = row * WIDTH as usize * 4;
        let dy = ((y0 as usize + row) * stride + x0 as usize) * 4;
        let n = WIDTH as usize * 4;
        panel[dy..dy + n].copy_from_slice(&src[sy..sy + n]);
    }
}

fn main() {
    if OffscreenRenderer::new(64, 64).is_none() {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    }
    let camera = room_camera();
    let settings = ddgi_settings();
    let mut panel = vec![0u8; ((WIDTH * 2 + GAP) * (HEIGHT * 2 + GAP)) as usize * 4];
    let col_x = |c: u32| c * (WIDTH + GAP);
    let row_y = |r: u32| r * (HEIGHT + GAP);

    // ---- 面板 1：DDGI 关（env 天空常量兜底）----
    let off_world = scene_world(DAY_PHASE);
    let px_off = render(&off_world, true);
    blit(&mut panel, &px_off, col_x(0), row_y(0));

    // ---- 面板 2 / 3 / 4：同一个已收敛的世界，只翻开关与光照相位 ----
    let mut day = scene_world(DAY_PHASE);
    day.insert_resource(settings);
    let warmup = install_and_converge(&mut day, &settings);
    println!("warmup = {warmup} 帧（⌈100/{PROBES_PER_FRAME}⌉ + 1），已断言 dirty_count = 0");
    let px_on = render(&day, true);
    blit(&mut panel, &px_on, col_x(1), row_y(0));
    // 漏光对照：同世界、同渲染参数，只关 chebyshev 深度项
    let px_leak = render(&day, false);
    blit(&mut panel, &px_leak, col_x(0), row_y(1));
    // 昼夜另一相位：夜间档 ⇒ 全表重照明跟随（sky_color = ambient）
    let mut night = scene_world(NIGHT_PHASE);
    night.insert_resource(settings);
    install_and_converge(&mut night, &settings);
    let px_night = render(&night, true);
    blit(&mut panel, &px_night, col_x(1), row_y(1));

    let hashes = [
        fnv1a(&px_off),
        fnv1a(&px_on),
        fnv1a(&px_leak),
        fnv1a(&px_night),
    ];

    // ---- 断言 1：关 / 开可判别，且 DDGI 把「无脑天空常量」换成遮挡感知值 ----
    // **跨模式作差 ⇒ 镜面项 / 天空项 / 色调映射项抵消**（gi_sh_snapshot 同款
    // 技术）：两次渲染只有 `irradiance` 一项不同（同相机、同材质、同几何），
    // 故差值纯粹是间接漫反射项的差异——直接比亮度会被视角依赖的 env 镜面项污染。
    let far_off = roi_lum(&px_off, &camera, WALL_FAR);
    let far_on = roi_lum(&px_on, &camera, WALL_FAR);
    let near_off = roi_lum(&px_off, &camera, WALL_NEAR_DOOR);
    let near_on = roi_lum(&px_on, &camera, WALL_NEAR_DOOR);
    let d_far = far_off - far_on;
    let d_near = near_off - near_on;
    println!("墙面远端（z=4.5）：关 {far_off} → 开 {far_on}（漫反射差 {d_far}）");
    println!("墙面门口旁（z=0.9）：关 {near_off} → 开 {near_on}（漫反射差 {d_near}）");
    assert_ne!(hashes[0], hashes[1], "关 / 开两格必须可判别");
    assert!(
        d_far >= 60,
        "全封闭暗室的墙面必须显著暗于天空常量兜底（遮挡感知）：漫反射差 {d_far}（实测 118）"
    );
    assert!(far_on > 0, "暗室不得纯黑（门口的光经反弹进来了）：{far_on}");

    // ---- 断言 2：门口梯度（GI 的空间结构只能来自真实反弹）----
    // 同一面墙、同一法线、同一材质，唯一变量是「离门口的距离」⇒ 差异只能来自
    // 门口进来的光。仍用**跨模式作差**消掉常量项：远端被 DDGI 压得多、门口旁
    // 压得少（门口旁的真实 GI 本来就高）。
    println!(
        "梯度：远端压暗 {d_far} vs 门口旁压暗 {d_near}（差 {}）；开格亮度 门口旁 {near_on} vs 远端 {far_on}",
        d_far - d_near
    );
    assert!(
        d_far - d_near >= 20,
        "DDGI 必须给出门口梯度（远端压暗 {d_far} 显著大于门口旁压暗 {d_near}，实测差 36）：\
         门口旁 {near_on} vs 远端 {far_on}"
    );
    assert!(
        near_on > far_on,
        "开格必须直接呈现「离门口越近越亮」（同墙面、同法线 ⇒ 唯一变量是距离）：\
         门口旁 {near_on} vs 远端 {far_on}"
    );

    // ---- 断言 3：漏光抑制（头条）——同世界只翻深度项开关 ----
    let far_leak = roi_lum(&px_leak, &camera, WALL_FAR);
    let near_leak = roi_lum(&px_leak, &camera, WALL_NEAR_DOOR);
    let ratio_far = far_leak as f32 / far_on.max(1) as f32;
    let ratio_near = near_leak as f32 / near_on.max(1) as f32;
    println!(
        "漏光对照（深度项关）：远端 {far_leak}（比值 {ratio_far:.3}）、门口旁 {near_leak}（比值 {ratio_near:.3}）"
    );
    assert_ne!(hashes[1], hashes[2], "深度项开 / 关两格必须可判别");
    assert!(
        far_leak - far_on >= 80,
        "关掉深度项后墙东侧的亮探针必须把暗室近墙墙面照亮（漏光）：\
         {far_leak} vs {far_on}（差 {}，实测 117）",
        far_leak - far_on
    );
    // 比值是**辅判据**，且在本 example 里被刻意稀释：验收图走 `sun_envelope` /
    // `ambient_envelope` 的真实昼间档（面板 4 的昼夜联动要求如此）⇒ 片元里叠着
    // 一层与 DDGI 无关的常量 ambient + env 镜面项（roughness = 1 的最高 mip 预
    // 滤波天空），它们在两格里逐位相同 ⇒ 参与比值分母却不参与差值。未被稀释的
    // 漏光量级看 `tests/ddgi_render.rs`（天空底色压到近零 ⇒ CPU 线性辐照比
    // 7.9×、像素比 2.88×）；这里只要求方向正确 + 差值可判别。
    assert!(
        ratio_far >= 1.15,
        "漏光比值辅判据（实测 1.253，被常量 ambient + env 镜面项稀释）：{ratio_far:.3}"
    );
    assert!(
        ratio_far > ratio_near,
        "漏光必须在「远离门口的近墙处」最强（门口旁已有真实光带主导）：\
         远端 {ratio_far:.3} vs 门口旁 {ratio_near:.3}"
    );

    // ---- 断言 4：昼夜跟随（R80 联动）——夜间档整体压暗且偏蓝 ----
    let night_far = sample_roi(&px_night, &camera, WALL_FAR, 4);
    let day_far = sample_roi(&px_on, &camera, WALL_FAR, 4);
    println!("暗室远端 RGB：昼 {day_far:?} → 夜 {night_far:?}（NIGHT_AMBIENT {NIGHT_AMBIENT:?}）");
    assert_ne!(hashes[1], hashes[3], "昼 / 夜两格必须可判别");
    assert!(
        luminance(night_far) * 3 < luminance(day_far) * 2,
        "夜间相位的暗室必须显著更暗：夜 {night_far:?} vs 昼 {day_far:?}"
    );
    assert!(
        night_far[2] > night_far[0],
        "夜间相位必须偏蓝（NIGHT_AMBIENT 的 B > R，且 DDGI 的 sky_color 取 ambient）：\
         {night_far:?}"
    );
    // 归因护栏：夜间的「蓝减红」必须比昼间更靠蓝——昼间的门口光带是暖白
    // （太阳色 × 地板反照率）会往红拉，夜间只剩暗蓝底光 ⇒ 相对差是相位跟随
    // 的证据（绝对色相会被场景配色影响，不作断言）。
    assert!(
        night_far[2] - night_far[0] > day_far[2] - day_far[0],
        "夜间必须比昼间更偏蓝（相位跟随）：夜 {night_far:?} vs 昼 {day_far:?}"
    );

    // ---- 断言 5：跨运行确定性——四格各自第二会话逐位一致 ----
    let off_b = render(&scene_world(DAY_PHASE), true);
    assert_eq!(fnv1a(&off_b), hashes[0], "面板 1 双会话必须逐位一致");
    let mut day_b = scene_world(DAY_PHASE);
    day_b.insert_resource(settings);
    install_and_converge(&mut day_b, &settings);
    assert_eq!(
        fnv1a(&render(&day_b, true)),
        hashes[1],
        "面板 2 双会话必须逐位一致"
    );
    assert_eq!(
        fnv1a(&render(&day_b, false)),
        hashes[2],
        "面板 3 双会话必须逐位一致"
    );
    let mut night_b = scene_world(NIGHT_PHASE);
    night_b.insert_resource(settings);
    install_and_converge(&mut night_b, &settings);
    assert_eq!(
        fnv1a(&render(&night_b, true)),
        hashes[3],
        "面板 4 双会话必须逐位一致"
    );

    // ---- 写 PPM（供 ffmpeg 转 PNG 入 docs/）----
    let header = format!("P6\n{} {}\n255\n", WIDTH * 2 + GAP, HEIGHT * 2 + GAP);
    let mut ppm = header.into_bytes();
    for chunk in panel.chunks_exact(4) {
        ppm.extend_from_slice(&chunk[..3]);
    }
    std::fs::write(Path::new("iris_ddgi.ppm"), &ppm).expect("写 iris_ddgi.ppm");
    println!("iris_ddgi.ppm 已写出（{} 字节）", ppm.len());
    for (name, h) in [
        "关（env 兜底）",
        "开（DDGI）",
        "深度项关（漏光对照）",
        "夜间相位",
    ]
    .iter()
    .zip(hashes)
    {
        println!("{name} FNV-1a = {h:#018x}");
    }
}
