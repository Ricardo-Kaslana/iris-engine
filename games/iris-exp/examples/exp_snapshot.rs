//! 工具：iris-exp 视觉验收（离屏渲染）。用与 headless 测试**完全相同的
//! 方式**驱动真实游戏世界：ExpPlugin 系统链 + `HierarchyPlugin` /
//! `AnimationPlugin`，`Time::advance` 确定性推进，不建窗口。定格**两帧**
//! （640×360，横向拼接）：
//!
//! - **帧 A「步行」**：步行循环中段（第 30 帧，迈步姿态）；
//! - **帧 B「跳跃」**：注入空格后推进到腾空段（骨盆最高点附近）。
//!
//! 输出 `exp_walk.bmp` / `exp_jump.bmp`。转 PNG（两帧横向拼接）：
//! ```sh
//! ffmpeg -y -i exp_walk.bmp -i exp_jump.bmp -filter_complex hstack docs/exp.png
//! ```
//!
//! 同帧打印 FrameStats 并断言有实例被渲染（角色确实入画）。
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

    // ---- 帧 A：步行循环中段（迈步姿态）----
    for _ in 0..30 {
        game.frame();
    }
    renderer.render(&game.app.world);
    renderer.save_bmp("exp_walk.bmp").expect("写步行帧");
    let stats = renderer.renderer().stats();
    println!("=== 帧 A（步行）FrameStats（640×360）===");
    println!(
        "draw_calls = {}，instances = {}，triangles = {}",
        stats.draw_calls, stats.instances, stats.triangles_drawn
    );
    assert!(stats.instances > 0, "步行帧应有实例入画");

    // ---- 帧 B：空格起跳，推进到腾空段（骨盆 z 最高点附近）----
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
