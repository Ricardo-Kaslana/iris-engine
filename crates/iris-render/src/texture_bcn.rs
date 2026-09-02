//! BC1（DXT1）纹理压缩编码器（R30）：纯函数、确定性、零 unsafe、零依赖。
//!
//! BC1 把 4×4 像素块压缩为 8 字节（RGBA8 的 1/8）：
//! - 2 字节 color0 + 2 字节 color1（RGB565，小端）；
//! - 4 字节 16 个像素的 2-bit 调色板索引（块内行主序，像素 0 位于
//!   第 4 字节的最低 2 位）。
//!
//! 编码采用**全不透明 4 色模式**（不使用三色 punch-through）：调色板为
//! 端点 c0/c1 与两个 1/3 插值 `(2c0+c1)/3`、`(c0+2c1)/3`。理由：BC1 的
//! alpha 通道只有「1 位 punch-through」形态，无法表达连续半透明；引擎
//! 纹理含半透明像素时整体回退 RGBA8 上传（见 [`bc1_eligible`]），编码器
//! 内部索性不感知 alpha（输入仍为 RGBA8，alpha 被忽略）。
//!
//! **确定性**：端点选择、亮度排序、量化、索引匹配全部为整数运算 +
//! 固定的并列取先规则（严格 `>` / `<` 比较保留首个最优），同输入两次
//! 编码逐位一致（单测守护）——这对场景序列化 / 回放 / 差分测试是硬要求。
//!
//! **端点选择取舍**：块内 RGB 欧氏距离（平方）最远的两像素，O(16²) = 256
//! 次平方距离比较的确定性暴力。比 PCA / 范围法（沿主轴取极值）质量略差
//! （最坏差约 5-10% PSNR），但行为完全可复现、实现 40 行、无浮点——对
//! 引擎「低占用 + 确定性」的优先级是正确取舍；更高质量的端点优化属于
//! 事后迭代（encoded 块可整体替换，接口不变）。
//!
//! **非 4 倍数尺寸**：统一**预 pad 到 4 的倍数**（边缘像素复制），选一
//! 写死：wgpu 压缩纹理要求尺寸 4 对齐，pad 后编码结果才是完整合法的
//! BC1 块阵列；「向下取整块数」会丢像素，「末块按实际像素编码」产出的
//! 不是合法 BC1 流（GPU 无法直接上传）。注意渲染器上传路径只在源纹理
//! **本身 4 对齐**时走 BC1（见 [`bc1_eligible`]），pad 能力面向工具链
//! 与离线编码场景（pad 区域复制边缘像素，采样 ClampToEdge 语义一致）。
//!
//! **mipmap v1 不做**：引擎暂无 mipmap 机制（`mip_level_count` 恒 1），
//! BC1 压缩不含 mip 链生成；引入 mipmap 时本模块补 `encode_bc1_mips`
//! （逐级对半 + 递归编码，接口另议）。

/// BC1 单块字节数（color0 2B + color1 2B + 索引 4B）。
pub const BC1_BLOCK_BYTES: usize = 8;

/// 压缩后尺寸：向上取整到 4 的倍数（wgpu 压缩纹理的块对齐要求）。
pub fn padded_extent(width: u32, height: u32) -> (u32, u32) {
    (width.div_ceil(4) * 4, height.div_ceil(4) * 4)
}

/// 压缩编码后的总字节数：块数 × 8。
pub fn encoded_len(width: u32, height: u32) -> u64 {
    let (pw, ph) = padded_extent(width, height);
    (pw as u64 / 4) * (ph as u64 / 4) * BC1_BLOCK_BYTES as u64
}

/// alpha 是否恒 255（全不透明）。BC1 无 alpha 通道，含任何非 255 alpha
/// 的纹理都必须回退 RGBA8 上传，否则半透明语义丢失。
pub fn is_fully_opaque(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4).all(|px| px[3] == 255)
}

