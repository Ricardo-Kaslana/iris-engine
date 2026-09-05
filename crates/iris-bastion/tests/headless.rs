//! headless 确定性集成测试：**不建窗口、不建 GPU、不依赖 winit**，复用
//! `BastionPlugin` 的完整系统链（`build_schedule`，与 DefaultPlugins 同一
//! 编排）驱动真实游戏世界。与 iris-arena 的 headless 同款约定，重点覆盖
//! 本轮新载荷的真实接线：
//!
//! - **鼠标拾取（R21）**：`Input::on_cursor_moved` 注入光标像素 →
//!   `world_to_screen` 的逆路径（引擎 `raycast` 打地面 collider）→ 网格；
//!   左键经 `Input::on_mouse_button` 注入边沿，放塔全链路走真实输入；
//! - **幽灵预览（R23）**：可见性 / 红蓝材质换挂 / 射程盘缩放 / 塔种切换；
//! - **粒子（R24）**：击杀爆炸 / 枪口闪光在 FX 池槽位上真实触发
//!   （`ParticlePlugin.build` 注册与窗口路径同一编排）；
//! - **空间音频（R25）**：事件定位声源入混音器 + 听者跟随相机；
//! - **脚本化自动玩家**：预设放塔序列经鼠标点击全链路放置，跑完 5 波
//!   **必须获胜且核心剩余 HP ≥ 3**——用真实模拟验证平衡与索敌/经济
//!   闭环可玩。
//!
//! `Time::advance` 确定性推进、`Audio::headless()` 纯混音器（绝不打开
//! 设备）、层级传播 `HierarchyPlugin`（PostUpdate）每帧时序镜像
//! `App::tick`：advance → Update → PostUpdate → clear_events →
//! input.end_frame。

use std::time::Duration;

use glam::IVec2;

use iris_bastion::{
    BANNER_TEXT_NAME, BastionPlugin, BastionState, CORE_TEXT_NAME, CREDITS_TEXT_NAME, EnemyKind,
    POOL_BULLETS, POOL_ENEMIES, POOL_FX, POOL_SHELLS, POOL_TOWERS, Phase, TowerKind, cell_center,
    world_to_screen,
};
use iris_core::prelude::Stage;
use iris_engine::prelude::{
    App, Audio, Camera, DirectionalLight, ElementState, HierarchyPlugin, Input, KeyCode,
    MaterialHandle, MaterialStore, MeshStore, MouseButton, ParticleEmitter, ParticlePlugin, Plugin,
    Time, UiText, Visibility,
};

/// 标准容差：1/60 s 步长下的浮点误差量级。
const EPS: f32 = 1e-3;

/// 脚本化自动玩家的建造清单（放塔顺序：攒够造价就点下一座）。格位均
/// 压在航点弯口 / 长段旁的性价比点位——前两座哨兵开局即放（初始积分恰
/// 好够两座），臼炮随击杀收入逐步跟进。
const BUILD_ORDER: [(IVec2, TowerKind); 8] = [
    (IVec2::new(2, 3), TowerKind::Sentry),  // 西段首塔（z=-5 段）
    (IVec2::new(6, 5), TowerKind::Sentry),  // 长纵段（x=-5）
    (IVec2::new(11, 4), TowerKind::Mortar), // 双弯内口（x=0 / z=-4）
    (IVec2::new(16, 7), TowerKind::Mortar), // 东段（x=5 / 核心前）
    (IVec2::new(11, 7), TowerKind::Sentry), // 中段补充（x=0 / z=2）
    (IVec2::new(6, 8), TowerKind::Mortar),  // 北段弯口（z=2）
    (IVec2::new(17, 8), TowerKind::Sentry), // 核心门前
    (IVec2::new(16, 3), TowerKind::Sentry), // 东段补充
];

/// headless 测试台：世界 + BastionPlugin 系统链 + 层级 / 粒子插件 +
/// 虚拟时钟。
struct Harness {
    app: App,
}

