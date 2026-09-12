//! 工具：Iris Infiltration（iris-infiltration）中段战局视觉验收（离屏
//! 渲染）。用与 headless 测试**完全相同的方式**驱动真实游戏世界：
//! InfiltrationPlugin 系统链 + `HierarchyPlugin` / `AnimationPlugin` /
//! `PhysicsPlugin` / `UiPlugin`，`Time::advance` 确定性推进，不建窗口。
//! 脚本化自动玩家真实推进到中段定格：
//! - 玩家**潜行**中（核心 A 已到手、正穿走廊奔压力板——画面里角色在
//!   走廊东行，守卫 0 在身后 NW 室巡逻）；
//! - 守卫巡逻位姿（混合空间动画 + 足部 IK 贴地）；
//! - 监控屏画中画（R40 RT 相机看核心房 → `UiImageRtt` 右上角）；
//! - HUD（CORES / SPOTTED / ALERT + CAM-02 标签）完整。
//!
//! 输出 `infiltration_mid.bmp`（640×360 一帧）。转 PNG：
//! ```sh
//! ffmpeg -y -i infiltration_mid.bmp docs/infiltration.png
//! ```
//!
//! 同时打印该帧 FrameStats 并断言视锥剔除（`culled_instances > 0`）。
//!
//! ```sh
//! cargo run -p iris-infiltration --example infiltration_snapshot --release
//! ```

use std::time::Duration;

use iris_core::prelude::Stage;
use iris_engine::prelude::{
    AnimationPlugin, App, Audio, Camera, DirectionalLight, EnvironmentSettings, HierarchyPlugin,
    Input, MeshStore, OffscreenRenderer, PhysicsPlugin, Plugin, Time, UiPlugin, Viewport,
};
use iris_infiltration::{
    AutoPilot, CORES, CORES_TEXT_NAME, InfiltrationPlugin, InfiltrationState, PLATE_POS, Phase,
};

/// 与 tests/headless.rs 同款测试台：虚拟时钟 + 确定性 1/60 s 推进。
struct Harness {
    app: App,
}

impl Harness {
    fn new() -> Self {
        let mut app = App::new();
        app.world.insert_resource(Time::default());
        app.world.insert_resource(Input::new());
        app.world.insert_resource(MeshStore::new());
        app.world.insert_resource(Camera::default());
        // 顶光偏斜：室内设施顶光 + 少量环境光（与 headless 同参数）
        app.world.insert_resource(DirectionalLight {
            direction: glam::Vec3::new(-0.4, -1.0, -0.3),
            color: glam::Vec3::new(0.9, 0.93, 1.0),
            intensity: 0.9,
            ambient: glam::Vec3::new(0.22, 0.22, 0.26),
            shadows: false,
            ..DirectionalLight::default()
        });
        app.world.insert_resource(Audio::headless());
        app.world
            .insert_resource(EnvironmentSettings { intensity: 0.3 });
        // 视口与离屏帧缓冲同尺寸：R41 锚定（监控屏画中画贴右上）在
        // 640×360 下解析到正确像素
        app.world.insert_resource(Viewport::new(640, 360));
        HierarchyPlugin.build(&mut app);
        AnimationPlugin.build(&mut app);
        PhysicsPlugin.build(&mut app);
        UiPlugin.build(&mut app);
        InfiltrationPlugin::default().build_schedule(&mut app.schedule);
        let mut harness = Self { app };
        harness
            .app
            .schedule
            .run(Stage::Startup, &mut harness.app.world);
        iris_core::hierarchy::propagate_transforms(&mut harness.app.world);
        harness
    }

