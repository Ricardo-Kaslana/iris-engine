//! 工具：iris-editor 的 R58b 离屏验收图（b 阶段验收②）。
//!
//! **用与 headless 测试完全相同的方式驱动编辑器**：`EditorPlugin` 系统链 +
//! `HierarchyPlugin` + `UiPlugin`，`Time::advance` 确定性推进，不建窗口；
//! 场景全部经**编辑命令层**摆出。四帧 1280×720，对应验收四要素：
//!
//! - 帧 A `editor_b_light_before.bmp`：**点光编辑前**（强度 6.0、暖白）+
//!   面板 LIGHT 行；
//! - 帧 B `editor_b_light_after.bmp`：**点光编辑后**（`]`×3 强度
//!   6 → 11.72、`H`×2 色相 +60°——光池明显变亮变色）；
//! - 帧 C `editor_b_bake_before.bmp`：**烘焙前**（无光照贴图）；
//! - 帧 D `editor_b_bake_after.bmp`：**烘焙后 + NavGrid 叠加 + 面板
//!   UNDO / BAKED / NAV 信息**（接触暗化可见、blocked 红片 + 域边框）。
//!
//! 拼图（2×2 → `docs/editor_b.png`）：
//! ```sh
//! ffmpeg -y -i editor_b_light_before.bmp -i editor_b_light_after.bmp \
//!   -filter_complex hstack editor_b_top.png
//! ffmpeg -y -i editor_b_bake_before.bmp -i editor_b_bake_after.bmp \
//!   -filter_complex hstack editor_b_bottom.png
//! ffmpeg -y -i editor_b_top.png -i editor_b_bottom.png \
//!   -filter_complex vstack docs/editor_b.png
//! ```
//!
//! 确定性：帧内所有文本都来自命令层状态（无墙钟——烘焙耗时毫秒数被
//! 确定性状态行覆盖，texel / 实体数为确定值）；主会话可复跑逐位对照。
//!
//! ```sh
//! cargo run -p iris-editor --example editor_b_snapshot --release
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

use glam::Vec3;
use iris_core::prelude::Stage;
use iris_editor::edit::{
    self, bake_and_install, editable_entities, light_hue_step, light_intensity_step, open_level,
    panel_text, place_at, select, toggle_nav_viz,
};
use iris_editor::shell::EditorPlugin;
use iris_engine::prelude::{
    App, Camera, DirectionalLight, Entity, EnvironmentSettings, HierarchyPlugin, Input,
    LightmapBinding, OffscreenRenderer, Plugin, Time, Transform, UiPlugin, Viewport, World,
};

/// 演示关卡档（写进 `target/`：不污染仓库内资产）。
const DEMO_LEVEL: &str = "target/editor_b_demo.level.json";
/// 离屏尺寸（= `Viewport`，属性面板锚定按它解析）。
const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

/// 与 headless 测试同款测试台：虚拟时钟 + 确定性 1/60 s 推进，不建窗口。
struct Harness {
    app: App,
}

impl Harness {
    fn new(level_path: &Path) -> Self {
        let mut app = App::new();
        app.world.insert_resource(Time::default());
        app.world.insert_resource(Input::new());
        app.world.insert_resource(Viewport::new(WIDTH, HEIGHT));
        app.world
            .insert_resource(EnvironmentSettings { intensity: 0.3 });
        HierarchyPlugin.build(&mut app);
        UiPlugin.build(&mut app);
        EditorPlugin {
            level_path: level_path.to_path_buf(),
        }
        .build_schedule(&mut app.schedule);
        let mut harness = Self { app };
        harness
            .app
            .schedule
            .run(Stage::Startup, &mut harness.app.world);
        iris_core::hierarchy::propagate_transforms(&mut harness.app.world);
        harness
    }

    fn frame(&mut self, times: usize) {
        for _ in 0..times {
            self.app.world.clear_all_events();
            {
                let mut time = self.app.world.resource_mut::<Time>();
                time.advance(Duration::from_secs_f64(1.0 / 60.0));
            }
            self.app.schedule.run(Stage::Update, &mut self.app.world);
            self.app
                .schedule
                .run(Stage::PostUpdate, &mut self.app.world);
            self.app.world.resource_mut::<Input>().end_frame();
        }
    }

    /// 取景（相机是编辑器自有资源、不入档）。
    fn look_at(&mut self, eye: Vec3, target: Vec3) {
        let mut camera = self.app.world.resource_mut::<Camera>();
        camera.eye = eye;
        camera.target = target;
    }
}

