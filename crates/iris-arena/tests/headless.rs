//! headless 确定性集成测试：**不建窗口、不建 GPU、不依赖 winit**，复用
//! `ArenaPlugin` 的完整系统链（`build_schedule`，与 DefaultPlugins 同一
//! 编排）驱动真实游戏世界，验证引擎资源可脚本化接线——重点是 iris-game
//! 未覆盖的三件套：**蒙皮动画 × 对象池 × 多游戏状态**：
//!
//! - `Time`：`advance(Duration)` 确定性推进（首帧即有效 delta）；
//! - `Input`：`press(KeyCode)` / `release(KeyCode)` 注入按键（边沿键
//!   「冲刺 / 确认 / 重开」注入前先 release 强制产生 pressed 边沿）；
//! - `Audio`：预插入 `Audio::headless()`（纯混音器，绝不打开设备）；
//! - **动画**：`AnimationPlugin.build(&mut app)` 注册 `update_animations`
//!   （与窗口路径 `DefaultPlugins` 同一插件同一编排；ArenaPlugin 刻意
//!   不自带，避免窗口路径重复注册导致动画双倍速）；
//! - **场景**：Harness 把内联 JSON 写进临时目录的 fixture 关卡（±24
//!   围墙 + 两根障碍柱，与正式关卡同一几何约定），经 `ArenaPlugin::new`
//!   注入——全部测试跑在真实的场景加载路径上；
//! - 层级传播：`HierarchyPlugin` 的 PostUpdate 阶段，每帧时序镜像
//!   `App::tick`：advance → Update → PostUpdate → clear_events →
//!   input.end_frame。

use std::path::PathBuf;
use std::time::Duration;

use iris_arena::{
    ArenaPlugin, ArenaState, BANNER_TEXT_NAME, COMBO_TEXT_NAME, EnemyPool, HP_FRONT_NAME, Phase,
    ProjectilePool, SCORE_TEXT_NAME, WAVE_TEXT_NAME,
};
use iris_core::prelude::Stage;
use iris_core::resource::{ResourceRef, ResourceRefMut};
use iris_engine::prelude::{
    AnimationPlayer, AnimationPlugin, App, Audio, Camera, DirectionalLight, GlobalTransform,
    HierarchyPlugin, Input, KeyCode, MaterialStore, MeshStore, OffscreenRenderer, Plugin,
    TextureStore, Time, Transform, UiRect, UiText, Visibility,
};

/// 标准容差：1/60 s 步长下的浮点误差量级。
const EPS: f32 = 1e-3;

/// 最小 fixture 关卡：与正式关卡（`assets/arena_battle.scene.json`）同一
/// 几何约定——四面围墙的静态 Box collider 内缘 ±24（玩家球体半径 0.45 →
/// 贴墙位置 23.55），外加两根障碍柱。地面省略 collider（俯视玩法垂直面
/// 无碰撞，见 iris-arena::logic 模块文档）。无需环境段（Harness 已预插
/// 默认方向光）。
const FIXTURE_SCENE: &str = r#"{
    "entities": [
        {
            "name": "wall-east",
            "transform": { "position": [24.5, 0.6, 0.0], "scale": [1.0, 1.2, 50.0] },
            "mesh": { "primitive": { "kind": "cube" } },
            "collider": { "shape": { "box": { "half_extents": [0.5, 0.6, 25.0] } }, "static": true }
        },
        {
            "name": "wall-west",
            "transform": { "position": [-24.5, 0.6, 0.0], "scale": [1.0, 1.2, 50.0] },
            "mesh": { "primitive": { "kind": "cube" } },
            "collider": { "shape": { "box": { "half_extents": [0.5, 0.6, 25.0] } }, "static": true }
        },
        {
            "name": "wall-north",
            "transform": { "position": [0.0, 0.6, -24.5], "scale": [50.0, 1.2, 1.0] },
            "mesh": { "primitive": { "kind": "cube" } },
            "collider": { "shape": { "box": { "half_extents": [25.0, 0.6, 0.5] } }, "static": true }
        },
        {
            "name": "wall-south",
            "transform": { "position": [0.0, 0.6, 24.5], "scale": [50.0, 1.2, 1.0] },
            "mesh": { "primitive": { "kind": "cube" } },
            "collider": { "shape": { "box": { "half_extents": [25.0, 0.6, 0.5] } }, "static": true }
        },
        {
            "name": "pillar-a",
            "transform": { "position": [8.0, 1.3, 6.0] },
            "mesh": { "primitive": { "kind": "cylinder", "radius": 0.7, "height": 2.6, "segments": 24 } },
            "collider": { "shape": { "box": { "half_extents": [0.7, 1.3, 0.7] } }, "static": true }
        },
        {
            "name": "pillar-b",
            "transform": { "position": [-9.0, 1.3, -7.0] },
            "mesh": { "primitive": { "kind": "cylinder", "radius": 0.7, "height": 2.6, "segments": 24 } },
            "collider": { "shape": { "box": { "half_extents": [0.7, 1.3, 0.7] } }, "static": true }
        }
    ]
}"#;

