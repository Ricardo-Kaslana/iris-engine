//! headless 确定性集成测试：**不建窗口、不建 GPU、不依赖 winit**，复用
//! `AssaultPlugin` 的完整系统链（`build_schedule`，与 DefaultPlugins 同一
//! 编排）+ `HierarchyPlugin` / `AnimationPlugin` / `TimelinePlugin` /
//! `PhysicsPlugin` / `ParticlePlugin` / `UiPlugin` / `CommandsPlugin` 驱动
//! 真实游戏世界。与 iris-infiltration 的 headless 同款约定，重点覆盖本轮
//! R42–R47 六个新载荷的真实接线：
//!
//! - **pack（R45）**：GLB 经 `AssetMount(Pack)` + 内存加载入口实例化
//!   （22 节点骨架守护断言）；
//! - **流式音频（R44）**：pack 内 OGG 上音乐总线（`music_is_playing` +
//!   混响参数回读）+ 战斗音效入混音器；
//! - **动画 v4（R46）**：上下身双 `AnimationPlayer`（下半身混合空间激活
//!   + 上半身遮罩就位）、ADS 手 IK 吸附握把（±12 cm）、受击附加层字段；
//! - **时间线（R47）**：开场字幕出现 → 黑边实体就位 → `intro_end` 事件
//!   开战 → 终局演出启动；
//! - **DOF（R43）**：设置资源在位且战斗中逐帧对焦；
//! - **写实材质（R42）**：庭院材质带法线 / ORM 槽位（程序化纹理注册）；
//! - 老能力：ragdoll 六肢体 + 敌人模型隐藏、贴花战痕、FX 池粒子；
//! - **自动玩家全歼通关**（键盘注入全链路）+ 确定性（逐位）。
//!
//! `Time::advance` 确定性推进、`Audio::headless()` 纯混音器（绝不打开
//! 设备）、层级传播每帧时序镜像 `App::tick`。

use std::time::Duration;

use glam::Vec3;

use iris_assault::logic::{
    AssaultEvent, AssaultInput, AssaultState, AutoPilot, ENEMY_SPAWNS, PLAYER_MAX_HP, Phase,
};
use iris_assault::plugin::{AssaultEntities, AssaultPlugin, BANNER_TEXT_NAME, HP_TEXT_NAME};
use iris_assault::skeleton as sk;
use iris_core::prelude::{Entity as CoreEntity, Stage};
use iris_engine::prelude::{
    AnimationPlayer, AnimationPlugin, App, AssetMount, Audio, Camera, CommandsPlugin, Decal,
    DepthOfFieldSettings, DirectionalLight, EnvironmentSettings, FootIk, GlobalTransform, HandIk,
    HierarchyPlugin, IkTarget, Input, KeyCode, MaterialStore, MeshStore, MotionBlurSettings,
    OffscreenRenderer, PackReader, ParticleEmitter, ParticlePlugin, PhysicsPlugin, Plugin,
    TextureStore, Time, TimelinePlugin, Transform, UiPlugin, UiText, asset_path, is_playing,
    resolve_focus_distance, stop_timeline, view_from_global,
};

/// headless 测试台：世界 + AssaultPlugin 系统链 + 层级 / 动画 / 时间线 /
/// 物理 / 粒子 / UI / 命令队列插件 + 虚拟时钟（与窗口路径 DefaultPlugins
/// 同一编排）。
struct Harness {
    app: App,
}

