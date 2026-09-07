//! 工具：R51 地形系统（heightmap 分块 + 3 级距离 LOD + 碰撞拼块集成）的
//! headless 断言 + 近 / 远双时刻视觉验收（离屏渲染 → `docs/terrain.png`）。
//!
//! 场景：[`generate_hills`] 丘陵地形（64×64 cell、0.5 m 格距、32 m 见方，
//! seed 固定 ⇒ 同输入逐位一致）+ [`build_terrain`] 铺世界（4 个 chunk
//! 实体各挂 [`LodMesh3`]，碰撞拼块 `collider_stride: 2` = 每 2×2 cell 一
//! 盒、顶面贴 cell 最高角），材质 = [`grass_textures`] 程序化草地（基色 /
//! 法线 / ORM 三件套，单纹理按世界 XZ 平铺）。第三人称角色
//! （[`move_and_slide`] 胶囊控制器，**零物理扩展**）从丘陵西侧沿 +X
//! 走 4 s 贴坡爬丘：
//!
//! - **近景帧**：相机跟在角色身后（第三人称位），近处 chunk 走高模、
//!   远处掉级——打印每 chunk 到相机的距离与选中级；
//! - **拉远帧**：相机升到地形东南外侧高空（四块距离 ≫ 两级阈值）→
//!   整图 LOD2，**headless 精确对账**：`triangles_drawn` 恰等
//!   4 × LOD2 三角形数 + 角色恒细模件数（96 + 768）、
//!   `culled_instances == 0`、且显著低于近景帧（LOD 生效的
//!   统计证据）；角色脚底贴坡 `|feet − height_at_world| ≤ 0.1 m`
//!   （= 坡面单 cell 抬升上界 `amplitude × 0.112` + 余量，见常量注释）
//!   且终态 grounded——断言失败直接 panic；
//! - 两帧横拼输出 `docs/terrain.png`（2560×720，image 直接拼接）。
//!
//! 目检要点：近景 = 丘陵起伏 + 草地法线细节 + 角色贴坡站立不悬空不
//! 陷地；拉远 = 整片丘陵轮廓完整、**chunk 接缝处无裂缝**、材质跨块
//! 连续（接缝不变量与 UV 平铺的视觉证据）、三角形统计骤降。
//!
//! ```sh
//! cargo run -p iris-engine --example terrain_snapshot --release
//! ```

use image::GenericImage;

use iris_core::hierarchy::propagate_transforms;
use iris_engine::prelude::{
    Camera, CharacterParams, DirectionalLight, MaterialDef, MaterialStore, MeshData, MeshStore,
    OffscreenRenderer, PointLight, Terrain, TextureStore, Transform, Vec3, build_terrain,
    generate_hills, grass_textures, move_and_slide, terrain_statics,
};
use iris_render::prelude::LodMesh3;

/// 地形格距（米，2 的幂 ⇒ 接缝 x/z 逐位一致的口径前提）。
const CELL: f32 = 0.5;
/// 地形 cell 边长（64 ⇒ 32 m 见方；heightmap 顶点 65×65）。
const CELLS: usize = 64;
/// 丘陵振幅（米）。相邻格点高差上界 = `amplitude × 0.112`（三频叠加
/// 的方向导数界，见 terrain.rs 生成器文档）≈ 0.09 m ⇒ 角色 / 导航
/// 默认 step_height 0.3 下全程可行走。
const AMPLITUDE: f32 = 0.8;
/// 角色步行速度（米/秒，+X）与固定 60 Hz 步长 × 帧数（4 s）。
const WALK_SPEED: f32 = 3.0;
const WALK_FRAMES: usize = 240;
const DT: f32 = 1.0 / 60.0;
/// 贴坡容差（米）：碰撞盒顶 = cell 最高角，与中心双线性高的偏差 ≤
/// 坡面单 cell 抬升 ≈ 0.09 m，再留 1 cm 吸附 / 舍入余量。
const STAND_TOLERANCE: f32 = 0.1;
/// LOD 两级切换距离（米）：拉远帧 eye 到最近 chunk 角 > 30 ⇒ 全 LOD2。
const SWITCH_DISTANCES: [f32; 2] = [14.0, 30.0];

/// 测试台：世界 + 地形 + 角色（控制器状态即权威，Transform 直写；
/// 无窗口 / 无音频 / 无调度——固定步手动推进）。
struct Harness {
    world: iris_core::prelude::World,
    terrain: Terrain,
    lod_triangles: [u32; 3],
    /// 四块 chunk 的原点（= 角点世界位置，`LodMesh3` 距离口径）。
    chunk_corners: Vec<Vec3>,
    character: iris_core::prelude::Entity,
    pos: Vec3,
    vel: Vec3,
    grounded: bool,
    params: CharacterParams,
}