/// 把 fixture 关卡写进临时目录（内容恒定，`Once` 防并行测试重复写），
/// 返回路径供 `ArenaPlugin::new` 注入。
fn fixture_scene_path() -> PathBuf {
    static WRITE_ONCE: std::sync::Once = std::sync::Once::new();
    let dir = std::env::temp_dir().join("iris-arena-headless");
    std::fs::create_dir_all(&dir).expect("创建临时目录");
    let path = dir.join("fixture.scene.json");
    WRITE_ONCE.call_once(|| {
        std::fs::write(&path, FIXTURE_SCENE).expect("写 fixture 场景文件");
    });
    path
}

/// headless 测试台：世界 + ArenaPlugin 系统链 + 动画 / 层级插件 + 虚拟时钟。
struct Harness {
    app: App,
}

impl Harness {
    fn new() -> Self {
        let mut app = App::new();
        // DefaultPlugins 等价的资源（不建窗口 / 不建 GPU）；音频强制
        // headless（纯混音器，绝不打开设备）；ArenaPlugin 的
        // setup_resources 对已存在资源一律不覆盖
        app.world.insert_resource(Time::default());
        app.world.insert_resource(Input::new());
        app.world.insert_resource(MeshStore::new());
        app.world.insert_resource(MaterialStore::new());
        app.world.insert_resource(TextureStore::new());
        app.world.insert_resource(Camera::default());
        app.world.insert_resource(DirectionalLight::default());
        app.world.insert_resource(Audio::headless());

        // 层级传播（PostUpdate，HierarchyPlugin）+ 动画推进（Update，
        // AnimationPlugin——已列入 iris-engine prelude，headless 直接
        // build，与窗口路径的 DefaultPlugins 同一插件同一编排；
        // ArenaPlugin 刻意不自带动画注册：窗口路径 DefaultPlugins 已注册，
        // 重复注册会让动画双倍速）。注册顺序 = DefaultPlugins 在前游戏
        // 插件在后（动画先于游戏系统推进）。
        HierarchyPlugin.build(&mut app);
        AnimationPlugin.build(&mut app);
        // 场景：临时 fixture 关卡；敌人模型走 Default 的 asset_path 解析
        // （cargo test 的 cwd 是 crate 目录，CWD 候选未命中回退 workspace
        // 根——资产路径摩擦闭合后无须再注入 CARGO_MANIFEST_DIR 绝对路径）
        ArenaPlugin {
            scene_path: fixture_scene_path(),
            ..Default::default()
        }
        .build_schedule(&mut app.schedule);

        let mut harness = Self { app };
        // Startup：资源兜底插入 + 场景搭建 + 双池预热（与 App 主循环一致）
        harness
            .app
            .schedule
            .run(Stage::Startup, &mut harness.app.world);
        harness
    }

