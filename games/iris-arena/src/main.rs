//! Iris Arena：波次生存竞技场（引擎 dogfooding II 示范游戏）。
//!
//! 窗口程序入口：DefaultPlugins 提供窗口 / 时间 / 输入 / 渲染 / 音频 /
//! 动画 / 物理 / 层级传播，ArenaPlugin 提供全部游戏内容。
//!
//! 运行：
//! ```sh
//! cargo run -p iris-arena --release
//! ```
//!
//! wasm32 说明：`App::run` 在 web 上不会返回（winit web 后端以 JS 异常
//! unwind 退出循环，见 iris-engine::app 文档），编译目标检查照常通过。

use iris_arena::ArenaPlugin;
use iris_engine::prelude::*;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins::new("Iris Arena"))
        // 关卡与敌人模型路径经 asset_path 解析（CWD 相对优先，workspace 根
        // 相对回退）；ArenaPlugin::new 可注入任意场景路径（测试 / 变体关卡）
        .add_plugin(ArenaPlugin::default())
        .run();
}
