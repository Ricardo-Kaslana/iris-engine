//! 程序化环境光照（IBL）：天空立方体贴图烘焙 + 分割求和预计算。
//!
//! 渲染器初始化时一次性烘焙四份产物，运行时主 pass 只做三次采样、零开销：
//! 1. 天空环境立方体贴图（程序化渐变 + HDR 太阳盘，无外部资产依赖）
//! 2. 漫反射辐照度立方体贴图（余弦加权半球卷积）
//! 3. 镜面预滤波立方体贴图（GGX 重要性采样，每级 mip 对应一档粗糙度）
//! 4. BRDF LUT（分割求和的环境 BRDF 积分，uv = (NdotV, roughness)）
//!
//! 近似来源：Epic "Split Sum"（Karis 2010）/ LearnOpenGL IBL 章节。

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

/// 一次性烘焙全部环境贴图。pass 参数（面索引/粗糙度）经 256B 步长的
/// 动态偏移 uniform 传入——单次 submit 内多次 write_buffer 会在命令
/// 缓冲执行前全部完成，逐 pass 复用同一 16B 缓冲会读到最后一帧参数。
pub(crate) fn bake_environment(gpu: &Gpu) -> EnvironmentMaps {
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
    let mut params_data = vec![0u8; SLOTS as usize * STRIDE as usize];
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
    let irradiance_pipeline =
        make_pipeline("iris.env.pipeline.irradiance", "fs_irradiance", &full_layout);
    let prefilter_pipeline = make_pipeline("iris.env.pipeline.prefilter", "fs_prefilter", &full_layout);
    let brdf_pipeline = make_pipeline("iris.env.pipeline.brdf", "fs_brdf", &plain_layout);

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("iris.env.bake"),
    });
    let mut pass = |view: &wgpu::TextureView, pipeline: &wgpu::RenderPipeline, bg: &wgpu::BindGroup, offset: u32, size: (u32, u32)| {
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
    // 1. 天空（写 sky 面，读 plain_bg：bind group 未引用 sky 立方体，无读写冲突）
    for face in 0..6u32 {
        pass(
            &face_view(&sky, face, 0),
            &sky_pipeline,
            &plain_bg,
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
