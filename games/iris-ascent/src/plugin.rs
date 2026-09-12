//! [`AscentPlugin`]：把纯逻辑核心（`crate::logic`）接到 Iris 引擎上。
//!
//! 职责划分（iris-game / iris-bastion 同款分层，载荷换成本轮 R28–R31
//! 的四个新能力）：
//! - **Startup**：`setup_resources`（兜底插入引擎资源 + 游戏资源）→
//!   `build_scene`（关卡全部代码生成：平台 / 大球 / 跳板辉光 / 检查点
//!   旗 / 宝石 / 山顶信标 / **22 个 LodMesh 远景装饰** / 三层玩家结构 /
//!   HUD / FX 池——静态内容启动 spawn 一次即可，不进对象池；唯一池是
//!   一次性粒子发射器的 FX 池）；
//! - **Update**：`run_ascent`（读 `Input` / `Time` → **相机相对**输入
//!   翻译 → [`AscentState::step`]（内部走引擎
//!   [`move_and_slide_statics`]——物理即角色控制器）→ 写玩家根实体 /
//!   模型朝向 / **动画状态机**（移动 ↔ 静止 `request_state`，双向
//!   0.15 s crossfade）→ `take_events` 消费 footstep 事件轨道 → 事件
//!   触发**空间音频**（听者跟随相机）→ 复活帧请求相机吸附；平滑追随
//!   由引擎 `update_smooth_follow` 系统（随后注册）完成）
//!   → `trigger_ascent_fx`（事件 → FX 池粒子 burst）→
//!   `update_ascent_visuals`（宝石自转浮动 / 检查点旗换色 / 信标自转）
//!   → `update_ascent_hud`（宝石 n/N / 检查点 / 坠落计数 / 横幅）。
//! - **层级传播**：交给 `HierarchyPlugin`（PostUpdate）；**粒子推进**：
//!   `ParticlePlugin`（PostUpdate、传播之后）；**动画推进**：
//!   `AnimationPlugin`（Update、本插件系统之前注册）——窗口路径由
//!   `DefaultPlugins` 注册，headless 测试 / 离屏快照需自行 build（与
//!   iris-bastion 约定同款），本插件不重复注册。
//!
//! # 三层玩家结构（iris-game 先例）
//!
//! 根实体只承载逻辑位置（rotation 恒恒等——朝向写根实体会让相机子实体
//! 绕玩家公转）；模型子实体挂双剪辑 [`AnimationPlayer`]（朝向 / 动画
//! 都写它，二者不冲突——朝向是 yaw、动画剪辑驱动模型内部节点）；相机
//! 子实体挂引擎 [`SmoothFollow`] 组件（打磨 IV 摩擦④闭合：平滑世界位
//! 记忆在组件里，局部位置由引擎系统逐帧写 = 平滑世界位经父系逆变换，
//! 层级传播还原出平滑世界位；复活瞬移调 `snap()` 直接吸附，不做长距离
//! 甩镜）。模型实体在装配时调用 [`AnimationPlayer::capture_rest`]
//! （打磨 IV 摩擦①闭合：双剪辑**不再强制路径对齐**——idle 只写 scale、
//! walk 只写 rotation，crossfade 混合期「仅源有」的通道平滑淡回基姿势）。
//!
//! # 角色模型（R28 主载荷）
//!
//! [`MODEL_PATH`]（`assets/ascent_bot.glb`）由 iris-engine 的
//! `gen_asset` 示例程序化生成：小机器人（盒身 + 眼），两剪辑——
//! `idle`（原地压缩伸展呼吸，2 s 循环）与 `walk`（前倾 + 左右摇摆，
//! 0.6 s 循环）。加载失败降级为程序化几何 + 代码构建的同构剪辑
//! （状态机 / crossfade / 事件轨道行为完全一致，游戏仍完整可玩）。
//!
//! 本插件只使用引擎公开 API（dogfooding）：不依赖引擎内部路径。

use std::path::{Path, PathBuf};

use glam::{Quat, Vec2, Vec3};

use iris_core::prelude::{Access, Entity, Name, Schedule, Stage};
use iris_engine::prelude::*;

use crate::logic::{
    AscentEvent, AscentInput, AscentState, CHECKPOINTS, GEMS, HALF_HEIGHT, LAUNCH_SPEED, PADS,
    PLATFORMS, Phase, SPAWN_FOOT, SPHERE_CENTER, SPHERE_RADIUS, SUMMIT, SUMMIT_ZONE_CENTER,
};

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 角色模型路径（仓库相对，经 [`asset_path`] 解析）。
pub const MODEL_PATH: &str = "assets/ascent_bot.glb";
/// 状态机状态下标：静止（idle 剪辑）。
pub const STATE_IDLE: usize = 0;
/// 状态机状态下标：移动（walk 剪辑）。
pub const STATE_WALK: usize = 1;
/// 状态机双向过渡时长（秒）：idle ↔ walk 的 crossfade。
pub const STATE_BLEND: f32 = 0.15;
/// FX（一次性粒子发射器）池容量：环形复用（bastion 同款）。
pub const POOL_FX: usize = 12;
/// LOD 切换距离（米）：装饰实体到相机眼点的欧氏距离。
pub const LOD_SWITCH_DISTANCE: f32 = 60.0;
/// 远景装饰数量（山景球 8 + 巨柱 8 + 浮岛 6）。
pub const DECOR_COUNT: usize = 22;
/// 空间音频距离参考（米）：第三人称相机距角色 ~9.5 m，按此尺度放大。
pub const ASCENT_REFERENCE_DISTANCE: f32 = 12.0;
/// 第三人称相机偏移（根实体局部；根无旋转 → 世界系方向恒定）。
pub const CAMERA_OFFSET: Vec3 = Vec3::new(0.0, 4.2, 8.5);
/// 相机俯仰（弧度，绕 X 负角 = 望向玩家脚下）。
const CAMERA_PITCH: f32 = -25.0_f32.to_radians();
/// 相机平滑追随的刚度（1/秒）：[`SmoothFollow`] 的指数趋近系数。
const CAMERA_STIFFNESS: f32 = 6.0;
/// 坠落横幅持续（秒）。
const FALL_BANNER_SECS: f32 = 2.0;
/// HUD 布局（按 DefaultPlugins 缺省 1280×720 窗口手工排布）。
const HUD_WIDTH: f32 = 1280.0;
/// HUD 实体名（`Name` 组件，按名定位用）。
pub const GEMS_TEXT_NAME: &str = "ascent/hud/gems";
pub const CHECKPOINT_TEXT_NAME: &str = "ascent/hud/checkpoint";
pub const FALLS_TEXT_NAME: &str = "ascent/hud/falls";
pub const BANNER_TEXT_NAME: &str = "ascent/hud/banner";

// ---------------------------------------------------------------------------
// 插件侧资源
// ---------------------------------------------------------------------------

/// 适配：把纯逻辑类型接入引擎资源存储（逻辑核心零引擎依赖，跨模块手动
/// 实现 `Resource` 即可「选择性接入」）。
impl iris_core::resource::Resource for AscentState {}

