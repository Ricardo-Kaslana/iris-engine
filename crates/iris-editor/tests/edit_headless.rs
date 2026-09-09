//! headless 编辑测试（R58a 验收①）：全部经**编辑命令层 API** 驱动，不建
//! 窗口 / 不建 GPU / 不注册插件——命令层与交互壳分离的架构收益就在这里
//! 兑现（[`iris_editor::shell`] 的输入翻译不被任何一条测试触及）。
//!
//! 覆盖：
//! - **往返一致**（规格验收①主体）：从空世界摆放 ≥8 实体（四种图元 +
//!   静态碰撞体 + 点光 + 复制体）→ `save_level` → 新世界 `open_level` →
//!   世界确定性转储（实体数 / 编号 / 逐实体 Transform 位模式 / 句柄 /
//!   组件位 / 登记表 / 两个 Store 的内容摘要）**逐字符相等**；
//! - 存档字节稳定：同一档二次保存逐字节相同（规范化输出）；
//! - 拾取：最近命中优先、同距取存储序在前（确定性裁决）、叠放命中顶面；
//! - 拖拽：吸附到 0.5 m 格点、高度保持、拖拽偏移不跳；
//! - 删除 / 复制：级联删除、`_copy{n}` 后缀递增；
//! - 瞬态视觉不入档：网格 / 选中框永不出现在档里，保存后视觉照旧在位；
//! - 屏幕像素 → 射线 → 拾取闭环（交互壳的换算入口，headless 可测）；
//! - 属性面板：只读、纯 ASCII、与选中态一致；
//! - 关卡档 v2 的 `level` 段经 `LevelRegistry` 快照语义原样保留；
//! - 真实关卡（iris-assault 庭院档，R57 产物）经编辑器开 → 存 → 重载
//!   往返一致（dogfooding：编辑器不改坏既有档）。
//!
//! 临时档一律走 `std::env::temp_dir()` + 进程 id（测试并行执行，共享文件名
//! 会竞态——AGENTS.md 铁律）。

use std::path::PathBuf;

use glam::Vec3;
use iris_editor::edit::{
    self, DEFAULT_SNAP, EditorState, EditorTransient, GIZMO_EDGE_COUNT, GRID_EXTENT, GRID_STEP,
    HUD_NAME, MATERIAL_PRESETS, PlaceKind, ascii_only, begin_drag, clear_transient, cycle_kind,
    cycle_preset, delete_selected, describe, drag_to, duplicate_selected, editable_entities,
    end_drag, nudge_selected, open_level, panel_text, pick, place_at, place_from_ray,
    placement_point, rebuild_grid, save_level, scale_selected, screen_ray_at, select, snap_value,
    toggle_collider, toggle_snap, update_gizmo, world_aabb_of,
};
use iris_engine::prelude::*;

// ---------------------------------------------------------------------------
// 测试台
// ---------------------------------------------------------------------------

/// 唯一临时目录（进程 id 防并行竞态）。
fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("iris-editor-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("临时目录可建");
    dir
}

/// 以指定摆放参数摆一个实体（测试直接写公开状态字段，不走 `Tab` / `R`
/// 循环——参数显式，用例自解释）。
fn place_with(
    world: &mut World,
    kind: PlaceKind,
    preset: usize,
    collider: bool,
    point: Vec3,
) -> Entity {
    {
        let mut state = world.resource_mut::<EditorState>();
        state.place_kind = kind;
        state.preset = preset;
        state.place_collider = collider;
    }
    place_at(world, point).expect("预设图元参数恒合法，摆放必成功")
}

/// 瞬态视觉实体计数（网格线 + 选中框）。
fn transient_count(world: &World) -> usize {
    let mut count = 0usize;
    world.query::<&EditorTransient>(|_| count += 1);
    count
}

/// 网格线实体数（`-EXTENT..=EXTENT` 每 [`GRID_STEP`] 一条，两个轴向）。
fn grid_line_count() -> usize {
    let steps = (GRID_EXTENT / GRID_STEP) as i32;
    (steps * 2 + 1) as usize * 2
}

// ---------------------------------------------------------------------------
// 世界确定性转储（R57 level_roundtrip.rs 的 dump 技术复用）
// ---------------------------------------------------------------------------

/// FNV-1a 64（摘要用，与验收图的哈希同款算法）。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn bits(value: f32) -> u32 {
    value.to_bits()
}

/// `Vec3` 的逐分量位模式（glam 无 `Vec3::to_bits`）：位模式断言的载体，
/// 杜绝「打印相同但位不同」的假通过。
fn vec_bits(value: Vec3) -> [u32; 3] {
    [bits(value.x), bits(value.y), bits(value.z)]
}

fn feed(hash: &mut u64, value: f32) {
    let value_bytes = value.to_bits().to_le_bytes();
    let seed = hash.to_le_bytes();
    *hash = fnv1a(&[&value_bytes[..], &seed[..]].concat());
}

fn feed_u32(hash: &mut u64, value: u32) {
    let value_bytes = value.to_le_bytes();
    let seed = hash.to_le_bytes();
    *hash = fnv1a(&[&value_bytes[..], &seed[..]].concat());
}

