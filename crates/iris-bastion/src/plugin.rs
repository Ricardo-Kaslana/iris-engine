//! [`BastionPlugin`]：把纯逻辑核心（`crate::logic`）接到 Iris 引擎上。
//!
//! 职责划分（iris-arena 的 `ArenaPlugin` 同款分层，载荷换成本轮 R21–R25
//! 的五个新能力）：
//! - **Startup**：`setup_resources`（兜底插入引擎资源 + 游戏资源）→
//!   `build_scene`（关卡全部代码生成：地面 + 静态 Box collider（拾取
//!   靶子）+ 航点路径带 + 边框 + 核心水晶 + 界外岩柱 + 灯光 + 相机 +
//!   幽灵预览 + **四池**（塔 / 敌 / 双弹 / FX，启动即全部隐藏待命，
//!   运行期零 spawn / despawn））；
//! - **Update**：`run_bastion`（读 `Input` / `Time` / `Viewport` →
//!   **鼠标拾取**——`cursor_position()` 像素 → NDC → 引擎
//!   `screen_ray_from_global` 世界射线 → `iris_core::physics::raycast`
//!   打地面静态 collider → 网格坐标；左键边沿 = 放塔请求，数字键 /
//!   右键 = 切换塔种 → `BastionState::step` → 事件列表触发**空间音频**
//!   （`play_spatial` 定位事件世界坐标，听者每帧跟随俯视相机））→
//!   `trigger_bastion_fx`（事件 → **粒子**：发射器挂 FX 池槽位实体，
//!   参数覆写 + `reset` 重新上膛按需触发）→ `update_bastion_visuals`
//!   （幽灵预览 + 四池表现：塔池下标直映射、敌池 / 弹池以稳定 id 映射）
//!   → `update_bastion_hud`（积分 / 波次 / 核心 HP / 当前塔种与价格 /
//!   胜负横幅，文本实体经 `Name` 按名定位）。
//! - **层级传播**：交给 `HierarchyPlugin`（PostUpdate）；**粒子推进**：
//!   交给 `ParticlePlugin`（PostUpdate、传播之后）——窗口路径由
//!   `DefaultPlugins` 注册，headless 测试 / 离屏快照需自行
//!   `HierarchyPlugin.build(&mut app)` + `ParticlePlugin.build(&mut app)`
//!   （与 iris-arena 的 `AnimationPlugin` 约定同款），本插件不重复注册。
//!
//! # 鼠标拾取（R21 dogfooding 重点）
//!
//! 屏幕像素 → NDC（y 翻转）→ 引擎 `screen_ray_from_global`（vp 逆对近 /
//! 远平面各取一点，见 `iris_render::camera`）→ 世界射线 →
//! `raycast(world, origin, dir, 400)` 打地面静态碰撞体 → 命中点 → 网格
//! 坐标。**刻意走引擎查询而非解析平面**：地面是场景里的静态 Box
//! collider，拾取与渲染 / 物理共用同一份几何事实来源。屏幕 → 射线与视口
//! 尺寸两个引擎缺口已于「打磨 III」闭合（`Camera::screen_ray` /
//! `screen_ray_from_global` + `Viewport` 资源），本插件的手工 vp 逆与
//! `BastionViewport` 绕行已删。
//!
//! # 幽灵预览（R23 dogfooding）
//!
//! 待放塔位置挂半透明塔身（`AlphaMode::Blend`）+ 射程半透明圆盘；
//! [`BastionState::check_placement`] 三分支通过 = 蓝调、失败 = 红调。
//! 光标离场 / 终局时整组隐藏（射程盘不挂 `Visibility`，由**层级级联**
//! 随根一并隐藏——「打磨 III」前的逐实体过滤语义下它会滞留画面）。
//!
//! 本插件只使用引擎公开 API（dogfooding III）：不依赖引擎内部路径。

use glam::{IVec2, Quat, Vec2, Vec3};

use iris_core::physics::{Collider, raycast};
use iris_core::prelude::{Access, Entity, Name, Schedule, Stage};
use iris_engine::prelude::*;

use crate::logic::{
    BastionEvent, BastionInput, BastionState, CORE_MAX_HP, CORE_POS, EnemyKind, FIELD_MIN, GRID_H,
    GRID_W, MAX_WAVES, MORTAR_COST, MORTAR_RANGE, Phase, SENTRY_COST, SENTRY_RANGE, START_CREDITS,
    TowerKind, WAYPOINTS, cell_center, cell_in_field, path_dir, world_to_cell,
};

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 塔对象池容量（与 [`MAX_TOWERS`](crate::logic::MAX_TOWERS) 同规格；
/// 塔从不移除，池槽位以逻辑列表下标直映射）。
pub const POOL_TOWERS: usize = 40;
/// 敌人对象池容量（与逻辑侧在场上限同规格）。
pub const POOL_ENEMIES: usize = 24;
/// 哨兵弹池容量。
pub const POOL_BULLETS: usize = 32;
/// 臼炮弹池容量。
pub const POOL_SHELLS: usize = 8;
/// FX（一次性粒子发射器）池容量：环形复用，单帧事件数超过容量时后者
/// 覆盖前者（同一帧同槽重触发 = 发射器就地重置，先前 burst 已出生的
/// 粒子不受影响——粒子出生即世界坐标，与发射器解耦；60Hz 下实测单帧
/// 事件远小于 12，仅极端爆帧时退化）。
pub const POOL_FX: usize = 12;
/// 空间音频距离参考（米）：俯视相机听者悬在 ~25 m 高空，若用混音器默认
/// 4 m 参考全场音效会衰减到不可闻——按堡垒尺度放大到 25 m。
pub const BASTION_REFERENCE_DISTANCE: f32 = 25.0;

/// 弹道飞行高度（米，哨兵弹）。
const BULLET_Y: f32 = 0.55;
/// 臼炮弹抛物线顶高（米，按飞行进度 4f(1−f) 内插）。
const SHELL_ARC_HEIGHT: f32 = 2.4;
/// 敌人 / 塔模型头部朝本地 +X，引擎朝向约定前方是 -Z，+90° 让「写入的
/// 朝向」与「模型头向」对齐（iris-arena 同款补偿）。
const MODEL_YAW_OFFSET: f32 = std::f32::consts::FRAC_PI_2;

/// HUD 实体名（`Name` 组件，按名定位用）。
pub const CREDITS_TEXT_NAME: &str = "bastion/hud/credits";
pub const WAVE_TEXT_NAME: &str = "bastion/hud/wave";
pub const CORE_TEXT_NAME: &str = "bastion/hud/core";
pub const BUILD_TEXT_NAME: &str = "bastion/hud/build";
pub const BANNER_TEXT_NAME: &str = "bastion/hud/banner";

// ---------------------------------------------------------------------------
// 插件侧资源
// ---------------------------------------------------------------------------

/// 适配：把纯逻辑类型接入引擎资源存储（逻辑核心零引擎依赖，跨模块手动
/// 实现 `Resource` 即可「选择性接入」）。
impl iris_core::resource::Resource for BastionState {}

/// 当前选中的待放塔种（数字键 1 / 2 或右键切换；幽灵预览与左键放塔共用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GhostKind(pub TowerKind);

impl Default for GhostKind {
    fn default() -> Self {
        Self(TowerKind::Sentry)
    }
}

impl iris_core::resource::Resource for GhostKind {}

/// 本帧光标拾取结果（`run_bastion` 写、表现层读）：命中格 + 当前塔种在
/// 该格的放塔校验是否通过（幽灵预览红蓝着色的依据）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HoverCell {
    /// 光标射线命中的网格（无光标 / 无相机 / 界外 → `None`）。
    pub cell: Option<IVec2>,
    /// 当前塔种在 `cell` 的放塔校验是否通过。
    pub placeable: bool,
}

impl iris_core::resource::Resource for HoverCell {}

/// 预生成的程序化音效（全部确定性、Startup 一次生成）。
#[derive(Clone, Resource)]
pub struct BastionAssets {
    /// 哨兵射击音：1250 Hz 短 blip。
    pub shoot: AudioClip,
    /// 臼炮发射音：170 Hz 低频闷响。
    pub thump: AudioClip,
    /// 命中音：640 Hz 短音。
    pub hit: AudioClip,
    /// 臼炮落点爆炸音：宽带噪声。
    pub boom: AudioClip,
    /// 敌人死亡音：另一噪声种子（更短促）。
    pub death: AudioClip,
    /// 漏怪告警音：392 / 262 Hz 下行双音。
    pub alarm: AudioClip,
}

/// 场景实体索引：相机 / 幽灵预览 / 核心等**每帧热路径**的句柄资源。
/// HUD 文本实体经引擎 `Name` + `World::entity_named` 按名定位（低频路径）；
/// 四类池由各自的池资源管理，不进本索引。
#[derive(Clone, Copy, Resource)]
pub struct BastionEntities {
    /// 俯视固定相机实体（`ActiveCamera`）。
    pub camera: Entity,
    /// 幽灵预览根实体（可见性开关；位置跟随光标格）。
    pub ghost_root: Entity,
    /// 幽灵塔身（哨兵形态，Blend 材质，红蓝可换）。
    pub ghost_sentry_body: Entity,
    /// 幽灵塔身（臼炮形态，Blend 材质，红蓝可换）。
    pub ghost_mortar_body: Entity,
    /// 射程指示盘（Blend 材质，按塔种缩放到射程半径）。
    pub ghost_ring: Entity,
    /// 核心水晶实体（漏怪告警粒子的定位锚点）。
    pub core: Entity,
}

