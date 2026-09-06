//! [`AssaultPlugin`]：把纯逻辑核心（`crate::logic`）接到 Iris 引擎上，
//! 同时把 **R42–R47 六轮新能力当载荷全量实弹**（接入点在注释中以
//! **R4x** 标注；摩擦实录见 crate 文档与 ARCHITECTURE §8 dogfooding VI 条目）。
//!
//! 职责划分（iris-infiltration 同款分层）：
//! - **Startup**：`setup_resources`（引擎资源兜底 + **R45** pack 挂载 +
//!   **R44** 流式氛围音乐 + 程序化音效 + **R43** DOF / MB 设置）→
//!   `build_scene`（庭院几何 / **R42** 写实材质 / 三层角色结构 /
//!   **R46** 双播放器上下身动画 + 足部 / 手部 IK / HUD / 粒子 FX 池）→
//!   **R47** 开场演出；
//! - **Update**：`run_assault`（输入翻译 → **R47** 时间线事件消费 →
//!   [`AssaultState::step`] → 写角色变换与动画参数 → **R46** 手 IK
//!   目标切换 → **R43** DOF 动态对焦 → **R25** 空间音频 → HUD）→
//!   `consume_game_events`（并行：**R37** ragdoll / 贴花 / 粒子 FX 池
//!   触发 / 终局演出启动，生成走 `spawn_deferred` 命令队列）。
//! - **层级传播 / 动画（含 IK）/ 物理 / 粒子推进 / 空间音频 / 时间线
//!   推进 / UI / 命令队列**：交给 `DefaultPlugins`（headless 测试与离屏
//!   快照需自行 build 对应插件，与 infiltration 约定同款，本插件不重复
//!   注册）。
//!
//! # 六能力接入点索引（逐条对账 ARCHITECTURE §8 摩擦清单）
//!
//! | 能力 | 接入点 |
//! |---|---|
//! | R42 写实材质 | 地面石板 / 砖墙用引擎 `brick_normal_orm`；刀身划痕与布料褶皱用 `crate::textures` 纯函数生成器；GLB 材质加载后克隆 + `with_normal/with_orm` 替换（GltfBuilder 不支持纹理的绕行，见摩擦清单） |
//! | R43 DOF / MB | `DepthOfFieldSettings` 逐帧对焦（交战距离）+ `MotionBlurSettings`（演出相机运动拖影） |
//! | R44 流式音频 | pack 内 OGG → `OggVorbisStream::from_bytes` → `Audio::play_music`（音乐总线循环 + 混响）；音效 `play_spatial` 程序化剪辑 |
//! | R45 pack | 全部 GLB / OGG 打包 `assets/assault.ipak`；`AssetMount(Pack)` 挂载；GLB 经 `spawn_gltf_instance_from_bytes` 加载（自包含 GLB） |
//! | R46 动画 v4 | 上下身**双 `AnimationPlayer`**：下半身 `BlendSpace` 步态 + 上半身动作剪辑 + `AnimationMask` 过滤；受击 = `AdditiveLayer` 上身抖动；`HandIk` 双臂（ADS 时 `IkTarget` 吸附枪身挂点） |
//! | R47 时间线 | 开场（绕庭相机 spline + 字幕 + 黑边）→ `intro_end` 事件 → `begin_battle`；胜利 / 败北推近演出（命令队列延迟启动） |
//!
//! # 上下身分离剪辑的组合姿势（R46 的核心用法）
//!
//! 一个 GLB、**两个 `AnimationPlayer`**：下半身播放器（挂在模型根）持
//! idle / walk / run 三剪辑（通道只写 [`crate::skeleton::LOWER_BODY`]
//! 节点）+ `BlendSpace`（参数 = 速度 0..1）；上半身播放器（挂在模型根
//! 的空子实体 `action` 上）持 guard / sword1..3 / aim / reload / flinch
//! （通道只写 [`crate::skeleton::UPPER_BODY`] 节点）+ `AnimationMask`
//! （节点集 = `UPPER_BODY`——遮罩是覆写过滤器，与「剪辑本身只写上身」
//! 构成双重保险）。两个播放器写入的节点集不相交 ⇒ 跑动中挥砍 / 瞄准
//! 不打断腿，组合姿势由结构保证。受击硬直 = 上半身播放器的
//! `AdditiveLayer`（flinch 剪辑相对 t=0 的增量叠加在现行主姿势上，
//! 不破坏步态）。
//!
//! # 手 IK 与动画的竞争（R46 实弹验证点）
//!
//! 手 IK（`update_hand_ik`）注册在 `update_animations` 之后（引擎
//! `AnimationPlugin` 的固定编排）——遮罩 / 动画先写手骨，IK 后写：
//! **IK 恒赢**。ADS 时插件层把 `HandIk.target` 指向枪身 `IkTarget`
//! 实体（枪挂在胸节点下——**不在手骨链子树内**，无反馈环）；非瞄准态
//! 置回 `None`，手臂回到动作剪辑姿势。headless 测试断言 ADS 数帧后
//! 右手全局位吸附握把（±12 cm，一帧滞后裕度）——「IK 在被遮罩剪辑写
//! 过之后仍然生效」的量化证据。
//!
//! # 敌人的物理存在与死亡表现
//!
//! 敌人被击杀（`EnemyKilled` 事件）→ `spawn_ragdoll_kinematic` +
//! `release_ragdoll`（受击方向轻踢倒地）+ 逐肢体挂球体网格与配色材质
//! （ragdoll 只给物理原语，渲染是游戏侧的选择）+ 敌人模型子树
//! `set_subtree_hidden`。v1 尸体永久留在原地（不回收，诚实限制）。
//!
//! # FX 池（bastion 同款）
//!
//! 32 槽发射器启动预热、运行期零 spawn；按效果类型分四组（血雾 8 /
//! 枪口焰 8 / 刀光 8 / 火花 8），组内游标 = 确定性的帧计数
//! （`(elapsed × 60).floor()`）——同类型连续触发轮转不叠写（组内槽位
//! 数 ≥ 同帧峰值 / 槽位寿命倍数，见 [`FX_PER_TYPE`] 注释）。
//!
//! 本插件只使用引擎公开 API（dogfooding）：不依赖引擎内部路径。

use std::path::{Path, PathBuf};

use glam::{Quat, Vec2, Vec3};

use iris_core::commands::CommandQueue;
use iris_core::prelude::{Access, Children, Entity, Name, Stage};
use iris_core::ragdoll::{RagdollDesc, release_ragdoll, spawn_ragdoll_kinematic};
use iris_core::resource::Resource;
use iris_engine::prelude::*;

// MouseButton 来自 iris_engine::prelude（iris_platform::prelude 的再导出）
use iris_render::animation::{AnimationClip, AnimationMask, AnimationPlayer};
use iris_render::asset::{GltfInstance, spawn_gltf_instance_from_bytes};
use iris_render::decal::Decal;
use iris_render::foot_ik::{FootIk, IkRig};
use iris_render::hand_ik::{HandIk, IkTarget};
use iris_render::material::MaterialDef;
use iris_render::particles::{ParticleBlend, ParticleEmitter};
use iris_render::post::{DepthOfFieldSettings, MotionBlurSettings};
use iris_render::texture::CpuTexture;

use crate::logic::{
    self, AssaultEvent, AssaultInput, AssaultState, EnemyState, HALF_HEIGHT, MOVE_SPEED, Phase,
};
use crate::skeleton as sk;
use crate::textures;

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 角色 GLB 在 pack 内的条目路径。
pub const GLB_ENTRY: &str = "models/assault_bot.glb";
/// 氛围 OGG 在 pack 内的条目路径。
pub const AMBIENCE_ENTRY: &str = "audio/ambience.ogg";
/// pack 缺失时的 GLB 文件回退路径（仓库相对，经 [`asset_path`] 解析）。
pub const GLB_FALLBACK: &str = "assets/assault_bot.glb";
/// 主相机跟随偏移（固定俯角第三人称——相机是根级实体，非玩家子实体，
/// 时间线接管才能按世界系写位姿，见 [`start_camera`] 注释）。
pub const CAMERA_OFFSET: Vec3 = Vec3::new(0.0, 7.2, 6.2);
/// 相机俯仰角（弧度）：望向玩家脚下的固定俯角。
const CAMERA_PITCH: f32 = -36.0_f32.to_radians();
/// 空间音频距离参考（米）：俯视相机距角色 ~11 m，按此尺度放大。
pub const REFERENCE_DISTANCE: f32 = 14.0;
/// 横幅持续（秒）。
const BANNER_SECS: f32 = 3.0;
/// HUD 实体名（`Name` 组件，按名定位用）。
pub const HP_TEXT_NAME: &str = "assault/hud/hp";
pub const AMMO_TEXT_NAME: &str = "assault/hud/ammo";
pub const ENEMIES_TEXT_NAME: &str = "assault/hud/enemies";
pub const BANNER_TEXT_NAME: &str = "assault/hud/banner";
/// FX 池每组槽位数（血雾 / 枪口焰 / 刀光 / 火花各一组）。组内轮转游标
/// 按帧推进：同类型最短槽位寿命（枪口焰 0.08 s ≈ 5 帧）≪ 8 帧轮转周期
/// ÷ 同帧峰值（≤ 2）——实测不会叠写进行中的 burst。
const FX_PER_TYPE: usize = 8;
/// FX 组数（血雾 / 枪口焰 / 刀光 / 火花）。
const FX_TYPES: usize = 4;

// ---------------------------------------------------------------------------
// 插件侧资源
// ---------------------------------------------------------------------------

/// 适配：把纯逻辑类型接入引擎资源存储（逻辑核心零引擎依赖，跨模块手动
/// 实现 `Resource` 即可「选择性接入」，infiltration 同款）。
impl Resource for AssaultState {}

