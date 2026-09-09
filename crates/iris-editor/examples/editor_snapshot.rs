//! 工具：iris-editor 的离屏验收图（R58a 验收②）。
//!
//! **用与 headless 测试完全相同的方式驱动编辑器**：`EditorPlugin` 的系统链
//! （Startup 开关卡 + Update 相机 / 输入 / 退出）+ `HierarchyPlugin`（全局
//! 变换传播）+ `UiPlugin`（属性面板锚定），`Time::advance` 确定性推进，
//! 不建窗口；关卡内容全部由**编辑命令层**（`iris_editor::edit`）摆出来
//! ——即验收图里的每一个实体都走过「摆放 → 登记 → 存档 → 重载」的真实
//! 通路，不是手搓的 World。
//!
//! 输出两帧 1280×720：
//! - `editor_mvp.bmp`：**整体**——测试关卡全貌 + 地面参考网格 + 选中框 +
//!   左上角只读属性面板（转 `docs/editor_mvp.png`）；
//! - `editor_viewport.bmp`：**编辑态近景**——换了视角与选中实体，面板数值
//!   随之变化（转 `docs/editor_viewport.png`）。
//!
//! 转 PNG：
//! ```sh
//! ffmpeg -y -i editor_mvp.bmp docs/editor_mvp.png
//! ffmpeg -y -i editor_viewport.bmp docs/editor_viewport.png
//! ```
//!
//! 同时把演示关卡存成 `target/editor_mvp_demo.level.json`（`Ctrl+S` 的同一条
//! 代码路径），并重载对账实体数；打印两帧 FrameStats。
//!
//! ```sh
//! cargo run -p iris-editor --example editor_snapshot --release
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

use glam::Vec3;
use iris_core::prelude::Stage;
use iris_editor::edit::{
    self, PlaceKind, editable_entities, open_level, panel_text, place_at, select,
};
use iris_editor::shell::EditorPlugin;
use iris_engine::prelude::{
    App, Camera, DirectionalLight, Entity, EnvironmentSettings, HierarchyPlugin, Input, MeshHandle,
    OffscreenRenderer, Plugin, Time, Transform, UiPlugin, Viewport, Visibility, World,
};

/// 演示关卡档（写进 `target/`：不污染仓库内资产，`*.bmp` 与 `/target`
/// 都在 .gitignore 内）。
const DEMO_LEVEL: &str = "target/editor_mvp_demo.level.json";

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
        // 视口与离屏帧缓冲同尺寸：属性面板的左上锚定解析到正确像素
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

    fn frame(&mut self) {
        self.app.world.clear_all_events();
        self.app
            .world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(1.0 / 60.0));
        self.app.schedule.run(Stage::Update, &mut self.app.world);
        self.app
            .schedule
            .run(Stage::PostUpdate, &mut self.app.world);
        self.app.world.resource_mut::<Input>().end_frame();
    }

    /// 取景（相机是编辑器自有资源、不入档，见 `iris_editor::edit` 模块文档）。
    fn look_at(&mut self, eye: Vec3, target: Vec3) {
        let mut camera = self.app.world.resource_mut::<Camera>();
        camera.eye = eye;
        camera.target = target;
    }
}

/// 直接写变换（演示关卡需要**非等比**缩放：a 阶段的热键缩放是等比的，
/// 非等比缩放属 b 阶段的属性编辑——此处由工具代码代劳，不经命令层）。
fn set_transform(world: &mut World, entity: Entity, position: Vec3, scale: Vec3) {
    if let Some(mut transform) = world.get_mut::<Transform>(entity) {
        transform.position = position;
        transform.scale = scale;
    }
    edit::update_gizmo(world);
}

/// 摆放一个图元（切换摆放参数 → 命令层摆放）。
fn place(world: &mut World, kind: PlaceKind, preset: usize, collider: bool, point: Vec3) -> Entity {
    {
        let mut state = world.resource_mut::<edit::EditorState>();
        state.place_kind = kind;
        state.preset = preset;
        state.place_collider = collider;
    }
    place_at(world, point).expect("预设图元参数恒合法")
}

