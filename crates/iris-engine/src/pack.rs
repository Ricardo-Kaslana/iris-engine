//! pack 单文件资产打包格式（PACK1）：多文件资产集打包为一个可随机读的
//! 归档，是 [`crate::vfs::AssetSource::Pack`] 挂载源的数据载体。
//!
//! # 格式（全部小端，布局图见 ARCHITECTURE §7.3）
//!
//! ```text
//! ┌────────┬──────────┬───────────┬──────────────────────────┬───────────┐
//! │ "IPAK" │ 版本 u32 │ 条目数 u32 │ 条目表 × N               │ 数据区     │
//! │ 4 B    │ = 1      │           │ 见下                     │ 连续排列   │
//! └────────┴──────────┴───────────┴──────────────────────────┴───────────┘
//! 条目（可变长，紧密排列）：
//! ┌─────────────┬──────────┬──────────┬──────────┬────────────┐
//! │ 路径长 u32  │ 路径字节 │ 偏移 u64 │ 长度 u64 │ 校验和 u64 │
//! └─────────────┴──────────┴──────────┴──────────┴────────────┘
//! ```
//!
//! - 路径为 UTF-8 字节、`/` 分隔、区分大小写，按写入时的原样存储
//!   （不做分隔符转换或大小写折叠——确定性优先）；偏移为**绝对偏移**
//!   （从文件头算起），条目表按路径**字典序**排列，数据区按同序连续
//!   排列——同输入两次 [`PackWriter::write_to`] 产出**逐位相同**的文件；
//! - 校验和字段为 **CRC-32（IEEE 802.3，反射多项式 0xEDB88320）**，
//!   以 u64 存储（高 32 位恒 0）。选型理由：定位是**防损坏**而非防篡改，
//!   CRC-32 纯 Rust 实现十余行、可审计、无依赖（XXH3 需要引入实现或
//!   依赖 crate）；u64 字段 + 版本号给将来升级更强的校验留了位——升级
//!   时版本号 +1，旧 reader 拒读而非误读。
//!
//! # 大文件语义
//!
//! [`PackReader::open`] 只读**头 + 条目表**（两次有界读取）；[`PackReader::read`]
//! 对目标条目做 `seek + read_exact`——**数据区不整体进内存**，读 1 KB
//! 条目不触碰 12 MB 的邻居。这是结构性保证而非实现巧合：[`PackReader`]
//! 是泛型（默认 `File`），测试用计数源验证过 open 与小条目读取的
//! 实际 IO 量（见 `partial_read_touches_only_requested_bytes`）。
//!
//! # v1 不含（诚实清单）
//!
//! - 无压缩、无去重（同内容两份就存两份——打包主要解决「文件数爆炸 +
//!   随机读」问题，压缩收益交给将来的版本号演进）；
//! - 无加密 / 签名（CRC-32 不是抗篡改校验）；
//! - 目录层级只是路径字符串约定（`a/b.txt`），无真目录节点；
//! - 写入只支持全量产出（`write_to`），无增量追加。
//!
//! # 用法
//!
//! ```ignore
//! use iris_engine::pack::{PackReader, PackWriter};
//!
//! let mut writer = PackWriter::new();
//! writer.add("scenes/arena.scene.json", scene_json_bytes)?;
//! writer.add("models/worm.glb", glb_bytes)?;
//! writer.write_file("target/game.ipak")?;
//!
//! let pack = PackReader::open("target/game.ipak")?;
//! let bytes = pack.read("models/worm.glb")?; // seek + read_exact，校验 CRC
//! for path in pack.iter() { /* 字典序列出 */ }
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use iris_render::meshlet::{MeshletAssetError, MeshletMesh};

/// 文件头魔数（"IPAK" = Iris PAcK）。
const MAGIC: [u8; 4] = *b"IPAK";

/// 当前格式版本。reader 拒读更高版本（向后兼容不向前兼容——诚实报错）。
const VERSION: u32 = 1;

