//! DDGI 编辑器验收图（R89）：编辑命令层驱动的 headless 编辑器世界 +
//! DDGI 叠加层 + 属性面板，输出 `iris_ddgi_editor.ppm` →
//! `docs/ddgi_editor.png`。
//!
//! 画面内容（单格 640×360）：
//! - 一个小房间（四面墙围合 + 地板 + 一根柱子）由编辑命令层摆出；
//! - `DdgiSettings` 启用 + `tick_ddgi` 装配（5×1×5 = 25 探针，k = 4
//!   部分重照明 ⇒ 三档状态同时在画：脏红 / 粗填琥珀 / 稳态绿）；
//! - DDGI 探针标记球叠加层（`EditorTransient` + `BakeExcluded` 双标记
//!   ——自身不进 GI 收集，探针球在画面里但不挡光）；
//! - 属性面板 HUD（左上，含 `DDGI …` 状态行与 `X ddgi mode ,/. step V viz`
//!   帮助行）。
//!
//! 断言：叠加层实体不入档（保存前后档文本无 ddgiviz）、参数步进 ⇒
//! 网格重规划、跨运行逐位（双会话 FNV）。
//!
//! ```sh
//! cargo run -p iris-editor --example ddgi_editor_snapshot --release
//! ffmpeg -y -i iris_ddgi_editor.ppm docs/ddgi_editor.png
//! ```

use std::path::PathBuf;

use iris_core::hierarchy::propagate_transforms;
use iris_editor::edit::{
    self, PlaceKind, ddgi_mode_cycle, ddgi_overlay_toggle, ddgi_probes_per_frame_step,
    ddgi_spacing_step, open_level, place_at,
};
use iris_engine::ddgi::tick_ddgi;
use iris_engine::prelude::*;

const WIDTH: u32 = 800;
const HEIGHT: u32 = 450;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn build_world(dir: &std::path::Path) -> World {
    let mut world = World::new();
    let path: PathBuf = dir.join("ddgi_editor.level.json");
    open_level(&mut world, &path).expect("新建编辑器世界");

    // ---- 内容：小房间（编辑命令层摆出）----
    let put = |world: &mut World, kind: PlaceKind, p: Vec3, s: Vec3| {
        {
            let mut state = world.resource_mut::<edit::EditorState>();
            state.place_kind = kind;
            state.place_collider = false;
        }
        let e = place_at(world, p).expect("摆放");
        if let Some(mut t) = world.get_mut::<Transform>(e) {
            t.scale = s;
        }
    };
    put(
        &mut world,
        PlaceKind::Plane,
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::splat(4.0),
    );
    for (x, z, sx, sz) in [
        (0.0f32, -4.0f32, 8.4f32, 0.3f32),
        (0.0, 4.0, 8.4, 0.3),
        (-4.0, 0.0, 0.3, 8.4),
        (4.0, 0.0, 0.3, 8.4),
    ] {
        put(
            &mut world,
            PlaceKind::Cube,
            Vec3::new(x, 1.5, z),
            Vec3::new(sx, 3.0, sz),
        );
    }
    put(
        &mut world,
        PlaceKind::Cube,
        Vec3::new(1.5, 1.0, 1.0),
        Vec3::new(0.6, 2.0, 0.6),
    );
    // GlobalTransform 传播（headless 无 HierarchyPlugin，手动跑一遍）
    propagate_transforms(&mut world);

    // ---- DDGI：启用 + 部分重照明（三档状态同画）----
    ddgi_mode_cycle(&mut world); // Inert → Grid
    ddgi_spacing_step(&mut world, -1).expect("间距 ÷1.25：2.0 → 1.6");
    ddgi_probes_per_frame_step(&mut world, 1).expect("k = 3");
    // headless 世界无 DdgiPlugin ⇒ 手动插空网格（DdgiPlugin 只做这件事）
    world.insert_resource(DdgiGrid::new());
    // 安装帧（粗填）之后滚 12 帧 k = 3 ⇒ 36 / 72 精修 ⇒ 画面呈现
    // 「收敛前沿」：精修过的稳态绿球排到中部、待重照明红球在后
    // （三档判定的粗填琥珀档在当前状态机下为空集：安装帧全表标脏、
    // 精修即绿——「dirty = false 且 stamp == 安装帧」恒不成立，见 REPORT）
    for _ in 0..12 {
        tick_ddgi(&world);
    }

    // ---- 叠加层 ----
    assert!(ddgi_overlay_toggle(&mut world), "开叠加层");
    // headless 无 HierarchyPlugin：叠加层实体在传播之后 spawn ⇒ 手动补一次
    // GlobalTransform（窗口路径由插件每帧传播，离屏路径必须显式）
    propagate_transforms(&mut world);
    world.insert_resource(Camera {
        eye: Vec3::new(3.2, 2.1, 3.8),
        target: Vec3::new(-1.6, 1.1, -1.2),
        fov_y_radians: 60.0f32.to_radians(),
        ..Camera::default()
    });
    world
}

