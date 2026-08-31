//! [`GamePlugin`]：把纯逻辑核心（`crate::logic`）接到 Iris 引擎上。
//!
//! 职责划分：
//! - **Startup**：`setup_resources`（兜底插入引擎资源 + 游戏资源）→
//!   `build_scene`（地面 / 围墙 / 灯光 / 玩家 + 子实体相机 / 金币 / HUD）；
//! - **Update**：`update_gameplay`（读 `Input` + `Time` → `GameState::step`
//!   → 写玩家 `Transform` + 触发音效）→ `animate_coins`（金币自转浮动，
//!   已收集者沉到 y = -100 隐藏）→ `update_hud`（SCORE / TIME / 横幅）→
//!   `post_update_propagate`（层级传播，见下方「引擎缺口绕行」注释）。
//!
//! 本插件**只使用引擎公开 API**（dogfooding）：不依赖引擎内部路径。
//! 场景搭建走 `World::spawn` + `MeshStore` / `MaterialStore`，相机挂在
//! 玩家子实体上验证层级跟随（`ActiveCamera` 优先于 `Camera` 资源），
//! 音效走程序化生成的 `AudioClip` + `Audio` 资源。
//!
//! 复用方式：窗口程序 `App::add_plugin(GamePlugin)`；headless 测试 /
//! 离屏快照直接 `GamePlugin.build_schedule(&mut Schedule)` 后手动
//! `schedule.run(Stage::Startup / Update, &mut world)`——与
//! `DefaultPlugins` 的注册方式一致（资源兜底插入已并入 Startup 首系统，
//! 两条路径无需差异配置）。

use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Access, Entity, Schedule, Stage};
use iris_engine::prelude::*;

use crate::logic::{GameState, MoveInput, Phase};

/// 适配：把纯逻辑类型接入引擎资源存储。
///
/// `logic.rs` 刻意保持零引擎依赖（只依赖 glam），而 `GameState` 需要作为
/// `World` 资源被系统读写——引擎的 `Resource` 是 `'static + Send + Sync`
/// 标记 trait，跨模块手动实现即可让纯类型「选择性接入」引擎，无须在
/// 逻辑核心里引入任何引擎路径（派生宏同理，但那会把 derive 写进 logic.rs）。
impl iris_core::resource::Resource for GameState {}

/// 世界资源：预生成的程序化音效（拾取 blip / 终局 boom）。
#[derive(Resource)]
pub struct GameAssets {
    /// 拾取音：880 Hz 正弦短音。
    pub pickup_clip: AudioClip,
    /// 胜负音：90 Hz 低频 boom。
    pub boom_clip: AudioClip,
}

/// 场景实体索引：HUD 刷新与金币动画需要跨帧定位具体实体。
///
/// 引擎 v1 没有「按名字查实体」的组件，插件以资源登记句柄
/// （见 ARCHITECTURE §8 本轮追加的引擎缺口清单）。
#[derive(Clone, Resource)]
pub struct GameEntities {
    pub player: Entity,
    pub camera: Entity,
    /// 金币实体，下标与 `GameState.pickups` 一一对应。
    pub coins: Vec<Entity>,
    pub score_text: Entity,
    pub time_text: Entity,
    pub banner_text: Entity,
}

/// 金币标记组件：携带 `GameState.pickups` 下标（供下游工具/查询识别）。
#[derive(Clone, Copy, Debug, Component)]
pub struct Coin(pub usize);

/// 对局时长（秒）。
pub const DURATION_SECS: f32 = 60.0;
/// 竞技场半边长（米）：地面 40 m、内壁贴 ±18 m。
pub const ARENA_HALF: f32 = 18.0;

/// 地面网格边长（米），略大于围墙外缘。
const GROUND_SIZE: f32 = 40.0;
/// 围墙：高 1 m、厚 1 m。
const WALL_HEIGHT: f32 = 1.0;
const WALL_THICKNESS: f32 = 1.0;
/// 玩家方块：边 0.7 × 0.9 × 0.7（半高 0.45，落地时贴地）。
const PLAYER_HALF_HEIGHT: f32 = 0.45;
/// 金币视觉基线高度与浮动幅度。
const COIN_BASE_Y: f32 = 0.35;
const COIN_BOB_AMPLITUDE: f32 = 0.12;
/// 已收集金币的隐藏高度（引擎 v1 无实体可见性开关，移出视野即隐藏）。
const HIDDEN_Y: f32 = -100.0;
/// HUD 布局（按 DefaultPlugins 缺省 1280×720 窗口手工排布）。
const HUD_WIDTH: f32 = 1280.0;

