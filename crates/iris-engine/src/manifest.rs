//! 资产清单 manifest（R59）：JSON 描述的资产集合——路径 / 类别 /
//! 优先级整数 / 纹理压缩标志，批量导入与优先级加载的确定性源头。
//!
//! # 格式（全蛇形命名，与场景 JSON 同风格）
//!
//! ```json
//! {
//!   "assets": [
//!     { "path": "models/actor.glb", "kind": "raw", "priority": 0, "bc3": true },
//!     { "path": "scenes/yard.scene.json", "kind": "scene" }
//!   ]
//! }
//! ```
//!
//! - `path`（必填）：资产在挂载内的路径约定（`/` 分隔），清单内唯一；
//! - `kind`（必填）：`"scene"`（JSON 结构校验）或 `"raw"`（字节透传，
//!   GLB / OGG 等自描述格式归此类）；
//! - `priority`（可选，缺省 0）：**越小越先加载**（[`AssetLoader`]
//!   出队序 =（优先级，请求序）升序）；负数比缺省更急，正数延后；
//! - `bc3`（可选，缺省 false）：纹理压缩标志（R59）——对 GLB / 贴图
//!   类条目声明「其入库纹理走 BC3 压缩上传」（1 B/px，带 alpha 纹理；
//!   全不透明仍归 BC1 分层）。声明经 [`crate::assets::asset_path`]
//!   消费方落到 `TextureStore::declare_bc3`，未声明纹理维持既有
//!   RGBA8 / BC1 行为逐位不变。
//!
//! # 确定性
//!
//! 条目表用 `BTreeMap<String, ManifestEntry>` 按路径字典序（与
//! broadphase / pack 条目表同一选型规约——需要迭代序的确定性结构不用
//! 哈希容器）：`entries()` 迭代恒字典序；[`AssetManifest::request_all`]
//! 按该序逐条入队，请求序（seq）与清单内容一一对应——加载线程的
//! 出队序列因此完全由清单内容决定，与解析时机无关（单测守护）。
//!
//! # 校验（错误全中文，定位到条目）
//!
//! 结构 JSON 错误 / 条目路径为空 / 清单内路径重复 / 未知 `kind` 值 /
//! 缺 `assets` 数组，全部收敛为 [`ManifestError`]（中文 Display）。
//! 未知字段按 serde 默认忽略（前向兼容：新版清单在旧引擎可载，
//! 与场景 JSON 同一约定）。

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::async_loading::{AssetKind, AssetLoader};
use crate::vfs::AssetMount;

/// 清单解析 / 校验错误（中文报错，定位到具体条目）。
#[derive(Debug)]
pub enum ManifestError {
    /// 不是合法 JSON 或顶层结构不符（缺 `assets` 数组等）。
    Json(String),
    /// 条目级校验失败：路径为空 / 路径重复 / 未知 kind。
    Validation(String),
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(what) => write!(f, "清单解析失败：{what}"),
            Self::Validation(what) => write!(f, "清单校验失败：{what}"),
        }
    }
}

impl std::error::Error for ManifestError {}

/// serde 反序列化载体（内部）：未知字段忽略（前向兼容）。
#[derive(Deserialize)]
struct ManifestFile {
    assets: Vec<ManifestEntryRaw>,
}

#[derive(Deserialize)]
struct ManifestEntryRaw {
    path: String,
    kind: String,
    #[serde(default)]
    priority: i32,
    #[serde(default)]
    bc3: bool,
    /// C5a 显式压缩声明（`"none" | "bc1" | "bc3" | "bc7"`）；缺省 = None
    /// （未声明）。与 legacy `bc3: true` 并存时本字段赢（择新）。
    #[serde(default)]
    compression: Option<String>,
}

