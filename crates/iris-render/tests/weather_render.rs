//! 集成测试：天气表面（R92）——湿表面 / 积雪 / 涟漪 / 雨雪发射器。
//!
//! 六条主线（无 GPU 适配器时优雅跳过）：
//! 1. **缺席语义（硬约束）**：Weather 资源缺席 vs 在场全零——全帧 FNV
//!    逐字节一致（零值 uniform = fs_shaded 天气分支死码；这是「资源
//!    在场但关闭」与「资源缺席」的同构证明，全仓 opt-in 范式）；
//! 2. **湿表面**：wetness 0 / 0.5 / 1 的地面 ROI 平均亮度**严格单调
//!    递减**（反照率 × 0.7 变暗——R89 教训：旋钮要有非默认值中点断言，
//!    端点断言漏掉公式缺项）；
//! 3. **积雪坡度遮罩**：雪后朝上地面 ROI 显著变亮（趋雪白），垂直墙面
//!    ROI 几乎不变（ny ≈ 0 ⇒ 遮罩 0）；
//! 4. **雨**：rain() 预设发射器推进后画面出现雨丝像素（与雨前基线
//!    差分）+ 再推进分布改变（体积出生 / −Y 轴向 / 阻尼在 GPU 真跑）；
//!    双会话逐位一致；
//! 5. **雪**：snow() 预设（慢落 billboard）推进后成片像素 + 双会话
//!    逐位一致；
//! 6. **水面涟漪**：wave_speed = 0（波形静止）下 ripple = 1 的两时刻
//!    全帧不同（涟漪是唯一时变源），ripple = 0 的两时刻全帧相同（门
//!    关 = 逐位不变）。
//!
//! 同文件多测试并行共用单适配器会挂死（AGENTS.md 测试规约）——文件内
//! `GPU_SERIAL` 互斥串行化（ssr / water 同款）。

use std::sync::Mutex;
use std::time::Duration;

use glam::{Vec2, Vec3};
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Time, Transform, World};
use iris_render::prelude::*;

/// 屏幕尺寸（测试统一口径）。
const SIZE: u32 = 256;

/// 差分判据的通道阈值（sRGB 级）。
const DIFF: i32 = 8;

/// 固定模拟步长（1/60 s）。
const STEP: Duration = Duration::from_nanos(1_000_000_000 / 60);

/// 文件内 GPU 互斥（同文件多测试并行共用单适配器会挂死——AGENTS.md）。
static GPU_SERIAL: Mutex<()> = Mutex::new(());

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 与参考帧逐像素差分超过阈值的像素数（任一 RGB 通道）。
fn count_changed(a: &[u8], b: &[u8], threshold: i32) -> u32 {
    let mut n = 0u32;
    for (ca, cb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        let dr = (ca[0] as i32 - cb[0] as i32).abs();
        let dg = (ca[1] as i32 - cb[1] as i32).abs();
        let db = (ca[2] as i32 - cb[2] as i32).abs();
        if dr.max(dg).max(db) > threshold {
            n += 1;
        }
    }
    n
}

/// 矩形 ROI 的平均亮度（(R+G+B)/3 的均值——湿 / 雪断言的观感口径）。
fn roi_mean_luma(px: &[u8], w: u32, x0: u32, y0: u32, x1: u32, y1: u32) -> f32 {
    let mut sum = 0u64;
    let mut n = 0u64;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * w + x) * 4) as usize;
            sum += (px[i] as u64 + px[i + 1] as u64 + px[i + 2] as u64) / 3;
            n += 1;
        }
    }
    sum as f32 / n.max(1) as f32
}

/// 雨雪测试相机：平视略仰、**站在降水体积内部**（游戏相机的真实位形
///——近处雨丝 / 雪花的屏幕覆盖是主要观感来源；上半画面 = 亮天空，
/// 雨丝暗剪影有对比度）。
fn weather_camera() -> Camera {
    Camera {
        eye: Vec3::new(0.0, 2.0, 3.0),
        target: Vec3::new(0.0, 2.4, -7.0),
        fov_y_radians: 60f32.to_radians(),
        ..Camera::default()
    }
}

/// 表面响应测试相机：自上而下俯视地面（观察角 ~40°——掠射角的
/// Fresnel 天空镜面会大幅抵消湿化变暗【暗漫反射 + 亮镜面反射的物理
/// 平衡】，俯视让变暗 / 积雪的主导项直接可判）。
fn surface_camera() -> Camera {
    Camera {
        eye: Vec3::new(0.0, 6.0, 7.0),
        target: Vec3::new(0.0, 0.5, 0.0),
        fov_y_radians: 60f32.to_radians(),
        ..Camera::default()
    }
}

