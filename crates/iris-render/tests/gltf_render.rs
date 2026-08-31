//! 集成测试：glTF 资产管线端到端（解析 → 实体树 → 渲染像素），
//! 同时验证实体挂载相机（ActiveCamera）与资源相机两条路径。

use std::path::PathBuf;

use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::asset::base64;
use iris_render::prelude::*;

/// 构造一个面向 +Z 的红色 2×2 四边形 glTF（data URI 内嵌 buffer），
/// 节点平移 (0, 0.6, 0)。从 +Z 方向看顶点为 CCW。
fn quad_gltf(path: &PathBuf) {
    let mut buffer: Vec<u8> = Vec::new();
    let positions: [[f32; 3]; 4] = [
        [-1.0, -1.0, 0.0],
        [1.0, -1.0, 0.0],
        [1.0, 1.0, 0.0],
        [-1.0, 1.0, 0.0],
    ];
    for p in positions {
        buffer.extend_from_slice(&p.map(f32::to_le_bytes).concat());
    }
    for _ in 0..4 {
        buffer.extend_from_slice(&[0.0f32, 0.0, 1.0].map(f32::to_le_bytes).concat()); // NORMAL +Z
    }
    buffer.extend_from_slice(&[0u16, 1, 2, 0, 2, 3].map(u16::to_le_bytes).concat());

    let json = format!(
        r#"{{
            "asset": {{ "version": "2.0" }},
            "buffers": [{{ "uri": "data:application/octet-stream;base64,{}", "byteLength": {} }}],
            "bufferViews": [
                {{ "buffer": 0, "byteOffset": 0, "byteLength": 48 }},
                {{ "buffer": 0, "byteOffset": 48, "byteLength": 48 }},
                {{ "buffer": 0, "byteOffset": 96, "byteLength": 12 }}
            ],
            "accessors": [
                {{ "bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3" }},
                {{ "bufferView": 1, "componentType": 5126, "count": 4, "type": "VEC3" }},
                {{ "bufferView": 2, "componentType": 5123, "count": 6, "type": "SCALAR" }}
            ],
            "materials": [{{ "pbrMetallicRoughness": {{ "baseColorFactor": [0.9, 0.1, 0.1, 1.0], "roughnessFactor": 0.5 }} }}],
            "meshes": [{{ "primitives": [{{ "attributes": {{ "POSITION": 0, "NORMAL": 1 }}, "indices": 2, "material": 0 }}] }}],
            "nodes": [{{ "mesh": 0, "translation": [0.0, 0.6, 0.0] }}],
            "scenes": [{{ "nodes": [0] }}],
            "scene": 0
        }}"#,
        base64::encode(&buffer),
        buffer.len()
    );
    std::fs::write(path, json).expect("写入测试 glTF");
}

fn world_with_stores() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());
    world
}

/// 天空背景色以左上角像素为参照（同一高度的角点互为天空；
/// 中心与之相同即说明该处没有几何体）。
fn is_background(px: &[u8], i: usize, sky: usize) -> bool {
    (0..3).all(|c| (px[i + c] as i32 - px[sky + c] as i32).abs() <= 8)
}

#[test]
fn offscreen_renders_gltf_model() {
    let dir = std::env::temp_dir().join("iris-gltf-render");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("quad.gltf");
    quad_gltf(&path);

    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    // 路径一：资源相机（默认看向原点）
    let mut world = world_with_stores();
    let spawned = spawn_gltf(&mut world, &path).expect("glTF 应加载成功");
    assert_eq!(spawned.len(), 1);
    assert_eq!(world.children(spawned[0]).len(), 1, "quad 的 primitive 应为子实体");
    propagate_transforms(&mut world);
    renderer.render(&world);
    let (w, h) = renderer.size();
    let px = renderer.read_pixels();
    let center = ((h / 2) * w + w / 2) as usize * 4;
    let sky = ((2 * w + 2) as usize) * 4;
    assert!(!is_background(&px, center, sky), "资源相机下画面中心应是红色 quad");
    // 红色材质：R 通道应显著高于 G/B
    assert!(
        px[center] as i32 - px[center + 1] as i32 > 40
            && px[center] as i32 - px[center + 2] as i32 > 40,
        "中心像素应偏红，实际 RGBA {:?}",
        &px[center..center + 4]
    );

    // 路径二：挂载相机（绕到 quad 正前方近距离观察）—— 应优先生效
    let mut world = world_with_stores();
    spawn_gltf(&mut world, &path).expect("glTF 应加载成功");
    let rig = world.spawn((Transform::from_xyz(0.0, 0.6, 4.0),));
    let camera = world.spawn((Transform::IDENTITY, ActiveCamera::default()));
    world.add_child(rig, camera);
    propagate_transforms(&mut world);
    renderer.render(&world);
    let px = renderer.read_pixels();
    let center = ((h / 2) * w + w / 2) as usize * 4;
    let sky = ((2 * w + 2) as usize) * 4;
    assert!(!is_background(&px, center, sky), "挂载相机下画面中心应是红色 quad");
    assert!(
        px[center] as i32 - px[center + 1] as i32 > 40,
        "挂载相机路径应看到红色 quad，实际 RGBA {:?}",
        &px[center..center + 4]
    );
}

#[test]
fn gltf_material_roughness_maps_to_shininess() {
    let dir = std::env::temp_dir().join("iris-gltf-render");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("quad.gltf");
    quad_gltf(&path);

    let mut world = world_with_stores();
    spawn_gltf(&mut world, &path).expect("glTF 应加载成功");
    // glTF 金属度/粗糙度直通：quad 材质 roughness 0.5、metallic 未指定（默认 1.0）
    let store = world.resource::<MaterialStore>();
    let material = store.get(MaterialHandle(1)).unwrap();
    assert!((material.roughness - 0.5).abs() < 0.01);
    assert!((material.metallic - 1.0).abs() < 0.01, "glTF 默认金属度为 1.0");
    assert_eq!(material.base_color, [0.9, 0.1, 0.1, 1.0]);
}

/// 仓库自带的 `assets/cube.gltf`（由 `gen_asset` 示例生成并提交）：
/// 外部 .bin buffer + POSITION/NORMAL/TEXCOORD_0 + 索引 + 45° 旋转节点。
#[test]
fn offscreen_renders_repository_cube_asset() {
    let asset = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/cube.gltf");
    if !asset.exists() {
        eprintln!("跳过：assets/cube.gltf 不存在（先运行 gen_asset 示例生成）");
        return;
    }
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    let mut world = world_with_stores();
    let spawned = spawn_gltf(&mut world, &asset).expect("仓库 cube 资产应加载成功");
    assert_eq!(spawned.len(), 1);
    // 模型抬到视野中心
    if let Some(mut t) = world.get_mut::<Transform>(spawned[0]) {
        t.position.y = 0.6;
    }
    propagate_transforms(&mut world);
    renderer.render(&world);

    let (w, h) = renderer.size();
    let px = renderer.read_pixels();
    let center = ((h / 2) * w + w / 2) as usize * 4;
    let sky = ((2 * w + 2) as usize) * 4;
    assert!(!is_background(&px, center, sky), "画面中心应是仓库立方体资产");
    // 材质 baseColorFactor [0.92, 0.48, 0.18]：橙色 → R 明显高于 B
    assert!(
        px[center] as i32 - px[center + 2] as i32 > 30,
        "中心像素应偏橙，实际 RGBA {:?}",
        &px[center..center + 4]
    );
}
