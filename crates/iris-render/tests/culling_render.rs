//! 集成测试：视锥剔除统计口径与蒙皮保守剔除（R22）。
//!
//! 两条主线：
//! 1. **统计一致性**：N 个立方体、相机只朝向其中一部分——
//!    `visible + culled == N`、`triangles_drawn == visible × 单网格三角形数`
//!    （精确断言）、`draw_calls` 与批数一致；画面像素侧证「朝向的可见、
//!    背对的全无」，相机转 180° 后 culled 集合对调。
//! 2. **蒙皮剔除 + 反误杀回归**：绑定姿势 AABB 在原点附近，但关节矩阵把
//!    几何刚性移到 (10, 0, 0)——相机看向 (10, 0, 0) 时该蒙皮实例必须可见
//!    （保守并集界生效；误用绑定姿势包围盒剔除就会误杀，本测试要能抓住），
//!    背对时计入 `culled_instances`。
//!
//! 环境无 GPU 适配器时优雅跳过（打印提示，测试仍算通过）。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 每个立方体网格的三角形数（6 面 × 2）。
const CUBE_TRIANGLES: u32 = 12;

/// 自发光红材质（与蓝天空背景易区分，计数方式与 skin_render.rs 一致）。
fn red_material(world: &mut World) -> MaterialHandle {
    world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [0.9, 0.15, 0.1, 1.0],
        emissive: [2.5, 0.28, 0.28, 1.0],
        ..MaterialDef::default()
    })
}

/// 统计全屏红像素数（自发光红不受光照影响，与天空色对比明确）。
fn count_red(px: &[u8], w: u32, h: u32) -> u32 {
    let mut total = 0u32;
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let (r, g, b) = (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32);
            if r > 100 && r > g + 40 && r > b + 40 {
                total += 1;
            }
        }
    }
    total
}

/// 空世界 + 目录资源 + 红材质立方体网格（无地面，保证统计精确可加）。
fn base_scene() -> (World, MeshHandle, MaterialHandle) {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let red = red_material(&mut world);
    (world, cube, red)
}

/// 统计一致性 + 相机 180° 对调（R22 统计口径的 GPU 验收）：
/// 相机在原点，前方（-Z，深度 12）4 个立方体、后方（+Z）6 个，同网格
/// 同材质 → 1 批。看向 -Z：visible 4 / culled 6 / 三角形 4×12 / 1 draw
/// call；转 180° 看向 +Z 后 culled 集合对调（visible 6 / culled 4）。
/// 两组均要求红像素为正（朝向的可见），且对调后红像素更多（6 个 > 4 个）。
#[test]
fn culling_stats_consistent_and_swap_with_camera_flip() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let (mut world, cube, red) = base_scene();

    // 前排 4 个（x = -3, -1, 1, 3）：看向 -Z 时全部位于视锥内
    // （深度 12、横向最远 3 + 半径 ~0.87 < 视锥半宽 ~4.97，余量充足）
    for i in 0..4 {
        let x = (i as f32 - 1.5) * 2.0;
        world.spawn((Transform::from_xyz(x, 0.5, -12.0), cube, red));
    }
    // 后排 6 个（x = ±0.6..±3，间距 1.2 不相接）：位于相机背后，全部剔除；
    // 横向最远 3 + 0.87 < 4.97，转 180° 后全部进入视锥（对调可精确断言）
    for i in 0..6 {
        let x = (i as f32 - 2.5) * 1.2;
        world.spawn((Transform::from_xyz(x, 0.5, 12.0), cube, red));
    }
    propagate_transforms(&mut world);

    // ---- 朝向一：看向 -Z（前排可见、后排剔除）----
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 0.5, 0.0);
        camera.target = Vec3::new(0.0, 0.5, -12.0);
    }
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(stats.instances, 4, "前排 4 个立方体应可见");
    assert_eq!(stats.culled_instances, 6, "相机背后的 6 个立方体应全部剔除");
    assert_eq!(
        stats.instances + stats.culled_instances,
        10,
        "visible + culled 必须等于实体总数 N = 10"
    );
    assert_eq!(
        stats.triangles_drawn,
        4 * CUBE_TRIANGLES,
        "主 pass 三角形 = 可见实例 × 单网格三角形数（精确口径）"
    );
    assert_eq!(stats.draw_calls, 1, "同网格同材质应合并为单批");
    let (w, h) = renderer.size();
    let red_px_front = count_red(&renderer.read_pixels(), w, h);
    assert!(
        red_px_front > 200,
        "朝向前排的立方体必须出现在画面中，红像素 {red_px_front}"
    );

    // ---- 朝向二：转 180° 看向 +Z（后排可见、前排剔除）----
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.target = Vec3::new(0.0, 0.5, 12.0);
    }
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(stats.instances, 6, "转 180° 后原被剔除的后排应全部可见");
    assert_eq!(stats.culled_instances, 4, "culled 集合应与前一对调");
    assert_eq!(
        stats.instances + stats.culled_instances,
        10,
        "对调后 visible + culled 仍须等于 N = 10"
    );
    assert_eq!(stats.triangles_drawn, 6 * CUBE_TRIANGLES);
    assert_eq!(stats.draw_calls, 1);
    let red_px_back = count_red(&renderer.read_pixels(), w, h);
    assert!(
        red_px_back > red_px_front,
        "6 个可见立方体的红像素（{red_px_back}）应多于 4 个时（{red_px_front}）"
    );
}

