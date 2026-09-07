//! # Ragdoll 与死亡混合（v1 球肢链 / **v2 胶囊肢人形**，R50）
//!
//! 「死亡混合」（death blending）的形态是**运动学窗口**：角色死亡的
//! 瞬间不直接进入物理模拟，而是先生成**静态**的肢体（动画接管期——游戏
//! /动画照常驱动肢体 [`Transform`](crate::transform::Transform)，或直接
//! 定格死亡姿势），窗口到期后调 [`release_ragdoll`] 把全部肢体翻转为
//! 动体（物理接管期——重力 / 碰撞 / 关节照常生效）。两阶段语义清晰、
//! 各自可实现，中间没有第三种「半物理」状态。
//!
//! # 两条路线：v1 球肢 vs v2 胶囊肢
//!
//! | | **v1（R37）** | **v2（R50）** |
//! |---|---|---|
//! | 预设 | [`RagdollDesc::humanoid`]（六球：头 / 躯干 / 双手 / 双脚） | [`RagdollDesc::humanoid_v2`]（十肢：躯干胶囊 + 头球 + 上臂 / 前臂 / 大腿 / 小腿各二） |
//! | 肢体形状 | 球（`half_length ≤ radius`） | 胶囊（`half_length > radius`，轴 = `axis`） |
//! | 朝向 | 无语义（各向同性） | `Transform.rotation` 由 `axis` 生成、运行期由关节修正 |
//! | 关节 | [`DistanceJoint`](crate::physics::DistanceJoint)（中心距硬约束） | [`SwingJoint`](crate::physics::SwingJoint)（锚点 pin + 摆动锥限位 + 肢体轴定向） |
//! | 死亡翻滚 | 无（球不需要旋转） | [`release_ragdoll`] 注入冲量相关角速度 |
//! | 消费者 | iris-assault / iris-infiltration（照旧，逐位不变） | 需要写实尸体的场景（`examples/ragdoll_v2_snapshot.rs`） |
//!
//! v1 的**球肢近似**是当年「纯位置物理、无角速度」下的诚实选择：球各向
//! 同性，翻滚 / 拖拽 / 堆叠语义全部正确且**不需要旋转**。R50 补齐了角速度
//! 积分与旋转感知胶囊，v2 因此可以诚实重建「四肢有朝向、关节有限位」的
//! 人形——**v1 一字未改**（同一函数、同一 desc 类型；球肢 / 距离关节分支
//! 的形状、rest 算式、挂载方向与 R37 逐位一致，既有测试零改动全过）。
//!
//! # v2 自动接线（[`spawn_ragdoll_kinematic`]）
//!
//! `links` 里 `swing_limit = Some(limit)` 的肢体对按肢体几何自动接线，
//! 无需手写锚点：
//!
//! - **关节点** = 子端肢体的**近端帽中心**（`中心 + rot·Y × (half_length −
//!   radius)`；球肢 = 朝父端的表面点）；
//! - `anchor_a = 0`（子端中心）⇒ `|rest_offset|` = 轴半长 ⇒ 距离约束等效
//!   「近端帽钉在关节窝」；
//! - `anchor_b` = 关节点在**父端本地系**的坐标（关节窝随父端翻滚）；
//! - `rest_offset` = 「关节点 − 子端中心」在**父端本地系**的向量 ⇒ 长度给
//!   距离 rest、方向给锥轴（直立人形每段都是 `(0, 轴半长, 0)`）。
//!
//! 前提：`links` 须**父先子后**（父端的朝向在子端之前确定；摆动关节是
//! 层级投影，根先叶后的求解序才能保证每轮末尾全部约束精确满足——见
//! [`SwingJoint`](crate::physics::SwingJoint) 的实测论证）。
//!
//! # 死亡翻滚与翻倒转轴（[`release_ragdoll`]）
//!
//! 冲量注入的同时写入角速度 `ω = topple_axis × 强度 ×`
//! [`RAGDOLL_SPIN_FACTOR`]。转轴**优先绕链根（`limbs[0]`）的肩线**（本地
//! +X 的水平投影，符号取头顶倒向冲量的一侧）⇒ 尸体**前扑 / 后仰**而不是
//! 侧倒。理由（实测）：侧倒时躯干侧躺，靠地一侧的肩窝落到地面以下（肩偏
//! 0.25 > 躯干半径 0.18），而「接触不产生角冲量」⇒ 被钉在肩窝上的手臂
//! 转不出来、稳态埋进土里 0.15–0.37 m；绕肩线翻倒双肩等高，同一场景埋深
//! 降到 0.028 m（在 5e-2 的求解残余容差内）。
//!
//! # 自碰撞免抖（groups / mask 过滤）
//!
//! 全部肢体同组位（[`RAGDOLL_GROUP`]，bit 0）且 `mask` 排除该位
//! （见 [`crate::physics`]「碰撞过滤掩码」）→ **肢体互不碰撞**。理由：
//! 生成时相邻肢体刻意相切 / 微重叠（链的视觉连续性），若互相求解会
//! 立刻被挤开炸裂。诚实限制：**无肢体间挤压，堆叠全靠地面 / 场景**
//! ——且全部 ragdoll 共用同一个组位，两具 ragdoll 的肢体也互不碰撞
//! （位组按语义层细分留游戏侧 / 未来轮）。与世界的碰撞照常：世界侧
//! 默认全 1 的 `groups` / `mask` 在对称判定
//! `(a.groups & b.mask) != 0 && (b.groups & a.mask) != 0` 下双向认可。
//!
//! # 两阶段语义细节
//!
//! - **动画接管期**：肢体是静态体（`Collider::is_static = true`；生成即
//!   带 [`RigidBody`] 但被静态语义忽略——is_static 取或）。静态体会挡
//!   玩家 / 弹道（合理语义：尸体挡路）；关节在此期被
//!   [`step_physics`](crate::physics::step_physics) 跳过（挂载体须为动体
//!   ——动画期姿势由游戏驱动，关节无语义）；release 前游戏侧可先驱动
//!   肢体 `Transform` 摆到死亡姿势（**v2 注意**：摆姿势要连 `rotation`
//!   一起写，胶囊肢的朝向是碰撞语义的一部分）；
//! - **物理接管期**：[`release_ragdoll`] 把 `is_static` 翻转为 `false`
//!   并写入继承速度 + 可选冲量（`(方向, 强度)`，作用于全部肢体——受击
//!   方向踢飞）+ **角速度注入**（v2 的翻倒）。翻转后关节自动生效（v1 的
//!   距离约束把肢体链拉成刚体拓扑；v2 的摆动约束另加限位与肢体定向），
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
//! - **v2：接触不产生角冲量**（引擎级限制）——尸体落地不会自动「滚起来」，
//!   翻滚全靠释放时注入的 ω + 关节方向修正；躯干（链根）没有父关节，
//!   它的最终朝向由 ω 与角阻尼决定（渐近总转角 = ω₀/λ，[`RAGDOLL_SPIN_FACTOR`]
//!   的取值据此让躯干恰好躺平）；
//! - **v2：侧倒仍可能埋臂**——翻倒转轴绕肩线只解决了「注入即侧倒」的主
//!   路径；若尸体被场景 / 后续冲量翻成侧躺，靠地一侧的肩窝仍会低于地面
//!   ⇒ 该侧手臂被关节钉进土里（上限 ≈ 肩偏 − 躯干半径 + 臂半径）。根治
//!   需要躯干改多胶囊 / OBB 或接触产出角冲量（后续轮）；
//! - **v2：锥限位不区分弯曲平面**——肘 / 膝的「只能单向弯」需要 hinge +
//!   twist，v2 用锥近似（超限方向一律拉回锥面）；
//! - **确定性**：生成序 = `limbs` 定义序、关节求解序 = 组件存储序、无
//!   随机源——同 desc 两次全流程（位置 + **旋转**）逐位一致（单测守护）。
//!
//! # 死亡接管完整用例
//!
//! ```ignore
//! use iris_core::prelude::*;
//!
//! // 1. 死亡瞬间：按死亡姿势生成静态肢体（动画接管期，照常挡玩家/弹道）
//! let limbs = spawn_ragdoll_kinematic(&mut world, death_pos, &RagdollDesc::humanoid_v2());
//! // ……窗口内游戏侧可继续驱动肢体 Transform（定格 / 播放死亡动画）……
//!
//! // 2. 尸体定型 / 被弹道撞击：物理接管，向受击方向踢飞 + 前扑翻倒
//! release_ragdoll(&world, &limbs, hit_velocity, Some((hit_dir, 4.0)));
//!
//! // 3. 此后 step_physics 照常推进：肢体链塌落地面、关节限位内摊倒不散架
//! ```

