//! 纯游戏逻辑核心：确定性潜入状态机。依赖 glam + [`iris_core`] 的**纯
//! 函数**（[`move_and_slide_statics`] / [`ColliderShape`] /
//! [`segment_hits_shape`] / [`NavGrid`]）——刻意的 dogfooding：移动就是
//! 引擎运动学角色控制器（R31）、守卫寻路就是引擎 NavGrid（R35）、视线
//! 遮挡就是引擎线段-形状射线纯函数。「Iris Infiltration」的全部规则
//! （移动 / 潜行 / 守卫视锥感知与警觉 / 巡逻与追逐 / 数据核心 / 压力板
//! 门 / 击晕 / 出口胜利 / 三次发现告负）都在这个模块里以纯函数推进：
//! 输入 [`InfiltrationInput`]，输出状态变化与 [`InfiltrationEvent`] 列表
//! ——不读时钟、不做随机、不触碰 ECS 世界。因此：
//! - 全部规则可毫秒级单测（本模块底部 11 项）；
//! - 插件层（`crate::plugin`）只负责把键盘输入翻译成本模块类型、消费
//!   物理接触事件（R34 压力板）与生成表现（ragdoll / 音频 / HUD）；
//! - headless 测试、自动玩家与离屏快照共享同一条确定性推进路径。
//!
//! # 与引擎能力的关系（载荷说明）
//!
//! - **角色控制器**：每帧把水平意图交给 [`move_and_slide_statics`]（静态
//!   体 = 内外墙 + 关闭的 pressure 门，由 [`InfiltrationState`] 构造时
//!   一次性转成 `Vec<(Vec3, ColliderShape)>`；重力缩放 0 + 速度无竖直
//!   分量 = 俯视平面语义，`grounded` 恒 false 是预期而非缺陷）；
//! - **NavGrid（R35）**：[`InfiltrationState::new`] 启动时 build 一次
//!   （压力门按**关闭**态计），守卫巡逻 / 追逐全部走 [`NavGrid::find_path`]
//!   航点；门被压力板打开时按引擎文档推荐的「便宜重建」姿势整图重建
//!   （[`NavGrid::build`] 毫秒级纯函数，[InfiltrationState::press_plate]）；
//! - **视线遮挡**：守卫眼到玩家胸口的线段对静态体逐个
//!   [`segment_hits_shape`]（内墙 + 关闭的门都挡视线；门开即透明）。
//!
//! # 设施布局（24×18 m，坐标即文档）
//!
//! 可玩区 x∈(-12, 12)、z∈(-9, 9)；横向内墙 z=∓2 把设施分成中央走廊
//! （z∈(-1.8, 1.8)）与南北四室；纵向内墙 x=0（z<-2 与 z>2 段）再把南北
//! 各分成东西两室。内墙上的门洞恒 2 m 宽（导航可靠域：`agent_radius`
//! 0.45 膨胀后仍余 ~1.1 m 净宽）：
//!
//! ```text
//!  z=-9 ┌─────────┬──────────┐
//!       │ NW 核心 A │ NE 核心 C │   A(-10,-7) C(10,-7)
//!  z=-2 ├───门洞────╥──压力门───┤   ╥ = pressure 门（压力板开）
//!       │  中央走廊（出生在西端）  │   压力板 (2, 0.5)
//!  z= 2 ├───门洞────┼───门洞────┤   门洞东口装弹簧门板（R36 纯展示）
//!       │ SW 出口   │ SE 核心 B │   B(10,7) 出口(-10,7)
//!  z= 9 └─────────┴──────────┘
//! ```
//!
//! 守卫 0 巡逻 NW 室矩形环；守卫 1 巡逻 SE 室矩形环；走廊刻意无守卫
//! （3.6 m 直通区无掩体，迎面视锥无法走位避开——设计上交给时机）。
//!
//! # 帧内顺序（固定，是确定性的一部分）
//!
//! 终局冻结 → 玩家移动（[`move_and_slide_statics`]）→ 数据核心拾取 →
//! 击晕判定（interact 背后 2.2 m）→ 守卫（感知/警觉 → 巡逻/追逐移动）→
//! 出口结算。守卫的「感知在移动前」保证同帧内视线用移动前的位姿——
//! 顺序本身是可单测的语义。
//!
//! # 击晕与诚实边界（v1）
//!
//! interact 在守卫**背后**（守卫朝向与「守卫→玩家」夹角超过背面阈值）
//! 2.2 m 内 → 守卫 [`GuardState::Stunned`]（警觉冻结、不再感知、不再
//! 移动），表现层生成 ragdoll 瘫倒。**v1 击晕后不醒来**——无苏醒计时，
//! 潜入门看不到「恢复巡逻」的守卫（诚实限制，写进插件文档）。
//! 追逐中的守卫**不**因贴身再触发发现（v1 追逐是压力演出而非处决）。
//!
//! # 自动玩家（[`AutoPilot`]）
//!
//! 恒定潜行（视锥发现半径减半到 4 m）+ 预设路线点逐段直走 +
//! 「巡逻守卫 5 m 内就停」的避让规则：路线点全部避开守卫巡逻环的
//! 中线，等待时守卫必然走远（巡逻不停步，无死锁；追逐态不避让——
//! 追逐意味着已被发现，跑路要紧）。被发现次数预算 ≤ 1（设计余量：
//! 走位与守卫环最近仅 2.5 m 且恒潜行，正常推进 0 次发现；1 次为
//! 离散化抖动的容错）。

use glam::{Vec2, Vec3};

use iris_core::behavior::{
    ActionId, BehaviorAgent, BehaviorContext, BehaviorTree, Blackboard, BtStatus, NodeSpec,
};
use iris_core::character::{CharacterParams, move_and_slide_statics};
use iris_core::nav::{NavGrid, NavGridParams};
use iris_core::perception::{can_see, flat_statics};
use iris_core::physics::{ColliderShape, StaticObstacle};

// ---------------------------------------------------------------------------
// 可调常量（pub：单测 / 插件层 / 文档共用同一事实来源）
// ---------------------------------------------------------------------------

/// 玩家正常移动速度（米/秒）。
pub const MOVE_SPEED: f32 = 4.2;
/// 玩家潜行速度（米/秒）：按住 Shift 慢走，守卫视锥发现半径减半。
pub const SNEAK_SPEED: f32 = 2.0;
/// 守卫巡逻速度（米/秒）。
pub const PATROL_SPEED: f32 = 1.8;
/// 守卫追逐速度（米/秒）：快于潜行玩家——被发现后不能磨蹭。
pub const CHASE_SPEED: f32 = 3.2;
/// 守卫视锥发现半径（米，正常移动的玩家）。
pub const VIEW_DISTANCE: f32 = 8.0;
/// 守卫视锥发现半径（米，潜行玩家 = [`VIEW_DISTANCE`] 的一半）。
pub const SNEAK_VIEW_DISTANCE: f32 = VIEW_DISTANCE * 0.5;
/// 守卫视锥半角余弦（半角 55°，cos = 0.5735764——const 上下文不能调用
/// `cos`，取 IEEE 精确值的字面量）：守卫朝向与「守卫→玩家」方向点积
/// 低于此值即不在视锥内。
pub const FOV_COS: f32 = 0.573_576_4;
/// 警觉计量上升速率（1/秒）：视锥内持续可见 1/此值 秒即被发现。
pub const ALERT_RISE: f32 = 1.2;
/// 警觉计量衰减速率（1/秒）：离开视锥后警觉缓慢回落。
pub const ALERT_DECAY: f32 = 0.6;
/// 击晕半径（米）：interact 时玩家到守卫的最大距离。
pub const STUN_RADIUS: f32 = 2.2;
/// 击晕背面阈值：守卫朝向与「守卫→玩家」单位方向点积低于此值才算
/// 背后（> 0.2 表示玩家在守卫前方 ~78° 锥角之外）。
pub const STUN_BEHIND_COS: f32 = 0.2;
/// 守卫追逐的记忆时长（秒）：失去视线后仍朝最后已知位置追多久。
pub const CHASE_MEMORY: f32 = 5.0;
/// 守卫追逐重寻路间隔（秒）：find_path 不是免费的，按节拍重算。
pub const REPATH_INTERVAL: f32 = 0.5;
/// 航点到达半径（米）：守卫与自动玩家共用的「到点」判定。
pub const WAYPOINT_REACH: f32 = 0.35;
/// 数据核心拾取半径（米）：玩家胶囊中心到核心的距离判定。
pub const CORE_RADIUS: f32 = 1.2;
/// 被发现次数上限：达到即告负。
pub const MAX_DETECTIONS: u32 = 3;

// ---- R65 行为树能力参数（演示能力默认关；见 [`GuardCapabilities`]）----

/// 逃跑触发：血量 ≤ 此值即视为「低血量」（v1 无伤害系统，测试直接设
/// [`Guard::health`]；正式玩法里留给后续轮的伤害/压制机制）。
pub const FLEE_HEALTH: f32 = 0.35;
/// 逃跑触发：被逼近 = 本帧看得见玩家且水平距离 ≤ 此值（米）。
pub const FLEE_RADIUS: f32 = 4.5;
/// 逃跑速度（米/秒）：与追逐同速——逃跑不能比追击慢。
pub const FLEE_SPEED: f32 = CHASE_SPEED;
/// 逃跑持续时长（秒）：触发后有粘性（恐慌语义）——途中转头看不见玩家
/// 也不会立刻缩回巡逻（同时消除视线抖动引起的模式振荡）。
pub const FLEE_SECS: f32 = 4.0;
/// 增援呼叫半径（米）：发现者与接收者的**水平**距离上限。
pub const REINFORCE_RADIUS: f32 = 12.0;
/// 支援时长（秒）：到期自动回巡逻（无记忆的搜索点，v1 简化）。
pub const REINFORCE_SECS: f32 = 8.0;
/// 支援移动速度（米/秒）。
pub const SUPPORT_SPEED: f32 = CHASE_SPEED;

/// 黑板键：本帧是否看得见玩家（感知动作写、追逐/逃跑条件读——黑板是
/// 感知与动作之间**唯一**的通信通道）。
pub const BB_SEES: &str = "guard.sees";
/// 黑板键：当前模式名（`patrol` / `chase` / `freeze` / `flee` / `support`）。
pub const BB_MODE: &str = "guard.mode";
/// 黑板键：支援剩余时长（秒；> 0 = 支援中）。
pub const BB_SUPPORT_TIMER: &str = "guard.support_timer";
/// 黑板键：支援目标点（世界坐标；警报发出时的玩家位置）。
pub const BB_ALERT_POS: &str = "guard.alert_pos";
/// 黑板键：逃跑剩余时长（秒；> 0 = 逃跑中——触发粘性，见 [`FLEE_SECS`]）。
pub const BB_FLEE_TIMER: &str = "guard.flee_timer";

/// 守卫行为树的模式名（黑板 `guard.mode` 的取值；诊断与测试断言用）。
pub mod mode {
    /// 巡逻。
    pub const PATROL: &str = "patrol";
    /// 追逐。
    pub const CHASE: &str = "chase";
    /// 击晕冻结。
    pub const FREEZE: &str = "freeze";
    /// 逃跑（能力①）。
    pub const FLEE: &str = "flee";
    /// 支援（能力②）。
    pub const SUPPORT: &str = "support";
}