    /// 推进一帧：确定性 1/60 s。`held` 为本帧按住的键（重复注入由 Input
    /// 去重，只有首帧产生 pressed 边沿）；`taps` 为边沿键（先强制释放再
    /// 按下，保证本帧产生 pressed——冲刺 / 确认 / 重开都是边沿语义）。
    fn frame(&mut self, held: &[KeyCode], taps: &[KeyCode]) {
        self.app
            .world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(1.0 / 60.0));
        {
            let mut input = self.app.world.resource_mut::<Input>();
            for &key in taps {
                input.release(key);
            }
            for &key in held {
                input.press(key); // 已按住会被去重：不产生新边沿
            }
            for &key in taps {
                input.press(key); // 释放后重按 = 强制新边沿
            }
        }
        self.app.schedule.run(Stage::Update, &mut self.app.world);
        self.app
            .schedule
            .run(Stage::PostUpdate, &mut self.app.world);
        self.app.world.clear_all_events();
        self.app.world.resource_mut::<Input>().end_frame();
    }

    /// 连续推进多帧（无按键）。
    fn frames(&mut self, count: usize) {
        for _ in 0..count {
            self.frame(&[], &[]);
        }
    }

    /// 任意确认键开局（Title → Playing）。
    fn start(&mut self) {
        self.frame(&[], &[KeyCode::KeyW]);
        assert_eq!(self.game().phase, Phase::Playing, "确认键应直接开局");
    }

    fn game(&self) -> ResourceRef<'_, ArenaState> {
        self.app
            .world
            .try_resource::<ArenaState>()
            .expect("ArenaState 应已由 Startup 插入")
    }

    fn game_mut(&mut self) -> ResourceRefMut<'_, ArenaState> {
        self.app.world.resource_mut::<ArenaState>()
    }

    fn enemy_pool(&self) -> ResourceRef<'_, EnemyPool> {
        self.app
            .world
            .try_resource::<EnemyPool>()
            .expect("敌人池应已由 Startup 插入")
    }

    fn projectile_pool(&self) -> ResourceRef<'_, ProjectilePool> {
        self.app
            .world
            .try_resource::<ProjectilePool>()
            .expect("弹道池应已由 Startup 插入")
    }

    /// 按名读取 HUD 文本（Name + entity_named 定位）。
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

    /// 敌人根实体的 AnimationPlayer（active 槽位第一个）。
    fn first_enemy_animation(&self) -> (f32, bool, usize) {
        let (id, _) = *self
            .enemy_pool()
            .active()
            .first()
            .expect("应有活跃敌人槽位");
        let entity = self.enemy_pool().entity_of(id).expect("活跃 id 应有槽位");
        let player = self
            .app
            .world
            .get::<AnimationPlayer>(entity)
            .expect("敌人槽位根实体应挂 AnimationPlayer");
        (player.time, player.playing, player.clips.len())
    }
}

/// Startup 后的初始状态：双池预热完毕、标题画面、HUD 就位。
#[test]
fn startup_prewarms_pools_and_shows_title() {
    let harness = Harness::new();
    {
        let game = harness.game();
        assert_eq!(game.phase, Phase::Title);
        assert_eq!(game.wave, 0);
        assert!(game.enemies.is_empty() && game.projectiles.is_empty());
    }
    // 敌人池：15 个蒙皮槽位全部隐藏待命（零运行期 spawn 的前提）
    {
        let pool = harness.enemy_pool();
        assert_eq!(pool.slots.len(), 15, "敌人池应预热 15 个槽位");
        assert_eq!(pool.free_count(), 15);
        assert!(pool.active().is_empty());
        for &slot in &pool.slots {
            assert_eq!(
                *harness
                    .app
                    .world
                    .get::<Visibility>(slot)
                    .expect("槽位应有 Visibility"),
                Visibility::HIDDEN,
                "待命槽位应隐藏"
            );
            // 蒙皮 × 动画：槽位根实体挂播放器（剪辑来自 GLB，自动循环）
            let player = harness
                .app
                .world
                .get::<AnimationPlayer>(slot)
                .expect("蒙皮敌人槽位应挂 AnimationPlayer");
            assert_eq!(player.clips.len(), 1, "GLB 内嵌 1 条扭动剪辑");
            assert!(player.playing, "挂载即自动播放");
            assert!(player.looping);
        }
    }
    // 弹道池：40 个自发光小球槽位全部隐藏待命
    {
        let pool = harness.projectile_pool();
        assert_eq!(pool.slots.len(), 40, "弹道池应预热 40 个槽位");
        assert_eq!(pool.free_count(), 40);
    }
    // HUD 实体按名可定位，标题横幅在位
    assert_eq!(harness.hud_text(BANNER_TEXT_NAME), "PRESS ANY KEY TO START");
    assert_eq!(harness.hud_text(WAVE_TEXT_NAME), "WAVE -");
    assert_eq!(harness.hud_text(SCORE_TEXT_NAME), "SCORE 0");
}

