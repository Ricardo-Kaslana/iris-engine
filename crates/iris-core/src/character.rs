//! 运动学角色控制器：竖直胶囊（[`ColliderShape::Capsule`]）+
//! `move_and_slide`——R21 球扫掠（[`shape_sweep`] / [`detect_pair`] 二分）
//! 的正式消费方。
//!
//! 与 [`crate::physics::resolve_static_push`] 同款哲学：**查询而非系统**
//! ——纯函数核心（[`move_and_slide_statics`]，静态体列表入参）可脱离
//! 世界单测；世界封装 [`move_and_slide`] 只收集静态体（语义与
//! `resolve_against_statics` 一致：`is_static`、或仅 Collider 无 RigidBody、
//! 或运动学体——见 `physics::BodyType::Kinematic`
//! 的组件缺席语义），不积分、不写任何组件。调用方（游戏逻辑）保留输入
//! 与状态所有权：每帧传入位置 / 速度，拿回结果自行写回。
//!
//! # 旋转（R50）
//!
//! - **角色胶囊自身恒竖直**：直立人体是控制器的语义前提（台阶 / 坡面 /
//!   吸附全部按「脚底 = 中心 − half_height」推导），需要「会倒下的胶囊」
//!   请用物理刚体（[`crate::physics`]）而不是角色控制器；
//! - **静态障碍旋转感知**：世界封装收集 [`StaticObstacle`]（位置 + 旋转 +
//!   形状）并走 [`move_and_slide_statics_rotated`]——动画接管期的 ragdoll
//!   肢体是「静态 + 斜置胶囊」，照实按其倒下的朝向阻挡 / 承托（旧口径当成
//!   直立胶囊，会出现「踩空气」与「被隐形柱挡住」两类错觉）；纯函数核心
//!   因此有两个入口：二元组静态体列表（胶囊恒竖直，R31 口径逐位不变）与
//!   三元组（旋转感知），共用同一份五步管线实现（无行为分岔）。
//!
//! # 管线（每次调用五步，顺序即文档）
//!
//! 1. **重力**：`vel.y += 重力 × gravity_scale × dt`（[`PHYSICS_GRAVITY`]）；
//! 2. **水平移动（贴墙滑行）**：位移 = `vel_xz × dt`，**最多
//!    [`MAX_SLIDE_ITERATIONS`]（4）次扫掠迭代**——形状扫掠求首次碰撞：
//!    无阻挡走完；有阻挡推进到命中点（扫掠皮肤收缩留防抖间隙），把
//!    「朝向阻挡面」的法向分量从速度与剩余位移中同时切掉
//!    （`v −= n·(v·n)`，n 取阻挡法线的水平归一——切向速度天然保留，
//!    数学同静态推离），剩余位移继续下一轮。**可站立坡面**（法线
//!    `n.y ≥ cos(slope_limit)`）不切速度——上坡交给台阶机制，避免把
//!    水平推力转成沿坡上滑的速度分量；
//! 3. **台阶**：水平被阻挡时尝试「抬起重试」——所选算法：
//!    ① 门禁：`vel.y ≤ 0`（非上升）且脚下 `snap_distance` 内探到地面
//!    （贴地才迈，防空中跨步）且正上方 `step_height` 内畅通（有天花板
//!    压顶不迈，保守）；② 抬升 `step_height` 后重试水平扫掠，前向位移
//!    必须能**完整走完**（否则视为不可整体迈上，拒绝）——前向距离对
//!    **不可站立阻挡**（墙 / 陡坡面）取 `max(剩余位移, radius)`：台阶
//!    迎风面是「墙」，中心须送进台面 radius 深、落到平顶（法线 +Y），
//!    否则贴边落在台缘角点（法线倾斜）永远迈不上去；对**可站立坡面**
//!    按剩余位移原样（缓坡上坡靠逐帧小步，radius 前跳会让坡面移动快过
//!    输入——半径前跳的速度红利因此只存在于「撞墙迈台阶」一种情形）；
//!    ③ 向下扫掠 `step_height + snap_distance` 落回，落点法线可站立、
//!    脚底高差 ≤ `step_height`（防御性校验——抬升量即 step_height 时恒
//!    成立）才接受。低台阶自动迈上、高墙照撞、缓坡上坡 = 逐帧低台阶；
//! 4. **斜坡限**：垂直下落 / 静止时向下扫掠命中地面：法线可站立
//!    （`n.y ≥ cos(slope_limit)`）→ `grounded = true`、`vel.y = 0`；
//!    过陡 → `grounded = false`、速度切掉入坡法向分量（沿坡面的下滑
//!    分量保留——下一帧重力继续沿坡加速，自然下滑）；
//! 5. **贴地吸附**：并入第 4 步的同一个向下扫掠——探测距离取
//!    `max(|vel.y × dt|, snap_distance)`：贴地行走 / 下小坡 / 小台阶下
//!    降时即使本帧重力位移极小也能探到 `snap_distance` 内的地面并贴下去
//!    （不腾空、y 连续）；`vel.y > 0`（跳跃上升）不吸附、不判定着地。
//!    上升帧改为向上扫掠 `vel.y × dt`（命中天花板贴住、`vel.y = 0`）。
//!
//! # 为什么胶囊放进 [`ColliderShape`]
//!
//! broadphase（`step_physics` 的网格 AABB，按形状分发）与扫掠
//! （[`shape_sweep`] 走 [`detect_pair`]）都按形状分发——把胶囊做成一等
//! 形状后，静态体、动态刚体、射线、扫掠**免费**获得胶囊支持；角色控制器
//! 只是它的正式消费方之一。
//!
//! # v1 诚实边界
//!
//! - **起点嵌入不去穿**：出生 / 传送进静态体内部时扫掠按「起点已重叠 =
//!   t = 0」处理，角色原地卡住（不做深度去穿透，调用方应保证出生点合法）；
//! - **一步一台阶**：单次调用至多接受一次抬升，同帧连续多级台阶由后续
//!   帧继续（楼梯逐帧爬升）；
//! - **全贯穿不检出（C3 起有 CCD 包装）**：单帧位移大于障碍厚度的完全
//!   贯穿沿 [`shape_sweep`] 的诚实边界返回未命中（单帧位移受 `vel × dt`
//!   自然限制，常规步长无虞）；冲刺 / 弹射等高速玩法用
//!   [`move_and_slide_ccd`] 把单帧位移拆成子步检出（刚体侧对应
//!   `physics::Ccd` 组件）；
//! - **确定性**：全程纯函数、固定迭代轮数（水平 4 轮、扫掠二分 24 次、
//!   投影收敛 4 轮）、无随机源——同输入两次调用逐位一致（单测守护）。
//!
//! # 平台跳跃衔接参数表（打磨 IV 摩擦⑥，iris-ascent 实测教训提炼）
//!
//! 「两块平台之间的衔接是否可靠」不取决于单个参数，而取决于落差 / 缝宽 /
//! 参数组合的**共同作用域**。下表以缺省参数（`radius 0.4 / step_height
//! 0.3 / snap_distance 0.2`）为基准；换参数时按「推导列」缩放。
//!
//! | 衔接场景 | 可靠域 | 推导 |
//! |---|---|---|
//! | 低台阶衔接（向上走） | 落差 ≤ `step_height` | 台阶机制一步迈上（高差超出即成「墙」，照撞不爬） |
//! | 走落衔接（向下走） | 落差 ≤ `snap_distance` | 贴地吸附兜住，不腾空、y 连续；略超也常被重力位移接住，但落地冲量阈值可能误触发「落地」表现 |
//! | 水平桥接缝 | 缝宽 < `2 × radius`（胶囊直径） | 直径内胶囊任一帧总有一端压在任一侧台面上，不可能整只漏过；≥ 直径的缝必须靠跳 |
//! | 跳板 / 落点带（弹道落点） | 落点窗口 ≥ `radius` 且落点台面法线可站立 | 落在台缘角点上法线倾斜、不可站立，会沿缘下滑或卡缘（iris-ascent 以落点窗口 ≥ 5 m 固化余量） |
//! | 平台厚度 | ≥ `radius`（薄板厚度方向） | 见下方「薄板卡缘」失败模式 |
//!
//! **超域失败模式与诊断**：
//!
//! - **贴边落在台缘角点**（最易误判为「控制器坏了」）：胶囊底帽恰好擦
//!   到台缘的立方体角点时，接触法线是 45° 斜线（不可站立），水平扫掠
//!   被角点挡住 → 台阶抬升重试的前向落点也在角点上 → 永远迈不上去，
//!   角色贴着台缘走不动。复现特征：`grounded = false` 或位置 XZ 停在
//!   距台面边缘 ≈ `radius` 处、`ground_normal` 呈 45° 倾斜。诊断：打印
//!   [`CharacterOutput::ground_normal`]（角点命中的直接证据）；修法是
//!   加大 `step_height` / 调整平台布局（避免以角点为第一接触点），而非
//!   调摩擦——本控制器没有摩擦参数。
//! - **缝宽 + 落差双超域**：缝宽 > `2 × radius` 且落差 > `step_height`
//!   的衔接以「贴边落在台缘角点」的方式卡墙，且比单超域更难定位（下
//!   坠一帧后被对面台缘接住，看似随机）。iris-ascent 的做法：以关卡
//!   不变量单测固化衔接域（相邻平台高差 ≤ 0.3、缝宽 ≤ 0.5），把「参数
//!   域」变成可回归的断言，推荐下游游戏照做。
//! - **薄板 / 薄墙穿通**：单帧水平位移大于障碍厚度的完全贯穿沿扫掠的
//!   诚实边界不检出（见下「v1 诚实边界」）——厚度 ≥ `radius` 的平台在
//!   常规速度（`vel × dt` < 厚度）下无虞；弹射 / 冲刺类玩法要么加厚
//!   碰撞体，要么用 [`move_and_slide_ccd`]（C3）将单帧位移拆小。
//!
//! # 用法
//!
//! ```ignore
//! use iris_core::character::{CharacterParams, move_and_slide};
//! use iris_core::prelude::*;
//!
//! let params = CharacterParams {
//!     radius: 0.4,
//!     half_height: 0.9,      // ≥ radius；脚底 = 中心 y − 0.9
//!     step_height: 0.3,
//!     slope_limit_radians: std::f32::consts::FRAC_PI_4,
//!     snap_distance: 0.2,
//!     gravity_scale: 1.0,
//! };
//! // 每帧：输入速度（水平意图 + 跳跃）→ 控制器 → 写回玩家 Transform
//! let out = move_and_slide(&world, pos, input_velocity, dt, &params);
//! world.get_mut::<Transform>(player).unwrap().position = out.position;
//! if out.grounded { /* HUD 落地标记；out.ground_normal 可用于坡面音效 */ }
//! ```