/// 守卫行为树的具名叶子 id（引擎不认识它们，游戏侧在这里登记）。
pub mod action {
    /// 条件：被击晕。
    pub const STUNNED: &str = "guard.stunned";
    /// 动作：冻结（v1 不醒来）。
    pub const FREEZE: &str = "guard.freeze";
    /// 动作：感知 → 警觉 → 满格发现。
    pub const SENSE: &str = "guard.sense";
    /// 条件：受威胁（能力①开启 且（低血量 或 被逼近））。
    pub const THREATENED: &str = "guard.threatened";
    /// 动作：逃跑。
    pub const FLEE: &str = "guard.flee";
    /// 条件：追逐中。
    pub const CHASING: &str = "guard.chasing";
    /// 动作：追逐。
    pub const CHASE: &str = "guard.chase";
    /// 条件：支援中（能力②开启 且 支援计时 > 0）。
    pub const SUPPORTING: &str = "guard.supporting";
    /// 动作：支援。
    pub const SUPPORT: &str = "guard.support";
    /// 条件：巡逻中。
    pub const PATROLLING: &str = "guard.patrolling";
    /// 动作：巡逻。
    pub const PATROL: &str = "guard.patrol";
}

/// 守卫能力开关（**缺席语义 = 旧行为**）：默认全 `false` 时行为树只走
/// 「击晕冻结 / 感知 / 追逐 / 巡逻」四条既有分支，逐位等价于 R35–R41 的
/// 手写状态机；两个演示能力各自独立开关，测试与关卡可按需打开。
///
/// 这是「组件缺席语义」在纯核心里的等价物：守卫不是实体，开关就是字段；
/// 默认构造 = 能力缺席 = 老玩家的既有体验不变。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GuardCapabilities {
    /// 逃跑（能力①，默认关）：低血量或被逼近时选择远离玩家的路点。
    pub flee: bool,
    /// 呼叫增援（能力②，默认关）：发现玩家时发 [`InfiltrationEvent::GuardAlert`]，
    /// 半径内其他守卫进入支援态（黑板记支援计时与目标点）。
    pub reinforcement: bool,
}

/// 角色控制器参数：引擎 [`CharacterParams::default`] 的俯视改版——
/// 半径 0.4 / 半高 0.9 / 台阶 0.3 / 坡限 45° 保持缺省，**重力缩放 0**
/// （俯视平面无竖直运动；`grounded` 恒 false 是布局无地板碰撞的预期
/// 语义，见模块文档「与引擎能力的关系」）。
pub const CHARACTER: CharacterParams = CharacterParams {
    radius: 0.4,
    half_height: 0.9,
    step_height: 0.3,
    slope_limit_radians: std::f32::consts::FRAC_PI_4,
    snap_distance: 0.2,
    gravity_scale: 0.0,
};

/// 角色半高（速查）：玩家 / 守卫的胶囊中心 y = 此值（脚底 y = 0）。
pub const HALF_HEIGHT: f32 = 0.9;

// ---------------------------------------------------------------------------
// 关卡数据（写死、确定性；视觉网格与逻辑静态体共用——见 WALLS / DOOR）
// ---------------------------------------------------------------------------

/// 一面墙（AABB 静态体）：高 2.5 m（y∈(0, 2.5)），`half` = 半边长。
/// 视觉网格与逻辑静态体 / 导航体 / 遮挡体共用本结构（插件层按
/// `half × 2` 缩放立方体，尺寸同源零漂移）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wall {
    /// 盒中心（世界坐标）。
    pub center: Vec3,
    /// 半边长。
    pub half: Vec3,
}

/// 内外墙（碰撞 / 导航 / 遮挡共用的事实来源）。布局推导见模块文档；
/// 门洞恒 2 m 宽。
pub const WALLS: [Wall; 12] = [
    // 外墙四面（内缘 = 可玩区边界）
    Wall {
        center: Vec3::new(0.0, 1.25, -9.2),
        half: Vec3::new(12.4, 1.25, 0.2),
    }, // 北
    Wall {
        center: Vec3::new(0.0, 1.25, 9.2),
        half: Vec3::new(12.4, 1.25, 0.2),
    }, // 南
    Wall {
        center: Vec3::new(-12.2, 1.25, 0.0),
        half: Vec3::new(0.2, 1.25, 9.4),
    }, // 西
    Wall {
        center: Vec3::new(12.2, 1.25, 0.0),
        half: Vec3::new(0.2, 1.25, 9.4),
    }, // 东
    // 内墙 z = -2（北排房间南墙），门洞 x∈(-8,-6) 与 x∈(6,8)
    Wall {
        center: Vec3::new(-10.0, 1.25, -2.0),
        half: Vec3::new(2.0, 1.25, 0.2),
    },
    Wall {
        center: Vec3::new(0.0, 1.25, -2.0),
        half: Vec3::new(6.0, 1.25, 0.2),
    },
    Wall {
        center: Vec3::new(10.0, 1.25, -2.0),
        half: Vec3::new(2.0, 1.25, 0.2),
    },
    // 内墙 z = +2（南排房间北墙），门洞 x∈(-8,-6) 与 x∈(6,8)
    Wall {
        center: Vec3::new(-10.0, 1.25, 2.0),
        half: Vec3::new(2.0, 1.25, 0.2),
    },
    Wall {
        center: Vec3::new(0.0, 1.25, 2.0),
        half: Vec3::new(6.0, 1.25, 0.2),
    },
    Wall {
        center: Vec3::new(10.0, 1.25, 2.0),
        half: Vec3::new(2.0, 1.25, 0.2),
    },
    // 内墙 x = 0（南北各一段，把南北排各分成东西两室）
    Wall {
        center: Vec3::new(0.0, 1.25, -5.5),
        half: Vec3::new(0.2, 1.25, 3.7),
    },
    Wall {
        center: Vec3::new(0.0, 1.25, 5.5),
        half: Vec3::new(0.2, 1.25, 3.7),
    },
];

/// 压力门（`WALLS` 之外的唯一可开闭静态体）：封闭北排门洞 x∈(6,8)、
/// z=-2。关闭时进逻辑静态体（挡玩家 / 挡视线 / 进导航图），被压力板
/// 打开后从静态体移除（[`InfiltrationState::press_plate`]）——实体侧
/// 由插件层做「隐藏 + 碰撞组清零」的标记语义。
pub const DOOR: Wall = Wall {
    center: Vec3::new(7.0, 1.25, -2.0),
    half: Vec3::new(1.0, 1.25, 0.2),
};

/// 压力板（世界坐标 XZ）：踩上即开压力门。逻辑侧踩上判定半径
/// [`PLATE_RADIUS`]；实体侧是 sensor 盒（R34 接触事件），插件层两条
/// 路径调用同一入口 [`InfiltrationState::press_plate`]（幂等）。
pub const PLATE_POS: Vec2 = Vec2::new(2.0, 0.5);
/// 压力板踩上判定半径（米）：语义 = **玩家中心进入板投影内**（板面
/// 0.9×0.9 m 的半宽 0.45，站上板面才触发，靠近不触发）；与插件层的
/// sensor 盒半宽、视觉板 `scale = 0.9` 三方同源同值。
pub const PLATE_RADIUS: f32 = 0.45;

/// 三个数据核心（XZ 坐标，y = 拾取高度）：NW（门洞直达）、NE（压力门
/// 之后——必经压力板）、SE（南排东室）。
pub const CORES: [Vec2; 3] = [
    Vec2::new(-10.0, -7.0),
    Vec2::new(10.0, -7.0),
    Vec2::new(10.0, 7.0),
];

/// 出口触发域（SW 室西北角）：核心全收 + 玩家进入 → 胜利。
pub const EXIT_CENTER: Vec3 = Vec3::new(-10.0, 0.9, 7.0);
pub const EXIT_HALF: Vec3 = Vec3::new(1.5, 1.2, 1.5);

/// 出生点（脚底）：走廊西端。
pub const SPAWN_FOOT: Vec3 = Vec3::new(-10.5, 0.0, 0.0);

/// 守卫巡逻路线（XZ 路点环）：两条路线都刻意避开玩家主路线的中线，
/// 「绕墙」由 [`NavGrid::find_path` ] 逐段保证（headless 断言折点）。
/// **刻意不让任何守卫巡逻走廊**——走廊是 3.6 m 宽的直通区，守卫沿线
/// 迎面而来的视锥无法用走位避开（v1 无掩体物）；设计上走廊由「时机」
/// 把守（守卫巡出房间、压力门未开），房间由守卫把守。
pub const GUARD_ROUTES: [&[Vec2]; 2] = [
    // 守卫 0：NW 室矩形环（绕核心 A）
    &[
        Vec2::new(-8.0, -6.5),
        Vec2::new(-4.0, -6.5),
        Vec2::new(-4.0, -3.5),
        Vec2::new(-8.0, -3.5),
    ],
    // 守卫 1：SE 室矩形环（绕核心 B；进房门洞在环外，玩家从门洞一侧
    // 贴墙绕行——环与东墙之间留 ~2.5 m 通道）
    &[
        Vec2::new(3.5, 4.5),
        Vec2::new(8.5, 4.5),
        Vec2::new(8.5, 7.5),
        Vec2::new(3.5, 7.5),
    ],
];

/// 导航图覆盖域与参数（构建参数收拢一处：重建时与首建同参）。
pub const NAV_BOUNDS_MIN: Vec3 = Vec3::new(-12.4, 0.0, -9.4);
pub const NAV_BOUNDS_MAX: Vec3 = Vec3::new(12.4, 0.0, 9.4);
/// 导航 cell 边长（米）：≤ agent 半径的经验规则（见 `iris_core::nav`）。
pub const NAV_CELL_SIZE: f32 = 0.5;
/// 导航 agent 膨胀半径（米）：守卫 / 玩家胶囊半径 0.4 + 5 cm 裕度。
pub const NAV_AGENT_RADIUS: f32 = 0.45;

/// 逻辑静态体（关卡碰撞事实来源）：`WALLS` 全部 + 压力门（仅关闭时）。
/// 角色移动 / 守卫视线遮挡 / 导航图构建三方共用同一份列表。
pub fn level_statics(door_open: bool) -> Vec<(Vec3, ColliderShape)> {
    let mut statics: Vec<(Vec3, ColliderShape)> = Vec::with_capacity(WALLS.len() + 1);
    for wall in WALLS.iter() {
        statics.push((
            wall.center,
            ColliderShape::Box {
                half_extents: wall.half,
            },
        ));
    }
    if !door_open {
        statics.push((
            DOOR.center,
            ColliderShape::Box {
                half_extents: DOOR.half,
            },
        ));
    }
    statics
}

/// 按 XZ 判点是否在 AABB 内（y 无关——俯视语义）。
pub fn point_in_aabb_xz(point: Vec2, center: Vec3, half: Vec3) -> bool {
    (point.x - center.x).abs() <= half.x && (point.y - center.z).abs() <= half.z
}

// ---------------------------------------------------------------------------
// 输入 / 事件 / 阶段
// ---------------------------------------------------------------------------

