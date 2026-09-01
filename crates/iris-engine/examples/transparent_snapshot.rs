//! 工具：透明渲染验收场景（离屏渲染输出 `iris_transparent.bmp`）。
//!
//! 场景：玻璃立方体罩住自发光内核（透过玻璃可见内核与 IBL 反射）+
//! 红绿蓝三块彩色透明板沿纵深排列（back-to-front 混合，板间互相透出）
//! + 地面方向光阴影（透明物不投影）。
//!
//! ```sh
//! cargo run --example transparent_snapshot --release
//! ```

use iris_core::hierarchy::propagate_transforms;
use iris_engine::prelude::*;

fn main() {
    env_logger::init();
    let Some(mut renderer) = OffscreenRenderer::new(640, 360) else {
        eprintln!("无 GPU");
        std::process::exit(1);
    };

    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());
    // 自发光内核略过泛光阈值，形成柔和辉光
    world.insert_resource(BloomSettings {
        enabled: true,
        strength: 0.25,
        threshold: 1.0,
    });

    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());

    // 自发光内核：暖橙色，位于原点
    let core = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [0.0, 0.0, 0.0, 1.0],
        emissive: [2.4, 1.1, 0.25, 1.0],
        ..Default::default()
    });
    world.spawn((
        Transform {
            scale: Vec3::new(0.9, 0.9, 0.9),
            ..Transform::IDENTITY
        },
        cube,
        core,
    ));

    // 玻璃罩：低粗糙度绝缘体 + alpha 混合（透过它应看到内核与天空反射）
    let glass = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::glass(0.8, 0.9, 1.0, 0.35));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 0.9, 0.0),
            scale: Vec3::new(1.8, 1.8, 1.8),
            ..Transform::IDENTITY
        },
        cube,
        glass,
    ));

    // 三块彩色透明板沿纵深排列（远蓝 ← 中绿 ← 近红），横向错开使
    // 板间互相透出、纵深混合层次可见
    let plates: [(f32, f32, [f32; 4]); 3] = [
        (-1.1, -2.6, [0.25, 0.45, 0.95, 0.45]), // 远：蓝
        (0.6, -1.0, [0.3, 0.9, 0.4, 0.4]),      // 中：绿
        (-0.4, 1.4, [0.95, 0.3, 0.3, 0.4]),     // 近：红
    ];
    for (x, z, rgba) in plates {
        let mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
            base_color: rgba,
            alpha_mode: AlphaMode::Blend,
            ..Default::default()
        });
        world.spawn((
            Transform {
                position: Vec3::new(x, 1.0, z),
                scale: Vec3::new(2.0, 2.0, 0.06),
                ..Transform::IDENTITY
            },
            cube,
            mat,
        ));
    }

    // 地面（受影：阴影只来自不透明几何——玻璃罩与透明板都不投影）
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let ground = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.5, 0.52, 0.55, 1.0));
    world.spawn((Transform::IDENTITY, plane, ground));

    // 相机：略俯视，看清纵深排列与玻璃罩内部
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 2.2, 7.0);
        camera.target = Vec3::new(0.0, 0.8, -0.6);
    }

    propagate_transforms(&mut world);
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    println!(
        "FrameStats: draw_calls={}, instances={}, triangles={}, culled={}",
        stats.draw_calls, stats.instances, stats.triangles_drawn, stats.culled_instances
    );
    renderer.save_bmp("iris_transparent.bmp").expect("写 BMP");
    println!("已输出 iris_transparent.bmp");
}
