//! [`InfiltrationPlugin`]：把纯逻辑核心（`crate::logic`）接到 Iris 引擎上，
//! 同时把 R34–R41 的新能力当载荷逐一压榨（接入点在注释中以 **R3x** 标注）。
//!
//! 职责划分（iris-ascent 同款分层）：
//! - **Startup**：`setup_resources`（兜底插入引擎资源 + 游戏资源 +
//!   [`Settings`] 加载）→ `build_scene`（设施几何 / 压力板 sensor / 弹簧
//!   门板 / 监控屏 RT 相机 / 三层玩家与守卫结构 / HUD / 暂停菜单——静态
//!   内容启动 spawn 一次）；
//! - **Update**：`run_infiltration`（读 `Input` / `Time` → 输入翻译 →
//!   **R34** 压力板 `CollisionStarted/Ended` 消费 → [`InfiltrationState::step`]
//!   → 写玩家 / 守卫 → **R38** 混合空间参数 → **R25** 空间音频
//!   → **R41** Esc 菜单栈与 `UiClickEvent` 消费）→
//!   `consume_game_events`（**并行系统**：**R37** 击晕 → ragdoll 命令
//!   入队（`spawn_deferred`，帧末 `apply_commands` 执行）+ 守卫模型
//!   子树隐藏（引擎原语 `set_subtree_hidden`））→
//!   `update_infiltration_visuals`（核心 / 门 / 出口信标表现）→
//!   `update_infiltration_hud`（核心计数 / 警觉 / 横幅）。
//! - **层级传播 / 粒子 / 动画 / 物理 / 空间音频 / UI**：交给
//!   `DefaultPlugins`；headless 测试与离屏快照需自行 build
//!   `HierarchyPlugin` / `AnimationPlugin` / `PhysicsPlugin` / `UiPlugin`
//!   （与 iris-ascent 约定同款，本插件不重复注册）。
//!
//! # 八能力接入点索引（逐条对账 ARCHITECTURE §8 摩擦清单）
//!
//! | 能力 | 接入点 |
//! |---|---|
//! | R35 navmesh | 守卫巡逻/追逐 `find_path`（logic）；压力门开 → `NavGrid` 便宜重建 |
//! | R38 动画 v3 | 玩家与守卫的 `BlendSpace`（参数 = 速度 0..1）+ 双腿链 `FootIk`（纯 IkRig） |
//! | R37 ragdoll | `consume_game_events`：`spawn_deferred` 命令 → `spawn_ragdoll_kinematic` + `release_ragdoll` |
//! | R34 接触事件 | 压力板 sensor × 玩家运动学胶囊 + `run_infiltration` 内 begin/end 消费 |
//! | R40 RTT | `build_scene` 的 RT 相机（盯核心房）+ HUD `UiImageRtt` 画中画 |
//! | R41 UI 菜单 | Esc → `UiMenuStack` push/pop；RESUME/QUIT 按钮 + `UiClickEvent`（Update 直接消费） |
//! | R36 约束 | 弹簧门板 = 动态盒 + `DistanceJoint` 拉回关闭位（玩家运动学胶囊推开） |
//! | R25 空间音频 | 事件 → `play_spatial`（听者跟随主相机） |
//!
//! # 玩家的物理存在（R34/R36 的载体，打磨 V 起无代理）
//!
//! 玩家是纯逻辑运动学角色（[`move_and_slide_statics`]，只认逻辑静态体），
//! 位移由本插件每帧写玩家根实体的 `Transform`。玩家根实体直接挂
//! `RigidBody { body_type: Kinematic, .. }` + 动态胶囊 Collider（打磨 V
//! 新语义）：跳过物理积分（逻辑全权驱动位移）、但参与碰撞——压力板
//! sensor（R34）由真实接触产出事件、弹簧门板（R36）被真实推离（动体撞
//! 运动学体按静态方路径全额退让）。v1 曾以「动态物理代理 + 逐帧同步」
//! 绕行（无运动学碰撞体概念），打磨 V 闭合后代理已删（§8.22⑥）。
//! kinematic 胶囊与真静态墙体 / 地板的配对求解为 no-op（双方
//! inv_mass = 0）、照常进接触集合——对逻辑零影响。
//!
//! # 击晕与表现（R37，诚实限制）
//!
//! `GuardStunned` 事件 → 在守卫脚底生成六球人形 ragdoll 并立即释放
//! （受击方向 = 守卫面朝方向，轻踢倒地）；守卫模型子树整体
//! `Visibility::HIDDEN`。ragdoll 只提供物理原语（引擎侧刻意不含渲染），
//! 肢体的**渲染表现是游戏侧的选择**：生成后逐肢体挂球体网格（半径 =
//! 肢体定义半径）+ 守卫配色材质 + `Visibility::VISIBLE`——否则模型
//! 隐藏后瘫倒肢体不可见（「击晕即凭空消失」的体验缺陷）。
//! **v1 击晕后不醒来**——没有苏醒计时与重定向逻辑，尸体永久留在原地
//! （ragdoll 不回收）。
//!
//! 本插件只使用引擎公开 API（dogfooding）：不依赖引擎内部路径。
//! 打磨 V 闭合记录：①`UiClickEvent` 生命周期（MenuCommand 信箱已删）、
//! ②纯 IkRig 腿链（零剪辑播放器已删）、③命令队列（ragdoll 独占屏障
//! 已删，改并行系统 + `spawn_deferred`）、⑤`set_subtree_hidden`（游戏
//! 侧递归隐藏已删）、⑥运动学碰撞体（物理代理已删）——§8.22 六条中
//! ④（节点 scale）无需游戏侧改动（装配本来就守契约）。

use std::path::{Path, PathBuf};

use glam::{Quat, Vec2, Vec3};

use iris_core::commands::CommandQueue;
use iris_core::physics::{
    BodyType, Collider, CollisionEnded, CollisionStarted, DistanceJoint, RigidBody,
};
use iris_core::prelude::{Access, Entity, Name, Stage};
use iris_core::ragdoll::{RagdollDesc, release_ragdoll, spawn_ragdoll_kinematic};
use iris_engine::prelude::*;

use crate::logic::{
    CORES, DOOR, HALF_HEIGHT, InfiltrationEvent, InfiltrationInput, InfiltrationState, MOVE_SPEED,
    PLATE_POS, PLATE_RADIUS, Phase, WALLS,
};

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 角色模型路径（仓库相对，经 [`asset_path`] 解析）——复用 iris-ascent 的
/// 双剪辑小机器人（idle / walk），混合空间样本点即挂这两个剪辑。
pub const MODEL_PATH: &str = "assets/ascent_bot.glb";
/// 主相机偏移（玩家根实体局部）：俯视偏斜视角（根无旋转 → 世界系恒定）。
pub const CAMERA_OFFSET: Vec3 = Vec3::new(0.0, 4.5, 4.0);
/// 相机俯仰（弧度，绕 X 负角 = 望向玩家脚下）。
const CAMERA_PITCH: f32 = -48.0_f32.to_radians();
/// 相机平滑追随刚度（1/秒）。
const CAMERA_STIFFNESS: f32 = 6.0;
/// 空间音频距离参考（米）：俯视相机距角色 ~11 m，按此尺度放大。
pub const REFERENCE_DISTANCE: f32 = 14.0;
/// 横幅持续（秒）。
const BANNER_SECS: f32 = 2.5;
/// HUD 实体名（`Name` 组件，按名定位用）。
pub const CORES_TEXT_NAME: &str = "infiltration/hud/cores";
pub const SPOTTED_TEXT_NAME: &str = "infiltration/hud/spotted";
pub const ALERT_TEXT_NAME: &str = "infiltration/hud/alert";
pub const BANNER_TEXT_NAME: &str = "infiltration/hud/banner";
/// 暂停菜单名（`UiMenuStack` 栈元素）。
pub const PAUSE_MENU: &str = "pause";
/// 设置文件（R41 `Settings`）：启动读取（存在才读）、QUIT 时保存。
pub const SETTINGS_PATH: &str = "infiltration_settings.json";

