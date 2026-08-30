//! 示例：旋转立方体 + 地面 + 悬浮球体，展示纹理、光照与飞行相机。
//!
//! 操作：WASD 移动，Space / 左Shift 升降，Ctrl 加速，滚轮变焦，Esc 退出。
//!
//! ```sh
//! cargo run --example cube --release
//! ```

use iris_engine::prelude::*;

fn main() {
    env_logger::init();
    App::new()
        .add_plugins(DefaultPlugins::new(
            "Iris · 旋转立方体（WASD 移动 / Space·Shift 升降 / 滚轮变焦 / Esc 退出）",
        ))
        .add_system(Stage::Startup, setup)
        .add_system(Stage::Update, spin)
        .add_system(Stage::Update, fly_camera)
        .add_system(Stage::Update, quit_on_escape)
        .run();
}

#[derive(Component)]
struct Spin(f32);

fn setup(world: &mut World) {
    // 场景资源：立方体 + 棋盘格纹理
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let checker = world
        .resource_mut::<TextureStore>()
        .add(CpuTexture::checkerboard(8, 16, [30, 32, 40, 255], [214, 92, 40, 255]));
    let mat_cube = world.resource_mut::<MaterialStore>().add(MaterialDef {
        texture: checker,
        metallic: 0.0,
        roughness: 0.25,
        ..Default::default()
    });
    world.spawn((Transform::from_xyz(0.0, 0.6, 0.0), cube, mat_cube, Spin(1.2)));

    // 地面
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(24.0));
    let mat_ground =
        world.resource_mut::<MaterialStore>().add(MaterialDef::solid(0.35, 0.38, 0.45, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_ground));

    // 悬浮球体：展示平滑法线与高光
    let sphere = world.resource_mut::<MeshStore>().add(MeshData::uv_sphere(0.45, 32, 20));
    let mat_sphere =
        world.resource_mut::<MaterialStore>().add(MaterialDef::solid(0.25, 0.65, 0.95, 1.0));
    world.spawn((Transform::from_xyz(1.9, 0.95, -0.7), sphere, mat_sphere));
}

fn spin(world: &mut World) {
    let dt = world.resource::<Time>().delta_secs();
    world.query::<(&mut Transform, &Spin)>(|(t, s)| {
        t.rotation = Quat::from_axis_angle(Vec3::Y, s.0 * dt) * t.rotation;
    });
}

fn fly_camera(world: &mut World) {
    // 先在只读借用中收集输入，再可变访问相机
    let (move_vec, wheel) = {
        let input = world.resource::<Input>();
        let dt = world.resource::<Time>().delta_secs();
        let mut mv = Vec3::ZERO;
        if input.key(KeyCode::KeyW) {
            mv.z -= 1.0;
        }
        if input.key(KeyCode::KeyS) {
            mv.z += 1.0;
        }
        if input.key(KeyCode::KeyA) {
            mv.x -= 1.0;
        }
        if input.key(KeyCode::KeyD) {
            mv.x += 1.0;
        }
        if input.key(KeyCode::Space) {
            mv.y += 1.0;
        }
        if input.key(KeyCode::ShiftLeft) {
            mv.y -= 1.0;
        }
        let boost = if input.key(KeyCode::ControlLeft) { 3.0 } else { 1.0 };
        (mv.normalize_or_zero() * 4.0 * boost * dt, input.wheel_delta())
    };
    let mut camera = world.resource_mut::<Camera>();
    camera.fly(move_vec);
    camera.zoom(wheel * -0.0015);
}

fn quit_on_escape(world: &mut World) {
    if world.resource::<Input>().key_pressed(KeyCode::Escape) {
        world.send_event(AppExit);
    }
}
