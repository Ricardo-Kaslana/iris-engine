//! Kajiya-Kay 各向异性毛发着色的 CPU 镜像（R90）。
//!
//! WGSL 侧本体在 `standard.wgsl::kk_light` / `kk_point_contrib`（直射光
//! 贡献瓣）；本模块提供**逐式同序**的 Rust 镜像 [`kk_lobe`] 供解析断言
//! 单测——GPU 侧无法在 CPU 上执行，位级等价靠 WGSL 源码层看门
//! （[`wgsl_kk_formula_matches_mirror`]）+ GPU 方向性断言
//! （`tests/fur_render.rs`）双轨钉住，同 `sss.rs` / `oit.rs` 先例。
//!
//! # v1 模型边界（诚实清单）
//!
//! - 单级高光带（无 primary/secondary 双带，Marschner 简化不做）；
//! - 漫反射 = `max(0, sin(T·L))` KK 1989 原式，无 Scheuermann 根梢
//!   混合（背光侧发丝偏暗）；
//! - 高光颜色 = `mix(白, albedo, 0.5)` 固定式，无独立强度 / 颜色旋钮；
//! - 间接项（IBL / 探针 / 光照贴图 / DDGI）维持各向同性伪法线路径
//!   （伪法线 = 发带 VS 预计算的视图投影法线，走 world_normal 通道）；
//! - `sin(θ)` 一律 `sqrt(1 − cos²)` 计算（libm 的 asin/acos 无位级
//!   保证——仓库确定性文化，同 `sqrt` 优于 `hypot` 条目）。

/// 移位切线：`T' = normalize(T + N · shift · (2t − 1))`。
///
/// `t` 为沿发丝参数（根 0 → 梢 1，发带 UV.x）。偏移量沿发丝线性渐变
/// ⇒ 高光带从根到梢倾斜（发丝高光"移位"质感的来源）。`t_strand` /
/// `n_pseudo` 必须已归一（调用方保证，与 WGSL 同前提）。
pub fn shifted_tangent(
    t_strand: [f32; 3],
    n_pseudo: [f32; 3],
    shift: f32,
    t_along: f32,
) -> [f32; 3] {
    let offset = shift * (2.0 * t_along - 1.0);
    let mut out = [
        t_strand[0] + n_pseudo[0] * offset,
        t_strand[1] + n_pseudo[1] * offset,
        t_strand[2] + n_pseudo[2] * offset,
    ];
    let len = (out[0] * out[0] + out[1] * out[1] + out[2] * out[2]).sqrt();
    // 与 WGSL normalize 同语义：零向量返回零（shift = 0 且 T 归一时
    // 不可能为零；防御性除以 max(len, ε) 由 WGSL normalize 内部语义承担）
    for v in out.iter_mut() {
        *v /= len;
    }
    out
}

/// KK 瓣的标量分解：`(漫反射系数, 高光系数)` ∈ [0, 1]²。
///
/// 出射辐照 = `(albedo · diffuse + spec_color · specular) · radiance`，
/// 其中 `spec_color = mix(白, albedo, 0.5)`——颜色合成式不进本函数，
/// 两个标量瓣各自可解析断言（R89「指标要可判别」）。`t_strand` /
/// `n_pseudo` / `l` / `v` 必须已归一；`h` 由调用方传入或内部以
/// `normalize(v + l)` 合成（与 WGSL `kk_light` 内部一致——镜像内部合成）。
pub fn kk_lobe(
    t_strand: [f32; 3],
    n_pseudo: [f32; 3],
    t_along: f32,
    v: [f32; 3],
    l: [f32; 3],
    shift: f32,
    exponent: f32,
) -> (f32, f32) {
    let t_shift = shifted_tangent(t_strand, n_pseudo, shift, t_along);
    // 漫反射 = sin(T·L)
    let ctl = t_strand[0] * l[0] + t_strand[1] * l[1] + t_strand[2] * l[2];
    let diffuse = (1.0 - ctl * ctl).max(0.0).sqrt();
    // 高光 = sin(T'·H)^e，H = normalize(v + l)
    let mut h = [v[0] + l[0], v[1] + l[1], v[2] + l[2]];
    let hl = (h[0] * h[0] + h[1] * h[1] + h[2] * h[2]).sqrt();
    for hv in h.iter_mut() {
        *hv /= hl;
    }
    let cth = t_shift[0] * h[0] + t_shift[1] * h[1] + t_shift[2] * h[2];
    let sin_th = (1.0 - cth * cth).max(0.0).sqrt();
    let specular = sin_th.powf(exponent);
    (diffuse, specular)
}

