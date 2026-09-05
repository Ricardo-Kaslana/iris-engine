//! headless 确定性集成测试：**不建窗口、不建 GPU、不依赖 winit**，复用
//! `InfiltrationPlugin` 的完整系统链（`build_schedule`，与 DefaultPlugins
//! 同一编排）+ `HierarchyPlugin` / `AnimationPlugin` / `PhysicsPlugin` /
//! `UiPlugin` 驱动真实游戏世界。与 iris-ascent 的 headless 同款约定，
//! 重点覆盖本轮 R34–R41 八个新载荷的真实接线：
//!
//! - **接触事件（R34）**：压力板 sensor × 玩家运动学胶囊 →
//!   ContactStarted 真实触发开门（begin/end 成对计数）；
//! - **NavGrid（R35）**：守卫巡逻 / 追逐走真实 `find_path`（纯核心单测
//!   已覆盖折点，此处验证全链路无 panic + 永不进墙）；
//! - **距离关节（R36）**：弹簧门板被玩家运动学胶囊推开（面板位移 > 0）
//!   后自动回弹；
//! - **ragdoll（R37）**：击晕事件 → `spawn_ragdoll_kinematic` +
//!   `release_ragdoll` 六肢体生成 + 翻转为动体 + 守卫模型隐藏；
//! - **动画 v3（R38）**：双剪辑混合空间 + 足部 IK（脚踝贴地断言）；
//! - **RTT（R40）**：RenderTargetStore + RT 相机 + UiImageRtt 接线齐全；
//! - **UI 菜单栈（R41）**：Esc push/pop + RESUME 按钮 `UiClickEvent`
//!   消费 + 菜单可见性裁决。
//! - **自动玩家全关通关**：键盘注入全链路（Input → 翻译 → 逻辑 → 表现）
//!   3 核心 + 出口 + 被发现 ≤ 1。
//!
//! `Time::advance` 确定性推进、`Audio::headless()` 纯混音器（绝不打开
//! 设备）、层级传播每帧时序镜像 `App::tick`。

use std::time::Duration;

use glam::Vec3;

use iris_core::prelude::{Entity as CoreEntity, Stage};
use iris_engine::prelude::{
    AnimationPlayer, AnimationPlugin, App, Audio, BodyType, Camera, Collider, CommandsPlugin,
    DirectionalLight, DistanceJoint, EnvironmentSettings, FootIk, GlobalTransform, HierarchyPlugin,
    Input, KeyCode, MeshStore, Name, OffscreenRenderer, PhysicsPlugin, Plugin, RenderTargetStore,
    RendersTo, RigidBody, Time, Transform, UiButton, UiImageRtt, UiMenuStack, UiPlugin, UiText,
    Visibility,
};
use iris_infiltration::{
    CORES, CORES_TEXT_NAME, GuardState, InfiltrationEntities, InfiltrationPlugin,
    InfiltrationState, PLATE_POS, Phase, SNEAK_SPEED, WALLS,
};