    fn frame(&mut self) {
        // 帧首事件轮换（镜像 App::tick 的新生命周期：上帧事件降级为仍可读的
        // 上一代、更早一代丢弃——打磨 V）
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

    /// 自动玩家推进一帧（键盘注入全链路，与 headless 测试同一脚本）。
    fn autopilot_frame(&mut self) {
        use iris_engine::prelude::KeyCode;
        let (move_dir, sneak) = {
            let mut state = self.app.world.resource_mut::<InfiltrationState>();
            let input = AutoPilot.drive(&mut state);
            (input.move_dir, input.sneak)
        };
        let mut input = self.app.world.resource_mut::<Input>();
        for key in [KeyCode::KeyW, KeyCode::KeyS, KeyCode::KeyA, KeyCode::KeyD] {
            input.release(key);
        }
        if move_dir.y < -1e-3 {
            input.press(KeyCode::KeyW);
        }
        if move_dir.y > 1e-3 {
            input.press(KeyCode::KeyS);
        }
        if move_dir.x < -1e-3 {
            input.press(KeyCode::KeyA);
        }
        if move_dir.x > 1e-3 {
            input.press(KeyCode::KeyD);
        }
        if sneak {
            input.press(KeyCode::ShiftLeft);
        } else {
            input.release(KeyCode::ShiftLeft);
        }
    }
}

fn main() {
    let Some(mut renderer) = OffscreenRenderer::new(640, 360) else {
        eprintln!("无 GPU，无法离屏渲染");
        std::process::exit(1);
    };

    let mut game = Harness::new();

    // 自动玩家真实推进：核心 A 已到手（潜行穿过 NW 室）且玩家在走廊
    // 东段（正奔压力板）时定格——中段战局：守卫在身后巡逻、监控屏里
    // 是还没拿的核心 C 房
    let mut framed = false;
    for _ in 0..60 * 90 {
        game.autopilot_frame();
        game.frame();
        let state = game
            .app
            .world
            .try_resource::<InfiltrationState>()
            .expect("状态在位");
        if state.cores[0]
            && state.pos.x > -1.0
            && state.pos.x < 0.5
            && state.pos.z > -0.5
            && state.pos.z < 1.5
            && !state.door_open
        {
            framed = true;
            break;
        }
    }
    assert!(framed, "90 秒内应推进到走廊中段的定格窗口");
    {
        let state = game
            .app
            .world
            .try_resource::<InfiltrationState>()
            .expect("状态在位");
        assert_eq!(state.phase, Phase::Playing);
        assert_eq!(state.cores_collected(), 1, "核心 A 应已到手");
        assert!(!state.door_open, "压力门应尚未打开（定格在踩板前）");
        assert!(state.sneaking, "自动玩家恒潜行");
        println!(
            "定格：玩家在 ({:.2}, {:.2}, {:.2}) 潜行中，核心 {}/{}，被发现 {}",
            state.pos.x,
            state.pos.y,
            state.pos.z,
            state.cores_collected(),
            CORES.len(),
            state.detections
        );
        let plate_dist =
            ((state.pos.x - PLATE_POS.x).powi(2) + (state.pos.z - PLATE_POS.y).powi(2)).sqrt();
        println!("距压力板 {plate_dist:.2} m（奔向 NE 室核心 C 的路上）");
    }

    // 渲染 + 落盘（中段战局主帧）
    renderer.render(&game.app.world);
    renderer.save_bmp("infiltration_mid.bmp").expect("写 BMP");
    let stats = renderer.renderer().stats();
    println!("=== 定格帧 FrameStats（640×360）===");
    println!(
        "draw_calls = {}，instances = {}，culled_instances = {}，triangles = {}",
        stats.draw_calls, stats.instances, stats.culled_instances, stats.triangles_drawn
    );
    assert!(
        stats.culled_instances > 0,
        "设施外墙背面应产生视锥剔除（culled_instances > 0），实际 {}",
        stats.culled_instances
    );

    // HUD 文本对账（画面文字应与逻辑一致）
    let cores_text = game
        .app
        .world
        .entity_named(CORES_TEXT_NAME)
        .and_then(|entity| {
            game.app
                .world
                .get::<iris_engine::prelude::UiText>(entity)
                .map(|t| t.string.clone())
        })
        .expect("CORES HUD 应在位");
    assert_eq!(cores_text, "CORES 1/3");

    println!("已输出 infiltration_mid.bmp");
    println!("转 PNG：ffmpeg -y -i infiltration_mid.bmp docs/infiltration.png");
}
