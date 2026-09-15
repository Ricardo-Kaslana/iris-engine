//! 链式 IK v2（R75）：FABRIK 任意长链 + look-at 瞄准链——与既有
//! 两骨 IK（[`crate::foot_ik`] / [`crate::hand_ik`]）**并存不替换**：
//! 两骨解析解（余弦定理，精确一次到位）继续服务腿 / 臂，本模块补
//! 「三骨以上的软链」（触手 / 尾巴 / 脊柱链）与「瞄准」（头 / 炮塔 /
//! 脊柱跟随视线）两类两骨解覆盖不了的需求。
//!
//! # FABRIK 链（[`ChainIk`] + [`update_chain_ik`]）
//!
//! 纯函数 [`solve_fabrik`]：标准 FABRIK 前后向交替迭代——后向遍把末端
//! 拉到目标、自末向根按骨长重排各关节；前向遍把根**钉回原位**（根是
//! 锚，链不能脱离身体）、自根向末重排。**确定性契约：固定迭代数上限
//! 加数据驱动的早退**（末端距目标 ≤ `tolerance` 即停——同输入同停点，
//! 无墙钟无随机）；两端点重合等退化方向取 +Y 兜底（与
//! [`crate::foot_ik::solve_two_bone_ik`] 同款约定）。**不可达目标 =
//! 拉直指靶**：目标超总链长时把链沿根→目标方向按累计骨长摆直——这是
//! FABRIK 的自然收敛态，也是软链想要的视觉（触手尽力伸向够不着的
//! 点），**刻意不做**手部 IK 式放弃门：手部的门防的是「authored 的
//! 异常握把位把臂拉成直线指天」，软链的伸长是期望行为。
//!
//! 回写与足 / 手部同构：骨方向旧 → 新的最小旋转弧
//! （`Quat::from_rotation_arc`）左乘旧世界旋转得新世界旋转，再按
//! 父级（链内取**新**值、链外父级不变）反推局部旋转并 **`normalize()`
//! （打磨 VI 摩擦①的纪律：IK 是唯一覆写不被剪辑重置的骨旋转的系统，
//! 归一化责任归 IK）**。**末端节点的局部旋转与位置都不写**（跟随末骨
//! 方向、保持原局部偏移）——与足 / 手部「末节点局部位置校正」的刻意
//! 差异：FABRIK 骨长取自**当前世界姿势**，若把末端按解算位回写局部
//! 位置，写回的偏移长会喂进下一帧的骨长测量，误差成为**吸收态**
//! （无回归力）逐帧游走（实现期探针实测 600 帧内 0.25 → 0.251 破
//! 容差）；不写则全部局部偏移恒定，骨长**结构性**严格保持，末端
//! 世界位 = 末骨新方向 × 原偏移长，与解算位的差在浮点噪声级（≪
//! 解算容差 1e-4——迭代解本就不承诺解析精确落地，与两骨解的精确
//! 到位语义不同）。
//!
//! # look-at 瞄准（[`LookIk`] + [`update_look_ik`]）
//!
//! 把末端节点的**瞄准轴**（`aim_axis`，末端节点局部系，默认 −Z 引擎
//! 前方）转到指向目标。**单遍分布式求解（闭式，非迭代）**：先算总
//! 摆动 `S`（当前瞄准方向 → 目标方向的最小旋转弧），再给链上第 i 个
//! 节点施加份额 `q = S^(w/n)`（`n` = 节点数、`w` = 权重，轴角幂
//! [`swing_fraction`]）。数学：每个节点的世界旋转左乘共轭后的 q，
//! 复合到末端恰为 `S^w` ——权重 1 时瞄准方向**精确**指向目标方向。
//!
//! 份额的推导（为何是 `S^{1/n}` 而不是「每节点转 S」）：世界系下给
//! 节点 i 及其整个子树施加旋转 q，末端瞄准方向获得 q；链上 n 个节点
//! 依次施加同一 q，末端复合 n 份 = `q^n`。令 `q = S^{1/n}` 则末端恰得
//! `S`；写成局部旋转为 `local_i = (world_{i-1})⁻¹ · q · world_i`
//! （父级取**旧**世界旋转——q 是世界系份额，共轭进局部系）。
//!
//! **诚实边界（近目标残差）**：`S` 按修正前的末端位置计算，份额施加
//! 过程中关节旋转会移动末端位置 ⇒ 目标方向随之微变——单遍不回算。
//! 残差量级 ∝（末端位移 × sin 摆角）/ 目标距离：头 / 炮塔瞄准（目标
//! 米级远、末端位移厘米级）< 0.5°，可感知性为零；目标贴近末端
//! （< 数倍链长）时残差增大，v1 不做迭代精化（下一帧会以新姿势重解，
//! 误差不累积——每帧瞄准都是「从当前方向到目标方向」的全量摆动）。
//!
//! # 两系统的公共约定（与足 / 手部同款）
//!
//! - **节点映射**：[`crate::foot_ik::IkRig`]（优先）或
//!   `AnimationPlayer::node_entities` 二选一；`start_node` +
//!   `node_count` 取 `node_entities` 的**连续下标段**（链节点在节点
//!   表中连续排列是装配惯例），段内节点须 `ChildOf` 直连成链
//!   （node[i+1] 的父 = node[i]），否则 `debug_assert!` 并跳过
//!   （release 静默）；
//! - **目标**：复用 [`crate::hand_ik::IkTarget`] 标记组件，读目标实体
//!   `GlobalTransform` 全局平移（上一帧传播产物，一帧滞后；挂点勿置于
//!   链子树内——反馈环，见 hand_ik 模块文档）；目标为 `None` / 实体
//!   失效 / 无 `GlobalTransform` / 自身 `Visibility::HIDDEN` → 本帧
//!   放弃（不写任何变换）；
//! - **链上节点 scale 恒 1 + 均匀缩放假设**：`chain_world` 沿父链累乘，
//!   节点 scale 会缩水骨长（足部 IK 有期望骨长偏差监测，本模块骨长
//!   逐帧取当前姿势实际距离、无状态无监测——契约本身见 §8.22④）；
//! - **回写幂等门**：链 IK 末端已在 `tolerance` 内 / look-at 已在
//!   瞄准容差内（瞄准方向与目标方向夹角 < ~0.08°）→ 不写任何变换
//!   （静止目标下姿势 bit 级稳定，不写不漂）；
//! - **编排**：DefaultPlugins 注册序 `update_animations →
//!   update_foot_ik → update_hand_ik → update_chain_ik →
//!   update_look_ik`（同段 IK 作用于同一份本帧局部姿势依次生效，
//!   注册序确定 ⇒ 结果确定）；look-at 收尾——瞄准是姿态的最后一笔，
//!   读到的链位形包含全部前序修正。
//! - **无关节角限制（v1 诚实边界）**：两套解算都不做关节角度 / 旋转
//!   限制（ elbows locked、锥形限制）——软链视觉通常可接受，需要时
//!   由游戏侧剪辑通道逐帧重置 + 本层重解吸收。确定性：全程纯函数 +
//!   固定遍历序，同输入两次调用逐位一致（单测守护）。
//!
//! # 用法
//!
//! ```ignore
//! // 触手：4 节点链（IkRig 连续段 0..4）追一个移动目标
//! let tip_target = world.spawn((Transform::from_xyz(0.0, 0.5, -0.6), IkTarget));
//! world.insert(root, IkRig::new(vec![Some(n0), Some(n1), Some(n2), Some(n3)]));
//! world.insert(root, ChainIk { node_count: 4, target: Some(tip_target), ..Default::default() });
//! // 头部瞄准：脊柱 3 节点分配摆动，瞄准轴 −Z，权重 0.8（软跟踪）
//! world.insert(head_root, LookIk { node_count: 3, target: Some(aim_point),
//!                                  weight: 0.8, ..Default::default() });
//! ```

