//! iris-core CPU 基准（criterion，R33）。
//!
//! 测的都是「每帧成本有界」的纯 CPU 热路径：ECS 生成 / 遍历、调度分批、
//! 并行执行、物理步进、角色控制器、broadphase、确定性模拟。数字含义：
//!
//! - `ecs_spawn_10k` / `ecs_query_iterate_10k`：World 级吞吐（稀疏集路径）；
//! - `schedule_build_batches`：30 系统读写集冲突分批的纯构建开销
//!   （每帧调度前的固定成本，与系统体无关）；
//! - `parallel_run_8_systems`：8 个无冲突并行系统 × 1 万实体（含每批
//!   线程创建——批次粒度线程模型的每帧真实代价）；
//! - `physics_step_100`：100 体（60 动 + 40 静、持续接触）一帧物理；
//! - `move_and_slide`：角色控制器一帧（平地 + 一墙，含滑行与贴地）；
//! - `broadphase_200_aabb`：200 AABB 建网格 + 全量候选查询一次；
//! - `determinism_trace_240`：确定性模拟 240 步 × 2 世界 + 采样记录
//!   （既有 bit 级确定性测试的开销上界参考）。
//!
//! ```sh
//! cargo bench -p iris-core
//! ```

use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use iris_core::broadphase::BroadphaseGrid;
use iris_core::prelude::*;

/// 本地组件：位置（不依赖 render，避免基准拖入渲染依赖树）。
#[derive(Component)]
struct Pos(Vec3);

/// 本地组件：速度。
#[derive(Component)]
struct Vel(Vec3);