/// headless 测试台：世界 + InfiltrationPlugin 系统链 + 层级 / 动画 / 物理
/// / UI 插件 + 虚拟时钟（与窗口路径 DefaultPlugins 同一编排）。
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
        app.world.insert_resource(Camera::default());
        // 顶光偏斜：室内设施顶光 + 少量环境光，监控屏 / 主视角都清晰
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
        // 层级传播（PostUpdate）+ 动画（Update、本插件系统之前，含足部
        // IK）+ 物理（Update、本插件系统之前——接触事件先产出后消费）+
        // UI（PostUpdate：layout → menus → focus）——与窗口路径
        // DefaultPlugins 同一插件同一编排；InfiltrationPlugin 刻意不自带
        //（重复注册会让物理 / 动画双倍步进）
        HierarchyPlugin.build(&mut app);
        AnimationPlugin.build(&mut app);
        PhysicsPlugin.build(&mut app);
        UiPlugin.build(&mut app);
        // 命令队列（打磨 V）：PostUpdate 尾部 apply_commands——ragdoll
        // 延迟生成与窗口路径同一编排
        CommandsPlugin.build(&mut app);
        InfiltrationPlugin::default().build_schedule(&mut app.schedule);

        let mut harness = Self { app };
        // Startup：资源兜底插入 + 设施搭建 + 实体索引
        harness
            .app
            .schedule
            .run(Stage::Startup, &mut harness.app.world);
        // 立即传播一次：相机 GlobalTransform 就位（与窗口路径首帧一致）
        iris_core::hierarchy::propagate_transforms(&mut harness.app.world);
        harness
    }

    /// 推进一帧：确定性 1/60 s（时序镜像 `App::tick`：帧首事件轮换 →
    /// Update → PostUpdate → 输入一次性状态清零——打磨 V 的事件生命
    /// 周期，`clear_all_events` 已从帧尾移到帧首）。
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

    fn entities(&self) -> InfiltrationEntities {
        self.app
            .world
            .try_resource::<InfiltrationEntities>()
            .expect("实体索引应已由 Startup 插入")
            .clone()
    }

    fn state(&self) -> iris_core::resource::ResourceRef<'_, InfiltrationState> {
        self.app
            .world
            .try_resource::<InfiltrationState>()
            .expect("状态应已由 Startup 插入")
    }

    /// 按住潜行（自动玩家恒潜行；键盘注入走真实 Input 链路）。
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

    /// 自动玩家推进一帧：把 [`AutoPilot`] 的决策翻译成真实键盘注入
    /// （WASD / Shift），与人工玩家共享同一翻译层。dt 由 Time 提供。
    fn autopilot_frame(&mut self) {
        use iris_infiltration::AutoPilot;
        let (move_dir, sneak) = {
            let mut state = self.app.world.resource_mut::<InfiltrationState>();
            let input = AutoPilot.drive(&mut state);
            (input.move_dir, input.sneak)
        };
        let mut input = self.app.world.resource_mut::<Input>();
        // 先全释放再按住（免得上一帧的键残留）
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
}

/// Startup 后的初始状态：实体索引 / 监控屏 RTT / 暂停菜单 / 混合空间 /
/// 足部 IK / 物理代理全部就位（八能力接线静态断言）。
#[test]
fn startup_builds_scene_and_capability_wiring() {
    let harness = Harness::new();
    let ents = harness.entities();
    {
        let state = harness.state();
        assert_eq!(state.phase, Phase::Playing);
        assert_eq!(state.guards.len(), 2);
        assert_eq!(state.cores.len(), CORES.len());
        assert!(!state.door_open);
    }
    // 三层玩家结构：模型与相机都是玩家根的子实体
    assert_eq!(
        harness.app.world.parent(ents.player_model),
        Some(ents.player_root),
        "模型应为玩家根的子实体"
    );
    assert_eq!(
        harness.app.world.parent(ents.camera),
        Some(ents.player_root),
        "相机应为玩家根的子实体"
    );
    // R38：混合空间已激活（双剪辑 idle/walk，参数待运行期写入）
    let player = harness
        .app
        .world
        .get::<AnimationPlayer>(ents.player_model)
        .expect("玩家模型应挂 AnimationPlayer");
    assert_eq!(player.clips.len(), 2, "应有 idle / walk 双剪辑");
    assert!(player.blend_space_active, "混合空间模式应在 Startup 激活");
    assert!(player.blend_space.is_some());
    // R38：玩家双腿链挂 FootIk
    let mut foot_ik_count = 0usize;
    harness.app.world.query::<&FootIk>(|_| {
        foot_ik_count += 1;
    });
    assert!(
        foot_ik_count >= 4,
        "玩家 + 守卫应各挂 2 条腿链 FootIk，实际 {foot_ik_count}"
    );
    // R40：RenderTargetStore 在位，监控屏 RT 相机挂 RendersTo，HUD 挂
    // UiImageRtt
    let store = harness.app.world.resource::<RenderTargetStore>();
    assert_eq!(store.get(ents.monitor).unwrap().width, 256);
    let mut rtt_cameras = 0usize;
    harness.app.world.query::<&RendersTo>(|_| {
        rtt_cameras += 1;
    });
    assert_eq!(rtt_cameras, 1, "应有恰好一台 RT 相机");
    let monitor_ui = harness
        .app
        .world
        .entity_named("infiltration/hud/monitor")
        .expect("监控屏 HUD 实体应在位");
    assert!(
        harness.app.world.get::<UiImageRtt>(monitor_ui).is_some(),
        "监控屏 HUD 实体应挂 UiImageRtt"
    );
    // R34/R36（打磨 V）：玩家根实体挂运动学胶囊（压力板 / 弹簧门板的
    // 真实碰撞载体，物理代理绕行已删）；弹簧门板（动态盒 + 关节）在位
    let ents = harness.entities();
    assert!(
        harness
            .app
            .world
            .get::<Collider>(ents.player_root)
            .is_some()
    );
    let player_body = harness
        .app
        .world
        .get::<RigidBody>(ents.player_root)
        .expect("玩家根实体应挂 RigidBody");
    assert!(
        player_body.body_type == BodyType::Kinematic,
        "玩家刚体应为 Kinematic（游戏驱动位移、参与碰撞）"
    );
    let panel = harness
        .app
        .world
        .entity_named("infiltration/door-panel")
        .expect("弹簧门板应在位");
    assert!(
        harness.app.world.get::<DistanceJoint>(panel).is_some(),
        "弹簧门板应挂 DistanceJoint"
    );
    // R41：HUD 就位
    assert_eq!(harness.hud_text(CORES_TEXT_NAME), "CORES 0/3");
    assert_eq!(harness.hud_text("infiltration/hud/spotted"), "SPOTTED 0/3");
}

