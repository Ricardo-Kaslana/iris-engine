//! 纯游戏逻辑核心：确定性战斗状态机。依赖 glam + [`iris_core`] 的**纯
//! 函数**（[`move_and_slide_statics`] / [`ColliderShape`] /
//! [`ray_vs_shape`] / [`NavGrid`]）——刻意的 dogfooding：移动就是引擎
//! 运动学角色控制器（R31）、敌人追击就是引擎 NavGrid（R35）、弹道与
//! 遮挡就是引擎射线纯函数（R21 同款）。「Iris Assault」的全部规则
//! （刀剑三连段 / 步枪 / 敌人追击与挥砍 / 胜负）都在这个模块里以纯函数
//! 推进：输入 [`AssaultInput`]，输出状态变化与 [`AssaultEvent`] 列表
//! ——不读时钟、不做随机、不触碰 ECS 世界。因此：
//! - 全部规则可毫秒级单测（本模块底部 19 项）；
//! - 插件层（`crate::plugin`）只负责把键盘 / 鼠标输入翻译成本模块类型、
//!   消费事件生成表现（动画 / 音频 / 粒子 / 贴花 / ragdoll / 演出）；
//! - headless 测试、自动玩家与离屏快照共享同一条确定性推进路径。
//!
//! # 战斗庭院布局（20×14 m，坐标即文档）
//!
//! 可玩区 x∈(-9.8, 9.8)、z∈(-6.8, 6.8)；四周砖围墙（高 3 m）；中央
//! 喷泉（圆障碍：球体碰撞元半径 1.8）+ 四角石柱（AABB，(±6.5, ±4.5)）
//! ——NavGrid 有绕行价值（追击敌人必须绕喷泉，`nav_paths_detour_around_
//! fountain` 单测锁定）：
//!
//! ```text
//!  z=-6.8 ┌──────────────────────────┐
//!         │  E0(-3.5,-4)    E1(3.5,-4.2) │  四角柱：P(±6.5, ±4.5)
//!         │           (╥) 喷泉           │
//!         │  E2(-4,4.2)     E3(4.2,4)    │
//!  z=+6.8 └──────────────────────────┘
//!         玩家出生 (-7.5, 0)，面朝 +X（庭院中心方向）
//! ```
//!
//! # 刀剑三连段（每段 = 前摇 / 横扫判定窗 / 后摇）
//!
//! 状态机 [`SwordState`]：`stage = 0` 空闲，否则 `stage 1..=3` ×
//! [`SwordPhase`]（前摇 → 横扫 → 后摇）。判定窗内**每帧**对扇形区域
//! （半径 [`SWORD_REACH`]、半角 55°）内的敌人做判定——同一敌人在同一次
//! 横扫（`sweep_id` 代次）内至多命中一次；第三段命中附带击退（线性衰减
//! 冲量，击退期间不追击）。横扫 / 后摇期的攻击键边沿预约下一段
//! （`queued`），后摇结束自动接续——三连段由此成立。前摇起手时面向
//! **自动锁定** 4 m 内最近敌人（无则保持当前朝向）。
//!
//! # 步枪（射速 + 弹匣 + 换弹计时）
//!
//! `fire_held` 按射速 [`RIFLE_INTERVAL`] 触发；弹匣 [`RIFLE_MAG`] 发、
//! 打空自动换弹（[`RIFLE_RELOAD`] 秒，换弹中不能射击）；`reload_press`
//! 边沿手动换弹（仅刀空闲时受理）。命中 = 射线扫掠：枪口 → 枪口 +
//! 朝向 × [`RIFLE_RANGE`]，先取**静态体最近命中**（引擎 [`ray_vs_shape`]
//! ——被墙挡住的敌人不中弹），再对敌人做 XZ 线段-圆扫掠
//! （[`segment_hits_circle_t`]，bastion 同款防穿模语义的参数化版）；
//! 命中墙面产出 [`AssaultEvent::BulletHitWorld`]（贴弹孔的位置与法线由
//! [`RayHit`] 自带）。枪击命中同样造成（重置型）硬直——火力压制反馈。
//!
//! # 敌人（追击 → 挥砍 → 硬直）
//!
//! 状态机 [`EnemyState`]：待机（玩家进入 [`AGGRO_RADIUS`] 即警觉）→
//! 追击（[`NavGrid::find_path`] 航点，0.5 s 重寻路节拍）→ 挥砍前摇
//! （0.5 s，持续转向玩家）→ 判定窗（窗中点一次性判定：近身圆 + 前扇形
//! 60°）→ 后摇 → 回追击。被击中进硬直 [`ENEMY_STAGGER`]——**硬直不
//! 叠加**：硬直中再被击只重置计时（单测锁定）。死亡（HP ≤ 0）即标记
//! [`EnemyState::Dead`]（表现层 ragdoll 瘫倒）。
//!
//! # 帧内顺序（固定，是确定性的一部分）
//!
//! 终局冻结 → 玩家（换弹 → 移动 → 刀剑状态机）→ 刀剑判定 → 步枪 →
//! 敌人（状态机 → 移动）→ 分离推挤 → 胜负结算。敌人的感知用移动前的
//! 位姿、玩家攻击在敌人行动前——顺序本身是可单测的语义。
//!
//! # 演出（R47）与逻辑的关系
//!
//! 开场 / 胜利 / 败北演出由插件层驱动（`iris_engine::timeline`），
//! 纯核心只认识 [`Phase::Intro`]（冻结、等插件层调用
//! [`AssaultState::begin_battle`]）与终局（冻结、事件交插件层起演出）。
//! 演出期间逻辑不推进——同一帧序下演出与战局严格串行，确定性不受演出
//! 影响。
//!
//! # 自动玩家（[`AutoPilot`]）
//!
//! 简单决策脚本：接近最近敌人（NavGrid 绕喷泉）→ 刀距内挥刀三连段 →
//! 中距切步枪补射 → 弹尽换弹 → 循环；HP < 30 改拉距射击（怯战规则，
//! 保证 headless 通关的 HP 余量）。无隐藏随机源，同输入两次模拟逐位
//! 一致。

use glam::{Vec2, Vec3};

use iris_core::character::{CharacterParams, move_and_slide_statics};
use iris_core::nav::{NavGrid, NavGridParams};
use iris_core::physics::{ColliderShape, RayHit, ray_vs_shape};

// ---------------------------------------------------------------------------
// 可调常量（pub：单测 / 插件层 / 文档共用同一事实来源）
// ---------------------------------------------------------------------------

/// 玩家正常移动速度（米/秒）。
pub const MOVE_SPEED: f32 = 4.4;
/// 玩家瞄准移动速度（米/秒）：ADS 时减速的写实步态。
pub const AIM_SPEED: f32 = 2.0;
/// 挥砍期间移动速度倍率（前摇 / 横扫期——大幅减速的写实攻击步）。
pub const ATTACK_MOVE_SCALE: f32 = 0.35;
/// 后摇期间移动速度倍率。
pub const RECOVER_MOVE_SCALE: f32 = 0.7;
/// 换弹期间移动速度倍率。
pub const RELOAD_MOVE_SCALE: f32 = 0.8;
/// 敌人追击速度（米/秒）：略慢于玩家——走位有意义。
pub const ENEMY_SPEED: f32 = 3.0;
/// 敌人警觉半径（米）：玩家接近即转入追击（战斗演示无潜行语义）。
pub const AGGRO_RADIUS: f32 = 9.0;
/// 敌人进入挥砍的距离（米）。
pub const ENEMY_ATTACK_RANGE: f32 = 1.7;
/// 敌人挥砍判定距离（米）：判定窗内玩家中心距离上限。
pub const ENEMY_STRIKE_REACH: f32 = 1.9;
/// 敌人挥砍扇形半角余弦（半角 60°，cos = 0.5，const 上下文取精确字面量）。
pub const ENEMY_STRIKE_COS: f32 = 0.5;
/// 敌人挥砍前摇（秒）。
pub const ENEMY_WINDUP: f32 = 0.5;
/// 敌人挥砍判定窗（秒）：判定在中点一次性完成。
pub const ENEMY_STRIKE_WINDOW: f32 = 0.12;
/// 敌人挥砍后摇（秒）。
pub const ENEMY_RECOVER: f32 = 0.6;
/// 敌人被击硬直（秒）；硬直中再被击**重置**为该值（不叠加，见模块文档）。
pub const ENEMY_STAGGER: f32 = 0.35;
/// 敌人伤害（每刀）。
pub const ENEMY_DAMAGE: i32 = 12;
/// 敌人最大生命。
pub const ENEMY_MAX_HP: i32 = 60;
/// 玩家最大生命。
pub const PLAYER_MAX_HP: i32 = 100;

/// 刀剑判定半径（米）：扇形扫描的距离上限。
pub const SWORD_REACH: f32 = 2.3;
/// 刀剑扇形半角余弦（半角 55°，cos = 0.5735764——const 上下文取 IEEE
/// 精确字面量）：朝向与「玩家→敌人」方向点积低于此值即出扇。
pub const SWORD_SECTOR_COS: f32 = 0.573_576_4;
/// 起手自动锁敌半径（米）：前摇起手面向最近敌人的搜索半径。
pub const SWORD_FACE_RADIUS: f32 = 4.0;
/// 三段的前摇（秒）：第三段蓄力更久、伤害更高。
pub const SWORD_WINDUP: [f32; 3] = [0.18, 0.16, 0.22];
/// 三段的横扫判定窗（秒）。
pub const SWORD_SWEEP: [f32; 3] = [0.14, 0.14, 0.16];
/// 三段的后摇（秒）。
pub const SWORD_RECOVER: [f32; 3] = [0.22, 0.24, 0.42];
/// 三段伤害。
pub const SWORD_DAMAGE: [i32; 3] = [25, 25, 40];
/// 第三段击退初速（米/秒）：线性衰减 [`KNOCKBACK_TIME`] 秒归零。
pub const KNOCKBACK_SPEED: f32 = 8.0;
/// 击退衰减时长（秒）：位移总量 = 初速 × 时长 / 2 ≈ 1.2 m。
pub const KNOCKBACK_TIME: f32 = 0.3;

/// 步枪射速（秒/发）。
pub const RIFLE_INTERVAL: f32 = 0.16;
/// 步枪弹匣容量（发）。
pub const RIFLE_MAG: u32 = 12;
/// 换弹时长（秒）。
pub const RIFLE_RELOAD: f32 = 1.6;
/// 步枪单发伤害。
pub const RIFLE_DAMAGE: i32 = 12;
/// 步枪射程（米）。
pub const RIFLE_RANGE: f32 = 18.0;
/// 步枪命中判定半径（米）：弹道线段到敌人中心的 XZ 距离上限。
pub const RIFLE_HIT_RADIUS: f32 = 0.55;
/// 瞄准自动锁敌半径（米）：ADS 时面向最近敌人（写实射击的键鼠辅助，
/// 声明为设计语义而非 bug）。
pub const AIM_LOCK_RADIUS: f32 = 10.0;

