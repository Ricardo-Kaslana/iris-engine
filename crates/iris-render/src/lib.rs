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
//!   纹素对齐拟合 + PCF；覆盖距离 120 m 之外保持受光）与点光 cubemap 阴影
//!   （首灯 6 面 × 512²，R32Float 存归一化线性距离）；
//! - HDR 中间缓冲 + ACES 色调映射 + 后处理链（泛光 / 曝光 / 暗角，
//!   pass 串联框架：描述 → 编码 + ping-pong 中间纹理分配）；
//! - 2D UI 叠加层（HUD）：屏幕空间彩色矩形 + 内嵌 font8x8 位图字体文本，
//!   单管线单纹理单 draw，渲染在 3D 场景色调映射之后（不吃泛光）；
//! - IBL 环境光照（程序化天空 + 分割求和，初始化时一次性烘焙）；
//! - 外部 HDR 环境贴图（Radiance .hdr equirectangular → IBL 与天空盒，generation 失效重烘）；
//! - 视锥剔除与实体挂载相机；
//! - 网格距离 LOD（`LodMesh` 组件：近 high / 远 low，收集期按距离选网格）；
//! - 纹理 BC1 GPU 压缩（确定性编码器 + 特性/对齐/不透明决策树，无特性
//!   或 web 自动回退 RGBA8）与 CPU / GPU 内存占用统计；
//! - glTF 2.0 资产管线与离屏渲染（`OffscreenRenderer`）。

pub mod animation;
pub mod asset;
pub mod camera;
pub mod context;
pub mod decal;
pub mod environment;
pub mod foot_ik;
pub mod frustum;
pub mod instance;
pub mod light;
pub mod material;
pub mod mesh;
pub mod offscreen;
pub mod particles;
pub mod pipeline;
pub mod post;
pub mod renderer;
pub mod rendertarget;
pub mod skin;
pub mod surface;
pub mod texture;
pub mod texture_bcn;
pub mod ui;
pub mod visibility;

pub mod prelude {
    pub use crate::animation::{
        AnimatedField, AnimationBlend, AnimationChannel, AnimationClip, AnimationEdge,
        AnimationEvent, AnimationInterpolation, AnimationPath, AnimationPlayer, AnimationState,
        BlendSpace, blend_pose, collect_events, root_motion_delta, update_animations,
    };
    pub use crate::asset::{
        GltfError, GltfInstance, HdrError, HdrImage, load_hdr, spawn_gltf, spawn_gltf_instance,
        write_glb,
    };
    pub use crate::camera::{
        ActiveCamera, Camera, screen_ray_from_global, view_from_global, view_proj_from_global,
    };
    pub use crate::context::Gpu;
    pub use crate::decal::{Decal, DecalInstanceRaw};
    pub use crate::environment::{EnvironmentSettings, SkySource, SkySourceKind};
    pub use crate::foot_ik::{FootIk, IkRig, solve_two_bone_ik, update_foot_ik};
    pub use crate::frustum::Frustum;
    pub use crate::instance::InstanceRaw;
    pub use crate::light::{
        DirectionalLight, MAX_POINT_LIGHTS, POINT_SHADOW_FACES, PointLight, cascade_fit,
        cascade_splits, snap_aabb_to_texels,
    };
    pub use crate::material::{AlphaMode, MaterialDef, MaterialHandle, MaterialStore};
    pub use crate::mesh::{
        LodMesh, MeshData, MeshHandle, MeshStore, SkinVertex, Vertex, primitives,
    };
    pub use crate::offscreen::OffscreenRenderer;
    pub use crate::particles::{
        AdvanceStats, Particle, ParticleBlend, ParticleEmitter, ParticleInstance, update_particles,
    };
    pub use crate::post::{BloomSettings, ExposureSettings, VignetteSettings};
    pub use crate::renderer::{
        CpuCollectResult, CpuCollector, FrameStats, Renderer, diff_changed_ranges,
    };
    pub use crate::rendertarget::{
        RenderTargetHandle, RenderTargetSpec, RenderTargetStore, RendersTo,
    };
    pub use crate::skin::{Skin, skin_vertex};
    pub use crate::surface::SurfaceKit;
    pub use crate::texture::{CpuTexture, TextureHandle, TextureStore};
    pub use crate::texture_bcn::{
        BC1_BLOCK_BYTES, bc1_eligible, encode_bc1, encoded_len, padded_extent,
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
