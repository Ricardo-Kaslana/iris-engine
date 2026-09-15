//! 工具：iris-exp 视觉验收（离屏渲染）。用与 headless 测试**完全相同的
//! 方式**驱动真实游戏世界：ExpPlugin 系统链 + `HierarchyPlugin` /
//! `AnimationPlugin`，`Time::advance` 确定性推进，不建窗口。定格**两帧**
//! （640×360，横向拼接）：
//!
//! - **帧 A「步行」**：按住 `S` 朝相机方向行走 30 帧（迈步姿态 + 正脸，
//!   相机注视点跟随角色）——移动即自动切步行动画（常态 Idle ↔ 步行，
//!   见 `ExpPlugin` 状态机）；
//! - **帧 B「跳跃」**：松开 `S` 后注入空格，推进到腾空段（骨盆最高点
//!   附近，原地正脸跳）。
//!
//! 输出 `exp_walk.bmp` / `exp_jump.bmp`。转 PNG（两帧横向拼接）：
//! ```sh
//! ffmpeg -y -i exp_walk.bmp -i exp_jump.bmp -filter_complex hstack docs/exp.png
//! ```
//!
//! 同帧打印 FrameStats 并断言有实例被渲染（角色确实入画）。
//! `EXP_CAM_HEAD=1` 走头部特写诊断机位——该路径无移动注入，角色停在
//! 常态 Idle（推进 30 帧 ≈ 呼吸循环开头，面部渲染验收不受影响）。
//!
//! ```sh
//! cargo run -p iris-exp --example exp_snapshot --release
//! ```

use std::time::Duration;

use iris_core::prelude::Stage;
use iris_engine::prelude::{
    AnimationPlugin, App, Audio, Camera, DirectionalLight, HierarchyPlugin, Input, KeyCode,
    MeshStore, OffscreenRenderer, Plugin, Time, Viewport,
};
use iris_exp::plugin::ExpPlugin;

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
        app.world.insert_resource(Viewport::new(640, 360));
        HierarchyPlugin.build(&mut app);
        AnimationPlugin.build(&mut app);
        ExpPlugin.build_schedule(&mut app.schedule);
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

    fn press_space(&mut self) {
        let mut input = self.app.world.resource_mut::<Input>();
        input.press(KeyCode::Space);
    }

    fn hold_s(&mut self) {
        self.app.world.resource_mut::<Input>().press(KeyCode::KeyS);
    }

    fn release_s(&mut self) {
        self.app
            .world
            .resource_mut::<Input>()
            .release(KeyCode::KeyS);
    }
}

fn main() {
    if !iris_engine::prelude::asset_path(iris_exp::MODEL_PATH).exists() {
        eprintln!("跳过：角色 GLB 缺席（assets/exp_weiqing.glb）——把导出的 GLB 放到该路径后重跑");
        std::process::exit(1);
    }
    let Some(mut renderer) = OffscreenRenderer::new(640, 360) else {
        eprintln!("无 GPU，无法离屏渲染");
        std::process::exit(1);
    };

    let mut game = Harness::new();

    // 诊断机位（EXP_CAM_HEAD=1）：头部特写——角色面向 +Z，头部约在
    // y≈3.0；相机放 +Z 侧近距直视面部。正常验收不设该变量。
    if std::env::var("EXP_CAM_HEAD").is_ok() {
        use iris_engine::prelude::{ActiveCamera, Transform};
        let mut hit = None;
        game.app
            .world
            .query::<(iris_engine::prelude::Entity, &ActiveCamera)>(|(e, _)| {
                hit = Some(e);
            });
        if let Some(cam) = hit {
            game.app.world.insert(
                cam,
                Transform {
                    position: glam::Vec3::new(0.0, 2.72, 1.5),
                    rotation: glam::Quat::IDENTITY,
                    scale: glam::Vec3::ONE,
                },
            );
        }
    }

    // ---- 帧 A：按住 S 朝相机行走 30 帧（迈步姿态 + 正脸）----
    // 诊断特写模式（EXP_CAM_HEAD）不注入移动：角色停在常态 Idle 循环
    if std::env::var("EXP_CAM_HEAD").is_err() {
        game.hold_s();
    }
    for _ in 0..30 {
        game.frame();
    }
    // 定格 = reset_taa + 三连渲：两张定格之间只推进模拟不渲染，跨帧时域
    // 状态（TAA 历史 / GTAO 上帧深度副本）全是上一张的——reset_taa 清
    // TAA/SSR 历史但**不清 AO 深度副本**（引擎缺口，frictions 记录），
    // 故再连渲两帧让 AO（上帧深度 = 本姿势首帧）与 TAA 历史都收敛到
    // 本姿势，定格图无跨张残影。确定性：固定帧序，跨运行逐位一致。
    renderer.renderer_mut().reset_taa();
    renderer.render(&game.app.world);
    renderer.render(&game.app.world);
    renderer.render(&game.app.world);
    renderer.save_bmp("exp_walk.bmp").expect("写步行帧");
    let stats = renderer.renderer().stats();
    println!("=== 帧 A（步行）FrameStats（640×360）===");
    println!(
        "draw_calls = {}，instances = {}，triangles = {}",
        stats.draw_calls, stats.instances, stats.triangles_drawn
    );
    assert!(stats.instances > 0, "步行帧应有实例入画");

    // ---- 帧 B：松开 S（原地）+ 空格起跳，推进到腾空段（骨盆 z 最高点附近）----
    game.release_s();
    game.press_space();
    let mut best_z = f32::MIN;
    let mut since_best = 0u32;
    for i in 0..114 {
        game.frame();
        let z = game
            .app
            .world
            .try_resource::<iris_exp::ExpEntities>()
            .and_then(|ents| ents.model)
            .and_then(|model| {
                let player = game
                    .app
                    .world
                    .get::<iris_engine::prelude::AnimationPlayer>(model)?;
                let pelvis = player.node_entity_by_name("gMan Pelvis")?;
                game.app
                    .world
                    .get::<iris_engine::prelude::GlobalTransform>(pelvis)
                    .map(|gt| gt.position().y)
            })
            .unwrap_or(f32::MIN);
        if z > best_z {
            best_z = z;
            since_best = 0;
        } else {
            since_best += 1;
        }
        // 骨盆 z 连续 6 帧回落 → 已过最高点，定格
        if i > 10 && since_best == 6 {
            break;
        }
    }
    renderer.renderer_mut().reset_taa();
    renderer.render(&game.app.world);
    renderer.render(&game.app.world);
    renderer.render(&game.app.world);
    renderer.save_bmp("exp_jump.bmp").expect("写跳跃帧");
    let stats = renderer.renderer().stats();
    println!("=== 帧 B（跳跃腾空）FrameStats（640×360）===");
    println!(
        "draw_calls = {}，instances = {}，triangles = {}",
        stats.draw_calls, stats.instances, stats.triangles_drawn
    );
    println!("腾空骨盆高度: {best_z:.3} m");
    assert!(
        best_z > 2.8,
        "跳跃腾空骨盆应显著升高（>2.8 m），实际 {best_z}"
    );
    assert!(stats.instances > 0, "跳跃帧应有实例入画");

    println!("已输出 exp_walk.bmp / exp_jump.bmp");
    println!(
        "转 PNG：ffmpeg -y -i exp_walk.bmp -i exp_jump.bmp -filter_complex hstack docs/exp.png"
    );
}
