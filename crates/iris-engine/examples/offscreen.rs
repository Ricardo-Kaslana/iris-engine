//! 示例：无窗口离屏渲染，输出 BMP 截图。
//!
//! 验证整条 GPU 管线（顶点/片段着色、深度、MSAA、实例化）无需窗口参与，
//! 适用于 CI 或服务器环境。
//!
//! ```sh
//! cargo run --example offscreen --release
//! ```

use iris_core::hierarchy::propagate_transforms;
use iris_engine::prelude::*;

fn main() {
    env_logger::init();
    let Some(mut renderer) = OffscreenRenderer::new(1280, 720) else {
        eprintln!("没有可用的 GPU 适配器（离屏渲染需要 Vulkan / DirectX 12 / Metal）");
        std::process::exit(1);
    };

    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());

    // 立方体 + 地面
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.9, 0.45, 0.2, 1.0));
    let mut transform = Transform::from_xyz(0.0, 0.6, 0.0);
    transform.rotation = Quat::from_rotation_y(0.6);
    world.spawn((transform, cube, mat));

    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(20.0));
    let mat_ground = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.3, 0.33, 0.4, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_ground));

    // 两盏彩色点光（渲染器每帧自动收集，上限 8 盏）
    world.spawn((
        Transform::from_xyz(-1.8, 1.2, 1.2),
        PointLight::new(Vec3::new(0.3, 0.6, 1.0), 14.0, 9.0),
    ));
    world.spawn((
        Transform::from_xyz(1.8, 1.0, -1.0),
        PointLight::new(Vec3::new(1.0, 0.25, 0.2), 12.0, 8.0),
    ));

    // 离屏渲染没有主循环：手动执行层级传播，生成 GlobalTransform
    propagate_transforms(&mut world);

    renderer.render(&world);
    renderer
        .save_bmp("iris_offscreen.bmp")
        .expect("写入 BMP 失败");
    let (w, h) = renderer.size();
    println!("已输出 iris_offscreen.bmp（{w}×{h}，无窗口离屏渲染）");
}
