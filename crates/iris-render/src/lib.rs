//! # iris-render
//!
//! Iris 引擎渲染层：基于 wgpu 的现代 GPU 渲染器。
//!
//! 特性：
//! - 跨后端：Vulkan / DirectX 12 / Metal 由 wgpu 统一抽象，同一份 WGSL 着色器；
//! - 自动实例化批处理：共享网格与材质的实体合并为一个 draw call；
//! - 4x MSAA、深度测试、Cook-Torrance PBR（方向光 + 点光 + 环境光 + 自发光）；
//! - HDR 中间缓冲 + ACES 色调映射 + 泛光（Bloom）后处理；
//! - IBL 环境光照（程序化天空 + 分割求和，初始化时一次性烘焙）；
//! - 视锥剔除与实体挂载相机；
//! - glTF 2.0 资产管线与离屏渲染（`OffscreenRenderer`）。

pub mod asset;
pub mod camera;
pub mod context;
pub mod environment;
pub mod frustum;
pub mod instance;
pub mod light;
pub mod material;
pub mod mesh;
pub mod offscreen;
pub mod pipeline;
pub mod post;
pub mod renderer;
pub mod surface;
pub mod texture;

pub mod prelude {
    pub use crate::asset::{spawn_gltf, GltfError};
    pub use crate::camera::{view_from_global, ActiveCamera, Camera};
    pub use crate::context::Gpu;
    pub use crate::environment::EnvironmentSettings;
    pub use crate::frustum::Frustum;
    pub use crate::instance::InstanceRaw;
    pub use crate::light::{DirectionalLight, PointLight, MAX_POINT_LIGHTS};
    pub use crate::material::{MaterialDef, MaterialHandle, MaterialStore};
    pub use crate::mesh::{primitives, MeshData, MeshHandle, MeshStore, Vertex};
    pub use crate::offscreen::OffscreenRenderer;
    pub use crate::post::BloomSettings;
    pub use crate::renderer::{FrameStats, Renderer};
    pub use crate::surface::SurfaceKit;
    pub use crate::texture::{CpuTexture, TextureHandle, TextureStore};
}

/// 全引擎固定的多重采样倍率（4x MSAA）。
pub const SAMPLE_COUNT: u32 = 4;
