//! 集成测试：阴影 v2（R61）——消偏艺术（坡度缩放 bias + 法线偏移）与
//! 点光阴影预算化多盏。
//!
//! 七条主线：
//!
//! 1. **归因对照组（硬约束）**：把本轮受影响 GPU FNV 基线场景以「阴影
//!    关闭」（`DirectionalLight.shadows = false` 且点光 `shadows = false`）
//!    渲染——该路径不进入任何阴影采样分支，R61 前后必须**逐位一致**。
//!    基线值取自改动落地前的旧代码实跑（同一场景、同一分辨率）：这是
//!    「改动被限制在阴影采样路径内」的直接证据（三条哈希实测逐位相同）。
//! 2. **对照模式复现 R60 逐位**：`Renderer::set_legacy_shadow_bias(true)`
//!    把偏移口径切回 v1 常数 bias，阴影开启帧必须与 R60 旧代码**逐位
//!    一致**——反过来证明 v2 与 v1 的差别**只**来自偏移量。
//! 3. **掠射角 acne**：无遮挡物的掠射角平面场景（环境项归零以使直接光
//!    主导），旧常数 bias 下 4575 个 acne 像素 → 新技术 0 个。
//! 4. **接触阴影 peter-panning 不恶化**：贴地立方体根部的亮带宽度
//!    （沿阴影方向逐像素扫描）在两种口径下的对照，新技术 ≤ 旧常数。
//! 5. **多盏点光各投各影**：≥2 盏同时投影、互不串扰（两盏各自的变暗
//!    像素集合可加：791 + 179 = 970），且预算外的灯被确定性裁掉。
//! 6. **确定性**：阴影开启（CSM + 双点光投射）的双会话逐帧逐位一致。
//! 7. 预算选择键的确定性由 `light.rs` 的纯函数单测承担（乱序插入选中
//!    同一批灯）。
//!
//! 环境无 GPU 适配器时优雅跳过（`OffscreenRenderer::new` 返回 `None`）。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Entity, Quat, Transform, World};
use iris_render::prelude::*;

/// 七项测试各自建渲染器、共用单 GPU 适配器，并行会互相争用直至停顿
/// （R62b 同款形态，debug 下亦复现两次）——文件内互斥串行化（参照
/// `ssr_render.rs::GPU_SERIAL`），标准门禁命令默认可靠。
static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ═══════════════════════════════════════════════════════════════════════
// 基线常量（全部为 R61 改动落地**之前**的旧代码实跑捕获）
// ═══════════════════════════════════════════════════════════════════════

/// R41 / R42 基线场景的「阴影关闭」对照组基线（归因证据）。
const R41_SCENE_SHADOWS_OFF_HASH: u64 = 0x6a56_af90_bf4c_6686;
/// R52b 室内盒场景的「阴影关闭」对照组基线（归因证据）。
const ROOM_SCENE_SHADOWS_OFF_HASH: u64 = 0x8fbb_a702_069d_c4f5;
/// 双点光（皆 `shadows = false`）场景的对照组基线（「无阴影点光零变化」）。
const PLAIN_POINT_LIGHTS_HASH: u64 = 0xf70f_dbd5_ec3b_06e1;
/// 掠射角平面场景 + 阴影开启：v1 常数 bias 下的旧代码输出（对照模式
/// 必须逐位复现）。
const GRAZING_LEGACY_HASH: u64 = 0x3c63_2619_13b3_b9b2;
/// 接触阴影场景 + 阴影开启：v1 常数 bias 下的旧代码输出（**与同场景
/// 「阴影关闭」帧逐位相同**——旧口径把 0.07 m 高薄板的整条接触阴影判成
/// 受光，见 `contact_shadow_stays_attached_for_low_caster`）。
const CONTACT_LEGACY_HASH: u64 = 0x1f1e_5fa0_8511_da1b;
/// 双点光场景「只有 A 投影」：v1 旧代码输出（v1 取查询序首盏 = 唯一
/// 一盏投影灯，与 R61 的单盏选择结果同灯同槽位）。
const CASTER_A_LEGACY_HASH: u64 = 0xe11a_b896_abd5_7ad9;
/// 掠射角平面场景在 v1 常数 bias 下的 acne 像素数（旧代码实跑：4575）。
const GRAZING_LEGACY_ACNE: usize = 4575;

