//! 集成测试：IBL 环境光照（程序化天空 + 分割求和）。
//!
//! 核心断言：光滑铬球（金属度 1、粗糙度 0.05）在「IBL 开」时应反射出
//! 蓝色的天空辐射（画面出现成片蓝主导像素），「IBL 关」（intensity=0）
//! 时同一区域接近黑暗——这是环境镜面反射的视觉签名，能同时暴露
//! 烘焙失败（立方体贴图全黑）、采样绑定错误、卷积/预滤波数值错误等问题。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

fn scene(env_intensity: f32) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    // 弱方向光 + 低环境补光：让铬球亮度几乎全部来自 IBL
    world.insert_resource(DirectionalLight {
        intensity: 0.3,
        ambient: Vec3::splat(0.02),
        ..DirectionalLight::default()
    });
    world.insert_resource(EnvironmentSettings { intensity: env_intensity });

    let sphere = world.resource_mut::<MeshStore>().add(MeshData::uv_sphere(1.0, 48, 24));
    let chrome =
        world.resource_mut::<MaterialStore>().add(MaterialDef::metal(0.95, 0.95, 0.98, 0.05));
    world.spawn((Transform::from_xyz(0.0, 1.0, 0.0), sphere, chrome));

    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let mat_ground =
        world.resource_mut::<MaterialStore>().add(MaterialDef::solid(0.25, 0.25, 0.25, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_ground));

    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 1.4, 4.5);
        camera.target = Vec3::new(0.0, 1.0, 0.0);
    }

    propagate_transforms(&mut world);
    world
}

#[test]
fn ibl_reflects_sky_on_chrome() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    renderer.render(&scene(1.0));
    let with_ibl = renderer.read_pixels();
    renderer.render(&scene(0.0));
    let without_ibl = renderer.read_pixels();

    assert_eq!(with_ibl.len(), without_ibl.len());

    // 铬球屏幕中心附近区域：上半球反射蓝天（蓝主导），下半球反射地面（暖色）。
    // 取画面中央 1/4 区域统计「蓝主导」像素（b 显著大于 r）。
    let (w, h) = renderer.size();
    let mut bluish_on = 0u32;
    let mut bluish_off = 0u32;
    let mut brightened = 0u32;
    for y in h / 4..h * 3 / 4 {
        for x in w / 4..w * 3 / 4 {
            let i = ((y * w + x) * 4) as usize;
            let (r, g, b) =
                (with_ibl[i] as i32, with_ibl[i + 1] as i32, with_ibl[i + 2] as i32);
            let (r0, g0, b0) = (
                without_ibl[i] as i32,
                without_ibl[i + 1] as i32,
                without_ibl[i + 2] as i32,
            );
            if b > r + 20 && b > 60 {
                bluish_on += 1;
            }
            if b0 > r0 + 20 && b0 > 60 {
                bluish_off += 1;
            }
            if r + g + b > r0 + g0 + b0 + 30 {
                brightened += 1;
            }
        }
    }
    assert!(
        bluish_on > 300,
        "IBL 开启时铬球应反射出成片蓝天像素，实际 {bluish_on}"
    );
    assert!(
        bluish_on > bluish_off * 4,
        "IBL 开/关的蓝主导像素对比度过低：开 {bluish_on} vs 关 {bluish_off}"
    );
    assert!(
        brightened > 300,
        "IBL 应给场景带来环境辐射（成片变亮），实际变亮 {brightened} 像素"
    );
}