use glam::{Quat, Vec3};

use crate::physics::{
    Collider, ColliderShape, MAX_CCD_SEGMENTS, PHYSICS_GRAVITY, RigidBody, StaticObstacle,
    shape_sweep, shape_sweep_rotated,
};
use crate::transform::Transform;
use crate::world::World;

/// 水平移动的最大扫掠迭代次数：每轮消费一段剩余位移（直行 / 撞墙滑行 /
/// 台阶迈上），4 轮覆盖「直墙 + 角点 + 内凹角」的常规场景——超出即停
/// （剩余位移放弃，下一帧继续），保证每帧代价有界且确定。
const MAX_SLIDE_ITERATIONS: usize = 4;

/// 剩余位移的放弃阈值（`length²`）：低于此值视为走完（1e-5 米线性量级，
/// 低于任何游戏可感知精度），不再发起空扫掠。
const SLIDE_RESIDUAL_SQ: f32 = 1e-10;

/// 台阶落点高差校验容差（米）：抬升量恒等于 `step_height`，落点理论必不
/// 高于抬起点——此容差仅防御未来抬升量可参数化后的浮点边界。
const STEP_LAND_EPSILON: f32 = 1e-4;

/// 水平法线的最小长度（`length²`）：阻挡法线近乎竖直（理论上不会——
/// 竖直面才会水平阻挡）时的防御兜底，避免归一化除零。
const HORIZONTAL_NORMAL_MIN_SQ: f32 = 1e-8;

/// 角色控制器参数：竖直胶囊的几何 + 运动手感。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterParams {
    /// 胶囊半径（米）：水平方向的最大半宽（阻挡距离 = 墙面 ± radius）。
    pub radius: f32,
    /// 胶囊半高（米，须 ≥ radius）：整体包围盒半高，脚底 = 中心 y − half_height。
    pub half_height: f32,
    /// 可迈上台阶的高（米）：低台阶自动抬升迈上，高于此值的墙照撞。
    pub step_height: f32,
    /// 可站立最大坡角（弧度）：地面法线 `n.y ≥ cos(该角)` 才算可站立
    /// （45° = `FRAC_PI_4` 时即 `n.y ≥ √2/2`）；更陡的坡不 grounded、下滑。
    pub slope_limit_radians: f32,
    /// 贴地吸附距离（米）：下坡 / 小台阶下降时向下探测的额外距离——
    /// 有地面就贴下去，不腾空。
    pub snap_distance: f32,
    /// 重力缩放：1.0 = 标准重力（[`PHYSICS_GRAVITY`]），0 = 失重。
    pub gravity_scale: f32,
}

