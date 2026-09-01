//! 工具：离屏渲染 `assets/cube.gltf` 并输出 BMP 快照（`iris_gltf_preview.bmp`）。
//!
//! 用于在不打开窗口的情况下验收 glTF 资产管线（外部 buffer、材质、层级）。
//!
//! ```sh
//! cargo run --example gen_asset --release
//! cargo run --example gltf_snapshot --release
//! ```

use iris_core::hierarchy::propagate_transforms;
use iris_engine::prelude::*;

fn main() {
    env_logger::init();
    let Some(mut renderer) = OffscreenRenderer::new(1280, 720) else {
        eprintln!("没有可用的 GPU 适配器");
        std::process::exit(1);
    };

    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());

    match spawn_gltf(&mut world, "assets/cube.gltf") {
        Ok(roots) => {
            println!("glTF 加载成功：{} 个顶层实体", roots.len());
            for root in roots {
                if let Some(mut t) = world.get_mut::<Transform>(root) {
                    t.position.y = 0.6;
                }
            }
        }
        Err(e) => {
            eprintln!("加载 assets/cube.gltf 失败：{e}");
            std::process::exit(1);
        }
    }

    // 地面 + 彩色点光
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(20.0));
    let mat_ground = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.3, 0.33, 0.4, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_ground));
    world.spawn((
        Transform::from_xyz(2.0, 1.5, 2.0),
        PointLight::new(Vec3::new(0.4, 0.7, 1.0), 10.0, 10.0),
    ));

    propagate_transforms(&mut world);
    renderer.render(&world);
    renderer
        .save_bmp("iris_gltf_preview.bmp")
        .expect("写入 BMP 失败");
    println!("已输出 iris_gltf_preview.bmp");
}
