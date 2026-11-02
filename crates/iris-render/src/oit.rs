//! Weighted Blended OIT（C7b，§8.3 半透明排序条目的逐像素闭合）。
//!
//! # 是什么
//!
//! [McGuire & Bavoil, *Weighted Blended Order-Independent Transparency*,
//! HPG 2013] 的两遍实现：**gather** 把透明几何以任意序累进两张 A-buffer
//! （accum RGBA16F：`Σ(color·α·w, α·w)`，blend `(ONE, ONE)`；reveal R16F：
//! `Π(1−α)`，blend `(ZERO, ONE_MINUS_SRC_COLOR)` 让连乘由固定功能完成），
//! **resolve** 全屏合成 `out = avg·(1−reveal) + bg·reveal` 写回 HDR MSAA。
//! 凹体 / 交叠体积的自遮挡排序误差（painter 按包围球中心视深排序的
//! 结构性缺陷）就此消灭——代价是**近似算法**：多层叠加的能量有偏。
//!
//! # 缺席语义（API 的一部分）
//!
//! [`OrderedIndependentTransparency`] 资源**缺席 = 关**——透明 pass 走
//! painter 原路，gather/resolve 管线一个 draw 都不发、A-buffer 一张都
//! 不分配，全部既有像素基线逐位不变（仓库「资源缺席 = 关」文化，同
//! `TaaSettings` / `SsrSettings`）。存在 = 开（v1 无细调旋钮）。
//!
//! # 与既有体系的关系
//!
//! - 只吃**主透明 pass**（`AlphaMode::Blend` 几何，含蒙皮与双面变体）；
//!   水体 / 粒子 / 贴花 / RT 相机各有独立管线，维持 painter 语义
//!   （§8.68④ 声明边界）。
//! - 遮挡真值 = scene_depth 副本（**不透明重画**，透明 / 贴花 / 粒子
//!   本就不进）：OIT 开启帧把副本 pass 提前编码到透明 pass 之前——
//!   副本内容与既有各消费者（DOF / MB / TAA / SSR / GTAO / 雾 / 水体）
//!   读到的完全一致，移动零外溢（水体/SSR 全链既有 GPU 测试原值背书）。
//! - 单层兼容锚：一张 α 面片时合成结果 ≡ 经典 src-alpha 混合
//!   （[`oit_resolve_color`] 的代数 + GPU 测试的量化容差双重钉住）。
//!
//! # 诚实限制（§8.68）
//!
//! 近似算法的多层偏差、无覆盖像素的逐位护栏、RT 相机 / 水 / 粒子不在
//! 覆盖范围、测深改走 1× 副本（掠射边缘口径）、透明覆盖区背景 MSAA
//! 子采样退化——完整清单在 ARCHITECTURE §8.68。

use glam::Vec3;

/// OIT 开关资源（标记型，v1 无旋钮）。插入 World ⇒ 主透明 pass 改走
/// gather+resolve；缺席 ⇒ painter 原路逐位不变。
#[derive(Clone, Copy, Debug, PartialEq, Eq, iris_core::Resource)]
pub struct OrderedIndependentTransparency;

/// 权重公式的 CPU 镜像（`standard.wgsl::oit_weight` 逐式同序，两侧
/// 必须同步修改——禁凭记忆纪律）。`z` = 窗口深度 ∈ [0,1]（DirectX 约定）。
pub fn oit_weight(color: Vec3, alpha: f32, z: f32) -> f32 {
    let lum = color.x.max(color.y).max(color.z);
    let scale = (0.03f32 / (1e-5 + (z / 200.0f32).powi(4))).clamp(1e-2, 3e3);
    ((lum * alpha).min(1.0) * scale).max(alpha)
}

