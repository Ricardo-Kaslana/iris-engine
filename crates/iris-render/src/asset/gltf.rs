//! glTF 2.0 加载器（v1 子集）。
//!
//! 支持：
//! - `.gltf`（JSON）+ 外部 `.bin` buffer 或 `data:` URI 内嵌 buffer；
//!   GLB 二进制容器暂不支持；
//! - 三角形网格（mode 4），POSITION 必需，NORMAL 缺失时自动计算平面法线，
//!   TEXCOORD_0 缺失时填充 (0, 0)；
//! - 索引 u16 / u32，顶点分量 FLOAT；
//! - 材质：`baseColorFactor`、`emissiveFactor`；`roughness` 映射为高光锐度；
//!   `baseColorTexture` 支持外部图片文件或 data URI（PNG / JPEG）；
//! - 节点 TRS 与 children 层级 → Iris 实体树（父节点实体 + 每 primitive 一个子实体）。
//!
//! ```ignore
//! let entities = iris_render::asset::spawn_gltf(&mut world, "assets/scene.gltf")?;
//! ```

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use iris_core::prelude::{Quat, Transform, World};
use serde::Deserialize;

use super::base64;
use crate::material::{MaterialDef, MaterialHandle, MaterialStore};
use crate::mesh::{MeshData, MeshHandle, MeshStore, Vertex};
use crate::texture::{CpuTexture, TextureHandle, TextureStore};

/// glTF 加载错误。
#[derive(Debug)]
pub enum GltfError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Image(image::ImageError),
    /// 文件存在但使用了 v1 不支持的特性。
    Unsupported(String),
    /// JSON 结构缺少必需的引用目标。
    Missing(String),
    /// 引用越界或数据长度不符。
    OutOfRange(String),
}

impl fmt::Display for GltfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO 错误：{e}"),
            Self::Json(e) => write!(f, "JSON 解析错误：{e}"),
            Self::Image(e) => write!(f, "图片解码错误：{e}"),
            Self::Unsupported(what) => write!(f, "暂不支持的 glTF 特性：{what}"),
            Self::Missing(what) => write!(f, "缺少必需的 glTF 元素：{what}"),
            Self::OutOfRange(what) => write!(f, "glTF 引用越界：{what}"),
        }
    }
}

impl std::error::Error for GltfError {}

impl From<std::io::Error> for GltfError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<serde_json::Error> for GltfError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
impl From<image::ImageError> for GltfError {
    fn from(e: image::ImageError) -> Self {
        Self::Image(e)
    }
}

// ---- glTF JSON schema（宽松解析，仅声明用到的字段） ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GltfJson {
    #[serde(default)]
    scene: usize,
    #[serde(default)]
    scenes: Vec<GltfScene>,
    #[serde(default)]
    nodes: Vec<GltfNode>,
    #[serde(default)]
    meshes: Vec<GltfMesh>,
    #[serde(default)]
    materials: Vec<GltfMaterial>,
    #[serde(default)]
    textures: Vec<GltfTexture>,
    #[serde(default)]
    images: Vec<GltfImage>,
    #[serde(default)]
    accessors: Vec<GltfAccessor>,
    #[serde(default)]
    buffer_views: Vec<GltfBufferView>,
    #[serde(default)]
    buffers: Vec<GltfBuffer>,
}

