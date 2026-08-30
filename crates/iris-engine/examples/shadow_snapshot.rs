//! 临时验收工具：纯方向光场景渲染阴影快照（验收后可删除）。

use iris_core::hierarchy::propagate_transforms;
use iris_engine::prelude::*;

fn main() {
    env_logger::init();
    let Some(mut renderer) = OffscreenRenderer::new(1280, 720) else {
        eprintln!("无 GPU");
        std::process::exit(1);
    };

    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());

    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world.resource_mut::<MaterialStore>().add(MaterialDef::solid(0.9, 0.3, 0.1, 1.0));
    world.spawn((Transform::from_xyz(0.0, 1.5, 0.0), cube, mat));
    let sphere = world.resource_mut::<MeshStore>().add(MeshData::uv_sphere(0.8, 32, 20));
    let mat_b = world.resource_mut::<MaterialStore>().add(MaterialDef::solid(0.25, 0.6, 0.95, 1.0));
    world.spawn((Transform::from_xyz(2.5, 0.8, -1.5), sphere, mat_b));
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let mat_g = world.resource_mut::<MaterialStore>().add(MaterialDef::solid(0.55, 0.55, 0.55, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_g));

    propagate_transforms(&mut world);
    renderer.render(&world);
    renderer.save_bmp("iris_shadow_preview.bmp").expect("写 BMP");
    println!("已输出 iris_shadow_preview.bmp");
}
