//! 集成测试：硬件遮挡查询剔除（R53，`occlusion` 模块）。
//!
//! 三条主线：
//! 1. **剔除生效 + 保守性（像素级）**：立柱完全挡住的小盒——开启
//!    [`OcclusionCulling`] 稳定两帧后 `occlusion_culled == 1`、
//!    `instances` 少 1，而**画面与关闭时逐位一致**（被剔者对本帧像素
//!    零贡献：AABB 保守界 + 查询语义是保守下界，论证见 occlusion.rs；
//!    阴影 pass 与深度副本保留完整列表，像素一致性不被阴影侧打破）；
//! 2. **缺席/关闭 = 零变化**：资源缺席与显式 `enabled = false` 画面
//!    逐位一致，且全帧 FNV-1a 哈希锁定（与 `lightmap_render.rs` 同款
//!    兼容锁——默认路径命令流与 R52c 之后不变）；
//! 3. **两阶段时延语义**：第一帧无结果可用 ⇒ 恒全部可见（`occlusion_culled
//!    == 0`）；静止场景第三帧起稳态（结果与像素逐帧一致 = 确定性）。
//!
//! 环境无 GPU 适配器时优雅跳过（打印提示，测试仍算通过）。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 缺席路径全帧 FNV-1a 兼容锁（本场景在 R53 实现后首跑实测锁定；
/// 语义 = 遮挡资源缺席时命令流与 R52c 之后逐位一致）。
const R53_OCCLUSION_OFF_BASELINE: u64 = 0x0369_3e06_193a_9c14;

/// 室内三件套 + 立柱：地板、后墙、立柱、柱后完全被挡的小盒。
/// 相机注视立柱中心；方向光阴影显式关闭（隔离遮挡语义，像素一致性
/// 论证不依赖阴影侧）。盒的屏幕矩形必须严格落在立柱轮廓内（AABB
/// 保守界才能全深度测试失败）：
/// - 立柱 x∈[−0.5,0.5]、y∈[0,2]、z∈[−0.5,0.5]（距离相机 ≈4.5，
///   角半径 ≈0.11 rad）；
/// - 小盒中心 (0, 1, −3)、半边 0.2（距离 ≈6.5，角半径 ≈0.03 rad）
///   ⇒ 水平垂直均有 >3× 余量。
fn room_world() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight {
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());

    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(20.0));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat_wall = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.75, 0.72, 0.66, 1.0));
    let mat_dark = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.25, 0.25, 0.3, 1.0));

    // 地板（y=0）
    world.spawn((Transform::IDENTITY, plane, mat_wall));
    // 后墙（z=−6，立起）：plane 绕 X 转 −90° 使正面朝 +Z
    world.spawn((
        Transform {
            rotation: glam::Quat::from_rotation_x(-90f32.to_radians()),
            position: Vec3::new(0.0, 3.0, -6.0),
            scale: Vec3::new(1.0, 6.0, 1.0),
        },
        plane,
        mat_wall,
    ));
    // 立柱（画面中央）
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 1.0, 0.0),
            scale: Vec3::new(1.0, 2.0, 1.0),
            ..Transform::IDENTITY
        },
        cube,
        mat_dark,
    ));
    // 柱后小盒（完全被挡）
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 1.0, -3.0),
            scale: Vec3::splat(0.4),
            ..Transform::IDENTITY
        },
        cube,
        mat_dark,
    ));

    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 1.2, 4.5);
        camera.target = Vec3::new(0.0, 1.0, 0.0);
    }
    propagate_transforms(&mut world);
    world
}

