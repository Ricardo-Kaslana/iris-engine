//! 庭院关卡档（R57）验收测试：**往返逐位一致** + **档与 logic 常量的
//! 双源一致性守护**。
//!
//! - 往返：`assets/assault_courtyard.level.json` 经 load → save → load，
//!   第二次加载的世界与第一次**逐位一致**（实体表 / Transform / 组件
//!   位模式 / 三大 Store 内容 FNV / 登记表 / 方向光资源）——规格验收 ②；
//! - 守护：关卡档内的墙 / 柱 / 喷泉 / 灯光数值与 `crate::logic` 常量
//!   （纯逻辑核心的碰撞事实来源）逐项位级对照——「档改了、逻辑常量忘改」
//!   的漂移在本测试即爆（双源局面的诚实对策，见 plugin.rs 的
//!   `build_courtyard` 文档与 ARCHITECTURE §8）。
//!
//! 全程 headless（无 GPU / 无窗口）；临时文件用进程 id 唯一名防并行竞态
//! （AGENTS.md 测试规约）。

use glam::Vec3;

use iris_assault::logic;
use iris_assault::plugin::LEVEL_ENTRY;
use iris_core::prelude::{Entity, World};
use iris_engine::prelude::*;

/// 测试固定目录（唯一名防竞态）。
fn test_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("iris-assault-level-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("创建临时目录");
    dir
}

/// FNV-1a 64（与 iris-render 测试锁同款哈希）。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn bits(v: f32) -> u32 {
    v.to_bits()
}

fn bits3(v: Vec3) -> [u32; 3] {
    [bits(v.x), bits(v.y), bits(v.z)]
}

