//! 物理与碰撞 v1：轻量刚体物理（Sphere / AABB），积分 → 检测 → 响应 → 迭代松弛。
//!
//! 与 [`crate::transform`] / [`crate::hierarchy`] 同款的「组件 + 单一系统」模型：
//! - [`Collider`]：碰撞体（形状 + 是否静态 + 弹性）；
//! - [`RigidBody`]：刚体（速度 / 重力缩放 / 摩擦 / 线性阻尼）；
//! - [`step_physics`]：每帧推进一步的物理系统（`&World` + 尝试型锁，可注册并行系统）。
//!
//! # 组件缺席语义
//!
//! - 只有 `Collider`（无 `RigidBody`）→ **静态体**（与 `is_static = true` 取或）：
//!   不积分、不移动，只作为碰撞对象（地面 / 墙 / 台面）；
//! - 只有 `RigidBody`（无 `Collider`）→ **仅受力积分**（重力 / 阻尼照常，
//!   不参与任何碰撞）；
//! - 两者都有且 `Collider::is_static == false` → 完整刚体（积分 + 碰撞）。
//!
//! # 求解管线
//!
//! [`step_physics`] 只做编排，全部数值逻辑在纯函数 [`detect_pair`] /
//! [`solve_contact`] 中（可脱离世界单测）。每帧六步：
//!
//! 1. **两阶段收集**（照 `update_animations` 模式）：读 `Time.delta`
//!    （资源缺席视为 0），收集全部 `(Transform 位置, Collider, Option<RigidBody>, Entity)`，
//!    另收集只有 `RigidBody`（无 Collider）的「仅受力积分体」；
//! 2. **积分**（半隐式欧拉，仅动体）：`v += 重力 × gravity_scale × dt`；
//!    `v ×= exp(-linear_damping × dt)`；`pos += v × dt`；
//! 3. **检测**：均匀网格 broadphase（[`crate::broadphase::BroadphaseGrid`]）
//!    先按 AABB 快照筛出候选对，再逐对 [`detect_pair`] 精检（跳过静-静对）。
//!    候选升序 + `j > i` 过滤 ⇒ 配对遍历序与朴素 O(n²) 双层循环**完全一致**
//!    （既有 bit 级确定性测试无需改动即守护等价性，见 [`step_physics`]）；
//! 4. **响应**：穿透深度沿法线分离（静态体不动，动体按动体数均分——
//!    等质量近似）；速度法向分量按 restitution 反弹（`v_n' = -e × v_n`，
//!    仅当朝向穿透，e 取双方最大）；切向分量乘 `max(0, 1 - friction)`
//!    （friction 取参与响应的动体的均值）；
//! 5. **松弛**：位置分离重复求解 [`SOLVER_ITERATIONS`] 次（速度响应只在
//!    首轮施加），减弱堆叠抖动；
//! 6. **写回**：动体的 `Transform.position` / `RigidBody.velocity`
//!    （静态体不写；两阶段处理保证迭代期间不持任何组件借用）。
//!
//! # 确定性
//!
//! 系统内部严格单线程、固定遍历序（收集序 = 组件存储序），同一输入两次
//! 模拟逐采样 bit 级一致（单测守护）。引擎侧配 `Time::advance` /
//! 固定步长驱动即可做确定性回放 / 联机同步。
//!
//! # 静态推离查询（运动学角色控制器）
//!
//! [`resolve_static_push`] / [`resolve_against_statics`]：**查询**而非系统
//! ——不积分、不写任何组件，把给定位置上的形状从全部静态体的重叠中沿
//! 法线推出，返回修正向量。消费方式完全由调用方决定（iris-game 的玩家
//! 控制器只取水平分量）：推离只沿接触法线 → 切向速度天然保留——这正是
//! 「贴墙滑行」手感的数学基础，也是游戏逻辑与物理库组合的推荐姿势
//! （kinematic 实体自管积分，物理库只回答「这里被什么挡住了」）。
//!
//! # 射线 / 线段查询（视线 · 拾取 · 高速命中）
//!
//! [`ray_sphere`] / [`ray_box`]（几何基元）→ [`ray_vs_shape`]（分发 +
//! `t ≤ max_t` 裁剪）→ [`segment_hits_shape`]（线段便捷封装，dir = 终点
//! − 起点、max_t = 长度）——高速离散体每帧位移可大于目标直径，当帧点
//! 判定会**穿模漏检**，线段扫掠判定是修法（iris-arena 的弹道命中即此）。
//! 世界级封装 [`raycast`] / [`raycast_filtered`]：遍历全部
//! `(Transform, Collider)`（静态动态都命中）取最小 t，纯查询不写组件。
//!
//! # 球扫掠查询（角色控制器地基）
//!
//! [`sphere_sweep`] / [`sphere_sweep_statics`]：半径 `r` 的球从 `origin`
//! 位移 `displacement`，问「首次碰到静态体的位移分数 t∈[0,1] 与阻挡
//! 法线」。实现为保守推进二分（复用 [`detect_pair`]，24 次收敛）——
//! 非解析解但确定性：同输入恒同输出。
//!
//! # v1 限制（诚实清单）
//!
//! - **AABB 不随实体旋转**：[`ColliderShape::Box`] 是世界轴对齐包围盒，
//!   旋转实体的碰撞盒不跟着转——物理体应使用不旋转的实体（或把旋转只用于
//!   表现子实体）；
//! - **物理体须为根实体**：`step_physics` 把 `Transform.position` 当世界
//!   坐标读写——与变换层级并用时请自行保证物理体没有父级（或父级变换恒等）；
//! - **非迭代完整求解器**：等质量冲量 + 2 次位置松弛，重堆叠场景松弛不
//!   彻底（残余穿透 / 缓慢滑移），不是完整 LCP 求解器；
//! - **粗检测为 AABB 网格快照**：均匀网格按「积分后、求解前」的 AABB
//!   一次构建，求解期位置修正（穿透量级）不重建——常规场景修正量远小于
//!   cell 尺寸、候选为保守超集；极端穿透（接近 cell 尺寸）理论上可能漏对。
//!
//! # 用法
//!
//! ```ignore
//! use iris_core::prelude::*;
//!
//! // 地面：静态碰撞体（无 RigidBody = 静态语义）
//! world.spawn((
//!     Transform::from_xyz(0.0, -1.0, 0.0),
//!     Collider::static_box(Vec3::new(30.0, 1.0, 30.0)), // 顶面 y = 0
//! ));
//! // 球：完整刚体，从 5 m 高度落下
//! world.spawn((
//!     Transform::from_xyz(0.0, 5.0, 0.0),
//!     Collider::dynamic_sphere(0.5, 0.8),
//!     RigidBody { friction: 0.4, linear_damping: 0.1, ..RigidBody::default() },
//! ));
//! ```

use glam::Vec3;

use crate::Component;
use crate::broadphase::BroadphaseGrid;
use crate::entity::Entity;
use crate::time::Time;
use crate::transform::Transform;
use crate::world::World;

/// 标准重力（米/秒²，世界 -Y 向下）。可用 [`RigidBody::gravity_scale`]
/// 缩放（0 = 失重、负值反向）。
pub const PHYSICS_GRAVITY: Vec3 = Vec3::new(0.0, -9.81, 0.0);

/// 位置松弛迭代次数：每帧对全部接触重复「检测 → 位置分离」这么多次，
/// 减弱堆叠场景的单帧残余穿透（速度响应只在首轮施加，见 [`solve_contact`]）。
pub const SOLVER_ITERATIONS: usize = 2;

/// 碰撞形状。v1 支持 Sphere 与世界轴对齐 Box（AABB 不随实体旋转，见模块文档）。
#[derive(Clone, Copy, Debug, PartialEq, Component)]
pub enum ColliderShape {
    /// 球体。
    Sphere {
        /// 半径（米）。
        radius: f32,
    },
    /// 轴对齐包围盒（AABB，**不随实体旋转**）。
    Box {
        /// 半边长（米）。
        half_extents: Vec3,
    },
}

/// 碰撞体组件：形状 + 是否静态 + 弹性。
#[derive(Clone, Copy, Debug, PartialEq, Component)]
pub struct Collider {
    pub shape: ColliderShape,
    /// 静态体（地面 / 墙 / 台面）不移动也不积分；缺席 [`RigidBody`] 时
    /// 同样按静态处理（与该标记取或）。
    pub is_static: bool,
    /// 弹性系数（0..1）：碰撞后法向速度的保留比例；接触组合取双方最大。
    pub restitution: f32,
}

impl Collider {
    /// 静态 AABB（地面 / 墙 / 台面），弹性 0。
    pub fn static_box(half_extents: Vec3) -> Self {
        Self {
            shape: ColliderShape::Box { half_extents },
            is_static: true,
            restitution: 0.0,
        }
    }

    /// 静态球，弹性 0。
    pub fn static_sphere(radius: f32) -> Self {
        Self {
            shape: ColliderShape::Sphere { radius },
            is_static: true,
            restitution: 0.0,
        }
    }

    /// 动态球（刚体参数在 [`RigidBody`] 上）。
    pub fn dynamic_sphere(radius: f32, restitution: f32) -> Self {
        Self {
            shape: ColliderShape::Sphere { radius },
            is_static: false,
            restitution,
        }
    }

    /// 动态 AABB。
    pub fn dynamic_box(half_extents: Vec3, restitution: f32) -> Self {
        Self {
            shape: ColliderShape::Box { half_extents },
            is_static: false,
            restitution,
        }
    }
}

/// 刚体组件：动体才有（静态体只有 [`Collider`]）。
#[derive(Clone, Copy, Debug, PartialEq, Component)]
pub struct RigidBody {
    /// 线速度（米/秒）。
    pub velocity: Vec3,
    /// 重力缩放：1.0 = 标准重力（[`PHYSICS_GRAVITY`]），0 = 失重，负值反向。
    pub gravity_scale: f32,
    /// 摩擦：接触时切向速度的每帧衰减系数（把切向相对速度乘
    /// `max(0, 1 - friction)`——近似库仑摩擦的廉价替代，非严格冲量模型）。
    pub friction: f32,
    /// 线性阻尼：每秒指数速度衰减（`v ×= exp(-damping × dt)`，空气阻力近似）。
    pub linear_damping: f32,
}

impl Default for RigidBody {
    fn default() -> Self {
        Self {
            velocity: Vec3::ZERO,
            gravity_scale: 1.0,
            friction: 0.0,
            linear_damping: 0.0,
        }
    }
}

/// 一次接触（纯数据）：法线 + 穿透深度。由 [`detect_pair`] 产出。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Contact {
    /// 接触法线（单位向量）：**从 a 指向 b**——分离时 b 沿 +法线移动、
    /// a 沿 -法线移动。几何退化（球心重合等）时取确定性的任意方向。
    pub normal: Vec3,
    /// 穿透深度（正数）：两形状沿法线的重叠量。
    pub penetration: f32,
}

/// 参与接触求解的单方参数（从组件解包的纯数值，不依赖 ECS，便于单测）。
#[derive(Clone, Copy, Debug)]
pub struct BodyParams {
    /// 线速度；静态体传 `None`（不参与速度求解，位置与速度修正恒为零）。
    pub velocity: Option<Vec3>,
    pub is_static: bool,
    pub restitution: f32,
    pub friction: f32,
}

impl BodyParams {
    /// 静态体参数（速度缺席、弹性摩擦为零）。
    pub const STATIC: Self = Self {
        velocity: None,
        is_static: true,
        restitution: 0.0,
        friction: 0.0,
    };

    /// 动态体参数。
    pub fn dynamic(velocity: Vec3, restitution: f32, friction: f32) -> Self {
        Self {
            velocity: Some(velocity),
            is_static: false,
            restitution,
            friction,
        }
    }
}

