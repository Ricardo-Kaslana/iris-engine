// Iris 标准材质着色器：Cook-Torrance PBR（金属度/粗糙度工作流）。
// 照明 = 环境光（Lambert 近似）+ 1 个方向光 + 最多 8 个点光（物理平方衰减，
// Globals 均摊路径）**或**屏幕空间分箱灯表（R78：点光不限盏数 + 锥光
// SpotLight，mode = 1 时旧点光循环由均匀门跳过，两路共用同一逐灯贡献
// 函数保证双路径位等价）。
// 方向光带级联阴影 CSM v2（R29）：3 层 2D 数组深度贴图（R通道存光空间
// 深度），片元按 view 深度选级联后做 3×3 手工比较 PCF；点光带 cubemap
// **数组**阴影贴图（R61 预算化多盏：逐灯开关 + 槽位，R 通道存归一化线性
// 距离，单采样无 PCF）。
// 两处阴影偏移自 R61 起为「坡度缩放 bias + 法线偏移」（v2，见
// slope_tan / shadow_factor / 点光循环的注释），常数 bias 只作下限。
// 渲染在线性 HDR 空间，色调映射由 blit pass 完成。
// 实例化绘制：每个实例携带模型矩阵（4 个 vec4 列）与法线矩阵（3 个 vec4 列）。
// R63 起带**预积分皮肤次表面散射**：肤质材质（material.sss.x > 0）的直射
// 漫反射改用 (NdotL × 曲率) 预积分 LUT 的包裹值——光软化终结线并渗入
// 背光侧（见 sss_diffuse）。缺省材质（strength = 0）走原路径，一个算术
// 都不多做。

struct PointLight {
    // xyz: 世界位置
    position: vec4<f32>,
    // rgb: 颜色 × 强度, w: 有效半径
    color: vec4<f32>,
    // x: 是否投射 cubemap 阴影（1/0）, y: cubemap 数组槽位（0..预算-1）,
    // z/w: 保留。R61 起阴影参数逐灯携带——多盏点光各投各影，v1 的
    // 「只对 i==0 采样」限制废止。
    shadow: vec4<f32>,
};

struct Globals {
    view_proj: mat4x4<f32>,
    // xyz: 相机世界坐标
    eye_pos: vec4<f32>,
    // xyz: 指向光源的单位方向（表面 -> 光源）
    light_dir: vec4<f32>,
    // rgb * intensity
    light_color: vec4<f32>,
    // rgb（线性，乘在 albedo 上的环境项）
    ambient: vec4<f32>,
    // x: 实际点光数量
    point_count: vec4<f32>,
    point_lights: array<PointLight, 8>,
    // x: 阴影贴图 texel 尺寸(1/2048), y: bias 常数下限, z: 阴影强度,
    // w: 是否启用（R61：y 从 v1 的常数 bias 降为下限，坡度项另加）
    shadow_params: vec4<f32>,
    // x: 预滤波环境最大 mip 级, y: IBL 强度
    env_params: vec4<f32>,
    // 天空盒射线重建用（clip -> world）
    inv_view_proj: mat4x4<f32>,
    // x: 是否有投影灯（全局掩码；逐灯开关看 point_lights[i].shadow.x）,
    // y: 点光 bias 常数下限（米）, z: 坡度缩放系数（对照模式为 0）,
    // w: 法线偏移系数（对照模式为 0）——R61
    point_shadow_params: vec4<f32>,
};

// 反射探针（R62a）：逐探针上传的影响域与 cubemap 槽位。
// 布局与 Rust 侧 `probe::ReflectionProbeRaw` 逐字段对齐（2×vec4 = 32 B）。
struct ReflectionProbeGpu {
    // xyz: 探针世界位置（影响域盒中心 = 实体原点），
    // w: 过渡带宽度（米，域边界向内衰减到 0 的距离）
    center_falloff: vec4<f32>,
    // xyz: 影响域半边长（米），w: cubemap 数组槽位
    extents_slot: vec4<f32>,
};

// 探针 uniform 块（独立缓冲 + 独立绑定槽，**不扩 Globals**——并行布局
// 陷阱见 skybox.wgsl 的头注释）：x = 生效探针数（0 = 全部片元走全局 env），
// w = 视差修正开关（1 = 生产路径；0 = 对照模式，见 set_legacy_probe_parallax）。
struct ProbeParams {
    count: vec4<f32>,
    probes: array<ReflectionProbeGpu, 4>,
};

// 级联阴影参数（256 字节，POD；独立 uniform 缓冲，不占 Globals）。
// 与 Rust 侧 CsmParamsRaw 逐字段对齐（array<mat4,3> = 192B + 4×vec4）。
struct CsmParams {
    // 每级联的光空间视图投影矩阵（世界 -> 第 i 级光源裁剪空间）
    view_proj: array<mat4x4<f32>, 3>,
    // xyz: 相机前向单位向量（片元级联选择用），w: 保留
    cam_forward: vec4<f32>,
    // x/y/z: 级联分割点（沿相机前向的距离，递增；与 CPU cascade_splits
    // 同源），w: 保留。级联 i 覆盖 [splits[i-1], splits[i]]。
    splits: vec4<f32>,
    // x/y/z: 每级联一个纹素的世界边长（米；与 CPU CascadeFit::texel_world
    // 同源），w: 保留。法线偏移量 = 该值 × 系数 × (1 − NdotL)。
    cascade_texel: vec4<f32>,
    // x/y/z: 每级联的 texel_world / depth_window（NDC 深度误差随 tanθ 的
    // 斜率），w: 保留。bias 坡度项 = 该值 × 系数 × tanθ。
    cascade_slope_k: vec4<f32>,
};

struct MaterialParams {
    base_color: vec4<f32>,
    emissive: vec4<f32>,
    // x: metallic, y: roughness, z: 绝缘体反射率基准 F0,
    // w: 光照贴图掩码（R52c，1 = 间接光取光照贴图、0 = 取 IBL 环境）
    surface: vec4<f32>,
    // x: 预积分 SSS 强度（R63，0 = 关闭——片元分支整段跳过），
    // y: 曲率包裹参数 t ∈ [0, 1]（CPU 侧 sss::wrap_param 换算好），
    // z/w: 保留。与 Rust 侧 MaterialRaw 逐字段对齐（64 B）。
    sss: vec4<f32>,
};

// ---- 阴影偏移 v2 的着色器侧常量（R61，与 light.rs 同名常量一一对应）----

// 坡度缩放 bias 系数（× texel_world / depth_window × tanθ）
const SHADOW_SLOPE_SCALE: f32 = 1.5;
// bias 上限（NDC）：tanθ 在掠射极端发散，不封顶会让掠射面退化为无影
const SHADOW_BIAS_MAX: f32 = 0.02;
// 法线偏移系数（× texel_world × (1 − NdotL)）与世界上限（米）
const SHADOW_NORMAL_SCALE: f32 = 1.5;
const SHADOW_NORMAL_MAX: f32 = 0.15;
// tan(acos(NdotL)) 的数值上限：clamp 保证「系数 × tan」不产生 Inf/NaN
// （对照模式下系数为 0，Inf 会给出 NaN 并把整帧判成阴影）
const SHADOW_TAN_MAX: f32 = 64.0;
// 点光 bias 的米制上限，与点光法线偏移的世界上限
const POINT_SHADOW_BIAS_MAX: f32 = 0.5;
const POINT_SHADOW_NORMAL_MAX: f32 = 0.15;
// 点光 cubemap 一个纹素在距离 d 处的世界边长：90° 面视锥摊在 512 个纹素上
const POINT_SHADOW_TEXEL_SCALE: f32 = 2.0 / 512.0;

@group(0) @binding(0) var<uniform> globals: Globals;
// 级联阴影贴图：R32Float 2D 数组，层序 = 级联序（R 通道存光空间深度，
// 清空值 1.0 = 无遮挡）；采样复用 binding 2 的 NonFiltering 采样器
// （R32Float 双线性过滤需要 float32-filterable 扩展，不依赖）。
@group(0) @binding(1) var shadow_map: texture_2d_array<f32>;
@group(0) @binding(2) var shadow_sampler: sampler;
// IBL 分割求和：辐照度立方体贴图 / 预滤波环境立方体贴图 / BRDF LUT
@group(0) @binding(3) var irradiance_map: texture_cube<f32>;
@group(0) @binding(4) var env_map: texture_cube<f32>;
@group(0) @binding(5) var brdf_lut: texture_2d<f32>;
@group(0) @binding(6) var env_sampler: sampler;
// 点光 cubemap 阴影贴图**数组**（R61）：R 存归一化线性距离
// length(world-light)/range（清空值 1.0 = 无遮挡），层 = 槽位 × 6 + 面，
// 逐灯按 point_lights[i].shadow.y 选 cube。
@group(0) @binding(7) var point_shadow_cube: texture_cube_array<f32>;
// 级联阴影参数（排在现有最大 binding 7 之后，不扩 Globals）
@group(0) @binding(8) var<uniform> csm: CsmParams;
// 反射探针（R62a）：参数（binding 9）+ cubemap 数组（binding 10，层序 =
// 槽位 × 6 + 面序，5 级 mip 与 env 同档）。采样复用 binding 6 的 Filtering
// 采样器（Rgba16Float 可过滤）。两者同样排在最大 binding 之后、不扩 Globals。
@group(0) @binding(9) var<uniform> probe_params: ProbeParams;
@group(0) @binding(10) var probe_cube: texture_cube_array<f32>;
// SSR 上帧资源（R62b，排在探针之后、不扩 Globals 前缀——skybox.wgsl 的
// 并行布局陷阱不触碰）：binding 11 = 上帧主解析缓冲帧尾副本（Rgba16Float，
// 可过滤，命中后一次 textureSampleLevel 取反射色）；binding 12 = 上帧场景
// 深度副本（R32Float 线性视距，textureLoad 读取——非过滤，与 DOF/MB 的
// 深度副本同源同口径，步进中逐样本读，不用采样器）；binding 13 = SSR
// 参数 uniform（flags.x = 0 时全部片元早退，缺席路径不执行任何步进）。
// 上帧深度的「读后写」时序：本帧 scene_depth pass 编码在主 pass 之后，
// 同一命令编码器内按序执行 ⇒ 主 pass 读到的恒为上一帧写入的内容
// （ARCHITECTURE §4.31 裁决 2）。
@group(0) @binding(11) var ssr_prev_color: texture_2d<f32>;
@group(0) @binding(12) var ssr_prev_depth: texture_2d<f32>;

// SSR 参数（96 B，POD；与 Rust 侧 SsrParamsRaw 逐字段对齐）。
struct SsrParams {
    // 上帧实际渲染矩阵（上帧快照，含 TAA 抖动——与 MB/TAA 的重投影基准
    // 同源）：步进样本投影到上帧屏幕取深度 / 颜色
    prev_view_proj: mat4x4<f32>,
    // x: 本帧是否执行（0 = 首帧启用 / 关→开 / 分辨率变化 / 切镜 / 无几何
    //    帧 ⇒ 整段跳过，与 SSR 关闭帧逐位一致）,
    // y: 全局强度, z: 粗糙度早退阈值, w: 屏幕边缘衰减带宽（UV 比例）
    flags: vec4<f32>,
    // x: 步进最大距离（米）, y: 厚度容差（米）, z/w: 保留
    extent: vec4<f32>,
};

