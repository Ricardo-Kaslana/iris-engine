//! 体素沙盒离屏验收图：出生点远景、荧光石房间近景、空中俯瞰。
//!
//! 输出三张 640×360 BMP（已被 `.gitignore` 忽略），用 ffmpeg 横向
//! 拼接成入库的 `docs/voxel.png`：
//!
//! ```sh
//! cargo run -p iris-voxel --example voxel_snapshot --release
//! ffmpeg -y -i voxel_spawn.bmp -i voxel_light.bmp -i voxel_overview.bmp \
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

    build_light_shaft(&mut app.world);

    // 1. 出生点远景：拉远抬高压低塔楼在画面中的占比，展示平台与丘陵。
    set_camera(
        &mut app.world,
        Vec3::new(92.0, 40.0, 108.0),
        Vec3::new(62.0, 20.0, 60.0),
    );
    shoot(&mut renderer, &app.world, "voxel_spawn.bmp");

    // 2. 光井内景：从塔顶俯视 3×3 净空的竖井。底部荧光石的暖光沿
    //    井壁向上衰减，近 20 格光程后完全熄灭，顶部只剩环境底光；
    //    底部四角可见 AO 压暗。
    set_camera(
        &mut app.world,
        Vec3::new(61.5, 33.0, 61.5),
        Vec3::new(60.8, 14.0, 60.8),
    );
    shoot(&mut renderer, &app.world, "voxel_light.bmp");

    // 3. 空中俯瞰：拉高相机俯视丘陵、平台与光井塔楼顶部。
    set_camera(
        &mut app.world,
        Vec3::new(40.0, 52.0, 100.0),
        Vec3::new(64.0, 18.0, 64.0),
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

/// 在平台北侧盖一座 5×5 的光井塔：石墙、木板屋顶，内部净空 3×3、
/// 高 19 格，南墙在出生点旁留一个 1×2 门洞，井底中央嵌一块荧光石。
/// 光井完全由方块编辑接口搭建，光照随每次写入增量更新；从塔顶俯视
/// 可以看到暖光沿井壁向上衰减到完全熄灭。
fn build_light_shaft(world: &mut World) {
    const X0: i32 = 59;
    const X1: i32 = 63;
    const Z0: i32 = 59;
    const Z1: i32 = 63;
    for z in Z0..=Z1 {
        for x in X0..=X1 {
            apply_edit(world, IVec3::new(x, 14, z), Block::Stone);
            let perimeter = x == X0 || x == X1 || z == Z0 || z == Z1;
            for y in 15..=33 {
                let door = z == Z1 && x == 61 && (15..=16).contains(&y);
                let block = if door {
                    Block::Air
                } else if perimeter {
                    Block::Stone
                } else {
                    Block::Air
                };
                apply_edit(world, IVec3::new(x, y, z), block);
            }
            apply_edit(world, IVec3::new(x, 34, z), Block::Planks);
        }
    }
    apply_edit(world, IVec3::new(61, 14, 61), Block::Glowstone);
}
