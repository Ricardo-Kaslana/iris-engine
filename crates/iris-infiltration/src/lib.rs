//! # iris-infiltration
//!
//! Iris 引擎下游示范游戏 V：**Iris Infiltration 第三人称潜入**（dogfooding
//! V）。俯视偏斜 45° 第三人称视角的单层设施潜入：玩家从走廊西端潜入，
//! 绕过巡逻守卫拿取 3 个数据核心，抵达出口获胜；被发现 3 次告负。
//!
//! 相比前四作，本作把 R34–R41 的新能力当载荷压榨：
//! - **NavGrid 寻路（R35）**：守卫巡逻 / 追逐全部走 [`NavGrid::find_path`]
//!   航点（硬编码航点敌人在本作只有「路线环目标点」，逐段走位交给导航
//!   图）；门被压力板打开时按引擎文档推荐的「便宜重建」姿势整图重建；
//! - **动画 v3（R38）**：玩家与守卫共用双剪辑混合空间（参数 = 移动速度
//!   0..1，idle ↔ walk 连续混合）+ `FootIk` 两骨足部 IK（双腿链脚踝贴地）；
//! - **ragdoll（R37）**：守卫被从背后击晕 → `spawn_ragdoll_kinematic` +
//!   `release_ragdoll` 瘫倒（v1 击晕不醒来，诚实限制）；
//! - **接触事件（R34）**：压力板 sensor + `ContactTracker` 差分事件
//!   （踩上开门，begin/end 成对计数）；
//! - **RTT（R40）**：监控屏玩法——固定 RT 相机看核心房，`UiImageRtt`
//!   画中画显示在 HUD；
//! - **UI 菜单栈（R41）**：Esc 暂停菜单（`UiMenuStack` push/pop + 焦点
//!   导航 + `UiClickEvent` 消费）+ 主 HUD；
//! - **距离关节（R36）**：设施门洞里的「弹簧门」——`DistanceJoint` 把
//!   门板拉回关闭位（玩家物理代理推开自动回弹，纯展示语义）。
//!
//! 结构（iris-game / iris-arena / iris-bastion / iris-ascent 同款分层）：
//! - [`logic`]：纯逻辑状态机（关卡写死、事件自带世界坐标、逐位确定性；
//!   含脚本化自动玩家 [`AutoPilot`]）；
//! - [`plugin`]：[`InfiltrationPlugin`]——三层玩家结构 + 混合空间动画 +
//!   足部 IK + 守卫 AI 表现 + ragdoll + 接触事件消费 + RTT + HUD / 菜单 +
//!   空间音频；系统编排经
//!   [`InfiltrationPlugin::build_schedule`](plugin::InfiltrationPlugin::build_schedule)
//!   暴露，供 headless 测试与离屏快照复用。
//!
//! ```ignore
//! use iris_engine::prelude::*;
//! use iris_infiltration::InfiltrationPlugin;
//!
//! App::new()
//!     .add_plugins(DefaultPlugins::new("Iris Infiltration"))
//!     .add_plugin(InfiltrationPlugin::default())
//!     .run();
//! ```
//!
//! 操作：WASD 移动（相机固定俯角，屏幕系 = 世界系）、Shift 潜行、
//! E 背后击晕、Esc 暂停菜单。
//!
//! # 本轮纪律
//!
//! 游戏轮不改引擎（dogfooding 纪律）：接入中发现的引擎摩擦全部记入
//! `ARCHITECTURE.md` §8 摩擦清单，绕行写在本 crate 内。

pub mod logic;
pub mod plugin;

pub use logic::{
    ALERT_DECAY, ALERT_RISE, AUTOPILOT_ROUTE, AutoPilot, CHARACTER, CHASE_MEMORY, CHASE_SPEED,
    CORE_RADIUS, CORES, DOOR, EXIT_CENTER, EXIT_HALF, FOV_COS, GUARD_ROUTES, Guard, GuardState,
    HALF_HEIGHT, InfiltrationEvent, InfiltrationInput, InfiltrationState, MAX_DETECTIONS,
    MOVE_SPEED, NAV_AGENT_RADIUS, NAV_BOUNDS_MAX, NAV_BOUNDS_MIN, NAV_CELL_SIZE, PATROL_SPEED,
    PLATE_POS, Phase, REPATH_INTERVAL, SNEAK_SPEED, SNEAK_VIEW_DISTANCE, SPAWN_FOOT,
    STUN_BEHIND_COS, STUN_RADIUS, VIEW_DISTANCE, WALLS, Wall, run_until,
};
pub use plugin::{
    ALERT_TEXT_NAME, BANNER_TEXT_NAME, BannerTimer, CORES_TEXT_NAME, InfiltrationAssets,
    InfiltrationEntities, InfiltrationPlugin, MODEL_PATH, MenuCommand, PAUSE_MENU, PROXY_NAME,
    REFERENCE_DISTANCE, SPOTTED_TEXT_NAME,
};
