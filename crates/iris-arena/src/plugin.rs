//! [`ArenaPlugin`]：把纯逻辑核心（`crate::logic`）接到 Iris 引擎上。
//!
//! 职责划分（iris-game 的 `GamePlugin` 同款分层，强度更高）：
//! - **Startup**：`setup_resources`（兜底插入引擎资源 + 游戏资源）→
//!   `build_scene`（[`load_scene`] 载入 `ArenaPlugin::scene_path` 指向的
//!   JSON 关卡：地面 / 围墙 / 障碍柱 / 灯光全部来自场景；随后**静态体
//!   收集**——场景 `Collider` 一次性转换为逻辑侧 [`StaticShape`] 列表存入
//!   [`ArenaStatics`] 资源，Update 每帧零查询）+ 玩家三层结构 + HUD +
//!   **敌人池**（15 个蒙皮蠕虫实例，`spawn_gltf_instance` + `AnimationPlayer`
//!   自动循环，启动即全部 `Visibility::HIDDEN` 待命）+ **弹道池**
//!   （40 个自发光小球实体，同样隐藏待命）；
//! - **Update**：`run_arena`（读 `Input` + `Time` + [`ArenaStatics`] →
//!   `ArenaState::step` → 差分事件计数触发音效）→ `update_arena_visuals`
//!   （写玩家三层 Transform + 敌人池槽位的可见性 / 变换 / 受击缩放脉冲 +
//!   弹道池槽位变换；池槽位以逻辑侧**稳定 id** 映射，取用 / 归还全程零
//!   spawn / despawn）→ `update_arena_hud`（标题 / HP 条 / WAVE / SCORE /
//!   COMBO / 横幅，文本实体经 `Name` 按名定位）。
//! - **层级传播**：交给 `HierarchyPlugin`（PostUpdate）；**动画推进**交给
//!   `AnimationPlugin`（Update 内 `update_animations`）——窗口路径由
//!   `DefaultPlugins` 注册，headless 测试 / 离屏快照需自行
//!   `AnimationPlugin.build(&mut app)`（与 `HierarchyPlugin` 同款约定），
//!   本插件不重复注册（重复推进会让动画双倍速）。
//!
//! # 敌人根实体 × 蒙皮子树
//!
//! 敌人的移动由插件写在 glTF 实例**根实体**的 `Transform` 上；蒙皮关节
//! （hip → j1 → j2）是根的后代实体（`spawn_gltf_instance` 加载时自动把
//! 兄弟顶层关节树收编为根实体的子树），`AnimationPlayer` 每帧覆写关节的
//! 局部旋转——「根平移 × 子树蒙皮动画」在同一实体树上叠加，层级传播
//! 当帧合成。
//! 蠕虫模型头部朝本地 +X，而引擎朝向约定 `facing = 0` 前方是 -Z，故敌人
//! 朝向写根旋转时补偿 +90°（[`MODEL_YAW_OFFSET`]）。
//!
//! # 运动学控制器与静态体
//!
//! 玩家与敌人的碰撞解算在纯逻辑核心内完成（本地 XZ 推离，语义近似引擎
//! `resolve_against_statics` 的水平投影，见 `logic` 模块文档）：本插件在
//! Startup 把场景静态 `Collider` 转换为 [`ArenaStatics`]，Update 不再触碰
//! 物理组件——场景是碰撞的**数据来源**，逻辑是唯一的消费者。
//!
//! 本插件只使用引擎公开 API（dogfooding II）：不依赖引擎内部路径。

use std::path::{Path, PathBuf};

use iris_core::physics::{Collider, ColliderShape, RigidBody};
use iris_core::prelude::{Access, Entity, Name, Schedule, Stage};
use iris_engine::prelude::*;

use crate::logic::{
    ArenaInput, ArenaState, EnemyState, Phase, ProjectileState, StaticShape, ENEMY_HIT_FLASH,
    MAX_ENEMIES, MAX_PROJECTILES, PLAYER_MAX_HP,
};

/// 默认关卡文件（仓库相对路径，经 [`asset_path`] 解析——CWD 未命中时
/// 回退 workspace 根，`cargo run` / `cargo test` 皆可用）。
pub const DEFAULT_SCENE_PATH: &str = "assets/arena_battle.scene.json";
/// 蒙皮蠕虫敌人模型（`gen_asset` 的 `write_enemy_worm_glb` 生成）。
pub const ENEMY_MODEL_PATH: &str = "assets/enemy_worm.glb";

/// 竞技场半边长（米）：传给逻辑的边界钳制**安全网**（±24）。正式关卡的
/// 墙体 collider 内缘在 ±24，玩家球体（半径 0.45）最远到 ±23.55。
pub const ARENA_HALF: f32 = 24.0;
/// 敌人对象池容量（与 [`MAX_ENEMIES`] 同规格：第 7 波起逻辑封顶于此）。
pub const POOL_ENEMIES: usize = MAX_ENEMIES;
/// 弹道对象池容量（与 [`MAX_PROJECTILES`] 同规格）。
pub const POOL_PROJECTILES: usize = MAX_PROJECTILES;