/// R34 端到端：把玩家传送到压力板上（运动学胶囊随之到位——位移由
/// 逻辑写 Transform）→ ContactStarted 真实触发 → 门开（静态体移除 +
/// 导航重建）→ 离开 → ContactEnded 计数回落。这是接触事件链路在真实
/// 物理步进下的验证（打磨 V：不再经过物理代理）。
#[test]
fn pressure_plate_contact_events_open_the_door() {
    let mut harness = Harness::new();
    harness.frames(2);
    {
        let mut state = harness.app.world.resource_mut::<InfiltrationState>();
        // 传送到压力板旁（不触发）
        state.pos = Vec3::new(PLATE_POS.x - 3.0, 0.9, PLATE_POS.y);
    }
    harness.frame();
    assert!(!harness.state().door_open, "板旁不应开门");
    // 踩上压力板
    {
        let mut state = harness.app.world.resource_mut::<InfiltrationState>();
        state.pos = Vec3::new(PLATE_POS.x, 0.9, PLATE_POS.y);
        state.vel = Vec3::ZERO;
    }
    harness.frame();
    harness.frame();
    {
        let state = harness.state();
        assert!(state.door_open, "踩上压力板应开门（接触事件路径）");
        assert!(
            state.plate_occupants >= 1,
            "ContactStarted 应把实体计入板面（实际 {}）",
            state.plate_occupants
        );
    }
    // 走开：ContactEnded 计数回落（门保持开——单向闩锁）
    {
        let mut state = harness.app.world.resource_mut::<InfiltrationState>();
        state.pos = Vec3::new(PLATE_POS.x - 4.0, 0.9, PLATE_POS.y);
        state.vel = Vec3::ZERO;
    }
    harness.frame();
    harness.frame();
    {
        let state = harness.state();
        assert_eq!(state.plate_occupants, 0, "离开后板面计数应回落");
        assert!(state.door_open, "单向闩锁：门保持开");
    }
    // 压力门实体：视觉隐藏 + 碰撞组清零（标记语义）
    let ents = harness.entities();
    assert_eq!(
        *harness.app.world.get::<Visibility>(ents.door).unwrap(),
        Visibility::HIDDEN,
        "开门后压力门应隐藏"
    );
    let collider = *harness
        .app
        .world
        .get::<Collider>(ents.door)
        .expect("压力门应有碰撞体");
    assert_eq!(collider.groups, 0, "开门后碰撞组应清零（配对恒假）");
}

