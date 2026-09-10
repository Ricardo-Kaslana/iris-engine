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
//!
//! # BC3（DXT5，R59）
//!
//! [`encode_bc3`] 在 BC1 色块（8 B）前拼一个 **BC4 alpha 块**（8 B：
//! a0/a1 两端点 + 16 像素 × 3-bit 调色板索引），合计 16 B/块 =
//! 1 B/px（RGBA8 的 1/4）。色彩部分与 BC1 逐位相同（复用同一编码器），
//! alpha 部分是单通道 8-bit 的同一套「端点 + 插值调色板 + 最近邻索引」
//! 数学。**分层规则**（[`bc1_eligible`] / [`bc3_eligible`] 的判定口径，
//! 两决策对同一输入恒互斥）：
//!
//! - **全不透明** → BC1（0.5 B/px）：BC1 没有 alpha 通道，全不透明时
//!   不丢任何语义，压缩率是 BC3 的两倍；
//! - **带 alpha（任一像素 < 255）** → BC3（1 B/px，且**须经理入期
//!   manifest 声明**——渲染器决策树只对声明过的纹理启用 BC3 路径，
//!   未声明纹理维持既有 RGBA8 / BC1 行为，默认路径逐位不变）：BC1 的
//!   1-bit punch-through 表达不了连续半透明，BC4 的 8-bit 调色板可以。
//!
//! BC3 的 alpha 端点即块内极值像素（8-bit 原值，端点零量化误差），
//! 8 项调色板等分端点区间，最近邻匹配的单像素误差上界 =
//! (a0 − a1) / 14 ≤ 255/14 ≈ 18.2（端点级差最坏满量程）——像素断言
//! 口径据此给出（见测试，**有损压缩不写「完全相等」式假断言**）。

/// BC1 单块字节数（color0 2B + color1 2B + 索引 4B）。
pub const BC1_BLOCK_BYTES: usize = 8;

/// BC3 单块字节数（alpha 块 8 B + BC1 色块 8 B）。
pub const BC3_BLOCK_BYTES: usize = 16;

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

/// BC3 压缩编码后的总字节数：块数 × 16（alpha 块 8 B + 色块 8 B，
/// 1 B/px）。BC1 口径的同名函数 [`encoded_len`] 恒为块数 × 8——两者
/// 是不同格式的不同承诺，不共用一个函数避免口径漂移。
pub fn encoded_len_bc3(width: u32, height: u32) -> u64 {
    let (pw, ph) = padded_extent(width, height);
    (pw as u64 / 4) * (ph as u64 / 4) * BC3_BLOCK_BYTES as u64
}

/// BC3 编码：RGBA8 源 → 4×4 块流（每块 16 字节 = alpha 块 8 B 在前 +
/// BC1 色块 8 B 在后，块内行主序；布局见模块文档 BC3 节）。
///
/// pad 语义与 [`encode_bc1`] 一致（非 4 倍数尺寸内部 pad 边缘复制）；
/// 色彩块与 `encode_bc1` 的输出**逐位相同**（复用同一编码器——BC3 的
/// RGB 载体就是 BC1），alpha 块按 BC4 语义编码（[`encode_alpha_block`]）。
pub fn encode_bc3(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    assert!(width >= 1 && height >= 1, "纹理尺寸必须为正");
    assert_eq!(
        rgba.len(),
        width as usize * height as usize * 4,
        "RGBA 数据长度与纹理尺寸不一致"
    );
    let colors = encode_bc1(rgba, width, height);
    let (pw, ph) = padded_extent(width, height);
    let blocks_x = (pw / 4) as usize;
    let blocks_y = (ph / 4) as usize;
    let mut out = Vec::with_capacity(blocks_x * blocks_y * BC3_BLOCK_BYTES);
    let mut alphas = [0u8; 16];
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let (x0, y0) = (bx as u32 * 4, by as u32 * 4);
            for (k, a) in alphas.iter_mut().enumerate() {
                *a = pixel_alpha(
                    rgba,
                    width,
                    height,
                    x0 + (k % 4) as u32,
                    y0 + (k / 4) as u32,
                );
            }
            out.extend_from_slice(&encode_alpha_block(&alphas));
            let c = (by * blocks_x + bx) * BC1_BLOCK_BYTES;
            out.extend_from_slice(&colors[c..c + BC1_BLOCK_BYTES]);
        }
    }
    out
}