impl Harness {
    fn new() -> Self {
        let mut app = App::new();
        // DefaultPlugins 等价的资源（不建窗口 / 不建 GPU）；音频强制
        // headless（纯混音器，绝不打开设备）；BastionPlugin 的
        // setup_resources 对已存在资源一律不覆盖
        app.world.insert_resource(Time::default());
        app.world.insert_resource(Input::new());
        app.world.insert_resource(MeshStore::new());
        app.world.insert_resource(MaterialStore::new());
        app.world.insert_resource(Camera::default());
        // 与 arena_battle.scene.json 同款的柔和日光（缺省灯 intensity 1.0
        // 会把中亮色材质洗白，快照 / 游戏画面统一用 0.55）
        app.world.insert_resource(DirectionalLight {
            direction: glam::Vec3::new(-0.45, -1.0, -0.35),
            color: glam::Vec3::new(0.95, 0.93, 0.9),
            intensity: 0.55,
            ambient: glam::Vec3::new(0.12, 0.12, 0.14),
            ..DirectionalLight::default()
        });
        app.world.insert_resource(Audio::headless());

        // 层级传播（PostUpdate）+ 粒子推进（PostUpdate、层级之后）——
        // 与窗口路径 DefaultPlugins 同一插件同一编排；BastionPlugin
        // 刻意不自带（重复注册会让粒子双倍推进）
        HierarchyPlugin.build(&mut app);
        ParticlePlugin.build(&mut app);
        // 关卡全部代码生成：无场景文件可注入
        BastionPlugin.build_schedule(&mut app.schedule);

        let mut harness = Self { app };
        // Startup：资源兜底插入 + 关卡搭建 + 四池预热（与 App 主循环一致）
        harness
            .app
            .schedule
            .run(Stage::Startup, &mut harness.app.world);
        // 立即传播一次：相机 GlobalTransform 就位（窗口路径首帧 PostUpdate
        // 亦如此），拾取从第一帧起可用
        iris_core::hierarchy::propagate_transforms(&mut harness.app.world);
        harness
    }

    /// 推进一帧：确定性 1/60 s。
    fn frame(&mut self) {
        // 帧首事件轮换（镜像 App::tick 的新生命周期：上帧事件降级为仍可读的
        // 上一代、更早一代丢弃——打磨 V）
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

    /// 连续推进多帧。
    fn frames(&mut self, count: usize) {
        for _ in 0..count {
            self.frame();
        }
    }

    /// 注入光标像素位置（`Input::on_cursor_moved` 公开 API）。
    fn move_cursor(&mut self, px: f64, py: f64) {
        self.app
            .world
            .resource_mut::<Input>()
            .on_cursor_moved((px, py));
    }

    /// 光标挪到目标格中心（只挪不点；幽灵预览跟随断言用）。
    fn move_cursor_to_cell(&mut self, cell: IVec2) {
        let center = cell_center(cell);
        let (px, py) = world_to_screen(&self.app.world, glam::Vec3::new(center.x, 0.0, center.y))
            .expect("场内格应可投影到屏幕");
        self.move_cursor(px, py);
    }

    /// 注入左键点击边沿（先强制释放再按下，保证本帧产生 pressed）。
    fn click_left(&mut self) {
        let mut input = self.app.world.resource_mut::<Input>();
        input.on_mouse_button(MouseButton::Left, ElementState::Released);
        input.on_mouse_button(MouseButton::Left, ElementState::Pressed);
    }

    /// 注入右键点击边沿（塔种切换）。
    fn click_right(&mut self) {
        let mut input = self.app.world.resource_mut::<Input>();
        input.on_mouse_button(MouseButton::Right, ElementState::Released);
        input.on_mouse_button(MouseButton::Right, ElementState::Pressed);
    }

    /// 把光标挪到目标格中心并左键点击（走完整的拾取 → 校验 → 放塔链路）。
    fn click_cell(&mut self, cell: IVec2) {
        let center = cell_center(cell);
        let (px, py) = world_to_screen(&self.app.world, glam::Vec3::new(center.x, 0.0, center.y))
            .expect("场内格应可投影到屏幕");
        self.move_cursor(px, py);
        self.click_left();
    }

    fn tap(&mut self, key: KeyCode) {
        let mut input = self.app.world.resource_mut::<Input>();
        input.release(key);
        input.press(key);
    }

    fn game(&self) -> iris_core::resource::ResourceRef<'_, BastionState> {
        self.app
            .world
            .try_resource::<BastionState>()
            .expect("BastionState 应已由 Startup 插入")
    }

