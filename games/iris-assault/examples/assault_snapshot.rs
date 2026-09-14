//! 工具：Iris Assault（iris-assault）战斗中段视觉验收（离屏渲染）。用与
//! headless 测试**完全相同的方式**驱动真实游戏世界：AssaultPlugin 系统链
//! 加 `HierarchyPlugin` / `AnimationPlugin` / `TimelinePlugin` /
//! `PhysicsPlugin` / `ParticlePlugin` / `UiPlugin` / `CommandsPlugin`，
//! `Time::advance` 确定性推进，不建窗口。
//!
//! 脚本化自动玩家真实推进到战斗中段，定格**两帧**（640×360，横向拼接）：
//!
//! - **帧 A「挥砍」**：玩家刀剑横扫判定窗中、敌人在扇区内——上身动作
//!   剪辑 + DOF 对焦在交战距离（远处置敌人自然焦外虚化）；
//! - **帧 B「枪口焰」**：Shot 事件当帧——FX 池枪口焰粒子 + 弹道。
//!
//! 输出 `assault_a.bmp` / `assault_b.bmp`。转 PNG（两帧横向拼接）：
//! ```sh
//! ffmpeg -y -i assault_a.bmp -i assault_b.bmp -filter_complex hstack docs/assault.png
//! ```
//!
//! 同时打印两帧 FrameStats 并断言视锥剔除（`culled_instances > 0`）。
//!
//! ```sh
//! cargo run -p iris-assault --example assault_snapshot --release
//! ```

use std::time::Duration;

use iris_assault::logic::{AssaultEvent, AssaultState, AutoPilot, Phase, SwordPhase};
use iris_assault::plugin::AssaultPlugin;
use iris_core::prelude::Stage;
use iris_engine::prelude::{
    AnimationPlugin, App, Audio, Camera, CommandsPlugin, DirectionalLight, EnvironmentSettings,
    HierarchyPlugin, Input, KeyCode, MeshStore, OffscreenRenderer, ParticlePlugin, PhysicsPlugin,
    Plugin, Time, TimelinePlugin, UiPlugin, Viewport,
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
        app.world.insert_resource(DirectionalLight::default());
        app.world.insert_resource(Audio::headless());
        app.world
            .insert_resource(EnvironmentSettings { intensity: 0.35 });
        // 视口与离屏帧缓冲同尺寸
        app.world.insert_resource(Viewport::new(640, 360));
        HierarchyPlugin.build(&mut app);
        AnimationPlugin.build(&mut app);
        TimelinePlugin.build(&mut app);
        PhysicsPlugin.build(&mut app);
        ParticlePlugin.build(&mut app);
        UiPlugin.build(&mut app);
        CommandsPlugin.build(&mut app);
        AssaultPlugin::default().build_schedule(&mut app.schedule);
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

    /// 自动玩家推进一帧（键盘注入全链路，与 headless 测试同一脚本）。
    fn autopilot_frame(&mut self) {
        let input: iris_assault::logic::AssaultInput = {
            let mut state = self.app.world.resource_mut::<AssaultState>();
            AutoPilot.drive(&mut state)
        };
        let mut input_res = self.app.world.resource_mut::<Input>();
        for key in [
            KeyCode::KeyW,
            KeyCode::KeyS,
            KeyCode::KeyA,
            KeyCode::KeyD,
            KeyCode::Space,
            KeyCode::KeyF,
            KeyCode::KeyQ,
            KeyCode::KeyR,
        ] {
            input_res.release(key);
        }
        let (x, y) = (input.move_dir.x, input.move_dir.y);
        if y < -1e-3 {
            input_res.press(KeyCode::KeyW);
        }
        if y > 1e-3 {
            input_res.press(KeyCode::KeyS);
        }
        if x < -1e-3 {
            input_res.press(KeyCode::KeyA);
        }
        if x > 1e-3 {
            input_res.press(KeyCode::KeyD);
        }
        if input.sword_press {
            input_res.press(KeyCode::Space);
        }
        if input.fire_held {
            input_res.press(KeyCode::KeyF);
        }
        if input.aim_held {
            input_res.press(KeyCode::KeyQ);
        }
        if input.reload_press {
            input_res.press(KeyCode::KeyR);
        }
    }
}

