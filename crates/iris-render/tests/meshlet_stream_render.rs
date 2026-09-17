//! 集成测试：虚拟几何体 V4 渲染（R87，分页流送 + 遮挡整合）。
//!
//! 四条主线：
//! 1. **全驻留等价（零成本回归锚点）**：流送缺席 / 预算 `None` ⇒ 与
//!    R85 全叶路径像素逐字节相等——「缺席 = 无预算 = 全驻留 = R86 行
//!    为逐位不变」的兼容硬约束；
//! 2. **预算流送**：字节预算只容粗层页 ⇒ 粗层渲染（像素哈希变化、
//!    选中簇数下降、物体不消失——回退链请求保证粗页流入，无死锁）；
//!    预算逐步放大 ⇒ 选中簇数单调趋近全驻留；
//! 3. **遮挡整合（R53 两阶段，实体级）**：遮挡物后的 meshlet 实体在
//!    收敛帧整帧跳过——像素与该实体缺席时逐字节相等（保守首帧除外）；
//!    可见实体恒不被剔、照常渲染（口径详见该测试文档）；
//! 4. **确定性**：同预算场景双会话逐字节一致。
//!
//! 环境无 GPU 适配器时优雅跳过（打印提示，测试仍算通过）。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::meshlet_stream::{MESHLET_PAGE_CLUSTERS, MeshletPageTable, MeshletStreamSettings};
use iris_render::prelude::*;

/// 屏幕尺寸（测试统一口径）。
const SIZE: u32 = 256;

/// 本套件串行化（meshlet v2 / lod / probe / post 同款纪律）：同文件多
/// 测试并行共用单适配器会挂死——文件内互斥锁把并发度压到 1。
static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 场景几何：互不接触、无共面重叠面。球体 ~6000 三角 ⇒ 数十叶簇 +
/// 多层父簇（流送粗化回退的主力样本）；地面 / 立方各为少量单簇（页
/// 粒度对照——粗层预算下必须全额驻留，否则整实例消失）。
fn scene_meshes() -> Vec<MeshData> {
    vec![
        MeshData::plane(10.0),
        MeshData::uv_sphere(0.7, 64, 48),
        MeshData::cube(),
    ]
}

/// 层级簇表示（R87 流送形态；构建器同时填充页表）。
fn hierarchy_meshes(meshes: &[MeshData]) -> Vec<MeshletMesh> {
    meshes
        .iter()
        .map(iris_engine::meshlet_builder::build_meshlet_hierarchy_from_mesh_data)
        .collect()
}