impl Default for CharacterParams {
    /// 常规第三人称角色尺寸：半径 0.4、半高 0.9（站立高 1.8 m）、
    /// 台阶 0.3、坡限 45°、吸附 0.2、标准重力。
    fn default() -> Self {
        Self {
            radius: 0.4,
            half_height: 0.9,
            step_height: 0.3,
            slope_limit_radians: std::f32::consts::FRAC_PI_4,
            snap_distance: 0.2,
            gravity_scale: 1.0,
        }
    }
}

/// 一步 `move_and_slide` 的输出（纯数据）：调用方自行写回组件。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterOutput {
    /// 本步结束时的胶囊中心位置。
    pub position: Vec3,
    /// 滑动投影后的实际速度：贴墙时切向保留、着地时垂直分量归零、
    /// 陡坡时投影到坡面（下滑分量保留）——下一帧可直接作为输入回灌。
    pub velocity: Vec3,
    /// 是否站在可站立地面上（坡限内、`snap_distance` 内探到）。
    pub grounded: bool,
    /// 站立地面的单位法线（指向角色一侧，平地 = +Y）；`grounded` 时有效，
    /// 腾空时为中性 +Y（命中不可站立坡面时也带出该坡面法线供调试）。
    pub ground_normal: Vec3,
}

/// 运动学角色控制器（纯函数核心，**胶囊恒竖直 + 静态障碍恒竖直**）：
/// 竖直胶囊从 `pos` 以 `velocity` 推进一帧 `dt`，对一组静态体做「重力 →
/// 水平滑行 → 台阶 → 斜坡限 → 贴地吸附」五步管线（见模块文档）。不触碰
/// 世界、无随机源——同输入两次调用逐位一致。
///
/// 静态障碍带旋转（倒地尸体的斜置胶囊）时用
/// [`move_and_slide_statics_rotated`]；世界封装 [`move_and_slide`] 已是
/// 旋转感知（从 `Transform` 收集）。
///
/// # Panics
/// `half_height < radius`（非竖直胶囊语义）或 `dt < 0` 时断言失败
/// （中文断言消息）。
pub fn move_and_slide_statics(
    pos: Vec3,
    velocity: Vec3,
    dt: f32,
    params: &CharacterParams,
    statics: &[(Vec3, ColliderShape)],
) -> CharacterOutput {
    let shape = ColliderShape::Capsule {
        radius: params.radius,
        half_height: params.half_height,
    };
    move_and_slide_core(pos, velocity, dt, params, |from, delta| {
        shape_sweep(from, delta, &shape, statics)
    })
}

/// 运动学角色控制器（纯函数核心，**旋转感知**，R50）：与
/// [`move_and_slide_statics`] 同一五步管线、同一断言，差别只在静态障碍
/// 快照带旋转（[`StaticObstacle`]）——动画接管期的 ragdoll 肢体是「静态 +
/// 斜置胶囊」，角色走到尸体上会被照实挡住 / 踩住（旧口径会把它当成直立
/// 胶囊，出现「踩空气」与「被隐形柱挡住」两类错觉）。
///
/// **角色胶囊自身恒竖直**（直立人体是控制器的语义前提，见模块文档）：
/// 扫掠以 `Quat::IDENTITY` 发起；需要「会倒下的胶囊」请用物理刚体
/// （[`crate::physics`]）而非角色控制器。
///
/// # Panics
/// 同 [`move_and_slide_statics`]。
pub fn move_and_slide_statics_rotated(
    pos: Vec3,
    velocity: Vec3,
    dt: f32,
    params: &CharacterParams,
    statics: &[StaticObstacle],
) -> CharacterOutput {
    let shape = ColliderShape::Capsule {
        radius: params.radius,
        half_height: params.half_height,
    };
    move_and_slide_core(pos, velocity, dt, params, |from, delta| {
        shape_sweep_rotated(from, delta, Quat::IDENTITY, &shape, statics)
    })
}

