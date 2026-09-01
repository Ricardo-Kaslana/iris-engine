//! 渲染器核心：批处理收集、GPU 资源懒上传、帧编码与提交。
//!
//! 数据流：
//! ```text
//! World 资源（MeshStore / MaterialStore / TextureStore，CPU 目录）
//!     -> Renderer 内 GPU 缓存（HashMap<句柄, GPU 资源>，按需懒上传）
//!     -> 查询 (Transform, MeshHandle, MaterialHandle) 收集实例
//!     -> 按 (网格, 材质) 排序合并为批次 -> 一个批次一个 draw call
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use glam::{Mat4, Vec3};
use iris_core::prelude::GlobalTransform;

use crate::SAMPLE_COUNT;
use crate::camera::{ActiveCamera, Camera, view_from_global};
use crate::context::Gpu;
use crate::frustum::Frustum;
use crate::instance::InstanceRaw;
use crate::light::{DirectionalLight, MAX_POINT_LIGHTS, PointLight};
use crate::material::{AlphaMode, MaterialHandle, MaterialStore};
use crate::mesh::{MeshHandle, MeshStore, upload_mesh};
use crate::particles::{ParticleBlend, ParticleEmitter, ParticleInstance};
use crate::pipeline::create_pipeline;
use crate::skin::Skin;
use crate::texture::{TextureHandle, TextureStore};

/// 与 WGSL `PointLight` 结构对齐（32 字节，POD）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PointLightRaw {
    position: [f32; 4],
    color: [f32; 4], // rgb × intensity, w = range
}

/// 与 WGSL `Globals` 结构对齐（576 字节，POD）。
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
    light_view_proj: [[f32; 4]; 4],
    shadow_params: [f32; 4],      // texel_size, bias, strength, enabled
    env_params: [f32; 4],         // 预滤波最大 mip 级, IBL 强度, 保留
    inv_view_proj: [[f32; 4]; 4], // 天空盒射线重建用
    // x: 是否启用点光阴影, y: bias（米）, z: 投影灯 range, w: 保留
    point_shadow_params: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<GlobalsRaw>() == 576,
    "Globals 必须与 WGSL 布局一致"
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

/// 阴影深度偏移（正交投影 + 正面剔除下的小常数即可抑制 acne）。
const SHADOW_BIAS: f32 = 0.0015;
/// 阴影暗部保留的光照比例（0 = 全黑阴影，1 = 无阴影）。
const SHADOW_STRENGTH: f32 = 0.45;
/// 点光阴影偏移（米，归一化距离值域下按 range 归一后比较）。
/// 点光距离值域大，从 0.02 起步实测调到无 acne 且无可见 peter-panning。
const POINT_SHADOW_BIAS: f32 = 0.03;

/// 与 WGSL `MaterialParams` 结构对齐（48 字节，POD）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MaterialRaw {
    base_color: [f32; 4],
    emissive: [f32; 4],
    surface: [f32; 4], // metallic, roughness, reflectance(F0), 保留
}

struct GpuMesh {
    vertex_buf: wgpu::Buffer,
    index_buf: wgpu::Buffer,
    num_indices: u32,
    /// 蒙皮属性缓冲（非蒙皮网格为 None）。
    skin_buf: Option<wgpu::Buffer>,
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
    instance: InstanceRaw,
}

/// 透明蒙皮绘制项：不透明蒙皮项 + 视深排序键（透明排序语义同上）。
struct TransparentSkinnedItem {
    skinned: SkinnedItem,
    depth: f32,
}

struct GpuTexture {
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
}

struct GpuMaterial {
    bind_group: wgpu::BindGroup,
}

/// 排序键 + 实例数据一体存储，可直接整体上传 GPU。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DrawItem {
    key: u64,
    instance: InstanceRaw,
}