/// 纹理压缩声明（C5a）：manifest `compression` 字段的解析形态。
/// 决策树消费口径见 `TextureStore` 各 `is_*_declared`（BC1 自动分层
/// 规则仍是缺省行为——本枚举表达的是**作者显式意图**）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Compression {
    /// 显式要求无压缩（数据贴图质量声明的清单表达；等价于
    /// `declare_uncompressed`）。
    None,
    Bc1,
    Bc3,
    /// BC7（引擎编码器只产 Mode 6，见 `iris_render::texture_bc7`）。
    Bc7,
}

/// 清单内的一条资产条目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestEntry {
    /// 资产路径（挂载内约定，清单内唯一）。
    pub path: String,
    /// 资产类别（决定 [`AssetLoader`] 在字节之上追加的校验工作）。
    pub kind: AssetKind,
    /// 优先级整数：越小越先加载（缺省 0）。
    pub priority: i32,
    /// 纹理压缩标志（R59）：该条目的入库纹理声明走 BC3 上传
    /// （带 alpha 才实际生效；见 [`crate::texture_bcn`] 分层规则——
    /// iris-render re-export，见 `iris_engine::prelude`）。
    /// **legacy 视图**：`compression` 缺省时 `bc3: true` 映射为
    /// [`Compression::Bc3`]；`compression` 在场时本字段不再是事实源
    /// （保留仅为构造兼容，新代码读 [`Self::compression`]）。
    pub bc3: bool,
    /// 显式压缩声明（C5a）：`None`（字段缺席）= 未声明（走现行分层
    /// 规则）；与 legacy `bc3` 并存时**本字段赢**（择新不报错）。
    pub compression: Option<Compression>,
}

impl ManifestEntry {
    /// 生效压缩声明（择新规则的单一事实源）。
    pub fn effective_compression(&self) -> Option<Compression> {
        match self.compression {
            Some(c) => Some(c),
            None if self.bc3 => Some(Compression::Bc3),
            None => None,
        }
    }
}

/// 资产清单：路径 → 条目的确定性集合。
#[derive(Debug, Default)]
pub struct AssetManifest {
    entries: BTreeMap<String, ManifestEntry>,
}

impl AssetManifest {
    /// 从字节解析清单（校验语义见模块文档）。
    pub fn parse(bytes: &[u8]) -> Result<Self, ManifestError> {
        let file: ManifestFile = serde_json::from_slice(bytes).map_err(|e| {
            ManifestError::Json(format!("不是合法的清单 JSON（需要顶层 assets 数组）：{e}"))
        })?;
        let mut manifest = Self::default();
        for raw in file.assets {
            let kind = match raw.kind.as_str() {
                "scene" => AssetKind::Scene,
                "raw" => AssetKind::RawBytes,
                other => {
                    return Err(ManifestError::Validation(format!(
                        "条目「{}」的 kind 值未知：「{other}」（支持 scene / raw）",
                        raw.path
                    )));
                }
            };
            if raw.path.is_empty() {
                return Err(ManifestError::Validation(
                    "条目路径不能为空字符串".to_string(),
                ));
            }
            let compression = match raw.compression.as_deref() {
                None => None,
                Some("none") => Some(Compression::None),
                Some("bc1") => Some(Compression::Bc1),
                Some("bc3") => Some(Compression::Bc3),
                Some("bc7") => Some(Compression::Bc7),
                Some(other) => {
                    return Err(ManifestError::Validation(format!(
                        "条目「{}」的 compression 值未知：「{other}」（支持 none / bc1 / bc3 / bc7）",
                        raw.path
                    )));
                }
            };
            let entry = ManifestEntry {
                path: raw.path.clone(),
                kind,
                priority: raw.priority,
                bc3: raw.bc3,
                compression,
            };
            if manifest.entries.insert(raw.path.clone(), entry).is_some() {
                return Err(ManifestError::Validation(format!(
                    "清单内重复路径：「{raw}」",
                    raw = raw.path
                )));
            }
        }
        Ok(manifest)
    }