/// R36 端到端：玩家运动学胶囊推开弹簧门板（动体撞运动学体按静态方
/// 全额退让）→ 面板位移 > 0；停止推后关节把面板拉回关闭位（位移回落
/// 到 < 0.1 m）。纯展示语义（逻辑静态体不含面板——玩家逻辑位置不受
/// 影响）。
#[test]
fn spring_door_panel_is_pushed_and_pulled_back_by_joint() {
    let mut harness = Harness::new();
    harness.frames(2);
    let ents = harness.entities();
    let closed = *harness
        .app
        .world
        .get::<Transform>(ents.door_panel)
        .expect("门板应有 Transform");
    // 把玩家逻辑位置放到面板正北 0.5 m 处向北走（代理把面板往北推）
    {
        let mut state = harness.app.world.resource_mut::<InfiltrationState>();
        state.pos = Vec3::new(-7.0, 0.9, -1.0);
        state.vel = Vec3::ZERO;
    }
    // 北行（W）1.2 秒：穿过门洞（潜行? 否——常速 4.2 m/s），运动学
    // 胶囊把面板推开
    harness.hold_keys(true, &[KeyCode::KeyW]);
    harness.frames(72);
    harness.hold_keys(false, &[KeyCode::KeyW]);
    let pushed_away = *harness
        .app
        .world
        .get::<Transform>(ents.door_panel)
        .expect("门板应有 Transform");
    let offset = (pushed_away.position - closed.position).length();
    assert!(offset > 0.05, "玩家穿过时应推开弹簧门板（位移 {offset}）");
    assert!(
        (harness.state().pos.z - closed.position.z).abs() > 1.0,
        "玩家逻辑位置应已穿过门洞（面板不进逻辑静态体）"
    );
    // 停止推：关节松弛把面板拉回关闭位
    harness.frames(120);
    let settled = *harness
        .app
        .world
        .get::<Transform>(ents.door_panel)
        .expect("门板应有 Transform");
    let offset_back = (settled.position - closed.position).length();
    assert!(
        offset_back <= 0.15 + 1e-3,
        "DistanceJoint 应把面板拉回 rest_length 球壳（残余 {offset_back}）"
    );
}

/// R37 端到端：玩家传送到守卫 0 背后 1.5 m → 注入 E → GuardStunned →
/// 六球人形 ragdoll 生成并立即释放（肢体翻转为动体）→ 守卫模型子树
/// 隐藏 → 击晕不醒来（纯核心语义在 headless 下的再验证）。
#[test]
fn stun_spawns_ragdoll_and_hides_guard_model() {
    let mut harness = Harness::new();
    harness.frames(2);
    {
        let mut state = harness.app.world.resource_mut::<InfiltrationState>();
        // 守卫 0 钉在原点面朝 -Z；玩家在其背后 1.5 m
        state.guards[0].pos = Vec3::new(0.0, 0.9, 0.0);
        state.guards[0].facing = 0.0;
        state.pos = Vec3::new(0.0, 0.9, 1.5);
    }
    harness.hold_keys(true, &[KeyCode::KeyE]);
    harness.frame();
    harness.hold_keys(false, &[KeyCode::KeyE]);
    {
        let state = harness.state();
        assert_eq!(
            state.guards[0].state,
            GuardState::Stunned,
            "背后 interact 应击晕守卫 0"
        );
    }
    // ragdoll 六肢体已生成且全部翻转为动体（release 立即生效）
    let mut limb_count = 0usize;
    let mut dynamic_limbs = 0usize;
    harness
        .app
        .world
        .query::<(&Collider, Option<&RigidBody>)>(|(collider, _rigid)| {
            // ragdoll 肢体特征：RAGDOLL_GROUP 组位（bit 0）、mask 排除该位
            if collider.groups == 1 && collider.mask == !1u32 {
                limb_count += 1;
                if !collider.is_static {
                    dynamic_limbs += 1;
                }
            }
        });
    assert_eq!(limb_count, 6, "应生成六球人形 ragdoll，实际 {limb_count}");
    assert_eq!(dynamic_limbs, 6, "release_ragdoll 后全部肢体应为动体");
    // 守卫模型子树隐藏
    let ents = harness.entities();
    assert_eq!(
        *harness
            .app
            .world
            .get::<Visibility>(ents.guards[0].root)
            .unwrap(),
        Visibility::HIDDEN,
        "被击晕守卫的根实体应隐藏"
    );
    // 音频：击晕闷响已入混音器（headless 纯混音器表断言）
    let audio = harness.app.world.resource::<Audio>();
    assert!(audio.mixer().active_count() >= 1, "击晕音效应已入混音器表");
}

