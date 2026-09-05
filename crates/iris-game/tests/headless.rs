//! headless 确定性集成测试：**不建窗口、不建 GPU、不依赖 winit**，复用
//! `GamePlugin` 的完整系统链（`build_schedule`，与 DefaultPlugins 同一编排）
//! 驱动真实游戏世界，验证引擎资源可脚本化接线：
//!
//! - `Time`：`advance(Duration)` 确定性推进（首帧即有效 delta，无须再
//!   手工构造单调 `Instant` 序列建立时钟基准）；
//! - `Input`：`press(KeyCode)` / `release(KeyCode)` 注入按键（引擎补齐
//!   便捷注入并在 prelude 重导出 winit 类型后，本测试已无 winit 依赖）；
//! - `Audio`：预插入 `Audio::headless()`（纯混音器，绝不打开设备），
//!   插件的 Startup 兜底逻辑遇到已存在资源不覆盖；
//! - **场景**：Harness 把内联 JSON 写进临时目录的 fixture 关卡（最小布局：
//!   四面围墙 + 一根障碍柱，无地面 collider——垂直面归逻辑 y 积分管），
//!   经 `GamePlugin::new` 注入——全部测试（含墙体阻挡 / 贴墙滑行）都跑在
//!   真实的场景加载路径上；
//! - 层级传播：`HierarchyPlugin` 注册的 PostUpdate 阶段（与窗口程序同一
//!   编排来源），每帧时序镜像 `App::tick`：advance → Update →
//!   PostUpdate → clear_events → input.end_frame。

use std::path::PathBuf;
use std::time::Duration;

use iris_core::prelude::Stage;
use iris_core::resource::ResourceRef;
use iris_engine::prelude::{
    App, Audio, Camera, DirectionalLight, GlobalTransform, HierarchyPlugin, Input, KeyCode,
    MaterialStore, MeshStore, Plugin, TextureStore, Time, Transform, UiText, Visibility,
};
use iris_game::{
    BANNER_TEXT_NAME, GameEntities, GamePlugin, GameState, Phase, SCORE_TEXT_NAME, TIME_TEXT_NAME,
};

/// 标准容差：1/60 s 步长下 30 帧位移的浮点误差量级。
const EPS: f32 = 1e-3;

/// 最小 fixture 关卡：与正式关卡（`assets/collect_arena.scene.json`）同一
/// 几何约定——四面围墙的静态 Box collider 内缘 ±17.5（玩家球体半径 0.45 →
/// 贴墙位置 17.05），外加一根障碍柱。**地面省略 collider**：垂直面由逻辑
/// 的 y 积分管，且推离只取水平分量（地面法线 ±Y 的水平投影恒 0），摆一个
/// 永不生效的地面碰撞盒不如省略诚实。柱子压在金币环缺口（环上两枚金币的
/// 中点），与任何金币 / 直行拾取路线不重叠——既有拾取 / 胜负测试的语义
/// 在该关卡下保持不变。无需环境段（Harness 已预插默认方向光）。
const FIXTURE_SCENE: &str = r#"{
    "entities": [
        {
            "name": "wall-east",
            "transform": { "position": [18.0, 0.5, 0.0], "scale": [1.0, 1.0, 38.0] },
            "mesh": { "primitive": { "kind": "cube" } },
            "collider": { "shape": { "box": { "half_extents": [0.5, 0.5, 19.0] } }, "static": true }
        },
        {
            "name": "wall-west",
            "transform": { "position": [-18.0, 0.5, 0.0], "scale": [1.0, 1.0, 38.0] },
            "mesh": { "primitive": { "kind": "cube" } },
            "collider": { "shape": { "box": { "half_extents": [0.5, 0.5, 19.0] } }, "static": true }
        },
        {
            "name": "wall-north",
            "transform": { "position": [0.0, 0.5, -18.0], "scale": [38.0, 1.0, 1.0] },
            "mesh": { "primitive": { "kind": "cube" } },
            "collider": { "shape": { "box": { "half_extents": [19.0, 0.5, 0.5] } }, "static": true }
        },
        {
            "name": "wall-south",
            "transform": { "position": [0.0, 0.5, 18.0], "scale": [38.0, 1.0, 1.0] },
            "mesh": { "primitive": { "kind": "cube" } },
            "collider": { "shape": { "box": { "half_extents": [19.0, 0.5, 0.5] } }, "static": true }
        },
        {
            "name": "pillar",
            "transform": { "position": [-2.781, 1.2, 8.56] },
            "mesh": { "primitive": { "kind": "cylinder", "radius": 0.6, "height": 2.4, "segments": 24 } },
            "collider": { "shape": { "box": { "half_extents": [0.6, 1.2, 0.6] } }, "static": true }
        }
    ]
}"#;

