//! Iris Bastion：塔防（引擎 dogfooding III 示范游戏）。
//!
//! 窗口程序入口：DefaultPlugins 提供窗口 / 时间 / 输入 / 渲染 / 音频 /
//! 动画 / 物理 / 层级传播 / 粒子，BastionPlugin 提供全部游戏内容
//! （几何、音源全部程序化生成，无外部资产文件）。
//!
//! 运行：
//! ```sh
//! cargo run -p iris-bastion --release
//! ```
//!
//! wasm32 说明：`App::run` 在 web 上不会返回（winit web 后端以 JS 异常
//! unwind 退出循环，见 iris-engine::app 文档），编译目标检查照常通过。

use iris_bastion::BastionPlugin;
use iris_engine::prelude::*;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins::new("Iris Bastion"))
        .add_plugin(BastionPlugin)
        .run();
}