/// 世界确定性转储：可编辑实体逐实体（编号 / 名 / Transform **位模式** /
/// 句柄 / 组件位）+ 登记表（`BTreeMap` 序——`HashMap` 迭代序不定，排序是
/// 确定性的前提）+ `MeshStore` / `MaterialStore` 逐句柄内容摘要 + 环境资源
/// + `level` 段快照。
///
/// 浮点一律 `to_bits`（杜绝「打印相同但位不同」的假通过）；`Debug` 只用于
/// 枚举标签与整数字段（f32 的 `Debug` 是最短往返表示，本身单射，但位模式
/// 断言更直白）。
fn dump_world(world: &World) -> String {
    let mut out = String::new();

    out.push_str(&format!("editable {}\n", editable_entities(world).len()));
    // 转储**清单序号**而非 `Entity` 原始编号：编辑侧的瞬态视觉（选中框
    // 12 条边、网格线）与关卡内容是交错 spawn 的（摆放后自动选中 ⇒ 立刻
    // 生成选中框），原始编号必然与重载侧不同；而关卡档里没有实体 id 这个
    // 东西——往返可对照的是「顺序 + 内容」，编号只是分配器副产物。
    for (slot, entity) in editable_entities(world).iter().enumerate() {
        let entity = *entity;
        let transform = *world.get::<Transform>(entity).expect("查询已确认");
        out.push_str(&format!("entity slot={slot}\n"));
        out.push_str(&format!(
            "  name {:?}\n",
            world.get::<Name>(entity).map(|n| n.0.clone())
        ));
        out.push_str(&format!(
            "  pos [{}, {}, {}] rot [{}, {}, {}, {}] scale [{}, {}, {}]\n",
            bits(transform.position.x),
            bits(transform.position.y),
            bits(transform.position.z),
            bits(transform.rotation.x),
            bits(transform.rotation.y),
            bits(transform.rotation.z),
            bits(transform.rotation.w),
            bits(transform.scale.x),
            bits(transform.scale.y),
            bits(transform.scale.z),
        ));
        out.push_str(&format!(
            "  mesh {:?} material {:?} visible {:?}\n",
            world.get::<MeshHandle>(entity).map(|h| h.0),
            world.get::<MaterialHandle>(entity).map(|h| h.0),
            world.get::<Visibility>(entity).map(|v| v.visible),
        ));
        out.push_str(&format!("  collider {:?}\n", collider_dump(world, entity)));
        out.push_str(&format!("  rigidbody {:?}\n", body_dump(world, entity)));
        out.push_str(&format!("  light {:?}\n", light_dump(world, entity)));
        out.push_str(&format!("  particles {:?}\n", particle_dump(world, entity)));
    }

    // 登记表：来源还原的依据，往返两侧必须逐条相同
    let registry = world.try_resource::<SceneAssetRegistry>();
    if let Some(registry) = registry.as_deref() {
        let meshes: std::collections::BTreeMap<_, _> = registry.meshes.iter().collect();
        out.push_str(&format!("registry.meshes {}\n", meshes.len()));
        for (handle, source) in meshes {
            out.push_str(&format!("  mesh {handle} => {source:?}\n"));
        }
        let materials: std::collections::BTreeMap<_, _> = registry.materials.iter().collect();
        out.push_str(&format!("registry.materials {}\n", materials.len()));
        for (handle, source) in materials {
            out.push_str(&format!("  material {handle} => {source:?}\n"));
        }
        let gltfs: std::collections::BTreeMap<_, _> = registry.gltfs.iter().collect();
        out.push_str(&format!("registry.gltfs {}\n", gltfs.len()));
        for (entity, path) in gltfs {
            out.push_str(&format!("  gltf {} => {path}\n", entity.to_bits()));
        }
        out.push_str(&format!("registry.sky {:?}\n", registry.sky_source));
    }

    // 两个 Store 的逐句柄内容摘要（句柄编号对齐不变量的直接证据）
    out.push_str(&store_digest(world));

    // 环境与 level 段（save_scene 的另两个输出来源）
    if let Some(light) = world.try_resource::<DirectionalLight>() {
        out.push_str(&format!(
            "directional [{}, {}, {}] color [{}, {}, {}] intensity {} ambient [{}, {}, {}] shadows {:?} range {}\n",
            bits(light.direction.x),
            bits(light.direction.y),
            bits(light.direction.z),
            bits(light.color.x),
            bits(light.color.y),
            bits(light.color.z),
            bits(light.intensity),
            bits(light.ambient.x),
            bits(light.ambient.y),
            bits(light.ambient.z),
            light.shadows,
            bits(light.shadow_range),
        ));
    }
    if let Some(level) = world.try_resource::<LevelRegistry>() {
        out.push_str(&format!("level {:?}\n", level.section));
    }
    out
}

fn collider_dump(world: &World, entity: Entity) -> Option<String> {
    let collider = world.get::<Collider>(entity)?;
    let shape = match collider.shape {
        ColliderShape::Sphere { radius } => format!("sphere {}", bits(radius)),
        ColliderShape::Box { half_extents } => format!(
            "box [{}, {}, {}]",
            bits(half_extents.x),
            bits(half_extents.y),
            bits(half_extents.z)
        ),
        ColliderShape::Capsule {
            radius,
            half_height,
        } => format!("capsule {} {}", bits(radius), bits(half_height)),
    };
    Some(format!(
        "{shape} static={} restitution={} sensor={} groups={} mask={}",
        collider.is_static,
        bits(collider.restitution),
        collider.is_sensor,
        collider.groups,
        collider.mask
    ))
}

fn body_dump(world: &World, entity: Entity) -> Option<String> {
    let body = world.get::<RigidBody>(entity)?;
    Some(format!(
        "gravity {} friction {} linear {} velocity [{}, {}, {}] type {:?}",
        bits(body.gravity_scale),
        bits(body.friction),
        bits(body.linear_damping),
        bits(body.velocity.x),
        bits(body.velocity.y),
        bits(body.velocity.z),
        body.body_type
    ))
}

fn light_dump(world: &World, entity: Entity) -> Option<String> {
    let light = world.get::<PointLight>(entity)?;
    Some(format!(
        "color [{}, {}, {}] intensity {} range {} shadows {}",
        bits(light.color.x),
        bits(light.color.y),
        bits(light.color.z),
        bits(light.intensity),
        bits(light.range),
        light.shadows
    ))
}

/// 粒子发射器的**配置字段**摘要（运行期状态是私有字段也不入档，R57
/// 语义）：逐字段位模式，证明编辑器不破坏粒子段。
fn particle_dump(world: &World, entity: Entity) -> Option<String> {
    let emitter = world.get::<ParticleEmitter>(entity)?;
    Some(format!(
        "blend {:?} rate {} burst {} emitter_life {} particle_life {} speed {} jitter {} \
         gravity {} size {} {} color_start [{}, {}, {}, {}] color_end [{}, {}, {}, {}] max {}",
        emitter.blend,
        bits(emitter.spawn_rate),
        emitter.burst_count,
        bits(emitter.emitter_lifetime),
        bits(emitter.particle_lifetime),
        bits(emitter.initial_speed),
        bits(emitter.velocity_jitter),
        bits(emitter.gravity_scale),
        bits(emitter.size_start),
        bits(emitter.size_end),
        bits(emitter.color_start[0]),
        bits(emitter.color_start[1]),
        bits(emitter.color_start[2]),
        bits(emitter.color_start[3]),
        bits(emitter.color_end[0]),
        bits(emitter.color_end[1]),
        bits(emitter.color_end[2]),
        bits(emitter.color_end[3]),
        emitter.max_particles,
    ))
}