@group(0) @binding(13) var<uniform> ssr: SsrParams;

// GTAO 可见度图（R77，排在 SSR 之后、不扩 Globals 前缀——skybox.wgsl 的
// 并行布局陷阱不触碰）：R16Float 半分辨率，R 通道 = 环境可见度 [0,1]，
// 由主 pass 之前的独立 pass 写出（shaders/ao.wgsl，读上一帧深度副本）。
// **缺席/关闭 = 1×1 哑纹理、纹素恰为 1.0**：无条件采样后 `× 1.0` 逐位
// 精确——不需要分支，缺席路径的零开销由纹素值承担（与探针/光照贴图的
// select 零算术同目标的另一形态）。采样复用 binding 6 的 Filtering 采样
// 器（R16Float 可过滤）；半分辨率 → 全分辨率的线性上采样即 AO 的低成本
// 恢复滤波（低频量，双线性足够）。
@group(0) @binding(14) var ao_visibility_tex: texture_2d<f32>;

// 前向光源分箱（R78，排在 GTAO binding 14 之后、不扩 Globals 前缀——
// skybox.wgsl 的并行布局陷阱不触碰）。三路资源：
//   15 = 参数 uniform（32B）：screen = (宽, 高, tiles_x, tiles_y)；
//        counts.x = mode（0 = 关——缺席/捕获/RT 绑全零哑缓冲，分箱分支
//        死码，Globals 均摊旧路径逐位不变；1 = 灯表生效，旧点光循环由
//        下方的 uniform 门跳过，两路互斥不重复计灯）；counts.y/z =
//        灯总数 / index 总数（诊断口径）；w 保留。
//   16 = 灯表 read-only storage：BinnedLight 定长数组（容量 512，与 Rust
//        侧 BinnedLightRaw 逐字段对齐，5×vec4 = 80 B/条——前 3×vec4 与
//        Globals.PointLight 同布局，灯表前段（前 8 盏点光）与 Globals 逐
//        字段一致，阴影参数原样携带）。
//   17 = tile 索引 read-only storage：前 2·tiles_x·tiles_y 个 u32 是每
//        tile 的 [start, count]（行主序 tile_id = ty·tiles_x + tx，ty 自
//        屏幕顶端起算——与 @builtin(position) 同向），其后是灯下标展平区
//        （tile 内按灯数组序升序——灯数组序即求和序，双路径位等价的根基
//        之一；CPU 侧 bin_lights 用 BTreeMap 保证展平确定性）。
// 锥光因子 cos(inner)/cos(outer) 由 CPU 预计算进 cone.w / outer.x（CPU
// 侧 spot_cone_factor 与下方 WGSL 镜像同式同序，两侧必须同步修改）。
struct BinnedLight {
    // xyz: 世界位置, w: 0.0 点光 / 1.0 锥光
    pos_type: vec4<f32>,
    // rgb × intensity, w: 有效半径（与 Globals.PointLight.color 同布局）
    color_range: vec4<f32>,
    // 与 Globals.PointLight.shadow 同语义：x 投影开关, y cubemap 槽位
    // （灯表前段与 Globals 逐字段一致；锥光恒 0——v1 不投影阴影）
    shadow: vec4<f32>,
    // xyz: 单位化锥轴方向, w: cos(inner_cutoff)
    cone: vec4<f32>,
    // x: cos(outer_cutoff), yzw: 保留
    outer: vec4<f32>,
};

// 32B 参数块（与 Rust 侧 LightBinsParamsRaw 逐字段对齐）
struct LightBinsParams {
    // 宽、高、tiles_x、tiles_y
    screen: vec4<f32>,
    // x: mode（0/1）, y: 灯总数, z: index 总数, w: 保留
    counts: vec4<f32>,
};

@group(0) @binding(15) var<uniform> bins_params: LightBinsParams;
@group(0) @binding(16) var<storage, read> binned_lights: array<BinnedLight>;
@group(0) @binding(17) var<storage, read> bins_tiles: array<u32>;

// GI 探针三阶 SH（R83）+ DDGI 深度表（R88，排在分箱 binding 17 之后、不扩
// Globals 前缀——skybox.wgsl 的并行布局陷阱不触碰）。三路资源：
//   18 = 探针表 read-only storage：GiProbe 定长数组（容量 256 = Rust 侧
//        MAX_IRRADIANCE_PROBES，与 irradiance::ShProbeRaw 逐字段对齐，
//        10×vec4 = 160 B/探针）。R88 的 mode = 2 复用同一张表（DDGI 在场 ⇒
//        整表替换，与 R83 烘焙探针集互斥）。
//   19 = 16B 小参数 uniform：params.x = mode（0 = 关——缺席 / 无 SH 探针集 /
//        捕获 / RT 绑全零哑缓冲，逐片元分支死码，loc14 逐实体路径逐位不变；
//        1 = R83 逐片元 SH 求值替代 GI 探针项；2 = R88 DDGI 深度感知混合），
//        params.y = 探针数（循环上界），params.z = DDGI 深度网格边长
//        （R88，= ddgi::DDGI_DEPTH_GRID = 8；mode ≤ 1 时不消费），
//        params.w = DDGI 深度项开关（R88，1 = 生产路径；0 = 对照模式，见
//        Renderer::set_ddgi_depth_weight_enabled，仅供验收图与归因测试）。
//   20 = DDGI 深度表 read-only storage：`array<f32>` **标量数组**（R88）。
//        片元阶段只读 storage 计数 = 4（16/17/18/20），恰在 WebGPU 默认的
//        maxStorageBuffersPerShaderStage = 4 之内。
// 系数存的是 **E/π 直读**值（Lambert 带通已在 CPU 侧卷积进系数，见
// irradiance.rs 的 sh_lambert_convolve）⇒ 求值是 9 项纯点积，与
// irradiance_map 采样值 / loc14 载荷同口径。
struct GiProbe {
    // xyz: 探针世界位置, w: 影响半径（米；inf = 覆盖全场）
    pos_radius: vec4<f32>,
    // 9 组「E/π 直读」SH 系数：sh[k] = (R_k, G_k, B_k, 保留)
    sh: array<vec4<f32>, 9>,
};

// 16B 参数块（与 Rust 侧 GiShParamsRaw 逐字段对齐）
struct GiShParams {
    // x: mode（0/1/2）, y: 探针数, z: DDGI 深度网格边长, w: DDGI 深度项开关
    params: vec4<f32>,
};

@group(0) @binding(18) var<storage, read> gi_probes: array<GiProbe>;
@group(0) @binding(19) var<uniform> gi_params: GiShParams;
@group(0) @binding(20) var<storage, read> gi_depth: array<f32>;
@group(1) @binding(0) var<uniform> material: MaterialParams;
@group(1) @binding(1) var base_texture: texture_2d<f32>;
@group(1) @binding(2) var base_sampler: sampler;
// 法线 / ORM 数据贴图（R42，均线性格式）。两槽位由渲染器保证恒有绑定：
// 材质缺省时绑中性常驻纹理（法线 = (0.5,0.5,1) 的 Rgba16Float——8-bit
// unorm 表示不了 0.5（128/255≈0.502，*2−1 残留 0.0039 扰动），16-bit
// float 恰好精确；ORM = (1,1,1)）。采样共用 base_sampler（Filtering）。
// 零分支保证：无这两张贴图的材质与引入本槽位前（R41）逐位一致，
// 论证链见 material.rs 模块文档。
@group(1) @binding(3) var normal_texture: texture_2d<f32>;
@group(1) @binding(4) var orm_texture: texture_2d<f32>;
// 光照贴图图集（R52c）：Rgba16Float 线性 HDR **间接**辐照度（天空 AO +
// 一次反弹色溢，不含烘焙直接光——直接光由下面的动态光提供，混合语义见
// lightmap.rs 模块文档）。UV2 顶点属性（loc13）直接是图集归一化坐标 ⇒
// 采样零常量变换，同一图集的全部实体共用一份绑定组。
// 无光照贴图的材质绑渲染器常驻的 1×1 黑纹理，且 material.surface.w = 0
// ⇒ 间接项走 select 的 IBL 分支（不做任何算术，逐位等于 R52b）。
@group(1) @binding(5) var lightmap_texture: texture_2d<f32>;
@group(1) @binding(6) var lightmap_sampler: sampler;
// 预积分皮肤 SSS LUT（R63）：Rgba16Float 64×64 常驻纹理，u = NdotL
// （纹素中心对齐）、v = 曲率包裹参数，RGB 同值（可当图看）、A = 1。
// 采样复用 binding 6 的 ClampToEdge 双线性采样器（LUT 坐标恒在 [0,1]，
// Repeat 会在端点混入对侧纹素）。生成式与精度依据见 `sss.rs` 模块文档。
@group(1) @binding(7) var sss_lut: texture_2d<f32>;
// 蒙皮：关节矩阵（vs_skinned 专用；vs_main 静态不使用，
// 因此常规管线的 layout 无需 group 2）
@group(2) @binding(0) var<storage, read> joint_mats: array<mat4x4<f32>>;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) model0: vec4<f32>,
    @location(4) model1: vec4<f32>,
    @location(5) model2: vec4<f32>,
    @location(6) model3: vec4<f32>,
    @location(7) normal0: vec4<f32>,
    @location(8) normal1: vec4<f32>,
    @location(9) normal2: vec4<f32>,
    // 顶点切线（loc12，独立顶点缓冲；网格无切线时渲染器绑定零填充垫，
    // 片元据此走屏幕空间 TBN 路径——xyz 全零即「无顶点切线」标记）
    @location(12) tangent: vec4<f32>,
    // 光照贴图 UV（loc13，独立顶点缓冲；网格无 UV2 时绑零填充垫，配合
    // material.surface.w = 0 的掩码，采样结果被 select 丢弃）
    @location(13) uv2: vec2<f32>,
    // 光照探针间接辐照（R64，实例属性 loc14）：xyz = E/π（与 irradiance_map
    // 采样值同口径）、w = 标志位（0 = 探针缺席 ⇒ 走原全局 env 路径）
    @location(14) probe: vec4<f32>,
};

