//! 工具：iris-exp 视觉验收（离屏渲染）。用与 headless 测试**完全相同的
//! 方式**驱动真实游戏世界：ExpPlugin 系统链 + `HierarchyPlugin` /
//! `AnimationPlugin`，`Time::advance` 确定性推进，不建窗口。定格**四帧**
//! （640×360，横向拼接）：
//!
//! - **帧 0「待机」**：装配后第 4 帧的常态 Idle（呼吸循环已起步）；
//! - **帧 1「步行」**：按住 `S` 朝相机行走 30 帧（满幅迈步 + 正脸，
//!   相机注视点跟随角色）；
//! - **帧 2「过渡中段」**：松开 `S` 后——停步相位门先走完当前步（至多
//!   约半步），过窗切 Idle 并以 0.10 s 淡出，帧 2 取淡出进行到约一半
//!   （混合权重约 0.5），源姿势冻结在过步姿势上，姿势应读作「迈步与
//!   站立之间」。采停步方向而非起走方向做验收，是因为 Walk 剪辑开头
//!   0.1 s 的姿势本身接近站立，起走中段与纯待机肉眼难分辨；
//! - **帧 3「跳跃」**：注入空格，推进到腾空段（骨盆最高点附近，
//!   原地正脸跳）。
//!
//! 输出 `exp_idle.bmp` / `exp_walk.bmp` / `exp_blend.bmp` /
//! `exp_jump.bmp`。转 PNG（四帧横向拼接）：
//! ```sh
//! ffmpeg -y -i exp_idle.bmp -i exp_walk.bmp -i exp_blend.bmp -i exp_jump.bmp -filter_complex hstack=inputs=4 docs/exp.png
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

    // ---- 帧 0：常态 Idle（推进 4 帧，呼吸循环起步）----
    for _ in 0..4 {
        game.frame();
    }
    renderer.renderer_mut().reset_taa();
    renderer.render(&game.app.world);
    renderer.save_bmp("exp_idle.bmp").expect("写待机帧");
    assert!(
        renderer.renderer().stats().instances > 0,
        "待机帧应有实例入画"
    );

    // ---- 帧 1：按住 S 朝相机行走 30 帧（满幅迈步）----
    // 诊断特写模式（EXP_CAM_HEAD）不注入移动：角色停在常态 Idle 循环，
    // 过渡帧无意义（无切换发生）。
    if std::env::var("EXP_CAM_HEAD").is_err() {
        game.hold_s();
    }
    for _ in 0..30 {
        game.frame();
    }
    // 定格 = reset_taa + 单帧渲染（R89 起 reset_taa 清一切跨帧时域状态：
    // TAA 历史混合系数 0 ⇒ 输出 = 当前帧，GTAO 上帧深度记账失效 ⇒ 可见度
    // 图中性 1.0——旧姿势的残影从根上消灭，不再需要三连渲绕行）。
    // 确定性：固定帧序，跨运行逐位一致。
    renderer.renderer_mut().reset_taa();
    renderer.render(&game.app.world);
    renderer.save_bmp("exp_walk.bmp").expect("写步行帧");
    let stats = renderer.renderer().stats();
    println!("=== 帧 1（步行）FrameStats（640×360）===");
    println!(
        "draw_calls = {}，instances = {}，triangles = {}",
        stats.draw_calls, stats.instances, stats.triangles_drawn
    );
    assert!(stats.instances > 0, "步行帧应有实例入画");

    // ---- 帧 2：松开 S——停步相位门放行后的淡出中段 ----
    // 释放后角色先走完当前步（相位门，至多约半步），过窗切 Idle 并以
    // 0.10 s 淡出；帧 2 取淡出进行到约一半（混合权重约 0.5）。
    if std::env::var("EXP_CAM_HEAD").is_err() {
        game.release_s();
        let state_of = |game: &Harness| -> usize {
            game.app
                .world
                .try_resource::<iris_exp::ExpEntities>()
                .and_then(|ents| ents.model)
                .and_then(|model| {
                    game.app
                        .world
                        .get::<iris_engine::prelude::AnimationPlayer>(model)
                        .map(|p| p.current_state)
                })
                .unwrap_or(usize::MAX)
        };
        let mut switched = false;
        for _ in 0..40 {
            game.frame();
            if state_of(&game) != iris_exp::plugin::STATE_WALK {
                switched = true;
                break;
            }
        }
        assert!(switched, "停步应在 40 帧内过相位门切回 Idle");
        for _ in 0..3 {
            game.frame();
        }
        renderer.renderer_mut().reset_taa();
        renderer.render(&game.app.world);
        renderer.save_bmp("exp_blend.bmp").expect("写过渡帧");
        assert!(
            renderer.renderer().stats().instances > 0,
            "过渡帧应有实例入画"
        );
    }

    // ---- 帧 3：原地空格起跳，推进到腾空段（骨盆 z 最高点附近）----
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
    renderer.save_bmp("exp_jump.bmp").expect("写跳跃帧");
    let stats = renderer.renderer().stats();
    println!("=== 帧 3（跳跃腾空）FrameStats（640×360）===");
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

    println!("已输出 exp_idle.bmp / exp_walk.bmp / exp_blend.bmp / exp_jump.bmp");
    println!(
        "转 PNG：ffmpeg -y -i exp_idle.bmp -i exp_walk.bmp -i exp_blend.bmp -i exp_jump.bmp -filter_complex hstack=inputs=4 docs/exp.png"
    );
}
