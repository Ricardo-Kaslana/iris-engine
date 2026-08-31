//! Iris Collect：第三人称限时收集小游戏（引擎 dogfooding 示范）。
//!
//! 窗口程序入口：DefaultPlugins 提供窗口 / 时间 / 输入 / 渲染 / 音频 /
//! 层级传播，GamePlugin 提供全部游戏内容。
//!
//! 运行：
//! ```sh
//! cargo run -p iris-game --release
//! ```
//!
//! wasm32 说明：`App::run` 在 web 上不会返回（winit web 后端以 JS 异常
//! unwind 退出循环，见 iris-engine::app 文档），编译目标检查照常通过。

use iris_engine::prelude::*;
use iris_game::GamePlugin;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins::new("Iris Collect"))
        .add_plugin(GamePlugin)
        .run();
}