/// 世界静态内容的全量确定性 dump（实体表 + 三大 Store + 登记表 +
/// 方向光）：两个世界 dump 逐行相等 = 逐位一致。
fn dump_world(world: &World) -> Vec<String> {
    let mut out = Vec::new();
    out.push(format!("entity_count = {}", world.entity_count()));
    let mut entities: Vec<Entity> = world.entities().collect();
    entities.sort_by_key(|e| e.index());
    for e in entities {
        let name = world
            .get::<Name>(e)
            .map(|n| n.0.clone())
            .unwrap_or_else(|| "-".into());
        let mut line = format!("E {:04} name={name}", e.index());
        if let Some(t) = world.get::<Transform>(e) {
            line.push_str(&format!(
                " T p={:?} r={:?} s={:?}",
                bits3(t.position),
                [
                    bits(t.rotation.x),
                    bits(t.rotation.y),
                    bits(t.rotation.z),
                    bits(t.rotation.w)
                ],
                bits3(t.scale)
            ));
        }
        if let Some(v) = world.get::<Visibility>(e) {
            line.push_str(&format!(" vis={}", v.visible));
        }
        if let Some(m) = world.get::<MeshHandle>(e) {
            line.push_str(&format!(" mesh={}", m.0));
        }
        if let Some(m) = world.get::<MaterialHandle>(e) {
            line.push_str(&format!(" mat={}", m.0));
        }
        if let Some(c) = world.get::<Collider>(e) {
            let shape = match c.shape {
                ColliderShape::Sphere { radius } => format!("sphere({})", bits(radius)),
                ColliderShape::Box { half_extents } => format!("box({:?})", bits3(half_extents)),
                ColliderShape::Capsule {
                    radius,
                    half_height,
                } => format!("capsule({},{})", bits(radius), bits(half_height)),
            };
            line.push_str(&format!(
                " col={shape} static={} rest={} sensor={} groups={} mask={}",
                c.is_static,
                bits(c.restitution),
                c.is_sensor,
                c.groups,
                c.mask
            ));
        }
        if let Some(l) = world.get::<PointLight>(e) {
            line.push_str(&format!(
                " pl c={:?} i={} r={} sh={}",
                bits3(l.color),
                bits(l.intensity),
                bits(l.range),
                l.shadows
            ));
        }
        if let Some(p) = world.get::<ParticleEmitter>(e) {
            line.push_str(&format!(
                " pe blend={:?} rate={} burst={} elife={} plife={} spd={} jit={} grav={} \
                 s0={} s1={} c0={:?} c1={:?} max={}",
                p.blend,
                bits(p.spawn_rate),
                p.burst_count,
                bits(p.emitter_lifetime),
                bits(p.particle_lifetime),
                bits(p.initial_speed),
                bits(p.velocity_jitter),
                bits(p.gravity_scale),
                bits(p.size_start),
                bits(p.size_end),
                p.color_start.map(bits),
                p.color_end.map(bits),
                p.max_particles
            ));
        }
        out.push(line);
    }

    // MeshStore：逐句柄顶点 / 索引内容 FNV
    let meshes = world.resource::<MeshStore>();
    out.push(format!("mesh_store len={}", meshes.len()));
    for i in 0..meshes.len() {
        if let Some(m) = meshes.get(MeshHandle(i as u32)) {
            let mut bytes = Vec::new();
            for v in &m.vertices {
                bytes.extend_from_slice(&v.position.map(bits).map(u32::to_le_bytes).concat());
                bytes.extend_from_slice(&v.normal.map(bits).map(u32::to_le_bytes).concat());
                bytes.extend_from_slice(&v.uv.map(bits).map(u32::to_le_bytes).concat());
            }
            for idx in &m.indices {
                bytes.extend_from_slice(&idx.to_le_bytes());
            }
            out.push(format!(
                "M {i:04} verts={} tris={} fnv={:#018x}",
                m.vertices.len(),
                m.indices.len() / 3,
                fnv1a(&bytes)
            ));
        }
    }
    drop(meshes);

    // MaterialStore：逐句柄全字段位模式（无 len()，while-let 递增遍历）
    let materials = world.resource::<MaterialStore>();
    let mut i = 0u32;
    while let Some(d) = materials.get(MaterialHandle(i)) {
        out.push(format!(
            "MAT {i:04} base={:?} emis={:?} met={} rough={} tex={} norm={:?} orm={:?} alpha={:?}",
            d.base_color.map(bits),
            d.emissive.map(bits),
            bits(d.metallic),
            bits(d.roughness),
            d.texture.0,
            d.normal_texture.map(|t| t.0),
            d.orm_texture.map(|t| t.0),
            d.alpha_mode
        ));
        i += 1;
    }
    out.push(format!("material_store len={i}"));
    drop(materials);

    // TextureStore：逐句柄像素 FNV + sRGB 标志
    let textures = world.resource::<TextureStore>();
    out.push(format!("texture_store len={}", textures.len()));
    for i in 0..textures.len() {
        if let Some(t) = textures.get(TextureHandle(i as u32)) {
            out.push(format!(
                "TEX {i:04} {}x{} srgb={} fnv={:#018x}",
                t.width,
                t.height,
                t.srgb,
                fnv1a(&t.rgba)
            ));
        }
    }
    drop(textures);

    // 登记表（句柄 → 来源；BTree 序输出保证行序确定）
    let registry = world.resource::<SceneAssetRegistry>();
    let mut mesh_keys: Vec<&u32> = registry.meshes.keys().collect();
    mesh_keys.sort();
    for key in mesh_keys {
        out.push(format!("REG mesh {key} -> {:?}", registry.meshes[key]));
    }
    let mut mat_keys: Vec<&u32> = registry.materials.keys().collect();
    mat_keys.sort();
    for key in mat_keys {
        out.push(format!("REG mat {key} -> {:?}", registry.materials[key]));
    }
    out.push(format!("REG sky_source -> {:?}", registry.sky_source));
    drop(registry);

    // 方向光资源（全字段位模式）
    if let Some(l) = world.try_resource::<DirectionalLight>() {
        out.push(format!(
            "DIRLIGHT d={:?} c={:?} i={} amb={:?} sh={} range={}",
            bits3(l.direction),
            bits3(l.color),
            bits(l.intensity),
            bits3(l.ambient),
            l.shadows,
            bits(l.shadow_range)
        ));
    } else {
        out.push("DIRLIGHT absent".into());
    }
    out
}

/// 验收②：庭院关卡档 load → save → load，第二次世界与第一次**逐位一致**
/// （实体表 / 组件位模式 / Store 内容 FNV / 登记表 / 资源全量对照）。
#[test]
fn courtyard_level_roundtrips_bitwise() {
    let path = asset_path(LEVEL_ENTRY);
    let mut world_a = World::new();
    let entities_a = load_scene(&mut world_a, &path).expect("首次加载庭院关卡档");
    assert_eq!(
        entities_a.len(),
        15,
        "地面 + 4 墙 + 4 柱 + 喷泉 4 件 + 2 火盆"
    );

    let out = test_dir().join("courtyard_roundtrip.json");
    let count = save_scene(&mut world_a, &out).expect("保存");
    assert_eq!(count, 15, "save 写出全部静态实体");

    let mut world_b = World::new();
    let entities_b = load_scene(&mut world_b, &out).expect("二次加载");
    assert_eq!(
        entities_a.iter().map(|e| e.index()).collect::<Vec<_>>(),
        entities_b.iter().map(|e| e.index()).collect::<Vec<_>>(),
        "实体位模式（spawn 序）应可复现"
    );

    let dump_a = dump_world(&world_a);
    let dump_b = dump_world(&world_b);
    assert_eq!(dump_a.len(), dump_b.len(), "dump 行数");
    for (index, (a, b)) in dump_a.iter().zip(&dump_b).enumerate() {
        assert_eq!(a, b, "dump 第 {index} 行应逐位一致");
    }
}