    fn enemy_pool(&self) -> iris_core::resource::ResourceRef<'_, iris_bastion::EnemyPool> {
        self.app
            .world
            .try_resource::<iris_bastion::EnemyPool>()
            .expect("敌人池应已由 Startup 插入")
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

    /// 幽灵塔身当前挂的材质句柄（红蓝着色断言用）。
    fn ghost_body_material(&self, body: iris_core::prelude::Entity) -> MaterialHandle {
        *self
            .app
            .world
            .get::<MaterialHandle>(body)
            .expect("幽灵塔身应有材质句柄")
    }
}

/// Startup 后的初始状态：四池预热完毕、HUD 就位、幽灵隐藏待命。
#[test]
fn startup_prewarms_pools_and_shows_hud() {
    let harness = Harness::new();
    {
        let game = harness.game();
        assert_eq!(game.phase, Phase::Playing, "塔防开局即对局");
        assert_eq!(game.wave, 1);
        assert_eq!(game.core_hp, 10);
        assert_eq!(game.credits, 100);
        assert!(game.towers.is_empty() && game.enemies.is_empty());
    }
    // 四池容量与待命态
    assert_eq!(
        harness
            .app
            .world
            .try_resource::<iris_bastion::TowerPool>()
            .expect("塔池在位")
            .slots
            .len(),
        POOL_TOWERS
    );
    {
        let pool = harness.enemy_pool();
        assert_eq!(pool.slots.slots.len(), POOL_ENEMIES);
        assert_eq!(pool.slots.free_count(), POOL_ENEMIES);
        assert!(pool.slots.active().is_empty());
        for &slot in &pool.slots.slots {
            assert_eq!(
                *harness
                    .app
                    .world
                    .get::<Visibility>(slot)
                    .expect("槽位应有 Visibility"),
                Visibility::HIDDEN,
                "待命槽位应隐藏"
            );
        }
    }
    for (resource, expected) in [
        (
            harness
                .app
                .world
                .try_resource::<iris_bastion::BulletPool>()
                .map(|p| p.0.slots.len()),
            POOL_BULLETS,
        ),
        (
            harness
                .app
                .world
                .try_resource::<iris_bastion::ShellPool>()
                .map(|p| p.0.slots.len()),
            POOL_SHELLS,
        ),
        (
            harness
                .app
                .world
                .try_resource::<iris_bastion::FxPool>()
                .map(|p| p.slots.len()),
            POOL_FX,
        ),
    ] {
        assert_eq!(resource, Some(expected));
    }
    // FX 槽位预挂禁用发射器（触发 = 覆写组件值的载体）
    let fx_slots = harness
        .app
        .world
        .try_resource::<iris_bastion::FxPool>()
        .expect("FX 池在位")
        .slots
        .clone();
    for &slot in &fx_slots {
        let emitter = harness
            .app
            .world
            .get::<ParticleEmitter>(slot)
            .expect("FX 槽位应预挂禁用发射器");
        assert_eq!(emitter.live_count(), 0, "待命发射器应无活粒子");
    }
    // HUD 实体按名可定位
    assert_eq!(harness.hud_text(CREDITS_TEXT_NAME), "CREDITS 100");
    assert_eq!(harness.hud_text(CORE_TEXT_NAME), "CORE HP 10/10");
    assert_eq!(harness.hud_text(BANNER_TEXT_NAME), "", "开局无横幅");
}

