//! 窗口表面渲染：深度附件管理、窗口尺寸自适应、帧呈现。
//!
//! MSAA 与 HDR 中间缓冲由 `Renderer` 内部管理；本层只负责
//! 表面配置、深度纹理与帧获取 / 呈现。

use std::sync::Arc;

use crate::context::Gpu;
use crate::renderer::Renderer;

/// 组合「GPU + 渲染器 + 窗口表面」的窗口渲染套件。
pub struct SurfaceKit {
    gpu: Arc<Gpu>,
    renderer: Renderer,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    depth_view: wgpu::TextureView,
    size: (u32, u32),
}

impl SurfaceKit {
    /// 同步创建窗口渲染套件（native 专用）：内部完成表面创建、GPU 初始化
    /// 与管线构建。
    ///
    /// 平台原因（wasm32）：wasm 上不提供此方法——GPU 适配器 / 设备请求是
    /// JS Promise，无法阻塞等待（`pollster` 在 wasm 是忙自旋，会卡死浏览器
    /// 主线程），必须走 [`SurfaceKit::with_window_async`]。误用在编译期报错。
    ///
    /// 无可用 GPU 适配器时返回 `None`。
    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_window(window: std::sync::Arc<winit::window::Window>) -> Option<Self> {
        pollster::block_on(Self::with_window_async(window))
    }

    /// 异步创建窗口渲染套件（全平台统一核心；web 上由事件循环逐帧驱动）。
    ///
    /// 平台原因（wasm32）：表面创建对 web 无需任何特判——winit 0.30 的
    /// web 窗口经 raw-window-handle 的 `WebCanvas` 变体携带
    /// `HtmlCanvasElement` 指针，wgpu 30 的 WebGPU 后端直接解引用该指针
    /// 拿到画布（不依赖旧版 `[data-raw-handle]` DOM 查询），与 native 是
    /// 同一条 `create_surface(window)` 调用；差异只在「请求适配器必须异步」。
    pub async fn with_window_async(window: std::sync::Arc<winit::window::Window>) -> Option<Self> {
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(window.clone())
            .expect("创建窗口表面失败");
        let gpu = Arc::new(Gpu::with_instance_async(instance, Some(&surface)).await?);
        let size = window.inner_size();
        let caps = surface.get_capabilities(&gpu.adapter);
        // 优先 sRGB 格式：blit 输出线性色，由硬件完成 gamma 编码
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Srgb,
            width: size.width.max(1),
            height: size.height.max(1),
            // FIFO 在所有平台都受支持，且不空烧 GPU
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&gpu.device, &config);
        let depth_view = create_depth(&gpu, size.width, size.height);
        let renderer = Renderer::new(gpu.clone(), format);
        Some(Self {
            gpu,
            renderer,
            surface,
            config,
            depth_view,
            size: (size.width, size.height),
        })
    }

    /// 窗口尺寸变化时调用：重建表面配置与深度附件。
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.gpu.device, &self.config);
        self.depth_view = create_depth(&self.gpu, width, height);
        self.size = (width, height);
    }

    /// 渲染并呈现一帧。表面丢失 / 过期时重新配置，下一帧恢复。
    pub fn render(&mut self, world: &iris_core::world::World) {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                // 表面属性已变：先重新配置，本帧仍可呈现
                self.surface.configure(&self.gpu.device, &self.config);
                frame
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.gpu.device, &self.config);
                return;
            }
            other => {
                log::warn!("获取交换链帧失败：{other:?}");
                return;
            }
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.renderer.draw(
            world,
            &view,
            &self.depth_view,
            self.config.width,
            self.config.height,
        );
        // wgpu 30：呈现移到了队列上
        self.gpu.queue.present(frame);
    }

    pub fn renderer(&self) -> &Renderer {
        &self.renderer
    }

    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }
}

fn create_depth(gpu: &Gpu, width: u32, height: u32) -> wgpu::TextureView {
    let depth = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("iris.depth"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: crate::SAMPLE_COUNT,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    depth.create_view(&wgpu::TextureViewDescriptor::default())
}