/// R85 平面簇表示（对照组：全叶路径）。
fn flat_meshes(meshes: &[MeshData]) -> Vec<MeshletMesh> {
    meshes
        .iter()
        .map(iris_engine::meshlet_builder::build_meshlets_from_mesh_data)
        .collect()
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

/// meshlet 场景世界：三件层级几何 + 各自 `MeshletInstance`（注册序 =
/// 地面 / 球 / 立方，页号全局基址按此累加）；`settings` 在场才挂流送
/// 设置。
fn stream_world(
    camera: Camera,
    meshes: &[MeshletMesh],
    settings: Option<MeshletStreamSettings>,
) -> World {
    let mut world = base_world(camera);
    let mut store = MeshletMeshStore::new();
    for mesh in meshes {
        store.add(mesh.clone());
    }
    world.insert_resource(store);
    if let Some(settings) = settings {
        world.insert_resource(settings);
    }
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

fn placements() -> [Vec3; 3] {
    [
        Vec3::ZERO,
        Vec3::new(0.0, 1.5, 0.0),
        Vec3::new(1.6, 0.5, -0.6),
    ]
}

/// 近景机位（球叶层全细节的「细化侧」；预算世界的粗化回退同样可见）。
fn camera_near() -> Camera {
    Camera {
        eye: Vec3::new(2.4, 1.6, 2.8),
        target: Vec3::new(0.0, 0.7, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 逐字节相等断言（带首个差异定位）。
fn assert_pixels_byte_equal(a: &[u8], b: &[u8], context: &str) {
    assert_eq!(a.len(), b.len(), "{context}：像素缓冲长度不同");
    if a == b {
        return;
    }
    let diff = a
        .iter()
        .zip(b.iter())
        .position(|(x, y)| x != y)
        .expect("长度相等且不相等 ⇒ 必有差异");
    let pixel = diff / 4;
    let (x, y) = (pixel % SIZE as usize, pixel / SIZE as usize);
    panic!(
        "{context}：像素逐字节不等（第 {diff} 字节 = 像素 ({x},{y}) 通道 {}，\
         左 {} vs 右 {}；全帧 FNV {:016x} vs {:016x}）",
        diff % 4,
        a[diff],
        b[diff],
        fnv1a(a),
        fnv1a(b)
    );
}

/// 预算定标（CPU 侧纯计算，与渲染器注册口径同源）：注册序 = [地面,
/// 球, 立方]，页号按注册序累加。返回：
/// - `coarse`：粗层预算 = 球的**纯粗页**（不含叶簇的页）+ 地面页 +
///   立方页——叶页挤不进、粗页 + 小网格全驻留 ⇒ 球以粗层完整呈现；
/// - `one_leaf`：粗层预算 + 球首叶页——叶组直接可绘 ⇒ 选中簇数上升。
///
/// 页大小 16 簇/页下球的纯粗页 = `ceil(叶数/16)..页数`（叶簇在前、
/// 父簇按层追加的容器布局 ⇒ 该区间不含任何叶簇）。
fn stream_budgets(hier: &[MeshletMesh]) -> (u64, u64, u64) {
    let tables: Vec<MeshletPageTable> = hier
        .iter()
        .map(|m| MeshletPageTable::derive(m, MESHLET_PAGE_CLUSTERS))
        .collect();
    assert_eq!(tables[0].page_count(), 1, "地面 = 单页（对照口径）");
    assert_eq!(tables[2].page_count(), 1, "立方 = 单页（对照口径）");
    let ground = tables[0].page_bytes(0);
    let sphere = &tables[1];
    let leaf_count = hier[1].leaf_count();
    let first_leaf_page = leaf_count.div_ceil(MESHLET_PAGE_CLUSTERS);
    // 纯粗页 = 叶层之后的页（容器布局：叶簇在前、父簇按层追加 ⇒ 该区
    // 间不含任何叶簇；混合页含叶簇 ⇒ 键同叶页，不在此列）。
    let coarse_bytes: u64 = (first_leaf_page..sphere.page_count())
        .map(|p| sphere.page_bytes(p))
        .sum();
    let coarse = coarse_bytes + ground + tables[2].page_bytes(0);
    let one_leaf = coarse + sphere.page_bytes(0);
    let total: u64 = tables
        .iter()
        .map(|t| (0..t.page_count()).map(|p| t.page_bytes(p)).sum::<u64>())
        .sum();
    (coarse, one_leaf, total)
}

/// 全驻留等价（本轮最强断言）：流送缺席 / 预算 None 与 R85 全叶路径
/// 像素逐字节相等，`meshlet_surviving` 同为叶簇总数——流送机制不激活
/// 时命令流与 R86 之后零漂移。
#[test]
fn stream_absent_and_unbudgeted_match_r85_byte_identical() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer_flat) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let Some(mut renderer_hier) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let Some(mut renderer_unbudgeted) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let meshes = scene_meshes();
    let hier = hierarchy_meshes(&meshes);
    let flat = flat_meshes(&meshes);
    let leaf_total: usize = flat.iter().map(|m| m.clusters.len()).sum();

    // R85 全叶路径：平面 mesh、无设置
    let world_flat = stream_world(camera_near(), &flat, None);
    renderer_flat.render(&world_flat);
    let px_flat = renderer_flat.read_pixels();
    let surviving_flat = renderer_flat.renderer().stats().meshlet_surviving as usize;

    // 层级 mesh + 流送缺席（R86 形态）
    let world_hier = stream_world(camera_near(), &hier, None);
    renderer_hier.render(&world_hier);
    let px_hier = renderer_hier.read_pixels();
    let surviving_hier = renderer_hier.renderer().stats().meshlet_surviving as usize;

    // 层级 mesh + 流送设置在场但预算 None（= 无预算 = 全驻留）
    let world_unbudgeted = stream_world(
        camera_near(),
        &hier,
        Some(MeshletStreamSettings {
            gpu_byte_budget: None,
            pages_per_frame: 4,
        }),
    );
    renderer_unbudgeted.render(&world_unbudgeted);
    let px_unbudgeted = renderer_unbudgeted.read_pixels();
    let surviving_unbudgeted = renderer_unbudgeted.renderer().stats().meshlet_surviving as usize;

    assert_eq!(surviving_flat, leaf_total, "R85 全叶 surviving = 叶簇总数");
    assert_eq!(
        surviving_hier, leaf_total,
        "流送缺席帧 surviving = 叶簇总数（全叶恒等表）"
    );
    assert_eq!(
        surviving_unbudgeted, leaf_total,
        "预算 None 帧 surviving = 叶簇总数"
    );
    assert_pixels_byte_equal(&px_hier, &px_flat, "流送缺席 vs R85 全叶");
    assert_pixels_byte_equal(&px_unbudgeted, &px_flat, "预算 None vs R85 全叶");
}

/// 预算流送：小预算 ⇒ 粗层渲染（物体完整、选中簇数下降）；预算放大
/// ⇒ 选中簇数单调趋近全驻留。帧间 `read_pixels` 模拟帧节拍（遮挡读回
/// 同款纪律）——驻留 tick 一帧延迟下 8 帧足以收敛（页数 ≤ 每帧上限）。
#[test]
fn stream_budget_coarse_renders_whole_object_monotone_toward_full() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer_full) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let Some(mut renderer_coarse) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let Some(mut renderer_mid) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let Some(mut renderer_flat) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let meshes = scene_meshes();
    let hier = hierarchy_meshes(&meshes);
    let flat = flat_meshes(&meshes);
    let leaf_total: usize = hier.iter().map(|m| m.leaf_count() as usize).sum();
    let (coarse, one_leaf, total) = stream_budgets(&hier);
    assert!(coarse > 0 && coarse < total, "粗层预算必须严格小于全量字节");
    let settings_for = |budget: u64| MeshletStreamSettings {
        gpu_byte_budget: Some(budget),
        pages_per_frame: 32, // ≥ 页数：一次 tick 全部准入，收敛只受一帧请求时延
    };

    // 全驻留（预算 = 全量字节）：收敛后像素与 R85 全叶路径逐字节相等
    //（GPU 剔除只删不可见几何 ⇒ 收敛帧与恒等帧像素恒等，R85 同款论证）
    let world_flat = stream_world(camera_near(), &flat, None);
    renderer_flat.render(&world_flat);
    let px_flat = renderer_flat.read_pixels();

    let world_full = stream_world(camera_near(), &hier, Some(settings_for(total)));
    for _ in 0..8 {
        renderer_full.render(&world_full);
        let _ = renderer_full.read_pixels();
    }
    let px_full = renderer_full.read_pixels();
    let surviving_full = renderer_full.renderer().stats().meshlet_surviving as usize;

    // 粗层预算：球叶页挤不进 ⇒ 回退到驻留粗页（物体完整、簇数下降）
    let world_coarse = stream_world(camera_near(), &hier, Some(settings_for(coarse)));
    for _ in 0..8 {
        renderer_coarse.render(&world_coarse);
        let _ = renderer_coarse.read_pixels();
    }
    let px_coarse = renderer_coarse.read_pixels();
    let surviving_coarse = renderer_coarse.renderer().stats().meshlet_surviving as usize;

    // 粗层 + 球首叶页：更多叶组直接可绘 ⇒ 选中簇数上升
    let world_mid = stream_world(camera_near(), &hier, Some(settings_for(one_leaf)));
    for _ in 0..8 {
        renderer_mid.render(&world_mid);
        let _ = renderer_mid.read_pixels();
    }
    let px_mid = renderer_mid.read_pixels();
    let _ = px_mid; // 中档像素流不参与哈希断言（选中簇数单调已足够判别）
    let surviving_mid = renderer_mid.renderer().stats().meshlet_surviving as usize;

    println!(
        "流送预算扫描：叶总数 {leaf_total}；选中簇数 粗层预算 {surviving_coarse} / \
         +首叶页 {surviving_mid} / 全驻留 {surviving_full}（预算 {coarse} / {one_leaf} / {total} B）"
    );
    // 全驻留收敛锚点：预算 = 全量字节的收敛帧与 R85 全叶路径逐字节相等
    assert_pixels_byte_equal(&px_full, &px_flat, "全驻留预算收敛帧 vs R85 全叶");
    // 物体不消失（无洞底线）：粗层帧 ≠ 空场景（同机位、零 meshlet 实体）
    let empty = base_world(camera_near());
    renderer_coarse.render(&empty);
    let px_empty = renderer_coarse.read_pixels();
    assert_ne!(
        fnv1a(&px_coarse),
        fnv1a(&px_empty),
        "粗层预算画面必须非空（回退链请求保证粗页流入，物体不得消失）"
    );
    // 粗层生效：选中簇数下降（< 叶总数）且低于全驻留；像素流确实改变
    assert!(
        surviving_coarse > 0 && surviving_coarse < leaf_total,
        "粗层预算必须以更少簇数呈现（{surviving_coarse} < {leaf_total}）"
    );
    assert_ne!(
        fnv1a(&px_coarse),
        fnv1a(&px_full),
        "粗层预算与全驻留的像素哈希必须不同（粗化生效）"
    );
    // 预算放大 ⇒ 选中簇数单调趋近全驻留（surviving 滞后一帧读回，
    // 8 帧后状态已收敛，口径稳定）
    assert!(
        surviving_coarse < surviving_mid && surviving_mid <= surviving_full,
        "预算单调 ⇒ 选中簇数单调：{surviving_coarse} < {surviving_mid} ≤ {surviving_full}"
    );
}

/// 遮挡整合（R53 两阶段，实体级）：遮挡物后的 meshlet 实体在收敛帧
/// 整帧跳过（像素与该实体缺席时逐字节相等，保守首帧除外）；可见的
/// meshlet 实体恒不被剔（照常渲染）。
///
/// 断言口径（与 `occlusion_render.rs` 的 R53 范式同构）：
///
/// - **像素锚点 = 同场景「实体缺席」世界**，且立柱几何必须逐位相同
///   ⇒ [`make_scene`] 恒定调 `propagate_transforms`（只在场分支调会让
///   缺席基线的立柱没有 `GlobalTransform`，两个世界根本不是同一几何，
///   「逐字节相等」的断言恒假——本测试首稿即踩此坑）；
/// - **前提校验（保守性的根据）**：无遮挡配置的「实体在场」帧与「实体
///   缺席」帧**逐字节相等**——球屏面严格落在立柱轮廓内 ⇒ 零像素贡献。
///   这是「剔除不改变画面」成立的前提，必须先立住；
/// - **照常渲染的判别式 = 统计而非像素**：无遮挡配置帧
///   `meshlet_surviving` = 球叶簇数（实体照常进实例表并提交 cull /
///   draw）；收敛帧降为 0（整帧跳过、不进 cull dispatch 输入）。像素
///   判别不了「提交与否」（零贡献），统计口径才判别得了；
/// - **可见实体不被剔**（第二场景）：球移到柱侧可见位 ⇒ 收敛帧
///   `occlusion_culled == 0`、像素与「同几何 + 无遮挡」逐字节相等、
///   且与缺席基线不同（球确实可见）——AABB 未被完全遮挡则照常渲染。
///
/// `GPU_SERIAL` 互斥：重负载 GPU 套件文件内串行化（同文件多测试并行
/// 共用单适配器会挂死，见 AGENTS.md 测试规约）。
#[test]
fn stream_occlusion_culls_hidden_meshlet_entity_to_absence_baseline() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer_absent) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let Some(mut renderer_hidden_off) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let Some(mut renderer_hidden_on) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let Some(mut renderer_vis_off) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let Some(mut renderer_vis_on) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let meshes = scene_meshes();
    let hier = hierarchy_meshes(&meshes);
    let sphere_leaf = hier[1].leaf_count();

    // ---- 基线：实体缺席（像素锚点；立柱几何与其余世界逐位相同）----
    let world_absent = make_scene(&hier, None, false);
    renderer_absent.render(&world_absent);
    let px_absent = renderer_absent.read_pixels();

    // ---- 柱后（完全被挡）：无遮挡配置 ----
    let world_hidden_off = make_scene(&hier, Some(HIDDEN_SPHERE), false);
    renderer_hidden_off.render(&world_hidden_off);
    let px_hidden_off = renderer_hidden_off.read_pixels();
    let stats_hidden_off = renderer_hidden_off.renderer().stats();
    assert_eq!(
        stats_hidden_off.occlusion_culled, 0,
        "遮挡资源缺席时 occlusion_culled 恒 0"
    );
    assert_eq!(
        stats_hidden_off.meshlet_surviving, sphere_leaf,
        "无遮挡配置时 meshlet 实体照常提交（选中簇数 = 球叶簇数）"
    );
    assert_pixels_byte_equal(
        &px_hidden_off,
        &px_absent,
        "前提校验：柱后球零像素贡献（在场帧与缺席帧必须逐字节相等）",
    );

    // ---- 柱后（完全被挡）：开启遮挡——帧 1 保守全可见，帧 2 起裁决 ----
    let world_hidden_on = make_scene(&hier, Some(HIDDEN_SPHERE), true);
    renderer_hidden_on.render(&world_hidden_on);
    let px_f1 = renderer_hidden_on.read_pixels();
    let stats_f1 = renderer_hidden_on.renderer().stats();
    assert_eq!(
        stats_f1.occlusion_culled, 0,
        "读回延迟一帧：第一帧无结果可用，必须全部可见（保守方向）"
    );
    assert_eq!(
        stats_f1.meshlet_surviving, sphere_leaf,
        "帧 1 实体照常提交（与无遮挡帧同口径）"
    );
    assert_pixels_byte_equal(&px_f1, &px_hidden_off, "帧 1 全可见 vs 无遮挡帧");

    renderer_hidden_on.render(&world_hidden_on);
    let px_f2 = renderer_hidden_on.read_pixels();
    let stats_f2 = renderer_hidden_on.renderer().stats();
    assert_eq!(
        stats_f2.occlusion_culled, 1,
        "柱后 meshlet 实体的世界 AABB 完全落在立柱轮廓内 ⇒ 查询计 0、整帧跳过"
    );
    assert_eq!(
        stats_f2.meshlet_surviving, 0,
        "被剔实体退出实例表（不进 cull dispatch 输入）⇒ 选中簇数归零"
    );
    assert_pixels_byte_equal(
        &px_f2,
        &px_absent,
        "保守性：被剔 meshlet 实体对本帧像素零贡献 ⇒ 画面与实体缺席逐字节相等",
    );

    // 稳态：帧 3 与帧 2 逐位一致（确定性）
    renderer_hidden_on.render(&world_hidden_on);
    let px_f3 = renderer_hidden_on.read_pixels();
    let stats_f3 = renderer_hidden_on.renderer().stats();
    assert_eq!(stats_f3.occlusion_culled, 1, "静止场景稳态持续剔除");
    assert_eq!(stats_f3.meshlet_surviving, 0, "稳态持续整帧跳过");
    assert_pixels_byte_equal(&px_f3, &px_f2, "稳态逐帧像素逐位一致");

    // ---- 柱侧（可见）：开启遮挡 ⇒ 恒不被剔、照常渲染 ----
    // 注意 `meshlet_surviving` 的两档口径：**首帧**无紧凑回读 ⇒ 恒等兜底
    // = 叶簇数（精确可断言）；**收敛帧** = GPU 逐簇剔除后的存活簇数
    // （背向 / 视锥外簇被 compute 剔除 ⇒ 可能 < 叶簇数，只断言 > 0）。
    let world_vis_off = make_scene(&hier, Some(VISIBLE_SPHERE), false);
    for _ in 0..3 {
        renderer_vis_off.render(&world_vis_off);
        let _ = renderer_vis_off.read_pixels();
    }
    let px_vis_off = renderer_vis_off.read_pixels();
    assert_ne!(
        fnv1a(&px_vis_off),
        fnv1a(&px_absent),
        "柱侧球必须可见（像素与缺席基线不同）"
    );

    let world_vis_on = make_scene(&hier, Some(VISIBLE_SPHERE), true);
    renderer_vis_on.render(&world_vis_on);
    let px_vis_f1 = renderer_vis_on.read_pixels();
    let stats_vis_f1 = renderer_vis_on.renderer().stats();
    assert_eq!(
        stats_vis_f1.occlusion_culled, 0,
        "读回延迟一帧：第一帧无结果可用，可见实体照常提交"
    );
    assert_eq!(
        stats_vis_f1.meshlet_surviving, sphere_leaf,
        "帧 1 恒等兜底 = 球叶簇数（实体照常进实例表）"
    );
    assert_pixels_byte_equal(&px_vis_f1, &px_vis_off, "帧 1 vs 无遮挡帧逐位一致");

    for _ in 0..2 {
        renderer_vis_on.render(&world_vis_on);
        let _ = renderer_vis_on.read_pixels();
    }
    let px_vis_on = renderer_vis_on.read_pixels();
    let stats_vis = renderer_vis_on.renderer().stats();
    assert_eq!(
        stats_vis.occlusion_culled, 0,
        "可见 meshlet 实体的 AABB 未被完全遮挡 ⇒ 不得剔除"
    );
    assert!(
        stats_vis.meshlet_surviving > 0,
        "可见实体照常提交绘制（收敛帧存活簇数 {} > 0）",
        stats_vis.meshlet_surviving
    );
    assert_pixels_byte_equal(
        &px_vis_on,
        &px_vis_off,
        "遮挡开启不改变可见实体的画面（保守性：只漏剔不误剔）",
    );
}