/// 敌人模型偏航补偿（弧度）：模型头部朝本地 +X，引擎朝向约定前方是 -Z，
/// +90° 让「写入的朝向」与「模型头向」对齐（见模块文档）。
const MODEL_YAW_OFFSET: f32 = std::f32::consts::FRAC_PI_2;
/// 弹道飞行高度（米）与敌人根高度（蠕虫贴地：模型 y 从 0 起）。
const PROJECTILE_Y: f32 = 0.5;
/// 玩家模型子实体的局部几何（模型本体 + 炮管，见 `build_scene`）。
const PLAYER_MODEL_LOCAL_Y: f32 = 0.55;
const PLAYER_INVULN_BLINK_HZ: f32 = 12.0;
/// HUD 布局（按 DefaultPlugins 缺省 1280×720 窗口手工排布）。
const HUD_WIDTH: f32 = 1280.0;
/// HP 条几何（背景条 + 前景条，前景宽度 = hp / max × 条宽）。
const HP_BAR_WIDTH: f32 = 200.0;
/// HUD 实体名（`Name` 组件，按名定位用）。
pub const TITLE_TEXT_NAME: &str = "arena/hud/title";
pub const WAVE_TEXT_NAME: &str = "arena/hud/wave";
pub const SCORE_TEXT_NAME: &str = "arena/hud/score";
pub const COMBO_TEXT_NAME: &str = "arena/hud/combo";
pub const HP_BACK_NAME: &str = "arena/hud/hp-back";
pub const HP_FRONT_NAME: &str = "arena/hud/hp-front";
pub const BANNER_TEXT_NAME: &str = "arena/hud/banner";

/// 适配：把纯逻辑类型接入引擎资源存储。
///
/// `logic.rs` 刻意保持零引擎依赖（只依赖 glam），而 [`ArenaState`] 需要
/// 作为 `World` 资源被系统读写——跨模块手动实现 `Resource` 即可让纯类型
/// 「选择性接入」引擎，无须在逻辑核心里引入任何引擎路径。
impl iris_core::resource::Resource for ArenaState {}

/// 世界资源：预生成的程序化音效（全部确定性、Startup 一次生成）。
#[derive(Resource)]
pub struct ArenaAssets {
    /// 射击音：1200 Hz 短 blip。
    pub shoot: AudioClip,
    /// 命中音：300 Hz 短音。
    pub hit: AudioClip,
    /// 受伤音：150 Hz 低频。
    pub hurt: AudioClip,
    /// 开波音：660 / 990 Hz 双音（[`double_beep`] 程序化拼接）。
    pub wave: AudioClip,
}

/// 场景静态体快照（Startup 一次性收集）：`(位置, 本地形状)` 列表，直接
/// 喂给 `ArenaState::step`——Update 每帧零查询、零转换。
#[derive(Clone, Debug, Default, Resource)]
pub struct ArenaStatics(pub Vec<(Vec3, StaticShape)>);

/// 场景实体索引：玩家三层结构等**每帧热路径**的句柄资源。
///
/// HUD 文本实体经引擎 `Name` + `World::entity_named` 按名定位（低频路径）；
/// 敌人 / 弹道由各自的池资源管理，不再进入本索引。
#[derive(Clone, Copy, Resource)]
pub struct ArenaEntities {
    /// 玩家根实体：只承载逻辑位置（rotation 恒为恒等）——相机子实体挂在
    /// 它下面，若把朝向写进根实体会让相机绕玩家公转、画面剧转。
    pub player: Entity,
    /// 玩家模型子实体：网格挂这里，转身朝向（偏航）写它的 rotation；
    /// 受伤 / 冲刺无敌期间的闪烁也写它的 `Visibility`。
    pub player_model: Entity,
    /// 相机子实体（俯视 3/4 机位，层级跟随玩家）。
    pub camera: Entity,
}

/// 敌人池：启动时一次性生成的蒙皮蠕虫实例槽位（`Visibility::HIDDEN`
/// 待命），按逻辑侧敌人 `id` 取用 / 归还——零运行期 spawn / despawn。
#[derive(Resource)]
pub struct EnemyPool {
    /// 全部槽位（调试 / 测试可枚举）。
    pub slots: Vec<Entity>,
    /// 闲置槽位（栈：后进先出，热槽位复用更友好）。
    free: Vec<Entity>,
    /// 活跃映射：逻辑敌人 id → 槽位实体。
    active: Vec<(u32, Entity)>,
}

/// 弹道池：与 [`EnemyPool`] 同款模式（40 个自发光小球实体）。
#[derive(Resource)]
pub struct ProjectilePool {
    pub slots: Vec<Entity>,
    free: Vec<Entity>,
    active: Vec<(u32, Entity)>,
}

impl EnemyPool {
    /// 以现成槽位建池（全部视为闲置）。
    fn new(slots: Vec<Entity>) -> Self {
        let free = slots.clone();
        Self { slots, free, active: Vec::new() }
    }

    /// 活跃映射（id → 实体，配对稳定）。
    pub fn active(&self) -> &[(u32, Entity)] {
        &self.active
    }

    /// 闲置槽位数。
    pub fn free_count(&self) -> usize {
        self.free.len()
    }

