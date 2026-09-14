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
//! `RigidBody`（组件缺席 = 静态）。**R50 旋转感知**：快照带
//! `Transform.rotation`（[`StaticObstacle`]），射线走
//! `ray_vs_shape_rotated`——动画接管期的斜置胶囊尸体照实作为支撑面 /
//! 阻挡（旧口径当成直立胶囊，脚会踩在幻影上）。**动态体不参与 IK 落地**
//! ——脚踝踩的「地面」必须是稳定的支撑面，跟随动态体会把运动物体的表面
//! 抖动喂进关节链；命中范围 = `ray_up_offset + max_ground_drop`（脚下探
//! 超出容差视为腾空，不修正）。命中点即脚踝目标：竖直射线 ⇒ 命中点 =
//! 脚踝正下方的地面点，脚踝低于地面（插地）时目标在上方 → 上拉，悬空时
//! → 下探。
//!
//! # 不可达回退（为何要放弃而非钳制）
//!
//! 解算前检查目标可达性：目标距髋超出 `( |upper − lower|, upper + lower )`
//! 可达域时**本帧放弃该腿的修正**（不写任何变换）。原因：两骨链的全腿长
//! 就是角色腿长，落地目标竟超出腿长（或近到双腿无法展开），唯一解释是
//! 射线采样到了**非地面几何**——过门、贴墙瞬间命中门框 / 墙顶，或起点
//! 落入几何内部（t = 0 异常采样）；此时钳制解算（拉伸至全长 / 钳到最小
//! 距离）会把腿部网格猛地指向异常目标，出现「腿拉成平面」式变形。放弃
//! 一帧修正是视觉上最小的错误（保持动画姿势），`max_ground_drop` 的正常
//! 下探容差语义不受影响。同类口径：**sensor**（事件域不是地面）与
//! **碰撞组 / 掩码清零**（标记失效的碰撞体，如游戏侧的开门标记）都不
//! 参与落地查询。
//!
//! # 解算与回写（完整两骨旋转解）
//!
//! 1. **世界系两骨解**（[`solve_two_bone_ik`]，纯函数）：骨长取**当前世界
//!    姿势的实际距离**（保留装配比例），余弦定理解膝角、pole 投影定弯曲
//!    方向，返回 `(ankle, knee)` 世界位——不可达拉伸至全长、过近钳制到
//!    最小距离（双腿长差）；调用前系统先做**不可达回退门**（见上文「
//!    不可达回退」），可达域外的目标根本不进入解算。
//! 2. **旋转回写**：髋骨 / 膝骨的世界方向从 `hip→knee / knee→ankle` 的
//!    旧方向旋到新方向（`Quat::from_rotation_arc` 最小旋转弧），再除以
//!    各自父级旋转反推**局部旋转**；foot 局部旋转不动、按膝关节新世界
//!    矩阵的逆回写**局部位置**把踝关节精确放到目标上。hip / knee 的局部
//!    位置不动 ⇒ 骨长严格保持。
//! 3. **平地门**：地面高差 ≤ [`GROUND_EPSILON`]（1 mm，远小于 ±2 cm
//!    验收域）时不写任何变换——平地姿势 bit 级不动。
//!
//! # 回写四元数归一化是 IK 的责任（打磨 VI 摩擦①）
//!
//! IK 是**唯一**会覆写「不被剪辑逐帧重置的骨旋转」的系统：`solve_two_bone_ik`
//! 的世界系解走 `Quat::from_rotation_arc` + 父级共轭反推局部旋转，两处都
//! 带浮点范数漂移。写回非单位四元数的后果取决于下帧有没有剪辑通道覆写该
//! 骨——有覆写则被重置（walk 驱动双腿的资产侥幸免疫），无覆写则该值作为
//! 输入再进复合链（共轭不再是逆），**范数逐帧平方复合**：R48 实测 idle
//! 角色（骨盆 bob 把脚抬离地面 → IK 每帧写，但 idle 无腿通道）1.9 s 内
//! thigh 旋转范数 386 → 5.8e7 → NaN，随后骨长 NaN 撞
//! [`solve_two_bone_ik`] 的正值断言 panic。因此**两个 IK 系统写回的局部
//! 旋转一律先 `normalize()`**（单位化后复合链收敛，漂移被限制在 1e-7 量级）
//! ——归一化不放在 `Transform` 写入的通用路径上（那是热路径且动画侧的采样
//! 值本由关键帧插值保证单位），谁引入非单位四元数谁负责。回归锁：
//! `foot_ik::tests::foot_ik_quaternion_norm_stable_without_clip_overwrite`
//! （600 帧逐帧断言 ‖q‖ ≤ 1 + 1e-3、无 NaN、位形稳定）。
//!
//! # 节点映射：IkRig 或 AnimationPlayer（打磨 V 起二选一）
//!
//! 链节点实体由 [`IkRig`]（纯 IK 装配：节点映射自带，与播放器无关）或
//! [`AnimationPlayer::node_entities`](crate::animation::AnimationPlayer)
//! （与播放器同挂的装配，回退路径）提供——查询 `Option<&AnimationPlayer>`
//! / `Option<&IkRig>` **二选一非空**即可，[`FootIk::hip_node`] 等下标在
//! 两者中同义（查 [`IkRig`] 优先）。纯 IK 腿链不再被迫挂零剪辑播放器
//! （§8.22② 的闭合）；两者并存时 [`IkRig`] 胜出。
//!
//! # 前提与诚实边界
//!
//! - **链契约**：`hip / knee / foot` 三节点须经 `ChildOf` 直连成链
//!   （knee 的父 = hip、foot 的父 = knee），不满足时 `debug_assert!` 并
//!   跳过该实体（release 静默）；
//! - **链上节点 scale 恒 1（契约，打磨 V）**：`chain_world` 沿父链累乘
//!   局部矩阵——节点 scale 会进入链矩阵、把骨长按比例缩水（节点缩放
//!   的本意通常是缩放关节网格，应放在**独立的子网格实体**或父级上）。
//!   系统按 [`FootIk::expected_upper`] / [`FootIk::expected_lower`] 监测
//!   骨长偏差，超过 10% 时 `log::warn!` 一次（每实体至多一次，防刷屏）
//!   ——生产保护而非 panic，IK 仍按当前（已失真的）骨长解算；
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
use iris_core::physics::{Collider, RayHit, RigidBody, StaticObstacle, ray_vs_shape_rotated};
use iris_core::prelude::{Entity, Transform};
use iris_core::world::World;

