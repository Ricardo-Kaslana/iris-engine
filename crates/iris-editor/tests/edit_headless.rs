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
use iris_core::hierarchy::propagate_transforms;
use iris_editor::edit::{
    self, DEFAULT_SNAP, DdgiEditMode, EditorState, EditorTransient, GIZMO_EDGE_COUNT, GRID_EXTENT,
    GRID_STEP, HUD_NAME, LIGHT_INTENSITY_STEP, LIGHT_RANGE_STEP, MATERIAL_PRESETS, PlaceKind,
    ascii_only, bake_and_install, begin_drag, clear_transient, cycle_kind, cycle_preset,
    ddgi_bounds_translate, ddgi_edit_mode, ddgi_hysteresis_step, ddgi_mode_cycle,
    ddgi_overlay_toggle, ddgi_probes_per_frame_step, ddgi_rays_face_step, ddgi_spacing_step,
    delete_selected, describe, drag_to, duplicate_selected, editable_entities, end_drag,
    focus_selected, light_edit_mode, light_hue_step, light_intensity_step, light_range_step,
    light_shadow_toggle, nav_blocked_count, nav_bounds_from_statics, nudge_selected, open_level,
    orbit_camera, panel_text, pick, place_at, place_from_ray, place_probe_at, placement_point,
    probe_falloff_step, probe_selected, probe_size_step, rebuild_grid, rebuild_nav, redo,
    redo_depth, save_level, scale_selected, screen_ray_at, select, snap_value, sun_ambient_step,
    sun_direction_step, toggle_collider, toggle_nav_viz, toggle_snap, undo, undo_depth,
    update_gizmo, world_aabb_of,
};
use iris_engine::prelude::*;

// ---------------------------------------------------------------------------
// 测试台
// ---------------------------------------------------------------------------

/// 唯一临时目录（进程 id + 进程内调用序计数）。
///
/// **只用 pid 会撞上历史运行的残留目录**：测试不在结束时清目录，
/// Windows 复用 pid 后 `create_dir_all` 命中旧目录 ⇒ `open_level`
/// 报 `loaded = true`、陈旧烘焙产物失配——R63 验收实测 release 挂
/// 4 项（debug 时序不同侥幸通过）。加调用序计数后需「同 pid 且同
/// 调用序」才可能撞（与 AGENTS.md 临时文件唯一名规约一致，第三案
/// asset_pipeline 同款修法）。
fn temp_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("iris-editor-{tag}-{}-{seq}", std::process::id()));
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
    dump_world_with(world, &editable_entities(world))
}

/// 名字序版世界转储（R58b undo 测试用）：undo 恢复是「全删全建」，实体
/// 槽位号经分配器空闲列表（LIFO 复用）后不可复现——槽位本就是分配器
/// 副产物（[`dump_world`] 注释同款立场）。undo 的正确性性质 =「同实体
/// 集 + 同组件内容」，名字唯一（`next_name` / `_copy{n}` 保证），按名字
/// 序转储即与槽位无关。
fn dump_world_sorted(world: &World) -> String {
    let mut entities = editable_entities(world);
    entities.sort_by_key(|entity| {
        world
            .get::<Name>(*entity)
            .map(|name| name.0.clone())
            .unwrap_or_default()
    });
    dump_world_with(world, &entities)
}

