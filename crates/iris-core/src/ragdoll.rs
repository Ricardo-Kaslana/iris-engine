//! # Ragdoll 与死亡混合 v1（球肢链 + 距离关节 + 运动学窗口）
//!
//! 「死亡混合」（death blending）的 v1 形态是**运动学窗口**：角色死亡的
//! 瞬间不直接进入物理模拟，而是先生成**静态**的肢体（动画接管期——游戏
//! /动画照常驱动肢体 [`Transform`](crate::transform::Transform)，或直接
//! 定格死亡姿势），窗口到期后调 [`release_ragdoll`] 把全部肢体翻转为
//! 动体（物理接管期——重力 / 碰撞 / 关节照常生效）。两阶段语义清晰、
//! 各自可实现，中间没有第三种「半物理」状态。
//!
//! # 球肢近似（诚实声明）
//!
//! v1 物理是纯位置物理（[`RigidBody`](crate::physics::RigidBody) 只有线
//! 速度，**无角速度 / 旋转积分**），语义完整的旋转铰链在此模型下无法诚
//! 实实现——因此 ragdoll 刻意用**球肢近似**：头 / 躯干 / 四肢 = 半径各异
//! 的球（[`ColliderShape::Sphere`](crate::physics::ColliderShape::Sphere)），
//! 连通靠既有 [`DistanceJoint`](crate::physics::DistanceJoint)（R36 的硬
//! 距离约束，rest = 生成时两端实际距离——链接拓扑保持，肢体链不散架）。
//! 球是各向同性的：翻滚 / 拖拽 / 堆叠全部语义正确且**不需要旋转**；
//! 「旋转肢体重建」（胶囊 / 盒四肢 + 角速度 + 铰链关节）需要角速度扩展，
//! 留未来轮——本模块不做任何角速度伪装。
//!
//! # 自碰撞免抖（groups / mask 过滤）
//!
//! 全部肢体同组位（[`RAGDOLL_GROUP`]，bit 0）且 `mask` 排除该位
//! （见 [`crate::physics`]「碰撞过滤掩码」）→ **肢体互不碰撞**。理由：
//! 生成时相邻肢体球刻意相切 / 微重叠（链的视觉连续性），若互相求解会
//! 立刻被挤开炸裂。v1 诚实限制：**无肢体间挤压，堆叠全靠地面 / 场景**
//! ——且全部 ragdoll 共用同一个组位，两具 ragdoll 的肢体也互不碰撞
//! （位组按语义层细分留游戏侧 / 未来轮）。与世界的碰撞照常：世界侧
//! 默认全 1 的 `groups` / `mask` 在对称判定
//! `(a.groups & b.mask) != 0 && (b.groups & a.mask) != 0` 下双向认可。
//!
//! # 两阶段语义细节
//!
//! - **动画接管期**：肢体是静态体（`Collider::is_static = true`；生成即
//!   带 [`RigidBody`] 但被静态语义忽略——is_static 取或）。静态体会挡
//!   玩家 / 弹道（合理语义：尸体挡路）；[`DistanceJoint`] 在此期被
//!   [`step_physics`](crate::physics::step_physics) 跳过（挂载体须为动体
//!   ——动画期姿势由游戏驱动，关节无语义）；release 前游戏侧可先驱动
//!   肢体 `Transform` 摆到死亡姿势；
//! - **物理接管期**：[`release_ragdoll`] 把 `is_static` 翻转为 `false`
//!   并写入继承速度 + 可选冲量（`(方向, 强度)`，作用于全部肢体——受击
//!   方向踢飞）。翻转后关节自动生效（距离约束把肢体链拉成刚体拓扑），
//!   重力 / 碰撞 / 接触事件照常。
//!
//! # 前提与边界（诚实清单）
//!
//! - **生成需 `&mut World`**：实体分配是结构性修改（实体分配器非内部
//!   可变，`&World` 无法分配实体——[`World::spawn`](crate::world::World::spawn)
//!   的既有边界）；组件存储是**懒注册**（首次 insert 自动建存储），
//!   无需预注册步骤；
//! - **释放只需 `&World`**：肢体在生成时即带 [`RigidBody`]（动画期被
//!   静态语义忽略），翻转只走组件写锁（`get_mut`）；实体已 despawn /
//!   组件缺席的肢体**静默跳过**（世代安全，不 panic）；
//! - **速度无钳制**：冲量过大可穿墙（v1 无 CCD / 扫掠防穿），调用方
//!   自控强度；
//! - **确定性**：生成序 = `limbs` 定义序、关节求解序 = 组件存储序、无
//!   随机源——同 desc 两次全流程逐位一致（单测守护）。
//!
//! # 死亡接管完整用例
//!
//! ```ignore
//! use iris_core::prelude::*;
//!
//! // 1. 死亡瞬间：按死亡姿势生成静态肢体（动画接管期，照常挡玩家/弹道）
//! let limbs = spawn_ragdoll_kinematic(&mut world, death_pos, &RagdollDesc::humanoid());
//! // ……窗口内游戏侧可继续驱动肢体 Transform（定格 / 播放死亡动画）……
//!
//! // 2. 尸体定型 / 被弹道撞击：物理接管，向受击方向踢飞
//! release_ragdoll(&world, &limbs, hit_velocity, Some((hit_dir, 4.0)));
//!
//! // 3. 此后 step_physics 照常推进：肢体链塌落地面、拓扑保持不散架
//! ```

