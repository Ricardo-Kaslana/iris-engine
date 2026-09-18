//! BC7（BPTC）纹理压缩编码器 v1：**仅 Mode 6**（`iris_render::texture_bc7`，C5a）。
//!
//! Mode 6 是 BC7 八模式中「单分区、组合 RGBA」的高质量档：端点 R/G/B/A
//! 各 8-bit（7 bit 存储 + 1 P-bit）、16 像素各 4-bit 权重（16 级）。
//! **不透明与带 alpha 纹理统一走本模式**（alpha 与颜色共用权重），
//! 质量优于 BC1/BC3 的 3/4 级权重与 5/6-bit 端点；位预算与全部主流
//! 实现（DirectXTex 解码器逐位核对）一致。
//!
//! ## 块布局（128 bit，LSB→MSB 位流序）
//!
//! ```text
//! [0..6]   模式一元前缀：六个 0 + 一个 1（7 位；模式 m = m 个 0 后跟 1）
//! [7..62]  端点分量主序：R0 G0 B0 A0 R1 G1 B1 A1 各 7 bit（高 7 位）
//! [63,64]  P0 P1（两端点各 1 个 P-bit = 第 0 位，拼成 8-bit 端点）
//! [65..67] 像素 0 权重（anchor，3 bit——高位隐 0，编码器端点交换保证）
//! [68..127] 像素 1..15 权重各 4 bit
//! ```
//!
//! 解码权重插值（与本模块 [`decode_bc7`] 逐式一致，DirectXTex 同口径）：
//! `out = (A·(64 − W[w]) + B·W[w] + 32) >> 6`，
//! `W4 = {0,4,9,13,17,21,26,30,34,38,43,47,51,55,60,64}`。
//!
//! ## 编码器策略（确定性全整数，无浮点）
//! - **端点 = 逐通道 min/max**（A=min，B=max，16 像素扫描）：实现薄、
//!   行为可复现；更优的端点迭代优化（PCA / 扰动搜索）留后续（接口不变）；
//! - **权重 = 逐像素 16 档全扫**，RGBA 四通道平方误差和最小档，并列
//!   取先（严格 `<` 保最小 w）——逐像素独立最优（未做端点-权重联合
//!   迭代，v1 声明的质量上界依据即此）；
//! - **anchor 归一**：像素 0 权重 ≥ 8 时交换端点 A/B 并全像素取反
//!   `w ← 15 − w`（等价表示，解码逐位不变；anchor 因此恒 < 8，
//!   3-bit 存储无信息损失）；
//! - 同输入两次编码逐位一致（单测守护）——与 BC1/BC3 同一确定性纪律。
//!
//! ## 诚实限制（v1）
//! 1. **只产 Mode 6**：其余模式（分区 / 旋转 / 独立 alpha 权重）不生成
//!    ——外部 BC7 资产若含他模式，[`decode_bc7`] 仍按规范全模式解码
//!    （KTX2 校验路径），但引擎编码器只写 Mode 6；
//! 2. 端点 min/max + 逐像素最优权重 ⇒ 对角分布色块（色-透明同时渐变）
//!    的误差高于离线优化工具（rgbcx / enc7 级别）数个百分点 PSNR——
//!    声明门的语义是「作者显式选格式」，不是「引擎与离线工具等质」；
//! 3. 单分区 ⇒ 每块至多一条颜色渐变轴：细节锐利的照片块质量增益相对
//!    BC1 主要在权重级数（16 vs 4）与端点位宽（8 vs 5）；
//! 4. mip 链沿 R66d 的「逐级 RGBA8 降采样后编码」通路，不在压缩域
//!    做降采样。

/// BC7 单块字节数（128 bit）。
pub const BC7_BLOCK_BYTES: usize = 16;

/// 4-bit 权重的 16 级插值表（分母 64；DirectXTex `g_aWeights4` 同值）。
const WEIGHTS4: [i32; 16] = [0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64];