// 阴影投影改在片元做（按 view 深度选级联后乘该级联矩阵），
// 顶点不再输出 shadow_pos（R29 CSM 前为 light_view_proj × world_pos）。
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    // 世界空间切线：xyz = 法线矩阵变换后的切线方向（未归一化，片元
    // 正交化），w = 手性符号（±1，插值保号——凸组合不越过 0）
    @location(3) tangent: vec4<f32>,
    // 光照贴图图集坐标（R52c，插值后直接采样——图集坐标是仿射量，
    // 透视正确插值由硬件的 1/w 校正照常处理）
    @location(4) uv2: vec2<f32>,
    // 光照探针载荷（R64）：逐实例常数，插值天然不变（四个顶点同值）
    @location(5) probe: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    let model = mat4x4<f32>(in.model0, in.model1, in.model2, in.model3);
    let normal_mat = mat3x3<f32>(in.normal0.xyz, in.normal1.xyz, in.normal2.xyz);
    let world_pos = model * vec4<f32>(in.position, 1.0);
    var out: VertexOutput;
    out.clip_position = globals.view_proj * world_pos;
    out.world_pos = world_pos.xyz;
    out.world_normal = normal_mat * in.normal;
    out.uv = in.uv;
    out.tangent = vec4<f32>(normal_mat * in.tangent.xyz, in.tangent.w);
    out.uv2 = in.uv2;
    out.probe = in.probe;
    return out;
}

// 蒙皮顶点输入：在常规属性之外多一路顶点缓冲（loc10 关节索引 / loc11 权重）。
struct SkinnedVertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) model0: vec4<f32>,
    @location(4) model1: vec4<f32>,
    @location(5) model2: vec4<f32>,
    @location(6) model3: vec4<f32>,
    @location(7) normal0: vec4<f32>,
    @location(8) normal1: vec4<f32>,
    @location(9) normal2: vec4<f32>,
    @location(12) tangent: vec4<f32>,
    @location(13) uv2: vec2<f32>,
    @location(14) probe: vec4<f32>,
    @location(10) joints: vec4<u32>,
    @location(11) weights: vec4<f32>,
};

// 线性混合蒙皮（LBS）：skin = Σ w_i · J_i，再叠加实例模型矩阵。
// 关节矩阵已在 CPU 侧乘过蒙皮实体全局逆（见 skin.rs），
// 因此 skin × position 处于蒙皮实体局部空间，随父级移动整体跟随。
// 法线按刚性变换处理（关节矩阵的 3x3 部分）；蒙皮实体自身的
// 非均匀缩放仍由实例法线矩阵承担。切线同法线一路变换（方向向量）。
@vertex
fn vs_skinned(in: SkinnedVertexInput) -> VertexOutput {
    let j = in.joints;
    let w = in.weights;
    let skin_mat = joint_mats[j.x] * w.x + joint_mats[j.y] * w.y
                 + joint_mats[j.z] * w.z + joint_mats[j.w] * w.w;
    let model = mat4x4<f32>(in.model0, in.model1, in.model2, in.model3);
    let normal_mat = mat3x3<f32>(in.normal0.xyz, in.normal1.xyz, in.normal2.xyz);
    let skinned_pos = model * (skin_mat * vec4<f32>(in.position, 1.0));
    let skin_nrm = mat3x3<f32>(skin_mat[0].xyz, skin_mat[1].xyz, skin_mat[2].xyz);
    var out: VertexOutput;
    out.clip_position = globals.view_proj * skinned_pos;
    out.world_pos = skinned_pos.xyz;
    out.world_normal = normal_mat * (skin_nrm * in.normal);
    out.uv = in.uv;
    out.tangent = vec4<f32>(normal_mat * (skin_nrm * in.tangent.xyz), in.tangent.w);
    out.uv2 = in.uv2;
    out.probe = in.probe;
    return out;
}

// ---- Cook-Torrance 项 ----

// 1/π 的 f32 最近值（0x3EA2F983 = 0.3183098733425140380859375）：光照
// 贴图存辐照度 E，Lambert 出射 = albedo/π × E（R52c，见 fs_main）。
const INV_PI: f32 = 0.3183098862;

// 法线分布：GGX / Trowbridge-Reitz
fn distribution_ggx(noh: f32, roughness: f32) -> f32 {
    let a = roughness * roughness;
    let a2 = a * a;
    let d = noh * noh * (a2 - 1.0) + 1.0;
    return a2 / (3.14159265 * d * d + 1e-7);
}

// 几何遮蔽：Schlick-GGX（k = roughness²/2，直接光照近似）
fn geometry_schlick_ggx(nov: f32, roughness: f32) -> f32 {
    let k = (roughness * roughness) * 0.5;
    return nov / (nov * (1.0 - k) + k);
}

fn geometry_smith(nov: f32, nol: f32, roughness: f32) -> f32 {
    return geometry_schlick_ggx(nov, roughness) * geometry_schlick_ggx(nol, roughness);
}

// 菲涅尔：Schlick 近似
fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (vec3<f32>(1.0) - f0) * pow(clamp(1.0 - cos_theta, 0.0, 1.0), 5.0);
}

// 环境光专用菲涅尔：边缘反射率随粗糙度抬高（避免粗糙金属掠射角过暗）
fn fresnel_schlick_roughness(cos_theta: f32, f0: vec3<f32>, roughness: f32) -> vec3<f32> {
    let fmax = max(vec3<f32>(1.0 - roughness), f0);
    return f0 + (fmax - f0) * pow(clamp(1.0 - cos_theta, 0.0, 1.0), 5.0);
}

// 单灯的 Cook-Torrance 贡献（radiance 已含强度与衰减）
fn pbr_light(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, radiance: vec3<f32>,
             albedo: vec3<f32>, f0: vec3<f32>, metallic: f32, roughness: f32) -> vec3<f32> {
    let h = normalize(v + l);
    let nol = max(dot(n, l), 0.0);
    let nov = max(dot(n, v), 0.0);
    let noh = max(dot(n, h), 0.0);
    let voh = max(dot(v, h), 0.0);

    let d = distribution_ggx(noh, roughness);
    let g = geometry_smith(nov, nol, roughness);
    let f = fresnel_schlick(voh, f0);
    let specular = (d * g * f) / max(4.0 * nov * nol, 1e-4);
    // 能量守恒：漫反射扣除菲涅尔反射占比，金属无漫反射
    let kd = (vec3<f32>(1.0) - f) * (1.0 - metallic);
    return (kd * albedo / 3.14159265 + specular) * radiance * nol;
}

// ---- 预积分皮肤次表面散射（R63，Penner 2011 预积分近似的引擎实现）----
//
// 积分式、精度依据与调参指引见 Rust 侧 `iris_render::sss` 模块文档
// （LUT 在那里程序化生成并上传）；此处是 GPU 侧的消费端，两个纯函数
// （sss_uv / sss_diffuse）与 `sss.rs` 的 `lut_uv` / `ndotl_response`
// 逐式对应，**必须同步修改**。

// LUT 边长（与 `sss::SSS_LUT_SIZE` 同值。WGSL 无「Rust 常量导入」，
// 靠这条注释 + 两侧单测（Rust 侧 lut_uv / GPU 侧验收图断言）看门）。
const SSS_LUT_SIZE: f32 = 64.0;

/// 归一化坐标（几何 NdotL、曲率包裹参数 t）→ LUT 采样坐标（CPU 镜像：
/// `sss::lut_uv`）。**纹素中心对齐**：`idx = coord·(N−1)`、`uv = (idx+0.5)/N`
/// ——`c = ±1` / `t = 1` 恰好落在首末纹素中心，端点无插值截断（LUT 的
/// `W(1) = 1 / W(−1) = 0` 因此逐位精确，全照面不会被抹暗）。
fn sss_uv(ndotl: f32, curvature: f32) -> vec2<f32> {
    let idx = vec2<f32>(ndotl * 0.5 + 0.5, curvature) * (SSS_LUT_SIZE - 1.0);
    return (idx + vec2<f32>(0.5)) / SSS_LUT_SIZE;
}

/// 直射漫反射的 **SSS 补项**：漫反射的 NdotL 由几何值换成球冠预积分值
/// `W = LUT(NdotL, 曲率)`，补项 = `kd · albedo/π · radiance · (nol_sss − nol)`，
/// 其中 `nol_sss = mix(nol, W, strength)`（strength ∈ [0,1] 线性混合）。
///
/// **为什么是「相加的补项」而不是把 `nol` 直接替换进 `pbr_light`**：
/// 1. **镜面项必须留在几何 NdotL 上**——皮下散射是漫反射现象，高光仍
///    遵守几何反射定律（改 `pbr_light` 内部会让高光在背光侧凭空出现）；
/// 2. **零值可证**：调用点在 `if (material.sss.x > 0.0)` 内（uniform
///    条件 ⇒ 无分支发散），strength ≤ 0 时整段不执行——比
///    `mix(a, b, 0) = a·1 + b·0` 的浮点恒等式论证更强（后者在 `a = −0.0`
///    时会翻符号，见 material.rs 模块文档的兼容铁律）。
///
/// **金属自动归零**：能量权重 `(1 − metallic)`，metallic = 1 ⇒ 权重 = 0
/// ⇒ 补项恒为 ±0.0，**无需额外分支**；GPU 测试以「metallic = 1 时
/// strength 0/1 整帧逐位一致」作硬断言。
///
/// **能量权重为什么不是 `kd = (1 − F)(1 − metallic)`（R63 实测教训）**：
/// 初版按 `kd` 写（与 `pbr_light` 的能量守恒式同源），需要在补项里重算
/// `h = normalize(v + l)` 与 `fresnel_schlick(voh, f0)`——这三个子表达式
/// 与 `pbr_light` 完全重合，**改变了编译器在 `pbr_light` 内的公共子表达式
/// 消除 / 内联决策，进而改变其浮点收缩（FMA）选择**：实测
/// `tests/shadow_v2_render.rs::legacy_bias_mode_reproduces_r60_output_bitwise`
/// 的接触阴影对照场景有 **3 个字节 ±1 LSB** 的漂移（320×180×4 中的 3 个
/// 通道，都在接触阴影边缘——PCF 比较的临界像素）。该漂移与 SSS 逻辑无关
/// （strength = 0 时补项根本不执行），纯属着色器代码增量的编译期副作用。
/// 去掉复用（权重只留 `1 − metallic`）后该基线**逐位复原**（实测）。
/// 代价：绝缘体的漫反射能量权重比 `kd` 高约 4%（F0 = 0.04 起）——对
/// 「包裹补项」这种近似项完全在噪声量级，且换来**默认路径的编译期稳定**。
/// **教训**：新增直射项时不要在辅助函数里重算 `pbr_light` 的子表达式，
/// 否则会以「±1 LSB 基线漂移」的形式付出代价。
///
/// **阴影协同（乘位论证）**：调用方传入的 `radiance` 已经乘过阴影因子
/// ——方向光是 `light_color × shadow_factor`（级联 CSM + 3×3 PCF），
/// 点光是 `light_color × atten × shadow`（cubemap）——补项与主体项
/// **消费同一个已乘完阴影的辐亮度**，不存在第二条绕过阴影的路径。
/// 投影阴影内 radiance = 0 ⇒ 补项 = 0（背光侧的包裹**绝不晒穿投影阴影**）；
/// 点光分支里 `dist < light.color.w` 的窗口衰减同理（补项不越过光源
/// 有效半径）。GPU 测试断言「阴影区像素与 strength 无关（逐位）」。
fn sss_diffuse(n: vec3<f32>, l: vec3<f32>, radiance: vec3<f32>,
               albedo: vec3<f32>, metallic: f32,
               strength: f32, curvature: f32) -> vec3<f32> {
    // **LUT 的 u 轴吃带符号的 NdotL**（[−1, 1] → [0, 1]），不是 max(·, 0)
    // 之后的几何 nol：背光侧的负值正是「光包裹进来」的自变量，clamp 到 0
    // 会把整个背光面采样到 LUT 的 c = 0 那一列（常数 0.18），包裹形状退化
    // 成一块平顶——R63 落地时的实测症状（剖面在背光侧恒为常数）。
    let ndotl = dot(n, l);
    let nol = max(ndotl, 0.0);
    // textureSampleLevel（显式 mip）不依赖屏幕导数 ⇒ 在材质级分支内合法。
    let wrapped = textureSampleLevel(sss_lut, lightmap_sampler,
                                     sss_uv(ndotl, curvature), 0.0).r;
    let nol_sss = mix(nol, wrapped, strength);
    // 能量权重只留 (1 − metallic)：不重算 normalize(v+l)/Schlick（见上）
    return (1.0 - metallic) * albedo / 3.14159265 * radiance * (nol_sss - nol);
}

