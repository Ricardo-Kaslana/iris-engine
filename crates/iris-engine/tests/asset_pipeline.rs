//! 集成测试：R59 资产管线端到端（headless，零 GPU 依赖）。
//!
//! 全流程钉住「外部资产从导入到运行」一条龙：
//! manifest（优先级 + bc3 标志）→ 异步分帧加载（inline 后端，出队序
//! =（优先级，请求序））→ 动画注册表导入（不 spawn 的元数据解析）→
//! 注册表 spawn（多实例共享剪辑数据，`Arc::ptr_eq` 断言）→ 按名切
//! 剪辑并推进系统 → 姿势精确断言 → bc3 标志落 `TextureStore` 声明表
//! （显存口径 4 B/px → 1 B/px）。GLB fixture 经 [`GltfBuilder`] 程序
//! 化生成（含内嵌带 alpha 贴图与 idle / swing / hit 三条命名剪辑），
//! 挂载走 `Fs` 源（清单 + GLB 落临时目录，进程唯一名防竞态）。

use iris_engine::gltf_builder::{AnimChannel, GltfBuilder, GltfMaterialTextures, GltfNodeDesc};
use iris_engine::prelude::*;
use iris_render::animation::{AnimationInterpolation, AnimationPath};
use iris_render::mesh::MeshData;
use iris_render::texture::CpuTexture;

/// 构造测试 GLB：立柱 + 摇摆板（带 alpha 棋盘格 baseColor 贴图），
/// 三条命名剪辑——idle（板上平移 bob 1 s）、swing（yaw ±40° 1.2 s）、
/// hit（yaw 0→60°→0 快速 jab 0.5 s）。返回 GLB 字节。
fn actor_glb() -> Vec<u8> {
    // 8×8 带 alpha 棋盘格（半透明格 → BC3 eligible；4 对齐）
    let mut base = CpuTexture::checkerboard(2, 4, [30, 120, 220, 90], [240, 240, 240, 255]);
    for (i, px) in base.rgba.chunks_exact_mut(4).enumerate() {
        if i % 2 == 1 {
            px[3] = 90;
        }
    }
    // 法线贴图占位（线性数据贴图；此处内容不影响断言，只求管线真实）
    let (normal, _) = CpuTexture::brick_normal_orm(2, 2, 8, 0.2, 1.0);

    let mut builder = GltfBuilder::new();
    let material = builder.material_textured(
        [1.0, 1.0, 1.0, 1.0],
        0.0,
        0.7,
        None,
        GltfMaterialTextures {
            base_color: Some(base),
            normal: Some(normal),
            orm: None,
        },
    );
    builder.set_alpha_blend(material);
    let board_mesh = builder.mesh("Board", &MeshData::cube(), material);
    let root = builder.node(GltfNodeDesc::new("Root"));
    let board = builder.node(
        GltfNodeDesc::new("Board")
            .with_mesh(board_mesh)
            .at([0.0, 1.0, 0.0]),
    );
    builder.add_child(root, board);

    let yaw = |deg: f32| {
        let (s, c) = (deg * 0.5).to_radians().sin_cos();
        [0.0f32, s, 0.0, c]
    };
    // idle：板上 bob（0 → 0.3 → 0，1 s）
    builder.animation(
        "idle",
        &[AnimChannel {
            node: board,
            path: AnimationPath::Translation,
            interpolation: AnimationInterpolation::Linear,
            times: vec![0.0, 0.5, 1.0],
            values: vec![0.0, 1.0, 0.0, 0.0, 1.3, 0.0, 0.0, 1.0, 0.0],
        }],
    );
    // swing：yaw +40° → −40° → +40°（1.2 s）
    builder.animation(
        "swing",
        &[AnimChannel {
            node: board,
            path: AnimationPath::Rotation,
            interpolation: AnimationInterpolation::Linear,
            times: vec![0.0, 0.6, 1.2],
            values: [40.0f32, -40.0, 40.0]
                .iter()
                .flat_map(|&deg| yaw(deg))
                .collect(),
        }],
    );
    // hit：yaw 0 → 60° → 0（0.5 s 快速 jab）
    builder.animation(
        "hit",
        &[AnimChannel {
            node: board,
            path: AnimationPath::Rotation,
            interpolation: AnimationInterpolation::Linear,
            times: vec![0.0, 0.25, 0.5],
            values: [0.0f32, 60.0, 0.0]
                .iter()
                .flat_map(|&deg| yaw(deg))
                .collect(),
        }],
    );
    builder.build_glb()
}