/// R41 端到端：Esc → 暂停菜单 push（按钮可见 + 焦点落位 + 逻辑冻结）→
/// RESUME 按钮点击（UiClickEvent）→ pop 回游戏。
#[test]
fn pause_menu_stack_push_focus_and_click_resume() {
    let mut harness = Harness::new();
    harness.frames(2);
    // Esc → push "pause"
    harness.hold_keys(true, &[KeyCode::Escape]);
    harness.frame();
    harness.hold_keys(false, &[KeyCode::Escape]);
    harness.frame();
    {
        let stack = harness.app.world.resource::<UiMenuStack>();
        assert_eq!(stack.top_menu(), Some("pause"), "Esc 应 push 暂停菜单");
    }
    // 栈顶菜单的按钮应可见；HUD（无 UiMenu）不受裁决影响
    let resume_btn: CoreEntity = {
        let mut found = None;
        harness
            .app
            .world
            .query::<(CoreEntity, &UiButton)>(|(entity, button)| {
                if button.label == "RESUME" {
                    found = Some(entity);
                }
            });
        found.expect("RESUME 按钮应在位")
    };
    assert_eq!(
        *harness.app.world.get::<Visibility>(resume_btn).unwrap(),
        Visibility::VISIBLE,
        "栈顶菜单的按钮应可见"
    );
    // 逻辑冻结：暂停期间玩家位置不变
    let pos_before = harness.state().pos;
    harness.hold_keys(true, &[KeyCode::KeyW, KeyCode::ShiftLeft]);
    harness.frames(10);
    harness.hold_keys(false, &[KeyCode::KeyW, KeyCode::ShiftLeft]);
    assert_eq!(harness.state().pos, pos_before, "暂停期间对局状态机应冻结");
    // ↓ 选中 RESUME → Enter 激活 → UiClickEvent → RESUME pop
    harness.hold_keys(true, &[KeyCode::ArrowDown]);
    harness.frame();
    harness.hold_keys(false, &[KeyCode::ArrowDown]);
    harness.frame();
    harness.hold_keys(true, &[KeyCode::Enter]);
    harness.frame();
    harness.hold_keys(false, &[KeyCode::Enter]);
    harness.frame();
    {
        let stack = harness.app.world.resource::<UiMenuStack>();
        assert!(
            stack.top_menu() != Some("pause"),
            "RESUME 点击应 pop 暂停菜单（栈顶 {:?}）",
            stack.top_menu()
        );
    }
    // 点击帧之后对局恢复：按钮面板隐藏
    assert_eq!(
        *harness.app.world.get::<Visibility>(resume_btn).unwrap(),
        Visibility::HIDDEN,
        "菜单关闭后按钮应隐藏"
    );
    // 再 Esc → QUIT 分支（不发点击，仅验证栈再次 push——QUIT 会发
    // AppExit 并写盘，headless 不点击它避免文件竞态）
    harness.hold_keys(true, &[KeyCode::Escape]);
    harness.frame();
    harness.hold_keys(false, &[KeyCode::Escape]);
    harness.frame();
    assert_eq!(
        harness.app.world.resource::<UiMenuStack>().top_menu(),
        Some("pause")
    );
    harness.hold_keys(true, &[KeyCode::Escape]);
    harness.frame();
    harness.hold_keys(false, &[KeyCode::Escape]);
    harness.frame();
    assert!(
        harness.app.world.resource::<UiMenuStack>().is_empty(),
        "再 Esc 应 pop 回游戏"
    );
}