/// 角色控制器参数：引擎 [`CharacterParams::default`] 的俯视改版——
/// 半径 0.4 / 半高 0.9 / 台阶 0.3 / 坡限 45° 保持缺省，**重力缩放 0**
/// （战斗平面无竖直运动；`grounded` 恒 false 是布局无地板碰撞的预期
/// 语义，与 iris-infiltration 同款）。
pub const CHARACTER: CharacterParams = CharacterParams {
    radius: 0.4,
    half_height: 0.9,
    step_height: 0.3,
    slope_limit_radians: std::f32::consts::FRAC_PI_4,
    snap_distance: 0.2,
    gravity_scale: 0.0,
};

/// 角色半高（速查）：玩家 / 敌人的胶囊中心 y = 此值（脚底 y = 0）。
pub const HALF_HEIGHT: f32 = 0.9;

// ---------------------------------------------------------------------------
// 关卡数据（写死、确定性；视觉网格与逻辑静态体共用——见 WALLS / PILLARS）
// ---------------------------------------------------------------------------

/// 一面墙 / 一根柱（AABB 静态体）：`half` = 半边长。视觉网格与逻辑
/// 静态体 / 导航体共用本结构（插件层按 `half × 2` 缩放立方体）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Block {
    /// 盒中心（世界坐标）。
    pub center: Vec3,
    /// 半边长。
    pub half: Vec3,
}

/// 围墙（四面；内缘 = 可玩区边界，高 3 m）。
pub const WALLS: [Block; 4] = [
    Block {
        center: Vec3::new(0.0, 1.5, -7.1),
        half: Vec3::new(10.4, 1.5, 0.3),
    }, // 北
    Block {
        center: Vec3::new(0.0, 1.5, 7.1),
        half: Vec3::new(10.4, 1.5, 0.3),
    }, // 南
    Block {
        center: Vec3::new(-10.1, 1.5, 0.0),
        half: Vec3::new(0.3, 1.5, 7.4),
    }, // 西
    Block {
        center: Vec3::new(10.1, 1.5, 0.0),
        half: Vec3::new(0.3, 1.5, 7.4),
    }, // 东
];

/// 四角石柱（AABB 障碍；柱础 0.9×0.9 m、高 3.2 m）——掩体与绕行价值。
pub const PILLARS: [Block; 4] = [
    Block {
        center: Vec3::new(-6.5, 1.6, -4.5),
        half: Vec3::new(0.45, 1.6, 0.45),
    },
    Block {
        center: Vec3::new(6.5, 1.6, -4.5),
        half: Vec3::new(0.45, 1.6, 0.45),
    },
    Block {
        center: Vec3::new(-6.5, 1.6, 4.5),
        half: Vec3::new(0.45, 1.6, 0.45),
    },
    Block {
        center: Vec3::new(6.5, 1.6, 4.5),
        half: Vec3::new(0.45, 1.6, 0.45),
    },
];

/// 中央喷泉碰撞元（圆障碍）：球体半径 1.8、中心 y 0.5——玩家中心高度
/// （0.9）处的水平半径 ≈ 1.76，与视觉盆沿（半径 1.7）匹配。**刻意不用
/// AABB**：庭院中心放圆障碍让绕行路径平滑，也把引擎碰撞元的球体分支
/// 当载荷压实。
pub const FOUNTAIN_COLLIDER: (Vec3, ColliderShape) = (
    Vec3::new(0.0, 0.5, 0.0),
    ColliderShape::Sphere { radius: 1.8 },
);

/// 玩家出生点（脚底）：庭院西侧。
pub const SPAWN_FOOT: Vec3 = Vec3::new(-7.5, 0.0, 0.0);

/// 敌人出生点（脚底，XZ）：庭院四象限，喷泉与石柱之间。
pub const ENEMY_SPAWNS: [Vec2; 4] = [
    Vec2::new(-3.5, -4.0),
    Vec2::new(3.5, -4.2),
    Vec2::new(-4.0, 4.2),
    Vec2::new(4.2, 4.0),
];

/// 导航图覆盖域与参数（构建参数收拢一处）。
pub const NAV_BOUNDS_MIN: Vec3 = Vec3::new(-10.4, 0.0, -7.4);
pub const NAV_BOUNDS_MAX: Vec3 = Vec3::new(10.4, 0.0, 7.4);
/// 导航 cell 边长（米）：≤ agent 半径的经验规则（见 `iris_core::nav`）。
pub const NAV_CELL_SIZE: f32 = 0.5;
/// 导航 agent 膨胀半径（米）：胶囊半径 0.4 + 5 cm 裕度。
pub const NAV_AGENT_RADIUS: f32 = 0.45;
/// 敌人重寻路间隔（秒）：find_path 不是免费的，按节拍重算。
pub const REPATH_INTERVAL: f32 = 0.5;
/// 航点到达半径（米）：敌人与自动玩家共用的「到点」判定。
pub const WAYPOINT_REACH: f32 = 0.35;
/// 敌人分离距离（米）：敌人间 / 敌人与玩家的最小中心距（推挤结算）。
pub const SEPARATION_DIST: f32 = 0.9;

/// 逻辑静态体（关卡碰撞事实来源）：四面围墙 + 四柱 + 喷泉。角色移动 /
/// 弹道遮挡 / 导航图构建三方共用同一份列表。
pub fn level_statics() -> Vec<(Vec3, ColliderShape)> {
    let mut statics = Vec::with_capacity(WALLS.len() + PILLARS.len() + 1);
    for block in WALLS.iter().chain(PILLARS.iter()) {
        statics.push((
            block.center,
            ColliderShape::Box {
                half_extents: block.half,
            },
        ));
    }
    statics.push(FOUNTAIN_COLLIDER);
    statics
}

/// 导航图参数（构建一次、全程复用——庭院无开门类动态障碍）。
pub fn nav_params() -> NavGridParams {
    NavGridParams {
        bounds_min: NAV_BOUNDS_MIN,
        bounds_max: NAV_BOUNDS_MAX,
        cell_size: NAV_CELL_SIZE,
        agent_radius: NAV_AGENT_RADIUS,
    }
}

// ---------------------------------------------------------------------------
// 输入 / 事件 / 阶段
// ---------------------------------------------------------------------------

/// 一帧的游戏输入：世界系水平意图 + 各键语义。纯核心不碰键盘——插件层
/// 把「相机相对的 WASD」翻译成世界系向量（相机固定俯角时世界系与屏幕系
/// 一致）；`sword_press` / `reload_press` 为边沿语义（本帧按下沿才为
/// true），`fire_held` / `aim_held` 为持续语义。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AssaultInput {
    /// 世界系水平移动意图（x = 世界 +X，y = 世界 +Z）。
    pub move_dir: Vec2,
    /// 本帧按下挥刀（边沿）。
    pub sword_press: bool,
    /// 本帧按住开火（持续）。
    pub fire_held: bool,
    /// 本帧按住瞄准（持续）。
    pub aim_held: bool,
    /// 本帧按下换弹（边沿）。
    pub reload_press: bool,
    /// 步长（秒）。
    pub dt: f32,
}

/// 一帧内产生的游戏事件（含世界坐标）：插件层据此定位音效 / 粒子 /
/// 贴花 / ragdoll / 演出。每帧 `step` 开头清空——`step` 返回后
/// [`AssaultState::frame_events`] 恰好持有本帧全部事件（顺序 = 结算
/// 顺序，确定性）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AssaultEvent {
    /// 开场演出结束、战斗开始（插件层调 [`AssaultState::begin_battle`]
    /// 时产生；位置 = 玩家胶囊中心）。
    BattleBegin { pos: Vec3 },
    /// 挥砍出刀（前摇结束进入横扫窗；位置 = 玩家胶囊中心）。
    SwordSwing { pos: Vec3, facing: f32, stage: u8 },
    /// 挥砍命中敌人（pos = 敌人被击位置；killed = 本击致死）。
    SwordHit {
        pos: Vec3,
        facing: f32,
        enemy: usize,
        stage: u8,
        killed: bool,
    },
    /// 挥砍落空（判定窗关闭时扇区内无人）。
    SwordWhiff { pos: Vec3, facing: f32, stage: u8 },
    /// 开火（pos = 枪口世界坐标；dir = 弹道单位方向）。
    Shot { pos: Vec3, dir: Vec3 },
    /// 弹道命中敌人（pos = 敌人中心；killed = 本击致死）。
    BulletHitEnemy {
        pos: Vec3,
        enemy: usize,
        killed: bool,
    },
    /// 弹道命中静态体（pos = 命中点；normal = 表面外法线——贴弹孔用）。
    BulletHitWorld { pos: Vec3, normal: Vec3 },
    /// 换弹开始（pos = 玩家胶囊中心）。
    ReloadStart { pos: Vec3 },
    /// 敌人挥砍出刀（判定窗开始；表现层放敌人刀光）。
    EnemyStruck { pos: Vec3, enemy: usize },
    /// 玩家被击中（pos = 玩家胶囊中心；hp = 击后余量）。
    PlayerHurt { pos: Vec3, hp: i32 },
    /// 敌人被击硬直（pos = 敌人胶囊中心；重置语义见 [`ENEMY_STAGGER`]）。
    EnemyStaggered { pos: Vec3, enemy: usize },
    /// 敌人死亡（pos = 敌人脚底；facing = 死亡时朝向——ragdoll 踢飞方向）。
    EnemyKilled {
        pos: Vec3,
        facing: f32,
        enemy: usize,
    },
    /// 全歼敌人（pos = 玩家胶囊中心）。
    Victory { pos: Vec3 },
    /// 玩家 HP 归零（pos = 玩家胶囊中心）。
    Defeat { pos: Vec3 },
}

/// 游戏阶段：开场演出中 / 战斗中 / 胜利 / 败北（终局均冻结；重开 =
/// 重建 [`AssaultState`]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// 开场演出中（逻辑冻结；插件层在时间线 intro_end 事件时调
    /// [`AssaultState::begin_battle`]）。
    Intro,
    /// 战斗中。
    Playing,
    /// 全歼敌人。
    Victory,
    /// 玩家 HP 归零。
    Defeat,
}

/// 刀剑阶段相位：前摇（起手蓄力、移动大幅减速）→ 横扫（判定窗）→
/// 后摇（可预约下一段）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwordPhase {
    /// 前摇。
    Windup,
    /// 横扫判定窗。
    Sweep,
    /// 后摇。
    Recover,
}

/// 玩家刀剑状态机。`stage = 0` 即空闲；`sweep_id` 为横扫代次（每次
/// 进入 Sweep 自增——同代次内敌人至多被打一次的防多帧重复判定键）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SwordState {
    /// 当前段（1..=3；0 = 空闲）。
    pub stage: u8,
    /// 当前相位（stage = 0 时无意义，恒为 `Windup` 占位）。
    pub phase: SwordPhase,
    /// 当前相位已流逝时间（秒）。
    pub t: f32,
    /// 是否已预约下一段（sweep / recover 期的攻击键边沿置位）。
    pub queued: bool,
    /// 横扫代次（防同一次横扫多帧重复命中）。
    pub sweep_id: u32,
    /// 本次横扫是否命中过任何人（判定窗关闭时全空 → `SwordWhiff`）。
    pub sweep_hit_any: bool,
}

impl Default for SwordState {
    fn default() -> Self {
        Self {
            stage: 0,
            phase: SwordPhase::Windup,
            t: 0.0,
            queued: false,
            sweep_id: 0,
            sweep_hit_any: false,
        }
    }
}

