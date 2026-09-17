//! DDGI v2 验收图（R89）：2×2 面板对照，输出 `iris_ddgi_v2.ppm` →
//! `docs/ddgi_v2.png`。场景与 `ddgi_snapshot.rs`（R88）同源：一墙分隔
//! 亮 / 暗两室，门口是唯一连通，太阳从东侧斜上方照进来。
//!
//! 面板布局（2 排 × 2 列）——每一格都对照出 R89 的一项语义修订：
//! 1. **DDGI 关**（左上）：`DdgiGrid` 缺席 ⇒ 间接漫反射吃 `irradiance_map`
//!    的无脑天空常量 + ambient 补光全额（`cov = 0` 的参照）；
//! 2. **R89 开（覆盖度羽化）**（右上）：mode = 2 深度感知混合 + `cov`
//!    连续羽化——探针影响半径最外 25% 从 R88 的硬断界变成连续过渡
//!    （§8.57⑱ 扇贝带的修复面；与旧版 `docs/ddgi.png` 的右上面板对比
//!    目检天花板墙角那条波浪带）；
//! 3. **深度项关（漏光对照）**（左下）：`set_ddgi_depth_weight_enabled(false)`
//!    ⇒ 墙东侧探针穿墙串味（chebyshev 项的归因开关，R88 语义原样）；
//! 4. **ambient 去双计对照**（右下）：同面板 2 但 `DirectionalLight.ambient`
//!    拉高 ⇒ `cov = 1` 内区不再叠 ambient（`× (1 − cov)`，§8.57⑬ 的修复
//!    面），面板 2（ambient 近零）与面板 4 的 ROI 差 = ambient 分摊的
//!    直接证据。
//!
//! 每格带硬断言（不通过直接 panic、不写文件）；三条 `assert_ne`（关/开、
//! 深度项开/关、ambient 前/后）+ 覆盖度的 CPU 镜像解析值打印 + 双会话
//! 逐位。滚动重照明按生产参数 k = 2 跑满 `⌈n/k⌉ + 1` 帧并断言已收敛。
//!
//! ```sh
//! cargo run -p iris-render --example ddgi_v2_snapshot --release
//! ffmpeg -y -i iris_ddgi_v2.ppm docs/ddgi_v2.png
//! ```

use std::path::Path;

use glam::{Quat, Vec3};
use iris_core::daynight::{ambient_envelope, sun_direction, sun_envelope};
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
const GAP: u32 = 6;

/// 面板 1–3 的昼间相位（与 ddgi_snapshot.rs 同值，画面可直接横向对比）。
const DAY_PHASE: f64 = 0.12;
/// 生产参数的滚动窗口宽度。
const PROBES_PER_FRAME: usize = 2;

/// 共享墙西侧面上的两个采样点（与 ddgi_snapshot.rs 同值；墙面法线 −X ⇒
/// 直射项结构性为零，ROI 差异全部来自间接漫反射）。
const WALL_FAR: Vec3 = Vec3::new(-0.15, 1.2, 4.5);
const WALL_NEAR_DOOR: Vec3 = Vec3::new(-0.15, 1.2, 0.9);
/// 墙面上部的球外采样点：与 WALL_FAR 同面同材质同朝向、只抬高到探针
/// 球顶之上（最近探针 (−0.8, 1.3, 4.0)，d = √(0.65² + 2.3² + 0.5²) ≈
/// 2.44 > 半径 2.4 ⇒ cov = 0，ambient 全额）。
const WALL_TOP_OUT_OF_COVER: Vec3 = Vec3::new(-0.15, 3.6, 4.5);

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

/// 采样点周围 `radius` 像素方窗的平均值。
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

/// 相机在暗室内、朝共享墙与门口方向看（与 ddgi_snapshot.rs 同值）。
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

/// 相位 → `DirectionalLight`（ddgi_snapshot.rs 同款四字段写入）。
fn light_at(phase: f64) -> DirectionalLight {
    let (color, intensity) = sun_envelope(phase);
    DirectionalLight {
        direction: -sun_direction(phase),
        color,
        intensity: intensity * 6.0,
        ambient: ambient_envelope(phase),
        shadows: true,
        ..DirectionalLight::default()
    }
}

/// 场景世界（ddgi_snapshot.rs 同源）；`ambient_override` 供面板 4 的
/// ambient 去双计对照。
fn scene_world(phase: f64, ambient_override: Option<Vec3>) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(room_camera());
    let mut light = light_at(phase);
    if let Some(ambient) = ambient_override {
        light.ambient = ambient;
    }
    world.insert_resource(light);
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
    for z in [-4.375f32, 4.375] {
        world.spawn((
            scaled(Vec3::new(0.0, 2.0, z), Vec3::new(0.3, 4.0, 7.25)),
            cube,
            wall_mat,
        ));
    }
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
    world.spawn((
        scaled(Vec3::new(-4.0, 4.15, 0.0), Vec3::new(8.3, 0.3, 16.3)),
        cube,
        wall_mat,
    ));
    propagate_transforms(&mut world);
    world
}