fn dump_world_with(world: &World, entities: &[Entity]) -> String {
    let mut out = String::new();

    out.push_str(&format!("editable {}\n", entities.len()));
    // 转储**清单序号**而非 `Entity` 原始编号：编辑侧的瞬态视觉（选中框
    // 12 条边、网格线）与关卡内容是交错 spawn 的（摆放后自动选中 ⇒ 立刻
    // 生成选中框），原始编号必然与重载侧不同；而关卡档里没有实体 id 这个
    // 东西——往返可对照的是「顺序 + 内容」，编号只是分配器副产物。
    for (slot, &entity) in entities.iter().enumerate() {
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
    if let Some(settings) = world.try_resource::<DdgiSettings>() {
        out.push_str(&format!(
            "ddgi spacing {} k {} rays {} hyst {} bounds {:?} sky {:?}\n",
            bits(settings.spacing),
            settings.probes_per_frame,
            settings.rays_per_face,
            bits(settings.hysteresis),
            settings.bounds.map(|(min, max)| (
                [bits(min.x), bits(min.y), bits(min.z)],
                [bits(max.x), bits(max.y), bits(max.z)]
            )),
            settings
                .sky_color
                .map(|v| [bits(v.x), bits(v.y), bits(v.z)]),
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
    // R58b：重载侧走「内容实体先行」的规范位布局（内容实体 0 号槽起步，
    // 与 `load_scene` 一致——烘焙产物按实体位引用的前提）；创建会话的
    // 内容实体在家具（50 网格线 + HUD）之后，两侧编号本就不同构
    // （dump 用清单序号对照，见模块文档「句柄编号对齐」）。
    assert_eq!(
        editable_entities(&reloaded)[0].index(),
        0,
        "重载侧首个内容实体 = 0 号槽（规范位布局）"
    );
    assert!(a.index() > 50, "创建会话的内容实体在家具之后");
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

// ---------------------------------------------------------------------------
// R58b：undo / redo（快照栈）
// ---------------------------------------------------------------------------

/// undo / redo 多步序列：逐步 dump 对照 + 「undo 到头再 redo 到尾」全恢复。
/// 覆盖的写操作：摆放 ×2、复制、微移、缩放、删除。快照字段集 = dump
/// 字段集 ⇒ 每步 undo 后的 dump 与该操作执行前逐字符一致。
#[test]
fn undo_redo_multi_step_sequence_with_full_recovery() {
    let dir = temp_dir("undo");
    let mut world = World::new();
    open_level(&mut world, &dir.join("undo.level.json")).expect("新建");

    let d0 = dump_world_sorted(&world); // 空世界
    let a = place_with(&mut world, PlaceKind::Cube, 0, true, Vec3::ZERO);
    let d1 = dump_world_sorted(&world);
    place_with(
        &mut world,
        PlaceKind::Sphere,
        2,
        false,
        Vec3::new(3.0, 0.0, 0.0),
    );
    let d2 = dump_world_sorted(&world);
    select(&mut world, Some(a));
    let _copy = duplicate_selected(&mut world).expect("复制");
    let d3 = dump_world_sorted(&world);
    nudge_selected(&mut world, Vec3::X, 0.3).expect("微移");
    let d4 = dump_world_sorted(&world);
    scale_selected(&mut world, 1.5).expect("缩放");
    let d5 = dump_world_sorted(&world);
    // 删除球
    let sphere = world.entity_named("sphere/1").expect("球在位");
    select(&mut world, Some(sphere));
    delete_selected(&mut world);
    let d6 = dump_world_sorted(&world);
    assert_eq!(undo_depth(&world), 6, "六步写操作 = 六个快照");
    assert_eq!(redo_depth(&world), 0);

    // undo：每步都精确回到该步执行前的 dump
    assert!(undo(&mut world));
    assert_eq!(dump_world_sorted(&world), d5, "undo 删除");
    assert!(undo(&mut world));
    assert_eq!(dump_world_sorted(&world), d4, "undo 缩放");
    assert!(undo(&mut world));
    assert_eq!(dump_world_sorted(&world), d3, "undo 微移");
    assert!(undo(&mut world));
    assert_eq!(dump_world_sorted(&world), d2, "undo 复制");
    assert!(undo(&mut world));
    assert_eq!(dump_world_sorted(&world), d1, "undo 第二次摆放");
    assert!(undo(&mut world));
    assert_eq!(dump_world_sorted(&world), d0, "undo 第一次摆放：世界回到空");
    assert!(!undo(&mut world), "undo 到头再按 = no-op");
    assert_eq!(dump_world_sorted(&world), d0);
    assert_eq!(undo_depth(&world), 0);
    assert_eq!(redo_depth(&world), 6);

    // redo 到尾：全恢复
    for expected in [
        d1.as_str(),
        d2.as_str(),
        d3.as_str(),
        d4.as_str(),
        d5.as_str(),
        d6.as_str(),
    ] {
        assert!(redo(&mut world));
        assert_eq!(dump_world_sorted(&world), expected, "redo 逐步恢复");
    }
    assert!(!redo(&mut world), "redo 到尾再按 = no-op");
    assert_eq!(undo_depth(&world), 6);
    // undo 后新写操作清空 redo 栈
    assert!(undo(&mut world));
    assert_eq!(redo_depth(&world), 1);
    place_with(
        &mut world,
        PlaceKind::Cube,
        1,
        false,
        Vec3::new(-5.0, 0.0, 0.0),
    );
    assert_eq!(redo_depth(&world), 0, "新写操作清空 redo 栈");
    assert!(!redo(&mut world));
    // 撤销后的世界照常保存、重载：内容逐实体一致（按名对照 Transform
    // 位模式）。注意不比整份 dump——undo 的全删全建让槽位序（= 存档实体
    // 序）偏离编辑器首触序，重载侧按文件序给句柄编号，两侧句柄号本就
    // 允许不同（编号是分配器 / 加载序副产物，R58a dump 注释同款立场）。
    save_level(&mut world).expect("undo 后保存");
    let mut reloaded = World::new();
    open_level(&mut reloaded, &dir.join("undo.level.json")).expect("重载");
    let world_names = editable_entities(&world).len();
    assert_eq!(editable_entities(&reloaded).len(), world_names);
    for entity in editable_entities(&world) {
        let name = world
            .get::<Name>(entity)
            .map(|n| n.0.clone())
            .expect("可编辑实体都有名");
        let reloaded_entity = reloaded
            .entity_named(&name)
            .unwrap_or_else(|| panic!("重载侧缺实体 {name}"));
        let a = *world.get::<Transform>(entity).expect("在位");
        let b = *reloaded
            .get::<Transform>(reloaded_entity)
            .unwrap_or_else(|| panic!("重载侧 {name} 缺 Transform"));
        let t_bits = |t: &Transform| {
            (
                t.position.to_array().map(|v| v.to_bits()),
                t.rotation.to_array().map(|v| v.to_bits()),
                t.scale.to_array().map(|v| v.to_bits()),
            )
        };
        assert_eq!(t_bits(&a), t_bits(&b), "{name} 的 Transform 逐位一致");
    }
}

/// undo 深度 32 环形丢弃（最旧覆盖）：33 次摆放后栈里只剩 32 份快照，
/// undo 32 次回到「第 1 次摆放后」而非空世界（最早的快照已被丢弃）。
#[test]
fn undo_depth_ring_discards_oldest() {
    let dir = temp_dir("undoring");
    let mut world = World::new();
    open_level(&mut world, &dir.join("ring.level.json")).expect("新建");
    for i in 0..33 {
        place_with(
            &mut world,
            PlaceKind::Cube,
            0,
            false,
            Vec3::new(i as f32 * 2.0, 0.0, 0.0),
        );
    }
    assert_eq!(editable_entities(&world).len(), 33);
    assert_eq!(undo_depth(&world), 32, "环形丢弃最旧（33 入 32 出）");
    for remaining in (1..=32).rev() {
        assert!(undo(&mut world));
        assert_eq!(
            editable_entities(&world).len(),
            remaining,
            "undo 后实体数 = 快照时点"
        );
    }
    assert!(!undo(&mut world), "栈已空");
    assert_eq!(
        editable_entities(&world).len(),
        1,
        "最早的快照（空世界）已被环形丢弃——只能回到第 1 次摆放后"
    );
}

/// 拖拽的快照语义：按下时拍一次（拖动中途不拍），undo 一步回到按下前。
#[test]
fn drag_is_one_undo_step_taken_at_press() {
    let dir = temp_dir("undodrag");
    let mut world = World::new();
    open_level(&mut world, &dir.join("undodrag.level.json")).expect("新建");
    place_with(&mut world, PlaceKind::Cube, 0, true, Vec3::ZERO);
    let d_after_place = dump_world_sorted(&world);

    begin_drag(
        &mut world,
        Vec3::new(0.0, 5.0, 0.0),
        Vec3::new(0.0, -1.0, 0.0),
    )
    .expect("拾取并进入拖拽");
    assert_eq!(undo_depth(&world), 2, "按下即拍（摆放 + 拖拽按下）");
    drag_to(
        &mut world,
        Vec3::new(2.3, 5.0, -1.7),
        Vec3::new(0.0, -1.0, 0.0),
    )
    .expect("拖动");
    drag_to(
        &mut world,
        Vec3::new(5.0, 5.0, 5.0),
        Vec3::new(0.0, -1.0, 0.0),
    )
    .expect("再拖");
    assert_eq!(undo_depth(&world), 2, "拖动中途不拍");
    end_drag(&mut world);
    assert_ne!(
        dump_world_sorted(&world),
        d_after_place,
        "拖拽确实移动了实体"
    );
    assert!(undo(&mut world));
    assert_eq!(
        dump_world_sorted(&world),
        d_after_place,
        "一步 undo 回到按下前（实体回原位，编号已换）"
    );
}

/// 无编辑器状态时新命令照常 inert（不 panic、不产生实体）。
#[test]
fn b_stage_commands_are_inert_without_editor_state() {
    let mut world = World::new();
    assert!(!undo(&mut world));
    assert!(!redo(&mut world));
    assert_eq!(undo_depth(&world), 0);
    assert_eq!(redo_depth(&world), 0);
    assert_eq!(
        bake_and_install(&mut world),
        None,
        "无状态 = 无档路径，不烘焙"
    );
    assert_eq!(toggle_nav_viz(&mut world), None);
    assert_eq!(rebuild_nav(&mut world), None);
    assert_eq!(focus_selected(&mut world), None);
    assert_eq!(light_edit_mode(&world), edit::LightEditMode::Inert);
    assert_eq!(light_intensity_step(&mut world, 1.0), None);
    assert_eq!(light_range_step(&mut world, -1.0), None);
    assert_eq!(light_hue_step(&mut world), None);
    assert_eq!(light_shadow_toggle(&mut world), None);
    assert_eq!(sun_ambient_step(&mut world, 1.0), None);
    assert_eq!(sun_direction_step(&mut world, 0.1, 0.1), None);
    orbit_camera(&mut world, (30.0, 30.0));
    assert_eq!(world.entity_count(), 0, "新命令在无状态世界不产生任何实体");
}

// ---------------------------------------------------------------------------
// R58b：光源参数编辑（位模式精确）
// ---------------------------------------------------------------------------

/// 点光四键：强度 / 范围 / 色相 / 阴影全部入 undo 栈；逐步 undo 的每一步
/// dump 与该步编辑前逐字符一致，全部 undo 后点光参数逐位复原。
#[test]
fn point_light_edits_undo_bit_exact() {
    let dir = temp_dir("lightedit");
    let mut world = World::new();
    open_level(&mut world, &dir.join("light.level.json")).expect("新建");
    let light = edit::place_light_at(&mut world, Vec3::new(1.0, 0.0, 1.0)).expect("点光");
    assert_eq!(
        world.resource::<EditorState>().selected,
        Some(light),
        "摆放后自动选中 ⇒ 点光模式"
    );
    assert_eq!(light_edit_mode(&world), edit::LightEditMode::PointLight);
    let d_before = dump_world_sorted(&world);

    // `]`：强度 ×1.25（6.0 × 1.25 = 7.5 精确）
    let intensity = light_intensity_step(&mut world, 1.0).expect("点光模式");
    assert_eq!(
        intensity.to_bits(),
        (6.0f32 * LIGHT_INTENSITY_STEP).to_bits()
    );
    let d_intensity = dump_world_sorted(&world);
    // `-`：范围 −0.5（10 − 0.5 = 9.5 精确）
    let range = light_range_step(&mut world, -1.0).expect("点光模式");
    assert_eq!(range.to_bits(), (10.0f32 - LIGHT_RANGE_STEP).to_bits());
    let d_range = dump_world_sorted(&world);
    // `H`：色相 +30°（饱和度 / 明度保持）
    let color = light_hue_step(&mut world).expect("点光模式");
    let (_, s_rot, v_rot) = edit::rgb_to_hsv(color);
    let (_, s0, v0) = edit::rgb_to_hsv(Vec3::from(edit::LIGHT_COLOR));
    assert!((s_rot - s0).abs() < 1e-5, "饱和度保持");
    assert!((v_rot - v0).abs() < 1e-6, "明度保持");
    let d_hue = dump_world_sorted(&world);
    // `K`：阴影开
    assert_eq!(light_shadow_toggle(&mut world), Some(true));
    let d_shadow = dump_world_sorted(&world);
    assert_eq!(undo_depth(&world), 5, "摆放 + 四步编辑 = 五个快照");

    // 逐步 undo：每步回到该步编辑前
    assert!(undo(&mut world));
    assert_eq!(dump_world_sorted(&world), d_hue, "undo 阴影");
    assert!(undo(&mut world));
    assert_eq!(dump_world_sorted(&world), d_range, "undo 色相");
    assert!(undo(&mut world));
    assert_eq!(dump_world_sorted(&world), d_intensity, "undo 范围");
    assert!(undo(&mut world));
    assert_eq!(
        dump_world_sorted(&world),
        d_before,
        "undo 强度：回到摆放时点"
    );
    let light_restored = world.entity_named("light/1").expect("undo 后按名找点光");
    let restored = *world.get::<PointLight>(light_restored).expect("在位");
    assert_eq!(
        restored.intensity.to_bits(),
        6.0f32.to_bits(),
        "强度逐位复原"
    );
    assert_eq!(
        restored.color.to_array().map(|v| v.to_bits()),
        Vec3::from(edit::LIGHT_COLOR)
            .to_array()
            .map(|v| v.to_bits()),
        "颜色逐位复原"
    );
    assert!(!restored.shadows, "阴影复原");
    let _ = d_shadow;
}

/// 方向光五键：强度 / 环境光 / 偏航俯仰全部入 undo 栈，undo 后资源位模式
/// 逐位复原（to_bits 对照）。
#[test]
fn directional_light_edits_undo_bit_exact() {
    let dir = temp_dir("sunedit");
    let mut world = World::new();
    open_level(&mut world, &dir.join("sun.level.json")).expect("新建");
    select(&mut world, None);
    assert_eq!(light_edit_mode(&world), edit::LightEditMode::Directional);
    let before = world.resource::<DirectionalLight>().clone();

    let intensity = light_intensity_step(&mut world, 1.0).expect("方向光模式");
    assert!(intensity > before.intensity);
    let ambient = sun_ambient_step(&mut world, 1.0).expect("方向光模式");
    assert!(ambient.x > before.ambient.x);
    let yaw = sun_direction_step(&mut world, edit::SUN_DIR_STEP, 0.0).expect("方向光模式");
    let pitch = sun_direction_step(&mut world, 0.0, -edit::SUN_DIR_STEP).expect("方向光模式");
    assert!(yaw.x.is_finite() && pitch.is_finite());
    assert_eq!(undo_depth(&world), 4, "四步编辑 = 四个快照");

    let after = world.resource::<DirectionalLight>().clone();
    assert_ne!(
        after.intensity.to_bits(),
        before.intensity.to_bits(),
        "强度位模式已变"
    );
    assert_ne!(
        after.ambient.to_array().map(|v| v.to_bits()),
        before.ambient.to_array().map(|v| v.to_bits())
    );
    assert_ne!(
        after.direction.to_array().map(|v| v.to_bits()),
        before.direction.to_array().map(|v| v.to_bits())
    );

    // 逐位复原
    assert!(undo(&mut world));
    assert!(undo(&mut world));
    assert!(undo(&mut world));
    assert!(undo(&mut world));
    let restored = world.resource::<DirectionalLight>().clone();
    assert_eq!(
        restored.intensity.to_bits(),
        before.intensity.to_bits(),
        "强度逐位"
    );
    assert_eq!(
        restored.ambient.to_array().map(|v| v.to_bits()),
        before.ambient.to_array().map(|v| v.to_bits()),
        "环境光逐位"
    );
    assert_eq!(
        restored.direction.to_array().map(|v| v.to_bits()),
        before.direction.to_array().map(|v| v.to_bits()),
        "方向逐位"
    );
    assert_eq!(restored.color, before.color, "未编辑字段不受影响");
}

/// 模式裁决与 Inert：非灯光实体选中时光源键 no-op（方向光不被触碰）。
#[test]
fn light_edit_mode_dispatch() {
    let dir = temp_dir("lightmode");
    let mut world = World::new();
    open_level(&mut world, &dir.join("mode.level.json")).expect("新建");
    // 未选中 = 方向光
    assert_eq!(light_edit_mode(&world), edit::LightEditMode::Directional);
    let sun_before = world.resource::<DirectionalLight>().clone();
    // 选中立方体 = Inert：光源键全部 no-op 且方向光不动
    let cube = place_with(&mut world, PlaceKind::Cube, 0, false, Vec3::ZERO);
    assert_eq!(light_edit_mode(&world), edit::LightEditMode::Inert);
    assert_eq!(light_intensity_step(&mut world, 1.0), None);
    assert_eq!(light_range_step(&mut world, 1.0), None);
    assert_eq!(light_hue_step(&mut world), None);
    assert_eq!(light_shadow_toggle(&mut world), None);
    assert_eq!(sun_ambient_step(&mut world, 1.0), None);
    assert_eq!(sun_direction_step(&mut world, 0.1, 0.1), None);
    assert_eq!(
        world.resource::<DirectionalLight>().intensity.to_bits(),
        sun_before.intensity.to_bits(),
        "Inert 模式方向光不被触碰"
    );
    // 选回点光 = PointLight
    let light = edit::place_light_at(&mut world, Vec3::new(2.0, 0.0, 0.0)).expect("点光");
    assert_eq!(light_edit_mode(&world), edit::LightEditMode::PointLight);
    select(&mut world, Some(cube));
    assert_eq!(light_edit_mode(&world), edit::LightEditMode::Inert);
    select(&mut world, Some(light));
    assert_eq!(light_edit_mode(&world), edit::LightEditMode::PointLight);
}

// ---------------------------------------------------------------------------
// R58b：烘焙入口
// ---------------------------------------------------------------------------

/// 烘焙闭环（复用 R57 闭环测试技术）：带 `level.bake` 段的档 → 烘焙 →
/// 产物落盘 → 挂载（UV2 副本 + LightmapBinding）→ undo 栈被清空 →
/// 烘焙后摆放的同图元实体无图集绑定 → save 写原始 mesh 段 + artifact
/// 引用 → 新世界重载重挂（句柄可复现、世界 dump 逐字符一致）。
#[test]
fn bake_closed_loop_roundtrip() {
    let dir = temp_dir("bake");
    let path = dir.join("bake.level.json");
    std::fs::write(
        &path,
        r#"{
  "version": 2,
  "level": { "bake": { "sky_samples": 4, "bounces": 0, "texels_per_meter": 4.0,
                        "atlas_size": 32, "artifact": "baked.ilmap" } },
  "entities": []
}"#,
    )
    .expect("写测试档");

    let mut world = World::new();
    open_level(&mut world, &path).expect("新建（档存在）");
    place_with(&mut world, PlaceKind::Cube, 0, true, Vec3::ZERO);
    assert_eq!(undo_depth(&world), 1, "摆放已入 undo 栈");

    let report = bake_and_install(&mut world).expect("烘焙成功");
    assert_eq!(report.entities, 1, "一个烘焙实体");
    assert!(report.texels > 0, "立方体表面应产出覆盖 texel");
    assert!(report.from_level_section, "参数来自档内 bake 段");
    assert_eq!(
        report.artifact,
        dir.join("baked.ilmap"),
        "产物按档内引用名落在档目录"
    );
    assert!(report.artifact.exists(), "产物已落盘");
    assert_eq!(
        undo_depth(&world),
        0,
        "烘焙清空 undo 栈（跨烘焙 undo 会截掉在用 UV2 句柄）"
    );

    let baked_entity = editable_entities(&world)[0];
    assert!(
        world.get::<LightmapBinding>(baked_entity).is_some(),
        "烘焙实体应挂 LightmapBinding"
    );
    let uv2_handle = *world.get::<MeshHandle>(baked_entity).expect("在位");
    let has_uv2 = world
        .resource::<MeshStore>()
        .get(uv2_handle)
        .map(|mesh| mesh.uv2.is_some())
        .unwrap_or(false);
    assert!(has_uv2, "网格应已换 UV2 副本");
    assert!(
        matches!(
            world
                .resource::<SceneAssetRegistry>()
                .meshes
                .get(&uv2_handle.0),
            Some(MeshRef::Primitive(_))
        ),
        "登记表回补：UV2 副本句柄继承图元来源"
    );
    // 烘焙后继续摆放（同图元段 ⇒ 句柄缓存命中，不新增 Store 条目——
    // 重载侧 Store 序才可复现）：新实体无图集绑定（引擎既有混合语义：
    // 无 UV2 的实体走 IBL）
    place_with(
        &mut world,
        PlaceKind::Cube,
        0,
        true,
        Vec3::new(4.0, 0.0, 0.0),
    );
    let later = editable_entities(&world)[1];
    assert!(
        world.get::<LightmapBinding>(later).is_none(),
        "烘焙后摆放的实体不入图集"
    );
    assert_eq!(undo_depth(&world), 1, "烘焙后的摆放重新入栈");
    let d_after_bake = dump_world(&world);

    // save：mesh 段仍还原为原始图元（登记表回补生效）+ artifact 引用回写
    save_level(&mut world).expect("烘焙后保存");
    let json = std::fs::read_to_string(&path).expect("档已写出");
    assert_eq!(
        json.matches("\"primitive\"").count(),
        2,
        "两个实体的 mesh 段都是图元来源"
    );
    assert!(json.contains("baked.ilmap"), "artifact 引用原样回写");

    // 新世界重载：产物重挂、UV2 副本句柄可复现、后摆实体无绑定、
    // 世界 dump 与烘焙保存时的世界逐字符一致
    let mut reloaded = World::new();
    open_level(&mut reloaded, &path).expect("重载");
    assert_eq!(editable_entities(&reloaded).len(), 2);
    let first = editable_entities(&reloaded)[0];
    let second = editable_entities(&reloaded)[1];
    assert!(
        reloaded.get::<LightmapBinding>(first).is_some(),
        "产物重挂到烘焙实体"
    );
    assert!(reloaded.get::<LightmapBinding>(second).is_none());
    let handle_b = *reloaded.get::<MeshHandle>(first).unwrap();
    assert_eq!(handle_b, uv2_handle, "UV2 副本句柄可复现（同序加载）");
    assert_eq!(
        dump_world(&reloaded),
        d_after_bake,
        "烘焙保存的世界与重载世界逐字符一致"
    );
}

/// 无 bake 段的档：默认参数 + 档同目录 `*.ilm` 默认名，状态行提示；
/// `level` 段保持只读语义不被写入（诚实限制：重载不自动挂载）。
#[test]
fn bake_without_section_uses_default_artifact_name() {
    let dir = temp_dir("bakedefault");
    let path = dir.join("no_bake_section.level.json");

    let mut world = World::new();
    open_level(&mut world, &path).expect("新建（无 level 段）");
    place_with(&mut world, PlaceKind::Cube, 0, true, Vec3::ZERO);

    let report = bake_and_install(&mut world).expect("默认参数烘焙");
    assert!(!report.from_level_section, "参数 = 引擎默认");
    let expected = dir.join("no_bake_section.level.ilm");
    assert_eq!(report.artifact, expected, "默认名 = 档同目录 *.ilm");
    assert!(expected.exists());
    assert!(
        world.resource::<EditorState>().status.contains("ilm"),
        "状态行提示产物名：{}",
        world.resource::<EditorState>().status
    );
    assert_eq!(undo_depth(&world), 0, "烘焙清栈");

    save_level(&mut world).expect("保存");
    let json = std::fs::read_to_string(&path).expect("档已写出");
    assert!(
        !json.contains("\"bake\""),
        "level 段只读语义：无 bake 段则保存仍无 bake 段（参数编辑在拒绝清单内）"
    );
}

// ---------------------------------------------------------------------------
// R58b：NavGrid 重建与可视化
// ---------------------------------------------------------------------------

/// 导航重建 + 可视化：自动 bounds 推导（与纯函数同源对照）、blocked 统计
/// 与 `is_blocked` 逐格一致、可视化实体挂 EditorTransient（不入档、不可
/// 拾取）、开关语义（每次开都重建；关只撤可视化、NavGrid 资源保留）。
#[test]
fn nav_rebuild_and_viz_end_to_end() {
    let dir = temp_dir("nav");
    let mut world = World::new();
    open_level(&mut world, &dir.join("nav.level.json")).expect("新建");
    // 两个静态盒：自动 bounds = AABB 并集
    place_with(&mut world, PlaceKind::Cube, 0, true, Vec3::ZERO);
    place_with(
        &mut world,
        PlaceKind::Cube,
        0,
        true,
        Vec3::new(4.0, 0.0, 0.0),
    );
    // 清空选择：摆放自动选中会挂 12 条选中框边（瞬态），下面按
    // grid_line_count() 基线数红片，先归零
    select(&mut world, None);
    assert_eq!(
        transient_count(&world),
        grid_line_count(),
        "基线 = 仅网格线"
    );

    // 期望 bounds：与纯函数同一输入（两盒的 (position, shape)）
    let expected = nav_bounds_from_statics(&[
        (
            Vec3::new(0.0, 0.5, 0.0),
            ColliderShape::Box {
                half_extents: Vec3::splat(0.5),
            },
        ),
        (
            Vec3::new(4.0, 0.5, 0.0),
            ColliderShape::Box {
                half_extents: Vec3::splat(0.5),
            },
        ),
    ])
    .expect("有静态体");

    let report = toggle_nav_viz(&mut world).expect("开 = 重建成功");
    assert!(!report.from_level_section, "无 nav 段 = 自动推导");
    // 只读阶段：全部 NavGrid 查询收在一个块里（资源借用不与后续写操作重叠）
    let blocked_center = {
        let grid = world.resource::<NavGrid>();
        assert_eq!(grid.params().bounds_min, expected.0, "自动 bounds min 同源");
        assert_eq!(grid.params().bounds_max, expected.1, "自动 bounds max 同源");
        assert_eq!(grid.cols(), 10, "X 跨 5 m / cell 0.5 = 10 列");
        assert_eq!(grid.rows(), 2, "Z 跨 1 m / cell 0.5 = 2 行");
        assert_eq!(report.cells, grid.cols() * grid.rows());
        assert_eq!(
            report.blocked,
            nav_blocked_count(&grid),
            "报告与 is_blocked 逐格统计一致"
        );
        assert!(report.blocked > 0, "两个实体盒应阻塞若干格");
        assert_eq!(
            report.size,
            [
                grid.cols() as f32 * grid.params().cell_size,
                grid.rows() as f32 * grid.params().cell_size
            ]
        );
        let mut center = None;
        for row in 0..grid.rows() {
            for col in 0..grid.cols() {
                if grid.is_blocked(col, row) {
                    center = Some(grid.cell_center(col, row));
                }
            }
        }
        center.expect("有 blocked 格")
    };

    // 可视化实体：blocked 红片 + 4 条边框，全部挂 EditorTransient
    let viz_count = transient_count(&world) - grid_line_count();
    assert_eq!(
        viz_count,
        report.blocked + 4,
        "红片数 = blocked 数，另有 4 条边框"
    );
    let mut viz_names = 0usize;
    world.query::<(Entity, &Name)>(|(_, name)| {
        if name.0.starts_with("navviz/") {
            viz_names += 1;
        }
    });
    assert_eq!(viz_names, viz_count, "可视化实体都以 navviz/ 命名");
    // blocked 格中心上方拾取：命中（若有）只能是可编辑实体——navviz
    // 红片挂 EditorTransient，从不参与拾取（editable_entities 排除）
    if let Some(hit) = pick(
        &world,
        blocked_center + Vec3::new(0.0, 5.0, 0.0),
        Vec3::NEG_Y,
    ) {
        let hit_name = world
            .get::<Name>(hit)
            .map(|n| n.0.clone())
            .unwrap_or_default();
        assert!(
            !hit_name.starts_with("navviz/"),
            "瞬态红片不可被拾取，实际命中 {hit_name}"
        );
    }
    // 直接命中红片位置的射线若穿过它之前先命中可编辑体，也证明同一点；
    // 再补一条必然不穿任何可编辑体的射线：纯红片区域 = None
    assert_eq!(
        pick(
            &world,
            Vec3::new(9.75, edit::NAV_BORDER_Y + 5.0, 0.25),
            Vec3::NEG_Y
        ),
        None,
        "红片本身（若在射线域内）不产生命中——此处无可编辑实体"
    );

    // 存档：可视化不入档；保存后可视化照旧在位
    save_level(&mut world).expect("保存");
    let json = std::fs::read_to_string(dir.join("nav.level.json")).expect("档已写出");
    assert!(!json.contains("navviz/"), "导航可视化不入档");
    assert_eq!(
        transient_count(&world) - grid_line_count(),
        viz_count,
        "保存后可视化重建"
    );

    // 关：撤可视化但 NavGrid 资源保留
    assert_eq!(toggle_nav_viz(&mut world), None, "关返回 None");
    assert_eq!(transient_count(&world), grid_line_count(), "可视化撤掉");
    assert!(
        world.try_resource::<NavGrid>().is_some(),
        "NavGrid 资源保留"
    );

    // 再开 = 重建（确定性 ⇒ 报告与第一次相同）
    let again = toggle_nav_viz(&mut world).expect("重开");
    assert_eq!(again, report, "同世界重建结果确定");
}

/// 档内 `level.nav` 段优先：参数取段值而非自动推导。
#[test]
fn nav_section_params_take_priority() {
    let dir = temp_dir("navsection");
    let path = dir.join("nav_section.level.json");
    std::fs::write(
        &path,
        r#"{
  "version": 2,
  "level": { "nav": { "bounds_min": [-2, 0, -2], "bounds_max": [2, 0, 2],
                       "cell_size": 1.0 } },
  "entities": []
}"#,
    )
    .expect("写测试档");
    let mut world = World::new();
    open_level(&mut world, &path).expect("直读");
    let report = toggle_nav_viz(&mut world).expect("段参数合法");
    assert!(report.from_level_section, "参数来自档内 nav 段");
    let grid = world.resource::<NavGrid>();
    assert_eq!(grid.params().cell_size, 1.0);
    assert_eq!(grid.params().bounds_min.x, -2.0);
    assert_eq!(grid.params().bounds_max.x, 2.0);
    assert_eq!(grid.cols(), 4, "ceil(4 / 1.0) = 4 列");
    assert_eq!(grid.rows(), 4);
    assert_eq!(report.cells, 16);
}

// ---------------------------------------------------------------------------
// R58b：相机轨道与聚焦
// ---------------------------------------------------------------------------

/// 中键轨道：eye 绕 target 旋转、target 不动、半径保持；`F` 聚焦 =
/// target 直设选中实体世界 AABB 中心（eye 不动）。
#[test]
fn orbit_camera_and_focus_selected() {
    let dir = temp_dir("orbit");
    let mut world = World::new();
    open_level(&mut world, &dir.join("orbit.level.json")).expect("新建");
    let (eye0, target0) = {
        let camera = world.resource::<Camera>();
        (camera.eye, camera.target)
    };

    orbit_camera(&mut world, (90.0, 0.0)); // 拖右 90 px = 偏航 +0.45 rad
    {
        let camera = world.resource::<Camera>();
        assert_eq!(camera.target, target0, "target 不动");
        assert_ne!(camera.eye, eye0, "eye 已绕转");
        let radius = (camera.eye - camera.target).length();
        let radius0 = (eye0 - target0).length();
        assert!(
            (radius - radius0).abs() < 1e-4 * radius0,
            "半径保持：{radius} vs {radius0}"
        );
    }
    // 垂直拖拽：屏幕 y 向下 ⇒ 拖上（dy < 0）= 俯仰 +；大幅拖拽被钳制
    orbit_camera(&mut world, (0.0, -100000.0));
    {
        let camera = world.resource::<Camera>();
        let offset = camera.eye - camera.target;
        let pitch = (offset.y / offset.length()).asin();
        assert!(
            (pitch - edit::ORBIT_PITCH_LIMIT).abs() < 1e-5,
            "俯仰钳制 +89°：{}",
            pitch.to_degrees()
        );
    }

    // F 聚焦：target = 选中实体世界 AABB 中心，eye 不动
    let placed = place_with(
        &mut world,
        PlaceKind::Cube,
        0,
        true,
        Vec3::new(2.0, 0.0, 0.0),
    );
    let eye_before = world.resource::<Camera>().eye;
    let (min, max) = world_aabb_of(&world, placed).expect("立方体有网格");
    let center = focus_selected(&mut world).expect("有选中");
    assert_eq!(center, (min + max) * 0.5);
    assert_eq!(center, Vec3::new(2.0, 0.5, 0.0));
    assert_eq!(world.resource::<Camera>().target, center, "target 直设中心");
    assert_eq!(world.resource::<Camera>().eye, eye_before, "eye 不动");
    select(&mut world, None);
    assert_eq!(focus_selected(&mut world), None, "无选中 = no-op");
}

// ---------------------------------------------------------------------------
// R89：DDGI 编辑入口（命令层 + 可视化 + 入档）
// ---------------------------------------------------------------------------

/// DDGI 步进命令：全部入 undo 栈（含「启用 DDGI」这一步本身），逐步 undo
/// 的 dump 与该步编辑前逐字符一致（`ddgi` 资源行在 dump 里逐字符对照）。
#[test]
fn ddgi_edits_undo_bit_exact() {
    let dir = temp_dir("ddgiedit");
    let mut world = World::new();
    open_level(&mut world, &dir.join("ddgi.level.json")).expect("新建");
    // 摆放前的世界基线（undo 栈底快照的对照：Store 缓存 / 方向光在位，
    // 只是无可编辑实体与 DDGI 资源）
    let d_initial = dump_world_sorted(&world);
    place_with(
        &mut world,
        PlaceKind::Cube,
        0,
        false,
        Vec3::new(2.0, 0.0, 2.0),
    );
    propagate_transforms(&mut world);
    assert_eq!(ddgi_edit_mode(&world), DdgiEditMode::Inert);
    assert!(
        !world.contains_resource::<DdgiSettings>(),
        "新建世界无 DdgiSettings（缺席 = DDGI 关）"
    );

    // `X` 循环到 Grid（模式切换是输入路由状态，不入 undo 栈）
    assert_eq!(ddgi_mode_cycle(&mut world), DdgiEditMode::Grid);

    // `,`：资源缺席 ⇒ 插入默认资源 + spacing ÷1.25（2.0 → 1.6）
    let spacing = ddgi_spacing_step(&mut world, -1).expect("命令层不做模式门");
    assert_eq!(
        spacing.to_bits(),
        (2.0f32 / 1.25).to_bits(),
        "默认 2.0 ÷ 1.25 = 1.6"
    );
    assert!(
        world.contains_resource::<DdgiSettings>(),
        "第一条命令 = 启用"
    );
    let d_enable = dump_world_sorted(&world);
    // `.`：×1.25（1.6 → 2.0）
    let spacing2 = ddgi_spacing_step(&mut world, 1).expect("命令层不做模式门");
    assert_eq!(spacing2.to_bits(), 2.0f32.to_bits());
    let d_spacing = dump_world_sorted(&world);
    // bounds 物化 + 平移（`,.` 系列之外的命令形态）
    let pair =
        ddgi_bounds_translate(&mut world, edit::DdgiAxis::Y, 1).expect("有几何 ⇒ 物化 + 平移成功");
    assert!(pair.1.y > pair.0.y, "平移不改变 span：{pair:?}");
    let d_bounds = dump_world_sorted(&world);
    // 预算组：k / rays / hysteresis
    let k = ddgi_probes_per_frame_step(&mut world, 1).expect("k 步进");
    assert_eq!(k, 3, "默认 k = 2 + 1");
    let d_k = dump_world_sorted(&world);
    let rays = ddgi_rays_face_step(&mut world, 1).expect("射线档");
    assert_eq!(rays, 32, "默认 16 → 下一档 32");
    let d_rays = dump_world_sorted(&world);
    let hyst = ddgi_hysteresis_step(&mut world, 1).expect("hysteresis");
    assert_eq!(hyst.to_bits(), (0.1f32).to_bits(), "默认 0 + 0.1");
    assert_eq!(undo_depth(&world), 7, "摆放 + 六步 DDGI 编辑 = 七个快照");

    // 逐步 undo：恢复的是「该步之前」的快照（快照序 = 编辑序）
    assert!(undo(&mut world));
    assert_eq!(
        dump_world_sorted(&world),
        d_rays,
        "undo hysteresis ⇒ rays 后"
    );
    assert!(undo(&mut world));
    assert_eq!(dump_world_sorted(&world), d_k, "undo rays ⇒ k 后");
    assert!(undo(&mut world));
    assert_eq!(dump_world_sorted(&world), d_bounds, "undo k ⇒ bounds 后");
    assert!(undo(&mut world));
    assert_eq!(
        dump_world_sorted(&world),
        d_spacing,
        "undo bounds ⇒ ×1.25 后"
    );
    assert!(undo(&mut world));
    assert_eq!(
        dump_world_sorted(&world),
        d_enable,
        "undo ×1.25 ⇒ 启用后（spacing = 1.6）"
    );
    assert!(undo(&mut world));
    assert!(
        !world.contains_resource::<DdgiSettings>(),
        "undo ÷1.25 ⇒ 撤掉「启用 DDGI」（启用本身可撤销——撤销栈谎言反例的钉子）"
    );
    assert!(undo(&mut world));
    assert_eq!(
        dump_world_sorted(&world),
        d_initial,
        "undo 摆放 ⇒ 回到摆放前的世界（摆放在启用之前，栈深 7 全部对账）"
    );
}

/// 模式裁决 + 无 `EditorState` ⇒ no-op 契约（`commands_are_inert_…` 的
/// DDGI 面）：只读查询返回 `Inert`、写操作全 no-op、toggle 返回 `false`。
#[test]
fn ddgi_commands_are_inert_without_editor_state() {
    let mut world = World::new();
    assert_eq!(ddgi_edit_mode(&world), DdgiEditMode::Inert);
    assert_eq!(ddgi_mode_cycle(&mut world), DdgiEditMode::Inert);
    assert_eq!(ddgi_spacing_step(&mut world, 1), None);
    assert_eq!(
        ddgi_bounds_translate(&mut world, edit::DdgiAxis::X, 1),
        None
    );
    assert_eq!(ddgi_probes_per_frame_step(&mut world, 1), None);
    assert_eq!(ddgi_rays_face_step(&mut world, 1), None);
    assert_eq!(ddgi_hysteresis_step(&mut world, 1), None);
    assert!(!ddgi_overlay_toggle(&mut world), "toggle 无状态 = false");
    assert!(
        !world.contains_resource::<DdgiSettings>(),
        "no-op 不插入资源"
    );
}

/// 参数步进 ⇒ `grid_fingerprint` 变 ⇒ 引擎 `tick_ddgi` 重规划网格：
/// `structure_generation` 前进 + 全表重填（probe_stamp 全部刷新）——
/// 编辑器零额外接线的重规划语义（设计稿 C1）钉在这里。
#[test]
fn ddgi_spacing_step_replans_the_grid() {
    let dir = temp_dir("ddgiplan");
    let mut world = World::new();
    open_level(&mut world, &dir.join("ddgiplan.level.json")).expect("新建");
    place_with(
        &mut world,
        PlaceKind::Cube,
        0,
        false,
        Vec3::new(2.0, 0.0, 2.0),
    );
    propagate_transforms(&mut world);
    ddgi_mode_cycle(&mut world);
    ddgi_spacing_step(&mut world, 1).expect("启用（默认 2.0 × 1.25 = 2.5）");
    // headless 世界无 DdgiPlugin ⇒ 手动插空网格（同上）
    world.insert_resource(DdgiGrid::new());
    iris_engine::ddgi::tick_ddgi(&world);
    let (gen0, stamps0, probes) = world
        .try_resource::<DdgiGrid>()
        .map(|g| {
            (
                g.structure_generation(),
                g.probe_stamp().to_vec(),
                g.probe_count(),
            )
        })
        .unwrap();
    assert!(gen0 >= 1, "首帧装配 ⇒ 结构版本 ≥ 1");
    assert!(probes > 0, "立方体在 ⇒ 网格非空");

    // spacing 变 ⇒ fingerprint 变 ⇒ 下一帧 tick 重建
    ddgi_spacing_step(&mut world, 1).expect("×1.25");
    iris_engine::ddgi::tick_ddgi(&world);
    let (gen1, stamps1) = world
        .try_resource::<DdgiGrid>()
        .map(|g| (g.structure_generation(), g.probe_stamp().to_vec()))
        .unwrap();
    assert_eq!(
        gen1,
        gen0 + 1,
        "spacing 变 ⇒ structure_generation 前进（渲染器全量上传的触发键）"
    );
    assert_ne!(stamps0, stamps1, "重建 ⇒ 全表重填（stamp 刷新到新 tick）");
}

/// DDGI 叠加层：不入档（`transient_visuals_never_reach_the_file` 同款）+
/// `BakeExcluded` 让叠加层不进 GI 三角收集（桶 C0 的编辑器面）+ `level.ddgi`
/// 段照常落盘（参数与视觉是两回事）。
#[test]
fn ddgi_overlay_never_reaches_the_file_nor_the_bake() {
    let dir = temp_dir("ddgiviz");
    let path = dir.join("ddgiviz.level.json");
    let mut world = World::new();
    open_level(&mut world, &path).expect("新建");
    place_with(
        &mut world,
        PlaceKind::Cube,
        0,
        false,
        Vec3::new(2.0, 0.0, 2.0),
    );
    propagate_transforms(&mut world);
    ddgi_mode_cycle(&mut world);
    ddgi_spacing_step(&mut world, 1).expect("启用");
    // headless 世界无 DdgiPlugin ⇒ 手动插空网格（DdgiPlugin 只做这件事）
    world.insert_resource(DdgiGrid::new());
    assert!(ddgi_overlay_toggle(&mut world), "开叠加层");
    let balls = world
        .try_resource::<DdgiGrid>()
        .map(|grid| grid.probe_count())
        .unwrap();
    assert!(balls > 0, "有几何 ⇒ 有探针 ⇒ 有标记球");
    // 每探针一个标记球（按名计数——query 无返回值，手数）
    let mut viz_named = 0usize;
    world.query::<&Name>(|name| {
        if name.0.starts_with("ddgiviz/") {
            viz_named += 1;
        }
    });
    assert_eq!(viz_named, balls, "每探针一个标记球");

    // 收集排除：叠加层在位的静态几何收集 == 叠加层撤除后（探针球不进 BVH）
    let tri_with_viz = collect_bake_triangles(&world).len();
    assert_eq!(save_level(&mut world).expect("保存"), 1, "只有立方体入档");
    let mut reloaded = World::new();
    open_level(&mut reloaded, &path).expect("重载");
    propagate_transforms(&mut reloaded);
    let mut reloaded_viz = 0usize;
    reloaded.query::<&Name>(|name| {
        if name.0.starts_with("ddgiviz/") {
            reloaded_viz += 1;
        }
    });
    assert_eq!(reloaded_viz, 0, "叠加层不入档（重载后无 ddgiviz 实体）");
    let tri_after = collect_bake_triangles(&reloaded).len();
    assert_eq!(
        tri_with_viz, tri_after,
        "叠加层在位的收集 == 无叠加层的收集（BakeExcluded 生效：{tri_with_viz} vs {tri_after}）"
    );
    // DDGI 参数段照常入档（参数与视觉是两回事）
    let text = std::fs::read_to_string(&path).expect("读档");
    assert!(text.contains("\"ddgi\""), "level.ddgi 段必须落盘");
}

/// `level.ddgi` 段端到端：步进参数 → 保存 → 重载 ⇒ `DdgiSettings` 资源
/// 复原（位模式级）；无 DDGI 的档保存两次逐字节一致且不含 ddgi 段。
#[test]
fn ddgi_section_survives_editor_roundtrip() {
    let dir = temp_dir("ddgirt");
    let path = dir.join("ddgirt.level.json");
    let mut world = World::new();
    open_level(&mut world, &path).expect("新建");
    save_level(&mut world).expect("保存无 DDGI 档");
    let text0 = std::fs::read_to_string(&path).expect("读");
    assert!(
        !text0.contains("\"ddgi\""),
        "无 DdgiSettings ⇒ 不写 ddgi 段（缺席 = 能力不启用）"
    );
    save_level(&mut world).expect("二次保存");
    let text1 = std::fs::read_to_string(&path).expect("读");
    assert_eq!(text0, text1, "无 DDGI ⇒ 二次保存逐字节一致（既有口径复核）");

    // 步进出非默认参数 ⇒ 保存 ⇒ 重载复原
    ddgi_mode_cycle(&mut world);
    // 第一条步进命令既「启用」又「步进」：默认 2.0 × 1.25 = 2.5
    let spacing = ddgi_spacing_step(&mut world, 1).expect("启用 + 步进");
    assert_eq!(
        spacing.to_bits(),
        (2.0f32 * 1.25).to_bits(),
        "2.0 × 1.25 = 2.5"
    );
    let k = ddgi_probes_per_frame_step(&mut world, 1).expect("k = 3");
    assert_eq!(k, 3);
    save_level(&mut world).expect("保存带 DDGI 档");
    let text2 = std::fs::read_to_string(&path).expect("读");
    let mut reloaded = World::new();
    open_level(&mut reloaded, &path).expect("重载");
    {
        let settings = reloaded
            .try_resource::<DdgiSettings>()
            .expect("ddgi 段 ⇒ 资源在场");
        assert_eq!(
            settings.spacing.to_bits(),
            spacing.to_bits(),
            "spacing 位模式"
        );
        assert_eq!(settings.probes_per_frame, k, "k 复原");
    }
    // 重存逐字节（往返稳定）
    save_level(&mut reloaded).expect("重存");
    let text3 = std::fs::read_to_string(&path).expect("读");
    assert_eq!(text2, text3, "带 DDGI 档的重存逐字节一致（往返稳定）");
}

// ---------------------------------------------------------------------------
// C2：反射探针编辑入口（摆放 / 步进 / undo / 入档）
// ---------------------------------------------------------------------------

/// 探针入口全链：摆放（双写 scale = 2×half + BakeExcluded + 自动选中）→
/// 步进（size ×1.25 同写标记、falloff +0.5）→ 逐步 undo 复原组件与标记
/// （§8.37⑧ 闭合前探针在 undo 后丢失的钉子）→ save_level 的 probe 段
/// 往返 → 重存逐字节稳定。
#[test]
fn probe_place_step_undo_and_level_roundtrip() {
    let dir = temp_dir("probeedit");
    let path = dir.join("probe.level.json");
    let mut world = World::new();
    open_level(&mut world, &path).expect("新建");
    let d_initial = dump_world_sorted(&world);

    let probe = place_probe_at(&mut world, Vec3::new(0.0, 0.0, 0.0)).expect("摆放探针");
    propagate_transforms(&mut world);
    {
        let component = world.get::<ReflectionProbe>(probe).expect("组件在位");
        assert_eq!(
            component.half_extents,
            Vec3::splat(2.0),
            "摆放初值半边长 2 m"
        );
    }
    assert!(
        world.get::<BakeExcluded>(probe).is_some(),
        "标记立方不进 GI 收集"
    );
    assert_eq!(
        world.get::<Transform>(probe).unwrap().scale,
        Vec3::splat(4.0),
        "标记 scale = 2 × half_extents（双写契约）"
    );
    assert!(probe_selected(&world), "摆放后自动选中");

    let half = probe_size_step(&mut world, 1).expect("选中步进");
    assert_eq!(half, Vec3::splat(2.0 * 1.25), "×1.25 统一步进");
    assert_eq!(
        world.get::<Transform>(probe).unwrap().scale,
        half * 2.0,
        "步进必须同步标记 scale"
    );
    let d_size = dump_world_sorted(&world);
    let falloff = probe_falloff_step(&mut world, 1).expect("过渡带步进");
    assert_eq!(falloff.to_bits(), (PROBE_DEFAULT_FALLOFF + 0.5).to_bits());

    // 逐步 undo：falloff → size → 摆放 ⇒ 每步都复原组件（此前探针组件
    // 不在 undo 快照字段集 = §8.37⑧ 声明的丢失 bug）。**undo 全删全建 ⇒
    // 实体句柄改变**（§8.34② 既有语义），断言一律经查询取回而非旧句柄。
    assert!(undo(&mut world));
    let mut probe_state: Option<(f32, Vec3, Vec3)> = None;
    world.query::<(&ReflectionProbe, &Transform)>(|(p, t)| {
        probe_state = Some((p.falloff, p.half_extents, t.scale));
    });
    let (falloff, half_now, scale_now) = probe_state.expect("undo 后探针仍在");
    assert_eq!(
        falloff.to_bits(),
        PROBE_DEFAULT_FALLOFF.to_bits(),
        "undo falloff"
    );
    assert_eq!(half_now, Vec3::splat(2.5), "falloff undo 不动 size");
    assert_eq!(scale_now, half_now * 2.0, "标记与组件同点");
    assert!(undo(&mut world));
    let mut probe_state: Option<(Vec3, Vec3)> = None;
    world.query::<(&ReflectionProbe, &Transform)>(|(p, t)| {
        probe_state = Some((p.half_extents, t.scale));
    });
    let (half_now, scale_now) = probe_state.expect("探针仍在");
    assert_eq!(half_now, Vec3::splat(2.0), "undo size ⇒ 组件回摆放初值");
    assert_eq!(scale_now, Vec3::splat(4.0), "标记 scale 同步回退");
    let _ = d_size;
    assert!(undo(&mut world));
    assert_eq!(
        dump_world_sorted(&world),
        d_initial,
        "undo 摆放 ⇒ 世界复原（摆放命令捕获的正是摆放前快照）"
    );
    let mut seen = 0usize;
    world.query::<&ReflectionProbe>(|_| seen += 1);
    assert_eq!(
        seen, 0,
        "摆放 undo 后探针组件必须消失（快照字段集覆盖探针）"
    );

    // 入档往返：重放摆放 + 步进 ⇒ save_level 带 probe 段 ⇒ 重载组件在位、
    // 重存逐字节稳定
    let probe2 = place_probe_at(&mut world, Vec3::new(1.0, 0.0, -1.0)).expect("再摆");
    probe_size_step(&mut world, 1).expect("步进");
    let _ = probe2;
    save_level(&mut world).expect("保存");
    let text = std::fs::read_to_string(&path).expect("读");
    assert!(text.contains("\"probe\""), "probe 段必须入档");
    let mut reloaded = World::new();
    open_level(&mut reloaded, &path).expect("重载");
    let mut seen = 0usize;
    reloaded.query::<&ReflectionProbe>(|p| {
        assert_eq!(p.half_extents, Vec3::splat(2.5), "重载步进结果");
        seen += 1;
    });
    assert_eq!(seen, 1, "恰一个探针实体");
    save_level(&mut reloaded).expect("重存");
    assert_eq!(
        std::fs::read_to_string(&path).expect("重读"),
        text,
        "探针档往返逐字节稳定"
    );
}

/// 探针命令守卫：无编辑器状态 / 未选中 / 选中非探针实体 ⇒ 全部 no-op
/// （`None` / `false`），摆放命令需要 EditorState 缺席 no-op。
#[test]
fn probe_commands_are_inert_without_selection() {
    // 无 EditorState 裸世界
    let mut bare = World::new();
    assert!(!probe_selected(&bare));
    assert_eq!(probe_size_step(&mut bare, 1), None);
    assert_eq!(probe_falloff_step(&mut bare, -1), None);
    assert!(place_probe_at(&mut bare, Vec3::ZERO).is_none());

    // 编辑器世界但无选中 / 选中普通立方体
    let dir = temp_dir("probeinert");
    let mut world = World::new();
    open_level(&mut world, &dir.join("probeinert.level.json")).expect("新建");
    assert!(!probe_selected(&world));
    assert_eq!(probe_size_step(&mut world, 1), None);
    let cube = place_at(&mut world, Vec3::ZERO).expect("摆立方");
    select(&mut world, Some(cube));
    assert!(
        !probe_selected(&world),
        "选中普通实体 ⇒ 探针命令 no-op（[ ] - = 归各自语义）"
    );
    assert_eq!(probe_size_step(&mut world, 1), None);
}

/// 钳域：半边长连乘封顶 `PROBE_SIZE_MAX`、连除托底 `PROBE_SIZE_MIN`；
/// 过渡带封底 0（硬边界合法）封顶 10 m（R89 规约的非默认值断言面）。
#[test]
fn probe_steps_clamp_at_band_edges() {
    let dir = temp_dir("probeclamp");
    let mut world = World::new();
    open_level(&mut world, &dir.join("probeclamp.level.json")).expect("新建");
    place_probe_at(&mut world, Vec3::ZERO).expect("摆放");
    for _ in 0..60 {
        probe_size_step(&mut world, 1);
    }
    assert_eq!(
        probe_size_step(&mut world, 1),
        Some(Vec3::splat(100.0)),
        "半边长封顶 100 m"
    );
    for _ in 0..200 {
        probe_size_step(&mut world, -1);
    }
    assert_eq!(
        probe_size_step(&mut world, -1),
        Some(Vec3::splat(0.05)),
        "半边长托底 0.05 m（不退化零体积盒）"
    );
    for _ in 0..40 {
        probe_falloff_step(&mut world, -1);
    }
    assert_eq!(
        probe_falloff_step(&mut world, -1),
        Some(0.0),
        "过渡带封底 0 = 硬边界合法值"
    );
    for _ in 0..40 {
        probe_falloff_step(&mut world, 1);
    }
    assert_eq!(
        probe_falloff_step(&mut world, 1),
        Some(10.0),
        "过渡带封顶 10 m"
    );
}
