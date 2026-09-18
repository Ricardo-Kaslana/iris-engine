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

/// 重负载 GPU 套件文件内串行互斥（AGENTS.md 纪律，照 `ssr_render.rs`；
/// C1 轮新增用例时随全文件补齐）。
static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 高模 uv_sphere(1.0, 32, 24) 三角形数：中间带 (24−2) × 32 × 2
/// + 上下极点扇 2 × 32（R58b：极点带改三角扇，消除零面积退化三角）。
const HIGH_TRIANGLES: u32 = (24 - 2) * 32 * 2 + 2 * 32;
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
    let _gpu = GPU_SERIAL.lock().unwrap();
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

// ---------------- C1 · LOD v2（滞回 + 包围球中心，闭合 §8.16） ----------------

/// 带 `LodSettings` 可选资源的对称场景工厂（网格 / 材质创建顺序固定 ⇒
/// 跨帧各世界句柄同值同内容——渲染器按句柄值缓存 GPU 资源的安全性前提）。
fn v2_world(high: MeshData, low: MeshData, settings: Option<LodSettings>) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());
    let h = world.resource_mut::<MeshStore>().add(high);
    let l = world.resource_mut::<MeshStore>().add(low);
    let red = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [0.9, 0.15, 0.1, 1.0],
        emissive: [2.5, 0.28, 0.28, 1.0],
        ..MaterialDef::default()
    });
    world.spawn((
        Transform::IDENTITY,
        LodMesh {
            high: h,
            low: l,
            switch_distance: 10.0,
        },
        red,
    ));
    if let Some(s) = settings {
        world.insert_resource(s);
    }
    propagate_transforms(&mut world);
    world
}

fn set_eye(world: &mut World, d: f32) {
    let mut camera = world.resource_mut::<Camera>();
    camera.eye = Vec3::new(0.0, 0.5, d);
    camera.target = Vec3::ZERO;
}

/// 滞回有效性（GPU 端到端，`triangles_drawn` 即档位口径）：h = 0.2 ⇒
/// 外阈值 12 / 内阈值 8。序列 d = 9 / 11 / 13 / 9 / 7.9 中，第 2 帧
/// （11：v1 会切 low）与第 4 帧（9：v1 会切 high）是**判别点**——
/// 单阈值在这两帧翻转档位，滞回带内维持原档。
#[test]
fn lod_v2_hysteresis_holds_level_across_boundary_oscillation() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let settings = LodSettings {
        hysteresis: 0.2,
        bounds_center: false,
    };
    let mut world = v2_world(
        MeshData::uv_sphere(1.0, 32, 24),
        MeshData::cube(),
        Some(settings),
    );
    let steps: [(f32, u32, &str); 5] = [
        (9.0, HIGH_TRIANGLES, "带内 high（无历史 = 单阈值 ≤10）"),
        (
            11.0,
            HIGH_TRIANGLES,
            "判别点①：11 ∈ (10,12] 滞回维持 high（v1 会切 low）",
        ),
        (13.0, LOW_TRIANGLES, "越外阈值 12 → low"),
        (
            9.0,
            LOW_TRIANGLES,
            "判别点②：9 ∈ [8,10) 滞回维持 low（v1 会切 high）",
        ),
        (7.9, HIGH_TRIANGLES, "进内阈值 8 → high"),
    ];
    // 同世界同渲染器逐帧推进（滞回记忆跨帧演化即被测对象）
    for (d, expect, msg) in steps {
        set_eye(&mut world, d);
        renderer.render(&world);
        let stats = renderer.renderer().stats();
        assert_eq!(stats.triangles_drawn, expect, "d = {d}：{msg}");
    }
}

/// 口径可判别 + 兼容锚（同渲染器多帧，h = 0 ⇒ 记忆不参与 ⇒ 与缺席同判）：
/// 偏心高模（cube 平移 +20Y，包围球中心 (0,20,0)）——eye (1.5,25,0)
/// （刻意偏离 Y 轴避免 look_at 与 up 平行退化）到原点 25.04 > 10（v1
/// 口径 → low），到高模中心 5.22 ≤ 10（bounds_center 口径 → high）。
/// 三帧断言 `triangles_drawn` 在缺席 / `{0,true}` / `{0,false}` 三配置下
/// 按参照系切换，且 h = 0 全程与「无记忆」逐帧独立（第 3 帧即便记忆
/// 残留第 2 帧的 high 也照原点判）。
#[test]
fn lod_v2_bounds_center_switches_reference_and_zero_band_matches_v1() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let eccentric = |offset: f32| {
        let mut m = MeshData::cube();
        for v in m.vertices.iter_mut() {
            v.position[1] += offset;
        }
        m
    };
    let scene = |settings: Option<LodSettings>| {
        let world = v2_world(eccentric(20.0), MeshData::uv_sphere(1.0, 32, 24), settings);
        {
            let mut camera = world.resource_mut::<Camera>();
            camera.eye = Vec3::new(1.5, 25.0, 0.0);
            camera.target = Vec3::new(0.0, 12.0, 0.0);
        }
        world
    };
    // 帧 1：缺席 = v1 原点口径 → 距离 25 > 10 → low（球 1472 三角）
    renderer.render(&scene(None));
    assert_eq!(
        renderer.renderer().stats().triangles_drawn,
        HIGH_TRIANGLES,
        "缺席 = 原点参照 → low 档（球网格）"
    );
    // 帧 2：bounds_center → 中心 (0,20,0) 距离 5 ≤ 10 → high（偏心 cube 12 三角）
    renderer.render(&scene(Some(LodSettings {
        hysteresis: 0.0,
        bounds_center: true,
    })));
    assert_eq!(
        renderer.renderer().stats().triangles_drawn,
        LOW_TRIANGLES,
        "bounds_center = 参照系真实生效（判别）"
    );
    // 帧 3：h = 0 且 center 关 → 即便记忆残留帧 2 的 high，也必须按 v1
    // 单阈值 + 原点重判为 low（h=0 逐位退化锚的 GPU 侧形态）
    renderer.render(&scene(Some(LodSettings {
        hysteresis: 0.0,
        bounds_center: false,
    })));
    assert_eq!(
        renderer.renderer().stats().triangles_drawn,
        HIGH_TRIANGLES,
        "h = 0 ⇒ 记忆不参判，逐位退化 v1"
    );
}