use glam::{Quat, Vec3};
use iris_core::prelude::{Entity, GlobalTransform, Transform};
use iris_core::world::World;

use crate::animation::AnimationPlayer;
use crate::foot_ik::{IkRig, MIN_BONE_LENGTH, chain_world};
use crate::visibility::Visibility;

/// 方向退化阈值：两端点距离平方低于此值时方向向量不可信，取 +Y 兜底
/// （与 [`crate::foot_ik::solve_two_bone_ik`] 的「目标与髋重合」同款）。
const DEGENERATE_DIR_EPS: f32 = 1e-9;

/// look-at 已瞄准门：瞄准方向与目标方向点积高于此值（夹角 < ~0.08°）
/// 视为已在瞄准，本帧不写任何变换（静止目标下姿势 bit 级稳定）。
const LOOK_AT_EPS_DOT: f32 = 1.0 - 1e-6;

/// FABRIK 纯函数：任意长链的末端到达解。
///
/// `joints` 为当前关节世界位（`joints[0]` = 根，**锚定不动**；骨长取
/// 相邻关节当前距离），把末端拉向 `target`：可达时迭代收敛至
/// `|end − target| ≤ tolerance`（早退，数据驱动 ⇒ 确定性）；不可达
/// （目标距根 ≥ 总链长）时沿根→目标方向按累计骨长**摆直**返回。
/// 返回新的关节位表（长度与输入一致；根逐位等于输入根）。
///
/// 确定性契约：固定迭代上限 + 数据驱动早退，同输入两次调用逐位一致；
/// 迭代中两端点重合等退化方向取 +Y 兜底。
///
/// # Panics
/// 关节数 < 2、`iterations` 为 0、任一骨长非正（≤
/// [`MIN_BONE_LENGTH`] 量级）时断言失败（中文消息）——零长骨无 IK
/// 语义（系统层已按 [`MIN_BONE_LENGTH`] 预拦截，纯函数防御独立成立）。
pub fn solve_fabrik(joints: &[Vec3], target: Vec3, iterations: usize, tolerance: f32) -> Vec<Vec3> {
    assert!(
        joints.len() >= 2,
        "FABRIK 至少两个节点（当前 {}）",
        joints.len()
    );
    assert!(iterations > 0, "FABRIK 迭代数须为正（当前 {iterations}）");
    let lengths: Vec<f32> = joints.windows(2).map(|w| (w[1] - w[0]).length()).collect();
    assert!(
        lengths.iter().all(|&len| len > MIN_BONE_LENGTH),
        "FABRIK 链骨长须为正：{lengths:?}（零长骨无 IK 语义）"
    );
    let total: f32 = lengths.iter().sum();
    let root = joints[0];
    // 不可达：沿根→目标方向按累计骨长摆直（FABRIK 的自然收敛态）
    let to_target = target - root;
    let reach = to_target.length();
    if reach >= total {
        let dir = if reach > DEGENERATE_DIR_EPS.sqrt() {
            to_target / reach
        } else {
            // 目标与根重合：方向退化取 −Y（链自然下垂方向，确定性兜底）
            Vec3::NEG_Y
        };
        let mut result = Vec::with_capacity(joints.len());
        let mut acc = 0.0;
        result.push(root);
        for &len in &lengths {
            acc += len;
            result.push(root + dir * acc);
        }
        return result;
    }
    // 可达域：前后向交替迭代（早退数据驱动，确定性）
    let mut current: Vec<Vec3> = joints.to_vec();
    for _ in 0..iterations {
        let end = current[current.len() - 1];
        if (end - target).length() <= tolerance {
            break;
        }
        // 后向遍：末端钉到目标，自末向根按骨长重排
        let n = current.len();
        current[n - 1] = target;
        for i in (0..n - 1).rev() {
            let offset = pull_direction(current[i], current[i + 1]);
            current[i] = current[i + 1] + offset * lengths[i];
        }
        // 前向遍：根钉回原位（锚），自根向末按骨长重排
        current[0] = root;
        for i in 1..n {
            let offset = pull_direction(current[i], current[i - 1]);
            current[i] = current[i - 1] + offset * lengths[i - 1];
        }
    }
    current
}

/// 重排方向：`(from − to)` 的单位向量——关节 from 被放在 to 旁
/// `length` 距离处的方向。两端重合（退化）取 +Y 兜底（确定性）。
fn pull_direction(from: Vec3, to: Vec3) -> Vec3 {
    let offset = from - to;
    let length_sq = offset.length_squared();
    if length_sq > DEGENERATE_DIR_EPS {
        offset / length_sq.sqrt()
    } else {
        Vec3::Y
    }
}

/// 摆动份额（轴角幂）：`swing` 的 `fraction` 倍旋转。
///
/// `swing` 取最短弧（`Quat::from_rotation_arc` 产物，`w ≥ 0` ⇒ 角
/// ≤ π），`fraction ∈ [0, 1]` 时份额同轴缩短；look-at 的分布数学
/// （`S^{w/n}`）与权重压幅都走本函数。恒等输入（角 0）恒等输出
/// （`from_axis_angle(任意轴, 0) = 恒等`）。
pub fn swing_fraction(swing: Quat, fraction: f32) -> Quat {
    let (axis, angle) = swing.to_axis_angle();
    Quat::from_axis_angle(axis, angle * fraction)
}

