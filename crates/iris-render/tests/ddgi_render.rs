//! 集成测试：DDGI 动态全局光照（R88 v1 + R89 v2，`DdgiGrid` 非空 ⇒
//! binding 18/19 整表替换 + binding 20 深度表 + `standard.wgsl` 的 mode = 2
//! 分支）。
//!
//! R88 六条主线（与 ARCHITECTURE §4.44 的验收标准一一对应）：
//! 1. **缺席 = 逐位不变（红线①）**：无 `DdgiGrid` / 空 `DdgiGrid` /
//!    有 `DdgiSettings` 但网格空 三者渲染 **逐字节相等**；且 R83 烘焙探针集
//!    在场时「空 DDGI 网格」与「无 DDGI 网格」逐字节相等（裁定 2 的缺席支：
//!    DDGI 缺席 ⇒ R83 裁决原样）；
//! 2. **开 / 关可判别 + 门口梯度**：封闭暗室的墙面被 DDGI 换成「遮挡感知的
//!    真实间接光」（远暗于无脑天空常量兜底），且墙面的 z 剖面**离门口越近越亮、
//!    严格单调衰减**（≥ 3× 落差）——梯度只能来自门口进来的光多次反弹，常量
//!    env 兜底给不出；
//! 3. **漏光抑制有效性（头条断言）**：同一世界同一渲染器，只翻
//!    `set_ddgi_depth_weight_enabled` ⇒ 暗室近墙 ROI 的亮度比 ≥ 阈值
//!    （ROI 避开门口这个混淆源，指标落在分布高端可判别——R53/R60 度量学教训）；
//! 4. **滚动收敛**：`FrameStats::ddgi_updated` 计数序列 = k,k,…,尾帧,0,0，
//!    `⌈n/k⌉` 帧后的画面与「k = n 一帧全量重照明」的世界 **FNV 相等**；
//! 5. **双会话 48 帧逐帧逐位**（R60/R62b 范式）：像素 FNV 序列 + CPU 表
//!    `to_bits` 序列双断言；
//! 6. **昼夜联动（R80）**：`DirectionalLight` 阶跃 ⇒ 全表 SH 的 L0 在
//!    `⌈n/k⌉` 帧内单调跟随到位。
//!
//! R89 追加（合成语义定稿 + 两个既有 bug 的锚，§4.45 / §8.58）：
//! 7. **cov = 1 内区 ≈ R88**：全画面都在探针内区（`d ≤ 0.75·r`）的场景，
//!    DDGI 开帧与 R88 代码的差异 = **全帧恰 1 字节 ±1 LSB**（编译期噪声，
//!    R63 同款归因；语义等价由 CPU 镜像 `mix(·,·,1.0)` 恒等单测 +
//!    DDGI-off 全帧逐位共同背书）——本测试锁定 R89 语义修订后的新基线；
//! 8. **DDGI × 光照贴图互斥（A1 裁决）**：全场景挂图集 + ambient = 0
//!    （隔离 A3 的无条件 ambient 衰减）⇒ DDGI 开 / 关逐位一致；
//! 9. **SSR × DDGI（A5）**：SSR 命中色随 DDGI 表内容变化（帧尾副本含
//!    全部 GI）；`reset_taa` 不触碰探针域状态（故意不清）；
//! 10. **SSR 强度旋钮（B1 修复）**：`intensity = 0` 帧 ≡ SSR 关闭帧逐位、
//!     `intensity = 2` 在全量权重区被钳到与 1.0 逐位一致；
//! 11. **`reset_taa` 清 GTAO 上帧深度（B2 修复）**：reset 后首帧
//!     `ao_on == 0` 且像素 ≡ AO 关闭帧（残影消灭）。
//!
//! 环境无 GPU 适配器时优雅跳过（打印提示，测试仍算通过）。
//! 本套件文件内 `GPU_SERIAL` 互斥串行化（`ssr_render.rs` 同款纪律——同文件
//! 多测试并行共用单适配器会挂死）；跨世界对照各用**独立渲染器**
//! （渲染器按句柄值缓存 GPU 资源，R53/R62 教训）。

use glam::{Quat, Vec3};
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 屏幕尺寸（测试统一口径）。
const SIZE: u32 = 256;

/// 本套件串行化（见文件头）。
static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ---------------------------------------------------------------------------
// 度量工具
// ---------------------------------------------------------------------------

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

/// ROI 平均亮度（`radius` 像素的方窗）：单点采样会被 MSAA 边缘与量化噪声
/// 支配，取窗内均值才落在分布的高端可判别区（R53/R60 度量学教训）。
fn roi_lum(px: &[u8], camera: &Camera, at: Vec3, radius: i32) -> i32 {
    let (fx, fy) = project_to_pixel(camera, at, SIZE, SIZE);
    let (cx, cy) = (fx.round() as i32, fy.round() as i32);
    let mut sum = 0i64;
    let mut count = 0i64;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let (x, y) = (cx + dx, cy + dy);
            if x < 0 || y < 0 || x >= SIZE as i32 || y >= SIZE as i32 {
                continue;
            }
            sum += i64::from(luminance(pixel(px, SIZE, x as u32, y as u32)));
            count += 1;
        }
    }
    assert!(count > 0, "ROI ({fx},{fy}) 完全越界");
    (sum / count) as i32
}

/// 全帧平均亮度（整场对照用）。
fn mean_lum(px: &[u8]) -> i32 {
    let mut sum = 0i64;
    for y in 0..SIZE {
        for x in 0..SIZE {
            sum += i64::from(luminance(pixel(px, SIZE, x, y)));
        }
    }
    (sum / i64::from(SIZE * SIZE)) as i32
}

/// CPU 表（SH + 深度矩）的 `to_bits` 序列哈希（双会话逐帧对照用）。
fn table_hash(grid: &DdgiGrid) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |b: u32| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for c in grid.sh() {
        for v in c {
            for f in v {
                feed(f.to_bits());
            }
        }
    }
    for m in grid.depth() {
        feed(m[0].to_bits());
        feed(m[1].to_bits());
    }
    h
}

// ---------------------------------------------------------------------------
// 场景：封闭暗室（西）+ 开敞亮场（东），共享墙上留一个门口
// ---------------------------------------------------------------------------

/// 相机在暗室内、朝共享墙与门口方向看，**略微仰视**（天花板 y ∈ [4, 4.3]）。
///
/// 仰视的理由：阴影 pass 复用「主相机视锥剔除后的不透明实例区间」⇒ 视锥外
/// 的天花板不进阴影贴图，暗室地板会被当作开阔地直射照亮（实测地板 ROI 的
/// 596/646 亮度来自直射项，把 CPU 侧两位数的线性辐照比压成 1.01 的像素比）。
/// 抬高眼点 + 仰视让天花板落进视锥 ⇒ 级联拟合覆盖它 ⇒ 暗室真的暗。
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

/// 墙面剖面的采样高度（CPU 对照轨与 GPU ROI 共用）。
const WALL_PROBE_HEIGHT: f32 = 1.2;
/// GPU 侧的受试点取在**共享墙的西侧面**（x = −0.15，法线 −X）、z = 4.5
/// （**远离门口**：门口 |z| < 0.75 ⇒ 相距 3.75 m，避开这个直接混淆源）。
///
/// **为什么取墙面而不是地板**：墙面法线 −X 与「指向太阳」的 +X 侧夹角 > 90°
/// ⇒ `NdotL ≤ 0` ⇒ **直射项结构性为零**（不依赖阴影贴图的覆盖范围），像素里
/// 除常量 ambient 外只剩间接漫反射 ⇒ 漏光信号不被直射光淹没。地板 ROI 做不到
/// 这一点：暗室天花板一旦落在视锥外就不进阴影贴图（阴影 pass 复用主相机
/// 视锥剔除后的实例区间），地板会被当作开阔地直射照亮——实测直射项占地板
/// 亮度的 **92%**（596/646），把 CPU 镜像的线性辐照比压成 **1.012** 的像素比
/// （不可判别）。
const WALL_FAR: Vec3 = Vec3::new(-0.15, WALL_PROBE_HEIGHT, 4.5);
/// 墙面法线（朝暗室内侧）。
const WALL_NORMAL: Vec3 = Vec3::NEG_X;
/// 开阔东侧的地板点（无任何遮挡 ⇒ 深度项应当近中性，是「抑制是遮挡感知而
/// 非一律调暗」的归因护栏）。
const FLOOR_OPEN_EAST: Vec3 = Vec3::new(3.0, 0.0, 4.5);