    /// 按逻辑 id 取槽位实体（未取用 → `None`）。
    pub fn entity_of(&self, id: u32) -> Option<Entity> {
        self.active.iter().find(|(active_id, _)| *active_id == id).map(|(_, e)| *e)
    }

    /// 为逻辑 id 取一个闲置槽位；池空返回 `None`（不超发）。
    pub fn acquire(&mut self, id: u32) -> Option<Entity> {
        if self.entity_of(id).is_some() {
            return self.entity_of(id); // 幂等：同 id 重复取用返回既有槽位
        }
        let entity = self.free.pop()?;
        self.active.push((id, entity));
        Some(entity)
    }

    /// 归还逻辑 id 的槽位（未取用 → `None`）。
    pub fn release(&mut self, id: u32) -> Option<Entity> {
        let index = self.active.iter().position(|(active_id, _)| *active_id == id)?;
        let (_, entity) = self.active.swap_remove(index);
        self.free.push(entity);
        Some(entity)
    }
}

impl ProjectilePool {
    /// 以现成槽位建池（全部视为闲置）。
    fn new(slots: Vec<Entity>) -> Self {
        let free = slots.clone();
        Self { slots, free, active: Vec::new() }
    }

    /// 活跃映射（id → 实体，配对稳定）。
    pub fn active(&self) -> &[(u32, Entity)] {
        &self.active
    }

    /// 闲置槽位数。
    pub fn free_count(&self) -> usize {
        self.free.len()
    }

    /// 按逻辑 id 取槽位实体（未取用 → `None`）。
    pub fn entity_of(&self, id: u32) -> Option<Entity> {
        self.active.iter().find(|(active_id, _)| *active_id == id).map(|(_, e)| *e)
    }

    /// 为逻辑 id 取一个闲置槽位；池空返回 `None`（不超发）。
    pub fn acquire(&mut self, id: u32) -> Option<Entity> {
        if self.entity_of(id).is_some() {
            return self.entity_of(id);
        }
        let entity = self.free.pop()?;
        self.active.push((id, entity));
        Some(entity)
    }

    /// 归还逻辑 id 的槽位（未取用 → `None`）。
    pub fn release(&mut self, id: u32) -> Option<Entity> {
        let index = self.active.iter().position(|(active_id, _)| *active_id == id)?;
        let (_, entity) = self.active.swap_remove(index);
        self.free.push(entity);
        Some(entity)
    }
}

/// 波次生存竞技场插件：Startup 载入场景 + 建池，Update 推进逻辑与表现。
///
/// [`Default`] 的关卡路径是 [`DEFAULT_SCENE_PATH`]、敌人模型路径是
/// [`ENEMY_MODEL_PATH`]——均经 [`asset_path`] 解析（CWD 相对优先、
/// workspace 根相对回退）：`cargo run` 与 `cargo test` 两种工作目录下
/// 都能命中仓库资产，无须按入口注入绝对路径。测试用
/// [`ArenaPlugin::new`] 注入任意场景路径（headless 测试写临时 fixture
/// 关卡；显式路径优先于默认解析）。
#[derive(Debug, Clone)]
pub struct ArenaPlugin {
    /// Startup 时 [`load_scene`] 的场景文件路径（关卡数据与代码分离）。
    pub scene_path: PathBuf,
    /// 敌人模型（蒙皮蠕虫 GLB）路径——池实例化的资产来源。
    pub enemy_model_path: PathBuf,
}

impl Default for ArenaPlugin {
    fn default() -> Self {
        Self {
            scene_path: asset_path(DEFAULT_SCENE_PATH),
            enemy_model_path: asset_path(ENEMY_MODEL_PATH),
        }
    }
}

impl ArenaPlugin {
    /// 以指定场景文件构造（测试注入 fixture 关卡、示例显式给默认路径）。
    pub fn new(scene_path: impl Into<PathBuf>) -> Self {
        Self { scene_path: scene_path.into(), ..Self::default() }
    }

    /// 把本插件的全部系统注册到调度器。
    ///
    /// 与 `DefaultPlugins` 的编排方式一致（Startup 一次、Update 每帧）；
    /// 独立暴露是为了 headless 测试与离屏快照在不建 `App` / 窗口 / GPU
    /// 的前提下复用同一条推进路径。**动画与层级不在此注册**：调用方按
    /// 窗口路径的编排补上 `AnimationPlugin`（Update）与 `HierarchyPlugin`
    /// （PostUpdate）。
    pub fn build_schedule(&self, schedule: &mut Schedule) {
        let scene_path = self.scene_path.clone();
        let enemy_model_path = self.enemy_model_path.clone();
        schedule
            .add_system(Stage::Startup, setup_resources)
            .add_system(Stage::Startup, move |world: &mut World| {
                build_scene(world, &scene_path, &enemy_model_path)
            })
            // Update 内的顺序即注册顺序（读写集冲突者分批保序）：
            // 逻辑推进 → 敌人 / 弹道 / 玩家表现 → HUD。动画（AnimationPlugin）
            // 与层级传播（HierarchyPlugin）的相对编排见本方法文档。
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<Input>()
                    .read::<Time>()
                    .read::<Audio>()
                    .read::<ArenaAssets>()
                    .read::<ArenaStatics>()
                    .write::<ArenaState>(),
                run_arena,
            )
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<ArenaState>()
                    .read::<ArenaEntities>()
                    .read::<Time>()
                    .write::<Transform>()
                    .write::<Visibility>()
                    .write::<EnemyPool>()
                    .write::<ProjectilePool>(),
                update_arena_visuals,
            )
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<ArenaState>()
                    .write::<UiText>()
                    .write::<UiRect>()
                    .write::<UiColor>(),
                update_arena_hud,
            );
    }
}