/// 合成公式的 CPU 镜像（`oit.wgsl::fs_main` 同式）：
/// `avg = accum.rgb / max(accum.a, 1e-5)`；`out = avg·(1−reveal) + bg·reveal`。
///
/// 代数锚（单测钉住）：
/// - **单层**（一次累积，α∈(0,1]、w>0）⇒ `out ≡ color·α + bg·(1−α)`
///   （painter 兼容锚：WBOIT 在单层时精确退化为经典混合）；
/// - **无覆盖**（accum = 0、reveal = 1）⇒ `out = bg` **逐位恒等**
///   （`0·0 + bg·1`，乘法单位元精确）——改动面护栏的代数根基。
pub fn oit_resolve_color(accum_rgb: Vec3, accum_a: f32, reveal: f32, bg: Vec3) -> Vec3 {
    let avg = accum_rgb / accum_a.max(1e-5);
    avg * (1.0 - reveal) + bg * reveal
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: Vec3, b: Vec3, eps: f32) -> bool {
        (a - b).length() < eps
    }

    #[test]
    fn weight_is_alpha_floor_and_lum_saturating() {
        // 外层 max(a, ·)：极暗颜色权重下界 = α（薄暗面不被权重饿死的原稿意图）
        let w = oit_weight(Vec3::splat(1e-9), 0.5, 0.5);
        assert_eq!(w.to_bits(), 0.5f32.to_bits(), "暗色地板必须 = α");
        // 亮度 × α ≥ 1 ⇒ min 饱和到 1（HDR 强光不再无限加权）：z=0 时
        // scale = clamp(0.03/1e-5, 1e-2, 3e3) = 3000 精确
        let s = oit_weight(Vec3::splat(100.0), 1.0, 0.0);
        assert_eq!(s.to_bits(), (1.0f32 * 3000.0f32).max(1.0).to_bits());
        // α = 0 ⇒ 权重 0（accum 不累积、reveal 不衰减——旁路逐位中性）
        assert_eq!(
            oit_weight(Vec3::splat(3.0), 0.0, 0.3).to_bits(),
            0.0f32.to_bits()
        );
    }

    #[test]
    fn weight_depth_scale_band_saturates_over_the_window_domain() {
        // 取证式 `0.03/(1e-5+pow(z/200,4))` 在窗口深度域 z∈[0,1] 内分母的
        // pow 项 ≤ 6.25e-10 ⇒ scale ∈ [2999.81, 3000] 恒贴上限（z=0 时
        // 0.03/1e-5 恰为 3000）。**如实入档**：窗口域内权重退化为
        // `max(min(1, lum·α)·3e3, α)`——前优先倾向保留（亮面权重大、
        // 暗面落 α 地板），深度带不起作用；带下限 1e-2 要 z > 263 才
        // 触发（越出 DirectX 窗口约定，纯公式边界 sanity）。
        let scale = |z: f32| (0.03f32 / (1e-5 + (z / 200.0f32).powi(4))).clamp(1e-2, 3e3);
        assert_eq!(scale(0.0).to_bits(), 3e3f32.to_bits());
        assert!(
            (scale(1.0) - 3e3).abs() < 0.5,
            "窗口域内恒贴上限：{}",
            scale(1.0)
        );
        let z_floor = 200.0f32 * (3.0f32 - 1e-5).powf(0.25);
        assert_eq!(scale(z_floor.max(1.0)).to_bits(), scale(z_floor).to_bits());
        assert!(scale(1.0) > scale(500.0), "公式带在域外单调降（sanity）");
        // 前优先形态直证：同一 α 下亮面权重 ≫ 暗面（≫ 表示 3 倍）
        let bright = oit_weight(Vec3::splat(0.5), 0.5, 0.5);
        let dark = oit_weight(Vec3::splat(1e-9), 0.5, 0.5);
        assert!(bright > dark * 3.0, "亮面前优先：{bright} vs {dark}");
    }

    #[test]
    fn resolve_single_layer_equals_classic_alpha_blend() {
        // 单层 (color c, α a)：accum = (c·a·w, a·w)，reveal = 1−a
        // ⇒ avg = c（w 精确约去）、out = c·a + bg·(1−a)——painter 兼容锚
        let c = Vec3::new(0.8, 0.2, 0.5);
        let bg = Vec3::new(0.1, 0.6, 0.3);
        for a in [0.25f32, 0.5, 0.9] {
            let w = oit_weight(c, a, 0.4);
            let accum_rgb = c * a * w;
            let accum_a = a * w;
            let got = oit_resolve_color(accum_rgb, accum_a, 1.0 - a, bg);
            let want = c * a + bg * (1.0 - a);
            assert!(approx(got, want, 1e-5), "α={a}: {got:?} vs {want:?}");
        }
    }

    #[test]
    fn resolve_no_coverage_is_bitwise_identity() {
        let bg = Vec3::new(0.31, 0.42, 0.53);
        let out = oit_resolve_color(Vec3::ZERO, 0.0, 1.0, bg);
        assert_eq!(out, bg, "reveal=1、accum=0 ⇒ 逐位 = bg（护栏代数根基）");
    }

    #[test]
    fn resolve_two_layers_order_independent_within_weighted_band() {
        // 两层交叠：accum 加法交换 ⇒ 任意提交序 (A,B)/(B,A) 逐位相等；
        // reveal 连乘同理。这是「顺序无关」的精确陈述（合成式确定）。
        let mk = |c: Vec3, a: f32, z: f32| {
            let w = oit_weight(c, a, z);
            (c * a * w, a * w)
        };
        let bg = Vec3::new(0.5, 0.5, 0.5);
        let (r1, a1) = mk(Vec3::new(1.0, 0.0, 0.0), 0.5, 0.1);
        let (r2, a2) = mk(Vec3::new(0.0, 0.0, 1.0), 0.4, 0.9);
        let fwd = oit_resolve_color(r1 + r2, a1 + a2, (1.0 - 0.5) * (1.0 - 0.4), bg);
        let bwd = oit_resolve_color(r2 + r1, a2 + a1, (1.0 - 0.4) * (1.0 - 0.5), bg);
        assert_eq!(fwd, bwd, "加法/乘法交换序必须逐位一致");
        assert!(fwd.x > 0.0 && fwd.z > 0.0, "两层颜色都须留痕：{fwd:?}");
    }

    #[test]
    fn resolve_accum_alpha_floor_guards_zero_division() {
        let out = oit_resolve_color(Vec3::splat(1e-9), 0.0, 0.5, Vec3::ZERO);
        assert!(
            out.length() < 1e-3,
            "accum.a=0 ⇒ avg 地板 1e-5 兜底：{out:?}"
        );
    }
}