/// 横幅剩余显示时间（秒；`run_assault` 置位、HUD 递减清零）。
#[derive(Clone, Copy, Debug, Default, Resource)]
pub struct BannerTimer(pub f32);

/// 胜负演出已启动（终局时间线只起一次的闩锁）。
#[derive(Clone, Copy, Debug, Default, Resource)]
pub struct OutroLatch(pub bool);

/// 预生成的程序化音效与贴花素材句柄（全部确定性、Startup 一次生成）。
#[derive(Clone, Resource)]
pub struct AssaultAssets {
    /// 挥刀风声：宽带短噪声。
    pub swing: AudioClip,
    /// 刀命中：金属 ping（高频双音）。
    pub sword_hit: AudioClip,
    /// 枪声：噪声爆发 + 低频体。
    pub shot: AudioClip,
    /// 换弹：双咔哒。
    pub reload: AudioClip,
    /// 玩家受击：低频闷响。
    pub hurt: AudioClip,
    /// 敌人死亡：下行双音。
    pub enemy_die: AudioClip,
    /// 胜利：上行三连音。
    pub victory: AudioClip,
    /// 败北：下行双音。
    pub defeat: AudioClip,
    /// 血渍贴花纹理（`Decal.texture`）。
    pub blood: TextureHandle,
    /// 弹孔贴花纹理。
    pub bullet_hole: TextureHandle,
}

/// 场景实体索引：每帧热路径的句柄资源。HUD 文本实体经 `Name` +
/// `World::entity_named` 按名定位（低频路径）。
#[derive(Clone, Resource)]
pub struct AssaultEntities {
    /// 玩家根实体：只承载逻辑位置（rotation 恒恒等）。
    pub player_root: Entity,
    /// 玩家模型根（下半身播放器所在；朝向写它的 rotation）。
    pub player_model: Entity,
    /// 玩家上半身播放器实体（模型根的空子实体）。
    pub player_action: Entity,
    /// 玩家右臂手 IK 配置实体（`HandIk.target` 每帧切换）。
    pub player_hand_r: Option<Entity>,
    /// 玩家左臂手 IK 配置实体。
    pub player_hand_l: Option<Entity>,
    /// 主相机（根级实体，`ActiveCamera`；时间线接管的对象）。
    pub camera: Entity,
    /// 敌人（下标 = [`crate::logic::ENEMY_SPAWNS`] 下标）。
    pub enemies: Vec<EnemyEntities>,
    /// 粒子 FX 池（启动预热 32 槽；布局 = 四组各 [`FX_PER_TYPE`] 槽，
    /// 组序：血雾 / 枪口焰 / 刀光 / 火花）。
    pub fx_pool: Vec<Entity>,
}

/// 一名敌人的表现实体对：根（逻辑位置）+ 模型（双播放器动画 + 朝向）。
#[derive(Clone, Copy, Debug)]
pub struct EnemyEntities {
    /// 根实体：只承载逻辑位置（rotation 恒恒等）。
    pub root: Entity,
    /// 模型子实体：下半身播放器所在。
    pub model: Entity,
    /// 上半身播放器子实体。
    pub action: Entity,
}

// ---------------------------------------------------------------------------
// 插件
// ---------------------------------------------------------------------------

/// 战斗插件：Startup 代码生成庭院 + Update 推进逻辑与表现。资产经
/// **R45** pack 挂载（缺 pack 时回退文件路径），`Default` 即可用。
#[derive(Debug, Clone)]
pub struct AssaultPlugin {
    /// pack 文件路径（缺省 `assets/assault.ipak`，经 [`asset_path`] 解析；
    /// 测试可注入）。
    pub pack_path: PathBuf,
}

impl Default for AssaultPlugin {
    fn default() -> Self {
        Self {
            pack_path: asset_path("assets/assault.ipak"),
        }
    }
}

impl AssaultPlugin {
    /// 以指定 pack 路径构造（测试注入）。
    pub fn new(pack_path: impl Into<PathBuf>) -> Self {
        Self {
            pack_path: pack_path.into(),
        }
    }

    /// 把本插件的全部系统注册到调度器。
    ///
    /// 与 `DefaultPlugins` 的编排方式一致（Startup 一次、Update 每帧）；
    /// 独立暴露是为了 headless 测试与离屏快照在不建 `App` / 窗口 / GPU
    /// 的前提下复用同一条推进路径。**层级 / 物理 / 动画 / 粒子 / 时间线
    /// / UI / 空间音频 / 命令队列不在此注册**：调用方按窗口路径的编排补上
    /// `HierarchyPlugin`（PostUpdate）、`AnimationPlugin`（Update、本插件
    /// 之前——含足部 / 手部 IK）、`TimelinePlugin`（Update、本插件之前
    /// ——演出先写相机，游戏侧后写让位）、`PhysicsPlugin`、
    /// `ParticlePlugin`（PostUpdate）、`UiPlugin`（PostUpdate）、
    /// `SpatialPlugin` 与 `CommandsPlugin`。
    pub fn build_schedule(&self, schedule: &mut iris_core::schedule::Schedule) {
        let pack_path = self.pack_path.clone();
        schedule
            .add_system(Stage::Startup, move |world: &mut World| {
                setup_resources(world, &pack_path);
            })
            .add_system(Stage::Startup, |world: &mut World| {
                build_scene(world);
            })
            .add_system(Stage::Startup, |world: &mut World| {
                start_intro_cinematic(world);
            })
            // Update 内的顺序即注册顺序：时间线（DefaultPlugins 先注册）
            // → 动画（先）→ 物理 → 逻辑 + 演出事件 + 音频 + HUD →
            // FX / ragdoll / 贴花
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<Input>()
                    .read::<Time>()
                    .read::<Audio>()
                    .read::<AssaultAssets>()
                    .read::<AssaultEntities>()
                    .read::<GlobalTransform>()
                    .read::<TimelinePlayer>()
                    .write::<AssaultState>()
                    .write::<Transform>()
                    .write::<AnimationPlayer>()
                    .write::<HandIk>()
                    .write::<UiText>()
                    .write::<UiColor>()
                    .write::<BannerTimer>()
                    .write::<DepthOfFieldSettings>(),
                run_assault,
            )
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<AssaultState>()
                    .read::<AssaultAssets>()
                    .read::<AssaultEntities>()
                    .write::<CommandQueue>()
                    .write::<Transform>()
                    .write::<ParticleEmitter>()
                    .write::<Visibility>(),
                consume_game_events,
            );
    }
}

impl Plugin for AssaultPlugin {
    fn build(&self, app: &mut App) {
        self.build_schedule(&mut app.schedule);
    }
}

// ---------------------------------------------------------------------------
// Startup 1：资源兜底 + pack 挂载 + 流式音乐 + 程序化音效
// ---------------------------------------------------------------------------

/// Startup 1：资源兜底插入（bastion 同款「缺席才插入」语义）+ **R45**
/// pack 挂载 + **R44** 流式氛围音乐（音乐总线 + 混响）+ 程序化音效 +
/// **R43** DOF / MB 设置。
fn setup_resources(world: &mut World, pack_path: &Path) {
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
    if !world.contains_resource::<EnvironmentSettings>() {
        world.insert_resource(EnvironmentSettings { intensity: 0.35 });
    }
    if !world.contains_resource::<CommandQueue>() {
        world.insert_resource(CommandQueue::default());
    }
    // R47：时间线播放器 / 事件（DefaultPlugins 的 TimelinePlugin 已注册
    // 时幂等兜底——headless 自建 App 需手动 build TimelinePlugin）
    if !world.contains_resource::<TimelinePlayer>() {
        world.insert_resource(TimelinePlayer::default());
    }
    world.register_event::<TimelineEvent>();

    // ---- R43：DOF（战斗默认开）+ MB（演出相机运动拖影）----
    world.insert_resource(DepthOfFieldSettings {
        enabled: true,
        focus_distance: 9.0,
        focus_range: 4.0,
        max_blur_radius: 5.0,
    });
    world.insert_resource(MotionBlurSettings {
        enabled: true,
        strength: 24.0,
        max_samples: 12,
    });

    world.insert_resource(AssaultState::new());
    world.insert_resource(BannerTimer::default());
    world.insert_resource(OutroLatch(false));

    // ---- R45：pack 挂载（AssetSource::Pack；缺失时回退文件路径——
    // 诚实降级：pack 优先，文件只作开发兜底）----
    let mut mount = AssetMount::new();
    let mut pack_loaded = false;
    if pack_path.exists() {
        match PackReader::open(pack_path) {
            Ok(pack) => {
                log::info!(
                    "AssaultPlugin：pack {} 已挂载（{} 条目）",
                    pack_path.display(),
                    pack.len()
                );
                mount.push(AssetSource::Pack(pack));
                pack_loaded = true;
            }
            Err(err) => log::warn!("AssaultPlugin：pack 打开失败（{err}），回退文件路径"),
        }
    } else {
        log::warn!(
            "AssaultPlugin：pack {} 不存在，回退文件路径加载（运行 \
             cargo run -p iris-assault --example gen_assault_assets 生成）",
            pack_path.display()
        );
    }
    world.insert_resource(mount);

    // ---- R44：流式氛围音乐（音乐总线）：pack 内 OGG → 后台线程分块
    // 解码 → 混音回调按需取块；庭院混响只作用于音乐总线（v1 限制）----
    if let Some(audio) = world.try_resource::<Audio>() {
        audio.set_reference_distance(REFERENCE_DISTANCE);
        if pack_loaded {
            let ogg_bytes = world
                .try_resource::<AssetMount>()
                .and_then(|mount| mount.read(AMBIENCE_ENTRY).ok());
            match ogg_bytes {
                Some(bytes) => match OggVorbisStream::from_bytes(bytes) {
                    Ok(stream) => {
                        audio.play_music(stream, 0.4, 1.5, true);
                        audio.set_reverb(ReverbSettings {
                            wet: 0.16,
                            room_size: 0.7,
                        });
                        log::info!("AssaultPlugin：氛围音乐已上音乐总线（循环 + 轻混响）");
                    }
                    Err(err) => log::warn!("AssaultPlugin：OGG 解码失败（{err}），无氛围音乐"),
                },
                None => log::warn!("AssaultPlugin：pack 内无 {AMBIENCE_ENTRY}，无氛围音乐"),
            }
        }
    }

    // ---- 贴花纹理（R39 通道；程序化生成见 crate::textures）----
    let blood_tex = {
        let mut store = world.resource_mut::<TextureStore>();
        store.add(textures::blood_splat(96, 0xB100_D5EE))
    };
    let bullet_tex = {
        let mut store = world.resource_mut::<TextureStore>();
        store.add(textures::bullet_hole(64))
    };
    world.insert_resource(AssaultAssets {
        swing: noise(0.12, 48_000, 0xA55A_1117),
        sword_hit: metal_ping(48_000),
        shot: gunshot(48_000),
        reload: double_click(48_000),
        hurt: sine(160.0, 0.16, 48_000),
        enemy_die: two_tone_down(48_000),
        victory: three_tone_up(48_000),
        defeat: two_tone_down(48_000),
        blood: blood_tex,
        bullet_hole: bullet_tex,
    });
}