/// BC1 上传决策（渲染器 `ensure_texture` 与 `TextureStore::gpu_bytes_estimate`
/// 共用，单一事实来源避免两处漂移）。全部满足才走 BC1：
/// 1. 设备启用 `wgpu::Features::TEXTURE_COMPRESSION_BC`（wasm 的 WebGPU
///    实现普遍不支持——这是**无分支回退**：不写 cfg，运行时特性检测天然
///    覆盖 web，native 无 BC 的老 GPU 同样命中回退）；
/// 2. 尺寸本身 4 对齐（编码器的 pad 能力不在上传路径启用——pad 区像素
///    是复制出的假像素，贴图边缘有渗色风险，v1 保守）；
/// 3. 数据长度与尺寸一致且 alpha 恒 255。
pub fn bc1_eligible(width: u32, height: u32, rgba: &[u8], feature_bc: bool) -> bool {
    feature_bc
        && width > 0
        && height > 0
        && width % 4 == 0
        && height % 4 == 0
        && rgba.len() == width as usize * height as usize * 4
        && is_fully_opaque(rgba)
}

/// BC1 编码：RGBA8 源 → 4×4 块流（每块 8 字节，块内行主序）。
///
/// 尺寸非 4 倍数时内部 pad（边缘像素复制，见模块文档）；断言数据长度
/// 与尺寸一致（中文断言消息，引擎约定）。
pub fn encode_bc1(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    assert!(width >= 1 && height >= 1, "纹理尺寸必须为正");
    assert_eq!(
        rgba.len(),
        width as usize * height as usize * 4,
        "RGBA 数据长度与纹理尺寸不一致"
    );
    let (pw, ph) = padded_extent(width, height);
    let blocks_x = (pw / 4) as usize;
    let blocks_y = (ph / 4) as usize;
    let mut out = vec![0u8; blocks_x * blocks_y * BC1_BLOCK_BYTES];
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let block = encode_block(rgba, width, height, bx, by);
            let i = (by * blocks_x + bx) * BC1_BLOCK_BYTES;
            out[i..i + BC1_BLOCK_BYTES].copy_from_slice(&block);
        }
    }
    out
}

/// 取像素（RGB），越界时复制边缘（pad 语义）。
fn pixel_rgb(rgba: &[u8], width: u32, height: u32, x: u32, y: u32) -> [u8; 3] {
    let x = x.min(width - 1);
    let y = y.min(height - 1);
    let i = ((y * width + x) * 4) as usize;
    [rgba[i], rgba[i + 1], rgba[i + 2]]
}

/// RGB 平方欧氏距离（整数，无浮点——确定性的一部分）。
fn dist_sq(a: [u8; 3], b: [u8; 3]) -> i64 {
    let dr = a[0] as i64 - b[0] as i64;
    let dg = a[1] as i64 - b[1] as i64;
    let db = a[2] as i64 - b[2] as i64;
    dr * dr + dg * dg + db * db
}

/// 亮度权值（BT.601 × 256 取整权值，仅用于端点排序的确定性约定）。
fn luma(c: [u8; 3]) -> u32 {
    c[0] as u32 * 299 + c[1] as u32 * 587 + c[2] as u32 * 114
}

/// 8-bit → RGB565（各通道四舍五入）。
fn pack565(c: [u8; 3]) -> u16 {
    let r = (c[0] as u16 * 31 + 127) / 255;
    let g = (c[1] as u16 * 63 + 127) / 255;
    let b = (c[2] as u16 * 31 + 127) / 255;
    (r << 11) | (g << 5) | b
}

/// RGB565 → 8-bit（位复制扩展：低位移位补齐高位，0 与满值精确还原）。
fn unpack565(v: u16) -> [u8; 3] {
    let r = (v >> 11) as u8 & 0x1f;
    let g = ((v >> 5) & 0x3f) as u8;
    let b = (v & 0x1f) as u8;
    [
        (r << 3) | (r >> 2),
        (g << 2) | (g >> 4),
        (b << 3) | (b >> 2),
    ]
}

