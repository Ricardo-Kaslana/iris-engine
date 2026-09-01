//! 环境光照（IBL）：天空立方体贴图烘焙 + 分割求和预计算。
//!
//! 渲染器初始化时一次性烘焙四份产物，运行时主 pass 只做三次采样、零开销：
//! 1. 天空环境立方体贴图（程序化渐变 + HDR 太阳盘，或外部 equirectangular
//!    Radiance `.hdr` 全景图——见 [`SkySource`]）
//! 2. 漫反射辐照度立方体贴图（余弦加权半球卷积）
//! 3. 镜面预滤波立方体贴图（GGX 重要性采样，每级 mip 对应一档粗糙度）
//! 4. BRDF LUT（分割求和的环境 BRDF 积分，uv = (NdotV, roughness)）
//!
//! 近似来源：Epic "Split Sum"（Karis 2010）/ LearnOpenGL IBL 章节。

use std::sync::atomic::{AtomicU64, Ordering};

use crate::asset::hdr::HdrImage;
use crate::context::Gpu;

/// 天空环境立方体贴图边长。
pub const ENV_MAP_SIZE: u32 = 128;
/// 预滤波 mip 级数（粗糙度 0..1 线性映射到 mip 层）。
pub const ENV_MIP_COUNT: u32 = 5;
/// BRDF LUT 边长。
pub const BRDF_LUT_SIZE: u32 = 512;

/// 环境光强度（World 资源，缺省 1.0；0 = 整帧关闭 IBL，便于测试与风格化）。
#[derive(Clone, Copy, Debug, iris_core::Resource)]
pub struct EnvironmentSettings {
    pub intensity: f32,
}

impl Default for EnvironmentSettings {
    fn default() -> Self {
        Self { intensity: 1.0 }
    }
}

/// 环境天空源（World 资源）。缺席 = 程序化天空（保持零资产依赖的开箱行为）。
///
/// `generation` 是失效标记：每次通过 [`SkySource::equirect_hdr`] 换入外部
/// 贴图都会从全局计数器取一个新值，渲染器在 `draw` 开头比较缓存值，不同
/// 则重烘焙 IBL 并重建全局绑定组。手工构造的资源务必沿用该约定（generation
/// 0 = 程序化天空）。
#[derive(Clone, Debug, iris_core::Resource)]
pub struct SkySource {
    pub kind: SkySourceKind,
    pub generation: u64,
}

/// 环境天空源的种类。
#[derive(Clone, Debug)]
pub enum SkySourceKind {
    /// 程序化天空（天顶蓝 → 地平线白 + HDR 太阳盘）。
    ProceduralSky,
    /// 外部 equirectangular 全景图（约定：图像第 0 行 = +Y 天顶）。
    EquirectHdr(HdrImage),
}

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(0);

impl Default for SkySource {
    fn default() -> Self {
        Self {
            kind: SkySourceKind::ProceduralSky,
            generation: 0,
        }
    }
}

impl SkySource {
    /// 用外部 HDR 全景图构造天空源，generation 取自全局递增计数——
    /// 同一图像换入两次也会得到不同 generation，保证渲染器必然重烘。
    pub fn equirect_hdr(image: HdrImage) -> Self {
        Self {
            kind: SkySourceKind::EquirectHdr(image),
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed) + 1,
        }
    }
}

/// 烘焙产物（纹理随 `Renderer` 存活，视图进全局绑定组）。
pub(crate) struct EnvironmentMaps {
    #[allow(dead_code)]
    irradiance: wgpu::Texture,
    pub irradiance_view: wgpu::TextureView,
    #[allow(dead_code)]
    prefiltered: wgpu::Texture,
    pub prefiltered_view: wgpu::TextureView,
    #[allow(dead_code)]
    brdf_lut: wgpu::Texture,
    pub brdf_view: wgpu::TextureView,
}

/// 一次性烘焙全部环境贴图（程序化天空源）。
pub(crate) fn bake_environment(gpu: &Gpu) -> EnvironmentMaps {
    bake_environment_from(gpu, &SkySourceKind::ProceduralSky)
}