impl Harness {
    fn new() -> Self {
        let mut world = iris_core::prelude::World::new();
        world.insert_resource(MeshStore::new());
        world.insert_resource(MaterialStore::new());
        world.insert_resource(TextureStore::new());
        world.insert_resource(Camera::default());
        world.insert_resource(DirectionalLight {
            intensity: 1.0,
            ambient: Vec3::splat(0.16),
            ..DirectionalLight::default()
        });
        // 暖色高点光：丘陵向阳面 / 背光面分离 → 目检起伏
        world.spawn((
            Transform::from_xyz(-6.0, 14.0, 12.0),
            PointLight {
                color: Vec3::new(1.0, 0.92, 0.78),
                intensity: 420.0,
                range: 120.0,
                shadows: true,
            },
        ));

        // ---- 草地材质（程序化三件套，世界 XZ 每 4 m 一重复）----
        let grass = grass_textures(256);
        let (albedo, normal, orm) = {
            let mut textures = world.resource_mut::<TextureStore>();
            (
                textures.add(grass.albedo),
                textures.add(grass.normal),
                textures.add(grass.orm),
            )
        };
        let material = world.resource_mut::<MaterialStore>().add(MaterialDef {
            base_color: [0.9, 0.95, 0.9, 1.0],
            roughness: 1.0,
            texture: albedo,
            normal_texture: Some(normal),
            orm_texture: Some(orm),
            ..MaterialDef::default()
        });

        // ---- 地形：丘陵 + 分块 + 碰撞拼块（stride 2 ⇒ 1024 盒）----
        let origin = Vec3::new(1.0, 0.0, 1.0);
        let heightmap = generate_hills(CELLS + 1, CELLS + 1, CELL, 2026, 0.8, AMPLITUDE);
        let options = iris_render::prelude::TerrainOptions {
            origin,
            chunk_cells: 32,
            collider_stride: 2,
            uv_tile: 4.0,
            switch_distances: SWITCH_DISTANCES,
        };
        let build = build_terrain(&mut world, heightmap, material, &options);
        let terrain = world
            .get::<Terrain>(build.root)
            .expect("根实体挂 Terrain")
            .clone();
        let chunk_corners: Vec<Vec3> = build
            .chunks
            .iter()
            .map(|&e| world.get::<Transform>(e).expect("chunk Transform").position)
            .collect();
        // 三级句柄全部可解析（MeshStore 有实体）
        for &chunk in &build.chunks {
            let lod3 = world.get::<LodMesh3>(chunk).expect("chunk 挂 LodMesh3");
            for &level in &lod3.levels {
                assert!(
                    world.resource::<MeshStore>().get(level).is_some(),
                    "LOD 级网格句柄有效"
                );
            }
        }

        // ---- 角色：胶囊视觉（圆柱身 + 子实体头球），控制器权威 ----
        let body = world
            .resource_mut::<MeshStore>()
            .add(MeshData::cylinder(0.35, 1.1, 24));
        let head = world
            .resource_mut::<MeshStore>()
            .add(MeshData::uv_sphere(0.28, 24, 16));
        let suit = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.85, 0.45, 0.15, 1.0));
        let character = world.spawn((Transform::IDENTITY, body, suit));
        let head_entity = world.spawn((Transform::from_xyz(0.0, 0.75, 0.0), head, suit));
        world.add_child(character, head_entity);

        // 出生：地形西侧 4 m、悬空 1.2 m（前几帧自由落体贴地）
        let start = Vec3::new(origin.x + 4.0, 0.0, origin.z + 16.0);
        let start_y = terrain
            .height_at_world(start.x, start.z)
            .expect("出生点在地形域内")
            + 1.2;
        let pos = Vec3::new(start.x, start_y, start.z);
        Harness {
            world,
            terrain,
            lod_triangles: build.lod_triangles,
            chunk_corners,
            character,
            pos,
            vel: Vec3::new(WALK_SPEED, 0.0, 0.0),
            grounded: false,
            params: CharacterParams::default(),
        }
    }

    /// 固定步推进角色（`move_and_slide` 世界封装：静态体收集 →「重力 /
    /// 滑行 / 台阶 / 坡限 / 吸附」管线），写回 Transform + 层级传播。
    fn step(&mut self) {
        let out = move_and_slide(&self.world, self.pos, self.vel, DT, &self.params);
        self.pos = out.position;
        self.vel = Vec3::new(WALK_SPEED, out.velocity.y, 0.0);
        self.grounded = out.grounded;
        if let Some(mut transform) = self.world.get_mut::<Transform>(self.character) {
            transform.position = self.pos;
        }
        propagate_transforms(&mut self.world);
    }

    fn walk(&mut self, frames: usize) {
        for _ in 0..frames {
            self.step();
        }
    }

    /// 每 chunk 的 LOD 选择明细（复算 `LodMesh3::select` 口径）——
    /// 打印与断言共用。
    fn lod_levels(&self, eye: Vec3) -> Vec<(f32, usize)> {
        self.chunk_corners
            .iter()
            .map(|&corner| {
                let dist = eye.distance(corner);
                let level = if dist <= SWITCH_DISTANCES[0] {
                    0
                } else if dist <= SWITCH_DISTANCES[1] {
                    1
                } else {
                    2
                };
                (dist, level)
            })
            .collect()
    }
}