/// 五步管线核心（两个公开入口共用）：`sweep(from, delta)` 回答「该位移的
/// 首次碰撞分数与阻挡法线」，管线本身与静态体列表形态无关（旋转感知与否
/// 只由闭包决定 ⇒ 无重复实现、无行为分岔）。
fn move_and_slide_core<S>(
    pos: Vec3,
    velocity: Vec3,
    dt: f32,
    params: &CharacterParams,
    sweep: S,
) -> CharacterOutput
where
    S: Fn(Vec3, Vec3) -> Option<(f32, Vec3)>,
{
    assert!(
        params.half_height >= params.radius,
        "胶囊 half_height {} 须 ≥ radius {}（半高小于半径退化为非胶囊形状）",
        params.half_height,
        params.radius
    );
    assert!(dt >= 0.0, "步长 dt {dt} 须 ≥ 0（负步长无物理语义）");
    let walkable = params.slope_limit_radians.cos();

    let mut vel = velocity;
    // ---- 步骤 1：重力（半隐式欧拉，与 step_physics 同式）----
    vel.y += PHYSICS_GRAVITY.y * params.gravity_scale * dt;

    // ---- 步骤 2 + 3：水平移动（贴墙滑行）与台阶 ----
    let mut position = pos;
    let mut remaining = Vec3::new(vel.x * dt, 0.0, vel.z * dt);
    for _ in 0..MAX_SLIDE_ITERATIONS {
        if remaining.length_squared() <= SLIDE_RESIDUAL_SQ {
            break;
        }
        let Some((t, normal)) = sweep(position, remaining) else {
            position += remaining; // 无阻挡：走完
            break;
        };
        position += remaining * t; // 推进到命中点（扫掠皮肤已留防抖间隙）
        remaining -= remaining * t;
        // 台阶：贴地且非上升时尝试抬起重试（缓坡上坡与低台阶同一机制）
        if vel.y <= 0.0 {
            if let Some(stepped) = try_step_up(position, remaining, normal, params, &sweep) {
                position = stepped;
                break;
            }
        }
        // 贴墙滑行：可站立坡面只切剩余位移（不切速度，上坡交给台阶）；
        // 不可站立面（墙 / 陡坡）把法向分量从速度与剩余位移中同时切掉，
        // 法线先水平归一（陡坡的斜法线只取水平阻挡方向，避免把水平推力
        // 转成沿坡上滑的竖直速度分量）。
        if normal.y < walkable {
            let horizontal = Vec3::new(normal.x, 0.0, normal.z);
            if horizontal.length_squared() > HORIZONTAL_NORMAL_MIN_SQ {
                let n = horizontal / horizontal.length();
                let into = vel.dot(n);
                if into < 0.0 {
                    vel -= n * into;
                }
                let into_move = remaining.dot(n);
                if into_move < 0.0 {
                    remaining -= n * into_move;
                }
            }
        } else {
            let into_move = remaining.dot(normal);
            if into_move < 0.0 {
                remaining -= normal * into_move;
            }
        }
        remaining.y = 0.0; // 水平阶段位移恒水平（竖直运动归第 4/5 步）
    }

    // ---- 步骤 4 + 5：垂直移动、斜坡限与贴地吸附（同一次向下扫掠）----
    let mut grounded = false;
    let mut ground_normal = Vec3::Y;
    if vel.y > 0.0 {
        // 上升（跳跃）：不吸附、不判定着地；向上扫掠命中天花板则贴住
        let up = Vec3::new(0.0, vel.y * dt, 0.0);
        if let Some((t, _)) = sweep(position, up) {
            position += up * t;
            vel.y = 0.0;
        } else {
            position += up;
        }
    } else {
        // 下落 / 静止：向下扫掠 max(|vel.y·dt|, snap_distance)——
        // 吸附语义：贴地行走 / 下小坡时重力位移极小也能探到近处地面贴下去
        let probe = vel.y * dt; // ≤ 0
        let down = Vec3::new(0.0, -probe.abs().max(params.snap_distance), 0.0);
        match sweep(position, down) {
            None => {
                position.y += probe; // 腾空：只走本帧重力位移
                ground_normal = Vec3::Y;
            }
            Some((t, normal)) => {
                position += down * t; // 贴到命中点（吸附 / 着地 / 沿陡坡贴面）
                ground_normal = normal;
                if normal.y >= walkable {
                    vel.y = 0.0; // 可站立：grounded、垂直分量归零
                    grounded = true;
                } else {
                    // 陡坡：不 grounded；切掉入坡法向分量，沿坡面的下滑
                    // 分量保留——下一帧重力继续沿坡加速，自然下滑
                    let into = vel.dot(normal);
                    if into < 0.0 {
                        vel -= normal * into;
                    }
                }
            }
        }
    }

    CharacterOutput {
        position,
        velocity: vel,
        grounded,
        ground_normal,
    }
}

/// 世界封装的角色控制器：收集世界中全部静态体（[`crate::physics::
/// effective_static`] 口径——[`Collider::is_static`]、仅 Collider 无
/// RigidBody 的组件缺席语义，或 `body_type == Kinematic` 的运动学体
/// ——运动学体按静态障碍处理：游戏驱动位移的碰撞体挡住角色是合理
/// 语义，与 [`crate::physics::resolve_against_statics`] 一致）后调用
/// 纯函数核心 [`move_and_slide_statics_rotated`]。纯查询：不写任何组件。
///
/// **旋转感知（R50）**：收集带 `Transform.rotation`——动画接管期的
/// ragdoll 肢体是「静态 + 斜置胶囊」，照实按其倒下的朝向阻挡 / 承托
/// （旧口径当成直立胶囊，会出现「踩空气」与「被隐形柱挡住」）。
pub fn move_and_slide(
    world: &World,
    pos: Vec3,
    velocity: Vec3,
    dt: f32,
    params: &CharacterParams,
) -> CharacterOutput {
    let mut statics: Vec<StaticObstacle> = Vec::new();
    world.query::<(&Transform, &Collider, Option<&RigidBody>)>(|(transform, collider, rigid)| {
        if crate::physics::effective_static(collider, rigid) {
            statics.push((transform.position, transform.rotation, collider.shape));
        }
    });
    move_and_slide_statics_rotated(pos, velocity, dt, params, &statics)
}

/// 运动学角色控制器的 **CCD 包装**（C3 闭合轮）：把单帧位移拆成至多
/// [`MAX_CCD_SEGMENTS`] 段、每段 ≤ 段长的子步，逐段走完整五步管线并
/// 链式回灌 `(position, velocity)`——冲刺 / 弹射玩法下「全贯穿不检出」
/// （[`move_and_slide`] 的诚实边界）被闭合：任何一段的扫掠位移都不
/// 大于胶囊宽度量级，薄墙无法整段被跳过。
///
/// - **段长**：`max_segment > 0` 且有限取显式值（米）；`0` / 负 / 非
///   有限 = 自动取 `params.radius`（胶囊最小宽度 `2r` 的一半，保守
///   捕获界）。显式段长大于 `2 × radius` 时捕获保证消失（自担）；
/// - **亚阈值兼容锚**：`|velocity| × dt ≤ 段长` ⇒ `steps = 1`，结果与
///   [`move_and_slide_statics_rotated`] **逐位一致**（同一次调用，无
///   拆分歧义）——常规行走速度不因此轮改变任何行为；
/// - **代价**：每子步完整跑五步管线（水平 4 轮 + 台阶 + 向下扫掠），
///   成本 × 子步数；静态快照只收集一次（子步间障碍不动，与单帧
///   语义一致）；
/// - **确定性**：子步数 = `ceil(|v|×dt / 段长)`（输入速度的纯函数、
///   封顶常数），子步长 `dt / steps` 为确定除法——同输入两次调用
///   逐位一致（单测守护）；重力按子步分摊（`n × g·dt/n`），与单次
///   `g·dt` 有浮点结合序差——新入口无 v1 逐位义务，仅保证自身确定。
pub fn move_and_slide_ccd(
    world: &World,
    pos: Vec3,
    velocity: Vec3,
    dt: f32,
    params: &CharacterParams,
    max_segment: f32,
) -> CharacterOutput {
    let mut statics: Vec<StaticObstacle> = Vec::new();
    world.query::<(&Transform, &Collider, Option<&RigidBody>)>(|(transform, collider, rigid)| {
        if crate::physics::effective_static(collider, rigid) {
            statics.push((transform.position, transform.rotation, collider.shape));
        }
    });
    move_and_slide_statics_rotated_ccd(pos, velocity, dt, params, max_segment, &statics)
}