impl SwordState {
    /// 是否处于挥砍动作中（任一相位）。
    pub fn active(&self) -> bool {
        self.stage != 0
    }
}

/// 玩家步枪状态：弹药 / 射速冷却 / 换弹计时。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RifleState {
    /// 弹匣余弹（发）。
    pub ammo: u32,
    /// 射击冷却（秒；> 0 时不能开火，每帧递减）。
    pub cooldown: f32,
    /// 换弹剩余时间（秒；0 = 不在换弹）。
    pub reload_t: f32,
    /// 累计开火数（簿记：供战报 / 表现层核对）。
    pub shots: u64,
}

impl RifleState {
    /// 满弹匣的新枪。
    pub fn new() -> Self {
        Self {
            ammo: RIFLE_MAG,
            ..Default::default()
        }
    }
}

/// 玩家纯逻辑状态。字段全部公开：测试可以直接构造 / 修改中间状态，
/// 插件层按只读约定消费。
#[derive(Clone, Copy, Debug)]
pub struct Player {
    /// 胶囊中心（世界坐标；脚底 = y − [`HALF_HEIGHT`]）。
    pub pos: Vec3,
    /// 线速度（世界系；由引擎控制器的滑行结果回灌）。
    pub vel: Vec3,
    /// 模型朝向（绕 Y 弧度；0 = 面朝 -Z，引擎前方约定）。
    pub facing: f32,
    /// 生命值。
    pub hp: i32,
    /// 刀剑状态机。
    pub sword: SwordState,
    /// 步枪状态。
    pub rifle: RifleState,
    /// 本帧是否瞄准（表现层姿态 / 移动速度共用）。
    pub aiming: bool,
    /// 本帧实际移动速度（米/秒；表现层混合空间参数，静止 = 0）。
    pub anim_speed: f32,
    /// 受击闪烁计时（秒，> 0 = 刚被击中；表现层提示用）。
    pub hurt_flash: f32,
}

/// 敌人状态机：待机 → 追击 → 挥砍（前摇 / 判定窗 / 后摇）→ 硬直 /
/// 死亡。追击走 [`NavGrid::find_path`] 航点；挥砍判定一次性（窗中点）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnemyState {
    /// 待机（玩家未进警觉半径或已死）。
    Idle,
    /// 追击（航点推进；进入 [`ENEMY_ATTACK_RANGE`] 转前摇）。
    Chase,
    /// 挥砍前摇（持续转向玩家）。
    Windup,
    /// 挥砍判定窗（中点一次性判定）。
    Strike,
    /// 挥砍后摇。
    Recover,
    /// 被击硬直（计时归零回追击；再被击重置计时——不叠加）。
    Stagger,
    /// 死亡（终态；表现层 ragdoll，逻辑只保留位置）。
    Dead,
}

/// 一名敌人的纯逻辑状态（含航点队列 `Vec`——只 `Clone` 不 `Copy`）。
#[derive(Clone, Debug)]
pub struct Enemy {
    /// 胶囊中心（世界坐标；脚底 = y − [`HALF_HEIGHT`]）。
    pub pos: Vec3,
    /// 朝向（绕 Y 弧度）。
    pub facing: f32,
    /// 生命值。
    pub hp: i32,
    /// 状态机。
    pub state: EnemyState,
    /// 当前状态已流逝时间（秒）。
    pub t: f32,
    /// 挥砍判定是否已结算（Strike 窗内只结算一次）。
    pub strike_resolved: bool,
    /// 当前 find_path 航点队列（y 恒 0，移动时覆写为敌人自己的高度）。
    pub path: Vec<Vec3>,
    /// 航点队列消费下标。
    pub path_index: usize,
    /// 重寻路计时（秒）。
    pub repath_timer: f32,
    /// 击退剩余时间（秒；> 0 时以击退速度移动、不追击）。
    pub knockback_t: f32,
    /// 击退方向（单位向量，XZ）。
    pub knockback_dir: Vec2,
    /// 最后一次被打中的横扫代次（同代次防重复命中）。
    pub last_sweep_id: u32,
    /// 本帧实际移动速度（米/秒；表现层混合空间参数）。
    pub anim_speed: f32,
}

// ---------------------------------------------------------------------------
// 状态
// ---------------------------------------------------------------------------

/// 游戏全局状态：阶段 + 玩家 + 敌人 + 导航图 + 本帧事件。字段全部公开：
/// 测试可以直接构造 / 修改中间状态，插件层按只读约定消费（每帧由
/// [`AssaultState::step`] 独占推进；开场结束经
/// [`AssaultState::begin_battle`] 由插件层在时间线事件里调用）。
#[derive(Clone, Debug)]
pub struct AssaultState {
    pub phase: Phase,
    /// 玩家状态。
    pub player: Player,
    /// 敌人（下标即 [`ENEMY_SPAWNS`] 下标）。
    pub enemies: Vec<Enemy>,
    /// 对局累计时间（秒；`step` 累加，确定性）。
    pub elapsed: f32,
    /// 已确认击杀数（= 死亡敌人数；HUD 与结算用）。
    pub kills: u32,
    /// 逻辑静态体（围墙 + 石柱 + 喷泉；启动构造一次）。
    pub statics: Vec<(Vec3, ColliderShape)>,
    /// 导航图（启动 build 一次；庭院无动态障碍，全程不重建）。
    pub nav: NavGrid,
    /// 本帧事件列表（每帧 `step` 开头清空，见 [`AssaultEvent`]）。
    pub frame_events: Vec<AssaultEvent>,
}

impl Default for AssaultState {
    fn default() -> Self {
        Self::new()
    }
}

impl AssaultState {
    /// 新状态：开场演出阶段（逻辑冻结），玩家与 4 名敌人各就各位，
    /// 导航图按庭院静态体构建。
    pub fn new() -> Self {
        let enemies = ENEMY_SPAWNS
            .iter()
            .map(|&spawn| Enemy {
                pos: Vec3::new(spawn.x, HALF_HEIGHT, spawn.y),
                facing: 0.0,
                hp: ENEMY_MAX_HP,
                state: EnemyState::Idle,
                t: 0.0,
                strike_resolved: false,
                path: Vec::new(),
                path_index: 0,
                repath_timer: 0.0,
                knockback_t: 0.0,
                knockback_dir: Vec2::ZERO,
                last_sweep_id: 0,
                anim_speed: 0.0,
            })
            .collect();
        Self {
            phase: Phase::Intro,
            player: Player {
                pos: SPAWN_FOOT + Vec3::new(0.0, HALF_HEIGHT, 0.0),
                vel: Vec3::ZERO,
                facing: -std::f32::consts::FRAC_PI_2, // 面朝 +X（庭院中心）
                hp: PLAYER_MAX_HP,
                sword: SwordState::default(),
                rifle: RifleState::new(),
                aiming: false,
                anim_speed: 0.0,
                hurt_flash: 0.0,
            },
            enemies,
            elapsed: 0.0,
            kills: 0,
            statics: level_statics(),
            nav: NavGrid::build(nav_params(), &level_statics()),
            frame_events: Vec::new(),
        }
    }

    /// 开场结束、战斗开始（插件层在时间线 `intro_end` 事件时调用；
    /// headless 测试直接调用同一入口）。幂等：重复调用不重复产生事件。
    pub fn begin_battle(&mut self) {
        if self.phase != Phase::Intro {
            return;
        }
        self.phase = Phase::Playing;
        let pos = self.player.pos;
        self.frame_events.push(AssaultEvent::BattleBegin { pos });
    }

    /// 活着的敌人数。
    pub fn enemies_alive(&self) -> usize {
        self.enemies
            .iter()
            .filter(|enemy| enemy.state != EnemyState::Dead)
            .count()
    }

    /// 存活敌人中距离玩家最近者（下标 + 距离；全灭返回 `None`）。
    pub fn nearest_enemy(&self) -> Option<(usize, f32)> {
        let mut best: Option<(usize, f32)> = None;
        for (index, enemy) in self.enemies.iter().enumerate() {
            if enemy.state == EnemyState::Dead {
                continue;
            }
            let dist = self.player.pos.distance(enemy.pos);
            if best.is_none_or(|(_, d)| dist < d) {
                best = Some((index, dist));
            }
        }
        best
    }

    /// 推进一帧。非 `Playing` 阶段冻结（什么都不做）；`dt <= 0` 时冻结。
    /// 帧内顺序见模块文档（是确定性的一部分）。
    pub fn step(&mut self, input: &AssaultInput) {
        self.frame_events.clear();
        if self.phase != Phase::Playing {
            return;
        }
        let dt = input.dt;
        if dt <= 0.0 {
            return;
        }
        self.elapsed += dt;

        self.update_player(input, dt);
        self.resolve_sword_sweep();
        self.resolve_rifle(input, dt);
        for index in 0..self.enemies.len() {
            self.update_enemy(index, dt);
        }
        self.resolve_separation();

        // ---- 胜负结算 ----
        if self.player.hp <= 0 {
            self.phase = Phase::Defeat;
            let pos = self.player.pos;
            self.frame_events.push(AssaultEvent::Defeat { pos });
            return;
        }
        if self.enemies_alive() == 0 {
            self.phase = Phase::Victory;
            let pos = self.player.pos;
            self.frame_events.push(AssaultEvent::Victory { pos });
        }
    }

