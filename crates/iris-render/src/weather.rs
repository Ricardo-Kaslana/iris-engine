//! 天气表面（R92）：全局 [`Weather`] 资源（湿度 / 积雪量 / 涟漪强度）
//! 与三组**纯函数**表面响应——湿表面（粗糙度压低 + 反照率变暗）、积雪
//! 坡度遮罩、程序化哈希网格涟漪。
//!
//! ## 语义摘要
//!
//! - **资源缺席 = 逐位不变**（全仓 opt-in 范式）：渲染器缺席资源时向
//!   binding 21 写全零 uniform，`fs_shaded` 的天气分支整段不执行——
//!   既有像素与命令流与引入前逐位一致（GPU 测试 FNV 锁定）。
//! - **湿表面**：[`wet_surface`]——粗糙度向镜面地板 0.08 压低（镜面
//!   反射聚拢），反照率乘暗因子 0.7（水膜吸收），暗化按 `(1 − metallic)`
//!   加权（金属不因水变暗）；`f0` 保持**干 albedo** 基准（金属湿感 v1
//!   不建模——Schlick 入射角色变留后续轮）。
//! - **积雪**：[`snow_mask`]（世界法线 y 的 smoothstep 坡度门——朝上
//!   满雪、陡坡 / 垂直面露原材质）× [`snow_surface`]（反照率向雪白 /
//!   粗糙度向 0.6 混合）。无时间累积（量由游戏侧设定——`Weather` 是
//!   状态不是系统，降水 → 积雪的演化留给上层）。
//! - **涟漪**：[`ripple_gradient`]——世界 XZ 哈希网格（格宽 0.4 m），
//!   每格一滴周期性落点（周期 1.3 s、相位由格哈希决定），环形波
//!   `A·decay·sin(π·u)` 以 0.85 m/s 扩展、0.6 s 衰减殆尽；返回高度场
//!   梯度（XZ），调用方按强度 × 湿度扰动着色法线。**无状态、纯函数、
//!   同输入逐位确定**（格哈希是整数运算，跨平台精确）。
//!
//! ## WGSL 镜像契约（三处同步）
//!
//! [`wet_surface`] / [`snow_mask`] / [`snow_surface`] / [`ripple_gradient`]
//! / [`weather_cell_hash`] 与着色器侧逐式镜像，**改一侧必须同步全部**：
//! - `shaders/standard.wgsl` 的 `weather_wet_rough` 段（fs_shaded 天气
//!   分支）与 `weather_ripple_gradient`（湿地表扰动）；
//! - `shaders/water.wgsl` 的 `weather_ripple_gradient`（雨打水面扰动，
//!   同式第二份拷贝——WGSL 无 include，双份镜像纪律同
//!   `water_single_wave`）。
//!
//! CPU 单测即公式的解析锚点；sin/cos 的库实现两侧不同，CPU↔GPU 不做
//! 位级等价声明（`fog.rs` / `water.rs` 同款纪律）——同侧同输入的位级
//! 一致由 [`ripple_gradient`] 的确定性单测守护。
//!
//! ## 诚实限制（ARCHITECTURE §8.71）
//!
//! - 涟漪是**周期性程序化噪声**，不与雨滴粒子位置对应（粒子位置状态
//!   在 GPU，CPU 无逐粒位置——R82 诚实边界的延续）；
//! - 湿 / 雪 / 涟漪是**全局量**，无空间变化（无「屋檐下干燥」）；
//! - 阴影 pass 不感知天气（雪的 albedo 不改变深度——无需感知）；
//! - 天气不入场景序列化（[`iris_core::Wind`] 同款——运行期资源）。

use glam::{Vec2, Vec3};

use iris_core::Resource;

// ---------- 资源 ----------

/// 全局天气表面资源（R92）：三个 0..1 量，全零 = 关闭（= 缺席语义）。
///
/// 渲染器每帧读取并 clamp 后上传 binding 21 uniform（x = wetness、
/// y = snow、z = ripple、w = 涟漪时间 t = `Time::elapsed_secs`，Time
/// 缺席 = 0——与水体波形同一时钟源，雨打水面与湿地表涟漪同相）。
/// 纯状态资源无系统推进（降水 → 湿度 / 积雪的自动演化由游戏侧驱动）。
#[derive(Clone, Copy, Debug, Resource)]
pub struct Weather {
    /// 湿度 0..1：粗糙度向 0.08 压低 + 反照率按 (1 − metallic) 乘 0.7。
    pub wetness: f32,
    /// 积雪量 0..1：世界法线 y 坡度遮罩内向雪白混合。
    pub snow: f32,
    /// 涟漪强度 0..1：湿的朝上表面（含水面）的法线扰动幅度。
    pub ripple: f32,
}