/// 并行调度基准的 8 个互不冲突分量列（bench_parallel 同款宏，迁移自
/// 既有 example 基准——职责并入本文件后 example 已删除）。
macro_rules! columns {
    ($($name:ident),+) => {
        $(#[derive(Component)] struct $name(f32);)+
    };
}

columns!(C0, C1, C2, C3, C4, C5, C6, C7);

/// 单个分量的更新体（每个并行系统一份完全相同的工作）。
macro_rules! step_column {
    ($world:expr, $ty:ty) => {
        for e in $world.entities() {
            if let Some(mut c) = $world.get_mut::<$ty>(e) {
                c.0 = c.0 * 1.0001 + 0.001;
            }
        }
    };
}

/// 确定性 LCG（与 broadphase 测试同款，5 行手写不引依赖）。
struct Lcg(u64);

impl Lcg {
    fn next_f32(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// 物理基准世界：地面 + 39 颗静态球 + 60 颗微穿透动球（每步都有接触
/// 要解：动球 y = 半径 × 0.9，第一步即陷入地面触发分离 + 摩擦）。
fn physics_world() -> World {
    let mut world = World::new();
    world.insert_resource(Time::default());
    // 顶面 y = 0 的大静态地面
    world.spawn((
        Transform::from_xyz(0.0, -1.0, 0.0),
        Collider::static_box(Vec3::new(50.0, 1.0, 50.0)),
    ));
    // 39 颗静态球：7 × 6 网格铺在场地上（缺角留出通道）
    for i in 0..39u32 {
        let x = (i % 7) as f32 * 3.0 - 9.0;
        let z = (i / 7) as f32 * 3.0 - 7.5;
        world.spawn((Transform::from_xyz(x, 0.5, z), Collider::static_sphere(0.5)));
    }
    // 60 颗动球：撒在静态球之间的通道里，微穿透地面 + 小初速（接触密集）
    let mut rng = Lcg(0x5EED_2026);
    for _ in 0..60u32 {
        let x = rng.next_f32() * 20.0 - 10.0;
        let z = rng.next_f32() * 16.0 - 8.0;
        let vx = (rng.next_f32() - 0.5) * 2.0;
        let vz = (rng.next_f32() - 0.5) * 2.0;
        world.spawn((
            Transform::from_xyz(x, 0.45, z),
            Collider::dynamic_sphere(0.5, 0.1),
            RigidBody {
                velocity: Vec3::new(vx, 0.0, vz),
                friction: 0.5,
                ..RigidBody::default()
            },
        ));
    }
    world
}

/// 推进一帧物理（Time 固定步长 + step_physics，与物理测试同款驱动）。
fn physics_tick(world: &World) {
    world
        .resource_mut::<Time>()
        .advance(Duration::from_secs_f64(1.0 / 60.0));
    step_physics(world);
}

/// 确定性模拟基准的一次完整运行（两世界各 240 步 + 逐采样记录，
/// 即 `physics.rs::simulation_is_deterministic_across_runs` 测试路径）。
fn determinism_run() -> usize {
    fn build() -> (World, Vec<Entity>) {
        let mut world = World::new();
        world.insert_resource(Time::default());
        world.spawn((
            Transform::from_xyz(0.0, -1.0, 0.0),
            Collider::static_box(Vec3::new(50.0, 1.0, 50.0)),
        ));
        let balls = [
            (
                Vec3::new(-2.0, 4.0, 0.0),
                Vec3::new(1.0, 0.0, 0.5),
                0.8,
                0.1,
            ),
            (Vec3::new(0.0, 6.5, 1.0), Vec3::ZERO, 0.5, 0.4),
            (
                Vec3::new(2.0, 3.2, -1.0),
                Vec3::new(-1.5, 0.0, 0.0),
                0.2,
                0.8,
            ),
            (
                Vec3::new(0.5, 8.0, 0.0),
                Vec3::new(0.5, 1.0, -0.5),
                0.9,
                0.2,
            ),
            (Vec3::new(-1.0, 5.0, 2.0), Vec3::ZERO, 0.35, 0.6),
        ]
        .map(|(pos, vel, restitution, friction)| {
            world.spawn((
                Transform::from_translation(pos),
                Collider::dynamic_sphere(0.5, restitution),
                RigidBody {
                    velocity: vel,
                    friction,
                    ..RigidBody::default()
                },
            ))
        })
        .to_vec();
        (world, balls)
    }

    let (world_a, balls_a) = build();
    let (world_b, balls_b) = build();
    let mut trace_a: Vec<Vec3> = Vec::new();
    let mut trace_b: Vec<Vec3> = Vec::new();
    for _ in 0..240 {
        physics_tick(&world_a);
        physics_tick(&world_b);
        for (&a, &b) in balls_a.iter().zip(&balls_b) {
            trace_a.push(
                world_a
                    .get::<Transform>(a)
                    .expect("球有 Transform")
                    .position,
            );
            trace_b.push(
                world_b
                    .get::<Transform>(b)
                    .expect("球有 Transform")
                    .position,
            );
        }
    }
    trace_a.len() + trace_b.len()
}

fn bench_core(c: &mut Criterion) {
    // ---- ECS：生成与全量遍历（稀疏集 dense 路径，每实体 2 组件）----
    c.bench_function("ecs_spawn_10k", |b| {
        b.iter(|| {
            let mut world = World::new();
            for i in 0..10_000u32 {
                world.spawn((Pos(Vec3::new(i as f32, 0.0, 0.0)), Vel(Vec3::ONE)));
            }
            black_box(world)
        })
    });

    let mut query_world = World::new();
    for i in 0..10_000u32 {
        query_world.spawn((Pos(Vec3::new(i as f32, 0.0, 0.0)), Vel(Vec3::ONE)));
    }
    // 读 Vel 写 Pos：帧循环里最热的「积分式查询」形态
    c.bench_function("ecs_query_iterate_10k", |b| {
        b.iter(|| {
            query_world.query::<(&mut Pos, &Vel)>(|(p, v)| {
                p.0 += v.0 * black_box(0.016);
            })
        })
    });

    // ---- 调度：30 系统读写集分批（纯构建，不执行系统体）----
    let mut batch_schedule = Schedule::new();
    // 5 组 × 6 系统：组内写同一列（互斥 → 各自成批），组间读不同列
    // （无冲突）——构造真实混合读写集：批数 ≈ 6，冲突判定覆盖读写交叉
    for g in 0..5u32 {
        for k in 0..6u32 {
            let writer = g == 0;
            let access = if writer {
                Access::new().write::<C0>()
            } else {
                Access::new().read::<C1>().read::<C2>()
            };
            batch_schedule.add_parallel_system(Stage::Update, access, move |_: &World| {
                black_box((g, k)); // 空系统体：分批构建与执行调度开销隔离
            });
        }
    }
    c.bench_function("schedule_build_batches", |b| {
        b.iter(|| black_box(batch_schedule.batch_sizes(Stage::Update)))
    });

    // ---- 调度：8 个无冲突并行系统 × 1 万实体（含每批线程创建）----
    let mut par_world = World::new();
    for i in 0..10_000u32 {
        let entity = par_world.spawn_empty();
        let x = i as f32;
        par_world.insert(entity, C0(x));
        par_world.insert(entity, C1(x));
        par_world.insert(entity, C2(x));
        par_world.insert(entity, C3(x));
        par_world.insert(entity, C4(x));
        par_world.insert(entity, C5(x));
        par_world.insert(entity, C6(x));
        par_world.insert(entity, C7(x));
    }
    let mut par_schedule = Schedule::new();
    par_schedule.add_parallel_system(
        Stage::Update,
        Access::new().write::<C0>(),
        |world: &World| step_column!(world, C0),
    );
    par_schedule.add_parallel_system(
        Stage::Update,
        Access::new().write::<C1>(),
        |world: &World| step_column!(world, C1),
    );
    par_schedule.add_parallel_system(
        Stage::Update,
        Access::new().write::<C2>(),
        |world: &World| step_column!(world, C2),
    );
    par_schedule.add_parallel_system(
        Stage::Update,
        Access::new().write::<C3>(),
        |world: &World| step_column!(world, C3),
    );
    par_schedule.add_parallel_system(
        Stage::Update,
        Access::new().write::<C4>(),
        |world: &World| step_column!(world, C4),
    );
    par_schedule.add_parallel_system(
        Stage::Update,
        Access::new().write::<C5>(),
        |world: &World| step_column!(world, C5),
    );
    par_schedule.add_parallel_system(
        Stage::Update,
        Access::new().write::<C6>(),
        |world: &World| step_column!(world, C6),
    );
    par_schedule.add_parallel_system(
        Stage::Update,
        Access::new().write::<C7>(),
        |world: &World| step_column!(world, C7),
    );
    // 预热一次（线程栈 / 页缓存），避免首轮冷启动污染
    par_schedule.run(Stage::Update, &mut par_world);
    c.bench_function("parallel_run_8_systems", |b| {
        b.iter(|| par_schedule.run(Stage::Update, black_box(&mut par_world)))
    });

    // ---- 物理：100 体持续接触一帧（构建在 iter 外，只测 step_physics）----
    c.bench_function("physics_step_100", |b| {
        b.iter_batched(
            physics_world,
            |world| {
                physics_tick(black_box(&world));
            },
            BatchSize::PerIteration,
        )
    });

    // ---- 角色控制器：一帧平地行走 + 迎面撞墙（滑行 + 贴地吸附）----
    let mut char_world = World::new();
    // 顶面 y = 0 的地面 + 一面 +Z 朝内的墙（角色向 -Z 撞上去）
    char_world.spawn((
        Transform::from_xyz(0.0, -1.0, 0.0),
        Collider::static_box(Vec3::new(50.0, 1.0, 50.0)),
    ));
    char_world.spawn((
        Transform::from_xyz(0.0, 1.0, -3.0),
        Collider::static_box(Vec3::new(10.0, 1.0, 0.25)),
    ));
    let char_params = CharacterParams::default();
    c.bench_function("move_and_slide", |b| {
        b.iter(|| {
            move_and_slide(
                black_box(&char_world),
                Vec3::new(0.0, 0.9, 0.0),
                Vec3::new(3.0, 0.0, -4.0),
                1.0 / 60.0,
                black_box(&char_params),
            )
        })
    });

    // ---- broadphase：200 AABB 建网格 + 全量候选查询（确定性 LCG 布局）----
    let mut rng = Lcg(0x5EED_2026);
    const COUNT: usize = 200;
    const SPAN: f32 = 12.0;
    let boxes: Vec<(Vec3, Vec3)> = (0..COUNT)
        .map(|_| {
            let min = Vec3::new(
                rng.next_f32() * SPAN - SPAN * 0.5,
                rng.next_f32() * SPAN - SPAN * 0.5,
                rng.next_f32() * SPAN - SPAN * 0.5,
            );
            let size = Vec3::new(
                0.3 + rng.next_f32() * 2.1,
                0.3 + rng.next_f32() * 2.1,
                0.3 + rng.next_f32() * 2.1,
            );
            (min, min + size)
        })
        .collect();
    c.bench_function("broadphase_200_aabb", |b| {
        b.iter(|| {
            let mut grid = BroadphaseGrid::new(2.0);
            for (index, (min, max)) in boxes.iter().enumerate() {
                grid.insert_aabb(index, *min, *max);
            }
            let mut hits = 0usize;
            for (min, max) in &boxes {
                hits += grid.candidates(*min, *max).len();
            }
            black_box(hits)
        })
    });

    // ---- 确定性模拟：240 步 × 2 世界 + 逐采样记录（测试路径上界参考）----
    // 单次迭代 ~2 ms × 240 步实际为秒级：样本数降到 10（均值仍稳定），
    // 否则单组基准耗时 200 s+，拖垮本地与 CI 的全量 bench 反馈周期
    let mut group = c.benchmark_group("determinism");
    group.sample_size(10);
    group.bench_function("trace_240", |b| b.iter(|| black_box(determinism_run())));
    group.finish();
}

criterion_group!(benches, bench_core);
criterion_main!(benches);
