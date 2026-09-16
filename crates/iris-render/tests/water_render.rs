//! 集成测试：平面水体 v1（R81，`WaterSurface` + `shaders/water.wgsl`）。
//!
//! 四条主线（与 ARCHITECTURE §4.37 的验收标准一一对应）：
//! 1. **生效反证（硬约束）**：同场景有 / 无 `WaterSurface` 实体全帧
//!    FNV-1a **不同**（水体真实参与合成）；无水体帧 = 引入前路径本身
//!    （缺席 = 零捕获 pass、零额外 write_buffer，既有基线不受扰动的
//!    结构保证）；
//! 2. **镜面反射（有效性①）**：水面上方亮红竖柱，水面像素出现柱的
//!    倒影色——与无水体时同像素的差值可判别（倒影 = 镜像相机捕获的
//!    逐像素投影，R62a 的环绕反转由 cull None 吸收）；
//! 3. **深度吸收（有效性②）**：浅水采样点（有地面支撑、水下厚度
//!    小）vs 深水采样点（无地面——scene_depth = 远平面清空值 →
//!    Beer-Lambert 透过率 ≈ 0）——深水像素趋 `deep_color`（红），浅水
//!    趋 `shallow_color`（青绿），R−G 色差可判别（采样点几何写明，
//!    避开 Fresnel 天空反射与太阳镜面两个混淆源）；
//! 4. **确定性**：水体 + Time 推进的双会话逐帧逐位一致；改
//!    `wave_speed` 全帧哈希变化（防恒等假通过）。
//!
//! 环境无 GPU 适配器时优雅跳过（打印提示，测试仍算通过）。

use std::time::Duration;

use glam::{Vec2, Vec3};
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Time, Transform, World};
use iris_render::prelude::*;

/// 屏幕尺寸（测试统一口径）。
const SIZE: u32 = 256;

/// 本套件串行化（ssr / ao / sky_fog 同款纪律）：同文件多测试并行共用
/// 单适配器会挂死——文件内互斥锁把并发度压到 1。
static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ---- 场景公共件 ----

/// 倒影场景：深色地面（y = −1）+ 亮红竖柱（z = 2，y 0..4）。
/// 水面颜色压暗（反射是唯一亮源，排除水体本色的混淆）。
fn pillar_world(camera: &Camera, with_water: bool, wave_speed: f32) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(camera.clone());
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.3, -1.0, -0.2).normalize(),
        intensity: 1.2,
        ambient: Vec3::new(0.10, 0.11, 0.13),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    let mut ground_mat = MaterialDef::solid(0.04, 0.045, 0.05, 1.0);
    ground_mat.roughness = 0.95;
    let mut pillar_mat = MaterialDef::solid(0.9, 0.05, 0.05, 1.0);
    pillar_mat.roughness = 0.6;
    let ground_mesh = world
        .resource_mut::<MeshStore>()
        .add(MeshData::plane(400.0));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let ground = world.resource_mut::<MaterialStore>().add(ground_mat);
    let pillar = world.resource_mut::<MaterialStore>().add(pillar_mat);
    // 地面下沉 1 m（水面下方的水体本色 / 折射内容的来源）
    world.spawn((
        Transform {
            position: Vec3::new(0.0, -1.0, 0.0),
            ..Transform::IDENTITY
        },
        ground_mesh,
        ground,
    ));
    // 亮红竖柱：z = 2、跨 y 0..4（镜像相机捕获的主角）
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 2.0, 2.0),
            scale: Vec3::new(0.5, 4.0, 0.5),
            ..Transform::IDENTITY
        },
        cube,
        pillar,
    ));
    if with_water {
        // 水面压暗：反射成为唯一可判别亮源（排除水体本色混淆）
        world.spawn((WaterSurface {
            shallow_color: Vec3::new(0.02, 0.06, 0.05),
            deep_color: Vec3::new(0.005, 0.02, 0.03),
            wave_speed,
            ..WaterSurface::default()
        },));
    }
    propagate_transforms(&mut world);
    world
}

