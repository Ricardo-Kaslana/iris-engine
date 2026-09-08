//! release 预算测试（R33）：渲染 CPU 路径的宽松上限断言。
//!
//! 定位与噪声容忍见 `iris-core/tests/budgets.rs` 模块文档与
//! ARCHITECTURE §7.1：预算 = criterion 基准 mean × 约 5 的烟雾报警，
//! 精确回归检测靠 `cargo bench` 的基线对比。debug 构建跳过（说明性
//! 测试给出原因）。

#[cfg(not(debug_assertions))]
mod release_budgets {
    use std::time::{Duration, Instant};

    use glam::Vec3;
    use iris_core::prelude::{Entity, GlobalTransform, Transform};
    use iris_render::prelude::{
        BakeParams, BakeTriangle, CpuCollector, Frustum, MaterialDef, MaterialHandle,
        MaterialStore, MeshData, MeshHandle, MeshStore, bake_points, build_bvh,
    };

    /// 实测 N 次取中位数（抗离群值）。
    fn median_of(runs: usize, mut measure: impl FnMut() -> Duration) -> Duration {
        let mut samples: Vec<Duration> = (0..runs).map(|_| measure()).collect();
        samples.sort();
        samples[runs / 2]
    }

    /// 5k 立方体场景（100 × 50 网格，部分在视锥外），同 benches/render_cpu.rs。
    fn collect_scene() -> (iris_core::world::World, MeshStore, MaterialStore) {
        let mut world = iris_core::world::World::new();
        let mut meshes = MeshStore::new();
        let mut materials = MaterialStore::new();
        let mesh = meshes.add(MeshData::cube());
        let material = materials.add(MaterialDef::solid(0.8, 0.4, 0.2, 1.0));
        for i in 0..5_000u32 {
            let x = (i % 100) as f32 * 2.0 - 100.0;
            let z = (i / 100) as f32 * 2.0 - 50.0;
            let t = Transform::from_xyz(x, 0.0, z);
            world.spawn((
                GlobalTransform::from_transform(t),
                MeshHandle(mesh.0),
                MaterialHandle(material.0),
            ));
        }
        (world, meshes, materials)
    }

    /// collect_instances_5k：真实 World + MeshStore 下 5k 立方体的收集 +
    /// 视锥剔除（`Renderer::draw` 每帧 GPU 编码前的全部 CPU 收集工作，
    /// 最接近帧预算的指标）。
    /// 预算 = 基准 mean 298 µs × 约 5（测量机器：Windows 11 / i7-13620H
    /// 笔记本，2026-09-03）。
    #[test]
    fn budget_collect_instances_5k() {
        let (world, meshes, materials) = collect_scene();
        let eye = Vec3::new(0.0, 12.0, 18.0);
        let view = glam::camera::rh::view::look_at_mat4(eye, Vec3::ZERO, Vec3::Y);
        let view_proj = glam::camera::rh::proj::directx::perspective(
            std::f32::consts::FRAC_PI_3,
            16.0 / 9.0,
            0.1,
            100.0,
        ) * view;
        let frustum = Frustum::from_view_proj(&view_proj);
        let mut collector = CpuCollector::new();
        // 预热一次（scratch 扩容 + 组件存储首次借用的懒注册）
        let warm = collector.collect(&world, &meshes, &materials, &view, eye, &frustum);
        assert!(warm.visible > 0, "场景应含可见实例");
        let median = median_of(7, || {
            let start = Instant::now();
            collector.collect(&world, &meshes, &materials, &view, eye, &frustum);
            start.elapsed()
        });
        assert!(
            median <= Duration::from_micros(1500),
            "collect_instances_5k 中位数 {median:?} 超出预算 1.5 ms——\
             若非偶发噪声，说明帧收集路径发生了数量级劣化"
        );
    }