use glam::Vec3;

use crate::entity::Entity;
use crate::physics::{Collider, ColliderShape, DistanceJoint, RigidBody};
use crate::transform::Transform;
use crate::world::World;

/// 布娃娃肢体专属的碰撞组位（bit 0）：全部肢体 `groups` 同为此位、
/// `mask` 排除此位 → 肢体互不碰撞（自碰撞免抖，见模块文档）。世界侧
/// 默认全 1 的掩码与肢体双向认可，肢体与世界碰撞照常。
pub const RAGDOLL_GROUP: u32 = 1 << 0;

/// 肢体链关节刚度：`1.0` = 硬约束（每轮松弛全额投影回 rest）——链的
/// 拓扑完整性优先，倒地 / 拖拽 / 堆叠下相邻肢体距离不允许漂移。
pub const RAGDOLL_JOINT_STIFFNESS: f32 = 1.0;

/// 肢体链关节轴向阻尼：抑制链在冲击下的持续震荡（轴向相对速度每帧
/// 衰减一半），同时保留切向翻滚的自然感。
pub const RAGDOLL_JOINT_DAMPING: f32 = 0.5;

/// 肢体的线性阻尼（指数速度衰减）：球肢是各向同性的刚体——没有摩擦
/// 与阻尼时被冲量踢飞的尸体会贴地滑出十几米（v1 球-地接触无滚动阻力）。
/// 默认让塌落的肢体短距离滑停；需要「冰面尸体」等特殊语义时字段直改。
pub const RAGDOLL_LIMB_DAMPING: f32 = 0.8;

/// 肢体的（近似库仑）摩擦：落地 / 滑行的切向速度衰减，与阻尼共同把
/// 倒地的链压停。
pub const RAGDOLL_LIMB_FRICTION: f32 = 0.6;

/// 单个肢体球的定义：相对出生点的偏移、半径与弹性（弹性走
/// [`Collider::restitution`]，肢体间已互不碰撞，只在落地 / 撞场景时用）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RagdollLimbDef {
    /// 相对出生点（`pos + offset` = 生成位置，bit 级）。
    pub offset: Vec3,
    /// 球肢半径（米）。
    pub radius: f32,
    /// 弹性系数（0..1）。
    pub restitution: f32,
}

/// 布娃娃描述：肢体球 + 相邻肢体对（生成时按实际距离建
/// [`DistanceJoint`] 硬约束，保持链拓扑）。生成函数不取所有权，同一
/// desc 可反复使用（确定性：同 pos 同 desc 生成逐位一致）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RagdollDesc {
    /// 相对出生点的肢体球（生成序 = 定义序 = 返回实体序）。
    pub limbs: Vec<RagdollLimbDef>,
    /// 相邻肢体对 `(a, b)` → 一条 [`DistanceJoint`]（挂在 b 上、target
    /// 指向 a，双向对称的硬距离约束，方向无语义）。索引须指向 `limbs`。
    pub links: Vec<(usize, usize)>,
}