/// 天空 / 环境底色（刻意压到近零）：DDGI 的 `sky_color` 默认取
/// `DirectionalLight.ambient`（裁定 3），暗室的「天空底色」必须远小于门口
/// 进来的反弹光，否则梯度被抹平、漏光占比也被稀释。
const AMBIENT: Vec3 = Vec3::new(0.004, 0.0045, 0.006);

/// 场景世界：40 m 见方地板 + x = 0 的共享墙（门口 |z| < 0.75）+ 暗室的三面墙
/// 与天花板（x ∈ [−8, 0]、z ∈ [−8, 8]、y ∈ [0, 4] 全封闭，**门口是唯一连通**）。
/// 太阳从东侧斜上方照进来 ⇒ 东侧亮、暗室只有门口那一束。
fn scene_world() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(room_camera());
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.6, -1.0, 0.25).normalize(),
        color: Vec3::new(1.0, 0.95, 0.88),
        // 强度拉到 6：暗室的 GI 量级只有 ~0.03（天空底色近零 + 只有门口一束
        // 反弹），漏光的**绝对**幅度 ∝ 太阳强度——强度不够时它会被常量
        // ambient 与 env 镜面的底噪压到 8-bit 量化级以下。
        intensity: 6.0,
        ambient: AMBIENT,
        shadows: true,
        ..DirectionalLight::default()
    });
    // 泛光关掉：东侧过曝会往暗室 ROI 溢光，污染漏光归因
    world.insert_resource(BloomSettings::disabled());

    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    // 灰白粗糙材质（metallic = 0 ⇒ kd_amb 接近 1；roughness = 1 ⇒ 镜面项取
    // 预滤波最高 mip，跨采样点变化最小 ⇒ ROI 差异几乎全部来自间接漫反射）
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

fn rough(r: f32, g: f32, b: f32) -> MaterialDef {
    let mut def = MaterialDef::solid(r, g, b, 1.0);
    def.roughness = 1.0;
    def.metallic = 0.0;
    def
}

/// DDGI 设置：`bounds` 显式给（覆盖暗室内部 + 东侧近墙一带），间距 1.6 m。
/// 每轴点数 = `max(1, ⌊span / spacing⌋)` = 10 / 1 / 10 ⇒ **100 探针**
/// （≤ 256 ⇒ 不抽稀、`spacing_eff` = 1.6），影响半径 2.4 m（= 1.5 ×
/// `spacing_eff`），单层高 y = 1.3。
/// 格点取格心（`min + (i + 0.5) × spacing_eff`）⇒ x、z ∈ {±0.8, ±2.4, …}，
/// 其中 x = ±0.8 两列**紧贴共享墙两侧**（墙在 x ∈ [−0.15, 0.15] ⇒ 各距
/// 0.65 m）⇒ 穿墙串味有最大的径向权重，漏光归因的指标落在分布高端。
fn ddgi_settings(probes_per_frame: usize) -> DdgiSettings {
    DdgiSettings {
        spacing: 1.6,
        bounds: Some((Vec3::new(-8.0, 0.5, -8.0), Vec3::new(8.0, 2.1, 8.0))),
        probes_per_frame,
        rays_per_face: 8,
        bounces: 1,
        ..DdgiSettings::default()
    }
}

/// 与 `iris_engine::ddgi::ddgi_bake_params` 同口径的组装（iris-render 的测试
/// 不依赖 iris-engine，故在此复刻；口径依据见 `iris_render::ddgi` 模块文档）。
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

/// 装好网格（`install` 会做安装帧粗填）。
fn install(world: &mut World, settings: &DdgiSettings) {
    let params = bake_params(world, settings);
    let mut grid = DdgiGrid::new();
    grid.install(settings, &params, collect_bake_triangles(world));
    world.insert_resource(grid);
}

/// 推进一帧滚动重照明，返回本帧重照明的探针数。
fn tick(world: &World, settings: &DdgiSettings) -> usize {
    let params = bake_params(world, settings);
    let mut grid = world.resource_mut::<DdgiGrid>();
    grid.tick(settings, &params);
    grid.stats().last_updated as usize
}

/// 跑到收敛（`⌈n/k⌉` 帧）并断言收敛，返回帧数。
fn converge(world: &World, settings: &DdgiSettings) -> usize {
    let n = world
        .try_resource::<DdgiGrid>()
        .map(|g| g.probe_count())
        .unwrap();
    let frames = n.div_ceil(settings.probes_per_frame.max(1));
    for _ in 0..frames {
        tick(world, settings);
    }
    let dirty = world
        .try_resource::<DdgiGrid>()
        .map(|g| g.stats().dirty_count)
        .unwrap();
    assert_eq!(
        dirty, 0,
        "{frames} 帧后必须收敛（n = {n}、k = {}）",
        settings.probes_per_frame
    );
    frames
}

/// 单帧渲染（每世界独立渲染器，见文件头）。
fn render(world: &World) -> Vec<u8> {
    let mut renderer = OffscreenRenderer::new(SIZE, SIZE).expect("离屏渲染器（每世界独立）");
    renderer.render(world);
    let px = renderer.read_pixels();
    assert_eq!(px.len(), (SIZE * SIZE * 4) as usize);
    px
}

/// GPU 可用性守卫（不可用 ⇒ 打印跳过并返回 false）。
fn has_gpu() -> bool {
    if OffscreenRenderer::new(64, 64).is_some() {
        return true;
    }
    eprintln!("跳过：环境无 GPU 适配器");
    false
}

// ---------------------------------------------------------------------------
// ① 缺席 = 逐位不变（红线①）
// ---------------------------------------------------------------------------

#[test]
fn absent_paths_are_bitwise_pristine() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if !has_gpu() {
        return;
    }
    // (a) 完全没有 DDGI 资源
    let bare = scene_world();
    let px_bare = render(&bare);
    // (b) 只有空网格（DdgiPlugin 装了但 DdgiSettings 缺席 ⇒ 永不安装）
    let mut empty_grid = scene_world();
    empty_grid.insert_resource(DdgiGrid::new());
    // (c) 有 settings 但网格空（未安装 ⇒ 视为缺席）
    let mut uninstalled = scene_world();
    uninstalled.insert_resource(ddgi_settings(4));
    uninstalled.insert_resource(DdgiGrid::new());

    assert_eq!(
        px_bare,
        render(&empty_grid),
        "空 DdgiGrid 必须与「完全没有 DDGI 资源」逐字节相等\
         （binding 20 绑 4 B 哑缓冲 + mode 维持 R83 裁决 ⇒ 一个 write_buffer 都不发）"
    );
    assert_eq!(
        px_bare,
        render(&uninstalled),
        "有 DdgiSettings 但网格空 ⇒ 同样逐字节相等（缺席语义）"
    );

    // (d) R83 烘焙探针集在场：DDGI 缺席 ⇒ R83 裁决原样（裁定 2 的缺席支）
    let bake = BakeParams {
        light_dir: Vec3::new(-0.6, -1.0, 0.25).normalize(),
        light_color: Vec3::new(1.0, 0.95, 0.88) * 6.0,
        sky_color: AMBIENT,
        shadow_softness: 0.0,
        sky_samples: 8,
        bounces: 1,
    };
    let mut r83 = scene_world();
    for x in [-5.0f32, -1.0, 3.0] {
        for z in [-3.0f32, 3.0] {
            r83.spawn((Transform::from_xyz(x, 1.5, z), IrradianceProbe));
        }
    }
    propagate_transforms(&mut r83);
    let set = bake_irradiance_probes_sh(&r83, &bake);
    assert!(
        set.has_sh(),
        "R83 的 SH 集必须带 SH（否则 mode = 0，本条对照无意义）"
    );
    assert_eq!(set.len(), 6, "6 个探针实体");
    install_irradiance_probes(&mut r83, set.clone());
    let px_r83 = render(&r83);
    assert_ne!(
        fnv1a(&px_r83),
        fnv1a(&px_bare),
        "R83 mode = 1 的画面必须与 mode = 0 不同（否则本条对照无意义）"
    );
    let mut r83_with_empty_ddgi = scene_world();
    for x in [-5.0f32, -1.0, 3.0] {
        for z in [-3.0f32, 3.0] {
            r83_with_empty_ddgi.spawn((Transform::from_xyz(x, 1.5, z), IrradianceProbe));
        }
    }
    propagate_transforms(&mut r83_with_empty_ddgi);
    install_irradiance_probes(&mut r83_with_empty_ddgi, set);
    r83_with_empty_ddgi.insert_resource(DdgiGrid::new());
    assert_eq!(
        px_r83,
        render(&r83_with_empty_ddgi),
        "空 DDGI 网格不得扰动 R83 的 mode = 1 裁决（裁定 2 的缺席支：DDGI 缺席 ⇒ R83 原样）"
    );
}