/// 幽灵预览的两种 Blend 材质（可放 = 蓝调 / 不可放 = 红调；表现层按
/// 校验结果换挂）。
#[derive(Clone, Copy, Resource)]
pub struct GhostMaterials {
    /// 可放：蓝调半透明。
    pub ok: MaterialHandle,
    /// 不可放：红调半透明。
    pub blocked: MaterialHandle,
    /// 射程指示盘：更淡的蓝调。
    pub ring: MaterialHandle,
}

// ---------------------------------------------------------------------------
// 对象池（iris-arena 同款：启动预热、可见性翻转、零运行期 spawn/despawn）
// ---------------------------------------------------------------------------

/// 通用池芯：闲置栈 + 活跃映射（逻辑稳定 id → 槽位实体）。塔池与 FX 池
/// 不走 id 映射（塔以列表下标直映射、FX 环形复用），只有敌人 / 双弹池用。
#[derive(Resource)]
pub struct MappedPool {
    /// 全部槽位（调试 / 测试可枚举）。
    pub slots: Vec<Entity>,
    free: Vec<Entity>,
    active: Vec<(u32, Entity)>,
}

impl MappedPool {
    /// 以现成槽位建池（全部视为闲置）。
    fn new(slots: Vec<Entity>) -> Self {
        let free = slots.clone();
        Self {
            slots,
            free,
            active: Vec::new(),
        }
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
        self.active
            .iter()
            .find(|(active_id, _)| *active_id == id)
            .map(|(_, e)| *e)
    }

    /// 为逻辑 id 取一个闲置槽位；池空返回 `None`（不超发；幂等：同 id
    /// 重复取用返回既有槽位）。
    pub fn acquire(&mut self, id: u32) -> Option<Entity> {
        if let Some(entity) = self.entity_of(id) {
            return Some(entity);
        }
        let entity = self.free.pop()?;
        self.active.push((id, entity));
        Some(entity)
    }

    /// 归还逻辑 id 的槽位（未取用 → `None`）。
    pub fn release(&mut self, id: u32) -> Option<Entity> {
        let index = self
            .active
            .iter()
            .position(|(active_id, _)| *active_id == id)?;
        let (_, entity) = self.active.swap_remove(index);
        self.free.push(entity);
        Some(entity)
    }
}

/// 敌人池：按逻辑敌人稳定 id 映射槽位（独立 newtype：资源按 TypeId
/// 寻址，三个同构池必须是三个不同类型，type alias 会互相覆盖——见
/// `World::insert_resource` 的模式警示）。
#[derive(Resource)]
pub struct EnemyPool {
    /// 槽位映射芯（稳定 id → 槽位根实体）。
    pub slots: MappedPool,
    /// 每槽的双形态 rig（`[疾行虫 rig, 重甲兽 rig]`，与
    /// [`EnemyPool::slots`] 的槽位序平行）：夹层 rig 无网格，可见性 =
    /// 本敌人的当前形态（**层级级联**下翻 rig 即整组切换，孙辈网格
    /// 组件缺席 = 可见）。
    pub forms: Vec<[Entity; 2]>,
}

/// 哨兵弹池：按逻辑弹道稳定 id 映射槽位。
#[derive(Resource)]
pub struct BulletPool(pub MappedPool);
/// 臼炮弹池：按逻辑弹道稳定 id 映射槽位。
#[derive(Resource)]
pub struct ShellPool(pub MappedPool);

/// 塔池：塔从不移除，槽位以逻辑 `towers` 列表**下标**直映射（比 id 映射
/// 更简单：下标即稳定身份）。
#[derive(Resource)]
pub struct TowerPool {
    /// 槽位根实体（下标 = 逻辑 `towers` 下标；根可见性 = 塔在位与否）。
    pub slots: Vec<Entity>,
    /// 每槽的双形态 rig（`[哨兵 rig, 臼炮 rig]`，与 [`TowerPool::slots`]
    /// 平行）：夹层 rig 无网格，可见性 = 本塔的当前形态（**层级级联**
    /// 下翻 rig 即整组切换，孙辈网格组件缺席 = 可见）。
    pub forms: Vec<[Entity; 2]>,
}

/// FX 池：一次性粒子发射器的环形复用槽位（触发 = 槽位移到事件坐标 +
/// 发射器组件**就地复用**：参数全字段覆写 + `ParticleEmitter::reset`
/// 清上一发模拟态并让 burst 重新上膛——对象池槽位复用的正道，打磨 III
/// 起「换挂整只新组件」的绕行已删）。
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
// 拾取（R21 dogfooding：屏幕 → 世界射线 → 引擎 raycast → 网格）
// ---------------------------------------------------------------------------

/// 屏幕像素（物理像素，原点左上）→ 世界射线 `(origin, dir)`：视口尺寸取
/// 引擎 [`Viewport`] 资源（窗口程序由 `DefaultPlugins` 插入并在
/// `Resized` 时同步；headless / 离屏路径由 [`setup_resources`] 兜底插入，
/// 也可由调用方预插入覆写），NDC（y 翻转）→ 引擎
/// [`screen_ray_from_global`](iris_render::camera::screen_ray_from_global)
/// ——「屏幕 → 射线」与「视口资源」两个引擎缺口已于打磨 III 闭合，本
/// 函数只做像素 → NDC 换算与相机查询。像素在视口外同样成立（射线照算，
/// 合法性由调用方裁决）。
pub fn screen_ray(world: &World, px: f64, py: f64) -> Option<(Vec3, Vec3)> {
    let viewport = world.try_resource::<Viewport>()?;
    let (w, h) = (viewport.width.max(1), viewport.height.max(1));
    let ndc = Vec2::new(
        2.0 * px as f32 / w as f32 - 1.0,
        1.0 - 2.0 * py as f32 / h as f32,
    );
    let aspect = w as f32 / h as f32;
    let mut found: Option<(Vec3, Vec3)> = None;
    world.query::<(&GlobalTransform, &ActiveCamera)>(|(global, active)| {
        if found.is_none() {
            found = Some(screen_ray_from_global(global, active, ndc, aspect));
        }
    });
    let (origin, dir) = found?;
    (dir != Vec3::ZERO).then_some((origin, dir))
}

/// 世界坐标（y = 0 平面附近的点）→ 屏幕像素（`None` = 点在相机后方）。
/// 拾取的逆变换：vp 取引擎
/// [`view_proj_from_global`](iris_render::camera::view_proj_from_global)
/// （视口来源同 [`screen_ray`]）。headless 测试用「格子中心 → 像素 →
/// 射线 → 格子」闭环自检，也可用于场内目标的世界坐标 UI 标记。
pub fn world_to_screen(world: &World, pos: Vec3) -> Option<(f64, f64)> {
    let viewport = world.try_resource::<Viewport>()?;
    let (w, h) = (viewport.width.max(1), viewport.height.max(1));
    let aspect = w as f32 / h as f32;
    let mut found: Option<Vec2> = None;
    world.query::<(&GlobalTransform, &ActiveCamera)>(|(global, active)| {
        if found.is_none() {
            let clip = view_proj_from_global(global, active, aspect) * pos.extend(1.0);
            if clip.w.abs() >= 1e-9 && clip.w > 0.0 {
                found = Some(Vec2::new(clip.x, clip.y) / clip.w);
            }
        }
    });
    let ndc = found?;
    Some((
        ((ndc.x + 1.0) * 0.5 * w as f32) as f64,
        ((1.0 - ndc.y) * 0.5 * h as f32) as f64,
    ))
}

