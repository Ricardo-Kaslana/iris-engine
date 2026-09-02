//! headless 确定性集成测试：**不建窗口、不建 GPU、不依赖 winit**，复用
//! `AscentPlugin` 的完整系统链（`build_schedule`，与 DefaultPlugins 同一
//! 编排）+ `HierarchyPlugin` / `ParticlePlugin` / `AnimationPlugin` 驱动
//! 真实游戏世界。与 iris-bastion 的 headless 同款约定，重点覆盖本轮
//! R28–R31 四个新载荷的真实接线：
//!
//! - **动画状态机（R28）**：双剪辑 GLB 加载 + states/edges 装配；
//!   移动 ↔ 静止的 `request_state` 双向 crossfade 实测切换；
//! - **角色控制器（R31）**：`move_and_slide_statics` 就是逻辑核心——
//!   键盘输入全链路（Input 注入 → 相机相对翻译 → 逻辑 → 写回实体）的
//!   **脚本化自动玩家全关通关**（宝石全收、检查点全激活、坠落 0）；
//! - **粒子 FX 池**：落地尘土 / 宝石火花在 FX 槽位上真实触发；
//! - **空间音频**：事件定位声源入混音器 + 听者跟随第三人称相机；
//! - **LOD（R30）**：装饰 `LodMesh` 近景 / 拉远的 `triangles_drawn`
//!   对比（无 GPU 环境自动跳过）。
//!
//! `Time::advance` 确定性推进、`Audio::headless()` 纯混音器（绝不打开
//! 设备）、层级传播每帧时序镜像 `App::tick`。

use std::time::Duration;

use glam::Vec3;

use iris_ascent::{
    AscentPlugin, AscentState, AutoPilot, BannerTimer, CHECKPOINTS, CameraRig, DECOR_COUNT,
    FALLS_TEXT_NAME, FxPool, GEMS, GEMS_TEXT_NAME, HALF_HEIGHT, Phase, STATE_IDLE, STATE_WALK,
};
use iris_core::prelude::Stage;
use iris_engine::prelude::{
    AnimationPlayer, App, Audio, Camera, DirectionalLight, HierarchyPlugin, Input, KeyCode,
    LodMesh, MeshStore, OffscreenRenderer, ParticleEmitter, ParticlePlugin, Plugin, Time,
    Transform, UiText, Visibility,
};

/// headless 测试台：世界 + AscentPlugin 系统链 + 层级 / 粒子 / 动画插件 +
/// 虚拟时钟（与窗口路径 DefaultPlugins 同一编排）。
struct Harness {
    app: App,
}

impl Harness {
    fn new() -> Self {
        let mut app = App::new();
        // DefaultPlugins 等价的资源（不建窗口 / 不建 GPU）；音频强制
        // headless（纯混音器，绝不打开设备）；AscentPlugin 的
        // setup_resources 对已存在资源一律不覆盖
        app.world.insert_resource(Time::default());
        app.world.insert_resource(Input::new());
        app.world.insert_resource(MeshStore::new());
        app.world.insert_resource(Camera::default());
        // 斜照方向光（-X 水平分量：影子横向铺开、在第三人称镜头里可见——
        // 高台把长影投在低台 / 跳板上，CSM 跨台投影；intensity 0.9 柔和日光）
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

        // 层级传播（PostUpdate）+ 粒子推进（PostUpdate、层级之后）+
        // 动画推进（Update、AscentPlugin 系统之前）——与窗口路径
        // DefaultPlugins 同一插件同一编排；AscentPlugin 刻意不自带
        //（重复注册会让粒子双倍推进 / 动画双倍步进）
        HierarchyPlugin.build(&mut app);
        ParticlePlugin.build(&mut app);
        iris_engine::prelude::AnimationPlugin.build(&mut app);
        AscentPlugin::default().build_schedule(&mut app.schedule);

        let mut harness = Self { app };
        // Startup：资源兜底插入 + 关卡搭建 + FX 池预热
        harness
            .app
            .schedule
            .run(Stage::Startup, &mut harness.app.world);
        // 立即传播一次：相机 GlobalTransform 就位（与窗口路径首帧一致）
        iris_core::hierarchy::propagate_transforms(&mut harness.app.world);
        harness
    }

