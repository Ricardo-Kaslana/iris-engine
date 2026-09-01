//! # iris-bastion
//!
//! Iris 引擎下游示范游戏 III：**Iris Bastion 塔防**（dogfooding III）。
//! 俯视固定视角：敌人沿航点折线（7 段 6 弯）进攻路径末端的核心，玩家用
//! 击杀所得积分在 1×1 m 网格上放塔——哨兵（速射单体快速弹）与臼炮
//! （慢速弹落点 AOE）——守到第 5 波全清获胜、核心 10 HP 归零告负。
//!
//! 相比前两作，本作把 R21–R25 的五个新能力当载荷压榨：
//! - **鼠标拾取（R21）**：`Input::cursor_position()` 像素 → NDC → 相机
//!   vp 逆 → 世界射线 → `iris_core::physics::raycast` 打静态地面碰撞体
//!   → 网格坐标（刻意走引擎查询而非解析平面）；
//! - **透明材质（R23）**：幽灵预览塔身（`AlphaMode::Blend`，可放 = 蓝调
//!   / 不可放 = 红调）+ 射程指示半透明圆盘；
//! - **粒子（R24）**：开火枪口闪光 / 命中火花 / 死亡爆炸 / 漏怪核心告警，
//!   发射器挂 FX 池槽位实体按需触发；
//! - **空间音频（R25）**：开火 / 命中 / 爆炸 / 死亡 / 漏怪五种音
//!   `play_spatial` 定位到事件世界坐标，听者每帧跟随俯视相机；
//! - **对象池（arena 模式）**：塔 / 敌 / 弹 / FX 四池启动预热，
//!   运行期零 spawn / despawn。
//!
//! 结构：
//! - [`logic`]：纯逻辑状态机（只依赖 glam、零引擎依赖、全部可单测；
//!   放塔校验三分支单独导出；事件列表自带世界坐标供表现层定位）；
//! - [`plugin`]：[`BastionPlugin`]——输入/时间 → 逻辑 → 塔池 / 敌池 /
//!   双弹池 / FX 池 / 幽灵预览 / HUD / 空间音频的每帧接线，系统编排可经
//!   [`BastionPlugin::build_schedule`](plugin::BastionPlugin::build_schedule)
//!   供 headless 测试与离屏快照复用（层级传播与粒子推进由 `HierarchyPlugin`
//!   / `ParticlePlugin` 负责，headless 路径需自行注册——与窗口路径
//!   `DefaultPlugins` 同一编排）。
//!
//! ```ignore
//! use iris_engine::prelude::*;
//! use iris_bastion::BastionPlugin;
//!
//! App::new()
//!     .add_plugins(DefaultPlugins::new("Iris Bastion"))
//!     .add_plugin(BastionPlugin)
//!     .run();
//! ```
//!
//! 操作：鼠标移动选格（幽灵预览蓝可放 / 红不可放）、左键放塔、数字键
//! 1 / 2 或右键切换塔种。击杀得积分、放塔扣款、漏怪扣核心 HP。

pub mod logic;
pub mod plugin;

pub use logic::{
    AOE_RADIUS, BRUTE_BOUNTY, BRUTE_HP, BRUTE_RADIUS, BRUTE_SPEED, BULLET_LIFE, BULLET_RADIUS,
    BULLET_SPEED, BastionEvent, BastionInput, BastionState, BulletState, CORE_MAX_HP, CORE_POS,
    EnemyKind, EnemyState, FIELD_MIN, GRID_H, GRID_W, MAX_BULLETS, MAX_ENEMIES, MAX_SHELLS,
    MAX_TOWERS, MAX_WAVES, MORTAR_COOLDOWN, MORTAR_COST, MORTAR_DAMAGE, MORTAR_RANGE, Phase,
    PlacementError, RUNNER_BOUNTY, RUNNER_HP, RUNNER_RADIUS, RUNNER_SPEED, SENTRY_COOLDOWN,
    SENTRY_COST, SENTRY_DAMAGE, SENTRY_RANGE, SHELL_SPEED, SPAWN_INTERVAL, START_CREDITS,
    ShellState, TowerKind, TowerState, WAVE_BREATHER, WAYPOINTS, cell_center, cell_in_field,
    path_dir, path_length, path_point, wave_composition, world_to_cell,
};
pub use plugin::{
    BANNER_TEXT_NAME, BASTION_REFERENCE_DISTANCE, BUILD_TEXT_NAME, BastionAssets, BastionEntities,
    BastionPickError, BastionPlugin, BastionViewport, BulletPool, CORE_TEXT_NAME,
    CREDITS_TEXT_NAME, DEFAULT_VIEWPORT, EnemyPool, FxPool, GhostKind, GhostMaterials, HoverCell,
    MappedPool, POOL_BULLETS, POOL_ENEMIES, POOL_FX, POOL_SHELLS, POOL_TOWERS, ShellPool,
    TowerPool, WAVE_TEXT_NAME, active_camera_vp, pick_ground_cell, screen_ray, world_to_screen,
};