/// 光标像素 → 网格坐标：世界射线经引擎 `raycast` 打场景静态碰撞体
/// （地面），命中点落到网格；界外格返回 `None`。**刻意走引擎查询而非
/// 解析平面**（见模块文档）。
pub fn pick_ground_cell(world: &World, px: f64, py: f64) -> Option<IVec2> {
    let (origin, dir) = screen_ray(world, px, py)?;
    let (_, hit) = raycast(world, origin, dir, 400.0)?;
    let cell = world_to_cell(Vec2::new(hit.point.x, hit.point.z));
    if cell_in_field(cell) {
        Some(cell)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// 插件
// ---------------------------------------------------------------------------

/// 堡垒塔防插件：Startup 代码生成关卡 + 预热四池，Update 推进逻辑与表现。
/// 关卡全部来自代码（无需场景文件），`Default` 即可用。
#[derive(Debug, Clone, Default)]
pub struct BastionPlugin;

impl BastionPlugin {
    /// 把本插件的全部系统注册到调度器。
    ///
    /// 与 `DefaultPlugins` 的编排方式一致（Startup 一次、Update 每帧）；
    /// 独立暴露是为了 headless 测试与离屏快照在不建 `App` / 窗口 / GPU
    /// 的前提下复用同一条推进路径。**层级与粒子不在此注册**：调用方按
    /// 窗口路径的编排补上 `HierarchyPlugin`（PostUpdate）与
    /// `ParticlePlugin`（PostUpdate、层级之后）。
    pub fn build_schedule(&self, schedule: &mut Schedule) {
        schedule
            .add_system(Stage::Startup, setup_resources)
            .add_system(Stage::Startup, build_scene)
            // Update 内的顺序即注册顺序（读写集冲突者分批保序）：
            // 逻辑 + 拾取 + 音频 → FX 粒子触发 → 四池 / 幽灵表现 → HUD
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<Input>()
                    .read::<Time>()
                    .read::<Audio>()
                    .read::<Viewport>()
                    .read::<GlobalTransform>()
                    .read::<Transform>()
                    .read::<Collider>()
                    .write::<BastionState>()
                    .write::<GhostKind>()
                    .write::<HoverCell>(),
                run_bastion,
            )
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<BastionState>()
                    .read::<FxPool>()
                    .write::<Transform>()
                    .write::<ParticleEmitter>(),
                trigger_bastion_fx,
            )
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<BastionState>()
                    .read::<BastionEntities>()
                    .read::<GhostKind>()
                    .read::<HoverCell>()
                    .read::<GhostMaterials>()
                    .write::<Transform>()
                    .write::<Visibility>()
                    .write::<MaterialHandle>()
                    .write::<TowerPool>()
                    .write::<EnemyPool>()
                    .write::<BulletPool>()
                    .write::<ShellPool>(),
                update_bastion_visuals,
            )
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<BastionState>()
                    .read::<GhostKind>()
                    .write::<UiText>()
                    .write::<UiRect>()
                    .write::<UiColor>(),
                update_bastion_hud,
            );
    }
}

impl Plugin for BastionPlugin {
    fn build(&self, app: &mut App) {
        self.build_schedule(&mut app.schedule);
    }
}

/// Startup 1：资源兜底插入。
///
/// 引擎资源按 DefaultPlugins 的 ResourcePlugin 语义**缺席才插入**——
/// 窗口程序里 RenderPlugin / AudioPlugin 已就位，不得覆盖（音频设备句柄
/// 独占）；headless / 离屏路径则由这里补齐。headless 测试请预插入
/// `Audio::headless()`（同样不会被这里覆盖）。游戏资源由本插件独占，
/// 直接插入。
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
    world.insert_resource(BastionState::new());
    // 视口尺寸：窗口路径由 DefaultPlugins 维护（缺席才插入不会覆盖），
    // headless / 离屏路径由这里兜底（缺省 1280×720，与缺省窗口一致；
    // 需要其他口径的调用方可预插入覆写）
    if !world.contains_resource::<Viewport>() {
        world.insert_resource(Viewport::default());
    }
    // 空间音频场域尺度（见 BASTION_REFERENCE_DISTANCE 文档）：只调参数、
    // 不覆盖资源（设备句柄独占）
    if let Some(audio) = world.try_resource::<Audio>() {
        audio.set_reference_distance(BASTION_REFERENCE_DISTANCE);
    }
    world.insert_resource(BastionAssets {
        shoot: sine(1250.0, 0.05, 48_000),
        thump: sine(170.0, 0.16, 48_000),
        hit: sine(640.0, 0.045, 48_000),
        boom: noise(0.28, 48_000, 0xB055_1D5E),
        death: noise(0.22, 48_000, 0xDEAD_10CC),
        alarm: down_beep(48_000),
    });
    world.insert_resource(GhostKind::default());
    world.insert_resource(HoverCell::default());
}

/// 漏怪告警双音：392 Hz 短音 + 间隙 + 262 Hz 长音（下行 = 恶化告警）。
/// 各段用程序化生成器产出（自带首尾 5 ms 包络防爆音），
/// [`AudioClip::concat`] 顺序拼接。
fn down_beep(sample_rate: u32) -> AudioClip {
    const TONE_A: f32 = 0.12;
    const GAP: f32 = 0.06;
    const TONE_B: f32 = 0.2;
    let tone_a = sine(392.0, TONE_A, sample_rate);
    let tone_b = sine(262.0, TONE_B, sample_rate);
    let gap = (GAP * sample_rate as f32) as usize;
    let silence =
        AudioClip::from_interleaved(vec![0.0; gap], 1, sample_rate).expect("程序化参数恒合法");
    AudioClip::concat(&[&tone_a, &silence, &tone_b]).expect("同参数生成的剪辑布局必然一致")
}