impl RagdollDesc {
    /// 六球人形（头 / 躯干 / 双手 / 双脚，直立姿势，脚底贴地）：演示与
    /// 测试共用的标准 desc——球肢近似的「人」只需要连通性，姿势语义
    /// （站着死 / 跪着死）由生成位置与动画接管期自由定制。
    pub fn humanoid() -> Self {
        Self {
            limbs: vec![
                // 头
                RagdollLimbDef {
                    offset: Vec3::new(0.0, 1.55, 0.0),
                    radius: 0.18,
                    restitution: 0.3,
                },
                // 躯干
                RagdollLimbDef {
                    offset: Vec3::new(0.0, 1.05, 0.0),
                    radius: 0.30,
                    restitution: 0.1,
                },
                // 左手
                RagdollLimbDef {
                    offset: Vec3::new(-0.42, 1.10, 0.0),
                    radius: 0.14,
                    restitution: 0.2,
                },
                // 右手
                RagdollLimbDef {
                    offset: Vec3::new(0.42, 1.10, 0.0),
                    radius: 0.14,
                    restitution: 0.2,
                },
                // 左脚
                RagdollLimbDef {
                    offset: Vec3::new(-0.16, 0.16, 0.0),
                    radius: 0.16,
                    restitution: 0.0,
                },
                // 右脚
                RagdollLimbDef {
                    offset: Vec3::new(0.16, 0.16, 0.0),
                    radius: 0.16,
                    restitution: 0.0,
                },
            ],
            links: vec![(0, 1), (1, 2), (1, 3), (1, 4), (1, 5)],
        }
    }
}

/// 生成静态肢体（**动画接管期**）：`limbs` 定义序逐个 spawn
/// `Transform + Collider + RigidBody`，再按 `links` 挂
/// [`DistanceJoint`]（rest = 生成时两端实际距离）。
///
/// - 肢体 = 静态体（`is_static = true` + [`RAGDOLL_GROUP`] 组位、mask
///   排除该位）：会挡玩家 / 弹道；互不碰撞（自碰撞免抖，见模块文档）；
/// - 返回实体序 = `desc.limbs` 定义序（后续 [`release_ragdoll`] 的入参）；
/// - **确定性**：同 pos 同 desc 两次生成逐位一致（位置 = `pos + offset`）；
/// - 断言：`radius > 0`、`links` 索引在界内且两端不同（中文断言消息）。
pub fn spawn_ragdoll_kinematic(world: &mut World, pos: Vec3, desc: &RagdollDesc) -> Vec<Entity> {
    let mut limbs = Vec::with_capacity(desc.limbs.len());
    for (index, def) in desc.limbs.iter().enumerate() {
        assert!(
            def.radius > 0.0,
            "肢体 {index} 的半径 {} 须 > 0（非正半径的球肢无碰撞语义）",
            def.radius
        );
        limbs.push(world.spawn((
            Transform::from_translation(pos + def.offset),
            Collider {
                shape: ColliderShape::Sphere { radius: def.radius },
                is_static: true,
                restitution: def.restitution,
                is_sensor: false,
                groups: RAGDOLL_GROUP,
                mask: !RAGDOLL_GROUP,
            },
            RigidBody {
                friction: RAGDOLL_LIMB_FRICTION,
                linear_damping: RAGDOLL_LIMB_DAMPING,
                ..RigidBody::default()
            },
        )));
    }
    for &(a, b) in &desc.links {
        assert!(
            a < limbs.len() && b < limbs.len(),
            "链接 ({a}, {b}) 越界：desc 只有 {} 个肢体（索引须指向 limbs）",
            limbs.len()
        );
        assert!(a != b, "链接 ({a}, {b}) 两端同一肢体（自链接无约束语义）");
        // rest = 生成时两端实际距离（同 pos + offset 的确定性算式）
        let rest = (pos + desc.limbs[b].offset).distance(pos + desc.limbs[a].offset);
        world.insert(
            limbs[b],
            DistanceJoint {
                target: limbs[a],
                rest_length: rest,
                stiffness: RAGDOLL_JOINT_STIFFNESS,
                damping: RAGDOLL_JOINT_DAMPING,
            },
        );
    }
    limbs
}