// ---------------------------------------------------------------------------
// 插件侧资源
// ---------------------------------------------------------------------------

/// 适配：把纯逻辑类型接入引擎资源存储（逻辑核心零引擎依赖，跨模块手动
/// 实现 `Resource` 即可「选择性接入」，iris-ascent 同款）。
impl iris_core::resource::Resource for InfiltrationState {}

/// 横幅剩余显示时间（秒；`run_infiltration` 置位、HUD 递减清零）。
#[derive(Clone, Copy, Debug, Default, Resource)]
pub struct BannerTimer(pub f32);

/// 预生成的程序化音效（全部确定性、Startup 一次生成）。
#[derive(Clone, Resource)]
pub struct InfiltrationAssets {
    /// 拾取核心：1318 Hz 亮 ping。
    pub pickup: AudioClip,
    /// 压力板踩下：220 Hz 短咔哒。
    pub plate: AudioClip,
    /// 压力门滑开：宽带噪声 whoosh。
    pub door: AudioClip,
    /// 被发现警报：880 Hz 三连急促 beep。
    pub alarm: AudioClip,
    /// 击晕闷响：低频噪声。
    pub stun: AudioClip,
    /// 胜利：523 / 659 / 784 Hz 上行三连音。
    pub victory: AudioClip,
    /// 告负：392 / 262 Hz 下行双音。
    pub defeat: AudioClip,
}

/// 守卫的表现实体对：根（逻辑位置）+ 模型（混合空间动画 + 朝向）。
#[derive(Clone, Copy, Debug)]
pub struct GuardEntities {
    /// 根实体：只承载逻辑位置（rotation 恒恒等）。
    pub root: Entity,
    /// 模型子实体：挂 [`AnimationPlayer`]，朝向（yaw）写它的 rotation。
    pub model: Entity,
}

/// 场景实体索引：每帧热路径的句柄资源。HUD 文本实体经 `Name` +
/// `World::entity_named` 按名定位（低频路径）。
#[derive(Clone, Resource)]
pub struct InfiltrationEntities {
    /// 玩家根实体：只承载逻辑位置（rotation 恒恒等）。
    pub player_root: Entity,
    /// 玩家模型子实体：挂 [`AnimationPlayer`]（混合空间）。
    pub player_model: Entity,
    /// 主相机子实体（`ActiveCamera` + `SmoothFollow`；无 `RendersTo`）。
    pub camera: Entity,
    /// 守卫（下标 = [`crate::logic::GUARD_ROUTES`] 下标）。
    pub guards: Vec<GuardEntities>,
    /// 数据核心实体（下标 = [`CORES`] 下标；拾取后隐藏）。
    pub cores: Vec<Entity>,
    /// 压力板实体（sensor 挂载体，R34 事件按它过滤）。
    pub plate: Entity,
    /// 压力门实体（开门 = 隐藏 + 碰撞组清零的标记语义）。
    pub door: Entity,
    /// 弹簧门板实体（R36：动态盒 + [`DistanceJoint`]）。
    pub door_panel: Entity,
    /// 出口信标（胜利点视觉）。
    pub exit_beacon: Entity,
    /// 监控屏渲染目标句柄（R40）。
    pub monitor: RenderTargetHandle,
}

// ---------------------------------------------------------------------------
// 插件
// ---------------------------------------------------------------------------

/// 潜入插件：Startup 代码生成设施 + Update 推进逻辑与表现。关卡全部来自
/// 代码（无需场景文件），`Default` 即可用。
#[derive(Debug, Clone)]
pub struct InfiltrationPlugin {
    /// Startup 时加载的角色 GLB 路径（[`asset_path`] 解析；测试可注入）。
    pub model_path: PathBuf,
    /// 设置文件路径（[`Settings`] 加载 / 保存，见 [`SETTINGS_PATH`]）。
    pub settings_path: PathBuf,
}

impl Default for InfiltrationPlugin {
    fn default() -> Self {
        Self {
            model_path: asset_path(MODEL_PATH),
            settings_path: PathBuf::from(SETTINGS_PATH),
        }
    }
}

impl InfiltrationPlugin {
    /// 以指定角色模型路径构造（测试注入临时资产 / 显式给默认路径）。
    pub fn new(model_path: impl Into<PathBuf>) -> Self {
        Self {
            model_path: model_path.into(),
            settings_path: PathBuf::from(SETTINGS_PATH),
        }
    }

    /// 把本插件的全部系统注册到调度器。
    ///
    /// 与 `DefaultPlugins` 的编排方式一致（Startup 一次、Update 每帧）；
    /// 独立暴露是为了 headless 测试与离屏快照在不建 `App` / 窗口 / GPU
    /// 的前提下复用同一条推进路径。**层级 / 物理 / 动画 / UI / 空间音频
    /// 不在此注册**：调用方按窗口路径的编排补上 `HierarchyPlugin`
    /// （PostUpdate）、`PhysicsPlugin`（Update、本插件系统之前——压力板
    /// 事件在帧内先产出后消费）、`AnimationPlugin`（Update、先于本插件）、
    /// `UiPlugin`（PostUpdate：layout → menus → focus）与 `SpatialPlugin`。
    pub fn build_schedule(&self, schedule: &mut Schedule) {
        let model_path = self.model_path.clone();
        let settings_path = self.settings_path.clone();
        schedule
            .add_system(Stage::Startup, move |world: &mut World| {
                setup_resources(world, &settings_path);
            })
            .add_system(Stage::Startup, move |world: &mut World| {
                build_scene(world, &model_path)
            })
            // Update 内的顺序即注册顺序：物理（DefaultPlugins 先注册）
            // → 逻辑 + 接触事件 + 输入 + 音频 + 菜单 → 独占（ragdoll）
            // → 表现 → HUD
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<Input>()
                    .read::<Time>()
                    .read::<Audio>()
                    .read::<InfiltrationAssets>()
                    .read::<InfiltrationEntities>()
                    .read::<GlobalTransform>()
                    .write::<InfiltrationState>()
                    .write::<Transform>()
                    .write::<AnimationPlayer>()
                    .write::<RigidBody>()
                    .write::<UiMenuStack>()
                    .write::<UiText>()
                    .write::<UiColor>()
                    .write::<BannerTimer>(),
                run_infiltration,
            )
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<InfiltrationState>()
                    .read::<InfiltrationEntities>()
                    .write::<Visibility>()
                    .write::<Collider>()
                    .write::<RigidBody>()
                    .write::<CommandQueue>(),
                consume_game_events,
            )
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<InfiltrationState>()
                    .read::<Time>()
                    .read::<InfiltrationEntities>()
                    .write::<Transform>()
                    .write::<Visibility>()
                    .write::<Collider>()
                    .write::<MaterialHandle>(),
                update_infiltration_visuals,
            )
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<InfiltrationState>()
                    .read::<Time>()
                    .write::<UiText>()
                    .write::<BannerTimer>(),
                update_infiltration_hud,
            );
    }
}

