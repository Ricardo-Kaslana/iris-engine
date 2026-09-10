//! # iris-render
//!
//! Iris 引擎渲染层：基于 wgpu 的现代 GPU 渲染器。
//!
//! 特性：
//! - 跨后端：Vulkan / DirectX 12 / Metal 由 wgpu 统一抽象，同一份 WGSL 着色器；
//! - 自动实例化批处理：共享网格与材质的实体合并为一个 draw call；
//! - 4x MSAA、深度测试、Cook-Torrance PBR（方向光 + 点光 + 环境光 + 自发光）；
//! - 透明渲染（alpha 混合材质 + 按视深排序的透明 pass，不写深度、不投影）；
//! - 盒投影贴花（`Decal` 组件：弹孔 / 血迹 / 地面标记，端盖代理几何 +
//!   逐实例世界盒矩阵 / 逆矩阵，按纹理分批，蒙皮后透明前叠加，v1 无光照）；
//! - 粒子系统 v1（Emitter 组件 + CPU 确定性模拟 + billboard 实例化渲染，
//!   additive / alpha 双混合）；
//! - 方向光级联阴影 CSM v2（2048² × 3 层 2D 数组深度图 + PFS 分割 +
//!   纹素对齐拟合 + PCF；覆盖距离 120 m 之外保持受光）与点光 cubemap
//!   阴影（R61 预算化多盏：最多 4 盏同时投影，cubemap 纹理数组 + 逐灯
//!   开关 / 槽位 + 确定性选择键，R32Float 存归一化线性距离）；两者的
//!   偏移自 R61 起为「坡度缩放 bias + 法线偏移」（逐级联 / 逐灯按纹素
//!   世界尺寸缩放、常数 bias 只作下限——掠射角 acne 与接触阴影浮起同时
//!   消除，`docs/shadow_v2.png` 四面板对照）；
//! - HDR 中间缓冲 + ACES 色调映射 + 后处理链（泛光 / 曝光 / 暗角，
//!   pass 串联框架：描述 → 编码 + ping-pong 中间纹理分配）；
//! - 2D UI 叠加层（HUD）：屏幕空间彩色矩形 + 内嵌 font8x8 位图字体文本，
//!   单管线单纹理单 draw，渲染在 3D 场景色调映射之后（不吃泛光）；
//! - IBL 环境光照（程序化天空 + 分割求和，初始化时一次性烘焙）；
//! - 外部 HDR 环境贴图（Radiance .hdr equirectangular → IBL 与天空盒，generation 失效重烘）；
//! - 视锥剔除与实体挂载相机；
//! - 网格距离 LOD（`LodMesh` 组件：近 high / 远 low，收集期按距离选网格）；
//! - 烘焙光照 a 阶段（R52a，`bake` 模块）：纯 CPU 光线跟踪核心（确定性
//!   BVH + Möller–Trumbore）+ 无 RNG 低差异采样 + 辐照度积分器（直接光 /
//!   天空 AO / 一次反弹色溢），输出世界空间采样点列表（UV2 图集与渲染
//!   集成留 b/c 阶段）；
//! - 烘焙光照 b 阶段（R52b，`bake_uv` 模块）：UV2 参数化与图集布局——
//!   主轴投影 chart（完备分类 + 正面积重叠贪心偏移防串光）→ shelf 装箱
//!   （面积降序全序、利用率口径 = 已用高度）→ texel 世界位回溯（含边界 +
//!   先者得）→ `bake_lightmap` 全链组装（逐 texel 辐照，行主序）→ ACES +
//!   sRGB 编码的 PPM 预览导出（渲染集成留 c 阶段）；
//! - 烘焙光照 c 阶段（R52c，`lightmap` 模块）：GPU 集成与混合语义——
//!   `MeshData` UV2 顶点属性（逐实体注入 + 跨 chart 顶点拆分）→ 图集
//!   Rgba16Float 懒上传（自带 IEEE 754 half 转换 + dilation 填充 chart
//!   边缘）→ 材质绑定组按 (材质, 图集) 缓存、绘制按图集切段 → shader
//!   间接项 `select(IBL 环境, 光照贴图辐照, mask)`（直接光不受影响 =
//!   「静态 GI 补动态直射」）；未挂光照贴图的场景**逐位不变**（FNV-1a
//!   全帧基线锁，见 `tests/lightmap_render.rs`）；
//! - 遮挡剔除（R53，`occlusion` 模块）：wgpu 硬件遮挡查询两阶段——
//!   主 pass 深度上逐实例画世界 AABB 代理领用查询槽，结果异步读回
//!   延迟一帧裁决（迟到即弃 = 全部可见），只跳过主 pass 绘制、阴影与
//!   深度副本保守保留；资源缺席关闭，静止场景被剔像素逐位不可见；
//! - FXAA 抗锯齿（R53，`shaders/fxaa.wgsl`）：后处理链尾 LDR sRGB 域
//!   单 pass（blit → FXAA → UI，HUD 在其后绘制保持锐利），非边缘像素
//!   早退逐位透传；资源缺席关闭 = blit 直写目标、链逐位不变；
//! - 纹理 BC1 GPU 压缩（确定性编码器 + 特性/对齐/不透明决策树，无特性
//!   或 web 自动回退 RGBA8）与 CPU / GPU 内存占用统计；
//! - 烘焙反射探针（R62a，`probe` 模块）：实体挂 `ReflectionProbe` 定义
//!   局部**轴对齐长方体**影响域（+ 边界过渡带），`Renderer::bake_reflection_probe`
//!   从探针位置 6 面捕获场景 cubemap（含天空、含探针盒单级联方向光阴影）
//!   并按 GGX 预滤波成与 env 同档的 mip 链；片元在域内时镜面走探针 +
//!   **视差修正**（反射射线 × 盒求交重定向，经典 box projection），域外 /
//!   无探针回落全局 env；挂光照贴图的实体的镜面也由探针补回（闭合 §8.31④）。
//!   无探针场景**逐位不变**（FNV-1a 全帧基线锁，见 `tests/probe_render.rs`）；
//! - glTF 2.0 资产管线与离屏渲染（`OffscreenRenderer`）。