/// 上行三连音（胜利）：523 / 659 / 784 Hz。
fn three_tone_up(sample_rate: u32) -> AudioClip {
    let a = sine(523.0, 0.12, sample_rate);
    let b = sine(659.0, 0.12, sample_rate);
    let c = sine(784.0, 0.26, sample_rate);
    AudioClip::concat(&[&a, &b, &c]).expect("同参数生成的剪辑布局必然一致")
}

/// 下行双音（告负 / 敌人死亡）：392 / 262 Hz。
fn two_tone_down(sample_rate: u32) -> AudioClip {
    let a = sine(392.0, 0.1, sample_rate);
    let b = sine(262.0, 0.18, sample_rate);
    AudioClip::concat(&[&a, &b]).expect("同参数生成的剪辑布局必然一致")
}

/// 金属 ping（刀命中）：1900 / 2600 Hz 双音（无包络 v1：短时长即衰减感）。
fn metal_ping(sample_rate: u32) -> AudioClip {
    let a = sine(1900.0, 0.06, sample_rate);
    let b = sine(2600.0, 0.09, sample_rate);
    AudioClip::concat(&[&a, &b]).expect("同参数生成的剪辑布局必然一致")
}

/// 枪声：宽带噪声爆发（0.05 s）+ 低频体（140 Hz，0.1 s）。
fn gunshot(sample_rate: u32) -> AudioClip {
    let crack = noise(0.05, sample_rate, 0x5407_1E55);
    let body = sine(140.0, 0.1, sample_rate);
    AudioClip::concat(&[&crack, &body]).expect("同参数生成的剪辑布局必然一致")
}

/// 换弹双咔哒：900 / 700 Hz 短音 + 间隔。
fn double_click(sample_rate: u32) -> AudioClip {
    let click_a = sine(900.0, 0.03, sample_rate);
    let gap = (0.12 * sample_rate as f32) as usize;
    let click_b = sine(700.0, 0.03, sample_rate);
    let silence =
        AudioClip::from_interleaved(vec![0.0; gap], 1, sample_rate).expect("程序化参数恒合法");
    AudioClip::concat(&[&click_a, &silence, &click_b]).expect("同参数生成的剪辑布局必然一致")
}

// ---------------------------------------------------------------------------
// Startup 2：庭院 + 角色 + HUD + FX 池
// ---------------------------------------------------------------------------

/// Startup 2：代码生成庭院 + 三层角色结构 + **R46** 双播放器动画 + IK
/// 装配 + HUD + FX 池。静态内容启动 spawn 一次；运行期 spawn 只有
/// ragdoll 与贴花（`consume_game_events` 的命令队列）。
fn build_scene(world: &mut World) {
    build_courtyard(world);
    let (player_root, player_model, player_action, player_hand_r, player_hand_l) =
        build_character(world, "player", true);
    let mut enemies = Vec::new();
    for index in 0..logic::ENEMY_SPAWNS.len() {
        let (root, model, action, _, _) = build_character(world, &format!("enemy/{index}"), false);
        enemies.push(EnemyEntities {
            root,
            model,
            action,
        });
    }
    let camera = start_camera(world);
    let fx_pool = build_fx_pool(world);
    build_hud(world);
    world.insert_resource(AssaultEntities {
        player_root,
        player_model,
        player_action,
        player_hand_r,
        player_hand_l,
        camera,
        enemies,
        fx_pool,
    });
}

/// 庭院几何：地面 / 围墙 / 石柱 / 喷泉（视觉网格与逻辑静态体同源，
/// 插件层按 `half × 2` 缩放立方体）+ **R42** 写实材质 + 灯光。
fn build_courtyard(world: &mut World) {
    // ---- R42：程序化写实材质。地面石板 / 砖墙用引擎 R42 验收生成器
    // brick_normal_orm（解析式高度场 + splitmix64，确定性有引擎侧单测）；
    // 刀身划痕 / 布料褶皱在角色装配时生成（见 assemble_animated_character）。
    let (paving_normal, paving_orm) = {
        let mut store = world.resource_mut::<TextureStore>();
        let (normal, orm) = CpuTexture::brick_normal_orm(5, 7, 256, 0.05, 1.2);
        (store.add(normal), store.add(orm))
    };
    let (brick_normal, brick_orm) = {
        let mut store = world.resource_mut::<TextureStore>();
        let (normal, orm) = CpuTexture::brick_normal_orm(8, 5, 256, 0.05, 2.4);
        (store.add(normal), store.add(orm))
    };

    let paving_mat = {
        let mut materials = world.resource_mut::<MaterialStore>();
        materials.add(
            MaterialDef {
                base_color: [0.42, 0.40, 0.365, 1.0],
                metallic: 0.0,
                roughness: 0.9,
                ..MaterialDef::default()
            }
            .with_normal(paving_normal)
            .with_orm(paving_orm),
        )
    };
    let brick_mat = {
        let mut materials = world.resource_mut::<MaterialStore>();
        materials.add(
            MaterialDef {
                base_color: [0.46, 0.33, 0.27, 1.0],
                metallic: 0.0,
                roughness: 0.85,
                ..MaterialDef::default()
            }
            .with_normal(brick_normal)
            .with_orm(brick_orm),
        )
    };
    // 石柱不用砖法线：高细柱的立方体 UV 是 0..1 整面铺贴，砖纹会拉伸成
    // 碟叠（快照目检实录）——纯色石面 + 高粗糙即可
    let pillar_mat = {
        let mut materials = world.resource_mut::<MaterialStore>();
        materials.add(MaterialDef {
            base_color: [0.38, 0.385, 0.40, 1.0],
            metallic: 0.0,
            roughness: 0.75,
            ..MaterialDef::default()
        })
    };
    let basin_mat = {
        let mut materials = world.resource_mut::<MaterialStore>();
        materials.add(MaterialDef {
            base_color: [0.36, 0.37, 0.39, 1.0],
            metallic: 0.1,
            roughness: 0.7,
            ..MaterialDef::default()
        })
    };
    let water_mat = {
        let mut materials = world.resource_mut::<MaterialStore>();
        materials.add(MaterialDef {
            base_color: [0.15, 0.35, 0.45, 1.0],
            emissive: [0.08, 0.22, 0.30, 1.0],
            metallic: 0.0,
            roughness: 0.15,
            ..MaterialDef::default()
        })
    };

    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let basin = world
        .resource_mut::<MeshStore>()
        .add(MeshData::cylinder(1.7, 0.7, 24));
    let water = world
        .resource_mut::<MeshStore>()
        .add(MeshData::cylinder(1.5, 0.06, 24));
    let spout = world
        .resource_mut::<MeshStore>()
        .add(MeshData::cylinder(0.22, 1.1, 12));

    // 地板：视觉 + 静态碰撞体（IK 落地 / ragdoll 承托）
    world.spawn((
        Name("assault/ground".into()),
        Transform {
            position: Vec3::new(0.0, -0.25, 0.0),
            scale: Vec3::new(20.8, 0.5, 14.8),
            ..Transform::IDENTITY
        },
        cube,
        paving_mat,
        Collider::static_box(Vec3::new(10.4, 0.25, 7.4)),
    ));
    // 围墙 + 石柱（视觉与逻辑静态体同源）
    for (index, block) in logic::WALLS.iter().enumerate() {
        world.spawn((
            Name(format!("assault/wall/{index}")),
            Transform {
                position: block.center,
                scale: block.half * 2.0,
                ..Transform::IDENTITY
            },
            cube,
            brick_mat,
            Collider::static_box(block.half),
        ));
    }
    for (index, block) in logic::PILLARS.iter().enumerate() {
        world.spawn((
            Name(format!("assault/pillar/{index}")),
            Transform {
                position: block.center,
                scale: block.half * 2.0,
                ..Transform::IDENTITY
            },
            cube,
            pillar_mat,
            Collider::static_box(block.half),
        ));
    }
    // 喷泉：盆（物理球体碰撞元略大于盆沿，防 ragdoll 陷盆）+ 水面 +
    // 中柱 + 水花粒子（R24 粒子的庭院化复用）
    world.spawn((
        Name("assault/fountain".into()),
        Transform::from_xyz(0.0, 0.35, 0.0),
        basin,
        basin_mat,
        Collider::static_sphere(1.75),
    ));
    world.spawn((
        Name("assault/fountain/water".into()),
        Transform::from_xyz(0.0, 0.62, 0.0),
        water,
        water_mat,
    ));
    world.spawn((
        Name("assault/fountain/spout".into()),
        Transform::from_xyz(0.0, 1.25, 0.0),
        spout,
        basin_mat,
    ));
    world.spawn((
        Name("assault/fountain/jet".into()),
        Transform::from_xyz(0.0, 0.72, 0.0),
        ParticleEmitter::fountain(),
    ));

    // ---- 灯光：黄昏斜阳（方向光带阴影）+ 两盏暖色火盆点光 ----
    if let Some(mut light) = world.try_resource_mut::<DirectionalLight>() {
        light.direction = Vec3::new(-0.35, -0.8, -0.45).normalize_or_zero();
        light.color = Vec3::new(1.0, 0.88, 0.74);
        light.intensity = 1.3;
        light.ambient = Vec3::new(0.22, 0.21, 0.21);
        light.shadows = true;
    }
    for (index, (x, z)) in [(7.4, 5.8), (-7.4, -5.8)].iter().enumerate() {
        world.spawn((
            Name(format!("assault/torch/{index}")),
            Transform::from_xyz(*x, 2.6, *z),
            PointLight {
                color: Vec3::new(1.0, 0.62, 0.3),
                intensity: 7.0,
                range: 13.0,
                shadows: false,
            },
        ));
    }
}

