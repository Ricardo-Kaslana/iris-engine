//! 集成测试：前向光源分箱 + SpotLight（R78，`LightBinningSettings` +
//! 灯表 binding 15/16/17 + `standard.wgsl` 分箱分支）。
//!
//! 四条主线（与 ARCHITECTURE §4.35 的验收标准一一对应）：
//! 1. **双路径位等价（硬约束）**：≤ 8 盏点光场景（含 1 盏投影阴影灯）
//!    `LightBinningSettings { force: true }`（mode = 1，分箱路径）与资源
//!    缺席（mode = 0，Globals 均摊路径）两次渲染 read_pixels **逐字节
//!    相等**——逐灯贡献函数共用 + tile 覆盖保守 + 灯表前段阴影字段
//!    逐字段镜像的联合实证；
//! 2. **第 9 盏点光生效**：8 盏均布点光（太阳强度 0、环境光近零隔离）
//!    的暗角基线 vs 加第 9 盏照向同一暗角——暗角采样点亮度显著上升、
//!    9 盏照不到的远点几乎不变（自动阈值 point_count > 8 生效，可判别）；
//! 3. **锥光形状**：单 SpotLight 斜轴下照，锥内采样点显著亮于锥外
//!    **同灯距**点（等距消衰减混淆、同平面法线消 NdotL 混淆），
//!    `force: true` 下同断言复跑（覆盖新路径的 spot 求值）；
//! 4. **确定性**：同场景双会话逐字节一致；其中一轮移动 SpotLight 位置
//!    → 全帧哈希改变（防「恒黑假通过」）。
//!
//! 环境无 GPU 适配器时优雅跳过（打印提示，测试仍算通过）。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 屏幕尺寸（测试统一口径；256² / 32 = 8×8 tiles）。
const SIZE: u32 = 256;

/// 本套件串行化（ssr_render 同款纪律）：同文件多测试并行共用单适配器
/// 会挂死——文件内互斥锁把并发度压到 1。
static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 公共相机（高角度俯视，角落与中部地面都在画面内）。
fn scene_camera() -> Camera {
    Camera {
        eye: Vec3::new(2.4, 2.6, 2.8),
        target: Vec3::new(-1.2, 0.4, -1.2),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    }
}

/// 地板世界：单面地板 + 近零环境光 + 零直射光 + **IBL 强度 0**（隔离
/// 点光/锥光信号——程序化天光的间接光会淹没采样点差异）。
fn floor_world() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(scene_camera());
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.4, -1.0, -0.3).normalize(),
        intensity: 0.0,
        ambient: Vec3::new(0.02, 0.02, 0.025),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(EnvironmentSettings { intensity: 0.0 });
    world.insert_resource(BloomSettings::disabled());
    let mut solid = MaterialDef::solid(0.6, 0.6, 0.62, 1.0);
    solid.roughness = 0.9;
    let mat = world.resource_mut::<MaterialStore>().add(solid);
    let floor_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(12.0));
    world.spawn((Transform::IDENTITY, floor_mesh, mat));
    propagate_transforms(&mut world);
    world
}

/// 在世界里放一盏点光（GlobalTransform 定位）。
fn add_point_light(world: &mut World, position: Vec3, range: f32, intensity: f32, shadows: bool) {
    world.spawn((
        Transform {
            position,
            ..Transform::IDENTITY
        },
        PointLight {
            color: Vec3::ONE,
            intensity,
            range,
            shadows,
        },
    ));
}