/// 任意键开局：第 1 波生成 → 池槽位取用 + 可见性翻开 + HUD 切换。
#[test]
fn any_key_starts_wave_one_and_acquires_pool_slots() {
    let mut harness = Harness::new();
    harness.start();
    {
        let game = harness.game();
        assert_eq!(game.wave, 1);
        assert_eq!(game.enemies.len(), 5, "第 1 波应为 5 只");
    }
    let pool = harness.enemy_pool();
    assert_eq!(pool.active().len(), 5, "5 只敌人应各占一个槽位");
    assert_eq!(pool.free_count(), 10);
    let wave1_slots: Vec<_> = pool.active().iter().map(|(_, e)| *e).collect();
    drop(pool);
    for &slot in &wave1_slots {
        assert_eq!(
            *harness
                .app
                .world
                .get::<Visibility>(slot)
                .expect("槽位应有 Visibility"),
            Visibility::VISIBLE,
            "在场的敌人槽位应可见"
        );
        // 敌人根实体已被表现系统挪出待命点（跟随逻辑位置）
        let position = harness
            .app
            .world
            .get::<Transform>(slot)
            .expect("槽位应有 Transform")
            .position;
        assert!(
            position.x.abs() < 24.0 && position.z.abs() < 24.0,
            "敌人应在场内：{position:?}"
        );
    }
    // HUD：波次与横幅切换
    assert_eq!(harness.hud_text(WAVE_TEXT_NAME), "WAVE 1");
    assert_eq!(harness.hud_text(BANNER_TEXT_NAME), "", "对局中横幅应为空");
}

/// 敌人池跨波复用：清空第 1 波 → 间歇 → 第 2 波**复用同一批槽位**，
/// 全程池不超发、退场槽位翻回隐藏。
#[test]
fn enemy_pool_reuses_slots_across_waves_without_overspawning() {
    let mut harness = Harness::new();
    harness.start();
    let wave1_slots: Vec<_> = harness
        .enemy_pool()
        .active()
        .iter()
        .map(|(_, e)| *e)
        .collect();
    // 直接清空第 1 波（跳过战斗），推进间歇全程进第 2 波
    harness.game_mut().enemies.clear();
    harness.frame(&[], &[]);
    {
        let pool = harness.enemy_pool();
        assert!(pool.active().is_empty(), "清空当帧全部槽位应归还");
        assert_eq!(pool.free_count(), 15, "归还后池应满");
    }
    // 退场槽位翻回隐藏（下一波登场前保持待命）
    for &slot in &wave1_slots {
        assert_eq!(
            *harness.app.world.get::<Visibility>(slot).unwrap(),
            Visibility::HIDDEN,
            "退场槽位应隐藏待命"
        );
    }
    // 间歇 3 s（180 帧）+ 余量 → 第 2 波自动开始（7 只）
    harness.frames(200);
    {
        let game = harness.game();
        assert_eq!(game.wave, 2, "间歇结束应进第 2 波");
        assert_eq!(game.enemies.len(), 7);
    }
    let pool = harness.enemy_pool();
    assert_eq!(pool.active().len(), 7);
    assert_eq!(pool.free_count(), 8);
    // 复用断言：第 2 波的 7 个槽位里有 5 个是第 1 波原班（栈式闲置表，
    // 先归还的先取回——零新 spawn 的直接证据）
    let reused = pool
        .active()
        .iter()
        .filter(|(_, e)| wave1_slots.contains(e))
        .count();
    assert_eq!(reused, 5, "第 2 波应复用第 1 波的全部 5 个槽位");
    // 全程不超发：活跃数从未超过池容量（以终态 + 每波断言为代表）
    assert!(pool.active().len() <= 15);
}

