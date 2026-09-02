//! 纯游戏逻辑核心：确定性平台跳跃状态机。依赖 glam + [`iris_core`]
//! 的**纯函数**（[`move_and_slide_statics`] / [`ColliderShape`]）——
//! 这是刻意的 dogfooding：平台跳跃的物理就是引擎运动学角色控制器
//! （R31），不重写第二份碰撞。「Iris Ascent」的全部规则（移动 / 跳跃
//! 与土狼时间 / 跳板弹射 / 台阶与球坡 / 宝石 / 检查点 / 坠落复活 /
//! 山顶胜利）都在这个模块里以纯函数推进：输入 [`AscentInput`]，输出
//! 状态变化与 [`AscentEvent`] 列表——不读时钟、不做随机、不触碰
//! ECS 世界。因此：
//! - 全部规则可毫秒级单测（本模块底部 13 项）；
//! - 插件层（`crate::plugin`）只负责把键盘输入翻译成本模块类型，再把
//!   结果写回实体组件与表现资源；
//! - headless 测试、自动玩家与离屏快照共享同一条确定性推进路径。
//!
//! # 与引擎角色控制器的关系（载荷说明）
//!
//! 每帧把「水平意图 + 竖直速度」交给 [`move_and_slide_statics`]（静态体
//! 列表 = 关卡平台 + 大球坡面，由 [`AscentState`] 在构造时一次性转成
//! `Vec<(Vec3, ColliderShape)>`），拿回位置 / 速度 / 落地状态后自行写回
//! ——调用方保留输入与状态所有权（引擎约定的正统用法）。控制器五步
//! 管线（重力 → 水平滑行 → 台阶 → 斜坡限 → 贴地吸附）确定性纯函数：
//! 同输入两次模拟逐位一致（底部确定性单测守护）。
//!
//! # 关卡设计（线性上升，数值即文档）
//!
//! 关卡沿世界 -Z 一路向上（出生在 z=+3，山顶在 z≈-116，顶面 y≈60.9，
//! 加信标后场景竖直跨度 ~66 m；配合远景装饰总场景跨度 ~130 m）：
//! 1. **台阶段（P0–P3）**：四级 0.28 m 台阶台地（≤ [`CHARACTER`] 的
//!    `step_height` 0.3）——纯行走迈上，不上跳（自动玩家的输入序列因此
//!    极简：前进 + 两个定点跳）；
//! 2. **大球坡面（R=14）**：从 P3 台缘走落球面（落差 0.14 m < 贴地吸附
//!    0.2，全程 grounded），沿坡走到球顶；**定点跳**越过球顶落在 P4；
//! 3. **四段跳板弹射**（P5/P7/P10/P12）：跳板不是物理——站在其顶面区域
//!    （AABB 判定）且 `vel.y ≤ 0` 时给竖直冲量 [`LAUNCH_SPEED`]（每段
//!    接触恰好一次）；落点平台与跳板顶面高差按弹道解析预留（见各测试的
//!    解析对照）；跳板与邻台的高差只有 0.2 m 的「走落」衔接（桥接缝
//!    0–0.5 m，胶囊直径 0.8 m 不可能漏过）；
//! 4. **检查点 ×3**（P6 / P8 / P11）：穿过激活域 → 复活点更新 + 事件；
//! 5. **宝石 ×14**：沿途路径线上距离收集；
//! 6. **山顶触发域**：进入 → 胜利冻结；**坠落线** `y < [`KILL_Y`]` →
//!    回最近检查点复活 + 事件。
//!
//! # 帧内顺序（固定，是确定性的一部分）
//!
//! 终局冻结 → 跳板冲量 → 水平意图积分（地面 / 空中加速）→ 跳跃（含
//! 土狼时间）→ [`move_and_slide_statics`] 回灌 → 落地沿与落地冲量 →
//! 宝石 → 检查点 → 山顶 → 坠落复活。
//!
//! # 自动玩家（[`AutoPilot`]）
//!
//! 关卡被刻意设计成「前进 + 两个定点跳」即可通关：出生台地一跳（展示
//! 跳跃机制）、球顶一跳（唯一的精度跳，落点窗口 ≥ 5 m）。脚本按状态
//! 条件（z 区间 + 落地状态 + 脚底高度）而非帧数触发，对 60 Hz 离散化
//! 不敏感；headless 测试与纯核心单测共用同一脚本（零偏离）。

use glam::{Vec2, Vec3};

use iris_core::character::{CharacterParams, move_and_slide_statics};
use iris_core::physics::{ColliderShape, PHYSICS_GRAVITY};

// ---------------------------------------------------------------------------
// 可调常量（pub：单测 / 插件层 / 文档共用同一事实来源）
// ---------------------------------------------------------------------------

/// 水平移动目标速度（米/秒）：地面与空中同一目标，差别只在趋近速率
/// （[`GROUND_ACCEL`] / [`AIR_ACCEL`]）。
pub const MOVE_SPEED: f32 = 6.0;
/// 跳跃初速（米/秒）：跳高 = v²/2g ≈ 2.5 m。
pub const JUMP_SPEED: f32 = 7.0;
/// 跳板弹射初速（米/秒）：弹高 = v²/2g ≈ 16.5 m——四段弹射承担关卡
/// 主要爬升。
pub const LAUNCH_SPEED: f32 = 18.0;
/// 土狼时间（秒）：离开地面后的短暂窗口内仍可起跳（平台跳跃手感标配）。
pub const COYOTE_TIME: f32 = 0.1;
/// 落地事件的最小冲量（米/秒）：低于此值的落地（台阶迈上、贴地吸附的
/// 小落差）不算「落地」，不触发落地音 / 尘土——防小台阶误触发。
pub const LAND_IMPACT_MIN: f32 = 3.0;
/// 宝石收集半径（米）：与角色胶囊中心的距离判定。
pub const GEM_RADIUS: f32 = 1.3;
/// 坠落线（米）：胶囊中心低于此值 → 回最近检查点复活。
pub const KILL_Y: f32 = -12.0;
/// 地面水平加速趋近速率（1/秒）：`vel → 目标速度` 的指数趋近系数。
pub const GROUND_ACCEL: f32 = 40.0;
/// 空中水平控制速率（1/秒）：空中仍有控制权但更迟钝（平台跳跃惯例）。
pub const AIR_ACCEL: f32 = 10.0;

