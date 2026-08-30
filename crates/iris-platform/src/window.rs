//! 窗口配置与句柄。

/// 窗口创建参数。
#[derive(Clone, Debug)]
pub struct WindowConfig {
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub resizable: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: "Iris Engine".to_owned(),
            width: 1280,
            height: 720,
            resizable: true,
        }
    }
}

impl WindowConfig {
    pub fn new(title: impl Into<String>, width: u32, height: u32) -> Self {
        Self { title: title.into(), width, height, ..Default::default() }
    }

    pub fn to_attributes(&self) -> winit::window::WindowAttributes {
        winit::window::Window::default_attributes()
            .with_title(&self.title)
            .with_inner_size(winit::dpi::LogicalSize::new(self.width, self.height))
            .with_resizable(self.resizable)
    }
}

/// 窗口句柄：`Arc<Window>` 的别名，可安全地跨系统共享。
pub type WindowHandle = std::sync::Arc<winit::window::Window>;