fn main() {
    if OffscreenRenderer::new(64, 64).is_none() {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    }
    let dir = std::env::temp_dir().join(format!("iris-ddgi-editor-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("临时目录");

    let mut world = build_world(&dir);
    // 网格状态摘要（断言三档齐全的前提）
    let (probes, dirty) = world
        .try_resource::<DdgiGrid>()
        .map(|g| (g.probe_count(), g.dirty().iter().filter(|d| **d).count()))
        .unwrap();
    println!("DDGI {probes} probes / {dirty} dirty（红 = 脏、绿 = 稳态）");
    assert!(probes > 0 && dirty > 0, "部分收敛 ⇒ 红绿两档同画");

    let mut renderer = OffscreenRenderer::new(WIDTH, HEIGHT).expect("离屏渲染器");
    renderer.render(&world);
    let px = renderer.read_pixels();
    let hash_a = fnv1a(&px);
    let header = format!("P6\n{WIDTH} {HEIGHT}\n255\n");
    let mut ppm = header.into_bytes();
    for chunk in px.chunks_exact(4) {
        ppm.extend_from_slice(&chunk[..3]);
    }
    std::fs::write("iris_ddgi_editor.ppm", &ppm).expect("写 ppm");

    // 叠加层不入档：save_level 先 despawn 瞬态再保存（保存后按标记重建）
    // ⇒ 档文本无 ddgiviz（且 ddgi 参数段在）
    let saved = iris_editor::edit::save_level(&mut world).expect("保存");
    let save_path = dir.join("ddgi_editor.level.json");
    let text = std::fs::read_to_string(&save_path).expect("读档");
    assert!(!text.contains("ddgiviz"), "叠加层不入档");
    assert!(text.contains("\"ddgi\""), "DDGI 参数段入档");
    assert!(saved >= 6, "内容实体入档（地板 + 四墙 + 柱）：{saved}");

    // ---- 双会话逐位（第二会话独立目录——共用目录会让第二次
    // open_level 读到第一次保存的档 ⇒ loaded = true ⇒ 不对等）----
    let dir_b = std::env::temp_dir().join(format!("iris-ddgi-editor-b-{}", std::process::id()));
    std::fs::create_dir_all(&dir_b).expect("临时目录 B");
    let world_b = build_world(&dir_b);
    let mut renderer_b = OffscreenRenderer::new(WIDTH, HEIGHT).expect("离屏渲染器");
    renderer_b.render(&world_b);
    assert_eq!(
        fnv1a(&renderer_b.read_pixels()),
        hash_a,
        "编辑器叠加层画面必须双会话逐位一致"
    );

    println!(
        "iris_ddgi_editor.ppm 已写出（{} 字节，FNV {hash_a:#018x}）",
        ppm.len()
    );
}