impl Plugin for ArenaPlugin {
    fn build(&self, app: &mut App) {
        self.build_schedule(&mut app.schedule);
    }
}

/// Startup 1：资源兜底插入。
///
/// 引擎资源（Store / 相机 / 泛光 / 音频）按 DefaultPlugins 的
/// ResourcePlugin 语义**缺席才插入**——窗口程序里 RenderPlugin /
/// AudioPlugin 已就位，不得覆盖（音频设备句柄是独占的）；headless /
/// 离屏路径则由这里补齐。headless 测试请预插入 `Audio::headless()`（同样
/// 不会被这里覆盖），避免 `cargo test` 间歇性打开真实设备。游戏资源
/// （[`ArenaState`] / [`ArenaAssets`]）由本插件独占，直接插入。
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
    world.insert_resource(ArenaState::new(ARENA_HALF));
    world.insert_resource(ArenaAssets {
        shoot: sine(1200.0, 0.06, 48_000),
        hit: sine(300.0, 0.08, 48_000),
        hurt: sine(150.0, 0.25, 48_000),
        wave: double_beep(48_000),
    });
}

/// Startup 2：载入关卡场景 + 收集静态体 + 玩家 / HUD / 双池搭建。
///
/// **关卡来自场景文件**（[`load_scene`]）：环境段（方向光）+ 地面 /
/// 四面围墙（内缘 ±24，静态 Box collider）/ 七根障碍柱（静态 collider，
/// 错落分布制造绕行）/ 场地中央点光（cubemap 阴影）。加载失败不 panic：
/// `log::error` 后继续（无墙模式下逻辑 ±24 钳制兜底，游戏仍可玩）。
///
/// 随后把场景静态 `Collider` 一次性转换进 [`ArenaStatics`]（Update 每帧
/// 零查询），再生成游戏私有实体：玩家三层结构（根 ← 模型 ← 炮管；相机
/// 挂根）、HUD 文本 / 血条、敌人池（蒙皮实例 × 15）与弹道池（小球 × 40）。
fn build_scene(world: &mut World, scene_path: &Path, enemy_model_path: &Path) {
    // ---- 场景文件：环境 + 地面 / 围墙 / 障碍柱 / 点光（含静态 collider）----
    if let Err(error) = load_scene(world, scene_path) {
        log::error!(
            "ArenaPlugin：场景文件 {} 加载失败（{error}）——以无场景模式继续\
             （玩家 / 池 / HUD 照常，边界由逻辑钳制兜底）",
            scene_path.display()
        );
        if !world.contains_resource::<DirectionalLight>() {
            world.insert_resource(DirectionalLight::default());
        }
    }

    // ---- 静态体收集：场景 Collider → 逻辑 StaticShape（一次性）----
    // 「仅 Collider 无 RigidBody = 静态」与 is_static 取或（引擎缺席语义）；
    // Box 只取 XZ 半边长（俯视玩法垂直方向无碰撞）。
    let mut statics = ArenaStatics::default();
    world.query::<(&Transform, &Collider, Option<&RigidBody>)>(
        |(transform, collider, rigid)| {
            if !collider.is_static && rigid.is_some() {
                return; // 动态刚体不进竞技场静态表（本关卡也不该有）
            }
            let shape = match collider.shape {
                ColliderShape::Sphere { radius } => StaticShape::Sphere { radius },
                ColliderShape::Box { half_extents } => StaticShape::Box {
                    half_x: half_extents.x,
                    half_z: half_extents.z,
                },
            };
            statics.0.push((transform.position, shape));
        },
    );
    world.insert_resource(statics);

    // ---- 玩家网格与材质（游戏私有，不进场景文件）----
    let cube_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let player_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.25, 0.55, 0.95, 1.0));
    let barrel_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.12, 0.14, 0.18, 1.0));

    // ---- 玩家三层结构：根（逻辑位置）← 模型（朝向 + 无敌闪烁）← 炮管；
    // 相机子实体与朝向解耦（iris-game 实测：朝向写根会让相机公转）----
    let player = world.spawn((Transform::IDENTITY,));
    let player_model = world.spawn((
        Transform {
            position: Vec3::new(0.0, PLAYER_MODEL_LOCAL_Y, 0.0),
            scale: Vec3::new(0.45, 0.55, 0.45),
            ..Transform::IDENTITY
        },
        cube_mesh,
        player_mat,
        // 无敌闪烁会翻转该组件：spawn 时就位（并行系统只能 get_mut 翻转）
        Visibility::VISIBLE,
    ));
    world.add_child(player, player_model);
    let barrel = world.spawn((
        Transform {
            position: Vec3::new(0.0, 0.0, -0.55),
            scale: Vec3::new(0.12, 0.12, 0.5),
            ..Transform::IDENTITY
        },
        cube_mesh,
        barrel_mat,
    ));
    world.add_child(player_model, barrel);
    // 俯视 3/4 机位：绕 X -59° 后局部 -Z 前方指向斜下，望向玩家脚下
    let camera = world.spawn((
        Transform {
            position: Vec3::new(0.0, 20.0, 12.0),
            rotation: Quat::from_rotation_x(-59.0_f32.to_radians()),
            scale: Vec3::ONE,
        },
        ActiveCamera::default(),
    ));
    world.add_child(player, camera);

    // ---- 敌人池：蒙皮蠕虫实例 × 15，启动即隐藏待命 ----
    // 每实例挂 AnimationPlayer（glTF 内嵌 wiggle 剪辑自动循环播放）——
    // 「蒙皮 × 动画 × 池复用」的组合验证。加载失败降级：池容量收缩 +
    // log 告警（游戏仍可玩，只是可见敌人变少）。
    let mut enemy_slots = Vec::new();
    for slot in 0..POOL_ENEMIES {
        match spawn_gltf_instance(world, enemy_model_path) {
            Ok(instance) => {
                let root = instance.roots[0];
                // 蒙皮实例结构：加载器已把兄弟顶层关节树收编为根实体的
                // 子树（引擎 API 打磨 II），移动槽位根实体整只蠕虫跟随，
                // 游戏内不再需要手动 add_child（旧绕行已删）
                world.insert(root, instance.player());
                // 待命即隐藏：全部渲染 pass 跳过（组件缺席 = 可见，故必须
                // 在 spawn 后立刻插入 HIDDEN，池化翻转由表现系统 get_mut）
                world.insert(root, Visibility::HIDDEN);
                world.insert(root, Name(format!("arena/enemy-pool/{slot}")));
                enemy_slots.push(root);
            }
            Err(error) => {
                log::error!(
                    "ArenaPlugin：敌人模型 {} 加载失败（{error}）\
                     ——敌人池容量收缩为 {}",
                    enemy_model_path.display(),
                    enemy_slots.len()
                );
                break;
            }
        }
    }
    world.insert_resource(EnemyPool::new(enemy_slots));

    // ---- 弹道池：自发光小球 × 40（HDR 泛光的光源），同样隐藏待命 ----
    let bullet_mesh = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(0.22, 12, 8));
    let bullet_mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [1.0, 0.85, 0.3, 1.0],
        emissive: [2.0, 1.5, 0.5, 1.0],
        metallic: 0.0,
        roughness: 0.4,
        ..MaterialDef::default()
    });
    let mut bullet_slots = Vec::new();
    for _ in 0..POOL_PROJECTILES {
        let entity = world.spawn((
            Transform::from_xyz(0.0, PROJECTILE_Y, 0.0),
            bullet_mesh,
            bullet_mat,
            Visibility::HIDDEN,
        ));
        bullet_slots.push(entity);
    }
    world.insert_resource(ProjectilePool::new(bullet_slots));

    // ---- HUD（屏幕像素坐标，文本实体的 UiRect 仅作定位载体）----
    // 文本实体挂 Name：刷新系统按名定位（O(n) 扫描，本场景 n ≈ 30 可忽略）
    let title = UiText { string: "IRIS ARENA".into(), scale: 3.0 };
    world.spawn((
        Name(TITLE_TEXT_NAME.into()),
        UiRect { position: [24.0, 20.0], size: [0.0, 0.0] },
        title,
        UiColor::default(),
    ));
    world.spawn((
        Name(WAVE_TEXT_NAME.into()),
        UiRect { position: [HUD_WIDTH - text_width(&wave_text(0)) - 24.0, 20.0], size: [0.0, 0.0] },
        wave_text(0),
        UiColor::default(),
    ));
    world.spawn((
        Name(SCORE_TEXT_NAME.into()),
        UiRect { position: [24.0, 60.0], size: [0.0, 0.0] },
        UiText { string: "SCORE 0".into(), scale: 3.0 },
        UiColor([1.0, 0.85, 0.3, 1.0]),
    ));
    world.spawn((
        Name(COMBO_TEXT_NAME.into()),
        UiRect { position: [24.0, 100.0], size: [0.0, 0.0] },
        UiText { string: String::new(), scale: 2.5 },
        UiColor([1.0, 0.5, 0.15, 1.0]),
    ));
    // HP 条：深底背景条 + 红色前景条（宽度 = hp/max × 条宽，每帧刷新）
    world.spawn((
        Name(HP_BACK_NAME.into()),
        UiRect { position: [22.0, 152.0], size: [HP_BAR_WIDTH + 4.0, 18.0] },
        UiColor([0.08, 0.08, 0.1, 0.9]),
    ));
    world.spawn((
        Name(HP_FRONT_NAME.into()),
        UiRect {
            position: [24.0, 154.0],
            size: [hp_bar_width(PLAYER_MAX_HP), 14.0],
        },
        UiColor([0.9, 0.2, 0.2, 1.0]),
    ));
    // 横幅：空串时不产生顶点（对局中不可见）；标题 / 开波 / 终局时居中
    let banner = "PRESS ANY KEY TO START";
    let banner_width = text_width(&UiText { string: banner.into(), scale: 4.0 });
    world.spawn((
        Name(BANNER_TEXT_NAME.into()),
        UiRect { position: [(HUD_WIDTH - banner_width) * 0.5, 300.0], size: [0.0, 0.0] },
        UiText { string: banner.into(), scale: 4.0 },
        UiColor([1.0, 0.85, 0.3, 1.0]),
    ));

    world.insert_resource(ArenaEntities { player, player_model, camera });
}

