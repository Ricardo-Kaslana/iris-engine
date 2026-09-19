//! 浮力（R93）：`BuoyancyPlane` 资源 + 逐形状浸没体积解析纯函数。
//!
//! ## 语义摘要
//!
//! - 世界插入 [`BuoyancyPlane`] 即开启浮力——[`submerged_volume`] 按形状解析
//!   算出浸没体积，`step_physics` 的半隐式积分段在重力之后给动体加
//!   `ρ_f · V_sub · g / m` 的向上加速度，并按浸没比例叠加一次水阻指数衰减。
//! - **资源缺席 = 逐位不变**（R76 `ContactSolverV2` 范式）：无资源时积分段
//!   不进入任何新算术，六游戏轨迹 FNV 锁原值通过。浸没比例为 0（完全在水
//!   上）同样零新算术——无浸没的体与引入前逐位一致。
//! - **纯函数优先**：浸没体积 / 比例 / 升加速度全部是可脱离 World 单测的自由
//!   函数（与 `solve_contact` / `shape_inertia` 同口径），`step_physics` 只做
//!   查表与写回。
//! - **确定性**：浸没体积是解析闭式或固定序多项式求和（无迭代、无随机、无
//!   全局状态），同输入 `to_bits()` 逐位一致；驱动它的水面高度来自资源字段
//!   （世界状态），时间步长来自 `Time`。
//!
//! ## 浸没比例的几何（本模块的核心论证）
//!
//! [`box_submerged_ratio`] 走的不是采样而是闭式。论据：盒（含 OBB）内任一点的
//! 世界 y 是**局部坐标的仿射函数**（局部→世界是仿射变换，取 y 是线性投影，
//! 仿射 ∘ 仿射 = 仿射），故把盒参数化到单位立方体 `v ∈ [0,1]³` 后，带符号深度
//! `d(v) = α + Σ βᵢ·vᵢ`（`βᵢ = 2·aᵢ.y·hᵢ`，`aᵢ` = 旋转矩阵第 i 列 = 第 i 条
//! 世界轴向、`hᵢ` = 第 i 条本地半轴长）。浸没体积 = 全体积 × 测度
//! `{v : d(v) < 0}`，而后者正是三次 **box spline**（截断幂 `(x)_+³` 的错差 /
//! 除以差商），给出下面的有限和闭式。球走球冠解析、胶囊走竖直包络线性比例。
//!
//! # 诚实限制清单（v1 明确不做）
//!
//! - **无扶正力矩**：升力只作用在质心，与姿态无关——船**不会**自己立起来，
//!   倾斜的盒子也不会回正。要做需要浸没体积形心（center of buoyancy）与
//!   浮力-重力偶矩，成本与风险翻倍，留后续轮；
//! - **无附加质量（added mass）与水体速度场**：水恒静止，物体不动水；
//!   浪涌对姿态 / 位置的耦合、波浪力（Morison 方程）一概不做；
//! - **线性水阻，无二次拖拽**：`exp(−linear_drag·ratio·dt)` 一阶项；
//!   形状阻力系数（Cd）、雷诺数相关、端面 / 侧向各向异性阻力均不做；
//! - **Capsule 是近似**：取「竖直包络内的线性浸没比例 × 总体积」——棱柱段
//!   精确、两端半球帽偏差最大（最坏出现在水面恰切过端帽时，误差量级
//!   ≤ 半球体积占总体积之比，与 `ARCHITECTURE` §8.72 一致）；
//! - **形状只认 [`ColliderShape`](crate::physics::ColliderShape) 四变体**：
//!   无任意三角网格的浸没体积（那需要体素化 / 凸分解，另立项）；
//! - **只作用于 Dynamic 刚体**：静态 / 运动学体不参与积分（与 `step_physics`
//!   一致），**只挂 `RigidBody` 无 `Collider` 的「飞行体」没有浮力**——
//!   没有形状就没有体积，诚实跳过；
//! - **水面恒为水平面**：`height` 是常量世界 y，无倾斜、无波形耦合（海浪
//!   起伏进浮力 = 让物理跟着位移图走，跨 crate 且确定性论证更重，留 R94 后）；
//! - **密度不由形状推导**：`RigidBody.mass` 与 `fluid_density` 是两个独立
//!   旋钮，引擎不替用户算「这个物体该不该浮」——平衡吃水由二者之比给出，
//!   见 [`buoyancy_acceleration`]。

