//! 工具：Iris Ascent（iris-ascent）中段战局视觉验收（离屏渲染）。
//!
//! 用与 headless 测试**完全相同的方式**驱动真实游戏世界：AscentPlugin
//! 系统链，`HierarchyPlugin` / `ParticlePlugin` / `AnimationPlugin` 注册，
//! `Time::advance` 确定性推进，不建窗口。脚本化自动玩家以真实输入打通
//! 第一段登顶（台地 → 大球坡面 → 定点跳 → 跳板弹射）后定格中段战局：
//! - 角色走在第一段弹射落点台 P6 上（检查点 1 旗已在前方点亮青光）；
//! - 沿途宝石已收 7 颗（HUD GEMS 7/14），剩余宝石在途可见；
//! - 落地尘土在脚下扩散（FX 池 burst）；
//! - 远景 LOD 装饰（山景球 / 巨柱 / 浮岛剪影）+ 斜照方向光的 CSM 长影；
//! - HUD（GEMS / CHECKPOINT / FALLS）完整。
//!
//! 输出 `ascent_mid.bmp`（640×360 一帧）。转 PNG：
//! ```sh
//! ffmpeg -y -i ascent_mid.bmp docs/ascent.png
//! ```
//!
//! 同时打印该帧 FrameStats 并断言 `culled_instances > 0`；另渲染一帧
//! 「相机拉远 160 m」的对照帧，`triangles_drawn` 显著下降即 **LOD（R30）
//! 的量化证据**（近 / 远两组数字打印在末尾）。
//!
//! ```sh
//! cargo run -p iris-ascent --example ascent_snapshot --release
//! ```

use std::time::Duration;

use glam::Vec3;

use iris_ascent::{AscentPlugin, AscentState, AutoPilot, FxPool, GEMS, Phase};
use iris_core::prelude::Stage;
use iris_engine::prelude::{
    AnimationPlugin, App, Audio, Camera, DirectionalLight, HierarchyPlugin, Input, KeyCode,
    MeshStore, OffscreenRenderer, ParticlePlugin, Plugin, Time, Transform,
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
        // 斜照方向光：影子横向铺开，第三人称镜头里角色 / 旗杆 / 高台长影均可见
        app.world.insert_resource(DirectionalLight {
            direction: glam::Vec3::new(-0.85, -1.0, -0.2),
            color: glam::Vec3::new(0.95, 0.93, 0.9),
            intensity: 0.9,
            ambient: glam::Vec3::new(0.12, 0.12, 0.14),
            ..DirectionalLight::default()
        });
        app.world.insert_resource(Audio::headless());
        // 程序化天空 IBL 调暗（缺省 1.0 的环境光会把中暗色材质洗白，
        // 方向光 0.9 + 弱环境 = 柔和日光基准）
        app.world
            .insert_resource(iris_engine::prelude::EnvironmentSettings { intensity: 0.35 });
        HierarchyPlugin.build(&mut app);
        ParticlePlugin.build(&mut app);
        AnimationPlugin.build(&mut app);
        AscentPlugin::default().build_schedule(&mut app.schedule);
        let mut harness = Self { app };
        harness
            .app
            .schedule
            .run(Stage::Startup, &mut harness.app.world);
        iris_core::hierarchy::propagate_transforms(&mut harness.app.world);
        harness
    }

    fn frame(&mut self) {
        self.app
            .world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(1.0 / 60.0));
        self.app.schedule.run(Stage::Update, &mut self.app.world);
        self.app
            .schedule
            .run(Stage::PostUpdate, &mut self.app.world);
        self.app.world.clear_all_events();
        self.app.world.resource_mut::<Input>().end_frame();
    }

    fn frames(&mut self, count: usize) {
        for _ in 0..count {
            self.frame();
        }
    }

    /// 自动玩家推进一帧（键盘注入全链路，与 headless 测试同一脚本）。
    fn autopilot_frame(&mut self, prev_jump: &mut bool) {
        let jump = {
            let state = self
                .app
                .world
                .try_resource::<AscentState>()
                .expect("状态在位");
            AutoPilot.drive(&state).jump
        };
        let mut input = self.app.world.resource_mut::<Input>();
        if jump || *prev_jump {
            input.release(KeyCode::Space);
            input.press(KeyCode::Space);
        }
        input.press(KeyCode::KeyW);
        drop(input);
        *prev_jump = jump;
        self.frame();
    }
}

