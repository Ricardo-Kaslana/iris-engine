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
        // 纹理压缩特性必须**显式请求**才会出现在设备特性集（wgpu 语义）：
        // 只请求 BCn 子集（适配器不支持时与运算为空集，等价默认请求），
        // 不整体转发 adapter.features()——个别特性请求失败会让设备创建
        // 整体失败，得不偿失。wasm 的 WebGPU 实现普遍无此特性，与 native
        // 无 BC 的老 GPU 走同一条运行时回退（无 cfg 分支，见 texture_bcn）。
        let bcn = adapter.features() & wgpu::Features::TEXTURE_COMPRESSION_BC;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("iris.gpu"),
                required_features: bcn,
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

    /// 设备实际启用的 wgpu 特性集（初始化时请求的 BCn 子集 + 平台默认）。
    /// 纹理压缩等可选能力的运行时检测入口（调用方按特性走不同路径）。
    pub fn features(&self) -> wgpu::Features {
        self.device.features()
    }

    /// 是否支持 BCn 纹理压缩（`Features::TEXTURE_COMPRESSION_BC`）。
    /// 纹理上传路径据此决策 BC1 / RGBA8 回退（见 `texture_bcn`）；
    /// wgpu 的压缩纹理格式要求该特性显式启用后才可用（初始化已请求，
    /// 本方法即最终裁决）。wasm 的 WebGPU 实现普遍返回 `false`——web
    /// 自动走 RGBA8 回退，无需 cfg 分支。
    pub fn supports_bc(&self) -> bool {
        self.features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    }

    /// 遮挡查询能力（R53）。wgpu 30 起遮挡查询是**核心能力**：WebGPU
    /// 规范把它从特性位升格为必选实现，`wgpu::Features` 中已无
    /// `OCCLUSION_QUERY` 位，各后端（Vulkan / DX12 / Metal / GL / WebGPU）
    /// 全部实现——因此本方法恒返回 `true`，仅作为能力探测点保留
    /// （若未来出现不支持的降级后端，改这里即可，调用方语义不变）。
    /// 真正的「无能力回退 = 全部可见」在渲染器执行路径：查询结果
    /// 迟到一帧即丢弃、不剔除（见 `occlusion.rs` 模块文档）。
    pub fn supports_occlusion_query(&self) -> bool {
        true
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
