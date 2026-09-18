//! 渲染器核心：批处理收集、GPU 资源懒上传、帧编码与提交。
//!
//! 数据流：
//! ```text
//! World 资源（MeshStore / MaterialStore / TextureStore，CPU 目录）
//!     -> Renderer 内 GPU 缓存（HashMap<句柄, GPU 资源>，按需懒上传；
//!        纹理按 BC1 决策树压缩上传，见 texture_bcn）
//!     -> 查询 (Transform, MaterialHandle, MeshHandle|LodMesh|LodMesh3) 收集实例
//!        （LodMesh 按实体-相机距离选 high/low，见 mesh.rs；LodMesh3 三级
//!        同口径，地形 chunk 用，见 terrain.rs）
//!     -> 按 (网格, 材质) 排序合并为批次 -> 一个批次一个 draw call
//!     -> 另查 (GlobalTransform, Decal) 收集贴花实例，按纹理分批在独立
//!        pass 绘制（蒙皮之后、透明之前，见 decal.rs）
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use glam::{Mat4, Vec3};
use iris_core::prelude::{Entity, GlobalTransform};
use wgpu::util::DeviceExt;

use crate::SAMPLE_COUNT;
use crate::camera::{ActiveCamera, Camera, view_from_global};
use crate::context::Gpu;
use crate::decal::{Decal, DecalInstanceRaw, PROXY_INDEX_COUNT, PROXY_INDICES, PROXY_POSITIONS};
use crate::frustum::Frustum;
use crate::instance::InstanceRaw;
use crate::irradiance::{IrradianceProbeSet, MAX_IRRADIANCE_PROBES, ShProbeRaw};
use crate::light::{
    DirectionalLight, MAX_POINT_LIGHTS, POINT_SHADOW_BIAS_LEGACY, POINT_SHADOW_BIAS_MIN,
    POINT_SHADOW_BUDGET, POINT_SHADOW_NORMAL_SCALE, POINT_SHADOW_SLOPE_SCALE, PointLight,
    PointShadowCandidate, SHADOW_BIAS_LEGACY, SHADOW_BIAS_MIN, SpotLight, cascade_fit_params,
    cascade_splits, select_point_shadow_casters,
};
use crate::light_bins::{
    LIGHT_TILE_SIZE, LightBinInput, LightBinningSettings, MAX_BINNED_LIGHTS, bin_lights,
    light_bins_enabled,
};
use crate::lightmap::{LightmapBinding, LightmapHandle, LightmapStore, NO_LIGHTMAP};
use crate::material::{AlphaMode, MaterialHandle, MaterialStore};
use crate::mesh::{LodMesh, LodSettings, MeshHandle, MeshStore, upload_mesh};
use crate::occlusion::{
    AABB_CORNERS, AABB_INDEX_COUNT, AABB_INDICES, AabbInstanceRaw, OcclusionCulling, world_aabb,
};
use crate::particles::{ParticleBlend, ParticleEmitter, ParticleInstance};
use crate::pipeline::{SHADOW_CASCADES, create_pipeline};
use crate::probe::{
    MAX_REFLECTION_PROBES, PROBE_CAPTURE_FAR, PROBE_CAPTURE_NEAR, PROBE_MIP_COUNT, ProbeCandidate,
    ProbeParamsRaw, ProbeStore, ReflectionProbe, ReflectionProbeRaw, distance_to_box,
    fit_probe_shadow, select_active_probes,
};
use crate::rendertarget::{RenderTargetHandle, RenderTargetStore, RendersTo};
use crate::skin::Skin;
use crate::texture::{TextureHandle, TextureStore};
use crate::visibility::{Visibility, hidden_by_ancestor};

/// 与 WGSL `PointLight` 结构对齐（48 字节，POD）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PointLightRaw {
    position: [f32; 4],
    color: [f32; 4], // rgb × intensity, w = range
    /// x: 是否投射 cubemap 阴影（1/0），y: cubemap 数组槽位（0..预算-1），
    /// z/w: 保留（R61 起逐灯携带阴影参数——多盏投影必须逐灯独立）。
    shadow: [f32; 4],
}

/// 与 WGSL `Globals` 结构对齐（640 字节，POD）。
///
/// R29 起方向光阴影为 CSM：单一 `light_view_proj` 字段移除（每级联矩阵
/// 在独立的 [`CsmParamsRaw`] 缓冲），576B → 512B；R61 起 `PointLight`
/// 增加逐灯阴影参数 vec4（多盏预算化投影），512B → 640B。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GlobalsRaw {
    view_proj: [[f32; 4]; 4],
    eye_pos: [f32; 4],
    light_dir: [f32; 4],
    light_color: [f32; 4],
    ambient: [f32; 4],
    point_count: [f32; 4],
    point_lights: [PointLightRaw; MAX_POINT_LIGHTS],
    shadow_params: [f32; 4],      // texel_size, bias 下限, strength, enabled
    env_params: [f32; 4],         // 预滤波最大 mip 级, IBL 强度, 保留
    inv_view_proj: [[f32; 4]; 4], // 天空盒射线重建用
    // x: 是否有投影灯（全局掩码，逐灯开关看 point_lights[i].shadow.x）,
    // y: 点光 bias 下限（米）, z: 坡度缩放系数, w: 法线偏移系数（R61）
    point_shadow_params: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<GlobalsRaw>() == 640,
    "Globals 必须与 WGSL 布局一致"
);

/// 与 WGSL `BinnedLight` 结构对齐（80 字节 = 5×vec4，POD）：分箱灯表
/// （[`MAX_BINNED_LIGHTS`] 条定长 storage 数组）的条目。
///
/// 前 3 个 vec4 与 [`PointLightRaw`] 逐字段同布局（位置 / 颜色×强度×range /
/// 阴影参数）——灯表前段（前 8 盏点光）与 `Globals.point_lights` 逐字段
/// 一致，是双路径位等价的数据根据；后 2 个 vec4 携带锥光形状（R78）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BinnedLightRaw {
    /// xyz: 世界位置，w = 0.0 点光 / 1.0 锥光。
    pos_type: [f32; 4],
    /// rgb × intensity, w = range（与 [`PointLightRaw::color`] 同布局）。
    color_range: [f32; 4],
    /// 逐字复制 [`PointLightRaw::shadow`] 语义：x 投影开关 y cubemap 槽位；
    /// 灯表前段与 Globals 逐字段一致，SpotLight 恒 0（v1 不投影阴影）。
    shadow: [f32; 4],
    /// xyz: 单位化锥轴方向，w = cos(inner_cutoff)。
    cone: [f32; 4],
    /// x = cos(outer_cutoff)，yzw 保留。
    outer: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<BinnedLightRaw>() == 80,
    "BinnedLight 必须与 WGSL 布局一致（5×vec4 = 80 B）"
);

/// 与 WGSL `LightBinsParams` 结构对齐（32 字节，POD）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LightBinsParamsRaw {
    /// 宽、高、tiles_x、tiles_y（像素与 tile 数，片元 tile 定位用）。
    screen: [f32; 4],
    /// x = mode（0 = 关——着色器分箱分支死码；1 = 灯表生效），
    /// y = 灯总数，z = index 总数，w = 保留。
    counts: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<LightBinsParamsRaw>() == 32,
    "LightBinsParams 必须与 WGSL 布局一致（2×vec4 = 32 B）"
);

/// 与 WGSL `GiShParams` 结构对齐（16 字节，POD）——GI 探针三阶 SH（R83）+
/// DDGI（R88）的小参数块。刻意与探针表（binding 18）分开：mode 是**逐帧
/// 裁决**的均匀门，探针表是烘焙 / 重照明产物（mode ≥ 1 帧才写），分开两个
/// 缓冲让「关闭帧零 write_buffer」与 R78 分箱同纪律。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GiShParamsRaw {
    /// x = mode（0 = 关——逐片元分支死码，loc14 逐实体路径逐位不变；
    /// 1 = R83 逐片元 SH 求值；2 = R88 DDGI 深度感知混合），
    /// y = 探针数（着色器循环上界），
    /// z = [`DDGI_DEPTH_GRID`](crate::ddgi::DDGI_DEPTH_GRID)（R88，= 8；
    /// mode ≤ 1 时不消费，恒写 0），
    /// w = DDGI 深度项开关（R88，1 = 生产路径；0 = 对照模式，见
    /// [`Renderer::set_ddgi_depth_weight_enabled`]）。
    ///
    /// **偏离声明**：设计稿 §7.1 写的是「w = 保留 0」（理由是 σ² 已在 CPU
    /// 钳过、着色器不需要 `DDGI_SIGMA_REL`）——那条理由成立，但 §7.4 要求的
    /// 对照模式开关必须有落脚处，`w` 是唯一空位（照 `probe_params.count.w`
    /// 承载视差开关的先例）。
    params: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<GiShParamsRaw>() == 16,
    "GiShParams 必须与 WGSL 布局一致（1×vec4 = 16 B）"
);

/// 与 WGSL `CsmParams` 对齐（256 字节，POD）：方向光级联阴影参数
/// （R29 CSM + R61 坡度缩放 bias / 法线偏移的逐级联世界尺度）。
/// 独立缓冲 + 独立绑定槽（globals 组 binding 8），不扩 Globals——级联
/// 矩阵只有主 pass 片元消费，阴影 pass 用每级联 64B 小缓冲。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CsmParamsRaw {
    /// 每级联的光空间视图投影矩阵（cascade_fit 逐帧拟合）。
    view_proj: [[[f32; 4]; 4]; SHADOW_CASCADES],
    /// xyz: 相机前向单位向量（片元 view 深度 = dot(world−eye, forward)），
    /// w: 保留。
    cam_forward: [f32; 4],
    /// x/y/z: 级联分割点（沿相机前向的距离，递增），w: 保留。
    splits: [f32; 4],
    /// x/y/z: 每级联一个纹素的世界边长（米，[`CascadeFit::texel_world`]），
    /// w: 保留。法线偏移量 = 该值 × 系数 × (1 − NdotL)。
    cascade_texel: [f32; 4],
    /// x/y/z: 每级联的 `texel_world / depth_window`（NDC 深度误差随 tanθ
    /// 的斜率，[`CascadeFit`]），w: 保留。bias 的坡度项 = 该值 × 系数 × tanθ。
    /// 对照模式（[`Renderer::set_legacy_shadow_bias`]）把两者置零 ⇒
    /// 坡度项与法线偏移同时退化为 0，只剩常数 bias。
    cascade_slope_k: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<CsmParamsRaw>() == 256,
    "CsmParams 必须与 WGSL 布局一致（3×64B 矩阵 + 4×16B 向量）"
);

/// 与 WGSL `CascadeParams` 对齐（64 字节，POD）：阴影 pass 的每级联
/// 光空间矩阵。一级联一缓冲（同 submit 内对同一缓冲的多次 write_buffer
/// 会在执行前合并，泛光/点光参数的既有教训）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CascadeParamsRaw {
    view_proj: [[f32; 4]; 4],
}

const _: () = assert!(
    std::mem::size_of::<CascadeParamsRaw>() == 64,
    "CascadeParams 必须与 WGSL 布局一致"
);

/// 与 WGSL `PointFaceParams` 对齐（80 字节，POD）：点光阴影每面独立小 uniform。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PointFaceParamsRaw {
    view_proj: [[f32; 4]; 4],
    light_pos_range: [f32; 4], // xyz: 灯世界位置, w: range
}

const _: () = assert!(
    std::mem::size_of::<PointFaceParamsRaw>() == 80,
    "PointFaceParams 必须与 WGSL 布局一致"
);

/// 阴影暗部保留的光照比例（0 = 全黑阴影，1 = 无阴影）。
const SHADOW_STRENGTH: f32 = 0.45;

/// 与 WGSL `MaterialParams` 结构对齐（64 字节，POD）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MaterialRaw {
    base_color: [f32; 4],
    emissive: [f32; 4],
    // metallic, roughness, reflectance(F0), 光照贴图掩码（R52c，0/1）
    surface: [f32; 4],
    // x: 预积分 SSS 强度（R63，0 = 关闭——着色器分支整段跳过），
    // y: 曲率包裹参数 t ∈ [0, 1]（CPU 由 sss::wrap_param 换算好，
    //    着色器不做除法），z/w: 保留
    sss: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<MaterialRaw>() == 64,
    "MaterialParams 必须与 WGSL 布局一致（R63 起 64 B）"
);

struct GpuMesh {
    vertex_buf: wgpu::Buffer,
    index_buf: wgpu::Buffer,
    num_indices: u32,
    /// 蒙皮属性缓冲（非蒙皮网格为 None）。
    skin_buf: Option<wgpu::Buffer>,
    /// 顶点切线缓冲（R42；网格无切线为 None——绑定常驻零垫缓冲，
    /// 零向量 = 「无顶点切线」标记，片元走屏幕空间 TBN 路径）。
    tangent_buf: Option<wgpu::Buffer>,
    /// 光照贴图 UV2 缓冲（R52c；网格无 UV2 为 None——绑定常驻零垫，
    /// 配合材质掩码 0，采样结果被 `select` 丢弃）。
    uv2_buf: Option<wgpu::Buffer>,
}

/// 一张光照贴图图集的 GPU 资源（R52c）：Rgba16Float 线性 HDR 纹理 +
/// 视图 + **随上传期口径创建的采样器**（C2：MipSettings 在场 = mip 链
/// 纹理 + 三线性 + 各向异性；缺席 = 与常驻采样器逐字段一致的参数，
/// 绑定按图集句柄取用，与 `GpuTexture { view, sampler }` 同构。SSS LUT
/// 经 binding 6 复用本采样器——显式 level 采样不受 mip 过滤影响）。
struct GpuLightmap {
    /// 纹理本体保活（视图的有效性依赖它）。
    #[allow(dead_code)]
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    /// 上传期是否生成 mip 链（C2；观测 API `lightmap_has_mips`）。
    mips: bool,
}

/// 每具蒙皮的 GPU 资源：关节矩阵 storage 缓冲与对应绑定组。
/// 动画驱动下关节矩阵每帧变化，缓冲按关节数上限容量复用。
struct GpuSkin {
    joint_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// 缓冲可容纳的关节数量（不足以时整体重建）。
    capacity: usize,
}

/// 一帧内收集到的蒙皮绘制项（每实体一项，不参与普通批处理）。
struct SkinnedItem {
    /// 蒙皮实体位标识（GpuSkin 缓存键）。
    skin_entity: u64,
    mesh: u32,
    material: u32,
    /// 光照贴图图集句柄值（[`NO_LIGHTMAP`] = 缺席；R52c）。
    lightmap: u32,
    /// 双面标志（R66d，收集期从材质目录烙印）：绘制期选 cull None 管线
    /// 变体。**不进任何排序键**——缺席双面场景的批划分与次序逐位不变。
    double_sided: bool,
    instance: InstanceRaw,
    /// 关节矩阵（列主序平铺后直接上传）。
    joint_matrices: Vec<glam::Mat4>,
    /// 形变权重快照（C4a，收集期从 [`MorphWeights`](crate::morph::MorphWeights)
    /// 拷入）：None = 实体无权重组件（绑定姿势）。由 `resolve_morphs`
    /// 消费——CPU 混合上传逐实体顶点缓冲并填 [`Self::vertex_override`]。
    morph_weights: Option<Vec<f32>>,
    /// 逐实体混合顶点缓冲（C4a）：None = 绑共享网格的静态 VBO（现路径
    /// 逐位不变）；Some 时蒙皮绘制分支（主 / 透明 / 阴影 / 场景深度 /
    /// RT / 探针捕获 / 水面）slot 0 改绑本缓冲。
    vertex_override: Option<wgpu::Buffer>,
}

impl SkinnedItem {
    /// slot 0 顶点缓冲取用（形变覆盖优先，见 [`Self::vertex_override`]）。
    fn vertex_buf<'a>(&'a self, mesh: &'a GpuMesh) -> &'a wgpu::Buffer {
        self.vertex_override.as_ref().unwrap_or(&mesh.vertex_buf)
    }
}

/// 逐形变实体的 GPU 侧顶点缓冲（C4a）：容量复用 + 权重位图案脏检。
/// 键 = 蒙皮实体位标识（与 `GpuSkin` 同构同键，生命周期同为只增缓存）。
struct MorphGpu {
    vertex_buf: wgpu::Buffer,
    /// 缓冲字节容量（顶点数变大时整体重建——LOD 切换 / 换网格句柄形态）。
    capacity_bytes: u64,
    /// 上次上传的权重位图案（一致 ⇒ 不重混合、不重上传）。
    last_weights: Vec<u32>,
}

/// 一帧内收集到的透明绘制项：不透明项额外携带视深排序键。
/// 排序 = 视深降序（远 → 近）+ 实体 bits 升序并列断言（见 `draw`）。
struct TransparentItem {
    /// (网格, 材质) 批键，与不透明 `DrawItem::key` 同构；
    /// 深度序中相邻同 key 的项合并为一个 run 批绘制。
    key: u64,
    /// 视深：包围球中心经视图矩阵变换后的 -z（相机前方为正，越大越远）。
    depth: f32,
    /// 实体位标识（深度并列时的确定性次序）。
    entity: u64,
    /// 双面标志（R66d）：同 key run 内恒同值（标志是材质的函数）。
    double_sided: bool,
    instance: InstanceRaw,
}

/// 透明蒙皮绘制项：不透明蒙皮项 + 视深排序键（透明排序语义同上）。
struct TransparentSkinnedItem {
    skinned: SkinnedItem,
    depth: f32,
}

/// 一帧内收集到的贴花绘制项：按纹理排序分组，同纹理连续区间为一个批
///（1 draw）；批内按实体 bits 并列序（确定性，与收集顺序无关，见
/// `decal.rs` 模块文档的分批策略）。
struct DecalItem {
    /// 贴花纹理句柄值（分批键）。
    texture: u32,
    /// 实体位标识（同批内次序的确定性并列键）。
    entity: u64,
    instance: DecalInstanceRaw,
}

struct GpuTexture {
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    /// 上传时是否走了 BC1 压缩（R30：`texture_is_bc1` 的观测口径，
    /// 供测试断言与内存对账）。
    bc1: bool,
    /// 上传时是否走了 BC3 压缩（R59：`texture_is_bc3` 的观测口径，
    /// 与 `bc1` 恒互斥——分层决策树一次只走一条压缩路径）。
    bc3: bool,
    /// 上传时是否走了 BC7 压缩（C5a：`texture_is_bc7` 观测口径；与
    /// `bc1`/`bc3` 恒互斥——决策树只走一条）。
    bc7: bool,
}

/// 单次纹理上传走的路径（`ensure_texture` 决策树的内部分类，标签与
/// 观测口径共用一个来源）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum CompressedPath {
    Bc7,
    Bc3,
    Bc1,
    Rgba8,
}

struct GpuMaterial {
    bind_group: wgpu::BindGroup,
}

/// 单张渲染目标（R40）的 GPU 侧资源：颜色纹理（UI 可采样）+ 深度 +
/// 线性 HDR 中间缓冲（几何渲到此处）+ ACES 色调映射绑定组 + RT 相机专用
/// 全局 uniform / 绑定组 + RT 实例缓冲。全部按句柄惰性创建、跨帧持有。
struct GpuRenderTarget {
    /// 构建时的尺寸（像素）——与 [`RenderTargetStore`] 描述失配即触发整体
    /// 重建（`resize` 的 GPU 侧生效路径，见 rendertarget.rs 模块文档）。
    width: u32,
    height: u32,
    /// RT 颜色纹理（Rgba8UnormSrgb）：`UiImageRtt` 采样目标。
    #[allow(dead_code)]
    color: wgpu::Texture,
    color_view: wgpu::TextureView,
    /// RT 深度缓冲（Depth32Float，单采样）。
    #[allow(dead_code)]
    depth: wgpu::Texture,
    depth_view: wgpu::TextureView,
    /// RT 线性 HDR 中间缓冲（Rgba16Float，单采样）——几何管线输出格式。
    #[allow(dead_code)]
    hdr: wgpu::Texture,
    hdr_view: wgpu::TextureView,
    /// ACES 色调映射绑定组（hdr_view → color_view，泛光恒 0）。
    blit_bg: wgpu::BindGroup,
    /// RT 相机专用全局 uniform（view_proj / eye 与主相机不同）。
    globals_buf: wgpu::Buffer,
    globals_bg: wgpu::BindGroup,
    /// RT 实例缓冲（独立于主实例缓冲——RT 相机视锥剔除结果不同）。
    instance_buf: Option<wgpu::Buffer>,
    instance_capacity: usize,
}

/// 单台 RT 相机的帧内解析结果（R40）：世界查询派生的相机参数 + 目标
/// 句柄/尺寸。收集阶段与渲染阶段解耦（渲染在阴影 pass 之后执行）。
struct RtCameraPrep {
    handle: RenderTargetHandle,
    view: glam::Mat4,
    eye: Vec3,
    vp: glam::Mat4,
    width: u32,
    height: u32,
}

/// 点光阴影候选（R61）：选择键的输入 + 该灯在 uniform 点光数组中的槽位。
/// 收集期按查询序填充，裁决在收集完成后由 [`select_point_shadow_casters`]
/// 统一做出——`light_slot` 只为回写「启用 / cubemap 槽位」而记录。
struct ShadowCandidateSlot {
    key: PointShadowCandidate,
    light_slot: usize,
}

/// 反射探针候选（R62a）：选择键 + uniform 上传所需的（中心 / 半边长 /
/// 过渡带 / cubemap 槽位）。与 [`ShadowCandidateSlot`] 同款两阶段形态——
/// 收集期按查询序填充，裁决在收集完成后由 [`select_active_probes`] 统一
/// 做出（结果与查询序无关）。
struct ProbeCandidateSlot {
    key: ProbeCandidate,
    center: Vec3,
    extents: Vec3,
    falloff: f32,
    slot: usize,
}

/// 一帧内收集到的不透明绘制项：批键 + 实例数据 + 图集槽位。
///
/// 只是 CPU 侧的收集 / 排序单元：上传走 `draw` 里另建的
/// `Vec<InstanceRaw>`（紧凑 112 B/实例），本结构不整体 cast 到 GPU
/// （`lightmap` 字段会引入尾部填充，bytemuck 的 Pod 契约不允许）。
#[derive(Clone, Copy)]
struct DrawItem {
    key: u64,
    instance: InstanceRaw,
    /// 光照贴图图集句柄值（[`NO_LIGHTMAP`] = 缺席；R52c）。
    ///
    /// **不进排序键**：排序仍只按 `key`（网格, 材质）⇒ 无光照贴图场景的
    /// 排序结果与批划分与 R52b **逐位一致**（兼容硬约束）；本字段只在
    /// 段划分（`run_length`）与材质绑定键里参与，同批内图集交替时切成
    /// 多段（段数 = draw call 数，v1 取舍见 `lightmap.rs` 诚实清单）。
    lightmap: u32,
    /// 实体位标识（R53 遮挡查询身份键：查询槽 ↔ 实体映射与「上一帧
    /// 遮挡」判定按此记账）。**同样不进排序键**，缺席场景逐位不变。
    entity: u64,
    /// 双面标志（R66d，收集期从材质目录烙印）：绘制期选 cull None 管线
    /// 变体。**不进排序键**（标志是材质句柄的函数，同批恒同值）——无
    /// 双面场景的排序 / 批划分与引入前逐位一致。
    double_sided: bool,
}

/// 渲染器：可同时服务窗口表面与离屏目标。
pub struct Renderer {
    gpu: Arc<Gpu>,
    bundle: crate::pipeline::PipelineBundle,
    globals_buf: wgpu::Buffer,
    globals_bg: wgpu::BindGroup,
    /// 级联阴影参数（CsmParamsRaw）缓冲（globals 组 binding 8 的资源，
    /// 绑定组由 build_globals_bind_group 一并写入）。
    csm_params_buf: wgpu::Buffer,
    /// 阴影 pass 的每级联 64B 矩阵缓冲与绑定组（一级联一份，一缓冲一级联）。
    cascade_bufs: [wgpu::Buffer; SHADOW_CASCADES],
    cascade_bgs: [wgpu::BindGroup; SHADOW_CASCADES],
    instance_buf: Option<wgpu::Buffer>,
    instance_capacity: usize,
    /// 上一帧已上传实例的 CPU 镜像（与 GPU 缓冲前 `len` 个实例内容一致），
    /// 供帧级比对决策全量 / 增量 / 跳过上传。上传完成后与组装 scratch
    /// `mem::swap` 零拷贝交换（见 `draw` 步骤 3）。
    last_instances: Vec<InstanceRaw>,
    mesh_gpu: HashMap<u32, GpuMesh>,
    texture_gpu: HashMap<u32, GpuTexture>,
    /// MipSettings 的上传期快照（R66）：`draw` 开头从 World 读取，此后
    /// 本帧内的全部懒上传（材质 / 探针捕获 / UI 图片）同口径。`None` =
    /// 资源缺席（单 mip + 采样器逐字段与 R66 之前一致）；`Some(a)` =
    /// RGBA8 路径上传 CPU mip 链 + 三线性 + 各向异性 `min(a, 适配器上限)`。
    /// 生效时机 = 纹理首次上传（句柄值缓存不追溯，见 `ensure_texture`）。
    mip_anisotropy: Option<u8>,
    /// 材质绑定组缓存（R52c 起键为 [`material_key`] 的复合值 =
    /// `(材质句柄 << 32) | 图集槽位`——同一材质配不同图集时掩码与图集
    /// 纹理都不同，必须分组；无光照贴图槽位恒 0，与旧键一一对应）。
    material_gpu: HashMap<u64, GpuMaterial>,
    /// 中性法线纹理（R42，常驻）：1×1 `Rgba16Float`，值 (0.5, 0.5, 1)。
    /// 材质缺法线槽位时绑定它——16-bit float 精确表示 0.5/1.0，采样
    /// *2−1 后恰为 (0,0,1)，扰动恒等（8-bit unorm 做不到，见
    /// material.rs 的逐位一致性论证）。纹理本体保活以维持 view 有效。
    #[allow(dead_code)]
    neutral_normal_tex: wgpu::Texture,
    neutral_normal_view: wgpu::TextureView,
    /// 常驻零填充切线垫缓冲（R42）：无切线网格共用（1 MiB = 65536 顶点
    /// × 16 B；顶点数超出垫容量的网格在 `ensure_mesh` 单独分配零缓冲）。
    /// 内容全零 = 顶点切线路径的「缺席」标记。
    tangent_dummy_buf: wgpu::Buffer,
    /// 常驻零填充 UV2 垫缓冲（R52c）：无光照贴图 UV 的网格共用
    /// （512 KiB = 65536 顶点 × 8 B；顶点数超出垫容量的网格在
    /// `ensure_mesh` 单独分配零缓冲）。内容全零，配合材质掩码 0。
    uv2_dummy_buf: wgpu::Buffer,
    /// 光照贴图图集 GPU 缓存（R52c）：句柄值 → Rgba16Float 纹理 + 采样器，
    /// 按句柄懒上传、跨帧持有（照 `texture_gpu`）。
    lightmap_gpu: HashMap<u32, GpuLightmap>,
    /// 常驻 1×1 黑光照贴图（R52c）：无图集的材质绑定它（创建即零初始化
    /// ⇒ 全零辐照，无需写入）。纹理本体保活以维持 view 有效。
    #[allow(dead_code)]
    lightmap_dummy_tex: wgpu::Texture,
    lightmap_dummy_view: wgpu::TextureView,
    /// 光照贴图采样器（R52c，双线性 + ClampToEdge）：兜底黑纹理与全部
    /// 真实图集共用一份（寻址 / 过滤参数与图集尺寸无关）。**R63 起预积分
    /// SSS LUT 也复用它**（同样 ClampToEdge 双线性，材质组不新增采样器槽位）。
    lightmap_sampler: wgpu::Sampler,
    /// 预积分皮肤 SSS LUT（R63，常驻）：`Rgba16Float` 64×64，程序化生成
    /// （`sss::lut_half_bytes`，纯函数 + 无 RNG）。纹理本体保活以维持
    /// view 有效；尺寸与纹素中心对齐口径见 `sss.rs` 模块文档。
    #[allow(dead_code)]
    sss_lut_tex: wgpu::Texture,
    sss_lut_view: wgpu::TextureView,
    /// 渲染目标（R40）GPU 缓存：句柄 → 颜色 / 深度 / HDR / 绑定组 /
    /// 专用全局与实例缓冲。按句柄惰性创建、跨帧持有（尺寸不变时零重建；
    /// `resize` 失配即重建，见 rendertarget.rs）。
    render_target_gpu: HashMap<u32, GpuRenderTarget>,
    /// LOD v2（C1）跨帧记忆：实体位 → 上一帧**主相机**收集的档位
    /// （true = high）。仅 `LodSettings` 资源在场时读写；缺席 = 恒空表
    /// 零触碰（v1 逐位不变）。不参与 `reset_taa`（相机无关的场景侧
    /// 状态，与探针域同立场，§8.58⑩）；滞留条目（despawn 实体）是
    /// 每实体 8 字节级的有界垃圾，随句柄复用自愈。
    lod_v2_memory: std::collections::BTreeMap<u64, bool>,
    /// RT 色调映射共享采样器 + 合成参数（泛光 0 / 曝光 1 / 暗角 0——RT
    /// 无后处理链，恒中性）。两者所有 RT 共用。
    rt_blit_sampler: wgpu::Sampler,
    rt_post_params_buf: wgpu::Buffer,
    /// 蒙皮实体位标识 → 关节矩阵缓冲。
    skin_gpu: HashMap<u64, GpuSkin>,
    /// 蒙皮实体位标识 → 形变混合顶点缓冲（C4a；仅带
    /// [`MorphWeights`](crate::morph::MorphWeights) 且网格有 morph 表的
    /// 实体进入，缺席路径零触碰）。
    morph_gpu: HashMap<u64, MorphGpu>,
    /// 形变顶点缓冲的实际上传帧次累计（观测口径：脏检生效时不增长，
    /// 供测试断言「静态权重 ⇒ 零重上传」）。
    morph_uploads: u64,
    /// 方向光级联阴影贴图：R32Float 2D 数组（2048² × SHADOW_CASCADES 层，
    /// R 存光空间深度），持有纹理以保证 view 有效。
    #[allow(dead_code)]
    shadow_map: wgpu::Texture,
    /// 全层数组采样视图（进全局绑定组 binding 1，片元按级联选层）。
    shadow_map_view: wgpu::TextureView,
    /// 阴影 pass 的深度测试附件（Depth32Float 数组，同层数——保证贴图
    /// 存最近面深度）。
    #[allow(dead_code)]
    shadow_depth_buf: wgpu::Texture,
    /// 每级联一个颜色/深度渲染视图（D2 + base_array_layer = 级联序），
    /// 逐级联 pass 渲染到对应层。
    shadow_color_views: [wgpu::TextureView; SHADOW_CASCADES],
    shadow_depth_views: [wgpu::TextureView; SHADOW_CASCADES],
    /// 点光 cubemap 阴影贴图数组：R32Float 预算 × 6 层（层序 = 槽位 × 6 +
    /// 面序），R 存归一化线性距离（length(world − light) / range，
    /// 1.0 = 无遮挡）。R61 起从单盏扩为 [`POINT_SHADOW_BUDGET`] 盏。
    #[allow(dead_code)]
    point_shadow_map: wgpu::Texture,
    /// Cube **数组**维度采样视图（进全局绑定组 binding 7；层数 =
    /// 预算 × 6，片元按逐灯槽位索引选 cube）。
    point_shadow_cube_view: wgpu::TextureView,
    /// 每（槽位, 面）一个 D2 渲染视图（base_array_layer = 槽位 × 6 + 面序）。
    point_shadow_face_views: [wgpu::TextureView; POINT_SHADOW_BUDGET * 6],
    /// 点光阴影深度附件（Depth32Float 预算 × 6 层，Less 测试保证存最近
    /// 遮挡物）。
    #[allow(dead_code)]
    point_shadow_depth: wgpu::Texture,
    point_shadow_depth_face_views: [wgpu::TextureView; POINT_SHADOW_BUDGET * 6],
    /// 每（槽位, 面）一个独立参数缓冲 + 绑定组（一缓冲一面：同一 submit
    /// 内对同一缓冲的多次 write_buffer 会在执行前全部合并，泛光参数的
    /// 教训；槽位之间互不共享——各自的灯位置 / range / 面矩阵都不同）。
    point_shadow_face_bufs: [wgpu::Buffer; POINT_SHADOW_BUDGET * 6],
    point_shadow_face_bgs: [wgpu::BindGroup; POINT_SHADOW_BUDGET * 6],
    /// 反射探针参数 uniform（R62a，globals 组 binding 9）：生效探针数 +
    /// 逐探针影响域 / cubemap 槽位。零值（count = 0）即「无探针」语义。
    probe_params_buf: wgpu::Buffer,
    /// 探针捕获期专用的**零值**参数缓冲（R62a）：捕获 pass 的片元着色器
    /// 与主 pass 同一份 `standard.wgsl`，若绑主参数缓冲，捕获会把既有探针
    /// 的反射再反射一次（自反馈）且与本帧选择状态耦合。绑一份恒零参数 +
    /// 下面的哑 cubemap ⇒ 捕获的镜面恒走全局 env（语义正交、无自反馈）。
    /// 字段仅为保活（绑定组另持内部引用）。
    #[allow(dead_code)]
    probe_capture_params_buf: wgpu::Buffer,
    /// 反射探针 cubemap 数组（R62a）：Rgba16Float、`MAX × 6` 层、
    /// [`PROBE_MIP_COUNT`] 级 mip（层序 = 槽位 × 6 + 面序，面序与
    /// [`crate::light::cube_face_view_proj`] 配对）。所有探针共用一张数组，
    /// 槽位由 [`ProbeStore`] 分配 ⇒ 绑定组只建一次、烘焙不重建。
    probe_map: wgpu::Texture,
    /// CubeArray 采样视图（globals 组 binding 10）。
    probe_cube_view: wgpu::TextureView,
    /// 捕获用的**共享原始立方体**（mip 0）：预滤波 pass 把源绑成采样纹理、
    /// 把目标绑成颜色附件，同纹理既读又写会触发 wgpu 校验错误 ⇒ 捕获落在这
    /// 张共享擦写立方体上，预滤波再写进 [`Self::probe_map`]。所有探针共用
    /// （烘焙是一次性的，内容无需跨烘焙保留）。字段仅为保活。
    #[allow(dead_code)]
    probe_raw: wgpu::Texture,
    probe_raw_cube_view: wgpu::TextureView,
    probe_raw_face_views: [wgpu::TextureView; 6],
    /// 捕获用的深度附件（Depth32Float × 6 层）——与主 pass 同款「颜色 +
    /// 深度双附件」，保证重叠表面按最近面着色。字段仅为保活。
    #[allow(dead_code)]
    probe_raw_depth: wgpu::Texture,
    probe_raw_depth_face_views: [wgpu::TextureView; 6],
    /// 捕获期每面一份 Globals 缓冲 + 绑定组（R62a）：面矩阵 / 眼点逐面不同，
    /// 一缓冲一面（同一 submit 内对同一缓冲的多次 write_buffer 会在执行前
    /// 合并）。绑定组内的探针资源指向**哑资源 + 零参数**（见上）。
    probe_face_globals_bufs: [wgpu::Buffer; 6],
    probe_face_globals_bgs: [wgpu::BindGroup; 6],
    /// 1×1 哑 cubemap 数组（捕获绑定组专用；内容永不被采样——捕获参数恒
    /// count = 0）。纹理本体保活以维持视图有效。
    #[allow(dead_code)]
    probe_dummy_tex: wgpu::Texture,
    /// 哑 cubemap 视图（捕获绑定组的 binding 10；字段仅为保活）
    #[allow(dead_code)]
    probe_dummy_view: wgpu::TextureView,
    /// 捕获实例缓冲（独立于主帧实例缓冲：主缓冲带跨帧增量上传镜像
    /// `last_instances`，复用会破坏其一致性）。
    probe_instance_buf: Option<wgpu::Buffer>,
    probe_instance_capacity: usize,
    /// 探针烘焙登记表（实体位 → cubemap 槽位 + 是否已烘焙）。
    probe_store: ProbeStore,
    /// `probe_params_buf` 当前是否持有非零生效数：为真时下一帧即使没有生效
    /// 探针也必须回写零值（否则退场的探针会滞留生效）。无探针场景恒 false
    /// ⇒ 一次 write_buffer 都不发（命令流与 R61 逐位一致）。
    probe_params_active: bool,
    /// 视差修正**对照模式**（R62a，验收 / 归因专用）：`true` 时探针 uniform
    /// 的视差开关置 0——采样方向不做 box projection（直接用反射方向），
    /// 同一二进制内复现「无修正」的反射，供 `docs/reflection_probe.png`
    /// 的左右面板对照与「修正确实把反射贴到盒几何上」的归因测试
    /// （`tests/probe_render.rs` 用解析镜像位置断言两侧的像素落点）。
    /// 生产路径恒 `false`。
    legacy_probe_no_parallax: bool,
    /// 阴影偏移**对照模式**（R61，验收 / 归因专用）：`true` 时 uniform 写
    /// 回 v1 的常数 bias 并把坡度项与法线偏移的系数置零——同一二进制内
    /// 逐位复现 R60 的阴影采样行为，供验收图左右面板对照与「新技术确实
    /// 改变了什么」的归因测试。生产路径恒 `false`（无运行时 bias 可视化
    /// 调试视图——本开关只切换偏移口径，不输出任何诊断可视化）。
    legacy_shadow_bias: bool,
    /// IBL 环境贴图（初始化时一次性烘焙；视图已进全局绑定组，
    /// 此字段保持纹理存活）。`SkySource` 资源变更时整体重烘焙替换。
    #[allow(dead_code)]
    env: crate::environment::EnvironmentMaps,
    /// 大气散射天空参数缓冲（R79，64B SkyParamsRaw，sky 组 binding 7）。
    /// 缺席 / 关闭帧不写（保持零初始化 mode = 0 死码分支——命令流与
    /// R78 逐位一致）。
    sky_params_buf: wgpu::Buffer,
    /// 主相机天空绑定组（R79，`sky_layout`：Globals + env + 采样器 +
    /// SkyParams）。env 重烘焙（视图更换）时随 `rebuild_globals_bind_group`
    /// 一并重建。
    sky_bg: wgpu::BindGroup,
    /// 探针捕获面的天空绑定组（R79，每面一份——面 Globals 缓冲不同；
    /// 与 `probe_face_globals_bgs` 同生命周期，探针内的天空与主视口
    /// 同一 `skybox.wgsl` 同一参数——大气对捕获相机同样生效）。
    probe_face_sky_bgs: [wgpu::BindGroup; 6],
    /// 已烘焙环境源对应的 generation（0 = 程序化天空；与 `SkySource`
    /// 资源上的 generation 不一致时在下一帧 `draw` 触发重烘焙）。
    env_baked_generation: u64,
    /// HDR 中间缓冲（MSAA 4x 颜色附件 + 单采样解析目标 + blit 绑定组）。
    hdr: Option<HdrTargets>,
    hdr_size: (u32, u32),
    /// 主相机场景深度副本（R43，仅 DOF / MB 启用时创建）。
    scene_depth: Option<SceneDepthTargets>,
    /// 深度副本尺寸（复用判定键）。
    scene_depth_size: (u32, u32),
    /// 最终颜色目标格式（R53：FXAA 中间 LDR 缓冲与 blit 目标同格式；
    /// 与 `create_pipeline` 的 color_format 同源）。
    color_format: wgpu::TextureFormat,
    /// 遮挡查询常驻 GPU 资源（R53，首次启用时惰性创建；关闭恒 `None`
    /// ——零开销默认路径的分配侧保证）。
    occ_gpu: Option<OccGpu>,
    /// 在途读回（R53，至多一帧；见 [`OccInflight`]）。
    occ_inflight: Option<OccInflight>,
    /// 最近一次新鲜读回中「样本数 = 0」的实体 bits（本帧主 pass 剔除
    /// 依据；无新鲜结果时不使用——全部可见）。
    occ_hidden: std::collections::HashSet<u64>,
    /// 帧计数（R53 新鲜度判定与读回双缓冲槽位；每帧 `draw` 开头自增）。
    occ_frame: u64,
    /// 上一帧主相机视图投影（R43 运动模糊的重投影基准）。`None` = 尚未
    /// 渲染过任何帧（首帧）——MB 用当前帧矩阵补位，速度恒 0、原色透传。
    /// TAA（R60）开启的帧保存的是**抖动后的**矩阵：下帧重投影需要的
    /// 正是上一帧的实际渲染矩阵（历史由该矩阵光栅化而来）。
    prev_view_proj: Option<glam::Mat4>,
    /// TAA 帧序号（R60）：Halton 抖动序列的索引（模 8）。只在实际渲染
    /// 且 TAA 开启的帧自增——被早退跳过的帧不推进序列（与
    /// `prev_view_proj` 的保存时机同一纪律）。
    taa_frame: u64,
    /// TAA 重置请求（R60）：`reset_taa()` 置位，下一帧 TAA pass 消费
    /// （混合系数置 0 = 输出逐位等于当前帧）。分辨率变化 / 首次创建
    /// 历史缓冲 / 关→开切换也会置位。
    taa_reset_pending: bool,
    /// 上一帧 TAA 是否开启（R60）：关→开切换帧强制重置（历史是旧的
    /// 抖动序列与相机下的产物，不可信）。
    taa_was_on: bool,
    /// SSR 常驻 GPU 资源（R62b，哑资源恒在——布局要求的 binding 占位）。
    ssr: SsrGpu,
    /// SSR 上帧资源是否有效（R62b）：上帧 SSR 开启且完成了场景深度副本
    /// 与帧尾颜色副本 ⇒ 本帧可以步进。首帧启用 / 关→开 / 分辨率变化 /
    /// 切镜（reset_taa 跟随）/ 上帧无不透明几何 时置 false ⇒ 该帧整段
    /// 跳过（输出与 SSR 关闭帧逐位一致）。
    ssr_prev_valid: bool,
    /// 上一帧 SSR 是否开启（R62b）：关→开切换帧强制跳过（上帧资源是
    /// 关闭语义下遗留的内容，不可信）。
    ssr_was_on: bool,
    /// SSR 参数缓冲是否处于「已写非零」状态（R62b）：缺席 / 关闭路径不
    /// 发 write_buffer，退场的那一帧才回写零值（探针 params_active
    /// 同款记账——关闭后残留 flags.x = 1 会拿陈旧上帧资源做步进）。
    ssr_uniform_live: bool,
    /// AO 常驻 GPU 资源（R77，哑纹理恒在——布局要求的 binding 14 占位）。
    ao: AoGpu,
    /// 前向光源分箱（R78）常驻资源：哑资源恒在（mode = 0 分支死码），
    /// 真缓冲按帧写（mode = 1），`bind_real` 决定 binding 15/16/17 指向。
    light_bins: LightBinsGpu,
    /// GI 探针三阶 SH（R83）常驻资源：哑资源恒在（mode = 0 分支死码），
    /// 真缓冲按帧写（mode = 1），`bind_real` 决定 binding 18/19 指向。
    gi_sh: GiShGpu,
    /// DDGI 深度表（R88）常驻资源：4 B 哑缓冲恒在，768 KiB 真表**惰性创建**
    /// （首个 mode = 2 帧），`bind_real` 决定 binding 20 指向。
    ddgi: DdgiGpu,
    /// DDGI 增量上传的跨帧记账（见 [`DdgiUploadState`]）。
    ddgi_upload: DdgiUploadState,
    /// DDGI 深度项**对照模式**（R88，验收 / 归因专用）：`false` 时参数块的
    /// w 分量置 0 ⇒ 着色器的 chebyshev 深度项恒 1，同一二进制内复现「只有
    /// R83 径向窗」的混合行为，供 `docs/ddgi.png` 的漏光对照面板与
    /// 「深度项确实抑制了串味」的归因测试（`tests/ddgi_render.rs`）。
    /// 生产路径恒 `true`（照 `legacy_probe_no_parallax` 先例）。
    ddgi_depth_weight: bool,
    /// 「DDGI 与 R83 烘焙探针集同时在场」的告警是否已发过（裁定 2 要求
    /// warn **一次**，不是每帧刷屏）。
    ddgi_mutex_warned: bool,
    /// scene_depth pass 最近一次运行的帧号（R77，`occ_frame` 口径）：
    /// GTAO 读的是「上一帧」深度——本帧编号恰好等于该值 + 1 才允许编码
    /// AO pass，否则可见度图清成中性 1.0（首帧启用 / 上帧无几何 / 深度
    /// 副本因分辨率变化重建时自然落进中性路径）。`None` = 从未运行。
    ao_depth_frame: Option<u64>,
    /// 平面水体（R81）常驻 GPU 资源：`WaterSurface` 实体首次在场时创建、
    /// 随窗口尺寸（半分辨率口径）变化重建；无水体帧恒 `None`——无捕获
    /// pass、无额外 write_buffer、零像素零命令流变化（零开销默认路径）。
    water: Option<WaterGpu>,
    /// 水体捕获图尺寸（半分辨率，复用判定键）。
    water_size: (u32, u32),
    /// 反射捕获的收集暂存（跨帧复用；收集视锥 = 镜像相机 VP）。
    water_items: Vec<DrawItem>,
    water_skinned: Vec<SkinnedItem>,
    water_transparent: Vec<TransparentItem>,
    water_transparent_skinned: Vec<TransparentSkinnedItem>,
    water_instance_scratch: Vec<InstanceRaw>,
    /// UI 字形图集纹理（初始化时一次性构建；字段仅为保持 view 存活）。
    #[allow(dead_code)]
    ui_atlas: wgpu::Texture,
    /// UI 绑定组：图集纹理 + Nearest 采样器 + 屏幕尺寸 uniform。
    ui_bind_group: wgpu::BindGroup,
    /// UI Nearest 采样器（图集 / 图片共用：位图锐利、像素化小地图）。
    ui_sampler: wgpu::Sampler,
    /// 屏幕尺寸 uniform（每帧 write_buffer，供像素 → NDC 映射）。
    ui_screen_buf: wgpu::Buffer,
    /// UI 顶点缓冲（容量不足时整体重建，照抄实例缓冲的容量复用模式）。
    ui_vertex_buf: Option<wgpu::Buffer>,
    /// UI 顶点缓冲可容纳的顶点数。
    ui_capacity: usize,
    /// 粒子实例缓冲（独立于几何实例缓冲；每帧全量上传，无跨帧镜像——
    /// 粒子逐帧全变，帧级比对无增量价值）。
    particle_buf: Option<wgpu::Buffer>,
    /// 粒子实例缓冲可容纳的实例数。
    particle_capacity: usize,
    /// 粒子相机 uniform（billboard 基向量，每帧有粒子时 write_buffer）。
    particle_camera_buf: wgpu::Buffer,
    /// 粒子相机绑定组（粒子管线的组 1）。
    particle_camera_bg: wgpu::BindGroup,
    /// 粒子 v2 GPU 池（R82；按实体绑定、销毁不回收，见 ParticleV2Pool）。
    particle_v2_pools: Vec<ParticleV2Pool>,
    /// 粒子 v2 收集暂存（跨帧复用）。
    particle_v2_scratch: Vec<ParticleV2Item>,
    /// meshlet GPU 资源束（R85；首个 meshlet 实例在场时惰性创建，缺席 =
    /// 不创建零开销）。
    meshlet_gpu: Option<MeshletGpu>,
    /// meshlet GPU arena 的 CPU 镜像 + 句柄登记表（注册期追加）。
    meshlet_arena: MeshletArena,
    /// meshlet 在途读回（遮挡 OccInflight 同款双槽协议）。
    meshlet_inflight: Option<MeshletInflight>,
    /// 最近一次成功消费的紧凑结果（绘制裁决 + 三角形统计输入）。
    meshlet_consumed: Option<MeshletConsumed>,
    /// meshlet 帧号（每 draw 自增——奇偶槽位轮转的基准；与 occ_frame
    /// 同款「只在真实渲染帧推进」纪律，被跳过的帧不产生「上一帧」）。
    meshlet_frame: u64,
    /// 上一帧实例槽 → mesh 句柄（消费读回时三角形统计的解析表）。
    meshlet_last_handles: Vec<u32>,
    /// meshlet 收集暂存（跨帧复用）。
    meshlet_frame_scratch: MeshletFrame,
    /// 上一收集帧的 run 起点表（与 meshlet_last_handles 同序同生命周期
    /// ——紧凑列表三角形统计的解析输入，R86）。
    meshlet_last_run_starts: Vec<u32>,
    /// meshlet 页驻留状态机（R87）：`MeshletStreamSettings` 预算在场时
    /// 惰性创建；None = 全驻留 = R86 行为逐位不变。
    meshlet_stream: Option<crate::meshlet_stream::ResidencyManager>,
    /// 上一收集帧的流送请求（全局页号, 页最小屏幕误差）——本帧 tick 的
    /// 输入（请求一帧延迟：收集在 tick 之后使用本帧驻留状态）。
    meshlet_stream_requests: Vec<(u32, f32)>,
    /// 流送 → 非流送迁移标记：驻留期间页可能被逐出 arena，回到非流送
    /// 路径前必须整库重传（下一次 [`Self::ensure_meshlet_world`] 消费）。
    meshlet_stream_recover_upload: bool,
    /// meshlet 实体的遮挡查询条目（R87 遮挡整合——收集期填充，2e 段
    /// 追加到普通实例条目之后；跨帧复用）。
    meshlet_occ_aabb_scratch: Vec<AabbInstanceRaw>,
    /// 与 [`Self::meshlet_occ_aabb_scratch`] 同序的实体位模式表。
    meshlet_occ_entity_scratch: Vec<u64>,
    /// 贴花代理几何顶点缓冲（单位盒 ±Z 端盖，初始化时一次性上传；
    /// 字段保持缓冲存活）。
    #[allow(dead_code)]
    decal_vertex_buf: wgpu::Buffer,
    /// 贴花代理几何索引缓冲（12 索引，初始化时一次性上传）。
    #[allow(dead_code)]
    decal_index_buf: wgpu::Buffer,
    /// 贴花实例缓冲（`DecalInstanceRaw` 144B/实例，独立于几何实例缓冲；
    /// 贴花实例少且常静止，每帧全量上传、无帧级比对——增量价值近零，
    /// 与粒子同款决策）。
    decal_instance_buf: Option<wgpu::Buffer>,
    /// 贴花实例缓冲可容纳的实例数。
    decal_capacity: usize,
    /// 贴花纹理绑定组缓存（纹理句柄值 → 组 1 绑定组；跨帧复用）。
    decal_bind_groups: HashMap<u32, wgpu::BindGroup>,
    /// 贴花收集暂存（跨帧复用）。
    decal_scratch: Vec<DecalItem>,
    /// 贴花实例组装暂存（跨帧复用）。
    decal_instance_scratch: Vec<DecalInstanceRaw>,
    scratch: Vec<DrawItem>,
    instance_scratch: Vec<InstanceRaw>,
    skinned_scratch: Vec<SkinnedItem>,
    /// 透明项收集暂存（普通 + 蒙皮，跨帧复用）。
    transparent_scratch: Vec<TransparentItem>,
    transparent_skinned_scratch: Vec<TransparentSkinnedItem>,
    /// UI 顶点组装暂存（跨帧复用，避免每帧堆分配）。
    ui_scratch: Vec<crate::ui::UiVertex>,
    /// UI 图片暂存（跨帧复用）——图片独立于 atlas 顶点缓冲（按源绑定组
    /// 分别绘制于 atlas UI 之下/之上，见 draw 的 UI pass）。
    ui_image_scratch: Vec<crate::ui::UiImageItem>,
    /// UI 图片顶点缓冲（容量不足时整体重建，跨帧复用）。
    ui_image_buf: Option<wgpu::Buffer>,
    ui_image_capacity: usize,
    /// RT 收集暂存（跨帧复用；最多 RENDER_TARGET_MAX 个 RT 相机，逐相机
    /// 顺序完成后清空复用）。
    rt_items: Vec<DrawItem>,
    rt_skinned: Vec<SkinnedItem>,
    rt_transparent: Vec<TransparentItem>,
    rt_transparent_skinned: Vec<TransparentSkinnedItem>,
    rt_instance_scratch: Vec<InstanceRaw>,
    /// 遮挡查询暂存（R53，跨帧复用）：主 pass 绘制判定布尔表、逐实例
    /// AABB、查询槽 ↔ 实体映射。长度与当帧 items 对齐（bool 表恒覆盖
    /// 全部不透明实例，`occ_filtered` 标记其是否生效）。
    occ_drawn_scratch: Vec<bool>,
    occ_aabb_scratch: Vec<AabbInstanceRaw>,
    occ_entity_scratch: Vec<u64>,
    /// 粒子收集暂存：additive 段（后接排好序的 alpha 段，一次全量上传）。
    particle_scratch: Vec<ParticleInstance>,
    /// alpha 粒子收集暂存：(视深, 收集序号, 实例)——排序键与数据一体。
    particle_alpha_scratch: Vec<(f32, u32, ParticleInstance)>,
    clear_color: wgpu::Color,
    pub(crate) stats: FrameStats,
}

/// HDR 中间缓冲集合（尺寸变化时整体重建）。
struct HdrTargets {
    #[allow(dead_code)]
    msaa: wgpu::Texture,
    msaa_view: wgpu::TextureView,
    #[allow(dead_code)]
    resolved: wgpu::Texture,
    resolved_view: wgpu::TextureView,
    /// blit 合成绑定组：hdr_texture = 主解析缓冲（链上无任何入链的全
    /// 分辨率 pass 时的默认路径，与 pass 串联框架引入前的绑定逐项一致）。
    blit_bind_group: wgpu::BindGroup,
    /// blit 合成绑定组的全分辨率 A 槽变体：hdr_texture = 全分辨率 A 槽
    /// （链尾为暗角输出或 DOF 输出时 blit 的源）。全分辨率对按需分配，
    /// 故本组随之惰性构建、跨帧复用；HdrTargets 尺寸重建时随集合丢弃。
    blit_full_a_bg: Option<wgpu::BindGroup>,
    /// blit 合成绑定组的全分辨率 B 槽变体（链尾为运动模糊输出时 blit
    /// 的源；构建时机与 A 槽变体一致）。
    blit_full_b_bg: Option<wgpu::BindGroup>,
    /// 后处理中间纹理 ping-pong 分配器（post.rs 框架）：半分辨率对在
    /// 建集合时即建好（泛光默认启用），全分辨率对由暗角 / DOF / MB
    /// 按需分配。
    post_scratch: crate::post::PostScratch,
    /// 后处理链共享过滤采样器（各中间 pass 与 blit 同一采样行为）。
    post_sampler: wgpu::Sampler,
    /// 三个泛光 pass 的参数缓冲（提取 / 水平模糊 / 垂直模糊）。
    /// 独立三份而非一份缓冲的三个插槽：绑定偏移必须满足
    /// min_uniform_buffer_offset_alignment（典型 256），32B 插槽不合规。
    bloom_params: [wgpu::Buffer; 3],
    /// 暗角参数缓冲（PostParamsRaw 布局，z 分量生效；pass 关闭时不写）。
    vignette_params: wgpu::Buffer,
    /// 景深参数缓冲（DofParamsRaw 布局；pass 关闭时不写）。
    dof_params: wgpu::Buffer,
    /// 运动模糊参数缓冲（MbParamsRaw 布局；pass 关闭时不写）。
    motion_blur_params: wgpu::Buffer,
    /// 体积雾参数缓冲（R79，FogParamsRaw 布局；pass 关闭时不写）。
    fog_params: wgpu::Buffer,
    /// blit 合成参数（16B，PostParamsRaw：x 泛光强度 / y 曝光 / z 暗角 /
    /// w 保留）。每帧整体覆写。
    post_params: wgpu::Buffer,
    /// FXAA 中间 LDR 缓冲（R53）：仅 `FxaaSettings` 开启时惰性创建
    /// （见 `ensure_fxaa_targets`）；关闭恒 `None`、blit 直写最终目标。
    fxaa: Option<FxaaTargets>,
    /// TAA 历史缓冲（R60）：仅 `TaaSettings` 开启时惰性创建（见
    /// `ensure_taa_targets`）；关闭恒 `None`——不抖动、不入链、与 R59
    /// 命令流逐位一致（零开销默认路径）。随集合尺寸重建整体失效
    /// （= 分辨率变化历史重置）。
    taa: Option<TaaTargets>,
}

impl HdrTargets {
    /// 一帧后处理编码期的视图解析表（主解析缓冲 + 中间纹理分配器 +
    /// 可选的 TAA 本帧输出视图），供 post.rs 的统一编码函数把 pass
    /// 描述解析成实际视图。TAA 开启时 `taa_history` = 本帧写半边的
    /// 历史纹理视图（下游链的 `PostInput::TaaHistory` 据此解析）。
    fn post_views<'a>(
        &'a self,
        taa_history: Option<&'a wgpu::TextureView>,
    ) -> crate::post::PostViews<'a> {
        crate::post::PostViews::new(&self.resolved_view, &self.post_scratch, taa_history)
    }
}

/// 主相机场景深度副本（R43）：R32Float 单采样纹理（R = 线性视距）+
/// 自带 Depth32Float 单采样深度附件。仅 DOF / MB 启用时创建并编码
/// （零开销默认路径）；尺寸变化时整体重建。
struct SceneDepthTargets {
    #[allow(dead_code)]
    texture: wgpu::Texture,
    /// 进 post_depth_layout binding 3 的采样视图。
    view: wgpu::TextureView,
    #[allow(dead_code)]
    depth: wgpu::Texture,
    depth_view: wgpu::TextureView,
    /// 64B 相机矩阵 uniform（CascadeParams 同布局，cascade_layout 复用）。
    params: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

/// FXAA 中间 LDR 缓冲（R53）：开启时色调映射 blit 改写这里（同格式的
/// 最终目标），FXAA pass 再采样写最终目标。缺席/关闭时整组资源不存在
/// （`HdrTargets` 里恒 `None`，blit 直写最终目标——逐位默认路径）。
/// 随 `HdrTargets` 尺寸重建一并失效重建。
struct FxaaTargets {
    /// 纹理本体保活（视图有效性依赖）。
    #[allow(dead_code)]
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// 16B 参数（vec4：edge_threshold / edge_threshold_min / 1/w / 1/h）。
    params: wgpu::Buffer,
    /// 线性采样器：FXAA 的 A/B 采样点在纹素中心 ±0.5 内偏移，双线性
    /// 过滤是其定义的一部分（Nearest 会把亚纹素偏移量化回同纹素、
    /// 整帧无差别透传——实测教训）；中心取样恰在纹素正中，权重
    /// (1,0,0,0) 精确命中，早退分支的逐位透传性质不受影响。链共享
    /// `post_sampler` 是 Nearest（泛光模糊靠显式纹素偏移工作），不能复用。
    /// 字段仅为保活与文档说明（绑定组已引用该采样器）。
    #[allow(dead_code)]
    sampler: wgpu::Sampler,
    /// post_layout 绑定组（参数 + LDR 源视图 + 线性采样器）。
    bind_group: wgpu::BindGroup,
}

/// TAA 历史缓冲（R60）：Rgba16Float 双纹理 ping-pong（TAA 读上帧写
/// 本帧，下游链读本帧槽）+ 参数缓冲 + 专属线性采样器 + blit 接入绑定组
/// （链尾直读 TAA 输出）。仅 `TaaSettings` 开启时创建（零开销默认路径）；
/// 随 `HdrTargets` 尺寸重建整体失效重建 = 分辨率变化免费获得历史重置。
/// TAA pass 自身的读取绑定组由 `encode_taa_pass` 按帧现建（框架惯例：
/// 轻量句柄不值得跨帧缓存）。
struct TaaTargets {
    /// 双历史纹理本体保活（视图有效性依赖）。
    #[allow(dead_code)]
    hist: [wgpu::Texture; 2],
    hist_views: [wgpu::TextureView; 2],
    /// 144B 参数（TaaParamsRaw 布局；每帧有 TAA 时 write_buffer）。
    params: wgpu::Buffer,
    /// 线性采样器（历史重投影在亚纹素位置采样——FXAA 同款教训）。
    #[allow(dead_code)]
    sampler: wgpu::Sampler,
    /// blit 接入绑定组 `[i]`：HDR 源 = hist[i]（TAA 开启且链上无其他
    /// 全分辨率 pass 时 blit 直读 TAA 输出）。
    blit_bgs: [wgpu::BindGroup; 2],
}

/// SSR 的常驻 GPU 资源（R62b）。布局要求的 binding 11/12/13 恒需合法
/// 绑定，故哑资源（1×1 纹理 + 零参数缓冲）**始终存在**（探针的
/// 「零参数 + 1×1 哑 cubemap」同款——缺席路径不分配任何大资源、着色器
/// flags.x = 0 早退，渲染与 R62a 逐位一致）。真实资源只有上帧颜色副本
/// 一张（`prev_color`，`SsrSettings` 首次开启时惰性分配）：上帧深度
/// **复用现有单份 scene_depth 纹理**——scene_depth pass 编码在主 pass
/// 之后（同一编码器按序执行），主 pass 读到的恒为上一帧内容，无需
/// ping-pong（裁决 2 的偏离声明见 ARCHITECTURE §4.31）。
struct SsrGpu {
    /// 96B 参数（SsrParamsRaw 布局；主相机路径，SSR 活跃 / 重置帧都写）。
    params_buf: wgpu::Buffer,
    /// 96B 零参数（探针捕获面 / RT 相机绑定组专用——永不写，恒 flags = 0，
    /// 捕获 / RT 片元不做 SSR）。
    off_params_buf: wgpu::Buffer,
    /// 1×1 哑颜色纹理 / 视图（prev_color 未创建时占位 binding 11）。
    #[allow(dead_code)]
    dummy_color: wgpu::Texture,
    dummy_color_view: wgpu::TextureView,
    /// 1×1 哑深度纹理 / 视图（占位 binding 12）。
    #[allow(dead_code)]
    dummy_depth: wgpu::Texture,
    dummy_depth_view: wgpu::TextureView,
    /// 上帧主解析缓冲的帧尾副本（一次 copy_texture_to_texture；仅 SSR
    /// 开启时创建，尺寸随 HDR 集合）。`None` = 从未启用（哑纹理占位）。
    prev_color: Option<SsrPrevColor>,
    /// `prev_color` 当前尺寸（复用判定键）。
    prev_size: (u32, u32),
}

/// 上帧颜色副本纹理 + 采样视图（本体保活字段仅供视图有效性依赖）。
struct SsrPrevColor {
    #[allow(dead_code)]
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// AO（GTAO，R77）的常驻 GPU 资源。globals 布局的 binding 14 恒需合法
/// 绑定，1×1 哑纹理**始终存在**（R16Float，纹素恰为 1.0——standard 无
/// 条件采样后 `× 1.0` 逐位精确，缺席路径零分支）；真实可见度图只有半
/// 分辨率 R16Float 一张（`AoSettings` 首次开启时惰性分配，尺寸随窗口）。
struct AoGpu {
    /// 32B 参数缓冲（AoParamsRaw；AO 开启帧每帧写——半径/强度/相机投影
    /// 常量 + 远平面天空标记。关闭帧不写：哑纹理路径不读参数）。
    params_buf: wgpu::Buffer,
    /// 1×1 R16Float 哑纹理 / 视图（纹素 1.0 = 中性可见度；探针捕获面 /
    /// RT 相机的绑定组也用它——捕获 / RT 视口无屏幕空间语义，与 SSR 的
    /// 哑资源纪律同款）。
    #[allow(dead_code)]
    dummy: wgpu::Texture,
    dummy_view: wgpu::TextureView,
    /// 半分辨率可见度图 + 视图。`None` = 从未启用（哑纹理占位）。
    target: Option<AoTarget>,
    /// 可见度图当前尺寸（复用判定键）。
    target_size: (u32, u32),
    /// 绑定裁决：`true` = binding 14 指向真实可见度图（AO 本帧开启），
    /// `false` = 指向哑纹理（关闭——即使 target 还在，陈旧内容也不得
    /// 被采样）。draw 逐帧设置，globals 绑定组重建时消费。
    bind_real: bool,
}

/// GTAO 半分辨率可见度图（R16Float；本体保活字段仅供视图有效性依赖）。
struct AoTarget {
    #[allow(dead_code)]
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// tiles 索引缓冲的固定容量（1 MiB）：前 `2 × tile 数` 个 u32 是每 tile
/// 的 [start, count]（ranges 区），其余是灯下标展平区（indices 区）。
/// 1 MiB = 262144 u32：8K×8K 屏的 ranges 区（131072 u32）也放得下，
/// indices 超出剩余容量按序截断（确定性：保留数组序靠前的，见
/// `upload_light_bins`）。
const LIGHT_BINS_TILES_BYTES: u64 = 1024 * 1024;

/// 前向光源分箱（R78）的常驻 GPU 资源。globals 布局的 binding 15/16/17
/// 恒需合法绑定，哑资源（全零参数 / 零灯表 / 零索引，mode = 0 = 着色器
/// 分箱分支死码）**始终存在**（SSR/AO 哑资源同款纪律——缺席路径零分配
/// 大资源、零 write_buffer，命令流与 R77 逐位一致）。
struct LightBinsGpu {
    /// 32B 参数（LightBinsParamsRaw 布局；mode=1 帧每帧写）。
    params_buf: wgpu::Buffer,
    /// 灯表 storage（512 × 80 B 定长；mode=1 帧每帧写）。
    lights_buf: wgpu::Buffer,
    /// tile 索引 storage（固定 1 MiB；mode=1 帧每帧写）。
    tiles_buf: wgpu::Buffer,
    /// 32B 全零参数（捕获 / RT / 关闭帧绑定——mode = 0）。
    off_params_buf: wgpu::Buffer,
    /// 80B 全零灯表（哑占位，永不读——mode = 0 分支死码）。
    off_lights_buf: wgpu::Buffer,
    /// 16B 全零索引（哑占位，永不读）。
    off_tiles_buf: wgpu::Buffer,
    /// 绑定裁决：`true` = binding 15/16/17 指向真实缓冲（本帧分箱生效），
    /// `false` = 指向哑资源（关闭——即使上一帧写过真数据也不得被采样）。
    /// draw 逐帧设置，globals 绑定组重建时消费（AO 的 bind_real 同款）。
    bind_real: bool,
}

/// GI 探针三阶 SH（R83）的常驻 GPU 资源。globals 布局的 binding 18/19 恒需
/// 合法绑定，哑资源（全零探针表 / 全零参数，mode = 0 = 着色器逐片元分支死码）
/// **始终存在**（分箱三路 / SSR / AO 哑资源同款纪律——缺席路径零分配大资源、
/// 零 write_buffer，命令流与 R82 逐位一致）。
struct GiShGpu {
    /// 探针表 storage（[`MAX_IRRADIANCE_PROBES`] × 160 B 定长；mode=1 帧每帧写）。
    probes_buf: wgpu::Buffer,
    /// 16B 参数（mode=1 帧每帧写）。
    params_buf: wgpu::Buffer,
    /// 160B 全零探针表（哑占位，永不读——mode = 0 分支死码）。
    off_probes_buf: wgpu::Buffer,
    /// 16B 全零参数（捕获 / RT / 关闭帧绑定——mode = 0）。
    off_params_buf: wgpu::Buffer,
    /// 绑定裁决：`true` = binding 18/19 指向真实缓冲（本帧逐片元 SH 生效）。
    /// draw 逐帧设置，globals 绑定组重建时消费。
    bind_real: bool,
}

/// DDGI 深度表（R88）的常驻 GPU 资源。globals 布局的 binding 20 恒需合法
/// 绑定：**4 B 全零哑缓冲始终存在**（mode ≤ 1 / 捕获 / RT 帧绑定——mode 2
/// 分支死码永不读），真实的 768 KiB 深度表**惰性创建**（首个 mode = 2 帧；
/// 照 `AoGpu` 的半分辨率可见度图 / `OccGpu` 的查询集先例——缺席路径零分配
/// 大资源、零 write_buffer）。
struct DdgiGpu {
    /// 深度表 storage（[`DDGI_DEPTH_TABLE_BYTES`] = 768 KiB 定长）。
    /// `None` = 从未启用（哑缓冲占位）。
    ///
    /// [`DDGI_DEPTH_TABLE_BYTES`]: crate::ddgi::DDGI_DEPTH_TABLE_BYTES
    depth_buf: Option<wgpu::Buffer>,
    /// 4 B 全零哑缓冲（关闭 / 捕获 / RT 帧绑定——永不读）。
    off_depth_buf: wgpu::Buffer,
    /// 绑定裁决：`true` = binding 20 指向真实深度表（本帧 mode = 2）。
    /// draw 逐帧设置，globals 绑定组重建时消费。
    bind_real: bool,
}

/// DDGI 增量上传的跨帧记账（R88）。
///
/// **偏离声明**：R83 的探针表是「每帧重传」（`upload_gi_probes` 的文档：
/// 烘焙产物、无 generation 字段可比）。DDGI 偏离成**增量上传**，理由三条：
/// ①数据量 19×（40 KiB → 768 KiB）；②DDGI **有** dirty / cursor 变更记账
/// （`DdgiGrid::probe_stamp`），不需要靠重传规避比对成本；③与 README 既有
/// 「实例缓冲跨帧持久：静态场景零上传，动态实体仅传变化段」同纪律。
/// 默认参数下增量 ≈ 6.3 KiB/脏帧（SH 2 × 160 B + 深度 2 × 3072 B）、
/// 稳态 **0 B**。
struct DdgiUploadState {
    /// 已同步的网格结构版本（不等 ⇒ 全量上传 + 重同步 `stamps`）。
    structure: u64,
    /// 逐探针「已上传的重照明戳」（与 `DdgiGrid::probe_stamp` 比对）。
    stamps: Vec<u64>,
    /// 已写的 16B 参数（`None` = 从未写过 ⇒ 首个 mode = 2 帧必写）。
    params: Option<[u32; 4]>,
}

/// 遮挡查询的常驻 GPU 资源（R53，首次启用时惰性创建，见
/// `ensure_occ_gpu`）。查询槽上限 = `wgpu::QUERY_SET_MAX_QUERIES`
/// （4096）：超出部分不查询、恒可见（保守方向，见 occlusion.rs 限制③）。
struct OccGpu {
    /// 遮挡查询集（每帧按实例序领用 0..n 槽，容量固定不重建）。
    query_set: wgpu::QuerySet,
    /// 单位立方体 8 角点（±0.5，静态）。
    corner_buf: wgpu::Buffer,
    /// 立方体 36 索引（u16）。
    index_buf: wgpu::Buffer,
    /// 64B 视图投影矩阵（cascade_layout 同源复用）。
    params_buf: wgpu::Buffer,
    params_bg: wgpu::BindGroup,
    /// 双帧读回环：resolve 缓冲（QUERY_RESOLVE | COPY_SRC）与映射读回
    /// 缓冲（MAP_READ | COPY_DST）成对按帧奇偶交替（读回在途时不覆盖）。
    resolve_bufs: [wgpu::Buffer; 2],
    readback_bufs: [wgpu::Buffer; 2],
    /// 逐实例 AABB 缓冲（容量复用，照实例缓冲模式）。
    aabb_buf: Option<wgpu::Buffer>,
    aabb_capacity: usize,
}

/// 在途的遮挡查询读回（R53）：帧 N 编码期 resolve + copy，本帧提交后
/// `map_async`；帧 N+1 的 `draw` 开头 `poll(Poll)` + `try_recv` 消费。
/// 读回缓冲在消费 unmap 前**不可复用**，因此同一时刻至多一条在途
/// （迟到期间该帧跳过查询与剔除——全部可见，保守方向）。
struct OccInflight {
    /// 读回缓冲槽位（`resolve_bufs` / `readback_bufs` 下标）。
    parity: usize,
    /// 查询槽 ↔ 实体映射（i = 查询索引，值 = 实体 bits；与帧 N 的
    /// AABB 实例缓冲同序——排序后 items 前缀，确定性）。
    entities: Vec<u64>,
    /// 本批查询数。
    count: usize,
    /// 发出帧号（消费时判新鲜：只有上一帧的结果才允许剔除）。
    frame: u64,
    /// `map_async` 完成信道（回调送达映射结果）。
    done: std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
}

/// 与 WGSL `blit_params` 对齐（16 字节，POD）：后处理链共享合成参数。
/// x = 泛光叠加强度（blit 消费）；y = 曝光（blit 在 ACES 之前乘）；
/// z = 暗角强度（vignette pass 消费）；w = 保留（恒写 0.0）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PostParamsRaw {
    bloom_strength: f32,
    exposure: f32,
    vignette_strength: f32,
    _reserved: f32,
}

const _: () = assert!(
    std::mem::size_of::<PostParamsRaw>() == 16,
    "PostParams 必须与 WGSL blit_params 布局一致"
);

/// 与 WGSL `DofParams` 对齐（32 字节，POD）：景深 pass 参数。
/// a = (聚焦距离, 清晰带半宽, 最大模糊半径/纹素, 保留)；
/// b = (1/宽, 1/高, 0, 0)——纹素半径 → UV 偏移的换算。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DofParamsRaw {
    a: [f32; 4],
    b: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<DofParamsRaw>() == 32,
    "DofParams 必须与 WGSL 布局一致"
);

/// 与 WGSL `MbParams` 对齐（240 字节，POD）：运动模糊 pass 参数。
/// 当前帧逆视图投影（深度 → 世界位反投影）+ 上一帧视图投影（重投影）+
/// 标量（速度缩放 / tap 上限 / 深度→NDC z 的两个预计算系数）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MbParamsRaw {
    inv_view_proj: [[f32; 4]; 4],
    prev_view_proj: [[f32; 4]; 4],
    scalars: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<MbParamsRaw>() == 144,
    "MbParams 必须与 WGSL 布局一致"
);

/// 与 WGSL `TaaParams` 对齐（240 字节，POD）：TAA pass 参数（R60）。
/// 当前帧逆视图投影（深度 → 世界位反投影，抖动后的矩阵——与深度副本
/// 同源）+ 上一帧视图投影（历史重投影，即上一帧的实际渲染矩阵）+ 标量
/// （历史权重 / 重置标志 / 深度→NDC z 的两个预计算系数，与 MB 同款）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TaaParamsRaw {
    inv_view_proj: [[f32; 4]; 4],
    prev_view_proj: [[f32; 4]; 4],
    scalars: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<TaaParamsRaw>() == 144,
    "TaaParams 必须与 WGSL 布局一致"
);

/// TAA 历史权重（混合 = current×0.1 + clamped_history×0.9）：收敛速度
/// 与残影长度的经验折中（约 10 帧量级把过期历史衰减到 1/e）。
const TAA_HISTORY_WEIGHT: f32 = 0.9;

/// 与 WGSL `SsrParams` 对齐（96 字节，POD）：SSR 步进参数（R62b）。
/// 上一帧实际渲染矩阵（反射样本投影到上帧屏幕——与 MB/TAA 重投影基准
/// 同源）+ 标量（执行开关 / 强度 / 粗糙度早退阈值 / 边缘衰减带宽 +
/// 最大距离 / 厚度）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SsrParamsRaw {
    prev_view_proj: [[f32; 4]; 4],
    flags: [f32; 4],
    extent: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<SsrParamsRaw>() == 96,
    "SsrParams 必须与 WGSL 布局一致"
);

/// 与 WGSL `AoParams` 对齐（32 字节，POD）：GTAO pass 参数（R77）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AoParamsRaw {
    /// x: 搜索半径（米）, y: 强度指数, z: tan(fov_y/2), w: 宽高比
    scalars: [f32; 4],
    /// x: 远平面（scene_depth 的天空清空值）, y/z/w: 保留
    extra: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<AoParamsRaw>() == 32,
    "AoParams 必须与 WGSL 布局一致"
);

/// 与 WGSL `SkyParams` 对齐（64 字节，POD）：大气散射天空参数（R79，
/// skybox 组 binding 7——专用 `sky_layout`，Globals 并行前缀零触碰）。
/// 缺席 / `SkySettings.atmosphere = false` 时**不写**此缓冲（保持 wgpu
/// 零初始化 = mode 0 死码分支），命令流与 R78 逐位一致。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SkyParamsRaw {
    /// x: mode（0 旧渐变 / env 采样，1 大气解析散射）, y: Rayleigh 强度,
    /// z: Mie 强度, w: 太阳盘 HDR 强度。
    mode_params: [f32; 4],
    /// xyz: 单位太阳方向（世界空间，= −light_dir 归一化）, w: 太阳盘
    /// smoothstep 内阈 cos（0.99985）。
    sun_dir: [f32; 4],
    /// rgb: 太阳辐照色（线性 HDR = light.color × intensity）, w: 太阳盘
    /// smoothstep 外阈 cos（0.99995）。
    sun_color: [f32; 4],
    /// rgb: 地平线以下淡出色, w: 天空整体强度校准（SKY_INTENSITY = 48.0
    /// ——物理量级的散射亮度折算到本仓 HDR 域的常量）。
    ground: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<SkyParamsRaw>() == 64,
    "SkyParams 必须与 WGSL 布局一致（64B）"
);

/// 太阳盘 smoothstep 内阈 cos（约 0.99° 视半径——真实太阳 0.26° 过小，
/// 艺术化放大；外阈 0.99995 给盘缘一圈软过渡）。
const SKY_DISK_COS_INNER: f32 = 0.999_85;

/// 太阳盘 smoothstep 外阈 cos。
const SKY_DISK_COS_OUTER: f32 = 0.999_95;

/// 天空整体强度校准常量（CPU 传入 SkyParams.ground.w）：散射积分沿用
/// 物理量级的系数（β_R ~1e-5/m 量级 ⇒ 天空辐射 ~1e-5 × 太阳辐照），
/// 本仓 HDR 域以方向光强度 1 ≈ 画面白为锚，乘本常量把天顶亮度抬到
/// ~0.3–0.4、地平线 ~2.5（ACES 后的常规白日天空观感）。
const SKY_INTENSITY: f32 = 48.0;

/// 与 WGSL `FogParams` 对齐（160 字节，POD）：体积雾 pass 参数（R79）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FogParamsRaw {
    /// 主相机（proj × view）⁻¹——NDC → 世界重建（与 MB 同矩阵同口径）。
    inv_view_proj: [[f32; 4]; 4],
    /// xyz: 相机世界位, w: 保留。
    eye: [f32; 4],
    /// xyz: 单位太阳方向（= −light_dir 归一化）, w: 保留。
    sun_dir: [f32; 4],
    /// rgb: 太阳辐照色（线性 HDR），w: 保留。
    sun_color: [f32; 4],
    /// x: 零高度消光密度, y: 高度衰减, z: 高度偏移, w: 内散射强度。
    a: [f32; 4],
    /// x: HG 相位 g（钳 ±0.999）, y: 步进距离上限, z: 远平面（深度副本
    /// 天空清空值）, w: a_coef（z_ndc = a − b/d 的 a）。
    b: [f32; 4],
    /// x: b_coef（z_ndc = a − b/d 的 b）, y/z/w: 保留。
    c: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<FogParamsRaw>() == 160,
    "FogParams 必须与 WGSL 布局一致（160B：mat4 + 6×vec4）"
);

/// 与 WGSL `WaterParams` 对齐（240 字节，POD）：平面水体 pass 参数（R81）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct WaterParamsRaw {
    /// 主相机 VP（含 TAA 抖动——与主 pass / scene_depth 同口径）。
    view_proj: [[f32; 4]; 4],
    /// 镜像相机 VP（proj × view × 反射矩阵）——反射捕获图的屏幕投影。
    refl_view_proj: [[f32; 4]; 4],
    /// xyz: 主相机眼点，w: 保留。
    eye: [f32; 4],
    /// xyz: 表面 → 光源单位方向，w: 保留。
    light_dir: [f32; 4],
    /// rgb: 方向光色 × 强度，w: 保留。
    light_color: [f32; 4],
    /// x: 主波振幅, y: 主波波长, z: 相速度, w: 陡度 Q。
    wave_a: [f32; 4],
    /// x: 半边长 X, y: 半边长 Z, z: 波形时间 t（Time.elapsed_secs，缺席
    /// = 0）, w: 吸收特征深度（下限 1e-3 防 ÷0）。
    wave_b: [f32; 4],
    /// rgb: 浅水色（线性 HDR），w: 保留。
    shallow: [f32; 4],
    /// rgb: 深水色（线性 HDR），w: 保留。
    deep: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<WaterParamsRaw>() == 240,
    "WaterParams 必须与 WGSL 布局一致（240B：2×mat4 + 7×vec4）"
);

/// 平面水体（R81）的 GPU 侧资源：反射 / 折射两张**半分辨率**捕获图
/// （Rgba16Float——与探针捕获同格式，可过滤供波纹扰动后的亚纹素采样；
/// 半分辨率的理由 = 反射 / 折射都是低频近似内容，全分辨率带宽翻倍无观
/// 感收益）+ 各自的 Depth32Float 附件 + 水体绑定组 + 每捕获相机一份
/// Globals / 天空绑定组 + 反射收集实例缓冲 + 静态水面网格。按窗口尺寸
/// 惰性创建、尺寸变化整体重建；无水体帧恒 `None`（零开销默认路径）。
struct WaterGpu {
    /// 反射捕获颜色（Rgba16Float 半分辨率）。
    #[allow(dead_code)]
    refl_color: wgpu::Texture,
    refl_view: wgpu::TextureView,
    /// 反射捕获深度（Depth32Float，单采样）。
    #[allow(dead_code)]
    refl_depth: wgpu::Texture,
    refl_depth_view: wgpu::TextureView,
    /// 折射捕获颜色（主相机视角，同格式）。
    #[allow(dead_code)]
    refr_color: wgpu::Texture,
    refr_view: wgpu::TextureView,
    #[allow(dead_code)]
    refr_depth: wgpu::Texture,
    refr_depth_view: wgpu::TextureView,
    /// 水体参数 uniform（WaterParamsRaw，水体 pass 每帧覆写）。
    params_buf: wgpu::Buffer,
    /// 水体绑定组（water_layout：params + 反射 / 折射图 + scene_depth +
    /// 采样器）。scene_depth 视图随尺寸重建，键与 `scene_depth_size`
    /// 同源 ⇒ 本组随 [`Self::ensure_water_targets`] 一并重建。
    bind_group: wgpu::BindGroup,
    /// 反射捕获相机的 Globals 缓冲 + 全局绑定组 + 天空绑定组（探针捕获
    /// 同款最低配组装：SSR / AO / 分箱 / 探针走哑资源——捕获无屏幕语义、
    /// 无探针自反馈）。
    refl_globals_buf: wgpu::Buffer,
    refl_globals_bg: wgpu::BindGroup,
    refl_sky_bg: wgpu::BindGroup,
    /// 折射捕获相机（= 主相机视角）的同一套（仍走哑屏幕资源——折射图
    /// 不吃 AO / SSR / 分箱，与「捕获内容 = 不透明几何 + 天空」一致）。
    refr_globals_buf: wgpu::Buffer,
    refr_globals_bg: wgpu::BindGroup,
    refr_sky_bg: wgpu::BindGroup,
    /// 反射捕获实例缓冲（独立于主实例缓冲——主缓冲带跨帧增量镜像，
    /// 复用会破坏其一致性；捕获每帧全量上传）。
    instance_buf: Option<wgpu::Buffer>,
    instance_capacity: usize,
    /// 静态水面网格（归一化 [−1,1]² 格点，CPU 一次生成 upload；顶点
    /// 着色器乘 half_extent 展开 + Gerstner 位移）。
    grid_vertex_buf: wgpu::Buffer,
    grid_index_buf: wgpu::Buffer,
    grid_index_count: u32,
}

/// 构建 blit 合成绑定组。初建（`ensure_hdr_targets`，源 = 主解析缓冲）
/// 与全分辨率 A/B 槽变体惰性构建（`ensure_full_targets`，源 = 暗角 /
/// DOF / MB 的链尾槽位）共用条目清单，避免两处漂移；泛光纹理恒为半
/// 分辨率 A 槽（泛光链的固定输出槽，见 post.rs `bloom_blur_slots`）。
fn build_blit_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    hdr_view: &wgpu::TextureView,
    bloom_view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    params: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("iris.blit.bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(hdr_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(bloom_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: params.as_entire_binding(),
            },
        ],
    })
}

/// 与 WGSL `BlurParams` 对齐（32 字节，POD）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BloomParamsRaw {
    a: [f32; 4], // 提取：x = 阈值, y = 软膝；模糊：z,w = texel 尺寸
    b: [f32; 4], // 模糊方向步长
}

const _: () = assert!(
    std::mem::size_of::<BloomParamsRaw>() == 32,
    "BlurParams 必须与 WGSL 布局一致"
);

/// 与 WGSL `ui_globals` 对齐（16 字节，POD）：xy = 屏幕尺寸（像素），
/// zw 恒 0（WGSL uniform 地址空间要求根结构体尺寸为 16 的倍数）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UiGlobalsRaw {
    screen: [f32; 2],
    _pad: [f32; 2],
}

const _: () = assert!(
    std::mem::size_of::<UiGlobalsRaw>() == 16,
    "UiGlobals 必须与 WGSL 布局一致"
);

/// 与 WGSL `ParticleCamera` 对齐（32 字节，POD）：粒子 billboard 基向量。
/// right / up 从视图矩阵第 0 / 1 行提取（刚体视图下即相机右 / 上方向），
/// 每帧 `write_buffer`（有粒子时）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ParticleCameraRaw {
    right: [f32; 4],
    up: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<ParticleCameraRaw>() == 32,
    "ParticleCamera 必须与 WGSL 布局一致"
);

/// 粒子 v2 单粒子状态步长（560 B）：3 × vec4 元数据 + 32 × vec4 环形
/// 历史——与 `shaders/particles_v2_sim.wgsl` 的 `SimParticle` 布局对齐
///（影子契约：改 WGSL 必须同步此常量）。
const PARTICLES_V2_STRIDE: u64 = 48 + 16 * crate::particles_v2::PARTICLES_V2_TRAIL_MAX as u64;

/// 粒子 v2 GPU 池（R82）：按发射器实体绑定（世代位含于 to_bits，销毁
/// 重建不撞旧池；销毁后不回收——诚实限制见 particles_v2.rs）。模拟状态
/// 常驻 storage，发射记账在组件；`bound_cursor` 供 `reset` 检测（游标
/// 回退 → 全量清零一次）。
struct ParticleV2Pool {
    /// 绑定的发射器实体位模式。
    entity: u64,
    /// 粒子状态 read-write storage（compute 写、渲染读）。
    particles_buf: wgpu::Buffer,
    /// 模拟 uniform（每份待消费帧一个槽位，按需增长跨帧复用——
    /// write_buffer 与 dispatch 无法在同一 submit 内交错，逐帧槽位
    /// 保证每份帧参数与对应 dispatch 配对）。
    sim_uniforms: Vec<wgpu::Buffer>,
    sim_bgs: Vec<wgpu::BindGroup>,
    /// 拖尾渲染 uniform（每帧每可见发射器写一份）。
    draw_params_buf: wgpu::Buffer,
    draw_bg: wgpu::BindGroup,
    /// 上次见到的发射器游标（reset 检测基准）。
    bound_cursor: u32,
}

/// 粒子 v2 收集项（`draw` 的收集阶段产出；frames = 本帧待消费模拟帧）。
struct ParticleV2Item {
    entity: u64,
    blend: ParticleBlend,
    /// 存活估计（统计口径；绘制跳过判据）。
    alive: u32,
    /// 发射器游标（reset 检测输入）。
    cursor: u32,
    /// 钳制后的拖尾历史点数。
    trail_length: u32,
    /// 可见性（Visibility 自身 + 祖先级联；隐藏只过滤绘制——模拟照常）。
    visible: bool,
    /// 对应池下标（ensure 阶段填）。
    pool: usize,
    /// 本帧待消费模拟帧（排水自组件）。
    frames: Vec<crate::particles_v2::SimParamsRaw>,
    /// 拖尾渲染 uniform（收集时按相机参数现算）。
    draw: crate::particles_v2::DrawParamsRaw,
}

// ---------------------------------------------------------------------------
// meshlet（R85，虚拟几何体 V2）：GPU 资源 + 帧收集结构
// ---------------------------------------------------------------------------

/// meshlet compute 剔除的工作组大小（WGSL `@workgroup_size(64)` 同值；
/// dispatch.x = ceil(实例最大簇数 / 本值)）。
const MESHLET_CULL_WORKGROUP: u32 = 64;

/// 单帧可绘 meshlet 实例（实体）上限：实例表缓冲按此定长，超出部分
/// warn + 跳过（确定性截断——按查询序保留前缀，与点光 uniform 填充的
/// 既有口径一致）。
const MAX_MESHLET_INSTANCES: usize = 256;

/// 回读 staging 尺寸：4 B 存活计数 + 紧凑列表全容量（166 KB 量级分配，
/// 典型帧只读前缀——预算见 meshlet.rs 模块头）。
const MESHLET_STAGING_BYTES: u64 = 4 + (crate::meshlet::MAX_MESHLET_CLUSTERS * 4) as u64;

/// 单个 meshlet 实例的 GPU 条目（176 B POD；与 `meshlet_cull.wgsl` /
/// `meshlet_draw.wgsl` 的 `InstanceEntry` 逐字段对齐——影子契约，guard
/// 单测 `meshlet_draw_shader_interfaces_match_standard` 锁定字段序）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MeshletInstanceEntryRaw {
    /// 模型矩阵（列主序；值直接取自 `InstanceRaw::from_matrix` ⇒ 与常规
    /// 路径的实例属性逐位同源）。
    model: [[f32; 4]; 4],
    /// 法线矩阵（模型矩阵逆转置，每列补零——同上）。
    normal: [[f32; 4]; 3],
    /// x = 顶点 arena 基址、y = 索引 arena 基址、z = 簇元数据 arena 基址、
    /// w = 本实例簇数。
    base_counts: [u32; 4],
    /// x = 本帧全局簇号基（实例前缀和），yzw 保留（0）。
    frame_base: [u32; 4],
    /// 网格包围盒 min（xyz；w 保留）——簇球心归一化基。
    bounds_min: [f32; 4],
    /// 网格包围盒 extent（xyz；w 保留）。
    bounds_extent: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<MeshletInstanceEntryRaw>() == 176,
    "MeshletInstanceEntryRaw 必须为 176 字节（与 WGSL InstanceEntry 布局一致）"
);

/// meshlet 剔除参数（96 B uniform；与 WGSL `CullParams` 逐字段对齐）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MeshletCullParamsRaw {
    /// 本帧主相机 VP（与 Globals.view_proj 同源，剔除与绘制同矩阵）。
    view_proj: [[f32; 4]; 4],
    /// xyz = 相机眼点（背面锥视线原点），w 保留。
    eye: [f32; 4],
    /// x = 本帧实例数、y = 本帧全局簇总数、zw 保留。
    counts: [u32; 4],
}

const _: () = assert!(
    std::mem::size_of::<MeshletCullParamsRaw>() == 96,
    "MeshletCullParamsRaw 必须为 96 字节（与 WGSL CullParams 布局一致）"
);

/// 单个 meshlet 句柄在 GPU arena 的登记位（注册期一次写入，之后不变——
/// 追加式 arena 保证已发布帧的紧凑列表引用恒有效）。
#[derive(Clone, Debug)]
struct MeshletArenaSlot {
    vertex_base: u32,
    index_base: u32,
    cluster_base: u32,
    /// 全部簇数（含层级各层的父簇——arena 排布记账用；帧实例表的簇范围
    /// 由 [`Self::leaf_cluster_count`] 与 LOD 选择决定）。
    cluster_count: u32,
    /// 叶簇数（R86：全叶表 / 容量核算的簇范围 = [cluster_base,
    /// cluster_base + 本值)）。
    leaf_cluster_count: u32,
    /// 簇 DAG（R86；None = 单层网格——全簇即叶，选择不可用）。
    hierarchy: Option<std::sync::Arc<crate::meshlet::MeshletHierarchy>>,
    /// 网格包围盒（min, extent）——剔除端球心反归一化基。
    bounds_min: [f32; 3],
    bounds_extent: [f32; 3],
    /// 逐簇三角形数（u8 直通 `triangle_count`，含各层父簇）——
    /// `triangles_drawn` 统计按 run 区间精确累加用。
    cluster_triangles: Vec<u8>,
    /// 页划分表（R87）：驻留裁决 / 逐页上传 / 驻留感知选择的输入；
    /// `page_base` = 本网格页号的全局基址（注册期累加分配）。
    pages: std::sync::Arc<crate::meshlet_stream::MeshletPageTable>,
    page_base: u32,
    /// 实例 AABB（R87 遮挡整合）：全部簇包围球（归一化球心反归一化 +
    /// 半径）的联合包围盒（网格局部空间）——注册期一次预计算，查询期
    /// 过模型矩阵取保守世界盒（与 R53 的 `world_aabb` 同式）。
    cluster_union: ([f32; 3], [f32; 3]),
}

/// GPU arena 的 CPU 镜像（增长重建时全量重传的源数据；注册期追加）。
#[derive(Default)]
struct MeshletArena {
    /// 句柄号 → 登记位；`None` = 未注册或注册被拒（超容量 warn 跳过）。
    slots: Vec<Option<MeshletArenaSlot>>,
    /// 顶点 storage 词镜像（每顶点 2 × [f32;4]，`vertex_storage_words`）。
    words: Vec<[f32; 4]>,
    /// 索引镜像。
    indices: Vec<u32>,
}

/// meshlet GPU 资源束（首个 [`MeshletInstance`] 在场时惰性创建；之后常驻
/// ——缺席语义 = 不创建、零 dispatch、零回读、零绘制）。
struct MeshletGpu {
    /// 簇元数据 arena（12×u32 词/簇，`MeshletClusterRaw` 48 B 逐字排布，
    /// 容量 [`MAX_MESHLET_CLUSTERS`] 固定）。
    cluster_words_buf: wgpu::Buffer,
    /// 存活标志 / 紧凑载荷 / 紧凑列表（容量同上）/ 存活总数（4 B）。
    /// flags / payload 仅被 compute 绑定组消费（字段本体保活）。
    #[allow(dead_code)]
    flags_buf: wgpu::Buffer,
    #[allow(dead_code)]
    payload_buf: wgpu::Buffer,
    compact_buf: wgpu::Buffer,
    count_buf: wgpu::Buffer,
    /// 剔除参数 uniform（96 B，每帧 write_buffer）。
    cull_params_buf: wgpu::Buffer,
    /// 顶点词 / 索引 arena（注册超出容量时重建 + 全量重传）。
    vertex_buf: wgpu::Buffer,
    index_buf: wgpu::Buffer,
    vertex_capacity_words: u64,
    index_capacity: u64,
    /// 实例表双缓冲（写入槽 = 本帧 compute，读取槽 = 产出当前所绘列表的
    /// 那一帧——紧凑列表与实例表槽位配对，见 draw 的 meshlet 段注释）。
    instance_tables: [wgpu::Buffer; 2],
    /// compute 侧绑定组（instances 槽位随 parity）。
    cull_bgs: [wgpu::BindGroup; 2],
    /// 绘制侧组 2 绑定组（instances / compact 只读，槽位随 parity）。
    draw_bgs: [wgpu::BindGroup; 2],
    /// 回读 staging 双缓冲（4 B 计数 + 紧凑列表全容量；map_async 在途时
    /// 另一槽可用——遮挡读回的成对双缓冲同款）。
    staging: [wgpu::Buffer; 2],
}

/// 在途的 meshlet 紧凑结果读回（遮挡 [`OccInflight`] 同款：帧 N 编码期
/// copy + 提交后 map_async，帧 N+1 的 draw 开头 poll + try_recv 消费；
/// 在途未决时跳过本帧 compute 与 copy，绘制走「上一份有效紧凑列表 /
/// 恒等全量」兜底）。
struct MeshletInflight {
    parity: usize,
    /// 发出帧号（消费时写入 [`MeshletConsumed::frame`]——绘制端只接受
    /// 「恰好上一帧」产出的列表，迟到的列表降级为全量兜底）。
    frame: u64,
    done: std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
}

/// 最近一次成功消费的紧凑结果（绘制裁决与三角形统计的输入）。
struct MeshletConsumed {
    /// 紧凑列表（packed：实例槽 << 16 | 簇局部号），升序 = 绘制序。
    list: Vec<u32>,
    /// 该列表的精确三角形总数（逐簇 triangle_count 累加，消费时按产出
    /// 帧的实例表解析——此后句柄表不再被引用）。
    triangles: u32,
    /// 产出帧号：绘制端要求 `frame + 1 == 当前帧`（紧凑列表与产出帧的
    /// 实例表槽位配对；间隔帧 / 迟到列表一律降级全量兜底——保守方向）。
    frame: u64,
}

/// 本帧 meshlet 绘制计划（收集 + 兜底裁决的产物，贯穿主 pass 与 compute
/// 编码段）。
struct MeshletDrawPlan {
    /// 本帧 draw 的实例数（紧凑回读数或全量兜底数）。
    draw_count: u32,
    /// 精确三角形数（按所绘列表逐簇累加）。
    triangles: u32,
    /// 绘制读取的实例表槽位（兜底帧 = 本帧槽；紧凑帧 = 上帧槽）。
    draw_parity: usize,
    /// compute 剔除写入的实例表槽位（= 本帧槽）。
    cull_parity: usize,
    /// cull dispatch 的 x 维工作组数（ceil(实例最大簇数 / 64)）。
    cull_blocks: u32,
    /// 本帧实例数（cull dispatch 的 y 维）。
    dispatch_instances: u32,
    /// 本帧是否发出 compute + copy（读回在途时跳过——staging 不可覆写）。
    do_compute: bool,
}

/// 单帧 meshlet 收集结果（跨帧复用 scratch）。
#[derive(Default)]
struct MeshletFrame {
    /// 实例表条目（查询序 = 实例槽序；R86 起一个可见实例可拆多条
    /// 「run」条目——每条对应一段选中簇的连续区间；容量截断见 collect
    /// 文档）。
    entries: Vec<MeshletInstanceEntryRaw>,
    /// 实例槽 → mesh 句柄（与 entries 同序；三角形统计解析用）。
    handles: Vec<u32>,
    /// 实例槽 → 本条目的网格局部簇起点（run 起点；与 entries 同序——
    /// 紧凑列表的 `(实例槽, run 内局部号)` 由此还原网格局部簇号）。
    run_starts: Vec<u32>,
    /// 全局簇总数（本帧实例前缀和的尾值 = 全部 run 长度合计；
    /// ≤ [`MAX_MESHLET_CLUSTERS`]）。
    total_clusters: u32,
    /// 流送请求（R87）：（全局页号, 页最小屏幕误差）——理想集 ∪ 可绘
    /// 区间的页；draw 段取走作为下一帧驻留 tick 的输入。
    stream_requests: Vec<(u32, f32)>,
    /// 遮挡查询条目（R87）：视锥可见的 meshlet 实体世界 AABB（含被
    /// 遮挡隐藏者——持续重查是恢复可见的唯一路径）。
    occ_aabbs: Vec<AabbInstanceRaw>,
    /// 与 `occ_aabbs` 同序的实体位模式表。
    occ_entities: Vec<u64>,
    /// 本帧因遮挡隐藏而整帧跳过的 meshlet 实体数（统计口径）。
    occ_hidden_count: u32,
}

/// 本帧 LOD 选择参数（R86）：[`MeshletLodSettings`] 启用时由
/// render_frame 组装；None = 全叶表（R85 行为逐位不变）。
struct MeshletLodFrameParams {
    /// 相机眼点（世界空间）。
    eye: Vec3,
    /// 渲染目标高度（像素）。
    screen_height: f32,
    /// tan(垂直半视场角)。
    tan_half_fov: f32,
    /// 选中阈值（屏幕像素；来自 `MeshletLodSettings.threshold_px`）。
    threshold_px: f32,
}

/// 单帧渲染统计。除注明外均为**主几何口径**（不透明主 pass、贴花 pass、
/// 透明 pass 与粒子 pass，见 ARCHITECTURE §4.16 / §4.17 / §4.20）：UI pass
/// 只计入 `draw_calls` 的单批，阴影 pass（CSM 3 级联各一遍 + 点光每投影灯
/// 6 面，复用不透明实例区间，贴花 / 透明项与粒子不进阴影）与天空 /
/// 后处理 pass 全部不计（投影灯数单列在 `point_shadow_casters`）。
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameStats {
    /// 主几何 draw call 数：普通批每批 1 + 蒙皮每实体 1 + 贴花每纹理批 1 +
    /// 透明 run 批每批 1 + 透明蒙皮每实体 1 + 粒子（additive / alpha 段各至
    /// 多 1）+ UI 顶点非空时 1。
    pub draw_calls: u32,
    /// 主几何提交的实例数（可见实例，普通 + 蒙皮 + 贴花 + 透明普通 +
    /// 透明蒙皮 + 粒子合计）。
    pub instances: u32,
    /// 本帧实例缓冲实际 `write_buffer` 的字节数。0 = 与上一帧逐实例比对
    /// 完全一致、整帧跳过实例上传（静态场景零上传）。粒子与贴花实例走
    /// 独立缓冲、每帧全量上传，**不计入**本字段（见 `particles_drawn`）。
    pub instance_upload_bytes: u64,
    /// 本帧被视锥剔除的实例数（普通 + 蒙皮 + 透明 + 贴花合计），
    /// **开启遮挡剔除（R53）时另加本帧被遮挡剔除的实例数**（单列口径见
    /// [`Self::occlusion_culled`]）。`Visibility::HIDDEN` 的过滤不算剔除
    /// （不进入 `instances` 也不进入本字段）。**粒子不进本口径**：剔除按
    /// 发射器包围球整批裁决，被剔的发射器既不进 `instances` 也不进本
    /// 字段（口径为几何实例，避免混淆，见 ARCHITECTURE §4.17）。
    pub culled_instances: u32,
    /// 本帧被遮挡剔除的实例数（R53，[`OcclusionCulling`] 开启且有上一帧
    /// 查询结果时的主 pass 跳过数；同时计入 `culled_instances`，关闭恒
    /// 0）。被剔实例只是不进入主 pass 绘制——阴影 pass 与场景深度副本
    /// 照常（保守性论证见 `occlusion.rs` 模块文档）。
    pub occlusion_culled: u32,
    /// 主几何实际提交的三角形总数：可见项的网格索引数 / 3 累加（普通批
    /// × 批实例数，蒙皮逐项，贴花每实例 4 三角形 = ±Z 两端盖；透明同口径
    /// 计入；粒子每粒 2 三角形）。**主几何口径**——不含 UI pass 与阴影
    /// pass（阴影 pass 只复用不透明实例区间，贴花 / 透明项与粒子不投影）。
    pub triangles_drawn: u32,
    /// 本帧粒子 pass 提交的粒子实例数（发射器包围球剔除后；已并入
    /// `instances` / `triangles_drawn`，其 draw call 已并入 `draw_calls`）。
    /// 与发射器池状态对账：= 各可见发射器 `live_count()` 之和。
    pub particles_drawn: u32,
    /// 本帧 TAA 是否处于重置透传（R60，观测字段，不进任何排序键）：
    /// 1 = 首帧启用 / 关→开切换 / 分辨率变化 / 显式 `reset_taa()` /
    /// 无几何帧——该帧 TAA 输出逐位等于当前帧（无历史贡献）。关闭恒 0。
    pub taa_reset: u32,
    /// 本帧实际渲染 cubemap 阴影的点光盏数（R61，观测字段，不进任何
    /// 排序键）：= 预算 [`POINT_SHADOW_BUDGET`] 内被选择键选中的
    /// `PointLight.shadows == true` 盏数，0 = 本帧无点光阴影 pass。
    pub point_shadow_casters: u32,
    /// 本帧生效的反射探针数（R62a，观测字段，不进任何排序键）：= 已烘焙
    /// 且在预算 [`MAX_REFLECTION_PROBES`] 内被选择键选中的探针数，0 =
    /// 全部片元走全局 env（与 R61 逐位一致的路径）。
    pub reflection_probes: u32,
    /// 本帧 SSR 是否活跃步进（R62b，观测字段，不进任何排序键）：1 =
    /// SSR 开启且上帧资源有效（片元内实际执行了屏幕空间步进）；0 = 关闭
    /// 或重置帧（首帧启用 / 关→开 / 分辨率变化 / 切镜 / 上帧或本帧无不
    /// 透明几何——该帧输出与 SSR 关闭帧逐位一致）。
    pub ssr_on: u32,
    /// 本帧 GTAO 是否实际执行（R77，观测字段，不进任何排序键）：1 =
    /// `AoSettings` 开启且上一帧深度副本有效（可见度图被真实重算）；0 =
    /// 关闭或重置帧（首帧启用 / 上帧无不透明几何 / 深度副本重建——该帧
    /// 可见度图清成中性 1.0，输出与 AO 关闭帧逐位一致）。
    pub ao_on: u32,
    /// 本帧 meshlet 绘制提交的（紧凑后）实例数（R85，观测字段，不进任何
    /// 排序键）：GPU compute 剔除 + 前缀和稳定紧凑化后的存活簇数——剔除
    /// 有效性可观测（相机背对场景时显著小于簇总数）。**兜底帧 = 全量簇
    /// 数**（读回迟到 / 首帧无上一帧结果 ⇒ 恒等列表全量绘制，像素与剔除
    /// 帧恒等——剔除只删不可见几何）；无 meshlet 实体 = 0。
    pub meshlet_surviving: u32,
    /// 本帧 DDGI 重照明的探针数（R88，观测字段，**不进任何排序键**）：
    /// 取自 [`DdgiGrid::stats`](crate::ddgi::DdgiGrid::stats) 的 `last_updated`。
    /// 0 = 稳态（无脏探针 ⇒ tick 立即 return，CPU 与上传成本双零）或 DDGI
    /// 缺席；`k`（= `DdgiSettings::probes_per_frame`）= 滚动窗口满载。
    /// 收敛进度可由「连续非零帧数 × k」对账（= `⌈n_dirty / k⌉`）。
    pub ddgi_updated: u32,
}

/// 变化实例数超过实例总数的该比例时，放弃碎段增量、回退整段全量上传——
/// 大量碎段 `write_buffer` 的驱动提交开销会超过省下的拷贝字节数。
const INSTANCE_DIFF_FULL_FALLBACK_RATIO: f32 = 0.5;

/// RT（R40）相机数量上限（v1）：小地图 + 监控屏两个典型用例。超出部分
/// `log::warn` 跳过（多视口编排是后续路线图，见 ARCHITECTURE §8.20）。
const RENDER_TARGET_MAX: usize = 2;

/// 计算两帧实例数组之间的连续变化段（实例索引区间，升序且互不相邻）。
///
/// 语义：
/// - 逐实例比对公共前缀（`min(prev.len(), next.len())`），相邻的变化实例
///   合并为一个连续区间；
/// - `next` 比 `prev` 长：新增尾部整体算作一个变化段（空 `prev` + 非空
///   `next` 即返回覆盖整个 `next` 的单区间）；
/// - `prev` 比 `next` 长：缩短部分无需写入 GPU，不产生区间——调用方在
///   实例数不等时本就走全量上传，此分支仅保证函数语义自洽。
///
/// 公开导出（R33）：增量上传决策是「最接近帧预算的 CPU 纯函数路径」之一，
/// criterion 基准（`benches/render_cpu.rs`）与预算测试直接测量它。
pub fn diff_changed_ranges(
    prev: &[InstanceRaw],
    next: &[InstanceRaw],
) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let common = prev.len().min(next.len());
    let mut start: Option<usize> = None;
    for i in 0..common {
        if prev[i] == next[i] {
            if let Some(s) = start.take() {
                ranges.push(s..i);
            }
        } else {
            start.get_or_insert(i);
        }
    }
    if let Some(s) = start.take() {
        ranges.push(s..common);
    }
    if next.len() > prev.len() {
        ranges.push(common..next.len());
    }
    ranges
}

/// 构建全局绑定组。其中的 env 视图随每次环境重烘焙而更换，重烘后必须
/// 重建本绑定组——初建（`Renderer::new`）与重烘重建（`ensure_environment`）
/// 共用此逻辑，避免两处条目清单漂移。级联参数（binding 8）与级联矩阵
/// 缓冲同属固定资源，一并写入。
/// 全局绑定组里与反射探针相关的两路资源（R62a）：参数缓冲（binding 9）+
/// cubemap 数组视图（binding 10）。
///
/// 打成一组传参，一是让 [`build_globals_bind_group`] 的实参数量保持在
/// clippy 的 7 参数阈值内，二是让调用点一眼看出「探针资源是一组」——主视图
/// 传真实资源，探针捕获的每面绑定组传**零参数 + 哑 cubemap**（自反馈说明
/// 见 [`Renderer::bake_reflection_probe`]）。
///
/// R83 起本组再携带 GI 探针 SH 的两路（binding 18 = 探针表 storage、
/// 19 = 16B 参数）：主视图按 `gi_sh.bind_real` 在真 / 哑之间裁决，捕获面与
/// RT 相机恒传哑资源（mode = 0 ⇒ 逐片元 SH 分支死码，与「捕获不渲染锥光与
/// 第 9+ 盏点光」同一立场——见 §8.47③ / §8.52）。
/// R88 追加第三路（binding 20 = DDGI 深度表 storage）：按 `ddgi.bind_real`
/// 裁决，捕获面 / RT 相机同样恒传哑缓冲（mode = 0，§8.52⑦ 同款立场）。
#[derive(Clone, Copy)]
struct ProbeGlobalsResources<'a> {
    params_buf: &'a wgpu::Buffer,
    cube_view: &'a wgpu::TextureView,
    /// GI 探针表 storage（binding 18）。
    gi_probes: &'a wgpu::Buffer,
    /// GI 探针参数 uniform（binding 19）：主相机 mode ≥ 1 走真缓冲，其余哑零。
    gi_params: &'a wgpu::Buffer,
    /// DDGI 深度表 storage（binding 20，R88）：主相机 mode = 2 走真表，
    /// 其余（mode ≤ 1 / 捕获 / RT）走 4 B 全零哑缓冲。
    gi_depth: &'a wgpu::Buffer,
}

/// 全局绑定组里的两张阴影贴图视图：方向光级联数组（binding 1）与点光
/// cubemap 数组（binding 7）。同样打成一组传参（保持实参数量在 clippy
/// 阈值内，且「阴影贴图是一组」在调用点一眼可见）。
#[derive(Clone, Copy)]
struct ShadowMapViews<'a> {
    cascades: &'a wgpu::TextureView,
    point_cube: &'a wgpu::TextureView,
}

/// 全局绑定组里的两路 uniform 缓冲：主 Globals（binding 0）+ 级联参数
/// （binding 8）。打包成组与 [`ProbeGlobalsResources`] / [`ShadowMapViews`]
/// 同款纪律——`build_globals_bind_group` 的实参数量保持在 clippy 的
/// 7 参数阈值内（R62b 加入 SSR 三路资源后 grouping 是必需而非风格）。
#[derive(Clone, Copy)]
struct GlobalsBuffers<'a> {
    globals: &'a wgpu::Buffer,
    csm_params: &'a wgpu::Buffer,
}

/// 全局绑定组里的屏幕空间效果资源（R62b 立组，R77 携带 AO 一路，R78
/// 再携带光源分箱三路）：SSR 参数缓冲（binding 13）+ 上帧颜色副本视图
/// （binding 11）+ 上帧深度副本视图（binding 12）+ GTAO 可见度图视图
/// （binding 14）+ 分箱参数 / 灯表 / tile 索引缓冲（binding 15/16/17）。
/// 主视图传真实资源；探针捕获面 / RT 相机的绑定组传**零参数 + 哑纹理 /
/// 哑缓冲**（捕获 / RT 视口不做屏幕空间效果、不渲染锥光与第 9+ 盏点光
/// ——上帧 / 屏幕坐标语义只对主相机成立，见 §4.31 裁决 2；AO 哑纹素恒
/// 1.0，bins 哑参数 mode = 0 = 分支死码）。
#[derive(Clone, Copy)]
struct ScreenSpaceGlobalsResources<'a> {
    params_buf: &'a wgpu::Buffer,
    color_view: &'a wgpu::TextureView,
    depth_view: &'a wgpu::TextureView,
    ao_view: &'a wgpu::TextureView,
    /// 分箱参数 uniform（binding 15）：主相机 mode=1 走真缓冲，其余哑零。
    bins_params: &'a wgpu::Buffer,
    /// 分箱灯表 storage（binding 16）。
    bins_lights: &'a wgpu::Buffer,
    /// 分箱 tile 索引 storage（binding 17）。
    bins_tiles: &'a wgpu::Buffer,
}

fn build_globals_bind_group(
    gpu: &Gpu,
    bundle: &crate::pipeline::PipelineBundle,
    buffers: GlobalsBuffers<'_>,
    shadows: ShadowMapViews<'_>,
    env: &crate::environment::EnvironmentMaps,
    probe: ProbeGlobalsResources<'_>,
    screen: ScreenSpaceGlobalsResources<'_>,
) -> wgpu::BindGroup {
    let globals_buf = buffers.globals;
    let csm_params_buf = buffers.csm_params;
    let env_sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("iris.env.sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    let shadow_sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("iris.shadow.sampler"),
        // 非过滤采样：R32Float 的双线性过滤需要 float32-filterable 扩展，不依赖；
        // 深度比较在 shader 内手工完成（standard.wgsl shadow_factor）
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        ..Default::default()
    });
    gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("iris.globals.bg"),
        layout: &bundle.globals_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(shadows.cascades),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&shadow_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&env.irradiance_view),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(&env.prefiltered_view),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(&env.brdf_view),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::Sampler(&env_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 7,
                resource: wgpu::BindingResource::TextureView(shadows.point_cube),
            },
            wgpu::BindGroupEntry {
                binding: 8,
                resource: csm_params_buf.as_entire_binding(),
            },
            // 反射探针（R62a）：参数 + cubemap 数组（采样器复用 binding 6）
            wgpu::BindGroupEntry {
                binding: 9,
                resource: probe.params_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 10,
                resource: wgpu::BindingResource::TextureView(probe.cube_view),
            },
            // SSR 上帧资源（R62b）：上帧颜色副本 + 上帧深度副本 + 参数
            // （采样器复用 binding 6 的 Filtering 档——Rgba16Float 可过滤；
            // 深度为 textureLoad 读取，不需要采样器）
            wgpu::BindGroupEntry {
                binding: 11,
                resource: wgpu::BindingResource::TextureView(screen.color_view),
            },
            wgpu::BindGroupEntry {
                binding: 12,
                resource: wgpu::BindingResource::TextureView(screen.depth_view),
            },
            wgpu::BindGroupEntry {
                binding: 13,
                resource: screen.params_buf.as_entire_binding(),
            },
            // GTAO 可见度图（R77）：真实半分辨率图或 1×1 哑纹理（纹素
            // 1.0——关闭 / 捕获 / RT 路径采样后 × 1.0 逐位精确）
            wgpu::BindGroupEntry {
                binding: 14,
                resource: wgpu::BindingResource::TextureView(screen.ao_view),
            },
            // 前向光源分箱（R78）：参数 + 灯表 + tile 索引（主相机 mode=1
            // 走真缓冲；捕获 / RT / 关闭帧走全零哑缓冲——mode = 0 使着色器
            // 分箱分支死码，Globals 均摊旧路径逐位不变）
            wgpu::BindGroupEntry {
                binding: 15,
                resource: screen.bins_params.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 16,
                resource: screen.bins_lights.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 17,
                resource: screen.bins_tiles.as_entire_binding(),
            },
            // GI 探针三阶 SH（R83）：探针表 + 16B 参数（主相机 mode ≥ 1 走真
            // 缓冲；捕获 / RT / 无 SH 探针集帧走全零哑缓冲——mode = 0 使着色器
            // 逐片元分支死码，loc14 逐实体路径逐位不变）
            wgpu::BindGroupEntry {
                binding: 18,
                resource: probe.gi_probes.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 19,
                resource: probe.gi_params.as_entire_binding(),
            },
            // DDGI 深度表（R88）：主相机 mode = 2 走 768 KiB 真表（惰性创建），
            // 其余走 4 B 全零哑缓冲（mode ≤ 1 ⇒ ddgi_irradiance 分支死码）
            wgpu::BindGroupEntry {
                binding: 20,
                resource: probe.gi_depth.as_entire_binding(),
            },
        ],
    })
}

/// 天空绑定组（R79）的共享要素：Globals 缓冲 + 预滤波环境视图 + SkyParams
/// 缓冲。主相机（`globals_buf`）与六个捕获面（`probe_face_globals_bufs`）
/// 共用本清单——天空绘制点全部经 `build_sky_bind_group` 创建绑定组，
/// 避免条目清单漂移。
struct SkyBindResources<'a> {
    globals: &'a wgpu::Buffer,
    sky_params: &'a wgpu::Buffer,
}

/// 构建天空绑定组（R79）：`sky_layout` 的四个 binding（0 = Globals
/// uniform、4 = 预滤波环境立方体贴图、6 = 过滤采样器、7 = SkyParams）。
/// 采样器描述与 `build_globals_bind_group` 的 env 采样器逐字段一致
///（ClampToEdge + Linear + Nearest mipmap）——mode = 0 路径采样行为与
/// 既有 globals 组逐位一致（缺席帧 FNV 基线的保证）。
fn build_sky_bind_group(
    gpu: &Gpu,
    bundle: &crate::pipeline::PipelineBundle,
    env: &crate::environment::EnvironmentMaps,
    resources: SkyBindResources<'_>,
) -> wgpu::BindGroup {
    let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("iris.sky.env_sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("iris.sky.bg"),
        layout: &bundle.sky_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: resources.globals.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(&env.prefiltered_view),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 7,
                resource: resources.sky_params.as_entire_binding(),
            },
        ],
    })
}

/// `collect_instances` 的收集结果（普通 + 蒙皮 + 透明合计）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CollectCounts {
    /// 进入本帧绘制的实例数（四个列表长度合计）。
    visible: usize,
    /// 被视锥剔除的实例数。
    culled: u32,
}

/// 材质的透明模式（目录缺席按 `Opaque`——0 号默认材质即不透明）。
fn alpha_of(materials: &MaterialStore, handle: MaterialHandle) -> AlphaMode {
    materials
        .get(handle)
        .map(|def| def.alpha_mode)
        .unwrap_or(AlphaMode::Opaque)
}

/// 材质的双面标志（R66d，目录缺席按 `false` = 单面背面剔除）。
fn double_sided_of(materials: &MaterialStore, handle: MaterialHandle) -> bool {
    materials
        .get(handle)
        .map(|def| def.double_sided)
        .unwrap_or(false)
}

/// LOD 网格解析（R30 / R51；C1 升级 v2）：挂 [`LodMesh3`](crate::terrain::LodMesh3) 的实体按距离在 3 级间
/// 选（地形 chunk，见 `terrain.rs`）；否则挂 [`LodMesh`] 的实体按距离
/// 选 high / low——**`lod_v2 = None`（世界无 `LodSettings`）= v1 口径
/// 逐位不变**（实体原点到 eye 的欧氏距离、单阈值）；带设置时主相机收集
/// 走双阈值滞回 + 可选包围球中心（`LodMesh::select_hysteresis`，§8.16
/// 闭合），记忆经 `lod_memory` 读写（辅助上下文 RT / 捕获传 `None` =
/// 无历史 = 当帧单阈值，见 `LodSettings` 文档）；未挂载回退 `MeshHandle`
/// 旧语义；皆无返回 `None`（收集跳过该实体——查询带多个 `Option` 后
/// 「无任何网格句柄」成为合法查询命中）。
#[allow(clippy::too_many_arguments)]
fn resolve_mesh_handle(
    lod: Option<&LodMesh>,
    lod3: Option<&crate::terrain::LodMesh3>,
    mesh: Option<&MeshHandle>,
    eye: Vec3,
    origin: Vec3,
    lod_v2: Option<LodSettings>,
    mut lod_memory: Option<&mut std::collections::BTreeMap<u64, bool>>,
    mesh_store: &MeshStore,
    model: Mat4,
    entity_bits: u64,
) -> Option<MeshHandle> {
    match (lod3, lod, mesh) {
        (Some(lod3), ..) => Some(lod3.select(eye, origin)),
        (None, Some(lod), _) => match lod_v2 {
            None => Some(lod.select(eye, origin)),
            Some(settings) => {
                // 距离参照：包围球中心（high 网格 CPU bounds，缺席回退
                // 原点 = v1 保守口径）或实体原点（v1 口径）。中心只对
                // high 取——两个参照系共用一个点，避免选中档位反馈进
                // 距离计算造成自激振荡。
                let point = if settings.bounds_center {
                    mesh_store
                        .get(lod.high)
                        .and_then(|data| data.bounds())
                        .map(|(min, max)| {
                            model.transform_point3((Vec3::from(min) + Vec3::from(max)) * 0.5)
                        })
                        .unwrap_or(origin)
                } else {
                    origin
                };
                let dist = eye.distance(point);
                let prev = lod_memory
                    .as_ref()
                    .and_then(|m| m.get(&entity_bits).copied());
                let (handle, high) = lod.select_hysteresis(dist, prev, settings.hysteresis);
                if let Some(m) = &mut lod_memory {
                    m.insert(entity_bits, high);
                }
                Some(handle)
            }
        },
        (None, None, Some(mesh)) => Some(*mesh),
        (None, None, None) => None,
    }
}

/// 透明排序视深：网格包围球中心（无 bounds 时回退模型平移列）经视图
/// 矩阵变换后的 -z（相机前方为正，值越大离相机越远）。包围球中心是
/// 排序启发式——逐像素排序不可行，中心视深对凸体与薄板是精确序。
fn view_depth(view: &Mat4, mesh_store: &MeshStore, mesh: MeshHandle, model: Mat4) -> f32 {
    let center_world = match mesh_store.get(mesh).and_then(|data| data.bounds()) {
        Some((min, max)) => model.transform_point3((Vec3::from(min) + Vec3::from(max)) * 0.5),
        // 无 CPU 包围盒数据：退回模型平移列（实例原点的视深）
        None => model.w_axis.truncate(),
    };
    -view.transform_point3(center_world).z
}

/// 蒙皮几何的保守世界 AABB（R22）：绑定姿势 AABB 的 8 个角点经**每个**
/// 关节矩阵 Jᵢ 变换到当前网格局部、再经实例 model 矩阵到世界
/// （world = model·Jᵢ·corner）后的并集。`None` = 网格无 CPU 数据 /
/// 空包围盒（不剔除、视深退回模型平移列）。
///
/// 数学正确性（保守性保证）：LBS 顶点 v′ = model·(Σᵢ wᵢ·Jᵢ·v)，权重
/// 非负且和为 1（加载时归一化）——蒙皮后的顶点落在各刚体变换结果
/// { Jᵢ·v } 的**凸组合**内；凸组合必属于各结果的凸包，而凸包又含于
/// 「逐关节变换后 8 角点并集的 AABB」（Jᵢ 仿射，绑定 AABB 的像必落在
/// 其变换后角点的包围盒内）。故该世界 AABB 是蒙皮几何的**保守上界**：
/// 据此剔除只可能多画、不可能漏画。误用绑定姿势包围盒会在关节把几何
/// 移出原位时误杀（回归测试
/// `culling_render.rs::skinned_culling_tracks_joint_transformed_geometry`）。
/// 规模：每蒙皮实例额外 关节数 × 8 次点变换 + 一次 AABB 相交测试
/// （Arena 15 只蠕虫 × ~5 关节量级，CPU 开销可忽略）。
fn skin_world_aabb(
    mesh_store: &MeshStore,
    mesh: MeshHandle,
    model: &Mat4,
    joints: &[glam::Mat4],
) -> Option<(Vec3, Vec3)> {
    let (min, max) = mesh_store.get(mesh).and_then(|data| data.bounds())?;
    let min = Vec3::from(min);
    let max = Vec3::from(max);
    let mut world_min = Vec3::splat(f32::INFINITY);
    let mut world_max = Vec3::splat(f32::NEG_INFINITY);
    for corner in 0..8u32 {
        let corner = Vec3::new(
            if corner & 1 != 0 { max.x } else { min.x },
            if corner & 2 != 0 { max.y } else { min.y },
            if corner & 4 != 0 { max.z } else { min.z },
        );
        for joint in joints {
            let world = model.transform_point3(joint.transform_point3(corner));
            world_min = world_min.min(world);
            world_max = world_max.max(world);
        }
    }
    Some((world_min, world_max))
}

/// 收集本帧待绘制实例，并按材质 `alpha_mode` 分区：
/// - 不透明项 → `items`（(网格, 材质) 键排序成批）与 `skinned`（每实体
///   一项）——语义与透明支持之前完全一致；
/// - 透明项（`AlphaMode::Blend`）→ `transparent` / `transparent_skinned`，
///   由调用方按**视深降序（远 → 近）**排序后在独立透明 pass 绘制。
///
/// **透明不进阴影**：方向光与点光阴影 pass 只遍历 `items` / `skinned`，
/// 收集期分区天然排除透明项——半透明几何不应在地面上投出实心黑影
/// （alpha 混合几何的阴影贡献需要分离投影，v1 不做）。
///
/// [`crate::visibility::Visibility`] 过滤在全部收集路径生效（含层级
/// 级联：任一祖先 `HIDDEN` ⇒ 子树全隐藏，见 `hidden_by_ancestor`——自身
/// `HIDDEN` 也跳过，组件缺席 = 可见的旧语义不变）；普通实体做包围球视锥
/// 剔除；蒙皮实体做**保守并集 AABB 视锥剔除**（见
/// `skin_world_aabb`）。视锥剔除与剔除计数对透明同样生效。返回可见 /
/// 被剔除的实例数（日志与 `FrameStats` 用）；排序由调用方完成。
///
/// **距离 LOD（R30 / R51）**：网格句柄经 `resolve_mesh_handle` 解析——挂
/// [`LodMesh3`](crate::terrain::LodMesh3) 的实体按距离在 3 级间选（地形 chunk，仅普通网格路径，
/// 见 `terrain.rs`），挂 [`LodMesh`] 的实体按「实体原点到 `eye` 的欧氏
/// 距离」选 high / low，未挂载用 `MeshHandle` 旧语义；选中的句柄决定
/// 合批键 / 剔除包围体 / 三角形统计（蒙皮路径同口径，LOD 高低模各自
/// 带蒙皮属性）。`eye` 由调用方从解析后的相机传入（资源相机 / 实体挂载
/// 相机同源）。
/// **光照探针（R64 / R83）**：世界带 [`IrradianceProbeSet`] 资源时，对**无光照
/// 贴图**的实体按实体根 [`GlobalTransform`] 位置求一次间接辐照，结果写进实例
/// 载荷 `InstanceRaw::probe`（xyz = E/π、w = 标志位）；资源缺席 / 空集 /
/// 实体挂图集 ⇒ 载荷恒零（着色器走原全局 env 路径，逐位不变）。
/// 不带 SH 的集 = v1 的四面体重心插值（逐实体）；**带 SH 的集（R83）**载荷
/// 改由 SH 的零阶项按径向权重给出，且这份 xyz 只被 mode = 0 的路径（探针
/// 捕获 / RT 相机）消费——主视图在 mode = 1 时逐片元重新求值，只读标志位。
/// **DDGI（R88）在场时优先**：`DdgiGrid` 非空 ⇒ 载荷改取 DDGI 的零阶径向
/// 混合（同口径），烘焙探针集被忽略（互斥，裁定 2）；主视图走 mode = 2 的
/// 逐片元深度感知混合，同样只读标志位。
/// 求值点选择的完整论证见 [`crate::irradiance`] 模块文档。
/// **光照贴图解析（R52c）**：挂 [`LightmapBinding`] 的实体取其图集句柄，
/// 但只在三个条件齐备时生效——世界有 [`LightmapStore`] 资源、句柄在目录内、
/// 解析出的网格带 UV2（[`MeshData::uv2`](crate::mesh::MeshData)）；任一不
/// 满足即退回 [`NO_LIGHTMAP`]（材质掩码 0 = 原 IBL 路径）并告警。查询元组
/// 已满 7 项（`iris_core::query` 上限），故绑定表用独立一遍查询收集成
/// `BTreeMap`，主查询闭包按实体位查表（捕获 `&BTreeMap`，不借 World 存储
/// ——AGENTS.md 的两阶段规约）。透明项不解析（透明路径不采样光照贴图，
/// v1 限制见 `lightmap.rs` 模块文档）。
// 四个输出列表各自类型不同、语义独立（跨帧复用的 scratch 缓冲逐个传入），
// 收拢成结构体反而多一层拆装；参数多但全为引用/裸缓冲，与 scene.rs
// `build_entity_section` 同理豁免。
#[allow(clippy::too_many_arguments)]
fn collect_instances(
    world: &iris_core::world::World,
    mesh_store: &MeshStore,
    material_store: &MaterialStore,
    view: &Mat4,
    eye: Vec3,
    frustum: &Frustum,
    items: &mut Vec<DrawItem>,
    skinned: &mut Vec<SkinnedItem>,
    transparent: &mut Vec<TransparentItem>,
    transparent_skinned: &mut Vec<TransparentSkinnedItem>,
    lod_v2: Option<LodSettings>,
    mut lod_memory: Option<&mut std::collections::BTreeMap<u64, bool>>,
) -> CollectCounts {
    items.clear();
    skinned.clear();
    transparent.clear();
    transparent_skinned.clear();
    let mut culled = 0u32;
    // 光照贴图绑定表（实体位 → 图集句柄值）+ 解析闭包（口径见函数文档）
    let lightmaps = world.try_resource::<LightmapStore>();
    let mut bindings: std::collections::BTreeMap<u64, u32> = std::collections::BTreeMap::new();
    world.query::<(Entity, &LightmapBinding)>(|(e, binding)| {
        bindings.insert(e.to_bits(), binding.lightmap.0);
    });
    let resolve_lightmap = |entity: u64, mesh: MeshHandle| -> u32 {
        let Some(&handle) = bindings.get(&entity) else {
            return NO_LIGHTMAP;
        };
        let Some(store) = lightmaps.as_deref() else {
            log::warn!(
                "实体 {entity:#x} 挂了光照贴图绑定但世界缺 LightmapStore 资源，按无光照贴图处理"
            );
            return NO_LIGHTMAP;
        };
        if store.get(LightmapHandle(handle)).is_none() {
            log::warn!("光照贴图句柄 {handle} 不在 LightmapStore，按无光照贴图处理");
            return NO_LIGHTMAP;
        }
        if !mesh_store.get(mesh).is_some_and(|data| data.uv2.is_some()) {
            log::warn!("网格 {mesh:?} 无 UV2（光照贴图 UV 未注入），按无光照贴图处理");
            return NO_LIGHTMAP;
        }
        handle
    };
    // 光照探针集（R64 / R83）：CPU 侧逐实体求值 → 实例载荷（E/π + 标志位）。
    // 资源缺席 / 空集 ⇒ 恒 [0; 4]（着色器 select 取 env 采样值的同一串位）。
    // **合成语义裁决 1/5（R89 定稿，原「未修订」）——挂图集实体 × DDGI =
    // 互斥，贴图赢**：挂图集的实体**不发**探针标志位（下方 `lightmap !=
    // NO_LIGHTMAP` 早退），DDGI 分支在同一守卫之后 ⇒ 图集实体完全吃不到
    // DDGI。物理依据：贴图已含 `sky + bounce`（bounce 里带方向光弹射，
    // bake.rs），与 DDGI 承载同一份间接漫反射贡献，相加即双计。同一空间
    // 两套静态/动态 GI **二选一**；差值法（贴图静态 + DDGI 动态增量）已
    // 裁定拒绝（R89 设计决策 9：需要持久化基线表 + 二次重照明对照 +
    // 负增量钳位 + 「贴图与网格参数严格同源」的脆弱契约，收益可由美术
    // 流程自行规避——一个关卡一套 GI）。语义权威：ARCHITECTURE §4.45。
    // 带 SH 的集由 `instance_payload` 内部改走 SH 零阶口径（R83，见
    // irradiance.rs）。
    // **合成语义裁决 2/5（R88 裁定 2）——DDGI 优先**：`DdgiGrid` 非空 ⇒
    // 载荷改取 DDGI 的零阶径向混合（与 R83 带 SH 的集**同口径**，见
    // `DdgiGrid::instance_payload`），烘焙探针集被忽略（互斥告警在 draw
    // 的三态裁决段发一次）。这份 xyz 只被 mode = 0 的路径消费（合成语义
    // 裁决 4/5：探针捕获 / RT 相机恒 mode 0），主视图在 mode = 2 时只读
    // 它的标志位。
    let ddgi = world.try_resource::<crate::ddgi::DdgiGrid>();
    let probes = world.try_resource::<IrradianceProbeSet>();
    let probe_payload = |position: Vec3, lightmap: u32| -> [f32; 4] {
        if lightmap != NO_LIGHTMAP {
            return [0.0; 4];
        }
        if let Some(grid) = ddgi.as_deref().filter(|grid| !grid.is_empty()) {
            return grid.instance_payload(position);
        }
        match probes.as_deref() {
            Some(set) => set.instance_payload(position),
            None => [0.0; 4],
        }
    };
    // 全部蒙皮实体位标识（含被剔除的）：普通路径据此跳过，避免同实体
    // 被两条路径重复处理（剔除计数 / 绘制都只算一次）
    let mut skinned_entities: std::collections::HashSet<u64> = std::collections::HashSet::new();

    // 蒙皮实体独立收集：关节矩阵在此即时计算（J = inv(mesh) ×
    // global(joint) × ibm，蒙皮定义在网格局部空间，随父级整体移动）。
    // 视锥剔除用「关节矩阵变换后的角点并集 AABB」（保守界，见
    // `skin_world_aabb` 的凸组合论证）；网格无 CPU 数据 / 空包围盒时
    // 退化为不剔除（宁可多画）。透明蒙皮的视深取该并集 AABB 的中心
    // （跟随骨骼实际位移，比绑定姿势中心更接近真实视深）。
    world.query::<(
        iris_core::prelude::Entity,
        &GlobalTransform,
        &MaterialHandle,
        Option<&MeshHandle>,
        Option<&LodMesh>,
        &Skin,
        Option<&Visibility>,
        Option<&crate::morph::MorphWeights>,
    )>(|(e, gt, mat, mesh, lod, skin, visibility, morph_weights)| {
        // 自身 HIDDEN 跳过 + 祖先链级联（任一祖先隐藏 ⇒ 子树全隐藏）
        if visibility.is_some_and(|v| !v.visible) || hidden_by_ancestor(world, e) {
            return;
        }
        // LOD 距离选择：挂 LodMesh 按距离选 high / low，否则 MeshHandle；
        // 两者皆无（纯 Skin 实体，合法但不可渲染）静默跳过。
        // 蒙皮路径不参与 LodMesh3（地形 chunk 恒非蒙皮，R51）
        let Some(m) = resolve_mesh_handle(
            lod,
            None,
            mesh,
            eye,
            gt.position(),
            lod_v2,
            lod_memory.as_deref_mut(),
            mesh_store,
            gt.matrix(),
            e.to_bits(),
        ) else {
            return;
        };
        skinned_entities.insert(e.to_bits());
        let mut joint_matrices = Vec::with_capacity(skin.joints.len());
        skin.joint_matrices(world, gt.matrix(), &mut joint_matrices);
        let model = gt.matrix();
        let world_aabb = skin_world_aabb(mesh_store, m, &model, &joint_matrices);
        if world_aabb.is_some_and(|(min, max)| !frustum.intersects_aabb(min, max)) {
            culled += 1;
            return;
        }
        let lightmap = resolve_lightmap(e.to_bits(), m);
        let item = SkinnedItem {
            skin_entity: e.to_bits(),
            mesh: m.0,
            material: mat.0,
            lightmap,
            double_sided: double_sided_of(material_store, *mat),
            instance: InstanceRaw::from_matrix(model)
                .with_probe(probe_payload(gt.position(), lightmap)),
            joint_matrices,
            morph_weights: morph_weights.map(|w| w.weights.clone()),
            vertex_override: None,
        };
        if alpha_of(material_store, *mat) == AlphaMode::Blend {
            let center_world =
                world_aabb.map_or(model.w_axis.truncate(), |(min, max)| (min + max) * 0.5);
            transparent_skinned.push(TransparentSkinnedItem {
                depth: -view.transform_point3(center_world).z,
                skinned: item,
            });
        } else {
            skinned.push(item);
        }
    });

    world.query::<(
        iris_core::prelude::Entity,
        &GlobalTransform,
        &MaterialHandle,
        Option<&MeshHandle>,
        Option<&LodMesh>,
        Option<&crate::terrain::LodMesh3>,
        Option<&Visibility>,
    )>(|(e, gt, mat, mesh, lod, lod3, visibility)| {
        // 自身 HIDDEN 跳过 + 祖先链级联（与蒙皮路径同语义）
        if visibility.is_some_and(|v| !v.visible) || hidden_by_ancestor(world, e) {
            return;
        }
        // 蒙皮实体走独立路径，避免同网格重复绘制
        if skinned_entities.contains(&e.to_bits()) {
            return;
        }
        // LOD 距离选择（组件缺席 = MeshHandle 旧语义；皆无跳过——
        // 挂了材质却没有任何网格句柄的实体不可渲染）
        let Some(m) = resolve_mesh_handle(
            lod,
            lod3,
            mesh,
            eye,
            gt.position(),
            lod_v2,
            lod_memory.as_deref_mut(),
            mesh_store,
            gt.matrix(),
            e.to_bits(),
        ) else {
            return;
        };
        let model = gt.matrix();
        // 包围球剔除：局部球心经模型矩阵变换，半径按最大列缩放保守放大
        //（按 LOD 选中的网格算，高低模包围体差异直接进入剔除边界）
        let culled_now = mesh_store
            .get(m)
            .and_then(|data| data.bounds())
            .is_some_and(|(min, max)| {
                let min = Vec3::from(min);
                let max = Vec3::from(max);
                let center_local = (min + max) * 0.5;
                let radius_local = (max - min).length() * 0.5;
                let center_world = model.transform_point3(center_local);
                let scale = model
                    .x_axis
                    .truncate()
                    .length()
                    .max(model.y_axis.truncate().length())
                    .max(model.z_axis.truncate().length());
                !frustum.intersects_sphere(center_world, radius_local * scale)
            });
        if culled_now {
            culled += 1;
            return;
        }
        let key = ((m.0 as u64) << 32) | mat.0 as u64;
        if alpha_of(material_store, *mat) == AlphaMode::Blend {
            // 透明项不解析光照贴图（v1 限制，见 lightmap.rs 模块文档）⇒ 探针
            // 直接适用（透明实体同样无贴图，吃的是 env 漫反射项）
            transparent.push(TransparentItem {
                key,
                depth: view_depth(view, mesh_store, m, model),
                entity: e.to_bits(),
                double_sided: double_sided_of(material_store, *mat),
                instance: InstanceRaw::from_matrix(model)
                    .with_probe(probe_payload(gt.position(), NO_LIGHTMAP)),
            });
        } else {
            let lightmap = resolve_lightmap(e.to_bits(), m);
            items.push(DrawItem {
                key,
                instance: InstanceRaw::from_matrix(model)
                    .with_probe(probe_payload(gt.position(), lightmap)),
                lightmap,
                entity: e.to_bits(),
                double_sided: double_sided_of(material_store, *mat),
            });
        }
    });
    CollectCounts {
        visible: items.len() + skinned.len() + transparent.len() + transparent_skinned.len(),
        culled,
    }
}

/// 材质绑定组的复合缓存键（R52c）：`(材质句柄 << 32) | 图集槽位`。
///
/// 图集槽位 = 句柄值 + 1（回绕），故 [`NO_LIGHTMAP`] 映射到槽位 0——
/// 无光照贴图时键的高 32 位是材质、低 32 位恒 0，与 R52b 的 `u32` 材质键
/// 一一对应（同一材质配多张图集时各占一条，绑定组内含掩码与图集纹理）。
fn material_key(material: u32, lightmap: u32) -> u64 {
    ((material as u64) << 32) | lightmap.wrapping_add(1) as u64
}

/// 批段长度（R52c）：从 `start` 起「同 (网格, 材质) 键**且**同图集」的
/// 连续实例数。
///
/// 排序键只含 (网格, 材质)——不含图集，故无光照贴图场景的排序结果与批
/// 划分与 R52b 逐位一致（兼容硬约束）；同批内图集交替时切成多段（每段
/// 一次 draw + 一次组 1 绑定）。段区间在实例缓冲里天然连续（排序后同键
/// 相邻），`first..last` 直接可用。
fn run_length(items: &[DrawItem], start: usize) -> usize {
    let key = items[start].key;
    let lightmap = items[start].lightmap;
    let mut count = 1;
    while start + count < items.len()
        && items[start + count].key == key
        && items[start + count].lightmap == lightmap
    {
        count += 1;
    }
    count
}

/// [`CpuCollector::collect`] 的结果（与内部 `CollectCounts` 同口径）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuCollectResult {
    /// 进入本帧绘制的实例数（普通 + 蒙皮 + 透明合计）。
    pub visible: usize,
    /// 被视锥剔除的实例数。
    pub culled: u32,
}

/// 帧收集 CPU 路径的基准 / 测试入口（R33，GPU 无关）。
///
/// [`Renderer::draw`] 每帧在 GPU 编码前先做纯 CPU 的实例收集：可见性
/// 过滤 + 包围球 / 保守 AABB 视锥剔除 + alpha 分区（`collect_instances`）。
/// 这段是「最接近帧预算」的 CPU 热路径，但不构造 GPU 设备就测不到——
/// 本结构把收集逻辑以公开入口暴露：语义与渲染器帧内调用完全一致
/// （四个输出列表 + scratch 缓冲跨帧复用，与真实渲染路径的每帧成本
/// 同构），只测收集本身、不做排序与上传（排序由调用方完成，见
/// `collect_instances` 文档）。
///
/// 基准用法：`World` + `MeshStore` + `MaterialStore` + 视图矩阵 +
/// [`Frustum`] 构造一次，反复调用 [`CpuCollector::collect`]——
/// 见 `crates/iris-render/benches/render_cpu.rs` 的 `collect_instances_5k`
/// 与 `crates/iris-render/tests/budgets.rs`。
#[derive(Default)]
pub struct CpuCollector {
    items: Vec<DrawItem>,
    skinned: Vec<SkinnedItem>,
    transparent: Vec<TransparentItem>,
    transparent_skinned: Vec<TransparentSkinnedItem>,
    /// LOD v2 滞回记忆（与 `Renderer::lod_v2_memory` 同构——本入口的
    /// 契约就是「与渲染器帧内调用完全一致」，主路径的世界带 `LodSettings`
    /// 时这里同样按当帧世界资源走 v2 判定）。
    lod_memory: std::collections::BTreeMap<u64, bool>,
}

impl CpuCollector {
    /// 创建收集器（scratch 缓冲随首次收集按需增长，跨调用复用）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 执行一次帧收集（语义 = 渲染器帧内 `collect_instances` 调用：
    /// `Visibility` 过滤 + 祖先级联 + 包围球 / 保守 AABB 视锥剔除 +
    /// 不透明 / 透明分区）。
    pub fn collect(
        &mut self,
        world: &iris_core::world::World,
        mesh_store: &MeshStore,
        material_store: &MaterialStore,
        view: &Mat4,
        eye: Vec3,
        frustum: &Frustum,
    ) -> CpuCollectResult {
        let lod_v2 = world.try_resource::<LodSettings>().map(|s| *s);
        let counts = collect_instances(
            world,
            mesh_store,
            material_store,
            view,
            eye,
            frustum,
            &mut self.items,
            &mut self.skinned,
            &mut self.transparent,
            &mut self.transparent_skinned,
            lod_v2,
            Some(&mut self.lod_memory),
        );
        CpuCollectResult {
            visible: counts.visible,
            culled: counts.culled,
        }
    }
}

/// 收集全部 [`ParticleEmitter`] 的活粒子为 GPU 实例（`draw` 的粒子 pass
/// 数据准备；必须在 `update_particles` 之后调用——收集的是本帧模拟后的
/// 活粒子，调用契约见 `particles.rs`）。
///
/// - **Visibility 过滤**（打磨 III）：挂 `Visibility::HIDDEN` 的发射器
///   整池跳过（组件缺席 = 可见，与网格 / 透明收集同语义）；任一祖先
///   隐藏同样整池跳过（层级级联，见 `hidden_by_ancestor`）。注意隐藏
///   只是渲染过滤——`update_particles` 照常推进，粒子继续老化。
/// - **剔除按发射器包围球**（R22 同款保守界）：活粒子 AABB 的包围球 +
///   最大出生/终点尺寸边距（粒子为中心对称 quad，中心到 quad 最远点 =
///   尺寸，取 `size_start`/`size_end` 较大者保证不漏）。被剔发射器整批
///   不画、不计入任何统计（`culled_instances` 口径为几何实例，粒子剔除
///   口径独立，见 ARCHITECTURE §4.17）。
/// - **分区**：additive 段直接追加（加法交换律，不排序）；alpha 段收集
///   (视深, 收集序号, 实例) 三元组，由调用方按视深降序（远 → 近，与
///   R23 透明同款 `total_cmp` + 序号并列断言的比较器——全序、结果与
///   收集顺序无关）排序后追加。
///
/// 返回收集的粒子总数。
fn collect_particles(
    world: &iris_core::world::World,
    view: &Mat4,
    frustum: &Frustum,
    additive: &mut Vec<ParticleInstance>,
    alpha: &mut Vec<(f32, u32, ParticleInstance)>,
) -> u32 {
    additive.clear();
    alpha.clear();
    let mut total = 0u32;
    let mut seq = 0u32;
    // 单发射器的实例组装暂存（跨发射器 clear 复用，池规模 ≤ 单发射器上限）
    let mut tmp: Vec<ParticleInstance> = Vec::new();
    world.query::<(
        Entity,
        &GlobalTransform,
        &ParticleEmitter,
        Option<&Visibility>,
    )>(|(e, _, emitter, visibility)| {
        // 隐藏发射器整池跳过（自身 HIDDEN 或任一祖先 HIDDEN）
        if visibility.is_some_and(|v| !v.visible) || hidden_by_ancestor(world, e) {
            return;
        }
        let alive = emitter.particles();
        if alive.is_empty() {
            return;
        }
        let mut min = alive[0].pos;
        let mut max = alive[0].pos;
        for p in alive {
            min = min.min(p.pos);
            max = max.max(p.pos);
        }
        let center = (min + max) * 0.5;
        let radius = (max - min).length() * 0.5 + emitter.size_start.max(emitter.size_end);
        if !frustum.intersects_sphere(center, radius) {
            return;
        }
        tmp.clear();
        emitter.collect_render(&mut tmp);
        match emitter.blend {
            ParticleBlend::Additive => additive.extend(tmp.iter().copied()),
            ParticleBlend::Alpha => {
                for inst in &tmp {
                    // 视深与 R23 透明同口径：中心经视图矩阵后的 -z
                    let depth = -view.transform_point3(inst.center()).z;
                    alpha.push((depth, seq, *inst));
                    seq += 1;
                }
            }
        }
        total += tmp.len() as u32;
    });
    total
}

/// 收集全部 [`ParticleEmitterV2`](crate::particles_v2::ParticleEmitterV2)
/// 的模拟帧与渲染参数（`draw` 的粒子 v2 收集阶段；须在
/// `update_particles_v2` 之后调用——排水的是当帧记账产出的模拟帧）。
///
/// - **帧排水**：每发射器 `take_frames` 排空待消费队列（多次 update 而
///   未渲染的帧按序补跑；同状态重复渲染 = 空队列 = 不重复步进）。
/// - **Visibility 过滤**与 v1 同语义（组件缺席 = 可见；自身或任一祖先
///   `HIDDEN` 跳过**绘制**——模拟照常排水推进，粒子继续老化）。隐藏
///   发射器 `visible = false` 仍入表。
/// - **无视锥剔除**（与 v1 的差别，诚实限制）：粒子位置状态在 GPU，
///   CPU 无逐粒位置，发射器包围球剔除不可做。
/// - 拖尾渲染 uniform 现场构建（相机投影参数每帧变化，不驻组件）。
fn collect_particles_v2(
    world: &iris_core::world::World,
    proj_y: f32,
    aspect: f32,
    out: &mut Vec<ParticleV2Item>,
) {
    out.clear();
    world.query::<(
        iris_core::prelude::Entity,
        &GlobalTransform,
        &crate::particles_v2::ParticleEmitterV2,
        Option<&Visibility>,
    )>(|(e, _, emitter, visibility)| {
        let visible = !(visibility.is_some_and(|v| !v.visible) || hidden_by_ancestor(world, e));
        let trail_length = emitter.trail_length_clamped();
        out.push(ParticleV2Item {
            entity: e.to_bits(),
            blend: emitter.blend,
            alive: emitter.alive_estimate(),
            cursor: emitter.spawn_cursor(),
            trail_length,
            visible,
            pool: usize::MAX,
            frames: emitter.take_frames(),
            draw: crate::particles_v2::DrawParamsRaw {
                lifetime_size: [
                    emitter.particle_lifetime,
                    emitter.size_start,
                    emitter.size_end,
                    emitter.trail_width,
                ],
                color_start: emitter.color_start,
                color_end: emitter.color_end,
                trail_view: [trail_length as f32, proj_y, aspect, 0.0],
            },
        });
    });
}

/// 收集全部 [`Decal`] 贴花实例（`draw` 的贴花 pass 数据准备）。
///
/// - **Visibility 过滤与层级级联**：与普通 / 蒙皮 / 透明 / 粒子收集同
///   语义（组件缺席 = 可见；自身 `HIDDEN` 或任一祖先 `HIDDEN` 跳过，见
///   `hidden_by_ancestor`）。
/// - **视锥剔除按变换后 AABB**（R22 同款保守界）：单位盒 8 角点经世界盒
///   矩阵变换后取包围盒与视锥 `intersects_aabb`——薄盒旋转后包围球半径
///   （对角线 / 2）远大于实际覆盖，AABB 界明显更贴合。被剔实例不计入
///   `instances`、计入 `culled_instances`（与其他几何同口径）。
/// - **世界盒矩阵** = 实体全局矩阵 × `size` 缩放（size 在实体局部轴上，
///   跟随层级缩放语义）；`inv_model` 由 CPU 求逆传入（着色器零求逆）。
///   size 任一分量 ≤ 0 时矩阵不可逆（NaN），debug 断言拦截。
///
/// 返回被视锥剔除的实例数；收集次序即查询顺序，分批排序由调用方完成
/// （按纹理句柄排序，同批内实体 bits 并列序——见 `decal.rs` 分批策略）。
/// 单帧 meshlet 收集（R85；R86 扩展 run 形实例表；R87 扩展流送门控 +
/// 遮挡查询条目）：可见 [`MeshletInstance`](crate::meshlet::MeshletInstance)
/// 实体 → GPU 实例表条目 + 全局簇号前缀。
///
/// - **可见性语义与常规路径一致**：`Visibility` 自身 + 祖先级联；实例
///   包围球视锥剔除与 `collect_instances` 同式（中心变换 + 半径按最大
///   列缩放保守放大）。整实例出视锥 ⇒ 该实例全部簇都不进本帧枚举
///   （剔除端与全量端都看不到它的像素——绘制对齐的前提）。
/// - **LOD 选择（R86）**：`lod` 在场且实例网格带层级 ⇒ 逐实例跑
///   [`select_lod_cluster_ranges_in`]（眼点经模型逆变换到网格局部、
///   误差按最大轴缩放放大——与剔除端半径口径一致），选中簇区间升序
///   合并成 run，一条 run = 一条实例表条目（簇范围 = run）；选择输出的
///   区间在 arena 中连续 ⇒ cull / draw 对 run 的枚举恰为选中簇集合。
///   层级缺席 / LOD 关闭 ⇒ 单条全叶 run = R85 恒等表。
/// - **流送门控（R87）**：`stream_threshold` 在场（= 驻留预算激活）⇒
///   选择改走 [`select_lod_cluster_ranges_resident`]：选中组页缺时沿
///   DAG 上取驻留父组，根链全缺 ⇒ 实例整帧跳过；理想集 ∪ 可绘区间的
///   页记入 `out.stream_requests`（渲染器喂下一帧驻留 tick——本帧粗化、
///   页流入后细化收敛）。与 LOD 的关系：阈值取 LOD 设置（启用时），
///   缺席 = 0（全叶理想）；`lod` 在流送分支只供视口参数。
/// - **遮挡查询条目（R87）**：`occ_collect` 在场时，每个视锥可见实体
///   的簇联合盒（注册期预计算）过模型矩阵取保守世界盒，追加进
///   `out.occ_aabbs / occ_entities`——**先于任何跳过路径**（被遮挡
///   隐藏者也持续重查，是恢复可见的唯一路径）；隐藏实体记入
///   `occ_hidden_count` 并整帧跳过（不进实例表 = 不进 cull dispatch
///   输入）。与流送的次序：未驻留实例在选择期已整帧跳过，遮挡路径
///   不双重处理；遮挡隐藏者不产生流送请求（不为看不见的物体预热页）。
/// - **容量退化（确定性，按查询序）**：选中 run 数或簇数超出单帧预算
///   ⇒ 该实例退化为单条全叶 run（流送帧再做整块驻留门控——页缺则跳
///   过）；全叶也装不下 ⇒ 跳过实例 + warn（与 R85 口径一致）。
/// - **确定性**：条目序 = 查询序（存储插入序，全仓既有口径）× run 升序；
///   全局簇号 = 实例前缀和 + run 内局部号——剔除端逐簇独立裁决，
///   紧凑序 = 本帧枚举序升序（stable）。
#[allow(clippy::too_many_arguments)]
fn collect_meshlet_instances(
    world: &iris_core::world::World,
    frustum: &Frustum,
    slots: &[Option<MeshletArenaSlot>],
    lod: Option<MeshletLodFrameParams>,
    stream_threshold: Option<f32>,
    resident_pages: Option<&std::collections::BTreeSet<u32>>,
    occ_collect: bool,
    occ_hidden: &std::collections::HashSet<u64>,
    out: &mut MeshletFrame,
) {
    out.entries.clear();
    out.handles.clear();
    out.run_starts.clear();
    out.total_clusters = 0;
    out.stream_requests.clear();
    out.occ_aabbs.clear();
    out.occ_entities.clear();
    out.occ_hidden_count = 0;
    let mut warned_instances = false;
    let mut warned_clusters = false;
    let mut warned_lod_overflow = false;
    world.query::<(
        iris_core::prelude::Entity,
        &GlobalTransform,
        &crate::meshlet::MeshletInstance,
        Option<&Visibility>,
    )>(|(e, gt, mi, visibility)| {
        if visibility.is_some_and(|v| !v.visible) || hidden_by_ancestor(world, e) {
            return;
        }
        let Some(Some(slot)) = slots.get(mi.mesh.0 as usize) else {
            // 未注册 / 注册被拒 / 空网格：注册处已 warn 过，此处静默跳过
            return;
        };
        if out.entries.len() >= MAX_MESHLET_INSTANCES {
            if !warned_instances {
                warned_instances = true;
                log::warn!("meshlet 实例数超过单帧上限 {MAX_MESHLET_INSTANCES}，多余的已跳过");
            }
            return;
        }
        let model = gt.matrix();
        // 实例包围球视锥剔除（与 collect_instances 同式）
        let min = Vec3::from(slot.bounds_min);
        let max = min + Vec3::from(slot.bounds_extent);
        let center_local = (min + max) * 0.5;
        let radius_local = (max - min).length() * 0.5;
        let center_world = model.transform_point3(center_local);
        let scale = model
            .x_axis
            .truncate()
            .length()
            .max(model.y_axis.truncate().length())
            .max(model.z_axis.truncate().length());
        if !frustum.intersects_sphere(center_world, radius_local * scale) {
            return;
        }

        // 遮挡查询条目（R87）：先于流送 / 隐藏跳过——持续重查是恢复
        // 可见的唯一路径；AABB = 簇联合盒（局部）过模型矩阵的保守
        // 世界盒（R53 world_aabb 同式）。
        if occ_collect {
            let (umin, umax) = slot.cluster_union;
            let (lo, hi) = world_aabb(Vec3::from(umin), Vec3::from(umax), &model);
            out.occ_aabbs.push(AabbInstanceRaw {
                min: [lo.x, lo.y, lo.z, 0.0],
                max: [hi.x, hi.y, hi.z, 0.0],
            });
            let bits = e.to_bits();
            out.occ_entities.push(bits);
            if occ_hidden.contains(&bits) {
                out.occ_hidden_count += 1;
                return; // 被遮挡隐藏：整帧跳过（不进实例表 = 不进 cull dispatch 输入）
            }
        }

        // 本实例的选中 run 集（默认 = 单条全叶 run = R85 恒等表）。
        let leaf_count = slot.leaf_cluster_count;
        let mut runs: Vec<(u32, u32)> = vec![(0, leaf_count)];
        if let Some(threshold) = stream_threshold {
            // 流送门控（R87）：驻留感知选择 + 理想集请求收集。视口参数
            // 缺席（内部契约破坏，防御式）⇒ 保持全叶恒等表。
            if let Some(params) = &lod {
                let empty = crate::meshlet::MeshletHierarchy::default();
                let hierarchy = slot.hierarchy.as_deref().unwrap_or(&empty);
                let resident_set = resident_pages.expect("流送激活 ⇒ 驻留集在场");
                let is_resident = |local: u32| resident_set.contains(&(slot.page_base + local));
                let eye_local = model.inverse().transform_point3(params.eye);
                let view = crate::meshlet::MeshletLodView {
                    bounds_min: min,
                    bounds_extent: max - min,
                    eye_local,
                    error_scale: scale,
                    screen_height: params.screen_height,
                    tan_half_fov: params.tan_half_fov,
                };
                let sel = crate::meshlet_stream::select_lod_cluster_ranges_resident(
                    hierarchy,
                    leaf_count,
                    &view,
                    threshold,
                    &slot.pages,
                    &is_resident,
                );
                for (local, err) in sel.requests {
                    out.stream_requests.push((slot.page_base + local, err));
                }
                if sel.skipped || sel.ranges.is_empty() {
                    // 根链全缺 ⇒ 实例整帧跳过（理想页已请求，页流入后恢复）
                    return;
                }
                runs = sel.ranges;
            }
        } else if let Some(params) = &lod {
            if let Some(hierarchy) = &slot.hierarchy {
                let eye_local = model.inverse().transform_point3(params.eye);
                let view = crate::meshlet::MeshletLodView {
                    bounds_min: min,
                    bounds_extent: max - min,
                    eye_local,
                    error_scale: scale,
                    screen_height: params.screen_height,
                    tan_half_fov: params.tan_half_fov,
                };
                let selected = crate::meshlet::select_lod_cluster_ranges_in(
                    hierarchy,
                    leaf_count,
                    &view,
                    params.threshold_px,
                );
                if !selected.is_empty() {
                    runs = selected;
                }
            }
        }
        // 容量退化：选中集超预算 ⇒ 全叶 run；全叶也超 ⇒ 跳过实例。
        let selected_total: u32 = runs.iter().map(|&(_, n)| n).sum();
        if out.total_clusters as usize + selected_total as usize
            > crate::meshlet::MAX_MESHLET_CLUSTERS
            || out.entries.len() + runs.len() > MAX_MESHLET_INSTANCES
        {
            let fits_leaf = out.total_clusters as usize + leaf_count as usize
                <= crate::meshlet::MAX_MESHLET_CLUSTERS
                && out.entries.len() < MAX_MESHLET_INSTANCES;
            if !fits_leaf {
                if !warned_clusters {
                    warned_clusters = true;
                    log::warn!(
                        "meshlet 簇总数超过单帧剔除容量 {}，多余的实例已跳过",
                        crate::meshlet::MAX_MESHLET_CLUSTERS
                    );
                }
                return;
            }
            if !warned_lod_overflow {
                warned_lod_overflow = true;
                log::warn!("LOD 选中 run 超出单帧实例表 / 簇预算，该实例退化为全叶绘制");
            }
            if stream_threshold.is_some() {
                // 流送帧：全叶 run 也必须过驻留门（页缺 ⇒ 跳过——退化
                // 不越预算边界，驻留语义优先于绘制收益）。
                let resident_set = resident_pages.expect("流送激活 ⇒ 驻留集在场");
                let is_resident = |local: u32| resident_set.contains(&(slot.page_base + local));
                match crate::meshlet_stream::gate_full_leaf(&slot.pages, leaf_count, &is_resident) {
                    Some(gated) => runs = gated,
                    None => return,
                }
            } else {
                runs = vec![(0, leaf_count)];
            }
        }

        let raw = InstanceRaw::from_matrix(model);
        for &(run_start, run_len) in &runs {
            out.entries.push(MeshletInstanceEntryRaw {
                model: raw.model,
                normal: raw.normal,
                base_counts: [
                    slot.vertex_base,
                    slot.index_base,
                    slot.cluster_base + run_start,
                    run_len,
                ],
                frame_base: [out.total_clusters, 0, 0, 0],
                bounds_min: [
                    slot.bounds_min[0],
                    slot.bounds_min[1],
                    slot.bounds_min[2],
                    0.0,
                ],
                bounds_extent: [
                    slot.bounds_extent[0],
                    slot.bounds_extent[1],
                    slot.bounds_extent[2],
                    0.0,
                ],
            });
            out.handles.push(mi.mesh.0);
            out.run_starts.push(run_start);
            out.total_clusters += run_len;
        }
    });
}

fn collect_decals(
    world: &iris_core::world::World,
    frustum: &Frustum,
    out: &mut Vec<DecalItem>,
    lit: bool,
) -> u32 {
    out.clear();
    let mut culled = 0u32;
    world.query::<(Entity, &GlobalTransform, &Decal, Option<&Visibility>)>(
        |(e, gt, decal, visibility)| {
            // 自身 HIDDEN 跳过 + 祖先链级联（与其他收集路径同语义）
            if visibility.is_some_and(|v| !v.visible) || hidden_by_ancestor(world, e) {
                return;
            }
            debug_assert!(
                decal.size.x > 0.0 && decal.size.y > 0.0 && decal.size.z > 0.0,
                "贴花 size 三轴必须为正（零/负分量使盒矩阵不可逆，inv_model 将为 NaN）"
            );
            let model = gt.matrix() * Mat4::from_scale(decal.size);
            // 单位盒 8 角点 → 世界 AABB → 视锥相交测试（保守界：只多画不漏画）
            let mut min = Vec3::splat(f32::INFINITY);
            let mut max = Vec3::splat(f32::NEG_INFINITY);
            for corner in 0..8u32 {
                let local = Vec3::new(
                    if corner & 1 != 0 { 0.5 } else { -0.5 },
                    if corner & 2 != 0 { 0.5 } else { -0.5 },
                    if corner & 4 != 0 { 0.5 } else { -0.5 },
                );
                let world = model.transform_point3(local);
                min = min.min(world);
                max = max.max(world);
            }
            if !frustum.intersects_aabb(min, max) {
                culled += 1;
                return;
            }
            out.push(DecalItem {
                texture: decal.texture.0,
                entity: e.to_bits(),
                instance: DecalInstanceRaw::from_model(model, decal.tint, lit),
            });
        },
    );
    culled
}

impl Renderer {
    pub fn new(gpu: Arc<Gpu>, color_format: wgpu::TextureFormat) -> Self {
        let bundle = create_pipeline(&gpu, color_format, SAMPLE_COUNT);
        let globals_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.globals"),
            size: std::mem::size_of::<GlobalsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // 方向光级联阴影贴图（CSM，R29）：固定分辨率 2048² × 3 层的
        // texture_2d_array，不随窗口尺寸重建。深度值写入颜色附件 R 通道
        // （绕开 depth-only 管线写入异常，见 ARCHITECTURE §4.7），同时挂
        // 真实深度附件做 Less 测试——没有它重叠表面会按绘制顺序互相覆盖，
        // 背面深度会污染贴图（实测症状：贴图值系统性偏大、阴影整体失效）。
        // 逐级联一个渲染 pass（每层一个 D2 颜色/深度视图），采样侧用全层
        // 数组视图按级联选层——纹理方案与 v1 单张一致（R32Float 双附件），
        // 仅维度扩为 2D 数组，最小改动且与既有测试兼容。
        let shadow_map = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.shadow.map"),
            size: wgpu::Extent3d {
                width: crate::pipeline::SHADOW_MAP_SIZE,
                height: crate::pipeline::SHADOW_MAP_SIZE,
                depth_or_array_layers: SHADOW_CASCADES as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let shadow_map_view = shadow_map.create_view(&wgpu::TextureViewDescriptor {
            label: Some("iris.shadow.array_view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let shadow_color_views: [wgpu::TextureView; SHADOW_CASCADES] = std::array::from_fn(|i| {
            shadow_map.create_view(&wgpu::TextureViewDescriptor {
                label: Some("iris.shadow.color_layer_view"),
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: i as u32,
                array_layer_count: Some(1),
                ..Default::default()
            })
        });
        let shadow_depth_buf = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.shadow.depth"),
            size: wgpu::Extent3d {
                width: crate::pipeline::SHADOW_MAP_SIZE,
                height: crate::pipeline::SHADOW_MAP_SIZE,
                depth_or_array_layers: SHADOW_CASCADES as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let shadow_depth_views: [wgpu::TextureView; SHADOW_CASCADES] = std::array::from_fn(|i| {
            shadow_depth_buf.create_view(&wgpu::TextureViewDescriptor {
                label: Some("iris.shadow.depth_layer_view"),
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: i as u32,
                array_layer_count: Some(1),
                ..Default::default()
            })
        });
        // 级联参数（主 pass binding 8）与每级联 64B 矩阵（阴影 pass 组 0）。
        // 一级联一缓冲：同一 submit 内对同一缓冲的多次 write_buffer 会在
        // 执行前全部合并（泛光 / 点光面参数的既有教训）。
        let csm_params_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.csm.params"),
            size: std::mem::size_of::<CsmParamsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let cascade_bufs: [wgpu::Buffer; SHADOW_CASCADES] = std::array::from_fn(|_| {
            gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("iris.csm.cascade_params"),
                size: std::mem::size_of::<CascadeParamsRaw>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        let cascade_bgs: [wgpu::BindGroup; SHADOW_CASCADES] = std::array::from_fn(|i| {
            gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("iris.csm.cascade.bg"),
                layout: &bundle.cascade_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: cascade_bufs[i].as_entire_binding(),
                }],
            })
        });
        // 点光 cubemap 阴影贴图**数组**（R61）：R32Float
        // （预算 × 6）层（层序 = 槽位 × 6 + 面序；面序 = OpenGL 约定
        // [+X, −X, +Y, −Y, +Z, −Z]，与 PointLight::face_view_proj 配对）。
        // 复刻方向光阴影的双附件教训：颜色附件存值的同时必须挂深度附件做
        // Less 测试——没有它重叠表面按绘制顺序互相覆盖，背面值污染贴图。
        // 每盏灯的面分辨率与 v1 完全一致（512²），预算只增加层数。
        let point_shadow_map = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.point_shadow.map"),
            size: wgpu::Extent3d {
                width: crate::pipeline::POINT_SHADOW_MAP_SIZE,
                height: crate::pipeline::POINT_SHADOW_MAP_SIZE,
                depth_or_array_layers: (POINT_SHADOW_BUDGET * 6) as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let point_shadow_cube_view = point_shadow_map.create_view(&wgpu::TextureViewDescriptor {
            label: Some("iris.point_shadow.cube_array_view"),
            dimension: Some(wgpu::TextureViewDimension::CubeArray),
            ..Default::default()
        });
        let point_shadow_face_views = std::array::from_fn(|layer| {
            point_shadow_map.create_view(&wgpu::TextureViewDescriptor {
                label: Some("iris.point_shadow.face_view"),
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: layer as u32,
                array_layer_count: Some(1),
                ..Default::default()
            })
        });
        let point_shadow_depth = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.point_shadow.depth"),
            size: wgpu::Extent3d {
                width: crate::pipeline::POINT_SHADOW_MAP_SIZE,
                height: crate::pipeline::POINT_SHADOW_MAP_SIZE,
                depth_or_array_layers: (POINT_SHADOW_BUDGET * 6) as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let point_shadow_depth_face_views = std::array::from_fn(|layer| {
            point_shadow_depth.create_view(&wgpu::TextureViewDescriptor {
                label: Some("iris.point_shadow.depth_face_view"),
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: layer as u32,
                array_layer_count: Some(1),
                ..Default::default()
            })
        });
        // 每（槽位, 面）一份独立小 uniform + 绑定组：一缓冲一面（同 submit
        // 内对同一缓冲的多次 write_buffer 会在执行前合并，泛光参数的教训）
        let point_shadow_face_bufs: [wgpu::Buffer; POINT_SHADOW_BUDGET * 6] =
            std::array::from_fn(|_| {
                gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("iris.point_shadow.face_params"),
                    size: std::mem::size_of::<PointFaceParamsRaw>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            });
        let point_shadow_face_bgs: [wgpu::BindGroup; POINT_SHADOW_BUDGET * 6] =
            std::array::from_fn(|slot| {
                gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("iris.point_shadow.face_bg"),
                    layout: &bundle.point_shadow_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: point_shadow_face_bufs[slot].as_entire_binding(),
                    }],
                })
            });
        // IBL 环境贴图一次性烘焙（供主 pass 的分割求和环境光采样）
        let env = crate::environment::bake_environment(&gpu);

        // ---- 反射探针（R62a）----
        // 探针 cubemap **数组**：Rgba16Float、MAX × 6 层、预滤波 mip 链与
        // env 同档（PROBE_MIP_COUNT = ENV_MIP_COUNT）。层序 = 槽位 × 6 +
        // 面序（面序与 cube_face_view_proj 的 POINT_SHADOW_FACES 配对）。
        // 预算做成常量（同点光阴影）：层数在初始化固定，运行时调整需重建
        // 纹理与全部面视图 / 绑定组，收益远小于复杂度。
        let probe_map = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.probe.map"),
            size: wgpu::Extent3d {
                width: crate::probe::PROBE_MAP_SIZE,
                height: crate::probe::PROBE_MAP_SIZE,
                depth_or_array_layers: (MAX_REFLECTION_PROBES * 6) as u32,
            },
            mip_level_count: PROBE_MIP_COUNT,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: crate::pipeline::HDR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let probe_cube_view = probe_map.create_view(&wgpu::TextureViewDescriptor {
            label: Some("iris.probe.cube_array_view"),
            dimension: Some(wgpu::TextureViewDimension::CubeArray),
            ..Default::default()
        });
        // 捕获擦写立方体（mip 0）+ 深度附件：预滤波 pass 不能同纹理既读又写
        // （源 = 采样纹理、目标 = 颜色附件），捕获因此落在共享擦写上。
        // 探针 cubemap 的**写入**只经预滤波链（它在内部按 (mip, 面) 建视图），
        // 故不为 probe_map 预制面视图。
        let probe_raw = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.probe.raw"),
            size: wgpu::Extent3d {
                width: crate::probe::PROBE_MAP_SIZE,
                height: crate::probe::PROBE_MAP_SIZE,
                depth_or_array_layers: 6,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: crate::pipeline::HDR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let probe_raw_cube_view = probe_raw.create_view(&wgpu::TextureViewDescriptor {
            label: Some("iris.probe.raw_cube_view"),
            dimension: Some(wgpu::TextureViewDimension::Cube),
            ..Default::default()
        });
        let probe_raw_face_views = std::array::from_fn(|face| {
            probe_raw.create_view(&wgpu::TextureViewDescriptor {
                label: Some("iris.probe.raw_face_view"),
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: face as u32,
                array_layer_count: Some(1),
                ..Default::default()
            })
        });
        let probe_raw_depth = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.probe.raw_depth"),
            size: wgpu::Extent3d {
                width: crate::probe::PROBE_MAP_SIZE,
                height: crate::probe::PROBE_MAP_SIZE,
                depth_or_array_layers: 6,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let probe_raw_depth_face_views = std::array::from_fn(|face| {
            probe_raw_depth.create_view(&wgpu::TextureViewDescriptor {
                label: Some("iris.probe.raw_depth_face_view"),
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: face as u32,
                array_layer_count: Some(1),
                ..Default::default()
            })
        });
        // 参数缓冲：主 pass 用（每帧按选择结果写入），捕获用（恒零，见字段注释）
        let probe_params_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.probe.params"),
            size: std::mem::size_of::<ProbeParamsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let probe_capture_params_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.probe.capture_params"),
            size: std::mem::size_of::<ProbeParamsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue.write_buffer(
            &probe_params_buf,
            0,
            bytemuck::bytes_of(&ProbeParamsRaw::default()),
        );
        gpu.queue.write_buffer(
            &probe_capture_params_buf,
            0,
            bytemuck::bytes_of(&ProbeParamsRaw::default()),
        );
        // 1×1 哑 cubemap：捕获绑定组的 binding 10 占位（内容永不被采样——
        // 捕获参数恒 count = 0；见 probe_capture_params_buf 的字段注释）
        let probe_dummy_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.probe.dummy"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 6,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: crate::pipeline::HDR_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let probe_dummy_view = probe_dummy_tex.create_view(&wgpu::TextureViewDescriptor {
            label: Some("iris.probe.dummy_view"),
            dimension: Some(wgpu::TextureViewDimension::CubeArray),
            ..Default::default()
        });
        // SSR 常驻资源（R62b）：1×1 哑纹理 + 双 96B 参数缓冲。布局要求
        // binding 11/12/13 恒需合法绑定，哑资源始终存在（内容永不被采样
        // ——flags = 0 时着色器整段早退）；真实上帧颜色副本由
        // `ensure_ssr_prev_color` 在 `SsrSettings` 首次开启时惰性创建。
        let make_ssr_params = |label: &'static str| {
            gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: std::mem::size_of::<SsrParamsRaw>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let ssr_params_buf = make_ssr_params("iris.ssr.params");
        let ssr_off_params_buf = make_ssr_params("iris.ssr.params_off");
        let ssr_dummy_color = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.ssr.dummy_color"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: crate::pipeline::HDR_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let ssr_dummy_color_view =
            ssr_dummy_color.create_view(&wgpu::TextureViewDescriptor::default());
        let ssr_dummy_depth = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.ssr.dummy_depth"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let ssr_dummy_depth_view =
            ssr_dummy_depth.create_view(&wgpu::TextureViewDescriptor::default());
        let ssr = SsrGpu {
            params_buf: ssr_params_buf,
            off_params_buf: ssr_off_params_buf,
            dummy_color: ssr_dummy_color,
            dummy_color_view: ssr_dummy_color_view,
            dummy_depth: ssr_dummy_depth,
            dummy_depth_view: ssr_dummy_depth_view,
            prev_color: None,
            prev_size: (0, 0),
        };
        // AO 常驻资源（R77）：1×1 R16Float 哑纹理（纹素 = 1.0 中性可见度，
        // f16 位型 0x3C00）+ 32B 参数缓冲。布局要求 binding 14 恒需合法
        // 绑定，哑资源始终存在；真实半分辨率可见度图由 `ensure_ao_targets`
        // 在 `AoSettings` 首次开启时惰性创建。
        let ao_params_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.ao.params"),
            size: std::mem::size_of::<AoParamsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let ao_dummy = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.ao.dummy"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // 纹素写 1.0（f16 0x3C00 小端 = [0x00, 0x3C]）：关闭 / 捕获 / RT
        // 路径采样后 × 1.0，IEEE 754 下与不乘逐位一致。
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &ao_dummy,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[0x00u8, 0x3C],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(2),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let ao_dummy_view = ao_dummy.create_view(&wgpu::TextureViewDescriptor::default());
        let ao = AoGpu {
            params_buf: ao_params_buf,
            dummy: ao_dummy,
            dummy_view: ao_dummy_view,
            target: None,
            target_size: (0, 0),
            bind_real: false,
        };
        // 光源分箱常驻资源（R78）：真缓冲（32B 参数 + 512×80B 灯表 +
        // 1 MiB tile 索引）与哑资源（32B / 80B / 16B 全零——mode = 0 使
        // 着色器分箱分支死码）各一套。wgpu 缓冲零初始化 ⇒ 哑资源无需
        // 写入即满足「全零 mode=0」语义；真缓冲只在分箱生效帧写。
        let make_bins_buf = |label: &'static str, size: u64, usage: wgpu::BufferUsages| {
            gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let light_bins = LightBinsGpu {
            params_buf: make_bins_buf(
                "iris.bins.params",
                std::mem::size_of::<LightBinsParamsRaw>() as u64,
                wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            ),
            lights_buf: make_bins_buf(
                "iris.bins.lights",
                (MAX_BINNED_LIGHTS * std::mem::size_of::<BinnedLightRaw>()) as u64,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            ),
            tiles_buf: make_bins_buf(
                "iris.bins.tiles",
                LIGHT_BINS_TILES_BYTES,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            ),
            off_params_buf: make_bins_buf(
                "iris.bins.params_off",
                std::mem::size_of::<LightBinsParamsRaw>() as u64,
                wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            ),
            off_lights_buf: make_bins_buf(
                "iris.bins.lights_off",
                std::mem::size_of::<BinnedLightRaw>() as u64,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            ),
            off_tiles_buf: make_bins_buf(
                "iris.bins.tiles_off",
                16,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            ),
            bind_real: false,
        };
        // GI 探针 SH 常驻资源（R83）：真缓冲（256 × 160B 探针表 + 16B 参数）
        // 与哑资源（160B / 16B 全零——mode = 0 使着色器逐片元分支死码）各一套。
        // wgpu 缓冲零初始化 ⇒ 哑资源无需写入即满足「全零 mode=0」语义；真缓冲
        // 只在探针集带 SH 的帧写（分箱同款纪律）。
        let gi_sh = GiShGpu {
            probes_buf: make_bins_buf(
                "iris.gi_sh.probes",
                (MAX_IRRADIANCE_PROBES * std::mem::size_of::<ShProbeRaw>()) as u64,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            ),
            params_buf: make_bins_buf(
                "iris.gi_sh.params",
                std::mem::size_of::<GiShParamsRaw>() as u64,
                wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            ),
            off_probes_buf: make_bins_buf(
                "iris.gi_sh.probes_off",
                std::mem::size_of::<ShProbeRaw>() as u64,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            ),
            off_params_buf: make_bins_buf(
                "iris.gi_sh.params_off",
                std::mem::size_of::<GiShParamsRaw>() as u64,
                wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            ),
            bind_real: false,
        };
        // DDGI 深度表常驻资源（R88）：4 B 全零哑缓冲恒在（binding 20 的布局
        // 占位——mode ≤ 1 / 捕获 / RT 帧绑定，着色器 mode 2 分支死码永不读）；
        // 768 KiB 真表**惰性创建**（首个 mode = 2 帧，见 `ensure_ddgi_gpu`）。
        let ddgi = DdgiGpu {
            depth_buf: None,
            off_depth_buf: make_bins_buf(
                "iris.ddgi.depth_off",
                4,
                wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            ),
            bind_real: false,
        };
        // 主视图绑定组初建时的屏幕空间资源（SSR 三路 + AO + 分箱三路）：
        // 参数走**活跃**缓冲（此后由 draw 逐帧管理），颜色 / 深度 / AO /
        // 分箱此时只有哑资源（prev_color / scene_depth / ao target 均未
        // 创建、bins bind_real = false；就位时重建绑定组，见 draw 的接线）。
        let ssr_main_res = ScreenSpaceGlobalsResources {
            params_buf: &ssr.params_buf,
            color_view: &ssr.dummy_color_view,
            depth_view: &ssr.dummy_depth_view,
            ao_view: &ao.dummy_view,
            bins_params: &light_bins.off_params_buf,
            bins_lights: &light_bins.off_lights_buf,
            bins_tiles: &light_bins.off_tiles_buf,
        };
        // 捕获期每面一份 Globals 缓冲 + 绑定组（面矩阵 / 眼点逐面不同）
        let probe_face_globals_bufs: [wgpu::Buffer; 6] = std::array::from_fn(|_| {
            gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("iris.probe.face_globals"),
                size: std::mem::size_of::<GlobalsRaw>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        // 捕获绑定组：绑定 0..8 与主组同源（阴影贴图 / env / CSM / 点光阴影），
        // 绑定 9/10 指向**哑资源 + 零参数**（捕获不做探针自反馈），
        // SSR 三路同样走零参数 + 哑纹理（捕获不做屏幕空间反射），
        // 分箱三路走哑零缓冲（捕获不渲染锥光与第 9+ 盏点光——mode = 0）
        let probe_face_globals_bgs: [wgpu::BindGroup; 6] = std::array::from_fn(|face| {
            build_globals_bind_group(
                &gpu,
                &bundle,
                GlobalsBuffers {
                    globals: &probe_face_globals_bufs[face],
                    csm_params: &csm_params_buf,
                },
                ShadowMapViews {
                    cascades: &shadow_map_view,
                    point_cube: &point_shadow_cube_view,
                },
                &env,
                ProbeGlobalsResources {
                    params_buf: &probe_capture_params_buf,
                    cube_view: &probe_dummy_view,
                    gi_probes: &gi_sh.off_probes_buf,
                    gi_params: &gi_sh.off_params_buf,
                    gi_depth: &ddgi.off_depth_buf,
                },
                ScreenSpaceGlobalsResources {
                    params_buf: &ssr.off_params_buf,
                    color_view: &ssr.dummy_color_view,
                    depth_view: &ssr.dummy_depth_view,
                    ao_view: &ao.dummy_view,
                    bins_params: &light_bins.off_params_buf,
                    bins_lights: &light_bins.off_lights_buf,
                    bins_tiles: &light_bins.off_tiles_buf,
                },
            )
        });
        // 大气散射天空（R79）：64B 参数缓冲（零初始化 = mode 0 死码分支
        // ——缺席帧一个 write_buffer 都不发，命令流与 R78 逐位一致）+
        // 每捕获面一份天空绑定组（与面 Globals 缓冲一一对应——探针内的
        // 天空与主视口同一着色器同一参数）。
        let sky_params_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.sky.params"),
            size: std::mem::size_of::<SkyParamsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let probe_face_sky_bgs: [wgpu::BindGroup; 6] = std::array::from_fn(|face| {
            build_sky_bind_group(
                &gpu,
                &bundle,
                &env,
                SkyBindResources {
                    globals: &probe_face_globals_bufs[face],
                    sky_params: &sky_params_buf,
                },
            )
        });

        // 内置 0 号白色兜底纹理：材质未指定贴图时使用
        let white = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.texture.white"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &white,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[255, 255, 255, 255],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let white_gpu = GpuTexture {
            view: white.create_view(&wgpu::TextureViewDescriptor::default()),
            sampler: gpu
                .device
                .create_sampler(&wgpu::SamplerDescriptor::default()),
            // 1×1 兜底纹理非 4 对齐，恒走 RGBA 路径（决策树与其一致）
            bc1: false,
            bc3: false,
            bc7: false,
        };
        let mut texture_gpu = HashMap::new();
        texture_gpu.insert(0u32, white_gpu);
        // 中性法线纹理（R42，常驻）：1×1 Rgba16Float = (0.5, 0.5, 1)。
        // 0.5 / 1.0 在 binary16 中精确表示（位型 0x3800 / 0x3C00），着色器
        // 采样 *2−1 后恰得 (0, 0, 1)——法线扰动恒等变换，保证无贴图材质
        // 与 R41 逐位一致（论证链见 material.rs 模块文档）。8-bit unorm
        // 无法精确表示 0.5（128/255 ≈ 0.502），故必须 16-bit float。
        let neutral_normal_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.texture.neutral_normal"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // half 精度的 (0.5, 0.5, 1.0, 1.0)：f16 位型小端字节（0.5 = 0x3800，
        // 1.0 = 0x3C00），免引 half 依赖
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &neutral_normal_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[
                0x00, 0x38, 0x00, 0x38, // 0.5, 0.5
                0x00, 0x3c, 0x00, 0x3c, // 1.0, 1.0
            ],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(8),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let neutral_normal_view =
            neutral_normal_tex.create_view(&wgpu::TextureViewDescriptor::default());
        // 常驻零填充切线垫（R42）：无切线网格共用，1 MiB = 65536 顶点 × 16 B。
        // wgpu 缓冲创建即零初始化，无需写入。
        let tangent_dummy_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.mesh.tangent_dummy"),
            size: 65536 * 16,
            usage: wgpu::BufferUsages::VERTEX,
            mapped_at_creation: false,
        });
        // 常驻零填充 UV2 垫（R52c）：无光照贴图 UV 的网格共用，
        // 512 KiB = 65536 顶点 × 8 B（与切线垫同容量口径）。
        let uv2_dummy_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.mesh.uv2_dummy"),
            size: 65536 * 8,
            usage: wgpu::BufferUsages::VERTEX,
            mapped_at_creation: false,
        });
        // 常驻 1×1 黑光照贴图（R52c）：wgpu 纹理创建即零初始化 ⇒ 无需
        // write_texture（全零辐照；掩码 0 时 select 直接丢弃采样结果）。
        let lightmap_dummy_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.lightmap.dummy"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let lightmap_dummy_view =
            lightmap_dummy_tex.create_view(&wgpu::TextureViewDescriptor::default());
        // 光照贴图采样器：双线性（低频信号，比最近邻平滑）+ ClampToEdge
        // （图集坐标恒在 [0,1]，Repeat 会让边缘 chart 采到对侧）。
        let lightmap_sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("iris.lightmap.sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        // 预积分皮肤 SSS LUT（R63，常驻）：64×64 Rgba16Float，程序化生成
        // （纯函数，无 RNG；同输入两次生成逐位同一输出——单测锁定）。
        // 初始化时一次性上传：LUT 与场景无关，没有「按需生成 / 热替换」
        // 语义（LUT 运行时热替换在拒绝清单里）。行步长 = 64 × 8 B
        // （queue.write_texture 无 256 字节对齐要求）。
        let sss_lut_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.sss.lut"),
            size: wgpu::Extent3d {
                width: crate::sss::SSS_LUT_SIZE,
                height: crate::sss::SSS_LUT_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &sss_lut_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &crate::sss::lut_half_bytes(crate::sss::SSS_LUT_SIZE),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(crate::sss::SSS_LUT_SIZE * 8),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: crate::sss::SSS_LUT_SIZE,
                height: crate::sss::SSS_LUT_SIZE,
                depth_or_array_layers: 1,
            },
        );
        let sss_lut_view = sss_lut_tex.create_view(&wgpu::TextureViewDescriptor::default());
        // UI 字形图集：初始化时一次性构建并上传。单一纹理同时服务矩形
        // （0 号格纯白块）与文本（字形 texel，白色 + alpha=行位码），
        // 使 UI 全部内容共享单管线单纹理单 draw。采样器 Nearest：
        // 位图字形锐利，也弱化图集渗色。
        let ui_atlas = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.ui.atlas"),
            size: wgpu::Extent3d {
                width: crate::ui::ATLAS_WIDTH,
                height: crate::ui::ATLAS_HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &ui_atlas,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &crate::ui::build_atlas_rgba(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(crate::ui::ATLAS_WIDTH * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: crate::ui::ATLAS_WIDTH,
                height: crate::ui::ATLAS_HEIGHT,
                depth_or_array_layers: 1,
            },
        );
        let ui_atlas_view = ui_atlas.create_view(&wgpu::TextureViewDescriptor::default());
        let ui_sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("iris.ui.sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let ui_screen_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.ui.screen"),
            size: std::mem::size_of::<UiGlobalsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let ui_bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iris.ui.bg"),
            layout: &bundle.ui_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&ui_atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&ui_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: ui_screen_buf.as_entire_binding(),
                },
            ],
        });
        // RT（R40）色调映射共享资源：采样器（线性 Clamp，与后处理采样器
        // 同行为）+ 恒中性合成参数（泛光 0 / 曝光 1 / 暗角 0——RT 无后处理
        // 链，ACES 前不叠泛光、不放大、不压角）。所有 RT 共用一份。
        let rt_blit_sampler = gpu
            .device
            .create_sampler(&wgpu::SamplerDescriptor::default());
        let rt_post_params_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.rt.blit_params"),
            size: std::mem::size_of::<PostParamsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue.write_buffer(
            &rt_post_params_buf,
            0,
            bytemuck::bytes_of(&PostParamsRaw {
                bloom_strength: 0.0,
                exposure: 1.0,
                vignette_strength: 0.0,
                _reserved: 0.0,
            }),
        );
        // 全局绑定组（env 视图相关的构建逻辑与重烘焙共用，见 build_globals_bind_group）
        let globals_bg = build_globals_bind_group(
            &gpu,
            &bundle,
            GlobalsBuffers {
                globals: &globals_buf,
                csm_params: &csm_params_buf,
            },
            ShadowMapViews {
                cascades: &shadow_map_view,
                point_cube: &point_shadow_cube_view,
            },
            &env,
            ProbeGlobalsResources {
                params_buf: &probe_params_buf,
                cube_view: &probe_cube_view,
                gi_probes: &gi_sh.off_probes_buf,
                gi_params: &gi_sh.off_params_buf,
                gi_depth: &ddgi.off_depth_buf,
            },
            ssr_main_res,
        );
        // 主相机天空绑定组（R79）：与 globals 组同 env / 同 Globals 缓冲，
        // 多一路 SkyParams（mode = 0 旧路径——缺席帧 FNV 基线的保证）
        let sky_bg = build_sky_bind_group(
            &gpu,
            &bundle,
            &env,
            SkyBindResources {
                globals: &globals_buf,
                sky_params: &sky_params_buf,
            },
        );
        // 粒子相机 uniform + 绑定组（粒子管线的组 1；内容每帧有粒子时覆写）
        let particle_camera_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.particle.camera"),
            size: std::mem::size_of::<ParticleCameraRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let particle_camera_bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iris.particle.camera.bg"),
            layout: &bundle.particle_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: particle_camera_buf.as_entire_binding(),
            }],
        });
        // 贴花代理几何（单位盒 ±Z 端盖）：静态小缓冲，初始化时一次性上传，
        // 全部贴花实例共享（实例数据在 decal_instance_buf 逐帧携带）
        let decal_vertex_buf = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("iris.decal.proxy_vertices"),
                contents: bytemuck::cast_slice(&PROXY_POSITIONS),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let decal_index_buf = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("iris.decal.proxy_indices"),
                contents: bytemuck::cast_slice(&PROXY_INDICES),
                usage: wgpu::BufferUsages::INDEX,
            });
        Self {
            gpu,
            bundle,
            globals_buf,
            globals_bg,
            csm_params_buf,
            cascade_bufs,
            cascade_bgs,
            instance_buf: None,
            instance_capacity: 0,
            last_instances: Vec::new(),
            mesh_gpu: HashMap::new(),
            texture_gpu,
            mip_anisotropy: None,
            material_gpu: HashMap::new(),
            neutral_normal_tex,
            neutral_normal_view,
            tangent_dummy_buf,
            uv2_dummy_buf,
            lightmap_gpu: HashMap::new(),
            lightmap_dummy_tex,
            lightmap_dummy_view,
            lightmap_sampler,
            sss_lut_tex,
            sss_lut_view,
            render_target_gpu: HashMap::new(),
            lod_v2_memory: std::collections::BTreeMap::new(),
            rt_blit_sampler,
            rt_post_params_buf,
            skin_gpu: HashMap::new(),
            morph_gpu: HashMap::new(),
            morph_uploads: 0,
            shadow_map,
            shadow_map_view,
            shadow_depth_buf,
            shadow_color_views,
            shadow_depth_views,
            point_shadow_map,
            point_shadow_cube_view,
            point_shadow_face_views,
            point_shadow_depth,
            point_shadow_depth_face_views,
            point_shadow_face_bufs,
            point_shadow_face_bgs,
            probe_params_buf,
            probe_capture_params_buf,
            probe_map,
            probe_cube_view,
            probe_raw,
            probe_raw_cube_view,
            probe_raw_face_views,
            probe_raw_depth,
            probe_raw_depth_face_views,
            probe_face_globals_bufs,
            probe_face_globals_bgs,
            sky_params_buf,
            sky_bg,
            probe_face_sky_bgs,
            probe_dummy_tex,
            probe_dummy_view,
            probe_instance_buf: None,
            probe_instance_capacity: 0,
            probe_store: ProbeStore::new(),
            probe_params_active: false,
            legacy_probe_no_parallax: false,
            legacy_shadow_bias: false,
            env,
            env_baked_generation: 0,
            hdr: None,
            hdr_size: (0, 0),
            scene_depth: None,
            scene_depth_size: (0, 0),
            color_format,
            occ_gpu: None,
            occ_inflight: None,
            occ_hidden: std::collections::HashSet::new(),
            occ_frame: 0,
            prev_view_proj: None,
            taa_frame: 0,
            taa_reset_pending: false,
            taa_was_on: false,
            ssr,
            ssr_prev_valid: false,
            ssr_was_on: false,
            ssr_uniform_live: false,
            ao,
            light_bins,
            gi_sh,
            ddgi,
            ddgi_upload: DdgiUploadState {
                structure: u64::MAX,
                stamps: Vec::new(),
                params: None,
            },
            ddgi_depth_weight: true,
            ddgi_mutex_warned: false,
            ao_depth_frame: None,
            water: None,
            water_size: (0, 0),
            water_items: Vec::new(),
            water_skinned: Vec::new(),
            water_transparent: Vec::new(),
            water_transparent_skinned: Vec::new(),
            water_instance_scratch: Vec::new(),
            ui_atlas,
            ui_bind_group,
            ui_sampler,
            ui_screen_buf,
            ui_vertex_buf: None,
            ui_capacity: 0,
            particle_buf: None,
            particle_capacity: 0,
            particle_camera_buf,
            particle_camera_bg,
            decal_vertex_buf,
            decal_index_buf,
            decal_instance_buf: None,
            decal_capacity: 0,
            decal_bind_groups: HashMap::new(),
            decal_scratch: Vec::new(),
            decal_instance_scratch: Vec::new(),
            scratch: Vec::new(),
            instance_scratch: Vec::new(),
            skinned_scratch: Vec::new(),
            transparent_scratch: Vec::new(),
            transparent_skinned_scratch: Vec::new(),
            ui_scratch: Vec::new(),
            ui_image_scratch: Vec::new(),
            ui_image_buf: None,
            ui_image_capacity: 0,
            rt_items: Vec::new(),
            rt_skinned: Vec::new(),
            rt_transparent: Vec::new(),
            rt_transparent_skinned: Vec::new(),
            rt_instance_scratch: Vec::new(),
            occ_drawn_scratch: Vec::new(),
            occ_aabb_scratch: Vec::new(),
            occ_entity_scratch: Vec::new(),
            particle_scratch: Vec::new(),
            particle_alpha_scratch: Vec::new(),
            particle_v2_pools: Vec::new(),
            meshlet_gpu: None,
            meshlet_arena: MeshletArena::default(),
            meshlet_inflight: None,
            meshlet_consumed: None,
            meshlet_frame: 0,
            meshlet_last_handles: Vec::new(),
            meshlet_frame_scratch: MeshletFrame::default(),
            meshlet_last_run_starts: Vec::new(),
            meshlet_stream: None,
            meshlet_stream_requests: Vec::new(),
            meshlet_stream_recover_upload: false,
            meshlet_occ_aabb_scratch: Vec::new(),
            meshlet_occ_entity_scratch: Vec::new(),
            particle_v2_scratch: Vec::new(),
            clear_color: wgpu::Color {
                r: 0.055,
                g: 0.07,
                b: 0.10,
                a: 1.0,
            },
            stats: FrameStats::default(),
        }
    }

    /// 重建全局绑定组。env 视图随每次环境重烘焙变化（旧视图随旧纹理销毁），
    /// irradiance / prefiltered / brdf 三个视图引用必须同步更新；SSR 的
    /// 上帧颜色副本 / 场景深度副本就位或更换时同样经此重建（draw 接线）。
    fn rebuild_globals_bind_group(&mut self) {
        self.globals_bg = build_globals_bind_group(
            &self.gpu,
            &self.bundle,
            GlobalsBuffers {
                globals: &self.globals_buf,
                csm_params: &self.csm_params_buf,
            },
            ShadowMapViews {
                cascades: &self.shadow_map_view,
                point_cube: &self.point_shadow_cube_view,
            },
            &self.env,
            self.probe_globals_resources(),
            self.ssr_main_resources(),
        );
        // 天空绑定组（R79）随重建同步更新：env 重烘焙会更换 prefiltered
        // 视图，sky 组的 binding 4 必须跟着指向新视图（Globals / SkyParams
        // 缓冲不变，仅视图与采样器重建）。
        self.sky_bg = build_sky_bind_group(
            &self.gpu,
            &self.bundle,
            &self.env,
            SkyBindResources {
                globals: &self.globals_buf,
                sky_params: &self.sky_params_buf,
            },
        );
    }

    /// 探针资源（R62a 反射两路 + R83 GI SH 两路 + R88 DDGI 深度表一路）的
    /// 「主相机」形态：反射参数 / cubemap 数组视图恒为活跃资源，GI 两路按
    /// `gi_sh.bind_real` 裁决（mode = 0 帧绑哑零缓冲），深度表按
    /// `ddgi.bind_real` 裁决（mode ≠ 2 帧绑 4 B 哑缓冲——即使真表已创建，
    /// 陈旧内容也不得被采样）。
    fn probe_globals_resources(&self) -> ProbeGlobalsResources<'_> {
        let (gi_probes, gi_params) = if self.gi_sh.bind_real {
            (&self.gi_sh.probes_buf, &self.gi_sh.params_buf)
        } else {
            (&self.gi_sh.off_probes_buf, &self.gi_sh.off_params_buf)
        };
        let gi_depth = match self.ddgi.bind_real.then_some(self.ddgi.depth_buf.as_ref()) {
            Some(Some(buf)) => buf,
            _ => &self.ddgi.off_depth_buf,
        };
        ProbeGlobalsResources {
            params_buf: &self.probe_params_buf,
            cube_view: &self.probe_cube_view,
            gi_probes,
            gi_params,
            gi_depth,
        }
    }

    /// 屏幕空间资源的「主相机」形态（R62b 立法，R77 扩 AO，R78 再扩
    /// 分箱三路）：活跃参数缓冲 + 真实上帧资源 + AO 可见度图（`bind_real`
    /// 裁决——AO 关闭帧即使 target 还在也回落哑纹理）+ 分箱真缓冲
    /// （`light_bins.bind_real` 裁决，关闭帧绑哑零缓冲 mode = 0）。
    fn ssr_main_resources(&self) -> ScreenSpaceGlobalsResources<'_> {
        let (bins_params, bins_lights, bins_tiles) = if self.light_bins.bind_real {
            (
                &self.light_bins.params_buf,
                &self.light_bins.lights_buf,
                &self.light_bins.tiles_buf,
            )
        } else {
            (
                &self.light_bins.off_params_buf,
                &self.light_bins.off_lights_buf,
                &self.light_bins.off_tiles_buf,
            )
        };
        ScreenSpaceGlobalsResources {
            params_buf: &self.ssr.params_buf,
            color_view: self
                .ssr
                .prev_color
                .as_ref()
                .map(|p| &p.view)
                .unwrap_or(&self.ssr.dummy_color_view),
            depth_view: self
                .scene_depth
                .as_ref()
                .map(|sd| &sd.view)
                .unwrap_or(&self.ssr.dummy_depth_view),
            ao_view: match self.ao.bind_real.then_some(self.ao.target.as_ref()) {
                Some(Some(target)) => &target.view,
                _ => &self.ao.dummy_view,
            },
            bins_params,
            bins_lights,
            bins_tiles,
        }
    }

    /// 分箱真缓冲上传（R78，mode = 1 的 draw 每帧调用）：灯表与 tile
    /// 索引、参数三缓冲。tile 索引缓冲为**固定容量**
    /// （[`LIGHT_BINS_TILES_BYTES`]）——前 `2 × tile 数` 个 u32 是
    /// ranges 区、indices 区接续；indices 超出剩余容量时**按序截断**
    /// （确定性：保留数组序靠前的灯），受影响 tile 及其后全部清零计数
    /// 并 `log::warn`（与点光超限同口径：不是静默丢弃）。
    fn upload_light_bins(&self, lights: &[BinnedLightRaw], vp: Mat4, width: u32, height: u32) {
        let inputs: Vec<LightBinInput> = lights
            .iter()
            .map(|l| LightBinInput {
                position: Vec3::new(l.pos_type[0], l.pos_type[1], l.pos_type[2]),
                range: l.color_range[3],
            })
            .collect();
        let bins = bin_lights(&inputs, vp, width, height, LIGHT_TILE_SIZE);
        let tile_count = bins.tiles_x as usize * bins.tiles_y as usize;
        let capacity_u32 = (LIGHT_BINS_TILES_BYTES / 4) as usize;
        let ranges_u32 = tile_count * 2;
        let indices_capacity = capacity_u32.saturating_sub(ranges_u32);
        // ranges 区 + indices 区（超容量按序截断）
        let mut ranges_part: Vec<u32> = Vec::with_capacity(ranges_u32);
        let mut indices_part: Vec<u32> = Vec::with_capacity(bins.indices.len());
        let mut truncated = false;
        let mut written = 0usize;
        for r in &bins.ranges {
            let count = r[1] as usize;
            let take = count.min(indices_capacity.saturating_sub(written));
            truncated |= take < count;
            ranges_part.push(written as u32);
            ranges_part.push(take as u32);
            indices_part.extend_from_slice(&bins.indices[r[0] as usize..r[0] as usize + take]);
            written += take;
        }
        ranges_part.extend(indices_part);
        if truncated {
            log::warn!(
                "分箱 tile 索引超出缓冲容量（{} u32），已按序截断（保留数组序靠前的灯）",
                capacity_u32
            );
        }
        self.gpu
            .queue
            .write_buffer(&self.light_bins.lights_buf, 0, bytemuck::cast_slice(lights));
        // 病态大屏（ranges 区本身超 1 MiB）下只上传放得下的前缀：WGSL 对
        // storage 越界读取返回 0 ⇒ 未覆盖 tile 恒无灯（优雅降级，诚实限制
        // 见 §8.47）
        let upload_len = ranges_part.len().min(capacity_u32);
        self.gpu.queue.write_buffer(
            &self.light_bins.tiles_buf,
            0,
            bytemuck::cast_slice(&ranges_part[..upload_len]),
        );
        let params = LightBinsParamsRaw {
            screen: [
                width as f32,
                height as f32,
                bins.tiles_x as f32,
                bins.tiles_y as f32,
            ],
            counts: [1.0, lights.len() as f32, written as f32, 0.0],
        };
        self.gpu
            .queue
            .write_buffer(&self.light_bins.params_buf, 0, bytemuck::bytes_of(&params));
    }

    /// GI 探针表上传（R83，mode = 1 的 draw 每帧调用）：探针 storage + 16B
    /// 参数两缓冲。
    ///
    /// 每帧重传（而不是「变了才传」）与 R78 分箱三缓冲同纪律：探针集是烘焙
    /// 产物、没有 generation 字段可比，逐帧比对 40 KiB 的哈希不会比直接写更快；
    /// 满载 256 探针 = 40 KiB/帧（分箱帧最多写 1 MiB tile 索引，同一量级）。
    /// 只写**用到的前缀**（`count × 160 B`）：着色器循环上界 = 参数 y，尾部
    /// 陈旧数据永不被读（与灯表 `cast_slice(lights)` 同口径）。
    ///
    /// `&mut self`（R88）：mode = 1 与 mode = 2 **共用 binding 19 的参数缓冲**，
    /// 本函数每帧覆写它 ⇒ 必须让 DDGI 的「已写参数」记账失效，否则 DDGI 回归
    /// 时会以为参数还在而跳过写入（`upload_ddgi` 只在位模式变化时写）。
    fn upload_gi_probes(&mut self, set: &IrradianceProbeSet) {
        let probes = set.gpu_probes();
        if probes.is_empty() {
            return; // mode = 1 的裁决前提就是非空 SH 表；防御性早退不写缓冲
        }
        self.ddgi_upload.params = None;
        self.gpu
            .queue
            .write_buffer(&self.gi_sh.probes_buf, 0, bytemuck::cast_slice(&probes));
        let params = GiShParamsRaw {
            params: [1.0, probes.len() as f32, 0.0, 0.0],
        };
        self.gpu
            .queue
            .write_buffer(&self.gi_sh.params_buf, 0, bytemuck::bytes_of(&params));
    }

    /// DDGI 768 KiB 深度表的惰性创建（R88，首个 mode = 2 帧调用；照
    /// `ensure_occ_gpu` / AO 半分辨率可见度图的先例——缺席路径零分配大资源）。
    fn ensure_ddgi_gpu(&mut self) {
        if self.ddgi.depth_buf.is_some() {
            return;
        }
        self.ddgi.depth_buf = Some(self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.ddgi.depth"),
            size: crate::ddgi::DDGI_DEPTH_TABLE_BYTES,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        // 新建的表对渲染器而言是「全零」⇒ 记账必须失效，否则首个 mode = 2
        // 帧会以为已经同步过而跳过全量上传
        self.ddgi_upload.structure = u64::MAX;
        self.ddgi_upload.stamps.clear();
        self.ddgi_upload.params = None;
    }

    /// DDGI 表上传（R88，mode = 2 的 draw 每帧调用）：**增量**写本帧重照明
    /// 的 k 个探针段（SH 的 `k × 160 B` + 深度的 `k × 3072 B`）；网格结构
    /// 变化 / 首次启用 ⇒ 全量写。策略与偏离理由见 [`DdgiUploadState`]。
    ///
    /// 16B 参数只在**内容变化**时写（mode / 探针数 / 深度网格 / 深度项开关
    /// 四元的位模式比对）——稳态帧一个 `write_buffer` 都不发。
    fn upload_ddgi(&mut self, grid: &crate::ddgi::DdgiGrid) {
        let n = grid.probe_count();
        if n == 0 {
            return; // mode = 2 的裁决前提就是非空网格；防御性早退不写缓冲
        }
        let Some(depth_buf) = self.ddgi.depth_buf.as_ref() else {
            return; // ensure_ddgi_gpu 已在裁决段先行调用；此支只为借用检查兜底
        };
        let structure = grid.structure_generation();
        if self.ddgi_upload.structure != structure || self.ddgi_upload.stamps.len() != n {
            // 全量：网格结构变化 / 首次启用 / 绑定组重建后
            self.gpu.queue.write_buffer(
                &self.gi_sh.probes_buf,
                0,
                bytemuck::cast_slice(&grid.gpu_probes()),
            );
            self.gpu
                .queue
                .write_buffer(depth_buf, 0, bytemuck::cast_slice(grid.depth_floats()));
            self.ddgi_upload.structure = structure;
            self.ddgi_upload.stamps = grid.probe_stamp().to_vec();
        } else {
            // 增量：只写戳变化的探针段（偏移与长度都是 4 的倍数 ⇒ 满足
            // wgpu 的 COPY_BUFFER_ALIGNMENT）
            let stamps = grid.probe_stamp();
            // `probe_stamp().len() ≡ probe_count()`（install 里 `vec![tick; n]`，
            // 单测钉住）⇒ enumerate 与 `0..n` 等价
            for (i, &stamp) in stamps.iter().enumerate() {
                if self.ddgi_upload.stamps[i] == stamp {
                    continue;
                }
                self.ddgi_upload.stamps[i] = stamp;
                let raw = grid.probe_raw(i);
                self.gpu.queue.write_buffer(
                    &self.gi_sh.probes_buf,
                    (i * std::mem::size_of::<ShProbeRaw>()) as u64,
                    bytemuck::bytes_of(&raw),
                );
                self.gpu.queue.write_buffer(
                    depth_buf,
                    i as u64 * crate::ddgi::DDGI_DEPTH_PROBE_BYTES,
                    bytemuck::cast_slice(grid.depth_segment(i)),
                );
            }
        }
        let params = GiShParamsRaw {
            params: [
                2.0,
                n as f32,
                crate::ddgi::DDGI_DEPTH_GRID as f32,
                if self.ddgi_depth_weight { 1.0 } else { 0.0 },
            ],
        };
        let bits = [
            params.params[0].to_bits(),
            params.params[1].to_bits(),
            params.params[2].to_bits(),
            params.params[3].to_bits(),
        ];
        if self.ddgi_upload.params != Some(bits) {
            self.ddgi_upload.params = Some(bits);
            self.gpu
                .queue
                .write_buffer(&self.gi_sh.params_buf, 0, bytemuck::bytes_of(&params));
        }
    }

    /// 环境源失效检查：`SkySource` 资源的 generation 与缓存不同（资源缺席
    /// 视为 generation 0 = 程序化天空）时重烘焙 IBL 并重建全局绑定组。
    fn ensure_environment(&mut self, world: &iris_core::world::World) {
        let generation = world
            .try_resource::<crate::environment::SkySource>()
            .map(|s| s.generation)
            .unwrap_or(0);
        if generation == self.env_baked_generation {
            return;
        }
        let kind = world
            .try_resource::<crate::environment::SkySource>()
            .map(|s| s.kind.clone())
            .unwrap_or(crate::environment::SkySourceKind::ProceduralSky);
        log::info!("环境源变更（generation → {generation}），重新烘焙 IBL 环境贴图");
        self.env = crate::environment::bake_environment_from(&self.gpu, &kind);
        self.rebuild_globals_bind_group();
        self.env_baked_generation = generation;
    }

    /// 确保HDR 中间缓冲与当前帧尺寸一致（尺寸变化时重建）。
    fn ensure_hdr_targets(&mut self, width: u32, height: u32) {
        if self.hdr.is_some() && self.hdr_size == (width, height) {
            return;
        }
        let size = wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        };
        let msaa = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.hdr.msaa"),
            size,
            mip_level_count: 1,
            sample_count: SAMPLE_COUNT,
            dimension: wgpu::TextureDimension::D2,
            format: crate::pipeline::HDR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let resolved = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.hdr.resolved"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: crate::pipeline::HDR_FORMAT,
            // COPY_SRC（R62b）：SSR 开启时帧尾 copy 到上帧颜色副本
            //（未开启时该 usage 位无副作用）
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let msaa_view = msaa.create_view(&wgpu::TextureViewDescriptor::default());
        let resolved_view = resolved.create_view(&wgpu::TextureViewDescriptor::default());
        // 后处理中间纹理：半分辨率对（泛光链）即刻分配；全分辨率对由暗角
        // 按需分配（PostScratch 账本与纹理同步，见 post.rs）
        let mut post_scratch = crate::post::PostScratch::default();
        post_scratch.ensure(&self.gpu, crate::post::PostScale::Half, width, height);
        let (half_a_view, _) = post_scratch
            .pair_views(crate::post::PostScale::Half)
            .expect("半分辨率中间纹理对应已随集合创建");
        let make_bloom_params = || {
            self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("iris.bloom.params"),
                size: std::mem::size_of::<BloomParamsRaw>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let bloom_params: [wgpu::Buffer; 3] = [
            make_bloom_params(),
            make_bloom_params(),
            make_bloom_params(),
        ];
        let make_post_params = |label: &'static str| {
            self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: std::mem::size_of::<PostParamsRaw>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let post_params = make_post_params("iris.post.params");
        let vignette_params = make_post_params("iris.vignette.params");
        // DOF 参数 32B、MB 参数 144B（两个矩阵 + 标量），均超出 16B——
        // 独立按各自 Raw 布局建（uniform 缓冲尺寸必须与写入字节数一致）
        let dof_params = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.dof.params"),
            size: std::mem::size_of::<DofParamsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let motion_blur_params = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.motion_blur.params"),
            size: std::mem::size_of::<MbParamsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // 体积雾参数（R79）：160B（矩阵 + 相机/太阳 + 三组标量）
        let fog_params = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.fog.params"),
            size: std::mem::size_of::<FogParamsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let post_sampler = self
            .gpu
            .device
            .create_sampler(&wgpu::SamplerDescriptor::default());
        let blit_bind_group = build_blit_bind_group(
            &self.gpu.device,
            &self.bundle.blit_layout,
            &resolved_view,
            half_a_view,
            &post_sampler,
            &post_params,
        );
        self.hdr_size = (width, height);
        self.hdr = Some(HdrTargets {
            msaa,
            msaa_view,
            resolved,
            resolved_view,
            blit_bind_group,
            blit_full_a_bg: None,
            blit_full_b_bg: None,
            post_scratch,
            post_sampler,
            bloom_params,
            vignette_params,
            dof_params,
            motion_blur_params,
            fog_params,
            post_params,
            fxaa: None,
            taa: None,
        });
    }

    /// TAA 历史缓冲按需资源（R60）：双 Rgba16Float 历史 ping-pong +
    /// 参数缓冲 + 专属线性采样器 + 每半边一套读取 / blit 接入绑定组。
    /// 仅 [`crate::post::TaaSettings`] 开启时在 `draw` 开头、`hdr` 不可
    /// 变借用建立之前调用；首次创建（含分辨率变化后的重建）置位重置
    /// 请求——新历史是零初始化纹理，首帧必须整体透传当前帧。关闭路径
    /// 不触本函数（资源恒 `None`）。
    fn ensure_taa_targets(&mut self, width: u32, height: u32) {
        let hdr = self
            .hdr
            .as_mut()
            .expect("draw 开头已 ensure_hdr_targets，HDR 中间缓冲必就绪");
        if hdr.taa.is_some() {
            return;
        }
        let device = &self.gpu.device;
        let size = wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        };
        let make_hist = |label: &'static str| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: crate::pipeline::HDR_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        };
        let hist = [make_hist("iris.taa.hist.0"), make_hist("iris.taa.hist.1")];
        let hist_views = [
            hist[0].create_view(&wgpu::TextureViewDescriptor::default()),
            hist[1].create_view(&wgpu::TextureViewDescriptor::default()),
        ];
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("iris.taa.sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.taa.params"),
            size: std::mem::size_of::<TaaParamsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // TAA 读取绑定组由 encode_taa_pass 每帧按视图现建（框架惯例），
        // 这里只预建 blit 接入组（HDR 源 = 历史半边；泛光恒半分辨率 A 槽）
        let bloom_view = hdr
            .post_scratch
            .pair_views(crate::post::PostScale::Half)
            .expect("半分辨率中间纹理对应已随集合创建")
            .0;
        let make_blit_bg = |hdr_view: &wgpu::TextureView| {
            build_blit_bind_group(
                device,
                &self.bundle.blit_layout,
                hdr_view,
                bloom_view,
                &hdr.post_sampler,
                &hdr.post_params,
            )
        };
        let blit_bgs = [make_blit_bg(&hist_views[0]), make_blit_bg(&hist_views[1])];
        hdr.taa = Some(TaaTargets {
            hist,
            hist_views,
            params,
            sampler,
            blit_bgs,
        });
        self.taa_reset_pending = true;
    }

    /// 显式请求 TAA 历史重置（R60）：下一帧 TAA pass 混合系数置 0、
    /// 输出逐位等于当前帧（无历史贡献），再下一帧恢复正常累积。相机
    /// 切镜 / 大幅传送 / 场景整体替换后由宿主调用——切镜阈值检测是
    /// 内容启发式（拒绝清单），决策交给知道切镜时刻的一方。
    ///
    /// **语义（R89 起定稿）=「清一切跨帧时域状态，除了探针域」**：
    /// - `taa_reset_pending`：TAA 历史（R60 原语义）；
    /// - `ssr_prev_valid`：SSR 上帧颜色/深度副本（R62b 跟随）；
    /// - `ao_depth_frame`：GTAO 上帧深度副本记账（R89 补齐——iris-exp
    ///   frictions ⑬ 实锤：切镜后首帧 GTAO 仍按旧场景深度算可见度，旧
    ///   腿形投出假遮蔽并被 TAA 历史拖尾多帧）。清空后首帧
    ///   `ao_depth_frame` 与 `occ_frame − 1` 失配 ⇒ GTAO 中性 1.0（输出
    ///   与 AO 关闭帧逐位一致）、`FrameStats::ao_on == 0`，第二帧起正确；
    /// - `prev_view_proj`：MB / TAA / SSR 的上帧矩阵（R89 补齐）——消费点
    ///   是 `unwrap_or(vp_raw)`（首帧语义 = 当前帧矩阵补位 ⇒ 速度恒 0、
    ///   原色透传），**清空恰是「无历史」语义本身**，与 TAA 重置同款；
    /// - **故意不清**：`ddgi_upload` / `DdgiGrid` 表内容 / `probe_stamp` /
    ///   `structure_generation`（探针域状态与镜头无关——镜头切走再切回，
    ///   探针辐射场仍是同一个物理量；锚测试
    ///   `reset_taa_does_not_disturb_ddgi` 钉住）。
    ///
    /// 不调 reset_taa 的宿主逐位不变；调了但没有 GTAO / SSR 的宿主也逐位
    /// 不变（`ao_depth_frame` 本就是 None，`ssr_prev_valid` 本就是 false）。
    pub fn reset_taa(&mut self) {
        self.taa_reset_pending = true;
        // SSR 跟随（R62b）：reset_taa 的语义是「宿主宣告切镜」，切镜让
        // 一切跨帧时域状态过期——TAA 历史与 SSR 的上帧颜色/深度同理
        // （上帧是另一个场景）。下一帧 SSR 整段跳过（输出 = 关闭帧），
        // 之后随帧尾副本恢复。成本是切镜帧的反射回退探针/env 一帧，
        // 收益是杜绝「新场景地板反射旧场景」的一帧闪现。
        self.ssr_prev_valid = false;
        // GTAO 跟随（R89，frictions ⑬）：上帧深度副本属于旧场景 ⇒ 记账
        // 失配 ⇒ 首帧可见度图中性 1.0。
        self.ao_depth_frame = None;
        // 上帧矩阵（R89）：清空 = 首帧语义（MB 零速度透传），与「无历史」
        // 等价；下一帧照常保存当前帧矩阵。
        self.prev_view_proj = None;
    }

    /// 切换阴影偏移口径到 **v1 对照模式**（R61，验收 / 归因口）。
    ///
    /// `legacy = true` 时 uniform 写回 v1 的常数 bias（方向光 0.0015 NDC、
    /// 点光 0.03 m）并把坡度缩放项与法线偏移的系数置零——阴影采样路径
    /// 在数值上与 R60 逐位等价（矩阵、贴图、PCF 权重、掩码全部不变，只有
    /// 偏移量回到常数）。用途仅两类：
    ///
    /// 1. **验收图对照面板**（`examples/shadow_v2_snapshot.rs`：同一次
    ///    运行内渲染「旧常数 bias（acne / peter-panning 可见）」与
    ///    「新技术（干净）」两帧——跨版本截图无法在同一 PNG 里对照）；
    /// 2. **归因测试**（`tests/shadow_v2_render.rs`：同场景两种口径的
    ///    acne 像素数与接触暗带宽度对照）。
    ///
    /// 这不是「运行时 bias 可视化调试视图」（拒绝清单）：不输出任何
    /// 诊断可视化，只在两个编译期常量集之间切换；生产路径恒 `false`，
    /// 默认帧的输出与该开关不存在时逐位一致。
    pub fn set_legacy_shadow_bias(&mut self, legacy: bool) {
        self.legacy_shadow_bias = legacy;
    }

    /// 反射探针的视差修正**对照模式**（R62a，验收 / 归因专用）：`true` =
    /// 关闭 box projection（探针采样直接用反射方向），用于验收图的左右面板
    /// 对照与「修正确实改变采样几何」的归因。生产路径恒 `false`；本开关
    /// 不改变探针的捕获 / 预滤波 / 选择裁决，只改采样方向的重定向。
    pub fn set_legacy_probe_parallax(&mut self, legacy: bool) {
        self.legacy_probe_no_parallax = legacy;
    }

    /// DDGI 深度项的**对照模式**（R88，验收 / 归因专用）：`false` = 关闭
    /// chebyshev 深度权重（着色器侧恒 1）⇒ 混合退回「只有 R83 径向窗」的
    /// 形态，同一二进制内复现 §8.52① 的墙两侧串味，供 `docs/ddgi.png` 的
    /// 漏光对照面板与「深度项确实抑制了漏光」的归因测试
    /// （`tests/ddgi_render.rs`）。生产路径恒 `true`（默认）；本开关不改变
    /// 重照明 / 深度表的**内容**（CPU 侧照算照传），只改片元的权重口径 ⇒
    /// 两侧的差异只能来自深度项。
    ///
    /// 照 [`Self::set_legacy_probe_parallax`] / [`Self::set_legacy_shadow_bias`]
    /// 先例：不是运行时诊断可视化，只在两个口径之间切换；参数块的 w 分量
    /// 变化由 `upload_ddgi` 的位模式比对自动补写。
    pub fn set_ddgi_depth_weight_enabled(&mut self, enabled: bool) {
        self.ddgi_depth_weight = enabled;
    }

    /// 该实体的反射探针是否已烘焙（观测口径，GPU 测试用）。
    pub fn probe_is_baked(&self, entity: Entity) -> bool {
        self.probe_store.slot_of(entity.to_bits()).is_some()
    }

    /// 已烘焙的反射探针数。
    pub fn baked_probe_count(&self) -> usize {
        self.probe_store.len()
    }

    /// 烘焙反射探针（R62a）：从实体当前世界位置向 6 面捕获场景 cubemap，
    /// 再按 GGX 跑完整条预滤波 mip 链（复用 env 的 `fs_prefilter` 与同一
    /// 粗糙度 → mip 映射）。`&mut self` 入口（与 [`Self::reset_taa`] 同款：
    /// 需要分配 / 复用 GPU 资源）。
    ///
    /// # 捕获内容与取舍（决策点）
    ///
    /// **捕获含天空**：6 个面都先画一遍天空盒（复用 `skybox.wgsl` 与
    /// `inv_view_proj` 射线重建，与主视口逐像素同源），再画不透明几何。
    /// 与之相对的做法是「天空清成黑、盒外方向回落全局 env」，但那需要
    /// **逐 texel 的有效性掩码 + 混合**（哪些 texel 打到了几何、哪些打到
    /// 了空），且盒内本来能看到的天空（窗口 / 天窗）会丢失。含天空的产物
    /// 是**完整辐射度函数**：域内任意方向的反射都有值，采样侧只需一次
    /// `textureSampleLevel`，无需任何回退混合。代价写进诚实限制：探针烘焙
    /// 与全局 env 的 `SkySource` generation 绑定——**换天空源后探针内容不再
    /// 匹配**，需调用方重新烘焙（v1 不自动重烘，与不自动跟随场景变化同一
    /// 条限制）。
    ///
    /// # 捕获内容清单
    ///
    /// - 方向光阴影：以**探针盒**做单级联拟合（[`fit_probe_shadow`]），
    ///   渲染进主帧的级联槽 0 后供 6 面捕获采样——室内场景的太阳光因此
    ///   被墙 / 天花板正确遮挡（否则封闭房间的地板会在探针里"晒到太阳"）。
    ///   槽 0 是**临时借用**：主帧每帧重画级联，下一次 `draw` 即覆盖。
    /// - 点光：按世界位置进入 uniform，但**阴影关闭**（探针域没有点光
    ///   cubemap 的槽位归属，逐灯重捕获代价高）——诚实限制。
    /// - 不画：透明 / 粒子 / 贴花（与点光阴影捕获同一取舍）。
    /// - 探针自身不参与捕获（捕获绑定组绑零参数 + 哑 cubemap），避免
    ///   「反射里看到反射」的自反馈。
    ///
    /// # 确定性
    ///
    /// 捕获是一次性的命令缓冲（阴影 pass → 6 面 → 预滤波 mip 链）单次
    /// submit，输入只取世界状态（几何 + 光照 + 探针影响域）；同输入两次
    /// 烘焙逐位一致（`tests/probe_render.rs` 双会话哈希锁）。
    ///
    /// 返回 `false`（并 `log::warn`）的情形：实体缺 [`ReflectionProbe`] /
    /// `GlobalTransform`、影响域退化（半边长非正）、cubemap 槽位预算耗尽、
    /// 世界缺 `MeshStore` / `MaterialStore`。
    pub fn bake_reflection_probe(
        &mut self,
        world: &iris_core::world::World,
        entity: Entity,
    ) -> bool {
        let Some(probe) = world.get::<ReflectionProbe>(entity).map(|p| *p) else {
            log::warn!("实体 {entity:?} 没有 ReflectionProbe 组件，无法烘焙");
            return false;
        };
        if !probe.is_valid() {
            log::warn!(
                "实体 {entity:?} 的 ReflectionProbe 影响域退化（half_extents = {:?}），跳过烘焙",
                probe.half_extents
            );
            return false;
        }
        let Some(gt) = world.get::<GlobalTransform>(entity) else {
            log::warn!("实体 {entity:?} 缺 GlobalTransform，无法确定探针位置");
            return false;
        };
        let center = gt.position();
        let Some(slot) = self.probe_store.ensure_slot(entity.to_bits()) else {
            log::warn!("反射探针槽位预算 {MAX_REFLECTION_PROBES} 已用尽，实体 {entity:?} 未烘焙");
            return false;
        };
        let (Some(mesh_store), Some(material_store)) = (
            world.try_resource::<MeshStore>(),
            world.try_resource::<MaterialStore>(),
        ) else {
            log::warn!("世界缺 MeshStore / MaterialStore 资源，反射探针无法捕获");
            return false;
        };
        let texture_store = world.try_resource::<TextureStore>();
        let lightmap_store = world.try_resource::<LightmapStore>();

        // ---- 收集（6 面并集 = 完整球域 ⇒ 「全通过」视锥，逐面剔除零收益）
        let frustum = Frustum::capture_all(center, PROBE_CAPTURE_FAR);
        let mut items: Vec<DrawItem> = Vec::new();
        let mut skinned: Vec<SkinnedItem> = Vec::new();
        let mut transparent: Vec<TransparentItem> = Vec::new();
        let mut transparent_skinned: Vec<TransparentSkinnedItem> = Vec::new();
        // `view` 只被透明项排序用（v1 不画透明物），传单位矩阵并忽略其结果
        collect_instances(
            world,
            &mesh_store,
            &material_store,
            &Mat4::IDENTITY,
            center,
            &frustum,
            &mut items,
            &mut skinned,
            &mut transparent,
            &mut transparent_skinned,
            // 探针捕获 = v1 选择口径（当帧纯距离、无滞回记忆）——捕获
            // 上下文的历史不该污染主视图，LodSettings 只作用于主相机收集
            None,
            None,
        );
        // 与主 pass 同款合批：不透明按 (网格, 材质) 键排序（同键区间一次 draw）
        items.sort_by_key(|item| item.key);
        let total = items.len();
        let mut raws: Vec<InstanceRaw> = Vec::with_capacity(total + skinned.len());
        raws.extend(items.iter().map(|item| item.instance));
        raws.extend(skinned.iter().map(|item| item.instance));

        // ---- GPU 资源懒上传（与 draw 步骤 4 同款；捕获用独立实例缓冲）----
        for item in &items {
            let key = item.key;
            self.ensure_mesh(&mesh_store, MeshHandle((key >> 32) as u32));
            self.ensure_material(
                &material_store,
                texture_store.as_deref(),
                lightmap_store.as_deref(),
                MaterialHandle(key as u32),
                item.lightmap,
            );
        }
        for item in &skinned {
            self.ensure_mesh(&mesh_store, MeshHandle(item.mesh));
            self.ensure_material(
                &material_store,
                texture_store.as_deref(),
                lightmap_store.as_deref(),
                MaterialHandle(item.material),
                item.lightmap,
            );
        }
        let mut joint_bytes: Vec<u8> = Vec::new();
        for item in &skinned {
            self.ensure_skin(item.skin_entity, item.joint_matrices.len());
            joint_bytes.clear();
            for m in &item.joint_matrices {
                joint_bytes.extend_from_slice(bytemuck::bytes_of(&m.to_cols_array_2d()));
            }
            let skin = self
                .skin_gpu
                .get(&item.skin_entity)
                .expect("蒙皮缓冲已就绪");
            self.gpu
                .queue
                .write_buffer(&skin.joint_buf, 0, &joint_bytes);
        }
        // 形变目标解算（C4a，蒙皮绘制前的 slot 0 覆盖；全体缺席时零触碰）
        self.resolve_morphs(&mesh_store, &mut skinned, &mut transparent_skinned);
        self.ensure_probe_instance_capacity(raws.len());
        if !raws.is_empty() {
            let instance_buf = self
                .probe_instance_buf
                .as_ref()
                .expect("捕获实例缓冲已就绪");
            self.gpu
                .queue
                .write_buffer(instance_buf, 0, bytemuck::cast_slice(&raws));
        }

        // ---- 光照：方向光（含探针盒单级联阴影）+ 点光（无阴影）----
        let light = world
            .try_resource::<DirectionalLight>()
            .map(|l| (*l).clone())
            .unwrap_or_default();
        let light_dir = (-light.direction).normalize_or_zero();
        let env_intensity = world
            .try_resource::<crate::environment::EnvironmentSettings>()
            .map(|e| e.intensity.max(0.0))
            .unwrap_or(1.0);
        let mut point_lights = [PointLightRaw {
            position: [0.0; 4],
            color: [0.0; 4],
            shadow: [0.0; 4],
        }; MAX_POINT_LIGHTS];
        let mut point_count = 0usize;
        world.query::<(&GlobalTransform, &PointLight)>(|(gt, pl)| {
            if point_count >= MAX_POINT_LIGHTS {
                return;
            }
            let position = gt.position();
            point_lights[point_count] = PointLightRaw {
                position: [position.x, position.y, position.z, 0.0],
                color: [
                    pl.color.x * pl.intensity,
                    pl.color.y * pl.intensity,
                    pl.color.z * pl.intensity,
                    pl.range,
                ],
                // 捕获不做点光阴影（诚实限制）：开关恒 0
                shadow: [0.0, 0.0, 0.0, 0.0],
            };
            point_count += 1;
        });
        let fit = fit_probe_shadow(light.direction, center, probe.half_extents);
        let shadow_enabled = light.shadows;
        let legacy = self.legacy_shadow_bias;
        for face in 0..6 {
            let vp = crate::light::cube_face_view_proj(
                center,
                face,
                PROBE_CAPTURE_NEAR,
                PROBE_CAPTURE_FAR,
            );
            let raw = GlobalsRaw {
                view_proj: vp.to_cols_array_2d(),
                eye_pos: [center.x, center.y, center.z, 1.0],
                light_dir: [light_dir.x, light_dir.y, light_dir.z, 0.0],
                light_color: [
                    light.color.x * light.intensity,
                    light.color.y * light.intensity,
                    light.color.z * light.intensity,
                    0.0,
                ],
                ambient: [light.ambient.x, light.ambient.y, light.ambient.z, 0.0],
                point_count: [point_count as f32, 0.0, 0.0, 0.0],
                point_lights,
                shadow_params: [
                    1.0 / crate::pipeline::SHADOW_MAP_SIZE as f32,
                    if legacy {
                        SHADOW_BIAS_LEGACY
                    } else {
                        SHADOW_BIAS_MIN
                    },
                    SHADOW_STRENGTH,
                    if shadow_enabled { 1.0 } else { 0.0 },
                ],
                env_params: [
                    crate::environment::ENV_MIP_COUNT as f32 - 1.0,
                    env_intensity,
                    0.0,
                    0.0,
                ],
                inv_view_proj: vp.inverse().to_cols_array_2d(),
                // 无点光阴影：全局掩码 0，逐灯开关也恒 0
                point_shadow_params: [
                    0.0,
                    POINT_SHADOW_BIAS_MIN,
                    POINT_SHADOW_SLOPE_SCALE,
                    POINT_SHADOW_NORMAL_SCALE,
                ],
            };
            self.gpu.queue.write_buffer(
                &self.probe_face_globals_bufs[face],
                0,
                bytemuck::bytes_of(&raw),
            );
        }
        // 级联槽 0 = 探针盒拟合；splits 全为 +∞、cam_forward 置零 ⇒ 片元的
        // 「view 深度」恒 0 < splits.x ⇒ 所有片元选级联 0（单级联语义）。
        // 槽 1/2 填单位矩阵占位（该帧不会被选中，但 uniform 必须有定义）。
        const PROBE_SPLIT_INFINITY: f32 = 1.0e30;
        let csm_raw = CsmParamsRaw {
            view_proj: [
                fit.view_proj.to_cols_array_2d(),
                Mat4::IDENTITY.to_cols_array_2d(),
                Mat4::IDENTITY.to_cols_array_2d(),
            ],
            cam_forward: [0.0; 4],
            splits: [
                PROBE_SPLIT_INFINITY,
                PROBE_SPLIT_INFINITY,
                PROBE_SPLIT_INFINITY,
                0.0,
            ],
            cascade_texel: [if legacy { 0.0 } else { fit.texel_world }, 0.0, 0.0, 0.0],
            cascade_slope_k: [
                if legacy {
                    0.0
                } else {
                    fit.texel_world / fit.depth_window.max(1e-6)
                },
                0.0,
                0.0,
                0.0,
            ],
        };
        self.gpu
            .queue
            .write_buffer(&self.csm_params_buf, 0, bytemuck::bytes_of(&csm_raw));
        let cascade_raw = CascadeParamsRaw {
            view_proj: fit.view_proj.to_cols_array_2d(),
        };
        self.gpu
            .queue
            .write_buffer(&self.cascade_bufs[0], 0, bytemuck::bytes_of(&cascade_raw));

        // ---- 编码：阴影 pass → 6 面捕获（天空 + 不透明）→ 预滤波 mip 链 ----
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("iris.probe.bake"),
            });
        if shadow_enabled && !items.is_empty() {
            let instance_buf = self
                .probe_instance_buf
                .as_ref()
                .expect("捕获实例缓冲已就绪");
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iris.probe.shadow"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.shadow_color_views[0],
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 1.0,
                            g: 1.0,
                            b: 1.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.shadow_depth_views[0],
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.bundle.shadow_pipeline);
            pass.set_bind_group(0, &self.cascade_bgs[0], &[]);
            pass.set_vertex_buffer(1, instance_buf.slice(..));
            let mut index = 0;
            while index < total {
                let key = items[index].key;
                let count = run_length(&items, index);
                let mesh = &self.mesh_gpu[&((key >> 32) as u32)];
                pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
                pass.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                let first = index as u32;
                let last = (index + count) as u32;
                pass.draw_indexed(0..mesh.num_indices, 0, first..last);
                index += count;
            }
            if !skinned.is_empty() {
                pass.set_pipeline(&self.bundle.skinned_shadow_pipeline);
                pass.set_vertex_buffer(2, instance_buf.slice(..));
                for (i, item) in skinned.iter().enumerate() {
                    let mesh = &self.mesh_gpu[&item.mesh];
                    let skin = &self.skin_gpu[&item.skin_entity];
                    pass.set_vertex_buffer(0, item.vertex_buf(mesh).slice(..));
                    pass.set_vertex_buffer(
                        1,
                        mesh.skin_buf
                            .as_ref()
                            .expect("蒙皮网格应有蒙皮属性缓冲")
                            .slice(..),
                    );
                    pass.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.set_bind_group(1, &skin.bind_group, &[]);
                    let base = (total + i) as u32;
                    pass.draw_indexed(0..mesh.num_indices, 0, base..base + 1);
                }
            }
        }
        if !items.is_empty() || !skinned.is_empty() {
            let instance_buf = self
                .probe_instance_buf
                .as_ref()
                .expect("捕获实例缓冲已就绪");
            for face in 0..6 {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("iris.probe.capture"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.probe_raw_face_views[face],
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            // 清黑后立刻被天空覆盖：天空缺席（无 env）时也不会
                            // 把未初始化内容带进预滤波
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &self.probe_raw_depth_face_views[face],
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Discard,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                // 天空（含天空决策，见方法文档）：同一 skybox.wgsl，面矩阵的
                // 逆给出该像素在立方体面上的方向。R79 起用专用 sky 绑定组
                // （大气参数与主相机同一份——探针里的天空与主视口同源）
                pass.set_pipeline(&self.bundle.probe_sky_pipeline);
                pass.set_bind_group(0, &self.probe_face_sky_bgs[face], &[]);
                pass.draw(0..3, 0..1);
                // 不透明几何（捕获变体：单采样 Rgba16Float 目标 + cull None——
                // 面矩阵的 NDC y 翻转会反转环绕方向，Back 剔除会剔错面）
                pass.set_pipeline(&self.bundle.probe_capture_pipeline);
                pass.set_bind_group(0, &self.probe_face_globals_bgs[face], &[]);
                pass.set_vertex_buffer(1, instance_buf.slice(..));
                let mut index = 0;
                while index < total {
                    let key = items[index].key;
                    let lightmap = items[index].lightmap;
                    let count = run_length(&items, index);
                    let mesh = &self.mesh_gpu[&((key >> 32) as u32)];
                    let material = &self.material_gpu[&material_key(key as u32, lightmap)];
                    pass.set_pipeline(if items[index].double_sided {
                        &self.bundle.probe_capture_double_sided_pipeline
                    } else {
                        &self.bundle.probe_capture_pipeline
                    });
                    pass.set_bind_group(1, &material.bind_group, &[]);
                    pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
                    pass.set_vertex_buffer(2, self.tangent_buf(mesh).slice(..));
                    pass.set_vertex_buffer(3, self.uv2_buf(mesh).slice(..));
                    pass.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    let first = index as u32;
                    let last = (index + count) as u32;
                    pass.draw_indexed(0..mesh.num_indices, 0, first..last);
                    index += count;
                }
                if !skinned.is_empty() {
                    pass.set_pipeline(&self.bundle.probe_capture_skinned_pipeline);
                    pass.set_vertex_buffer(2, instance_buf.slice(..));
                    for (i, item) in skinned.iter().enumerate() {
                        let mesh = &self.mesh_gpu[&item.mesh];
                        let material =
                            &self.material_gpu[&material_key(item.material, item.lightmap)];
                        let skin = &self.skin_gpu[&item.skin_entity];
                        pass.set_pipeline(if item.double_sided {
                            &self.bundle.probe_capture_double_sided_skinned_pipeline
                        } else {
                            &self.bundle.probe_capture_skinned_pipeline
                        });
                        pass.set_bind_group(1, &material.bind_group, &[]);
                        pass.set_bind_group(2, &skin.bind_group, &[]);
                        pass.set_vertex_buffer(0, item.vertex_buf(mesh).slice(..));
                        pass.set_vertex_buffer(3, self.tangent_buf(mesh).slice(..));
                        pass.set_vertex_buffer(4, self.uv2_buf(mesh).slice(..));
                        pass.set_vertex_buffer(
                            1,
                            mesh.skin_buf
                                .as_ref()
                                .expect("蒙皮网格应有蒙皮属性缓冲")
                                .slice(..),
                        );
                        pass.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                        let base = (total + i) as u32;
                        pass.draw_indexed(0..mesh.num_indices, 0, base..base + 1);
                    }
                }
            }
        }
        // 预滤波：源 = 捕获擦写立方体（读 mip 0），目标 = 该探针槽位的 6 层
        // mip 链（层基址 = 槽位 × 6）。与 env 同一条链、同一粗糙度映射。
        crate::environment::encode_prefilter_chain(
            &self.gpu,
            &mut encoder,
            &self.probe_raw_cube_view,
            &self.probe_map,
            (slot * 6) as u32,
        );
        self.gpu.queue.submit([encoder.finish()]);
        log::info!(
            "反射探针烘焙完成：实体 {entity:?} @ ({:.2}, {:.2}, {:.2})，影响域 ±({:.2}, {:.2}, {:.2})，\
             槽位 {slot}，捕获 {} 个不透明实例 + {} 个蒙皮实例（{} 面 × {PROBE_MIP_COUNT} 级 mip）",
            center.x,
            center.y,
            center.z,
            probe.half_extents.x,
            probe.half_extents.y,
            probe.half_extents.z,
            items.len(),
            skinned.len(),
            crate::probe::PROBE_MAP_SIZE
        );
        true
    }

    /// 暗角 / 景深 / 运动模糊任一开启时的按需资源：全分辨率中间纹理对
    /// （ping-pong 分配器惰性分配、跨帧复用）与「blit 读全分辨率链尾」
    /// 的 A / B 两个接入绑定组。随 `HdrTargets` 尺寸重建一并失效（
    /// `blit_full_*_bg` 归 `None`、纹理对重新按需分配）。仅在 `draw`
    /// 开头、`hdr` 不可变借用建立之前调用。
    fn ensure_full_targets(&mut self, width: u32, height: u32) {
        let hdr = self
            .hdr
            .as_mut()
            .expect("draw 开头已 ensure_hdr_targets，HDR 中间缓冲必就绪");
        hdr.post_scratch
            .ensure(&self.gpu, crate::post::PostScale::Full, width, height);
        if hdr.blit_full_a_bg.is_none() || hdr.blit_full_b_bg.is_none() {
            let (full_a_view, full_b_view) = hdr
                .post_scratch
                .pair_views(crate::post::PostScale::Full)
                .expect("全分辨率中间纹理对应已按需创建");
            let bloom_view = hdr
                .post_scratch
                .pair_views(crate::post::PostScale::Half)
                .expect("半分辨率中间纹理对应已随集合创建")
                .0;
            let make = |hdr_view: &wgpu::TextureView| {
                build_blit_bind_group(
                    &self.gpu.device,
                    &self.bundle.blit_layout,
                    hdr_view,
                    bloom_view,
                    &hdr.post_sampler,
                    &hdr.post_params,
                )
            };
            hdr.blit_full_a_bg = Some(make(full_a_view));
            hdr.blit_full_b_bg = Some(make(full_b_view));
        }
    }

    /// 主相机场景深度副本的按需资源（R32Float 线性视距 + Depth32Float
    /// 附件 + 相机矩阵 uniform 与绑定组）。仅 DOF / MB / SSR 启用时调用
    /// （`draw` 开头、`hdr` 借用建立之前）；尺寸变化时整体重建。返回本
    /// 次调用是否（重）建了纹理——SSR 据此判断是否需要重建全局绑定组
    /// （其 binding 12 指向本纹理的视图）。
    fn ensure_scene_depth(&mut self, width: u32, height: u32) -> bool {
        if self.scene_depth.is_some() && self.scene_depth_size == (width, height) {
            return false;
        }
        let size = wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        };
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.scene_depth.texture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let depth = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.scene_depth.depth"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
        let params = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.scene_depth.params"),
            size: std::mem::size_of::<CascadeParamsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = self
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("iris.scene_depth.bg"),
                layout: &self.bundle.cascade_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                }],
            });
        self.scene_depth_size = (width, height);
        self.scene_depth = Some(SceneDepthTargets {
            texture,
            view,
            depth,
            depth_view,
            params,
            bind_group,
        });
        true
    }

    /// SSR 上帧颜色副本的按需资源（R62b）：主 HDR 解析缓冲的同尺寸副本
    /// 纹理（COPY_DST | TEXTURE_BINDING），`SsrSettings` 首次开启时创建、
    /// 分辨率变化时重建。返回本次是否（重）建——重建即「无上帧」（新
    /// 纹理是空的），调用方据此置 `ssr_prev_valid = false`（重置语义）。
    fn ensure_ssr_prev_color(&mut self, width: u32, height: u32) -> bool {
        if self.ssr.prev_color.is_some() && self.ssr.prev_size == (width, height) {
            return false;
        }
        let size = wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        };
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.ssr.prev_color"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: crate::pipeline::HDR_FORMAT,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.ssr.prev_color = Some(SsrPrevColor { texture, view });
        self.ssr.prev_size = (width.max(1), height.max(1));
        true
    }

    /// GTAO 可见度图按需资源（R77）：半分辨率 R16Float（RENDER_ATTACHMENT
    /// | TEXTURE_BINDING）。仅 [`crate::post::AoSettings`] 开启时在 `draw`
    /// 开头、`hdr` 不可变借用建立之前调用；尺寸不符整体重建（旧纹理随之
    /// 释放）。返回 true = 本次（重）建——新纹理内容不是上一帧深度的产物
    /// ⇒ binding 14 需重新指向（全局绑定组重建）。
    fn ensure_ao_targets(&mut self, width: u32, height: u32) -> bool {
        let size = ((width / 2).max(1), (height / 2).max(1));
        if self.ao.target.is_some() && self.ao.target_size == size {
            return false;
        }
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.ao.visibility"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.ao.target = Some(AoTarget { texture, view });
        self.ao.target_size = size;
        true
    }

    /// 平面水体按需资源（R81）：反射 / 折射两张半分辨率捕获图 + 水体
    /// 绑定组 + 每捕获相机一份 Globals / 天空绑定组 + 静态水面网格。
    /// 仅 [`crate::water::WaterSurface`] 首次在场时创建、随窗口尺寸变化
    /// 整体重建（半分辨率口径）；无水体帧不触本函数。须在
    /// `ensure_scene_depth` 之后调用——绑定组要绑本帧 scene_depth 视图
    /// （两者复用判定键同源，尺寸变化必然同步重建）。
    fn ensure_water_targets(&mut self, width: u32, height: u32) {
        let size = ((width / 2).max(1), (height / 2).max(1));
        if self.water.is_some() && self.water_size == size {
            return;
        }
        let extent = wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        };
        let make_capture = |label: &'static str| {
            let color = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                // Rgba16Float：与探针捕获同格式（线性 HDR，可过滤——
                // 波纹扰动后的采样在亚纹素位置）。半分辨率的理由：反射
                // / 折射都是低频近似内容（水面本身还做逐片元 Fresnel 与
                // 扰动），全分辨率带宽翻倍无观感收益。
                format: crate::pipeline::HDR_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let depth = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = color.create_view(&wgpu::TextureViewDescriptor::default());
            let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
            (color, view, depth, depth_view)
        };
        let (refl_color, refl_view, refl_depth, refl_depth_view) = make_capture("iris.water.refl");
        let (refr_color, refr_view, refr_depth, refr_depth_view) = make_capture("iris.water.refr");
        let params_buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.water.params"),
            size: std::mem::size_of::<WaterParamsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // 水体绑定组：反射 / 折射捕获图 + 本帧 scene_depth（水体 pass 的
        // 深度吸收输入——scene_depth 视图随 `scene_depth_size` 键重建，
        // 本函数与之同键同步重建，视图恒有效）
        let sd = self
            .scene_depth
            .as_ref()
            .expect("水体资源须在 ensure_scene_depth 之后创建");
        let sampler = self.gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("iris.water.sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind_group = self
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("iris.water.bg"),
                layout: &self.bundle.water_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: params_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&refl_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&refr_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&sd.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            });
        // 每捕获相机一份 Globals 缓冲 + 绑定组 + 天空绑定组（探针捕获
        // 同款最低配组装：绑定 0..8 与主组同源——阴影贴图 / env / CSM
        // 复用本帧主相机的阴影产物；绑定 9/10 探针走零参数 + 哑视图（无
        // 自反馈）、SSR 三路走零参数 + 哑纹理（捕获无上帧语义）、AO 哑
        // 纹素 1.0、分箱哑零（捕获不渲染锥光与第 9+ 盏点光）。
        let make_capture_globals = |buf_label: &'static str| {
            let globals_buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(buf_label),
                size: std::mem::size_of::<GlobalsRaw>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let globals_bg = build_globals_bind_group(
                &self.gpu,
                &self.bundle,
                GlobalsBuffers {
                    globals: &globals_buf,
                    csm_params: &self.csm_params_buf,
                },
                ShadowMapViews {
                    cascades: &self.shadow_map_view,
                    point_cube: &self.point_shadow_cube_view,
                },
                &self.env,
                ProbeGlobalsResources {
                    params_buf: &self.probe_capture_params_buf,
                    cube_view: &self.probe_dummy_view,
                    gi_probes: &self.gi_sh.off_probes_buf,
                    gi_params: &self.gi_sh.off_params_buf,
                    gi_depth: &self.ddgi.off_depth_buf,
                },
                ScreenSpaceGlobalsResources {
                    params_buf: &self.ssr.off_params_buf,
                    color_view: &self.ssr.dummy_color_view,
                    depth_view: &self.ssr.dummy_depth_view,
                    ao_view: &self.ao.dummy_view,
                    bins_params: &self.light_bins.off_params_buf,
                    bins_lights: &self.light_bins.off_lights_buf,
                    bins_tiles: &self.light_bins.off_tiles_buf,
                },
            );
            (globals_buf, globals_bg)
        };
        let (refl_globals_buf, refl_globals_bg) = make_capture_globals("iris.water.refl_globals");
        let (refr_globals_buf, refr_globals_bg) = make_capture_globals("iris.water.refr_globals");
        // 天空绑定组：与捕获 Globals 缓冲一一对应（捕获内的天空与主视口
        // 同一 skybox.wgsl 同一 SkyParams——大气对捕获相机同样生效）
        let refl_sky_bg = build_sky_bind_group(
            &self.gpu,
            &self.bundle,
            &self.env,
            SkyBindResources {
                globals: &refl_globals_buf,
                sky_params: &self.sky_params_buf,
            },
        );
        let refr_sky_bg = build_sky_bind_group(
            &self.gpu,
            &self.bundle,
            &self.env,
            SkyBindResources {
                globals: &refr_globals_buf,
                sky_params: &self.sky_params_buf,
            },
        );
        // 静态水面网格：归一化 [−1,1]² 格点（顶点着色器乘 half_extent 展开
        // + Gerstner 位移），CPU 一次生成 upload（纯函数 + 单测）
        let (grid_vertices, grid_indices) =
            crate::water::water_grid_mesh(crate::water::WATER_GRID_SUBDIVISIONS);
        let grid_vertex_buf =
            self.gpu
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("iris.water.grid_vertices"),
                    contents: bytemuck::cast_slice(&grid_vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
        let grid_index_buf =
            self.gpu
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("iris.water.grid_indices"),
                    contents: bytemuck::cast_slice(&grid_indices),
                    usage: wgpu::BufferUsages::INDEX,
                });
        self.water_size = size;
        self.water = Some(WaterGpu {
            refl_color,
            refl_view,
            refl_depth,
            refl_depth_view,
            refr_color,
            refr_view,
            refr_depth,
            refr_depth_view,
            params_buf,
            bind_group,
            refl_globals_buf,
            refl_globals_bg,
            refl_sky_bg,
            refr_globals_buf,
            refr_globals_bg,
            refr_sky_bg,
            instance_buf: None,
            instance_capacity: 0,
            grid_vertex_buf,
            grid_index_buf,
            grid_index_count: grid_indices.len() as u32,
        });
        log::debug!(
            "水体 GPU 资源已构建（捕获 {size:?}，网格 {} 顶点 / {} 索引）",
            grid_vertices.len(),
            grid_indices.len()
        );
    }

    /// 水体反射捕获实例缓冲容量复用（与主实例缓冲同款——容量不足时
    /// 2 倍扩容整体重建）。捕获每帧全量上传（视锥结果常变，帧级比对
    /// 无增量价值）。
    fn ensure_water_instance_capacity(&mut self, needed: usize) {
        if self
            .water
            .as_ref()
            .is_some_and(|w| w.instance_capacity >= needed)
        {
            return;
        }
        let capacity = (needed * 2).max(256);
        let buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.water.instances"),
            size: (capacity * std::mem::size_of::<InstanceRaw>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let water = self.water.as_mut().expect("水体资源已在此前创建");
        water.instance_buf = Some(buf);
        water.instance_capacity = capacity;
    }

    pub fn set_clear_color(&mut self, color: wgpu::Color) {
        self.clear_color = color;
    }

    /// FXAA 按需资源（R53）：中间 LDR 纹理（与最终目标同格式，blit 管线
    /// 原样可写）+ 16B 参数缓冲 + `post_layout` 绑定组。仅
    /// [`crate::post::FxaaSettings`] 开启时在 `draw` 开头、`hdr` 不可变
    /// 借用建立之前调用；关闭路径不触本函数（资源恒 `None`）。随
    /// `HdrTargets` 尺寸重建整体失效、下次开启惰性重建。采样器用专属
    /// 线性档（字段注释说明为何链共享的 Nearest `post_sampler` 不可用）。
    fn ensure_fxaa_targets(&mut self, width: u32, height: u32) {
        let hdr = self
            .hdr
            .as_mut()
            .expect("draw 开头已 ensure_hdr_targets，HDR 中间缓冲必就绪");
        if hdr.fxaa.is_some() {
            return;
        }
        let device = &self.gpu.device;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.fxaa.ldr"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.color_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("iris.fxaa.sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.fxaa.params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iris.fxaa.bg"),
            layout: &self.bundle.post_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
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
        hdr.fxaa = Some(FxaaTargets {
            texture,
            view,
            params,
            sampler,
            bind_group,
        });
    }

    /// 遮挡查询常驻资源的首次惰性创建（R53）：4096 槽查询集（核心能力
    /// 上限，见 `occlusion.rs` 限制③）+ 单位盒代理几何 + 双帧读回环。
    /// 全部资源与开关同生命周期：功能从未开启过 = 一个字节都不分配。
    fn ensure_occ_gpu(&mut self) {
        if self.occ_gpu.is_some() {
            return;
        }
        let device = &self.gpu.device;
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("iris.occlusion.queries"),
            ty: wgpu::QueryType::Occlusion,
            count: wgpu::QUERY_SET_MAX_QUERIES,
        });
        let corner_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("iris.occlusion.corners"),
            contents: bytemuck::cast_slice(&AABB_CORNERS),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("iris.occlusion.indices"),
            contents: bytemuck::cast_slice(&AABB_INDICES),
            usage: wgpu::BufferUsages::INDEX,
        });
        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.occlusion.params"),
            size: std::mem::size_of::<CascadeParamsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let params_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iris.occlusion.params.bg"),
            layout: &self.bundle.cascade_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: params_buf.as_entire_binding(),
            }],
        });
        let query_bytes = u64::from(wgpu::QUERY_SET_MAX_QUERIES) * u64::from(wgpu::QUERY_SIZE);
        let resolve_bufs = std::array::from_fn(|_| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("iris.occlusion.resolve"),
                size: query_bytes,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        });
        let readback_bufs = std::array::from_fn(|_| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("iris.occlusion.readback"),
                size: query_bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        self.occ_gpu = Some(OccGpu {
            query_set,
            corner_buf,
            index_buf,
            params_buf,
            params_bg,
            resolve_bufs,
            readback_bufs,
            aabb_buf: None,
            aabb_capacity: 0,
        });
        log::debug!(
            "遮挡查询资源已创建（{} 槽双缓冲）",
            wgpu::QUERY_SET_MAX_QUERIES
        );
    }

    /// 遮挡 AABB 实例缓冲容量复用（与实例缓冲同款：不足时 2 倍扩容整体
    /// 重建；本缓冲每帧全量上传——实例序随收集变化，增量无意义）。
    fn ensure_occ_aabb_capacity(&mut self, needed: usize) {
        let capacity_need = {
            let occ = self
                .occ_gpu
                .as_ref()
                .expect("ensure_occ_gpu 已在本帧先调用");
            occ.aabb_capacity < needed
        };
        if capacity_need {
            let capacity = (needed * 2).max(256);
            let buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("iris.occlusion.aabbs"),
                size: (capacity * std::mem::size_of::<AabbInstanceRaw>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let occ = self.occ_gpu.as_mut().expect("同上");
            occ.aabb_buf = Some(buf);
            occ.aabb_capacity = capacity;
            log::debug!("遮挡 AABB 缓冲扩容至 {capacity} 实例");
        }
    }

    /// 消费上一帧的遮挡查询读回（R53，`draw` 开头帧号自增后调用）。
    ///
    /// 非阻塞：`poll(Poll)` 推进一次设备、`try_recv` 取映射回调——
    /// 到达则重建隐藏表（**只采纳上一帧发出的结果**，迟到丢弃）并
    /// `unmap` 释放槽位；未到达则保持在途、本帧不剔除（全部可见，
    /// 保守方向）。返回值 = 本帧可用新鲜剔除裁决。
    fn consume_occlusion_results(&mut self) -> bool {
        let Some(inflight) = self.occ_inflight.as_ref() else {
            return false;
        };
        let _ = self.gpu.device.poll(wgpu::PollType::Poll);
        match inflight.done.try_recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                // 映射失败（设备丢失等）：缓冲未被映射，直接废弃在途记录
                log::warn!("遮挡查询读回映射失败：{e}，本帧不剔除");
                self.occ_inflight = None;
                return false;
            }
            Err(_) => {
                // 回调未到（GPU 尚未退休本批提交）：保持在途、稍后再收
                return false;
            }
        }
        let inflight = self.occ_inflight.take().expect("刚确认回调到达");
        let fresh = inflight.frame == self.occ_frame - 1;
        let occ = self.occ_gpu.as_ref().expect("读回缓冲随查询资源同在");
        {
            let bytes = occ.readback_bufs[inflight.parity]
                .get_mapped_range(0..(inflight.count * std::mem::size_of::<u64>()) as u64)
                .expect("已映射的读回缓冲必须可读");
            self.occ_hidden.clear();
            if fresh {
                for (i, &entity) in inflight.entities.iter().enumerate() {
                    let slot = i * std::mem::size_of::<u64>();
                    let samples =
                        u64::from_le_bytes(bytes[slot..slot + 8].try_into().expect("u64 槽"));
                    // 0 = 全部深度测试失败 ⇒ 对本帧深度零贡献（保守性论证
                    // 见 occlusion.rs）；非 0 保守地保留可见
                    if samples == 0 {
                        self.occ_hidden.insert(entity);
                    }
                }
            } else {
                log::debug!(
                    "遮挡查询结果迟到（帧 {}/{}），本帧跳过剔除",
                    inflight.frame,
                    self.occ_frame
                );
            }
        }
        occ.readback_bufs[inflight.parity].unmap();
        fresh
    }

    pub fn stats(&self) -> FrameStats {
        self.stats
    }

    /// 最近一次成功消费的 meshlet 紧凑列表（packed 条目：实例槽 << 16 |
    /// 簇局部号；紧凑序 = 全局簇号升序 ⇒ 本切片恒严格升序）。GPU 测试与
    /// 验收的观测口（稳定紧凑化的确定性断言）；`None` = 尚无有效回读
    /// （首帧 / 读回迟到 / 映射失败）。
    pub fn meshlet_consumed_list(&self) -> Option<&[u32]> {
        self.meshlet_consumed.as_ref().map(|c| c.list.as_slice())
    }

    /// 渲染一帧：场景渲染到内部 HDR 中间缓冲（MSAA 4x），贴花 pass、
    /// 透明 pass 与粒子 pass 依次叠加——贴花位于蒙皮之后、透明之前（不
    /// 透明表面的装饰，玻璃等透明物盖在其混合结果上，见 `decal.rs`），
    /// 粒子位于透明之后、泛光之前（粒子叠加在玻璃等透明结果之上，且
    /// 高亮 additive 粒子参与泛光），随后 ACES 色调映射
    /// blit 到 `color_view`（通常为 sRGB 目标）；最后如有 UI 组件
    /// （`UiRect` / `UiText` / `UiColor`），在同一 `color_view` 上
    /// 以 `LoadOp::Load` 叠加 HUD pass——**位于 blit 之后**，因此 HUD
    /// 不参与 HDR / 泛光 / 色调映射，是干净的 sRGB 界面叠加。
    /// 粒子收集发生在 `update_particles` 之后才有意义（调用契约见
    /// `particles.rs`：与 `propagate_transforms` 同层的每帧前置系统）。
    pub fn draw(
        &mut self,
        world: &iris_core::world::World,
        color_view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) {
        // 环境源失效检查在最前：重烘焙与绑定组重建必须发生在本帧编码之前
        // MipSettings（R66）：先于环境/探针路径读取——探针捕获内的材质
        // 懒上传与本帧主 pass 同口径。上传时生效：已上传纹理不追溯。
        self.mip_anisotropy = world
            .try_resource::<crate::texture::MipSettings>()
            .map(|m| m.anisotropy);
        self.ensure_environment(world);
        self.ensure_hdr_targets(width, height);
        // 遮挡查询（R53）：帧号自增后先消费上一帧读回（非阻塞 poll +
        // try_recv，迟到即弃），裁决在本帧收集之后、主 pass 编码之前应用
        self.occ_frame += 1;
        // meshlet 帧号同步推进（奇偶槽位轮转的基准——被跳过的帧不产生
        // 「上一帧」，与 occ_frame 同款纪律）
        self.meshlet_frame += 1;
        let occ_fresh = self.consume_occlusion_results();
        // meshlet 注册 + 读回消费（R85）：arena 追加（变更时上传）与上一帧
        // 紧凑结果的非阻塞消费（遮挡同款协议）。资源缺席 / 无实体 = 两者
        // 皆零开销（缺席语义：零 dispatch、零回读、零绘制）。
        // R87：流送激活裁决在此先行——预算在场 ⇒ 注册跳过整库数据上传
        //（页数据由驻留 tick 逐页写入）。
        let meshlet_stream_active = world
            .try_resource::<crate::meshlet_stream::MeshletStreamSettings>()
            .is_some_and(|s| s.gpu_byte_budget.is_some());
        let meshlet_store = world.try_resource::<crate::meshlet::MeshletMeshStore>();
        if let Some(store) = meshlet_store.as_deref() {
            self.ensure_meshlet_world(store, meshlet_stream_active);
        }
        self.consume_meshlet_readback();
        // 暗角 / 景深 / 运动模糊的按需资源（全分辨率中间对 + blit 接入
        // 绑定组 + 主相机深度副本）必须在贯穿全部 pass 的 hdr 不可变借用
        // 建立之前惰性分配（关闭 = 零开销跳过——与可见性、粒子同款
        // 「缺席零开销」哲学）
        let vignette_strength = world
            .try_resource::<crate::post::VignetteSettings>()
            .map(|v| v.strength.max(0.0))
            .unwrap_or(0.0);
        // 资源缺席语义：DOF / MB 缺席 = 关闭（与暗角同款零开销默认路径）
        let dof = world
            .try_resource::<crate::post::DepthOfFieldSettings>()
            .map(|d| *d)
            .unwrap_or_default();
        let mb = world
            .try_resource::<crate::post::MotionBlurSettings>()
            .map(|m| *m)
            .unwrap_or_default();
        let dof_on = dof.enabled;
        let mb_on = mb.enabled;
        // 体积雾（R79）：开启语义在此先读（按需资源分配的前置裁决），
        // 参数在链编码段复用。缺席/关闭 = 不分配、不编码 pass、不写参数
        // （零开销默认路径，链拓扑走 full_chain_slots 旧函数）。
        let fog_settings = world.try_resource::<crate::fog::FogSettings>().map(|f| *f);
        let fog_on = fog_settings.is_some_and(|f| f.enabled);
        if vignette_strength > 0.0 || dof_on || mb_on || fog_on {
            self.ensure_full_targets(width, height);
        }
        if dof_on || mb_on {
            self.ensure_scene_depth(width, height);
        }
        // FXAA（R53）：开启 = blit 改写中间 LDR、FXAA pass 再写最终目标；
        // 缺席/关闭 = blit 直写，命令流与引入本功能前逐位一致。资源分配
        // 同样必须在 hdr 不可变借用建立之前（随 HdrTargets 尺寸失效）。
        let fxaa = world
            .try_resource::<crate::post::FxaaSettings>()
            .map(|f| *f)
            .unwrap_or_default();
        let fxaa_on = fxaa.enabled;
        if fxaa_on {
            self.ensure_fxaa_targets(width, height);
        }
        // TAA（R60）：开启 = 主相机投影亚像素抖动 + 链首时域累加 pass；
        // 缺席/关闭 = 不抖动、不入链，命令流与引入前逐位一致。资源分配
        // 必须在 hdr 不可变借用建立之前（深度副本供重投影消费，与
        // DOF/MB 共用同一份）。
        let taa_on = world
            .try_resource::<crate::post::TaaSettings>()
            .is_some_and(|t| t.enabled);
        if taa_on {
            self.ensure_scene_depth(width, height);
            self.ensure_taa_targets(width, height);
        }
        // SSR（R62b）：开启 = 上帧颜色副本（惰性、随 HDR 尺寸）+ 复用
        // 主相机深度副本；缺席/关闭 = 不分配、不写参数（零开销默认路径，
        // 与 R62a 逐位一致）。分配必须在 hdr 不可变借用建立之前（与
        // TAA/DOF/MB 同款纪律），并且**先于主 pass** 消费其视图。
        let ssr_settings = world.try_resource::<crate::post::SsrSettings>().map(|s| *s);
        let ssr_on = ssr_settings.is_some_and(|s| s.enabled);
        let mut ssr_needs_globals_rebuild = false;
        if ssr_on {
            ssr_needs_globals_rebuild |= self.ensure_scene_depth(width, height);
            ssr_needs_globals_rebuild |= self.ensure_ssr_prev_color(width, height);
            // 重置裁决：关→开切换（上帧资源是关闭语义下的遗留）或任一
            // 资源本次（重）建（新纹理是空的 / 尺寸变化）⇒ 上帧无效。
            if !self.ssr_was_on || ssr_needs_globals_rebuild {
                self.ssr_prev_valid = false;
            }
            self.ssr_was_on = true;
        } else {
            self.ssr_was_on = false;
        }
        // 本帧执行开关（重置语义：无效帧整段跳过 = 与 SSR 关闭帧逐位一致）；
        // 切镜（reset_taa 跟随置位）已在上游把 ssr_prev_valid 清掉。
        let ssr_active = ssr_on && self.ssr_prev_valid;
        // AO（R77）：开启 = 半分辨率可见度图（惰性、随窗口）+ 复用主相机
        // 深度副本；缺席/关闭 = 不分配、binding 14 回落哑纹理（零开销默认
        // 路径，全部既有 FNV 基线原值）。分配必须在 hdr 不可变借用建立
        // 之前（与 TAA/DOF/MB/SSR 同款纪律）。
        let ao_settings = world.try_resource::<crate::post::AoSettings>().map(|a| *a);
        let ao_on = ao_settings.is_some_and(|a| a.enabled);
        let ao_rebuilt = if ao_on {
            // 可见度图 + 主相机深度副本（GTAO 的输入——AO 单独开启时副本
            // 也必须跑）。副本（重）建的帧 ao_depth_frame 编号对不上，本
            // 帧自然走中性清屏路径，无需额外记账。
            self.ensure_scene_depth(width, height);
            self.ensure_ao_targets(width, height)
        } else {
            false
        };
        // 体积雾深度副本（R79）：与 DOF / MB 同款链上消费，同一编码器内
        // 副本在雾 pass 之前编码，读到本帧深度。分配在 hdr 不可变借用建
        // 立之前（与 TAA/DOF/MB/SSR/AO 同款纪律）。
        if fog_on {
            self.ensure_scene_depth(width, height);
        }
        // 平面水体（R81）：确定性选择键（实体位模式升序，见
        // `water::select_water_surface`——结果与查询 / 插入顺序无关）取
        // 首盏水面，其余 `warn` 忽略。在场时需要两份按需资源：本帧
        // scene_depth（深度吸收的深度差输入——水体 pass 编码在
        // scene_depth pass 之后，链上同帧消费）与捕获图 / 绑定组（惰性
        // 创建、随窗口尺寸重建）。分配在 hdr 不可变借用建立之前（与
        // TAA/DOF/MB/SSR/AO/雾同款纪律）。**缺席语义**：无 WaterSurface
        // 实体 = 下方整段逻辑零执行——无捕获 pass、无 write_buffer、无
        // 水体 draw，像素与命令流与引入前逐位一致。
        let mut water_candidates: Vec<(u64, crate::water::WaterSurface)> = Vec::new();
        world.query::<(Entity, &crate::water::WaterSurface)>(|(e, ws)| {
            water_candidates.push((e.to_bits(), *ws));
        });
        let water_selected: Option<crate::water::WaterSurface> = {
            let bits: Vec<u64> = water_candidates.iter().map(|c| c.0).collect();
            crate::water::select_water_surface(&bits).map(|i| water_candidates[i].1)
        };
        if water_candidates.len() > 1 {
            log::warn!(
                "检测到 {} 个 WaterSurface 实体，v1 仅支持单水面，\
                 已按（实体位模式升序）选择键取首个，其余忽略",
                water_candidates.len()
            );
        }
        let water_on = water_selected.is_some();
        if water_on {
            self.ensure_scene_depth(width, height);
            self.ensure_water_targets(width, height);
        }
        // 天空大气（R79）：只读开关（参数缓冲恒存在，缺席/关闭帧不写）。
        let sky_settings = world.try_resource::<crate::post::SkySettings>().map(|s| *s);
        // binding 14 的指向裁决变化（开 ↔ 关）或可见度图（重）建 ⇒ 全局
        // 绑定组需要重新编码（与 SSR 上帧资源就位同款重建路径）。
        let ao_bind_changed = self.ao.bind_real != ao_on;
        self.ao.bind_real = ao_on;
        if ssr_needs_globals_rebuild || ao_rebuilt || ao_bind_changed {
            // 上帧颜色副本 / 场景深度纹理 / AO 可见度图的视图变化 ⇒ 全局
            // 绑定组的 binding 11/12/14 需要重新指向（与 env 重烘同款
            // 重建路径）
            self.rebuild_globals_bind_group();
        }
        // 遮挡剔除（R53）：开启语义、保守性论证与「读回迟到一帧 ⇒ 本帧
        // 不剔除」的回退见 occlusion.rs 模块文档。
        let occlusion_on = world
            .try_resource::<OcclusionCulling>()
            .is_some_and(|o| o.enabled)
            && self.gpu.supports_occlusion_query();
        self.stats = FrameStats::default();
        // 相机解析：优先挂载在实体上的 ActiveCamera（跟随层级运动），
        // 其次回退到全局 Camera 资源。`RendersTo`（R40）标记的相机不复用
        // 主视图选择——主视图 = 无 `RendersTo` 的相机；只有 RT 相机时
        // 回退 Camera 资源（原语义，见 rendertarget.rs）。
        let mut mounted: Option<(glam::Mat4, ActiveCamera, Vec3)> = None;
        world.query::<(&GlobalTransform, &ActiveCamera, Option<&RendersTo>)>(|(gt, ac, rt)| {
            if mounted.is_none() && rt.is_none() {
                mounted = Some((view_from_global(gt), *ac, gt.position()));
            }
        });
        let (view, fov, z_near, z_far, eye) = match mounted {
            Some((view, ac, eye)) => (view, ac.fov_y_radians, ac.z_near, ac.z_far, eye),
            None => {
                let Some(camera) = world.try_resource::<Camera>().map(|c| (*c).clone()) else {
                    log::debug!("世界中没有 ActiveCamera 实体也没有 Camera 资源，跳过本帧渲染");
                    return;
                };
                (
                    camera.view_matrix(),
                    camera.fov_y_radians,
                    camera.z_near,
                    camera.z_far,
                    camera.eye,
                )
            }
        };
        let aspect = width as f32 / height.max(1) as f32;
        // 投影与未抖动相机矩阵（TAA 关闭帧的最终矩阵；TAA 开启帧在下方
        // 重置裁决后以抖动投影重算——抖动只作用于**纯投影**再乘 view，
        // 对合成矩阵做行运算不是纯 NDC 平移，见 jitter_projection 文档）
        let proj = glam::camera::rh::proj::directx::perspective(fov, aspect, z_near, z_far);
        let vp_raw = proj * view;
        let light = world
            .try_resource::<DirectionalLight>()
            .map(|l| (*l).clone())
            .unwrap_or_default();
        // 阴影 v2（CSM，R29）：沿相机视锥深度分割出 SHADOW_CASCADES 个
        // 级联，各自拟合光空间正交矩阵（PFS 分割 + 纹素对齐，纯函数见
        // light.rs）。v1 的「以注视点为中心、shadow_range 为半径」单张
        // 覆盖废止：级联范围由相机视锥深度片直接决定，覆盖距离由
        // SHADOW_DISTANCE 常量钳制（之外的片元保持受光，无影）。
        let cam_forward = (-view.z_axis.truncate()).normalize_or_zero();
        let splits = cascade_splits(z_near, z_far);
        let csm_near = z_near.min(1.0);
        // R61：拟合产物多带两个世界尺度（纹素世界尺寸 / 深度窗口），
        // 阴影偏移 v2 的坡度项与法线偏移逐级联按它们缩放；矩阵本身与
        // R29 逐位相同（cascade_fit_params 内部算式未动）。
        let cascade_fits: [crate::light::CascadeFit; SHADOW_CASCADES] = std::array::from_fn(|i| {
            let z_start = if i == 0 { csm_near } else { splits[i - 1] };
            cascade_fit_params(light.direction, &view, fov, aspect, z_start, splits[i])
        });
        let light_vps: [Mat4; SHADOW_CASCADES] = cascade_fits.map(|f| f.view_proj);
        // 资源目录按引用借用（不复制数据），生命周期覆盖整个 draw 过程
        let Some(mesh_store) = world.try_resource::<MeshStore>() else {
            return;
        };
        let Some(material_store) = world.try_resource::<MaterialStore>() else {
            return;
        };
        let texture_store = world.try_resource::<TextureStore>();
        // 光照贴图目录（R52c）：资源缺席 = 场景没有烘焙产物，材质绑定组
        // 恒绑常驻黑图集 + 掩码 0（与引入本槽位前逐位一致）。
        let lightmap_store = world.try_resource::<LightmapStore>();
        // 运动模糊的重投影基准（R43）：上一帧主相机视图投影；首帧用当前
        // 帧矩阵补位（速度恒 0 → 原色透传）。保存时机在全部早退路径之后
        // （被跳过的帧不产生「上一帧」）。TAA 开帧保存抖动后矩阵（R60：
        // 下帧重投影需要上一帧的实际渲染矩阵）。
        let prev_vp = self.prev_view_proj.unwrap_or(vp_raw);
        // TAA 帧序号推进与重置裁决（同一纪律：只在实际渲染的帧推进）。
        // taa_index = 本帧的 Halton 槽位（自增前取值）；关→开切换与
        // reset_taa() / 历史缓冲重建都置重置。**重置帧抖动置零**：该帧
        // 输出 = 未抖动当前帧，与无 TAA 帧逐位一致（重置语义 = 透传）。
        let taa_index = self.taa_frame;
        let mut taa_reset_frame = false;
        if taa_on {
            self.taa_frame += 1;
            taa_reset_frame = self.taa_reset_pending || !self.taa_was_on;
            self.taa_reset_pending = false;
            self.taa_was_on = true;
        } else {
            self.taa_was_on = false;
        }
        // 本帧相机矩阵定稿：Halton(2,3) 8 点序列按 TAA 帧号取模，偏移
        // =（Halton − 0.5）纹素换算 NDC（±0.5 纹素）。关闭 / 重置帧 =
        // 零偏移 = 矩阵逐位原样（jitter_projection 对零偏移原样返回）。
        // 定稿后贯穿主 pass / 视锥剔除 / scene_depth / 遮挡查询代理 /
        // MB / TAA（一致抖动，ARCHITECTURE §4.29 裁决 2）。
        let taa_jitter_ndc = if taa_on && !taa_reset_frame {
            let j = crate::post::taa_jitter(self.taa_frame - 1);
            glam::Vec2::new(
                j[0] * 2.0 / width.max(1) as f32,
                j[1] * 2.0 / height.max(1) as f32,
            )
        } else {
            glam::Vec2::ZERO
        };
        let vp = crate::post::jitter_projection(proj, taa_jitter_ndc) * view;
        self.prev_view_proj = Some(vp);
        // 平面水体（R81）的镜像相机矩阵：关于 y = height 平面的反射 M
        //（列主序：y 轴取反 + 平移 (0, 2h, 0)），镜像视图 = view × M（对
        // 世界点先做镜像再进主相机视图，⇒ 镜像 VP = proj × view × M）。
        // **M 行列式为负 ⇒ 三角形环绕方向反转**——捕获管线用 cull None
        // 吸收（R62a 教训的「cull None 吸收」分支，与阴影 pass 同款）。
        // 抖动与主相机同款作用于纯投影（与 vp 的组装一致）。
        let (water_refl_view, water_refl_vp, water_refl_eye) = match water_selected {
            Some(ws) => {
                let mirror = Mat4::from_cols_array_2d(&[
                    [1.0, 0.0, 0.0, 0.0],
                    [0.0, -1.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0, 0.0],
                    [0.0, 2.0 * ws.height, 0.0, 1.0],
                ]);
                let refl_view = view * mirror;
                let refl_vp = crate::post::jitter_projection(proj, taa_jitter_ndc) * refl_view;
                let refl_eye = mirror.transform_point3(eye);
                (refl_view, refl_vp, refl_eye)
            }
            None => (Mat4::IDENTITY, Mat4::IDENTITY, Vec3::ZERO),
        };

        // RT（R40）相机收集：全部 (ActiveCamera + RendersTo) 实体，数量上限
        // RENDER_TARGET_MAX（小地图 / 监控屏两个典型用例，超出 warn 跳过）。
        // 缺席 RenderTargetStore 资源 = 无 RT 相机（主路径零变化）。
        let rt_cameras: Vec<RtCameraPrep> = match world.try_resource::<RenderTargetStore>() {
            Some(store) => {
                let mut list = Vec::new();
                let mut total = 0usize;
                world.query::<(&GlobalTransform, &ActiveCamera, &RendersTo)>(|(gt, ac, rt)| {
                    total += 1;
                    if total > RENDER_TARGET_MAX {
                        return;
                    }
                    let Some(spec) = store.get(rt.target) else {
                        log::warn!(
                            "渲染目标 {:?} 未在 RenderTargetStore 中注册，跳过该 RT 相机",
                            rt.target
                        );
                        return;
                    };
                    let view = view_from_global(gt);
                    let aspect = spec.width as f32 / spec.height.max(1) as f32;
                    let vp = glam::camera::rh::proj::directx::perspective(
                        ac.fov_y_radians,
                        aspect,
                        ac.z_near,
                        ac.z_far,
                    ) * view;
                    list.push(RtCameraPrep {
                        handle: rt.target,
                        view,
                        eye: gt.position(),
                        vp,
                        width: spec.width,
                        height: spec.height,
                    });
                });
                if total > RENDER_TARGET_MAX {
                    log::warn!(
                        "检测到 {total} 个 RT 相机，v1 仅渲染前 {RENDER_TARGET_MAX} 个（超出已跳过）"
                    );
                }
                list
            }
            None => Vec::new(),
        };

        // 1. 全局 uniform：相机 + 方向光 + 点光
        let light_dir = (-light.direction).normalize_or_zero();
        let mut point_lights = [PointLightRaw {
            position: [0.0; 4],
            color: [0.0; 4],
            shadow: [0.0; 4],
        }; MAX_POINT_LIGHTS];
        let mut point_count = 0usize;
        // 投影灯（R61 预算化多盏）：收集期只记候选（灯实体位模式 + 到相机
        // eye 的距离 + uniform 槽位），收集完成后由确定性选择键
        // `select_point_shadow_casters` 统一裁决并分配 cubemap 槽位——
        // **选择键不依赖查询存储序**（查询序只决定 uniform 数组的填充序，
        // 不参与选择；乱序插入的两次世界选中同一批灯）。
        let mut caster_candidates: Vec<ShadowCandidateSlot> = Vec::new();
        let mut caster_total = 0usize;
        // R78 分箱：全部点光（查询序）同时进灯表（容量 [`MAX_BINNED_LIGHTS`]），
        // 前 8 盏照旧进 `Globals.point_lights`（语义不变）；`point_total`
        // 记总数供自动阈值判断（超容量的部分只计数，与超限日志口径一致）。
        let mut bin_points: Vec<([f32; 4], [f32; 4])> = Vec::new();
        let mut point_total = 0usize;
        world.query::<(Entity, &GlobalTransform, &PointLight)>(|(e, gt, pl)| {
            point_total += 1;
            let position = gt.position();
            if bin_points.len() < MAX_BINNED_LIGHTS {
                bin_points.push((
                    [position.x, position.y, position.z, 0.0],
                    [
                        pl.color.x * pl.intensity,
                        pl.color.y * pl.intensity,
                        pl.color.z * pl.intensity,
                        pl.range,
                    ],
                ));
            }
            if point_count >= MAX_POINT_LIGHTS {
                return;
            }
            if pl.shadows {
                caster_total += 1;
                caster_candidates.push(ShadowCandidateSlot {
                    key: PointShadowCandidate {
                        entity_bits: e.to_bits(),
                        eye_distance: (position - eye).length(),
                    },
                    light_slot: point_count,
                });
            }
            point_lights[point_count] = PointLightRaw {
                position: [position.x, position.y, position.z, 0.0],
                color: [
                    pl.color.x * pl.intensity,
                    pl.color.y * pl.intensity,
                    pl.color.z * pl.intensity,
                    pl.range,
                ],
                shadow: [0.0, 0.0, 0.0, 0.0],
            };
            point_count += 1;
        });
        // 选择键：（相机距离升序, 实体位模式升序）——结果只取决于灯集合。
        // 选中者按选择键序占据槽位 0..n（槽位 = cubemap 数组下标）。
        let caster_keys: Vec<PointShadowCandidate> =
            caster_candidates.iter().map(|c| c.key).collect();
        let caster_count = caster_keys.len().min(POINT_SHADOW_BUDGET);
        let caster_slots = select_point_shadow_casters(&caster_keys, POINT_SHADOW_BUDGET);
        if caster_total > caster_count {
            log::warn!(
                "有 {caster_total} 盏点光开启阴影，超出预算 {POINT_SHADOW_BUDGET} 盏，\
                 已按（相机距离升序, 实体位模式升序）选择键取前 {caster_count} 盏"
            );
        }
        // 逐灯把「启用 + 槽位」写进 uniform（着色器逐灯读取，无位置交换——
        // 多盏各投各影，v1 的「换到槽位 0」限制随之废止）
        for (slot, &candidate) in caster_slots.iter().enumerate() {
            let light_slot = caster_candidates[candidate].light_slot;
            point_lights[light_slot].shadow = [1.0, slot as f32, 0.0, 0.0];
        }
        // R78：组装灯表前段 = 全部点光（查询序）；前 8 盏的阴影参数与
        // `Globals.point_lights` 逐字段一致（双路径位等价的数据根据），
        // 第 9 盏起不投影（本就进不了阴影预算）。
        let mut binned_lights: Vec<BinnedLightRaw> = bin_points
            .iter()
            .enumerate()
            .map(|(i, &(position, color_range))| BinnedLightRaw {
                pos_type: position,
                color_range,
                shadow: if i < point_count {
                    point_lights[i].shadow
                } else {
                    [0.0; 4]
                },
                cone: [0.0; 4],
                outer: [0.0; 4],
            })
            .collect();
        let point_shadow_params = [
            if caster_count > 0 { 1.0 } else { 0.0 },
            if self.legacy_shadow_bias {
                POINT_SHADOW_BIAS_LEGACY
            } else {
                POINT_SHADOW_BIAS_MIN
            },
            if self.legacy_shadow_bias {
                0.0
            } else {
                POINT_SHADOW_SLOPE_SCALE
            },
            if self.legacy_shadow_bias {
                0.0
            } else {
                POINT_SHADOW_NORMAL_SCALE
            },
        ];
        self.stats.point_shadow_casters = caster_count as u32;
        if point_total > MAX_POINT_LIGHTS {
            log::warn!("点光数量 {point_total} 超过上限 {MAX_POINT_LIGHTS}，多余的已被忽略");
        }
        let shadow_enabled = light.shadows;
        // ---- R78：SpotLight 收集（查询序追加在点光之后）----
        //
        // 锥光不进 Globals（8 盏均摊装不下也不该挤占点光），只进灯表；
        // v1 不投影阴影（shadow 恒 0）、`inner > outer` 钳到硬边、零长度
        // 方向 = 不发光（颜色置零——锥光因子对零向量无从定义）。
        let mut spot_total = 0usize;
        world.query::<(&GlobalTransform, &SpotLight)>(|(gt, sl)| {
            spot_total += 1;
            if binned_lights.len() >= MAX_BINNED_LIGHTS {
                return;
            }
            let position = gt.position();
            let direction = sl.direction.normalize_or_zero();
            let zero_color = [0.0f32; 3];
            let rgb = if sl.direction.length_squared() == 0.0 {
                zero_color
            } else {
                [
                    sl.color.x * sl.intensity,
                    sl.color.y * sl.intensity,
                    sl.color.z * sl.intensity,
                ]
            };
            // inner > outer 钳到硬边：cos(inner) - cos(outer) ≤ 0 落入
            // spot_cone_factor 的硬边分支（denom ≤ 1e-6）
            let inner = sl.inner_cutoff.min(sl.outer_cutoff);
            binned_lights.push(BinnedLightRaw {
                pos_type: [position.x, position.y, position.z, 1.0],
                color_range: [rgb[0], rgb[1], rgb[2], sl.range],
                shadow: [0.0; 4],
                cone: [direction.x, direction.y, direction.z, inner.cos()],
                outer: [sl.outer_cutoff.cos(), 0.0, 0.0, 0.0],
            });
        });
        if point_total + spot_total > MAX_BINNED_LIGHTS {
            log::warn!(
                "灯表数量 {} 超过上限 {MAX_BINNED_LIGHTS}，多余的已被忽略",
                point_total + spot_total
            );
        }
        // 分箱裁决：`LightBinningSettings` 缺席 = 自动阈值
        // （[`light_bins_enabled`]）；`force` 为强制入口（双路径位等价
        // 验收用）。绑定指向变化时重建全局绑定组（AO bind_real 同款）。
        let bins_force = world
            .try_resource::<LightBinningSettings>()
            .map(|s| s.force)
            .unwrap_or(false);
        let bins_on = bins_force || light_bins_enabled(point_total, spot_total);
        let bins_bind_changed = self.light_bins.bind_real != bins_on;
        self.light_bins.bind_real = bins_on;
        // GI **三态**裁决（R83 / R88）：DDGI 网格非空 ⇒ mode = 2（逐片元 SH
        // × chebyshev 深度项），并**整表替换** binding 18/19 + 忽略 R83 的
        // 烘焙探针集（裁定 2：两份间接漫反射叠加会双重计入天空与色溢）；
        // DDGI 缺席 ⇒ R83 现规则原样（探针集**带 SH** ⇒ mode = 1，资源缺席 /
        // 空集 / 无 SH ⇒ mode = 0，loc14 逐实体路径逐位不变）。
        // 没有独立开关资源：SH 是烘焙期的显式选择、DDGI 是 `DdgiSettings`
        // 的在场与否，「装了就用」与 R64 同一心智模型。绑定指向变化时重建
        // 全局绑定组（bins / AO 的 bind_real 同款）。
        let ddgi_grid = world.try_resource::<crate::ddgi::DdgiGrid>();
        let ddgi_on = ddgi_grid.as_deref().is_some_and(|grid| !grid.is_empty());
        let gi_set = world.try_resource::<IrradianceProbeSet>();
        let baked_on = gi_set.as_deref().is_some_and(|set| set.has_sh());
        if ddgi_on && baked_on && !self.ddgi_mutex_warned {
            log::warn!(
                "世界同时存在 DDGI 网格（{} 探针）与 R83 烘焙探针集（带 SH）——\
                 两者互斥，本轮以 DDGI 为准（整表替换 binding 18/19），烘焙探针集被忽略。\
                 理由：两份间接漫反射叠加会双重计入天空与色溢",
                ddgi_grid.as_deref().map_or(0, |g| g.probe_count())
            );
            self.ddgi_mutex_warned = true;
        }
        let gi_on = ddgi_on || baked_on;
        let gi_bind_changed = self.gi_sh.bind_real != gi_on;
        self.gi_sh.bind_real = gi_on;
        // 768 KiB 真表的惰性创建必须在 `ddgi.bind_real` 翻转**之前**：
        // `rebuild_globals_bind_group` 会按它裁决 binding 20 的指向
        if ddgi_on {
            self.ensure_ddgi_gpu();
        }
        let ddgi_bind_changed = self.ddgi.bind_real != ddgi_on;
        self.ddgi.bind_real = ddgi_on;
        if bins_bind_changed || gi_bind_changed || ddgi_bind_changed {
            self.rebuild_globals_bind_group();
        }
        // DDGI 观测（不进任何排序键）：本帧重照明的探针数
        self.stats.ddgi_updated = ddgi_grid
            .as_deref()
            .map(|grid| grid.stats().last_updated)
            .unwrap_or(0);
        // 反射探针（R62a）：收集「挂了组件**且已烘焙**（登记表命中）」的
        // 探针，按（相机到影响域距离升序, 实体位模式升序）取预算内一批，
        // 再按实体位模式升序填 uniform（= 着色器的 tie-break 序）。
        // 未烘焙的探针不进 uniform ⇒ 着色路径与 R61 逐位一致（兼容硬约束）。
        let mut probe_candidates: Vec<ProbeCandidateSlot> = Vec::new();
        world.query::<(Entity, &GlobalTransform, &ReflectionProbe)>(|(e, gt, probe)| {
            if !probe.is_valid() {
                return; // 退化影响域（半边长非正 / 非有限）：不参与，见组件文档
            }
            let Some(slot) = self.probe_store.slot_of(e.to_bits()) else {
                return; // 组件在场但未烘焙：静默走全局 env（缺席语义）
            };
            let center = gt.position();
            probe_candidates.push(ProbeCandidateSlot {
                key: ProbeCandidate {
                    entity_bits: e.to_bits(),
                    camera_distance: distance_to_box(eye, center, probe.half_extents),
                },
                center,
                extents: probe.half_extents,
                falloff: probe.falloff.max(0.0),
                slot,
            });
        });
        let probe_keys: Vec<ProbeCandidate> = probe_candidates.iter().map(|c| c.key).collect();
        let probe_budget = probe_keys.len().min(MAX_REFLECTION_PROBES);
        let probe_selected = select_active_probes(&probe_keys, MAX_REFLECTION_PROBES);
        if probe_keys.len() > probe_budget {
            log::warn!(
                "有 {} 个已烘焙反射探针，超出预算 {MAX_REFLECTION_PROBES} 个，\
                 已按（相机到影响域距离升序, 实体位模式升序）选择键取前 {probe_budget} 个",
                probe_keys.len()
            );
        }
        let mut probe_raws = [ReflectionProbeRaw::default(); MAX_REFLECTION_PROBES];
        for (i, &candidate) in probe_selected.iter().enumerate() {
            let c = &probe_candidates[candidate];
            probe_raws[i] = ReflectionProbeRaw {
                center_falloff: [c.center.x, c.center.y, c.center.z, c.falloff],
                extents_slot: [c.extents.x, c.extents.y, c.extents.z, c.slot as f32],
            };
        }
        self.stats.reflection_probes = probe_selected.len() as u32;
        // 只在「本帧有生效探针」或「上一帧有（需回写零值让探针退场）」时写
        // uniform：无探针场景一次 write_buffer 都不发，命令流与 R61 一致。
        if !probe_selected.is_empty() || self.probe_params_active {
            let raw = ProbeParamsRaw {
                // w = 视差修正开关（对照模式置 0，见 set_legacy_probe_parallax）
                count: [
                    probe_selected.len() as f32,
                    0.0,
                    0.0,
                    if self.legacy_probe_no_parallax {
                        0.0
                    } else {
                        1.0
                    },
                ],
                probes: probe_raws,
            };
            self.gpu
                .queue
                .write_buffer(&self.probe_params_buf, 0, bytemuck::bytes_of(&raw));
            self.probe_params_active = !probe_selected.is_empty();
        }
        let env_intensity = world
            .try_resource::<crate::environment::EnvironmentSettings>()
            .map(|e| e.intensity.max(0.0))
            .unwrap_or(1.0);
        let globals = GlobalsRaw {
            view_proj: vp.to_cols_array_2d(),
            eye_pos: [eye.x, eye.y, eye.z, 1.0],
            light_dir: [light_dir.x, light_dir.y, light_dir.z, 0.0],
            light_color: [
                light.color.x * light.intensity,
                light.color.y * light.intensity,
                light.color.z * light.intensity,
                0.0,
            ],
            ambient: [light.ambient.x, light.ambient.y, light.ambient.z, 0.0],
            point_count: [point_count as f32, 0.0, 0.0, 0.0],
            point_lights,
            shadow_params: [
                1.0 / crate::pipeline::SHADOW_MAP_SIZE as f32,
                if self.legacy_shadow_bias {
                    SHADOW_BIAS_LEGACY
                } else {
                    SHADOW_BIAS_MIN
                },
                SHADOW_STRENGTH,
                if shadow_enabled { 1.0 } else { 0.0 },
            ],
            env_params: [
                crate::environment::ENV_MIP_COUNT as f32 - 1.0,
                env_intensity,
                0.0,
                0.0,
            ],
            inv_view_proj: vp.inverse().to_cols_array_2d(),
            point_shadow_params,
        };
        self.gpu
            .queue
            .write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&globals));
        // 大气散射天空参数（R79，仅 mode=1 帧写）：缺席 / 关闭帧一个
        // write_buffer 都不发——缓冲保持零初始化（mode = 0 死码分支），
        // 命令流与 R78 逐位一致。太阳方向 = −light_dir（Globals.light_dir
        // 是从光源指向场景的传播方向，太阳视线方向取其反），太阳色 =
        // light.color × intensity（与 Globals.light_color 同口径）。
        if let Some(sky) = sky_settings {
            if sky.atmosphere {
                let sun_dir = (-light.direction).normalize_or_zero();
                self.gpu.queue.write_buffer(
                    &self.sky_params_buf,
                    0,
                    bytemuck::bytes_of(&SkyParamsRaw {
                        mode_params: [
                            1.0,
                            sky.rayleigh_strength.max(0.0),
                            sky.mie_strength.max(0.0),
                            sky.sun_disk_intensity.max(0.0),
                        ],
                        sun_dir: [sun_dir.x, sun_dir.y, sun_dir.z, SKY_DISK_COS_INNER],
                        sun_color: [
                            light.color.x * light.intensity,
                            light.color.y * light.intensity,
                            light.color.z * light.intensity,
                            SKY_DISK_COS_OUTER,
                        ],
                        ground: [
                            sky.ground_color.x.max(0.0),
                            sky.ground_color.y.max(0.0),
                            sky.ground_color.z.max(0.0),
                            SKY_INTENSITY,
                        ],
                    }),
                );
            }
        }
        // 分箱真缓冲上传（R78，仅 mode=1）：mode=0 帧一个 write_buffer
        // 都不发（零开销默认路径，命令流与 R77 一致）。分箱矩阵用主相机
        // 最终 view_proj（含 TAA 抖动——与片元实际所见同一投影）。
        if bins_on {
            self.upload_light_bins(&binned_lights, vp, width, height);
        }
        // GI 表上传（R83 mode = 1 **全量** / R88 mode = 2 **增量**）：
        // mode = 0 帧一个 write_buffer 都不发（零开销默认路径，命令流与
        // R82 一致）。DDGI 优先（裁定 2）⇒ 烘焙探针集在场也不写。
        if ddgi_on {
            if let Some(grid) = ddgi_grid.as_deref() {
                self.upload_ddgi(grid);
            }
        } else if baked_on {
            if let Some(set) = gi_set.as_deref() {
                self.upload_gi_probes(set);
            }
        }
        // 级联参数上传（恒写，含阴影关闭帧）：片元着色器的采样路径无条件
        // 执行（uniform 控制流要求），关闭语义由 shadow_params.w 掩码兜底
        // 混向受光，但 uniform 本身必须有定义。主 pass 读 CsmParamsRaw
        // （binding 8），阴影 pass 逐级联读各自的 64B 矩阵（一级联一缓冲，
        // 规避同 submit 多次 write_buffer 合并问题）。
        //
        // R61：逐级联附上 `texel_world` 与 `texel_world / depth_window`
        // （坡度项斜率），**对照模式下两者置零** ⇒ 采样公式退化为「常数
        // bias + 零偏移」，与 R60 逐位等价（见 set_legacy_shadow_bias）。
        let cascade_texel: [f32; SHADOW_CASCADES] = if self.legacy_shadow_bias {
            [0.0; SHADOW_CASCADES]
        } else {
            cascade_fits.map(|f| f.texel_world)
        };
        let cascade_slope_k: [f32; SHADOW_CASCADES] = if self.legacy_shadow_bias {
            [0.0; SHADOW_CASCADES]
        } else {
            cascade_fits.map(|f| f.texel_world / f.depth_window.max(1e-6))
        };
        let csm_raw = CsmParamsRaw {
            view_proj: light_vps.map(|m| m.to_cols_array_2d()),
            cam_forward: [cam_forward.x, cam_forward.y, cam_forward.z, 0.0],
            splits: [splits[0], splits[1], splits[2], 0.0],
            cascade_texel: [cascade_texel[0], cascade_texel[1], cascade_texel[2], 0.0],
            cascade_slope_k: [
                cascade_slope_k[0],
                cascade_slope_k[1],
                cascade_slope_k[2],
                0.0,
            ],
        };
        self.gpu
            .queue
            .write_buffer(&self.csm_params_buf, 0, bytemuck::bytes_of(&csm_raw));
        for (i, light_vp) in light_vps.iter().enumerate() {
            let raw = CascadeParamsRaw {
                view_proj: light_vp.to_cols_array_2d(),
            };
            self.gpu
                .queue
                .write_buffer(&self.cascade_bufs[i], 0, bytemuck::bytes_of(&raw));
        }

        // 2. 收集实例（ECS 查询 -> Visibility 过滤 + 视锥剔除 + alpha 分区
        //    -> 排序：不透明按 (网格, 材质) 键，透明按视深降序）
        let frustum = Frustum::from_view_proj(&vp);
        let mut items = std::mem::take(&mut self.scratch);
        let mut skinned = std::mem::take(&mut self.skinned_scratch);
        let mut transparent = std::mem::take(&mut self.transparent_scratch);
        let mut transparent_skinned = std::mem::take(&mut self.transparent_skinned_scratch);
        let counts = collect_instances(
            world,
            &mesh_store,
            &material_store,
            &view,
            eye,
            &frustum,
            &mut items,
            &mut skinned,
            &mut transparent,
            &mut transparent_skinned,
            // 主相机收集 = LOD v2 唯一消费点（缺席 = None = v1 逐位不变）
            world.try_resource::<LodSettings>().map(|s| *s),
            Some(&mut self.lod_v2_memory),
        );
        items.sort_unstable_by_key(|item| item.key);
        // 透明排序：视深降序（远 → 近，back-to-front）——alpha 混合不满足
        // 交换律，必须先画远的，近面才能以自身 alpha 正确叠加在远面结果上。
        // 深度并列按实体 bits 升序断言：比较器因此是全序，`sort_unstable`
        // 的结果不依赖输入顺序（确定性）；并列深度的绘制先后只影响并列项
        // 之间的叠放，两者视深相同、任一顺序在视觉上等价。
        // `total_cmp` 给出全序比较（NaN 也有确定位置），并列再比实体。
        transparent.sort_unstable_by(|a, b| {
            b.depth
                .total_cmp(&a.depth)
                .then_with(|| a.entity.cmp(&b.entity))
        });
        transparent_skinned.sort_unstable_by(|a, b| {
            b.depth
                .total_cmp(&a.depth)
                .then_with(|| a.skinned.skin_entity.cmp(&b.skinned.skin_entity))
        });
        // 2c. 粒子收集（须在 update_particles 之后，调用契约见 particles.rs）：
        // 发射器包围球剔除 → additive 段不排序、alpha 段按视深降序
        //（远 → 近；total_cmp + 收集序号并列断言，比较器全序、与收集顺序
        // 无关——确定性）。alpha 实例按排序后的次序追加在 additive 段之后，
        // 两个混合段共享同一实例缓冲，各一次 draw。
        let mut particle_instances = std::mem::take(&mut self.particle_scratch);
        let mut particle_alpha = std::mem::take(&mut self.particle_alpha_scratch);
        let particle_total = collect_particles(
            world,
            &view,
            &frustum,
            &mut particle_instances,
            &mut particle_alpha,
        );
        particle_alpha.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let particle_additive_len = particle_instances.len();
        particle_instances.extend(particle_alpha.iter().map(|(_, _, inst)| *inst));
        // 2c'. 粒子 v2 收集（R82）：排水待消费模拟帧 + 构建拖尾渲染
        //      uniform + 池对齐（新实体建池 / reset 检测 / uniform 槽位
        //      按需增长——建池需要 &mut self，必须在 hdr 借用之前，与 4c
        //      同款纪律）。须在 update_particles_v2 之后（调用契约见
        //      particles_v2.rs）；组件缺席 = 空表 = 零计算零绘制。
        let mut particle_v2_items = std::mem::take(&mut self.particle_v2_scratch);
        collect_particles_v2(
            world,
            1.0 / (fov * 0.5).tan(),
            aspect,
            &mut particle_v2_items,
        );
        self.ensure_particle_v2_pools(&mut particle_v2_items);
        // 2c''. meshlet 收集 + 帧上传 + 绘制裁决（R85；R86 LOD 选择；
        //       R87 流送门控 + 遮挡查询条目）：实例表条目（含全局簇号
        //       前缀）与兜底裁决（无有效紧凑回读 ⇒ CPU 写恒等列表全量
        //       绘制）。hdr 不可变借用之前完成全部 &mut 操作（tick /
        //       上传 / 默认材质预热），计划贯穿主 pass 与 compute 编码。
        let mut meshlet_plan: Option<MeshletDrawPlan> = None;
        let mut meshlet_occ_hidden = 0u32;
        if let Some(store) = meshlet_store.as_deref() {
            if !store.is_empty() {
                // R87 流送激活裁决：设置在场且预算 Some；缺席 / None =
                // 全驻留 = R86 行为逐位不变（页表仍随注册构建，但不门
                // 控、不逐页上传）。active → inactive 迁移 ⇒ 驻留状态
                // 作废 + 下次注册整库重传（页可能已被逐出 arena）。
                let stream_settings =
                    world.try_resource::<crate::meshlet_stream::MeshletStreamSettings>();
                let stream_active = stream_settings
                    .as_ref()
                    .is_some_and(|s| s.gpu_byte_budget.is_some());
                if !stream_active && self.meshlet_stream.is_some() {
                    self.meshlet_stream = None;
                    self.meshlet_stream_requests.clear();
                    self.meshlet_stream_recover_upload = true;
                }
                if stream_active && self.meshlet_stream.is_none() {
                    self.meshlet_stream = Some(crate::meshlet_stream::ResidencyManager::new());
                }
                // LOD 设置（R86）：资源缺席 / enabled = false ⇒ 参数不
                // 组装 = 全叶表（R85 行为逐位不变）。流送激活时参数必
                // 须在场（选择视口）——阈值取 LOD 设置（启用时），否则
                // 0（全叶理想：选择必下钻到叶组，页缺再逐组回退）。
                let lod_on = world
                    .try_resource::<crate::meshlet::MeshletLodSettings>()
                    .filter(|s| s.enabled);
                let lod = if lod_on.is_some() || stream_active {
                    Some(MeshletLodFrameParams {
                        eye,
                        screen_height: height.max(1) as f32,
                        tan_half_fov: (fov * 0.5).tan(),
                        threshold_px: lod_on.map(|s| s.threshold_px).unwrap_or(0.0),
                    })
                } else {
                    None
                };
                // 驻留 tick（R87）：上一帧收集的请求 → 本帧上传 / 驱逐
                // 裁决 → write_buffer 落 arena（随本次提交生效，本帧绘
                // 制即可消费）。请求一帧延迟 = R85 紧凑回读的同款节拍。
                let residency_ops = {
                    let page_bytes = {
                        let arena = &self.meshlet_arena;
                        move |page: u32| -> u64 {
                            for slot in arena.slots.iter().flatten() {
                                let pc = slot.pages.page_count();
                                if page >= slot.page_base && page < slot.page_base + pc {
                                    return slot.pages.page_bytes(page - slot.page_base);
                                }
                            }
                            0
                        }
                    };
                    let requests = std::mem::take(&mut self.meshlet_stream_requests);
                    match (
                        self.meshlet_stream.as_mut(),
                        stream_settings.as_ref().and_then(|s| s.gpu_byte_budget),
                    ) {
                        (Some(mgr), Some(budget)) => Some(
                            mgr.tick(
                                budget,
                                stream_settings
                                    .as_ref()
                                    .map(|s| s.pages_per_frame)
                                    .unwrap_or(4),
                                &requests,
                                &page_bytes,
                            ),
                        ),
                        _ => None,
                    }
                };
                if let Some(ops) = residency_ops.as_ref() {
                    self.apply_residency_ops(store, ops);
                }
                let mut frame = std::mem::take(&mut self.meshlet_frame_scratch);
                collect_meshlet_instances(
                    world,
                    &frustum,
                    &self.meshlet_arena.slots,
                    lod,
                    stream_active.then(|| {
                        world
                            .try_resource::<crate::meshlet::MeshletLodSettings>()
                            .filter(|s| s.enabled)
                            .map(|s| s.threshold_px)
                            .unwrap_or(0.0)
                    }),
                    self.meshlet_stream.as_ref().map(|m| m.resident()),
                    occlusion_on,
                    &self.occ_hidden,
                    &mut frame,
                );
                // 本帧请求 / 遮挡条目移交（下一帧 tick 与 2e 段的输入）
                self.meshlet_stream_requests = std::mem::take(&mut frame.stream_requests);
                self.meshlet_occ_aabb_scratch = std::mem::take(&mut frame.occ_aabbs);
                self.meshlet_occ_entity_scratch = std::mem::take(&mut frame.occ_entities);
                meshlet_occ_hidden = frame.occ_hidden_count;
                if frame.total_clusters > 0 {
                    // 三角形统计的解析表（下一帧消费读回时按此对账）
                    self.meshlet_last_handles.clear();
                    self.meshlet_last_handles.extend_from_slice(&frame.handles);
                    self.meshlet_last_run_starts.clear();
                    self.meshlet_last_run_starts
                        .extend_from_slice(&frame.run_starts);
                    let plan = self.prepare_meshlet_frame(
                        &frame,
                        &material_store,
                        texture_store.as_deref(),
                        lightmap_store.as_deref(),
                        vp,
                        eye,
                    );
                    meshlet_plan = Some(plan);
                }
                self.meshlet_frame_scratch = frame;
            } else {
                // 本帧无 meshlet 几何：紧凑结果作废——它与产出帧的实例表
                // 槽位配对，跳帧会破坏槽位配对契约；下一活动帧走恒等全量
                // 兜底重新建立（像素与剔除帧恒等，无视觉跳变）
                self.meshlet_consumed = None;
            }
        } else {
            self.meshlet_consumed = None;
        }
        // 2d. 贴花收集（R39）：Visibility 过滤 + 祖先级联 + 变换后 AABB
        //     视锥剔除，随后按纹理句柄排序分批（同纹理连续区间 = 1 draw；
        //     批内实体 bits 并列序——确定性，与收集顺序无关）。无贴花时
        //     整个 pass 跳过（零开销默认路径）。
        let mut decal_items = std::mem::take(&mut self.decal_scratch);
        // C2：贴花受光开关（资源缺席 = 恒 false = v1 逐位不变）
        let decal_lit = world.contains_resource::<crate::decal::DecalLighting>();
        let decal_culled = collect_decals(world, &frustum, &mut decal_items, decal_lit);
        decal_items.sort_by(|a, b| a.texture.cmp(&b.texture).then(a.entity.cmp(&b.entity)));
        let decal_total = decal_items.len();
        let total = items.len();
        let skinned_total = skinned.len();
        let transparent_total = transparent.len();
        let instance_total = total + skinned_total + transparent_total + transparent_skinned.len();
        self.stats.culled_instances = counts.culled + decal_culled;
        if counts.culled > 0 {
            log::trace!(
                "视锥剔除 {} / {} 实例（普通 + 蒙皮，含透明）",
                counts.culled,
                counts.visible + counts.culled as usize
            );
        }
        // 2e. 遮挡查询（R53）：查询列表 = 排序后全部普通不透明实例的
        //     世界 AABB（**含已遮挡者**——被剔实体必须持续重查，否则没有
        //     恢复可见的路径；无 CPU 包围盒的网格不查询、恒可见；超出
        //     4096 槽上限的尾部不查询、恒可见——都是保守方向）。剔除
        //     裁决只作用于主 pass 绘制（阴影 / 深度副本保留，保守性论证
        //     见 occlusion.rs），且仅在上一帧查询的新鲜结果上生效。
        //     items 序 = 排序后确定序 ⇒ 查询槽 ↔ 实体映射稳定（确定性）。
        //     R87：meshlet 实体的簇联合盒条目追加在普通实例之后（收集
        //     序 = 查询序，映射同样稳定）；4096 上限对合并列表统一封顶，
        //     尾部不查询恒可见（保守方向）。
        let mut occ_drawn = std::mem::take(&mut self.occ_drawn_scratch);
        let mut occ_aabbs = std::mem::take(&mut self.occ_aabb_scratch);
        let mut occ_entities = std::mem::take(&mut self.occ_entity_scratch);
        let meshlet_occ_aabbs = std::mem::take(&mut self.meshlet_occ_aabb_scratch);
        let meshlet_occ_entities = std::mem::take(&mut self.meshlet_occ_entity_scratch);
        occ_drawn.clear();
        occ_aabbs.clear();
        occ_entities.clear();
        let mut occ_filtered = false;
        if occlusion_on && (total > 0 || !meshlet_occ_aabbs.is_empty()) {
            self.ensure_occ_gpu();
            for item in items.iter().take(wgpu::QUERY_SET_MAX_QUERIES as usize) {
                let Some((min, max)) = mesh_store
                    .get(MeshHandle((item.key >> 32) as u32))
                    .and_then(|data| data.bounds())
                else {
                    continue;
                };
                let (lo, hi) = world_aabb(
                    Vec3::from(min),
                    Vec3::from(max),
                    &Mat4::from_cols_array_2d(&item.instance.model),
                );
                occ_aabbs.push(AabbInstanceRaw {
                    min: [lo.x, lo.y, lo.z, 0.0],
                    max: [hi.x, hi.y, hi.z, 0.0],
                });
                occ_entities.push(item.entity);
            }
            for (&aabb, &entity) in meshlet_occ_aabbs.iter().zip(meshlet_occ_entities.iter()) {
                if occ_aabbs.len() >= wgpu::QUERY_SET_MAX_QUERIES as usize {
                    break;
                }
                occ_aabbs.push(aabb);
                occ_entities.push(entity);
            }
            if occ_fresh && !self.occ_hidden.is_empty() {
                occ_drawn.resize(items.len(), true);
                let mut culled_now = 0u32;
                for (i, item) in items.iter().enumerate() {
                    if self.occ_hidden.contains(&item.entity) {
                        occ_drawn[i] = false;
                        culled_now += 1;
                    }
                }
                // meshlet 侧的隐藏裁决发生在收集期（occ_hidden 同源——
                // 本帧 tick 已消费的读回结果），这里只并入统计口径。
                culled_now += meshlet_occ_hidden;
                self.stats.culled_instances += culled_now;
                self.stats.occlusion_culled = culled_now;
                occ_filtered = true;
            }
            // 容量复用必须在贯穿后续全部 pass 的 hdr 不可变借用建立之前
            // （缓冲重建需 &mut self）；空列表 = 零开销 no-op，5a 段据此
            // 直接取用。
            self.ensure_occ_aabb_capacity(occ_aabbs.len());
        }

        // 2b. 点光阴影 pass 的实例可见性（CPU 粗剔除，R61 起**逐槽位**）：
        // 实例包围球到该槽位投影灯的距离减去半径后仍超出 range，则六个
        // 90° 面的并集（= 以灯为中心、range 为半径的完整球域）必然不覆盖
        // 它，该灯的六面全部跳过。结果只用于阴影 pass 的绘制区间划分，
        // 不影响主 pass 批处理。布局 = 槽位主序（`slot * items.len() + i`），
        // 与渲染循环同序，避免 Vec<Vec<bool>> 的逐槽位分配。
        let mut point_shadow_visible: Vec<bool> = Vec::new();
        if caster_count > 0 {
            point_shadow_visible.reserve(caster_count * items.len());
            for &candidate in &caster_slots {
                let light_slot = caster_candidates[candidate].light_slot;
                let light_pos = Vec3::new(
                    point_lights[light_slot].position[0],
                    point_lights[light_slot].position[1],
                    point_lights[light_slot].position[2],
                );
                let range = point_lights[light_slot].color[3];
                point_shadow_visible.extend(items.iter().map(|item| {
                    let Some(data) = mesh_store.get(MeshHandle((item.key >> 32) as u32)) else {
                        return true;
                    };
                    let Some((min, max)) = data.bounds() else {
                        return true;
                    };
                    let min = Vec3::from(min);
                    let max = Vec3::from(max);
                    let model = Mat4::from_cols_array_2d(&item.instance.model);
                    let center = model.transform_point3((min + max) * 0.5);
                    let scale = model
                        .x_axis
                        .truncate()
                        .length()
                        .max(model.y_axis.truncate().length())
                        .max(model.z_axis.truncate().length());
                    let radius = (max - min).length() * 0.5 * scale;
                    (center - light_pos).length() - radius <= range
                }));
            }
        }

        // 3. 实例缓冲（容量不足时重建）+ 跨帧增量上传：剔除排序键后按
        //    112B 紧凑布局写入。实例区间布局（后续绘制按下标引用）：
        //    [0, total) 不透明普通 → [total, total+skinned) 不透明蒙皮 →
        //    [.., ..+transparent) 透明普通（视深降序）→ 末段透明蒙皮
        //    （视深降序）。透明实例与不透明共用同一缓冲：阴影 pass 只
        //    绘制前两段（透明不投影），透明 pass 绘制后两段。
        //    实例缓冲跨帧持久：GPU 缓冲不因帧切换失效，上一帧上传内容由
        //    `last_instances` 镜像记录，本帧组装完成后逐实例比对——完全
        //    一致则整帧跳过上传（静态场景零上传），存在变化则只写连续
        //    变化段，缓冲重建 / 实例数变化 / 变化过半时回退全量。
        if instance_total > 0 {
            let rebuilt = self.ensure_instance_capacity(instance_total);
            let mut instances = std::mem::take(&mut self.instance_scratch);
            instances.clear();
            instances.extend(items.iter().map(|item| item.instance));
            instances.extend(skinned.iter().map(|item| item.instance));
            instances.extend(transparent.iter().map(|item| item.instance));
            instances.extend(transparent_skinned.iter().map(|t| t.skinned.instance));
            let count = instances.len();
            let instance_bytes = std::mem::size_of::<InstanceRaw>() as u64;
            let buffer = self.instance_buf.as_ref().expect("实例缓冲已就绪");
            let mut uploaded_bytes = 0u64;
            if rebuilt || self.last_instances.len() != count {
                // 缓冲刚重建（旧内容随缓冲作废）或实例数变化：全量上传
                self.gpu
                    .queue
                    .write_buffer(buffer, 0, bytemuck::cast_slice(&instances));
                uploaded_bytes = count as u64 * instance_bytes;
            } else {
                let ranges = diff_changed_ranges(&self.last_instances, &instances);
                let changed: usize = ranges.iter().map(|r| r.len()).sum();
                if changed as f32 > count as f32 * INSTANCE_DIFF_FULL_FALLBACK_RATIO {
                    // 变化过半：碎段编码开销超过带宽收益，回退全量
                    self.gpu
                        .queue
                        .write_buffer(buffer, 0, bytemuck::cast_slice(&instances));
                    uploaded_bytes = count as u64 * instance_bytes;
                } else if changed > 0 {
                    for range in ranges {
                        // write_buffer 的 offset 必须 4 字节对齐；InstanceRaw
                        // 112B = 4 × 28，实例粒度偏移天然满足
                        self.gpu.queue.write_buffer(
                            buffer,
                            range.start as u64 * instance_bytes,
                            bytemuck::cast_slice(&instances[range]),
                        );
                    }
                    uploaded_bytes = changed as u64 * instance_bytes;
                }
            }
            self.stats.instance_upload_bytes = uploaded_bytes;
            // 零拷贝交换：镜像取得本帧数据，上帧镜像连同其分配交还
            // instance_scratch——下帧 clear + extend 直接复用该容量
            std::mem::swap(&mut self.last_instances, &mut instances);
            self.instance_scratch = instances;
        }

        // 4. 预先完成 GPU 资源懒上传（避免与渲染通道编码的借用冲突）。
        // 段划分与绘制段同源（run_length：同键且同图集），保证绘制时
        // material_gpu 恒命中。
        let mut index = 0;
        while index < total {
            let key = items[index].key;
            let lightmap = items[index].lightmap;
            let count = run_length(&items, index);
            let mesh = MeshHandle((key >> 32) as u32);
            let material = MaterialHandle(key as u32);
            self.ensure_mesh(&mesh_store, mesh);
            self.ensure_material(
                &material_store,
                texture_store.as_deref(),
                lightmap_store.as_deref(),
                material,
                lightmap,
            );
            index += count;
        }
        // 4a. 蒙皮资源：网格 / 材质 + 关节矩阵缓冲（动画驱动下每帧覆写）。
        // 不透明蒙皮与透明蒙皮同构处理，只是分属两个实例区间 / 两条管线。
        for item in skinned
            .iter()
            .chain(transparent_skinned.iter().map(|t| &t.skinned))
        {
            self.ensure_mesh(&mesh_store, MeshHandle(item.mesh));
            self.ensure_material(
                &material_store,
                texture_store.as_deref(),
                lightmap_store.as_deref(),
                MaterialHandle(item.material),
                item.lightmap,
            );
        }
        let mut joint_bytes: Vec<u8> = Vec::new();
        for item in skinned
            .iter()
            .chain(transparent_skinned.iter().map(|t| &t.skinned))
        {
            self.ensure_skin(item.skin_entity, item.joint_matrices.len());
            joint_bytes.clear();
            for m in &item.joint_matrices {
                joint_bytes.extend_from_slice(bytemuck::bytes_of(&m.to_cols_array_2d()));
            }
            let skin = self
                .skin_gpu
                .get(&item.skin_entity)
                .expect("蒙皮缓冲已就绪");
            self.gpu
                .queue
                .write_buffer(&skin.joint_buf, 0, &joint_bytes);
        }
        // 形变目标解算（C4a，阴影 / 主 / 深度各 pass 消费同一份覆盖，
        // 全体缺席时零触碰——脏检保证静态权重不重混合不重上传）
        self.resolve_morphs(&mesh_store, &mut skinned, &mut transparent_skinned);
        // 4b. 透明普通项的网格 / 材质懒上传（透明蒙皮已在 4a 覆盖）。
        // 透明路径不采样光照贴图（v1 限制）⇒ 图集槽位恒 NO_LIGHTMAP。
        for item in &transparent {
            self.ensure_mesh(&mesh_store, MeshHandle((item.key >> 32) as u32));
            self.ensure_material(
                &material_store,
                texture_store.as_deref(),
                lightmap_store.as_deref(),
                MaterialHandle(item.key as u32),
                NO_LIGHTMAP,
            );
        }
        // 4d. 贴花资源（R39）：纹理懒上传 + 组 1 绑定组按纹理缓存构建 +
        //     实例缓冲容量复用与全量上传。放在 HDR 目标借用（hdr）建立
        //     之前：容量不足的缓冲重建需要 &mut self，会与贯穿后续全部
        //     pass 的 hdr 借用冲突（同粒子 4c 的理由）。贴花实例少且常
        //     静止，但静态场景同批也无从比对（独立缓冲、144B/实例），v1
        //     每帧全量上传——增量比对留待实测出现热点再做。
        for item in &decal_items {
            let handle = crate::texture::TextureHandle(item.texture);
            self.ensure_texture(texture_store.as_deref(), handle);
            if !self.decal_bind_groups.contains_key(&item.texture) {
                // 句柄不存在时 ensure_texture 只告警不上传——回退 0 号
                // 内置白色兜底纹理的绑定（与材质缺纹理告警同哲学）
                let texture = self
                    .texture_gpu
                    .get(&item.texture)
                    .unwrap_or_else(|| &self.texture_gpu[&0]);
                let bind_group = self
                    .gpu
                    .device
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("iris.decal.bg"),
                        layout: &self.bundle.decal_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(&texture.view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(&texture.sampler),
                            },
                        ],
                    });
                self.decal_bind_groups.insert(item.texture, bind_group);
            }
        }
        if decal_total > 0 {
            self.ensure_decal_capacity(decal_total);
            let mut decal_raws = std::mem::take(&mut self.decal_instance_scratch);
            decal_raws.clear();
            decal_raws.extend(decal_items.iter().map(|item| item.instance));
            let decal_buf = self
                .decal_instance_buf
                .as_ref()
                .expect("贴花实例缓冲已就绪");
            self.gpu
                .queue
                .write_buffer(decal_buf, 0, bytemuck::cast_slice(&decal_raws));
            self.decal_instance_scratch = decal_raws;
        }

        // 5. 编码渲染通道：先阴影 pass（光源视角，深度写入颜色附件；
        //     只画不透明实例区间——透明项不投影），后主 pass、贴花 pass
        //     （蒙皮之后、透明之前，见下）与透明 pass
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("iris.frame"),
            });
        let mut draw_calls = 0u32;
        let mut meshlet_surviving = 0u32;
        let mut instances = 0u32;
        let mut triangles_drawn = 0u32;
        let mut particles_drawn = 0u32;
        // 阴影 pass 的几何量 = 不透明普通 + 不透明蒙皮（收集期分区排除
        // 了透明项，透明蒙皮同样不投影）
        let opaque_total = total + skinned_total;
        if shadow_enabled && opaque_total > 0 {
            let instance_buf = self.instance_buf.as_ref().expect("实例缓冲已就绪");
            // 逐级联一个渲染 pass（各写数组纹理的对应层）。v1 策略保留：
            // 实例收集沿用主 pass 视锥剔除后的不透明区间，三个级联全部
            // 重画同一区间——保守不漏影（按级联视锥再剔除是后续优化点，
            // 见 ARCHITECTURE §4.7.2）。
            for cascade_i in 0..SHADOW_CASCADES {
                let mut shadow_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("iris.pass.shadow_cascade"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.shadow_color_views[cascade_i],
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            // 清空为 1.0（光源远平面）：贴图空缺处视为无遮挡
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 1.0,
                                g: 1.0,
                                b: 1.0,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &self.shadow_depth_views[cascade_i],
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Discard,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                shadow_pass.set_pipeline(&self.bundle.shadow_pipeline);
                shadow_pass.set_bind_group(0, &self.cascade_bgs[cascade_i], &[]);
                shadow_pass.set_vertex_buffer(
                    1,
                    self.instance_buf
                        .as_ref()
                        .expect("实例缓冲已就绪")
                        .slice(..),
                );
                index = 0;
                while index < total {
                    let key = items[index].key;
                    let mut count = 1;
                    while index + count < total && items[index + count].key == key {
                        count += 1;
                    }
                    let mesh = &self.mesh_gpu[&((key >> 32) as u32)];
                    shadow_pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
                    shadow_pass
                        .set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    let first = index as u32;
                    let last = (index + count) as u32;
                    shadow_pass.draw_indexed(0..mesh.num_indices, 0, first..last);
                    index += count;
                }
                // 蒙皮几何体：独立阴影管线（组 1 = 关节矩阵），阴影随骨骼变形
                if !skinned.is_empty() {
                    shadow_pass.set_pipeline(&self.bundle.skinned_shadow_pipeline);
                    shadow_pass.set_vertex_buffer(2, instance_buf.slice(..));
                    for (i, item) in skinned.iter().enumerate() {
                        let mesh = &self.mesh_gpu[&item.mesh];
                        let skin = &self.skin_gpu[&item.skin_entity];
                        shadow_pass.set_vertex_buffer(0, item.vertex_buf(mesh).slice(..));
                        shadow_pass.set_vertex_buffer(
                            1,
                            mesh.skin_buf
                                .as_ref()
                                .expect("蒙皮网格应有蒙皮属性缓冲")
                                .slice(..),
                        );
                        shadow_pass
                            .set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                        shadow_pass.set_bind_group(1, &skin.bind_group, &[]);
                        let base = (total + i) as u32;
                        shadow_pass.draw_indexed(0..mesh.num_indices, 0, base..base + 1);
                    }
                }
            }
        }
        // 5b. 点光 cubemap 阴影：每个预算内投影灯（R61：0..预算盏）对 6 面
        // 各开一个 render pass，逐灯写入 cubemap 数组自己的层区间
        // （层 = 槽位 × 6 + 面）。颜色附件（该层 R32Float 视图）与深度附件
        // 都清空为 1.0（1.0 = range/range = 无遮挡）；存储值为归一化线性
        // 距离（见 fs_point），深度附件 Less 测试保证每 texel 存最近遮挡物
        // （方向光阴影的教训）。
        if caster_count > 0 {
            // 每（槽位, 面）独立小 uniform：一缓冲一面，全部 write_buffer 后
            // 随本帧统一 submit（write_buffer 按 submit 顺序在 pass 执行前
            // 生效）。槽位 = cubemap 数组下标，与选择键序一致。
            for (slot, &candidate) in caster_slots.iter().enumerate() {
                let light_slot = caster_candidates[candidate].light_slot;
                let light_pos = Vec3::new(
                    point_lights[light_slot].position[0],
                    point_lights[light_slot].position[1],
                    point_lights[light_slot].position[2],
                );
                let range = point_lights[light_slot].color[3];
                for face in 0..6 {
                    let raw = PointFaceParamsRaw {
                        view_proj: PointLight::face_view_proj(light_pos, face, range)
                            .to_cols_array_2d(),
                        light_pos_range: [light_pos.x, light_pos.y, light_pos.z, range],
                    };
                    self.gpu.queue.write_buffer(
                        &self.point_shadow_face_bufs[slot * 6 + face],
                        0,
                        bytemuck::bytes_of(&raw),
                    );
                }
            }
            if opaque_total > 0 {
                let instance_buf = self.instance_buf.as_ref().expect("实例缓冲已就绪");
                for slot in 0..caster_count {
                    // 该槽位的实例可见性切片（布局 = 槽位主序）
                    let visible = &point_shadow_visible[slot * total..(slot + 1) * total];
                    for face in 0..6 {
                        let layer = slot * 6 + face;
                        let mut point_pass =
                            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                                label: Some("iris.pass.point_shadow"),
                                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                    view: &self.point_shadow_face_views[layer],
                                    depth_slice: None,
                                    resolve_target: None,
                                    ops: wgpu::Operations {
                                        load: wgpu::LoadOp::Clear(wgpu::Color {
                                            r: 1.0,
                                            g: 1.0,
                                            b: 1.0,
                                            a: 1.0,
                                        }),
                                        store: wgpu::StoreOp::Store,
                                    },
                                })],
                                depth_stencil_attachment: Some(
                                    wgpu::RenderPassDepthStencilAttachment {
                                        view: &self.point_shadow_depth_face_views[layer],
                                        depth_ops: Some(wgpu::Operations {
                                            load: wgpu::LoadOp::Clear(1.0),
                                            store: wgpu::StoreOp::Discard,
                                        }),
                                        stencil_ops: None,
                                    },
                                ),
                                timestamp_writes: None,
                                occlusion_query_set: None,
                                multiview_mask: None,
                            });
                        point_pass.set_pipeline(&self.bundle.point_shadow_pipeline);
                        point_pass.set_bind_group(0, &self.point_shadow_face_bgs[layer], &[]);
                        point_pass.set_vertex_buffer(1, instance_buf.slice(..));
                        // 超出 range 的实例已在该槽位的 point_shadow_visible
                        // 中剔除；可见性变化处拆分绘制区间，保持同 key 连续
                        // 实例合批
                        index = 0;
                        while index < total {
                            if !visible[index] {
                                index += 1;
                                continue;
                            }
                            let key = items[index].key;
                            let mut count = 1;
                            while index + count < total
                                && items[index + count].key == key
                                && visible[index + count]
                            {
                                count += 1;
                            }
                            let mesh = &self.mesh_gpu[&((key >> 32) as u32)];
                            point_pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
                            point_pass.set_index_buffer(
                                mesh.index_buf.slice(..),
                                wgpu::IndexFormat::Uint32,
                            );
                            let first = index as u32;
                            let last = (index + count) as u32;
                            point_pass.draw_indexed(0..mesh.num_indices, 0, first..last);
                            index += count;
                        }
                        // 蒙皮批次：实例区间 [total, total+skinned)，v1 不做距离剔除
                        if !skinned.is_empty() {
                            point_pass.set_pipeline(&self.bundle.skinned_point_shadow_pipeline);
                            point_pass.set_vertex_buffer(2, instance_buf.slice(..));
                            for (i, item) in skinned.iter().enumerate() {
                                let mesh = &self.mesh_gpu[&item.mesh];
                                let skin = &self.skin_gpu[&item.skin_entity];
                                point_pass.set_vertex_buffer(0, item.vertex_buf(mesh).slice(..));
                                point_pass.set_vertex_buffer(
                                    1,
                                    mesh.skin_buf
                                        .as_ref()
                                        .expect("蒙皮网格应有蒙皮属性缓冲")
                                        .slice(..),
                                );
                                point_pass.set_index_buffer(
                                    mesh.index_buf.slice(..),
                                    wgpu::IndexFormat::Uint32,
                                );
                                point_pass.set_bind_group(1, &skin.bind_group, &[]);
                                let base = (total + i) as u32;
                                point_pass.draw_indexed(0..mesh.num_indices, 0, base..base + 1);
                            }
                        }
                    }
                }
            }
        }
        // RT（R40）渲染目标 pass：每台 RT 相机渲一次到其纹理（几何 pass +
        // ACES blit）。位于阴影 pass 之后（复用刚写出的阴影贴图 / CSM）、
        // 主 pass 的 hdr 借用建立之前（RT 资源惰性创建需 &mut self）。
        // FrameStats 口径 = 主几何，RT 辅助视口不计入。
        for prep in &rt_cameras {
            self.render_to_rt_target(
                world,
                &mut encoder,
                &mesh_store,
                &material_store,
                texture_store.as_deref(),
                lightmap_store.as_deref(),
                &globals,
                prep,
            );
        }
        // 平面水体（R81）反射 / 折射捕获：阴影 / RT 相机 pass 之后（复用
        // 本帧阴影产物）、主 pass 的 hdr 借用建立之前（捕获资源的惰性
        // 创建需要 &mut self）。每帧两张半分辨率图——反射 = 镜像相机
        //（view × 反射矩阵），折射 = 主相机视角；捕获内容 = 不透明几何
        // + 天空（探针捕获同款最低配组装），**不含水体自身**（无递归）。
        // 内部捕获不占用户的 RT 相机预算（§4.21/§8.20 的 R81 修订）。
        if water_on {
            self.encode_water_captures(
                world,
                &mut encoder,
                &mesh_store,
                &material_store,
                texture_store.as_deref(),
                lightmap_store.as_deref(),
                &globals,
                eye,
                vp,
                water_refl_view,
                water_refl_vp,
                water_refl_eye,
                &items,
                &skinned,
                total,
            );
        }
        // 4c. 粒子实例上传（独立缓冲，每帧全量——粒子逐帧全变，帧级比对
        // 无增量价值）与相机基向量写入。放在 HDR 目标借用（hdr）建立之前：
        // 容量不足时的缓冲重建需要 &mut self，会与贯穿后续全部 pass 的
        // hdr 借用冲突。
        if particle_total > 0 {
            let cam = ParticleCameraRaw {
                right: [view.x_axis.x, view.y_axis.x, view.z_axis.x, 0.0],
                up: [view.x_axis.y, view.y_axis.y, view.z_axis.y, 0.0],
            };
            self.gpu
                .queue
                .write_buffer(&self.particle_camera_buf, 0, bytemuck::bytes_of(&cam));
            self.ensure_particle_capacity(particle_instances.len());
            let particle_buf = self.particle_buf.as_ref().expect("粒子实例缓冲已就绪");
            self.gpu
                .queue
                .write_buffer(particle_buf, 0, bytemuck::cast_slice(&particle_instances));
        }
        // 4c'. 粒子 v2 模拟（R82，全仓首个 compute 管线）：逐份待消费帧
        // write_buffer 模拟 uniform + compute pass 恒定 dispatch。编码在
        // 全部 render pass **之前**——同一编码器按序执行，后续渲染 pass
        // 读到的是本帧模拟后的粒子状态。每份帧一个 uniform 槽位（write
        // 与 dispatch 无法在同一 submit 内交错，逐槽配对保证帧参数与
        // dispatch 一一对应）；帧队列空（重复渲染）= 零开销跳过。
        if !particle_v2_items.is_empty() {
            for item in &particle_v2_items {
                let pool = &self.particle_v2_pools[item.pool];
                for (slot, frame) in item.frames.iter().enumerate() {
                    self.gpu.queue.write_buffer(
                        &pool.sim_uniforms[slot],
                        0,
                        bytemuck::bytes_of(frame),
                    );
                }
            }
            let any_dispatch = particle_v2_items.iter().any(|i| !i.frames.is_empty());
            if any_dispatch {
                let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("iris.pass.particle_v2_sim"),
                    timestamp_writes: None,
                });
                cpass.set_pipeline(&self.bundle.particle_v2_sim_pipeline);
                for item in &particle_v2_items {
                    if item.frames.is_empty() {
                        continue;
                    }
                    let pool = &self.particle_v2_pools[item.pool];
                    for slot in 0..item.frames.len() {
                        cpass.set_bind_group(0, &pool.sim_bgs[slot], &[]);
                        // 恒定 dispatch：池容量 / 工作组大小（常量算出，
                        // 不随存活数变——会话内逐位确定性的根基）
                        cpass.dispatch_workgroups(crate::particles_v2::PARTICLES_V2_DISPATCH, 1, 1);
                    }
                }
            }
        }
        // SSR 参数（R62b）：上帧重投影矩阵（prev_vp = 上帧实际渲染矩阵，
        // 与 MB/TAA 同源）+ 标量。`active = 0` 的帧（首帧启用 / 关→开 /
        // 分辨率变化 / 切镜 / 上帧或本帧无不透明几何——深度副本没跑，
        // 「上帧深度」语义不成立）整段跳过 ⇒ 输出与 SSR 关闭帧逐位一致。
        // SSR 关闭路径不发 write_buffer；退场的那一帧回写零值（探针
        // params_active 同款记账——残留 flags.x = 1 会拿陈旧资源步进）。
        if ssr_on {
            let ssr_cfg = ssr_settings.expect("ssr_on 蕴含资源在");
            let active = ssr_active && opaque_total > 0;
            self.gpu.queue.write_buffer(
                &self.ssr.params_buf,
                0,
                bytemuck::bytes_of(&SsrParamsRaw {
                    prev_view_proj: prev_vp.to_cols_array_2d(),
                    flags: [
                        if active { 1.0 } else { 0.0 },
                        ssr_cfg.intensity.max(0.0),
                        ssr_cfg.roughness_cutoff.clamp(0.0, 1.0),
                        ssr_cfg.edge_margin.max(0.0),
                    ],
                    extent: [
                        ssr_cfg.max_distance.max(1e-3),
                        ssr_cfg.thickness.max(1e-3),
                        0.0,
                        0.0,
                    ],
                }),
            );
            self.ssr_uniform_live = true;
            self.stats.ssr_on = if active { 1 } else { 0 };
        } else if self.ssr_uniform_live {
            self.gpu.queue.write_buffer(
                &self.ssr.params_buf,
                0,
                bytemuck::bytes_of(&SsrParamsRaw {
                    prev_view_proj: [[0.0; 4]; 4],
                    flags: [0.0; 4],
                    extent: [0.0; 4],
                }),
            );
            self.ssr_uniform_live = false;
        }
        // AO（R77）：GTAO 可见度图 pass——编码在主 pass **之前**（点光
        // 阴影之后），读上一帧深度副本：scene_depth pass 在本帧编码于主
        // pass 之后，同一编码器按序执行 ⇒ 此处读到的恒为上一帧内容（与
        // SSR 同款「上帧」语义，§4.31 裁决 2）。有效帧（上一帧副本确实
        // 跑过——`ao_depth_frame` 编号恰好 = 本帧 − 1）重算可见度图；
        // 无效帧（首帧启用 / 上帧无几何 / 上帧 AO 关闭且无人跑副本）只
        // 以清屏值 1.0 覆写——输出与 AO 关闭帧逐位一致（重置语义）。
        // 关闭帧整段跳过（哑纹素 × 1.0 兜底，零编码开销）。
        let ao_active = ao_on && self.ao_depth_frame == Some(self.occ_frame - 1);
        if ao_on {
            let cfg = ao_settings.expect("ao_on 蕴含资源在");
            let ao_target = self.ao.target.as_ref().expect("ao_on 时可见度图应已创建");
            self.gpu.queue.write_buffer(
                &self.ao.params_buf,
                0,
                bytemuck::bytes_of(&AoParamsRaw {
                    // 半径/强度钳非负（与 SSR 参数的钳制纪律一致）；tan/
                    // aspect 是重建公式的投影常量，z_far 是天空标记
                    scalars: [
                        cfg.radius.max(1e-3),
                        cfg.intensity.max(0.0),
                        (fov * 0.5).tan(),
                        width as f32 / height.max(1) as f32,
                    ],
                    extra: [z_far, 0.0, 0.0, 0.0],
                }),
            );
            let mut ao_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iris.pass.ao"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &ao_target.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // 中性可见度 1.0：无效帧整帧保持此值（无绘制），
                        // 有效帧被全屏三角形覆写（Clear 只是兜底）
                        load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if ao_active {
                // 深度副本有效 ⇒ scene_depth 纹理必在（ao_depth_frame 只
                // 在副本编码处写入）
                let sd = self
                    .scene_depth
                    .as_ref()
                    .expect("AO 有效帧必有场景深度副本");
                let bind_group = self
                    .gpu
                    .device
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("iris.ao.bg"),
                        layout: &self.bundle.ao_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: self.ao.params_buf.as_entire_binding(),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(&sd.view),
                            },
                        ],
                    });
                ao_pass.set_pipeline(&self.bundle.ao_pipeline);
                ao_pass.set_bind_group(0, &bind_group, &[]);
                ao_pass.draw(0..3, 0..1);
            }
            self.stats.ao_on = if ao_active { 1 } else { 0 };
        }
        // 主 pass：场景渲染到 HDR MSAA 中间缓冲，解析到单采样 HDR
        let hdr = self.hdr.as_ref().expect("HDR 中间缓冲已就绪");
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iris.pass.main"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &hdr.msaa_view,
                    depth_slice: None,
                    resolve_target: Some(&hdr.resolved_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        // 深度必须 Store：主 pass 结束后透明 pass 以 LoadOp::Load
                        // 读取同一深度附件做遮挡测试（透明被不透明几何挡住仍不可见）。
                        // Discard 会让附件内容变未定义（DX12 实测为 0，Less 全挂，
                        // 透明整体消失）——该教训固化在 ARCHITECTURE §4.16。
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // 天空盒背景最先绘制（深度只读，几何体随后覆盖）。R79 起
            // 用专用 sky 绑定组（sky_layout：Globals + env + SkyParams），
            // 随后几何切回 globals 组——mode = 0 时采样行为与旧 globals
            // 组逐位一致。
            pass.set_pipeline(&self.bundle.sky_pipeline);
            pass.set_bind_group(0, &self.sky_bg, &[]);
            pass.draw(0..3, 0..1);
            pass.set_pipeline(&self.bundle.pipeline);
            pass.set_bind_group(0, &self.globals_bg, &[]);
            if instance_total > 0 {
                pass.set_vertex_buffer(
                    1,
                    self.instance_buf
                        .as_ref()
                        .expect("实例缓冲已就绪")
                        .slice(..),
                );
            }
            let occ_drawn_ref = occ_filtered.then_some(occ_drawn.as_slice());
            index = 0;
            while index < total {
                let key = items[index].key;
                let lightmap = items[index].lightmap;
                // 段划分与懒上传同源（run_length）：同键同图集才并成一段
                let count = run_length(&items, index);
                let mesh = &self.mesh_gpu[&((key >> 32) as u32)];
                let material = &self.material_gpu[&material_key(key as u32, lightmap)];
                // 双面材质（R66d）选 cull None 变体；同批恒同值（标志是
                // 材质句柄的函数），状态切换只发生在批边界
                pass.set_pipeline(if items[index].double_sided {
                    &self.bundle.double_sided_pipeline
                } else {
                    &self.bundle.pipeline
                });
                pass.set_bind_group(1, &material.bind_group, &[]);
                pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
                // 切线缓冲（R42，槽 2）：缺席网格绑常驻零垫
                pass.set_vertex_buffer(2, self.tangent_buf(mesh).slice(..));
                // 光照贴图 UV2（R52c，槽 3）：缺席网格同样绑零垫
                pass.set_vertex_buffer(3, self.uv2_buf(mesh).slice(..));
                pass.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                // 子段遍历（R53）：遮挡关闭 = 恒整段一次 draw，命令流与
                // 引入前逐位一致；开启 = 只提交「未被遮挡」的连续子段
                // （实例区间天然全局连续，子段直接可用）。主 pass 三角形
                // 统计：网格索引数 / 3 × 子段实例数；索引数取 CPU 目录
                //（网格缺席按 0 处理，不 panic——该路径理论上不可达，
                // 缺席网格在懒上传阶段已告警跳过）
                let tris_per_instance = mesh_store
                    .get(MeshHandle((key >> 32) as u32))
                    .map_or(0, |data| data.indices.len() as u32 / 3);
                let mut sub = 0usize;
                while sub < count {
                    if !occ_drawn_ref.is_none_or(|d| d[index + sub]) {
                        sub += 1;
                        continue;
                    }
                    let mut end = sub + 1;
                    while end < count && occ_drawn_ref.is_none_or(|d| d[index + end]) {
                        end += 1;
                    }
                    let first = (index + sub) as u32;
                    let last = (index + end) as u32;
                    pass.draw_indexed(0..mesh.num_indices, 0, first..last);
                    draw_calls += 1;
                    let run = (end - sub) as u32;
                    instances += run;
                    triangles_drawn += tris_per_instance * run;
                    sub = end;
                }
                index += count;
            }
            // 蒙皮实体：独立管线（组 2 = 关节矩阵 storage），实例区间紧随普通实例
            if !skinned.is_empty() {
                let instance_buf = self.instance_buf.as_ref().expect("实例缓冲已就绪");
                pass.set_pipeline(&self.bundle.skinned_pipeline);
                pass.set_bind_group(0, &self.globals_bg, &[]);
                pass.set_vertex_buffer(2, instance_buf.slice(..));
                for (i, item) in skinned.iter().enumerate() {
                    let mesh = &self.mesh_gpu[&item.mesh];
                    let material = &self.material_gpu[&material_key(item.material, item.lightmap)];
                    let skin = &self.skin_gpu[&item.skin_entity];
                    pass.set_pipeline(if item.double_sided {
                        &self.bundle.double_sided_skinned_pipeline
                    } else {
                        &self.bundle.skinned_pipeline
                    });
                    pass.set_bind_group(1, &material.bind_group, &[]);
                    pass.set_bind_group(2, &skin.bind_group, &[]);
                    pass.set_vertex_buffer(0, item.vertex_buf(mesh).slice(..));
                    // 切线缓冲（R42，槽 3，蒙皮布局）：缺席绑零垫
                    pass.set_vertex_buffer(3, self.tangent_buf(mesh).slice(..));
                    // UV2（R52c，槽 4，蒙皮布局）：缺席绑零垫
                    pass.set_vertex_buffer(4, self.uv2_buf(mesh).slice(..));
                    pass.set_vertex_buffer(
                        1,
                        mesh.skin_buf
                            .as_ref()
                            .expect("蒙皮网格应有蒙皮属性缓冲")
                            .slice(..),
                    );
                    pass.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    let base = (total + i) as u32;
                    pass.draw_indexed(0..mesh.num_indices, 0, base..base + 1);
                    draw_calls += 1;
                    instances += 1;
                    // 蒙皮逐项累加：索引数 / 3（网格缺席按 0，同普通批注释）
                    triangles_drawn += mesh_store
                        .get(MeshHandle(item.mesh))
                        .map_or(0, |data| data.indices.len() as u32 / 3);
                }
            }
            // meshlet 几何（R85，虚拟几何体 V2）：主 pass 内、常规不透明段
            // 之后——不透明几何 z-buffer 裁决与顺序无关（Less + REPLACE，
            // 无混合），对既有像素流零交互；透明 / 贴花 / 粒子照常后叠。
            // vertex-pull：实例 = 紧凑簇列表条目，无顶点 / 索引缓冲；片元
            // = standard.wgsl fs_main 本体（跨模块管线阶段，PBR 零复制）。
            // 缺席 = 零绘制（命令流与引入前逐位一致）。
            if let Some(plan) = &meshlet_plan {
                if plan.draw_count > 0 {
                    let material = &self.material_gpu[&material_key(0, NO_LIGHTMAP)];
                    let mg = self.meshlet_gpu.as_ref().expect("meshlet GPU 资源已就绪");
                    pass.set_pipeline(&self.bundle.meshlet_pipeline);
                    pass.set_bind_group(0, &self.globals_bg, &[]);
                    pass.set_bind_group(1, &material.bind_group, &[]);
                    pass.set_bind_group(2, &mg.draw_bgs[plan.draw_parity], &[]);
                    // 固定顶点数契约：每实例 128 三角 × 3 顶点，越界槽位在
                    // 顶点着色器输出零面积退化三角（meshlet_draw.wgsl）
                    pass.draw(
                        0..crate::meshlet::MESHLET_CLUSTER_MAX_TRIANGLES as u32 * 3,
                        0..plan.draw_count,
                    );
                    draw_calls += 1;
                    instances += plan.draw_count;
                    triangles_drawn += plan.triangles;
                    meshlet_surviving = plan.draw_count;
                }
            }
        }
        // 4e'. meshlet compute 剔除 + 稳定紧凑化（R85）：主 pass **之后**
        //      编码——本帧 draw 消费的紧凑列表是上一帧的产物（读回一帧
        //      延迟契约），本帧 compute 对 compact_buf 的覆写必须发生在
        //      draw 之后（同一编码器按序执行）。读回在途 = staging 槽不
        //      可覆写 ⇒ 整段跳过（下一帧继续消费 / 兜底）。cull（逐簇
        //      视锥 + 背面锥，无原子）→ compact（单工作组 u32 前缀和，
        //      紧凑序 = 全局簇号升序 ⇒ 绘制序确定），随后把存活总数 +
        //      紧凑列表 copy 进 staging 双缓冲槽，提交后 map_async。
        let mut meshlet_issued: Option<usize> = None;
        if let Some(plan) = &meshlet_plan {
            if plan.do_compute && self.meshlet_inflight.is_none() {
                let mg = self.meshlet_gpu.as_ref().expect("meshlet GPU 资源已就绪");
                {
                    let mut cpass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("iris.pass.meshlet_cull"),
                        timestamp_writes: None,
                    });
                    cpass.set_pipeline(&self.bundle.meshlet_cull_pipeline);
                    cpass.set_bind_group(0, &mg.cull_bgs[plan.cull_parity], &[]);
                    // 恒定形状 dispatch：x = ceil(实例最大簇数 / 64)，
                    // y = 实例数——越界线程早退（R82 范式）
                    cpass.dispatch_workgroups(plan.cull_blocks, plan.dispatch_instances, 1);
                    cpass.set_pipeline(&self.bundle.meshlet_compact_pipeline);
                    cpass.set_bind_group(0, &mg.cull_bgs[plan.cull_parity], &[]);
                    cpass.dispatch_workgroups(1, 1, 1);
                }
                let parity = plan.cull_parity;
                encoder.copy_buffer_to_buffer(&mg.count_buf, 0, &mg.staging[parity], 0, 4);
                encoder.copy_buffer_to_buffer(
                    &mg.compact_buf,
                    0,
                    &mg.staging[parity],
                    4,
                    (crate::meshlet::MAX_MESHLET_CLUSTERS * 4) as u64,
                );
                meshlet_issued = Some(parity);
            }
        }
        // 5a. 遮挡查询 pass（R53，紧随主 pass）：复用本帧不透明深度
        //（Load 不写、Store 原样——其后的贴花/透明 pass 仍要 Load 同一
        // 附件），逐实例画世界 AABB 盒领用查询槽（一槽一 draw，限制③）。
        // 结果 resolve → copy 进双缓冲、本帧提交后 map_async，下一帧
        // draw 开头消费（延迟一帧）。读回在途时整段跳过（缓冲不可覆写、
        // 帧无新结果 = 不剔除，全部可见）。
        let mut occ_issued: Option<(usize, Vec<u64>, usize)> = None;
        if occlusion_on && !occ_aabbs.is_empty() && self.occ_inflight.is_none() {
            let n = occ_aabbs.len();
            let occ = self.occ_gpu.as_ref().expect("查询资源已在收集段创建");
            self.gpu.queue.write_buffer(
                &occ.params_buf,
                0,
                bytemuck::bytes_of(&CascadeParamsRaw {
                    view_proj: vp.to_cols_array_2d(),
                }),
            );
            let aabb_buf = occ.aabb_buf.as_ref().expect("AABB 缓冲容量已保证");
            self.gpu
                .queue
                .write_buffer(aabb_buf, 0, bytemuck::cast_slice(&occ_aabbs));
            {
                let mut qpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("iris.pass.occlusion"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: depth_view,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            // 深度内容未变，Store 是严格定义的零成本透传
                            //（Discard 会把附件置未定义，毁掉后续透明 pass）
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: Some(&occ.query_set),
                    multiview_mask: None,
                });
                qpass.set_pipeline(&self.bundle.occlusion_pipeline);
                qpass.set_bind_group(0, &occ.params_bg, &[]);
                qpass.set_vertex_buffer(0, occ.corner_buf.slice(..));
                qpass.set_vertex_buffer(
                    1,
                    aabb_buf.slice(0..(n * std::mem::size_of::<AabbInstanceRaw>()) as u64),
                );
                qpass.set_index_buffer(occ.index_buf.slice(..), wgpu::IndexFormat::Uint16);
                for q in 0..n as u32 {
                    qpass.begin_occlusion_query(q);
                    qpass.draw_indexed(0..AABB_INDEX_COUNT, 0, q..q + 1);
                    qpass.end_occlusion_query();
                }
            }
            let parity = (self.occ_frame % 2) as usize;
            encoder.resolve_query_set(&occ.query_set, 0..n as u32, &occ.resolve_bufs[parity], 0);
            encoder.copy_buffer_to_buffer(
                &occ.resolve_bufs[parity],
                0,
                &occ.readback_bufs[parity],
                0,
                (n * std::mem::size_of::<u64>()) as u64,
            );
            occ_issued = Some((parity, std::mem::take(&mut occ_entities), n));
        }
        // 贴花 pass（R39）：主 pass（天空 + 不透明 + 蒙皮）之后、透明 pass
        // 之前——贴花是**不透明表面的装饰**，属于场景「实体」的一部分；
        // 玻璃等透明物应盖在贴花的**混合结果**之上（透过玻璃看到的墙面
        // 弹孔带玻璃色），「后画者在上」由 pass 顺序统一裁决（与 R23 透明、
        // R24 粒子同构；透明 / 粒子在贴花之后依次叠加）。
        // - 颜色：同一 HDR MSAA 目标 LoadOp::Load 继续混合，pass 结束再
        //   resolve（= 主 pass 结果 + 贴花混合，透明 pass 将在此基础上继续）。
        // - 深度附件 Load + **Store**（非透明 / 粒子 pass 的 Discard）：贴花
        //   管线深度只测不写、本 pass 不改深度，但其后的透明 pass 仍要 Load
        //   同一深度附件做遮挡测试——Discard 会把附件内容置为未定义（R23
        //   的 DX12 教训），Store 未变更内容是严格定义的零成本透传。
        // - 管线：alpha 混合 + LessEqual 只测不写（端盖与被贴表面共面，
        //   见 pipeline.rs 贴花管线注释）。
        // - 按纹理分批：排序后同纹理实例区间连续，每批 1 draw（组 1 切纹理
        //   绑定组）。无贴花实例时整个 pass 跳过（零开销）。
        if decal_total > 0 {
            let decal_buf = self
                .decal_instance_buf
                .as_ref()
                .expect("贴花实例缓冲已就绪");
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iris.pass.decal"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &hdr.msaa_view,
                    depth_slice: None,
                    resolve_target: Some(&hdr.resolved_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.bundle.decal_pipeline);
            pass.set_bind_group(0, &self.globals_bg, &[]);
            pass.set_vertex_buffer(0, self.decal_vertex_buf.slice(..));
            pass.set_index_buffer(self.decal_index_buf.slice(..), wgpu::IndexFormat::Uint32);
            pass.set_vertex_buffer(1, decal_buf.slice(..));
            let mut d = 0usize;
            while d < decal_total {
                let texture = decal_items[d].texture;
                let mut count = 1;
                while d + count < decal_total && decal_items[d + count].texture == texture {
                    count += 1;
                }
                let bind_group = self
                    .decal_bind_groups
                    .get(&texture)
                    .expect("贴花纹理绑定组应在 4d 阶段构建");
                pass.set_bind_group(1, bind_group, &[]);
                pass.draw_indexed(0..PROXY_INDEX_COUNT as u32, 0, d as u32..(d + count) as u32);
                draw_calls += 1;
                instances += count as u32;
                // 每实例 4 三角形（±Z 两块端盖）
                triangles_drawn += 4 * count as u32;
                d += count;
            }
        }
        // 透明 pass：主 pass（天空 + 不透明 + 蒙皮）与贴花 pass 之后、
        // 泛光 / 后处理之前，
        // 写同一 HDR MSAA 颜色目标——LoadOp::Load 载入主 pass（含贴花
        // 混合）结果继续混合，
        // pass 结束的 resolve 与主 pass 同一目标（MSAA 内容保留完整，再次
        // resolve 的结果 = 主 pass 结果 + 透明混合）。深度附件同样 Load：
        // 保留不透明几何的深度做遮挡测试（透明被墙挡住仍不可见），
        // 管线侧深度只测不写（见 pipeline.rs 透明管线注释）。
        // 无透明实例时整个 pass 跳过（零开销）。
        if transparent_total + transparent_skinned.len() > 0 {
            let instance_buf = self.instance_buf.as_ref().expect("实例缓冲已就绪");
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iris.pass.transparent"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &hdr.msaa_view,
                    depth_slice: None,
                    resolve_target: Some(&hdr.resolved_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.bundle.transparent_pipeline);
            pass.set_bind_group(0, &self.globals_bg, &[]);
            pass.set_vertex_buffer(1, instance_buf.slice(..));
            // 透明普通项：深度序中相邻同 (网格, 材质) 键合并为一个 run 批
            // （实例按深度序连续存放于区间 [opaque_total, +transparent_total)）
            let base = opaque_total as u32;
            let mut t = 0usize;
            while t < transparent_total {
                let key = transparent[t].key;
                let mut count = 1;
                while t + count < transparent_total && transparent[t + count].key == key {
                    count += 1;
                }
                let mesh = &self.mesh_gpu[&((key >> 32) as u32)];
                // 透明普通项不采样光照贴图（v1 限制，见 lightmap.rs）⇒
                // 图集槽位恒 NO_LIGHTMAP，与 4b 的懒上传键一致
                let material = &self.material_gpu[&material_key(key as u32, NO_LIGHTMAP)];
                // 双面透明（R66d）：发片类资产两朝向都参与 alpha 混合
                pass.set_pipeline(if transparent[t].double_sided {
                    &self.bundle.transparent_double_sided_pipeline
                } else {
                    &self.bundle.transparent_pipeline
                });
                pass.set_bind_group(1, &material.bind_group, &[]);
                pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
                // 切线缓冲（R42，槽 2）：缺席网格绑常驻零垫
                pass.set_vertex_buffer(2, self.tangent_buf(mesh).slice(..));
                // UV2（R52c，槽 3）：缺席绑零垫（掩码 0 ⇒ 采样结果被丢弃）
                pass.set_vertex_buffer(3, self.uv2_buf(mesh).slice(..));
                pass.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(
                    0..mesh.num_indices,
                    0,
                    base + t as u32..base + (t + count) as u32,
                );
                draw_calls += 1;
                instances += count as u32;
                // 三角形统计与不透明普通批同口径：索引数 / 3 × run 实例数
                triangles_drawn += mesh_store
                    .get(MeshHandle((key >> 32) as u32))
                    .map_or(0, |data| data.indices.len() as u32 / 3)
                    * count as u32;
                t += count;
            }
            // 透明蒙皮：透明管线 + 组 2 关节矩阵，每实体一次 draw，
            // 实例区间 [opaque_total + transparent_total, +len)
            if !transparent_skinned.is_empty() {
                pass.set_pipeline(&self.bundle.transparent_skinned_pipeline);
                pass.set_bind_group(0, &self.globals_bg, &[]);
                pass.set_vertex_buffer(2, instance_buf.slice(..));
                for (i, t) in transparent_skinned.iter().enumerate() {
                    let item = &t.skinned;
                    let mesh = &self.mesh_gpu[&item.mesh];
                    // 蒙皮路径（含透明蒙皮）解析出的图集照用：每实体一次
                    // draw，不存在分批冲突（与 4a 的懒上传键一致）
                    let material = &self.material_gpu[&material_key(item.material, item.lightmap)];
                    let skin = &self.skin_gpu[&item.skin_entity];
                    pass.set_pipeline(if item.double_sided {
                        &self.bundle.transparent_double_sided_skinned_pipeline
                    } else {
                        &self.bundle.transparent_skinned_pipeline
                    });
                    pass.set_bind_group(1, &material.bind_group, &[]);
                    pass.set_bind_group(2, &skin.bind_group, &[]);
                    pass.set_vertex_buffer(0, item.vertex_buf(mesh).slice(..));
                    // 切线缓冲（R42，槽 3，蒙皮布局）：缺席绑零垫
                    pass.set_vertex_buffer(3, self.tangent_buf(mesh).slice(..));
                    // UV2（R52c，槽 4，蒙皮布局）：缺席绑零垫
                    pass.set_vertex_buffer(4, self.uv2_buf(mesh).slice(..));
                    pass.set_vertex_buffer(
                        1,
                        mesh.skin_buf
                            .as_ref()
                            .expect("蒙皮网格应有蒙皮属性缓冲")
                            .slice(..),
                    );
                    pass.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    let start = (opaque_total + transparent_total + i) as u32;
                    pass.draw_indexed(0..mesh.num_indices, 0, start..start + 1);
                    draw_calls += 1;
                    instances += 1;
                    triangles_drawn += mesh_store
                        .get(MeshHandle(item.mesh))
                        .map_or(0, |data| data.indices.len() as u32 / 3);
                }
            }
        }
        // 粒子 pass：透明 pass 之后、泛光之前——
        // 1) 顺序理由：粒子叠加在玻璃等透明几何的**混合结果**之上（火焰 /
        //    火花应浮在玻璃前面时的合成结果上），与 R23 透明同处 HDR 链路
        //    按「后画者在上」由 pass 顺序统一裁决；同时在泛光之前，高亮
        //    additive 粒子（HDR 亮度 > 1）参与亮度提取，与自发光几何同权。
        // 2) 写同一 HDR MSAA 目标（LoadOp::Load 继续混合）；深度附件同样
        //    Load——被不透明几何挡住的粒子照常被遮挡（管线侧深度只测不写）。
        // 3) additive 先画（顺序无关、不排序——加法交换律），alpha 后画
        //    （视深降序整段一次 draw）。无粒子时整个 pass 跳过（零开销）。
        //    实例已在 4c 全量上传，这里只绑定与绘制。
        // 4) 粒子 v2（R82）在同一 pass、v1 之后绘制——pass 位置照旧不动
        //    （透明之后、泛光之前），R81 水体（更后）不受影响；逐发射器
        //    一次 draw（固定顶点数 = 池 × trail_length × 6，死粒子 /
        //    历史不足在顶点着色器零面积退化）。仅 v2 发射器在场时同样
        //    打开本 pass。
        let particle_v2_drawable = particle_v2_items.iter().any(|i| i.visible && i.alive > 0);
        if particle_total > 0 || particle_v2_drawable {
            // v1 实例缓冲仅在 v1 粒子在场时解包（仅 v2 场景下不分配）
            let particle_buf = self.particle_buf.as_ref();
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iris.pass.particle"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &hdr.msaa_view,
                    depth_slice: None,
                    resolve_target: Some(&hdr.resolved_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if particle_total > 0 {
                pass.set_bind_group(0, &self.globals_bg, &[]);
                pass.set_bind_group(1, &self.particle_camera_bg, &[]);
                pass.set_vertex_buffer(
                    0,
                    particle_buf.expect("v1 粒子在场时实例缓冲已就绪").slice(..),
                );
                if particle_additive_len > 0 {
                    pass.set_pipeline(&self.bundle.particle_additive_pipeline);
                    pass.draw(0..4, 0..particle_additive_len as u32);
                    draw_calls += 1;
                }
                if particle_instances.len() > particle_additive_len {
                    pass.set_pipeline(&self.bundle.particle_alpha_pipeline);
                    pass.draw(
                        0..4,
                        particle_additive_len as u32..particle_instances.len() as u32,
                    );
                    draw_calls += 1;
                }
                instances += particle_total;
                // 每粒一个 quad = 2 三角形
                triangles_drawn += 2 * particle_total;
                particles_drawn = particle_total;
            }
            // 粒子 v2（R82）：逐发射器一次 draw（收集序 = 查询序，确定
            // 性）；additive / alpha 按各自混合管线。拖尾渲染 uniform 每
            // 帧现场上传（含相机投影参数）。
            for item in &particle_v2_items {
                if !item.visible || item.alive == 0 {
                    continue;
                }
                let pool = &self.particle_v2_pools[item.pool];
                self.gpu.queue.write_buffer(
                    &pool.draw_params_buf,
                    0,
                    bytemuck::bytes_of(&item.draw),
                );
                pass.set_bind_group(0, &self.globals_bg, &[]);
                pass.set_bind_group(1, &pool.draw_bg, &[]);
                pass.set_pipeline(match item.blend {
                    ParticleBlend::Additive => &self.bundle.particle_v2_additive_pipeline,
                    ParticleBlend::Alpha => &self.bundle.particle_v2_alpha_pipeline,
                });
                pass.draw(
                    0..crate::particles_v2::PARTICLES_V2_MAX * item.trail_length * 6,
                    0..1,
                );
                draw_calls += 1;
                instances += item.alive;
                triangles_drawn += item.alive * item.trail_length * 2;
                particles_drawn += item.alive;
            }
        }
        // 主相机场景深度副本 pass（R43）：仅 DOF / MB / TAA / SSR 启用时
        // 编码。把不透明 + 蒙皮实例从相机视角重画一遍，R32Float 颜色附件
        // 存线性视距（clip.w，见 scene_depth.wgsl 的插值推导），自带
        // Depth32Float 附件做最近面裁决——阴影的「深度写颜色 + 双附件」
        // 既有方案（主 pass 的深度附件是 MSAA 且无 TEXTURE_BINDING，
        // 不可直接采样，DX12 下 depth-only 采样另有写入异常的教训）。
        // 透明 / 贴花 / 粒子不进副本（半透明深度无定义，ARCHITECTURE §8）。
        // TAA（R60）与 DOF/MB 共用同一份副本与本帧同一抖动矩阵。
        // SSR（R62b）同样消费它：本 pass 编码在主 pass **之后**，主 pass
        // 的 SSR 步进读到的恒为上一帧内容（同编码器按序执行，§4.31 裁决
        // 2 的「不 ping-pong」偏离）；SSR 开启但本帧无几何 ⇒ 副本没跑，
        // 「上帧深度」作废（ssr_prev_valid 记账，下帧跳过）。深度着色只
        // 用顶点变换，SSR 是否活跃不影响副本内容。
        // GTAO（R77）是副本的第四个消费者（主 pass **之前**读上一帧）：
        // `ao_depth_frame` 记录本帧副本已跑——下一帧编号恰好 +1 才允许
        // 编码 AO pass（首帧启用 / 上帧无几何的帧自然落进中性路径）。
        // 体积雾（R79）是第五个消费者（链头读深度——链上同帧：副本在
        // 雾 pass 之前编码，读到本帧内容；无记账门）。
        // 平面水体（R81）是第六个消费者（同雾的链上同帧：水体 pass 编码
        // 在本 pass 之后，读到的恒为本帧深度——水下厚度 =
        // scene_depth − 水面片元视距）。
        if (dof_on || mb_on || taa_on || ssr_on || ao_on || fog_on || water_on) && opaque_total > 0
        {
            self.ao_depth_frame = Some(self.occ_frame);
            let sd = self
                .scene_depth
                .as_ref()
                .expect("DOF/MB 开启时应已创建深度副本");
            self.gpu.queue.write_buffer(
                &sd.params,
                0,
                bytemuck::bytes_of(&CascadeParamsRaw {
                    view_proj: vp.to_cols_array_2d(),
                }),
            );
            let instance_buf = self.instance_buf.as_ref().expect("实例缓冲已就绪");
            let mut depth_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iris.pass.scene_depth"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &sd.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // 天空 / 无几何像素 = 远平面视距（DOF 视为最远
                        // 模糊带；MB 反投影到远平面点）
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: z_far as f64,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &sd.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // 普通不透明批（区间 [0, total)）：与阴影 pass 同款逐批 draw
            depth_pass.set_pipeline(&self.bundle.scene_depth_pipeline);
            depth_pass.set_bind_group(0, &sd.bind_group, &[]);
            depth_pass.set_vertex_buffer(1, instance_buf.slice(..));
            let mut d = 0usize;
            while d < total {
                let key = items[d].key;
                let mut count = 1;
                while d + count < total && items[d + count].key == key {
                    count += 1;
                }
                let mesh = &self.mesh_gpu[&((key >> 32) as u32)];
                depth_pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
                depth_pass.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                depth_pass.draw_indexed(0..mesh.num_indices, 0, d as u32..(d + count) as u32);
                d += count;
            }
            // 蒙皮：深度随骨骼变形（组 1 关节矩阵，与蒙皮阴影管线同构）
            if !skinned.is_empty() {
                depth_pass.set_pipeline(&self.bundle.scene_depth_skinned_pipeline);
                depth_pass.set_vertex_buffer(2, instance_buf.slice(..));
                for (i, item) in skinned.iter().enumerate() {
                    let mesh = &self.mesh_gpu[&item.mesh];
                    let skin = &self.skin_gpu[&item.skin_entity];
                    depth_pass.set_vertex_buffer(0, item.vertex_buf(mesh).slice(..));
                    depth_pass.set_vertex_buffer(
                        1,
                        mesh.skin_buf
                            .as_ref()
                            .expect("蒙皮网格应有蒙皮属性缓冲")
                            .slice(..),
                    );
                    depth_pass
                        .set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    depth_pass.set_bind_group(1, &skin.bind_group, &[]);
                    let base = (total + i) as u32;
                    depth_pass.draw_indexed(0..mesh.num_indices, 0, base..base + 1);
                }
            }
        }
        // 平面水体 pass（R81）：scene_depth 之后（深度吸收读本帧深度——
        // 第六消费者，链上同帧）、TAA / 后处理链之前（HDR 线性域合成——
        // 水面参与泛光与色调映射，与场景内容同权）。同一 HDR MSAA 目标
        // Load 继续 + resolve，深度附件 Load 不写（水体不写主深度，与
        // 「水像素不写 scene_depth」同语义——下一帧雾 / AO 对水面像素用
        // 其后地面深度，近似入档 §8.50）。无水体帧整段跳过（零开销）。
        if let Some(ws) = water_selected {
            let water = self.water.as_ref().expect("water_on 时水体资源应已创建");
            // 波形时间 = 世界 Time 资源的 elapsed_secs（缺席 = 0，波形静止
            // 在初始相位——不引入渲染器内部时钟，测试可精确驱动）
            let wave_t = world
                .try_resource::<iris_core::time::Time>()
                .map(|t| t.elapsed_secs())
                .unwrap_or(0.0);
            let light_dir_w = (-light.direction).normalize_or_zero();
            self.gpu.queue.write_buffer(
                &water.params_buf,
                0,
                bytemuck::bytes_of(&WaterParamsRaw {
                    view_proj: vp.to_cols_array_2d(),
                    refl_view_proj: water_refl_vp.to_cols_array_2d(),
                    eye: [eye.x, eye.y, eye.z, 0.0],
                    light_dir: [light_dir_w.x, light_dir_w.y, light_dir_w.z, 0.0],
                    light_color: [
                        light.color.x * light.intensity,
                        light.color.y * light.intensity,
                        light.color.z * light.intensity,
                        0.0,
                    ],
                    wave_a: [
                        ws.wave_amplitude,
                        ws.wave_length,
                        ws.wave_speed,
                        ws.choppiness,
                    ],
                    wave_b: [
                        ws.half_extent.x,
                        ws.half_extent.y,
                        wave_t,
                        ws.absorb_depth.max(1e-3),
                    ],
                    shallow: [
                        ws.shallow_color.x,
                        ws.shallow_color.y,
                        ws.shallow_color.z,
                        0.0,
                    ],
                    deep: [ws.deep_color.x, ws.deep_color.y, ws.deep_color.z, 0.0],
                }),
            );
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iris.pass.water"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &hdr.msaa_view,
                    depth_slice: None,
                    resolve_target: Some(&hdr.resolved_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        // 水体之后无几何 pass 消费主深度（后处理链读
                        // scene_depth），Discard 与透明 / 粒子 pass 同款
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.bundle.water_pipeline);
            pass.set_bind_group(0, &water.bind_group, &[]);
            pass.set_vertex_buffer(0, water.grid_vertex_buf.slice(..));
            pass.set_index_buffer(water.grid_index_buf.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..water.grid_index_count, 0, 0..1);
            draw_calls += 1;
            triangles_drawn += water.grid_index_count / 3;
        }
        // TAA pass（R60，链首）：抖动色 + 深度副本 + 上帧历史 → 3×3
        // 邻域钳制混合 → 写入本帧历史槽（ping-pong 半边按 TAA 帧号奇偶
        // 交替）。下游链经 `PostInput::TaaHistory` 直接读本帧输出（链源
        // 切换由 chain_source 单点定义）。无几何帧（无深度副本支撑重
        // 投影）整帧退化为透传：置重置标志，输出 = 当前帧。重置帧
        // （首帧启用 / 切换 / 分辨率变化 / 显式 reset_taa）同样透传。
        if taa_on {
            if opaque_total == 0 {
                taa_reset_frame = true;
            }
            let taa_t = hdr.taa.as_ref().expect("TAA 开启时应已创建历史缓冲");
            let parity = (taa_index % 2) as usize;
            // 深度 → NDC z 的两个预计算系数（与 MB 同款，DirectX [0,1] 约定）
            let z_den = (z_far - z_near).max(1e-6);
            self.gpu.queue.write_buffer(
                &taa_t.params,
                0,
                bytemuck::bytes_of(&TaaParamsRaw {
                    inv_view_proj: vp.inverse().to_cols_array_2d(),
                    prev_view_proj: prev_vp.to_cols_array_2d(),
                    scalars: [
                        TAA_HISTORY_WEIGHT,
                        if taa_reset_frame { 1.0 } else { 0.0 },
                        z_far / z_den,
                        z_near * z_far / z_den,
                    ],
                }),
            );
            let depth_view = &self
                .scene_depth
                .as_ref()
                .expect("TAA 开启时应已创建深度副本")
                .view;
            crate::post::encode_taa_pass(
                &mut encoder,
                &self.gpu.device,
                crate::post::TaaPassDesc {
                    label: "iris.pass.taa",
                    pipeline: &self.bundle.taa_pipeline,
                    layout: &self.bundle.taa_layout,
                    params: &taa_t.params,
                    sampler: &taa_t.sampler,
                    src: &hdr.resolved_view,
                    depth: depth_view,
                    history: &taa_t.hist_views[1 - parity],
                    dst: &taa_t.hist_views[parity],
                },
            );
        }
        // 本帧 TAA 输出视图（下游链 PostInput::TaaHistory 的解析目标）；
        // 关闭恒 None（该输入变体不应出现在任何 pass 描述里）。
        let taa_curr_view: Option<&wgpu::TextureView> = if taa_on {
            Some(
                &hdr.taa
                    .as_ref()
                    .expect("TAA 开启时应已创建历史缓冲")
                    .hist_views[(taa_index % 2) as usize],
            )
        } else {
            None
        };
        // ---- 后处理链（ARCHITECTURE §4.11）：TAA（可选，链首）→ 景深
        // （可选）→ 运动模糊
        // （可选）→ 泛光（半分辨率，可选）→ 暗角（全分辨率，强度 > 0 才
        // 入链）→ blit 色调映射（链的固定终点）。pass 编码统一走 post.rs
        // 的 encode_post_pass（描述 → 编码）。资源缺席语义：
        // BloomSettings 缺席 = 默认启用；ExposureSettings 缺席 = 1.0 中性；
        // VignetteSettings / DepthOfFieldSettings / MotionBlurSettings
        // 缺席 = 关闭（enabled = false 等价）。
        let bloom = world
            .try_resource::<crate::post::BloomSettings>()
            .map(|b| *b)
            .unwrap_or_default();
        let exposure = world
            .try_resource::<crate::post::ExposureSettings>()
            .map(|e| e.exposure.max(0.0))
            .unwrap_or(1.0);
        let bloom_strength = if bloom.enabled {
            bloom.strength.max(0.0)
        } else {
            0.0
        };
        // 全链共享合成参数：blit 消费 x（泛光）/ y（曝光），暗角 pass 消费
        // z。曝光恒写——1.0 的乘法逐位中性（资源缺席画面不变的保证）。
        self.gpu.queue.write_buffer(
            &hdr.post_params,
            0,
            bytemuck::bytes_of(&PostParamsRaw {
                bloom_strength,
                exposure,
                vignette_strength,
                _reserved: 0.0,
            }),
        );
        // 全分辨率链（雾 / DOF / MB / 暗角）的槽位分配：纯函数单点定义
        // （post.rs，离线单测守护拓扑不变量）。TAA 开启不改变槽位拓扑，
        // 只把「主解析缓冲」这一源整体替换为本帧 TAA 输出（chain_source
        // 单点定义：有链头槽位取槽位，无链头时 TAA 开 = TAA 输出、关 =
        // 主解析缓冲——R59 拓扑原样）。雾开启时相机级链 A→B→A 轮转（
        // full_chain_slots_with_fog 单点定义），雾关时与旧函数逐字段
        // 相同（命令流与 R78 逐位一致）。
        let chain_slots =
            crate::post::full_chain_slots_with_fog(dof_on, mb_on, vignette_strength > 0.0, fog_on);
        let slots = chain_slots.slots;
        let fog_dst = chain_slots.fog_dst;
        let scene_or_taa =
            |slot: Option<crate::post::PingPongSlot>| crate::post::chain_source(slot, taa_on);
        // 链头（泛光与暗角的源）：雾/DOF/MB 输出槽、TAA 输出或主解析缓冲
        let head_input = scene_or_taa(slots.head);
        // 体积雾（R79，链头）：链源（主解析缓冲 / TAA 输出）→ 全分辨率
        // A 槽。逐像素 16 步光线步进（发射-吸收积分），深度 = 本帧副本
        // （链上同帧——见 fog.rs 模块文档的偏离声明；天空像素射线长 =
        // max_distance）。位于 DOF / MB / 泛光之前——雾是场景内容的大气
        // 调制，理应参与焦外 / 拖影 / 亮度提取（与「相机级效果在 HDR
        // 线性域」的既有链序同理）。
        if let Some(fog_slot) = fog_dst {
            let fog = fog_settings.expect("雾开启时设置必已读取");
            // 深度 → NDC z 的两个预计算系数（与 MB / TAA 同款，DirectX
            // [0,1] 约定）：z_ndc = f/(f−n) − (n·f/(f−n))/d
            let z_den = (z_far - z_near).max(1e-6);
            let sun_dir = (-light.direction).normalize_or_zero();
            self.gpu.queue.write_buffer(
                &hdr.fog_params,
                0,
                bytemuck::bytes_of(&FogParamsRaw {
                    inv_view_proj: vp.inverse().to_cols_array_2d(),
                    eye: [eye.x, eye.y, eye.z, 0.0],
                    sun_dir: [sun_dir.x, sun_dir.y, sun_dir.z, 0.0],
                    sun_color: [
                        light.color.x * light.intensity,
                        light.color.y * light.intensity,
                        light.color.z * light.intensity,
                        0.0,
                    ],
                    a: [
                        fog.density.max(0.0),
                        fog.height_falloff.max(0.0),
                        fog.height_offset,
                        fog.inscatter_strength.max(0.0),
                    ],
                    b: [
                        fog.phase_g.clamp(-0.999, 0.999),
                        fog.max_distance.max(1e-3),
                        z_far,
                        z_far / z_den,
                    ],
                    c: [z_near * z_far / z_den, 0.0, 0.0, 0.0],
                }),
            );
            let depth_view = &self
                .scene_depth
                .as_ref()
                .expect("雾开启时应已创建深度副本")
                .view;
            let views = hdr.post_views(taa_curr_view);
            crate::post::encode_post_pass(
                &mut encoder,
                &self.gpu.device,
                &views,
                &hdr.post_sampler,
                crate::post::PostPassDesc {
                    label: "iris.pass.fog",
                    pipeline: &self.bundle.fog_pipeline,
                    layout: &self.bundle.post_depth_layout,
                    params: &hdr.fog_params,
                    // 源 = 链源：TAA 输出（TAA 开，链首）或主解析缓冲
                    src: scene_or_taa(None),
                    dst: crate::post::PostScratchRef::Full(fog_slot),
                    extra_texture: Some(depth_view),
                },
            );
        }
        // 景深（DOF）：雾输出（雾开）或主解析缓冲 → 全分辨率槽。CoC 按逐像素线性视距
        // 计算，12-tap 泊松盘 HDR 平均；位于泛光之前（HDR 线性域——焦外
        // 亮源参与亮度提取更物理）。
        if dof_on {
            // 焦点实体解析（打磨 VI 摩擦⑤）：解析点就在链参数上传处——
            // 相机视图矩阵已就绪、不新增每帧系统、不回写资源字段
            let focus = crate::post::resolve_focus_distance(world, &dof, &view);
            self.gpu.queue.write_buffer(
                &hdr.dof_params,
                0,
                bytemuck::bytes_of(&DofParamsRaw {
                    a: [
                        focus.max(0.0),
                        dof.focus_range.max(1e-6),
                        dof.max_blur_radius.max(0.0),
                        0.0,
                    ],
                    b: [
                        1.0 / width.max(1) as f32,
                        1.0 / height.max(1) as f32,
                        0.0,
                        0.0,
                    ],
                }),
            );
            let depth_view = &self
                .scene_depth
                .as_ref()
                .expect("DOF 开启时应已创建深度副本")
                .view;
            let views = hdr.post_views(taa_curr_view);
            crate::post::encode_post_pass(
                &mut encoder,
                &self.gpu.device,
                &views,
                &hdr.post_sampler,
                crate::post::PostPassDesc {
                    label: "iris.pass.dof",
                    pipeline: &self.bundle.dof_pipeline,
                    layout: &self.bundle.post_depth_layout,
                    params: &hdr.dof_params,
                    // 源 = 雾输出槽（雾开——链头前插了一级）或链首
                    // （TAA 输出 / 主解析缓冲）
                    src: scene_or_taa(fog_dst),
                    dst: crate::post::PostScratchRef::Full(
                        slots.dof_dst.expect("DOF 开启时必有目标槽位"),
                    ),
                    extra_texture: Some(depth_view),
                },
            );
        }
        // 运动模糊（MB）：链头 → 全分辨率 B 槽。逐像素深度重建世界位 →
        // 上一帧相机重投影 → 屏幕速度方向 N-tap 采样；只覆盖相机运动
        // （物体级速度缓冲是 v1 限制，ARCHITECTURE §8）。首帧 / 相机静止
        // 时 prev_vp == vp → 速度 0 → 原色透传（逐位一致）。
        if mb_on {
            // 深度 → NDC z 的两个预计算系数（DirectX [0,1] 深度约定，
            // 推导见 motion_blur.wgsl 模块头）：z_ndc = f/(f−n) − (n·f/(f−n))/d
            let z_den = (z_far - z_near).max(1e-6);
            let a_coef = z_far / z_den;
            let b_coef = z_near * z_far / z_den;
            self.gpu.queue.write_buffer(
                &hdr.motion_blur_params,
                0,
                bytemuck::bytes_of(&MbParamsRaw {
                    inv_view_proj: vp.inverse().to_cols_array_2d(),
                    prev_view_proj: prev_vp.to_cols_array_2d(),
                    scalars: [mb.strength.max(0.0), mb.max_samples as f32, a_coef, b_coef],
                }),
            );
            let depth_view = &self
                .scene_depth
                .as_ref()
                .expect("MB 开启时应已创建深度副本")
                .view;
            let views = hdr.post_views(taa_curr_view);
            crate::post::encode_post_pass(
                &mut encoder,
                &self.gpu.device,
                &views,
                &hdr.post_sampler,
                crate::post::PostPassDesc {
                    label: "iris.pass.motion_blur",
                    pipeline: &self.bundle.motion_blur_pipeline,
                    layout: &self.bundle.post_depth_layout,
                    params: &hdr.motion_blur_params,
                    // 源 = DOF 输出槽（DOF 开）或链首（TAA 输出 / 主解析缓冲）
                    src: scene_or_taa(slots.mb_src),
                    dst: crate::post::PostScratchRef::Full(
                        slots.mb_dst.expect("MB 开启时必有目标槽位"),
                    ),
                    extra_texture: Some(depth_view),
                },
            );
        }
        if bloom.enabled {
            let bw = (width / 2).max(1) as f32;
            let bh = (height / 2).max(1) as f32;
            let texel = [1.0 / bw, 1.0 / bh];
            let params = [
                BloomParamsRaw {
                    a: [bloom.threshold.max(0.0), 0.5, 0.0, 0.0],
                    b: [0.0; 4],
                },
                BloomParamsRaw {
                    a: [0.0, 0.0, texel[0], texel[1]],
                    b: [texel[0], 0.0, 0.0, 0.0],
                },
                BloomParamsRaw {
                    a: [0.0, 0.0, texel[0], texel[1]],
                    b: [0.0, texel[1], 0.0, 0.0],
                },
            ];
            for (buffer, raw) in hdr.bloom_params.iter().zip(params) {
                self.gpu
                    .queue
                    .write_buffer(buffer, 0, bytemuck::bytes_of(&raw));
            }
            let views = hdr.post_views(taa_curr_view);
            // 提取：链头（DOF/MB 输出槽或主解析缓冲）→ 半分辨率 A 槽
            crate::post::encode_post_pass(
                &mut encoder,
                &self.gpu.device,
                &views,
                &hdr.post_sampler,
                crate::post::PostPassDesc {
                    label: "iris.pass.bloom_threshold",
                    pipeline: &self.bundle.bloom_threshold_pipeline,
                    layout: &self.bundle.post_layout,
                    params: &hdr.bloom_params[0],
                    src: head_input,
                    dst: crate::post::PostScratchRef::Half(crate::post::PingPongSlot::A),
                    extra_texture: None,
                },
            );
            // 模糊：槽位交替由 bloom_blur_slots 单点定义（任意轮数输出恒回
            // A 槽 = blit 固定采样）；源槽位决定绑定哪份方向参数与标签
            // （读 A = 水平、读 B = 垂直）
            for (src, dst) in crate::post::bloom_blur_slots(crate::post::BLOOM_BLUR_ROUNDS) {
                let (label, params_buf) = match src {
                    crate::post::PingPongSlot::A => {
                        ("iris.pass.bloom_blur_h", &hdr.bloom_params[1])
                    }
                    crate::post::PingPongSlot::B => {
                        ("iris.pass.bloom_blur_v", &hdr.bloom_params[2])
                    }
                };
                crate::post::encode_post_pass(
                    &mut encoder,
                    &self.gpu.device,
                    &views,
                    &hdr.post_sampler,
                    crate::post::PostPassDesc {
                        label,
                        pipeline: &self.bundle.bloom_blur_pipeline,
                        layout: &self.bundle.post_layout,
                        params: params_buf,
                        src: crate::post::PostInput::Scratch(crate::post::PostScratchRef::Half(
                            src,
                        )),
                        dst: crate::post::PostScratchRef::Half(dst),
                        extra_texture: None,
                    },
                );
            }
        }
        // 暗角：全分辨率 HDR → HDR（镜头物理遮挡近似，线性域压暗四角），
        // 位于泛光之后、blit 之前。强度 0（或缺席资源）整段跳过——blit
        // 直读主解析缓冲，与无暗角帧逐位一致（零开销默认路径）。DOF/MB
        // 入链时源切到链头、目标写链头对面槽位（同 pass 读写互斥，
        // 槽位由 full_chain_slots 单点定义）。
        if vignette_strength > 0.0 {
            self.gpu.queue.write_buffer(
                &hdr.vignette_params,
                0,
                bytemuck::bytes_of(&PostParamsRaw {
                    bloom_strength: 0.0,
                    exposure: 1.0,
                    vignette_strength,
                    _reserved: 0.0,
                }),
            );
            let views = hdr.post_views(taa_curr_view);
            crate::post::encode_post_pass(
                &mut encoder,
                &self.gpu.device,
                &views,
                &hdr.post_sampler,
                crate::post::PostPassDesc {
                    label: "iris.pass.vignette",
                    pipeline: &self.bundle.vignette_pipeline,
                    layout: &self.bundle.post_layout,
                    params: &hdr.vignette_params,
                    src: head_input,
                    dst: crate::post::PostScratchRef::Full(
                        slots.vignette_dst.expect("暗角开启时必有目标槽位"),
                    ),
                    extra_texture: None,
                },
            );
        }
        // 色调映射 blit：链尾输出 -> sRGB 输出。链尾 = 暗角输出（暗角开）
        // 或 DOF/MB 输出槽（相机级效果开）或 TAA 输出（仅 TAA 开）或主
        // 解析缓冲（全关——默认绑定组，与引入暗角前逐项一致）。源切换
        // 统一走 chain_source；全分辨率槽位与 TAA 半边的接入绑定组惰性
        // 构建、跨帧复用。
        {
            let blit_bg = match scene_or_taa(slots.blit_hdr) {
                crate::post::PostInput::Scene => &hdr.blit_bind_group,
                crate::post::PostInput::Scratch(crate::post::PostScratchRef::Full(
                    crate::post::PingPongSlot::A,
                )) => hdr
                    .blit_full_a_bg
                    .as_ref()
                    .expect("全分辨率 A 槽 blit 接入绑定组应已惰性构建"),
                crate::post::PostInput::Scratch(crate::post::PostScratchRef::Full(
                    crate::post::PingPongSlot::B,
                )) => hdr
                    .blit_full_b_bg
                    .as_ref()
                    .expect("全分辨率 B 槽 blit 接入绑定组应已惰性构建"),
                crate::post::PostInput::TaaHistory => {
                    &hdr.taa.as_ref().expect("TAA 开启时应有历史缓冲").blit_bgs
                        [(taa_index % 2) as usize]
                }
                // blit 的源恒全分辨率或主解析缓冲 / TAA 输出，半分辨率槽位
                // 只服务泛光模糊链内部（拓扑由 full_chain_slots 保证）
                crate::post::PostInput::Scratch(crate::post::PostScratchRef::Half(_)) => {
                    panic!("blit 源不可能是半分辨率槽位（链拓扑契约破坏）")
                }
            };
            // FXAA（R53）开启时 blit 改写中间 LDR 纹理（同格式、同管线、
            // 同绑定组——输出字节与直写最终目标一致），最终目标由紧随
            // 其后的 FXAA pass 写入；关闭时直写 color_view，本 pass 与
            // 引入前逐位一致（兼容硬约束实现点）。
            let blit_dst = match hdr.fxaa.as_ref() {
                Some(fx) if fxaa_on => &fx.view,
                _ => color_view,
            };
            let mut blit = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iris.pass.blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: blit_dst,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            blit.set_pipeline(&self.bundle.blit_pipeline);
            blit.set_bind_group(0, blit_bg, &[]);
            blit.draw(0..3, 0..1);
        }
        // FXAA pass（R53）：LDR 域边缘平滑，blit 之后、UI 之前——HUD
        // 文字画在被平滑的画面之上，自身保持锐利。全屏三角形逐像素
        // 覆写（清屏值随即被覆盖，与 blit pass 同款清屏以走最稳的
        // 附件生命周期）；非边缘像素在着色器内早退透传中心采样，
        // 画面与不开 FXAA 的差异只可能出现在边缘带上（开启即视觉
        // 变化是功能本意，关闭路径不经过本 pass）。
        if fxaa_on {
            let fx = hdr
                .fxaa
                .as_ref()
                .expect("FXAA 开启时 ensure_fxaa_targets 已建立中间缓冲");
            self.gpu.queue.write_buffer(
                &fx.params,
                0,
                bytemuck::bytes_of(&[
                    fxaa.edge_threshold.max(0.0),
                    fxaa.edge_threshold_min.max(0.0),
                    1.0 / width.max(1) as f32,
                    1.0 / height.max(1) as f32,
                ]),
            );
            let mut fxaa_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iris.pass.fxaa"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            fxaa_pass.set_pipeline(&self.bundle.fxaa_pipeline);
            fxaa_pass.set_bind_group(0, &fx.bind_group, &[]);
            fxaa_pass.draw(0..3, 0..1);
        }
        // UI 叠加层：blit 之后的最后一个 pass。同一 color_view、LoadOp::Load
        // （不清屏，直接叠加在色调映射结果上）、无深度附件。顶点非空才开
        // pass——空 UI 场景零额外开销。屏幕尺寸 uniform 每帧写入（write_buffer
        // 与本帧 submit 同队列，执行顺序保证在 pass 之前生效）。
        //
        // 绘制顺序（R40 含图片）：**图片先（在下）、atlas 文本/矩形后（在上）**
        // ——小地图底图应垫在 HUD 文字下方，「后画者在上」由 pass 内顺序统一
        // 裁决（同模块里矩形先、文本后再收集的既有语义）。图片按源（普通
        // 纹理 / 渲染目标）各持独立绑定组，逐个 draw；atlas 内容合并为单
        // draw call（与 UI 图片并存时 UI 总 draw call = 图片数 + 1）。
        let mut ui_vertices = std::mem::take(&mut self.ui_scratch);
        ui_vertices.clear();
        crate::ui::collect_ui_vertices(world, &mut ui_vertices);
        let mut ui_images = std::mem::take(&mut self.ui_image_scratch);
        ui_images.clear();
        crate::ui::collect_ui_image_vertices(world, &mut ui_images);
        if !ui_vertices.is_empty() || !ui_images.is_empty() {
            let screen = UiGlobalsRaw {
                screen: [width as f32, height as f32],
                _pad: [0.0; 2],
            };
            self.gpu
                .queue
                .write_buffer(&self.ui_screen_buf, 0, bytemuck::bytes_of(&screen));
            // 图片绑定组：每源一个（纹理 / RT 颜色视图 + UI 采样器 + 屏幕
            // uniform）。本帧重建（逐帧构建，RT 颜色视图跨帧稳定但懒建的
            // RT 可能在上一帧未渲染，逐帧重建保证拿到最新视图；图片为数
            // 个，重建成本可忽略）。
            let mut img_bgs: Vec<wgpu::BindGroup> = Vec::with_capacity(ui_images.len());
            for img in &ui_images {
                let view: &wgpu::TextureView = match (img.texture, img.rtt) {
                    (Some(h), None) => {
                        self.ensure_texture(texture_store.as_deref(), h);
                        self.texture_gpu
                            .get(&h.0)
                            .map(|t| &t.view)
                            .unwrap_or(&self.texture_gpu[&0].view)
                    }
                    (None, Some(t)) => match self.render_target_gpu.get(&t.0) {
                        Some(rt) => &rt.color_view,
                        None => {
                            log::warn!(
                                "渲染目标 {t:?} 未被渲染（无对应 RT 相机），UiImageRtt 回退白色兜底"
                            );
                            &self.texture_gpu[&0].view
                        }
                    },
                    _ => &self.texture_gpu[&0].view, // 两源皆无：防御回退白色
                };
                let bg = self
                    .gpu
                    .device
                    .create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("iris.ui.image.bg"),
                        layout: &self.bundle.ui_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(&self.ui_sampler),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: self.ui_screen_buf.as_entire_binding(),
                            },
                        ],
                    });
                img_bgs.push(bg);
            }
            // 图片顶点缓冲：合批后整段上传（每图 6 顶点，图序 = 顶点序）。
            if !ui_images.is_empty() {
                self.ensure_ui_image_capacity(ui_images.len() * 6);
                let mut image_vertices = Vec::with_capacity(ui_images.len() * 6);
                for img in &ui_images {
                    image_vertices.extend_from_slice(&img.vertices);
                }
                let image_buf = self.ui_image_buf.as_ref().expect("UI 图片顶点缓冲已就绪");
                self.gpu
                    .queue
                    .write_buffer(image_buf, 0, bytemuck::cast_slice(&image_vertices));
            }
            // atlas 顶点缓冲（矩形 + 文本合并）。
            if !ui_vertices.is_empty() {
                self.ensure_ui_vertex_capacity(ui_vertices.len());
                let ui_buf = self.ui_vertex_buf.as_ref().expect("UI 顶点缓冲已就绪");
                self.gpu
                    .queue
                    .write_buffer(ui_buf, 0, bytemuck::cast_slice(&ui_vertices));
            }
            let mut ui_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iris.pass.ui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // 图片（在下）：各源独立绑定组，逐图 draw。
            if !ui_images.is_empty() {
                let image_buf = self.ui_image_buf.as_ref().expect("UI 图片顶点缓冲已就绪");
                ui_pass.set_pipeline(&self.bundle.ui_pipeline);
                ui_pass.set_vertex_buffer(0, image_buf.slice(..));
                for (i, bg) in img_bgs.iter().enumerate() {
                    ui_pass.set_bind_group(0, bg, &[]);
                    let first = (i * 6) as u32;
                    ui_pass.draw(first..first + 6, 0..1);
                    draw_calls += 1;
                }
            }
            // atlas 内容（矩形 + 文本，在上）：单批单 draw。
            if !ui_vertices.is_empty() {
                let ui_buf = self.ui_vertex_buf.as_ref().expect("UI 顶点缓冲已就绪");
                ui_pass.set_pipeline(&self.bundle.ui_pipeline);
                ui_pass.set_bind_group(0, &self.ui_bind_group, &[]);
                ui_pass.set_vertex_buffer(0, ui_buf.slice(..));
                ui_pass.draw(0..ui_vertices.len() as u32, 0..1);
                draw_calls += 1;
            }
        }
        self.ui_scratch = ui_vertices;
        self.ui_image_scratch = ui_images;
        // SSR 帧尾副本（R62b）：主解析缓冲（MSAA 解析 + 天空 + 透明 + 粒子
        // 的最终结果）整张 copy 到上帧颜色副本——一次 copy_texture_to_
        // texture，成本 = 带宽往返 2×W×H×8 B（1080p ≈ 33 MiB/帧）。仅 SSR
        // 开启且本帧完成了场景深度副本时执行（两者凑齐，下一帧的「上帧」
        // 才完整）；分辨率变化后的首帧 prev_color 重建为空纹理，同样不在
        // 有效集合（重置语义）。
        if ssr_on && opaque_total > 0 {
            // 局部重借（此前的 hdr 借用早已结束；与 prev_color /
            // ssr_prev_valid 是互斥字段借用）
            let targets = self.hdr.as_ref().expect("HDR 中间缓冲已就绪");
            let prev = self
                .ssr
                .prev_color
                .as_ref()
                .expect("SSR 开启时应已创建上帧颜色副本");
            encoder.copy_texture_to_texture(
                targets.resolved.as_image_copy(),
                prev.texture.as_image_copy(),
                wgpu::Extent3d {
                    width: self.ssr.prev_size.0,
                    height: self.ssr.prev_size.1,
                    depth_or_array_layers: 1,
                },
            );
            self.ssr_prev_valid = true;
        } else if ssr_on {
            // SSR 开但本帧无几何：深度副本没跑，上帧资源不完整
            self.ssr_prev_valid = false;
        }
        self.gpu.queue.submit([encoder.finish()]);
        // 遮挡查询读回（R53）：提交后对映射区发起 map_async，回调经
        // 下一帧 draw 开头的 poll + try_recv 消费（缓冲在消费 unmap 前
        // 不可复用——在途期间该帧跳过查询与剔除）。
        if let Some((parity, entities, count)) = occ_issued {
            let occ = self
                .occ_gpu
                .as_ref()
                .expect("发出帧必有查询资源（本帧刚使用）");
            let (sender, receiver) = std::sync::mpsc::channel();
            occ.readback_bufs[parity]
                .slice(0..(count * std::mem::size_of::<u64>()) as u64)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let _ = sender.send(result);
                });
            self.occ_inflight = Some(OccInflight {
                parity,
                entities,
                count,
                frame: self.occ_frame,
                done: receiver,
            });
        }
        // meshlet 紧凑结果读回（R85）：提交后对 staging 槽发起 map_async，
        // 下一帧 draw 开头的 poll + try_recv 消费（遮挡同款协议——缓冲在
        // 消费 unmap 前不可复用，在途期间跳过 compute 与 copy）。
        if let Some(parity) = meshlet_issued {
            let mg = self
                .meshlet_gpu
                .as_ref()
                .expect("发出帧必有 meshlet GPU 资源（本帧刚使用）");
            let (sender, receiver) = std::sync::mpsc::channel();
            mg.staging[parity]
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let _ = sender.send(result);
                });
            self.meshlet_inflight = Some(MeshletInflight {
                parity,
                frame: self.meshlet_frame,
                done: receiver,
            });
        }
        self.stats.draw_calls = draw_calls;
        self.stats.instances = instances;
        self.stats.triangles_drawn = triangles_drawn;
        self.stats.particles_drawn = particles_drawn;
        self.stats.meshlet_surviving = meshlet_surviving;
        self.stats.taa_reset = u32::from(taa_on && taa_reset_frame);
        self.scratch = items;
        self.skinned_scratch = skinned;
        self.transparent_scratch = transparent;
        self.transparent_skinned_scratch = transparent_skinned;
        self.occ_drawn_scratch = occ_drawn;
        self.occ_aabb_scratch = occ_aabbs;
        self.occ_entity_scratch = occ_entities;
        self.meshlet_occ_aabb_scratch = meshlet_occ_aabbs;
        self.meshlet_occ_entity_scratch = meshlet_occ_entities;
        self.decal_scratch = decal_items;
        self.particle_scratch = particle_instances;
        self.particle_alpha_scratch = particle_alpha;
        self.particle_v2_scratch = particle_v2_items;
    }

    /// 实例缓冲容量复用：足够则零开销返回 `false`；不足时按 2 倍扩容
    /// 整体重建并返回 `true`——重建后缓冲内容作废，调用方必须全量上传。
    fn ensure_instance_capacity(&mut self, needed: usize) -> bool {
        if self.instance_capacity >= needed {
            return false;
        }
        let capacity = (needed * 2).max(256);
        self.instance_buf = Some(self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.instances"),
            size: (capacity * std::mem::size_of::<InstanceRaw>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.instance_capacity = capacity;
        log::debug!("实例缓冲扩容至 {capacity} 实例");
        true
    }

    /// 探针捕获实例缓冲容量复用（R62a）：独立于主帧实例缓冲——主缓冲带
    /// 跨帧增量上传镜像（`last_instances`），捕获写入会破坏其一致性。
    fn ensure_probe_instance_capacity(&mut self, needed: usize) {
        if self.probe_instance_capacity >= needed {
            return;
        }
        let capacity = (needed * 2).max(256);
        self.probe_instance_buf = Some(self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.probe.instances"),
            size: (capacity * std::mem::size_of::<InstanceRaw>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.probe_instance_capacity = capacity;
        log::debug!("探针捕获实例缓冲扩容至 {capacity} 实例");
    }

    /// UI 顶点缓冲容量复用（与实例缓冲同款模式）：容量足够则零开销，
    /// 不足时按 2 倍扩容整体重建。
    fn ensure_ui_vertex_capacity(&mut self, needed: usize) {
        if self.ui_capacity >= needed {
            return;
        }
        let capacity = (needed * 2).max(1024);
        self.ui_vertex_buf = Some(self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.ui.vertices"),
            size: (capacity * std::mem::size_of::<crate::ui::UiVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.ui_capacity = capacity;
        log::debug!("UI 顶点缓冲扩容至 {capacity} 顶点");
    }

    /// UI 图片顶点缓冲容量复用（照 UI atlas 顶点缓冲模式：容量不足时 2 倍
    /// 扩容整体重建）。图片顶点独立于 atlas 顶点缓冲——图片按源绑定组分别
    /// 绘制（与 atlas 单批单 draw 解耦，见 draw 的 UI pass）。
    fn ensure_ui_image_capacity(&mut self, needed: usize) {
        if self.ui_image_capacity >= needed {
            return;
        }
        let capacity = (needed * 2).max(64);
        self.ui_image_buf = Some(self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.ui.image_vertices"),
            size: (capacity * std::mem::size_of::<crate::ui::UiVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.ui_image_capacity = capacity;
        log::debug!("UI 图片顶点缓冲扩容至 {capacity} 顶点");
    }

    /// 粒子实例缓冲容量复用（同款模式）：容量足够则零开销，不足时按
    /// 2 倍扩容整体重建。粒子每帧全量上传，重建后无须特殊处理。
    fn ensure_particle_capacity(&mut self, needed: usize) {
        if self.particle_capacity >= needed {
            return;
        }
        let capacity = (needed * 2).max(1024);
        self.particle_buf = Some(self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.particles"),
            size: (capacity * std::mem::size_of::<ParticleInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.particle_capacity = capacity;
        log::debug!("粒子实例缓冲扩容至 {capacity} 实例");
    }

    /// 粒子 v2 池对齐（R82）：逐收集项找 / 建实体对应池，并处理
    /// `reset` 检测（游标回退 → storage 全量清零一次，~2.3 MB 上传，
    /// 仅检测到的那帧）。池的模拟 uniform 槽位按本帧待消费帧数增长
    ///（跨帧复用，不回收）。
    fn ensure_particle_v2_pools(&mut self, items: &mut [ParticleV2Item]) {
        for item in items {
            let idx = match self
                .particle_v2_pools
                .iter()
                .position(|p| p.entity == item.entity)
            {
                Some(i) => i,
                None => {
                    let pool = self.create_particle_v2_pool(item.entity);
                    self.particle_v2_pools.push(pool);
                    self.particle_v2_pools.len() - 1
                }
            };
            // reset 检测：游标回退（v1 语义里池满停发只前进不回退）
            if item.cursor < self.particle_v2_pools[idx].bound_cursor {
                let zero = vec![
                    0u8;
                    (PARTICLES_V2_STRIDE * crate::particles_v2::PARTICLES_V2_MAX as u64)
                        as usize
                ];
                self.gpu
                    .queue
                    .write_buffer(&self.particle_v2_pools[idx].particles_buf, 0, &zero);
                log::debug!("粒子 v2 池检测到发射器 reset（游标回退），storage 已清零");
            }
            self.particle_v2_pools[idx].bound_cursor = item.cursor;
            // 模拟 uniform 槽位按需增长（每份待消费帧一个槽）
            let needed = item.frames.len();
            while self.particle_v2_pools[idx].sim_uniforms.len() < needed {
                let device = &self.gpu.device;
                let buf = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("iris.particle_v2.sim_params"),
                    size: std::mem::size_of::<crate::particles_v2::SimParamsRaw>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("iris.particle_v2.sim.bg"),
                    layout: &self.bundle.particle_v2_sim_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: buf.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: self.particle_v2_pools[idx]
                                .particles_buf
                                .as_entire_binding(),
                        },
                    ],
                });
                let pool = &mut self.particle_v2_pools[idx];
                pool.sim_uniforms.push(buf);
                pool.sim_bgs.push(bg);
            }
            item.pool = idx;
        }
    }

    // ---- meshlet（R85，虚拟几何体 V2）----

    /// 创建 meshlet GPU 资源束（首个 meshlet 注册时一次；顶点 / 索引容量
    /// 取 CPU 镜像当前尺寸的向上余量，后续注册超出时走重建路径）。
    fn create_meshlet_gpu(&self) -> MeshletGpu {
        let device = &self.gpu.device;
        let cluster_words_bytes = (crate::meshlet::MAX_MESHLET_CLUSTERS
            * std::mem::size_of::<crate::meshlet::MeshletClusterRaw>())
            as u64;
        let flags_bytes = crate::meshlet::MAX_MESHLET_CLUSTERS as u64 * 4;
        let words_cap = (self.meshlet_arena.words.len() as u64 * 2).max(1 << 14);
        let index_cap = (self.meshlet_arena.indices.len() as u64 * 2).max(1 << 14);
        let vertex_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.meshlet.vertex_arena"),
            size: words_cap * 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let index_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.meshlet.index_arena"),
            size: index_cap * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let instance_tables: [wgpu::Buffer; 2] = std::array::from_fn(|i| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("iris.meshlet.instances[{i}]")),
                size: (MAX_MESHLET_INSTANCES * std::mem::size_of::<MeshletInstanceEntryRaw>())
                    as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        let cluster_words_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.meshlet.cluster_words"),
            size: cluster_words_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let flags_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.meshlet.flags"),
            size: flags_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let payload_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.meshlet.payload"),
            size: flags_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let compact_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.meshlet.compact"),
            size: flags_bytes,
            // COPY_SRC：帧尾 copy 进回读 staging（消费紧凑列表与计数）
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let count_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.meshlet.count"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let cull_params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.meshlet.cull_params"),
            size: std::mem::size_of::<MeshletCullParamsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let staging: [wgpu::Buffer; 2] = std::array::from_fn(|i| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(&format!("iris.meshlet.staging[{i}]")),
                size: MESHLET_STAGING_BYTES,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        let mk_cull_bg = |instances: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("iris.meshlet.cull.bg"),
                layout: &self.bundle.meshlet_cull_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: cull_params_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: cluster_words_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: instances.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: flags_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: payload_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: compact_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: count_buf.as_entire_binding(),
                    },
                ],
            })
        };
        let cull_bgs = [
            mk_cull_bg(&instance_tables[0]),
            mk_cull_bg(&instance_tables[1]),
        ];
        let mk_draw_bg = |instances: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("iris.meshlet.draw.bg"),
                layout: &self.bundle.meshlet_draw_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: cluster_words_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: vertex_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: index_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: instances.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: compact_buf.as_entire_binding(),
                    },
                ],
            })
        };
        let draw_bgs = [
            mk_draw_bg(&instance_tables[0]),
            mk_draw_bg(&instance_tables[1]),
        ];
        MeshletGpu {
            cluster_words_buf,
            flags_buf,
            payload_buf,
            compact_buf,
            count_buf,
            cull_params_buf,
            vertex_buf,
            index_buf,
            vertex_capacity_words: words_cap,
            index_capacity: index_cap,
            instance_tables,
            cull_bgs,
            draw_bgs,
            staging,
        }
    }

    /// meshlet arena 注册（变更时上传）：把未登记的 meshlet 句柄追加进
    /// 簇 / 顶点 / 索引 arena。追加式布局保证已发布帧的紧凑列表引用恒
    /// 有效；顶点 / 索引容量不足时重建缓冲 + 全量重传（注册期低频）+
    /// 重建绘制绑定组（旧缓冲随重建作废）。全部句柄已登记时零上传零
    /// 开销（每帧调用安全）。
    ///
    /// R87：注册期同时构建页划分表（容器 v3 随附优先，v1/v2 按同参数
    /// 现算——逐位等价）、页号全局基址与簇 bounds 联合盒（遮挡查询的
    /// 实例 AABB）。`stream_active` = 流送预算在场：真 ⇒ **跳过整库数
    /// 据上传**（页数据按驻留裁决逐页 `write_buffer`；缓冲容量照常预
    /// 留——页槽 = arena 规范区间，VRAM 全量预留是 v1 诚实限制）；假
    /// ⇒ 整库上传照旧（R86 逐位不变）。流送 → 非流送迁移
    /// （`meshlet_stream_recover_upload`）在本函数头部整库重传——驻留
    /// 期间页可能被逐出 arena，非流送路径要求全量有效。
    fn ensure_meshlet_world(
        &mut self,
        store: &crate::meshlet::MeshletMeshStore,
        stream_active: bool,
    ) {
        // R87 迁移重传：先于「无新句柄」早退——迁移帧即使无新网格也补。
        if self.meshlet_stream_recover_upload {
            self.meshlet_stream_recover_upload = false;
            if let Some(mg) = self.meshlet_gpu.as_ref() {
                for (handle, slot) in self.meshlet_arena.slots.iter().enumerate() {
                    let Some(slot) = slot else { continue };
                    let Some(mesh) = store.get(crate::meshlet::MeshletMeshHandle(handle as u32))
                    else {
                        continue;
                    };
                    self.gpu.queue.write_buffer(
                        &mg.cluster_words_buf,
                        slot.cluster_base as u64
                            * std::mem::size_of::<crate::meshlet::MeshletClusterRaw>() as u64,
                        bytemuck::cast_slice(&mesh.clusters),
                    );
                    let v0 = slot.vertex_base as usize * 2;
                    let v1 = v0 + mesh.vertices.len() * 2;
                    self.gpu.queue.write_buffer(
                        &mg.vertex_buf,
                        slot.vertex_base as u64 * 32,
                        bytemuck::cast_slice(&self.meshlet_arena.words[v0..v1]),
                    );
                    let i0 = slot.index_base as usize;
                    let i1 = i0 + mesh.indices.len();
                    self.gpu.queue.write_buffer(
                        &mg.index_buf,
                        slot.index_base as u64 * 4,
                        bytemuck::cast_slice(&self.meshlet_arena.indices[i0..i1]),
                    );
                }
            }
        }
        if store.len() <= self.meshlet_arena.slots.len() {
            return; // 无新句柄（句柄只追加，登记数单调追赶目录长度）
        }
        let mut new_slots: Vec<Option<MeshletArenaSlot>> = Vec::new();
        let mut new_words: Vec<[f32; 4]> = Vec::new();
        let mut new_indices: Vec<u32> = Vec::new();
        let mut new_cluster_bytes: Vec<u8> = Vec::new();
        let mut total_clusters = self.cluster_count();
        let mut page_base = self
            .meshlet_arena
            .slots
            .iter()
            .flatten()
            .map(|s| s.pages.page_count())
            .sum::<u32>();
        let mut base_vertex = self.meshlet_arena.words.len() as u32 / 2;
        let mut base_index = self.meshlet_arena.indices.len() as u32;
        for handle in self.meshlet_arena.slots.len()..store.len() {
            let Some(mesh) = store.get(crate::meshlet::MeshletMeshHandle(handle as u32)) else {
                new_slots.push(None);
                continue;
            };
            // 容量裁决（注册处校验 + warn，spec 口径）：单帧剔除容量按
            // arena 总量计；空网格不可实例化。
            if mesh.clusters.is_empty()
                || mesh.vertices.is_empty()
                || total_clusters + mesh.clusters.len() > crate::meshlet::MAX_MESHLET_CLUSTERS
            {
                log::warn!(
                    "meshlet 网格 #{handle}（{} 簇）超出单帧剔除容量或为空，\
                     已跳过注册（总量 {total_clusters}/{}）",
                    mesh.clusters.len(),
                    crate::meshlet::MAX_MESHLET_CLUSTERS
                );
                new_slots.push(None);
                continue;
            }
            let Some((min, extent)) = crate::meshlet::mesh_bounds(&mesh.vertices) else {
                new_slots.push(None);
                continue;
            };
            // R86：叶簇界定全叶表范围；层级 Arc 随槽登记（选择输入）。
            let leaf_cluster_count = mesh.leaf_count();
            let hierarchy = if mesh.hierarchy.groups.is_empty() {
                None
            } else {
                Some(std::sync::Arc::new(mesh.hierarchy.clone()))
            };
            let cluster_triangles: Vec<u8> =
                mesh.clusters.iter().map(|c| c.triangle_count).collect();
            // 页表（R87）：容器 v3 随附的划分表优先；v1/v2 / 内存态网格
            // 按同参数现算（纯派生，与构建期写入逐位等价）。
            let pages = std::sync::Arc::new(mesh.pages.clone().unwrap_or_else(|| {
                crate::meshlet_stream::MeshletPageTable::derive(
                    mesh,
                    crate::meshlet_stream::MESHLET_PAGE_CLUSTERS,
                )
            }));
            // 簇 bounds 联合（R87 遮挡整合）：全部簇包围球的联合 AABB
            //（网格局部空间；球心 = 归一化 × extent + min，半径世界单
            // 位）。min/max 精确无累加舍入，扫描序 = 簇构建序（确定）。
            let mut umin = Vec3::splat(f32::INFINITY);
            let mut umax = Vec3::splat(f32::NEG_INFINITY);
            let min_v = Vec3::from(min);
            let extent_v = Vec3::from(extent);
            for c in &mesh.clusters {
                let center = min_v + Vec3::new(c.bounds[0], c.bounds[1], c.bounds[2]) * extent_v;
                let r = c.bounds[3];
                umin = umin.min(center - Vec3::splat(r));
                umax = umax.max(center + Vec3::splat(r));
            }
            new_words.extend(crate::meshlet::vertex_storage_words(&mesh.vertices));
            new_indices.extend(mesh.indices.iter().copied());
            new_cluster_bytes.extend_from_slice(bytemuck::cast_slice(&mesh.clusters));
            // 全局页基址 = 本网格之前累计的页数（先取值后累加——基址指向
            // 本网格首个页；写反会与后一网格的页号区间重叠，驻留集的
            // FALSE POSITIVE 让未上传页以全零数据参与绘制）。
            let slot_page_base = page_base;
            page_base += pages.page_count();
            new_slots.push(Some(MeshletArenaSlot {
                vertex_base: base_vertex,
                index_base: base_index,
                cluster_base: total_clusters as u32,
                cluster_count: mesh.clusters.len() as u32,
                leaf_cluster_count,
                hierarchy,
                bounds_min: min,
                bounds_extent: extent,
                cluster_triangles,
                page_base: slot_page_base,
                pages,
                cluster_union: (umin.to_array(), umax.to_array()),
            }));
            base_vertex += mesh.vertices.len() as u32;
            base_index += mesh.indices.len() as u32;
            total_clusters += mesh.clusters.len();
        }
        if self.meshlet_gpu.is_none() {
            self.meshlet_gpu = Some(self.create_meshlet_gpu());
        }
        let cluster_offset = self.cluster_count() as u64
            * std::mem::size_of::<crate::meshlet::MeshletClusterRaw>() as u64;
        let words_offset = self.meshlet_arena.words.len() as u64 * 16;
        let index_offset = self.meshlet_arena.indices.len() as u64 * 4;
        // 顶点 / 索引容量检查：不足 = 重建 + 旧数据全量重传 + 重建设计侧
        // 绑定组（compute 绑定组不引用这两个缓冲，无需重建）。
        // 流送激活时重建只分配不回传（页数据按驻留逐页上传——全量回传会
        // 绕过预算）；**已在驻留的页由本函数尾部的重建补传还原**（见那里
        // 的论证：`tick` 不会重新请求已驻留页，不补传就是静默消失）。
        let needed_words = (self.meshlet_arena.words.len() + new_words.len()) as u64;
        let needed_indices = (self.meshlet_arena.indices.len() + new_indices.len()) as u64;
        let mut rebuilt = false;
        {
            let mg = self.meshlet_gpu.as_mut().expect("GPU 资源束已创建");
            if needed_words > mg.vertex_capacity_words {
                let cap = needed_words * 2;
                mg.vertex_buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("iris.meshlet.vertex_arena"),
                    size: cap * 16,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                mg.vertex_capacity_words = cap;
                if !stream_active {
                    self.gpu.queue.write_buffer(
                        &mg.vertex_buf,
                        0,
                        bytemuck::cast_slice(&self.meshlet_arena.words),
                    );
                }
                rebuilt = true;
            }
            if needed_indices > mg.index_capacity {
                let cap = needed_indices * 2;
                mg.index_buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("iris.meshlet.index_arena"),
                    size: cap * 4,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                mg.index_capacity = cap;
                if !stream_active {
                    self.gpu.queue.write_buffer(
                        &mg.index_buf,
                        0,
                        bytemuck::cast_slice(&self.meshlet_arena.indices),
                    );
                }
                rebuilt = true;
            }
        }
        if rebuilt {
            let mg = self.meshlet_gpu.as_mut().expect("GPU 资源束已创建");
            let device = &self.gpu.device;
            let draw_bgs: [wgpu::BindGroup; 2] = std::array::from_fn(|i| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("iris.meshlet.draw.bg"),
                    layout: &self.bundle.meshlet_draw_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: mg.cluster_words_buf.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: mg.vertex_buf.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: mg.index_buf.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: mg.instance_tables[i].as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 4,
                            resource: mg.compact_buf.as_entire_binding(),
                        },
                    ],
                })
            });
            mg.draw_bgs = draw_bgs;
        }
        // 追加上传（write_buffer 偏移均为 4 的倍数——48/16/4 天然满足）。
        // 流送激活 = 逐页上传接管（数据落点 = 同一规范区间，页驻留裁决
        // 通过后的 tick 中写入），此处跳过整库传。
        let mg = self.meshlet_gpu.as_ref().expect("GPU 资源束已创建");
        if !stream_active {
            self.gpu
                .queue
                .write_buffer(&mg.cluster_words_buf, cluster_offset, &new_cluster_bytes);
            self.gpu.queue.write_buffer(
                &mg.vertex_buf,
                words_offset,
                bytemuck::cast_slice(&new_words),
            );
            self.gpu.queue.write_buffer(
                &mg.index_buf,
                index_offset,
                bytemuck::cast_slice(&new_indices),
            );
        }
        self.meshlet_arena.slots.extend(new_slots);
        self.meshlet_arena.words.extend(new_words);
        self.meshlet_arena.indices.extend(new_indices);
        // 流送激活时的**重建补传**（R87）：顶点 / 索引 arena 被重建 ⇒ 新缓冲
        // 零初始化，而驻留状态机仍认为旧页驻留——`ResidencyManager::tick`
        // 只对**未驻留**页组队（`resident.contains` 短路），所以「后续请求
        // 自然补传」不成立：不补传就是「静默消失」（簇元数据在定长缓冲里
        // 完好 ⇒ cull 照报存活、顶点 / 索引全零 ⇒ 零面积三角、画面空而
        // 统计不空）。按当前驻留集全量补传，保住「驻留集 ⇒ arena 字节有效」
        // 这一选择端依赖的不变量。首帧注册时驻留集为空（管理器尚未创建），
        // 本段天然零开销。
        if rebuilt && stream_active {
            let resident: Vec<u32> = self
                .meshlet_stream
                .as_ref()
                .map(|m| m.resident().iter().copied().collect())
                .unwrap_or_default();
            if !resident.is_empty() {
                let ops = crate::meshlet_stream::ResidencyOps {
                    evict: Vec::new(),
                    upload: resident,
                };
                self.apply_residency_ops(store, &ops);
            }
        }
    }

    /// 已登记的全局簇总数（arena 占用）。
    fn cluster_count(&self) -> usize {
        self.meshlet_arena
            .slots
            .iter()
            .flatten()
            .map(|s| s.cluster_count as usize)
            .sum()
    }

    /// 全局页号 →（网格槽位下标 = 句柄, 网格内局部页号）。
    fn locate_meshlet_page(&self, page: u32) -> Option<(usize, u32)> {
        for (si, slot) in self.meshlet_arena.slots.iter().enumerate() {
            let Some(slot) = slot else { continue };
            let pc = slot.pages.page_count();
            if page >= slot.page_base && page < slot.page_base + pc {
                return Some((si, page - slot.page_base));
            }
        }
        None
    }

    /// 执行一帧驻留裁决（R87）：先逐 `evict`（页数据清零——非驻留页的
    /// arena 区间不保留有效数据，预算的字节语义落在真实拷贝上），后传
    /// `upload`（页数据从 [`crate::meshlet::MeshletMeshStore`] 写进
    /// arena 的规范区间；簇 / 顶点词 / 索引三段）。页号失配（防御）静
    /// 默跳过——注册期页基址记账与状态机共享同一来源，正规路径不触。
    fn apply_residency_ops(
        &self,
        store: &crate::meshlet::MeshletMeshStore,
        ops: &crate::meshlet_stream::ResidencyOps,
    ) {
        let Some(mg) = self.meshlet_gpu.as_ref() else {
            return;
        };
        for &page in &ops.evict {
            let Some((si, local)) = self.locate_meshlet_page(page) else {
                continue;
            };
            let Some(Some(slot)) = self.meshlet_arena.slots.get(si) else {
                continue;
            };
            let (c0, cc) = slot.pages.page_cluster_range(local);
            let (vs, vc) = slot.pages.page_vertex_range(local);
            let (is, ic) = slot.pages.page_index_range(local);
            let zero_c =
                vec![0u8; cc as usize * std::mem::size_of::<crate::meshlet::MeshletClusterRaw>()];
            let zero_v = vec![0u8; vc as usize * std::mem::size_of::<crate::mesh::Vertex>()];
            let zero_i = vec![0u8; ic as usize * 4];
            self.gpu.queue.write_buffer(
                &mg.cluster_words_buf,
                (slot.cluster_base + c0) as u64
                    * std::mem::size_of::<crate::meshlet::MeshletClusterRaw>() as u64,
                &zero_c,
            );
            self.gpu.queue.write_buffer(
                &mg.vertex_buf,
                (slot.vertex_base + vs) as u64 * 32,
                &zero_v,
            );
            self.gpu
                .queue
                .write_buffer(&mg.index_buf, (slot.index_base + is) as u64 * 4, &zero_i);
        }
        for &page in &ops.upload {
            let Some((si, local)) = self.locate_meshlet_page(page) else {
                continue;
            };
            let Some(Some(slot)) = self.meshlet_arena.slots.get(si) else {
                continue;
            };
            let Some(mesh) = store.get(crate::meshlet::MeshletMeshHandle(si as u32)) else {
                continue;
            };
            let (c0, cc) = slot.pages.page_cluster_range(local);
            let (vs, vc) = slot.pages.page_vertex_range(local);
            let (is, ic) = slot.pages.page_index_range(local);
            let c0 = c0 as usize;
            let cc = cc as usize;
            let vs = vs as usize;
            let vc = vc as usize;
            let is = is as usize;
            let ic = ic as usize;
            self.gpu.queue.write_buffer(
                &mg.cluster_words_buf,
                (slot.cluster_base + c0 as u32) as u64
                    * std::mem::size_of::<crate::meshlet::MeshletClusterRaw>() as u64,
                bytemuck::cast_slice(&mesh.clusters[c0..c0 + cc]),
            );
            let words = crate::meshlet::vertex_storage_words(&mesh.vertices[vs..vs + vc]);
            self.gpu.queue.write_buffer(
                &mg.vertex_buf,
                (slot.vertex_base + vs as u32) as u64 * 32,
                bytemuck::cast_slice(&words),
            );
            self.gpu.queue.write_buffer(
                &mg.index_buf,
                (slot.index_base + is as u32) as u64 * 4,
                bytemuck::cast_slice(&mesh.indices[is..is + ic]),
            );
        }
    }

    /// 消费上一帧的 meshlet 紧凑结果读回（遮挡 consume_occlusion_results
    /// 同款协议：非阻塞 poll + try_recv，迟到保持在途、映射失败废弃、
    /// 到达即解析并构建绘制输入）。解析 = 4 B 存活计数 + 紧凑列表前缀 +
    /// 逐簇三角形精确统计（解析表 = 产出该列表那一帧的实例槽 → 句柄）。
    fn consume_meshlet_readback(&mut self) {
        if self.meshlet_inflight.is_none() {
            return;
        }
        let _ = self.gpu.device.poll(wgpu::PollType::Poll);
        let (ready, map_err) = {
            let inflight = self.meshlet_inflight.as_ref().expect("在途记录已确认存在");
            match inflight.done.try_recv() {
                Ok(Ok(())) => (true, false),
                Ok(Err(e)) => {
                    log::warn!("meshlet 紧凑结果读回映射失败：{e}，本帧走全量兜底");
                    (true, true)
                }
                Err(_) => (false, false), // 回调未到：保持在途（保守方向）
            }
        };
        if !ready {
            return;
        }
        let inflight = self.meshlet_inflight.take().expect("刚确认回调到达");
        if map_err {
            self.meshlet_consumed = None;
            return; // 缓冲未被映射，直接废弃在途记录
        }
        let mg = self
            .meshlet_gpu
            .as_ref()
            .expect("读回缓冲随 GPU 资源束同在");
        let list = {
            let bytes = mg.staging[inflight.parity]
                .get_mapped_range(..)
                .expect("已映射的读回缓冲必须可读");
            let count = u32::from_le_bytes(bytes[0..4].try_into().expect("计数 4 B")) as usize;
            let count = count.min(crate::meshlet::MAX_MESHLET_CLUSTERS);
            let words: &[u32] = bytemuck::cast_slice(&bytes[4..4 + count * 4]);
            words.to_vec()
        };
        mg.staging[inflight.parity].unmap();
        // 三角形精确统计（handles / run_starts = 产出该列表那一帧的实例
        // 表；run 内局部号经 run 起点还原网格局部簇号——R86 run 形实例
        // 表口径。防御式越界跳过：正规路径下 compute 产物必然良构）
        let handles = std::mem::take(&mut self.meshlet_last_handles);
        let run_starts = std::mem::take(&mut self.meshlet_last_run_starts);
        let mut triangles = 0u32;
        for &e in &list {
            let inst = (e >> 16) as usize;
            let local = (e & 0xFFFF) as usize;
            let handle = handles.get(inst).copied();
            let run_start = run_starts.get(inst).copied().unwrap_or(0);
            if let Some(Some(slot)) = handle.and_then(|h| self.meshlet_arena.slots.get(h as usize))
            {
                triangles += slot
                    .cluster_triangles
                    .get(run_start as usize + local)
                    .copied()
                    .unwrap_or(0) as u32;
            }
        }
        self.meshlet_consumed = Some(MeshletConsumed {
            list,
            triangles,
            frame: inflight.frame,
        });
    }

    /// 本帧 meshlet 帧上传 + 绘制裁决（hdr 不可变借用之前的 `&mut self`
    /// 段内调用）：实例表 / 剔除参数上传、兜底裁决（无已消费紧凑结果 ⇒
    /// CPU 写恒等列表全量绘制——像素与剔除帧恒等）、默认材质绑定组预热。
    #[allow(clippy::too_many_arguments)]
    fn prepare_meshlet_frame(
        &mut self,
        frame: &MeshletFrame,
        materials: &MaterialStore,
        textures: Option<&TextureStore>,
        lightmaps: Option<&LightmapStore>,
        vp: Mat4,
        eye: Vec3,
    ) -> MeshletDrawPlan {
        let frame_index = self.meshlet_frame;
        let cull_parity = (frame_index % 2) as usize;
        let mg = self.meshlet_gpu.as_ref().expect("GPU 资源束已随注册创建");
        // 兜底裁决：无已消费紧凑结果或其产出帧不是恰好上一帧（首帧 /
        // 读回迟到 / 映射失败 / 跳帧）⇒ 恒等列表全量绘制。剔除只删不可见
        // 几何 ⇒ 全量帧与剔除帧像素恒等，这是「回读一帧延迟」下的连续性
        // 兜底（诚实限制 §8.54）。
        let consumed_valid = self
            .meshlet_consumed
            .as_ref()
            .is_some_and(|c| c.frame + 1 == frame_index);
        let fallback = !consumed_valid;
        let (draw_count, draw_parity, triangles) = if fallback {
            let mut identity: Vec<u32> = Vec::with_capacity(frame.total_clusters as usize);
            let mut triangles = 0u32;
            for (i, entry) in frame.entries.iter().enumerate() {
                let handle = frame.handles[i];
                let run_start = frame.run_starts[i];
                let run_len = entry.base_counts[3];
                if let Some(Some(slot)) = self.meshlet_arena.slots.get(handle as usize) {
                    // run 区间三角形精确累加（防御式 get——畸形层级
                    // 数据走统计缺失而非 panic；正规路径区间必在界内）
                    let range = run_start as usize..(run_start + run_len) as usize;
                    triangles += slot
                        .cluster_triangles
                        .get(range)
                        .map(|slice| slice.iter().map(|&t| t as u32).sum::<u32>())
                        .unwrap_or(0);
                }
                for local in 0..run_len {
                    identity.push(((i as u32) << 16) | local);
                }
            }
            self.gpu
                .queue
                .write_buffer(&mg.compact_buf, 0, bytemuck::cast_slice(&identity));
            (frame.total_clusters, cull_parity, triangles)
        } else {
            let consumed = self.meshlet_consumed.as_ref().expect("兜底裁决已排除 None");
            (
                consumed.list.len() as u32,
                ((frame_index + 1) % 2) as usize,
                consumed.triangles,
            )
        };
        // 本帧实例表（compute 槽位）+ 剔除参数（VP 与绘制同源——含 TAA
        // 抖动，保证「剔除边界」与「实际可见边界」一致）
        self.gpu.queue.write_buffer(
            &mg.instance_tables[cull_parity],
            0,
            bytemuck::cast_slice(&frame.entries),
        );
        let max_count = frame
            .entries
            .iter()
            .map(|e| e.base_counts[3])
            .max()
            .unwrap_or(1);
        self.gpu.queue.write_buffer(
            &mg.cull_params_buf,
            0,
            bytemuck::bytes_of(&MeshletCullParamsRaw {
                view_proj: vp.to_cols_array_2d(),
                eye: [eye.x, eye.y, eye.z, 0.0],
                counts: [frame.entries.len() as u32, frame.total_clusters, 0, 0],
            }),
        );
        // meshlet 路径 v1 恒用默认材质（句柄 0；单 instanced draw 单材质，
        // 诚实限制 §8.54）——在 hdr 借用之前预热绑定组
        self.ensure_material(
            materials,
            textures,
            lightmaps,
            MaterialHandle(0),
            NO_LIGHTMAP,
        );
        MeshletDrawPlan {
            draw_count,
            triangles,
            draw_parity,
            cull_parity,
            cull_blocks: max_count.div_ceil(MESHLET_CULL_WORKGROUP).max(1),
            dispatch_instances: frame.entries.len() as u32,
            do_compute: self.meshlet_inflight.is_none(),
        }
    }

    /// 创建一个粒子 v2 GPU 池（storage 由 wgpu 零初始化 = 全体死粒子，
    /// 首帧 dispatch 前状态良构）。
    fn create_particle_v2_pool(&self, entity: u64) -> ParticleV2Pool {
        let device = &self.gpu.device;
        let particles_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.particle_v2.pool"),
            size: PARTICLES_V2_STRIDE * crate::particles_v2::PARTICLES_V2_MAX as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let draw_params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.particle_v2.draw_params"),
            size: std::mem::size_of::<crate::particles_v2::DrawParamsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let draw_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iris.particle_v2.draw.bg"),
            layout: &self.bundle.particle_v2_draw_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: draw_params_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: particles_buf.as_entire_binding(),
                },
            ],
        });
        ParticleV2Pool {
            entity,
            particles_buf,
            sim_uniforms: Vec::new(),
            sim_bgs: Vec::new(),
            draw_params_buf,
            draw_bg,
            bound_cursor: 0,
        }
    }

    /// 贴花实例缓冲容量复用（同款模式）：容量足够则零开销，不足时按
    /// 2 倍扩容整体重建。贴花每帧全量上传，重建后无须特殊处理。
    fn ensure_decal_capacity(&mut self, needed: usize) {
        if self.decal_capacity >= needed {
            return;
        }
        let capacity = (needed * 2).max(256);
        self.decal_instance_buf = Some(self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.decals"),
            size: (capacity * std::mem::size_of::<DecalInstanceRaw>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.decal_capacity = capacity;
        log::debug!("贴花实例缓冲扩容至 {capacity} 实例");
    }

    /// 构建单张渲染目标的 GPU 资源（惰性创建后被 `ensure_render_target`
    /// 缓存，尺寸不变则跨帧复用）。颜色纹理挂 `TEXTURE_BINDING` 供
    /// [`crate::ui::UiImageRtt`] 采样；几何渲到线性 HDR 中间缓冲
    /// （几何管线均为 HDR_FORMAT 输出），再经 `blit_bg` ACES 色调映射到
    /// 颜色纹理。
    fn build_render_target(&self, width: u32, height: u32) -> GpuRenderTarget {
        let size = wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        };
        let color = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.rt.color"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let depth = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.rt.depth"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let hdr = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.rt.hdr"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
        let hdr_view = hdr.create_view(&wgpu::TextureViewDescriptor::default());
        let globals_buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.rt.globals"),
            size: std::mem::size_of::<GlobalsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // RT 相机专用全局绑定组：与主全局组共享级联参数（CSM 仍按主相机
        // 拟合——RT 相机朝向不同时的 CSM 拟合是 v1 限制，见 rendertarget.rs
        // 与 ARCHITECTURE §8.20）、阴影贴图、IBL，仅视图投影 / 眼点不同。
        // SSR 三路走零参数 + 哑纹理（RT 视口无上帧语义，见 §4.31 裁决 2）。
        let globals_bg = build_globals_bind_group(
            &self.gpu,
            &self.bundle,
            GlobalsBuffers {
                globals: &globals_buf,
                csm_params: &self.csm_params_buf,
            },
            ShadowMapViews {
                cascades: &self.shadow_map_view,
                point_cube: &self.point_shadow_cube_view,
            },
            &self.env,
            ProbeGlobalsResources {
                params_buf: &self.probe_params_buf,
                cube_view: &self.probe_cube_view,
                gi_probes: &self.gi_sh.off_probes_buf,
                gi_params: &self.gi_sh.off_params_buf,
                gi_depth: &self.ddgi.off_depth_buf,
            },
            ScreenSpaceGlobalsResources {
                params_buf: &self.ssr.off_params_buf,
                color_view: &self.ssr.dummy_color_view,
                depth_view: &self.ssr.dummy_depth_view,
                ao_view: &self.ao.dummy_view,
                bins_params: &self.light_bins.off_params_buf,
                bins_lights: &self.light_bins.off_lights_buf,
                bins_tiles: &self.light_bins.off_tiles_buf,
            },
        );
        // ACES 色调映射绑定组：hdr_view → color_view；bloom 槽绑定白色
        // 兜底纹素（泛光强度恒 0，着色器不采样）、采样器 / 参数为 RT 常值。
        let blit_bg = build_blit_bind_group(
            &self.gpu.device,
            &self.bundle.blit_layout,
            &hdr_view,
            &self.texture_gpu[&0].view,
            &self.rt_blit_sampler,
            &self.rt_post_params_buf,
        );
        GpuRenderTarget {
            color,
            color_view,
            depth,
            depth_view,
            hdr,
            hdr_view,
            blit_bg,
            globals_buf,
            globals_bg,
            instance_buf: None,
            instance_capacity: 0,
            width: width.max(1),
            height: height.max(1),
        }
    }

    /// 若句柄尚无 GPU 资源则构建并缓存；**尺寸与 store 描述失配则整体
    /// 重建**（[`RenderTargetStore::resize`] 的生效路径——旧纹理 / 缓冲随
    /// 缓存条目替换即刻释放，实例缓冲本就每帧全量上传无增量损失）。
    /// 尺寸一致的常态帧走零动作快路（既有逐位行为不变）。
    fn ensure_render_target(&mut self, handle: RenderTargetHandle, width: u32, height: u32) {
        let (w, h) = (width.max(1), height.max(1));
        let resizes = match self.render_target_gpu.get(&handle.0) {
            Some(rt) if rt.width == w && rt.height == h => return,
            Some(_) => true,
            None => false,
        };
        let rt = self.build_render_target(w, h);
        self.render_target_gpu.insert(handle.0, rt);
        if resizes {
            log::debug!("渲染目标 {handle:?} 尺寸变更，GPU 资源已重建（{w}×{h}）");
        } else {
            log::debug!("渲染目标 {handle:?} GPU 资源已构建（{w}×{h}）");
        }
    }

    /// RT 实例缓冲容量复用（与主实例缓冲同款——容量不足时 2 倍扩容整体
    /// 重建，返回 `true` 时调用方须全量上传）。RT 每帧全量上传（实例少、
    /// 视锥剔除结果常变，帧级比对无增量价值）。
    fn ensure_rt_instance_capacity(&mut self, handle: RenderTargetHandle, needed: usize) -> bool {
        let rt = self
            .render_target_gpu
            .get_mut(&handle.0)
            .expect("RT 已就绪");
        if rt.instance_capacity >= needed {
            return false;
        }
        let capacity = (needed * 2).max(256);
        rt.instance_buf = Some(self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.rt.instances"),
            size: (capacity * std::mem::size_of::<InstanceRaw>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        rt.instance_capacity = capacity;
        log::debug!("RT 实例缓冲扩容至 {capacity} 实例");
        true
    }

    /// 渲染一台 RT 相机到其渲染目标（R40）。
    ///
    /// 语义（v1，见 rendertarget.rs / ARCHITECTURE §4.21）：
    /// - **几何**：不透明 + 蒙皮 + 透明（实例区间与主 pass 同布局），用 RT
    ///   相机自己的视锥做剔除（复用 [`collect_instances`] 的视锥参数化）；
    ///   天空盒 / 贴花 / 粒子 v1 不画（限制）；
    /// - **阴影**：复用主阴影图 / CSM（csm_params 仍按主相机拟合——RT 相机
    ///   朝向不同时是 v1 限制，见 §8.20）；
    /// - **输出**：几何渲到 RT 线性 HDR 中间缓冲 → `rt_blit_pipeline` ACES
    ///   色调映射到 RT 颜色纹理（Rgba8UnormSrgb）。无后处理链（无泛光 /
    ///   曝光 / 暗角）无 UI（UI 由主 pass 的 HUD 在 blit 之后叠加）；
    /// - **确定性 / 帧统计**：收集排序与主 pass 同款（`total_cmp` 全序 + 并列
    ///   次序断言）；FrameStats 口径 = **主几何**——RT 辅助视口不计入
    ///   `draw_calls` / `instances`（见 §4.21 与 §4.4.1 口径说明）。
    ///
    /// 调用时机：阴影 pass 之后（复用刚写出的阴影贴图）、`hdr` 借用建立之前
    ///（RT 资源惰性创建需要 &mut self，与贯穿主 pass 的 hdr 借用冲突）。
    #[allow(clippy::too_many_arguments)]
    fn render_to_rt_target(
        &mut self,
        world: &iris_core::world::World,
        encoder: &mut wgpu::CommandEncoder,
        mesh_store: &MeshStore,
        material_store: &MaterialStore,
        texture_store: Option<&TextureStore>,
        lightmap_store: Option<&LightmapStore>,
        globals: &GlobalsRaw,
        prep: &RtCameraPrep,
    ) {
        self.ensure_render_target(prep.handle, prep.width, prep.height);
        let mut items = std::mem::take(&mut self.rt_items);
        let mut skinned = std::mem::take(&mut self.rt_skinned);
        let mut transparent = std::mem::take(&mut self.rt_transparent);
        let mut transparent_skinned = std::mem::take(&mut self.rt_transparent_skinned);
        items.clear();
        skinned.clear();
        transparent.clear();
        transparent_skinned.clear();
        // RT 相机自己的视锥剔除（复用主 pass 的收集逻辑与排序）。
        let frustum = Frustum::from_view_proj(&prep.vp);
        let _counts = collect_instances(
            world,
            mesh_store,
            material_store,
            &prep.view,
            prep.eye,
            &frustum,
            &mut items,
            &mut skinned,
            &mut transparent,
            &mut transparent_skinned,
            // RT 相机 = v1 选择口径（无滞回记忆，见 LodSettings 文档）
            None,
            None,
        );
        items.sort_unstable_by_key(|item| item.key);
        transparent.sort_unstable_by(|a, b| {
            b.depth
                .total_cmp(&a.depth)
                .then_with(|| a.entity.cmp(&b.entity))
        });
        transparent_skinned.sort_unstable_by(|a, b| {
            b.depth
                .total_cmp(&a.depth)
                .then_with(|| a.skinned.skin_entity.cmp(&b.skinned.skin_entity))
        });
        let total = items.len();
        let opaque_total = total + skinned.len();
        let instance_total = opaque_total + transparent.len() + transparent_skinned.len();

        // GPU 资源预上传：网格 / 材质 / 光照贴图 / 蒙皮关节缓冲（与主 pass
        // 懒上传同款；同一网格/材质/图集/蒙皮实体若主 pass 已上传则缓存命中
        // 零开销）。段划分与绘制段同源（run_length）。
        let mut idx = 0;
        while idx < total {
            let key = items[idx].key;
            let lightmap = items[idx].lightmap;
            let count = run_length(&items, idx);
            self.ensure_mesh(mesh_store, MeshHandle((key >> 32) as u32));
            self.ensure_material(
                material_store,
                texture_store,
                lightmap_store,
                MaterialHandle(key as u32),
                lightmap,
            );
            idx += count;
        }
        for item in skinned
            .iter()
            .chain(transparent_skinned.iter().map(|t| &t.skinned))
        {
            self.ensure_mesh(mesh_store, MeshHandle(item.mesh));
            self.ensure_material(
                material_store,
                texture_store,
                lightmap_store,
                MaterialHandle(item.material),
                item.lightmap,
            );
            self.ensure_skin(item.skin_entity, item.joint_matrices.len());
        }
        // 形变目标解算（C4a，RT 几何 pass 消费同一份覆盖）
        self.resolve_morphs(mesh_store, &mut skinned, &mut transparent_skinned);
        for item in &transparent {
            self.ensure_mesh(mesh_store, MeshHandle((item.key >> 32) as u32));
            self.ensure_material(
                material_store,
                texture_store,
                lightmap_store,
                MaterialHandle(item.key as u32),
                NO_LIGHTMAP,
            );
        }

        // 全部需要 &mut self 的资源预上传完成后再取 rt（&self 借用），避免
        // 与 ensure_* 的 &mut self 冲突。
        self.ensure_rt_instance_capacity(prep.handle, instance_total);
        let rt = &self.render_target_gpu[&prep.handle.0];

        // RT 专用全局（复用主全局的光 / 点光 / 阴影参数，仅换视图投影与眼点）。
        let mut rglobals = *globals;
        rglobals.view_proj = prep.vp.to_cols_array_2d();
        rglobals.eye_pos = [prep.eye.x, prep.eye.y, prep.eye.z, 1.0];
        rglobals.inv_view_proj = prep.vp.inverse().to_cols_array_2d();
        self.gpu
            .queue
            .write_buffer(&rt.globals_buf, 0, bytemuck::bytes_of(&rglobals));

        // RT 实例：每帧全量上传（实例少、RT 视锥剔除结果常变，帧级比对无
        // 增量价值）。蒙皮关节矩阵同样全量覆写（同实体与主 pass 并列时末次
        // 写同值，不破坏确定性）。缓冲已在 ensure_rt_instance_capacity 就绪。
        if instance_total > 0 {
            let mut instances = std::mem::take(&mut self.rt_instance_scratch);
            instances.clear();
            instances.extend(items.iter().map(|i| i.instance));
            instances.extend(skinned.iter().map(|i| i.instance));
            instances.extend(transparent.iter().map(|i| i.instance));
            instances.extend(transparent_skinned.iter().map(|t| t.skinned.instance));
            let instance_buf = rt.instance_buf.as_ref().expect("RT 实例缓冲已就绪");
            self.gpu
                .queue
                .write_buffer(instance_buf, 0, bytemuck::cast_slice(&instances));
            // 蒙皮关节矩阵（RT 视锥内的蒙皮实体全量覆写，与主 pass 同路径）。
            let mut joint_bytes: Vec<u8> = Vec::new();
            for item in skinned
                .iter()
                .chain(transparent_skinned.iter().map(|t| &t.skinned))
            {
                joint_bytes.clear();
                for m in &item.joint_matrices {
                    joint_bytes.extend_from_slice(bytemuck::bytes_of(&m.to_cols_array_2d()));
                }
                let skin = self
                    .skin_gpu
                    .get(&item.skin_entity)
                    .expect("蒙皮缓冲已就绪");
                self.gpu
                    .queue
                    .write_buffer(&skin.joint_buf, 0, &joint_bytes);
            }
            self.rt_instance_scratch = instances;
        }

        // 几何 pass：单 pass 依次画不透明 + 蒙皮 + 透明（同一 HDR 中间缓冲，
        // 深度清一次、不透明写入、透明只测不写）——RT 无贴花 / 粒子 / 天空盒。
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iris.pass.rt_geometry"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &rt.hdr_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &rt.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        // 深度 Store：透明 pass 在同一 pass 内（Load 同附件）做遮挡
                        // 测试——与主 pass 的透明语义一致（主 pass 的 DX12 教训）。
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if instance_total > 0 {
                let instance_buf = rt.instance_buf.as_ref().expect("RT 实例缓冲已就绪");
                pass.set_pipeline(&self.bundle.rt_pipeline);
                pass.set_bind_group(0, &rt.globals_bg, &[]);
                pass.set_vertex_buffer(1, instance_buf.slice(..));
                idx = 0;
                while idx < total {
                    let key = items[idx].key;
                    let lightmap = items[idx].lightmap;
                    let count = run_length(&items, idx);
                    let mesh = &self.mesh_gpu[&((key >> 32) as u32)];
                    let material = &self.material_gpu[&material_key(key as u32, lightmap)];
                    pass.set_pipeline(if items[idx].double_sided {
                        &self.bundle.rt_double_sided_pipeline
                    } else {
                        &self.bundle.rt_pipeline
                    });
                    pass.set_bind_group(1, &material.bind_group, &[]);
                    pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
                    // 切线缓冲（R42，槽 2）：缺席网格绑常驻零垫
                    pass.set_vertex_buffer(2, self.tangent_buf(mesh).slice(..));
                    // 光照贴图 UV2（R52c，槽 3）：缺席网格同样绑零垫
                    pass.set_vertex_buffer(3, self.uv2_buf(mesh).slice(..));
                    pass.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.num_indices, 0, idx as u32..(idx + count) as u32);
                    idx += count;
                }
                if !skinned.is_empty() {
                    pass.set_pipeline(&self.bundle.rt_skinned_pipeline);
                    pass.set_bind_group(0, &rt.globals_bg, &[]);
                    pass.set_vertex_buffer(2, instance_buf.slice(..));
                    for (i, item) in skinned.iter().enumerate() {
                        let mesh = &self.mesh_gpu[&item.mesh];
                        let material =
                            &self.material_gpu[&material_key(item.material, item.lightmap)];
                        let skin = &self.skin_gpu[&item.skin_entity];
                        pass.set_pipeline(if item.double_sided {
                            &self.bundle.rt_double_sided_skinned_pipeline
                        } else {
                            &self.bundle.rt_skinned_pipeline
                        });
                        pass.set_bind_group(1, &material.bind_group, &[]);
                        pass.set_bind_group(2, &skin.bind_group, &[]);
                        pass.set_vertex_buffer(0, item.vertex_buf(mesh).slice(..));
                        // 切线缓冲（R42，槽 3，蒙皮布局）：缺席绑零垫
                        pass.set_vertex_buffer(3, self.tangent_buf(mesh).slice(..));
                        // UV2（R52c，槽 4，蒙皮布局）：缺席绑零垫
                        pass.set_vertex_buffer(4, self.uv2_buf(mesh).slice(..));
                        pass.set_vertex_buffer(
                            1,
                            mesh.skin_buf
                                .as_ref()
                                .expect("蒙皮网格应有蒙皮属性缓冲")
                                .slice(..),
                        );
                        pass.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                        let base = (total + i) as u32;
                        pass.draw_indexed(0..mesh.num_indices, 0, base..base + 1);
                    }
                }
                if !transparent.is_empty() || !transparent_skinned.is_empty() {
                    pass.set_pipeline(&self.bundle.rt_transparent_pipeline);
                    pass.set_bind_group(0, &rt.globals_bg, &[]);
                    pass.set_vertex_buffer(1, instance_buf.slice(..));
                    let base = opaque_total as u32;
                    let mut t = 0usize;
                    while t < transparent.len() {
                        let key = transparent[t].key;
                        let mut count = 1;
                        while t + count < transparent.len() && transparent[t + count].key == key {
                            count += 1;
                        }
                        let mesh = &self.mesh_gpu[&((key >> 32) as u32)];
                        // 透明普通项不采样光照贴图（与主 pass 同口径）
                        let material = &self.material_gpu[&material_key(key as u32, NO_LIGHTMAP)];
                        pass.set_pipeline(if transparent[t].double_sided {
                            &self.bundle.rt_transparent_double_sided_pipeline
                        } else {
                            &self.bundle.rt_transparent_pipeline
                        });
                        pass.set_bind_group(1, &material.bind_group, &[]);
                        pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
                        // 切线缓冲（R42，槽 2）：缺席网格绑常驻零垫
                        pass.set_vertex_buffer(2, self.tangent_buf(mesh).slice(..));
                        // UV2（R52c，槽 3）：缺席绑零垫
                        pass.set_vertex_buffer(3, self.uv2_buf(mesh).slice(..));
                        pass.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(
                            0..mesh.num_indices,
                            0,
                            base + t as u32..base + (t + count) as u32,
                        );
                        t += count;
                    }
                    if !transparent_skinned.is_empty() {
                        pass.set_pipeline(&self.bundle.rt_transparent_skinned_pipeline);
                        pass.set_bind_group(0, &rt.globals_bg, &[]);
                        pass.set_vertex_buffer(2, instance_buf.slice(..));
                        for (i, t) in transparent_skinned.iter().enumerate() {
                            let item = &t.skinned;
                            let mesh = &self.mesh_gpu[&item.mesh];
                            let material =
                                &self.material_gpu[&material_key(item.material, item.lightmap)];
                            let skin = &self.skin_gpu[&item.skin_entity];
                            pass.set_pipeline(if item.double_sided {
                                &self.bundle.rt_transparent_double_sided_skinned_pipeline
                            } else {
                                &self.bundle.rt_transparent_skinned_pipeline
                            });
                            pass.set_bind_group(1, &material.bind_group, &[]);
                            pass.set_bind_group(2, &skin.bind_group, &[]);
                            pass.set_vertex_buffer(0, item.vertex_buf(mesh).slice(..));
                            // 切线缓冲（R42，槽 3，蒙皮布局）：缺席绑零垫
                            pass.set_vertex_buffer(3, self.tangent_buf(mesh).slice(..));
                            // UV2（R52c，槽 4，蒙皮布局）：缺席绑零垫
                            pass.set_vertex_buffer(4, self.uv2_buf(mesh).slice(..));
                            pass.set_vertex_buffer(
                                1,
                                mesh.skin_buf
                                    .as_ref()
                                    .expect("蒙皮网格应有蒙皮属性缓冲")
                                    .slice(..),
                            );
                            pass.set_index_buffer(
                                mesh.index_buf.slice(..),
                                wgpu::IndexFormat::Uint32,
                            );
                            let start = (opaque_total + transparent.len() + i) as u32;
                            pass.draw_indexed(0..mesh.num_indices, 0, start..start + 1);
                        }
                    }
                }
            }
        }
        // ACES blit：RT 线性 HDR 中间缓冲 → RT 颜色纹理（sRGB）。
        let mut blit = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("iris.pass.rt_blit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &rt.color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(self.clear_color),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        blit.set_pipeline(&self.bundle.rt_blit_pipeline);
        blit.set_bind_group(0, &rt.blit_bg, &[]);
        blit.draw(0..3, 0..1);

        self.rt_items = items;
        self.rt_skinned = skinned;
        self.rt_transparent = transparent;
        self.rt_transparent_skinned = transparent_skinned;
    }

    /// 平面水体捕获（R81）：反射（镜像相机）与折射（主相机）两张半分辨率
    /// 图。编码位置 = 阴影 / RT 相机 pass 之后、主 pass 的 hdr 借用建立
    /// 之前（捕获资源的容量管理需要 &mut self）。捕获内容 = 不透明几何
    ///（普通 + 蒙皮）+ 天空（探针捕获同款最低配组装：SSR / AO / 分箱 /
    /// 探针走哑资源——无屏幕语义、无探针自反馈），**不含水体自身**（无
    /// 递归）、不含透明 / 贴花 / 粒子；CSM 复用主相机拟合（捕获朝向不
    /// 同时阴影近似，与 RT 相机 §8.20 同款限制）。
    ///
    /// **镜像环绕反转的处理**：反射矩阵行列式为负 ⇒ 三角形环绕方向反转
    /// （R62a 教训），捕获管线统一用 `probe_capture_double_sided` 族
    ///（cull None）吸收——单面 / 双面 / 镜像 / 主相机四种场景共用两条
    /// 管线，front_face 约定随之失去意义（cull None 不剔除）。
    #[allow(clippy::too_many_arguments)]
    fn encode_water_captures(
        &mut self,
        world: &iris_core::world::World,
        encoder: &mut wgpu::CommandEncoder,
        mesh_store: &MeshStore,
        material_store: &MaterialStore,
        texture_store: Option<&TextureStore>,
        lightmap_store: Option<&LightmapStore>,
        globals: &GlobalsRaw,
        main_eye: Vec3,
        main_vp: Mat4,
        refl_view: Mat4,
        refl_vp: Mat4,
        refl_eye: Vec3,
        main_items: &[DrawItem],
        main_skinned: &[SkinnedItem],
        main_total: usize,
    ) {
        // ---- 反射捕获收集：镜像相机自己的视锥（镜像视角能看到主视锥外
        // 的物体——如相机身后的岸壁，必须独立收集而不是复用主视锥列表）
        let mut items = std::mem::take(&mut self.water_items);
        let mut skinned = std::mem::take(&mut self.water_skinned);
        let mut transparent = std::mem::take(&mut self.water_transparent);
        let mut transparent_skinned = std::mem::take(&mut self.water_transparent_skinned);
        items.clear();
        skinned.clear();
        transparent.clear();
        transparent_skinned.clear();
        let frustum = Frustum::from_view_proj(&refl_vp);
        collect_instances(
            world,
            mesh_store,
            material_store,
            &refl_view,
            refl_eye,
            &frustum,
            &mut items,
            &mut skinned,
            &mut transparent,
            &mut transparent_skinned,
            // 水体反射捕获 = v1 选择口径（同探针 / RT 立场）
            None,
            None,
        );
        // 透明列表不消费（捕获不画透明）——原样归还 scratch
        items.sort_unstable_by_key(|item| item.key);
        let refl_total = items.len();
        let refl_skinned = skinned.len();
        // 懒上传（网格 / 材质 / 关节——主 pass 未覆盖的镜像视锥实体在此
        // 补齐；已上传的命中缓存零开销）
        for item in &items {
            let key = item.key;
            let lightmap = item.lightmap;
            self.ensure_mesh(mesh_store, MeshHandle((key >> 32) as u32));
            self.ensure_material(
                material_store,
                texture_store,
                lightmap_store,
                MaterialHandle(key as u32),
                lightmap,
            );
        }
        for item in &skinned {
            self.ensure_mesh(mesh_store, MeshHandle(item.mesh));
            self.ensure_material(
                material_store,
                texture_store,
                lightmap_store,
                MaterialHandle(item.material),
                item.lightmap,
            );
        }
        let mut joint_bytes: Vec<u8> = Vec::new();
        for item in &skinned {
            self.ensure_skin(item.skin_entity, item.joint_matrices.len());
            joint_bytes.clear();
            for m in &item.joint_matrices {
                joint_bytes.extend_from_slice(bytemuck::bytes_of(&m.to_cols_array_2d()));
            }
            let skin = self
                .skin_gpu
                .get(&item.skin_entity)
                .expect("蒙皮缓冲已就绪");
            self.gpu
                .queue
                .write_buffer(&skin.joint_buf, 0, &joint_bytes);
        }
        // 形变目标解算（C4a，水面反射 / 水下绘制消费同一份覆盖）
        self.resolve_morphs(mesh_store, &mut skinned, &mut transparent_skinned);
        // 反射实例：专用缓冲每帧全量上传（主缓冲带跨帧增量镜像，复用会
        // 破坏其一致性）
        self.ensure_water_instance_capacity(refl_total + refl_skinned);
        if refl_total + refl_skinned > 0 {
            let mut raws = std::mem::take(&mut self.water_instance_scratch);
            raws.clear();
            raws.extend(items.iter().map(|i| i.instance));
            raws.extend(skinned.iter().map(|i| i.instance));
            let buf = self
                .water
                .as_ref()
                .expect("水体资源应已创建")
                .instance_buf
                .as_ref()
                .expect("反射实例缓冲已就绪");
            self.gpu
                .queue
                .write_buffer(buf, 0, bytemuck::cast_slice(&raws));
            self.water_instance_scratch = raws;
        }
        // ---- 每捕获相机一份 Globals（复用主全局的光照 / 阴影参数，仅换
        // 视图投影与眼点——反射相机的眼点 = 主相机眼点的镜像，天空射线
        // 重建因此自洽：inv(M·vp·) 恰把像素还原到镜像世界）
        let mut refl_globals = *globals;
        refl_globals.view_proj = refl_vp.to_cols_array_2d();
        refl_globals.eye_pos = [refl_eye.x, refl_eye.y, refl_eye.z, 1.0];
        refl_globals.inv_view_proj = refl_vp.inverse().to_cols_array_2d();
        let mut refr_globals = *globals;
        refr_globals.view_proj = main_vp.to_cols_array_2d();
        refr_globals.eye_pos = [main_eye.x, main_eye.y, main_eye.z, 1.0];
        refr_globals.inv_view_proj = main_vp.inverse().to_cols_array_2d();
        {
            let water = self.water.as_ref().expect("水体资源应已创建");
            self.gpu.queue.write_buffer(
                &water.refl_globals_buf,
                0,
                bytemuck::bytes_of(&refl_globals),
            );
            self.gpu.queue.write_buffer(
                &water.refr_globals_buf,
                0,
                bytemuck::bytes_of(&refr_globals),
            );
        }
        // ---- 两个捕获 pass（天空最先、几何随后覆盖——探针捕获同款）
        // 反射 pass
        {
            let water = self.water.as_ref().expect("水体资源应已创建");
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iris.pass.water_refl"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &water.refl_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // 清黑后立刻被天空覆盖（探针捕获同款）
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &water.refl_depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.bundle.probe_sky_pipeline);
            pass.set_bind_group(0, &water.refl_sky_bg, &[]);
            pass.draw(0..3, 0..1);
            if refl_total + refl_skinned > 0 {
                // cull None 捕获管线（单面 / 双面共用——镜像环绕反转由
                // cull None 吸收，见方法文档）
                let instance_buf = water.instance_buf.as_ref().expect("反射实例缓冲已就绪");
                pass.set_pipeline(&self.bundle.probe_capture_double_sided_pipeline);
                pass.set_bind_group(0, &water.refl_globals_bg, &[]);
                pass.set_vertex_buffer(1, instance_buf.slice(..));
                let mut idx = 0;
                while idx < refl_total {
                    let key = items[idx].key;
                    let lightmap = items[idx].lightmap;
                    let count = run_length(&items, idx);
                    let mesh = &self.mesh_gpu[&((key >> 32) as u32)];
                    let material = &self.material_gpu[&material_key(key as u32, lightmap)];
                    pass.set_bind_group(1, &material.bind_group, &[]);
                    pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
                    // 切线 / UV2（槽 2/3）：缺席网格绑常驻零垫（与主 pass
                    // 同口径——零切线 = 着色器走屏幕空间 TBN）
                    pass.set_vertex_buffer(2, self.tangent_buf(mesh).slice(..));
                    pass.set_vertex_buffer(3, self.uv2_buf(mesh).slice(..));
                    pass.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.num_indices, 0, idx as u32..(idx + count) as u32);
                    idx += count;
                }
                if refl_skinned > 0 {
                    pass.set_pipeline(&self.bundle.probe_capture_double_sided_skinned_pipeline);
                    pass.set_vertex_buffer(2, instance_buf.slice(..));
                    for (i, item) in skinned.iter().enumerate() {
                        let mesh = &self.mesh_gpu[&item.mesh];
                        let material =
                            &self.material_gpu[&material_key(item.material, item.lightmap)];
                        let skin = &self.skin_gpu[&item.skin_entity];
                        pass.set_bind_group(1, &material.bind_group, &[]);
                        pass.set_bind_group(2, &skin.bind_group, &[]);
                        pass.set_vertex_buffer(0, item.vertex_buf(mesh).slice(..));
                        pass.set_vertex_buffer(
                            1,
                            mesh.skin_buf
                                .as_ref()
                                .expect("蒙皮网格应有蒙皮属性缓冲")
                                .slice(..),
                        );
                        pass.set_vertex_buffer(3, self.tangent_buf(mesh).slice(..));
                        pass.set_vertex_buffer(4, self.uv2_buf(mesh).slice(..));
                        pass.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                        let base = (refl_total + i) as u32;
                        pass.draw_indexed(0..mesh.num_indices, 0, base..base + 1);
                    }
                }
            }
        }
        // 折射 pass（主相机视角；实例直接复用主实例缓冲的不透明区间
        // [0, opaque_total)——同一相机同一 VP，内容与主 pass 不透明结果
        // 一致，零额外上传）
        {
            let water = self.water.as_ref().expect("水体资源应已创建");
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iris.pass.water_refr"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &water.refr_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &water.refr_depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.bundle.probe_sky_pipeline);
            pass.set_bind_group(0, &water.refr_sky_bg, &[]);
            pass.draw(0..3, 0..1);
            let main_opaque = main_total + main_skinned.len();
            if main_opaque > 0 {
                let instance_buf = self.instance_buf.as_ref().expect("实例缓冲已就绪");
                pass.set_pipeline(&self.bundle.probe_capture_double_sided_pipeline);
                pass.set_bind_group(0, &water.refr_globals_bg, &[]);
                pass.set_vertex_buffer(1, instance_buf.slice(..));
                let mut idx = 0;
                while idx < main_total {
                    let key = main_items[idx].key;
                    let lightmap = main_items[idx].lightmap;
                    let count = run_length(main_items, idx);
                    let mesh = &self.mesh_gpu[&((key >> 32) as u32)];
                    let material = &self.material_gpu[&material_key(key as u32, lightmap)];
                    pass.set_bind_group(1, &material.bind_group, &[]);
                    pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
                    pass.set_vertex_buffer(2, self.tangent_buf(mesh).slice(..));
                    pass.set_vertex_buffer(3, self.uv2_buf(mesh).slice(..));
                    pass.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.num_indices, 0, idx as u32..(idx + count) as u32);
                    idx += count;
                }
                if !main_skinned.is_empty() {
                    pass.set_pipeline(&self.bundle.probe_capture_double_sided_skinned_pipeline);
                    pass.set_vertex_buffer(2, instance_buf.slice(..));
                    for (i, item) in main_skinned.iter().enumerate() {
                        let mesh = &self.mesh_gpu[&item.mesh];
                        let material =
                            &self.material_gpu[&material_key(item.material, item.lightmap)];
                        let skin = &self.skin_gpu[&item.skin_entity];
                        pass.set_bind_group(1, &material.bind_group, &[]);
                        pass.set_bind_group(2, &skin.bind_group, &[]);
                        pass.set_vertex_buffer(0, item.vertex_buf(mesh).slice(..));
                        pass.set_vertex_buffer(
                            1,
                            mesh.skin_buf
                                .as_ref()
                                .expect("蒙皮网格应有蒙皮属性缓冲")
                                .slice(..),
                        );
                        pass.set_vertex_buffer(3, self.tangent_buf(mesh).slice(..));
                        pass.set_vertex_buffer(4, self.uv2_buf(mesh).slice(..));
                        pass.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                        let base = (main_total + i) as u32;
                        pass.draw_indexed(0..mesh.num_indices, 0, base..base + 1);
                    }
                }
            }
        }
        // 归还反射收集暂存（透明列表本就为空，原样归还）
        self.water_items = items;
        self.water_skinned = skinned;
        self.water_transparent = transparent;
        self.water_transparent_skinned = transparent_skinned;
    }

    fn ensure_mesh(&mut self, store: &MeshStore, handle: MeshHandle) {
        if self.mesh_gpu.contains_key(&handle.0) {
            return;
        }
        let Some(data) = store.get(handle) else {
            log::warn!("网格句柄 {handle:?} 不存在");
            return;
        };
        let (vertex_buf, index_buf, num_indices, skin_buf, tangent_buf, uv2_buf) =
            upload_mesh(&self.gpu, data);
        // 无切线 / 无 UV2 且顶点数超出常驻零垫容量（65536）的网格：单独
        // 分配零缓冲兜底（wgpu 缓冲创建即零初始化）。常规网格恒走常驻零垫，
        // 零逐网格开销。
        let oversized = data.vertices.len() > Self::TANGENT_DUMMY_VERTICES;
        let tangent_buf = tangent_buf.or_else(|| {
            oversized.then(|| {
                self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("iris.mesh.tangent_zero"),
                    size: (data.vertices.len() * 16) as u64,
                    usage: wgpu::BufferUsages::VERTEX,
                    mapped_at_creation: false,
                })
            })
        });
        let uv2_buf = uv2_buf.or_else(|| {
            oversized.then(|| {
                self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("iris.mesh.uv2_zero"),
                    size: (data.vertices.len() * 8) as u64,
                    usage: wgpu::BufferUsages::VERTEX,
                    mapped_at_creation: false,
                })
            })
        });
        self.mesh_gpu.insert(
            handle.0,
            GpuMesh {
                vertex_buf,
                index_buf,
                num_indices,
                skin_buf,
                tangent_buf,
                uv2_buf,
            },
        );
    }

    /// 常驻零填充切线 / UV2 垫容量（R42 / R52c）：65536 顶点
    /// （切线 1 MiB = ×16 B、UV2 512 KiB = ×8 B）。
    const TANGENT_DUMMY_VERTICES: usize = 65536;

    /// 该网格应绑定的切线顶点缓冲（R42）：网格自带切线用自有缓冲；
    /// 缺席绑常驻零垫。顶点数超出垫容量（> 65536）的无切线网格在
    /// `ensure_mesh` 单独分配零缓冲兜底，此处的 `unwrap_or` 恒安全。
    fn tangent_buf<'a>(&'a self, mesh: &'a GpuMesh) -> &'a wgpu::Buffer {
        mesh.tangent_buf.as_ref().unwrap_or(&self.tangent_dummy_buf)
    }

    /// 该网格应绑定的 UV2 顶点缓冲（R52c，口径同 [`Self::tangent_buf`]）：
    /// 缺席绑常驻零垫（全零 + 材质掩码 0 ⇒ 光照贴图路径不生效）。
    fn uv2_buf<'a>(&'a self, mesh: &'a GpuMesh) -> &'a wgpu::Buffer {
        mesh.uv2_buf.as_ref().unwrap_or(&self.uv2_dummy_buf)
    }

    /// 蒙皮的关节矩阵缓冲：按关节数上限分配，不足以时整体重建。
    fn ensure_skin(&mut self, skin_entity: u64, joint_count: usize) {
        if let Some(skin) = self.skin_gpu.get(&skin_entity) {
            if skin.capacity >= joint_count {
                return;
            }
        }
        let capacity = joint_count.max(16);
        let joint_buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.skin.joints"),
            size: (capacity * 64) as u64, // 每关节一个 4x4 f32 矩阵
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = self
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("iris.skin.bg"),
                layout: &self.bundle.joints_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: joint_buf.as_entire_binding(),
                }],
            });
        self.skin_gpu.insert(
            skin_entity,
            GpuSkin {
                joint_buf,
                bind_group,
                capacity,
            },
        );
    }

    /// KTX2 预压缩直传（C5a-2）：按容器 vkFormat 建纹理并逐级
    /// `write_texture`（bytes_per_row = ⌈级宽/4⌉×块字节，与编码路径
    /// 同口径）。保守裁剪两处（与本仓 BC1 决策树的 4 对齐纪律同源）：
    /// 基级维数非 4 倍数 ⇒ 白图兜底 + 告警（wgpu 对压缩纹理的块对齐
    /// 校验面）；维数 < 4 的尾级跳过上传且不计入 `mip_level_count`
    /// （与 `mip_chain_bc` 「链止于 4×4」同口径，容器多出的 2/1 级
    /// 数据驻留 CPU 侧不浪费显存——采样器永远取不到的级无意义）。
    /// 观测口径 `texture_is_bc1/3/7` 按容器族置位，两条路径语义统一。
    fn upload_compressed(
        &mut self,
        handle: TextureHandle,
        levels: &crate::texture::CompressedLevels,
    ) {
        use crate::texture_ktx2::Ktx2BcFormat;
        if levels.width % 4 != 0 || levels.height % 4 != 0 {
            log::warn!(
                "KTX2 纹理 {handle:?} 基级 {}×{} 非 4 对齐，绑白色兜底（压缩纹理块对齐保守口径）",
                levels.width,
                levels.height
            );
            return;
        }
        // 逐级维数 ≥4 才上传（链止于 4×4，与 `mip_chain_bc` 同口径；
        // 基级已过 4 对齐闸 ⇒ 至少 1 级，`max(1)` 只是防御性兜底）。
        let uploadable = levels
            .levels
            .iter()
            .enumerate()
            .take_while(|(i, _)| {
                let (lw, lh) = levels.level_extent(*i);
                lw >= 4 && lh >= 4
            })
            .count();
        let wgpu_format = match levels.format {
            Ktx2BcFormat::Bc1Unorm => wgpu::TextureFormat::Bc1RgbaUnorm,
            Ktx2BcFormat::Bc1Srgb => wgpu::TextureFormat::Bc1RgbaUnormSrgb,
            Ktx2BcFormat::Bc3Unorm => wgpu::TextureFormat::Bc3RgbaUnorm,
            Ktx2BcFormat::Bc3Srgb => wgpu::TextureFormat::Bc3RgbaUnormSrgb,
            Ktx2BcFormat::Bc7Unorm => wgpu::TextureFormat::Bc7RgbaUnorm,
            Ktx2BcFormat::Bc7Srgb => wgpu::TextureFormat::Bc7RgbaUnormSrgb,
        };
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.texture.ktx2"),
            size: wgpu::Extent3d {
                width: levels.width,
                height: levels.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: uploadable.max(1) as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let block = levels.format.block_bytes() as u32;
        for (level, data) in levels.levels.iter().enumerate().take(uploadable) {
            let (lw, lh) = levels.level_extent(level);
            let bytes_per_row = lw.div_ceil(4) * block;
            self.gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: lw,
                    height: lh,
                    depth_or_array_layers: 1,
                },
            );
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mips_on = self.mip_anisotropy.is_some();
        let sampler = self.gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("iris.sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: if mips_on {
                wgpu::MipmapFilterMode::Linear
            } else {
                wgpu::MipmapFilterMode::Nearest
            },
            anisotropy_clamp: self.mip_anisotropy.map_or(1, |a| (a as u16).clamp(1, 16)),
            ..Default::default()
        });
        self.texture_gpu.insert(
            handle.0,
            GpuTexture {
                view,
                sampler,
                bc1: matches!(
                    levels.format,
                    Ktx2BcFormat::Bc1Unorm | Ktx2BcFormat::Bc1Srgb
                ),
                bc3: matches!(
                    levels.format,
                    Ktx2BcFormat::Bc3Unorm | Ktx2BcFormat::Bc3Srgb
                ),
                bc7: matches!(
                    levels.format,
                    Ktx2BcFormat::Bc7Unorm | Ktx2BcFormat::Bc7Srgb
                ),
            },
        );
    }

    /// 形变目标解算（C4a）：带权重快照且网格有 morph 表的蒙皮项，CPU
    /// 混合出本帧顶点并上传逐实体顶点缓冲，填 `vertex_override`。
    ///
    /// 脏检：容量足够且权重位图案与上次上传一致 ⇒ **不重混合、不重
    /// 上传**（缓冲仍被复用，覆盖照常生效）——「静态权重 ⇒ 逐帧字节
    /// 不变」的口径由 `morph_uploads` 观测计数钉住。
    ///
    /// 缺席语义：`morph_weights` 为 None（组件缺席）或网格无 morph 表
    /// ⇒ 项保持 `vertex_override = None`，绑共享网格静态 VBO，现路径
    /// 逐位不变（v1 只对蒙皮路径生效的裁剪见 `morph.rs` 模块文档）。
    fn resolve_morphs(
        &mut self,
        mesh_store: &MeshStore,
        skinned: &mut [SkinnedItem],
        transparent_skinned: &mut [TransparentSkinnedItem],
    ) {
        for item in skinned
            .iter_mut()
            .chain(transparent_skinned.iter_mut().map(|t| &mut t.skinned))
        {
            let Some(weights) = item.morph_weights.take() else {
                continue;
            };
            let Some(base) = mesh_store.get(MeshHandle(item.mesh)) else {
                continue;
            };
            let Some(morph) = base.morph.as_ref() else {
                continue;
            };
            let bytes: u64 =
                (base.vertices.len() * std::mem::size_of::<crate::mesh::Vertex>()) as u64;
            let key = item.skin_entity;
            let bits: Vec<u32> = weights.iter().map(|w| w.to_bits()).collect();
            let clean = self
                .morph_gpu
                .get(&key)
                .is_some_and(|entry| entry.last_weights == bits && bytes <= entry.capacity_bytes);
            if clean {
                // 权重位图案未变 ⇒ 不重混合、不重上传（缓冲内容照常生效）
                let entry = self.morph_gpu.get(&key).expect("上查在场");
                item.vertex_override = Some(entry.vertex_buf.clone());
                continue;
            }
            let blended = crate::morph::blend_morph_vertices(&base.vertices, morph, &weights);
            let mut dirty = false;
            if !self.morph_gpu.contains_key(&key) {
                let vertex_buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("iris.morph.vbo"),
                    // 空网格兜底 4 字节（wgpu 零大小缓冲的常规口径）
                    size: bytes.max(4),
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                self.morph_gpu.insert(
                    key,
                    MorphGpu {
                        vertex_buf,
                        capacity_bytes: bytes.max(4),
                        last_weights: Vec::new(),
                    },
                );
                dirty = true;
            }
            let entry = self.morph_gpu.get_mut(&key).expect("形变缓冲刚建好必在场");
            if bytes > entry.capacity_bytes {
                // 容量不足（换网格句柄 / LOD 切换顶点数变大）：整体重建
                entry.vertex_buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("iris.morph.vbo"),
                    size: bytes.max(4),
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                entry.capacity_bytes = bytes.max(4);
                dirty = true;
            }
            if dirty || entry.last_weights != bits {
                if bytes > 0 {
                    self.gpu.queue.write_buffer(
                        &entry.vertex_buf,
                        0,
                        bytemuck::cast_slice(&blended),
                    );
                }
                entry.last_weights.clone_from(&bits);
                self.morph_uploads += 1;
            }
            item.vertex_override = Some(entry.vertex_buf.clone());
        }
    }

    /// 形变上传帧次累计观测（C4a，测试口径）。
    pub fn morph_uploads(&self) -> u64 {
        self.morph_uploads
    }

    fn ensure_texture(&mut self, store: Option<&TextureStore>, handle: TextureHandle) {
        // 0 号纹理是渲染器内置的白色兜底纹理，无需上传
        if handle.0 == 0 || self.texture_gpu.contains_key(&handle.0) {
            return;
        }
        let Some(cpu) = store.and_then(|s| s.get(handle)) else {
            log::warn!("纹理句柄 {handle:?} 不存在，使用白色兜底纹理");
            return;
        };
        let width = cpu.width.max(1);
        let height = cpu.height.max(1);
        // KTX2 预压缩纹理（C5a-2）：容器已带块数据，短路 RGBA8/编码
        // 决策树直传 GPU——预压缩资产的存在意义就是免运行时编码。
        // 无 BC 特性设备：容器不带 RGBA8 回退数据 ⇒ 告警后返回（不
        // 入 texture_gpu，绑定层落到白色兜底纹理，与「句柄不存在」同
        // 一路径；这是 §8.63/texture_ktx2 限制④的诚实形态）。
        if let Some(levels) = store.and_then(|s| s.compressed_levels(handle)) {
            if !self.gpu.supports_bc() {
                log::warn!(
                    "纹理 {handle:?} 是 KTX2 预压缩（{:?}）但设备无 BC 特性，绑白色兜底",
                    levels.format
                );
                return;
            }
            self.upload_compressed(handle, levels);
            return;
        }
        // R30 BC1 压缩决策树（决策纯函数与 TextureStore::gpu_bytes_estimate
        // 共用）：特性启用 + 尺寸 4 对齐 + 全不透明 → 8 倍显存节省；
        // 任一不满足回退 RGBA8（web / 无特性 / 非对齐 / 半透明自动命中，
        // 无 cfg 分支）。mipmap v1 无——单 mip 上传，`mip_level_count` 恒 1。
        // R42 色彩空间：`srgb == false`（法线 / ORM 数据贴图）上传为线性
        // 格式（采样即原值）；`true`（颜色数据）维持既有 sRGB 格式。
        //
        // R59 BC3 分层：**只对经理入期 manifest 声明的纹理**启用
        // （`TextureStore` 声明表是准入闸门——未声明纹理的决策树与
        // R59 之前逐位一致，全部既有 GPU 基线零变化）；声明过的带
        // alpha 纹理走 BC3（1 B/px），全不透明仍归 BC1（分层互斥，
        // 见 `bc3_eligible` / `bc1_eligible` 文档）。
        let declared_bc3 = store.is_some_and(|s| s.is_bc3_declared(handle));
        // BC7 声明（C5a）：**永不自动**——只有显式声明（manifest
        // `compression: "bc7"` / `declare_bc7`）的纹理进入本路径；与
        // BC3 双声明时 BC7 赢（同 1 B/px、质量更高）。
        let declared_bc7 = store.is_some_and(|s| s.is_bc7_declared(handle));
        // 无压缩声明（R66e）：数据贴图（法线 / ORM）质量优先，跳过 BC 块
        // 压缩（BC1 对法线的近距块状伪梯度，frictions ⑨）；未声明 = 既有
        // 决策树逐位不变
        let declared_raw = store.is_some_and(|s| s.is_uncompressed_declared(handle));
        let use_bc7 = !declared_raw
            && declared_bc7
            && crate::texture_bc7::bc7_eligible(width, height, &cpu.rgba, self.gpu.supports_bc());
        if use_bc7 {
            log::info!(
                "纹理 {handle:?}（{width}×{height}）经声明走 BC7（Mode 6）压缩上传（显存 1/4）"
            );
        }
        let use_bc3 = !declared_raw
            && !use_bc7
            && declared_bc3
            && crate::texture_bcn::bc3_eligible(width, height, &cpu.rgba, self.gpu.supports_bc());
        if use_bc3 {
            log::info!("纹理 {handle:?}（{width}×{height}）经声明走 BC3 压缩上传（显存 1/4）");
        }
        let use_bc1 = !declared_raw
            && !use_bc7
            && !use_bc3
            && crate::texture_bcn::bc1_eligible(width, height, &cpu.rgba, self.gpu.supports_bc());
        if use_bc1 {
            log::info!("纹理 {handle:?}（{width}×{height}）走 BC1 压缩上传（显存 1/8）");
        }
        // 压缩路径：数据为 4×4 块流，bytes_per_row 按块计（BC7 / BC3
        // 宽/4 块 × 16B、BC1 × 8B）；回退路径：RGBA8 逐字节（Cow 借用
        // 源数据，零拷贝）。
        let (format, data, bytes_per_row, path) = if use_bc7 {
            let blocks = crate::texture_bc7::encode_bc7(&cpu.rgba, width, height);
            debug_assert_eq!(
                blocks.len() as u64,
                crate::texture_bc7::encoded_len_bc7(width, height)
            );
            (
                if cpu.srgb {
                    wgpu::TextureFormat::Bc7RgbaUnormSrgb
                } else {
                    wgpu::TextureFormat::Bc7RgbaUnorm
                },
                std::borrow::Cow::Owned(blocks),
                (width / 4) * 16,
                CompressedPath::Bc7,
            )
        } else if use_bc3 {
            let blocks = crate::texture_bcn::encode_bc3(&cpu.rgba, width, height);
            debug_assert_eq!(
                blocks.len() as u64,
                crate::texture_bcn::encoded_len_bc3(width, height)
            );
            (
                if cpu.srgb {
                    wgpu::TextureFormat::Bc3RgbaUnormSrgb
                } else {
                    wgpu::TextureFormat::Bc3RgbaUnorm
                },
                std::borrow::Cow::Owned(blocks),
                (width / 4) * 16,
                CompressedPath::Bc3,
            )
        } else if use_bc1 {
            let blocks = crate::texture_bcn::encode_bc1(&cpu.rgba, width, height);
            debug_assert_eq!(
                blocks.len() as u64,
                crate::texture_bcn::encoded_len(width, height)
            );
            (
                if cpu.srgb {
                    wgpu::TextureFormat::Bc1RgbaUnormSrgb
                } else {
                    wgpu::TextureFormat::Bc1RgbaUnorm
                },
                std::borrow::Cow::Owned(blocks),
                (width / 4) * 8,
                CompressedPath::Bc1,
            )
        } else {
            (
                if cpu.srgb {
                    wgpu::TextureFormat::Rgba8UnormSrgb
                } else {
                    wgpu::TextureFormat::Rgba8Unorm
                },
                std::borrow::Cow::Borrowed(&cpu.rgba[..]),
                width * 4,
                CompressedPath::Rgba8,
            )
        };
        // MipSettings 在场（R66 / R66d）：三条上传路径都生成完整 CPU mip
        // 链——RGBA8 回退路径直接 box 链；BC1 / BC3 压缩路径逐级 RGBA8
        // 降采样后编码成块流链（R66d：法线 / ORM 等全不透明数据贴图命中
        // BC1，R66 只做 RGBA8 链时恒单 mip，远距无法线平滑出点状噪点，
        // iris-exp frictions ⑧）。**MipSettings 缺席 = 空 Vec = 单 mip，
        // 与 R66 之前逐位一致**。1×1 纹理链长恒 1，天然落入单 mip 形态，
        // 白色兜底（句柄 0）根本不进本函数。
        let mip_levels: Vec<Vec<u8>> = if self.mip_anisotropy.is_some() && !cpu.rgba.is_empty() {
            match path {
                CompressedPath::Bc7 => crate::texture::mip_chain_bc7(width, height, &cpu.rgba),
                CompressedPath::Bc3 => crate::texture::mip_chain_bc(width, height, &cpu.rgba, true),
                CompressedPath::Bc1 => {
                    crate::texture::mip_chain_bc(width, height, &cpu.rgba, false)
                }
                CompressedPath::Rgba8 => crate::texture::mip_chain_rgba8(width, height, &cpu.rgba),
            }
        } else {
            Vec::new()
        };
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: match path {
                CompressedPath::Bc7 => Some("iris.texture.bc7"),
                CompressedPath::Bc3 => Some("iris.texture.bc3"),
                CompressedPath::Bc1 => Some("iris.texture.bc1"),
                CompressedPath::Rgba8 => Some("iris.texture"),
            },
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: mip_levels.len().max(1) as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        if mip_levels.is_empty() {
            self.gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        } else {
            // 逐级上传：第 0 级 = 源数据，每级维数 max(1, dim >> level)；
            // 行步长按路径——RGBA8 = 级宽 × 4；BC 块流 = 补边块数 × 块
            // 字节（BC1 8 / BC3 16；queue.write_texture 无 256 字节对齐
            // 要求）
            for (level, bytes) in mip_levels.iter().enumerate() {
                let level_w = (width >> level).max(1);
                let level_h = (height >> level).max(1);
                let (level_row, level_bytes) = match path {
                    CompressedPath::Bc1 => (
                        level_w.div_ceil(4) * 8,
                        level_w.div_ceil(4) * level_h.div_ceil(4) * 8,
                    ),
                    // BC7 与 BC3 同为 16 B/4×4 块（C5a）
                    CompressedPath::Bc3 | CompressedPath::Bc7 => (
                        level_w.div_ceil(4) * 16,
                        level_w.div_ceil(4) * level_h.div_ceil(4) * 16,
                    ),
                    CompressedPath::Rgba8 => (level_w * 4, level_w * level_h * 4),
                };
                debug_assert_eq!(bytes.len(), level_bytes as usize);
                self.gpu.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &texture,
                        mip_level: level as u32,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    bytes,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(level_row),
                        rows_per_image: None,
                    },
                    wgpu::Extent3d {
                        width: level_w,
                        height: level_h,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // 采样器（R66）：MipSettings 缺席 = 逐字段与既有采样器一致
        // （Nearest mip 过滤 + anisotropy_clamp 1——Default 值）；在场 =
        // 三线性 + 各向异性（u8 值夹到 wgpu hal 接口的 [1, 16]；适配器
        // 不支持各向异性时 wgpu-core 内部再收敛为 1）。随纹理缓存按
        // 句柄持有，上传时生效、运行中修改资源不追溯。
        let mips_on = self.mip_anisotropy.is_some();
        let sampler = self.gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("iris.sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: if mips_on {
                wgpu::MipmapFilterMode::Linear
            } else {
                wgpu::MipmapFilterMode::Nearest
            },
            anisotropy_clamp: self.mip_anisotropy.map_or(1, |a| (a as u16).clamp(1, 16)),
            ..Default::default()
        });
        self.texture_gpu.insert(
            handle.0,
            GpuTexture {
                view,
                sampler,
                bc1: path == CompressedPath::Bc1,
                bc3: path == CompressedPath::Bc3,
                bc7: path == CompressedPath::Bc7,
            },
        );
    }

    /// 该纹理句柄的 GPU 上传是否走了 BC1 压缩（R30）。未上传（懒上传未
    /// 触发）/ 不存在 / 回退 RGBA8 均返回 `false`。测试与内存对账用。
    pub fn texture_is_bc1(&self, handle: TextureHandle) -> bool {
        self.texture_gpu.get(&handle.0).is_some_and(|t| t.bc1)
    }

    /// 该纹理句柄的 GPU 上传是否走了 BC3 压缩（R59）：只对经理入期
    /// manifest 声明且带 alpha 的纹理成立。未上传 / 不存在 / 回退 / 走
    /// BC1 均返回 `false`（与 [`Self::texture_is_bc1`] 恒互斥）。测试与
    /// 内存对账用。
    pub fn texture_is_bc3(&self, handle: TextureHandle) -> bool {
        self.texture_gpu.get(&handle.0).is_some_and(|t| t.bc3)
    }

    /// 该纹理句柄的 GPU 上传是否走了 BC7 压缩（C5a）：只对经**显式声明**
    /// （manifest `compression: "bc7"` 或 `TextureStore::declare_bc7`）的
    /// 纹理成立——BC7 永不自动。未上传 / 不存在 / 回退 RGBA8 均 `false`
    /// （与 `texture_is_bc1` / `texture_is_bc3` 三态恒互斥）。
    pub fn texture_is_bc7(&self, handle: TextureHandle) -> bool {
        self.texture_gpu.get(&handle.0).is_some_and(|t| t.bc7)
    }

    /// 光照贴图图集的懒上传（R52c）：`Rgba16Float` 线性 HDR 纹理，数据源
    /// = [`LightmapItem::half_bytes`]（已 dilation 填充，见 `lightmap.rs`）。
    ///
    /// [`NO_LIGHTMAP`] 与已上传句柄直接返回；句柄悬垂只告警不上传——
    /// 调用方（`ensure_material`）据此把掩码留在 0，间接光退回 IBL 路径
    /// （比绑黑纹理把间接光抹掉更合理）。
    fn ensure_lightmap(&mut self, store: Option<&LightmapStore>, handle: u32) {
        if handle == NO_LIGHTMAP || self.lightmap_gpu.contains_key(&handle) {
            return;
        }
        let Some(item) = store.and_then(|s| s.get(LightmapHandle(handle))) else {
            log::warn!("光照贴图句柄 {handle} 不存在，该材质退回 IBL 间接光");
            return;
        };
        let size = item.size().max(1);
        // C2（§8.31⑤ 闭合）：MipSettings 在场 = 线性域 mip 链多级上传 +
        // 三线性采样器；缺席 = 单 mip + 采样器逐字段与 R52c 常驻份一致
        // （既有基线逐位不变的兼容形态，与 R66 纹理路径同纪律）。
        let chain = self.mip_anisotropy.map(|_| item.mip_chain_half_bytes());
        let levels = chain.as_ref().map_or(1, |c| c.len() as u32);
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.lightmap.atlas"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // queue.write_texture 无 256 字节行对齐要求（那是 copy_buffer_to_texture
        // 的规则），行步长直接给 size × 8 B（与既有 RGBA8 上传同款做法）。
        // 逐级上传：级 i 尺寸 = div_ceil 序列（与 downsample_rgb 同口径）。
        let mut w = size;
        let mut h = size;
        match &chain {
            Some(bytes) => {
                for (level, data) in bytes.iter().enumerate() {
                    self.gpu.queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &texture,
                            mip_level: level as u32,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        data,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(w * 8),
                            rows_per_image: None,
                        },
                        wgpu::Extent3d {
                            width: w,
                            height: h,
                            depth_or_array_layers: 1,
                        },
                    );
                    w = (w / 2).max(1);
                    h = (h / 2).max(1);
                }
            }
            None => self.gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &item.half_bytes(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(size * 8),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
            ),
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // 采样器（C2）：缺席分支逐字段 = 常驻 lightmap_sampler（双线性 +
        // ClampToEdge + mip Nearest + 各向异性 1）；在场 = 三线性 +
        // 各向异性 min(资源值, [1,16] 夹取)，与 ensure_texture 同口径。
        let sampler = self.gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("iris.lightmap.sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: if chain.is_some() {
                wgpu::MipmapFilterMode::Linear
            } else {
                wgpu::MipmapFilterMode::Nearest
            },
            anisotropy_clamp: self.mip_anisotropy.map_or(1, |a| (a as u16).clamp(1, 16)),
            ..Default::default()
        });
        self.lightmap_gpu.insert(
            handle,
            GpuLightmap {
                texture,
                view,
                sampler,
                mips: chain.is_some(),
            },
        );
    }

    /// 该光照贴图句柄的 GPU 上传是否完成（R52c）。测试与调试观测口径：
    /// 未上传（懒上传未触发）/ 悬垂句柄 / [`NO_LIGHTMAP`] 均为 `false`。
    pub fn lightmap_is_uploaded(&self, handle: LightmapHandle) -> bool {
        self.lightmap_gpu.contains_key(&handle.0)
    }

    /// 该光照贴图句柄的 GPU 上传是否走 **mip 链**（C2）：仅当上传发生在
    /// `MipSettings` 在场帧才为 `true`（上传期口径，句柄缓存不追溯——与
    /// `texture_is_bc1` 同纪律）。未上传 / 悬垂 / `NO_LIGHTMAP` ⇒ `false`。
    pub fn lightmap_has_mips(&self, handle: LightmapHandle) -> bool {
        self.lightmap_gpu.get(&handle.0).is_some_and(|lm| lm.mips)
    }

    /// 材质绑定组（组 1）的懒构建：材质参数 uniform + base / 法线 / ORM
    /// 纹理 + **光照贴图图集**（R52c，binding 5/6）。
    ///
    /// 缓存键 = [`material_key`]`(材质, 图集)`：同一材质配不同图集时掩码
    /// （`surface.w`）与图集纹理都不同，必须各建一组；无光照贴图时槽位
    /// 恒 0，与 R52b 的按材质缓存一一对应。
    fn ensure_material(
        &mut self,
        materials: &MaterialStore,
        textures: Option<&TextureStore>,
        lightmaps: Option<&LightmapStore>,
        handle: MaterialHandle,
        lightmap: u32,
    ) {
        let key = material_key(handle.0, lightmap);
        if self.material_gpu.contains_key(&key) {
            return;
        }
        let Some(def) = materials.get(handle) else {
            log::warn!("材质句柄 {handle:?} 不存在");
            return;
        };
        self.ensure_texture(textures, def.texture);
        // 法线 / ORM 槽位（R42）：Some 懒上传后用真实纹理；None 绑中性
        // 常驻纹理（法线 (0.5,0.5,1) / ORM 白色 1.0——白色兜底纹理采样
        // 恒 1.0，与中性 ORM 语义一致）。三张贴图共用 base 采样器。
        let normal_handle = def.normal_texture;
        let orm_handle = def.orm_texture;
        if let Some(tex) = normal_handle {
            self.ensure_texture(textures, tex);
        }
        if let Some(tex) = orm_handle {
            self.ensure_texture(textures, tex);
        }
        // 光照贴图（R52c）：懒上传后取真实图集视图；缺席 / 悬垂绑常驻
        // 1×1 黑纹理。采样器全图集共用一份常驻。
        self.ensure_lightmap(lightmaps, lightmap);
        let texture = &self.texture_gpu[&def.texture.0];
        let normal_view = match normal_handle {
            Some(tex) => &self.texture_gpu[&tex.0].view,
            None => &self.neutral_normal_view,
        };
        let orm_view = match orm_handle {
            Some(tex) => &self.texture_gpu[&tex.0].view,
            None => &self.texture_gpu[&0].view,
        };
        let (lightmap_view, lightmap_sampler) = match self.lightmap_gpu.get(&lightmap) {
            // C2：采样器随图集上传期口径（mip 在场 = 三线性；缺席 =
            // 与常驻份逐字段一致）。
            Some(gpu_lightmap) => (&gpu_lightmap.view, &gpu_lightmap.sampler),
            None => (&self.lightmap_dummy_view, &self.lightmap_sampler),
        };
        // 掩码只在图集**确实已上传**时置 1：悬垂句柄（ensure_lightmap 只
        // 告警）保持 0 ⇒ 间接光退回 IBL，而不是被黑纹理抹成零。
        let mask = f32::from(self.lightmap_gpu.contains_key(&lightmap));
        // 预积分 SSS（R63）：强度 clamp 到 [0, 1]；曲率尺度**在 CPU 换算**
        // 成 LUT 的 v 坐标（`sss::wrap_param`，纯函数 + 单测锁定调参表）
        // ——片元侧不做除法，也不做非法的尺度判定。强度 0 时 y 的取值
        // 无关紧要（着色器分支不进入），但仍按同一公式算，保证同一材质
        // 在任意强度下的 v 坐标一致（无「先开后关再开」的坐标跳变）。
        let raw = MaterialRaw {
            base_color: def.base_color,
            emissive: def.emissive,
            surface: [
                def.metallic.clamp(0.0, 1.0),
                def.roughness.clamp(0.0, 1.0),
                0.04, // 绝缘体反射率基准（glTF 规范约定值）
                mask,
            ],
            sss: [
                def.subsurface_strength.clamp(0.0, 1.0),
                crate::sss::wrap_param(def.subsurface_scale),
                0.0,
                0.0,
            ],
        };
        let params_buf = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.material.params"),
            size: std::mem::size_of::<MaterialRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.gpu
            .queue
            .write_buffer(&params_buf, 0, bytemuck::bytes_of(&raw));
        let bind_group = self
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("iris.material.bg"),
                layout: &self.bundle.material_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: params_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&texture.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&texture.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(normal_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(orm_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(lightmap_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: wgpu::BindingResource::Sampler(lightmap_sampler),
                    },
                    // 预积分 SSS LUT（R63，常驻纹理）：全部材质绑定同一份
                    // ——是否消费由 `material.sss.x > 0` 决定。
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: wgpu::BindingResource::TextureView(&self.sss_lut_view),
                    },
                ],
            });
        self.material_gpu.insert(key, GpuMaterial { bind_group });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造仅平移 x 分量不同的实例数据（其余字段保持单位矩阵）。
    fn raw(x: f32) -> InstanceRaw {
        let mut r = InstanceRaw::from_matrix(glam::Mat4::IDENTITY);
        r.model[3][0] = x;
        r
    }

    /// `collect_instances` 的测试便捷封装：视图恒等、eye 在原点、材质目录
    /// 只有 0 号默认材质（句柄缺席按不透明，与既有测试语义一致），透明列表丢弃。
    fn collect_opaque(
        world: &iris_core::world::World,
        mesh_store: &MeshStore,
        frustum: &Frustum,
        items: &mut Vec<DrawItem>,
        skinned: &mut Vec<SkinnedItem>,
    ) -> CollectCounts {
        let materials = MaterialStore::new();
        let mut transparent = Vec::new();
        let mut transparent_skinned = Vec::new();
        collect_instances(
            world,
            mesh_store,
            &materials,
            &glam::Mat4::IDENTITY,
            Vec3::ZERO,
            frustum,
            items,
            skinned,
            &mut transparent,
            &mut transparent_skinned,
            None,
            None,
        )
    }

    /// Visibility 过滤：隐藏实体不出现在实例收集结果（主 pass 与全部
    /// 阴影 pass 共用这份收集，故一条测试覆盖三个 pass 的过滤语义）。
    /// 组件缺席 = 可见；蒙皮路径同样生效。
    #[test]
    fn hidden_entities_are_skipped_in_instance_collection() {
        use crate::mesh::primitives;
        use crate::visibility::Visibility;
        use iris_core::world::World;

        let mut mesh_store = MeshStore::new();
        let mesh = mesh_store.add(primitives::cube()); // MeshHandle(0)
        let mut world = World::new();

        let spawn_plain = |world: &mut World, material: u32, visibility: Option<Visibility>| {
            let entity = world.spawn((
                iris_core::prelude::GlobalTransform::from_matrix(glam::Mat4::IDENTITY),
                mesh,
                MaterialHandle(material),
            ));
            if let Some(v) = visibility {
                world.insert(entity, v);
            }
            entity
        };
        // 普通实体：缺席 / 显式 VISIBLE / HIDDEN（按材质编号区分收集结果）
        spawn_plain(&mut world, 1, None);
        spawn_plain(&mut world, 2, Some(Visibility::VISIBLE));
        spawn_plain(&mut world, 3, Some(Visibility::HIDDEN));

        // 蒙皮实体一对：可见 + 隐藏（关节实体需带 GlobalTransform）
        let joint = world.spawn((iris_core::prelude::GlobalTransform::from_matrix(
            glam::Mat4::IDENTITY,
        ),));
        let skin = Skin::new(vec![joint], vec![glam::Mat4::IDENTITY]);
        let spawn_skinned = |world: &mut World, material: u32, visibility: Option<Visibility>| {
            let entity = world.spawn((
                iris_core::prelude::GlobalTransform::from_matrix(glam::Mat4::IDENTITY),
                mesh,
                MaterialHandle(material),
                skin.clone(),
            ));
            if let Some(v) = visibility {
                world.insert(entity, v);
            }
            entity
        };
        spawn_skinned(&mut world, 5, None);
        spawn_skinned(&mut world, 6, Some(Visibility::HIDDEN));

        // 恒等矩阵提取的视锥包含原点附近的全部包围球（不产生额外剔除，
        // 测试只观察 Visibility 过滤）
        let frustum = Frustum::from_view_proj(&glam::Mat4::IDENTITY);
        let mut items = Vec::new();
        let mut skinned = Vec::new();
        let counts = collect_opaque(&world, &mesh_store, &frustum, &mut items, &mut skinned);

        assert_eq!(counts.culled, 0);
        assert_eq!(counts.visible, items.len() + skinned.len());
        let keys: std::collections::HashSet<u64> = items.iter().map(|i| i.key).collect();
        assert!(keys.contains(&(1u64)), "缺席 Visibility 的实体应可见");
        assert!(keys.contains(&(2u64)), "显式 VISIBLE 应可见");
        assert!(!keys.contains(&(3u64)), "HIDDEN 不应被收集");
        assert_eq!(skinned.len(), 1, "隐藏的蒙皮实体不应被收集");
        assert_eq!(skinned[0].material, 5);
    }

    /// 蒙皮保守剔除跟随关节变换（反误杀核心，CPU 级）：绑定姿势 AABB 在
    /// 原点附近，但关节矩阵把几何刚性移到 (10, 0, 0)——只看向 (10, 0, 0)
    /// 的相机必须看到该实例（误用绑定姿势包围盒剔除会在这里误杀），
    /// 背对的相机必须把它计入剔除数。
    #[test]
    fn skinned_culling_follows_joint_transformed_bounds() {
        use crate::mesh::primitives;
        use iris_core::prelude::Transform;
        use iris_core::world::World;

        let mut mesh_store = MeshStore::new();
        let mesh = mesh_store.add(primitives::cube()); // 绑定 AABB [-0.5, 0.5]³
        let mut world = World::new();

        // 关节全局平移 (10, 0, 0)、ibm = 恒等、蒙皮实体在原点：
        // J = T(10,0,0)，蒙皮后几何出现在 (10, 0, 0) 附近
        let joint = world.spawn((Transform::from_xyz(10.0, 0.0, 0.0),));
        let skin = Skin::new(vec![joint], vec![glam::Mat4::IDENTITY]);
        world.spawn((
            iris_core::prelude::GlobalTransform::from_matrix(glam::Mat4::IDENTITY),
            mesh,
            MaterialHandle(1),
            skin,
        ));
        iris_core::hierarchy::propagate_transforms(&mut world);

        // 相机在 (10, 0, 20) 看向 (10, 0, 0)：绑定 AABB（原点）在视锥外
        // （横向偏移 10 > 视锥半宽 ~8.3），蒙皮几何在视锥内
        let toward = Frustum::from_view_proj(&{
            let view = glam::camera::rh::view::look_at_mat4(
                Vec3::new(10.0, 0.0, 20.0),
                Vec3::new(10.0, 0.0, 0.0),
                Vec3::Y,
            );
            let proj = glam::camera::rh::proj::directx::perspective(
                std::f32::consts::FRAC_PI_4,
                1.0,
                0.1,
                1000.0,
            );
            proj * view
        });
        let mut items = Vec::new();
        let mut skinned = Vec::new();
        let counts = collect_opaque(&world, &mesh_store, &toward, &mut items, &mut skinned);
        assert_eq!(counts.visible, 1, "蒙皮几何被关节移入视锥，必须可见");
        assert_eq!(counts.culled, 0);
        assert_eq!(skinned.len(), 1);

        // 同一相机看向 +Z（背对蒙皮几何与绑定 AABB）：剔除数计 1
        let away = Frustum::from_view_proj(&{
            let view = glam::camera::rh::view::look_at_mat4(
                Vec3::new(10.0, 0.0, 20.0),
                Vec3::new(10.0, 0.0, 40.0),
                Vec3::Y,
            );
            let proj = glam::camera::rh::proj::directx::perspective(
                std::f32::consts::FRAC_PI_4,
                1.0,
                0.1,
                1000.0,
            );
            proj * view
        });
        items.clear();
        skinned.clear();
        let counts = collect_opaque(&world, &mesh_store, &away, &mut items, &mut skinned);
        assert_eq!(counts.visible, 0, "背对时蒙皮实例应被剔除");
        assert_eq!(counts.culled, 1);
        assert!(skinned.is_empty());
    }

    /// 距离 LOD 收集（R30 CPU 级）：挂 `LodMesh` 的实体按「实体原点到
    /// eye 的欧氏距离」选择参与收集的网格——近处选 high、远处选 low，
    /// 合批键 / 剔除 / 统计全部按选中的网格算；未挂 `LodMesh` 的实体走
    /// `MeshHandle` 旧语义不受影响（GPU 端 triangles_drawn 验收见
    /// `lod_render.rs`）。
    #[test]
    fn lod_entities_select_mesh_by_distance_in_collection() {
        use crate::mesh::primitives;
        use iris_core::prelude::Transform;
        use iris_core::world::World;

        let mut mesh_store = MeshStore::new();
        let high = mesh_store.add(primitives::uv_sphere(1.0, 24, 16)); // MeshHandle(0)
        let low = mesh_store.add(primitives::cube()); // MeshHandle(1)
        let plain = mesh_store.add(primitives::cube()); // MeshHandle(2)

        let mut world = World::new();
        // 近处 LOD 实体（距原点 5 < 10 → high）+ 远处 LOD 实体（距原点
        // 50 > 10 → low）+ 普通 MeshHandle 实体（旧语义）
        world.spawn((
            Transform::from_xyz(0.0, 0.0, -5.0),
            LodMesh {
                high,
                low,
                switch_distance: 10.0,
            },
            MaterialHandle(1),
        ));
        world.spawn((
            Transform::from_xyz(0.0, 0.0, -50.0),
            LodMesh {
                high,
                low,
                switch_distance: 10.0,
            },
            MaterialHandle(2),
        ));
        world.spawn((
            Transform::from_xyz(0.0, 0.0, -5.0),
            plain,
            MaterialHandle(3),
        ));
        iris_core::hierarchy::propagate_transforms(&mut world);

        // eye 在原点（与 collect_opaque 硬编码一致）；宽视锥（90° / far 1000）
        // 覆盖 -5 与 -50 两个深度，避免视锥剔除混入本测试的观察
        let frustum = Frustum::from_view_proj(&{
            let view = glam::camera::rh::view::look_at_mat4(
                Vec3::ZERO,
                Vec3::new(0.0, 0.0, -1.0),
                Vec3::Y,
            );
            let proj = glam::camera::rh::proj::directx::perspective(
                std::f32::consts::FRAC_PI_2,
                1.0,
                0.1,
                1000.0,
            );
            proj * view
        });
        let mut items = Vec::new();
        let mut skinned = Vec::new();
        let counts = collect_opaque(&world, &mesh_store, &frustum, &mut items, &mut skinned);
        assert_eq!(counts.visible, 3);
        assert_eq!(counts.culled, 0);
        // 排序键高 32 位 = 网格句柄：三个网格必须各有一项
        let mesh_ids: std::collections::HashSet<u32> =
            items.iter().map(|i| (i.key >> 32) as u32).collect();
        assert!(mesh_ids.contains(&high.0), "近距实体应选 high 网格");
        assert!(mesh_ids.contains(&low.0), "远距实体应选 low 网格");
        assert!(
            mesh_ids.contains(&plain.0),
            "无 LodMesh 实体保持 MeshHandle 旧语义"
        );
    }

    /// 透明分区与视深排序（CPU 级）：Blend 材质进透明列表、Opaque 进不透明
    /// 列表；视深 = 包围球中心经视图变换后的 -z；排序降序（远 → 近）、
    /// 深度并列按实体 bits 升序。蒙皮 Blend 实体进透明蒙皮列表（不透明
    /// 蒙皮列表不受影响）。
    #[test]
    fn transparent_items_partition_and_sort_by_view_depth() {
        use crate::material::MaterialDef;
        use crate::mesh::primitives;
        use iris_core::prelude::Transform;
        use iris_core::world::World;

        let mut mesh_store = MeshStore::new();
        let mesh = mesh_store.add(primitives::cube()); // 包围球中心 = 实例位置
        let mut material_store = MaterialStore::new();
        let opaque_mat = material_store.add(MaterialDef::solid(1.0, 1.0, 1.0, 1.0));
        let blend_mat = material_store.add(MaterialDef::glass(0.5, 0.5, 1.0, 0.5));

        let mut world = World::new();
        // 相机在 +Z 看向原点：z 越小越远（视深 = 20 - z，降序 = z 升序）
        let view =
            glam::camera::rh::view::look_at_mat4(Vec3::new(0.0, 0.0, 20.0), Vec3::ZERO, Vec3::Y);

        // 三个透明立方体：近 z=2 / 中 z=0 / 远 z=-2（生成顺序故意与深度序
        // 相反——排序必须与查询顺序无关）
        let spawn = |world: &mut World, z: f32, mat| {
            world.spawn((Transform::from_xyz(0.0, 0.0, z), mesh, mat))
        };
        spawn(&mut world, 2.0, blend_mat); // 近
        spawn(&mut world, -2.0, blend_mat); // 远
        spawn(&mut world, 0.0, blend_mat); // 中
        // 一个不透明立方体混在其间：必须留在不透明列表
        spawn(&mut world, 1.0, opaque_mat);
        // 蒙皮透明实体：进透明蒙皮列表（关节恒等 → 并集 AABB = 绑定 AABB）
        let joint = world.spawn((Transform::IDENTITY,));
        let skin = Skin::new(vec![joint], vec![glam::Mat4::IDENTITY]);
        world.spawn((Transform::from_xyz(0.0, 0.0, 0.5), mesh, blend_mat, skin));
        iris_core::hierarchy::propagate_transforms(&mut world);

        let frustum = Frustum::from_view_proj(&{
            let proj = glam::camera::rh::proj::directx::perspective(
                std::f32::consts::FRAC_PI_4,
                1.0,
                0.1,
                1000.0,
            );
            proj * view
        });
        let mut items = Vec::new();
        let mut skinned = Vec::new();
        let mut transparent = Vec::new();
        let mut transparent_skinned = Vec::new();
        let counts = collect_instances(
            &world,
            &mesh_store,
            &material_store,
            &view,
            Vec3::new(0.0, 0.0, 20.0),
            &frustum,
            &mut items,
            &mut skinned,
            &mut transparent,
            &mut transparent_skinned,
            None,
            None,
        );
        assert_eq!(counts.visible, 5);
        assert_eq!(counts.culled, 0);
        assert_eq!(items.len(), 1, "不透明实体留在普通列表");
        assert!(skinned.is_empty(), "Blend 蒙皮实体不进不透明蒙皮列表");
        assert_eq!(transparent.len(), 3, "三个 Blend 普通实体进透明列表");
        assert_eq!(transparent_skinned.len(), 1, "Blend 蒙皮实体进透明蒙皮列表");

        // 视深数值：中心 z 经视图变换后的 -z（相机在 z=20，视深 = 20 - z）。
        // 逐项断言（不假设查询顺序）：三个深度值与解析值一一对应
        let mut depths: Vec<f32> = transparent.iter().map(|t| t.depth).collect();
        depths.sort_by(|a, b| a.total_cmp(b));
        let expected = {
            let mut e = vec![18.0f32, 20.0, 22.0]; // 20 - {2, 0, -2}
            e.sort_by(|a, b| a.total_cmp(b));
            e
        };
        for (got, want) in depths.iter().zip(&expected) {
            assert!(
                (got - want).abs() < 1e-4,
                "视深集合应为 {expected:?}，实际 {depths:?}"
            );
        }
        // 蒙皮透明视深取并集 AABB 中心：恒等关节下 J = inv(model)，
        // 蒙皮几何被拉回原点（蒙皮定义在网格局部空间），中心 = (0,0,0)
        let ts = &transparent_skinned[0];
        assert!((ts.depth - 20.0).abs() < 1e-4);

        // 排序（与 draw() 同款比较器）：远 → 近
        transparent.sort_unstable_by(|a, b| {
            b.depth
                .total_cmp(&a.depth)
                .then_with(|| a.entity.cmp(&b.entity))
        });
        let sorted_depths: Vec<f32> = transparent.iter().map(|t| t.depth).collect();
        assert_eq!(
            sorted_depths,
            vec![22.0, 20.0, 18.0],
            "透明项应按视深降序（远 → 近）排列"
        );
    }

    fn slice(xs: &[f32]) -> Vec<InstanceRaw> {
        xs.iter().map(|x| raw(*x)).collect()
    }

    fn ranges_of(prev: &[f32], next: &[f32]) -> Vec<std::ops::Range<usize>> {
        diff_changed_ranges(&slice(prev), &slice(next))
    }

    #[test]
    fn diff_identical_is_empty() {
        assert!(ranges_of(&[1.0, 2.0, 3.0], &[1.0, 2.0, 3.0]).is_empty());
    }

    #[test]
    fn diff_single_point_change() {
        assert_eq!(ranges_of(&[1.0, 2.0, 3.0], &[1.0, 9.0, 3.0]), vec![1..2]);
    }

    #[test]
    fn diff_first_and_last_changes_are_separate_ranges() {
        assert_eq!(
            ranges_of(&[1.0, 2.0, 3.0], &[9.0, 2.0, 9.0]),
            vec![0..1, 2..3]
        );
    }

    #[test]
    fn diff_adjacent_changes_merge() {
        // 交替「变 / 不变」：相邻变化各自成段，间隔的不变项把段分开
        assert_eq!(
            ranges_of(&[1.0, 1.0, 1.0, 1.0, 1.0], &[9.0, 9.0, 1.0, 9.0, 9.0]),
            vec![0..2, 3..5]
        );
    }

    #[test]
    fn diff_length_mismatch() {
        // next 更长：新增尾部整体为变化段
        assert_eq!(ranges_of(&[1.0, 2.0], &[1.0, 2.0, 3.0, 4.0]), vec![2..4]);
        // prev 更长：缩短部分不产生写入区间（调用方走全量上传）
        assert!(ranges_of(&[1.0, 2.0, 3.0], &[1.0, 2.0]).is_empty());
        assert_eq!(ranges_of(&[1.0, 2.0, 3.0], &[9.0, 2.0]), vec![0..1]);
    }

    #[test]
    fn diff_empty_to_nonempty_is_entire_range() {
        assert_eq!(ranges_of(&[], &[1.0, 2.0, 3.0]), vec![0..3]);
        assert!(ranges_of(&[1.0], &[]).is_empty());
    }
}