/// FABRIK 链 IK 配置组件：`node_entities` 连续段 `[start_node,
/// start_node + node_count)` 声明一条 `ChildOf` 直连链，末端追
/// [`IkTarget`] 目标。每实体独立开关：没挂本组件的实体零参与。
#[derive(Clone, Debug, iris_core::Component)]
pub struct ChainIk {
    /// 链首节点（`node_entities` 下标，段的起点）。
    pub start_node: usize,
    /// 链上节点数（含末端；≥ 2）。**三骨以上才是本模块的目标场景**——
    /// 两 / 三节点的腿臂链请用 [`crate::foot_ik::FootIk`] /
    /// [`crate::hand_ik::HandIk`]（解析解，一次到位）。
    pub node_count: usize,
    /// 末端目标：挂 [`IkTarget`] 的实体。`None` / 失效 / 无
    /// `GlobalTransform` / 隐藏 → 本帧放弃（语义见模块文档）。
    pub target: Option<Entity>,
    /// 迭代上限（固定数——确定性契约，默认 8；软链典型 4–10 次收敛
    /// 到毫米级）。
    pub iterations: usize,
    /// 早退容差（米，默认 1e-4）：末端距目标低于此值停止迭代；也是
    /// 系统层的「已在目标」幂等门——静止目标不写不漂。
    pub tolerance: f32,
}

impl Default for ChainIk {
    /// 常规装配：段起点 0、三节点、无目标、8 次迭代、容差 1e-4。
    fn default() -> Self {
        Self {
            start_node: 0,
            node_count: 3,
            target: None,
            iterations: 8,
            tolerance: 1e-4,
        }
    }
}

/// look-at 瞄准 IK 配置组件：`node_entities` 连续段声明一条 `ChildOf`
/// 直连链，末端节点的 [`LookIk::aim_axis`]（局部系）指向 [`IkTarget`]
/// 目标——链上节点均分总摆动（份额 `S^(weight/n)`）。每实体独立开关。
#[derive(Clone, Debug, iris_core::Component)]
pub struct LookIk {
    /// 链首节点（`node_entities` 下标，段的起点）。
    pub start_node: usize,
    /// 链上节点数（≥ 1；1 = 单节点硬瞄准如炮塔，多节点 = 脊柱 / 颈链
    /// 分配摆动）。
    pub node_count: usize,
    /// 瞄准目标：挂 [`IkTarget`] 的实体，缺席 / 失效 / 隐藏语义同
    /// [`ChainIk::target`]。
    pub target: Option<Entity>,
    /// 瞄准轴（**末端节点局部系**，默认 −Z 引擎前方）。零向量无效
    /// （debug_assert 并跳过）；不必归一（系统内部归一）。
    pub aim_axis: Vec3,
    /// 总摆动权重 ∈ [0, 1]（默认 1.0）：0 = 不摆（本帧放弃）、
    /// 0.5 = 摆一半角（软跟踪）。超域值 clamp 进 [0, 1]。
    pub weight: f32,
}

impl Default for LookIk {
    /// 常规装配：段起点 0、三节点、无目标、瞄准轴 −Z、权重 1。
    fn default() -> Self {
        Self {
            start_node: 0,
            node_count: 3,
            target: None,
            aim_axis: Vec3::NEG_Z,
            weight: 1.0,
        }
    }
}

/// 解析节点段：`[start, start + count)` 下标映射齐备且 `ChildOf` 直连
/// 成链（node[i+1] 的父 = node[i]）时返回节点实体表，否则 `None`
/// （debug_assert 已在内部发出，release 静默跳过）。
fn resolve_chain(
    world: &World,
    player: Option<&AnimationPlayer>,
    rig: Option<&IkRig>,
    start: usize,
    count: usize,
) -> Option<Vec<Entity>> {
    let resolve = |index: usize| -> Option<Entity> {
        let mapped = rig
            .and_then(|rig| rig.node_entities.get(index).copied().flatten())
            .or_else(|| {
                player.and_then(|player| player.node_entities.get(index).copied().flatten())
            });
        if mapped.is_none() {
            debug_assert!(
                false,
                "链式 IK 节点索引 {index} 越界或未映射到实体（IkRig /                 AnimationPlayer 至少挂一个且表长覆盖段 [{start}, {start}+{count}))"
            );
        }
        mapped
    };
    let mut nodes = Vec::with_capacity(count);
    for offset in 0..count {
        nodes.push(resolve(start + offset)?);
    }
    for pair in nodes.windows(2) {
        if world.parent(pair[1]) != Some(pair[0]) {
            debug_assert!(
                false,
                "链式 IK 节点须直连成链（node[i+1] 的父 = node[i]）：{pair:?}"
            );
            return None;
        }
    }
    Some(nodes)
}

/// 目标解析：`IkTarget` 实体的 `GlobalTransform` 全局平移；缺席 /
/// 失效 / 无全局位 / 隐藏 → `None`（本帧放弃，语义见模块文档）。
fn resolve_target(world: &World, target: Option<Entity>) -> Option<Vec3> {
    let target = target?;
    let position = world
        .get::<GlobalTransform>(target)
        .map(|global| global.position())?;
    if world
        .get::<Visibility>(target)
        .is_some_and(|visibility| !visibility.visible)
    {
        return None;
    }
    Some(position)
}

