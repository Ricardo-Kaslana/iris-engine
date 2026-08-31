//! 渲染管线构建：WGSL 着色器 + 绑定组布局 + 管线状态对象。

/// 阴影贴图分辨率（正方形边长）。
pub const SHADOW_MAP_SIZE: u32 = 2048;

/// 点光 cubemap 阴影贴图分辨率（每面边长；6 面 × 512² R32Float）。
pub const POINT_SHADOW_MAP_SIZE: u32 = 512;

/// HDR 中间缓冲格式（线性、单采样，供色调映射 pass 采样）。
pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// 主管线、蒙皮管线、阴影管线（常规 + 蒙皮变体）、天空盒、泛光管线
/// （提取 + 模糊）与色调映射管线，以及全部绑定组布局。
pub(crate) struct PipelineBundle {
    pub pipeline: wgpu::RenderPipeline,
    /// 蒙皮变体：顶点多一路蒙皮缓冲，组 2 绑定关节矩阵 storage。
    pub skinned_pipeline: wgpu::RenderPipeline,
    pub sky_pipeline: wgpu::RenderPipeline,
    pub shadow_pipeline: wgpu::RenderPipeline,
    /// 阴影蒙皮变体（组 1 绑定关节矩阵，保证阴影随骨骼变形）。
    pub skinned_shadow_pipeline: wgpu::RenderPipeline,
    /// 点光 cubemap 阴影管线（每面独立 uniform + 6 个 pass）。
    pub point_shadow_pipeline: wgpu::RenderPipeline,
    /// 点光阴影蒙皮变体（组 1 绑定关节矩阵）。
    pub skinned_point_shadow_pipeline: wgpu::RenderPipeline,
    pub blit_pipeline: wgpu::RenderPipeline,
    pub bloom_threshold_pipeline: wgpu::RenderPipeline,
    pub bloom_blur_pipeline: wgpu::RenderPipeline,
    /// 2D UI 叠加层管线（像素坐标顶点 + 字形图集采样，alpha 混合）。
    pub ui_pipeline: wgpu::RenderPipeline,
    pub globals_layout: wgpu::BindGroupLayout,
    /// 阴影 pass 专用的精简全局布局（只含 uniform，
    /// 不绑定阴影贴图本身——避免同 pass 内读写冲突）。
    pub shadow_globals_layout: wgpu::BindGroupLayout,
    /// 点光阴影每面参数布局：binding 0 = 独立小 uniform
    /// （面视图投影矩阵 + 灯位置/range；顶点算裁剪、片段算归一化距离）。
    pub point_shadow_layout: wgpu::BindGroupLayout,
    pub material_layout: wgpu::BindGroupLayout,
    /// 关节矩阵只读 storage（蒙皮管线专用）。
    pub joints_layout: wgpu::BindGroupLayout,
    pub blit_layout: wgpu::BindGroupLayout,
    pub bloom_layout: wgpu::BindGroupLayout,
    /// UI 绑定组布局：binding 0 = 字形图集纹理、1 = 采样器、
    /// 2 = 屏幕尺寸 uniform（顶点阶段）。
    pub ui_layout: wgpu::BindGroupLayout,
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
            // IBL 分割求和：辐照度立方体贴图 + 预滤波环境立方体贴图 + BRDF LUT
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::Cube,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::Cube,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 5,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 6,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            // 点光 cubemap 阴影贴图（R32Float 存归一化线性距离）。
            // 采样复用 binding 2 的 NonFiltering 采样器：R32Float 双线性过滤
            // 需要 float32-filterable 扩展，不依赖；与方向光阴影一致。
            wgpu::BindGroupLayoutEntry {
                binding: 7,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::Cube,
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                },
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

    // 关节矩阵只读 storage：蒙皮管线在顶点阶段读取（只读 storage 允许出现在
    // 顶点阶段；每具蒙皮一个缓冲，动画关节矩阵每帧整体覆写）
    let joints_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.layout.joints"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
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

