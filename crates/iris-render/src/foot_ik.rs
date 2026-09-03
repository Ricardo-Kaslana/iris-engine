//! 两骨足部 IK（v3）：动画之后的落地修正层——把 hip-knee-ankle 两骨链的
//! 脚踝拉到脚下地面（斜坡不悬空、平地不插地），膝盖按余弦定理下弯、
//! pole 向量定弯曲方向。
//!
//! # 编排（为何在 update_animations 之后、propagate_transforms 之前）
//!
//! IK 修正的对象是**动画刚写出的本帧局部关节姿势**——晚于动画才能读到
//! 本帧姿势（GlobalTransform 是上一帧传播的产物，不可用）；IK 写回的也是
//! 局部 `Transform`——早于层级传播才能让修正当帧全局生效。DefaultPlugins
//! 中注册在 [`crate::animation::update_animations`] 之后（同一 Update 阶段、
//! 写 Transform 访问集重叠 ⇒ 调度器按注册序分批），层级传播在 PostUpdate
//! 天然晚于本系统。
//!
//! # 落地查询（静态体 only）
//!
//! 从脚踝当前世界位置上抬 [`FootIk::ray_up_offset`] 处竖直向下射线
//! （[`raycast_statics`]），收集语义与 `iris_core::physics` 的
//! `resolve_against_statics` 一致：`Collider::is_static` 或仅 Collider 无
//! `RigidBody`（组件缺席 = 静态）。**动态体不参与 IK 落地**——脚踝踩的
//! 「地面」必须是稳定的支撑面，跟随动态体会把运动物体的表面抖动喂进
//! 关节链；命中范围 = `ray_up_offset + max_ground_drop`（脚下探超出容差
//! 视为腾空，不修正）。命中点即脚踝目标：竖直射线 ⇒ 命中点 = 脚踝正下方
//! 的地面点，脚踝低于地面（插地）时目标在上方 → 上拉，悬空时 → 下探。
//!
//! # 解算与回写（完整两骨旋转解）
//!
//! 1. **世界系两骨解**（[`solve_two_bone_ik`]，纯函数）：骨长取**当前世界
//!    姿势的实际距离**（保留装配比例），余弦定理解膝角、pole 投影定弯曲
//!    方向，返回 `(ankle, knee)` 世界位——不可达拉伸至全长、过近钳制到
//!    最小距离（双腿长差）。
//! 2. **旋转回写**：髋骨 / 膝骨的世界方向从 `hip→knee / knee→ankle` 的
//!    旧方向旋到新方向（`Quat::from_rotation_arc` 最小旋转弧），再除以
//!    各自父级旋转反推**局部旋转**；foot 局部旋转不动、按膝关节新世界
//!    矩阵的逆回写**局部位置**把踝关节精确放到目标上。hip / knee 的局部
//!    位置不动 ⇒ 骨长严格保持。
//! 3. **平地门**：地面高差 ≤ [`GROUND_EPSILON`]（1 mm，远小于 ±2 cm
//!    验收域）时不写任何变换——平地姿势 bit 级不动。
//!
//! # 前提与诚实边界
//!
//! - **链契约**：`hip / knee / foot` 三节点须经 `ChildOf` 直连成链
//!   （knee 的父 = hip、foot 的父 = knee），不满足时 `debug_assert!` 并
//!   跳过该实体（release 静默）；
//! - **均匀缩放假设**：旋转回写沿父链按四元数复合（父级缩放不参与旋转
//!   语义）——链上均匀缩放（glTF 刚性关节的常态，测试亦为 1）下关节
//!   位置精确；非均匀缩放时旋转可能偏差（位置回写走矩阵、仍然精确）；
//! - **只动一条链**：hip 的旋转会带动其全部子级——v1 不隔离兄弟分支，
//!   腿部链请挂在独立节点上（装配惯例即如此）；
//! - **每帧全量修正**：不做足部接触状态机（plant / lift 判定留给游戏侧
//!   关闭 FootIk 或控制 `max_ground_drop`）；
//! - **确定性**：全程纯函数 + 固定遍历序（组件存储序），同输入两次调用
//!   逐位一致（单测守护）。
//!
//! # 用法
//!
//! ```ignore
//! // 装配：hip ← knee ← foot 三节点直连，播放器与 FootIk 同挂根实体
//! let player = AnimationPlayer::new(clips, node_entities); // 0=hip 1=knee 2=foot
//! world.insert(root, player);
//! world.insert(root, FootIk::default()); // pole −Z（引擎前方），探地 0.5 m
//! // 每帧 DefaultPlugins 自动执行：update_animations → update_foot_ik → 传播
//! ```

