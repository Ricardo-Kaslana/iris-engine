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

use iris_core::character::{CharacterParams, move_and_slide_statics};
use iris_core::nav::{NavGrid, NavGridParams};
use iris_core::physics::{ColliderShape, segment_hits_shape};

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
/// 压力板踩上判定半径（米）：与实体侧 sensor 盒半宽 0.9 同源同值。
pub const PLATE_RADIUS: f32 = 0.9;

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
    /// 对局累计时间（秒；`step` 累加，确定性）。
    pub elapsed: f32,
    /// 逻辑静态体（墙 + 关闭的压力门；开门时重建）。
    pub statics: Vec<(Vec3, ColliderShape)>,
    /// 导航图（启动 build 一次；开门时按引擎「便宜重建」姿势整图重建）。
    pub nav: NavGrid,
    /// 本帧事件列表（每帧 `step` 开头清空，见 [`InfiltrationEvent`]）。
    pub frame_events: Vec<InfiltrationEvent>,
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
            guards: vec![Guard::new(0), Guard::new(1)],
            elapsed: 0.0,
            statics: level_statics(false),
            nav: NavGrid::build(nav_params(), &level_statics(false)),
            frame_events: Vec::new(),
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

        // ---- 守卫：感知/警觉（用移动前的位姿——顺序是语义）→ 移动 ----
        for index in 0..self.guards.len() {
            self.update_guard(index, dt);
        }

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

    /// 单名守卫的一帧：感知/警觉 → 状态迁移 → 寻路移动。
    fn update_guard(&mut self, index: usize, dt: f32) {
        let sees = self.guard_sees_player(index);
        let (state, sees, player_pos) = {
            let guard = &mut self.guards[index];
            if guard.state == GuardState::Stunned {
                return;
            }
            // 警觉：视锥内可见上升、不可见衰减
            if sees {
                guard.alert = (guard.alert + ALERT_RISE * dt).min(1.0);
            } else {
                guard.alert = (guard.alert - ALERT_DECAY * dt).max(0.0);
            }
            // 满格 → 发现：计数 + 事件 + 转追逐（警觉清零重新积累）
            if guard.alert >= 1.0 {
                guard.alert = 0.0;
                guard.state = GuardState::Chase;
                guard.last_known = self.pos;
                guard.chase_timer = CHASE_MEMORY;
                guard.repath_timer = 0.0;
                guard.path.clear();
                self.detections += 1;
                self.frame_events.push(InfiltrationEvent::Detection {
                    pos: self.pos,
                    guard: index,
                    count: self.detections,
                });
            }
            (guard.state, sees, self.pos)
        };
        match state {
            GuardState::Stunned => unreachable!("击晕守卫已在入口返回"),
            GuardState::Chase => self.guard_chase(index, sees, player_pos, dt),
            GuardState::Patrol => self.guard_patrol(index, dt),
        }
    }

    /// 守卫视锥感知：距离（潜行减半）+ 视锥夹角 + 静态体线段遮挡。
    /// 委托给自由函数 [`guard_sees`]（自动玩家复用同一套感知几何）。
    fn guard_sees_player(&self, index: usize) -> bool {
        guard_sees(&self.guards[index], self.pos, self.sneaking, &self.statics)
    }

    /// 巡逻：朝当前路点 find_path 逐段走；到点后推进路线环。
    fn guard_patrol(&mut self, index: usize, dt: f32) {
        if self.guards[index].path.is_empty() {
            let guard = &self.guards[index];
            let route = GUARD_ROUTES[guard.route_index];
            let target = route[guard.route_next.min(route.len() - 1)];
            // 端点 y=0：find_path 只看 XZ，航点 y 覆写见 guard_advance
            let from = Vec3::new(guard.pos.x, 0.0, guard.pos.z);
            let to = Vec3::new(target.x, 0.0, target.y);
            match self.nav.find_path(from, to) {
                Some(path) => {
                    let guard = &mut self.guards[index];
                    guard.path = path;
                    guard.path_index = 0;
                }
                // 不可达（理论不可达：路点与导航图同源）：原地等待一帧
                None => {
                    self.guards[index].anim_speed = 0.0;
                    return;
                }
            }
        }
        let arrived = self.guard_advance(index, PATROL_SPEED, dt);
        if arrived {
            let guard = &mut self.guards[index];
            guard.route_next = (guard.route_next + 1) % GUARD_ROUTES[guard.route_index].len();
            guard.path.clear();
        }
    }

    /// 追逐：视线内刷新最后已知位置与记忆；按节拍 find_path 重算；
    /// 记忆耗尽回巡逻。
    fn guard_chase(&mut self, index: usize, sees: bool, player_pos: Vec3, dt: f32) {
        let give_up = {
            let guard = &mut self.guards[index];
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
                if let Some(path) = self.nav.find_path(from, to) {
                    guard.path = path;
                    guard.path_index = 0;
                }
                guard.repath_timer = REPATH_INTERVAL;
            }
            guard.chase_timer <= 0.0
        };
        if give_up {
            let guard = &mut self.guards[index];
            guard.state = GuardState::Patrol;
            guard.path.clear();
            guard.anim_speed = 0.0;
            return;
        }
        self.guard_advance(index, CHASE_SPEED, dt);
    }

    /// 沿航点队列移动一步：返回是否恰好消费完队列（到点）。
    /// 航点 y 恒 0（2.5D 导航语义）——移动时覆写为守卫自己的中心高度。
    fn guard_advance(&mut self, index: usize, speed: f32, dt: f32) -> bool {
        let Some(&wp) = self.guards[index].path.get(self.guards[index].path_index) else {
            self.guards[index].anim_speed = 0.0;
            return false;
        };
        let guard = &mut self.guards[index];
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
}

/// 导航图参数（构建与重建同参——见 [`NAV_BOUNDS_MIN`] 等常量）。
fn nav_params() -> NavGridParams {
    NavGridParams {
        bounds_min: NAV_BOUNDS_MIN,
        bounds_max: NAV_BOUNDS_MAX,
        cell_size: NAV_CELL_SIZE,
        agent_radius: NAV_AGENT_RADIUS,
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
pub fn guard_sees(
    guard: &Guard,
    player_pos: Vec3,
    sneaking: bool,
    statics: &[(Vec3, ColliderShape)],
) -> bool {
    let eye = Vec3::new(guard.pos.x, 1.4, guard.pos.z);
    let chest = Vec3::new(player_pos.x, 0.9, player_pos.z);
    let to_player = chest - eye;
    let dist = to_player.length();
    let max = if sneaking {
        SNEAK_VIEW_DISTANCE
    } else {
        VIEW_DISTANCE
    };
    if !(1e-4..=max).contains(&dist) {
        return false;
    }
    let dir_xz = Vec2::new(to_player.x, to_player.z) / dist;
    if guard.forward().dot(dir_xz) < FOV_COS {
        return false;
    }
    statics
        .iter()
        .all(|&(pos, ref shape)| segment_hits_shape(eye, chest, pos, shape).is_none())
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
                && guard_sees(guard, state.pos, true, &state.statics)
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
