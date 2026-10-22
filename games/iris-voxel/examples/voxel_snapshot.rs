//! 体素沙盒离屏验收图：出生点远景、编辑后的木板墙近景、空中俯瞰。
//!
//! 输出三张 640×360 BMP（已被 `.gitignore` 忽略），用 ffmpeg 横向
//! 拼接成入库的 `docs/voxel.png`：
//!
//! ```sh
//! cargo run -p iris-voxel --example voxel_snapshot --release
//! ffmpeg -y -i voxel_spawn.bmp -i voxel_edit.bmp -i voxel_overview.bmp \
//!   -filter_complex hstack=inputs=3 docs/voxel.png
//! ```

use glam::{IVec3, Vec3};
use iris_engine::prelude::*;
use iris_voxel::VoxelPlugin;
use iris_voxel::logic::Block;
use iris_voxel::plugin::apply_edit;

const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;

fn main() {
    let mut app = App::new();
    app.world.insert_resource(Time::default());
    app.world.insert_resource(Input::new());
    app.world.insert_resource(MouseCapture::new());
    app.world.insert_resource(MeshStore::new());
    app.world.insert_resource(MaterialStore::new());
    app.world.insert_resource(TextureStore::new());
    app.world.insert_resource(Camera::default());
    app.world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.45, -1.0, -0.35),
        color: Vec3::new(0.98, 0.96, 0.9),
        intensity: 1.0,
        ambient: Vec3::new(0.16, 0.18, 0.22),
        ..DirectionalLight::default()
    });
    app.world.insert_resource(Viewport::new(WIDTH, HEIGHT));
    // 区块实体挂的是局部 Transform，渲染收集读 GlobalTransform：
    // 与窗口路径一样需要 PostUpdate 的层级传播。
    HierarchyPlugin.build(&mut app);
    VoxelPlugin::new(20_261_022).build_schedule(&mut app.schedule);
    app.schedule.run(Stage::Startup, &mut app.world);
    app.schedule.run(Stage::PostUpdate, &mut app.world);
    // 跑一次 Update 让 HUD 完成布局（dt 为 0，玩家不会移动）。
    app.schedule.run(Stage::Update, &mut app.world);

    let Some(mut renderer) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };

    // 1. 出生点远景：抬高一点俯瞰平台与丘陵，避免掠射角把台地间隙
    //    压成一条天空细线。
    set_camera(
        &mut app.world,
        Vec3::new(64.5, 24.0, 80.0),
        Vec3::new(64.0, 14.0, 56.0),
    );
    shoot(&mut renderer, &app.world, "voxel_spawn.bmp");

    // 2. 编辑后的木板墙：直接写入世界并重建受影响区块。
    build_demo_wall(&mut app.world);
    set_camera(
        &mut app.world,
        Vec3::new(64.5, 18.0, 72.0),
        Vec3::new(64.5, 16.0, 61.0),
    );
    shoot(&mut renderer, &app.world, "voxel_edit.bmp");

    // 3. 空中俯瞰：抬高相机俯视丘陵与平台。
    set_camera(
        &mut app.world,
        Vec3::new(52.0, 40.0, 84.0),
        Vec3::new(64.0, 14.0, 64.0),
    );
    shoot(&mut renderer, &app.world, "voxel_overview.bmp");
}

fn set_camera(world: &mut World, eye: Vec3, target: Vec3) {
    let mut camera = world.resource_mut::<Camera>();
    camera.eye = eye;
    camera.target = target;
    camera.up = Vec3::Y;
}

fn shoot(renderer: &mut OffscreenRenderer, world: &World, path: &str) {
    renderer.renderer_mut().reset_taa();
    renderer.render(world);
    renderer.save_bmp(path).expect("写 BMP 失败");
    let stats = renderer.renderer().stats();
    assert!(stats.instances > 0, "场景应有可见实例");
    println!(
        "{path}: {} 个实例，{} 个三角形",
        stats.instances, stats.triangles_drawn
    );
}

/// 在出生点前方搭一面 6×5 的木板墙，中间留 2×3 的门洞，并在墙前
/// 放两根石柱，作为编辑闭环的视觉证据。
fn build_demo_wall(world: &mut World) {
    for x in 61..=67 {
        for y in 15..=19 {
            if (64..=65).contains(&x) && (15..=17).contains(&y) {
                continue;
            }
            apply_edit(world, IVec3::new(x, y, 61), Block::Planks);
        }
    }
    for y in 15..=17 {
        apply_edit(world, IVec3::new(60, y, 63), Block::Stone);
        apply_edit(world, IVec3::new(68, y, 63), Block::Stone);
    }
    // 墙前的地面铺一条沙路，保证近景有明显的材质对比。
    for x in 62..=66 {
        apply_edit(world, IVec3::new(x, 14, 60), Block::Sand);
    }
}