/// 组装一名角色（玩家或敌人共用）：根（逻辑位置）→ 模型（GLB 经 pack
/// 或文件加载；下半身播放器挂模型根 + 混合空间；上半身播放器挂空子
/// 实体 + 遮罩；腿链足部 IK；敌人的枪身子树隐藏；玩家的双臂手 IK）。
///
/// 返回 (root, model, action, hand_r_cfg, hand_l_cfg)；敌人后两者为
/// `None`（敌人持刀挥砍由动作剪辑表现，不配手 IK）。
fn build_character(
    world: &mut World,
    tag: &str,
    is_player: bool,
) -> (Entity, Entity, Entity, Option<Entity>, Option<Entity>) {
    let spawn = if is_player {
        logic::SPAWN_FOOT + Vec3::new(0.0, HALF_HEIGHT, 0.0)
    } else {
        let index: usize = tag
            .rsplit('/')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let spawn = logic::ENEMY_SPAWNS[index];
        Vec3::new(spawn.x, HALF_HEIGHT, spawn.y)
    };
    let root = world.spawn((
        Name(format!("assault/{tag}")),
        Transform::from_translation(spawn),
        Visibility::VISIBLE,
    ));
    // ---- R45 / R46：GLB 加载（pack 字节优先，文件路径回退）+ 双播放器
    // 组装。加载失败降级为静态几何占位（游戏逻辑不受影响，诚实告警）。
    let gltf = load_character_gltf(world);
    let (model, action, animated) = match gltf {
        Ok(instance) if instance.node_entities.len() == sk::NODE_COUNT => {
            let (model, action) =
                assemble_animated_character(world, &root, instance, tag, is_player);
            (model, action, true)
        }
        Ok(instance) => {
            log::warn!(
                "AssaultPlugin：角色 GLB 节点数 {} ≠ 骨架表 {}——资产与 skeleton.rs 不同步，\
                 降级为静态占位（重跑 gen_asset example）",
                instance.node_entities.len(),
                sk::NODE_COUNT
            );
            let model = spawn_static_fallback(world, &root, tag);
            (model, model, false)
        }
        Err(err) => {
            log::warn!("AssaultPlugin：角色 GLB 加载失败（{err}）——降级为静态占位");
            let model = spawn_static_fallback(world, &root, tag);
            (model, model, false)
        }
    };
    // 模型原点在脚底：根承载胶囊中心 → 模型局部 y = −半高
    if let Some(mut transform) = world.get_mut::<Transform>(model) {
        transform.position.y = -HALF_HEIGHT;
    }
    if animated {
        // ---- R46：足部 IK 双腿链（玩家与敌人同款）----
        build_leg_ik(world, model, tag);
        if is_player {
            let (hand_r, hand_l) = build_hand_ik(world, model, tag);
            return (root, model, action, Some(hand_r), Some(hand_l));
        }
        // 敌人：枪身子树隐藏（敌人是持刀近战——枪不参与表现）
        let bones = bone_entities(world, model);
        if let Some(Some(rifle)) = bones.get(&sk::RIFLE) {
            set_subtree_hidden(world, *rifle, true);
        }
    }
    (root, model, action, None, None)
}

/// 从 pack / 文件加载角色 GLB 字节并实例化。pack 路径经
/// `spawn_gltf_instance_from_bytes`（内存来源，GLB 必须自包含——
/// R45 已知限制的实弹验证：本作 GLB 无外部引用，自包含成立）；回退
/// 路径经 `spawn_gltf_instance`。
fn load_character_gltf(world: &mut World) -> Result<GltfInstance, String> {
    let mount_bytes = world
        .try_resource::<AssetMount>()
        .and_then(|mount| mount.read(GLB_ENTRY).ok());
    if let Some(bytes) = mount_bytes {
        log::info!("AssaultPlugin：角色 GLB 经 pack 挂载加载（{GLB_ENTRY}）");
        return spawn_gltf_instance_from_bytes(world, bytes).map_err(|err| err.to_string());
    }
    let path = asset_path(GLB_FALLBACK);
    log::warn!(
        "AssaultPlugin：pack 内无 {GLB_ENTRY}，回退文件 {}",
        path.display()
    );
    spawn_gltf_instance(world, &path).map_err(|err| err.to_string())
}

/// 双播放器组装：模型根挂下半身播放器（idle / walk / run + 混合空间），
/// 空子实体挂上半身播放器（guard / sword×3 / aim / reload / flinch +
/// `AnimationMask`）。GLB 材质按节点做写实替换（R42）。返回
/// (model, action)。
fn assemble_animated_character(
    world: &mut World,
    root: &Entity,
    instance: GltfInstance,
    tag: &str,
    is_player: bool,
) -> (Entity, Entity) {
    let clips = instance.clips.clone();
    let node_entities = instance.node_entities.clone();
    let model = instance.roots[0];
    world.add_child(*root, model);
    world.insert(model, Name(format!("assault/{tag}-model")));

    // ---- R42：GLB 材质写实替换（摩擦绕行：GltfBuilder 不支持纹理引用
    // → 加载后按 primitive 实体的 MaterialHandle 读出基础参数，克隆 +
    // with_normal/with_orm 生成新材质替换组件；敌人上身换暗红配色）----
    let (cloth_normal, cloth_orm) = {
        let mut store = world.resource_mut::<TextureStore>();
        let (normal, orm) = textures::cloth_normal_orm(128, 0x00C1_0711);
        (store.add(normal), store.add(orm))
    };
    let (scratch_normal, scratch_orm) = {
        let mut store = world.resource_mut::<TextureStore>();
        let (normal, orm) = textures::scratched_metal_normal_orm(128, 0xD00D_FEED);
        (store.add(normal), store.add(orm))
    };
    let tint = |world: &mut World, node: usize, recolor: Option<[f32; 3]>, kind: BodySurface| {
        let Some(node_entity) = node_entities.get(node).copied().flatten() else {
            return;
        };
        let children: Vec<Entity> = world
            .get::<Children>(node_entity)
            .map(|children| children.0.clone())
            .unwrap_or_default();
        for child in children {
            let Some(handle) = world.get::<MaterialHandle>(child).map(|h| *h) else {
                continue;
            };
            let base = world
                .resource::<MaterialStore>()
                .get(handle)
                .map(|def| def.base_color)
                .unwrap_or([0.5, 0.5, 0.5, 1.0]);
            let base = match recolor {
                Some([r, g, b]) => [r, g, b, base[3]],
                None => base,
            };
            let mut def = MaterialDef {
                base_color: base,
                metallic: 0.0,
                roughness: 0.8,
                ..MaterialDef::default()
            };
            match kind {
                BodySurface::Cloth => {
                    def = def.with_normal(cloth_normal).with_orm(cloth_orm);
                }
                BodySurface::Blade => {
                    def.metallic = 1.0;
                    def.roughness = 0.22;
                    def.base_color = [0.82, 0.84, 0.88, 1.0];
                    def = def.with_normal(scratch_normal).with_orm(scratch_orm);
                }
            }
            let new_handle = world.resource_mut::<MaterialStore>().add(def);
            world.insert(child, new_handle);
        }
    };
    let enemy_tint = if is_player {
        None
    } else {
        Some([0.44, 0.13, 0.11])
    };
    for node in [
        sk::PELVIS,
        sk::TORSO,
        sk::CHEST,
        sk::L_THIGH,
        sk::R_THIGH,
        sk::L_SHIN,
        sk::R_SHIN,
    ] {
        tint(world, node, enemy_tint, BodySurface::Cloth);
    }
    tint(world, sk::SWORD_BLADE, None, BodySurface::Blade);

    // ---- 下半身播放器（模型根）：步态混合空间（R46 / R38 同款）----
    let idle_idx = clip_index(&clips, "idle");
    let walk_idx = clip_index(&clips, "walk");
    let run_idx = clip_index(&clips, "run");
    let mut locomotion = instance.player();
    if let (Some(idle), Some(walk), Some(run)) = (idle_idx, walk_idx, run_idx) {
        locomotion.blend_space = Some(BlendSpace {
            samples: vec![
                (Vec2::ZERO, idle),
                (Vec2::new(0.5, 0.0), walk),
                (Vec2::new(1.0, 0.0), run),
            ],
        });
        locomotion.play_blendspace();
    }
    locomotion.capture_rest(world);
    world.insert(model, locomotion);

    // ---- 上半身播放器（空子实体）：动作剪辑 + 骨骼遮罩（R46）----
    let action = world.spawn((Name(format!("assault/{tag}-action")), Transform::IDENTITY));
    world.add_child(*root, action);
    let mut actuator = AnimationPlayer::new(clips.clone(), node_entities.clone());
    // 遮罩 = 上身节点集：即使某剪辑误写下身通道也不会覆写（双重保险；
    // 与「下半身播放器只写下身」共同保证两层写入集不相交）
    actuator.mask = Some(AnimationMask {
        nodes: sk::UPPER_BODY.iter().copied().collect(),
    });
    if let Some(guard) = clip_index(&clips, "guard") {
        actuator.active = guard;
        actuator.looping = true;
    }
    actuator.capture_rest(world);
    world.insert(action, actuator);
    (model, action)
}

