//! GPU 上下文：实例 / 适配器 / 设备 / 队列。

use log::info;

/// GPU 上下文。wgpu 统一封装 Vulkan / DX12 / Metal 后端，
/// 由驱动自动选择当前平台的最优路径。
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl Gpu {
    /// 初始化 GPU 上下文。
    ///
    /// `compatible_surface` 传入已创建的窗口表面时，会优先选择支持该表面
    /// 显示的适配器；传 `None` 用于离屏渲染（无头环境 / 测试）。
    ///
    /// 无可用适配器时返回 `None`，调用方应优雅降级而非 panic。
    pub fn with_instance(
        instance: wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'static>>,
    ) -> Option<Self> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface,
            force_fallback_adapter: false,
            ..Default::default()
        }))
        .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("iris.gpu"),
                ..Default::default()
            },
        ))
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
        Some(Self { instance, adapter, device, queue })
    }

    /// 离屏 GPU 上下文（无窗口表面）。
    pub fn offscreen() -> Option<Self> {
        Self::with_instance(wgpu::Instance::default(), None)
    }
}