/// 坠落横幅剩余显示时间（秒；`run_ascent` 置位、HUD 递减清零）。
#[derive(Clone, Copy, Debug, Default, Resource)]
pub struct BannerTimer(pub f32);

/// 预生成的程序化音效（全部确定性、Startup 一次生成）。
#[derive(Clone, Resource)]
pub struct AscentAssets {
    /// 起跳音：660 Hz 短 blip。
    pub jump: AudioClip,
    /// 落地音：110 Hz 低频闷响。
    pub land: AudioClip,
    /// 宝石音：1318 Hz 亮 ping。
    pub gem: AudioClip,
    /// 检查点音：523 / 784 Hz 上行双音。
    pub checkpoint: AudioClip,
    /// 跳板弹射音：宽带噪声 whoosh。
    pub launch: AudioClip,
    /// 复活音：392 / 262 Hz 下行双音。
    pub respawn: AudioClip,
    /// 胜利音：523 / 659 / 784 Hz 上行三连音。
    pub victory: AudioClip,
    /// 脚步音：短噪声（事件轨道驱动）。
    pub footstep: AudioClip,
}

/// 场景实体索引：每帧热路径的句柄资源。HUD 文本实体经 `Name` +
/// `World::entity_named` 按名定位（低频路径）。
#[derive(Clone, Resource)]
pub struct AscentEntities {
    /// 玩家根实体：只承载逻辑位置（rotation 恒恒等）。
    pub player_root: Entity,
    /// 模型子实体：挂 [`AnimationPlayer`]，朝向（yaw）写它的 rotation。
    pub player_model: Entity,
    /// 相机子实体（`ActiveCamera` + `SmoothFollow`）。
    pub camera: Entity,
    /// 山顶信标（自旋表现）。
    pub beacon: Entity,
    /// 宝石实体（下标 = [`GEMS`] 下标；收集后切 `Visibility::HIDDEN`）。
    pub gems: Vec<Entity>,
    /// 检查点旗实体（下标 = [`CHECKPOINTS`] 下标；激活换材质）。
    pub flags: Vec<Entity>,
    /// 旗帜材质 `[未激活灰, 已激活青]`。
    pub flag_materials: [MaterialHandle; 2],
}

/// FX 池：一次性粒子发射器的环形复用槽位（bastion 同款：槽位预挂禁用
/// 发射器作组件载体，触发 = 参数全字段覆写 + `reset` 重新上膛）。
#[derive(Resource)]
pub struct FxPool {
    /// 环形槽位（触发顺序轮转）。
    pub slots: Vec<Entity>,
    next: usize,
}

impl FxPool {
    /// 取下一个槽位（环形推进）。
    fn next_slot(&mut self) -> Entity {
        let entity = self.slots[self.next];
        self.next = (self.next + 1) % self.slots.len();
        entity
    }
}

// ---------------------------------------------------------------------------
// 插件
// ---------------------------------------------------------------------------

/// 平台跳跃插件：Startup 代码生成关卡 + FX 池预热，Update 推进逻辑与
/// 表现。关卡全部来自代码（无需场景文件），`Default` 即可用。
#[derive(Debug, Clone)]
pub struct AscentPlugin {
    /// Startup 时加载的角色 GLB 路径（[`asset_path`] 解析；测试可注入）。
    pub model_path: PathBuf,
}

impl Default for AscentPlugin {
    fn default() -> Self {
        Self {
            model_path: asset_path(MODEL_PATH),
        }
    }
}

impl AscentPlugin {
    /// 以指定角色模型路径构造（测试注入临时资产 / 显式给默认路径）。
    pub fn new(model_path: impl Into<PathBuf>) -> Self {
        Self {
            model_path: model_path.into(),
        }
    }

    /// 把本插件的全部系统注册到调度器。
    ///
    /// 与 `DefaultPlugins` 的编排方式一致（Startup 一次、Update 每帧）；
    /// 独立暴露是为了 headless 测试与离屏快照在不建 `App` / 窗口 / GPU
    /// 的前提下复用同一条推进路径。**层级 / 粒子 / 动画不在此注册**：
    /// 调用方按窗口路径的编排补上 `HierarchyPlugin`（PostUpdate）、
    /// `ParticlePlugin`（PostUpdate、层级之后）与 `AnimationPlugin`
    /// （Update、先于本插件注册）。
    pub fn build_schedule(&self, schedule: &mut Schedule) {
        let model_path = self.model_path.clone();
        schedule
            .add_system(Stage::Startup, setup_resources)
            .add_system(Stage::Startup, move |world: &mut World| {
                build_scene(world, &model_path)
            })
            // Update 内的顺序即注册顺序：逻辑 + 输入 + 音频 + 相机 +
            // 状态机 → 平滑追随（引擎系统，须在玩家位置写入之后）→
            // FX 粒子触发 → 表现 → HUD
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<Input>()
                    .read::<Time>()
                    .read::<Audio>()
                    .read::<AscentAssets>()
                    .read::<AscentEntities>()
                    .read::<GlobalTransform>()
                    .write::<AscentState>()
                    .write::<Transform>()
                    .write::<AnimationPlayer>()
                    .write::<SmoothFollow>()
                    .write::<BannerTimer>(),
                run_ascent,
            )
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<Time>()
                    .write::<SmoothFollow>()
                    .write::<Transform>(),
                update_smooth_follow,
            )
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<AscentState>()
                    .read::<FxPool>()
                    .write::<Transform>()
                    .write::<ParticleEmitter>(),
                trigger_ascent_fx,
            )
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<AscentState>()
                    .read::<Time>()
                    .read::<AscentEntities>()
                    .write::<Transform>()
                    .write::<Visibility>()
                    .write::<MaterialHandle>(),
                update_ascent_visuals,
            )
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<AscentState>()
                    .read::<Time>()
                    .write::<UiText>()
                    .write::<BannerTimer>(),
                update_ascent_hud,
            );
    }
}

impl Plugin for AscentPlugin {
    fn build(&self, app: &mut App) {
        self.build_schedule(&mut app.schedule);
    }
}

/// Startup 1：资源兜底插入（bastion 同款「缺席才插入」语义）。
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
    if !world.contains_resource::<BloomSettings>() {
        world.insert_resource(BloomSettings::default());
    }
    if !world.contains_resource::<Audio>() {
        world.insert_resource(Audio::new());
    }
    if !world.contains_resource::<Viewport>() {
        world.insert_resource(Viewport::default());
    }
    world.insert_resource(AscentState::new());
    world.insert_resource(BannerTimer::default());
    if let Some(audio) = world.try_resource::<Audio>() {
        audio.set_reference_distance(ASCENT_REFERENCE_DISTANCE);
    }
    world.insert_resource(AscentAssets {
        jump: sine(660.0, 0.08, 48_000),
        land: sine(110.0, 0.14, 48_000),
        gem: sine(1318.0, 0.12, 48_000),
        checkpoint: two_tone(523.0, 784.0, 48_000),
        launch: noise(0.3, 48_000, 0xA5C3_1177),
        respawn: two_tone(392.0, 262.0, 48_000),
        victory: three_tone(48_000),
        footstep: noise(0.045, 48_000, 0xF00D_0B0A),
    });
}

