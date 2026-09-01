//! 并行调度基准：8 个互不冲突的分量系统，串行（独占）vs 并行（访问集分批）。
//!
//! 同样的 8 份工作分别以「逐个独占执行（强制串行）」与「8 个并行系统
//! （读写集不相交 → 同批并发）」执行，输出两者耗时与加速比。
//!
//! ```sh
//! cargo run -p iris-core --example bench_parallel --release
//! ```

// Instant 用 prelude 的跨平台封装（wasm 上 std 版本运行时 panic，见 iris_core::instant）
use iris_core::prelude::*;

const ENTITY_COUNT: u32 = 50_000;
const ITERATIONS: u32 = 20;

macro_rules! columns {
    ($($name:ident),+) => {
        $(#[derive(Component)] struct $name(f32);)+

        fn insert_all(world: &mut World, entity: Entity, x: f32) {
            $(world.insert(entity, $name(x));)+
        }
    };
}

columns!(C0, C1, C2, C3, C4, C5, C6, C7);

fn spawn_world() -> World {
    let mut world = World::new();
    for i in 0..ENTITY_COUNT {
        let entity = world.spawn_empty();
        insert_all(&mut world, entity, i as f32);
    }
    world
}

/// 单个分量的更新体（串行基线与并行系统执行完全相同的工作）。
macro_rules! step_column {
    ($world:expr, $ty:ty) => {
        for e in $world.entities() {
            if let Some(mut c) = $world.get_mut::<$ty>(e) {
                c.0 = c.0 * 1.0001 + 0.001;
            }
        }
    };
}

fn main() {
    // 串行基线：8 份工作逐个执行
    let serial = spawn_world();
    let t0 = Instant::now();
    for _ in 0..ITERATIONS {
        step_column!(serial, C0);
        step_column!(serial, C1);
        step_column!(serial, C2);
        step_column!(serial, C3);
        step_column!(serial, C4);
        step_column!(serial, C5);
        step_column!(serial, C6);
        step_column!(serial, C7);
    }
    let serial_time = t0.elapsed();

    // 并行：8 个并行系统读写集互不相交 → 调度器并入同一批并发执行
    let mut world = spawn_world();
    let mut schedule = Schedule::new();
    schedule.add_parallel_system(
        Stage::Update,
        Access::new().write::<C0>(),
        |world: &World| step_column!(world, C0),
    );
    schedule.add_parallel_system(
        Stage::Update,
        Access::new().write::<C1>(),
        |world: &World| step_column!(world, C1),
    );
    schedule.add_parallel_system(
        Stage::Update,
        Access::new().write::<C2>(),
        |world: &World| step_column!(world, C2),
    );
    schedule.add_parallel_system(
        Stage::Update,
        Access::new().write::<C3>(),
        |world: &World| step_column!(world, C3),
    );
    schedule.add_parallel_system(
        Stage::Update,
        Access::new().write::<C4>(),
        |world: &World| step_column!(world, C4),
    );
    schedule.add_parallel_system(
        Stage::Update,
        Access::new().write::<C5>(),
        |world: &World| step_column!(world, C5),
    );
    schedule.add_parallel_system(
        Stage::Update,
        Access::new().write::<C6>(),
        |world: &World| step_column!(world, C6),
    );
    schedule.add_parallel_system(
        Stage::Update,
        Access::new().write::<C7>(),
        |world: &World| step_column!(world, C7),
    );

    // 预热一次（线程创建 / 页缓存）
    schedule.run(Stage::Update, &mut world);
    let t1 = Instant::now();
    for _ in 0..ITERATIONS {
        schedule.run(Stage::Update, &mut world);
    }
    let parallel_time = t1.elapsed();

    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    println!("实体 {ENTITY_COUNT} × 8 个互不冲突分量系统 × {ITERATIONS} 帧（核心数 {cores}）");
    println!("串行（逐个执行）：{serial_time:?}");
    println!("并行（访问集分批并发）：{parallel_time:?}");
    println!(
        "加速比：{:.2}×",
        serial_time.as_secs_f64() / parallel_time.as_secs_f64().max(1e-9)
    );
}