/// 角色控制器参数：引擎 [`CharacterParams::default`] 的同值显式写出
/// （半径 0.4、半高 0.9、台阶 0.3、坡限 45°、吸附 0.2、标准重力）。
/// 半高 0.9 = 胶囊中心到脚底的竖直距离（插件层的模型挂点 / 跳板脚底
/// 判定都以此为据）。
pub const CHARACTER: CharacterParams = CharacterParams {
    radius: 0.4,
    half_height: 0.9,
    step_height: 0.3,
    slope_limit_radians: std::f32::consts::FRAC_PI_4,
    snap_distance: 0.2,
    gravity_scale: 1.0,
};

/// 角色半高速查（= [`CHARACTER`].half_height）：脚底 = 中心 y − 此值。
pub const HALF_HEIGHT: f32 = 0.9;

// ---------------------------------------------------------------------------
// 关卡数据（写死、确定性；视觉尺寸与碰撞体同源）
// ---------------------------------------------------------------------------

/// 一块静态平台（AABB）：`top = center.y + half.y`。视觉网格与逻辑静态
/// 体共用本结构（插件层按 `half × 2` 缩放立方体，尺寸同源零漂移）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Platform {
    /// 盒中心（世界坐标）。
    pub center: Vec3,
    /// 半边长。
    pub half: Vec3,
}

impl Platform {
    /// 顶面高度（米）。
    pub const fn top(&self) -> f32 {
        self.center.y + self.half.y
    }
}

/// 一块跳板：几何就是一块平台；语义额外一层——角色脚底进入其顶面区域
/// （XZ 在半边长内、脚底在 `[top−0.15, top+0.6]` 带）且 `vel.y ≤ 0`
/// 时给一次竖直冲量 [`LAUNCH_SPEED`]。
#[derive(Clone, Copy, Debug)]
pub struct Pad {
    /// 盒中心（世界坐标）。
    pub center: Vec3,
    /// 半边长。
    pub half: Vec3,
}

impl Pad {
    /// 顶面高度（米）。
    pub const fn top(&self) -> f32 {
        self.center.y + self.half.y
    }
}

/// 一个检查点：激活域（AABB）+ 复活脚底位置（域中心正上方台面）。
#[derive(Clone, Copy, Debug)]
pub struct Checkpoint {
    /// 激活域中心（世界坐标）。
    pub zone_center: Vec3,
    /// 激活域半边长。
    pub zone_half: Vec3,
    /// 复活点（脚底）：复活时胶囊中心 = 此点 + (0, [`HALF_HEIGHT`], 0)。
    pub respawn_foot: Vec3,
}

/// 全部静态平台（含台地、跳板台、台阶段、山顶台；视觉与碰撞同源）。
/// 布局推导见模块文档；相邻衔接的高差全部 ≤ 0.3 m（可迈上）或为
/// 「走落」小落差，缝宽 0–0.5 m（胶囊直径 0.8 m 不可能漏过）。
pub const PLATFORMS: [Platform; 19] = [
    // P0 出生台地（顶 0）
    Platform {
        center: Vec3::new(0.0, -1.0, 0.0),
        half: Vec3::new(6.0, 1.0, 6.0),
    },
    // 台阶段：0.28 m × 3 级（≤ step_height 0.3，纯行走迈上）
    Platform {
        center: Vec3::new(0.0, -0.32, -9.5),
        half: Vec3::new(2.5, 0.6, 3.0),
    }, // 顶 0.28
    Platform {
        center: Vec3::new(0.0, -0.04, -15.5),
        half: Vec3::new(2.5, 0.6, 3.0),
    }, // 顶 0.56
    Platform {
        center: Vec3::new(0.0, 0.24, -21.5),
        half: Vec3::new(2.5, 0.6, 3.0),
    }, // 顶 0.84
    // P4：球顶定点跳的落点台（顶 2.6，落点窗口 ≥ 5 m）
    Platform {
        center: Vec3::new(0.0, 2.0, -33.0),
        half: Vec3::new(2.5, 0.6, 4.0),
    },
    // P5 跳板台（顶 2.4）：与 P4 缝 0 m、落差 0.2 走落
    Platform {
        center: Vec3::new(0.0, 1.9, -40.0),
        half: Vec3::new(2.0, 0.5, 3.0),
    },
    // P6：第一段弹射落点（顶 16.7）+ 检查点 1
    Platform {
        center: Vec3::new(0.0, 15.95, -51.0),
        half: Vec3::new(3.0, 0.75, 5.0),
    },
    // P7 跳板台（顶 16.5）：与 P6 重叠 0.5 m、落差 0.2 走落
    Platform {
        center: Vec3::new(0.0, 16.0, -58.5),
        half: Vec3::new(2.0, 0.5, 3.0),
    },
    // P8：第二段弹射落点（顶 30.8）+ 检查点 2
    Platform {
        center: Vec3::new(0.0, 30.05, -69.5),
        half: Vec3::new(3.0, 0.75, 5.0),
    },
    // 台阶段 2：0.28 m × 6 级（顶 31.08 → 32.48，纯行走迈上）
    Platform {
        center: Vec3::new(0.0, 30.58, -74.9),
        half: Vec3::new(2.0, 0.5, 0.4),
    },
    Platform {
        center: Vec3::new(0.0, 30.86, -75.7),
        half: Vec3::new(2.0, 0.5, 0.4),
    },
    Platform {
        center: Vec3::new(0.0, 31.14, -76.5),
        half: Vec3::new(2.0, 0.5, 0.4),
    },
    Platform {
        center: Vec3::new(0.0, 31.42, -77.3),
        half: Vec3::new(2.0, 0.5, 0.4),
    },
    Platform {
        center: Vec3::new(0.0, 31.70, -78.1),
        half: Vec3::new(2.0, 0.5, 0.4),
    },
    Platform {
        center: Vec3::new(0.0, 31.98, -78.9),
        half: Vec3::new(2.0, 0.5, 0.4),
    },
    // P9：台阶顶台（顶 32.68）
    Platform {
        center: Vec3::new(0.0, 32.08, -82.0),
        half: Vec3::new(2.5, 0.6, 2.5),
    },
    // P10 跳板台（顶 32.48）：与 P9 重叠 0.5 m、落差 0.2 走落
    Platform {
        center: Vec3::new(0.0, 31.98, -87.0),
        half: Vec3::new(2.0, 0.5, 3.0),
    },
    // P11：第三段弹射落点（顶 46.78）+ 检查点 3
    Platform {
        center: Vec3::new(0.0, 46.03, -98.0),
        half: Vec3::new(3.0, 0.75, 5.0),
    },
    // P12 跳板台（顶 46.58）：与 P11 重叠 0.5 m、落差 0.2 走落
    Platform {
        center: Vec3::new(0.0, 46.08, -105.5),
        half: Vec3::new(2.0, 0.5, 3.0),
    },
];

