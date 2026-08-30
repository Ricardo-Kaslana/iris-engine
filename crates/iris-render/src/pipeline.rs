//! 渲染管线构建：WGSL 着色器 + 绑定组布局 + 管线状态对象。

/// 阴影贴图分辨率（正方形边长）。
pub const SHADOW_MAP_SIZE: u32 = 2048;

/// HDR 中间缓冲格式（线性、单采样，供色调映射 pass 采样）。
pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// 主管线、阴影深度管线与色调映射管线，以及全部绑定组布局。
pub(crate) struct PipelineBundle {
    pub pipeline: wgpu::RenderPipeline,
    pub shadow_pipeline: wgpu::RenderPipeline,
    pub blit_pipeline: wgpu::RenderPipeline,
    pub globals_layout: wgpu::BindGroupLayout,
    /// 阴影 pass 专用的精简全局布局（只含 uniform，
    /// 不绑定阴影贴图本身——避免同 pass 内读写冲突）。
    pub shadow_globals_layout: wgpu::BindGroupLayout,
    pub material_layout: wgpu::BindGroupLayout,
    pub blit_layout: wgpu::BindGroupLayout,
}

pub(crate) fn create_pipeline(
    gpu: &crate::context::Gpu,
    color_format: wgpu::TextureFormat,
    sample_count: u32,
) -> PipelineBundle {
    let device = &gpu.device;
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.standard"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/standard.wgsl").into()),
    });
    let shadow_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.shadow"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/shadow.wgsl").into()),
    });

    // 全局组：uniform + 阴影深度纹理（Rgba32Float 颜色缓冲）+ 普通采样器。
    // 深度比较在 shader 内手工完成（见 standard.wgsl shadow_factor）。
    let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.layout.globals"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                count: None,
            },
        ],
    });

    let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.layout.material"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
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

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("iris.pipeline.layout"),
        bind_group_layouts: &[Some(&globals_layout), Some(&material_layout)],
        immediate_size: 0,
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("iris.pipeline.standard"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(crate::mesh::Vertex::LAYOUT), Some(crate::instance::InstanceRaw::LAYOUT)],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            // 场景渲染到线性 HDR 中间缓冲（色调映射在 blit 完成）
            targets: &[Some(wgpu::ColorTargetState {
                format: HDR_FORMAT,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: Some(wgpu::Face::Back),
            front_face: wgpu::FrontFace::Ccw,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: sample_count,
            ..Default::default()
        },
        multiview_mask: None,
        cache: None,
    });

    // 阴影 pass 的精简全局组：只含 uniform（VERTEX），不引用阴影贴图
    let shadow_globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.layout.shadow_globals"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });

    // 阴影管线：从光源视角把深度写入 Rgba32Float 颜色附件（详见 shadow.wgsl）。
    // 独立 pipeline layout：只含精简全局组，无需材质与阴影采样绑定。
    let shadow_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("iris.shadow.pipeline.layout"),
        bind_group_layouts: &[Some(&shadow_globals_layout)],
        immediate_size: 0,
    });
    let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("iris.pipeline.shadow"),
        layout: Some(&shadow_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shadow_shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(crate::mesh::Vertex::LAYOUT), Some(crate::instance::InstanceRaw::LAYOUT)],
        },
        // 颜色附件写入深度（Rgba32Float），无深度附件、无剔除
        fragment: Some(wgpu::FragmentState {
            module: &shadow_shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::R32Float,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            // 正面剔除在光空间下会错误剔除几乎所有几何（环绕方向与
            // 正交视图组合的实证结果），v1 使用 cull None，配合深度偏移抑制 acne。
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            // 深度测试保证颜色附件存最近面深度（无测试时背面深度会覆盖正面）
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    // 色调映射 blit：全屏三角形采样 HDR 中间缓冲，输出到 sRGB 目标
    let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.blit"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/blit.wgsl").into()),
    });
    let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.layout.blit"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("iris.blit.pipeline.layout"),
        bind_group_layouts: &[Some(&blit_layout)],
        immediate_size: 0,
    });
    let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("iris.pipeline.blit"),
        layout: Some(&blit_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &blit_shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &blit_shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: color_format,
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
    });

    PipelineBundle {
        pipeline,
        shadow_pipeline,
        blit_pipeline,
        globals_layout,
        shadow_globals_layout,
        material_layout,
        blit_layout,
    }
}
