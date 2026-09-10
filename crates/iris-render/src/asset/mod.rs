//! 资产管线：glTF 2.0 加载与内嵌 base64 编解码、Radiance .hdr 全景图
//! 解析、GLB 动画注册表（R59）。

pub mod base64;
pub mod gltf;
pub mod hdr;
pub mod registry;

pub use gltf::{
    GltfError, GltfInstance, spawn_gltf, spawn_gltf_instance, spawn_gltf_instance_from_bytes,
    write_glb,
};
pub use hdr::{HdrError, HdrImage, encode_hdr, load_hdr, parse_hdr};
pub use registry::{AnimationRegistry, ClipMeta, GltfAnimationLibrary};
