//! 程序化写实纹理生成器（**R42 载荷**）：法线 / ORM 数据贴图与贴花
//! 纹理的纯函数集合。
//!
//! 引擎侧已有 [`CpuTexture::brick_normal_orm`]（砖缝高度场 → 法线 +
//! ORM 打包，R42 验收数据）——地面石板与庭院砖墙**直接复用它**（解析
//! 式高度场、splitmix64 无状态哈希，确定性有引擎侧单测守护；本模块
//! 不重复造轮子）。本模块补齐引擎没有的四类程序化纹理，全部纯函数：
//!
//! - [`scratched_metal_normal_orm`]：刀身拉丝划痕——金属高光面上刻
//!   细密纵向划痕（法线扰动 + ORM 粗糙度起伏，金属度恒 1）；
//! - [`cloth_normal_orm`]：布料褶皱——低频正弦起伏 + 高频哈希抖动
//!   （法线），粗糙度高频变化、金属度 0；
//! - [`blood_splat`]：血渍贴花（RGBA，A 通道控覆盖；中心浓、边缘
//!   放射状溅点）；
//! - [`bullet_hole`]：弹孔贴花（暗心 + 浅灰外环，径向 alpha 衰减）。
//!
//! # 确定性
//!
//! 与引擎约定同款：无状态哈希流（splitmix64 终结函数，参照
//! `iris-render/particles.rs` 的 `mix64` 与 `texture.rs`），**禁止
//! rand 与全局状态**——同一参数两次调用输出逐位一致（单测锁定）。
//! 程序化美术数据生成允许 `f32::sqrt` 直接使用（非物理确定性路径，
//! 与引擎 `brick_normal_orm` 同口径）。

use iris_render::texture::CpuTexture;

/// splitmix64 终结函数（无状态哈希；与引擎 `texture.rs` 同源同式）。
fn mix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// 哈希到 [0, 1) 的浮点（第 24..48 位段——比低 8 位细腻、比全 64 位便宜）。
fn hash01(seed: u64) -> f32 {
    ((mix64(seed) >> 24) & 0xff_ffff) as f32 / 0x100_0000 as f32
}

/// smoothstep（`t²(3−2t)`，两端钳制）。
fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 把高度场中心差分编码为切线空间法线 RGB（A = 255）。
///
/// `sample(u, v)` 返回高度（值域不限，差分只看变化率）；`bump` 为法线
/// 强度（中心差分步长系数，与引擎 `brick_normal_orm` 同语义）。
fn normal_from_height(size: u32, bump: f32, sample: impl Fn(f32, f32) -> f32) -> Vec<u8> {
    let eps = 1.0 / size as f32;
    let step = 1.5 * eps;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let u = (x as f32 + 0.5) / size as f32;
            let v = (y as f32 + 0.5) / size as f32;
            let dhdu = (sample(u + step, v) - sample(u - step, v)) / (2.0 * step);
            let dhdv = (sample(u, v + step) - sample(u, v - step)) / (2.0 * step);
            let len = (dhdu * dhdu * bump * bump + dhdv * dhdv * bump * bump + 1.0).sqrt();
            let nx = -dhdu * bump / len;
            let ny = -dhdv * bump / len;
            let nz = 1.0 / len;
            out.extend_from_slice(&[
                ((nx * 0.5 + 0.5) * 255.0) as u8,
                ((ny * 0.5 + 0.5) * 255.0) as u8,
                ((nz * 0.5 + 0.5) * 255.0) as u8,
                255,
            ]);
        }
    }
    out
}

