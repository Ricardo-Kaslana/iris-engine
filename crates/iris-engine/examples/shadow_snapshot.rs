//! 工具：PBR / 阴影 / HDR 视觉验收场景（离屏渲染输出 `iris_shadow_preview.bmp`）。
//!
//! 场景：一排不同粗糙度的金属球 + 一颗绝缘体球 + 地面方向光阴影。
//! 金属球应呈现彩色反射高光，粗糙度越高高光越大越暗；绝缘体只有白色高光。
//!
//! ```sh
//! cargo run --example shadow_snapshot --release
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
    // 光强调到 2.5：让金属高光进入 ACES 色调映射的滚降区（HDR 验收点）
    world.insert_resource(DirectionalLight {
        intensity: 2.5,
        ..DirectionalLight::default()
    });

    let sphere = world.resource_mut::<MeshStore>().add(MeshData::uv_sphere(0.8, 48, 24));

    // 金属球：金 / 铜 / 银色，粗糙度 0.05 → 0.5 递增
    let metals: [([f32; 3], f32); 4] = [
        ([1.0, 0.78, 0.34], 0.05),  // 抛光金
        ([0.95, 0.55, 0.30], 0.2),  // 铜
        ([0.95, 0.95, 0.98], 0.35), // 银
        ([0.85, 0.85, 0.85], 0.55), // 粗糙钢
    ];
    for (i, (color, roughness)) in metals.iter().enumerate() {
        let x = -3.3 + i as f32 * 2.2;
        let mat = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::metal(color[0], color[1], color[2], *roughness));
        world.spawn((Transform::from_xyz(x, 0.8, 0.0), sphere, mat));
    }

    // 绝缘体红球（只有白色高光）
    let mat_red = world.resource_mut::<MaterialStore>().add(MaterialDef::solid(0.9, 0.15, 0.1, 1.0));
    world.spawn((Transform::from_xyz(5.8, 0.8, 0.0), sphere, mat_red));

    // 地面
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let mat_g = world.resource_mut::<MaterialStore>().add(MaterialDef::solid(0.55, 0.55, 0.55, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_g));

    // 相机稍微拉高看全景
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 3.2, 8.5);
        camera.target = Vec3::new(0.0, 0.6, 0.0);
    }

    propagate_transforms(&mut world);
    renderer.render(&world);
    renderer.save_bmp("iris_shadow_preview.bmp").expect("写 BMP");
    println!("已输出 iris_shadow_preview.bmp");
}
