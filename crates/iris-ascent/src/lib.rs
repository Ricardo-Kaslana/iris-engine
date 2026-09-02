//! # iris-ascent
//!
//! Iris 引擎下游示范游戏 IV：**Iris Ascent 3D 平台跳跃**（dogfooding IV）。
//! 第三人称沿线性上升关卡（悬浮台地 + 台阶 + 大球坡面 + 四段跳板）向上
//! 攀登，沿途收集宝石、激活检查点，抵达山顶信标获胜；坠落到坠落线以下
//! 回最近检查点复活。
//!
//! 相比前三作，本作把 R28–R31 的四个新能力当载荷压榨：
//! - **动画状态机 / crossfade / 事件轨道（R28）**：双剪辑角色 GLB
//!   （`idle` 呼吸 / `walk` 前倾摇摆），运行时状态机
//!   `request_state` 双向 0.15 s crossfade；walk 剪辑注入「footstep」
//!   事件轨道，`take_events` 驱动脚步声（事件轨道为 GLB 解析不产的
//!   API 侧构造）；
//! - **CSM 级联阴影（R29）**：方向光斜照 ~130 m 场景（关卡高差
//!   ~61 m），近 / 远影子同框（无需新代码，快照与 FrameStats 作证据）；
//! - **距离 LOD（R30）**：≥ 20 个远景装饰（山景球 / 巨柱 / 浮岛剪影）
//!   挂 `LodMesh`（高 / 低模 + 60 m 切换），`FrameStats.triangles_drawn`
//!   在相机拉远后显著下降（headless 断言）；
//! - **运动学角色控制器（R31）**：`move_and_slide_statics` 就是本作的
//!   物理核心——纯逻辑核心（`logic`）刻意只依赖 glam + 引擎纯函数，
//!   headless 与窗口走同一模拟。
//!
//! 结构（iris-game / iris-arena / iris-bastion 同款分层）：
//! - [`logic`]：纯逻辑状态机（关卡写死、事件自带世界坐标、逐位确定性；
//!   含脚本化自动玩家 [`AutoPilot`]）；
//! - [`plugin`]：[`AscentPlugin`]——三层玩家结构（根实体 / 模型子实体 /
//!   相机子实体）+ 状态机动画 + 粒子 FX 池 + 空间音频 + HUD，系统编排经
//!   [`AscentPlugin::build_schedule`](plugin::AscentPlugin::build_schedule)
//!   供 headless 测试与离屏快照复用。
//!
//! ```ignore
//! use iris_engine::prelude::*;
//! use iris_ascent::AscentPlugin;
//!
//! App::new()
//!     .add_plugins(DefaultPlugins::new("Iris Ascent"))
//!     .add_plugin(AscentPlugin::default())
//!     .run();
//! ```
//!
//! 操作：WASD 移动（相机相对——相机固定偏移时世界系与屏幕系一致）、
//! 空格跳、R 手动回检查点。

pub mod logic;
pub mod plugin;

pub use logic::{
    AIR_ACCEL, AscentEvent, AscentInput, AscentState, AutoPilot, CHARACTER, CHECKPOINTS,
    COYOTE_TIME, GEM_RADIUS, GEMS, GROUND_ACCEL, HALF_HEIGHT, KILL_Y, LAND_IMPACT_MIN,
    LAUNCH_SPEED, MOVE_SPEED, PADS, PLATFORMS, Phase, Platform, SPAWN_FOOT, SPHERE_CENTER,
    SPHERE_RADIUS, SPHERE_TOP, SUMMIT, SUMMIT_ZONE_CENTER, SUMMIT_ZONE_HALF, run_until,
};
pub use plugin::{
    AscentEntities, AscentPlugin, BANNER_TEXT_NAME, BannerTimer, CAMERA_OFFSET,
    CHECKPOINT_TEXT_NAME, CameraRig, DECOR_COUNT, FALLS_TEXT_NAME, FxPool, GEMS_TEXT_NAME,
    LOD_SWITCH_DISTANCE, STATE_IDLE, STATE_WALK,
};