/// 蒙皮保守剔除 + 反误杀回归（R22 正确性核心）：
/// 绑定姿势 AABB（[-0.5, 0.5]³，原点附近）与关节矩阵刚性移动后的实际
/// 几何位置 (10, 0, 0) 相距 10 m——相机在 (10, 0.5, 8)：
/// - 看向 (10, 0.5, 0)：绑定 AABB 在原点、横向偏移 10 >> 视锥半宽 ~3.3，
///   **在视锥外**；蒙皮几何在视锥内。实例必须可见且画面出现红像素——
///   误用绑定姿势包围盒剔除会同时栽在这两条断言上；
/// - 转为看向 +Z：蒙皮几何与绑定 AABB 均在视锥外，计入 culled_instances。
///
/// 关节摆法：关节实体全局平移 (10, 0, 0)，ibm = 恒等，蒙皮实体在原点
/// （model = 恒等）→ J = T(10, 0, 0)，全部顶点权重 1 绑该关节，
/// 蒙皮后几何 = 绑定网格整体刚移到 (10, 0, 0)。
#[test]
fn skinned_culling_tracks_joint_transformed_geometry() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let (mut world, _, red) = base_scene();

    // 蒙皮立方体网格：24 顶点全部绑关节 0（权重 1）
    let mut skinned_cube = MeshData::cube();
    let vertex_count = skinned_cube.vertices.len();
    skinned_cube.skin = Some(vec![
        SkinVertex {
            joints: [0, 0, 0, 0],
            weights: [1.0, 0.0, 0.0, 0.0],
        };
        vertex_count
    ]);
    let mesh = world.resource_mut::<MeshStore>().add(skinned_cube);

    let joint = world.spawn((Transform::from_xyz(10.0, 0.0, 0.0),));
    let skin = Skin::new(vec![joint], vec![glam::Mat4::IDENTITY]);
    world.spawn((Transform::from_xyz(0.0, 0.0, 0.0), mesh, red, skin));
    propagate_transforms(&mut world);

    // ---- 相机看向蒙皮几何 (10, 0, 0)：必须可见（保守并集界生效）----
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(10.0, 0.5, 8.0);
        camera.target = Vec3::new(10.0, 0.5, 0.0);
    }
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(
        stats.instances, 1,
        "关节把几何移入视锥：蒙皮实例必须可见（绑定姿势 AABB 在视锥外，\
         若误用绑定包围盒剔除此断言即失败）"
    );
    assert_eq!(stats.culled_instances, 0);
    assert_eq!(stats.triangles_drawn, CUBE_TRIANGLES);
    assert_eq!(stats.draw_calls, 1, "蒙皮实体独立成项（每实体一次 draw）");
    let (w, h) = renderer.size();
    let red_px = count_red(&renderer.read_pixels(), w, h);
    assert!(
        red_px > 200,
        "蒙皮几何应渲染在 (10, 0, 0) 方向的画面中央，红像素 {red_px}"
    );

    // ---- 相机背对（看向 +Z）：计入 culled_instances ----
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.target = Vec3::new(10.0, 0.5, 16.0);
    }
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(stats.instances, 0, "背对时蒙皮实例应被剔除（零提交）");
    assert_eq!(stats.culled_instances, 1, "蒙皮剔除应计入 culled_instances");
    assert_eq!(stats.triangles_drawn, 0);
    assert_eq!(stats.draw_calls, 0);
    let red_px = count_red(&renderer.read_pixels(), w, h);
    assert_eq!(red_px, 0, "背对时画面不应出现任何蒙皮几何像素");
}
