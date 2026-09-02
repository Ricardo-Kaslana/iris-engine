//! 渲染管线构建：WGSL 着色器 + 绑定组布局 + 管线状态对象。

/// 阴影贴图分辨率（正方形边长）。
pub const SHADOW_MAP_SIZE: u32 = 2048;

/// 方向光级联阴影（CSM）的级联数量（R29）。
///
/// 阴影贴图是 2048² × [`SHADOW_CASCADES`] 层的 `texture_2d_array`，
/// 每级联一次渲染 pass 写入对应层；片元按 view 深度选级联采样。
pub const SHADOW_CASCADES: usize = 3;

/// 方向光阴影的覆盖距离（世界单位，米；v1 常量）。
///
/// 级联沿相机视锥深度只铺到该距离：超出最远级联的片元**不投影、保持
/// 受光**（远处背景不吃阴影开销与精度）。更远的投影（如地形级阴影）
/// 留待后续路线。
pub const SHADOW_DISTANCE: f32 = 120.0;

/// 点光 cubemap 阴影贴图分辨率（每面边长；6 面 × 512² R32Float）。
pub const POINT_SHADOW_MAP_SIZE: u32 = 512;

/// HDR 中间缓冲格式（线性、单采样，供色调映射 pass 采样）。
pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// 主管线、蒙皮管线、透明管线（常规 + 蒙皮变体）、阴影管线（常规 + 蒙皮
/// 变体）、天空盒、后处理管线（泛光提取 + 模糊、暗角、色调映射 blit），
/// 以及全部绑定组布局。
pub(crate) struct PipelineBundle {
    pub pipeline: wgpu::RenderPipeline,
    /// 蒙皮变体：顶点多一路蒙皮缓冲，组 2 绑定关节矩阵 storage。
    pub skinned_pipeline: wgpu::RenderPipeline,
    /// 透明变体：alpha 混合 + 深度只测不写（透明 pass，见 renderer.rs）。
    pub transparent_pipeline: wgpu::RenderPipeline,
    /// 透明蒙皮变体：组 2 绑定关节矩阵，混合语义同上。
    pub transparent_skinned_pipeline: wgpu::RenderPipeline,
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
    /// 暗角 pass 管线（全分辨率 HDR → HDR，pass 串联框架的示范 pass）。
    pub vignette_pipeline: wgpu::RenderPipeline,
    /// 2D UI 叠加层管线（像素坐标顶点 + 字形图集采样，alpha 混合）。
    pub ui_pipeline: wgpu::RenderPipeline,
    /// 粒子 additive 管线（billboard 实例化，`SrcAlpha + One` 加法混合，
    /// 深度只测不写——火焰 / 火花类高亮粒子，叠加顺序无关）。
    pub particle_additive_pipeline: wgpu::RenderPipeline,
    /// 粒子 alpha 管线（标准 alpha 混合 + 深度只测不写，与透明管线参数
    /// 一致；实例按视深降序绘制）。
    pub particle_alpha_pipeline: wgpu::RenderPipeline,
    pub globals_layout: wgpu::BindGroupLayout,
    /// 级联阴影 pass 的每级联精简布局：binding 0 = 64B 级联视图投影矩阵
    /// uniform（顶点算裁剪位置；一缓冲一级联，同 pass 内不读写阴影贴图）。
    pub cascade_layout: wgpu::BindGroupLayout,
    /// 点光阴影每面参数布局：binding 0 = 独立小 uniform
    /// （面视图投影矩阵 + 灯位置/range；顶点算裁剪、片段算归一化距离）。
    pub point_shadow_layout: wgpu::BindGroupLayout,
    pub material_layout: wgpu::BindGroupLayout,
    /// 关节矩阵只读 storage（蒙皮管线专用）。
    pub joints_layout: wgpu::BindGroupLayout,
    pub blit_layout: wgpu::BindGroupLayout,
    /// 后处理全屏 pass 通用组布局：binding 0 = 参数 uniform、1 = 源纹理、
    /// 2 = 采样器（泛光 / 暗角等全部链上中间 pass 同构，pass 串联框架
    /// 据此用一个编码函数服务所有 pass）。
    pub post_layout: wgpu::BindGroupLayout,
    /// UI 绑定组布局：binding 0 = 字形图集纹理、1 = 采样器、
    /// 2 = 屏幕尺寸 uniform（顶点阶段）。
    pub ui_layout: wgpu::BindGroupLayout,
    /// 粒子相机布局：binding 0 = billboard 基向量小 uniform
    ///（相机 right / up，视图矩阵第 0 / 1 行，每帧 write_buffer）。
    pub particle_layout: wgpu::BindGroupLayout,
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