/// 用命令层摆出演示关卡：3×3 地砖 + 4 根柱子 + 一段砖墙 + 中心金属球 +
/// 两盏点光 + 柱顶发光体。
fn build_demo_level(world: &mut World) -> usize {
    // 地砖（plane 4 m 见方，间距 4 m ⇒ 铺满 12×12 m）
    for x in [-4.0f32, 0.0, 4.0] {
        for z in [-4.0f32, 0.0, 4.0] {
            place(world, PlaceKind::Plane, 0, true, Vec3::new(x, 0.0, z));
        }
    }
    // 四角柱子（圆柱等比不够高 ⇒ 工具侧写非等比缩放）
    for x in [-5.0f32, 5.0] {
        for z in [-5.0f32, 5.0] {
            let pillar = place(world, PlaceKind::Cylinder, 0, true, Vec3::new(x, 0.0, z));
            set_transform(
                world,
                pillar,
                Vec3::new(x, 2.0, z),
                Vec3::new(1.0, 2.5, 1.0),
            );
        }
    }
    // 北侧砖墙（5 段立方体拉扁）
    for i in 0..5 {
        let x = -4.0 + i as f32 * 2.0;
        let block = place(world, PlaceKind::Cube, 1, true, Vec3::new(x, 0.0, -5.5));
        set_transform(
            world,
            block,
            Vec3::new(x, 0.75, -5.5),
            Vec3::new(2.0, 1.5, 0.4),
        );
    }
    // 中心金属球（放大一倍）
    let sphere = place(world, PlaceKind::Sphere, 2, true, Vec3::ZERO);
    set_transform(world, sphere, Vec3::new(0.0, 1.0, 0.0), Vec3::splat(2.0));
    // 木质台阶（球前）
    let step = place(world, PlaceKind::Cube, 3, true, Vec3::new(0.0, 0.0, 2.5));
    set_transform(
        world,
        step,
        Vec3::new(0.0, 0.2, 2.5),
        Vec3::new(3.0, 0.4, 1.2),
    );
    // 两盏点光（灯泡网格 + PointLight，抬到 2.5 m 高）
    for (x, z) in [(-3.0f32, 3.0), (3.0, -3.0)] {
        let light = edit::place_light_at(world, Vec3::new(x, 0.0, z)).expect("点光摆放");
        set_transform(world, light, Vec3::new(x, 2.5, z), Vec3::ONE);
    }
    // 柱顶发光体（glow 预设，小立方体）
    for (x, z) in [(-5.0f32, -5.0), (5.0, 5.0)] {
        let lamp = place(world, PlaceKind::Cube, 4, false, Vec3::new(x, 0.0, z));
        set_transform(world, lamp, Vec3::new(x, 4.2, z), Vec3::splat(0.6));
    }

    // 环境光调暖一点（方向光入档，是关卡的一部分）
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.42, -0.78, -0.46).normalize_or_zero(),
        color: Vec3::new(1.0, 0.93, 0.84),
        intensity: 1.2,
        ambient: Vec3::new(0.18, 0.19, 0.23),
        ..DirectionalLight::default()
    });

    select(world, Some(sphere));
    editable_entities(world).len()
}

