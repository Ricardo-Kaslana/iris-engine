//! headless 确定性集成测试：**不建窗口、不建 GPU**，复用 `GamePlugin`
//! 的完整系统链（`build_schedule`，与 DefaultPlugins 同一编排）驱动真实
//! 游戏世界，验证引擎资源可脚本化接线：
//!
//! - `Time`：`begin_frame(Instant)` 以固定 1/60 s 推进（在 delta 钳制上限
//!   0.25 s 之内），首帧 delta 为 0（与 App 主循环一致）；
//! - `Input`：`on_key(PhysicalKey, ElementState)` 事件注入按键（winit
//!   类型在 dev-dependencies），`end_frame` 清一次性状态；
//! - 各 Store / `Audio` 由测试手动插入，插件的 Startup 只兜底补缺。
//!
//! 每帧时序镜像 `App::tick`：begin_frame → Update → clear_events →
//! input.end_frame。

use std::time::Duration;

use iris_core::prelude::{Entity, Instant, Schedule, Stage, World};
use iris_core::resource::ResourceRef;
use iris_engine::prelude::{
    Audio, Camera, DirectionalLight, GlobalTransform, Input, KeyCode, MaterialStore, MeshStore,
    TextureStore, Time, Transform, UiText,
};
use iris_game::{GameEntities, GamePlugin, GameState, Phase};
// 注入侧的 winit 事件类型（Input::on_key 的参数）；查询侧 KeyCode 已由
// iris_engine::prelude 重导出
use winit::event::ElementState;
use winit::keyboard::PhysicalKey;

/// 标准容差：1/60 s 步长下 30 帧位移的浮点误差量级。
const EPS: f32 = 1e-3;

fn press(input: &mut Input, key: KeyCode, down: bool) {
    let state = if down { ElementState::Pressed } else { ElementState::Released };
    input.on_key(PhysicalKey::Code(key), state);
}

/// headless 测试台：世界 + GamePlugin 系统链 + 虚拟时钟。
struct Harness {
    world: World,
    schedule: Schedule,
    /// 已推进的虚拟时间（秒）；begin_frame 的时间点 = 起点采样 + t。
    elapsed: f64,
    /// begin_frame 的时间基准（构造时采样一次，保证帧间差分精确 1/60 s）。
    origin: Instant,
}

impl Harness {
    fn new() -> Self {
        let mut world = World::new();
        // 手动插入 DefaultPlugins 等价的资源（不建窗口 / 不建 GPU）；
        // GamePlugin 的 setup_resources 对已存在资源一律不覆盖
        world.insert_resource(Time::default());
        world.insert_resource(Input::new());
        world.insert_resource(MeshStore::new());
        world.insert_resource(MaterialStore::new());
        world.insert_resource(TextureStore::new());
        world.insert_resource(Camera::default());
        world.insert_resource(DirectionalLight::default());
        world.insert_resource(Audio::new());

        let mut schedule = Schedule::new();
        GamePlugin.build_schedule(&mut schedule);

        let mut harness =
            Self { world, schedule, elapsed: 0.0, origin: Instant::now() };
        // Startup：资源兜底插入 + 场景搭建（与 App 主循环一致，先于首帧）
        harness.schedule.run(Stage::Startup, &mut harness.world);
        // 首帧 delta = 0（Time::begin_frame 的既定语义），只建立时间基准
        harness.frame(&[]);
        harness
    }

    /// 推进一帧：固定 1/60 s，`held` 为本帧按住的键（重复注入由 Input
    /// 去重——按住不放只在首帧产生 key_pressed）。
    fn frame(&mut self, held: &[KeyCode]) {
        self.elapsed += 1.0 / 60.0;
        let now = self.origin + Duration::from_secs_f64(self.elapsed);
        self.world.resource_mut::<Time>().begin_frame(now);
        {
            let mut input = self.world.resource_mut::<Input>();
            for &key in held {
                press(&mut input, key, true);
            }
        }
        self.schedule.run(Stage::Update, &mut self.world);
        self.world.clear_all_events();
        self.world.resource_mut::<Input>().end_frame();
    }

    /// 连续推进多帧，按住同一组键。除首帧外每帧都有 1/60 s 的有效 delta。
    fn frames(&mut self, count: usize, held: &[KeyCode]) {
        for _ in 0..count {
            self.frame(held);
        }
    }