/// [`move_and_slide_ccd`] 的纯函数核心（静态障碍快照显式给出，旋转
/// 感知口径）——脱离 World 单测。断言同 [`move_and_slide_statics`]。
///
/// # Panics
/// 同 [`move_and_slide_statics`]（首个子步即由内核断言）。
pub fn move_and_slide_statics_rotated_ccd(
    pos: Vec3,
    velocity: Vec3,
    dt: f32,
    params: &CharacterParams,
    max_segment: f32,
    statics: &[StaticObstacle],
) -> CharacterOutput {
    let step = if max_segment.is_finite() && max_segment > 0.0 {
        max_segment
    } else {
        params.radius
    };
    // 规划口径：输入速度 × dt 的水平 / 合成位移预估（竖直向下落每帧
    // 重力增量 g·dt ≪ 半径，不构成贯穿源；见函数文档）
    let planned = velocity.length() * dt;
    let steps = if step > 0.0 {
        (planned / step).ceil() as usize
    } else {
        1
    };
    let steps = steps.clamp(1, MAX_CCD_SEGMENTS);
    let sub_dt = dt / steps as f32;
    let mut position = pos;
    let mut vel = velocity;
    let mut out: Option<CharacterOutput> = None;
    for _ in 0..steps {
        let next = move_and_slide_statics_rotated(position, vel, sub_dt, params, statics);
        position = next.position;
        vel = next.velocity;
        out = Some(next);
    }
    // steps ≥ 1 ⇒ 循环必执行（钳制语义，非可失败的运行时检查）
    out.expect("CCD 子步数 ≥ 1，输出必存在")
}