use glam::{Quat, Vec3};

use crate::entity::Entity;
use crate::physics::{Collider, ColliderShape, DistanceJoint, RigidBody, SwingJoint};
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

/// 肢体的**角阻尼**（R50 v2）：释放时注入的翻滚角速度按 `e^(−λt)` 衰减
/// ——λ = 1.5 时约 2 s 剩 5%（尸体翻滚几圈后停下，而不是永远打转）。
/// v1 球肢同样带此值（球各向同性，旋转无视觉语义 ⇒ 行为零变化）。
pub const RAGDOLL_LIMB_ANGULAR_DAMPING: f32 = 1.5;

/// 释放时的**角速度注入系数**（R50 v2）：`ω = (Y × 冲量方向).normalize()
/// × 强度 × 本系数`（rad/s）。绕该轴正转把「头顶 +Y」推向冲量方向 ⇒
/// 尸体朝**受击方向翻倒**（而不是直挺挺地平移滑走）。
///
/// 系数 0.8 的取值依据（**渐近总转角 = ω₀ / λ**，λ =
/// [`RAGDOLL_LIMB_ANGULAR_DAMPING`] = 1.5）：强度 3 的冲量给 ω₀ = 2.4
/// rad/s ⇒ 总转角 1.6 rad ≈ 92°——正好把躯干从直立转到**躺平**。实测
/// 1.2（总转角 137°）会让躯干斜插在地面上 45°，把钉在骨盆上的腿压进土里
/// 0.35 m（层级投影下关节精确 ⇒ 父端的错误朝向被忠实传到肢体）。要「翻
/// 两圈再摊平」把冲量强度调大即可（总转角随强度线性）。
pub const RAGDOLL_SPIN_FACTOR: f32 = 0.8;

/// 单个肢体的定义（R50 v2：球肢 / 胶囊肢统一描述）。
///
/// **形状由 `half_length` 与 `radius` 的关系决定**（v1 兼容口径）：
/// - `half_length ≤ radius` ⇒ **球肢**（[`ColliderShape::Sphere`]，v1 语义
///   ——各向同性、旋转无语义，`axis` 被忽略）；v1 预设
///   [`RagdollDesc::humanoid`] 的六肢全部落在此支，产出与 R37 **逐位一致**；
/// - `half_length > radius` ⇒ **胶囊肢**（[`ColliderShape::Capsule`]，v2
///   语义——轴沿 `axis`，生成时写进 `Transform.rotation`，关节由
///   [`SwingJoint`](crate::physics::SwingJoint) 限位与定向）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RagdollLimbDef {
    /// 相对出生点（`pos + offset` = 生成位置，bit 级）的**肢体中心**。
    pub offset: Vec3,
    /// 半径（米）：球肢的半径 / 胶囊肢的端帽半径（垂直轴向的最大半宽）。
    pub radius: f32,
    /// 肢体半长（米）：胶囊肢沿轴向的半长（须 > `radius` 才成胶囊）；
    /// `≤ radius` 即退化为球肢（见类型文档的形状判定）。
    pub half_length: f32,
    /// 胶囊肢的**近端方向**（单位向，世界系）：即肢体本地 +Y 的朝向，
    /// 指向父端关节（悬挂的手臂 = `(0, 1, 0)`——近端帽在肩上）。生成时
    /// 经 `Quat::from_rotation_arc(Y, axis)` 写进 `Transform.rotation`，
    /// 于是 [`SwingJoint`](crate::physics::SwingJoint) 的「肢体 +Y 指向
    /// 关节窝」方向修正口径与生成姿态自洽。球肢忽略（写 `Vec3::Y` 即可）。
    ///
    /// **为什么朝向必须由 desc 给出**（不能从 offset 推）：手臂挂在躯干
    /// 侧面时「肢体中心 → 父端中心」是**水平**的，而手臂的轴是**竖直**的
    /// ——两者正交，偏移信息里根本没有轴向。
    pub axis: Vec3,
    /// 弹性系数（0..1）。
    pub restitution: f32,
}

/// 相邻肢体对的关节语义（R50 v2）。
///
/// - `swing_limit = None` ⇒ **v1 语义**：只挂
///   [`DistanceJoint`](crate::physics::DistanceJoint)（rest = 生成时两端
///   **中心**实际距离、硬约束、轴向阻尼 [`RAGDOLL_JOINT_DAMPING`]），
///   挂在 `b` 指向 `a`（与 R37 逐位同一条挂载方向；距离约束本身对称，
///   a / b 次序无语义）；
/// - `swing_limit = Some(limit)` ⇒ **v2 语义**：自动接线
///   [`SwingJoint`](crate::physics::SwingJoint)（挂在 `a`、target = `b`）
///   ——`a` = **子端 / 肢体侧**、`b` = **父端 / 躯干侧**，次序**有语义**。
///   锚点与 `rest_offset` 由肢体几何自动推出（见
///   [`spawn_ragdoll_kinematic`] 的接线规则），`limit` = 摆动锥半角（弧度，
///   `0..=π`）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RagdollLink {
    /// 肢体对的一端（`None` 时无序；`Some` 时 = 子端 / 关节挂载体）。
    pub a: usize,
    /// 肢体对的另一端（`None` 时 = 距离关节的挂载端；`Some` 时 = 父端 /
    /// 躯干侧 / target）。
    pub b: usize,
    /// 摆动限位（弧度，锥半角）：`None` = v1 距离关节语义，`Some` = v2
    /// 摆动关节语义（见类型文档）。
    pub swing_limit: Option<f32>,
}

/// 布娃娃描述：肢体 + 相邻肢体对的关节语义。生成函数不取所有权，同一
/// desc 可反复使用（确定性：同 pos 同 desc 生成逐位一致）。
///
/// 两个预设：[`RagdollDesc::humanoid`]（v1 六球人形，R37 消费者照旧）与
/// [`RagdollDesc::humanoid_v2`]（R50 十肢胶囊人形 + 关节限位）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RagdollDesc {
    /// 相对出生点的肢体（生成序 = 定义序 = 返回实体序）。
    pub limbs: Vec<RagdollLimbDef>,
    /// 相邻肢体对 → 一条关节（语义见 [`RagdollLink`]）。索引须指向 `limbs`。
    pub links: Vec<RagdollLink>,
}

impl RagdollDesc {
    /// 六球人形（v1，R37）：头 / 躯干 / 双手 / 双脚，直立姿势、脚底贴地；
    /// 肢体全部 `half_length = radius` ⇒ **球肢**（形状与关节与 R37 逐位
    /// 一致）。演示与测试共用的标准 desc——球肢近似的「人」只需要连通性，
    /// 姿势语义（站着死 / 跪着死）由生成位置与动画接管期自由定制。
    ///
    /// 要「四肢有朝向、关节不反折」的写实尸体用 [`RagdollDesc::humanoid_v2`]。
    pub fn humanoid() -> Self {
        // v1 球肢：half_length = radius（退化 ⇒ Sphere）、axis 被忽略
        let sphere = |offset: Vec3, radius: f32, restitution: f32| RagdollLimbDef {
            offset,
            radius,
            half_length: radius,
            axis: Vec3::Y,
            restitution,
        };
        Self {
            limbs: vec![
                // 头
                sphere(Vec3::new(0.0, 1.55, 0.0), 0.18, 0.3),
                // 躯干
                sphere(Vec3::new(0.0, 1.05, 0.0), 0.30, 0.1),
                // 左手
                sphere(Vec3::new(-0.42, 1.10, 0.0), 0.14, 0.2),
                // 右手
                sphere(Vec3::new(0.42, 1.10, 0.0), 0.14, 0.2),
                // 左脚
                sphere(Vec3::new(-0.16, 0.16, 0.0), 0.16, 0.0),
                // 右脚
                sphere(Vec3::new(0.16, 0.16, 0.0), 0.16, 0.0),
            ],
            links: vec![
                RagdollLink {
                    a: 0,
                    b: 1,
                    swing_limit: None,
                },
                RagdollLink {
                    a: 1,
                    b: 2,
                    swing_limit: None,
                },
                RagdollLink {
                    a: 1,
                    b: 3,
                    swing_limit: None,
                },
                RagdollLink {
                    a: 1,
                    b: 4,
                    swing_limit: None,
                },
                RagdollLink {
                    a: 1,
                    b: 5,
                    swing_limit: None,
                },
            ],
        }
    }