/// WAVE 文本（Title 阶段波次为 0 → 显示占位）。
fn wave_text(wave: u32) -> UiText {
    let string =
        if wave == 0 { "WAVE -".to_string() } else { format!("WAVE {wave}") };
    UiText { string, scale: 3.0 }
}

/// HP 前景条宽度（hp / max × 条宽）。
fn hp_bar_width(hp: i32) -> f32 {
    HP_BAR_WIDTH * (hp.max(0) as f32 / PLAYER_MAX_HP as f32)
}

/// 开波双音：660 Hz 短音 + 间隙 + 990 Hz 长音。各段用程序化生成器产出
/// （自带首尾 5 ms 包络防爆音），[`AudioClip::concat`] 顺序拼接——
/// 采样访问与拼接公开后无须再手写采样缓冲（dogfooding 摩擦⑤已闭合）。
fn double_beep(sample_rate: u32) -> AudioClip {
    const TONE_A: f32 = 0.12;
    const GAP: f32 = 0.06;
    const TONE_B: f32 = 0.18;
    let tone_a = sine(660.0, TONE_A, sample_rate);
    let tone_b = sine(990.0, TONE_B, sample_rate);
    let gap = (GAP * sample_rate as f32) as usize;
    let silence =
        AudioClip::from_interleaved(vec![0.0; gap], 1, sample_rate).expect("程序化参数恒合法");
    AudioClip::concat(&[&tone_a, &silence, &tone_b])
        .expect("同参数生成的剪辑布局必然一致")
}

