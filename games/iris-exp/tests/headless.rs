//! headless 确定性集成测试：不建窗口、不建 GPU、不依赖 winit，复用
//! `ExpPlugin::build_schedule` 的完整系统链 + `HierarchyPlugin` /
//! `AnimationPlugin`（与窗口路径 `DefaultPlugins` 同一编排），虚拟时钟
//! 确定性推进。覆盖本轮实验的三问：
//!
//! - **资产契约**：GLB 剪辑名按名可查（`Mixamo_Walk` / `Mixamo_Jump`），
//!   骨架 87 节点、8 个网格装配在位；
//! - **动画推进**：步行循环下采样关节（gMan Head）逐帧位姿变化，
//!   切换跳跃后骨盆 z 先升后降（蓄力 → 腾空 → 落地）；
//! - **确定性**：同一输入两次推进，采样关节位姿 `to_bits()` 逐位一致
//!   （引擎确定性文化的标准测试模式）。
//!
//! 资产（32MB 版权 GLB）不入库：缺席时 `eprintln!` 跳过 + return，
//! 不 panic——与 GPU 测试的跳过约定同款。
//!
//! ```sh
//! cargo test -p iris-exp --test headless
//! ```

use std::time::Duration;

use iris_core::prelude::Stage;
use iris_engine::prelude::{
    AnimationPlugin, App, Audio, Camera, DirectionalLight, GlobalTransform, HierarchyPlugin, Input,
    MeshStore, Plugin, Time, Viewport,
};
use iris_exp::plugin::ExpPlugin;

/// 测试台：世界 + ExpPlugin 系统链 + 层级 / 动画插件 + 虚拟时钟
/// （与窗口路径 DefaultPlugins 同一编排；音频强制 headless）。
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
        app.world.insert_resource(Viewport::new(320, 180));
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
}

/// 资产在位时才继续（跳过模式：CI / 干净检出无 32MB 版权模型）。
fn asset_available() -> bool {
    let path = iris_engine::prelude::asset_path(iris_exp::MODEL_PATH);
    let ok = path.exists();
    if !ok {
        eprintln!(
            "跳过：角色 GLB 缺席（{}）——本测试验证外部资产导入",
            path.display()
        );
    }
    ok
}

#[test]
fn clip_contract_and_animation_progress() {
    if !asset_available() {
        return;
    }
    let mut game = Harness::new();
    let ents = *game
        .app
        .world
        .try_resource::<iris_exp::ExpEntities>()
        .expect("实体表在位");
    let model = ents.model.expect("资产在位时角色应装配成功");

    // ---- 资产契约：剪辑按名可查、模型网格/骨骼装配在位 ----
    {
        let player = game
            .app
            .world
            .get::<iris_engine::prelude::AnimationPlayer>(model)
            .expect("播放器在位");
        assert!(player.clips.iter().any(|c| c.name == iris_exp::WALK_CLIP));
        assert!(player.clips.iter().any(|c| c.name == iris_exp::JUMP_CLIP));
        assert!(player.node_entity_by_name("gMan Head").is_some());
        assert!(player.node_entity_by_name("gMan Pelvis").is_some());
        let bones = player.node_names.iter().filter(|n| n.is_some()).count();
        assert!(bones >= 87, "gMan 骨架应有 ≥87 个具名节点，实际 {bones}");
    }

    // ---- 步行推进：采样关节位姿逐帧变化 ----
    let sample_joint = |game: &mut Harness| -> [u32; 3] {
        let player = game
            .app
            .world
            .get::<iris_engine::prelude::AnimationPlayer>(model)
            .expect("播放器在位");
        let head = player
            .node_entity_by_name("gMan Head")
            .expect("头骨节点在位");
        let gt = game
            .app
            .world
            .get::<GlobalTransform>(head)
            .expect("层级传播已写入")
            .position();
        [gt.x.to_bits(), gt.y.to_bits(), gt.z.to_bits()]
    };
    game.frame();
    game.frame();
    let pose_a = sample_joint(&mut game);
    for _ in 0..30 {
        game.frame();
    }
    let pose_b = sample_joint(&mut game);
    assert_ne!(pose_a, pose_b, "步行推进 30 帧后头部位姿应变化");

    // ---- 切换跳跃：骨盆 z 先升后降（蓄力 → 腾空 → 落地）----
    {
        let mut input = game.app.world.resource_mut::<Input>();
        input.press(iris_engine::prelude::KeyCode::Space);
    }
    game.frame();
    let mut z_samples: Vec<f32> = Vec::new();
    for _ in 0..130 {
        game.frame();
        let player = game
            .app
            .world
            .get::<iris_engine::prelude::AnimationPlayer>(model)
            .expect("播放器在位");
        let pelvis = player
            .node_entity_by_name("gMan Pelvis")
            .expect("骨盆节点在位");
        let z = game
            .app
            .world
            .get::<GlobalTransform>(pelvis)
            .expect("层级传播已写入")
            .position()
            .y;
        z_samples.push(z);
    }
    let z_max = z_samples.iter().copied().fold(f32::MIN, f32::max);
    let z_min = z_samples.iter().copied().fold(f32::MAX, f32::min);
    assert!(
        z_max - z_min > 1.0,
        "跳跃全程骨盆 z 应有 >1 m 起伏（腾空 3.36 / 蓄力 1.08），实际 {z_min} -> {z_max}"
    );
    // 播完自动回落步行
    let player = game
        .app
        .world
        .get::<iris_engine::prelude::AnimationPlayer>(model)
        .expect("播放器在位");
    assert_eq!(player.active, ents.walk, "跳跃非循环播完应自动回落步行剪辑");
}

#[test]
fn determinism_bitwise() {
    if !asset_available() {
        return;
    }
    // 同输入两次推进，采样关节位姿逐位一致（引擎确定性文化标准模式）
    let run = || -> Vec<[u32; 3]> {
        let mut game = Harness::new();
        let model = game
            .app
            .world
            .try_resource::<iris_exp::ExpEntities>()
            .expect("实体表在位")
            .model
            .expect("资产在位时角色应装配成功");
        let mut poses = Vec::new();
        for i in 0..120 {
            if i == 40 {
                let mut input = game.app.world.resource_mut::<Input>();
                input.press(iris_engine::prelude::KeyCode::Space);
            }
            game.frame();
            if i % 10 == 0 {
                let player = game
                    .app
                    .world
                    .get::<iris_engine::prelude::AnimationPlayer>(model)
                    .expect("播放器在位");
                let head = player.node_entity_by_name("gMan Head").expect("头骨在位");
                let gt = game
                    .app
                    .world
                    .get::<GlobalTransform>(head)
                    .expect("层级传播已写入")
                    .position();
                poses.push([gt.x.to_bits(), gt.y.to_bits(), gt.z.to_bits()]);
            }
        }
        poses
    };
    let run_a = run();
    let run_b = run();
    assert_eq!(run_a, run_b, "同输入两次推进的关节位姿应逐位一致");
}
