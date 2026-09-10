//! 工具：R59 资产管线全流程视觉验收（离屏渲染 → `docs/asset_pipeline.png`）。
//!
//! 全流程（manifest → 优先级异步加载 → 注册表导入 → spawn → 按名动画
//! → 渲染验收），非一次性阻塞：加载线程真线程 + 主循环式分帧 poll。
//! 面板布局（1536×576，image 直接拼接，不依赖 ffmpeg）：
//!
//! - **第一排**：三条命名动画各一帧（idle bob 峰值 / swing 负向
//!   关键帧 / hit jab 峰值）——按名取用（注册表查索引 → 播放器
//!   transition）驱动的姿势必须肉眼可辨；
//! - **第二排左 / 中**：同一 alpha 贴图 BC3 压缩（经 manifest 声明）
//!   与 RGBA8 回退的渲染对照——目检无可见劣化；日志给出真实上传
//!   路径（`texture_is_bc3`）与显存口径数字；
//! - **第二排右**：信息面板（8×8 位图字体，ASCII）——加载时序与
//!   显存对比数字。
//!
//! 先决：`cargo run --example gen_r59_assets --release`（在仓库根执行）。
//!
//! ```sh
//! cargo run --example gen_r59_assets --release
//! cargo run --example r59_snapshot --release
//! ```
//!
//! 产物：`docs/asset_pipeline.png`（1536×576）。

use iris_core::hierarchy::propagate_transforms;
use iris_engine::prelude::*;

const PANEL_W: u32 = 512;
const PANEL_H: u32 = 288;

