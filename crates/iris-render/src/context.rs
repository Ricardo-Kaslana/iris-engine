//! GPU 上下文：实例 / 适配器 / 设备 / 队列。

use log::info;

/// GPU 上下文。wgpu 统一封装 Vulkan / DX12 / Metal / WebGPU 后端，
/// 由驱动自动选择当前平台的最优路径。
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl Gpu {
    /// 异步初始化 GPU 上下文（全平台统一核心）。
    ///
    /// `compatible_surface` 传入已创建的窗口表面时，会优先选择支持该表面
    /// 显示的适配器；传 `None` 用于离屏渲染（无头环境 / 测试）。
    ///
    /// 平台原因（wasm32）：WebGPU 后端的适配器 / 设备请求是 JS Promise，
    /// **无法同步阻塞等待**——`pollster::block_on` 在 wasm 是忙自旋
    /// （std 线程原语为空壳，`thread::park` 立即返回），而 Promise 只能等
    /// 事件循环让出控制权后 resolve，同步等待会卡死浏览器主线程。因此
    /// web 路径一律走本异步入口（由 [`crate::surface::SurfaceKit::with_window_async`]
    /// 与 winit 事件循环逐帧驱动）。
    ///
    /// 无可用适配器时返回 `None`，调用方应优雅降级而非 panic。
    pub async fn with_instance_async(
        instance: wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'static>>,
    ) -> Option<Self> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface,
                force_fallback_adapter: false,
                ..Default::default()
            })
            .await
            .ok()?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("iris.gpu"),
                ..Default::default()
            })
            .await
            .ok()?;
        let info = adapter.get_info();
        info!(
            "Iris 渲染后端：{}（{}）",
            info.name,
            match info.backend {
                wgpu::Backend::Vulkan => "Vulkan",
                wgpu::Backend::Dx12 => "DirectX 12",
                wgpu::Backend::Metal => "Metal",
                wgpu::Backend::Gl => "OpenGL",
                wgpu::Backend::BrowserWebGpu => "WebGPU",
                _ => "未知",
            }
        );
        Some(Self {
            instance,
            adapter,
            device,
            queue,
        })
    }

    /// 同步初始化 GPU 上下文（native 专用）：pollster 阻塞等待请求完成。
    ///
    /// 平台原因（wasm32）：wasm 上不提供此方法——JS Promise 无法阻塞等待
    /// （见 [`Gpu::with_instance_async`] 的说明），误用会在编译期报
    /// 「方法不存在」而非运行时卡死浏览器。
    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_instance(
        instance: wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'static>>,
    ) -> Option<Self> {
        pollster::block_on(Self::with_instance_async(instance, compatible_surface))
    }

    /// 离屏 GPU 上下文（无窗口表面）。
    #[cfg(not(target_arch = "wasm32"))]
    pub fn offscreen() -> Option<Self> {
        Self::with_instance(wgpu::Instance::default(), None)
    }

    /// 平台原因（wasm32）：Web 目标不支持离屏渲染路径，恒返回 `None`。
    ///
    /// - 适配器 / 设备请求是 JS Promise，只能异步驱动（见
    ///   [`Gpu::with_instance_async`]），离屏路径的同步构造无法等待；
    /// - 离屏读回依赖 `device.poll(Wait)` 与 `std::fs` 落盘，前者在 WebGPU
    ///   后端不提供阻塞语义，后者在 wasm 运行时不可用。
    ///
    /// 返回 `None` 使快照示例 / 测试在 web 上编译通过并优雅降级
    /// （打印「无 GPU」退出），符合「web 只需编译可用」的定位。
    #[cfg(target_arch = "wasm32")]
    pub fn offscreen() -> Option<Self> {
        log::warn!(
            "Web 目标无离屏渲染路径（GPU 请求为异步 Promise，读回依赖不可用的 std::fs），返回 None"
        );
        None
    }
}