/// 全帧像素 FNV-1a 64（与既有 GPU 基线锁同款哈希）。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ═══════════════════════════════════════════════════════════════════════
// 场景
// ═══════════════════════════════════════════════════════════════════════

/// 与 `normal_render.rs` / `post_render.rs` 基线完全一致的场景（地面 +
/// 砖色立方体 + 金属圆柱 + 默认 IBL + 侧光），`shadows` 可切。
fn r41_scene(shadows: bool) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 2.2, 6.0),
        target: Vec3::new(0.0, 1.0, 0.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        intensity: 1.2,
        shadows,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let cyl = world
        .resource_mut::<MeshStore>()
        .add(MeshData::cylinder(0.6, 2.0, 24));
    let mut mats = world.resource_mut::<MaterialStore>();
    let wall = mats.add(MaterialDef::solid(0.65, 0.55, 0.45, 1.0));
    let metal = mats.add(MaterialDef::metal(0.9, 0.85, 0.8, 0.35));
    drop(mats);
    world.spawn((Transform::IDENTITY, plane, wall));
    world.spawn((
        Transform {
            position: Vec3::new(-2.0, 1.0, -1.5),
            ..Transform::IDENTITY
        },
        cube,
        wall,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(1.5, 1.0, -1.0),
            rotation: Quat::IDENTITY,
            ..Transform::IDENTITY
        },
        cyl,
        metal,
    ));
    propagate_transforms(&mut world);
    world
}

/// 与 `lightmap_render.rs` 基线完全一致的室内盒场景（地板 + 两墙 + 遮挡
/// 柱 + 侧向暖光），`shadows` 可切。
fn room_scene(shadows: bool) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(3.4, 2.2, 3.8),
        target: Vec3::new(-1.2, 0.6, -2.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.55, -1.0, 0.3).normalize(),
        color: Vec3::new(1.0, 0.95, 0.85),
        intensity: 2.2,
        ambient: Vec3::new(0.10, 0.11, 0.14),
        shadows,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());

    let floor_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(10.0));
    let wall_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(10.0));
    let column_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mut mats = world.resource_mut::<MaterialStore>();
    let floor_mat = mats.add(MaterialDef::solid(0.8, 0.78, 0.72, 1.0));
    let wall_mat = mats.add(MaterialDef::solid(0.55, 0.6, 0.7, 1.0));
    let column_mat = mats.add(MaterialDef::solid(0.9, 0.35, 0.2, 1.0));
    drop(mats);

    world.spawn((Transform::IDENTITY, floor_mesh, floor_mat));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 2.0, -5.0),
            rotation: Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
            scale: Vec3::new(1.0, 1.0, 0.4),
        },
        wall_mesh,
        wall_mat,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(-5.0, 2.0, 0.0),
            rotation: Quat::from_rotation_z(-std::f32::consts::FRAC_PI_2),
            scale: Vec3::new(0.4, 1.0, 1.0),
        },
        wall_mesh,
        wall_mat,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(1.5, 1.0, 0.0),
            scale: Vec3::new(0.6, 2.0, 0.6),
            ..Transform::IDENTITY
        },
        column_mesh,
        column_mat,
    ));
    propagate_transforms(&mut world);
    world
}

/// 双点光场景（无阴影）：地面 + 两盏 `shadows = false` 的点光——「无阴影
/// 点光零变化」的对照组载体（点光着色循环全程不进入任何阴影分支）。
fn two_plain_point_lights_scene() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(4.0, 2.5, 4.0),
        target: Vec3::new(0.0, 0.5, 0.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());

    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.75, 0.75, 0.75, 1.0));
    world.spawn((Transform::from_xyz(0.0, 1.0, 0.0), cube, mat));
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(30.0));
    let mat_ground = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.5, 0.5, 0.5, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_ground));
    world.spawn((
        Transform::from_xyz(-2.0, 3.0, 0.0),
        PointLight {
            shadows: false,
            ..PointLight::default()
        },
    ));
    world.spawn((
        Transform::from_xyz(2.0, 2.5, 1.0),
        PointLight {
            color: Vec3::new(0.6, 0.8, 1.0),
            intensity: 6.0,
            range: 10.0,
            shadows: false,
        },
    ));
    propagate_transforms(&mut world);
    world
}

