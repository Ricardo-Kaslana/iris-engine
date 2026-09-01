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
        Self {
            title: title.into(),
            width,
            height,
            ..Default::default()
        }
    }

    pub fn to_attributes(&self) -> winit::window::WindowAttributes {
        let attrs = winit::window::Window::default_attributes()
            .with_title(&self.title)
            .with_inner_size(winit::dpi::LogicalSize::new(self.width, self.height))
            .with_resizable(self.resizable);
        // 平台原因（wasm32）：winit 0.30 web 后端创建的 canvas **默认不附加
        // 进 DOM**（with_append 默认 false，winit 源码 PlatformSpecificWindowAttributes
        // ::default），画布脱离文档树就不会被浏览器合成显示。显式请求把
        // canvas 附加到 document.body（需要容器页提供 body，见 web 示例）。
        #[cfg(target_arch = "wasm32")]
        let attrs = {
            use winit::platform::web::WindowAttributesExtWebSys;
            attrs.with_append(true)
        };
        attrs
    }
}

/// 窗口句柄：`Arc<Window>` 的别名，可安全地跨系统共享。
pub type WindowHandle = std::sync::Arc<winit::window::Window>;
