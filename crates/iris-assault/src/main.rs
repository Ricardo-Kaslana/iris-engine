//! Iris Assault：写实战斗庭院（引擎 dogfooding VI 示范游戏）。
//!
//! 窗口程序入口：DefaultPlugins 提供窗口 / 时间 / 输入 / 渲染 / 音频 /
//! 动画（含足部与手部 IK）/ 时间线 / 物理 / 层级传播 / 粒子 / 空间音频 /
//! UI / 命令队列编排，AssaultPlugin 提供全部游戏内容（角色 GLB 与氛围
//! OGG 经 `assets/assault.ipak` pack 加载——由
//! `cargo run -p iris-assault --example gen_assault_assets` 程序化生成；
//! pack 缺失时插件降级为文件路径、再降级为静态几何占位，游戏仍完整可玩）。
//!
//! 运行：
//! ```sh
//! cargo run -p iris-assault --release
//! ```
//!
//! wasm32 说明：`App::run` 在 web 上不会返回（winit web 后端以 JS 异常
//! unwind 退出循环，见 iris-engine::app 文档），编译目标检查照常通过。

use iris_assault::AssaultPlugin;
use iris_engine::prelude::*;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins::new("Iris Assault"))
        .add_plugin(AssaultPlugin::default())
        .run();
}
