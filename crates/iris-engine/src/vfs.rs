//! 资产挂载（mount）：多来源顺序查找的统一读取抽象。
//!
//! [`AssetMount`] 把「资产从哪来」收拢为一个资源：文件系统目录、pack
//! 单文件（[`crate::pack::PackReader`]）、编译期嵌入文本，按**先挂先
//! 命中**的顺序逐源查找。挂载是**可选资源**——不注册时引擎既有资产
//! 路径（[`crate::asset_path`] + `std::fs`）语义**零变化**；注册后由
//! 调用方显式传给 mount 感知的加载入口（如
//! [`crate::scene::load_scene_from`]）。
//!
//! # 顺序语义
//!
//! - `read` 按挂载顺序逐源尝试：源内**未命中**（路径不存在）→ 继续找
//!   下一源；源内**命中** → 立即返回（不看后续源）；全部未命中 →
//!   [`AssetError::NotFound`]；
//! - 源内命中但**读取失败**（pack 校验和不符 / IO 错误）→ 立即报错，
//!   **不**静默回退到后续源——损坏数据被静默跳过是 bug 温床（与仓库
//!   「偶发失败 = 真 bug 信号」文化一致）；
//! - 同一路径挂进多个源 = 用挂载顺序实现**覆盖 / 补丁**（先挂的赢）。
//!
//! # wasm 姿态（选型写明）
//!
//! `Fs` 源在 wasm32 上**运行时降级**而非 cfg 删除变体：`std::fs` 在
//! wasm 上是桩实现（读取恒失败），枚举形状跨平台一致、调用方代码无需
//! cfg 分叉。wasm 布局请只挂 `Embedded`（文本类；后续 fetch 源在路线
//! 图，本轮不做——v1 限制）。
//!
//! # v1 不含（诚实清单）
//!
//! - `Embedded` 仅支持**文本**资产（路径 → 字符串）：场景 JSON 这类
//!   wasm 刚需；二进制嵌入（`include_bytes!` + 路径表宏）留待扩展；
//! - 无 fetch / 网络源；无写入；无监听热重载。

use std::path::PathBuf;

use crate::pack::{PackError, PackReader};
use iris_core::Resource;

/// 资产来源。顺序查找语义见模块文档。
pub enum AssetSource {
    /// 文件系统目录：相对路径拼到 `root` 下直读（现有 `asset_path`
    /// 的目录化形态）。wasm 上读取恒失败（运行时降级，见模块文档）。
    Fs {
        /// 根目录；pack 之外的常规来源。
        root: PathBuf,
    },
    /// pack 单文件（[`PackReader::open`] 打开的归档）。
    Pack(PackReader),
    /// 编译期嵌入的文本资产表（路径 → 内容字符串）。零 IO、wasm
    /// 友好；v1 仅文本类。
    Embedded(&'static [(&'static str, &'static str)]),
}

impl std::fmt::Debug for AssetSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fs { root } => write!(f, "Fs({root:?})"),
            Self::Pack(pack) => write!(f, "Pack({} 条目)", pack.len()),
            Self::Embedded(entries) => write!(f, "Embedded({} 条)", entries.len()),
        }
    }
}

impl AssetSource {
    /// 单源读取：`Ok(None)` = 本源未命中（路径不存在），`Ok(Some)` =
    /// 命中，`Err` = 命中但读取失败（调用方立即报错，不回退下一源）。
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>, AssetError> {
        match self {
            Self::Fs { root } => {
                let full = root.join(path);
                match std::fs::read(&full) {
                    Ok(bytes) => Ok(Some(bytes)),
                    // 未命中：继续找下一源；其他 IO 错误（权限 / 损坏）
                    // 是真错误，不静默回退
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(AssetError::Failed(format!("读取 {full:?} 失败：{e}"))),
                }
            }
            Self::Pack(pack) => match pack.read(path) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(PackError::NotFound(_)) => Ok(None),
                Err(e) => Err(AssetError::Failed(format!("pack 读取 {path:?} 失败：{e}"))),
            },
            Self::Embedded(entries) => Ok(entries
                .iter()
                .find(|(embedded_path, _)| *embedded_path == path)
                .map(|(_, content)| content.as_bytes().to_vec())),
        }
    }

    /// 本源是否可能命中该路径（不读数据体；嵌入源查表，Fs / Pack 查
    /// 条目存在性）。
    fn exists(&self, path: &str) -> bool {
        match self {
            Self::Fs { root } => root.join(path).is_file(),
            Self::Pack(pack) => pack.exists(path),
            Self::Embedded(entries) => entries
                .iter()
                .any(|(embedded_path, _)| *embedded_path == path),
        }
    }
}

/// 挂载读取错误。两态刻意收敛：调用方（如场景加载）只需要区分
/// 「没有」与「坏了」。
#[derive(Debug)]
pub enum AssetError {
    /// 全部来源都未命中该路径。
    NotFound,
    /// 命中了但读取失败（IO 错误 / pack 校验和不符 / 源内格式损坏）。
    Failed(String),
}