fn main() {
    let Some(mut renderer) = OffscreenRenderer::new(640, 360) else {
        eprintln!("无 GPU，无法离屏渲染");
        std::process::exit(1);
    };

    let mut game = Harness::new();

    // 自动玩家真实推进：跳过开场（与 headless 的 skip_intro 同一入口
    // 语义——这里直接推帧到 intro_end 之后自然开战，同时让 DOF / 音乐
    // / 演出字幕路径也被真实走过）
    let mut frame_a_done = false;
    let mut frame_b_done = false;
    let mut total_frames = 0usize;
    for _ in 0..(60 * 120) {
        game.autopilot_frame();
        game.frame();
        total_frames += 1;
        let state = game
            .app
            .world
            .try_resource::<AssaultState>()
            .expect("状态在位");
        if state.phase != Phase::Playing {
            continue;
        }
        // 帧条件在事件分流前读取——Shot 事件当帧粒子刚出生，直接定格。
        // 相机定格条件额外要求玩家在内场（|x| < 5、z < 2）——玩家贴南墙
        // 时相机（玩家 + 固定偏移）会在墙外被墙体遮挡
        let interior = state.player.pos.x.abs() < 6.5 && state.player.pos.z < 3.0;
        // 剔除断言的几何前提：相机朝北俯视（CAMERA_OFFSET 南偏 +6.2），
        // 玩家在北半场（z < 0）时南外墙必在视锥背后 → culled > 0 有保证。
        // R67 取消窗口让首次 stage ≥ 2 横扫提前，曾捕捉到南半场画面
        // （南墙进入视锥 → culled = 0 断言炸）——收窄捕捉条件而非放宽
        // 断言（R67 实测教训）
        let north = state.player.pos.z < 0.0;
        let swept = interior
            && north
            && state.player.sword.phase == SwordPhase::Sweep
            && state.player.sword.stage >= 2;
        let shot = interior
            && state
                .frame_events
                .iter()
                .any(|e| matches!(e, AssaultEvent::Shot { .. }));
        // 帧先渲染再落盘（世界状态此刻就是定格态——下帧会继续推进）
        if !frame_a_done && swept {
            renderer.render(&game.app.world);
            renderer.save_bmp("assault_a.bmp").expect("写帧 A");
            let stats = renderer.renderer().stats();
            println!("=== 帧 A（挥砍）FrameStats（640×360）===");
            println!(
                "draw_calls = {}，instances = {}，culled_instances = {}，triangles = {}",
                stats.draw_calls, stats.instances, stats.culled_instances, stats.triangles_drawn
            );
            assert!(
                stats.culled_instances > 0,
                "庭院外墙背面应产生视锥剔除（culled_instances > 0），实际 {}",
                stats.culled_instances
            );
            frame_a_done = true;
        } else if !frame_b_done && shot {
            renderer.render(&game.app.world);
            renderer.save_bmp("assault_b.bmp").expect("写帧 B");
            let stats = renderer.renderer().stats();
            println!("=== 帧 B（枪口焰）FrameStats（640×360）===");
            println!(
                "draw_calls = {}，instances = {}，culled_instances = {}，triangles = {}",
                stats.draw_calls, stats.instances, stats.culled_instances, stats.triangles_drawn
            );
            frame_b_done = true;
        }
        if frame_a_done && frame_b_done {
            break;
        }
    }
    assert!(frame_a_done, "120 秒内应捕捉到挥砍帧");
    assert!(frame_b_done, "120 秒内应捕捉到枪口焰帧");

    let state = game
        .app
        .world
        .try_resource::<AssaultState>()
        .expect("状态在位");
    println!(
        "定格于第 {total_frames} 帧：击杀 {}/4，玩家 HP {}，枪击 {} 发，DOF 对焦见 FrameStats 上方",
        state.kills, state.player.hp, state.player.rifle.shots,
    );
    println!("已输出 assault_a.bmp / assault_b.bmp");
    println!(
        "转 PNG：ffmpeg -y -i assault_a.bmp -i assault_b.bmp -filter_complex hstack docs/assault.png"
    );
}