use glam::{Quat, Vec3};

use crate::Resource;

/// 标准水密度（千克/米³，淡水量级；海水 ≈ 1025 由用户自行给定）。
/// 仅作 [`BuoyancyPlane::fluid_density`] 的默认值参考常量。
pub const WATER_DENSITY: f32 = 1000.0;

/// 浮力平面（世界水平面）与流体参数——**资源即开关**，缺席 = 无浮力。
///
/// 注册方式：`world.insert_resource(BuoyancyPlane::default())`（默认 `height`
/// = 0，即 xz 平面）。`height` 由游戏直设，或置 [`Self::follow_water`] 让
/// `iris-engine` 的同步系统每帧从水面组件灌入（跨界同步的理由见
/// `ARCHITECTURE` §4.52：iris-core 零渲染依赖，读不到 `iris_render::water`）。
#[derive(Clone, Copy, Debug, PartialEq, Resource)]
pub struct BuoyancyPlane {
    /// 水面的**世界 y 高度**（米）。平面以下算浸没。
    pub height: f32,
    /// 流体密度 ρ_f（千克/米³，默认水 1000）。≤ 0 = 无升力（退化关闭）。
    pub fluid_density: f32,
    /// 水阻（每秒指数衰减系数，按浸没比例加权）：`v ×= exp(−k·ratio·dt)`。
    /// 默认 2.0——落水后约 0.5 s 内速度衰减到 1/e，观感上「入水即沉一段再
    /// 稳定」；0 = 无水阻（只有升力，物体会在平衡吃水上下长期振荡）。
    pub linear_drag: f32,
    /// 是否让 `iris-engine` 的水面同步系统覆写 [`Self::height`]。
    /// **默认 `false`**：游戏手设的高度永不被引擎改写（opt-in 才有自动跟随）。
    pub follow_water: bool,
}

impl Default for BuoyancyPlane {
    fn default() -> Self {
        Self {
            height: 0.0,
            fluid_density: WATER_DENSITY,
            linear_drag: 2.0,
            follow_water: false,
        }
    }
}

impl BuoyancyPlane {
    /// 按水面高度构造（其余走默认水参数）。
    pub fn water(height: f32) -> Self {
        Self {
            height,
            ..Self::default()
        }
    }
}

/// 形状总体积（米³，纯函数）——浸没比例的归一化分母，也是「中性浮力」
/// 判据（`mass / volume` = 物体密度）的入口。
///
/// - 球 `4/3·π·r³`；盒 / 定向盒 `(2hx)(2hy)(2hz)`；
/// - 胶囊 = 圆柱段 `π r²·2(hh−r)` + 完整球 `4/3·π·r³`（两端帽合成一球）；
///   `half_height == radius` 时圆柱段退化为零，恰为球（与 [`shape_inertia`]
///   同款退化校验，见单测）。
///
/// 任何非正 / 退化参数都返回 0（不返回负体积），调用方据此跳过浮力。
pub fn shape_volume(shape: &crate::physics::ColliderShape) -> f32 {
    use crate::physics::ColliderShape as S;
    use std::f32::consts::PI;
    match *shape {
        S::Sphere { radius } => {
            if radius <= 0.0 {
                0.0
            } else {
                4.0 / 3.0 * PI * radius * radius * radius
            }
        }
        S::Box { half_extents } | S::OrientedBox { half_extents } => {
            8.0 * half_extents.x * half_extents.y * half_extents.z
        }
        S::Capsule {
            radius,
            half_height,
        } => {
            if radius <= 0.0 {
                return 0.0;
            }
            let cyl = 2.0 * (half_height - radius).max(0.0);
            PI * radius * radius * cyl + 4.0 / 3.0 * PI * radius * radius * radius
        }
    }
}

/// 球冠体积（精确解析）：半径 `r` 的球，自**底部**起算的浸没深度 `h`。
///
/// `V(h) = π·h²·(3r − h)/3`，定义域钳到 `[0, 2r]`：`h = 0` 给 0、`h = r` 给
/// 半球 `2/3·π·r³`、`h = 2r` 给整球 `4/3·π·r³`（三处都是解析锚点，见单测——
/// 末项即公式在 h=2r 处 `π·4r²·r/3 = 4/3·π·r³` 的闭合）。
pub fn sphere_cap_volume(radius: f32, depth: f32) -> f32 {
    use std::f32::consts::PI;
    if radius <= 0.0 {
        return 0.0;
    }
    let h = depth.clamp(0.0, 2.0 * radius);
    PI * h * h * (3.0 * radius - h) / 3.0
}