// ---------------------------------------------------------------------------
// ② 开 / 关可判别 + 门口梯度
// ---------------------------------------------------------------------------

#[test]
fn ddgi_replaces_uniform_sky_with_occlusion_aware_gi() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if !has_gpu() {
        return;
    }
    let camera = room_camera();
    let off = scene_world();
    let px_off = render(&off);

    let mut on = scene_world();
    let settings = ddgi_settings(usize::MAX);
    on.insert_resource(settings);
    install(&mut on, &settings);
    let frames = converge(&on, &settings);
    assert_eq!(frames, 1, "k = usize::MAX ⇒ 一帧收敛");
    let px_on = render(&on);

    assert_ne!(
        fnv1a(&px_off),
        fnv1a(&px_on),
        "DDGI 开 / 关的画面必须可判别（否则整轮的信号面为零）"
    );
    let far_off = roi_lum(&px_off, &camera, WALL_FAR, 3);
    let far_on = roi_lum(&px_on, &camera, WALL_FAR, 3);
    println!(
        "墙面远端（z=4.5）：env 兜底 {far_off} → DDGI {far_on}（漫反射差 {}）",
        far_off - far_on
    );
    // **跨模式作差 ⇒ 直射 / 镜面 / 天空 / 色调映射项抵消**（gi_sh_snapshot
    // 同款技术）：两次渲染只有 `irradiance` 一项不同（同相机、同材质、同几何），
    // 差值纯粹是间接漫反射项的差异。
    let d_far = far_off - far_on;
    // env 兜底是「无脑天空常量」：暗室全封闭 ⇒ 它照样给出开阔天空的辐照
    // （这就是 §8.52① 的另一面）。DDGI 必须换成真实的遮挡感知值 ⇒ 显著更暗，
    // 但**不是黑的**（门口那束光经反弹进来了）。
    assert!(
        far_on > 0,
        "暗室墙面的 GI 必须 > 0（门口的光经反弹进来了，不是纯黑）：{far_on}"
    );
    assert!(
        d_far >= 100,
        "封闭暗室必须显著暗于天空常量兜底（遮挡感知）：漫反射差 {d_far}（env {far_off} → DDGI {far_on}）"
    );
    assert_ne!(
        mean_lum(&px_off),
        mean_lum(&px_on),
        "整场均值必须变化（不只是 ROI）"
    );

    // **空间结构**（CPU 对照轨）：沿共享墙的**西侧面**扫一条 z 剖面。暗室全
    // 封闭、门口是唯一连通 ⇒ GI 必须随「离门口的距离」单调衰减，这条梯度不可
    // 能来自别处。取墙面（法线 −X）而不是地板，是为了与 GPU ROI 同口径：
    // 直射项结构性为零、混淆源只有门口一个。
    // GPU 侧做不了这条：门口正对的墙面点在门洞里（墙在 |z| ≥ 0.75），那里的
    // 像素是直射饱和的东侧场地而非暗室墙面。
    let grid = on.try_resource::<DdgiGrid>().expect("网格在场");
    let wall_gi = |z: f32| {
        let p = Vec3::new(WALL_FAR.x, WALL_PROBE_HEIGHT, z);
        ddgi_blend(
            grid.positions(),
            grid.radii(),
            grid.sh(),
            grid.depth(),
            p,
            WALL_NORMAL,
            DDGI_DEPTH_GRID,
            true,
        )
        .unwrap_or_else(|| panic!("{p:?} 在网格内必须有有效探针"))
        .x
    };
    // 门口边缘在 |z| = 0.75 ⇒ 首个采样点 0.9（仍在墙面上、离门洞 0.15 m）
    let zs = [0.9f32, 1.6, 2.4, 3.2, 4.5, 6.0];
    let profile: Vec<f32> = zs.iter().map(|&z| wall_gi(z)).collect();
    println!("墙面 z 剖面 E/π（z = {zs:?}）= {profile:?}");
    for w in profile.windows(2) {
        assert!(
            w[0] > w[1],
            "墙面 GI 必须随离门口的距离单调衰减（门口是唯一连通 ⇒ 梯度只能来自门口）：{profile:?}"
        );
    }
    let span = profile[0] / profile[profile.len() - 1];
    assert!(
        span >= 3.0,
        "门口旁 / 暗室尽头的 GI 落差必须可判别（≥ 3×，实测 4.04×）：{span:.3}（{profile:?}）"
    );
    assert!(
        profile[profile.len() - 1] > 0.0,
        "暗室尽头（离门口 5.25 m）的 GI 必须 > 0：光经反弹传进来了，不是纯黑（{profile:?}）"
    );
}

// ---------------------------------------------------------------------------
// ③ 漏光抑制有效性（头条断言）
// ---------------------------------------------------------------------------