#[derive(Deserialize)]
struct GltfScene {
    #[serde(default)]
    nodes: Vec<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GltfNode {
    #[serde(default)]
    mesh: Option<usize>,
    #[serde(default)]
    children: Vec<usize>,
    #[serde(default)]
    translation: [f32; 3],
    /// glTF 四元数为 (x, y, z, w)，与 glam 一致。
    #[serde(default)]
    rotation: [f32; 4],
    #[serde(default = "default_scale")]
    scale: [f32; 3],
}

fn default_scale() -> [f32; 3] {
    [1.0, 1.0, 1.0]
}

#[derive(Deserialize)]
struct GltfMesh {
    #[serde(default)]
    primitives: Vec<GltfPrimitive>,
}

#[derive(Deserialize)]
struct GltfPrimitive {
    #[serde(default)]
    attributes: HashMap<String, usize>,
    #[serde(default)]
    indices: Option<usize>,
    #[serde(default)]
    material: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GltfMaterial {
    #[serde(default)]
    pbr_metallic_roughness: Option<GltfPbr>,
    #[serde(default)]
    emissive_factor: [f32; 3],
}

impl GltfMaterial {
    fn emissive(&self) -> [f32; 3] {
        self.emissive_factor
    }
}

#[derive(Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct GltfPbr {
    #[serde(default = "default_base_color")]
    base_color_factor: [f32; 4],
    #[serde(default)]
    base_color_texture: Option<GltfTextureInfo>,
    #[serde(default = "default_roughness")]
    roughness_factor: f32,
}

fn default_base_color() -> [f32; 4] {
    [1.0, 1.0, 1.0, 1.0]
}

fn default_roughness() -> f32 {
    1.0
}

#[derive(Deserialize)]
#[derive(Clone)]
struct GltfTextureInfo {
    index: usize,
}

#[derive(Deserialize)]
struct GltfTexture {
    #[serde(default)]
    source: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GltfImage {
    #[serde(default)]
    uri: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GltfAccessor {
    #[serde(default)]
    buffer_view: Option<usize>,
    component_type: u32,
    #[serde(rename = "type")]
    attribute_type: String,
    count: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GltfBufferView {
    buffer: usize,
    #[serde(default)]
    byte_offset: usize,
    byte_length: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GltfBuffer {
    #[serde(default)]
    uri: Option<String>,
    #[allow(dead_code)]
    byte_length: usize,
}

const COMPONENT_FLOAT: u32 = 5126;
const COMPONENT_UNSIGNED_SHORT: u32 = 5123;
const COMPONENT_UNSIGNED_INT: u32 = 5125;

/// glTF 加载器：持有解析后的文档与去重缓存。
struct GltfLoader {
    doc: GltfJson,
    base_dir: PathBuf,
    buffers: RefCell<HashMap<usize, Vec<u8>>>,
    mesh_handles: RefCell<HashMap<usize, MeshHandle>>,
    material_handles: RefCell<HashMap<usize, MaterialHandle>>,
    texture_handles: RefCell<HashMap<usize, TextureHandle>>,
}

impl GltfLoader {
    /// 读取 buffer 内容（文件或 data URI，带缓存）。
    fn buffer(&self, index: usize) -> Result<Vec<u8>, GltfError> {
        if let Some(data) = self.buffers.borrow().get(&index) {
            return Ok(data.clone());
        }
        let buffer = self
            .doc
            .buffers
            .get(index)
            .ok_or_else(|| GltfError::OutOfRange(format!("buffer #{index}")))?;
        let data = match &buffer.uri {
            None => {
                return Err(GltfError::Unsupported(
                    "GLB 二进制容器（buffer 无 uri）。请使用 .gltf + .bin 或 data URI".into(),
                ))
            }
            Some(uri) if uri.starts_with("data:") => {
                let payload = uri
                    .split_once("base64,")
                    .ok_or_else(|| GltfError::Unsupported(format!("不支持的 data URI：{uri}")))?
                    .1;
                base64::decode(payload)
                    .ok_or_else(|| GltfError::OutOfRange("buffer 的 base64 数据非法".into()))?
            }
            Some(uri) => std::fs::read(self.base_dir.join(uri))?,
        };
        self.buffers.borrow_mut().insert(index, data.clone());
        Ok(data)
    }

    /// accessor 的平铺 float 数据与分量数（仅 FLOAT）。
    fn read_floats(&self, accessor: usize) -> Result<(Vec<f32>, usize), GltfError> {
        let acc = self
            .doc
            .accessors
            .get(accessor)
            .ok_or_else(|| GltfError::OutOfRange(format!("accessor #{accessor}")))?;
        if acc.component_type != COMPONENT_FLOAT {
            return Err(GltfError::Unsupported(format!(
                "顶点分量类型 {:#x}（仅支持 FLOAT / 5126）",
                acc.component_type
            )));
        }
        let components = match acc.attribute_type.as_str() {
            "SCALAR" => 1,
            "VEC2" => 2,
            "VEC3" => 3,
            "VEC4" => 4,
            other => return Err(GltfError::Unsupported(format!("分量类型 {other}"))),
        };
        let view_index = acc.buffer_view.ok_or_else(|| {
            GltfError::Unsupported("无 bufferView 的 accessor（稀疏存取）".into())
        })?;
        let view = self
            .doc
            .buffer_views
            .get(view_index)
            .ok_or_else(|| GltfError::OutOfRange(format!("bufferView #{view_index}")))?;
        let data = self.buffer(view.buffer)?;
        let start = view.byte_offset;
        let end = start + view.byte_length;
        if end > data.len() || (end - start) < acc.count * components * 4 {
            return Err(GltfError::OutOfRange(format!("accessor #{accessor} 越界")));
        }
        let floats: Vec<f32> = data[start..end]
            .chunks_exact(4)
            .take(acc.count * components)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        Ok((floats, components))
    }

    /// 索引数据（u16 / u32），以 u32 返回。
    fn read_indices(&self, accessor: usize) -> Result<Vec<u32>, GltfError> {
        let acc = self
            .doc
            .accessors
            .get(accessor)
            .ok_or_else(|| GltfError::OutOfRange(format!("accessor #{accessor}")))?;
        let view_index = acc.buffer_view.ok_or_else(|| {
            GltfError::Unsupported("无 bufferView 的 accessor（稀疏存取）".into())
        })?;
        let view = self
            .doc
            .buffer_views
            .get(view_index)
            .ok_or_else(|| GltfError::OutOfRange(format!("bufferView #{view_index}")))?;
        let data = self.buffer(view.buffer)?;
        let start = view.byte_offset;
        let stride = match acc.component_type {
            COMPONENT_UNSIGNED_SHORT => 2,
            COMPONENT_UNSIGNED_INT => 4,
            other => {
                return Err(GltfError::Unsupported(format!(
                    "索引分量类型 {other:#x}（仅支持 u16 / u32）"
                )))
            }
        };
        let end = start + view.byte_length;
        if end > data.len() || (end - start) < acc.count * stride {
            return Err(GltfError::OutOfRange(format!("索引 accessor #{accessor} 越界")));
        }
        let bytes = &data[start..end];
        let indices = match stride {
            2 => bytes
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]) as u32)
                .collect(),
            _ => bytes
                .chunks_exact(4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect(),
        };
        Ok(indices)
    }

    /// 组装单个 primitive 的 CPU 网格。
    fn build_mesh_data(&self, mesh_index: usize, prim_index: usize) -> Result<MeshData, GltfError> {
        let primitive = self
            .doc
            .meshes
            .get(mesh_index)
            .and_then(|m| m.primitives.get(prim_index))
            .ok_or_else(|| GltfError::OutOfRange(format!("mesh #{mesh_index} primitive #{prim_index}")))?;

        let position_accessor = *primitive
            .attributes
            .get("POSITION")
            .ok_or_else(|| GltfError::Missing(format!("mesh #{mesh_index} 的 POSITION 属性")))?;
        let (positions, components) = self.read_floats(position_accessor)?;
        if components != 3 {
            return Err(GltfError::OutOfRange("POSITION 必须是 VEC3".into()));
        }
        let normals = match primitive.attributes.get("NORMAL") {
            Some(&n) => {
                let (data, comps) = self.read_floats(n)?;
                if comps != 3 {
                    return Err(GltfError::OutOfRange("NORMAL 必须是 VEC3".into()));
                }
                Some(data)
            }
            None => None,
        };
        let uvs = match primitive.attributes.get("TEXCOORD_0") {
            Some(&u) => {
                let (data, comps) = self.read_floats(u)?;
                if comps != 2 {
                    return Err(GltfError::OutOfRange("TEXCOORD_0 必须是 VEC2".into()));
                }
                Some(data)
            }
            None => None,
        };

        let vertex_count = positions.len() / 3;
        let indices = match primitive.indices {
            Some(i) => self.read_indices(i)?,
            None => (0..vertex_count as u32).collect(),
        };

        let mut vertices: Vec<Vertex> = (0..vertex_count)
            .map(|i| Vertex {
                position: [positions[i * 3], positions[i * 3 + 1], positions[i * 3 + 2]],
                normal: normals.as_ref().map(|n| [n[i * 3], n[i * 3 + 1], n[i * 3 + 2]]).unwrap_or([0.0; 3]),
                uv: uvs
                    .as_ref()
                    .map(|d| [d[i * 2], d[i * 2 + 1]])
                    .unwrap_or([0.0; 2]),
            })
            .collect();

        // 法线缺失：按三角形面法线填充（数据已按索引展开顺序排列）
        if normals.is_none() {
            for tri in indices.chunks_exact(3) {
                let (a, b, c) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
                let pa = glam::Vec3::from(vertices[a].position);
                let pb = glam::Vec3::from(vertices[b].position);
                let pc = glam::Vec3::from(vertices[c].position);
                let normal = (pb - pa).cross(pc - pa).normalize_or_zero();
                let n = [normal.x, normal.y, normal.z];
                vertices[a].normal = n;
                vertices[b].normal = n;
                vertices[c].normal = n;
            }
        }

        Ok(MeshData { vertices, indices })
    }

    /// 读取图片（外部文件或 data URI）并解码为 CPU 纹理。
    fn load_image(&self, image_index: usize) -> Result<CpuTexture, GltfError> {
        let image = self
            .doc
            .images
            .get(image_index)
            .ok_or_else(|| GltfError::OutOfRange(format!("image #{image_index}")))?;
        let bytes = match &image.uri {
            None => {
                return Err(GltfError::Unsupported(format!(
                    "image #{image_index}：bufferView 引用图片暂不支持，请使用 uri"
                )))
            }
            Some(uri) if uri.starts_with("data:") => {
                let payload = uri
                    .split_once("base64,")
                    .ok_or_else(|| GltfError::Unsupported(format!("不支持的 data URI：{uri}")))?
                    .1;
                base64::decode(payload)
                    .ok_or_else(|| GltfError::OutOfRange("图片的 base64 数据非法".into()))?
            }
            Some(uri) => std::fs::read(self.base_dir.join(uri))?,
        };
        let decoded = image::load_from_memory(&bytes)?;
        let rgba = decoded.to_rgba8();
        let (width, height) = rgba.dimensions();
        Ok(CpuTexture { width, height, rgba: rgba.into_raw() })
    }

    fn texture(&self, world: &mut World, index: usize) -> Result<TextureHandle, GltfError> {
        if let Some(&handle) = self.texture_handles.borrow().get(&index) {
            return Ok(handle);
        }
        let source = self
            .doc
            .textures
            .get(index)
            .and_then(|t| t.source)
            .ok_or_else(|| GltfError::Missing(format!("texture #{index} 的 source")))?;
        let cpu = self.load_image(source)?;
        let handle = world.resource_mut::<TextureStore>().add(cpu);
        self.texture_handles.borrow_mut().insert(index, handle);
        Ok(handle)
    }

    fn material(&self, world: &mut World, index: usize) -> Result<MaterialHandle, GltfError> {
        if let Some(&handle) = self.material_handles.borrow().get(&index) {
            return Ok(handle);
        }
        let def = match self.doc.materials.get(index) {
            None => MaterialDef::default(),
            Some(mat) => {
                let pbr = mat.pbr_metallic_roughness.clone().unwrap_or_default();
                let texture = match &pbr.base_color_texture {
                    Some(info) => self.texture(world, info.index)?,
                    None => TextureHandle(0),
                };
                // Blinn-Phong 近似：越粗糙高光越钝（128 光滑 → 4 哑光）
                let shininess = 4.0 + (1.0 - pbr.roughness_factor.clamp(0.0, 1.0)) * 124.0;
                MaterialDef {
                    base_color: pbr.base_color_factor,
                    emissive: [mat.emissive()[0], mat.emissive()[1], mat.emissive()[2], 1.0],
                    shininess,
                    texture,
                }
            }
        };
        let handle = world.resource_mut::<MaterialStore>().add(def);
        self.material_handles.borrow_mut().insert(index, handle);
        Ok(handle)
    }

    /// 递归生成节点实体树，返回本节点实体。
    fn spawn_node(
        &self,
        world: &mut World,
        node_index: usize,
        parent: Option<iris_core::prelude::Entity>,
    ) -> Result<iris_core::prelude::Entity, GltfError> {
        let node = self
            .doc
            .nodes
            .get(node_index)
            .ok_or_else(|| GltfError::OutOfRange(format!("node #{node_index}")))?;
        let transform = Transform {
            position: node.translation.into(),
            rotation: Quat::from_xyzw(
                node.rotation[0],
                node.rotation[1],
                node.rotation[2],
                node.rotation[3],
            ),
            scale: node.scale.into(),
        };
        let entity = world.spawn((transform,));
        if let Some(parent) = parent {
            world.add_child(parent, entity);
        }

        // mesh 的每个 primitive 一个子实体（独立材质批次）
        if let Some(mesh_index) = node.mesh {
            let primitive_count = self
                .doc
                .meshes
                .get(mesh_index)
                .map(|m| m.primitives.len())
                .unwrap_or(0);
            for prim in 0..primitive_count {
                let mesh_handle = self.primitive_mesh_handle(world, mesh_index, prim)?;
                let material_handle = match self
                    .doc
                    .meshes
                    .get(mesh_index)
                    .and_then(|m| m.primitives.get(prim))
                    .and_then(|p| p.material)
                {
                    Some(mat_index) => self.material(world, mat_index)?,
                    None => MaterialHandle(0),
                };
                let part = world.spawn((Transform::IDENTITY, mesh_handle, material_handle));
                world.add_child(entity, part);
            }
        }

        for &child in &node.children {
            self.spawn_node(world, child, Some(entity))?;
        }
        Ok(entity)
    }

    /// 按 (mesh, primitive) 缓存网格句柄。
    fn primitive_mesh_handle(
        &self,
        world: &mut World,
        mesh_index: usize,
        prim: usize,
    ) -> Result<MeshHandle, GltfError> {
        let key = format_key(mesh_index, prim);
        if let Some(&h) = self.mesh_handles.borrow().get(&key) {
            return Ok(h);
        }
        let handle = world
            .resource_mut::<MeshStore>()
            .add(self.build_mesh_data(mesh_index, prim)?);
        self.mesh_handles.borrow_mut().insert(key, handle);
        Ok(handle)
    }
}

fn format_key(mesh: usize, prim: usize) -> usize {
    // 组合键：(mesh << 16) | prim；primitive 数量在合法 glTF 中远小于 2^16
    (mesh << 16) | prim
}

/// 从 glTF 文件生成实体树，返回场景顶层实体列表。
///
/// 节点层级映射为 Iris 层级：有 mesh 的节点生成一个父实体，
/// 其每个 primitive 挂一个子实体（共享父节点的变换）。
pub fn spawn_gltf(
    world: &mut World,
    path: impl AsRef<Path>,
) -> Result<Vec<iris_core::prelude::Entity>, GltfError> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)?;
    let doc: GltfJson = serde_json::from_str(&text)?;
    if !doc.scenes.is_empty() && doc.scene >= doc.scenes.len() {
        return Err(GltfError::OutOfRange(format!(
            "scene #{} 超出 scenes 长度 {}",
            doc.scene,
            doc.scenes.len()
        )));
    }
    let loader = GltfLoader {
        doc,
        base_dir: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
        buffers: RefCell::new(HashMap::new()),
        mesh_handles: RefCell::new(HashMap::new()),
        material_handles: RefCell::new(HashMap::new()),
        texture_handles: RefCell::new(HashMap::new()),
    };