/// 幽灵预览（R23）：光标跟格、可放蓝 / 不可放红、射程盘随塔种缩放、
/// 数字键与右键切换塔种、离场隐藏。
#[test]
fn ghost_preview_follows_cursor_tints_and_switches_kind() {
    let mut harness = Harness::new();
    let ents = *harness
        .app
        .world
        .try_resource::<iris_bastion::BastionEntities>()
        .expect("实体索引在位");
    let (ok_mat, blocked_mat) = {
        let materials = harness
            .app
            .world
            .try_resource::<iris_bastion::GhostMaterials>()
            .expect("幽灵材质在位");
        (materials.ok, materials.blocked)
    };

    // 初始（无光标）：幽灵隐藏
    assert_eq!(
        *harness
            .app
            .world
            .get::<Visibility>(ents.ghost_root)
            .expect("幽灵根应有 Visibility"),
        Visibility::HIDDEN
    );

    // 光标进格：可见 + 哨兵形态 + 蓝（可放）+ 射程盘 = 哨兵射程
    harness.move_cursor_to_cell(IVec2::new(8, 2));
    harness.frame();
    assert_eq!(
        *harness
            .app
            .world
            .get::<Visibility>(ents.ghost_root)
            .unwrap(),
        Visibility::VISIBLE,
        "光标在场上时幽灵应可见"
    );
    assert_eq!(
        harness.ghost_body_material(ents.ghost_sentry_body),
        ok_mat,
        "空格应蓝（可放）"
    );
    assert_eq!(
        harness
            .app
            .world
            .get::<iris_core::prelude::Transform>(ents.ghost_ring)
            .expect("射程盘应有 Transform")
            .scale
            .x,
        iris_bastion::SENTRY_RANGE,
        "哨兵射程盘半径应等于哨兵射程"
    );

    // 数字键 2 → 臼炮形态 + 射程盘放大
    harness.tap(KeyCode::Digit2);
    harness.frame();
    assert_eq!(
        *harness
            .app
            .world
            .get::<Visibility>(ents.ghost_mortar_body)
            .unwrap(),
        Visibility::VISIBLE,
        "切臼炮后应显示臼炮形态"
    );
    assert_eq!(
        *harness
            .app
            .world
            .get::<Visibility>(ents.ghost_sentry_body)
            .unwrap(),
        Visibility::HIDDEN
    );
    assert!(
        (harness
            .app
            .world
            .get::<iris_core::prelude::Transform>(ents.ghost_ring)
            .unwrap()
            .scale
            .x
            - iris_bastion::MORTAR_RANGE)
            .abs()
            < EPS,
        "臼炮射程盘半径应等于臼炮射程"
    );

    // 占住一格后光标移过去：红（不可放）
    {
        let mut game = harness.app.world.resource_mut::<BastionState>();
        game.towers.push(iris_bastion::TowerState {
            cell: IVec2::new(8, 2),
            kind: TowerKind::Sentry,
            cooldown: 0.0,
        });
    }
    harness.frame();
    assert_eq!(
        harness.ghost_body_material(ents.ghost_mortar_body),
        blocked_mat,
        "已占格应红（不可放）"
    );

    // 光标移出场地：幽灵整组隐藏
    harness.move_cursor(3.0, 3.0);
    harness.frame();
    assert_eq!(
        *harness
            .app
            .world
            .get::<Visibility>(ents.ghost_root)
            .unwrap(),
        Visibility::HIDDEN,
        "界外光标应隐藏幽灵"
    );

    // 右键切回哨兵
    harness.move_cursor(
        world_to_screen(
            &harness.app.world,
            glam::Vec3::new(
                cell_center(IVec2::new(9, 2)).x,
                0.0,
                cell_center(IVec2::new(9, 2)).y,
            ),
        )
        .expect("应可投影")
        .0,
        world_to_screen(
            &harness.app.world,
            glam::Vec3::new(
                cell_center(IVec2::new(9, 2)).x,
                0.0,
                cell_center(IVec2::new(9, 2)).y,
            ),
        )
        .expect("应可投影")
        .1,
    );
    harness.click_right();
    harness.frame();
    {
        let kind = harness
            .app
            .world
            .try_resource::<iris_bastion::GhostKind>()
            .expect("GhostKind 在位");
        assert_eq!(kind.0, TowerKind::Sentry, "右键应从臼炮切回哨兵");
    }
}