/// HUD 随状态刷新：SCORE / COMBO / WAVE / HP 条宽度 / 横幅三态。
#[test]
fn hud_tracks_score_combo_hp_and_banner_states() {
    let mut harness = Harness::new();
    harness.start();
    // 直接构造中间态（字段公开，同 logic 单测约定）：得分 / 连击 / 掉血
    // （连击窗口须同时给满，否则当帧衰减即清零）
    {
        let mut game = harness.game_mut();
        game.score = 375;
        game.combo = 4;
        game.combo_timer = 3.0;
        game.player.hp = 3;
    }
    harness.frame(&[], &[]);
    assert_eq!(harness.hud_text(SCORE_TEXT_NAME), "SCORE 375");
    assert_eq!(harness.hud_text(COMBO_TEXT_NAME), "COMBO x4");
    // HP 前景条宽度 = hp/max × 200
    let hp_entity = harness
        .app
        .world
        .entity_named(HP_FRONT_NAME)
        .expect("HP 条存在");
    let hp_width = harness
        .app
        .world
        .get::<UiRect>(hp_entity)
        .expect("HP 条应有 UiRect")
        .size[0];
    assert!(
        (hp_width - 120.0).abs() < EPS,
        "hp 3/5 → 条宽 120，实际 {hp_width}"
    );
    // 开波倒计时横幅：清空 → 间歇当帧显示下一波倒计时
    harness.game_mut().enemies.clear();
    harness.frame(&[], &[]);
    assert_eq!(harness.hud_text(BANNER_TEXT_NAME), "WAVE 2 IN 3...");
    // 连击清零后 COMBO 文本消失
    harness.game_mut().combo = 0;
    harness.frame(&[], &[]);
    assert_eq!(harness.hud_text(COMBO_TEXT_NAME), "");
    // GAME OVER 横幅 + HP 条清零
    {
        let mut game = harness.game_mut();
        game.phase = Phase::GameOver;
        game.player.hp = 0;
    }
    harness.frame(&[], &[]);
    assert_eq!(
        harness.hud_text(BANNER_TEXT_NAME),
        "GAME OVER - PRESS R TO RESTART"
    );
    let hp_width = harness.app.world.get::<UiRect>(hp_entity).unwrap().size[0];
    assert!(hp_width.abs() < EPS, "hp 0 → 条宽 0，实际 {hp_width}");
}

/// 蒙皮动画在池槽位上真实推进：`update_animations`（AnimationPlugin）
/// 每帧推进扭动剪辑，敌人槽位的播放时间随帧增长。
#[test]
fn skinned_animation_plays_and_advances_on_pool_slots() {
    let mut harness = Harness::new();
    harness.start();
    let (time_a, playing, clips) = harness.first_enemy_animation();
    assert!(playing, "挂载即自动播放");
    assert_eq!(clips, 1);
    // 30 帧（0.5 s）后播放时间应推进 0.5 s（1.2 s 循环回绕内）
    harness.frames(30);
    let (time_b, playing_b, _) = harness.first_enemy_animation();
    assert!(playing_b);
    let expected = (time_a + 0.5) % 1.2;
    assert!(
        (time_b - expected).abs() < 1e-3,
        "0.5 s 后播放时间应为 ({time_a} + 0.5) mod 1.2 = {expected}，实际 {time_b}"
    );
    // 换波（清空 + 间歇 + 第 2 波）后新取用的槽位同样在播（池复用 × 动画）
    harness.game_mut().enemies.clear();
    harness.frames(200);
    let (_, playing_c, _) = harness.first_enemy_animation();
    assert!(playing_c, "第 2 波复用的槽位应继续自动播放");
}