/// FABRIK 链 IK 每帧系统：对每个挂有 [`ChainIk`] 且节点段齐备的实体，
/// 把链末端拉到目标（算法与边界见模块文档）。
///
/// 只读世界签名（`&World`）：组件写访问走世界内部的尝试型锁，注册为
/// **并行系统**（访问集：写 Transform，读 ChainIk / IkTarget / IkRig /
/// AnimationPlayer / ChildOf / GlobalTransform / Visibility）。两阶段：
/// 只读解算收集回写任务 → 统一写局部变换（照 `update_foot_ik` 模式）。
pub fn update_chain_ik(world: &World) {
    // 阶段一（只读解算）：每链产出「节点局部旋转表」（末端节点不写，
    // 见模块文档「回写」——局部偏移恒定 ⇒ 骨长结构性严格保持）
    let mut jobs: Vec<Vec<(Entity, Quat)>> = Vec::new();
    world.query::<(Entity, &ChainIk, Option<&AnimationPlayer>, Option<&IkRig>)>(
        |(_entity, ik, player, rig)| {
            if ik.node_count < 2 {
                debug_assert!(false, "ChainIk 节点数须 ≥ 2（当前 {}）", ik.node_count);
                return;
            }
            let Some(target) = resolve_target(world, ik.target) else {
                return;
            };
            let Some(nodes) = resolve_chain(world, player, rig, ik.start_node, ik.node_count)
            else {
                return;
            };
            // 链姿势（本帧局部变换复合，动画之后读取——见模块文档「编排」）
            let mut olds: Vec<(Vec3, Quat)> = Vec::with_capacity(nodes.len());
            for &node in &nodes {
                let Some((matrix, rotation)) = chain_world(world, node) else {
                    return;
                };
                olds.push((matrix.w_axis.truncate(), rotation));
            }
            let lengths: Vec<f32> = olds
                .windows(2)
                .map(|w| (w[1].0 - w[0].0).length())
                .collect();
            if lengths.iter().any(|&len| len <= MIN_BONE_LENGTH) {
                return;
            }
            let end_old = olds[olds.len() - 1].0;
            // 幂等门：末端已在容差内 → 不写任何变换（静止目标 bit 级稳定）
            if (end_old - target).length() <= ik.tolerance {
                return;
            }
            let positions: Vec<Vec3> = olds.iter().map(|&(pos, _)| pos).collect();
            let solved = solve_fabrik(&positions, target, ik.iterations, ik.tolerance);
            // 骨方向旧 → 新的最小旋转弧（世界系），左乘旧世界旋转
            let mut world_rots_new: Vec<Quat> = Vec::with_capacity(nodes.len() - 1);
            for i in 0..nodes.len() - 1 {
                let dir_old = (olds[i + 1].0 - olds[i].0) / lengths[i];
                let len_new = (solved[i + 1] - solved[i]).length();
                if len_new <= MIN_BONE_LENGTH {
                    return; // 解算退化（理论不可达，防御）
                }
                let dir_new = (solved[i + 1] - solved[i]) / len_new;
                world_rots_new.push(Quat::from_rotation_arc(dir_old, dir_new) * olds[i].1);
            }
            // 局部旋转：链内父级取**新**世界旋转，链外父级不变；
            // normalize 是 IK 的责任（打磨 VI 摩擦①，推导见 foot_ik）
            let mut rotations: Vec<(Entity, Quat)> = Vec::with_capacity(nodes.len() - 1);
            for i in 0..nodes.len() - 1 {
                let parent_world_rot = if i == 0 {
                    match world.parent(nodes[0]) {
                        Some(parent) => match chain_world(world, parent) {
                            Some((_, rotation)) => rotation,
                            None => return,
                        },
                        None => Quat::IDENTITY,
                    }
                } else {
                    world_rots_new[i - 1]
                };
                rotations.push((
                    nodes[i],
                    (parent_world_rot.conjugate() * world_rots_new[i]).normalize(),
                ));
            }
            jobs.push(rotations);
        },
    );

    // 阶段二（写入）：链节点局部旋转（位置全部不动——骨长结构性保持，
    // 末端节点整体不写，见模块文档「回写」）
    for rotations in jobs {
        for (node, rot) in rotations {
            if let Some(mut transform) = world.get_mut::<Transform>(node) {
                transform.rotation = rot;
            }
        }
    }
}