    // 全局组：uniform + 级联阴影贴图（R32Float 2D 数组，3 层）+ 普通采样器
    // + 级联参数 uniform（binding 8，R29 CSM）。
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
            // 方向光级联阴影贴图：R32Float 2D 数组（SHADOW_CASCADES 层，
            // 深度存 R 通道），片元按 view 深度选层采样。
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2Array,
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
            // 级联阴影参数（R29 CSM，224B）：3 × 光空间视图投影矩阵 +
            // 相机前向（片元 view 深度）+ 级联分割点。独立缓冲独立绑定槽，
            // 不扩 Globals（绑定号排在现有最大 binding 7 之后）。
            wgpu::BindGroupLayoutEntry {
                binding: 8,
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

    // 蒙皮管线布局：组 2 多绑关节矩阵 storage（顶点着色器 vs_skinned）。
    let skinned_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("iris.skinned.pipeline.layout"),
        bind_group_layouts: &[
            Some(&globals_layout),
            Some(&material_layout),
            Some(&joints_layout),
        ],
        immediate_size: 0,
    });

    // 主 pass 几何管线族构造：不透明 / 透明 × 普通 / 蒙皮共四条，顶点布局、
    // 拓扑（背面剔除 Ccw/Cull Back）、深度格式与绑定组全部一致，只有
    // 混合状态与深度写入按变体区分——
    // - 不透明：REPLACE + 深度写入（Less）；
    // - 透明：`ALPHA_BLENDING`（src α, 1 − src α，与 UI 管线一致）+
    //   深度只测不写（Less 测试保留、写入关闭）——写入会让后画的远透明面
    //   被先画的近透明面错误裁掉（共面透明对必然丢失其一），只测不写则
    //   透明仍被不透明几何正确遮挡。
    let make_mesh_pipeline = |label: &'static str,
                              entry_point: &'static str,
                              layout: &wgpu::PipelineLayout,
                              skinned: bool,
                              blend: wgpu::BlendState,
                              depth_write: bool|
     -> wgpu::RenderPipeline {
        let buffers: &[Option<wgpu::VertexBufferLayout<'static>>] = if skinned {
            &[
                Some(crate::mesh::Vertex::LAYOUT),
                Some(crate::mesh::SkinVertex::LAYOUT),
                Some(crate::instance::InstanceRaw::LAYOUT),
            ]
        } else {
            &[
                Some(crate::mesh::Vertex::LAYOUT),
                Some(crate::instance::InstanceRaw::LAYOUT),
            ]
        };
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                buffers,
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                // 场景渲染到线性 HDR 中间缓冲（色调映射在 blit 完成）
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR_FORMAT,
                    blend: Some(blend),
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
                depth_write_enabled: Some(depth_write),
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
        })
    };

    let pipeline = make_mesh_pipeline(
        "iris.pipeline.standard",
        "vs_main",
        &pipeline_layout,
        false,
        wgpu::BlendState::REPLACE,
        true,
    );
    // 蒙皮主管线：顶点输入 = [常规顶点, 蒙皮属性, 实例]，
    // 绑定组 = [全局, 材质, 关节矩阵 storage]；顶点着色器 vs_skinned。
    let skinned_pipeline = make_mesh_pipeline(
        "iris.pipeline.skinned",
        "vs_skinned",
        &skinned_pipeline_layout,
        true,
        wgpu::BlendState::REPLACE,
        true,
    );
    // 透明变体（普通 + 蒙皮）：alpha 混合 + 深度只测不写，
    // 绘制在不透明主 pass 之后、按视深降序（见 renderer.rs 透明 pass）。
    let transparent_pipeline = make_mesh_pipeline(
        "iris.pipeline.transparent",
        "vs_main",
        &pipeline_layout,
        false,
        wgpu::BlendState::ALPHA_BLENDING,
        false,
    );
    let transparent_skinned_pipeline = make_mesh_pipeline(
        "iris.pipeline.transparent_skinned",
        "vs_skinned",
        &skinned_pipeline_layout,
        true,
        wgpu::BlendState::ALPHA_BLENDING,
        false,
    );

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

    // 级联阴影 pass 的精简组：binding 0 = 64B 级联矩阵 uniform（VERTEX）。
    // 不引用阴影贴图本身——完整全局组引用了阴影贴图，同一 render pass
    // 内读写会触发资源互斥校验错误（教训见 ARCHITECTURE §4.7）。
    let cascade_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.layout.cascade"),
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

    // 阴影管线：从本级联光视角把深度写入 R32Float 颜色附件（详见 shadow.wgsl）。
    // 独立 pipeline layout：只含级联矩阵组，无需材质与阴影采样绑定。
    let shadow_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("iris.shadow.pipeline.layout"),
        bind_group_layouts: &[Some(&cascade_layout)],
        immediate_size: 0,
    });
    let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("iris.pipeline.shadow"),
        layout: Some(&shadow_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shadow_shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[
                Some(crate::mesh::Vertex::LAYOUT),
                Some(crate::instance::InstanceRaw::LAYOUT),
            ],
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
        bind_group_layouts: &[Some(&cascade_layout), Some(&joints_layout)],
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
    let point_shadow_pipeline_layout =
        device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
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
            buffers: &[
                Some(crate::mesh::Vertex::LAYOUT),
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

    // 后处理全屏 pass 族：泛光（半分辨率亮度提取 + 可分离高斯模糊）与
    // 暗角（全分辨率逐像素压暗）。共用同一「参数 uniform + 源纹理 +
    // 采样器」组布局（post_layout）与 HDR 颜色目标，只有着色器模块与
    // 片段入口不同——pass 串联框架（post.rs）据此用单一编码函数服务
    // 全部中间 pass。
    let bloom_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.bloom"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/bloom.wgsl").into()),
    });
    let post_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.layout.post"),
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
    let post_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("iris.post.pipeline.layout"),
        bind_group_layouts: &[Some(&post_layout)],
        immediate_size: 0,
    });
    let post_target = &[Some(wgpu::ColorTargetState {
        format: HDR_FORMAT,
        blend: None,
        write_mask: wgpu::ColorWrites::ALL,
    })];
    let make_post_pipeline = |label: &'static str,
                              shader: &wgpu::ShaderModule,
                              fs: &'static str|
     -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&post_pipeline_layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some(fs),
                compilation_options: Default::default(),
                targets: post_target,
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
    let bloom_threshold_pipeline = make_post_pipeline(
        "iris.pipeline.bloom_threshold",
        &bloom_shader,
        "fs_threshold",
    );
    let bloom_blur_pipeline =
        make_post_pipeline("iris.pipeline.bloom_blur", &bloom_shader, "fs_blur");
    // 暗角：全分辨率 HDR → HDR 的逐像素四角压暗（pass 串联框架的示范
    // pass——与泛光共用组布局与目标格式，仅换着色器模块）
    let vignette_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.vignette"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/vignette.wgsl").into()),
    });
    let vignette_pipeline =
        make_post_pipeline("iris.pipeline.vignette", &vignette_shader, "fs_main");

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

    // 粒子 billboard：4 顶点 triangle-strip 由 vertex_index 展开（无顶点
    // 缓冲），实例缓冲 = (中心 xyz, 尺寸 w) + 线性 RGBA（32 字节/粒）。
    // 两条管线仅混合不同——
    // - additive：color = src·SrcAlpha + dst·One（加法交换律 → 实例
    //   顺序无关、不排序），alpha 通道同取叠加（结果 alpha 无人消费）；
    // - alpha：标准 ALPHA_BLENDING（src α, 1 − src α），实例按视深降序。
    // 都深度**测试开（Less）/ 写入关**——粒子不遮挡任何后续几何，也
    // 不得因互相重叠而被裁（与透明管线参数一致）；cull None（billboard
    // 恒面向相机，strip 绕向随四角展开，剔除无意义）；组 1 绑定相机
    // right / up 小 uniform（每帧 write_buffer）。
    let particle_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.particle"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/particle.wgsl").into()),
    });
    let particle_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.layout.particle"),
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
    let particle_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("iris.particle.pipeline.layout"),
        bind_group_layouts: &[Some(&globals_layout), Some(&particle_layout)],
        immediate_size: 0,
    });
    let make_particle_pipeline =
        |label: &'static str, blend: wgpu::BlendState| -> wgpu::RenderPipeline {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&particle_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &particle_shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[Some(crate::particles::ParticleInstance::LAYOUT)],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &particle_shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    // 写线性 HDR 中间缓冲（与主 / 透明 pass 同目标，泛光与
                    // 色调映射照常作用于粒子）
                    targets: &[Some(wgpu::ColorTargetState {
                        format: HDR_FORMAT,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    cull_mode: None,
                    front_face: wgpu::FrontFace::Ccw,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth32Float,
                    depth_write_enabled: Some(false),
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
            })
        };
    let particle_additive_pipeline = make_particle_pipeline(
        "iris.pipeline.particle_additive",
        wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        },
    );
    let particle_alpha_pipeline = make_particle_pipeline(
        "iris.pipeline.particle_alpha",
        wgpu::BlendState::ALPHA_BLENDING,
    );

    PipelineBundle {
        pipeline,
        skinned_pipeline,
        transparent_pipeline,
        transparent_skinned_pipeline,
        sky_pipeline,
        shadow_pipeline,
        skinned_shadow_pipeline,
        point_shadow_pipeline,
        skinned_point_shadow_pipeline,
        blit_pipeline,
        bloom_threshold_pipeline,
        bloom_blur_pipeline,
        vignette_pipeline,
        ui_pipeline,
        particle_additive_pipeline,
        particle_alpha_pipeline,
        globals_layout,
        cascade_layout,
        point_shadow_layout,
        material_layout,
        joints_layout,
        blit_layout,
        post_layout,
        ui_layout,
        particle_layout,
    }
}
