//! 示例：第一人称鼠标捕获与原始鼠标视角。
//!
//! 左键请求光标捕获（web 上同时满足指针锁定的用户手势要求），捕获期间
//! 鼠标位移来自原始设备运动，视角无边界、指针隐藏；Esc 释放捕获，未
//! 捕获时再按 Esc 退出。浏览器可能自行退出指针锁定且不派发 Esc，此时
//! 左键重新捕获即可。
//!
//! 操作：左键捕获 / Esc 释放或退出；WASD 移动，Space / 左Shift 升降，
//! Ctrl 加速。
//!
//! ```sh
//! cargo run --example first_person --release
//! ```

use iris_engine::prelude::*;

/// 原始运动计数到弧度的换算系数。原始输入单位依设备而定，游戏应按
/// 实测手感调整。
const LOOK_SENSITIVITY: f32 = 0.0025;
const MOVE_SPEED: f32 = 6.0;
const PITCH_LIMIT: f32 = 89.0_f32.to_radians();

/// 视角状态（yaw 绕世界 Y 轴，0 朝 -Z；pitch 向上为正）。
#[derive(Default, Resource)]
struct Look {
    yaw: f32,
    pitch: f32,
}

fn main() {
    env_logger::init();
    App::new()
        .add_plugins(DefaultPlugins::new(
            "Iris · 第一人称（左键捕获 / Esc 释放再退出 / WASD 移动 / Space·Shift 升降）",
        ))
        .add_system(Stage::Startup, setup)
        .add_system(Stage::Update, toggle_capture)
        .add_system(Stage::Update, first_person_look)
        .add_system(Stage::Update, report_capture)
        .run();
}

fn setup(world: &mut World) {
    world.insert_resource(Look::default());
    world.insert_resource(CaptureReport::default());

    let ground = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let checker = world
        .resource_mut::<TextureStore>()
        .add(CpuTexture::checkerboard(
            8,
            24,
            [54, 58, 66, 255],
            [104, 112, 124, 255],
        ));
    let ground_mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
        texture: checker,
        roughness: 0.8,
        ..Default::default()
    });
    world.spawn((Transform::IDENTITY, ground, ground_mat));

    // 环形色块 + 中央高柱：转动视角与俯仰时提供明确参照。
    let palette = [
        [0.85, 0.35, 0.25],
        [0.25, 0.65, 0.9],
        [0.35, 0.8, 0.45],
        [0.9, 0.75, 0.3],
    ];
    for i in 0..8 {
        let angle = i as f32 * std::f32::consts::TAU / 8.0;
        let mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
        let [r, g, b] = palette[i % palette.len()];
        let material = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(r, g, b, 1.0));
        world.spawn((
            Transform::from_xyz(angle.cos() * 6.0, 0.5, angle.sin() * 6.0),
            mesh,
            material,
        ));
    }

    let pillar = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let pillar_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.7, 0.7, 0.72, 1.0));
    let mut transform = Transform::from_xyz(0.0, 30.0, -12.0);
    transform.scale = Vec3::new(1.0, 60.0, 1.0);
    world.spawn((transform, pillar, pillar_mat));
}

/// 左键捕获（已请求时重试，覆盖浏览器强制退出指针锁定）；Esc 释放，
/// 未捕获时退出。
fn toggle_capture(world: &mut World) {
    let (clicked, escape) = {
        let input = world.resource::<Input>();
        (
            input.mouse_button_pressed(MouseButton::Left),
            input.key_pressed(KeyCode::Escape),
        )
    };
    if clicked {
        let mut capture = world.resource_mut::<MouseCapture>();
        if capture.is_requested() {
            capture.retry();
        } else {
            capture.request(true);
        }
    }
    if escape {
        let was_requested = world.resource::<MouseCapture>().is_requested();
        if was_requested {
            world.resource_mut::<MouseCapture>().request(false);
        } else {
            world.send_event(AppExit);
        }
    }
}

/// 上一帧上报的捕获状态（控制台只打印状态变化）。
#[derive(Default, Resource)]
struct CaptureReport(bool);

/// 捕获状态变化时打印一行，便于确认平台抓取是否成功。
fn report_capture(world: &mut World) {
    let (active, mode) = {
        let capture = world.resource::<MouseCapture>();
        (capture.is_active(), capture.grab_mode())
    };
    let mut report = world.resource_mut::<CaptureReport>();
    if active != report.0 {
        report.0 = active;
        log::info!("鼠标捕获：{}（{mode:?}）", if active { "开" } else { "关" });
    }
}

/// 捕获期间读原始运动转视角；移动按 yaw 在地面平面内进行。
fn first_person_look(world: &mut World) {
    let dt = world.resource::<Time>().delta_secs();
    let ((dx, dy), captured) = {
        let input = world.resource::<Input>();
        (input.mouse_delta(), input.is_relative())
    };
    if captured {
        let mut look = world.resource_mut::<Look>();
        look.yaw -= dx as f32 * LOOK_SENSITIVITY;
        look.pitch = (look.pitch - dy as f32 * LOOK_SENSITIVITY).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }
    let (yaw, pitch) = {
        let look = world.resource::<Look>();
        (look.yaw, look.pitch)
    };
    let (move_dir, boost) = {
        let input = world.resource::<Input>();
        let forward = Vec3::new(-yaw.sin(), 0.0, -yaw.cos());
        let right = Vec3::new(yaw.cos(), 0.0, -yaw.sin());
        let mut dir = Vec3::ZERO;
        if input.key(KeyCode::KeyW) {
            dir += forward;
        }
        if input.key(KeyCode::KeyS) {
            dir -= forward;
        }
        if input.key(KeyCode::KeyD) {
            dir += right;
        }
        if input.key(KeyCode::KeyA) {
            dir -= right;
        }
        if input.key(KeyCode::Space) {
            dir += Vec3::Y;
        }
        if input.key(KeyCode::ShiftLeft) {
            dir -= Vec3::Y;
        }
        (
            dir.normalize_or_zero(),
            if input.key(KeyCode::ControlLeft) {
                3.0
            } else {
                1.0
            },
        )
    };

    let mut camera = world.resource_mut::<Camera>();
    camera.fly(move_dir * MOVE_SPEED * boost * dt);
    let forward = Vec3::new(-yaw.sin(), 0.0, -yaw.cos());
    let dir = Vec3::new(
        forward.x * pitch.cos(),
        pitch.sin(),
        forward.z * pitch.cos(),
    );
    camera.target = camera.eye + dir;
    camera.up = Vec3::Y;
}