/// `tan(acos(NdotL))` 的保守取值（clamp 到 SHADOW_TAN_MAX）。
///
/// 坡度缩放 bias 的核心量：接收面相对光源平面的倾角 θ 使「一个阴影图纹素
/// 跨度内的深度变化」= texel_world · tanθ，这正是最近邻比较下的自遮挡量
/// （acne）。NdotL → 0 时 tan 发散，clamp 到 64（≈ NdotL ≥ 0.016）既保
/// 有界、也避免 `系数 × tan` 在对照模式（系数 = 0）下产生 NaN。
fn slope_tan(nol: f32) -> f32 {
    let n = max(nol, 0.0);
    return min(sqrt(max(1.0 - n * n, 0.0)) / max(n, 1e-4), SHADOW_TAN_MAX);
}

/// 级联阴影采样（CSM v2 + 阴影偏移 v2）：按片元的 view 深度选级联，
/// 用该级联的光空间矩阵投影后做 3×3 PCF 百分比渐近软阴影（R 通道存光源
/// 空间深度，清空值 1.0 代表无遮挡）。返回阴影后保留的光照比例。
///
/// **级联选择用 view 深度**（世界位置到相机的前向投影距离
/// `dot(world_pos − eye, cam_forward)`），而非欧氏距离——欧氏距离在视锥
/// 边缘会把本属近级联的片元误分到远级联；也不直接用 clip z（透视下
/// 非线性，分割点得先在 CPU 做一次非线性换算，view 深度更直白）。
/// 选择条件与 CPU 分割同源：`view_z < splits[i]` 即落入第 i 级。
/// 级联选择用**原始**世界位（法线偏移只作用于投影采样点，不改变片元的
/// 深度归属——否则边界处的片元会随偏移在级联间跳变）。
///
/// **越最远级联（view_z ≥ splits.z，即超过 SHADOW_DISTANCE 覆盖带）的
/// 片元不投影、保持受光**（lit = 1）——级联只铺到覆盖距离。
///
/// **阴影偏移 v2（R61）**：`bias = clamp(bias_min + SLOPE_SCALE · k · tanθ,
/// bias_min, BIAS_MAX)`（k = texel_world / depth_window，逐级联取自
/// `csm.cascade_slope_k`），采样点另沿几何法线推移
/// `NORMAL_SCALE · texel_world · (1 − NdotL)`（`csm.cascade_texel`）。
/// 两项都由 CPU 逐级联算好世界尺度后传入——级联越远纹素越大、深度窗口
/// 越宽，偏移量随之缩放，平地上则退回常数下限（v1 的 0.0015 降到
/// 0.0004，peter-panning 相应缩小）。
///
/// textureSample 要求 uniform 控制流，因此级联越界/阴影关闭都不做提前
/// 返回：级联序号钳制在 [0, 2] 后照常采样，最后用掩码把结果混向「受光」。
/// 阴影相机覆盖范围外（NDC/深度越界，含光空间背后）的片段同样混向受光。
fn shadow_factor(world_pos: vec3<f32>, n_geo: vec3<f32>, l: vec3<f32>) -> f32 {
    let view_z = dot(world_pos - globals.eye_pos.xyz, csm.cam_forward.xyz);
    var cascade = 2u;
    if (view_z < csm.splits.x) {
        cascade = 0u;
    } else if (view_z < csm.splits.y) {
        cascade = 1u;
    }
    let beyond = view_z >= csm.splits.z || globals.shadow_params.w < 0.5;
    // 偏移 v2：坡度缩放 bias（法线入射角越大偏移越大）+ 法线偏移
    let nol = max(dot(n_geo, l), 0.0);
    let texel_w = csm.cascade_texel[cascade];
    let bias = clamp(
        globals.shadow_params.y
            + SHADOW_SLOPE_SCALE * csm.cascade_slope_k[cascade] * slope_tan(nol),
        globals.shadow_params.y,
        SHADOW_BIAS_MAX,
    );
    let n_off = min(SHADOW_NORMAL_SCALE * texel_w * (1.0 - nol), SHADOW_NORMAL_MAX);
    let sp = csm.view_proj[cascade] * vec4<f32>(world_pos + n_geo * n_off, 1.0);
    let w = max(sp.w, 1e-6);
    let ndc = sp.xy / w;
    let depth = sp.z / w;
    // NDC -> UV：NDC y 正方向 = 贴图上端（与 blit 的 V 翻转约定一致）；
    // 数组层 = 级联序。PCF texel 尺寸各级相同（各级都铺满同一张
    // 2048² 贴图，UV 空间 1/2048），循环共用。
    let uv = ndc * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    let texel = globals.shadow_params.x;
    var lit = 0.0;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let d = textureSample(shadow_map, shadow_sampler,
                                  uv + vec2<f32>(f32(dx), f32(dy)) * texel,
                                  i32(cascade)).r;
            // 遮挡物深度 >= 片段参考深度 - bias -> 未被遮挡
            lit = lit + select(0.0, 1.0, d >= depth - bias);
        }
    }
    lit = lit / 9.0;
    let shaded = mix(1.0 - globals.shadow_params.z, 1.0, lit);
    let outside = beyond
        || sp.w <= 0.0
        || ndc.x < -1.0 || ndc.x > 1.0
        || ndc.y < -1.0 || ndc.y > 1.0
        || depth < 0.0 || depth > 1.0;
    return mix(shaded, 1.0, select(0.0, 1.0, outside));
}

// ---- 反射探针（R62a）：影响域权重 + 视差修正（box projection）----
//
// 与 Rust 侧 `crate::probe::{influence_weight, parallax_direction}` 同一
// 公式（解析单测锁定 Rust 版；此处逐行镜像），两者必须同步修改。

// 影响域权重：逐轴 f_i = clamp((E_i − |p_i|) / falloff, 0, 1)，三轴乘积。
// 域内（|p_i| ≤ E_i − falloff）恒 1；过渡带内随 |p_i| 线性降到 0；任一轴
// 越界（|p_i| > E_i）⇒ 因子 0 ⇒ 乘积 0（域外必不生效）。falloff = 0 时
// 退化为二值判定（不做除法，避免 0/0）。p = 片元 − 探针中心（盒轴对齐）。
fn probe_weight(p: vec3<f32>, e: vec3<f32>, falloff: f32) -> f32 {
    var w = 1.0;
    for (var i = 0; i < 3; i = i + 1) {
        let d = e[i] - abs(p[i]);
        if (falloff > 0.0) {
            w = w * clamp(d / falloff, 0.0, 1.0);
        } else {
            w = w * select(0.0, 1.0, d >= 0.0);
        }
    }
    return w;
}

// 视差修正（经典 box projection）：把反射方向重定向到「从探针中心看向盒面
// 出射点」的方向。射线 p + t·d 与盒 [−E, E] 逐轴求交：
//   d_i ≠ 0 :  t_i = (sign(d_i)·E_i − p_i) / d_i
//   d_i = 0 :  |p_i| ≤ E_i ⇒ 不约束；|p_i| > E_i ⇒ 射线恒在板外（无交点）
//   t = min_i t_i，  q = p + t·d，  d' = normalize(q)
// 盒外且射线背离盒（t ≤ 0）/ 平行失配 / 零方向 ⇒ 原样返回（该片元权重
// 必为 0，由调用方回落到全局 env）。d_i 精确为 0 时不做除法（无 NaN）。
fn box_project(d: vec3<f32>, p: vec3<f32>, e: vec3<f32>) -> vec3<f32> {
    // 无交点哨兵：远大于任何真实盒出射参数（米制场景），不引入 Inf/NaN
    let T_MAX = 1e30;
    var t_min = T_MAX;
    var miss = false;
    for (var i = 0; i < 3; i = i + 1) {
        let di = d[i];
        if (di == 0.0) {
            if (abs(p[i]) > e[i]) {
                miss = true;
            }
            continue;
        }
        let bound = select(-e[i], e[i], di > 0.0);
        t_min = min(t_min, (bound - p[i]) / di);
    }
    if (miss || t_min <= 0.0 || t_min >= T_MAX) {
        return d;
    }
    let hit = p + t_min * d;
    if (dot(hit, hit) <= 0.0) {
        return d;
    }
    return normalize(hit);
}

// ---- SSR（R62b）：屏幕空间反射——前向片元内对上帧颜色/深度射线步进 ----
//
// 单一来源的合成公式（ARCHITECTURE §4.31 裁决 4，与 Rust 侧
// `post::{ssr_roughness_fade, ssr_edge_fade}` 同一公式）：
//
//   spec_refl_w = spec × (f_amb·brdf.x + brdf.y)            // 既有探针/env 镜面项
//   w_ssr       = roughness_fade(r) × edge_fade(uv) × 强度 × [命中]
//   roughness_fade(r) = clamp((CUT − r)/(CUT − FLOOR), 0, 1)
//   edge_fade(uv)     = clamp(min(u.x,1−u.x)/M) × clamp(min(u.y,1−u.y)/M)
//   ssr_spec    = prev_color(uv_hit) × f_amb                 // BRDF 权 = 纯 Fresnel
//   spec_refl_w ← mix(spec_refl_w, ssr_spec, w_ssr)          // 命中才执行
//
// 未命中 / 权重 0 ⇒ mix 不执行或取 0 ⇒ spec_refl_w 保持既有通路原串位
// （缺省路径逐位一致：SSR 关闭时整段分支不进入，不执行任何步进算术）。

