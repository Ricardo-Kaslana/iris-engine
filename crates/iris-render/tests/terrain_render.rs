//! 集成测试：地形（R51）——分块 3 级距离 LOD 的 GPU 端到端 + 角色贴坡。
//!
//! - **LOD 生效**（`FrameStats::triangles_drawn` 精确对账）：三 chunk
//!   横排，相机近处按 `LodMesh3` 阈值逐级切换——近帧混合级（LOD0 +
//!   LOD1 + LOD2 各一，三角形数 = 三级之和精确相等）、远帧整图最粗级
//!   （骤降），证明收集期选择、合批与统计全按选中网格走；
//! - **地形可见**：绿色调像素在两帧都存在（画面「不变太离谱」宽松证据）；
//! - **角色贴坡**：缓丘上 `move_and_slide` 推进若干帧，脚底贴合
//!   `Terrain::height_at_world`（±2 cm 容差依据见测试内注释）；
//! - **兼容零开销**：`LodMesh3` 从未注册的世界照常渲染（既有网格 / 蒙皮
//!   收集路径不受影响，口径照 `lod_render.rs` / `skin_render.rs` 全过）。
//!
//! 无 GPU 环境优雅跳过（`OffscreenRenderer::new` 返回 `None`）。

use glam::Vec3;
use iris_core::character::{CharacterParams, move_and_slide_statics};
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 三 chunk 横排世界：24×8 cell（12×4 m），chunk 8 cell 边长，
/// LOD 切换距离 [5.5, 9]（配合下方相机几何给出 0/1/2 各一的混合级）。
fn hills_world() -> (World, TerrainBuild) {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight {
        intensity: 1.0,
        ..DirectionalLight::default()
    });
    let grass = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [0.25, 0.45, 0.18, 1.0],
        emissive: [0.05, 0.11, 0.04, 1.0],
        roughness: 0.9,
        ..MaterialDef::default()
    });
    let hm = generate_hills(25, 9, 0.5, 11, 1.0, 0.8);
    let options = TerrainOptions {
        chunk_cells: 8,
        switch_distances: [5.5, 9.0],
        ..TerrainOptions::default()
    };
    let build = build_terrain(&mut world, hm, grass, &options);
    propagate_transforms(&mut world);
    (world, build)
}

/// 统计绿主导像素数（与天空 / 背景区分：g 明显高于 r、b）。
fn count_green(px: &[u8], w: u32, h: u32) -> u32 {
    let mut total = 0u32;
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let (r, g, b) = (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32);
            if g > 40 && g > r + 20 && g > b + 20 {
                total += 1;
            }
        }
    }
    total
}

#[test]
fn terrain_lod_switches_by_distance_and_renders() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let (world, build) = hills_world();
    let (t0, t1, t2) = (
        build.lod_triangles[0] as usize,
        build.lod_triangles[1] as usize,
        build.lod_triangles[2] as usize,
    );
    assert_eq!(build.chunks.len(), 3);
    assert!(t0 > t1 && t1 > t2, "三角形数须逐级递减：{t0}, {t1}, {t2}");

    // ---- 近帧：eye (-2, 4, -2) → 三 chunk 角点距离 ≈ 4.9 / 7.5 / 11.0
    //（LOD0 / LOD1 / LOD2 各一；`FrameStats.triangles_drawn` 精确对账）----
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(-2.0, 4.0, -2.0);
        camera.target = Vec3::new(6.0, 1.0, 0.0);
    }
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(stats.instances, 3, "三 chunk 全部收集");
    assert_eq!(stats.culled_instances, 0, "近帧无剔除（视锥覆盖整条地形）");
    assert_eq!(
        stats.triangles_drawn as usize,
        t0 + t1 + t2,
        "近帧每 chunk 选中各自 LOD 级：LOD0({t0}) + LOD1({t1}) + LOD2({t2})"
    );
    let (w, h) = renderer.size();
    let green_near = count_green(&renderer.read_pixels(), w, h);
    assert!(green_near > 500, "近帧地形应大面积可见，{green_near}");

    // ---- 远帧：eye (-2, 60, -2)（距离全部 ≫ 9）→ 整图最粗级 ----
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(-2.0, 60.0, -2.0);
        camera.target = Vec3::new(6.0, 1.0, 0.0);
    }
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(stats.instances, 3);
    assert_eq!(stats.culled_instances, 0);
    assert_eq!(
        stats.triangles_drawn as usize,
        3 * t2,
        "远帧整图 LOD2（{t0}+{t1}+{t2} → 3×{t2} 骤降 = LOD 生效）"
    );
    let green_far = count_green(&renderer.read_pixels(), w, h);
    assert!(green_far > 0, "远帧地形仍应可见（宽松像素证据）{green_far}");
}

/// 缓丘贴坡：amp 0.15 的丘陵（相邻格点高差 ≲ 0.016 m = 2× 验收容差），
/// 角色 `move_and_slide` 推进 90 帧后脚底贴合 `height_at_world`——
/// 容差 2 cm 的依据：碰撞盒顶 = cell 最高角，与中心双线性高的偏差
/// ≤ 单 cell 坡面抬升 ≈ amplitude × 0.224 × 0.5·√2 / 1.75 …。
#[test]
fn character_stands_flush_on_hills() {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    let hm = generate_hills(25, 25, 0.5, 5, 0.6, 0.15);
    let grass = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.3, 0.5, 0.2, 1.0));
    let build = build_terrain(
        &mut world,
        hm.clone(),
        grass,
        &TerrainOptions {
            chunk_cells: 8,
            ..TerrainOptions::default()
        },
    );
    let terrain = world.get::<Terrain>(build.root).expect("Terrain").clone();
    let statics = terrain_statics(&world, &terrain);
    let params = CharacterParams::default();
    let mut pos = Vec3::new(3.0, 2.0, 4.5);
    let mut vel = Vec3::new(1.5, 0.0, 0.0);
    let mut grounded = false;
    for _ in 0..90 {
        let out = move_and_slide_statics(pos, vel, 1.0 / 60.0, &params, &statics);
        pos = out.position;
        vel = Vec3::new(1.5, out.velocity.y, 0.0);
        grounded = out.grounded;
    }
    assert!(grounded, "90 帧后应贴地（缓丘可站）");
    let surface = terrain.height_at_world(pos.x, pos.z).expect("落点在域内");
    let feet = pos.y - params.half_height;
    assert!(
        (feet - surface).abs() <= 0.02,
        "脚底 {feet} 应贴合 height_at_world {surface}（±2 cm，坡面单 cell 抬升上界）"
    );
}

/// 兼容（缺省零开销）：完全没有地形的世界照常渲染——`LodMesh3` 组件
/// 从未注册时收集路径逐位不受影响（口径照 query.rs「未注册 = 空」）。
#[test]
fn world_without_terrain_renders_unchanged() {
    let Some(mut renderer) = OffscreenRenderer::new(64, 64) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.8, 0.2, 0.2, 1.0));
    world.spawn((Transform::IDENTITY, cube, mat));
    propagate_transforms(&mut world);
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(stats.instances, 1);
    assert_eq!(stats.triangles_drawn, 12, "单立方体 12 三角形（旧语义）");
}