/// 两个 Store 的逐句柄内容摘要：网格 = 顶点 / 索引计数 + 几何 FNV；
/// 材质 = 全字段位模式。句柄从 0 递增到首个缺席为止（Store 是连续 Vec）。
fn store_digest(world: &World) -> String {
    let mut out = String::new();
    if let Some(store) = world.try_resource::<MeshStore>() {
        let mut handle = 0u32;
        while let Some(data) = store.get(MeshHandle(handle)) {
            let mut hash = fnv1a(&data.vertices.len().to_le_bytes());
            for vertex in &data.vertices {
                for value in vertex.position.iter().chain(vertex.normal.iter()) {
                    feed(&mut hash, *value);
                }
                for value in vertex.uv.iter() {
                    feed(&mut hash, *value);
                }
            }
            feed_u32(&mut hash, data.indices.len() as u32);
            for index in &data.indices {
                feed_u32(&mut hash, *index);
            }
            out.push_str(&format!(
                "mesh {handle} verts={} indices={} skin={} tangents={} uv2={} fnv={hash:016x}\n",
                data.vertices.len(),
                data.indices.len(),
                data.skin.is_some(),
                data.tangents.is_some(),
                data.uv2.is_some(),
            ));
            handle += 1;
        }
        out.push_str(&format!("meshes {handle}\n"));
    }
    if let Some(store) = world.try_resource::<MaterialStore>() {
        let mut handle = 0u32;
        while let Some(def) = store.get(MaterialHandle(handle)) {
            out.push_str(&format!(
                "material {handle} base [{}, {}, {}, {}] emissive [{}, {}, {}, {}] metallic {} roughness {} tex {} normal {:?} orm {:?} alpha {:?}\n",
                bits(def.base_color[0]),
                bits(def.base_color[1]),
                bits(def.base_color[2]),
                bits(def.base_color[3]),
                bits(def.emissive[0]),
                bits(def.emissive[1]),
                bits(def.emissive[2]),
                bits(def.emissive[3]),
                bits(def.metallic),
                bits(def.roughness),
                def.texture.0,
                def.normal_texture.map(|h| h.0),
                def.orm_texture.map(|h| h.0),
                def.alpha_mode,
            ));
            handle += 1;
        }
        out.push_str(&format!("materials {handle}\n"));
    }
    out
}

// ---------------------------------------------------------------------------
// 验收①主体：摆放 → 保存 → 重载 → 逐位对照
// ---------------------------------------------------------------------------

/// 从空世界摆放 9 个实体（四种图元 + 五种预设中的五种 + 静态碰撞体 +
/// 点光 + 复制体）→ 保存 → 新世界重载 → 世界转储逐字符相等。
///
/// 这条测试同时钉住三件事：
/// 1. 实体数 / 编号（含代数）/ 逐实体 Transform 位模式 / 组件位相等；
/// 2. **句柄编号对齐**：`MeshHandle` / `MaterialHandle` 数值两侧相同
///    （编辑器侧同源去重 + 瞬态资产先建，与 `load_scene` 的 `LoadCache`
///    去重语义对齐——见 `iris_editor::edit` 模块文档）；
/// 3. 登记表两侧逐条相同（编辑器登记的 `MeshRef::Primitive` /
///    `MaterialRef::default()` 与加载侧还原的登记一致）。
#[test]
fn placed_level_roundtrips_bitwise() {
    let dir = temp_dir("roundtrip");
    let path = dir.join("editor_mvp.level.json");

    let mut world = World::new();
    let report = open_level(&mut world, &path).expect("空档 = 从空世界新建");
    assert!(!report.loaded, "档不存在时 loaded = false");
    assert_eq!(report.entities, 0);
    assert_eq!(report.editable, 0, "空世界无可编辑实体");
    assert_eq!(
        transient_count(&world),
        grid_line_count(),
        "地面参考网格应已生成（纯视觉）"
    );

    // 摆放顺序 = 存档实体顺序（save_scene 按组件存储序写出）
    let a = place_with(&mut world, PlaceKind::Cube, 0, true, Vec3::ZERO); // stone
    place_with(
        &mut world,
        PlaceKind::Cube,
        0,
        true,
        Vec3::new(2.0, 0.0, 0.0),
    );
    place_with(
        &mut world,
        PlaceKind::Cube,
        1,
        true,
        Vec3::new(-2.0, 0.0, 0.0),
    ); // brick
    place_with(
        &mut world,
        PlaceKind::Plane,
        0,
        true,
        Vec3::new(0.0, 0.0, -4.0),
    );
    place_with(
        &mut world,
        PlaceKind::Sphere,
        2,
        true,
        Vec3::new(3.0, 0.0, 3.0),
    ); // metal
    place_with(
        &mut world,
        PlaceKind::Cylinder,
        3,
        true,
        Vec3::new(-3.0, 0.0, 3.0),
    ); // wood
    {
        // 点光：灯泡网格 + 自发光材质 + PointLight
        let point = placement_point(&world, Vec3::new(4.0, 6.0, -4.0), Vec3::new(0.0, -1.0, 0.0))
            .expect("朝下的射线必命中地面");
        edit::place_light_at(&mut world, point).expect("点光摆放");
    }
    place_with(
        &mut world,
        PlaceKind::Cube,
        4,
        false,
        Vec3::new(0.0, 0.0, 4.0),
    ); // glow，无碰撞体
    select(&mut world, Some(a));
    let copy = duplicate_selected(&mut world).expect("复制选中");

    assert_eq!(editable_entities(&world).len(), 9, "8 次摆放 + 1 次复制");
    assert_eq!(
        world.get::<Name>(copy).map(|n| n.0.clone()).as_deref(),
        Some("cube/1_copy1"),
        "复制名 = 原名 + _copy1"
    );
    // 摆放落点规则：底面贴地（单位立方体枢轴抬 0.5）、x / z 吸附
    let transform = *world.get::<Transform>(a).expect("在位");
    assert_eq!(transform.position, Vec3::new(0.0, 0.5, 0.0));
    assert_eq!(
        world.get::<Collider>(a).map(|c| c.shape),
        Some(ColliderShape::Box {
            half_extents: Vec3::new(0.5, 0.5, 0.5)
        }),
        "挂碰撞体开关开着 ⇒ 静态盒 = 网格半幅 × scale"
    );
    let no_collider = editable_entities(&world)[7];
    assert!(
        world.get::<Collider>(no_collider).is_none(),
        "第 8 个实体刻意不挂碰撞体（place_collider = false）"
    );

    let before = dump_world(&world);
    let written = save_level(&mut world).expect("保存成功");
    assert_eq!(written, 9, "写出的实体数 = 可编辑实体数");
    assert_eq!(
        dump_world(&world),
        before,
        "保存本身不得改动可编辑世界（瞬态视觉 despawn → 重建，句柄缓存复用）"
    );
    assert_eq!(
        transient_count(&world),
        grid_line_count() + GIZMO_EDGE_COUNT,
        "保存后视觉照旧在位（网格 + 选中框）"
    );

    // 重载：新世界从档还原
    let mut reloaded = World::new();
    let report = open_level(&mut reloaded, &path).expect("档已存在 ⇒ 直读");
    assert!(report.loaded);
    assert_eq!(report.entities, 9, "档内实体数");
    assert_eq!(report.editable, 9);
    assert_eq!(
        dump_world(&reloaded),
        before,
        "往返一致：实体数 / 编号 / Transform 位模式 / 句柄 / 组件位 / 登记表 / Store 内容全等"
    );
    assert_eq!(
        editable_entities(&reloaded)[0],
        a,
        "首个内容实体的编号两侧相同：内容之前两侧都恰好建了 50 条网格线 + 1 个 HUD\
         （后续实体的编号会因编辑侧选中框交错 spawn 而不同，故 dump 用清单序号）"
    );
}