/// 收集小游戏插件：Startup 搭场景 + Update 推进逻辑与表现。
#[derive(Default)]
pub struct GamePlugin;

impl GamePlugin {
    /// 把本插件的全部系统注册到调度器。
    ///
    /// 与 `DefaultPlugins` 的编排方式一致（Startup 一次、Update 每帧）；
    /// 独立暴露是为了 headless 测试与离屏快照在不建 `App` / 窗口 / GPU
    /// 的前提下复用同一条推进路径。
    pub fn build_schedule(&self, schedule: &mut Schedule) {
        schedule
            .add_system(Stage::Startup, setup_resources)
            .add_system(Stage::Startup, build_scene)
            // Update 内的顺序即注册顺序（读写集冲突者分批保序）：
            // 逻辑推进 → 金币表现 → HUD → 层级传播
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<Input>()
                    .read::<Time>()
                    .read::<Audio>()
                    .read::<GameAssets>()
                    .read::<GameEntities>()
                    .write::<GameState>()
                    .write::<Transform>(),
                update_gameplay,
            )
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<GameState>()
                    .read::<Time>()
                    .read::<GameEntities>()
                    .write::<Transform>(),
                animate_coins,
            )
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<GameState>()
                    .read::<GameEntities>()
                    .write::<UiText>(),
                update_hud,
            )
            .add_system(Stage::Update, post_update_propagate);
    }
}

impl Plugin for GamePlugin {
    fn build(&self, app: &mut App) {
        self.build_schedule(&mut app.schedule);
    }
}

/// Startup 1：资源兜底插入。
///
/// 引擎资源（Store / 相机 / 灯光 / 泛光 / 音频）按 DefaultPlugins 的
/// ResourcePlugin 语义**缺席才插入**——窗口程序里 RenderPlugin /
/// AudioPlugin 已就位，不得覆盖（音频设备句柄是独占的）；headless /
/// 离屏路径则由这里补齐。游戏资源（`GameState` / `GameAssets`）由本插件
/// 独占，直接插入。
fn setup_resources(world: &mut World) {
    if !world.contains_resource::<MeshStore>() {
        world.insert_resource(MeshStore::new());
    }
    if !world.contains_resource::<MaterialStore>() {
        world.insert_resource(MaterialStore::new());
    }
    if !world.contains_resource::<TextureStore>() {
        world.insert_resource(TextureStore::new());
    }
    if !world.contains_resource::<Camera>() {
        world.insert_resource(Camera::default());
    }
    if !world.contains_resource::<DirectionalLight>() {
        // 微弱日光打底（点光是主光）；阴影照开，围墙阴影落向场地一侧
        world.insert_resource(DirectionalLight {
            intensity: 0.6,
            ..DirectionalLight::default()
        });
    }
    if !world.contains_resource::<BloomSettings>() {
        world.insert_resource(BloomSettings::default());
    }
    if !world.contains_resource::<Audio>() {
        world.insert_resource(Audio::new());
    }
    world.insert_resource(GameState::new(ARENA_HALF, DURATION_SECS));
    world.insert_resource(GameAssets {
        pickup_clip: sine(880.0, 0.15, 48_000),
        boom_clip: sine(90.0, 0.4, 48_000),
    });
}