/// 蒙皮实例收编的端到端证据（引擎 API 打磨 II 闭合①）：加载器已把兄弟
/// 顶层关节树收编为槽位根实体的子树——逻辑侧移动敌人 → 表现系统写根
/// 实体 Transform → 层级传播当帧把 primitive 部分与关节子树一起带走。
/// 收编缺失时（旧引擎缺陷）关节全局恒在世界原点，根移动后蒙皮角色会被
/// 关节矩阵拉向反方向。
#[test]
fn enemy_root_translation_carries_joint_subtree() {
    let mut harness = Harness::new();
    harness.start();
    let (id, slot) = *harness
        .enemy_pool()
        .active()
        .first()
        .expect("应有活跃敌人槽位");
    // 收编层级：槽位根实体 = mesh 节点，子实体应含 primitive 部分与
    // 关节树根（蠕虫 GLB：1 个 primitive + hip）
    let children = harness.app.world.children(slot);
    assert!(
        children.len() >= 2,
        "槽位根应有 primitive 部分 + 收编的关节树根，实际 {} 个子实体",
        children.len()
    );
    assert!(
        children
            .iter()
            .all(|&child| harness.app.world.parent(child) == Some(slot)),
        "全部子实体的父级都应是槽位根实体"
    );

    // 逻辑侧把敌人挪到远处：表现系统写根实体，层级传播当帧合成子树全局
    let target = glam::Vec2::new(10.0, -8.0);
    {
        let mut game = harness.game_mut();
        let enemy = game
            .enemies
            .iter_mut()
            .find(|enemy| enemy.id == id)
            .expect("敌人应在场");
        enemy.pos = target;
    }
    harness.frame(&[], &[]);
    let root_position = harness
        .app
        .world
        .get::<Transform>(slot)
        .expect("槽位根应有 Transform")
        .position;
    assert!(
        (root_position.x - target.x).abs() < 0.1 && (root_position.z - target.y).abs() < 0.1,
        "敌人根应已被表现系统挪到目标附近：{root_position:?} vs {target:?}"
    );
    // 关键断言：全部子实体（primitive 部分 + 关节）的世界位置跟随根——
    // 子实体局部均为恒等，全局位置应与根一致
    for &child in &children {
        let global = harness
            .app
            .world
            .get::<GlobalTransform>(child)
            .expect("子实体应有 GlobalTransform")
            .position();
        assert!(
            (global.x - root_position.x).abs() < EPS && (global.z - root_position.z).abs() < EPS,
            "子实体全局位置应跟随根移动：{global:?} vs 根 {root_position:?}"
        );
    }
}

/// 弹道池封顶：逻辑侧 40 上限 ↔ 池 40 槽位一一对应，池满不开火、
/// 弹道消亡后槽位归还，全程不超发。
#[test]
fn projectile_pool_is_bounded_and_slots_recycle() {
    let mut harness = Harness::new();
    harness.start();
    // 塞满 40 条弹道（id 与逻辑分配器错开）+ 射程内敌人 → 池满不开火
    {
        let mut game = harness.game_mut();
        game.projectiles.clear();
        for i in 0..40u32 {
            game.projectiles.push(iris_arena::ProjectileState {
                id: 9000 + i,
                pos: glam::Vec2::new(i as f32 * 0.1 - 2.0, 15.0),
                vel: glam::Vec2::new(0.0, -0.5),
                life: 0.4, // 短命：24 帧后开始归还槽位
            });
        }
    }
    harness.frame(&[], &[]);
    {
        let pool = harness.projectile_pool();
        assert_eq!(pool.active().len(), 40, "40 条弹道应各占一个槽位");
        assert_eq!(pool.free_count(), 0, "池应被顶满");
    }
    assert_eq!(harness.game().total_shots, 0, "池满时逻辑侧不应开火");
    // 短命弹道陆续耗尽：槽位归还、活跃数回落
    harness.frames(40);
    {
        let pool = harness.projectile_pool();
        assert!(
            pool.free_count() >= 38,
            "短命弹道耗尽后槽位应归还，实际闲置 {}",
            pool.free_count()
        );
    }
    // 混战不变量：8 s 混战（站桩开火 + 击杀 + 换波）全程 弹道槽位 ≤ 40
    harness.frames(480);
    {
        let game = harness.game();
        let pool = harness.projectile_pool();
        assert!(game.projectiles.len() <= 40);
        assert!(pool.active().len() <= 40);
        assert_eq!(
            pool.active().len() + pool.free_count(),
            40,
            "活跃 + 闲置应恒等于池容量（不超发、不泄漏）"
        );
    }
}