impl Plugin for InfiltrationPlugin {
    fn build(&self, app: &mut App) {
        self.build_schedule(&mut app.schedule);
    }
}

/// Startup 1：资源兜底插入（bastion 同款「缺席才插入」语义）+ R41 设置
/// 加载（文件存在才读——headless / 首启零告警）。
fn setup_resources(world: &mut World, settings_path: &Path) {
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
    // R40：渲染目标目录（游戏侧按需 insert，缺席 = 无 RT 相机主路径）
    if !world.contains_resource::<RenderTargetStore>() {
        world.insert_resource(RenderTargetStore::new());
    }
    // R41：UI 资源 + 事件（DefaultPlugins 的 UiPlugin 已注册时幂等兜底）
    if !world.contains_resource::<UiFocus>() {
        world.insert_resource(UiFocus::default());
    }
    if !world.contains_resource::<UiMenuStack>() {
        world.insert_resource(UiMenuStack::default());
    }
    world.register_event::<UiClickEvent>();
    world.register_event::<AppExit>();
    // R41 设置：存在才读（缺省路径写盘只发生在 QUIT）
    let settings = if settings_path.exists() {
        Settings::load(settings_path)
    } else {
        Settings::default()
    };
    if !world.contains_resource::<Settings>() {
        world.insert_resource(settings);
    }
    world.insert_resource(InfiltrationState::new());
    world.insert_resource(BannerTimer::default());
    if let Some(audio) = world.try_resource::<Audio>() {
        audio.set_reference_distance(REFERENCE_DISTANCE);
    }
    world.insert_resource(InfiltrationAssets {
        pickup: sine(1318.0, 0.12, 48_000),
        plate: sine(220.0, 0.06, 48_000),
        door: noise(0.25, 48_000, 0xD00D_FACE),
        alarm: three_beep(48_000),
        stun: noise(0.15, 48_000, 0x057A_B1E5),
        victory: three_tone_up(48_000),
        defeat: two_tone_down(48_000),
    });
}

/// 警报三连音：880 Hz 三声急促 beep（发现告警）。
fn three_beep(sample_rate: u32) -> AudioClip {
    let beep = sine(880.0, 0.09, sample_rate);
    let gap = (0.05 * sample_rate as f32) as usize;
    let long = sine(880.0, 0.18, sample_rate);
    let silence =
        AudioClip::from_interleaved(vec![0.0; gap], 1, sample_rate).expect("程序化参数恒合法");
    AudioClip::concat(&[&beep, &silence, &beep, &silence, &long])
        .expect("同参数生成的剪辑布局必然一致")
}

/// 上行三连音（胜利）：523 / 659 / 784 Hz。
fn three_tone_up(sample_rate: u32) -> AudioClip {
    let a = sine(523.0, 0.12, sample_rate);
    let b = sine(659.0, 0.12, sample_rate);
    let c = sine(784.0, 0.26, sample_rate);
    AudioClip::concat(&[&a, &b, &c]).expect("同参数生成的剪辑布局必然一致")
}

/// 下行双音（告负）：392 / 262 Hz。
fn two_tone_down(sample_rate: u32) -> AudioClip {
    let a = sine(392.0, 0.1, sample_rate);
    let b = sine(262.0, 0.18, sample_rate);
    AudioClip::concat(&[&a, &b]).expect("同参数生成的剪辑布局必然一致")
}

// ---------------------------------------------------------------------------
// Startup 2：代码生成设施
// ---------------------------------------------------------------------------

/// Startup 2：代码生成设施 + 三层玩家与守卫结构 + 监控屏 RT 相机 + HUD +
/// 暂停菜单。静态内容启动 spawn 一次即可；唯一运行期 spawn 是守卫 ragdoll
/// 肢体（`consume_game_events` 独占系统）。
fn build_scene(world: &mut World, model_path: &Path) {
    build_facility(world);
    let (player_root, player_model, camera) = build_player(world, model_path);
    let guards = build_guards(world, model_path);
    let cores = build_cores(world);
    let (plate, door, door_panel) = build_machinery(world);
    let exit_beacon = build_exit(world);
    let monitor = build_monitor_camera(world);
    build_hud(world, monitor);
    build_pause_menu(world);
    world.insert_resource(InfiltrationEntities {
        player_root,
        player_model,
        camera,
        guards,
        cores,
        plate,
        door,
        door_panel,
        exit_beacon,
        monitor,
    });
}

/// 设施几何：地板 + `WALLS` 全部内墙（视觉与逻辑静态体同源，插件层按
/// `half × 2` 缩放立方体）+ 墙体静态碰撞体（供物理代理 / ragdoll /
/// IK 落地查询——逻辑核心自己持有一份同源 AABB，见 logic）。
fn build_facility(world: &mut World) {
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mut stores = world.resource_mut::<MaterialStore>();
    let floor_mat = stores.add(MaterialDef::solid(0.16, 0.17, 0.2, 1.0));
    let wall_mat = stores.add(MaterialDef::solid(0.3, 0.32, 0.38, 1.0));
    drop(stores);
    // 地板：视觉 + 静态碰撞体（IK 落地 / ragdoll 承托 / 代理站立）
    world.spawn((
        Name("infiltration/floor".into()),
        Transform {
            position: Vec3::new(0.0, -0.5, 0.0),
            scale: Vec3::new(24.8, 1.0, 18.8),
            ..Transform::IDENTITY
        },
        cube,
        floor_mat,
        Collider::static_box(Vec3::new(12.4, 0.5, 9.4)),
    ));
    for (index, wall) in WALLS.iter().enumerate() {
        world.spawn((
            Name(format!("infiltration/wall/{index}")),
            Transform {
                position: wall.center,
                scale: wall.half * 2.0,
                ..Transform::IDENTITY
            },
            cube,
            wall_mat,
            Collider::static_box(wall.half),
        ));
    }
}