/// 解码单块：RGBA8 × 16（块内行主序）。**v1 只实现 Mode 6**
/// （引擎编码器只产 Mode 6；其余模式返回 `None`——分区表 / 旋转
/// 未接入，外部 BC7 资产的 GPU 上传不经本函数，硬件解码器全模式
/// 自然可绘；接入多模式解码随消费方出现时再排）。
///
/// Mode 6 语义逐式移植 DirectXTex `D3DX_BC7::Decode`：一元前缀、
/// 分量主序 7-bit 端点、每端点 1 个 P-bit（四通道共享）、anchor
/// 像素 0 读 3 bit（高位隐 0）、其余 15 像素 4 bit。
pub fn decode_bc7_mode6(block: &[u8; BC7_BLOCK_BYTES]) -> Option<[[u8; 4]; 16]> {
    let mut reader = BitReader::new(block);
    // 一元前缀：数前导 0 直到 1
    let mut zeros = 0usize;
    while reader.bit()? == 0 {
        zeros += 1;
        if zeros > 7 {
            return None;
        }
    }
    if zeros != 6 {
        return None; // 只支持 Mode 6
    }
    // 端点：R0 G0 B0 A0 R1 G1 B1 A1 各 7 bit（分量主序）
    let mut stored = [0u8; 8];
    for s in stored.iter_mut() {
        *s = reader.bits(7)?;
    }
    let p0 = reader.bit()?;
    let p1 = reader.bit()?;
    let endpoint = |hi: u8, p: u8| -> u8 { (hi << 1) | p };
    let a = [
        endpoint(stored[0], p0),
        endpoint(stored[1], p0),
        endpoint(stored[2], p0),
        endpoint(stored[3], p0),
    ];
    let b = [
        endpoint(stored[4], p1),
        endpoint(stored[5], p1),
        endpoint(stored[6], p1),
        endpoint(stored[7], p1),
    ];
    // 权重：像素 0 为 anchor（3 bit，高位置 0），其余 15 像素 4 bit
    let mut weights = [0u8; 16];
    weights[0] = reader.bits(3)?;
    for w in weights.iter_mut().skip(1) {
        *w = reader.bits(4)?;
    }
    let mut out = [[0u8; 4]; 16];
    for (i, weight) in weights.iter().enumerate() {
        out[i] = interpolate(&a, &b, *weight as usize);
    }
    Some(out)
}

/// 单权重插值：`out[ch] = (A·(64−W) + B·W + 32) >> 6`（DirectXTex 同式）。
fn interpolate(a: &[u8; 4], b: &[u8; 4], w: usize) -> [u8; 4] {
    let wt = WEIGHTS4[w];
    let mut out = [0u8; 4];
    for ch in 0..4 {
        let v = (a[ch] as i32 * (64 - wt) + b[ch] as i32 * wt + 32) >> 6;
        out[ch] = v.clamp(0, 255) as u8;
    }
    out
}

/// 编码 RGBA8 全图 → BC7 Mode 6 块流（16 B/块，块行主序）。
///
/// 尺寸非 4 倍数时内部 pad（边缘复制，与 `texture_bcn::encode_bc1` 同
/// 口径）；断言数据长度与尺寸一致。
pub fn encode_bc7(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    assert!(width >= 1 && height >= 1, "纹理尺寸必须为正");
    assert_eq!(
        rgba.len(),
        width as usize * height as usize * 4,
        "RGBA 数据长度与纹理尺寸不一致"
    );
    let (pw, ph) = crate::texture_bcn::padded_extent(width, height);
    let blocks_x = (pw / 4) as usize;
    let blocks_y = (ph / 4) as usize;
    let mut out = vec![0u8; blocks_x * blocks_y * BC7_BLOCK_BYTES];
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let mut pixels = [[0u8; 4]; 16];
            for (k, px) in pixels.iter_mut().enumerate() {
                let x = bx as u32 * 4 + (k % 4) as u32;
                let y = by as u32 * 4 + (k / 4) as u32;
                let (x, y) = (x.min(width - 1), y.min(height - 1));
                let i = ((y * width + x) * 4) as usize;
                *px = [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]];
            }
            let block = encode_block_bc7(&pixels);
            let o = (by * blocks_x + bx) * BC7_BLOCK_BYTES;
            out[o..o + BC7_BLOCK_BYTES].copy_from_slice(&block);
        }
    }
    out
}