/// 文件头固定长度：魔数 4 + 版本 4 + 条目数 4。
const HEADER_LEN: u64 = 12;

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// pack 打包 / 读取错误。
#[derive(Debug)]
pub enum PackError {
    /// 磁盘 IO 失败。
    Io(std::io::Error),
    /// 打包时路径非法（空路径）。
    InvalidPath(String),
    /// 打包时同一路径写两次。
    DuplicatePath(String),
    /// 不是 pack 文件（魔数不符）。
    BadMagic,
    /// 文件版本高于本实现支持的版本。
    UnsupportedVersion(u32),
    /// 结构损坏：条目表截断 / 路径非法 UTF-8 / 偏移越界 / 重复条目。
    Corrupt(String),
    /// 路径不存在（读取用）。
    NotFound(String),
    /// 数据损坏：CRC-32 校验和不符（防损坏）。
    ChecksumMismatch {
        /// 条目路径。
        path: String,
        /// 条目表登记的校验和。
        expected: u64,
        /// 实际算出的校验和。
        actual: u64,
    },
}

impl fmt::Display for PackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO 错误：{e}"),
            Self::InvalidPath(path) => write!(f, "非法路径：{path:?}（不能为空）"),
            Self::DuplicatePath(path) => write!(f, "重复路径：{path:?}（同一路径只能写一次）"),
            Self::BadMagic => write!(f, "不是 pack 文件（魔数不符，期望 \"IPAK\"）"),
            Self::UnsupportedVersion(v) => {
                write!(f, "pack 版本 {v} 高于本实现支持的版本 {VERSION}")
            }
            Self::Corrupt(what) => write!(f, "pack 结构损坏：{what}"),
            Self::NotFound(path) => write!(f, "pack 内无此条目：{path:?}"),
            Self::ChecksumMismatch {
                path,
                expected,
                actual,
            } => write!(
                f,
                "数据损坏（CRC 校验和不符）：{path:?} 期望 {expected:#x} 实际 {actual:#x}"
            ),
        }
    }
}

impl std::error::Error for PackError {}

impl From<std::io::Error> for PackError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ---------------------------------------------------------------------------
// CRC-32（IEEE 802.3，反射多项式 0xEDB88320）——纯 Rust 表驱动，无 unsafe
// ---------------------------------------------------------------------------

/// 编译期生成 256 项反射表（const fn 循环，运行期零初始化成本）。
const fn build_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = build_crc_table();

/// CRC-32（IEEE 802.3）。返回 u64（高 32 位恒 0），与条目表的 u64
/// 校验和字段直接对应——字段宽度是格式承诺，算法宽度是实现细节。
pub fn crc32(bytes: &[u8]) -> u64 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc = CRC_TABLE[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    (crc ^ 0xFFFF_FFFF) as u64
}

// ---------------------------------------------------------------------------
// PackWriter
// ---------------------------------------------------------------------------

/// pack 打包器：`add` 收集条目 → [`PackWriter::write_to`] 一次性产出。
///
/// 条目表按路径字典序（`BTreeMap`——确定性硬账：同输入逐位同输出），
/// 数据区按同序连续排列，条目间无对齐填充。
#[derive(Debug, Default)]
pub struct PackWriter {
    entries: BTreeMap<String, Vec<u8>>,
}

impl PackWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// 收集一条资产。路径约定 `/` 分隔；同一路径只能写一次（覆盖是
    /// 静默丢数据的温床，显式报错）。
    pub fn add(&mut self, path: impl Into<String>, bytes: Vec<u8>) -> Result<(), PackError> {
        let path = path.into();
        if path.is_empty() {
            return Err(PackError::InvalidPath(path));
        }
        if self.entries.insert(path.clone(), bytes).is_some() {
            return Err(PackError::DuplicatePath(path));
        }
        Ok(())
    }

    /// 已收集的条目数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空 pack（0 条目——合法：写出后可 open 可 iter）。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 全部条目写出为一个 pack 字节流，返回总字节数。
    ///
    /// 偏移在写出时计算：条目表尺寸确定数据区起点，无需缓冲整个
    /// 输出（逐条直写 `writer`）。
    pub fn write_to(&self, mut writer: impl Write) -> Result<u64, PackError> {
        // 头 + 条目表尺寸先算好：数据区起点 = HEADER_LEN + Σ条目长
        let mut table_len = 0u64;
        for path in self.entries.keys() {
            table_len += 4 + path.len() as u64 + 8 + 8 + 8;
        }
        let data_start = HEADER_LEN + table_len;

        writer.write_all(&MAGIC)?;
        writer.write_all(&VERSION.to_le_bytes())?;
        writer.write_all(&(self.entries.len() as u32).to_le_bytes())?;

        let mut offset = data_start;
        for (path, bytes) in &self.entries {
            writer.write_all(&(path.len() as u32).to_le_bytes())?;
            writer.write_all(path.as_bytes())?;
            writer.write_all(&offset.to_le_bytes())?;
            writer.write_all(&(bytes.len() as u64).to_le_bytes())?;
            writer.write_all(&crc32(bytes).to_le_bytes())?;
            offset += bytes.len() as u64;
        }
        for bytes in self.entries.values() {
            writer.write_all(bytes)?;
        }
        Ok(data_start + self.data_len())
    }

    /// 便捷：写出为磁盘文件，返回总字节数。
    pub fn write_file(&self, path: impl AsRef<Path>) -> Result<u64, PackError> {
        let file = File::create(path)?;
        self.write_to(file)
    }

    fn data_len(&self) -> u64 {
        self.entries.values().map(|b| b.len() as u64).sum()
    }
}

