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
//! - **crossfade 状态机**：切状态当帧挂过渡（源快照与时长精确断言）、
//!   过渡到期清除、过渡中段姿势与硬切对照可判别地不同、跳跃播完由
//!   引擎条件转移自动分流（静止回 Idle、移动中直落 Walk）；
//! - **停步运动感**：相位门窗外释放保持步行、过窗后按 STOP_BLEND 淡出
//!   （窗口内容契约锁防 GLB 换版失配）；起步加速与停步减速的位移等于
//!   速度平滑纯函数的镜像积分，减速归零后无输入帧恢复逐位恒等；
//! - **控制**：轨道相机拖拽 / 缩放 / 跟随（不依赖资产）、WASD 移动 +
//!   朝向转身 + 速度平滑（依赖资产）；
//! - **插件装配**：`ExpPlugin` 不重复注册动画推进（合成播放器回归锚，
//!   不依赖资产——历史上曾因双重注册导致窗口路径动画双倍速）；
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

use iris_core::prelude::{Entity, Stage};
use iris_engine::prelude::{
    AnimationPlayer, AnimationPlugin, App, Audio, Camera, DirectionalLight, GlobalTransform,
    HierarchyPlugin, Input, KeyCode, MeshStore, MouseButton, Plugin, Time, Viewport,
};
use iris_exp::ExpPlugin;
use iris_exp::plugin::{
    ACCEL_RATE, DIST_MAX, FOCUS_H_MAX, FOCUS_H_MIN, FOCUS_H_RATE, GROUND_HALF, JUMP_IN_BLEND,
    JUMP_OUT_BLEND, MOVE_SPEED, ORBIT_SENS, PITCH_MAX, STATE_BLEND, STATE_IDLE, STATE_JUMP,
    STATE_WALK, STOP_BLEND, TARGET_HEIGHT, WHEEL_SENS, exp_approach_vec3, facing_yaw, orbit_pose,
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
        assert!(player.clips.iter().any(|c| c.name == iris_exp::IDLE_CLIP));
        assert!(player.node_entity_by_name("gMan Head").is_some());
        assert!(player.node_entity_by_name("gMan Pelvis").is_some());
        let bones = player.node_names.iter().filter(|n| n.is_some()).count();
        assert!(bones >= 87, "gMan 骨架应有 ≥87 个具名节点，实际 {bones}");
        // 初始常态：Idle 循环开播
        assert_eq!(player.active, ents.idle, "装配后初始应播 Idle（常态）");
        assert!(player.playing && player.looping, "Idle 应为循环播放态");
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
    {
        let player = game
            .app
            .world
            .get::<iris_engine::prelude::AnimationPlayer>(model)
            .expect("播放器在位");
        assert_eq!(player.active, ents.walk, "移动后应自动切步行循环");
    }
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
    // 播完自动回落常态；此刻 W 仍按住 → 同帧常态切换随即指步行
    let player = game
        .app
        .world
        .get::<iris_engine::prelude::AnimationPlayer>(model)
        .expect("播放器在位");
    assert_eq!(
        player.active, ents.walk,
        "跳跃播完回落 Idle 后，移动中同帧应切步行（无移动则停 Idle，见 idle_default_state 测试）"
    );
    assert!(player.playing, "移动中回落应处于播放态");
}