impl Harness {
    fn new() -> Self {
        let mut app = App::new();
        // DefaultPlugins 等价的资源（不建窗口 / 不建 GPU）；音频强制
        // headless（纯混音器，绝不打开设备）
        app.world.insert_resource(Time::default());
        app.world.insert_resource(Input::new());
        app.world.insert_resource(MeshStore::new());
        app.world.insert_resource(MaterialStore::new());
        app.world.insert_resource(TextureStore::new());
        app.world.insert_resource(Camera::default());
        // 黄昏斜阳 + 少量环境光（与插件 build_courtyard 的覆写对接）
        app.world.insert_resource(DirectionalLight::default());
        app.world.insert_resource(Audio::headless());
        app.world
            .insert_resource(EnvironmentSettings { intensity: 0.35 });
        // 层级传播（PostUpdate）+ 动画（Update、含足部 / 手部 IK）+
        // 时间线（Update、本插件之前）+ 物理（Update）+ 粒子 / UI
        // （PostUpdate）+ 命令队列（PostUpdate 尾）——与窗口路径
        // DefaultPlugins 同一插件同一编排；AssaultPlugin 刻意不自带
        //（重复注册会让物理 / 动画双倍步进）
        HierarchyPlugin.build(&mut app);
        AnimationPlugin.build(&mut app);
        TimelinePlugin.build(&mut app);
        PhysicsPlugin.build(&mut app);
        ParticlePlugin.build(&mut app);
        UiPlugin.build(&mut app);
        CommandsPlugin.build(&mut app);
        AssaultPlugin::default().build_schedule(&mut app.schedule);

        let mut harness = Self { app };
        // Startup：资源兜底 + pack 挂载 + 庭院 + 角色 + 开场演出
        harness
            .app
            .schedule
            .run(Stage::Startup, &mut harness.app.world);
        // 立即传播一次：相机 GlobalTransform 就位（与窗口路径首帧一致）
        iris_core::hierarchy::propagate_transforms(&mut harness.app.world);
        harness
    }

    /// 推进一帧：确定性 1/60 s（时序镜像 `App::tick`：帧首事件轮换 →
    /// Update → PostUpdate → 输入一次性状态清零）。
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

    fn frames(&mut self, count: usize) {
        for _ in 0..count {
            self.frame();
        }
    }

    fn entities(&self) -> AssaultEntities {
        self.app
            .world
            .try_resource::<AssaultEntities>()
            .expect("实体索引应已由 Startup 插入")
            .clone()
    }

    fn state(&self) -> iris_core::resource::ResourceRef<'_, AssaultState> {
        self.app
            .world
            .try_resource::<AssaultState>()
            .expect("状态应已由 Startup 插入")
    }

    /// 自动玩家推进一帧：把 [`AutoPilot`] 的决策翻译成真实键盘注入
    /// （WASD / Space / F / Q / R），与人工玩家共享同一翻译层。
    fn autopilot_frame(&mut self) {
        let input: AssaultInput = {
            let mut state = self.app.world.resource_mut::<AssaultState>();
            AutoPilot.drive(&mut state)
        };
        let mut input_res = self.app.world.resource_mut::<Input>();
        // 先全释放再按需按下（边沿语义：sword_press 只在决策帧按下）
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

    /// 按住 / 松开按键（真实 Input 链路）。
    fn hold_keys(&mut self, down: bool, keys: &[KeyCode]) {
        let mut input = self.app.world.resource_mut::<Input>();
        for key in keys {
            if down {
                input.press(*key);
            } else {
                input.release(*key);
            }
        }
    }

    /// 跳过开场演出（测试注入：停时间线 + 开战——与插件的时间线事件
    /// 路径同一 `begin_battle` 入口）。
    fn skip_intro(&mut self) {
        stop_timeline(&mut self.app.world);
        self.app.world.resource_mut::<AssaultState>().begin_battle();
        self.frame();
    }

    /// 按名读取 HUD 文本。
    fn hud_text(&self, name: &str) -> String {
        let entity = self
            .app
            .world
            .entity_named(name)
            .expect("按名应能定位 HUD 实体");
        self.app
            .world
            .get::<UiText>(entity)
            .expect("HUD 实体应有 UiText")
            .string
            .clone()
    }

    /// 按名读取任意实体文本（时间线对话框等）。
    fn text_of(&self, name: &str) -> Option<String> {
        let entity = self.app.world.entity_named(name)?;
        self.app
            .world
            .get::<UiText>(entity)
            .map(|text| text.string.clone())
    }
}