/// 双音（上行 = 检查点激活；下行 = 复活告警）：两段程序化正弦 + 间隙
/// 拼接（bastion 的 `down_beep` 同款做法，方向参数化）。
fn two_tone(first_hz: f32, second_hz: f32, sample_rate: u32) -> AudioClip {
    let tone_a = sine(first_hz, 0.1, sample_rate);
    let tone_b = sine(second_hz, 0.18, sample_rate);
    let gap = (0.04 * sample_rate as f32) as usize;
    let silence =
        AudioClip::from_interleaved(vec![0.0; gap], 1, sample_rate).expect("程序化参数恒合法");
    AudioClip::concat(&[&tone_a, &silence, &tone_b]).expect("同参数生成的剪辑布局必然一致")
}

/// 胜利三连音：523 / 659 / 784 Hz 上行琶音。
fn three_tone(sample_rate: u32) -> AudioClip {
    let a = sine(523.0, 0.12, sample_rate);
    let b = sine(659.0, 0.12, sample_rate);
    let c = sine(784.0, 0.26, sample_rate);
    AudioClip::concat(&[&a, &b, &c]).expect("同参数生成的剪辑布局必然一致")
}

// ---------------------------------------------------------------------------
// Startup 2：代码生成关卡
// ---------------------------------------------------------------------------

/// Startup 2：代码生成关卡 + 三层玩家 + 远景装饰 + HUD + FX 池。
///
/// 静态内容（平台 / 大球 / 宝石 / 装饰……）启动 spawn 一次即可——静态
/// 内容不进对象池；唯一池是 FX 发射器池（一次性 burst 复用）。
fn build_scene(world: &mut World, model_path: &Path) {
    build_level_geometry(world);
    build_decor(world);
    let (player_root, player_model, camera) = build_player(world, model_path);
    let (gems, flags, flag_materials, beacon) = build_pickups_and_flags(world);
    build_fx_pool(world);
    build_hud(world);
    world.insert_resource(AscentEntities {
        player_root,
        player_model,
        camera,
        beacon,
        gems,
        flags,
        flag_materials,
    });
}

/// 关卡几何：平台 / 大球 / 跳板辉光 / 山顶信标（视觉尺寸与逻辑静态体
/// 同源：同一份 [`PLATFORMS`] / [`SUMMIT`] / [`SPHERE_CENTER`] 常量）。
fn build_level_geometry(world: &mut World) {
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let sphere_mesh =
        world
            .resource_mut::<MeshStore>()
            .add(MeshData::uv_sphere(SPHERE_RADIUS, 48, 24));
    let mut stores = world.resource_mut::<MaterialStore>();
    let platform_mat = stores.add(MaterialDef::solid(0.42, 0.38, 0.52, 1.0));
    let step_mat = stores.add(MaterialDef::solid(0.55, 0.48, 0.62, 1.0));
    let pad_base_mat = stores.add(MaterialDef::solid(0.2, 0.24, 0.34, 1.0));
    let pad_glow_mat = stores.add(MaterialDef {
        base_color: [0.3, 0.9, 1.0, 1.0],
        emissive: [0.5, 2.2, 2.6, 1.0],
        metallic: 0.0,
        roughness: 0.4,
        ..MaterialDef::default()
    });
    let summit_mat = stores.add(MaterialDef::solid(0.75, 0.62, 0.3, 1.0));
    let sphere_mat = stores.add(MaterialDef::solid(0.3, 0.44, 0.5, 1.0));
    drop(stores);

    for (index, platform) in PLATFORMS.iter().enumerate() {
        let is_pad = PADS.iter().any(|pad| pad.center == platform.center);
        // 台阶段（细长条）用浅色区分，跳板台用暗底 + 辉光板
        let is_step = platform.half.z < 1.0;
        let material = if is_pad {
            pad_base_mat
        } else if is_step {
            step_mat
        } else {
            platform_mat
        };
        world.spawn((
            Name(format!("ascent/platform/{index}")),
            Transform {
                position: platform.center,
                scale: platform.half * 2.0,
                ..Transform::IDENTITY
            },
            cube,
            material,
        ));
        if is_pad {
            // 跳板辉光板：顶面上方 3 cm 的薄板（纯视觉，无碰撞）
            let top = platform.center.y + platform.half.y;
            world.spawn((
                Name(format!("ascent/pad-glow/{index}")),
                Transform {
                    position: Vec3::new(platform.center.x, top + 0.03, platform.center.z),
                    scale: Vec3::new(platform.half.x * 1.7, 0.06, platform.half.z * 1.7),
                    ..Transform::IDENTITY
                },
                cube,
                pad_glow_mat,
            ));
        }
    }

    // 山顶台（金色）+ 信标（自发光长棱柱 + 点光）
    world.spawn((
        Name("ascent/summit".into()),
        Transform {
            position: SUMMIT.center,
            scale: SUMMIT.half * 2.0,
            ..Transform::IDENTITY
        },
        cube,
        summit_mat,
    ));
    let beacon_mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [1.0, 0.9, 0.4, 1.0],
        emissive: [2.6, 2.0, 0.6, 1.0],
        metallic: 0.0,
        roughness: 0.3,
        ..MaterialDef::default()
    });
    world.spawn((
        Name("ascent/beacon".into()),
        Transform {
            position: Vec3::new(
                SUMMIT_ZONE_CENTER.x,
                SUMMIT.top() + 2.5,
                SUMMIT_ZONE_CENTER.z - 2.5,
            ),
            scale: Vec3::new(0.6, 5.0, 0.6),
            ..Transform::IDENTITY
        },
        cube,
        beacon_mat,
    ));
    world.spawn((
        Name("ascent/beacon-light".into()),
        Transform::from_xyz(
            SUMMIT_ZONE_CENTER.x,
            SUMMIT.top() + 5.5,
            SUMMIT_ZONE_CENTER.z - 2.5,
        ),
        PointLight {
            color: Vec3::new(1.0, 0.85, 0.5),
            intensity: 12.0,
            range: 30.0,
            shadows: true,
        },
    ));

    // 大球坡面
    world.spawn((
        Name("ascent/sphere".into()),
        Transform {
            position: SPHERE_CENTER,
            ..Transform::IDENTITY
        },
        sphere_mesh,
        sphere_mat,
    ));
}