/// 刀身拉丝划痕的法线 + ORM 对（**R42 写实材质**：金属刀身高光划痕）。
///
/// 高度场 = 纵向（v 向）平行细划痕：每条划痕取哈希决定的 v 基线与
/// 深度，沿 u 走向按哈希相位轻微起伏；另叠加极低频锻打鼓包。ORM：
/// R = AO 恒 1（无遮蔽概念）、G = 粗糙度（划痕内变粗 0.55、锻面
/// 0.18 + 哈希微变——粗细起伏正是「拉丝高光」的来源）、B = 金属度
/// 恒 1。两张均为线性数据贴图（`srgb = false`）。
///
/// `size` 为边长（像素，≥ 8）；`seed` 为划痕布局种子。
pub fn scratched_metal_normal_orm(size: u32, seed: u64) -> (CpuTexture, CpuTexture) {
    assert!(size >= 8, "划痕纹理边长至少 8 像素");
    // 划痕参数：count 条，基线 v、深度、u 向起伏相位都由哈希决定
    let count = 40usize;
    let scratch_base = |i: usize| hash01(seed ^ (i as u64).wrapping_mul(0x2545_f491_4f6c_dd1d));
    let height = |u: f32, v: f32| -> f32 {
        // 锻打鼓包：两轴极低频正弦（幅 0.04）
        let forge = 0.04 * (u * 7.0).sin() * (v * 11.0).sin();
        // 划痕：距离最近基线 < 半宽则凹陷（深度随 u 起伏）
        let mut depth = 0.0f32;
        for i in 0..count {
            let base = scratch_base(i);
            let d = (v - base).abs();
            let half = 0.002 + 0.004 * scratch_base(i + count);
            if d < half {
                let wobble = 0.004 * (u * 90.0 + base * 200.0).sin();
                let t = ((d + wobble.abs()) / (half + 0.004)).clamp(0.0, 1.0);
                depth = depth.max((1.0 - smooth(t)) * 0.06);
            }
        }
        forge - depth
    };
    let normal = normal_from_height(size, 2.0, height);
    // ORM：粗糙度 = 锻面基线 − 划痕更亮（更粗糙）的高光破碎
    let mut orm = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let u = (x as f32 + 0.5) / size as f32;
            let v = (y as f32 + 0.5) / size as f32;
            let h = height(u, v);
            // 划痕（h 比锻面低）→ 粗糙度上探
            let scratch = (-h / 0.06).clamp(0.0, 1.0);
            let rough = 0.18 + 0.37 * scratch + 0.06 * hash01(seed ^ ((x * 31 + y) as u64));
            orm.extend_from_slice(&[255, (rough.clamp(0.0, 1.0) * 255.0) as u8, 255, 255]);
        }
    }
    (
        CpuTexture {
            width: size,
            height: size,
            rgba: normal,
            srgb: false,
        },
        CpuTexture {
            width: size,
            height: size,
            rgba: orm,
            srgb: false,
        },
    )
}

/// 布料褶皱的法线 + ORM 对（**R42 写实材质**：角色身体的布料通道）。
///
/// 高度场 = 低频大褶皱（两轴正弦叠加，相位逐褶皱哈希）+ 高频织纹
/// （细密正交条纹），织物接缝感由织纹的陡峭度给出。ORM：粗糙度
/// 0.78 基线 + 织纹调制、金属度 0、AO 恒 1。线性数据贴图。
pub fn cloth_normal_orm(size: u32, seed: u64) -> (CpuTexture, CpuTexture) {
    assert!(size >= 8, "布料纹理边长至少 8 像素");
    let p1 = 2.0 + 3.0 * hash01(seed);
    let p2 = 2.0 + 3.0 * hash01(seed ^ 0x9e37_79b9);
    let height = |u: f32, v: f32| -> f32 {
        // 大褶皱：两组低频波斜交
        let fold = 0.05 * (u * p1 + 0.6 * (v * p2).sin()).sin()
            + 0.04 * (v * p1 * 1.3 + 0.5 * (u * p2).sin()).sin();
        // 织纹：正交细条纹（陡峭高频，幅 0.012）
        let weave = 0.012 * (u * size as f32 * 0.75).sin() * (v * size as f32 * 0.75).sin();
        fold + weave
    };
    let normal = normal_from_height(size, 1.2, height);
    let mut orm = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let u = (x as f32 + 0.5) / size as f32;
            let v = (y as f32 + 0.5) / size as f32;
            let weave = (u * size as f32 * 0.75).sin() * (v * size as f32 * 0.75).sin();
            let rough = 0.78 + 0.05 * weave;
            orm.extend_from_slice(&[255, (rough * 255.0) as u8, 0, 255]);
        }
    }
    (
        CpuTexture {
            width: size,
            height: size,
            rgba: normal,
            srgb: false,
        },
        CpuTexture {
            width: size,
            height: size,
            rgba: orm,
            srgb: false,
        },
    )
}