/// 鼠标放塔全链路（R21）：点击 → 拾取 → 校验 → 扣款落塔 → 池槽位翻开；
/// 重复点击同格被拒；积分不足被拒；HUD 积分刷新。
#[test]
fn mouse_click_places_tower_through_full_pick_chain() {
    let mut harness = Harness::new();
    // 点击 (2, 3)：落塔 + 扣款
    harness.click_cell(IVec2::new(2, 3));
    harness.frame();
    {
        let game = harness.game();
        assert_eq!(game.towers.len(), 1, "点击应落塔");
        assert_eq!(game.towers[0].cell, IVec2::new(2, 3));
        assert_eq!(game.credits, 100 - 50, "应扣除哨兵造价");
    }
    // 塔池槽位 0 可见、位置在格心
    let slot = harness
        .app
        .world
        .try_resource::<iris_bastion::TowerPool>()
        .expect("塔池在位")
        .slots[0];
    assert_eq!(
        *harness.app.world.get::<Visibility>(slot).unwrap(),
        Visibility::VISIBLE
    );
    let position = harness
        .app
        .world
        .get::<iris_core::prelude::Transform>(slot)
        .expect("槽位应有 Transform")
        .position;
    let center = cell_center(IVec2::new(2, 3));
    assert!(
        (position.x - center.x).abs() < EPS && (position.z - center.y).abs() < EPS,
        "塔应立在格心：{position:?} vs {center:?}"
    );
    // 同格再点：拒绝（不扣款、不落塔）
    harness.click_cell(IVec2::new(2, 3));
    harness.frame();
    {
        let game = harness.game();
        assert_eq!(game.towers.len(), 1, "同格重复放塔应被拒");
        assert_eq!(game.credits, 50, "被拒请求不应扣款");
    }
    // 积分不足：拒绝
    {
        harness.app.world.resource_mut::<BastionState>().credits = 10;
    }
    harness.click_cell(IVec2::new(8, 5));
    harness.frame();
    {
        let game = harness.game();
        assert_eq!(game.towers.len(), 1, "积分不足放塔应被拒");
        assert_eq!(game.credits, 10, "被拒请求不应扣款");
    }
    // HUD 刷新
    assert_eq!(harness.hud_text(CREDITS_TEXT_NAME), "CREDITS 10");
}

/// 空间音频（R25）：哨兵开火 / 命中 / 击杀声定位入混音器，听者悬于
/// 相机高度。headless 断言混音器表状态而非实听（与 iris-arena 同约定）。
#[test]
fn spatial_audio_sounds_enter_mixer_and_listener_follows_camera() {
    let mut harness = Harness::new();
    // 直接构造接敌：哨兵钉在路径边、疾行虫进入射程（字段公开，同 logic 单测约定）
    {
        let mut game = harness.app.world.resource_mut::<BastionState>();
        game.enemies.clear();
        game.spawn_queue.clear();
        game.enemies.push(iris_bastion::EnemyState {
            id: 100,
            kind: EnemyKind::Runner,
            progress: 8.0,
            hp: 1, // 一发即死：同帧可观察到 命中 + 死亡 两类声源
        });
        game.towers.push(iris_bastion::TowerState {
            cell: IVec2::new(6, 4),
            kind: TowerKind::Sentry,
            cooldown: 0.0,
        });
    }
    harness.frames(3);
    {
        let audio = harness.app.world.resource::<Audio>();
        let listener = audio.mixer().listener();
        assert!(
            (listener.position.y - 25.0).abs() < EPS,
            "听者应悬在相机高度（25 m）：{:?}",
            listener.position
        );
        assert!(
            audio.mixer().active_count() >= 1,
            "交火中的开火 / 命中 / 死亡声应已入混音器表（空间化播放）"
        );
    }
}

