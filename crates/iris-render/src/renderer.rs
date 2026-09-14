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
use crate::irradiance::IrradianceProbeSet;
use crate::light::{
    DirectionalLight, MAX_POINT_LIGHTS, POINT_SHADOW_BIAS_LEGACY, POINT_SHADOW_BIAS_MIN,
    POINT_SHADOW_BUDGET, POINT_SHADOW_NORMAL_SCALE, POINT_SHADOW_SLOPE_SCALE, PointLight,
    PointShadowCandidate, SHADOW_BIAS_LEGACY, SHADOW_BIAS_MIN, cascade_fit_params, cascade_splits,
    select_point_shadow_casters,
};
use crate::lightmap::{LightmapBinding, LightmapHandle, LightmapStore, NO_LIGHTMAP};
use crate::material::{AlphaMode, MaterialHandle, MaterialStore};
use crate::mesh::{LodMesh, MeshHandle, MeshStore, upload_mesh};
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
/// 视图。采样器全图集共用一份常驻（双线性 + ClampToEdge，与尺寸无关，
/// 照 `rt_blit_sampler` 的做法）。按句柄懒上传、跨帧持有（照 `GpuTexture`）。
struct GpuLightmap {
    /// 纹理本体保活（视图的有效性依赖它）。
    #[allow(dead_code)]
    texture: wgpu::Texture,
    view: wgpu::TextureView,
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
}

/// 单次纹理上传走的路径（`ensure_texture` 决策树的内部分类，标签与
/// 观测口径共用一个来源）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum CompressedPath {
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
    /// 专用全局与实例缓冲。按句柄惰性创建、跨帧持有（尺寸不变时零重建）。
    render_target_gpu: HashMap<u32, GpuRenderTarget>,
    /// RT 色调映射共享采样器 + 合成参数（泛光 0 / 曝光 1 / 暗角 0——RT
    /// 无后处理链，恒中性）。两者所有 RT 共用。
    rt_blit_sampler: wgpu::Sampler,
    rt_post_params_buf: wgpu::Buffer,
    /// 蒙皮实体位标识 → 关节矩阵缓冲。
    skin_gpu: HashMap<u64, GpuSkin>,
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

/// 与 WGSL `MbParams` 对齐（144 字节，POD）：运动模糊 pass 参数。
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

/// 与 WGSL `TaaParams` 对齐（144 字节，POD）：TAA pass 参数（R60）。
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
#[derive(Clone, Copy)]
struct ProbeGlobalsResources<'a> {
    params_buf: &'a wgpu::Buffer,
    cube_view: &'a wgpu::TextureView,
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

/// 全局绑定组里的 SSR 三路资源（R62b）：参数缓冲（binding 13）+ 上帧
/// 颜色副本视图（binding 11）+ 上帧深度副本视图（binding 12）。主视图
/// 传真实资源；探针捕获面 / RT 相机的绑定组传**零参数 + 哑纹理**（捕获
/// / RT 视口不做屏幕空间反射——上帧语义只对主相机成立，见 §4.31 裁决 2）。
#[derive(Clone, Copy)]
struct SsrGlobalsResources<'a> {
    params_buf: &'a wgpu::Buffer,
    color_view: &'a wgpu::TextureView,
    depth_view: &'a wgpu::TextureView,
}

