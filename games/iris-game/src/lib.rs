//! # iris-game
//!
//! Iris 引擎下游示范游戏：**第三人称限时收集**（10 枚金币、60 秒倒计时、
//! 竞技场边界、跳跃拾取、HUD 与音效）。本 crate 是 dogfooding 验证——
//! 只使用引擎公开 API（`iris-engine` 门面 + glam），不修改引擎本体。
//!
//! 结构：
//! - [`logic`]：纯逻辑状态机（只依赖 glam、零引擎依赖、全部可单测）；
//! - [`plugin`]：[`GamePlugin`]——场景文件载入关卡（`assets/
//!   collect_arena.scene.json`，墙体 / 障碍柱带静态 Collider）、输入/时间 →
//!   逻辑 → **运动学角色控制器**（静态推离查询，贴墙滑行）→ 实体/HUD/
//!   音效的每帧接线，系统编排可经
//!   [`GamePlugin::build_schedule`](plugin::GamePlugin::build_schedule)
//!   供 headless 测试与离屏快照复用。
//!
//! ```ignore
//! use iris_engine::prelude::*;
//! use iris_game::GamePlugin;
//!
//! App::new()
//!     .add_plugins(DefaultPlugins::new("Iris Collect"))
//!     .add_plugin(GamePlugin::default())   // 或 GamePlugin::new("关卡.json")
//!     .run();
//! ```
//!
//! 操作：WASD 世界系移动，Space 跳跃；60 秒内收齐 10 枚金币获胜。

pub mod logic;
pub mod plugin;

pub use logic::{
    GRAVITY, GROUND_Y, GameState, JUMP_SPEED, MOVE_SPEED, MoveInput, PICKUP_MAX_Y, PICKUP_RADIUS,
    Phase, PickupState, PlayerState,
};
pub use plugin::{
    BANNER_TEXT_NAME, Coin, DEFAULT_SCENE_PATH, DURATION_SECS, GameAssets, GameEntities,
    GamePlugin, SCORE_TEXT_NAME, TIME_TEXT_NAME,
};