/// 一帧的游戏输入：世界系水平意图 + 潜行键 + 互动键 + 步长。纯核心不碰
/// 键盘——插件层把「相机相对的 WASD」翻译成世界系向量（相机固定俯角时
/// 世界系与屏幕系一致）；`interact` 为边沿语义（本帧按下沿才为 true）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InfiltrationInput {
    /// 世界系水平移动意图（x = 世界 +X，y = 世界 +Z）。
    pub move_dir: Vec2,
    /// 本帧是否按住潜行（Shift）。
    pub sneak: bool,
    /// 本帧是否按下互动（E，边沿）。
    pub interact: bool,
    /// 步长（秒）。
    pub dt: f32,
}

/// 一帧内产生的游戏事件（含世界坐标）：插件层据此触发空间音频 / ragdoll
/// / 横幅。每帧 `step` 开头清空——`step` 返回后
/// [`InfiltrationState::frame_events`] 恰好持有本帧全部事件（顺序 =
/// 结算顺序，确定性）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InfiltrationEvent {
    /// 拾取数据核心（位置 = 核心坐标，y = 拾取高度）。
    CorePicked { pos: Vec3, index: usize },
    /// 压力板被踩下（位置 = 压力板坐标）。
    PlatePressed { pos: Vec3 },
    /// 压力门打开（位置 = 门中心）。
    DoorOpened { pos: Vec3 },
    /// 守卫击晕（位置 = 守卫胶囊中心；朝向 = 击晕时 yaw，供踢飞方向）。
    GuardStunned {
        pos: Vec3,
        facing: f32,
        index: usize,
    },
    /// 玩家被发现（位置 = 玩家胶囊中心；count = 累计次数，1 起）。
    Detection { pos: Vec3, guard: usize, count: u32 },
    /// 守卫呼叫增援（R65 能力②；位置 = 发现时的玩家位置，`from` = 发现者
    /// 下标）。仅在发现者开启 [`GuardCapabilities::reinforcement`] 时产生。
    GuardAlert { pos: Vec3, from: usize },
    /// 全核心到手 + 抵达出口（位置 = 玩家胶囊中心）。
    Victory { pos: Vec3 },
    /// 被发现满三次（位置 = 玩家胶囊中心）。
    Defeat { pos: Vec3 },
}

/// 游戏阶段：潜入中 / 胜利 / 告负（终局均冻结；重开 = 重建
/// [`InfiltrationState`]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// 潜入中。
    Playing,
    /// 全核心到手 + 抵达出口。
    Victory,
    /// 被发现满三次。
    Defeat,
}

/// 守卫状态机：巡逻（固定路点环 find_path 逐段走）/ 追逐（朝玩家最后
/// 已知位置 find_path）/ 被击晕（冻结，v1 不醒来）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardState {
    /// 沿固定路点环巡逻。
    Patrol,
    /// 朝玩家最后已知位置追逐。
    Chase,
    /// 被击晕（v1 不醒来，见模块文档「击晕与诚实边界」）。
    Stunned,
}

/// 一名守卫的纯逻辑状态。字段全部公开：测试可以直接构造 / 修改中间
/// 状态，插件层按只读约定消费。
#[derive(Clone, Debug)]
pub struct Guard {
    /// 胶囊中心（世界坐标；脚底 = y − [`HALF_HEIGHT`]）。
    pub pos: Vec3,
    /// 朝向（绕 Y 弧度；0 = 面朝 -Z，引擎前方约定）。
    pub facing: f32,
    /// 警觉计量 0..=1：视锥内可见时上升、离开视锥衰减；满格 = 发现。
    pub alert: f32,
    /// 状态机。
    pub state: GuardState,
    /// 所属巡逻路线（[`GUARD_ROUTES`] 下标，出生即定）。
    pub route_index: usize,
    /// 路线环内的目标路点下标（`path` 清空后按此寻路）。
    pub route_next: usize,
    /// 当前 find_path 航点队列（y 恒 0，移动时覆写为守卫自己的高度）。
    pub path: Vec<Vec3>,
    /// 航点队列消费下标。
    pub path_index: usize,
    /// 追逐重寻路计时（秒）。
    pub repath_timer: f32,
    /// 玩家最后已知位置（追逐目标）。
    pub last_known: Vec3,
    /// 失去视线后的追逐剩余时间（秒）。
    pub chase_timer: f32,
    /// 本帧实际移动速度（米/秒；供表现层混合空间参数，静止 = 0）。
    pub anim_speed: f32,
    /// 能力开关（默认全关 = 旧行为；见 [`GuardCapabilities`]）。
    pub capabilities: GuardCapabilities,
    /// 血量 0..=1（v1 无伤害系统：仅逃跑条件使用；默认满血 1.0）。
    pub health: f32,
}

impl Guard {
    /// 从巡逻路线 `route_index` 的 0 号路点出发。
    pub fn new(route: usize) -> Self {
        let start = GUARD_ROUTES[route][0];
        Self {
            pos: Vec3::new(start.x, HALF_HEIGHT, start.y),
            facing: 0.0,
            alert: 0.0,
            state: GuardState::Patrol,
            route_index: route,
            route_next: 1 % GUARD_ROUTES[route].len(),
            path: Vec::new(),
            path_index: 0,
            repath_timer: 0.0,
            last_known: Vec3::ZERO,
            chase_timer: 0.0,
            anim_speed: 0.0,
            capabilities: GuardCapabilities::default(),
            health: 1.0,
        }
    }

    /// 朝向前方单位向量（XZ）：yaw 0 = -Z（引擎前方约定）。
    pub fn forward(&self) -> Vec2 {
        Vec2::new(-self.facing.sin(), -self.facing.cos())
    }
}

// ---------------------------------------------------------------------------
// 状态
// ---------------------------------------------------------------------------

/// 游戏全局状态：阶段 + 玩家 + 目标进度 + 守卫 + 门 + 导航图 + 本帧
/// 事件。字段全部公开：测试可以直接构造 / 修改中间状态，插件层按只读
/// 约定消费（每帧由 [`InfiltrationState::step`] 独占推进；压力板经
/// [`InfiltrationState::press_plate`] 由插件层在接触事件里调用）。
#[derive(Clone, Debug)]
pub struct InfiltrationState {
    pub phase: Phase,
    /// 玩家胶囊中心（世界坐标；脚底 = y − [`HALF_HEIGHT`]）。
    pub pos: Vec3,
    /// 玩家线速度（世界系；水平分量每帧向输入意图趋近——由引擎控制器
    /// 的滑行结果回灌）。
    pub vel: Vec3,
    /// 玩家模型朝向（绕 Y 弧度）。
    pub facing: f32,
    /// 玩家本帧是否潜行（表现层姿态 / 守卫发现半径共用）。
    pub sneaking: bool,
    /// 数据核心拾取进度（下标与 [`CORES`] 一一对应）。
    pub cores: Vec<bool>,
    /// 被发现次数（≥ [`MAX_DETECTIONS`] 即告负）。
    pub detections: u32,
    /// 压力门是否已打开（打开后不可再关——v1 单向语义）。
    pub door_open: bool,
    /// 压力板上的实体计数（插件层从 ContactStarted/Ended 维护，
    /// 供测试断言 begin/end 成对语义；开门本身是单向闩锁）。
    pub plate_occupants: u32,
    /// 守卫（下标即 [`GUARD_ROUTES`] 下标）。
    pub guards: Vec<Guard>,
    /// 守卫的行为树「大脑」（与 [`Self::guards`] **同下标一一对应**：树 +
    /// 运行态 + 黑板三合一）。引擎的 [`BehaviorAgent`] 是逐代理组件；纯核心
    /// 用平行 Vec 表达同一语义（索引即代理身份，长度不变式与 guards 同源）。
    pub guard_brains: Vec<BehaviorAgent>,
    /// 对局累计时间（秒；`step` 累加，确定性）。
    pub elapsed: f32,
    /// 逻辑静态体（墙 + 关闭的压力门；开门时重建）。
    pub statics: Vec<(Vec3, ColliderShape)>,
    /// 视线遮挡用的静态体快照（[`Self::statics`] 的 [`StaticObstacle`] 形式，
    /// 恒等旋转）——与 statics **同源同序**：构造与 [`Self::press_plate`]
    /// 重建时一并刷新，逐帧零分配。
    pub vision_statics: Vec<StaticObstacle>,
    /// 导航图（启动 build 一次；开门时按引擎「便宜重建」姿势整图重建）。
    pub nav: NavGrid,
    /// 本帧事件列表（每帧 `step` 开头清空，见 [`InfiltrationEvent`]）。
    pub frame_events: Vec<InfiltrationEvent>,
    /// 本帧产生的增援警报（守卫循环结束后统一派发 → **下一帧**在接收者
    /// 黑板上生效；能力②关闭时恒空——见 [`Self::dispatch_alerts`]）。
    pub pending_alerts: Vec<PendingAlert>,
    /// 自动玩家的路线点游标（脚本簿记，普通玩家不用；见 [`AutoPilot`]）。
    pub autopilot_route: usize,
}

impl Default for InfiltrationState {
    fn default() -> Self {
        Self::new()
    }
}

impl InfiltrationState {
    /// 新状态：玩家站在出生点（胶囊中心 = [`SPAWN_FOOT`] + 半高），
    /// 两名守卫各就巡逻环 0 号位，导航图按「门关闭」构建。
    pub fn new() -> Self {
        let pos = SPAWN_FOOT + Vec3::new(0.0, HALF_HEIGHT, 0.0);
        let statics = level_statics(false);
        let nav = NavGrid::build(nav_params(), &statics);
        let vision_statics = flat_statics(&statics);
        // 同一棵树克隆给每名守卫（树是不可变纯数据），运行态与黑板各自独立
        let tree = guard_behavior_tree();
        let guards: Vec<Guard> = (0..GUARD_ROUTES.len()).map(Guard::new).collect();
        let guard_brains: Vec<BehaviorAgent> = (0..GUARD_ROUTES.len())
            .map(|_| BehaviorAgent::new(tree.clone()))
            .collect();
        Self {
            phase: Phase::Playing,
            pos,
            vel: Vec3::ZERO,
            facing: 0.0,
            sneaking: false,
            cores: vec![false; CORES.len()],
            detections: 0,
            door_open: false,
            plate_occupants: 0,
            guards,
            guard_brains,
            elapsed: 0.0,
            statics,
            vision_statics,
            nav,
            frame_events: Vec::new(),
            pending_alerts: Vec::new(),
            autopilot_route: 0,
        }
    }

    /// 已拾取核心数。
    pub fn cores_collected(&self) -> usize {
        self.cores.iter().filter(|&&got| got).count()
    }

    /// 全核心到手？
    pub fn all_cores_collected(&self) -> bool {
        self.cores.iter().all(|&got| got)
    }

    /// 当前守卫最高警觉（0..=1，HUD 警觉指示用）。
    pub fn max_alert(&self) -> f32 {
        self.guards.iter().map(|g| g.alert).fold(0.0, f32::max)
    }