    /// 玩家：瞄准标记 → 换弹计时 → 移动（含动作减速）→ 朝向 → 刀剑
    /// 状态机推进（横扫的命中结算在 [`AssaultState::resolve_sword_sweep`]）。
    fn update_player(&mut self, input: &AssaultInput, dt: f32) {
        self.player.aiming = input.aim_held;
        self.player.hurt_flash = (self.player.hurt_flash - dt).max(0.0);

        // ---- 换弹计时（换弹中不能开火 / 挥刀；归零瞬间弹匣回满——
        // 恰好一次：reload_t 已 0，分支不再进入）----
        if self.player.rifle.reload_t > 0.0 {
            self.player.rifle.reload_t = (self.player.rifle.reload_t - dt).max(0.0);
            if self.player.rifle.reload_t == 0.0 {
                self.player.rifle.ammo = RIFLE_MAG;
            }
        }
        // 手动换弹请求：边沿 + 刀空闲 + 不在换弹 + 弹匣未满
        if input.reload_press
            && !self.player.sword.active()
            && self.player.rifle.reload_t <= 0.0
            && self.player.rifle.ammo < RIFLE_MAG
        {
            self.player.rifle.reload_t = RIFLE_RELOAD;
            let pos = self.player.pos;
            self.frame_events.push(AssaultEvent::ReloadStart { pos });
        }

        // ---- 移动速度：基础 → 瞄准 / 动作衰减 ----
        let mut speed = if input.aim_held {
            AIM_SPEED
        } else {
            MOVE_SPEED
        };
        if self.player.sword.active() {
            speed *= match self.player.sword.phase {
                SwordPhase::Recover => RECOVER_MOVE_SCALE,
                _ => ATTACK_MOVE_SCALE,
            };
        }
        if self.player.rifle.reload_t > 0.0 {
            speed *= RELOAD_MOVE_SCALE;
        }
        let target = input.move_dir * speed;
        self.player.vel.x = target.x;
        self.player.vel.z = target.y;
        let out = move_and_slide_statics(
            self.player.pos,
            self.player.vel,
            dt,
            &CHARACTER,
            &self.statics,
        );
        self.player.pos = out.position;
        self.player.vel = out.velocity;
        self.player.anim_speed = Vec2::new(out.velocity.x, out.velocity.z).length();

        // ---- 朝向：瞄准锁定 > 移动方向 > 保持 ----
        if input.aim_held {
            if let Some((index, dist)) = self.nearest_enemy() {
                if dist <= AIM_LOCK_RADIUS {
                    let to = Vec2::new(
                        self.enemies[index].pos.x - self.player.pos.x,
                        self.enemies[index].pos.z - self.player.pos.z,
                    );
                    if to.length_squared() > 1e-8 {
                        self.player.facing = (-to.x).atan2(-to.y);
                    }
                }
            }
        } else if input.move_dir.length_squared() > 1e-6 {
            self.player.facing = (-input.move_dir.x).atan2(-input.move_dir.y);
        }

        // ---- 刀剑状态机推进 ----
        if !self.player.sword.active() {
            if input.sword_press {
                // 起手：自动锁面向 4 m 内最近敌人，否则保持当前朝向
                if let Some((index, dist)) = self.nearest_enemy() {
                    if dist <= SWORD_FACE_RADIUS {
                        let to = Vec2::new(
                            self.enemies[index].pos.x - self.player.pos.x,
                            self.enemies[index].pos.z - self.player.pos.z,
                        );
                        if to.length_squared() > 1e-8 {
                            self.player.facing = (-to.x).atan2(-to.y);
                        }
                    }
                }
                self.player.sword.stage = 1;
                self.player.sword.phase = SwordPhase::Windup;
                self.player.sword.t = 0.0;
                self.player.sword.queued = false;
            }
            return;
        }
        self.player.sword.t += dt;
        let stage_index = (self.player.sword.stage - 1) as usize;
        match self.player.sword.phase {
            SwordPhase::Windup => {
                if self.player.sword.t >= SWORD_WINDUP[stage_index] {
                    // 进入横扫：代次自增、命中簿记清零、出刀事件
                    self.player.sword.phase = SwordPhase::Sweep;
                    self.player.sword.t = 0.0;
                    self.player.sword.sweep_id = self.player.sword.sweep_id.wrapping_add(1);
                    self.player.sword.sweep_hit_any = false;
                    let pos = self.player.pos;
                    let facing = self.player.facing;
                    let stage = self.player.sword.stage;
                    self.frame_events
                        .push(AssaultEvent::SwordSwing { pos, facing, stage });
                }
            }
            SwordPhase::Sweep => {
                if self.player.sword.t >= SWORD_SWEEP[stage_index] {
                    // 判定窗关闭：全空 → 落空事件
                    if !self.player.sword.sweep_hit_any {
                        let pos = self.player.pos;
                        let facing = self.player.facing;
                        let stage = self.player.sword.stage;
                        self.frame_events
                            .push(AssaultEvent::SwordWhiff { pos, facing, stage });
                    }
                    self.player.sword.phase = SwordPhase::Recover;
                    self.player.sword.t = 0.0;
                }
            }
            SwordPhase::Recover => {
                if self.player.sword.t >= SWORD_RECOVER[stage_index] {
                    if self.player.sword.queued && self.player.sword.stage < 3 {
                        // 接续下一段（三连段）
                        self.player.sword.stage += 1;
                        self.player.sword.phase = SwordPhase::Windup;
                        self.player.sword.t = 0.0;
                        self.player.sword.queued = false;
                    } else {
                        // 收刀归鞘（保留横扫代次——敌人侧防重键仍单调）
                        let sweep_id = self.player.sword.sweep_id;
                        self.player.sword = SwordState {
                            sweep_id,
                            ..SwordState::default()
                        };
                    }
                }
            }
        }
        // sweep / recover 期的攻击键边沿预约下一段
        if input.sword_press
            && matches!(
                self.player.sword.phase,
                SwordPhase::Sweep | SwordPhase::Recover
            )
            && self.player.sword.stage < 3
        {
            self.player.sword.queued = true;
        }
    }

    /// 刀剑横扫判定：判定窗内的每帧对扇形区域（半径 [`SWORD_REACH`]、
    /// 半角 55°）内的敌人判定——同代次内每名敌人至多命中一次
    /// （`last_sweep_id`）。第三段命中附带击退与（重置型）硬直。
    fn resolve_sword_sweep(&mut self) {
        let sword = self.player.sword;
        if !sword.active() || sword.phase != SwordPhase::Sweep {
            return;
        }
        let fwd = Vec2::new(-self.player.facing.sin(), -self.player.facing.cos());
        let stage_index = (sword.stage - 1) as usize;
        for index in 0..self.enemies.len() {
            let enemy = &self.enemies[index];
            if enemy.state == EnemyState::Dead {
                continue;
            }
            // 同代次已命中过：跳过（wrapping 仅为避免溢出 panic，
            // 2^32 次横扫的回绕概率可忽略）
            if enemy.last_sweep_id == sword.sweep_id {
                continue;
            }
            let to = Vec2::new(
                enemy.pos.x - self.player.pos.x,
                enemy.pos.z - self.player.pos.z,
            );
            let dist = to.length();
            if !(1e-4..=SWORD_REACH).contains(&dist) {
                continue;
            }
            if fwd.dot(to / dist) < SWORD_SECTOR_COS {
                continue;
            }
            // ---- 命中 ----
            let stage = sword.stage;
            let damage = SWORD_DAMAGE[stage_index];
            let killed = enemy.hp - damage <= 0;
            let enemy_pos = enemy.pos;
            let enemy_facing = enemy.facing;
            let _ = enemy; // 借用到此为止，下面开始写回
            self.enemies[index].hp -= damage;
            self.enemies[index].last_sweep_id = sword.sweep_id;
            self.player.sword.sweep_hit_any = true;
            if killed {
                self.enemies[index].state = EnemyState::Dead;
                self.enemies[index].anim_speed = 0.0;
                self.kills += 1;
                let foot = enemy_pos - Vec3::new(0.0, HALF_HEIGHT, 0.0);
                self.frame_events.push(AssaultEvent::EnemyKilled {
                    pos: foot,
                    facing: enemy_facing,
                    enemy: index,
                });
            } else {
                // 硬直：重置型（不叠加）；第三段附带击退
                self.enemies[index].state = EnemyState::Stagger;
                self.enemies[index].t = 0.0;
                self.enemies[index].anim_speed = 0.0;
                if stage == 3 {
                    let dir = to / dist;
                    self.enemies[index].knockback_t = KNOCKBACK_TIME;
                    self.enemies[index].knockback_dir = dir;
                }
                self.frame_events.push(AssaultEvent::EnemyStaggered {
                    pos: enemy_pos,
                    enemy: index,
                });
            }
            self.frame_events.push(AssaultEvent::SwordHit {
                pos: enemy_pos,
                facing: self.player.facing,
                enemy: index,
                stage,
                killed,
            });
        }
    }

    /// 步枪：射速冷却 → 打空自动换弹 → 换弹中禁射 → 开火结算
    /// （静态遮挡 + 敌人 XZ 扫掠）。
    fn resolve_rifle(&mut self, input: &AssaultInput, dt: f32) {
        self.player.rifle.cooldown = (self.player.rifle.cooldown - dt).max(0.0);
        // 打空自动换弹（fire_held 持续按时触发一次）
        if input.fire_held && self.player.rifle.ammo == 0 && self.player.rifle.reload_t <= 0.0 {
            self.player.rifle.reload_t = RIFLE_RELOAD;
            let pos = self.player.pos;
            self.frame_events.push(AssaultEvent::ReloadStart { pos });
            return;
        }
        if !input.fire_held
            || self.player.rifle.cooldown > 0.0
            || self.player.rifle.reload_t > 0.0
            || self.player.rifle.ammo == 0
        {
            return;
        }
        self.player.rifle.cooldown = RIFLE_INTERVAL;
        self.player.rifle.ammo -= 1;
        self.player.rifle.shots += 1;

        // 枪口：胶囊中心 + 朝向前伸 0.45 m + 抬高 0.45 m（肩眼高度）
        let fwd3 = Vec3::new(-self.player.facing.sin(), 0.0, -self.player.facing.cos());
        let muzzle = self.player.pos + fwd3 * 0.45 + Vec3::new(0.0, 0.45, 0.0);
        let dir = fwd3;
        self.frame_events
            .push(AssaultEvent::Shot { pos: muzzle, dir });

        // 静态体最近命中（遮挡基线）
        let end = muzzle + dir * RIFLE_RANGE;
        let mut world_hit: Option<RayHit> = None;
        for &(pos, ref shape) in &self.statics {
            if let Some(hit) = ray_vs_shape(muzzle, dir, RIFLE_RANGE, pos, shape) {
                if world_hit.is_none_or(|prev| hit.t < prev.t) {
                    world_hit = Some(hit);
                }
            }
        }
        let wall_t = world_hit.as_ref().map(|hit| hit.t).unwrap_or(RIFLE_RANGE);

        // 敌人扫掠（XZ 线段-圆；被墙挡住者不会中弹）
        let from_xz = Vec2::new(muzzle.x, muzzle.z);
        let to_xz = Vec2::new(end.x, end.z);
        let mut best: Option<(usize, f32)> = None;
        for (index, enemy) in self.enemies.iter().enumerate() {
            if enemy.state == EnemyState::Dead {
                continue;
            }
            let center_xz = Vec2::new(enemy.pos.x, enemy.pos.z);
            let t = segment_hits_circle_t(from_xz, to_xz, center_xz, RIFLE_HIT_RADIUS);
            if let Some(t) = t {
                if t * RIFLE_RANGE < wall_t && best.is_none_or(|(_, bt)| t < bt) {
                    best = Some((index, t));
                }
            }
        }
        let Some((index, _)) = best else {
            if let Some(hit) = world_hit {
                self.frame_events.push(AssaultEvent::BulletHitWorld {
                    pos: hit.point,
                    normal: hit.normal,
                });
            }
            // 射程内空放：无事件（弹道飞出庭院）
            return;
        };
        let damage = RIFLE_DAMAGE;
        let killed = self.enemies[index].hp - damage <= 0;
        let enemy_pos = self.enemies[index].pos;
        let enemy_facing = self.enemies[index].facing;
        self.enemies[index].hp -= damage;
        if killed {
            self.enemies[index].state = EnemyState::Dead;
            self.enemies[index].anim_speed = 0.0;
            self.kills += 1;
            let foot = enemy_pos - Vec3::new(0.0, HALF_HEIGHT, 0.0);
            self.frame_events.push(AssaultEvent::EnemyKilled {
                pos: foot,
                facing: enemy_facing,
                enemy: index,
            });
        } else {
            // 枪击同样造成（重置型）硬直——火力压制的写实反馈
            self.enemies[index].state = EnemyState::Stagger;
            self.enemies[index].t = 0.0;
            self.enemies[index].anim_speed = 0.0;
            self.frame_events.push(AssaultEvent::EnemyStaggered {
                pos: enemy_pos,
                enemy: index,
            });
        }
        self.frame_events.push(AssaultEvent::BulletHitEnemy {
            pos: enemy_pos,
            enemy: index,
            killed,
        });
    }