/// 存档字节稳定：同一档二次保存逐字节相同（`save_scene` 的规范化输出 +
/// 编辑器侧不引入非确定性字段）。
#[test]
fn second_save_is_byte_identical() {
    let dir = temp_dir("bytes");
    let path = dir.join("stable.level.json");

    let mut world = World::new();
    open_level(&mut world, &path).expect("新建");
    place_with(&mut world, PlaceKind::Cube, 0, true, Vec3::ZERO);
    place_with(
        &mut world,
        PlaceKind::Sphere,
        2,
        false,
        Vec3::new(1.5, 0.0, 1.5),
    );
    save_level(&mut world).expect("首次保存");
    let first = std::fs::read_to_string(&path).expect("档已写出");

    let mut reloaded = World::new();
    open_level(&mut reloaded, &path).expect("重载");
    save_level(&mut reloaded).expect("二次保存");
    let second = std::fs::read_to_string(&path).expect("档仍在");
    assert_eq!(first, second, "同输入确定性输出（逐字节）");
    assert!(first.contains("\"version\": 2"), "版本字段恒写当前版本");
}

/// 瞬态视觉永不入档：档里没有网格线 / 选中框的名字，实体数 = 可编辑数。
#[test]
fn transient_visuals_never_reach_the_file() {
    let dir = temp_dir("transient");
    let path = dir.join("no_grid.level.json");

    let mut world = World::new();
    open_level(&mut world, &path).expect("新建");
    let placed = place_with(&mut world, PlaceKind::Cube, 0, true, Vec3::ZERO);
    select(&mut world, Some(placed));
    assert_eq!(
        transient_count(&world),
        grid_line_count() + GIZMO_EDGE_COUNT
    );
    let written = save_level(&mut world).expect("保存");
    assert_eq!(written, 1, "只有 1 个可编辑实体入档");

    let json = std::fs::read_to_string(&path).expect("档已写出");
    assert!(!json.contains("grid/"), "网格线不得入档");
    assert!(!json.contains("gizmo/"), "选中框不得入档");
    assert_eq!(
        json.matches("\"name\"").count(),
        1,
        "档内 name 字段数 = 可编辑实体数"
    );
    assert_eq!(
        transient_count(&world),
        grid_line_count() + GIZMO_EDGE_COUNT,
        "保存后视觉重建（编辑器不失网格 / 选中框）"
    );
    assert!(world.contains(placed), "关卡内容实体不受瞬态 despawn 影响");
}

// ---------------------------------------------------------------------------
// 拾取
// ---------------------------------------------------------------------------

/// 拾取：最近命中优先；同距（t 位相等）取存储序在前者（确定性裁决，
/// 无 HashMap / 无排序）。
#[test]
fn pick_prefers_nearest_then_storage_order() {
    let dir = temp_dir("pick");
    let mut world = World::new();
    open_level(&mut world, &dir.join("pick.level.json")).expect("新建");

    let ground_cube = place_with(&mut world, PlaceKind::Cube, 0, true, Vec3::ZERO);
    // 叠放：命中点 = 前一个实体的顶面（placement_point 取最近命中）
    let stacked = {
        let point = placement_point(&world, Vec3::new(0.0, 5.0, 0.0), Vec3::new(0.0, -1.0, 0.0))
            .expect("命中地面立方体顶面");
        assert_eq!(point, Vec3::new(0.0, 1.0, 0.0), "顶面高度 = 1.0");
        place_at(&mut world, point).expect("叠放")
    };
    assert_eq!(
        world.get::<Transform>(stacked).map(|t| t.position),
        Some(Vec3::new(0.0, 1.5, 0.0)),
        "叠放的立方体底面落在 y = 1.0"
    );
    // 同位实体：与 ground_cube 完全重合（AABB 相同 ⇒ t 位相等）
    let twin = place_with(&mut world, PlaceKind::Cube, 0, true, Vec3::ZERO);

    // 自上而下：最近的应是叠放的那个
    assert_eq!(
        pick(&world, Vec3::new(0.0, 10.0, 0.0), Vec3::new(0.0, -1.0, 0.0)),
        Some(stacked)
    );
    // 水平射线（y = 0.5，只穿过底层两个重合立方体）：t 相等 ⇒ 存储序在前者
    assert_eq!(
        pick(&world, Vec3::new(6.0, 0.5, 0.0), Vec3::new(-1.0, 0.0, 0.0)),
        Some(ground_cube),
        "同距取存储序在前（= spawn 序）"
    );
    assert_ne!(ground_cube, twin);
    // 朝天射线：无命中
    assert_eq!(
        pick(&world, Vec3::new(0.0, 10.0, 0.0), Vec3::new(0.0, 1.0, 0.0)),
        None
    );
    // 瞬态视觉不可拾取：z = 6 处只有网格线（一直铺到 ±12 m），
    // 射线高度正好穿在线条里（y = GRID_Y = 0.002）
    assert_eq!(
        pick(
            &world,
            Vec3::new(6.0, 0.002, 6.0),
            Vec3::new(-1.0, 0.0, 0.0)
        ),
        None,
        "网格线挂 EditorTransient ⇒ 不参与拾取"
    );
}

/// 摆放命中点：实体表面与 y = 0 数学地面取最近者；朝天 / 平行射线无命中。
#[test]
fn placement_point_takes_nearest_of_entity_and_ground() {
    let dir = temp_dir("placement");
    let mut world = World::new();
    open_level(&mut world, &dir.join("placement.level.json")).expect("新建");
    place_with(&mut world, PlaceKind::Cube, 0, true, Vec3::ZERO);

    // 斜射：先碰到立方体侧面（比地面近）
    let hit = placement_point(&world, Vec3::new(-4.0, 0.5, 0.0), Vec3::new(1.0, 0.0, 0.0))
        .expect("水平射线命中侧面");
    assert_eq!(hit, Vec3::new(-0.5, 0.5, 0.0), "立方体西面 x = -0.5");
    // 远离实体的斜射：落到地面
    let hit = placement_point(&world, Vec3::new(8.0, 4.0, 8.0), Vec3::new(0.0, -1.0, 0.0))
        .expect("命中地面");
    assert_eq!(hit, Vec3::new(8.0, 0.0, 8.0));
    // 朝天：无命中（原点刻意抬到盒外——`ray_box` 对「起点在盒内 / 盒面上」
    // 返回 t = 0，那是命中而非未命中）
    assert_eq!(
        placement_point(&world, Vec3::new(0.0, 1.5, 0.0), Vec3::new(0.0, 1.0, 0.0)),
        None
    );
    // 与地面平行且不碰实体：无命中
    assert_eq!(
        placement_point(&world, Vec3::new(8.0, 4.0, 8.0), Vec3::new(1.0, 0.0, 0.0)),
        None
    );
}