/// 守护：关卡档与 `crate::logic` 常量的双源一致性（漂移即爆）。
/// 墙 / 柱逐块对照（位置 = center、缩放 = half×2、碰撞盒 = half），
/// 喷泉 / 方向光 / 火盆 / 水柱粒子对照代码生成时代的既定值。
#[test]
fn level_file_mirrors_logic_statics() {
    let path = asset_path(LEVEL_ENTRY);
    let mut world = World::new();
    load_scene(&mut world, &path).expect("加载庭院关卡档");

    let named = |name: &str| -> Entity {
        world
            .entity_named(name)
            .unwrap_or_else(|| panic!("缺实体 {name}"))
    };

    // 地面（既定值：视觉盒 20.8×0.5×14.8 垫底 0.25）
    let ground = named("assault/ground");
    let t = *world.get::<Transform>(ground).unwrap();
    assert_eq!(bits3(t.position), bits3(Vec3::new(0.0, -0.25, 0.0)));
    assert_eq!(bits3(t.scale), bits3(Vec3::new(20.8, 0.5, 14.8)));
    let c = *world.get::<Collider>(ground).unwrap();
    assert_eq!(
        c.shape,
        ColliderShape::Box {
            half_extents: Vec3::new(10.4, 0.25, 7.4)
        }
    );
    assert!(c.is_static);

    // 墙 / 柱：与 logic 常量逐块位级对照（视觉缩放 = half×2 的 f32 精确值）
    for (index, block) in logic::WALLS.iter().enumerate() {
        let entity = named(&format!("assault/wall/{index}"));
        let t = *world.get::<Transform>(entity).unwrap();
        assert_eq!(bits3(t.position), bits3(block.center), "墙 {index} 位置");
        assert_eq!(
            bits3(t.scale),
            bits3(block.half * 2.0),
            "墙 {index} 缩放 = half×2（位级）"
        );
        let c = *world.get::<Collider>(entity).unwrap();
        assert_eq!(
            c.shape,
            ColliderShape::Box {
                half_extents: block.half
            },
            "墙 {index} 碰撞盒 = logic 常量"
        );
        assert!(c.is_static);
    }
    for (index, block) in logic::PILLARS.iter().enumerate() {
        let entity = named(&format!("assault/pillar/{index}"));
        let t = *world.get::<Transform>(entity).unwrap();
        assert_eq!(bits3(t.position), bits3(block.center), "柱 {index} 位置");
        assert_eq!(bits3(t.scale), bits3(block.half * 2.0), "柱 {index} 缩放");
        let c = *world.get::<Collider>(entity).unwrap();
        assert_eq!(
            c.shape,
            ColliderShape::Box {
                half_extents: block.half
            },
            "柱 {index} 碰撞盒 = logic 常量"
        );
        assert!(c.is_static);
    }

    // 喷泉盆：物理球 1.75 @ y 0.35（防 ragdoll 陷盆的既定值；注意与
    // logic::FOUNTAIN_COLLIDER（1.8 @ 0.5，纯逻辑元）刻意不同源）
    let fountain = named("assault/fountain");
    let t = *world.get::<Transform>(fountain).unwrap();
    assert_eq!(bits3(t.position), bits3(Vec3::new(0.0, 0.35, 0.0)));
    let c = *world.get::<Collider>(fountain).unwrap();
    assert_eq!(c.shape, ColliderShape::Sphere { radius: 1.75 });
    assert!(c.is_static);

    // 方向光：黄昏斜阳（direction = normalize(-0.35, -0.8, -0.45) 位级）
    let light = world.resource::<DirectionalLight>();
    assert_eq!(
        bits3(light.direction),
        bits3(Vec3::new(-0.35, -0.8, -0.45).normalize_or_zero()),
        "方向光 direction 应与代码时代逐位一致"
    );
    assert_eq!(bits3(light.color), bits3(Vec3::new(1.0, 0.88, 0.74)));
    assert_eq!(bits(light.intensity), bits(1.3));
    assert_eq!(bits3(light.ambient), bits3(Vec3::new(0.22, 0.21, 0.21)));
    assert!(light.shadows);
    drop(light);

    // 火盆点光：两盏暖色（位置 / 参数既定值）
    for (index, at) in [Vec3::new(7.4, 2.6, 5.8), Vec3::new(-7.4, 2.6, -5.8)]
        .iter()
        .enumerate()
    {
        let torch = named(&format!("assault/torch/{index}"));
        let t = *world.get::<Transform>(torch).unwrap();
        assert_eq!(bits3(t.position), bits3(*at), "火盆 {index} 位置");
        let l = world.get::<PointLight>(torch).expect("火盆应有点光");
        assert_eq!(bits3(l.color), bits3(Vec3::new(1.0, 0.62, 0.3)));
        assert_eq!(bits(l.intensity), bits(7.0));
        assert_eq!(bits(l.range), bits(13.0));
        assert!(!l.shadows);
    }

    // 水柱粒子：与 ParticleEmitter::fountain() 配置字段逐项位级一致
    let jet = named("assault/fountain/jet");
    let emitter = world.get::<ParticleEmitter>(jet).expect("水柱应有发射器");
    let expected = ParticleEmitter::fountain();
    assert_eq!(emitter.blend, expected.blend);
    for (name, a, b) in [
        ("spawn_rate", emitter.spawn_rate, expected.spawn_rate),
        (
            "particle_lifetime",
            emitter.particle_lifetime,
            expected.particle_lifetime,
        ),
        (
            "initial_speed",
            emitter.initial_speed,
            expected.initial_speed,
        ),
        (
            "velocity_jitter",
            emitter.velocity_jitter,
            expected.velocity_jitter,
        ),
        (
            "gravity_scale",
            emitter.gravity_scale,
            expected.gravity_scale,
        ),
        ("size_start", emitter.size_start, expected.size_start),
        ("size_end", emitter.size_end, expected.size_end),
    ] {
        assert_eq!(bits(a), bits(b), "水柱 {name} 应与 fountain() 预设位级一致");
    }
    assert_eq!(
        emitter.color_start.map(bits),
        expected.color_start.map(bits)
    );
    assert_eq!(emitter.color_end.map(bits), expected.color_end.map(bits));
    assert_eq!(emitter.max_particles, expected.max_particles);
    assert_eq!(emitter.burst_count, expected.burst_count);
    assert_eq!(
        emitter.emitter_lifetime.to_bits(),
        expected.emitter_lifetime.to_bits()
    );
}

