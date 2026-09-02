//! 集成测试：网格距离 LOD（R30）。
//!
//! 同一 `LodMesh` 实体近 / 远两帧（只移动相机）：
//! - 近距（≤ switch_distance）→ `triangles_drawn` 精确等于高模三角形数；
//! - 远距（> switch_distance）→ 精确等于低模三角形数（**下降**即验收
//!   指标；收集 / 合批 / 统计全按选中的网格走）；
//! - 两帧都可见（低模红像素宽松证据——画面「不变太离谱」）且近帧像素
//!   更多（高模更近更大）；
//! - 不触发剔除、不增加 draw call（LOD 切换不产生额外批次）。
//!
//! 选择纯函数 `LodMesh::select` 的单测在 mesh.rs（边界归近侧 / 方向
//! 无关），收集级选网格断言在 renderer.rs 单测；本文件为 GPU 端到端。
//! 无 GPU 环境优雅跳过。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 高模 uv_sphere(1.0, 32, 24) 三角形数：32 × 24 × 6 索引 / 3。
const HIGH_TRIANGLES: u32 = 32 * 24 * 2;
/// 低模 cube 三角形数。
const LOW_TRIANGLES: u32 = 12;

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

#[test]
fn lod_switches_mesh_by_distance_and_reports_triangle_counts() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());
    let high = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, 32, 24));
    let low = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let red = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [0.9, 0.15, 0.1, 1.0],
        emissive: [2.5, 0.28, 0.28, 1.0],
        ..MaterialDef::default()
    });
    world.spawn((
        Transform::IDENTITY,
        LodMesh {
            high,
            low,
            switch_distance: 10.0,
        },
        red,
    ));
    propagate_transforms(&mut world);

    // ---- 近帧：eye (0, 0.5, 5)，距离 ≈ 5.02 ≤ 10 → 高模 ----
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 0.5, 5.0);
        camera.target = Vec3::ZERO;
    }
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(stats.instances, 1, "LOD 实体应正常收集（不重复、不丢失）");
    assert_eq!(stats.culled_instances, 0);
    assert_eq!(
        stats.triangles_drawn, HIGH_TRIANGLES,
        "近距必须提交高模网格（triangles_drawn 即 LOD 验收口径）"
    );
    assert_eq!(stats.draw_calls, 1, "LOD 切换不应产生额外批次");
    let (w, h) = renderer.size();
    let red_near = count_red(&renderer.read_pixels(), w, h);
    assert!(red_near > 200, "近帧高模应清晰可见，红像素 {red_near}");

    // ---- 远帧：eye (0, 0.5, 15)，距离 ≈ 15.01 > 10 → 低模 ----
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 0.5, 15.0);
        camera.target = Vec3::ZERO;
    }
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(stats.instances, 1);
    assert_eq!(stats.culled_instances, 0);
    assert_eq!(
        stats.triangles_drawn, LOW_TRIANGLES,
        "远距必须切换到低模网格（1536 → 12）"
    );
    assert_eq!(stats.draw_calls, 1);
    let red_far = count_red(&renderer.read_pixels(), w, h);
    assert!(
        red_far > 0,
        "远帧低模仍应可见（宽松像素证据），红像素 {red_far}"
    );
    assert!(
        red_near > red_far,
        "近帧高模（更近更大）像素应多于远帧低模：{red_near} vs {red_far}"
    );
}
