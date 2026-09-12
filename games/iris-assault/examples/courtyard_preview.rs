//! 工具：庭院场景多机位预览（离屏渲染，目检验收用）。
//!
//! 与游戏**同一条装配路径**（[`AssaultPlugin`] 的 Startup：关卡档加载 →
//! 程序化建筑 GLB 装配 → 角色 / 相机 / HUD），但相机由本工具接管、不推进
//! 逻辑帧——用来看「建筑本身长什么样」（游戏内视角另见 assault_snapshot）。
//!
//! ```sh
//! cargo run -p iris-assault --example courtyard_preview --release
//! ffmpeg -y -i court_01_hero.bmp -i court_02_corner.bmp -i court_03_fountain.bmp -i court_04_niche.bmp \
//!   -filter_complex "[0][1]hstack[a];[2][3]hstack[b];[a][b]vstack" -update 1 docs/courtyard.png
//! ```

use iris_assault::plugin::AssaultPlugin;
use iris_core::prelude::Stage;
use iris_engine::prelude::*;

/// 机位：`(文件名, 眼位, 注视点, 视场角°)`。
fn views() -> [(&'static str, Vec3, Vec3, f32); 4] {
    [
        (
            "court_01_hero.bmp",
            Vec3::new(9.1, 2.5, 2.0),
            Vec3::new(-2.5, 1.0, -2.0),
            46.0,
        ),
        (
            "court_02_corner.bmp",
            Vec3::new(0.0, 2.2, -1.9),
            Vec3::new(0.0, 1.6, -6.9),
            44.0,
        ),
        (
            "court_03_fountain.bmp",
            Vec3::new(2.9, 1.7, 2.3),
            Vec3::new(0.0, 0.9, 0.0),
            44.0,
        ),
        (
            "court_04_niche.bmp",
            Vec3::new(11.5, 8.5, 11.0),
            Vec3::new(0.0, 0.4, 0.0),
            52.0,
        ),
    ]
}

fn main() {
    let Some(mut renderer) = OffscreenRenderer::new(960, 600) else {
        eprintln!("跳过：环境无 GPU");
        std::process::exit(1);
    };
    let mut app = App::new();
    app.world.insert_resource(Time::default());
    app.world.insert_resource(Input::new());
    app.world.insert_resource(MeshStore::new());
    app.world.insert_resource(Camera::default());
    app.world.insert_resource(DirectionalLight::default());
    app.world.insert_resource(Audio::headless());
    app.world
        .insert_resource(EnvironmentSettings { intensity: 0.35 });
    app.world.insert_resource(Viewport::new(960, 600));
    HierarchyPlugin.build(&mut app);
    TimelinePlugin.build(&mut app);
    PhysicsPlugin.build(&mut app);
    ParticlePlugin.build(&mut app);
    CommandsPlugin.build(&mut app);
    AssaultPlugin::default().build_schedule(&mut app.schedule);
    app.schedule.run(Stage::Startup, &mut app.world);
    // 预览只看建筑：关掉景深 / 运动模糊（它们在游戏里由 Update 逐帧写对焦
    // 距离，本工具不跑 Update ⇒ 默认对焦把整幅画面糊成雾——第一版现象）
    app.world.remove_resource::<DepthOfFieldSettings>();
    app.world.remove_resource::<MotionBlurSettings>();
    iris_core::hierarchy::propagate_transforms(&mut app.world);

    // 相机是**实体挂载**的（ActiveCamera）：改实体位姿，而不是全局 Camera
    // 资源——渲染器优先用挂载相机（第一版四个机位渲染出同一张图的原因）。
    let camera_entity = app
        .world
        .entity_named("assault/camera")
        .expect("主相机实体在位");
    for (name, eye, target, fov) in views() {
        let forward = (target - eye).normalize();
        let rotation = Quat::from_rotation_arc(-Vec3::Z, forward);
        {
            let mut transform = app
                .world
                .get_mut::<Transform>(camera_entity)
                .expect("相机变换在位");
            transform.position = eye;
            transform.rotation = rotation;
        }
        if let Some(mut active) = app.world.get_mut::<ActiveCamera>(camera_entity) {
            active.fov_y_radians = fov.to_radians();
        }
        // 渲染读的是 GlobalTransform：改完局部 Transform 必须传播一次
        // （否则相机停在启动位姿，四个机位只差视场角——第一版现象）
        iris_core::hierarchy::propagate_transforms(&mut app.world);
        if name == "court_04_niche.bmp" {
            // 关阴影对照（定位「重影」是否来自阴影路径）
            app.world.resource_mut::<DirectionalLight>().shadows = false;
            renderer.render(&app.world);
            renderer
                .save_bmp("court_05_noshadow.bmp")
                .expect("写对照 BMP");
            app.world.resource_mut::<DirectionalLight>().shadows = true;
        }
        renderer.render(&app.world);
        renderer.save_bmp(name).expect("写 BMP");
        let stats = renderer.renderer().stats();
        println!(
            "{name}: draw_calls={} instances={} culled={} triangles={}",
            stats.draw_calls, stats.instances, stats.culled_instances, stats.triangles_drawn
        );
    }
    println!("完成：四机位 BMP 已落盘（仓库根）");
}