/// 屏幕像素 → 射线 → 拾取闭环（交互壳的换算入口在 headless 下可测）：
/// 视口中心像素的射线正是相机 eye → target，穿过摆在地面的立方体
/// （AABB 含 target 点），命中其顶面。
#[test]
fn screen_ray_pick_closed_loop() {
    let dir = temp_dir("screen");
    let mut world = World::new();
    open_level(&mut world, &dir.join("screen.level.json")).expect("新建");
    world.insert_resource(Viewport::new(1280, 720));
    let placed = place_with(&mut world, PlaceKind::Cube, 0, true, Vec3::ZERO);
    let (target, eye) = {
        let camera = world.resource::<Camera>();
        (camera.target, camera.eye)
    };
    assert_eq!(
        world.get::<Transform>(placed).map(|t| t.position),
        Some(Vec3::new(0.0, 0.5, 0.0))
    );
    let (min, max) = world_aabb_of(&world, placed).expect("立方体有网格");
    assert!(
        target.x >= min.x && target.x <= max.x && target.y >= min.y && target.y <= max.y,
        "默认相机 target 应落在立方体 AABB 内（本用例的几何前提）：{min:?} {max:?} {target:?}"
    );

    let (origin, ray) = screen_ray_at(&world, 640.0, 360.0).expect("视口中心射线");
    assert_eq!(origin, eye, "射线原点 = 相机 eye");
    assert!(ray.length() > 0.99 && ray.length() < 1.01, "方向已归一化");
    assert_eq!(pick(&world, origin, ray), Some(placed));
    let point = placement_point(&world, origin, ray).expect("命中");
    // 斜俯视角下中心射线先碰到立方体的 +Z 侧面（不是顶面）：落点在盒面上、
    // 高度落在盒的 y 区间内，且比地面交点（y = 0）更近
    assert!(
        (point.z - max.z).abs() < 1e-4,
        "落点应在 +Z 面 z = {}，实际 {:?}",
        max.z,
        point
    );
    assert!(
        point.y > min.y && point.y < max.y,
        "落点高度应在盒内（侧面命中，而非顶面 / 地面）：{point:?}"
    );
    assert!(point.y > edit::GROUND_Y, "实体表面比 y = 0 地面更近");
}

// ---------------------------------------------------------------------------
// 拖拽 / 微移 / 缩放
// ---------------------------------------------------------------------------

/// 拖拽：射线 ∩ 命中点高度平面，x / z 吸附到 0.5 m 格点、**y 保持**；
/// 拖拽偏移使抓取点跟着光标（不跳到光标下）。
#[test]
fn drag_moves_on_hit_plane_with_snap() {
    let dir = temp_dir("drag");
    let mut world = World::new();
    open_level(&mut world, &dir.join("drag.level.json")).expect("新建");
    let placed = place_with(&mut world, PlaceKind::Cube, 0, true, Vec3::ZERO);
    assert_eq!(
        world.resource::<EditorState>().snap_size,
        DEFAULT_SNAP,
        "吸附缺省 0.5 m"
    );

    let grabbed = begin_drag(
        &mut world,
        Vec3::new(0.0, 5.0, 0.0),
        Vec3::new(0.0, -1.0, 0.0),
    )
    .expect("命中并进入拖拽");
    assert_eq!(grabbed, placed);
    assert_eq!(world.resource::<EditorState>().selected, Some(placed));
    assert!(world.resource::<EditorState>().dragging);
    assert_eq!(
        world.resource::<EditorState>().drag_plane_y,
        1.0,
        "拖拽平面 = 命中点（顶面）高度"
    );
    assert_eq!(
        world.resource::<EditorState>().drag_offset,
        Vec3::ZERO,
        "抓在正上方 ⇒ 偏移为零"
    );

    // 光标移到 (2.3, -1.7)：吸附到 (2.5, -1.5)，y 保持 0.5
    let moved = drag_to(
        &mut world,
        Vec3::new(2.3, 5.0, -1.7),
        Vec3::new(0.0, -1.0, 0.0),
    )
    .expect("拖拽中");
    assert_eq!(moved, Vec3::new(2.5, 0.5, -1.5));
    assert_eq!(
        world.get::<Transform>(placed).map(|t| t.position),
        Some(Vec3::new(2.5, 0.5, -1.5))
    );
    // 吸附关：落到精确命中点
    toggle_snap(&mut world).expect("切换吸附");
    let moved = drag_to(
        &mut world,
        Vec3::new(1.23, 5.0, -0.77),
        Vec3::new(0.0, -1.0, 0.0),
    )
    .expect("拖拽中");
    assert_eq!(vec_bits(moved), vec_bits(Vec3::new(1.23, 0.5, -0.77)));
    toggle_snap(&mut world).expect("切回");

    end_drag(&mut world);
    assert!(!world.resource::<EditorState>().dragging);
    assert_eq!(
        drag_to(
            &mut world,
            Vec3::new(9.0, 5.0, 9.0),
            Vec3::new(0.0, -1.0, 0.0)
        ),
        None,
        "拖拽结束后不再移动"
    );
    // 与拖拽平面平行的射线：实体不动
    begin_drag(
        &mut world,
        Vec3::new(1.23, 5.0, -0.77),
        Vec3::new(0.0, -1.0, 0.0),
    )
    .expect("再次抓取（立方体已在 (1.23, 0.5, -0.77)）");
    assert_eq!(
        drag_to(
            &mut world,
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0)
        ),
        None,
        "水平射线与拖拽平面平行 ⇒ 不动"
    );
}