/// 掠射角大平面场景：120 m 地面 + 近水平方向光（与地面法线夹角 ≈ 83°），
/// 场景内**没有任何遮挡物**——画面里任何变暗像素都只能来自自遮挡 acne。
///
/// 环境项（ambient 与 IBL）全部归零：acne 的可判别性取决于「直接光在
/// 像素中的占比」，环境光不参与阴影调制，会把自遮挡的相对暗化稀释到
/// 阈值之下（实测：带默认 ambient + IBL 时旧常数 bias 的 acne 暗化
/// 仅 ~3/255，落进量化噪声）。
fn grazing_plane_scene(shadows: bool) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 3.0, 20.0),
        target: Vec3::new(0.0, 2.0, -20.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-1.0, -0.12, -0.15).normalize(),
        intensity: 3.0,
        ambient: Vec3::ZERO,
        shadows,
        ..DirectionalLight::default()
    });
    world.insert_resource(EnvironmentSettings { intensity: 0.0 });
    world.insert_resource(BloomSettings::disabled());
    let plane = world
        .resource_mut::<MeshStore>()
        .add(MeshData::plane(120.0));
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.6, 0.6, 0.6, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat));
    propagate_transforms(&mut world);
    world
}

/// 接触阴影场景：20 m 地面 + **贴地薄板**（1 × 0.08 × 1 m，底面恰在
/// y = 0），相机在 −X 侧俯看薄板的接触阴影带。
///
/// 为什么用低矮遮挡物：peter-panning 的敏感量是「接收面沿光线的余量」
/// ——薄板顶面在接地射线方向上的距离 = 板高 / cosθ ≈ 0.08 / 0.617 ≈
/// **0.13 m**，与旧常数 bias 的世界量级（0.0015 × 深度窗口 ≈ 0.15 m）
/// 同阶：旧口径把整条接触阴影**抹掉**（阴影整体浮起 = peter-panning 的
/// 极端形态），v2 的 0.07 m 量级则让它完整保留。高物体（1 m 立方体）
/// 的余量 1.6 m ≫ 两种 bias，对口径不敏感，测不出差别（R61 实测踩过：
/// 用立方体做对照，两种口径的剖面逐像素相同）。
fn contact_scene(shadows: bool) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(-2.6, 1.6, 2.6),
        target: Vec3::new(0.0, 0.0, 0.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.75, -0.62, -0.25).normalize(),
        intensity: 1.5,
        ambient: Vec3::ZERO,
        shadows,
        ..DirectionalLight::default()
    });
    world.insert_resource(EnvironmentSettings { intensity: 0.0 });
    world.insert_resource(BloomSettings::disabled());
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(20.0));
    let mat_ground = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.6, 0.6, 0.6, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_ground));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat_obj = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.8, 0.4, 0.3, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 0.035, 0.0),
            scale: Vec3::new(2.0, 0.07, 2.0),
            ..Transform::IDENTITY
        },
        cube,
        mat_obj,
    ));
    propagate_transforms(&mut world);
    world
}

/// 点光阴影投射器的开关组合（多盏互不串扰测试用）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum CasterMode {
    /// 两盏都不投影。
    None,
    /// 只有 −X 侧那盏（灯 A）投影。
    A,
    /// 只有 +X 侧那盏（灯 B）投影。
    B,
    /// 两盏都投影。
    Both,
}

/// 双点光场景：中央遮挡方块 + 左右两盏点光（各自把阴影投向背离自己的
/// 一侧：A → +X 侧阴影带，B → −X 侧阴影带）。方向光关阴影。
fn two_casters_scene(mode: CasterMode) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 6.5, 9.0),
        target: Vec3::new(0.0, 0.6, 0.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        intensity: 0.25,
        ambient: Vec3::splat(0.05),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let mat_ground = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.5, 0.5, 0.5, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_ground));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat_obj = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.7, 0.7, 0.7, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 1.6, 0.0),
            scale: Vec3::splat(1.2),
            ..Transform::IDENTITY
        },
        cube,
        mat_obj,
    ));
    world.spawn((
        Transform::from_xyz(-4.0, 3.2, 0.0),
        PointLight {
            color: Vec3::new(1.0, 0.85, 0.6),
            intensity: 30.0,
            range: 14.0,
            shadows: matches!(mode, CasterMode::A | CasterMode::Both),
        },
    ));
    world.spawn((
        Transform::from_xyz(4.0, 3.2, 0.0),
        PointLight {
            color: Vec3::new(0.6, 0.85, 1.0),
            intensity: 30.0,
            range: 14.0,
            shadows: matches!(mode, CasterMode::B | CasterMode::Both),
        },
    ));
    propagate_transforms(&mut world);
    world
}

