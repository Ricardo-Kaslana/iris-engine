//! Iris Ascent：3D 平台跳跃（引擎 dogfooding IV 示范游戏）。
//!
//! 窗口程序入口：DefaultPlugins 提供窗口 / 时间 / 输入 / 渲染 / 音频 /
//! 动画 / 物理 / 层级传播 / 粒子，AscentPlugin 提供全部游戏内容（角色
//! 模型来自 `assets/ascent_bot.glb`——由 `cargo run --example gen_asset`
//! 程序化生成；缺失时插件降级为程序化几何 + 代码剪辑，游戏仍完整可玩）。
//!
//! 运行：
//! ```sh
//! cargo run -p iris-ascent --release
//! ```
//!
//! wasm32 说明：`App::run` 在 web 上不会返回（winit web 后端以 JS 异常
//! unwind 退出循环，见 iris-engine::app 文档），编译目标检查照常通过。

use iris_ascent::AscentPlugin;
use iris_engine::prelude::*;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins::new("Iris Ascent"))
        .add_plugin(AscentPlugin::default())
        .run();
}