impl std::fmt::Display for AssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "资产在全部已挂载来源中未命中"),
            Self::Failed(what) => write!(f, "资产读取失败：{what}"),
        }
    }
}

impl std::error::Error for AssetError {}

impl From<PackError> for AssetError {
    fn from(e: PackError) -> Self {
        match e {
            PackError::NotFound(_) => Self::NotFound,
            other => Self::Failed(other.to_string()),
        }
    }
}

/// 资产挂载：有序来源表 + 统一读取入口。
///
/// 可作为资源入 World（`#[derive(Resource)]`），但**默认不注册**——
/// 缺席 = 引擎既有 `std::fs` 资产路径语义零变化（这是刻意的 API 契约，
/// 见模块文档）。构建用 [`AssetMount::new`] + [`AssetMount::push`] 链式
/// 挂载，或 `FromIterator` 批量。
#[derive(Debug, Default, Resource)]
pub struct AssetMount {
    sources: Vec<AssetSource>,
}

impl AssetMount {
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一个来源（先挂的先命中）。返回 `&mut Self` 支持链式挂载。
    pub fn push(&mut self, source: AssetSource) -> &mut Self {
        self.sources.push(source);
        self
    }

    /// 顺序查找读取：先挂先命中，未命中走下一源（语义见模块文档）。
    pub fn read(&self, path: &str) -> Result<Vec<u8>, AssetError> {
        let mut last_miss = AssetError::NotFound;
        for source in &self.sources {
            match source.read(path) {
                Ok(Some(bytes)) => return Ok(bytes),
                Ok(None) => last_miss = AssetError::NotFound,
                Err(e) => return Err(e),
            }
        }
        Err(last_miss)
    }

    /// 顺序查找读取并按 UTF-8 解码（场景 JSON 等文本资产）。
    pub fn read_text(&self, path: &str) -> Result<String, AssetError> {
        let bytes = self.read(path)?;
        String::from_utf8(bytes)
            .map_err(|e| AssetError::Failed(format!("「{path}」不是合法 UTF-8 文本：{e}")))
    }

    /// 任一来源可命中该路径（不读数据体）。
    pub fn exists(&self, path: &str) -> bool {
        self.sources.iter().any(|source| source.exists(path))
    }

    /// 已挂载来源数。
    pub fn len(&self) -> usize {
        self.sources.len()
    }

    /// 是否空挂载（read 恒 NotFound）。
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}

