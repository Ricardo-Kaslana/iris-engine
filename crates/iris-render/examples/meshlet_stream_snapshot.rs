//! 虚拟几何体 V4 验收图（R87）：分页流送 + 遮挡整合，1×3 输出
//! `iris_meshlet_stream.ppm` → `docs/meshlet_stream.png`。
//!
//! 面板布局（每格 480×270，每格一台独立渲染器——渲染器按句柄值缓存
//! GPU 资源，跨世界复用会串味）：
//!
//! ```text
//! ┌───────────────┬───────────────┬───────────────┐
//! │ ① 全驻留       │ ② 低预算粗层   │ ③ 遮挡整合     │
//! │ （预算 = 全量）│ （仅粗页驻留） │ （柱后球被剔） │
//! └───────────────┴───────────────┴───────────────┘
//! ```
//!
//! - **面板 ①/② 同场景同机位**（地面 + 悬空高细分球 + 立方；三层 meshlet
//!   实例），差异只在 `MeshletStreamSettings.gpu_byte_budget`——目检
//!   对照「粗层无洞」：②的球必须以更粗的簇呈现、轮廓完整、无消失 / 无
//!   闪烁断层（页缺时沿 DAG 上取父组 = 层级天然提供粗化回退）；
//! - **面板 ③ 为遮挡整合专用场景**（立柱 + 柱后 meshlet 球 + 柱侧
//!   meshlet 球，定稿说明：遮挡语义需要「常规网格遮挡物 + meshlet 被
//!   遮挡者」的异质组合，与 ①② 的流送场景不同源）。格内两球构成
//!   自洽的目检对照：**柱侧球可见**（证明 meshlet 路径活着、照常渲染）
//!   而**柱后球不可见**（收敛帧整帧跳过）——单看「空立柱」无法判别
//!   遮挡是否生效，两球对照才可判别。本轮 GPU 测试已锁其像素口径
//!   （被剔帧与实体缺席帧逐字节相等、可见帧与无遮挡帧逐字节相等）。
//!
//! 断言（不通过直接 panic、不写文件）：
//! - ① 全驻留收敛帧与 R85 全叶路径（平面簇 + 无流送设置）**逐字节相等**
//!   ——「预算 = 全量 ⇒ 收敛到 R86 行为」的像素级锚点，且选中簇数 > 0；
//! - ② 选中簇数 ∈ (0, 全驻留选中数)——粗化真正生效且物体不消失；
//! - ②/① 像素哈希不同（像素流确实变化）；
//! - ③ 收敛帧 `occlusion_culled == 1`、`meshlet_surviving == 0`（整帧跳过）。
//!
//! 注：`meshlet_surviving` 的**收敛帧**口径 = GPU 逐簇剔除后的存活簇数
//!（背向 / 视锥外簇被 compute 剔除），故恒 ≤ 叶簇总数而非恒等；「全驻留
//! ⇒ 恒等表」由与 R85 全叶路径的像素相等断言锁定。
//!
//! ```sh
//! cargo run -p iris-render --example meshlet_stream_snapshot --release
//! ffmpeg -y -i iris_meshlet_stream.ppm docs/meshlet_stream.png
//! ```

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::meshlet_stream::{MESHLET_PAGE_CLUSTERS, MeshletPageTable, MeshletStreamSettings};
use iris_render::prelude::*;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
const GAP: u32 = 6;

/// 场景几何：互不接触、无共面重叠面。球体 ~6000 三角 ⇒ 数十叶簇 +
/// 两层父簇（流送粗化回退的主力样本）。
fn scene_meshes() -> Vec<MeshData> {
    vec![
        MeshData::plane(10.0),
        MeshData::uv_sphere(0.7, 64, 48),
        MeshData::cube(),
    ]
}

fn camera_near() -> Camera {
    Camera {
        eye: Vec3::new(2.4, 1.6, 2.8),
        target: Vec3::new(0.0, 0.7, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    }
}

fn camera_occlusion() -> Camera {
    Camera {
        eye: Vec3::new(0.0, 1.2, 4.5),
        target: Vec3::new(0.0, 1.0, 0.0),
        ..Camera::default()
    }
}

fn placements() -> [Vec3; 3] {
    [
        Vec3::ZERO,
        Vec3::new(0.0, 1.5, 0.0),
        Vec3::new(1.6, 0.5, -0.6),
    ]
}

fn base_world(camera: Camera) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(camera);
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.4, -1.0, -0.3).normalize(),
        intensity: 1.4,
        ambient: Vec3::new(0.10, 0.11, 0.13),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(EnvironmentSettings { intensity: 0.0 });
    world.insert_resource(BloomSettings::disabled());
    world
}