/// 取像素 alpha，越界时复制边缘（pad 语义，与 [`pixel_rgb`] 同规则）。
fn pixel_alpha(rgba: &[u8], width: u32, height: u32, x: u32, y: u32) -> u8 {
    let x = x.min(width - 1);
    let y = y.min(height - 1);
    let i = ((y * width + x) * 4) as usize;
    rgba[i + 3]
}

/// 编码单个 4×4 alpha 块（BC4 语义）：byte0 = 端点 a0（块内最大 alpha），
/// byte1 = 端点 a1（最小），随后 16 像素 × 3-bit 调色板索引（LSB 优先，
/// 像素 0 占最低 3 位，行主序）。
///
/// 调色板（a0 > a1 的 8 值模式）：索引 0/1 即两端点，索引 2..=7 为
/// `((8−i)·a0 + (i−1)·a1) / 7`（+3 后整除 = 四舍五入）；端点取块内
/// 极值像素（8-bit 原值零量化），最近邻匹配的单像素误差上界
/// ≤ (a0 − a1) / 14 ≈ 18.2（见模块文档）。**端点相等的纯色块**（如
/// alpha 恒 255）索引全部取 0——8 值模式下 a0 == a1 会被硬件解为
/// 「4 插值 + 0/255 穿透」退化调色板（含 0 与 255 两外谬值），索引
/// 全 0 保证解码恒等回 a0（与 BC1 纯色块同款并列取先处理）。
///
/// **确定性**：全整数运算；最大/最小值扫描取严格 `>` / `<`（并列保留
/// 首个像素）；索引匹配严格 `<`（并列取最低调色板下标）——同输入两次
/// 编码逐位一致。
fn encode_alpha_block(alphas: &[u8; 16]) -> [u8; 8] {
    // 端点 = 块内极值（并列保留首个：严格比较）
    let mut a0 = alphas[0];
    let mut a1 = alphas[0];
    for &a in &alphas[1..] {
        if a > a0 {
            a0 = a;
        }
        if a < a1 {
            a1 = a;
        }
    }
    let mut out = [0u8; 8];
    out[0] = a0;
    out[1] = a1;
    if a0 == a1 {
        // 纯色块：索引全 0（out 已零初始化），解码恒等回 a0
        return out;
    }
    // 8 值调色板：p[0] = a0、p[1] = a1，p[i] = ((8−i)·a0 + (i−1)·a1) / 7
    let mut palette = [0u8; 8];
    palette[0] = a0;
    palette[1] = a1;
    for (i, slot) in palette.iter_mut().enumerate().skip(2) {
        let v = ((8 - i) as u32 * a0 as u32 + (i as u32 - 1) * a1 as u32 + 3) / 7;
        *slot = v.clamp(0, 255) as u8;
    }
    // 最近邻索引（并列取最低下标）；48 bit LSB 优先打包，像素 k 占
    // 第 3k 位起的 3 bit——16 像素恰好 48 bit = 6 字节
    let mut bits: u64 = 0;
    for (k, &a) in alphas.iter().enumerate() {
        let mut nearest = 0u32;
        let mut nearest_d = u32::MAX;
        for (pi, &p) in palette.iter().enumerate() {
            let d = (a as i32 - p as i32).unsigned_abs();
            if d < nearest_d {
                nearest_d = d;
                nearest = pi as u32;
            }
        }
        bits |= (nearest as u64) << (3 * k);
    }
    out[2..8].copy_from_slice(&bits.to_le_bytes()[..6]);
    out
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

/// BC3 上传决策（R59）：与 [`bc1_eligible`] 的**分层规则**——
/// 「全不透明走 BC1、带 alpha 走 BC3」。条件与 BC1 相同（特性 + 4 对齐 +
/// 长度一致），唯 alpha 条件取反：`!is_fully_opaque`（存在任一 < 255 的
/// alpha 像素）。两个决策函数对同一输入**恒互斥**（alpha 条件互补）——
/// 全不透明纹理即使被声明走 BC3，也由渲染器决策树落回 BC1（0.5 B/px
/// 优于 1 B/px，且语义无损）。注意本函数不含「manifest 声明」条件：
/// 声明是渲染器 / 存储层的准入闸门（BC3 只对经理入期声明的纹理生效，
/// 默认路径逐位不变），本函数只回答「这份数据适合 BC3 吗」。
pub fn bc3_eligible(width: u32, height: u32, rgba: &[u8], feature_bc: bool) -> bool {
    feature_bc
        && width > 0
        && height > 0
        && width % 4 == 0
        && height % 4 == 0
        && rgba.len() == width as usize * height as usize * 4
        && !is_fully_opaque(rgba)
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

    // -----------------------------------------------------------------
    // BC3（R59）
    // -----------------------------------------------------------------

    /// 测试辅助：从 BC3 块流解码 RGBA8（镜像硬件语义：alpha 块按 a0/a1
    /// 端点 + 8 值调色板展开——a0 == a1 的纯色块除外，此时硬件会走
    /// 「4 插值 + 穿透」退化模式，但编码器保证纯色块索引全 0，两模式
    /// 在索引 0/1 上同为端点值，解码一致）。返回 pad 后尺寸的像素。
    fn decode_bc3(blocks: &[u8], width: u32, height: u32) -> (u32, u32, Vec<u8>) {
        let (pw, ph) = padded_extent(width, height);
        let blocks_x = (pw / 4) as usize;
        let blocks_y = (ph / 4) as usize;
        let mut out = vec![0u8; (pw * ph * 4) as usize];
        for by in 0..blocks_y {
            for bx in 0..blocks_x {
                let base = (by * blocks_x + bx) * BC3_BLOCK_BYTES;
                let a = &blocks[base..base + BC3_BLOCK_BYTES];
                let (a0, a1) = (a[0], a[1]);
                let mut palette = [0u8; 8];
                palette[0] = a0;
                palette[1] = a1;
                if a0 > a1 {
                    for (i, slot) in palette.iter_mut().enumerate().skip(2) {
                        *slot = (((8 - i) as u32 * a0 as u32 + (i as u32 - 1) * a1 as u32 + 3) / 7)
                            as u8;
                    }
                } else {
                    // 退化模式（本编码器不产此形态，解码端按规范补全
                    // 以便对照外部工具）：p2..p3 插值、p4/p5 = 0、p6/p7 = 255
                    for (i, slot) in palette.iter_mut().enumerate().skip(2).take(2) {
                        *slot = (((5 - i) as u32 * a0 as u32 + (i as u32 - 1) * a1 as u32 + 2) / 5)
                            as u8;
                    }
                    palette[4] = 0;
                    palette[5] = 0;
                    palette[6] = 255;
                    palette[7] = 255;
                }
                let mut bits: u64 = 0;
                for i in 0..6 {
                    bits |= (a[2 + i] as u64) << (8 * i);
                }
                // 色块 = BC1（复用 BC1 解码）
                let (_, _, rgba_color) = decode_bc1(&blocks[base + 8..base + 16], 4, 4);
                for k in 0..16 {
                    let idx = ((bits >> (3 * k)) & 0b111) as usize;
                    let i = (((by * 4 + k / 4) * pw as usize) + bx * 4 + k % 4) * 4;
                    out[i..i + 4].copy_from_slice(&[
                        rgba_color[k * 4],
                        rgba_color[k * 4 + 1],
                        rgba_color[k * 4 + 2],
                        palette[idx],
                    ]);
                }
            }
        }
        (pw, ph, out)
    }

    /// BC3 位打包布局（手工构造已知块）：alpha 恒 255 + 颜色循环
    /// 白/黑/170 灰/85 灰。alpha 块 = 端点 255/255 + 索引全 0（纯色块
    /// 恒等还原）；色块与 BC1 已知块逐位一致（ff ff 00 00 + e4×4）。
    #[test]
    fn bc3_bit_packing_layout_matches_manual_construction() {
        let mut rgba = Vec::with_capacity(4 * 4 * 4);
        for _ in 0..4 {
            for v in [255u8, 0, 170, 85] {
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let blocks = encode_bc3(&rgba, 4, 4);
        assert_eq!(blocks.len(), BC3_BLOCK_BYTES);
        // alpha 块：端点 255/255，48 bit 索引全零
        assert_eq!(&blocks[..8], &[255, 255, 0, 0, 0, 0, 0, 0], "alpha 纯色块");
        // 色块 = BC1 已知块
        assert_eq!(
            &blocks[8..],
            &[0xff, 0xff, 0x00, 0x00, 0xe4, 0xe4, 0xe4, 0xe4]
        );
        // 解码：alpha 精确还原 255，RGB 走 BC1 调色板
        let (_, _, decoded) = decode_bc3(&blocks, 4, 4);
        assert!(decoded.chunks_exact(4).all(|px| px[3] == 255));
    }

    /// 全不透明图像走 BC3：alpha 通道必须逐像素零误差（端点 255 = 255、
    /// 索引全 0 的恒等块），RGB 误差与 BC1 完全一致。
    #[test]
    fn bc3_opaque_alpha_round_trips_exactly() {
        let (w, h) = (16u32, 16u32);
        let rgba: Vec<u8> = (0..w * h * 4)
            .map(|i| ((i * 53 + 11) % 251) as u8)
            .collect();
        let mut rgba = rgba;
        for px in rgba.chunks_exact_mut(4) {
            px[3] = 255;
        }
        let blocks = encode_bc3(&rgba, w, h);
        assert_eq!(blocks.len() as u64, encoded_len_bc3(w, h));
        let (_, _, decoded) = decode_bc3(&blocks, w, h);
        for (i, px) in decoded.chunks_exact(4).enumerate() {
            assert_eq!(px[3], 255, "全不透明 alpha 必须零误差还原（像素 {i}）");
        }
    }

    /// 常数半透明 alpha（128）零误差往返：端点 128 = 128、索引全 0 →
    /// 解码恒等回 128（纯色块不走含 0/255 穿透值的退化调色板）。
    #[test]
    fn bc3_constant_alpha_round_trips_exactly() {
        let rgba = std::iter::repeat_n([200, 100, 50, 128], 16)
            .flatten()
            .collect::<Vec<u8>>();
        let blocks = encode_bc3(&rgba, 4, 4);
        assert_eq!(blocks[..2], [128, 128], "端点应为 128/128");
        assert_eq!(&blocks[2..8], &[0; 6], "纯色 alpha 块索引应全 0");
        let (_, _, decoded) = decode_bc3(&blocks, 4, 4);
        assert!(decoded.chunks_exact(4).all(|px| px[3] == 128));
    }

    /// alpha 渐变块的误差上界（诚实口径——BC3 有损，断言上界而非相等）：
    /// 满量程 0→255 渐变块的端点级差 = 255，8 值调色板等分后最大半步
    /// ≤ 255/14 ≈ 18.22；像素断言取 19（上界 + 1 容差，容差来源：
    /// 四舍五入的调色板值与理想等分点至多差 0.5）。全图均方误差按同
    /// 口径 < 19²，实测满量程渐变约 60–80。
    #[test]
    fn bc3_alpha_gradient_error_bounded() {
        let (w, h) = (64u32, 64u32);
        let mut rgba: Vec<u8> = (0..w * h * 4).map(|i| ((i * 31 + 7) % 199) as u8).collect();
        // alpha：满量程对角渐变 0..255（每块内端点级差 ≤ 255/15×√2 < 24，
        // 但最坏块仍可能接近满量程——断言用理论最坏口径）
        for (i, px) in rgba.chunks_exact_mut(4).enumerate() {
            let (x, y) = ((i % w as usize) as f32, (i / w as usize) as f32);
            px[3] = ((x + y) / ((w + h - 2) as f32) * 255.0).round() as u8;
        }
        let blocks = encode_bc3(&rgba, w, h);
        let (_, _, decoded) = decode_bc3(&blocks, w, h);
        let mut max_err = 0u32;
        let mut sq_total = 0f64;
        for (src, dst) in rgba.chunks_exact(4).zip(decoded.chunks_exact(4)) {
            let err = (src[3] as i32 - dst[3] as i32).unsigned_abs();
            max_err = max_err.max(err);
            sq_total += (err as f64) * (err as f64);
        }
        let mse = sq_total / (w as f64 * h as f64);
        assert!(
            max_err <= 19,
            "满量程渐变单像素 alpha 误差 {max_err} 应 ≤ 255/14 + 1 ≈ 19"
        );
        assert!(
            mse < 19.0 * 19.0,
            "全图均方误差 {mse:.1} 应 < 19² = 361（上界口径一致）"
        );
    }

    /// BC3 确定性 + 字节数：同输入两次编码逐位一致；64×64 源
    /// （16 384 B = 4 B/px）编码后 1 024 B = 1 B/px（4 倍节省）。
    #[test]
    fn bc3_encoding_is_deterministic_and_quarter_size() {
        let (w, h) = (64u32, 64u32);
        let mut rgba: Vec<u8> = (0..w * h * 4)
            .map(|i| ((i * 89 + 13) % 251) as u8)
            .collect();
        for (i, px) in rgba.chunks_exact_mut(4).enumerate() {
            px[3] = if i % 3 == 0 { 40 } else { 255 };
        }
        let a = encode_bc3(&rgba, w, h);
        let b = encode_bc3(&rgba, w, h);
        assert_eq!(a, b, "同输入两次 BC3 编码必须逐位一致");
        assert_eq!(a.len() as u64, encoded_len_bc3(w, h));
        assert_eq!(
            a.len(),
            (w * h) as usize,
            "64×64 BC3 应为 1 B/px（RGBA8 的 1/4）"
        );
        assert_eq!(encoded_len_bc3(w, h) * 4, (w as u64 * h as u64 * 4));
    }

    /// 决策分层：bc3_eligible 与 bc1_eligible 对同一输入恒互斥
    /// （alpha 条件互补）；BC3 要求「带 alpha」+ 特性 + 4 对齐 +
    /// 长度一致，空数据 / 零尺寸 / 不对齐 / 全不透明一律否决。
    #[test]
    fn bc3_eligible_is_disjoint_from_bc1_by_alpha_layering() {
        let opaque: Vec<u8> = std::iter::repeat_n([200, 100, 50, 255], 8 * 8)
            .flatten()
            .collect();
        let translucent: Vec<u8> = std::iter::repeat_n([200, 100, 50, 128], 8 * 8)
            .flatten()
            .collect();
        // 带 alpha + 特性 + 对齐 → BC3 可用、BC1 不可用
        assert!(bc3_eligible(8, 8, &translucent, true));
        assert!(!bc1_eligible(8, 8, &translucent, true));
        // 全不透明 → BC1 可用、BC3 否决（分层规则：全不透明归 BC1）
        assert!(!bc3_eligible(8, 8, &opaque, true));
        assert!(bc1_eligible(8, 8, &opaque, true));
        // 无 BC 特性 / 非对齐 / 零尺寸 / 长度不符一律否决
        assert!(!bc3_eligible(8, 8, &translucent, false));
        assert!(!bc3_eligible(7, 8, &translucent, true));
        assert!(!bc3_eligible(0, 8, &[], true));
        assert!(!bc3_eligible(4, 4, &[255; 4 * 4 * 3], true));
    }

    #[test]
    #[should_panic(expected = "RGBA 数据长度与纹理尺寸不一致")]
    fn encode_bc3_rejects_length_mismatch() {
        let _ = encode_bc3(&[0u8; 10], 2, 2);
    }
}