fn main() {
    env_logger::init();
    let Some(_guard_renderer) = OffscreenRenderer::new(PANEL_W, PANEL_H) else {
        eprintln!("无 GPU");
        std::process::exit(1);
    };
    drop(_guard_renderer);

    // ---- 1. 清单：经挂载读入（清单本身也是一份资产）----
    let mut mount = AssetMount::new();
    mount.push(AssetSource::Fs {
        root: "assets".into(),
    });
    let manifest = AssetManifest::from_mount(&mount, "r59_manifest.json")
        .expect("解析 assets/r59_manifest.json（先跑 gen_r59_assets）");
    println!(
        "清单：{} 条（{}）",
        manifest.len(),
        manifest
            .entries()
            .map(|e| format!("{} prio={} bc3={}", e.path, e.priority, e.bc3))
            .collect::<Vec<_>>()
            .join(" | ")
    );

    // ---- 2. 优先级异步加载：真线程 + 分帧 poll（非一次性阻塞）----
    let loader = AssetLoader::spawn(mount);
    manifest.request_all(&loader);
    let mut loaded: Vec<(String, Vec<u8>)> = Vec::new();
    let mut polls = 0u32;
    while loaded.len() < manifest.len() {
        polls += 1;
        if let Some((path, result)) = loader.poll() {
            println!(
                "帧 {polls}: 完成「{path}」（{} B）",
                result.as_ref().map(|b| b.len()).unwrap_or(0)
            );
            loaded.push((path, result.expect("清单内资产应全部可读")));
        }
        // 模拟「帧间隙」：真线程加载期间 poll 恒非阻塞空转
        if loaded.len() < manifest.len() && polls % 1000 == 0 {
            std::thread::yield_now();
        }
    }
    let actor_bytes = loaded
        .iter()
        .find(|(p, _)| p == "r59_actor.glb")
        .map(|(_, b)| b.clone())
        .expect("清单含 r59_actor.glb");
    let order: Vec<String> = loaded.iter().map(|(p, _)| p.clone()).collect();
    let load_order_text: Vec<String> = order
        .iter()
        .enumerate()
        .map(|(i, p)| format!("#{i} {} (prio {})", p, manifest.get(p).unwrap().priority))
        .collect();

    // ---- 3. 注册表：导入元数据 + 双实例 spawn（共享剪辑数据）----
    let mut world_main = world_with_stores();
    let mut registry = AnimationRegistry::new();
    let library = registry
        .import("r59_actor.glb", &actor_bytes)
        .expect("导入动画库");
    let clip_names: Vec<&str> = library.clip_meta.iter().map(|m| m.name.as_str()).collect();
    println!(
        "注册表：剪辑 {:?}，时长 {:?}，骨架节点 {} 个",
        clip_names,
        library
            .clip_meta
            .iter()
            .map(|m| m.duration)
            .collect::<Vec<_>>(),
        library.node_names.len()
    );
    let first = registry
        .spawn_instance(&mut world_main, "r59_actor.glb", &actor_bytes)
        .expect("spawn 实例 1");
    let second = registry
        .spawn_instance(&mut world_main, "r59_actor.glb", &actor_bytes)
        .expect("spawn 实例 2");
    let shared = std::sync::Arc::ptr_eq(&first.clips, &second.clips);
    let strong = std::sync::Arc::strong_count(&library.clips);
    println!(
        "多实例共享：Arc::ptr_eq(实例1, 实例2) = {shared}，强计数 = {strong}（注册表 + 2 实例）"
    );
    assert!(shared, "注册表命中 spawn 必须共享剪辑数据");
    drop(world_main);

    // ---- 4. BC3 声明：manifest 的 bc3 标志落到第一实例的纹理句柄 ----
    // 显存口径（单实例 store）：声明前 4 + 256(RGBA) + 32(BC1 法线)，
    // 声明后 4 + 64(BC3) + 32(BC1 法线)
    let mem_undeclared = {
        let mut w = world_with_stores();
        registry
            .spawn_instance(&mut w, "r59_actor.glb", &actor_bytes)
            .unwrap();
        w.resource::<TextureStore>().gpu_bytes_estimate(true)
    };
    let mem_declared = {
        let mut w = world_with_stores();
        let inst = registry
            .spawn_instance(&mut w, "r59_actor.glb", &actor_bytes)
            .unwrap();
        for entry in manifest.entries() {
            if entry.bc3 {
                for &h in &inst.textures {
                    w.resource_mut::<TextureStore>().declare_bc3(h);
                }
            }
        }
        w.resource::<TextureStore>().gpu_bytes_estimate(true)
    };
    println!(
        "显存口径（单实例贴图，BC 特性开）：RGBA8+BC1 = {mem_undeclared} B → 声明 BC3 后 = {mem_declared} B"
    );

    // ---- 5. 面板渲染 ----
    // 第一排：三条命名动画的姿势帧（同一渲染器——内容一致的句柄缓存安全）
    let pose_specs = [
        ("idle", 0.5, "IDLE t=0.5 (bob peak)"),
        ("swing", 0.6, "SWING t=0.6 (-40deg)"),
        ("hit", 0.25, "HIT t=0.25 (+60deg peak)"),
    ];
    let mut pose_renderer = OffscreenRenderer::new(PANEL_W, PANEL_H).expect("GPU 渲染器");
    let mut pose_panels = Vec::new();
    for (clip, t, label) in pose_specs {
        let mut world = world_with_stores();
        let instance = registry
            .spawn_instance(&mut world, "r59_actor.glb", &actor_bytes)
            .expect("姿势帧 spawn");
        let index = library
            .clip_index_by_name(clip)
            .unwrap_or_else(|| panic!("剪辑 {clip} 应存在"));
        let mut player = instance.player();
        player.transition(index, 0.0);
        player.looping = false;
        player.time = t;
        world.insert(instance.roots[0], player);
        update_animations(&world);
        let mut panel = render_labeled(&mut pose_renderer, &mut world, label);
        stamp_label(&mut panel, label);
        pose_panels.push(panel);
    }

    // 第二排左：BC3 声明路径；第二排中：RGBA8 回退（同内容不同声明）
    // ——独立渲染器（GPU 纹理缓存按句柄号复用，声明差异必须隔离）
    let mut bc3_renderer = OffscreenRenderer::new(PANEL_W, PANEL_H).expect("GPU 渲染器");
    let mut world_bc3 = world_with_stores();
    let inst_bc3 = registry
        .spawn_instance(&mut world_bc3, "r59_actor.glb", &actor_bytes)
        .expect("BC3 帧 spawn");
    for &h in &inst_bc3.textures {
        world_bc3.resource_mut::<TextureStore>().declare_bc3(h);
    }
    freeze_at(&mut world_bc3, &inst_bc3, &library, "idle", 0.25);
    let bc_supported = bc3_renderer.gpu().supports_bc();
    bc3_renderer.render(&world_bc3);
    let bc3_uploaded = inst_bc3
        .textures
        .iter()
        .any(|&h| bc3_renderer.renderer().texture_is_bc3(h));
    println!("上传路径：BC3 = {bc3_uploaded}（设备 BC 特性 = {bc_supported}）");
    let (w, h) = bc3_renderer.size();
    let panel_bc3 =
        image::RgbaImage::from_raw(w, h, bc3_renderer.read_pixels()).expect("像素缓冲尺寸匹配");

    let mut rgba_renderer = OffscreenRenderer::new(PANEL_W, PANEL_H).expect("GPU 渲染器");
    let mut world_rgba = world_with_stores();
    let inst_rgba = registry
        .spawn_instance(&mut world_rgba, "r59_actor.glb", &actor_bytes)
        .expect("RGBA 帧 spawn");
    freeze_at(&mut world_rgba, &inst_rgba, &library, "idle", 0.25);
    rgba_renderer.render(&world_rgba);
    let (w, h) = rgba_renderer.size();
    let panel_rgba =
        image::RgbaImage::from_raw(w, h, rgba_renderer.read_pixels()).expect("像素缓冲尺寸匹配");

    // ---- 6. 信息面板 + 拼图 ----
    let info = info_panel(&[
        "R59 asset pipeline (manifest -> async -> registry)",
        "load order (priority dequeue):",
        &format!("  {}", load_order_text.first().cloned().unwrap_or_default()),
        &format!("  {}", load_order_text.get(1).cloned().unwrap_or_default()),
        &format!("clips by name: {}", clip_names.join(" / ")),
        &format!("clip data shared: ptr_eq={shared} strong={strong}"),
        &format!("mem(RGBA8+BC1) = {mem_undeclared} B"),
        &format!("mem(BC3+BC1)   = {mem_declared} B"),
        "base tex: 256B RGBA -> 64B BC3 (4x)",
        &format!("BC3 upload = {bc3_uploaded} (device_bc={bc_supported})"),
        "lossy bound: alpha err <= 255/14 per px",
    ]);

    let mut canvas = image::RgbaImage::new(PANEL_W * 3, PANEL_H * 2);
    for (i, panel) in pose_panels.iter().enumerate() {
        image::imageops::replace(&mut canvas, panel, (i as u32 * PANEL_W) as i64, 0);
    }
    image::imageops::replace(&mut canvas, &panel_bc3, 0, PANEL_H as i64);
    image::imageops::replace(&mut canvas, &panel_rgba, PANEL_W as i64, PANEL_H as i64);
    image::imageops::replace(&mut canvas, &info, (PANEL_W * 2) as i64, PANEL_H as i64);

    if let Some(parent) = std::path::Path::new("docs/asset_pipeline.png").parent() {
        std::fs::create_dir_all(parent).expect("创建 docs 目录");
    }
    image::DynamicImage::ImageRgba8(canvas)
        .save("docs/asset_pipeline.png")
        .expect("写 docs/asset_pipeline.png");
    println!(
        "已输出 docs/asset_pipeline.png（{}x{}）",
        PANEL_W * 3,
        PANEL_H * 2
    );
}