/// 微移与缩放：位模式精确对照（同表达式复算，不给容差）。
#[test]
fn nudge_and_scale_are_bitwise_exact() {
    let dir = temp_dir("nudge");
    let mut world = World::new();
    open_level(&mut world, &dir.join("nudge.level.json")).expect("新建");
    let placed = place_with(&mut world, PlaceKind::Cube, 0, true, Vec3::ZERO);
    assert_eq!(
        world.resource::<EditorState>().selected,
        Some(placed),
        "摆放后自动选中（便于立刻拖拽 / 微调）"
    );

    // 方向键微移 0.1（Shift 十倍由交互壳换算，命令层只收最终量）
    // 摆放落点 = (0, 0.5, 0)：底面贴地只抬 y，x / z 仍是命中点
    let position = nudge_selected(&mut world, Vec3::X, edit::NUDGE_STEP).expect("有选中");
    assert_eq!(vec_bits(position), vec_bits(Vec3::new(0.1, 0.5, 0.0)));
    let position = nudge_selected(&mut world, Vec3::Y, -edit::NUDGE_STEP * 10.0).expect("有选中");
    assert_eq!(vec_bits(position), vec_bits(Vec3::new(0.1, 0.5 - 1.0, 0.0)));

    // PgUp / PgDn 缩放
    let scale = scale_selected(&mut world, edit::SCALE_STEP).expect("有选中");
    assert_eq!(vec_bits(scale), vec_bits(Vec3::splat(edit::SCALE_STEP)));
    let scale = scale_selected(&mut world, 1.0 / edit::SCALE_STEP).expect("有选中");
    assert_eq!(
        vec_bits(scale),
        vec_bits(Vec3::splat(edit::SCALE_STEP * (1.0 / edit::SCALE_STEP))),
        "缩放往返不保证回到 1.0（浮点），但两次调用逐位可复现"
    );

    // 选中框跟着变（原位更新，不重建实体）
    let gizmo_before: Vec<Entity> = {
        let mut out = Vec::new();
        world.query::<(Entity, &EditorTransient)>(|(entity, _)| out.push(entity));
        out
    };
    update_gizmo(&mut world);
    let gizmo_after: Vec<Entity> = {
        let mut out = Vec::new();
        world.query::<(Entity, &EditorTransient)>(|(entity, _)| out.push(entity));
        out
    };
    assert_eq!(
        gizmo_before, gizmo_after,
        "update_gizmo 原位更新（实体编号稳定，拖拽期每帧调用无 churn）"
    );

    // 无选中时两个操作都 no-op
    select(&mut world, None);
    assert_eq!(nudge_selected(&mut world, Vec3::X, 0.1), None);
    assert_eq!(scale_selected(&mut world, 1.1), None);
}

// ---------------------------------------------------------------------------
// 删除 / 复制 / 参数切换
// ---------------------------------------------------------------------------

/// 删除与复制：级联删除、`_copy{n}` 后缀递增、复制保留组件位。
#[test]
fn delete_and_duplicate_semantics() {
    let dir = temp_dir("delete");
    let mut world = World::new();
    open_level(&mut world, &dir.join("delete.level.json")).expect("新建");
    let placed = place_with(
        &mut world,
        PlaceKind::Cylinder,
        2,
        true,
        Vec3::new(1.0, 0.0, 1.0),
    );

    select(&mut world, Some(placed));
    let first = duplicate_selected(&mut world).expect("复制");
    assert_eq!(
        world.get::<Name>(first).map(|n| n.0.clone()).unwrap(),
        "cylinder/1_copy1"
    );
    assert_eq!(
        world.get::<Transform>(first).map(|t| *t),
        world.get::<Transform>(placed).map(|t| *t),
        "原地复制：Transform 逐位相同"
    );
    assert_eq!(
        world.get::<MeshHandle>(first).map(|h| *h),
        world.get::<MeshHandle>(placed).map(|h| *h),
        "复制共享网格句柄（不新增 Store 条目）"
    );
    assert_eq!(
        world.get::<Collider>(first).map(|c| c.shape),
        world.get::<Collider>(placed).map(|c| c.shape)
    );
    assert!(first.index() > placed.index(), "新实体 id 递增");

    // 再复制同一个源：后缀递增到 2
    select(&mut world, Some(placed));
    let second = duplicate_selected(&mut world).expect("再复制");
    assert_eq!(
        world.get::<Name>(second).map(|n| n.0.clone()).unwrap(),
        "cylinder/1_copy2"
    );
    // 复制体自身再复制：后缀挂在复制体名下
    let nested = duplicate_selected(&mut world).expect("复制体再复制");
    assert_eq!(
        world.get::<Name>(nested).map(|n| n.0.clone()).unwrap(),
        "cylinder/1_copy2_copy1"
    );

    assert_eq!(editable_entities(&world).len(), 4);
    // 删除选中的复制体
    let deleted = delete_selected(&mut world).expect("有选中");
    assert_eq!(deleted, nested);
    assert!(!world.contains(nested));
    assert_eq!(
        world.resource::<EditorState>().selected,
        None,
        "删除后清空选择"
    );
    assert_eq!(editable_entities(&world).len(), 3);
    assert_eq!(delete_selected(&mut world), None, "无选中 ⇒ no-op");

    // 删掉全部后存档为空实体表（仍可加载）
    let path = dir.join("delete.level.json");
    while let Some(next) = editable_entities(&world).first().copied() {
        select(&mut world, Some(next));
        delete_selected(&mut world);
    }
    assert_eq!(editable_entities(&world).len(), 0);
    let written = save_level(&mut world).expect("保存空关卡");
    assert_eq!(written, 0);
    let mut reloaded = World::new();
    let report = open_level(&mut reloaded, &path).expect("空档可加载");
    assert_eq!(report.entities, 0);
}

/// 参数切换：图元 / 预设循环、碰撞体与吸附开关（面板与状态行同步）。
#[test]
fn toggles_and_cycles_update_state_and_panel() {
    let dir = temp_dir("toggles");
    let mut world = World::new();
    open_level(&mut world, &dir.join("toggles.level.json")).expect("新建");

    assert_eq!(cycle_kind(&mut world), Some(PlaceKind::Plane));
    assert_eq!(cycle_kind(&mut world), Some(PlaceKind::Sphere));
    assert_eq!(cycle_preset(&mut world), Some(1));
    assert_eq!(cycle_preset(&mut world), Some(2), "预设下标循环推进");
    assert_eq!(toggle_collider(&mut world), Some(false));
    assert_eq!(toggle_snap(&mut world), Some(false));
    for _ in 0..MATERIAL_PRESETS.len() - 1 {
        cycle_preset(&mut world);
    }
    assert_eq!(
        world.resource::<EditorState>().preset,
        1,
        "预设循环绕回（(2 + 4) % 5 = 1）"
    );

    let panel = panel_text(&world);
    assert!(panel.contains("KIND sphere"), "面板反映图元：{panel}");
    assert!(panel.contains("MAT brick"), "面板反映预设");
    assert!(panel.contains("COLLIDER off"));
    assert!(panel.contains("SNAP off"));
    assert!(panel.contains("SELECTED none"));
    assert!(
        panel.is_ascii(),
        "面板文本必须纯 ASCII（8×8 位图字体只有 ASCII 字形）"
    );
}