/// 主测试：剔除生效 + 保守性像素逐位一致 + 稳态确定性。
#[test]
fn occlusion_culls_fully_hidden_objects_without_pixel_change() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    // ---- 基线：关闭遮挡（资源缺席），首帧即最终画面 ----
    let mut world = room_world();
    renderer.render(&world);
    let stats_off = renderer.renderer().stats();
    let pixels_off = renderer.read_pixels();
    assert_eq!(
        stats_off.occlusion_culled, 0,
        "遮挡关闭时 occlusion_culled 恒 0"
    );
    assert_eq!(
        fnv1a(&pixels_off),
        R53_OCCLUSION_OFF_BASELINE,
        "缺席路径的兼容锁：命令流必须与 R52c 之后逐位一致"
    );

    // ---- 开启遮挡：帧 1 无结果（全部可见）、帧 2 起裁决生效 ----
    world.insert_resource(OcclusionCulling::ENABLED);
    renderer.render(&world);
    let stats_f1 = renderer.renderer().stats();
    let pixels_f1 = renderer.read_pixels();
    assert_eq!(pixels_off, pixels_f1, "帧 1 全部可见，画面与基线逐位一致");
    assert_eq!(
        stats_f1.occlusion_culled, 0,
        "读回延迟一帧：第一帧无结果可用，必须全部可见（保守方向）"
    );
    assert_eq!(
        stats_f1.instances, stats_off.instances,
        "第一帧实例数与关闭时一致"
    );

    renderer.render(&world);
    let stats_f2 = renderer.renderer().stats();
    let pixels_f2 = renderer.read_pixels();
    assert_eq!(
        stats_f2.occlusion_culled, 1,
        "柱后小盒的世界 AABB 完全落在立柱轮廓内 ⇒ 查询计 0、被剔除"
    );
    assert_eq!(
        stats_f2.instances + 1,
        stats_off.instances,
        "被剔实例退出主 pass：instances 恰少 1（同时计入 culled 口径）"
    );
    assert_eq!(
        stats_off.culled_instances + 1,
        stats_f2.culled_instances,
        "遮挡剔除计入 culled_instances（单列字段同步加）"
    );
    assert_eq!(
        pixels_off, pixels_f2,
        "保守性：被剔者全部采样深度测试失败 = 对本帧像素零贡献 ⇒ \
         开启遮挡的画面与关闭逐位一致"
    );

    // ---- 稳态：帧 3 与帧 2 的统计与像素逐位一致（确定性）----
    renderer.render(&world);
    let stats_f3 = renderer.renderer().stats();
    let pixels_f3 = renderer.read_pixels();
    assert_eq!(stats_f3.occlusion_culled, 1, "静止场景稳态持续剔除");
    assert_eq!(stats_f3.instances, stats_f2.instances);
    assert_eq!(pixels_f2, pixels_f3, "稳态逐帧像素必须逐位一致（确定性）");
}

/// 显式 `enabled = false` 与资源缺席逐位一致（缺席 = 关的语义锁）。
#[test]
fn explicit_disabled_matches_absent_baseline() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = room_world();
    renderer.render(&world);
    let absent = renderer.read_pixels();

    world.insert_resource(OcclusionCulling { enabled: false });
    renderer.render(&world);
    let disabled = renderer.read_pixels();
    assert_eq!(
        absent, disabled,
        "显式关闭与资源缺席必须逐位一致（缺席 = 关）"
    );
    assert_eq!(
        fnv1a(&disabled),
        R53_OCCLUSION_OFF_BASELINE,
        "关闭路径同样受兼容锁保护"
    );
}

/// 能力探测口径（回退契约）：wgpu 30 起遮挡查询为核心能力，各后端
/// 恒可用；「无能力 = 全部可见」由「读回不新鲜 ⇒ 不剔除」的执行路径
/// 承担（帧 1 断言已在主测试锁定此语义）。
#[test]
fn occlusion_query_is_a_core_capability() {
    let Some(gpu) = Gpu::offscreen() else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    assert!(
        gpu.supports_occlusion_query(),
        "wgpu 30 遮挡查询为核心能力，探测点必须返回 true（见 context.rs 文档）"
    );
}

/// 全帧像素 FNV-1a 64（与 `lightmap_render.rs` / `post_render.rs` 同款
/// 兼容锁哈希）。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// 射线隐藏盒摆法验证（example 二分）：隐藏盒中心 = 相机→柱心射线
/// 延长 dist+2.6 m 处（与 example 同口径），必须计 0 被剔。
#[test]
fn ray_hidden_boxes_are_culled() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = room_world();
    let eye = Vec3::new(0.0, 1.2, 4.5);
    let center = Vec3::new(0.0, 1.0, 0.0);
    let dir = (center - eye).normalize();
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.9, 0.8, 0.1, 1.0));
    world.spawn((
        Transform {
            position: eye + dir * (eye.distance(center) + 2.6),
            scale: Vec3::splat(0.7),
            ..Transform::IDENTITY
        },
        cube,
        mat,
    ));
    propagate_transforms(&mut world);
    world.insert_resource(OcclusionCulling::ENABLED);
    // 帧间 read_pixels（阻塞 poll）派发 map 回调：紧凑循环里 GPU 退休
    // 需要真实时间，纯 draw 背靠背会让读回永不到达（60fps 事件循环
    // 的帧间隔天然满足，见 occlusion.rs 读回节拍说明）。
    for _ in 0..3 {
        renderer.render(&world);
        let _ = renderer.read_pixels();
    }
    let s = renderer.renderer().stats();
    println!(
        "ray 隐藏盒：occ={} inst={} culled={}",
        s.occlusion_culled, s.instances, s.culled_instances
    );
    assert!(
        s.occlusion_culled >= 1,
        "射线隐藏盒必须被遮挡，occ={}",
        s.occlusion_culled
    );
}