/// 粒子（R24）：击杀爆炸在 FX 池槽位上真实触发（ParticlePlugin 推进，
/// 池槽位零 spawn 复用），活粒子数与发射器池对账。
#[test]
fn death_triggers_explosion_on_fx_pool_slots() {
    let mut harness = Harness::new();
    {
        let mut game = harness.app.world.resource_mut::<BastionState>();
        game.enemies.clear();
        game.spawn_queue.clear();
        game.enemies.push(iris_bastion::EnemyState {
            id: 200,
            kind: EnemyKind::Runner,
            progress: 8.0,
            hp: 1,
        });
        game.towers.push(iris_bastion::TowerState {
            cell: IVec2::new(6, 4),
            kind: TowerKind::Sentry,
            cooldown: 0.0,
        });
    }
    // 推进到击杀（哨兵 1 发即死）
    let mut killed = false;
    for _ in 0..60 {
        harness.frame();
        if harness.game().total_kills > 0 {
            killed = true;
            break;
        }
    }
    assert!(killed, "1 HP 敌人应在 1 s 内被击杀");
    // 击杀帧的 PostUpdate 已跑 update_particles：某个 FX 槽位应有活粒子
    let fx_slots = harness
        .app
        .world
        .try_resource::<iris_bastion::FxPool>()
        .expect("FX 池在位")
        .slots
        .clone();
    let total_live: usize = fx_slots
        .iter()
        .map(|&slot| {
            harness
                .app
                .world
                .get::<ParticleEmitter>(slot)
                .expect("FX 槽位应有发射器")
                .live_count()
        })
        .sum();
    assert!(
        total_live > 60,
        "击杀爆炸应在 FX 槽位上留有大量活粒子，实际 {total_live}"
    );
    // 爆炸粒子随寿命自然清零（槽位可复用）
    harness.frames(120);
    let total_live: usize = fx_slots
        .iter()
        .map(|&slot| {
            harness
                .app
                .world
                .get::<ParticleEmitter>(slot)
                .expect("FX 槽位应有发射器")
                .live_count()
        })
        .sum();
    assert_eq!(total_live, 0, "2 s 后爆炸粒子应全部熄灭");
}

/// 脚本化自动玩家：预设放塔序列经鼠标点击全链路放置，跑完 5 波必须
/// **获胜且核心剩余 HP ≥ 3**——真实模拟验证平衡与索敌 / 经济闭环。
#[test]
fn auto_player_wins_all_five_waves_with_core_hp_ge_3() {
    let mut harness = Harness::new();
    let mut curve: Vec<(usize, u32, u32, i32, u32)> = Vec::new(); // (秒, 波, 积分, 核心HP, 击杀)
    let mut last_wave = 1;
    let max_frames = 60 * 420; // 7 分钟游戏时间封顶
    let mut frames = 0usize;

    while frames < max_frames {
        let (phase, wave, credits, core_hp, kills, pending) = {
            let game = harness.game();
            let pending = BUILD_ORDER
                .iter()
                .position(|&(cell, kind)| game.check_placement(cell, kind).is_ok());
            (
                game.phase,
                game.wave,
                game.credits,
                game.core_hp,
                game.total_kills,
                pending,
            )
        };
        if wave != last_wave {
            last_wave = wave;
            curve.push((frames / 60, wave, credits, core_hp, kills));
        }
        if phase != Phase::Playing {
            break;
        }
        // 自动玩家：清单里下一座攒够造价的塔 → 挪光标 + 左键（全链路）
        if let Some(index) = pending {
            let (cell, _) = BUILD_ORDER[index];
            harness.click_cell(cell);
        }
        harness.frame();
        frames += 1;
    }

    // ---- 结果断言：必须获胜、核心余量 ≥ 3 ----
    let game = harness.game();
    println!("=== 自动玩家战报 ===");
    println!(
        "终局：{:?}，用时 {:.1} s（游戏时间）",
        game.phase,
        frames as f32 / 60.0
    );
    println!("波次曲线（秒 | 波 | 积分 | 核心HP | 击杀）:");
    for (sec, wave, credits, hp, kills) in &curve {
        println!(
            "  {:>3}s | W{} | {:>4} | {:>2} | {}",
            sec, wave, credits, hp, kills
        );
    }
    println!(
        "终态：积分 {}，核心 HP {}/10，击杀 {}，漏怪 {}，塔 {} 座",
        game.credits,
        game.core_hp,
        game.total_kills,
        game.total_leaks,
        game.towers.len()
    );
    assert_eq!(
        game.phase,
        Phase::Victory,
        "自动玩家必须获胜（实际 {:?}，漏怪 {}）",
        game.phase,
        game.total_leaks
    );
    assert!(
        game.core_hp >= 3,
        "核心剩余 HP 必须 ≥ 3（实际 {}，漏怪 {}）",
        game.core_hp,
        game.total_leaks
    );
    assert_eq!(game.wave, 5, "应打满 5 波");
    assert!(
        game.towers.len() >= 6,
        "经济闭环应支撑 ≥ 6 座塔（实际 {}）",
        game.towers.len()
    );
    assert!(curve.len() >= 4, "波次应逐波推进");
    // 击杀数 = 5 波敌人总数（获胜即全灭，无一漏网）
    let expected_kills: usize = (1..=iris_bastion::MAX_WAVES)
        .map(|wave| iris_bastion::wave_composition(wave).len())
        .sum();
    assert_eq!(
        game.total_kills as usize, expected_kills,
        "获胜应全灭全部波次敌人（应杀 {expected_kills}）"
    );
    // 敌池完全归还（胜负已分、场上清空）
    drop(game);
    let pool = harness.enemy_pool();
    assert!(pool.slots.active().is_empty(), "终局后敌人池应全部归还");
    assert_eq!(
        pool.slots.active().len() + pool.slots.free_count(),
        POOL_ENEMIES,
        "池守恒：活跃 + 闲置 = 容量（不超发、不泄漏）"
    );
}

