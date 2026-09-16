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

/// 点光 cubemap 阴影贴图分辨率（每面边长；每盏 6 面 × 512² R32Float）。
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
    /// 双面变体（R66d，glTF `doubleSided`）：与 [`Self::pipeline`] 唯一
    /// 差异 = `cull_mode: None`（背面不剔除）；绘制期按材质
    /// [`crate::material::MaterialDef::double_sided`] 选族。
    pub double_sided_pipeline: wgpu::RenderPipeline,
    /// 双面蒙皮变体（组 2 绑定关节矩阵）。
    pub double_sided_skinned_pipeline: wgpu::RenderPipeline,
    /// 透明双面变体（alpha 混合 + 深度只测不写 + 不剔除）。
    pub transparent_double_sided_pipeline: wgpu::RenderPipeline,
    /// 透明双面蒙皮变体。
    pub transparent_double_sided_skinned_pipeline: wgpu::RenderPipeline,
    pub sky_pipeline: wgpu::RenderPipeline,
    /// 反射探针捕获的天空盒变体（R62a）：与 [`Self::sky_pipeline`] 同
    /// 着色器 / 同布局，仅采样数为 1（单采样立方体面目标）。
    pub probe_sky_pipeline: wgpu::RenderPipeline,
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
    /// 景深 pass 管线（R43，全分辨率 HDR → HDR + 深度副本采样，
    /// `post_depth_layout`：组 0 多一路 binding 3 的非过滤纹理）。
    pub dof_pipeline: wgpu::RenderPipeline,
    /// 运动模糊 pass 管线（R43，全分辨率 HDR → HDR + 深度副本采样，
    /// 参数 uniform 含当前 / 上一帧视图投影矩阵）。
    pub motion_blur_pipeline: wgpu::RenderPipeline,
    /// 主相机场景深度副本管线（R43）：R32Float 颜色附件存线性视距 +
    /// Depth32Float 双附件（阴影既有方案，见 scene_depth.wgsl）。
    pub scene_depth_pipeline: wgpu::RenderPipeline,
    /// 场景深度副本蒙皮变体（组 1 绑定关节矩阵，深度随骨骼变形）。
    pub scene_depth_skinned_pipeline: wgpu::RenderPipeline,
    /// GTAO 可见度图管线（R77）：全屏三角形 → R16Float 半分辨率目标，
    /// `ao_layout`（组 0 = 参数 uniform + 上帧深度副本，textureLoad 读取），
    /// 见 `shaders/ao.wgsl` 与 post.rs 的 AoSettings。
    pub ao_pipeline: wgpu::RenderPipeline,
    /// 体积雾管线（R79）：后处理链头的全屏 pass——链头 HDR + 上帧深度
    /// 副本光线步进，组 0 复用 `post_depth_layout`（与 DOF / MB 同构），
    /// 见 `shaders/fog.wgsl` 与 fog.rs 的 FogSettings。
    pub fog_pipeline: wgpu::RenderPipeline,
    /// 平面水体管线（R81）：主 pass 后的水面合成网格 draw——Gerstner
    /// 顶点位移 + 反射 / 折射捕获采样 + Fresnel + 深度吸收 + 太阳镜面，
    /// `water_layout`（组 0 独立五槽位，不扩 globals 组），目标 = HDR
    /// MSAA alpha 混合、深度只测不写、cull None，见 `shaders/water.wgsl`
    /// 与 water.rs 的 WaterSurface。
    pub water_pipeline: wgpu::RenderPipeline,
    /// 2D UI 叠加层管线（像素坐标顶点 + 字形图集采样，alpha 混合）。
    pub ui_pipeline: wgpu::RenderPipeline,
    /// 渲染目标（RT，R40）单采样几何管线：与主管线同源（PBR / 蒙皮 /
    /// 透明变体），仅 `multisample.count = 1`、片段目标仍为线性 HDR——
    /// RT 场景先渲到离屏线性 HDR 中间缓冲，再经 [`Self::rt_blit_pipeline`]
    /// ACES 色调映射到 RT 颜色纹理。**不画天空盒**（v1 限制，见
    /// `rendertarget.rs` 与 ARCHITECTURE §4.21）。
    pub rt_pipeline: wgpu::RenderPipeline,
    /// RT 蒙皮变体（组 2 绑定关节矩阵）。
    pub rt_skinned_pipeline: wgpu::RenderPipeline,
    /// RT 透明变体（alpha 混合 + 深度只测不写）。
    pub rt_transparent_pipeline: wgpu::RenderPipeline,
    /// RT 透明蒙皮变体。
    pub rt_transparent_skinned_pipeline: wgpu::RenderPipeline,
    /// RT 双面变体（R66d）：RT 内双面材质与主 pass 同语义。
    pub rt_double_sided_pipeline: wgpu::RenderPipeline,
    /// RT 双面蒙皮变体。
    pub rt_double_sided_skinned_pipeline: wgpu::RenderPipeline,
    /// RT 透明双面变体。
    pub rt_transparent_double_sided_pipeline: wgpu::RenderPipeline,
    /// RT 透明双面蒙皮变体。
    pub rt_transparent_double_sided_skinned_pipeline: wgpu::RenderPipeline,
    /// 反射探针捕获的不透明几何管线（R62a）：单采样 + `front_face: Cw`
    /// （捕获面矩阵的 NDC y 翻转会反转环绕方向，正面约定须同步翻转，
    /// 否则剔除的是房间内壁而非外壳——见 `create_pipeline` 内的实测结论）。
    pub probe_capture_pipeline: wgpu::RenderPipeline,
    /// 反射探针捕获的蒙皮变体（组 2 绑定关节矩阵）。
    pub probe_capture_skinned_pipeline: wgpu::RenderPipeline,
    /// 反射探针捕获的双面变体（R66d）。
    pub probe_capture_double_sided_pipeline: wgpu::RenderPipeline,
    /// 反射探针捕获的双面蒙皮变体。
    pub probe_capture_double_sided_skinned_pipeline: wgpu::RenderPipeline,
    /// RT 色调映射管线（Rgba8UnormSrgb 输出）：复用 blit 着色器做 ACES +
    /// 曝光、泛光强度恒 0（RT 无 bloom），把 RT 线性 HDR 中间缓冲 map 到
    /// RT 颜色纹理。
    pub rt_blit_pipeline: wgpu::RenderPipeline,
    /// 粒子 additive 管线（billboard 实例化，`SrcAlpha + One` 加法混合，
    /// 深度只测不写——火焰 / 火花类高亮粒子，叠加顺序无关）。
    pub particle_additive_pipeline: wgpu::RenderPipeline,
    /// 粒子 alpha 管线（标准 alpha 混合 + 深度只测不写，与透明管线参数
    /// 一致；实例按视深降序绘制）。
    pub particle_alpha_pipeline: wgpu::RenderPipeline,
    /// 粒子 v2 compute 模拟管线（R82，**全仓首个 compute 管线**）：
    /// 固定池 4096 粒 × 恒定 64 工作组 × 64 线程，按索引有序读写
    /// storage——会话内逐位确定性的根基（见 particles_v2.rs）。
    pub particle_v2_sim_pipeline: wgpu::ComputePipeline,
    /// 粒子 v2 拖尾 additive 管线（R82）：ribbon + 头部 billboard，
    /// 顶点全由 vertex_index 派生（storage 读数据，无顶点缓冲）。
    pub particle_v2_additive_pipeline: wgpu::RenderPipeline,
    /// 粒子 v2 拖尾 alpha 管线（R82）：参数同 additive、标准 alpha 混合。
    pub particle_v2_alpha_pipeline: wgpu::RenderPipeline,
    /// 贴花管线（R39）：盒投影代理几何（单位盒 ±Z 端盖）× 逐实例世界盒
    /// 矩阵 + 逆矩阵 + tint（144B）。alpha 混合 + 深度 LessEqual 只测不写
    /// （端盖与被贴表面共面，等深必须通过；不污染后续 pass 读取的深度），
    /// 位于蒙皮之后、透明之前（绘制序理由见 decal.rs 模块文档）。
    pub decal_pipeline: wgpu::RenderPipeline,
    /// FXAA 管线（R53）：全屏三角形 LDR sRGB 域边缘平滑，目标 =
    /// 最终颜色格式（blit 之后、UI 之前，见 `shaders/fxaa.wgsl`）。
    pub fxaa_pipeline: wgpu::RenderPipeline,
    /// TAA 管线（R60）：链首 HDR 域时域累加，`taa_layout`（组 0 五
    /// binding：参数 + 抖动色 + 采样器 + 深度副本 + 历史），目标 = 本帧
    /// 历史槽（见 `shaders/taa.wgsl` 与 post.rs `encode_taa_pass`）。
    pub taa_pipeline: wgpu::RenderPipeline,
    /// 遮挡查询代理管线（R53）：实例世界 AABB 盒、无片段阶段（深度只测
    /// 不写，查询计数由光栅化 + 深度测试产生），组 0 = 视图投影矩阵
    /// （`cascade_layout` 同源 64B uniform）。
    pub occlusion_pipeline: wgpu::RenderPipeline,
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
    /// 带附加纹理的后处理组布局（R43，DOF / MB 专用）：`post_layout` 的
    /// 基础上多一路 binding 3 = 深度副本纹理（R32Float 非过滤——该格式
    /// 不可过滤，声明 filterable = false 不依赖 float32-filterable 扩展）。
    pub post_depth_layout: wgpu::BindGroupLayout,
    /// GTAO 组布局（R77）：binding 0 = 参数 uniform（32B AoParamsRaw）、
    /// 1 = 上帧场景深度副本（R32Float 非过滤，textureLoad 读取）。
    pub ao_layout: wgpu::BindGroupLayout,
    /// 天空盒组布局（R79）：binding 0 = Globals uniform（640B，与
    /// `globals_layout` 的 binding 0 同缓冲）、4 = 预滤波环境立方体贴图、
    /// 6 = 过滤采样器（三者与 globals 组同资源）、**7 = SkyParams uniform
    /// （64B，R79 大气散射）**。sky/probe_sky 两条管线由此专用布局驱动
    /// （globals 组的 binding 7 已是点光 cubemap 阴影贴图，不能在原布局
    /// 上扩号——专用布局让 skybox.wgsl 的 binding 编号 0/4/6/7 与声明
    /// 逐一对齐，Globals 并行前缀结构零触碰）。绑定组由
    /// `build_sky_bind_group` 在全部天空绘制点创建。
    pub sky_layout: wgpu::BindGroupLayout,
    /// 平面水体组布局（R81）：binding 0 = WaterParams uniform（240B，
    /// vertex|fragment）、1 = 反射捕获图、2 = 折射捕获图（Rgba16Float
    /// 可过滤）、3 = scene_depth（R32Float 非过滤）、4 = 过滤采样器。
    /// 独立组不扩 globals（R61/R77/R78 先例）。
    pub water_layout: wgpu::BindGroupLayout,
    /// TAA 组布局（R60）：`post_depth_layout` 的基础上再多一路
    /// binding 4 = 上帧历史纹理（Rgba16Float，可过滤——历史重投影采样
    /// 在亚纹素位置，线性过滤是其定义的一部分）。
    pub taa_layout: wgpu::BindGroupLayout,
    /// UI 绑定组布局：binding 0 = 字形图集纹理、1 = 采样器、
    /// 2 = 屏幕尺寸 uniform（顶点阶段）。
    pub ui_layout: wgpu::BindGroupLayout,
    /// 粒子相机布局：binding 0 = billboard 基向量小 uniform
    ///（相机 right / up，视图矩阵第 0 / 1 行，每帧 write_buffer）。
    pub particle_layout: wgpu::BindGroupLayout,
    /// 粒子 v2 模拟布局（R82，compute）：binding 0 = SimParams uniform
    ///（96B）、1 = 粒子状态 read-write storage（固定池 4096 × 560B）。
    pub particle_v2_sim_layout: wgpu::BindGroupLayout,
    /// 粒子 v2 拖尾布局（R82，渲染）：binding 0 = DrawParams uniform
    ///（64B，vertex 阶段）、1 = 粒子状态只读 storage（vertex 阶段）。
    pub particle_v2_draw_layout: wgpu::BindGroupLayout,
    /// meshlet vertex-pull 绘制管线（R85）：顶点 = `meshlet_draw.wgsl`
    /// vs_main（紧凑簇列表 → storage 拉顶点 / 索引），**片元 =
    /// standard.wgsl 的 fs_main 本体**（wgpu 跨模块管线阶段——PBR 求值
    /// 零复制共享，与常规路径像素等价的根据）。光栅状态与不透明主管线
    /// 一致（Cull Back、REPLACE、深度写、Less）。
    pub meshlet_pipeline: wgpu::RenderPipeline,
    /// meshlet 逐簇剔除 compute 管线（R85）：包围球视锥 + 背面锥 →
    /// flags / payload（一线程一（实例, 簇）对，无簇间交互）。
    pub meshlet_cull_pipeline: wgpu::ComputePipeline,
    /// meshlet 稳定紧凑化 compute 管线（R85）：单工作组 256×64 两遍扫描
    /// ——u32 前缀和 + 按前缀槽位写紧凑列表与存活总数（无原子，紧凑序 =
    /// 全局簇号升序，绘制序确定）。
    pub meshlet_compact_pipeline: wgpu::ComputePipeline,
    /// meshlet draw 组 2 布局（R85，vertex 阶段）：binding 0..4 = 只读
    /// storage（簇词 / 顶点词 / 索引 / 实例表 / 紧凑列表）。
    pub meshlet_draw_layout: wgpu::BindGroupLayout,
    /// meshlet compute 布局（R85）：binding 0 = CullParams uniform（96B）、
    /// 1 = 簇词只读、2 = 实例表只读、3..6 = flags / payload / compact /
    /// survive_count read-write storage。main_cull / main_compact 两条
    /// 管线共用（布局声明是着色器用量的超集，合法）。
    pub meshlet_cull_layout: wgpu::BindGroupLayout,
    /// 贴花纹理布局：binding 0 = 贴花纹理、1 = 采样器（组 1 按纹理分批
    /// 绑定；tint 在实例数据里逐实例携带，不占绑定组）。
    pub decal_layout: wgpu::BindGroupLayout,
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
            // 点光 cubemap 阴影贴图**数组**（R61：R32Float 存归一化线性
            // 距离；层序 = 槽位 × 6 面，槽位由确定性选择键分配）。
            // 采样复用 binding 2 的 NonFiltering 采样器：R32Float 双线性过滤
            // 需要 float32-filterable 扩展，不依赖；与方向光阴影一致。
            wgpu::BindGroupLayoutEntry {
                binding: 7,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::CubeArray,
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
            // 反射探针参数（R62a，`ProbeParams`）：生效数 + 逐探针
            // （中心 / 过渡带 / 半边长 / cubemap 槽位）。与 CSM 同样**不扩
            // Globals**——并行布局陷阱（skybox.wgsl 自持 Globals 前缀声明）
            // 因此完全不被触碰，且无探针时着色路径与 R61 逐位一致。
            wgpu::BindGroupLayoutEntry {
                binding: 9,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            // 反射探针 cubemap **数组**（R62a）：Rgba16Float、层序 =
            // 槽位 × 6 + 面序、5 级 mip（与 env 同档）。采样复用 binding 6
            // 的 Filtering 采样器（Rgba16Float 可过滤，无需扩展）。
            wgpu::BindGroupLayoutEntry {
                binding: 10,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::CubeArray,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            // SSR 上帧资源（R62b，R61/R62a 的「不扩 Globals 前缀」扩容先例：
            // 排在现有最大 binding 之后追加，skybox.wgsl 的并行布局不受
            // 影响）。11 = 上帧主解析缓冲帧尾副本（Rgba16Float 可过滤，
            // 命中后一次采样取反射色）；12 = 上帧场景深度副本（R32Float
            // 非过滤，步进中 textureLoad 逐样本读——不依赖
            // float32-filterable 扩展）；13 = SSR 参数 uniform（96B）。
            wgpu::BindGroupLayoutEntry {
                binding: 11,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 12,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 13,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            // GTAO 可见度图（R77，「不扩 Globals 前缀」扩容先例的延续）：
            // R16Float 半分辨率（可过滤——standard 线性采样按屏幕坐标取
            // 遮蔽），缺席/关闭 = 1×1 哑纹理（纹素恰为 1.0，× 1.0 逐位
            // 精确）。排在现有最大 binding 13 之后，skybox.wgsl 的并行
            // 布局不受影响。
            wgpu::BindGroupLayoutEntry {
                binding: 14,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            // 前向光源分箱（R78，「不扩 Globals 前缀」扩容先例的延续）：
            // 15 = 参数 uniform（32B，counts.x = mode，0 = 分箱分支死码）；
            // 16 = 灯表 read-only storage（512 × 80B 定长）；
            // 17 = tile 索引 read-only storage（ranges 区 + indices 区）。
            // 缺席/关闭/捕获/RT = 全零哑缓冲（mode = 0），skybox.wgsl 的
            // 并行布局不受影响。
            wgpu::BindGroupLayoutEntry {
                binding: 15,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 16,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 17,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            // GI 探针三阶 SH（R83，同一扩容先例的延续）：
            // 18 = 探针表 read-only storage（256 × 160B 定长）；
            // 19 = 16B 参数 uniform（params.x = mode，0 = 逐片元分支死码）。
            // 缺席 / 无 SH 探针集 / 捕获 / RT = 全零哑缓冲（mode = 0），
            // skybox.wgsl 的并行布局不受影响。片元阶段只读 storage 缓冲
            // 计数 = 3（16/17/18），在 WebGPU 默认的
            // maxStorageBuffersPerShaderStage = 4 之内。
            wgpu::BindGroupLayoutEntry {
                binding: 18,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 19,
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
            // 法线 / ORM 数据贴图（R42，均线性格式；材质缺省时渲染器绑
            // 中性常驻纹理——零分支 shader，见 material.rs 模块文档）。
            // 采样共用 binding 2 的 Filtering 采样器。
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            // 光照贴图图集（R52c）：Rgba16Float 线性 HDR 辐照度，UV2 顶点
            // 属性直接是图集归一化坐标 ⇒ 着色器零常量变换。无光照贴图的
            // 材质由渲染器绑常驻 1×1 黑纹理 + `mask = 0`（间接项走
            // `select` 取原 IBL 分支，逐位不变，见 lightmap.rs 模块文档）。
            // 绑定组按 (材质, 图集句柄) 缓存 ⇒ 同图集的实体共组。
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
            // 预积分皮肤 SSS LUT（R63）：Rgba16Float 64×64 常驻纹理
            // （u = NdotL、v = 曲率包裹参数，程序化生成见 `sss.rs`）。
            // 追加在现有最大 binding 之后（R61/R62a/R62b 扩容先例），
            // 采样复用 binding 6 的 ClampToEdge 双线性采样器——材质组
            // 不新增采样器槽位。绑定组布局变更不影响任何着色器输出：
            // 全部既有材质的 `material.sss.x = 0`（uniform 缺省）⇒
            // 片元 SSS 分支整段不进入。
            wgpu::BindGroupLayoutEntry {
                binding: 7,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
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

    // 主 pass 几何管线族构造：不透明 / 透明 × 普通 / 蒙皮 × 单面 / 双面
    // 共八条，顶点布局、拓扑、深度格式与绑定组全部一致，只有混合状态、
    // 深度写入与背面剔除按变体区分——
    // - 不透明：REPLACE + 深度写入（Less）；
    // - 透明：`ALPHA_BLENDING`（src α, 1 − src α，与 UI 管线一致）+
    //   深度只测不写（Less 测试保留、写入关闭）——写入会让后画的远透明面
    //   被先画的近透明面错误裁掉（共面透明对必然丢失其一），只测不写则
    //   透明仍被不透明几何正确遮挡；
    // - 双面（R66d，glTF `doubleSided`）：`cull_mode: None`——发片 / 薄片
    //   类资产绕向朝内的面在单面管线下会被整片剔掉（iris-exp frictions
    //   ⑦）；单面变体维持 `Cull Back`，既有材质逐位不变。
    let make_mesh_pipeline = |label: &'static str,
                              entry_point: &'static str,
                              layout: &wgpu::PipelineLayout,
                              skinned: bool,
                              blend: wgpu::BlendState,
                              depth_write: bool,
                              msaa: u32,
                              double_sided: bool|
     -> wgpu::RenderPipeline {
        // 顶点缓冲（R42 起含切线路、R52c 起含光照贴图 UV2 路）：
        // 普通 = [顶点, 实例, 切线, UV2]，蒙皮 = [顶点, 蒙皮属性, 实例,
        // 切线, UV2]（切线 loc12 / UV2 loc13）。两路都按网格携带，缺席由
        // 渲染器垫零缓冲（TANGENT_LAYOUT / UV2_LAYOUT 注释）。
        let buffers: &[Option<wgpu::VertexBufferLayout<'static>>] = if skinned {
            &[
                Some(crate::mesh::Vertex::LAYOUT),
                Some(crate::mesh::SkinVertex::LAYOUT),
                Some(crate::instance::InstanceRaw::LAYOUT),
                Some(crate::mesh::TANGENT_LAYOUT),
                Some(crate::mesh::UV2_LAYOUT),
            ]
        } else {
            &[
                Some(crate::mesh::Vertex::LAYOUT),
                Some(crate::instance::InstanceRaw::LAYOUT),
                Some(crate::mesh::TANGENT_LAYOUT),
                Some(crate::mesh::UV2_LAYOUT),
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
                // 双面材质（R66d）关背面剔除；单面维持既有 Cull Back
                cull_mode: if double_sided {
                    None
                } else {
                    Some(wgpu::Face::Back)
                },
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
                count: msaa,
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
        sample_count,
        false,
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
        sample_count,
        false,
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
        sample_count,
        false,
    );
    let transparent_skinned_pipeline = make_mesh_pipeline(
        "iris.pipeline.transparent_skinned",
        "vs_skinned",
        &skinned_pipeline_layout,
        true,
        wgpu::BlendState::ALPHA_BLENDING,
        false,
        sample_count,
        false,
    );
    // 双面变体（R66d，glTF doubleSided）：与单面族唯一差异 = cull None。
    // 不透明双面 / 透明双面 × 普通 / 蒙皮共四条，绘制期按材质标志选族。
    let double_sided_pipeline = make_mesh_pipeline(
        "iris.pipeline.double_sided",
        "vs_main",
        &pipeline_layout,
        false,
        wgpu::BlendState::REPLACE,
        true,
        sample_count,
        true,
    );
    let double_sided_skinned_pipeline = make_mesh_pipeline(
        "iris.pipeline.double_sided_skinned",
        "vs_skinned",
        &skinned_pipeline_layout,
        true,
        wgpu::BlendState::REPLACE,
        true,
        sample_count,
        true,
    );
    let transparent_double_sided_pipeline = make_mesh_pipeline(
        "iris.pipeline.transparent_double_sided",
        "vs_main",
        &pipeline_layout,
        false,
        wgpu::BlendState::ALPHA_BLENDING,
        false,
        sample_count,
        true,
    );
    let transparent_double_sided_skinned_pipeline = make_mesh_pipeline(
        "iris.pipeline.transparent_double_sided_skinned",
        "vs_skinned",
        &skinned_pipeline_layout,
        true,
        wgpu::BlendState::ALPHA_BLENDING,
        false,
        sample_count,
        true,
    );
    // RT（R40）几何管线族：单采样变体（RT 无 MSAA——像素化小地图天然契合，
    // 见 rendertarget.rs），复用同源着色器 / 布局，仅 multisample.count = 1。
    let rt_pipeline = make_mesh_pipeline(
        "iris.pipeline.rt",
        "vs_main",
        &pipeline_layout,
        false,
        wgpu::BlendState::REPLACE,
        true,
        1,
        false,
    );
    let rt_skinned_pipeline = make_mesh_pipeline(
        "iris.pipeline.rt_skinned",
        "vs_skinned",
        &skinned_pipeline_layout,
        true,
        wgpu::BlendState::REPLACE,
        true,
        1,
        false,
    );
    let rt_transparent_pipeline = make_mesh_pipeline(
        "iris.pipeline.rt_transparent",
        "vs_main",
        &pipeline_layout,
        false,
        wgpu::BlendState::ALPHA_BLENDING,
        false,
        1,
        false,
    );
    let rt_transparent_skinned_pipeline = make_mesh_pipeline(
        "iris.pipeline.rt_transparent_skinned",
        "vs_skinned",
        &skinned_pipeline_layout,
        true,
        wgpu::BlendState::ALPHA_BLENDING,
        false,
        1,
        false,
    );
    // RT 双面变体（R66d）：小地图 / 监视器 RT 内的双面材质与主 pass 同语义。
    let rt_double_sided_pipeline = make_mesh_pipeline(
        "iris.pipeline.rt_double_sided",
        "vs_main",
        &pipeline_layout,
        false,
        wgpu::BlendState::REPLACE,
        true,
        1,
        true,
    );
    let rt_double_sided_skinned_pipeline = make_mesh_pipeline(
        "iris.pipeline.rt_double_sided_skinned",
        "vs_skinned",
        &skinned_pipeline_layout,
        true,
        wgpu::BlendState::REPLACE,
        true,
        1,
        true,
    );
    let rt_transparent_double_sided_pipeline = make_mesh_pipeline(
        "iris.pipeline.rt_transparent_double_sided",
        "vs_main",
        &pipeline_layout,
        false,
        wgpu::BlendState::ALPHA_BLENDING,
        false,
        1,
        true,
    );
    let rt_transparent_double_sided_skinned_pipeline = make_mesh_pipeline(
        "iris.pipeline.rt_transparent_double_sided_skinned",
        "vs_skinned",
        &skinned_pipeline_layout,
        true,
        wgpu::BlendState::ALPHA_BLENDING,
        false,
        1,
        true,
    );

    // 反射探针捕获管线族（R62a）：单采样 + **front_face: Cw**。捕获的面矩阵
    // 含 NDC y 翻转（Cube 采样约定要求，见 `light::cube_face_view_proj`），
    // 而翻转会**反转三角形在帧缓冲空间的环绕方向**（y 翻转 = 屏幕空间定向
    // 反转）⇒ 沿用 `Ccw` 会把正面判成背面、剔掉房间里朝内的墙面而渲染墙的
    // 外侧（实测症状：探针被球壳包住时整张 cubemap 变成球内壁的暗灰，墙上
    // 图案与自发光板全部消失）。把正面约定同步翻成 `Cw` 即恢复与主 pass
    // 一致的剔除语义（房间内壁是正面、包住探针的壳体是背面），比阴影 pass
    // 的「cull None 交给深度」更省片元。
    let make_capture_pipeline = |label: &'static str,
                                 entry_point: &'static str,
                                 layout: &wgpu::PipelineLayout,
                                 skinned: bool,
                                 double_sided: bool|
     -> wgpu::RenderPipeline {
        let buffers: &[Option<wgpu::VertexBufferLayout<'static>>] = if skinned {
            &[
                Some(crate::mesh::Vertex::LAYOUT),
                Some(crate::mesh::SkinVertex::LAYOUT),
                Some(crate::instance::InstanceRaw::LAYOUT),
                Some(crate::mesh::TANGENT_LAYOUT),
                Some(crate::mesh::UV2_LAYOUT),
            ]
        } else {
            &[
                Some(crate::mesh::Vertex::LAYOUT),
                Some(crate::instance::InstanceRaw::LAYOUT),
                Some(crate::mesh::TANGENT_LAYOUT),
                Some(crate::mesh::UV2_LAYOUT),
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
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR_FORMAT,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // 双面材质（R66d）关剔除；单面维持 Cw + Cull Back 的捕获语义
                cull_mode: if double_sided {
                    None
                } else {
                    Some(wgpu::Face::Back)
                },
                front_face: wgpu::FrontFace::Cw,
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
        })
    };
    let probe_capture_pipeline = make_capture_pipeline(
        "iris.pipeline.probe_capture",
        "vs_main",
        &pipeline_layout,
        false,
        false,
    );
    let probe_capture_skinned_pipeline = make_capture_pipeline(
        "iris.pipeline.probe_capture_skinned",
        "vs_skinned",
        &skinned_pipeline_layout,
        true,
        false,
    );
    // 双面捕获变体（R66d）：探针 cubemap 内的双面材质与主 pass 同语义。
    let probe_capture_double_sided_pipeline = make_capture_pipeline(
        "iris.pipeline.probe_capture_double_sided",
        "vs_main",
        &pipeline_layout,
        false,
        true,
    );
    let probe_capture_double_sided_skinned_pipeline = make_capture_pipeline(
        "iris.pipeline.probe_capture_double_sided_skinned",
        "vs_skinned",
        &skinned_pipeline_layout,
        true,
        true,
    );

    // 天空盒：全屏三角形按像素射线采样预滤波环境贴图 0 级 mip（= 原始天空）。
    // 主 pass 内最先绘制；深度只测不写（Always + write false），几何体随后覆盖。
    // R79：管线布局换成专用 `sky_layout`（bindings 0/4/6 同资源 + 7 =
    // SkyParams uniform 大气参数），skybox.wgsl 的绑定声明与布局逐一对齐。
    let sky_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.skybox"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/skybox.wgsl").into()),
    });
    let sky_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.layout.sky"),
        entries: &[
            // Globals uniform（640B）——与 globals_layout 的 binding 0 同
            // 资源同布局（skybox.wgsl 的并行 Globals 前缀声明一字未动）
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
            // 预滤波环境立方体贴图（旧渐变路径的采样源，mode = 0）
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
                binding: 6,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            // SkyParams uniform（64B，R79 大气散射——缺席 = 全零哑参数，
            // mode = 0 死码分支）
            wgpu::BindGroupLayoutEntry {
                binding: 7,
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
    let sky_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("iris.sky.pipeline.layout"),
        bind_group_layouts: &[Some(&sky_layout)],
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

    // 反射探针捕获的天空盒变体（R62a）：同一 `skybox.wgsl`、同一布局，
    // 只把采样数改成 1（探针捕获目标是单采样 Rgba16Float 立方体面）。
    // 复用同一着色器保证探针里的天空与主视口**逐像素同源**（同一
    // `inv_view_proj` 射线重建 + 同一 env 0 级 mip 采样）。
    let probe_sky_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("iris.pipeline.probe_sky"),
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
        multisample: wgpu::MultisampleState::default(),
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

    // RT 色调映射管线（R40）：复用 blit 着色器（ACES + 曝光 + 泛光叠加），
    // 但输出固定为 RT 颜色格式 Rgba8UnormSrgb（与视图表面格式解耦——
    // RT 颜色纹理恒为 sRGB 8 位，不依赖 window 的 surface 格式）。
    // 单采样无 MSAA；泛光强度恒 0（rt_blit_bind_group 的 bloom 槽位绑定
    // 白色兜底纹理、强度参数 0，见 renderer.rs 的 build_rt_bind_group）。
    let rt_blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("iris.pipeline.rt_blit"),
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
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
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

    // 景深 / 运动模糊（R43）：与暗角同构的全屏 pass，但多采样一路主相机
    // 深度副本（scene_depth pass 的 R32Float 线性视距）——组 0 用
    // post_depth_layout（binding 3 = 非过滤纹理），其余与 post_layout
    // 逐项一致。深度读取走 textureLoad（整数纹素坐标），非过滤声明是
    // 对 R32Float 不可过滤的匹配（见 dof.wgsl / motion_blur.wgsl 模块头）。
    let scene_depth_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.scene_depth"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/scene_depth.wgsl").into()),
    });
    let dof_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.dof"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/dof.wgsl").into()),
    });
    let motion_blur_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.motion_blur"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/motion_blur.wgsl").into()),
    });
    let post_depth_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.layout.post_depth"),
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
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                },
                count: None,
            },
        ],
    });
    let post_depth_pipeline_layout =
        device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("iris.post_depth.pipeline.layout"),
            bind_group_layouts: &[Some(&post_depth_layout)],
            immediate_size: 0,
        });
    let make_post_depth_pipeline = |label: &'static str,
                                    shader: &wgpu::ShaderModule,
                                    fs: &'static str|
     -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&post_depth_pipeline_layout),
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
    let dof_pipeline = make_post_depth_pipeline("iris.pipeline.dof", &dof_shader, "fs_main");
    let motion_blur_pipeline =
        make_post_depth_pipeline("iris.pipeline.motion_blur", &motion_blur_shader, "fs_main");

    // GTAO（R77）：主 pass 之前的全屏 pass——组 0 = 参数 uniform + 上帧
    // 场景深度副本（R32Float 非过滤，textureLoad 整数纹素读取，与 DOF/MB
    // 的深度消费同款）；目标 = R16Float 半分辨率可见度图（不在 post 家族
    // 的 HDR 目标里——AO 是照明输入不是后处理，格式与分辨率独立裁制）。
    let ao_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.ao"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/ao.wgsl").into()),
    });
    let ao_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.layout.ao"),
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
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                },
                count: None,
            },
        ],
    });
    let ao_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("iris.ao.pipeline.layout"),
        bind_group_layouts: &[Some(&ao_layout)],
        immediate_size: 0,
    });
    let ao_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("iris.pipeline.ao"),
        layout: Some(&ao_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &ao_shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &ao_shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::R16Float,
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

    // 体积雾（R79）：链头全屏 pass——组 0 与 DOF / MB 同构（post_depth_
    // layout：参数 uniform + 链头色 + 采样器 + 非过滤深度副本），目标 =
    // HDR 全分辨率槽（链上中间 pass 同构）。着色器做逐像素 16 步光线
    // 步进（算法与缺席语义见 fog.rs 模块文档与 ARCHITECTURE §4.36）。
    let fog_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.fog"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/fog.wgsl").into()),
    });
    let fog_pipeline = make_post_depth_pipeline("iris.pipeline.fog", &fog_shader, "fs_main");

    // 平面水体（R81）：主 pass 之后、TAA / 后处理链之前的水面合成 pass
    // （三角网格 draw，非全屏三角形）。组 0 = `water_layout`（独立组，
    // **不扩 globals 组**——R61/R77/R78 先例：0 = WaterParams uniform、
    // 1 = 反射捕获图、2 = 折射捕获图、3 = scene_depth（R32Float 非过滤
    // textureLoad）、4 = 过滤采样器）。目标 = HDR MSAA（alpha 混合，
    // 片元输出 a = 1 ⇒ 等价替换——折射图已含水下内容，输出即完整合成
    // 色）；深度只测不写（LessEqual 与贴花同款等深不丢失；水体不写主
    // 深度附件，与「水像素不写 scene_depth」同语义）；cull None（波形
    // 会倾斜面片，水下视角从背面看亦可见——相机入水无特殊处理是 v1
    // 限制，见 §8.50）。
    let water_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.water"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/water.wgsl").into()),
    });
    let water_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.layout.water"),
        entries: &[
            // 0：WaterParams uniform（240B，vertex | fragment——顶点做
            // Gerstner 位移与裁剪变换，片元做采样合成）
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
            // 1 / 2：反射 / 折射捕获图（Rgba16Float 半分辨率，可过滤——
            // 波纹扰动后的采样在亚纹素位置，双线性是其定义的一部分）
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
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
            // 3：scene_depth 副本（R32Float 非过滤，textureLoad 读取——
            // 与 AO / 雾的深度消费同款，不依赖 float32-filterable 扩展）
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                },
                count: None,
            },
            // 4：过滤采样器（ClampToEdge——扰动把 uv 推出捕获图边界时钳边）
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let water_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("iris.water.pipeline.layout"),
        bind_group_layouts: &[Some(&water_layout)],
        immediate_size: 0,
    });
    let water_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("iris.pipeline.water"),
        layout: Some(&water_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &water_shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(crate::water::WaterVertex::LAYOUT)],
        },
        fragment: Some(wgpu::FragmentState {
            module: &water_shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: HDR_FORMAT,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
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
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
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

    // TAA（R60）：组布局在 post_depth_layout 基础上再加 binding 4 = 上帧
    // 历史（Rgba16Float 可过滤）；目标 = 本帧历史槽（HDR 格式，与链上
    // 其他中间 pass 同构）。编码走 post.rs 的 encode_taa_pass。
    let taa_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.taa"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/taa.wgsl").into()),
    });
    let taa_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.layout.taa"),
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
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                },
                count: None,
            },
        ],
    });
    let taa_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("iris.taa.pipeline.layout"),
        bind_group_layouts: &[Some(&taa_layout)],
        immediate_size: 0,
    });
    let taa_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("iris.pipeline.taa"),
        layout: Some(&taa_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &taa_shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &taa_shader,
            entry_point: Some("fs_main"),
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
    });

    // 场景深度副本管线（R43）：与阴影管线同构（R32Float 颜色附件存深度
    // 通道 + Depth32Float 深度附件 Less 测试 + cull None，绕开 DX12
    // depth-only 写入异常的既有方案），区别是存**线性视距**（clip.w）
    // 而非裁剪 z——供 DOF 的 CoC 与 MB 的重投影直接消费（scene_depth.wgsl）。
    let scene_depth_pipeline_layout =
        device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("iris.scene_depth.pipeline.layout"),
            bind_group_layouts: &[Some(&cascade_layout)],
            immediate_size: 0,
        });
    let scene_depth_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("iris.pipeline.scene_depth"),
        layout: Some(&scene_depth_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &scene_depth_shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[
                Some(crate::mesh::Vertex::LAYOUT),
                Some(crate::instance::InstanceRaw::LAYOUT),
            ],
        },
        fragment: Some(wgpu::FragmentState {
            module: &scene_depth_shader,
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
            // 与阴影管线同款：cull None（环绕方向的实证结论见阴影管线）
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
    // 深度副本蒙皮变体：组 1 绑定关节矩阵（与蒙皮阴影管线同构）
    let scene_depth_skinned_layout =
        device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("iris.scene_depth.skinned.pipeline.layout"),
            bind_group_layouts: &[Some(&cascade_layout), Some(&joints_layout)],
            immediate_size: 0,
        });
    let scene_depth_skinned_pipeline =
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("iris.pipeline.scene_depth_skinned"),
            layout: Some(&scene_depth_skinned_layout),
            vertex: wgpu::VertexState {
                module: &scene_depth_shader,
                entry_point: Some("vs_skinned"),
                compilation_options: Default::default(),
                buffers: &[
                    Some(crate::mesh::Vertex::LAYOUT),
                    Some(crate::mesh::SkinVertex::LAYOUT),
                    Some(crate::instance::InstanceRaw::LAYOUT),
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &scene_depth_shader,
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

    // 粒子 v2（R82）——**全仓首个 compute 管线**。模拟管线：组 0 =
    // SimParams uniform（96B，CPU 每帧 write_buffer：发射数 / 游标 /
    // dt / 风 / 位姿）+ 粒子状态 read-write storage（固定池 4096 ×
    // 560B）；恒定 dispatch（池 / 工作组常量算出，不随存活数变）+
    // 每线程按索引有序读写 = 会话内逐位确定性的根基（跨适配器位级
    // 不可保，§8.51）。WGSL 无 u64 算术——出生哈希以 u32 肢对模拟
    // mix64（整数精确，跨适配器恒等）。
    let particle_v2_sim_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.particle_v2_sim"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/particles_v2_sim.wgsl").into()),
    });
    let particle_v2_sim_layout =
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("iris.layout.particle_v2_sim"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
    let particle_v2_sim_pipeline_layout =
        device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("iris.particle_v2.sim.pipeline.layout"),
            bind_group_layouts: &[Some(&particle_v2_sim_layout)],
            immediate_size: 0,
        });
    let particle_v2_sim_pipeline =
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("iris.pipeline.particle_v2_sim"),
            layout: Some(&particle_v2_sim_pipeline_layout),
            module: &particle_v2_sim_shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });

    // 粒子 v2 拖尾渲染：ribbon + 头部 billboard，顶点全由 vertex_index
    // 派生（storage 读数据——无顶点缓冲、无实例缓冲），固定顶点数 =
    // 池 × trail_length × 6。组 0 复用全局布局（精简 Globals 声明，
    // particle.wgsl 同款）；组 1 = DrawParams uniform + 只读 storage。
    // 深度只测不写（Less）、cull None（屏幕空间展宽绕向随相机变化）、
    // HDR 目标 alpha 混合——两条管线仅混合不同（与 v1 粒子管线同参数，
    // 渲染进同一粒子 pass 位置）。
    let particle_v2_draw_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.particle_v2_draw"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/particles_v2_draw.wgsl").into()),
    });
    let particle_v2_draw_layout =
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("iris.layout.particle_v2_draw"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
    let particle_v2_draw_pipeline_layout =
        device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("iris.particle_v2.draw.pipeline.layout"),
            bind_group_layouts: &[Some(&globals_layout), Some(&particle_v2_draw_layout)],
            immediate_size: 0,
        });
    let make_particle_v2_pipeline =
        |label: &'static str, blend: wgpu::BlendState| -> wgpu::RenderPipeline {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&particle_v2_draw_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &particle_v2_draw_shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &particle_v2_draw_shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: HDR_FORMAT,
                        blend: Some(blend),
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
    let particle_v2_additive_pipeline = make_particle_v2_pipeline(
        "iris.pipeline.particle_v2_additive",
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
    let particle_v2_alpha_pipeline = make_particle_v2_pipeline(
        "iris.pipeline.particle_v2_alpha",
        wgpu::BlendState::ALPHA_BLENDING,
    );

    // meshlet 剔除 compute（R85，虚拟几何体 V2）：一条布局服务两个 entry
    // point——main_cull（一线程一（实例, 簇）对：包围球视锥 + 背面锥，
    // 写 flags / payload）与 main_compact（单工作组 256×64 两遍扫描：前缀
    // 和稳定紧凑化，写 compact / 存活总数）。u32 前缀和结合精确 ⇒ 扫描树
    // 形状无关的会话内确定性（meshlet.rs 模块头）；dispatch 形状由 CPU
    // 每帧按常量算出（越界线程早退，R82 范式）。
    let meshlet_cull_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.meshlet_cull"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/meshlet_cull.wgsl").into()),
    });
    let meshlet_cull_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.layout.meshlet_cull"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            // flags / payload / compact / survive_count：read-write storage
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 5,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 6,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let meshlet_cull_pipeline_layout =
        device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("iris.meshlet.cull.pipeline.layout"),
            bind_group_layouts: &[Some(&meshlet_cull_layout)],
            immediate_size: 0,
        });
    let meshlet_cull_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("iris.pipeline.meshlet_cull"),
        layout: Some(&meshlet_cull_pipeline_layout),
        module: &meshlet_cull_shader,
        entry_point: Some("main_cull"),
        compilation_options: Default::default(),
        cache: None,
    });
    let meshlet_compact_pipeline =
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("iris.pipeline.meshlet_compact"),
            layout: Some(&meshlet_cull_pipeline_layout),
            module: &meshlet_cull_shader,
            entry_point: Some("main_compact"),
            compilation_options: Default::default(),
            cache: None,
        });

    // meshlet vertex-pull 绘制管线（R85）：顶点 = meshlet_draw.wgsl
    // vs_main（紧凑簇列表 → storage 拉顶点 / 索引，无顶点 / 索引缓冲），
    // **片元 = standard.wgsl 的 fs_main 本体**——wgpu 允许管线两阶段跨
    // 模块（接口按 location 匹配），PBR 求值零复制共享，与常规路径同一
    // 材质绑定组语义（像素等价断言的根据，ARCHITECTURE §4.41）。布局 =
    // [globals, material, meshlet 组 2（5 个只读 storage）]；光栅状态与
    // 不透明主管线一致（Cull Back + CCW、REPLACE、深度写、Less、MSAA 同
    // 采样数、HDR 目标）。组 2 布局同时被 draw 管线（vertex 只读）消费。
    let meshlet_draw_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.meshlet_draw"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/meshlet_draw.wgsl").into()),
    });
    let meshlet_draw_storage_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let meshlet_draw_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.layout.meshlet_draw"),
        entries: &[
            meshlet_draw_storage_entry(0),
            meshlet_draw_storage_entry(1),
            meshlet_draw_storage_entry(2),
            meshlet_draw_storage_entry(3),
            meshlet_draw_storage_entry(4),
        ],
    });
    let meshlet_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("iris.meshlet.pipeline.layout"),
        bind_group_layouts: &[
            Some(&globals_layout),
            Some(&material_layout),
            Some(&meshlet_draw_layout),
        ],
        immediate_size: 0,
    });
    let meshlet_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("iris.pipeline.meshlet"),
        layout: Some(&meshlet_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &meshlet_draw_shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
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

    // 贴花（R39）：盒投影代理几何（单位盒 ±Z 端盖，位置顶点）× 逐实例
    // 世界盒矩阵 + 逆矩阵 + tint（144B，DecalInstanceRaw::LAYOUT location
    // 1..=9）。组 0 复用全局布局（着色器只声明 view_proj，精简 Globals，
    // skybox.wgsl 同款做法）；组 1 = 贴花纹理 + 采样器（按纹理分批绑定）。
    // alpha 混合（src α, 1 − src α）；深度 **LessEqual** 只测不写——端盖
    // 与被贴表面共面（推荐摆放见 decal.rs），等深必须通过（Less 浮点抖动
    // 会闪烁），不写则透明 / 粒子照常按不透明深度裁决；cull Back + CCW
    // （端盖正面朝盒外，相机在哪侧只见哪侧端盖）。
    let decal_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.decal"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/decal.wgsl").into()),
    });
    let decal_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("iris.layout.decal"),
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
    let decal_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("iris.decal.pipeline.layout"),
        bind_group_layouts: &[Some(&globals_layout), Some(&decal_layout)],
        immediate_size: 0,
    });
    let decal_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("iris.pipeline.decal"),
        layout: Some(&decal_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &decal_shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[
                Some(crate::decal::PROXY_VERTEX_LAYOUT),
                Some(crate::decal::DecalInstanceRaw::LAYOUT),
            ],
        },
        fragment: Some(wgpu::FragmentState {
            module: &decal_shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            // 写线性 HDR 中间缓冲（与主 / 透明 pass 同目标，泛光与色调
            // 映射照常作用于贴花）
            targets: &[Some(wgpu::ColorTargetState {
                format: HDR_FORMAT,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
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
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
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

    // FXAA（R53）：与后处理链共用「参数 uniform + 源纹理 + 采样器」组
    // 布局（post_layout），唯独目标格式 = 最终颜色格式（LDR sRGB 域
    // pass，输入是 blit 写出的同格式中间纹理；sRGB 纹理由硬件解码为
    // 线性样本，着色器内以 pow 近似回显示域，见 fxaa.wgsl 模块头）。
    let fxaa_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.fxaa"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/fxaa.wgsl").into()),
    });
    let fxaa_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("iris.pipeline.fxaa"),
        layout: Some(&post_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &fxaa_shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &fxaa_shader,
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

    // 遮挡查询代理（R53）：世界 AABB 盒 → 裁剪位置（组 0 = 64B 视图投影
    // 矩阵，`cascade_layout` 与场景深度副本同源复用）。**无片段阶段**
    // （pass 零颜色附件，计数 = 通过深度测试的样本数）；深度只测不写
    // （不污染本帧深度——查询 pass 夹在主 pass 与贴花 pass 之间，后者
    // 仍要 Load 同一附件）；cull None（背面也计数 = 保守方向，见
    // occlusion.rs）；MSAA 采样数与主 pass 一致（附件就是主深度纹理）。
    let occlusion_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("iris.shader.occlusion"),
        source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/occlusion.wgsl").into()),
    });
    let occlusion_pipeline_layout =
        device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("iris.occlusion.pipeline.layout"),
            bind_group_layouts: &[Some(&cascade_layout)],
            immediate_size: 0,
        });
    let occlusion_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("iris.pipeline.occlusion"),
        layout: Some(&occlusion_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &occlusion_shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[
                Some(crate::occlusion::AABB_CORNER_LAYOUT),
                Some(crate::occlusion::AabbInstanceRaw::LAYOUT),
            ],
        },
        fragment: None,
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None,
            front_face: wgpu::FrontFace::Ccw,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(false),
            // LessEqual（贴花 R39 同款教训）：可见实例的 AABB 前面与其
            // 自身在主 pass 写入的深度共面等深，严格 Less 会「自遮挡」
            // 误剔自己；等深必须通过。查询计 0 的语义不变——全部采样
            // **严格更远**于已写入深度 ⇒ 该实例对本帧像素零贡献。
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
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

    PipelineBundle {
        pipeline,
        skinned_pipeline,
        transparent_pipeline,
        transparent_skinned_pipeline,
        double_sided_pipeline,
        double_sided_skinned_pipeline,
        transparent_double_sided_pipeline,
        transparent_double_sided_skinned_pipeline,
        sky_pipeline,
        probe_sky_pipeline,
        shadow_pipeline,
        skinned_shadow_pipeline,
        point_shadow_pipeline,
        skinned_point_shadow_pipeline,
        blit_pipeline,
        bloom_threshold_pipeline,
        bloom_blur_pipeline,
        vignette_pipeline,
        dof_pipeline,
        motion_blur_pipeline,
        scene_depth_pipeline,
        scene_depth_skinned_pipeline,
        ui_pipeline,
        rt_pipeline,
        rt_skinned_pipeline,
        rt_transparent_pipeline,
        rt_transparent_skinned_pipeline,
        rt_double_sided_pipeline,
        rt_double_sided_skinned_pipeline,
        rt_transparent_double_sided_pipeline,
        rt_transparent_double_sided_skinned_pipeline,
        probe_capture_pipeline,
        probe_capture_skinned_pipeline,
        probe_capture_double_sided_pipeline,
        probe_capture_double_sided_skinned_pipeline,
        rt_blit_pipeline,
        particle_additive_pipeline,
        particle_alpha_pipeline,
        particle_v2_sim_pipeline,
        particle_v2_additive_pipeline,
        particle_v2_alpha_pipeline,
        meshlet_pipeline,
        meshlet_cull_pipeline,
        meshlet_compact_pipeline,
        decal_pipeline,
        fxaa_pipeline,
        taa_pipeline,
        occlusion_pipeline,
        ao_pipeline,
        fog_pipeline,
        water_pipeline,
        globals_layout,
        cascade_layout,
        point_shadow_layout,
        material_layout,
        joints_layout,
        blit_layout,
        post_layout,
        post_depth_layout,
        taa_layout,
        ao_layout,
        sky_layout,
        water_layout,
        ui_layout,
        particle_layout,
        particle_v2_sim_layout,
        particle_v2_draw_layout,
        meshlet_draw_layout,
        meshlet_cull_layout,
        decal_layout,
    }
}

