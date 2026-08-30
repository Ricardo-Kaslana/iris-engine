//! 资产管线：glTF 2.0 加载与内嵌 base64 编解码。

pub mod base64;
pub mod gltf;

pub use gltf::{spawn_gltf, GltfError};
