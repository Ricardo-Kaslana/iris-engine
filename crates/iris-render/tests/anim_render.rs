//! 集成测试：GLB 容器 + glTF 动画端到端（加载 → 播放 → 渲染）。
//!
//! 核心断言：一个朝向相机（+Z）的红色方块带 yaw 0°→80° 旋转动画，
//! 播放到 t=1.0 时方块接近侧对相机，画面中心的红色像素应锐减——
//! 动画确实驱动了几体的世界变换（能同时暴露剪辑解析、节点映射、
//! 播放器推进、GlobalTransform 传播任一环脱落）。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::World;
use iris_render::prelude::*;

/// 红色方块（XY 平面，法线 +Z）+ yaw 0°→80° / 1s 旋转动画的 GLB。
fn animated_quad_glb() -> Vec<u8> {
    let mut bin: Vec<u8> = Vec::new();
    // positions：4 顶点（±1 方块）
    for p in [
        [-1.0f32, -1.0, 0.0],
        [1.0, -1.0, 0.0],
        [1.0, 1.0, 0.0],
        [-1.0, 1.0, 0.0],
    ] {
        bin.extend_from_slice(&p.map(f32::to_le_bytes).concat());
    }
    // normals：全部 +Z
    for _ in 0..4 {
        bin.extend_from_slice(&[0.0f32, 0.0, 1.0].map(f32::to_le_bytes).concat());
    }
    // indices：u16 两三角形
    bin.extend_from_slice(&[0u16, 1, 2, 0, 2, 3].map(u16::to_le_bytes).concat());
    // times 声明在 bufferView 偏移 112 处：显式填充到位
    while bin.len() < 112 {
        bin.push(0);
    }
    // times：0s / 1s
    bin.extend_from_slice(&[0.0f32, 1.0].map(f32::to_le_bytes).concat());
    // quats：yaw 0° → yaw 80°
    let (s, c) = (40f32).to_radians().sin_cos();
    bin.extend_from_slice(&[0.0f32, 0.0, 0.0, 1.0].map(f32::to_le_bytes).concat());
    bin.extend_from_slice(&[0.0f32, s, 0.0, c].map(f32::to_le_bytes).concat());

    let json = r#"{
        "asset": {"version": "2.0"},
        "buffers": [{"byteLength": 152}],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 48},
            {"buffer": 0, "byteOffset": 48, "byteLength": 48},
            {"buffer": 0, "byteOffset": 96, "byteLength": 12},
            {"buffer": 0, "byteOffset": 112, "byteLength": 8},
            {"buffer": 0, "byteOffset": 120, "byteLength": 32}
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3"},
            {"bufferView": 1, "componentType": 5126, "count": 4, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5123, "count": 6, "type": "SCALAR"},
            {"bufferView": 3, "componentType": 5126, "count": 2, "type": "SCALAR"},
            {"bufferView": 4, "componentType": 5126, "count": 2, "type": "VEC4"}
        ],
        "materials": [{"pbrMetallicRoughness": {
            "baseColorFactor": [0.9, 0.15, 0.1, 1.0],
            "metallicFactor": 0.0, "roughnessFactor": 0.9
        }}],
        "meshes": [{"primitives": [{
            "attributes": {"POSITION": 0, "NORMAL": 1}, "indices": 2, "material": 0
        }]}],
        "nodes": [{"mesh": 0}],
        "scenes": [{"nodes": [0]}],
        "scene": 0,
        "animations": [{
            "channels": [{"sampler": 0, "target": {"node": 0, "path": "rotation"}}],
            "samplers": [{"input": 3, "output": 4, "interpolation": "LINEAR"}]
        }]
    }"#;
    write_glb(json, &bin)
}

fn red_dominant_pixels(px: &[u8], w: u32, h: u32) -> u32 {
    let mut count = 0u32;
    for y in (h / 4..h * 3 / 4).step_by(2) {
        for x in (w / 4..w * 3 / 4).step_by(2) {
            let i = ((y * w + x) * 4) as usize;
            let (r, g, b) = (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32);
            if r > 110 && r > g + 40 && r > b + 40 {
                count += 1;
            }
        }
    }
    count
}

#[test]
fn animated_quad_rotates_on_screen() {
    let dir = std::env::temp_dir().join("iris-anim-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("quad.glb");
    std::fs::write(&path, animated_quad_glb()).unwrap();

    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 0.0, 3.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight::default());

    let instance = spawn_gltf_instance(&mut world, &path).expect("GLB 应加载成功");
    let root = instance.roots[0];
    assert_eq!(instance.clips.len(), 1);
    assert!((instance.clips[0].duration - 1.0).abs() < 1e-6);
    world.insert(root, instance.player());

    // t = 0：方块正对相机
    propagate_transforms(&mut world);
    renderer.render(&world);
    let (w, h) = renderer.size();
    let at_start = red_dominant_pixels(&renderer.read_pixels(), w, h);

    // t = 1.0：yaw 80°，接近侧对相机（非循环模式使 time=1.0 不被取模回绕）
    {
        let mut player = world.get_mut::<AnimationPlayer>(root).unwrap();
        player.looping = false;
        player.time = 1.0;
    }
    update_animations(&world);
    propagate_transforms(&mut world);
    renderer.render(&world);
    let at_end = red_dominant_pixels(&renderer.read_pixels(), w, h);

    assert!(at_start > 300, "t=0 方块应充满视野，红色像素 {at_start}");
    assert!(
        at_end * 3 < at_start,
        "旋转到 80° 后红色像素应锐减（{at_start} -> {at_end}）"
    );
}
