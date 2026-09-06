//! 两骨手部 IK（v4）：动画（与足部 IK）之后的持枪吸附层——把
//! 肩-肘-手两骨链的手位拉到武器握把（边走边瞄准、持枪手位吸附），
//! 肘按余弦定理下弯、pole 向量定弯曲方向。复用足部的
//! [`crate::foot_ik::solve_two_bone_ik`] 纯函数与「世界系解 → 最小
//! 旋转弧回写局部变换」的两阶段模式（见 [`update_hand_ik`]）。
//!
//! # 与足部 IK 的目标差异（为何不能共用一套落地查询）
//!
//! - **目标来源**：足部目标 = 脚踝正下方射线打静态地面（环境量）；
//!   手部目标 = [`IkTarget`] 实体的 `GlobalTransform` 全局平移
//!   （**武器挂点**，玩法量）——按实体 id 查全局位，不做任何射线。
//!   目标实体失效 / 无 `GlobalTransform` / 自身 [`Visibility::HIDDEN`]
//!   → 本帧放弃该臂修正（不写任何变换）。诚实边界：`GlobalTransform`
//!   是上一帧层级传播的产物，目标位有一帧滞后——挂点通常独立于手骨
//!   子树（武器根 / 躯干挂点），滞后在吸附收敛语义下不可感知；
//!   **挂点若位于手骨链子树内会形成「手追挂点、挂点随手」的一帧
//!   滞后反馈**（装配禁忌，见下方用例）；
//! - **不可达门的容差带**：足部目标超出可达域的唯一解释是射线采样到
//!   非地面几何（门框 / 墙顶），钳制解算必错 → 全域放弃。手部目标
//!   是玩法 authored 的握把位，接近全臂长的目标是「伸臂取枪」的
//!   正常表现——超出全臂长但在 [`HandIk::reach_threshold`] 容差带内
//!   （`(upper + lower, upper + lower + reach_threshold]`）仍解算，
//!   拉伸至全长贴向握把；**超出容差带或近于 `|upper − lower|`
//!   才视为异常目标，本帧放弃**（不写任何变换——把臂拉向异常目标
//!   产生「臂拉成直线指天」式变形，与足部的过门教训同源）；
//! - **无平地门**：足部有 1 mm 平地门保平地姿势 bit 级不动；手部
//!   目标逐帧随武器移动，没有「平地」概念，回写天然幂等（同姿势
//!   同目标重解恒得同局部变换）。
//!
//! # 持枪用例（双臂装配模式）
//!
//! 单臂：`HandIk` 与节点映射（[`crate::foot_ik::IkRig`] 或
//! [`crate::animation::AnimationPlayer`]，二选一，语义与足部完全同款）
//! 同挂模型根实体。**双臂（左右手各一条链）**：每臂一条配置实体，
//! 挂 `(IkRig(该臂三节点), HandIk)`——组件存储每实体单槽，双臂
//! 不可能挤在同一实体上；两条链节点不重叠，互不干扰（单测守护）。
//! 目标实体 = 武器握把（挂 [`IkTarget`] 的实体），**不要挂在手骨
//! 子树内**（反馈环，见上文）。
//!
//! ```ignore
//! // 右臂：配置实体 + 纯 IK 装配映射 + 握把目标
//! let grip = world.spawn((Transform::from_xyz(0.3, 1.4, -0.5), IkTarget));
//! let cfg = world.spawn((Transform::IDENTITY,));
//! world.insert(cfg, IkRig::new(vec![Some(r_shoulder), Some(r_elbow), Some(r_hand)]));
//! world.insert(cfg, HandIk { target: Some(grip), ..Default::default() });
//! // 每帧 DefaultPlugins 自动执行：update_animations → update_foot_ik
//! //   → update_hand_ik → 传播
//! ```
//!
//! # 编排（为何在 update_foot_ik 之后、propagate_transforms 之前）
//!
//! 同足部：修正对象是**本帧局部关节姿势**（晚于动画），写回的也是
//! 局部 `Transform`（早于层级传播，当帧全局生效）。注册在
//! `update_foot_ik` **之后**：持枪手位是比脚落地更高频的视觉焦点，
//! 让手 IK 读到的肩肘世界位已含足部修正后的完整本帧姿势（两段 IK
//! 作用于同一份局部姿势依次生效，顺序确定 = 结果确定）。
//!
//! # 前提与诚实边界（与足部同款，详见 foot_ik 模块文档）
//!
//! - 三节点（shoulder / elbow / hand）须 `ChildOf` 直连成链
//!   （elbow 的父 = shoulder、hand 的父 = elbow），不满足时
//!   `debug_assert!` 并跳过（release 静默）；
//! - 链上节点 scale 恒 1（骨长按当前世界姿势实际距离取用，节点
//!   scale 会缩水骨长）；均匀缩放假设（旋转回写沿父链按四元数复合）；
//! - shoulder 旋转带动其全部子级——手臂链挂独立节点（装配惯例）；
//! - 不做手部接触状态机（何时吸附 / 松开由游戏侧置空
//!   [`HandIk::target`] 控制）；
//! - 确定性：全程纯函数 + 固定遍历序，同输入两次调用逐位一致
//!   （单测守护）。

