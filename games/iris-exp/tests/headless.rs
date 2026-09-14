//! headless 确定性集成测试：不建窗口、不建 GPU、不依赖 winit，复用
//! `ExpPlugin::build_schedule` 的完整系统链 + `HierarchyPlugin` /
//! `AnimationPlugin`（与窗口路径 `DefaultPlugins` 同一编排），虚拟时钟
//! 确定性推进。覆盖本轮实验的四问：
//!
//! - **资产契约**：GLB 剪辑名按名可查（`Mixamo_Walk` / `Mixamo_Jump`），
//!   骨架 87 节点、8 个网格装配在位；
//! - **动画推进**：步行动画下采样关节（gMan Head）逐帧位姿变化，
//!   切换跳跃后骨盆 z 先升后降（蓄力 → 腾空 → 落地）——移动键（W）
//!   电平注入以驱动「播放状态随移动」的新语义（frictions ⑥）；
//! - **控制**：轨道相机拖拽 / 缩放 / 跟随（不依赖资产）、WASD 移动 +
//!   朝向转身 + 停下定格（依赖资产）；
//! - **确定性**：同一输入两次推进，采样关节位姿 `to_bits()` 逐位一致
//!   （引擎确定性文化的标准测试模式）。
//!
//! 资产（32MB 版权 GLB）不入库：缺席时相关测试 `eprintln!` 跳过 +
//! return，不 panic——与 GPU 测试的跳过约定同款（相机控制测试不依赖
//! 资产，任何环境都跑）。
//!
//! ```sh
//! cargo test -p iris-exp --test headless
//! ```

use std::time::Duration;

use iris_core::prelude::Stage;
use iris_engine::prelude::{
    AnimationPlugin, App, Audio, Camera, DirectionalLight, GlobalTransform, HierarchyPlugin, Input,
    KeyCode, MeshStore, MouseButton, Plugin, Time, Viewport,
};
use iris_exp::ExpPlugin;
use iris_exp::plugin::{
    DIST_MAX, FOCUS_H_MAX, FOCUS_H_MIN, FOCUS_H_RATE, GROUND_HALF, MOVE_SPEED, ORBIT_SENS,
    PITCH_MAX, TARGET_HEIGHT, WHEEL_SENS, facing_yaw, orbit_pose,
};

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

    /// 电平注入：按住键（跨帧保持，直到显式 release）。
    fn hold(&mut self, key: KeyCode) {
        self.app.world.resource_mut::<Input>().press(key);
    }

    fn release(&mut self, key: KeyCode) {
        self.app.world.resource_mut::<Input>().release(key);
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

    // ---- 步行推进：按住 W（移动 → 步行播放），采样关节位姿逐帧变化 ----
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
    game.hold(KeyCode::KeyW);
    game.frame();
    game.frame();
    let pose_a = sample_joint(&mut game);
    for _ in 0..30 {
        game.frame();
    }
    let pose_b = sample_joint(&mut game);
    assert_ne!(pose_a, pose_b, "步行推进 30 帧后头部位姿应变化");

    // ---- 切换跳跃：骨盆 z 先升后降（蓄力 → 腾空 → 落地）----
    // W 保持按住（移动中跳跃——跳跃播放不受移动影响，水平移动照常，
    // 骨盆 y 与水平位移无关，断言不受影响）
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
    // 播完自动回落步行（移动中回落 → playing 应为 true）
    let player = game
        .app
        .world
        .get::<iris_engine::prelude::AnimationPlayer>(model)
        .expect("播放器在位");
    assert_eq!(player.active, ents.walk, "跳跃非循环播完应自动回落步行剪辑");
    assert!(player.playing, "移动中回落步行应继续播放");
}