// ---------------------------------------------------------------------------
// PackReader
// ---------------------------------------------------------------------------

/// 条目表内的一条登记（条目数据本体不驻留——大文件语义的核心）。
#[derive(Clone, Copy, Debug)]
struct PackEntry {
    /// 数据区绝对偏移（从文件头算起）。
    offset: u64,
    /// 数据字节数。
    length: u64,
    /// 写入时的 CRC-32。
    checksum: u64,
}

/// pack 读取器：open 时只读头 + 条目表，数据按需 `seek + read_exact`。
///
/// 泛型参数 `S` 默认 [`File`]（`PackReader` 即 `PackReader<File>`）；
/// `from_source` 接受任意 `Read + Seek` 源（测试注入计数源做 IO 记账、
/// 将来内存源复用同一路径）。内部 [`std::sync::Mutex`] 保证 `&self`
/// 可读且 `Send + Sync`（可进 [`crate::vfs::AssetMount`] 资源）。
#[derive(Debug)]
pub struct PackReader<S = File> {
    source: std::sync::Mutex<S>,
    /// 路径 → 条目登记。BTreeMap：iter 字典序 + open 时重复路径显式报
    /// Corrupt（写侧 BTreeMap 天然去重，读侧格式可能被手工构造）。
    entries: BTreeMap<String, PackEntry>,
}

impl PackReader<File> {
    /// 打开磁盘上的 pack 文件（只读方式）。头 + 条目表两次有界读取，
    /// 数据区不进内存。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PackError> {
        Self::from_source(File::open(path)?)
    }
}

impl<S: Read + Seek> PackReader<S> {
    /// 从任意 `Read + Seek` 源打开（测试计账 / 内存源）。
    pub fn from_source(mut source: S) -> Result<Self, PackError> {
        let file_size = source.seek(SeekFrom::End(0))?;
        source.seek(SeekFrom::Start(0))?;

        // 头：魔数 + 版本 + 条目数
        let mut header = [0u8; HEADER_LEN as usize];
        read_exact_bounded(&mut source, &mut header)?;
        if header[0..4] != MAGIC {
            return Err(PackError::BadMagic);
        }
        let version = u32::from_le_bytes(header[4..8].try_into().expect("版本字段 4 字节"));
        if version > VERSION {
            return Err(PackError::UnsupportedVersion(version));
        }
        let count = u32::from_le_bytes(header[8..12].try_into().expect("条目数字段 4 字节"));

        // 条目表：逐条读、逐条验
        let mut entries = BTreeMap::new();
        let mut prev_offset: Option<u64> = None;
        for index in 0..count {
            let mut len_buf = [0u8; 4];
            read_exact_bounded(&mut source, &mut len_buf)?;
            let path_len = u32::from_le_bytes(len_buf) as usize;
            let mut path_buf = vec![0u8; path_len];
            read_exact_bounded(&mut source, &mut path_buf)?;
            let path = String::from_utf8(path_buf)
                .map_err(|_| PackError::Corrupt(format!("条目 #{index} 路径不是合法 UTF-8")))?;
            let mut rest = [0u8; 24];
            read_exact_bounded(&mut source, &mut rest)?;
            let entry = PackEntry {
                offset: u64::from_le_bytes(rest[0..8].try_into().expect("偏移 8 字节")),
                length: u64::from_le_bytes(rest[8..16].try_into().expect("长度 8 字节")),
                checksum: u64::from_le_bytes(rest[16..24].try_into().expect("校验和 8 字节")),
            };
            if path.is_empty() {
                return Err(PackError::Corrupt(format!("条目 #{index} 路径为空")));
            }
            if entries.contains_key(&path) {
                return Err(PackError::Corrupt(format!("条目表重复路径：{path:?}")));
            }
            if entry.offset < HEADER_LEN || entry.length > file_size.saturating_sub(entry.offset) {
                return Err(PackError::Corrupt(format!(
                    "条目 {path:?} 数据区间越界：偏移 {} 长度 {}（文件 {} 字节）",
                    entry.offset, entry.length, file_size
                )));
            }
            // 条目表按路径字典序（写侧承诺）：读侧不强制但校验数据区
            // 不重叠有等价防损坏效果——这里只查偏移递增（写侧字典序 =
            // 偏移递增），乱序 pack 仍可读，重叠区间才是真损坏。
            if let Some(prev) = prev_offset {
                if entry.offset < prev {
                    return Err(PackError::Corrupt(format!(
                        "条目 {path:?} 数据区间与前一条件目重叠（偏移回退）"
                    )));
                }
            }
            prev_offset = Some(entry.offset + entry.length);
            entries.insert(path, entry);
        }

        Ok(Self {
            source: std::sync::Mutex::new(source),
            entries,
        })
    }