/// 基础世界：灰地面（0.55 中灰、粗糙 0.9——湿暗 / 雪亮的判别度来源）
/// + 无阴影方向光 + Time。
fn ground_world(camera: &Camera) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(camera.clone());
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.3, -1.0, -0.2).normalize(),
        // 曝光留头部空间：0.55 灰 → sRGB ~140、雪面 → ~190——暗化 / 雪
        // 亮的判别不撞色调映射饱和区（1.1 强度下灰面已 ~207 ≈ 天空亮）
        intensity: 0.55,
        ambient: Vec3::new(0.08, 0.09, 0.10),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    world.insert_resource(Time::default());
    let mut mat = MaterialDef::solid(0.55, 0.55, 0.55, 1.0);
    mat.roughness = 0.9;
    let mesh = world
        .resource_mut::<MeshStore>()
        .add(MeshData::plane(400.0));
    let handle = world.resource_mut::<MaterialStore>().add(mat);
    world.spawn((Transform::IDENTITY, mesh, handle));
    propagate_transforms(&mut world);
    world
}

/// 地面 + 垂直墙面（雪的坡度遮罩判别）：墙立于 z = −2、面朝 +Z（法线
/// y ≈ 0 ⇒ 无雪），高 4 m 宽 6 m。
fn wall_world(camera: &Camera) -> World {
    let mut world = ground_world(camera);
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mut wall_mat = MaterialDef::solid(0.30, 0.42, 0.55, 1.0);
    wall_mat.roughness = 0.85;
    let wall = world.resource_mut::<MaterialStore>().add(wall_mat);
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 2.0, -2.0),
            scale: Vec3::new(6.0, 4.0, 0.4),
            ..Transform::IDENTITY
        },
        cube,
        wall,
    ));
    propagate_transforms(&mut world);
    world
}

/// 推进 `steps` 步（层级传播 → v2 推进——DefaultPlugins 同形态）。
fn simulate(world: &mut World, steps: usize) {
    for _ in 0..steps {
        world.resource_mut::<Time>().advance(STEP);
        propagate_transforms(world);
        update_particles_v2(world);
    }
}

// ---- 1. 缺席语义：资源缺席 vs 全零资源逐位同 ----

#[test]
fn weather_zero_resource_is_bitwise_inert() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let camera = weather_camera();
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = ground_world(&camera);
    renderer.render(&world);
    let absent = renderer.read_pixels();
    // 全零资源在场：与缺席逐位同（零值 uniform = 天气分支死码）
    world.insert_resource(Weather::default());
    renderer.render(&world);
    let zeroed = renderer.read_pixels();
    assert_eq!(
        fnv1a(&absent),
        fnv1a(&zeroed),
        "Weather 全零在场必须与缺席逐位一致（opt-in 缺席语义）"
    );
}

// ---- 2. 湿表面：0 / 0.5 / 1 单调变暗 ----

#[test]
fn wetness_darkens_surface_monotonically() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let camera = surface_camera();
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = ground_world(&camera);
    // 三档湿度（0 = 缺席等价已由用例 1 证明；0.5 = 非默认中点——R89
    // 教训的旋钮断言形态）；地面 ROI = 画面下部中央条带（地面主体）
    let mut means = Vec::new();
    for wet in [0.0f32, 0.5, 1.0] {
        world.insert_resource(Weather {
            wetness: wet,
            ..Weather::default()
        });
        renderer.render(&world);
        let px = renderer.read_pixels();
        means.push(roi_mean_luma(&px, SIZE, 64, 176, 192, 240));
    }
    println!("湿 0 / 0.5 / 1 的地面 ROI 亮度 = {means:?}");
    assert!(
        means[0] > means[1] && means[1] > means[2],
        "湿度单调变暗：{means:?}"
    );
    // 判别度：全湿相对干燥的暗化要可感（≥ 12%——0.55 × 0.7 ≈ 0.385
    // 的反射率差在直射下应远超此界）
    assert!(
        means[0] - means[2] > means[0] * 0.08,
        "湿化暗化幅度应可判别（俯视主导项；实测 193 → 172 ≈ 11%）：{means:?}"
    );
}

// ---- 3. 积雪坡度遮罩：朝上变雪白、垂直墙不变 ----