/// 把 fixture 关卡写进临时目录（内容恒定，`Once` 防并行测试重复写），
/// 返回路径供 `GamePlugin::new` 注入。
fn fixture_scene_path() -> PathBuf {
    static WRITE_ONCE: std::sync::Once = std::sync::Once::new();
    let dir = std::env::temp_dir().join("iris-game-headless");
    std::fs::create_dir_all(&dir).expect("创建临时目录");
    let path = dir.join("fixture.scene.json");
    WRITE_ONCE.call_once(|| {
        std::fs::write(&path, FIXTURE_SCENE).expect("写 fixture 场景文件");
    });
    path
}

/// headless 测试台：世界 + GamePlugin 系统链 + 虚拟时钟。
struct Harness {
    app: App,
}

impl Harness {
    fn new() -> Self {
        let mut app = App::new();
        // 手动插入 DefaultPlugins 等价资源（不建窗口 / 不建 GPU）；
        // 音频强制 headless（纯混音器，绝不打开设备）；GamePlugin 的
        // setup_resources 对已存在资源一律不覆盖
        app.world.insert_resource(Time::default());
        app.world.insert_resource(Input::new());
        app.world.insert_resource(MeshStore::new());
        app.world.insert_resource(MaterialStore::new());
        app.world.insert_resource(TextureStore::new());
        app.world.insert_resource(Camera::default());
        app.world.insert_resource(DirectionalLight::default());
        app.world.insert_resource(Audio::headless());

        // 层级传播：HierarchyPlugin 注册的 PostUpdate 阶段（与 DefaultPlugins
        // 同一插件，PostUpdate 每帧在 Update 之后执行一次）
        HierarchyPlugin.build(&mut app);
        // 场景：临时 fixture 关卡经 GamePlugin::new 注入（Startup 走真实的
        // load_scene 路径——墙体 / 障碍柱的静态 Collider 由场景文件提供）
        GamePlugin::new(fixture_scene_path()).build_schedule(&mut app.schedule);

        let mut harness = Self { app };
        // Startup：资源兜底插入 + 场景搭建（与 App 主循环一致，先于首帧）
        harness
            .app
            .schedule
            .run(Stage::Startup, &mut harness.app.world);
        harness
    }

    /// 推进一帧：确定性 1/60 s（`Time::advance`，与 App::tick 的
    /// begin_frame 等价但不依赖单调时钟），`held` 为本帧按住的键（重复
    /// 注入由 Input 去重——按住不放只在首帧产生 key_pressed）。
    fn frame(&mut self, held: &[KeyCode]) {
        // 帧首事件轮换（镜像 App::tick 的新生命周期：上帧事件降级为仍可读的
        // 上一代、更早一代丢弃——打磨 V）
        self.app.world.clear_all_events();
        self.app
            .world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(1.0 / 60.0));
        {
            let mut input = self.app.world.resource_mut::<Input>();
            for &key in held {
                input.press(key);
            }
        }
        self.app.schedule.run(Stage::Update, &mut self.app.world);
        self.app
            .schedule
            .run(Stage::PostUpdate, &mut self.app.world);
        self.app.world.resource_mut::<Input>().end_frame();
    }

    /// 连续推进多帧，按住同一组键。
    fn frames(&mut self, count: usize, held: &[KeyCode]) {
        for _ in 0..count {
            self.frame(held);
        }
    }

    fn game(&self) -> ResourceRef<'_, GameState> {
        self.app
            .world
            .try_resource::<GameState>()
            .expect("GameState 应已由 Startup 插入")
    }

    fn entities(&self) -> GameEntities {
        self.app
            .world
            .try_resource::<GameEntities>()
            .expect("实体索引应已由 Startup 插入")
            .clone()
    }

    fn player_position(&self) -> glam::Vec3 {
        self.app
            .world
            .get::<Transform>(self.entities().player)
            .expect("玩家实体应有 Transform")
            .position
    }

    /// 按名读取 HUD 文本（Name + entity_named 定位，替代旧句柄资源字段）。
    fn hud_text(&self, name: &str) -> String {
        let entity = self
            .app
            .world
            .entity_named(name)
            .expect("按名应能定位 HUD 文本实体");
        self.app
            .world
            .get::<UiText>(entity)
            .expect("HUD 文本实体应有 UiText")
            .string
            .clone()
    }
}