/// 组装双腿链（R38 足部 IK）：wrapper（挂纯 IK 装配 [`IkRig`] +
/// [`FootIk`]，见 [`FootIk::from_rig`]）→ hip → knee → foot 直连链。
/// 腿根在模型局部 y 0.85、两骨各 0.45：脚踝初始位在地板下 5 cm——
/// 平地上 IK 每帧把踝拉回 y=0（视觉证据：脚不插地）。链上三个节点
/// scale 恒 1，小方块网格挂独立的子网格实体（节点缩放会把骨长搅乱，
/// 见模块文档与 ARCHITECTURE §8.22④）。
fn build_leg(world: &mut World, parent: Entity, tag: &str, side_x: f32) -> Entity {
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.15, 0.18, 0.22, 1.0));
    let wrapper = world.spawn((Name(format!("infiltration/{tag}")), Transform::IDENTITY));
    world.add_child(parent, wrapper);
    // 关节节点 scale 恒 1（链长 0.45 + 0.45 由节点位置定义）——网格尺寸
    // 全部放在独立的子网格实体上：节点缩放会被 IK 的链矩阵继承，把骨长
    // 搅乱（踩坑实录，见 ARCHITECTURE §8 摩擦清单）
    let bone_mesh = |world: &mut World, parent: Entity, name: String, scale: Vec3| {
        let mesh = world.spawn((
            Name(name),
            Transform {
                scale,
                ..Transform::IDENTITY
            },
            cube,
            mat,
        ));
        world.add_child(parent, mesh);
    };
    let hip = world.spawn((
        Name(format!("infiltration/{tag}/hip")),
        Transform::from_xyz(side_x, 0.85, 0.0),
    ));
    bone_mesh(
        world,
        hip,
        format!("infiltration/{tag}/hip-mesh"),
        Vec3::new(0.18, 0.5, 0.2),
    );
    let knee = world.spawn((
        Name(format!("infiltration/{tag}/knee")),
        Transform::from_xyz(0.0, -0.45, 0.0),
    ));
    bone_mesh(
        world,
        knee,
        format!("infiltration/{tag}/knee-mesh"),
        Vec3::new(0.14, 0.5, 0.16),
    );
    let foot = world.spawn((
        Name(format!("infiltration/{tag}/foot")),
        Transform::from_xyz(0.0, -0.45, 0.0),
    ));
    bone_mesh(
        world,
        foot,
        format!("infiltration/{tag}/foot-mesh"),
        Vec3::new(0.2, 0.12, 0.34),
    );
    world.add_child(wrapper, hip);
    world.add_child(hip, knee);
    world.add_child(knee, foot);
    // 纯 IK 装配（打磨 V）：IkRig 自带节点映射，不再被迫挂零剪辑
    // AnimationPlayer（§8.22② 闭合）；node 0/1/2 = hip/knee/foot
    let (rig, ik) = FootIk::from_rig(hip, knee, foot);
    world.insert(wrapper, rig);
    world.insert(wrapper, ik);
    wrapper
}

/// 装配混合空间（R38）：参数 x = 移动速度 0..1，样本 (0,0)=idle 剪辑、
/// (1,0)=walk 剪辑——轴向双线性插值在单 y 行退化为 1D 链式 lerp，
/// 参数连续移动即 idle ↔ walk 连续混合。
fn configure_blend_space(player: &mut AnimationPlayer) {
    player.blend_space = Some(BlendSpace {
        samples: vec![(Vec2::ZERO, 0), (Vec2::new(1.0, 0.0), 1)],
    });
    player.play_blendspace();
}

/// 组装一名角色（玩家或守卫共用）：根（逻辑位置）→ 模型（GLB 优先 +
/// 混合空间；降级程序化几何 + 代码剪辑）→ 双腿链。
fn build_character(
    world: &mut World,
    model_path: &Path,
    tag: &str,
    root_pos: Vec3,
) -> (Entity, Entity) {
    let root = world.spawn((
        Name(format!("infiltration/{tag}")),
        Transform::from_translation(root_pos),
        Visibility::VISIBLE,
    ));
    let model = match spawn_gltf_instance(world, model_path) {
        Ok(instance) if instance.clips.len() >= 2 => {
            let gltf_root = instance.roots[0];
            let mut player = instance.player();
            configure_blend_space(&mut player);
            player.capture_rest(world);
            world.insert(gltf_root, player);
            world.insert(gltf_root, Name(format!("infiltration/{tag}-model")));
            world.add_child(root, gltf_root);
            // 模型原点在脚底：胶囊中心 − 半高
            if let Some(mut transform) = world.get_mut::<Transform>(gltf_root) {
                transform.position = Vec3::new(0.0, -HALF_HEIGHT, 0.0);
            }
            gltf_root
        }
        Ok(_) | Err(_) => {
            log::warn!(
                "InfiltrationPlugin：角色模型 {} 不可用（缺失或剪辑不足）——\
                 降级为程序化几何 + 代码剪辑（游戏行为不受影响）",
                model_path.display()
            );
            let fallback = fallback_bot(world, tag);
            world.add_child(root, fallback);
            if let Some(mut transform) = world.get_mut::<Transform>(fallback) {
                transform.position = Vec3::new(0.0, -HALF_HEIGHT, 0.0);
            }
            fallback
        }
    };
    build_leg(world, model, &format!("{tag}-leg-l"), -0.16);
    build_leg(world, model, &format!("{tag}-leg-r"), 0.16);
    (root, model)
}

/// 降级角色：程序化盒身 + 眼，代码构建的同构双剪辑（idle 呼吸缩放 /
/// walk 前倾摇摆）+ 混合空间——与 GLB 路径行为一致。
fn fallback_bot(world: &mut World, tag: &str) -> Entity {
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let body_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.55, 0.2, 0.2, 1.0));
    let eye_mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [0.1, 0.1, 0.12, 1.0],
        emissive: [2.2, 0.4, 0.3, 1.0],
        metallic: 0.0,
        roughness: 0.3,
        ..MaterialDef::default()
    });
    let wrapper = world.spawn((
        Name(format!("infiltration/{tag}-model")),
        Transform::IDENTITY,
    ));
    let body = world.spawn((
        Name(format!("infiltration/{tag}-model/body")),
        Transform {
            position: Vec3::new(0.0, 0.9, 0.0),
            scale: Vec3::new(0.7, 0.8, 0.5),
            ..Transform::IDENTITY
        },
        cube,
        body_mat,
    ));
    let eye = world.spawn((
        Name(format!("infiltration/{tag}-model/eye")),
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
    let mut player = AnimationPlayer::new(vec![idle, walk], vec![Some(body)]);
    configure_blend_space(&mut player);
    player.capture_rest(world);
    world.insert(wrapper, player);
    wrapper
}

/// 三层玩家结构：根 → 模型（混合空间 + 腿链）→ 相机（`SmoothFollow` +
/// 固定俯角）。相机无 `RendersTo` → 渲染器主视图。
fn build_player(world: &mut World, model_path: &Path) -> (Entity, Entity, Entity) {
    let spawn = crate::logic::SPAWN_FOOT + Vec3::new(0.0, HALF_HEIGHT, 0.0);
    let (root, model) = build_character(world, model_path, "player", spawn);
    // 玩家的物理存在（打磨 V）：运动学胶囊——跳过物理积分（逻辑全权
    // 驱动位移），但参与碰撞与接触事件：压力板 sensor（R34）由真实
    // 接触产出事件、弹簧门板（R36）被真实推离（见模块文档「玩家的
    // 物理存在」；v1 的动态代理绕行已删）
    world.insert(root, Collider::dynamic_capsule(0.4, HALF_HEIGHT, 0.0));
    world.insert(
        root,
        RigidBody {
            body_type: BodyType::Kinematic,
            ..RigidBody::default()
        },
    );
    let camera = world.spawn((
        Name("infiltration/player-camera".into()),
        Transform {
            position: CAMERA_OFFSET,
            rotation: Quat::from_rotation_x(CAMERA_PITCH),
            scale: Vec3::ONE,
        },
        ActiveCamera {
            fov_y_radians: 50.0_f32.to_radians(),
            ..ActiveCamera::default()
        },
        SmoothFollow::new(root, CAMERA_OFFSET, CAMERA_STIFFNESS),
    ));
    world.add_child(root, camera);
    (root, model, camera)
}

/// 两名守卫（表现侧；逻辑在 [`InfiltrationState::guards`]，每帧写回）。
fn build_guards(world: &mut World, model_path: &Path) -> Vec<GuardEntities> {
    crate::logic::GUARD_ROUTES
        .iter()
        .enumerate()
        .map(|(index, route)| {
            let start = route[0];
            let (root, model) = build_character(
                world,
                model_path,
                &format!("guard/{index}"),
                Vec3::new(start.x, HALF_HEIGHT, start.y),
            );
            GuardEntities { root, model }
        })
        .collect()
}

/// 三个数据核心：悬浮发光方块（拾取后隐藏）。
fn build_cores(world: &mut World) -> Vec<Entity> {
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [1.0, 0.82, 0.25, 1.0],
        emissive: [1.8, 1.2, 0.25, 1.0],
        metallic: 0.6,
        roughness: 0.3,
        ..MaterialDef::default()
    });
    CORES
        .iter()
        .enumerate()
        .map(|(index, core)| {
            world.spawn((
                Name(format!("infiltration/core/{index}")),
                Transform {
                    position: Vec3::new(core.x, 0.5, core.y),
                    scale: Vec3::new(0.4, 0.4, 0.4),
                    ..Transform::IDENTITY
                },
                cube,
                mat,
                Visibility::VISIBLE,
            ))
        })
        .collect()
}