/// 在世界里放一盏锥光。
fn add_spot_light(world: &mut World, position: Vec3, direction: Vec3, spot: SpotLight) {
    world.spawn((
        Transform {
            position,
            ..Transform::IDENTITY
        },
        SpotLight { direction, ..spot },
    ));
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

fn pixel(px: &[u8], w: u32, x: u32, y: u32) -> [i32; 3] {
    let i = ((y * w + x) * 4) as usize;
    [px[i] as i32, px[i + 1] as i32, px[i + 2] as i32]
}

fn luminance(p: [i32; 3]) -> i32 {
    p[0] + p[1] + p[2]
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

fn sample_luminance(px: &[u8], at: Vec3) -> i32 {
    let (fx, fy) = project_to_pixel(&scene_camera(), at, SIZE, SIZE);
    luminance(pixel(px, SIZE, fx as u32, fy as u32))
}

/// 暗角（8 盏聚簇点光的 range 并集之外）与远点（第 9 盏的 range 之外，
/// 但在 8 盏并集之内——两世界都亮，作不变对照）。
/// 8 盏 (x, 1.2, z) 到暗角 (−2.3, 0, −2.3) 最近 ≈ 4.1 m > 2.5（暗）；
/// 远点 (0.8, 0, 0.8) 到第 9 盏 (−1.7, 0.9, −1.7) 距离 ≈ 3.63 > 2.2（隔离）。
const DARK_CORNER: Vec3 = Vec3::new(-2.3, 0.0, -2.3);
const FAR_POINT: Vec3 = Vec3::new(0.8, 0.0, 0.8);

/// 8 盏点光聚在画面右/中部（暗角基线）：两排 (x, 1.2, z)、x ∈
/// {0.5, 1.5}、z ∈ {0.2, 1.2, 2.2, 3.2}——range 2.5 只覆盖画面右侧地面
/// （到暗角 (−2.3, 0, −2.3) 最近 ≈ 4.1 m，并集之外 ⇒ 暗）；第 9 盏挂在
/// (−1.7, 0.9, −1.7) 照亮暗角。
fn eight_lights_world() -> World {
    let mut world = floor_world();
    for x in [0.5f32, 1.5] {
        for z in [0.2f32, 1.2, 2.2, 3.2] {
            add_point_light(&mut world, Vec3::new(x, 1.2, z), 2.5, 12.0, false);
        }
    }
    // 灯是 floor_world 传播之后 spawn 的——必须再传播一次生成
    // GlobalTransform（查询按它定位，漏传播 = 灯全部失踪）
    propagate_transforms(&mut world);
    world
}

/// 双路径位等价：≤ 8 盏点光（含 1 盏投影阴影灯）+ 无锥光，force = true
/// （分箱路径）与资源缺席（Globals 均摊路径）必须逐字节相等。
#[test]
fn bins_force_bitwise_equals_globals_path() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let build = |force: bool| -> World {
        let mut world = floor_world();
        // 5 盏点光，其中 (0, 1.5, 0) 投影阴影（验证灯表前段阴影字段与
        // Globals 逐字段镜像；地板自遮挡的阴影在两条路径必须同样采样）
        add_point_light(&mut world, Vec3::new(-1.5, 1.5, -1.5), 4.0, 8.0, false);
        add_point_light(&mut world, Vec3::new(0.0, 1.5, 0.0), 4.0, 8.0, true);
        add_point_light(&mut world, Vec3::new(1.5, 1.5, 1.5), 4.0, 8.0, false);
        add_point_light(&mut world, Vec3::new(1.5, 1.5, -1.5), 4.0, 8.0, false);
        add_point_light(&mut world, Vec3::new(-1.5, 1.5, 1.5), 4.0, 8.0, false);
        // 灯是传播之后 spawn 的——再传播一次生成 GlobalTransform
        propagate_transforms(&mut world);
        if force {
            world.insert_resource(LightBinningSettings { force: true });
        }
        world
    };

    let Some(mut renderer_absent) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    renderer_absent.render(&build(false));
    let absent = fnv1a(&renderer_absent.read_pixels());

    let mut renderer_force = OffscreenRenderer::new(SIZE, SIZE).expect("force 渲染器");
    renderer_force.render(&build(true));
    let forced = fnv1a(&renderer_force.read_pixels());

    assert_eq!(
        absent, forced,
        "≤ 8 盏点光场景 force（分箱路径）必须与缺席（Globals 路径）逐位一致"
    );
}

/// 第 9 盏点光生效（自动阈值）：8 盏基线的暗角 vs 加第 9 盏后的暗角，
/// 亮度显著上升；9 盏 range 之外的远点几乎不变（可判别性）。
#[test]
fn ninth_point_light_brightens_dark_corner() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer_base) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let world_base = eight_lights_world();
    renderer_base.render(&world_base);
    let base = renderer_base.read_pixels();
    let mut world_ninth = eight_lights_world();
    add_point_light(
        &mut world_ninth,
        Vec3::new(-1.7, 0.9, -1.7),
        2.2,
        12.0,
        false,
    );
    // 第 9 盏是传播之后 spawn 的——再传播一次生成 GlobalTransform
    propagate_transforms(&mut world_ninth);
    let mut renderer_ninth = OffscreenRenderer::new(SIZE, SIZE).expect("第 9 盏渲染器");
    renderer_ninth.render(&world_ninth);
    let ninth = renderer_ninth.read_pixels();

    let corner_base = sample_luminance(&base, DARK_CORNER);
    let corner_ninth = sample_luminance(&ninth, DARK_CORNER);
    let far_base = sample_luminance(&base, FAR_POINT);
    let far_ninth = sample_luminance(&ninth, FAR_POINT);
    println!(
        "暗角 base={corner_base} ninth={corner_ninth}；远点 base={far_base} ninth={far_ninth}"
    );
    assert!(
        corner_ninth > corner_base + 24,
        "第 9 盏点光必须显著照亮暗角（base = {corner_base}，ninth = {corner_ninth}）"
    );
    assert!(
        (far_ninth - far_base).abs() <= 12,
        "第 9 盏 range 之外的远点必须几乎不变（base = {far_base}，ninth = {far_ninth}）"
    );
}