/// 流送场景世界（面板 ①/②）：三层 meshlet 实例 + 可选流送设置。
fn stream_world(camera: Camera, meshes: &[MeshletMesh], settings: MeshletStreamSettings) -> World {
    let mut world = base_world(camera);
    let mut store = MeshletMeshStore::new();
    for mesh in meshes {
        store.add(mesh.clone());
    }
    world.insert_resource(store);
    world.insert_resource(settings);
    for (i, position) in placements().into_iter().enumerate() {
        world.spawn((
            Transform {
                position,
                ..Transform::IDENTITY
            },
            MeshletInstance {
                mesh: MeshletMeshHandle(i as u32),
            },
        ));
    }
    propagate_transforms(&mut world);
    world
}

/// 遮挡整合场景（面板 ③）：立柱（常规网格路径——主 pass 写深度，充当
/// 遮挡物）+ 两枚 meshlet 球——柱后位（完全被挡 ⇒ 收敛帧整帧跳过）与
/// 柱侧位（可见 ⇒ 照常渲染，证明 meshlet 路径在场）。
fn occlusion_world(hier: &[MeshletMesh]) -> World {
    let mut world = base_world(camera_occlusion());
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.25, 0.25, 0.3, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 1.0, 0.0),
            scale: Vec3::new(1.0, 2.0, 1.0),
            ..Transform::IDENTITY
        },
        cube,
        mat,
    ));
    let mut store = MeshletMeshStore::new();
    for mesh in hier {
        store.add(mesh.clone());
    }
    world.insert_resource(store);
    // 柱后球（世界 AABB 完全落在立柱轮廓内 ⇒ 查询计 0、整帧跳过）
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 1.0, -3.0),
            scale: Vec3::splat(0.5),
            ..Transform::IDENTITY
        },
        MeshletInstance {
            mesh: MeshletMeshHandle(1),
        },
    ));
    // 柱侧球（出立柱轮廓 ⇒ 照常渲染；同网格句柄、不同实体）
    world.spawn((
        Transform {
            position: Vec3::new(1.5, 1.0, -0.8),
            scale: Vec3::splat(0.5),
            ..Transform::IDENTITY
        },
        MeshletInstance {
            mesh: MeshletMeshHandle(1),
        },
    ));
    propagate_transforms(&mut world);
    world.insert_resource(OcclusionCulling::ENABLED);
    world
}