/// Startup 2：搭建场景。
///
/// 玩家实体是根（位置 + 朝向由逻辑写入），**相机作为玩家子实体**
/// （局部 (0, 4.5, 8) + 绕 X 俯视 -25°，挂 `ActiveCamera`）——层级传播
/// 让相机逐帧跟随玩家；渲染器优先使用 `ActiveCamera` 实体。
fn build_scene(world: &mut World) {
    // ---- 网格与材质 ----
    let ground_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(GROUND_SIZE));
    let cube_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let coin_mesh = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, 24, 12));
    let ground_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.25, 0.34, 0.22, 1.0));
    let wall_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.46, 0.43, 0.40, 1.0));
    let player_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.25, 0.50, 0.95, 1.0));
    // 金币：金色金属 + 超过泛光阈值的自发光（HDR 泛光的光源）
    let coin_mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [1.0, 0.82, 0.25, 1.0],
        emissive: [1.8, 1.2, 0.25, 1.0],
        metallic: 1.0,
        roughness: 0.35,
        ..MaterialDef::default()
    });

    // ---- 地面 ----
    world.spawn((Transform::IDENTITY, ground_mesh, ground_mat));

    // ---- 四面围墙：内缘贴 ±ARENA_HALF，转角互相搭接 ----
    let wall_span = ARENA_HALF * 2.0 + WALL_THICKNESS * 2.0;
    for (position, scale) in [
        (
            Vec3::new(ARENA_HALF + WALL_THICKNESS * 0.5, WALL_HEIGHT * 0.5, 0.0),
            Vec3::new(WALL_THICKNESS, WALL_HEIGHT, wall_span),
        ),
        (
            Vec3::new(-ARENA_HALF - WALL_THICKNESS * 0.5, WALL_HEIGHT * 0.5, 0.0),
            Vec3::new(WALL_THICKNESS, WALL_HEIGHT, wall_span),
        ),
        (
            Vec3::new(0.0, WALL_HEIGHT * 0.5, ARENA_HALF + WALL_THICKNESS * 0.5),
            Vec3::new(wall_span, WALL_HEIGHT, WALL_THICKNESS),
        ),
        (
            Vec3::new(0.0, WALL_HEIGHT * 0.5, -ARENA_HALF - WALL_THICKNESS * 0.5),
            Vec3::new(wall_span, WALL_HEIGHT, WALL_THICKNESS),
        ),
    ] {
        world.spawn((
            Transform { position, scale, ..Transform::IDENTITY },
            cube_mesh,
            wall_mat,
        ));
    }

    // ---- 灯光：场地中央点光（cubemap 阴影）+ 微弱日光 ----
    world.spawn((
        Transform::from_xyz(0.0, 6.0, 0.0),
        PointLight {
            color: Vec3::new(1.0, 0.97, 0.88),
            intensity: 28.0,
            range: 40.0,
            shadows: true,
        },
    ));

    // ---- 玩家（蓝方块）+ 子实体相机 ----
    let player = world.spawn((Transform::from_xyz(0.0, PLAYER_HALF_HEIGHT, 0.0), cube_mesh, player_mat));
    // 俯视 -25°：绕 X 负角旋转后局部 -Z 前方指向斜下，恰好望向玩家
    let camera = world.spawn((
        Transform {
            position: Vec3::new(0.0, 4.5, 8.0),
            rotation: Quat::from_rotation_x(-25.0_f32.to_radians()),
            scale: Vec3::ONE,
        },
        ActiveCamera::default(),
    ));
    world.add_child(player, camera);

    // ---- 金币：压扁金球（scale (0.45, 0.15, 0.45)）----
    let coins: Vec<Entity> = GameState::spawn_positions()
        .into_iter()
        .enumerate()
        .map(|(i, (x, z))| {
            world.spawn((
                Transform::from_xyz(x, COIN_BASE_Y, z),
                coin_mesh,
                coin_mat,
                Coin(i),
            ))
        })
        .collect();

    // ---- HUD（屏幕像素坐标，文本实体的 UiRect 仅作定位载体）----
    world.spawn((
        UiRect { position: [24.0, 20.0], size: [0.0, 0.0] },
        UiText { string: "IRIS COLLECT".into(), scale: 3.0 },
        UiColor::default(),
    ));
    let score_text = world.spawn((
        UiRect { position: [24.0, 60.0], size: [0.0, 0.0] },
        UiText { string: format!("SCORE 0/{}", GameState::spawn_positions().len()), scale: 3.0 },
        UiColor([1.0, 0.85, 0.3, 1.0]),
    ));
    let time_width = format!("TIME {}", DURATION_SECS as i32).len() as f32 * 8.0 * 3.0;
    let time_text = world.spawn((
        UiRect { position: [HUD_WIDTH - time_width - 24.0, 20.0], size: [0.0, 0.0] },
        UiText { string: format!("TIME {}", DURATION_SECS as i32), scale: 3.0 },
        UiColor::default(),
    ));
    // 横幅：空串时不产生顶点（开局不可见），胜负时居中显示
    let banner_text = world.spawn((
        UiRect {
            position: [(HUD_WIDTH - 8.0 * 8.0 * 5.0) * 0.5, 290.0],
            size: [0.0, 0.0],
        },
        UiText { string: String::new(), scale: 5.0 },
        UiColor([1.0, 0.85, 0.3, 1.0]),
    ));

    world.insert_resource(GameEntities {
        player,
        camera,
        coins,
        score_text,
        time_text,
        banner_text,
    });
}