impl Default for Weather {
    fn default() -> Self {
        Self {
            wetness: 0.0,
            snow: 0.0,
            ripple: 0.0,
        }
    }
}

impl Weather {
    /// 三个量各自钳到 [0, 1]（上传前的卫生处理；负值 / 越界视为关闭方向）。
    pub fn clamped(&self) -> (f32, f32, f32) {
        (
            self.wetness.clamp(0.0, 1.0),
            self.snow.clamp(0.0, 1.0),
            self.ripple.clamp(0.0, 1.0),
        )
    }
}

// ---------- uniform（与 shaders/standard.wgsl 的 WeatherParams 对齐） ----------

/// 天气 uniform（16 B，POD；binding 21——「不扩 Globals 前缀」扩容先例
/// 的延续，R77/R78/R83/R88 同款）。渲染器每帧覆写；缺席资源 = 全零。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct WeatherRaw {
    /// x = wetness、y = snow、z = ripple、w = 涟漪时间 t（秒）。
    pub params: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<WeatherRaw>() == 16,
    "WeatherRaw 必须为 16 字节（与 WGSL WeatherParams 布局一致）"
);

// ---------- 表面响应常量（WGSL 侧逐值同步——修改必须两侧同步） ----------

/// 湿表面粗糙度地板（镜面聚拢目标值）。
pub const WET_ROUGHNESS_FLOOR: f32 = 0.08;
/// 湿表面反照率乘暗因子（水膜吸收）。
pub const WET_ALBEDO_DARKEN: f32 = 0.7;
/// 积雪坡度门起（世界法线 y 低于此恒无雪）。
pub const SNOW_SLOPE_START: f32 = 0.45;
/// 积雪坡度门止（世界法线 y 高于此满雪）。
pub const SNOW_SLOPE_END: f32 = 0.8;
/// 雪面反照率（线性域雪白）。
pub const SNOW_ALBEDO: [f32; 3] = [0.87, 0.89, 0.94];
/// 雪面粗糙度。
pub const SNOW_ROUGHNESS: f32 = 0.6;

/// 涟漪格宽（米）：世界 XZ 哈希网格的格尺寸（也是落点密度）。
pub const RIPPLE_CELL: f32 = 0.4;
/// 涟漪周期（秒）：每格落点的重复周期。
pub const RIPPLE_PERIOD: f32 = 1.3;
/// 涟漪波前扩展速度（米/秒）。
pub const RIPPLE_SPEED: f32 = 0.85;
/// 涟漪可见寿命（秒；< 周期——滴与滴之间有间歇）。
pub const RIPPLE_LIFE: f32 = 0.6;
/// 涟漪环宽（米）：sin 半周期的空间尺度。
pub const RIPPLE_WIDTH: f32 = 0.09;
/// 涟漪振幅（米）：高度场尺度——梯度上界 ≈ A·π/WIDTH ≈ 0.35（单环，
/// 视觉标定：远中景水面 / 湿地面可判别的最低量级）。
pub const RIPPLE_AMPLITUDE: f32 = 0.01;

// ---------- 纯函数（WGSL 逐式镜像） ----------

/// 湿表面响应（`weather_wet_*` 镜像）：返回（湿后粗糙度, 湿后反照率）。
///
/// - 粗糙度：`roughness·(1−wet) + 0.08·wet`——**重心形式**（两端点
///   wet = 0 / 1 逐位精确：`x·1 + y·0 = x`、`x·0 + y·1 = y`；线性
///   插值 `x + (y−x)·a` 在 a = 1 处有舍入残差——实测 2 ULP，弃用）；
/// - 反照率：`albedo · (1 + (0.7 − 1)·(wet·(1 − metallic)))`——金属
///   不因水变暗（权重 0 ⇒ 乘 1 逐位还原；wet = 1 时 1 − 0.3f32 由
///   Sterbenz 引理精确 = 0.7f32）。
///
/// WGSL 侧**同式展开**（不写 `mix`——其实现形式非规范保证，CPU↔GPU
/// 镜像依赖同一表达式，R63 教训的公式层变体）。`f0` 不在此处理（用
/// 干 albedo——金属湿感 v1 不建模，见模块限制）。
pub fn wet_surface(wetness: f32, metallic: f32, roughness: f32, albedo: Vec3) -> (f32, Vec3) {
    let rough = roughness * (1.0 - wetness) + WET_ROUGHNESS_FLOOR * wetness;
    let darken = 1.0 + (WET_ALBEDO_DARKEN - 1.0) * (wetness * (1.0 - metallic));
    (rough, albedo * darken)
}

