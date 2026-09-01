//! 集成测试：骨骼蒙皮端到端（GLB 蒙皮加载 → 动画驱动关节 → GPU 顶点位移）。
//!
//! 场景：两段式手臂（下段 y∈[0,1] 绑髋关节，上段 y∈[1,2] 绑肘关节），
//! 肘关节带 rotZ 0°→90° / 1s 动画。t=1 时上段绕肘点 (0,1,0) 扫成
//! 水平（从 (0,1,0) 指向 -X）。核心断言：
//! 1. 臂没有消失（两个时刻的红色像素同数量级）——蒙皮矩阵没有把
//!    顶点抛到无穷远或原点；
//! 2. 左半屏红像素在 t=1 显著增多——顶点确实被关节矩阵移动了
//!    （能同时暴露 IBM 解析、关节映射、storage 上传、vs_skinned 任一环脱落）。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 把 Mat4 以列主序平铺写入 BIN（与 glTF MAT4 accessor 布局一致）。
fn extend_mat4(bin: &mut Vec<u8>, m: glam::Mat4) {
    for col in m.to_cols_array_2d() {
        bin.extend_from_slice(&col.map(f32::to_le_bytes).concat());
    }
}

/// 变换立方体为长条并把蒙皮属性追加进网格。
fn add_arm_segment(data: &mut MeshData, center: [f32; 3], joint: u32) {
    let base = data.vertices.len() as u32;
    let cube = MeshData::cube();
    for v in cube.vertices {
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
    for _ in 0..24 {
        data.skin.get_or_insert_with(Vec::new).push(SkinVertex {
            joints: [joint, 0, 0, 0],
            weights: [1.0, 0.0, 0.0, 0.0],
        });
    }
}

/// 两段臂 + 肘关节 rotZ 0°→90° 动画的 GLB。
fn skinned_arm_glb() -> Vec<u8> {
    let mut data = MeshData {
        skin: Some(Vec::new()),
        ..MeshData::default()
    };
    add_arm_segment(&mut data, [0.0, 0.5, 0.0], 0); // 下段 y∈[0,1]，绑髋
    add_arm_segment(&mut data, [0.0, 1.5, 0.0], 1); // 上段 y∈[1,2]，绑肘

    let mut bin: Vec<u8> = Vec::new();
    let vertex_count = data.vertices.len() as u32;
    let index_count = data.indices.len() as u32;
    for v in &data.vertices {
        bin.extend_from_slice(&v.position.map(f32::to_le_bytes).concat());
    }
    let positions_len = bin.len();
    for v in &data.vertices {
        bin.extend_from_slice(&v.normal.map(f32::to_le_bytes).concat());
    }
    let normals_len = bin.len() - positions_len;
    for i in &data.indices {
        bin.extend_from_slice(&(*i as u16).to_le_bytes());
    }
    let indices_len = bin.len() - positions_len - normals_len;
    while bin.len() % 4 != 0 {
        bin.push(0);
    }
    let joints_offset = bin.len();
    for s in data.skin.as_ref().unwrap() {
        bin.extend_from_slice(&s.joints.map(|j| (j as u16).to_le_bytes()).concat());
    }
    let joints_len = bin.len() - joints_offset;
    let weights_offset = bin.len();
    for s in data.skin.as_ref().unwrap() {
        bin.extend_from_slice(&s.weights.map(f32::to_le_bytes).concat());
    }
    let weights_len = bin.len() - weights_offset;
    // 动画时间轴 0s / 1s
    let times_offset = bin.len();
    bin.extend_from_slice(&[0.0f32, 1.0].map(f32::to_le_bytes).concat());
    // 肘 rotZ 0° → 90° 的四元数 (x, y, z, w)
    let quats_offset = bin.len();
    bin.extend_from_slice(&[0.0f32, 0.0, 0.0, 1.0].map(f32::to_le_bytes).concat());
    let (s, c) = (45f32).to_radians().sin_cos();
    bin.extend_from_slice(&[0.0f32, 0.0, s, c].map(f32::to_le_bytes).concat());
    // 逆绑定矩阵：髋 bind = 恒等；肘 bind 全局 = T(0,1,0) → ibm = T(0,-1,0)
    let ibm_offset = bin.len();
    extend_mat4(&mut bin, glam::Mat4::IDENTITY);
    extend_mat4(
        &mut bin,
        glam::Mat4::from_translation([0.0, -1.0, 0.0].into()),
    );

    let json = format!(
        r#"{{
            "asset": {{"version": "2.0"}},
            "buffers": [{{"byteLength": {}}}],
            "bufferViews": [
                {{"buffer": 0, "byteOffset": 0, "byteLength": {positions_len}}},
                {{"buffer": 0, "byteOffset": {positions_len}, "byteLength": {normals_len}}},
                {{"buffer": 0, "byteOffset": {}, "byteLength": {indices_len}}},
                {{"buffer": 0, "byteOffset": {joints_offset}, "byteLength": {joints_len}}},
                {{"buffer": 0, "byteOffset": {weights_offset}, "byteLength": {weights_len}}},
                {{"buffer": 0, "byteOffset": {times_offset}, "byteLength": 8}},
                {{"buffer": 0, "byteOffset": {quats_offset}, "byteLength": 32}},
                {{"buffer": 0, "byteOffset": {ibm_offset}, "byteLength": 128}}
            ],
            "accessors": [
                {{"bufferView": 0, "componentType": 5126, "count": {vertex_count}, "type": "VEC3"}},
                {{"bufferView": 1, "componentType": 5126, "count": {vertex_count}, "type": "VEC3"}},
                {{"bufferView": 2, "componentType": 5123, "count": {index_count}, "type": "SCALAR"}},
                {{"bufferView": 3, "componentType": 5123, "count": {vertex_count}, "type": "VEC4"}},
                {{"bufferView": 4, "componentType": 5126, "count": {vertex_count}, "type": "VEC4"}},
                {{"bufferView": 5, "componentType": 5126, "count": 2, "type": "SCALAR"}},
                {{"bufferView": 6, "componentType": 5126, "count": 2, "type": "VEC4"}},
                {{"bufferView": 7, "componentType": 5126, "count": 2, "type": "MAT4"}}
            ],
            "materials": [{{"pbrMetallicRoughness": {{
                "baseColorFactor": [0.9, 0.15, 0.1, 1.0],
                "metallicFactor": 0.0, "roughnessFactor": 0.9
            }}, "emissiveFactor": [2.5, 0.28, 0.28]}}],
            "meshes": [{{"primitives": [{{
                "attributes": {{"POSITION": 0, "NORMAL": 1, "JOINTS_0": 3, "WEIGHTS_0": 4}},
                "indices": 2, "material": 0
            }}]}}],
            "skins": [{{"joints": [1, 2], "inverseBindMatrices": 7}}],
            "nodes": [
                {{"mesh": 0, "skin": 0}},
                {{"name": "hip", "children": [2]}},
                {{"name": "elbow", "translation": [0.0, 1.0, 0.0]}}
            ],
            "scenes": [{{"nodes": [0, 1]}}],
            "scene": 0,
            "animations": [{{
                "channels": [{{"sampler": 0, "target": {{"node": 2, "path": "rotation"}}}}],
                "samplers": [{{"input": 5, "output": 6, "interpolation": "LINEAR"}}]
            }}]
        }}"#,
        bin.len(),
        positions_len + normals_len,
    );
    write_glb(&json, &bin)
}