/// fixture 目录：写 GLB + manifest（support 优先级 0 先加载、actor 5
/// 后加载、bc3 标志挂 actor），返回（目录, 文件已就绪）。
///
/// 目录名带**进程内调用序**：本二进制的两个测试并行线程运行，若共用
/// `pid` 同名目录，一方末尾的 `remove_dir_all` 会删掉另一方仍在读取的
/// 文件（偶发 `NotFound`，R62a 验收实测踩中一次）——与 gltf / hdr 临时
/// 文件竞态同类（AGENTS.md「测试间共享文件名会竞态」第三案），按规约
/// 用唯一名根治。
fn fixture_dir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("iris-r59-pipeline-{}-{seq}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("创建临时目录");
    std::fs::write(dir.join("actor.glb"), actor_glb()).expect("写 actor.glb");
    std::fs::write(dir.join("support.bin"), vec![7u8; 64]).expect("写 support.bin");
    std::fs::write(
        dir.join("manifest.json"),
        r#"{
        "assets": [
            { "path": "support.bin", "kind": "raw", "priority": 0 },
            { "path": "actor.glb", "kind": "raw", "priority": 5, "bc3": true }
        ]
    }"#,
    )
    .expect("写 manifest.json");
    dir
}

fn world_with_stores() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world
}

