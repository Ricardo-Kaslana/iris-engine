//! KTX 2.0 容器解析（C5a-2）：**仅未压缩载荷**的 2D 单面 BC 纹理
//! （`iris_render::texture_ktx2`，闭合战役格式役）。
//!
//! 支持面（用户裁决定稿 2026-09-18 批次五 → 2026-09-19 定案）：
//! - `supercompressionScheme == 0`（KtxNone）：块数据原样直读；
//!   **BasisLZ(1) / Zstd(2) / ZLIB(3) 拒收**（超压缩需引入解码依赖，
//!   与本仓「零新依赖 + 确定性」取向相悖，随消费方出现再议）；
//! - vkFormat 白名单：BC1 RGBA（133/134）、BC3（137/138）、
//!   **BC7（145/146）**——数值以 Khronos `vk.xml` 为准（枚举定义非
//!   连续算术，勿凭记忆推算）；
//! - 2D、`faceCount == 1`、`layerCount ≤ 1`、`typeSize == 1`、
//!   `pixelDepth ≤ 1`（3D / 数组 / 立方贴图拒收）；
//! - mip 全级直载：`levels[p] = {byteOffset, byteLength,
//!   uncompressedByteLength}` 未压缩时后两者相等，块数据原样进
//!   GPU（这正是 KTX2 的存在意义——**预压缩资产免运行时编码**，
//!   与 `texture_bc7` 的引擎编码路径互补）。
//!
//! 头部布局（规范 §File Structure，小端）：12 B identifier +
//! 十一个 u32（vkFormat…supercompressionScheme）+ 六项 Index
//! （dfd/kvd 各 u32×2、sgd u64×2）= **定长 80 B**，随后是
//! `max(1, levelCount)` 条 level index（3×u64 = 24 B/条）。解析只做
//! 偏移与校验，不消费 dfd/kvd/sgd（v1 不读 DFD——格式判定以
//! vkFormat 为唯一事实源；不读 KVD/OGA 元数据）。
//!
//! ## 诚实限制
//! 1. 超压缩 / ETC2 / ASTC / 未压缩 vkFormat（R8G8B8A8 等）全拒收：
//!    引擎 BC 决策树只产/吃 BC1/BC3/BC7，未压缩纹理走 `CpuTexture`
//!    常规路径（PNG/JPEG 解码），KTX2 的未压缩形态无消费场景；
//! 2. 立方贴图 / 纹理数组 / 3D 拒收（`faceCount > 1` 等）；
//! 3. DFD（Data Format Descriptor）不校验——预压缩资产的正确性由
//!    **GPU 上传成功 + 采样验证**兜底（`ktx2_render.rs`），引擎不做
//!    双源描述一致性检查（v1 取舍：DFD 解析 ≈ 半個数据格式规范）；
//! 4. 无 BC 特性的设备（web / 老硬件）：已上传的 KTX2 纹理无
//!    RGBA8 回退数据 ⇒ 渲染器绑白色兜底并告警（区别于编码路径的
//!    自动回退——那是「有源数据」的奢侈品）；
//! 5. `levelCount == 0`（生成 mip 的授权形态）拒收：块压缩格式规范
//!    本就要求 ≥1（消费方自行离线生成 mip 再入容器）。

/// KTX2 文件魔数（规范 §identifier：`«KTX 20»` + CR LF SUB CTRL LF）。
pub const KTX2_IDENTIFIER: [u8; 12] = [
    0xAB, b'K', b'T', b'X', b' ', b'2', b'0', 0xBB, 0x0D, 0x0A, 0x1A, 0x0A,
];

/// 定长头字节数（identifier 12 + 11×u32 44 + Index 24 = 80；level
/// index 从第 80 字节起，每条 24 B）。
const HEADER_BYTES: usize = 80;
const LEVEL_INDEX_BYTES: usize = 24;

/// 白名单 vkFormat（Khronos vk.xml 权威值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ktx2BcFormat {
    /// VK_FORMAT_BC1_RGBA_UNORM_BLOCK (133)
    Bc1Unorm,
    /// VK_FORMAT_BC1_RGBA_SRGB_BLOCK (134)
    Bc1Srgb,
    /// VK_FORMAT_BC3_UNORM_BLOCK (137)
    Bc3Unorm,
    /// VK_FORMAT_BC3_SRGB_BLOCK (138)
    Bc3Srgb,
    /// VK_FORMAT_BC7_UNORM_BLOCK (145)
    Bc7Unorm,
    /// VK_FORMAT_BC7_SRGB_BLOCK (146)
    Bc7Srgb,
}

