//! # iris-arena
//!
//! Iris 引擎下游示范游戏 II：**Iris Arena 波次生存竞技场**（dogfooding
//! II）。俯视 3/4 视角：WASD 移动、Shift 冲刺（短程高速 + 无敌帧）、
//! 自动开火（朝 14 m 内最近敌人发射弹道）、蒙皮蠕虫敌人成波追击
//! （第 n 波 3 + 2n 只、速度逐波 +0.15）、接触掉血 + 连击清零、
//! 击杀涨连击（得分 = 25 × 连击数）、HP 5 点归零即 GAME OVER。
//!
//! 相比 Iris Collect（`iris-game`），本作把引擎压到更高强度的真实用法：
//! - **蒙皮动画 × 对象池**：15 个蒙皮蠕虫实例（`spawn_gltf_instance` +
//!   `AnimationPlayer` 自动循环）与 40 个弹道小球在启动时一次性生成、
//!   全程 `Visibility::HIDDEN` 待命，波次间取用 / 归还，零运行期 spawn；
//! - **弹道投影体 + 追击 AI**：确定性纯逻辑核心驱动，敌人以稳定 id 映射
//!   池槽位；
//! - **多游戏状态**：Title / Playing / GameOver 三态状态机 + 波次间歇。
//!
//! 结构：
//! - [`logic`]：纯逻辑状态机（只依赖 glam、零引擎依赖、全部可单测；
//!   含本地 XZ 平面静态推离，与引擎物理查询语义近似但刻意解耦）；
//! - [`plugin`]：[`ArenaPlugin`]——场景文件载入关卡（`assets/
//!   arena_battle.scene.json`）、输入/时间 → 逻辑 → 敌人池 / 弹道池 /
//!   玩家三层 / HUD / 音效的每帧接线，系统编排可经
//!   [`ArenaPlugin::build_schedule`](plugin::ArenaPlugin::build_schedule)
//!   供 headless 测试与离屏快照复用（动画推进由 `AnimationPlugin` 负责，
//!   headless 路径需自行注册——与窗口路径 `DefaultPlugins` 同一编排）。
//!
//! ```ignore
//! use iris_engine::prelude::*;
//! use iris_arena::ArenaPlugin;
//!
//! App::new()
//!     .add_plugins(DefaultPlugins::new("Iris Arena"))
//!     .add_plugin(ArenaPlugin::default())   // 或 ArenaPlugin::new("关卡.json")
//!     .run();
//! ```
//!
//! 操作：WASD 移动（屏幕相对 + 贴墙滑行）、Shift 冲刺、自动开火；
//! R 在 GAME OVER 后直接重开。波次间歇 3 s，敌人越打越快。

pub mod logic;
pub mod plugin;

pub use logic::{
    ArenaInput, ArenaState, BREATHER_TIME, COMBO_WINDOW, DASH_COOLDOWN, DASH_INVULN, DASH_SPEED,
    DASH_TIME, ENEMY_BASE_SPEED, ENEMY_KNOCKBACK, ENEMY_MAX_HP, ENEMY_RADIUS, ENEMY_SPEED_PER_WAVE,
    EnemyState, FIRE_INTERVAL, FIRE_RANGE, HURT_INVULN, KNOCKBACK_DECAY, MAX_ENEMIES,
    MAX_PROJECTILES, PLAYER_KNOCKBACK, PLAYER_MAX_HP, PLAYER_RADIUS, PLAYER_SPEED, PROJECTILE_LIFE,
    PROJECTILE_RADIUS, PROJECTILE_SPEED, Phase, PlayerState, ProjectileState, SCORE_PER_KILL,
    StaticShape, WAVE_BASE, WAVE_SCALE,
};
pub use plugin::{
    ArenaAssets, ArenaEntities, ArenaPlugin, BANNER_TEXT_NAME, COMBO_TEXT_NAME, DEFAULT_SCENE_PATH,
    ENEMY_MODEL_PATH, EnemyPool, HP_BACK_NAME, HP_FRONT_NAME, ProjectilePool, SCORE_TEXT_NAME,
    TITLE_TEXT_NAME, WAVE_TEXT_NAME,
};