/// 机关：压力板（**R34** sensor 盒）+ 压力门（静态体，开门 = 标记）+
/// 弹簧门板（**R36** 动态盒 + [`DistanceJoint`] 拉回关闭位）。
/// 返回 (压力板, 压力门, 弹簧门板)。
fn build_machinery(world: &mut World) -> (Entity, Entity, Entity) {
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mut stores = world.resource_mut::<MaterialStore>();
    let plate_mat = stores.add(MaterialDef {
        base_color: [0.85, 0.7, 0.2, 1.0],
        emissive: [0.9, 0.7, 0.15, 1.0],
        metallic: 0.2,
        roughness: 0.5,
        ..MaterialDef::default()
    });
    let door_mat = stores.add(MaterialDef::solid(0.45, 0.25, 0.2, 1.0));
    // 弹簧门板配色刻意区别于墙体（墙 0.3,0.32,0.38）：青色 + 微发光
    // ——「这块方块能推开」的视觉提示（弹簧门是设计语义：动态盒 +
    // 关节回拉，玩家可推开穿过，松手回弹关门）
    let panel_mat = stores.add(MaterialDef {
        base_color: [0.2, 0.55, 0.6, 1.0],
        emissive: [0.1, 0.45, 0.5, 1.0],
        metallic: 0.4,
        roughness: 0.35,
        ..MaterialDef::default()
    });
    drop(stores);
    // 压力板：实体 = sensor 载体（中心 y 0.25、半高 0.25，检测域
    // y 0..0.5 覆盖脚部；不阻挡不反弹，只报接触事件）；视觉薄板挂子实体。
    // sensor 半宽 = 板面半宽 [`PLATE_RADIUS`]（0.45）——「站上板面才
    // 触发」：sensor 只在玩家胶囊接触到板面投影时出事件（胶囊半径使其
    // 比纯逻辑的中心距离判定早 ~0.4 m 报 begin，两条路径调用同一幂等
    // 入口，门是单向闩锁，语义无碍）
    let plate = world.spawn((
        Name("infiltration/plate".into()),
        Transform::from_xyz(PLATE_POS.x, 0.25, PLATE_POS.y),
        Collider::sensor_box(Vec3::new(PLATE_RADIUS, 0.25, PLATE_RADIUS)),
        Visibility::VISIBLE,
    ));
    let plate_visual = world.spawn((
        Name("infiltration/plate-visual".into()),
        Transform {
            position: Vec3::new(PLATE_POS.x, 0.05, PLATE_POS.y),
            scale: Vec3::new(PLATE_RADIUS * 2.0, 0.1, PLATE_RADIUS * 2.0),
            ..Transform::IDENTITY
        },
        cube,
        plate_mat,
        Visibility::VISIBLE,
    ));
    world.add_child(plate, plate_visual);
    // 压力门：视觉 + 静态碰撞体（关闭态）；开门由 update_infiltration_visuals
    // 做标记语义（Visibility::HIDDEN + groups/mask 清零）
    let door = world.spawn((
        Name("infiltration/door".into()),
        Transform {
            position: DOOR.center,
            scale: DOOR.half * 2.0,
            ..Transform::IDENTITY
        },
        cube,
        door_mat,
        Collider::static_box(DOOR.half),
        Visibility::VISIBLE,
    ));
    // 弹簧门板（NW 门洞）：动态盒 + 关节锚（无碰撞体——`DistanceJoint`
    // 的 target 只需 Transform）+ 弹簧参数。玩家运动学胶囊把它推开，
    // 关节每轮松弛拉回关闭位（**设计语义**：可推开的回弹门，不在逻辑
    // 静态体里、玩家逻辑畅通；配色微发光区别于墙，见 panel_mat 注释）。
    let panel_center = Vec3::new(-7.0, 1.25, -2.0);
    let anchor = world.spawn((
        Name("infiltration/door-panel-anchor".into()),
        Transform::from_translation(panel_center),
    ));
    let door_panel = world.spawn((
        Name("infiltration/door-panel".into()),
        Transform::from_translation(panel_center),
        cube,
        panel_mat,
        Visibility::VISIBLE,
        Collider::dynamic_box(Vec3::new(1.0, 1.25, 0.15), 0.0),
        RigidBody {
            velocity: Vec3::ZERO,
            gravity_scale: 0.0,
            friction: 0.6,
            linear_damping: 3.0,
            body_type: BodyType::Dynamic,
        },
        DistanceJoint {
            target: anchor,
            rest_length: 0.15,
            stiffness: 0.3,
            damping: 0.2,
        },
    ));
    (plate, door, door_panel)
}

/// 出口信标：绿色发光柱 + 点光（SW 室）。
fn build_exit(world: &mut World) -> Entity {
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [0.3, 0.95, 0.5, 1.0],
        emissive: [0.4, 2.2, 0.8, 1.0],
        metallic: 0.0,
        roughness: 0.4,
        ..MaterialDef::default()
    });
    let beacon = world.spawn((
        Name("infiltration/exit".into()),
        Transform {
            position: Vec3::new(
                crate::logic::EXIT_CENTER.x,
                1.5,
                crate::logic::EXIT_CENTER.z,
            ),
            scale: Vec3::new(0.4, 3.0, 0.4),
            ..Transform::IDENTITY
        },
        cube,
        mat,
        Visibility::VISIBLE,
    ));
    world.spawn((
        Name("infiltration/exit-light".into()),
        Transform::from_xyz(
            crate::logic::EXIT_CENTER.x,
            3.5,
            crate::logic::EXIT_CENTER.z,
        ),
        PointLight {
            color: Vec3::new(0.4, 1.0, 0.6),
            intensity: 8.0,
            range: 18.0,
            shadows: false,
        },
    ));
    beacon
}

/// 监控屏 RT 相机（**R40**）：固定在核心 C（NE 室）正上方 9 m 垂直下望，
/// 输出到 [`RenderTargetStore`] 新增的 256×192 渲染目标。
fn build_monitor_camera(world: &mut World) -> RenderTargetHandle {
    let monitor = world.resource_mut::<RenderTargetStore>().add(256, 192);
    world.spawn((
        Name("infiltration/monitor-camera".into()),
        Transform {
            position: Vec3::new(CORES[1].x, 9.0, CORES[1].y),
            rotation: Quat::from_rotation_arc(Vec3::NEG_Z, Vec3::NEG_Y),
            scale: Vec3::ONE,
        },
        ActiveCamera {
            fov_y_radians: 60.0_f32.to_radians(),
            ..ActiveCamera::default()
        },
        RendersTo { target: monitor },
    ));
    monitor
}

