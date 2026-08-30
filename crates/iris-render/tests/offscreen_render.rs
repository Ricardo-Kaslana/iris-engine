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

/// 背景清屏色的 sRGB 编码（渲染器默认 clearColor 0.055/0.07/0.10 线性）。
fn is_background(px: &[u8], i: usize) -> bool {
    let clear = [64i32, 77, 99]; // ACES 色调映射后的背景 sRGB 值
    (0..3).all(|c| (px[i + c] as i32 - clear[c]).abs() <= 8)
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

    // 1. 画面角落必须是背景清屏色（场景不会铺满画面；全零数据会在此暴露）
    let corner = ((2 * w + 2) as usize) * 4;
    assert!(
        is_background(&px, corner),
        "左上角应为背景色，实际 RGBA {:?}",
        &px[corner..corner + 4]
    );

    // 2. 画面中心应命中立方体（相机注视点在原点上方，非背景）
    let center = ((h / 2) * w + w / 2) as usize * 4;
    assert!(
        !is_background(&px, center),
        "画面中心应为立方体而非背景，实际 RGBA {:?}",
        &px[center..center + 4]
    );

    // 2. 非背景覆盖率应超过 3%（相机距 4.5，边长 1 立方体的合理投影）——
    //    若面环绕顺序写反导致全部背面剔除，覆盖率会趋近 0
    let mut sampled = 0u32;
    let mut covered = 0u32;
    for y in (0..h).step_by(4) {
        for x in (0..w).step_by(4) {
            let i = ((y * w + x) as usize) * 4;
            sampled += 1;
            if !is_background(&px, i) {
                covered += 1;
            }
        }
    }
    let ratio = covered as f32 / sampled as f32;
    assert!(ratio > 0.03, "非背景覆盖率 {ratio:.3} 过低，管线可能失效");
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