#[test]
fn manifest_async_registry_spawn_play_by_name_end_to_end() {
    let dir = fixture_dir();

    // ---- 清单：从挂载读入并解析 ----
    let mut mount = AssetMount::new();
    mount.push(AssetSource::Fs { root: dir.clone() });
    let manifest = AssetManifest::from_mount(&mount, "manifest.json").expect("解析清单");
    assert_eq!(manifest.len(), 2);
    assert!(
        manifest.get("actor.glb").unwrap().bc3,
        "actor 条目应带 bc3 标志"
    );

    // ---- 异步分帧加载（inline 后端；线程后端出队序由 async_loading
    //      模块测试钉住）----
    let loader = AssetLoader::inline(mount);
    manifest.request_all(&loader);
    let mut loaded: Vec<(String, Vec<u8>)> = Vec::new();
    while let Some((path, result)) = loader.poll() {
        loaded.push((path, result.expect("fixture 资产应全部可读")));
    }
    // 出队序由清单决定：support（priority 0）先于 actor（priority 5），
    // 尽管字典序 actor 在前
    let order: Vec<&str> = loaded.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        order,
        vec!["support.bin", "actor.glb"],
        "优先级出队：清单 priority 0 的 support 应先于 priority 5 的 actor"
    );
    let actor_bytes = loaded
        .iter()
        .find(|(p, _)| p == "actor.glb")
        .map(|(_, b)| b.clone())
        .unwrap();

    // ---- 注册表：先导入（元数据），再 spawn 两实例（共享剪辑数据）----
    let mut world = world_with_stores();
    let mut registry = AnimationRegistry::new();
    let library = registry
        .import("actor.glb", &actor_bytes)
        .expect("导入 GLB 动画库");
    // 元数据：三条命名剪辑逐项对账
    let meta_names: Vec<&str> = library.clip_meta.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(meta_names, vec!["idle", "swing", "hit"]);
    let hit_meta = library.clip_meta_by_name("hit").unwrap();
    assert!((hit_meta.duration - 0.5).abs() < 1e-6);
    assert_eq!(hit_meta.channels, 1);
    assert_eq!(library.clip_meta_by_name("不存在"), None);

    let first = registry
        .spawn_instance(&mut world, "actor.glb", &actor_bytes)
        .expect("spawn 第一实例");
    let second = registry
        .spawn_instance(&mut world, "actor.glb", &actor_bytes)
        .expect("spawn 第二实例");
    assert!(
        std::sync::Arc::ptr_eq(&first.clips, &second.clips),
        "两实例的剪辑数据必须是同一份分配（共享语义）"
    );
    assert!(std::sync::Arc::ptr_eq(&first.clips, &library.clips));
    assert_eq!(
        std::sync::Arc::strong_count(&library.clips),
        3,
        "注册表 + 两实例共享同一份剪辑数据"
    );

    // ---- bc3 标志落声明表：显存口径 4 B/px → 1 B/px ----
    let textures_before = world.resource::<TextureStore>().gpu_bytes_estimate(true);
    let handle = *first.textures.first().expect("GLB 应登记纹理句柄");
    assert!(
        !world.resource::<TextureStore>().is_bc3_declared(handle),
        "声明前不应有标志"
    );
    for &h in &first.textures {
        world.resource_mut::<TextureStore>().declare_bc3(h);
    }
    let store = world.resource::<TextureStore>();
    assert!(store.is_bc3_declared(handle));
    let textures_after = store.gpu_bytes_estimate(true);
    drop(store);
    // 两实例各登记两张 8×8 纹理（每次 spawn 独立注册——诚实清单在案）：
    // - baseColor（带 alpha，未声明 BC3）→ RGBA8 4 B/px = 256 B；
    // - 法线（brick_normal_orm 全不透明）→ **R66e 起加载器自动声明无压缩**
    //   （BC1 对法线的近距块状伪梯度，frictions ⑨）⇒ RGBA8 256 B，不再
    //   归 BC1。
    // 声明只作用在第一实例的两张句柄上：baseColor → BC3（4 块 ×
    // 16 B = 64 B）；法线句柄的 BC3 声明被无压缩声明**前置否决**（R66e
    // 决策树序），维持 RGBA8。0 号槽位 1×1 白底恒 4 B。
    assert_eq!(
        textures_before,
        4 + 2 * (8 * 8 * 4 + 8 * 8 * 4),
        "声明前：两实例 baseColor 与法线均按 RGBA8 计（R66e 法线自动无压缩）"
    );
    assert_eq!(
        textures_after,
        4 + (4 * 16 + 8 * 8 * 4) + (8 * 8 * 4 + 8 * 8 * 4),
        "声明后：第一实例 baseColor 改按 BC3 1 B/px，法线被无压缩声明否决维持 RGBA8，第二实例不变"
    );

    // ---- 按名播放：swing 负向关键帧应恰为 yaw −40°（第二实例，
    //      验证共享数据可独立驱动）----
    let swing = library
        .clip_index_by_name("swing")
        .expect("应有 swing 剪辑");
    let mut player = second.player();
    player.transition(swing, 0.0);
    player.looping = false;
    player.time = 0.6; // swing 负向关键帧（yaw −40°）
    world.insert(second.roots[0], player);
    update_animations(&world);
    let board = second.node_entities[1].expect("Board 节点应有实体");
    let expected = glam::Quat::from_rotation_y(-40f32.to_radians());
    let transform = world.get::<Transform>(board).unwrap();
    assert!(
        (transform.rotation.dot(expected) - 1.0).abs() < 1e-4,
        "swing@0.3s 应为 yaw −40°，实际 {:?}",
        transform.rotation
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// 中断 / 续载（管线级）：分帧加载收一半后 drop loader，重建 loader
/// 补齐剩余路径——消费方持已得结果、重发未完成请求（loader 无记忆，
/// 幂等重读）。
#[test]
fn pipeline_resumes_after_loader_interruption() {
    let dir = fixture_dir();
    let mut mount = AssetMount::new();
    mount.push(AssetSource::Fs { root: dir.clone() });
    let manifest = AssetManifest::from_mount(&mount, "manifest.json").unwrap();

    // 第一段：support（priority 0）完成后即中断
    let loader = AssetLoader::inline(mount);
    manifest.request_all(&loader);
    let mut got: Vec<(String, Vec<u8>)> = Vec::new();
    if let Some((path, result)) = loader.poll() {
        got.push((path, result.unwrap()));
    }
    drop(loader); // 中断：actor 请求作废
    assert_eq!(got.len(), 1, "中断前应恰有 priority 0 的一条完成");
    assert_eq!(got[0].0, "support.bin");

    // 第二段：重建 loader 续载 actor（幂等重读）
    let mut mount = AssetMount::new();
    mount.push(AssetSource::Fs { root: dir.clone() });
    let loader2 = AssetLoader::inline(mount);
    loader2.request("actor.glb", AssetKind::RawBytes);
    let (path, result) = loader2.poll().unwrap();
    got.push((path, result.unwrap()));

    let mut paths: Vec<&str> = got.iter().map(|(p, _)| p.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(paths, vec!["actor.glb", "support.bin"], "续载应补齐全集");

    std::fs::remove_dir_all(&dir).ok();
}