/// 统计「左半屏红像素」「全屏红像素」与红像素的最左/最右列号
/// （自发光红材质，与蓝天空背景易区分）。
fn count_red(px: &[u8], w: u32, h: u32) -> (u32, u32, u32, u32) {
    let mut left = 0u32;
    let mut total = 0u32;
    let mut min_x = u32::MAX;
    let mut max_x = 0u32;
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let (r, g, b) = (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32);
            if r > 100 && r > g + 40 && r > b + 40 {
                total += 1;
                if x < w / 2 {
                    left += 1;
                }
                min_x = min_x.min(x);
                max_x = max_x.max(x);
            }
        }
    }
    (left, total, min_x, max_x)
}

#[test]
fn skinned_arm_bends_with_joint_animation() {
    let dir = std::env::temp_dir().join("iris-skin-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("arm.glb");
    std::fs::write(&path, skinned_arm_glb()).unwrap();

    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 1.0, 5.5),
        target: Vec3::new(0.0, 1.0, 0.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight::default());

    let instance = spawn_gltf_instance(&mut world, &path).expect("GLB 应加载成功");
    assert_eq!(instance.clips.len(), 1, "应有 1 个动画剪辑");
    world.insert(instance.roots[0], instance.player());

    // t = 0：bind 姿势，臂竖直居中
    propagate_transforms(&mut world);
    renderer.render(&world);
    let (w, h) = renderer.size();
    let (left_start, total_start, min_x_start, _) = count_red(&renderer.read_pixels(), w, h);

    // t = 1.0：肘 rotZ 90°，上段水平指向 -X（左半屏）
    {
        let mut player = world.get_mut::<AnimationPlayer>(instance.roots[0]).unwrap();
        player.looping = false;
        player.time = 1.0;
    }
    update_animations(&world);
    propagate_transforms(&mut world);
    renderer.render(&world);
    let (left_end, total_end, min_x_end, _) = count_red(&renderer.read_pixels(), w, h);

    assert!(
        total_start > 200,
        "bind 姿势下臂应可见，红像素 {total_start}"
    );
    assert!(
        total_end > total_start / 2,
        "弯曲后臂不应消失（{total_start} -> {total_end}）"
    );
    // 关节矩阵确实移动了顶点：上段（原在屏幕中央竖直）扫向 -X，
    // 最左红像素应左移约 0.5 单位以上（≥30 像素）
    assert!(
        min_x_start < u32::MAX && min_x_end + 30 < min_x_start,
        "上段应绕肘点扫向 -X：最左红像素列 {min_x_start} -> {min_x_end}"
    );
    assert!(
        left_end > left_start + 150,
        "左半屏红像素应显著增多：{left_start} -> {left_end}"
    );
}

