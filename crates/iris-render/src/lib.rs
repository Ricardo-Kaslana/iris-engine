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
//! - 光照探针 GI（R64 / R83，`irradiance» 模块）：烘焙 irradiance 探针云 + 四面体
//!   重心插值——`IrradianceProbe» 组件定义探针位置（取 `GlobalTransform»），
//!   `bake_irradiance_probes» 逐探针复用 `bake_sample»（六轴平均，与光照贴图
//!   同口径的间接辐照）并做**自研确定性 Delaunay 四面体化**（增量
//!   Bowyer-Watson：BFS 连通空腔 + 幽灵胞元凸包接入 + 最胖初始单纯形 +
//!   三线性哈希抖动 ±1e-6 + 抖动幅度阶梯重试），运行时对**无光照贴图实体**
//!   按位置插值（逐实体 CPU 侧）注入实例载荷
//!   `InstanceRaw::probe»（xyz = E/π、w = 标志位）；着色器以
//!   `select(env_irradiance, in.probe.rgb, in.probe.w > 0.5)» 取代全局 env 的
//!   漫反射辐照。探针缺席 / 空集 / 实体挂光照贴图 ⇒ 载荷恒零 ⇒ **逐位不变**
//!   （全部既有 FNV 基线原值通过，见 `tests/irradiance_render.rs»）；
//!   包外取最近凸包面投影（连续延拓）。语义：静态漫反射 = 光照贴图 /
//!   动态漫反射 = 探针 / 镜面 = 反射探针 → SSR → env。
//!   **R83 追加逐片元支**：`bake_irradiance_probes_sh` 给探针追加**三阶 SH**
//!   （L0–L2 每通道 9 系数，六面 × 32² 精确固体角投影 + Lambert 带通卷积除 π
//!   ⇒ E/π 直读）+ 影响半径（最近邻 × 1.5 自动派生），渲染器在探针集带 SH 时
//!   走 mode = 1（binding 18/19，片端径向权重 × 9 项点积逐片元求值——空间
//!   梯度 + 方向性）；不带 SH ⇒ mode = 0，v1 逐实体路径**逐位不变**。
//!   见 ARCHITECTURE §4.39 / §8.52 与 `tests/gi_sh_render.rs`。
//! - DDGI 动态全局光照 v1（R88，`ddgi` 模块）：**规则探针网格**（纯资源态，
//!   不落实体；超容量走确定性抽稀而非按序截断 ⇒ 覆盖范围不变）+ **CPU 光线
//!   追踪的时间片滚动重照明**（复用 R52a 的 BVH，每帧只重照明 k 个脏探针，
//!   收敛 `⌈n/k⌉` 帧；稳态零 CPU 零上传）+ **深度感知混合**（每探针 6×8²
//!   方向深度两阶矩 (μ, σ²) × chebyshev 单侧上界权重，**闭合 §8.52①** 的
//!   「隔墙串味」）。SH 投影与深度矩**同一批射线** ⇒ 深度项零额外光线成本；
//!   σ² 在 CPU 用 f64 求差后钳下限（f32 会灾难性抵消）⇒ 着色器零减法。
//!   binding 20 = 768 KiB 深度表（惰性创建）+ binding 19 的 mode 扩到 2；
//!   R83 / R64 的 mode 0/1 分支**语句逐条不动**。`DdgiSettings` 缺席 ⇒
//!   一个 `write_buffer` 都不发、既有 FNV 基线原值通过。
//!   见 ARCHITECTURE §4.44 / §8.57 与 `tests/ddgi_render.rs`。
//! - 平面水体 v1（R81，`water` 模块）：`WaterSurface` 组件声明世界空间
//!   水平面（多实体确定性选择键取首盏），Gerstner 4 波顶点位移（解析
//!   法线，CPU 纯函数与 WGSL 逐式镜像）+ 反射 / 折射半分辨率捕获
//!   （镜像相机 / 主相机，探针同款最低配组装）+ Fresnel + 深度吸收 +
//!   太阳镜面，主 pass 后 HDR 合成；无水体实体 = 像素与命令流逐位不变。
//! - glTF 2.0 资产管线与离屏渲染（`OffscreenRenderer»）。

pub mod animation;
pub mod asset;
pub mod bake;
pub mod bake_uv;
pub mod camera;
pub mod chain_ik;
pub mod context;
pub mod ddgi;
pub mod decal;
pub mod environment;
pub mod fog;
pub mod foot_ik;
pub mod frustum;
pub mod hand_ik;
pub mod instance;
pub mod irradiance;
pub mod light;
pub mod light_bins;
pub mod lightmap;
pub mod material;
pub mod mesh;
pub mod meshlet;
pub mod meshlet_stream;
pub mod occlusion;
pub mod offscreen;
pub mod particles;
pub mod particles_v2;
pub mod pipeline;
pub mod post;
pub mod probe;
pub mod renderer;
pub mod rendertarget;
pub mod skin;
pub mod sss;
pub mod surface;
pub mod terrain;
pub mod texture;
pub mod texture_bcn;
pub mod ui;
pub mod visibility;
pub mod water;