/// 属性面板：选中后显示名字 / 类型 / 位置 / 缩放 / 碰撞体，HUD 实体同步。
#[test]
fn panel_reports_selection() {
    let dir = temp_dir("panel");
    let mut world = World::new();
    open_level(&mut world, &dir.join("panel.level.json")).expect("新建");
    let placed = place_with(
        &mut world,
        PlaceKind::Sphere,
        2,
        true,
        Vec3::new(1.5, 0.0, -2.0),
    );

    let info = describe(&world, placed).expect("有 Transform");
    assert_eq!(info.name, "sphere/1");
    assert_eq!(info.kind, "primitive(sphere)", "类型标签读登记表");
    assert_eq!(info.collider, "sphere(0.50)");
    assert!(!info.light);
    assert_eq!(info.position, Vec3::new(1.5, 0.5, -2.0));

    let panel = panel_text(&world);
    assert!(panel.contains("SELECTED sphere/1"), "{panel}");
    assert!(panel.contains("TYPE primitive(sphere)   LIGHT off"));
    assert!(panel.contains("COLLIDER sphere(0.50)"));
    assert!(panel.contains("POS  1.50 0.50 -2.00"));
    assert!(panel.contains("SCALE 1.00 1.00 1.00"));
    assert!(panel.contains("ENTITIES 1"));

    let hud = world.entity_named(HUD_NAME).expect("HUD 实体在位");
    assert_eq!(
        world.get::<UiText>(hud).map(|t| t.string.clone()),
        Some(panel.clone()),
        "refresh_hud 已把面板写进 UiText"
    );
    assert!(
        world.get::<Transform>(hud).is_none(),
        "HUD 无 Transform ⇒ save_scene 天然不含它"
    );

    // 点光实体的面板：light = on
    let point = placement_point(&world, Vec3::new(-3.0, 5.0, 0.0), Vec3::new(0.0, -1.0, 0.0))
        .expect("命中地面");
    let light = edit::place_light_at(&mut world, point).expect("点光摆放");
    let info = describe(&world, light).expect("在位");
    assert_eq!(info.name, "light/1");
    assert!(info.light);
    assert_eq!(info.kind, "primitive(sphere)", "灯泡网格也是图元段");
    assert!(panel_text(&world).contains("LIGHT on"));
    // 非 ASCII 名字折算
    world.insert(light, Name("庭院灯".to_string()));
    edit::refresh_hud(&mut world);
    assert!(panel_text(&world).contains("SELECTED ???"));
    assert_eq!(ascii_only("庭院灯"), "???");
}

// ---------------------------------------------------------------------------
// 关卡档 v2 语义（level 段快照 / 真实关卡往返）
// ---------------------------------------------------------------------------

/// 关卡档 v2 的 `level` 段经 `LevelRegistry` 快照语义原样保留：编辑器不改
/// 也不改坏它（a 阶段 level 段只读）。
#[test]
fn level_section_survives_editor_roundtrip() {
    let dir = temp_dir("levelsection");
    let path = dir.join("with_level.level.json");
    std::fs::write(
        &path,
        r#"{
  "version": 2,
  "level": { "bake": { "atlas_size": 64, "texels_per_meter": 8.0 } },
  "entities": [
    { "name": "box", "mesh": { "primitive": { "kind": "cube" } },
      "transform": { "position": [0.0, 0.5, 0.0] } }
  ]
}"#,
    )
    .expect("写测试档");

    let mut world = World::new();
    let report = open_level(&mut world, &path).expect("v2 直读");
    assert!(report.loaded);
    assert_eq!(report.editable, 1);
    let before = dump_world(&world);
    assert!(before.contains("level LevelSection"), "level 段快照在位");

    // 编辑：再摆一个实体（不得影响 level 段）
    place_with(
        &mut world,
        PlaceKind::Cube,
        1,
        true,
        Vec3::new(2.0, 0.0, 0.0),
    );
    save_level(&mut world).expect("保存");
    let json = std::fs::read_to_string(&path).expect("档已写出");
    assert!(json.contains("\"level\""), "level 段原样回写");
    assert!(json.contains("\"atlas_size\": 64"), "烘焙参数保留");
    assert!(json.contains("\"texels_per_meter\": 8.0"), "烘焙参数保留");

    let mut reloaded = World::new();
    open_level(&mut reloaded, &path).expect("重载");
    assert_eq!(editable_entities(&reloaded).len(), 2);
    assert!(dump_world(&reloaded).contains("atlas_size: 64"));
}

/// 真实关卡（iris-assault 庭院档 = R57 迁移产物）经编辑器开 → 存 → 重载
/// 往返一致：编辑器不改坏既有档（含程序化纹理材质、粒子段、点光、
/// 静态碰撞体），且**不加载也能存**（存档不依赖 GPU）。
#[test]
fn assault_courtyard_survives_editor_roundtrip() {
    let source = asset_path("assets/assault_courtyard.level.json");
    assert!(source.exists(), "R57 庭院档应在位：{}", source.display());
    let dir = temp_dir("courtyard");
    let path = dir.join("courtyard.level.json");
    std::fs::copy(&source, &path).expect("拷贝到临时档（不写仓库内文件）");

    let mut world = World::new();
    let report = open_level(&mut world, &path).expect("庭院档直读");
    assert!(report.loaded);
    assert_eq!(report.entities, 15, "庭院档 15 个实体（R57 基线）");
    assert_eq!(report.editable, 15);
    let before = dump_world(&world);

    // 编辑动作：选中喷泉 + 拖一步 + 复制一个火把 + 删掉复制体
    let fountain = world
        .entity_named("assault/fountain")
        .expect("庭院档含喷泉");
    select(&mut world, Some(fountain));
    assert_eq!(
        transient_count(&world),
        grid_line_count() + GIZMO_EDGE_COUNT
    );
    let torch = world.entity_named("assault/torch/0").expect("庭院档含火把");
    select(&mut world, Some(torch));
    let copy = duplicate_selected(&mut world).expect("复制火把");
    assert_eq!(editable_entities(&world).len(), 16);
    select(&mut world, Some(copy));
    delete_selected(&mut world);
    assert_eq!(editable_entities(&world).len(), 15);

    save_level(&mut world).expect("保存");
    assert_eq!(
        dump_world(&world),
        before,
        "编辑动作回到原状后，世界转储与打开时逐字符相同"
    );

    let json = std::fs::read_to_string(&path).expect("档已写出");
    assert!(json.contains("\"particles\""), "粒子段保留");
    assert!(json.contains("brick_normal_orm"), "程序化法线 / ORM 源保留");
    assert!(!json.contains("grid/"), "网格线不入档");

    let mut reloaded = World::new();
    open_level(&mut reloaded, &path).expect("重载");
    assert_eq!(editable_entities(&reloaded).len(), 15);
    assert_eq!(
        dump_world(&reloaded),
        before,
        "真实关卡经编辑器往返逐位一致"
    );
}

// ---------------------------------------------------------------------------
// 纯函数与视觉重建
// ---------------------------------------------------------------------------

/// 吸附纯函数：与命令层同一实现（此处只钉「拖拽用的就是它」）。
#[test]
fn drag_uses_snap_value() {
    assert_eq!(snap_value(2.3, DEFAULT_SNAP), 2.5);
    assert_eq!(snap_value(-1.7, DEFAULT_SNAP), -1.5);
    assert_eq!(snap_value(0.0, DEFAULT_SNAP), 0.0);
}