/// 远景装饰（R30 载荷）：山景球 ×8 + 巨柱 ×8 + 浮岛剪影 ×6，全部挂
/// [`LodMesh`]（高 / 低模 + [`LOD_SWITCH_DISTANCE`] 切换）——近距离
/// 全高模、拉远后三角形数显著下降（headless 断言量化）。
fn build_decor(world: &mut World) {
    let mountain_high = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, 48, 24));
    let mountain_low = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, 10, 6));
    let pillar_high = world
        .resource_mut::<MeshStore>()
        .add(MeshData::cylinder(1.0, 1.0, 48));
    let pillar_low = world
        .resource_mut::<MeshStore>()
        .add(MeshData::cylinder(1.0, 1.0, 8));
    let mut stores = world.resource_mut::<MaterialStore>();
    let mountain_mat = stores.add(MaterialDef::solid(0.24, 0.3, 0.38, 1.0));
    let pillar_mat = stores.add(MaterialDef::solid(0.3, 0.27, 0.33, 1.0));
    let island_mat = stores.add(MaterialDef::solid(0.35, 0.42, 0.5, 1.0));
    drop(stores);

    // 山景球：(x, z, 半径)——半沉在关卡下方，剪影群
    for (index, (x, z, r)) in [
        (-52.0, -12.0, 22.0),
        (55.0, -20.0, 26.0),
        (-60.0, -52.0, 24.0),
        (62.0, -66.0, 20.0),
        (-58.0, -92.0, 25.0),
        (66.0, -104.0, 22.0),
        (-48.0, -122.0, 20.0),
        (50.0, -128.0, 24.0),
    ]
    .into_iter()
    .enumerate()
    {
        world.spawn((
            Name(format!("ascent/decor/mountain/{index}")),
            Transform {
                position: Vec3::new(x, -r * 0.45, z),
                scale: Vec3::new(r, r * 0.8, r),
                ..Transform::IDENTITY
            },
            LodMesh {
                high: mountain_high,
                low: mountain_low,
                switch_distance: LOD_SWITCH_DISTANCE,
            },
            mountain_mat,
        ));
    }
    // 巨柱：(x, z, 半径, 高)——从关卡下方拔起
    for (index, (x, z, radius, height)) in [
        (-42.0, -18.0, 2.4, 44.0),
        (40.0, -32.0, 3.0, 56.0),
        (-45.0, -44.0, 2.0, 38.0),
        (46.0, -58.0, 2.6, 50.0),
        (-44.0, -74.0, 3.2, 62.0),
        (48.0, -88.0, 2.2, 42.0),
        (-42.0, -102.0, 2.8, 54.0),
        (44.0, -116.0, 2.4, 46.0),
    ]
    .into_iter()
    .enumerate()
    {
        world.spawn((
            Name(format!("ascent/decor/pillar/{index}")),
            Transform {
                position: Vec3::new(x, height * 0.5 - 12.0, z),
                scale: Vec3::new(radius, height, radius),
                ..Transform::IDENTITY
            },
            LodMesh {
                high: pillar_high,
                low: pillar_low,
                switch_distance: LOD_SWITCH_DISTANCE,
            },
            pillar_mat,
        ));
    }
    // 浮岛剪影：(x, y, z, 半径)——压扁的球
    for (index, (x, y, z, r)) in [
        (-30.0, 14.0, -24.0, 5.0),
        (32.0, 22.0, -46.0, 6.0),
        (-34.0, 30.0, -66.0, 5.5),
        (36.0, 40.0, -86.0, 6.5),
        (-32.0, 50.0, -104.0, 5.0),
        (30.0, 56.0, -122.0, 6.0),
    ]
    .into_iter()
    .enumerate()
    {
        world.spawn((
            Name(format!("ascent/decor/island/{index}")),
            Transform {
                position: Vec3::new(x, y, z),
                scale: Vec3::new(r, r * 0.35, r),
                ..Transform::IDENTITY
            },
            LodMesh {
                high: mountain_high,
                low: mountain_low,
                switch_distance: LOD_SWITCH_DISTANCE,
            },
            island_mat,
        ));
    }
}

/// 三层玩家结构：根（逻辑位置）→ 模型（双剪辑动画 + 朝向）→ 相机
/// （`SmoothFollow` 平滑追随 + 固定俯仰）。模型优先加载 [`MODEL_PATH`]
/// GLB（双剪辑 + 状态机 + footstep 事件轨道 + `capture_rest` 基姿势）；
/// 失败降级为程序化几何 + 代码剪辑（同构）。
fn build_player(world: &mut World, model_path: &Path) -> (Entity, Entity, Entity) {
    // 根：只承载逻辑位置
    let player_root = world.spawn((
        Name("ascent/player".into()),
        Transform::from_xyz(SPAWN_FOOT.x, SPAWN_FOOT.y + HALF_HEIGHT, SPAWN_FOOT.z),
    ));
    // 模型：GLB 优先，降级程序化（两者都把 AnimationPlayer 挂在模型根）
    let player_model = match spawn_gltf_instance(world, model_path) {
        Ok(instance) if instance.clips.len() >= 2 => {
            let root = instance.roots[0];
            let mut player = instance.player();
            configure_state_machine(&mut player);
            // 事件轨道：GLB 解析不产事件，由 API 侧注入 walk 的脚步标记
            // （R59 起剪辑数据为 Arc 共享形态——clips_mut 写时复制出本
            // 实例私有副本，注入不影响注册表 / 其他实例）
            if let Some(walk) = player.clips_mut().get_mut(1) {
                walk.events = vec![
                    AnimationEvent {
                        time: 0.15,
                        name: "footstep".into(),
                    },
                    AnimationEvent {
                        time: 0.45,
                        name: "footstep".into(),
                    },
                ];
            }
            // 基姿势捕获（打磨 IV 摩擦①）：实体已全部生成、动画尚未
            // 推进——此刻的 Transform 就是「未播放动画时的初始值」，
            // crossfade「仅源有」的通道以此平滑淡回
            player.capture_rest(world);
            world.insert(root, player);
            world.insert(root, Name("ascent/player-model".into()));
            world.add_child(player_root, root);
            // 模型原点在脚底：胶囊中心 - 半高
            if let Some(mut transform) = world.get_mut::<Transform>(root) {
                transform.position = Vec3::new(0.0, -HALF_HEIGHT, 0.0);
            }
            root
        }
        Ok(_) | Err(_) => {
            log::warn!(
                "AscentPlugin：角色模型 {} 不可用（缺失或剪辑不足）——\
                 降级为程序化几何 + 代码剪辑（游戏行为不受影响）",
                model_path.display()
            );
            let model = fallback_robot(world);
            world.add_child(player_root, model);
            if let Some(mut transform) = world.get_mut::<Transform>(model) {
                transform.position = Vec3::new(0.0, -HALF_HEIGHT, 0.0);
            }
            model
        }
    };
    // 相机子实体：固定偏移 + 俯视；平滑追随由引擎 SmoothFollow 系统逐帧
    // 写局部位置（打磨 IV 摩擦④：游戏侧 CameraRig 绕行已删除）
    let camera = world.spawn((
        Name("ascent/player-camera".into()),
        Transform {
            position: CAMERA_OFFSET,
            rotation: Quat::from_rotation_x(CAMERA_PITCH),
            scale: Vec3::ONE,
        },
        ActiveCamera {
            fov_y_radians: 55.0_f32.to_radians(),
            ..ActiveCamera::default()
        },
        SmoothFollow::new(player_root, CAMERA_OFFSET, CAMERA_STIFFNESS),
    ));
    world.add_child(player_root, camera);
    (player_root, player_model, camera)
}