/// 建带目录资源的空世界（每面板独立——GPU 缓存按句柄号复用，同渲染器
/// 只渲内容一致的世界）。
fn world_with_stores() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    // 中性灰地面（衬托 alpha 透明与 BC3 / RGBA8 对照）
    let ground_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.30, 0.32, 0.35, 1.0));
    let ground = world.resource_mut::<MeshStore>().add(MeshData::plane(14.0));
    world.spawn((Transform::IDENTITY, ground, ground_mat));
    world.insert_resource(Camera {
        eye: Vec3::new(1.9, 1.7, 2.7),
        target: Vec3::new(0.0, 0.95, 0.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(0.5, -0.6, 0.6).normalize(),
        intensity: 1.15,
        ambient: Vec3::new(0.16, 0.17, 0.20),
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    world
}

/// 把实例定格在指定剪辑的指定时刻（播放器按名切换 + 手动定位）。
fn freeze_at(
    world: &mut World,
    instance: &GltfInstance,
    library: &GltfAnimationLibrary,
    clip: &str,
    t: f32,
) {
    let index = library
        .clip_index_by_name(clip)
        .unwrap_or_else(|| panic!("剪辑 {clip} 应存在"));
    let mut player = instance.player();
    player.transition(index, 0.0);
    player.looping = false;
    player.time = t;
    world.insert(instance.roots[0], player);
    update_animations(world);
    propagate_transforms(world);
}

/// 渲染一帧并读回像素（标签由 `stamp_label` 在读回后补写——渲染器
/// 内部无文本通道，PNG 纯像素路径统一在 CPU 侧标注）。
fn render_labeled(
    renderer: &mut OffscreenRenderer,
    world: &mut World,
    _label: &str,
) -> image::RgbaImage {
    propagate_transforms(world);
    renderer.render(world);
    let (w, h) = renderer.size();
    image::RgbaImage::from_raw(w, h, renderer.read_pixels()).expect("像素缓冲尺寸匹配")
}

/// 用引擎 UI 的 8×8 位图字体（`glyph_bits`，LSB = 最左像素）把标签
/// 戳到面板左上角（白字 + 黑描边底衬，任何背景下可读）。
fn stamp_label(panel: &mut image::RgbaImage, text: &str) {
    draw_text(panel, 8, 6, text, [0, 0, 0, 255]);
    draw_text(panel, 6, 4, text, [255, 255, 255, 255]);
}

/// 在 `img` 的 (x0, y0) 处以 8×8 字体绘制 ASCII 文本（越界像素裁剪）。
fn draw_text(img: &mut image::RgbaImage, x0: i64, y0: i64, text: &str, color: [u8; 4]) {
    for (col, ch) in text.chars().enumerate() {
        if !ch.is_ascii() {
            continue; // 位图字体仅 ASCII（与 UI 文本 v1 同一限制）
        }
        let Some(rows) = glyph_bits(ch as u8) else {
            continue;
        };
        for (ry, &line_bits) in rows.iter().enumerate() {
            for rx in 0..8u32 {
                if (line_bits >> rx) & 1 == 1 {
                    let x = x0 + (col * 8 + rx as usize) as i64;
                    let y = y0 + ry as i64;
                    if x >= 0 && y >= 0 && x < img.width() as i64 && y < img.height() as i64 {
                        img.put_pixel(x as u32, y as u32, image::Rgba(color));
                    }
                }
            }
        }
    }
}

/// 信息面板：深底 + 等宽 8×8 文本行（ASCII）。
fn info_panel(lines: &[&str]) -> image::RgbaImage {
    let mut panel = image::RgbaImage::from_pixel(PANEL_W, PANEL_H, image::Rgba([24, 26, 32, 255]));
    for (i, line) in lines.iter().enumerate() {
        draw_text(
            &mut panel,
            8,
            10 + (i * 14) as i64,
            line,
            [220, 224, 230, 255],
        );
    }
    panel
}