    /// 从挂载读取并解析清单（先经 [`AssetMount::read`] 取字节再
    /// [`Self::parse`]——清单本身也是一份资产，走同一挂载语义）。
    pub fn from_mount(mount: &AssetMount, path: &str) -> Result<Self, ManifestError> {
        let bytes = mount
            .read(path)
            .map_err(|e| ManifestError::Json(format!("清单文件「{path}」读取失败：{e}")))?;
        Self::parse(&bytes)
    }

    /// 按路径字典序遍历条目（确定性迭代口）。
    pub fn entries(&self) -> impl Iterator<Item = &ManifestEntry> {
        self.entries.values()
    }

    /// 按路径查条目。
    pub fn get(&self, path: &str) -> Option<&ManifestEntry> {
        self.entries.get(path)
    }

    /// 条目数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空清单。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 把全部条目按字典序提交给加载器（R59 批量加载口）：每条以自身
    /// `priority` 调 [`AssetLoader::request_with_priority`]，请求序 =
    /// 字典序——加载线程的出队序列（优先级，请求序）因此完全由清单
    /// 内容决定。返回按字典序的（路径，优先级）表，供调用方记录 /
    /// 断言加载时序。
    pub fn request_all(&self, loader: &AssetLoader) {
        for entry in self.entries() {
            loader.request_with_priority(&entry.path, entry.kind, entry.priority);
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"{
        "assets": [
            { "path": "models/actor.glb", "kind": "raw", "priority": 0, "bc3": true },
            { "path": "scenes/yard.scene.json", "kind": "scene" },
            { "path": "audio/ambient.ogg", "kind": "raw", "priority": 10 }
        ]
    }"#;

    #[test]
    fn parses_entries_with_defaults_and_flags() {
        let manifest = AssetManifest::parse(VALID.as_bytes()).unwrap();
        assert_eq!(manifest.len(), 3);
        let actor = manifest.get("models/actor.glb").unwrap();
        assert_eq!(actor.kind, AssetKind::RawBytes);
        assert_eq!(actor.priority, 0);
        assert!(actor.bc3, "bc3 标志应显式解析");
        let scene = manifest.get("scenes/yard.scene.json").unwrap();
        assert_eq!(scene.kind, AssetKind::Scene);
        assert_eq!(scene.priority, 0, "缺省优先级应为 0");
        assert!(!scene.bc3, "缺省 bc3 应为 false");
        assert_eq!(manifest.get("audio/ambient.ogg").unwrap().priority, 10);
    }

    /// 迭代确定性：条目恒按路径字典序（与书写顺序无关）。
    #[test]
    fn iteration_is_path_lexicographic() {
        let manifest = AssetManifest::parse(VALID.as_bytes()).unwrap();
        let paths: Vec<&str> = manifest.entries().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "audio/ambient.ogg",
                "models/actor.glb",
                "scenes/yard.scene.json"
            ],
            "entries 迭代应为路径字典序（书写顺序无关）"
        );
    }

    #[test]
    fn rejects_with_chinese_errors() {
        // 结构损坏
        let err = AssetManifest::parse(b"{ not json").unwrap_err();
        assert!(
            matches!(err, ManifestError::Json(ref m) if m.contains("JSON")),
            "{err}"
        );
        // 缺 assets
        let err = AssetManifest::parse(b"{}").unwrap_err();
        assert!(matches!(err, ManifestError::Json(_)), "{err}");
        // 未知 kind
        let err =
            AssetManifest::parse(br#"{ "assets": [{ "path": "a.bin", "kind": "texture" }] }"#)
                .unwrap_err();
        assert!(
            matches!(err, ManifestError::Validation(ref m) if m.contains("kind") && m.contains("texture")),
            "{err}"
        );
        // 空路径
        let err =
            AssetManifest::parse(br#"{ "assets": [{ "path": "", "kind": "raw" }] }"#).unwrap_err();
        assert!(
            matches!(err, ManifestError::Validation(ref m) if m.contains("路径不能为空")),
            "{err}"
        );
        // 重复路径
        let err = AssetManifest::parse(
            br#"{ "assets": [
                { "path": "a.bin", "kind": "raw" },
                { "path": "a.bin", "kind": "raw" }
            ] }"#,
        )
        .unwrap_err();
        assert!(
            matches!(err, ManifestError::Validation(ref m) if m.contains("重复")),
            "{err}"
        );
    }

    /// 前向兼容：未知字段按 serde 默认忽略（新版清单在旧引擎可载）。
    #[test]
    fn unknown_fields_are_ignored_for_forward_compatibility() {
        let manifest = AssetManifest::parse(
            br#"{ "version": 2, "assets": [
                { "path": "a.bin", "kind": "raw", "hint": "hot" }
            ] }"#,
        )
        .unwrap();
        assert_eq!(manifest.len(), 1);
        assert_eq!(manifest.get("a.bin").unwrap().priority, 0);
    }

    /// request_all：入队序 = 字典序、优先级逐条携带——经 inline 后端
    /// 的出队序（（优先级，请求序）升序）完全由清单内容决定。
    #[test]
    fn request_all_produces_manifest_determined_dequeue_order() {
        let manifest = AssetManifest::parse(VALID.as_bytes()).unwrap();
        let loader = AssetLoader::inline(mount_for_manifest());
        manifest.request_all(&loader);

        let mut order = Vec::new();
        while let Some((path, result)) = loader.poll() {
            result.expect("清单内条目应全部可读（fixture 内已备齐）");
            order.push(path);
        }
        // priority 0 两条按字典序 FIFO，priority 10 的 ambient 最后
        assert_eq!(
            order,
            vec![
                "models/actor.glb".to_string(),
                "scenes/yard.scene.json".to_string(),
                "audio/ambient.ogg".to_string(),
            ],
            "出队序应由清单决定：先优先级、同优先级按字典序"
        );
    }

    /// 清单 fixture 的挂载：清单内容 + 三条资产（内容最小化，仅验证
    /// 出队序与可读性）。
    fn mount_for_manifest() -> AssetMount {
        static EMBEDDED: &[(&str, &str)] = &[
            ("models/actor.glb", "glb-bytes"),
            ("audio/ambient.ogg", "ogg-bytes"),
            ("scenes/yard.scene.json", r#"{ "entities": [] }"#),
        ];
        let mut mount = AssetMount::new();
        mount.push(AssetSource::Embedded(EMBEDDED));
        mount
    }

    use crate::vfs::AssetSource;

    /// C5a：`compression` 显式声明解析 + legacy `bc3` 读兼容映射 +
    /// 双声明择新 + 非法值中文拒绝。
    #[test]
    fn compression_parses_with_legacy_mapping() {
        let json = r#"{
            "assets": [
                { "path": "a.png", "kind": "raw", "compression": "bc7" },
                { "path": "b.png", "kind": "raw", "bc3": true },
                { "path": "c.png", "kind": "raw", "bc3": true, "compression": "none" },
                { "path": "d.png", "kind": "raw" }
            ]
        }"#;
        let manifest = AssetManifest::parse(json.as_bytes()).unwrap();
        let get = |p: &str| {
            manifest
                .get(p)
                .unwrap_or_else(|| panic!("条目 {p} 应存在"))
                .effective_compression()
        };
        assert_eq!(get("a.png"), Some(Compression::Bc7), "显式 bc7");
        assert_eq!(get("b.png"), Some(Compression::Bc3), "legacy bc3 映射");
        assert_eq!(get("c.png"), Some(Compression::None), "双声明择新");
        assert_eq!(get("d.png"), None, "双缺席 = 未声明（现行分层规则）");

        let bad = r#"{ "assets": [ { "path": "x", "kind": "raw", "compression": "bc9" } ] }"#;
        let err = AssetManifest::parse(bad.as_bytes()).unwrap_err();
        assert!(
            matches!(&err, ManifestError::Validation(m) if m.contains("bc9")),
            "非法 compression 应中文拒绝，实际 {err}"
        );
    }
}