/// Update 1：读输入与时间 → 推进纯逻辑 → 差分事件计数触发音效。
///
/// 事件（开火 / 命中 / 受伤 / 换波）由逻辑侧单调计数器的前后差分检出——
/// 逻辑层保持纯数据，表现层（音效）完全由差分驱动，无需事件队列。
fn run_arena(world: &World) {
    let dt = world
        .try_resource::<Time>()
        .map(|time| time.delta_secs())
        .unwrap_or(0.0);

    // ---- 输入翻译：WASD 世界系移动（相机方位恒定 → 屏幕相对一致）、
    // Shift 边沿冲刺、R 重开、any-key 探测「任意键」----
    let input = world
        .try_resource::<Input>()
        .map(|keys| {
            let move_x =
                (keys.key(KeyCode::KeyD) as i32 - keys.key(KeyCode::KeyA) as i32) as f32;
            let move_z =
                (keys.key(KeyCode::KeyS) as i32 - keys.key(KeyCode::KeyW) as i32) as f32;
            ArenaInput {
                move_x,
                move_z,
                dash: keys.key_pressed(KeyCode::ShiftLeft)
                    || keys.key_pressed(KeyCode::ShiftRight),
                // 任意键：Input::any_key_pressed（引擎 API 打磨 II 闭合——
                // 旧 8 键白名单会漏掉白名单外的按键，玩家卡在标题屏）
                confirm: keys.any_key_pressed(),
                restart: keys.key_pressed(KeyCode::KeyR),
            }
        })
        .unwrap_or(ArenaInput::IDLE);

    let Some(mut state) = world.try_resource_mut::<ArenaState>() else {
        return;
    };
    let (shots_before, hits_before, kills_before, damage_before, wave_before) = (
        state.total_shots,
        state.total_hits,
        state.total_kills,
        state.total_damage,
        state.wave,
    );
    let statics_list = world
        .try_resource::<ArenaStatics>()
        .map(|statics| statics.0.clone())
        .unwrap_or_default();
    state.step(&input, dt, &statics_list);

    // ---- 音效：差分触发（每次状态变化至多一响，音量按频繁程度压低）。
    // saturating_sub：restart 会把计数器复位为 0——差分可能「为负」，
    // 此时应静音（重开帧不补播旧对局的事件）----
    let events = (
        state.total_shots.saturating_sub(shots_before),
        state.total_hits.saturating_sub(hits_before),
        state.total_kills.saturating_sub(kills_before),
        state.total_damage.saturating_sub(damage_before),
        state.wave != wave_before,
    );
    drop(state);
    if events == (0, 0, 0, 0, false) {
        return;
    }
    if let (Some(assets), Some(audio)) =
        (world.try_resource::<ArenaAssets>(), world.try_resource::<Audio>())
    {
        if events.0 > 0 {
            audio.play(assets.shoot.clone(), 0.22, false);
        }
        if events.1 > 0 {
            audio.play(assets.hit.clone(), 0.35, false);
        }
        if events.3 > 0 {
            audio.play(assets.hurt.clone(), 0.6, false);
        }
        if events.4 {
            // wave 计数在 restart 时也会变化（旧波 → 1）：重开帧响开波音
            // 恰好是想要的反馈，无需区分来源
            audio.play(assets.wave.clone(), 0.5, false);
        }
        // 击杀（events.2）不单独配音：命中音 + 连击涨分已足够反馈
    }
}