// 线性步进步数：32 步 × 默认最大距离 12 m = 0.375 m 步长。穿越 0.2–0.5 m
// 体块时的最大穿透深度 ≤ 一个步长（法向穿越），小于默认厚度 0.5 m ⇒ 不
// 漏判；步数翻倍对薄体块收益递减、对片元成本线性（每步一次 mat4 乘 +
// textureLoad），32 在两者间取中。
const SSR_STEPS: u32 = 32u;
// 命中后二分细化轮数：把命中精度从步长 0.375 m 收到 0.375/2⁶ ≈ 6 mm
// （256 px 宽画面、3 m 距离下 ≈ 0.5 px——反射落点的像素级对齐由此保证）。
const SSR_REFINE: u32 = 6u;
// 粗糙度衰减带：≤ 0.25 全量、≥ 0.6 早退（早退依据：GGX lobe 半角 ~40°，
// 单根镜像射线无法积分该宽度，且上帧颜色无预滤波 mip——直接采样是噪声；
// 该段探针/env 预滤波已是正确估计，步进纯浪费）。与 Rust 侧
// `post::SSR_ROUGH_FLOOR` 同值，同步修改。
const SSR_ROUGH_FLOOR: f32 = 0.25;

// 粗糙度衰减（CPU 镜像：post::ssr_roughness_fade）。带退化时不做除法。
fn ssr_roughness_fade(r: f32, cutoff: f32) -> f32 {
    if (cutoff <= SSR_ROUGH_FLOOR) {
        return select(0.0, 1.0, r < cutoff);
    }
    return clamp((cutoff - r) / (cutoff - SSR_ROUGH_FLOOR), 0.0, 1.0);
}

// 屏幕边缘衰减（CPU 镜像：post::ssr_edge_fade）。命中点接近屏幕边缘时
// 线性淡出——出屏方向的反射平滑过渡回探针/env，避免边界处突变。
fn ssr_edge_fade(uv: vec2<f32>, margin: f32) -> f32 {
    if (margin <= 0.0) {
        return 1.0;
    }
    let fx = clamp(min(uv.x, 1.0 - uv.x) / margin, 0.0, 1.0);
    let fy = clamp(min(uv.y, 1.0 - uv.y) / margin, 0.0, 1.0);
    return fx * fy;
}

/// 世界坐标 → 上帧屏幕 UV + 线性视距（clip.w 与 scene_depth 副本的存储
/// 口径同源，透视下 clip.w = 该点到相机的前向距离）。UV 约定与 MB/TAA
/// 的重投影一致（blit 的 V 翻转：uv.y = −ndc.y × 0.5 + 0.5）。
fn ssr_project(vp: mat4x4<f32>, world: vec3<f32>) -> vec4<f32> {
    // 返回 (uv.xy, 0, w)；w = clip.w = 线性视距，w ≤ 0（上帧相机背后）
    // 由调用方裁决
    let clip = vp * vec4<f32>(world, 1.0);
    let w = clip.w;
    let uv = vec2<f32>(clip.x, -clip.y) / max(w, 1e-6) * 0.5 + vec2<f32>(0.5);
    return vec4<f32>(uv, 0.0, w);
}