/// HUD：标题 / 核心计数 / 发现计数 / 警觉 / 横幅 + 监控屏画中画
/// （**R40** `UiImageRtt` 右上角）。HUD 实体不挂 `UiMenu`（常驻可见）。
fn build_hud(world: &mut World, monitor: RenderTargetHandle) {
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
        "infiltration/hud/title",
        [24.0, 20.0],
        "IRIS INFILTRATION".into(),
        3.0,
        [0.75, 0.95, 1.0, 1.0],
    );
    hud(
        world,
        CORES_TEXT_NAME,
        [24.0, 56.0],
        format!("CORES 0/{}", CORES.len()),
        3.0,
        [1.0, 0.85, 0.3, 1.0],
    );
    hud(
        world,
        SPOTTED_TEXT_NAME,
        [24.0, 92.0],
        format!("SPOTTED 0/{}", crate::logic::MAX_DETECTIONS),
        3.0,
        [1.0, 0.45, 0.35, 1.0],
    );
    hud(
        world,
        ALERT_TEXT_NAME,
        [24.0, 128.0],
        "ALERT 0%".into(),
        3.0,
        [0.9, 0.9, 0.9, 1.0],
    );
    hud(
        world,
        BANNER_TEXT_NAME,
        [1280.0 * 0.5 - 240.0, 300.0],
        String::new(),
        4.0,
        [1.0, 0.4, 0.3, 1.0],
    );
    // 监控屏画中画：右上角 256×192 + 标签（R41 UiAnchor 锚定视口右上——
    // 窗口 1280×720 与离屏 640×360 都贴右上角，不漂移）
    world.spawn((
        Name("infiltration/hud/monitor".into()),
        UiRect {
            position: [0.0, 0.0],
            size: [256.0, 192.0],
        },
        UiImageRtt { target: monitor },
        UiColor::default(),
        UiAnchor {
            point: UiAnchorPoint::TopRight,
            offset: [-8.0, 8.0],
        },
    ));
    world.spawn((
        Name("infiltration/hud/monitor-label".into()),
        UiRect {
            position: [0.0, 0.0],
            size: [8.0, 8.0],
        },
        UiText {
            string: "CAM-02 CORE VAULT".into(),
            scale: 2.0,
        },
        UiColor([1.0, 1.0, 1.0, 1.0]),
        UiAnchor {
            point: UiAnchorPoint::TopRight,
            offset: [-264.0, 204.0],
        },
    ));
}

/// 暂停菜单（**R41**）：底板文字 + RESUME / QUIT 按钮（`spawn_ui_button`
/// 组装底板 + 标签，挂 `UiMenu("pause")`——菜单栈顶才可见）。
fn build_pause_menu(world: &mut World) {
    world.spawn((
        Name("infiltration/menu/pause/title".into()),
        UiRect {
            position: [0.0, 0.0],
            size: [300.0, 30.0],
        },
        UiText {
            string: "PAUSED".into(),
            scale: 4.0,
        },
        UiColor([0.9, 0.95, 1.0, 1.0]),
        UiMenu(PAUSE_MENU.into()),
        UiAnchor {
            point: UiAnchorPoint::Center,
            offset: [0.0, -110.0],
        },
    ));
    spawn_ui_button(
        world,
        "RESUME",
        [220.0, 44.0],
        [0.2, 0.25, 0.32, 1.0],
        [0.9, 0.95, 1.0, 1.0],
        3.0,
        UiAnchor {
            point: UiAnchorPoint::Center,
            offset: [0.0, -40.0],
        },
        PAUSE_MENU,
    );
    spawn_ui_button(
        world,
        "QUIT",
        [220.0, 44.0],
        [0.2, 0.25, 0.32, 1.0],
        [0.9, 0.95, 1.0, 1.0],
        3.0,
        UiAnchor {
            point: UiAnchorPoint::Center,
            offset: [0.0, 20.0],
        },
        PAUSE_MENU,
    );
}

// ---------------------------------------------------------------------------
// Update 1：逻辑推进 + 接触事件 + 状态机参数 + 音频 + 菜单
// ---------------------------------------------------------------------------