/// GAME OVER → R 直接重开：阶段 / 计分 / 波次复位，旧敌人槽位全部归还、
/// 新波取用（多游戏状态 × 池的完整闭环）。
#[test]
fn game_over_and_restart_recycle_everything() {
    let mut harness = Harness::new();
    harness.start();
    harness.frames(30); // 混战 0.5 s，留下一堆中间态
    // 直接构造终局：hp 1 + 敌人贴脸（玩家挪回原点：start 的 W 仍按住，
    // 前 30 帧已让玩家跑出 3 m——贴脸接触需要在接触半径内）
    {
        let mut game = harness.game_mut();
        game.player.hp = 1;
        game.player.invuln = 0.0;
        game.player.pos = glam::Vec2::ZERO;
        game.enemies.clear();
        game.enemies.push(iris_arena::EnemyState {
            id: 777,
            pos: glam::Vec2::new(0.3, 0.0),
            hp: 999,
            hit_flash: 0.0,
            kb: glam::Vec2::ZERO,
        });
    }
    harness.frame(&[], &[]);
    {
        let game = harness.game();
        assert_eq!(game.phase, Phase::GameOver);
        assert_eq!(game.player.hp, 0);
        assert_eq!(game.score, 0, "本脚本未击杀任何敌人（出生环在射程外）");
    }
    // 终局时逻辑侧贴脸敌人仍在场（死亡当帧冻结，不做清理）→ 槽位仍活跃
    assert_eq!(
        harness.enemy_pool().active().len(),
        1,
        "贴脸敌人的槽位保留到重开"
    );
    // R 直接重开（实现取舍：不回 Title，见 ArenaState::restart 文档）
    harness.frame(&[], &[KeyCode::KeyR]);
    {
        let game = harness.game();
        assert_eq!(game.phase, Phase::Playing, "R 应直接重开对局");
        assert_eq!(game.wave, 1);
        assert_eq!(game.score, 0);
        assert_eq!(game.player.hp, 5, "重开应满血");
        assert_eq!(game.enemies.len(), 5, "重开应重生成第 1 波");
        assert_eq!(game.total_damage, 0, "事件计数应复位");
    }
    let pool = harness.enemy_pool();
    assert_eq!(pool.active().len(), 5, "新波应重新取用槽位");
    assert_eq!(pool.free_count(), 10);
    assert!(
        pool.active().iter().all(|(id, _)| *id != 777),
        "旧对局的敌人 id 应全部释放（id 不复用）"
    );
}

