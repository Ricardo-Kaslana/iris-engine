//! 集成测试：离屏渲染端到端验证（顶点/片段着色、深度、MSAA、实例化）。
//!
//! 环境无 GPU 适配器时优雅跳过（打印提示，测试仍算通过）。

use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Quat, Transform, World};
use iris_render::prelude::*;

fn scene() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());

    // 旋转 0.6 弧度的立方体悬在地面上方
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat =
        world.resource_mut::<MaterialStore>().add(MaterialDef::solid(0.9, 0.3, 0.1, 1.0));
    let mut transform = Transform::from_xyz(0.0, 0.6, 0.0);
    transform.rotation = Quat::from_rotation_y(0.6);
    world.spawn((transform, cube, mat));

    // 地面
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(20.0));
    let mat_ground =
        world.resource_mut::<MaterialStore>().add(MaterialDef::solid(0.3, 0.33, 0.4, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_ground));

    // 模拟 HierarchyPlugin：主循环之外需要手动执行全局变换传播
    propagate_transforms(&mut world);
    world
}

/// 两个像素是否为同一颜色（天空背景下角点互证：同一高度的左右角点
/// 天空色相同——仍能捕获全零回读与区域错位）。
fn same_color(px: &[u8], a: usize, b: usize) -> bool {
    (0..3).all(|c| (px[a + c] as i32 - px[b + c] as i32).abs() <= 8)
}

#[test]
fn offscreen_renders_lit_cube() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let world = scene(); // scene() 内部已完成传播
    renderer.render(&world);
    let (w, h) = renderer.size();
    let px = renderer.read_pixels();
    assert_eq!(px.len(), (w * h * 4) as usize);

    // 1. 画面上缘左右角点应为同一片天空色（场景不会铺满画面；
    //    全零回读或天空 pass 失效会在此暴露）
    let corner_l = ((2 * w + 2) as usize) * 4;
    let corner_r = ((2 * w + w - 3) as usize) * 4;
    assert!(
        same_color(&px, corner_l, corner_r),
        "上缘左右角点应为同一片天空色，实际 {:?} vs {:?}",
        &px[corner_l..corner_l + 4],
        &px[corner_r..corner_r + 4]
    );

    // 2. 画面中心应命中立方体（与天空色不同）
    let center = ((h / 2) * w + w / 2) as usize * 4;
    assert!(
        !same_color(&px, center, corner_l),
        "画面中心应为立方体而非天空，实际 RGBA {:?}",
        &px[center..center + 4]
    );

    // 3. 天空覆盖率应在合理区间（相机俯视原点，天空只占上缘一条；
    //    天空全无说明几何铺满异常，天空占大半说明几何未绘制）
    let mut sampled = 0u32;
    let mut sky = 0u32;
    for y in (0..h).step_by(4) {
        for x in (0..w).step_by(4) {
            let i = ((y * w + x) as usize) * 4;
            sampled += 1;
            if same_color(&px, i, corner_l) {
                sky += 1;
            }
        }
    }
    let ratio = sky as f32 / sampled as f32;
    assert!(
        (0.05..0.9).contains(&ratio),
        "天空覆盖率 {ratio:.3} 超出合理区间，几何或天空 pass 异常"
    );
}

#[test]
fn offscreen_renders_many_instances() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = scene();
    // 5×5 网格铺 24 个新实例（复用句柄 0 网格 / 1 材质）→ 立方体共 25 个
    for x in 0..5u32 {
        for z in 0..5u32 {
            if x == 0 && z == 0 {
                continue; // 场景里已有第一个
            }
            world.spawn((
                Transform::from_xyz(1.4 * x as f32, 0.6, 1.4 * z as f32),
                MeshHandle(0),
                MaterialHandle(1),
            ));
        }
    }
    // 相机拉远，保证 25 个立方体全部位于视锥内（剔除行为由专门测试覆盖）
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = glam::Vec3::new(5.0, 9.0, 12.0);
        camera.target = glam::Vec3::new(2.8, 0.0, 2.8);
    }
    propagate_transforms(&mut world);
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(stats.instances, 26, "25 个立方体实例 + 1 个地面实例（全部在视锥内）");
    // 25 个立方体（同网格同材质）合并为 1 个 draw call + 地面 1 个
    assert_eq!(stats.draw_calls, 2, "26 个实例应只需 2 个 draw call");
}

#[test]
fn frustum_culls_entities_behind_camera() {
    let Some(mut renderer) = OffscreenRenderer::new(128, 128) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = scene();
    // 场景基线：立方体 + 地面 = 2 实例 / 2 draw call
    propagate_transforms(&mut world);
    renderer.render(&world);
    let baseline = renderer.renderer().stats();
    assert_eq!(baseline.instances, 2);
    assert_eq!(baseline.draw_calls, 2);

    // 在相机背后（+Z 方向，相机看向 -Z）放 27 个立方体 → 全部应被剔除
    for y in 0..3u32 {
        for x in 0..3u32 {
            for z in 0..3u32 {
                world.spawn((
                    Transform::from_xyz(2.0 * x as f32, 0.6 + 2.0 * y as f32, 20.0 + 2.0 * z as f32),
                    MeshHandle(0),
                    MaterialHandle(1),
                ));
            }
        }
    }
    propagate_transforms(&mut world);
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(stats.instances, 2, "相机背后的 27 个实体必须全部被视锥剔除");
    assert_eq!(stats.draw_calls, 2, "剔除后 draw call 不应增加");

    // 对照组：把其中 1 个立方体放到视锥内 → 应被提交并合入既有批次
    world.spawn((Transform::from_xyz(0.0, 0.6, -2.0), MeshHandle(0), MaterialHandle(1)));
    propagate_transforms(&mut world);
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(stats.instances, 3, "视锥内的立方体应被提交");
    assert_eq!(stats.draw_calls, 2, "与既有立方体同批，draw call 数不变");
}