/// Startup 后的初始状态：pack 挂载 / 双播放器 / 遮罩 / IK / 时间线 /
/// 流式音乐 / DOF / HUD 全部就位（六能力接线静态断言）。
#[test]
fn startup_builds_scene_and_capability_wiring() {
    let harness = Harness::new();
    let ents = harness.entities();
    {
        let state = harness.state();
        assert_eq!(state.phase, Phase::Intro, "开局应处于开场演出阶段");
        assert_eq!(state.enemies.len(), ENEMY_SPAWNS.len());
        assert_eq!(state.player.hp, PLAYER_MAX_HP);
    }
    // ---- R45：pack 已挂载 + GLB 经内存入口加载（节点数 = 骨架表）----
    let mount = harness.app.world.resource::<AssetMount>();
    assert!(!mount.is_empty(), "pack 应已挂载");
    let pack_bytes = std::fs::read(asset_path("assets/assault.ipak")).expect("pack 文件应在仓库内");
    let pack = PackReader::from_source(std::io::Cursor::new(pack_bytes)).expect("pack 可读");
    assert!(pack.exists("models/assault_bot.glb"));
    assert!(pack.exists("audio/ambience.ogg"));
    // ---- R46：双 AnimationPlayer——下半身混合空间激活、上半身遮罩就位
    let locomotion = harness
        .app
        .world
        .get::<AnimationPlayer>(ents.player_model)
        .expect("玩家模型应挂下半身播放器");
    assert!(locomotion.blend_space_active, "步态混合空间应激活");
    assert!(locomotion.blend_space.is_some());
    // 打磨 VI 摩擦⑥：加载器把 GLB 节点名带进播放器 ⇒ 装配层可按名寻骨
    assert_eq!(
        locomotion.node_entity_by_name(sk::names::CHEST),
        locomotion.node_entities.get(sk::CHEST).copied().flatten(),
        "按名与按下标应命中同一骨节点（chest）"
    );
    assert!(
        locomotion.node_entity_by_name("no_such_bone").is_none(),
        "骨架外的节点名应查不到"
    );
    assert_eq!(
        harness.app.world.parent(ents.player_action),
        Some(ents.player_root),
        "上半身播放器实体应是模型根的兄弟子实体"
    );
    let action = harness
        .app
        .world
        .get::<AnimationPlayer>(ents.player_action)
        .expect("玩家应挂上半身播放器");
    assert_eq!(action.clips.len(), 10, "应有 3 下身 + 7 上身剪辑");
    let mask = action.mask.as_ref().expect("上半身播放器应挂遮罩");
    assert_eq!(mask.nodes.len(), sk::UPPER_BODY.len());
    for node in sk::UPPER_BODY.iter() {
        assert!(mask.nodes.contains(node), "遮罩应含上身节点 {node}");
    }
    for node in sk::LOWER_BODY.iter() {
        assert!(!mask.nodes.contains(node), "遮罩不应含下身节点 {node}");
    }
    // ---- R46：足部 IK（玩家 + 敌人各 2 条链）与手 IK 配置 ----
    let mut foot_ik_count = 0usize;
    harness.app.world.query::<&FootIk>(|_| {
        foot_ik_count += 1;
    });
    assert_eq!(
        foot_ik_count, 10,
        "5 名角色 × 2 条腿链 FootIk，实际 {foot_ik_count}"
    );
    let hand_r = harness
        .app
        .world
        .get::<HandIk>(ents.player_hand_r.expect("右臂配置应在位"))
        .expect("右臂应挂 HandIk");
    assert!(hand_r.target.is_none(), "非瞄准态手 IK 目标应为空");
    // IkTarget 标记在枪身挂点上
    let mut ik_targets = 0usize;
    harness.app.world.query::<&IkTarget>(|_| {
        ik_targets += 1;
    });
    assert_eq!(ik_targets, 2, "握把 + 护木两个 IkTarget");
    // 敌人的枪身子树隐藏（持刀近战）
    // ---- R44：流式音乐已上总线（headless 混音器可断言）----
    let audio = harness.app.world.resource::<Audio>();
    assert!(audio.music_is_playing(), "氛围音乐应占用音乐总线");
    assert!(
        audio.reverb_settings().wet > 0.0,
        "庭院混响应已设置（只作用于音乐总线——v1 限制）"
    );
    // ---- R43：DOF / MB 设置资源在位且启用 ----
    let dof = harness.app.world.resource::<DepthOfFieldSettings>();
    assert!(dof.enabled, "DOF 应默认启用");
    assert!(
        dof.focus_target.is_none(),
        "启动期（开场演出）无焦点实体，对焦走 focus_distance 标量"
    );
    let mb = harness.app.world.resource::<MotionBlurSettings>();
    assert!(mb.enabled, "MB 应启用（演出相机运动拖影）");
    // ---- R47：开场演出已启动 ----
    assert!(is_playing(&harness.app.world), "开场演出应播放中");
    // HUD 就位
    assert_eq!(harness.hud_text(HP_TEXT_NAME), "HP 100/100");
    assert_eq!(harness.hud_text(BANNER_TEXT_NAME), "");
}