pub mod animation;
pub mod asset;
pub mod bake;
pub mod bake_uv;
pub mod camera;
pub mod context;
pub mod decal;
pub mod environment;
pub mod foot_ik;
pub mod frustum;
pub mod hand_ik;
pub mod instance;
pub mod light;
pub mod lightmap;
pub mod material;
pub mod mesh;
pub mod occlusion;
pub mod offscreen;
pub mod particles;
pub mod pipeline;
pub mod post;
pub mod probe;
pub mod renderer;
pub mod rendertarget;
pub mod skin;
pub mod surface;
pub mod terrain;
pub mod texture;
pub mod texture_bcn;
pub mod ui;
pub mod visibility;

pub mod prelude {
    pub use crate::animation::{
        AdditiveLayer, AnimatedField, AnimationBlend, AnimationChannel, AnimationClip,
        AnimationEdge, AnimationEvent, AnimationInterpolation, AnimationMask, AnimationPath,
        AnimationPlayer, AnimationState, BlendSpace, additive_delta, blend_pose, collect_crossings,
        collect_events, root_motion_delta, update_animations,
    };
    pub use crate::asset::{
        AnimationRegistry, ClipMeta, GltfAnimationLibrary, GltfError, GltfInstance, HdrError,
        HdrImage, load_hdr, spawn_gltf, spawn_gltf_instance, spawn_gltf_instance_from_bytes,
        write_glb,
    };
    pub use crate::bake::{
        BAKE_RAY_MIN_T, BAKE_SHADOW_BIAS, BVH_LEAF_SIZE, BakeParams, BakeSample, BakeTriangle,
        BakedLightmap, Bvh, BvhHit, SHADOW_SOFT_SAMPLES, bake_lightmap, bake_points, bake_sample,
        build_bvh, collect_bake_triangles, cosine_hemisphere, radical_inverse_2,
        ray_triangle_intersection, raycast_bvh, uniform_cone,
    };
    pub use crate::bake_uv::{
        AtlasEntry, AtlasTriangle, BakeUvParams, CHART_PAD_TEXELS, Chart, ChartAxis,
        FLOOR_NORMAL_THRESHOLD, LightmapAtlas, TexelSource, aces_tonemap, bake_texels,
        build_charts, classify_axis, pack_atlas, write_lightmap_preview,
    };
    pub use crate::camera::{
        ActiveCamera, Camera, screen_ray_from_global, view_from_global, view_proj_from_global,
    };
    pub use crate::context::Gpu;
    pub use crate::decal::{Decal, DecalInstanceRaw};
    pub use crate::environment::{EnvironmentSettings, SkySource, SkySourceKind};
    pub use crate::foot_ik::{FootIk, IkRig, solve_two_bone_ik, update_foot_ik};
    pub use crate::frustum::Frustum;
    pub use crate::hand_ik::{HandIk, IkTarget, update_hand_ik};
    pub use crate::instance::InstanceRaw;
    pub use crate::light::{
        CascadeFit, DirectionalLight, MAX_POINT_LIGHTS, POINT_SHADOW_BIAS_LEGACY,
        POINT_SHADOW_BIAS_MIN, POINT_SHADOW_BUDGET, POINT_SHADOW_FACES, PointLight,
        PointShadowCandidate, SHADOW_BIAS_LEGACY, SHADOW_BIAS_MIN, cascade_fit, cascade_fit_params,
        cascade_splits, point_shadow_bias_meters, point_shadow_texel_world,
        select_point_shadow_casters, shadow_bias_ndc, shadow_normal_offset_world, slope_tan,
        snap_aabb_to_texels,
    };
    pub use crate::lightmap::{
        LIGHTMAP_DILATE_ITERATIONS, LightmapBinding, LightmapHandle, LightmapInstall, LightmapItem,
        LightmapStore, NO_LIGHTMAP, build_uv2_mesh, dilate_atlas, f32_to_f16, install_lightmap,
    };
    pub use crate::material::{AlphaMode, MaterialDef, MaterialHandle, MaterialStore};
    pub use crate::mesh::{
        LodMesh, MeshData, MeshHandle, MeshStore, SkinVertex, UV2_LAYOUT, Vertex, primitives,
    };
    pub use crate::occlusion::OcclusionCulling;
    pub use crate::offscreen::OffscreenRenderer;
    pub use crate::particles::{
        AdvanceStats, Particle, ParticleBlend, ParticleEmitter, ParticleInstance, update_particles,
    };
    pub use crate::post::{
        BloomSettings, DepthOfFieldSettings, ExposureSettings, FxaaSettings, MotionBlurSettings,
        TaaSettings, VignetteSettings, halton, jitter_projection, resolve_focus_distance,
        taa_jitter,
    };
    pub use crate::probe::{
        MAX_REFLECTION_PROBES, PROBE_CAPTURE_FAR, PROBE_CAPTURE_NEAR, PROBE_DEFAULT_FALLOFF,
        PROBE_MAP_SIZE, PROBE_MIP_COUNT, ProbeCandidate, ProbeStore, ReflectionProbe,
        distance_to_box, fit_probe_shadow, influence_weight, parallax_direction,
        select_active_probes,
    };
    pub use crate::renderer::{
        CpuCollectResult, CpuCollector, FrameStats, Renderer, diff_changed_ranges,
    };
    pub use crate::rendertarget::{
        RenderTargetHandle, RenderTargetSpec, RenderTargetStore, RendersTo,
    };
    pub use crate::skin::{Skin, skin_vertex};
    pub use crate::surface::SurfaceKit;
    pub use crate::terrain::{
        COLLIDER_DEPTH_MARGIN, GrassTextures, Heightmap, LodMesh3, TERRAIN_LOD_LEVELS,
        TERRAIN_LOD_STRIDES, Terrain, TerrainBuild, TerrainOptions, build_terrain, chunk_mesh,
        generate_hills, grass_textures, terrain_statics,
    };
    pub use crate::texture::{CpuTexture, TextureHandle, TextureStore};
    pub use crate::texture_bcn::{
        BC1_BLOCK_BYTES, BC3_BLOCK_BYTES, bc1_eligible, bc3_eligible, encode_bc1, encode_bc3,
        encoded_len, encoded_len_bc3, padded_extent,
    };
    pub use crate::ui::{
        Focusable, UiAnchor, UiAnchorPoint, UiButton, UiClickEvent, UiColor, UiFocus, UiImage,
        UiImageRtt, UiMenu, UiMenuStack, UiRect, UiText, anchor_to_position, glyph_bits,
        sort_focus_items, spawn_ui_button, text_width, update_ui_focus, update_ui_menus,
    };
    pub use crate::visibility::{Visibility, set_subtree_hidden};
}

/// 全引擎固定的多重采样倍率（4x MSAA）。
pub const SAMPLE_COUNT: u32 = 4;