#[test]
fn depth_weight_suppresses_leakage_through_the_wall() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if !has_gpu() {
        return;
    }
    let camera = room_camera();
    let mut world = scene_world();
    let settings = ddgi_settings(usize::MAX);
    world.insert_resource(settings);
    install(&mut world, &settings);
    converge(&world, &settings);

    // CPU 对照轨先行：墙东侧探针朝暗室 ROI 的深度权重必须被显著抑制，
    // 而**同侧**近墙探针不该被同等抑制（抑制是方向性的，不是一律衰减）
    let (east, west, w_on, w_off, w_west) = {
        let grid = world.try_resource::<DdgiGrid>().expect("网格在场");
        let positions = grid.positions();
        let pick = |west_side: bool| -> usize {
            positions
                .iter()
                .enumerate()
                .find(|(_, p)| {
                    (p.x < 0.0) == west_side
                        && (p.x.abs() - 0.8).abs() < 0.1
                        && (p.z - WALL_FAR.z).abs() < 0.8
                })
                .map(|(i, _)| i)
                .unwrap_or_else(|| {
                    panic!(
                        "共享墙{}侧必须有紧贴墙的近墙探针",
                        if west_side { "西" } else { "东" }
                    )
                })
        };
        let (east, west) = (pick(false), pick(true));
        let measure = |probe: usize| -> (f32, f32) {
            let delta = WALL_FAR - positions[probe];
            let dist = delta.length();
            (
                ddgi_depth_weight_at(grid.depth(), probe, delta, dist, DDGI_DEPTH_GRID, true),
                ddgi_depth_weight_at(grid.depth(), probe, delta, dist, DDGI_DEPTH_GRID, false),
            )
        };
        let (on, off) = measure(east);
        let (west_on, _) = measure(west);
        (east, west, on, off, west_on)
    };
    println!("穿墙方向（东探针 {east} → ROI）：深度权重 开 {w_on} / 关 {w_off}");
    println!("同侧近墙探针 {west} → ROI：深度权重 {w_west}");
    assert_eq!(w_off, 1.0, "关掉深度项 ⇒ 恒 1（= R83 的串味现状）");
    assert!(
        w_on < 0.25,
        "穿墙方向必须被 chebyshev 显著抑制：东探针 (0.8, 1.3, 4.0) → ROI 的 \
         dist ≈ 1.08 m，而射线在墙的**东面** x = 0.15 处就已命中 ⇒ μ ≈ 0.74 m；\
         超出 1.47×（≫ σ² 下限钳的 0.1μ 衰减带）⇒ 实测 0.059：{w_on}"
    );
    assert!(
        w_west >= 0.9,
        "同侧近墙探针必须**几乎不被抑制**（它的射线正好命中 ROI 所在的墙面 ⇒ μ ≥ dist ⇒ \
         权重恒 1）——否则深度项是「一律衰减」而非遮挡感知：实测 {w_west}"
    );
    assert!(
        w_west > w_on * 4.0,
        "同侧权重必须显著高于穿墙权重：{w_west} vs {w_on}（相差 {:.1}×）",
        w_west / w_on
    );
    // CPU 镜像的**线性**辐照比 = GPU 像素比的解析上界（ACES + sRGB 会压缩它），
    // 打印出来给下面的像素阈值定标（先算清期望量级，再断言，不猜数）
    let cpu_ratio = {
        let grid = world.try_resource::<DdgiGrid>().expect("网格在场");
        let blend = |enabled: bool| {
            ddgi_blend(
                grid.positions(),
                grid.radii(),
                grid.sh(),
                grid.depth(),
                WALL_FAR,
                WALL_NORMAL,
                DDGI_DEPTH_GRID,
                enabled,
            )
            .expect("ROI 在网格内必须有有效探针")
        };
        let (on, off) = (blend(true), blend(false));
        println!("CPU 镜像 E/π（法线 −X = 墙面）：开 {on:?} / 关 {off:?}");
        off.x / on.x.max(1e-6)
    };
    println!("CPU 线性辐照比（关 / 开）= {cpu_ratio:.3}");
    assert!(
        cpu_ratio >= 3.0,
        "CPU 镜像的漏光比必须可判别（≥ 3×）：{cpu_ratio:.3}——否则 GPU 侧不可能量出来"
    );

    // 归因护栏（CPU 侧，精确）：**开阔无遮挡处**的深度项必须近中性 ⇒
    // 「抑制」是遮挡感知，不是把整场 GI 一律调暗。
    {
        let grid = world.try_resource::<DdgiGrid>().expect("网格在场");
        let blend = |p: Vec3, enabled: bool| {
            ddgi_blend(
                grid.positions(),
                grid.radii(),
                grid.sh(),
                grid.depth(),
                p,
                Vec3::Y,
                DDGI_DEPTH_GRID,
                enabled,
            )
            .unwrap_or_else(|| panic!("{p:?} 在网格内必须有有效探针"))
        };
        let open_ratio = blend(FLOOR_OPEN_EAST, false).x / blend(FLOOR_OPEN_EAST, true).x;
        println!("开阔东侧地板的关/开比 = {open_ratio:.3}（应 ≈ 1）");
        assert!(
            open_ratio < 1.5,
            "开阔无遮挡处的深度项必须近中性（否则是一律调暗而非遮挡感知）：{open_ratio:.3}"
        );
        assert!(
            cpu_ratio > open_ratio * 2.0,
            "穿墙处的抑制必须显著强于开阔处：{cpu_ratio:.3} vs {open_ratio:.3}"
        );
    }

    // GPU 归因：同一世界、同一渲染器，只翻深度项开关（其余全同 ⇒ 像素差
    // 只能来自 chebyshev 项）
    let mut renderer = OffscreenRenderer::new(SIZE, SIZE).expect("离屏渲染器");
    renderer.render(&world);
    let px_on = renderer.read_pixels();
    renderer.renderer_mut().set_ddgi_depth_weight_enabled(false);
    renderer.render(&world);
    let px_off = renderer.read_pixels();

    let wall_on = roi_lum(&px_on, &camera, WALL_FAR, 3);
    let wall_off = roi_lum(&px_off, &camera, WALL_FAR, 3);
    let ratio = wall_off as f32 / wall_on.max(1) as f32;
    println!(
        "墙面 ROI：深度项开 {wall_on} / 关 {wall_off}（比值 {ratio:.3}，差 {}）",
        wall_off - wall_on
    );
    assert_ne!(px_on, px_off, "深度项开 / 关的画面必须可判别");
    // 头条断言：墙另一侧的探针在关掉深度项后把暗室近墙墙面照亮（= 漏光），
    // 开启后被压回去。ROI 在墙面上 ⇒ 直射项结构性为零、像素里除常量 ambient
    // 外只剩间接漫反射 ⇒ 比值不被稀释（实测 CPU 线性比 7.9× → 像素比 2.9×，
    // 差的倍数是 ACES + sRGB 的压缩，不是信号丢失）。
    assert!(
        wall_off - wall_on >= 60,
        "漏光抑制的绝对幅度必须可判别（8-bit 亮度差 ≥ 60）：关 {wall_off} vs 开 {wall_on}（差 {}）",
        wall_off - wall_on
    );
    assert!(
        ratio >= 1.8,
        "漏光抑制的比值必须可判别（≥ 1.8）：关 {wall_off} / 开 {wall_on} = {ratio:.3}"
    );
}

// ---------------------------------------------------------------------------
// ④ 滚动收敛：ddgi_updated 计数序列 + ⌈n/k⌉ 帧后 ≡ 全量重照明
// ---------------------------------------------------------------------------

#[test]
fn rolling_convergence_matches_full_relight() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if !has_gpu() {
        return;
    }
    let k = 9usize;
    let mut world = scene_world();
    let settings = ddgi_settings(k);
    world.insert_resource(settings);
    install(&mut world, &settings);
    let n = world
        .try_resource::<DdgiGrid>()
        .map(|g| g.probe_count())
        .unwrap();
    let expect_frames = n.div_ceil(k);
    println!("n = {n}、k = {k} ⇒ 收敛帧数应为 ⌈n/k⌉ = {expect_frames}");
    assert_eq!(n, 100, "10×1×10 网格");

    // FrameStats::ddgi_updated 的计数序列：前 ⌈n/k⌉ 帧恒 k（尾帧 = n − k·(⌈n/k⌉−1)），
    // 之后恒 0（稳态零操作 ⇒ 渲染器也零上传）
    let mut renderer = OffscreenRenderer::new(SIZE, SIZE).expect("离屏渲染器");
    let mut counts = Vec::new();
    for _ in 0..expect_frames + 3 {
        tick(&world, &settings);
        renderer.draw_frame(&world);
        counts.push(renderer.renderer().stats().ddgi_updated);
    }
    println!("ddgi_updated 序列 = {counts:?}");
    assert_eq!(counts.len(), expect_frames + 3);
    let mut remaining = n as u32;
    for (frame, &c) in counts.iter().enumerate() {
        let expected = remaining.min(k as u32);
        assert_eq!(
            c, expected,
            "第 {frame} 帧的 ddgi_updated 必须是 {expected}"
        );
        remaining -= expected;
    }
    assert_eq!(remaining, 0, "⌈n/k⌉ 帧后必须全部重照明完");
    assert_eq!(
        counts[expect_frames..],
        vec![0; 3],
        "收敛后必须稳态零操作（决策 8）"
    );

    // ⌈n/k⌉ 帧后的画面 ≡ 「k = n 一帧全量重照明」的世界（等价锚点①的 GPU 半）
    renderer.render(&world);
    let px_rolling = renderer.read_pixels();
    let mut full = scene_world();
    let full_settings = ddgi_settings(usize::MAX);
    full.insert_resource(full_settings);
    install(&mut full, &full_settings);
    converge(&full, &full_settings);
    let px_full = render(&full);
    assert_eq!(
        fnv1a(&px_rolling),
        fnv1a(&px_full),
        "滚动收敛后的画面必须与「一帧全量重照明」FNV 相等（等价锚点①的 GPU 半）"
    );
    // CPU 表也逐位相等（不只是像素）
    let rolling = world.try_resource::<DdgiGrid>().expect("在场");
    let converged = full.try_resource::<DdgiGrid>().expect("在场");
    assert_eq!(
        table_hash(&rolling),
        table_hash(&converged),
        "滚动收敛后的 CPU 表（SH + 深度矩）必须逐位等于全量重照明"
    );
}

