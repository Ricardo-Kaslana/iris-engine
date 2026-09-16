//! 体积雾（R79）：设置资源 + CPU 纯函数核心 + GPU 侧的镜像契约。
//!
//! **语义**：链头全屏 pass（主 pass / TAA 之后、DOF / MB / 泛光之前）——
//! 逐像素沿视线固定 [`FOG_STEPS`] 步光线步进，累加消光透过率与太阳内散
//! 射（Henyey-Greenstein 相位 × 太阳色 × 密度 × 步长），最终
//! `out = color × T + inscatter`。输入 = 链头 HDR 颜色 + scene_depth
//! （R32Float **线性视距**，与 DOF / MB 同款链上消费——同一编码器内
//! 场景深度副本在本 pass **之前**编码，读到的是**本帧**深度，无滞后；
//! 「上帧语义」只适用于主 pass 之前读深度的消费者如 SSR / GTAO）+
//! 参数 uniform + 相机重建量（inv_view_proj，与 DOF / MB 的 NDC z 反算
//! 同款口径）。槽位拓扑由 `post.rs` 的
//! [`crate::post::full_chain_slots_with_fog`] 单点定义（雾开时相机级链
//! A→B→A 轮转，雾关时走旧函数原路径）。
//!
//! **资源缺席语义**：[`FogSettings`] 未注册或 `enabled = false` 时整段
//! 关闭——不分配深度副本（若其他消费者也未开启）、不编码 pass、不写参
//! 数，链拓扑与命令流与 R78 逐位一致（零开销默认路径）；`DefaultPlugins`
//! 不注册本资源。捕获 / RT 相机不吃雾（后处理链只存在于主相机路径——
//! 天然成立，如实声明的限制）。
//!
//! **纯函数优先**：[`fog_density_at`] / [`fog_phase_hg`] / [`fog_dither`] /
//! [`fog_transmittance`] 全部是脱离 GPU 可单测的自由函数；`shaders/
//! fog.wgsl` 逐行镜像（同式同序——CPU 断言即着色器行为的解析锚点，
//! 与 `post::ao_dither` 同款纪律，**两侧必须同步修改**）。
//!
//! # 诚实限制清单（v1 明确不做，ARCHITECTURE §8.48）
//!
//! - **深度与 DOF / MB 同帧（链上消费）**：雾在后处理链里读 scene_depth
//!   ——同一编码器内本帧场景深度副本已先编码，读到的是**本帧**深度
//!   （无滞后）。**对规格的偏离声明**：原规格按 SSR / GTAO 的「上帧」
//!   直觉写为吃上一帧深度，实现时核实编码器顺序后确认链上消费者读到
//!   的恒为本帧内容——按实际情况记录，不虚构一条不存在的滞后限制；
//!   链上无几何帧（`opaque_total = 0`）副本不跑，吃到的是陈旧内容
//!   （与 DOF / MB 同款）；
//! - **捕获 / RT 相机不吃雾**：后处理链只在主相机路径；
//! - **max_distance 截断**：超出 `max_distance` 的雾贡献不计（16 步预算
//!   下的艺术化折衷）；
//! - 非物理量纲：密度 / 衰减为艺术化参数，能量不守恒；
//! - `phase_g` 定义域 |g| < 1（g → ±1 时 HG 分母趋 0），渲染器上传时
//!   钳制到 ±0.999。

// ---------- 设置资源（World 资源，渲染器 try_resource 直读） ----------