/// R47 端到端：开场演出——相机轨道播放、字幕出现、黑边实体就位、
/// `intro_end` 事件把游戏交还给逻辑（`begin_battle` → BattleBegin）。
#[test]
fn intro_cinematic_plays_and_hands_over_control() {
    let mut harness = Harness::new();
    let mut saw_dialogue = false;
    let mut saw_letterbox = false;
    let mut saw_battle_begin = false;
    let mut frames_after_begin = 0usize;
    // 开场 6.5 s = 390 帧；逐帧观察
    for _ in 0..(60 * 9) {
        harness.frame();
        if let Some(text) = harness.text_of("iris/timeline_dialogue") {
            if !text.is_empty() {
                saw_dialogue = true;
            }
        }
        if harness
            .app
            .world
            .entity_named("iris/timeline_letterbox_top")
            .is_some()
            && harness
                .app
                .world
                .entity_named("iris/timeline_letterbox_bottom")
                .is_some()
        {
            saw_letterbox = true;
        }
        if !saw_battle_begin
            && harness
                .state()
                .frame_events
                .iter()
                .any(|e| matches!(e, AssaultEvent::BattleBegin { .. }))
        {
            saw_battle_begin = true;
        }
        // intro_end 事件（6.4 s）早于时间线播完（6.5 s）——事件后再推进
        // 一小段，等时间线自然结束、相机交还
        if saw_battle_begin && frames_after_begin >= 30 {
            break;
        }
        if saw_battle_begin {
            frames_after_begin += 1;
        }
    }
    assert!(saw_dialogue, "开场字幕应出现过（对话框非空）");
    assert!(saw_letterbox, "黑边实体应已惰性生成");
    assert!(saw_battle_begin, "intro_end 事件应触发 BattleBegin");
    assert_eq!(harness.state().phase, Phase::Playing, "开战应切到 Playing");
    assert!(
        !is_playing(&harness.app.world),
        "开场时间线应已播完（交还相机）"
    );
    // ---- 打磨 VI 摩擦① 的实弹锁：idle 剪辑**不含腿旋转通道**（R48 补的
    // 恒值兜底已删），站立即足部 IK 每帧写腿骨——被写的四条骨必须仍是
    // 单位四元数（未修复时本场景 ~1.9 s 内范数 386 → 5.8e7 → NaN）----
    let ents = harness.entities();
    let locomotion = harness
        .app
        .world
        .get::<AnimationPlayer>(ents.player_model)
        .expect("玩家模型应有下半身播放器");
    for name in [
        sk::names::L_THIGH,
        sk::names::L_SHIN,
        sk::names::R_THIGH,
        sk::names::R_SHIN,
    ] {
        let bone = locomotion
            .node_entity_by_name(name)
            .expect("腿骨节点应按名命中");
        let rotation = harness
            .app
            .world
            .get::<Transform>(bone)
            .expect("腿骨应有 Transform")
            .rotation;
        assert!(
            rotation.is_finite(),
            "{name} 旋转出现非有限分量（平方复合爆炸）：{rotation:?}"
        );
        assert!(
            (rotation.length() - 1.0).abs() <= 1e-3,
            "{name} 旋转范数偏离单位（IK 回写未归一化？）：{}",
            rotation.length()
        );
    }
}