// ---------------------------------------------------------------------------
// ⑤ 双会话 48 帧逐帧逐位（R60/R62b 范式）
// ---------------------------------------------------------------------------

#[test]
fn two_sessions_are_bitwise_identical_over_frames() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if !has_gpu() {
        return;
    }
    const FRAMES: usize = 48;
    let settings = ddgi_settings(3);
    let run = |tag: &str| -> (Vec<u64>, Vec<u64>) {
        let mut world = scene_world();
        world.insert_resource(settings);
        install(&mut world, &settings);
        let mut renderer = OffscreenRenderer::new(SIZE, SIZE).expect("离屏渲染器");
        let mut pixels = Vec::with_capacity(FRAMES);
        let mut tables = Vec::with_capacity(FRAMES);
        for frame in 0..FRAMES {
            tick(&world, &settings);
            renderer.render(&world);
            pixels.push(fnv1a(&renderer.read_pixels()));
            let grid = world.try_resource::<DdgiGrid>().expect("在场");
            tables.push(table_hash(&grid));
            assert_eq!(
                grid.stats().ticks as usize,
                frame + 2,
                "{tag}：第 {frame} 帧的 tick 计数（安装帧占 1 = 粗填的 stamp 时钟）"
            );
        }
        (pixels, tables)
    };
    let (pixels_a, tables_a) = run("会话 A");
    let (pixels_b, tables_b) = run("会话 B");
    assert_eq!(pixels_a.len(), FRAMES);
    for (i, (a, b)) in pixels_a.iter().zip(&pixels_b).enumerate() {
        assert_eq!(a, b, "第 {i} 帧的像素 FNV 必须双会话逐位一致");
    }
    assert_eq!(
        tables_a, tables_b,
        "CPU 表（SH + 深度矩）的逐帧哈希序列必须双会话一致"
    );
    // 非平凡性：滚动过程中画面确实在变（不是「恒等假通过」）
    let distinct: std::collections::BTreeSet<u64> = pixels_a.iter().copied().collect();
    assert!(
        distinct.len() > 5,
        "48 帧的滚动重照明必须产生多帧不同画面（实得 {} 种）",
        distinct.len()
    );
    // 收敛后稳态：尾部若干帧必须完全相同（无脏探针 ⇒ 表与像素都不再变）
    let tail = &pixels_a[pixels_a.len() - 5..];
    assert!(
        tail.iter().all(|h| *h == tail[0]),
        "收敛后必须稳态：{tail:?}"
    );
    let tail_tables = &tables_a[tables_a.len() - 5..];
    assert!(
        tail_tables.iter().all(|h| *h == tail_tables[0]),
        "收敛后 CPU 表必须稳态：{tail_tables:?}"
    );
}

// ---------------------------------------------------------------------------
// ⑥ 昼夜联动（R80）：DirectionalLight 变化 ⇒ L0 在 ⌈n/k⌉ 帧内单调跟随
// ---------------------------------------------------------------------------

#[test]
fn daynight_change_is_followed_within_convergence_window() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let mut world = scene_world();
    let settings = ddgi_settings(16); // k = 16、n = 100 ⇒ 收敛窗口 ⌈100/16⌉ = 7 帧
    world.insert_resource(settings);
    install(&mut world, &settings);
    let n = world
        .try_resource::<DdgiGrid>()
        .map(|g| g.probe_count())
        .unwrap();
    let window = n.div_ceil(settings.probes_per_frame);
    assert_eq!(
        (n, window),
        (100, 7),
        "k = 16、n = 100 ⇒ 收敛窗口 ⌈100/16⌉ = 7 帧"
    );
    converge(&world, &settings);

    let mean_l0 = |world: &World| -> f32 {
        let grid = world.try_resource::<DdgiGrid>().expect("在场");
        let sum: f32 = grid.sh().iter().map(|c| c[0][0] + c[0][1] + c[0][2]).sum();
        sum / grid.probe_count() as f32
    };
    let fp_of = |world: &World| -> u64 {
        relight_fingerprint(&bake_params(world, &settings), settings.rays_per_face)
    };
    let before = mean_l0(&world);
    let fp_before = fp_of(&world);
    assert!(before > 0.0, "前提：收敛后的 L0 必须为正（{before}）");

    // 阶跃：黄昏 ⇒ 正午（R80 昼夜系统当帧写方向光，DDGI 当帧消费）
    world.resource_mut::<DirectionalLight>().ambient = Vec3::splat(0.35);
    world.resource_mut::<DirectionalLight>().intensity = 6.0;
    assert_ne!(
        fp_before,
        fp_of(&world),
        "光照阶跃必须改变重照明指纹（否则下面全是恒等假通过）"
    );
    let mut series = Vec::new();
    for _ in 0..window + 2 {
        tick(&world, &settings);
        series.push(mean_l0(&world));
    }
    println!("L0 均值序列（阶跃后 {window}+2 帧）= {series:?}（阶跃前 {before}）");
    // 单调不降：滚动窗口逐帧把旧值换成更亮的新值
    for pair in series.windows(2) {
        assert!(
            pair[1] >= pair[0] - 1e-6,
            "L0 必须单调跟随光照上升：{series:?}"
        );
    }
    assert!(
        series[0] > before,
        "第 1 帧就必须开始跟随（不是等窗口结束才跳）"
    );
    // ⌈n/k⌉ 帧后**完全到位** = 新光照下从头全量重照明的值（逐位对照）
    let mut reference = scene_world();
    reference.resource_mut::<DirectionalLight>().ambient = Vec3::splat(0.35);
    reference.resource_mut::<DirectionalLight>().intensity = 6.0;
    reference.insert_resource(settings);
    install(&mut reference, &settings);
    converge(&reference, &settings);
    let target = mean_l0(&reference);
    assert!(
        (series[window - 1] - target).abs() < 1e-6 * target,
        "第 {window} 帧必须完全到位：{} vs 全量重照明 {target}",
        series[window - 1]
    );
    let stepped = world.try_resource::<DdgiGrid>().expect("在场");
    let reference_grid = reference.try_resource::<DdgiGrid>().expect("在场");
    assert_eq!(
        table_hash(&stepped),
        table_hash(&reference_grid),
        "阶跃后 ⌈n/k⌉ 帧的表必须逐位等于「新光照下从头全量重照明」的表"
    );
    assert!(
        target > before * 1.5,
        "正午的 L0 必须显著高于黄昏：{target} vs {before}"
    );
}

// ---------------------------------------------------------------------------
// ⑦ R89 · cov = 1 内区 ≈ R88（语义修订的归因锚）
// ---------------------------------------------------------------------------

