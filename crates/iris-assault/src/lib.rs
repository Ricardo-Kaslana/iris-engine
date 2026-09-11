//! # iris-assault
//!
//! Iris 引擎下游示范游戏 VI：**Iris Assault 写实战斗庭院**（dogfooding
//! VI，写实路线垂直切片）。20×14 m 庭院中，持刀 + 步枪的玩家与 4 名持刀
//! 敌人死斗：刀剑三连段（前摇 / 扇形判定窗 / 后摇 + 第三段击退）、步枪
//! （射速 / 弹匣 / 换弹 / 弹道遮挡）、全歼胜利、HP 归零败北；开场与终局
//! 各有一段相机演出。
//!
//! 本作把 **R42–R47 六轮新能力当载荷全量实弹**（与往作把单轮新能力压榨
//! 到极限不同，本作的首要目的是六轮能力的**接线验收**与摩擦清单）：
//! - **写实材质（R42）**：地面石板 / 砖墙用引擎 `brick_normal_orm`，
//!   刀身划痕与布料褶皱用本 crate 纯函数生成器（`textures` 模块），
//!   经 `GltfBuilder::material_textured` **在资产侧**把法线 / ORM 通道
//!   嵌进 GLB（打磨 VI 摩擦③ 闭合）；运行期只剩敌我配色覆写；
//! - **DOF / MB（R43）**：战斗默认开（对焦 = 引擎 `focus_target` 解析
//!   交战目标骨，摩擦⑤ 闭合）、演出相机运动模糊；
//! - **流式音频（R44）**：pack 内 OGG 氛围循环上音乐总线（+ 混响）、
//!   战斗音效 `play_spatial`；
//! - **pack 资产（R45）**：全部 GLB / OGG 打包 `assets/assault.ipak`，
//!   `AssetMount(Pack)` 挂载，GLB 走内存加载入口（自包含约束实弹）；
//! - **动画 v4（R46）**：上下身**双 `AnimationPlayer`**（下半身
//!   `BlendSpace` 步态 / 上半身动作剪辑 + `AnimationMask`）的组合姿势、
//!   受击 `AdditiveLayer` 抖动、`HandIk` 双臂 ADS 吸附枪身；
//! - **演出时间线（R47）**：开场绕庭相机 spline + 字幕 + 黑边 →
//!   `intro_end` 事件开战；胜利 / 败北推近结算演出。
//!   老能力照用：ragdoll 死亡、贴花战痕（血渍 / 弹孔）、粒子 FX 池、
//!   NavGrid 绕喷泉追击。
//!
//! 结构（iris-game / iris-arena / iris-bastion / iris-ascent /
//! iris-infiltration 同款分层）：
//! - [`logic`]：纯逻辑状态机（关卡写死、事件自带世界坐标、逐位确定性；
//!   含脚本化自动玩家 [`AutoPilot`]）；
//! - [`skeleton`]：角色骨架节点表（GLB 节点**下标**（动画通道 / 遮罩）
//!   与**名**（装配层按名寻骨）的单一事实来源，资产生成器与插件层共用）；
//! - [`textures`]：程序化写实纹理生成器（划痕金属 / 布料 / 贴花，纯
//!   函数）；
//! - [`plugin`]：[`AssaultPlugin`]——三层角色结构 + 双播放器动画 + 手 /
//!   足 IK + ragdoll / 贴花 / FX + pack 挂载 + 流式音乐 + DOF + 时间线；
//!   系统编排经
//!   [`AssaultPlugin::build_schedule`](plugin::AssaultPlugin::build_schedule)
//!   暴露，供 headless 测试与离屏快照复用。
//!
//! ```ignore
//! use iris_engine::prelude::*;
//! use iris_assault::AssaultPlugin;
//!
//! App::new()
//!     .add_plugins(DefaultPlugins::new("Iris Assault"))
//!     .add_plugin(AssaultPlugin::default())
//!     .run();
//! ```
//!
//! 操作：WASD 移动（相机固定俯角，屏幕系 = 世界系）、空格 / 鼠标左键
//! 挥刀（连按三连段）、F 开火、Q / 鼠标右键瞄准（ADS + 手 IK 吸附）、
//! R 换弹、Esc 退出由平台自理。
//!
//! # 本轮纪律
//!
//! 游戏轮不改引擎（dogfooding 纪律）：接入中发现的引擎摩擦全部记入
//! `ARCHITECTURE.md` §8 摩擦清单（dogfooding VI 条目 = **§8.26**），绕行
//! 写在本 crate 内。**打磨 VI 已闭合 §8.26 全部七条**（引擎侧修复 +
//! 本 crate 侧绕行删除：IK 回写归一化、prelude 内存加载入口、
//! `GltfBuilder::material_textured`、Vorbis 编码管线文档化、
//! `focus_target`、`node_entity_by_name`、example 命名约定），本 crate
//! 不再保留 R48 的四处绕行代码。

pub mod geom;
pub mod logic;
pub mod plugin;
pub mod scenery;
pub mod scenery_tex;
pub mod skeleton;
pub mod soldier;
pub mod soldier_tex;
pub mod textures;

pub use logic::{
    AIM_LOCK_RADIUS, AIM_SPEED, AssaultEvent, AssaultInput, AssaultState, AutoPilot, Block,
    CHARACTER, ENEMY_DAMAGE, ENEMY_MAX_HP, ENEMY_SPAWNS, ENEMY_SPEED, Enemy, EnemyState,
    FOUNTAIN_COLLIDER, HALF_HEIGHT, KNOCKBACK_SPEED, KNOCKBACK_TIME, MOVE_SPEED, NAV_AGENT_RADIUS,
    NAV_BOUNDS_MAX, NAV_BOUNDS_MIN, NAV_CELL_SIZE, PILLARS, PLAYER_MAX_HP, Phase, Player,
    REPATH_INTERVAL, RIFLE_DAMAGE, RIFLE_HIT_RADIUS, RIFLE_INTERVAL, RIFLE_MAG, RIFLE_RANGE,
    RIFLE_RELOAD, SEPARATION_DIST, SPAWN_FOOT, SWORD_DAMAGE, SWORD_FACE_RADIUS, SWORD_REACH,
    SWORD_RECOVER, SWORD_SECTOR_COS, SWORD_SWEEP, SWORD_WINDUP, SwordPhase, SwordState, WALLS,
    run_until,
};
pub use plugin::{
    AMBIENCE_ENTRY, AMMO_TEXT_NAME, AssaultAssets, AssaultEntities, AssaultPlugin,
    BANNER_TEXT_NAME, BannerTimer, CAMERA_OFFSET, ENEMIES_TEXT_NAME, EnemyEntities, GLB_ENTRY,
    GLB_FALLBACK, HP_TEXT_NAME, OutroLatch, REFERENCE_DISTANCE,
};
pub use skeleton::{
    CHEST, FOREGRIP, HEAD, L_FOOT, L_FOREARM, L_HAND, L_SHIN, L_THIGH, L_UPPERARM, LOWER_BODY,
    NODE_COUNT, PELVIS, R_FOOT, R_FOREARM, R_HAND, R_SHIN, R_THIGH, R_UPPERARM, RIFLE, RIFLE_GRIP,
    ROOT, SWORD, SWORD_BLADE, TORSO, UPPER_BODY,
};
pub use textures::{blood_splat, bullet_hole, cloth_normal_orm, scratched_metal_normal_orm};