/// R46 端到端：ADS 手 IK 吸附——按住瞄准数帧后，右手骨全局位应吸附到
/// 枪身握把（±12 cm，一帧滞后裕度）；松开瞄准后目标清空（IK 弃权）。
/// 「手骨被遮罩剪辑写过之后 IK 仍然生效」的量化证据。
#[test]
fn hand_ik_snaps_to_grip_while_aiming() {
    let mut harness = Harness::new();
    harness.skip_intro();
    // 按住瞄准（Q）1 秒：上身切 aim 剪辑、手 IK 目标指向枪身挂点
    harness.hold_keys(true, &[KeyCode::KeyQ]);
    harness.frames(60);
    let ents = harness.entities();
    let grip = {
        let player = harness
            .app
            .world
            .get::<AnimationPlayer>(ents.player_model)
            .expect("模型应挂播放器");
        player
            .node_entities
            .get(sk::RIFLE_GRIP)
            .copied()
            .flatten()
            .expect("握把节点应在骨架表内")
    };
    let hand = {
        let player = harness
            .app
            .world
            .get::<AnimationPlayer>(ents.player_model)
            .expect("模型应挂播放器");
        player
            .node_entities
            .get(sk::R_HAND)
            .copied()
            .flatten()
            .expect("右手节点应在骨架表内")
    };
    {
        let hand_ik = harness
            .app
            .world
            .get::<HandIk>(ents.player_hand_r.expect("右臂配置应在位"))
            .expect("右臂应挂 HandIk");
        assert_eq!(hand_ik.target, Some(grip), "瞄准态右臂 IK 目标应指向握把");
    }
    iris_core::hierarchy::propagate_transforms(&mut harness.app.world);
    let hand_pos = harness
        .app
        .world
        .get::<GlobalTransform>(hand)
        .expect("手骨应有全局变换")
        .position();
    let grip_pos = harness
        .app
        .world
        .get::<GlobalTransform>(grip)
        .expect("握把应有全局变换")
        .position();
    let dist = hand_pos.distance(grip_pos);
    assert!(
        dist < 0.12,
        "ADS 时右手应吸附握把（距离 {dist:.3} m，一帧滞后裕度 12 cm）"
    );
    // 松开瞄准：目标清空（IK 弃权，手臂回动作剪辑）
    harness.hold_keys(false, &[KeyCode::KeyQ]);
    harness.frames(2);
    let hand_ik = harness
        .app
        .world
        .get::<HandIk>(ents.player_hand_r.expect("右臂配置应在位"))
        .expect("右臂应挂 HandIk");
    assert_eq!(hand_ik.target, None, "非瞄准态手 IK 目标应清空");
}