// 山顶台（顶 60.88）：终局平台，不与跳板衔接（最后一跳直达）
pub const SUMMIT: Platform = Platform {
    center: Vec3::new(0.0, 60.13, -116.0),
    half: Vec3::new(4.0, 0.75, 4.5),
};

/// 大球坡面：半径 14 m 的静态球，球顶在 (0, 0.84, −26.5)——从 P3 台缘
/// 走落（落差 0.14 m < 贴地吸附 0.2）滚上球顶。球心 = 球顶 − (0, R, 0)。
pub const SPHERE_RADIUS: f32 = 14.0;
pub const SPHERE_TOP: Vec3 = Vec3::new(0.0, 0.84, -26.5);
pub const SPHERE_CENTER: Vec3 = Vec3::new(SPHERE_TOP.x, SPHERE_TOP.y - SPHERE_RADIUS, SPHERE_TOP.z);

/// 四段跳板（按通关顺序）：顶面即弹射触发区。顶面与邻台的高差恒
/// 0.2 m（走落衔接），弹射落点平台的高差按 `LAUNCH_SPEED` 弹道解析预留。
pub const PADS: [Pad; 4] = [
    Pad {
        center: Vec3::new(0.0, 1.9, -40.0),
        half: Vec3::new(2.0, 0.5, 3.0),
    },
    Pad {
        center: Vec3::new(0.0, 16.0, -58.5),
        half: Vec3::new(2.0, 0.5, 3.0),
    },
    Pad {
        center: Vec3::new(0.0, 31.98, -87.0),
        half: Vec3::new(2.0, 0.5, 3.0),
    },
    Pad {
        center: Vec3::new(0.0, 46.08, -105.5),
        half: Vec3::new(2.0, 0.5, 3.0),
    },
];

/// 三个检查点（按通关顺序，都在弹射落点台上）：激活域覆盖整块台面的
/// 弹射落点带（落点即激活）；旗杆 / 复活位在台面远端（朝下一跳方向，
/// 第三人称镜头里落点台上的旗在玩家前方可见）。复活位避开跳板触发带。
pub const CHECKPOINTS: [Checkpoint; 3] = [
    Checkpoint {
        zone_center: Vec3::new(0.0, 17.6, -52.75),
        zone_half: Vec3::new(3.0, 1.2, 3.25),
        respawn_foot: Vec3::new(0.0, 16.7, -55.0),
    },
    Checkpoint {
        zone_center: Vec3::new(0.0, 31.7, -72.0),
        zone_half: Vec3::new(3.0, 1.2, 3.5),
        respawn_foot: Vec3::new(0.0, 30.8, -74.0),
    },
    Checkpoint {
        zone_center: Vec3::new(0.0, 47.68, -100.5),
        zone_half: Vec3::new(3.0, 1.2, 3.5),
        respawn_foot: Vec3::new(0.0, 46.78, -101.0),
    },
];

/// 宝石（世界坐标，全部压在自动玩家的行进线上，y = 所在台面顶 + 0.9
/// = 角色中心高度）。收集半径 [`GEM_RADIUS`]。
pub const GEMS: [Vec3; 14] = [
    Vec3::new(0.0, 2.0, 0.8),      // P0（出生跳的越线上拾取）
    Vec3::new(0.0, 1.18, -9.5),    // 台阶 1
    Vec3::new(0.0, 1.46, -15.5),   // 台阶 2
    Vec3::new(0.0, 1.74, -21.5),   // 台阶 3
    Vec3::new(0.0, 1.74, -26.5),   // 大球坡面顶
    Vec3::new(0.0, 3.5, -35.0),    // P4（定点跳落点带上）
    Vec3::new(0.0, 17.6, -53.5),   // P6（检查点 1 之后）
    Vec3::new(0.0, 31.7, -72.0),   // P8（检查点 2 之后）
    Vec3::new(0.0, 33.58, -82.0),  // P9 台阶顶台
    Vec3::new(0.0, 47.68, -100.5), // P11（检查点 3 之后）
    Vec3::new(0.0, 47.48, -102.3), // P11 跳板前
    Vec3::new(0.0, 62.2, -116.2),  // 山顶（空中胜利触发的越线上，同帧结算）
    Vec3::new(0.0, 32.54, -76.5),  // 台阶段 2 中段（第 3 级上）
    Vec3::new(0.0, 31.7, -73.5),   // P8 尾部
];

/// 山顶触发域：进入即胜利（域覆盖整块山顶台的落点带）。
pub const SUMMIT_ZONE_CENTER: Vec3 = Vec3::new(0.0, 61.8, -116.0);
pub const SUMMIT_ZONE_HALF: Vec3 = Vec3::new(3.0, 1.2, 4.5);

/// 出生点（脚底）：P0 台面 +Z 侧。
pub const SPAWN_FOOT: Vec3 = Vec3::new(0.0, 0.0, 3.0);

/// 逻辑静态体（关卡碰撞事实来源）：平台 AABB + 大球。插件层与纯核心
/// 单测共用（每次 [`AscentState::new`] 构建一份）。
pub fn level_statics() -> Vec<(Vec3, ColliderShape)> {
    let mut statics: Vec<(Vec3, ColliderShape)> = Vec::with_capacity(PLATFORMS.len() + 1);
    for platform in PLATFORMS.iter() {
        statics.push((
            platform.center,
            ColliderShape::Box {
                half_extents: platform.half,
            },
        ));
    }
    statics.push((
        SUMMIT.center,
        ColliderShape::Box {
            half_extents: SUMMIT.half,
        },
    ));
    statics.push((
        SPHERE_CENTER,
        ColliderShape::Sphere {
            radius: SPHERE_RADIUS,
        },
    ));
    statics
}

