//! # iris-platform
//!
//! Iris 引擎平台层：跨平台窗口配置与键鼠输入抽象。
//!
//! `Input` 作为 `World` 资源存在，游戏系统直接查询：
//! ```ignore
//! if world.resource::<Input>().key_pressed(KeyCode::Escape) { ... }
//! ```
//!
//! `MouseCapture` 保存第一人称光标捕获请求。请求由游戏系统写入，实际
//! 抓取与原始鼠标位移的接线由 `iris-engine` 主循环结算。

pub mod input;
pub mod window;

pub mod prelude {
    pub use crate::input::{Input, MouseCapture};
    pub use crate::window::{WindowConfig, WindowHandle};
    // 查询侧（MouseButton / KeyCode）与注入侧（ElementState / PhysicalKey）
    // 类型一并重导出：headless 测试与工具脚本无须直接依赖 winit
    pub use winit::event::{ElementState, MouseButton};
    pub use winit::keyboard::{KeyCode, PhysicalKey};
}