/// Startup 后的初始状态：场景、实体索引、逻辑初值齐备。
#[test]
fn startup_builds_playable_world() {
    let mut harness = Harness::new();
    {
        let game = harness.game();
        assert_eq!(game.phase, Phase::Playing);
        assert_eq!(game.score, 0);
        assert!((game.time_left - 60.0).abs() < 1e-5);
        assert_eq!(game.pickups.len(), 10, "应布下 10 枚金币");
        assert!(!game.pickups.iter().any(|p| p.collected));
    }
    let ents = harness.entities();
    assert_eq!(ents.coins.len(), 10);
    // 金币开局全部可见（Visibility 组件就位，收集后由表现系统切 HIDDEN）
    for &coin in &ents.coins {
        assert_eq!(
            *harness
                .app
                .world
                .get::<Visibility>(coin)
                .expect("金币应有 Visibility"),
            Visibility::VISIBLE
        );
    }
    // 推进一帧（PostUpdate 传播）后：玩家贴地站在原点（方块半高 0.45），
    // 相机子实体经层级传播到达玩家后上方
    harness.frame(&[]);
    let player = harness.player_position();
    assert!(player.x.abs() < 1e-5 && player.z.abs() < 1e-5);
    assert!((player.y - 0.45).abs() < 1e-5);
    let cam = harness
        .app
        .world
        .get::<GlobalTransform>(ents.camera)
        .expect("相机应已有 GlobalTransform")
        .position();
    assert!(
        (cam - glam::Vec3::new(0.0, 4.95, 8.0)).length() < 1e-4,
        "开局相机应在 (0, 4.95, 8)，实际 {cam:?}"
    );
}

/// 无输入时玩家静止（Time 照常流逝、倒计时推进）。
#[test]
fn idle_input_keeps_player_still() {
    let mut harness = Harness::new();
    harness.frames(30, &[]);
    let pos = harness.player_position();
    assert!(
        pos.x.abs() < 1e-5 && pos.z.abs() < 1e-5,
        "静止输入不应移动：{pos:?}"
    );
    let game = harness.game();
    assert_eq!(game.score, 0);
    assert_eq!(game.phase, Phase::Playing);
    // 约 0.5 s 流逝，倒计时已开始但尚未跨过整数秒
    assert!(game.time_left < 60.0 && game.time_left > 59.0);
}