/// 柱后球位置（完全被挡：立柱轮廓内 + 更远深度）。
const HIDDEN_SPHERE: Vec3 = Vec3::new(0.0, 1.0, -3.0);
/// 柱侧球位置（可见：x 出立柱轮廓、与立柱屏幕矩形无交叠）。
const VISIBLE_SPHERE: Vec3 = Vec3::new(1.5, 1.0, -0.8);

/// 遮挡测试场景：立柱（常规网格路径——主 pass 写深度，充当遮挡物）+
/// 可选 meshlet 球体（`sphere_at` = 球心位置，`None` = 实体缺席基线）。
/// 球 `scale 0.5` ⇒ 半径 0.35：柱后位（距离 ≈7.5、角半径 ≈0.047 rad ≪
/// 立柱的 0.11 rad）的世界 AABB 完全落在立柱轮廓内；柱侧位出轮廓。
///
/// **恒定调用 `propagate_transforms`**——两个世界必须只有「球在场 /
/// 缺席（+ 遮挡开关）」一个差异，否则像素锚点无效（见测试文档）。
fn make_scene(hier: &[MeshletMesh], sphere_at: Option<Vec3>, occlusion: bool) -> World {
    let mut world = base_world(Camera {
        eye: Vec3::new(0.0, 1.2, 4.5),
        target: Vec3::new(0.0, 1.0, 0.0),
        ..Camera::default()
    });
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.25, 0.25, 0.3, 1.0));
    // 立柱（画面中央）
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 1.0, 0.0),
            scale: Vec3::new(1.0, 2.0, 1.0),
            ..Transform::IDENTITY
        },
        cube,
        mat,
    ));
    if let Some(position) = sphere_at {
        let mut store = MeshletMeshStore::new();
        for mesh in hier {
            store.add(mesh.clone());
        }
        world.insert_resource(store);
        world.spawn((
            Transform {
                position,
                scale: Vec3::splat(0.5),
                ..Transform::IDENTITY
            },
            MeshletInstance {
                mesh: MeshletMeshHandle(1),
            },
        ));
    }
    propagate_transforms(&mut world);
    if occlusion {
        world.insert_resource(OcclusionCulling::ENABLED);
    }
    world
}

