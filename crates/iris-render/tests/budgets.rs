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
    use iris_core::prelude::{GlobalTransform, Transform};
    use iris_render::prelude::{
        CpuCollector, Frustum, MaterialDef, MaterialHandle, MaterialStore, MeshData, MeshHandle,
        MeshStore,
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