/// 屏幕空间射线步进：世界空间反射射线 `p(t) = world_pos + dir·t` 等分
/// 线性步进，逐样本投影到上帧屏幕，与上帧深度副本（线性视距）比较；
/// 「上帧表面在前、射线点在其后一个厚度以内」判穿越命中，随后在跨越
/// 区间二分细化，末样本取上帧颜色。返回 vec4(rgb 反射色, w = 合成权重
/// ——0 = 未命中 / 出屏 / 衰减殆尽)。
///
/// 确定性：全程无 RNG、无导数依赖（textureLoad + 命中后一次
/// textureSampleLevel 显式 mip）——同输入逐位可复现。
fn ssr_march(world_pos: vec3<f32>, dir: vec3<f32>, roughness: f32) -> vec4<f32> {
    var result = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    let vp = ssr.prev_view_proj;
    let max_dist = max(ssr.extent.x, 1e-3);
    let thickness = max(ssr.extent.y, 1e-3);
    let step_len = max_dist / f32(SSR_STEPS);
    // 首样本从半步长起步：t = 0 是片元自身（深度恰等于存储值，立即自命中）
    var t_front = 0.5 * step_len;
    var hit_uv = vec2<f32>(0.0, 0.0);
    var hit = false;
    for (var i = 0u; i < SSR_STEPS; i = i + 1u) {
        let t = (f32(i) + 0.5) * step_len;
        let s = ssr_project(vp, world_pos + dir * t);
        // 上帧相机背后 / 出屏（含边缘衰减带宽外）⇒ 出屏方向，终止为未命中
        if (s.w <= 1e-4 || s.x < 0.0 || s.x > 1.0 || s.y < 0.0 || s.y > 1.0) {
            break;
        }
        let dims = vec2<f32>(textureDimensions(ssr_prev_depth));
        let px = clamp(vec2<i32>(s.xy * dims), vec2<i32>(0, 0), vec2<i32>(dims) - vec2<i32>(1, 1));
        let stored = textureLoad(ssr_prev_depth, px, 0).r;
        // 穿越判定：射线点已在上帧表面之后、且穿透不超过厚度容差
        if (s.w > stored && s.w - stored <= thickness) {
            // 二分细化 [t_front, t]：t_front 恒在前（stored ≥ 射线点）
            var lo = t_front;
            var hi = t;
            for (var r = 0u; r < SSR_REFINE; r = r + 1u) {
                let mid = 0.5 * (lo + hi);
                let sm = ssr_project(vp, world_pos + dir * mid);
                if (sm.w <= 1e-4) {
                    break;
                }
                let pxm = clamp(
                    vec2<i32>(sm.xy * dims),
                    vec2<i32>(0, 0),
                    vec2<i32>(dims) - vec2<i32>(1, 1),
                );
                let stored_m = textureLoad(ssr_prev_depth, pxm, 0).r;
                if (sm.w > stored_m) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            let t_hit = 0.5 * (lo + hi);
            let s_hit = ssr_project(vp, world_pos + dir * t_hit);
            hit = true;
            hit_uv = s_hit.xy;
            break;
        }
        t_front = t;
    }
    if (hit) {
        let color = textureSampleLevel(ssr_prev_color, env_sampler, hit_uv, 0.0).rgb;
        let fade = ssr_roughness_fade(roughness, ssr.flags.z) * ssr_edge_fade(hit_uv, ssr.flags.w);
        result = vec4<f32>(color, fade);
    }
    return result;
}

// ---- 切线空间（R42）----

// 片元 TBN（切线 → 世界）两路选择：
//
// 路 1「顶点切线」：网格带 TANGENT 属性（渲染器上传独立切线缓冲）。
// 世界空间切线经 Gram-Schmidt 对插值法线正交化后归一化；副切线 =
// cross(N, T) × 手性 w（glTF 约定）。精度最高（逐顶点插值、无导数
// 噪声），首选。
//
// 路 2「屏幕空间推导」：网格无切线（顶点切线 xyz 恒零，渲染器垫零
// 缓冲）。用 world_pos / uv 的屏幕空间偏导解 2×2 线性方程组重建
// T / B（「normal mapping without precomputed tangents」标准做法），
// 手性由解的几何关系自然给出。像素级导数在高频几何上有噪声，精度
// 略逊路 1，但免 CPU 预计算与逐网格 16 B/顶点的切线缓冲。
//
// 选择依据 = 顶点切线是否非零（逐片元 select，无管线变体）。两路的
// TBN 第三列恒取**未归一化**的插值世界法线 in.world_normal：中性法线
// 贴图采样 (0,0,1) 时 `T·0 + B·0 + N·1` 逐位等于该列（x·0 = ±0，
// ±0 + x = x），normalize 的输入与 R41 完全同串位——这是「无纹理
// 逐位一致」的关键，见 material.rs 模块文档。
fn perturbed_normal(in: VertexOutput, n_geo: vec3<f32>) -> vec3<f32> {
    // 路 1：顶点切线
    let t1_raw = in.tangent.xyz - n_geo * dot(n_geo, in.tangent.xyz);
    let t1 = t1_raw / max(length(t1_raw), 1e-8);
    let b1 = cross(n_geo, t1) * sign(in.tangent.w);
    // 路 2：屏幕空间导数
    let dp1 = dpdx(in.world_pos);
    let dp2 = dpdy(in.world_pos);
    let duv1 = dpdx(in.uv);
    let duv2 = dpdy(in.uv);
    let p2perp = cross(dp2, n_geo);
    let p1perp = cross(n_geo, dp1);
    let t2 = p2perp * duv1.x + p1perp * duv2.x;
    let b2 = p2perp * duv1.y + p1perp * duv2.y;
    // 尺度不变归一化；uv 退化（det ≈ 0）时 t2/b2 = 0，1e-12 下限防
    // 除零产生 Inf·0 = NaN
    let invmax = 1.0 / sqrt(max(max(dot(t2, t2), dot(b2, b2)), 1e-12));
    let t2n = t2 * invmax;
    let b2n = b2 * invmax;

    let has_vertex_tangent = dot(in.tangent.xyz, in.tangent.xyz) > 0.0;
    let t = select(t2n, t1, has_vertex_tangent);
    let b = select(b2n, b1, has_vertex_tangent);
    // 切线空间法线解码（*2−1）+ TBN 世界变换
    let ts = textureSample(normal_texture, base_sampler, in.uv).xyz * 2.0 - 1.0;
    return t * ts.x + b * ts.y + n_geo * ts.z;
}

// ---- 前向光源分箱（R78）----
//
// tile 边长（与 Rust 侧 light_bins::LIGHT_TILE_SIZE 同值；WGSL 无
// 「Rust 常量导入」，靠这条注释 + 双路径位等价 GPU 测试看门）。
const BIN_TILE_SIZE: f32 = 32.0;

// 锥光强度因子（CPU 镜像：light_bins::spot_cone_factor，同式同序，
// 两侧必须同步修改）。denom ≤ 1e-6（含 inner ≥ outer 的钳制形态）走
// 硬边；NaN 输入按 0 贡献处理不外溢（软边分支用 `t != t` 自比较判定
// ——WGSL 无 is_nan 内建，与 CPU 侧 is_nan 同义；硬边分支的 >= 比较
// 对 NaN 恒假）。
fn spot_cone_factor(cos_theta: f32, cos_inner: f32, cos_outer: f32) -> f32 {
    let denom = cos_inner - cos_outer;
    if (denom <= 1e-6) {
        return select(0.0, 1.0, cos_theta >= cos_outer);
    }
    let t = (cos_theta - cos_outer) / denom;
    if (t != t) { // NaN 自比较判定（WGSL 无 is_nan 内建；与 CPU 侧 is_nan 同义）
        return 0.0;
    }
    return clamp(t, 0.0, 1.0);
}

// 单盏点光的完整贡献（R78 从 fs_main 的点光循环体**逐式提取**——算术
// 逐式同序，这是双路径位等价的根基：Globals 均摊旧循环与分箱新路径
// 调用同一函数；改动必须与旧循环体逐式核对）。通过函数指针累计进
// `*color` 而非返回增量：保持「color + pbr 再 color + sss」的原加法
// 结合序（返回值方案会把它变成 color + (pbr + sss)，浮点加法不结合，
// SSS 场景会 ±1 LSB——R63 的编译期副作用教训同源）。
fn point_light_contrib(
    color: ptr<function, vec3<f32> >,
    world_pos: vec3<f32>, n: vec3<f32>, v: vec3<f32>, n_geo: vec3<f32>,
    light_pos: vec3<f32>, light_rgb: vec3<f32>, light_range: f32,
    light_shadow: vec4<f32>,
    albedo: vec3<f32>, f0: vec3<f32>, metallic: f32, roughness: f32,
    sss_strength: f32, sss_curvature: f32,
) {
    let to_light = light_pos - world_pos;
    let dist = length(to_light);
    let lp = to_light / max(dist, 1e-4);

    // 点光阴影系数（R61 预算化多盏）：逐灯读 light_shadow.x（0/1 开关）
    // 与 light_shadow.y（cubemap 数组槽位），对对应 cube 单采样（无 PCF）。
    // 采样在 `dist < range` 分支之外无条件执行（textureSampleLevel 带
    // 显式 mip 不依赖导数，掩码模式与 shadow_factor 一致）；开关与槽位
    // 都由 uniform 派生，分支本身均匀。
    // 偏移 v2：bias = clamp(bias_min + 坡度系数 · texel_world(dist) · tanθ,
    // bias_min, MAX)，texel_world(dist) = dist · 2/512（90° 面摊在 512
    // 纹素上）；采样点沿几何法线推移「法线偏移系数 · texel_world ·
    // (1 − NdotL)」——两项的系数由 uniform 给出（对照模式为 0）。
    // 比较口径与 v1 一致：bias 以米存、除以 range 后与归一化距离比较。
    var shadow = 1.0;
    if (light_shadow.x > 0.5) {
        let nol = max(dot(n_geo, lp), 0.0);
        let texel_w = dist * POINT_SHADOW_TEXEL_SCALE;
        let bias_m = clamp(
            globals.point_shadow_params.y
                + globals.point_shadow_params.z * texel_w * slope_tan(nol),
            globals.point_shadow_params.y,
            POINT_SHADOW_BIAS_MAX,
        );
        let n_off = min(
            globals.point_shadow_params.w * texel_w * (1.0 - nol),
            POINT_SHADOW_NORMAL_MAX,
        );
        let dir = (world_pos + n_geo * n_off - light_pos)
            / max(dist, 1e-4);
        let range = max(light_range, 1e-4);
        let stored = textureSampleLevel(point_shadow_cube, shadow_sampler, dir,
                                        i32(light_shadow.y), 0.0).r;
        shadow = select(0.0, 1.0, stored >= dist / range - bias_m / range);
    }

    if (dist < light_range) {
        let t = dist / light_range;
        let window = 1.0 - t * t;
        let atten = (window * window) / max(dist * dist, 0.01);
        let pl = light_rgb * atten * shadow;
        *color = *color + pbr_light(n, v, lp, pl, albedo, f0, metallic, roughness);
        // 预积分 SSS 补项（R63）：点光同样适用——终结线软化是方向
        // 无关的漫反射现象，且 pl 已含距离窗口衰减与 cubemap 阴影，
        // 补项自动继承两者（阴影内 / 有效半径外为 0）。成本 = 每盏
        // 生效光源多一次 64×64 LUT 采样（显式 mip、常驻缓存）。
        if (sss_strength > 0.0) {
            *color = *color + sss_diffuse(n, lp, pl, albedo, metallic,
                                          sss_strength, sss_curvature);
        }
    }
}

// ---- GI 探针三阶 SH（R83）----
//
// 实球谐归一化常数（L0–L2）。**与 Rust 侧 irradiance.rs 的 SH_Y0 / SH_Y1 /
// SH_Y2A / SH_Y2B / SH_Y2C 同一张表逐值一致**（同步契约：改一侧必须改另一侧；
// 看门单测 irradiance.rs::wgsl_sh_constants_match_cpu_table 源码层比对位模式，
// 照 pipeline.rs::skybox_globals_prefix_matches_standard_layout 的先例）。
const SH_Y0: f32 = 0.28209479177387814;
const SH_Y1: f32 = 0.4886025119029199;
const SH_Y2A: f32 = 1.0925484305920792;
const SH_Y2B: f32 = 0.31539156525252005;
const SH_Y2C: f32 = 0.5462742152960396;

// 加权混合的分母下限（CPU 镜像：irradiance::SH_BLEND_EPS）。
const GI_BLEND_EPS: f32 = 1e-8;

// 三阶实球谐基（CPU 镜像：irradiance::sh_basis，**逐式同序**——连
// `SH_Y2A * n.x * n.y` 的左结合顺序也是契约的一部分）。n 必须已归一化。
// 序：L0 = 1；L1 = y, z, x；L2 = xy, yz, 3z²−1, xz, x²−y²。
fn gi_sh_basis(n: vec3<f32>) -> array<f32, 9> {
    var y: array<f32, 9>;
    y[0] = SH_Y0;
    y[1] = SH_Y1 * n.y;
    y[2] = SH_Y1 * n.z;
    y[3] = SH_Y1 * n.x;
    y[4] = SH_Y2A * n.x * n.y;
    y[5] = SH_Y2A * n.y * n.z;
    y[6] = SH_Y2B * (3.0 * n.z * n.z - 1.0);
    y[7] = SH_Y2A * n.x * n.z;
    y[8] = SH_Y2C * (n.x * n.x - n.y * n.y);
    return y;
}

// 单探针 SH 求值 → E/π（CPU 镜像：irradiance::sh_eval，同 k 升序 vec3 累加）。
// 钳 ≥ 0 在调用方 gi_sh_irradiance 的出口做一次（混合后再钳，保留加权平均语义）。
fn gi_sh_eval(p: GiProbe, n: vec3<f32>) -> vec3<f32> {
    let y = gi_sh_basis(n);
    var acc = vec3<f32>(0.0);
    for (var k = 0u; k < 9u; k = k + 1u) {
        acc = acc + p.sh[k].xyz * y[k];
    }
    return acc;
}

// 径向影响权重（CPU 镜像：irradiance::probe_influence_weight）——与点光衰减窗
// **同式同源**（point_light_contrib 的 `window * window`，1 − (d/r)² 的平方）。
// `!(d < r)` 而不是 `d >= r`：NaN 距离同样落进 0 贡献（不外溢）。
fn gi_probe_weight(d: f32, r: f32) -> f32 {
    if (!(d < r)) {
        return 0.0;
    }
    let t = d / r;
    let window = 1.0 - t * t;
    return window * window;
}

// 逐片元 GI 求值（CPU 镜像：irradiance::sh_blend）：固定序循环全部探针 →
// 径向权重 → 加权平均。**固定循环序 = 固定累加序** ⇒ 会话内逐位确定。
// Σw ≤ ε（片元在全部影响球之外）⇒ 回退**最近探针**的 SH：与加权支在球边界
// 处连续（边界上 w = 0、最近探针即该探针），且延续 v1「包外无距离衰减」的
// 语义（§8.40⑦）。最近探针用严格 `<` 裁决 ⇒ 等距时下标小者胜（探针表按
// 实体位升序，全序）。无四面体搜索、无可见性加权（§8.40③ 维持拒绝）。
fn gi_sh_irradiance(world_pos: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let count = u32(gi_params.params.y);
    if (count == 0u) {
        // mode = 1 恒有 ≥ 1 探针（CPU 侧裁决），此支只为参数错配兜底
        return vec3<f32>(0.0);
    }
    var acc = vec3<f32>(0.0);
    var wsum = 0.0;
    var best_d = 3.402823466e+38; // f32::MAX（哨兵：任何有限距离都更小）
    var best = 0u;
    for (var i = 0u; i < count; i = i + 1u) {
        let p = gi_probes[i];
        let d = distance(world_pos, p.pos_radius.xyz);
        let w = gi_probe_weight(d, p.pos_radius.w);
        if (w > 0.0) {
            acc = acc + gi_sh_eval(p, n) * w;
            wsum = wsum + w;
        }
        if (d < best_d) {
            best_d = d;
            best = i;
        }
    }
    var gi = acc / max(wsum, GI_BLEND_EPS);
    if (wsum <= GI_BLEND_EPS) {
        gi = gi_sh_eval(gi_probes[best], n);
    }
    return max(gi, vec3<f32>(0.0));
}

// ---- DDGI 深度感知混合（R88）----
//
// 每探针的深度表存 **cube 面参数化**的两阶矩（6 面 × 8² 纹素 × (μ, σ²)），
// 与 SH 投影同批射线 ⇒ 零额外光线成本。索引式（CPU 侧
// ddgi::DDGI_DEPTH_TEXELS 同值，看门单测 ddgi.rs::wgsl_ddgi_source_matches_cpu_contract）：
//
//   索引 = (probe × DDGI_DEPTH_TEXELS + texel) × 2 + moment
//   texel = face × grid² + row × grid + col        （grid = gi_params.params.z）
//   moment: 0 = μ（平均可见深度，米）、1 = σ²（方差，米²，CPU 侧已钳下限）
//
// 用 `array<f32>` **标量数组**而不是 vec4 打包：WGSL 无动态 swizzle ⇒ vec4
// 取值要 3 次 select，正是 irradiance.rs 里 ShProbeRaw 偏离声明记录过的坑。

// 每探针的深度纹素数（6 面 × 8²；= Rust 侧 ddgi::DDGI_DEPTH_TEXELS）
const DDGI_DEPTH_TEXELS: u32 = 384u;

// 方向 → cube 面下标（CPU 镜像：ddgi::ddgi_depth_face）。主轴 = |x|,|y|,|z|
// 最大者，**并列取 PROBE_AXES 序低者**（严格 > 扫描 ⇒ x → y → z 优先）；
// 符号决定 ±（comp < 0 ⇒ 负面，NaN 落正面支——与 CPU 侧同式同极性）。
fn ddgi_depth_face(dir: vec3<f32>) -> u32 {
    let ax = abs(dir.x);
    let ay = abs(dir.y);
    let az = abs(dir.z);
    var axis = 0u;
    var m = ax;
    if (ay > m) {
        axis = 1u;
        m = ay;
    }
    if (az > m) {
        axis = 2u;
        m = az;
    }
    var comp = dir.x;
    if (axis == 1u) {
        comp = dir.y;
    }
    if (axis == 2u) {
        comp = dir.z;
    }
    if (comp < 0.0) {
        return axis * 2u + 1u;
    }
    return axis * 2u;
}

// 方向 → 深度纹素下标（CPU 镜像：ddgi::ddgi_depth_texel，**同式同序**）。
// 局部轴约定同 irradiance.rs 的 cube_face_direction：±X 面 (u→y, v→z)、
// ±Y 面 (u→x, v→z)、±Z 面 (u→x, v→y)。只用分量的**比值与符号** ⇒ 传单位
// 方向或 delta/dist 同结果。nearest-texel，无双线性插值（诚实限制）。
// NaN ⇒ row/col 落 0 支（`floor(c) >= 0.0` 对 NaN 恒假，与 CPU 的
// `!(cell >= 0.0)` 同义），调用方已用 dist > 0 把它挡在外面。
fn ddgi_depth_texel(dir: vec3<f32>, grid: u32) -> u32 {
    let face = ddgi_depth_face(dir);
    let axis = face / 2u;
    var main = abs(dir.x);
    if (axis == 1u) {
        main = abs(dir.y);
    }
    if (axis == 2u) {
        main = abs(dir.z);
    }
    var au = dir.y;
    var av = dir.z;
    if (face >= 2u && face <= 3u) {
        au = dir.x;
        av = dir.z;
    }
    if (face >= 4u) {
        au = dir.x;
        av = dir.y;
    }
    let g = f32(grid);
    let cu = floor((au / main + 1.0) * 0.5 * g);
    let cv = floor((av / main + 1.0) * 0.5 * g);
    var col = 0u;
    var row = 0u;
    if (cu >= 0.0) {
        col = u32(min(cu, g - 1.0));
    }
    if (cv >= 0.0) {
        row = u32(min(cv, g - 1.0));
    }
    return face * grid * grid + row * grid + col;
}

// chebyshev 单侧上界权重（CPU 镜像：ddgi::ddgi_depth_weight_at +
// ddgi::ddgi_depth_weight，含两道取值中性的守卫）。
//
//   dist ≤ μ ⇒ 1                                 （着色点在探针可见范围内）
//   dist > μ ⇒ σ² / (σ² + (dist − μ)²)           （「真实深度 ≥ dist」的概率上界）
//
// 墙后片元的 dist 远超该方向的 μ ⇒ 权重趋 0，这就是 §8.52① 串味的解药。
// σ² 已在 CPU 侧钳过下限（ddgi::DDGI_SIGMA_REL）⇒ 着色器零减法、零钳位。
// enabled = 0 是对照模式（Renderer::set_ddgi_depth_weight_enabled），恒 1。
fn ddgi_depth_weight(probe: u32, delta: vec3<f32>, dist: f32, grid: u32,
                     enabled: f32) -> f32 {
    if (!(enabled > 0.5)) {
        return 1.0;
    }
    // dist = 0 ⇒ 着色点即探针位置（delta 退化成零向量、归一化得 NaN），
    // 「无遮挡证据」是唯一正确语义；同时挡住 NaN 进查表（径向窗对 NaN
    // 距离已给 0 ⇒ 乘积恒 0，本支取值不影响结果）
    if (!(dist > 0.0)) {
        return 1.0;
    }
    let texel = ddgi_depth_texel(delta / dist, grid);
    let base = (probe * DDGI_DEPTH_TEXELS + texel) * 2u;
    let mu = gi_depth[base];
    // !(dist > mu) 而不是 dist <= mu：NaN 同样落进「无遮挡证据」支（保守：不抑制）
    if (!(dist > mu)) {
        return 1.0;
    }
    let sigma2 = gi_depth[base + 1u];
    let excess = dist - mu;
    return sigma2 / (sigma2 + excess * excess);
}

// 逐片元 DDGI 求值（CPU 镜像：ddgi::ddgi_blend）：固定序循环全部探针 →
// **径向窗 × chebyshev 深度项**双剪枝 → 加权平均。固定循环序 = 固定累加序
// ⇒ 会话内逐位确定。径向窗 gi_probe_weight 与 SH 求值 gi_sh_eval 都是 R83
// 的原函数**零改动**（只新增深度项这一层）。
//
// 返回 vec4：xyz = E/π（钳 ≥ 0）、w = 有效标志（1 = 有有效探针）。
//
// **兜底语义对 R83 的显式偏离**：mode 1 的 `Σw ≤ ε ⇒ 回退最近探针` 在
// mode 2 **改为回退 env_irradiance**（w = 0 由调用方 select）。理由：回退
// 最近探针会**绕过深度项** ⇒ 墙后片元吃到墙另一侧探针的辐照 = 正是本轮要
// 消灭的漏光。
fn ddgi_irradiance(world_pos: vec3<f32>, n: vec3<f32>) -> vec4<f32> {
    let count = u32(gi_params.params.y);
    if (count == 0u) {
        // mode = 2 恒有 ≥ 1 探针（CPU 侧裁决），此支只为参数错配兜底
        return vec4<f32>(0.0);
    }
    let grid = u32(gi_params.params.z);
    let enabled = gi_params.params.w;
    var acc = vec3<f32>(0.0);
    var wsum = 0.0;
    for (var i = 0u; i < count; i = i + 1u) {
        let p = gi_probes[i];
        let d = distance(world_pos, p.pos_radius.xyz);
        // 第一道剪枝：R83 的径向窗（1 − (d/r)²)²，d ≥ r ⇒ 0
        let radial = gi_probe_weight(d, p.pos_radius.w);
        if (radial > 0.0) {
            // 第二道剪枝：chebyshev 深度项（墙后 ⇒ 趋 0）
            let delta = world_pos - p.pos_radius.xyz;
            let w = radial * ddgi_depth_weight(i, delta, d, grid, enabled);
            if (w > 0.0) {
                acc = acc + gi_sh_eval(p, n) * w;
                wsum = wsum + w;
            }
        }
    }
    if (wsum > GI_BLEND_EPS) {
        return vec4<f32>(max(acc / wsum, vec3<f32>(0.0)), 1.0);
    }
    return vec4<f32>(0.0);
}

@fragment
// 注：VertexOutput 的 @builtin(position)（字段名 clip_position 沿用顶点
// 侧语义）作为片元入口输入时，WGSL 规定其值为**帧缓冲像素坐标**——
// 下文的 in.clip_position.xy 即 frag coord。
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let base = material.base_color * textureSample(base_texture, base_sampler, in.uv);
    let albedo = base.rgb;
    // ORM 打包（线性采样）：R = AO、G = 粗糙度、B = 金属度；标量因子
    // 作倍率相乘（glTF factor × texel 语义）。中性 ORM = (1,1,1) 时
    // x·1.0 = x 逐位精确，退化为 R41 的标量路径。
    let orm = textureSample(orm_texture, base_sampler, in.uv);
    let metallic = clamp(material.surface.x * orm.b, 0.0, 1.0);
    let roughness = clamp(material.surface.y * orm.g, 0.045, 1.0);
    let ao = orm.r;
    let f0 = mix(vec3<f32>(material.surface.z), albedo, metallic);

    // 光照法线：几何法线 + 法线贴图扰动。n_geo 未归一化（见
    // perturbed_normal 的逐位一致说明），归一化只在此处做一次，
    // 与 R41 的 normalize(in.world_normal) 同构。
    let n = normalize(perturbed_normal(in, in.world_normal));
    // 阴影偏移 v2 用**几何法线**（而非扰动后的着色法线）：深度误差由
    // 光栅化几何决定，用逐纹素高频抖动的法线贴图法线只会给偏移量注入
    // 噪声（凹凸面出现漏光条纹）。归一化一次供两处阴影共用。
    let n_geo = normalize(in.world_normal);
    let v = normalize(globals.eye_pos.xyz - in.world_pos);

    // 方向光（受级联阴影遮挡）
    let l = normalize(globals.light_dir.xyz);
    let sun = shadow_factor(in.world_pos, n_geo, l);
    let radiance = globals.light_color.rgb * sun;
    var color = pbr_light(n, v, l, radiance, albedo, f0, metallic, roughness);
    // 预积分 SSS 补项（R63）：radiance 已含 CSM 阴影因子 ⇒ 阴影内补项为 0。
    if (material.sss.x > 0.0) {
        color = color + sss_diffuse(n, l, radiance, albedo, metallic,
                                    material.sss.x, material.sss.y);
    }

    // 点光：物理平方衰减 + range 平滑窗口；投影灯（预算内多盏）各自受
    // 自己的 cubemap 阴影调制。循环体已逐式提取为 point_light_contrib
    // （R78）——Globals 均摊旧循环与分箱新路径共用同一函数，算术逐式
    // 同序。**均匀门**：mode = 1（分箱生效）时本循环整体跳过（灯表的
    // 前段就是这 8 盏，重复执行会把它们计两次）——uniform 条件，mode = 0
    // （缺席/关闭/捕获/RT）时恒真，执行路径与 R77 逐位一致。
    let count = u32(globals.point_count.x);
    if (bins_params.counts.x < 0.5) {
        for (var i = 0u; i < count; i = i + 1u) {
            let light = globals.point_lights[i];
            point_light_contrib(&color, in.world_pos, n, v, n_geo,
                                light.position.xyz, light.color.rgb, light.color.w,
                                light.shadow,
                                albedo, f0, metallic, roughness,
                                material.sss.x, material.sss.y);
        }
    }

    // 分箱路径（R78）：mode = 1 时片元按 @builtin(position) 定位 tile，
    // 遍历本 tile 的灯表下标（升序 = 灯数组序 = 旧路径求和序；tile 覆盖
    // 是 range 球投影矩形的保守超集 ⇒ 不漏灯，多覆盖的灯由 dist < range
    // 早退兜底）。点光与旧路径调同一贡献函数；锥光把 spot_cone_factor
    // 乘在灯色（radiance）上——贡献的每一项（漫反射/镜面/SSS 补项）对
    // radiance 线性，与「乘在最终贡献」数学一致，且贡献函数零改动。
    // mode = 0 时本分支死码不执行 ⇒ 既有像素逐位不变（哑缓冲 counts = 0）。
    if (bins_params.counts.x >= 0.5) {
        let tiles_x = u32(bins_params.screen.z);
        let tiles_y = u32(bins_params.screen.w);
        // 片元 tile（@builtin(position) 是帧缓冲像素坐标，.5 结尾；u32
        // 截断即 floor，min 钳到合法 tile——右/下边缘的精确触界防御）
        let tx = min(u32(in.clip_position.x / BIN_TILE_SIZE), tiles_x - 1u);
        let ty = min(u32(in.clip_position.y / BIN_TILE_SIZE), tiles_y - 1u);
        let tile_id = ty * tiles_x + tx;
        let start = bins_tiles[tile_id * 2u];
        let num = bins_tiles[tile_id * 2u + 1u];
        // 灯下标展平区紧跟 ranges 区（2·tile 数个 u32）之后
        let indices_base = 2u * tiles_x * tiles_y;
        for (var k = 0u; k < num; k = k + 1u) {
            let light = binned_lights[bins_tiles[indices_base + start + k]];
            var light_rgb = light.color_range.rgb;
            if (light.pos_type.w > 0.5) {
                let cos_theta = dot(normalize(in.world_pos - light.pos_type.xyz),
                                    light.cone.xyz);
                light_rgb = light_rgb
                    * spot_cone_factor(cos_theta, light.cone.w, light.outer.x);
            }
            point_light_contrib(&color, in.world_pos, n, v, n_geo,
                                light.pos_type.xyz, light_rgb, light.color_range.w,
                                light.shadow,
                                albedo, f0, metallic, roughness,
                                material.sss.x, material.sss.y);
        }
    }

    // 环境光（IBL 分割求和）：漫反射采样辐照度贴图；镜面按粗糙度取预滤波
    // 环境 mip，乘 BRDF LUT 的 (缩放, 偏移)。intensity 可整帧关闭 IBL。
    // AO（orm.r）乘环境项：IBL 与美术补光都被遮蔽削弱，直接光不乘
    // （物理上 AO 描述小尺度互遮蔽的环境光损失）。中性 ORM 时 ao = 1.0，
    // x·1.0 = x 逐位精确 = R41 行为。
    // ---- 光照探针（R64）：逐实体插值的间接漫反射辐照，取代全局 env 采样 ----
    // 载荷存的就是 E/π（与 irradiance_map 同口径）⇒ 着色器零换算，只是把
    // 采样值整项换掉。select 是**纯值选择、不做任何算术** ⇒ 标志位 0 时返回
    // 值与引入本槽位前是**同一串位**（R52c 先例：把逐位不变交给语言语义，
    // 而不是浮点恒等式）。探针缺席 / 空集 / 实体挂光照贴图（CPU 侧直接不发
    // 标志位）⇒ 走原全局 env 路径，全部既有 FNV 基线原值通过。
    let env_irradiance = textureSampleLevel(irradiance_map, env_sampler, n, 0.0).rgb;
    // ---- GI 探针 SH（R83）/ DDGI（R88）：mode 均匀门三支（R78 分箱同款形态）----
    // mode = 0（探针集缺席 / 无 SH / 捕获 / RT）⇒ 走 R64 的 loc14 逐实体
    // 载荷支，**执行的语句与 R82 逐条相同**（同一个 select、同一串位）；
    // mode = 1 ⇒ GI 探针项由 R83 的逐片元 SH 求值替代（空间梯度 + 方向性），
    // **语句与 R83 逐条相同**；
    // mode = 2（R88）⇒ 逐片元 SH 求值再乘 chebyshev 深度项（遮挡感知），
    // Σw ≤ ε 时**回退 env_irradiance 而不是最近探针**（回退最近探针会绕过
    // 深度项 ⇒ 把要消灭的漏光从后门放回来，见 ddgi_irradiance 头注释）。
    // 三支都只替换 `in.probe.w > 0.5` 的实体：天空 irradiance_map 兜底与
    // R52c 的 lightmap `select` 混合语义**零触碰**（正交，见 §4.39）。
    var irradiance = env_irradiance;
    if (gi_params.params.x < 0.5) {
        irradiance = select(env_irradiance, in.probe.rgb, in.probe.w > 0.5);
    } else if (gi_params.params.x < 1.5) {
        irradiance = select(env_irradiance,
                            gi_sh_irradiance(in.world_pos, n), in.probe.w > 0.5);
    } else {
        let ddgi = ddgi_irradiance(in.world_pos, n);
        irradiance = select(env_irradiance, ddgi.xyz, in.probe.w > 0.5 && ddgi.w > 0.5);
    }
    let nov = max(dot(n, v), 0.0);
    let f_amb = fresnel_schlick_roughness(nov, f0, roughness);
    let kd_amb = (vec3<f32>(1.0) - f_amb) * (1.0 - metallic);
    // 镜面反射方向（未修正）：探针的视差修正以它为入射射线方向
    let spec_dir = reflect(-v, n);
    let env_spec = textureSampleLevel(env_map, env_sampler, spec_dir,
                                      roughness * globals.env_params.x).rgb;
    let brdf = textureSample(brdf_lut, env_sampler, vec2<f32>(nov, roughness)).rg;
    // ---- 反射探针（R62a）：域内以探针 cubemap 取代全局 env 的镜面项 ----
    // 逐探针算影响权重，取**严格更大**者 ⇒ 权重并列时先出现者胜出；uniform
    // 数组由 CPU 按实体位模式升序排列 ⇒ 并列裁决 = 实体位模式小者（全序、
    // 与查询 / 插入顺序无关）。拒绝清单：不做多探针加权混合（见 §8.37）。
    let probe_count = u32(probe_params.count.x);
    var best_w = 0.0;
    var best_slot = 0;
    var best_center = vec3<f32>(0.0, 0.0, 0.0);
    var best_extents = vec3<f32>(1.0, 1.0, 1.0);
    for (var i = 0u; i < probe_count; i = i + 1u) {
        let p = probe_params.probes[i];
        let w = probe_weight(in.world_pos - p.center_falloff.xyz,
                             p.extents_slot.xyz, p.center_falloff.w);
        if (w > best_w) {
            best_w = w;
            best_slot = i32(p.extents_slot.w);
            best_center = p.center_falloff.xyz;
            best_extents = p.extents_slot.xyz;
        }
    }
    // 域外 / 无探针（count = 0）：spec 保持 env_spec 的**同一串位**——`select`
    // 是纯值选择、分支不改变 env_spec 的计算 ⇒ 缺省路径与 R61 逐位一致
    // （兼容硬约束的实现根据，GPU 全帧基线锁见 tests/probe_render.rs）。
    var spec = env_spec;
    if (best_w > 0.0) {
        // 视差修正：反射射线与盒求交，按交点重定向后再采样（经典 box
        // projection）。粗糙度 → mip 与 env 走**同一表达式**（同一套预滤波
        // 链、同一 mip 语义），故粗糙度对比只取决于探针自身的内容。
        // count.w = 视差修正开关（1 = 生产路径；0 = 对照模式，见
        // Renderer::set_legacy_probe_parallax，仅供验收图与归因测试）。
        var dir = spec_dir;
        if (probe_params.count.w > 0.5) {
            dir = box_project(spec_dir, in.world_pos - best_center, best_extents);
        }
        spec = textureSampleLevel(probe_cube, env_sampler, dir, best_slot,
                                  roughness * globals.env_params.x).rgb;
    }
    // 镜面反射项的 BRDF 权重拆出为独立变量（与原行内表达式同串位——
    // 同操作数同顺序，SSR 关闭路径逐位一致）：SSR 命中时按合成公式混入
    // 上帧反射色（裁决 4，见 ssr_march 头注释）。
    var spec_refl_w = spec * (f_amb * brdf.x + brdf.y);
    if (ssr.flags.x > 0.5 && roughness < ssr.flags.z) {
        let hit = ssr_march(in.world_pos, spec_dir, roughness);
        if (hit.w > 0.0) {
            spec_refl_w = mix(spec_refl_w, hit.rgb * f_amb, hit.w);
        }
    }
    let ibl_env = (kd_amb * albedo * irradiance + spec_refl_w)
            * globals.env_params.y * ao;

    // 光照贴图间接项（R52c）：图集存**辐照度** E（天空 AO + 一次反弹
    // 色溢，不含烘焙直接光），Lambert 出射 = albedo/π × E ⇒ 乘 INV_PI 后
    // 与 IBL 的 `irradiance` 同口径（后者存的就是 E/π，见 environment.wgsl
    // 的 fs_irradiance），两项可直接互换。**不再乘 ao**：遮蔽已烘进天空项，
    // 再乘一次是双重 AO（角落会黑过头）。
    let lm = textureSample(lightmap_texture, lightmap_sampler, in.uv2).rgb;
    let ibl_lightmap = kd_amb * albedo * (lm * INV_PI);
    // 掩码 = 材质常量（material.surface.w，0/1，随 (材质, 图集) 绑定组
    // 缓存）：`select` 是纯值选择、不做任何算术 ⇒ mask = 0 时 `ibl` 与
    // 引入光照贴图前是**同一串位**（`ibl_env × 1.0 + x × 0.0` 那种写法
    // 虽然数学恒等，但把逐位不变交给浮点恒等式不如交给语言语义）。这是
    // 「未挂光照贴图的场景渲染逐位不变」硬约束的实现根据，由
    // tests/lightmap_render.rs 的 FNV-1a 全帧基线锁看门。
    let ibl = select(ibl_env, ibl_lightmap, material.surface.w > 0.5);
    // 光照贴图协同（R62a 闭合 §8.31④）：挂图集的实体其间接**漫反射**走
    // 光照贴图、**镜面**走探针——补充项只在「有图集 且 有生效探针」时叠加。
    // 无探针时这一项恒不执行 ⇒ 光照贴图实体仍然没有环境镜面（R52c 语义
    // 与逐位兼容不变）；无图集时镜面已含在 ibl_env 里，不重复叠加。
    // 乘 env 强度与 ao 的理由与 IBL 镜面项一致（探针取代的就是该项）。
    var ibl_final = ibl;
    if (material.surface.w > 0.5 && best_w > 0.0) {
        // spec_refl_w 已含 SSR 混合（如有）——光照贴图实体的环境镜面与
        // IBL 实体走同一份镜面项
        ibl_final = ibl + spec_refl_w * globals.env_params.y * ao;
    }
    // GTAO 屏幕空间环境光遮蔽（R77）：无条件采样可见度图（缺席/关闭 =
    // 常驻 1×1 哑纹理、纹素恰为 1.0 ⇒ `× 1.0` 逐位精确——零分支由纹素
    // 值承担，与探针 select 零算术殊途同归）。乘点与 ORM 的 ao 同靶：
    // 间接光（ibl_final = IBL / 光照贴图 / 探针镜面）与常量环境项；
    // 直接光（方向光 / 点光）不乘——AO 描述小尺度互遮蔽的**环境**光
    // 损失，是 R41「直接光不乘 ao」语义的屏幕空间扩展。
    // 屏幕坐标 = 帧缓冲像素坐标 / 屏幕尺寸。尺寸取可见度图纹素数 ×2
    // （半分辨率）——不扩 Globals（skybox 并行布局零触碰）；AO 关闭时
    // 绑定的是 1×1 哑纹理，坐标失去意义但采样值恒 1.0，× 1.0 逐位精确。
    // 罕见奇数分辨率下右/下边缘有 ≤1 像素的口径差——AO 是低频量，且
    // 采样为 ClampToEdge，不可见（诚实限制见 §8）。
    // 为什么不用顶点 varying：screen uv = clip.x/w 是投影量，地面/墙等
    // 大三角形常有顶点在相机背后，近平面裁剪后新顶点的 x/w 爆炸，插值
    // 出越界坐标把可见度图采样扭曲成大片假遮蔽（R77 实测归因）；
    // @builtin(position) 是帧缓冲像素坐标（纯仿射），逐片元精确。
    let ao_dims = vec2<f32>(textureDimensions(ao_visibility_tex)) * 2.0;
    let ao_ss = textureSampleLevel(ao_visibility_tex, env_sampler,
                                   in.clip_position.xy / ao_dims, 0.0).r;
    // 美术补光：方向光资源上的常量环境项（与 IBL 叠加）
    let f_env = mix(vec3<f32>(material.surface.z), albedo, 0.5);
    color = color + ibl_final * ao_ss
          + mix(albedo, f_env, metallic) * globals.ambient.rgb * ao * ao_ss
          + material.emissive.rgb;

    return vec4<f32>(color, base.a);
}