use crate::animation::AnimationPlayer;

/// 平地门容差（米）：地面高差低于此值不写任何变换——平地姿势 bit 级
/// 不动（远小于 ±2 cm 的贴地验收域，浮点噪声也被挡在门外）。
const GROUND_EPSILON: f32 = 1e-3;

/// 退化骨长阈值（米）：骨骼世界长度低于此值视为被压扁的退化装配，
/// 跳过该实体（余弦定理在零长骨上无解）。pub(crate)：手部 IK 同门。
pub(crate) const MIN_BONE_LENGTH: f32 = 1e-6;

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

/// 纯 IK 装配的节点映射组件（打磨 V）：`node_entities` 下标 → 实体，
/// 语义与 `AnimationPlayer::node_entities` 的映射部分一致。
///
/// 与 [`AnimationPlayer`] **二选一**即可被 [`update_foot_ik`] 命中：
/// 纯 IK 装配（无动画的腿链 rig）挂本组件；与播放器同挂时本组件优先。
/// 便利构造见 [`FootIk::from_rig`]。
#[derive(Clone, Debug, iris_core::Component)]
pub struct IkRig {
    /// 链节点实体表（下标即 [`FootIk::hip_node`] 等声明的下标）。
    pub node_entities: Vec<Option<Entity>>,
}

impl IkRig {
    /// 以节点实体表构造（典型：`vec![Some(hip), Some(knee), Some(foot)]`）。
    pub fn new(node_entities: Vec<Option<Entity>>) -> Self {
        Self { node_entities }
    }
}

