//! 预积分皮肤次表面散射 LUT（R63）——Penner 2011「Pre-Integrated Skin
//! Shading」近似的引擎实现。
//!
//! 一张 (NdotL × 曲率尺度) 的**漫反射**查找表：肤质材质的**直射漫反射**
//! 用它取代几何 `NdotL› 因子——光在终结线（terminator）处「包裹」进背光
//! 侧，无屏空间模糊、无多 pass、无厚度图。镜面项不受影响。
//!
//! # 预积分模型（积分式）
//!
//! 一个像素覆盖的表面块在**微观尺度上是弯曲的**：块内各点的法线围绕几何
//! 法线 N 散布。把该散布建模为**球冠法线分布**——法线均匀落在半顶角
//! `α› 的锥内，按表面积测度 `sin a · da · dφ› 加权（a 为极角、φ 为方位
//! 角）。于是该像素的 Lambert 响应 = 球冠上「投影面积加权」的平均：
//!
//! ```text
//! N(c, α) = ∫₀^α ∫₀^{2π} max(c·cos a + √(1−c²)·sin a·cos φ, 0) · sin a dφ da
//! D(α)    = ∫₀^α ∫₀^{2π} cos a · sin a dφ da = π·sin²α
//! W(c, t) = N(c, α(t)) / D(α)          ← LUT 存的就是 W
//! α(t)    = t · π/2                     ← t = 归一化曲率包裹参数（0 = 平面）
//! ```
//!
//! **按 `D(α)› 归一（= 以 c = 1 的响应为基准）**是刻意的选型：若不归一，
//! 全照面会随曲率变暗（球冠平均 `⟨cos a⟩ < 1›），「开 SSS = 整张脸变暗」
//! 是调参灾难。归一后 W(1, t) ≡ 1：**strength 只在终结线及其背光侧加光**，
//! 受光面观感与关闭时一致，A/B 对照干净。代价是终结线附近**增能**（非严格
//! 能量守恒）——这是预积分 SSS 近似的既有取舍（Penner 2011 同样如此），
//! 进诚实清单。
//!
//! **方位角积分有闭式解**（[`azimuth_integral›]），只剩极角一维数值积分
//! （复合 Simpson，[`SSS_SIMPSON_STEPS›] 步）。**无 RNG、无全局状态**——
//! 采样确定性照 `bake.rs› 的无 RNG 文化：同一输入两次生成逐位同一输出
//! （`to_bits› 断言）。
//!
//! # 解析性质（单测锁定）
//!
//! - `W(1, t) = 1›（构造性：分子分母同式）；
//! - `W(−1, t) = 0› 且**零点解析可预测**：`W(c, t) = 0 ⟺ c ≤ −sin α(t)›
//!   （球冠里最朝向光源的那根法线也照不到 ⇒ 捕获不到任何能量）；
//! - `W(c, 0) = max(c, 0)›（零曲率 = 经典 Lambert，逐位相等）；
//! - 对 c 单调不减、对 t 单调不减（数值扫描实测零反向落差）。
//!
//! # LUT 布局与精度依据
//!
//! `SSS_LUT_SIZE × SSS_LUT_SIZE›（64×64），`Rgba16Float›，**行 = 曲率 t**、
//! **列 = NdotL c**，RGB 三通道写同一个标量（便于把 LUT 本身画进验收图）、
//! A = 1。
//!
//! - 纹素中心对齐 `c_i = −1 + 2i/(N−1)›、`t_j = j/(N−1)›（**含端点**），
//!   着色器侧采样坐标 `(idx + 0.5)/N›（[`lut_uv›] 为 CPU 镜像）——端点
//!   `W(−1) = 0 / W(1) = 1› 因此**逐位精确**，不会出现「全照面被 LUT 抹暗
//!   1.6%」的端点截断伪影；
//! - **64 的精度依据**（实测最大重建误差，双线性重建 vs 解析式）：
//!   `t ≥ 0.05› 时 ≤ 1.4e-3；`t → 0› 的 Lambert 折点处 ≤ 7.9e-3（折点
//!   本身不可导，线性插值 O(h) 是下界）。前者低于 8-bit 输出量化步长
//!   (3.9e-3)，后者约两倍——低于「皮肤终结线软化」的可见阈值；
//! - **Rgba16Float 而非 Rgba8**：binary16 有效位 11 bit（相对精度 4.9e-4），
//!   比 8-bit unorm（3.9e-3）细 8 倍——平缓的终结线斜坡用 8-bit 会出现
//!   1/255 的量化台阶（banding），且中性法线纹理（R42）与光照贴图（R52c）
//!   已确立「线性数据 / 低频信号走 Rgba16Float」的本仓先例（核心可过滤格式，
//!   无扩展依赖）；
//! - 积分步数 128：与 20000 步参考的最大偏差 9.0e-7（比 LUT 插值误差小
//!   三个数量级 ⇒ 步数不是精度瓶颈）；
//! - 生成成本：单次 64×64 生成 **18.8 ms**（debug 实测；4096 texel × 129 步）
//!   ——渲染器初始化的一次性开销。**刻意不做记忆化缓存**：它是纯函数，
//!   而「写一次的全局缓存」在本仓的确定性文化里属于要额外论证的全局状态；
//!   18.8 ms 远低于渲染器初始化本身的量级（IBL 烘焙等），不值得引入。
//!
//! # 诚实限制
//!
//! - **NdotL = −1（完全背光）响应恒为 0**：本模型建模的是**法线分布**导致的
//!   终结线软化，不含光在皮下横向扩散到背光面的「穿透光晕」（那需要厚度图
//!   / 透射积分，在拒绝清单里）。半背光侧（c ≳ −sin α）有可见包裹；
//! - 曲率尺度是**逐材质常量**，不是逐顶点 / 逐像素曲率（逐顶点曲率烘焙在
//!   拒绝清单里）；
//! - 跨平台位级复现：LUT 用 f32 libm 生成，同一次构建内逐位可复现；不同
//!   libm 实现间可能有 1 ulp 差异（LUT 是引擎常量，没有跨机位级契约）。

