//! 示例：加载 glTF 资产（`assets/cube.gltf`，由 `gen_asset` 示例生成）。
//!
//! 展示完整资产链路：外部 .bin buffer → 顶点/索引/材质 → 实体树（层级）
//! → 渲染。加载失败时回退到程序化立方体（优雅降级）。
//!
//! ```sh
//! cargo run --example gen_asset --release
//! cargo run --example model --release
//! ```

use iris_engine::prelude::*;

fn main() {
    env_logger::init();
    App::new()
        .add_plugins(DefaultPlugins::new("Iris · glTF 资产加载（Esc 退出）"))
        .add_system(Stage::Startup, setup)
        .add_system(Stage::Update, spin_model)
        .add_system(Stage::Update, quit_on_escape)
        .run();
}

#[derive(Component)]
struct Spin(f32);

fn setup(world: &mut World) {
    match spawn_gltf(world, "assets/cube.gltf") {
        Ok(roots) => {
            println!("glTF 加载成功：{} 个顶层实体", roots.len());
            for root in roots {
                world.insert(root, Spin(0.8));
                if let Some(mut transform) = world.get_mut::<Transform>(root) {
                    transform.position.y = 0.6;
                }
            }
        }
        Err(e) => {
            log::error!("加载 assets/cube.gltf 失败：{e}，回退到程序化立方体");
            let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
            let mat = world
                .resource_mut::<MaterialStore>()
                .add(MaterialDef::solid(0.92, 0.48, 0.18, 1.0));
            world.spawn((Transform::from_xyz(0.0, 0.6, 0.0), cube, mat, Spin(0.8)));
        }
    }

    // 地面 + 点光点缀
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(20.0));
    let mat_ground = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.3, 0.33, 0.4, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_ground));
    world.spawn((
        Transform::from_xyz(2.0, 1.5, 2.0),
        PointLight::new(Vec3::ONE, 10.0, 10.0),
    ));
}

fn spin_model(world: &mut World) {
    let dt = world.resource::<Time>().delta_secs();
    world.query::<(&mut Transform, &Spin)>(|(t, s)| {
        t.rotation = Quat::from_axis_angle(Vec3::Y, s.0 * dt) * t.rotation;
    });
}

fn quit_on_escape(world: &mut World) {
    if world.resource::<Input>().key_pressed(KeyCode::Escape) {
        world.send_event(AppExit);
    }
}