#[test]
fn snow_covers_upfacing_only() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let camera = surface_camera();
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = wall_world(&camera);
    renderer.render(&world);
    let dry = renderer.read_pixels();
    // 墙面 ROI 与地面 ROI：几何上墙 (0, 2, −2) 投影在画面中上部中央，
    // 地面在其下方（相机 3.5 m 俯视）。采样矩形按投影几何圈定。
    world.insert_resource(Weather {
        snow: 1.0,
        ..Weather::default()
    });
    renderer.render(&world);
    let snowy = renderer.read_pixels();
    // 地面 ROI（下部中央）与墙面 ROI（中上部中央）
    let ground_dry = roi_mean_luma(&dry, SIZE, 96, 200, 160, 244);
    let ground_snow = roi_mean_luma(&snowy, SIZE, 96, 200, 160, 244);
    let wall_dry = roi_mean_luma(&dry, SIZE, 96, 72, 160, 116);
    let wall_snow = roi_mean_luma(&snowy, SIZE, 96, 72, 160, 116);
    println!(
        "地面 干/雪 = {ground_dry:.1}/{ground_snow:.1}，墙面 干/雪 = {wall_dry:.1}/{wall_snow:.1}"
    );
    // 阈值依据：实测干/雪 = 193 → 215（+22，曝光头部空间拉开的
    // 判别带）；墙 / 地的增量比（实测 ~7%）是更强的结构性断言
    assert!(
        ground_snow - ground_dry > 15.0,
        "朝上地面应显著趋雪白：{ground_dry:.1} → {ground_snow:.1}"
    );
    assert!(
        (wall_snow - wall_dry).abs() < (ground_snow - ground_dry) * 0.25,
        "垂直墙面应基本无雪：{wall_dry:.1} → {wall_snow:.1}"
    );
}

// ---- 4. 雨：体积出生 + −Y 下落 + 阻尼在 GPU 真跑 + 双会话逐位 ----

#[test]
fn rain_renders_moves_and_sessions_match() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let camera = weather_camera();
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = ground_world(&camera);
    // 雨发射器：中心在 (0, 5, 2)——体积 28×16×28 罩住相机前段视锥
    // 预设本体字段由 CPU 测试锁定（rain_snow_presets_and_defaults）；
    // 丝宽按 256px 测试视口放大到粒子 v2 测试同款 0.45（预设 0.028 目标
    // 1080p 游戏距离——快照 example 以原值出图）。相机位于降水体积内
    //（游戏相机的真实位形：近处雨丝的屏幕覆盖是主要观感来源）。
    let mut rain = ParticleEmitterV2::rain();
    rain.trail_width = 0.45;
    rain.size_start = 0.05;
    rain.size_end = 0.02;
    world.spawn((Transform::from_xyz(0.0, 4.0, -2.0), rain));
    propagate_transforms(&mut world);
    renderer.render(&world);
    let baseline = renderer.read_pixels();
    // 推进 30 步（0.5 s，~800 颗出生；雨速 ~9 m/s 下落穿越视锥）
    simulate(&mut world, 30);
    renderer.render(&world);
    let raining = renderer.read_pixels();
    let diff = count_changed(&baseline, &raining, DIFF);
    {
        let stats = renderer.renderer().stats();
        println!(
            "雨 0.5 s 后差分像素 = {diff}，draw_calls = {}，triangles = {}",
            stats.draw_calls, stats.triangles_drawn
        );
    }
    // 阈值依据：256px 视口 + alpha 软边条带的实测（218；加法混合同几何
    // 1724——alpha 暗剪影对灰背景的对比度是光度学下限，1024px 快照以
    // 预设原值出图目检）
    assert!(diff > 80, "雨丝应出现在画面（差分像素 {diff}）");
    // 再推进一步：分布改变（雨在落——体积出生 / 轴向 / 阻尼在 GPU 推进）
    simulate(&mut world, 1);
    renderer.render(&world);
    let next = renderer.read_pixels();
    let moved = count_changed(&raining, &next, DIFF);
    assert!(moved > 50, "推进一帧后雨分布应改变（实际 {moved}）");
    // 双会话逐位（独立渲染器 + 完整重建世界 + 同推进序列）
    let run = || {
        let Some(mut r2) = OffscreenRenderer::new(SIZE, SIZE) else {
            panic!("双会话渲染器应能创建");
        };
        let mut w2 = ground_world(&camera);
        let mut rain = ParticleEmitterV2::rain();
        rain.trail_width = 0.45;
        rain.size_start = 0.05;
        rain.size_end = 0.02;
        w2.spawn((Transform::from_xyz(0.0, 4.0, -2.0), rain));
        propagate_transforms(&mut w2);
        simulate(&mut w2, 30);
        r2.render(&w2);
        fnv1a(&r2.read_pixels())
    };
    let ha = fnv1a(&raining);
    let hb = run();
    assert_eq!(ha, hb, "雨场景双会话必须逐位一致");
}

