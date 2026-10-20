//! meshlet 资产构建工具（R84，虚拟几何体 V1）：GLB → 逐网格确定性聚类
//! → 写出 IRML（`.imesh`）→ 读回验证（位级往返 + 三角守恒）。
//!
//! 测试 GLB 的来源：本例用 [`GltfBuilder`] 现场生成（球 / 立方 / 圆柱，
//! 三角形量级足以产生多簇）并落盘，再经 `spawn_gltf_instance` 的**真实
//! GLB 解析路径**读回。不放 `assets/` 既有 GLB 的原因（诚实清单）：
//! 仓内全部内容资产要么带蒙皮（actor / worm），要么带 TANGENT
//! （assault_courtyard——R59 管线为法线贴图写入切线），V1 容器只载
//! 标准 32 B 顶点、按 MVP 边界一律拒收（边界本身有
//! `is_meshlet_eligible` 判定与跳过分支佐证）。
//!
//! 验证链（全部硬断言，不通过即 panic）：
//! 1. **容器往返位级**：写出 → 读回，顶点 / 索引 / 簇元数据逐字段
//!    位级一致（`to_bits`）；
//! 2. **三角守恒**：Σ 簇三角形数 = 输入三角形数，且 meshlet 展开的
//!    三角形集合（属性三元组位型排序比较）与原网格逐三角相同；
//! 3. **簇上限**：逐簇 ≤ 128 三角 / ≤ 255 顶点（u8 容量口径）。
//!
//! 产物写 `target/meshlet/*.imesh`（不入库——pack 集成与流送是 R87 的
//! 事，见 `iris_render::meshlet` 模块头 MVP 边界）。
//!
//! ```sh
//! cargo run -p iris-engine --example meshlet_build_example --release
//! ```

use iris_engine::gltf_builder::{GltfBuilder, GltfNodeDesc};
use iris_engine::meshlet_builder::{build_meshlets_from_mesh_data, is_meshlet_eligible};
use iris_engine::prelude::*;

/// 用 GltfBuilder 生成测试 GLB（球 2976 三角 / 圆柱 224 三角 / 立方
/// 12 三角——多簇与尾簇两种形态都覆盖）。
fn write_test_glb(path: &str) {
    let mut builder = GltfBuilder::new();
    let mat = builder.material([0.7, 0.7, 0.72, 1.0], 0.1, 0.6, None);
    builder.mesh("Sphere", &MeshData::uv_sphere(1.0, 48, 32), mat);
    builder.mesh("Cube", &MeshData::cube(), mat);
    builder.mesh("Cylinder", &MeshData::cylinder(0.5, 1.0, 32), mat);
    builder.node(GltfNodeDesc::new("Sphere").with_mesh(0));
    builder.node(GltfNodeDesc::new("Cube").with_mesh(1));
    builder.node(GltfNodeDesc::new("Cylinder").with_mesh(2));
    std::fs::write(path, builder.build_glb()).expect("写测试 GLB");
}