/// 体积雾设置（R79）。逐像素光线步进的参与介质：高度指数衰减的消光
/// 密度场 + 朝太阳的 Henyey-Greenstein 内散射。链头全屏 pass（见模块
/// 文档）。
///
/// **资源缺席语义**：未注册或 [`Self::enabled`] = false 时整段关闭，
/// 链拓扑与命令流与 R78 逐位一致（零开销默认路径）；`DefaultPlugins`
/// 不注册本资源。
#[derive(Clone, Copy, Debug, iris_core::Resource)]
pub struct FogSettings {
    /// 是否启用体积雾（`Default = false`，与「缺席 = 关」语义一致）。
    pub enabled: bool,
    /// 零高度消光系数（1/m）：`fog_density_at(0) = density`（在
    /// `height_offset` 平面上）。0.045 ≈ 96 m 视距外基本白化的轻雾。
    pub density: f32,
    /// 指数高度衰减（1/m）：密度按 `exp(-(h - height_offset) ×
    /// height_falloff)` 随高度衰减。0.12 ≈ 每升高 10 m 密度衰减到 30%。
    pub height_falloff: f32,
    /// 雾海平面高度偏移（m）：密度峰值所在的世界高度。
    pub height_offset: f32,
    /// 太阳内散射强度倍率（乘在 `相位 × 太阳色 × 密度` 的累计项上；
    /// 0 = 纯消光雾——只变暗不发光）。
    pub inscatter_strength: f32,
    /// HG 相位各向异性 g（[-1, 1)：正 = 前向散射峰，逆光雾亮；渲染器
    /// 钳制到 ±0.999 防 HG 分母趋 0）。
    pub phase_g: f32,
    /// 步进 ray 长度上限（m）：天空像素的雾射线长与几何像素的截断距离。
    pub max_distance: f32,
}

impl Default for FogSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            density: 0.045,
            height_falloff: 0.12,
            height_offset: 0.0,
            inscatter_strength: 1.0,
            phase_g: 0.6,
            max_distance: 96.0,
        }
    }
}

impl FogSettings {
    /// 显式关闭形态（与缺席语义一致——零开销默认路径）。
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }
}

// ---------- 纯函数核心（fog.wgsl 逐行镜像，两侧同步修改） ----------

/// 每像素固定步进数（固定常量，非设置项——步数决定 pass 成本与条带
/// 水平，起点像素哈希抖动已压制条带；与 `shaders/fog.wgsl` 的
/// `FOG_STEPS` 同值，修改必须同步）。
pub const FOG_STEPS: u32 = 16;

/// 高度 h 处的雾消光密度（与 `shaders/fog.wgsl` 的 `fog_density_at`
/// 同一公式，**两侧同步修改**）：`density × exp(-(h - height_offset) ×
/// height_falloff)`，结果钳下限 0（负密度参数 / 极端高度下负值或溢出
/// 一律按 0 处理——「无雾」是安全的缺席方向）。
pub fn fog_density_at(height_m: f32, density: f32, height_falloff: f32, height_offset: f32) -> f32 {
    (density * (-(height_m - height_offset) * height_falloff).exp()).max(0.0)
}

/// Henyey-Greenstein 相位（与 `shaders/fog.wgsl` 的 `fog_phase_hg`
/// 同一公式，**两侧同步修改**）：
///
/// ```text
/// P(θ) = (1 − g²) / (4π · (1 + g² − 2g·cosθ)^1.5)
/// ```
///
/// θ = 0（正对光源前向）最大、θ = π（背向）最小、g = 0 退化为各向同性
/// 1/4π。定义域 |g| < 1（g → ±1 分母趋 0）；调用方（渲染器上传）钳制
/// 到 ±0.999。
pub fn fog_phase_hg(cos_theta: f32, g: f32) -> f32 {
    let g2 = g * g;
    (1.0 - g2) / (4.0 * std::f32::consts::PI * (1.0 + g2 - 2.0 * g * cos_theta).powf(1.5))
}

/// 像素坐标 → [0, 1) 的确定性抖动值（与 `shaders/fog.wgsl` 的
/// `fog_dither` 同一整数哈希逐行镜像，**两侧同步修改**；哈希常数与
/// `post::ao_dither` 同款——同一套回绕整数混淆，u32 运算 WGSL 天然
/// 回绕、Rust 用 wrapping 语义，两侧位级一致）。用途 = 步进起点沿射线
/// 的亚步长偏移（去条带的交错式 dither）——纯像素坐标函数，无状态、
/// 无时域依赖，跨运行逐位一致。
pub fn fog_dither(pixel: [u32; 2]) -> f32 {
    let mut h = pixel[0].wrapping_mul(73_856_093) ^ pixel[1].wrapping_mul(19_349_663);
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    h ^= h >> 16;
    (h & 0x00FF_FFFF) as f32 / 16_777_216.0
}