use glam::{Mat4, Quat, Vec3};
use iris_core::physics::{Collider, ColliderShape, RayHit, RigidBody, ray_vs_shape};
use iris_core::prelude::{Entity, Transform};
use iris_core::world::World;

use crate::animation::AnimationPlayer;

/// 平地门容差（米）：地面高差低于此值不写任何变换——平地姿势 bit 级
/// 不动（远小于 ±2 cm 的贴地验收域，浮点噪声也被挡在门外）。
const GROUND_EPSILON: f32 = 1e-3;

/// 退化骨长阈值（米）：骨骼世界长度低于此值视为被压扁的退化装配，
/// 跳过该实体（余弦定理在零长骨上无解）。
const MIN_BONE_LENGTH: f32 = 1e-6;

/// 两骨 IK 纯函数：hip-knee-ankle 链，ankle 去往 `target`（可达域内时
/// 精确到达），返回 `(ankle, knee)` 的世界位。
///
/// 数学：`d = |target − hip|` 钳制到 `[|upper − lower|, upper + lower]`
/// （不可达 → 拉伸至全长朝向 target；过近 → 钳到最小距离，双腿伸直）；
/// 膝关节投影距离 `a = (d² + upper² − lower²) / (2d)`（余弦定理），离轴
/// 高度 `h = √(upper² − a²)`；`ankle = hip + dir·d`、
/// `knee = hip + dir·a + bend·h`。`bend` = pole 向量投影到与 `dir` 垂直的
/// 平面后的单位向量——**弯曲方向由调用方锁定**（pole 不变则膝弯方向
/// 不翻转）；pole 与骨干平行（投影退化）时取与骨干夹角最小的世界轴作
/// 确定性兜底（x < y < z 优先）。
///
/// # Panics
/// `upper` / `lower` 非正时断言失败（中文断言消息）——零长骨没有 IK 语义。
pub fn solve_two_bone_ik(
    hip: Vec3,
    target: Vec3,
    upper: f32,
    lower: f32,
    pole: Vec3,
) -> (Vec3, Vec3) {
    assert!(
        upper > 0.0 && lower > 0.0,
        "两骨长度须为正：upper {upper} / lower {lower}（零长骨无 IK 语义）"
    );
    let to_target = target - hip;
    let distance = to_target.length();
    // 方向：目标与髋重合的退化取 +Y（确定性兜底）
    let dir = if distance > 1e-9 {
        to_target / distance
    } else {
        Vec3::Y
    };
    let max_reach = upper + lower;
    let min_reach = (upper - lower).abs();
    let clamped = if distance <= MIN_BONE_LENGTH {
        min_reach.max(MIN_BONE_LENGTH)
    } else {
        distance.clamp(min_reach, max_reach)
    };
    // 弯曲方向：pole 去掉沿骨干分量后归一；与骨干平行（投影长度² 低于
    // 1e-8）→ 取与骨干夹角最小的世界轴投影（确定性，x < y < z 并列优先）
    let bend = {
        let projected = pole - dir * pole.dot(dir);
        if projected.length_squared() > 1e-8 {
            projected / projected.length()
        } else {
            let axis = if dir.x.abs() <= dir.y.abs() && dir.x.abs() <= dir.z.abs() {
                Vec3::X
            } else if dir.y.abs() <= dir.z.abs() {
                Vec3::Y
            } else {
                Vec3::Z
            };
            let projected = axis - dir * axis.dot(dir);
            projected / projected.length()
        }
    };
    let ankle = hip + dir * clamped;
    let a = (clamped * clamped + upper * upper - lower * lower) / (2.0 * clamped);
    // 浮点防御：a 因舍入可轻微超过 upper（h² 名义负），钳零即伸直
    let h = (upper * upper - a * a).max(0.0).sqrt();
    let knee = hip + dir * a + bend * h;
    (ankle, knee)
}