    /// 派发本帧产生的增援警报：半径内**其他**守卫写入支援黑板键
    /// （[`BB_SUPPORT_TIMER`] / [`BB_ALERT_POS`]），被派发者下一帧进入支援
    /// 分支。语义细节：
    /// - 只派发给开启 [`GuardCapabilities::reinforcement`] 且水平距离
    ///   ≤ [`REINFORCE_RADIUS`] 的守卫（发现者自己除外——它在追逐）；
    /// - 已在支援中的守卫**不被覆盖**（保留剩余时长与目标点，先到先得）；
    /// - 派发序 = 警报产出序 × 守卫下标升序（确定性）。
    fn dispatch_alerts(&mut self) {
        if self.pending_alerts.is_empty() {
            return;
        }
        let alerts = std::mem::take(&mut self.pending_alerts);
        for alert in alerts {
            for (index, guard) in self.guards.iter().enumerate() {
                if index == alert.from || !guard.capabilities.reinforcement {
                    continue;
                }
                let to = Vec2::new(guard.pos.x - alert.pos.x, guard.pos.z - alert.pos.z);
                if to.length() > REINFORCE_RADIUS {
                    continue;
                }
                let bb = &mut self.guard_brains[index].blackboard;
                if bb.get_f32_or(BB_SUPPORT_TIMER, 0.0) > 0.0 {
                    continue; // 已在支援：不打断
                }
                bb.set_f32(BB_SUPPORT_TIMER, REINFORCE_SECS);
                bb.set_vec3(BB_ALERT_POS, alert.pos);
            }
        }
    }

    /// 压力板被踩下（两条调用路径：纯逻辑的踩上距离判定 [`PLATE_RADIUS`]
    /// 与插件层从 ContactStarted 事件调用——幂等，单向闩锁开门）：静态体
    /// 重建（门移除）+ 导航图按引擎文档推荐的「便宜重建」姿势整图重建
    /// （守卫此后可穿门洞追逐）。重复踩不重复触发。
    pub fn press_plate(&mut self) {
        if self.door_open {
            return;
        }
        self.door_open = true;
        self.statics = level_statics(true);
        self.vision_statics = flat_statics(&self.statics);
        self.nav = NavGrid::build(nav_params(), &self.statics);
        self.frame_events.push(InfiltrationEvent::PlatePressed {
            pos: Vec3::new(PLATE_POS.x, HALF_HEIGHT, PLATE_POS.y),
        });
        self.frame_events
            .push(InfiltrationEvent::DoorOpened { pos: DOOR.center });
    }

    /// 压力板上有实体进入 / 离开（插件层从 ContactStarted/Ended 调用，
    /// begin/end 成对语义；纯逻辑距离判定不计——occupants 是 R34 接触
    /// 事件链路的簿记，headless 纯核心测试经由接触事件链路覆盖）。
    pub fn enter_plate(&mut self) {
        self.plate_occupants = self.plate_occupants.saturating_add(1);
    }

    pub fn leave_plate(&mut self) {
        self.plate_occupants = self.plate_occupants.saturating_sub(1);
    }

    /// 推进一帧。终局阶段冻结（什么都不做）；`dt <= 0` 时冻结对局。
    /// 帧内顺序见模块文档（是确定性的一部分）。
    pub fn step(&mut self, input: &InfiltrationInput) {
        self.frame_events.clear();
        if self.phase != Phase::Playing {
            return;
        }
        let dt = input.dt;
        if dt <= 0.0 {
            return;
        }
        self.elapsed += dt;
        self.sneaking = input.sneak;

        // ---- 玩家移动：水平意图 → 速度 → 引擎角色控制器（俯视、无
        // 重力），结果回灌（调用方保留所有权）----
        let speed = if input.sneak { SNEAK_SPEED } else { MOVE_SPEED };
        let target = input.move_dir * speed;
        self.vel.x = target.x;
        self.vel.z = target.y;
        let out = move_and_slide_statics(self.pos, self.vel, dt, &CHARACTER, &self.statics);
        self.pos = out.position;
        self.vel = out.velocity;
        if input.move_dir.length_squared() > 1e-6 {
            self.facing = (-input.move_dir.x).atan2(-input.move_dir.y);
        }

        // ---- 数据核心：与玩家胶囊中心距离拾取（每颗至多一次）----
        for (index, core) in CORES.iter().enumerate() {
            if !self.cores[index] {
                let core_pos = Vec3::new(core.x, HALF_HEIGHT, core.y);
                if self.pos.distance(core_pos) <= CORE_RADIUS {
                    self.cores[index] = true;
                    self.frame_events.push(InfiltrationEvent::CorePicked {
                        pos: core_pos,
                        index,
                    });
                }
            }
        }

        // ---- 压力板：玩家踩上（XZ 距离判定——确定性的纯逻辑路径；
        // 窗口路径的 ContactStarted 事件调用同一入口，幂等）----
        if !self.door_open {
            let plate_dist = Vec2::new(self.pos.x - PLATE_POS.x, self.pos.z - PLATE_POS.y).length();
            if plate_dist <= PLATE_RADIUS {
                self.press_plate();
            }
        }

        // ---- 击晕：interact 边沿 + 最近一名「在背后且 2.2 m 内」的
        // 未击晕守卫（每帧至多一名，结算顺序确定）----
        if input.interact {
            let mut best: Option<(usize, f32)> = None;
            for (index, guard) in self.guards.iter().enumerate() {
                if guard.state == GuardState::Stunned {
                    continue;
                }
                let to_player = Vec2::new(self.pos.x - guard.pos.x, self.pos.z - guard.pos.z);
                let dist = to_player.length();
                if !(1e-4..=STUN_RADIUS).contains(&dist) {
                    continue;
                }
                // 背后 = 守卫前方与「守卫→玩家」方向点积低于背面阈值
                if guard.forward().dot(to_player / dist) < STUN_BEHIND_COS
                    && best.is_none_or(|(_, d)| dist < d)
                {
                    best = Some((index, dist));
                }
            }
            if let Some((index, _)) = best {
                let guard = &mut self.guards[index];
                guard.state = GuardState::Stunned;
                guard.alert = 0.0;
                guard.anim_speed = 0.0;
                self.frame_events.push(InfiltrationEvent::GuardStunned {
                    pos: guard.pos,
                    facing: guard.facing,
                    index,
                });
            }
        }

        // ---- 守卫：感知/警觉（用移动前的位姿——顺序是语义）→ 移动。
        // R65 起每名守卫 tick 自己的行为树一次（树见 [`guard_behavior_tree`]）----
        for index in 0..self.guards.len() {
            self.update_guard(index, dt);
        }

        // ---- 增援派发（能力②，默认关）：本帧产生的警报在守卫循环结束后统一
        // 派发，**下一帧生效**（事件驱动语义）。能力关时队列恒空 ⇒ 零影响 ----
        self.dispatch_alerts();

        // ---- 发现满三次 → 告负 ----
        if self.detections >= MAX_DETECTIONS {
            self.phase = Phase::Defeat;
            self.frame_events
                .push(InfiltrationEvent::Defeat { pos: self.pos });
            return;
        }

        // ---- 出口：全核心 + 进入触发域 → 胜利（终局冻结）----
        if self.all_cores_collected()
            && point_in_aabb_xz(Vec2::new(self.pos.x, self.pos.z), EXIT_CENTER, EXIT_HALF)
        {
            self.phase = Phase::Victory;
            self.frame_events
                .push(InfiltrationEvent::Victory { pos: self.pos });
        }
    }

    /// 单名守卫的一帧：**行为树 tick 一次**（R65 迁移，结构见
    /// [`guard_behavior_tree`]）。旧状态机的四条分支（击晕冻结 / 感知警觉 /
    /// 追逐 / 巡逻）逐位等价地落在树上；两个演示能力（逃跑 / 支援）默认关
    /// （[`GuardCapabilities`] = 能力缺席 = 旧行为）。
    ///
    /// 借用结构（字段解构，照「系统用 `&World` + 两阶段」的纪律）：
    /// `guards[index]` 交给上下文（动作要写位置 / 航点），`guard_brains[index]`
    /// （树 + 运行态 + 黑板）交给 tick，`nav` / `vision_statics` / 玩家位姿
    /// 只读共享——三者互不别名，借用检查器可验证。
    fn update_guard(&mut self, index: usize, dt: f32) {
        let InfiltrationState {
            guards,
            guard_brains,
            nav,
            vision_statics,
            pos,
            sneaking,
            detections,
            frame_events,
            pending_alerts,
            ..
        } = self;
        let view = GuardView {
            player_pos: *pos,
            sneaking: *sneaking,
            nav,
            statics: vision_statics,
        };
        let mut ctx = GuardCtx {
            index,
            guard: &mut guards[index],
            view,
            detections,
            events: frame_events,
            alerts: pending_alerts,
        };
        guard_brains[index].tick(&mut ctx, dt);
    }

    /// 守卫 `index` 本帧是否看得见玩家（视锥距离/半角 + 静态体遮挡）。
    /// 委托给自由函数 [`guard_sees`]（自动玩家与单测复用同一套感知几何）——
    /// 行为树里的感知动作走同一条路径（`GuardCtx::sense`）。
    pub fn guard_sees_player(&self, index: usize) -> bool {
        guard_sees(
            &self.guards[index],
            self.pos,
            self.sneaking,
            &self.vision_statics,
        )
    }
}

// ---------------------------------------------------------------------------
// 守卫行为树（R65：数据驱动节点 + 黑板 + 感知事件的实战载荷）
// ---------------------------------------------------------------------------

/// 守卫行为树（结构 = 文档；引擎侧只提供节点语义，动作实现在本模块）：
///
/// ```text
/// Selector                                     ← 优先级从上到下
/// ├── Sequence[ Condition(guard.stunned),      ← 被击晕：冻结，不感知不移动
/// │             Action(guard.freeze) ]            （v1 不醒来）
/// └── Sequence[ Action(guard.sense),           ← 感知 → 警觉 →（满格）发现
///               Selector[
///                 ├── Sequence[ Condition(guard.threatened),  ← ①逃跑（默认关）
///                 │             Action(guard.flee) ]
///                 ├── Sequence[ Condition(guard.chasing),     ← 追击
///                 │             Action(guard.chase) ]
///                 ├── Sequence[ Condition(guard.supporting),  ← ②增援支援（默认关）
///                 │             Action(guard.support) ]
///                 └── Sequence[ Condition(guard.patrolling),  ← 巡逻（兜底）
///                               Action(guard.patrol) ]
///               ] ]
/// ```
///
/// 「击晕」必须是最高优先级分支：旧实现在击晕时**直接返回**（不感知、不
/// 移动），树上就是「条件命中即短路」的 Selector 语义——警觉不会为瘫倒的
/// 守卫继续累积（否则会出现「躺着发现玩家」）。
pub fn guard_behavior_tree() -> BehaviorTree {
    BehaviorTree::new(NodeSpec::selector([
        NodeSpec::sequence([
            NodeSpec::condition(action::STUNNED),
            NodeSpec::action(action::FREEZE),
        ]),
        NodeSpec::sequence([
            NodeSpec::action(action::SENSE),
            NodeSpec::selector([
                NodeSpec::sequence([
                    NodeSpec::condition(action::THREATENED),
                    NodeSpec::action(action::FLEE),
                ]),
                NodeSpec::sequence([
                    NodeSpec::condition(action::CHASING),
                    NodeSpec::action(action::CHASE),
                ]),
                NodeSpec::sequence([
                    NodeSpec::condition(action::SUPPORTING),
                    NodeSpec::action(action::SUPPORT),
                ]),
                NodeSpec::sequence([
                    NodeSpec::condition(action::PATROLLING),
                    NodeSpec::action(action::PATROL),
                ]),
            ]),
        ]),
    ]))
}