/// 摆放一个图元（切换摆放参数 → 命令层摆放）。
fn place(
    world: &mut World,
    kind: edit::PlaceKind,
    preset: usize,
    collider: bool,
    point: Vec3,
) -> Entity {
    {
        let mut state = world.resource_mut::<edit::EditorState>();
        state.place_kind = kind;
        state.preset = preset;
        state.place_collider = collider;
    }
    place_at(world, point).expect("预设图元参数恒合法")
}

/// 直接写变换（演示关卡的非等比缩放由工具代码代劳——热键只有等比缩放，
/// 同 R58a 例子的立场）。
fn set_transform(world: &mut World, entity: Entity, position: Vec3, scale: Vec3) {
    if let Some(mut transform) = world.get_mut::<Transform>(entity) {
        transform.position = position;
        transform.scale = scale;
    }
    edit::update_gizmo(world);
}

/// 用命令层摆出演示场景：3×3 地砖 + 北墙三段 + 中心金属球 + 灯柱 +
/// 柱顶点光。返回点光实体。
fn build_demo_level(world: &mut World) -> Entity {
    // 地砖（plane 4 m 见方，间距 4 m ⇒ 铺满 12×12 m）。原点一块刻意
    // **不挂碰撞体**：静态盒全覆盖导航域会没有顶面雕刻种子（§3.9 实测
    // 教训 / §8.33⑤），留一块隐式地面当种子
    for x in [-4.0f32, 0.0, 4.0] {
        for z in [-4.0f32, 0.0, 4.0] {
            let collider = !(x == 0.0 && z == 0.0);
            place(
                world,
                edit::PlaceKind::Plane,
                0,
                collider,
                Vec3::new(x, 0.0, z),
            );
        }
    }
    // 北墙三段（brick，拉扁拉高）
    for i in 0..3u32 {
        let x = -2.0 + i as f32 * 2.0;
        let block = place(
            world,
            edit::PlaceKind::Cube,
            1,
            true,
            Vec3::new(x, 0.0, -5.5),
        );
        set_transform(
            world,
            block,
            Vec3::new(x, 0.75, -5.5),
            Vec3::new(2.0, 1.5, 0.4),
        );
    }
    // 中心金属球
    let sphere = place(
        world,
        edit::PlaceKind::Sphere,
        2,
        true,
        Vec3::new(-2.0, 0.0, 2.0),
    );
    set_transform(world, sphere, Vec3::new(-2.0, 1.0, 2.0), Vec3::splat(2.0));
    // 灯柱（stone 圆柱拉高）+ 柱顶点光
    let pole = place(
        world,
        edit::PlaceKind::Cylinder,
        0,
        true,
        Vec3::new(2.5, 0.0, 0.5),
    );
    set_transform(
        world,
        pole,
        Vec3::new(2.5, 1.0, 0.5),
        Vec3::new(1.0, 2.0, 1.0),
    );
    let light = edit::place_light_at(world, Vec3::new(2.5, 2.2, 0.5)).expect("点光摆放");
    // 黄昏方向光（方向光在档里，烘焙参数独立于它——演示「档内参数」路径）
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.45, -0.75, -0.45).normalize_or_zero(),
        color: Vec3::new(1.0, 0.93, 0.84),
        intensity: 1.2,
        ambient: Vec3::new(0.16, 0.17, 0.21),
        ..DirectionalLight::default()
    });
    light
}