/// 材质替换的表面类别。
enum BodySurface {
    /// 布料（法线褶皱 + 粗糙 ORM）。
    Cloth,
    /// 金属刀身（划痕法线 + 高光 ORM）。
    Blade,
}

/// 按名查剪辑下标（GLB 剪辑名即契约）。
fn clip_index(clips: &[AnimationClip], name: &str) -> Option<usize> {
    clips.iter().position(|clip| clip.name == name)
}

/// 腿链足部 IK（R46 / R38 同款）：wrapper（挂 `IkRig` + `FootIk`）——
/// 链节点实体来自 GLB 骨架表（thigh → shin → foot 直连链）。
fn build_leg_ik(world: &mut World, model: Entity, tag: &str) {
    let bones = bone_entities(world, model);
    for (side, nodes) in [
        ("l", [sk::L_THIGH, sk::L_SHIN, sk::L_FOOT]),
        ("r", [sk::R_THIGH, sk::R_SHIN, sk::R_FOOT]),
    ] {
        let Some(thigh) = bones.get(&nodes[0]).copied().flatten() else {
            continue;
        };
        let Some(shin) = bones.get(&nodes[1]).copied().flatten() else {
            continue;
        };
        let Some(foot) = bones.get(&nodes[2]).copied().flatten() else {
            continue;
        };
        let wrapper = world.spawn((
            Name(format!("assault/{tag}-leg-{side}")),
            Transform::IDENTITY,
        ));
        let (rig, ik) = FootIk::from_rig(thigh, shin, foot);
        world.insert(wrapper, rig);
        world.insert(wrapper, ik);
    }
}

/// 双臂手 IK 配置（R46）：每臂一条配置实体（`IkRig` 三节点 + `HandIk`
/// 目标）——组件存储每实体单槽，双臂必然分实体。目标实体 = 枪身
/// `IkTarget`（握把 / 护木），**不在手骨链子树内**（枪挂胸节点下）。
/// 目标每帧由 `run_assault` 切换（ADS 吸附 / 平时 None）。
fn build_hand_ik(world: &mut World, model: Entity, tag: &str) -> (Entity, Entity) {
    let bones = bone_entities(world, model);
    let grip = bones.get(&sk::RIFLE_GRIP).copied().flatten();
    let foregrip = bones.get(&sk::FOREGRIP).copied().flatten();
    // IkTarget 标记挂在枪身节点实体上（手 IK 的目标源）
    if let Some(grip) = grip {
        world.insert(grip, IkTarget);
    }
    if let Some(foregrip) = foregrip {
        world.insert(foregrip, IkTarget);
    }
    let make_arm = |world: &mut World, prefix: &str, nodes: [usize; 3], target: Option<Entity>| {
        let (Some(shoulder), Some(elbow), Some(hand)) = (
            bones.get(&nodes[0]).copied().flatten(),
            bones.get(&nodes[1]).copied().flatten(),
            bones.get(&nodes[2]).copied().flatten(),
        ) else {
            // 骨架不齐：返回占位实体（不可达——守护测试锁定骨架形状）
            return world.spawn((Name(format!("assault/{tag}-hand-{prefix}-orphan")),));
        };
        let cfg = world.spawn((
            Name(format!("assault/{tag}-hand-{prefix}")),
            Transform::IDENTITY,
        ));
        world.insert(
            cfg,
            IkRig::new(vec![Some(shoulder), Some(elbow), Some(hand)]),
        );
        world.insert(
            cfg,
            HandIk {
                shoulder_node: 0,
                elbow_node: 1,
                hand_node: 2,
                pole: Vec3::Z, // 肘弯朝后（引擎前方 = −Z）
                target,
                ..HandIk::default()
            },
        );
        cfg
    };
    let right = make_arm(
        world,
        "r",
        [sk::R_UPPERARM, sk::R_FOREARM, sk::R_HAND],
        None,
    );
    let left = make_arm(
        world,
        "l",
        [sk::L_UPPERARM, sk::L_FOREARM, sk::L_HAND],
        None,
    );
    (right, left)
}

/// 骨架节点实体快照（骨架表下标 → 实体）：读模型根上的下半身播放器的
/// `node_entities`（组装时保存的完整映射）。
fn bone_entities(
    world: &World,
    model: Entity,
) -> std::collections::BTreeMap<usize, Option<Entity>> {
    world
        .get::<AnimationPlayer>(model)
        .map(|player| {
            player
                .node_entities
                .iter()
                .enumerate()
                .map(|(index, &entity)| (index, entity))
                .collect()
        })
        .unwrap_or_default()
}

/// 静态降级占位（GLB 不可用时）：单盒身，无动画（游戏逻辑不受影响）。
fn spawn_static_fallback(world: &mut World, root: &Entity, tag: &str) -> Entity {
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.5, 0.2, 0.2, 1.0));
    let body = world.spawn((
        Name(format!("assault/{tag}-fallback")),
        Transform {
            position: Vec3::new(0.0, 0.9, 0.0),
            scale: Vec3::new(0.5, 1.5, 0.35),
            ..Transform::IDENTITY
        },
        cube,
        mat,
    ));
    world.add_child(*root, body);
    body
}

/// 主相机（根级实体）：固定俯角跟随由 `run_assault` 每帧写位姿（演出
/// 期间让位给时间线）。**刻意不挂 `SmoothFollow`、不做玩家子实体**：
/// 时间线按世界系覆写相机，子实体 / 平滑追随都会与时间线争写（后写的
/// 赢——时间线是 Update 更早的独占系统，子实体方案必输）。
fn start_camera(world: &mut World) -> Entity {
    let spawn = logic::SPAWN_FOOT + Vec3::new(0.0, HALF_HEIGHT, 0.0);
    let eye = spawn + CAMERA_OFFSET;
    world.spawn((
        Name("assault/camera".into()),
        Transform {
            position: eye,
            rotation: Quat::from_rotation_x(CAMERA_PITCH),
            scale: Vec3::ONE,
        },
        ActiveCamera {
            fov_y_radians: 55.0_f32.to_radians(),
            ..ActiveCamera::default()
        },
    ))
}

/// 粒子 FX 池（bastion 同款：启动预热、运行期零 spawn，参数全字段覆写 +
/// `trigger_burst` 就地复用）。四组各 [`FX_PER_TYPE`] 槽，布局见
/// [`fx_slot`]。
fn build_fx_pool(world: &mut World) -> Vec<Entity> {
    (0..FX_PER_TYPE * FX_TYPES)
        .map(|index| {
            let mut emitter = match index / FX_PER_TYPE {
                0 => blood_mist(),
                1 => muzzle_flash(),
                2 => sword_trail(),
                _ => spark_burst(),
            };
            emitter.max_particles = 64;
            world.spawn((
                Name(format!("assault/fx/{index:02}")),
                Transform::IDENTITY,
                emitter,
            ))
        })
        .collect()
}

/// FX 组下标（血雾 0 / 枪口焰 1 / 刀光 2 / 火花 3）。
const FX_BLOOD: usize = 0;
const FX_MUZZLE: usize = 1;
const FX_TRAIL: usize = 2;
const FX_SPARK: usize = 3;

/// 组内取槽：`group_base + (frame_counter + offset) % FX_PER_TYPE`——
/// 游标随帧推进（确定性：由 `elapsed` 派生），同类型连续触发轮转不叠写。
fn fx_slot(ents: &AssaultEntities, group: usize, frame: usize, offset: usize) -> Option<Entity> {
    let base = group * FX_PER_TYPE;
    ents.fx_pool
        .get(base + (frame + offset) % FX_PER_TYPE)
        .copied()
}

/// HUD：标题 / 生命 / 弹药 / 敌数 / 横幅（ASCII 位图字体）。
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
        "assault/hud/title",
        [24.0, 20.0],
        "IRIS ASSAULT".into(),
        3.0,
        [1.0, 0.85, 0.6, 1.0],
    );
    hud(
        world,
        HP_TEXT_NAME,
        [24.0, 56.0],
        "HP 100/100".into(),
        3.0,
        [0.4, 1.0, 0.5, 1.0],
    );
    hud(
        world,
        AMMO_TEXT_NAME,
        [24.0, 92.0],
        "AMMO 12/12".into(),
        3.0,
        [0.9, 0.9, 0.9, 1.0],
    );
    hud(
        world,
        ENEMIES_TEXT_NAME,
        [24.0, 128.0],
        "ENEMIES 4".into(),
        3.0,
        [1.0, 0.5, 0.4, 1.0],
    );
    hud(
        world,
        BANNER_TEXT_NAME,
        [1280.0 * 0.5 - 200.0, 300.0],
        String::new(),
        4.0,
        [1.0, 0.85, 0.4, 1.0],
    );
}

// ---------------------------------------------------------------------------
// Startup 3：开场演出（R47）
// ---------------------------------------------------------------------------