/// 空间音频 dogfooding：听者每帧跟随场景相机（俯视机位 → 听者悬于玩家
/// 上空 20 m、后方 12 m），开火 / 命中音经 `play_spatial` 入混音器表。
/// headless 断言的是槽位状态（听者位姿 + 活跃声音数）而非实听；headless
/// 不驱动混音 → 非循环声不会自然播完，留在表内可查。
#[test]
fn spatial_listener_follows_camera_and_sounds_enter_mixer() {
    let mut harness = Harness::new();
    harness.start();
    // 拉一只敌人到玩家 2 m 处：当帧即进入射程 → 自动开火（并可能命中）
    {
        let mut game = harness.game_mut();
        game.enemies[0].pos = game.player.pos + glam::Vec2::new(2.0, 0.0);
    }
    // 第 1 帧：开火（此时尚无相机 GlobalTransform，听者保持默认）+
    // PostUpdate 层级传播出相机全局位姿；第 2 帧：run_arena 取到位姿 →
    // set_listener 跟随（位姿取传播时刻的玩家位置——滞后一帧，正好以此
    // 验证「读上一帧全局位姿」的编排）
    harness.frame(&[], &[]);
    let player_propagated = harness.game().player.pos;
    harness.frame(&[], &[]);
    let audio = harness.app.world.resource::<Audio>();
    let listener = audio.mixer().listener();
    assert!(
        (listener.position.y - 20.0).abs() < EPS,
        "听者应悬在相机高度（20 m）：{:?}",
        listener.position
    );
    assert!(
        (listener.position.x - player_propagated.x).abs() < 1e-2
            && (listener.position.z - (player_propagated.y + 12.0)).abs() < 1e-2,
        "听者应在玩家上空、后方 12 m（相机子实体偏移）：{:?} vs 玩家 {player_propagated:?}",
        listener.position
    );
    assert!(
        audio.mixer().active_count() >= 1,
        "交火中的开火 / 命中音应已入混音器表（空间化播放）"
    );
}

/// 混战负载下的 draw calls（记录性断言）：13 敌 + 20 弹在场的离屏一帧
/// 保持在 300 以内。无 GPU 环境自动跳过（与其余 GPU 集成测试同约定）。
#[test]
fn draw_calls_stay_under_300_in_melee() {
    let Some(mut renderer) = OffscreenRenderer::new(320, 180) else {
        println!("无 GPU，跳过 draw calls 记录性断言");
        return;
    };
    let mut harness = Harness::new();
    harness.start();
    // 直接构造混战：13 只敌人围一圈 + 20 条在飞弹道（字段公开，同 logic 单测）
    {
        let mut game = harness.game_mut();
        game.enemies.clear();
        for i in 0..13u32 {
            let angle = i as f32 * std::f32::consts::TAU / 13.0;
            game.enemies.push(iris_arena::EnemyState {
                id: 600 + i,
                pos: glam::Vec2::new(9.0 * angle.cos(), 9.0 * angle.sin()),
                hp: 3,
                hit_flash: 0.0,
                kb: glam::Vec2::ZERO,
            });
        }
        game.projectiles.clear();
        for i in 0..20u32 {
            let angle = i as f32 * std::f32::consts::TAU / 20.0;
            game.projectiles.push(iris_arena::ProjectileState {
                id: 700 + i,
                pos: glam::Vec2::new(4.0 * angle.cos(), 4.0 * angle.sin()),
                vel: glam::Vec2::new(angle.cos(), angle.sin()) * 5.0,
                life: 1.0,
            });
        }
    }
    // 一帧表现追平（池取用 / 可见性 / 变换 / 层级传播）后离屏渲染；
    // 关掉自动开火（射程内有敌人，否则当帧会多打一发）
    harness.game_mut().player.fire_timer = 60.0;
    harness.frame(&[], &[]);
    {
        let pool = harness.enemy_pool();
        assert_eq!(pool.active().len(), 13, "13 只敌人应各占一个槽位");
    }
    assert_eq!(harness.projectile_pool().active().len(), 20);
    renderer.render(&harness.app.world);
    let stats = renderer.renderer().stats();
    println!(
        "混战一帧：draw_calls = {}，instances = {}（断言上限 300）",
        stats.draw_calls, stats.instances
    );
    assert!(
        stats.draw_calls < 300,
        "13 敌 + 20 弹 + 场景 + HUD 的 draw calls 应 < 300，实际 {}",
        stats.draw_calls
    );
}