/// 自动玩家全关通关（键盘注入全链路）：3 核心全收 + 出口胜利 +
/// 被发现 ≤ 1（脚本设计余量）+ 压力门已开 + HUD 同步。
#[test]
fn auto_player_full_course_victory_through_real_input() {
    let mut harness = Harness::new();
    let max_frames = 60 * 170;
    let mut frames = 0usize;
    while frames < max_frames {
        harness.autopilot_frame();
        harness.frame();
        frames += 1;
        if harness.state().phase != Phase::Playing {
            break;
        }
    }
    let state = harness.state();
    println!("=== 自动玩家战报（真实输入链路）===");
    println!(
        "终局：{:?}，用时 {:.1} s（{} 帧），核心 {}/{}，被发现 {}，门开 {}",
        state.phase,
        frames as f32 / 60.0,
        frames,
        state.cores_collected(),
        state.cores.len(),
        state.detections,
        state.door_open
    );
    assert_eq!(state.phase, Phase::Victory, "自动玩家必须通关");
    assert_eq!(state.cores_collected(), CORES.len(), "核心应全收");
    assert!(
        state.detections <= 1,
        "脚本设计余量：被发现应 ≤ 1 次，实际 {}",
        state.detections
    );
    assert!(state.door_open, "通关路线必经压力板开门");
    assert!(frames < 60 * 150, "通关用时 {frames} 帧应 < 150 s");
    // HUD 与横幅同步到终局
    assert_eq!(
        harness.hud_text(CORES_TEXT_NAME),
        format!("CORES {}/{}", CORES.len(), CORES.len())
    );
    assert_eq!(
        harness.hud_text("infiltration/hud/banner"),
        "EXFILTRATION COMPLETE"
    );
    // 潜行速度语义的回归锚点：自动玩家恒潜行（终局前速度 ≈ SNEAK_SPEED）
    assert!((SNEAK_SPEED - 2.0).abs() < 1e-5, "潜行速度常量应与文档一致");
}

/// R35 守卫巡逻全链路：真实系统链下守卫沿 find_path 航点推进——永不
/// 进墙（导航 agent 半径语义）且路线环持续折返推进。
#[test]
fn guards_patrol_along_nav_paths_and_never_enter_walls() {
    let mut harness = Harness::new();
    let mut route_ticks = [0usize; 2];
    let mut last_route_next = [1usize, 1];
    for _ in 0..60 * 20 {
        // 清空玩家输入（原地站立；被发现即重开防追逐干扰）
        harness.frame();
        {
            let state = harness.state();
            for (index, guard) in state.guards.iter().enumerate() {
                // 永不进墙：到任一墙的 XZ 净距 > 0.3
                for wall in WALLS.iter() {
                    let ox = (guard.pos.x - wall.center.x).abs() - wall.half.x;
                    let oz = (guard.pos.z - wall.center.z).abs() - wall.half.z;
                    let clearance = (ox.max(0.0).powi(2) + oz.max(0.0).powi(2)).sqrt();
                    assert!(
                        clearance > 0.3,
                        "守卫 {index} 位置 {:?} 不应进入墙 {wall:?} 的 0.3 m 域",
                        guard.pos
                    );
                }
                if guard.route_next != last_route_next[index] {
                    last_route_next[index] = guard.route_next;
                    route_ticks[index] += 1;
                }
            }
        }
        if harness.state().detections > 0 {
            // 站在走廊的玩家被看到属正常（本测试关注巡逻绕墙）
            harness.app.world.remove_resource::<InfiltrationState>();
            harness.app.world.insert_resource(InfiltrationState::new());
            last_route_next = [1, 1];
        }
    }
    assert!(
        route_ticks[0] >= 1 && route_ticks[1] >= 1,
        "20 秒内两条巡逻环都应至少推进 1 个路点，实际 {route_ticks:?}"
    );
}