/// 按住 D（屏幕右侧 = 世界 +X，相机固定方位下二者一致）：
/// 位移符合 5 m/s 积分、朝向**平滑逼近** +X（渐进而非瞬跳）、
/// 相机（玩家子实体）经 PostUpdate 层级传播跟随且方位恒定。
#[test]
fn held_key_drives_player_and_camera_follows() {
    let mut harness = Harness::new();
    harness.frames(30, &[KeyCode::KeyD]);
    let expected_x = 30.0 / 60.0 * 5.0; // 30 个有效步（Time::advance 首帧即有效）
    let pos = harness.player_position();
    assert!(
        (pos.x - expected_x).abs() < EPS,
        "x 应为 {expected_x}，实际 {}",
        pos.x
    );
    assert!(pos.z.abs() < EPS);
    {
        let game = harness.game();
        // 指数平滑 30 帧（t=0.2/帧）后应已逼近目标 99% 以上，但不是精确等于——
        // 转身是渐进动画
        let want = -std::f32::consts::FRAC_PI_2;
        assert!(
            (game.player.facing - want).abs() < 0.05,
            "+X 移动后朝向应逼近 -π/2（容差 0.05），实际 {}",
            game.player.facing
        );
    }
    // 相机全局位置 = 玩家位置 + (0, 4.5, 8)——朝向写在模型子实体上，
    // 根实体 rotation 恒等，相机方位不随转身变化（旧实现会绕玩家公转）。
    let cam = harness
        .app
        .world
        .get::<GlobalTransform>(harness.entities().camera)
        .expect("相机应已有 GlobalTransform")
        .position();
    let expected_cam = glam::Vec3::new(pos.x, 4.95, pos.z + 8.0);
    assert!(
        (cam - expected_cam).length() < 1e-3,
        "相机应固定方位跟随玩家到 {expected_cam:?}，实际 {cam:?}"
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
    assert!(
        (max_y - 1.225).abs() < 0.08,
        "跳跃顶点应约 1.225 m，实际 {max_y}"
    );
    let game = harness.game();
    assert_eq!(game.player.y, 0.0, "0.7 s 后应已落地");
    assert_eq!(game.player.y_vel, 0.0);
}

/// 脚本化路线：从原点按住 D 直行拾取环上 (9, 0) 金币——计分、金币以
/// `Visibility::HIDDEN` 隐藏（不再钉出视野）、HUD 计分变化、拾取音效
/// 进入混音器。
#[test]
fn scripted_route_picks_up_coin_and_updates_hud() {
    let mut harness = Harness::new();
    // 110 个有效步 × 5/60 m ≈ 9.17 m，途经 8.1 m 处进入拾取半径
    harness.frames(110, &[KeyCode::KeyD]);
    assert_eq!(harness.game().score, 1, "应拾取 1 枚金币");
    assert!(harness.game().pickups[0].collected);

    let ents = harness.entities();
    // 已收集金币切为 Visibility::HIDDEN：全部渲染 pass 跳过
    assert_eq!(
        *harness
            .app
            .world
            .get::<Visibility>(ents.coins[0])
            .expect("金币实体应有 Visibility"),
        Visibility::HIDDEN,
        "已收集金币应以可见性开关隐藏"
    );
    // 金币不再被钉出视野：Transform 保持在场内的浮动基线附近
    let coin_y = harness
        .app
        .world
        .get::<Transform>(ents.coins[0])
        .expect("金币实体应存在")
        .position
        .y;
    assert!(coin_y > 0.0, "隐藏不再依赖移出视野（y = {coin_y}）");
    // 未收集金币保持可见
    assert_eq!(
        *harness.app.world.get::<Visibility>(ents.coins[1]).unwrap(),
        Visibility::VISIBLE
    );
    // HUD 计分文本刷新（按名定位）
    assert_eq!(harness.hud_text(SCORE_TEXT_NAME), "SCORE 1/10");
    // 拾取音效已入混音器（0.15 s 短音，同帧内仍在活跃表）
    let audio = harness.app.world.try_resource::<Audio>().unwrap();
    assert!(audio.mixer().active_count() >= 1, "拾取应有音效入列");
}

/// 倒计时会刷新 HUD 的 TIME 文本（约 1.15 s 后从 60 变 59）。
#[test]
fn countdown_updates_hud_time_text() {
    let mut harness = Harness::new();
    assert_eq!(harness.hud_text(TIME_TEXT_NAME), "TIME 60");
    harness.frames(70, &[]);
    assert_eq!(
        harness.hud_text(TIME_TEXT_NAME),
        "TIME 59",
        "约 1.15 s 后应显示 59"
    );
}

/// 全收集流程：标记 9 枚已收 + 传送到最后一枚旁 → 胜利横幅 →
/// 终局后输入冻结（位置 / 计分 / 横幅不再变化）。
#[test]
fn collecting_all_wins_then_freezes() {
    let mut harness = Harness::new();
    // 直接构造「已收 9 枚」的中间态（GameState 字段公开，同 logic 单测）
    {
        let mut game = harness.app.world.resource_mut::<GameState>();
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
    assert_eq!(
        harness.hud_text(BANNER_TEXT_NAME),
        "YOU WIN!",
        "胜利横幅应显示"
    );

    // 终局冻结：继续输入，一切不再变化
    let before_pos = harness.player_position();
    harness.frames(30, &[KeyCode::KeyD, KeyCode::Space]);
    let game = harness.game();
    assert_eq!(harness.player_position(), before_pos, "终局后玩家应冻结");
    assert_eq!(game.score, 10);
    assert_eq!(game.phase, Phase::Won);
    assert_eq!(harness.hud_text(BANNER_TEXT_NAME), "YOU WIN!");
}

/// 超时路线：把倒计时拨到临界值再推进一帧 → TIME UP 横幅 + 冻结。
#[test]
fn timeout_shows_time_up_banner() {
    let mut harness = Harness::new();
    harness.app.world.resource_mut::<GameState>().time_left = 0.01;
    harness.frame(&[KeyCode::KeyD]);
    {
        let game = harness.game();
        assert_eq!(game.phase, Phase::Lost);
        assert_eq!(game.time_left, 0.0);
        assert_eq!(game.score, 0, "超时时不应有拾取分（半径外直行）");
    }
    assert_eq!(harness.hud_text(BANNER_TEXT_NAME), "TIME UP");
    // 终局冻结
    let before_pos = harness.player_position();
    harness.frames(10, &[KeyCode::KeyD]);
    assert_eq!(harness.player_position(), before_pos);
    let game = harness.game();
    assert_eq!(game.phase, Phase::Lost, "终局后阶段不再变化");
}

/// 墙体阻挡（物理而非钳制）：把玩家挪到 +X 墙内侧（x = 17.0）按住 D
/// 推进 → x 稳定在 17.05 = 墙内缘 17.5 − 玩家半径 0.45，且**严格小于**
/// 逻辑钳制上限 18——挡路的是场景静态体的推离查询，不是边界 clamp。
#[test]
fn wall_collider_blocks_player_before_clamp() {
    let mut harness = Harness::new();
    harness.app.world.resource_mut::<GameState>().player.x = 17.0;
    harness.frames(60, &[KeyCode::KeyD]);
    let x = harness.game().player.x;
    assert!(
        (x - 17.05).abs() <= 0.05,
        "x 应被墙体 Collider 挡在 17.05，实际 {x}"
    );
    assert!(x < 17.9, "x 应小于钳制上限 18（证明在挡的是 Collider）");
    // 表现实体同步：玩家 Transform 已按修正后的逻辑坐标回写
    assert!(
        (harness.player_position().x - 17.05).abs() <= 0.05,
        "玩家实体应停在 17.05，实际 {:?}",
        harness.player_position()
    );
    // 持续推进 60 帧仍稳定贴墙：不隧穿、不缓慢渗入
    harness.frames(60, &[KeyCode::KeyD]);
    let x2 = harness.game().player.x;
    assert!((x2 - 17.05).abs() <= 0.05, "持续推进应稳定贴墙，实际 {x2}");
}

/// 贴墙滑行：玩家贴 +X 墙同时按 D+W → x 被墙挡住（法向分量被推离清零），
/// z 持续减小（切向速度保留）——滑行的数学基础是推离只沿接触法线。
#[test]
fn wall_slide_keeps_tangential_motion() {
    let mut harness = Harness::new();
    harness.app.world.resource_mut::<GameState>().player.x = 17.0;
    // 斜向输入归一化：每轴 5/√2 m/s，30 帧 ≈ 2.5 m 的切向滑行
    harness.frames(30, &[KeyCode::KeyD, KeyCode::KeyW]);
    let game = harness.game();
    assert!(
        (game.player.x - 17.05).abs() <= 0.05,
        "法向应被墙挡在 17.05，实际 {}",
        game.player.x
    );
    assert!(
        game.player.z < -1.5,
        "切向应持续滑行（30 帧约 -2.5 m），实际 z = {}",
        game.player.z
    );
}