fn main() {
    let Some(mut renderer) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU");
        return;
    };
    // 每次跑都从空档开始（演示内容确定性）
    let demo = PathBuf::from(DEMO_LEVEL);
    let _ = std::fs::remove_file(&demo);
    if let Some(parent) = demo.parent() {
        std::fs::create_dir_all(parent).expect("target/ 可建");
    }

    let mut game = Harness::new(&demo);
    let count = build_demo_level(&mut game.app.world);
    println!("演示关卡：{count} 个可编辑实体（全部经命令层摆放）");
    assert_eq!(
        count, 24,
        "9 地砖 + 4 柱 + 5 墙段 + 中心球 + 台阶 + 2 点光 + 2 柱顶灯"
    );

    // 保存（Ctrl+S 的同一条路径：瞬态视觉 despawn → save_scene → 重建）
    let written = edit::save_level(&mut game.app.world).expect("保存演示关卡");
    assert_eq!(written, count, "写出的实体数 = 可编辑实体数");
    assert!(demo.exists(), "档已落盘：{}", demo.display());

    // === 帧 A：整体（默认斜俯视角拉远，中心球选中）===
    game.look_at(Vec3::new(15.0, 12.5, 17.0), Vec3::new(0.0, 1.2, 0.0));
    for _ in 0..3 {
        game.frame();
    }
    renderer.render(&game.app.world);
    renderer.save_bmp("editor_mvp.bmp").expect("写 BMP");
    let stats = renderer.renderer().stats();
    println!("=== 帧 A（整体 1280×720）FrameStats ===");
    println!(
        "draw_calls = {}，instances = {}，culled = {}，triangles = {}",
        stats.draw_calls, stats.instances, stats.culled_instances, stats.triangles_drawn
    );
    assert!(stats.instances > 0, "帧 A 应有实例");
    let panel_a = panel_text(&game.app.world);
    assert!(
        panel_a.contains("SELECTED sphere/1"),
        "帧 A 面板应显示选中的金属球：{panel_a}"
    );
    assert!(panel_a.is_ascii(), "面板文本必须纯 ASCII（8×8 位图字体）");

    // === 帧 B：编辑态近景（换视角 + 换选中：北墙第一段）===
    // 清单序：0..8 = 地砖，9..12 = 柱子，13..17 = 墙段
    let wall = editable_entities(&game.app.world)[13];
    select(&mut game.app.world, Some(wall));
    game.look_at(Vec3::new(-1.0, 4.0, 3.0), Vec3::new(-3.0, 0.9, -5.5));
    for _ in 0..2 {
        game.frame();
    }
    renderer.render(&game.app.world);
    renderer.save_bmp("editor_viewport.bmp").expect("写 BMP");
    let stats = renderer.renderer().stats();
    println!("=== 帧 B（编辑态近景）FrameStats ===");
    println!(
        "draw_calls = {}，instances = {}，culled = {}，triangles = {}",
        stats.draw_calls, stats.instances, stats.culled_instances, stats.triangles_drawn
    );
    let panel_b = panel_text(&game.app.world);
    assert!(
        panel_b.contains("SELECTED cube/1"),
        "帧 B 面板应显示换选后的实体：{panel_b}"
    );
    assert_ne!(panel_a, panel_b, "换选中后面板内容应变化");
    println!("--- 帧 B 属性面板 ---\n{panel_b}");

    // 重载对账：档里的实体数与摆出来的一致，且网格 / 材质句柄都在
    let mut reloaded = World::new();
    let report = open_level(&mut reloaded, &demo).expect("演示档可重载");
    assert!(report.loaded);
    assert_eq!(report.editable, count, "重载实体数一致");
    assert_eq!(
        editable_entities(&reloaded)
            .iter()
            .filter(|entity| reloaded.get::<MeshHandle>(**entity).is_some())
            .count(),
        count,
        "每个实体都有网格（登记表登记 ⇒ mesh 段还原成功）"
    );
    assert!(
        reloaded
            .get::<Visibility>(editable_entities(&reloaded)[0])
            .is_none(),
        "可见性缺席 = 可见（引擎缺席语义）"
    );

    println!("已输出 editor_mvp.bmp / editor_viewport.bmp / {DEMO_LEVEL}");
    println!("转 PNG：ffmpeg -y -i editor_mvp.bmp docs/editor_mvp.png");
    println!("转 PNG：ffmpeg -y -i editor_viewport.bmp docs/editor_viewport.png");
}