/// 4×3×4 m 封闭小房（南墙留窗），太阳从窗外照入 ⇒ 室内有真实的一次反弹。
/// `ambient = 0`：A3（ambient × (1 − cov)）在此场景**恰好逐位中性**，
/// 剩下的唯一变量是 A2 的覆盖度混合。
fn cov1_world() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(2.0, 1.6, 3.4),
        target: Vec3::new(2.0, 1.1, 0.6),
        fov_y_radians: 70.0f32.to_radians(),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.25, -0.9, 0.45).normalize(),
        color: Vec3::new(1.0, 0.95, 0.88),
        intensity: 5.0,
        ambient: Vec3::ZERO,
        shadows: true,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());

    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(4.0));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let floor_mat = world
        .resource_mut::<MaterialStore>()
        .add(rough(0.78, 0.76, 0.72));
    let wall_mat = world
        .resource_mut::<MaterialStore>()
        .add(rough(0.62, 0.6, 0.58));
    world.spawn((
        scaled(Vec3::new(2.0, 0.0, 2.0), Vec3::ONE),
        plane,
        floor_mat,
    ));
    world.spawn((
        scaled(Vec3::new(0.0, 1.5, 2.0), Vec3::new(0.2, 3.0, 4.2)),
        cube,
        wall_mat,
    ));
    world.spawn((
        scaled(Vec3::new(2.0, 1.5, 4.0), Vec3::new(4.2, 3.0, 0.2)),
        cube,
        wall_mat,
    ));
    world.spawn((
        scaled(Vec3::new(4.0, 1.5, 2.0), Vec3::new(0.2, 3.0, 4.2)),
        cube,
        wall_mat,
    ));
    // 南墙 z=0：两段 + 窗台 + 窗楣（窗 = x ∈ (1.5, 2.5)、y ∈ (0.5, 2.0)）
    for x in [0.75f32, 3.25] {
        world.spawn((
            scaled(Vec3::new(x, 1.5, 0.0), Vec3::new(1.5, 3.0, 0.2)),
            cube,
            wall_mat,
        ));
    }
    world.spawn((
        scaled(Vec3::new(2.0, 0.25, 0.0), Vec3::new(1.1, 0.5, 0.2)),
        cube,
        wall_mat,
    ));
    world.spawn((
        scaled(Vec3::new(2.0, 2.5, 0.0), Vec3::new(1.1, 1.0, 0.2)),
        cube,
        wall_mat,
    ));
    world.spawn((
        scaled(Vec3::new(2.0, 3.15, 2.0), Vec3::new(4.2, 0.3, 4.2)),
        cube,
        wall_mat,
    ));
    propagate_transforms(&mut world);
    world
}

/// 全画面 cov = 1 的网格：bounds (0,0,0)-(4,3.5,4)、spacing 1.0 ⇒ 4×3×4 =
/// 48 探针（格心 x/z ∈ {0.5,1.5,2.5,3.5}、y ∈ {0.5,1.5,2.5}）、radius 1.5
/// ⇒ cov = 1 内区 = d ≤ 1.125 ≥ 任意表面点到格心的最大距离 0.866（半格
/// 对角；bounds min y = 0 保证最低探针层贴地 0.5 m）。
fn cov1_settings() -> DdgiSettings {
    DdgiSettings {
        spacing: 1.0,
        bounds: Some((Vec3::new(0.0, 0.0, 0.0), Vec3::new(4.0, 3.5, 4.0))),
        probes_per_frame: usize::MAX,
        rays_per_face: 8,
        ..DdgiSettings::default()
    }
}

/// R89 锚（⑦）：全画面 cov = 1、ambient = 0 的场景——A2
/// （`mix(env, gi, cov)`）在 cov = 1 处是**精确恒等**（CPU 镜像单测
/// `cov_endpoints_are_exact_identities` 钉住数学），A3 在 ambient = 0 处
/// 恰好中性 ⇒ 语义上逐位等于 R88。实测差异 = **全帧 196,608 字节中恰
/// 1 字节 ±1 LSB**（R63 同款编译期副作用：mode 2 分支的增量代码改变
/// GPU 编译器的 FMA / 公共子表达式决策，一个暗部量化临界像素翻位）——
/// 语义等价但哈希不可逐位锁 R88 原值，故锁定 R89 修订后的新基线
/// （R88 原值 0x7353e0ce27f6a7c6，diff 归因见 R89 CHANGELOG）。
/// DDGI-off 对照组**不受 fs_main 增量影响、与 R88 逐位一致** ⇒ 锁 R88
/// 原值（改动面没外溢到缺席路径的直接证据）。
#[test]
fn cov1_interior_matches_r88_within_single_lsb_compiler_noise() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if !has_gpu() {
        return;
    }
    const R89_COV1_ON: u64 = 0x1df3_e381_0bb4_1fe3;
    const R88_COV1_OFF: u64 = 0x6e3e_b0bc_9131_b077;

    let off = cov1_world();
    let px_off = render(&off);
    assert_eq!(
        fnv1a(&px_off),
        R88_COV1_OFF,
        "DDGI 缺席帧必须与 R88 代码逐位一致（fs_main 的 R89 增量不得外溢到缺席路径）"
    );

    let mut on = cov1_world();
    let settings = cov1_settings();
    let params = bake_params(&on, &settings);
    let mut grid = DdgiGrid::new();
    grid.install(&settings, &params, collect_bake_triangles(&on));
    on.insert_resource(grid);
    tick(&on, &settings);
    let px_on = render(&on);
    assert_eq!(
        fnv1a(&px_on),
        R89_COV1_ON,
        "cov = 1 内区帧必须锁定 R89 基线（= R88 语义 + 1 字节编译期噪声；实得 {:#018x}）",
        fnv1a(&px_on)
    );
    assert_ne!(
        fnv1a(&px_on),
        fnv1a(&px_off),
        "cov1 场景关 / 开必须可判别（DDGI 活着，否则本锚是恒等假通过）"
    );
}

// ---------------------------------------------------------------------------
// ⑧ R89 · DDGI × 光照贴图 = 互斥（A1 裁决）
// ---------------------------------------------------------------------------

/// A1：全场景挂图集 + `ambient = 0`（隔离 A3 的无条件 ambient 衰减——
/// 该衰减按设计作用于所有 `cov > 0` 像素、含图集实体）⇒ DDGI 开 / 关
/// **逐位一致**：图集实体不发探针标志位（`renderer.rs` 的 `probe_payload`
/// 裁决）⇒ 完全吃不到 DDGI；贴图已含 `sky + bounce`、相加即双计。
/// 末尾对照：撤掉一个实体的图集绑定后同场景立即变得可判别
/// （证明 DDGI 在本场景确实活着、mode = 2 在跑——非恒等假通过）。
#[test]
fn ddgi_does_not_touch_lightmapped_entities_bitwise() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if !has_gpu() {
        return;
    }
    const SIZE: u32 = 256;
    let build = |with_ddgi: bool| -> (World, iris_core::prelude::Entity) {
        // lightmap_render.rs 的室内盒场景（地板 + 两墙 + 遮挡柱，全部参与
        // 烘焙 ⇒ 全部挂图集），ambient 压到 0
        let mut world = World::new();
        world.insert_resource(MeshStore::new());
        world.insert_resource(MaterialStore::new());
        world.insert_resource(TextureStore::new());
        world.insert_resource(Camera {
            eye: Vec3::new(3.4, 2.2, 3.8),
            target: Vec3::new(-1.2, 0.6, -2.0),
            ..Camera::default()
        });
        world.insert_resource(DirectionalLight {
            direction: Vec3::new(-0.55, -1.0, 0.3).normalize(),
            color: Vec3::new(1.0, 0.95, 0.85),
            intensity: 2.2,
            ambient: Vec3::ZERO,
            ..DirectionalLight::default()
        });
        world.insert_resource(BloomSettings::disabled());
        let floor_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(10.0));
        let wall_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(10.0));
        let column_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
        let mut mats = world.resource_mut::<MaterialStore>();
        let floor_mat = mats.add(MaterialDef::solid(0.8, 0.78, 0.72, 1.0));
        let wall_mat = mats.add(MaterialDef::solid(0.55, 0.6, 0.7, 1.0));
        let column_mat = mats.add(MaterialDef::solid(0.9, 0.35, 0.2, 1.0));
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
        let column = world.spawn((
            Transform {
                position: Vec3::new(1.5, 1.0, 0.0),
                scale: Vec3::new(0.6, 2.0, 0.6),
                ..Transform::IDENTITY
            },
            column_mesh,
            column_mat,
        ));
        propagate_transforms(&mut world);
        // 烘焙 + 挂载（图集含 sky + bounce ⇒ 与 DDGI 同一份贡献）
        let baked = bake_lightmap(
            &world,
            &BakeParams {
                light_dir: Vec3::new(-0.55, -1.0, 0.3),
                light_color: Vec3::new(1.0, 0.95, 0.85) * 2.2,
                sky_color: Vec3::new(0.12, 0.15, 0.22),
                shadow_softness: 0.08,
                sky_samples: 16,
                bounces: 1,
            },
            &BakeUvParams {
                texels_per_meter: 4.0,
                atlas_size: 128,
            },
        );
        install_lightmap(&mut world, baked);
        if with_ddgi {
            let settings = DdgiSettings {
                spacing: 2.5,
                bounds: Some((Vec3::new(-5.0, 0.0, -5.0), Vec3::new(5.0, 2.5, 5.0))),
                probes_per_frame: usize::MAX,
                rays_per_face: 8,
                ..DdgiSettings::default()
            };
            let params = bake_params(&world, &settings);
            let mut grid = DdgiGrid::new();
            grid.install(&settings, &params, collect_bake_triangles(&world));
            world.insert_resource(grid);
            tick(&world, &settings);
        }
        (world, column)
    };

    let (off_world, _) = build(false);
    let (on_world, column) = build(true);
    let px_off = {
        let mut renderer = OffscreenRenderer::new(SIZE, SIZE).expect("离屏渲染器");
        renderer.render(&off_world);
        renderer.read_pixels()
    };
    let px_on = {
        let mut renderer = OffscreenRenderer::new(SIZE, SIZE).expect("离屏渲染器");
        renderer.render(&on_world);
        renderer.read_pixels()
    };
    assert_eq!(
        px_off, px_on,
        "全场景挂图集 + ambient = 0 ⇒ DDGI 开 / 关必须逐位一致\
         （图集实体不发探针标志位 ⇒ 吃不到 DDGI；A1 互斥裁决）"
    );

    // 非恒等对照：撤掉柱子的图集绑定 ⇒ 它的实体标志位恢复 ⇒ mode 2 立即可判别
    let mut exposed = on_world;
    exposed.remove_component::<LightmapBinding>(column);
    let mut renderer = OffscreenRenderer::new(SIZE, SIZE).expect("离屏渲染器");
    renderer.render(&exposed);
    assert_ne!(
        fnv1a(&renderer.read_pixels()),
        fnv1a(&px_on),
        "撤图集绑定的实体必须立刻吃到 DDGI（证明互斥的另一半：未挂图集 = 探针接管）"
    );
}

