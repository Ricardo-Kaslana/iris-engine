//! 工具：泛光（Bloom）视觉验收场景（离屏渲染输出 `iris_bloom_preview.bmp`）。
//!
//! 场景：暗地面 + 三颗高自发光彩球（橙 / 青 / 品红）。
//! 发光体应向周围溢出柔和的彩色光晕（HDR 提取 + 半分辨率高斯模糊），
//! 暗部与地面保持干净（阈值提取，无全屏灰雾）。
//!
//! ```sh
//! cargo run --example bloom_snapshot --release
//! ```

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
    // 弱照明 + 低环境光：让发光体成为画面里唯一的亮源
    world.insert_resource(DirectionalLight {
        intensity: 0.6,
        ambient: Vec3::splat(0.05),
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings { strength: 0.45, ..BloomSettings::default() });

    let sphere = world.resource_mut::<MeshStore>().add(MeshData::uv_sphere(0.9, 48, 24));
    let glow = |emissive: [f32; 3]| MaterialDef {
        emissive: [emissive[0], emissive[1], emissive[2], 1.0],
        ..MaterialDef::solid(0.0, 0.0, 0.0, 1.0)
    };
    let mats: [([f32; 3], [f32; 3]); 3] = [
        ([22.0, 9.0, 2.5], [0.0, 1.1, 0.0]),   // 橙
        ([1.5, 14.0, 18.0], [2.6, 1.0, -0.8]), // 青
        ([20.0, 3.5, 13.0], [-2.6, 1.0, -0.8]),// 品红
    ];
    for (emissive, pos) in mats {
        let mat = world.resource_mut::<MaterialStore>().add(glow(emissive));
        world.spawn((Transform::from_xyz(pos[0], pos[1], pos[2]), sphere, mat));
    }

    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let mat_g = world.resource_mut::<MaterialStore>().add(MaterialDef::solid(0.12, 0.12, 0.14, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_g));

    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 2.2, 7.5);
        camera.target = Vec3::new(0.0, 1.1, 0.0);
    }

    propagate_transforms(&mut world);
    renderer.render(&world);
    renderer.save_bmp("iris_bloom_preview.bmp").expect("写 BMP");
    println!("已输出 iris_bloom_preview.bmp");
}