impl Ktx2BcFormat {
    /// 按 vkFormat 枚举值识别；白名单外返回 `None`。
    pub fn from_vk(vk: u32) -> Option<Self> {
        Some(match vk {
            133 => Self::Bc1Unorm,
            134 => Self::Bc1Srgb,
            137 => Self::Bc3Unorm,
            138 => Self::Bc3Srgb,
            145 => Self::Bc7Unorm,
            146 => Self::Bc7Srgb,
            _ => return None,
        })
    }

    /// 每 4×4 块字节数（BC1 = 8，BC3/BC7 = 16）。
    pub fn block_bytes(&self) -> usize {
        match self {
            Self::Bc1Unorm | Self::Bc1Srgb => 8,
            _ => 16,
        }
    }

    /// mipPadding 对齐粒度 = lcm(block_bytes, 4)（规范 levelImages）。
    pub fn align_bytes(&self) -> usize {
        match self {
            Self::Bc1Unorm | Self::Bc1Srgb => 8,
            _ => 16,
        }
    }

    /// sRGB 色彩空间标注（喂 `CpuTexture::srgb` 与 wgpu 格式选择）。
    pub fn srgb(&self) -> bool {
        matches!(self, Self::Bc1Srgb | Self::Bc3Srgb | Self::Bc7Srgb)
    }
}

/// 解析错误（中文，带字节定位——容器解析的报错必须可归因）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ktx2Error {
    /// 文件过短 / 偏移越界（含截断）。
    Truncated(String),
    /// 魔数不符（不是 KTX2 文件）。
    BadIdentifier,
    /// 字段组合超出支持面（超压缩 / 数组 / 3D / 非白名单格式等）。
    Unsupported(String),
    /// 尺寸 / 级表自相矛盾（byteLength 与块数推算不符）。
    Inconsistent(String),
}

impl std::fmt::Display for Ktx2Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated(what) => write!(f, "KTX2 文件截断：{what}"),
            Self::BadIdentifier => write!(f, "KTX2 魔数不符（前 12 字节非 «KTX 20»）"),
            Self::Unsupported(what) => write!(f, "KTX2 不支持：{what}"),
            Self::Inconsistent(what) => write!(f, "KTX2 头自相矛盾：{what}"),
        }
    }
}

impl std::error::Error for Ktx2Error {}

/// 解析产物：2D 单面 BC 纹理的全级块数据（借用文件字节）。
#[derive(Clone, Debug)]
pub struct Ktx2Image {
    pub format: Ktx2BcFormat,
    pub width: u32,
    pub height: u32,
    /// 各级块流（下标 = mip 级；第 0 级 = 基级）。
    pub levels: Vec<Vec<u8>>,
    /// kvdByteLength > 0（文件带 KTX 元数据键值区，已跳过）。
    pub has_key_value_data: bool,
}

impl Ktx2Image {
    /// 基级 GPU 字节数（级 0 块流长，观测 / 估算口径）。
    pub fn base_level_bytes(&self) -> usize {
        self.levels.first().map_or(0, Vec::len)
    }
}