#[test]
fn skinned_arm_casts_matching_shadow() {
    // 蒙皮阴影：开/关阴影不应让画面出现成片变亮（阴影贴图与蒙皮几何
    // 使用同一套关节矩阵，若阴影 pass 掉回 bind 姿势，泄漏区域会异常）。
    let dir = std::env::temp_dir().join("iris-skin-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("arm.glb");
    std::fs::write(&path, skinned_arm_glb()).unwrap();

    let Some(mut renderer) = OffscreenRenderer::new(128, 128) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    // 地面承接阴影；臂弯曲到 90° 使阴影形状明显偏离 bind 姿势
    let build = |shadows: bool| {
        let mut world = World::new();
        world.insert_resource(MeshStore::new());
        world.insert_resource(MaterialStore::new());
        world.insert_resource(TextureStore::new());
        world.insert_resource(Camera {
            eye: Vec3::new(3.0, 3.5, 6.0),
            target: Vec3::new(0.0, 0.8, 0.0),
            ..Camera::default()
        });
        world.insert_resource(DirectionalLight {
            direction: Vec3::new(-0.4, -1.0, -0.3),
            shadows,
            ..DirectionalLight::default()
        });
        let ground = world.resource_mut::<MeshStore>().add(MeshData::plane(30.0));
        let mat = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.55, 0.55, 0.6, 1.0));
        world.spawn((Transform::from_xyz(0.0, 0.0, 0.0), ground, mat));
        let instance = spawn_gltf_instance(&mut world, &path).expect("GLB 应加载成功");
        world.insert(instance.roots[0], instance.player());
        {
            let mut player = world.get_mut::<AnimationPlayer>(instance.roots[0]).unwrap();
            player.looping = false;
            player.time = 1.0;
        }
        update_animations(&world);
        propagate_transforms(&mut world);
        world
    };

    let world = build(true);
    renderer.render(&world);
    let with_shadows = renderer.read_pixels();
    let world = build(false);
    renderer.render(&world);
    let without = renderer.read_pixels();

    assert_eq!(with_shadows.len(), without.len());
    let mut darkened = 0u32;
    let mut brightened = 0u32;
    for i in (0..with_shadows.len()).step_by(4) {
        let on = with_shadows[i] as i32 + with_shadows[i + 1] as i32 + with_shadows[i + 2] as i32;
        let off = without[i] as i32 + without[i + 1] as i32 + without[i + 2] as i32;
        if on < off - 12 {
            darkened += 1;
        } else if on > off + 12 {
            brightened += 1;
        }
    }
    assert!(
        darkened > 30,
        "蒙皮几何体应向地面投影（变暗像素 {darkened}）"
    );
    assert!(
        brightened <= 4,
        "开阴影不应让任何像素变亮，实际 {brightened}"
    );
}