// ---------------------------------------------------------------------------
// ⑨ R89 · SSR × DDGI（A5）+ 强度旋钮（B1）
// ---------------------------------------------------------------------------

/// A5 + B1：①`intensity = 0` 帧 ≡ SSR 关闭帧逐位（修复的正确性：零强度
/// = 零反射）；②`intensity = 2` 在全量权重区被钳到 1 ⇒ 与 1.0 逐位一致
/// （命中权重、不是亮度倍率）；③SSR 命中色随 DDGI 表内容变化（帧尾副本
/// 含全部 GI ⇒ 一致性天然成立，A5 裁决）。相机压低看向光滑地板
/// （roughness 0.15 < `SSR_ROUGH_FLOOR` ⇒ 粗糙度全量权重），反射里是带
/// GI 的墙面。帧序：首帧 ssr_prev_valid = false（跳过）、第 2 帧起步进
/// ⇒ 每跑渲 3 帧取末帧。
#[test]
fn ssr_reflects_ddgi_and_intensity_knob_is_live() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if !has_gpu() {
        return;
    }
    const SIZE: u32 = 256;
    let build = |intensity: f32, enabled: bool, converged: bool| -> World {
        let mut world = cov1_world();
        world.insert_resource(Camera {
            eye: Vec3::new(2.0, 0.7, 3.3),
            target: Vec3::new(2.0, 0.25, 1.0),
            fov_y_radians: 70.0f32.to_radians(),
            ..Camera::default()
        });
        // 地板换光滑材质：floor_mat 在 cov1_world 先于 wall_mat 创建 ⇒
        // 句柄 1 = 地板材质；两阶段（收集 → 写回，AGENTS.md 规约）
        let smooth = world.resource_mut::<MaterialStore>().add({
            let mut def = MaterialDef::solid(0.78, 0.76, 0.72, 1.0);
            def.roughness = 0.15;
            def.metallic = 0.0;
            def
        });
        let mut retarget: Vec<iris_core::prelude::Entity> = Vec::new();
        world.query::<(iris_core::prelude::Entity, &MaterialHandle)>(|(e, h)| {
            if h.0 == 1 {
                retarget.push(e);
            }
        });
        for e in retarget {
            world.insert(e, smooth);
        }
        world.insert_resource(SsrSettings {
            enabled,
            intensity,
            ..SsrSettings::default()
        });
        let settings = cov1_settings();
        let params = bake_params(&world, &settings);
        let mut grid = DdgiGrid::new();
        grid.install(&settings, &params, collect_bake_triangles(&world));
        world.insert_resource(grid);
        if converged {
            tick(&world, &settings);
        } // 否则 = 安装帧粗填表（GI 明显不同的另一个表态）
        world
    };
    let render3 = |world: &World| -> Vec<u8> {
        let mut renderer = OffscreenRenderer::new(SIZE, SIZE).expect("离屏渲染器");
        for _ in 0..3 {
            renderer.render(world);
        }
        renderer.read_pixels()
    };

    let off = render3(&build(1.0, false, true));
    let zero = render3(&build(0.0, true, true));
    assert_eq!(
        off, zero,
        "intensity = 0 的 SSR 帧 ≡ SSR 关闭帧逐位（B1 修复：零强度 = 零反射）"
    );
    let one = render3(&build(1.0, true, true));
    // 非默认强度可判别（AGENTS.md「每个公开旋钮至少一条非默认值断言」）：
    // intensity 0.5 在命中点 edge_fade < 1 的区域把权重减半 ⇒ 像素可判别。
    // （`> 1` 的钳到 1 是纯标量性质，解析断言在 CPU 镜像
    // `post.rs::ssr_blend_weight_follows_full_formula_with_nondefault_intensity`
    // ——GPU 帧级断言在边缘衰减区无法构造「全量权重」前提。）
    let half = render3(&build(0.5, true, true));
    assert_ne!(
        one, half,
        "intensity 0.5 的 SSR 帧必须与 1.0 可判别（B1 修复：强度旋钮真实生效）"
    );
    assert_ne!(
        off, one,
        "SSR 开启（光滑地板反射 GI 墙面）必须与关闭可判别（否则 ③ 无信号）"
    );
    let coarse = render3(&build(1.0, true, false));
    assert_ne!(
        coarse, one,
        "SSR 命中色必须随 DDGI 表变化（上帧颜色副本含全部 GI ⇒ A5 一致性）"
    );
}

// ---------------------------------------------------------------------------
// ⑩ R89 · reset_taa 与时域状态（A5 / B2）
// ---------------------------------------------------------------------------

/// A5 / B2 的 DDGI 半：`reset_taa()` 是「清一切跨帧时域状态」，探针域是
/// **故意不清**的那一类——表内容 / `probe_stamp` / `structure_generation` /
/// `dirty` / cursor 在 reset 前后逐位不变，reset 后稳态帧像素不变
/// （`ddgi_upload` 记账不被扰动 ⇒ 无全量重传行为差异）。
#[test]
fn reset_taa_does_not_disturb_ddgi() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if !has_gpu() {
        return;
    }
    let mut world = cov1_world();
    let settings = cov1_settings();
    let params = bake_params(&world, &settings);
    let mut grid = DdgiGrid::new();
    grid.install(&settings, &params, collect_bake_triangles(&world));
    world.insert_resource(grid);
    tick(&world, &settings);

    let mut renderer = OffscreenRenderer::new(SIZE, SIZE).expect("离屏渲染器");
    renderer.render(&world);
    renderer.render(&world);
    let px_before = renderer.read_pixels();

    let snapshot = |world: &World| -> Vec<u64> {
        let grid = world.try_resource::<DdgiGrid>().expect("网格在场");
        let mut out = Vec::new();
        for c in grid.sh() {
            for v in c {
                for f in v {
                    out.push(u64::from(f.to_bits()));
                }
            }
        }
        for m in grid.depth() {
            out.push(u64::from(m[0].to_bits()));
            out.push(u64::from(m[1].to_bits()));
        }
        for s in grid.probe_stamp() {
            out.push(*s);
        }
        out.push(grid.structure_generation());
        out.push(grid.grid_fp());
        out.push(grid.geometry_generation());
        out.push(grid.cursor() as u64);
        for d in grid.dirty() {
            out.push(u64::from(*d));
        }
        out
    };
    let before = snapshot(&world);
    renderer.renderer_mut().reset_taa();
    let after = snapshot(&world);
    assert_eq!(
        before, after,
        "reset_taa 不得触碰探针域状态（表 / stamp / generation / dirty / cursor 逐位不变）"
    );
    renderer.render(&world);
    assert_eq!(
        fnv1a(&renderer.read_pixels()),
        fnv1a(&px_before),
        "reset 后的稳态帧必须与 reset 前逐位一致（ddgi_upload 记账未被扰动）"
    );
}