pub mod prelude {
    pub use crate::animation::{
        AdditiveLayer, AnimatedField, AnimationBlend, AnimationChannel, AnimationClip,
        AnimationEdge, AnimationEvent, AnimationInterpolation, AnimationMask, AnimationPath,
        AnimationPlayer, AnimationState, BlendSpace, ClipWindow, ParameterTrack, additive_delta,
        blend_pose, collect_crossings, collect_events, root_motion_delta, sample_track,
        update_animations, window_active,
    };
    pub use crate::asset::{
        AnimationRegistry, ClipMeta, GltfAnimationLibrary, GltfError, GltfInstance, HdrError,
        HdrImage, load_hdr, spawn_gltf, spawn_gltf_instance, spawn_gltf_instance_from_bytes,
        write_glb,
    };
    pub use crate::bake::{
        BAKE_RAY_MIN_T, BAKE_SHADOW_BIAS, BVH_LEAF_SIZE, BakeExcluded, BakeParams, BakeSample,
        BakeTriangle, BakedLightmap, Bvh, BvhHit, SHADOW_SOFT_SAMPLES, bake_lightmap, bake_points,
        bake_sample, build_bvh, collect_bake_triangles, cosine_hemisphere, radical_inverse_2,
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
    pub use crate::chain_ik::{
        ChainIk, LookIk, solve_fabrik, swing_fraction, update_chain_ik, update_look_ik,
    };
    pub use crate::context::Gpu;
    // DDGI 动态全局光照（R88 v1 + R89 v2）：规则探针网格 + 时间片滚动重照明
    // + 深度感知混合 + 覆盖度合成
    pub use crate::ddgi::{
        DDGI_COVER_FADE, DDGI_DEPTH_FLOATS, DDGI_DEPTH_GRID, DDGI_DEPTH_PROBE_BYTES,
        DDGI_DEPTH_TABLE_BYTES, DDGI_DEPTH_TEXELS, DDGI_DEPTH_TEXELS_PER_FACE, DDGI_PREFILL_GRID,
        DDGI_RAYS_PER_FACE_CHOICES, DDGI_SIGMA_REL, DDGI_SKY_DEPTH, DdgiGrid, DdgiLayout,
        DdgiSettings, DdgiStats, advance, axis_points, ddgi_blend, ddgi_blend_with_cov,
        ddgi_coverage, ddgi_coverage_with_fade, ddgi_depth_face, ddgi_depth_texel,
        ddgi_depth_weight, ddgi_depth_weight_at, default_bounds, depth_cell_of_ray,
        depth_hysteresis, depth_moment_finish, grid_fingerprint, layout_positions,
        neutral_depth_moment, neutral_depth_table, plan_layout, probe_relight, relight_fingerprint,
        sh_hysteresis, stride_for_capacity, triangles_bounds,
    };
    pub use crate::decal::{Decal, DecalInstanceRaw};
    pub use crate::environment::{EnvironmentSettings, SkySource, SkySourceKind};
    pub use crate::fog::{
        FOG_STEPS, FogSettings, fog_density_at, fog_dither, fog_phase_hg, fog_transmittance,
    };
    pub use crate::foot_ik::{FootIk, IkRig, solve_two_bone_ik, update_foot_ik};
    pub use crate::frustum::Frustum;
    pub use crate::hand_ik::{HandIk, IkTarget, update_hand_ik};
    pub use crate::instance::InstanceRaw;
    pub use crate::irradiance::{
        IrradianceProbe, IrradianceProbeInstall, IrradianceProbeRadius, IrradianceProbeSet,
        MAX_IRRADIANCE_PROBES, PROBE_AXES, PROBE_RADIUS_NEIGHBOR_FACTOR, ProbeBakeStats, SH_BANDS,
        SH_BLEND_EPS, SH_COEFFS, SH_CUBE_FACES, SH_CUBE_GRID, SH_LAMBERT_BANDS, SH_VALUES, SH_Y0,
        SH_Y1, SH_Y2A, SH_Y2B, SH_Y2C, ShCoeffs, ShProbeRaw, TetraStats, TetraValidation,
        Tetrahedralization, bake_irradiance_probes, bake_irradiance_probes_sh, cube_face_direction,
        cube_texel_solid_angle, derive_probe_radii, install_irradiance_probes,
        probe_influence_weight, probe_ray_radiance_depth, probe_sh_coefficients, probe_sh_radiance,
        project_probe_sh, sh_basis, sh_blend, sh_blend_l0, sh_dc, sh_eval, sh_lambert_convolve,
    };
    pub use crate::light::{
        CascadeFit, DirectionalLight, MAX_POINT_LIGHTS, POINT_SHADOW_BIAS_LEGACY,
        POINT_SHADOW_BIAS_MIN, POINT_SHADOW_BUDGET, POINT_SHADOW_FACES, PointLight,
        PointShadowCandidate, SHADOW_BIAS_LEGACY, SHADOW_BIAS_MIN, SpotLight, cascade_fit,
        cascade_fit_params, cascade_splits, point_shadow_bias_meters, point_shadow_texel_world,
        select_point_shadow_casters, shadow_bias_ndc, shadow_normal_offset_world, slope_tan,
        snap_aabb_to_texels,
    };
    pub use crate::light_bins::{
        LIGHT_TILE_SIZE, LightBinInput, LightBinningSettings, LightBins, MAX_BINNED_LIGHTS,
        bin_lights, light_bins_enabled, spot_cone_factor,
    };
    pub use crate::lightmap::{
        LIGHTMAP_DILATE_ITERATIONS, LightmapBinding, LightmapHandle, LightmapInstall, LightmapItem,
        LightmapStore, NO_LIGHTMAP, build_uv2_mesh, dilate_atlas, f32_to_f16, install_lightmap,
    };
    pub use crate::material::{AlphaMode, MaterialDef, MaterialHandle, MaterialStore};
    pub use crate::mesh::{
        LodMesh, MeshData, MeshHandle, MeshStore, SkinVertex, UV2_LAYOUT, Vertex, primitives,
    };
    pub use crate::meshlet::{
        MAX_MESHLET_CLUSTERS, MESHLET_CLUSTER_MAX_TRIANGLES, MESHLET_CLUSTER_MAX_VERTICES,
        MESHLET_COMPACT_CHUNK, MESHLET_COMPACT_THREADS, MESHLET_CONE_FALLBACK_AXIS,
        MESHLET_CONE_FALLBACK_CUTOFF_COS, MeshletAssetError, MeshletClusterRaw, MeshletHierarchy,
        MeshletInstance, MeshletLodGroup, MeshletLodSettings, MeshletLodView, MeshletMesh,
        MeshletMeshHandle, MeshletMeshStore, chunked_prefix_compact, cluster_bounding_sphere,
        cluster_bounds_normalized, cluster_cone, cluster_is_backfacing, mesh_bounds,
        select_lod_cluster_ranges_in, select_lod_clusters, vertex_storage_words,
    };
    pub use crate::meshlet_stream::{
        MESHLET_NO_GROUP, MESHLET_PAGE_CLUSTERS, MeshletPageTable, MeshletStreamSettings,
        ResidencyManager, ResidencyOps, ResidencySelection, select_lod_cluster_ranges_resident,
    };
    pub use crate::occlusion::OcclusionCulling;
    pub use crate::offscreen::OffscreenRenderer;
    pub use crate::particles::{
        AdvanceStats, Particle, ParticleBlend, ParticleEmitter, ParticleInstance, update_particles,
    };
    pub use crate::particles_v2::{
        PARTICLES_V2_DISPATCH, PARTICLES_V2_MAX, PARTICLES_V2_PENDING_CAP, PARTICLES_V2_TRAIL_MAX,
        PARTICLES_V2_WORKGROUP, ParticleEmitterV2, update_particles_v2,
    };
    pub use crate::post::{
        AO_SLICES, AO_STEPS, AoSettings, BloomSettings, DepthOfFieldSettings, ExposureSettings,
        FxaaSettings, MotionBlurSettings, SkySettings, SsrSettings, TaaSettings, VignetteSettings,
        ao_dither, ao_falloff, ao_sample_sin, ao_slice_occlusion, ao_uv_offset, ao_view_pos,
        ao_visibility, halton, jitter_projection, resolve_focus_distance, ssr_blend_weight,
        ssr_edge_fade, ssr_roughness_fade, taa_jitter,
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
    pub use crate::texture::{
        CpuTexture, MipSettings, TextureHandle, TextureStore, downsample_rgba8, mip_chain_bc,
        mip_chain_rgba8, mip_level_count,
    };
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
    pub use crate::water::{
        WATER_F0, WATER_GRID_SUBDIVISIONS, WATER_WAVE_COUNT, WAVE_AMPLITUDE_DECAY,
        WAVE_AMPLITUDE_SUM, WAVE_DIRECTIONS, WAVE_LENGTH_DECAY, WaterSurface, WaterVertex,
        gerstner_evaluate, gerstner_single_wave, select_water_surface, water_grid_mesh,
    };
}

/// 全引擎固定的多重采样倍率（4x MSAA）。
pub const SAMPLE_COUNT: u32 = 4;