    /// 读一条资产的完整字节（seek + read_exact + CRC 校验）。
    ///
    /// 数据损坏（校验和不符）返回 [`PackError::ChecksumMismatch`]——
    /// 防 corrupted-asset 静默流入解析器；不存在返回
    /// [`PackError::NotFound`]（与「损坏」区分，挂载层据此继续找
    /// 下一来源）。
    pub fn read(&self, path: &str) -> Result<Vec<u8>, PackError> {
        let entry = self
            .entries
            .get(path)
            .ok_or_else(|| PackError::NotFound(path.to_string()))?;
        let mut source = self.source.lock().expect("pack 源锁不应中毒");
        source.seek(SeekFrom::Start(entry.offset))?;
        let mut bytes = vec![0u8; entry.length as usize];
        source.read_exact(&mut bytes)?;
        drop(source);
        let actual = crc32(&bytes);
        if actual != entry.checksum {
            return Err(PackError::ChecksumMismatch {
                path: path.to_string(),
                expected: entry.checksum,
                actual,
            });
        }
        Ok(bytes)
    }

    /// 条目是否存在（不读数据）。
    pub fn exists(&self, path: &str) -> bool {
        self.entries.contains_key(path)
    }

    /// 全部条目路径，按字典序。
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// 条目数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空 pack。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 有界读取：短读 / 截断统一报 Corrupt（头与条目表都是固定布局，
/// 「文件比表声称的短」就是损坏，不让 io::ErrorKind::UnexpectedEof
/// 漏到调用方面前变成费解错误）。
fn read_exact_bounded(source: &mut impl Read, buf: &mut [u8]) -> Result<(), PackError> {
    source
        .read_exact(buf)
        .map_err(|e| PackError::Corrupt(format!("头或条目表截断：{e}")))
}

// ---------------------------------------------------------------------------
// pack × IRML：meshlet 资产的最小读取入口（R87）
// ---------------------------------------------------------------------------

/// [`PackReader`] 读 meshlet 资产的错误：pack 层（路径不存在 / 条目损坏
/// / CRC 不符）与 IRML 层（魔数 / 版本 / 结构 / 数据损坏）分开报告，
/// 调用方可判别「挂载层继续找下一来源」（NotFound）与「资产真的坏
/// 了」（其余）。
#[derive(Debug)]
pub enum MeshletPackError {
    /// pack 层失败（`read` 的 [`PackError`] 原样透传）。
    Pack(PackError),
    /// 条目不是 IRML 容器（魔数不符——路径扩展名只是约定，读取以
    /// 魔数为准）。
    NotIrml,
    /// IRML 容器解析失败（版本 / 结构 / CRC——详见
    /// [`MeshletAssetError`]）。
    Irml(MeshletAssetError),
}

impl fmt::Display for MeshletPackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pack(e) => write!(f, "pack 读取失败：{e}"),
            Self::NotIrml => write!(f, "pack 条目不是 IRML 容器（魔数不符）"),
            Self::Irml(e) => write!(f, "IRML 解析失败：{e}"),
        }
    }
}

