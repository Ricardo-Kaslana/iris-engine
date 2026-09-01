//! 工具：粒子系统验收场景（离屏渲染输出喷泉 / 爆炸两张 BMP）。
//!
//! 场景：暗色地面 + 两台发射器分框——左「喷泉」（`ParticleEmitter::fountain`
//! 预设：持续发射、竖直上抛 + 方向抖动、重力回落、alpha 混合）持续模拟
//! 60 步（1 s）后成像；右「爆炸」（`ParticleEmitter::explosion` 预设：
//! burst 一次性全向爆发、additive 混合 + HDR 暖色触发泛光）爆发后扩散
//! 0.3 s 成像。
//!
//! 输出 `iris_particles_fountain.bmp` 与 `iris_particles_explosion.bmp`
//! （640×360，→ docs/particles_fountain.png / docs/particles_explosion.png）。
//!
//! ```sh
//! cargo run -p iris-engine --example particle_snapshot --release
//! ```

use std::time::Duration;

use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Time, Transform, World};
use iris_engine::prelude::*;

/// 固定模拟步长（1/60 s）。
const STEP: Duration = Duration::from_nanos(1_000_000_000 / 60);

/// 基础世界：目录资源 + 相机 + 方向光 + 泛光 + 暗地面（空间参照）。
fn base_world() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());
    // 爆炸火花的 HDR 亮度（> 1）触发泛光，火花外围出现柔和辉光
    world.insert_resource(BloomSettings {
        enabled: true,
        strength: 0.5,
        threshold: 1.0,
    });
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(60.0));
    let ground = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.16, 0.17, 0.20, 1.0));
    world.spawn((Transform::IDENTITY, plane, ground));
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 2.6, 9.5);
        camera.target = Vec3::new(0.0, 1.6, 0.0);
    }
    world
}

/// 模拟驱动样板：Time::advance（确定性步长）→ update_particles（与
/// DefaultPlugins 的 ParticlePlugin 相同的系统，快照手动按调用契约执行：
/// 层级传播之后、渲染之前）。
fn simulate(world: &World, steps: usize) {
    for _ in 0..steps {
        world.resource_mut::<Time>().advance(STEP);
        update_particles(world);
    }
}

fn main() {
    env_logger::init();
    let Some(mut renderer) = OffscreenRenderer::new(640, 360) else {
        eprintln!("无 GPU");
        std::process::exit(1);
    };

    // ---- 喷泉：持续发射 60 步（1 s）后渲染 ----
    let mut world = base_world();
    world.insert_resource(Time::default());
    world.spawn((
        Transform {
            position: Vec3::new(-2.2, 0.0, 0.0),
            ..Transform::IDENTITY
        },
        ParticleEmitter::fountain(),
    ));
    propagate_transforms(&mut world);
    simulate(&world, 60);
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    println!(
        "喷泉 FrameStats: particles_drawn={}, draw_calls={}, instances={}, triangles={}",
        stats.particles_drawn, stats.draw_calls, stats.instances, stats.triangles_drawn
    );
    renderer
        .save_bmp("iris_particles_fountain.bmp")
        .expect("写喷泉 BMP");

    // ---- 爆炸：burst 一次性爆发 + 0.3 s 径向扩散后渲染 ----
    let mut world = base_world();
    world.insert_resource(Time::default());
    world.spawn((
        Transform {
            position: Vec3::new(2.0, 1.0, 0.0),
            ..Transform::IDENTITY
        },
        ParticleEmitter::explosion(),
    ));
    propagate_transforms(&mut world);
    simulate(&world, 18); // 首步触发 burst，其后 0.3 s 扩散 + 衰减
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    println!(
        "爆炸 FrameStats: particles_drawn={}, draw_calls={}, instances={}, triangles={}",
        stats.particles_drawn, stats.draw_calls, stats.instances, stats.triangles_drawn
    );
    renderer
        .save_bmp("iris_particles_explosion.bmp")
        .expect("写爆炸 BMP");

    println!("已输出 iris_particles_fountain.bmp / iris_particles_explosion.bmp");
}