/// 一次增援警报的派发请求（本帧产出 → 守卫循环结束后统一派发 → 下一帧生效）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PendingAlert {
    /// 发现者下标（不给自己派发）。
    pub from: usize,
    /// 警报点（发现时的玩家位置）。
    pub pos: Vec3,
}

/// 守卫循环期间**只读**的世界视图（玩家位姿 / 导航 / 视线静态体）。
struct GuardView<'a> {
    player_pos: Vec3,
    sneaking: bool,
    nav: &'a NavGrid,
    statics: &'a [StaticObstacle],
}

/// 守卫行为树上下文：把具名叶子 id 映射到纯核心的状态操作。
///
/// 黑板**不**在这个结构里——它由 `BehaviorAgent::tick` 单独传入（逐代理），
/// 于是「感知结果 → 黑板 → 动作」是唯一的通信通道（动作之间不通过结构
/// 字段走私状态）。
struct GuardCtx<'a> {
    index: usize,
    guard: &'a mut Guard,
    view: GuardView<'a>,
    detections: &'a mut u32,
    events: &'a mut Vec<InfiltrationEvent>,
    alerts: &'a mut Vec<PendingAlert>,
}

/// 模式切换：黑板记住上一帧的模式，切换时清空航点队列——路径是模式的私有
/// 资源（巡逻 / 追逐 / 逃跑 / 支援各走各的航线，互不串味）。旧语义里
/// 「转追逐 / 放弃追逐」本来就清路径，故对既有行为逐位无影响。
fn enter_mode(guard: &mut Guard, bb: &mut Blackboard, mode: &str) {
    if bb.get_text(BB_MODE) != Some(mode) {
        guard.path.clear();
        guard.path_index = 0;
        bb.set_text(BB_MODE, mode);
    }
}

/// 只写模式名（冻结分支保留既有航点，与旧实现「击晕不清路径」一致）。
fn set_mode(bb: &mut Blackboard, mode: &str) {
    if bb.get_text(BB_MODE) != Some(mode) {
        bb.set_text(BB_MODE, mode);
    }
}

/// 沿航点队列移动一步：返回是否恰好消费完队列（到点）。航点 y 恒 0
/// （2.5D 导航语义）——移动时覆写为守卫自己的中心高度。旧
/// `InfiltrationState::guard_advance` 的逐字搬运（逐位同源）。
fn advance_along_path(guard: &mut Guard, speed: f32, dt: f32) -> bool {
    let Some(&wp) = guard.path.get(guard.path_index) else {
        guard.anim_speed = 0.0;
        return false;
    };
    let target = Vec2::new(wp.x, wp.z);
    let pos = Vec2::new(guard.pos.x, guard.pos.z);
    let to = target - pos;
    let dist = to.length();
    let step = speed * dt;
    if dist <= WAYPOINT_REACH.max(step) {
        guard.pos.x = target.x;
        guard.pos.z = target.y;
        guard.path_index += 1;
        guard.anim_speed = 0.0;
        return guard.path_index >= guard.path.len();
    }
    let dir = to / dist;
    guard.pos.x += dir.x * step;
    guard.pos.z += dir.y * step;
    guard.facing = (-dir.x).atan2(-dir.y);
    guard.anim_speed = speed;
    false
}

impl GuardCtx<'_> {
    /// 感知 → 警觉 → 满格发现（旧 `update_guard` 前半段的逐位搬运）。
    ///
    /// 感知结果写进黑板（[`BB_SEES`]）——追逐 / 逃跑条件都从黑板读，
    /// 动作之间不共享结构字段。
    fn sense(&mut self, bb: &mut Blackboard, dt: f32) -> BtStatus {
        let sees = guard_sees(
            self.guard,
            self.view.player_pos,
            self.view.sneaking,
            self.view.statics,
        );
        bb.set_bool(BB_SEES, sees);
        let player_pos = self.view.player_pos;
        let index = self.index;
        let guard = &mut *self.guard;
        if sees {
            guard.alert = (guard.alert + ALERT_RISE * dt).min(1.0);
        } else {
            guard.alert = (guard.alert - ALERT_DECAY * dt).max(0.0);
        }
        if guard.alert >= 1.0 {
            guard.alert = 0.0;
            guard.state = GuardState::Chase;
            guard.last_known = player_pos;
            guard.chase_timer = CHASE_MEMORY;
            guard.repath_timer = 0.0;
            guard.path.clear();
            *self.detections += 1;
            let count = *self.detections;
            self.events.push(InfiltrationEvent::Detection {
                pos: player_pos,
                guard: index,
                count,
            });
            // 能力②：呼叫增援（默认关——关着就与旧行为逐位一致）
            if guard.capabilities.reinforcement {
                self.events.push(InfiltrationEvent::GuardAlert {
                    pos: player_pos,
                    from: index,
                });
                self.alerts.push(PendingAlert {
                    from: index,
                    pos: player_pos,
                });
            }
        }
        BtStatus::Success
    }

    /// 追逐：视线内刷新最后已知位置与记忆；按节拍重寻路；记忆耗尽回巡逻。
    fn chase(&mut self, bb: &mut Blackboard, dt: f32) -> BtStatus {
        let nav = self.view.nav;
        let player_pos = self.view.player_pos;
        let sees = bb.get_bool_or(BB_SEES, false);
        let guard = &mut *self.guard;
        enter_mode(guard, bb, mode::CHASE);
        if sees {
            guard.last_known = player_pos;
            guard.chase_timer = CHASE_MEMORY;
        } else {
            guard.chase_timer -= dt;
        }
        guard.repath_timer -= dt;
        if guard.repath_timer <= 0.0 {
            let from = Vec3::new(guard.pos.x, 0.0, guard.pos.z);
            let to = Vec3::new(guard.last_known.x, 0.0, guard.last_known.z);
            if let Some(path) = nav.find_path(from, to) {
                guard.path = path;
                guard.path_index = 0;
            }
            guard.repath_timer = REPATH_INTERVAL;
        }
        if guard.chase_timer <= 0.0 {
            guard.state = GuardState::Patrol;
            guard.path.clear();
            guard.anim_speed = 0.0;
            return BtStatus::Success;
        }
        advance_along_path(guard, CHASE_SPEED, dt);
        BtStatus::Success
    }

    /// 巡逻：朝当前路点 find_path 逐段走；到点后推进路线环。
    fn patrol(&mut self, bb: &mut Blackboard, dt: f32) -> BtStatus {
        let nav = self.view.nav;
        let guard = &mut *self.guard;
        enter_mode(guard, bb, mode::PATROL);
        if guard.path.is_empty() {
            let route = GUARD_ROUTES[guard.route_index];
            let target = route[guard.route_next.min(route.len() - 1)];
            // 端点 y=0：find_path 只看 XZ，航点 y 覆写见 advance_along_path
            let from = Vec3::new(guard.pos.x, 0.0, guard.pos.z);
            let to = Vec3::new(target.x, 0.0, target.y);
            match nav.find_path(from, to) {
                Some(path) => {
                    guard.path = path;
                    guard.path_index = 0;
                }
                // 不可达（理论不可达：路点与导航图同源）：原地等待一帧
                None => {
                    guard.anim_speed = 0.0;
                    return BtStatus::Success;
                }
            }
        }
        let arrived = advance_along_path(guard, PATROL_SPEED, dt);
        if arrived {
            guard.route_next = (guard.route_next + 1) % GUARD_ROUTES[guard.route_index].len();
            guard.path.clear();
        }
        BtStatus::Success
    }

    /// 逃跑（能力①，默认关）：锁定「距玩家最远的巡逻路点」全速脱离；
    /// 到达后重新评估（玩家可能在追，最远点会变）。
    fn flee(&mut self, bb: &mut Blackboard, dt: f32) -> BtStatus {
        let nav = self.view.nav;
        let player_pos = self.view.player_pos;
        let guard = &mut *self.guard;
        enter_mode(guard, bb, mode::FLEE);
        // 计时：首次进入盖章，其后逐帧递减（粘性由条件读同一键）
        let remaining = bb.get_f32_or(BB_FLEE_TIMER, 0.0);
        if remaining <= 0.0 {
            bb.set_f32(BB_FLEE_TIMER, FLEE_SECS);
        } else {
            bb.set_f32(BB_FLEE_TIMER, (remaining - dt).max(0.0));
        }
        if guard.path.is_empty() {
            let route = GUARD_ROUTES[guard.route_index];
            let mut best = 0usize;
            let mut best_distance = f32::NEG_INFINITY;
            for (index, waypoint) in route.iter().enumerate() {
                let dx = waypoint.x - player_pos.x;
                let dz = waypoint.y - player_pos.z;
                let distance = dx * dx + dz * dz;
                // 严格更优才替换：并列取下标小者（确定性 tie-break）
                if distance > best_distance {
                    best_distance = distance;
                    best = index;
                }
            }
            let waypoint = route[best];
            let from = Vec3::new(guard.pos.x, 0.0, guard.pos.z);
            let to = Vec3::new(waypoint.x, 0.0, waypoint.y);
            match nav.find_path(from, to) {
                Some(path) => {
                    guard.path = path;
                    guard.path_index = 0;
                }
                None => {
                    guard.anim_speed = 0.0;
                    return BtStatus::Success;
                }
            }
        }
        if advance_along_path(guard, FLEE_SPEED, dt) {
            // 抵达安全点：清队列，下一帧按新位置重新评估最远路点
            guard.path.clear();
            guard.path_index = 0;
        }
        BtStatus::Success
    }

    /// 支援（能力②，默认关）：朝警报点推进；计时到期清航点回巡逻。
    fn support(&mut self, bb: &mut Blackboard, dt: f32) -> BtStatus {
        let nav = self.view.nav;
        let guard = &mut *self.guard;
        enter_mode(guard, bb, mode::SUPPORT);
        let remaining = bb.get_f32_or(BB_SUPPORT_TIMER, 0.0) - dt;
        bb.set_f32(BB_SUPPORT_TIMER, remaining);
        if remaining <= 0.0 {
            guard.path.clear();
            guard.path_index = 0;
            guard.anim_speed = 0.0;
            return BtStatus::Success;
        }
        if guard.path.is_empty() {
            let target = bb.get_vec3(BB_ALERT_POS).unwrap_or(Vec3::ZERO);
            let from = Vec3::new(guard.pos.x, 0.0, guard.pos.z);
            let to = Vec3::new(target.x, 0.0, target.z);
            match nav.find_path(from, to) {
                Some(path) => {
                    guard.path = path;
                    guard.path_index = 0;
                }
                None => {
                    guard.anim_speed = 0.0;
                    return BtStatus::Success;
                }
            }
        }
        if advance_along_path(guard, SUPPORT_SPEED, dt) {
            // 抵达警报点：原地搜索（等计时到期，或途中发现玩家转追逐）
            guard.path.clear();
            guard.path_index = 0;
        }
        BtStatus::Success
    }
}

