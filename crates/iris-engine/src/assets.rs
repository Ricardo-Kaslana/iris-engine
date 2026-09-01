//! 资产路径解析：工作目录相对 → 工作区根相对的候选回退。
//!
//! `cargo run` 与示例的 cwd 是 workspace 根，但 `cargo test` 的 cwd 是
//! **crate 目录**——「assets/xxx」这类相对路径在两种入口下指向不同位置
//! （iris-arena dogfooding II 摩擦③：Default 资产路径在测试里静默失效，
//! 池缩容为 0 且错误被 `log::error` 吞掉）。[`asset_path`] 按序尝试候选
//! 位置，返回第一个**存在**者；测试与示例无须再注入绝对路径。
//!
//! ```ignore
//! let path = iris_engine::asset_path("assets/enemy_worm.glb");
//! let instance = spawn_gltf_instance(&mut world, path)?;
//! ```

use std::path::{Path, PathBuf};

/// 解析资产相对路径，返回第一个存在的候选：
///
/// 1. **工作目录相对**（`cargo run` / 示例的 cwd 即 workspace 根，直接命中）；
/// 2. **工作区根相对**（`cargo test` 的 cwd 是 crate 目录，回退到
///    workspace 根下查找——编译期 `CARGO_MANIFEST_DIR` 上溯两级）；
/// 3. 都不存在时返回 CWD 相对路径——让后续 `load` 报出调用方熟悉的
///    相对路径错误（路径解析不吞错，加载失败自有其报告职责）。
///
/// 字段注入优先：显式给定路径（测试 fixture、变体关卡）时调用方应直接
/// 使用该路径，[`asset_path`] 只服务「约定俗成的仓库内默认资产」。
pub fn asset_path(relative: impl AsRef<Path>) -> PathBuf {
    let relative = relative.as_ref();
    let from_cwd = PathBuf::from(relative);
    if from_cwd.exists() {
        return from_cwd;
    }
    let from_workspace = workspace_root().join(relative);
    if from_workspace.exists() {
        return from_workspace;
    }
    from_cwd
}

/// 工作区根：编译期 `CARGO_MANIFEST_DIR`（workspace 成员 crate 目录）
/// 上溯两级。Iris 的部署形态是源码工作区（`cargo run` / `cargo test`），
/// 发布布局的运行期重定位不在路线图内。
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("CARGO_MANIFEST_DIR 必有工作区根两级祖先")
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 工作区根下存在的文件：cargo test 的 cwd 是 crate 目录（CWD 候选
    /// 不可达），应回退到工作区根候选解析成功。
    #[test]
    fn resolves_workspace_root_relative_when_cwd_misses() {
        // 放进 target/（workspace 根内、git 忽略），进程 id 防并行冲突
        let relative = format!("target/asset_path_probe_{}.tmp", std::process::id());
        let expected = workspace_root().join(&relative);
        std::fs::write(&expected, b"probe").expect("写探测文件");

        let resolved = asset_path(&relative);
        assert_eq!(resolved, expected, "应解析为工作区根下的路径");
        assert!(resolved.exists());
        std::fs::remove_file(&expected).ok();
    }

    /// 首个存在者优先：绝对路径（必然存在）原样返回，不做任何拼接。
    #[test]
    fn existing_absolute_path_passes_through() {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert_eq!(asset_path(&manifest), manifest);
    }

    /// 都不存在：返回 CWD 相对路径原样透传——加载器随后报出的错误里
    /// 是调用方写下的那个相对路径（解析层不吞错、不改写）。
    #[test]
    fn missing_paths_fall_back_to_cwd_relative() {
        let relative = "definitely/missing/asset_path_probe.tmp";
        assert_eq!(asset_path(relative), PathBuf::from(relative));
    }
}