/// 释放布娃娃（**物理接管期**）：把全部肢体翻转为动体（`is_static =
/// false`）并写入继承速度；`impulse = (方向, 强度)` 时再叠加
/// `方向.normalize() × 强度`（作用于**全部**肢体——受击方向整体踢飞；
/// 零长度方向 = 无冲量，确定性跳过）。
///
/// - 只需 `&World`：翻转走组件写锁（`get_mut`）；实体已 despawn / 缺
///   [`Collider`] / 缺 [`RigidBody`] 的肢体静默跳过（世代安全，不
///   panic）——由 [`spawn_ragdoll_kinematic`] 生成的肢体必然齐备；
/// - 释放后 [`DistanceJoint`] 自动生效（挂载体已为动体），链拓扑保持；
/// - 速度无钳制（v1 无 CCD），冲量强度调用方自控（见模块文档）。
pub fn release_ragdoll(
    world: &World,
    limbs: &[Entity],
    velocity: Vec3,
    impulse: Option<(Vec3, f32)>,
) {
    let kick = impulse
        .map(|(dir, strength)| dir.normalize_or_zero() * strength)
        .unwrap_or(Vec3::ZERO);
    for &limb in limbs {
        if let Some(mut collider) = world.get_mut::<Collider>(limb) {
            collider.is_static = false;
        }
        if let Some(mut body) = world.get_mut::<RigidBody>(limb) {
            body.velocity = velocity + kick;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::step_physics;
    use crate::time::Time;
    use std::time::Duration;

    /// 常规测试步长 60Hz（与 physics 测试同款）。
    const DT: f32 = 1.0 / 60.0;

    /// 测试台：Time 资源 + 推进一帧（确定性驱动 + step_physics）。
    fn make_world() -> World {
        let mut world = World::new();
        world.insert_resource(Time::default());
        world
    }

    fn step(world: &World, dt: f32) {
        world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(dt as f64));
        step_physics(world);
    }

    /// 顶面 y = 0 的大静态平台（地面，与 physics 测试同款）。
    fn spawn_floor(world: &mut World) -> Entity {
        world.spawn((
            Transform::from_xyz(0.0, -1.0, 0.0),
            Collider::static_box(Vec3::new(50.0, 1.0, 50.0)),
        ))
    }

    fn limb_position(world: &World, entity: Entity) -> Vec3 {
        world
            .get::<Transform>(entity)
            .expect("肢体应有 Transform")
            .position
    }

    fn limb_collider(world: &World, entity: Entity) -> Collider {
        *world.get::<Collider>(entity).expect("肢体应有 Collider")
    }

    fn limb_velocity(world: &World, entity: Entity) -> Vec3 {
        world
            .get::<RigidBody>(entity)
            .expect("肢体应有 RigidBody")
            .velocity
    }

    /// 世界里全部距离关节（挂载实体 → 关节值，收集序 = 组件存储序）。
    fn all_joints(world: &World) -> Vec<(Entity, DistanceJoint)> {
        let mut joints = Vec::new();
        world.query::<(Entity, &DistanceJoint)>(|(entity, joint)| {
            joints.push((entity, *joint));
        });
        joints
    }

    // ---- 生成拓扑：limb/link 数量、rest = 生成距离、组位 / mask、静态初值 ----

    /// 六球人形：实体数 / 关节数与 desc 一致，位置 = pos + offset（bit
    /// 级）、rest = 生成时实际距离（同式复算 bit 级相等）、组位与 mask
    /// 正确、静态初值 + 默认刚体。
    #[test]
    fn spawn_topology_matches_desc_bit_exact() {
        let mut world = make_world();
        let pos = Vec3::new(5.0, 0.0, -3.0);
        let desc = RagdollDesc::humanoid();
        let limbs = spawn_ragdoll_kinematic(&mut world, pos, &desc);

        assert_eq!(limbs.len(), desc.limbs.len(), "实体数应等于肢体定义数");

        for (index, (&entity, def)) in limbs.iter().zip(&desc.limbs).enumerate() {
            let collider = limb_collider(&world, entity);
            assert_eq!(
                limb_position(&world, entity),
                pos + def.offset,
                "肢体 {index} 位置应为 pos + offset（bit 级）"
            );
            assert_eq!(
                collider.shape,
                ColliderShape::Sphere { radius: def.radius },
                "肢体 {index} 应为定义半径的球肢"
            );
            assert!(
                collider.is_static,
                "肢体 {index} 初值应为静态（动画接管期）"
            );
            assert_eq!(
                collider.restitution, def.restitution,
                "肢体 {index} 弹性应来自定义"
            );
            assert_eq!(
                collider.groups, RAGDOLL_GROUP,
                "肢体 {index} 应挂在 RAGDOLL_GROUP 组位"
            );
            assert_eq!(
                collider.mask, !RAGDOLL_GROUP,
                "肢体 {index} 的 mask 应排除 RAGDOLL_GROUP（自碰撞免抖）"
            );
            assert_eq!(
                limb_velocity(&world, entity),
                Vec3::ZERO,
                "肢体 {index} 初速度应为零"
            );
        }

        // 链接：数量一致，rest = 生成时实际距离（与生成同式复算，bit 级）
        let joints = all_joints(&world);
        assert_eq!(joints.len(), desc.links.len(), "关节数应等于链接数");
        let by_owner: std::collections::HashMap<u64, DistanceJoint> = joints
            .iter()
            .map(|(entity, joint)| (entity.to_bits(), *joint))
            .collect();
        for &(a, b) in &desc.links {
            let joint = by_owner
                .get(&limbs[b].to_bits())
                .unwrap_or_else(|| panic!("肢体 {b} 上应挂有指向肢体 {a} 的关节"));
            assert_eq!(joint.target, limbs[a], "关节 target 应指向肢体 {a}");
            let want_rest = (pos + desc.limbs[b].offset).distance(pos + desc.limbs[a].offset);
            assert_eq!(
                joint.rest_length, want_rest,
                "链接 ({a}, {b}) 的 rest 应等于生成时实际距离"
            );
            assert_eq!(joint.stiffness, RAGDOLL_JOINT_STIFFNESS, "应为硬约束");
        }
    }

    /// 断言守护：越界链接在生成时 panic（中文断言消息）。
    #[test]
    #[should_panic(expected = "越界")]
    fn spawn_rejects_out_of_range_link() {
        let mut world = make_world();
        let _ = spawn_ragdoll_kinematic(
            &mut world,
            Vec3::ZERO,
            &RagdollDesc {
                limbs: vec![RagdollLimbDef {
                    offset: Vec3::ZERO,
                    radius: 0.2,
                    restitution: 0.0,
                }],
                links: vec![(0, 3)],
            },
        );
    }

    /// 断言守护：自链接（两端同一肢体）在生成时 panic。
    #[test]
    #[should_panic(expected = "自链接")]
    fn spawn_rejects_self_link() {
        let mut world = make_world();
        let _ = spawn_ragdoll_kinematic(
            &mut world,
            Vec3::ZERO,
            &RagdollDesc {
                limbs: vec![RagdollLimbDef {
                    offset: Vec3::ZERO,
                    radius: 0.2,
                    restitution: 0.0,
                }],
                links: vec![(0, 0)],
            },
        );
    }

    /// 断言守护：非正半径的肢体在生成时 panic（中文断言消息）。
    #[test]
    #[should_panic(expected = "半径")]
    fn spawn_rejects_non_positive_radius() {
        let mut world = make_world();
        let _ = spawn_ragdoll_kinematic(
            &mut world,
            Vec3::ZERO,
            &RagdollDesc {
                limbs: vec![RagdollLimbDef {
                    offset: Vec3::ZERO,
                    radius: 0.0,
                    restitution: 0.0,
                }],
                links: Vec::new(),
            },
        );
    }

    // ---- 动画接管期：肢体纹丝不动 + 静态体挡弹道 ----

    /// kinematic 期：物理推进多帧后肢体位置 bit 级不变（静态体不积分）；
    /// 落向躯干的球被静态肢体挡住悬在躯干顶（对照球无肢体阻挡直达地面）
    /// ——「kinematic 期肢体是静态体，会挡玩家 / 弹道」的语义锚。
    #[test]
    fn kinematic_limbs_hold_pose_and_block_bodies() {
        let mut world = make_world();
        spawn_floor(&mut world);
        let spawn_pos = Vec3::new(0.0, 0.0, 0.0);
        let desc = RagdollDesc::humanoid();
        let limbs = spawn_ragdoll_kinematic(&mut world, spawn_pos, &desc);
        let initial: Vec<Vec3> = limbs.iter().map(|&e| limb_position(&world, e)).collect();

        // 被阻挡球：正对躯干顶上方自由落体；对照球：同样落体但下方无肢体
        let drop = |world: &mut World, x: f32| {
            world.spawn((
                Transform::from_xyz(x, 2.0, 0.0),
                Collider::dynamic_sphere(0.2, 0.0),
                RigidBody::default(),
            ))
        };
        let blocked = drop(&mut world, 0.0);
        let control = drop(&mut world, 5.0);

        for _ in 0..48 {
            step(&world, DT);
        }

        for (index, &entity) in limbs.iter().enumerate() {
            assert_eq!(
                limb_position(&world, entity),
                initial[index],
                "kinematic 期肢体 {index} 应 bit 级不动（静态体不积分）"
            );
            assert!(
                limb_collider(&world, entity).is_static,
                "kinematic 期肢体 {index} 应保持静态"
            );
        }
        // 被阻挡球悬在躯干顶（躯干顶 = 1.05 + 0.3，球心静置 ≈ 1.55）；
        // 对照球已落地静置（y ≈ 半径 0.2）
        let blocked_y = limb_position(&world, blocked).y;
        let control_y = limb_position(&world, control).y;
        assert!(
            blocked_y > 1.0,
            "落向躯干的球应被静态肢体挡住（y = {blocked_y}）"
        );
        assert!(
            control_y < 0.4,
            "无肢体阻挡的对照球应已落地（y = {control_y}）"
        );
    }

    // ---- release：翻转 + 速度继承 + 冲量 + 重力接管 ----

    /// release 翻转 is_static、速度 = 继承速度 + 冲量增量（同式复算 bit
    /// 级）；无冲量时速度原样继承；已 despawn 的肢体静默跳过不 panic；
    /// 释放后受重力下坠（位置下降）。
    #[test]
    fn release_flips_static_inherits_velocity_and_applies_impulse() {
        let mut world = make_world();
        let limbs = spawn_ragdoll_kinematic(
            &mut world,
            Vec3::new(0.0, 3.0, 0.0),
            &RagdollDesc::humanoid(),
        );

        let inherit = Vec3::new(0.5, 0.25, 0.0);
        let (dir, strength) = (Vec3::new(3.0, 0.0, 0.0), 2.0f32);
        release_ragdoll(&world, &limbs, inherit, Some((dir, strength)));

        let kick = dir.normalize_or_zero() * strength; // 与实现同式，bit 级对照
        for (index, &entity) in limbs.iter().enumerate() {
            assert!(
                !limb_collider(&world, entity).is_static,
                "释放后肢体 {index} 应为动体"
            );
            assert_eq!(
                limb_velocity(&world, entity),
                inherit + kick,
                "释放后肢体 {index} 速度应为继承 + 冲量增量（bit 级）"
            );
        }

        // 释放后受重力落地：全部肢体下坠（中高空释放，60 帧必然显著下降）
        for _ in 0..60 {
            step(&world, DT);
        }
        for (index, &entity) in limbs.iter().enumerate() {
            let y = limb_position(&world, entity).y;
            assert!(
                y < 1.0,
                "释放后肢体 {index} 应受重力下坠（60 帧后 y = {y}）"
            );
        }

        // 已 despawn 的肢体静默跳过；余下肢体照常翻转与写入
        assert!(world.despawn(limbs[0]), "肢体应仍在场");
        let new_velocity = Vec3::new(-1.0, 0.0, 0.0);
        release_ragdoll(&world, &limbs, new_velocity, None);
        assert!(
            !limb_collider(&world, limbs[1]).is_static,
            "余下肢体应照常翻转"
        );
        assert_eq!(
            limb_velocity(&world, limbs[1]),
            new_velocity,
            "无冲量时速度应原样继承"
        );
    }

    // ---- 倒地链不散架：release 后 240 步拓扑保持 + 塌在地面 + 无爆炸 ----

    /// 释放六球人形（带侧向冲量踢倒）跑 240 步：
    /// - 链上相邻肢体距离全程保持 rest ± 3e-2、终态 ± 1e-2（硬约束生效）；
    /// - 肢体全部塌在地面附近（质心 y 显著下降且 < 0.55），无肢体飞出；
    /// - 全程速度有界（无求解爆炸）；
    /// - 佐证自碰撞免抖：相邻肢体球两两重叠区不产生任何推离（距离恒等
    ///   于 rest 而非半径和——若肢体互碰，链会被挤爆）。
    #[test]
    fn released_chain_collapses_and_stays_connected_for_240_steps() {
        let mut world = make_world();
        spawn_floor(&mut world);
        let limbs = spawn_ragdoll_kinematic(
            &mut world,
            Vec3::new(0.0, 0.0, 0.0),
            &RagdollDesc::humanoid(),
        );
        let desc = RagdollDesc::humanoid();
        let rests: Vec<f32> = desc
            .links
            .iter()
            .map(|&(a, b)| {
                (Vec3::ZERO + desc.limbs[b].offset).distance(Vec3::ZERO + desc.limbs[a].offset)
            })
            .collect();

        release_ragdoll(
            &world,
            &limbs,
            Vec3::new(0.3, 0.0, 0.0),
            Some((Vec3::X, 3.0)),
        );

        const TRANSIENT_TOLERANCE: f32 = 3e-2;
        const FINAL_TOLERANCE: f32 = 1e-2;
        const SPEED_CAP: f32 = 25.0;
        let mut max_speed = 0.0f32;
        let mut max_deviation = 0.0f32;
        for _ in 0..240 {
            step(&world, DT);
            for &entity in &limbs {
                let speed = limb_velocity(&world, entity).length();
                assert!(speed < SPEED_CAP, "肢体速度 {speed} 应有界（无爆炸）");
                max_speed = max_speed.max(speed);
            }
            for (&(a, b), &rest) in desc.links.iter().zip(&rests) {
                let dist =
                    limb_position(&world, limbs[a]).distance(limb_position(&world, limbs[b]));
                let deviation = (dist - rest).abs();
                assert!(
                    deviation <= TRANSIENT_TOLERANCE,
                    "链接 ({a}, {b}) 距离 {dist} 应保持 rest {rest} ± {TRANSIENT_TOLERANCE}"
                );
                max_deviation = max_deviation.max(deviation);
            }
        }
        assert!(
            max_deviation <= TRANSIENT_TOLERANCE,
            "全程最大链距偏差 {max_deviation} 应有界（约束全程生效）"
        );

        // 终态：链距收敛回 rest、质心塌在地面附近、无肢体飞出 / 穿地
        let mut com = Vec3::ZERO;
        for &entity in &limbs {
            let position = limb_position(&world, entity);
            com += position;
            assert!(
                position.y > 0.0 && position.y < 1.0 && position.x.abs() < 10.0,
                "肢体应塌在地面附近且未飞出，实际 {position:?}"
            );
        }
        com /= limbs.len() as f32;
        for (&(a, b), &rest) in desc.links.iter().zip(&rests) {
            let dist = limb_position(&world, limbs[a]).distance(limb_position(&world, limbs[b]));
            assert!(
                (dist - rest).abs() <= FINAL_TOLERANCE,
                "终态链接 ({a}, {b}) 距离 {dist} 应收敛回 rest {rest} ± {FINAL_TOLERANCE}"
            );
        }
        assert!(
            com.y < 0.55,
            "终态质心 y = {} 应显著低于直立姿势（塌地）",
            com.y
        );
        assert!(
            max_speed > 1.0,
            "冲量应真实驱动过肢体（最大速度 {max_speed}，测试非空转）"
        );
    }

    // ---- 确定性：同 desc 两次全流程逐位一致 ----

    /// 同 desc 两次构建（生成 → kinematic 30 步 → release → 物理 210 步）
    /// 的全部肢体位置逐采样 bit 级一致；且轨迹非平凡（确有运动）。
    #[test]
    fn ragdoll_pipeline_is_deterministic_across_runs() {
        fn build() -> (World, Vec<Entity>) {
            let mut world = make_world();
            spawn_floor(&mut world);
            let limbs = spawn_ragdoll_kinematic(
                &mut world,
                Vec3::new(0.0, 0.0, 0.0),
                &RagdollDesc::humanoid(),
            );
            (world, limbs)
        }

        let (world_a, limbs_a) = build();
        let (world_b, limbs_b) = build();
        let mut trace_a: Vec<Vec3> = Vec::new();
        let mut trace_b: Vec<Vec3> = Vec::new();
        for frame in 0..240 {
            if frame == 30 {
                release_ragdoll(
                    &world_a,
                    &limbs_a,
                    Vec3::new(0.4, 0.0, 0.0),
                    Some((Vec3::X, 2.5)),
                );
                release_ragdoll(
                    &world_b,
                    &limbs_b,
                    Vec3::new(0.4, 0.0, 0.0),
                    Some((Vec3::X, 2.5)),
                );
            }
            step(&world_a, DT);
            step(&world_b, DT);
            for (pa, pb) in limbs_a.iter().zip(&limbs_b) {
                trace_a.push(limb_position(&world_a, *pa));
                trace_b.push(limb_position(&world_b, *pb));
            }
        }
        assert_eq!(trace_a, trace_b, "同 desc 两次全流程应逐采样 bit 级一致");
        assert!(
            trace_a[0] != *trace_a.last().expect("轨迹非空"),
            "全流程应有运动（测试非空转）"
        );
    }
}