/// DDGI 设置（生产默认口径，ddgi_snapshot.rs 同值）。
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

/// 与 `iris_engine::ddgi::ddgi_bake_params` 同口径的组装。
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

/// 装网格 + 滚动到收敛（断言 `dirty_count == 0`，不许拿未收敛的表出图）。
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
        "warmup {frames} 帧后必须已收敛（n = {n}、k = {PROBES_PER_FRAME}）"
    );
    frames
}

/// 单格渲染（每格独立渲染器——R53/R62 教训）。
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

    // ---- 面板 1：DDGI 关 ----
    let off_world = scene_world(DAY_PHASE, None);
    let px_off = render(&off_world, true);
    blit(&mut panel, &px_off, col_x(0), row_y(0));

    // ---- 面板 2 / 3：R89 开（羽化）+ 深度项关（漏光）----
    let mut day = scene_world(DAY_PHASE, None);
    day.insert_resource(settings);
    install_and_converge(&mut day, &settings);
    let px_on = render(&day, true);
    blit(&mut panel, &px_on, col_x(1), row_y(0));
    let px_leak = render(&day, false);
    blit(&mut panel, &px_leak, col_x(0), row_y(1));

    // ---- 面板 4：ambient 去双计对照（A3 的锐利形态）----
    // `DirectionalLight.ambient` 拉高约 40×，但 `DdgiSettings::sky_color`
    // **显式覆盖**为面板 2 的 ambient 值 ⇒ 重照明指纹与面板 2 逐位相同
    // ⇒ GI 表逐位一致。两帧的唯一差异 = ambient 补光行（0.02 → 0.6）
    // × (1 − cov)：cov = 1 内区该行被 ×0 完全拆掉（§8.57⑬ 修复面）⇒
    // **内区像素逐位不动**；cov < 1 / 球外按 (1 − cov) 分摊 ⇒ 可判别。
    // （R88 形态下 ambient 拉高会原样叠进内区 ⇒ 内区必然移动——本面板
    // 就是「移动没有发生」的硬证据。）
    let mut bright = scene_world(DAY_PHASE, Some(Vec3::splat(0.6)));
    let bright_settings = DdgiSettings {
        sky_color: Some(ambient_envelope(DAY_PHASE)),
        ..settings
    };
    bright.insert_resource(bright_settings);
    install_and_converge(&mut bright, &bright_settings);
    let px_ambient = render(&bright, true);
    blit(&mut panel, &px_ambient, col_x(1), row_y(1));

    let hashes = [
        fnv1a(&px_off),
        fnv1a(&px_on),
        fnv1a(&px_leak),
        fnv1a(&px_ambient),
    ];

    // ---- 覆盖度的 CPU 镜像解析值（可判别性的定标依据）----
    let grid = day.try_resource::<DdgiGrid>().expect("网格在场");
    for (name, at) in [("WALL_FAR", WALL_FAR), ("WALL_NEAR_DOOR", WALL_NEAR_DOOR)] {
        let cov = ddgi_coverage(at, grid.positions(), grid.radii());
        println!("CPU cov[{name}] = {cov:.4}（半径 {} m）", grid.radii()[0]);
    }

    // ---- 断言 1：关 / 开可判别 + 遮挡感知（GI 显著暗于天空常量兜底）----
    let far_off = roi_lum(&px_off, &camera, WALL_FAR);
    let far_on = roi_lum(&px_on, &camera, WALL_FAR);
    let near_off = roi_lum(&px_off, &camera, WALL_NEAR_DOOR);
    let near_on = roi_lum(&px_on, &camera, WALL_NEAR_DOOR);
    let d_far = far_off - far_on;
    println!("墙面远端：关 {far_off} → 开 {far_on}（漫反射差 {d_far}）");
    assert_ne!(hashes[0], hashes[1], "关 / 开两格必须可判别");
    assert!(d_far >= 60, "遮挡感知的压暗必须可判别：{d_far}");
    assert!(far_on > 0, "暗室不得纯黑：{far_on}");

    // ---- 断言 2：门口梯度（R89 羽化不改变内区 GI 的空间结构）----
    let d_near = near_off - near_on;
    assert!(
        d_far - d_near >= 20,
        "门口梯度必须保留：远端压暗 {d_far} vs 门口旁压暗 {d_near}"
    );
    assert!(near_on > far_on, "离门口越近越亮：{near_on} vs {far_on}");

    // ---- 断言 3：漏光抑制（深度项开 / 关）----
    let far_leak = roi_lum(&px_leak, &camera, WALL_FAR);
    assert_ne!(hashes[1], hashes[2], "深度项开 / 关必须可判别");
    assert!(
        far_leak - far_on >= 60,
        "关掉深度项 ⇒ 穿墙漏光可判别：{far_leak} vs {far_on}"
    );

    // ---- 断言 4：ambient 去双计（A3 的锐利像素证据）----
    // 4a：内区（cov = 1）的 ROI **逐位不动**——ambient 拉高后墙面采样窗
    // 与面板 2 逐字节相同（ambient 行 × 0 = 拆除；GI 表已由 sky_color
    // 覆盖锁同）。
    let roi_block = |px: &[u8], at: Vec3| -> Vec<u8> {
        let (fx, fy) = project_to_pixel(&camera, at, WIDTH, HEIGHT);
        let (cx, cy) = (fx.round() as i32, fy.round() as i32);
        let mut out = Vec::new();
        for dy in -2..=2 {
            for dx in -2..=2 {
                let (x, y) = (cx + dx, cy + dy);
                assert!(
                    x >= 0 && y >= 0 && x < WIDTH as i32 && y < HEIGHT as i32,
                    "ROI ({fx},{fy}) 越界"
                );
                let i = ((y as u32 * WIDTH + x as u32) * 4) as usize;
                out.extend_from_slice(&px[i..i + 4]);
            }
        }
        out
    };
    assert_eq!(
        roi_block(&px_ambient, WALL_FAR),
        roi_block(&px_on, WALL_FAR),
        "ambient 0.6 vs 0.02：cov = 1 内区的墙面窗必须逐位一致\
         （ambient 行被 (1 − cov) = 0 拆除；GI 表已由 sky_color 覆盖锁同）"
    );
    assert_eq!(
        roi_block(&px_ambient, WALL_NEAR_DOOR),
        roi_block(&px_on, WALL_NEAR_DOOR),
        "内区第二采样点同样逐位一致"
    );
    // 4b：球外（cov = 0）分区 ambient 全额 ⇒ 可判别（分摊的分母面；
    // 与 WALL_FAR 同墙面 ⇒ 唯一变量是 cov 与 ambient 增益）
    let top_on = roi_lum(&px_on, &camera, WALL_TOP_OUT_OF_COVER);
    let top_amb = roi_lum(&px_ambient, &camera, WALL_TOP_OUT_OF_COVER);
    println!("球外墙面（cov = 0）：{top_on} → {top_amb}");
    assert!(
        top_amb > top_on,
        "球外 ambient 全额（无探针接管）⇒ 必须可判别：{top_amb} vs {top_on}"
    );
    assert_ne!(
        hashes[1], hashes[3],
        "ambient 前 / 后整帧必须可判别（A3 生效）"
    );

    // ---- 断言 5：双会话逐位（四格各自第二会话）----
    assert_eq!(
        fnv1a(&render(&scene_world(DAY_PHASE, None), true)),
        hashes[0]
    );
    let mut day_b = scene_world(DAY_PHASE, None);
    day_b.insert_resource(settings);
    install_and_converge(&mut day_b, &settings);
    assert_eq!(fnv1a(&render(&day_b, true)), hashes[1]);
    assert_eq!(fnv1a(&render(&day_b, false)), hashes[2]);
    let mut bright_b = scene_world(DAY_PHASE, Some(Vec3::splat(0.6)));
    let bright_settings_b = DdgiSettings {
        sky_color: Some(ambient_envelope(DAY_PHASE)),
        ..settings
    };
    bright_b.insert_resource(bright_settings_b);
    install_and_converge(&mut bright_b, &bright_settings_b);
    assert_eq!(fnv1a(&render(&bright_b, true)), hashes[3]);

    // ---- 写 PPM ----
    let header = format!("P6\n{} {}\n255\n", WIDTH * 2 + GAP, HEIGHT * 2 + GAP);
    let mut ppm = header.into_bytes();
    for chunk in panel.chunks_exact(4) {
        ppm.extend_from_slice(&chunk[..3]);
    }
    std::fs::write(Path::new("iris_ddgi_v2.ppm"), &ppm).expect("写 iris_ddgi_v2.ppm");
    println!("iris_ddgi_v2.ppm 已写出（{} 字节）", ppm.len());
    for (name, h) in [
        "关（env + ambient 全额）",
        "R89 开（羽化）",
        "深度项关（漏光对照）",
        "ambient 去双计对照",
    ]
    .iter()
    .zip(hashes)
    {
        println!("{name} FNV-1a = {h:#018x}");
    }
}