fn build_globals_bind_group(
    gpu: &Gpu,
    bundle: &crate::pipeline::PipelineBundle,
    buffers: GlobalsBuffers<'_>,
    shadows: ShadowMapViews<'_>,
    env: &crate::environment::EnvironmentMaps,
    probe: ProbeGlobalsResources<'_>,
    ssr: SsrGlobalsResources<'_>,
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
                resource: wgpu::BindingResource::TextureView(ssr.color_view),
            },
            wgpu::BindGroupEntry {
                binding: 12,
                resource: wgpu::BindingResource::TextureView(ssr.depth_view),
            },
            wgpu::BindGroupEntry {
                binding: 13,
                resource: ssr.params_buf.as_entire_binding(),
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

/// LOD 网格解析（R30 / R51）：挂 [`LodMesh3`](crate::terrain::LodMesh3) 的实体按距离在 3 级间
/// 选（地形 chunk，见 `terrain.rs`）；否则挂 [`LodMesh`] 的实体按距离
/// 选 high / low（距离 = 实体原点到相机 eye 的欧氏距离，v1 不用包围球
/// 中心，取舍见组件文档）；未挂载回退 `MeshHandle` 旧语义；皆无返回
/// `None`（收集跳过该实体——查询带多个 `Option` 后「无任何网格句柄」
/// 成为合法查询命中）。
fn resolve_mesh_handle(
    lod: Option<&LodMesh>,
    lod3: Option<&crate::terrain::LodMesh3>,
    mesh: Option<&MeshHandle>,
    eye: Vec3,
    origin: Vec3,
) -> Option<MeshHandle> {
    match (lod3, lod, mesh) {
        (Some(lod3), ..) => Some(lod3.select(eye, origin)),
        (None, Some(lod), _) => Some(lod.select(eye, origin)),
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
/// **光照探针（R64）**：世界带 [`IrradianceProbeSet`] 资源时，对**无光照
/// 贴图**的实体按实体根 [`GlobalTransform`] 位置做一次四面体重心插值，
/// 结果写进实例载荷 `InstanceRaw::probe`（xyz = E/π、w = 标志位）；
/// 资源缺席 / 空集 / 实体挂图集 ⇒ 载荷恒零（着色器走原全局 env 路径，
/// 逐位不变）。逐实体 CPU 侧求值（不做逐片元 GPU 插值）的理由见
/// [`crate::irradiance`] 模块文档。
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
    // 光照探针集（R64）：CPU 侧逐实体插值 → 实例载荷（E/π + 标志位）。
    // 资源缺席 / 空集 ⇒ 恒 [0; 4]（着色器 select 取 env 采样值的同一串位）。
    // **光照贴图优先**：挂图集的实体走 R52c 的逐 texel 分支，CPU 侧就不再发
    // 标志位（两道保险：着色器那里 select 也是整项替换）。
    let probes = world.try_resource::<IrradianceProbeSet>();
    let probe_payload = |position: Vec3, lightmap: u32| -> [f32; 4] {
        if lightmap != NO_LIGHTMAP {
            return [0.0; 4];
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
    )>(|(e, gt, mat, mesh, lod, skin, visibility)| {
        // 自身 HIDDEN 跳过 + 祖先链级联（任一祖先隐藏 ⇒ 子树全隐藏）
        if visibility.is_some_and(|v| !v.visible) || hidden_by_ancestor(world, e) {
            return;
        }
        // LOD 距离选择：挂 LodMesh 按距离选 high / low，否则 MeshHandle；
        // 两者皆无（纯 Skin 实体，合法但不可渲染）静默跳过。
        // 蒙皮路径不参与 LodMesh3（地形 chunk 恒非蒙皮，R51）
        let Some(m) = resolve_mesh_handle(lod, None, mesh, eye, gt.position()) else {
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
        let Some(m) = resolve_mesh_handle(lod, lod3, mesh, eye, gt.position()) else {
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
fn collect_decals(
    world: &iris_core::world::World,
    frustum: &Frustum,
    out: &mut Vec<DecalItem>,
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
                instance: DecalInstanceRaw::from_model(model, decal.tint),
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
        // 主视图绑定组初建时的 SSR 三路：参数走**活跃**缓冲（此后由 draw
        // 逐帧管理），颜色 / 深度此时只有哑纹理（prev_color / scene_depth
        // 均未创建；二者就位时重建绑定组，见 draw 的接线）。
        let ssr_main_res = SsrGlobalsResources {
            params_buf: &ssr.params_buf,
            color_view: &ssr.dummy_color_view,
            depth_view: &ssr.dummy_depth_view,
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
        // SSR 三路同样走零参数 + 哑纹理（捕获不做屏幕空间反射）
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
                },
                SsrGlobalsResources {
                    params_buf: &ssr.off_params_buf,
                    color_view: &ssr.dummy_color_view,
                    depth_view: &ssr.dummy_depth_view,
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
            },
            ssr_main_res,
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
            rt_blit_sampler,
            rt_post_params_buf,
            skin_gpu: HashMap::new(),
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
            ProbeGlobalsResources {
                params_buf: &self.probe_params_buf,
                cube_view: &self.probe_cube_view,
            },
            self.ssr_main_resources(),
        );
    }

    /// SSR 绑定资源的「主相机」形态（R62b）：活跃参数缓冲 + 真实上帧
    /// 资源（尚未创建的一路回落哑纹理——flags = 0 时不会被采样；二者
    /// 就位时由 draw 重建绑定组）。
    fn ssr_main_resources(&self) -> SsrGlobalsResources<'_> {
        SsrGlobalsResources {
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
    pub fn reset_taa(&mut self) {
        self.taa_reset_pending = true;
        // SSR 跟随（R62b）：reset_taa 的语义是「宿主宣告切镜」，切镜让
        // 一切跨帧时域状态过期——TAA 历史与 SSR 的上帧颜色/深度同理
        // （上帧是另一个场景）。下一帧 SSR 整段跳过（输出 = 关闭帧），
        // 之后随帧尾副本恢复。成本是切镜帧的反射回退探针/env 一帧，
        // 收益是杜绝「新场景地板反射旧场景」的一帧闪现。
        self.ssr_prev_valid = false;
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
                    pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
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
                // 逆给出该像素在立方体面上的方向
                pass.set_pipeline(&self.bundle.probe_sky_pipeline);
                pass.set_bind_group(0, &self.probe_face_globals_bgs[face], &[]);
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
                        pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
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
        let occ_fresh = self.consume_occlusion_results();
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
        if vignette_strength > 0.0 || dof_on || mb_on {
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
        if ssr_needs_globals_rebuild {
            // 上帧颜色副本 / 场景深度纹理的视图变化 ⇒ 全局绑定组的
            // binding 11/12 需要重新指向（与 env 重烘同款重建路径）
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
        world.query::<(Entity, &GlobalTransform, &PointLight)>(|(e, gt, pl)| {
            if point_count >= MAX_POINT_LIGHTS {
                return;
            }
            let position = gt.position();
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
        if point_count == MAX_POINT_LIGHTS {
            // 有可能恰好还有更多点光被截断；再探一次数量仅用于日志
            let mut extra = 0;
            world.query::<&PointLight>(|_| extra += 1);
            if extra > MAX_POINT_LIGHTS {
                log::warn!("点光数量 {extra} 超过上限 {MAX_POINT_LIGHTS}，多余的已被忽略");
            }
        }
        let shadow_enabled = light.shadows;
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
        // 2d. 贴花收集（R39）：Visibility 过滤 + 祖先级联 + 变换后 AABB
        //     视锥剔除，随后按纹理句柄排序分批（同纹理连续区间 = 1 draw；
        //     批内实体 bits 并列序——确定性，与收集顺序无关）。无贴花时
        //     整个 pass 跳过（零开销默认路径）。
        let mut decal_items = std::mem::take(&mut self.decal_scratch);
        let decal_culled = collect_decals(world, &frustum, &mut decal_items);
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
        let mut occ_drawn = std::mem::take(&mut self.occ_drawn_scratch);
        let mut occ_aabbs = std::mem::take(&mut self.occ_aabb_scratch);
        let mut occ_entities = std::mem::take(&mut self.occ_entity_scratch);
        occ_drawn.clear();
        occ_aabbs.clear();
        occ_entities.clear();
        let mut occ_filtered = false;
        if occlusion_on && total > 0 {
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
            if occ_fresh && !self.occ_hidden.is_empty() {
                occ_drawn.resize(items.len(), true);
                let mut culled_now = 0u32;
                for (i, item) in items.iter().enumerate() {
                    if self.occ_hidden.contains(&item.entity) {
                        occ_drawn[i] = false;
                        culled_now += 1;
                    }
                }
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
                        shadow_pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
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
                                point_pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
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
            // 天空盒背景最先绘制（深度只读，几何体随后覆盖）
            pass.set_pipeline(&self.bundle.sky_pipeline);
            pass.set_bind_group(0, &self.globals_bg, &[]);
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
                    pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
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
                    pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
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
        if particle_total > 0 {
            let particle_buf = self.particle_buf.as_ref().expect("粒子实例缓冲已就绪");
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
            pass.set_bind_group(0, &self.globals_bg, &[]);
            pass.set_bind_group(1, &self.particle_camera_bg, &[]);
            pass.set_vertex_buffer(0, particle_buf.slice(..));
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
        if (dof_on || mb_on || taa_on || ssr_on) && opaque_total > 0 {
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
                    depth_pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
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
        // 全分辨率链（DOF / MB / 暗角）的槽位分配：纯函数单点定义（
        // post.rs，离线单测守护拓扑不变量）。TAA 开启不改变槽位拓扑，
        // 只把「主解析缓冲」这一源整体替换为本帧 TAA 输出（chain_source
        // 单点定义：有链头槽位取槽位，无链头时 TAA 开 = TAA 输出、关 =
        // 主解析缓冲——R59 拓扑原样）。
        let slots = crate::post::full_chain_slots(dof_on, mb_on, vignette_strength > 0.0);
        let scene_or_taa =
            |slot: Option<crate::post::PingPongSlot>| crate::post::chain_source(slot, taa_on);
        // 链头（泛光与暗角的源）：DOF/MB 输出槽、TAA 输出或主解析缓冲
        let head_input = scene_or_taa(slots.head);
        // 景深（DOF）：主解析缓冲 → 全分辨率 A 槽。CoC 按逐像素线性视距
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
                    // 源 = TAA 输出（TAA 开，链首）或主解析缓冲（TAA 关）
                    src: scene_or_taa(None),
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
        self.stats.draw_calls = draw_calls;
        self.stats.instances = instances;
        self.stats.triangles_drawn = triangles_drawn;
        self.stats.particles_drawn = particles_drawn;
        self.stats.taa_reset = u32::from(taa_on && taa_reset_frame);
        self.scratch = items;
        self.skinned_scratch = skinned;
        self.transparent_scratch = transparent;
        self.transparent_skinned_scratch = transparent_skinned;
        self.occ_drawn_scratch = occ_drawn;
        self.occ_aabb_scratch = occ_aabbs;
        self.occ_entity_scratch = occ_entities;
        self.decal_scratch = decal_items;
        self.particle_scratch = particle_instances;
        self.particle_alpha_scratch = particle_alpha;
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
            },
            SsrGlobalsResources {
                params_buf: &self.ssr.off_params_buf,
                color_view: &self.ssr.dummy_color_view,
                depth_view: &self.ssr.dummy_depth_view,
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
        }
    }

    /// 若句柄尚无 GPU 资源则构建并缓存；尺寸变化时**不重建**（v1 无
    /// resize，见 [`RenderTargetStore`] 文档——尺寸由 store 创建时决定）。
    fn ensure_render_target(&mut self, handle: RenderTargetHandle, width: u32, height: u32) {
        if !self.render_target_gpu.contains_key(&handle.0) {
            let rt = self.build_render_target(width, height);
            self.render_target_gpu.insert(handle.0, rt);
            log::debug!("渲染目标 {handle:?} GPU 资源已构建（{width}×{height}）");
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
                        pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
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
                            pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
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
        let use_bc3 = declared_bc3
            && crate::texture_bcn::bc3_eligible(width, height, &cpu.rgba, self.gpu.supports_bc());
        if use_bc3 {
            log::info!("纹理 {handle:?}（{width}×{height}）经声明走 BC3 压缩上传（显存 1/4）");
        }
        let use_bc1 = !use_bc3
            && crate::texture_bcn::bc1_eligible(width, height, &cpu.rgba, self.gpu.supports_bc());
        if use_bc1 {
            log::info!("纹理 {handle:?}（{width}×{height}）走 BC1 压缩上传（显存 1/8）");
        }
        // 压缩路径：数据为 4×4 块流，bytes_per_row 按块计（BC3 宽/4 块
        // × 16B、BC1 × 8B）；回退路径：RGBA8 逐字节（Cow 借用源数据，
        // 零拷贝）。
        let (format, data, bytes_per_row, path) = if use_bc3 {
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
                    CompressedPath::Bc3 => (
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
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.lightmap.atlas"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // queue.write_texture 无 256 字节行对齐要求（那是 copy_buffer_to_texture
        // 的规则），行步长直接给 size × 8 B（与既有 RGBA8 上传同款做法）。
        self.gpu.queue.write_texture(
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
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.lightmap_gpu
            .insert(handle, GpuLightmap { texture, view });
    }

    /// 该光照贴图句柄的 GPU 上传是否完成（R52c）。测试与调试观测口径：
    /// 未上传（懒上传未触发）/ 悬垂句柄 / [`NO_LIGHTMAP`] 均为 `false`。
    pub fn lightmap_is_uploaded(&self, handle: LightmapHandle) -> bool {
        self.lightmap_gpu.contains_key(&handle.0)
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
        let lightmap_view = match self.lightmap_gpu.get(&lightmap) {
            Some(gpu_lightmap) => &gpu_lightmap.view,
            None => &self.lightmap_dummy_view,
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
                        resource: wgpu::BindingResource::Sampler(&self.lightmap_sampler),
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