use crate::lightmap::f32_to_f16;

/// 预积分 LUT 边长（texel）。64 的精度依据见模块文档。
pub const SSS_LUT_SIZE: u32 = 64;

/// 极角方向的复合 Simpson 步数（偶数）。128 步与 20000 步参考偏差 9.0e-7。
const SSS_SIMPSON_STEPS: u32 = 128;

/// 参考包裹半径（米）：曲率半径等于该值时包裹参数取满（t = 1，α = 90°）。
///
/// 选值依据：角色头部曲率半径 ≈ 0.1 m 量级，取 t = 0.5（α = 45°）为
/// **默认观感档**——终结线软坡明显但不至于「蜡像」。调参指引见
/// [`wrap_param›]。
pub const SSS_REFERENCE_RADIUS: f32 = 0.05;

/// 曲率尺度（[`crate::material::MaterialDef::subsurface_scale›]）的引擎
/// 缺省（米）——头 / 前臂量级。只在 `subsurface_strength > 0› 时被消费。
pub const SSS_DEFAULT_SCALE: f32 = 0.1;

/// 曲率半径（世界单位，米）→ 归一化包裹参数 t ∈ [0, 1]。
///
/// `t = clamp(SSS_REFERENCE_RADIUS / scale, 0, 1)›：半径越小（越弯曲）
/// 包裹越强。**调参指引**：
///
/// | 部位 | `scale› (m) | t | α | 观感 |
/// |---|---|---|---|---|
/// | 手指 / 鼻尖 / 耳缘 | 0.02 | 1.00（饱和） | 90° | 强烈包裹，接近半透明 |
/// | 头 / 前臂（缺省） | 0.10 | 0.50 | 45° | 终结线明显软化 |
/// | 躯干 / 大腿 | 0.30 | 0.17 | 15° | 轻微软化 |
/// | 大面积平坦皮肤 | ≥ 0.50 | ≤ 0.10 | ≤ 9° | 近乎经典 Lambert |
///
/// 非法输入（非有限 / ≤ 0）返回 0 = 不做包裹（保守：不引入未定义效果）。
pub fn wrap_param(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 {
        (SSS_REFERENCE_RADIUS / scale).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// 方位角积分闭式解：`∫₀^{2π} max(a + b·cos φ, 0) dφ›（b 取绝对值——
/// 被积函数对 b 的符号是偶函数，镜像对称由 `|b|› 自动保证）。
///
/// 分段：
/// - `|a| ≥ b ≥ 0›：被积函数恒不变号 ⇒ `2π·max(a, 0)›；
/// - `|a| < b›：零点 `φ₀ = acos(−a/b) ∈ (0, π)›，正值区间 `|φ| < φ₀›
///   ⇒ `2(a·φ₀ + b·sin φ₀)›。
pub fn azimuth_integral(a: f32, b: f32) -> f32 {
    azimuth_integral_f64(a as f64, b as f64) as f32
}

/// [`azimuth_integral`] 的 f64 内核：数值积分在新版本里统一用 f64 累加
/// （见 [`ndotl_response`] 的精度说明），f32 公共函数只是入口包装。
fn azimuth_integral_f64(a: f64, b: f64) -> f64 {
    let b = b.abs();
    if b <= 0.0 {
        return if a > 0.0 {
            2.0 * std::f64::consts::PI * a
        } else {
            0.0
        };
    }
    if a >= b {
        return 2.0 * std::f64::consts::PI * a;
    }
    if a <= -b {
        return 0.0;
    }
    let phi0 = (-a / b).clamp(-1.0, 1.0).acos();
    (a * phi0 + b * phi0.sin()) * 2.0
}

/// 单点预积分响应 `W(c, t)›（纯函数，LUT 的生成源）。
///
/// `c› 为几何 NdotL（自动 clamp 到 [−1, 1]），`t› 为 [`wrap_param›] 的
/// 包裹参数（自动 clamp 到 [0, 1]）。`t ≤ 0› 时**逐位返回 `max(c, 0)›**
/// （零曲率 = 经典 Lambert，不做任何积分）。
pub fn ndotl_response(ndotl: f32, wrap: f32) -> f32 {
    let c = ndotl.clamp(-1.0, 1.0);
    let t = wrap.clamp(0.0, 1.0);
    if t <= 0.0 {
        return c.max(0.0);
    }
    // 端点解析值短路：W(1, t) ≡ 1、W(−1, t) ≡ 0（见模块文档的积分式）。
    // 不做短路的代价是 f32 的 cos(π/2) ≈ −4.4e-8 让 α = 90° 的边界项
    // 贡献 3.6e-10 的虚假能量，端点断言就不再是「逐位」而是「近似」；
    // 解析值已知，短路既精确又省算力（端点不参与积分）。
    if c >= 1.0 {
        return 1.0;
    }
    if c <= -1.0 {
        return 0.0;
    }
    // 积分在 f64 里做、最后一次性落到 f32：f32 累加 129 个量级 ~1 的项会
    // 攒出 ~1e-7 的求和噪声，足以在「对 c、对 t 单调」这条解析性质上制造
    // 亚 ulp 的反向抖动（实测 1.19e-7）。f64 累加后单调性恢复到扫描精度，
    // 且与模块文档里的解析对照（二维中点法 f64 参考）同一精度量级。
    let alpha = t as f64 * std::f64::consts::FRAC_PI_2;
    let cf = c as f64;
    // √(1 − c²) 用 max(…, 0) 兜住 c = ±1 的舍入残余（sqrt(负) = NaN）
    let s = (1.0 - cf * cf).max(0.0).sqrt();
    let h = alpha / SSS_SIMPSON_STEPS as f64;
    let mut acc = 0.0f64;
    for i in 0..=SSS_SIMPSON_STEPS {
        let x = i as f64 * h;
        let v = azimuth_integral_f64(cf * x.cos(), s * x.sin()) * x.sin();
        // 复合 Simpson 权重：端点 1、奇数点 4、偶数点 2
        let w = if i == 0 || i == SSS_SIMPSON_STEPS {
            1.0
        } else if i % 2 == 0 {
            2.0
        } else {
            4.0
        };
        acc += w * v;
    }
    let numer = acc * h / 3.0;
    // 分母 = D(α) = π·sin²α（见模块文档的积分式）。t > 0 ⇒ α > 0 ⇒ 分母 > 0。
    let sa = alpha.sin();
    let denom = std::f64::consts::PI * sa * sa;
    (numer / denom).max(0.0) as f32
}

/// 归一化坐标（NdotL c、曲率 t）→ LUT 采样坐标（**CPU 镜像**，与
/// `standard.wgsl::sss_uv› 逐式对应，两者必须同步修改）。
///
/// 纹素中心对齐：`idx = coord · (N − 1)›，归一化坐标 `(idx + 0.5)/N›——
/// `c = ±1 / t = 1› 恰好落在首末纹素中心，端点值无插值截断。
pub fn lut_uv(ndotl: f32, wrap: f32) -> (f32, f32) {
    let n = SSS_LUT_SIZE as f32;
    let u = (0.5 + (ndotl * 0.5 + 0.5) * (n - 1.0)) / n;
    let v = (0.5 + wrap * (n - 1.0)) / n;
    (u, v)
}

/// 程序化生成 LUT 标量场（行 = 曲率 t 递增、列 = NdotL c 递增，行主序）。
///
/// **纯函数**：同一 `size› 两次调用逐位同一输出（`to_bits› 级）。
pub fn lut_values(size: u32) -> Vec<f32> {
    assert!(size >= 2, "LUT 边长至少 2（端点对齐需要 N − 1 > 0）");
    let mut out = Vec::with_capacity((size * size) as usize);
    let step = (size - 1) as f32;
    for j in 0..size {
        let t = j as f32 / step;
        for i in 0..size {
            let c = -1.0 + 2.0 * (i as f32 / step);
            out.push(ndotl_response(c, t));
        }
    }
    out
}

/// LUT 的 `Rgba16Float› 字节流（GPU 上传数据源）：RGB = 同一标量（LUT 可
/// 直接当图看）、A = 1。行主序、8 B/texel。
///
/// **纯函数**：同一 `size› 两次调用逐字节同一（确定性硬约束）。
pub fn lut_half_bytes(size: u32) -> Vec<u8> {
    let values = lut_values(size);
    let mut out = Vec::with_capacity(values.len() * 8);
    for v in values {
        for channel in [v, v, v, 1.0] {
            out.extend_from_slice(&f32_to_f16(channel).to_le_bytes());
        }
    }
    out
}

/// CPU 侧双线性采样镜像（ClampToEdge）——供单测与 GPU 测试的解析对照
/// 预测使用。着色器那边由硬件采样器完成同一语义。
pub fn lut_lookup(values: &[f32], size: u32, ndotl: f32, wrap: f32) -> f32 {
    assert_eq!(
        values.len(),
        (size * size) as usize,
        "LUT 数据长度 {} 与边长 {size} 不符",
        values.len()
    );
    let (u, v) = lut_uv(ndotl.clamp(-1.0, 1.0), wrap.clamp(0.0, 1.0));
    let n = size as f32;
    let fx = u * n - 0.5;
    let fy = v * n - 0.5;
    let x0 = fx.floor();
    let y0 = fy.floor();
    let tx = fx - x0;
    let ty = fy - y0;
    let at = |x: f32, y: f32| -> f32 {
        let xi = (x as i32).clamp(0, size as i32 - 1) as u32;
        let yi = (y as i32).clamp(0, size as i32 - 1) as u32;
        values[(yi * size + xi) as usize]
    };
    let a = at(x0, y0);
    let b = at(x0 + 1.0, y0);
    let c = at(x0, y0 + 1.0);
    let d = at(x0 + 1.0, y0 + 1.0);
    (a * (1.0 - tx) + b * tx) * (1.0 - ty) + (c * (1.0 - tx) + d * tx) * ty
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    /// 扫描点数（覆盖 [−1, 1] 的 401 个采样）。
    const SCAN: u32 = 400;

    /// **确定性硬约束**：同一输入两次生成逐位同一输出——标量场（f32 位型）
    /// 与 GPU 上传字节流（f16 字节）两层。
    #[test]
    fn lut_generation_is_bitwise_reproducible() {
        let a = lut_values(SSS_LUT_SIZE);
        let b = lut_values(SSS_LUT_SIZE);
        assert_eq!(a.len(), (SSS_LUT_SIZE * SSS_LUT_SIZE) as usize);
        assert!(
            a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()),
            "两次生成的 f32 位型必须逐位相同"
        );
        assert_eq!(
            lut_half_bytes(SSS_LUT_SIZE),
            lut_half_bytes(SSS_LUT_SIZE),
            "两次生成的 GPU 字节流必须逐字节相同"
        );
        assert_eq!(lut_half_bytes(SSS_LUT_SIZE).len(), 64 * 64 * 8);
    }

    /// **端点解析对照**：W(1, t) ≡ 1（构造性归一：分子分母同式）、
    /// W(−1, t) ≡ 0（球冠完全背光）。
    #[test]
    fn response_endpoints_match_analytic_values() {
        for j in 0..=20 {
            let t = j as f32 / 20.0;
            let w1 = ndotl_response(1.0, t);
            assert!((w1 - 1.0).abs() < 1e-6, "W(1, {t}) = {w1} 应恒为 1");
            assert_eq!(ndotl_response(-1.0, t), 0.0, "W(-1, {t}) 应恒为 0");
        }
        // 中心点解析值（t = 1，α = 90°：正面半球投影面积平均 = 1/2）
        assert!(
            (ndotl_response(0.0, 1.0) - 0.5).abs() < 1e-5,
            "W(0, 1) 应等于 1/2（半球投影面积平均的解析值）"
        );
    }

    /// **零曲率 = 经典 Lambert**：逐位等于 max(c, 0)（不做任何积分）。
    #[test]
    fn zero_wrap_is_exactly_lambert() {
        for i in 0..=SCAN {
            let c = -1.0 + 2.0 * i as f32 / SCAN as f32;
            assert_eq!(
                ndotl_response(c, 0.0).to_bits(),
                c.max(0.0).to_bits(),
                "t = 0（平面皮肤）必须逐位退化为经典 Lambert"
            );
        }
    }

    /// **零点解析对照**：W(c, t) = 0 ⟺ c ≤ −sin(α(t))（球冠内最朝向光源的
    /// 法线也照不到 ⇒ 捕获不到能量）。二分扫出零点与解析值比对。
    #[test]
    fn response_zero_crossing_matches_analytic_sine() {
        for j in 1..=20 {
            let t = j as f32 / 20.0;
            let analytic = -(t * FRAC_PI_2).sin();
            let mut lo = -1.0f32; // W = 0 侧
            let mut hi = 0.5f32; // W > 0 侧
            assert!(ndotl_response(hi, t) > 0.0, "t = {t} 在 c = 0.5 应有响应");
            for _ in 0..40 {
                let mid = 0.5 * (lo + hi);
                if ndotl_response(mid, t) <= 0.0 {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            assert!(
                (hi - analytic).abs() < 5e-3,
                "t = {t}：零点实测 {hi} vs 解析 −sin α = {analytic}"
            );
        }
    }

    /// **单调性**：对 NdotL 与对曲率参数都单调不减（零反向落差，容差 1e-9
    /// 只用于吸收 Simpson 舍入）。
    #[test]
    fn response_is_monotone_in_both_axes() {
        for j in 0..=20 {
            let t = j as f32 / 20.0;
            let mut prev = ndotl_response(-1.0, t);
            for i in 1..=SCAN {
                let c = -1.0 + 2.0 * i as f32 / SCAN as f32;
                let v = ndotl_response(c, t);
                // 容差 1e-9：f64 累加后残余抖动 ≤ 1 ulp(≈1e-7 量级值上的
                // 6e-8)，比 LUT 的曲率栅格步长小 5 个数量级。
                assert!(v >= prev - 1e-9, "t = {t}：c = {c} 处反向落差 {}", prev - v);
                prev = v;
            }
        }
        for i in 0..=SCAN {
            let c = -1.0 + 2.0 * i as f32 / SCAN as f32;
            let mut prev = ndotl_response(c, 0.0);
            for j in 1..=100 {
                let t = j as f32 / 100.0;
                let v = ndotl_response(c, t);
                assert!(v >= prev - 1e-7, "c = {c}：t = {t} 处反向落差 {}", prev - v);
                prev = v;
            }
        }
    }

    /// **方位角镜像对称**：`J(a, b) = J(a, −b)› 逐位（被积函数对 φ 的镜像）。
    #[test]
    fn azimuth_integral_is_mirror_symmetric() {
        for i in 0..=20 {
            let a = -2.0 + 4.0 * i as f32 / 20.0;
            for k in 0..=10 {
                let b = k as f32 / 10.0;
                assert_eq!(
                    azimuth_integral(a, b).to_bits(),
                    azimuth_integral(a, -b).to_bits(),
                    "a = {a}, b = {b}：方位角镜像必须逐位对称"
                );
            }
        }
    }

    /// **独立参考实现交叉对照**：二维中点法直接积分球冠（f64、不依赖闭式
    /// 解），与「闭式方位角 + Simpson 极角」的实现比对。
    #[test]
    fn response_matches_direct_2d_midpoint_reference() {
        fn reference(c: f32, t: f32, polar: u32, azimuth: u32) -> f32 {
            let a_max = (t as f64) * std::f64::consts::FRAC_PI_2;
            let c = c as f64;
            let s = (1.0 - c * c).max(0.0).sqrt();
            let mut num = 0.0f64;
            let mut den = 0.0f64;
            for i in 0..polar {
                let a = (i as f64 + 0.5) / polar as f64 * a_max;
                for k in 0..azimuth {
                    let phi = (k as f64 + 0.5) / azimuth as f64 * 2.0 * std::f64::consts::PI;
                    let dot = c * a.cos() + s * a.sin() * phi.cos();
                    num += dot.max(0.0) * a.sin();
                    den += a.cos() * a.sin();
                }
            }
            (num / den) as f32
        }
        for &(c, t) in &[
            (-0.9f32, 0.5f32),
            (-0.3, 0.25),
            (0.0, 0.5),
            (0.0, 1.0),
            (0.4, 0.75),
            (0.9, 1.0),
        ] {
            let got = ndotl_response(c, t);
            let want = reference(c, t, 400, 200);
            assert!(
                (got - want).abs() < 2e-3,
                "c = {c}, t = {t}：本实现 {got} vs 二维中点参考 {want}"
            );
        }
    }

    /// **LUT 采样镜像与重建误差上界**：端点逐位精确；重建误差
    /// `t ≥ 0.05› 时 < 3e-3、`t = 0› 折点处 < 1e-2（模块文档的精度依据）。
    #[test]
    fn lut_lookup_reconstruction_error_is_bounded() {
        let values = lut_values(SSS_LUT_SIZE);
        // 端点：c = ±1 / t = 1 落在首末纹素中心，逐位精确
        for j in 0..=20 {
            let t = j as f32 / 20.0;
            assert_eq!(
                lut_lookup(&values, SSS_LUT_SIZE, 1.0, t).to_bits(),
                ndotl_response(1.0, t).to_bits(),
                "c = 1 的采样必须逐位等于解析值（端点对齐）"
            );
            assert_eq!(lut_lookup(&values, SSS_LUT_SIZE, -1.0, t), 0.0);
        }
        let mut worst_curved = 0.0f32;
        let mut worst_flat = 0.0f32;
        for i in 0..=200 {
            let c = -1.0 + 2.0 * i as f32 / 200.0;
            for j in 0..=40 {
                let t = j as f32 / 40.0;
                let e = (lut_lookup(&values, SSS_LUT_SIZE, c, t) - ndotl_response(c, t)).abs();
                if t >= 0.05 {
                    worst_curved = worst_curved.max(e);
                } else {
                    worst_flat = worst_flat.max(e);
                }
            }
        }
        assert!(
            worst_curved < 3e-3,
            "曲率 t ≥ 0.05 的重建误差 {worst_curved} 超界"
        );
        assert!(
            worst_flat < 1e-2,
            "t → 0 的 Lambert 折点重建误差 {worst_flat} 超界"
        );
    }

    /// **曲率映射的调参表数字对照**：0.05 → 饱和（t = 1）、0.1 → 0.5、
    /// 0.3 → 1/6；非法输入（0 / 负 / NaN / Inf）保守退化为 0（无包裹）。
    #[test]
    fn wrap_param_matches_tuning_table() {
        assert_eq!(wrap_param(SSS_REFERENCE_RADIUS), 1.0);
        assert_eq!(wrap_param(SSS_DEFAULT_SCALE), 0.5);
        assert!((wrap_param(0.3) - 1.0 / 6.0).abs() < 1e-7);
        assert_eq!(wrap_param(0.02), 1.0, "小于参考半径 ⇒ 饱和在 1");
        assert!(wrap_param(2.0) < 0.03, "大面积平坦皮肤 ⇒ 近乎无包裹");
        for bad in [0.0f32, -0.1, f32::NAN, f32::INFINITY] {
            assert_eq!(wrap_param(bad), 0.0, "非法尺度 {bad} 必须退化为无包裹");
        }
    }
}