/// B2 的 GTAO 半（iris-exp frictions ⑬ 的修复锚）：AO 开启的世界渲染 2 帧
/// （ao_on 序列 0 → 1）⇒ `reset_taa()` ⇒ 首帧 `ao_on == 0` 且**像素 ≡
/// AO 关闭帧**（旧场景深度副本被宣告失效，切镜残影消灭）⇒ 再一帧 ao_on
/// 恢复 1。对照组：不调 reset 的世界连续 3 帧 ao_on 序列恒 [0, 1, 1]
/// （默认路径零影响）。
#[test]
fn reset_taa_clears_gtao_depth_history() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if !has_gpu() {
        return;
    }
    const SIZE: u32 = 256;
    let ao_settings = AoSettings {
        enabled: true,
        radius: 0.9,
        ..AoSettings::default()
    };
    let build = |ao: bool| -> World {
        let mut world = cov1_world();
        if ao {
            world.insert_resource(ao_settings);
        }
        world
    };
    let ao_world = build(true);
    let mut renderer = OffscreenRenderer::new(SIZE, SIZE).expect("离屏渲染器");
    renderer.render(&ao_world);
    renderer.render(&ao_world);
    assert_eq!(
        renderer.renderer().stats().ao_on,
        1,
        "前提：第二帧起 AO 激活（上帧深度副本已就位）"
    );
    let px_ao_steady = renderer.read_pixels();

    let off_world = build(false);
    let mut off_renderer = OffscreenRenderer::new(SIZE, SIZE).expect("离屏渲染器");
    off_renderer.render(&off_world);
    let px_ao_disabled = off_renderer.read_pixels();

    renderer.renderer_mut().reset_taa();
    renderer.render(&ao_world);
    assert_eq!(
        renderer.renderer().stats().ao_on,
        0,
        "reset_taa 后首帧 ao_on 必须 = 0（ao_depth_frame 记账被宣告失效）"
    );
    let px_after_reset = renderer.read_pixels();
    assert_eq!(
        px_after_reset, px_ao_disabled,
        "reset 后首帧像素必须 ≡ AO 关闭帧（可见度图中性 1.0，GTAO 残影消灭）"
    );
    renderer.render(&ao_world);
    assert_eq!(renderer.renderer().stats().ao_on, 1, "第二帧起 AO 恢复");
    // 静止场景的正确性质：reset 后系统回到与 reset 前**逐位相同**的稳态
    // （恢复帧的「新」上帧深度 = 同一静止场景 ⇒ 可见度图一致）——同时是
    // 「重置不引入永久损伤」的确定性断言。（「内容换新」的可观测形态需要
    // 运动场景，由 ao_render.rs 的确定性运动套件覆盖，此处不重复。）
    renderer.render(&ao_world);
    assert_eq!(
        fnv1a(&renderer.read_pixels()),
        fnv1a(&px_ao_steady),
        "reset 后的稳态帧必须回到与 reset 前逐位一致的稳态（重置无永久损伤）"
    );

    let steady = build(true);
    let mut renderer2 = OffscreenRenderer::new(SIZE, SIZE).expect("离屏渲染器");
    let mut seq = Vec::new();
    for _ in 0..3 {
        renderer2.render(&steady);
        seq.push(renderer2.renderer().stats().ao_on);
    }
    assert_eq!(seq, vec![0, 1, 1], "不调 reset 的宿主不受影响");
}

// ---------------------------------------------------------------------------
// ㉑ C7a · 火把进 GI 与同源天空的 GPU 端到端（c7-design-v1 裁定甲案）
// ---------------------------------------------------------------------------

/// 灯集 / 同源天空两个重照明输入在**像素管线**上生效且可判别，并锚住
/// 「输入不变 ⇒ 表不动 ⇒ 像素逐位不动」的稳态语义。
///
/// 几何设计：火把藏在暗室西南角（`range 4`），与共享墙西面 ROI 相距 ≈ 10 m
/// ⇒ **直射项在 ROI 结构性为零**，`px_off → px_on` 的差值只可能来自
/// DDGI 表（火把照亮西角地板 → 探针反弹 → 整室间接光抬升）——不需要
/// 再差一份「无火把世界」来剥离直接光。
#[test]
fn torch_and_sky_inputs_reach_screen_and_are_distinguishable() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if !has_gpu() {
        return;
    }
    let mut world = scene_world();
    let torch_at = Vec3::new(-6.5, 1.5, -4.0);
    world.spawn((
        Transform::from_xyz(torch_at.x, torch_at.y, torch_at.z),
        PointLight {
            color: Vec3::ONE,
            intensity: 30.0,
            range: 4.0,
            shadows: false,
        },
    ));
    propagate_transforms(&mut world);
    let settings = ddgi_settings(100);
    let params = bake_params(&world, &settings);
    let mut grid = DdgiGrid::new();
    grid.install(&settings, &params, collect_bake_triangles(&world));
    assert_eq!(grid.tick(&settings, &params), 100, "k = 100 ⇒ 首帧全表精修");
    world.insert_resource(grid);

    let camera = world.resource::<Camera>().clone();
    let px_off = render(&world);
    let roi_off = roi_lum(&px_off, &camera, WALL_FAR, 5);

    // 开门：火把进表（收集口径 color×intensity=30 与主灯表同式）
    let lit = RelightInputs {
        lights: vec![GiLight::point(torch_at, Vec3::splat(30.0), 4.0, true)],
        ..RelightInputs::default()
    };
    assert_eq!(
        world
            .resource_mut::<DdgiGrid>()
            .tick_with(&settings, &params, &lit),
        100,
        "灯集入指纹 ⇒ 全表标脏（k = 100 一帧收敛）"
    );
    let px_on = render(&world);
    let roi_on = roi_lum(&px_on, &camera, WALL_FAR, 5);
    assert_ne!(fnv1a(&px_off), fnv1a(&px_on), "火把进 GI 必须改变全帧像素");
    assert!(
        roi_on > roi_off,
        "共享墙西面 ROI 必须由火把反弹抬亮（直射在 ROI 结构性为零）：{roi_off} → {roi_on}"
    );

    // 稳态锚：同输入再 tick ⇒ 零脏 ⇒ 逐位同帧（「输入不变」的 GPU 级证据）
    assert_eq!(
        world
            .resource_mut::<DdgiGrid>()
            .tick_with(&settings, &params, &lit),
        0,
        "输入不变不得标脏"
    );
    assert_eq!(px_on, render(&world), "稳态帧逐位不动");

    // 天空同源档：程序化天空（远亮于 AMBIENT ≈0.004 底）再翻一次口径
    let sky_lit = RelightInputs {
        sky: Some(DdgiSky::Procedural),
        ..lit.clone()
    };
    assert!(
        world
            .resource_mut::<DdgiGrid>()
            .tick_with(&settings, &params, &sky_lit)
            > 0,
        "天空快照变 ⇒ 全表标脏"
    );
    let px_sky = render(&world);
    assert_ne!(
        fnv1a(&px_on),
        fnv1a(&px_sky),
        "同源天空（逐向程序化渐变）必须与常量底可判别"
    );
}