/// 渲染器：可同时服务窗口表面与离屏目标。
pub struct Renderer {
    gpu: Arc<Gpu>,
    bundle: crate::pipeline::PipelineBundle,
    globals_buf: wgpu::Buffer,
    globals_bg: wgpu::BindGroup,
    shadow_globals_bg: wgpu::BindGroup,
    instance_buf: Option<wgpu::Buffer>,
    instance_capacity: usize,
    /// 上一帧已上传实例的 CPU 镜像（与 GPU 缓冲前 `len` 个实例内容一致），
    /// 供帧级比对决策全量 / 增量 / 跳过上传。上传完成后与组装 scratch
    /// `mem::swap` 零拷贝交换（见 `draw` 步骤 3）。
    last_instances: Vec<InstanceRaw>,
    mesh_gpu: HashMap<u32, GpuMesh>,
    texture_gpu: HashMap<u32, GpuTexture>,
    material_gpu: HashMap<u32, GpuMaterial>,
    /// 蒙皮实体位标识 → 关节矩阵缓冲。
    skin_gpu: HashMap<u64, GpuSkin>,
    /// 持有纹理以保证对应 view 在渲染器存活期间有效。
    #[allow(dead_code)]
    shadow_map: wgpu::Texture,
    shadow_map_view: wgpu::TextureView,
    /// 阴影 pass 的深度测试附件（保证贴图存最近面深度）。
    #[allow(dead_code)]
    shadow_depth_buf: wgpu::Texture,
    shadow_depth_buf_view: wgpu::TextureView,
    /// 点光 cubemap 阴影贴图：R32Float 6 层，R 存归一化线性距离
    /// （length(world − light) / range，1.0 = 无遮挡）。
    #[allow(dead_code)]
    point_shadow_map: wgpu::Texture,
    /// Cube 维度采样视图（进全局绑定组 binding 7）。
    point_shadow_cube_view: wgpu::TextureView,
    /// 每面一个 D2 渲染视图（base_array_layer = 面序）。
    point_shadow_face_views: [wgpu::TextureView; 6],
    /// 点光阴影深度附件（Depth32Float 6 层，Less 测试保证存最近遮挡物）。
    #[allow(dead_code)]
    point_shadow_depth: wgpu::Texture,
    point_shadow_depth_face_views: [wgpu::TextureView; 6],
    /// 每面一个独立参数缓冲 + 绑定组（一缓冲一面：同一 submit 内对同一
    /// 缓冲的多次 write_buffer 会在执行前全部合并，泛光参数的教训）。
    point_shadow_face_bufs: [wgpu::Buffer; 6],
    point_shadow_face_bgs: [wgpu::BindGroup; 6],
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
    /// UI 字形图集纹理（初始化时一次性构建；字段仅为保持 view 存活）。
    #[allow(dead_code)]
    ui_atlas: wgpu::Texture,
    /// UI 绑定组：图集纹理 + Nearest 采样器 + 屏幕尺寸 uniform。
    ui_bind_group: wgpu::BindGroup,
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
    scratch: Vec<DrawItem>,
    instance_scratch: Vec<InstanceRaw>,
    skinned_scratch: Vec<SkinnedItem>,
    /// 透明项收集暂存（普通 + 蒙皮，跨帧复用）。
    transparent_scratch: Vec<TransparentItem>,
    transparent_skinned_scratch: Vec<TransparentSkinnedItem>,
    /// UI 顶点组装暂存（跨帧复用，避免每帧堆分配）。
    ui_scratch: Vec<crate::ui::UiVertex>,
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
    blit_bind_group: wgpu::BindGroup,
    /// 泛光中间缓冲（半分辨率；a 为提取/垂直模糊输出并供 blit 采样，
    /// b 为水平模糊中转）。
    #[allow(dead_code)]
    bloom_a: wgpu::Texture,
    bloom_a_view: wgpu::TextureView,
    #[allow(dead_code)]
    bloom_b: wgpu::Texture,
    bloom_b_view: wgpu::TextureView,
    /// 三个 pass 的参数缓冲（提取 / 水平模糊 / 垂直模糊）。
    /// 独立三份而非一份缓冲的三个插槽：绑定偏移必须满足
    /// min_uniform_buffer_offset_alignment（典型 256），32B 插槽不合规。
    bloom_params: [wgpu::Buffer; 3],
    /// blit 合成的泛光强度（16B，x 分量生效；关闭泛光时写 0）。
    bloom_strength: wgpu::Buffer,
    bloom_threshold_bg: wgpu::BindGroup,
    bloom_blur_h_bg: wgpu::BindGroup,
    bloom_blur_v_bg: wgpu::BindGroup,
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

/// 单帧渲染统计。除注明外均为**主几何口径**（不透明主 pass、透明 pass
/// 与粒子 pass，见 ARCHITECTURE §4.16 / §4.17）：UI pass 只计入
/// `draw_calls` 的单批，阴影 pass（复用不透明实例区间，透明项与粒子
/// 不进阴影）与天空 / 后处理 pass 全部不计。
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameStats {
    /// 主几何 draw call 数：普通批每批 1 + 蒙皮每实体 1 + 透明 run 批每批 1 +
    /// 透明蒙皮每实体 1 + 粒子（additive / alpha 段各至多 1）+ UI 顶点非空
    /// 时 1。
    pub draw_calls: u32,
    /// 主几何提交的实例数（可见实例，普通 + 蒙皮 + 透明普通 + 透明蒙皮 +
    /// 粒子合计）。
    pub instances: u32,
    /// 本帧实例缓冲实际 `write_buffer` 的字节数。0 = 与上一帧逐实例比对
    /// 完全一致、整帧跳过实例上传（静态场景零上传）。粒子实例走独立
    /// 缓冲、每帧全量上传，**不计入**本字段（见 `particles_drawn`）。
    pub instance_upload_bytes: u64,
    /// 本帧被视锥剔除的实例数（普通 + 蒙皮 + 透明合计）。`Visibility::HIDDEN`
    /// 的过滤不算剔除（不进入 `instances` 也不进入本字段）。**粒子不进本
    /// 口径**：剔除按发射器包围球整批裁决，被剔的发射器既不进 `instances`
    /// 也不进本字段（口径为几何实例，避免混淆，见 ARCHITECTURE §4.17）。
    pub culled_instances: u32,
    /// 主几何实际提交的三角形总数：可见项的网格索引数 / 3 累加（普通批
    /// × 批实例数，蒙皮逐项；透明同口径计入；粒子每粒 2 三角形）。
    /// **主几何口径**——不含 UI pass 与阴影 pass（阴影 pass 只复用不透明
    /// 实例区间，透明项与粒子不投影）。
    pub triangles_drawn: u32,
    /// 本帧粒子 pass 提交的粒子实例数（发射器包围球剔除后；已并入
    /// `instances` / `triangles_drawn`，其 draw call 已并入 `draw_calls`）。
    /// 与发射器池状态对账：= 各可见发射器 `live_count()` 之和。
    pub particles_drawn: u32,
}

/// 变化实例数超过实例总数的该比例时，放弃碎段增量、回退整段全量上传——
/// 大量碎段 `write_buffer` 的驱动提交开销会超过省下的拷贝字节数。
const INSTANCE_DIFF_FULL_FALLBACK_RATIO: f32 = 0.5;

/// 计算两帧实例数组之间的连续变化段（实例索引区间，升序且互不相邻）。
///
/// 语义：
/// - 逐实例比对公共前缀（`min(prev.len(), next.len())`），相邻的变化实例
///   合并为一个连续区间；
/// - `next` 比 `prev` 长：新增尾部整体算作一个变化段（空 `prev` + 非空
///   `next` 即返回覆盖整个 `next` 的单区间）；
/// - `prev` 比 `next` 长：缩短部分无需写入 GPU，不产生区间——调用方在
///   实例数不等时本就走全量上传，此分支仅保证函数语义自洽。
fn diff_changed_ranges(prev: &[InstanceRaw], next: &[InstanceRaw]) -> Vec<std::ops::Range<usize>> {
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
/// 共用此逻辑，避免两处条目清单漂移。
fn build_globals_bind_group(
    gpu: &Gpu,
    bundle: &crate::pipeline::PipelineBundle,
    globals_buf: &wgpu::Buffer,
    shadow_map_view: &wgpu::TextureView,
    point_shadow_cube_view: &wgpu::TextureView,
    env: &crate::environment::EnvironmentMaps,
) -> wgpu::BindGroup {
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
                resource: wgpu::BindingResource::TextureView(shadow_map_view),
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
                resource: wgpu::BindingResource::TextureView(point_shadow_cube_view),
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
/// [`crate::visibility::Visibility`] 过滤在全部收集路径生效；普通实体做
/// 包围球视锥剔除；蒙皮实体做**保守并集 AABB 视锥剔除**（见
/// `skin_world_aabb`）。视锥剔除与剔除计数对透明同样生效。返回可见 /
/// 被剔除的实例数（日志与 `FrameStats` 用）；排序由调用方完成。
// 四个输出列表各自类型不同、语义独立（跨帧复用的 scratch 缓冲逐个传入），
// 收拢成结构体反而多一层拆装；参数多但全为引用/裸缓冲，与 scene.rs
// `build_entity_section` 同理豁免。
#[allow(clippy::too_many_arguments)]
fn collect_instances(
    world: &iris_core::world::World,
    mesh_store: &MeshStore,
    material_store: &MaterialStore,
    view: &Mat4,
    frustum: &Frustum,
    items: &mut Vec<DrawItem>,
    skinned: &mut Vec<SkinnedItem>,
    transparent: &mut Vec<TransparentItem>,
    transparent_skinned: &mut Vec<TransparentSkinnedItem>,
) -> CollectCounts {
    use crate::visibility::Visibility;
    items.clear();
    skinned.clear();
    transparent.clear();
    transparent_skinned.clear();
    let mut culled = 0u32;
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
        &MeshHandle,
        &MaterialHandle,
        &Skin,
        Option<&Visibility>,
    )>(|(e, gt, m, mat, skin, visibility)| {
        if visibility.is_some_and(|v| !v.visible) {
            return;
        }
        skinned_entities.insert(e.to_bits());
        let mut joint_matrices = Vec::with_capacity(skin.joints.len());
        skin.joint_matrices(world, gt.matrix(), &mut joint_matrices);
        let model = gt.matrix();
        let world_aabb = skin_world_aabb(mesh_store, *m, &model, &joint_matrices);
        if world_aabb.is_some_and(|(min, max)| !frustum.intersects_aabb(min, max)) {
            culled += 1;
            return;
        }
        let item = SkinnedItem {
            skin_entity: e.to_bits(),
            mesh: m.0,
            material: mat.0,
            instance: InstanceRaw::from_matrix(model),
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
        &MeshHandle,
        &MaterialHandle,
        Option<&Visibility>,
    )>(|(e, gt, m, mat, visibility)| {
        if visibility.is_some_and(|v| !v.visible) {
            return;
        }
        // 蒙皮实体走独立路径，避免同网格重复绘制
        if skinned_entities.contains(&e.to_bits()) {
            return;
        }
        let model = gt.matrix();
        // 包围球剔除：局部球心经模型矩阵变换，半径按最大列缩放保守放大
        let culled_now = mesh_store
            .get(*m)
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
            transparent.push(TransparentItem {
                key,
                depth: view_depth(view, mesh_store, *m, model),
                entity: e.to_bits(),
                instance: InstanceRaw::from_matrix(model),
            });
        } else {
            items.push(DrawItem {
                key,
                instance: InstanceRaw::from_matrix(model),
            });
        }
    });
    CollectCounts {
        visible: items.len() + skinned.len() + transparent.len() + transparent_skinned.len(),
        culled,
    }
}

/// 收集全部 [`ParticleEmitter`] 的活粒子为 GPU 实例（`draw` 的粒子 pass
/// 数据准备；必须在 `update_particles` 之后调用——收集的是本帧模拟后的
/// 活粒子，调用契约见 `particles.rs`）。
///
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
    world.query::<(&GlobalTransform, &ParticleEmitter)>(|(_, emitter)| {
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

impl Renderer {
    pub fn new(gpu: Arc<Gpu>, color_format: wgpu::TextureFormat) -> Self {
        let bundle = create_pipeline(&gpu, color_format, SAMPLE_COUNT);
        let globals_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.globals"),
            size: std::mem::size_of::<GlobalsRaw>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // 方向光阴影贴图（固定分辨率，不随窗口尺寸重建）：
        // 深度值写入颜色附件 R 通道（绕开 depth-only 管线写入异常，见 ARCHITECTURE §4.7），
        // 同时挂真实深度附件做 Less 测试——没有它重叠表面会按绘制顺序互相覆盖，
        // 背面深度会污染贴图（实测症状：贴图值系统性偏大、阴影整体失效）。
        let shadow_map = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.shadow.map"),
            size: wgpu::Extent3d {
                width: crate::pipeline::SHADOW_MAP_SIZE,
                height: crate::pipeline::SHADOW_MAP_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let shadow_map_view = shadow_map.create_view(&wgpu::TextureViewDescriptor::default());
        let shadow_depth_buf = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.shadow.depth"),
            size: wgpu::Extent3d {
                width: crate::pipeline::SHADOW_MAP_SIZE,
                height: crate::pipeline::SHADOW_MAP_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let shadow_depth_buf_view =
            shadow_depth_buf.create_view(&wgpu::TextureViewDescriptor::default());
        // 点光 cubemap 阴影贴图：R32Float 6 层（层序 = OpenGL 约定
        // [+X, −X, +Y, −Y, +Z, −Z]，与 PointLight::shadow_face_view_proj 配对）。
        // 复刻方向光阴影的双附件教训：颜色附件存值的同时必须挂深度附件做
        // Less 测试——没有它重叠表面按绘制顺序互相覆盖，背面值污染贴图。
        let point_shadow_map = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.point_shadow.map"),
            size: wgpu::Extent3d {
                width: crate::pipeline::POINT_SHADOW_MAP_SIZE,
                height: crate::pipeline::POINT_SHADOW_MAP_SIZE,
                depth_or_array_layers: 6,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let point_shadow_cube_view = point_shadow_map.create_view(&wgpu::TextureViewDescriptor {
            label: Some("iris.point_shadow.cube_view"),
            dimension: Some(wgpu::TextureViewDimension::Cube),
            ..Default::default()
        });
        let point_shadow_face_views = std::array::from_fn(|face| {
            point_shadow_map.create_view(&wgpu::TextureViewDescriptor {
                label: Some("iris.point_shadow.face_view"),
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: face as u32,
                array_layer_count: Some(1),
                ..Default::default()
            })
        });
        let point_shadow_depth = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.point_shadow.depth"),
            size: wgpu::Extent3d {
                width: crate::pipeline::POINT_SHADOW_MAP_SIZE,
                height: crate::pipeline::POINT_SHADOW_MAP_SIZE,
                depth_or_array_layers: 6,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let point_shadow_depth_face_views = std::array::from_fn(|face| {
            point_shadow_depth.create_view(&wgpu::TextureViewDescriptor {
                label: Some("iris.point_shadow.depth_face_view"),
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: face as u32,
                array_layer_count: Some(1),
                ..Default::default()
            })
        });
        let point_shadow_face_bufs: [wgpu::Buffer; 6] = std::array::from_fn(|_| {
            gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("iris.point_shadow.face_params"),
                size: std::mem::size_of::<PointFaceParamsRaw>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        let point_shadow_face_bgs: [wgpu::BindGroup; 6] = std::array::from_fn(|face| {
            gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("iris.point_shadow.face_bg"),
                layout: &bundle.point_shadow_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: point_shadow_face_bufs[face].as_entire_binding(),
                }],
            })
        });
        // IBL 环境贴图一次性烘焙（供主 pass 的分割求和环境光采样）
        let env = crate::environment::bake_environment(&gpu);
        let shadow_globals_bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("iris.shadow.globals.bg"),
            layout: &bundle.shadow_globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buf.as_entire_binding(),
            }],
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
        };
        let mut texture_gpu = HashMap::new();
        texture_gpu.insert(0u32, white_gpu);
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
        // 全局绑定组（env 视图相关的构建逻辑与重烘焙共用，见 build_globals_bind_group）
        let globals_bg = build_globals_bind_group(
            &gpu,
            &bundle,
            &globals_buf,
            &shadow_map_view,
            &point_shadow_cube_view,
            &env,
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
        Self {
            gpu,
            bundle,
            globals_buf,
            globals_bg,
            shadow_globals_bg,
            instance_buf: None,
            instance_capacity: 0,
            last_instances: Vec::new(),
            mesh_gpu: HashMap::new(),
            texture_gpu,
            material_gpu: HashMap::new(),
            skin_gpu: HashMap::new(),
            shadow_map,
            shadow_map_view,
            shadow_depth_buf,
            shadow_depth_buf_view,
            point_shadow_map,
            point_shadow_cube_view,
            point_shadow_face_views,
            point_shadow_depth,
            point_shadow_depth_face_views,
            point_shadow_face_bufs,
            point_shadow_face_bgs,
            env,
            env_baked_generation: 0,
            hdr: None,
            hdr_size: (0, 0),
            ui_atlas,
            ui_bind_group,
            ui_screen_buf,
            ui_vertex_buf: None,
            ui_capacity: 0,
            particle_buf: None,
            particle_capacity: 0,
            particle_camera_buf,
            particle_camera_bg,
            scratch: Vec::new(),
            instance_scratch: Vec::new(),
            skinned_scratch: Vec::new(),
            transparent_scratch: Vec::new(),
            transparent_skinned_scratch: Vec::new(),
            ui_scratch: Vec::new(),
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
    /// irradiance / prefiltered / brdf 三个视图引用必须同步更新。
    fn rebuild_globals_bind_group(&mut self) {
        self.globals_bg = build_globals_bind_group(
            &self.gpu,
            &self.bundle,
            &self.globals_buf,
            &self.shadow_map_view,
            &self.point_shadow_cube_view,
            &self.env,
        );
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
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let msaa_view = msaa.create_view(&wgpu::TextureViewDescriptor::default());
        let resolved_view = resolved.create_view(&wgpu::TextureViewDescriptor::default());
        // 泛光目标：半分辨率，规格与 HDR 中间缓冲一致
        let bloom_size = wgpu::Extent3d {
            width: (width / 2).max(1),
            height: (height / 2).max(1),
            depth_or_array_layers: 1,
        };
        let make_bloom_tex = |label| {
            self.gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: bloom_size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: crate::pipeline::HDR_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        };
        let bloom_a = make_bloom_tex("iris.bloom.a");
        let bloom_b = make_bloom_tex("iris.bloom.b");
        let bloom_a_view = bloom_a.create_view(&wgpu::TextureViewDescriptor::default());
        let bloom_b_view = bloom_b.create_view(&wgpu::TextureViewDescriptor::default());
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
        let bloom_strength = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("iris.bloom.strength"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bloom_sampler = self
            .gpu
            .device
            .create_sampler(&wgpu::SamplerDescriptor::default());
        let bloom_bg = |view: &wgpu::TextureView, params: &wgpu::Buffer| {
            self.gpu
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("iris.bloom.bg"),
                    layout: &self.bundle.bloom_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: params.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&bloom_sampler),
                        },
                    ],
                })
        };
        let bloom_threshold_bg = bloom_bg(&resolved_view, &bloom_params[0]);
        let bloom_blur_h_bg = bloom_bg(&bloom_a_view, &bloom_params[1]);
        let bloom_blur_v_bg = bloom_bg(&bloom_b_view, &bloom_params[2]);
        let blit_bind_group = self
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("iris.blit.bg"),
                layout: &self.bundle.blit_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&resolved_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(
                            &self
                                .gpu
                                .device
                                .create_sampler(&wgpu::SamplerDescriptor::default()),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&bloom_a_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: bloom_strength.as_entire_binding(),
                    },
                ],
            });
        self.hdr_size = (width, height);
        self.hdr = Some(HdrTargets {
            msaa,
            msaa_view,
            resolved,
            resolved_view,
            blit_bind_group,
            bloom_a,
            bloom_a_view,
            bloom_b,
            bloom_b_view,
            bloom_params,
            bloom_strength,
            bloom_threshold_bg,
            bloom_blur_h_bg,
            bloom_blur_v_bg,
        });
    }