/// look-at 瞄准 IK 每帧系统：对每个挂有 [`LookIk`] 且节点段齐备的实体，
/// 把末端节点的瞄准轴摆向目标（分布数学与残差边界见模块文档）。
///
/// 只读世界签名与两阶段模式同 [`update_chain_ik`]（访问集：写
/// Transform，读 LookIk / IkTarget / IkRig / AnimationPlayer / ChildOf /
/// GlobalTransform / Visibility）。
pub fn update_look_ik(world: &World) {
    // 阶段一（只读解算）：每链产出「节点局部旋转表」
    let mut jobs: Vec<Vec<(Entity, Quat)>> = Vec::new();
    world.query::<(Entity, &LookIk, Option<&AnimationPlayer>, Option<&IkRig>)>(
        |(_entity, ik, player, rig)| {
            if ik.node_count < 1 {
                debug_assert!(false, "LookIk 节点数须 ≥ 1（当前 {}）", ik.node_count);
                return;
            }
            if !ik.weight.is_finite() || !ik.aim_axis.is_finite() {
                debug_assert!(false, "LookIk weight / aim_axis 须为有限值");
                return;
            }
            let weight = ik.weight.clamp(0.0, 1.0);
            if weight <= 0.0 {
                return; // 零权重 = 不摆（本帧放弃）
            }
            let Some(target) = resolve_target(world, ik.target) else {
                return;
            };
            let Some(nodes) = resolve_chain(world, player, rig, ik.start_node, ik.node_count)
            else {
                return;
            };
            // 瞄准轴归一（局部系 → 末端世界系方向）
            let axis_len = ik.aim_axis.length();
            if axis_len <= MIN_BONE_LENGTH {
                debug_assert!(false, "LookIk aim_axis 为零向量（无瞄准语义）");
                return;
            }
            let aim_local = ik.aim_axis / axis_len;
            // 链姿势：末端位 / 朝向 + 各节点世界旋转（分布数学要用）
            let mut world_rots: Vec<Quat> = Vec::with_capacity(nodes.len());
            for &node in &nodes {
                let Some((_, rotation)) = chain_world(world, node) else {
                    return;
                };
                world_rots.push(rotation);
            }
            let end = nodes[nodes.len() - 1];
            let Some((end_matrix, _)) = chain_world(world, end) else {
                return;
            };
            let end_pos = end_matrix.w_axis.truncate();
            let aim = (world_rots[world_rots.len() - 1] * aim_local).normalize();
            let to_target = target - end_pos;
            if to_target.length_squared() <= DEGENERATE_DIR_EPS {
                return; // 目标与末端重合：无方向可瞄
            }
            let to_target_dir = to_target / to_target.length();
            // 幂等门：已在瞄准容差内 → 不写任何变换
            if aim.dot(to_target_dir) >= LOOK_AT_EPS_DOT {
                return;
            }
            // 总摆动 S 与每节点份额 q = S^(w/n)（推导见模块文档）
            let swing = Quat::from_rotation_arc(aim, to_target_dir);
            let share = swing_fraction(swing, weight / nodes.len() as f32);
            // 局部旋转 = 旧父世界旋转⁻¹ · q · 旧世界旋转（q 是世界系份额，
            // 共轭进局部系；父级取旧值——份额定义在世界系，见模块文档）
            let mut rotations: Vec<(Entity, Quat)> = Vec::with_capacity(nodes.len());
            for (i, &node) in nodes.iter().enumerate() {
                let parent_world_rot = if i == 0 {
                    match world.parent(node) {
                        Some(parent) => match chain_world(world, parent) {
                            Some((_, rotation)) => rotation,
                            None => return,
                        },
                        None => Quat::IDENTITY,
                    }
                } else {
                    world_rots[i - 1]
                };
                rotations.push((
                    node,
                    (parent_world_rot.conjugate() * share * world_rots[i]).normalize(),
                ));
            }
            jobs.push(rotations);
        },
    );

    // 阶段二（写入）：节点局部旋转（位置全部不动——旋转绕关节枢轴，
    // 骨长严格保持）
    for rotations in jobs {
        for (node, rot) in rotations {
            if let Some(mut transform) = world.get_mut::<Transform>(node) {
                transform.rotation = rot;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hand_ik::IkTarget;
    use iris_core::hierarchy::propagate_transforms;
    use iris_core::physics::Collider;
    use iris_core::prelude::GlobalTransform;

    /// 组装「root → n 节点直连链」（每骨 `bone` 长、沿 −Y 下垂）+
    /// 播放器（node_entities 覆盖链）+ 指定 IK 组件。
    fn rig<Ik: iris_core::component::Component>(
        root_pos: Vec3,
        count: usize,
        bone: f32,
        ik: Ik,
    ) -> (World, Entity, Vec<Entity>) {
        let mut world = World::new();
        let root = world.spawn((Transform::from_xyz(root_pos.x, root_pos.y, root_pos.z),));
        let mut nodes = Vec::new();
        let mut parent = root;
        for _ in 0..count {
            let node = world.spawn((Transform::from_xyz(0.0, -bone, 0.0),));
            world.add_child(parent, node);
            parent = node;
            nodes.push(node);
        }
        let mapping: Vec<Option<Entity>> = nodes.iter().map(|&n| Some(n)).collect();
        world.insert(root, AnimationPlayer::new(vec![], mapping));
        world.insert(root, ik);
        (world, root, nodes)
    }

    /// 目标实体：`IkTarget` + 直接插入 `GlobalTransform`（模拟上一帧
    /// 传播产物，与 hand_ik 测试同款）。
    fn target(world: &mut World, pos: Vec3) -> Entity {
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

    // ---- 纯函数：solve_fabrik ----

    /// 不可达（目标距根 ≥ 总链长）：沿根→目标按累计骨长摆直——解析可
    /// 预言每个关节位（3-4-5 直角三角形方向，累计 0.5 / 1.0）。
    #[test]
    fn fabrik_unreachable_straightens_analytically() {
        let joints = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.0, -0.5, 0.0),
            Vec3::new(0.0, -1.0, 0.0),
        ];
        // |target − root| = |(0.6, −0.8)| = 1.0 = 总链长（边界即不可达）
        let target = Vec3::new(0.6, -0.8, 0.0);
        let solved = solve_fabrik(&joints, target, 8, 1e-4);
        assert_eq!(solved[0], joints[0], "根锚定不动（逐位）");
        assert!(
            (solved[1] - Vec3::new(0.3, -0.4, 0.0)).length() < 1e-6,
            "中途关节应在根→目标方向累计 0.5 处，实际 {:?}",
            solved[1]
        );
        assert!(
            (solved[2] - target).length() < 1e-6,
            "末端应在目标处（总长恰达），实际 {:?}",
            solved[2]
        );
    }

    /// 可达 + 需弯折：足够迭代内收敛到容差；根逐位锚定；骨长保持。
    #[test]
    fn fabrik_reachable_converges_and_preserves_lengths() {
        let joints = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.0, -0.5, 0.0),
            Vec3::new(0.0, -1.0, 0.0),
        ];
        let target = Vec3::new(0.6, -0.4, 0.0); // 距根 0.72 < 1.0，需弯折
        let solved = solve_fabrik(&joints, target, 32, 1e-4);
        assert_eq!(solved[0], joints[0], "根锚定不动（逐位）");
        assert!(
            (solved[2] - target).length() <= 1e-4,
            "末端应收敛到容差内，差 {}",
            (solved[2] - target).length()
        );
        for pair in solved.windows(2) {
            assert!(
                ((pair[1] - pair[0]).length() - 0.5).abs() < 1e-5,
                "骨长应保持 0.5，实际 {}",
                (pair[1] - pair[0]).length()
            );
        }
    }

    /// 确定性：同输入两次调用逐位一致。
    #[test]
    fn fabrik_is_bitwise_deterministic() {
        let joints = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.0, -0.5, 0.0),
            Vec3::new(0.0, -1.0, 0.0),
        ];
        let a = solve_fabrik(&joints, Vec3::new(0.6, -0.4, 0.0), 8, 1e-4);
        let b = solve_fabrik(&joints, Vec3::new(0.6, -0.4, 0.0), 8, 1e-4);
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(
                (x.x.to_bits(), x.y.to_bits(), x.z.to_bits()),
                (y.x.to_bits(), y.y.to_bits(), y.z.to_bits()),
                "同输入两次 FABRIK 应逐位一致：{x:?} vs {y:?}"
            );
        }
    }

    #[test]
    #[should_panic(expected = "至少两个节点")]
    fn fabrik_single_node_panics() {
        solve_fabrik(&[Vec3::ZERO], Vec3::Y, 8, 1e-4);
    }

    #[test]
    #[should_panic(expected = "迭代数须为正")]
    fn fabrik_zero_iterations_panics() {
        solve_fabrik(&[Vec3::ZERO, Vec3::NEG_Y], Vec3::Y, 0, 1e-4);
    }

    #[test]
    #[should_panic(expected = "骨长须为正")]
    fn fabrik_zero_length_bone_panics() {
        solve_fabrik(&[Vec3::ZERO, Vec3::ZERO, Vec3::NEG_Y], Vec3::Y, 8, 1e-4);
    }

    // ---- 纯函数：swing_fraction ----

    /// 份额 = 同轴角缩放：90° 摆动的 1/3 = 30°（解析轴角对照）。
    #[test]
    fn swing_fraction_scales_angle_on_same_axis() {
        let swing = Quat::from_axis_angle(Vec3::Y, std::f32::consts::FRAC_PI_2);
        let third = swing_fraction(swing, 1.0 / 3.0);
        let expected = Quat::from_axis_angle(Vec3::Y, std::f32::consts::FRAC_PI_6);
        assert!(
            (third.x - expected.x).abs() < 1e-6
                && (third.y - expected.y).abs() < 1e-6
                && (third.z - expected.z).abs() < 1e-6
                && (third.w - expected.w).abs() < 1e-6,
            "S^(1/3) 应为同轴 30°：{third:?} vs {expected:?}"
        );
        assert!(
            (swing_fraction(swing, 0.0) - Quat::IDENTITY).length() < 1e-7,
            "份额 0 = 恒等"
        );
        assert!(
            (swing_fraction(swing, 1.0) - swing).length() < 1e-7,
            "份额 1 = 原摆动"
        );
    }

    /// 恒等摆动任何份额都是恒等（from_axis_angle(任意轴, 0)）。
    #[test]
    fn swing_fraction_identity_is_identity() {
        let share = swing_fraction(Quat::IDENTITY, 0.37);
        assert!((share - Quat::IDENTITY).length() < 1e-7, "{share:?}");
    }

    // ---- 世界级：update_chain_ik ----

    /// 4 节点触手（3 骨 × 0.25）追侧向目标：末端精确落目标（容差级）、
    /// 骨长保持、根世界位不动。
    #[test]
    fn chain_ik_reaches_target() {
        let ik = ChainIk {
            node_count: 4,
            ..ChainIk::default()
        };
        let (mut world, _root, nodes) = rig(Vec3::new(0.0, 1.0, 0.0), 4, 0.25, ik);
        let goal = target(&mut world, Vec3::new(0.5, 0.4, 0.0));
        world.get_mut::<ChainIk>(_root).unwrap().target = Some(goal);
        update_chain_ik(&world);
        propagate_transforms(&mut world);
        let tip = world.get::<GlobalTransform>(nodes[3]).unwrap().position();
        assert!(
            (tip - Vec3::new(0.5, 0.4, 0.0)).length() <= 1e-3,
            "末端应精确落到目标（容差级），实际 {tip:?}"
        );
        let mut positions = vec![Vec3::new(0.0, 1.0, 0.0)];
        positions.extend(
            nodes
                .iter()
                .map(|&n| world.get::<GlobalTransform>(n).unwrap().position()),
        );
        for pair in positions.windows(2) {
            assert!(
                ((pair[1] - pair[0]).length() - 0.25).abs() < 1e-3,
                "骨长应保持 0.25，实际 {}",
                (pair[1] - pair[0]).length()
            );
        }
    }

    /// 不可达目标：链拉直指靶（末端 = 锚点 + 方向 × 总链长；锚点 =
    /// 链首节点世界位 (0, 0.75, 0)——链首位置永不动，与足部 IK 的髋
    /// 同款）——软链的期望行为，非放弃门（与手部 IK 的刻意差异）。
    #[test]
    fn chain_ik_unreachable_stretches_toward_target() {
        let ik = ChainIk {
            node_count: 4,
            ..ChainIk::default()
        };
        let (mut world, _root, nodes) = rig(Vec3::new(0.0, 1.0, 0.0), 4, 0.25, ik);
        // 目标与锚点等高（y = 0.75）：拉直方向纯 +X，期望可解析预言
        let goal = target(&mut world, Vec3::new(2.0, 0.75, 0.0));
        world.get_mut::<ChainIk>(_root).unwrap().target = Some(goal);
        update_chain_ik(&world);
        propagate_transforms(&mut world);
        let tip = world.get::<GlobalTransform>(nodes[3]).unwrap().position();
        let expected = Vec3::new(0.75, 0.75, 0.0); // 锚点 + (1,0,0) × 0.75
        assert!(
            (tip - expected).length() < 1e-3,
            "不可达应拉直指靶（末端在 {expected:?}），实际 {tip:?}"
        );
    }

    /// 目标缺席 / 隐藏：本帧放弃，姿势逐位不动。
    #[test]
    fn chain_ik_missing_or_hidden_target_skips_bitwise() {
        let build = || {
            let ik = ChainIk {
                node_count: 3,
                ..ChainIk::default()
            };
            rig(Vec3::new(0.0, 1.0, 0.0), 3, 0.25, ik)
        };
        let (world, _root, nodes) = build();
        let before = joint_bits(&world, &nodes);
        update_chain_ik(&world); // target = None
        assert_eq!(joint_bits(&world, &nodes), before, "无目标应跳过");
        let (mut world, root, nodes) = build();
        let goal = target(&mut world, Vec3::new(0.5, 0.4, 0.0));
        world.insert(goal, Visibility::HIDDEN);
        world.get_mut::<ChainIk>(root).unwrap().target = Some(goal);
        let before = joint_bits(&world, &nodes);
        update_chain_ik(&world);
        assert_eq!(joint_bits(&world, &nodes), before, "隐藏目标应跳过");
    }

    /// 幂等门：末端已在容差内（目标 = 当前末端）→ 不写任何变换。
    #[test]
    fn chain_ik_already_at_target_skips_bitwise() {
        let ik = ChainIk {
            node_count: 3,
            ..ChainIk::default()
        };
        let (mut world, root, nodes) = rig(Vec3::new(0.0, 1.0, 0.0), 3, 0.25, ik);
        // 当前末端世界位 = (0, 1 − 0.75, 0)
        let goal = target(&mut world, Vec3::new(0.0, 0.25, 0.0));
        world.get_mut::<ChainIk>(root).unwrap().target = Some(goal);
        let before = joint_bits(&world, &nodes);
        update_chain_ik(&world);
        assert_eq!(joint_bits(&world, &nodes), before, "已在目标应跳过");
    }

    /// 确定性：同配置两世界交替推进逐位一致。
    #[test]
    fn chain_ik_is_bitwise_deterministic() {
        let build = || {
            let ik = ChainIk {
                node_count: 4,
                ..ChainIk::default()
            };
            let (mut world, root, nodes) = rig(Vec3::new(0.0, 1.0, 0.0), 4, 0.25, ik);
            let goal = target(&mut world, Vec3::new(0.5, 0.4, 0.0));
            world.get_mut::<ChainIk>(root).unwrap().target = Some(goal);
            (world, nodes)
        };
        let (world_a, nodes_a) = build();
        let (world_b, nodes_b) = build();
        for _ in 0..3 {
            update_chain_ik(&world_a);
            update_chain_ik(&world_b);
            assert_eq!(
                joint_bits(&world_a, &nodes_a),
                joint_bits(&world_b, &nodes_b),
                "同配置两次链 IK 应逐位一致"
            );
        }
    }

    /// 仅 debug：链契约（node[i+1] 的父 = node[i]）debug_assert（目标
    /// 有效是前置——缺席目标在链契约检查之前就放弃了）。
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "直连成链")]
    fn chain_ik_broken_chain_panics_in_debug() {
        let mut world = World::new();
        let root = world.spawn((Transform::IDENTITY,));
        let a = world.spawn((Transform::from_xyz(0.0, -0.25, 0.0),));
        let b = world.spawn((Transform::from_xyz(0.0, -0.25, 0.0),));
        world.add_child(root, a); // b 悬空：a 的子不是 b
        let goal = target(&mut world, Vec3::new(0.5, 0.5, 0.0));
        let player = AnimationPlayer::new(vec![], vec![Some(a), Some(b)]);
        world.insert(root, player);
        world.insert(
            root,
            ChainIk {
                node_count: 2,
                target: Some(goal),
                ..ChainIk::default()
            },
        );
        update_chain_ik(&world);
    }

    /// 范数回归锁（打磨 VI 摩擦①的链式版）：无剪辑覆写的链追**移动**
    /// 目标 600 帧，逐帧断言旋转有限、‖q‖ − 1 ≤ 1e-3、骨长保持——
    /// 写回非单位四元数会在无覆写路径逐帧平方复合爆炸（R48 教训）。
    /// 目标按固定脚本绕圆移动（纯 f32 三角，无随机无墙钟）；两世界
    /// 同脚本推进逐位一致（确定性）。
    #[test]
    fn chain_ik_norm_stable_chasing_scripted_target() {
        let ik = ChainIk {
            node_count: 4,
            ..ChainIk::default()
        };
        let (mut world, _root, nodes) = rig(Vec3::new(0.0, 1.0, 0.0), 4, 0.25, ik);
        let goal = target(&mut world, Vec3::new(0.5, 0.4, 0.0));
        world.get_mut::<ChainIk>(_root).unwrap().target = Some(goal);
        for frame in 0..600u32 {
            // 目标绕 (0.45, 0.55) 半径 0.15 的圆移动（60 帧一圈的约数角速）
            let angle = frame as f32 * 0.13;
            let pos = Vec3::new(0.45 + angle.cos() * 0.15, 0.55 + angle.sin() * 0.15, 0.0);
            *world.get_mut::<GlobalTransform>(goal).unwrap() =
                GlobalTransform::from_transform(Transform::from_xyz(pos.x, pos.y, pos.z));
            update_chain_ik(&world);
            for &node in &nodes {
                let rotation = world.get::<Transform>(node).unwrap().rotation;
                assert!(rotation.is_finite(), "第 {frame} 帧出现非有限分量");
                assert!(
                    (rotation.length() - 1.0).abs() <= 1e-3,
                    "第 {frame} 帧范数偏离单位：{}",
                    rotation.length()
                );
            }
            let mut positions: Vec<Vec3> = Vec::new();
            for &node in &nodes {
                positions.push(chain_world(&world, node).unwrap().0.w_axis.truncate());
            }
            for pair in positions.windows(2) {
                assert!(
                    ((pair[1] - pair[0]).length() - 0.25).abs() <= 1e-3,
                    "第 {frame} 帧骨长失真"
                );
            }
        }
    }

    // ---- 世界级：update_look_ik ----

    /// 单节点（炮塔式）：瞄准轴 −Z 摆到 +X 方向 = 绕 Y −90°（解析对照：
    /// Ry(−90°) 把 −Z 转到 +X；目标与节点等高 ⇒ to_target 纯 +X）。
    #[test]
    fn look_ik_single_node_aims_exactly() {
        let ik = LookIk {
            node_count: 1,
            ..LookIk::default()
        };
        let (mut world, root, nodes) = rig(Vec3::new(0.0, 1.5, 0.0), 1, 0.25, ik);
        let goal = target(&mut world, Vec3::new(1.0, 1.25, 0.0)); // 节点世界位 y=1.25
        world.get_mut::<LookIk>(root).unwrap().target = Some(goal);
        update_look_ik(&world);
        let rotation = world.get::<Transform>(nodes[0]).unwrap().rotation;
        let expected = Quat::from_axis_angle(Vec3::Y, -std::f32::consts::FRAC_PI_2);
        assert!(
            (rotation - expected).length() < 1e-6,
            "单节点应整体摆 Ry(−90°)：{rotation:?} vs {expected:?}"
        );
    }

    /// 三节点链均分 90° 摆动：每个局部 = Ry(−30°)（份额 S^(1/3) 的解析
    /// 对照）；末端世界瞄准 = +X 精确（目标与末端等高 ⇒ to_target 纯 +X，
    /// 末端世界位 = root(0,1.5) − 3×0.5 = (0,0,0)）。
    #[test]
    fn look_ik_chain_distributes_uniformly() {
        let ik = LookIk {
            node_count: 3,
            ..LookIk::default()
        };
        let (mut world, root, nodes) = rig(Vec3::new(0.0, 1.5, 0.0), 3, 0.5, ik);
        let goal = target(&mut world, Vec3::new(1.0, 0.0, 0.0));
        world.get_mut::<LookIk>(root).unwrap().target = Some(goal);
        update_look_ik(&world);
        let expected = Quat::from_axis_angle(Vec3::Y, -std::f32::consts::FRAC_PI_6);
        for &node in &nodes {
            let rotation = world.get::<Transform>(node).unwrap().rotation;
            assert!(
                (rotation - expected).length() < 1e-6,
                "三节点均分：每个局部应 = Ry(−30°)，实际 {rotation:?}"
            );
        }
        // 末端世界瞄准方向（局部复合）：三份 30° 复合 = −90°，−Z → +X
        let (_, end_world_rot) = chain_world(&world, nodes[2]).unwrap();
        let aim = end_world_rot * Vec3::NEG_Z;
        assert!(
            (aim - Vec3::X).length() < 1e-5,
            "末端瞄准应精确为 +X，实际 {aim:?}"
        );
    }

    /// 权重 0.5：总摆动减半（单节点 45°）——软跟踪压幅。
    #[test]
    fn look_ik_weight_halves_swing() {
        let ik = LookIk {
            node_count: 1,
            weight: 0.5,
            ..LookIk::default()
        };
        let (mut world, root, nodes) = rig(Vec3::new(0.0, 1.5, 0.0), 1, 0.25, ik);
        let goal = target(&mut world, Vec3::new(1.0, 1.25, 0.0));
        world.get_mut::<LookIk>(root).unwrap().target = Some(goal);
        update_look_ik(&world);
        let rotation = world.get::<Transform>(nodes[0]).unwrap().rotation;
        let expected = Quat::from_axis_angle(Vec3::Y, -std::f32::consts::FRAC_PI_4);
        assert!(
            (rotation - expected).length() < 1e-6,
            "权重 0.5 应摆一半角 Ry(−45°)：{rotation:?}"
        );
    }

    /// 零权重 / 无目标 / 目标与末端重合：本帧放弃，姿势逐位不动。
    #[test]
    fn look_ik_skip_gates_bitwise() {
        let build = || {
            let ik = LookIk {
                node_count: 3,
                ..LookIk::default()
            };
            rig(Vec3::new(0.0, 1.5, 0.0), 3, 0.5, ik)
        };
        let (mut world, root, nodes) = build();
        let before = joint_bits(&world, &nodes);
        update_look_ik(&world); // target = None
        assert_eq!(joint_bits(&world, &nodes), before, "无目标应跳过");
        world.get_mut::<LookIk>(root).unwrap().weight = 0.0;
        let goal = target(&mut world, Vec3::new(1.0, 1.5, 0.0));
        world.get_mut::<LookIk>(root).unwrap().target = Some(goal);
        update_look_ik(&world);
        assert_eq!(joint_bits(&world, &nodes), before, "零权重应跳过");
        world.get_mut::<LookIk>(root).unwrap().weight = 1.0;
        // 目标与末端世界位重合（末端 (0, 0, 0)... root(0,1.5) − 3×0.5）：
        let goal2 = target(&mut world, Vec3::new(0.0, 0.0, 0.0));
        world.get_mut::<LookIk>(root).unwrap().target = Some(goal2);
        update_look_ik(&world);
        assert_eq!(joint_bits(&world, &nodes), before, "目标与末端重合应跳过");
    }

    /// 已在瞄准容差内（目标恰在瞄准轴延长线上）→ 不写任何变换。
    #[test]
    fn look_ik_already_aiming_skips_bitwise() {
        let ik = LookIk {
            node_count: 3,
            ..LookIk::default()
        };
        let (mut world, root, nodes) = rig(Vec3::new(0.0, 1.5, 0.0), 3, 0.5, ik);
        // 瞄准轴 −Z（世界系）：目标放在末端正 −Z 方向
        let goal = target(&mut world, Vec3::new(0.0, 0.0, -2.0));
        world.get_mut::<LookIk>(root).unwrap().target = Some(goal);
        let before = joint_bits(&world, &nodes);
        update_look_ik(&world);
        assert_eq!(joint_bits(&world, &nodes), before, "已在瞄准应跳过");
    }

    /// 确定性 + 范数锁：三节点链追移动目标 600 帧，逐帧旋转有限、
    /// ‖q‖ − 1 ≤ 1e-3、骨长保持（与链式版同款回归锁）。
    #[test]
    fn look_ik_norm_stable_chasing_scripted_target() {
        let ik = LookIk {
            node_count: 3,
            ..LookIk::default()
        };
        let (mut world, _root, nodes) = rig(Vec3::new(0.0, 1.5, 0.0), 3, 0.5, ik);
        let goal = target(&mut world, Vec3::new(1.0, 1.5, 0.0));
        world.get_mut::<LookIk>(_root).unwrap().target = Some(goal);
        for frame in 0..600u32 {
            let angle = frame as f32 * 0.11;
            let pos = Vec3::new(
                3.0 * angle.cos(),
                1.5 + 0.5 * angle.sin(),
                -3.0 * angle.sin(),
            );
            *world.get_mut::<GlobalTransform>(goal).unwrap() =
                GlobalTransform::from_transform(Transform::from_xyz(pos.x, pos.y, pos.z));
            update_look_ik(&world);
            for &node in &nodes {
                let rotation = world.get::<Transform>(node).unwrap().rotation;
                assert!(rotation.is_finite(), "第 {frame} 帧出现非有限分量");
                assert!(
                    (rotation.length() - 1.0).abs() <= 1e-3,
                    "第 {frame} 帧范数偏离单位：{}",
                    rotation.length()
                );
            }
            let mut positions: Vec<Vec3> = Vec::new();
            for &node in &nodes {
                positions.push(chain_world(&world, node).unwrap().0.w_axis.truncate());
            }
            for pair in positions.windows(2) {
                assert!(
                    ((pair[1] - pair[0]).length() - 0.5).abs() <= 1e-3,
                    "第 {frame} 帧骨长失真"
                );
            }
        }
    }

    /// 链式 + look-at 并存：同世界两套组件（不同链）各自解算互不干扰
    /// （挂不同实体；编排序 chain → look，见模块文档）。
    #[test]
    fn chain_and_look_ik_coexist() {
        let mut world = World::new();
        // 链 A：FABRIK 触手（root_a 下 4 节点）
        let root_a = world.spawn((Transform::from_xyz(0.0, 1.0, 0.0),));
        let mut nodes_a = Vec::new();
        let mut parent = root_a;
        for _ in 0..4 {
            let node = world.spawn((Transform::from_xyz(0.0, -0.25, 0.0),));
            world.add_child(parent, node);
            parent = node;
            nodes_a.push(node);
        }
        world.insert(
            root_a,
            AnimationPlayer::new(vec![], nodes_a.iter().map(|&n| Some(n)).collect()),
        );
        world.insert(
            root_a,
            ChainIk {
                node_count: 4,
                ..ChainIk::default()
            },
        );
        // 链 B：look-at 头链（root_b 下 3 节点）
        let root_b = world.spawn((Transform::from_xyz(2.0, 1.5, 0.0),));
        let mut nodes_b = Vec::new();
        let mut parent = root_b;
        for _ in 0..3 {
            let node = world.spawn((Transform::from_xyz(0.0, -0.5, 0.0),));
            world.add_child(parent, node);
            parent = node;
            nodes_b.push(node);
        }
        world.insert(
            root_b,
            AnimationPlayer::new(vec![], nodes_b.iter().map(|&n| Some(n)).collect()),
        );
        world.insert(
            root_b,
            LookIk {
                node_count: 3,
                ..LookIk::default()
            },
        );
        let goal_a = target(&mut world, Vec3::new(0.5, 0.4, 0.0));
        let goal_b = target(&mut world, Vec3::new(3.0, 0.0, 0.0)); // 末端 (2,0,0) 等高 → 纯 +X
        world.get_mut::<ChainIk>(root_a).unwrap().target = Some(goal_a);
        world.get_mut::<LookIk>(root_b).unwrap().target = Some(goal_b);
        update_chain_ik(&world);
        update_look_ik(&world);
        propagate_transforms(&mut world);
        let tip = world.get::<GlobalTransform>(nodes_a[3]).unwrap().position();
        assert!(
            (tip - Vec3::new(0.5, 0.4, 0.0)).length() <= 1e-3,
            "链 A 末端应落目标（与 B 无关），实际 {tip:?}"
        );
        let (_, end_rot_b) = chain_world(&world, nodes_b[2]).unwrap();
        let aim = end_rot_b * Vec3::NEG_Z;
        assert!(
            (aim - Vec3::X).length() < 1e-5,
            "链 B 末端应瞄准 +X（root_b 在 x=2、目标 x=3），实际 {aim:?}"
        );
    }

    // ---- 静态体查询隔离（FABRIK 不做射线，这里锁「零静态体依赖」） ----

    /// 空世界（无任何 Collider / 静态体）链 IK 照常解算——FABRIK 目标
    /// 是玩法量不是射线（与足部落地查询的刻意差异，见模块文档）。
    #[test]
    fn chain_ik_needs_no_statics() {
        let ik = ChainIk {
            node_count: 3,
            ..ChainIk::default()
        };
        let (mut world, root, nodes) = rig(Vec3::new(0.0, 1.0, 0.0), 3, 0.25, ik);
        let _ = world.spawn((
            Transform::IDENTITY,
            Collider::dynamic_sphere(0.5, 0.0),
            iris_core::prelude::RigidBody::default(),
        )); // 世界里有动态体也不影响
        let goal = target(&mut world, Vec3::new(0.3, 0.5, 0.0));
        world.get_mut::<ChainIk>(root).unwrap().target = Some(goal);
        update_chain_ik(&world);
        propagate_transforms(&mut world);
        let tip = world.get::<GlobalTransform>(nodes[2]).unwrap().position();
        assert!(
            (tip - Vec3::new(0.3, 0.5, 0.0)).length() <= 1e-3,
            "无静态体世界链 IK 应照常解算，实际 {tip:?}"
        );
    }
}