/// f32 → f16 位模式（舍入到最近偶数）。wgpu 未重导出 half crate，
/// 而环境贴图上传只需要这一处转换——手写位操作并配单测锁定。
fn f32_to_f16_bits(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let abs = bits & 0x7fff_ffff;
    // NaN / 无穷（f16 同布局：5 位指数、10 位尾数）
    if abs >= 0x7f80_0000 {
        return if abs > 0x7f80_0000 {
            sign | 0x7e00
        } else {
            sign | 0x7c00
        };
    }
    // 舍入到最近偶数：加「被丢弃的 13 位之半 + 进位提示位」
    let rounded = abs.wrapping_add(0x0fff + ((abs >> 13) & 1));
    if rounded >= 0x7f80_0000 {
        return sign | 0x7c00; // 舍入后超出 f16 范围 → 无穷
    }
    let e16 = (rounded >> 23) as i32 - 127 + 15;
    if e16 >= 0x1f {
        return sign | 0x7c00;
    }
    if e16 > 0 {
        // 规格化：指数偏置 15 + 尾数高 10 位
        let mantissa = ((rounded >> 13) & 0x03ff) as u16;
        return sign | ((e16 as u16) << 10) | mantissa;
    }
    // f16 非规格化 / 零：带隐藏位的 11 位尾数右移（f32 非规格化必归零）
    let mantissa = ((rounded >> 13) & 0x03ff) | 0x0400;
    let shift = (1 - e16).clamp(0, 31) as u32;
    sign | (mantissa >> shift) as u16
}

/// 把 equirect 全景图上传为 Rgba16Float 2D 纹理（捕获 pass 的采样源）。
fn upload_equirect_texture(gpu: &Gpu, image: &HdrImage) -> wgpu::Texture {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("iris.env.equirect"),
        size: wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    // write_texture 的 bytes_per_row 必须 256 对齐：按最大行宽建暂存，
    // 逐行写入有效数据、行尾留填充（纹理本体只取有效部分）
    let row_bytes = image.width as usize * 8;
    let stride = row_bytes.div_ceil(256) * 256;
    let mut staged = vec![0u8; stride * image.height as usize];
    for (y, row) in staged.chunks_mut(stride).enumerate() {
        for x in 0..image.width as usize {
            let [r, g, b] = image.pixel(x as u32, y as u32);
            let f16: [u16; 4] = [
                f32_to_f16_bits(r),
                f32_to_f16_bits(g),
                f32_to_f16_bits(b),
                f32_to_f16_bits(1.0),
            ];
            row[x * 8..x * 8 + 8].copy_from_slice(bytemuck::cast_slice(&f16));
        }
    }
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &staged,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(stride as u32),
            rows_per_image: Some(image.height),
        },
        wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        },
    );
    texture
}