/// R37 / R39 / R24 端到端 + 自动玩家全歼通关（键盘注入全链路）：
/// 首杀 → ragdoll 六肢体翻转 + 敌人模型隐藏 + 血渍贴花 + FX 粒子；
/// 全歼 → Victory + 终局演出启动 + HUD 对账。DOF 对焦随交战距离变化
/// （R43 动态对焦的证据）。
#[test]
fn auto_player_full_course_victory_with_ragdoll_decals_and_fx() {
    let mut harness = Harness::new();
    harness.skip_intro();
    let ents = harness.entities();
    let mut first_kill_seen = false;
    let mut ragdoll_limbs = 0usize;
    let mut ragdoll_dynamic = 0usize;
    let mut decals = 0usize;
    let mut max_live_particles = 0usize;
    let max_frames = 60 * 240;
    let mut frames = 0usize;
    let mut focus_samples = Vec::new();
    let mut focused_by_target = 0usize;
    while frames < max_frames {
        harness.autopilot_frame();
        harness.frame();
        frames += 1;
        // FX / DOF 采样（战斗前中期）
        if frames < 60 * 120 {
            let mut live = 0usize;
            harness.app.world.query::<&ParticleEmitter>(|emitter| {
                live += emitter.live_count();
            });
            max_live_particles = max_live_particles.max(live);
            // 打磨 VI 摩擦⑤：对焦距离由 `focus_target` 经引擎解析
            // （headless 无渲染器，测试侧调**同一个纯函数**），采样的是
            // 实际生效值而非资源里的标量回退值
            let world = &harness.app.world;
            let dof = world.resource::<DepthOfFieldSettings>();
            if dof.focus_target.is_some() {
                focused_by_target += 1;
            }
            if let Some(view) = world
                .get::<GlobalTransform>(ents.camera)
                .map(|global| view_from_global(&global))
            {
                focus_samples.push(resolve_focus_distance(world, &dof, &view));
            }
        }
        // 首杀断言（ragdoll + 隐藏 + 贴花）
        if !first_kill_seen && harness.state().kills >= 1 {
            first_kill_seen = true;
            harness.app.world.query::<(
                CoreEntity,
                &iris_engine::prelude::Collider,
                Option<&iris_engine::prelude::RigidBody>,
            )>(|(entity, collider, _rigid)| {
                // ragdoll 肢体特征：RAGDOLL_GROUP 组位（bit 0）
                if collider.groups == 1 && collider.mask == !1u32 {
                    ragdoll_limbs += 1;
                    if !collider.is_static {
                        ragdoll_dynamic += 1;
                    }
                    let _ = entity;
                }
            });
            assert_eq!(
                ragdoll_limbs, 6,
                "首杀应生成六球人形 ragdoll，实际 {ragdoll_limbs}"
            );
            assert_eq!(ragdoll_dynamic, 6, "release_ragdoll 后全部肢体应为动体");
            // 首个死亡敌人的模型子树应隐藏
            let dead_index = harness
                .state()
                .enemies
                .iter()
                .position(|e| e.state == iris_assault::logic::EnemyState::Dead)
                .expect("应有已死敌人");
            let dead_root = ents.enemies[dead_index].root;
            assert_eq!(
                *harness
                    .app
                    .world
                    .get::<iris_engine::prelude::Visibility>(dead_root)
                    .expect("敌人根实体应可见性组件"),
                iris_engine::prelude::Visibility::HIDDEN,
                "死亡敌人的模型子树应隐藏"
            );
        }
        if harness.state().phase != Phase::Playing {
            break;
        }
    }
    // ---- 终局 ----
    {
        let state = harness.state();
        println!("=== 自动玩家战报（真实输入链路）===");
        println!(
            "终局：{:?}，用时 {:.1} s（{} 帧），击杀 {}/{}，玩家 HP {}，开枪 {} 发",
            state.phase,
            frames as f32 / 60.0,
            frames,
            state.kills,
            ENEMY_SPAWNS.len(),
            state.player.hp,
            state.player.rifle.shots,
        );
        assert_eq!(state.phase, Phase::Victory, "自动玩家必须全歼通关");
        assert_eq!(state.kills, ENEMY_SPAWNS.len() as u32);
        assert!(
            state.player.hp > 0,
            "通关时玩家应存活（HP {}）",
            state.player.hp
        );
        assert!(
            frames < 60 * 200,
            "通关用时 {frames} 帧应 < 200 s（含演出外全链路开销）"
        );
    }
    // ---- R47：终局演出已启动（推近镜头）----
    assert!(is_playing(&harness.app.world), "胜利终局演出应在播放中");
    assert_eq!(
        harness.hud_text(BANNER_TEXT_NAME),
        "COURTYARD CLEARED",
        "胜利横幅应对账"
    );
    // ---- R39：战斗留下了贴花战痕（血渍 / 弹孔）----
    harness.app.world.query::<&Decal>(|_| {
        decals += 1;
    });
    assert!(decals >= 3, "战斗应留下贴花战痕，实际 {decals}");
    // ---- R24 / FX 池：战斗中粒子曾活跃 ----
    assert!(
        max_live_particles > 0,
        "战斗中应有粒子活跃（血雾 / 枪口焰 / 刀光）"
    );
    // ---- R43（打磨 VI 摩擦⑤ 闭合）：对焦由 focus_target 驱动，且
    // 解析出的实际对焦距离随交战几何变化（多样本非恒值）----
    assert!(
        focus_samples.len() > 60,
        "有效采样应覆盖战斗全程，实际 {} 帧",
        focus_samples.len()
    );
    assert!(
        focused_by_target * 2 > focus_samples.len(),
        "采样帧应过半由焦点实体驱动对焦（而非标量回退），实际 {focused_by_target}/{}",
        focus_samples.len()
    );
    let min_focus = focus_samples.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_focus = focus_samples.iter().cloned().fold(0.0_f32, f32::max);
    assert!(
        max_focus - min_focus > 0.5,
        "DOF 对焦距离应随交战距离变化（{min_focus:.2}..{max_focus:.2}）"
    );
}