    /// 十肢胶囊人形（**v2**，R50）：躯干胶囊 + 头球 + 双臂（上臂 / 前臂）
    /// + 双腿（大腿 / 小腿），直立姿势、脚底贴地（总高 ≈ 1.64 m）。
    ///
    /// **骨架比例（可调）**：各段的 `half_length − radius`（轴半长）首尾
    /// 相接——踝 0.075 → 膝 0.445 → 胯 0.815 → 肩 1.195 → 头顶 1.635；
    /// 相邻肢体**刻意微重叠 0.01 m**（与 v1 球肢「相切 / 微重叠」同款：
    /// 链的视觉连续 + 检测稳健，自碰撞已由组位过滤关掉），单测
    /// `humanoid_v2_skeleton_is_connected` 逐对锁定。
    ///
    /// **关节限位（锥半角，弧度）**：肩 1.9（≈109°，手臂可抬过水平线）、
    /// 肘 2.1（≈120°，可全弯但**不许反折**）、胯 1.4（≈80°）、膝 2.2
    /// （≈126°）。头是球肢 ⇒ 无朝向语义，走 v1 距离关节（`None`）。
    ///
    /// 限位是**锥形**的（不区分弯曲平面）：肘 / 膝的「只能单向弯」需要
    /// hinge + twist 两自由度约束，v2 用锥近似——见
    /// [`SwingJoint`](crate::physics::SwingJoint) 的诚实边界。
    pub fn humanoid_v2() -> Self {
        // 胶囊肢：axis = 近端方向（直立姿势下全部朝上 +Y）
        let bone = |offset: Vec3, radius: f32, half_length: f32, restitution: f32| RagdollLimbDef {
            offset,
            radius,
            half_length,
            axis: Vec3::Y,
            restitution,
        };
        // 摆动关节（a = 子端 / 肢体，b = 父端 / 躯干）
        let swing = |a: usize, b: usize, limit: f32| RagdollLink {
            a,
            b,
            swing_limit: Some(limit),
        };
        Self {
            limbs: vec![
                // 0 躯干（骨盆 0.815 → 肩 1.195，顶面 1.375）
                bone(Vec3::new(0.0, 1.005, 0.0), 0.18, 0.37, 0.1),
                // 1 头（球肢：底 1.365 与躯干顶面 1.375 微重叠 0.01）
                RagdollLimbDef {
                    offset: Vec3::new(0.0, 1.50, 0.0),
                    radius: 0.135,
                    half_length: 0.135,
                    axis: Vec3::Y,
                    restitution: 0.3,
                },
                // 2 / 3 上臂（肩窝在躯干顶帽高度 1.195、外偏 ±0.25 = 躯干
                // 半径 0.18 + 臂半径 0.08 − 微重叠 0.01）
                bone(Vec3::new(-0.25, 1.085, 0.0), 0.08, 0.19, 0.2),
                bone(Vec3::new(0.25, 1.085, 0.0), 0.08, 0.19, 0.2),
                // 4 / 5 前臂（肘 0.975 → 腕 0.765）
                bone(Vec3::new(-0.25, 0.87, 0.0), 0.065, 0.17, 0.2),
                bone(Vec3::new(0.25, 0.87, 0.0), 0.065, 0.17, 0.2),
                // 6 / 7 大腿（胯 0.815 → 膝 0.445）
                bone(Vec3::new(-0.10, 0.63, 0.0), 0.095, 0.28, 0.1),
                bone(Vec3::new(0.10, 0.63, 0.0), 0.095, 0.28, 0.1),
                // 8 / 9 小腿（膝 0.445 → 踝 0.075，脚底 0）
                bone(Vec3::new(-0.10, 0.26, 0.0), 0.075, 0.26, 0.0),
                bone(Vec3::new(0.10, 0.26, 0.0), 0.075, 0.26, 0.0),
            ],
            links: vec![
                // 头：球肢无朝向语义 ⇒ v1 距离关节
                RagdollLink {
                    a: 1,
                    b: 0,
                    swing_limit: None,
                },
                swing(2, 0, 1.9), // 左肩
                swing(3, 0, 1.9), // 右肩
                swing(4, 2, 2.1), // 左肘
                swing(5, 3, 2.1), // 右肘
                swing(6, 0, 1.4), // 左胯
                swing(7, 0, 1.4), // 右胯
                swing(8, 6, 2.2), // 左膝
                swing(9, 7, 2.2), // 右膝
            ],
        }
    }
}