    /// 推进一帧：确定性 1/60 s。
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

    /// 按住 / 松开 W（相机相对的前进）。
    fn hold_forward(&mut self, down: bool) {
        let mut input = self.app.world.resource_mut::<Input>();
        if down {
            input.press(KeyCode::KeyW);
        } else {
            input.release(KeyCode::KeyW);
        }
    }

    /// 注入空格按下边沿（先强制释放再按下，保证本帧产生 pressed）。
    fn tap_jump(&mut self) {
        let mut input = self.app.world.resource_mut::<Input>();
        input.release(KeyCode::Space);
        input.press(KeyCode::Space);
    }

    fn state(&self) -> iris_core::resource::ResourceRef<'_, AscentState> {
        self.app
            .world
            .try_resource::<AscentState>()
            .expect("AscentState 应已由 Startup 插入")
    }

    /// FX 槽位上的活粒子总数。
    fn live_particles(&self) -> usize {
        let slots = self
            .app
            .world
            .try_resource::<FxPool>()
            .expect("FX 池在位")
            .slots
            .clone();
        slots
            .iter()
            .map(|&slot| {
                self.app
                    .world
                    .get::<ParticleEmitter>(slot)
                    .expect("FX 槽位应有发射器")
                    .live_count()
            })
            .sum()
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

/// Startup 后的初始状态：FX 池预热、HUD 就位、动画状态机装配完毕、
/// LOD 装饰齐数、三层玩家结构就位。
#[test]
fn startup_builds_scene_pools_and_state_machine() {
    let harness = Harness::new();
    {
        let state = harness.state();
        assert_eq!(state.phase, Phase::Playing);
        assert_eq!(state.gems.len(), GEMS.len());
        assert_eq!(state.checkpoints.len(), CHECKPOINTS.len());
        assert_eq!(state.falls, 0);
    }
    // FX 池容量与待命态
    let fx_slots = harness
        .app
        .world
        .try_resource::<FxPool>()
        .expect("FX 池在位")
        .slots
        .clone();
    assert_eq!(fx_slots.len(), 12);
    for &slot in &fx_slots {
        let emitter = harness
            .app
            .world
            .get::<ParticleEmitter>(slot)
            .expect("FX 槽位应预挂禁用发射器");
        assert_eq!(emitter.live_count(), 0, "待命发射器应无活粒子");
    }
    // HUD 按名可定位
    assert_eq!(
        harness.hud_text(GEMS_TEXT_NAME),
        format!("GEMS 0/{}", GEMS.len())
    );
    assert_eq!(harness.hud_text(FALLS_TEXT_NAME), "FALLS 0");
    // 动画状态机：双态双向边（GLB 双剪辑路径）
    let ents = harness
        .app
        .world
        .try_resource::<iris_ascent::AscentEntities>()
        .expect("实体索引在位")
        .clone();
    let player = harness
        .app
        .world
        .get::<AnimationPlayer>(ents.player_model)
        .expect("模型实体应挂 AnimationPlayer");
    assert_eq!(player.clips.len(), 2, "应有 idle / walk 双剪辑");
    assert_eq!(player.states.len(), 2);
    assert_eq!(player.edges.len(), 2, "idle ↔ walk 双向边");
    assert_eq!(player.current_state, STATE_IDLE, "开局静止态");
    assert!(
        player.clips.iter().any(|clip| clip.name == "idle")
            && player.clips.iter().any(|clip| clip.name == "walk"),
        "剪辑名应为 idle / walk：{:?}",
        player.clips.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    let walk = player
        .clips
        .iter()
        .find(|clip| clip.name == "walk")
        .expect("walk 剪辑在位");
    assert!(
        walk.events.iter().any(|event| event.name == "footstep"),
        "walk 剪辑应注入 footstep 事件轨道"
    );
    // LOD 装饰齐数：全部挂 LodMesh、切换距离一致
    let mut lod_count = 0usize;
    harness
        .app
        .world
        .query::<(&Transform, Option<&LodMesh>)>(|(_transform, lod)| {
            if lod.is_some() {
                lod_count += 1;
            }
        });
    assert!(
        lod_count >= DECOR_COUNT,
        "LOD 装饰应 ≥ {DECOR_COUNT}，实际 {lod_count}"
    );
    // 三层结构：模型与相机都是玩家根的子实体
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
}

/// 脚本化自动玩家：键盘注入全链路（Input → 相机相对翻译 → 逻辑 →
/// 表现）跑完线性关卡——**必须通关、宝石全收、检查点全激活、坠落
/// 0**（脚本设计内余量：0，见 logic 单测的解析对照）。
#[test]
fn auto_player_full_course_victory_through_real_input() {
    let mut harness = Harness::new();
    let pilot = AutoPilot;
    let mut prev_jump = false;
    let max_frames = 60 * 150;
    let mut frames = 0usize;
    let mut events_seen = (0usize, 0usize, 0usize); // (gem, checkpoint, launch)

    while frames < max_frames {
        let (jump, phase, gems, cps, launches) = {
            let state = harness.state();
            (
                pilot.drive(&state).jump,
                state.phase,
                state.gems_collected(),
                state.checkpoints_activated(),
                state
                    .frame_events
                    .iter()
                    .filter(|e| matches!(e, iris_ascent::AscentEvent::Launch { .. }))
                    .count(),
            )
        };
        events_seen.0 = events_seen.0.max(gems);
        events_seen.1 = events_seen.1.max(cps);
        events_seen.2 += launches; // 逐帧事件列表不重叠，直接累加
        if phase != Phase::Playing {
            break;
        }
        // 前进恒按住；跳跃带内注入空格边沿（电平 → 边沿的插件翻译镜像）
        harness.hold_forward(true);
        if jump || prev_jump {
            harness.tap_jump();
        }
        prev_jump = jump;
        harness.frame();
        frames += 1;
    }

    let state = harness.state();
    println!("=== 自动玩家战报（真实输入链路）===");
    println!(
        "终局：{:?}，用时 {:.1} s（游戏时间，{} 帧）",
        state.phase,
        frames as f32 / 60.0,
        frames
    );
    println!(
        "宝石 {}/{}，检查点 {}/{}，坠落 {}",
        state.gems_collected(),
        state.gems.len(),
        state.checkpoints_activated(),
        state.checkpoints.len(),
        state.falls
    );
    assert_eq!(state.phase, Phase::Victory, "自动玩家必须通关");
    assert_eq!(state.falls, 0, "坠落必须为 0（脚本设计余量）");
    assert_eq!(state.gems_collected(), GEMS.len(), "宝石应全收");
    assert_eq!(
        state.checkpoints_activated(),
        CHECKPOINTS.len(),
        "检查点应全激活"
    );
    assert_eq!(events_seen.2, 4, "四段跳板都应弹射过");
    assert!(frames < 60 * 75, "通关用时 {} 帧应 < 75 s", frames);
    // HUD 与横幅同步到终局
    assert_eq!(
        harness.hud_text(GEMS_TEXT_NAME),
        format!("GEMS {}/{}", GEMS.len(), GEMS.len())
    );
    assert_eq!(harness.hud_text("ascent/hud/banner"), "SUMMIT REACHED!");
}

/// 状态机（R28）：按住 W → request_state(walk) + crossfade 启动；松开 →
/// 回 idle 且过渡完成。
#[test]
fn state_machine_switches_walk_idle_with_crossfade() {
    let mut harness = Harness::new();
    let ents = harness
        .app
        .world
        .try_resource::<iris_ascent::AscentEntities>()
        .expect("实体索引在位")
        .clone();

    // 按住 W 两帧：应切入 walk 并带 crossfade（0.15 s 过渡进行中）
    harness.hold_forward(true);
    harness.frame();
    harness.frame();
    {
        let player = harness
            .app
            .world
            .get::<AnimationPlayer>(ents.player_model)
            .expect("AnimationPlayer 在位");
        assert_eq!(player.current_state, STATE_WALK, "移动应切到 walk 态");
        assert!(player.blend.is_some(), "有边应启动 crossfade");
        assert_eq!(player.active, STATE_WALK, "活动剪辑应为 walk");
    }
    // 松开 W 0.25 s（> 过渡时长 0.15 s）：应回 idle 且过渡清除
    harness.hold_forward(false);
    harness.frames(15);
    {
        let player = harness
            .app
            .world
            .get::<AnimationPlayer>(ents.player_model)
            .expect("AnimationPlayer 在位");
        assert_eq!(player.current_state, STATE_IDLE, "停下应回 idle 态");
        assert!(player.blend.is_none(), "过渡应已完成清除");
    }
}

/// 落地 / 宝石事件 → FX 池粒子 + 空间音频（R24/R25 同款链路在本作的
/// 再验证）：传送角色到宝石旁收集 → 火花 burst + 声源入混音器；跳一次
/// → 起跳尘土 + 落地重尘土。
#[test]
fn gem_and_landing_events_trigger_fx_and_spatial_audio() {
    let mut harness = Harness::new();
    // 站稳一帧（首帧落地吸附）
    harness.frames(2);
    // 传送到 1 号宝石旁（字段公开，构造中间态）→ 收集帧
    {
        let mut state = harness.app.world.resource_mut::<AscentState>();
        state.pos = GEMS[1] + Vec3::new(0.2, 0.0, 0.0);
        state.vel = Vec3::ZERO;
        state.grounded = true;
    }
    harness.frame();
    {
        let state = harness.state();
        assert!(state.gems[1], "贴近的宝石应被收集");
        assert!(
            state
                .frame_events
                .iter()
                .any(|e| matches!(e, iris_ascent::AscentEvent::Gem { .. })),
            "收集帧应有 Gem 事件"
        );
    }
    // 火花 burst 已在 FX 槽位上出生（Update 内触发、PostUpdate 推进一帧）
    assert!(harness.live_particles() > 12, "宝石火花应有大量活粒子");
    // 声源入混音器（headless 纯混音器表断言）
    {
        let audio = harness.app.world.resource::<Audio>();
        assert!(audio.mixer().active_count() >= 1, "宝石音应已入混音器表");
    }
    // 宝石实体隐藏
    let ents = harness
        .app
        .world
        .try_resource::<iris_ascent::AscentEntities>()
        .expect("实体索引在位")
        .clone();
    assert_eq!(
        *harness.app.world.get::<Visibility>(ents.gems[1]).unwrap(),
        Visibility::HIDDEN,
        "已收集宝石应隐藏"
    );
    // 跳一次：起跳尘土 → 落地重尘土（冲量 ≈ 跳跃初速 ≥ 阈值）
    harness.tap_jump();
    harness.frame();
    assert!(harness.live_particles() > 0, "起跳应有尘土 burst");
    let mut landed = false;
    for _ in 0..120 {
        harness.frame();
        if harness
            .state()
            .frame_events
            .iter()
            .any(|e| matches!(e, iris_ascent::AscentEvent::Landed { .. }))
        {
            landed = true;
            break;
        }
    }
    assert!(landed, "跳跃落地应产生 Landed 事件（冲量过阈值）");
    assert!(harness.live_particles() > 0, "落地应有尘土 burst");
    // 2 s 后粒子自然熄灭（槽位可复用）
    harness.frames(130);
    assert_eq!(harness.live_particles(), 0, "尘土 / 火花应全部熄灭");
}

/// 坠落复活：位置精确回检查点、相机瞬移吸附（不做长距离甩镜）、HUD
/// 坠落计数与横幅同步。
#[test]
fn respawn_snaps_camera_and_updates_hud() {
    let mut harness = Harness::new();
    harness.frames(2);
    // 先激活检查点 1（传送到激活域）
    {
        let mut state = harness.app.world.resource_mut::<AscentState>();
        state.pos = CHECKPOINTS[0].zone_center;
        state.vel = Vec3::ZERO;
        state.grounded = true;
    }
    harness.frame();
    assert!(harness.state().checkpoints[0], "检查点 1 应已激活");
    // 记录相机平滑位（此刻应跟随在玩家旁）
    let rig_before = *harness
        .app
        .world
        .try_resource::<CameraRig>()
        .expect("CameraRig 在位");
    // 坠落：低于坠落线 → 复活
    {
        let mut state = harness.app.world.resource_mut::<AscentState>();
        state.pos.y = -30.0;
    }
    harness.frame();
    {
        let state = harness.state();
        assert_eq!(state.falls, 1, "应计一次坠落");
        let want = CHECKPOINTS[0].respawn_foot + Vec3::new(0.0, HALF_HEIGHT, 0.0);
        assert_eq!(state.pos, want, "复活位置应逐位等于检查点复活位");
    }
    // 相机吸附：rig 直接等于目标（玩家 + 固定偏移），无逐帧趋近的长距离
    // 甩镜（数值上 rig 与目标逐位一致即证明吸附）
    {
        let rig = *harness
            .app
            .world
            .try_resource::<CameraRig>()
            .expect("CameraRig 在位");
        let target = harness.state().pos + iris_ascent::CAMERA_OFFSET;
        assert_eq!(rig.pos, target, "复活帧相机应整体吸附到目标位");
        assert_ne!(
            rig.pos, rig_before.pos,
            "相机位应发生显著变化（长距离传送）"
        );
    }
    // HUD：坠落计数 + 横幅
    assert_eq!(harness.hud_text(FALLS_TEXT_NAME), "FALLS 1");
    assert_eq!(
        harness.hud_text("ascent/hud/banner"),
        "FELL - BACK TO CHECKPOINT",
        "坠落横幅应在计时窗口内显示"
    );
    // 2.5 s 后横幅清空（BannerTimer 递减）
    harness.frames(150);
    assert_eq!(harness.hud_text("ascent/hud/banner"), "");
    let timer = harness
        .app
        .world
        .try_resource::<BannerTimer>()
        .expect("BannerTimer 在位");
    assert_eq!(timer.0, 0.0, "横幅计时应归零");
}

/// LOD（R30）量化（记录性断言）：同一世界近景 / 拉远两帧——远景装饰
/// 全部切低模后 `triangles_drawn` 显著下降，且近景有视锥剔除。无 GPU
/// 环境自动跳过（与其余 GPU 集成测试同约定）。
#[test]
fn lod_triangles_drop_when_camera_pulls_far() {
    let Some(mut renderer) = OffscreenRenderer::new(320, 180) else {
        println!("无 GPU，跳过 LOD 记录性断言");
        return;
    };
    let mut harness = Harness::new();
    harness.frames(2);

    // 近景：第三人称机位原样
    renderer.render(&harness.app.world);
    let near = renderer.renderer().stats();

    // 拉远：把相机子实体挪到高空远望（局部位置相对玩家根，玩家在出生
    // 点附近 → 相机被抬到 ~160 m 外，全部装饰超出 60 m 切换距离）
    let ents = harness
        .app
        .world
        .try_resource::<iris_ascent::AscentEntities>()
        .expect("实体索引在位")
        .clone();
    let camera_local = *harness
        .app
        .world
        .get::<Transform>(ents.camera)
        .expect("相机应有 Transform");
    if let Some(mut transform) = harness.app.world.get_mut::<Transform>(ents.camera) {
        transform.position = Vec3::new(0.0, 90.0, 130.0);
    }
    iris_core::hierarchy::propagate_transforms(&mut harness.app.world);
    renderer.render(&harness.app.world);
    let far = renderer.renderer().stats();
    // 还原相机
    if let Some(mut transform) = harness.app.world.get_mut::<Transform>(ents.camera) {
        transform.position = camera_local.position;
    }

    println!(
        "LOD 对比：近景 triangles = {}（draw calls {}），远景 triangles = {}（draw calls {}）",
        near.triangles_drawn, near.draw_calls, far.triangles_drawn, far.draw_calls
    );
    assert!(
        near.culled_instances > 0,
        "近景应有视锥剔除（culled_instances > 0），实际 {}",
        near.culled_instances
    );
    assert!(
        far.triangles_drawn < near.triangles_drawn,
        "拉远后三角形数应下降（LOD 低模生效）：近 {} vs 远 {}",
        near.triangles_drawn,
        far.triangles_drawn
    );
    assert!(
        near.triangles_drawn > 0 && far.triangles_drawn > 0,
        "两帧都应有几何提交"
    );
}
