//! 工具：场景序列化验收（离屏渲染，输出 `scene_snapshot.bmp`）。
//!
//! 内容工作流的端到端示范：**关卡数据即文件**——`load_scene` 加载手写的
//! `assets/arena.scene.json`（地面 + 四围墙 + 中央台面 + 台上圆柱 + 两颗静态
//! 球（金属 / 漫反射）+ 一颗悬空动态球 + 点光 + 方向光环境段），世界内全部
//! 实体与组件均来自场景文件，示例代码零场景搭建。装上 `PhysicsPlugin` 确定性
//! 模拟 1.5 s（动态球落到台面静置），离屏渲染验证「场景 + 物理」组合，并
//! headless 断言动态球终态（y 回落到台面高度、速度近似为零）。
//!
//! ```sh
//! cargo run -p iris-engine --example scene_snapshot --release
//! ```

use std::time::Duration;

use iris_core::prelude::{Stage, Time, Vec3};
use iris_engine::prelude::{
    App, Camera, HierarchyPlugin, OffscreenRenderer, PhysicsPlugin, Plugin, RigidBody, Transform,
    load_scene,
};

fn main() {
    env_logger::init();
    let Some(mut renderer) = OffscreenRenderer::new(1280, 720) else {
        eprintln!("无 GPU");
        std::process::exit(1);
    };

    let mut app = App::new();
    // 时间资源（物理系统读 delta）与相机（资源级，v1 不进场景文件，见
    // scene.rs 模块文档）；场景加载自带其余资源兜底
    // （MeshStore / MaterialStore / TextureStore / SceneAssetRegistry）
    app.world.insert_resource(Time::default());
    app.world.insert_resource(Camera::default());

    // 关卡数据即文件：世界里的实体与组件全部来自 arena.scene.json
    let entities = load_scene(&mut app.world, "assets/arena.scene.json")
        .expect("加载 assets/arena.scene.json");
    println!("已加载 {} 个场景实体", entities.len());

    // 物理（Update）+ 层级传播（PostUpdate）——与 DefaultPlugins 同序
    PhysicsPlugin.build(&mut app);
    HierarchyPlugin.build(&mut app);

    // 环绕机位：俯瞰整个竞技场
    {
        let mut camera = app.world.resource_mut::<Camera>();
        camera.eye = Vec3::new(13.0, 9.0, 13.0);
        camera.target = Vec3::new(0.0, 1.0, 0.0);
    }

    // 确定性模拟 1.5 s（90 帧 @ 60Hz 固定步长，推进序与 App::tick 一致）：
    // 动态球（ball-dynamic，初始悬空 y=5）落到台面静置
    for _ in 0..90 {
        app.world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(1.0 / 60.0));
        app.schedule.run(Stage::Update, &mut app.world);
        app.schedule.run(Stage::PostUpdate, &mut app.world);
    }

    // headless 终态断言：动态球应已落到台面上（顶面 y=1，球心静息 y=1.5）
    let ball = app
        .world
        .entity_named("ball-dynamic")
        .expect("场景文件应有 ball-dynamic");
    let y = app
        .world
        .get::<Transform>(ball)
        .expect("球应有 Transform")
        .position
        .y;
    let speed = app
        .world
        .get::<RigidBody>(ball)
        .expect("球应有 RigidBody")
        .velocity
        .length();
    assert!(
        y < 3.0 && y > 1.0,
        "动态球应落在台面上（1.0 < y = {y} < 3.0）"
    );
    assert!(speed < 0.5, "动态球应近似静止，实际速度 {speed}");
    println!("物理终态断言通过：ball-dynamic y = {y:.3}（台面静息 1.5）、速度 {speed:.3} m/s");

    renderer.render(&app.world);
    renderer.save_bmp("scene_snapshot.bmp").expect("写 BMP");
    println!("已输出 scene_snapshot.bmp");
    println!("转 PNG：ffmpeg -y -i scene_snapshot.bmp docs/scene_preview.png（转后删 BMP）");
}