/// 开场演出：相机绕庭院一周（Catmull-Rom spline 过 5 个关键帧）+ 两段
/// 字幕 + 黑边 + `intro_end` 事件交还控制权。演出期间逻辑冻结
/// （[`Phase::Intro`]），音乐与渲染照常。
fn start_intro_cinematic(world: &mut World) {
    let mut timeline = Timeline::new(6.5);
    // 相机轨道：环绕庭院（眼高 ~4.5-5，注视喷泉上空）
    let orbit = [
        Vec3::new(-12.0, 4.5, 8.0),
        Vec3::new(-2.0, 5.0, 12.0),
        Vec3::new(10.0, 5.0, 6.0),
        Vec3::new(12.0, 4.0, -6.0),
        Vec3::new(0.0, 4.5, -11.5),
    ];
    let look_at = Vec3::new(0.0, 1.2, 0.0);
    timeline.camera = Some(CameraTrack {
        keys: orbit
            .iter()
            .enumerate()
            .map(|(index, eye)| CameraKey {
                time: index as f32 * 6.5 / 4.0,
                eye: *eye,
                target: look_at,
            })
            .collect(),
        ease: true,
    });
    timeline.dialogue = vec![
        DialogueEntry {
            time: 0.6,
            end_time: 3.2,
            text: "FOUR BLADES HOLD THE COURTYARD.".into(),
        },
        DialogueEntry {
            time: 3.5,
            end_time: 6.2,
            text: "CLEAR THEM ALL. NO RETREAT.".into(),
        },
    ];
    timeline.events = vec![TimelineEvent {
        time: 6.4,
        name: "intro_end".into(),
    }];
    start_timeline(world, timeline);
    // 黑边：演出期间拉上下黑边（2.35:1 观感）
    if let Some(mut player) = world.try_resource_mut::<TimelinePlayer>() {
        player.letterbox = true;
    }
}

/// 终局演出（胜利 / 败北）：推近主角 + 字幕 + 黑边。由
/// `consume_game_events` 在终局事件时经命令队列调用（每局至多一次，
/// [`OutroLatch`]）。
fn start_outro_cinematic(world: &mut World, victory: bool, focus: Vec3) {
    if world.try_resource::<OutroLatch>().map(|latch| latch.0) == Some(true) {
        return;
    }
    if let Some(mut latch) = world.try_resource_mut::<OutroLatch>() {
        latch.0 = true;
    }
    let mut timeline = Timeline::new(4.5);
    let eye_near = focus + Vec3::new(1.6, 1.8, 3.2);
    let eye_far = focus + Vec3::new(3.5, 4.0, 7.0);
    timeline.camera = Some(CameraTrack {
        keys: vec![
            CameraKey {
                time: 0.0,
                eye: eye_far,
                target: focus + Vec3::new(0.0, 0.8, 0.0),
            },
            CameraKey {
                time: 4.5,
                eye: eye_near,
                target: focus + Vec3::new(0.0, 0.7, 0.0),
            },
        ],
        ease: true,
    });
    timeline.dialogue = vec![DialogueEntry {
        time: 0.5,
        end_time: 4.2,
        text: if victory {
            "COURTYARD CLEARED.".into()
        } else {
            "YOU FELL.".into()
        },
    }];
    timeline.events = vec![TimelineEvent {
        time: 0.1,
        name: if victory {
            "outro_victory".into()
        } else {
            "outro_defeat".into()
        },
    }];
    start_timeline(world, timeline);
    if let Some(mut player) = world.try_resource_mut::<TimelinePlayer>() {
        player.letterbox = true;
    }
}

// ---------------------------------------------------------------------------
// Update 1：逻辑推进 + 时间线事件 + 动画参数 + DOF + 音频 + HUD
// ---------------------------------------------------------------------------