/// 解析 KTX2 字节流（纯函数、零分配倾向：级数据一次 `to_vec` 拷出）。
pub fn parse_ktx2(bytes: &[u8]) -> Result<Ktx2Image, Ktx2Error> {
    if bytes.len() < HEADER_BYTES {
        return Err(Ktx2Error::Truncated(format!(
            "文件 {} B < 定长头 {HEADER_BYTES} B",
            bytes.len()
        )));
    }
    if bytes[0..12] != KTX2_IDENTIFIER {
        return Err(Ktx2Error::BadIdentifier);
    }
    let u32_at = |off: usize| -> u32 {
        u32::from_le_bytes(bytes[off..off + 4].try_into().expect("长度已验"))
    };
    let u64_at = |off: usize| -> u64 {
        u64::from_le_bytes(bytes[off..off + 8].try_into().expect("长度已验"))
    };
    let vk_format = u32_at(12);
    let type_size = u32_at(16);
    let width = u32_at(20);
    let height = u32_at(24);
    let depth = u32_at(28);
    let layer_count = u32_at(32);
    let face_count = u32_at(36);
    let level_count = u32_at(40);
    let supercompression = u32_at(44);
    // dfd/kvd/sgd 的偏移字段 v1 不消费内容，只留「sgd 必须为空」判定
    // （sgdByteOffset 64 / sgdByteLength 72，KVD 长度在 60）
    let kvd_byte_length = u32_at(60);
    let sgd_byte_length = u64_at(72);

    let format = Ktx2BcFormat::from_vk(vk_format).ok_or_else(|| {
        Ktx2Error::Unsupported(format!(
            "vkFormat {vk_format} 不在白名单（仅 BC1/BC3/BC7 的 UNORM/SRGB 六种，\
             未压缩 / ETC2 / ASTC / 3D 类型拒收）"
        ))
    })?;
    if type_size != 1 {
        return Err(Ktx2Error::Unsupported(format!(
            "typeSize {type_size} ≠ 1（块压缩格式规范要求）"
        )));
    }
    if supercompression != 0 {
        return Err(Ktx2Error::Unsupported(format!(
            "supercompressionScheme {supercompression} ≠ 0（BasisLZ/Zstd/ZLIB 需解码器，\
             v1 只吃未压缩载荷；用 toktx 时勿加 --zstd/--compress"
        )));
    }
    if face_count != 1 || layer_count > 1 || depth > 1 {
        return Err(Ktx2Error::Unsupported(format!(
            "非 2D 单面纹理（faceCount {face_count}, layerCount {layer_count}, depth {depth}）"
        )));
    }
    if width == 0 || height == 0 {
        return Err(Ktx2Error::Inconsistent(format!("零尺寸 {width}×{height}")));
    }
    if level_count == 0 {
        return Err(Ktx2Error::Unsupported(
            "levelCount = 0（mip 生成授权形态）——块压缩资产须显式携带全部级".to_string(),
        ));
    }
    if sgd_byte_length > 0 {
        return Err(Ktx2Error::Unsupported(
            "supercompressionGlobalData 非空（超压缩残留，scheme 字段却为 0）".to_string(),
        ));
    }
    let levels_expected = (HEADER_BYTES + LEVEL_INDEX_BYTES * level_count as usize) as u64;
    // 级数据尺寸推算与级表校验
    let mut cursor = levels_expected; // 近似界：级数据必须落在文件内，逐项精确校验
    let mut levels = Vec::with_capacity(level_count as usize);
    for p in 0..level_count as usize {
        let entry = HEADER_BYTES + p * LEVEL_INDEX_BYTES;
        if entry + LEVEL_INDEX_BYTES > bytes.len() {
            return Err(Ktx2Error::Truncated(format!("level index #{p} 越界")));
        }
        let byte_offset = u64_at(entry);
        let byte_length = u64_at(entry + 8);
        let uncompressed_length = u64_at(entry + 16);
        if byte_length != uncompressed_length {
            return Err(Ktx2Error::Inconsistent(format!(
                "级 {p} byteLength {byte_length} ≠ uncompressedByteLength {uncompressed_length}\
                 （未压缩载荷恒等）"
            )));
        }
        // 期望尺寸：级 p 维数 max(1, dim >> p) 向上取整到 4×4 块（4×4
        // 块格式三族一致）
        let lw = (width >> p).max(1);
        let lh = (height >> p).max(1);
        let nbx = lw.div_ceil(4) as u64;
        let nby = lh.div_ceil(4) as u64;
        let want = nbx * nby * format.block_bytes() as u64;
        // 未压缩载荷的级布局 = mipPadding（前导，对齐到
        // lcm(block_bytes, 4)）+ 紧排块数据（规范 levelImages）：
        // byteLength − want = 前导填充，必须小于对齐粒度；数据切出
        // 精确 want 字节交给上传
        if byte_length < want {
            return Err(Ktx2Error::Inconsistent(format!(
                "级 {p} byteLength {byte_length} < 块数推算 {want}（{lw}×{lh}）"
            )));
        }
        let align = format.align_bytes() as u64;
        let pad = byte_length - want;
        if pad >= align {
            return Err(Ktx2Error::Inconsistent(format!(
                "级 {p} 前导填充 {pad} ≥ 对齐粒度 {align}（byteLength 异常）"
            )));
        }
        cursor = cursor.max(byte_offset + byte_length);
        levels.push((byte_offset + pad, want));
    }
    if cursor > bytes.len() as u64 {
        return Err(Ktx2Error::Truncated(format!(
            "级数据尾 {cursor} > 文件 {} B",
            bytes.len()
        )));
    }
    // 末级维数 sanity：块压缩要求基级 ≥ 4 的维数在末级 ≥1 自然满足；
    // 级数超 mip 全链上限（长边 log2）视为矛盾
    if level_count > 32 - width.max(height).leading_zeros() {
        return Err(Ktx2Error::Inconsistent(format!(
            "levelCount {level_count} 超过 {width}×{height} 的 mip 全链上限"
        )));
    }
    let owned = levels
        .iter()
        .map(|&(off, len)| bytes[off as usize..(off + len) as usize].to_vec())
        .collect();
    Ok(Ktx2Image {
        format,
        width,
        height,
        levels: owned,
        has_key_value_data: kvd_byte_length > 0,
    })
}