/// 运行期新增资产触发的 arena 重建**不得丢掉已驻留页**（R87 重建补传）：
/// 全驻留预算下先收敛，再向 store 追加一个**更大**的网格（无实例，只为把
/// 顶点词容量推过当前容量 ⇒ 触发顶点 / 索引缓冲重建），继续渲染后像素
/// 必须与重建前**逐字节相等**（几何未变）。
///
/// 无补传时的症状：新缓冲零初始化，而驻留状态机仍标记旧页驻留——
/// `ResidencyManager::tick` 只对**未驻留**页组队（`resident.contains`
/// 短路），「后续请求自然补传」不成立 ⇒ 顶点 / 索引全零、零面积三角：
/// `meshlet_surviving` 照报存活、画面却空了。本测试即锁这条不变量。
#[test]
fn stream_survives_arena_rebuild_on_late_registration() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let meshes = scene_meshes();
    let hier = hierarchy_meshes(&meshes);
    let (_, _, total) = stream_budgets(&hier);

    let world = stream_world(
        camera_near(),
        &hier,
        Some(MeshletStreamSettings {
            gpu_byte_budget: Some(total), // 全驻留（收敛后无驱逐）
            pages_per_frame: 64,
        }),
    );
    for _ in 0..6 {
        renderer.render(&world);
        let _ = renderer.read_pixels();
    }
    let px_before = renderer.read_pixels();
    let surviving_before = renderer.renderer().stats().meshlet_surviving;
    assert!(surviving_before > 0, "全驻留收敛帧必须有簇存活");

    // 追加更大网格（顶点数 > 现有全部合计 ⇒ 容量翻倍后仍不够 ⇒ 重建）；
    // 不挂 `MeshletInstance`，画面语义零变化，只有 arena 容量变化。
    let big = iris_engine::meshlet_builder::build_meshlet_hierarchy_from_mesh_data(
        &MeshData::uv_sphere(1.0, 96, 72),
    );
    {
        let mut store = world.resource_mut::<MeshletMeshStore>();
        let before = store.len();
        store.add(big);
        assert_eq!(store.len(), before + 1, "新句柄已登记");
    }

    for _ in 0..3 {
        renderer.render(&world);
        let _ = renderer.read_pixels();
    }
    let px_after = renderer.read_pixels();
    let surviving_after = renderer.renderer().stats().meshlet_surviving;

    assert!(
        surviving_after > 0,
        "重建后必须有簇存活（无补传时计数照报、顶点 / 索引全零）"
    );
    assert_pixels_byte_equal(
        &px_after,
        &px_before,
        "运行期注册触发的 arena 重建后，已驻留页数据必须原样（重建补传）",
    );
}

/// 确定性：同预算场景双会话逐字节一致（驻留状态机 + 门控选择的全序
/// 迭代保证裁决与像素逐位可复现）。
#[test]
fn stream_deterministic_across_sessions() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer_a) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let Some(mut renderer_b) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    let meshes = scene_meshes();
    let hier = hierarchy_meshes(&meshes);
    let (coarse, _, total) = stream_budgets(&hier);
    let settings = MeshletStreamSettings {
        gpu_byte_budget: Some(coarse),
        pages_per_frame: 32,
    };
    let world = stream_world(camera_near(), &hier, Some(settings));

    for _ in 0..6 {
        renderer_a.render(&world);
        let _ = renderer_a.read_pixels();
        renderer_b.render(&world);
        let _ = renderer_b.read_pixels();
    }
    let px_a = renderer_a.read_pixels();
    let px_b = renderer_b.read_pixels();
    assert_pixels_byte_equal(&px_a, &px_b, "同预算双会话逐字节一致");
    let _ = total;
}
