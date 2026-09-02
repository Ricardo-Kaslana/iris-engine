//! 运动学角色控制器：竖直胶囊（[`ColliderShape::Capsule`]）+
//! `move_and_slide`——R21 球扫掠（[`shape_sweep`] / [`detect_pair`] 二分）
//! 的正式消费方。
//!
//! 与 [`crate::physics::resolve_static_push`] 同款哲学：**查询而非系统**
//! ——纯函数核心（[`move_and_slide_statics`]，静态体列表入参）可脱离
//! 世界单测；世界封装 [`move_and_slide`] 只收集静态体（语义与
//! `resolve_against_statics` 一致：`is_static` 或仅 Collider 无 RigidBody
//! 的组件缺席语义），不积分、不写任何组件。调用方（游戏逻辑）保留输入
//! 与状态所有权：每帧传入位置 / 速度，拿回结果自行写回。
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
//! - **全贯穿不检出**：单帧位移大于障碍厚度的完全贯穿沿 [`shape_sweep`]
//!   的诚实边界返回未命中（单帧位移受 `vel × dt` 自然限制，常规步长无虞）；
//! - **确定性**：全程纯函数、固定迭代轮数（水平 4 轮、扫掠二分 24 次、
//!   投影收敛 4 轮）、无随机源——同输入两次调用逐位一致（单测守护）。
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

use glam::Vec3;

use crate::physics::{Collider, ColliderShape, PHYSICS_GRAVITY, RigidBody, shape_sweep};
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

/// 运动学角色控制器（纯函数核心）：竖直胶囊从 `pos` 以 `velocity` 推进
/// 一帧 `dt`，对一组静态体做「重力 → 水平滑行 → 台阶 → 斜坡限 → 贴地
/// 吸附」五步管线（见模块文档）。不触碰世界、无随机源——同输入两次
/// 调用逐位一致。
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
    assert!(
        params.half_height >= params.radius,
        "胶囊 half_height {} 须 ≥ radius {}（半高小于半径退化为非胶囊形状）",
        params.half_height,
        params.radius
    );
    assert!(dt >= 0.0, "步长 dt {dt} 须 ≥ 0（负步长无物理语义）");
    let shape = ColliderShape::Capsule {
        radius: params.radius,
        half_height: params.half_height,
    };
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
        let Some((t, normal)) = shape_sweep(position, remaining, &shape, statics) else {
            position += remaining; // 无阻挡：走完
            break;
        };
        position += remaining * t; // 推进到命中点（扫掠皮肤已留防抖间隙）
        remaining -= remaining * t;
        // 台阶：贴地且非上升时尝试抬起重试（缓坡上坡与低台阶同一机制）
        if vel.y <= 0.0 {
            if let Some(stepped) = try_step_up(position, remaining, normal, &shape, params, statics)
            {
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
        if let Some((t, _)) = shape_sweep(position, up, &shape, statics) {
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
        match shape_sweep(position, down, &shape, statics) {
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

/// 世界封装的角色控制器：收集世界中全部静态体（[`Collider::is_static`]
/// 或仅 Collider 无 RigidBody 的组件缺席语义，与
/// [`crate::physics::resolve_against_statics`] 一致）后调用纯函数核心
/// [`move_and_slide_statics`]。纯查询：不写任何组件。
pub fn move_and_slide(
    world: &World,
    pos: Vec3,
    velocity: Vec3,
    dt: f32,
    params: &CharacterParams,
) -> CharacterOutput {
    let mut statics: Vec<(Vec3, ColliderShape)> = Vec::new();
    world.query::<(&Transform, &Collider, Option<&RigidBody>)>(|(transform, collider, rigid)| {
        if collider.is_static || rigid.is_none() {
            statics.push((transform.position, collider.shape));
        }
    });
    move_and_slide_statics(pos, velocity, dt, params, &statics)
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
fn try_step_up(
    position: Vec3,
    remaining: Vec3,
    blocker_normal: Vec3,
    shape: &ColliderShape,
    params: &CharacterParams,
    statics: &[(Vec3, ColliderShape)],
) -> Option<Vec3> {
    // 门 1：脚下 snap_distance 内必须有地面（贴地才迈，防空中跨步）
    shape_sweep(
        position,
        Vec3::new(0.0, -params.snap_distance, 0.0),
        shape,
        statics,
    )?;
    // 门 2：正上方 step_height 内畅通（有天花板压顶不迈，保守）
    let lift = Vec3::new(0.0, params.step_height, 0.0);
    if shape_sweep(position, lift, shape, statics).is_some() {
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
    if shape_sweep(lifted, forward_vec, shape, statics).is_some() {
        return None;
    }
    let moved = lifted + forward_vec;
    // 门 4：向下扫掠 step_height + snap_distance 必须落回地面
    //（落不回 = 台阶另一侧悬空 / 是坑，不迈）
    let down = Vec3::new(0.0, -(params.step_height + params.snap_distance), 0.0);
    let (t, normal) = shape_sweep(moved, down, shape, statics)?;
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
}