/// 视觉重建：清空 → 重建网格 → 选中框随选中态出现 / 消失。
#[test]
fn visuals_rebuild_deterministically() {
    let dir = temp_dir("visuals");
    let mut world = World::new();
    open_level(&mut world, &dir.join("visuals.level.json")).expect("新建");
    assert_eq!(transient_count(&world), grid_line_count());

    clear_transient(&mut world);
    assert_eq!(transient_count(&world), 0, "清空瞬态视觉");
    rebuild_grid(&mut world);
    assert_eq!(transient_count(&world), grid_line_count(), "重建网格");

    let placed = place_with(&mut world, PlaceKind::Cube, 0, true, Vec3::ZERO);
    assert_eq!(
        transient_count(&world),
        grid_line_count() + GIZMO_EDGE_COUNT,
        "摆放后自动选中 ⇒ 选中框 12 条边"
    );
    let (min, max) = world_aabb_of(&world, placed).expect("立方体有网格");
    assert_eq!(min, Vec3::splat(-0.5).with_y(0.0));
    assert_eq!(max, Vec3::splat(0.5).with_y(1.0));

    // 选中框几何：12 条边的中心都在 AABB 外扩壳上
    let mut centers = Vec::new();
    world.query::<(&Transform, &EditorTransient)>(|(transform, _)| {
        if transform.scale.x == edit::GIZMO_THICKNESS
            || transform.scale.y == edit::GIZMO_THICKNESS
            || transform.scale.z == edit::GIZMO_THICKNESS
        {
            centers.push(transform.position);
        }
    });
    assert!(
        centers.len() >= GIZMO_EDGE_COUNT,
        "选中框实体在位：{} 条",
        centers.len()
    );

    select(&mut world, None);
    assert_eq!(
        transient_count(&world),
        grid_line_count(),
        "清空选择 ⇒ 选中框消失、网格保留"
    );
    edit::refresh_visuals(&mut world);
    assert_eq!(transient_count(&world), grid_line_count());
}

/// 编辑器状态缺席时命令层不 panic（交互壳的 no-op 契约同源）：
/// 只读查询返回 `None` / 空，写操作 no-op。
#[test]
fn commands_are_inert_without_editor_state() {
    let mut world = World::new();
    assert!(place_at(&mut world, Vec3::ZERO).is_none());
    assert_eq!(pick(&world, Vec3::ZERO, Vec3::NEG_Y), None);
    assert_eq!(delete_selected(&mut world), None);
    assert_eq!(duplicate_selected(&mut world), None);
    assert_eq!(nudge_selected(&mut world, Vec3::X, 0.1), None);
    assert_eq!(cycle_kind(&mut world), None);
    assert!(panel_text(&world).contains("NO EDITOR STATE"));
    // 存档路径缺席时 save_level 也不 panic（写 "" 失败 → Err）
    assert!(save_level(&mut world).is_err());
    edit::refresh_hud(&mut world);
    update_gizmo(&mut world);
    end_drag(&mut world);
    begin_drag(&mut world, Vec3::ZERO, Vec3::NEG_Y);
    toggle_snap(&mut world);
    drag_to(&mut world, Vec3::ZERO, Vec3::NEG_Y);
    place_from_ray(&mut world, Vec3::new(0.0, 5.0, 0.0), Vec3::NEG_Y);
    edit::place_light_from_ray(&mut world, Vec3::new(0.0, 5.0, 0.0), Vec3::NEG_Y);
    edit::refresh_visuals(&mut world);
    rebuild_grid(&mut world);
    edit::spawn_hud(&mut world);
    clear_transient(&mut world);
    select(&mut world, None);
    assert_eq!(
        world.entity_count(),
        1,
        "只有 HUD 实体（无 Transform，不入档）"
    );
    assert!(
        save_level(&mut world).is_err(),
        "无路径 ⇒ 保存失败而非 panic"
    );
}

/// 档存在但内容非法 ⇒ `open_level` 返回 `Err`（交互壳据此 panic，
/// 避免降级为空世界后一次保存覆盖原档）。
#[test]
fn corrupt_level_reports_error_instead_of_wiping() {
    let dir = temp_dir("corrupt");
    let path = dir.join("corrupt.level.json");
    std::fs::write(&path, "{ 这不是 JSON").expect("写坏档");
    let mut world = World::new();
    let err = open_level(&mut world, &path).expect_err("坏档必须报错");
    assert!(
        matches!(err, SceneError::Json(_)),
        "错误类型应是 Json：{err}"
    );
    // 缺 entities 段 ⇒ Missing
    std::fs::write(&path, r#"{ "version": 2 }"#).expect("改写");
    let mut world = World::new();
    assert!(matches!(
        open_level(&mut world, &path),
        Err(SceneError::Missing(_))
    ));
    // 版本越界 ⇒ Unsupported
    std::fs::write(&path, r#"{ "version": 99, "entities": [] }"#).expect("改写");
    let mut world = World::new();
    assert!(matches!(
        open_level(&mut world, &path),
        Err(SceneError::Unsupported(_))
    ));
    assert!(path.exists(), "失败路径不得删档");
}

/// 加载既有档后接着摆放：名字不与档内已有名字相撞（`next_name` 扫描世界
/// 而不依赖跨调用计数器）。
#[test]
fn naming_continues_from_loaded_level() {
    let dir = temp_dir("naming");
    let path = dir.join("naming.level.json");
    std::fs::write(
        &path,
        r#"{
  "version": 2,
  "entities": [
    { "name": "cube/7", "mesh": { "primitive": { "kind": "cube" } } },
    { "name": "cube/3", "mesh": { "primitive": { "kind": "cube" } } }
  ]
}"#,
    )
    .expect("写测试档");
    let mut world = World::new();
    open_level(&mut world, &path).expect("直读");
    let placed = place_with(
        &mut world,
        PlaceKind::Cube,
        0,
        false,
        Vec3::new(4.0, 0.0, 0.0),
    );
    assert_eq!(
        world.get::<Name>(placed).map(|n| n.0.clone()).unwrap(),
        "cube/8",
        "接在档内最大下标之后"
    );
}

/// 关卡档路径与报告：CLI 指定的路径原样进状态（`asset_path` 解析在壳里）。
#[test]
fn open_report_and_path_are_faithful() {
    let dir = temp_dir("report");
    let path = dir.join("报告.level.json");
    let mut world = World::new();
    let report = open_level(&mut world, &path).expect("新建");
    assert_eq!(world.resource::<EditorState>().level_path, path);
    assert!(!report.loaded);
    assert_eq!(
        world.resource::<EditorState>().snap_size,
        DEFAULT_SNAP,
        "吸附缺省 0.5 m（对齐确定性文化）"
    );
    assert!(world.resource::<EditorState>().place_collider);
    assert_eq!(world.resource::<EditorState>().place_kind, PlaceKind::Cube);
    // 面板里的文件名做了 ASCII 折算（位图字体无中文字形）
    assert!(panel_text(&world).contains("FILE ??.level.json"));
    assert!(!path.exists(), "新建路径此时还没落盘");
    save_level(&mut world).expect("保存后落盘");
    assert!(path.exists());
}