/// 单个接触的求解输出：双方的位置修正与速度修正（叠加到各自当前值上；
/// 静态方恒为零）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContactResponse {
    pub delta_pos_a: Vec3,
    pub delta_pos_b: Vec3,
    pub delta_vel_a: Vec3,
    pub delta_vel_b: Vec3,
}

/// 形状对碰撞检测（纯函数）：穿透时返回接触，否则 `None`。
/// 只依赖几何入参、不触碰世界——「穿透量 / 法线」可直接单测。
pub fn detect_pair(
    a_pos: Vec3,
    a_shape: &ColliderShape,
    b_pos: Vec3,
    b_shape: &ColliderShape,
) -> Option<Contact> {
    match (a_shape, b_shape) {
        (ColliderShape::Sphere { radius: ra }, ColliderShape::Sphere { radius: rb }) => {
            sphere_sphere(a_pos, *ra, b_pos, *rb)
        }
        // 球在前：sphere_box 返回「从球指向盒」的法线，恰为 a → b
        (ColliderShape::Sphere { radius: r }, ColliderShape::Box { half_extents }) => {
            sphere_box(a_pos, *r, b_pos, half_extents).map(|(normal, penetration)| Contact {
                normal,
                penetration,
            })
        }
        // 盒在前：交换到「球在前」求解，再把法线翻回 a → b
        (ColliderShape::Box { half_extents }, ColliderShape::Sphere { radius: r }) => {
            sphere_box(b_pos, *r, a_pos, half_extents).map(|(normal, penetration)| Contact {
                normal: -normal,
                penetration,
            })
        }
        (ColliderShape::Box { half_extents: a_he }, ColliderShape::Box { half_extents: b_he }) => {
            box_box(a_pos, a_he, b_pos, b_he)
        }
    }
}

/// 接触求解（纯函数）：位置分离 + 法向反弹 + 切向摩擦。
///
/// - **分离**：静态体不动，动体按动体数均分穿透深度（1 动体全吃、
///   2 动体各半——等质量近似）；
/// - **反弹**：相对速度（a 相对 b）的法向分量在「朝向穿透」（`v_n > 0`）
///   时反转为 `v_n' = -e × v_n`，e 取双方 restitution 最大；分离中
///   （`v_n ≤ 0`）不额外注入速度（这是静止接触不抖的关键）；
/// - **摩擦**：切向相对速度乘 `max(0, 1 - friction)`，friction 取
///   **参与响应的动体**的均值（静体速度不可写，其摩擦不参与）。
///
/// 「反弹量 / 摩擦衰减」据此可直接单测。
pub fn solve_contact(contact: Contact, a: BodyParams, b: BodyParams) -> ContactResponse {
    let zero = ContactResponse {
        delta_pos_a: Vec3::ZERO,
        delta_pos_b: Vec3::ZERO,
        delta_vel_a: Vec3::ZERO,
        delta_vel_b: Vec3::ZERO,
    };
    let inv_a = if a.is_static { 0.0 } else { 1.0 };
    let inv_b = if b.is_static { 0.0 } else { 1.0 };
    let inv_sum = inv_a + inv_b;
    if contact.penetration <= 0.0 || inv_sum == 0.0 {
        // 无穿透 / 静-静对（收集阶段已跳过，防御兜底）：无响应
        return zero;
    }
    let n = contact.normal;
    let share_a = inv_a / inv_sum;
    let share_b = inv_b / inv_sum;

    // 位置分离：动体按动体数均分穿透深度（法线从 a 指向 b：a 沿 -n、b 沿 +n）
    let delta_pos_a = -n * (contact.penetration * share_a);
    let delta_pos_b = n * (contact.penetration * share_b);

    // 速度响应：相对速度 = a 相对 b
    let rel = a.velocity.unwrap_or(Vec3::ZERO) - b.velocity.unwrap_or(Vec3::ZERO);
    let v_n = rel.dot(n);
    let tangent = rel - n * v_n;
    let e = a.restitution.max(b.restitution);
    let new_n = if v_n > 0.0 { -e * v_n } else { v_n };
    let friction = match (a.is_static, b.is_static) {
        (false, false) => (a.friction + b.friction) * 0.5,
        (false, true) => a.friction,
        (true, false) => b.friction,
        (true, true) => 0.0, // 不可达（静-静对已跳过），确定性兜底
    };
    let tangent_scale = (1.0 - friction).max(0.0);
    let new_rel = n * new_n + tangent * tangent_scale;
    let delta_rel = new_rel - rel;
    let delta_vel_a = delta_rel * share_a;
    let delta_vel_b = -delta_rel * share_b;

    ContactResponse {
        delta_pos_a,
        delta_pos_b,
        delta_vel_a,
        delta_vel_b,
    }
}

/// 静态推离查询（纯函数）：把 `pos` 处的 `shape` 从全部静态体的重叠中
/// 推出，返回累计修正向量。
///
/// 对每个静态体调 [`detect_pair`] 求（法线, 穿透），**顺序累加**推离
/// ——逐个用推离后的位置重测（Gauss-Seidel 风格）；角点处同一静态体
/// 的重复推离可接受（v1 简化：顺序单遍而非联合求解，分离性优先于精确性）。
/// 无穿透（含 `statics` 为空）返回零向量。
///
/// 推离只沿接触法线：调用方保留自己的切向速度即可实现「贴墙滑行」。
/// 世界封装见 [`resolve_against_statics`]。
pub fn resolve_static_push(
    mut pos: Vec3,
    shape: &ColliderShape,
    statics: &[(Vec3, ColliderShape)],
) -> Vec3 {
    let mut push = Vec3::ZERO;
    for (static_pos, static_shape) in statics {
        // 法线从查询方指向静态体：静态体不动，查询方沿 -法线退出穿透量
        let Some(contact) = detect_pair(pos, shape, *static_pos, static_shape) else {
            continue;
        };
        let correction = -contact.normal * contact.penetration;
        pos += correction; // 后续静态体用修正后的位置重测
        push += correction;
    }
    push
}

/// 球-球检测：法线从 a 指向 b。
fn sphere_sphere(a_pos: Vec3, a_radius: f32, b_pos: Vec3, b_radius: f32) -> Option<Contact> {
    let d = b_pos - a_pos;
    let r_sum = a_radius + b_radius;
    let dist_sq = d.length_squared();
    if dist_sq >= r_sum * r_sum {
        return None;
    }
    if dist_sq <= 1e-12 {
        // 球心重合：方向退化，取 +Y（确定性任意方向）
        return Some(Contact {
            normal: Vec3::Y,
            penetration: r_sum,
        });
    }
    let dist = dist_sq.sqrt();
    Some(Contact {
        normal: d / dist,
        penetration: r_sum - dist,
    })
}

/// 球-AABB 检测：返回（从球心指向盒的法线，穿透深度）。
fn sphere_box(
    sphere_pos: Vec3,
    radius: f32,
    box_pos: Vec3,
    half_extents: &Vec3,
) -> Option<(Vec3, f32)> {
    let local = sphere_pos - box_pos; // 球心在盒局部坐标（盒轴对齐，无需旋转变换）
    let closest = local.clamp(-*half_extents, *half_extents); // 盒面（或盒内）最近点
    let delta = closest - local; // 球心 → 最近点
    let dist_sq = delta.length_squared();
    if dist_sq > radius * radius {
        return None;
    }
    if dist_sq > 1e-12 {
        // 球心在盒外：法线 = 球心 → 最近点（即从球指向盒）
        let dist = dist_sq.sqrt();
        Some((delta / dist, radius - dist))
    } else {
        // 球心在盒内：沿「最浅穿透轴」从最近的面推出。法线仍从球指向盒
        // （分离时球沿 -法线移动 = 朝最近的面退出）。并列时按 x < y < z
        // 优先，保证确定性。
        let dx = half_extents.x - local.x.abs();
        let dy = half_extents.y - local.y.abs();
        let dz = half_extents.z - local.z.abs();
        let (depth, axis, positive) = if dx <= dy && dx <= dz {
            (dx, 0, local.x >= 0.0)
        } else if dy <= dz {
            (dy, 1, local.y >= 0.0)
        } else {
            (dz, 2, local.z >= 0.0)
        };
        let away = if positive { -1.0 } else { 1.0 };
        let normal = match axis {
            0 => Vec3::new(away, 0.0, 0.0),
            1 => Vec3::new(0.0, away, 0.0),
            _ => Vec3::new(0.0, 0.0, away),
        };
        Some((normal, depth + radius))
    }
}

/// AABB-AABB 检测：法线取最小穿透轴、从 a 指向 b。
fn box_box(a_pos: Vec3, a_half: &Vec3, b_pos: Vec3, b_half: &Vec3) -> Option<Contact> {
    let d = b_pos - a_pos;
    let overlap = Vec3::new(
        a_half.x + b_half.x - d.x.abs(),
        a_half.y + b_half.y - d.y.abs(),
        a_half.z + b_half.z - d.z.abs(),
    );
    if overlap.x <= 0.0 || overlap.y <= 0.0 || overlap.z <= 0.0 {
        return None;
    }
    // 最小穿透轴（并列时 x < y < z 优先，保证确定性）
    let (normal, penetration) = if overlap.x <= overlap.y && overlap.x <= overlap.z {
        (Vec3::new(signum(d.x), 0.0, 0.0), overlap.x)
    } else if overlap.y <= overlap.z {
        (Vec3::new(0.0, signum(d.y), 0.0), overlap.y)
    } else {
        (Vec3::new(0.0, 0.0, signum(d.z)), overlap.z)
    };
    Some(Contact {
        normal,
        penetration,
    })
}

/// 零取 +1 的符号函数（中心重合时方向退化，确定性取正）。
fn signum(x: f32) -> f32 {
    if x >= 0.0 { 1.0 } else { -1.0 }
}

/// 碰撞体的世界 AABB（球 → 中心 ± 半径；盒 → 中心 ± 半边长——盒本就
/// 轴对齐，无需旋转）。供 broadphase 网格的插入与查询使用。
fn collider_aabb(pos: Vec3, shape: &ColliderShape) -> (Vec3, Vec3) {
    match *shape {
        ColliderShape::Sphere { radius } => {
            let r = Vec3::splat(radius);
            (pos - r, pos + r)
        }
        ColliderShape::Box { half_extents } => (pos - half_extents, pos + half_extents),
    }
}

/// AABB 的最大轴向跨度（供 cell 尺寸估计：贴着最大动体的包围盒粒度）。
fn aabb_span(aabb: &(Vec3, Vec3)) -> f32 {
    (aabb.1 - aabb.0).max_element()
}