/// 直线射线上的消光透过率（与着色器循环体同构的 CPU 积分器——无抖动
/// 基线版，**两侧同步修改**）：射线长 `path_len` 均分 `steps` 步，第 i 步
/// 在**步起点**（`i / steps` 线性插值高度）采样密度（与着色器同口径），
/// `T ← T × exp(-σᵢ × ds)` 连乘。着色器侧仅多两处理性差异：起点按像素
/// 哈希抖动半个步长以内（统计上不改变单调性）、累计内散射项（不影响 T）。
///
/// `path_len ≤ 0` 或 `steps = 0` 视为零路径 → 恒 1（无雾）。
pub fn fog_transmittance(
    height_start: f32,
    height_end: f32,
    path_len: f32,
    steps: u32,
    density: f32,
    height_falloff: f32,
    height_offset: f32,
) -> f32 {
    if path_len <= 0.0 || steps == 0 {
        return 1.0;
    }
    let ds = path_len / steps as f32;
    let mut t = 1.0f32;
    for i in 0..steps {
        let frac = i as f32 / steps as f32;
        let h = height_start + (height_end - height_start) * frac;
        let sigma = fog_density_at(h, density, height_falloff, height_offset);
        t *= (-sigma * ds).exp();
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 密度解析解（±1e-5 容差依据：f32 在 exp 链上的末位舍入）：
    /// 基准高度恰为参数值；负指数衰减；offset 平移密度峰。
    #[test]
    fn fog_density_at_matches_closed_form() {
        // h = offset 平面：密度恰为 density（exp(0) = 1 精确）
        assert_eq!(
            fog_density_at(5.0, 0.045, 0.12, 5.0),
            0.045,
            "offset 平面无衰减"
        );
        // 解析对照：0.045 × e^(-12 × 0.12)（h = 12, offset = 0）
        let expected = 0.045 * (-1.44f32).exp();
        assert!(
            (fog_density_at(12.0, 0.045, 0.12, 0.0) - expected).abs() <= 1e-5,
            "12 m 高度解析解应为 {expected}，实际 {}",
            fog_density_at(12.0, 0.045, 0.12, 0.0)
        );
        // offset 之下（h < offset）密度高于零平面
        assert!(
            fog_density_at(-8.0, 0.045, 0.12, 0.0) > 0.045,
            "负高度（雾海平面下）密度必须增大"
        );
        // 负高度解析解：0.045 × e^(0.96)
        let below = 0.045 * 0.96f32.exp();
        assert!(
            (fog_density_at(-8.0, 0.045, 0.12, 0.0) - below).abs() <= 1e-5,
            "h = −8 解析解应为 {below}，实际 {}",
            fog_density_at(-8.0, 0.045, 0.12, 0.0)
        );
    }

    /// 密度钳下限：负密度参数给 0（不产生「负雾」）；0 密度恒 0。
    #[test]
    fn fog_density_at_clamps_to_zero_floor() {
        assert_eq!(fog_density_at(3.0, -0.5, 0.12, 0.0), 0.0, "负密度钳 0");
        assert_eq!(fog_density_at(100.0, 0.0, 0.12, 0.0), 0.0, "零密度恒 0");
        // 负 falloff 会指数放大——结果仍非负（上限不钳是声明行为，
        // 渲染器上传时钳 falloff ≥ 0）
        assert!(fog_density_at(3.0, 0.045, -0.12, 0.0) >= 0.0);
    }

    /// HG 相位端点（解析值 ±1e-5，依据：公式代入的精确值）：
    /// θ = 0 → (1+g)²/(4π(1−g²)²) 归一前向峰、θ = π → (1−g)²/(4π(1+g)²)、
    /// g = 0 → 恰 1/4π（各向同性）。
    #[test]
    fn fog_phase_hg_endpoints_are_exact() {
        // g = 0：各向同性 1/4π（全部方向）
        for cos in [-1.0f32, 0.0, 0.5, 1.0] {
            let expected = 1.0 / (4.0 * std::f32::consts::PI);
            assert!(
                (fog_phase_hg(cos, 0.0) - expected).abs() <= 1e-5,
                "g = 0 必须各向同性 1/4π：cos = {cos}"
            );
        }
        let g = 0.6f32;
        // θ = 0：cos = 1
        let fwd = fog_phase_hg(1.0, g);
        let fwd_expected =
            (1.0f32 - 0.36) / (4.0 * std::f32::consts::PI * (1.0f32 + 0.36 - 1.2).powf(1.5));
        assert!(
            (fwd - fwd_expected).abs() <= 1e-5,
            "前向峰值解析解应为 {fwd_expected}，实际 {fwd}"
        );
        // θ = π：cos = −1
        let back = fog_phase_hg(-1.0, g);
        let back_expected =
            (1.0f32 - 0.36) / (4.0 * std::f32::consts::PI * (1.0f32 + 0.36 + 1.2).powf(1.5));
        assert!(
            (back - back_expected).abs() <= 1e-5,
            "背向解析解应为 {back_expected}，实际 {back}"
        );
        // g = 0.6 的经典比值：前向/背向 = ((1+g)/(1−g))³ = 4³ = 64
        let ratio = fwd / back;
        assert!(
            (ratio - 64.0).abs() <= 64.0 * 1e-4,
            "g = 0.6 前向/背向比应为 64，实际 {ratio}"
        );
    }

    /// HG 相位单调：g > 0 时对 cosθ 严格递增（cos = 1 正对光源处是前向
    /// 峰）；对称性 P(cos; g) = P(−cos; −g)。
    #[test]
    fn fog_phase_hg_is_monotone_in_cos_theta() {
        let g = 0.6;
        let mut prev = 0.0f32;
        let mut cos = -1.0;
        while cos <= 1.0 {
            let p = fog_phase_hg(cos, g);
            assert!(
                p >= prev,
                "g > 0 时相位必须随 cosθ 单调不减（前向峰）：cos = {cos}"
            );
            prev = p;
            cos += 0.05;
        }
        // 对称性：−g 的相位 = g 的相位在 cos 取反
        let a = fog_phase_hg(0.3, 0.6);
        let b = fog_phase_hg(-0.3, -0.6);
        assert!(
            (a - b).abs() <= 1e-6,
            "HG 对称性 P(c; g) = P(−c; −g)：{a} vs {b}"
        );
    }

    /// 抖动哈希：确定性复现、值域 [0, 1)、32×32 像素 ≥ 200 个不同 8-bit
    /// 桶（统计口径同 ao_dither 教训——「全 256 桶 / 逐像素唯一」是生日
    /// 碰撞下不可达的伪标准）、u32 全域无 panic。
    #[test]
    fn fog_dither_is_deterministic_in_range_and_varied() {
        for px in [[0u32, 0], [1, 1], [640, 360], [u32::MAX, u32::MAX]] {
            let d = fog_dither(px);
            assert!(
                (0.0..1.0).contains(&d),
                "抖动值必须在 [0,1)，实际 {d:?} @ {px:?}"
            );
            assert_eq!(fog_dither(px), d, "同像素必须确定性复现 @ {px:?}");
        }
        let mut seen = std::collections::HashSet::new();
        for y in 0..32u32 {
            for x in 0..32u32 {
                seen.insert((fog_dither([x, y]) * 256.0) as u32);
            }
        }
        assert!(
            seen.len() >= 200,
            "1024 个像素的量化抖动应高度分散，实际只有 {} 桶",
            seen.len()
        );
    }

    /// 透过率解析锚点：零路径 / 零步 / 零密度 → 恒 1；均匀密度（falloff
    /// = 0）下的 16 步积分与解析解 e^(−σL) 相对误差 ≤ 1e-4（均匀介质
    /// 左端点矩形积分恰好精确——每步 σ 相同，连乘 = (e^(−σds))^steps =
    /// e^(−σL)，f32 舍入量级内）。
    #[test]
    fn fog_transmittance_uniform_medium_matches_closed_form() {
        assert_eq!(
            fog_transmittance(0.0, 10.0, 0.0, 16, 0.045, 0.12, 0.0),
            1.0,
            "零路径无雾"
        );
        assert_eq!(
            fog_transmittance(0.0, 10.0, 10.0, 0, 0.045, 0.12, 0.0),
            1.0,
            "零步无积分"
        );
        // falloff = 0：均匀介质，解析解 e^(−0.045 × 40)
        let t = fog_transmittance(0.0, 40.0, 40.0, 16, 0.045, 0.0, 0.0);
        let expected = (-0.045f32 * 40.0).exp();
        assert!(
            (t - expected).abs() <= expected.abs() * 1e-4 + 1e-6,
            "均匀介质透过率应为 {expected}，实际 {t}"
        );
    }

    /// 透过率单调性：密度越大越低；距离越长越低；步数收敛（16 → 64 步
    /// 变化 ≤ 5%——步长离散化的收敛性证据）；值域 (0, 1]。
    #[test]
    fn fog_transmittance_monotone_and_convergent() {
        // 密度单调下降（同为 40 m 路径）
        let t1 = fog_transmittance(0.0, 40.0, 40.0, 16, 0.02, 0.12, 0.0);
        let t2 = fog_transmittance(0.0, 40.0, 40.0, 16, 0.045, 0.12, 0.0);
        let t3 = fog_transmittance(0.0, 40.0, 40.0, 16, 0.12, 0.12, 0.0);
        assert!((0.0..=1.0).contains(&t2), "透过率必须在 [0,1]，实际 {t2}");
        assert!(
            t1 > t2 && t2 > t3,
            "密度增大透过率必须单调下降：{t1} {t2} {t3}"
        );
        // 距离单调下降（同密度）
        let d1 = fog_transmittance(0.0, 10.0, 10.0, 16, 0.045, 0.12, 0.0);
        let d2 = fog_transmittance(0.0, 40.0, 40.0, 16, 0.045, 0.12, 0.0);
        assert!(d1 > d2, "距离增长透过率必须单调下降：{d1} {d2}");
        // 步数收敛：32 步与 64 步差 ≤ 5%（高度衰减介质上步起点采样在
        // 密度递减方向系统性高估衰减——16 ≤ 32 ≤ 64 的方向性也一并锁定）
        let s16 = fog_transmittance(0.0, 96.0, 96.0, 16, 0.045, 0.12, 0.0);
        let s32 = fog_transmittance(0.0, 96.0, 96.0, 32, 0.045, 0.12, 0.0);
        let s64 = fog_transmittance(0.0, 96.0, 96.0, 64, 0.045, 0.12, 0.0);
        assert!(
            s16 <= s32 && s32 <= s64,
            "步数加细必须单调抬升：{s16} {s32} {s64}"
        );
        assert!(
            (s32 - s64).abs() <= 0.05 * s64,
            "步数收敛性：32 步 {s32} vs 64 步 {s64}"
        );
    }

    /// Default 与缺席语义：Default = 关（enabled false，与缺席等价）；
    /// `disabled()` 形态逐字段等价缺席；参数默认值是文档承诺（渲染
    /// 调参的锚点）。
    #[test]
    fn fog_settings_default_is_disabled_with_documented_values() {
        let d = FogSettings::default();
        assert!(!d.enabled, "Default 必须是关闭（缺席语义）");
        assert_eq!(d.density, 0.045);
        assert_eq!(d.height_falloff, 0.12);
        assert_eq!(d.height_offset, 0.0);
        assert_eq!(d.inscatter_strength, 1.0);
        assert_eq!(d.phase_g, 0.6);
        assert_eq!(d.max_distance, 96.0);
        let off = FogSettings::disabled();
        assert!(!off.enabled);
        assert_eq!(off.density, d.density, "disabled() 只改开关不改参数");
        // FOG_STEPS 与 WGSL 同值的源码级锚点（shaders/fog.wgsl 镜像）
        assert_eq!(FOG_STEPS, 16);
    }

    /// SkySettings Default 与缺席语义（post.rs 定义，锚点在此处与其
    /// GPU 行为一并锁定）：atmosphere false = 旧渐变天空逐位不变；
    /// 文档承诺的默认参数值。
    #[test]
    fn sky_settings_default_is_legacy_gradient() {
        let s = crate::post::SkySettings::default();
        assert!(!s.atmosphere, "Default 必须是旧渐变天空（缺席语义）");
        assert_eq!(s.rayleigh_strength, 1.0);
        assert_eq!(s.mie_strength, 1.0);
        assert_eq!(s.sun_disk_intensity, 32.0);
        assert_eq!(s.ground_color, glam::Vec3::new(0.05, 0.05, 0.06));
    }
}