/// Startup 2：代码生成关卡 + 幽灵预览 + HUD + 四池。
///
/// 全部实体：地面（含静态 Box collider = 拾取靶子）、航点路径带、场地
/// 边框、核心水晶（自发光 + 点光）、出生口标记、界外岩柱（压帧数用的
/// 真实剔除载荷：大半落在视锥外）、俯视固定相机、幽灵预览三件套、
/// HUD 文本、塔 / 敌 / 双弹 / FX 四池。
fn build_scene(world: &mut World) {
    // ---- 地面 + 拾取碰撞体（顶面 y = 0）----
    let ground_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(80.0));
    let ground_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.07, 0.12, 0.09, 1.0));
    world.spawn((
        Name("bastion/ground".into()),
        Transform::IDENTITY,
        ground_mesh,
        ground_mat,
    ));
    // 拾取碰撞体：独立的纯逻辑实体（无网格），盒顶面对齐 y = 0——射线
    // 命中点即游戏平面。渲染网格与拾取盒分离是为了让碰撞几何顶面精确
    // 落在 y = 0（盒有厚度，若与视觉网格同位，顶面会在 y = +0.5，掠射
    // 远角的拾取点会偏移近半米）
    world.spawn((
        Name("bastion/ground-collider".into()),
        Transform::from_xyz(0.0, -0.5, 0.0),
        // 顶面 y = 0 的静态薄盒：鼠标拾取射线的靶子（与渲染地面同一份
        // 水平几何事实来源）
        Collider::static_box(Vec3::new(13.0, 0.5, 10.0)),
    ));

    // ---- 航点路径带：逐段一条略高于地面的窄板（方向 = 段方向）----
    let path_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let path_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.5, 0.42, 0.28, 1.0));
    for pair in WAYPOINTS.windows(2) {
        let mid = pair[0].lerp(pair[1], 0.5);
        let len = pair[1].distance(pair[0]);
        let yaw = (-pair[1].y - -pair[0].y).atan2(pair[1].x - pair[0].x);
        // 板的本地 +X 为长度方向：缩放 (len + 0.9, 0.06, 0.9) 后旋转
        let along_x_yaw = std::f32::consts::FRAC_PI_2 - yaw;
        world.spawn((
            Name("bastion/path-seg".into()),
            Transform {
                position: Vec3::new(mid.x, 0.03, mid.y),
                rotation: Quat::from_rotation_y(along_x_yaw),
                scale: Vec3::new(len + 0.9, 1.0, 0.9),
            },
            path_mesh,
            path_mat,
        ));
    }

    // ---- 场地边框（可建区视觉边界）----
    let frame_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let frame_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.4, 0.42, 0.45, 1.0));
    let field_w = GRID_W as f32;
    let field_h = GRID_H as f32;
    let center_x = FIELD_MIN.x + field_w * 0.5;
    let center_z = FIELD_MIN.y + field_h * 0.5;
    for (position, scale) in [
        (
            Vec3::new(center_x, 0.05, FIELD_MIN.y - 0.15),
            Vec3::new(field_w + 0.6, 1.0, 0.3),
        ),
        (
            Vec3::new(center_x, 0.05, FIELD_MIN.y + field_h + 0.15),
            Vec3::new(field_w + 0.6, 1.0, 0.3),
        ),
        (
            Vec3::new(FIELD_MIN.x - 0.15, 0.05, center_z),
            Vec3::new(0.3, 1.0, field_h + 0.6),
        ),
        (
            Vec3::new(FIELD_MIN.x + field_w + 0.15, 0.05, center_z),
            Vec3::new(0.3, 1.0, field_h + 0.6),
        ),
    ] {
        world.spawn((
            Transform {
                position,
                scale,
                ..Transform::IDENTITY
            },
            frame_mesh,
            frame_mat,
        ));
    }

    // ---- 核心水晶（自发光 + 点光）+ 出生口标记 ----
    let core_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let core_mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [1.0, 0.72, 0.25, 1.0],
        emissive: [1.8, 1.1, 0.35, 1.0],
        metallic: 0.1,
        roughness: 0.3,
        ..MaterialDef::default()
    });
    let core = world.spawn((
        Name("bastion/core".into()),
        Transform {
            position: Vec3::new(CORE_POS.x, 0.75, CORE_POS.y),
            scale: Vec3::new(0.9, 1.5, 0.9),
            ..Transform::IDENTITY
        },
        core_mesh,
        core_mat,
    ));
    world.spawn((
        Name("bastion/core-light".into()),
        Transform::from_xyz(CORE_POS.x, 2.5, CORE_POS.y),
        PointLight {
            color: Vec3::new(1.0, 0.8, 0.5),
            intensity: 10.0,
            range: 14.0,
            shadows: true,
        },
    ));
    let spawn_pad_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.5, 0.12, 0.1, 1.0));
    let spawn_pos = WAYPOINTS[0];
    world.spawn((
        Name("bastion/spawn-pad".into()),
        Transform {
            position: Vec3::new(spawn_pos.x + 0.5, 0.05, spawn_pos.y),
            scale: Vec3::new(0.4, 1.0, 2.2),
            ..Transform::IDENTITY
        },
        frame_mesh,
        spawn_pad_mat,
    ));

    // ---- 界外岩柱（视锥剔除的真实载荷：8 根大半落在视锥外）----
    let rock_mesh = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, 10, 6));
    let rock_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.3, 0.31, 0.33, 1.0));
    for (index, (x, z, scale)) in [
        (24.0, 2.0, 2.2),
        (-24.0, 2.0, 2.6),
        (28.0, 8.0, 3.0),
        (-28.0, 8.0, 2.4),
        (20.0, 12.0, 2.8),
        (-20.0, 12.0, 2.2),
        (8.0, 33.0, 3.2),
        (-8.0, 33.0, 2.8),
    ]
    .into_iter()
    .enumerate()
    {
        world.spawn((
            Name(format!("bastion/rock/{index}")),
            Transform {
                position: Vec3::new(x, 0.4, z),
                scale: Vec3::new(scale, scale * 0.7, scale),
                ..Transform::IDENTITY
            },
            rock_mesh,
            rock_mat,
        ));
    }

    // ---- 俯视固定相机：望向场地中心（见 git blame 旁的推导注释）----
    // 位姿 (0, 25, 36)、俯仰 -35°、fov 22°：16:9 下场地四角全部入画且
    // 占画面宽度 ~70%（近角水平半张角 19° ≈ 水平半 fov）——塔 / 敌单格
    // 约 22 px（640×360 下仍可辨）。浅俯仰给 3/4 体感，窄 fov 保像素密度。
    let camera = world.spawn((
        Name("bastion/camera".into()),
        Transform {
            position: Vec3::new(0.0, 25.0, 36.0),
            rotation: Quat::from_rotation_x(-35.0_f32.to_radians()),
            scale: Vec3::ONE,
        },
        ActiveCamera {
            fov_y_radians: 22.0_f32.to_radians(),
            ..ActiveCamera::default()
        },
    ));

    // ---- 幽灵预览三件套（Blend 材质 = R23 dogfooding）----
    let ghost_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let ring_mesh = world
        .resource_mut::<MeshStore>()
        .add(MeshData::cylinder(1.0, 0.04, 48));
    let mut materials = world.resource_mut::<MaterialStore>();
    let ok_mat = materials.add(MaterialDef::glass(0.3, 0.6, 1.0, 0.4));
    let blocked_mat = materials.add(MaterialDef::glass(1.0, 0.25, 0.2, 0.4));
    let ring_mat = materials.add(MaterialDef::glass(0.55, 0.8, 1.0, 0.12));
    drop(materials);
    let ghost_root = world.spawn((
        Name("bastion/ghost".into()),
        Transform::IDENTITY,
        Visibility::HIDDEN,
    ));
    let ghost_sentry_body = world.spawn((
        Name("bastion/ghost/sentry".into()),
        Transform {
            position: Vec3::new(0.0, 0.4, 0.0),
            scale: Vec3::new(0.55, 0.8, 0.55),
            ..Transform::IDENTITY
        },
        ghost_mesh,
        ok_mat,
        Visibility::HIDDEN,
    ));
    let ghost_mortar_body = world.spawn((
        Name("bastion/ghost/mortar".into()),
        Transform {
            position: Vec3::new(0.0, 0.25, 0.0),
            scale: Vec3::new(0.75, 0.5, 0.75),
            ..Transform::IDENTITY
        },
        ghost_mesh,
        ok_mat,
        Visibility::HIDDEN,
    ));
    let ghost_ring = world.spawn((
        Name("bastion/ghost/ring".into()),
        Transform {
            position: Vec3::new(0.0, 0.06, 0.0),
            rotation: Quat::IDENTITY,
            scale: Vec3::new(SENTRY_RANGE, 1.0, SENTRY_RANGE),
        },
        ring_mesh,
        ring_mat,
        // 射程盘不挂 Visibility：ghost_root 隐藏时经**层级级联**一并隐藏
        // （打磨 III 前的逐实体过滤语义下「组件缺席 = 恒渲染」——幽灵
        // 待命时射程盘滞留在最后悬停格的缺陷由级联顺带修复）
    ));
    world.add_child(ghost_root, ghost_sentry_body);
    world.add_child(ghost_root, ghost_mortar_body);
    world.add_child(ghost_root, ghost_ring);
    world.insert_resource(GhostMaterials {
        ok: ok_mat,
        blocked: blocked_mat,
        ring: ring_mat,
    });

    // ---- 塔池：40 槽位，每槽「根 → 形态 rig → 网格」三层。根可见性 =
    // 塔在位与否；形态 rig 可见性 = 本塔当前形态；孙辈网格不挂
    // Visibility（组件缺席 = 可见）。**层级级联**（打磨 III：渲染收集期
    // 沿父链上行，任一祖先隐藏 ⇒ 子树全隐藏）保证根隐藏时待命塔的身管
    // 不入画——dogfooding III 时级联缺失，夹层 rig 翻不动孙子网格
    // （draw calls 139 事故），被迫拍平成「网格直接挂根 + 名字识别形态」；
    // 现引擎闭合后还原夹层 rig，形态切换回到「翻两个 rig」，名字识别
    // 与逐网格翻 Visibility 都不再需要 ----
    let mut tower_slots = Vec::new();
    let mut tower_forms = Vec::new();
    for slot in 0..POOL_TOWERS {
        let root = world.spawn((
            Name(format!("bastion/tower-pool/{slot}")),
            Transform::IDENTITY,
            Visibility::HIDDEN,
        ));
        // 哨兵形态 rig：窄身高塔 + 前伸炮管
        let sentry_rig = world.spawn((
            Name(format!("bastion/tower-pool/{slot}/sentry-rig")),
            Transform::IDENTITY,
            Visibility::HIDDEN,
        ));
        let sentry_mat = tower_sentry_material(world);
        let sentry_body = world.spawn((
            Name(format!("bastion/tower-pool/{slot}/sentry-body")),
            Transform {
                position: Vec3::new(0.0, 0.45, 0.0),
                scale: Vec3::new(0.5, 0.9, 0.5),
                ..Transform::IDENTITY
            },
            ghost_mesh,
            sentry_mat,
        ));
        world.add_child(sentry_rig, sentry_body);
        let barrel = world.spawn((
            Name(format!("bastion/tower-pool/{slot}/sentry-barrel")),
            Transform {
                position: Vec3::new(0.0, 0.62, -0.4),
                scale: Vec3::new(0.14, 0.14, 0.6),
                ..Transform::IDENTITY
            },
            ghost_mesh,
            sentry_mat,
        ));
        world.add_child(sentry_rig, barrel);
        // 臼炮形态 rig：宽矮底座 + 斜置炮筒
        let mortar_rig = world.spawn((
            Name(format!("bastion/tower-pool/{slot}/mortar-rig")),
            Transform::IDENTITY,
            Visibility::HIDDEN,
        ));
        let mortar_mat = tower_mortar_material(world);
        let base = world.spawn((
            Name(format!("bastion/tower-pool/{slot}/mortar-base")),
            Transform {
                position: Vec3::new(0.0, 0.2, 0.0),
                scale: Vec3::new(0.75, 0.4, 0.75),
                ..Transform::IDENTITY
            },
            ghost_mesh,
            mortar_mat,
        ));
        world.add_child(mortar_rig, base);
        let tube = world.spawn((
            Name(format!("bastion/tower-pool/{slot}/mortar-tube")),
            Transform {
                position: Vec3::new(0.0, 0.45, 0.0),
                rotation: Quat::from_axis_angle(Vec3::X, -50.0_f32.to_radians()),
                scale: Vec3::new(0.4, 0.7, 0.4),
            },
            ghost_mesh,
            mortar_mat,
        ));
        world.add_child(mortar_rig, tube);
        world.add_child(root, sentry_rig);
        world.add_child(root, mortar_rig);
        tower_slots.push(root);
        tower_forms.push([sentry_rig, mortar_rig]);
    }
    world.insert_resource(TowerPool {
        slots: tower_slots,
        forms: tower_forms,
    });

    // ---- 敌人池：24 槽位，每槽「根 → 形态 rig → 网格」三层（与塔池
    // 同款：rig 可见性 = 本敌人形态，孙辈网格不挂 Visibility）----
    let mut enemy_slots = Vec::new();
    let mut enemy_forms = Vec::new();
    for slot in 0..POOL_ENEMIES {
        let root = world.spawn((
            Name(format!("bastion/enemy-pool/{slot}")),
            Transform::IDENTITY,
            Visibility::HIDDEN,
        ));
        let runner_rig = world.spawn((
            Name(format!("bastion/enemy-pool/{slot}/runner-rig")),
            Transform::IDENTITY,
            Visibility::HIDDEN,
        ));
        let runner_mat = runner_material(world);
        let runner_body = world.spawn((
            Name(format!("bastion/enemy-pool/{slot}/runner-body")),
            Transform {
                position: Vec3::new(0.0, 0.2, 0.0),
                scale: Vec3::new(0.62, 0.36, 0.4),
                ..Transform::IDENTITY
            },
            ghost_mesh,
            runner_mat,
        ));
        world.add_child(runner_rig, runner_body);
        let brute_rig = world.spawn((
            Name(format!("bastion/enemy-pool/{slot}/brute-rig")),
            Transform::IDENTITY,
            Visibility::HIDDEN,
        ));
        let brute_mat = brute_material(world);
        let brute_body = world.spawn((
            Name(format!("bastion/enemy-pool/{slot}/brute-body")),
            Transform {
                position: Vec3::new(0.0, 0.33, 0.0),
                scale: Vec3::new(0.92, 0.66, 0.92),
                ..Transform::IDENTITY
            },
            ghost_mesh,
            brute_mat,
        ));
        world.add_child(brute_rig, brute_body);
        world.add_child(root, runner_rig);
        world.add_child(root, brute_rig);
        enemy_slots.push(root);
        enemy_forms.push([runner_rig, brute_rig]);
    }
    world.insert_resource(EnemyPool {
        slots: MappedPool::new(enemy_slots),
        forms: enemy_forms,
    });

    // ---- 哨兵弹池：自发光小球 × 32 ----
    let bullet_mesh = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(0.09, 8, 5));
    let bullet_mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [1.0, 0.9, 0.4, 1.0],
        emissive: [2.5, 2.0, 0.8, 1.0],
        metallic: 0.0,
        roughness: 0.4,
        ..MaterialDef::default()
    });
    let mut bullet_slots = Vec::new();
    for slot in 0..POOL_BULLETS {
        let entity = world.spawn((
            Name(format!("bastion/bullet-pool/{slot}")),
            Transform::from_xyz(0.0, BULLET_Y, 0.0),
            bullet_mesh,
            bullet_mat,
            Visibility::HIDDEN,
        ));
        bullet_slots.push(entity);
    }
    world.insert_resource(BulletPool(MappedPool::new(bullet_slots)));

    // ---- 臼炮弹池：自发光大球 × 8 ----
    let shell_mesh = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(0.16, 10, 6));
    let shell_mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [1.0, 0.5, 0.2, 1.0],
        emissive: [2.2, 0.9, 0.3, 1.0],
        metallic: 0.0,
        roughness: 0.5,
        ..MaterialDef::default()
    });
    let mut shell_slots = Vec::new();
    for slot in 0..POOL_SHELLS {
        let entity = world.spawn((
            Name(format!("bastion/shell-pool/{slot}")),
            Transform::IDENTITY,
            shell_mesh,
            shell_mat,
            Visibility::HIDDEN,
        ));
        shell_slots.push(entity);
    }
    world.insert_resource(ShellPool(MappedPool::new(shell_slots)));

    // ---- FX 池：12 个纯发射器槽位（无网格）。槽位**预挂一个禁用发射器**
    //（零速率零 burst，永不产粒子）作为组件载体——Update 系统拿 &World，
    // 组件只能经 get_mut 换值、不能 insert 新组件类型，故待命态必须预挂；
    // 触发 = 参数全字段覆写 + `reset()` 重新上膛（对象池槽位复用的正道，
    // 打磨 III 起「换挂整只新组件」的绕行已删）----
    let mut fx_slots = Vec::new();
    for slot in 0..POOL_FX {
        let entity = world.spawn((
            Name(format!("bastion/fx-pool/{slot}")),
            Transform::IDENTITY,
            disabled_emitter(),
        ));
        fx_slots.push(entity);
    }
    world.insert_resource(FxPool {
        slots: fx_slots,
        next: 0,
    });

    // ---- HUD（屏幕像素坐标，文本实体的 UiRect 仅作定位载体）----
    let hud = |world: &mut World, name: &str, position: [f32; 2], text: UiText, color: [f32; 4]| {
        world.spawn((
            Name(name.into()),
            UiRect {
                position,
                size: [0.0, 0.0],
            },
            text,
            UiColor(color),
        ));
    };
    hud(
        world,
        CREDITS_TEXT_NAME,
        [24.0, 16.0],
        UiText {
            string: format!("CREDITS {START_CREDITS}"),
            scale: 3.0,
        },
        [1.0, 0.85, 0.3, 1.0],
    );
    hud(
        world,
        WAVE_TEXT_NAME,
        [24.0, 52.0],
        UiText {
            string: format!("WAVE 1/{MAX_WAVES}"),
            scale: 3.0,
        },
        [0.85, 0.9, 1.0, 1.0],
    );
    hud(
        world,
        CORE_TEXT_NAME,
        [24.0, 88.0],
        UiText {
            string: format!("CORE HP {CORE_MAX_HP}/{CORE_MAX_HP}"),
            scale: 3.0,
        },
        [0.55, 1.0, 0.55, 1.0],
    );
    hud(
        world,
        BUILD_TEXT_NAME,
        [24.0, 124.0],
        UiText {
            string: format!("[1] SENTRY {SENTRY_COST}  [2] MORTAR {MORTAR_COST}  > SENTRY"),
            scale: 2.0,
        },
        [0.9, 0.9, 0.9, 1.0],
    );
    let banner = "";
    hud(
        world,
        BANNER_TEXT_NAME,
        [
            (1280.0
                - text_width(&UiText {
                    string: banner.into(),
                    scale: 4.0,
                }))
                * 0.5,
            300.0,
        ],
        UiText {
            string: banner.into(),
            scale: 4.0,
        },
        [1.0, 0.85, 0.3, 1.0],
    );

    world.insert_resource(BastionEntities {
        camera,
        ghost_root,
        ghost_sentry_body,
        ghost_mortar_body,
        ghost_ring,
        core,
    });
}