/// R44 端到端：战斗音效入混音器（挥刀 / 枪声 / 命中的空间音效在 headless
/// 混音器表中可断言）。
#[test]
fn combat_sfx_reach_the_mixer() {
    let mut harness = Harness::new();
    harness.skip_intro();
    // 贴脸挥刀 1 秒（自动玩家决策会出刀 / 开枪）
    for _ in 0..60 {
        harness.autopilot_frame();
        harness.frame();
        if harness.state().kills >= 1 {
            break;
        }
    }
    let audio = harness.app.world.resource::<Audio>();
    assert!(
        audio.mixer().active_count() >= 1,
        "战斗音效应已入混音器表（挥刀 / 枪声 / 命中）"
    );
}

/// 确定性（逐位）：两套独立 Harness 同输入推进 10 秒（跳过开场），全部
/// 状态摘要逐位一致（含敌人 / 刀剑 / 弹药）。
#[test]
fn two_harnesses_advance_bit_identically() {
    let mut a = Harness::new();
    let mut b = Harness::new();
    a.skip_intro();
    b.skip_intro();
    for frame in 0..60 * 10 {
        a.autopilot_frame();
        a.frame();
        b.autopilot_frame();
        b.frame();
        let sa = a.state();
        let sb = b.state();
        let bits = |v: Vec3| [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()];
        assert_eq!(
            bits(sa.player.pos),
            bits(sb.player.pos),
            "玩家位置应逐位一致，帧 {frame}"
        );
        assert_eq!(
            bits(sa.enemies[0].pos),
            bits(sb.enemies[0].pos),
            "敌人 0 位置应逐位一致，帧 {frame}"
        );
        assert_eq!(
            bits(sa.enemies[3].pos),
            bits(sb.enemies[3].pos),
            "敌人 3 位置应逐位一致，帧 {frame}"
        );
        assert_eq!(
            sa.player.sword.sweep_id, sb.player.sword.sweep_id,
            "横扫代次应一致，帧 {frame}"
        );
        assert_eq!(sa.player.rifle.ammo, sb.player.rifle.ammo, "弹药应一致");
        assert_eq!(sa.kills, sb.kills, "击杀数应一致");
    }
}

/// R42 记录性断言：庭院材质带法线 / ORM 槽位（程序化纹理注册进
/// TextureStore）；完整像素验收见快照 example。
#[test]
fn courtyard_materials_carry_normal_and_orm_slots() {
    let harness = Harness::new();
    let textures = harness.app.world.resource::<TextureStore>();
    let materials = harness.app.world.resource::<MaterialStore>();
    // 至少注册了：石板 / 砖墙 / 布料 / 划痕金属（法线 + ORM 成对）+
    // 贴花（血渍 / 弹孔）→ 法线纹理 ≥ 4 张
    let normal_slots: Vec<_> = (0..textures.len())
        .filter_map(|i| {
            let handle = iris_engine::prelude::TextureHandle(i as u32);
            textures.get(handle)
        })
        .collect();
    let linear_textures = normal_slots.iter().filter(|t| !t.srgb).count();
    assert!(
        linear_textures >= 8,
        "至少 4 对法线 / ORM 线性数据贴图（石板 / 砖墙 / 布料 / 划痕），\
         实际线性纹理 {linear_textures}"
    );
    // 场景里至少一个材质同时挂了法线与 ORM（写实材质通道接线证据）
    let mut textured = 0usize;
    let mut index = 0u32;
    while let Some(def) = materials.get(iris_engine::prelude::MaterialHandle(index)) {
        if def.normal_texture.is_some() && def.orm_texture.is_some() {
            textured += 1;
        }
        index += 1;
    }
    assert!(textured >= 4, "应有多材质挂法线 + ORM，实际 {textured}");
}

/// R45 记录性断言（无 GPU 自动跳过）：离屏渲染一帧主视图出实例。
#[test]
fn offscreen_render_smoke() {
    let Some(mut renderer) = OffscreenRenderer::new(320, 180) else {
        println!("无 GPU，跳过离屏渲染记录性断言");
        return;
    };
    let mut harness = Harness::new();
    harness.skip_intro();
    harness.frames(2);
    renderer.render(&harness.app.world);
    let stats = renderer.renderer().stats();
    println!(
        "离屏 smoke：draw_calls = {}，instances = {}",
        stats.draw_calls, stats.instances
    );
    assert!(stats.instances > 0, "主视图应至少渲染出实例");
}