/// f32 版 smoothstep（WGSL 内建 `smoothstep` 同义式：t 钳 0..1 后
/// `t²·(3 − 2t)`——两侧展开式一致，单测锚点）。
fn smoothstep_01(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 积雪坡度遮罩（`weather_snow_mask` 镜像）：`snow · smoothstep(0.45,
/// 0.8, clamp(ny, 0, 1))`——朝上（ny ≥ 0.8）满雪、缓坡渐变、陡坡 /
/// 垂直面（ny ≤ 0.45）露原材质。用**几何法线**（`n_geo`——着色法线
/// 含法线贴图高频，作积雪遮罩会出噪点，与阴影偏移用 n_geo 同哲学）。
pub fn snow_mask(normal_y: f32, snow: f32) -> f32 {
    smoothstep_01(SNOW_SLOPE_START, SNOW_SLOPE_END, normal_y.clamp(0.0, 1.0)) * snow
}

/// 积雪表面响应（`weather_snow_*` 镜像）：遮罩内向雪混合（**重心形式**
/// `x·(1−m) + y·m`——两端点逐位精确，理由见 [`wet_surface`]）。返回
/// （雪后粗糙度, 雪后反照率）。mask = 0 时两项逐位还原。
pub fn snow_surface(mask: f32, roughness: f32, albedo: Vec3) -> (f32, Vec3) {
    let keep = 1.0 - mask;
    let rough = roughness * keep + SNOW_ROUGHNESS * mask;
    let alb = Vec3::new(
        albedo.x * keep + SNOW_ALBEDO[0] * mask,
        albedo.y * keep + SNOW_ALBEDO[1] * mask,
        albedo.z * keep + SNOW_ALBEDO[2] * mask,
    );
    (rough, alb)
}

/// 涟漪格哈希（`weather_hash` 镜像）：整数混淆（乘常数 + 移位异或），
/// 返回两个 u32 词。整数运算精确——CPU / GPU / 跨平台逐位一致（浮点
/// 哈希无此保证，故走整数路）。
pub fn weather_cell_hash(a: u32, b: u32) -> (u32, u32) {
    let mut h = a
        .wrapping_mul(0x9E37_79B1)
        .wrapping_add(b.wrapping_mul(0x85EB_CA77));
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 16;
    let g = h.wrapping_mul(0x27D4_EB2F).wrapping_add(0x5BD1_E995);
    (h, g)
}

/// 高 24 位 → [0, 1)（`particles_v2.rs::v2_fraction` 同式：f32 尾数
/// 24 位内精确表示，乘 2⁻²⁴ 幂次精确）。
fn fraction24(w: u32) -> f32 {
    (w >> 8) as f32 * (1.0 / 16_777_216.0)
}

/// 涟漪高度场梯度（`weather_ripple_gradient` 镜像）：世界 XZ 点 `p`、
/// 时刻 `t`（秒）处的梯度（XZ 平面）。调用方按 `强度 × 湿度` 缩放后
/// 加到着色法线（`normalize(n + vec3(g.x, 0, g.y))`）。
///
/// 结构：3×3 邻格求和（固定 dz 外 / dx 内循环序——确定性求和序），
/// 每格一个落点——中心 = 格内 [0.15, 0.85] 带内的哈希偏移（贴格边会
/// 与邻格落点成对挤在一起，收缩带避开）；相位 = 哈希 × 周期；环形波
/// 高度 `A·decay·sin(π·u)`，`u = (r − v·age)/width + 1 ∈ (0, 2)`
/// （波前居中、前导凸后拖凹），梯度 = `dh/du / width · d̂`。
pub fn ripple_gradient(p: Vec2, t: f32) -> Vec2 {
    let cell = Vec2::new((p.x / RIPPLE_CELL).floor(), (p.y / RIPPLE_CELL).floor());
    let mut grad = Vec2::ZERO;
    for dz in -1i32..=1 {
        for dx in -1i32..=1 {
            let cx = (cell.x as i32 + dx) as u32;
            let cz = (cell.y as i32 + dz) as u32;
            let (w0, w1) = weather_cell_hash(cx, cz);
            let fx = fraction24(w0);
            let fy = fraction24(w0 ^ w1);
            let fp = fraction24(w1);
            let center = Vec2::new(
                (cell.x + dx as f32 + 0.15 + 0.7 * fx) * RIPPLE_CELL,
                (cell.y + dz as f32 + 0.15 + 0.7 * fy) * RIPPLE_CELL,
            );
            let t0 = fp * RIPPLE_PERIOD;
            // 周期取模（rem_euclid 语义：WGSL 侧 t − P·floor((t − t0)/P) 同式）
            let mut age = t - t0;
            age -= RIPPLE_PERIOD * (age / RIPPLE_PERIOD).floor();
            if age < RIPPLE_LIFE {
                let d = p - center;
                let r = d.length();
                if r > 1e-5 {
                    let u = (r - RIPPLE_SPEED * age) / RIPPLE_WIDTH + 1.0;
                    if u > 0.0 && u < 2.0 {
                        let decay = 1.0 - age / RIPPLE_LIFE;
                        let dhdu = RIPPLE_AMPLITUDE
                            * decay
                            * std::f32::consts::PI
                            * (std::f32::consts::PI * u).cos();
                        grad += (dhdu / RIPPLE_WIDTH) * (d / r);
                    }
                }
            }
        }
    }
    grad
}

#[cfg(test)]
mod tests {
    use super::*;

    /// f32 逐位相等辅助。
    fn bits(x: f32) -> u32 {
        x.to_bits()
    }

    /// Vec3 逐位比较辅助（glam 0.33 本仓特性集无 Vec3::to_bits——逐分量）。
    fn bits3(v: Vec3) -> [u32; 3] {
        [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()]
    }

    /// 默认全关 + clamped 卫生钳制。
    #[test]
    fn defaults_and_clamping() {
        let w = Weather::default();
        assert_eq!(w.clamped(), (0.0, 0.0, 0.0), "默认 = 全关（缺席语义）");
        let w = Weather {
            wetness: -3.0,
            snow: 0.5,
            ripple: 7.0,
        };
        assert_eq!(w.clamped(), (0.0, 0.5, 1.0), "越界值向关闭方向钳制");
    }

    /// 湿表面：wet = 0 逐位还原；wet = 1 锚点值（粗糙度恰为地板、
    /// 非金属反照率恰乘 0.7、金属反照率逐位不变）。
    #[test]
    fn wet_surface_zero_is_identity_full_is_anchor() {
        let alb = Vec3::new(0.62, 0.43, 0.21);
        // wet = 0：两项逐位还原
        let (r0, a0) = wet_surface(0.0, 0.35, 0.55, alb);
        assert_eq!(bits(r0), bits(0.55), "wet=0 粗糙度逐位不变");
        assert_eq!(bits3(a0), bits3(alb), "wet=0 反照率逐位不变");
        // wet = 1、非金属：粗糙度 = 地板；反照率 = ×0.7 恰
        let (r1, a1) = wet_surface(1.0, 0.0, 0.55, alb);
        assert_eq!(bits(r1), bits(WET_ROUGHNESS_FLOOR));
        assert_eq!(bits3(a1), bits3(alb * WET_ALBEDO_DARKEN));
        // wet = 1、金属：反照率逐位不变（暗化权重 (1−metallic) = 0）
        let (_, am) = wet_surface(1.0, 1.0, 0.55, alb);
        assert_eq!(bits3(am), bits3(alb), "金属不因水变暗");
        // 半湿介于两端（单调方向）
        let (rh, _) = wet_surface(0.5, 0.0, 0.55, alb);
        assert!(
            r1 < rh && rh < r0,
            "粗糙度应单调向地板压低：{r1} < {rh} < {r0}"
        );
    }

    /// 积雪遮罩：坡度门端点、单调性、snow = 0 恒零。
    #[test]
    fn snow_mask_slope_gate() {
        assert_eq!(snow_mask(0.3, 1.0), 0.0, "陡坡（ny ≤ 起点下方）无雪");
        assert_eq!(snow_mask(SNOW_SLOPE_START, 1.0), 0.0, "起点恰为零");
        let full = snow_mask(SNOW_SLOPE_END, 1.0);
        assert!((full - 1.0).abs() < 1e-6, "止点满雪");
        assert_eq!(snow_mask(1.0, 1.0), 1.0, "正上方满雪");
        // 过渡带单调
        let mut prev = -1.0f32;
        for k in 0..=10 {
            let ny = SNOW_SLOPE_START + (SNOW_SLOPE_END - SNOW_SLOPE_START) * k as f32 / 10.0;
            let m = snow_mask(ny, 1.0);
            assert!(m >= prev, "过渡带应单调不减");
            prev = m;
        }
        assert_eq!(snow_mask(1.0, 0.0), 0.0, "snow = 0 恒零");
        assert!((snow_mask(1.0, 0.4) - 0.4).abs() < 1e-6, "量值线性进入");
        // 负法线（朝下）钳 0
        assert_eq!(snow_mask(-0.5, 1.0), 0.0);
    }

    /// 积雪表面：mask = 0 逐位还原；mask = 1 恰为雪参数；中间混合。
    #[test]
    fn snow_surface_mixes_to_snow() {
        let alb = Vec3::new(0.2, 0.5, 0.1);
        let (r0, a0) = snow_surface(0.0, 0.35, alb);
        assert_eq!(bits(r0), bits(0.35));
        assert_eq!(bits3(a0), bits3(alb));
        let (r1, a1) = snow_surface(1.0, 0.35, alb);
        assert_eq!(bits(r1), bits(SNOW_ROUGHNESS));
        assert_eq!(bits3(a1), bits3(Vec3::from(SNOW_ALBEDO)));
        let (rm, am) = snow_surface(0.5, 0.35, alb);
        assert!(
            r0 < rm && rm < r1,
            "粗糙度应单调向雪混合：{r0} < {rm} < {r1}"
        );
        assert!(am.x > alb.x && am.x < SNOW_ALBEDO[0], "反照率向雪白混合");
    }

    /// 格哈希：同格确定、邻格散布、分数覆盖 [0, 1) 且不退化。
    #[test]
    fn cell_hash_deterministic_and_spread() {
        for (a, b) in [(0u32, 0), (1, 0), (0, 1), (12345, 67890), (u32::MAX, 7)] {
            assert_eq!(weather_cell_hash(a, b), weather_cell_hash(a, b));
        }
        // 邻格互异（批量：64×64 网格内无碰撞对首词）
        let mut seen = std::collections::BTreeSet::new();
        for z in 0..64u32 {
            for x in 0..64u32 {
                let (w0, _) = weather_cell_hash(x, z);
                assert!(seen.insert(w0), "格 ({x},{z}) 首词碰撞");
            }
        }
        // 分数散布：min/max 覆盖宽（不挤在端点）
        let (mut lo, mut hi) = (1.0f32, 0.0f32);
        for z in 0..32u32 {
            for x in 0..32u32 {
                let (_, w1) = weather_cell_hash(x, z);
                lo = lo.min(fraction24(w1));
                hi = hi.max(fraction24(w1));
            }
        }
        assert!(lo < 0.05 && hi > 0.95, "相位分数应铺满 [0,1)：{lo}..{hi}");
    }

    /// 涟漪梯度：确定性（逐位）、有界（3×3 单环梯度上界）、非零存在
    /// 但稀疏（大部分时刻大部分点无环覆盖——间歇性感）、t 推进变化。
    #[test]
    fn ripple_gradient_anchors() {
        // 逐位确定性
        let p = Vec2::new(3.37, -2.11);
        let a = ripple_gradient(p, 1.234);
        let b = ripple_gradient(p, 1.234);
        assert_eq!(
            [a.x.to_bits(), a.y.to_bits()],
            [b.x.to_bits(), b.y.to_bits()],
            "同输入逐位一致"
        );
        // 有界：单环 |grad| ≤ A·π/WIDTH；9 环最坏 < 9×上界
        let single_max = RIPPLE_AMPLITUDE * std::f32::consts::PI / RIPPLE_WIDTH;
        let bound = 9.0 * single_max;
        let mut nonzero = 0usize;
        let mut total = 0usize;
        for iz in 0..96 {
            for ix in 0..96 {
                let q = Vec2::new(ix as f32 * 0.11 - 5.0, iz as f32 * 0.13 + 2.0);
                let g = ripple_gradient(q, 0.77);
                assert!(g.length() <= bound + 1e-6, "梯度越界：{g:?} @ {q:?}");
                total += 1;
                if g.length() > 0.01 {
                    nonzero += 1;
                }
            }
        }
        assert!(nonzero > 0, "应存在非零涟漪点（非退化）");
        let frac = nonzero as f32 / total as f32;
        assert!(
            frac > 0.01 && frac < 0.9,
            "涟漪应间歇覆盖（部分点活跃），实际 {frac}"
        );
        // 时间推进改变场（波前在动）
        let c = ripple_gradient(p, 1.234);
        let d = ripple_gradient(p, 1.334);
        assert_ne!(
            [c.x.to_bits(), c.y.to_bits()],
            [d.x.to_bits(), d.y.to_bits()],
            "时间推进应改变涟漪场"
        );
        // 远离所有落点（取格中心间的空档在大 age 时……至少存在近零点）
        let mut min_g = f32::MAX;
        for iz in 0..64 {
            for ix in 0..64 {
                let q = Vec2::new(ix as f32 * 0.37 - 11.0, iz as f32 * 0.41 - 7.0);
                min_g = min_g.min(ripple_gradient(q, 2.0).length());
            }
        }
        assert!(min_g < 0.02, "应存在近零点（环外空档）：{min_g}");
    }

    /// 涟漪镜像公式的结构性锚点（防 CPU/WGSL 漂移）：振幅 / 速度 /
    /// 寿命的量级关系——波前半径 = speed·age < speed·life = 最大覆盖
    /// 半径 0.51 m ≈ 1.3 格（3×3 邻域够覆盖，无接缝漏洞）。
    #[test]
    fn ripple_geometry_covers_neighborhood() {
        let max_radius = RIPPLE_SPEED * RIPPLE_LIFE;
        assert!(
            max_radius > RIPPLE_CELL,
            "波前最大半径应超过一格（{max_radius} > {RIPPLE_CELL}）"
        );
        assert!(max_radius < 2.0 * RIPPLE_CELL, "邻域 3×3 应完整覆盖波前");
        // 寿命 < 周期（滴间有间歇）是纯常量关系——编译期锁定
        const _: () = assert!(RIPPLE_LIFE < RIPPLE_PERIOD, "寿命短于周期（滴间有间歇）");
    }

    /// WGSL 镜像看门（源码层）：standard.wgsl 的天气绑定与公式、
    /// water.wgsl 的涟漪拷贝、粒子 v2 模拟着色器的缺席门——三处任一
    /// 漂移（改名 / 改常数）在此炸出。
    #[test]
    fn wgsl_mirrors_present() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/shaders");
        let standard = std::fs::read_to_string(format!("{dir}/standard.wgsl")).unwrap();
        // binding 21 与 uniform 结构
        assert!(standard.contains("@group(0) @binding(21) var<uniform> weather: WeatherParams;"));
        assert!(standard.contains("struct WeatherParams"));
        // 湿 / 雪 / 涟漪公式关键串（常数与 Rust 侧一致）
        assert!(standard.contains("roughness = roughness * (1.0 - wet) + 0.08 * wet;"));
        assert!(standard.contains("smoothstep(0.45, 0.8, clamp(n_geo.y, 0.0, 1.0))"));
        assert!(standard.contains("weather_ripple_gradient"));
        let water = std::fs::read_to_string(format!("{dir}/water.wgsl")).unwrap();
        assert!(
            water.contains("weather_ripple_gradient"),
            "water.wgsl 应有涟漪镜像拷贝"
        );
        assert!(
            water.contains("weather: vec4<f32>"),
            "WaterParams 应有天气通道"
        );
        let sim = std::fs::read_to_string(format!("{dir}/particles_v2_sim.wgsl")).unwrap();
        // v2 扩展的三个缺席门（select——缺席 = 逐位不变的结构保证）
        assert!(sim.contains("select(params.origin_dt.xyz, params.origin_dt.xyz + vol,"));
        assert!(sim.contains("params.axis.xyz + params.seed_jitter.x * u"));
        assert!(sim.contains("select(vel_raw, vel_raw / (1.0 + params.extent_damp.w * dt)"));
    }
}