/// 组装一个本引擎可读的最小 KTX2 容器（未压缩载荷 + 逐级 mip，
/// `supercompressionScheme = 0`）。`levels_data[p]` = 第 p 级的连续块流
/// （由 [`crate::texture_bc7::encode_bc7`] 等产出，维数 = `max(1, dim>>p)`
/// 的块对齐字节数）。
///
/// 用途定位：给**离线预压缩资产**打包、以及容器 ↔ 上传的闭环自测
/// （`ktx2_render.rs` / 场景 `.ktx2` 装载）——是「读入路径的产侧对偶」，
/// **非通用导出器**：DFD 区填 28 B 占位（解析侧不消费、格式判定以
/// vkFormat 为唯一事实源），故不保证通过外部严格校验器（完整 DFD /
/// 超压缩方案随外部工具消费方出现再议，见模块文档限制③）。
pub fn build_ktx2(vk_format: u32, width: u32, height: u32, levels_data: &[Vec<u8>]) -> Vec<u8> {
    let level_count = levels_data.len() as u32;
    let index_len = LEVEL_INDEX_BYTES * level_count as usize;
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(&KTX2_IDENTIFIER);
    for v in [
        vk_format,
        1,
        width,
        height,
        0,
        0,
        1,
        level_count,
        0, // supercompressionScheme = KtxNone
    ] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    // Index：dfd（占位 28 B，解析侧不消费）；kvd/sgd 空。
    let dfd_offset = (HEADER_BYTES + index_len) as u32;
    out.extend_from_slice(&dfd_offset.to_le_bytes());
    out.extend_from_slice(&28u32.to_le_bytes()); // dfdByteLength
    out.extend_from_slice(&0u32.to_le_bytes()); // kvdByteOffset
    out.extend_from_slice(&0u32.to_le_bytes()); // kvdByteLength
    out.extend_from_slice(&0u64.to_le_bytes()); // sgdByteOffset
    out.extend_from_slice(&0u64.to_le_bytes()); // sgdByteLength
    let mut data_start = dfd_offset as usize + 28;
    let mut table = Vec::new();
    for d in levels_data {
        table.push((data_start as u64, d.len() as u64, d.len() as u64));
        data_start += d.len();
    }
    for (off, len, ulen) in table {
        out.extend_from_slice(&off.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&ulen.to_le_bytes());
    }
    out.resize(dfd_offset as usize + 28, 0);
    // dfd 内容（不消费）：零填充已在 resize 完成
    for d in levels_data {
        out.extend_from_slice(d);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试别名：容器组装统一走公开 [`build_ktx2`]（读入路径的产侧对偶）。
    fn bake_ktx2(vk: u32, w: u32, h: u32, levels_data: &[Vec<u8>]) -> Vec<u8> {
        build_ktx2(vk, w, h, levels_data)
    }

    fn bc7_levels(w: u32, h: u32) -> Vec<Vec<u8>> {
        // 16×16 基级（4×4 块 ×16 B = 256）+ 8×8 二级（2×2 块 ×16 = 64）
        let base = vec![7u8; (w / 4 * w / 4 * 16) as usize];
        let mip = vec![9u8; (h / 8 * h / 8 * 16) as usize];
        vec![base, mip]
    }

    #[test]
    fn parses_bc7_two_level_texture() {
        let bytes = bake_ktx2(145, 16, 16, &bc7_levels(16, 16));
        let img = parse_ktx2(&bytes).expect("合法文件应可解析");
        assert_eq!(img.format, Ktx2BcFormat::Bc7Unorm);
        assert_eq!((img.width, img.height), (16, 16));
        assert_eq!(img.levels.len(), 2);
        assert_eq!(img.levels[0].len(), 256);
        assert_eq!(img.levels[1].len(), 64);
        assert!(!img.has_key_value_data);
        // 级内容 = 文件块数据原样
        assert!(img.levels[0].iter().all(|&b| b == 7));
        assert!(img.levels[1].iter().all(|&b| b == 9));
    }

    #[test]
    fn rejects_bad_identifier_and_truncation() {
        assert_eq!(
            parse_ktx2(&[]).err(),
            Some(Ktx2Error::Truncated("文件 0 B < 定长头 80 B".to_string()))
        );
        let mut bytes = bake_ktx2(145, 16, 16, &bc7_levels(16, 16));
        bytes[3] = b'Y';
        assert_eq!(parse_ktx2(&bytes).err(), Some(Ktx2Error::BadIdentifier));
        bytes[3] = b' '; // 复位标识符，单独考察截断闸（截断不足 80 B 定长头）
        bytes.truncate(72);
        assert!(matches!(parse_ktx2(&bytes), Err(Ktx2Error::Truncated(_))));
    }

    #[test]
    fn rejects_out_of_whitelist_and_unsupported_knobs() {
        // R8G8B8A8_UNORM = 37（未压缩形态拒收：引擎无消费场景）
        let b = bake_ktx2(37, 16, 16, &bc7_levels(16, 16));
        assert!(matches!(
            parse_ktx2(&b),
            Err(Ktx2Error::Unsupported(m)) if m.contains("37")
        ));
        // zstd 超压缩（scheme = 2）：字段偏移 44
        let mut v = bake_ktx2(145, 16, 16, &bc7_levels(16, 16));
        let pos = 12 + 8 * 4;
        v[pos..pos + 4].copy_from_slice(&2u32.to_le_bytes());
        assert!(matches!(
            parse_ktx2(&v),
            Err(Ktx2Error::Unsupported(m)) if m.contains("supercompressionScheme 2")
        ));
        // 立方贴图 faceCount = 6（字段偏移 36）
        let mut c = bake_ktx2(145, 16, 16, &bc7_levels(16, 16));
        let pos = 12 + 6 * 4;
        c[pos..pos + 4].copy_from_slice(&6u32.to_le_bytes());
        assert!(matches!(
            parse_ktx2(&c),
            Err(Ktx2Error::Unsupported(m)) if m.contains("非 2D 单面")
        ));
    }

    #[test]
    fn rejects_inconsistent_level_table() {
        let mut v = bake_ktx2(145, 16, 16, &bc7_levels(16, 16));
        // 篡改第 0 级 byteLength（≠ uncompressedByteLength）
        let entry = HEADER_BYTES;
        v[entry + 8..entry + 16].copy_from_slice(&999_999u64.to_le_bytes());
        assert!(matches!(
            parse_ktx2(&v),
            Err(Ktx2Error::Inconsistent(m)) if m.contains("byteLength")
        ));
    }

    /// vkFormat 白名单与块字节：解析表的口径锁（145/146 权威值等
    /// 易错数字钉死，防「凭记忆改代码」回潮）。
    #[test]
    fn vk_whitelist_and_block_sizes() {
        assert_eq!(Ktx2BcFormat::from_vk(133), Some(Ktx2BcFormat::Bc1Unorm));
        assert_eq!(Ktx2BcFormat::from_vk(146), Some(Ktx2BcFormat::Bc7Srgb));
        assert_eq!(Ktx2BcFormat::from_vk(144), None, "BC6H 不在白名单");
        assert_eq!(Ktx2BcFormat::Bc1Unorm.block_bytes(), 8);
        assert_eq!(Ktx2BcFormat::Bc7Srgb.block_bytes(), 16);
        assert!(Ktx2BcFormat::Bc3Srgb.srgb() && !Ktx2BcFormat::Bc3Unorm.srgb());
    }

    /// 级前导 mipPadding（未压缩载荷的规范布局）被剥离：级 1 声明
    /// byteLength = want + 8 填充，解析产出的数据恰为后段 want 字节。
    #[test]
    fn strips_leading_mip_padding() {
        let (w, h) = (16u32, 16u32);
        let level_count = 2u32;
        let want0: usize = 4 * 4 * 16; // 256
        let want1: usize = 2 * 2 * 16; // 64
        let pad1 = 8usize;
        let index_len = 24 * level_count as usize;
        let data0 = HEADER_BYTES + index_len + 28;
        let data1 = data0 + want0;
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(&KTX2_IDENTIFIER);
        for v in [145u32, 1, w, h, 0, 0, 1, level_count, 0] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&(data0 as u32).to_le_bytes());
        out.extend_from_slice(&28u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        for (off, len) in [
            (data0 as u64, want0 as u64),
            (data1 as u64, (want1 + pad1) as u64),
        ] {
            out.extend_from_slice(&off.to_le_bytes());
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&len.to_le_bytes());
        }
        out.resize(data0, 0); // dfd 占位
        out.extend_from_slice(&vec![7u8; want0]);
        out.extend_from_slice(&vec![0u8; pad1]); // 前导填充
        out.extend_from_slice(&vec![9u8; want1]);
        let img = parse_ktx2(&out).expect("带 mipPadding 的合法文件应可解析");
        assert_eq!(img.levels[0].len(), want0);
        assert_eq!(img.levels[1].len(), want1, "填充已剥离，只留块数据");
        assert!(img.levels[1].iter().all(|&b| b == 9));
    }
}