fn main() {
    env_logger::init();
    let Some(mut renderer) = OffscreenRenderer::new(1280, 720) else {
        eprintln!("无 GPU");
        std::process::exit(1);
    };

    let mut harness = Harness::new();

    // ---- 角色贴坡行走（碰撞拼块 = 台阶语义，引擎零扩展）----
    harness.walk(WALK_FRAMES);
    let surface = harness
        .terrain
        .height_at_world(harness.pos.x, harness.pos.z)
        .expect("落点在域内");
    let feet = harness.pos.y - harness.params.half_height;
    assert!(harness.grounded, "终态应贴地（缓丘可行走断言）");
    assert!(
        (feet - surface).abs() <= STAND_TOLERANCE,
        "脚底 {feet} 应贴合 height_at_world {surface}（±{STAND_TOLERANCE}）"
    );
    let statics = terrain_statics(&harness.world, &harness.terrain);
    assert_eq!(statics.len(), CELLS / 2 * CELLS / 2, "stride 2 ⇒ 1024 盒");
    println!(
        "角色行走 {WALK_FRAMES} 帧：x {:.1} → {:.1}、脚底 {feet:.3} vs 地表 {surface:.3}（贴合 ≤ {STAND_TOLERANCE}）、碰撞拼块 {} 盒",
        harness.chunk_corners[0].x + 4.0,
        harness.pos.x,
        statics.len()
    );

    // ---- 近景帧（第三人称）----
    let near_eye = harness.pos + Vec3::new(-4.5, 2.6, 4.5);
    let near_target = harness.pos + Vec3::new(4.0, 0.2, 0.0);
    {
        let mut camera = harness.world.resource_mut::<Camera>();
        camera.eye = near_eye;
        camera.target = near_target;
        camera.fov_y_radians = 1.1;
    }
    renderer.render(&harness.world);
    let near_stats = renderer.renderer().stats();
    let near_detail = harness
        .lod_levels(near_eye)
        .iter()
        .map(|(d, l)| format!("{d:.1} m→LOD{l}"))
        .collect::<Vec<_>>()
        .join("、");
    println!(
        "近景帧：triangles_drawn = {}、instances = {}、culled = {}；chunk 距离与选级：{near_detail}",
        near_stats.triangles_drawn, near_stats.instances, near_stats.culled_instances
    );
    let near_pixels = renderer.read_pixels();

    // ---- 拉远帧（全图超阈值 → 全 LOD2，精确对账）----
    let far_eye = Vec3::new(17.0, 26.0, 62.0);
    let far_target = Vec3::new(17.0, 0.5, 17.0);
    {
        let mut camera = harness.world.resource_mut::<Camera>();
        camera.eye = far_eye;
        camera.target = far_target;
        camera.fov_y_radians = std::f32::consts::FRAC_PI_4;
    }
    renderer.render(&harness.world);
    let far_stats = renderer.renderer().stats();
    let far_report = harness.lod_levels(far_eye);
    // 场景固定含角色（圆柱身 24 段 = 96 三角、头球 24×16 = 768 三角，
    // 无 LOD 恒细模）——统计口径 = 4 chunk + 角色两件。
    const CHARACTER_TRIANGLES: u32 = 96 + 768;
    assert_eq!(far_stats.instances, 6, "拉远帧四块 chunk + 角色身/头全收集");
    assert_eq!(far_stats.culled_instances, 0, "拉远俯视整图无剔除");
    let want_far = 4 * harness.lod_triangles[2] + CHARACTER_TRIANGLES;
    assert_eq!(
        far_stats.triangles_drawn, want_far,
        "拉远帧应全 LOD2（4 × {} = {want_far}），实际 {}",
        harness.lod_triangles[2], far_stats.triangles_drawn
    );
    assert!(
        far_report.iter().all(|&(_, level)| level == 2),
        "拉远帧距离判定须全 LOD2：{far_report:?}"
    );
    assert!(
        near_stats.triangles_drawn > want_far,
        "近景帧三角形数 {} 应大于全 LOD2 的 {want_far}（LOD 生效证据）",
        near_stats.triangles_drawn
    );
    println!(
        "LOD 生效证据：每 chunk 三级三角形 {} / {} / {}；近景 {} → 拉远 {want_far}（骤降）",
        harness.lod_triangles[0],
        harness.lod_triangles[1],
        harness.lod_triangles[2],
        near_stats.triangles_drawn
    );
    let far_pixels = renderer.read_pixels();

    // ---- 左右横拼 → docs/terrain.png ----
    let (w, h) = renderer.size();
    let mut canvas = image::RgbaImage::new(w * 2, h);
    let left = image::RgbaImage::from_raw(w, h, near_pixels).expect("近景像素尺寸匹配");
    let right = image::RgbaImage::from_raw(w, h, far_pixels).expect("拉远像素尺寸匹配");
    canvas.copy_from(&left, 0, 0).expect("近景拼入画布");
    canvas.copy_from(&right, w, 0).expect("拉远拼入画布");
    canvas
        .save("docs/terrain.png")
        .expect("写 docs/terrain.png（从仓库根目录运行）");
    println!(
        "已输出 docs/terrain.png（{}x{}）：左 = 第三人称贴坡近景、右 = 高空拉远（LOD2 整图）",
        w * 2,
        h
    );
}
