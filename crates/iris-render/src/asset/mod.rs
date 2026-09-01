//! 资产管线：glTF 2.0 加载与内嵌 base64 编解码、Radiance .hdr 全景图解析。

pub mod base64;
pub mod gltf;
pub mod hdr;

pub use gltf::{GltfError, GltfInstance, spawn_gltf, spawn_gltf_instance, write_glb};
pub use hdr::{HdrError, HdrImage, encode_hdr, load_hdr, parse_hdr};
