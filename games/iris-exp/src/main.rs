//! Iris Exp 窗口程序入口：DefaultPlugins 提供窗口 / 时间 / 输入 / 渲染 /
//! 音频 / 动画 / 物理 / 层级传播，ExpPlugin 提供角色装配与动作切换。
//!
//! 运行：
//! ```sh
//! cargo run -p iris-exp --release
//! ```

use iris_engine::prelude::*;
use iris_exp::ExpPlugin;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins::new("Iris Exp"))
        .add_plugin(ExpPlugin)
        .run();
}