    fn game(&self) -> ResourceRef<'_, GameState> {
        self.world.try_resource::<GameState>().expect("GameState 应已由 Startup 插入")
    }

    fn entities(&self) -> GameEntities {
        self.world
            .try_resource::<GameEntities>()
            .expect("实体索引应已由 Startup 插入")
            .clone()
    }

    fn player_position(&self) -> glam::Vec3 {
        self.world
            .get::<Transform>(self.entities().player)
            .expect("玩家实体应有 Transform")
            .position
    }

    fn hud_text(&self, entity: Entity) -> String {
        self.world
            .get::<UiText>(entity)
            .expect("HUD 文本实体应存在")
            .string
            .clone()
    }
}

/// Startup 后的初始状态：场景、实体索引、逻辑初值齐备。
#[test]
fn startup_builds_playable_world() {
    let harness = Harness::new();
    let game = harness.game();
    assert_eq!(game.phase, Phase::Playing);
    assert_eq!(game.score, 0);
    assert!((game.time_left - 60.0).abs() < 1e-5);
    assert_eq!(game.pickups.len(), 10, "应布下 10 枚金币");
    assert!(!game.pickups.iter().any(|p| p.collected));

    let ents = harness.entities();
    assert_eq!(ents.coins.len(), 10);
    // 玩家贴地站在原点（方块半高 0.45），相机子实体在玩家后上方
    let player = harness.player_position();
    assert!(player.x.abs() < 1e-5 && player.z.abs() < 1e-5);
    assert!((player.y - 0.45).abs() < 1e-5);
    let cam = harness
        .world
        .get::<GlobalTransform>(ents.camera)
        .expect("相机应已有 GlobalTransform")
        .position();
    assert!((cam - glam::Vec3::new(0.0, 4.95, 8.0)).length() < 1e-4,
        "开局相机应在 (0, 4.95, 8)，实际 {cam:?}");
}

/// 无输入时玩家静止（Time 照常流逝、倒计时推进）。
#[test]
fn idle_input_keeps_player_still() {
    let mut harness = Harness::new();
    harness.frames(30, &[]);
    let pos = harness.player_position();
    assert!(pos.x.abs() < 1e-5 && pos.z.abs() < 1e-5, "静止输入不应移动：{pos:?}");
    let game = harness.game();
    assert_eq!(game.score, 0);
    assert_eq!(game.phase, Phase::Playing);
    // 约 0.5 s 流逝，倒计时已开始但尚未跨过整数秒
    assert!(game.time_left < 60.0 && game.time_left > 59.0);
}

/// 按住 D（世界 +X）：位移符合 5 m/s 积分、朝向转往 +X、
/// 相机（玩家子实体）经层级传播跟随。
#[test]
fn held_key_drives_player_and_camera_follows() {
    let mut harness = Harness::new();
    harness.frames(30, &[KeyCode::KeyD]);
    let expected_x = 30.0 / 60.0 * 5.0; // 30 个有效步（首帧 delta 为 0）
    let pos = harness.player_position();
    assert!((pos.x - expected_x).abs() < EPS, "x 应为 {expected_x}，实际 {}", pos.x);
    assert!(pos.z.abs() < EPS);
    let game = harness.game();
    assert!(
        (game.player.facing + std::f32::consts::FRAC_PI_2).abs() < 1e-3,
        "+X 移动后朝向应为 -π/2，实际 {}",
        game.player.facing
    );
    // 相机全局位置 = 玩家位置 + Ry(facing)·(0, 4.5, 8)；
    // facing = -π/2 时局部偏移旋到 (-8, 4.5, 0)
    let cam = harness
        .world
        .get::<GlobalTransform>(harness.entities().camera)
        .expect("相机应已有 GlobalTransform")
        .position();
    let expected_cam = glam::Vec3::new(pos.x - 8.0, 4.95, pos.z);
    assert!(
        (cam - expected_cam).length() < 1e-3,
        "相机应跟随玩家到 {expected_cam:?}，实际 {cam:?}"
    );
}

/// 按住 Space：单次起跳（key_pressed 只在首帧为真）、解析顶点约 1.225 m、
/// 约 0.7 s 后落地；落地不因持续按住而连跳。
#[test]
fn space_triggers_single_jump_arc() {
    let mut harness = Harness::new();
    let mut max_y = 0.0f32;
    for _ in 0..60 {
        harness.frame(&[KeyCode::Space]);
        max_y = max_y.max(harness.game().player.y);
    }
    // 解析顶点 1.225 + 离散化误差 ≈ ½·|g|·t_apex·dt（dt = 1/60 时约 +0.058）
    assert!((max_y - 1.225).abs() < 0.08, "跳跃顶点应约 1.225 m，实际 {max_y}");
    let game = harness.game();
    assert_eq!(game.player.y, 0.0, "0.7 s 后应已落地");
    assert_eq!(game.player.y_vel, 0.0);
}