/// 足部 IK 配置组件（v3）：声明哪条两骨链做 IK + 落地查询参数。
///
/// 挂在 [`AnimationPlayer`] 同一实体上，`*_node` 为其 `node_entities`
/// 下标；三节点须直连成链（前提与边界见模块文档）。每实体独立开关：
/// 没挂本组件的角色零参与（系统查询不到即过）。
#[derive(Clone, Debug, iris_core::Component)]
pub struct FootIk {
    /// 髋关节节点（`node_entities` 下标）。
    pub hip_node: usize,
    /// 膝关节节点（须为 hip 的子级）。
    pub knee_node: usize,
    /// 脚踝节点（须为 knee 的子级）。
    pub foot_node: usize,
    /// 膝盖弯曲方向（世界系）——人类角色通常为面朝方向（引擎约定前方
    /// = −Z）。与骨干平行的退化兜底见 [`solve_two_bone_ik`]。
    pub pole: Vec3,
    /// 最大下探（米）：脚踝到地面的容差，超出视为腾空不修正。
    pub max_ground_drop: f32,
    /// 射线起点自脚踝的上抬量（米）：保证脚踝插地时射线仍能命中地面
    /// （上拉修正依赖起点高于地面）。
    pub ray_up_offset: f32,
}

impl Default for FootIk {
    /// 常规装配：节点 0/1/2 = hip/knee/foot，pole −Z（引擎前方），探地
    /// 与上抬各 0.5 m。
    fn default() -> Self {
        Self {
            hip_node: 0,
            knee_node: 1,
            foot_node: 2,
            pole: Vec3::NEG_Z,
            max_ground_drop: 0.5,
            ray_up_offset: 0.5,
        }
    }
}

/// 静态体射线（私有封装）：沿 `origin → origin + dir × max_t` 找
/// 最近命中的静态体（`statics` 由 [`update_foot_ik`] 每帧一次收集，
/// 语义与 `resolve_against_statics` 一致：`is_static` 或仅 Collider 无
/// RigidBody）——动态体不参与 IK 落地（理由见模块文档「落地查询」）。
/// `dir` 须为单位方向。纯查询。
fn raycast_statics(
    statics: &[(Vec3, ColliderShape)],
    origin: Vec3,
    dir: Vec3,
    max_t: f32,
) -> Option<RayHit> {
    let mut best: Option<RayHit> = None;
    for (pos, shape) in statics {
        if let Some(hit) = ray_vs_shape(origin, dir, max_t, *pos, shape) {
            // 严格小于才替换：t 并列保留先遍历者（存储序，确定性）
            if best.is_none_or(|current| hit.t < current.t) {
                best = Some(hit);
            }
        }
    }
    best
}

/// 沿 `ChildOf` 链自根向叶累乘局部矩阵与旋转（读的是本帧最新的局部
/// `Transform`——动画刚写过、传播尚未发生）。链上有实体缺 `Transform`
/// 时返回 `None`（调用方跳过）。
fn chain_world(world: &World, entity: Entity) -> Option<(Mat4, Quat)> {
    let mut matrices = Vec::new();
    let mut rotations = Vec::new();
    let mut current = Some(entity);
    while let Some(node) = current {
        let transform = *world.get::<Transform>(node)?;
        matrices.push(transform.matrix());
        rotations.push(transform.rotation);
        current = world.parent(node); // 父已失效 = 链到此为止（与传播语义一致）
    }
    let matrix = matrices
        .iter()
        .rev()
        .fold(Mat4::IDENTITY, |acc, m| acc * *m);
    let rotation = rotations
        .iter()
        .rev()
        .fold(Quat::IDENTITY, |acc, q| acc * *q);
    Some((matrix, rotation))
}