/// 点是否在 AABB 内（闭区间）。
fn point_in_aabb(point: Vec3, center: Vec3, half: Vec3) -> bool {
    let d = point - center;
    d.x.abs() <= half.x && d.y.abs() <= half.y && d.z.abs() <= half.z
}

// ---------------------------------------------------------------------------
// 输入 / 事件 / 阶段
// ---------------------------------------------------------------------------

/// 一帧的游戏输入：世界系水平意图 + 跳跃键 + 步长。纯核心不碰键盘——
/// 插件层把「相机相对的 WASD」翻译成世界系向量（相机固定偏移时世界系
/// 与屏幕系一致，见 iris-game 先例）；`jump` 为电平语义（按住即落地就
/// 跳）；`dt` 由调用方随输入给出（纯核心不读时钟）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AscentInput {
    /// 世界系水平移动意图（x = 世界 +X，y = 世界 +Z）。
    pub move_dir: Vec2,
    /// 本帧是否按住跳跃。
    pub jump: bool,
    /// 步长（秒）。
    pub dt: f32,
}

/// 一帧内产生的游戏事件（含世界坐标）：插件层据此触发空间音频与粒子。
/// 每帧 `step` 开头清空——`step` 返回后 [`AscentState::frame_events`]
/// 恰好持有本帧全部事件（顺序 = 结算顺序，确定性）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AscentEvent {
    /// 起跳（位置 = 胶囊中心）。
    Jump { pos: Vec3 },
    /// 落地（位置 = 胶囊中心；冲量 = 触地前的下落速度，米/秒，恒
    /// ≥ [`LAND_IMPACT_MIN`]——小台阶 / 贴地吸附不产生本事件）。
    Landed { pos: Vec3, impact: f32 },
    /// 跳板弹射（位置 = 胶囊中心）。
    Launch { pos: Vec3 },
    /// 宝石收集（位置 = 宝石坐标）。
    Gem { pos: Vec3 },
    /// 检查点激活（位置 = 激活域中心）。
    Checkpoint { pos: Vec3, index: usize },
    /// 坠落 / 手动复活（位置 = 复活后的胶囊中心）。
    Respawn { pos: Vec3 },
    /// 抵达山顶（位置 = 胶囊中心）。
    Victory { pos: Vec3 },
}

/// 游戏阶段：攀登中 / 胜利（胜利为终局冻结；重开 = 重建
/// [`AscentState`]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// 攀登中。
    Playing,
    /// 抵达山顶。
    Victory,
}

// ---------------------------------------------------------------------------
// 状态
// ---------------------------------------------------------------------------

/// 游戏全局状态：阶段 + 角色（位置 / 速度 / 落地 / 朝向 / 土狼计时）+
/// 收集与检查点进度 + 复活点 + 统计 + 本帧事件。字段全部公开：测试可以
/// 直接构造 / 修改中间状态，插件层按只读约定消费（每帧由
/// [`AscentState::step`] 独占推进）。
#[derive(Clone, Debug)]
pub struct AscentState {
    pub phase: Phase,
    /// 胶囊中心（世界坐标；脚底 = y − [`HALF_HEIGHT`]）。
    pub pos: Vec3,
    /// 线速度（世界系；水平分量每帧向输入意图趋近）。
    pub vel: Vec3,
    /// 是否站在可站立地面上（引擎控制器的判定结果）。
    pub grounded: bool,
    /// 模型朝向（绕 Y 弧度；0 = 面朝 -Z，引擎前方约定）。
    pub facing: f32,
    /// 土狼剩余时间（秒）：> 0 时离地仍可起跳。
    pub coyote: f32,
    /// 复活点（胶囊中心；随检查点激活更新）。
    pub respawn: Vec3,
    /// 宝石收集进度（下标与 [`GEMS`] 一一对应）。
    pub gems: Vec<bool>,
    /// 检查点激活进度（下标与 [`CHECKPOINTS`] 一一对应）。
    pub checkpoints: Vec<bool>,
    /// 最近激活的检查点（`None` = 尚未激活任何检查点，复活回出生点）。
    pub active_checkpoint: Option<usize>,
    /// 坠落复活次数（HUD 统计；手动 R 复活也计入）。
    pub falls: u32,
    /// 对局累计时间（秒；`step` 累加，确定性）。
    pub elapsed: f32,
    /// 关卡静态体（构造时由 [`level_statics`] 一次性转出）。
    pub statics: Vec<(Vec3, ColliderShape)>,
    /// 本帧事件列表（每帧 `step` 开头清空，见 [`AscentEvent`]）。
    pub frame_events: Vec<AscentEvent>,
}

impl Default for AscentState {
    fn default() -> Self {
        Self::new()
    }
}

impl AscentState {
    /// 新状态：角色站在出生点（胶囊中心 = [`SPAWN_FOOT`] + 半高），
    /// 复活点同位。
    pub fn new() -> Self {
        let pos = SPAWN_FOOT + Vec3::new(0.0, HALF_HEIGHT, 0.0);
        Self {
            phase: Phase::Playing,
            pos,
            vel: Vec3::ZERO,
            grounded: false,
            facing: 0.0,
            coyote: 0.0,
            respawn: pos,
            gems: vec![false; GEMS.len()],
            checkpoints: vec![false; CHECKPOINTS.len()],
            active_checkpoint: None,
            falls: 0,
            elapsed: 0.0,
            statics: level_statics(),
            frame_events: Vec::new(),
        }
    }

    /// 已收集宝石数。
    pub fn gems_collected(&self) -> usize {
        self.gems.iter().filter(|&&got| got).count()
    }

    /// 已激活检查点数。
    pub fn checkpoints_activated(&self) -> usize {
        self.checkpoints.iter().filter(|&&on| on).count()
    }

    /// 手动复活（R 键）：与坠落复活同一套结算（计数、归位、事件）。
    pub fn request_respawn(&mut self) {
        self.falls += 1;
        self.pos = self.respawn;
        self.vel = Vec3::ZERO;
        self.grounded = false;
        self.coyote = 0.0;
        self.frame_events
            .push(AscentEvent::Respawn { pos: self.pos });
    }