/// 预算压力场景：中央遮挡块 + 5 盏开启阴影的点光（预算 4 盏 ⇒ 最远的一盏
/// 必须被确定性裁掉）。灯位置沿 −Z 逐盏远离相机（eye 在 +Z 侧）。
fn over_budget_scene() -> World {
    let mut world = two_casters_scene(CasterMode::Both);
    for i in 0..3 {
        world.spawn((
            Transform::from_xyz(0.0, 3.2, -3.0 - i as f32 * 2.5),
            PointLight {
                color: Vec3::new(0.7, 0.7, 0.7),
                intensity: 12.0,
                range: 10.0,
                shadows: true,
            },
        ));
    }
    propagate_transforms(&mut world);
    world
}

// ═══════════════════════════════════════════════════════════════════════
// 1. 归因对照组
// ═══════════════════════════════════════════════════════════════════════

/// 归因对照组：三个场景的「阴影关闭」变体在 R61 前后逐位一致——非阴影
/// 像素零触碰（本轮全部 FNV 基线重锁的归因前提）。
#[test]
fn shadow_off_control_group_is_bit_identical_to_pre_r61() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(320, 180) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    renderer.render(&r41_scene(false));
    let r41 = fnv1a(&renderer.read_pixels());

    let Some(mut room_renderer) = OffscreenRenderer::new(480, 270) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    room_renderer.render(&room_scene(false));
    let room = fnv1a(&room_renderer.read_pixels());

    let Some(mut point_renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    point_renderer.render(&two_plain_point_lights_scene());
    let points = fnv1a(&point_renderer.read_pixels());

    println!("对照组 R41 场景（阴影关）= {r41:#018x}");
    println!("对照组 室内盒（阴影关）= {room:#018x}");
    println!("对照组 双点光（皆无阴影）= {points:#018x}");

    assert_eq!(
        r41, R41_SCENE_SHADOWS_OFF_HASH,
        "阴影关闭的 R41 场景必须与 R61 前的旧代码逐位一致——\
         改动被限制在阴影采样路径内的归因证据"
    );
    assert_eq!(
        room, ROOM_SCENE_SHADOWS_OFF_HASH,
        "阴影关闭的室内盒场景必须与 R61 前的旧代码逐位一致"
    );
    assert_eq!(
        points, PLAIN_POINT_LIGHTS_HASH,
        "无阴影点光的着色路径必须与 R61 前的旧代码逐位一致"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// 2. 对照模式复现 R60
// ═══════════════════════════════════════════════════════════════════════

/// `set_legacy_shadow_bias(true)` 逐位复现 R60 的阴影采样输出（三个阴影
/// 开启场景）：v2 与 v1 的差别**只**是偏移量口径，采样路径、矩阵、PCF
/// 权重、掩码全部未动。
///
/// **每个场景一个独立渲染器**：渲染器的 GPU 缓存按（网格 / 材质 / 纹理）
/// 句柄值键控、假设目录跨帧稳定，同尺寸但内容不同的两个场景共用一个渲染器
/// 会让后者的句柄命中前者的缓存（R53 / taa_snapshot 的既有教训；R61 实测
/// 又踩过一次——共用渲染器时接触阴影场景的哈希对不上 R60，拆开即逐位一致）。
#[test]
fn legacy_bias_mode_reproduces_r60_output_bitwise() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut grazing_renderer) = OffscreenRenderer::new(320, 180) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    grazing_renderer.renderer_mut().set_legacy_shadow_bias(true);
    grazing_renderer.render(&grazing_plane_scene(true));
    let grazing = fnv1a(&grazing_renderer.read_pixels());

    let Some(mut contact_renderer) = OffscreenRenderer::new(320, 180) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    contact_renderer.renderer_mut().set_legacy_shadow_bias(true);
    contact_renderer.render(&contact_scene(true));
    let contact = fnv1a(&contact_renderer.read_pixels());
    // 旧口径把低矮薄板的接触阴影整条抹掉：对照模式下「开阴影」与
    // 「关阴影」两帧**逐位相同**（R60 实跑同结论）
    contact_renderer.render(&contact_scene(false));
    let contact_off = fnv1a(&contact_renderer.read_pixels());
    assert_eq!(
        contact, contact_off,
        "对照模式（旧常数 bias）下低矮薄板的接触阴影必须整条消失：\
         开/关阴影两帧应逐位相同（实测 R60 亦如此）"
    );

    let Some(mut multi) = OffscreenRenderer::new(384, 216) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    multi.renderer_mut().set_legacy_shadow_bias(true);
    multi.render(&two_casters_scene(CasterMode::A));
    let caster_a = fnv1a(&multi.read_pixels());

    println!("对照模式 掠射角 = {grazing:#018x}（R60 = {GRAZING_LEGACY_HASH:#018x}）");
    println!("对照模式 接触阴影 = {contact:#018x}（R60 = {CONTACT_LEGACY_HASH:#018x}）");
    println!("对照模式 单盏点光 = {caster_a:#018x}（R60 = {CASTER_A_LEGACY_HASH:#018x}）");

    assert_eq!(
        grazing, GRAZING_LEGACY_HASH,
        "对照模式必须逐位复现 R60 的掠射角场景"
    );
    assert_eq!(
        contact, CONTACT_LEGACY_HASH,
        "对照模式必须逐位复现 R60 的接触阴影场景"
    );
    assert_eq!(
        caster_a, CASTER_A_LEGACY_HASH,
        "对照模式必须逐位复现 R60 的单盏点光阴影（同灯、同槽位、同结果）"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// 3. 掠射角 acne
// ═══════════════════════════════════════════════════════════════════════

/// acne 判据（相对口径，与曝光 / 反照率解耦）：开关阴影两帧同像素，
/// 亮度（RGB 平均）相对下降超过 8% 且受光帧亮度不低（> 20/255）——
/// 绝对差分阈值在掠射角下失效（直接光本身很暗，绝对暗化落进量化噪声）。
fn acne_pixels(with_shadows: &[u8], without_shadows: &[u8]) -> usize {
    with_shadows
        .chunks(4)
        .zip(without_shadows.chunks(4))
        .filter(|(on, off)| {
            let lon = (on[0] as f32 + on[1] as f32 + on[2] as f32) / 3.0;
            let loff = (off[0] as f32 + off[1] as f32 + off[2] as f32) / 3.0;
            loff > 20.0 && lon < loff * 0.92
        })
        .count()
}

/// 掠射角平面（NdotL ≈ 0.118、θ ≈ 83°、场景无任何遮挡物）：
/// 旧常数 bias 下 4575 个自遮挡 acne 像素 → 坡度缩放 bias + 法线偏移 **0**
/// 个。断言同时钉住「旧口径确实有 acne」（否则测试退化）与「新口径归零」。
#[test]
fn slope_scaled_bias_removes_grazing_plane_acne() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(320, 180) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    // 受光参考帧（阴影关闭 = 无任何自遮挡）
    renderer.render(&grazing_plane_scene(false));
    let reference = renderer.read_pixels();
    // 新口径（v2）
    renderer.render(&grazing_plane_scene(true));
    let v2 = acne_pixels(&renderer.read_pixels(), &reference);
    // 旧口径（v1 常数 bias 0.0015 NDC）
    renderer.renderer_mut().set_legacy_shadow_bias(true);
    renderer.render(&grazing_plane_scene(true));
    let legacy = acne_pixels(&renderer.read_pixels(), &reference);

    println!("掠射角 acne 像素：旧常数 bias = {legacy}，v2 = {v2}（全帧 57600 像素）");
    assert_eq!(
        legacy, GRAZING_LEGACY_ACNE,
        "旧常数 bias 的 acne 像素数应与 R60 实跑一致（口径敏感性自检）"
    );
    assert!(
        legacy > 1000,
        "测试场景必须在旧常数 bias 下产生成片 acne，否则断言退化：{legacy}"
    );
    assert_eq!(v2, 0, "坡度缩放 bias + 法线偏移必须消除掠射角 acne：{v2}");
}

// ═══════════════════════════════════════════════════════════════════════
// 4. 接触阴影 peter-panning
// ═══════════════════════════════════════════════════════════════════════

/// 世界点投影到像素坐标（与 Camera 的 DirectX 深度约定一致的 CPU 复算）。
fn project_to_pixel(point: Vec3, camera: &Camera, width: u32, height: u32) -> (f32, f32) {
    let vp = camera.view_proj(width as f32 / height as f32);
    let clip = vp * point.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    (
        (ndc.x * 0.5 + 0.5) * width as f32,
        (1.0 - (ndc.y * 0.5 + 0.5)) * height as f32,
    )
}

/// 像素亮度（RGB 平均）。
fn lum(px: &[u8], w: u32, x: u32, y: u32) -> f32 {
    let i = ((y * w + x) * 4) as usize;
    (px[i] as f32 + px[i + 1] as f32 + px[i + 2] as f32) / 3.0
}

/// 接触阴影「根部亮带宽度」（像素）：从贴地薄板的根部沿**阴影方向**逐
/// 像素往外扫，找到第一个落到「阴影暗度」的采样点；亮带宽度 = 根部到该
/// 点的像素距离（peter-panning 的直接度量：数值越小 = 阴影越贴合物体）。
///
/// 阴影方向推导：方向光 `direction = normalize(−0.75, −0.62, −0.25)` 是
/// **传播**方向；地面点向光源移动（−direction）每升高 1 m，水平位移
/// (1.2097, 0.4032) m。薄板底面 [−0.5, 0.5]²、高 0.08 ⇒ 接地射线方向
/// 的顶面余量 = 0.08 / 0.617 ≈ 0.13 m；阴影落在 **−X 侧**，沿 z = 0 从
/// 根部 x = −0.5 扫到 −1.0（解析阴影带 x ∈ [−0.60, 0.5]）。
fn contact_gap_pixels(px: &[u8], w: u32, h: u32) -> f32 {
    let camera = Camera {
        eye: Vec3::new(-2.6, 1.6, 2.6),
        target: Vec3::new(0.0, 0.0, 0.0),
        ..Camera::default()
    };
    let to_px = |world: Vec3| {
        let (x, y) = project_to_pixel(world, &camera, w, h);
        (
            x.round().clamp(0.0, (w - 1) as f32) as u32,
            y.round().clamp(0.0, (h - 1) as f32) as u32,
        )
    };
    // 亮地面参考亮度：阴影带之外（x = −2.2 远在解析阴影带 x ≥ −1.09 之外）
    let (rx, ry) = to_px(Vec3::new(-2.2, 0.002, 0.0));
    let lit = lum(px, w, rx, ry);
    assert!(lit > 20.0, "亮地面参考亮度应在量化噪声之上：{lit}");
    let threshold = lit * 0.8;
    let base = project_to_pixel(Vec3::new(-1.0, 0.002, 0.0), &camera, w, h).0;
    for i in 0..300 {
        let x = -1.0 - i as f32 * 0.004;
        let (xi, yi) = to_px(Vec3::new(x, 0.002, 0.0));
        if lum(px, w, xi, yi) < threshold {
            return (project_to_pixel(Vec3::new(x, 0.002, 0.0), &camera, w, h).0 - base).abs();
        }
    }
    f32::NAN
}

/// 接触阴影两端都钉住（消偏艺术的低矮遮挡物端）：
///
/// ① **旧常数 bias 抹掉低矮物体的接触阴影**——薄板顶面在接地射线方向的
///    余量 0.13 m 小于旧 bias 的世界量级 0.15 m，整条阴影被判为「受光」
///    （阴影浮起 = peter-panning 的极端形态），变暗像素 ≈ 0；
/// ② **v2 保留完整阴影带**（余量 0.13 m > v2 的 0.07 m 量级），变暗像素
///    成片；
/// ③ **不浮起**：v2 的阴影根部亮带 ≤ 4 px（法线偏移没有把接触阴影推走）。
#[test]
fn contact_shadow_stays_attached_for_low_caster() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(640, 360) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let (w, h) = renderer.size();
    renderer.render(&contact_scene(false));
    let reference = renderer.read_pixels();
    renderer.render(&contact_scene(true));
    let v2_px = renderer.read_pixels();
    renderer.renderer_mut().set_legacy_shadow_bias(true);
    renderer.render(&contact_scene(true));
    let legacy_px = renderer.read_pixels();

    let v2_shadow = acne_pixels(&v2_px, &reference);
    let legacy_shadow = acne_pixels(&legacy_px, &reference);
    let v2_gap = contact_gap_pixels(&v2_px, w, h);
    println!(
        "接触阴影（薄板，射线余量 0.13 m）：旧常数 bias 阴影像素 {legacy_shadow}，\
         v2 阴影像素 {v2_shadow}，v2 根部亮带 {v2_gap:.1} px"
    );
    assert!(
        legacy_shadow < 50,
        "旧常数 bias（世界量级 ~0.15 m）应抹掉 0.13 m 余量的接触阴影：{legacy_shadow}"
    );
    assert!(
        v2_shadow > 800,
        "v2 必须保留低矮物体的接触阴影带：{v2_shadow}"
    );
    assert!(
        v2_gap.is_finite() && v2_gap <= 4.0,
        "v2 的接触阴影必须贴住物体根部（亮带 ≤ 4 px，防法线偏移过大浮起）：{v2_gap}"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// 5. 多盏点光各投各影
// ═══════════════════════════════════════════════════════════════════════

/// 变暗像素集合（RGB 和差 > 12）与「所有变暗像素的平均 x」。
fn darkened_mask(on: &[u8], off: &[u8], w: u32, h: u32) -> (usize, f32) {
    let mut count = 0usize;
    let mut sum_x = 0f64;
    for y in 0..h as usize {
        for x in 0..w as usize {
            let i = (y * w as usize + x) * 4;
            let sa = on[i] as i32 + on[i + 1] as i32 + on[i + 2] as i32;
            let sb = off[i] as i32 + off[i + 1] as i32 + off[i + 2] as i32;
            if sa < sb - 12 {
                count += 1;
                sum_x += x as f64;
            }
        }
    }
    (
        count,
        if count == 0 {
            0.0
        } else {
            (sum_x / count as f64) as f32
        },
    )
}

/// 两盏点光同时投影、各投各影、互不串扰：
/// ① 单开 A 的变暗像素集中在遮挡物 +X 侧（A 在 −X，阴影被推向 +X）；
/// ② 单开 B 的集中在 −X 侧；
/// ③ 双开的变暗像素数 ≈ 两者之和（阴影集合可加 = 无串扰）；
/// ④ `FrameStats.point_shadow_casters` 逐帧报告 0/1/1/2。
#[test]
fn two_point_lights_cast_independent_shadows() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(384, 216) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let (w, h) = renderer.size();
    renderer.render(&two_casters_scene(CasterMode::None));
    let none = renderer.read_pixels();
    assert_eq!(
        renderer.renderer().stats().point_shadow_casters,
        0,
        "无投影灯时点光阴影 pass 必须完全跳过"
    );

    renderer.render(&two_casters_scene(CasterMode::A));
    let a_only = renderer.read_pixels();
    let a_casters = renderer.renderer().stats().point_shadow_casters;
    renderer.render(&two_casters_scene(CasterMode::B));
    let b_only = renderer.read_pixels();
    let b_casters = renderer.renderer().stats().point_shadow_casters;
    renderer.render(&two_casters_scene(CasterMode::Both));
    let both = renderer.read_pixels();
    let both_casters = renderer.renderer().stats().point_shadow_casters;

    let (a_px, a_cx) = darkened_mask(&a_only, &none, w, h);
    let (b_px, b_cx) = darkened_mask(&b_only, &none, w, h);
    let (both_px, _) = darkened_mask(&both, &none, w, h);
    println!(
        "双点光阴影：A 单开 {a_px} px（重心 x = {a_cx:.1}）、B 单开 {b_px} px（重心 x = {b_cx:.1}）\
         、双开 {both_px} px；投影灯数 = {a_casters}/{b_casters}/{both_casters}"
    );

    assert!(a_px > 100 && b_px > 50, "两盏灯的阴影都必须成片可见");
    assert_eq!(
        (a_casters, b_casters, both_casters),
        (1, 1, 2),
        "投影灯数必须逐帧随开关变化（预算内多盏同时投影）"
    );
    let half = w as f32 * 0.5;
    assert!(
        a_cx > half,
        "灯 A 在 −X 侧，其阴影应落在遮挡物 +X 侧：重心 x = {a_cx:.1}"
    );
    assert!(
        b_cx < half,
        "灯 B 在 +X 侧，其阴影应落在遮挡物 −X 侧：重心 x = {b_cx:.1}"
    );
    // ③ 可加性：两盏的阴影像素集合互不重叠（容差 = MSAA 边缘抖动）
    let sum = a_px + b_px;
    assert!(
        (both_px as i64 - sum as i64).unsigned_abs() <= 8,
        "双开阴影像素数应等于两次单开之和（互不串扰）：双开 {both_px} vs {a_px} + {b_px}"
    );
}

/// 预算语义：5 盏开启阴影、预算 4 盏——逐帧只渲染 4 盏（最远的被确定性
/// 裁掉），`FrameStats.point_shadow_casters` 如实报告；两次渲染逐位一致
/// （选择键不依赖查询序 ⇒ 同灯集合必得同结果）。
#[test]
fn point_shadow_budget_caps_casters_deterministically() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(320, 180) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    renderer.render(&over_budget_scene());
    let casters = renderer.renderer().stats().point_shadow_casters;
    let first = renderer.read_pixels();
    renderer.render(&over_budget_scene());
    let second = renderer.read_pixels();
    println!("预算压力场景：投影灯数 = {casters}（预算 4）");
    assert_eq!(casters, 4, "超出预算的投影灯必须被裁掉（预算 = 4）");
    assert_eq!(first, second, "同灯集合的两次渲染必须逐位一致");
}

