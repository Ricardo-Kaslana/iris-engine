//! 集成测试：方向光阴影贴图（深度 pass + PCF 采样）。
//!
//! 核心断言：同一场景分别以「开阴影 / 关阴影」渲染一帧，
//! 整帧差分应表现为「成片像素变暗、几乎没有像素变亮」——
//! 这正是阴影的视觉特征，能同时暴露深度 pass 失效、
//! 采样绑定错误、bias 失当（全亮或全黑）等问题。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

fn scene() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());

    // 悬浮立方体（在地面上方，必然向地面投影）
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world.resource_mut::<MaterialStore>().add(MaterialDef::solid(0.9, 0.3, 0.1, 1.0));
    world.spawn((Transform::from_xyz(0.0, 1.5, 0.0), cube, mat));

    // 地面
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let mat_ground =
        world.resource_mut::<MaterialStore>().add(MaterialDef::solid(0.5, 0.5, 0.5, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_ground));

    propagate_transforms(&mut world);
    world
}

#[test]
fn shadow_darkens_ground_without_brightening_anything() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    // 开阴影（默认）
    let world = scene();
    renderer.render(&world);
    let with_shadows = renderer.read_pixels();

    // 关阴影
    let mut world = scene();
    world.resource_mut::<DirectionalLight>().shadows = false;
    renderer.render(&world);
    let without_shadows = renderer.read_pixels();

    assert_eq!(with_shadows.len(), without_shadows.len());

    let (w, h) = renderer.size();
    let mut darkened = 0u32;
    let mut brightened = 0u32;
    let mut unchanged = 0u32;
    for i in (0..with_shadows.len()).step_by(4) {
        let sum_on = with_shadows[i] as i32 + with_shadows[i + 1] as i32 + with_shadows[i + 2] as i32;
        let sum_off =
            without_shadows[i] as i32 + without_shadows[i + 1] as i32 + without_shadows[i + 2] as i32;
        if sum_on < sum_off - 12 {
            darkened += 1;
        } else if sum_on > sum_off + 12 {
            brightened += 1;
        } else {
            unchanged += 1;
        }
    }
    let total_pixels = ((w * h) as f32) * 0.9;
    assert!(
        (darkened as f32) > total_pixels * 0.005,
        "开阴影后应有成片像素变暗（阴影区域），实际 {darkened}/{total_pixels:.0}"
    );
    assert!(
        brightened <= 4,
        "关->开阴影不应让任何像素显著变亮（MSAA 边缘容差 4 像素），实际 {brightened}"
    );
    assert!(
        unchanged as f32 > total_pixels * 0.5,
        "大部分画面（天空/远地面）不应受阴影影响"
    );
}

#[test]
fn shadow_map_covers_scene_range() {
    // 纯数学验证：以覆盖中心为球心、半径 ≤ shadow_range 的点
    // 必然落在阴影贴图的 NDC 盒内（内切球保证）。
    let light = DirectionalLight::default();
    let range = light.shadow_range;
    let vp = light.shadow_view_proj(Vec3::ZERO, range);
    let corners = [
        Vec3::new(14.0, 8.0, -14.0),
        Vec3::new(-14.0, -5.0, 14.0),
        Vec3::new(10.0, 10.0, 10.0),
    ];
    for c in corners {
        assert!(c.length() <= range, "测试点应位于内切球内");
        let clip = vp * c.extend(1.0);
        let ndc = clip.truncate() / clip.w;
        assert!(
            ndc.x.abs() <= 1.0 && ndc.y.abs() <= 1.0 && (0.0..=1.0).contains(&ndc.z),
            "角点 {c:?} 应位于阴影贴图覆盖范围内：{ndc:?}"
        );
    }
}