/// 给播放器装配 idle ↔ walk 状态机（双向 0.15 s crossfade）。
fn configure_state_machine(player: &mut AnimationPlayer) {
    player.states = vec![
        AnimationState {
            clip: STATE_IDLE,
            looping: true,
            speed: 1.0,
        },
        AnimationState {
            clip: STATE_WALK,
            looping: true,
            speed: 1.0,
        },
    ];
    player.edges = vec![
        AnimationEdge {
            from: STATE_IDLE,
            to: STATE_WALK,
            duration: STATE_BLEND,
        },
        AnimationEdge {
            from: STATE_WALK,
            to: STATE_IDLE,
            duration: STATE_BLEND,
        },
    ];
}

/// 降级角色：程序化盒身 + 眼，代码构建的同构双剪辑（idle 呼吸缩放 /
/// walk 前倾摇摆 + footstep 事件轨道，各只一条通道）——状态机 /
/// crossfade / 事件轨道 / rest 基姿势行为与 GLB 路径完全一致。
fn fallback_robot(world: &mut World) -> Entity {
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let body_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.2, 0.65, 0.6, 1.0));
    let eye_mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [0.1, 0.3, 0.32, 1.0],
        emissive: [0.3, 1.8, 1.8, 1.0],
        metallic: 0.0,
        roughness: 0.3,
        ..MaterialDef::default()
    });
    // 包装实体：朝向写它（内部身体被动画覆写旋转 / 缩放，互不干扰）
    let wrapper = world.spawn((Name("ascent/player-model".into()), Transform::IDENTITY));
    let body = world.spawn((
        Name("ascent/player-model/body".into()),
        Transform {
            position: Vec3::new(0.0, 0.9, 0.0),
            scale: Vec3::new(0.7, 0.8, 0.5),
            ..Transform::IDENTITY
        },
        cube,
        body_mat,
    ));
    let eye = world.spawn((
        Name("ascent/player-model/eye".into()),
        Transform {
            position: Vec3::new(0.0, 0.15, -0.27),
            scale: Vec3::new(0.24, 0.1, 0.06),
            ..Transform::IDENTITY
        },
        cube,
        eye_mat,
    ));
    world.add_child(wrapper, body);
    world.add_child(body, eye);

    // 代码剪辑（与 GLB 同构，各只声明真正驱动的通道——打磨 IV 摩擦①
    // 闭合后无需恒值通道对齐）：idle 呼吸（缩放，围绕建模尺寸 0.7 ×
    // 0.8 × 0.5 压缩伸展）；walk 前倾 10° + 左右摇摆 ±6°（旋转）。
    // rest 捕获的基姿势 = body 初始 TRS（scale (0.7, 0.8, 0.5)、旋转
    // 恒等）= idle 的首末关键帧，过渡完成后「仅源有」通道恰停在基姿势
    let lean_sway = |sway_deg: f32| {
        let q = Quat::from_rotation_z(sway_deg.to_radians())
            * Quat::from_rotation_x(10.0_f32.to_radians());
        [q.x, q.y, q.z, q.w]
    };
    let breathe = 0.93f32;
    let mut idle = AnimationClip {
        name: "idle".into(),
        duration: 2.0,
        ..AnimationClip::default()
    };
    idle.channels = vec![AnimationChannel {
        node: 0,
        path: AnimationPath::Scale,
        interpolation: AnimationInterpolation::Linear,
        times: vec![0.0, 1.0, 2.0],
        values: vec![0.7, 0.8, 0.5, 0.7, 0.8 * breathe, 0.5, 0.7, 0.8, 0.5],
    }];
    let mut walk = AnimationClip {
        name: "walk".into(),
        duration: 0.6,
        ..AnimationClip::default()
    };
    walk.channels = vec![AnimationChannel {
        node: 0,
        path: AnimationPath::Rotation,
        interpolation: AnimationInterpolation::Linear,
        times: vec![0.0, 0.3, 0.6],
        values: [lean_sway(6.0), lean_sway(-6.0), lean_sway(6.0)].concat(),
    }];
    walk.events = vec![
        AnimationEvent {
            time: 0.15,
            name: "footstep".into(),
        },
        AnimationEvent {
            time: 0.45,
            name: "footstep".into(),
        },
    ];
    let mut player = AnimationPlayer::new(vec![idle, walk], vec![Some(body)]);
    configure_state_machine(&mut player);
    // 基姿势捕获：body 已 spawn、动画未推进（语义同 GLB 路径）
    player.capture_rest(world);
    world.insert(wrapper, player);
    wrapper
}

/// 宝石 + 检查点旗 + 山顶信标实体索引。
fn build_pickups_and_flags(
    world: &mut World,
) -> (Vec<Entity>, Vec<Entity>, [MaterialHandle; 2], Entity) {
    let gem_mesh = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(0.28, 12, 8));
    let pole_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let gem_mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [1.0, 0.82, 0.25, 1.0],
        emissive: [1.8, 1.2, 0.25, 1.0],
        metallic: 0.6,
        roughness: 0.3,
        ..MaterialDef::default()
    });
    let mut stores = world.resource_mut::<MaterialStore>();
    let flag_off = stores.add(MaterialDef::solid(0.45, 0.45, 0.5, 1.0));
    let flag_on = stores.add(MaterialDef {
        base_color: [0.3, 0.95, 0.9, 1.0],
        emissive: [0.4, 2.2, 2.0, 1.0],
        metallic: 0.0,
        roughness: 0.4,
        ..MaterialDef::default()
    });
    let pole_mat = stores.add(MaterialDef::solid(0.7, 0.7, 0.75, 1.0));
    drop(stores);

    let gems = GEMS
        .iter()
        .enumerate()
        .map(|(index, pos)| {
            world.spawn((
                Name(format!("ascent/gem/{index}")),
                Transform::from_xyz(pos.x, pos.y, pos.z),
                gem_mesh,
                gem_mat,
                Visibility::VISIBLE,
            ))
        })
        .collect();

    let mut flags = Vec::new();
    for (index, checkpoint) in CHECKPOINTS.iter().enumerate() {
        let pole = world.spawn((
            Name(format!("ascent/checkpoint/{index}/pole")),
            Transform {
                position: Vec3::new(
                    checkpoint.respawn_foot.x,
                    checkpoint.respawn_foot.y + 1.1,
                    checkpoint.respawn_foot.z,
                ),
                scale: Vec3::new(0.12, 2.2, 0.12),
                ..Transform::IDENTITY
            },
            pole_mesh,
            pole_mat,
        ));
        // 旗面挂在杆顶：激活时换发光材质（组件 spawn 时就位，Update 经
        // get_mut 翻转）
        let flag = world.spawn((
            Name(format!("ascent/checkpoint/{index}/flag")),
            Transform {
                position: Vec3::new(0.3, 2.0, 0.0),
                scale: Vec3::new(0.6, 0.38, 0.06),
                ..Transform::IDENTITY
            },
            pole_mesh,
            flag_off,
        ));
        world.add_child(pole, flag);
        flags.push(flag);
    }
    let beacon = world
        .entity_named("ascent/beacon")
        .expect("信标应在 build_level_geometry 中生成");
    (gems, flags, [flag_off, flag_on], beacon)
}