/// 平行六面体被水平面所截的**浸没比例**（闭式，见模块文档的仿射论证）。
///
/// 参数是单位立方体参数化后的带符号深度 `d(v) = alpha + Σ betas[i]·v_i`
/// （`v ∈ [0,1]³`，深度 < 0 为浸没）。返回 `{d < 0}` 的测度（∈ [0,1]）。
///
/// 实现：对负系数轴做 `v_i ← 1 − v_i` 的变量替换（把系数翻正、同时把
/// `alpha` 平移该系数），于是区域化为 `{Σ γᵢ vᵢ < t}`（γ ≥ 0），其测度是
/// 截断幂的错差（box spline）：
///
/// ```text
/// ratio = 1/(m!·Πγ) · Σ_{S⊆active} (−1)^|S| · (t − Σ_{i∈S} γᵢ)_+^m
/// ```
///
/// `m` = 非退化轴数。水平轴（γᵢ ≈ 0）不参与分母也不进子集枚举——该轴在
/// [0,1] 上自由，测度贡献恰为 ×1，故维度自然递降（三次 → 二次 → 一次 →
/// 常数），避免 1/0。退化门 `γᵢ ≤ ε·max(1, Σγ)` 是相对阈值（缩放不变）。
pub fn box_submerged_ratio(alpha: f32, betas: [f32; 3]) -> f32 {
    // 变量替换：负系数轴翻正，alpha 吸收被平移的常量。
    let mut a = alpha;
    let mut gamma = [0.0f32; 3];
    for i in 0..3 {
        if betas[i] < 0.0 {
            a += betas[i];
            gamma[i] = -betas[i];
        } else {
            gamma[i] = betas[i];
        }
    }
    let t = -a;
    // 非退化轴集合（相对阈值——全零时 m = 0，退化成「整盒在同一侧」）。
    let scale = 1.0f32.max(gamma[0] + gamma[1] + gamma[2]);
    let eps = 1e-6 * scale;
    let mut active: [f32; 3] = [0.0; 3];
    let mut m = 0usize;
    for g in gamma {
        if g > eps {
            active[m] = g;
            m += 1;
        }
    }
    // 固定枚举序 0..(1<<m)：子集符号交错求和（顺序写死 ⇒ 逐位确定）。
    let pow = match m {
        0 => {
            // 无变化轴：整盒深度恒为 alpha。
            return if t > 0.0 { 1.0 } else { 0.0 };
        }
        1 => 1.0f64,
        2 => 2.0f64,
        _ => 6.0f64,
    };
    let mut denom = 1.0f32;
    for &g in active.iter().take(m) {
        denom *= g;
    }
    let mut sum = 0.0f32;
    for mask in 0u32..(1u32 << m) {
        let mut shift = 0.0f32;
        let mut bits = 0u32;
        for (i, &g) in active.iter().enumerate().take(m) {
            if mask & (1u32 << i) != 0 {
                shift += g;
                bits += 1;
            }
        }
        let x = t - shift;
        if x <= 0.0 {
            continue;
        }
        let term = match m {
            1 => x,
            2 => x * x,
            _ => x * x * x,
        };
        sum += if bits & 1 == 0 { term } else { -term };
    }
    let ratio = (sum as f64) / (pow * denom as f64);
    // f64 求和只用于压制交错相消的相对误差；结果钳回 [0,1] 后落 f32。
    (ratio as f32).clamp(0.0, 1.0)
}