/// 物理主系统：每帧推进一次（积分 → 检测 → 响应 → 迭代松弛 → 写回，
/// 见模块文档的管线六步）。
///
/// 只读世界签名（`&World`）+ 两阶段处理（先全量收集再统一写回，
/// `update_animations` 同款模式），组件写访问走世界内部的尝试型锁，
/// 因此可注册为**并行系统**（访问集：写 Transform / Collider / RigidBody，
/// 读 Time）。`Time` 资源缺席时 dt = 0：不积分，但仍会分离初始摆放重叠。
pub fn step_physics(world: &World) {
    let dt = world
        .try_resource::<Time>()
        .map(|t| t.delta_secs())
        .unwrap_or(0.0);

    // ---- 阶段一（只读收集）：全部碰撞体实体 + 可选刚体 ----
    let mut collected: Vec<(Entity, Collider, Option<RigidBody>)> = Vec::new();
    let mut positions: Vec<Vec3> = Vec::new();
    world.query::<(Entity, &Transform, &Collider, Option<&RigidBody>)>(
        |(entity, transform, collider, rigid)| {
            collected.push((entity, *collider, rigid.copied()));
            positions.push(transform.position);
        },
    );
    let n = collected.len();

    // 仅受力积分体：只有 RigidBody（无 Collider）的实体——不参与碰撞，
    // 只随积分与写回阶段一起推进（模块文档的「缺席语义」第二条）。
    let mut flyers: Vec<(Entity, RigidBody)> = Vec::new();
    world.query::<(Entity, &RigidBody)>(|(entity, rigid)| {
        // 递归读另一组件存储是合法的（共享读可并存，渲染收集闭包同款模式）
        if world.get::<Collider>(entity).is_none() {
            flyers.push((entity, *rigid));
        }
    });
    if n == 0 && flyers.is_empty() {
        return;
    }

    // 有效静态：Collider::is_static 或缺席 RigidBody（取或，见模块文档）
    let is_static: Vec<bool> = collected
        .iter()
        .map(|(_, collider, rigid)| collider.is_static || rigid.is_none())
        .collect();

    // ---- 积分（半隐式欧拉）----
    let mut velocities: Vec<Option<Vec3>> = collected
        .iter()
        .map(|(_, _, rigid)| rigid.map(|b| b.velocity))
        .collect();
    for index in 0..n {
        if is_static[index] {
            continue;
        }
        let rigid = collected[index].2.expect("非静态实体必有 RigidBody");
        let mut velocity = rigid.velocity;
        velocity += PHYSICS_GRAVITY * rigid.gravity_scale * dt;
        velocity *= (-rigid.linear_damping * dt).exp();
        positions[index] += velocity * dt;
        velocities[index] = Some(velocity);
    }
    // 仅受力积分体：同样的积分式，无接触可解
    let mut flyer_states: Vec<(Entity, Vec3, Vec3)> = Vec::new();
    for &(entity, rigid) in &flyers {
        let Some(position) = world.get::<Transform>(entity).map(|t| t.position) else {
            continue;
        };
        let mut velocity = rigid.velocity;
        velocity += PHYSICS_GRAVITY * rigid.gravity_scale * dt;
        velocity *= (-rigid.linear_damping * dt).exp();
        flyer_states.push((entity, position + velocity * dt, velocity));
    }

    // 求解参数快照（速度随响应即时同步，见松弛循环内的回写）
    let mut params: Vec<BodyParams> = Vec::with_capacity(n);
    for index in 0..n {
        let (_, collider, rigid) = &collected[index];
        params.push(BodyParams {
            velocity: if is_static[index] {
                None
            } else {
                velocities[index]
            },
            is_static: is_static[index],
            restitution: collider.restitution,
            friction: rigid.map(|b| b.friction).unwrap_or(0.0),
        });
    }

    // ---- 检测 + 响应 + 松弛（均匀网格 broadphase 筛候选对）----
    // 粗筛：按「积分后、求解前」的位置算每个碰撞体的世界 AABB（球 →
    // 中心 ± 半径；盒 → 中心 ± 半边长），建一次性网格。cell 尺寸取全部
    // **非静态**体的最大轴向跨度，钳制 [1.0, 50.0]——静态大地面（跨度
    // 100 m）会把 cell 撑到场景尺度、网格退化为全配对，故不参与定尺寸
    //（地面照样插入网格、照样被查询，只是不决定粒度）；无动体时回退
    // 全体最大跨度（此时只剩静-静对，配对本就全跳过，尺寸不影响结果）。
    let aabbs: Vec<(Vec3, Vec3)> = (0..n)
        .map(|index| collider_aabb(positions[index], &collected[index].1.shape))
        .collect();
    let mut max_span = 0.0f32;
    for index in 0..n {
        if !is_static[index] {
            max_span = max_span.max(aabb_span(&aabbs[index]));
        }
    }
    if max_span <= 0.0 {
        for aabb in &aabbs {
            max_span = max_span.max(aabb_span(aabb));
        }
    }
    let cell_size = max_span.clamp(1.0, 50.0);
    let mut grid = BroadphaseGrid::new(cell_size);
    for (index, (min, max)) in aabbs.iter().copied().enumerate() {
        grid.insert_aabb(index, min, max);
    }
    // 每 i 的候选一次算好（AABB 快照固定，候选与松弛轮次无关）。候选
    // 升序 + `j > i` 过滤 ⇒ 配对遍历序与原 O(n²) 双层循环完全一致——
    // 既有 bit 级确定性测试（simulation_is_deterministic_across_runs）
    // 无需改动即守护这一等价性。快照后的位置修正为穿透量级（远小于
    // cell 尺寸），候选保持保守超集（诚实边界见模块文档「v1 限制」）。
    let candidates: Vec<Vec<usize>> = (0..n)
        .map(|index| grid.candidates(aabbs[index].0, aabbs[index].1))
        .collect();

    // 静-静对跳过（互不动，永不穿透）。位置分离即时生效（Gauss-Seidel
    // 风格），共 SOLVER_ITERATIONS 轮；速度响应只在首轮施加（重复施加
    // 会把摩擦/反弹多次结算，语义错误）。
    for iteration in 0..SOLVER_ITERATIONS {
        for i in 0..n {
            for &j in &candidates[i] {
                if j <= i {
                    continue; // 只保留下三角（j > i），与原 (i+1)..n 内层一致
                }
                if is_static[i] && is_static[j] {
                    continue;
                }
                let Some(contact) = detect_pair(
                    positions[i],
                    &collected[i].1.shape,
                    positions[j],
                    &collected[j].1.shape,
                ) else {
                    continue;
                };
                let response = solve_contact(contact, params[i], params[j]);
                positions[i] += response.delta_pos_a;
                positions[j] += response.delta_pos_b;
                if iteration == 0 {
                    if let Some(v) = velocities[i].as_mut() {
                        *v += response.delta_vel_a;
                        params[i].velocity = Some(*v);
                    }
                    if let Some(v) = velocities[j].as_mut() {
                        *v += response.delta_vel_b;
                        params[j].velocity = Some(*v);
                    }
                }
            }
        }
    }

    // ---- 阶段二（写回）：仅动体写 Transform / RigidBody，静态体不触碰 ----
    for index in 0..n {
        if is_static[index] {
            continue;
        }
        let (entity, _, _) = collected[index];
        if let Some(mut transform) = world.get_mut::<Transform>(entity) {
            transform.position = positions[index];
        }
        if let (Some(velocity), Some(mut rigid)) =
            (velocities[index], world.get_mut::<RigidBody>(entity))
        {
            rigid.velocity = velocity;
        }
    }

    // 仅受力积分体写回（不参与碰撞，只有位置与速度）
    for (entity, position, velocity) in flyer_states {
        if let Some(mut transform) = world.get_mut::<Transform>(entity) {
            transform.position = position;
        }
        if let Some(mut rigid) = world.get_mut::<RigidBody>(entity) {
            rigid.velocity = velocity;
        }
    }
}

/// 世界封装的静态推离查询：把 `pos` 处的 `shape` 从世界中全部静态体的
/// 重叠中推出，返回累计修正向量（[`resolve_static_push`] 的收集 + 调用）。
///
/// 「静态体」判定与 [`step_physics`] 一致：有 [`Collider`] 且
/// `is_static` **或无** [`RigidBody`]（组件缺席语义：只有 Collider =
/// 静态）。两阶段中的收集阶段照 `step_physics` 同款模式（只读查询一次
/// 收齐），随后不写任何组件——纯查询，可与任何系统并存。
pub fn resolve_against_statics(world: &World, pos: Vec3, shape: &ColliderShape) -> Vec3 {
    let mut statics: Vec<(Vec3, ColliderShape)> = Vec::new();
    world.query::<(&Transform, &Collider, Option<&RigidBody>)>(|(transform, collider, rigid)| {
        if collider.is_static || rigid.is_none() {
            statics.push((transform.position, collider.shape));
        }
    });
    resolve_static_push(pos, shape, &statics)
}

// ---------------------------------------------------------------------------
// 射线 / 线段查询（视线 · 拾取 · 高速命中，见模块文档）
// ---------------------------------------------------------------------------

/// 射线命中结果：命中参数 + 命中点 + 表面法线（[`raycast`] 系列的产出）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    /// 沿射线参数：命中点 = `origin + dir × t`（`0 ≤ t ≤ max_t`）。
    pub t: f32,
    /// 命中点世界坐标。
    pub point: Vec3,
    /// 单位表面**外**法线，指向射线来侧（`dot(normal, dir) ≤ 0`）。
    pub normal: Vec3,
}

/// 零长度方向的判定容差：`length²` 低于此值按「无方向」处理（返回 `None`）。
const DIR_EPSILON_SQ: f32 = 1e-12;

/// 线段退化为点的判定容差：`from` 与 `to` 距离低于此值时
/// [`segment_hits_shape`] 返回 `None`。
const SEGMENT_EPSILON: f32 = 1e-9;

/// 射线-球求交（纯函数，几何法解二次方程取最小非负根）。
///
/// `dir` 须为**单位方向**（调用方约定；零长度方向返回 `None`）。返回
/// `(t, 表面外法线)`；`t = 0` 表示起点已在球内 / 球面上。
///
/// 退化情形的确定性选择：
/// - 起点在球内 → `t = 0`、法线 = `(origin − center).normalize_or_zero()`
///   （指向来侧即离开球心的方向）；起点与球心**重合**时该向量为零，
///   取 `-dir`（单位入参保证非零）。
/// - 球整条在射线背后（两根皆负）→ `None`。
pub fn ray_sphere(origin: Vec3, dir: Vec3, center: Vec3, radius: f32) -> Option<(f32, Vec3)> {
    if dir.length_squared() <= DIR_EPSILON_SQ {
        return None;
    }
    let oc = origin - center;
    // |origin + dir·t − center|² = r²  →  t² + 2b·t + c = 0（dir 单位长）
    let b = oc.dot(dir); // 半系数：两根 = −b ± √(b²−c)
    let c = oc.length_squared() - radius * radius;
    if c < 0.0 {
        // 起点在球内：t = 0；法线指向来侧（离开球心）。重合退化为 −dir
        // （确定性任意方向，见文档）。
        let normal = oc.normalize_or_zero();
        return Some((0.0, if normal == Vec3::ZERO { -dir } else { normal }));
    }
    let discriminant = b * b - c;
    if discriminant < 0.0 {
        return None; // 射线与球不相交
    }
    let sqrt_d = discriminant.sqrt();
    let t = -b - sqrt_d; // 最小根（外表面入射点）；负 = 起点在球内或球在背后
    if t < 0.0 {
        return None; // 球内已在上面处理，这里必为「球在射线背后」
    }
    let normal = (origin + dir * t - center).normalize_or_zero();
    Some((t, normal))
}