/// 混战负载下的 draw calls 与剔除（记录性断言）：真实战局一帧保持
/// 300 以内，且界外岩柱提供 culled_instances > 0 的量化证据。无 GPU
/// 环境自动跳过（与其余 GPU 集成测试同约定）。
#[test]
fn draw_calls_and_culling_in_real_battle() {
    let Some(mut renderer) = iris_engine::prelude::OffscreenRenderer::new(320, 180) else {
        println!("无 GPU，跳过 draw calls 记录性断言");
        return;
    };
    let mut harness = Harness::new();
    // 构造混战：两座塔 + 8 只敌人 + 一条在飞弹道（字段公开，同 logic 单测约定）
    {
        let mut game = harness.app.world.resource_mut::<BastionState>();
        game.enemies.clear();
        game.spawn_queue.clear();
        for (index, progress) in [6.0f32, 8.0, 10.0, 12.0, 20.0, 22.0, 30.0, 32.0]
            .into_iter()
            .enumerate()
        {
            game.enemies.push(iris_bastion::EnemyState {
                id: 300 + index as u32,
                kind: if index % 3 == 2 {
                    EnemyKind::Brute
                } else {
                    EnemyKind::Runner
                },
                progress,
                hp: 9,
            });
        }
        game.towers.push(iris_bastion::TowerState {
            cell: IVec2::new(6, 5),
            kind: TowerKind::Sentry,
            cooldown: 0.0,
        });
        game.bullets.push(iris_bastion::BulletState {
            id: 900,
            pos: iris_bastion::path_point(7.0),
            vel: glam::Vec2::new(14.0, 0.0),
            life: 0.5,
        });
    }
    harness.frame();
    renderer.render(&harness.app.world);
    let stats = renderer.renderer().stats();
    println!(
        "混战一帧：draw_calls = {}，instances = {}，culled = {}，particles = {}",
        stats.draw_calls, stats.instances, stats.culled_instances, stats.particles_drawn
    );
    assert!(
        stats.draw_calls < 300,
        "混战一帧 draw calls 应 < 300，实际 {}",
        stats.draw_calls
    );
    assert!(
        stats.culled_instances > 0,
        "界外岩柱应产生视锥剔除（culled_instances > 0），实际 {}",
        stats.culled_instances
    );
}