impl std::error::Error for MeshletPackError {}

/// 从 pack 源读一个 `.imesh`（IRML）条目并解析为
/// [`MeshletMesh`](iris_render::meshlet::MeshletMesh)（R87 的 pack 集成
/// 兑现——R84/R85 两次推迟的最小入口：一个函数 + 往返测试；编辑器与
/// AssetMount 的全接线留后续，见 ARCHITECTURE §4.43）。
///
/// 语义 = [`PackReader::read`]（seek + read_exact + CRC 校验，数据区
/// 不整体进内存——大资产只触碰所读条目）+ [`MeshletMesh::from_bytes`]
/// （版本矩阵 {1, 2, 3} / 结构校验链）。v1 边界：**一次性读入整条目**
/// ——流送的分页在 GPU 显存维度（见 `iris_render::meshlet_stream`），
/// 磁盘级按页异步 IO 不做（诚实限制 §8.56）。
pub fn read_meshlet<S: Read + Seek>(
    pack: &PackReader<S>,
    path: &str,
) -> Result<MeshletMesh, MeshletPackError> {
    let bytes = pack.read(path).map_err(MeshletPackError::Pack)?;
    if bytes.len() < 4 || bytes[0..4] != *b"IRML" {
        return Err(MeshletPackError::NotIrml);
    }
    MeshletMesh::from_bytes(&bytes).map_err(MeshletPackError::Irml)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 小工具：n 字节位置相关的合成模式（i % 251 + 异或扰动），任何
    /// 一字节错位都能被逐字节比对抓到。
    fn pattern(n: usize, salt: u8) -> Vec<u8> {
        (0..n)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(salt))
            .collect()
    }

    /// 收集若干条目并写出为内存 Vec（测试高频路径）。
    fn build_pack(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut writer = PackWriter::new();
        for (path, bytes) in entries {
            writer.add(*path, bytes.clone()).expect("收集条目");
        }
        let mut out = std::io::Cursor::new(Vec::new());
        writer.write_to(&mut out).expect("写出");
        out.into_inner()
    }

    // ---- 往返 ----

    /// 写后读逐字节一致：多文件、空文件条目、非对齐长度、含 0 字节。
    #[test]
    fn roundtrip_is_byte_exact() {
        let entries: Vec<(&str, Vec<u8>)> = vec![
            ("a.txt", b"hello pack".to_vec()),
            ("dir/b.bin", vec![0u8; 0]),            // 空条目
            ("dir/c.dat", pattern(7777, 3)),        // 非对齐长度
            ("zzz/d.bin", vec![0u8, 1, 0, 255, 0]), // 含 0 字节
        ];
        let pack = build_pack(&entries);
        let reader = PackReader::from_source(std::io::Cursor::new(pack)).expect("打开");

        assert_eq!(reader.len(), 4);
        for (path, bytes) in &entries {
            assert_eq!(
                reader.read(path).expect("读回"),
                *bytes,
                "条目 {path} 往返应逐字节一致"
            );
        }
        // 字典序列出
        let listed: Vec<&str> = reader.iter().collect();
        assert_eq!(
            listed,
            vec!["a.txt", "dir/b.bin", "dir/c.dat", "zzz/d.bin"],
            "iter 应按路径字典序"
        );
    }

    /// 条目表字典序确定性：乱序 add，两次写出逐位一致，且条目表内
    /// 首个路径是字典序最小者（直接解包文件字节验证表顺序）。
    #[test]
    fn entries_are_lexicographic_and_deterministic() {
        let make = || {
            let mut writer = PackWriter::new();
            writer.add("zeta", vec![3]).unwrap();
            writer.add("alpha", vec![1]).unwrap();
            writer.add("mid", vec![2]).unwrap();
            writer
        };
        let mut out_a = Vec::new();
        make().write_to(&mut out_a).unwrap();
        let mut out_b = Vec::new();
        make().write_to(&mut out_b).unwrap();
        assert_eq!(out_a, out_b, "同输入两次写出应逐位一致");

        // 直接解包文件字节：头 12 B 后第一条目 = 路径长 u32 + 路径
        // （alpha < mid < zeta，首条目必须是 alpha）
        let first_len = u32::from_le_bytes(out_a[12..16].try_into().unwrap()) as usize;
        assert_eq!(
            &out_a[16..16 + first_len],
            b"alpha",
            "首条目应为字典序最小者"
        );
    }

    /// CRC：数据区一个字节被翻坏 → read 报 ChecksumMismatch（含期望 /
    /// 实际值）；表区翻坏 → Corrupt；魔数错 → BadMagic。
    #[test]
    fn corruption_is_detected() {
        let payload = pattern(1024, 7);
        let pack = build_pack(&[("data.bin", payload)]);

        // 找到数据区起点（头 12 + 条目 4+8+24=36）
        let data_start = 12 + 4 + 8 + 24;
        let mut corrupted = pack.clone();
        corrupted[data_start] ^= 0xFF;
        let reader = PackReader::from_source(std::io::Cursor::new(corrupted)).unwrap();
        match reader.read("data.bin") {
            Err(PackError::ChecksumMismatch {
                path,
                expected,
                actual,
            }) => {
                assert_eq!(path, "data.bin");
                assert_ne!(expected, actual, "校验和应不同");
                assert_ne!(expected, 0, "期望值应来自条目表");
            }
            other => panic!("应报 ChecksumMismatch，实际 {other:?}"),
        }

        // 条目表翻坏（路径长度字段改成超大值）→ Corrupt（截断）
        let mut bad_table = pack.clone();
        bad_table[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        let reader = PackReader::from_source(std::io::Cursor::new(bad_table));
        assert!(
            matches!(reader, Err(PackError::Corrupt(_))),
            "表损坏应报 Corrupt"
        );

        // 魔数错 → BadMagic
        let mut bad_magic = pack.clone();
        bad_magic[0] = b'X';
        let reader = PackReader::from_source(std::io::Cursor::new(bad_magic));
        assert!(matches!(reader, Err(PackError::BadMagic)));
    }

    /// 不存在的路径 → NotFound（与「损坏」可区分）；空 pack 合法：
    /// 可写出、可打开、iter 为空、read NotFound。
    #[test]
    fn missing_path_and_empty_pack() {
        let pack = build_pack(&[("a", vec![1])]);
        let reader = PackReader::from_source(std::io::Cursor::new(pack)).unwrap();
        assert!(!reader.exists("b"));
        assert!(matches!(
            reader.read("b"),
            Err(PackError::NotFound(path)) if path == "b"
        ));

        let empty = build_pack(&[]);
        let reader = PackReader::from_source(std::io::Cursor::new(empty)).unwrap();
        assert!(reader.is_empty());
        assert_eq!(reader.iter().count(), 0);
        assert!(matches!(
            reader.read("anything"),
            Err(PackError::NotFound(_))
        ));
    }

    /// 重复路径在 add 时报错（不静默覆盖）；空路径报 InvalidPath。
    #[test]
    fn duplicate_and_empty_paths_rejected() {
        let mut writer = PackWriter::new();
        writer.add("dup", vec![1]).expect("首次写入");
        assert!(matches!(
            writer.add("dup", vec![2]),
            Err(PackError::DuplicatePath(path)) if path == "dup"
        ));
        assert!(matches!(
            writer.add("", vec![1]),
            Err(PackError::InvalidPath(_))
        ));
    }

    /// 磁盘路径 open == 内存源 open（同 pack 两种入口语义一致）。
    #[test]
    fn disk_open_matches_memory_source() {
        let pack = build_pack(&[("x/y.bin", pattern(100, 9))]);
        let dir = std::env::temp_dir().join("iris-pack-test");
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let path = dir.join(format!("disk_open_{}.ipak", std::process::id()));
        std::fs::write(&path, &pack).expect("写 pack 文件");

        let disk = PackReader::open(&path).expect("磁盘打开");
        let memory = PackReader::from_source(std::io::Cursor::new(&pack)).expect("内存打开");
        assert_eq!(
            disk.read("x/y.bin").unwrap(),
            memory.read("x/y.bin").unwrap()
        );
        std::fs::remove_file(&path).ok();
    }

    // ---- 大文件 partial-read 语义（IO 记账） ----

    /// IO 量计数器：包一层 Read + Seek，记录**实际从底层读的字节数**。
    /// 这是「内存峰值计账」的可测替身——PackReader 只经 Read/Seek 触碰
    /// 底层，读入的字节量即触碰的数据量（无隐藏的全量缓冲）。
    /// 计数器放 `Rc<Cell>`：源整体会 move 进 PackReader，外部留一个
    /// 共享句柄事后读数（测试单线程使用，不要求 Send）。
    struct CountingSource {
        inner: std::io::Cursor<Vec<u8>>,
        bytes_read: std::rc::Rc<std::cell::Cell<u64>>,
    }

    impl CountingSource {
        fn new(data: Vec<u8>) -> (Self, std::rc::Rc<std::cell::Cell<u64>>) {
            let counter = std::rc::Rc::new(std::cell::Cell::new(0));
            (
                Self {
                    inner: std::io::Cursor::new(data),
                    bytes_read: std::rc::Rc::clone(&counter),
                },
                counter,
            )
        }
    }

    impl Read for CountingSource {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.bytes_read.set(self.bytes_read.get() + n as u64);
            Ok(n)
        }
    }

    impl Seek for CountingSource {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    /// 10 MB 以上的合成数据 pack：open 只读头 + 条目表（读不到 1 KB），只读
    /// 邻居小条目全程底层读取 < 1 KB——**12 MB 数据从未进过内存**。
    /// 随后完整读大条目一次，验证内容逐字节正确（读多少、算多少）。
    #[test]
    fn partial_read_touches_only_requested_bytes() {
        let big = pattern(12 * 1024 * 1024 + 13, 1); // 12 MB + 13（非对齐）
        let small = b"tiny neighbour".to_vec();
        let pack = build_pack(&[("big.bin", big.clone()), ("small.txt", small.clone())]);

        let (source, counter) = CountingSource::new(pack);
        let reader = PackReader::from_source(source).expect("打开");

        // open 的 IO 账：头（12）+ 条目表（2 × 36 + 路径字节）≈ 90 B
        let after_open = counter.get();
        assert!(
            after_open < 1024,
            "open 只应读头 + 条目表（{after_open} B ≥ 1 KB？大文件被整体载入）"
        );

        // 小条目读取的 IO 账：只多读该条目自身（15 B）——12 MB 邻居
        // 未被触碰
        assert_eq!(reader.read("small.txt").unwrap(), small);
        let after_small = counter.get();
        assert!(
            after_small - after_open <= small.len() as u64,
            "读小条目只应触碰其自身字节（多读了 {} B）",
            after_small - after_open - small.len() as u64
        );

        // 大条目完整读回：逐字节一致（本测试的 12 MB 读取结果即峰值
        // 内存下界——调用方请求多少、返回多少，无额外复制缓冲暴露）
        assert_eq!(reader.read("big.bin").unwrap(), big);
    }

    // ---- pack × IRML（R87 最小入口）----

    /// `.imesh` 条目读取往返：构建器产物（带页表 ⇒ v3 容器）写进
    /// pack → `read_meshlet` 读回逐字段相等（簇 + 页表）；路径不存在
    /// 报 Pack(NotFound)（挂载层可判别继续找）；非 IRML 条目报
    /// NotIrml（魔数为准，扩展名只是约定）。
    #[test]
    fn read_meshlet_roundtrip_and_error_discrimination() {
        let mesh = crate::meshlet_builder::build_meshlets_from_mesh_data(
            &iris_render::mesh::MeshData::cube(),
        );
        let mut irml = Vec::new();
        let written = mesh.write_to(&mut irml).expect("写出 IRML");
        assert!(written > 0);
        let pack = build_pack(&[
            ("models/cube.imesh", irml),
            ("junk.bin", b"not a mesh".to_vec()),
        ]);
        let reader = PackReader::from_source(std::io::Cursor::new(pack)).expect("打开");

        let back = read_meshlet(&reader, "models/cube.imesh").expect("读取 meshlet");
        assert_eq!(back.clusters, mesh.clusters, "簇元数据逐字段相等");
        assert_eq!(back.hierarchy.groups, mesh.hierarchy.groups);
        assert_eq!(back.pages, mesh.pages, "v3 页表往返逐字段相等");
        assert_eq!(back.vertices.len(), mesh.vertices.len());

        assert!(matches!(
            read_meshlet(&reader, "models/missing.imesh"),
            Err(MeshletPackError::Pack(PackError::NotFound(path))) if path == "models/missing.imesh"
        ));
        assert!(
            matches!(
                read_meshlet(&reader, "junk.bin"),
                Err(MeshletPackError::NotIrml)
            ),
            "魔数不符 = 不是 IRML"
        );
    }
}
