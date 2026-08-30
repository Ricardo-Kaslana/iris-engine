//! ECS 吞吐基准：10 万实体生成 + 1000 帧双组件遍历。
//!
//! ```sh
//! cargo run -p iris-core --example bench_ecs --release
//! ```

use std::time::Instant;

use iris_core::prelude::*;

#[derive(Component)]
struct Position(Vec3);

#[derive(Component)]
struct Velocity(Vec3);

fn main() {
    let mut world = World::new();
    const N: u32 = 100_000;

    let t0 = Instant::now();
    for i in 0..N {
        world.spawn((
            Position(Vec3::new(i as f32, 0.0, 0.0)),
            Velocity(Vec3::ONE),
        ));
    }
    let spawn_time = t0.elapsed();
    println!("spawn {N} 实体（2 组件）：{spawn_time:?}");

    const FRAMES: u32 = 1000;
    let t1 = Instant::now();
    for _ in 0..FRAMES {
        world.query::<(&mut Position, &Velocity)>(|(p, v)| p.0 += v.0 * 0.016);
    }
    let total = t1.elapsed();
    println!(
        "遍历查询 × {FRAMES} 帧（{N} 实体/帧）：总计 {total:?}，平均每帧 {:?}",
        total / FRAMES
    );
    println!(
        "吞吐 ≈ {:.0} 万实体组件更新/秒",
        (N as f64 * FRAMES as f64) / total.as_secs_f64() / 10_000.0
    );
}