/// 端点插值：`t = 1` 为 `(2a + b) / 3`，`t = 2` 为 `(a + 2b) / 3`
/// （+1 后整除实现对称舍入，与常见硬件解码器的整数实现一致量级；
/// 插值点只用于索引匹配，不进入输出字节流）。
fn mix_third(a: [u8; 3], b: [u8; 3], t: u32) -> [u8; 3] {
    let mut out = [0u8; 3];
    for i in 0..3 {
        let v = if t == 1 {
            (2 * a[i] as i32 + b[i] as i32 + 1) / 3
        } else {
            (a[i] as i32 + 2 * b[i] as i32 + 1) / 3
        };
        out[i] = v.clamp(0, 255) as u8;
    }
    out
}

/// 编码单个 4×4 块（见模块文档的算法说明）。
fn encode_block(rgba: &[u8], width: u32, height: u32, bx: usize, by: usize) -> [u8; 8] {
    let (bx, by) = (bx as u32 * 4, by as u32 * 4);
    let mut pixels = [[0u8; 3]; 16];
    for (k, px) in pixels.iter_mut().enumerate() {
        *px = pixel_rgb(
            rgba,
            width,
            height,
            bx + (k % 4) as u32,
            by + (k / 4) as u32,
        );
    }
    // 端点 = 平方距离最远的两像素；严格 `>` 使并列保留首个 (i, j)
    // （下标字典序），输入相同则输出逐位相同
    let mut best = (0usize, 1usize);
    let mut best_d: i64 = -1;
    for i in 0..16 {
        for j in (i + 1)..16 {
            let d = dist_sq(pixels[i], pixels[j]);
            if d > best_d {
                best_d = d;
                best = (i, j);
            }
        }
    }
    let (mut c0, mut c1) = (pixels[best.0], pixels[best.1]);
    // 约定亮度序 c0 > c1（BC1 四色模式不要求，但固定方向是确定性的一部分）
    if luma(c1) > luma(c0) {
        std::mem::swap(&mut c0, &mut c1);
    }
    let c0_565 = pack565(c0);
    let c1_565 = pack565(c1);
    let p0 = unpack565(c0_565);
    let p1 = unpack565(c1_565);
    // 四色调色板：两 565 端点（8-bit 展开）+ 两个 1/3 插值。
    // 同色 565 端点（纯色块）时四项相同 → 索引全部取 0（并列取先），
    // 解码精确还原（纯色往返零误差的来源）
    let palette = [p0, p1, mix_third(p0, p1, 1), mix_third(p0, p1, 2)];
    let mut indices = [0u8; 16];
    for (k, px) in pixels.iter().enumerate() {
        let mut nearest = 0u8;
        let mut nearest_d = i64::MAX;
        for (pi, pc) in palette.iter().enumerate() {
            let d = dist_sq(*px, *pc);
            // 严格 `<` 使并列取最小索引（确定性）
            if d < nearest_d {
                nearest_d = d;
                nearest = pi as u8;
            }
        }
        indices[k] = nearest;
    }
    // 位打包：color0/color1 RGB565 小端；索引每像素 2 bit、行主序，
    // 像素 0 在第 4 字节最低 2 位
    let mut out = [0u8; 8];
    out[0..2].copy_from_slice(&c0_565.to_le_bytes());
    out[2..4].copy_from_slice(&c1_565.to_le_bytes());
    for (k, &idx) in indices.iter().enumerate() {
        out[4 + k / 4] |= (idx & 0b11) << ((k % 4) * 2);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试辅助：从 BC1 块流解码 RGBA8（仅测试用，镜像硬件解码语义：
    /// 565 位复制展开 + 四色调色板）。返回 pad 后尺寸的像素。
    fn decode_bc1(blocks: &[u8], width: u32, height: u32) -> (u32, u32, Vec<u8>) {
        let (pw, ph) = padded_extent(width, height);
        let blocks_x = (pw / 4) as usize;
        let blocks_y = (ph / 4) as usize;
        let mut out = vec![0u8; (pw * ph * 4) as usize];
        for by in 0..blocks_y {
            for bx in 0..blocks_x {
                let b = &blocks[(by * blocks_x + bx) * 8..(by * blocks_x + bx) * 8 + 8];
                let c0 = u16::from_le_bytes([b[0], b[1]]);
                let c1 = u16::from_le_bytes([b[2], b[3]]);
                let p0 = unpack565(c0);
                let p1 = unpack565(c1);
                let palette = [p0, p1, mix_third(p0, p1, 1), mix_third(p0, p1, 2)];
                for k in 0..16 {
                    let idx = (b[4 + k / 4] >> ((k % 4) * 2)) & 0b11;
                    let i = (((by * 4 + k / 4) * pw as usize) + bx * 4 + k % 4) * 4;
                    out[i..i + 3].copy_from_slice(&palette[idx as usize]);
                    out[i + 3] = 255;
                }
            }
        }
        (pw, ph, out)
    }

    /// 逐像素平均绝对误差（RGB 三通道均值，0-255 刻度；只统计 pad 前区域）。
    fn mean_abs_error(a: &[u8], b: &[u8], width: u32, height: u32) -> f64 {
        let mut total = 0f64;
        for y in 0..height {
            for x in 0..width {
                let i = ((y * width + x) * 4) as usize;
                for c in 0..3 {
                    total += (a[i + c] as i32 - b[i + c] as i32).abs() as f64;
                }
            }
        }
        total / (width as f64 * height as f64 * 3.0)
    }

    /// 位打包布局：手工构造已知块断言字节序。像素依次为白/黑/170 灰/85 灰
    /// （循环填充 4×4）→ 端点必为白（亮，c0 = 0xFFFF）与黑（c1 = 0x0000），
    /// 调色板 = {255, 0, 170, 85} 灰阶 → 索引 0,1,2,3 每行循环 →
    /// 每字节（LSB = 像素 0 的 bit0）：00 01 10 11 → 0b1110_0100 = 0xE4。
    #[test]
    fn bit_packing_layout_matches_manual_construction() {
        let mut rgba = Vec::with_capacity(4 * 4 * 4);
        for _ in 0..4 {
            for v in [255u8, 0, 170, 85] {
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let blocks = encode_bc1(&rgba, 4, 4);
        assert_eq!(blocks.len(), 8);
        assert_eq!(&blocks[..4], &[0xff, 0xff, 0x00, 0x00], "端点应为白/黑 565");
        assert_eq!(
            &blocks[4..],
            &[0xe4, 0xe4, 0xe4, 0xe4],
            "索引应为 0,1,2,3 循环"
        );
    }

    /// 纯色块往返零误差：565 可精确表示的颜色（黑/白/三原色/三补色）
    /// 编码后解码逐位还原——纯色块端点同色、四调色板项相同、索引无歧义。
    #[test]
    fn solid_565_exact_colors_round_trip_exactly() {
        let colors: [[u8; 4]; 8] = [
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 0, 255],
            [0, 255, 255, 255],
            [255, 0, 255, 255],
        ];
        for c in colors {
            let rgba = std::iter::repeat_n(c, 16).flatten().collect::<Vec<u8>>();
            let blocks = encode_bc1(&rgba, 4, 4);
            let (_, _, decoded) = decode_bc1(&blocks, 4, 4);
            assert_eq!(decoded, rgba, "纯色 {c:?} 必须零误差往返");
        }
    }

    /// 灰度渐变块平均误差阈值：温和对角渐变（每块跨度 ~7 级），565 量化
    /// + 1/3 调色板步进的合成平均误差应 < 4/255（8-bit 刻度即 4.0）。
    #[test]
    fn gentle_gray_gradient_mean_error_below_threshold() {
        let (w, h) = (64u32, 64u32);
        let rgba: Vec<u8> = (0..h)
            .flat_map(|y| (0..w).map(move |x| (x + y) as u8))
            .flat_map(|v| [v, v, v, 255])
            .collect();
        let blocks = encode_bc1(&rgba, w, h);
        let (_, _, decoded) = decode_bc1(&blocks, w, h);
        let err = mean_abs_error(&rgba, &decoded, w, h);
        assert!(
            err < 4.0,
            "温和渐变平均误差 {err:.3} 应 < 4/255（8-bit 刻度 4.0）"
        );
    }

    /// 确定性：同输入两次编码逐位一致（端点 / 索引并列规则不依赖迭代
    /// 顺序的浮点噪声——全整数运算）。
    #[test]
    fn encoding_is_bit_identical_across_runs() {
        let (w, h) = (37u32, 23u32);
        let rgba: Vec<u8> = (0..w * h * 4)
            .map(|i| ((i * 89 + 13) % 251) as u8)
            .collect();
        let a = encode_bc1(&rgba, w, h);
        let b = encode_bc1(&rgba, w, h);
        assert_eq!(a, b, "同输入两次编码必须逐位一致");
        assert_eq!(a.len() as u64, encoded_len(w, h));
    }

    /// pad 语义：非 4 倍数尺寸内部 pad 到 4 倍数（边缘像素复制），
    /// 与手工 pad 后编码的结果逐位一致；块数为向上取整。
    #[test]
    fn non_multiple_sizes_pad_by_edge_replication() {
        let (w, h) = (5u32, 3u32);
        let src: Vec<u8> = (0..w * h * 4).map(|i| ((i * 31 + 7) % 199) as u8).collect();
        let (pw, ph) = padded_extent(w, h);
        assert_eq!((pw, ph), (8, 4));
        // 手工 pad：边缘复制（与 encode_bc1 内部同规则）
        let mut padded = Vec::with_capacity(pw as usize * ph as usize * 4);
        for y in 0..ph {
            for x in 0..pw {
                let sx = x.min(w - 1) as usize;
                let sy = y.min(h - 1) as usize;
                let i = (sy * w as usize + sx) * 4;
                padded.extend_from_slice(&src[i..i + 4]);
            }
        }
        assert_eq!(
            encode_bc1(&src, w, h),
            encode_bc1(&padded, pw, ph),
            "内部 pad 必须等价于边缘复制后编码"
        );
        assert_eq!(encode_bc1(&src, w, h).len(), 16, "2×1 块 = 16 字节");
    }

    /// 决策函数：特性 + 4 对齐 + 全不透明三条件缺一不可；空数据 /
    /// 零尺寸一律否决；半透明（alpha < 255）否决。
    #[test]
    fn bc1_eligible_requires_feature_alignment_and_opacity() {
        let opaque: Vec<u8> = vec![255; 8 * 8 * 4];
        let translucent: Vec<u8> = vec![128; 8 * 8 * 4];
        assert!(bc1_eligible(8, 8, &opaque, true));
        assert!(!bc1_eligible(8, 8, &opaque, false), "无 BC 特性必须否决");
        assert!(!bc1_eligible(7, 8, &opaque, true), "非 4 对齐必须否决");
        assert!(!bc1_eligible(8, 8, &translucent, true), "半透明必须否决");
        assert!(!bc1_eligible(0, 8, &[], true), "零尺寸必须否决");
        assert!(
            !bc1_eligible(4, 4, &[255; 4 * 4 * 3], true),
            "长度不符必须否决"
        );
        assert!(is_fully_opaque(&opaque));
        assert!(!is_fully_opaque(&translucent));
    }

    #[test]
    #[should_panic(expected = "RGBA 数据长度与纹理尺寸不一致")]
    fn encode_rejects_length_mismatch() {
        let _ = encode_bc1(&[0u8; 10], 2, 2);
    }
}