/// 血渍贴花（RGBA8，`srgb = true`）：中心浓斑 + 哈希分布的放射溅点，
/// alpha = 覆盖（A 通道控制贴花覆盖语义，见 `iris_render::decal`）。
///
/// `size` 为边长（像素，≥ 8）；`seed` 决定溅点布局。
pub fn blood_splat(size: u32, seed: u64) -> CpuTexture {
    assert!(size >= 8, "血渍贴花边长至少 8 像素");
    let center = size as f32 * 0.5;
    // 中心浓斑半径 + 12 个溅点（半径、方向、距离全部哈希）
    let core_r = size as f32 * 0.16;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    let drops: Vec<(f32, f32, f32)> = (0..12)
        .map(|i| {
            let s = seed ^ (i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
            let angle = hash01(s) * std::f32::consts::TAU;
            let dist = size as f32 * (0.18 + 0.28 * hash01(s ^ 1));
            let radius = size as f32 * (0.03 + 0.05 * hash01(s ^ 2));
            (
                center + angle.cos() * dist,
                center + angle.sin() * dist,
                radius,
            )
        })
        .collect();
    for y in 0..size {
        for x in 0..size {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            // 覆盖度：主斑 smoothstep 衰减 + 溅点硬覆盖
            let d_core = ((px - center).powi(2) + (py - center).powi(2)).sqrt();
            let mut a = 1.0 - smooth((d_core - core_r * 0.7) / (core_r * 0.9));
            for &(dx, dy, dr) in &drops {
                let d = ((px - dx).powi(2) + (py - dy).powi(2)).sqrt();
                if d < dr {
                    a = a.max(0.75);
                }
            }
            let a = a.clamp(0.0, 1.0);
            // 深红：核心更暗（中心 alpha 高处乘深），边缘略亮
            let shade = 0.55 + 0.3 * (1.0 - a);
            rgba.extend_from_slice(&[
                (0.42 * shade * 255.0) as u8,
                (0.03 * shade * 255.0) as u8,
                (0.04 * shade * 255.0) as u8,
                (a * 255.0) as u8,
            ]);
        }
    }
    CpuTexture {
        width: size,
        height: size,
        rgba,
        srgb: true,
    }
}

/// 弹孔贴花（RGBA8，`srgb = true`）：暗色孔心 + 浅灰崩边环 + 径向
/// alpha 衰减。`size` 为边长（像素，≥ 8）。
pub fn bullet_hole(size: u32) -> CpuTexture {
    assert!(size >= 8, "弹孔贴花边长至少 8 像素");
    let center = size as f32 * 0.5;
    let hole_r = size as f32 * 0.18;
    let rim_r = size as f32 * 0.34;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let d = ((px - center).powi(2) + (py - center).powi(2)).sqrt();
            // 三段：孔心（深黑不透明）→ 崩边环（灰、alpha 衰减）→ 外透明
            let (rgb, a) = if d < hole_r {
                ([0.02, 0.02, 0.02], 1.0)
            } else if d < rim_r {
                let t = (d - hole_r) / (rim_r - hole_r);
                let g = 0.55 - 0.35 * t;
                ([g, g * 0.98, g * 0.94], 1.0 - smooth(t))
            } else {
                ([0.0, 0.0, 0.0], 0.0)
            };
            rgba.extend_from_slice(&[
                (rgb[0] * 255.0) as u8,
                (rgb[1] * 255.0) as u8,
                (rgb[2] * 255.0) as u8,
                (a * 255.0) as u8,
            ]);
        }
    }
    CpuTexture {
        width: size,
        height: size,
        rgba,
        srgb: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 划痕金属：确定性（同种子逐位一致、异种子不同）；ORM 金属度恒 1、
    /// 粗糙度有起伏（拉丝高光的量化证据）。
    #[test]
    fn scratched_metal_is_deterministic_and_textured() {
        let (n1, o1) = scratched_metal_normal_orm(32, 0xABCD);
        let (n2, o2) = scratched_metal_normal_orm(32, 0xABCD);
        assert_eq!(n1.rgba, n2.rgba, "同种子法线应逐位一致");
        assert_eq!(o1.rgba, o2.rgba, "同种子 ORM 应逐位一致");
        let (n3, _) = scratched_metal_normal_orm(32, 0xABCE);
        assert_ne!(n1.rgba, n3.rgba, "异种子应产生不同划痕布局");
        assert_eq!(n1.width, 32);
        assert!(!n1.srgb, "法线是线性数据贴图");
        assert!(!o1.srgb, "ORM 是线性数据贴图");
        // 金属度通道（B）全 255
        assert!(o1.rgba.chunks_exact(4).all(|px| px[2] == 255));
        // 粗糙度通道（G）有变化（划痕 vs 锻面）
        let rough_values: Vec<u8> = o1.rgba.chunks_exact(4).map(|px| px[1]).collect();
        let min = *rough_values.iter().min().expect("非空");
        let max = *rough_values.iter().max().expect("非空");
        assert!(max - min > 40, "粗糙度应有明显起伏（{min}..{max}）");
    }

    /// 布料：确定性；金属度恒 0、粗糙度基线附近；法线偏离恒等
    /// （褶皱存在 → 不是全部 (128, 128, 255)）。
    #[test]
    fn cloth_is_deterministic_and_wrinkled() {
        let (n1, o1) = cloth_normal_orm(32, 7);
        let (n2, o2) = cloth_normal_orm(32, 7);
        assert_eq!(n1.rgba, n2.rgba);
        assert_eq!(o1.rgba, o2.rgba);
        assert!(!n1.srgb && !o1.srgb);
        assert!(
            o1.rgba.chunks_exact(4).all(|px| px[2] == 0),
            "布料金属度恒 0"
        );
        let flat = n1
            .rgba
            .chunks_exact(4)
            .all(|px| px[0] == 128 && px[1] == 128 && px[2] == 255);
        assert!(!flat, "褶皱法线不应全部为恒等 (128,128,255)");
        let rough_values: Vec<u8> = o1.rgba.chunks_exact(4).map(|px| px[1]).collect();
        let min = *rough_values.iter().min().expect("非空");
        let max = *rough_values.iter().max().expect("非空");
        assert!(
            (min as i32 - 199).abs() <= 20 && (max as i32 - 211).abs() <= 30,
            "粗糙度应贴近 0.78 基线（实测 {min}..{max}，0.78×255≈199）"
        );
    }

    /// 血渍：确定性；核心不透明、四角透明；非纯灰（深红着色）。
    #[test]
    fn blood_splat_shape_and_color() {
        let a = blood_splat(48, 42);
        let b = blood_splat(48, 42);
        assert_eq!(a.rgba, b.rgba, "同种子应逐位一致");
        assert!(a.srgb, "贴花是颜色数据");
        let at = |x: u32, y: u32| -> [u8; 4] {
            let i = ((y * 48 + x) * 4) as usize;
            [a.rgba[i], a.rgba[i + 1], a.rgba[i + 2], a.rgba[i + 3]]
        };
        // 中心：不透明深红
        let c = at(24, 24);
        assert_eq!(c[3], 255, "中心应不透明");
        assert!(c[0] > c[1] * 4 && c[0] > c[2] * 4, "应为深红（{c:?}）");
        // 四角：透明（溅点未必到角落——用 alpha 低做弱断言）
        let corner = at(1, 1);
        assert!(corner[3] < 128, "角落应大部分透明（实际 {corner:?}）");
    }

    /// 弹孔：中心不透明黑、外环灰、边缘透明；确定性（无种子纯函数）。
    #[test]
    fn bullet_hole_shape() {
        let a = bullet_hole(32);
        let b = bullet_hole(32);
        assert_eq!(a.rgba, b.rgba);
        let at = |x: u32, y: u32| -> [u8; 4] {
            let i = ((y * 32 + x) * 4) as usize;
            [a.rgba[i], a.rgba[i + 1], a.rgba[i + 2], a.rgba[i + 3]]
        };
        let center = at(16, 16);
        assert_eq!(center, [5, 5, 5, 255], "孔心应近黑不透明");
        let edge = at(31, 16);
        assert_eq!(edge[3], 0, "贴花边缘应透明");
        let rim = at(23, 16); // 半径 ~7.5 px ∈ (0.18, 0.34)×32 = (5.8, 10.9)
        assert!(rim[3] > 0 && rim[0] > 30, "崩边环应有灰度覆盖（{rim:?}）");
    }
}