// 材质小助手（独立函数而非闭包：&mut World 的独占借用分开结算）
fn tower_sentry_material(world: &mut World) -> MaterialHandle {
    world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.2, 0.42, 0.85, 1.0))
}
fn tower_mortar_material(world: &mut World) -> MaterialHandle {
    world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.75, 0.5, 0.25, 1.0))
}
fn runner_material(world: &mut World) -> MaterialHandle {
    world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [1.0, 0.42, 0.1, 1.0],
        emissive: [0.45, 0.1, 0.0, 1.0],
        metallic: 0.0,
        roughness: 0.6,
        ..MaterialDef::default()
    })
}
fn brute_material(world: &mut World) -> MaterialHandle {
    world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.14, 0.16, 0.2, 1.0))
}

/// Update 1：读输入与时间 → 鼠标拾取 → 推进纯逻辑 → 事件触发空间音频。
///
/// - 拾取：光标像素（[`Input::cursor_position`]，物理像素）→
///   [`pick_ground_cell`]（引擎 `raycast` 打地面静态 collider）→ 网格；
/// - 左键边沿 = 放塔请求（当前塔种）；数字键 1 / 2 = 选塔种，右键 = 切换；
/// - 音频：[`BastionEvent`] 自带世界坐标——同类型事件按帧去重（取最后
///   一件的位置），每类每帧至多一响（混音器软限幅前的量控）；
/// - 听者：每帧取场景相机的全局位姿（俯视机位、悬于场地上方，pan 由 XZ
///   方位决定），直接 `set_listener` 而非挂 `SpatialListener` 组件——
///   headless / 离屏快照路径（未注册 SpatialPlugin）同样得到正确听者
///   （iris-arena 同款取舍）。
fn run_bastion(world: &World) {
    let dt = world
        .try_resource::<Time>()
        .map(|time| time.delta_secs())
        .unwrap_or(0.0);

    // ---- 拾取与输入翻译 ----
    let cursor = world
        .try_resource::<Input>()
        .and_then(|input| input.cursor_position());
    let hover_cell = cursor.and_then(|(px, py)| pick_ground_cell(world, px, py));
    let (left_clicked, right_clicked, digit1, digit2) = world
        .try_resource::<Input>()
        .map(|input| {
            (
                input.mouse_button_pressed(MouseButton::Left),
                input.mouse_button_pressed(MouseButton::Right),
                input.key_pressed(KeyCode::Digit1),
                input.key_pressed(KeyCode::Digit2),
            )
        })
        .unwrap_or((false, false, false, false));

    let Some(mut state) = world.try_resource_mut::<BastionState>() else {
        return;
    };
    let selected = world
        .try_resource::<GhostKind>()
        .map(|kind| kind.0)
        .unwrap_or(TowerKind::Sentry);

    // ---- 塔种切换：数字键 1 / 2 直接选中，右键在两塔种间来回 ----
    let next = if digit1 {
        TowerKind::Sentry
    } else if digit2 {
        TowerKind::Mortar
    } else if right_clicked {
        match selected {
            TowerKind::Sentry => TowerKind::Mortar,
            TowerKind::Mortar => TowerKind::Sentry,
        }
    } else {
        selected
    };
    if next != selected {
        if let Some(mut kind) = world.try_resource_mut::<GhostKind>() {
            kind.0 = next;
        }
    }
    let selected = world
        .try_resource::<GhostKind>()
        .map(|kind| kind.0)
        .unwrap_or(selected);

    // ---- 放塔请求：光标在格上 + 左键边沿 + 对局中 ----
    let mut input = BastionInput {
        dt,
        ..BastionInput::default()
    };
    if state.phase == Phase::Playing && left_clicked {
        if let Some(cell) = hover_cell {
            input.place_requests.push((cell, selected));
        }
    }
    state.step(&input);

    // ---- 本帧拾取结果落资源（表现层红蓝着色用）----
    if let Some(mut hover) = world.try_resource_mut::<HoverCell>() {
        hover.cell = hover_cell;
        hover.placeable = hover_cell
            .map(|cell| state.check_placement(cell, selected).is_ok())
            .unwrap_or(false);
    }

    // ---- 空间听者：每帧取场景相机的全局位姿（俯视机位、悬于场地上方）。
    // GlobalTransform 来自上一帧的层级传播（滞后一帧，对音频无感）；首帧
    // 传播未跑时优雅跳过 ----
    if let (Some(ents), Some(audio)) = (
        world.try_resource::<BastionEntities>(),
        world.try_resource::<Audio>(),
    ) {
        if let Some(global) = world.get::<GlobalTransform>(ents.camera) {
            let matrix = global.matrix();
            let forward = -Vec3::new(matrix.z_axis.x, matrix.z_axis.y, matrix.z_axis.z);
            audio.set_listener(ListenerParams::from_view(
                global.position(),
                forward,
                Vec3::Y,
            ));
        }

        // ---- 事件 → 音效：同类型去重（取最后一件的位置），每类每帧至多
        // 一响。事件按结算顺序到达，「最后一件」即最接近当前时刻的位置----
        let mut shot_at: Option<Vec3> = None;
        let mut thump_at: Option<Vec3> = None;
        let mut hit_at: Option<Vec3> = None;
        let mut boom_at: Option<Vec3> = None;
        let mut death_at: Option<Vec3> = None;
        let mut leak_at: Option<Vec3> = None;
        for event in &state.frame_events {
            let at = |pos: Vec2| Vec3::new(pos.x, 0.0, pos.y);
            match *event {
                BastionEvent::SentryShot { pos } => shot_at = Some(at(pos)),
                BastionEvent::MortarShot { pos } => thump_at = Some(at(pos)),
                BastionEvent::Hit { pos } => hit_at = Some(at(pos)),
                BastionEvent::Boom { pos } => boom_at = Some(at(pos)),
                BastionEvent::Death { pos } => death_at = Some(at(pos)),
                BastionEvent::Leak { pos } => leak_at = Some(at(pos)),
            }
        }
        let assets = world.try_resource::<BastionAssets>();
        if let (Some(assets), Some(audio)) = (assets, world.try_resource::<Audio>()) {
            let spatial = |clip: AudioClip, at: Option<Vec3>, volume: f32| {
                if let Some(position) = at {
                    audio.play_spatial(clip, position, volume, false);
                }
            };
            spatial(assets.shoot.clone(), shot_at, 0.16);
            spatial(assets.thump.clone(), thump_at, 0.35);
            spatial(assets.hit.clone(), hit_at, 0.12);
            spatial(assets.boom.clone(), boom_at, 0.3);
            spatial(assets.death.clone(), death_at, 0.25);
            // 漏怪是「核心受创」的全局告警：定位到核心水晶（事件坐标本就
            // 是核心位置，空间化播放即得方位线索）
            spatial(assets.alarm.clone(), leak_at, 0.5);
        }
    }
}