/// Update 1：读输入与时间 → **R34** 压力板接触事件消费 → 推进纯逻辑 →
/// 写玩家 / 守卫 → **R38** 混合空间参数 → **R25** 空间音频 →
/// **R41** Esc 菜单栈与 `UiClickEvent` 消费。菜单栈非空 = 暂停（逻辑
/// 冻结；物理 / 动画表现照常——v1 暂停只冻结对局状态机，诚实边界）。
fn run_infiltration(world: &World) {
    let dt = world
        .try_resource::<Time>()
        .map(|time| time.delta_secs())
        .unwrap_or(0.0);
    let paused = world
        .try_resource::<UiMenuStack>()
        .map(|stack| !stack.is_empty())
        .unwrap_or(false);

    // ---- R41：Esc 切换暂停菜单栈 + 按钮点击消费 ----
    let toggle_pause = world
        .try_resource::<Input>()
        .map(|input| input.key_pressed(KeyCode::Escape))
        .unwrap_or(false);
    if toggle_pause {
        if let Some(mut stack) = world.try_resource_mut::<UiMenuStack>() {
            if stack.top_menu() == Some(PAUSE_MENU) {
                stack.pop_menu();
            } else {
                stack.push_menu(PAUSE_MENU);
            }
        }
    }
    // UiClickEvent 直接消费（打磨 V：事件生命周期闭合 §8.22①）——
    // `update_ui_focus` 在 PostUpdate 产出，事件可读窗口覆盖下一帧全程，
    // 本帧 Update 经上一代窗口读到并 drain（不再需要 MenuCommand 信箱）
    let mut resume_requested = false;
    let mut quit_requested = false;
    for click in world.events_mut::<UiClickEvent>().drain() {
        let label = world
            .get::<UiButton>(click.entity)
            .map(|button| button.label.clone());
        match label.as_deref() {
            Some("RESUME") => resume_requested = true,
            Some("QUIT") => quit_requested = true,
            _ => {}
        }
    }
    if resume_requested || quit_requested {
        if let Some(mut stack) = world.try_resource_mut::<UiMenuStack>() {
            if stack.top_menu() == Some(PAUSE_MENU) {
                stack.pop_menu();
            }
        }
    }
    if quit_requested {
        // R41 设置持久化：QUIT 落盘后请求退出（AppExit 由主循环帧尾
        // 退出检查读本帧产出——打磨 V 调序后 Update / PostUpdate 发出的
        // 都能读到；headless 测试从不点击 QUIT，无写盘竞态）
        if let Some(settings) = world.try_resource::<Settings>() {
            let _ = settings.save(plugin_settings_path(world));
        }
        world.try_send_event(AppExit);
    }

    // ---- 输入翻译：相机固定俯角 → 世界系与屏幕系一致（W = -Z）----
    let input = world
        .try_resource::<Input>()
        .map(|input| {
            let x = (input.key(KeyCode::KeyD) as i32 - input.key(KeyCode::KeyA) as i32) as f32;
            let z = (input.key(KeyCode::KeyS) as i32 - input.key(KeyCode::KeyW) as i32) as f32;
            InfiltrationInput {
                move_dir: Vec2::new(x, z),
                sneak: input.key(KeyCode::ShiftLeft) || input.key(KeyCode::ShiftRight),
                interact: input.key_pressed(KeyCode::KeyE),
                dt,
            }
        })
        .unwrap_or(InfiltrationInput {
            move_dir: Vec2::ZERO,
            sneak: false,
            interact: false,
            dt,
        });

    let Some(mut state) = world.try_resource_mut::<InfiltrationState>() else {
        return;
    };
    if !paused {
        // ---- R34：压力板 sensor 的接触事件消费（begin/end 成对计数，
        // begin 单向闩锁开门）。事件由 PhysicsPlugin 的 step_physics 在
        // 本阶段更早的批次产出（注册序在先），读到的是本帧物理结果 ----
        if let Some(ents) = world.try_resource::<InfiltrationEntities>() {
            let plate = ents.plate;
            // drain 消费（打磨 V 消费契约：事件可读窗口跨两帧，iter 不取
            // 走会在下一帧重复计数 begin）
            for event in world.events_mut::<CollisionStarted>().drain() {
                if event.a == plate || event.b == plate {
                    state.enter_plate();
                    state.press_plate();
                }
            }
            for event in world.events_mut::<CollisionEnded>().drain() {
                if event.a == plate || event.b == plate {
                    state.leave_plate();
                }
            }
        }
        state.step(&input);
    }

    // ---- 事件分流：横幅置位（文本就地写，HUD 系统按计时窗口维持）----
    for event in &state.frame_events {
        let banner: Option<(&str, [f32; 4])> = match *event {
            InfiltrationEvent::Detection { count, .. } => Some((
                match count {
                    1 => "SPOTTED! 1/3",
                    2 => "SPOTTED! 2/3",
                    _ => "COMPROMISED!",
                },
                [1.0, 0.4, 0.3, 1.0],
            )),
            InfiltrationEvent::Victory { .. } => {
                Some(("EXFILTRATION COMPLETE", [0.4, 1.0, 0.6, 1.0]))
            }
            InfiltrationEvent::Defeat { .. } => Some(("MISSION FAILED", [1.0, 0.3, 0.3, 1.0])),
            _ => None,
        };
        if let Some((text, color)) = banner {
            if let Some(entity) = world.entity_named(BANNER_TEXT_NAME) {
                if let Some(mut ui_text) = world.get_mut::<UiText>(entity) {
                    ui_text.string = text.to_string();
                }
                if let Some(mut ui_color) = world.get_mut::<UiColor>(entity) {
                    ui_color.0 = color;
                }
            }
            if let Some(mut timer) = world.try_resource_mut::<BannerTimer>() {
                timer.0 = BANNER_SECS;
            }
        }
    }

    // ---- 三层结构写回 + R38 混合空间参数 + 物理代理同步 ----
    if let Some(ents) = world.try_resource::<InfiltrationEntities>() {
        if let Some(mut transform) = world.get_mut::<Transform>(ents.player_root) {
            transform.position = state.pos;
        }
        // 玩家模型：朝向 + 混合空间参数（速度比 = 水平速度 / 巡航速度）
        if let Some(mut transform) = world.get_mut::<Transform>(ents.player_model) {
            transform.rotation = Quat::from_rotation_y(state.facing);
        }
        let player_speed = Vec2::new(state.vel.x, state.vel.z).length();
        if let Some(mut player) = world.get_mut::<AnimationPlayer>(ents.player_model) {
            player.blend_param = Vec2::new((player_speed / MOVE_SPEED).min(1.0), 0.0);
        }
        // 守卫：根位置 / 模型朝向 / 混合空间参数（潜行接近也可以从动画
        // 节奏读出来——参数连续即步速连续）
        for (index, guard_view) in ents.guards.iter().enumerate() {
            let Some(guard) = state.guards.get(index) else {
                continue;
            };
            if let Some(mut transform) = world.get_mut::<Transform>(guard_view.root) {
                transform.position = guard.pos;
            }
            if let Some(mut transform) = world.get_mut::<Transform>(guard_view.model) {
                transform.rotation = Quat::from_rotation_y(guard.facing);
            }
            if let Some(mut player) = world.get_mut::<AnimationPlayer>(guard_view.model) {
                player.blend_param = Vec2::new((guard.anim_speed / MOVE_SPEED).min(1.0), 0.0);
            }
        }
    }

    // ---- R25：事件 → 空间音频（听者跟随主相机）----
    if let (Some(assets), Some(audio)) = (
        world.try_resource::<InfiltrationAssets>(),
        world.try_resource::<Audio>(),
    ) {
        let spatial = |clip: &AudioClip, at: Vec3, volume: f32| {
            audio.play_spatial(clip.clone(), at, volume, false);
        };
        for event in &state.frame_events {
            match *event {
                InfiltrationEvent::CorePicked { pos, .. } => spatial(&assets.pickup, pos, 0.3),
                InfiltrationEvent::PlatePressed { pos } => spatial(&assets.plate, pos, 0.25),
                InfiltrationEvent::DoorOpened { pos } => spatial(&assets.door, pos, 0.35),
                InfiltrationEvent::GuardStunned { pos, .. } => spatial(&assets.stun, pos, 0.4),
                InfiltrationEvent::Detection { pos, .. } => spatial(&assets.alarm, pos, 0.45),
                InfiltrationEvent::Victory { pos } => spatial(&assets.victory, pos, 0.5),
                InfiltrationEvent::Defeat { pos } => spatial(&assets.defeat, pos, 0.5),
            }
        }
        // 听者：主相机全局位姿（GlobalTransform 来自上一帧传播，滞后一帧
        // 对音频无感；headless / 离屏路径同样得到正确听者）
        if let Some(ents) = world.try_resource::<InfiltrationEntities>() {
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
}

/// QUIT 落盘路径：插件缺省路径（资源里不带路径——绕行记录见摩擦清单）。
fn plugin_settings_path(_world: &World) -> std::path::PathBuf {
    PathBuf::from(SETTINGS_PATH)
}

// ---------------------------------------------------------------------------
// Update 2（并行）：击晕 → ragdoll 命令入队（R37）
// ---------------------------------------------------------------------------

/// Update 2（**并行系统**，打磨 V 起不再独占）：消费本帧 `GuardStunned`
/// 事件 → ragdoll 生成经命令队列入队（`spawn_deferred`——实体分配是
/// 结构性修改，帧末 `apply_commands` 独占执行，并行系统吞吐保留，
/// §8.22③ 闭合）→ 守卫模型子树整体隐藏（引擎原语
/// [`set_subtree_hidden`]，§8.22⑤ 闭合）。
///
/// ragdoll 的「生成 + 立即释放（受击方向轻踢）+ 逐肢体挂球体网格与
/// 材质（渲染表现是游戏侧选择，见模块文档「击晕与表现」）」打包进同一
/// 条命令：闭包按值捕获击晕数据（`Send`），执行结果只依赖捕获值与生成
/// 时的确定性的 `RagdollDesc`——命令队列的确定性警示见
/// `iris_core::commands` 模块文档（本处闭包不读可变外部状态，回放安全）。
fn consume_game_events(world: &World) {
    let Some(events) = world
        .try_resource::<InfiltrationState>()
        .map(|state| state.frame_events.clone())
    else {
        return;
    };
    for event in events {
        if let InfiltrationEvent::GuardStunned { pos, facing, index } = event {
            // 瘫倒：静态肢体（动画接管期）→ 立即释放（物理接管期）——
            // 帧末独占执行
            let foot = pos - Vec3::new(0.0, HALF_HEIGHT, 0.0);
            if let Some(queue) = world.try_resource::<CommandQueue>() {
                queue.spawn_deferred(move |world: &mut World| {
                    let desc = RagdollDesc::humanoid();
                    let limbs = spawn_ragdoll_kinematic(world, foot, &desc);
                    // 肢体可见化：ragdoll 只提供物理原语（Transform +
                    // Collider + RigidBody），渲染表现是游戏侧的选择——
                    // 不挂网格的话守卫模型子树一隐藏，瘫倒肢体就「凭空
                    // 消失」。逐肢体挂球体网格（半径 = 肢体定义半径，
                    // 视觉与碰撞体同源）+ 守卫配色暗材质 + 可见。
                    let mut meshes = world.resource_mut::<MeshStore>();
                    let mesh_handles: Vec<_> = desc
                        .limbs
                        .iter()
                        .map(|def| meshes.add(MeshData::uv_sphere(def.radius, 16, 12)))
                        .collect();
                    drop(meshes);
                    let material = world
                        .resource_mut::<MaterialStore>()
                        .add(MaterialDef::solid(0.55, 0.2, 0.2, 1.0));
                    for (&limb, mesh) in limbs.iter().zip(&mesh_handles) {
                        world.insert(limb, *mesh);
                        world.insert(limb, material);
                        world.insert(limb, Visibility::VISIBLE);
                    }
                    let kick_dir = Vec3::new(-facing.sin(), 0.0, -facing.cos());
                    release_ragdoll(world, &limbs, Vec3::ZERO, Some((kick_dir, 1.2)));
                });
            }
            // 守卫模型隐藏（根 + 全子树：模型 / 腿链 / 网格节点）
            let guard_root = world
                .try_resource::<InfiltrationEntities>()
                .and_then(|ents| ents.guards.get(index).map(|guard_view| guard_view.root));
            if let Some(root) = guard_root {
                set_subtree_hidden(world, root, true);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Update 3：表现（核心 / 门 / 压力板 / 出口信标）
// ---------------------------------------------------------------------------

/// Update 3：逻辑状态 → 表现。核心自转浮动（拾取后隐藏）；压力门开门 =
/// 标记语义（`Visibility::HIDDEN` + 碰撞组清零——静态体不移除的 v1
/// 绕行，见摩擦清单）；压力板踩下换发光强度；出口信标慢速自旋。
fn update_infiltration_visuals(world: &World) {
    let Some(state) = world.try_resource::<InfiltrationState>() else {
        return;
    };
    let Some(ents) = world.try_resource::<InfiltrationEntities>() else {
        return;
    };
    let elapsed = world
        .try_resource::<Time>()
        .map(|time| time.elapsed_secs())
        .unwrap_or(0.0);

    for (index, &entity) in ents.cores.iter().enumerate() {
        let Some(collected) = state.cores.get(index) else {
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
            transform.position.y = 0.5 + (elapsed * 2.5 + index as f32 * 0.7).sin() * 0.12;
        }
    }

    // 压力门：开门标记（视觉隐藏 + 碰撞失效；物理世界里的静态体无法
    // despawn——`&World` 系统不能改结构，用组位清零让配对判定恒假）
    if state.door_open {
        if let Some(mut visibility) = world.get_mut::<Visibility>(ents.door) {
            *visibility = Visibility::HIDDEN;
        }
        if let Some(mut collider) = world.get_mut::<Collider>(ents.door) {
            collider.groups = 0;
            collider.mask = 0;
        }
    }

    // 出口信标：核心全收后才点亮（绿色指示「可以撤离」）
    if let Some(mut transform) = world.get_mut::<Transform>(ents.exit_beacon) {
        transform.rotation = Quat::from_rotation_y(elapsed * 1.5);
    }
}

// ---------------------------------------------------------------------------
// Update 4：HUD
// ---------------------------------------------------------------------------

/// Update 4：HUD 刷新——核心 / 发现 / 警觉每帧重写，横幅按阶段与计时
/// 切换。文本实体经 `Name` 按名定位。
fn update_infiltration_hud(world: &World) {
    let Some(state) = world.try_resource::<InfiltrationState>() else {
        return;
    };
    let dt = world
        .try_resource::<Time>()
        .map(|time| time.delta_secs())
        .unwrap_or(0.0);

    let set_text = |world: &World, name: &str, text: String| {
        if let Some(entity) = world.entity_named(name) {
            if let Some(mut ui_text) = world.get_mut::<UiText>(entity) {
                ui_text.string = text;
            }
        }
    };
    set_text(
        world,
        CORES_TEXT_NAME,
        format!("CORES {}/{}", state.cores_collected(), state.cores.len()),
    );
    set_text(
        world,
        SPOTTED_TEXT_NAME,
        format!(
            "SPOTTED {}/{}",
            state.detections,
            crate::logic::MAX_DETECTIONS
        ),
    );
    let alert = state.max_alert();
    set_text(
        world,
        ALERT_TEXT_NAME,
        format!("ALERT {}%", (alert * 100.0).round() as u32),
    );
    if let Some(entity) = world.entity_named(ALERT_TEXT_NAME) {
        if let Some(mut color) = world.get_mut::<UiColor>(entity) {
            color.0 = if alert > 0.6 {
                [1.0, 0.25, 0.2, 1.0]
            } else if alert > 0.0 {
                [1.0, 0.75, 0.3, 1.0]
            } else {
                [0.9, 0.9, 0.9, 1.0]
            };
        }
    }
    // 横幅：终局态优先，否则按计时窗口显示
    if let Some(mut timer) = world.try_resource_mut::<BannerTimer>() {
        timer.0 = (timer.0 - dt).max(0.0);
        if let Some(entity) = world.entity_named(BANNER_TEXT_NAME) {
            if let Some(mut text) = world.get_mut::<UiText>(entity) {
                text.string = match state.phase {
                    Phase::Victory => "EXFILTRATION COMPLETE".into(),
                    Phase::Defeat => "MISSION FAILED".into(),
                    Phase::Playing if timer.0 > 0.0 => text.string.clone(),
                    _ => String::new(),
                };
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 单测（插件侧组件语义；全流程走 tests/headless.rs）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 自动玩家 drive 的簿记语义：只在到点时推进游标，输入恒潜行。
    #[test]
    fn autopilot_drive_advances_cursor_and_always_sneaks() {
        use crate::logic::{AUTOPILOT_ROUTE, AutoPilot, SPAWN_FOOT};
        let mut state = InfiltrationState::new();
        // 出生点 → 首路点距离 > 到点半径：游标不动
        let input = AutoPilot.drive(&mut state);
        assert_eq!(state.autopilot_route, 0);
        assert!(input.sneak, "自动玩家恒潜行");
        // 传送到首路点上：推进游标
        state.pos = Vec3::new(
            AUTOPILOT_ROUTE[0].x,
            SPAWN_FOOT.y + HALF_HEIGHT,
            AUTOPILOT_ROUTE[0].y,
        );
        let _ = AutoPilot.drive(&mut state);
        assert_eq!(state.autopilot_route, 1, "到点应推进路线游标");
    }
}