use glam::{Mat4, Quat, Vec3};
use iris_core::prelude::{Entity, GlobalTransform, Transform};
use iris_core::world::World;

use crate::animation::AnimationPlayer;
use crate::foot_ik::{IkRig, MIN_BONE_LENGTH, chain_world, solve_two_bone_ik};
use crate::visibility::Visibility;

/// 手部 IK 目标标记（v4）：挂在**武器握把**实体上，[`update_hand_ik`]
/// 消费其 `GlobalTransform` 的全局平移作为手位目标。
///
/// 目标实体失效 / 无 `GlobalTransform` / 自身 `Visibility::HIDDEN`
/// → 本帧放弃（语义见模块文档「与足部 IK 的目标差异」）。装配禁忌：
/// 握把实体不要挂在手骨链子树内（一帧滞后反馈环，模块文档有推导）。
#[derive(Clone, Copy, Debug, Default, PartialEq, iris_core::Component)]
pub struct IkTarget;

/// 手部两骨 IK 配置组件（v4）：声明哪条肩-肘-手两骨链做 IK + 握把
/// 目标。节点映射来源与足部同款：[`IkRig`]（优先）或
/// [`AnimationPlayer::node_entities`](crate::animation::AnimationPlayer)
/// 二选一，`*_node` 为其下标；三节点须直连成链。每实体独立开关：
/// 没挂本组件的实体零参与。
#[derive(Clone, Debug, iris_core::Component)]
pub struct HandIk {
    /// 肩关节节点（`node_entities` 下标）。
    pub shoulder_node: usize,
    /// 肘关节节点（须为 shoulder 的子级）。
    pub elbow_node: usize,
    /// 手节点（须为 elbow 的子级）。
    pub hand_node: usize,
    /// 肘弯方向（世界系）——人类手臂自然后摆，默认 +Z（引擎前方
    /// = −Z，肘弯朝后）；持枪装配按需调整（如 −Y 压肘）。与骨干平行
    /// 的退化兜底见 [`solve_two_bone_ik`]。
    pub pole: Vec3,
    /// 手位目标 = 挂 [`IkTarget`] 的实体（武器握把位）；`None` =
    /// 不启用本臂 IK。目标实体失效 / 无 `GlobalTransform` / 隐藏
    /// → 本帧放弃（语义见模块文档）。
    pub target: Option<Entity>,
    /// 不可达容差带（米）：目标距肩超出全臂长但未超出
    /// `upper + lower + reach_threshold` 时仍解算（拉伸至全长贴向
    /// 握把——「伸臂取枪」的正常表现）；再超出（或近于
    /// `|upper − lower|`）视为异常目标，本帧放弃（门的完整语义见
    /// 模块文档「与足部 IK 的目标差异」）。
    pub reach_threshold: f32,
}