#[test]
fn determinism_bitwise() {
    if !asset_available() {
        return;
    }
    // 同输入两次推进，采样关节位姿逐位一致（引擎确定性文化标准模式）。
    // W 全程按住（移动 → 步行播放；水平移动不影响头骨位姿采样的
    // 可判别性——姿态变化来自动画采样）。
    let run = || -> Vec<[u32; 3]> {
        let mut game = Harness::new();
        let model = game
            .app
            .world
            .try_resource::<iris_exp::ExpEntities>()
            .expect("实体表在位")
            .model
            .expect("资产在位时角色应装配成功");
        game.hold(KeyCode::KeyW);
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

/// 轨道相机控制（**不依赖资产**——无角色模式下相机照样可玩）：
/// Startup 位姿 = 轨道参数构造、无输入帧恒等、右键拖拽环绕（yaw/pitch
/// 响应 + clamp）、滚轮缩放（exp 乘法 + clamp）、松开右键拖拽不生效。
#[test]
fn orbit_camera_control() {
    use iris_engine::prelude::{ElementState, Transform, Vec3};

    let mut game = Harness::new();
    let ents = *game
        .app
        .world
        .try_resource::<iris_exp::ExpEntities>()
        .expect("实体表在位");

    // 期望注视点（无角色 → 原点；有角色 → 角色位置，测试台内角色静止在
    // 装配位——spawn_gltf_instance 根为恒等变换，动态读取不假设）
    let focus = |game: &Harness| -> Vec3 {
        (match ents.model {
            Some(m) => game
                .app
                .world
                .get::<Transform>(m)
                .map(|t| t.position)
                .unwrap_or(Vec3::ZERO),
            None => Vec3::ZERO,
        }) + Vec3::new(0.0, TARGET_HEIGHT, 0.0)
    };
    let cam_transform = |game: &Harness| -> [u32; 7] {
        let t = game
            .app
            .world
            .get::<Transform>(ents.camera)
            .expect("相机 Transform 在位");
        let r = t.rotation;
        [
            t.position.x.to_bits(),
            t.position.y.to_bits(),
            t.position.z.to_bits(),
            r.x.to_bits(),
            r.y.to_bits(),
            r.z.to_bits(),
            r.w.to_bits(),
        ]
    };

    // ---- Startup 位姿 = 默认轨道参数（build_scene 与轨道系统同口径）----
    let init = cam_transform(&game);
    let (eye, rot) = orbit_pose(0.0, 9.0_f32.to_radians(), 7.5, focus(&game));
    {
        let t = game
            .app
            .world
            .get::<Transform>(ents.camera)
            .expect("相机在位");
        assert!(
            t.position.abs_diff_eq(eye, 1e-5) && t.rotation.abs_diff_eq(rot, 1e-5),
            "Startup 相机位姿应由轨道参数构造：期望 {eye:?}/{rot:?}，实际 {:?}/{:?}",
            t.position,
            t.rotation
        );
    }

    // ---- 无输入帧恒等：不写相机 Transform（逐位）----
    game.frame();
    assert_eq!(cam_transform(&game), init, "无输入帧相机位姿应逐位不变");

    // ---- 右键拖拽：yaw 随 dx、pitch 随 −dy（俯角口径），dist 不变 ----
    {
        let mut input = game.app.world.resource_mut::<Input>();
        input.on_mouse_button(MouseButton::Right, ElementState::Pressed);
        input.on_mouse_motion((40.0, -30.0));
    }
    game.frame();
    let (yaw_a, pitch_a, dist_a) = {
        let o = game
            .app
            .world
            .try_resource::<iris_exp::OrbitCamera>()
            .expect("轨道资源在位");
        assert!(
            (o.yaw - 40.0 * ORBIT_SENS).abs() < 1e-6,
            "右拖 40 px → yaw 增 {}，实际 {}",
            40.0 * ORBIT_SENS,
            o.yaw
        );
        assert!(
            (o.pitch - (9.0_f32.to_radians() + 30.0 * ORBIT_SENS)).abs() < 1e-6,
            "上拖 30 px → 俯角增大，实际 {}",
            o.pitch
        );
        (o.yaw, o.pitch, o.dist)
    };
    {
        let (eye, rot) = orbit_pose(yaw_a, pitch_a, dist_a, focus(&game));
        let t = game
            .app
            .world
            .get::<Transform>(ents.camera)
            .expect("相机在位");
        assert!(
            t.position.abs_diff_eq(eye, 1e-5) && t.rotation.abs_diff_eq(rot, 1e-5),
            "拖拽后相机位姿应等于轨道位姿（纯函数 oracle）"
        );
    }

    // ---- 按住继续拖拽（held 跨帧，delta 需逐帧注入）+ 俯角 clamp ----
    {
        let mut input = game.app.world.resource_mut::<Input>();
        input.on_mouse_motion((0.0, -60_000.0));
    }
    game.frame();
    {
        let o = game
            .app
            .world
            .try_resource::<iris_exp::OrbitCamera>()
            .expect("轨道资源在位");
        assert_eq!(o.pitch, PITCH_MAX, "大角度上拖俯角应精确钳到上限");
        assert!((o.yaw - yaw_a).abs() < 1e-6, "dx=0 时 yaw 不应漂移");
    }

    // ---- 滚轮缩放：上滚拉近（exp 乘法）+ 距离不变量 + 下限 clamp ----
    {
        let mut input = game.app.world.resource_mut::<Input>();
        input.on_wheel(40.0);
    }
    game.frame();
    let dist_b = {
        let o = game
            .app
            .world
            .try_resource::<iris_exp::OrbitCamera>()
            .expect("轨道资源在位");
        let expect = 7.5 * (-40.0 * WHEEL_SENS).exp();
        assert!(
            (o.dist - expect).abs() < 1e-5,
            "上滚一格应缩至 7.5·exp(−40k)={expect}，实际 {}",
            o.dist
        );
        o.dist
    };
    {
        // 距离不变量：|eye − focus| = dist（任意轨道角度）
        let t = game
            .app
            .world
            .get::<Transform>(ents.camera)
            .expect("相机在位");
        let d = t.position.distance(focus(&game));
        assert!(
            (d - dist_b).abs() < 1e-4,
            "注视点距离应等于轨道距离 {dist_b}，实际 {d}"
        );
    }
    {
        let mut input = game.app.world.resource_mut::<Input>();
        input.on_wheel(-4000.0);
    }
    game.frame();
    {
        let o = game
            .app
            .world
            .try_resource::<iris_exp::OrbitCamera>()
            .expect("轨道资源在位");
        assert_eq!(o.dist, DIST_MAX, "大幅下滚距离应精确钳到上限");
    }

    // ---- 松开右键：拖拽位移不再生效（滚轮不受按键影响）----
    {
        let mut input = game.app.world.resource_mut::<Input>();
        input.on_mouse_button(MouseButton::Right, ElementState::Released);
        input.on_mouse_motion((123.0, -45.0));
    }
    game.frame();
    {
        let o = game
            .app
            .world
            .try_resource::<iris_exp::OrbitCamera>()
            .expect("轨道资源在位");
        assert!((o.yaw - yaw_a).abs() < 1e-6, "松开右键后拖拽不应改变 yaw");
        assert_eq!(o.pitch, PITCH_MAX, "松开右键后拖拽不应改变 pitch");
    }
}

/// Q/E 注视点高度调节：升降速率、双端 clamp、相机位姿跟随（纯函数
/// oracle）与无输入恒等（focus_h 改动后恢复逐位）。
#[test]
fn orbit_camera_focus_height() {
    use iris_engine::prelude::{Transform, Vec3};

    let mut game = Harness::new();
    let ents = *game
        .app
        .world
        .try_resource::<iris_exp::ExpEntities>()
        .expect("实体表在位");

    let focus = |game: &Harness, h: f32| -> Vec3 {
        (match ents.model {
            Some(m) => game
                .app
                .world
                .get::<Transform>(m)
                .map(|t| t.position)
                .unwrap_or(Vec3::ZERO),
            None => Vec3::ZERO,
        }) + Vec3::new(0.0, h, 0.0)
    };
    let orbit = |game: &Harness| -> iris_exp::OrbitCamera {
        *game
            .app
            .world
            .try_resource::<iris_exp::OrbitCamera>()
            .expect("轨道资源在位")
    };

    // ---- E 按住 30 帧：focus_h = 1.5 + 2.5 × 0.5 = 2.75 m（升向头顶）----
    game.hold(KeyCode::KeyE);
    for _ in 0..30 {
        game.frame();
    }
    game.release(KeyCode::KeyE);
    {
        let o = orbit(&game);
        let expect = TARGET_HEIGHT + FOCUS_H_RATE * 0.5;
        assert!(
            (o.focus_h - expect).abs() < 1e-5,
            "E 30 帧注视点应升至 {expect} m，实际 {}",
            o.focus_h
        );
        // 相机位姿 = 轨道参数 × 新注视点（纯函数 oracle，逐位口径同系统）
        let (eye, rot) = orbit_pose(o.yaw, o.pitch, o.dist, focus(&game, o.focus_h));
        let t = game
            .app
            .world
            .get::<Transform>(ents.camera)
            .expect("相机在位");
        assert!(
            t.position.abs_diff_eq(eye, 1e-5) && t.rotation.abs_diff_eq(rot, 1e-5),
            "升注视点后相机位姿应跟随（注视点 = 位置 + focus_h）"
        );
    }

    // ---- Q 按住长推：精确钳到下限（贴地）----
    game.hold(KeyCode::KeyQ);
    for _ in 0..600 {
        game.frame();
    }
    game.release(KeyCode::KeyQ);
    {
        let o = orbit(&game);
        assert_eq!(o.focus_h, FOCUS_H_MIN, "长按 Q 应精确钳到下限（贴地）");
    }

    // ---- E 长推：精确钳到上限（头顶之上）----
    game.hold(KeyCode::KeyE);
    for _ in 0..600 {
        game.frame();
    }
    game.release(KeyCode::KeyE);
    {
        let o = orbit(&game);
        assert_eq!(o.focus_h, FOCUS_H_MAX, "长按 E 应精确钳到上限");
    }

    // ---- 无输入帧恒等：focus_h 与相机位姿逐位不变 ----
    let o_before = orbit(&game);
    let t_before = *game
        .app
        .world
        .get::<Transform>(ents.camera)
        .expect("相机在位");
    game.frame();
    {
        let o = orbit(&game);
        let t = game
            .app
            .world
            .get::<Transform>(ents.camera)
            .expect("相机在位");
        assert!(
            o.focus_h.to_bits() == o_before.focus_h.to_bits()
                && t.position == t_before.position
                && t.rotation == t_before.rotation,
            "无输入帧 focus_h 与相机位姿应逐位不变"
        );
    }
}

/// WASD 角色移动 + 朝向转身 + 停下定格 + 相机注视跟随（**依赖资产**）。
#[test]
fn character_move_camera_follow() {
    use iris_engine::prelude::{Transform, Vec3};

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

    let model_pos = |game: &Harness| -> Vec3 {
        game.app
            .world
            .get::<Transform>(model)
            .expect("角色 Transform 在位")
            .position
    };

    // ---- W 按住 30 帧：朝屏幕里侧（-Z）移动 3.0 × 0.5 s = 1.5 m ----
    game.hold(KeyCode::KeyW);
    for _ in 0..30 {
        game.frame();
    }
    {
        let pos = model_pos(&game);
        assert!(
            (pos.z + MOVE_SPEED * 0.5).abs() < 1e-4,
            "W 30 帧应向 -Z 移动 {} m，实际 {pos:?}",
            MOVE_SPEED * 0.5
        );
        assert!(pos.x.abs() < 1e-6, "W 沿视线方向不应有侧移");
        // 朝向：W = 屏幕里侧（-Z），角色由原生 +Z 前向转身背对相机——
        // 30 帧趋近残差 e^{−6}·π ≈ 0.008，|yaw| 应贴近 π
        let yaw = game
            .app
            .world
            .try_resource::<iris_exp::ExpCharacter>()
            .expect("角色状态在位")
            .yaw;
        assert!(
            (yaw.abs() - core::f32::consts::PI).abs() < 0.05,
            "面向 -Z 移动应转身背对相机（|yaw|→π），实际 {yaw}"
        );
        // 步行在播
        let player = game
            .app
            .world
            .get::<iris_engine::prelude::AnimationPlayer>(model)
            .expect("播放器在位");
        assert!(
            player.active == ents.walk && player.playing,
            "移动中步行剪辑应在播"
        );
        // 相机注视跟随：位姿 = 轨道参数 × 角色当前位置（纯函数 oracle）
        let o = *game
            .app
            .world
            .try_resource::<iris_exp::OrbitCamera>()
            .expect("轨道资源在位");
        let (eye, rot) = orbit_pose(
            o.yaw,
            o.pitch,
            o.dist,
            pos + Vec3::new(0.0, TARGET_HEIGHT, 0.0),
        );
        let t = game
            .app
            .world
            .get::<Transform>(ents.camera)
            .expect("相机在位");
        assert!(
            t.position.abs_diff_eq(eye, 1e-5) && t.rotation.abs_diff_eq(rot, 1e-5),
            "移动后相机应跟随角色（注视点 = 位置 + {TARGET_HEIGHT} m）"
        );
    }

    // ---- 切 S：朝 +Z 折返，60 帧后朝向趋近 π（转身残差 exp(−12)≈6e-6）----
    game.release(KeyCode::KeyW);
    game.hold(KeyCode::KeyS);
    for _ in 0..60 {
        game.frame();
    }
    {
        let pos = model_pos(&game);
        assert!(
            (pos.z - MOVE_SPEED * 0.5).abs() < 1e-4,
            "S 60 帧应从 -1.5 折返至 +1.5，实际 {}",
            pos.z
        );
        let yaw = game
            .app
            .world
            .try_resource::<iris_exp::ExpCharacter>()
            .expect("角色状态在位")
            .yaw;
        assert!(
            yaw.abs() < 1e-3,
            "S 方向 = 模型原生前向（+Z），朝向应回归 0，实际 {yaw}"
        );
    }

    // ---- 停下定格：无移动输入 → 播放停 + 位置/朝向/相机逐位恒等 ----
    game.release(KeyCode::KeyS);
    game.frame();
    {
        let player = game
            .app
            .world
            .get::<iris_engine::prelude::AnimationPlayer>(model)
            .expect("播放器在位");
        assert!(
            player.active == ents.walk && !player.playing,
            "停下后步行剪辑应定格（playing = false）"
        );
    }
    let t_stop = |game: &Harness| -> [u32; 7] {
        let t = game
            .app
            .world
            .get::<Transform>(model)
            .expect("角色 Transform 在位");
        let r = t.rotation;
        [
            t.position.x.to_bits(),
            t.position.y.to_bits(),
            t.position.z.to_bits(),
            r.x.to_bits(),
            r.y.to_bits(),
            r.z.to_bits(),
            r.w.to_bits(),
        ]
    };
    let cam_stop = |game: &Harness| -> [u32; 7] {
        let t = game
            .app
            .world
            .get::<Transform>(ents.camera)
            .expect("相机在位");
        let r = t.rotation;
        [
            t.position.x.to_bits(),
            t.position.y.to_bits(),
            t.position.z.to_bits(),
            r.x.to_bits(),
            r.y.to_bits(),
            r.z.to_bits(),
            r.w.to_bits(),
        ]
    };
    let model_bits = t_stop(&game);
    let cam_bits = cam_stop(&game);
    for _ in 0..5 {
        game.frame();
    }
    assert_eq!(t_stop(&game), model_bits, "停下后角色 Transform 应逐位恒等");
    assert_eq!(cam_stop(&game), cam_bits, "停下后相机 Transform 应逐位恒等");

    // ---- 活动范围 clamp：S 长按折返冲出地面 → 精确钳在半宽 ----
    game.hold(KeyCode::KeyS);
    for _ in 0..400 {
        game.frame();
    }
    {
        let pos = model_pos(&game);
        assert_eq!(pos.z, GROUND_HALF, "长按 S 应精确钳在地面半宽");
    }
    // 纯函数对照：S 方向 = 模型原生前向，解析朝向为 0
    assert!(facing_yaw(Vec3::Z).abs() < 1e-5, "facing_yaw(+Z) 应为 0");
}