#[cfg(test)]
mod tests {
    /// **并行 Globals 布局的看门测试（R61 实测教训）**：`skybox.wgsl`
    /// 为了让天空盒只读 `inv_view_proj` 而声明了一份自己的 `Globals`
    /// 结构体——它是 standard.wgsl 布局的**前缀**，逐字段偏移必须一致，
    /// 而数组型成员的长度会平移后面所有字段。R61 给 `PointLight` 加了
    /// 逐灯阴影参数（2×vec4 → 3×vec4）时漏改这里，实测症状是**天空整体
    /// 错色**（skybox 读到的 `inv_view_proj` 落在别的字段上），三条「阴影
    /// 关闭」对照组 FNV 基线同时失守才定位到。此断言把这类「影子布局」
    /// 耦合钉在源码层：改 standard.wgsl 的 PointLight 布局必须同步这里。
    #[test]
    fn skybox_globals_prefix_matches_standard_layout() {
        let skybox = include_str!("../shaders/skybox.wgsl");
        assert!(
            skybox.contains("point_lights: array<vec4<f32>, 24>"),
            "skybox.wgsl 的并行 Globals 必须与 standard.wgsl 的 PointLight（8 × 3×vec4 = 24）同步"
        );
        let standard = include_str!("../shaders/standard.wgsl");
        assert!(
            standard.contains("point_lights: array<PointLight, 8>"),
            "standard.wgsl 的点光数组容量必须仍是 MAX_POINT_LIGHTS = 8"
        );
        // skybox 声明的字段顺序必须与 standard 的前缀一致（逐名核对）
        let order = [
            "view_proj",
            "eye_pos",
            "light_dir",
            "light_color",
            "ambient",
            "point_count",
            "point_lights",
        ];
        let body_of = |src: &str| -> String {
            src.split("struct Globals {")
                .nth(1)
                .and_then(|s| s.split('}').next())
                .expect("着色器应声明 Globals")
                .to_string()
        };
        let skybox_body = body_of(skybox);
        let standard_body = body_of(standard);
        let find = |body: &str, from: usize, name: &str| {
            body[from..]
                .find(&format!("{name}:"))
                .map(|i| from + i)
                .unwrap_or_else(|| panic!("Globals 缺少字段 {name}"))
        };
        let mut cursor = (0usize, 0usize);
        for field in order {
            cursor = (
                find(&skybox_body, cursor.0, field),
                find(&standard_body, cursor.1, field),
            );
        }
    }