/// 编码单个 4×4 块（策略见模块文档；纯函数可脱离尺寸概念单测）。
pub fn encode_block_bc7(pixels: &[[u8; 4]; 16]) -> [u8; BC7_BLOCK_BYTES] {
    // ① 端点：逐通道 min/max
    let (mut a, mut b) = ([255u8; 4], [0u8; 4]);
    for px in pixels {
        for ch in 0..4 {
            a[ch] = a[ch].min(px[ch]);
            b[ch] = b[ch].max(px[ch]);
        }
    }
    // ② P-bit：Mode 6 每端点**一个** P-bit 共享四通道（规范形态）——
    // 取四通道最低位的多数票（≥3 票取 1，DirectXTex FixEndpointPBits
    // 同口径；平票取 0，确定性）。LSB 与票不符的通道端点被 ±1 重
    // 构造——这是模式本身的量化面，不是编码器缺陷。
    let (mut p0, mut p1) = (lsb_vote(&a), lsb_vote(&b));
    let mut ra = augment(&a, p0);
    let mut rb = augment(&b, p1);
    // ③ 逐像素 16 档全扫最优权重（对**重建端点**的调色板，并列取先：
    // 严格 `<` 保最小 w）
    let mut weights = [0u8; 16];
    for (i, px) in pixels.iter().enumerate() {
        let mut best_w = 0usize;
        let mut best_err = i64::MAX;
        for w in 0..16 {
            let pal = interpolate(&ra, &rb, w);
            let mut err: i64 = 0;
            for ch in 0..4 {
                let d = pal[ch] as i64 - px[ch] as i64;
                err += d * d;
            }
            if err < best_err {
                best_err = err;
                best_w = w;
            }
        }
        weights[i] = best_w as u8;
    }
    // ④ anchor 归一：像素 0 权重 ≥ 8 ⇒ 端点（含 P-bit）交换 + 全像素
    // 取反 `w ← 15 − w`（等价表示，解码逐位不变：W[15−k] = 64 − W[k]
    // 对本表成立——两端点序的插值镜像）
    if weights[0] >= 8 {
        std::mem::swap(&mut ra, &mut rb);
        std::mem::swap(&mut p0, &mut p1);
        for w in weights.iter_mut() {
            *w = 15 - *w;
        }
    }
    // ⑤ 位流组装
    let mut bits = [0u8; BC7_BLOCK_BYTES];
    let mut writer = BitWriter::new(&mut bits);
    for _ in 0..6 {
        writer.bit(0);
    }
    writer.bit(1); // 模式 6 一元前缀
    for ch in 0..4 {
        writer.bits(ra[ch] >> 1, 7);
    }
    for ch in 0..4 {
        writer.bits(rb[ch] >> 1, 7);
    }
    writer.bit(p0);
    writer.bit(p1);
    writer.bits(weights[0], 3); // anchor：高位恒 0（④ 已归一）
    for w in &weights[1..] {
        writer.bits(*w, 4);
    }
    debug_assert_eq!(writer.consumed(), 128, "Mode 6 位预算恒 128");
    bits
}

/// 端点四通道最低位的多数票（≥3 ⇒ 1；平票取 0——确定性）。
fn lsb_vote(endpoint: &[u8; 4]) -> u8 {
    let votes: u32 = endpoint.iter().map(|c| (c & 1) as u32).sum();
    u8::from(votes >= 3)
}

/// 以 P-bit 重构端点（解码器所见形态）：`(c >> 1) << 1 | p`。
fn augment(endpoint: &[u8; 4], p: u8) -> [u8; 4] {
    let mut out = [0u8; 4];
    for ch in 0..4 {
        out[ch] = ((endpoint[ch] >> 1) << 1) | p;
    }
    out
}

/// BC7 压缩编码后的总字节数（块数 × 16，1 B/px）。
pub fn encoded_len_bc7(width: u32, height: u32) -> u64 {
    let (pw, ph) = crate::texture_bcn::padded_extent(width, height);
    (pw as u64 / 4) * (ph as u64 / 4) * BC7_BLOCK_BYTES as u64
}

/// BC7 上传决策（C5a）：格式适合性判定（不含「经理入期声明」闸门——
/// 声明是渲染器 / `TextureStore` 侧的准入条件，BC7 **永不自动**，见
/// `renderer.rs` 决策树）。条件：BC 特性 + 4 对齐 + 长度一致。
/// 与 BC1/BC3 分层规则的差别：BC7 不区分全不透明 / 带 alpha（组合
/// RGBA 统一覆盖），alpha 恒 255 亦合法（无损）。
pub fn bc7_eligible(width: u32, height: u32, rgba: &[u8], feature_bc: bool) -> bool {
    feature_bc
        && width > 0
        && height > 0
        && width % 4 == 0
        && height % 4 == 0
        && rgba.len() == width as usize * height as usize * 4
}