fn main() {
    let Some(mut renderer) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    // 每次跑都从同一档面开始（确定性）。演示档自带 level.bake 段——
    // 演示「档内参数」烘焙路径（atlas 128 / 32 样本，离屏跑得动）。
    let demo = PathBuf::from(DEMO_LEVEL);
    let _ = std::fs::remove_file(&demo);
    // 产物按段内引用名落盘（相对档目录）；陈旧产物在加载期会报「失配」
    // 错误（R57 语义）——重跑前必须一并清掉
    let _ = std::fs::remove_file(
        demo.parent()
            .unwrap_or(Path::new("."))
            .join("editor_b_demo.ilm"),
    );
    if let Some(parent) = demo.parent() {
        std::fs::create_dir_all(parent).expect("target/ 可建");
    }
    std::fs::write(
        &demo,
        r#"{
  "version": 2,
  "level": { "bake": { "sky_samples": 32, "bounces": 1, "texels_per_meter": 4.0,
                        "atlas_size": 256, "artifact": "editor_b_demo.ilm" } },
  "entities": []
}"#,
    )
    .expect("写演示档");

    let mut game = Harness::new(&demo);
    let light = build_demo_level(&mut game.app.world);
    println!(
        "演示关卡：{} 个可编辑实体（全部经命令层摆放）",
        editable_entities(&game.app.world).len()
    );

    // === 帧 A：点光编辑前（选中点光，面板 LIGHT 行可见）===
    select(&mut game.app.world, Some(light));
    game.look_at(Vec3::new(6.5, 4.5, 7.5), Vec3::new(1.6, 1.2, 0.2));
    game.frame(3);
    renderer.render(&game.app.world);
    renderer
        .save_bmp("editor_b_light_before.bmp")
        .expect("写 BMP");
    let panel_a = panel_text(&game.app.world);
    assert!(
        panel_a.contains("LIGHT I6.00"),
        "帧 A 面板应显示点光初值：{panel_a}"
    );

    // === 帧 B：点光编辑后（`]`×3：6 → 11.72；`H`×2：色相 +60°）===
    for _ in 0..3 {
        light_intensity_step(&mut game.app.world, 1.0).expect("点光模式");
    }
    for _ in 0..2 {
        light_hue_step(&mut game.app.world).expect("点光模式");
    }
    game.frame(2);
    renderer.render(&game.app.world);
    renderer
        .save_bmp("editor_b_light_after.bmp")
        .expect("写 BMP");
    let panel_b = panel_text(&game.app.world);
    assert!(
        panel_b.contains("LIGHT I11.72"),
        "帧 B 面板应显示编辑后强度：{panel_b}"
    );
    assert_ne!(panel_a, panel_b, "编辑后面板应变化");

    // === 帧 C：烘焙前（全景，无光照贴图）===
    game.look_at(Vec3::new(13.5, 11.0, 15.5), Vec3::new(0.0, 0.6, 0.0));
    game.frame(2);
    renderer.render(&game.app.world);
    renderer
        .save_bmp("editor_b_bake_before.bmp")
        .expect("写 BMP");

    // === 烘焙（档内 bake 段参数）+ NavGrid 可视化 ===
    let report = bake_and_install(&mut game.app.world).expect("烘焙成功");
    println!(
        "烘焙：{} texel / {} 实体 / {} ms -> {}",
        report.texels,
        report.entities,
        report.elapsed_ms,
        report.artifact.display()
    );
    assert!(report.from_level_section, "参数来自档内 bake 段");
    assert!(report.artifact.exists(), "产物落盘");
    assert!(
        game.app.world.get::<LightmapBinding>(light).is_some(),
        "光照贴图已挂载（点光灯泡实体拿到 LightmapBinding）"
    );
    // undo 栈已被烘焙清空；演示一次 undo/redo 的面板计数
    assert_eq!(edit::undo_depth(&game.app.world), 0);
    let nav = toggle_nav_viz(&mut game.app.world).expect("导航可视化开");
    println!("导航：{} 格 / {} blocked", nav.cells, nav.blocked);
    assert!(nav.blocked > 0, "墙 / 柱应产生 blocked 格");

    // 帧内文本确定性：烘焙耗时（墙钟）用确定性行覆盖（texel / 实体数
    // 是确定值，可复跑逐位对照）
    {
        let mut state = game.app.world.resource_mut::<edit::EditorState>();
        state.status = format!(
            "BAKED {} tx {} ents -> editor_b_demo.ilm | NAV on",
            report.texels, report.entities
        );
    }
    game.frame(2);
    renderer.render(&game.app.world);
    renderer
        .save_bmp("editor_b_bake_after.bmp")
        .expect("写 BMP");
    let panel_d = panel_text(&game.app.world);
    assert!(
        panel_d.contains("NAV ") && panel_d.contains("blocked"),
        "帧 D 面板应有 NAV 行：{panel_d}"
    );
    assert!(panel_d.contains("BAKED"), "帧 D 状态行应有 BAKED 信息");
    assert!(panel_d.contains("UNDO"), "帧 D 面板应有 UNDO 行");
    assert!(panel_d.is_ascii(), "面板文本必须纯 ASCII（8×8 位图字体）");

    // 保存 + 重载对账（烘焙往返：登记表回补 ⇒ mesh 段还原、产物重挂）
    let written = edit::save_level(&mut game.app.world).expect("保存演示关卡");
    let mut reloaded = World::new();
    let reopened = open_level(&mut reloaded, &demo).expect("重载");
    assert_eq!(reopened.editable, written, "重载实体数一致");

    println!("已输出 4 帧 BMP + {DEMO_LEVEL}（产物 editor_b_demo.ilm）");
    println!("拼图：见文件头注释（ffmpeg hstack/vstack → docs/editor_b.png）");
}