    /// 单名敌人的一帧：状态机（局部副本推进 + 事件暂存）→ 击退 / 追击
    /// 移动 → 写回。两阶段模式（收集 → 写回）避免借用冲突，事件顺序
    /// 确定。
    fn update_enemy(&mut self, index: usize, dt: f32) {
        if self.enemies[index].state == EnemyState::Dead {
            self.enemies[index].anim_speed = 0.0;
            return;
        }
        let player_alive = self.player.hp > 0;
        let mut enemy = self.enemies[index].clone(); // Clone：局部副本推进
        enemy.t += dt;
        let dist = self.player.pos.distance(enemy.pos);
        let sees = player_alive && dist <= AGGRO_RADIUS;
        let mut events: Vec<AssaultEvent> = Vec::new();

        // ---- 状态机（读取用移动前的位姿——顺序是语义）----
        match enemy.state {
            EnemyState::Idle => {
                if sees {
                    enemy.state = EnemyState::Chase;
                    enemy.t = 0.0;
                    enemy.path.clear();
                    enemy.path_index = 0;
                    enemy.repath_timer = 0.0;
                }
                enemy.anim_speed = 0.0;
            }
            EnemyState::Chase => {
                if !player_alive {
                    enemy.state = EnemyState::Idle;
                    enemy.t = 0.0;
                } else if dist <= ENEMY_ATTACK_RANGE {
                    enemy.state = EnemyState::Windup;
                    enemy.t = 0.0;
                }
            }
            EnemyState::Windup => {
                // 前摇持续转向玩家（确定性下直接面向）
                let to = Vec2::new(
                    self.player.pos.x - enemy.pos.x,
                    self.player.pos.z - enemy.pos.z,
                );
                if to.length_squared() > 1e-8 {
                    enemy.facing = (-to.x).atan2(-to.y);
                }
                if enemy.t >= ENEMY_WINDUP {
                    enemy.state = EnemyState::Strike;
                    enemy.t = 0.0;
                    enemy.strike_resolved = false;
                }
                enemy.anim_speed = 0.0;
            }
            EnemyState::Strike => {
                // 判定在窗中点一次性完成
                if !enemy.strike_resolved && enemy.t >= ENEMY_STRIKE_WINDOW * 0.5 {
                    enemy.strike_resolved = true;
                    let to = Vec2::new(
                        self.player.pos.x - enemy.pos.x,
                        self.player.pos.z - enemy.pos.z,
                    );
                    let d = to.length();
                    let fwd = Vec2::new(-enemy.facing.sin(), -enemy.facing.cos());
                    if d <= ENEMY_STRIKE_REACH && d > 1e-4 && fwd.dot(to / d) >= ENEMY_STRIKE_COS {
                        self.player.hp -= ENEMY_DAMAGE;
                        self.player.hurt_flash = 0.25;
                        events.push(AssaultEvent::PlayerHurt {
                            pos: self.player.pos,
                            hp: self.player.hp,
                        });
                    }
                    events.push(AssaultEvent::EnemyStruck {
                        pos: enemy.pos,
                        enemy: index,
                    });
                }
                if enemy.t >= ENEMY_STRIKE_WINDOW {
                    enemy.state = EnemyState::Recover;
                    enemy.t = 0.0;
                }
                enemy.anim_speed = 0.0;
            }
            EnemyState::Recover => {
                if enemy.t >= ENEMY_RECOVER {
                    enemy.state = EnemyState::Chase;
                    enemy.t = 0.0;
                    enemy.path.clear();
                    enemy.path_index = 0;
                    enemy.repath_timer = 0.0;
                }
                enemy.anim_speed = 0.0;
            }
            EnemyState::Stagger => {
                if enemy.t >= ENEMY_STAGGER {
                    enemy.state = EnemyState::Chase;
                    enemy.t = 0.0;
                    enemy.path.clear();
                    enemy.path_index = 0;
                    enemy.repath_timer = 0.0;
                }
                enemy.anim_speed = 0.0;
            }
            EnemyState::Dead => unreachable!("死亡敌人在入口已返回"),
        }

        // ---- 击退（优先于追击：击退期间不追击）----
        let mut vel = Vec3::ZERO;
        let knock_speed = if enemy.knockback_t > 0.0 {
            let remaining = enemy.knockback_t;
            enemy.knockback_t = (remaining - dt).max(0.0);
            // 线性衰减：v = KNOCKBACK_SPEED × (剩余 / 总时长)
            KNOCKBACK_SPEED * (remaining / KNOCKBACK_TIME)
        } else {
            0.0
        };
        if knock_speed > 0.0 {
            vel.x = enemy.knockback_dir.x * knock_speed;
            vel.z = enemy.knockback_dir.y * knock_speed;
            enemy.anim_speed = knock_speed;
        } else if enemy.state == EnemyState::Chase {
            // ---- 追击移动：航点队列（find_path 节拍重算）或直线贴脸 ----
            if enemy.path_index >= enemy.path.len() {
                enemy.repath_timer -= dt;
                if enemy.repath_timer <= 0.0 {
                    let from = Vec3::new(enemy.pos.x, 0.0, enemy.pos.z);
                    let to = Vec3::new(self.player.pos.x, 0.0, self.player.pos.z);
                    if let Some(path) = self.nav.find_path(from, to) {
                        enemy.path = path;
                        enemy.path_index = 0;
                    }
                    enemy.repath_timer = REPATH_INTERVAL;
                }
            }
            if let Some(&wp) = enemy.path.get(enemy.path_index) {
                let target = Vec2::new(wp.x, wp.z);
                let pos = Vec2::new(enemy.pos.x, enemy.pos.z);
                let to = target - pos;
                let d = to.length();
                let step = ENEMY_SPEED * dt;
                if d <= WAYPOINT_REACH.max(step) {
                    enemy.path_index += 1;
                    enemy.anim_speed = 0.0;
                } else {
                    let dir = to / d;
                    vel.x = dir.x * ENEMY_SPEED;
                    vel.z = dir.y * ENEMY_SPEED;
                    enemy.anim_speed = ENEMY_SPEED;
                    enemy.facing = (-dir.x).atan2(-dir.y);
                }
            } else {
                // 无航点：直接朝玩家走（近距离 / 导航图全通的等价路径）
                let to = Vec2::new(
                    self.player.pos.x - enemy.pos.x,
                    self.player.pos.z - enemy.pos.z,
                );
                if to.length_squared() > 1e-8 {
                    let dir = to.normalize_or_zero();
                    vel.x = dir.x * ENEMY_SPEED;
                    vel.z = dir.y * ENEMY_SPEED;
                    enemy.anim_speed = ENEMY_SPEED;
                    enemy.facing = (-to.x).atan2(-to.y);
                }
            }
        }
        if vel.length_squared() > 0.0 {
            let out = move_and_slide_statics(enemy.pos, vel, dt, &CHARACTER, &self.statics);
            enemy.pos = out.position;
        }

        // ---- 写回 ----
        self.enemies[index] = enemy;
        self.frame_events.extend(events);
    }

    /// 分离推挤：敌人之间 / 敌人与玩家之间的最小间距（推敌人不推玩家；
    /// 下标序遍历，确定性）。位置直接写（推挤量每帧 ≤ 数厘米，贴墙语义
    /// 由控制器自身的滑行保证，此处只防堆叠）。
    fn resolve_separation(&mut self) {
        let count = self.enemies.len();
        for i in 0..count {
            if self.enemies[i].state == EnemyState::Dead {
                continue;
            }
            // 敌 vs 玩家
            let to = Vec2::new(
                self.enemies[i].pos.x - self.player.pos.x,
                self.enemies[i].pos.z - self.player.pos.z,
            );
            let dist = to.length();
            if dist < SEPARATION_DIST && dist > 1e-4 {
                let push = (SEPARATION_DIST - dist) * 0.5;
                let dir = to / dist;
                self.enemies[i].pos.x += dir.x * push;
                self.enemies[i].pos.z += dir.y * push;
            }
            // 敌 vs 敌（只推后者，单遍 O(n²)）
            for j in (i + 1)..count {
                if self.enemies[j].state == EnemyState::Dead {
                    continue;
                }
                let to = Vec2::new(
                    self.enemies[j].pos.x - self.enemies[i].pos.x,
                    self.enemies[j].pos.z - self.enemies[i].pos.z,
                );
                let dist = to.length();
                if dist < SEPARATION_DIST && dist > 1e-4 {
                    let push = (SEPARATION_DIST - dist) * 0.5;
                    let dir = to / dist;
                    self.enemies[i].pos.x -= dir.x * push;
                    self.enemies[i].pos.z -= dir.y * push;
                    self.enemies[j].pos.x += dir.x * push;
                    self.enemies[j].pos.z += dir.y * push;
                }
            }
        }
    }
}