fn main() {
    std::fs::create_dir_all("target/meshlet").expect("创建 target/meshlet 输出目录");
    let glb_path = "target/meshlet/test_scene.glb";
    write_test_glb(glb_path);
    println!("测试 GLB 已生成：{glb_path}");

    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    match spawn_gltf_instance(&mut world, glb_path) {
        Ok(instance) => println!("GLB 加载成功：{} 个顶层实体", instance.roots.len()),
        Err(e) => {
            eprintln!("加载 {glb_path} 失败：{e}");
            std::process::exit(1);
        }
    }

    // 逐槽位克隆：构建过程不持有 Store 借用；句柄号直接作为产物编号，
    // 墓碑（已删除槽位）由 `iter` 自动跳过。
    let meshes: Vec<(MeshHandle, MeshData)> = world
        .resource::<MeshStore>()
        .iter()
        .map(|(handle, data)| (handle, data.clone()))
        .collect();
    println!("网格总数：{}", meshes.len());

    let mut built = 0usize;
    let mut skipped = 0usize;
    for (handle, data) in &meshes {
        let i = handle.0;
        if !is_meshlet_eligible(data) {
            println!("  #{i} 跳过：含蒙皮 / 切线 / UV2 属性（V1 容器只载标准 32 B 顶点）");
            skipped += 1;
            continue;
        }
        let in_tris = data.indices.len() / 3;
        if in_tris == 0 {
            println!("  #{i} 跳过：空网格");
            skipped += 1;
            continue;
        }
        let meshlet = build_meshlets_from_mesh_data(data);
        let path = format!("target/meshlet/mesh_{i}.imesh");
        let bytes = meshlet.write_file(&path).expect("写 .imesh");

        // ---- 验证 1：容器往返位级 ----
        let back = MeshletMesh::open(&path).expect("读回 .imesh");
        assert_eq!(back.clusters, meshlet.clusters, "#{i} 簇元数据往返不一致");
        assert_eq!(back.indices, meshlet.indices, "#{i} 索引往返不一致");
        assert_eq!(back.vertices.len(), meshlet.vertices.len());
        for (a, b) in back.vertices.iter().zip(meshlet.vertices.iter()) {
            for (p, q) in a.position.iter().zip(b.position) {
                assert_eq!(p.to_bits(), q.to_bits(), "#{i} 顶点位置位级不一致");
            }
            for (p, q) in a.normal.iter().zip(b.normal) {
                assert_eq!(p.to_bits(), q.to_bits(), "#{i} 顶点法线位级不一致");
            }
            for (p, q) in a.uv.iter().zip(b.uv) {
                assert_eq!(p.to_bits(), q.to_bits(), "#{i} 顶点 UV 位级不一致");
            }
        }

        // ---- 验证 2：三角守恒 + 展开三角形集合与原网格相同 ----
        let clustered: usize = meshlet
            .clusters
            .iter()
            .map(|c| c.triangle_count as usize)
            .sum();
        assert_eq!(clustered, in_tris, "#{i} 三角形数不守恒");
        let expanded = back.expand();
        let triangle_keys = |m: &MeshData| {
            let mut keys: Vec<[u32; 24]> = m
                .indices
                .chunks_exact(3)
                .map(|tri| {
                    let mut key = [0u32; 24];
                    for (slot, &vi) in tri.iter().enumerate() {
                        let v = m.vertices[vi as usize];
                        for (k, f) in v
                            .position
                            .iter()
                            .chain(v.normal.iter())
                            .chain(v.uv.iter())
                            .enumerate()
                        {
                            key[slot * 8 + k] = f.to_bits();
                        }
                    }
                    key
                })
                .collect();
            keys.sort();
            keys
        };
        assert_eq!(
            triangle_keys(&expanded),
            triangle_keys(data),
            "#{i} 展开三角形集合与原网格不一致"
        );

        // ---- 验证 3：簇上限 ----
        for (ci, c) in meshlet.clusters.iter().enumerate() {
            assert!(
                (c.triangle_count as usize) <= 128,
                "#{i} 簇 #{ci} 三角数超上限"
            );
            assert!(
                (c.vertex_count as usize) <= 255,
                "#{i} 簇 #{ci} 顶点数超 u8 容量"
            );
        }

        let avg_tris = clustered as f32 / meshlet.clusters.len() as f32;
        println!(
            "  #{i} → {path}（{bytes} B）：{} 顶点 / {in_tris} 三角 → {} 簇 \
             （均 {avg_tris:.1} 三角/簇），展开顶点 {}（跨簇重复 +{:.1}%）",
            data.vertices.len(),
            meshlet.clusters.len(),
            meshlet.vertices.len(),
            (meshlet.vertices.len() as f32 / data.vertices.len() as f32 - 1.0) * 100.0,
        );
        built += 1;
    }
    assert!(built >= 3, "测试 GLB 的三个网格必须全部簇化成功");
    println!("完成：{built} 个网格已簇化写出，{skipped} 个跳过");
}