// ---------------------------------------------------------------------------
// 粒子（R24 dogfooding）
// ---------------------------------------------------------------------------

/// 禁用发射器（FX 槽位的待命态）：零速率、零 burst、永久——永不产粒子，
/// 仅占位让槽位组件齐备（触发时经 [`configure_event_emitter`] + `reset`
/// 就地复用）。
fn disabled_emitter() -> ParticleEmitter {
    let mut e = ParticleEmitter::new(ParticleBlend::Additive);
    e.spawn_rate = 0.0;
    e.burst_count = 0;
    e
}

// 六类事件的 FX 参数（全字段写入函数，作用于槽位既有发射器组件）：
// 槽位长期存活、字段会残留上一发配置，故每类**从默认基线逐字段全量
// 覆写**——与「换挂新组件」逐位等价，但组件对象不再销毁重建。

/// 枪口闪光：小型暖黄 burst（additive，触发泛光）。
fn configure_muzzle_flash(e: &mut ParticleEmitter) {
    e.blend = ParticleBlend::Additive;
    e.spawn_rate = 0.0;
    e.burst_count = 10;
    e.emitter_lifetime = 0.0;
    e.particle_lifetime = 0.16;
    e.initial_speed = 3.5;
    e.velocity_jitter = 1.6;
    e.gravity_scale = 0.2;
    e.size_start = 0.14;
    e.size_end = 0.02;
    e.color_start = [3.0, 2.2, 0.6, 1.0];
    e.color_end = [1.5, 0.6, 0.1, 0.0];
    e.max_particles = 32;
}

/// 命中火花：冷白短 burst。
fn configure_spark_burst(e: &mut ParticleEmitter) {
    e.blend = ParticleBlend::Additive;
    e.spawn_rate = 0.0;
    e.burst_count = 8;
    e.emitter_lifetime = 0.0;
    e.particle_lifetime = 0.22;
    e.initial_speed = 4.5;
    e.velocity_jitter = 2.0;
    e.gravity_scale = 0.4;
    e.size_start = 0.1;
    e.size_end = 0.02;
    e.color_start = [2.5, 2.8, 3.2, 1.0];
    e.color_end = [0.6, 0.9, 1.6, 0.0];
    e.max_particles = 32;
}

/// 臼炮发射烟尘：alpha 混合的灰烟（慢速上升）。
fn configure_mortar_smoke(e: &mut ParticleEmitter) {
    e.blend = ParticleBlend::Alpha;
    e.spawn_rate = 0.0;
    e.burst_count = 14;
    e.emitter_lifetime = 0.0;
    e.particle_lifetime = 0.7;
    e.initial_speed = 2.0;
    e.velocity_jitter = 1.2;
    e.gravity_scale = -0.05;
    e.size_start = 0.3;
    e.size_end = 0.55;
    e.color_start = [0.55, 0.5, 0.45, 0.7];
    e.color_end = [0.4, 0.38, 0.36, 0.0];
    e.max_particles = 64;
}

/// 落点爆炸：爆炸预设（explosion 预设）收紧到 AOE 尺度。
fn configure_explosion_small(e: &mut ParticleEmitter) {
    e.blend = ParticleBlend::Additive;
    e.spawn_rate = 0.0;
    e.burst_count = 120;
    e.emitter_lifetime = 0.0;
    e.particle_lifetime = 0.9;
    e.initial_speed = 5.5;
    e.velocity_jitter = 2.6;
    e.gravity_scale = 0.55;
    e.size_start = 0.24;
    e.size_end = 0.02;
    e.color_start = [3.2, 1.6, 0.5, 1.0];
    e.color_end = [1.5, 0.12, 0.04, 0.0];
    e.max_particles = 256;
}

/// 敌人死亡：爆炸预设原样（全向 320 粒、HDR 暖色拖尾）。
fn configure_death_explosion(e: &mut ParticleEmitter) {
    e.blend = ParticleBlend::Additive;
    e.spawn_rate = 0.0;
    e.burst_count = 320;
    e.emitter_lifetime = 0.0;
    e.particle_lifetime = 1.4;
    e.initial_speed = 7.0;
    e.velocity_jitter = 2.6;
    e.gravity_scale = 0.55;
    e.size_start = 0.30;
    e.size_end = 0.02;
    e.color_start = [3.2, 1.6, 0.5, 1.0];
    e.color_end = [1.5, 0.12, 0.04, 0.0];
    e.max_particles = 1024;
}

/// 漏怪核心告警：红色 burst 从核心迸出。
fn configure_core_breach(e: &mut ParticleEmitter) {
    e.blend = ParticleBlend::Additive;
    e.spawn_rate = 0.0;
    e.burst_count = 64;
    e.emitter_lifetime = 0.0;
    e.particle_lifetime = 0.8;
    e.initial_speed = 4.0;
    e.velocity_jitter = 2.2;
    e.gravity_scale = 0.3;
    e.size_start = 0.2;
    e.size_end = 0.03;
    e.color_start = [3.0, 0.4, 0.25, 1.0];
    e.color_end = [1.2, 0.05, 0.02, 0.0];
    e.max_particles = 128;
}

/// 事件 → 粒子出生高度（世界 XZ 取事件坐标，各事件按视觉合适的高度
/// 抬升）+ 参数写入函数（六类事件各一档）。
fn event_origin(event: BastionEvent) -> (f32, f32, f32) {
    match event {
        BastionEvent::SentryShot { pos } => (pos.x, 0.7, pos.y),
        BastionEvent::MortarShot { pos } => (pos.x, 0.5, pos.y),
        BastionEvent::Hit { pos } => (pos.x, 0.5, pos.y),
        BastionEvent::Boom { pos } => (pos.x, 0.25, pos.y),
        BastionEvent::Death { pos } => (pos.x, 0.3, pos.y),
        BastionEvent::Leak { pos } => (pos.x, 0.8, pos.y),
    }
}