/// 生成静态肢体（**动画接管期**）：`limbs` 定义序逐个 spawn
/// `Transform + Collider + RigidBody`，再按 `links` 挂关节。
///
/// - 肢体 = 静态体（`is_static = true` + [`RAGDOLL_GROUP`] 组位、mask
///   排除该位）：会挡玩家 / 弹道；互不碰撞（自碰撞免抖，见模块文档）；
/// - **形状 / 朝向（R50 v2）**：`half_length ≤ radius` ⇒ 球肢
///   （[`ColliderShape::Sphere`]、旋转恒等 ⇒ 与 R37 逐位一致）；否则 ⇒
///   胶囊肢（[`ColliderShape::Capsule`]、`Transform.rotation =
///   from_rotation_arc(Y, axis)`——本地 +Y 即近端方向）；
/// - **关节接线**（[`RagdollLink`]）：`swing_limit = None` ⇒ v1
///   [`DistanceJoint`]（挂在 `b` 指向 `a`，rest = 生成时两端**中心**实际
///   距离，算式与 R37 逐位一致）；`Some(limit)` ⇒ v2
///   [`SwingJoint`](crate::physics::SwingJoint)（挂在 `a`、target = `b`），
///   锚点与 `rest_offset` 由肢体几何自动推出（[`auto_swing_joint`]）；
/// - 返回实体序 = `desc.limbs` 定义序（后续 [`release_ragdoll`] 的入参）；
/// - **确定性**：同 pos 同 desc 两次生成逐位一致（位置 = `pos + offset`、
///   旋转 = `axis` 的最小弧、关节参数 = 同一算式，无随机源）；
/// - 断言：`radius > 0`、胶囊肢 `axis` 非零、`links` 索引在界内且两端
///   不同、`swing_limit ∈ [0, π]`（中文断言消息）。
pub fn spawn_ragdoll_kinematic(world: &mut World, pos: Vec3, desc: &RagdollDesc) -> Vec<Entity> {
    let mut limbs = Vec::with_capacity(desc.limbs.len());
    // 接线需要「生成时的世界中心与朝向」——与写进 Transform 的同值（确定性）
    let mut centers: Vec<Vec3> = Vec::with_capacity(desc.limbs.len());
    let mut rotations: Vec<Quat> = Vec::with_capacity(desc.limbs.len());
    for (index, def) in desc.limbs.iter().enumerate() {
        assert!(
            def.radius > 0.0,
            "肢体 {index} 的半径 {} 须 > 0（非正半径的肢体无碰撞语义）",
            def.radius
        );
        let rotation = limb_rotation(index, def);
        let center = pos + def.offset;
        limbs.push(world.spawn((
            Transform {
                position: center,
                rotation,
                ..Transform::IDENTITY
            },
            Collider {
                shape: limb_shape(def),
                is_static: true,
                restitution: def.restitution,
                is_sensor: false,
                groups: RAGDOLL_GROUP,
                mask: !RAGDOLL_GROUP,
            },
            RigidBody {
                friction: RAGDOLL_LIMB_FRICTION,
                linear_damping: RAGDOLL_LIMB_DAMPING,
                angular_damping: RAGDOLL_LIMB_ANGULAR_DAMPING,
                ..RigidBody::default()
            },
        )));
        centers.push(center);
        rotations.push(rotation);
    }
    for link in &desc.links {
        let (a, b) = (link.a, link.b);
        assert!(
            a < limbs.len() && b < limbs.len(),
            "链接 ({a}, {b}) 越界：desc 只有 {} 个肢体（索引须指向 limbs）",
            limbs.len()
        );
        assert!(a != b, "链接 ({a}, {b}) 两端同一肢体（自链接无约束语义）");
        match link.swing_limit {
            None => {
                // v1 语义：中心距硬约束（挂在 b 指向 a，与 R37 同一条挂载
                // 方向 ⇒ 组件存储序 / 求解序逐位一致）
                let rest = centers[b].distance(centers[a]);
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
            Some(limit) => {
                assert!(
                    (0.0..=std::f32::consts::PI).contains(&limit),
                    "链接 ({a}, {b}) 的摆动限位 {limit} 须在 [0, π]（锥半角超 π 无几何语义；\
                     要「无限位」请写 None 走 v1 距离关节）"
                );
                world.insert(
                    limbs[a],
                    auto_swing_joint(desc, a, b, limbs[b], &centers, &rotations, limit),
                );
            }
        }
    }
    limbs
}

/// 肢体形状：`half_length > radius` ⇒ 胶囊（轴半长 = `half_length − radius`），
/// 否则 ⇒ 球（v1 语义；退化胶囊与球的**检测**等价，但形状标记不同——v1
/// 消费者按 `Sphere` 匹配、按半径挂球体网格，见 [`RagdollLimbDef`] 文档）。
fn limb_shape(def: &RagdollLimbDef) -> ColliderShape {
    if def.half_length > def.radius {
        ColliderShape::capsule(def.radius, def.half_length)
    } else {
        ColliderShape::Sphere { radius: def.radius }
    }
}

/// 肢体生成朝向：胶囊肢 = 把本地 +Y 以最小弧转到 `axis`（近端方向）；
/// 球肢 = 恒等（各向同性、旋转无语义 ⇒ v1 肢体的 `Transform.rotation`
/// 与 R37 逐位一致）。
fn limb_rotation(index: usize, def: &RagdollLimbDef) -> Quat {
    if def.half_length <= def.radius {
        return Quat::IDENTITY;
    }
    let axis = def.axis.normalize_or_zero();
    assert!(
        axis != Vec3::ZERO,
        "肢体 {index} 的 axis {:?} 不能是零向量（胶囊肢的轴朝向不定）",
        def.axis
    );
    Quat::from_rotation_arc(Vec3::Y, axis)
}

/// v2 自动接线（纯函数）：从肢体几何推出 [`SwingJoint`] 的锚点与
/// `rest_offset`（规则全文见 [`spawn_ragdoll_kinematic`] 文档）。
///
/// **关节点** = 子端肢体的**近端帽中心**（胶囊肢 = `中心 + rot_a·Y·
/// (half_length − radius)`；球肢 = `中心 + 朝父端单位向 × radius`）。于是：
/// - `anchor_a = 0`（子端中心）⇒ `|rest_offset|` = 关节点到子端中心的距离
///   = 轴半长 ⇒ 距离约束等效「近端帽钉在关节窝」；
/// - `anchor_b = rot_b⁻¹·(关节点 − 父端中心)`（父端本地系的关节窝）⇒
///   躯干翻滚时窝跟着走；
/// - `rest_offset = rot_b⁻¹·(关节点 − 子端中心)`（**父端本地系**）⇒ 长度
///   给距离 rest、方向给锥轴；直立人形的每段都是 `(0, 轴半长, 0)`，即
///   [`SwingJoint`](crate::physics::SwingJoint) 文档的「通常 (0, 半肢长, 0)」。
#[allow(clippy::too_many_arguments)]
fn auto_swing_joint(
    desc: &RagdollDesc,
    a: usize,
    b: usize,
    target: Entity,
    centers: &[Vec3],
    rotations: &[Quat],
    limit: f32,
) -> SwingJoint {
    let def = &desc.limbs[a];
    let joint_point = if def.half_length > def.radius {
        centers[a] + rotations[a] * Vec3::Y * (def.half_length - def.radius)
    } else {
        centers[a] + (centers[b] - centers[a]).normalize_or_zero() * def.radius
    };
    SwingJoint {
        target,
        anchor_a: Vec3::ZERO,
        anchor_b: rotations[b].inverse() * (joint_point - centers[b]),
        rest_offset: rotations[b].inverse() * (joint_point - centers[a]),
        swing_limit_radians: limit,
        stiffness: RAGDOLL_JOINT_STIFFNESS,
        damping: RAGDOLL_JOINT_DAMPING,
    }
}

/// 翻倒转轴（R50 纯函数）：**优先绕链根的肩线**——`root_rotation` 的本地
/// +X 投影到水平面，符号取「头顶倒向 `impulse_dir`」的那一侧（`ω × Y` 即
/// 头顶的瞬时移动方向）。
///
/// 为什么不直接用 `Y × 冲量方向`（垂直于冲量的水平轴）：那样侧向受击会
/// **侧倒**，躯干侧躺时靠地一侧的肩窝落到地面以下（肩偏 0.25 > 躯干半径
/// 0.18），而「接触不产生角冲量」⇒ 被钉在肩窝上的手臂转不出来、稳态埋在
/// 土里 0.15–0.37 m（实测）。绕肩线翻倒是**前扑 / 后仰**：双肩等高、都在
/// 躯干胶囊的接地高度上 ⇒ 手臂自然摊在身侧。线性踢飞方向仍由冲量决定
/// （侧向受击 = 前扑 + 侧向滑移），表现上读得通。
///
/// 回退口径（确定性）：根肢体缺 [`Transform`] / 肩线水平投影近零（躯干竖直
/// 悬挂等退化姿态）→ 回到 `Y × 冲量方向`；冲量方向零长 → 零向量（无注入）。
fn topple_axis(root_rotation: Option<Quat>, impulse_dir: Vec3) -> Vec3 {
    let kick = impulse_dir.normalize_or_zero();
    if kick == Vec3::ZERO {
        return Vec3::ZERO; // 零长冲量 = 无冲量（与 kick 的跳过语义一致）
    }
    let mut shoulder_line = root_rotation
        .map(|rotation| rotation * Vec3::X)
        .unwrap_or(Vec3::ZERO);
    shoulder_line.y = 0.0; // 肩线取水平投影（躯干已倾斜时也绕水平轴翻倒）
    if shoulder_line.length_squared() <= 1e-8 {
        return Vec3::Y.cross(kick).normalize_or_zero();
    }
    let line = shoulder_line.normalize();
    // 头顶倒向 = ω × Y；ω = s·line ⇒ s 取 (line × Y)·kick 的符号（并列取正，确定性）
    let sign = if line.cross(Vec3::Y).dot(kick) < 0.0 {
        -1.0
    } else {
        1.0
    };
    line * sign
}

/// 释放布娃娃（**物理接管期**）：把全部肢体翻转为动体（`is_static =
/// false`）并写入继承速度；`impulse = (方向, 强度)` 时再叠加
/// `方向.normalize() × 强度`（作用于**全部**肢体——受击方向整体踢飞；
/// 零长度方向 = 无冲量，确定性跳过）。
///
/// **角速度注入（R50）**：有冲量时同时写入
/// `ω = topple_axis × 强度 ×` [`RAGDOLL_SPIN_FACTOR`]——转轴规则（绕链根
/// 肩线**前扑 / 后仰**而非侧倒；侧倒会把靠地一侧的手臂钉进地面，论证与
/// 实测数字见 `topple_axis` 文档）。**链根 = `limbs[0]`**
/// （生成序 = desc 定义序，v2 预设的躯干在首位；缺 `Transform` 时走回退
/// 口径，不 panic）。v2 胶囊肢因此有真实的翻倒表现；v1 球肢各向同性、
/// 旋转无视觉语义 ⇒ 表现零变化。无冲量 ⇒ 无注入（ω 保持 0，
/// `step_physics` 跳过角积分与旋转写回）。
///
/// - 只需 `&World`：翻转走组件写锁（`get_mut`）；实体已 despawn / 缺
///   [`Collider`] / 缺 [`RigidBody`] 的肢体静默跳过（世代安全，不
///   panic）——由 [`spawn_ragdoll_kinematic`] 生成的肢体必然齐备；
/// - 释放后关节自动生效（挂载体已为动体）：v1 距离约束保链拓扑，v2
///   摆动约束另加锥面限位与肢体轴定向；
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
    // 链根朝向（limbs[0]）：决定翻倒转轴（埋臂论证见 topple_axis）
    let root_rotation = limbs
        .first()
        .and_then(|&root| world.get::<Transform>(root))
        .map(|transform| transform.rotation);
    let spin = impulse
        .map(|(dir, strength)| topple_axis(root_rotation, dir) * (strength * RAGDOLL_SPIN_FACTOR))
        .unwrap_or(Vec3::ZERO);
    for &limb in limbs {
        if let Some(mut collider) = world.get_mut::<Collider>(limb) {
            collider.is_static = false;
        }
        if let Some(mut body) = world.get_mut::<RigidBody>(limb) {
            body.velocity = velocity + kick;
            body.angular_velocity = spin;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::{detect_pair_rotated, step_physics};
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
        for &RagdollLink { a, b, .. } in &desc.links {
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
                    half_length: 0.2,
                    axis: Vec3::Y,
                    restitution: 0.0,
                }],
                links: vec![RagdollLink {
                    a: 0,
                    b: 3,
                    swing_limit: None,
                }],
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
                    half_length: 0.2,
                    axis: Vec3::Y,
                    restitution: 0.0,
                }],
                links: vec![RagdollLink {
                    a: 0,
                    b: 0,
                    swing_limit: None,
                }],
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
                    half_length: 0.0,
                    axis: Vec3::Y,
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
            .map(|&RagdollLink { a, b, .. }| {
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
            for (&RagdollLink { a, b, .. }, &rest) in desc.links.iter().zip(&rests) {
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
        for (&RagdollLink { a, b, .. }, &rest) in desc.links.iter().zip(&rests) {
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

    // =======================================================================
    // R50 v2：胶囊肢人形 + 摆动关节自动接线 + 死亡角速度注入
    // =======================================================================

    /// 四元数逐位比较口径（AGENTS 的 `to_bits` 确定性模式）。
    fn quat_bits(q: Quat) -> [u32; 4] {
        [q.x.to_bits(), q.y.to_bits(), q.z.to_bits(), q.w.to_bits()]
    }

    fn limb_rotation(world: &World, entity: Entity) -> Quat {
        world
            .get::<Transform>(entity)
            .expect("肢体应有 Transform")
            .rotation
    }

    fn limb_angular(world: &World, entity: Entity) -> Vec3 {
        world
            .get::<RigidBody>(entity)
            .expect("肢体应有 RigidBody")
            .angular_velocity
    }

    /// 世界里全部摆动关节（挂载实体 → 关节值，收集序 = 组件存储序）。
    fn all_swing_joints(world: &World) -> Vec<(Entity, SwingJoint)> {
        let mut joints = Vec::new();
        world.query::<(Entity, &SwingJoint)>(|(entity, joint)| {
            joints.push((entity, *joint));
        });
        joints
    }

    /// 肢体在世界里的最低点（胶囊 = 中心 − |轴 y 分量|·轴半长 − 半径；
    /// 球 = 中心 − 半径）——「脚底贴地 / 无穿地」的口径。
    fn lowest_point(world: &World, entity: Entity, def: &RagdollLimbDef) -> f32 {
        let transform = world.get::<Transform>(entity).expect("肢体应有 Transform");
        if def.half_length > def.radius {
            let axis_y = (transform.rotation * Vec3::Y).y.abs();
            transform.position.y - axis_y * (def.half_length - def.radius) - def.radius
        } else {
            transform.position.y - def.radius
        }
    }

    /// v2 十肢拓扑与**自动接线**逐位对照：形状按 `half_length` vs `radius`
    /// 分流（9 胶囊 + 1 球）、胶囊肢朝向 = `axis` 的最小弧（球肢恒等）、
    /// 组位 / mask / 静态初值照旧、刚体带角阻尼、`anchor_a = 0`、
    /// `rest_offset = (0, 轴半长, 0)`（父端本地系，直立人形的解析值）、
    /// `anchor_b` = 关节窝（子端近端帽中心 − 父端中心）、限位 / 刚度 / 阻尼
    /// 来自常量；头（`None` 链接）仍走 v1 距离关节。
    #[test]
    fn spawn_v2_topology_and_wiring_match_desc_bit_exact() {
        let mut world = make_world();
        let pos = Vec3::new(2.0, 0.0, -1.0);
        let desc = RagdollDesc::humanoid_v2();
        let limbs = spawn_ragdoll_kinematic(&mut world, pos, &desc);

        assert_eq!(
            limbs.len(),
            10,
            "v2 人形应为十肢（躯干 / 头 / 上臂×2 / 前臂×2 / 大腿×2 / 小腿×2）"
        );
        assert_eq!(desc.limbs.len(), limbs.len(), "实体数应等于肢体定义数");

        for (index, (&entity, def)) in limbs.iter().zip(&desc.limbs).enumerate() {
            let collider = limb_collider(&world, entity);
            let is_capsule = def.half_length > def.radius;
            assert_eq!(
                collider.shape,
                if is_capsule {
                    ColliderShape::capsule(def.radius, def.half_length)
                } else {
                    ColliderShape::Sphere { radius: def.radius }
                },
                "肢体 {index} 的形状应由 half_length 与 radius 的关系决定"
            );
            assert_eq!(
                limb_position(&world, entity),
                pos + def.offset,
                "肢体 {index} 位置应为 pos + offset（bit 级）"
            );
            let rotation = limb_rotation(&world, entity);
            if is_capsule {
                let axis = def.axis.normalize_or_zero();
                assert!(
                    (rotation * Vec3::Y - axis).length() < 1e-6,
                    "胶囊肢 {index} 的本地 +Y 应对准 axis {axis:?}，实际 {:?}",
                    rotation * Vec3::Y
                );
            } else {
                assert_eq!(
                    rotation,
                    Quat::IDENTITY,
                    "球肢 {index} 旋转应为恒等（各向同性、旋转无语义）"
                );
            }
            assert!(
                collider.is_static,
                "肢体 {index} 初值应为静态（动画接管期）"
            );
            assert_eq!(collider.groups, RAGDOLL_GROUP, "肢体 {index} 组位照旧");
            assert_eq!(
                collider.mask, !RAGDOLL_GROUP,
                "肢体 {index} 的 mask 应排除 RAGDOLL_GROUP（自碰撞免抖）"
            );
            let body = world.get::<RigidBody>(entity).expect("肢体应有 RigidBody");
            assert_eq!(
                body.angular_damping, RAGDOLL_LIMB_ANGULAR_DAMPING,
                "肢体 {index} 应带角阻尼（翻滚会停下，不是永远打转）"
            );
            assert_eq!(
                body.angular_velocity,
                Vec3::ZERO,
                "肢体 {index} 生成时角速度为零（释放时才注入）"
            );
        }

        // 关节：8 条摆动（挂在子端 a、target = 父端 b）+ 1 条 v1 距离（头）
        let swings = all_swing_joints(&world);
        let distances = all_joints(&world);
        let swing_links: Vec<&RagdollLink> = desc
            .links
            .iter()
            .filter(|link| link.swing_limit.is_some())
            .collect();
        assert_eq!(
            swings.len(),
            swing_links.len(),
            "摆动关节数应等于 Some 链接数（{}）",
            swing_links.len()
        );
        assert_eq!(
            distances.len(),
            desc.links.len() - swing_links.len(),
            "None 链接应走 v1 距离关节（头 → 躯干）"
        );
        assert_eq!(
            distances[0].0, limbs[0],
            "头的距离关节应挂在躯干（b 端）上——v1 挂载方向照旧"
        );
        assert_eq!(
            distances[0].1.target, limbs[1],
            "头的距离关节 target 应指向头（a 端）"
        );

        for link in &swing_links {
            let (a, b) = (link.a, link.b);
            let (_, joint) = swings
                .iter()
                .find(|&(entity, _)| *entity == limbs[a])
                .unwrap_or_else(|| panic!("肢体 {a} 上应挂摆动关节"));
            assert_eq!(joint.target, limbs[b], "摆动关节 target 应指向父端 {b}");
            assert_eq!(
                joint.anchor_a,
                Vec3::ZERO,
                "子端锚 = 肢体中心（rest = 近端帽距离 ⇒ 等效把近端帽钉在关节窝）"
            );
            assert_eq!(
                joint.swing_limit_radians,
                link.swing_limit.expect("该链接为 Some"),
                "限位应来自 desc"
            );
            assert_eq!(joint.stiffness, RAGDOLL_JOINT_STIFFNESS, "应为硬约束");
            assert_eq!(joint.damping, RAGDOLL_JOINT_DAMPING, "应带轴向阻尼");

            // rest_offset（父端本地系）：直立人形每段都是 (0, 轴半长, 0)
            let def_a = &desc.limbs[a];
            let half_axis = def_a.half_length - def_a.radius;
            let want_rest = Vec3::new(0.0, half_axis, 0.0);
            assert!(
                (joint.rest_offset - want_rest).length() < 1e-6,
                "链接 ({a}, {b}) 的 rest_offset 应为 (0, 轴半长, 0) = {want_rest:?}，实际 {:?}",
                joint.rest_offset
            );
            // anchor_b = 关节窝 = 子端近端帽中心 − 父端中心（父端旋转恒等）
            let joint_point = pos + def_a.offset + Vec3::Y * half_axis;
            let want_anchor_b = joint_point - (pos + desc.limbs[b].offset);
            assert!(
                (joint.anchor_b - want_anchor_b).length() < 1e-6,
                "链接 ({a}, {b}) 的关节窝应为 {want_anchor_b:?}，实际 {:?}",
                joint.anchor_b
            );
        }
    }

    /// v2 骨架**相连性**：每条链接的两肢真实重叠（刻意微重叠 ⇒ 视觉连续、
    /// 不是恰好相切的脆弱边界）、脚底贴地（最低点 = 0）、头顶 1.635 m
    /// （骨架比例口径锁定）。
    #[test]
    fn humanoid_v2_skeleton_is_connected() {
        let mut world = make_world();
        let desc = RagdollDesc::humanoid_v2();
        let limbs = spawn_ragdoll_kinematic(&mut world, Vec3::ZERO, &desc);

        for link in &desc.links {
            let (a, b) = (link.a, link.b);
            let contact = detect_pair_rotated(
                limb_position(&world, limbs[a]),
                limb_rotation(&world, limbs[a]),
                &limb_collider(&world, limbs[a]).shape,
                limb_position(&world, limbs[b]),
                limb_rotation(&world, limbs[b]),
                &limb_collider(&world, limbs[b]).shape,
            )
            .unwrap_or_else(|| panic!("链接 ({a}, {b}) 的两肢应重叠（骨架相连、无缝隙）"));
            assert!(
                contact.penetration > 1e-3,
                "链接 ({a}, {b}) 的重叠量 {} 应 > 1e-3（刻意微重叠，非脆弱相切）",
                contact.penetration
            );
        }

        let lowest = limbs
            .iter()
            .zip(&desc.limbs)
            .map(|(&entity, def)| lowest_point(&world, entity, def))
            .fold(f32::MAX, f32::min);
        assert!(lowest.abs() < 1e-6, "v2 人形应脚底贴地（最低点 {lowest}）");
        let highest = limbs
            .iter()
            .zip(&desc.limbs)
            .map(|(&entity, def)| {
                limb_position(&world, entity).y
                    + if def.half_length > def.radius {
                        def.half_length
                    } else {
                        def.radius
                    }
            })
            .fold(f32::MIN, f32::max);
        assert!(
            (highest - (1.50 + 0.135)).abs() < 1e-6,
            "头顶应为 1.635 m（骨架比例），实际 {highest}"
        );
    }

    /// 非竖直 `axis` 的接线（T 姿势：手臂平举）：子端朝向 = `axis` 的最小弧、
    /// 关节点落在平举方向上、**锥轴 = 肢体自身方向**（rest_offset 在父端
    /// 本地系里是 (±轴半长, 0, 0) 而非 (0, 轴半长, 0)）——证明接线不是
    /// 「只对直立姿势成立」的硬编码。
    ///
    /// `axis` 是**近端方向**（指向父端）：右臂平举向 +X 时肩（近端帽）在
    /// −X 侧 ⇒ `axis = NEG_X`（若写反，近端帽会落在手的位置上，关节窝与
    /// 锥轴一起错到肢体外侧——这条测试就是那道锁）。
    #[test]
    fn capsule_limb_wiring_follows_declared_axis() {
        let mut world = make_world();
        let desc = RagdollDesc {
            limbs: vec![
                // 0 躯干（竖直胶囊）
                RagdollLimbDef {
                    offset: Vec3::new(0.0, 1.0, 0.0),
                    radius: 0.18,
                    half_length: 0.37,
                    axis: Vec3::Y,
                    restitution: 0.1,
                },
                // 1 平举的右臂：axis = −X（近端 / 肩在内侧，手在 +X 外侧）
                RagdollLimbDef {
                    offset: Vec3::new(0.43, 1.195, 0.0),
                    radius: 0.08,
                    half_length: 0.19,
                    axis: Vec3::NEG_X,
                    restitution: 0.2,
                },
            ],
            links: vec![RagdollLink {
                a: 1,
                b: 0,
                swing_limit: Some(1.2),
            }],
        };
        let limbs = spawn_ragdoll_kinematic(&mut world, Vec3::ZERO, &desc);

        let rotation = limb_rotation(&world, limbs[1]);
        assert!(
            (rotation * Vec3::Y - Vec3::NEG_X).length() < 1e-6,
            "平举手臂的本地 +Y 应对准 axis −X，实际 {:?}",
            rotation * Vec3::Y
        );
        let (_, joint) = all_swing_joints(&world)
            .into_iter()
            .find(|&(entity, _)| entity == limbs[1])
            .expect("手臂上应挂摆动关节");
        let half_axis = 0.19 - 0.08;
        assert!(
            (joint.rest_offset - Vec3::new(-half_axis, 0.0, 0.0)).length() < 1e-6,
            "锥轴（rest_offset）应是肢体自身方向 (−轴半长, 0, 0)，实际 {:?}",
            joint.rest_offset
        );
        // 关节窝 = 近端帽中心 (0.32, 1.195, 0) − 躯干中心 (0, 1, 0)
        assert!(
            (joint.anchor_b - Vec3::new(0.32, 0.195, 0.0)).length() < 1e-6,
            "关节窝应为「近端帽中心 − 躯干中心」= (0.32, 0.195, 0)，实际 {:?}",
            joint.anchor_b
        );
    }

    /// 翻倒转轴的纯函数规则：①根朝向恒等（肩线 = 世界 +X）时转轴恒为
    /// ±X（**前扑 / 后仰**，绝不侧倒），符号取「头顶倒向冲量」的一侧：
    /// 冲量 +Z ⇒ +X（头顶倒向 X × Y = +Z）、冲量 −Z ⇒ −X；②冲量与肩线
    /// 平行（纯侧向 +X）⇒ 点积为 0、并列取正 ⇒ +X（确定性）；③根朝向
    /// 缺席 ⇒ 回退 `Y × 冲量`；④肩线竖直（水平投影退化）⇒ 同样回退。
    #[test]
    fn topple_axis_prefers_shoulder_line_with_deterministic_fallbacks() {
        let upright = Some(Quat::IDENTITY);
        assert_eq!(
            topple_axis(upright, Vec3::Z),
            Vec3::X,
            "冲量 +Z ⇒ 绕 +X 前扑（头顶倒向 X × Y = +Z = 冲量方向）"
        );
        assert_eq!(
            topple_axis(upright, Vec3::NEG_Z),
            Vec3::NEG_X,
            "冲量 −Z ⇒ 绕 −X 后仰（头顶倒向 −Z）"
        );
        assert_eq!(
            topple_axis(upright, Vec3::X),
            Vec3::X,
            "冲量与肩线平行（纯侧向）⇒ 点积 0、并列取正（确定性）"
        );
        // 根已绕 Y 转 90°：肩线（本地 +X）指向世界 −Z（水平、非退化）
        // ⇒ 转轴 = −Z（符号由「头顶倒向冲量 +X」定：(0,0,−1) × Y = +X ✓）
        let turned = Some(Quat::from_rotation_y(std::f32::consts::FRAC_PI_2));
        let axis = topple_axis(turned, Vec3::X);
        assert!(
            (axis - Vec3::new(0.0, 0.0, -1.0)).length() < 1e-6,
            "转身后转轴应为肩线 −Z（头顶倒向 +X），实际 {axis:?}"
        );
        assert_eq!(
            topple_axis(turned, Vec3::NEG_X),
            Vec3::Z,
            "冲量反向 ⇒ 转轴反向（头顶倒向 −X）"
        );
        // 回退：无根朝向 ⇒ Y × 冲量
        let fallback = topple_axis(None, Vec3::X);
        assert!(
            (fallback - Vec3::Y.cross(Vec3::X).normalize()).length() < 1e-6,
            "根朝向缺席应回退 Y × 冲量方向，实际 {fallback:?}"
        );
        // 回退：肩线竖直（根绕 Z 转 −90° ⇒ 本地 +X 指向世界 −Y，水平投影退化）
        let shoulder_vertical = Quat::from_rotation_z(-std::f32::consts::FRAC_PI_2);
        let vertical_axis = shoulder_vertical * Vec3::X;
        let degenerate = topple_axis(Some(shoulder_vertical), Vec3::X);
        assert!(
            vertical_axis.y.abs() > 0.9,
            "本用例的根朝向应让肩线接近竖直（构造前提），实际 {vertical_axis:?}"
        );
        assert!(
            (degenerate - Vec3::Y.cross(Vec3::X).normalize()).length() < 1e-6,
            "肩线水平投影退化 ⇒ 回退 Y × 冲量方向，实际 {degenerate:?}"
        );
        // 零长冲量 ⇒ 零转轴（无注入）
        assert_eq!(
            topple_axis(upright, Vec3::ZERO),
            Vec3::ZERO,
            "零冲量 ⇒ 零转轴"
        );
    }

    /// 释放的**角速度注入**：`ω = topple_axis(链根朝向, 冲量方向) × 强度 ×
    /// RAGDOLL_SPIN_FACTOR`（同式复算 bit 级，全部肢体一致）；无冲量 ⇒ ω
    /// 归零；且注入方向使躯干**朝 ω × Y 翻倒**（v2 预设肩线 = 世界 +X ⇒
    /// 冲量 +X 时前扑向 +Z、冲量 +Z 时同样倒向 +Z = 朝受击方向）。
    #[test]
    fn release_injects_impulse_related_angular_velocity() {
        let mut world = make_world();
        spawn_floor(&mut world);
        let limbs = spawn_ragdoll_kinematic(&mut world, Vec3::ZERO, &RagdollDesc::humanoid_v2());
        let root_rotation = limb_rotation(&world, limbs[0]);

        let (dir, strength) = (Vec3::X, 3.0f32);
        release_ragdoll(&world, &limbs, Vec3::ZERO, Some((dir, strength)));
        let want = topple_axis(Some(root_rotation), dir) * (strength * RAGDOLL_SPIN_FACTOR);
        assert!(
            want.length() > 1.0,
            "注入量应显著（{want:?}），否则翻倒断言没有意义"
        );
        assert!(
            (want - Vec3::new(strength * RAGDOLL_SPIN_FACTOR, 0.0, 0.0)).length() < 1e-6,
            "v2 预设肩线 = 世界 +X ⇒ 转轴应为 ±X（前扑 / 后仰，不侧倒），实际 {want:?}"
        );
        for (index, &entity) in limbs.iter().enumerate() {
            assert_eq!(
                limb_angular(&world, entity),
                want,
                "肢体 {index} 的角速度应为冲量相关注入值（同式 bit 级）"
            );
        }

        // 无冲量 ⇒ 无注入（ω 归零，step_physics 因此跳过角积分与旋转写回）
        release_ragdoll(&world, &limbs, Vec3::ZERO, None);
        for (index, &entity) in limbs.iter().enumerate() {
            assert_eq!(
                limb_angular(&world, entity),
                Vec3::ZERO,
                "无冲量时肢体 {index} 的角速度应归零"
            );
        }

        // 翻倒方向：ω = +X ⇒ 头顶（+Y）倒向 ω × Y = +Z
        release_ragdoll(&world, &limbs, Vec3::ZERO, Some((dir, strength)));
        for _ in 0..30 {
            step(&world, DT);
        }
        let torso_up = limb_rotation(&world, limbs[0]) * Vec3::Y;
        assert!(
            torso_up.z > 0.3,
            "ω = +X ⇒ 躯干应前扑向 +Z（+Y 的 z 分量 {torso_up:?}）"
        );
        assert!(
            torso_up.y < 0.95,
            "躯干应真实倾倒（+Y 不再竖直），实际 {torso_up:?}"
        );
    }

    /// v2 释放后 240 步（侧向冲量踢倒 + 角速度翻滚）：
    /// - **关节不散架**：每条摆动链接的锚距全程保持 `|rest_offset|` ± 2e-2；
    /// - **关节不反折**：摆角全程 ≤ 限位 + 5e-3（锥面拉回生效，且确实甩到
    ///   过限位附近 ⇒ 非空转）；
    /// - **不穿地 / 不飞出**：每肢最低点 ≥ −2e-2、质心塌落（< 0.5，直立时
    ///   ≈ 0.82）、水平位移有界；
    /// - **不爆炸**：全程速度 < 25 m/s 且 3 s 后显著衰减；
    /// - **确实翻滚过**：躯干倾角峰值 > 1.0 rad（ω 注入生效，不是直挺挺倒下）。
    #[test]
    fn released_v2_ragdoll_tumbles_collapses_and_respects_limits() {
        let mut world = make_world();
        spawn_floor(&mut world);
        let desc = RagdollDesc::humanoid_v2();
        let limbs = spawn_ragdoll_kinematic(&mut world, Vec3::ZERO, &desc);

        // 关节参数快照（求解期不变，避免每帧查世界）
        let wired: Vec<(usize, usize, SwingJoint)> = desc
            .links
            .iter()
            .filter_map(|link| {
                link.swing_limit?;
                let joint = *world
                    .get::<SwingJoint>(limbs[link.a])
                    .expect("Some 链接应已接线摆动关节");
                Some((link.a, link.b, joint))
            })
            .collect();
        assert_eq!(wired.len(), 8, "v2 人形应有 8 条摆动链接");

        release_ragdoll(
            &world,
            &limbs,
            Vec3::new(0.3, 0.0, 0.0),
            Some((Vec3::X, 3.0)),
        );

        // 容差依据（实测量级）：**层级投影**（修正全给子端、父端不动）+
        // 根先叶后的求解序 ⇒ 每轮末尾全部关节约束精确满足，240 步实测
        // 最大锚距偏差 1.2e-7、最大摆角超限 7.2e-7 rad（都是 f32 舍入量级）
        // ⇒ 容差取 1e-4 / 1e-5（比实测松三个数量级，仍远紧于 v1 的 3e-2）。
        const ANCHOR_TOLERANCE: f32 = 1e-4;
        const FINAL_TOLERANCE: f32 = 1e-5;
        const ANGLE_TOLERANCE: f32 = 1e-4;
        // 穿地容差 5e-2：与 examples/ragdoll_snapshot.rs 的 GROUND_TOLERANCE
        // 同值（2 轮位置松弛的残余穿透 + 髋关节把大腿往地面拽的量级）
        const GROUND_TOLERANCE: f32 = 5e-2;
        const SPEED_CAP: f32 = 25.0;
        let mut max_speed = 0.0f32;
        let mut max_deviation = 0.0f32;
        let mut max_excess = 0.0f32;
        let mut max_torso_tilt = 0.0f32;
        let mut clamped_links = 0usize;
        for _ in 0..240 {
            step(&world, DT);
            for &entity in &limbs {
                let speed = limb_velocity(&world, entity).length();
                assert!(speed < SPEED_CAP, "肢体速度 {speed} 应有界（无爆炸）");
                max_speed = max_speed.max(speed);
            }
            for &(a, b, ref joint) in &wired {
                let transform_a = world
                    .get::<Transform>(limbs[a])
                    .expect("肢体应有 Transform");
                let transform_b = world
                    .get::<Transform>(limbs[b])
                    .expect("肢体应有 Transform");
                let anchor_a = transform_a.position + transform_a.rotation * joint.anchor_a;
                let anchor_b = transform_b.position + transform_b.rotation * joint.anchor_b;
                let delta = anchor_b - anchor_a;
                let rest = joint.rest_offset.length();
                let deviation = (delta.length() - rest).abs();
                assert!(
                    deviation <= ANCHOR_TOLERANCE,
                    "链接 ({a}, {b}) 锚距 {} 应保持 rest {rest} ± {ANCHOR_TOLERANCE}（不散架）",
                    delta.length()
                );
                max_deviation = max_deviation.max(deviation);
                // 摆角 ≤ 限位（锥轴随父端旋转 ⇒ 用父端当前朝向）
                let axis = transform_b.rotation * joint.rest_offset.normalize_or_zero();
                let angle = (delta.normalize().dot(axis)).clamp(-1.0, 1.0).acos();
                assert!(
                    angle <= joint.swing_limit_radians + ANGLE_TOLERANCE,
                    "关节 ({a}, {b}) 摆角 {angle} rad 不得超限位 {} rad（不反折）",
                    joint.swing_limit_radians
                );
                max_excess = max_excess.max(angle - joint.swing_limit_radians);
                if angle > joint.swing_limit_radians - 0.1 {
                    clamped_links += 1;
                }
            }
            for (index, (&entity, def)) in limbs.iter().zip(&desc.limbs).enumerate() {
                let lowest = lowest_point(&world, entity, def);
                assert!(
                    lowest >= -GROUND_TOLERANCE,
                    "肢体 {index} 不应穿地（最低点 {lowest}）"
                );
            }
            let tilt = (limb_rotation(&world, limbs[0]) * Vec3::Y)
                .dot(Vec3::Y)
                .clamp(-1.0, 1.0)
                .acos();
            max_torso_tilt = max_torso_tilt.max(tilt);
        }

        assert!(
            clamped_links > 0,
            "限位应真实被触发过（否则限位断言是空转）"
        );
        assert!(
            max_excess <= ANGLE_TOLERANCE,
            "全程最大摆角超限 {max_excess} rad 应在 {ANGLE_TOLERANCE} 内（锥面拉回全程生效）"
        );
        assert!(
            max_torso_tilt > 1.0,
            "躯干应真实翻倒过（峰值倾角 {max_torso_tilt} rad > 1.0）"
        );
        assert!(
            max_deviation <= ANCHOR_TOLERANCE,
            "全程最大锚距偏差 {max_deviation} 应有界（约束全程生效）"
        );

        // 终态：锚距收敛回 rest（静止后每帧 2 轮松弛足够）
        for &(a, b, ref joint) in &wired {
            let transform_a = world
                .get::<Transform>(limbs[a])
                .expect("肢体应有 Transform");
            let transform_b = world
                .get::<Transform>(limbs[b])
                .expect("肢体应有 Transform");
            let anchor_a = transform_a.position + transform_a.rotation * joint.anchor_a;
            let anchor_b = transform_b.position + transform_b.rotation * joint.anchor_b;
            let rest = joint.rest_offset.length();
            assert!(
                (anchor_b.distance(anchor_a) - rest).abs() <= FINAL_TOLERANCE,
                "终态链接 ({a}, {b}) 锚距 {} 应收敛回 rest {rest} ± {FINAL_TOLERANCE}",
                anchor_b.distance(anchor_a)
            );
        }

        // 终态：塌倒（质心显著低于直立）、肢体都在地面附近、速度收敛
        let mut com = Vec3::ZERO;
        for &entity in &limbs {
            let position = limb_position(&world, entity);
            com += position;
            assert!(
                position.y < 1.2 && position.x.abs() < 10.0 && position.z.abs() < 10.0,
                "肢体应塌在地面附近且未飞出，实际 {position:?}"
            );
        }
        com /= limbs.len() as f32;
        assert!(
            com.y < 0.5,
            "终态质心 y = {} 应显著低于直立姿势（≈0.82）⇒ 摊倒",
            com.y
        );
        let total_speed: f32 = limbs
            .iter()
            .map(|&entity| limb_velocity(&world, entity).length())
            .sum();
        assert!(
            total_speed < 1.5,
            "4 秒（阻尼 + 摩擦）后应趋于静止：总速度 {total_speed}（实测 0.8 量级）"
        );
        assert!(
            max_speed > 1.0,
            "冲量应真实驱动过肢体（最大速度 {max_speed}，测试非空转）"
        );
    }

    /// v2 全流程确定性：同 desc 两次构建（生成 → 动画期 30 步 → 释放（含
    /// 角速度注入）→ 物理 210 步）的全部肢体**位置与旋转**逐采样 bit 级
    /// 一致；轨迹非平凡。
    #[test]
    fn v2_ragdoll_pipeline_is_deterministic_across_runs() {
        fn build() -> (World, Vec<Entity>) {
            let mut world = make_world();
            spawn_floor(&mut world);
            let limbs = spawn_ragdoll_kinematic(
                &mut world,
                Vec3::new(1.0, 0.0, -2.0),
                &RagdollDesc::humanoid_v2(),
            );
            (world, limbs)
        }

        let (world_a, limbs_a) = build();
        let (world_b, limbs_b) = build();
        let mut trace_a: Vec<(Vec3, [u32; 4])> = Vec::new();
        let mut trace_b: Vec<(Vec3, [u32; 4])> = Vec::new();
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
            for (&a, &b) in limbs_a.iter().zip(&limbs_b) {
                trace_a.push((
                    limb_position(&world_a, a),
                    quat_bits(limb_rotation(&world_a, a)),
                ));
                trace_b.push((
                    limb_position(&world_b, b),
                    quat_bits(limb_rotation(&world_b, b)),
                ));
            }
        }
        assert_eq!(
            trace_a, trace_b,
            "同 desc 两次全流程应逐采样 bit 级一致（位置 + 旋转）"
        );
        assert!(
            trace_a[0].0 != trace_a.last().expect("轨迹非空").0,
            "全流程应有运动（测试非空转）"
        );
        assert_ne!(
            trace_a[0].1,
            trace_a.last().expect("轨迹非空").1,
            "全流程应有翻滚（旋转确实被积分 / 被关节修正）"
        );
    }

    /// 断言守护：摆动限位越界（> π）在生成时 panic（中文断言消息）。
    #[test]
    #[should_panic(expected = "限位")]
    fn spawn_rejects_out_of_range_swing_limit() {
        let mut world = make_world();
        let _ = spawn_ragdoll_kinematic(
            &mut world,
            Vec3::ZERO,
            &RagdollDesc {
                limbs: vec![
                    RagdollLimbDef {
                        offset: Vec3::ZERO,
                        radius: 0.2,
                        half_length: 0.2,
                        axis: Vec3::Y,
                        restitution: 0.0,
                    },
                    RagdollLimbDef {
                        offset: Vec3::new(0.0, 0.5, 0.0),
                        radius: 0.2,
                        half_length: 0.2,
                        axis: Vec3::Y,
                        restitution: 0.0,
                    },
                ],
                links: vec![RagdollLink {
                    a: 0,
                    b: 1,
                    swing_limit: Some(4.0),
                }],
            },
        );
    }

    /// 断言守护：胶囊肢的 `axis` 为零向量时在生成时 panic（轴朝向不定）。
    #[test]
    #[should_panic(expected = "axis")]
    fn spawn_rejects_zero_axis_for_capsule_limb() {
        let mut world = make_world();
        let _ = spawn_ragdoll_kinematic(
            &mut world,
            Vec3::ZERO,
            &RagdollDesc {
                limbs: vec![RagdollLimbDef {
                    offset: Vec3::ZERO,
                    radius: 0.1,
                    half_length: 0.4,
                    axis: Vec3::ZERO,
                    restitution: 0.0,
                }],
                links: Vec::new(),
            },
        );
    }
}