/// 深度吸收场景：地面只铺在 z ∈ [0, 16]（浅水半边），z < 0 的水面下是
/// 空域（scene_depth = 远平面清空值 → 透过率 ≈ 0 → 深水色）。
fn shore_world(camera: &Camera, with_water: bool, wave_speed: f32) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(camera.clone());
    // 直射光压低（隔离太阳镜面高光这个混淆源）+ 低环境光
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.3, -1.0, -0.2).normalize(),
        intensity: 0.2,
        ambient: Vec3::new(0.08, 0.09, 0.10),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    let mut ground_mat = MaterialDef::solid(0.5, 0.48, 0.45, 1.0);
    ground_mat.roughness = 0.95;
    let ground_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(1.0));
    let ground = world.resource_mut::<MaterialStore>().add(ground_mat);
    // 浅水半边的地面：x ∈ [−12, 12]、z ∈ [0, 16]、y = 0
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 0.0, 8.0),
            scale: Vec3::new(24.0, 1.0, 16.0),
            ..Transform::IDENTITY
        },
        ground_mesh,
        ground,
    ));
    if with_water {
        world.spawn((WaterSurface {
            height: 0.5,
            half_extent: Vec2::new(12.0, 12.0),
            // 深水 = 亮红、浅水 = 青绿：深度吸收方向用「色相翻转」判别
            //（比亮度差更有判别度——R−G 的符号翻转）
            shallow_color: Vec3::new(0.05, 0.50, 0.40),
            deep_color: Vec3::new(0.90, 0.03, 0.03),
            wave_speed,
            ..WaterSurface::default()
        },));
    }
    propagate_transforms(&mut world);
    world
}

/// 推过 Time 的世界（波形时间确定可复现——渲染器读 `elapsed_secs`）。
fn with_advanced_time(mut world: World, frames: u32, dt: Duration) -> World {
    let mut time = Time::default();
    for _ in 0..frames {
        time.advance(dt);
    }
    world.insert_resource(time);
    world
}

fn pixel(px: &[u8], w: u32, x: u32, y: u32) -> [i32; 3] {
    let i = ((y * w + x) * 4) as usize;
    [px[i] as i32, px[i + 1] as i32, px[i + 2] as i32]
}

/// 全帧像素 FNV-1a 64（兼容锁哈希，与既有 GPU 测试同款）。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// 世界坐标 → 像素坐标（与渲染器同一条相机路径；NDC y = +1 对应第 0 行）。
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

fn sample(px: &[u8], camera: &Camera, at: Vec3) -> [i32; 3] {
    let (fx, fy) = project_to_pixel(camera, at, SIZE, SIZE);
    pixel(px, SIZE, fx as u32, fy as u32)
}

/// 渲染 `frames` 帧并读回最后一帧（固定帧数口径：跨会话比较时两侧
/// 帧序一致——渲染器有跨帧状态如实例增量上传镜像）。
fn render_frames(renderer: &mut OffscreenRenderer, world: &World, frames: u32) -> Vec<u8> {
    for _ in 0..frames {
        renderer.render(world);
    }
    renderer.read_pixels()
}

// ---- 1. 生效反证：有 / 无水体全帧不同 ----

#[test]
fn water_presence_changes_frame() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let camera = Camera {
        eye: Vec3::new(0.0, 6.0, 10.0),
        target: Vec3::new(0.0, 0.0, 0.0),
        fov_y_radians: 60f32.to_radians(),
        ..Camera::default()
    };
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let world_off = pillar_world(&camera, false, 1.2);
    let off = render_frames(&mut renderer, &world_off, 2);
    let world_on = with_advanced_time(
        pillar_world(&camera, true, 1.2),
        3,
        Duration::from_millis(33),
    );
    let on = render_frames(&mut renderer, &world_on, 2);
    let h_off = fnv1a(&off);
    let h_on = fnv1a(&on);
    println!("水体关 FNV = {h_off:#018x}，水体开 FNV = {h_on:#018x}");
    assert_ne!(h_off, h_on, "水体实体必须改变全帧（生效反证）");
}

// ---- 2. 镜面反射：亮红竖柱在水面出现倒影色 ----

#[test]
fn water_reflects_pillar_on_surface() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    // 低机位掠射角：Fresnel 权重放大倒影（掠射 cosθ ≈ 0.27 → F ≈ 0.23，
    // 俯角越平镜面越亮——与物理直觉一致）
    let camera = Camera {
        eye: Vec3::new(0.0, 0.8, 12.0),
        target: Vec3::new(0.0, 1.5, 2.0),
        fov_y_radians: 60f32.to_radians(),
        ..Camera::default()
    };
    // 采样点 = 镜像几何的解析落点：柱中段 (0, 2, 2) 关于 y=0 的镜像
    // (0, −2, 2)，眼点 (0, 0.8, 12) 到镜像点的连线在 y=0 处交于
    // (0, 0, 9.14)——该水面像素的反射捕获恰好采到柱身
    let refl_point = Vec3::new(0.0, 0.0, 9.14);
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let world_off = pillar_world(&camera, false, 1.2);
    let off = render_frames(&mut renderer, &world_off, 2);
    let world_on = with_advanced_time(
        pillar_world(&camera, true, 1.2),
        3,
        Duration::from_millis(33),
    );
    let on = render_frames(&mut renderer, &world_on, 2);

    let p_off = sample(&off, &camera, refl_point);
    let p_on = sample(&on, &camera, refl_point);
    let rg_off = p_off[0] - p_off[1];
    let rg_on = p_on[0] - p_on[1];
    println!("倒影采样：无水 {p_off:?}（R−G={rg_off}），有水 {p_on:?}（R−G={rg_on}）");
    assert!(rg_on >= 40, "水面倒影像素必须红主导（柱色）：R−G = {rg_on}");
    assert!(
        rg_on - rg_off >= 30,
        "有水与无水的倒影差必须可判别：Δ(R−G) = {}",
        rg_on - rg_off
    );
}