/// 蒙皮实例结构回归（dogfooding II 摩擦①⑦闭合的 GPU 证据）：
/// 兄弟顶层关节树的蒙皮 GLB（mesh 根与 hip 是场景顶层兄弟，`skinned_arm_glb`
/// 正是此布局）生成的实例定位到 (1.5, 0, 0) 后，**整只角色**应渲染在偏移后
/// 的屏幕位置。收编缺失时蒙皮矩阵 `J = inv(mesh全局) × global(joint) × ibm`
/// 中关节全局恒在世界原点，移动 mesh 根会把几何体拉向反方向——臂会出现在
/// 左半屏而不是右半屏。断言覆盖「GLB 文件 → 关节实体 → 关节矩阵 → GPU
/// 顶点位移」全链路：
/// 1. 位移前角色跨屏幕中线（左半屏有红像素）；
/// 2. 位移后全部红像素整体右移越过原角色的最右端，且左半屏（原位置）无
///    红像素——位移前后差分；
/// 3. 红像素总量同数量级（角色没有被蒙皮矩阵撕碎或抛出画面）。
#[test]
fn skinned_instance_follows_root_translation() {
    let dir = std::env::temp_dir().join("iris-skin-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("arm_follow.glb");
    std::fs::write(&path, skinned_arm_glb()).unwrap();

    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 1.0, 5.5),
        target: Vec3::new(0.0, 1.0, 0.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight::default());

    let instance = spawn_gltf_instance(&mut world, &path).expect("GLB 应加载成功");
    assert_eq!(instance.roots.len(), 1, "收编后 mesh 根应是唯一顶层实体");

    // 位移前：bind 姿势臂竖直居中，跨屏幕中线
    propagate_transforms(&mut world);
    renderer.render(&world);
    let (w, h) = renderer.size();
    let (left_before, total_before, min_x_before, max_x_before) =
        count_red(&renderer.read_pixels(), w, h);
    assert!(
        total_before > 200,
        "bind 姿势下臂应可见，红像素 {total_before}"
    );
    assert!(
        left_before > 0 && min_x_before < w / 2,
        "位移前臂应跨屏幕中线（左半屏有红像素）：left {left_before}，min_x {min_x_before}"
    );

    // 实例定位到 (1.5, 0, 0)：fov 45°、视距 5.5 → 臂平面半屏宽 ≈ 2.3，
    // 偏移后仍在画面内（右半屏）
    world
        .get_mut::<Transform>(instance.roots[0])
        .expect("实例根应有 Transform")
        .position = Vec3::new(1.5, 0.0, 0.0);
    propagate_transforms(&mut world);
    renderer.render(&world);
    let (left_after, total_after, min_x_after, _) = count_red(&renderer.read_pixels(), w, h);

    assert!(
        total_after > total_before / 2,
        "位移后角色应完整可见（{total_before} -> {total_after}）"
    );
    assert!(
        min_x_after > max_x_before,
        "整只角色应跟随实例根右移：位移前最右红像素列 {max_x_before}，\
         位移后最左红像素列 {min_x_after}（收编失效时几何体被关节矩阵拉向反方向）"
    );
    assert_eq!(left_after, 0, "原位置（左半屏）不应再有红像素");
}