/// 按事件类型把 FX 参数全字段写入槽位发射器（六类事件各一档，见上方
/// 各 configure_* 函数）。
fn configure_event_emitter(emitter: &mut ParticleEmitter, event: BastionEvent) {
    match event {
        BastionEvent::SentryShot { .. } => configure_muzzle_flash(emitter),
        BastionEvent::MortarShot { .. } => configure_mortar_smoke(emitter),
        BastionEvent::Hit { .. } => configure_spark_burst(emitter),
        BastionEvent::Boom { .. } => configure_explosion_small(emitter),
        BastionEvent::Death { .. } => configure_death_explosion(emitter),
        BastionEvent::Leak { .. } => configure_core_breach(emitter),
    }
}

/// Update 2：事件 → 粒子。发射器挂 FX 池槽位实体按需触发：槽位 Transform
/// 挪到事件坐标，发射器组件**就地复用**——参数全字段覆写
///（[`configure_event_emitter`]）+ [`ParticleEmitter::reset`] 清上一发
/// 模拟态并让 burst 重新上膛（打磨 III 的新 API；「换挂整只新组件」的
/// 绕行已删）。同帧事件多于槽位时环形覆盖（先前 burst 已在飞的粒子
/// 不受影响——粒子出生即世界坐标，与发射器解耦）。
fn trigger_bastion_fx(world: &World) {
    let Some(state) = world.try_resource::<BastionState>() else {
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
        let (x, y, z) = event_origin(*event);
        if let Some(mut transform) = world.get_mut::<Transform>(entity) {
            transform.position = Vec3::new(x, y, z);
        }
        // 槽位发射器就地复用：参数覆写 + reset（burst 重新上膛——
        // burst_fired 标记归位，下一帧 update_particles 射出整批）
        if let Some(mut slot) = world.get_mut::<ParticleEmitter>(entity) {
            let emitter = &mut *slot;
            configure_event_emitter(emitter, *event);
            emitter.reset();
        }
    }
}

// ---------------------------------------------------------------------------
// 表现（四池 + 幽灵预览）
// ---------------------------------------------------------------------------

/// Update 3：逻辑状态 → 表现。幽灵预览（位置 / 红蓝 / 射程盘）、塔池
/// （下标直映射 + 双形态可见性）、敌池 / 双弹池（稳定 id 映射，先归还
/// 后取用——同帧「旧波退场 + 新波登场」不会饿死，iris-arena 同款）。
fn update_bastion_visuals(world: &World) {
    let Some(state) = world.try_resource::<BastionState>() else {
        return;
    };
    let Some(ents) = world.try_resource::<BastionEntities>() else {
        return;
    };
    let selected = world
        .try_resource::<GhostKind>()
        .map(|kind| kind.0)
        .unwrap_or(TowerKind::Sentry);
    let hover = world
        .try_resource::<HoverCell>()
        .map(|hover| (hover.cell, hover.placeable))
        .unwrap_or((None, false));

    // ---- 幽灵预览：光标在场 + 对局中可见；红蓝按校验；射程盘按塔种缩放 ----
    let (hover_cell, placeable) = hover;
    let ghost_visible = state.phase == Phase::Playing && hover_cell.is_some();
    if let Some(mut visibility) = world.get_mut::<Visibility>(ents.ghost_root) {
        *visibility = if ghost_visible {
            Visibility::VISIBLE
        } else {
            Visibility::HIDDEN
        };
    }
    if let Some(cell) = hover_cell {
        let center = cell_center(cell);
        if let Some(mut transform) = world.get_mut::<Transform>(ents.ghost_root) {
            transform.position = Vec3::new(center.x, 0.0, center.y);
        }
        let (active_body, range) = match selected {
            TowerKind::Sentry => (ents.ghost_sentry_body, SENTRY_RANGE),
            TowerKind::Mortar => (ents.ghost_mortar_body, MORTAR_RANGE),
        };
        for body in [ents.ghost_sentry_body, ents.ghost_mortar_body] {
            if let Some(mut visibility) = world.get_mut::<Visibility>(body) {
                *visibility = if body == active_body {
                    Visibility::VISIBLE
                } else {
                    Visibility::HIDDEN
                };
            }
        }
        // R23 dogfooding：Blend 材质红蓝换挂（可放 = 蓝 / 不可放 = 红）
        if let Some(materials) = world.try_resource::<GhostMaterials>() {
            let wanted = if placeable {
                materials.ok
            } else {
                materials.blocked
            };
            if let Some(mut handle) = world.get_mut::<MaterialHandle>(active_body) {
                *handle = wanted;
            }
        }
        if let Some(mut transform) = world.get_mut::<Transform>(ents.ghost_ring) {
            transform.scale = Vec3::new(range, 1.0, range);
        }
    }

    // ---- 塔池：下标直映射（塔从不移除）。根可见性 = 塔在位；形态切换
    // = 翻两个夹层 rig 的可见性（**层级级联**带动孙辈网格整组切换，
    // 打磨 III 起无需逐网格翻 / 名字识别）----
    if let Some(pool) = world.try_resource::<TowerPool>() {
        for (index, tower) in state.towers.iter().enumerate() {
            let Some(&root) = pool.slots.get(index) else {
                break; // 池耗尽（逻辑封顶应先于此发生，防御性跳过）
            };
            let Some(&[sentry_rig, mortar_rig]) = pool.forms.get(index) else {
                break;
            };
            let center = cell_center(tower.cell);
            if let Some(mut transform) = world.get_mut::<Transform>(root) {
                transform.position = Vec3::new(center.x, 0.0, center.y);
            }
            if let Some(mut visibility) = world.get_mut::<Visibility>(root) {
                *visibility = Visibility::VISIBLE;
            }
            let (active_rig, dormant_rig) = match tower.kind {
                TowerKind::Sentry => (sentry_rig, mortar_rig),
                TowerKind::Mortar => (mortar_rig, sentry_rig),
            };
            if let Some(mut visibility) = world.get_mut::<Visibility>(active_rig) {
                *visibility = Visibility::VISIBLE;
            }
            if let Some(mut visibility) = world.get_mut::<Visibility>(dormant_rig) {
                *visibility = Visibility::HIDDEN;
            }
        }
    }

    // ---- 敌人池：先归还（退场槽位翻回隐藏），再取用（arena 同款顺序）----
    let mut pool = match world.try_resource_mut::<EnemyPool>() {
        Some(pool) => pool,
        None => return,
    };
    let mut released: Vec<Entity> = Vec::new();
    pool.slots.active.retain(|(id, entity)| {
        let alive = state.enemies.iter().any(|enemy| enemy.id == *id);
        if !alive {
            released.push(*entity);
        }
        alive
    });
    for entity in released {
        if let Some(mut visibility) = world.get_mut::<Visibility>(entity) {
            *visibility = Visibility::HIDDEN;
        }
        pool.slots.free.push(entity);
    }
    for enemy in &state.enemies {
        if pool.slots.entity_of(enemy.id).is_none() {
            pool.slots.acquire(enemy.id);
        }
    }
    let mut enemy_jobs: Vec<(Entity, [Entity; 2], f32, EnemyKind)> = Vec::new();
    for enemy in &state.enemies {
        if let Some(entity) = pool.slots.entity_of(enemy.id) {
            // 槽位序号 = forms 下标（槽位表静态，线性查找 n ≤ 24 可忽略）
            let forms = pool
                .slots
                .slots
                .iter()
                .position(|&s| s == entity)
                .and_then(|index| pool.forms.get(index))
                .copied();
            if let Some(forms) = forms {
                enemy_jobs.push((entity, forms, enemy.progress, enemy.kind));
            }
        }
        // 池耗尽的敌人无槽位可写：静默跳过（逻辑封顶应先于此发生）
    }
    drop(pool);
    for (entity, [runner_rig, brute_rig], progress, kind) in enemy_jobs {
        if let Some(mut visibility) = world.get_mut::<Visibility>(entity) {
            *visibility = Visibility::VISIBLE;
        }
        let Some(mut transform) = world.get_mut::<Transform>(entity) else {
            continue;
        };
        let pos = crate::logic::path_point(progress);
        transform.position = Vec3::new(pos.x, 0.0, pos.y);
        // 朝向 = 路径方向（模型头部 +X → yaw 补偿见 MODEL_YAW_OFFSET 文档）
        let dir = path_dir(progress);
        let yaw = (-dir.y).atan2(dir.x) + MODEL_YAW_OFFSET;
        transform.rotation = Quat::from_rotation_y(yaw);
        // 双形态 rig 按敌种翻可见性（**层级级联**带动孙辈网格整组切换）
        let (active_rig, dormant_rig) = match kind {
            EnemyKind::Runner => (runner_rig, brute_rig),
            EnemyKind::Brute => (brute_rig, runner_rig),
        };
        if let Some(mut visibility) = world.get_mut::<Visibility>(active_rig) {
            *visibility = Visibility::VISIBLE;
        }
        if let Some(mut visibility) = world.get_mut::<Visibility>(dormant_rig) {
            *visibility = Visibility::HIDDEN;
        }
    }

    // ---- 哨兵弹池：同款 id 映射（小球只写位置）----
    let mut pool = match world.try_resource_mut::<BulletPool>() {
        Some(pool) => pool,
        None => return,
    };
    sync_mapped_pool(
        world,
        &mut pool.0,
        &state
            .bullets
            .iter()
            .map(|b| (b.id, b.pos))
            .collect::<Vec<_>>(),
        BULLET_Y,
    );
    drop(pool);

    // ---- 臼炮弹池：抛物线高度按飞行进度内插（4f(1−f) 拱形）----
    let mut pool = match world.try_resource_mut::<ShellPool>() {
        Some(pool) => pool,
        None => return,
    };
    let shells: Vec<(u32, Vec2, f32)> = state
        .shells
        .iter()
        .map(|shell| {
            let f = if shell.flight_total > 1e-6 {
                (1.0 - shell.fuse / shell.flight_total).clamp(0.0, 1.0)
            } else {
                1.0
            };
            (
                shell.id,
                shell.pos,
                0.3 + SHELL_ARC_HEIGHT * 4.0 * f * (1.0 - f),
            )
        })
        .collect();
    sync_shell_pool(world, &mut pool.0, &shells);
    drop(pool);
}