    pub fn set_clear_color(&mut self, color: wgpu::Color) {
        self.clear_color = color;
    }

    pub fn stats(&self) -> FrameStats {
        self.stats
    }

    /// 渲染一帧：场景渲染到内部 HDR 中间缓冲（MSAA 4x），透明 pass 与
    /// 粒子 pass 依次叠加（粒子位于透明之后、泛光之前——粒子叠加在玻璃
    /// 等透明结果之上，且高亮 additive 粒子参与泛光），随后 ACES 色调映射
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
        self.ensure_environment(world);
        self.ensure_hdr_targets(width, height);
        self.stats = FrameStats::default();
        // 相机解析：优先挂载在实体上的 ActiveCamera（跟随层级运动），
        // 其次回退到全局 Camera 资源
        let mut mounted: Option<(glam::Mat4, ActiveCamera, Vec3)> = None;
        world.query::<(&GlobalTransform, &ActiveCamera)>(|(gt, ac)| {
            if mounted.is_none() {
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
        let vp = glam::camera::rh::proj::directx::perspective(fov, aspect, z_near, z_far) * view;
        let light = world
            .try_resource::<DirectionalLight>()
            .map(|l| (*l).clone())
            .unwrap_or_default();
        // 阴影覆盖以注视点（资源相机 target / 挂载相机跟随场景原点附近）为中心。
        // 挂载相机没有显式 target：沿视线前方中等距离处作为覆盖中心。
        let shadow_center = if let Some((v, _, cam_eye)) = mounted {
            let forward = -v.z_axis.truncate().normalize_or_zero();
            cam_eye + forward * 10.0
        } else {
            world
                .try_resource::<Camera>()
                .map(|c| c.target)
                .unwrap_or(Vec3::ZERO)
        };
        let light_vp = light.shadow_view_proj(shadow_center, light.shadow_range);
        // 资源目录按引用借用（不复制数据），生命周期覆盖整个 draw 过程
        let Some(mesh_store) = world.try_resource::<MeshStore>() else {
            return;
        };
        let Some(material_store) = world.try_resource::<MaterialStore>() else {
            return;
        };
        let texture_store = world.try_resource::<TextureStore>();

        // 1. 全局 uniform：相机 + 方向光 + 点光
        let light_dir = (-light.direction).normalize_or_zero();
        let mut point_lights = [PointLightRaw {
            position: [0.0; 4],
            color: [0.0; 4],
        }; MAX_POINT_LIGHTS];
        let mut point_count = 0usize;
        // 投影灯（v1 限定一盏）：查询顺序中第一个 shadows==true 的点光，
        // 记录其槽位，收集完成后交换到槽位 0（着色器只对 i==0 采样阴影）。
        let mut caster_slot: Option<usize> = None;
        let mut caster_total = 0usize;
        world.query::<(&GlobalTransform, &PointLight)>(|(gt, pl)| {
            if point_count >= MAX_POINT_LIGHTS {
                return;
            }
            let position = gt.position();
            if pl.shadows {
                caster_total += 1;
                if caster_slot.is_none() {
                    caster_slot = Some(point_count);
                }
            }
            point_lights[point_count] = PointLightRaw {
                position: [position.x, position.y, position.z, 0.0],
                color: [
                    pl.color.x * pl.intensity,
                    pl.color.y * pl.intensity,
                    pl.color.z * pl.intensity,
                    pl.range,
                ],
            };
            point_count += 1;
        });
        if caster_total > 1 {
            log::warn!(
                "有 {caster_total} 盏点光开启阴影，v1 只取查询顺序中的第一盏投射 cubemap 阴影"
            );
        }
        if let Some(slot) = caster_slot {
            point_lights.swap(0, slot);
        }
        // 投影灯交换后必然位于槽位 0；range 直接从 uniform 数据取
        let point_shadow_params = match caster_slot {
            Some(_) => [1.0, POINT_SHADOW_BIAS, point_lights[0].color[3], 0.0],
            None => [0.0, POINT_SHADOW_BIAS, 1.0, 0.0],
        };
        if point_count == MAX_POINT_LIGHTS {
            // 有可能恰好还有更多点光被截断；再探一次数量仅用于日志
            let mut extra = 0;
            world.query::<&PointLight>(|_| extra += 1);
            if extra > MAX_POINT_LIGHTS {
                log::warn!("点光数量 {extra} 超过上限 {MAX_POINT_LIGHTS}，多余的已被忽略");
            }
        }
        let shadow_enabled = light.shadows;
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
            light_view_proj: light_vp.to_cols_array_2d(),
            shadow_params: [
                1.0 / crate::pipeline::SHADOW_MAP_SIZE as f32,
                SHADOW_BIAS,
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
        let total = items.len();
        let skinned_total = skinned.len();
        let transparent_total = transparent.len();
        let instance_total = total + skinned_total + transparent_total + transparent_skinned.len();
        self.stats.culled_instances = counts.culled;
        if counts.culled > 0 {
            log::trace!(
                "视锥剔除 {} / {} 实例（普通 + 蒙皮，含透明）",
                counts.culled,
                counts.visible + counts.culled as usize
            );
        }

        // 2b. 点光阴影 pass 的实例可见性（CPU 粗剔除）：实例包围球到投影灯的
        // 距离减去半径后仍超出 range，则六个 90° 面的并集（= 以灯为中心、
        // range 为半径的完整球域）必然不覆盖它，六面全部跳过。结果只用于
        // 阴影 pass 的绘制区间划分，不影响主 pass 批处理。
        let point_shadow_visible: Vec<bool> = if caster_slot.is_some() {
            let light_pos = Vec3::new(
                point_lights[0].position[0],
                point_lights[0].position[1],
                point_lights[0].position[2],
            );
            let range = point_lights[0].color[3];
            items
                .iter()
                .map(|item| {
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
                })
                .collect()
        } else {
            Vec::new()
        };

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

        // 4. 预先完成 GPU 资源懒上传（避免与渲染通道编码的借用冲突）
        let mut index = 0;
        while index < total {
            let key = items[index].key;
            let mut count = 1;
            while index + count < total && items[index + count].key == key {
                count += 1;
            }
            let mesh = MeshHandle((key >> 32) as u32);
            let material = MaterialHandle(key as u32);
            self.ensure_mesh(&mesh_store, mesh);
            self.ensure_material(&material_store, texture_store.as_deref(), material);
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
                MaterialHandle(item.material),
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
        for item in &transparent {
            self.ensure_mesh(&mesh_store, MeshHandle((item.key >> 32) as u32));
            self.ensure_material(
                &material_store,
                texture_store.as_deref(),
                MaterialHandle(item.key as u32),
            );
        }

        // 5. 编码渲染通道：先阴影 pass（光源视角，深度写入颜色附件；
        //     只画不透明实例区间——透明项不投影），后主 pass 与透明 pass
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
            let mut shadow_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iris.pass.shadow"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.shadow_map_view,
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
                    view: &self.shadow_depth_buf_view,
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
            shadow_pass.set_bind_group(0, &self.shadow_globals_bg, &[]);
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
                shadow_pass.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
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
        // 5b. 点光 cubemap 阴影：投影灯存在时对 6 面各开一个 render pass。
        // 颜色附件（该面 R32Float 视图）与深度附件都清空为 1.0（1.0 =
        // range/range = 无遮挡）；存储值为归一化线性距离（见 fs_point），
        // 深度附件 Less 测试保证每 texel 存最近遮挡物（方向光阴影的教训）。
        if caster_slot.is_some() {
            let light_pos = Vec3::new(
                point_lights[0].position[0],
                point_lights[0].position[1],
                point_lights[0].position[2],
            );
            let range = point_lights[0].color[3];
            // 6 面独立小 uniform：一缓冲一面，全部 write_buffer 后随本帧
            // 统一 submit（write_buffer 按 submit 顺序在 pass 执行前生效）
            for face in 0..6 {
                let raw = PointFaceParamsRaw {
                    view_proj: PointLight::face_view_proj(light_pos, face, range)
                        .to_cols_array_2d(),
                    light_pos_range: [light_pos.x, light_pos.y, light_pos.z, range],
                };
                self.gpu.queue.write_buffer(
                    &self.point_shadow_face_bufs[face],
                    0,
                    bytemuck::bytes_of(&raw),
                );
            }
            if opaque_total > 0 {
                let instance_buf = self.instance_buf.as_ref().expect("实例缓冲已就绪");
                for face in 0..6 {
                    let mut point_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("iris.pass.point_shadow"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &self.point_shadow_face_views[face],
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
                            view: &self.point_shadow_depth_face_views[face],
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
                    point_pass.set_pipeline(&self.bundle.point_shadow_pipeline);
                    point_pass.set_bind_group(0, &self.point_shadow_face_bgs[face], &[]);
                    point_pass.set_vertex_buffer(1, instance_buf.slice(..));
                    // 超出 range 的实例已在 point_shadow_visible 中剔除；
                    // 可见性变化处拆分绘制区间，保持同 key 连续实例合批
                    index = 0;
                    while index < total {
                        if !point_shadow_visible[index] {
                            index += 1;
                            continue;
                        }
                        let key = items[index].key;
                        let mut count = 1;
                        while index + count < total
                            && items[index + count].key == key
                            && point_shadow_visible[index + count]
                        {
                            count += 1;
                        }
                        let mesh = &self.mesh_gpu[&((key >> 32) as u32)];
                        point_pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
                        point_pass
                            .set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
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
            index = 0;
            while index < total {
                let key = items[index].key;
                let mut count = 1;
                while index + count < total && items[index + count].key == key {
                    count += 1;
                }
                let mesh = &self.mesh_gpu[&((key >> 32) as u32)];
                let material = &self.material_gpu[&(key as u32)];
                pass.set_bind_group(1, &material.bind_group, &[]);
                pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
                pass.set_index_buffer(mesh.index_buf.slice(..), wgpu::IndexFormat::Uint32);
                let first = index as u32;
                let last = (index + count) as u32;
                pass.draw_indexed(0..mesh.num_indices, 0, first..last);
                draw_calls += 1;
                instances += count as u32;
                // 主 pass 三角形统计：网格索引数 / 3 × 批实例数。索引数取
                // CPU 目录（网格缺席按 0 处理，不 panic——该路径理论上不可达，
                // 缺席网格在懒上传阶段已告警跳过）
                triangles_drawn += mesh_store
                    .get(MeshHandle((key >> 32) as u32))
                    .map_or(0, |data| data.indices.len() as u32 / 3)
                    * count as u32;
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
                    let material = &self.material_gpu[&item.material];
                    let skin = &self.skin_gpu[&item.skin_entity];
                    pass.set_bind_group(1, &material.bind_group, &[]);
                    pass.set_bind_group(2, &skin.bind_group, &[]);
                    pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
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
        // 透明 pass：主 pass（天空 + 不透明 + 蒙皮）之后、泛光 / 后处理之前，
        // 写同一 HDR MSAA 颜色目标——LoadOp::Load 载入主 pass 结果继续混合，
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
                let material = &self.material_gpu[&(key as u32)];
                pass.set_bind_group(1, &material.bind_group, &[]);
                pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
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
                    let material = &self.material_gpu[&item.material];
                    let skin = &self.skin_gpu[&item.skin_entity];
                    pass.set_bind_group(1, &material.bind_group, &[]);
                    pass.set_bind_group(2, &skin.bind_group, &[]);
                    pass.set_vertex_buffer(0, mesh.vertex_buf.slice(..));
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
        // 泛光：提取 -> 水平模糊 -> 垂直模糊（半分辨率三个全屏 pass）。
        // 强度经 uniform 进 blit：关闭时写 0，blit 绑定组无需重建。
        let bloom = world
            .try_resource::<crate::post::BloomSettings>()
            .map(|b| *b)
            .unwrap_or_default();
        let strength = if bloom.enabled {
            bloom.strength.max(0.0)
        } else {
            0.0
        };
        self.gpu.queue.write_buffer(
            &hdr.bloom_strength,
            0,
            bytemuck::bytes_of(&[strength, 0.0, 0.0, 0.0]),
        );
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
            let mut bloom_pass = |label,
                                  pipeline: &wgpu::RenderPipeline,
                                  bg: &wgpu::BindGroup,
                                  view: &wgpu::TextureView| {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some(label),
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
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, bg, &[]);
                pass.draw(0..3, 0..1);
            };
            bloom_pass(
                "iris.pass.bloom_threshold",
                &self.bundle.bloom_threshold_pipeline,
                &hdr.bloom_threshold_bg,
                &hdr.bloom_a_view,
            );
            // 三轮水平/垂直迭代扩展模糊半径（绑定组固定：H 读 a 写 b，V 读 b 写 a）
            for _ in 0..3 {
                bloom_pass(
                    "iris.pass.bloom_blur_h",
                    &self.bundle.bloom_blur_pipeline,
                    &hdr.bloom_blur_h_bg,
                    &hdr.bloom_b_view,
                );
                bloom_pass(
                    "iris.pass.bloom_blur_v",
                    &self.bundle.bloom_blur_pipeline,
                    &hdr.bloom_blur_v_bg,
                    &hdr.bloom_a_view,
                );
            }
        }
        // 色调映射 blit：HDR -> sRGB 输出
        {
            let mut blit = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("iris.pass.blit"),
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
            blit.set_pipeline(&self.bundle.blit_pipeline);
            blit.set_bind_group(0, &hdr.blit_bind_group, &[]);
            blit.draw(0..3, 0..1);
        }
        // UI 叠加层：blit 之后的最后一个 pass。同一 color_view、LoadOp::Load
        // （不清屏，直接叠加在色调映射结果上）、无深度附件。顶点非空才开
        // pass——空 UI 场景零额外开销。屏幕尺寸 uniform 每帧写入（write_buffer
        // 与本帧 submit 同队列，执行顺序保证在 pass 之前生效）。
        let mut ui_vertices = std::mem::take(&mut self.ui_scratch);
        ui_vertices.clear();
        crate::ui::collect_ui_vertices(world, &mut ui_vertices);
        if !ui_vertices.is_empty() {
            let screen = UiGlobalsRaw {
                screen: [width as f32, height as f32],
                _pad: [0.0; 2],
            };
            self.gpu
                .queue
                .write_buffer(&self.ui_screen_buf, 0, bytemuck::bytes_of(&screen));
            self.ensure_ui_vertex_capacity(ui_vertices.len());
            let ui_buf = self.ui_vertex_buf.as_ref().expect("UI 顶点缓冲已就绪");
            self.gpu
                .queue
                .write_buffer(ui_buf, 0, bytemuck::cast_slice(&ui_vertices));
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
            ui_pass.set_pipeline(&self.bundle.ui_pipeline);
            ui_pass.set_bind_group(0, &self.ui_bind_group, &[]);
            ui_pass.set_vertex_buffer(0, ui_buf.slice(..));
            ui_pass.draw(0..ui_vertices.len() as u32, 0..1);
            // HUD 全部顶点合批为单 draw call
            draw_calls += 1;
        }
        self.ui_scratch = ui_vertices;
        self.gpu.queue.submit([encoder.finish()]);
        self.stats.draw_calls = draw_calls;
        self.stats.instances = instances;
        self.stats.triangles_drawn = triangles_drawn;
        self.stats.particles_drawn = particles_drawn;
        self.scratch = items;
        self.skinned_scratch = skinned;
        self.transparent_scratch = transparent;
        self.transparent_skinned_scratch = transparent_skinned;
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

    fn ensure_mesh(&mut self, store: &MeshStore, handle: MeshHandle) {
        if self.mesh_gpu.contains_key(&handle.0) {
            return;
        }
        let Some(data) = store.get(handle) else {
            log::warn!("网格句柄 {handle:?} 不存在");
            return;
        };
        let (vertex_buf, index_buf, num_indices, skin_buf) = upload_mesh(&self.gpu, data);
        self.mesh_gpu.insert(
            handle.0,
            GpuMesh {
                vertex_buf,
                index_buf,
                num_indices,
                skin_buf,
            },
        );
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
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("iris.texture"),
            size: wgpu::Extent3d {
                width: cpu.width.max(1),
                height: cpu.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &cpu.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(cpu.width * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: cpu.width.max(1),
                height: cpu.height.max(1),
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = self.gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("iris.sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        self.texture_gpu
            .insert(handle.0, GpuTexture { view, sampler });
    }

    fn ensure_material(
        &mut self,
        materials: &MaterialStore,
        textures: Option<&TextureStore>,
        handle: MaterialHandle,
    ) {
        if self.material_gpu.contains_key(&handle.0) {
            return;
        }
        let Some(def) = materials.get(handle) else {
            log::warn!("材质句柄 {handle:?} 不存在");
            return;
        };
        self.ensure_texture(textures, def.texture);
        let texture = &self.texture_gpu[&def.texture.0];
        let raw = MaterialRaw {
            base_color: def.base_color,
            emissive: def.emissive,
            surface: [
                def.metallic.clamp(0.0, 1.0),
                def.roughness.clamp(0.0, 1.0),
                0.04, // 绝缘体反射率基准（glTF 规范约定值）
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
                ],
            });
        self.material_gpu
            .insert(handle.0, GpuMaterial { bind_group });
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

    /// `collect_instances` 的测试便捷封装：视图恒等、材质目录只有 0 号
    /// 默认材质（句柄缺席按不透明，与既有测试语义一致），透明列表丢弃。
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