/// 足部 IK 每帧系统：对每个「[`AnimationPlayer`] + [`FootIk`]」实体，
/// 把声明的两骨链脚踝拉到脚下静态地面（算法、编排与边界见模块文档）。
///
/// 只读世界签名（`&World`）：组件写访问走世界内部的尝试型锁，注册为
/// **并行系统**（访问集：写 Transform，读 AnimationPlayer / FootIk /
/// ChildOf / Collider / RigidBody）。两阶段：只读收集回写任务 → 统一
/// 写局部变换（照 `update_animations` 模式）。
pub fn update_foot_ik(world: &World) {
    // 静态体一次收集（组件缺席 = 静态的缺席语义与 step_physics 一致）
    let mut statics: Vec<(Vec3, ColliderShape)> = Vec::new();
    world.query::<(&Transform, &Collider, Option<&RigidBody>)>(|(transform, collider, rigid)| {
        if collider.is_static || rigid.is_none() {
            statics.push((transform.position, collider.shape));
        }
    });

    // 阶段一（只读解算）：每实体产出「hip/knee 局部旋转 + foot 局部位置」
    let mut jobs: Vec<(Entity, Entity, Entity, Quat, Quat, Vec3)> = Vec::new();
    world.query::<(&AnimationPlayer, &FootIk)>(|(player, ik)| {
        let resolve = |index: usize| -> Option<Entity> {
            match player.node_entities.get(index).copied().flatten() {
                Some(entity) => Some(entity),
                None => {
                    debug_assert!(
                        false,
                        "FootIk 节点索引越界或未映射到实体：{index} / {}",
                        player.node_entities.len()
                    );
                    None
                }
            }
        };
        let Some(hip_entity) = resolve(ik.hip_node) else {
            return;
        };
        let Some(knee_entity) = resolve(ik.knee_node) else {
            return;
        };
        let Some(foot_entity) = resolve(ik.foot_node) else {
            return;
        };
        // 链契约：knee 的父 = hip、foot 的父 = knee
        if world.parent(knee_entity) != Some(hip_entity)
            || world.parent(foot_entity) != Some(knee_entity)
        {
            debug_assert!(
                false,
                "FootIk 三节点须直连成链（knee 的父 = hip、foot 的父 = knee）"
            );
            return;
        }
        let Some((hip_world, hip_world_rot)) = chain_world(world, hip_entity) else {
            return;
        };
        let Some((knee_world, knee_world_rot)) = chain_world(world, knee_entity) else {
            return;
        };
        let Some(foot_world) = chain_world(world, foot_entity).map(|(m, _)| m) else {
            return;
        };
        let hip = hip_world.w_axis.truncate();
        let knee = knee_world.w_axis.truncate();
        let foot = foot_world.w_axis.truncate();
        // 骨长取当前世界姿势的实际距离（保留装配比例）；退化骨长跳过
        let upper = (knee - hip).length();
        let lower = (foot - knee).length();
        if upper <= MIN_BONE_LENGTH || lower <= MIN_BONE_LENGTH {
            return;
        }
        // 落地查询：脚踝上抬后竖直向下，命中范围 = 上抬 + 下探容差
        let origin = foot + Vec3::Y * ik.ray_up_offset;
        let Some(hit) = raycast_statics(
            &statics,
            origin,
            Vec3::NEG_Y,
            ik.ray_up_offset + ik.max_ground_drop,
        ) else {
            return;
        };
        // 平地门：高差低于容差不写任何变换（bit 级不动）
        if (hit.point.y - foot.y).abs() <= GROUND_EPSILON {
            return;
        }
        // 世界系两骨解
        let (ankle_new, knee_new) = solve_two_bone_ik(hip, hit.point, upper, lower, ik.pole);
        // 骨方向的最小旋转弧：旧方向 → 新方向（世界系）
        let hip_to_knee_old = (knee - hip) / upper;
        let hip_to_knee_new = (knee_new - hip) / upper;
        let knee_to_foot_old = (foot - knee) / lower;
        let knee_to_foot_new = (ankle_new - knee_new) / lower;
        let arc_hip = Quat::from_rotation_arc(hip_to_knee_old, hip_to_knee_new);
        let arc_knee = Quat::from_rotation_arc(knee_to_foot_old, knee_to_foot_new);
        // 反推局部旋转：世界旋转 = 父级旋转 × 局部旋转（单位四元数，
        // 逆 = 共轭）。hip 的父级旋转 = 髋世界旋转 × 髋局部旋转⁻¹；
        // knee 的父级 = hip（链契约已校验），其新世界旋转 = arc_hip × 旧值
        let Some(hip_local_rot) = world.get::<Transform>(hip_entity).map(|t| t.rotation) else {
            return;
        };
        let parent_rot = hip_world_rot * hip_local_rot.conjugate();
        let hip_world_rot_new = arc_hip * hip_world_rot;
        let knee_world_rot_new = arc_knee * knee_world_rot;
        let hip_local_rot_new = parent_rot.conjugate() * hip_world_rot_new;
        let knee_local_rot_new = hip_world_rot_new.conjugate() * knee_world_rot_new;
        // foot 局部位置：膝关节新世界矩阵的逆 × 踝目标（hip 局部旋转已
        // 更新、knee 局部位置不动 ⇒ 膝世界位 = 解算值；foot 局部旋转不动）
        let Some(hip_local) = world.get::<Transform>(hip_entity).map(|t| *t) else {
            return;
        };
        let Some(knee_local) = world.get::<Transform>(knee_entity).map(|t| *t) else {
            return;
        };
        let parent_matrix = match world.parent(hip_entity) {
            Some(parent) => match chain_world(world, parent) {
                Some((matrix, _)) => matrix,
                None => return,
            },
            None => Mat4::IDENTITY,
        };
        let hip_matrix_new = parent_matrix
            * Mat4::from_scale_rotation_translation(
                hip_local.scale,
                hip_local_rot_new,
                hip_local.position,
            );
        let knee_matrix_new = hip_matrix_new
            * Mat4::from_scale_rotation_translation(
                knee_local.scale,
                knee_local_rot_new,
                knee_local.position,
            );
        let foot_local_pos_new = (knee_matrix_new.inverse() * ankle_new.extend(1.0)).truncate();
        jobs.push((
            hip_entity,
            knee_entity,
            foot_entity,
            hip_local_rot_new,
            knee_local_rot_new,
            foot_local_pos_new,
        ));
    });

    // 阶段二（写入）：hip/knee 局部旋转 + foot 局部位置（其余字段不动）
    for (hip_entity, knee_entity, foot_entity, hip_rot, knee_rot, foot_pos) in jobs {
        if let Some(mut transform) = world.get_mut::<Transform>(hip_entity) {
            transform.rotation = hip_rot;
        }
        if let Some(mut transform) = world.get_mut::<Transform>(knee_entity) {
            transform.rotation = knee_rot;
        }
        if let Some(mut transform) = world.get_mut::<Transform>(foot_entity) {
            transform.position = foot_pos;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core::hierarchy::propagate_transforms;
    use iris_core::physics::Collider;
    use iris_core::prelude::GlobalTransform;

    /// 组装「root → hip → knee → foot」直连链（腿长 0.5 + 0.5）+
    /// 播放器（node 0/1/2 = hip/knee/foot）+ 默认 FootIk，root 置于
    /// `root_pos`（hip 世界位 = root_pos）。
    fn rig(root_pos: Vec3) -> (World, Entity, Entity, Entity, Entity) {
        let mut world = World::new();
        let root = world.spawn((Transform::from_xyz(root_pos.x, root_pos.y, root_pos.z),));
        let hip = world.spawn((Transform::IDENTITY,));
        let knee = world.spawn((Transform::from_xyz(0.0, -0.5, 0.0),));
        let foot = world.spawn((Transform::from_xyz(0.0, -0.5, 0.0),));
        world.add_child(root, hip);
        world.add_child(hip, knee);
        world.add_child(knee, foot);
        let player = AnimationPlayer::new(vec![], vec![Some(hip), Some(knee), Some(foot)]);
        world.insert(root, player);
        world.insert(root, FootIk::default());
        (world, root, hip, knee, foot)
    }

    /// 顶面 y = `top` 的大静态平台（盒心 y = top − 1）。
    fn platform(world: &mut World, top: f32) {
        world.spawn((
            Transform::from_xyz(0.0, top - 1.0, 0.0),
            Collider::static_box(Vec3::new(50.0, 1.0, 50.0)),
        ));
    }

    fn position_bits(transform: &Transform) -> [u32; 3] {
        [
            transform.position.x.to_bits(),
            transform.position.y.to_bits(),
            transform.position.z.to_bits(),
        ]
    }

    fn rotation_bits(transform: &Transform) -> [u32; 4] {
        [
            transform.rotation.x.to_bits(),
            transform.rotation.y.to_bits(),
            transform.rotation.z.to_bits(),
            transform.rotation.w.to_bits(),
        ]
    }

    fn joint_bits(world: &World, joints: &[Entity]) -> Vec<([u32; 3], [u32; 4])> {
        joints
            .iter()
            .map(|&joint| {
                let transform = *world.get::<Transform>(joint).unwrap();
                (position_bits(&transform), rotation_bits(&transform))
            })
            .collect()
    }

    // ---- 纯函数：solve_two_bone_ik ----

    #[test]
    fn two_bone_exact_solution_within_reach() {
        // hip (0,1,0)、target (0,0.6,0)、两骨各 0.5、pole +X：
        // d = 0.4 → a = 0.2、h = √(0.25 − 0.04) = √0.21
        let hip = Vec3::new(0.0, 1.0, 0.0);
        let (ankle, knee) = solve_two_bone_ik(hip, Vec3::new(0.0, 0.6, 0.0), 0.5, 0.5, Vec3::X);
        assert!(
            (ankle - Vec3::new(0.0, 0.6, 0.0)).length() < 1e-6,
            "可达域内踝应精确到目标，实际 {ankle:?}"
        );
        let expected_knee = Vec3::new(0.21f32.sqrt(), 0.8, 0.0);
        assert!(
            (knee - expected_knee).length() < 1e-5,
            "膝应按余弦定理落在 ({:?})，实际 {knee:?}",
            expected_knee
        );
        assert!(
            ((knee - hip).length() - 0.5).abs() < 1e-5,
            "股骨长应严格保持 0.5，实际 {}",
            (knee - hip).length()
        );
        assert!(
            ((ankle - knee).length() - 0.5).abs() < 1e-5,
            "胫骨长应严格保持 0.5，实际 {}",
            (ankle - knee).length()
        );
    }

    #[test]
    fn two_bone_unreachable_stretches_full_length() {
        let hip = Vec3::new(0.0, 1.0, 0.0);
        let (ankle, knee) = solve_two_bone_ik(hip, Vec3::new(0.0, 11.0, 0.0), 0.5, 0.5, Vec3::X);
        assert!(
            ((ankle - hip).length() - 1.0).abs() < 1e-6,
            "不可达应拉伸至全长，实际 {}",
            (ankle - hip).length()
        );
        assert!(
            (knee - (hip + Vec3::Y * 0.5)).length() < 1e-6,
            "拉伸时膝应落在髋踝连线上（直腿），实际 {knee:?}"
        );
    }

    #[test]
    fn two_bone_too_close_clamps_to_min_reach() {
        // upper 0.6 / lower 0.4：最小距离 0.2；目标距 0.05 → 钳到 0.2 伸直
        let hip = Vec3::new(0.0, 1.0, 0.0);
        let (ankle, knee) = solve_two_bone_ik(hip, Vec3::new(0.0, 1.05, 0.0), 0.6, 0.4, Vec3::X);
        assert!(
            ((ankle - hip).length() - 0.2).abs() < 1e-6,
            "过近应钳到 |upper − lower| = 0.2，实际 {}",
            (ankle - hip).length()
        );
        assert!(
            (ankle - (hip + Vec3::Y * 0.2)).length() < 1e-6,
            "钳制方向应朝目标侧（+Y），实际 {ankle:?}"
        );
        assert!(
            (knee - (hip + Vec3::Y * 0.6)).length() < 1e-6,
            "最小距离处双腿伸直：膝应在髋上方 0.6，实际 {knee:?}"
        );
        assert!(
            ((knee - ankle).length() - 0.4).abs() < 1e-6,
            "胫骨长应保持 0.4"
        );
    }

    #[test]
    fn two_bone_pole_locks_bend_direction() {
        let hip = Vec3::new(0.0, 1.0, 0.0);
        let target = Vec3::new(0.0, 0.6, 0.0);
        let (ankle_pos, knee_pos) = solve_two_bone_ik(hip, target, 0.5, 0.5, Vec3::X);
        let (ankle_neg, knee_neg) = solve_two_bone_ik(hip, target, 0.5, 0.5, Vec3::NEG_X);
        assert_eq!(ankle_pos, ankle_neg, "pole 只定膝弯方向，不改踝位");
        assert!(
            knee_pos.x > 0.1 && knee_neg.x < -0.1,
            "pole 翻转膝弯应镜像不翻转：{knee_pos:?} vs {knee_neg:?}"
        );
    }

    #[test]
    #[should_panic(expected = "两骨长度须为正")]
    fn two_bone_zero_length_panics() {
        solve_two_bone_ik(Vec3::ZERO, Vec3::Y, 0.0, 0.5, Vec3::X);
    }

    // ---- 世界级：update_foot_ik ----

    #[test]
    fn flat_ground_keeps_pose_bitwise() {
        let (mut world, _root, hip, knee, foot) = rig(Vec3::new(0.0, 1.0, 0.0));
        platform(&mut world, 0.0); // 脚踝恰在 y = 0 的地面上
        let joints = vec![hip, knee, foot];
        let before = joint_bits(&world, &joints);
        update_foot_ik(&world);
        assert_eq!(
            joint_bits(&world, &joints),
            before,
            "平地（高差 ≤ 容差）应不写任何变换：姿势 bit 级不动"
        );
    }

    #[test]
    fn slope_keeps_ankle_on_surface() {
        // 大山球做斜坡（球心 (0,−r,0)）：x=2 处表面 y = −r + √(r²−4)
        let r = 10.0f32;
        let (mut world, _root, _hip, knee, foot) = rig(Vec3::new(2.0, 0.75, 0.0));
        world.spawn((
            Transform::from_xyz(0.0, -r, 0.0),
            Collider::static_sphere(r),
        ));
        update_foot_ik(&world);
        propagate_transforms(&mut world);
        let expected = -r + (r * r - 2.0 * 2.0).sqrt();
        let ankle = world.get::<GlobalTransform>(foot).unwrap().position();
        assert!(
            (ankle.y - expected).abs() < 2e-2,
            "斜坡脚踝应贴合球面 ±2 cm：踝 {} vs 面 {expected}",
            ankle.y
        );
        assert!(
            (ankle.x - 2.0).abs() < 1e-5 && ankle.z.abs() < 1e-5,
            "竖直落地不应改 xz，实际 {ankle:?}"
        );
        let knee_pos = world.get::<GlobalTransform>(knee).unwrap().position();
        assert!(
            knee_pos.z < -0.1,
            "膝应朝 pole（−Z）侧弯曲，实际 {knee_pos:?}"
        );
    }

    #[test]
    fn unreachable_ground_stretches_leg() {
        // 地面在脚踝下 0.4（探地范围内），但髋到目标 1.4 > 全腿长 1.0：
        // 全腿伸直朝向目标，踝停在髋下 1.0 处（不再下探）
        let (mut world, _root, hip, knee, foot) = rig(Vec3::new(0.0, 0.6, 0.0));
        platform(&mut world, -0.8);
        update_foot_ik(&world);
        propagate_transforms(&mut world);
        let hip_pos = world.get::<GlobalTransform>(hip).unwrap().position();
        let ankle = world.get::<GlobalTransform>(foot).unwrap().position();
        let knee_pos = world.get::<GlobalTransform>(knee).unwrap().position();
        assert!(
            ((ankle - hip_pos).length() - 1.0).abs() < 1e-4,
            "不可达应全腿伸直：长 {}",
            (ankle - hip_pos).length()
        );
        assert!(
            (ankle.y - (hip_pos.y - 1.0)).abs() < 1e-4,
            "踝应朝地面拉伸到全长（y ≈ 髋 − 1.0），实际 {}",
            ankle.y
        );
        assert!(
            (knee_pos.y - (hip_pos.y - 0.5)).abs() < 1e-4,
            "直腿膝应落在链上，实际 {}",
            knee_pos.y
        );
    }

    #[test]
    fn no_ground_leaves_pose_untouched() {
        let (world, _root, hip, knee, foot) = rig(Vec3::new(0.0, 1.0, 0.0));
        let joints = vec![hip, knee, foot];
        let before = joint_bits(&world, &joints);
        update_foot_ik(&world); // 无任何静态体：射线不命中 → 不写
        assert_eq!(
            joint_bits(&world, &joints),
            before,
            "探地未命中（腾空超容差）应不写任何变换"
        );
    }

    #[test]
    fn dynamic_bodies_do_not_ground_the_foot() {
        // 脚正下方的**动态**球不参与 IK 落地（静态体 only 语义）
        let (mut world, _root, hip, knee, foot) = rig(Vec3::new(0.0, 1.0, 0.0));
        world.spawn((
            Transform::from_xyz(0.0, -0.5, 0.0),
            Collider::dynamic_sphere(0.5, 0.0),
            RigidBody::default(),
        ));
        let joints = vec![hip, knee, foot];
        let before = joint_bits(&world, &joints);
        update_foot_ik(&world);
        assert_eq!(
            joint_bits(&world, &joints),
            before,
            "动态体不应作为 IK 落地面：姿势 bit 级不动"
        );
    }

    #[test]
    fn foot_ik_is_bitwise_deterministic() {
        let build = || {
            let r = 10.0f32;
            let (mut world, _root, hip, knee, foot) = rig(Vec3::new(2.0, 0.75, 0.0));
            world.spawn((
                Transform::from_xyz(0.0, -r, 0.0),
                Collider::static_sphere(r),
            ));
            (world, vec![hip, knee, foot])
        };
        let (world_a, joints_a) = build();
        let (world_b, joints_b) = build();
        for _ in 0..3 {
            update_foot_ik(&world_a);
            update_foot_ik(&world_b);
            assert_eq!(
                joint_bits(&world_a, &joints_a),
                joint_bits(&world_b, &joints_b),
                "同配置两次 IK 应逐位一致"
            );
        }
    }

    /// 仅 debug 构建：链契约走 `debug_assert!`（release 下静默跳过是
    /// 模块文档明文语义），release 编译掉断言后不应再测 panic。
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "直连成链")]
    fn foot_ik_broken_chain_panics_in_debug() {
        // hip / knee / foot 未建立父子关系：链契约 debug_assert
        let mut world = World::new();
        let root = world.spawn((Transform::IDENTITY,));
        let hip = world.spawn((Transform::IDENTITY,));
        let knee = world.spawn((Transform::from_xyz(0.0, -0.5, 0.0),));
        let foot = world.spawn((Transform::from_xyz(0.0, -0.5, 0.0),));
        let player = AnimationPlayer::new(vec![], vec![Some(hip), Some(knee), Some(foot)]);
        world.insert(root, player);
        world.insert(root, FootIk::default());
        update_foot_ik(&world);
    }
}
