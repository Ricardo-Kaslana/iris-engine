//! 集成测试：泛光（Bloom）后处理。
//!
//! 核心断言：同一场景分别以「开泛光 / 关泛光」渲染一帧，整帧差分应表现为
//! 「发光体周围成片像素变亮、几乎没有像素变暗、远场不变」——
//! 泛光是 ACES 之前的纯加法（色调映射单调），因此数学上不可能变暗；
//! 该性质能同时暴露阈值失效（全屏泛光）、模糊绑定错误（无变化）、
//! 合成通道错误（画面异常）等问题。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

fn scene(bloom_enabled: bool) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());
    if !bloom_enabled {
        world.insert_resource(BloomSettings::disabled());
    }

    // 高自发光球：HDR 值远超阈值 1.0，是唯一的泛光源
    let sphere = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, 32, 16));
    let mat_glow = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [0.02, 0.02, 0.02, 1.0],
        emissive: [18.0, 12.0, 7.0, 1.0],
        ..MaterialDef::solid(0.0, 0.0, 0.0, 1.0)
    });
    world.spawn((Transform::from_xyz(0.0, 1.2, 0.0), sphere, mat_glow));

    // 暗地面（照明后亮度远低于泛光阈值，不应产生泛光）
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let mat_ground = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.22, 0.22, 0.25, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_ground));

    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 1.6, 6.0);
        camera.target = Vec3::new(0.0, 1.2, 0.0);
    }

    propagate_transforms(&mut world);
    world
}

#[test]
fn bloom_brightens_glow_without_darkening_anything() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    renderer.render(&scene(true));
    let with_bloom = renderer.read_pixels();
    renderer.render(&scene(false));
    let without_bloom = renderer.read_pixels();

    assert_eq!(with_bloom.len(), without_bloom.len());

    let (w, h) = renderer.size();
    let mut brightened = 0u32;
    let mut darkened = 0u32;
    let mut unchanged = 0u32;
    for i in (0..with_bloom.len()).step_by(4) {
        let sum_on = with_bloom[i] as i32 + with_bloom[i + 1] as i32 + with_bloom[i + 2] as i32;
        let sum_off =
            without_bloom[i] as i32 + without_bloom[i + 1] as i32 + without_bloom[i + 2] as i32;
        if sum_on > sum_off + 12 {
            brightened += 1;
        } else if sum_on < sum_off - 12 {
            darkened += 1;
        } else {
            unchanged += 1;
        }
    }
    let total_pixels = ((w * h) as f32) * 0.9;
    assert!(
        (brightened as f32) > total_pixels * 0.005,
        "开泛光后发光体周围应有成片像素变亮（光晕），实际 {brightened}/{total_pixels:.0}"
    );
    assert!(
        darkened <= 4,
        "泛光是 ACES 之前的纯加法，不应有任何像素显著变暗，实际 {darkened}"
    );
    assert!(
        (unchanged as f32) > total_pixels * 0.5,
        "远场（天空/远地面）不应受泛光影响"
    );
}