/// FX 池：12 个纯发射器槽位，预挂禁用发射器（bastion 同款）。
fn build_fx_pool(world: &mut World) {
    let mut slots = Vec::new();
    for slot in 0..POOL_FX {
        let mut emitter = ParticleEmitter::new(ParticleBlend::Additive);
        emitter.spawn_rate = 0.0;
        emitter.burst_count = 0;
        let entity = world.spawn((
            Name(format!("ascent/fx-pool/{slot}")),
            Transform::IDENTITY,
            emitter,
        ));
        slots.push(entity);
    }
    world.insert_resource(FxPool { slots, next: 0 });
}

/// HUD：宝石 n/N / 检查点 / 坠落计数 / 横幅（bastion 同款 Name 定位）。
fn build_hud(world: &mut World) {
    let hud = |world: &mut World,
               name: &str,
               position: [f32; 2],
               text: String,
               scale: f32,
               color: [f32; 4]| {
        world.spawn((
            Name(name.into()),
            UiRect {
                position,
                size: [0.0, 0.0],
            },
            UiText {
                string: text,
                scale,
            },
            UiColor(color),
        ));
    };
    hud(
        world,
        "ascent/hud/title",
        [24.0, 20.0],
        "IRIS ASCENT".into(),
        3.0,
        [0.8, 0.95, 1.0, 1.0],
    );
    hud(
        world,
        GEMS_TEXT_NAME,
        [24.0, 56.0],
        format!("GEMS 0/{}", GEMS.len()),
        3.0,
        [1.0, 0.85, 0.3, 1.0],
    );
    hud(
        world,
        CHECKPOINT_TEXT_NAME,
        [24.0, 92.0],
        format!("CHECKPOINT 0/{}", CHECKPOINTS.len()),
        3.0,
        [0.55, 1.0, 0.9, 1.0],
    );
    hud(
        world,
        FALLS_TEXT_NAME,
        [24.0, 128.0],
        "FALLS 0".into(),
        3.0,
        [0.9, 0.9, 0.9, 1.0],
    );
    hud(
        world,
        BANNER_TEXT_NAME,
        [HUD_WIDTH * 0.5 - 200.0, 300.0],
        String::new(),
        4.0,
        [1.0, 0.85, 0.3, 1.0],
    );
}

// ---------------------------------------------------------------------------
// Update 1：逻辑推进 + 状态机 + 音频 + 相机
// ---------------------------------------------------------------------------

/// Update 1：读输入与时间 → 相机相对翻译 → 推进纯逻辑 → 写玩家 /
/// 朝向 / 状态机 → footstep 事件轨道 → 事件空间音频 → 复活帧请求相机
/// 吸附（平滑追随本体由引擎 `update_smooth_follow` 完成）。
fn run_ascent(world: &World) {
    let dt = world
        .try_resource::<Time>()
        .map(|time| time.delta_secs())
        .unwrap_or(0.0);

    // ---- 相机相对输入（iris-game 同款）：W/S = 相机水平前向，A/D =
    // 相机右向。相机固定偏移（根实体恒无旋转）→ 世界系与屏幕系一致；
    // 按相机向量实现可容忍未来相机环绕的扩展。传播前的首帧回退固定
    // 世界系（与相机初始方位一致：看 -Z）----
    let (mut forward, mut right) = (Vec2::new(0.0, -1.0), Vec2::new(1.0, 0.0));
    if let Some(ents) = world.try_resource::<AscentEntities>() {
        if let Some(cam) = world.get::<GlobalTransform>(ents.camera) {
            let m = cam.matrix();
            let f = -Vec2::new(m.z_axis.x, m.z_axis.z);
            let r = Vec2::new(m.x_axis.x, m.x_axis.z);
            if f.length_squared() > 1e-6 {
                forward = f.normalize();
            }
            if r.length_squared() > 1e-6 {
                right = r.normalize();
            }
        }
    }
    let input = world
        .try_resource::<Input>()
        .map(|input| {
            let x = (input.key(KeyCode::KeyD) as i32 - input.key(KeyCode::KeyA) as i32) as f32;
            let z = (input.key(KeyCode::KeyS) as i32 - input.key(KeyCode::KeyW) as i32) as f32;
            let dir = right * x + forward * -z;
            AscentInput {
                move_dir: dir,
                jump: input.key_pressed(KeyCode::Space),
                dt,
            }
        })
        .unwrap_or(AscentInput {
            move_dir: Vec2::ZERO,
            jump: false,
            dt,
        });
    let manual_respawn = world
        .try_resource::<Input>()
        .map(|input| input.key_pressed(KeyCode::KeyR))
        .unwrap_or(false);
    let moving = input.move_dir.length_squared() > 1e-4;

    let Some(mut state) = world.try_resource_mut::<AscentState>() else {
        return;
    };
    state.step(&input);
    if manual_respawn && state.phase == Phase::Playing {
        state.request_respawn();
    }

    // ---- 事件分流：音频去重（同类型取最后一件的位置）+ 横幅置位 +
    // 相机吸附标记 ----
    let mut jump_at = None;
    let mut land_at = None;
    let mut gem_at = None;
    let mut checkpoint_at = None;
    let mut launch_at = None;
    let mut respawn_at = None;
    let mut victory_at = None;
    for event in &state.frame_events {
        match *event {
            AscentEvent::Jump { pos } => jump_at = Some(pos),
            AscentEvent::Landed { pos, .. } => land_at = Some(pos),
            AscentEvent::Gem { pos } => gem_at = Some(pos),
            AscentEvent::Checkpoint { pos, .. } => checkpoint_at = Some(pos),
            AscentEvent::Launch { pos } => launch_at = Some(pos),
            AscentEvent::Respawn { pos } => respawn_at = Some(pos),
            AscentEvent::Victory { pos } => victory_at = Some(pos),
        }
    }
    let respawned = respawn_at.is_some();

    // ---- 三层结构写回：根 = 逻辑位置（rotation 恒恒等），模型 = 朝向，
    // 相机 = 平滑追随（引擎系统写局部位置）----
    if let Some(ents) = world.try_resource::<AscentEntities>() {
        if let Some(mut transform) = world.get_mut::<Transform>(ents.player_root) {
            transform.position = state.pos;
        }
        if let Some(mut transform) = world.get_mut::<Transform>(ents.player_model) {
            transform.rotation = Quat::from_rotation_y(state.facing);
        }
        // 相机平滑追随：平滑与局部位置写入由引擎系统 `update_smooth_follow`
        // 完成（注册在本系统之后）；本系统只负责瞬移帧的吸附请求（打磨 IV
        // 摩擦④闭合：游戏侧 CameraRig 绕行删除）
        if respawned {
            if let Some(mut follow) = world.get_mut::<SmoothFollow>(ents.camera) {
                follow.snap();
            }
        }

        // ---- 动画状态机：移动 ↔ 静止（request_state 双向 crossfade），
        // 并消费 walk 剪辑的事件轨道（footstep → 脚步声，仅地面有效）----
        if let Some(mut player) = world.get_mut::<AnimationPlayer>(ents.player_model) {
            let wanted = if moving && state.phase == Phase::Playing {
                STATE_WALK
            } else {
                STATE_IDLE
            };
            player.request_state(wanted);
            let footstep_at = if state.grounded {
                Some(state.pos)
            } else {
                None
            };
            for name in player.take_events() {
                if name == "footstep" {
                    if let (Some(assets), Some(audio), Some(at)) = (
                        world.try_resource::<AscentAssets>(),
                        world.try_resource::<Audio>(),
                        footstep_at,
                    ) {
                        audio.play_spatial(assets.footstep.clone(), at, 0.08, false);
                    }
                }
            }
        }
    }

    // ---- 事件 → 空间音频（听者跟随相机，见文件尾注释）----
    if let (Some(assets), Some(audio)) = (
        world.try_resource::<AscentAssets>(),
        world.try_resource::<Audio>(),
    ) {
        let spatial = |clip: &AudioClip, at: Option<Vec3>, volume: f32| {
            if let Some(position) = at {
                audio.play_spatial(clip.clone(), position, volume, false);
            }
        };
        spatial(&assets.jump, jump_at, 0.2);
        spatial(&assets.land, land_at, 0.4);
        spatial(&assets.gem, gem_at, 0.25);
        spatial(&assets.checkpoint, checkpoint_at, 0.3);
        spatial(&assets.launch, launch_at, 0.4);
        spatial(&assets.respawn, respawn_at, 0.35);
        spatial(&assets.victory, victory_at, 0.5);
        // 听者：取相机全局位姿（GlobalTransform 来自上一帧传播，滞后一帧
        // 对音频无感；headless / 离屏路径同样得到正确听者）
        if let Some(ents) = world.try_resource::<AscentEntities>() {
            if let Some(global) = world.get::<GlobalTransform>(ents.camera) {
                let matrix = global.matrix();
                let forward = -Vec3::new(matrix.z_axis.x, matrix.z_axis.y, matrix.z_axis.z);
                audio.set_listener(ListenerParams::from_view(
                    global.position(),
                    forward,
                    Vec3::Y,
                ));
            }
        }
    }

    // ---- 坠落横幅置位 ----
    if respawned {
        if let Some(mut timer) = world.try_resource_mut::<BannerTimer>() {
            timer.0 = FALL_BANNER_SECS;
        }
    }
}