fn main() {
    let Some(mut renderer) = OffscreenRenderer::new(640, 360) else {
        eprintln!("无 GPU，无法离屏渲染");
        std::process::exit(1);
    };

    let mut game = Harness::new();

    // 自动玩家真实推进：直到踩上第一段弹射落点台 P6（检查点 1 已点亮、
    // 落地尘土新鲜、沿途宝石已收），且已越过 P6 上的宝石朝跳板走去
    let mut prev_jump = false;
    let mut framed = false;
    for _ in 0..60 * 60 {
        game.autopilot_frame(&mut prev_jump);
        let state = game
            .app
            .world
            .try_resource::<AscentState>()
            .expect("状态在位");
        if state.checkpoints[0]
            && state.gems_collected() >= 7
            && state.grounded
            && state.pos.z < -52.8
            && state.pos.z > -54.2
        {
            framed = true;
            break;
        }
    }
    assert!(framed, "60 秒内应推进到 P6 中段的定格窗口");
    {
        let state = game
            .app
            .world
            .try_resource::<AscentState>()
            .expect("状态在位");
        assert_eq!(state.phase, Phase::Playing);
        assert_eq!(state.falls, 0, "自动玩家到此不应有坠落");
        assert_eq!(state.gems_collected(), 7, "沿途宝石应已收 7 颗");
        assert!(state.checkpoints[0], "检查点 1 应已点亮");
        println!(
            "定格：玩家在 (0, {:.2}, {:.2})，宝石 {}/14，检查点 {}/3",
            state.pos.y,
            state.pos.z,
            state.gems_collected(),
            state.checkpoints_activated()
        );
    }

    // 渲染 + 落盘（中段战局主帧）
    renderer.render(&game.app.world);
    renderer.save_bmp("ascent_mid.bmp").expect("写 BMP");
    let near = renderer.renderer().stats();
    println!("=== 定格帧 FrameStats（640×360）===");
    println!(
        "draw_calls = {}，instances = {}，culled_instances = {}，triangles = {}，particles_drawn = {}",
        near.draw_calls,
        near.instances,
        near.culled_instances,
        near.triangles_drawn,
        near.particles_drawn
    );
    assert!(
        near.culled_instances > 0,
        "远景装饰应产生视锥剔除（culled_instances > 0），实际 {}",
        near.culled_instances
    );

    // LOD 对照：换一个**全新世界**（玩家仍在出生台地——对照帧的视野里
    // 不含 130 m 长关卡的全部几何，唯一变量是装饰到相机的距离）：第三
    // 人称近景一帧 vs 相机抬到 ~160 m 外一帧（全部装饰超出 60 m 切换
    // 距离，低模生效）→ triangles_drawn 应显著下降
    {
        let mut fresh = Harness::new();
        fresh.frames(2);
        renderer.render(&fresh.app.world);
        let lod_near = renderer.renderer().stats();
        let ents = fresh
            .app
            .world
            .try_resource::<iris_ascent::AscentEntities>()
            .expect("实体索引在位")
            .clone();
        if let Some(mut transform) = fresh.app.world.get_mut::<Transform>(ents.camera) {
            transform.position = Vec3::new(0.0, 90.0, 130.0);
        }
        iris_core::hierarchy::propagate_transforms(&mut fresh.app.world);
        renderer.render(&fresh.app.world);
        let lod_far = renderer.renderer().stats();
        println!("=== LOD 对照（出生台地机位 vs 相机 +160 m）===");
        println!(
            "近景 triangles = {}（draw calls {}），远景 triangles = {}（draw calls {}）",
            lod_near.triangles_drawn,
            lod_near.draw_calls,
            lod_far.triangles_drawn,
            lod_far.draw_calls
        );
        assert!(
            lod_far.triangles_drawn < lod_near.triangles_drawn,
            "拉远后三角形数应下降（LOD 低模生效）：近 {} vs 远 {}",
            lod_near.triangles_drawn,
            lod_far.triangles_drawn
        );
        assert!(
            lod_near.culled_instances > 0,
            "近景应有视锥剔除，实际 {}",
            lod_near.culled_instances
        );
    }

    // FX 池活性对账（落地尘土应仍在画面生命期内）
    let fx_slots = game
        .app
        .world
        .try_resource::<FxPool>()
        .expect("FX 池在位")
        .slots
        .clone();
    let live: usize = fx_slots
        .iter()
        .map(|&slot| {
            game.app
                .world
                .get::<iris_engine::prelude::ParticleEmitter>(slot)
                .expect("FX 槽位应有发射器")
                .live_count()
        })
        .sum();
    println!("FX 池活粒子：{live}");
    let _ = GEMS.len();
    println!("已输出 ascent_mid.bmp");
    println!("转 PNG：ffmpeg -y -i ascent_mid.bmp docs/ascent.png");
}
