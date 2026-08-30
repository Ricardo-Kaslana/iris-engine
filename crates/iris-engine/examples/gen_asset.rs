//! 工具：程序化生成 `assets/cube.gltf` + `cube0.bin`（外部 buffer 的标准 glTF 资产）。
//!
//! ```sh
//! cargo run --example gen_asset --release   # 在仓库根目录执行
//! ```
//!
//! 产出的资产随后由 `model.rs` 示例加载，用于验证 glTF 管线的
//! 外部 buffer、POSITION/NORMAL/TEXCOORD、索引与材质完整链路。


use iris_render::mesh::primitives;

fn main() {
    let cube = primitives::cube();
    write_gltf_cube("assets/cube.gltf", "cube0.bin", &cube);
    println!("已生成 assets/cube.gltf 与 assets/cube0.bin");
}

/// 把单位立方体数据写成 glTF（JSON + 外部二进制 buffer）。
fn write_gltf_cube(gltf_path: &str, bin_name: &str, cube: &iris_render::mesh::MeshData) {
    // 布局：POSITION(24×12) | NORMAL(24×12) | TEXCOORD(24×8) | INDICES(36×2)
    let pos_len = cube.vertices.len() * 12;
    let normal_len = cube.vertices.len() * 12;
    let uv_len = cube.vertices.len() * 8;
    let index_len = cube.indices.len() * 2;

    let mut bin = Vec::with_capacity(pos_len + normal_len + uv_len + index_len);
    for v in &cube.vertices {
        bin.extend_from_slice(&v.position.map(f32::to_le_bytes).concat());
    }
    for v in &cube.vertices {
        bin.extend_from_slice(&v.normal.map(f32::to_le_bytes).concat());
    }
    for v in &cube.vertices {
        bin.extend_from_slice(&v.uv.map(f32::to_le_bytes).concat());
    }
    for i in &cube.indices {
        bin.extend_from_slice(&(*i as u16).to_le_bytes());
    }

    let min = cube.bounds().map(|(min, _)| min).unwrap_or([0.0; 3]);
    let max = cube.bounds().map(|(_, max)| max).unwrap_or([0.0; 3]);
    let uv_offset = pos_len + normal_len;
    let index_offset = uv_offset + uv_len;
    let json = format!(
        r#"{{
  "asset": {{ "version": "2.0", "generator": "iris gen_asset" }},
  "buffers": [{{ "uri": "{bin_name}", "byteLength": {buffer_len} }}],
  "bufferViews": [
    {{ "buffer": 0, "byteOffset": 0, "byteLength": {pos_len} }},
    {{ "buffer": 0, "byteOffset": {pos_len}, "byteLength": {normal_len} }},
    {{ "buffer": 0, "byteOffset": {uv_offset}, "byteLength": {uv_len} }},
    {{ "buffer": 0, "byteOffset": {index_offset}, "byteLength": {index_len} }}
  ],
  "accessors": [
    {{ "bufferView": 0, "componentType": 5126, "count": {vertex_count}, "type": "VEC3",
       "min": [{min0:.4}, {min1:.4}, {min2:.4}], "max": [{max0:.4}, {max1:.4}, {max2:.4}] }},
    {{ "bufferView": 1, "componentType": 5126, "count": {vertex_count}, "type": "VEC3" }},
    {{ "bufferView": 2, "componentType": 5126, "count": {vertex_count}, "type": "VEC2" }},
    {{ "bufferView": 3, "componentType": 5123, "count": {index_count}, "type": "SCALAR" }}
  ],
  "materials": [
    {{ "pbrMetallicRoughness": {{ "baseColorFactor": [0.92, 0.48, 0.18, 1.0], "roughnessFactor": 0.35 }} }}
  ],
  "meshes": [
    {{ "name": "Cube", "primitives": [{{ "attributes": {{ "POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2 }}, "indices": 3, "material": 0 }}] }}
  ],
  "nodes": [
    {{ "name": "CubeRoot", "mesh": 0, "rotation": [0.0, 0.38268343, 0.0, 0.92387953] }}
  ],
  "scenes": [{{ "nodes": [0] }}],
  "scene": 0
}}"#,
        buffer_len = bin.len(),
        pos_len = pos_len,
        normal_len = normal_len,
        uv_offset = uv_offset,
        uv_len = uv_len,
        index_offset = index_offset,
        index_len = index_len,
        vertex_count = cube.vertices.len(),
        min0 = min[0], min1 = min[1], min2 = min[2],
        max0 = max[0], max1 = max[1], max2 = max[2],
        index_count = cube.indices.len(),
    );

    if let Some(parent) = std::path::Path::new(gltf_path).parent() {
        std::fs::create_dir_all(parent).expect("创建 assets 目录");
    }
    std::fs::write(gltf_path, json).expect("写入 glTF");
    std::fs::write(
        std::path::Path::new(gltf_path).parent().unwrap().join(bin_name),
        bin,
    )
    .expect("写入 bin");
}