    /// splitmix64 终结函数（无 RNG 的确定性抖动源，与 `bake.rs` 测试同源）。
    fn mix64(mut z: u64) -> u64 {
        z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn unit(seed: u64) -> f32 {
        (mix64(seed) >> 40) as f32 / (1u64 << 24) as f32
    }

    /// 烘焙阶段验收形状：1000 三角（500 个抖动 quad 的 25 × 20 场）+
    /// 1000 个采样点 + 64 sky 样本 + 1 反弹，一次全烘焙（BVH 构建 + 积分）
    /// 的中位耗时。
    /// 预算 = criterion 均值 34.3 ms × 约 5（`bake_irradiance_1k_points`；
    /// 一次性前置的 BVH 构建另计 217 µs = `bake_bvh_build_1k_tris`。测量
    /// 机器：Windows 11 / i7-13620H 笔记本，2026-09-08）。
    ///
    /// 这是「写实场景离线烘焙到底要不要并行」的判据数字（同记于
    /// ARCHITECTURE §4.24）：单线程 1 k 点 ≈ 34 ms ⇒ a 阶段不引入线程池；
    /// b/c 阶段若因 texel 数量放大而越界（1024² 图集是 10⁶ 点量级，
    /// 线性外推 ≈ 35 s），切 `bake_points` 的 `points` 分段即可——各点结果
    /// 互不依赖、按序拼接保序 ⇒ 确定性不破。
    #[test]
    fn budget_bake_1000_triangles_1000_points() {
        let mut triangles: Vec<BakeTriangle> = Vec::with_capacity(1000);
        for i in 0..500u64 {
            let gx = (i % 25) as f32;
            let gz = (i / 25) as f32;
            let center = Vec3::new(
                gx * 2.0 - 25.0 + unit(i * 3 + 1) - 0.5,
                unit(i * 3 + 2) * 3.0 - 1.5,
                gz * 2.0 - 20.0 + unit(i * 3 + 3) - 0.5,
            );
            let u = Vec3::new(1.0, unit(i * 7 + 1) - 0.5, unit(i * 7 + 2) - 0.5);
            let v = Vec3::new(unit(i * 11 + 1) - 0.5, 1.0, unit(i * 11 + 2) - 0.5);
            let corners = [
                center - u - v,
                center + u - v,
                center + u + v,
                center - u + v,
            ];
            let normal = u.cross(v);
            let entity = Entity::from_bits(i + 1);
            let albedo = Vec3::new(unit(i * 13 + 1), unit(i * 13 + 2), unit(i * 13 + 3));
            for indices in [[0usize, 1, 2], [0, 2, 3]] {
                triangles.push(BakeTriangle {
                    vertices: [
                        corners[indices[0]],
                        corners[indices[1]],
                        corners[indices[2]],
                    ],
                    normal,
                    albedo,
                    entity,
                });
            }
        }
        assert_eq!(triangles.len(), 1000);
        let points: Vec<(Vec3, Vec3)> = (0..1000u64)
            .map(|i| {
                (
                    Vec3::new(
                        unit(i * 5 + 1) * 40.0 - 20.0,
                        unit(i * 5 + 2) * 4.0 + 0.5,
                        unit(i * 5 + 3) * 30.0 - 15.0,
                    ),
                    Vec3::new(
                        unit(i * 17 + 1) - 0.5,
                        unit(i * 17 + 2),
                        unit(i * 17 + 3) - 0.5,
                    )
                    .normalize(),
                )
            })
            .collect();
        let params = BakeParams {
            sky_samples: 64,
            bounces: 1,
            shadow_softness: 0.2,
            ..Default::default()
        };
        // 预热一次（分配器扩容 + BVH 构建路径的首次分支预编译）
        let warmed = bake_points(&build_bvh(triangles.clone()), &points[..64], &params);
        assert_eq!(warmed.len(), 64);
        let median = median_of(3, || {
            let start = Instant::now();
            let samples = bake_points(&build_bvh(triangles.clone()), &points, &params);
            assert_eq!(samples.len(), 1000);
            start.elapsed()
        });
        assert!(
            median <= Duration::from_millis(200),
            "bake_1000_triangles_1000_points 中位数 {median:?} 超出预算 200 ms——\
             若非偶发噪声，说明跟踪器 / 积分器发生了数量级劣化（并行化候选）"
        );
    }
}

/// debug 构建说明（原因见模块文档）。
#[cfg(debug_assertions)]
#[test]
fn budgets_only_run_in_release() {
    println!(
        "预算测试仅在 release 构建生效：请用 `cargo test --workspace --release` \
         （CI 的 test-release job 即此配置）。debug 下的耗时没有参照意义。"
    );
}