/// Update 2：逻辑状态 → 表现。玩家三层结构、敌人池槽位（可见性 / 变换 /
/// 受击缩放脉冲 / 取用与归还）、弹道池槽位。
///
/// 池槽位以逻辑侧稳定 `id` 映射：新增敌人 / 弹道 → `acquire`，消失 →
/// `release` + `Visibility::HIDDEN`。整个过程零 spawn / despawn——波次
/// 之间池槽位翻转可见性即完成「一波敌人退场、下一波登场」。
fn update_arena_visuals(world: &World) {
    let Some(state) = world.try_resource::<ArenaState>() else {
        return;
    };
    let elapsed = world
        .try_resource::<Time>()
        .map(|time| time.elapsed_secs())
        .unwrap_or(0.0);
    let Some(ents) = world.try_resource::<ArenaEntities>() else {
        return;
    };

    // ---- 玩家：根写位置（rotation 恒等，保相机方位稳定）、模型写朝向，
    // 无敌期模型闪烁（受伤红闪与冲刺残影共用一个低成本开关）----
    if let Some(mut transform) = world.get_mut::<Transform>(ents.player) {
        transform.position = Vec3::new(state.player.pos.x, 0.0, state.player.pos.y);
    }
    if let Some(mut model) = world.get_mut::<Transform>(ents.player_model) {
        model.rotation = Quat::from_rotation_y(state.player.facing);
    }
    if let Some(mut visibility) = world.get_mut::<Visibility>(ents.player_model) {
        let blink = state.phase == Phase::Playing
            && state.player.invuln > 0.0
            && (elapsed * PLAYER_INVULN_BLINK_HZ) as u32 % 2 == 1;
        *visibility = if blink { Visibility::HIDDEN } else { Visibility::VISIBLE };
    }

    // ---- 敌人池：先归还（退场槽位翻回隐藏），再取用（同帧「旧波退场 +
    // 新波登场」（restart / 直接改写状态）时不会因池被旧 id 占满而饿死）----
    let mut pool = match world.try_resource_mut::<EnemyPool>() {
        Some(pool) => pool,
        None => return,
    };
    let mut released: Vec<Entity> = Vec::new();
    pool.active.retain(|(id, entity)| {
        let alive = state.enemies.iter().any(|e| e.id == *id);
        if !alive {
            released.push(*entity);
        }
        alive
    });
    for entity in released {
        // 归还即隐藏：全部渲染 pass 跳过（表现退场），槽位回到待命态
        if let Some(mut visibility) = world.get_mut::<Visibility>(entity) {
            *visibility = Visibility::HIDDEN;
        }
        pool.free.push(entity);
    }
    for enemy in &state.enemies {
        if pool.entity_of(enemy.id).is_none() {
            pool.acquire(enemy.id);
        }
    }
    let mut enemy_jobs: Vec<(Entity, EnemyState)> = Vec::new();
    for enemy in &state.enemies {
        if let Some(entity) = pool.entity_of(enemy.id) {
            enemy_jobs.push((entity, enemy.clone()));
        }
        // 池耗尽（容量收缩降级模式）的敌人无槽位可写：静默跳过
    }
    drop(pool);
    for (entity, enemy) in enemy_jobs {
        if let Some(mut visibility) = world.get_mut::<Visibility>(entity) {
            *visibility = Visibility::VISIBLE;
        }
        let Some(mut transform) = world.get_mut::<Transform>(entity) else {
            continue;
        };
        transform.position = Vec3::new(enemy.pos.x, 0.0, enemy.pos.y);
        // 敌人朝向追击方向：模型头部 +X → yaw = atan2(-dz, dx)，再补偿
        // 模型偏航（见 MODULE 文档「敌人根实体 × 蒙皮子树」）
        let chase = (state.player.pos - enemy.pos).normalize_or_zero();
        let yaw = (-chase.y).atan2(chase.x) + MODEL_YAW_OFFSET;
        transform.rotation = Quat::from_rotation_y(yaw);
        // 受击缩放脉冲：0.12 s 内从 +30% 平滑回落（蒙皮根均匀缩放）
        let pulse = 1.0 + 0.3 * (enemy.hit_flash / ENEMY_HIT_FLASH);
        transform.scale = Vec3::splat(pulse);
    }

    // ---- 弹道池：同款模式（先归还后取用；小球无朝向语义，只写位置）----
    let mut pool = match world.try_resource_mut::<ProjectilePool>() {
        Some(pool) => pool,
        None => return,
    };
    let mut released: Vec<Entity> = Vec::new();
    pool.active.retain(|(id, entity)| {
        let alive = state.projectiles.iter().any(|p| p.id == *id);
        if !alive {
            released.push(*entity);
        }
        alive
    });
    for entity in released {
        if let Some(mut visibility) = world.get_mut::<Visibility>(entity) {
            *visibility = Visibility::HIDDEN;
        }
        pool.free.push(entity);
    }
    for projectile in &state.projectiles {
        if pool.entity_of(projectile.id).is_none() {
            pool.acquire(projectile.id);
        }
    }
    let mut bullet_jobs: Vec<(Entity, ProjectileState)> = Vec::new();
    for projectile in &state.projectiles {
        if let Some(entity) = pool.entity_of(projectile.id) {
            bullet_jobs.push((entity, projectile.clone()));
        }
    }
    drop(pool);
    for (entity, projectile) in bullet_jobs {
        if let Some(mut visibility) = world.get_mut::<Visibility>(entity) {
            *visibility = Visibility::VISIBLE;
        }
        if let Some(mut transform) = world.get_mut::<Transform>(entity) {
            transform.position = Vec3::new(projectile.pos.x, PROJECTILE_Y, projectile.pos.y);
        }
    }
}