/// WGSL 源码层看门：`standard.wgsl` 的 `kk_light` 关键算式必须与镜像
/// 逐式同序（照 `irradiance.rs::wgsl_sh_constants_match_cpu_table` /
/// `pipeline.rs::skybox_globals_prefix_matches_standard_layout` 先例）。
/// 改公式漏同步时此测试挂——把「两侧一致」从纪律变成断言。
#[cfg(test)]
fn wgsl_kk_source() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/shaders/standard.wgsl");
    std::fs::read_to_string(path).expect("standard.wgsl 必须可读（源码层看门）")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// sqrt 形式的 sin 瓣：切线与光源垂直（cos = 0）时漫反射峰值 1；
    /// 平行（cos = ±1）时为 0——KK 漫反射沿发丝各向异性的两个解析锚。
    #[test]
    fn diffuse_sin_anchors() {
        let n = [0.0, 0.0, 1.0];
        let v = [0.0, 0.0, 1.0];
        let t = [1.0, 0.0, 0.0];
        // L ⊥ T：sin = 1
        let (d_perp, _) = kk_lobe(t, n, 0.5, v, [0.0, 0.0, 1.0], 0.0, 32.0);
        assert!(
            (d_perp - 1.0).abs() < 1e-6,
            "垂直光源漫反射应峰值 1，得 {d_perp}"
        );
        // L ∥ T：sin = 0
        let (d_par, _) = kk_lobe(t, n, 0.5, v, [1.0, 0.0, 0.0], 0.0, 32.0);
        assert!(d_par.abs() < 1e-6, "平行光源漫反射应为 0，得 {d_par}");
    }

    /// 高光移位：shift > 0 且沿丝参数从根到梢变化时，高光峰值对应的
    /// 光源方向随之偏移——移位切线的方向性锚。v 固定沿伪法线时，峰值
    /// 光源角 = 2·θ_T'（半程向量落在 v 与 l 的角平分线上），根（t = 0，
    /// T' 向 −N 偏 shift）与梢（t = 1，+shift）的峰值角差一阶 ≈ 4·shift
    ///（推导：2·(θ_+ − θ_−) = 4·shift 小角近似）——解析期望，容差吸收
    /// 归一化非线性。
    #[test]
    fn spec_shift_moves_peak() {
        let n = [0.0, 0.0, 1.0];
        let t = [1.0, 0.0, 0.0];
        let v = [0.0, 0.0, 1.0];
        let shift = 0.25f32;
        // 视线沿伪法线（v = n）：H ≈ 光源方向的角平分——对固定 v 扫描
        // 光源在 xz 平面内的方向角，记录高光峰值角
        let peak_angle = |t_along: f32| -> f32 {
            let mut best = 0.0f32;
            let mut best_val = -1.0f32;
            let steps = 720;
            for i in 0..steps {
                let a = i as f32 / steps as f32 * std::f32::consts::TAU;
                let l = [a.cos(), 0.0, a.sin()];
                let (_, s) = kk_lobe(t, n, t_along, v, l, shift, 64.0);
                if s > best_val {
                    best_val = s;
                    best = a;
                }
            }
            best
        };
        let peak_root = peak_angle(0.0);
        let peak_tip = peak_angle(1.0);
        // 峰值角差（取最短弧）应为正（向伪法线 +z 移动）且 ≈ 4·shift
        let delta = (peak_tip - peak_root + std::f32::consts::TAU) % std::f32::consts::TAU;
        let delta = if delta > std::f32::consts::PI {
            delta - std::f32::consts::TAU
        } else {
            delta
        };
        assert!(
            (delta - 4.0 * shift).abs() < 0.1,
            "根梢高光峰值角差应 ≈ 4·shift（{}），得 {delta}",
            4.0 * shift
        );
    }

    /// 高光锐度：exponent 越大瓣越窄——半高宽单调递减（旋钮的非默认
    /// 值断言，R89 铁律）。
    #[test]
    fn spec_exponent_narrows_lobe() {
        let n = [0.0, 0.0, 1.0];
        let t = [1.0, 0.0, 0.0];
        let v = [0.0, 0.0, 1.0];
        let l_peak = [0.0, 0.0, 1.0]; // ⊥ T ⇒ sin 峰值
        let half_width = |exponent: f32| -> f32 {
            let (_, peak) = kk_lobe(t, n, 0.5, v, l_peak, 0.0, exponent);
            // 从峰值向 +x 方向扫到半高
            let mut lo = 0.0f32;
            let mut hi = std::f32::consts::FRAC_PI_2;
            for _ in 0..40 {
                let mid = (lo + hi) * 0.5;
                let l = [mid.sin(), 0.0, mid.cos()];
                let (_, s) = kk_lobe(t, n, 0.5, v, l, 0.0, exponent);
                if s > peak * 0.5 {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            lo
        };
        let w_low = half_width(16.0);
        let w_high = half_width(128.0);
        assert!(
            w_high < w_low * 0.75,
            "exponent 128 的半高宽 ({w_high}) 应明显窄于 16 ({w_low})"
        );
    }

    /// 移位切线的解析锚：shift = 0 ⇒ 恒等；t = 0.5 ⇒ 偏移项为零
    /// （中段不移位）；符号翻转对称。
    #[test]
    fn shifted_tangent_anchors() {
        let t = [1.0, 0.0, 0.0];
        let n = [0.0, 0.0, 1.0];
        assert_eq!(shifted_tangent(t, n, 0.3, 0.5), t, "中段偏移为零");
        assert_eq!(shifted_tangent(t, n, 0.0, 0.0), t, "shift 0 恒等");
        let plus = shifted_tangent(t, n, 0.3, 1.0);
        let minus = shifted_tangent(t, n, 0.3, 0.0);
        assert!(plus[2] > 0.0 && minus[2] < 0.0, "梢正根负，沿伪法线对称");
        assert!((plus[2] + minus[2]).abs() < 1e-6, "对称量级一致");
    }

    /// WGSL 源码看门：kk_light 的关键算式逐条在场（改公式漏同步即挂）。
    #[test]
    fn wgsl_kk_formula_matches_mirror() {
        let src = wgsl_kk_source();
        // 移位切线式
        assert!(
            src.contains(
                "normalize(t_strand\n        + n_pseudo * (shift * (2.0 * t_along - 1.0)))"
            ) || src.contains("shift * (2.0 * t_along - 1.0)"),
            "WGSL kk_light 必须含镜像同款移位式 shift * (2t - 1)"
        );
        // 漫反射 sin(T·L)（sqrt(1 - ctl²)）
        assert!(
            src.contains("let ctl = dot(t_strand, l);"),
            "WGSL kk_light 必须先算 dot(t_strand, l)（镜像 ctl）"
        );
        assert!(
            src.contains("sqrt(max(1.0 - ctl * ctl, 0.0))"),
            "WGSL 漫反射必须是 sqrt(1 - ctl²)（sin 瓣）"
        );
        // 高光 sin(T'·H)
        assert!(
            src.contains("sqrt(max(1.0 - cth * cth, 0.0))"),
            "WGSL 高光必须是 sqrt(1 - cth²)（sin 瓣）"
        );
        assert!(
            src.contains("let h = normalize(v + l);"),
            "WGSL 半程向量必须 normalize(v + l)（镜像同式）"
        );
        // 高光颜色固定式
        assert!(
            src.contains("mix(vec3<f32>(1.0), albedo, 0.5)"),
            "WGSL 高光颜色必须是 mix(白, albedo, 0.5) 固定式"
        );
    }
}