impl Extend<AssetSource> for AssetMount {
    fn extend<T: IntoIterator<Item = AssetSource>>(&mut self, iter: T) {
        self.sources.extend(iter);
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::PackWriter;

    /// 嵌入源 fixture（'static 生命周期模拟 include_str! 使用姿态）。
    static EMBEDDED: &[(&str, &str)] = &[("text/hello.txt", "嵌入内容")];

    /// 临时目录 + 一个已写好的文件，返回（目录, 相对路径）。
    fn fs_fixture(name: &str, content: &[u8]) -> (PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("iris-vfs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        std::fs::write(dir.join(name), content).expect("写 fixture");
        (dir, name.to_string())
    }

    /// pack 字节落盘并打开（挂载的 Pack 源是磁盘形态）。
    fn pack_file(tag: &str, bytes: &[u8]) -> (PathBuf, PackReader) {
        let dir = std::env::temp_dir().join(format!("iris-vfs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let path = dir.join(format!("pack_{tag}_{}.ipak", std::process::id()));
        std::fs::write(&path, bytes).expect("写 pack 文件");
        let reader = PackReader::open(&path).expect("打开 pack");
        (path, reader)
    }

    /// 三源顺序查找：先挂先命中（同路径以先挂者为准）、跨源补齐、
    /// 全部未命中 → NotFound、损坏立即报错不回退。
    #[test]
    fn sources_resolve_in_mount_order() {
        // 文件系统条目用独有名字（pack 里已有 shared.txt——先挂先命中）
        let (fs_dir, fs_name) = fs_fixture("fs_only.txt", "来自文件系统".as_bytes());

        // pack 也带 shared.txt（应被更先挂的源覆盖）+ 独有条目
        let mut writer = PackWriter::new();
        writer
            .add("shared.txt", "来自 pack".as_bytes().to_vec())
            .unwrap();
        writer.add("pack_only.bin", vec![1, 2, 3]).unwrap();
        let mut pack_bytes = Vec::new();
        writer.write_to(&mut pack_bytes).unwrap();
        let (pack_path, pack) = pack_file("order", &pack_bytes);

        let mut mount = AssetMount::new();
        mount
            .push(AssetSource::Embedded(EMBEDDED))
            .push(AssetSource::Pack(pack))
            .push(AssetSource::Fs { root: fs_dir });

        // 先挂先命中：pack 的 shared.txt 被 Embedded？——Embedded 没有它，
        // 落到 pack；Fs 的 shared.txt 被更先挂的 pack 覆盖
        assert_eq!(mount.read("shared.txt").unwrap(), "来自 pack".as_bytes());
        // pack 独有
        assert_eq!(mount.read("pack_only.bin").unwrap(), vec![1, 2, 3]);
        // 嵌入独有
        assert_eq!(
            mount.read("text/hello.txt").unwrap(),
            "嵌入内容".as_bytes().to_vec()
        );
        // 只有文件系统有
        assert_eq!(mount.read(&fs_name).unwrap(), "来自文件系统".as_bytes());
        // 全部未命中
        assert!(matches!(
            mount.read("不存在.txt"),
            Err(AssetError::NotFound)
        ));
        assert!(!mount.exists("不存在.txt"));
        assert!(mount.exists(&fs_name));

        // 覆盖语义：把 Embedded 再挂到最前，同路径以它为准
        static OVERRIDE: &[(&str, &str)] = &[("shared.txt", "嵌入覆盖")];
        let mut mount = AssetMount::new();
        mount
            .push(AssetSource::Embedded(OVERRIDE))
            .push(AssetSource::Pack(PackReader::open(&pack_path).unwrap()));
        assert_eq!(mount.read("shared.txt").unwrap(), "嵌入覆盖".as_bytes());
    }

    /// pack 数据损坏：命中但校验失败 → 立即报 Failed，**不**回退到
    /// 后续源（防损坏静默跳过）。
    #[test]
    fn corruption_does_not_fall_through() {
        let mut writer = PackWriter::new();
        writer.add("a.bin", vec![7; 32]).unwrap();
        let mut pack_bytes = Vec::new();
        writer.write_to(&mut pack_bytes).unwrap();
        // 数据区起点：头 12 + 条目表（4 + 5 + 24 = 33）
        pack_bytes[12 + 33] ^= 0xFF;
        let (_, pack) = pack_file("corrupt", &pack_bytes);

        let (fs_dir, fs_name) = fs_fixture("backup.bin", "完好的备份".as_bytes());
        let mut mount = AssetMount::new();
        mount
            .push(AssetSource::Pack(pack))
            .push(AssetSource::Fs { root: fs_dir });

        match mount.read("a.bin") {
            Err(AssetError::Failed(msg)) => {
                assert!(msg.contains("CRC"), "应透传校验错误：{msg}");
            }
            other => panic!("损坏应报 Failed 而非回退下一源，实际 {other:?}"),
        }
        // 路径不冲突的文件系统条目照常可达
        assert_eq!(mount.read(&fs_name).unwrap(), "完好的备份".as_bytes());
    }

    /// read_text：UTF-8 解码；坏 UTF-8 → Failed（不 panic）。
    #[test]
    fn read_text_decodes_utf8() {
        static TEXT: &[(&str, &str)] = &[("greeting.json", "{\"hi\": \"你好\"}")];
        let mut mount = AssetMount::new();
        mount.push(AssetSource::Embedded(TEXT));
        assert_eq!(
            mount.read_text("greeting.json").unwrap(),
            "{\"hi\": \"你好\"}"
        );

        static BINARY: &[(&str, &str)] = &[];
        let mut writer = PackWriter::new();
        writer.add("bad.bin", vec![0xFF, 0xFE, 0xFF]).unwrap();
        let mut pack_bytes = Vec::new();
        writer.write_to(&mut pack_bytes).unwrap();
        let (_, pack) = pack_file("utf8", &pack_bytes);
        let mut mount = AssetMount::new();
        mount.push(AssetSource::Embedded(BINARY));
        mount.push(AssetSource::Pack(pack));
        assert!(matches!(
            mount.read_text("bad.bin"),
            Err(AssetError::Failed(_))
        ));
    }

    /// 空挂载：read 恒 NotFound；wasm 姿态的 Fs 运行时降级——把 Fs 指
    /// 到必然不存在的目录等价于「该源永远未命中」，多源查找不受影响
    /// （此处用不存在目录模拟桩实现行为）。
    #[test]
    fn empty_mount_and_dead_fs_source() {
        let mount = AssetMount::new();
        assert!(mount.is_empty());
        assert!(matches!(mount.read("anything"), Err(AssetError::NotFound)));

        let mut mount = AssetMount::new();
        mount
            .push(AssetSource::Fs {
                root: PathBuf::from("确定/不存在的目录"),
            })
            .push(AssetSource::Embedded(EMBEDDED));
        // Fs 未命中 → 落到 Embedded（降级源不阻断查找链）
        assert_eq!(mount.read("text/hello.txt").unwrap(), "嵌入内容".as_bytes());
        assert_eq!(mount.len(), 2);
    }
}