/// 通用池同步（位置类：哨兵弹）：先归还后取用 + 写位置。臼炮弹因带高度
/// 内插单独处理（[`sync_shell_pool`]）。
fn sync_mapped_pool(world: &World, pool: &mut MappedPool, items: &[(u32, Vec2)], y: f32) {
    let mut released: Vec<Entity> = Vec::new();
    pool.active.retain(|(id, entity)| {
        let alive = items.iter().any(|(item_id, _)| item_id == id);
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
    for (id, _) in items {
        if pool.entity_of(*id).is_none() {
            pool.acquire(*id);
        }
    }
    for (id, pos) in items {
        if let Some(entity) = pool.entity_of(*id) {
            if let Some(mut visibility) = world.get_mut::<Visibility>(entity) {
                *visibility = Visibility::VISIBLE;
            }
            if let Some(mut transform) = world.get_mut::<Transform>(entity) {
                transform.position = Vec3::new(pos.x, y, pos.y);
            }
        }
    }
}

/// 臼炮弹池同步：同 [`sync_mapped_pool`]，位置自带抛物线高度。
fn sync_shell_pool(world: &World, pool: &mut MappedPool, shells: &[(u32, Vec2, f32)]) {
    let mut released: Vec<Entity> = Vec::new();
    pool.active.retain(|(id, entity)| {
        let alive = shells.iter().any(|(shell_id, _, _)| shell_id == id);
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
    for (id, _, _) in shells {
        if pool.entity_of(*id).is_none() {
            pool.acquire(*id);
        }
    }
    for (id, pos, height) in shells {
        if let Some(entity) = pool.entity_of(*id) {
            if let Some(mut visibility) = world.get_mut::<Visibility>(entity) {
                *visibility = Visibility::VISIBLE;
            }
            if let Some(mut transform) = world.get_mut::<Transform>(entity) {
                transform.position = Vec3::new(pos.x, *height, pos.y);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// HUD
// ---------------------------------------------------------------------------

/// Update 4：HUD 刷新——积分 / 波次 / 核心 HP / 当前塔种与价格每帧重写，
/// 横幅按阶段切换（波次间歇倒计时 / 胜利 / 失败）。文本实体经 `Name`
/// 按名定位（`entity_named` 为 O(n) 线性扫描，本场景 n ≈ 200 可忽略）。
fn update_bastion_hud(world: &World) {
    let Some(state) = world.try_resource::<BastionState>() else {
        return;
    };
    let selected = world
        .try_resource::<GhostKind>()
        .map(|kind| kind.0)
        .unwrap_or(TowerKind::Sentry);

    if let Some(entity) = world.entity_named(CREDITS_TEXT_NAME) {
        if let Some(mut text) = world.get_mut::<UiText>(entity) {
            text.string = format!("CREDITS {}", state.credits);
        }
    }
    if let Some(entity) = world.entity_named(WAVE_TEXT_NAME) {
        if let Some(mut text) = world.get_mut::<UiText>(entity) {
            text.string = format!("WAVE {}/{MAX_WAVES}", state.wave);
        }
    }
    if let Some(entity) = world.entity_named(CORE_TEXT_NAME) {
        if let Some(mut text) = world.get_mut::<UiText>(entity) {
            text.string = format!("CORE HP {}/{}", state.core_hp.max(0), CORE_MAX_HP);
        }
        if let Some(mut color) = world.get_mut::<UiColor>(entity) {
            color.0 = if state.core_hp <= 3 {
                [1.0, 0.3, 0.25, 1.0]
            } else {
                [0.55, 1.0, 0.55, 1.0]
            };
        }
    }
    if let Some(entity) = world.entity_named(BUILD_TEXT_NAME) {
        if let Some(mut text) = world.get_mut::<UiText>(entity) {
            let selected_name = match selected {
                TowerKind::Sentry => "SENTRY",
                TowerKind::Mortar => "MORTAR",
            };
            text.string =
                format!("[1] SENTRY {SENTRY_COST}  [2] MORTAR {MORTAR_COST}  > {selected_name}");
        }
    }
    if let Some(entity) = world.entity_named(BANNER_TEXT_NAME) {
        let (banner, color): (String, [f32; 4]) = match state.phase {
            Phase::Victory => (
                "VICTORY! THE BASTION HOLDS".to_string(),
                [1.0, 0.85, 0.3, 1.0],
            ),
            Phase::Defeat => (
                "DEFEAT - THE CORE HAS FALLEN".to_string(),
                [1.0, 0.3, 0.25, 1.0],
            ),
            Phase::Playing if state.breather > 0.0 => (
                format!(
                    "WAVE {} IN {}...",
                    state.wave + 1,
                    state.breather.ceil() as i32
                ),
                [0.7, 0.9, 1.0, 1.0],
            ),
            Phase::Playing => (String::new(), [1.0, 0.85, 0.3, 1.0]),
        };
        if let Some(mut text) = world.get_mut::<UiText>(entity) {
            text.string = banner;
            let width = text_width(&text);
            if let Some(mut rect) = world.get_mut::<UiRect>(entity) {
                rect.position = [(1280.0 - width) * 0.5, 150.0];
            }
        }
        if let Some(mut ui_color) = world.get_mut::<UiColor>(entity) {
            ui_color.0 = color;
        }
    }
}

// ---------------------------------------------------------------------------
// 单测（池语义 + 拾取投影闭环）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logic::{CORE_POS, cell_center};

    /// 池语义：取用 / 归还 / 不超发 / 幂等取用（敌人 / 双弹池共用
    /// MappedPool，以单池断言）。
    #[test]
    fn mapped_pool_acquire_release_never_oversubscribes() {
        let mut world = World::new();
        let slots: Vec<Entity> = (0..3).map(|_| world.spawn_empty()).collect();
        let mut pool = EnemyPool {
            slots: MappedPool::new(slots.clone()),
            forms: Vec::new(),
        };
        let pool = &mut pool.slots;
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
        assert!(
            pool.release(99).is_none(),
            "未取用的 id 归还应安全返回 None"
        );
        let _ = (a, c);
    }

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

    /// 拾取投影闭环：格子中心 → 世界坐标 → 屏幕像素 → 射线 → 引擎
    /// raycast 打地面 collider → 同一格子（世界 ↔ 屏幕 ↔ 网格往返一致）。
    #[test]
    fn pick_roundtrip_world_to_screen_to_cell() {
        let mut world = World::new();
        world.insert_resource(Viewport::default());
        // 俯视固定相机（与 build_scene 同位姿）+ 地面静态 collider
        world.spawn((
            Transform {
                position: Vec3::new(0.0, 25.0, 36.0),
                rotation: Quat::from_rotation_x(-35.0_f32.to_radians()),
                scale: Vec3::ONE,
            },
            ActiveCamera {
                fov_y_radians: 22.0_f32.to_radians(),
                ..ActiveCamera::default()
            },
        ));
        world.spawn((
            Transform::from_xyz(0.0, -0.5, 0.0),
            Collider::static_box(Vec3::new(13.0, 0.5, 10.0)),
        ));
        iris_core::hierarchy::propagate_transforms(&mut world);

        for cell in [
            IVec2::new(0, 0),
            IVec2::new(10, 7),
            IVec2::new(19, 13),
            IVec2::new(6, 3),
        ] {
            let center = cell_center(cell);
            let (px, py) = world_to_screen(&world, Vec3::new(center.x, 0.0, center.y))
                .expect("场内点应投影到屏幕");
            let picked = pick_ground_cell(&world, px, py).expect("屏内射线应命中地面");
            assert_eq!(
                picked, cell,
                "格子 {cell} 往返应一致：像素 ({px}, {py}) 拾取到 {picked:?}"
            );
        }
        // 场地之外：界外像素拾取返回 None（射线打不中场地内的格子）
        let outside = world_to_screen(&world, Vec3::new(CORE_POS.x + 30.0, 0.0, CORE_POS.y))
            .expect("界外点仍应可投影");
        assert!(
            pick_ground_cell(&world, outside.0, outside.1).is_none(),
            "界外射线不应返回格子"
        );
    }
}
