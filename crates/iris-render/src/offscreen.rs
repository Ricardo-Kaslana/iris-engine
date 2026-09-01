//! 离屏渲染器：无窗口渲染到纹理并读回像素。
//!
//! 用途：无头环境运行（CI）、集成测试、截图导出。无可用 GPU 适配器时
//! 构造返回 `None`，调用方可优雅降级。

use std::sync::Arc;

use crate::SAMPLE_COUNT;
use crate::context::Gpu;
use crate::renderer::Renderer;

pub struct OffscreenRenderer {
    gpu: Arc<Gpu>,
    renderer: Renderer,
    color: wgpu::Texture,
    color_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    readback: wgpu::Buffer,
    width: u32,
    height: u32,
    padded_bytes_per_row: u32,
}

impl OffscreenRenderer {
    /// 创建离屏渲染器。无适配器 / 设备创建失败时返回 `None`。
    pub fn new(width: u32, height: u32) -> Option<Self> {
        // 平台原因（wasm32）：wgpu 的 WebGPU 后端类型内嵌 JS 句柄、非
        // Send + Sync，clippy::arc_with_non_send_sync 只在 wasm 目标触发。
        // native 上 `Gpu` 恒为 Send + Sync（跨线程共享渲染器是既有语义）；
        // wasm 单线程事件循环内不会跨线程使用，Arc 语义不变。
        #[allow(clippy::arc_with_non_send_sync)]
        let gpu = Arc::new(Gpu::offscreen()?);
        let device = &gpu.device;
        let width = width.max(1);
        let height = height.max(1);
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        // blit 输出目标（COPY_SRC 用于读回）；MSAA/HDR 由 renderer 内部管理
        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.offscreen.color"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.offscreen.depth"),
            size,
            mip_level_count: 1,
            sample_count: SAMPLE_COUNT,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        // 读回缓冲：每行 256 字节对齐
        let raw_bytes_per_row = width * 4;
        let padded_bytes_per_row = raw_bytes_per_row.div_ceil(256) * 256;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.offscreen.readback"),
            size: (padded_bytes_per_row * height) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let renderer = Renderer::new(gpu.clone(), wgpu::TextureFormat::Rgba8UnormSrgb);
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
        Some(Self {
            gpu,
            renderer,
            color,
            color_view,
            depth_view: depth.create_view(&wgpu::TextureViewDescriptor::default()),
            readback,
            width,
            height,
            padded_bytes_per_row,
        })
    }

    /// 渲染一帧到离屏纹理，并把结果拷贝到读回缓冲。
    pub fn render(&mut self, world: &iris_core::world::World) {
        self.renderer.draw(
            world,
            &self.color_view,
            &self.depth_view,
            self.width,
            self.height,
        );
        // 颜色纹理 -> 读回缓冲（同一队列，顺序保证在绘制之后）
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("iris.offscreen.copy"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.color,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_bytes_per_row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        self.gpu.queue.submit([encoder.finish()]);
    }

    /// 阻塞读回 RGBA8 像素（sRGB 编码，行主序，已去除对齐填充）。
    pub fn read_pixels(&self) -> Vec<u8> {
        let (sender, receiver) = std::sync::mpsc::channel();
        self.readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        let _ = self.gpu.device.poll(wgpu::PollType::wait_indefinitely());
        receiver
            .recv()
            .expect("映射回调未送达")
            .expect("映射读回缓冲失败");
        let data = self
            .readback
            .get_mapped_range(..)
            .expect("读取已映射的读回缓冲不应失败");
        let raw_per_row = (self.width * 4) as usize;
        let mut out = Vec::with_capacity(raw_per_row * self.height as usize);
        for row in 0..self.height as usize {
            let start = row * self.padded_bytes_per_row as usize;
            out.extend_from_slice(&data[start..start + raw_per_row]);
        }
        drop(data);
        self.readback.unmap();
        out
    }

    /// 将最近一帧保存为 24 位 BMP 文件。
    pub fn save_bmp(&self, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
        let pixels = self.read_pixels();
        let (w, h) = (self.width, self.height);
        let row_padding = (4 - (w * 3) % 4) % 4;
        let pixel_bytes = (w * 3 + row_padding) * h;
        let mut buf = Vec::with_capacity((54 + pixel_bytes) as usize);
        // BITMAPFILEHEADER
        buf.extend_from_slice(b"BM");
        buf.extend_from_slice(&(54u32 + pixel_bytes).to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&54u32.to_le_bytes());
        // BITMAPINFOHEADER
        buf.extend_from_slice(&40u32.to_le_bytes());
        buf.extend_from_slice(&(w as i32).to_le_bytes());
        buf.extend_from_slice(&(h as i32).to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&24u16.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
        buf.extend_from_slice(&pixel_bytes.to_le_bytes());
        buf.extend_from_slice(&2835u32.to_le_bytes());
        buf.extend_from_slice(&2835u32.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        // 像素：自底向上、BGR
        for y in (0..h).rev() {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                buf.push(pixels[i + 2]);
                buf.push(pixels[i + 1]);
                buf.push(pixels[i]);
            }
            // 行末 4 字节对齐填充（BGR 行长补零到 4 的倍数）
            buf.extend(std::iter::repeat_n(0, row_padding as usize));
        }
        std::fs::write(path, buf)
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn renderer(&self) -> &Renderer {
        &self.renderer
    }

    pub fn gpu(&self) -> &Gpu {
        &self.gpu
    }
}