// ═══════════════════════════════════════════════════════════════════════
// 6. 确定性（阴影开启的双会话）
// ═══════════════════════════════════════════════════════════════════════

/// 阴影开启（CSM + 两盏点光投射）的两个同输入完整会话逐帧逐位一致：
/// 帧内相机与物体都在动（阴影贴图、级联拟合、选择键每帧重算）。
#[test]
fn shadow_v2_sessions_are_bitwise_identical() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    const FRAMES: u32 = 30;
    let scene = |frame: u32| {
        let mut world = two_casters_scene(CasterMode::Both);
        world.insert_resource(Camera {
            eye: Vec3::new(frame as f32 * 0.05, 6.5, 9.0),
            target: Vec3::new(0.0, 0.6, 0.0),
            ..Camera::default()
        });
        // 两盏点光逐帧浮动（点光阴影贴图逐帧重画；选择键的候选距离随之变化）
        let mut lights: Vec<Entity> = Vec::new();
        world.query::<(Entity, &PointLight)>(|(e, _)| lights.push(e));
        for (i, entity) in lights.iter().enumerate() {
            if let Some(mut t) = world.get_mut::<Transform>(*entity) {
                t.position.z = (frame as f32 * 0.2 + i as f32 * 2.0).sin() * 0.5;
            }
        }
        propagate_transforms(&mut world);
        world
    };
    let run = || -> Option<Vec<u64>> {
        let mut renderer = OffscreenRenderer::new(256, 144)?;
        let mut hashes = Vec::new();
        for frame in 0..FRAMES {
            renderer.render(&scene(frame));
            hashes.push(fnv1a(&renderer.read_pixels()));
        }
        Some(hashes)
    };
    let Some(a) = run() else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let b = run().expect("第二次会话");
    assert_eq!(a.len(), FRAMES as usize);
    assert_eq!(
        a, b,
        "阴影开启的两个同输入完整会话必须逐帧逐位一致（确定性）"
    );
    println!(
        "双会话确定性：{FRAMES} 帧逐位一致（末帧 {:#018x}）",
        a[FRAMES as usize - 1]
    );
}