/// 足部 IK 配置组件（v3）：声明哪条两骨链做 IK + 落地查询参数。
///
/// 节点映射来源见模块文档「节点映射」——[`IkRig`] 或
/// [`AnimationPlayer`] 二选一，`*_node` 为其下标；三节点须直连成链
/// （前提与边界见模块文档）。每实体独立开关：没挂本组件的角色零参与
/// （系统查询不到即过）。
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
    /// 股骨（hip→knee）期望长（米）：scale 污染监测的基准。`0.0` =
    /// 未捕获——首次解算按当时姿势记录（首帧姿势即基准）；装配场景
    /// 可显式填写。见模块文档「链上节点 scale 恒 1」。
    pub expected_upper: f32,
    /// 胫骨（knee→foot）期望长（米），语义同 [`FootIk::expected_upper`]。
    pub expected_lower: f32,
    /// scale 污染告警已发过（每实体至多 warn 一次，防刷屏）。
    pub scale_warned: bool,
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
            expected_upper: 0.0,
            expected_lower: 0.0,
            scale_warned: false,
        }
    }
}

impl FootIk {
    /// 纯 IK 装配辅助（打磨 V）：hip ← knee ← foot 三实体直连链 →
    /// `(IkRig, FootIk)`——两个组件同挂链根（原 `AnimationPlayer` 同体
    /// 位置）即可被系统命中。骨长期望不在此捕获（构造期无世界访问），
    /// 留给 `update_foot_ik` 首次解算按当时姿势记录；需要显式基准时
    /// 直接填写 [`FootIk::expected_upper`] / [`FootIk::expected_lower`]。
    pub fn from_rig(hip: Entity, knee: Entity, foot: Entity) -> (IkRig, FootIk) {
        (
            IkRig::new(vec![Some(hip), Some(knee), Some(foot)]),
            FootIk::default(),
        )
    }
}