/// 守护：庭院材质三槽接线（R42 写实材质经关卡档的程序化纹理源还原）——
/// 石板 / 砖墙材质持法线 + ORM 槽且纹理为线性数据（srgb = false），
/// 与 headless 测试 `courtyard_materials_carry_normal_and_orm_slots` 的
/// 庭院侧前提一致（那条测试统计含角色 GLB 材质，本条只看关卡档产物）。
#[test]
fn level_materials_carry_procedural_normal_and_orm() {
    let path = asset_path(LEVEL_ENTRY);
    let mut world = World::new();
    load_scene(&mut world, &path).expect("加载庭院关卡档");

    let textures = world.resource::<TextureStore>();
    let linear = (0..textures.len())
        .filter_map(|i| textures.get(TextureHandle(i as u32)))
        .filter(|t| !t.srgb)
        .count();
    assert_eq!(linear, 4, "石板对 + 砖墙对 = 4 张线性数据纹理");
    drop(textures);

    let materials = world.resource::<MaterialStore>();
    let mut textured = 0usize;
    let mut i = 0u32;
    while let Some(def) = materials.get(MaterialHandle(i)) {
        if def.normal_texture.is_some() && def.orm_texture.is_some() {
            textured += 1;
        }
        i += 1;
    }
    assert_eq!(textured, 2, "石板 + 砖墙两材质挂法线 + ORM");

    // 登记表：两材质的法线来源是程序化参数（save 还原依据）
    let registry = world.resource::<SceneAssetRegistry>();
    let mut procedural = 0usize;
    for mat_ref in registry.materials.values() {
        if matches!(
            mat_ref.normal_source,
            Some(TextureSourceSection::Procedural(_))
        ) {
            procedural += 1;
        }
    }
    assert_eq!(procedural, 2, "石板 / 砖墙的法线源应为程序化参数");
}