/// Update 3：HUD 刷新——WAVE / SCORE / COMBO / HP 条每帧重写，横幅按
/// 阶段切换（标题提示 / 开波倒计时 / GAME OVER）。
///
/// 文本实体经 `Name` 按名定位（`entity_named` 为 O(n) 线性扫描，n ≈ 30
/// 可忽略）；横幅居中偏移按当前文本的 `text_width` 实时测量。
fn update_arena_hud(world: &World) {
    let Some(state) = world.try_resource::<ArenaState>() else {
        return;
    };

    if let Some(entity) = world.entity_named(WAVE_TEXT_NAME) {
        if let Some(mut text) = world.get_mut::<UiText>(entity) {
            *text = wave_text(state.wave);
        }
    }
    if let Some(entity) = world.entity_named(SCORE_TEXT_NAME) {
        if let Some(mut text) = world.get_mut::<UiText>(entity) {
            text.string = format!("SCORE {}", state.score);
        }
    }
    if let Some(entity) = world.entity_named(COMBO_TEXT_NAME) {
        if let Some(mut text) = world.get_mut::<UiText>(entity) {
            text.string = if state.combo >= 2 {
                format!("COMBO x{}", state.combo)
            } else {
                String::new()
            };
        }
    }
    if let Some(entity) = world.entity_named(HP_FRONT_NAME) {
        if let Some(mut rect) = world.get_mut::<UiRect>(entity) {
            rect.size = [hp_bar_width(state.player.hp), 14.0];
        }
    }
    if let Some(entity) = world.entity_named(BANNER_TEXT_NAME) {
        // 横幅文案按优先级：终局 > 标题 > 开波倒计时 > 空
        let (banner, color): (String, [f32; 4]) = match state.phase {
            Phase::GameOver => {
                ("GAME OVER - PRESS R TO RESTART".to_string(), [1.0, 0.3, 0.25, 1.0])
            }
            Phase::Title => ("PRESS ANY KEY TO START".to_string(), [1.0, 0.85, 0.3, 1.0]),
            Phase::Playing if state.breather > 0.0 => (
                format!("WAVE {} IN {}...", state.wave + 1, state.breather.ceil() as i32),
                [0.7, 0.9, 1.0, 1.0],
            ),
            Phase::Playing => (String::new(), [1.0, 0.85, 0.3, 1.0]),
        };
        if let Some(mut text) = world.get_mut::<UiText>(entity) {
            text.string = banner;
            let width = text_width(&text);
            if let Some(mut rect) = world.get_mut::<UiRect>(entity) {
                rect.position = [(HUD_WIDTH - width) * 0.5, 300.0];
            }
        }
        if let Some(mut ui_color) = world.get_mut::<UiColor>(entity) {
            ui_color.0 = color;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 池语义：取用 / 归还 / 不超发 / 幂等取用（敌人与弹道池共用同一实现
    /// 模式，以敌人池为代表断言）。
    #[test]
    fn pool_acquire_release_never_oversubscribes() {
        let mut world = World::new();
        let slots: Vec<Entity> = (0..3).map(|_| world.spawn_empty()).collect();
        let mut pool = EnemyPool::new(slots.clone());
        assert_eq!(pool.free_count(), 3);

        let a = pool.acquire(10).expect("空闲池应可取用");
        let b = pool.acquire(11).expect("应可取用第二个");
        assert_eq!(pool.free_count(), 1);
        assert_eq!(pool.entity_of(10), Some(a));
        assert_eq!(pool.entity_of(11), Some(b));
        // 幂等：同 id 重复取用返回同一槽位、不再消耗
        assert_eq!(pool.acquire(10), Some(a));
        assert_eq!(pool.free_count(), 1);
        // 取用满第三个后池空：不超发
        let c = pool.acquire(12).expect("应可取用第三个");
        assert!(pool.acquire(13).is_none(), "池空不应超发");
        assert_eq!(pool.active().len(), 3);
        // 归还后可复用（同一实体槽位流转）
        assert_eq!(pool.release(11), Some(b));
        assert_eq!(pool.free_count(), 1);
        let reused = pool.acquire(14).expect("归还后应可再取用");
        assert_eq!(reused, b, "栈式闲置表应先归还后进槽位");
        assert!(pool.release(99).is_none(), "未取用的 id 归还应安全返回 None");
        let _ = (a, c);
    }
}
