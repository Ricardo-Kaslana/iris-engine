//! 集成测试：形变目标 v1（C4a）端到端——CPU 混合上传的逐实体顶点缓冲
//! 经蒙皮绘制分支（主 / 阴影）进屏幕。
//!
//! 场景：单位立方体（单关节恒等蒙皮）浮在灰色地面上方，一个形变目标
//! 把全部顶点沿 +X 推 1.5。核心断言三件：
//! 1. **零权重逐位锚（像素级）**：挂 `MorphWeights [0.0]` 与不挂组件的
//!    帧逐字节一致——覆盖缓冲内容与基座一致 ⇒ 光栅化一致；
//! 2. **位移判别**：权重 1.0 时红色体整体右移（最左红列越过无位移时的
//!    最右红列）——暴露「混合 → 上传 → slot 0 覆盖」任一环脱落；
//! 3. **阴影一致性**：位移帧与原位帧在「剪影之外、纯地面」的观测带上
//!    差分像素显著——若阴影 pass 掉回基座 VBO，投影不会跟着走。
//!
//! 脏检口径由 `morph_uploads` 观测计数钉住：连续同权重帧不重上传。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 文件内互斥串行化（重负载 GPU 套件并行会挂死，AGENTS 规约）。
static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 位移目标：全部 24 顶点 +X。
fn shifted_cube() -> MeshData {
    let cube = MeshData::cube();
    let morph = MorphData::new(vec![MorphTarget::new(
        Some("shift_x".into()),
        vec![[1.5, 0.0, 0.0]; cube.vertices.len()],
        None,
    )]);
    let skin = vec![
        SkinVertex {
            joints: [0, 0, 0, 0],
            weights: [1.0, 0.0, 0.0, 0.0],
        };
        cube.vertices.len()
    ];
    MeshData {
        skin: Some(skin),
        morph: Some(morph),
        ..cube
    }
}

struct Scene {
    world: World,
    entity: iris_core::prelude::Entity,
}

/// 恒等单关节蒙皮的红色立方体浮在地面上方（y=2），阴影开。
fn build_scene(with_ground: bool) -> Scene {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(3.0, 3.0, 8.5),
        target: Vec3::new(0.8, 1.5, 0.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.4, -1.0, -0.3),
        shadows: with_ground,
        ..DirectionalLight::default()
    });
    if with_ground {
        let ground = world.resource_mut::<MeshStore>().add(MeshData::plane(30.0));
        let mat = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.55, 0.55, 0.6, 1.0));
        world.spawn((Transform::from_xyz(0.0, 0.0, 0.0), ground, mat));
    }
    // 关节与蒙皮实体同位：J = inv(mesh全局) × global(joint) × ibm = 恒等
    // ⇒ 立方体悬在 y=2（贴地则阴影被地面自遮挡，判别失效）
    let joint = world.spawn((Transform::from_xyz(0.0, 2.0, 0.0),));
    let mesh = world.resource_mut::<MeshStore>().add(shifted_cube());
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.9, 0.12, 0.1, 1.0));
    let entity = world.spawn((
        Transform::from_xyz(0.0, 2.0, 0.0),
        mesh,
        mat,
        Skin::new(vec![joint], vec![glam::Mat4::IDENTITY]),
    ));
    propagate_transforms(&mut world);
    Scene { world, entity }
}

/// 红色像素的最左 / 最右列与总数（背景蓝灰天空易区分）。
fn red_span(px: &[u8], w: u32, h: u32) -> (u32, u32, u32) {
    let mut min_x = u32::MAX;
    let mut max_x = 0u32;
    let mut total = 0u32;
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let (r, g, b) = (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32);
            if r > 100 && r > g + 40 && r > b + 40 {
                total += 1;
                min_x = min_x.min(x);
                max_x = max_x.max(x);
            }
        }
    }
    (min_x, max_x, total)
}