/// 按指定天空源一次性烘焙全部环境贴图。pass 参数（面索引/粗糙度）经
/// 256B 步长的动态偏移 uniform 传入——单次 submit 内多次 write_buffer 会在
/// 命令缓冲执行前全部完成，逐 pass 复用同一 16B 缓冲会读到最后一帧参数。
pub(crate) fn bake_environment_from(gpu: &Gpu, source: &SkySourceKind) -> EnvironmentMaps {
    let device = &gpu.device;
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.environment"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/environment.wgsl").into()),
    });

    let make_cube = |label: &'static str, mips: u32| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: ENV_MAP_SIZE,
                height: ENV_MAP_SIZE,
                depth_or_array_layers: 6,
            },
            mip_level_count: mips,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    };
    let sky = make_cube("iris.env.sky", 1);
    let irradiance = make_cube("iris.env.irradiance", 1);
    let prefiltered = make_cube("iris.env.prefiltered", ENV_MIP_COUNT);
    let brdf_lut = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("iris.env.brdf_lut"),
        size: wgpu::Extent3d {
            width: BRDF_LUT_SIZE,
            height: BRDF_LUT_SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let cube_view = |tex: &wgpu::Texture| {
        tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::Cube),
            ..Default::default()
        })
    };
    let sky_cube_view = cube_view(&sky);
    // 面片目标视图：D2 单层（可指定 mip），供渲染通道作颜色附件
    let face_view = |tex: &wgpu::Texture, face: u32, mip: u32| {
        tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2),
            base_array_layer: face,
            array_layer_count: Some(1),
            base_mip_level: mip,
            mip_level_count: Some(1),
            ..Default::default()
        })
    };

    // 烘焙 pass 绑定布局：plain（天空 / BRDF，只读参数）与
    // full（辐照度 / 预滤波，加采样源环境立方体贴图）
    let uniform_entry = |dynamic: bool| wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: dynamic,
            min_binding_size: None,
        },
        count: None,
    };
    let plain_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.env.layout.plain"),
        entries: &[uniform_entry(true)],
    });
    let full_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.env.layout.full"),
        entries: &[
            uniform_entry(true),
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::Cube,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("iris.env.bake.sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });

    // pass 参数槽位：每个 pass 占 256B（动态偏移对齐要求），共 sky 6 +
    // irradiance 6 + prefilter 6×mips + brdf 1
    const SKY_BASE: usize = 0;
    const IRR_BASE: usize = 6;
    const PRE_BASE: usize = 12;
    const BRDF_SLOT: usize = PRE_BASE + 6 * ENV_MIP_COUNT as usize;
    const SLOTS: usize = BRDF_SLOT + 1;
    const STRIDE: u64 = 256;
    let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("iris.env.params"),
        size: SLOTS as u64 * STRIDE,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut params_data = vec![0u8; SLOTS * STRIDE as usize];
    let mut write_slot = |slot: usize, face: f32, roughness: f32| {
        // 写入步长必须与烘焙 pass 的动态偏移步长（STRIDE=256）一致：
        // slot k 的参数必须落在字节 k·256 处，pass k 才能在偏移 k·256 读到它
        let base = slot * STRIDE as usize;
        let bytes: [u8; 16] = bytemuck::cast([face, roughness, 0.0, 0.0]);
        params_data[base..base + 16].copy_from_slice(&bytes);
    };
    for face in 0..6u32 {
        write_slot(SKY_BASE + face as usize, face as f32, 0.0);
        write_slot(IRR_BASE + face as usize, face as f32, 0.0);
    }
    for mip in 0..ENV_MIP_COUNT {
        for face in 0..6u32 {
            write_slot(
                PRE_BASE + (mip * 6 + face) as usize,
                face as f32,
                mip as f32 / (ENV_MIP_COUNT - 1) as f32,
            );
        }
    }
    write_slot(BRDF_SLOT, 0.0, 0.0);
    gpu.queue.write_buffer(&params_buf, 0, &params_data);

    // equirect 捕获路径的绑定资源：uniform + 2D 全景纹理 + 采样器。
    // 程序化路径不创建（wgsl 入口不静态使用绑定时无需提供）。
    let equirect = match source {
        SkySourceKind::ProceduralSky => None,
        SkySourceKind::EquirectHdr(image) => {
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("iris.env.layout.equirect"),
                entries: &[
                    uniform_entry(true),
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });
            let texture = upload_equirect_texture(gpu, image);
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("iris.env.bg.equirect"),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &params_buf,
                            offset: 0,
                            size: std::num::NonZeroU64::new(16),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            });
            Some((layout, texture, view, bind_group))
        }
    };

    let plain_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("iris.env.bg.plain"),
        layout: &plain_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &params_buf,
                offset: 0,
                size: std::num::NonZeroU64::new(16),
            }),
        }],
    });
    let full_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("iris.env.bg.full"),
        layout: &full_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &params_buf,
                    offset: 0,
                    size: std::num::NonZeroU64::new(16),
                }),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&sky_cube_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let make_pipeline = |label, fs: &'static str, layout: &wgpu::BindGroupLayout| {
        let pipe_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: &[Some(layout)],
            immediate_size: 0,
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&pipe_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(fs),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba16Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        })
    };
    let sky_pipeline = make_pipeline("iris.env.pipeline.sky", "fs_sky", &plain_layout);
    let equirect_pipeline = equirect.as_ref().map(|(layout, ..)| {
        make_pipeline("iris.env.pipeline.equirect", "fs_capture_equirect", layout)
    });
    let irradiance_pipeline = make_pipeline(
        "iris.env.pipeline.irradiance",
        "fs_irradiance",
        &full_layout,
    );
    let prefilter_pipeline =
        make_pipeline("iris.env.pipeline.prefilter", "fs_prefilter", &full_layout);
    let brdf_pipeline = make_pipeline("iris.env.pipeline.brdf", "fs_brdf", &plain_layout);

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("iris.env.bake"),
    });
    let mut pass = |view: &wgpu::TextureView,
                    pipeline: &wgpu::RenderPipeline,
                    bg: &wgpu::BindGroup,
                    offset: u32,
                    size: (u32, u32)| {
        let mut p = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("iris.env.bake.pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        p.set_pipeline(pipeline);
        p.set_bind_group(0, bg, &[offset]);
        p.draw(0..3, 0..1);
        let _ = size;
    };
    // 1. 天空捕获（写 sky 面：程序化走 shader 内 sky(dir)；外部 equirect 走
    //    2D 全景采样。两条路径的 bind group 均未引用 sky 立方体，无读写冲突）
    let (sky_pass_pipeline, sky_pass_bg): (&wgpu::RenderPipeline, &wgpu::BindGroup) =
        match &equirect {
            None => (&sky_pipeline, &plain_bg),
            Some((.., bg)) => (
                equirect_pipeline.as_ref().expect("equirect 管线应已创建"),
                bg,
            ),
        };
    for face in 0..6u32 {
        pass(
            &face_view(&sky, face, 0),
            sky_pass_pipeline,
            sky_pass_bg,
            (SKY_BASE + face as usize) as u32 * 256,
            (ENV_MAP_SIZE, ENV_MAP_SIZE),
        );
    }
    // 2. 漫反射辐照度（读 sky 立方体）
    for face in 0..6u32 {
        pass(
            &face_view(&irradiance, face, 0),
            &irradiance_pipeline,
            &full_bg,
            (IRR_BASE + face as usize) as u32 * 256,
            (ENV_MAP_SIZE, ENV_MAP_SIZE),
        );
    }
    // 3. GGX 预滤波（每级 mip 一档粗糙度）
    for mip in 0..ENV_MIP_COUNT {
        for face in 0..6u32 {
            pass(
                &face_view(&prefiltered, face, mip),
                &prefilter_pipeline,
                &full_bg,
                (PRE_BASE + (mip * 6 + face) as usize) as u32 * 256,
                (ENV_MAP_SIZE >> mip, ENV_MAP_SIZE >> mip),
            );
        }
    }
    // 4. BRDF LUT
    pass(
        &brdf_lut.create_view(&wgpu::TextureViewDescriptor::default()),
        &brdf_pipeline,
        &plain_bg,
        BRDF_SLOT as u32 * 256,
        (BRDF_LUT_SIZE, BRDF_LUT_SIZE),
    );
    gpu.queue.submit([encoder.finish()]);

    EnvironmentMaps {
        irradiance_view: cube_view(&irradiance),
        irradiance,
        prefiltered_view: cube_view(&prefiltered),
        prefiltered,
        brdf_view: brdf_lut.create_view(&wgpu::TextureViewDescriptor::default()),
        brdf_lut,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// f32 → f16 位模式：规格化、舍入到最近偶数、特殊值。
    #[test]
    fn f16_conversion_bit_patterns() {
        assert_eq!(f32_to_f16_bits(0.0), 0x0000);
        assert_eq!(f32_to_f16_bits(-0.0), 0x8000);
        assert_eq!(f32_to_f16_bits(1.0), 0x3c00);
        assert_eq!(f32_to_f16_bits(-2.0), 0xc000);
        assert_eq!(f32_to_f16_bits(4.0), 0x4400);
        assert_eq!(f32_to_f16_bits(0.5), 0x3800);
        // f16 最大规格化值 65504 与溢出 → 无穷
        assert_eq!(f32_to_f16_bits(65504.0), 0x7bff);
        assert_eq!(f32_to_f16_bits(65536.0), 0x7c00);
        assert_eq!(f32_to_f16_bits(1.0e30), 0x7c00);
        assert_eq!(f32_to_f16_bits(f32::INFINITY), 0x7c00);
        assert_eq!(f32_to_f16_bits(f32::NEG_INFINITY), 0xfc00);
        assert_eq!(f32_to_f16_bits(f32::NAN) & 0x7e00, 0x7e00);
        // 舍入到最近偶数：1 + 2^-11 恰为 1.0 与 1+2^-10 的中点 → 取偶数 1.0；
        // 1 + 1.5×2^-10 恰为 1+2^-10 与 1+2×2^-10 的中点 → 取偶数尾数 2 → 0x3c02
        assert_eq!(f32_to_f16_bits(1.0 + 2.0f32.powi(-11)), 0x3c00);
        assert_eq!(f32_to_f16_bits(1.0 + 1.5 * 2.0f32.powi(-10)), 0x3c02);
        // 最小 f16 规格化值 2^-14 与非规格化区间（2^-15 → 尾数 0x200）
        assert_eq!(f32_to_f16_bits(2.0f32.powi(-14)), 0x0400);
        assert_eq!(f32_to_f16_bits(2.0f32.powi(-15)), 0x0200);
        assert_eq!(f32_to_f16_bits(2.0f32.powi(-24)), 0x0001);
        assert_eq!(f32_to_f16_bits(2.0f32.powi(-25)), 0x0000);
    }

    /// 环境值域（0.03 ≈ 暗部 .. 40 ≈ 太阳盘）经 f16 往返的相对误差必须
    /// 在半精度 2^-11 量级内——上传路径按位转换，解码后由 GPU 线性采样。
    #[test]
    fn f16_conversion_holds_hdr_range() {
        // 测试内独立实现 f16 → f32（按数学定义），与转换器交叉验证位布局
        let decode = |h: u16| -> f32 {
            let sign = if h & 0x8000 != 0 { -1.0f32 } else { 1.0 };
            let exp = ((h >> 10) & 0x1f) as i32;
            let frac = (h & 0x03ff) as f32;
            match exp {
                0 => sign * frac * 2f32.powi(-24),
                0x1f if frac == 0.0 => sign * f32::INFINITY,
                0x1f => f32::NAN,
                e => sign * (1.0 + frac / 1024.0) * 2f32.powi(e - 15),
            }
        };
        for value in [0.03125, 0.25, 1.0, 4.0, 40.0] {
            let back = decode(f32_to_f16_bits(value));
            let rel = ((back - value) / value).abs();
            assert!(rel < 1.0 / 1024.0, "{value} → {back} 相对误差 {rel}");
        }
    }

    /// SkySource generation 约定：程序化默认 0，equirect 便捷构造必递增。
    #[test]
    fn sky_source_generation_increments() {
        assert_eq!(SkySource::default().generation, 0);
        assert!(matches!(
            SkySource::default().kind,
            SkySourceKind::ProceduralSky
        ));
        let a = SkySource::equirect_hdr(HdrImage::new(4, 2));
        let b = SkySource::equirect_hdr(HdrImage::new(4, 2));
        assert_ne!(
            a.generation, 0,
            "首个外部源的 generation 不得与程序化天空冲突"
        );
        assert_ne!(a.generation, b.generation, "换一次资源 generation 必变");
        assert!(matches!(a.kind, SkySourceKind::EquirectHdr(_)));
    }
}