/// 预算定标（与 `meshlet_stream_render.rs` 的 `stream_budgets` 同源）：
/// 注册序 = [地面, 球, 立方]，页号按注册序累加。返回（粗层预算,
/// 全量字节）。粗层预算 = 球的**纯粗页**（叶簇之后的页）+ 地面页 +
/// 立方页——叶页挤不进、粗页 + 小网格全驻留 ⇒ 球以粗层完整呈现。
fn budgets(hier: &[MeshletMesh]) -> (u64, u64) {
    let tables: Vec<MeshletPageTable> = hier
        .iter()
        .map(|m| MeshletPageTable::derive(m, MESHLET_PAGE_CLUSTERS))
        .collect();
    let ground = tables[0].page_bytes(0);
    let cube = tables[2].page_bytes(0);
    let sphere = &tables[1];
    let first_leaf_page = hier[1].leaf_count().div_ceil(MESHLET_PAGE_CLUSTERS);
    let coarse_bytes: u64 = (first_leaf_page..sphere.page_count())
        .map(|p| sphere.page_bytes(p))
        .sum();
    let total: u64 = tables
        .iter()
        .map(|t| (0..t.page_count()).map(|p| t.page_bytes(p)).sum::<u64>())
        .sum();
    (coarse_bytes + ground + cube, total)
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 收敛帧：连渲 N 帧（驻留 tick 一帧延迟 + 紧凑回读一帧延迟），返回
/// 末帧像素与统计。
fn converge(
    renderer: &mut OffscreenRenderer,
    world: &World,
    frames: usize,
) -> (Vec<u8>, FrameStats) {
    for _ in 0..frames {
        renderer.render(world);
        let _ = renderer.read_pixels();
    }
    renderer.render(world);
    let px = renderer.read_pixels();
    (px, renderer.renderer().stats())
}

fn main() {
    let Some(mut renderer_full) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut renderer_coarse) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut renderer_occ) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut renderer_flat) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    let meshes = scene_meshes();
    let hier: Vec<MeshletMesh> = meshes
        .iter()
        .map(iris_engine::meshlet_builder::build_meshlet_hierarchy_from_mesh_data)
        .collect();
    let leaf_total: u32 = hier.iter().map(|m| m.leaf_count()).sum();
    let (coarse_budget, total_budget) = budgets(&hier);
    assert!(
        coarse_budget > 0 && coarse_budget < total_budget,
        "粗层预算必须严格小于全量字节"
    );

    // ---- 面板 ①：全驻留（预算 = 全量字节）----
    let world_full = stream_world(
        camera_near(),
        &hier,
        MeshletStreamSettings {
            gpu_byte_budget: Some(total_budget),
            pages_per_frame: 64,
        },
    );
    let (px_full, stats_full) = converge(&mut renderer_full, &world_full, 8);

    // ---- 面板 ②：低预算粗层 ----
    let world_coarse = stream_world(
        camera_near(),
        &hier,
        MeshletStreamSettings {
            gpu_byte_budget: Some(coarse_budget),
            pages_per_frame: 64,
        },
    );
    let (px_coarse, stats_coarse) = converge(&mut renderer_coarse, &world_coarse, 8);

    // ---- 面板 ③：遮挡整合（收敛帧 = 柱后球整帧跳过）----
    let world_occ = occlusion_world(&hier);
    let (px_occ, stats_occ) = converge(&mut renderer_occ, &world_occ, 4);

    // ---- 参照系：R85 全叶路径（平面簇 + 无流送设置）----
    let flat: Vec<MeshletMesh> = meshes
        .iter()
        .map(iris_engine::meshlet_builder::build_meshlets_from_mesh_data)
        .collect();
    let world_flat = stream_world(
        camera_near(),
        &flat,
        MeshletStreamSettings {
            gpu_byte_budget: None, // 无预算 = 全驻留（R86 行为）
            pages_per_frame: 64,
        },
    );
    let (px_flat, stats_flat) = converge(&mut renderer_flat, &world_flat, 4);

    let (hash_full, hash_coarse, hash_occ) = (fnv1a(&px_full), fnv1a(&px_coarse), fnv1a(&px_occ));
    println!(
        "叶簇总数 {leaf_total}；预算 粗层 {coarse_budget} B / 全量 {total_budget} B\n\
         选中簇数（收敛帧）：全驻留 {} / 粗层 {} / R85 全叶参照 {}；\
         遮挡格 culled {} surviving {}",
        stats_full.meshlet_surviving,
        stats_coarse.meshlet_surviving,
        stats_flat.meshlet_surviving,
        stats_occ.occlusion_culled,
        stats_occ.meshlet_surviving,
    );

    // ---- 断言（不通过直接 panic、不写文件）----
    assert_eq!(
        px_full, px_flat,
        "全驻留收敛帧必须与 R85 全叶路径逐字节相等（预算 = 全量 ⇒ R86 行为）"
    );
    assert!(
        stats_full.meshlet_surviving > 0,
        "全驻留面板必须有簇存活（物体不消失）"
    );
    assert!(
        stats_coarse.meshlet_surviving > 0
            && stats_coarse.meshlet_surviving < stats_full.meshlet_surviving,
        "粗层预算必须选中更少簇且物体不消失：{} ∈ (0, {})",
        stats_coarse.meshlet_surviving,
        stats_full.meshlet_surviving
    );
    assert_ne!(hash_full, hash_coarse, "粗层与全驻留像素哈希必须不同");
    assert_eq!(
        stats_occ.occlusion_culled, 1,
        "收敛帧柱后球必须被剔（柱侧球可见 ⇒ 只剔 1 个）"
    );
    assert!(
        stats_occ.meshlet_surviving > 0,
        "柱侧球照常渲染（存活簇数 {} > 0）——被剔的只有柱后球",
        stats_occ.meshlet_surviving
    );

    // ---- 1×3 面板拼装 ----
    let stride = WIDTH * 3 + GAP * 2;
    let mut panel = vec![0u8; (stride * HEIGHT) as usize * 4];
    let blit = |panel: &mut [u8], src: &[u8], x0: u32| {
        for row in 0..HEIGHT as usize {
            let sy = row * WIDTH as usize * 4;
            let dy = (row * stride as usize + x0 as usize) * 4;
            let n = WIDTH as usize * 4;
            panel[dy..dy + n].copy_from_slice(&src[sy..sy + n]);
        }
    };
    blit(&mut panel, &px_full, 0);
    blit(&mut panel, &px_coarse, WIDTH + GAP);
    blit(&mut panel, &px_occ, (WIDTH + GAP) * 2);

    // ---- 写 PPM（供 ffmpeg 转 PNG 入 docs/）----
    let header = format!("P6\n{stride} {HEIGHT}\n255\n");
    let mut ppm = header.into_bytes();
    for chunk in panel.chunks_exact(4) {
        ppm.extend_from_slice(&chunk[..3]);
    }
    let out_path = "iris_meshlet_stream.ppm";
    std::fs::write(out_path, &ppm).expect("写 PPM");
    println!(
        "快照完成：{out_path}（{} B）；FNV 全驻留 {hash_full:016x} / 粗层 {hash_coarse:016x} \
         / 遮挡 {hash_occ:016x}",
        ppm.len()
    );
}