/// 脚本化路线：从原点按住 D 直行拾取环上 (9, 0) 金币——计分、金币沉底
/// 隐藏、HUD 计分变化、拾取音效进入混音器。
#[test]
fn scripted_route_picks_up_coin_and_updates_hud() {
    let mut harness = Harness::new();
    // 110 个有效步 × 5/60 m ≈ 9.17 m，途经 8.1 m 处进入拾取半径
    harness.frames(110, &[KeyCode::KeyD]);
    assert_eq!(harness.game().score, 1, "应拾取 1 枚金币");
    assert!(harness.game().pickups[0].collected);

    let ents = harness.entities();
    // 已收集金币被表现系统钉到 y = -100 隐藏
    let coin_y = harness
        .world
        .get::<Transform>(ents.coins[0])
        .expect("金币实体应存在")
        .position
        .y;
    assert_eq!(coin_y, -100.0, "已收集金币应沉底隐藏");
    // HUD 计分文本刷新
    assert_eq!(harness.hud_text(ents.score_text), "SCORE 1/10");
    // 拾取音效已入混音器（0.15 s 短音，同帧内仍在活跃表）
    let audio = harness.world.try_resource::<Audio>().unwrap();
    assert!(audio.mixer().active_count() >= 1, "拾取应有音效入列");
}

/// 倒计时会刷新 HUD 的 TIME 文本（约 1.15 s 后从 60 变 59）。
#[test]
fn countdown_updates_hud_time_text() {
    let mut harness = Harness::new();
    let time_text = harness.entities().time_text;
    assert_eq!(harness.hud_text(time_text), "TIME 60");
    harness.frames(70, &[]);
    assert_eq!(harness.hud_text(time_text), "TIME 59", "约 1.15 s 后应显示 59");
}

/// 全收集流程：标记 9 枚已收 + 传送到最后一枚旁 → 胜利横幅 →
/// 终局后输入冻结（位置 / 计分 / 横幅不再变化）。
#[test]
fn collecting_all_wins_then_freezes() {
    let mut harness = Harness::new();
    // 直接构造「已收 9 枚」的中间态（GameState 字段公开，同 logic 单测）
    {
        let mut game = harness.world.resource_mut::<GameState>();
        for (i, pickup) in game.pickups.iter_mut().enumerate() {
            pickup.collected = i != 4; // 留下 4 号（环上 (2.78, -8.56)）
        }
        game.score = 9;
        // 传送到最后一枚 0.5 m 旁（y 贴地满足拾取高度）
        game.player.x = game.pickups[4].x + 0.5;
        game.player.z = game.pickups[4].z;
    }
    harness.frame(&[]);
    {
        let game = harness.game();
        assert_eq!(game.score, 10);
        assert_eq!(game.phase, Phase::Won);
        assert!(game.pickups.iter().all(|p| p.collected));
    }
    let ents = harness.entities();
    assert_eq!(harness.hud_text(ents.banner_text), "YOU WIN!", "胜利横幅应显示");

    // 终局冻结：继续输入，一切不再变化
    let before_pos = harness.player_position();
    harness.frames(30, &[KeyCode::KeyD, KeyCode::Space]);
    let game = harness.game();
    assert_eq!(harness.player_position(), before_pos, "终局后玩家应冻结");
    assert_eq!(game.score, 10);
    assert_eq!(game.phase, Phase::Won);
    assert_eq!(harness.hud_text(ents.banner_text), "YOU WIN!");
}

/// 超时路线：把倒计时拨到临界值再推进一帧 → TIME UP 横幅 + 冻结。
#[test]
fn timeout_shows_time_up_banner() {
    let mut harness = Harness::new();
    harness.world.resource_mut::<GameState>().time_left = 0.01;
    harness.frame(&[KeyCode::KeyD]);
    {
        let game = harness.game();
        assert_eq!(game.phase, Phase::Lost);
        assert_eq!(game.time_left, 0.0);
        assert_eq!(game.score, 0, "超时时不应有拾取分（半径外直行）");
    }
    let ents = harness.entities();
    assert_eq!(harness.hud_text(ents.banner_text), "TIME UP");
    // 终局冻结
    let before_pos = harness.player_position();
    harness.frames(10, &[KeyCode::KeyD]);
    assert_eq!(harness.player_position(), before_pos);
    let game = harness.game();
    assert_eq!(game.phase, Phase::Lost, "终局后阶段不再变化");
}