// ---- 3. 深度吸收：深水趋 deep_color、浅水趋 shallow_color ----

#[test]
fn water_depth_absorption_shallow_vs_deep() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let camera = Camera {
        eye: Vec3::new(0.0, 10.0, 6.0),
        target: Vec3::new(0.0, 0.0, -2.0),
        fov_y_radians: 60f32.to_radians(),
        ..Camera::default()
    };
    // 浅水点：z = 4，地面（y=0）在其正下方 → 水下厚度 ≈ 0.5 m / sinθ
    // ≈ 0.6 m → 透过率 exp(−0.6/2.5) ≈ 0.79 → 体色以 shallow 为主；
    // 深水点：z = −8，地面只铺到 z ≥ 0 → 该像素 scene_depth = 远平面
    // 清空值 → 厚度巨大 → 透过率 ≈ 0 → 体色 = deep_color（红）
    let shallow_point = Vec3::new(0.0, 0.5, 4.0);
    let deep_point = Vec3::new(0.0, 0.5, -8.0);
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let world_off = shore_world(&camera, false, 1.2);
    let off = render_frames(&mut renderer, &world_off, 2);
    let world_on = with_advanced_time(
        shore_world(&camera, true, 1.2),
        3,
        Duration::from_millis(33),
    );
    let on = render_frames(&mut renderer, &world_on, 2);

    let shallow_p = sample(&on, &camera, shallow_point);
    let deep_p = sample(&on, &camera, deep_point);
    let deep_off_p = sample(&off, &camera, deep_point);
    let rg_shallow = shallow_p[0] - shallow_p[1];
    let rg_deep = deep_p[0] - deep_p[1];
    println!(
        "吸收采样：浅水 {shallow_p:?}（R−G={rg_shallow}），深水 {deep_p:?}（R−G={rg_deep}），深水无水体 {deep_off_p:?}"
    );
    assert!(
        rg_deep >= 90,
        "深水必须趋 deep_color（红主导）：R−G = {rg_deep}"
    );
    assert!(
        rg_deep - rg_shallow >= 90,
        "深浅水的色相差必须可判别：Δ(R−G) = {}",
        rg_deep - rg_shallow
    );
    assert_ne!(
        fnv1a(&off),
        fnv1a(&on),
        "水体开关必须改变全帧（吸收测试的反证面）"
    );
}

// ---- 4. 确定性：双会话逐位 + wave_speed 改变哈希 ----

#[test]
fn water_double_session_bitwise_and_speed_changes_hash() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let camera = Camera {
        eye: Vec3::new(0.0, 6.0, 10.0),
        target: Vec3::new(0.0, 0.0, 0.0),
        fov_y_radians: 60f32.to_radians(),
        ..Camera::default()
    };
    let Some(mut renderer_a) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut renderer_b) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    // 双会话同一世界构造序列（Time 推进 3 × 33 ms——波形时间确定）
    let build = |speed: f32| {
        with_advanced_time(
            pillar_world(&camera, true, speed),
            3,
            Duration::from_millis(33),
        )
    };
    let wa = build(1.2);
    let wb = build(1.2);
    let ha = fnv1a(&render_frames(&mut renderer_a, &wa, 2));
    let hb = fnv1a(&render_frames(&mut renderer_b, &wb, 2));
    println!("会话 A FNV = {ha:#018x}，会话 B FNV = {hb:#018x}");
    assert_eq!(ha, hb, "双会话必须逐位一致（确定性）");

    // 改 wave_speed 必须改变全帧哈希（波形时间真实参与——防恒等假通过）
    let wc = build(0.4);
    let hc = fnv1a(&render_frames(&mut renderer_a, &wc, 2));
    assert_ne!(hc, ha, "wave_speed 变化必须改变全帧哈希");
}