/// 静态体射线（私有封装）：沿 `origin → origin + dir × max_t` 找
/// 最近命中的静态体（`statics` 由 [`update_foot_ik`] 每帧一次收集，
/// 语义为**真静态体**：`is_static` 或仅 Collider 无 RigidBody，且**不是
/// sensor、碰撞组/掩码未清零**——sensor 是事件域、清零是标记失效，
/// 两者都不是可站立的地面）——动态体
/// 与**运动学体**（`BodyType::Kinematic`）都不参与 IK 落地：前者会抖动
/// 喂进关节链（见模块文档「落地查询」），后者常与自身链的角色碰撞体
/// 重叠——命中自身会把落地查询毁掉（打磨 V 的刻意口径差异，与
/// `resolve_against_statics` 的推离/角色口径不同）。`dir` 须为单位方向。
/// 纯查询。
fn raycast_statics(
    statics: &[StaticObstacle],
    origin: Vec3,
    dir: Vec3,
    max_t: f32,
) -> Option<RayHit> {
    let mut best: Option<RayHit> = None;
    for &(pos, rotation, shape) in statics {
        if let Some(hit) = ray_vs_shape_rotated(origin, dir, max_t, pos, rotation, &shape) {
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
/// 时返回 `None`（调用方跳过）。pub(crate)：手部 IK（`hand_ik` 模块）
/// 与足部共用同一链姿势读取（同一帧内两段 IK 先后作用于同一份本帧
/// 局部姿势，读取语义必须逐位一致）。
pub(crate) fn chain_world(world: &World, entity: Entity) -> Option<(Mat4, Quat)> {
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

/// 足部 IK 每帧系统：对每个挂有 [`FootIk`] 且节点映射齐备（[`IkRig`] 或
/// [`AnimationPlayer`]，二选一）的实体，把声明的两骨链脚踝拉到脚下静态
/// 地面（算法、编排与边界见模块文档）。
///
/// 只读世界签名（`&World`）：组件写访问走世界内部的尝试型锁，注册为
/// **并行系统**（访问集：写 Transform / FootIk，读 AnimationPlayer /
/// IkRig / ChildOf / Collider / RigidBody）。两阶段：只读收集回写任务 →
/// 统一写局部变换与骨长基准（照 `update_animations` 模式——查询闭包持
/// 共享借用，`FootIk` 的监测状态写回延后到阶段二）。
pub fn update_foot_ik(world: &World) {
    // 静态体一次收集（**真静态体**：运动学体不参与 IK 落地——常与自身
    // 链的角色碰撞体重叠，见 raycast_statics 文档；sensor 是事件域、
    // 碰撞组/掩码清零是标记失效，两者同样不是地面）
    let mut statics: Vec<StaticObstacle> = Vec::new();
    world.query::<(&Transform, &Collider, Option<&RigidBody>)>(|(transform, collider, rigid)| {
        if collider.is_static || rigid.is_none() {
            if collider.is_sensor || collider.groups == 0 || collider.mask == 0 {
                return;
            }
            statics.push((transform.position, transform.rotation, collider.shape));
        }
    });

    // 阶段一（只读解算）：每实体产出「hip/knee 局部旋转 + foot 局部位置」
    // + 骨长（scale 污染监测用，阶段二统一写 FootIk）
    let mut jobs: Vec<(Entity, Entity, Entity, Quat, Quat, Vec3)> = Vec::new();
    let mut bone_lengths: Vec<(Entity, f32, f32)> = Vec::new();
    world.query::<(Entity, &FootIk, Option<&AnimationPlayer>, Option<&IkRig>)>(
        |(entity, ik, player, rig)| {
        // 节点解析：IkRig 优先，回退 AnimationPlayer（二选一非空，见模块
        // 文档「节点映射」）；两者都缺 = 装配错误
        let resolve = |index: usize| -> Option<Entity> {
            let mapped = rig
                .and_then(|rig| rig.node_entities.get(index).copied().flatten())
                .or_else(|| {
                    player.and_then(|player| {
                        player.node_entities.get(index).copied().flatten()
                    })
                });
            match mapped {
                Some(entity) => Some(entity),
                None => {
                    debug_assert!(
                        false,
                        "FootIk 节点索引越界或未映射到实体：{index}（IkRig /                          AnimationPlayer 至少挂一个且表长覆盖该下标）"
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
        // 骨长取当前世界姿势的实际距离（保留装配比例）；先记录供 scale
        // 污染监测（阶段二统一写 FootIk，查询闭包内不可再借 FootIk）
        let upper = (knee - hip).length();
        let lower = (foot - knee).length();
        bone_lengths.push((entity, upper, lower));
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
        // 不可达回退门：踝目标距髋超出 (|upper − lower|, upper + lower)
        // 可达域时**本帧放弃该腿的 IK 修正**（保持动画/当前姿势，不写任何
        // 变换）——不可达或过近说明射线采样到了非地面几何（过门、贴墙
        // 瞬间的门框 / 墙顶命中，或起点入体的异常采样），把踝拉到位必然
        // 错：拉伸至全长会把腿部网格指向异常目标。`max_ground_drop` 的
        // 正常下探容差语义不变（容差内的地面照常修正）。
        let target_dist = (hit.point - hip).length();
        if target_dist > upper + lower || target_dist < (upper - lower).abs() {
            return;
        }
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
        // 归一化是 IK 的责任（打磨 VI 摩擦①）：本系统是唯一会覆写
        // **不被剪辑逐帧重置**的骨旋转的系统——`from_rotation_arc` 产物
        // 与父级共轭反推链都带浮点范数漂移，若把非单位四元数写回，下一帧
        // 无剪辑通道覆写时被当作输入再乘（共轭不再是逆）→ 范数逐帧平方
        // 复合直至 NaN。剪辑覆写的骨每帧重置故免疫（infiltration 的 walk
        // 恰覆写双腿 = 未触发本 bug 的侥幸）。归一化点选在局部旋转的产生
        // 处：位置回写（下方 hip/knee 新世界矩阵）与旋转回写共用同一份。
        let hip_local_rot_new = (parent_rot.conjugate() * hip_world_rot_new).normalize();
        let knee_local_rot_new = (hip_world_rot_new.conjugate() * knee_world_rot_new).normalize();
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
        },
    );

    // 阶段二（写入）：hip/knee 局部旋转 + foot 局部位置（其余字段不动）；
    // FootIk 的骨长基准捕获 / scale 污染告警也在此统一写（契约见模块
    // 文档「链上节点 scale 恒 1」）
    for &(entity, upper, lower) in &bone_lengths {
        if let Some(mut ik) = world.get_mut::<FootIk>(entity) {
            if ik.expected_upper <= 0.0 || ik.expected_lower <= 0.0 {
                // 未捕获基准：首次解算按当时姿势记录
                ik.expected_upper = upper;
                ik.expected_lower = lower;
            } else {
                let polluted = (upper - ik.expected_upper).abs() > 0.10 * ik.expected_upper
                    || (lower - ik.expected_lower).abs() > 0.10 * ik.expected_lower;
                if polluted && !ik.scale_warned {
                    log::warn!(
                        "FootIk 链骨长偏离期望超过 10%（upper {upper:.4} vs 期望                          {:.4}，lower {lower:.4} vs 期望 {:.4}）——IK 链节点 scale                          须恒 1（缩放请用网格子实体或父级），解算继续但骨长语义已失真",
                        ik.expected_upper,
                        ik.expected_lower
                    );
                    ik.scale_warned = true;
                }
            }
        }
    }
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
    fn unreachable_ground_leaves_pose_untouched() {
        // 地面在脚踝下 0.4（探地范围内），但髋到目标 1.4 > 全腿长 1.0：
        // 不可达域 = 射线采样到非地面几何的信号 → 本帧放弃修正、姿势
        // bit 级不动（不再拉伸至全长——那会把腿拉向门框 / 墙顶式异常
        // 目标，产生「腿拉成平面」的变形）
        let (mut world, _root, hip, knee, foot) = rig(Vec3::new(0.0, 0.6, 0.0));
        platform(&mut world, -0.8);
        let joints = vec![hip, knee, foot];
        let before = joint_bits(&world, &joints);
        update_foot_ik(&world);
        assert_eq!(
            joint_bits(&world, &joints),
            before,
            "不可达目标（> 全腿长）应放弃本帧修正：姿势 bit 级不动"
        );
    }

    #[test]
    fn too_close_target_leaves_pose_untouched() {
        // 胫骨短的链（upper 0.7 / lower 0.2 → 过近域 < |0.7 − 0.2| = 0.5）：
        // 平台顶 y = 0.55，髋(1.0)到目标 0.45 落入过近域 → 本帧放弃修正
        // （纯函数本身会钳到最小距离，系统门在解算之前拦截）
        let mut world = World::new();
        let root = world.spawn((Transform::from_xyz(0.0, 1.0, 0.0),));
        let hip = world.spawn((Transform::IDENTITY,));
        let knee = world.spawn((Transform::from_xyz(0.0, -0.7, 0.0),));
        let foot = world.spawn((Transform::from_xyz(0.0, -0.2, 0.0),));
        world.add_child(root, hip);
        world.add_child(hip, knee);
        world.add_child(knee, foot);
        let (rig, ik) = FootIk::from_rig(hip, knee, foot);
        world.insert(root, rig);
        world.insert(root, ik);
        // 平台顶 y = 0.55：踝(0.1)上抬 0.5 → 射线起点 0.6 可命中顶面
        world.spawn((
            Transform::from_xyz(0.0, -0.45, 0.0),
            Collider::static_box(Vec3::new(50.0, 1.0, 50.0)),
        ));
        let joints = vec![hip, knee, foot];
        let before = joint_bits(&world, &joints);
        update_foot_ik(&world);
        assert_eq!(
            joint_bits(&world, &joints),
            before,
            "过近域目标（< |upper − lower|）应放弃本帧修正：姿势 bit 级不动"
        );
    }

    #[test]
    fn sensor_volume_is_not_ground() {
        // 脚正下方的 sensor 盒（事件域，顶面恰在射线起点处）不参与
        // IK 落地——若被当作地面，t=0 命中会把踝上拉到起点高度
        let (mut world, _root, hip, knee, foot) = rig(Vec3::new(0.0, 1.0, 0.0));
        world.spawn((
            Transform::from_xyz(0.0, 0.25, 0.0),
            Collider::sensor_box(Vec3::new(1.0, 0.25, 1.0)),
        ));
        let joints = vec![hip, knee, foot];
        let before = joint_bits(&world, &joints);
        update_foot_ik(&world);
        assert_eq!(
            joint_bits(&world, &joints),
            before,
            "sensor 事件域不应作为 IK 落地面：姿势 bit 级不动"
        );
    }

    #[test]
    fn disabled_collider_is_not_ground() {
        // 碰撞组清零（标记失效，如游戏侧的开门标记：组位/掩码 = 0 →
        // 配对恒假）的静态体不参与 IK 落地——射线起点在其体内也不命中
        let (mut world, _root, hip, knee, foot) = rig(Vec3::new(0.0, 1.0, 0.0));
        let mut disabled = Collider::static_box(Vec3::new(1.0, 1.25, 0.2));
        disabled.groups = 0;
        disabled.mask = 0;
        world.spawn((Transform::from_xyz(0.0, 1.25, 0.0), disabled));
        let joints = vec![hip, knee, foot];
        let before = joint_bits(&world, &joints);
        update_foot_ik(&world);
        assert_eq!(
            joint_bits(&world, &joints),
            before,
            "碰撞组清零（标记失效）的静态体不应作为 IK 落地面"
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

    // ---- 节点映射（打磨 V）：IkRig 与 AnimationPlayer 二选一 ----

    /// 纯 IK 装配（只挂 IkRig，无 AnimationPlayer）：链 IK 照常生效——
    /// §8.22② 的「被迫挂零剪辑播放器」绕行已不需要。
    #[test]
    fn pure_ik_rig_without_animation_player_works() {
        // 与 slope 同场景：山球斜坡 + 纯 IkRig 链
        let r = 10.0f32;
        let mut world = World::new();
        let root = world.spawn((Transform::from_xyz(2.0, 0.75, 0.0),));
        let hip = world.spawn((Transform::IDENTITY,));
        let knee = world.spawn((Transform::from_xyz(0.0, -0.5, 0.0),));
        let foot = world.spawn((Transform::from_xyz(0.0, -0.5, 0.0),));
        world.add_child(root, hip);
        world.add_child(hip, knee);
        world.add_child(knee, foot);
        let (rig, ik) = FootIk::from_rig(hip, knee, foot);
        world.insert(root, rig);
        world.insert(root, ik);
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
            "纯 IkRig 链的脚踝应贴合斜坡 ±2 cm：踝 {} vs 面 {expected}",
            ankle.y
        );
    }

    /// 两者并存（AnimationPlayer + IkRig 同挂）：IkRig 优先——播放器的
    /// 节点表是坏数据（全 None，会触发链契约断言的配置）也不影响。
    #[test]
    fn ik_rig_takes_precedence_over_animation_player() {
        let mut world = World::new();
        // 根在 y 0.75：地面顶 -0.2 距髋 0.95 < 全腿长 1.0（可达域内，
        // 踝会精确落到地面——修正发生即证明 IkRig 路径生效）
        let root = world.spawn((Transform::from_xyz(0.0, 0.75, 0.0),));
        let hip = world.spawn((Transform::IDENTITY,));
        let knee = world.spawn((Transform::from_xyz(0.0, -0.5, 0.0),));
        let foot = world.spawn((Transform::from_xyz(0.0, -0.5, 0.0),));
        world.add_child(root, hip);
        world.add_child(hip, knee);
        world.add_child(knee, foot);
        // 播放器在位但节点表为坏数据（全 None）；IkRig 提供正确映射
        let player = AnimationPlayer::new(vec![], vec![None, None, None]);
        world.insert(root, player);
        let (rig, ik) = FootIk::from_rig(hip, knee, foot);
        world.insert(root, rig);
        world.insert(root, ik);
        world.spawn((
            Transform::from_xyz(0.0, -1.2, 0.0),
            Collider::static_box(Vec3::new(50.0, 1.0, 50.0)),
        ));
        update_foot_ik(&world);
        propagate_transforms(&mut world);
        let ankle = world.get::<GlobalTransform>(foot).unwrap().position();
        assert!(
            (ankle.y - (-0.2)).abs() < 1e-4,
            "IkRig 应优先解析节点（踝被拉到地面 -0.2），实际 {ankle:?}"
        );
    }

    // ---- scale 污染监测（打磨 ④）：链上节点 scale 恒 1 的契约保护 ----

    /// 组装「root → hip → knee → foot」链并按需给节点 scale，返回
    /// (world, root, foot)。
    fn rig_with_scale(root_pos: Vec3, knee_scale: f32) -> (World, Entity, Entity) {
        let mut world = World::new();
        let root = world.spawn((Transform::from_xyz(root_pos.x, root_pos.y, root_pos.z),));
        let hip = world.spawn((Transform::IDENTITY,));
        let knee = world.spawn((Transform {
            position: Vec3::new(0.0, -0.5, 0.0),
            scale: Vec3::splat(knee_scale),
            ..Transform::IDENTITY
        },));
        let foot = world.spawn((Transform::from_xyz(0.0, -0.5, 0.0),));
        world.add_child(root, hip);
        world.add_child(hip, knee);
        world.add_child(knee, foot);
        let (rig, ik) = FootIk::from_rig(hip, knee, foot);
        world.insert(root, rig);
        world.insert(root, ik);
        (world, root, foot)
    }

    /// scale = 0.5 污染（胫骨缩半）：显式期望 0.5/0.5 → 偏差 50% > 10%
    /// → warn 恰好一次（防刷屏：warn 过即记）。
    #[test]
    fn scale_pollution_triggers_warn_once() {
        let (world, root, _foot) = rig_with_scale(Vec3::new(0.0, 1.0, 0.0), 0.5);
        if let Some(mut ik) = world.get_mut::<FootIk>(root) {
            ik.expected_upper = 0.5;
            ik.expected_lower = 0.5;
        }
        update_foot_ik(&world);
        assert!(
            world.get::<FootIk>(root).unwrap().scale_warned,
            "胫骨被节点 scale 缩半应触发告警"
        );
        update_foot_ik(&world);
        update_foot_ik(&world);
        assert!(
            world.get::<FootIk>(root).unwrap().scale_warned,
            "告警状态保持（每实体至多一次的标志位语义）"
        );
    }

    /// scale = 1 的健康链：不触发告警；期望长未显式填写时由首次解算
    /// 捕获（捕获值 ≈ 装配骨长 0.5）。
    #[test]
    fn healthy_chain_captures_expected_lengths_without_warn() {
        let (world, root, _foot) = rig_with_scale(Vec3::new(0.0, 1.0, 0.0), 1.0);
        update_foot_ik(&world);
        let ik = world.get::<FootIk>(root).unwrap();
        assert!(!ik.scale_warned, "健康链不应触发告警");
        assert!(
            (ik.expected_upper - 0.5).abs() < 1e-5 && (ik.expected_lower - 0.5).abs() < 1e-5,
            "期望骨长应由首次解算捕获为装配值 0.5（实际 {} / {}）",
            ik.expected_upper,
            ik.expected_lower
        );
    }

    // ---- 回写四元数归一化（打磨 VI 摩擦① 的永久回归锁）----

    /// 摩擦① 场景的最小复刻：剪辑**只有髋的下探平移通道**（4–12 cm /
    /// 2 s 循环；下探而非上抬——上抬会让目标超出可达域被门放弃），髋与膝
    /// 的旋转没有任何通道覆写 ⇒ IK 写回的旋转每帧作为下一帧复合链的输入。
    /// 幅度**不回到装配全长**（髋踝距离恒 < 1.0）：膝在全伸配置处几何
    /// 奇异（h → 0），采样时刻的微小差异被放大成 mm 级膝位差——那不是
    /// 漂移，是病态条件；同相位位形须在良定义域内比较。
    /// 修复前范数逐帧平方复合（R48 实测 idle 角色 ~1.9 s 内
    /// 386 → 5.8e7 → NaN，随后骨长 NaN 撞 [`solve_two_bone_ik`] 的正值
    /// 断言 panic）；修复后跑 600 帧（10 s = 5 个整循环）逐帧断言：
    /// ① 旋转分量有限；② ‖q‖ − 1 ≤ 1e-3；③ 骨长保持 0.5；④ 末帧与
    /// 同相位（提前 2 个整循环 = 120 帧）的膝 / 踝**世界位置**一致
    /// ≤ 1e-3。
    ///
    /// ④ 为何比位置不比旋转：最小旋转弧只约束**骨方向**，四元数沿骨轴的
    /// 自转（twist）分量不参与任何关节位置、且由写入历史逐帧携带——它是
    /// 本 IK 表述的自由度（装配上由网格朝向吸收），同相位两帧的 twist 可
    /// 差 ~0.5° 而位形完全一致。范数爆炸则必然破坏位置与骨长，故位置口径
    /// 既稳定又是有效锁。
    #[test]
    fn foot_ik_quaternion_norm_stable_without_clip_overwrite() {
        use crate::animation::{
            AnimationChannel, AnimationClip, AnimationInterpolation, AnimationPath,
            update_animations,
        };
        use iris_core::prelude::Time;
        use std::time::Duration;

        let (mut world, root, hip, knee, foot) = rig(Vec3::new(0.0, 1.0, 0.0));
        platform(&mut world, 0.0);
        let clip = AnimationClip {
            name: "idle".into(),
            channels: vec![AnimationChannel {
                node: 0, // hip：只写平移，旋转通道缺席 = 本测试的前提
                path: AnimationPath::Translation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 1.0, 2.0],
                values: vec![0.0, -0.04, 0.0, 0.0, -0.12, 0.0, 0.0, -0.04, 0.0],
            }],
            duration: 2.0,
            events: Vec::new(),

            windows: Vec::new(),
            curves: Vec::new(),
        };
        world.insert(
            root,
            AnimationPlayer::new(vec![clip], vec![Some(hip), Some(knee), Some(foot)]),
        );
        world.insert_resource(Time::default());

        // 每帧位形快照（膝 / 踝世界位置）供同相位比对
        let mut poses: Vec<(Vec3, Vec3)> = Vec::new();
        let dt = Duration::from_secs_f64(1.0 / 60.0);
        for frame in 0..600 {
            world.resource_mut::<Time>().advance(dt);
            update_animations(&world);
            update_foot_ik(&world);
            for &(joint, what) in &[(hip, "hip"), (knee, "knee")] {
                let rotation = world.get::<Transform>(joint).unwrap().rotation;
                assert!(
                    rotation.is_finite(),
                    "第 {frame} 帧 {what} 旋转出现非有限分量（平方复合爆炸的直接表现）：{rotation:?}"
                );
                let norm = rotation.length();
                assert!(
                    (norm - 1.0).abs() <= 1e-3,
                    "第 {frame} 帧 {what} 旋转范数偏离单位超过 1e-3：‖q‖ = {norm}"
                );
            }
            let hip_world = chain_world(&world, hip).unwrap().0.w_axis.truncate();
            let knee_world = chain_world(&world, knee).unwrap().0.w_axis.truncate();
            let foot_world = chain_world(&world, foot).unwrap().0.w_axis.truncate();
            assert!(
                ((knee_world - hip_world).length() - 0.5).abs() <= 1e-3
                    && ((foot_world - knee_world).length() - 0.5).abs() <= 1e-3,
                "第 {frame} 帧骨长偏离装配值 0.5：位形已失真"
            );
            poses.push((knee_world, foot_world));
        }

        // 剪辑周期 2 s = 120 帧：第 600 帧与第 480 帧同相位（都是 t = 0）
        let (a_knee_pos, a_foot_pos) = poses[479];
        let (b_knee_pos, b_foot_pos) = poses[599];
        assert!(
            (a_knee_pos - b_knee_pos).length() <= 1e-3
                && (a_foot_pos - b_foot_pos).length() <= 1e-3,
            "同相位两帧的关节世界位应一致（不逐循环漂移）：膝差 {} 踝差 {}",
            (a_knee_pos - b_knee_pos).length(),
            (a_foot_pos - b_foot_pos).length()
        );
    }
}