/// 射线-AABB 求交（纯函数，slab 法）。
///
/// `dir` 须为**单位方向**（零长度方向返回 `None`）。分母为 0 的轴按
/// 「起点在界内则该轴不裁剪、在界外则直接未命中」处理。返回
/// `(t, 入射面外法线)`；`t = 0` 表示起点已在盒内 / 盒面上。
///
/// 退化情形的确定性选择（与 [`sphere_box`] 的盒内逻辑同约定）：
/// - 起点在盒内 → `t = 0`、法线取「最浅出面」（到各面距离最小者）的
///   **外**法线，并列时按 x < y < z 优先；
/// - 入射面轴并列（斜射角点）→ 取先设置 `tmin` 的轴，即 x < y < z 优先。
pub fn ray_box(origin: Vec3, dir: Vec3, center: Vec3, half_extents: &Vec3) -> Option<(f32, Vec3)> {
    if dir.length_squared() <= DIR_EPSILON_SQ {
        return None;
    }
    let local = origin - center;
    let mut t_min = f32::MIN;
    let mut t_max = f32::MAX;
    let mut entry_axis = 2; // 产生 t_min 的轴（入射面法线的轴向）
    for axis in 0..3 {
        let (d, half, offset) = match axis {
            0 => (dir.x, half_extents.x, local.x),
            1 => (dir.y, half_extents.y, local.y),
            _ => (dir.z, half_extents.z, local.z),
        };
        if d.abs() <= DIR_EPSILON_SQ {
            // 该轴与 slab 平行：起点在界内 → 不裁剪；界外 → 未命中
            if offset.abs() > half {
                return None;
            }
            continue;
        }
        let t1 = (-half - offset) / d;
        let t2 = (half - offset) / d;
        let (near, far) = if t1 <= t2 { (t1, t2) } else { (t2, t1) };
        // 严格大于才更新入射轴：并列（角点斜射）保留先者（x < y < z 优先）
        if near > t_min {
            t_min = near;
            entry_axis = axis;
        }
        t_max = t_max.min(far);
        if t_min > t_max {
            return None;
        }
    }
    if t_max < 0.0 {
        return None; // 盒整条在射线背后
    }
    if t_min < 0.0 {
        // 起点在盒内：t = 0，法线取最浅出面的外法线（x < y < z 并列优先，
        // 与 sphere_box 盒内分支同约定；外法线指向偏移量所在侧）。
        let dx = half_extents.x - local.x.abs();
        let dy = half_extents.y - local.y.abs();
        let dz = half_extents.z - local.z.abs();
        let normal = if dx <= dy && dx <= dz {
            Vec3::new(signum(local.x), 0.0, 0.0)
        } else if dy <= dz {
            Vec3::new(0.0, signum(local.y), 0.0)
        } else {
            Vec3::new(0.0, 0.0, signum(local.z))
        };
        return Some((0.0, normal));
    }
    // 入射面外法线：与该轴方向分量相反（dot(normal, dir) ≤ 0 指向来侧）
    let sign = -signum(match entry_axis {
        0 => dir.x,
        1 => dir.y,
        _ => dir.z,
    });
    let normal = match entry_axis {
        0 => Vec3::new(sign, 0.0, 0.0),
        1 => Vec3::new(0.0, sign, 0.0),
        _ => Vec3::new(0.0, 0.0, sign),
    };
    Some((t_min, normal))
}

/// 射线-形状求交（[`ColliderShape`] 分发）＋ `t ≤ max_t` 裁剪。
///
/// `dir` 须为**单位方向**；`t > max_t` 视为未命中（命中点超出查询范围）。
/// 命中参数 / 命中点 / 法线打包为 [`RayHit`]。
pub fn ray_vs_shape(
    origin: Vec3,
    dir: Vec3,
    max_t: f32,
    pos: Vec3,
    shape: &ColliderShape,
) -> Option<RayHit> {
    let (t, normal) = match shape {
        ColliderShape::Sphere { radius } => ray_sphere(origin, dir, pos, *radius)?,
        ColliderShape::Box { half_extents } => ray_box(origin, dir, pos, half_extents)?,
    };
    if t > max_t {
        return None;
    }
    Some(RayHit {
        t,
        point: origin + dir * t,
        normal,
    })
}

/// 线段-形状求交：`from → to` 的扫掠判定（高速离散体防穿模）。
///
/// 等价于 `ray_vs_shape(from, dir = (to − from).normalize(), max_t = 距离)`
/// ——每帧位移可大于目标直径的弹道 / 高速刚体用它替代当帧点判定。
/// `from` 与 `to` 容差内重合（零长度线段）返回 `None`。
pub fn segment_hits_shape(
    from: Vec3,
    to: Vec3,
    pos: Vec3,
    shape: &ColliderShape,
) -> Option<RayHit> {
    let delta = to - from;
    let length = delta.length();
    if length <= SEGMENT_EPSILON {
        return None;
    }
    ray_vs_shape(from, delta / length, length, pos, shape)
}

/// 世界级射线查询：沿 `origin → origin + dir × max_t` 找**最近**命中的
/// 碰撞体（静态与动态都命中），返回 `(实体, RayHit)`；未命中 / 零长度
/// 方向返回 `None`。
///
/// [`raycast_filtered`] 的「无过滤」便捷封装。纯查询：不写任何组件。
pub fn raycast(world: &World, origin: Vec3, dir: Vec3, max_t: f32) -> Option<(Entity, RayHit)> {
    raycast_filtered(world, origin, dir, max_t, |_| true)
}

/// 世界级射线查询（带实体过滤）：`filter` 返回 `false` 的实体不参与命中。
///
/// `dir` 须为**单位方向**；遍历全部 `(Transform, Collider)` 取最小 `t`，
/// `t` 相等取**先遍历者**（组件存储序——确定性，与 [`step_physics`] 的
/// 收集序同源）。纯查询：不写任何组件，可与任何系统并存。
///
/// **闭包不得访问世界**（如 `world.get`）：迭代期间查询持有组件存储的
/// 读借用，`filter` 只应基于 `Entity` 做纯判定（如排除发射者、按组件
/// 标签预存的位集筛选）。
pub fn raycast_filtered(
    world: &World,
    origin: Vec3,
    dir: Vec3,
    max_t: f32,
    filter: impl Fn(Entity) -> bool,
) -> Option<(Entity, RayHit)> {
    if dir.length_squared() <= DIR_EPSILON_SQ {
        return None;
    }
    let mut best: Option<(Entity, RayHit)> = None;
    world.query::<(Entity, &Transform, &Collider)>(|(entity, transform, collider)| {
        if !filter(entity) {
            return;
        }
        let Some(hit) = ray_vs_shape(origin, dir, max_t, transform.position, &collider.shape)
        else {
            return;
        };
        // 严格小于才替换：t 并列保留先遍历者（存储序，确定性）
        if best.is_none_or(|(_, current)| hit.t < current.t) {
            best = Some((entity, hit));
        }
    });
    best
}

// ---------------------------------------------------------------------------
// 球扫掠查询（角色控制器地基，见模块文档）
// ---------------------------------------------------------------------------

/// 球扫掠二分迭代次数：区间宽度收敛到 2⁻²⁴ ≈ 6e-8（对位移分数而言
/// 已远低于任何游戏可感知精度）。
const SWEEP_BISECTION_STEPS: usize = 24;

/// 球扫掠收缩量：命中分数从「无重叠侧」边界向回收这么多，给控制器
/// 留一层防抖皮肤。
const SWEEP_SKIN: f32 = 1e-4;