/// 锥光形状：斜轴下照的单 SpotLight，锥内点显著亮于锥外**同灯距**点。
/// 两点等灯距（衰减相同）、同平面（NdotL 相同），唯一差异 = 锥光因子；
/// `force: true` 复跑覆盖新路径的 spot 求值。
#[test]
fn spot_light_cone_shape_inside_beats_outside() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    // 锥轴向 -x 倾斜：P_in = (−1.6, 0, 0) 距轴角 ≈ 9°（< inner 25°，全强），
    // P_out = (1.6, 0, 0) 距轴角 ≈ 47°（> outer 40°，归零）；两点到灯
    // (0, 3, 0) 的距离同为 √(1.6² + 3²) ≈ 3.4 m。
    let build = |force: bool| -> World {
        let mut world = floor_world();
        add_spot_light(
            &mut world,
            Vec3::new(0.0, 3.0, 0.0),
            Vec3::new(-0.35, -1.0, 0.0).normalize(),
            SpotLight {
                color: Vec3::ONE,
                intensity: 60.0,
                range: 12.0,
                direction: Vec3::NEG_Y, // 被覆盖
                inner_cutoff: 25.0f32.to_radians(),
                outer_cutoff: 40.0f32.to_radians(),
            },
        );
        if force {
            world.insert_resource(LightBinningSettings { force: true });
        }
        // 锥光是传播之后 spawn 的——再传播一次生成 GlobalTransform
        propagate_transforms(&mut world);
        world
    };
    let inner_point = Vec3::new(-1.6, 0.0, 0.0);
    let outer_point = Vec3::new(1.6, 0.0, 0.0);
    for force in [false, true] {
        let world = build(force);
        renderer.render(&world);
        let px = renderer.read_pixels();
        let inside = sample_luminance(&px, inner_point);
        let outside = sample_luminance(&px, outer_point);
        println!("force={force} 锥内={inside} 锥外={outside}");
        assert!(
            inside > outside + 24,
            "force={force}：锥内点必须显著亮于锥外同灯距点（inside = {inside}，outside = {outside}）"
        );
    }
}

/// 确定性：同场景双会话逐字节一致；移动 SpotLight 位置 → 全帧哈希改变
/// （分箱路径的画面确实依赖锥光数据，防「恒黑假通过」）。
#[test]
fn bins_deterministic_and_moving_spot_changes_frame() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut a) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut b = OffscreenRenderer::new(SIZE, SIZE).expect("第二会话渲染器");
    let build = |spot_x: f32| -> World {
        let mut world = floor_world();
        add_spot_light(
            &mut world,
            Vec3::new(spot_x, 3.0, 0.0),
            Vec3::new(-0.35, -1.0, 0.0).normalize(),
            SpotLight {
                intensity: 60.0,
                ..SpotLight::default()
            },
        );
        // 锥光是传播之后 spawn 的——再传播一次生成 GlobalTransform
        propagate_transforms(&mut world);
        world
    };
    let wa = build(0.0);
    let wb = build(0.0);
    a.render(&wa);
    b.render(&wb);
    let ha = fnv1a(&a.read_pixels());
    let hb = fnv1a(&b.read_pixels());
    assert_eq!(ha, hb, "同输入两条会话必须逐位一致");

    // 移动锥光位置 → 画面必须改变
    let wc = build(1.5);
    a.render(&wc);
    let hc = fnv1a(&a.read_pixels());
    assert_ne!(hc, ha, "移动 SpotLight 后全帧哈希必须改变（防恒黑假通过）");
}