impl Default for HandIk {
    /// 常规装配：节点 0/1/2 = shoulder/elbow/hand，肘弯 +Z（朝后），
    /// 容差带 0.1 m，目标未设置（不启用）。
    fn default() -> Self {
        Self {
            shoulder_node: 0,
            elbow_node: 1,
            hand_node: 2,
            pole: Vec3::Z,
            target: None,
            reach_threshold: 0.1,
        }
    }
}

/// 手部 IK 每帧系统：对每个挂有 [`HandIk`] 且节点映射齐备（[`IkRig`]
/// 或 [`AnimationPlayer`]，二选一）的实体，把声明的两骨链手位拉到
/// [`IkTarget`] 实体的全局平移（目标差异、不可达门与边界见模块文档）。
///
/// 只读世界签名（`&World`）：组件写访问走世界内部的尝试型锁，注册为
/// **并行系统**（访问集：写 Transform，读 HandIk / IkTarget /
/// IkRig / AnimationPlayer / ChildOf / GlobalTransform /
/// Visibility）。两阶段：只读解算收集回写任务 → 统一写局部变换
/// （照 `update_foot_ik` 模式）。本组件无可变状态（骨长逐帧取实际
/// 距离、无基准捕获），阶段二只写 `Transform`。
pub fn update_hand_ik(world: &World) {
    // 阶段一（只读解算）：每臂产出「shoulder/elbow 局部旋转 + hand
    // 局部位置」回写任务
    let mut jobs: Vec<(Entity, Entity, Entity, Quat, Quat, Vec3)> = Vec::new();
    world.query::<(Entity, &HandIk, Option<&AnimationPlayer>, Option<&IkRig>)>(
        |(_entity, ik, player, rig)| {
            // 目标解析：None / 实体失效 / 无 GlobalTransform / 隐藏
            // → 本帧放弃（模块文档「与足部 IK 的目标差异」）
            let Some(target_entity) = ik.target else {
                return;
            };
            let Some(target) = world
                .get::<GlobalTransform>(target_entity)
                .map(|global| global.position())
            else {
                return;
            };
            if world
                .get::<Visibility>(target_entity)
                .is_some_and(|visibility| !visibility.visible)
            {
                return;
            }
            // 节点解析：IkRig 优先，回退 AnimationPlayer（与足部同款
            // 二选一语义，见 foot_ik 模块文档「节点映射」）
            let resolve = |index: usize| -> Option<Entity> {
                let mapped = rig
                    .and_then(|rig| rig.node_entities.get(index).copied().flatten())
                    .or_else(|| {
                        player
                            .and_then(|player| player.node_entities.get(index).copied().flatten())
                    });
                match mapped {
                    Some(entity) => Some(entity),
                    None => {
                        debug_assert!(
                            false,
                            "HandIk 节点索引越界或未映射到实体：{index}（IkRig /                              AnimationPlayer 至少挂一个且表长覆盖该下标）"
                        );
                        None
                    }
                }
            };
            let Some(shoulder_entity) = resolve(ik.shoulder_node) else {
                return;
            };
            let Some(elbow_entity) = resolve(ik.elbow_node) else {
                return;
            };
            let Some(hand_entity) = resolve(ik.hand_node) else {
                return;
            };
            // 链契约：elbow 的父 = shoulder、hand 的父 = elbow
            if world.parent(elbow_entity) != Some(shoulder_entity)
                || world.parent(hand_entity) != Some(elbow_entity)
            {
                debug_assert!(
                    false,
                    "HandIk 三节点须直连成链（elbow 的父 = shoulder、hand 的父 = elbow）"
                );
                return;
            }
            let Some((shoulder_world, shoulder_world_rot)) = chain_world(world, shoulder_entity)
            else {
                return;
            };
            let Some((elbow_world, elbow_world_rot)) = chain_world(world, elbow_entity) else {
                return;
            };
            let Some(hand_world) = chain_world(world, hand_entity).map(|(m, _)| m) else {
                return;
            };
            let shoulder = shoulder_world.w_axis.truncate();
            let elbow = elbow_world.w_axis.truncate();
            let hand = hand_world.w_axis.truncate();
            // 骨长取当前世界姿势的实际距离（与足部同款，保留装配比例）
            let upper = (elbow - shoulder).length();
            let lower = (hand - elbow).length();
            if upper <= MIN_BONE_LENGTH || lower <= MIN_BONE_LENGTH {
                return;
            }
            // 不可达回退门（容差带语义见模块文档）：目标距肩近于
            // |upper − lower| 或超出 upper + lower + reach_threshold
            // → 本帧放弃该臂修正（不写任何变换）；容差带内拉伸贴向目标
            let target_dist = (target - shoulder).length();
            if target_dist < (upper - lower).abs()
                || target_dist > upper + lower + ik.reach_threshold
            {
                return;
            }
            // 世界系两骨解（与足部共用纯函数）
            let (hand_new, elbow_new) =
                solve_two_bone_ik(shoulder, target, upper, lower, ik.pole);
            // 旋转回写：与 update_foot_ik 逐位同构——骨方向旧 → 新的
            // 最小旋转弧，除以父级旋转反推局部旋转；hand 局部旋转不动、
            // 按肘关节新世界矩阵的逆回写局部位置把手精确放到目标
            let shoulder_to_elbow_old = (elbow - shoulder) / upper;
            let shoulder_to_elbow_new = (elbow_new - shoulder) / upper;
            let elbow_to_hand_old = (hand - elbow) / lower;
            let elbow_to_hand_new = (hand_new - elbow_new) / lower;
            let arc_shoulder =
                Quat::from_rotation_arc(shoulder_to_elbow_old, shoulder_to_elbow_new);
            let arc_elbow = Quat::from_rotation_arc(elbow_to_hand_old, elbow_to_hand_new);
            let Some(shoulder_local_rot) = world.get::<Transform>(shoulder_entity).map(|t| t.rotation)
            else {
                return;
            };
            let parent_rot = shoulder_world_rot * shoulder_local_rot.conjugate();
            let shoulder_world_rot_new = arc_shoulder * shoulder_world_rot;
            let elbow_world_rot_new = arc_elbow * elbow_world_rot;
            let shoulder_local_rot_new = parent_rot.conjugate() * shoulder_world_rot_new;
            let elbow_local_rot_new = shoulder_world_rot_new.conjugate() * elbow_world_rot_new;
            let Some(shoulder_local) = world.get::<Transform>(shoulder_entity).map(|t| *t) else {
                return;
            };
            let Some(elbow_local) = world.get::<Transform>(elbow_entity).map(|t| *t) else {
                return;
            };
            let parent_matrix = match world.parent(shoulder_entity) {
                Some(parent) => match chain_world(world, parent) {
                    Some((matrix, _)) => matrix,
                    None => return,
                },
                None => Mat4::IDENTITY,
            };
            let shoulder_matrix_new = parent_matrix
                * Mat4::from_scale_rotation_translation(
                    shoulder_local.scale,
                    shoulder_local_rot_new,
                    shoulder_local.position,
                );
            let elbow_matrix_new = shoulder_matrix_new
                * Mat4::from_scale_rotation_translation(
                    elbow_local.scale,
                    elbow_local_rot_new,
                    elbow_local.position,
                );
            let hand_local_pos_new =
                (elbow_matrix_new.inverse() * hand_new.extend(1.0)).truncate();
            jobs.push((
                shoulder_entity,
                elbow_entity,
                hand_entity,
                shoulder_local_rot_new,
                elbow_local_rot_new,
                hand_local_pos_new,
            ));
        },
    );

    // 阶段二（写入）：shoulder/elbow 局部旋转 + hand 局部位置
    // （其余字段不动；骨长严格保持——shoulder/elbow 局部位置不动）
    for (shoulder_entity, elbow_entity, hand_entity, shoulder_rot, elbow_rot, hand_pos) in jobs {
        if let Some(mut transform) = world.get_mut::<Transform>(shoulder_entity) {
            transform.rotation = shoulder_rot;
        }
        if let Some(mut transform) = world.get_mut::<Transform>(elbow_entity) {
            transform.rotation = elbow_rot;
        }
        if let Some(mut transform) = world.get_mut::<Transform>(hand_entity) {
            transform.position = hand_pos;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core::hierarchy::propagate_transforms;

    /// 组装「root → shoulder → elbow → hand」直连链（臂长 0.5 + 0.5，
    /// root 在 y 1.5：肩世界位 (0, 1.5, 0)）+ 播放器（node 0/1/2 =
    /// shoulder/elbow/hand）+ 默认 HandIk（pole 覆写为 −X）。
    fn arm(root_pos: Vec3) -> (World, Entity, Entity, Entity, Entity) {
        let mut world = World::new();
        let root = world.spawn((Transform::from_xyz(root_pos.x, root_pos.y, root_pos.z),));
        let shoulder = world.spawn((Transform::IDENTITY,));
        let elbow = world.spawn((Transform::from_xyz(0.0, -0.5, 0.0),));
        let hand = world.spawn((Transform::from_xyz(0.0, -0.5, 0.0),));
        world.add_child(root, shoulder);
        world.add_child(shoulder, elbow);
        world.add_child(elbow, hand);
        let player = AnimationPlayer::new(vec![], vec![Some(shoulder), Some(elbow), Some(hand)]);
        world.insert(root, player);
        world.insert(
            root,
            HandIk {
                pole: Vec3::NEG_X,
                ..HandIk::default()
            },
        );
        (world, root, shoulder, elbow, hand)
    }

    /// 握把目标实体：只带 `GlobalTransform`（测试直接插入，模拟上一帧
    /// 传播产物）。
    fn grip(world: &mut World, pos: Vec3) -> Entity {
        let entity = world.spawn((IkTarget,));
        world.insert(
            entity,
            GlobalTransform::from_transform(Transform::from_xyz(pos.x, pos.y, pos.z)),
        );
        entity
    }

    fn joint_bits(world: &World, joints: &[Entity]) -> Vec<([u32; 3], [u32; 4])> {
        joints
            .iter()
            .map(|&joint| {
                let transform = *world.get::<Transform>(joint).unwrap();
                (
                    [
                        transform.position.x.to_bits(),
                        transform.position.y.to_bits(),
                        transform.position.z.to_bits(),
                    ],
                    [
                        transform.rotation.x.to_bits(),
                        transform.rotation.y.to_bits(),
                        transform.rotation.z.to_bits(),
                        transform.rotation.w.to_bits(),
                    ],
                )
            })
            .collect()
    }

    // ---- 解析解：可达域内的握把吸附 ----

    /// 握把在 (0, 1.5, −0.6)（距肩 0.6，可达域内）：手世界位精确落上
    /// 握把；肘按余弦定理落 (−0.4, 1.5, −0.3)（pole −X 定弯向），
    /// 骨长严格保持 0.5 + 0.5。
    #[test]
    fn hand_ik_reaches_grip_analytically() {
        let (mut world, _root, shoulder, elbow, hand) = arm(Vec3::new(0.0, 1.5, 0.0));
        let target = grip(&mut world, Vec3::new(0.0, 1.5, -0.6));
        world.get_mut::<HandIk>(_root).unwrap().target = Some(target);
        update_hand_ik(&world);
        propagate_transforms(&mut world);
        let hand_pos = world.get::<GlobalTransform>(hand).unwrap().position();
        assert!(
            (hand_pos - Vec3::new(0.0, 1.5, -0.6)).length() < 1e-4,
            "手应精确吸附到握把，实际 {hand_pos:?}"
        );
        let elbow_pos = world.get::<GlobalTransform>(elbow).unwrap().position();
        // d = 0.6 → a = (0.36 + 0.25 − 0.25)/1.2 = 0.3、h = √(0.25 − 0.09) = 0.4
        let expected_elbow = Vec3::new(-0.4, 1.5, -0.3);
        assert!(
            (elbow_pos - expected_elbow).length() < 1e-4,
            "肘应按余弦定理落在 {expected_elbow:?}，实际 {elbow_pos:?}"
        );
        let shoulder_pos = world.get::<GlobalTransform>(shoulder).unwrap().position();
        assert!(
            ((elbow_pos - shoulder_pos).length() - 0.5).abs() < 1e-4
                && ((hand_pos - elbow_pos).length() - 0.5).abs() < 1e-4,
            "骨长应严格保持 0.5 + 0.5"
        );
    }

    // ---- 不可达回退门 ----

    /// 目标距肩 1.5 > 全臂长 1.0 + 容差带 0.1：异常目标 → 本帧放弃，
    /// 三关节 Transform 逐位不动（不拉伸、不指天）。
    #[test]
    fn hand_ik_unreachable_gives_up_bitwise() {
        let (mut world, root, shoulder, elbow, hand) = arm(Vec3::new(0.0, 1.5, 0.0));
        let target = grip(&mut world, Vec3::new(0.0, 1.5, -1.5));
        world.get_mut::<HandIk>(root).unwrap().target = Some(target);
        let joints = vec![shoulder, elbow, hand];
        let before = joint_bits(&world, &joints);
        update_hand_ik(&world);
        assert_eq!(
            joint_bits(&world, &joints),
            before,
            "超容差带目标应放弃本帧修正：姿势 bit 级不动"
        );
    }

    /// 容差带内（d = 1.05 ∈ (1.0, 1.1]）：仍解算，拉伸至全长贴向
    /// 握把（「伸臂取枪」的正常表现，与足部全域放弃的刻意差异）。
    #[test]
    fn hand_ik_tolerance_band_stretches_toward_target() {
        let (mut world, root, _shoulder, _elbow, hand) = arm(Vec3::new(0.0, 1.5, 0.0));
        let target = grip(&mut world, Vec3::new(0.0, 1.5, -1.05));
        world.get_mut::<HandIk>(root).unwrap().target = Some(target);
        update_hand_ik(&world);
        propagate_transforms(&mut world);
        let hand_pos = world.get::<GlobalTransform>(hand).unwrap().position();
        let expected = Vec3::new(0.0, 1.5, -1.0);
        assert!(
            (hand_pos - expected).length() < 1e-4,
            "容差带内应拉伸至全长贴向目标 {expected:?}，实际 {hand_pos:?}"
        );
    }

    // ---- 目标缺席 / 隐藏 ----

    /// 目标为 None（默认）与指向无 `GlobalTransform` 的实体：两种缺席
    /// 语义都本帧放弃、姿势逐位不动。
    #[test]
    fn hand_ik_missing_target_skips() {
        let (mut world, root, shoulder, elbow, hand) = arm(Vec3::new(0.0, 1.5, 0.0));
        let joints = vec![shoulder, elbow, hand];
        let before = joint_bits(&world, &joints);
        update_hand_ik(&world);
        assert_eq!(joint_bits(&world, &joints), before, "target = None 应跳过");
        // 目标实体存在但无 GlobalTransform（未入层级的裸实体）
        let bare = world.spawn((IkTarget,));
        world.get_mut::<HandIk>(root).unwrap().target = Some(bare);
        update_hand_ik(&world);
        assert_eq!(
            joint_bits(&world, &joints),
            before,
            "目标实体无 GlobalTransform 应跳过"
        );
    }

    /// 目标实体自身 `Visibility::HIDDEN`：本帧放弃（隐藏的握把不是
    /// 有效目标）。
    #[test]
    fn hand_ik_hidden_target_skips() {
        let (mut world, root, shoulder, elbow, hand) = arm(Vec3::new(0.0, 1.5, 0.0));
        let target = grip(&mut world, Vec3::new(0.0, 1.5, -0.6));
        world.insert(target, Visibility::HIDDEN);
        world.get_mut::<HandIk>(root).unwrap().target = Some(target);
        let joints = vec![shoulder, elbow, hand];
        let before = joint_bits(&world, &joints);
        update_hand_ik(&world);
        assert_eq!(
            joint_bits(&world, &joints),
            before,
            "隐藏目标应本帧放弃：姿势 bit 级不动"
        );
    }

    // ---- 双臂对称 ----

    /// 左右臂各一条配置实体（IkRig + HandIk）：两手同时吸附各自的
    /// 握把，互不干扰（组件存储每实体单槽 → 双臂必然分实体配置）。
    #[test]
    fn hand_ik_dual_arms_independent() {
        let mut world = World::new();
        let root = world.spawn((Transform::from_xyz(0.0, 1.5, 0.0),));
        let left_grip = grip(&mut world, Vec3::new(-0.3, 1.5, -0.6));
        let right_grip = grip(&mut world, Vec3::new(0.3, 1.5, -0.6));
        let mut build_arm = |offset_x: f32, pole: Vec3, target: Entity| {
            let shoulder = world.spawn((Transform::from_xyz(offset_x, 0.0, 0.0),));
            let elbow = world.spawn((Transform::from_xyz(0.0, -0.5, 0.0),));
            let hand = world.spawn((Transform::from_xyz(0.0, -0.5, 0.0),));
            world.add_child(root, shoulder);
            world.add_child(shoulder, elbow);
            world.add_child(elbow, hand);
            // 配置实体：IkRig（该臂三节点映射）+ HandIk（目标与参数）
            let cfg = world.spawn((Transform::IDENTITY,));
            world.insert(
                cfg,
                IkRig::new(vec![Some(shoulder), Some(elbow), Some(hand)]),
            );
            world.insert(
                cfg,
                HandIk {
                    pole,
                    target: Some(target),
                    ..HandIk::default()
                },
            );
            hand
        };
        let left_hand = build_arm(-0.3, Vec3::NEG_X, left_grip);
        let right_hand = build_arm(0.3, Vec3::X, right_grip);
        update_hand_ik(&world);
        propagate_transforms(&mut world);
        let left_pos = world.get::<GlobalTransform>(left_hand).unwrap().position();
        let right_pos = world.get::<GlobalTransform>(right_hand).unwrap().position();
        assert!(
            (left_pos - Vec3::new(-0.3, 1.5, -0.6)).length() < 1e-4,
            "左手应吸附左握把，实际 {left_pos:?}"
        );
        assert!(
            (right_pos - Vec3::new(0.3, 1.5, -0.6)).length() < 1e-4,
            "右手应吸附右握把，实际 {right_pos:?}"
        );
    }

    // ---- 确定性 ----

    #[test]
    fn hand_ik_is_bitwise_deterministic() {
        let build = || {
            let (mut world, root, shoulder, elbow, hand) = arm(Vec3::new(0.0, 1.5, 0.0));
            let target = grip(&mut world, Vec3::new(0.0, 1.5, -0.6));
            world.get_mut::<HandIk>(root).unwrap().target = Some(target);
            (world, vec![shoulder, elbow, hand])
        };
        let (world_a, joints_a) = build();
        let (world_b, joints_b) = build();
        for _ in 0..3 {
            update_hand_ik(&world_a);
            update_hand_ik(&world_b);
            assert_eq!(
                joint_bits(&world_a, &joints_a),
                joint_bits(&world_b, &joints_b),
                "同配置两次手 IK 应逐位一致"
            );
        }
    }

    /// 仅 debug 构建：链契约走 `debug_assert!`（release 下静默跳过是
    /// 模块文档明文语义），release 编译掉断言后不应再测 panic。
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "直连成链")]
    fn hand_ik_broken_chain_panics_in_debug() {
        // shoulder / elbow / hand 未建立父子关系：链契约 debug_assert
        // （目标有效是前置——缺席目标在链契约检查之前就放弃了）
        let mut world = World::new();
        let root = world.spawn((Transform::IDENTITY,));
        let shoulder = world.spawn((Transform::IDENTITY,));
        let elbow = world.spawn((Transform::from_xyz(0.0, -0.5, 0.0),));
        let hand = world.spawn((Transform::from_xyz(0.0, -0.5, 0.0),));
        let target = world.spawn((IkTarget,));
        world.insert(
            target,
            GlobalTransform::from_transform(Transform::from_xyz(0.0, 1.5, -0.6)),
        );
        let player = AnimationPlayer::new(vec![], vec![Some(shoulder), Some(elbow), Some(hand)]);
        world.insert(root, player);
        world.insert(
            root,
            HandIk {
                target: Some(target),
                ..HandIk::default()
            },
        );
        update_hand_ik(&world);
    }
}
