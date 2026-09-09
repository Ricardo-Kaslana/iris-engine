//! Iris Editor：关卡摆放编辑器（R58a 最小 MVP）窗口入口。
//!
//! `DefaultPlugins` 提供窗口 / 时间 / 输入 / 渲染 / 音频 / 动画 / 时间线 /
//! 物理 / 层级传播 / 粒子 / 空间音频 / UI / 命令队列编排，[`EditorPlugin`]
//! 提供编辑器本体（Startup 开关卡 + Update 相机 / 输入 / 退出）。
//!
//! 运行：
//! ```sh
//! cargo run -p iris-editor --release                        # 缺省 assets/editor_mvp.level.json
//! cargo run -p iris-editor --release -- 关卡档路径.json      # CLI 指定
//! ```
//!
//! 键位表见 `iris_editor::shell` 模块文档；视口里左上角也常驻一份。
//!
//! wasm32 说明：编辑器是原生工具（文件读写与 CLI 参数在 web 运行期不可
//! 用），但 crate 内无原生专属 API，编译目标检查照常通过。

use iris_editor::shell::EditorPlugin;
use iris_engine::prelude::*;

fn main() {
    let plugin = EditorPlugin::from_args();
    let title = format!(
        "Iris Editor · R58a MVP · {}",
        plugin.level_path.to_string_lossy()
    );
    App::new()
        .add_plugins(DefaultPlugins::new(title))
        .add_plugin(plugin)
        .run();
}