// ---------------------------------------------------------------------------
// Update 2：事件 → 粒子（FX 池）
// ---------------------------------------------------------------------------

// 七类事件的 FX 参数（全字段覆写，槽位长期存活防残留）：

/// 尘土（落地大 / 起跳小 / 复活中）：alpha 混合灰土向四周低溅。
fn configure_dust(e: &mut ParticleEmitter, burst: u32, speed: f32, size: f32) {
    e.blend = ParticleBlend::Alpha;
    e.spawn_rate = 0.0;
    e.burst_count = burst;
    e.emitter_lifetime = 0.0;
    e.particle_lifetime = 0.5;
    e.initial_speed = speed;
    e.velocity_jitter = speed * 0.5;
    e.gravity_scale = 0.35;
    e.size_start = size;
    e.size_end = size * 2.2;
    e.color_start = [0.62, 0.58, 0.52, 0.8];
    e.color_end = [0.45, 0.42, 0.4, 0.0];
    e.max_particles = 128;
}

/// 宝石火花：金黄短 burst（additive）。
fn configure_spark(e: &mut ParticleEmitter) {
    e.blend = ParticleBlend::Additive;
    e.spawn_rate = 0.0;
    e.burst_count = 24;
    e.emitter_lifetime = 0.0;
    e.particle_lifetime = 0.35;
    e.initial_speed = 5.0;
    e.velocity_jitter = 2.2;
    e.gravity_scale = 0.5;
    e.size_start = 0.12;
    e.size_end = 0.02;
    e.color_start = [3.0, 2.4, 0.8, 1.0];
    e.color_end = [1.6, 0.9, 0.15, 0.0];
    e.max_particles = 64;
}

/// 检查点点亮：青色上升光屑。
fn configure_checkpoint(e: &mut ParticleEmitter) {
    e.blend = ParticleBlend::Additive;
    e.spawn_rate = 0.0;
    e.burst_count = 40;
    e.emitter_lifetime = 0.0;
    e.particle_lifetime = 0.7;
    e.initial_speed = 2.6;
    e.velocity_jitter = 0.8;
    e.gravity_scale = -0.25;
    e.size_start = 0.14;
    e.size_end = 0.02;
    e.color_start = [0.5, 2.6, 2.3, 1.0];
    e.color_end = [0.1, 0.8, 0.9, 0.0];
    e.max_particles = 96;
}

/// 跳板弹射：环形白汽（alpha，大尺寸快散）。
fn configure_launch(e: &mut ParticleEmitter) {
    e.blend = ParticleBlend::Alpha;
    e.spawn_rate = 0.0;
    e.burst_count = 36;
    e.emitter_lifetime = 0.0;
    e.particle_lifetime = 0.4;
    e.initial_speed = 7.0;
    e.velocity_jitter = 1.5;
    e.gravity_scale = 0.1;
    e.size_start = 0.3;
    e.size_end = 0.7;
    e.color_start = [0.85, 0.95, 1.0, 0.7];
    e.color_end = [0.6, 0.8, 0.9, 0.0];
    e.max_particles = 96;
}

/// 胜利礼花：大号金色 additive 爆发。
fn configure_victory(e: &mut ParticleEmitter) {
    e.blend = ParticleBlend::Additive;
    e.spawn_rate = 0.0;
    e.burst_count = 220;
    e.emitter_lifetime = 0.0;
    e.particle_lifetime = 1.3;
    e.initial_speed = 7.5;
    e.velocity_jitter = 3.0;
    e.gravity_scale = 0.5;
    e.size_start = 0.22;
    e.size_end = 0.02;
    e.color_start = [3.2, 2.5, 0.9, 1.0];
    e.color_end = [1.6, 0.8, 0.1, 0.0];
    e.max_particles = 512;
}

/// 按事件类型把 FX 参数全字段写入槽位发射器并定位。
fn configure_event_emitter(emitter: &mut ParticleEmitter, event: AscentEvent) {
    match event {
        AscentEvent::Landed { impact, .. } => {
            // 落地尘土随冲量放大（软着陆小扬尘、跳板重落大扬尘）
            let strength = (impact / LAUNCH_SPEED).clamp(0.2, 1.0);
            configure_dust(
                emitter,
                (16.0 + 40.0 * strength) as u32,
                2.5 + 3.5 * strength,
                0.18 + 0.14 * strength,
            );
        }
        AscentEvent::Jump { .. } => configure_dust(emitter, 10, 2.0, 0.14),
        AscentEvent::Respawn { .. } => configure_dust(emitter, 20, 3.0, 0.2),
        AscentEvent::Gem { .. } => configure_spark(emitter),
        AscentEvent::Checkpoint { .. } => configure_checkpoint(emitter),
        AscentEvent::Launch { .. } => configure_launch(emitter),
        AscentEvent::Victory { .. } => configure_victory(emitter),
    }
}

