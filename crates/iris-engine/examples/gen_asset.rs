//! 工具：程序化生成示例 glTF 资产。
//!
//! - `assets/cube.gltf` + `cube0.bin`：外部 buffer 的标准 glTF（`model` 示例加载）
//! - `assets/anim_cube.glb`：GLB 容器，带旋转 + 浮动动画（`anim_snapshot` 示例加载）
//! - `assets/skin_arm.glb`：GLB 容器，两段式蒙皮手臂 + 肘关节挥舞动画（`skin_snapshot` 示例加载）
//!
//! ```sh
//! cargo run --example gen_asset --release   # 在仓库根目录执行
//! ```

use iris_render::mesh::primitives;
use iris_render::asset::write_glb;
use iris_render::mesh::{MeshData, SkinVertex, Vertex};

fn main() {
    let cube = primitives::cube();
    write_gltf_cube("assets/cube.gltf", "cube0.bin", &cube);
    write_animated_cube_glb("assets/anim_cube.glb", &cube);
    write_skinned_arm_glb("assets/skin_arm.glb");
    println!("已生成 assets/cube.gltf、assets/cube0.bin、assets/anim_cube.glb 与 assets/skin_arm.glb");
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

/// 把立方体写成 GLB 容器，附带「自转 4s + 上下浮动 4s」循环动画。
///
/// BIN 布局：POSITION | NORMAL | TEXCOORD | INDICES | times_spin(5) |
/// quats_spin(5×16) | times_bob(3) | trans_bob(3×12)，各段 4 字节对齐。
fn write_animated_cube_glb(path: &str, cube: &iris_render::mesh::MeshData) {
    let pos_len = cube.vertices.len() * 12;
    let normal_len = cube.vertices.len() * 12;
    let uv_len = cube.vertices.len() * 8;
    let index_len = cube.indices.len() * 2;

    let mut bin = Vec::new();
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
    // 动画数据（网格数据总长 840 已是 4 的倍数，直接续写）
    let times_spin_off = bin.len();
    let times_spin = [0.0f32, 1.0, 2.0, 3.0, 4.0];
    bin.extend_from_slice(&times_spin.map(f32::to_le_bytes).concat());
    let quats_spin_off = bin.len();
    for deg in [0.0f32, 90.0, 180.0, 270.0, 360.0] {
        let (s, c) = (deg * 0.5).to_radians().sin_cos();
        bin.extend_from_slice(&[0.0f32, s, 0.0, c].map(f32::to_le_bytes).concat());
    }
    let times_bob_off = bin.len();
    bin.extend_from_slice(&[0.0f32, 2.0, 4.0].map(f32::to_le_bytes).concat());
    let trans_bob_off = bin.len();
    for y in [0.0f32, 0.6, 0.0] {
        bin.extend_from_slice(&[0.0f32, y, 0.0].map(f32::to_le_bytes).concat());
    }

    let (tq, tt, tb, tr) = (
        times_spin_off, quats_spin_off, times_bob_off, trans_bob_off,
    );
    let json = format!(
        r#"{{
  "asset": {{ "version": "2.0", "generator": "iris gen_asset" }},
  "buffers": [{{ "byteLength": {buffer_len} }}],
  "bufferViews": [
    {{ "buffer": 0, "byteOffset": 0, "byteLength": {pos_len} }},
    {{ "buffer": 0, "byteOffset": {pos_len}, "byteLength": {normal_len} }},
    {{ "buffer": 0, "byteOffset": {uv_offset}, "byteLength": {uv_len} }},
    {{ "buffer": 0, "byteOffset": {index_offset}, "byteLength": {index_len} }},
    {{ "buffer": 0, "byteOffset": {tq}, "byteLength": 20 }},
    {{ "buffer": 0, "byteOffset": {tt}, "byteLength": 80 }},
    {{ "buffer": 0, "byteOffset": {tb}, "byteLength": 12 }},
    {{ "buffer": 0, "byteOffset": {tr}, "byteLength": 36 }}
  ],
  "accessors": [
    {{ "bufferView": 0, "componentType": 5126, "count": {vertex_count}, "type": "VEC3" }},
    {{ "bufferView": 1, "componentType": 5126, "count": {vertex_count}, "type": "VEC3" }},
    {{ "bufferView": 2, "componentType": 5126, "count": {vertex_count}, "type": "VEC2" }},
    {{ "bufferView": 3, "componentType": 5123, "count": {index_count}, "type": "SCALAR" }},
    {{ "bufferView": 4, "componentType": 5126, "count": 5, "type": "SCALAR" }},
    {{ "bufferView": 5, "componentType": 5126, "count": 5, "type": "VEC4" }},
    {{ "bufferView": 6, "componentType": 5126, "count": 3, "type": "SCALAR" }},
    {{ "bufferView": 7, "componentType": 5126, "count": 3, "type": "VEC3" }}
  ],
  "materials": [
    {{ "pbrMetallicRoughness": {{ "baseColorFactor": [0.25, 0.55, 0.95, 1.0], "metallicFactor": 0.1, "roughnessFactor": 0.3 }} }}
  ],
  "meshes": [
    {{ "name": "Cube", "primitives": [{{ "attributes": {{ "POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2 }}, "indices": 3, "material": 0 }}] }}
  ],
  "nodes": [{{ "name": "SpinCube", "mesh": 0 }}],
  "scenes": [{{ "nodes": [0] }}],
  "scene": 0,
  "animations": [
    {{ "name": "spin_bob",
       "channels": [
         {{ "sampler": 0, "target": {{ "node": 0, "path": "rotation" }} }},
         {{ "sampler": 1, "target": {{ "node": 0, "path": "translation" }} }}
       ],
       "samplers": [
         {{ "input": 4, "output": 5, "interpolation": "LINEAR" }},
         {{ "input": 6, "output": 7, "interpolation": "LINEAR" }}
       ]
    }}
  ]
}}"#,
        buffer_len = bin.len(),
        pos_len = pos_len,
        normal_len = normal_len,
        uv_offset = pos_len + normal_len,
        uv_len = uv_len,
        index_offset = pos_len + normal_len + uv_len,
        index_len = index_len,
        vertex_count = cube.vertices.len(),
        index_count = cube.indices.len(),
        tq = tq,
        tt = tt,
        tb = tb,
        tr = tr,
    );

    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent).expect("创建 assets 目录");
    }
    std::fs::write(path, write_glb(&json, &bin)).expect("写入 GLB");
}

/// 两段式蒙皮手臂 + 地面：下段 y∈[0,1] 绑髋关节，上段 y∈[1,2] 绑肘关节，
/// 肘关节 rotZ 0°→90°→0° / 3s 循环挥舞。`skin_snapshot` 示例加载。
///
/// 节点树：0=臂(mesh+skin) 1=hip 2=elbow(T(0,1,0)) 3=地面；
/// skin.joints=[1,2]，ibm=[I, T(0,-1,0)]。
fn write_skinned_arm_glb(path: &str) {
    // 臂网格：两个细长立方体段
    let mut arm = MeshData { skin: Some(Vec::new()), ..MeshData::default() };
    let add_segment = |data: &mut MeshData, center: [f32; 3], joint: u32| {
        let base = data.vertices.len() as u32;
        let cube = primitives::cube();
        for v in &cube.vertices {
            data.vertices.push(Vertex {
                position: [
                    v.position[0] * 0.3 + center[0],
                    v.position[1] + center[1],
                    v.position[2] * 0.3 + center[2],
                ],
                normal: v.normal,
                uv: v.uv,
            });
        }
        data.indices.extend(cube.indices.iter().map(|i| i + base));
        data.skin
            .as_mut()
            .unwrap()
            .extend(std::iter::repeat_n(
                SkinVertex { joints: [joint, 0, 0, 0], weights: [1.0, 0.0, 0.0, 0.0] },
                cube.vertices.len(),
            ));
    };
    add_segment(&mut arm, [0.0, 0.5, 0.0], 0);
    add_segment(&mut arm, [0.0, 1.5, 0.0], 1);
    let ground = primitives::plane(30.0);

    let mut bin: Vec<u8> = Vec::new();
    let put_vertices = |bin: &mut Vec<u8>, mesh: &MeshData| -> (usize, usize, usize, usize, usize) {
        let pos_off = bin.len();
        for v in &mesh.vertices {
            bin.extend_from_slice(&v.position.map(f32::to_le_bytes).concat());
        }
        let normal_off = bin.len();
        for v in &mesh.vertices {
            bin.extend_from_slice(&v.normal.map(f32::to_le_bytes).concat());
        }
        let uv_off = bin.len();
        for v in &mesh.vertices {
            bin.extend_from_slice(&v.uv.map(f32::to_le_bytes).concat());
        }
        while bin.len() % 4 != 0 {
            bin.push(0);
        }
        (pos_off, normal_off, uv_off, mesh.vertices.len(), mesh.indices.len())
    };
    let (a_pos, a_nrm, a_uv, a_vtx, a_idx) = put_vertices(&mut bin, &arm);
    let a_idx_off = bin.len();
    for i in &arm.indices {
        bin.extend_from_slice(&(*i as u16).to_le_bytes());
    }
    let a_idx_len = bin.len() - a_idx_off;
    while bin.len() % 4 != 0 {
        bin.push(0);
    }
    let a_joint_off = bin.len();
    for s in arm.skin.as_ref().unwrap() {
        bin.extend_from_slice(&s.joints.map(|j| (j as u16).to_le_bytes()).concat());
    }
    let a_joint_len = bin.len() - a_joint_off;
    let a_weight_off = bin.len();
    for s in arm.skin.as_ref().unwrap() {
        bin.extend_from_slice(&s.weights.map(f32::to_le_bytes).concat());
    }
    let a_weight_len = bin.len() - a_weight_off;
    let (g_pos, g_nrm, g_uv, g_vtx, g_idx) = put_vertices(&mut bin, &ground);
    let g_idx_off = bin.len();
    for i in &ground.indices {
        bin.extend_from_slice(&(*i as u16).to_le_bytes());
    }
    let g_idx_len = bin.len() - g_idx_off;
    while bin.len() % 4 != 0 {
        bin.push(0);
    }
    // 肘挥舞：rotZ 0° → 90° → 0°，3s 循环
    let times_off = bin.len();
    bin.extend_from_slice(&[0.0f32, 1.5, 3.0].map(f32::to_le_bytes).concat());
    let quats_off = bin.len();
    for deg in [0.0f32, 90.0, 0.0] {
        let (s, c) = (deg * 0.5).to_radians().sin_cos();
        bin.extend_from_slice(&[0.0f32, 0.0, s, c].map(f32::to_le_bytes).concat());
    }
    // 逆绑定矩阵：髋恒等；肘 bind 全局 = T(0,1,0) → ibm = T(0,-1,0)
    let ibm_off = bin.len();
    for m in [
        iris_core::prelude::Mat4::IDENTITY,
        iris_core::prelude::Mat4::from_translation([0.0, -1.0, 0.0].into()),
    ] {
        for col in m.to_cols_array_2d() {
            bin.extend_from_slice(&col.map(f32::to_le_bytes).concat());
        }
    }

    let json = format!(
        r#"{{
  "asset": {{ "version": "2.0", "generator": "iris gen_asset" }},
  "buffers": [{{ "byteLength": {buffer_len} }}],
  "bufferViews": [
    {{ "buffer": 0, "byteOffset": {a_pos}, "byteLength": {a_pos_len} }},
    {{ "buffer": 0, "byteOffset": {a_nrm}, "byteLength": {a_nrm_len} }},
    {{ "buffer": 0, "byteOffset": {a_uv}, "byteLength": {a_uv_len} }},
    {{ "buffer": 0, "byteOffset": {a_idx_off}, "byteLength": {a_idx_len} }},
    {{ "buffer": 0, "byteOffset": {a_joint_off}, "byteLength": {a_joint_len} }},
    {{ "buffer": 0, "byteOffset": {a_weight_off}, "byteLength": {a_weight_len} }},
    {{ "buffer": 0, "byteOffset": {g_pos}, "byteLength": {g_pos_len} }},
    {{ "buffer": 0, "byteOffset": {g_nrm}, "byteLength": {g_nrm_len} }},
    {{ "buffer": 0, "byteOffset": {g_uv}, "byteLength": {g_uv_len} }},
    {{ "buffer": 0, "byteOffset": {g_idx_off}, "byteLength": {g_idx_len} }},
    {{ "buffer": 0, "byteOffset": {times_off}, "byteLength": 12 }},
    {{ "buffer": 0, "byteOffset": {quats_off}, "byteLength": 48 }},
    {{ "buffer": 0, "byteOffset": {ibm_off}, "byteLength": 128 }}
  ],
  "accessors": [
    {{ "bufferView": 0, "componentType": 5126, "count": {a_vtx}, "type": "VEC3" }},
    {{ "bufferView": 1, "componentType": 5126, "count": {a_vtx}, "type": "VEC3" }},
    {{ "bufferView": 2, "componentType": 5126, "count": {a_vtx}, "type": "VEC2" }},
    {{ "bufferView": 3, "componentType": 5123, "count": {a_idx}, "type": "SCALAR" }},
    {{ "bufferView": 4, "componentType": 5123, "count": {a_vtx}, "type": "VEC4" }},
    {{ "bufferView": 5, "componentType": 5126, "count": {a_vtx}, "type": "VEC4" }},
    {{ "bufferView": 6, "componentType": 5126, "count": {g_vtx}, "type": "VEC3" }},
    {{ "bufferView": 7, "componentType": 5126, "count": {g_vtx}, "type": "VEC3" }},
    {{ "bufferView": 8, "componentType": 5126, "count": {g_vtx}, "type": "VEC2" }},
    {{ "bufferView": 9, "componentType": 5123, "count": {g_idx}, "type": "SCALAR" }},
    {{ "bufferView": 10, "componentType": 5126, "count": 3, "type": "SCALAR" }},
    {{ "bufferView": 11, "componentType": 5126, "count": 3, "type": "VEC4" }},
    {{ "bufferView": 12, "componentType": 5126, "count": 2, "type": "MAT4" }}
  ],
  "materials": [
    {{ "pbrMetallicRoughness": {{ "baseColorFactor": [0.92, 0.48, 0.18, 1.0], "metallicFactor": 0.1, "roughnessFactor": 0.4 }} }},
    {{ "pbrMetallicRoughness": {{ "baseColorFactor": [0.45, 0.47, 0.5, 1.0], "metallicFactor": 0.0, "roughnessFactor": 0.9 }} }}
  ],
  "meshes": [
    {{ "name": "Arm", "primitives": [{{ "attributes": {{ "POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2, "JOINTS_0": 4, "WEIGHTS_0": 5 }}, "indices": 3, "material": 0 }}] }},
    {{ "name": "Ground", "primitives": [{{ "attributes": {{ "POSITION": 6, "NORMAL": 7, "TEXCOORD_0": 8 }}, "indices": 9, "material": 1 }}] }}
  ],
  "skins": [{{ "name": "ArmSkin", "joints": [1, 2], "inverseBindMatrices": 12 }}],
  "nodes": [
    {{ "name": "ArmRoot", "mesh": 0, "skin": 0 }},
    {{ "name": "hip", "children": [2] }},
    {{ "name": "elbow", "translation": [0.0, 1.0, 0.0] }},
    {{ "name": "Ground", "mesh": 1 }}
  ],
  "scenes": [{{ "nodes": [0, 1, 3] }}],
  "scene": 0,
  "animations": [
    {{ "name": "wave",
       "channels": [{{ "sampler": 0, "target": {{ "node": 2, "path": "rotation" }} }}],
       "samplers": [{{ "input": 10, "output": 11, "interpolation": "LINEAR" }}]
    }}
  ]
}}"#,
        buffer_len = bin.len(),
        a_pos = a_pos, a_pos_len = a_nrm - a_pos,
        a_nrm = a_nrm, a_nrm_len = a_uv - a_nrm,
        a_uv = a_uv, a_uv_len = a_idx_off - a_uv,
        a_idx_off = a_idx_off, a_idx_len = a_idx_len,
        a_joint_off = a_joint_off, a_joint_len = a_joint_len,
        a_weight_off = a_weight_off, a_weight_len = a_weight_len,
        g_pos = g_pos, g_pos_len = g_nrm - g_pos,
        g_nrm = g_nrm, g_nrm_len = g_uv - g_nrm,
        g_uv = g_uv, g_uv_len = g_idx_off - g_uv,
        g_idx_off = g_idx_off, g_idx_len = g_idx_len,
        times_off = times_off,
        quats_off = quats_off,
        ibm_off = ibm_off,
        a_vtx = a_vtx, a_idx = a_idx,
        g_vtx = g_vtx, g_idx = g_idx,
    );

    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent).expect("创建 assets 目录");
    }
    std::fs::write(path, write_glb(&json, &bin)).expect("写入 GLB");
}