impl BehaviorContext for GuardCtx<'_> {
    fn action(&mut self, id: ActionId, bb: &mut Blackboard, dt: f32) -> BtStatus {
        match id.as_str() {
            action::FREEZE => {
                set_mode(bb, mode::FREEZE);
                self.guard.anim_speed = 0.0;
                BtStatus::Success
            }
            action::SENSE => self.sense(bb, dt),
            action::CHASE => self.chase(bb, dt),
            action::PATROL => self.patrol(bb, dt),
            action::FLEE => self.flee(bb, dt),
            action::SUPPORT => self.support(bb, dt),
            other => {
                debug_assert!(false, "守卫行为树出现了未登记的动作 id：{other}");
                BtStatus::Failure
            }
        }
    }

    fn condition(&mut self, id: ActionId, bb: &Blackboard, _dt: f32) -> bool {
        match id.as_str() {
            action::STUNNED => self.guard.state == GuardState::Stunned,
            action::THREATENED => {
                if !self.guard.capabilities.flee {
                    return false; // 能力缺席 = 旧行为（绝不进入逃跑分支）
                }
                // 粘性：逃跑一旦开始就持续 FLEE_SECS（见常量文档）
                if bb.get_f32_or(BB_FLEE_TIMER, 0.0) > 0.0 {
                    return true;
                }
                let low_health = self.guard.health <= FLEE_HEALTH;
                let approached = bb.get_bool_or(BB_SEES, false)
                    && Vec2::new(
                        self.view.player_pos.x - self.guard.pos.x,
                        self.view.player_pos.z - self.guard.pos.z,
                    )
                    .length()
                        <= FLEE_RADIUS;
                low_health || approached
            }
            action::CHASING => self.guard.state == GuardState::Chase,
            action::SUPPORTING => {
                self.guard.capabilities.reinforcement && bb.get_f32_or(BB_SUPPORT_TIMER, 0.0) > 0.0
            }
            action::PATROLLING => self.guard.state == GuardState::Patrol,
            other => {
                debug_assert!(false, "守卫行为树出现了未登记的条件 id：{other}");
                false
            }
        }
    }
}

/// 导航图参数（构建与重建同参——见 [`NAV_BOUNDS_MIN`] 等常量）。R49
/// 新增 `step_height` / `max_slope_radians`（与角色控制器同值 0.3 / 45°）；
/// 本作全平地世界（一切立面顶离地 > step_height 的孤岛）→ 位图与行为
/// 与 R35 逐位一致。
fn nav_params() -> NavGridParams {
    NavGridParams {
        bounds_min: NAV_BOUNDS_MIN,
        bounds_max: NAV_BOUNDS_MAX,
        cell_size: NAV_CELL_SIZE,
        agent_radius: NAV_AGENT_RADIUS,
        step_height: 0.3,
        max_slope_radians: std::f32::consts::FRAC_PI_4,
    }
}

// ---------------------------------------------------------------------------
// 自动玩家（headless 测试与纯核心单测共用的确定性脚本）
// ---------------------------------------------------------------------------

/// 自动玩家的预设路线（XZ）：出生 → 核心A（NW）→ 压力板 → 核心C（NE，
/// 门开）→ 核心B（SE）→ 出口（SW）。全部路线点避开两条守卫环的中线，
/// 门洞走门洞中心线；点间距 ≥ 1.4 m（到点半径 0.6 不误跳段）。
pub const AUTOPILOT_ROUTE: &[Vec2] = &[
    Vec2::new(-7.0, 0.0),   // 东行走廊
    Vec2::new(-7.0, -3.0),  // 穿 NW 门洞（弹簧门板所在——纯展示不挡路）
    Vec2::new(-11.0, -3.5), // NW 室西南角（避开守卫 0 环）
    Vec2::new(-11.0, -7.0),
    Vec2::new(-10.0, -7.0), // 核心 A
    Vec2::new(-11.0, -7.0),
    Vec2::new(-11.0, -3.5),
    Vec2::new(-7.0, -3.0),
    Vec2::new(-7.0, 0.0),
    Vec2::new(2.0, 0.5), // 压力板（开门）
    Vec2::new(7.0, -0.5),
    Vec2::new(7.0, -3.0), // 穿压力门门洞（已开）
    Vec2::new(11.0, -3.5),
    Vec2::new(11.0, -7.0),
    Vec2::new(10.0, -7.0), // 核心 C
    Vec2::new(11.0, -7.0),
    Vec2::new(11.0, -3.5),
    Vec2::new(7.0, -3.0),
    Vec2::new(7.0, 0.0),
    Vec2::new(7.0, 3.0), // 穿 SE 门洞
    Vec2::new(11.0, 3.5),
    Vec2::new(11.0, 7.0),
    Vec2::new(10.0, 7.0), // 核心 B
    Vec2::new(11.0, 7.0),
    Vec2::new(11.0, 3.5),
    Vec2::new(7.0, 3.0),
    Vec2::new(7.0, 0.0),
    Vec2::new(0.0, 0.5),
    Vec2::new(-7.0, 0.5), // 横穿走廊去 SW
    Vec2::new(-7.0, 3.0), // 穿 SW 门洞
    Vec2::new(-11.0, 3.5),
    Vec2::new(-11.0, 7.0),
    Vec2::new(-10.0, 7.0), // 出口
];

/// 守卫视锥感知（自由函数，守卫 AI 与自动玩家共用同一套感知几何）：
/// 距离（潜行减半）+ 视锥夹角 + 静态体线段遮挡。眼位 = 守卫头顶
/// （y 1.4），目标 = 玩家胸口（y 0.9）——内墙高 2.5 m 恒挡线，门开即透明。
///
/// R65 起几何判定委托给引擎纯函数 [`can_see`]（`perception` 模块）：视锥的
/// 距离/半角运算序与 R35–R41 的手写实现**逐位同源**（同为
/// `Vec2::new(to.x, to.z) / dist` 归一 + 三维距离判界），遮挡复用同一条
/// `segment_hits_shape_rotated`（恒等旋转 = 旧 `segment_hits_shape`）。
/// `statics` 是 [`InfiltrationState::vision_statics`]（恒等旋转快照）。
pub fn guard_sees(
    guard: &Guard,
    player_pos: Vec3,
    sneaking: bool,
    statics: &[StaticObstacle],
) -> bool {
    let eye = Vec3::new(guard.pos.x, 1.4, guard.pos.z);
    let chest = Vec3::new(player_pos.x, 0.9, player_pos.z);
    let max = if sneaking {
        SNEAK_VIEW_DISTANCE
    } else {
        VIEW_DISTANCE
    };
    can_see(eye, chest, guard.forward(), max, FOV_COS, statics)
}

/// 脚本化自动玩家：恒定潜行 + 逐路线点直走 + **视线避让**。规则：
/// - 当前目标 = 路线表 `state.autopilot_route` 下标（到点半径 0.6 内
///   推进游标——游标存 [`InfiltrationState`]，`drive` 是纯函数之外唯一
///   的簿记写入，确定性不受影响：同输入两次模拟的游标轨迹逐位一致）；
/// - 恒潜行：视锥发现半径减半到 4 m；
/// - 视线避让：任意**巡逻中**守卫「当前看得见玩家」→ 本帧朝远离该守卫
///   的方向撤退（潜行速度 2.0 略快于巡逻 1.8，距离不缩小）；守卫背过身
///   （沿路线环继续走）的窗口一旦出现就恢复推进——「潜行跟随」行为由此
///   涌现，走廊等长直区段也可靠（在守卫身后跟进，不在其视锥内停留）。
///   **追逐态不避让**——追逐意味着已被发现，跑路要紧（防追逐死锁）。
#[derive(Clone, Copy, Debug, Default)]
pub struct AutoPilot;

impl AutoPilot {
    /// 由当前状态产出本帧输入（含路线游标簿记推进）。
    pub fn drive(&self, state: &mut InfiltrationState) -> InfiltrationInput {
        const GOAL_REACH: f32 = 0.6;
        if state.phase == Phase::Playing {
            let target = AUTOPILOT_ROUTE[state.autopilot_route.min(AUTOPILOT_ROUTE.len() - 1)];
            let pos = Vec2::new(state.pos.x, state.pos.z);
            if pos.distance(target) <= GOAL_REACH
                && state.autopilot_route + 1 < AUTOPILOT_ROUTE.len()
            {
                state.autopilot_route += 1;
            }
        }
        let target = AUTOPILOT_ROUTE[state.autopilot_route.min(AUTOPILOT_ROUTE.len() - 1)];
        let pos = Vec2::new(state.pos.x, state.pos.z);
        let mut move_dir = (target - pos).normalize_or_zero();
        // 视线避让（见类型文档）：被巡逻守卫看见 → 朝反方向撤退
        for guard in &state.guards {
            if guard.state == GuardState::Patrol
                && guard_sees(guard, state.pos, true, &state.vision_statics)
            {
                let guard_xz = Vec2::new(guard.pos.x, guard.pos.z);
                move_dir = (pos - guard_xz).normalize_or_zero();
                break;
            }
        }
        InfiltrationInput {
            move_dir,
            sneak: true,
            interact: false,
            dt: 1.0 / 60.0,
        }
    }
}

/// 以固定步长推进自动玩家直到 `done` 返回真或帧数封顶，返回推进帧数。
/// headless 测试与纯核心单测共用（同一推进路径，零偏离）。
pub fn run_until(
    state: &mut InfiltrationState,
    done: impl Fn(&InfiltrationState) -> bool,
    max_frames: u32,
) -> u32 {
    let pilot = AutoPilot;
    for frame in 0..max_frames {
        let input = pilot.drive(state);
        state.step(&input);
        if done(state) {
            return frame + 1;
        }
    }
    max_frames
}