    // 顶层节点：优先使用 active scene；无 scenes 时以全部无父节点为顶层
    let top_level: Vec<usize> = if loader.doc.scenes.is_empty() {
        let has_parent: Vec<usize> = loader
            .doc
            .nodes
            .iter()
            .flat_map(|n| n.children.iter().copied())
            .collect();
        (0..loader.doc.nodes.len()).filter(|i| !has_parent.contains(i)).collect()
    } else {
        loader.doc.scenes[loader.doc.scene].nodes.clone()
    };

    let mut spawned = Vec::new();
    for node_index in top_level {
        spawned.push(loader.spawn_node(world, node_index, None)?);
    }
    Ok(spawned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset::base64;
    use crate::material::MaterialStore;
    use crate::mesh::MeshStore;
    use crate::texture::TextureStore;

    /// 构造一个内嵌三角形的 glTF JSON（POSITION + indices，无 NORMAL/UV）。
    fn triangle_gltf() -> String {
        let mut buffer: Vec<u8> = Vec::new();
        for p in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            buffer.extend_from_slice(&p.map(f32::to_le_bytes).concat());
        }
        buffer.extend_from_slice(&[0u16, 1, 2].map(u16::to_le_bytes).concat());

        format!(
            r#"{{
                "asset": {{ "version": "2.0" }},
                "buffers": [{{ "uri": "data:application/octet-stream;base64,{}", "byteLength": {} }}],
                "bufferViews": [
                    {{ "buffer": 0, "byteOffset": 0, "byteLength": 36 }},
                    {{ "buffer": 0, "byteOffset": 36, "byteLength": 6 }}
                ],
                "accessors": [
                    {{ "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3" }},
                    {{ "bufferView": 1, "componentType": 5123, "count": 3, "type": "SCALAR" }}
                ],
                "meshes": [{{ "primitives": [{{ "attributes": {{ "POSITION": 0 }}, "indices": 1 }}] }}],
                "nodes": [{{ "mesh": 0, "translation": [2.0, 0.0, 0.0] }}],
                "scenes": [{{ "nodes": [0] }}],
                "scene": 0
            }}"#,
            base64::encode(&buffer),
            buffer.len()
        )
    }

    fn world_with_stores() -> World {
        let mut world = World::new();
        world.insert_resource(MeshStore::new());
        world.insert_resource(MaterialStore::new());
        world.insert_resource(TextureStore::new());
        world
    }

    #[test]
    fn loads_triangle_with_generated_normals() -> Result<(), GltfError> {
        let dir = std::env::temp_dir().join("iris-gltf-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("triangle.gltf");
        std::fs::write(&path, triangle_gltf()).unwrap();

        let mut world = world_with_stores();
        let spawned = spawn_gltf(&mut world, &path)?;
        assert_eq!(spawned.len(), 1);

        // 数据校验
        let meshes = world.resource::<MeshStore>();
        let data = meshes.get(MeshHandle(0)).expect("第一个网格应为句柄 0");
        assert_eq!(data.vertices.len(), 3);
        assert_eq!(data.indices, vec![0, 1, 2]);
        // 法线自动生成：XY 平面上的三角形朝 +Z
        assert!(data.vertices.iter().all(|v| (v.normal[2] - 1.0).abs() < 1e-5));
        // 默认 UV 填充
        assert!(data.vertices.iter().all(|v| v.uv == [0.0, 0.0]));

        // 层级校验：mesh 父节点带平移，primitive 为子实体
        let parent = spawned[0];
        let children = world.children(parent);
        assert_eq!(children.len(), 1, "一个 primitive 应挂一个子实体");
        return Ok(());
    }

    #[test]
    fn node_translation_becomes_local_transform() -> Result<(), GltfError> {
        let dir = std::env::temp_dir().join("iris-gltf-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("triangle.gltf");
        std::fs::write(&path, triangle_gltf()).unwrap();

        let mut world = world_with_stores();
        let spawned = spawn_gltf(&mut world, &path)?;
        let parent = spawned[0];
        let t = *world.get::<Transform>(parent).unwrap();
        assert_eq!(t.position, [2.0, 0.0, 0.0].into());
        Ok(())
    }

    #[test]
    fn rejects_broken_references() {
        let dir = std::env::temp_dir().join("iris-gltf-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("broken.gltf");
        std::fs::write(
            &path,
            r#"{ "asset": {"version": "2.0"}, "nodes": [{}], "scenes": [{"nodes": [7]}], "scene": 0 }"#,
        )
        .unwrap();
        let mut world = world_with_stores();
        let err = spawn_gltf(&mut world, &path).unwrap_err();
        assert!(matches!(err, GltfError::OutOfRange(_)));
    }

    #[test]
    fn rejects_glb_without_uri() {
        let dir = std::env::temp_dir().join("iris-gltf-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("glb.gltf");
        // buffer 无 uri 且被 accessor 实际引用 —— 惰性加载时应报「不支持 GLB」
        std::fs::write(
            &path,
            r#"{
                "asset": {"version": "2.0"},
                "buffers": [{"byteLength": 36}],
                "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": 36}],
                "accessors": [{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"}],
                "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
                "nodes": [{"mesh": 0}],
                "scenes": [{"nodes": [0]}],
                "scene": 0
            }"#,
        )
        .unwrap();
        let mut world = world_with_stores();
        let err = spawn_gltf(&mut world, &path).unwrap_err();
        assert!(matches!(err, GltfError::Unsupported(_)), "实际错误：{err:?}");
    }
}
