//! release 预算测试（R33）：关键 CPU 路径的宽松上限断言。
//!
//! # 定位（重要）
//!
//! 预算测试是「防突然 10 倍劣化」的**烟雾报警**，不是精确基准：
//! - 预算值 = criterion 基准（`benches/core.rs`）mean × 约 5（详见
//!   ARCHITECTURE §7.1 性能基线），吸收 CI 机器与本地开发机的差距与
//!   噪声——预算测试**允许误报率接近零**，代价是只能抓住数量级劣化；
//! - 精确的回归检测靠 criterion 基准的逐次对比（`cargo bench` 的
//!   基线变更报告），不靠这里的断言；
//! - 断言方式：N 次实测取**中位数**（比均值抗离群值），与预算比较。
//!
//! debug 构建下本文件只有一个说明性测试（release 数值在 debug 下无意义，
//! 慢一个数量级以上，断言必假）。

#[cfg(not(debug_assertions))]
mod release_budgets {
    use std::time::{Duration, Instant};

    use iris_core::prelude::*;

    /// 实测 N 次取中位数（抗离群值；奇数次避免插值）。
    fn median_of(runs: usize, mut measure: impl FnMut() -> Duration) -> Duration {
        let mut samples: Vec<Duration> = (0..runs).map(|_| measure()).collect();
        samples.sort();
        samples[runs / 2]
    }

    fn assert_budget(name: &str, median: Duration, budget: Duration) {
        assert!(
            median <= budget,
            "{name} 中位数 {median:?} 超出预算 {budget:?}——若非偶发噪声，\
             说明热路径发生了数量级劣化，请对照 criterion 基准定位"
        );
    }

    // ---- 本地组件（与 benches/core.rs 同构：Pos / Vel）----

    #[derive(Component)]
    struct Pos(Vec3);

    #[derive(Component)]
    struct Vel(Vec3);

    fn query_world() -> World {
        let mut world = World::new();
        for i in 0..10_000u32 {
            world.spawn((Pos(Vec3::new(i as f32, 0.0, 0.0)), Vel(Vec3::ONE)));
        }
        world
    }

    /// ecs_query_iterate_10k：1 万实体双组件遍历 + 写回（帧循环最热路径）。
    /// 预算 = 基准 mean 60 µs × 5（测量机器：Windows 11 / i7-13620H 笔记本，
    /// 2026-09-03）。
    #[test]
    fn budget_ecs_query_iterate_10k() {
        let world = query_world();
        let median = median_of(7, || {
            let start = Instant::now();
            world.query::<(&mut Pos, &Vel)>(|(p, v)| p.0 += v.0 * 0.016);
            start.elapsed()
        });
        assert_budget("ecs_query_iterate_10k", median, Duration::from_micros(300));
    }

    /// 100 体物理世界（60 动 + 40 静，微穿透保证持续接触），同 benches/core.rs。
    fn physics_world() -> World {
        let mut world = World::new();
        world.insert_resource(Time::default());
        world.spawn((
            Transform::from_xyz(0.0, -1.0, 0.0),
            Collider::static_box(Vec3::new(50.0, 1.0, 50.0)),
        ));
        for i in 0..39u32 {
            let x = (i % 7) as f32 * 3.0 - 9.0;
            let z = (i / 7) as f32 * 3.0 - 7.5;
            world.spawn((Transform::from_xyz(x, 0.5, z), Collider::static_sphere(0.5)));
        }
        for i in 0..60u32 {
            let x = (i % 10) as f32 * 2.0 - 9.0;
            let z = (i / 10) as f32 * 2.0 - 9.0;
            world.spawn((
                Transform::from_xyz(x, 0.45, z),
                Collider::dynamic_sphere(0.5, 0.1),
                RigidBody {
                    velocity: Vec3::new(0.1, 0.0, -0.1),
                    friction: 0.5,
                    ..RigidBody::default()
                },
            ));
        }
        world
    }

    /// physics_step_100：100 体持续接触一帧物理。
    /// 预算 = 基准 mean 4.64 ms × 约 5.4（测量机器与日期同上）。
    #[test]
    fn budget_physics_step_100() {
        let world = physics_world();
        let dt = Duration::from_secs_f64(1.0 / 60.0);
        let median = median_of(7, || {
            world.resource_mut::<Time>().advance(dt);
            let start = Instant::now();
            step_physics(&world);
            start.elapsed()
        });
        assert_budget("physics_step_100", median, Duration::from_millis(25));
    }
}

/// debug 构建说明：预算只在 release 有意义（debug 无优化，数值慢一个
/// 数量级以上），跑 release 版本才会执行真实断言。
#[cfg(debug_assertions)]
#[test]
fn budgets_only_run_in_release() {
    println!(
        "预算测试仅在 release 构建生效：请用 `cargo test --workspace --release` \
         （CI 的 test-release job 即此配置）。debug 下的耗时没有参照意义。"
    );
}