// ---------------------------------------------------------------------------
// 单测（纯核心全规则；坐标针对写死关卡设计）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3Swizzles;

    const DT: f32 = 1.0 / 60.0;
    /// 常用容差：60Hz 步长下的浮点积累误差量级。
    const EPS: f32 = 1e-3;

    /// 输入速查（可带潜行 / 互动）。
    fn input(move_dir: Vec2, sneak: bool, interact: bool) -> InfiltrationInput {
        InfiltrationInput {
            move_dir,
            sneak,
            interact,
            dt: DT,
        }
    }

    /// 静止输入（空推）。
    fn rest() -> InfiltrationInput {
        input(Vec2::ZERO, false, false)
    }

    /// 空推 `seconds` 秒。
    fn idle(state: &mut InfiltrationState, seconds: f32) {
        let steps = (seconds / DT).round() as u32;
        for _ in 0..steps {
            state.step(&rest());
        }
    }

    /// 点（XZ）到 AABB 的 XZ 最近点距离（与 `iris_core::nav` 的投影
    /// 判定同式——本模块自持一份供「守卫不进墙」断言）。
    fn xz_clearance(point: Vec2, center: Vec3, half: Vec3) -> f32 {
        let ox = (point.x - center.x).abs() - half.x;
        let oz = (point.y - center.z).abs() - half.z;
        (ox.max(0.0) * ox.max(0.0) + oz.max(0.0) * oz.max(0.0)).sqrt()
    }

    // ---- 关卡数据不变量 ----

    #[test]
    fn level_invariants_hold() {
        // 压力门恰好封住北排东门洞（x 6..8、z=-2），与相邻墙段严丝合缝
        assert_eq!(DOOR.center, Vec3::new(7.0, 1.25, -2.0));
        assert_eq!(DOOR.half, Vec3::new(1.0, 1.25, 0.2));
        // 两个横墙门洞中心恰好 (-7, ∓2) 与 (7, ±2)，净宽 2 m
        for (gap_x, gap_z) in [(-7.0, -2.0), (7.0, -2.0), (-7.0, 2.0), (7.0, 2.0)] {
            let clearance = WALLS
                .iter()
                .map(|w| xz_clearance(Vec2::new(gap_x, gap_z), w.center, w.half))
                .fold(f32::INFINITY, f32::min);
            assert!(
                clearance >= 1.0 - EPS,
                "门洞中心 ({gap_x},{gap_z}) 到墙净距应 ≈1.0（2 m 门洞半宽），实际 {clearance}"
            );
        }
        // 核心与出口、出生点都不在任何墙的膨胀域内（守卫 / 玩家可达）
        for core in CORES.iter() {
            for wall in WALLS.iter() {
                assert!(
                    xz_clearance(*core, wall.center, wall.half) > NAV_AGENT_RADIUS,
                    "核心 {core:?} 不应落在墙 {wall:?} 的导航膨胀域内"
                );
            }
        }
        // 出生点可站立（远离墙）
        for wall in WALLS.iter() {
            assert!(
                xz_clearance(
                    Vec2::new(SPAWN_FOOT.x, SPAWN_FOOT.z),
                    wall.center,
                    wall.half
                ) > 1.0,
                "出生点应离墙至少 1 m"
            );
        }
        // 压力板与出口域、核心互不重叠
        let plate = Vec3::new(PLATE_POS.x, HALF_HEIGHT, PLATE_POS.y);
        assert!(
            !point_in_aabb_xz(Vec2::new(plate.x, plate.z), EXIT_CENTER, EXIT_HALF),
            "压力板不应在出口域内"
        );
    }

    #[test]
    fn nav_reachability_matches_door_semantics() {
        let state = InfiltrationState::new();
        let at = |v: Vec2| Vec3::new(v.x, 0.0, v.y);
        // 门关：NW / SE / 出口 / 守卫路点可达；NE（核心 C）被压力门封死
        assert!(state.nav.is_reachable(at(SPAWN_FOOT.xz()), at(CORES[0])));
        assert!(state.nav.is_reachable(at(SPAWN_FOOT.xz()), at(CORES[2])));
        assert!(state.nav.is_reachable(
            at(SPAWN_FOOT.xz()),
            Vec3::new(EXIT_CENTER.x, 0.0, EXIT_CENTER.z)
        ));
        assert!(
            !state.nav.is_reachable(at(SPAWN_FOOT.xz()), at(CORES[1])),
            "压力门关闭时 NE 室（核心 C）应不可达"
        );
        for route in GUARD_ROUTES.iter() {
            for wp in route.iter() {
                assert!(
                    state.nav.is_reachable(at(route[0]), at(*wp)),
                    "巡逻路点 {wp:?} 应与其路线首点连通"
                );
            }
        }
        // 门开（便宜重建）：NE 可达——守卫可穿门追逐
        let mut state = InfiltrationState::new();
        state.press_plate();
        assert!(state.door_open);
        assert!(
            state.nav.is_reachable(at(SPAWN_FOOT.xz()), at(CORES[1])),
            "压力门打开并重建导航图后 NE 室应可达"
        );
        // 开门后静态体不再含门
        assert!(
            state.statics.iter().all(|&(pos, _)| pos != DOOR.center),
            "开门后逻辑静态体应不含压力门"
        );
    }

    // ---- 玩家移动（引擎角色控制器 / 贴墙滑行）----

    #[test]
    fn player_walks_and_slides_along_walls() {
        let mut state = InfiltrationState::new();
        // 向东走 2 秒应到走廊中部（x ≈ -10.5 + 2×4.2 = -2.1 附近，贴墙
        // 无阻挡时直线）
        for _ in 0..120 {
            state.step(&input(Vec2::new(1.0, 0.0), false, false));
        }
        assert!(
            (state.pos.x - (-2.1)).abs() < 0.3,
            "2 秒东行应走约 8.4 m，实际 x = {}",
            state.pos.x
        );
        // 向北顶 z=-2 内墙：墙内缘 z=-1.8，胶囊从 +Z 侧接近 → 中心停在
        // -1.8 + 半径 0.4 = -1.4
        for _ in 0..120 {
            state.step(&input(Vec2::new(0.0, -1.0), false, false));
        }
        assert!(
            (state.pos.z - (-1.4)).abs() < 0.05,
            "北行应停在 z=-2 内墙前（墙内缘 -1.8 + 半径 0.4），实际 z = {}",
            state.pos.z
        );
        // 潜行速度恰为正常的一半
        let mut state = InfiltrationState::new();
        for _ in 0..60 {
            state.step(&input(Vec2::new(1.0, 0.0), true, false));
        }
        assert!(
            (state.vel.x - SNEAK_SPEED).abs() < EPS,
            "潜行速度应 = {SNEAK_SPEED}，实际 {}",
            state.vel.x
        );
        assert!(state.sneaking, "潜行标志应随输入置位");
    }

    // ---- 数据核心 ----

    #[test]
    fn cores_are_picked_once_with_event() {
        let mut state = InfiltrationState::new();
        // 传送到核心 A 旁（字段公开，构造中间态）
        state.pos = Vec3::new(CORES[0].x + 0.5, HALF_HEIGHT, CORES[0].y);
        state.step(&rest());
        assert!(state.cores[0], "贴近的核心应被拾取");
        assert_eq!(state.cores_collected(), 1);
        assert!(
            matches!(
                state.frame_events.first(),
                Some(InfiltrationEvent::CorePicked { index: 0, .. })
            ),
            "拾取帧应有 CorePicked 事件"
        );
        // 原地再站一帧：不重复拾取、不重复事件
        state.step(&rest());
        assert_eq!(state.cores_collected(), 1);
        assert!(
            !state
                .frame_events
                .iter()
                .any(|e| matches!(e, InfiltrationEvent::CorePicked { .. })),
            "已拾取核心不应重复产生事件"
        );
    }

    // ---- 守卫视锥（距离 / 夹角 / 遮挡 / 潜行减半）----

    #[test]
    fn guard_vision_respects_cone_distance_occlusion_and_sneak() {
        let mut state = InfiltrationState::new();
        // 场景 A：守卫 0 置于走廊原点、面朝 -Z；玩家摆在其正前方 1.5 m
        state.guards[0].pos = Vec3::new(0.0, HALF_HEIGHT, 0.0);
        state.guards[0].facing = 0.0; // 前方 = -Z
        state.pos = Vec3::new(0.0, HALF_HEIGHT, -1.5);
        state.sneaking = false;
        assert!(state.guard_sees_player(0), "1.5 m 正前应可见");
        state.sneaking = true;
        assert!(state.guard_sees_player(0), "1.5 m 潜行仍应可见");
        // 场景 B：守卫面朝 +X（facing = -π/2），玩家在 6 m 正前——潜行
        // （> 4 m 减半视距）不可见、正常移动（< 8 m）可见
        state.guards[0].pos = Vec3::new(-10.5, HALF_HEIGHT, 0.0);
        state.guards[0].facing = -std::f32::consts::FRAC_PI_2;
        state.pos = Vec3::new(-4.5, HALF_HEIGHT, 0.0);
        state.sneaking = true;
        assert!(!state.guard_sees_player(0), "6 m 潜行应超出减半视距");
        state.sneaking = false;
        assert!(state.guard_sees_player(0), "6 m 正常移动应可见");
        // 场景 C：夹角——守卫 (−4.5,0) 面朝 +X，玩家在侧后方（与前方
        // 夹角 135° > 55°）不可见
        state.guards[0].pos = Vec3::new(-4.5, HALF_HEIGHT, 0.0);
        state.pos = Vec3::new(-5.5, HALF_HEIGHT, 1.0);
        assert!(!state.guard_sees_player(0), "视锥（半角 55°）外不可见");
        // 场景 D：遮挡——守卫 (0,0) 面朝 -Z、玩家 NW 室 (-4,-4)：距离
        // 5.7 m、夹角 45° 都满足，但视线穿过 z=-2 内墙（在 x=-2 处相交）
        state.guards[0].pos = Vec3::new(0.0, HALF_HEIGHT, 0.0);
        state.guards[0].facing = 0.0;
        state.pos = Vec3::new(-4.0, HALF_HEIGHT, -4.0);
        state.sneaking = false;
        assert!(
            !state.guard_sees_player(0),
            "内墙遮挡下应不可见（哪怕距离与夹角都满足）"
        );
    }

    // ---- 警觉累积与衰减 / 发现计数 / 告负 ----

    #[test]
    fn alert_accumulates_decays_and_three_detections_lose() {
        let mut state = InfiltrationState::new();
        // 构造「守卫 0 看见潜行玩家」的持续场景：1.5 m 正前（走廊内，
        // 与 z=-2 内墙保持距离）
        state.guards[0].pos = Vec3::new(0.0, HALF_HEIGHT, 0.0);
        state.guards[0].facing = 0.0;
        state.pos = Vec3::new(0.0, HALF_HEIGHT, -1.5);
        state.sneaking = true;
        // 潜行视距 4 m：可见 → 警觉上升；1/1.2 s ≈ 50 帧后满格发现。
        // 每帧把守卫钉回原位（Patrol 态会自己走掉、转身就看不见了）
        let mut detected_frame = None;
        for frame in 0..120u32 {
            state.guards[0].pos = Vec3::new(0.0, HALF_HEIGHT, 0.0);
            state.guards[0].facing = 0.0;
            state.pos = Vec3::new(0.0, HALF_HEIGHT, -1.5);
            state.step(&rest());
            if state
                .frame_events
                .iter()
                .any(|e| matches!(e, InfiltrationEvent::Detection { count: 1, .. }))
            {
                detected_frame = Some(frame);
                break;
            }
        }
        let frame = detected_frame.expect("持续可见应在 2 秒内触发第一次发现");
        assert!(
            (frame as f32 * DT - 1.0 / ALERT_RISE).abs() < 0.1,
            "发现时刻应 ≈ 1/ALERT_RISE = {:.2} s，实际 {:.2} s",
            1.0 / ALERT_RISE,
            frame as f32 * DT
        );
        assert_eq!(state.guards[0].state, GuardState::Chase, "发现后应转追逐");
        assert_eq!(state.detections, 1);
        // 玩家躲到守卫看不到的地方（NW 室，视线被 z=-2 内墙挡住）：
        // 记忆 5 s 后应回巡逻、警觉衰减
        state.pos = Vec3::new(-4.0, HALF_HEIGHT, -4.0);
        idle(&mut state, CHASE_MEMORY + 1.0);
        assert_eq!(
            state.guards[0].state,
            GuardState::Patrol,
            "记忆耗尽应回巡逻"
        );
        assert!(state.guards[0].alert < 0.1, "脱离视锥后警觉应衰减");
        // 连续两次再发现 → 告负（终局冻结）。回到 1.5 m 正前并重置
        // 追逐态计时（字段公开，构造中间态）
        for _ in 0..MAX_DETECTIONS * 400 {
            state.guards[0].pos = Vec3::new(0.0, HALF_HEIGHT, 0.0);
            state.guards[0].facing = 0.0;
            state.pos = Vec3::new(0.0, HALF_HEIGHT, -1.5);
            state.step(&rest());
            if state.phase == Phase::Defeat {
                break;
            }
        }
        assert_eq!(state.phase, Phase::Defeat, "三次发现应告负");
        assert_eq!(state.detections, MAX_DETECTIONS);
        let snapshot = (state.pos, state.elapsed);
        state.step(&rest());
        assert_eq!((state.pos, state.elapsed), snapshot, "告负后应冻结");
    }

    // ---- 守卫巡逻：NavGrid 折点绕墙 + 永不进墙 ----

    #[test]
    fn guard_patrol_navigates_around_walls() {
        let mut state = InfiltrationState::new();
        let mut waypoints_visited = [0usize; 2];
        let mut last_route_next = [0usize, 1];
        for _ in 0..60 * 90 {
            state.step(&rest());
            for (index, guard) in state.guards.iter().enumerate() {
                // 永不进墙：到任一墙的 XZ 净距 > 0.3（导航 agent 半径以下）
                for wall in WALLS.iter() {
                    let clearance =
                        xz_clearance(Vec2::new(guard.pos.x, guard.pos.z), wall.center, wall.half);
                    assert!(
                        clearance > 0.3,
                        "守卫 {index} 位置 {:?} 不应进入墙 {wall:?} 的 0.3 m 域",
                        guard.pos
                    );
                }
                // 路线环推进计数（折点断言的代理：route_next 循环即说明
                // 每段 find_path 都真的抵达了路点——抵达由 guard_advance
                // 的到点半径保证，绕墙由 NavGrid 的防穿角保证）
                if guard.route_next != last_route_next[index] {
                    last_route_next[index] = guard.route_next;
                    waypoints_visited[index] += 1;
                }
            }
            if state.detections > 0 {
                // 站在原地的玩家若被发现属正常（本测试关注巡逻绕墙与
                // 折点，不是隐身）——重开一局防追逐干扰断言
                state = InfiltrationState::new();
                last_route_next = [0, 1];
            }
        }
        assert!(
            waypoints_visited[0] >= 2 && waypoints_visited[1] >= 2,
            "90 秒内两条巡逻环都应至少推进 2 个路点，实际 {waypoints_visited:?}"
        );
    }

    // ---- 压力板开门 ----

    #[test]
    fn plate_opens_door_and_player_passes_through() {
        let mut state = InfiltrationState::new();
        // 开门前：玩家从走廊 (7,0) 北行顶门被挡（门内缘 z=-1.8 + 半径 0.4）
        state.pos = Vec3::new(7.0, HALF_HEIGHT, 0.0);
        for _ in 0..90 {
            state.step(&input(Vec2::new(0.0, -1.0), false, false));
        }
        assert!(
            (state.pos.z - (-1.4)).abs() < 0.05,
            "关门时北行应被压力门挡在 z≈-1.4，实际 {}",
            state.pos.z
        );
        // 踩压力板（直接调用入口——距离判定路径在自动玩家测试里覆盖）：
        // 开门 + 双事件 + 导航重建
        let base = state.frame_events.len();
        state.press_plate();
        assert!(state.door_open);
        assert!(
            matches!(
                state.frame_events[base],
                InfiltrationEvent::PlatePressed { .. }
            ) && matches!(
                state.frame_events[base + 1],
                InfiltrationEvent::DoorOpened { .. }
            ),
            "开板帧应依次产生 PlatePressed 与 DoorOpened 事件"
        );
        // 开门后：同一走廊位置北行应穿过门洞进 NE 室（z < -4）
        state.pos = Vec3::new(7.0, HALF_HEIGHT, 0.0);
        state.vel = Vec3::ZERO;
        for _ in 0..150 {
            state.step(&input(Vec2::new(0.0, -1.0), false, false));
        }
        assert!(
            state.pos.z < -4.0,
            "开门后应能穿过门洞（实际 z = {}）",
            state.pos.z
        );
        // 重复踩板：幂等（不重复开门事件）
        state.press_plate();
        assert!(
            !state
                .frame_events
                .iter()
                .any(|e| matches!(e, InfiltrationEvent::DoorOpened { .. })),
            "已开之门不应重复产生 DoorOpened"
        );
        // 距离判定路径：玩家走到压力板上 → step 内自动开门（另起一局）
        let mut state = InfiltrationState::new();
        state.pos = Vec3::new(PLATE_POS.x, HALF_HEIGHT, PLATE_POS.y);
        state.step(&rest());
        assert!(state.door_open, "踩上压力板应开门（距离判定路径）");
        assert!(
            matches!(
                state.frame_events.first(),
                Some(InfiltrationEvent::PlatePressed { .. })
            ),
            "踩板帧应有 PlatePressed 事件"
        );
    }

    // ---- 击晕（背后限定 + v1 不醒来）----

    #[test]
    fn stun_requires_behind_and_is_permanent_in_v1() {
        let mut state = InfiltrationState::new();
        state.guards[0].pos = Vec3::new(0.0, HALF_HEIGHT, 0.0);
        state.guards[0].facing = 0.0; // 面朝 -Z
        // 正面 1.5 m：interact 不生效（不在背面弧内）
        state.pos = Vec3::new(0.0, HALF_HEIGHT, -1.5);
        state.step(&input(Vec2::ZERO, false, true));
        assert_ne!(state.guards[0].state, GuardState::Stunned, "正面不应击晕");
        // 背面 1.5 m：击晕 + 事件
        state.pos = Vec3::new(0.0, HALF_HEIGHT, 1.5);
        state.step(&input(Vec2::ZERO, false, true));
        assert_eq!(state.guards[0].state, GuardState::Stunned);
        assert!(
            matches!(
                state.frame_events.last(),
                Some(InfiltrationEvent::GuardStunned { index: 0, .. })
            ),
            "击晕帧应有 GuardStunned 事件"
        );
        // v1 不醒来：空推 10 秒仍是 Stunned（且警觉冻结为 0）
        idle(&mut state, 10.0);
        assert_eq!(
            state.guards[0].state,
            GuardState::Stunned,
            "v1 击晕不应醒来"
        );
        assert_eq!(state.guards[0].alert, 0.0);
        // 超距（2.5 m > 2.2 m）背面：不击晕（守卫 1 验证）
        state.guards[1].pos = Vec3::new(0.0, HALF_HEIGHT, 0.0);
        state.guards[1].facing = 0.0;
        state.pos = Vec3::new(0.0, HALF_HEIGHT, 2.5);
        state.step(&input(Vec2::ZERO, false, true));
        assert_ne!(state.guards[1].state, GuardState::Stunned, "超距不应击晕");
    }

    // ---- 胜利 ----

    #[test]
    fn exit_with_all_cores_triggers_victory_and_freezes() {
        let mut state = InfiltrationState::new();
        // 直接送进出口域但核心未满：不胜利
        state.pos = Vec3::new(EXIT_CENTER.x, HALF_HEIGHT, EXIT_CENTER.z);
        state.step(&rest());
        assert_eq!(state.phase, Phase::Playing, "核心未全收不应胜利");
        // 全核心 + 进域 → 胜利 + 冻结
        state.cores = vec![true; CORES.len()];
        state.step(&rest());
        assert_eq!(state.phase, Phase::Victory);
        assert!(
            matches!(
                state.frame_events.last(),
                Some(InfiltrationEvent::Victory { .. })
            ),
            "进入出口域应产生 Victory 事件"
        );
        let snapshot = (state.pos, state.elapsed);
        state.step(&input(Vec2::new(1.0, 0.0), false, true));
        assert_eq!((state.pos, state.elapsed), snapshot, "终局应冻结");
    }

    // ---- 全流程自动玩家（核心余量：被发现 ≤ 1）----

    #[test]
    fn auto_pilot_full_runthrough_victory_within_detection_budget() {
        let mut state = InfiltrationState::new();
        let frames = run_until(&mut state, |s| s.phase != Phase::Playing, 60 * 180);
        assert_eq!(state.phase, Phase::Victory, "3 分钟内自动玩家应通关");
        assert_eq!(state.cores_collected(), CORES.len(), "应收集全部核心");
        assert!(
            state.detections <= 1,
            "脚本设计余量：被发现应 ≤ 1 次，实际 {}",
            state.detections
        );
        assert!(state.door_open, "通关路线必经压力板开门");
        assert!(
            frames < 60 * 150,
            "通关用时 {frames} 帧应 < 150 s（含避让等待）"
        );
        println!(
            "自动玩家通关：{frames} 帧（{:.1} s 游戏时间），核心 {}/{}，被发现 {}，门已开 {}",
            frames as f32 / 60.0,
            state.cores_collected(),
            CORES.len(),
            state.detections,
            state.door_open
        );
    }

    // ---- 确定性（逐位）----

    #[test]
    fn same_input_two_runs_are_bit_identical() {
        /// 逐帧采样行：玩家位置 / 速度 bits、朝向、守卫摘要、进度、事件数。
        type Sample = (
            [u32; 3],
            [u32; 3],
            u32,
            Vec<([u32; 3], u32, u64, u32)>,
            usize,
            usize,
            u32,
            u32,
            usize,
        );

        fn trace() -> Vec<Sample> {
            let mut state = InfiltrationState::new();
            let mut samples = Vec::new();
            for _ in 0..60 * 180 {
                let input = AutoPilot.drive(&mut state);
                state.step(&input);
                let guards = state
                    .guards
                    .iter()
                    .map(|g| {
                        (
                            [g.pos.x.to_bits(), g.pos.y.to_bits(), g.pos.z.to_bits()],
                            g.facing.to_bits(),
                            g.alert.to_bits() as u64,
                            g.state as u32,
                        )
                    })
                    .collect();
                samples.push((
                    [
                        state.pos.x.to_bits(),
                        state.pos.y.to_bits(),
                        state.pos.z.to_bits(),
                    ],
                    [
                        state.vel.x.to_bits(),
                        state.vel.y.to_bits(),
                        state.vel.z.to_bits(),
                    ],
                    state.facing.to_bits(),
                    guards,
                    state.cores_collected(),
                    state.autopilot_route,
                    state.detections,
                    state.phase as u32,
                    state.frame_events.len(),
                ));
                if state.phase != Phase::Playing {
                    break;
                }
            }
            samples
        }
        let a = trace();
        let b = trace();
        assert_eq!(a.len(), b.len(), "两次模拟帧数应一致");
        assert!(a.len() > 60 * 10, "采样应覆盖一段真实推进（非空转）");
        for (frame, (sa, sb)) in a.iter().zip(&b).enumerate() {
            assert_eq!(sa, sb, "同输入两遍应逐采样 bit 级一致，帧 #{frame}");
        }
        let last = a.last().expect("非空");
        assert_eq!(last.7, Phase::Victory as u32, "脚本应打到胜利终局");
    }
}