    /// **meshlet vertex-pull 管线的三重接口看门（R85）**：
    /// ① `meshlet_draw.wgsl` 自持的 `Globals` 是 standard 的**完整前缀**
    ///    （skybox 看门同款——数组长度平移后面所有字段的教训延续）；
    /// ② 其 `VertexOutput` 必须与 standard.wgsl 的逐字段同名同序（管线
    ///    顶点 / 片元跨模块按 location 匹配，字段名不参与匹配但漂移会让
    ///    「逐式同构」的像素等价论证失效）；
    /// ③ `meshlet_cull.wgsl` 与 `meshlet_draw.wgsl` 的 `InstanceEntry`
    ///    必须同字段序（176 B 双侧镜像）。
    #[test]
    fn meshlet_draw_shader_interfaces_match_standard() {
        let standard = include_str!("../shaders/standard.wgsl");
        let draw = include_str!("../shaders/meshlet_draw.wgsl");
        let cull = include_str!("../shaders/meshlet_cull.wgsl");
        let body_of = |src: &str, marker: &str| -> String {
            src.split(marker)
                .nth(1)
                .and_then(|s| s.split('}').next())
                .unwrap_or_else(|| panic!("着色器应声明 {marker}"))
                .to_string()
        };
        // ① Globals 前缀（standard 的全部 11 个字段逐名同序）
        let globals_fields = [
            "view_proj",
            "eye_pos",
            "light_dir",
            "light_color",
            "ambient",
            "point_count",
            "point_lights",
            "shadow_params",
            "env_params",
            "inv_view_proj",
            "point_shadow_params",
        ];
        let std_globals = body_of(standard, "struct Globals {");
        let mesh_globals = body_of(draw, "struct Globals {");
        let find = |body: &str, from: usize, name: &str| {
            body[from..]
                .find(&format!("{name}:"))
                .map(|i| from + i)
                .unwrap_or_else(|| panic!("Globals 缺少字段 {name}"))
        };
        let mut cursor = (0usize, 0usize);
        for field in globals_fields {
            cursor = (
                find(&mesh_globals, cursor.0, field),
                find(&std_globals, cursor.1, field),
            );
        }
        // ② VertexOutput 逐字段同序（跨模块片元接口）
        let vo_fields = ["world_pos", "world_normal", "uv", "tangent", "uv2", "probe"];
        let std_vo = body_of(standard, "struct VertexOutput {");
        let mesh_vo = body_of(draw, "struct VertexOutput {");
        let mut cursor = (0usize, 0usize);
        for field in vo_fields {
            cursor = (
                find(&mesh_vo, cursor.0, field),
                find(&std_vo, cursor.1, field),
            );
        }
        // ③ InstanceEntry 双侧同字段序（compute / draw 两模块镜像）
        let entry_fields = [
            "model0",
            "model1",
            "model2",
            "model3",
            "normal0",
            "normal1",
            "normal2",
            "base_counts",
            "frame_base",
            "bounds_min",
            "bounds_extent",
        ];
        let cull_entry = body_of(cull, "struct InstanceEntry {");
        let draw_entry = body_of(draw, "struct InstanceEntry {");
        let mut cursor = (0usize, 0usize);
        for field in entry_fields {
            cursor = (
                find(&cull_entry, cursor.0, field),
                find(&draw_entry, cursor.1, field),
            );
        }
        // draw 模块不声明 read_write storage（只读 storage 顶点阶段合法）
        assert!(
            !draw.contains("read_write"),
            "meshlet_draw.wgsl 不得声明 read_write storage"
        );
    }
}
