//! 基准：内存占用与距离 LOD 量化验收（R30，「低占用」卖点兑现轮）。
//!
//! 场景：
//! - **纹理**：4 张 2048² 程序化棋盘格（RGBA8 源，全不透明、4 对齐——
//!   BC1 决策树的理想输入），注册进 `TextureStore` 后渲染一帧触发真实
//!   GPU 上传（`RUST_LOG=info` 可见逐张 BC1 上传日志）；
//! - **网格**：高模 / 低模 UV 球网格对，8 个实体挂 `LodMesh`（切换距离
//!   60 m）沿 -Z 纵深排开，4 种贴图材质轮转；
//! - **LOD 实测**：近景 / 远景相机各渲染数帧（首帧预热），`FrameStats`
//!   的 `triangles_drawn` 即 LOD 两级的实际提交三角形数。
//!
//! 输出：CPU 源字节 / GPU RGBA8 上传估算 / GPU BC1 上传估算 / 压缩比 /
//! LOD 两级三角形数。估算与渲染器实际决策共用同一纯函数
//! （`bc1_eligible`），`TextureStore::gpu_bytes_estimate(true)` 即本机
//! 实际显存占用量级。数字受开发机与驱动影响，仅供量级参考（实测记录
//! 见 ARCHITECTURE §4.18）。
//!
//! ```sh
//! cargo run -p iris-engine --example memory_bench --release
//! ```

use iris_core::prelude::{Transform, Vec3, World};
use iris_engine::prelude::{
    Camera, CpuTexture, DirectionalLight, FrameStats, LodMesh, MaterialDef, MaterialStore,
    MeshData, MeshStore, OffscreenRenderer, TextureStore,
};

/// 程序化纹理数量与边长（2048² × 4 张 = 64 MiB RGBA8 源）。
const TEXTURE_COUNT: u32 = 4;
const TEXTURE_SIZE: u32 = 2048;
/// LOD 实体数（沿 -Z 排开）。
const ENTITY_COUNT: u32 = 8;
/// LOD 切换距离（米）：近景相机到全部实体 ≤ 此值、远景全部 > 此值。
const SWITCH_DISTANCE: f32 = 60.0;
/// 每相机渲染帧数；首帧为预热（懒上传 / 缓冲扩容），统计取末帧。
const FRAMES: u32 = 3;

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