#[test]
fn idle_default_state() {
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
    let player_of = |game: &Harness| -> (usize, f32, bool, bool) {
        let p = game
            .app
            .world
            .get::<iris_engine::prelude::AnimationPlayer>(model)
            .expect("播放器在位");
        (p.active, p.time, p.playing, p.looping)
    };

    // ---- 无输入：Idle 常态循环推进（不再定格）----
    let (active0, time0, playing0, looping0) = player_of(&game);
    assert_eq!(active0, ents.idle, "初始常态应为 Idle");
    assert!(playing0 && looping0, "Idle 初始即循环播放");
    for _ in 0..10 {
        game.frame();
    }
    let (active1, time1, playing1, _) = player_of(&game);
    assert_eq!(active1, ents.idle, "无输入帧应停在 Idle");
    assert!(
        time1 > time0,
        "Idle 应随时间推进（循环播放）：{time0} -> {time1}"
    );
    assert!(playing1, "Idle 循环保持播放");

    // ---- 移动 → 步行；停止 → 经相位门回落 Idle（走完当前步再切）----
    game.hold(KeyCode::KeyW);
    game.frame();
    game.frame();
    let (active_w, _, _, _) = player_of(&game);
    assert_eq!(active_w, ents.walk, "移动应自动切步行");
    game.release(KeyCode::KeyW);
    // 停步相位门：此刻步行相位约 0.02（落在 0~0.10 窗内）立即放行；其他
    // 相位则保持步行至多约半步——两种情形都应在 40 帧内切回 Idle。
    let mut switched = false;
    for _ in 0..40 {
        game.frame();
        let (active_i, time_i, playing_i, _) = player_of(&game);
        if active_i == ents.idle {
            assert!(
                time_i < 0.1,
                "回落 Idle 应从头播放（time 归零后至多推进一帧）：{time_i}"
            );
            assert!(playing_i, "回落 Idle 处于播放态");
            switched = true;
            break;
        }
        assert_eq!(active_i, ents.walk, "相位门等待期应保持步行");
    }
    assert!(switched, "停止应在 40 帧内过相位门切回 Idle");

    // ---- 跳跃（无移动）播完 → 回落 Idle 并保持循环 ----
    {
        let mut input = game.app.world.resource_mut::<Input>();
        input.press(iris_engine::prelude::KeyCode::Space);
    }
    game.frame();
    let (active_j, _, _, looping_j) = player_of(&game);
    assert_eq!(active_j, ents.jump, "空格应进跳跃单次");
    assert!(!looping_j, "跳跃应为非循环");
    // Jump 1.9 s ≈ 114 帧 @60fps，推进 130 帧确保播完
    for _ in 0..130 {
        game.frame();
    }
    let (active_after, _, playing_after, looping_after) = player_of(&game);
    assert_eq!(active_after, ents.idle, "无移动时跳跃播完应回落常态 Idle");
    assert!(playing_after && looping_after, "回落 Idle 应为循环播放态");
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

    // ---- W 按住 30 帧：位移 = 起步加速的镜像积分 ----
    // 镜像用与控制相同的纯函数（exp_approach_vec3）逐步推进——公式的
    // 解析性质（残差指数衰减、切分无关）由 plugin 单测锁定，这里锁接线：
    // 目标速度方向、加速率与积分口径一致。dt 取测试台时钟的精确值。
    game.hold(KeyCode::KeyW);
    for _ in 0..30 {
        game.frame();
    }
    let dt = game.app.world.resource::<Time>().delta_secs();
    let mut vel = Vec3::ZERO;
    let mut z_mirror = 0.0_f32;
    for _ in 0..30 {
        vel = exp_approach_vec3(vel, Vec3::new(0.0, 0.0, -MOVE_SPEED), ACCEL_RATE, dt);
        z_mirror += vel.z * dt;
    }
    {
        let pos = model_pos(&game);
        assert!(
            (pos.z - z_mirror).abs() < 1e-5,
            "W 30 帧位移应等于起步加速的镜像积分（{z_mirror:.4} m），实际 {pos:?}"
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

    // ---- 切 S：朝 +Z 折返（速度经零点反向加速），60 帧后朝向趋近 0 ----
    game.release(KeyCode::KeyW);
    game.hold(KeyCode::KeyS);
    for _ in 0..60 {
        game.frame();
    }
    for _ in 0..60 {
        vel = exp_approach_vec3(vel, Vec3::new(0.0, 0.0, MOVE_SPEED), ACCEL_RATE, dt);
        z_mirror += vel.z * dt;
    }
    {
        let pos = model_pos(&game);
        assert!(
            (pos.z - z_mirror).abs() < 1e-5,
            "S 60 帧折返位移应等于镜像积分（{z_mirror:.4} m，速度过零反向加速），实际 {pos:?}"
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

    // ---- 停止：相位门走完当前步 → 淡出 + 速度指数减速 → 归零后恒等 ----
    game.release(KeyCode::KeyS);
    // 释放时步行相位约 0.53（窗外）——门先保持步行：释放后 3 帧位置仍在
    // 前进（走完最后一步，脚步与位移匹配）
    let pos_release = model_pos(&game);
    for _ in 0..3 {
        game.frame();
    }
    assert!(
        model_pos(&game).z > pos_release.z,
        "停步相位门等待期应保持步行前进（走完最后一步）"
    );
    // 过门（约 2 帧）+ 0.10 s 淡出 + 指数减速到 STOP_EPS 以下精确归零
    let mut stopped = false;
    for _ in 0..60 {
        game.frame();
        let velocity = game
            .app
            .world
            .try_resource::<iris_exp::ExpCharacter>()
            .expect("角色状态在位")
            .velocity;
        let active = game
            .app
            .world
            .get::<iris_engine::prelude::AnimationPlayer>(model)
            .expect("播放器在位")
            .active;
        if velocity == Vec3::ZERO && active == ents.idle {
            stopped = true;
            break;
        }
    }
    assert!(
        stopped,
        "停步应在 60 帧内完全静止（相位门 + 淡出 + 减速归零）"
    );
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

/// crossfade 状态机接线（依赖资产）：切状态当帧挂过渡（源剪辑快照、
/// 时长、计时精确断言）、过渡到期清除、过渡中段姿势与硬切对照可判别地
/// 不同、过渡完成后与硬切对照重新逐位一致（目标剪辑时间轴两台同源，
/// 过渡清除后姿势唯一来源是目标剪辑）。
#[test]
fn state_machine_crossfade_blend_lifecycle() {
    if !asset_available() {
        return;
    }
    let mut soft = Harness::new();
    let mut hard = Harness::new();
    let ents = *soft
        .app
        .world
        .try_resource::<iris_exp::ExpEntities>()
        .expect("实体表在位");
    let model = ents.model.expect("资产在位时角色应装配成功");
    let model_hard = hard
        .app
        .world
        .try_resource::<iris_exp::ExpEntities>()
        .expect("实体表在位")
        .model
        .expect("资产在位时角色应装配成功");

    // 骨盆全局位姿逐位采样（髋部在 Idle 呼吸与 Walk 摆动间差异显著，
    // 根 Transform 两台同输入同值，差异只来自动画姿势）
    let pelvis_bits = |game: &Harness, model: Entity| -> [u32; 3] {
        let player = game
            .app
            .world
            .get::<AnimationPlayer>(model)
            .expect("播放器在位");
        let pelvis = player
            .node_entity_by_name("gMan Pelvis")
            .expect("骨盆节点在位");
        let gt = game
            .app
            .world
            .get::<GlobalTransform>(pelvis)
            .expect("层级传播已写入")
            .position();
        [gt.x.to_bits(), gt.y.to_bits(), gt.z.to_bits()]
    };

    // ---- 初始：Idle 常态、无过渡 ----
    {
        let player = soft
            .app
            .world
            .get::<AnimationPlayer>(model)
            .expect("播放器在位");
        assert_eq!(player.current_state, STATE_IDLE, "装配后应在 Idle 状态");
        assert_eq!(player.active, ents.idle);
        assert!(player.blend.is_none(), "初始应无过渡");
    }
    // ---- 对照台：清空边表——request_state 无边退化为硬切换 ----
    {
        let mut player = hard
            .app
            .world
            .get_mut::<AnimationPlayer>(model_hard)
            .expect("播放器在位");
        player.edges.clear();
    }

    // ---- 按住 W 各推进 6 帧（过渡中段，约 0.08 s / 0.15 s）----
    soft.hold(KeyCode::KeyW);
    hard.hold(KeyCode::KeyW);
    for _ in 0..6 {
        soft.frame();
        hard.frame();
    }
    {
        let player = soft
            .app
            .world
            .get::<AnimationPlayer>(model)
            .expect("播放器在位");
        assert_eq!(player.current_state, STATE_WALK, "移动应切 Walk 状态");
        let blend = player
            .blend
            .expect("切状态当帧（控制段在动画段之后）应挂上 crossfade");
        assert_eq!(blend.from_clip, ents.idle, "过渡源应是切换前的 Idle 剪辑");
        assert_eq!(blend.duration, STATE_BLEND, "过渡时长应取边表 0.15 s");
        // 第 1 帧控制段挂账（elapsed 0），其后 5 帧动画段各推进一个 dt
        assert!(
            (blend.elapsed - 5.0 / 60.0).abs() < 1e-6,
            "6 帧后过渡计时应约 5/60 s，实际 {}",
            blend.elapsed
        );
    }
    assert_ne!(
        pelvis_bits(&soft, model),
        pelvis_bits(&hard, model_hard),
        "过渡中段的骨盆位姿应与硬切对照可判别地不同（混合权重生效）"
    );

    // ---- 再推进 6 帧（共 12 帧 > 0.15 s）：过渡到期清除 ----
    for _ in 0..6 {
        soft.frame();
        hard.frame();
    }
    {
        let player = soft
            .app
            .world
            .get::<AnimationPlayer>(model)
            .expect("播放器在位");
        assert!(
            player.blend.is_none(),
            "0.15 s 过渡在 12 帧（0.2 s）后应已清除"
        );
        assert_eq!(player.current_state, STATE_WALK);
    }

    // ---- 再推进 24 帧（共 36 帧）：与硬切对照逐位一致 ----
    for _ in 0..24 {
        soft.frame();
        hard.frame();
    }
    assert_eq!(
        pelvis_bits(&soft, model),
        pelvis_bits(&hard, model_hard),
        "过渡完成后位姿应与硬切对照逐位一致（目标时间轴同源）"
    );
}

/// 跳跃自动回落（依赖资产）：起跳挂淡入过渡（源是起跳前剪辑）；跳跃中
/// 移动键不打断；播完由 `ClipFinished` 条件转移分流——静止回 Idle、
/// 移动中直落 Walk（落地即走），回落过渡以跳跃剪辑末端姿势为源。
#[test]
fn jump_landing_auto_transition() {
    if !asset_available() {
        return;
    }
    // —— 变体一：静止起跳、全程无移动，播完自动回 Idle ——
    let mut game = Harness::new();
    let ents = *game
        .app
        .world
        .try_resource::<iris_exp::ExpEntities>()
        .expect("实体表在位");
    let model = ents.model.expect("资产在位时角色应装配成功");
    game.hold(KeyCode::Space);
    game.frame();
    {
        let player = game
            .app
            .world
            .get::<AnimationPlayer>(model)
            .expect("播放器在位");
        assert_eq!(player.current_state, STATE_JUMP, "空格应进 Jump 状态");
        assert!(!player.looping, "Jump 状态应为非循环");
        let blend = player.blend.expect("起跳应挂淡入过渡");
        assert_eq!(blend.from_clip, ents.idle, "静止起跳的过渡源是 Idle");
        assert_eq!(blend.duration, JUMP_IN_BLEND);
    }
    // 推进到播完（Jump 1.9 s ≈ 114 帧 + 余量），落地帧即断言回落过渡
    let mut landed_idle = false;
    for _ in 0..200 {
        game.frame();
        let player = game
            .app
            .world
            .get::<AnimationPlayer>(model)
            .expect("播放器在位");
        if player.current_state != STATE_JUMP {
            assert_eq!(
                player.current_state, STATE_IDLE,
                "静止跳跃播完应自动回 Idle"
            );
            let blend = player
                .blend
                .expect("落地帧应挂回落过渡（引擎条件转移触发）");
            assert_eq!(
                blend.from_clip, ents.jump,
                "回落过渡的源应是跳跃剪辑（末端姿势冻结）"
            );
            assert_eq!(blend.duration, JUMP_OUT_BLEND);
            assert_eq!(blend.elapsed, 0.0, "落地当帧过渡计时应为零");
            assert_eq!(player.active, ents.idle);
            landed_idle = true;
            break;
        }
    }
    assert!(landed_idle, "静止跳跃应在 200 帧内播完自动回落");

    // —— 变体二：移动中起跳且 W 持续按住——跳跃中不打断，播完直落 Walk ——
    let mut game2 = Harness::new();
    let model2 = game2
        .app
        .world
        .try_resource::<iris_exp::ExpEntities>()
        .expect("实体表在位")
        .model
        .expect("资产在位时角色应装配成功");
    game2.hold(KeyCode::KeyW);
    for _ in 0..10 {
        game2.frame();
    }
    game2.hold(KeyCode::Space);
    game2.frame();
    {
        let player = game2
            .app
            .world
            .get::<AnimationPlayer>(model2)
            .expect("播放器在位");
        assert_eq!(player.current_state, STATE_JUMP, "移动中空格应进 Jump");
        let blend = player.blend.expect("起跳应挂淡入过渡");
        assert_eq!(blend.from_clip, ents.walk, "移动中起跳的过渡源是 Walk");
    }
    // 跳跃中段（W 仍按住）：不切走
    for _ in 0..30 {
        game2.frame();
    }
    {
        let player = game2
            .app
            .world
            .get::<AnimationPlayer>(model2)
            .expect("播放器在位");
        assert_eq!(player.current_state, STATE_JUMP, "跳跃进行中移动键不应打断");
    }
    // 播完直落 Walk（moving 参数为真走 Jump 到 Walk 的条件转移）
    let mut landed_walk = false;
    for _ in 0..200 {
        game2.frame();
        let player = game2
            .app
            .world
            .get::<AnimationPlayer>(model2)
            .expect("播放器在位");
        if player.current_state != STATE_JUMP {
            assert_eq!(
                player.current_state, STATE_WALK,
                "移动中跳跃播完应直落 Walk（落地即走）"
            );
            let blend = player
                .blend
                .expect("落地帧应挂回落过渡（引擎条件转移触发）");
            assert_eq!(blend.from_clip, ents.jump);
            assert_eq!(blend.duration, JUMP_OUT_BLEND);
            landed_walk = true;
            break;
        }
    }
    assert!(landed_walk, "移动中跳跃应在 200 帧内播完直落 Walk");
}

/// `ExpPlugin` 不重复注册动画推进（**不依赖资产**）：合成播放器（单通道
/// 剪辑）挂在裸实体上，按窗口路径的装配方式（ExpPlugin + 显式
/// AnimationPlugin，后者等价于 DefaultPlugins 提供的那一份）推进一帧，
/// 播放时间应恰好前进一个 dt。历史 bug 回归锚：ExpPlugin 曾在 build 里
/// 再 build 一次 AnimationPlugin，update_animations 每帧执行两遍，窗口
/// 路径动画以双倍速播放。
#[test]
fn exp_plugin_registers_animation_advance_once() {
    use iris_engine::prelude::{
        AnimationChannel, AnimationClip, AnimationInterpolation, AnimationPath, Transform,
    };

    let mut app = App::new();
    app.world.insert_resource(Time::default());
    app.world.insert_resource(Input::new());
    app.world.insert_resource(MeshStore::new());
    app.world.insert_resource(Camera::default());
    app.world.insert_resource(DirectionalLight::default());
    HierarchyPlugin.build(&mut app);
    ExpPlugin.build(&mut app);
    AnimationPlugin.build(&mut app);
    app.schedule.run(Stage::Startup, &mut app.world);

    let probe = app.world.spawn((Transform::IDENTITY,));
    let clip = AnimationClip {
        name: "probe".to_string(),
        channels: vec![AnimationChannel {
            node: 0,
            path: AnimationPath::Translation,
            interpolation: AnimationInterpolation::Linear,
            times: vec![0.0, 1.0],
            values: vec![0.0, 0.0, 0.0, 0.0, 0.0, 1.0],
        }],
        duration: 1.0,
        events: Vec::new(),
        windows: Vec::new(),
        curves: Vec::new(),
    };
    let player = AnimationPlayer {
        clips: std::sync::Arc::new(vec![clip]),
        node_entities: vec![Some(probe)],
        active: 0,
        playing: true,
        looping: true,
        ..Default::default()
    };
    app.world.insert(probe, player);

    app.world
        .resource_mut::<Time>()
        .advance(Duration::from_secs_f64(1.0 / 60.0));
    app.schedule.run(Stage::Update, &mut app.world);
    let dt = app.world.resource::<Time>().delta_secs();
    let time = app
        .world
        .get::<AnimationPlayer>(probe)
        .expect("播放器在位")
        .time;
    assert!(
        (time - dt).abs() < 1e-9,
        "单次注册的动画推进应恰好前进一个 dt（{dt}），实际 {time}——\
         若接近两倍说明 update_animations 被重复注册"
    );
}

/// 停步相位门行为（依赖资产）：窗外释放保持步行（走完当前步，无过渡
/// 挂账），过窗后切 Idle 并以 STOP_BLEND 淡出——过渡源是步行剪辑的
/// 过步姿势（此刻距 Idle 姿势最近）。
#[test]
fn stop_phase_gate_holds_then_switches() {
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
    // 步行 20 帧：步行相位约 0.33（窗外——窗口在 0.93~1.0、0~0.10、
    // 0.57~0.70），此刻释放应保持步行
    game.hold(KeyCode::KeyW);
    for _ in 0..20 {
        game.frame();
    }
    game.release(KeyCode::KeyW);
    game.frame();
    {
        let player = game
            .app
            .world
            .get::<AnimationPlayer>(model)
            .expect("播放器在位");
        assert_eq!(
            player.current_state, STATE_WALK,
            "窗外释放应保持步行（相位门等待，走完当前步）"
        );
        assert!(player.blend.is_none(), "等待期不应有过渡挂账");
    }
    // 相位 0.33 到 0.57 窗口约 0.22 s（13 帧）+ 切换余量
    let mut switched = false;
    for _ in 0..40 {
        game.frame();
        let player = game
            .app
            .world
            .get::<AnimationPlayer>(model)
            .expect("播放器在位");
        if player.current_state != STATE_WALK {
            assert_eq!(player.current_state, STATE_IDLE, "停步应切回 Idle");
            let blend = player
                .blend
                .expect("切换帧应挂停步淡出过渡（stop_phase_ready 放行）");
            assert_eq!(blend.from_clip, ents.walk, "淡出源是步行剪辑");
            assert_eq!(blend.duration, STOP_BLEND, "停步方向过渡取 STOP_BLEND");
            switched = true;
            break;
        }
    }
    assert!(switched, "停步应在 40 帧内过相位门切回 Idle");
}

/// 停步相位窗口的内容契约锁（依赖资产）：Mixamo_Walk 对 Idle 首帧的
/// 姿势距离曲线（同通道逐分量 L1）在窗口区段处于低位、在满幅分腿区
/// 处于高位——GLB 换版后窗口失配（步行循环相位挪位）时此测试报警。
/// 窗口表的实测依据与曲线数字见 plugin.rs 的 [`stop_phase_ready`] 文档。
#[test]
fn walk_stop_phase_windows_match_pose_distance_curve() {
    use iris_engine::prelude::AnimatedField;

    if !asset_available() {
        return;
    }
    let game = Harness::new();
    let ents = *game
        .app
        .world
        .try_resource::<iris_exp::ExpEntities>()
        .expect("实体表在位");
    let model = ents.model.expect("资产在位时角色应装配成功");
    let player = game
        .app
        .world
        .get::<AnimationPlayer>(model)
        .expect("播放器在位");
    let walk = &player.clips[ents.walk];
    let idle = &player.clips[ents.idle];
    assert_eq!(
        walk.channels.len(),
        idle.channels.len(),
        "两剪辑通道数应一致（crossfade 后与硬切逐位一致的混合前提）"
    );
    let comps = |f: &AnimatedField| -> Vec<f32> {
        match f {
            AnimatedField::Translation(v) | AnimatedField::Scale(v) => v.to_vec(),
            AnimatedField::Rotation(q) => vec![q.x, q.y, q.z, q.w],
        }
    };
    let dist = |phase: f32| -> f32 {
        let t = phase * walk.duration;
        let mut total = 0.0;
        for ch in &walk.channels {
            if let Some(other) = idle
                .channels
                .iter()
                .find(|c| c.node == ch.node && c.path == ch.path)
            {
                for (x, y) in comps(&ch.sample(t)).iter().zip(comps(&other.sample(0.0))) {
                    total += (x - y).abs();
                }
            }
        }
        total
    };
    // 主窗口中心（循环末尾两脚并拢）距离最深
    let d_main = dist(0.97);
    assert!(d_main < 11.5, "主窗口中心距离应 < 11.5，实际 {d_main}");
    // 两段次窗口与循环首帧同量级
    let d_w2 = dist(0.05);
    let d_w3 = dist(0.635);
    assert!(d_w2 < 14.0, "次窗口 0~0.10 中点距离应 < 14.0，实际 {d_w2}");
    assert!(
        d_w3 < 14.0,
        "次窗口 0.57~0.70 中点距离应 < 14.0，实际 {d_w3}"
    );
    // 满幅分腿区显著更远——窗口避开的正是它们
    let d_split_a = dist(0.26);
    let d_split_b = dist(0.52);
    assert!(
        d_split_a > 16.0,
        "相位 0.26（分腿峰）距离应 > 16.0，实际 {d_split_a}"
    );
    assert!(
        d_split_b > 16.0,
        "相位 0.52（分腿峰）距离应 > 16.0，实际 {d_split_b}"
    );
}