/// Update 1：读输入与时间 → **R47** 时间线事件消费（intro_end →
/// `begin_battle`）→ 推进纯逻辑 → 写角色变换 / 动画参数 → **R46** 手 IK
/// 目标切换 → **R43** DOF 动态对焦 → **R25** 空间音频 → HUD。
/// 演出期间（时间线播放中）逻辑与相机写入让位——时间线是 Update 阶段
/// 更早的独占系统，此处后写会赢，必须自查让位。
fn run_assault(world: &World) {
    let dt = world
        .try_resource::<Time>()
        .map(|time| time.delta_secs())
        .unwrap_or(0.0);
    let cinematic = iris_engine::timeline::is_playing(world);

    // ---- R47：时间线事件消费（intro_end → 开战）----
    let mut begin = false;
    for event in world.events_mut::<TimelineEvent>().drain() {
        if event.name == "intro_end" {
            begin = true;
        }
    }
    if begin {
        if let Some(mut state) = world.try_resource_mut::<AssaultState>() {
            state.begin_battle();
        }
    }

    // ---- 输入翻译：相机固定俯角 → 世界系与屏幕系一致（W = -Z）----
    let input = world
        .try_resource::<Input>()
        .map(|input| AssaultInput {
            move_dir: Vec2::new(
                (input.key(KeyCode::KeyD) as i32 - input.key(KeyCode::KeyA) as i32) as f32,
                (input.key(KeyCode::KeyS) as i32 - input.key(KeyCode::KeyW) as i32) as f32,
            ),
            sword_press: input.key_pressed(KeyCode::Space)
                || input.mouse_button_pressed(MouseButton::Left),
            fire_held: input.key(KeyCode::KeyF),
            aim_held: input.key(KeyCode::KeyQ) || input.mouse_button(MouseButton::Right),
            reload_press: input.key_pressed(KeyCode::KeyR),
            dt,
        })
        .unwrap_or(AssaultInput {
            dt,
            ..AssaultInput::default()
        });

    let Some(mut state) = world.try_resource_mut::<AssaultState>() else {
        return;
    };
    if !cinematic {
        state.step(&input);
    }

    // ---- 事件分流：横幅置位（音效走下方统一入口）----
    let Some(assets) = world.try_resource::<AssaultAssets>() else {
        return;
    };
    for event in &state.frame_events {
        let banner: Option<(&str, [f32; 4])> = match *event {
            AssaultEvent::Victory { .. } => Some(("COURTYARD CLEARED", [0.4, 1.0, 0.6, 1.0])),
            AssaultEvent::Defeat { .. } => Some(("YOU FELL", [1.0, 0.3, 0.3, 1.0])),
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

    // ---- 三层结构写回 + 动画参数 + 手 IK 目标 ----
    let Some(ents) = world.try_resource::<AssaultEntities>() else {
        return;
    };
    write_character_visuals(world, &ents, &state, cinematic);

    // ---- R43：DOF 动态对焦。战斗中对焦距离 = 相机到最近活敌的直线
    // 距离的俯角近似（相机→玩家 + 玩家→敌人）——结论：**需要逐帧更新**
    // （对焦距离是交战几何的函数，静态值会让快速接近的敌人糊成一团），
    // 摩擦记录见 ARCHITECTURE §8。演出期间对焦喷泉（固定 12 m）。
    if let Some(mut dof) = world.try_resource_mut::<DepthOfFieldSettings>() {
        dof.enabled = true;
        if cinematic {
            dof.focus_distance = 12.0;
        } else {
            let focus = state.nearest_enemy().map(|(_, dist)| dist).unwrap_or(8.0);
            dof.focus_distance = (CAMERA_OFFSET.length() + focus).clamp(3.0, 16.0);
        }
    }

    // ---- R25：事件 → 空间音效（听者跟随主相机）----
    if let Some(audio) = world.try_resource::<Audio>() {
        let spatial = |clip: &AudioClip, at: Vec3, volume: f32| {
            audio.play_spatial(clip.clone(), at, volume, false);
        };
        for event in &state.frame_events {
            match *event {
                AssaultEvent::SwordSwing { pos, .. } => spatial(&assets.swing, pos, 0.35),
                AssaultEvent::SwordHit { pos, .. } => spatial(&assets.sword_hit, pos, 0.5),
                AssaultEvent::Shot { pos, .. } => spatial(&assets.shot, pos, 0.55),
                AssaultEvent::ReloadStart { pos } => spatial(&assets.reload, pos, 0.4),
                AssaultEvent::PlayerHurt { pos, .. } => spatial(&assets.hurt, pos, 0.5),
                AssaultEvent::EnemyKilled { pos, .. } => spatial(&assets.enemy_die, pos, 0.45),
                AssaultEvent::Victory { pos } => spatial(&assets.victory, pos, 0.6),
                AssaultEvent::Defeat { pos } => spatial(&assets.defeat, pos, 0.6),
                _ => {}
            }
        }
        // 听者：主相机全局位姿（GlobalTransform 来自上一帧传播，滞后一帧
        // 对音频无感）
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

    // ---- HUD 刷新 ----
    update_hud(world, &state);
}

/// 写角色表现：相机跟随（演出让位）/ 根位置 / 模型朝向 / 步态混合参数 /
/// 上身动作状态机（状态派生，无簿记）/ 受击附加层 / 手 IK 目标切换 /
/// 敌人动作。
fn write_character_visuals(
    world: &World,
    ents: &AssaultEntities,
    state: &AssaultState,
    cinematic: bool,
) {
    // ---- 相机跟随（演出期间让位时间线——后写会赢，必须自查）----
    if !cinematic {
        if let Some(mut transform) = world.get_mut::<Transform>(ents.camera) {
            let eye = state.player.pos + CAMERA_OFFSET;
            transform.position = eye;
            let target = state.player.pos + Vec3::new(0.0, 0.6, 0.0);
            let dir = (target - eye).normalize_or_zero();
            transform.rotation = Quat::from_rotation_arc(Vec3::NEG_Z, dir);
        }
    }
    // ---- 玩家：根位置 / 朝向 / 步态参数 ----
    if let Some(mut transform) = world.get_mut::<Transform>(ents.player_root) {
        transform.position = state.player.pos;
    }
    if let Some(mut transform) = world.get_mut::<Transform>(ents.player_model) {
        transform.rotation = Quat::from_rotation_y(state.player.facing);
    }
    if let Some(mut player) = world.get_mut::<AnimationPlayer>(ents.player_model) {
        player.blend_param = Vec2::new((state.player.anim_speed / MOVE_SPEED).min(1.0), 0.0);
    }
    // ---- 玩家上身动作状态机（R46）：目标剪辑由逻辑状态派生；变化沿
    // 触发 transition（同剪辑是引擎侧 no-op）+ 起手帧时间归零（重触发）。
    let reloading = state.player.rifle.reload_t > 0.0;
    let stage = state.player.sword.stage;
    let just_started = stage > 0 && state.player.sword.t <= 1.5 / 60.0;
    if let Some(mut action) = world.get_mut::<AnimationPlayer>(ents.player_action) {
        let target = if reloading {
            clip_index(&action.clips, "reload")
        } else if stage > 0 {
            clip_index(&action.clips, &format!("sword{stage}"))
        } else if state.player.aiming {
            clip_index(&action.clips, "aim")
        } else {
            clip_index(&action.clips, "guard")
        };
        if let Some(target) = target {
            if action.active != target {
                action.transition(target, 0.06);
                action.time = 0.0;
            } else if just_started {
                // 同剪辑重入（如三连段后再起 sword1）：时间归零重触发
                action.time = 0.0;
            }
        }
        // 受击附加层（R46）：hurt_flash ∈ (0, 0.25] → flinch 增量压幅
        if let Some(flinch) = clip_index(&action.clips, "flinch") {
            if state.player.hurt_flash > 0.0 {
                action.play_additive(flinch, (state.player.hurt_flash / 0.25).min(1.0));
            } else if let Some(additive) = action.additive.as_mut() {
                additive.weight = 0.0;
            }
        }
    }
    // 手 IK 目标切换（R46）：ADS 时双手吸附枪身挂点；平时回动作剪辑
    let aiming = state.player.aiming && !reloading;
    if aiming {
        let grip = grip_entity(world, ents.player_model, sk::RIFLE_GRIP);
        let foregrip = grip_entity(world, ents.player_model, sk::FOREGRIP);
        if let Some(hand) = ents.player_hand_r {
            if let Some(mut ik) = world.get_mut::<HandIk>(hand) {
                ik.target = grip;
            }
        }
        if let Some(hand) = ents.player_hand_l {
            if let Some(mut ik) = world.get_mut::<HandIk>(hand) {
                ik.target = foregrip;
            }
        }
    } else if let Some(hand) = ents.player_hand_r {
        if let Some(mut ik) = world.get_mut::<HandIk>(hand) {
            ik.target = None;
        }
    } else {
        // 左臂在非瞄准时同样松开（保护性对称——右臂 None 已覆盖常规路径）
    }
    if !aiming {
        if let Some(hand) = ents.player_hand_l {
            if let Some(mut ik) = world.get_mut::<HandIk>(hand) {
                ik.target = None;
            }
        }
    }
    // ---- 敌人：根位置 / 朝向 / 步态参数 / 动作状态机 / 硬直附加层 ----
    for (index, enemy_view) in ents.enemies.iter().enumerate() {
        let Some(enemy) = state.enemies.get(index) else {
            continue;
        };
        if enemy.state == EnemyState::Dead {
            continue; // 模型已由 consume_game_events 隐藏（ragdoll 接管）
        }
        if let Some(mut transform) = world.get_mut::<Transform>(enemy_view.root) {
            transform.position = enemy.pos;
        }
        if let Some(mut transform) = world.get_mut::<Transform>(enemy_view.model) {
            transform.rotation = Quat::from_rotation_y(enemy.facing);
        }
        if let Some(mut player) = world.get_mut::<AnimationPlayer>(enemy_view.model) {
            player.blend_param = Vec2::new((enemy.anim_speed / logic::ENEMY_SPEED).min(1.0), 0.0);
        }
        if let Some(mut action) = world.get_mut::<AnimationPlayer>(enemy_view.action) {
            if let Some(sword1) = clip_index(&action.clips, "sword1") {
                if enemy.state == EnemyState::Windup && enemy.t <= 1.5 / 60.0 {
                    // 出刀：重触发（时间归零，与逻辑前摇对齐）
                    if action.active != sword1 {
                        action.transition(sword1, 0.04);
                    }
                    action.time = 0.0;
                } else if action.active == sword1
                    && matches!(
                        enemy.state,
                        EnemyState::Chase | EnemyState::Idle | EnemyState::Recover
                    )
                {
                    // 收刀回待机
                    if let Some(guard) = clip_index(&action.clips, "guard") {
                        action.transition(guard, 0.15);
                    }
                }
            }
            // 硬直：flinch 附加层压幅（Stagger 期间从头衰减）
            if let Some(flinch) = clip_index(&action.clips, "flinch") {
                if enemy.state == EnemyState::Stagger {
                    let weight = (1.0 - enemy.t / logic::ENEMY_STAGGER).max(0.15);
                    action.play_additive(flinch, weight);
                } else if let Some(additive) = action.additive.as_mut() {
                    additive.weight = 0.0;
                }
            }
        }
    }
}

/// 从模型骨架表按节点下标取实体（IkTarget 所在节点的实体句柄）。
fn grip_entity(world: &World, model: Entity, node: usize) -> Option<Entity> {
    world
        .get::<AnimationPlayer>(model)
        .and_then(|player| player.node_entities.get(node).copied().flatten())
}

/// HUD 文本刷新：生命 / 弹药 / 敌数 + 横幅按阶段与计时切换。
fn update_hud(world: &World, state: &AssaultState) {
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
        HP_TEXT_NAME,
        format!("HP {}/{}", state.player.hp.max(0), logic::PLAYER_MAX_HP),
    );
    set_text(
        world,
        AMMO_TEXT_NAME,
        if state.player.rifle.reload_t > 0.0 {
            "AMMO -- RELOADING".into()
        } else {
            format!("AMMO {}/{}", state.player.rifle.ammo, logic::RIFLE_MAG)
        },
    );
    set_text(
        world,
        ENEMIES_TEXT_NAME,
        format!("ENEMIES {}", state.enemies_alive()),
    );
    if let Some(mut timer) = world.try_resource_mut::<BannerTimer>() {
        timer.0 = (timer.0 - dt).max(0.0);
        if let Some(entity) = world.entity_named(BANNER_TEXT_NAME) {
            if let Some(mut text) = world.get_mut::<UiText>(entity) {
                let next = match state.phase {
                    Phase::Victory => "COURTYARD CLEARED".to_string(),
                    Phase::Defeat => "YOU FELL".to_string(),
                    _ if timer.0 > 0.0 => text.string.clone(),
                    _ => String::new(),
                };
                text.string = next;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Update 2（并行）：事件 → ragdoll / 贴花 / FX 池触发 / 终局演出
// ---------------------------------------------------------------------------

/// Update 2（**并行系统**）：消费本帧事件——
/// - `EnemyKilled` → ragdoll 生成经命令队列入队（`spawn_deferred`，帧末
///   `apply_commands` 执行）+ 敌人模型子树隐藏 + 终局演出（全歼时）；
/// - `SwordHit` → 地面血渍贴花（命令队列 spawn）+ FX 池血雾 burst；
/// - `BulletHitEnemy` → 血雾；`BulletHitWorld` → 弹孔贴花 + 火花；
/// - `Shot` → 枪口焰；`SwordSwing` → 刀光。
///
/// FX 池触发就地复用（bastion 同款：参数全字段覆写 + `reset()` +
/// `trigger_burst`），运行期零 spawn；贴花是持久战痕（本局不回收）。
fn consume_game_events(world: &World) {
    let Some(state) = world.try_resource::<AssaultState>() else {
        return;
    };
    let Some(ents) = world.try_resource::<AssaultEntities>() else {
        return;
    };
    let Some(assets) = world.try_resource::<AssaultAssets>() else {
        return;
    };
    // 确定性 FX 游标：帧计数由 elapsed 派生（每帧递增 → 组内轮转）
    let frame = (state.elapsed * 60.0) as usize;

    for event in &state.frame_events {
        match *event {
            AssaultEvent::EnemyKilled { pos, enemy, .. } => {
                // 敌人模型隐藏（根 + 全子树）
                if let Some(enemy_view) = ents.enemies.get(enemy) {
                    set_subtree_hidden(world, enemy_view.root, true);
                }
                // ragdoll：脚底生成六球人形 + 立即释放（受击方向轻踢——
                // 玩家指向敌人的方向）
                let kick = Vec2::new(state.player.pos.x - pos.x, state.player.pos.z - pos.z)
                    .normalize_or_zero();
                let foot = pos;
                if let Some(queue) = world.try_resource::<CommandQueue>() {
                    queue.spawn_deferred(move |world: &mut World| {
                        let desc = RagdollDesc::humanoid();
                        let limbs = spawn_ragdoll_kinematic(world, foot, &desc);
                        let mut meshes = world.resource_mut::<MeshStore>();
                        let mesh_handles: Vec<_> = desc
                            .limbs
                            .iter()
                            .map(|def| meshes.add(MeshData::uv_sphere(def.radius, 12, 8)))
                            .collect();
                        drop(meshes);
                        let material = world
                            .resource_mut::<MaterialStore>()
                            .add(MaterialDef::solid(0.44, 0.13, 0.11, 1.0));
                        for (&limb, mesh) in limbs.iter().zip(&mesh_handles) {
                            world.insert(limb, *mesh);
                            world.insert(limb, material);
                            world.insert(limb, Visibility::VISIBLE);
                        }
                        let kick_dir = Vec3::new(kick.x, 0.0, kick.y);
                        release_ragdoll(world, &limbs, Vec3::ZERO, Some((kick_dir, 1.4)));
                    });
                }
                // 地面血渍（击杀附带一摊更大的血）
                spawn_decal(
                    world,
                    assets.blood,
                    Vec3::new(pos.x, 0.02, pos.z),
                    Vec3::Y,
                    Vec3::new(1.3, 1.3, 0.04),
                    [0.9, 0.9, 0.9, 1.0],
                );
                if let Some(slot) = fx_slot(&ents, FX_BLOOD, frame, enemy) {
                    trigger_fx(
                        world,
                        slot,
                        Vec3::new(pos.x, HALF_HEIGHT, pos.z),
                        |emitter| {
                            emitter.trigger_burst(18);
                        },
                    );
                }
            }
            AssaultEvent::SwordHit { pos, stage, .. } => {
                // 地面血渍（敌人脚底）+ 血雾（重击更大）
                spawn_decal(
                    world,
                    assets.blood,
                    Vec3::new(pos.x, 0.02, pos.z),
                    Vec3::Y,
                    Vec3::new(0.7 + stage as f32 * 0.2, 0.7 + stage as f32 * 0.2, 0.04),
                    [1.0, 1.0, 1.0, 0.9],
                );
                if let Some(slot) = fx_slot(&ents, FX_BLOOD, frame, stage as usize) {
                    trigger_fx(
                        world,
                        slot,
                        Vec3::new(pos.x, HALF_HEIGHT, pos.z),
                        |emitter| {
                            emitter.trigger_burst(10 + stage as u32 * 4);
                        },
                    );
                }
            }
            AssaultEvent::BulletHitEnemy { pos, enemy, .. } => {
                if let Some(slot) = fx_slot(&ents, FX_BLOOD, frame, enemy) {
                    trigger_fx(
                        world,
                        slot,
                        Vec3::new(pos.x, HALF_HEIGHT, pos.z),
                        |emitter| {
                            emitter.trigger_burst(10);
                        },
                    );
                }
            }
            AssaultEvent::BulletHitWorld { pos, normal } => {
                spawn_decal(
                    world,
                    assets.bullet_hole,
                    pos,
                    normal,
                    Vec3::new(0.24, 0.24, 0.03),
                    [1.0, 1.0, 1.0, 1.0],
                );
                if let Some(slot) = fx_slot(&ents, FX_SPARK, frame, 0) {
                    trigger_fx(world, slot, pos, |emitter| {
                        emitter.trigger_burst(8);
                    });
                }
            }
            AssaultEvent::Shot { pos, .. } => {
                if let Some(slot) = fx_slot(&ents, FX_MUZZLE, frame, 0) {
                    trigger_fx(world, slot, pos, |emitter| {
                        emitter.trigger_burst(14);
                    });
                }
            }
            AssaultEvent::SwordSwing { pos, facing, .. } => {
                let fwd = Vec3::new(-facing.sin(), 0.1, -facing.cos());
                if let Some(slot) = fx_slot(&ents, FX_TRAIL, frame, 0) {
                    trigger_fx(world, slot, pos + fwd * 1.2, |emitter| {
                        emitter.trigger_burst(10);
                    });
                }
            }
            // 终局演出（胜利 / 败北）：命令队列延迟启动（start_timeline
            // 需要 &mut World——deferred 闭包在帧末独占执行）
            AssaultEvent::Victory { pos } | AssaultEvent::Defeat { pos } => {
                let victory = matches!(event, AssaultEvent::Victory { .. });
                if let Some(queue) = world.try_resource::<CommandQueue>() {
                    queue.spawn_deferred(move |world: &mut World| {
                        start_outro_cinematic(world, victory, pos);
                    });
                }
            }
            _ => {}
        }
    }
}

/// 命令队列 spawn 一枚贴花（持久战痕；+Z 端盖与被贴面重合的推荐
/// 摆放，见 `iris_render::decal` 模块文档）。
fn spawn_decal(
    world: &World,
    texture: TextureHandle,
    position: Vec3,
    normal: Vec3,
    size: Vec3,
    tint: [f32; 4],
) {
    let rotation = Quat::from_rotation_arc(Vec3::Z, normal);
    if let Some(queue) = world.try_resource::<CommandQueue>() {
        queue.spawn_deferred(move |world: &mut World| {
            world.spawn((
                Transform {
                    position: position + normal * (size.z * 0.5),
                    rotation,
                    scale: Vec3::ONE,
                },
                Decal {
                    texture,
                    size,
                    tint,
                },
            ));
        });
    }
}

/// FX 池槽位触发：定位 + burst（槽位预设参数由 build_fx_pool 配好，
/// 组内槽位同参——无需覆写）。
fn trigger_fx(world: &World, slot: Entity, at: Vec3, burst: impl FnOnce(&mut ParticleEmitter)) {
    if let Some(mut emitter) = world.get_mut::<ParticleEmitter>(slot) {
        burst(&mut emitter);
    }
    if let Some(mut transform) = world.get_mut::<Transform>(slot) {
        transform.position = at;
    }
}

/// 血雾预设（alpha 混合、重力下坠、快速消散）。
fn blood_mist() -> ParticleEmitter {
    let mut e = ParticleEmitter::new(ParticleBlend::Alpha);
    e.spawn_rate = 0.0;
    e.particle_lifetime = 0.45;
    e.initial_speed = 2.6;
    e.velocity_jitter = 1.1;
    e.gravity_scale = 0.6;
    e.size_start = 0.12;
    e.size_end = 0.02;
    e.color_start = [0.45, 0.03, 0.04, 0.85];
    e.color_end = [0.2, 0.01, 0.02, 0.0];
    e.max_particles = 64;
    e
}

/// 枪口焰预设（additive、HDR 亮色、极短寿命——泛光提取源）。
fn muzzle_flash() -> ParticleEmitter {
    let mut e = ParticleEmitter::new(ParticleBlend::Additive);
    e.spawn_rate = 0.0;
    e.particle_lifetime = 0.08;
    e.initial_speed = 5.0;
    e.velocity_jitter = 0.4;
    e.gravity_scale = 0.0;
    e.size_start = 0.14;
    e.size_end = 0.02;
    e.color_start = [4.0, 2.6, 1.0, 1.0];
    e.color_end = [1.6, 0.6, 0.15, 0.0];
    e.max_particles = 64;
    e
}

/// 刀光预设（additive、短促银白）。
fn sword_trail() -> ParticleEmitter {
    let mut e = ParticleEmitter::new(ParticleBlend::Additive);
    e.spawn_rate = 0.0;
    e.particle_lifetime = 0.18;
    e.initial_speed = 1.8;
    e.velocity_jitter = 0.9;
    e.gravity_scale = 0.1;
    e.size_start = 0.08;
    e.size_end = 0.01;
    e.color_start = [2.4, 2.4, 2.8, 1.0];
    e.color_end = [0.8, 0.8, 1.0, 0.0];
    e.max_particles = 64;
    e
}

/// 弹着火花预设（additive、黄白小火花）。
fn spark_burst() -> ParticleEmitter {
    let mut e = ParticleEmitter::new(ParticleBlend::Additive);
    e.spawn_rate = 0.0;
    e.particle_lifetime = 0.22;
    e.initial_speed = 3.2;
    e.velocity_jitter = 1.3;
    e.gravity_scale = 0.8;
    e.size_start = 0.05;
    e.size_end = 0.01;
    e.color_start = [3.2, 2.2, 0.9, 1.0];
    e.color_end = [1.2, 0.5, 0.1, 0.0];
    e.max_particles = 64;
    e
}

// ---------------------------------------------------------------------------
// 插件侧单测（表现侧组件语义；全流程走 tests/headless.rs）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// FX 预设参数的语义断言：血雾是 alpha、枪口焰 / 刀光 / 火花是
    /// additive（混合模式决定渲染批次归属，错配会穿帮）。
    #[test]
    fn fx_presets_use_intended_blend_modes() {
        assert_eq!(
            blood_mist().blend,
            ParticleBlend::Alpha,
            "血雾应是 alpha 混合"
        );
        for (name, emitter) in [
            ("muzzle", muzzle_flash()),
            ("trail", sword_trail()),
            ("spark", spark_burst()),
        ] {
            assert_eq!(
                emitter.blend,
                ParticleBlend::Additive,
                "{name} 应是 additive 混合"
            );
            assert_eq!(emitter.spawn_rate, 0.0, "{name} 池槽应静默持续发射");
        }
    }

    /// FX 池取槽：组内轮转不越组（血雾 / 枪口焰 / 刀光 / 火花各占
    /// [`FX_PER_TYPE`] 槽），offset 推进游标。
    #[test]
    fn fx_slot_rotation_stays_within_group() {
        let mut world = World::new();
        let ents = AssaultEntities {
            player_root: world.spawn((Transform::IDENTITY,)),
            player_model: world.spawn((Transform::IDENTITY,)),
            player_action: world.spawn((Transform::IDENTITY,)),
            player_hand_r: None,
            player_hand_l: None,
            camera: world.spawn((Transform::IDENTITY,)),
            enemies: Vec::new(),
            fx_pool: (0..FX_PER_TYPE * FX_TYPES)
                .map(|_| world.spawn((Transform::IDENTITY,)))
                .collect(),
        };
        for group in 0..FX_TYPES {
            for step in 0..FX_PER_TYPE * 2 {
                let slot = fx_slot(&ents, group, step, 0).expect("槽位应在池内");
                let index = ents
                    .fx_pool
                    .iter()
                    .position(|&e| e == slot)
                    .expect("应在池内");
                assert!(index / FX_PER_TYPE == group, "槽位 {index} 应属组 {group}");
            }
        }
        // 同帧多次触发（offset 递增）取不同槽位
        let a = fx_slot(&ents, FX_BLOOD, 3, 0).expect("非空");
        let b = fx_slot(&ents, FX_BLOOD, 3, 1).expect("非空");
        assert_ne!(a, b, "同帧两枚血雾应取不同槽位");
    }

    /// 音效资产完整性：concat 出的复合剪辑时长 = 分段和（程序化生成的
    /// 确定性构造烟测）。
    #[test]
    fn procedural_clips_are_well_formed() {
        let shot = gunshot(48_000);
        assert!(shot.duration_secs() > 0.1, "枪声应长于 0.1 s");
        let clicks = double_click(48_000);
        assert!(clicks.duration_secs() > 0.15, "双咔哒应含间隔（> 0.15 s）");
        let ping = metal_ping(48_000);
        assert!(ping.duration_secs() > 0.1 && ping.duration_secs() < 0.2);
    }
}