/// 球扫掠（纯函数）：半径 `radius` 的球从 `origin` 位移 `displacement`，
/// 对一组静态体问「首次碰撞」。返回 `(位移分数 t ∈ [0,1], 阻挡法线)`——
/// 法线为**从静态体指向扫掠球**的单位向量（消费方沿法线切掉位移的法向
/// 分量即得滑动）。未命中返回 `None`。
///
/// 实现为**保守推进二分**（复用 [`detect_pair`]，非解析解但确定性）：
/// - 终点（t = 1）无任何重叠 → `None`；
/// - 起点（t = 0）已重叠 → `(0.0, 阻挡法线)`——取 `statics` 迭代序中
///   **首个**检出接触的静态体（确定性）；
/// - 否则以「lo 无重叠、hi 重叠」为不变式二分 24 次收敛到首次接触
///   边界，命中分数取**无重叠侧**边界 `lo` 再减 [`SWEEP_SKIN`] 收缩；
///   法线 = `−detect_pair(球@t, 静态).normal`（接触法线从球指向静态，
///   翻转即阻挡方向），静态体取迭代序首个；浮点噪声致收缩点测不出
///   接触时回退重叠侧边界 `hi`（两者相差 ≤ 2⁻²⁴）。
///
/// **检出范围（诚实边界）**：只检出「终点重叠」括起的首次接触——
/// 起点与终点都自由的**完全贯穿**（薄障碍被整段跳过）返回 `None`。
/// 典型用法（角色控制器单帧位移被墙挡住，终点即嵌入处）恰为括弧
/// 情形；位移可能贯穿薄障碍的调用方请分步扫掠。
pub fn sphere_sweep(
    origin: Vec3,
    displacement: Vec3,
    radius: f32,
    statics: &[(Vec3, ColliderShape)],
) -> Option<(f32, Vec3)> {
    let shape = ColliderShape::Sphere { radius };
    // 指定位置上首个检出接触的静态体（迭代序，确定性）→ 阻挡法线
    let blocker_at = |pos: Vec3| -> Option<Vec3> {
        statics.iter().find_map(|(static_pos, static_shape)| {
            detect_pair(pos, &shape, *static_pos, static_shape).map(|c| -c.normal)
        })
    };
    // 终点必须重叠（完全贯穿的诚实边界见函数文档）——`?` 在此只做
    // 存在性检查，终点阻挡法线不消费（起点 / 二分边界处各自重测）
    blocker_at(origin + displacement)?;
    if let Some(normal) = blocker_at(origin) {
        return Some((0.0, normal)); // 起点已重叠
    }
    // 二分不变式：lo 无重叠、hi 重叠，首次接触边界夹在中间
    let mut lo = 0.0f32;
    let mut hi = 1.0f32;
    for _ in 0..SWEEP_BISECTION_STEPS {
        let mid = 0.5 * (lo + hi);
        if blocker_at(origin + displacement * mid).is_some() {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let t = (lo - SWEEP_SKIN).max(0.0);
    let normal =
        blocker_at(origin + displacement * t).or_else(|| blocker_at(origin + displacement * hi))?;
    Some((t, normal))
}

/// 世界封装的球扫掠查询：从 `from` 出发位移 `displacement` 的半径
/// `radius` 球，问世界中全部静态体的首次碰撞（收集语义与
/// [`resolve_against_statics`] 一致：`is_static` 或仅 Collider 无
/// RigidBody 的缺席语义）。纯查询：不写任何组件。
pub fn sphere_sweep_statics(
    world: &World,
    from: Vec3,
    displacement: Vec3,
    radius: f32,
) -> Option<(f32, Vec3)> {
    let mut statics: Vec<(Vec3, ColliderShape)> = Vec::new();
    world.query::<(&Transform, &Collider, Option<&RigidBody>)>(|(transform, collider, rigid)| {
        if collider.is_static || rigid.is_none() {
            statics.push((transform.position, collider.shape));
        }
    });
    sphere_sweep(from, displacement, radius, &statics)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 常规测试步长 60Hz（解析解对照测试专用，其余测试按需用更细步长）。
    const DT: f32 = 1.0 / 60.0;

    // ---- 纯函数测试：几何 / 求解不依赖世界 ----

    /// 球-球：穿透法线与分离量精确（沿法线精确恢复到恰好相切）。
    #[test]
    fn sphere_sphere_penetration_normal_and_separation() {
        // a 在原点 r=1，b 在 (1.5, 0, 0) r=1：中心距 1.5 < 半径和 2
        let a = ColliderShape::Sphere { radius: 1.0 };
        let b = ColliderShape::Sphere { radius: 1.0 };
        let contact =
            detect_pair(Vec3::ZERO, &a, Vec3::new(1.5, 0.0, 0.0), &b).expect("两球重叠应检出接触");
        assert_eq!(contact.normal, Vec3::X, "法线应从 a 指向 b（+X）");
        assert_eq!(contact.penetration, 0.5, "穿透 = 半径和 - 中心距");

        // 静态 b + 朝向 b 运动（v=+2, e=0.5）的动体 a：a 全额分离并反弹
        let response = solve_contact(
            contact,
            BodyParams::dynamic(Vec3::new(2.0, 0.0, 0.0), 0.5, 0.0),
            BodyParams::STATIC,
        );
        assert_eq!(
            response.delta_pos_a,
            Vec3::new(-0.5, 0.0, 0.0),
            "动体沿 -法线全额退出"
        );
        assert_eq!(response.delta_pos_b, Vec3::ZERO, "静态体不移动");
        assert_eq!(
            response.delta_vel_a,
            Vec3::new(-3.0, 0.0, 0.0),
            "v_n' = -e·v_n → Δv = -(1+e)·2"
        );
        assert_eq!(response.delta_vel_b, Vec3::ZERO);

        // 分离状态的球对：无接触
        assert!(detect_pair(Vec3::ZERO, &a, Vec3::new(2.1, 0.0, 0.0), &b).is_none());
        // 恰好相切：不算穿透
        assert!(detect_pair(Vec3::ZERO, &a, Vec3::new(2.0, 0.0, 0.0), &b).is_none());

        // 重合退化：法线取确定性 +Y，穿透 = 半径和
        let coincident = detect_pair(Vec3::ZERO, &a, Vec3::ZERO, &b).expect("重合应检出接触");
        assert_eq!(coincident.normal, Vec3::Y);
        assert_eq!(coincident.penetration, 2.0);
    }

    /// 球-AABB：球心正对盒顶面（法线 -Y、穿透精确）。
    #[test]
    fn sphere_box_top_face_contact() {
        // 盒半边长 (1, 0.5, 1)，顶面 y = 0.5；球 r=0.5 悬在 (0, 0.8, 0)
        let sphere = ColliderShape::Sphere { radius: 0.5 };
        let box_ = ColliderShape::Box {
            half_extents: Vec3::new(1.0, 0.5, 1.0),
        };
        let contact = detect_pair(Vec3::new(0.0, 0.8, 0.0), &sphere, Vec3::ZERO, &box_)
            .expect("球底 0.3 < 盒顶 0.5，应接触");
        assert_eq!(
            contact.normal,
            Vec3::new(0.0, -1.0, 0.0),
            "法线从球指向盒（-Y）"
        );
        assert!((contact.penetration - 0.2).abs() < 1e-6, "穿透 = 0.5 - 0.3");

        // 反转参数序（盒在前）：法线应翻回「从 a 指向 b」（+Y），穿透不变
        let flipped = detect_pair(Vec3::ZERO, &box_, Vec3::new(0.0, 0.8, 0.0), &sphere)
            .expect("参数序无关，接触仍存在");
        assert_eq!(flipped.normal, Vec3::new(0.0, 1.0, 0.0));
        assert_eq!(flipped.penetration, contact.penetration);

        // 分离：球底在盒顶之上
        assert!(
            detect_pair(Vec3::new(0.0, 1.01, 0.0), &sphere, Vec3::ZERO, &box_).is_none(),
            "球底 0.51 > 盒顶 0.5 不应接触"
        );
    }

    /// 球-AABB：侧向接触与角点接触（45° 法线）。
    #[test]
    fn sphere_box_side_and_corner_contact() {
        let sphere = ColliderShape::Sphere { radius: 0.5 };
        let box_ = ColliderShape::Box {
            half_extents: Vec3::new(1.0, 0.5, 1.0),
        };

        // 侧向：球心 (1.35, 0, 0)，最近点 (1, 0, 0)，间距 0.35
        let side =
            detect_pair(Vec3::new(1.35, 0.0, 0.0), &sphere, Vec3::ZERO, &box_).expect("侧向应接触");
        assert_eq!(side.normal, Vec3::new(-1.0, 0.0, 0.0));
        assert!((side.penetration - 0.15).abs() < 1e-6);

        // 角点：球心 (1.2, 0.7, 0)，最近点 (1, 0.5, 0)，间距 √(0.2² + 0.2²)
        let corner =
            detect_pair(Vec3::new(1.2, 0.7, 0.0), &sphere, Vec3::ZERO, &box_).expect("角点应接触");
        let want = Vec3::new(
            -std::f32::consts::FRAC_1_SQRT_2,
            -std::f32::consts::FRAC_1_SQRT_2,
            0.0,
        );
        assert!(
            (corner.normal - want).length() < 1e-5,
            "角点法线应为 45°（-X-Y），实际 {:?}",
            corner.normal
        );
        let dist = 0.2f32.hypot(0.2);
        assert!((corner.penetration - (0.5 - dist)).abs() < 1e-5);
    }

    /// 球心在盒内：沿最浅穿透轴从最近的面退出。
    #[test]
    fn sphere_center_inside_box_exits_nearest_face() {
        let sphere = ColliderShape::Sphere { radius: 0.5 };
        let box_ = ColliderShape::Box {
            half_extents: Vec3::new(1.0, 0.5, 1.0),
        };
        // 球心 (0, 0.3, 0)：到顶面 0.2、底面 0.8、侧面 1.0 → 顶面最浅
        let contact =
            detect_pair(Vec3::new(0.0, 0.3, 0.0), &sphere, Vec3::ZERO, &box_).expect("盒内应接触");
        assert_eq!(
            contact.normal,
            Vec3::new(0.0, -1.0, 0.0),
            "法线从球指向盒（顶面退出）"
        );
        assert!((contact.penetration - 0.7).abs() < 1e-6, "穿透 = 0.2 + 0.5");

        // 偏向 -X：最近面变为 -X 侧壁
        let side =
            detect_pair(Vec3::new(-0.9, 0.0, 0.0), &sphere, Vec3::ZERO, &box_).expect("盒内应接触");
        assert_eq!(
            side.normal,
            Vec3::new(1.0, 0.0, 0.0),
            "法线从球指向盒（-X 面退出）"
        );
        assert!((side.penetration - 0.6).abs() < 1e-6, "穿透 = 0.1 + 0.5");
    }

    /// AABB-AABB：最小穿透轴法线与分离；完全分离时无接触。
    #[test]
    fn box_box_min_axis_normal_and_separation() {
        let unit = ColliderShape::Box {
            half_extents: Vec3::splat(1.0),
        };
        // x 重叠 0.3，y/z 各重叠 2.0 → 法线沿 x
        let contact = detect_pair(Vec3::ZERO, &unit, Vec3::new(1.7, 0.0, 0.0), &unit)
            .expect("重叠应检出接触");
        assert_eq!(contact.normal, Vec3::X, "法线从 a 指向 b");
        assert!((contact.penetration - 0.3).abs() < 1e-6);

        // b 在 a 下方：y 最浅 → 法线 -Y
        let vertical = detect_pair(Vec3::ZERO, &unit, Vec3::new(0.0, -1.8, 0.0), &unit)
            .expect("重叠应检出接触");
        assert_eq!(vertical.normal, Vec3::new(0.0, -1.0, 0.0));
        assert!((vertical.penetration - 0.2).abs() < 1e-6);

        // 完全分离：无接触
        assert!(detect_pair(Vec3::ZERO, &unit, Vec3::new(2.01, 0.0, 0.0), &unit).is_none());
    }

    /// restitution 组合取双方最大。
    #[test]
    fn restitution_combines_by_max() {
        let contact = Contact {
            normal: Vec3::X,
            penetration: 0.5,
        };
        let response = solve_contact(
            contact,
            BodyParams::dynamic(Vec3::new(2.0, 0.0, 0.0), 0.2, 0.0),
            BodyParams::dynamic(Vec3::ZERO, 0.7, 0.0),
        );
        // e = max(0.2, 0.7) = 0.7：Δv = -(1 + 0.7) × 2 = -3.4（双方各半）
        assert!((response.delta_vel_a.x - -3.4 * 0.5).abs() < 1e-6);
        assert!((response.delta_vel_b.x - 3.4 * 0.5).abs() < 1e-6);
        assert_eq!(response.delta_vel_a.y, 0.0);
        // 分离中（v_n ≤ 0）不反弹：速度不变
        let separating = solve_contact(
            contact,
            BodyParams::dynamic(Vec3::new(-2.0, 0.0, 0.0), 0.7, 0.0),
            BodyParams::STATIC,
        );
        assert_eq!(separating.delta_vel_a, Vec3::ZERO, "背向接触面运动不应反弹");
    }

    /// 摩擦取参与响应的动体均值；切向分量按 max(0, 1-f) 衰减。
    #[test]
    fn friction_uses_dynamic_mean_and_damps_tangent() {
        let contact = Contact {
            normal: Vec3::Y,
            penetration: 0.1,
        };
        // 两动体：f = (0.5 + 0.0)/2 = 0.25 → 切向 ×0.75
        let both = solve_contact(
            contact,
            BodyParams::dynamic(Vec3::new(3.0, -4.0, 0.0), 0.0, 0.5),
            BodyParams::dynamic(Vec3::new(1.0, -4.0, 0.0), 0.0, 0.0),
        );
        let rel_after = Vec3::new(3.0, -4.0, 0.0) + both.delta_vel_a
            - (Vec3::new(1.0, -4.0, 0.0) + both.delta_vel_b);
        assert!(
            (rel_after - Vec3::new(1.5, 0.0, 0.0)).length() < 1e-5,
            "切向相对速度应从 2 衰减到 1.5、法向归零，实际 {rel_after:?}"
        );
        // 单动体：摩擦取动体自身（静体不参与）；v_n = +1 朝向穿透、e = 0 → 法向归零
        let sliding = solve_contact(
            contact,
            BodyParams::dynamic(Vec3::new(2.0, 1.0, 0.0), 0.0, 0.8),
            BodyParams::STATIC,
        );
        let v_after = Vec3::new(2.0, 1.0, 0.0) + sliding.delta_vel_a;
        assert!(
            (v_after - Vec3::new(0.4, 0.0, 0.0)).length() < 1e-5,
            "切向应 ×0.2、法向归零，实际 {v_after:?}"
        );
    }

    // ---- 世界级测试：step_physics 管线 ----

    /// 测试台：Time 资源 + 推进一帧（Time::advance 确定性驱动 + step_physics）。
    fn make_world() -> World {
        let mut world = World::new();
        world.insert_resource(Time::default());
        world
    }

    fn step(world: &World, dt: f32) {
        world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(dt as f64));
        step_physics(world);
    }

    /// 顶面 y = 0 的大静态平台（地面）。
    fn spawn_floor(world: &mut World) -> Entity {
        world.spawn((
            Transform::from_xyz(0.0, -1.0, 0.0),
            Collider::static_box(Vec3::new(50.0, 1.0, 50.0)),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_ball(
        world: &mut World,
        pos: Vec3,
        vel: Vec3,
        radius: f32,
        restitution: f32,
        friction: f32,
    ) -> Entity {
        world.spawn((
            Transform::from_translation(pos),
            Collider::dynamic_sphere(radius, restitution),
            RigidBody {
                velocity: vel,
                friction,
                ..RigidBody::default()
            },
        ))
    }

    fn ball_position(world: &World, entity: Entity) -> Vec3 {
        world
            .get::<Transform>(entity)
            .expect("球实体应有 Transform")
            .position
    }

    fn ball_velocity(world: &World, entity: Entity) -> Vec3 {
        world
            .get::<RigidBody>(entity)
            .expect("球实体应有 RigidBody")
            .velocity
    }

    /// 自由落体对照解析解 y(t) = h - ½gt²（60Hz 半隐式欧拉的离散容差：
    /// y_n = h - ½g·t·(t+dt)，比连续解析解多落 ½g·t·dt ≈ 0.082 m @ 1s）。
    #[test]
    fn free_fall_matches_analytic_solution() {
        let mut world = make_world();
        let h = 10.0;
        let ball = spawn_ball(
            &mut world,
            Vec3::new(0.0, h, 0.0),
            Vec3::ZERO,
            0.5,
            0.0,
            0.0,
        );
        let steps = 60; // 1 秒 @ 60Hz
        for _ in 0..steps {
            step(&world, DT);
        }
        let t = steps as f32 * DT;
        let y = ball_position(&world, ball).y;
        let g = 9.81f32;
        let analytic = h - 0.5 * g * t * t;
        let discrete = h - 0.5 * g * t * (t + DT); // 半隐式欧拉的精确离散解
        assert!((y - discrete).abs() < 1e-2, "y {y} 应匹配离散解 {discrete}");
        assert!(
            (y - analytic).abs() < 0.5 * g * t * DT + 1e-2,
            "y {y} 应落在解析解 {analytic} 的 60Hz 离散容差内"
        );
        assert!(
            y < analytic,
            "半隐式欧拉应比连续解析解略快落地（{y} < {analytic}）"
        );
        // 速度对照 v = g·t
        let vy = ball_velocity(&world, ball).y;
        assert!((vy - -g * t).abs() < 1e-2, "垂直速度应为 -g·t，实际 {vy}");
    }

    /// gravity_scale 线性缩放重力加速度。
    #[test]
    fn gravity_scale_scales_acceleration() {
        let mut world = make_world();
        let normal = spawn_ball(
            &mut world,
            Vec3::new(0.0, 50.0, 0.0),
            Vec3::ZERO,
            0.5,
            0.0,
            0.0,
        );
        let doubled = world.spawn((
            Transform::from_xyz(10.0, 50.0, 0.0),
            Collider::dynamic_sphere(0.5, 0.0),
            RigidBody {
                gravity_scale: 2.0,
                ..RigidBody::default()
            },
        ));
        step(&world, DT);
        let dt = Duration::from_secs_f64(DT as f64).as_secs_f32();
        assert!(
            (ball_velocity(&world, normal).y - PHYSICS_GRAVITY.y * dt).abs() < 1e-6,
            "一帧后 v = g·dt"
        );
        assert!(
            (ball_velocity(&world, doubled).y - PHYSICS_GRAVITY.y * 2.0 * dt).abs() < 1e-6,
            "scale=2 应双倍重力"
        );
        // 位置同步积分：pos += v·dt
        assert!(
            (ball_position(&world, normal).y - (50.0 + PHYSICS_GRAVITY.y * dt * dt)).abs() < 1e-6
        );
    }

    /// restitution = 0.8 的落地回弹顶点 ≈ 下落高度 × 0.64（e²），且逐次衰减。
    #[test]
    fn restitution_bounce_height_scales_with_e_squared() {
        const FINE: f32 = 1.0 / 240.0; // 细步长降低离散误差
        let mut world = make_world();
        spawn_floor(&mut world);
        // 静息球心高度 0.5，从 2.5 落下 → 下落高度 2.0；回弹顶点应 ≈ 2.0 × 0.8² = 1.28
        let ball = spawn_ball(
            &mut world,
            Vec3::new(0.0, 2.5, 0.0),
            Vec3::ZERO,
            0.5,
            0.8,
            0.0,
        );

        let mut samples: Vec<f32> = Vec::new();
        for _ in 0..900 {
            step(&world, FINE);
            samples.push(ball_position(&world, ball).y);
        }
        let rest = 0.5f32;
        let touching = |y: f32| y <= rest + 1e-3;
        let first = samples
            .iter()
            .position(|&y| touching(y))
            .expect("应发生第一次触地");
        let second = samples[first + 1..]
            .iter()
            .position(|&y| touching(y))
            .map(|i| i + first + 1)
            .expect("应发生第二次触地");
        let apex1 = samples[first + 1..second]
            .iter()
            .cloned()
            .fold(f32::MIN, f32::max);
        let apex2 = samples[second..].iter().cloned().fold(f32::MIN, f32::max);

        let expected = 2.0 * 0.8 * 0.8;
        assert!(
            ((apex1 - rest) - expected).abs() < 0.09,
            "首次回弹顶点（高于静息面）应 ≈ {expected}，实际 {}",
            apex1 - rest
        );
        assert!(
            apex2 < apex1 && apex2 > rest,
            "第二次回弹应低于首次（能量耗散）且高于静息面：{apex2} vs {apex1}"
        );
    }

    /// restitution = 0 落地静止：穿透 < 1e-3、3 秒后速度 ≈ 0、不抖。
    #[test]
    fn zero_restitution_settles_without_jitter() {
        const FINE: f32 = 1.0 / 120.0;
        let mut world = make_world();
        spawn_floor(&mut world);
        let ball = spawn_ball(
            &mut world,
            Vec3::new(0.0, 3.0, 0.0),
            Vec3::ZERO,
            0.5,
            0.0,
            0.0,
        );
        let steps = (3.0 / FINE) as usize; // 3 秒
        let mut tail: Vec<f32> = Vec::new();
        for i in 0..steps {
            step(&world, FINE);
            if i >= steps - 10 {
                tail.push(ball_position(&world, ball).y);
            }
        }
        let y = ball_position(&world, ball).y;
        let penetration = 0.5 - y; // 球心应回到静息高度（半径 0.5）
        assert!(
            penetration.abs() < 1e-3,
            "静止穿透应 < 1e-3，实际 {penetration}"
        );
        assert!(
            ball_velocity(&world, ball).length() < 1e-5,
            "静止后速度应 ≈ 0，实际 {:?}",
            ball_velocity(&world, ball)
        );
        let spread = tail.iter().cloned().fold(f32::MIN, f32::max)
            - tail.iter().cloned().fold(f32::MAX, f32::min);
        assert!(
            spread < 1e-5,
            "静止后最后 10 帧位置波动应 < 1e-5（不抖），实际 {spread}"
        );
    }

    /// 摩擦：斜落碰地后切向速度显著衰减（对比 friction = 0）。
    #[test]
    fn friction_damps_tangential_slide_on_landing() {
        const FINE: f32 = 1.0 / 120.0;
        let mut world = make_world();
        spawn_floor(&mut world);
        let sticky = spawn_ball(
            &mut world,
            Vec3::new(-10.0, 1.5, 0.0),
            Vec3::new(3.0, -6.0, 0.0),
            0.5,
            0.0,
            0.9,
        );
        let slippery = spawn_ball(
            &mut world,
            Vec3::new(10.0, 1.5, 0.0),
            Vec3::new(3.0, -6.0, 0.0),
            0.5,
            0.0,
            0.0,
        );
        for _ in 0..60 {
            step(&world, FINE); // 0.5 s：两球都已落地滑行
        }
        let sticky_vx = ball_velocity(&world, sticky).x.abs();
        let slippery_vx = ball_velocity(&world, slippery).x.abs();
        assert!(
            sticky_vx < 0.1,
            "friction=0.9 的球切向速度应几乎归零，实际 {sticky_vx}"
        );
        assert!(
            slippery_vx > 2.5,
            "friction=0 的球应保留切向速度，实际 {slippery_vx}"
        );
        assert!(
            sticky_vx < slippery_vx * 0.1,
            "摩擦球的切向应显著低于无摩擦球"
        );
    }

    /// 线性阻尼：无碰撞自由飞行的速度指数衰减对照解析式 v = v₀·e^(-λt)。
    /// 同时覆盖「只有 RigidBody 无 Collider = 仅受力积分」的缺席语义。
    #[test]
    fn linear_damping_decays_velocity_exponentially() {
        let mut world = make_world();
        let damping = 2.0f32;
        let flyer = world.spawn((
            Transform::from_xyz(0.0, 100.0, 0.0),
            RigidBody {
                velocity: Vec3::new(4.0, 0.0, 0.0),
                gravity_scale: 0.0, // 隔离重力，只看阻尼
                friction: 0.0,
                linear_damping: damping,
            },
        ));
        let seconds = 1.0;
        // round 而非截断：1.0/(1/60 f32) = 59.999…，直接 as usize 会丢一步
        for _ in 0..(seconds / DT).round() as usize {
            step(&world, DT);
        }
        let want = 4.0 * (-damping * seconds).exp();
        let vx = ball_velocity(&world, flyer).x;
        assert!(
            (vx - want).abs() < 1e-4,
            "v 应为 v₀·e^(-λt) = {want}，实际 {vx}"
        );
        assert!(
            ball_position(&world, flyer).x > 0.0,
            "无 Collider 也应照常积分位移"
        );
    }

    /// 静态体不积分不移动：只有 Collider 的台子（被高速球撞击）、以及
    /// is_static = true 但带 RigidBody 的实体，位置都 bit 级不变。
    #[test]
    fn static_bodies_never_integrate_or_move() {
        const FINE: f32 = 1.0 / 120.0;
        let mut world = make_world();
        // 静态台子：只有 Collider（无 RigidBody 的静态缺席语义），顶面 y = 0.8
        let table_pos = Vec3::new(0.0, 0.4, 0.0);
        let table = world.spawn((
            Transform::from_translation(table_pos),
            Collider::static_box(Vec3::new(1.0, 0.4, 1.0)),
        ));
        // is_static = true 但带 RigidBody：同样按静态处理（取或语义）
        let anchored_pos = Vec3::new(5.0, 5.0, 5.0);
        let anchored = world.spawn((
            Transform::from_translation(anchored_pos),
            Collider {
                shape: ColliderShape::Sphere { radius: 0.5 },
                is_static: true,
                restitution: 0.0,
            },
            RigidBody {
                velocity: Vec3::new(0.0, -3.0, 0.0),
                ..RigidBody::default()
            },
        ));
        // 高速球横撞台子
        spawn_ball(
            &mut world,
            Vec3::new(-8.0, 1.3, 0.0),
            Vec3::new(9.0, 0.0, 0.0),
            0.5,
            0.3,
            0.0,
        );

        for _ in 0..120 {
            step(&world, FINE);
        }
        assert_eq!(
            ball_position(&world, table),
            table_pos,
            "只有 Collider 的静态体位置应 bit 级不变"
        );
        assert_eq!(
            ball_position(&world, anchored),
            anchored_pos,
            "is_static = true 的实体不应被积分（重力也不生效）"
        );
    }

    /// 确定性：同一场景跑两遍，逐采样位置 bit 级一致。
    #[test]
    fn simulation_is_deterministic_across_runs() {
        fn build() -> (World, Vec<Entity>) {
            let mut world = make_world();
            spawn_floor(&mut world);
            // 五颗异质球：不同高度 / 初速 / 弹性 / 摩擦 / 阻尼（坐标写死，无随机源）
            let balls = [
                (
                    Vec3::new(-2.0, 4.0, 0.0),
                    Vec3::new(1.0, 0.0, 0.5),
                    0.8,
                    0.1,
                    0.0,
                ),
                (Vec3::new(0.0, 6.5, 1.0), Vec3::ZERO, 0.5, 0.4, 0.3),
                (
                    Vec3::new(2.0, 3.2, -1.0),
                    Vec3::new(-1.5, 0.0, 0.0),
                    0.2,
                    0.8,
                    0.0,
                ),
                (
                    Vec3::new(0.5, 8.0, 0.0),
                    Vec3::new(0.5, 1.0, -0.5),
                    0.9,
                    0.2,
                    0.5,
                ),
                (Vec3::new(-1.0, 5.0, 2.0), Vec3::ZERO, 0.35, 0.6, 0.1),
            ]
            .map(|(pos, vel, radius, restitution, friction)| {
                spawn_ball(&mut world, pos, vel, radius, restitution, friction)
            })
            .to_vec();
            (world, balls)
        }

        let (world_a, balls_a) = build();
        let (world_b, balls_b) = build();
        let mut trace_a: Vec<Vec3> = Vec::new();
        let mut trace_b: Vec<Vec3> = Vec::new();
        for _ in 0..240 {
            step(&world_a, DT);
            step(&world_b, DT);
            for (&a, &b) in balls_a.iter().zip(&balls_b) {
                trace_a.push(ball_position(&world_a, a));
                trace_b.push(ball_position(&world_b, b));
            }
        }
        assert_eq!(trace_a, trace_b, "同输入两次模拟应逐采样 bit 级一致");
    }

    /// 多球落地互不重叠：终态两两距离 ≥ 半径和 - ε（堆叠松弛的残余穿透
    /// 容差），且没有球掉出地面。
    #[test]
    fn ball_pile_rests_without_overlap() {
        const FINE: f32 = 1.0 / 120.0;
        let mut world = make_world();
        spawn_floor(&mut world);
        let radius = 0.5;
        // 4 + 3 双层堆：底层 4 颗贴地排开，上层 3 颗落在缝隙上方（坐标写死）
        let starts = [
            Vec3::new(-1.65, 0.6, 0.0),
            Vec3::new(-0.55, 0.6, 0.0),
            Vec3::new(0.55, 0.6, 0.0),
            Vec3::new(1.65, 0.6, 0.0),
            Vec3::new(-1.1, 1.9, 0.0),
            Vec3::new(0.0, 2.0, 0.0),
            Vec3::new(1.1, 1.9, 0.0),
        ];
        let balls: Vec<Entity> = starts
            .iter()
            .map(|&pos| spawn_ball(&mut world, pos, Vec3::ZERO, radius, 0.05, 0.6))
            .collect();
        for _ in 0..720 {
            step(&world, FINE); // 6 秒：充分沉降
        }
        let min_gap = radius * 2.0;
        for (i, &a) in balls.iter().enumerate() {
            let pa = ball_position(&world, a);
            assert!(
                pa.y > 0.0 && pa.y < 2.0,
                "球 {i} 应停在地面附近，实际 y = {}",
                pa.y
            );
            for &b in balls.iter().skip(i + 1) {
                let dist = pa.distance(ball_position(&world, b));
                assert!(
                    dist >= min_gap - 5e-3,
                    "球 {i} 与后继球的最小间距应 ≥ {min_gap}，实际 {dist}"
                );
            }
        }
    }

    /// Time 资源缺席 → dt = 0：不积分、速度不变（空中球纹丝不动）。
    #[test]
    fn missing_time_resource_means_zero_delta() {
        let mut world = World::new(); // 故意不插入 Time
        let pos = Vec3::new(1.0, 20.0, 2.0);
        let ball = spawn_ball(&mut world, pos, Vec3::new(1.0, 0.0, 0.0), 0.5, 0.0, 0.0);
        step_physics(&world);
        assert_eq!(ball_position(&world, ball), pos, "dt = 0 不应积分位移");
        assert_eq!(
            ball_velocity(&world, ball),
            Vec3::new(1.0, 0.0, 0.0),
            "dt = 0 不应改速度"
        );
    }

    // ---- 静态推离查询：resolve_static_push / resolve_against_statics ----

    /// 单墙阻挡量精确：推离后球心到墙面恰为半径，且修正只沿法线
    /// （y / z 分量为零——切向分量保留是「贴墙滑行」的数学基础）。
    #[test]
    fn static_push_single_wall_exact_and_normal_only() {
        let sphere = ColliderShape::Sphere { radius: 0.45 };
        // +X 侧墙：内缘 x = 17.5（盒心 18.0、半边 0.5），z 向长条；
        // 玩家从场地内侧（x < 17.5）逼近
        let wall = ColliderShape::Box {
            half_extents: Vec3::new(0.5, 0.5, 19.0),
        };
        let statics = [(Vec3::new(18.0, 0.5, 0.0), wall)];
        // 球心 17.4 距内缘 0.1（球面已侵入 0.35）：推离应恰好把球心送回
        // 「内缘 − 半径」= 17.05，且不带任何切向修正
        let pos = Vec3::new(17.4, 0.45, 5.0);
        let push = resolve_static_push(pos, &sphere, &statics);
        assert!(
            (push.x + 0.35).abs() < 1e-5,
            "推离量应 = 穿透 0.35（−X 向），实际 {push:?}"
        );
        assert_eq!(push.y, 0.0, "修正只沿法线（−X），不应有 y 分量");
        assert_eq!(push.z, 0.0, "切向 z 分量保留（不把玩家往墙中段推）");
        assert!(
            (pos.x + push.x - 17.05).abs() < 1e-5,
            "推离后球心到墙面应为半径 0.45"
        );
    }

    /// 两正交墙夹角：顺序累加、逐个用推离后的位置重测——两轴修正互不
    /// 干扰，合计把球从角点推离；无重叠时恒零。
    #[test]
    fn static_push_two_orthogonal_walls_accumulate() {
        let sphere = ColliderShape::Sphere { radius: 0.45 };
        let statics = [
            (
                Vec3::new(18.0, 0.5, 0.0),
                ColliderShape::Box {
                    half_extents: Vec3::new(0.5, 0.5, 19.0),
                },
            ),
            (
                Vec3::new(0.0, 0.5, 18.0),
                ColliderShape::Box {
                    half_extents: Vec3::new(19.0, 0.5, 0.5),
                },
            ),
        ];
        // 角点内侧球面各侵入两墙 0.1 m：先对 +X 墙推离 0.35（原位置重测），
        // 再对 +Z 墙用推离后的位置重测再推 0.35
        let push = resolve_static_push(Vec3::new(17.4, 0.45, 17.4), &sphere, &statics);
        assert!(
            (push.x + 0.35).abs() < 1e-5 && (push.z + 0.35).abs() < 1e-5,
            "两墙应各贡献 0.35 的正交推离，实际 {push:?}"
        );
        // 场地中央无重叠：零向量
        let clear = resolve_static_push(Vec3::new(0.0, 0.45, 0.0), &sphere, &statics);
        assert_eq!(clear, Vec3::ZERO, "无穿透应返回零向量");
    }

    /// 空静态表恒等：没有任何静态体时返回零向量（位置原样可用）。
    #[test]
    fn static_push_empty_statics_is_identity() {
        let sphere = ColliderShape::Sphere { radius: 0.45 };
        let push = resolve_static_push(Vec3::new(3.0, 0.45, -4.0), &sphere, &[]);
        assert_eq!(push, Vec3::ZERO);
    }

    /// 世界封装：收集遵循「仅 Collider 无 RigidBody = 静态」缺席语义——
    /// 挡路的是静态墙；路径上的动态球（完整刚体）不参与推离，
    /// `is_static = true` 的实体（即使带 RigidBody）同样静态。
    #[test]
    fn resolve_against_statics_skips_dynamic_bodies() {
        let mut world = World::new();
        // 静态墙：只有 Collider（组件缺席 = 静态）
        world.spawn((
            Transform::from_xyz(18.0, 0.5, 0.0),
            Collider::static_box(Vec3::new(0.5, 0.5, 19.0)),
        ));
        // 动态球停在推离路径上（与查询球明显重叠）：完整刚体——
        // 若被错误计入，下方断言的推离量将不止墙的 0.35
        world.spawn((
            Transform::from_xyz(17.3, 0.45, 0.0),
            Collider::dynamic_sphere(0.45, 0.0),
            RigidBody::default(),
        ));
        let sphere = ColliderShape::Sphere { radius: 0.45 };
        // 球心 17.4（球面侵入内缘 0.35）：只应被静态墙推回 0.35——
        // 若把路径上的动态球错误计入，推离量将明显更多
        let push = resolve_against_statics(&world, Vec3::new(17.4, 0.45, 0.0), &sphere);
        assert!(
            (push.x + 0.35).abs() < 1e-5,
            "只应被静态墙推离，实际 {push:?}"
        );

        // is_static = true + RigidBody：取或语义，同样静态、同样推离。
        // （摆在第一面墙的 z 跨度之外——同轴重叠的两个静态体会被先后
        // 各推一次，即文档所述 v1 的角点重复推离简化，测试不依赖它。）
        world.spawn((
            Transform::from_xyz(28.0, 0.5, 4.0),
            Collider {
                shape: ColliderShape::Box {
                    half_extents: Vec3::new(0.5, 0.5, 19.0),
                },
                is_static: true,
                restitution: 0.0,
            },
            RigidBody {
                velocity: Vec3::new(1.0, 0.0, 0.0),
                ..RigidBody::default()
            },
        ));
        let push2 = resolve_against_statics(&world, Vec3::new(27.4, 0.45, 4.0), &sphere);
        assert!(
            (push2.x + 0.35).abs() < 1e-5,
            "is_static = true 应同样参与推离，实际 {push2:?}"
        );
    }

    // ---- 射线 / 线段查询：ray_sphere / ray_box / ray_vs_shape / segment ----

    /// 射线-球：正对命中 t / 法线精确；未命中；球在射线背后；起点在球内；
    /// 零长度方向。
    #[test]
    fn ray_sphere_hit_miss_inside_and_degenerates() {
        let dir = Vec3::new(0.0, 0.0, -1.0); // 单位方向：-Z
        // 球心原点 r=1，射线起点 (0,0,5)：t = 5 − 1 = 4，命中点 (0,0,1)
        let (t, normal) =
            ray_sphere(Vec3::new(0.0, 0.0, 5.0), dir, Vec3::ZERO, 1.0).expect("正对球心应命中");
        assert_eq!(t, 4.0, "t = 起距 − 半径");
        assert_eq!(normal, Vec3::Z, "外法线指向射线来侧（+Z）");

        // 未命中：射线从球侧旁掠过（最近距 2 > 半径 1）
        assert!(ray_sphere(Vec3::new(0.0, 2.0, 5.0), dir, Vec3::ZERO, 1.0).is_none());

        // 背面：球整条在射线背后（两根皆负）
        assert!(
            ray_sphere(
                Vec3::new(0.0, 0.0, 5.0),
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::ZERO,
                1.0
            )
            .is_none(),
            "背向球的射线不应命中"
        );

        // 起点在球内：t = 0、法线 = 离开球心的单位向量（指向来侧）
        let (t_in, n_in) = ray_sphere(Vec3::new(0.0, 0.0, 0.5), dir, Vec3::ZERO, 1.0)
            .expect("球内起点应命中 t = 0");
        assert_eq!(t_in, 0.0);
        assert_eq!(n_in, Vec3::Z);

        // 起点与球心重合：退化为 −dir（确定性任意方向，见文档）
        let (t_zero, n_zero) =
            ray_sphere(Vec3::new(0.2, 0.0, 0.0), dir, Vec3::new(0.2, 0.0, 0.0), 1.0)
                .expect("重合起点应命中 t = 0");
        assert_eq!(t_zero, 0.0);
        assert_eq!(n_zero, -dir, "重合退化取 −dir");

        // 零长度方向：None
        assert!(ray_sphere(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::ZERO, 1.0).is_none());
    }

    /// 射线-盒：正对面命中 t / 法线精确；盒内起点最浅出面；斜射角点法线取
    /// 主轴（x 优先）；平行轴起点在界外未命中。
    #[test]
    fn ray_box_face_inside_corner_and_parallel_axis() {
        let half = Vec3::new(1.0, 0.5, 1.0);
        // 正对盒面：起点 (0,0,5) 沿 -Z，命中 z = 1 面：t = 4、法线 +Z
        let (t, normal) = ray_box(
            Vec3::new(0.0, 0.0, 5.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::ZERO,
            &half,
        )
        .expect("正对盒面应命中");
        assert_eq!(t, 4.0);
        assert_eq!(normal, Vec3::Z, "入射面外法线指向射线来侧");

        // 盒内起点：t = 0、法线取最浅出面的外法线（x<y<z 并列优先）
        // 局部 (0, 0.3, 0)：到顶面 0.2 < 侧面 1.0 < 底面 0.8 → +Y 面
        let (t_in, n_in) = ray_box(
            Vec3::new(0.0, 0.3, 0.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::ZERO,
            &half,
        )
        .expect("盒内起点应命中 t = 0");
        assert_eq!(t_in, 0.0);
        assert_eq!(n_in, Vec3::Y, "最浅出面为顶面（+Y 外法线）");

        // 斜射角点：单位立方盒，从 (5,5,0) 沿 (−1,−1,0)/√2 入射，x / y
        // 两轴同时给出 tmin（命中点在角点 (1,1,0)）→ 入射轴并列取 x
        // （确定性），法线 = −sign(dir.x)·X = +X（主轴，非角点对角向量）
        let diagonal = Vec3::new(-1.0, -1.0, 0.0).normalize();
        let (t_c, n_c) = ray_box(Vec3::new(5.0, 5.0, 0.0), diagonal, Vec3::ZERO, &Vec3::ONE)
            .expect("斜射角点应命中");
        assert!(
            (t_c - 4.0 * std::f32::consts::SQRT_2).abs() < 1e-5,
            "t = 4√2，实际 {t_c}"
        );
        assert_eq!(n_c, Vec3::X, "角点斜射法线取主轴 x（并列优先）");

        // 平行轴且起点在界外：x 向射线从 y=5（盒半边 0.5）掠过 → 未命中
        assert!(ray_box(Vec3::new(0.0, 5.0, 0.0), Vec3::X, Vec3::ZERO, &half).is_none());

        // 平行轴且起点在界内：该轴不裁剪，沿 x 贯穿命中侧面 x = 1
        let (t_p, n_p) =
            ray_box(Vec3::new(-5.0, 0.0, 0.0), Vec3::X, Vec3::ZERO, &half).expect("贯穿应命中");
        assert_eq!(t_p, 4.0);
        assert_eq!(n_p, Vec3::new(-1.0, 0.0, 0.0));

        // 背面：盒在射线背后
        assert!(
            ray_box(
                Vec3::new(0.0, 0.0, 5.0),
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::ZERO,
                &half
            )
            .is_none()
        );
    }

    /// 线段扫掠防穿模：步长（10 m）远大于目标直径（1.6 m），两端点都在
    /// 目标外——当帧点判定必漏检，线段判定命中；from == to 容差内为 None。
    #[test]
    fn segment_hits_shape_catches_high_speed_tunnelling() {
        let shape = ColliderShape::Sphere { radius: 0.8 };
        let from = Vec3::new(0.0, 0.0, 0.0);
        let to = Vec3::new(10.0, 0.0, 0.0);
        // 佐证「点判定必漏」：两端点到球心距都是 5 > 0.8
        let center = Vec3::new(5.0, 0.0, 0.0);
        assert!(from.distance(center) > 0.8 && to.distance(center) > 0.8);
        let hit = segment_hits_shape(from, to, center, &shape).expect("线段应命中扫掠球");
        assert!(
            (hit.t - 4.2).abs() < 1e-5,
            "命中参数 = 5 − 0.8 = 4.2，实际 {}",
            hit.t
        );
        assert!(
            (hit.point - Vec3::new(4.2, 0.0, 0.0)).length() < 1e-5,
            "命中点 = (4.2, 0, 0)，实际 {:?}",
            hit.point
        );
        assert_eq!(
            hit.normal,
            Vec3::new(-1.0, 0.0, 0.0),
            "法线指向线段来侧（−X）"
        );

        // from == to 容差内：None（零长度线段无扫掠语义）
        assert!(segment_hits_shape(from, from, center, &shape).is_none());
        // 起点已在目标内：t = 0（近距离判定仍成立）
        let inside = segment_hits_shape(
            Vec3::new(5.0, 0.0, 0.3),
            Vec3::new(6.0, 0.0, 0.3),
            center,
            &shape,
        )
        .expect("扫掠段穿过球体应命中");
        assert_eq!(inside.t, 0.0);
    }

    /// 世界级 raycast：静态动态都命中、最近者胜；filter 排除发射者（最近
    /// 命中）后取次近；max_t 裁剪；空世界 / 零方向 None。
    #[test]
    fn raycast_picks_nearest_and_filter_excludes_shooter() {
        let mut world = World::new();
        // 近处动态球（t=4）与远处静态球（t=11）沿 +Z 排开
        let near = world.spawn((
            Transform::from_xyz(0.0, 0.0, 5.0),
            Collider::dynamic_sphere(1.0, 0.0),
            RigidBody::default(),
        ));
        let far = world.spawn((
            Transform::from_xyz(0.0, 0.0, 12.0),
            Collider::static_sphere(1.0),
        ));
        let dir = Vec3::Z;

        let (entity, hit) = raycast(&world, Vec3::ZERO, dir, 50.0).expect("应命中最近的近处球");
        assert_eq!(entity, near, "最近命中（t=4）应优先于远处（t=11）");
        assert!((hit.t - 4.0).abs() < 1e-6);
        assert_eq!(
            hit.normal,
            Vec3::new(0.0, 0.0, -1.0),
            "外法线指向射线来侧（−Z）"
        );

        // filter 排除发射者（近处球）→ 命中远处静态球（静态动态语义一致）
        let (entity_f, hit_f) = raycast_filtered(&world, Vec3::ZERO, dir, 50.0, |e| e != near)
            .expect("排除近处后应命中远处球");
        assert_eq!(entity_f, far);
        assert!((hit_f.t - 11.0).abs() < 1e-6);

        // max_t 裁剪：4 < t ≤ 11 的查询范围内只有近处；全排除时 None
        assert_eq!(raycast(&world, Vec3::ZERO, dir, 10.0).unwrap().0, near);
        assert!(raycast_filtered(&world, Vec3::ZERO, dir, 10.0, |e| e != near).is_none());
        // 零长度方向 / 空世界：None
        assert!(raycast(&world, Vec3::ZERO, Vec3::ZERO, 10.0).is_none());
        let empty = World::new();
        assert!(raycast(&empty, Vec3::ZERO, dir, 10.0).is_none());
    }

    // ---- 球扫掠查询：sphere_sweep / sphere_sweep_statics ----

    /// 扫掠畅通返回 None；斜向滑过角点（最近距 0.566 > 半径 0.5）不误报。
    #[test]
    fn sphere_sweep_clear_path_and_corner_graze_miss() {
        let wall = ColliderShape::Box {
            half_extents: Vec3::new(0.5, 5.0, 5.0),
        };
        let statics = [(Vec3::new(18.0, 0.0, 0.0), wall)];
        // 背向墙位移：整段畅通
        assert!(
            sphere_sweep(Vec3::ZERO, Vec3::new(-5.0, 0.0, 0.0), 0.5, &statics).is_none(),
            "远离墙的扫掠应畅通"
        );

        // 斜向滑过单位盒角点：路径沿 y = −x + 3 从 (1,2) 位移 (0.7,−0.7)，
        // 到盒（角点 (1,1)）的最近距 ≈ 0.762 > 半径 0.5 → 不误报
        let unit_box = ColliderShape::Box {
            half_extents: Vec3::ONE,
        };
        let statics = [(Vec3::ZERO, unit_box)];
        assert!(
            sphere_sweep(
                Vec3::new(1.0, 2.0, 0.0),
                Vec3::new(0.7, -0.7, 0.0),
                0.5,
                &statics
            )
            .is_none(),
            "最近距 0.762 > 0.5 的斜掠不应误报命中"
        );
        // 非空转佐证：同一位置与路径、半径放大到 0.8（> 终点距 0.762，
        // < 起点距 1.0——终点重叠、起点自由的括弧情形）应命中
        assert!(
            sphere_sweep(
                Vec3::new(1.0, 2.0, 0.0),
                Vec3::new(0.7, -0.7, 0.0),
                0.8,
                &statics
            )
            .is_some(),
            "半径 0.8 覆盖角点附近路径的斜掠应命中（测试非空转的佐证）"
        );
    }

    /// 扫掠正碰：半径 0.5 的球向墙位移 2.0、球心到墙面 1.2 →
    /// t ≈ (1.2 − 0.5) / 2 = 0.35、阻挡法线 −X；起点已重叠时 t = 0。
    #[test]
    fn sphere_sweep_head_on_fraction_and_normal() {
        // 墙内缘面 x = 1.2（盒心 1.7、半边 0.5）
        let wall = ColliderShape::Box {
            half_extents: Vec3::new(0.5, 5.0, 5.0),
        };
        let statics = [(Vec3::new(1.7, 0.0, 0.0), wall)];
        let (t, normal) = sphere_sweep(Vec3::ZERO, Vec3::new(2.0, 0.0, 0.0), 0.5, &statics)
            .expect("朝墙位移 2 m 应碰撞");
        assert!(
            (t - 0.35).abs() < 1e-3,
            "首次碰撞分数 ≈ 0.35（皮肤收缩 1e-4），实际 {t}"
        );
        assert!(
            (normal - Vec3::new(-1.0, 0.0, 0.0)).length() < 1e-5,
            "阻挡法线应从墙指向球（−X），实际 {normal:?}"
        );

        // 起点已重叠（球心 0.8、球面越过墙面 1.2 的 0.1）：t = 0、法线同向
        let (t_in, n_in) = sphere_sweep(
            Vec3::new(0.8, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            0.5,
            &statics,
        )
        .expect("起点重叠应报告 t = 0");
        assert_eq!(t_in, 0.0);
        assert!((n_in - Vec3::new(-1.0, 0.0, 0.0)).length() < 1e-5);
    }

    /// 世界封装：只对静态体扫掠（动态刚体不阻挡），缺席语义与
    /// resolve_against_statics 一致。
    #[test]
    fn sphere_sweep_statics_skips_dynamic_bodies() {
        let mut world = World::new();
        // 动态球横在路径上：不参与阻挡
        world.spawn((
            Transform::from_xyz(2.0, 0.0, 0.0),
            Collider::dynamic_sphere(1.0, 0.0),
            RigidBody::default(),
        ));
        // 静态墙：内缘 x = 5.5（盒心 6.0、半边 0.5）
        world.spawn((
            Transform::from_xyz(6.0, 0.0, 0.0),
            Collider::static_box(Vec3::new(0.5, 5.0, 5.0)),
        ));
        // 位移 5.6：终点 (5.6,0,0) 嵌入墙内（终点重叠的括弧情形）；
        // 路径上的动态球（半径 1，距路径起点仅 2）若被错误计入会在
        // t = 1/5.6 ≈ 0.18 处阻挡——远早于墙的 0.89
        let (t, normal) = sphere_sweep_statics(&world, Vec3::ZERO, Vec3::new(5.6, 0.0, 0.0), 0.5)
            .expect("应被静态墙阻挡");
        // 接触点：球心到 5.5 − 0.5 = 5.0 → t = 5/5.6 ≈ 0.8929
        assert!(
            (t - 5.0 / 5.6).abs() < 1e-3,
            "只应被静态墙挡在 t ≈ 0.893，实际 {t}"
        );
        assert!((normal - Vec3::new(-1.0, 0.0, 0.0)).length() < 1e-5);
        // 反向（远离墙）：畅通
        assert!(sphere_sweep_statics(&world, Vec3::ZERO, Vec3::new(-5.6, 0.0, 0.0), 0.5).is_none());
    }

    // ---- broadphase 接入：密集场景求解后无重叠（网格候选保守性回归）----

    /// 100 球密集阵（初始间距 0.62 < 直径 0.7，即摆放即穿透）：交给
    /// step_physics（均匀网格 broadphase 配对）挤开，6 s 沉降后两两不
    /// 重叠、无球掉出地面——网格候选漏对时密集阵是首先暴露的场景。
    #[test]
    fn hundred_ball_dense_grid_separates_without_overlap() {
        const FINE: f32 = 1.0 / 120.0;
        let mut world = make_world();
        spawn_floor(&mut world);
        let radius = 0.35;
        // 10×10 方阵：中心对称排布，全部同层 y = 半径
        let mut balls: Vec<Entity> = Vec::with_capacity(100);
        for row in 0..10 {
            for col in 0..10 {
                let pos = Vec3::new((col as f32 - 4.5) * 0.62, radius, (row as f32 - 4.5) * 0.62);
                balls.push(spawn_ball(&mut world, pos, Vec3::ZERO, radius, 0.05, 0.6));
            }
        }
        for _ in 0..720 {
            step(&world, FINE); // 6 s：充分挤开 + 沉降
        }
        let positions: Vec<Vec3> = balls.iter().map(|&b| ball_position(&world, b)).collect();
        let min_gap = radius * 2.0;
        for (i, &pa) in positions.iter().enumerate() {
            assert!(
                pa.y > 0.0 && pa.y < 2.0,
                "球 {i} 应停在地面附近，实际 y = {pa}"
            );
            for (j, &pb) in positions.iter().enumerate().skip(i + 1) {
                let dist = pa.distance(pb);
                assert!(
                    dist >= min_gap - 5e-3,
                    "球 ({i},{j}) 求解后最小间距应 ≥ {min_gap}，实际 {dist}"
                );
            }
        }
    }
}