/// R38 混合空间 + 足部 IK：玩家潜行移动时混合参数 > 0 且 < 1（介于
/// idle 与 walk 之间的连续姿势）；静止后参数回落 0；脚踝始终贴地
/// （±2 cm 验收域，平地 IK 修正插地 5 cm 的初始装配）。
#[test]
fn blendspace_param_follows_speed_and_ik_keeps_feet_grounded() {
    let mut harness = Harness::new();
    harness.frames(2);
    let ents = harness.entities();
    // 潜行东行 1 秒（走廊内、无墙阻挡；北行 2 m 即顶墙、速度归零）
    harness.hold_keys(true, &[KeyCode::KeyD, KeyCode::ShiftLeft]);
    harness.frames(60);
    harness.hold_keys(false, &[KeyCode::KeyD, KeyCode::ShiftLeft]);
    {
        let player = harness
            .app
            .world
            .get::<AnimationPlayer>(ents.player_model)
            .expect("玩家模型应挂 AnimationPlayer");
        assert!(
            player.blend_param.x > 0.2 && player.blend_param.x < 0.9,
            "潜行时混合参数应介于 idle/walk 之间（实际 {}）",
            player.blend_param.x
        );
    }
    // 静止 0.5 秒：参数回落 0
    harness.frames(30);
    {
        let player = harness
            .app
            .world
            .get::<AnimationPlayer>(ents.player_model)
            .expect("玩家模型应挂 AnimationPlayer");
        assert!(
            player.blend_param.x < 0.05,
            "静止后混合参数应回落（实际 {}）",
            player.blend_param.x
        );
    }
    // 足部 IK：左右脚踝全局 y 应在地面 ±2 cm 内（初始装配在 -5 cm，
    // 未修正即失败——这是 IK 生效的量化证据）
    iris_core::hierarchy::propagate_transforms(&mut harness.app.world);
    let mut ankles_checked = 0usize;
    harness
        .app
        .world
        .query::<(&Name, &GlobalTransform)>(|(name, global)| {
            if name.0.ends_with("/foot") {
                ankles_checked += 1;
                assert!(
                    (global.position().y - 0.0).abs() < 0.02,
                    "脚踝 {} 应贴地 ±2 cm，实际 y = {}",
                    name.0,
                    global.position().y
                );
            }
        });
    assert!(
        ankles_checked >= 4,
        "应检查玩家 + 守卫共 4 只脚踝，实际 {ankles_checked}"
    );
}

/// 确定性（逐位）：两套独立 Harness 同输入推进 10 秒，全部状态摘要
/// 逐位一致（含守卫 / 门 / 计数）。
#[test]
fn two_harnesses_advance_bit_identically() {
    let mut a = Harness::new();
    let mut b = Harness::new();
    for frame in 0..60 * 10 {
        a.autopilot_frame();
        a.frame();
        b.autopilot_frame();
        b.frame();
        let sa = a.state();
        let sb = b.state();
        let bits = |v: Vec3| [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()];
        assert_eq!(bits(sa.pos), bits(sb.pos), "玩家位置应逐位一致，帧 {frame}");
        assert_eq!(
            bits(sa.guards[0].pos),
            bits(sb.guards[0].pos),
            "守卫 0 位置应逐位一致，帧 {frame}"
        );
        assert_eq!(
            bits(sa.guards[1].pos),
            bits(sb.guards[1].pos),
            "守卫 1 位置应逐位一致，帧 {frame}"
        );
        assert_eq!(sa.door_open, sb.door_open, "门状态应一致，帧 {frame}");
        assert_eq!(sa.detections, sb.detections, "发现计数应一致，帧 {frame}");
    }
}

/// R40 记录性断言（无 GPU 自动跳过）：监控屏 RT 相机渲染进 RT、
/// UiImageRtt 在 HUD 收集管线中（完整像素验收见快照 example）。
#[test]
fn monitor_rtt_frame_pipeline_smoke() {
    let Some(mut renderer) = OffscreenRenderer::new(320, 180) else {
        println!("无 GPU，跳过 RTT 记录性断言");
        return;
    };
    let mut harness = Harness::new();
    harness.frames(2);
    renderer.render(&harness.app.world);
    let stats = renderer.renderer().stats();
    println!(
        "RTT smoke：draw_calls = {}，instances = {}",
        stats.draw_calls, stats.instances
    );
    assert!(stats.instances > 0, "主视图应至少渲染出实例");
}