#[test]
fn morph_weights_blend_shifts_and_stays_bitwise_when_zero() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(128, 128) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Scene { mut world, entity } = build_scene(false);

    // 帧 A：组件缺席 = 现路径基线
    renderer.render(&world);
    let pixels_absent = renderer.read_pixels();

    // 帧 B：零权重 ⇒ 覆盖缓冲内容 = 基座 ⇒ 逐字节一致（像素级逐位锚）
    world.insert(entity, MorphWeights::new(vec![0.0]));
    renderer.render(&world);
    let pixels_zero = renderer.read_pixels();
    assert_eq!(
        pixels_absent, pixels_zero,
        "零权重帧必须与组件缺席帧逐字节一致"
    );
    // 脏检：权重位图案未变 ⇒ 不重上传（首次挂上已建缓冲 + 传一次）
    let uploads_after_zero = renderer.renderer().morph_uploads();
    renderer.render(&world);
    assert_eq!(
        renderer.renderer().morph_uploads(),
        uploads_after_zero,
        "同权重连续帧不得重上传"
    );

    // 帧 C：权重 1.0 ⇒ 全顶点 +X 1.5 ⇒ 红色体整体右移
    world.insert(entity, MorphWeights::new(vec![1.0]));
    renderer.render(&world);
    let pixels_shifted = renderer.read_pixels();
    assert_eq!(
        renderer.renderer().morph_uploads(),
        uploads_after_zero + 1,
        "权重变化恰好触发一次重上传"
    );
    let (w, h) = renderer.size();
    let (_min_a, max_a, total_a) = red_span(&pixels_absent, w, h);
    let (min_c, max_c, total_c) = red_span(&pixels_shifted, w, h);
    assert!(total_a > 100, "基线立方体应可见，红像素 {total_a}");
    assert!(
        min_c > max_a,
        "位移后最左红列 {min_c} 应越过基线最右红列 {max_a}（+X 1.5 位移未生效？）"
    );
    assert!(
        total_c > total_a * 3 / 4,
        "位移不应让几何体缩水或撕裂：{total_a} -> {total_c}"
    );
    assert!(max_c > max_a, "红像素应向右扩展：{max_a} -> {max_c}");
}

#[test]
fn morph_shadow_follows_displaced_geometry() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(128, 128) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Scene { mut world, entity } = build_scene(true);

    renderer.render(&world);
    let base = renderer.read_pixels();
    world.insert(entity, MorphWeights::new(vec![1.0]));
    renderer.render(&world);
    let shifted = renderer.read_pixels();

    let (w, h) = renderer.size();
    // 地面阴影带（行 72..100：立方体剪影在行 45..66 之外，此带内只有
    // 地面与其投影）：位移 ⇒ 投影跟着走。若阴影 pass 用基座 VBO
    // （覆盖未接进阴影分支），此带逐像素不变 ⇒ 差分为 0。
    let mut diff = 0u32;
    for y in 72..100.min(h) {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let on: i32 = (0..3).map(|k| base[i + k] as i32).sum();
            let off: i32 = (0..3).map(|k| shifted[i + k] as i32).sum();
            if (on - off).abs() > 12 {
                diff += 1;
            }
        }
    }
    assert!(diff > 30, "位移应带动地面阴影变化，地面带差分像素 {diff}");
}

/// 形变网格同句柄更新：权重位图案不变也必须重新混合上传（`MorphGpu`
/// 的网格句柄与修订号脏检），否则画面会继续使用旧基座的混合结果。
#[test]
fn morph_mesh_update_at_same_handle_reblends_with_same_weights() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(128, 128) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Scene { mut world, entity } = build_scene(false);
    world.insert(entity, MorphWeights::new(vec![1.0]));
    renderer.render(&world);
    let before = renderer.read_pixels();
    let uploads_before = renderer.renderer().morph_uploads();

    let mesh = *world.get::<MeshHandle>(entity).expect("实体应挂网格句柄");
    let mut updated = shifted_cube();
    // 基座几何沿 +Z 平移 0.5：权重不变，混合结果必须随之变化。
    for vertex in &mut updated.vertices {
        vertex.position[2] += 0.5;
    }
    assert!(
        world.resource_mut::<MeshStore>().update(mesh, updated),
        "存活形变网格更新必须命中"
    );
    renderer.render(&world);
    let after = renderer.read_pixels();
    assert_eq!(
        renderer.renderer().morph_uploads(),
        uploads_before + 1,
        "网格修订号变化必须强制重新混合并上传一次"
    );
    assert!(before != after, "基座几何变化必须反映到画面");
}