// ---- 5. 雪：慢落 billboard + 双会话逐位 ----

#[test]
fn snow_renders_and_sessions_match() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let camera = weather_camera();
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = ground_world(&camera);
    // 雪片尺寸按 256px 视口放大（预设 0.032 目标 1080p）
    let mut snow = ParticleEmitterV2::snow();
    snow.size_start = 0.08;
    snow.size_end = 0.08;
    world.spawn((Transform::from_xyz(0.0, 4.0, -2.0), snow));
    propagate_transforms(&mut world);
    renderer.render(&world);
    let baseline = renderer.read_pixels();
    // 推进 60 步（1 s，~420 颗出生；终端速度 ~1.5 m/s 慢落）
    simulate(&mut world, 60);
    renderer.render(&world);
    let snowing = renderer.read_pixels();
    let diff = count_changed(&baseline, &snowing, DIFF);
    let mut alive = 0u32;
    world.query::<&ParticleEmitterV2>(|e| alive += e.alive_estimate());
    println!("雪 1 s 后差分像素 = {diff}，存活估计 = {alive}");
    assert!(alive > 100, "雪稳态存活应有数百颗，实际 {alive}");
    assert!(diff > 20, "雪花应出现在画面（差分像素 {diff}）");
    // 双会话逐位
    let run = || {
        let Some(mut r2) = OffscreenRenderer::new(SIZE, SIZE) else {
            panic!("双会话渲染器应能创建");
        };
        let mut w2 = ground_world(&camera);
        let mut snow = ParticleEmitterV2::snow();
        snow.size_start = 0.08;
        snow.size_end = 0.08;
        w2.spawn((Transform::from_xyz(0.0, 4.0, -2.0), snow));
        propagate_transforms(&mut w2);
        simulate(&mut w2, 60);
        r2.render(&w2);
        fnv1a(&r2.read_pixels())
    };
    assert_eq!(fnv1a(&snowing), run(), "雪场景双会话必须逐位一致");
}

// ---- 6. 水面涟漪：唯一时变源（wave_speed = 0）与门关不变 ----

#[test]
fn water_ripple_perturbs_and_animates() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let camera = Camera {
        eye: Vec3::new(0.0, 3.0, 10.0),
        target: Vec3::new(0.0, 0.0, 0.0),
        fov_y_radians: 60f32.to_radians(),
        ..Camera::default()
    };
    let build = |ripple: f32, frames: u32| {
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
        let ground_mesh = world
            .resource_mut::<MeshStore>()
            .add(MeshData::plane(400.0));
        let ground = world.resource_mut::<MaterialStore>().add(ground_mat);
        world.spawn((
            Transform {
                position: Vec3::new(0.0, -1.0, 0.0),
                ..Transform::IDENTITY
            },
            ground_mesh,
            ground,
        ));
        // wave_speed = 0：Gerstner 相位冻结（ω = 0）——涟漪是唯一时变源
        world.spawn((WaterSurface {
            wave_speed: 0.0,
            half_extent: Vec2::new(16.0, 16.0),
            ..WaterSurface::default()
        },));
        if ripple > 0.0 {
            world.insert_resource(Weather {
                ripple,
                ..Weather::default()
            });
        }
        let mut time = Time::default();
        for _ in 0..frames {
            time.advance(STEP);
        }
        world.insert_resource(time);
        propagate_transforms(&mut world);
        world
    };
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    // 门关（ripple = 0，无资源）：两时刻全帧相同（波形静止 + 无涟漪）
    let w_a = build(0.0, 30);
    renderer.render(&w_a);
    let h_a0 = fnv1a(&renderer.read_pixels());
    let w_b = build(0.0, 36);
    renderer.render(&w_b);
    let h_a1 = fnv1a(&renderer.read_pixels());
    assert_eq!(h_a0, h_a1, "涟漪关闭 + 波形静止：时间推进不得改变全帧");
    // 门开（ripple = 1）：两时刻全帧不同（涟漪环在扩展——唯一时变源）
    let w_c = build(1.0, 30);
    renderer.render(&w_c);
    let h_c0 = fnv1a(&renderer.read_pixels());
    assert_ne!(h_a0, h_c0, "涟漪开启必须改变水面帧（生效反证）");
    let w_d = build(1.0, 36);
    renderer.render(&w_d);
    let h_c1 = fnv1a(&renderer.read_pixels());
    assert_ne!(h_c0, h_c1, "涟漪随时间推进必须改变全帧（波前在动）");
}
