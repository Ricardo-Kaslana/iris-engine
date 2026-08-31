//! 集成测试：实例缓冲跨帧持久化与增量上传。
//!
//! 静态场景连续渲染应整帧跳过实例上传（`instance_upload_bytes == 0`）且
//! 像素逐帧一致；移动单个实体后仅变化段上传（字节数小于全量）且画面中
//! 方块位置确实变化。
//!
//! 环境无 GPU 适配器时优雅跳过（打印提示，测试仍算通过）。

use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Entity, Quat, Transform, World};
use iris_render::prelude::*;

/// 方块 + 地面的静态场景，返回场景与方块实体句柄。
fn scene() -> (World, Entity) {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());

    // 旋转 0.6 弧度的立方体悬在地面上方
    let cube_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat =
        world.resource_mut::<MaterialStore>().add(MaterialDef::solid(0.9, 0.3, 0.1, 1.0));
    let mut transform = Transform::from_xyz(0.0, 0.6, 0.0);
    transform.rotation = Quat::from_rotation_y(0.6);
    let cube = world.spawn((transform, cube_mesh, mat));

    // 地面
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(20.0));
    let mat_ground =
        world.resource_mut::<MaterialStore>().add(MaterialDef::solid(0.3, 0.33, 0.4, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_ground));

    // 模拟 HierarchyPlugin：主循环之外需要手动执行全局变换传播
    propagate_transforms(&mut world);
    (world, cube)
}

/// 两帧像素的差分字节数（RGBA 逐字节比较）。
fn diff_bytes(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b.iter()).filter(|(x, y)| x != y).count()
}

#[test]
fn static_scene_skips_instance_upload() {
    let Some(mut renderer) = OffscreenRenderer::new(128, 128) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let (world, _) = scene();
    // 首帧：实例缓冲初建 → 全量上传
    renderer.render(&world);
    let first = renderer.read_pixels();
    let baseline = renderer.renderer().stats();
    assert_eq!(baseline.instances, 2, "方块 + 地面均应在视锥内");
    assert!(
        baseline.instance_upload_bytes > 0,
        "首帧必须上传实例（缓冲初建后无镜像可比）"
    );
    assert_eq!(
        baseline.instance_upload_bytes,
        baseline.instances as u64 * 112,
        "首帧上传字节数应为全量（实例数 × 112B）"
    );

    // 第 2、3 帧：内容与上帧逐项一致 → 零上传，且像素与首帧完全一致
    for frame in 2..=3 {
        renderer.render(&world);
        let stats = renderer.renderer().stats();
        assert_eq!(
            stats.instance_upload_bytes, 0,
            "第 {frame} 帧静态场景应整帧跳过实例上传"
        );
        assert_eq!(stats.instances, baseline.instances, "实例数不应变化");
        assert_eq!(
            renderer.read_pixels(),
            first,
            "第 {frame} 帧像素必须与首帧完全一致（跳过上传不得改变画面）"
        );
    }
}

#[test]
fn moved_entity_uploads_partial_range() {
    let Some(mut renderer) = OffscreenRenderer::new(128, 128) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let (mut world, cube) = scene();
    renderer.render(&world);
    let before = renderer.read_pixels();
    let total_instances = renderer.renderer().stats().instances as u64;
    let full_bytes = total_instances * 112;

    // 静态基线：第 2 帧应零上传
    renderer.render(&world);
    assert_eq!(
        renderer.renderer().stats().instance_upload_bytes,
        0,
        "移动前静态帧应零上传"
    );

    // 横移方块 1.5 单位（仍在视锥内）→ 仅方块实例（单一连续变化段）变化
    world.get_mut::<Transform>(cube).expect("方块实体存在").position.x = 1.5;
    propagate_transforms(&mut world);
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(stats.instances as u64, total_instances, "移动不改变实例数");
    assert!(stats.instance_upload_bytes > 0, "移动实体后必须有实例上传");
    assert!(
        stats.instance_upload_bytes < full_bytes,
        "单实体移动应只上传变化段（{} < 全量 {}）",
        stats.instance_upload_bytes,
        full_bytes
    );

    // 画面中方块确实移动：与移动前帧差分像素 > 0
    let after = renderer.read_pixels();
    let diff = diff_bytes(&before, &after);
    assert!(diff > 0, "移动方块后画面必须变化（差分 {diff} 字节）");
}