/// 台阶尝试（水平被阻挡时调用）：抬起 `step_height` 重试水平位移，成功
/// 落回且落点可站立才接受。返回 `Some(落点)` = 接受抬升（位移已消费
/// 完毕）；`None` = 拒绝（调用方走贴墙滑行）。算法与门禁见模块文档
/// 第 3 步。
///
/// `blocker_normal` 为水平扫掠的阻挡法线：**不可站立面**（墙 / 陡坡面，
/// `n.y < cos(slope_limit)`）的抬升重试把前向距离加到至少 `radius`——
/// 台阶迎风面是「墙」，中心须送进台面 radius 深、落到平顶（法线 +Y），
/// 贴边落在台缘角点上的法线倾斜、不可站立，会永远迈不上去；**可站立
/// 坡面**按剩余位移原样重试——缓坡上坡靠逐帧小步，radius 前跳会让
/// 坡面移动快过输入速度。
fn try_step_up<S>(
    position: Vec3,
    remaining: Vec3,
    blocker_normal: Vec3,
    params: &CharacterParams,
    sweep: &S,
) -> Option<Vec3>
where
    S: Fn(Vec3, Vec3) -> Option<(f32, Vec3)>,
{
    // 门 1：脚下 snap_distance 内必须有地面（贴地才迈，防空中跨步）
    sweep(position, Vec3::new(0.0, -params.snap_distance, 0.0))?;
    // 门 2：正上方 step_height 内畅通（有天花板压顶不迈，保守）
    let lift = Vec3::new(0.0, params.step_height, 0.0);
    if sweep(position, lift).is_some() {
        return None;
    }
    // 前向距离：不可站立阻挡至少 radius（方向与剩余位移一致）
    let distance = remaining.length();
    let forward = if blocker_normal.y < params.slope_limit_radians.cos() {
        distance.max(params.radius)
    } else {
        distance
    };
    let forward_vec = if distance > 0.0 {
        remaining / distance * forward
    } else {
        remaining
    };
    let lifted = position + lift;
    // 门 3：抬起后前向位移必须完整走完（否则视为不可整体迈上）
    if sweep(lifted, forward_vec).is_some() {
        return None;
    }
    let moved = lifted + forward_vec;
    // 门 4：向下扫掠 step_height + snap_distance 必须落回地面
    //（落不回 = 台阶另一侧悬空 / 是坑，不迈）
    let down = Vec3::new(0.0, -(params.step_height + params.snap_distance), 0.0);
    let (t, normal) = sweep(moved, down)?;
    let landed = moved + down * t;
    // 校验：脚底高差 ≤ step_height（抬升量即 step_height 时恒成立，
    // 防御性保留）且落点法线可站立（陡坡坡面 / 台缘角点不算站稳）
    if landed.y - position.y > params.step_height + STEP_LAND_EPSILON {
        return None;
    }
    if normal.y < params.slope_limit_radians.cos() {
        return None;
    }
    Some(landed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::ColliderShape;

    /// 常规测试步长 60Hz（与 physics.rs 测试同款）。
    const DT: f32 = 1.0 / 60.0;

    /// 标准角色参数（与 [`CharacterParams::default`] 同值，显式写出便于
    /// 测试自读）。
    fn params() -> CharacterParams {
        CharacterParams {
            radius: 0.4,
            half_height: 0.9,
            step_height: 0.3,
            slope_limit_radians: std::f32::consts::FRAC_PI_4,
            snap_distance: 0.2,
            gravity_scale: 1.0,
        }
    }

    /// 顶面 y = 0 的大静态平台（地面，盒心 (0,-1,0) 半边长 (50,1,50)）。
    fn floor() -> (Vec3, ColliderShape) {
        (
            Vec3::new(0.0, -1.0, 0.0),
            ColliderShape::Box {
                half_extents: Vec3::new(50.0, 1.0, 50.0),
            },
        )
    }

    /// 贴地站立的胶囊中心（脚底 y = 0 → 中心 = half_height）。
    fn standing(x: f32, z: f32) -> Vec3 {
        Vec3::new(x, 0.9, z)
    }

    /// 斜面「大山球」：半径 `r` 的静态球心在原点正下方 `r` 处（顶点在
    /// y = 0），距顶点弧角 `theta` 处的表面点与外法线解析可算——
    /// 法线 = (sin θ, cos θ, 0)，表面高度 = −r + r·cos θ 的球面。
    fn hill(r: f32) -> (Vec3, ColliderShape) {
        (Vec3::new(0.0, -r, 0.0), ColliderShape::Sphere { radius: r })
    }

    /// 竖直胶囊在大山球上的**贴地静置**中心高：斜面上的胶囊以底端帽球
    /// 与山球相切静置（端帽球心与山球心距离 = r + 端帽半径，沿连心线），
    /// 而非「表面点正上方」——把 surface + half_height 当静置高会让胶囊
    /// 嵌进坡里。端帽球心 y = −r + √((r+ρ)² − x²)，中心再抬半轴
    /// (half_height − radius = 0.5)。
    fn rest_center_y(r: f32, x: f32) -> f32 {
        let rho = 0.4f32; // params().radius
        let cap_y = -r + ((r + rho) * (r + rho) - x * x).sqrt();
        cap_y + 0.5
    }

    // ---- 平地 ----

    /// 平地行走：grounded、y 稳定在 half_height、速度原样保留。
    #[test]
    fn flat_ground_walk_is_grounded_and_level() {
        let statics = [floor()];
        let out = move_and_slide_statics(
            standing(0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            DT,
            &params(),
            &statics,
        );
        assert!(out.grounded, "平地行走应 grounded");
        assert_eq!(out.ground_normal, Vec3::Y, "平地法线 = +Y");
        assert!(
            (out.position.y - 0.9).abs() < 1e-4,
            "y 应稳定在 half_height，实际 {}",
            out.position.y
        );
        assert!(
            (out.position.x - 2.0 * DT).abs() < 1e-5,
            "x 应按 vel·dt 前进，实际 {}",
            out.position.x
        );
        assert_eq!(out.velocity, Vec3::new(2.0, 0.0, 0.0), "速度应原样保留");
    }

    // ---- 墙：阻挡与贴墙滑行 ----

    /// 内缘 x = 4.5 的高墙（盒心 (5,2,0) 半边长 (0.5,2,5)）。
    fn wall() -> (Vec3, ColliderShape) {
        (
            Vec3::new(5.0, 2.0, 0.0),
            ColliderShape::Box {
                half_extents: Vec3::new(0.5, 2.0, 5.0),
            },
        )
    }

    /// 正对墙走：停在墙前 radius 处、法向速度被切掉、仍 grounded。
    #[test]
    fn wall_stops_at_radius_and_cuts_normal_velocity() {
        let statics = [floor(), wall()];
        // 球面到墙面 0.1，本帧位移 0.2 → 撞墙停在内缘 − 半径
        let out = move_and_slide_statics(
            standing(4.0, 0.0),
            Vec3::new(12.0, 0.0, 0.0),
            DT,
            &params(),
            &statics,
        );
        assert!(
            (out.position.x - (4.5 - 0.4)).abs() < 1e-3,
            "应停在墙前 radius 处（4.1），实际 {}",
            out.position.x
        );
        assert_eq!(out.velocity.x, 0.0, "法向（+X）速度应被切掉");
        assert!(out.grounded, "贴墙站立仍应 grounded");
        assert!(
            (out.position.y - 0.9).abs() < 1e-4,
            "贴墙不改变站立高度，实际 {}",
            out.position.y
        );
    }

    /// 斜向走向墙：位置被挡、切向速度保留（贴墙滑行）。
    #[test]
    fn wall_slide_preserves_tangential_velocity() {
        let statics = [floor(), wall()];
        let out = move_and_slide_statics(
            standing(4.0, 0.0),
            Vec3::new(12.0, 0.0, 4.0),
            DT,
            &params(),
            &statics,
        );
        assert!(
            (out.position.x - 4.1).abs() < 1e-3,
            "x 应停在墙前，实际 {}",
            out.position.x
        );
        assert!(
            (out.velocity.x).abs() < 1e-6,
            "法向速度应归零，实际 {}",
            out.velocity.x
        );
        assert!(
            (out.velocity.z - 4.0).abs() < 1e-6,
            "切向（z）速度应完整保留，实际 {}",
            out.velocity.z
        );
        assert!(
            out.position.z > 0.0,
            "同帧应沿墙滑过一段 z，实际 {}",
            out.position.z
        );
    }

    // ---- 台阶 ----

    /// 顶面 y = 0.25（≤ step_height 0.3）的低台阶，迎风面 x = 2.0。
    fn low_step() -> (Vec3, ColliderShape) {
        (
            Vec3::new(2.5, 0.125, 0.0),
            ColliderShape::Box {
                half_extents: Vec3::new(0.5, 0.125, 1.0),
            },
        )
    }

    /// 顶面 y = 1.0（> step_height 0.3）的高墙，迎风面 x = 2.0。
    fn high_wall() -> (Vec3, ColliderShape) {
        (
            Vec3::new(2.5, 0.5, 0.0),
            ColliderShape::Box {
                half_extents: Vec3::new(0.5, 0.5, 1.0),
            },
        )
    }

    /// 低台阶（高 ≤ step_height）：一步迈上并站稳。
    #[test]
    fn low_step_is_climbed() {
        let statics = [floor(), low_step()];
        // 球面距台阶面 0.1，本帧位移 0.2 → 先被挡在面 − 半径（1.6），
        // 台阶面是「墙」→ 抬升重试前向 max(剩余 0.1, radius 0.4) = 0.4，
        // 中心落到台缘正上方（2.0）的平顶边缘（法线 +Y）→ 接受
        let out = move_and_slide_statics(
            standing(1.5, 0.0),
            Vec3::new(12.0, 0.0, 0.0),
            DT,
            &params(),
            &statics,
        );
        assert!(out.grounded, "迈上台阶应站稳");
        assert!(
            (out.position.y - (0.25 + 0.9)).abs() < 1e-3,
            "应站在台阶顶（中心 y = 1.15），实际 {}",
            out.position.y
        );
        // 首次接触并非迎风面（x = 1.6）而是底端帽擦到台缘角点：
        // 端帽球心 (x, 0.4) 距角点 (2.0, 0.25) = 0.4 → x = 2 − √(0.16 −
        // 0.0225) ≈ 1.6292；radius 前跳后中心 = 1.6292 + 0.4 ≈ 2.0292，
        // 落在台面平顶上（法线 +Y）→ 接受
        assert!(
            (out.position.x - 2.0292).abs() < 1e-3,
            "中心应落在台面上（x ≈ 2.0292），实际 {}",
            out.position.x
        );
    }

    /// 高墙（高 > step_height）：不抬升、被挡在墙前 radius 处。
    #[test]
    fn high_wall_is_not_climbed() {
        let statics = [floor(), high_wall()];
        let out = move_and_slide_statics(
            standing(1.5, 0.0),
            Vec3::new(12.0, 0.0, 0.0),
            DT,
            &params(),
            &statics,
        );
        assert!(
            (out.position.x - (2.0 - 0.4)).abs() < 1e-3,
            "应被挡在墙前 radius 处（1.6），实际 {}",
            out.position.x
        );
        assert!(
            (out.position.y - 0.9).abs() < 1e-4,
            "不应抬升，实际 y = {}",
            out.position.y
        );
        assert_eq!(out.velocity.x, 0.0, "法向速度应被切掉");
        assert!(out.grounded, "墙脚站立仍 grounded");
    }

    // ---- 斜坡（大山球解析斜面）----

    /// 缓坡（30° < 坡限 45°）：上坡 grounded、贴地（y 跟随静置解析高度）。
    #[test]
    fn gentle_slope_uphill_is_grounded_and_glued() {
        let r = 10.0f32;
        let statics = [hill(r)];
        // 30° 处：x = r·sin30° = 5，静置中心 = rest_center_y（端帽相切），
        // 抬高 1 cm 起步（首帧吸附贴地）
        let start = Vec3::new(5.0, rest_center_y(r, 5.0) + 0.01, 0.0);
        let mut out =
            move_and_slide_statics(start, Vec3::new(-3.0, 0.0, 0.0), DT, &params(), &statics);
        assert!(out.grounded, "缓坡首帧应吸附 grounded");
        for _ in 0..20 {
            out = move_and_slide_statics(
                out.position,
                Vec3::new(-3.0, out.velocity.y, 0.0), // 每帧重灌水平意图
                DT,
                &params(),
                &statics,
            );
            assert!(out.grounded, "缓坡上坡应全程 grounded");
        }
        let expected_y = rest_center_y(r, out.position.x);
        assert!(
            (out.position.y - expected_y).abs() < 5e-3,
            "应贴地（y {} ≈ 静置 {}），实际差 {}",
            out.position.y,
            expected_y,
            (out.position.y - expected_y).abs()
        );
        assert!(
            out.position.x < 4.8,
            "应持续上坡（x 从 5 递减），实际 {}",
            out.position.x
        );
    }

    /// 陡坡（60° > 坡限 45°）：不 grounded、下滑速度存在且沿坡加速。
    /// 只滑 0.5 s——大山球是凸面，滑近赤道（x → r）坡度趋于竖直，脱离
    /// 贴面属凸面真实几何而非控制器缺陷，测试不跨入该区段。
    #[test]
    fn steep_slope_is_not_grounded_and_slides_down() {
        let r = 10.0f32;
        let statics = [hill(r)];
        // 60° 处：x = r·sin60° ≈ 8.66，法线 (sin60, cos60, 0) 不可站立；
        // 静置中心 = rest_center_y（端帽相切），抬高 1 cm 起步
        let start = Vec3::new(8.660_254, rest_center_y(r, 8.660_254) + 0.01, 0.0);
        let mut out = move_and_slide_statics(start, Vec3::ZERO, DT, &params(), &statics);
        assert!(!out.grounded, "陡坡首帧即不应 grounded");
        for _ in 0..30 {
            out = move_and_slide_statics(out.position, out.velocity, DT, &params(), &statics);
            assert!(!out.grounded, "陡坡全程不应 grounded");
        }
        // 下滑方向 = 远离坡顶的坡面切向（cos60, −sin60）：速度沿坡面分量
        // 应显著存在（无摩擦模型下 ≈ g·sin60°·t ≈ 4.2 m/s @ 0.5 s）
        let along_slope = out.velocity.x * 0.5 - out.velocity.y * 0.866_025_4;
        assert!(
            out.velocity.x > 1.0,
            "0.5 秒后应有明显下滑速度，实际 {:?}",
            out.velocity
        );
        assert!(
            along_slope > 1.5,
            "沿坡下滑分量应 > 1.5 m/s，实际 {along_slope}"
        );
        assert!(
            out.position.x > 8.7,
            "应沿坡下滑（x 递增），实际 {}",
            out.position.x
        );
        // 贴面：y 仍跟随静置高度（陡坡贴面下滑而非腾空坠落）
        let expected_y = rest_center_y(r, out.position.x);
        assert!(
            (out.position.y - expected_y).abs() < 5e-3,
            "陡坡下滑应贴面（y {} ≈ {}），实际差 {}",
            out.position.y,
            expected_y,
            (out.position.y - expected_y).abs()
        );
    }

    /// 下小坡：snap_distance 吸附生效——全程 grounded、y 连续（不腾空）。
    #[test]
    fn downhill_snap_keeps_continuous_height() {
        let r = 10.0f32;
        let statics = [hill(r)];
        // 从坡顶附近向下坡方向（+x）行走：每帧落差 ≈ tan30°·位移 ≈ 0.029
        // << snap_distance 0.2 → 应全程贴地不腾空
        let start = Vec3::new(0.2, rest_center_y(r, 0.2) + 0.01, 0.0);
        let mut out =
            move_and_slide_statics(start, Vec3::new(3.0, 0.0, 0.0), DT, &params(), &statics);
        for _ in 0..20 {
            out = move_and_slide_statics(
                out.position,
                Vec3::new(3.0, out.velocity.y, 0.0),
                DT,
                &params(),
                &statics,
            );
            assert!(out.grounded, "下小坡应全程吸附 grounded");
            let expected_y = rest_center_y(r, out.position.x);
            assert!(
                (out.position.y - expected_y).abs() < 5e-3,
                "y 应连续跟随静置高度（不腾空）：y {} vs {expected_y}",
                out.position.y
            );
        }
    }

    // ---- 跳跃 ----

    /// 跳跃：上升帧不吸附不 grounded、离地到抛物线顶点、重力回落再落地。
    #[test]
    fn jump_rises_falls_and_lands() {
        let statics = [floor()];
        let mut out = move_and_slide_statics(
            standing(0.0, 0.0),
            Vec3::new(0.0, 7.0, 0.0), // 跳：v₀ = 7 → 理论顶点 ≈ 0.9 + 7²/2g ≈ 3.4
            DT,
            &params(),
            &statics,
        );
        let mut apex = 0.0f32;
        let mut rose = false;
        for _ in 0..120 {
            // 上升帧（vel.y > 0）不吸附：grounded 必为 false
            if out.velocity.y > 0.0 {
                assert!(!out.grounded, "上升帧不应吸附判定 grounded");
            }
            apex = apex.max(out.position.y);
            if out.position.y > 2.0 {
                rose = true;
            }
            out = move_and_slide_statics(out.position, out.velocity, DT, &params(), &statics);
            if out.grounded && rose {
                break; // 已回落落地
            }
        }
        assert!(rose, "跳跃应明显离地（y > 2）");
        assert!(
            (apex - (0.9 + 7.0 * 7.0 / (2.0 * 9.81))).abs() < 0.15,
            "顶点应接近抛物线解析值 3.4，实际 {apex}"
        );
        assert!(out.grounded, "最终应回落落地");
        assert!(
            (out.position.y - 0.9).abs() < 1e-3,
            "落地后 y 应回到 half_height，实际 {}",
            out.position.y
        );
        assert_eq!(out.velocity.y, 0.0, "落地后垂直速度归零");
    }

    // ---- 确定性 ----

    /// 同输入（平地 + 墙 + 台阶 + 大山球，固定输入序列）两次调用逐位一致。
    #[test]
    fn move_and_slide_is_deterministic() {
        let r = 10.0f32;
        let statics = [
            floor(),
            wall(),
            low_step(),
            (
                Vec3::new(-14.0, -r, 0.0),
                ColliderShape::Sphere { radius: r },
            ),
        ];
        // 一条走过所有障碍的固定输入序列（坐标写死，无随机源）
        let inputs = [
            (Vec3::new(4.0, 0.9, 0.0), Vec3::new(12.0, 0.0, 0.0)),
            (Vec3::new(1.5, 0.9, 0.0), Vec3::new(12.0, 0.0, 0.0)),
            (Vec3::new(0.2, 0.9, 0.0), Vec3::new(3.0, 0.0, 0.0)),
            (Vec3::new(0.0, 0.9, 0.0), Vec3::new(0.0, 7.0, 0.0)),
            (Vec3::new(4.6, 0.9, 0.0), Vec3::new(4.0, 0.0, 3.0)),
            (Vec3::new(-3.0, 0.9, 0.0), Vec3::new(-4.0, 0.0, 0.0)),
        ];
        let mut trace_a: Vec<CharacterOutput> = Vec::new();
        let mut trace_b: Vec<CharacterOutput> = Vec::new();
        for &(pos, vel) in &inputs {
            let mut out = move_and_slide_statics(pos, vel, DT, &params(), &statics);
            // 每个起点再推 30 帧（速度回灌），覆盖滑行 / 台阶 / 跳跃 / 下滑
            trace_a.push(out);
            trace_b.push(out);
            for _ in 0..30 {
                out = move_and_slide_statics(out.position, out.velocity, DT, &params(), &statics);
                trace_a.push(out);
                trace_b.push(out);
            }
        }
        assert_eq!(
            trace_a, trace_b,
            "同输入两次模拟应逐采样 bit 级一致（CharacterOutput 全字段相等）"
        );
    }

    /// 空静态体：无阻挡自由运动（重力照常、无着地）。
    #[test]
    fn empty_statics_free_motion() {
        let out = move_and_slide_statics(
            Vec3::new(0.0, 10.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            DT,
            &params(),
            &[],
        );
        assert!(!out.grounded);
        assert_eq!(out.ground_normal, Vec3::Y, "腾空时法线为中性 +Y");
        assert!(
            (out.position.y - (10.0 - 9.81 * DT * DT)).abs() < 1e-6,
            "重力位移应生效，实际 {}",
            out.position.y
        );
        assert!((out.position.x - DT).abs() < 1e-6);
    }

    // ---- C3 CCD 包装（全贯穿检出的闭合） ----

    /// 逐位指纹（CCD 兼容锚用）：分量 `to_bits` 数组。
    fn bits3(v: Vec3) -> [u32; 3] {
        [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()]
    }

    /// 薄墙（x∈[0.5,0.7]，高 4 m）静态快照。
    fn thin_wall_statics() -> Vec<StaticObstacle> {
        vec![(
            Vec3::new(0.6, 0.9, 0.0),
            Quat::IDENTITY,
            ColliderShape::Box {
                half_extents: Vec3::new(0.1, 2.0, 2.0),
            },
        )]
    }

    /// 高速冲刺（120 m/s，单帧位移 2 m ≫ 墙厚 + 胶囊宽）：v1 全贯穿
    /// 不检出（终点越过重叠窗口 [0.1, 1.1]，x 落到 −0.8）；CCD 包装
    /// 拆子步后首个嵌入终点 (0.8) 被扫掠括住 ⇒ 挡在墙近面外（x≈1.1）。
    #[test]
    fn ccd_wrapper_blocks_dash_through_thin_wall() {
        let statics = thin_wall_statics();
        let start = Vec3::new(1.2, 0.9, 0.0);
        let dash = Vec3::new(-120.0, 0.0, 0.0);
        let tunneled = move_and_slide_statics_rotated(start, dash, DT, &params(), &statics);
        assert!(
            tunneled.position.x < 0.1,
            "v1 对照：单帧应整段穿过薄墙（bug 在场证明，x={}）",
            tunneled.position.x
        );
        let blocked = move_and_slide_statics_rotated_ccd(start, dash, DT, &params(), 0.0, &statics);
        assert!(
            blocked.position.x > 0.9,
            "CCD 包装应挡在墙近面外（x≈1.1，实际 {}）",
            blocked.position.x
        );
    }

    /// 亚阈值兼容锚：`|v|×dt ≤ 段长` ⇒ 单段调用，结果与
    /// [`move_and_slide_statics_rotated`] 逐位一致（新入口对常规速度
    /// 零行为改变）。
    #[test]
    fn ccd_wrapper_below_threshold_matches_single_call_bitwise() {
        let statics: Vec<StaticObstacle> = vec![(
            Vec3::new(0.0, -1.0, 0.0),
            Quat::IDENTITY,
            ColliderShape::Box {
                half_extents: Vec3::new(50.0, 1.0, 50.0),
            },
        )];
        let single = move_and_slide_statics_rotated(
            standing(3.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            DT,
            &params(),
            &statics,
        );
        let wrapped = move_and_slide_statics_rotated_ccd(
            standing(3.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            DT,
            &params(),
            0.0,
            &statics,
        );
        assert_eq!(
            bits3(wrapped.position),
            bits3(single.position),
            "steps=1 ⇒ 位置逐位 = 单次调用"
        );
        assert_eq!(
            bits3(wrapped.velocity),
            bits3(single.velocity),
            "steps=1 ⇒ 速度逐位 = 单次调用"
        );
        assert_eq!(wrapped.grounded, single.grounded);
    }

    /// 确定性：高速冲刺场景两跑 10 帧，逐帧 `(position, velocity)`
    /// to_bits 全等。
    #[test]
    fn ccd_wrapper_is_deterministic_across_two_runs() {
        let run = || {
            let statics = thin_wall_statics();
            let mut pos = Vec3::new(4.0, 0.9, 0.0);
            let mut vel = Vec3::new(-120.0, 0.0, 0.0);
            let mut trace = Vec::new();
            for _ in 0..10 {
                let out =
                    move_and_slide_statics_rotated_ccd(pos, vel, DT, &params(), 0.0, &statics);
                pos = out.position;
                vel = out.velocity;
                trace.push((bits3(pos), bits3(vel)));
            }
            trace
        };
        assert_eq!(run(), run(), "CCD 包装同输入两跑逐位一致");
    }
}