fn main() {
    env_logger::init();
    let Some(mut renderer) = OffscreenRenderer::new(1280, 720) else {
        eprintln!("无 GPU");
        std::process::exit(1);
    };
    let bc = renderer.gpu().supports_bc();

    // ---- 资源：纹理目录 + 高低模网格对 + 材质 ----
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());

    let mut materials = Vec::new();
    let palettes = [
        ([200u8, 60, 40, 255], [30, 30, 34, 255]),
        ([40, 200, 60, 255], [24, 28, 30, 255]),
        ([60, 80, 220, 255], [28, 26, 30, 255]),
        ([220, 200, 60, 255], [30, 28, 24, 255]),
    ];
    for (a, b) in palettes {
        let tex = world
            .resource_mut::<TextureStore>()
            .add(CpuTexture::checkerboard(TEXTURE_SIZE / 256, 256, a, b));
        let mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
            texture: tex,
            ..MaterialDef::default()
        });
        materials.push(mat);
    }
    let high = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, 64, 48));
    let low = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, 8, 6));

    // ---- 场景：8 实体沿 -Z 每 5 m 一个，LodMesh + 材质轮转 ----
    for i in 0..ENTITY_COUNT {
        world.spawn((
            Transform::from_xyz(0.0, 1.0, -5.0 - 5.0 * i as f32),
            LodMesh {
                high,
                low,
                switch_distance: SWITCH_DISTANCE,
            },
            materials[(i as usize) % materials.len()],
        ));
    }
    iris_core::hierarchy::propagate_transforms(&mut world);

    // ---- LOD 实测：近景全高模 / 远景全低模 ----
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 1.8, 10.0);
        camera.target = Vec3::new(0.0, 0.0, -20.0);
    }
    let near = draw_frames(&mut renderer, &world);
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 1.8, 200.0);
        camera.target = Vec3::new(0.0, 0.0, -20.0);
    }
    let far = draw_frames(&mut renderer, &world);

    // ---- 账本输出 ----
    let (tex_count, tex_side) = (TEXTURE_COUNT, TEXTURE_SIZE);
    let cpu_bytes = world.resource::<TextureStore>().memory_bytes();
    let gpu_rgba = world.resource::<TextureStore>().gpu_bytes_estimate(false);
    let gpu_best = world.resource::<TextureStore>().gpu_bytes_estimate(true);
    let mesh_bytes = world.resource::<MeshStore>().memory_bytes();
    let high_tris = world
        .resource::<MeshStore>()
        .get(high)
        .unwrap()
        .indices
        .len() as u32
        / 3;
    let low_tris = world
        .resource::<MeshStore>()
        .get(low)
        .unwrap()
        .indices
        .len() as u32
        / 3;

    println!("\n== 内存占用对比：纹理 BC1 压缩 ==");
    println!("  纹理：{tex_count} 张 {tex_side}² 程序化棋盘格（RGBA8 源，全不透明、4 对齐）");
    println!(
        "  CPU 源数据                = {:>12} B（{:>7.2} MiB）",
        cpu_bytes,
        mib(cpu_bytes)
    );
    println!(
        "  GPU 上传（RGBA8 回退路径）= {:>12} B（{:>7.2} MiB）",
        gpu_rgba,
        mib(gpu_rgba)
    );
    println!(
        "  GPU 上传（BC1 压缩路径）  = {:>12} B（{:>7.2} MiB）",
        gpu_best,
        mib(gpu_best)
    );
    println!(
        "  压缩比 {:.2}x（显存 -{:.1}%）；本机 TEXTURE_COMPRESSION_BC = {bc}\
         （是 → 实际上传走 BC1，否 → 全部回退 RGBA8）",
        gpu_rgba as f64 / gpu_best as f64,
        (1.0 - gpu_best as f64 / gpu_rgba as f64) * 100.0,
    );
    println!("  网格 CPU 内存合计（顶点 + 索引）= {mesh_bytes} B");

    println!("\n== 距离 LOD：网格两级 ==");
    println!(
        "  高模 uv_sphere(64, 48) = {high_tris} 三角形；低模 uv_sphere(8, 6) = {low_tris} 三角形\
         （{:.1}%，{:.0}x 削减）",
        low_tris as f64 / high_tris as f64 * 100.0,
        high_tris as f64 / low_tris as f64,
    );
    println!("\n== LOD 实测（{ENTITY_COUNT} 实体 × LodMesh，切换距离 {SWITCH_DISTANCE} m）==");
    println!(
        "  近景帧：triangles_drawn = {}（{ENTITY_COUNT} × {high_tris} 高模），instances = {}，draw_calls = {}",
        near.triangles_drawn, near.instances, near.draw_calls
    );
    println!(
        "  远景帧：triangles_drawn = {}（{ENTITY_COUNT} × {low_tris} 低模），instances = {}，draw_calls = {}",
        far.triangles_drawn, far.instances, far.draw_calls
    );
    println!(
        "  LOD 三角形削减 = {:.1}%（实例数与 draw call 不变，只换几何）",
        (1.0 - far.triangles_drawn as f64 / near.triangles_drawn as f64) * 100.0,
    );
}

/// 渲染 `FRAMES` 帧（首帧预热）并返回末帧统计。
fn draw_frames(renderer: &mut OffscreenRenderer, world: &World) -> FrameStats {
    let mut stats = FrameStats::default();
    for frame in 0..FRAMES {
        renderer.draw_frame(world);
        stats = renderer.renderer().stats();
        if frame == 0 {
            assert_eq!(
                stats.instances, ENTITY_COUNT,
                "全部实体都应在两台机位的视锥内（剔除不得混入 LOD 观测）"
            );
        }
    }
    stats
}