/// 事件 → 粒子出生高度（事件坐标多为胶囊中心，脚部事件下移到脚底）。
fn event_origin(event: AscentEvent) -> Vec3 {
    match event {
        AscentEvent::Landed { pos, .. }
        | AscentEvent::Jump { pos }
        | AscentEvent::Respawn { pos } => pos - Vec3::new(0.0, HALF_HEIGHT, 0.0),
        AscentEvent::Launch { pos } => pos - Vec3::new(0.0, HALF_HEIGHT, 0.0),
        AscentEvent::Gem { pos } | AscentEvent::Victory { pos } => pos,
        AscentEvent::Checkpoint { pos, .. } => pos + Vec3::new(0.0, 1.5, 0.0),
    }
}

/// Update 2：事件 → 粒子。发射器挂 FX 池槽位实体按需触发：槽位
/// Transform 挪到事件坐标，发射器组件就地复用（参数全字段覆写 +
/// `reset` 重新上膛）。同帧事件多于槽位时环形覆盖。
fn trigger_ascent_fx(world: &World) {
    let Some(state) = world.try_resource::<AscentState>() else {
        return;
    };
    if state.frame_events.is_empty() {
        return;
    }
    let Some(mut fx) = world.try_resource_mut::<FxPool>() else {
        return;
    };
    for event in &state.frame_events {
        let entity = fx.next_slot();
        if let Some(mut transform) = world.get_mut::<Transform>(entity) {
            transform.position = event_origin(*event);
        }
        if let Some(mut slot) = world.get_mut::<ParticleEmitter>(entity) {
            let emitter = &mut *slot;
            configure_event_emitter(emitter, *event);
            emitter.reset();
        }
    }
}

// ---------------------------------------------------------------------------
// Update 3：表现（宝石 / 检查点旗 / 信标）
// ---------------------------------------------------------------------------

/// Update 3：逻辑状态 → 表现。宝石自转浮动（收集后隐藏）；检查点旗
/// 激活换发光材质；信标慢速自旋。
fn update_ascent_visuals(world: &World) {
    let Some(state) = world.try_resource::<AscentState>() else {
        return;
    };
    let Some(ents) = world.try_resource::<AscentEntities>() else {
        return;
    };
    let elapsed = world
        .try_resource::<Time>()
        .map(|time| time.elapsed_secs())
        .unwrap_or(0.0);

    for (index, &entity) in ents.gems.iter().enumerate() {
        let Some(collected) = state.gems.get(index) else {
            continue;
        };
        if let Some(mut visibility) = world.get_mut::<Visibility>(entity) {
            *visibility = if *collected {
                Visibility::HIDDEN
            } else {
                Visibility::VISIBLE
            };
        }
        if *collected {
            continue;
        }
        if let Some(mut transform) = world.get_mut::<Transform>(entity) {
            transform.rotation = Quat::from_rotation_y(elapsed * 2.0 + index as f32 * 0.7);
            transform.position.y =
                GEMS[index].y + (elapsed * 2.5 + index as f32 * 0.7).sin() * 0.12;
        }
    }

    for (index, &flag) in ents.flags.iter().enumerate() {
        let Some(active) = state.checkpoints.get(index) else {
            continue;
        };
        let wanted = if *active {
            ents.flag_materials[1]
        } else {
            ents.flag_materials[0]
        };
        if let Some(mut handle) = world.get_mut::<MaterialHandle>(flag) {
            if *handle != wanted {
                *handle = wanted;
            }
        }
    }

    if let Some(mut transform) = world.get_mut::<Transform>(ents.beacon) {
        transform.rotation = Quat::from_rotation_y(elapsed * 0.8);
    }
}

// ---------------------------------------------------------------------------
// Update 4：HUD
// ---------------------------------------------------------------------------

/// Update 4：HUD 刷新——宝石 / 检查点 / 坠落计数每帧重写，横幅按阶段
/// 与坠落计时切换。文本实体经 `Name` 按名定位。
fn update_ascent_hud(world: &World) {
    let Some(state) = world.try_resource::<AscentState>() else {
        return;
    };
    let dt = world
        .try_resource::<Time>()
        .map(|time| time.delta_secs())
        .unwrap_or(0.0);

    if let Some(entity) = world.entity_named(GEMS_TEXT_NAME) {
        if let Some(mut text) = world.get_mut::<UiText>(entity) {
            text.string = format!("GEMS {}/{}", state.gems_collected(), state.gems.len());
        }
    }
    if let Some(entity) = world.entity_named(CHECKPOINT_TEXT_NAME) {
        if let Some(mut text) = world.get_mut::<UiText>(entity) {
            text.string = format!(
                "CHECKPOINT {}/{}",
                state.checkpoints_activated(),
                state.checkpoints.len()
            );
        }
    }
    if let Some(entity) = world.entity_named(FALLS_TEXT_NAME) {
        if let Some(mut text) = world.get_mut::<UiText>(entity) {
            text.string = format!("FALLS {}", state.falls);
        }
    }
    if let Some(mut timer) = world.try_resource_mut::<BannerTimer>() {
        timer.0 = (timer.0 - dt).max(0.0);
        if let Some(entity) = world.entity_named(BANNER_TEXT_NAME) {
            if let Some(mut text) = world.get_mut::<UiText>(entity) {
                text.string = if state.phase == Phase::Victory {
                    "SUMMIT REACHED!".into()
                } else if timer.0 > 0.0 {
                    "FELL - BACK TO CHECKPOINT".into()
                } else {
                    String::new()
                };
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 单测（纯逻辑已覆盖规则；此处覆盖插件侧组件语义）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logic::AutoPilot;

    /// FX 池环形推进：槽位序循环复用、越界回绕。
    #[test]
    fn fx_pool_cycles_through_slots() {
        let mut world = World::new();
        let slots: Vec<Entity> = (0..3).map(|_| world.spawn_empty()).collect();
        let mut pool = FxPool { slots, next: 0 };
        let first = pool.next_slot();
        let second = pool.next_slot();
        let third = pool.next_slot();
        assert_ne!(first, second);
        assert_ne!(second, third);
        assert_eq!(pool.next_slot(), first, "环形应回绕到首个槽位");
    }

    /// 自动玩家在纯状态上的推进（插件共享 [`AutoPilot`]）能越过球顶：
    /// 验证脚本条件与关卡常量的配套关系（插件层回归锚点）。
    #[test]
    fn autopilot_reaches_first_checkpoint() {
        let mut state = AscentState::new();
        let pilot = AutoPilot;
        let mut reached = false;
        for _ in 0..60 * 60 {
            let input = pilot.drive(&state);
            state.step(&input);
            if state.checkpoints[0] {
                reached = true;
                break;
            }
        }
        assert!(reached, "60 秒内应激活检查点 1");
        assert_eq!(state.falls, 0);
    }
}