    // 蒙皮主管线：顶点输入 = [常规顶点, 蒙皮属性, 实例]，
    // 绑定组 = [全局, 材质, 关节矩阵 storage]；顶点着色器 vs_skinned。
    let skinned_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("iris.skinned.pipeline.layout"),
        bind_group_layouts: &[Some(&globals_layout), Some(&material_layout), Some(&joints_layout)],
        immediate_size: 0,
    });
    let skinned_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("iris.pipeline.skinned"),
        layout: Some(&skinned_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_skinned"),
            compilation_options: Default::default(),
            buffers: &[
                Some(crate::mesh::Vertex::LAYOUT),
                Some(crate::mesh::SkinVertex::LAYOUT),
                Some(crate::instance::InstanceRaw::LAYOUT),
            ],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
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

    // 天空盒：全屏三角形按像素射线采样预滤波环境贴图 0 级 mip（= 原始天空）。
    // 主 pass 内最先绘制；深度只测不写（Always + write false），几何体随后覆盖。
    let sky_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.skybox"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/skybox.wgsl").into()),
    });
    let sky_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("iris.sky.pipeline.layout"),
        bind_group_layouts: &[Some(&globals_layout)],
        immediate_size: 0,
    });
    let sky_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("iris.pipeline.sky"),
        layout: Some(&sky_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &sky_shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &sky_shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: HDR_FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Always),
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

    // 阴影蒙皮变体：组 1 绑定关节矩阵，蒙皮几何体的阴影随骨骼变形。
    let skinned_shadow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("iris.skinned.shadow.pipeline.layout"),
        bind_group_layouts: &[Some(&shadow_globals_layout), Some(&joints_layout)],
        immediate_size: 0,
    });
    let skinned_shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("iris.pipeline.skinned_shadow"),
        layout: Some(&skinned_shadow_layout),
        vertex: wgpu::VertexState {
            module: &shadow_shader,
            entry_point: Some("vs_skinned"),
            compilation_options: Default::default(),
            buffers: &[
                Some(crate::mesh::Vertex::LAYOUT),
                Some(crate::mesh::SkinVertex::LAYOUT),
                Some(crate::instance::InstanceRaw::LAYOUT),
            ],
        },
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
            cull_mode: None,
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
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    // 点光 cubemap 阴影：每面一个独立小 uniform（80B：面视图投影矩阵 +
    // 灯位置/range），顶点算裁剪位置、片段算归一化线性距离写入 R32Float。
    // 一面一缓冲而非一份缓冲的六个插槽：绑定偏移必须满足
    // min_uniform_buffer_offset_alignment（典型 256），且同一 submit 内
    // 对同一缓冲的多次 write_buffer 会在执行前全部合并（泛光参数的教训）。
    let point_shadow_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.layout.point_shadow"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let point_shadow_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("iris.point_shadow.pipeline.layout"),
        bind_group_layouts: &[Some(&point_shadow_layout)],
        immediate_size: 0,
    });
    // 阴影面管线与方向光阴影同构：R32Float 颜色附件 + Depth32Float 深度附件
    // （Less 测试保证每 texel 存最近遮挡物）+ cull None（面翻转与环绕方向的
    // 实证结论见方向光阴影管线）。
    let point_shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("iris.pipeline.point_shadow"),
        layout: Some(&point_shadow_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shadow_shader,
            entry_point: Some("vs_point"),
            compilation_options: Default::default(),
            buffers: &[Some(crate::mesh::Vertex::LAYOUT), Some(crate::instance::InstanceRaw::LAYOUT)],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shadow_shader,
            entry_point: Some("fs_point"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::R32Float,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
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
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    // 点光阴影蒙皮变体：组 1 绑定关节矩阵，蒙皮几何体的点光阴影随骨骼变形。
    let skinned_point_shadow_layout =
        device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("iris.skinned.point_shadow.pipeline.layout"),
            bind_group_layouts: &[Some(&point_shadow_layout), Some(&joints_layout)],
            immediate_size: 0,
        });
    let skinned_point_shadow_pipeline =
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("iris.pipeline.skinned_point_shadow"),
            layout: Some(&skinned_point_shadow_layout),
            vertex: wgpu::VertexState {
                module: &shadow_shader,
                entry_point: Some("vs_point_skinned"),
                compilation_options: Default::default(),
                buffers: &[
                    Some(crate::mesh::Vertex::LAYOUT),
                    Some(crate::mesh::SkinVertex::LAYOUT),
                    Some(crate::instance::InstanceRaw::LAYOUT),
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shadow_shader,
                entry_point: Some("fs_point"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::R32Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
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
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

    // 色调映射 blit：全屏三角形采样 HDR 中间缓冲 + 泛光叠加，输出到 sRGB 目标
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
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
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

    // 泛光：半分辨率亮度提取（软阈值）+ 可分离高斯模糊（水平/垂直共用一条管线，
    // 方向由 uniform 决定）。两个 pass 输出到同规格的 Rgba16Float 中间缓冲。
    let bloom_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.bloom"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/bloom.wgsl").into()),
    });
    let bloom_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.layout.bloom"),
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
    let bloom_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("iris.bloom.pipeline.layout"),
        bind_group_layouts: &[Some(&bloom_layout)],
        immediate_size: 0,
    });
    let bloom_target = &[Some(wgpu::ColorTargetState {
        format: HDR_FORMAT,
        blend: None,
        write_mask: wgpu::ColorWrites::ALL,
    })];
    let make_bloom_pipeline = |label: &'static str, fs: &'static str| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&bloom_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &bloom_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &bloom_shader,
                entry_point: Some(fs),
                compilation_options: Default::default(),
                targets: bloom_target,
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
    let bloom_threshold_pipeline = make_bloom_pipeline("iris.pipeline.bloom_threshold", "fs_threshold");
    let bloom_blur_pipeline = make_bloom_pipeline("iris.pipeline.bloom_blur", "fs_blur");

    // 2D UI 叠加层：像素坐标顶点 + 字形图集采样。写 blit 之后的 sRGB 目标：
    // 无深度附件（叠加而非遮挡）、cull None（UI 面向屏幕）、无 MSAA
    // （目标是 blit 后的单采样缓冲）、标准 alpha 混合（文本/半透明矩形）。
    let ui_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.ui"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/ui.wgsl").into()),
    });
    let ui_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.layout.ui"),
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
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let ui_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("iris.ui.pipeline.layout"),
        bind_group_layouts: &[Some(&ui_layout)],
        immediate_size: 0,
    });
    let ui_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("iris.pipeline.ui"),
        layout: Some(&ui_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &ui_shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(crate::ui::UiVertex::LAYOUT)],
        },
        fragment: Some(wgpu::FragmentState {
            module: &ui_shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: color_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    PipelineBundle {
        pipeline,
        skinned_pipeline,
        sky_pipeline,
        shadow_pipeline,
        skinned_shadow_pipeline,
        point_shadow_pipeline,
        skinned_point_shadow_pipeline,
        blit_pipeline,
        bloom_threshold_pipeline,
        bloom_blur_pipeline,
        ui_pipeline,
        globals_layout,
        shadow_globals_layout,
        point_shadow_layout,
        material_layout,
        joints_layout,
        blit_layout,
        bloom_layout,
        ui_layout,
    }
}