/// Update 1：读输入与时间 → 推进纯逻辑 → 写玩家实体 → 触发音效。
fn update_gameplay(world: &World) {
    let dt = world
        .try_resource::<Time>()
        .map(|time| time.delta_secs())
        .unwrap_or(0.0);
    let move_input = world
        .try_resource::<Input>()
        .map(|input| MoveInput {
            // 世界系 WASD：W = -Z（屏幕里侧），D = +X
            x: (input.key(KeyCode::KeyD) as i32 - input.key(KeyCode::KeyA) as i32) as f32,
            z: (input.key(KeyCode::KeyS) as i32 - input.key(KeyCode::KeyW) as i32) as f32,
            jump: input.key_pressed(KeyCode::Space),
        })
        .unwrap_or(MoveInput::IDLE);

    let Some(mut game) = world.try_resource_mut::<GameState>() else {
        return;
    };
    let was_playing = game.phase == Phase::Playing;
    let score_before = game.score;
    game.step(&move_input, dt);
    let picked = game.score > score_before;
    let finished = was_playing && game.phase != Phase::Playing;

    // 逻辑坐标 → 玩家实体：位置（y 抬半高贴地）+ 朝向偏航
    let Some(ents) = world.try_resource::<GameEntities>() else {
        return;
    };
    if let Some(mut transform) = world.get_mut::<Transform>(ents.player) {
        transform.position =
            Vec3::new(game.player.x, game.player.y + PLAYER_HALF_HEIGHT, game.player.z);
        transform.rotation = Quat::from_rotation_y(game.player.facing);
    }
    drop(ents);

    // 音效：拾取 blip / 终局 boom（引擎 v1 事件只能从独占系统发送，
    // 并行系统直接调用 Audio 资源，见 ARCHITECTURE §8 缺口清单）
    if picked || finished {
        if let (Some(assets), Some(audio)) =
            (world.try_resource::<GameAssets>(), world.try_resource::<Audio>())
        {
            if picked {
                audio.play(assets.pickup_clip.clone(), 0.5, false);
            }
            if finished {
                audio.play(assets.boom_clip.clone(), 0.7, false);
            }
        }
    }
}

/// Update 2：金币表现——绕 Y 自转 + 正弦浮动；已收集者沉底隐藏。
fn animate_coins(world: &World) {
    let Some(game) = world.try_resource::<GameState>() else {
        return;
    };
    let elapsed = world
        .try_resource::<Time>()
        .map(|time| time.elapsed_secs())
        .unwrap_or(0.0);
    let Some(ents) = world.try_resource::<GameEntities>() else {
        return;
    };
    for (i, &entity) in ents.coins.iter().enumerate() {
        let Some(pickup) = game.pickups.get(i) else {
            continue;
        };
        let Some(mut transform) = world.get_mut::<Transform>(entity) else {
            continue;
        };
        if pickup.collected {
            // 无可见性开关：把实体移出场景（每帧幂等地钉在 -100）
            transform.position.y = HIDDEN_Y;
            continue;
        }
        let phase = i as f32 * 0.7;
        transform.rotation = Quat::from_rotation_y(elapsed * 2.0 + phase);
        transform.position.y = COIN_BASE_Y + (elapsed * 2.5 + phase).sin() * COIN_BOB_AMPLITUDE;
    }
}

/// Update 3：HUD 刷新——SCORE / TIME 每帧重写，终局横幅。
fn update_hud(world: &World) {
    let Some(game) = world.try_resource::<GameState>() else {
        return;
    };
    let Some(ents) = world.try_resource::<GameEntities>() else {
        return;
    };
    if let Some(mut text) = world.get_mut::<UiText>(ents.score_text) {
        text.string = format!("SCORE {}/{}", game.score, game.pickups.len());
    }
    if let Some(mut text) = world.get_mut::<UiText>(ents.time_text) {
        text.string = format!("TIME {}", game.time_left.ceil().max(0.0) as i32);
    }
    if let Some(mut text) = world.get_mut::<UiText>(ents.banner_text) {
        text.string = match game.phase {
            Phase::Won => "YOU WIN!".into(),
            Phase::Lost => "TIME UP".into(),
            Phase::Playing => String::new(),
        };
    }
}

/// Update 4（引擎缺口绕行）：层级传播。
///
/// `DefaultPlugins` 的 `HierarchyPlugin` 把传播系统注册在本插件**之前**
/// （插件注册顺序即执行顺序），而引擎没有系统排序 API（无 before/after
/// 约束）——本插件写完玩家 `Transform` 后当帧已无传播可依赖，相机
/// （玩家子实体）会滞后一帧。这里在系统链末尾补一次幂等的
/// `propagate_transforms`：传播是纯函数重建，同帧跑两次结果一致，
/// 代价只是约 20 个实体的一次多余矩阵复合。
fn post_update_propagate(world: &mut World) {
    propagate_transforms(world);
}