/// 盒 / 定向盒的浸没比例参数：把「半边长 + 姿态 + 中心 y + 水面高度」化成
/// [`box_submerged_ratio`] 要的 `(alpha, betas)`。单独成函数是为了可测
/// （仿射性由它构造，见单测的「平放退化 = 逐位闭式」锚）。
///
/// `rotation` 的**列** = 三条本地轴在世界系的朝向（glam 列主序约定，与
/// `ColliderShape::OrientedBox` 的检测路径同口径）。
pub fn box_depth_params(
    half_extents: Vec3,
    rotation: Quat,
    center_y: f32,
    plane_height: f32,
) -> (f32, [f32; 3]) {
    // 本地三条轴的世界像（`rotation * e_i` = 旋转矩阵第 i 列）；其 y 分量
    // 就是该轴对世界高度的贡献率。写成向量旋转而非 Mat3 轴访问，是为了让
    // 「列 vs 行」的约定在代码里自证（ glam 的 x_axis 也是列，两者等价，
    // 但 `rotation * Vec3::X` 不需要读者记得列主序——见单测的 90° 硬锚）。
    let dir_y = [
        (rotation * Vec3::X).y,
        (rotation * Vec3::Y).y,
        (rotation * Vec3::Z).y,
    ];
    let h = [half_extents.x, half_extents.y, half_extents.z];
    let betas = [
        2.0 * dir_y[0] * h[0],
        2.0 * dir_y[1] * h[1],
        2.0 * dir_y[2] * h[2],
    ];
    // d(v) = (center_y − plane) + Σ dir_y[i]·(2 h_i v_i − h_i)
    //      = [D − Σ dir_y[i] h_i] + Σ β_i v_i
    let d = center_y - plane_height;
    let alpha = d - (dir_y[0] * h[0] + dir_y[1] * h[1] + dir_y[2] * h[2]);
    (alpha, betas)
}

/// 浸没体积（米³，纯函数）：形状 `shape` 位于世界 `center` / `rotation` 时，
/// 在水平面 `plane_height` 以下的那部分体积。值域 `[0, shape_volume]`。
///
/// 逐形状策略（诚实限制的近似项都在这里的分支上）：
/// - [`ColliderShape::Sphere`](crate::physics::ColliderShape::Sphere)：
///   球冠精确闭式 [`sphere_cap_volume`]；
/// - [`ColliderShape::Box`](crate::physics::ColliderShape::Box)（AABB，
///   `rotation` 无语义 ⇒ 恒按恒等旋转）与
///   [`ColliderShape::OrientedBox`](crate::physics::ColliderShape::OrientedBox)：
///   [`box_submerged_ratio`] × 全体积（两者都是精确闭式——AABB 是该式在
///   恒等姿态下的退化档）；
/// - [`ColliderShape::Capsule`](crate::physics::ColliderShape::Capsule)：
///   竖直包围盒线性比例 × 全体积（**近似**，模块文档诚实限制）。
pub fn submerged_volume(
    shape: &crate::physics::ColliderShape,
    center: Vec3,
    rotation: Quat,
    plane_height: f32,
) -> f32 {
    use crate::physics::ColliderShape as S;
    let total = shape_volume(shape);
    if total <= 0.0 {
        return 0.0;
    }
    match *shape {
        S::Sphere { radius } => {
            // 底部世界 y = center.y − r；浸没深度自底向上。
            sphere_cap_volume(radius, plane_height - (center.y - radius))
        }
        S::Box { half_extents } | S::OrientedBox { half_extents } => {
            // Box 是 AABB 语义（rotation 不参与碰撞检测），这里同样忽略之：
            // 恒等旋转 ⇒ 只有本地 Y 贡献高度，与既有碰撞 / 惯量口径一致。
            let rot = if matches!(*shape, S::Box { .. }) {
                Quat::IDENTITY
            } else {
                rotation
            };
            let (alpha, betas) = box_depth_params(half_extents, rot, center.y, plane_height);
            box_submerged_ratio(alpha, betas) * total
        }
        S::Capsule { .. } => {
            // 近似档（模块文档诚实限制）：竖直包络内线性插值浸没比例。
            // extent 是**相对中心**的 (bottom, top) 偏移，须加回 center.y。
            let (bottom, top) = capsule_vertical_extent(shape, rotation);
            let span = top - bottom;
            if span <= 0.0 {
                return 0.0;
            }
            let ratio = ((plane_height - (center.y + bottom)) / span).clamp(0.0, 1.0);
            ratio * total
        }
    }
}

/// 胶囊的竖直包络 `(bottom, top)`——**相对中心**的 y 偏移（米）：轴 = 旋转后
/// 的本地 ±Y（与碰撞检测同一约定，R50），端帽各外扩 `radius`，圆柱段轴向半长
/// `(half_height − radius)` 按其竖直分量 `|axis.y|` 投影。退化（零半径）时给
/// 零长区间，调用方除法前须查 `top > bottom`。
///
/// 恒等姿态给 `(−half_height, +half_height)`（胶囊定义的半高即包围半高）；
/// 轴水平时退化为 `(−radius, +radius)`。
pub fn capsule_vertical_extent(
    shape: &crate::physics::ColliderShape,
    rotation: Quat,
) -> (f32, f32) {
    use crate::physics::ColliderShape as S;
    let (radius, half_height) = match *shape {
        S::Capsule {
            radius,
            half_height,
        } => (radius.max(0.0), half_height.max(0.0)),
        _ => return (0.0, 0.0),
    };
    let axis = rotation * Vec3::Y;
    let axis_y = axis.y.clamp(-1.0, 1.0);
    // 轴向半跨 = |axis.y| · (hh − r)（圆柱段半长）；水平轴时为 0。
    let seg = (half_height - radius).max(0.0) * axis_y.abs();
    (-seg - radius, seg + radius)
}