/// XZ 平面线段-圆扫掠的参数化版本：命中返回**归一化参数** t ∈ [0, 1]
/// （命中点 = `from + (to − from) × t`；与 bastion 的布尔版本同式，
/// 额外给出 t 供「最近的敌人赢」比较——t × 线段长即世界距离）。
fn segment_hits_circle_t(from: Vec2, to: Vec2, center: Vec2, radius: f32) -> Option<f32> {
    let d = to - from;
    let len_sq = d.length_squared();
    if len_sq <= 1e-12 {
        // 零长度线段退化为点判定
        return if from.distance_squared(center) <= radius * radius {
            Some(0.0)
        } else {
            None
        };
    }
    let t = ((center - from).dot(d) / len_sq).clamp(0.0, 1.0);
    let closest = from + d * t;
    if closest.distance_squared(center) <= radius * radius {
        Some(t)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// 自动玩家（headless 测试与纯核心单测共用的确定性脚本）
// ---------------------------------------------------------------------------

/// 脚本化自动玩家：接近最近敌人（NavGrid 绕喷泉）→ 刀距内挥刀三连段 →
/// 中距切步枪补射 → 弹尽换弹 → 循环；HP < 30 改拉距射击。决策只读当前
/// 帧状态（无隐藏随机源、无跨帧簿记——find_path 每帧重算，庭院网格
/// 42×30 cell 的 A* 在 60 Hz 下可忽略），同输入两次模拟逐位一致。
#[derive(Clone, Copy, Debug, Default)]
pub struct AutoPilot;

impl AutoPilot {
    /// 自动玩家的固定步长（与 headless 测试的 60 Hz 一致）。
    const DT: f32 = 1.0 / 60.0;

    /// 由当前状态产出本帧输入。
    pub fn drive(&self, state: &mut AssaultState) -> AssaultInput {
        let mut input = AssaultInput {
            dt: Self::DT,
            ..AssaultInput::default()
        };
        if state.phase != Phase::Playing {
            return input;
        }
        let Some((target_index, target_dist)) = state.nearest_enemy() else {
            return input;
        };
        let target_pos = state.enemies[target_index].pos;
        let target_xz = Vec2::new(target_pos.x, target_pos.z);
        let pos = Vec2::new(state.player.pos.x, state.player.pos.z);

        let hp_low = state.player.hp < 30;
        let sword_active = state.player.sword.active();
        let can_chain = matches!(
            state.player.sword.phase,
            SwordPhase::Sweep | SwordPhase::Recover
        );
        let reloading = state.player.rifle.reload_t > 0.0;
        let ammo_empty = state.player.rifle.ammo == 0;
        let ready = state.player.rifle.cooldown <= 0.0;

        // 刀距（且血量健康）：优先近战三连段
        if target_dist <= 1.9 && !hp_low {
            if !sword_active || can_chain {
                input.sword_press = true;
            }
            return input;
        }
        // 挥刀动作中：让连段走完，同时小幅逼近
        if sword_active {
            input.move_dir = Self::step_toward(state, pos, target_xz, 1.6);
            if can_chain {
                input.sword_press = true;
            }
            return input;
        }
        // 中距且枪有弹：瞄准射击（瞄准自动锁敌面向）
        if !ammo_empty && ready && !reloading && target_dist <= 8.0 {
            input.aim_held = true;
            input.fire_held = true;
            return input;
        }
        // 弹尽且不贴脸：换弹 + 拉距离
        if ammo_empty && !reloading && target_dist > 2.4 {
            input.reload_press = true;
        }
        // 走位：血量健康时贴近到刀距，血量低 / 换弹时拉开
        let want_dist = if hp_low || reloading || ammo_empty {
            5.5
        } else {
            1.4
        };
        let goal = if target_dist > want_dist {
            target_xz
        } else {
            // 太近且不近战：拉开方向（远离敌人 3 m 的点）
            pos + (pos - target_xz).normalize_or_zero() * 3.0
        };
        input.move_dir = Self::step_toward(state, pos, goal, 0.0);
        input
    }

    /// 朝目标走一步的方向：经导航图寻路取下一航点（喷泉 / 石柱绕行由
    /// NavGrid 保证）；寻路失败（不可达）退化为直线方向。
    fn step_toward(state: &AssaultState, pos: Vec2, goal: Vec2, arrive: f32) -> Vec2 {
        if pos.distance(goal) <= arrive {
            return Vec2::ZERO;
        }
        let from = Vec3::new(pos.x, 0.0, pos.y);
        let to = Vec3::new(goal.x, 0.0, goal.y);
        state
            .nav
            .find_path(from, to)
            .and_then(|path| path.get(1).or_else(|| path.first()).copied())
            .map(|wp| Vec2::new(wp.x, wp.z) - pos)
            .map(|d| d.normalize_or_zero())
            .unwrap_or_else(|| (goal - pos).normalize_or_zero())
    }
}

/// 以固定步长推进自动玩家直到 `done` 返回真或帧数封顶，返回推进帧数。
/// headless 测试与纯核心单测共用（同一推进路径，零偏离）。
pub fn run_until(
    state: &mut AssaultState,
    done: impl Fn(&AssaultState) -> bool,
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

    const DT: f32 = 1.0 / 60.0;
    /// 常用容差：60Hz 步长下的浮点积累误差量级。
    const EPS: f32 = 1e-3;

    /// 输入速查。
    fn input(move_dir: Vec2, sword_press: bool, fire_held: bool, aim_held: bool) -> AssaultInput {
        AssaultInput {
            move_dir,
            sword_press,
            fire_held,
            aim_held,
            reload_press: false,
            dt: DT,
        }
    }

    /// 静止输入（空推）。
    fn rest() -> AssaultInput {
        input(Vec2::ZERO, false, false, false)
    }

    /// 开局并进入战斗（跳过开场演出）。
    fn battle() -> AssaultState {
        let mut state = AssaultState::new();
        state.begin_battle();
        assert_eq!(state.phase, Phase::Playing);
        state
    }

    /// 把全部敌人挪到庭院东缘远角（武器 / 移动单测的隔离手段）：远离
    /// 玩家的警觉半径（9 m）保持待机——**保持存活**：标记死亡会立刻
    /// 触发全歼胜利冻结（敌人必须参战存在，逻辑才不终局）。
    fn enemies_away(state: &mut AssaultState) {
        let corners = [
            Vec2::new(9.3, 6.3),
            Vec2::new(9.3, -6.3),
            Vec2::new(8.0, 6.3),
            Vec2::new(8.0, -6.3),
        ];
        for (index, enemy) in state.enemies.iter_mut().enumerate() {
            enemy.pos = Vec3::new(corners[index].x, HALF_HEIGHT, corners[index].y);
            enemy.state = EnemyState::Idle;
            enemy.t = 0.0;
            enemy.knockback_t = 0.0;
            enemy.path.clear();
            enemy.path_index = 0;
        }
    }

    /// 把玩家钉在 `foot`（脚底）处、面向 `facing`。
    fn pin_player(state: &mut AssaultState, foot: Vec3, facing: f32) {
        state.player.pos = foot + Vec3::new(0.0, HALF_HEIGHT, 0.0);
        state.player.facing = facing;
        state.player.vel = Vec3::ZERO;
    }

    /// 把敌人 `index` 钉在 `foot`（脚底）处、状态就绪（追击）。
    fn pin_enemy(state: &mut AssaultState, index: usize, foot: Vec3) {
        let enemy = &mut state.enemies[index];
        enemy.pos = foot + Vec3::new(0.0, HALF_HEIGHT, 0.0);
        enemy.state = EnemyState::Chase;
        enemy.t = 0.0;
        enemy.knockback_t = 0.0;
        enemy.path.clear();
        enemy.path_index = 0;
        enemy.last_sweep_id = 0;
    }

    /// 静止推进 `seconds` 秒。
    fn idle(state: &mut AssaultState, seconds: f32) {
        let steps = (seconds / DT).round() as u32;
        for _ in 0..steps {
            state.step(&rest());
        }
    }

    /// 点（XZ）到 AABB 的 XZ 最近点距离（测试自持，供布局不变量断言）。
    fn xz_clearance(point: Vec2, center: Vec3, half: Vec3) -> f32 {
        let ox = (point.x - center.x).abs() - half.x;
        let oz = (point.y - center.z).abs() - half.z;
        (ox.max(0.0) * ox.max(0.0) + oz.max(0.0) * oz.max(0.0)).sqrt()
    }

    // ---- 关卡数据不变量 ----

    #[test]
    fn level_invariants_hold() {
        // 出生点远离一切静态体
        for block in WALLS.iter().chain(PILLARS.iter()) {
            assert!(
                xz_clearance(
                    Vec2::new(SPAWN_FOOT.x, SPAWN_FOOT.z),
                    block.center,
                    block.half
                ) > 1.0,
                "出生点应离墙/柱至少 1 m"
            );
        }
        // 敌人出生点也不在任何静态体膨胀域内
        for spawn in ENEMY_SPAWNS.iter() {
            for block in WALLS.iter().chain(PILLARS.iter()) {
                assert!(
                    xz_clearance(*spawn, block.center, block.half) > NAV_AGENT_RADIUS,
                    "敌人出生点 {spawn:?} 不应落在静态体 {block:?} 的膨胀域内"
                );
            }
            let fountain_dist = spawn.length();
            assert!(
                fountain_dist > 1.8 + NAV_AGENT_RADIUS,
                "敌人出生点 {spawn:?} 不应落在喷泉膨胀域内（距 {fountain_dist}）"
            );
        }
        // 石柱不与喷泉重叠
        for pillar in PILLARS.iter() {
            let center_dist = Vec2::new(pillar.center.x, pillar.center.z).length();
            assert!(
                center_dist > 1.8 + pillar.half.x,
                "石柱 {pillar:?} 不应与喷泉重叠（中心距 {center_dist}）"
            );
        }
    }

    #[test]
    fn nav_reachability_across_courtyard() {
        let state = AssaultState::new();
        let at = |v: Vec2| Vec3::new(v.x, 0.0, v.y);
        // 庭院四角从玩家出生点全部可达
        for corner in [
            Vec2::new(-9.0, -6.0),
            Vec2::new(9.0, -6.0),
            Vec2::new(-9.0, 6.0),
            Vec2::new(9.0, 6.0),
        ] {
            assert!(
                state
                    .nav
                    .is_reachable(at(Vec2::new(SPAWN_FOOT.x, SPAWN_FOOT.z)), at(corner)),
                "角落 {corner:?} 应可达"
            );
        }
        for spawn in ENEMY_SPAWNS.iter() {
            assert!(
                state
                    .nav
                    .is_reachable(at(Vec2::new(SPAWN_FOOT.x, SPAWN_FOOT.z)), at(*spawn)),
                "敌人出生点 {spawn:?} 应可达"
            );
        }
    }

    /// 喷泉确实挡路：喷泉正东西两点的寻路必须绕行（航点不进喷泉域、
    /// 路径有偏离直线的折点）——NavGrid 绕行价值的量化断言。
    #[test]
    fn nav_paths_detour_around_fountain() {
        let state = AssaultState::new();
        let from = Vec3::new(-7.0, 0.0, 0.0);
        let to = Vec3::new(7.0, 0.0, 0.0);
        let path = state.nav.find_path(from, to).expect("庭院东西两点应连通");
        assert!(path.len() >= 2, "路径应至少含起点与终点");
        for wp in &path {
            let clear = Vec2::new(wp.x, wp.z).length();
            assert!(
                clear > 1.8 || wp.x.abs() > 1.8,
                "航点 {wp:?} 不应落在喷泉碰撞域内"
            );
        }
        assert!(
            path.iter().any(|wp| wp.z.abs() > 0.1),
            "绕喷泉路径应有偏离直线的折点"
        );
    }

    // ---- 玩家移动（引擎角色控制器 / 贴墙滑行 / 瞄准减速）----

    #[test]
    fn player_walks_slides_and_aims_slower() {
        let mut state = battle();
        enemies_away(&mut state);
        // 东行 1 秒 ≈ 4.4 m
        for _ in 0..60 {
            state.step(&input(Vec2::new(1.0, 0.0), false, false, false));
        }
        assert!(
            (state.player.pos.x - (SPAWN_FOOT.x + 4.4)).abs() < 0.3,
            "1 秒东行应走约 4.4 m，实际 x = {}",
            state.player.pos.x
        );
        // 北顶围墙：墙内缘 z = -6.8，胶囊停在 -6.8 + 0.4 = -6.4
        for _ in 0..120 {
            state.step(&input(Vec2::new(0.0, -1.0), false, false, false));
        }
        assert!(
            (state.player.pos.z - (-6.4)).abs() < 0.05,
            "北行应停在北墙前，实际 z = {}",
            state.player.pos.z
        );
        // 瞄准减速：速度 = AIM_SPEED
        let mut state = battle();
        enemies_away(&mut state);
        for _ in 0..60 {
            state.step(&input(Vec2::new(1.0, 0.0), false, false, true));
        }
        assert!(
            (state.player.vel.x - AIM_SPEED).abs() < EPS,
            "瞄准东行速度应 = {AIM_SPEED}，实际 {}",
            state.player.vel.x
        );
        assert!(state.player.aiming, "瞄准标志应随输入置位");
    }

    // ---- 刀剑三连段：扇形判定 / 越界不伤 / 连段预约 ----

    #[test]
    fn sword_sweep_hits_only_enemies_in_sector() {
        let mut state = battle();
        // 敌人 0 在玩家正前 2 m（扇区内，且是起手锁敌的最近者），敌人 1
        // 在侧后 2.5 m（扇区外），敌人 2 在侧前 2.79 m（扇区外 + 判定
        // 半径外）；布点全部避开喷泉碰撞域（球心距 > 2.2 m）
        pin_player(&mut state, Vec3::new(0.0, 0.0, -3.5), 0.0); // 面朝 -Z
        pin_enemy(&mut state, 0, Vec3::new(0.0, 0.0, -5.5));
        pin_enemy(&mut state, 1, Vec3::new(2.0, 0.0, -2.0));
        pin_enemy(&mut state, 2, Vec3::new(-2.6, 0.0, -4.5));
        state.enemies[3].state = EnemyState::Dead;

        // 起手（第一段）+ 推进整段
        state.step(&input(Vec2::ZERO, true, false, false));
        idle(
            &mut state,
            SWORD_WINDUP[0] + SWORD_SWEEP[0] + SWORD_RECOVER[0] + 0.1,
        );

        assert_eq!(
            state.enemies[0].hp,
            ENEMY_MAX_HP - SWORD_DAMAGE[0],
            "扇区内敌人应中刀"
        );
        assert_eq!(
            state.enemies[1].hp, ENEMY_MAX_HP,
            "侧后敌人不应中刀（越界不伤）"
        );
        assert_eq!(
            state.enemies[2].hp, ENEMY_MAX_HP,
            "侧前敌人不应中刀（越界不伤）"
        );
        // 第一段命中过 → 不应有落空事件
        assert!(
            !state
                .frame_events
                .iter()
                .any(|e| matches!(e, AssaultEvent::SwordWhiff { stage: 1, .. })),
            "第一段命中过，不应产生 SwordWhiff"
        );
    }

    #[test]
    fn sword_combo_chains_and_third_stage_kills_with_knockback() {
        let mut state = battle();
        // 单个敌人正前 2 m，其余放逐
        enemies_away(&mut state);
        pin_player(&mut state, Vec3::new(0.0, 0.0, -3.5), 0.0);
        pin_enemy(&mut state, 0, Vec3::new(0.0, 0.0, -5.5));

        // 每帧按住攻击键（预约连段）：三段应连续接续
        let mut stages_seen = [false; 4];
        let mut knocked = false;
        for _ in 0..(60 * 4) {
            state.step(&input(Vec2::ZERO, true, false, false));
            for event in &state.frame_events {
                if let AssaultEvent::SwordSwing { stage, .. } = *event {
                    stages_seen[stage as usize] = true;
                }
                if let AssaultEvent::SwordHit { stage, .. } = *event {
                    if stage == 3 {
                        knocked = state.enemies[0].knockback_t > 0.0
                            || state.enemies[0].state == EnemyState::Dead;
                    }
                }
            }
            if state.enemies[0].state == EnemyState::Dead {
                break;
            }
        }
        assert!(
            stages_seen[1] && stages_seen[2] && stages_seen[3],
            "三段都应出刀（实际 {stages_seen:?}）"
        );
        // 25 + 25 + 40 = 90 ≥ 60：第三段应击杀
        assert_eq!(
            state.enemies[0].state,
            EnemyState::Dead,
            "三连段应击杀敌人（HP 60）"
        );
        assert_eq!(state.kills, 1);
        assert!(knocked, "第三段命中时敌人应处于击退中或已死");
    }

    /// 同一次横扫内敌人至多被打一次：判定窗跨多帧，敌人 HP 只扣一次。
    #[test]
    fn sweep_hits_enemy_once_per_sweep() {
        let mut state = battle();
        enemies_away(&mut state);
        pin_player(&mut state, Vec3::new(0.0, 0.0, -3.5), 0.0);
        pin_enemy(&mut state, 0, Vec3::new(0.0, 0.0, -5.5));
        // 起手 + 只推进前摇 + 一半判定窗（横扫窗内多帧）
        state.step(&input(Vec2::ZERO, true, false, false));
        let steps = ((SWORD_WINDUP[0] + SWORD_SWEEP[0] * 0.5) / DT) as u32;
        for _ in 1..steps {
            state.step(&rest());
        }
        assert_eq!(
            state.enemies[0].hp,
            ENEMY_MAX_HP - SWORD_DAMAGE[0],
            "判定窗内多帧只应命中一次"
        );
        let hits = state
            .frame_events
            .iter()
            .filter(|e| matches!(e, AssaultEvent::SwordHit { enemy: 0, .. }))
            .count();
        assert!(hits <= 1, "单帧至多一条 SwordHit 事件（实际 {hits}）");
    }

    // ---- 步枪：射速 / 弹匣 / 换弹时序 / 弹道遮挡 ----

    #[test]
    fn rifle_rate_magazine_and_reload_timing() {
        let mut state = battle();
        enemies_away(&mut state);
        pin_player(&mut state, Vec3::new(0.0, 0.0, -3.5), 0.0);
        // 持续开火：射速 0.16 s/发，弹匣 12 发 → ~1.92 s 后打空并自动换弹
        let mut reload_started_at: Option<usize> = None;
        for frame in 0..(60 * 4) {
            state.step(&input(Vec2::ZERO, false, true, false));
            if state.player.rifle.reload_t > 0.0 && reload_started_at.is_none() {
                reload_started_at = Some(frame);
                break;
            }
        }
        let start_frame = reload_started_at.expect("4 秒内应触发一次自动换弹");
        // 打空时刻 ≈ 第 12 发（射速 0.16）+ 1 帧
        assert!(
            (start_frame as f32 * DT - (RIFLE_MAG as f32 - 1.0) * RIFLE_INTERVAL).abs() < 0.3,
            "打空时刻应 ≈ (弹匣-1) × 射速 = {:.2} s，实际 {:.2} s",
            (RIFLE_MAG as f32 - 1.0) * RIFLE_INTERVAL,
            start_frame as f32 * DT
        );
        // 换弹全程不能开火（计数只算「整帧都处于换弹」的帧——换弹完成
        // 当帧的立即开火是合法行为：reload_t 在 update_player 归零后，
        // 同帧 resolve_rifle 已允许射击）
        let mut shots_in_reload = 0usize;
        while state.player.rifle.reload_t > 0.0 {
            let was_reloading = state.player.rifle.reload_t;
            state.step(&input(Vec2::ZERO, false, true, false));
            if was_reloading > 0.0 && state.player.rifle.reload_t > 0.0 {
                shots_in_reload += state
                    .frame_events
                    .iter()
                    .filter(|e| matches!(e, AssaultEvent::Shot { .. }))
                    .count();
            }
        }
        assert_eq!(shots_in_reload, 0, "换弹中不能开火");
        assert_eq!(state.player.rifle.reload_t, 0.0, "换弹应恰好走完");
        // 换弹完成帧弹匣回满（12），同帧立即开火消耗 1 发（合法——
        // reload_t 归零后 resolve_rifle 即可射击）
        assert_eq!(
            state.player.rifle.ammo,
            RIFLE_MAG - 1,
            "换弹完成应回满弹匣再被完成帧的首发消耗 1 发"
        );
    }

    #[test]
    fn rifle_cannot_shoot_through_fountain() {
        let mut state = battle();
        // 玩家在 (-4, 0) 面朝 +X（facing = -π/2）；喷泉（球体半径 1.8
        // @ 原点）挡在弹道上。枪口 y 1.35：过球心的水平偏 0.85 →
        // 半弦长 √(1.8² − 0.85²) ≈ 1.587，命中点 x ≈ −1.587
        enemies_away(&mut state);
        pin_player(
            &mut state,
            Vec3::new(-4.0, 0.0, 0.0),
            -std::f32::consts::FRAC_PI_2,
        );
        state.step(&input(Vec2::ZERO, false, true, false));
        let hit = state
            .frame_events
            .iter()
            .find_map(|e| match *e {
                AssaultEvent::BulletHitWorld { pos, normal } => Some((pos, normal)),
                _ => None,
            })
            .expect("穿喷泉弹道应命中静态体");
        assert!(
            (hit.0.x - (-1.587)).abs() < 0.05,
            "命中点应在喷泉西缘（x ≈ -1.587），实际 {:?}",
            hit.0
        );
        assert!(
            hit.1.x < -0.8,
            "外法线应指向弹道来侧（-X），实际 {:?}",
            hit.1
        );
        assert!(
            !state
                .frame_events
                .iter()
                .any(|e| matches!(e, AssaultEvent::BulletHitEnemy { .. })),
            "被墙挡住的弹道不应命中任何敌人"
        );
    }

    // ---- 换弹请求（边沿 + 刀动作互斥）----

    #[test]
    fn manual_reload_needs_idle_sword_and_takes_fixed_time() {
        let mut state = battle();
        enemies_away(&mut state);
        pin_player(&mut state, Vec3::new(0.0, 0.0, -3.5), 0.0);
        state.player.rifle.ammo = 5;
        // 挥刀动作未结束期间请求换弹：被忽略
        state.step(&input(Vec2::ZERO, true, false, false));
        state.step(&AssaultInput {
            move_dir: Vec2::ZERO,
            sword_press: false,
            fire_held: false,
            aim_held: false,
            reload_press: true,
            dt: DT,
        });
        assert!(state.player.sword.active(), "挥刀动作应仍在进行（前摇期）");
        assert_eq!(state.player.rifle.reload_t, 0.0, "挥刀中换弹请求应被忽略");
        // 刀动作结束后请求：生效 + 固定时长
        idle(
            &mut state,
            SWORD_WINDUP[0] + SWORD_SWEEP[0] + SWORD_RECOVER[0] + 0.1,
        );
        assert!(!state.player.sword.active(), "挥刀应已结束");
        state.step(&AssaultInput {
            move_dir: Vec2::ZERO,
            sword_press: false,
            fire_held: false,
            aim_held: false,
            reload_press: true,
            dt: DT,
        });
        assert!(
            (state.player.rifle.reload_t - RIFLE_RELOAD).abs() < EPS,
            "换弹开始应为满时长 {RIFLE_RELOAD}，实际 {}",
            state.player.rifle.reload_t
        );
        assert!(
            state
                .frame_events
                .iter()
                .any(|e| matches!(e, AssaultEvent::ReloadStart { .. })),
            "换弹开始应有事件"
        );
        idle(&mut state, RIFLE_RELOAD + 0.1);
        assert_eq!(state.player.rifle.ammo, RIFLE_MAG, "换弹完成应满弹匣");
    }

    // ---- 敌人：追击 / 挥砍判定 / 硬直不叠加 ----

    #[test]
    fn enemy_chases_attacks_and_deals_damage() {
        let mut state = battle();
        enemies_away(&mut state);
        pin_player(&mut state, Vec3::new(0.0, 0.0, -3.5), 0.0);
        pin_enemy(&mut state, 0, Vec3::new(0.0, 0.0, -6.9));
        // 3.4 m 追击（喷泉外直线）+ 前摇 + 判定：3 秒内应至少挨一刀
        let mut hurt = false;
        for _ in 0..(60 * 3) {
            state.step(&rest());
            if state
                .frame_events
                .iter()
                .any(|e| matches!(e, AssaultEvent::PlayerHurt { .. }))
            {
                hurt = true;
                break;
            }
        }
        assert!(hurt, "敌人追上后应砍中玩家");
        assert_eq!(state.player.hp, PLAYER_MAX_HP - ENEMY_DAMAGE);
        // 敌人挥砍走完前摇 + 窗 + 后摇：应回到追击
        idle(
            &mut state,
            ENEMY_WINDUP + ENEMY_STRIKE_WINDOW + ENEMY_RECOVER + 0.1,
        );
        assert_eq!(state.enemies[0].state, EnemyState::Chase);
    }

    /// 挥砍判定窗的扇形语义：玩家在敌人**背后**（前摇结束瞬间瞬移到
    /// 背后——判定窗内不重新转向）应落空。
    #[test]
    fn enemy_strike_misses_out_of_sector() {
        let mut state = battle();
        enemies_away(&mut state);
        pin_enemy(&mut state, 0, Vec3::new(0.0, 0.0, -3.5));
        state.enemies[0].state = EnemyState::Windup;
        state.enemies[0].t = ENEMY_WINDUP - DT; // 下一帧进判定窗
        // 前摇最后一帧：玩家在正前 1.5 m（敌人转向玩家）
        pin_player(&mut state, Vec3::new(0.0, 0.0, -5.0), 0.0);
        state.step(&rest());
        assert_eq!(state.enemies[0].state, EnemyState::Strike, "前摇应结束");
        // 判定窗内瞬移到敌人背后（前摇已结束，朝向不再更新）
        pin_player(&mut state, Vec3::new(0.0, 0.0, -2.0), 0.0);
        idle(&mut state, ENEMY_STRIKE_WINDOW + 0.1);
        assert_eq!(
            state.player.hp, PLAYER_MAX_HP,
            "判定窗内绕后应落空（扇形 60°）"
        );
    }

    /// 被击硬直不叠加：硬直中再被击，计时**重置**为 0 而非续算。
    #[test]
    fn stagger_resets_not_accumulates() {
        let mut state = battle();
        enemies_away(&mut state);
        pin_player(&mut state, Vec3::new(0.0, 0.0, -3.5), 0.0);
        pin_enemy(&mut state, 0, Vec3::new(0.0, 0.0, -5.5));
        // 第一刀（完整一段：起手 + 前摇 + 判定窗）
        state.step(&input(Vec2::ZERO, true, false, false));
        idle(&mut state, SWORD_WINDUP[0] + SWORD_SWEEP[0] + 0.02);
        assert_eq!(state.enemies[0].state, EnemyState::Stagger, "第一刀应硬直");
        // 硬直计时推到只剩 0.05 s
        state.enemies[0].t = ENEMY_STAGGER - 0.05;
        state.enemies[0].hp = ENEMY_MAX_HP * 2; // 防击杀干扰
        // 第二刀：起手 + 前摇 + 判定（第一段硬直早该结束了——0.05 s 后）。
        // 重置刀状态时显式给一个全新的横扫代次（default 的 0 会在下一段
        // 横扫开始时 +1 回绕到 1，与敌人 last_sweep_id 撞车——防重复键
        // 会把这一刀整段跳过）
        state.player.sword = SwordState {
            sweep_id: 100,
            ..SwordState::default()
        };
        state.step(&input(Vec2::ZERO, true, false, false));
        let mut hit_frame_t = None;
        for _ in 0..40 {
            state.step(&rest());
            if state
                .frame_events
                .iter()
                .any(|e| matches!(e, AssaultEvent::SwordHit { enemy: 0, .. }))
            {
                hit_frame_t = Some(state.enemies[0].t);
                break;
            }
        }
        // 命中帧计时：命中帧内 update_enemy 在判定之后又推进了一步，
        // 故重置语义下 t = dt；若是续算，t 应 ≥ 0.30 + 前摇 0.18 且敌人
        // 早已回追击（状态断言先拦）
        let t = hit_frame_t.expect("第二刀应命中");
        assert_eq!(state.enemies[0].state, EnemyState::Stagger);
        assert!(
            t <= 1.5 * DT,
            "硬直中再被击应重置计时（命中帧 t = {t}，不叠加）"
        );
        // 重置后应走满整段硬直才回追击
        idle(&mut state, ENEMY_STAGGER - t - 0.02);
        assert_eq!(
            state.enemies[0].state,
            EnemyState::Stagger,
            "重置后应走满整段硬直"
        );
    }

    // ---- 击退 ----

    #[test]
    fn knockback_pushes_enemy_away_and_decays() {
        let mut state = battle();
        enemies_away(&mut state);
        pin_player(&mut state, Vec3::new(0.0, 0.0, -3.5), 0.0);
        pin_enemy(&mut state, 0, Vec3::new(0.0, 0.0, -4.6));
        // 直接注入击退（第三段命中的效果）：朝 -Z 推（远离玩家；-4.6 -
        // 1.2 = -5.8，仍在北墙内缘 -6.8 之外——不截断位移）
        state.enemies[0].knockback_t = KNOCKBACK_TIME;
        state.enemies[0].knockback_dir = Vec2::new(0.0, -1.0);
        let start = state.enemies[0].pos;
        idle(&mut state, KNOCKBACK_TIME);
        let moved = (state.enemies[0].pos - start).length();
        // 理论位移 = 初速 × 时长 / 2 = 8 × 0.3 / 2 = 1.2 m（击退期间
        // 不追击——纯击退位移）
        assert!(
            (moved - KNOCKBACK_SPEED * KNOCKBACK_TIME * 0.5).abs() < 0.15,
            "击退位移应 ≈ 1.2 m（实际 {moved}）"
        );
        assert_eq!(state.enemies[0].knockback_t, 0.0, "击退应衰减归零");
    }

    // ---- 胜负 ----

    #[test]
    fn all_enemies_dead_triggers_victory_and_freezes() {
        let mut state = battle();
        for enemy in state.enemies.iter_mut() {
            enemy.hp = 0;
            enemy.state = EnemyState::Dead;
        }
        state.kills = 4;
        state.step(&rest());
        assert_eq!(state.phase, Phase::Victory);
        assert!(
            matches!(
                state.frame_events.first(),
                Some(AssaultEvent::Victory { .. })
            ),
            "全歼帧应有 Victory 事件"
        );
        let snapshot = (state.player.pos, state.elapsed);
        state.step(&input(Vec2::new(1.0, 0.0), true, true, true));
        assert_eq!((state.player.pos, state.elapsed), snapshot, "终局应冻结");
    }

    #[test]
    fn player_death_triggers_defeat_and_freezes() {
        let mut state = battle();
        state.player.hp = ENEMY_DAMAGE; // 下一刀致死
        enemies_away(&mut state);
        pin_player(&mut state, Vec3::new(0.0, 0.0, -3.5), 0.0);
        pin_enemy(&mut state, 0, Vec3::new(0.0, 0.0, -5.0));
        state.enemies[0].state = EnemyState::Windup;
        state.enemies[0].facing = std::f32::consts::PI; // 面朝 +Z（玩家方向）
        state.enemies[0].t = ENEMY_WINDUP - DT; // 下一帧进判定窗
        let mut defeated = false;
        for _ in 0..120 {
            state.step(&rest());
            if state.phase == Phase::Defeat {
                defeated = true;
                break;
            }
        }
        assert!(defeated, "最后一刀应触发败北");
        assert!(state.player.hp <= 0, "败北时 HP 应归零");
        let snapshot = state.elapsed;
        idle(&mut state, 1.0);
        assert_eq!(state.elapsed, snapshot, "败北后应冻结");
    }

    // ---- 开场幂等 ----

    #[test]
    fn begin_battle_is_idempotent() {
        let mut state = AssaultState::new();
        assert_eq!(state.phase, Phase::Intro);
        state.step(&rest()); // Intro 冻结：不推进
        assert_eq!(state.elapsed, 0.0);
        state.begin_battle();
        assert_eq!(state.phase, Phase::Playing);
        assert!(
            matches!(
                state.frame_events.first(),
                Some(AssaultEvent::BattleBegin { .. })
            ),
            "战斗开始应有事件"
        );
        let events_after_first = state.frame_events.len();
        state.begin_battle();
        assert_eq!(
            state.frame_events.len(),
            events_after_first,
            "重复 begin_battle 不应重复产生事件"
        );
    }

    // ---- 全流程自动玩家 ----

    #[test]
    fn auto_pilot_full_runthrough_victory() {
        let mut state = AssaultState::new();
        state.begin_battle();
        let frames = run_until(&mut state, |s| s.phase != Phase::Playing, 60 * 240);
        assert_eq!(state.phase, Phase::Victory, "4 分钟内自动玩家应全歼敌人");
        assert_eq!(state.kills, ENEMY_SPAWNS.len() as u32, "击杀数应 = 敌人数");
        assert!(
            state.player.hp > 0 && state.player.hp <= PLAYER_MAX_HP,
            "通关时玩家应存活（HP {}）",
            state.player.hp
        );
        assert!(frames < 60 * 180, "通关用时 {frames} 帧应 < 180 s");
        println!(
            "自动玩家战报：{frames} 帧（{:.1} s 游戏时间），击杀 {}/{}，玩家 HP {}/{}，开枪 {} 发",
            frames as f32 / 60.0,
            state.kills,
            ENEMY_SPAWNS.len(),
            state.player.hp,
            PLAYER_MAX_HP,
            state.player.rifle.shots,
        );
    }

    // ---- 确定性（逐位）----

    #[test]
    fn same_input_two_runs_are_bit_identical() {
        /// 逐帧采样行：玩家与敌人的关键状态 bits、事件数。
        type Sample = (
            [u32; 3],
            u32,
            i32,
            u8,
            u32,
            u32,
            u64,
            Vec<([u32; 3], u32, i32, u32, u32)>,
            u32,
            u32,
            usize,
        );

        fn trace() -> Vec<Sample> {
            let mut state = AssaultState::new();
            state.begin_battle();
            let mut samples = Vec::new();
            for _ in 0..60 * 120 {
                let input = AutoPilot.drive(&mut state);
                state.step(&input);
                let enemies = state
                    .enemies
                    .iter()
                    .map(|e| {
                        (
                            [e.pos.x.to_bits(), e.pos.y.to_bits(), e.pos.z.to_bits()],
                            e.facing.to_bits(),
                            e.hp,
                            e.state as u32,
                            e.last_sweep_id,
                        )
                    })
                    .collect();
                samples.push((
                    [
                        state.player.pos.x.to_bits(),
                        state.player.pos.y.to_bits(),
                        state.player.pos.z.to_bits(),
                    ],
                    state.player.facing.to_bits(),
                    state.player.hp,
                    state.player.sword.stage,
                    state.player.sword.sweep_id,
                    state.player.rifle.ammo,
                    state.player.rifle.shots,
                    enemies,
                    state.kills,
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
        assert!(a.len() > 60 * 2, "采样应覆盖一段真实推进（非空转）");
        for (frame, (sa, sb)) in a.iter().zip(&b).enumerate() {
            assert_eq!(sa, sb, "同输入两遍应逐采样 bit 级一致，帧 #{frame}");
        }
        let last = a.last().expect("非空");
        assert_eq!(last.9, Phase::Victory as u32, "脚本应打到胜利终局");
    }
}
