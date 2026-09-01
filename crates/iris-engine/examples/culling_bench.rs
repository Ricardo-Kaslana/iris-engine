//! 基准：视锥剔除量化验收（5000 立方体 × 双朝向 300 帧）。
//!
//! 场景：100×50 网格铺开 5000 个立方体（同 mesh/material，全部可见时
//! 走实例化单批）。两个相机朝向各渲染 300 帧（首 50 帧预热丢弃）：
//!
//! - **A 面向场景大片**：高视角俯瞰全场，绝大多数实例进入视锥（高可见）；
//! - **B 背对望空**：同一机位转 180°，整个网格在相机身后（高剔除，
//!   理想情况 5000 实例全部剔除、零提交）。
//!
//! 每朝向输出 `FrameStats` 全字段 + `Renderer::draw` 的平均 / 最坏 CPU
//! 耗时（`std::time::Instant` 包住 draw 调用；离屏的读回 / 落盘不计——
//! 使用不拷贝读回缓冲的 `OffscreenRenderer::draw_frame`）。数字受开发机
//! 与驱动影响，仅供量级参考（实测记录见 ARCHITECTURE §4.4.1）。
//!
//! ```sh
//! cargo run -p iris-engine --example culling_bench --release
//! ```

use std::time::Instant;

use iris_core::prelude::{Transform, Vec3};
use iris_engine::prelude::{
    Camera, DirectionalLight, FrameStats, MaterialDef, MaterialStore, MeshData, MeshStore,
    OffscreenRenderer, TextureStore,
};

/// 网格规模：100 × 50 = 5000 实例。
const GRID_X: u32 = 100;
const GRID_Z: u32 = 50;
/// 网格间距（米）。
const SPACING: f32 = 1.5;
/// 每朝向渲染帧数；前 `WARMUP` 帧为预热（着色器编译 / 缓冲扩容），丢弃。
const FRAMES: u32 = 300;
const WARMUP: u32 = 50;

fn main() {
    env_logger::init();
    let Some(mut renderer) = OffscreenRenderer::new(1280, 720) else {
        eprintln!("无 GPU");
        std::process::exit(1);
    };

    // ---- 场景：5000 立方体（同 mesh/material → 全可见时单批实例化）----
    let mut world = iris_core::prelude::World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let material = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.55, 0.42, 0.28, 1.0));
    let half_x = (GRID_X as f32 - 1.0) * SPACING * 0.5;
    let half_z = (GRID_Z as f32 - 1.0) * SPACING * 0.5;
    for x in 0..GRID_X {
        for z in 0..GRID_Z {
            world.spawn((
                Transform::from_xyz(
                    x as f32 * SPACING - half_x,
                    0.5,
                    z as f32 * SPACING - half_z,
                ),
                cube,
                material,
            ));
        }
    }
    iris_core::hierarchy::propagate_transforms(&mut world);

    // ---- 朝向 A：高视角面向全场（高可见）----
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 40.0, 100.0);
        camera.target = Vec3::new(0.0, 0.0, 0.0);
    }
    let stats_a = run_orientation(&mut renderer, &world, "A 面向场景大片（高可见）");

    // ---- 朝向 B：同一机位转 180° 望空（高剔除）----
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.target = Vec3::new(0.0, 40.0, 180.0);
    }
    let stats_b = run_orientation(&mut renderer, &world, "B 背对望空（高剔除）");

    println!(
        "\n汇总：A 可见 {} / 剔除 {} / {:.2} M 三角形，B 可见 {} / 剔除 {} / {} draw call",
        stats_a.instances,
        stats_a.culled_instances,
        stats_a.triangles_drawn as f64 / 1e6,
        stats_b.instances,
        stats_b.culled_instances,
        stats_b.draw_calls,
    );
}

/// 渲染 `FRAMES` 帧（预热丢弃）并输出该朝向的 FrameStats 与 CPU 耗时。
fn run_orientation(
    renderer: &mut OffscreenRenderer,
    world: &iris_core::prelude::World,
    label: &str,
) -> FrameStats {
    let mut timings = Vec::with_capacity(FRAMES as usize);
    let mut stats = FrameStats::default();
    for frame in 0..FRAMES {
        let start = Instant::now();
        renderer.draw_frame(world);
        let elapsed = start.elapsed();
        stats = renderer.renderer().stats();
        if frame >= WARMUP {
            timings.push(elapsed.as_secs_f64() * 1000.0);
        }
    }
    let avg: f64 = timings.iter().sum::<f64>() / timings.len() as f64;
    let worst = timings.iter().cloned().fold(0.0f64, f64::max);
    println!(
        "\n== 朝向 {label}（{} 帧实测，前 {WARMUP} 帧预热丢弃） ==",
        timings.len()
    );
    println!(
        "  draw_calls = {}，instances = {}，culled_instances = {}，\
         triangles_drawn = {}，instance_upload_bytes = {}",
        stats.draw_calls,
        stats.instances,
        stats.culled_instances,
        stats.triangles_drawn,
        stats.instance_upload_bytes,
    );
    println!("  draw() CPU：平均 {avg:.3} ms / 最坏 {worst:.3} ms");
    stats
}
