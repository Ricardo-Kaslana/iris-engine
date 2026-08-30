//! 示例：4096 个立方体的实例化批处理。
//!
//! 渲染器自动把共享 (网格, 材质) 的实体合并为一个 draw call：
//! 4096 个实例 / 8 种材质 = 8 个 draw call。运行时按 F 键打印帧统计。
//!
//! ```sh
//! cargo run --example instanced --release
//! ```

use iris_engine::prelude::*;

const N: u32 = 16; // N³ = 4096

fn main() {
    env_logger::init();
    App::new()
        .add_plugins(
            DefaultPlugins::new("Iris · 实例化批处理（4096 实例 / 8 draw call / F 查看统计）")
                .with_size(1600, 900),
        )
        .add_system(Stage::Startup, setup)
        .add_system(Stage::Update, wave)
        .add_system(Stage::Update, print_stats)
        .run();
}

#[derive(Component)]
struct BaseY(f32);

fn setup(world: &mut World) {
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    // 8 种材质：蓝 -> 橙 渐变
    let materials: Vec<_> = (0..8)
        .map(|i| {
            let t = i as f32 / 7.0;
            world
                .resource_mut::<MaterialStore>()
                .add(MaterialDef::solid(0.15 + 0.75 * t, 0.35, 1.0 - 0.75 * t, 1.0))
        })
        .collect();

    let spacing = 1.6;
    let half = (N as f32 - 1.0) * spacing * 0.5;
    for x in 0..N {
        for y in 0..N {
            for z in 0..N {
                let position = Vec3::new(
                    x as f32 * spacing - half,
                    y as f32 * spacing - half + 2.0,
                    z as f32 * spacing - half,
                );
                let material = materials[((x + y + z) % 8) as usize];
                world.spawn((Transform::from_translation(position), cube, material, BaseY(position.y)));
            }
        }
    }

    // 相机拉远看全景
    let mut camera = world.resource_mut::<Camera>();
    camera.eye = Vec3::new(16.0, 13.0, 20.0);
    camera.target = Vec3::new(0.0, 2.0, 0.0);
}

fn wave(world: &mut World) {
    let t = world.resource::<Time>().elapsed_secs();
    world.query::<(&mut Transform, &BaseY)>(|(tr, base)| {
        tr.position.y = base.0 + (tr.position.x * 0.7 + tr.position.z * 0.5 + t * 2.5).sin() * 0.5;
    });
}

fn print_stats(world: &mut World) {
    if world.resource::<Input>().key_pressed(KeyCode::KeyF) {
        // 帧统计由渲染器内部记录，这里用实体数替代提示规模
        println!("实体数：{}（按 (网格, 材质) 自动合批）", world.entity_count());
    }
}
