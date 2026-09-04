//! 设置持久化：把引擎 / 游戏全局设置序列化为 JSON 文件（R41）。
//!
//! [`Settings`] 作为 World 资源使用（`insert_resource`），游戏读改跑一帧后
//! 调 [`Settings::save`] 落盘。文件缺失 / 损坏 / 字段缺失一律回退默认——
//! **加载永不失败**（只 `log::warn`），运行时零 panic。
//!
//! 缺字段向后兼容：每个字段带 `#[serde(default = ...)]`，旧版本文件（少了
//! 本轮新增字段）解析时自动填默认，不整单失败。
//!
//! 写入路径：先建父目录再写 JSON（pretty 打印，便于人读 / diff）。

use std::path::Path;

use iris_core::Resource;
use serde::{Deserialize, Serialize};

/// 全局设置（World 资源）：主音量 / 全屏 / 垂直同步 / 视场角。
///
/// 序列化为 JSON 文件（经 [`Settings::load`] / [`Settings::save`]）。
#[derive(Serialize, Deserialize, Resource, Clone, Debug, PartialEq)]
pub struct Settings {
    /// 主音量（0..1）。
    #[serde(default = "default_master_volume")]
    pub master_volume: f32,
    /// 是否全屏。
    #[serde(default)]
    pub fullscreen: bool,
    /// 是否垂直同步。
    #[serde(default = "default_vsync")]
    pub vsync: bool,
    /// 垂直视场角（度）。
    #[serde(default = "default_fov_degrees")]
    pub fov_degrees: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            master_volume: default_master_volume(),
            fullscreen: false,
            vsync: default_vsync(),
            fov_degrees: default_fov_degrees(),
        }
    }
}

fn default_master_volume() -> f32 {
    1.0
}
fn default_vsync() -> bool {
    true
}
fn default_fov_degrees() -> f32 {
    70.0
}

impl Settings {
    /// 从文件加载设置。文件缺失 / 损坏 / 字段缺失一律回退 [`Settings::default`]
    /// 并 `log::warn`（加载永不失败）。
    pub fn load(path: impl AsRef<Path>) -> Settings {
        let path = path.as_ref();
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Settings>(&text) {
                Ok(settings) => settings,
                Err(e) => {
                    log::warn!("设置文件解析失败（{e}），回退默认设置：{path:?}");
                    Settings::default()
                }
            },
            Err(e) => {
                log::warn!("设置文件读取失败（{e}），回退默认设置：{path:?}");
                Settings::default()
            }
        }
    }

    /// 把设置写成 JSON 文件（先建父目录）。`io::Result`：写盘失败时返回
    /// （与加载不同，保存失败由调用方决定是否重试 / 提示）。
    pub fn save(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        let path = path.as_ref();
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::other(format!("设置序列化失败：{e}")))?;
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        std::fs::write(path, json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 唯一临时文件路径（并行测试不能共享文件名——AGENTS.md 竞态教训）。
    fn unique_temp_path(tag: &str) -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("iris_{tag}_{}_{n}.json", std::process::id()))
    }

    /// 往返：save 后 load 得到等值设置（精确值比对）。
    #[test]
    fn settings_roundtrip_through_file() {
        let path = unique_temp_path("settings_roundtrip");
        let s = Settings {
            master_volume: 0.6,
            fullscreen: true,
            vsync: false,
            fov_degrees: 75.0,
        };
        s.save(&path).unwrap();
        let loaded = Settings::load(&path);
        assert_eq!(loaded, s, "保存后加载应等值");
        std::fs::remove_file(&path).ok();
    }

    /// 损坏 JSON：回退默认（加载不 panic）。
    #[test]
    fn corrupt_json_falls_back_to_default() {
        let path = unique_temp_path("settings_corrupt");
        std::fs::write(&path, "{ 不是合法 JSON").unwrap();
        let s = Settings::load(&path);
        assert_eq!(s, Settings::default(), "损坏文件应回退默认");
        std::fs::remove_file(&path).ok();
    }

    /// 缺字段向后兼容：只写 master_volume，其余字段取 serde 默认。
    #[test]
    fn missing_fields_default_backward_compatible() {
        let path = unique_temp_path("settings_missing");
        std::fs::write(&path, r#"{"master_volume":0.5}"#).unwrap();
        let s = Settings::load(&path);
        assert_eq!(s.master_volume, 0.5);
        assert!(!s.fullscreen, "缺字段 fullscreen 取默认 false");
        assert!(s.vsync, "缺字段 vsync 取默认 true");
        assert_eq!(s.fov_degrees, 70.0, "缺字段 fov_degrees 取默认 70");
        std::fs::remove_file(&path).ok();
    }

    /// 文件缺失：回退默认（首次启动无配置文件）。
    #[test]
    fn missing_file_falls_back_to_default() {
        let path = unique_temp_path("settings_absent");
        std::fs::remove_file(&path).ok();
        let s = Settings::load(&path);
        assert_eq!(s, Settings::default(), "缺席文件应回退默认");
    }
}