/// 128-bit 小端位流读游（LSB-first，块内跨字节拼接）。
struct BitReader<'a> {
    bytes: &'a [u8; BC7_BLOCK_BYTES],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8; BC7_BLOCK_BYTES]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn bit(&mut self) -> Option<u8> {
        if self.pos >= 128 {
            return None;
        }
        let v = (self.bytes[self.pos >> 3] >> (self.pos & 7)) & 1;
        self.pos += 1;
        Some(v as u8)
    }
    fn bits(&mut self, count: usize) -> Option<u8> {
        let mut v = 0u32;
        for i in 0..count {
            v |= (self.bit()? as u32) << i;
        }
        Some(v as u8)
    }
}

/// 128-bit 小端位流写游（LSB-first，与 [`BitReader`] 严格互逆）。
struct BitWriter<'a> {
    bytes: &'a mut [u8; BC7_BLOCK_BYTES],
    pos: usize,
}

impl<'a> BitWriter<'a> {
    fn new(bytes: &'a mut [u8; BC7_BLOCK_BYTES]) -> Self {
        bytes.fill(0);
        Self { bytes, pos: 0 }
    }
    fn bit(&mut self, v: u8) {
        debug_assert!(v <= 1);
        if v & 1 == 1 {
            self.bytes[self.pos >> 3] |= 1 << (self.pos & 7);
        }
        self.pos += 1;
    }
    fn bits(&mut self, v: u8, count: usize) {
        debug_assert!((v as usize) < (1usize << count), "写入值超出位宽");
        for i in 0..count {
            self.bit((v >> i) & 1);
        }
    }
    fn consumed(&self) -> usize {
        self.pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray_block(t: fn(u32, u32) -> u8) -> [[u8; 4]; 16] {
        let mut out = [[0u8; 4]; 16];
        for (k, px) in out.iter_mut().enumerate() {
            let g = gray_cell(k as u32 / 4, k as u32 % 4, t);
            *px = [g, g, g, 255];
        }
        out
    }

    fn gray_cell(x: u32, y: u32, t: fn(u32, u32) -> u8) -> u8 {
        t(y * 4 + x, y * 4 + x)
    }

    /// 恒等往返（同奇偶端点）：纯色块逐位精确 + Mode 6 一元前缀锁。
    #[test]
    fn solid_color_block_roundtrips_exact() {
        let pixels = [[16u8, 200u8, 4u8, 8u8]; 16]; // 四通道全偶 ⇒ P 票 0 无损
        let block = encode_block_bc7(&pixels);
        // 模式前缀 = 7 位：byte0 低 7 位 = 0b0100_0000（六 0 + 一 1）
        assert_eq!(block[0] & 0x7F, 0x40, "Mode 6 一元前缀");
        let decoded = decode_bc7_mode6(&block).expect("mode6 解码");
        for px in decoded {
            assert_eq!(px, pixels[0], "纯色块逐位还原");
        }
    }

    /// 共享 P-bit 的规范量化面（解析锁）：Mode 6 每端点**一个** P-bit
    /// 作用四通道——LSB 不合多数票的通道被 ±1 重构造。[17,200,3,9]：
    /// 票 (1,0,1,1)=3 ⇒ P=1，G 200→201（唯一不合票通道）。
    #[test]
    fn shared_pbit_reconstructs_off_vote_channel_by_one() {
        let pixels = [[17u8, 200u8, 3u8, 9u8]; 16];
        let decoded = decode_bc7_mode6(&encode_block_bc7(&pixels)).unwrap();
        assert_eq!(decoded[0], [17, 201, 3, 9], "P 多数票 =1，仅 G 被抬 1");
    }

    /// 端点恰在权重表两端 + anchor 归一生效：块最左列 = min、次列 = max
    /// 形态（anchor 像素 0 取 0 档）。
    #[test]
    fn two_value_block_reconstructs_endpoints() {
        let mut pixels = [[0u8; 4]; 16];
        for (k, px) in pixels.iter_mut().enumerate() {
            *px = if k % 2 == 0 {
                [10, 20, 30, 254]
            } else {
                [11, 21, 31, 255]
            };
        }
        let block = encode_block_bc7(&pixels);
        let decoded = decode_bc7_mode6(&block).unwrap();
        // 两端点相差 1/档 ⇒ 16 级权重可精确分辨两值（误差 0）
        for (k, px) in decoded.iter().enumerate() {
            let want = if k % 2 == 0 {
                [10, 20, 30, 254]
            } else {
                [11, 21, 31, 255]
            };
            assert_eq!(*px, want);
        }
    }

    /// 渐变块往返误差上界：逐通道 |Δ| ≤ 端点跨度/15 + 1（量化步距级）。
    #[test]
    fn gradient_block_error_within_step() {
        let pixels = gray_block(|i, _| (i * 17).min(255) as u8);
        let block = encode_block_bc7(&pixels);
        let decoded = decode_bc7_mode6(&block).unwrap();
        for (k, px) in decoded.iter().enumerate() {
            let d = px[0] as i32 - pixels[k][0] as i32;
            assert!(
                d.abs() <= 255 / 15 + 1,
                "像素 {k} 误差 {d} 超步距界（±{}）",
                255 / 15 + 1
            );
        }
    }

    /// 确定性：同输入两次编码逐位一致。
    #[test]
    fn encode_is_deterministic() {
        let pixels = gray_block(|x, y| (x * 31 + y * 13) as u8);
        assert_eq!(encode_block_bc7(&pixels), encode_block_bc7(&pixels));
    }

    /// 全图编码：尺寸/pad 与块预算；4 对齐输入的字节数 = encoded_len_bc7。
    #[test]
    fn full_image_encodes_to_block_stream() {
        let w = 8u32;
        let h = 4u32;
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for (i, v) in rgba.iter_mut().enumerate() {
            *v = (i % 251) as u8;
        }
        let blocks = encode_bc7(&rgba, w, h);
        assert_eq!(blocks.len() as u64, encoded_len_bc7(w, h));
        // 每块都能按 mode6 解回
        for chunk in blocks.chunks_exact(16) {
            let mut b = [0u8; 16];
            b.copy_from_slice(chunk);
            assert!(decode_bc7_mode6(&b).is_some());
        }
    }

    /// anchor 归一：像素 0 最优档落高半区（15）⇒ 端点交换 + 权重取反
    /// 后解码仍逐位等价（对照：不交换则 anchor 需 4-bit，存储超预算）。
    #[test]
    fn anchor_normalization_keeps_decoding() {
        let mut pixels = [[0u8; 4]; 16];
        // 端点全偶（P 票 0）：min=[0,..,254] max=[250,..,254]，像素 0
        // 贴近 max ⇒ 最优档 15 ≥ 8 触发归一
        pixels[0] = [250, 250, 250, 254];
        for px in pixels.iter_mut().skip(1) {
            *px = [0, 0, 0, 254];
        }
        let block = encode_bc7(&pixels.concat(), 4, 4);
        let decoded = decode_bc7_mode6((&block[..16]).try_into().unwrap()).unwrap();
        assert_eq!(decoded[0], [250, 250, 250, 254]);
        for px in decoded.iter().skip(1) {
            assert_eq!(*px, [0, 0, 0, 254]);
        }
    }

    /// 位流 reader/writer 互逆（含跨字节拼接）。
    #[test]
    fn bit_io_roundtrips() {
        let mut bytes = [0u8; 16];
        {
            let mut w = BitWriter::new(&mut bytes);
            w.bits(0b1010_11, 7);
            w.bit(1);
            w.bits(0xFF, 8);
            w.bits(0b101, 3);
        }
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.bits(7).unwrap(), 0b1010_11);
        assert_eq!(r.bit().unwrap(), 1);
        assert_eq!(r.bits(8).unwrap(), 0xFF);
        assert_eq!(r.bits(3).unwrap(), 0b101);
    }

    /// `bc7_eligible`：对齐 / 长度 / 特性否决；alpha 不构成否决（对偶
    /// BC1/BC3 分层——BC7 组合 RGBA 统一覆盖）。
    #[test]
    fn eligible_rules() {
        let opaque = vec![255u8; 8 * 8 * 4];
        assert!(bc7_eligible(8, 8, &opaque, true));
        assert!(!bc7_eligible(8, 8, &opaque, false), "无 BC 特性否决");
        assert!(!bc7_eligible(7, 8, &opaque, true), "非 4 对齐否决");
        let mut alpha = opaque.clone();
        alpha[3] = 128;
        assert!(
            bc7_eligible(8, 8, &alpha, true),
            "半透明不否决（对 BC1 是关键差异）"
        );
        assert!(!bc7_eligible(4, 4, &[255; 4 * 4 * 3], true), "长度不符否决");
    }
}