    /// 推进一帧。终局阶段冻结（什么都不做）；`dt <= 0` 时冻结对局。
    /// 帧内顺序见模块文档（是确定性的一部分）。
    pub fn step(&mut self, input: &AscentInput) {
        self.frame_events.clear();
        if self.phase != Phase::Playing {
            return;
        }
        let dt = input.dt;
        if dt <= 0.0 {
            return;
        }
        self.elapsed += dt;

        // ---- 跳板：脚底在触发区内且非上升 → 一次竖直冲量（水平速度
        // 原样保留；`vel.y ≤ 0` 门禁保证每次接触只触发一次——弹起后
        // 立刻离开触发带，不会再叠加）----
        let foot = self.pos.y - HALF_HEIGHT;
        for pad in PADS.iter() {
            let inside_xz = (self.pos.x - pad.center.x).abs() <= pad.half.x
                && (self.pos.z - pad.center.z).abs() <= pad.half.z;
            let in_band = foot >= pad.top() - 0.15 && foot <= pad.top() + 0.6;
            if inside_xz && in_band && self.vel.y <= 0.0 {
                self.vel.y = LAUNCH_SPEED;
                self.grounded = false;
                self.frame_events
                    .push(AscentEvent::Launch { pos: self.pos });
            }
        }

        // ---- 水平意图 → 速度：向目标速度指数趋近（地面快 / 空中慢）----
        let target_x = input.move_dir.x * MOVE_SPEED;
        let target_z = input.move_dir.y * MOVE_SPEED;
        let rate = if self.grounded {
            GROUND_ACCEL
        } else {
            AIR_ACCEL
        };
        let blend = (rate * dt).min(1.0);
        self.vel.x += (target_x - self.vel.x) * blend;
        self.vel.z += (target_z - self.vel.z) * blend;

        // ---- 跳跃（含土狼时间）：grounded 或土狼窗口内才可起跳；起跳
        // 即消耗土狼（防离地后二段跳）----
        if input.jump && (self.grounded || self.coyote > 0.0) {
            self.vel.y = JUMP_SPEED;
            self.grounded = false;
            self.coyote = 0.0;
            self.frame_events.push(AscentEvent::Jump { pos: self.pos });
        }

        // ---- 引擎角色控制器：重力 / 滑行 / 台阶 / 坡限 / 吸附一次完成，
        // 结果回灌（调用方保留所有权）----
        let prev_grounded = self.grounded;
        // 落地冲量估计：进入控制器前的竖直速度 + 本帧重力（控制器内部
        // 半隐式先加重力再位移，触地瞬间的下落速度即二者之和）
        let impact = -(self.vel.y + PHYSICS_GRAVITY.y * CHARACTER.gravity_scale * dt);
        let out = move_and_slide_statics(self.pos, self.vel, dt, &CHARACTER, &self.statics);
        self.pos = out.position;
        self.vel = out.velocity;
        self.grounded = out.grounded;

        // ---- 落地沿：腾空 → 站立且冲量达阈值才算「落地」（防小台阶 /
        // 贴地吸附误触发）；土狼计时随落地刷新 / 离地递减 ----
        if self.grounded {
            self.coyote = COYOTE_TIME;
            if !prev_grounded && impact >= LAND_IMPACT_MIN {
                self.frame_events.push(AscentEvent::Landed {
                    pos: self.pos,
                    impact,
                });
            }
        } else {
            self.coyote -= dt;
        }

        // ---- 朝向：移动意图非零时面向移动方向（yaw = atan2(−x, −z)，
        // 引擎前方约定 -Z）----
        if input.move_dir.length_squared() > 1e-6 {
            self.facing = (-input.move_dir.x).atan2(-input.move_dir.y);
        }

        // ---- 宝石：与胶囊中心距离收集（每颗至多一次）----
        for (index, gem) in GEMS.iter().enumerate() {
            if !self.gems[index] && self.pos.distance(*gem) <= GEM_RADIUS {
                self.gems[index] = true;
                self.frame_events.push(AscentEvent::Gem { pos: *gem });
            }
        }

        // ---- 检查点：穿过激活域 → 激活（一次性）+ 复活点更新 ----
        for (index, checkpoint) in CHECKPOINTS.iter().enumerate() {
            if !self.checkpoints[index]
                && point_in_aabb(self.pos, checkpoint.zone_center, checkpoint.zone_half)
            {
                self.checkpoints[index] = true;
                self.active_checkpoint = Some(index);
                self.respawn = checkpoint.respawn_foot + Vec3::new(0.0, HALF_HEIGHT, 0.0);
                self.frame_events.push(AscentEvent::Checkpoint {
                    pos: checkpoint.zone_center,
                    index,
                });
            }
        }

        // ---- 山顶：进入触发域 → 胜利（终局冻结，本帧到此为止）----
        if point_in_aabb(self.pos, SUMMIT_ZONE_CENTER, SUMMIT_ZONE_HALF) {
            self.phase = Phase::Victory;
            self.frame_events
                .push(AscentEvent::Victory { pos: self.pos });
            return;
        }

        // ---- 坠落线：低于阈值 → 回最近检查点复活 ----
        if self.pos.y < KILL_Y {
            self.request_respawn();
        }
    }
}

// ---------------------------------------------------------------------------
// 自动玩家（headless 测试与纯核心单测共用的确定性脚本）
// ---------------------------------------------------------------------------

/// 脚本化自动玩家：关卡被设计成「前进 + 两个定点跳」即可通关——
/// - 出生台地一跳（z > −2 且落地即跳，恰好一跳后落点已越出触发带）；
/// - 大球坡顶一跳（z ∈ (−31, −26.8) 且脚底 > 2.9 且落地即跳：越过球顶
///   落在 P4，落点窗口 ≥ 5 m）；
/// - 其余全程只按前进：台地与台阶 0.28 m 高差由引擎控制器自动迈上，
///   球坡走落、跳板走落衔接（落差 0.2 m + 缝 ≤ 0.5 m）由贴地吸附兜住，
///   弹射与滑翔交给跳板冲量。
///
/// 条件按状态（z 区间 + 落地状态 + 脚底高度）而非帧数触发，对 60 Hz
/// 离散化不敏感；同输入逐位确定（见确定性单测）。
#[derive(Clone, Copy, Debug, Default)]
pub struct AutoPilot;