/// 升加速度（米/秒²，向上）：`ρ_f · V_sub · g / m`。
///
/// `mass ≤ 0`（无穷质量 / 静态语义，与 `step_physics` 的逆质量 0 同口径）
/// 或非正的 `fluid_density` / `gravity` 一律返回 0——不推动的体不因浮力
/// 而被挪动。平衡吃水判据：`V_sub = m / ρ_f` 时升力恰等于重力（单测锚）。
pub fn buoyancy_acceleration(
    submerged_volume: f32,
    mass: f32,
    fluid_density: f32,
    gravity: f32,
) -> f32 {
    if mass <= 0.0 || fluid_density <= 0.0 || gravity <= 0.0 || submerged_volume <= 0.0 {
        return 0.0;
    }
    fluid_density * submerged_volume * gravity / mass
}

/// 中性浮力的平衡浸没体积：`V\* = m / ρ_f`（升力 == 重力的解析判据，
/// 供游戏侧与测试用；`mass ≤ 0` / `ρ_f ≤ 0` 返回 0）。
pub fn neutral_buoyancy_volume(mass: f32, fluid_density: f32) -> f32 {
    if mass <= 0.0 || fluid_density <= 0.0 {
        0.0
    } else {
        mass / fluid_density
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::{ColliderShape, PHYSICS_GRAVITY};
    use glam::{Quat, Vec3, vec3};
    use std::f32::consts::FRAC_1_SQRT_2;

    fn g() -> f32 {
        -PHYSICS_GRAVITY.y
    }

    #[test]
    fn sphere_cap_three_anchors_are_exact() {
        let r = 1.5;
        let v = sphere_cap_volume(r, 0.0);
        assert_eq!(v.to_bits(), 0.0f32.to_bits(), "h=0 必为 0");
        let half = sphere_cap_volume(r, r);
        assert!(
            (half - 4.0 / 3.0 * std::f32::consts::PI * r.powi(3) / 2.0).abs() < 1e-5,
            "h=r 应为半球"
        );
        let full = sphere_cap_volume(r, 2.0 * r);
        let whole = 4.0 / 3.0 * std::f32::consts::PI * r.powi(3);
        assert!(
            (full - whole).abs() < 1e-4,
            "h=2r 应闭合到整球，实测 {full} vs {whole}"
        );
        // 越界钳制（不把负深度变成负体积）。
        assert_eq!(sphere_cap_volume(r, -3.0), 0.0);
        assert!(sphere_cap_volume(r, 99.0) <= whole + 1e-4);
    }

    #[test]
    fn submerged_sphere_matches_cap_and_monotonic() {
        let s = ColliderShape::Sphere { radius: 1.0 };
        let total = shape_volume(&s);
        // 完全在水下 / 完全在水上 / 恰心齐面 三档。
        assert!(submerged_volume(&s, vec3(0.0, 5.0, 0.0), Quat::IDENTITY, 0.0) == 0.0);
        let deep = submerged_volume(&s, vec3(0.0, -5.0, 0.0), Quat::IDENTITY, 0.0);
        assert!((deep - total).abs() < 1e-4, "全浸应给整球体积，实测 {deep}");
        let mid = submerged_volume(&s, Vec3::ZERO, Quat::IDENTITY, 0.0);
        assert!(
            (mid - total / 2.0).abs() < 1e-5,
            "球心齐水面应恰为半球（球冠对称性）"
        );
        // 单调：下沉越多浸没越大（严格）。
        let mut prev = -1.0;
        for k in 0..11 {
            let y = 1.0 - 0.2 * k as f32;
            let v = submerged_volume(&s, vec3(0.0, y, 0.0), Quat::IDENTITY, 0.0);
            assert!(v >= prev, "浸没体积应随下沉单调不减（y={y}）");
            prev = v;
        }
    }

    #[test]
    fn axis_aligned_box_ratio_is_linear_closed_form() {
        // 恒等盒：只有 y 轴贡献 ⇒ 退化为一次公式，比例 = 浸没高 / 全高。
        let s = ColliderShape::Box {
            half_extents: vec3(1.0, 2.0, 3.0),
        };
        let total = shape_volume(&s);
        assert_eq!(total, 8.0 * 1.0 * 2.0 * 3.0);
        let cases = [(2.0f32, 0.0f32), (0.0, 0.5), (-2.0, 1.0), (1.0, 0.25)];
        for (center_y, want) in cases {
            let got = submerged_volume(&s, vec3(0.0, center_y, 0.0), Quat::IDENTITY, 0.0);
            assert!(
                (got - want * total).abs() < 1e-5,
                "盒心 y={center_y} 应给 {want} 比例，实测 {}",
                got / total
            );
        }
        // 盒心齐面 = 恰一半（与球同族的对称锚）。
        let mid = submerged_volume(&s, Vec3::ZERO, Quat::IDENTITY, 0.0);
        assert_eq!(mid / total, 0.5, "平放盒中心齐面必须精确一半");
    }

    #[test]
    fn box_ignores_rotation_like_aabb() {
        // AABB 语义固化：rotation 不参与（与碰撞检测 / shape_inertia 同口径）。
        let s = ColliderShape::Box {
            half_extents: Vec3::splat(1.0),
        };
        let rot = Quat::from_rotation_z(0.7);
        let a = submerged_volume(&s, vec3(0.0, 0.5, 0.0), rot, 0.0);
        let b = submerged_volume(&s, vec3(0.0, 0.5, 0.0), Quat::IDENTITY, 0.0);
        assert_eq!(a.to_bits(), b.to_bits(), "Box 必须无视 rotation");
    }

    /// 数值参照：局部坐标域上的固定序 `n³` 中点采样浸没比例。
    /// 纯函数、无随机 ⇒ 可作为闭式的独立对照（收敛锚，不是解析锚）。
    fn sampled_ratio(h: Vec3, rot: Quat, center_y: f32, plane: f32, n: usize) -> f32 {
        let dir_y = [(rot * Vec3::X).y, (rot * Vec3::Y).y, (rot * Vec3::Z).y];
        let hs = [h.x, h.y, h.z];
        let step = [
            2.0 * hs[0] / n as f32,
            2.0 * hs[1] / n as f32,
            2.0 * hs[2] / n as f32,
        ];
        let mut hit = 0usize;
        for i in 0..n {
            for j in 0..n {
                for k in 0..n {
                    let idx = [i, j, k];
                    let mut d = center_y - plane;
                    for a in 0..3 {
                        // 中点在 (−h_a, h_a) 内等距铺开（求和序固定 i→j→k）。
                        let u = -hs[a] + (idx[a] as f32 + 0.5) * step[a];
                        d += dir_y[a] * u;
                    }
                    if d < 0.0 {
                        hit += 1;
                    }
                }
            }
        }
        hit as f32 / (n * n * n) as f32
    }

    #[test]
    fn oriented_box_closed_form_converges_to_midpoint_sampling() {
        let h = vec3(1.0, 0.4, 2.0);
        // 一般姿态：绕 (1,2,3) 轴 37°——三轴都倾斜 ⇒ 三次公式全程生效。
        let rot = Quat::from_axis_angle(Vec3::new(1.0, 2.0, 3.0).normalize(), 0.645_771_8);
        for &(center_y, plane) in &[(0.3f32, 0.0f32), (-0.2, 0.0), (0.05, -0.1)] {
            let (alpha, betas) = box_depth_params(h, rot, center_y, plane);
            let closed = box_submerged_ratio(alpha, betas);
            let sampled = sampled_ratio(h, rot, center_y, plane, 16);
            assert!(
                (closed - sampled).abs() < 6e-3,
                "center={center_y} 闭式 {closed} vs 16³ 中点 {sampled}（应同量级）"
            );
            assert!((0.0..=1.0).contains(&closed));
        }
    }

    #[test]
    fn oriented_box_flat_pose_equals_aabb_bitwise() {
        // 恒等姿态的 OBB 必须与 Box 走同一数值（平放退化锚）。
        let h = vec3(1.5, 0.5, 2.5);
        let obb = ColliderShape::OrientedBox { half_extents: h };
        let aabb = ColliderShape::Box { half_extents: h };
        for y in [-2.0f32, -0.7, 0.0, 0.4, 2.0] {
            let a = submerged_volume(&obb, vec3(0.0, y, 0.0), Quat::IDENTITY, 0.0);
            let b = submerged_volume(&aabb, vec3(0.0, y, 0.0), Quat::IDENTITY, 0.0);
            assert!(
                (a - b).abs() <= 1e-6 * b.abs().max(1.0),
                "y={y}：OBB 恒等姿态应复现 Box，实测 {a} vs {b}"
            );
        }
    }

    #[test]
    fn oriented_box_quarter_turn_picks_the_right_axis_for_height() {
        // 姿态相关的硬锚（专治「旋转矩阵列 vs 行」的静默转置错）：
        // 盒 half = (1, 2, 3)——三条半轴长互不相同，故「谁躺到世界 Y 上」
        // 决定竖直半跨。中心 y = 0.5、水面 y = 0 ⇒ 浸没比例 = (半跨 − 0.5) /
        // (2·半跨)（平面在下底之上、上底之下的一档线性比例）：
        //   恒等        ⇒ 竖直半跨 = hy = 2 ⇒ 1.5/4   = 0.375；
        //   绕 Z 转 90° ⇒ 本地 X 躺到世界 Y = 1 ⇒ 0.5/2 = 0.25；
        //   绕 X 转 90° ⇒ 本地 Z 躺到世界 Y = 3 ⇒ 2.5/6 ≈ 0.41667。
        // 三值互不相同，取错轴（或把列读成行）必露。
        let h = vec3(1.0, 2.0, 3.0);
        let s = ColliderShape::OrientedBox { half_extents: h };
        let total = shape_volume(&s);
        assert_eq!(total, 48.0);
        let cases: [(&str, Quat, f32); 3] = [
            ("identity", Quat::IDENTITY, 0.375),
            (
                "rot_z_90",
                Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
                0.25,
            ),
            (
                "rot_x_90",
                Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                5.0 / 12.0,
            ),
        ];
        for (name, rot, want) in cases {
            let got = submerged_volume(&s, vec3(0.0, 0.5, 0.0), rot, 0.0);
            assert!(
                (got / total - want).abs() < 1e-6,
                "{name}: 期望浸没比例 {want}，实测 {}",
                got / total
            );
        }
    }

    #[test]
    fn oriented_box_tilted_45_is_half_submerged_at_centre_plane() {
        // 中心关于平面对称 ⇒ 任意姿态的浸没比例都必须是精确 0.5
        // （盒是中心对称凸体，平面过中心时两侧体积相等——闭式应当复现）。
        let h = vec3(1.0, 1.0, 1.0);
        let rots = [
            Quat::IDENTITY,
            Quat::from_rotation_z(std::f32::consts::FRAC_PI_4),
            Quat::from_axis_angle(vec3(FRAC_1_SQRT_2, FRAC_1_SQRT_2, 0.0), 1.1),
            Quat::from_rotation_x(0.3).mul_quat(Quat::from_rotation_z(-0.9)),
        ];
        for rot in rots {
            let (alpha, betas) = box_depth_params(h, rot, 0.0, 0.0);
            let r = box_submerged_ratio(alpha, betas);
            assert!((r - 0.5).abs() < 1e-6, "过中心平面必须精确对半分，实测 {r}");
        }
    }

    #[test]
    fn capsule_uses_vertical_envelope_linear_ratio() {
        let s = ColliderShape::Capsule {
            radius: 0.5,
            half_height: 1.5,
        };
        let total = shape_volume(&s);
        let (bottom, top) = capsule_vertical_extent(&s, Quat::IDENTITY);
        assert!((bottom - (-1.5)).abs() < 1e-6 && (top - 1.5).abs() < 1e-6);
        let full = submerged_volume(&s, vec3(0.0, -3.0, 0.0), Quat::IDENTITY, 0.0);
        assert!((full - total).abs() < 1e-4);
        let none = submerged_volume(&s, vec3(0.0, 3.0, 0.0), Quat::IDENTITY, 0.0);
        assert_eq!(none, 0.0);
        // 横躺胶囊（轴水平）：包络退化到 ±r，比例仍良定义 ∈ [0,1]。
        let flat = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        let (b2, t2) = capsule_vertical_extent(&s, flat);
        assert!((b2 + 0.5).abs() < 1e-6 && (t2 - 0.5).abs() < 1e-6);
        let v = submerged_volume(&s, Vec3::ZERO, flat, 0.0);
        assert!((0.0..=total).contains(&v));
    }

    #[test]
    fn degenerate_shapes_give_zero_volume_and_ratio() {
        assert_eq!(shape_volume(&ColliderShape::Sphere { radius: 0.0 }), 0.0);
        assert_eq!(
            shape_volume(&ColliderShape::Box {
                half_extents: Vec3::ZERO
            }),
            0.0
        );
        let s = ColliderShape::Capsule {
            radius: 0.4,
            half_height: 0.4,
        };
        // hh == r ⇒ 圆柱段零长 ⇒ 恰为球（与 shape_inertia 的退化校验同族）。
        let ball = ColliderShape::Sphere { radius: 0.4 };
        assert!(
            (shape_volume(&s) - shape_volume(&ball)).abs() < 1e-6,
            "退化胶囊体积应等于同半径球"
        );
        assert_eq!(
            submerged_volume(
                &ColliderShape::Sphere { radius: 0.0 },
                Vec3::ZERO,
                Quat::IDENTITY,
                0.0
            ),
            0.0
        );
    }

    #[test]
    fn buoyancy_acceleration_balances_gravity_at_neutral_volume() {
        // 判据：V_sub = m/ρ_f 时升加速度恰等于重力（物体中性悬浮）。
        let mass = 3.0;
        let rho = 600.0;
        let v = neutral_buoyancy_volume(mass, rho);
        assert!((v - 0.005).abs() < 1e-9);
        let a = buoyancy_acceleration(v, mass, rho, g());
        assert!(
            (a - g()).abs() < 1e-4,
            "中性浮力体积的升加速度应等于 g，实测 {a} vs {}",
            g()
        );
        // 非正输入一律 0（无穷质量 / 无水 / 零浸没）。
        assert_eq!(buoyancy_acceleration(1.0, 0.0, rho, g()), 0.0);
        assert_eq!(buoyancy_acceleration(1.0, mass, 0.0, g()), 0.0);
        assert_eq!(buoyancy_acceleration(0.0, mass, rho, g()), 0.0);
        assert_eq!(neutral_buoyancy_volume(-1.0, rho), 0.0);
    }

    #[test]
    fn buoyancy_is_bitwise_deterministic() {
        let s = ColliderShape::OrientedBox {
            half_extents: vec3(0.8, 0.3, 1.2),
        };
        let rot = Quat::from_axis_angle(vec3(0.3, 0.5, 0.8).normalize(), 0.9);
        let f = || {
            let mut acc = 0.0f32;
            for k in 0..32 {
                let y = 0.6 - 0.05 * k as f32;
                acc = acc.to_bits().wrapping_mul(31) as f32
                    + submerged_volume(&s, vec3(0.0, y, 0.0), rot, 0.0);
            }
            acc
        };
        let a = f();
        let b = f();
        assert_eq!(a.to_bits(), b.to_bits(), "同输入两次必须逐位一致");
    }

    #[test]
    fn box_ratio_endpoints_are_exact_zero_and_one() {
        // 全在水上 / 全在水下：闭式必须给精确 0 / 1（无浮点毛刺）。
        let (alpha, betas) = box_depth_params(Vec3::splat(1.0), Quat::IDENTITY, 5.0, 0.0);
        assert_eq!(box_submerged_ratio(alpha, betas), 0.0);
        let (alpha, betas) = box_depth_params(Vec3::splat(1.0), Quat::IDENTITY, -5.0, 0.0);
        assert_eq!(box_submerged_ratio(alpha, betas), 1.0);
        // 恰触碰（底面贴水）⇒ 0 比例，不产生 -0 或极小正数。
        let (alpha, betas) = box_depth_params(Vec3::splat(1.0), Quat::IDENTITY, 1.0, 0.0);
        assert_eq!(box_submerged_ratio(alpha, betas), 0.0);
    }

    #[test]
    fn resource_defaults_are_off_follow_water_and_water_density() {
        let p = BuoyancyPlane::default();
        assert_eq!(p.height, 0.0);
        assert_eq!(p.fluid_density, WATER_DENSITY);
        assert_eq!(p.linear_drag, 2.0);
        assert!(!p.follow_water, "默认不得覆盖游戏手设的 height");
        assert_eq!(BuoyancyPlane::water(3.5).height, 3.5);
    }
}