impl AutoPilot {
    /// 由当前状态产出本帧输入：恒定前进意图（-Z）+ 条件跳跃。
    pub fn drive(&self, state: &AscentState) -> AscentInput {
        let foot = state.pos.y - HALF_HEIGHT;
        let z = state.pos.z;
        let jump = state.phase == Phase::Playing
            && state.grounded
            && (z > -2.0 || (foot > 0.5 && z < -26.8 && z > -31.0));
        AscentInput {
            move_dir: Vec2::new(0.0, -1.0),
            jump,
            dt: 1.0 / 60.0,
        }
    }
}

/// 以固定步长推进自动玩家直到 `done` 返回真或帧数封顶，返回推进帧数。
/// headless 测试与纯核心单测共用（同一推进路径，零偏离）。
pub fn run_until(
    state: &mut AscentState,
    done: impl Fn(&AscentState) -> bool,
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

    /// 空推 `seconds` 秒（无输入）。
    fn idle(state: &mut AscentState, seconds: f32) {
        let steps = (seconds / DT).round() as u32;
        for _ in 0..steps {
            state.step(&rest());
        }
    }

    /// 脚底高度速查。
    fn foot(state: &AscentState) -> f32 {
        state.pos.y - HALF_HEIGHT
    }

    /// 前进输入（-Z，可带跳）。
    fn fwd(jump: bool) -> AscentInput {
        AscentInput {
            move_dir: Vec2::new(0.0, -1.0),
            jump,
            dt: DT,
        }
    }

    /// 静止输入（可带跳）。
    fn still(jump: bool) -> AscentInput {
        AscentInput {
            move_dir: Vec2::ZERO,
            jump,
            dt: DT,
        }
    }

    /// 静止不跳（空推）。
    fn rest() -> AscentInput {
        still(false)
    }

    // ---- 关卡数据不变量 ----

    #[test]
    fn level_invariants_hold() {
        // 坠落线必须低于一切可行走表面（出生台面顶 0）
        for platform in PLATFORMS.iter() {
            assert!(
                platform.top() > KILL_Y + 5.0,
                "平台顶 {} 应远高于坠落线 {}",
                platform.top(),
                KILL_Y
            );
        }
        assert!(SUMMIT.top() > KILL_Y + 5.0);
        // 台阶与台地衔接高差全部 ≤ step_height 0.3 或为走落小落差 ≤ 0.35
        // （P4→P5 / P6→P7 / P9→P10 / P11→P12 的跳板衔接）；逐对检查相邻
        // （z 递减序）平台的高差
        let tops = [
            PLATFORMS[0].top(),
            PLATFORMS[1].top(),
            PLATFORMS[2].top(),
            PLATFORMS[3].top(),
        ];
        for pair in tops.windows(2) {
            assert!(
                (pair[1] - pair[0]).abs() <= 0.3 + EPS,
                "台阶段相邻高差应 ≤ 0.3：{pair:?}"
            );
        }
        // 跳板顶与邻台顶高差 0.2（走落衔接），跳板弹射落点高差 ≈ 9.1
        // （LAUNCH_SPEED 弹道解析，见 pad_launch_matches_analytic_apex）
        assert!((PADS[0].top() - PLATFORMS[4].top() + 0.2).abs() < EPS);
        assert!((PADS[1].top() - PLATFORMS[6].top() + 0.2).abs() < EPS);
        assert!((PADS[2].top() - PLATFORMS[15].top() + 0.2).abs() < EPS);
        assert!((PADS[3].top() - PLATFORMS[17].top() + 0.2).abs() < EPS);
        // 检查点复活脚底必须精确落在某块平台顶上
        for checkpoint in CHECKPOINTS.iter() {
            assert!(
                PLATFORMS
                    .iter()
                    .any(|p| (p.top() - checkpoint.respawn_foot.y).abs() < EPS),
                "复活点脚底 {} 应在某块平台顶上",
                checkpoint.respawn_foot.y
            );
        }
        // 宝石数量与收集进度表一致；山顶域含住山顶台落点带
        assert_eq!(GEMS.len(), 14);
        assert_eq!(CHECKPOINTS.len(), 3);
        assert!(SUMMIT_ZONE_CENTER.y - SUMMIT_ZONE_HALF.y < SUMMIT.top());
        assert!(SUMMIT_ZONE_CENTER.y + SUMMIT_ZONE_HALF.y > SUMMIT.top() + HALF_HEIGHT);
    }

    // ---- 台阶（0.28 m ≤ step_height）与台地行走 ----

    #[test]
    fn terraces_are_walked_up_without_jumping() {
        let mut state = AscentState::new();
        // 无跳（jump=false）纯前进：应依次迈上三级台阶
        let mut reached = false;
        for _ in 0..60 * 8 {
            state.step(&fwd(false));
            if state.pos.z < -21.5 {
                reached = true;
                break;
            }
        }
        assert!(reached, "8 秒内应走到 P3 台地（台阶自动迈上）");
        assert!(state.grounded, "台阶行走应全程贴地");
        assert!(
            (foot(&state) - PLATFORMS[3].top()).abs() < 1e-2,
            "应站在 P3 台面顶 {}，实际 {}",
            PLATFORMS[3].top(),
            foot(&state)
        );
    }

    // ---- 大球坡面（grounded 贴面行走）----

    #[test]
    fn sphere_slope_is_walked_while_grounded() {
        let mut state = AscentState::new();
        // 纯前进走完台地并走落球面（落差 0.14 < 吸附 0.2：全程贴地）
        let mut on_sphere = false;
        for _ in 0..60 * 10 {
            state.step(&fwd(false));
            if state.pos.z < -25.0 && state.pos.z > -26.2 {
                on_sphere = true;
                assert!(state.grounded, "球面近坡应 grounded（贴地吸附）");
                // 贴面：脚底 ≈ 球面高度（球顶附近坡度平缓，误差留 5 cm）
                let dz = state.pos.z - SPHERE_TOP.z;
                let surface = SPHERE_TOP.y
                    - (SPHERE_RADIUS - (SPHERE_RADIUS * SPHERE_RADIUS - dz * dz).sqrt());
                assert!(
                    (foot(&state) - surface).abs() < 0.05,
                    "应贴球面：脚底 {} vs 解析 {}",
                    foot(&state),
                    surface
                );
                break;
            }
        }
        assert!(on_sphere, "10 秒内应走落大球坡面");
    }

    // ---- 跳板弹射（解析对照）----

    #[test]
    fn pad_launch_matches_analytic_apex_and_lands_on_next_platform() {
        let mut state = AscentState::new();
        // 直奔第一块跳板（AutoPilot 的两个定点跳不影响本段——球顶跳的
        // 落点在 P4，随后走落 P5 跳板）
        let frames = run_until(
            &mut state,
            |s| {
                s.frame_events
                    .iter()
                    .any(|e| matches!(e, AscentEvent::Launch { .. }))
            },
            60 * 40,
        );
        assert!(frames < 60 * 40, "40 秒内应踩上第一块跳板");
        let pad_top = PADS[0].top();
        // 弹射后跟踪最高点与落点
        let mut apex = f32::MIN;
        let mut landed = false;
        for _ in 0..60 * 8 {
            let input = AutoPilot.drive(&state);
            state.step(&input);
            apex = apex.max(foot(&state));
            if state.grounded && foot(&state) > 10.0 {
                landed = true;
                break;
            }
        }
        assert!(landed, "弹射后应落在 P6 台上");
        // 弹高解析：apex ≈ pad_top + v²/2g（半隐式欧拉 + 单次触发的
        // 一帧内重力损耗，容差 0.25 m）
        let want = pad_top + LAUNCH_SPEED * LAUNCH_SPEED / (2.0 * 9.81);
        assert!(
            (apex - want).abs() < 0.25,
            "弹射顶点应 ≈ {want}（pad 顶 {pad_top} + v²/2g），实际 {apex}"
        );
        // 落点在 P6 台面上
        let p6 = &PLATFORMS[6];
        assert!(
            (foot(&state) - p6.top()).abs() < EPS,
            "应落在 P6 顶 {}，实际 {}",
            p6.top(),
            foot(&state)
        );
        assert!(
            state.pos.z >= p6.center.z - p6.half.z && state.pos.z <= p6.center.z + p6.half.z,
            "落点 z {} 应在 P6 范围内",
            state.pos.z
        );
        // 检查点 1 应在落点带内顺路激活
        assert!(state.checkpoints[0], "落点应落入检查点 1 激活域");
    }

    // ---- 土狼时间 ----

    #[test]
    fn coyote_time_allows_jump_shortly_after_leaving_ground() {
        // 构造「刚走出 P0 台缘」的中间态：字段公开（bastion 同约定）——
        // 真实行走离台会被台缘角点接触 + 迈台阶接住（见 level 单测），
        // 土狼窗口需要显式 staged 腾空帧
        let mut state = AscentState::new();
        idle(&mut state, 0.3); // 先站稳
        state.pos = Vec3::new(0.0, 1.2, -6.3); // 台缘外一点（整体高于台面，无嵌）
        state.grounded = false;
        state.coyote = COYOTE_TIME;
        // 土狼窗口内起跳：成功（速度竖直向上 + Jump 事件）
        let before = state.vel.y;
        state.step(&fwd(true));
        assert!(
            state.vel.y > before + 1.0,
            "土狼窗口内应可起跳，vel.y = {}",
            state.vel.y
        );
        assert!(
            state
                .frame_events
                .iter()
                .any(|e| matches!(e, AscentEvent::Jump { .. })),
            "土狼跳应产生 Jump 事件"
        );
        // 对照：耗尽土狼后（悬空 > 0.15 s）跳跃无效
        let mut state = AscentState::new();
        state.pos = Vec3::new(0.0, 5.0, -8.0); // 悬在关卡下方的开阔空中
        state.coyote = 0.0;
        idle(&mut state, 0.15);
        assert!(!state.grounded && state.coyote <= 0.0, "应悬空且土狼已耗尽");
        let vy = state.vel.y;
        state.step(&fwd(true));
        assert!(
            state
                .frame_events
                .iter()
                .all(|e| !matches!(e, AscentEvent::Jump { .. })),
            "土狼耗尽后跳跃不应触发 Jump 事件"
        );
        // 速度只应含本帧重力（无 JUMP_SPEED 注入；控制器每帧先加重力）
        assert!(
            (state.vel.y - (vy - 9.81 * DT)).abs() < EPS,
            "土狼耗尽后速度应只含重力，实际 {}（前 {}）",
            state.vel.y,
            vy
        );
    }

    // ---- 落地事件与冲量阈值 ----

    #[test]
    fn landed_event_respects_impact_threshold() {
        let mut state = AscentState::new();
        // 原地跳一次：落地冲量 ≈ 7 ≥ 阈值 → Landed 事件
        let mut jumped = false;
        let mut landed = None;
        for _ in 0..60 * 4 {
            state.step(&still(!jumped && state.grounded));
            if state
                .frame_events
                .iter()
                .any(|e| matches!(e, AscentEvent::Jump { .. }))
            {
                jumped = true;
            }
            for event in &state.frame_events {
                if let AscentEvent::Landed { impact, .. } = *event {
                    landed = Some(impact);
                }
            }
            if landed.is_some() {
                break;
            }
        }
        let impact = landed.expect("跳跃落地应产生 Landed 事件");
        assert!(
            (impact - JUMP_SPEED).abs() < 0.5,
            "落地冲量应 ≈ 跳跃初速 {JUMP_SPEED}，实际 {impact}"
        );
        // 对照：走下 0.28 m 小台阶（贴地吸附 / 低速触地）不应产生 Landed
        let mut state = AscentState::new();
        idle(&mut state, 0.2); // 先站稳
        let mut landed_on_step = false;
        for _ in 0..60 * 6 {
            state.step(&fwd(false));
            if state.pos.z < -7.0 {
                if state
                    .frame_events
                    .iter()
                    .any(|e| matches!(e, AscentEvent::Landed { .. }))
                {
                    landed_on_step = true;
                }
                break;
            }
        }
        assert!(
            !landed_on_step,
            "走下小台阶不应误触发 Landed（冲量低于阈值）"
        );
    }

    // ---- 宝石 ----

    #[test]
    fn gems_are_collected_once_with_event() {
        let mut state = AscentState::new();
        // 直接把角色传送到 1 号宝石旁（字段公开，构造中间态）
        state.pos = GEMS[1] + Vec3::new(0.2, 0.0, 0.0);
        state.step(&rest());
        assert!(state.gems[1], "贴近的宝石应被收集");
        assert_eq!(state.gems_collected(), 1);
        assert!(matches!(
            state.frame_events.first(),
            Some(AscentEvent::Gem { .. })
        ));
        // 原地再站一帧：不重复收集、不重复事件
        state.step(&rest());
        assert_eq!(state.gems_collected(), 1);
        assert!(
            !state
                .frame_events
                .iter()
                .any(|e| matches!(e, AscentEvent::Gem { .. })),
            "已收集宝石不应重复产生事件"
        );
    }

    // ---- 检查点与坠落复活 ----

    #[test]
    fn checkpoint_activation_updates_respawn_exactly() {
        let mut state = AscentState::new();
        // 传送到检查点 1 的激活域内
        state.pos = CHECKPOINTS[0].zone_center;
        state.vel = Vec3::ZERO;
        state.step(&rest());
        assert!(state.checkpoints[0]);
        assert_eq!(state.active_checkpoint, Some(0));
        assert!(
            state
                .frame_events
                .iter()
                .any(|e| matches!(e, AscentEvent::Checkpoint { index: 0, .. })),
            "激活应产生 Checkpoint 事件"
        );
        // 复活点 = 复活脚底 + 半高（逐位比较）
        let want = CHECKPOINTS[0].respawn_foot + Vec3::new(0.0, HALF_HEIGHT, 0.0);
        assert_eq!(state.respawn, want, "复活点应精确等于检查点复活位");
        // 坠落：回检查点 1 复活，位置逐位一致
        state.pos.y = KILL_Y - 1.0;
        state.step(&rest());
        assert_eq!(state.falls, 1);
        assert_eq!(state.pos, want, "复活位置应逐位等于检查点复活位");
        assert!(state.vel == Vec3::ZERO, "复活后速度清零");
        assert!(
            matches!(state.frame_events.last(), Some(AscentEvent::Respawn { .. })),
            "复活应产生 Respawn 事件"
        );
    }

    #[test]
    fn falling_before_any_checkpoint_respawns_at_spawn() {
        let mut state = AscentState::new();
        idle(&mut state, 0.3); // 站稳
        state.pos = Vec3::new(8.0, 5.0, 0.0); // 抛出台侧
        for _ in 0..60 * 3 {
            state.step(&rest());
            if state.falls == 1 {
                break; // 复活当帧即断言（出生点会再触发跳跃带，后续帧不比）
            }
        }
        assert_eq!(state.falls, 1, "3 秒内应坠过坠落线并复活一次");
        let spawn_center = SPAWN_FOOT + Vec3::new(0.0, HALF_HEIGHT, 0.0);
        assert_eq!(state.pos, spawn_center, "无检查点时复活回出生点");
        assert_eq!(state.active_checkpoint, None);
    }

    // ---- 胜利 ----

    #[test]
    fn summit_zone_triggers_victory_and_freezes() {
        let mut state = AscentState::new();
        // 直接放入触发域一角（远离山顶宝石，构造中间态）
        state.pos = Vec3::new(2.0, 61.9, -113.5);
        state.vel = Vec3::ZERO;
        state.grounded = true;
        state.step(&fwd(false));
        assert_eq!(state.phase, Phase::Victory);
        assert!(
            state
                .frame_events
                .iter()
                .any(|e| matches!(e, AscentEvent::Victory { .. })),
            "进入触发域应产生 Victory 事件"
        );
        // 终局冻结：位置与时间不再变化
        let snapshot = (state.pos, state.vel, state.elapsed);
        state.step(&fwd(true));
        assert_eq!(
            (state.pos, state.vel, state.elapsed),
            snapshot,
            "终局应冻结"
        );
    }

    // ---- 全流程自动玩家（核心余量：坠落 = 0）----

    #[test]
    fn auto_pilot_full_runthrough_victory_zero_falls() {
        let mut state = AscentState::new();
        let frames = run_until(&mut state, |s| s.phase == Phase::Victory, 60 * 180);
        assert_eq!(state.phase, Phase::Victory, "3 分钟内应通关");
        assert_eq!(state.falls, 0, "脚本设计内坠落次数应为 0");
        assert_eq!(state.gems_collected(), GEMS.len(), "应收集全部宝石");
        assert_eq!(
            state.checkpoints_activated(),
            CHECKPOINTS.len(),
            "应激活全部检查点"
        );
        // 用时应显著短于封顶（关卡长度 ~130 m + 四段弹射 ≈ 50 s 量级）
        assert!(
            frames < 60 * 90,
            "通关用时 {frames} 帧应 < 90 s（关卡线性无绕路）"
        );
        println!(
            "自动玩家通关：{frames} 帧（{:.1} s 游戏时间），宝石 {}/{}，检查点 {}/{}，坠落 {}",
            frames as f32 / 60.0,
            state.gems_collected(),
            GEMS.len(),
            state.checkpoints_activated(),
            CHECKPOINTS.len(),
            state.falls
        );
    }

    // ---- 确定性（逐位）----

    #[test]
    fn same_input_two_runs_are_bit_identical() {
        /// 逐帧采样行：位置 3 × bits、速度 3 × bits、grounded、facing
        /// bits、gems、checkpoints、falls、阶段、事件数。
        type Sample = ([u32; 3], [u32; 3], bool, u32, usize, usize, u32, u32, usize);

        fn trace() -> Vec<Sample> {
            let mut state = AscentState::new();
            let mut samples = Vec::new();
            let pilot = AutoPilot;
            for _ in 0..60 * 180 {
                let input = pilot.drive(&state);
                state.step(&input);
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
                    state.grounded,
                    state.facing.to_bits(),
                    state.gems_collected(),
                    state.checkpoints_activated(),
                    state.falls,
                    state.phase as u32,
                    state.frame_events.len(),
                ));
                if state.phase == Phase::Victory {
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
