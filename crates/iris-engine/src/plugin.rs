//! 插件系统：以可组合的方式向 `App` 注入资源与配置。

use iris_core::prelude::Time;
use iris_platform::input::Input;
use iris_platform::window::WindowConfig;
use iris_render::prelude::{Camera, DirectionalLight, MaterialStore, MeshStore, TextureStore};

use crate::app::App;
use crate::audio::Audio;
use crate::ui::UiPlugin;

/// 插件：一次 `build` 机会，可修改世界的资源、系统与窗口配置。
pub trait Plugin {
    fn build(&self, app: &mut App);
}

/// 视口尺寸（物理像素，与 [`Input::cursor_position`] 同口径）。
///
/// **窗口程序由框架维护，不要手动写**：[`DefaultPlugins`]（经
/// [`WindowPlugin`]）以窗口初始尺寸首次插入，此后每次
/// `WindowEvent::Resized` 由主循环自动同步（照 `Input` 资源的模式）——
/// 用户系统只读即可（典型消费：屏幕射线拾取的宽高比输入，见
/// `iris_render::camera::screen_ray_from_global`）。
///
/// **离屏 / headless 程序**没有窗口事件，可（也应）手动
/// `insert_resource(Viewport::new(w, h))` 写入实际渲染目标尺寸；未插入
/// 时的读取方兜底口径为 `Viewport::default()`（1280×720，与缺省窗口
/// 一致）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewport {
    /// 宽（物理像素）。
    pub width: u32,
    /// 高（物理像素）。
    pub height: u32,
}

impl Viewport {
    /// 以显式宽高构造（写入方保证 ≥ 1，消费方按宽高比使用）。
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

impl Default for Viewport {
    fn default() -> Self {
        Self::new(1280, 720)
    }
}

impl iris_core::resource::Resource for Viewport {}

/// 窗口配置插件。同时以窗口初始尺寸**首次插入** [`Viewport`] 资源
/// （此后 `Resized` 事件由主循环同步；预插入的同型资源会被本插件以
/// 窗口配置覆写——窗口程序以窗口事实为准）。
#[derive(Clone, Debug)]
pub struct WindowPlugin {
    pub config: WindowConfig,
}

impl Plugin for WindowPlugin {
    fn build(&self, app: &mut App) {
        app.window_config = self.config.clone();
        app.world
            .insert_resource(Viewport::new(self.config.width, self.config.height));
    }
}

/// 时间资源插件。
#[derive(Default)]
pub struct TimePlugin;

impl Plugin for TimePlugin {
    fn build(&self, app: &mut App) {
        app.world.insert_resource(Time::default());
    }
}

/// 输入资源插件。
#[derive(Default)]
pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut App) {
        app.world.insert_resource(Input::new());
    }
}

/// 渲染资源插件：注册网格 / 材质 / 纹理目录、默认相机与灯光。
#[derive(Default)]
pub struct RenderPlugin;

impl Plugin for RenderPlugin {
    fn build(&self, app: &mut App) {
        app.world.insert_resource(MeshStore::new());
        app.world.insert_resource(MaterialStore::new());
        app.world.insert_resource(TextureStore::new());
        app.world.insert_resource(Camera::default());
        app.world.insert_resource(DirectionalLight::default());
    }
}

/// 音频插件：注册 [`Audio`] 资源（优先接真实输出设备，无设备 / wasm32
/// 自动降级为纯混音器，见 `crate::audio`）。
#[derive(Default)]
pub struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn build(&self, app: &mut App) {
        app.world.insert_resource(Audio::new());
    }
}

/// 层级插件：每帧在 **PostUpdate** 阶段执行全局变换传播。
///
/// 编排语义：用户 Update 系统写局部 `Transform` → PostUpdate 传播 →
/// 渲染读取 `GlobalTransform`——用户系统写入的变换当帧即完成全局传播，
/// 无须关心本插件与自己的注册顺序（此前传播在 Update 内、先于一切用户
/// 系统，后写的变换要滞后一帧，下游插件被迫在系统链末尾补一次幂等传播
/// 绕行）。已包含在 [`DefaultPlugins`] 中；自建 App（不用 DefaultPlugins）
/// 时需要手动添加，否则渲染器读取的 `GlobalTransform` 不会被更新。
#[derive(Default)]
pub struct HierarchyPlugin;

impl Plugin for HierarchyPlugin {
    fn build(&self, app: &mut App) {
        app.schedule.add_system(
            iris_core::prelude::Stage::PostUpdate,
            iris_core::hierarchy::propagate_transforms,
        );
    }
}

/// 动画插件：在 Update 阶段推进所有 [`iris_render::animation::AnimationPlayer`]，
/// 并在同一阶段按序执行足部 IK（v3，`FootIk` 开关）、手部 IK（v4，
/// `HandIk` 开关）、链式 IK v2（R75，`ChainIk`——FABRIK 任意长链）与
/// look-at 瞄准（R75，`LookIk`——链上均分摆动）。
///
/// 已包含在 [`DefaultPlugins`] 中。动画在 Update 写局部变换，随后的
/// PostUpdate（[`HierarchyPlugin`]）当帧即完成全局传播——「动画写入的
/// 变换当帧可见」的语义不变。
///
/// **足部 IK 的注册序（[`iris_render::foot_ik::update_foot_ik`]，紧跟
/// `update_animations`）**：IK 修正的对象是动画刚写出的本帧局部关节姿势
/// （晚于动画才能读到本帧姿势），写回的也是局部 `Transform`（早于层级
/// 传播才能让修正当帧全局生效）。两个系统的访问集在写 Transform /
/// 读写 AnimationPlayer 上重叠 ⇒ 调度器按注册序分批（动画先、IK 后）；
/// 按 `FootIk` 组件开关，未挂组件的世界空转一次查询即过。
///
/// **手部 IK 的注册序（[`iris_render::hand_ik::update_hand_ik`]，紧跟
/// 足部 IK）**：持枪手位是比脚落地更高频的视觉焦点，且两段 IK 作用于
/// 同一份本帧局部姿势依次生效——注册序确定 ⇒ 修正次序确定 ⇒ 结果确定；
/// 层级传播之前写回局部 `Transform`，当帧传播全局生效。目标差异与
/// 不可达门语义见 hand_ik 模块文档。均注册为**并行系统**（只读世界 +
/// 组件写访问走尝试型锁）。
///
/// **链式 / look-at IK 的注册序（R75，[`iris_render::chain_ik`] 的
/// `update_chain_ik` / `update_look_ik`，紧跟手部 IK）**：同段四系统
/// 依次作用于同一份本帧局部姿势，注册序确定 ⇒ 结果确定；look-at 收尾
/// ——瞄准是姿态的最后一笔，读到的位形含全部前序修正。两系统与足 /
/// 手部共用节点映射（`IkRig` / 播放器二选一）与目标语义（`IkTarget`
/// 全局位），语义与诚实边界（近目标残差、无关节角限制）见 chain_ik
/// 模块文档。
#[derive(Default)]
pub struct AnimationPlugin;

impl Plugin for AnimationPlugin {
    fn build(&self, app: &mut App) {
        use iris_core::prelude::{
            Access, ChildOf, Collider, GlobalTransform, RigidBody, Stage, Transform,
        };
        use iris_render::prelude::{
            AnimationPlayer, ChainIk, FootIk, HandIk, IkRig, IkTarget, LookIk,
        };
        use iris_render::visibility::Visibility;
        app.schedule.add_parallel_system(
            Stage::Update,
            Access::new()
                .write::<AnimationPlayer>()
                .write::<Transform>()
                .read::<Time>(),
            iris_render::animation::update_animations,
        );
        // 足部 IK（v3）：动画之后、层级传播之前（顺序理由见插件文档与
        // foot_ik 模块文档「编排」）。写 FootIk：骨长基准捕获 / scale
        // 污染告警状态（打磨 V）
        app.schedule.add_parallel_system(
            Stage::Update,
            Access::new()
                .write::<Transform>()
                .write::<FootIk>()
                .read::<AnimationPlayer>()
                .read::<IkRig>()
                .read::<ChildOf>()
                .read::<Collider>()
                .read::<RigidBody>(),
            iris_render::foot_ik::update_foot_ik,
        );
        // 手部 IK（v4）：足部 IK 之后、层级传播之前（顺序理由见插件文档
        // 与 hand_ik 模块文档「编排」）。组件无可变状态，只读 HandIk /
        // IkTarget / GlobalTransform / Visibility
        app.schedule.add_parallel_system(
            Stage::Update,
            Access::new()
                .write::<Transform>()
                .read::<HandIk>()
                .read::<IkTarget>()
                .read::<IkRig>()
                .read::<AnimationPlayer>()
                .read::<ChildOf>()
                .read::<GlobalTransform>()
                .read::<Visibility>(),
            iris_render::hand_ik::update_hand_ik,
        );
        // 链式 IK v2（R75）：手部 IK 之后、层级传播之前——FABRIK 任意
        // 长链（触手 / 尾巴 / 脊柱）追 IkTarget 目标，与两骨解析解并存
        // 不替换（语义见 chain_ik 模块文档）
        app.schedule.add_parallel_system(
            Stage::Update,
            Access::new()
                .write::<Transform>()
                .read::<ChainIk>()
                .read::<IkTarget>()
                .read::<IkRig>()
                .read::<AnimationPlayer>()
                .read::<ChildOf>()
                .read::<GlobalTransform>()
                .read::<Visibility>(),
            iris_render::chain_ik::update_chain_ik,
        );
        // look-at 瞄准（R75）：链式 IK 之后收尾——瞄准是姿态的最后一笔，
        // 读到的链位形包含全部前序修正（单遍分布份额 S^(w/n)，残差边界
        // 见 chain_ik 模块文档）
        app.schedule.add_parallel_system(
            Stage::Update,
            Access::new()
                .write::<Transform>()
                .read::<LookIk>()
                .read::<IkTarget>()
                .read::<IkRig>()
                .read::<AnimationPlayer>()
                .read::<ChildOf>()
                .read::<GlobalTransform>()
                .read::<Visibility>(),
            iris_render::chain_ik::update_look_ik,
        );
    }
}

/// 演出时间线插件（R47）：注册 [`crate::timeline::TimelinePlayer`] 资源与
/// [`crate::timeline::TimelineEvent`] 事件（游戏侧不注册即得），并把独占
/// 系统 [`crate::timeline::update_timeline`] 注册到 **Update 阶段**
/// （动画之后、物理之前）。
///
/// 编排语义（详见 `crate::timeline` 模块文档）：时间线在 Update 写相机
/// 局部 `Transform` / 对话与黑边的 UI 组件，必须早于 PostUpdate 的层级
/// 传播（实体相机 `GlobalTransform` 当帧新鲜）与渲染收集；注册在动画
/// 之后（演出相机覆写不与动画姿势写入冲突——动画写角色骨架、时间线写
/// 相机，两者访问集不相交但独占系统的屏障语义要求确定位置）；用户系统
/// 在 `DefaultPlugins` 之后注册、因此在时间线之后执行——**演出期间
/// 游戏侧不要在 Update 后段改主相机**（相机跟随应暂停，否则后写的赢），
/// 输入拦截同样由游戏侧自行决定（时间线不自动拦输入）。时间线产出
/// `TimelineEvent` 于 Update：同帧更晚系统与下一帧全程可读，消费方
/// `drain`（事件双代契约，见 `iris_core::event` 模块文档）。
#[derive(Default)]
pub struct TimelinePlugin;

impl Plugin for TimelinePlugin {
    fn build(&self, app: &mut App) {
        use crate::timeline::{TimelineEvent, TimelinePlayer, update_timeline};
        use iris_core::prelude::Stage;
        app.world.insert_resource(TimelinePlayer::default());
        app.world.register_event::<TimelineEvent>();
        app.schedule.add_system(Stage::Update, update_timeline);
    }
}

/// 物理插件：在 Update 阶段推进刚体物理（积分 → 检测 → 响应 → 迭代松弛，
/// 见 `iris_core::physics::step_physics`），并注册接触事件资源
/// （`Events<CollisionStarted>` / `Events<CollisionEnded>` /
/// `ContactTracker`——帧末差分产出 begin/end，游戏侧不注册即得）。
///
/// 已包含在 [`DefaultPlugins`] 中。编排语义与动画一致：物理在 Update 写
/// 局部 `Transform`，随后的 PostUpdate（[`HierarchyPlugin`]）当帧完成全局
/// 传播——「物理结果当帧可见」。注册为**并行系统**：访问集声明
/// 写 Transform / Collider / RigidBody + 读 Time——Collider v1 只读，
/// 属保守声明（保留物理材质演化等扩展位，并把读 Collider 的用户系统
/// 与物理分批，避免读到半求解状态）；与 [`AnimationPlugin`] 的访问集
/// 重叠（都写 Transform、读 Time），调度器自动分批并保持注册顺序
/// （动画先、物理后）。
/// 水面 ↔ 浮力平面的跨界同步系统（R93）。
///
/// **为什么在 iris-engine**：水面组件在 `iris_render`（渲染侧）、浮力资源在
/// `iris_core`（零渲染依赖），只有引擎编排层同时依赖两者——跨界搬运放这里
/// 是唯一不破依赖方向的落点（裁定 7）。
///
/// **语义**：仅当 `BuoyancyPlane` 资源在场**且** `follow_water == true` 时，
/// 把世界内的水面高度灌进资源的 `height`——海洋优先于池塘（与渲染器的互斥
/// 裁定同序：两路同挂时渲染的是海，物理也得跟着海）；同类的多实体按
/// `select_water_surface`（实体位模式升序取首盏）选，与渲染侧同一确定性键。
///
/// **缺席 = 逐位不变**：资源缺席 / `follow_water == false` ⇒ 整段零写入，
/// 游戏手设的 `height` 永不被引擎改写（默认 `false`，opt-in 才跟随）。
pub fn sync_buoyancy_water_surface(world: &mut iris_core::world::World) {
    use iris_core::buoyancy::BuoyancyPlane;
    // 只读探测（`ResourceRef` 不可写，写之前必须释放该借用）：资源缺席或
    // 显式关闭跟随 ⇒ 零触碰（两条缺席路径在这里合并）。
    let follow = world
        .try_resource::<BuoyancyPlane>()
        .is_some_and(|p| p.follow_water);
    if !follow {
        return;
    }
    let mut best_ocean: Option<(u64, f32)> = None;
    world.query::<(iris_core::entity::Entity, &iris_render::ocean::OceanSurface)>(
        |(entity, ocean)| {
            let bits = entity.to_bits();
            if best_ocean.is_none_or(|(b, _)| bits < b) {
                best_ocean = Some((bits, ocean.height));
            }
        },
    );
    let surface = match best_ocean {
        Some((_, height)) => Some(height),
        None => {
            let mut best_pond: Option<(u64, f32)> = None;
            world.query::<(iris_core::entity::Entity, &iris_render::water::WaterSurface)>(
                |(entity, water)| {
                    let bits = entity.to_bits();
                    if best_pond.is_none_or(|(b, _)| bits < b) {
                        best_pond = Some((bits, water.height));
                    }
                },
            );
            best_pond.map(|(_, height)| height)
        }
    };
    if let Some(height) = surface {
        // 水面在别处（编辑器 / 脚本）刚挪动 ⇒ 本帧物理跟着走；同值写入是
        // 恒等操作（f32 赋值逐位相同），不引入任何数值扰动。
        world.resource_mut::<BuoyancyPlane>().height = height;
    }
}

#[derive(Default)]
pub struct PhysicsPlugin;

impl Plugin for PhysicsPlugin {
    fn build(&self, app: &mut App) {
        use iris_core::physics::{
            BrokenJoints, Collider, ContactTracker, JointBroken, RigidBody, prune_broken_joints,
            step_physics,
        };
        use iris_core::prelude::{Access, CollisionEnded, CollisionStarted, Stage, Transform};
        // 接触事件资源 + 差分基线：游戏侧不注册即得（事件资源缺席时
        // step_physics 静默跳过产出，自建 App 也可按需手动注册）
        app.world.register_event::<CollisionStarted>();
        app.world.register_event::<CollisionEnded>();
        app.world.insert_resource(ContactTracker::default());
        // 水面同步（R93）：独占系统，注册在 `step_physics` **之前**
        //（调度器按注册序分批 ⇒ 本系统先跑，物理当帧就用到新水面高度）。
        app.schedule
            .add_system(Stage::Update, sync_buoyancy_water_surface);
        app.schedule.add_parallel_system(
            Stage::Update,
            Access::new()
                .write::<Transform>()
                .write::<Collider>()
                .write::<RigidBody>()
                .read::<Time>()
                .read::<iris_core::physics::ContactSolverV2>()
                // 浮力平面（R93）：`step_physics` 现在读它——缺席同样合法。
                .read::<iris_core::buoyancy::BuoyancyPlane>(),
            step_physics,
        );
        // 可断关节（C3）：断裂兑现——step_physics 只入队（&World 无法
        // 改组件表），本系统同阶段稍后消费队列移除关节组件并发
        // JointBroken 事件。注册为常规（独占）系统，排在 step_physics
        // 之后（调度器按注册序分批）。
        app.world.register_event::<JointBroken>();
        app.world.insert_resource(BrokenJoints::default());
        app.schedule.add_system(Stage::Update, prune_broken_joints);
    }
}

/// 粒子插件：在 **PostUpdate** 阶段（层级传播之后）推进所有
/// [`iris_render::particles::ParticleEmitter`] 与
/// [`iris_render::particles_v2::ParticleEmitterV2`]。
///
/// 编排语义：发射原点 = `GlobalTransform` 平移——必须在
/// [`HierarchyPlugin`] 的传播之后执行才能取到当帧全局位置（用户在
/// Update 移动发射器，当帧新粒子即从新位置出生）；渲染发生在
/// PostUpdate 全部结束之后，本帧模拟出的活粒子当帧即成像（调用契约
/// 见 `iris_render::particles` / `particles_v2` 模块文档）。注册为**
/// 并行系统**：访问集写 ParticleEmitter / ParticleEmitterV2、读
/// GlobalTransform / Time / Wind（v2 的风采样——R80 接口首个引擎内
/// 消费点；`Wind` 资源缺席 = 无风）。
#[derive(Default)]
pub struct ParticlePlugin;

impl Plugin for ParticlePlugin {
    fn build(&self, app: &mut App) {
        use iris_core::prelude::{Access, GlobalTransform, Stage, Time};
        use iris_render::prelude::{ParticleEmitter, update_particles, update_particles_v2};
        app.schedule.add_parallel_system(
            Stage::PostUpdate,
            Access::new()
                .write::<ParticleEmitter>()
                .read::<GlobalTransform>()
                .read::<Time>(),
            update_particles,
        );
        // v2（R82）：与 v1 同层同序——CPU 侧只做发射记账（发射数 /
        // 种子偏移 / 风采样入队），GPU 模拟由渲染器在 draw 时消费帧
        // 队列执行。两个系统访问集不相交（v1 写 ParticleEmitter、v2 写
        // ParticleEmitterV2），可同批并行。
        app.schedule.add_parallel_system(
            Stage::PostUpdate,
            Access::new()
                .write::<iris_render::prelude::ParticleEmitterV2>()
                .read::<GlobalTransform>()
                .read::<Time>()
                .read::<iris_core::wind::Wind>(),
            update_particles_v2,
        );
        // 毛发摆动（R91）：与粒子同层——写 HairStrands（模拟写回控制
        // 网格）+ HairDynamics（内部状态），与粒子系统访问集不相交。
        app.schedule.add_parallel_system(
            Stage::PostUpdate,
            Access::new()
                .write::<iris_render::prelude::HairStrands>()
                .write::<iris_render::prelude::HairDynamics>()
                .read::<GlobalTransform>()
                .read::<Time>()
                .read::<iris_core::wind::Wind>(),
            iris_render::prelude::update_hair_dynamics,
        );
    }
}

/// 空间音频插件：在 **PostUpdate** 阶段（层级传播之后）同步听者位姿与
/// 空间声源（见 `crate::spatial`）。
///
/// 编排语义与 [`ParticlePlugin`] 同层、同理由：听者与声源都读
/// `GlobalTransform`——必须在 [`HierarchyPlugin`] 的传播之后执行，用户在
/// Update 挪动相机 / 声源，**当帧**的混音增益即跟随新位置（混音增益每帧
/// 重算，无渲染式的当帧成像截止点，晚于粒子的注册序只是确定性的表现
/// 收尾顺序，两者访问集不相交、可同批并行）。注册为**并行系统**：访问集
/// 写 SpatialSource、读 SpatialListener / GlobalTransform / Audio（播放
/// 编排经混音器的 `Arc<Mutex>` 完成，`&Audio` 即可——与 `run_arena` 等
/// 既有系统同款声明）。
#[derive(Default)]
pub struct SpatialPlugin;

impl Plugin for SpatialPlugin {
    fn build(&self, app: &mut App) {
        use crate::spatial::{SpatialListener, SpatialSource, update_spatial_audio};
        use iris_core::prelude::{Access, GlobalTransform, Stage, Time, Transform};
        app.schedule.add_parallel_system(
            Stage::PostUpdate,
            Access::new()
                .write::<SpatialSource>()
                .read::<SpatialListener>()
                .read::<GlobalTransform>()
                .read::<Audio>()
                // v3 多普勒差分读 Time；遮挡判定读静态碰撞体（Collider /
                // RigidBody 的 effective_static 口径 + Transform 位姿）
                .read::<Time>()
                .read::<Transform>()
                .read::<iris_core::physics::Collider>()
                .read::<iris_core::physics::RigidBody>(),
            update_spatial_audio,
        );
    }
}

/// 命令队列插件（打磨 V）：注册 [`iris_core::commands::CommandQueue`]
/// 资源，并在 **PostUpdate 尾部**（渲染之前）注册独占系统
/// [`iris_core::commands::apply_commands`]。
///
/// 编排语义：本帧入队的命令本帧生效——渲染读到最终实体态。放尾部而非
/// 传播之前：spawn 的实体下一帧才进层级传播（新实体当帧无
/// `GlobalTransform`，与「运行期动态组件要预挂池槽」的既有约束同族，
/// 诚实边界见 `iris_core::commands` 模块文档）。已包含在
/// [`DefaultPlugins`] 中；自建 App（不用 DefaultPlugins）需要 spawn /
/// despawn 延迟执行时手动添加（headless 测试同款）。
#[derive(Default)]
pub struct CommandsPlugin;

impl Plugin for CommandsPlugin {
    fn build(&self, app: &mut App) {
        use iris_core::commands::CommandQueue;
        use iris_core::commands::apply_commands;
        use iris_core::prelude::Stage;
        if !app.world.contains_resource::<CommandQueue>() {
            app.world.insert_resource(CommandQueue::default());
        }
        // PostUpdate 最后一个注册（UiPlugin 之后——DefaultPlugins 的注册
        // 顺序保证）独占系统，天然晚于全部并行收尾系统
        app.schedule.add_system(Stage::PostUpdate, apply_commands);
    }
}

/// 昼夜循环插件（R80）：注册 Update 阶段的
/// [`crate::daynight::update_day_night`] 并行系统——读
/// [`iris_core::daynight::DayNightCycle`] 相位推进并把太阳方向 /
/// 色温 / 强度 / 环境光写进 [`DirectionalLight`] 四字段（写入边界与
/// 方向换算见 `crate::daynight` 模块文档；`shadows` / `shadow_range`
/// 不动，`SkySettings` / `FogSettings` 不自动联动）。
///
/// **资源缺席语义**：`DayNightCycle` 未注册 = 系统空转一次资源查询即
/// 过，`DirectionalLight` 一个字段都不被触碰（`RenderPlugin` 插入的
/// 默认日光保持原样）。已包含在 [`DefaultPlugins`] 中（`RenderPlugin`
/// 之后——默认方向光先就位，首帧派生即有目标）；自建 App 时手动添加。
#[derive(Default)]
pub struct DayNightPlugin;

impl Plugin for DayNightPlugin {
    fn build(&self, app: &mut App) {
        use iris_core::daynight::DayNightCycle;
        use iris_core::prelude::{Access, Stage, Time};
        app.schedule.add_parallel_system(
            Stage::Update,
            Access::new()
                .write::<DayNightCycle>()
                .write::<DirectionalLight>()
                .read::<Time>(),
            crate::daynight::update_day_night,
        );
    }
}

/// DDGI 动态全局光照插件（R88）：Startup 插入空的 [`DdgiGrid`] 资源，
/// Update 注册 [`crate::ddgi::tick_ddgi`] 并行系统（时间片滚动重照明）。
///
/// **注册序**：必须在 [`DayNightPlugin`] **之后**——昼夜系统当帧写进
/// `DirectionalLight` 的方向 / 色温 / 强度 / 环境光，当帧就被 DDGI 的重照明
/// 口径消费（`sky_color` 默认取 `ambient`，裁定 3 的 R80 免费联动）。
///
/// **资源缺席语义**：[`DdgiSettings`] 未注册 = 系统空转一次资源查询即过，
/// 渲染器维持 R83 的 mode 裁决（0 / 1），**既有像素逐位不变**（一个
/// `write_buffer` 都不发）。已包含在 [`DefaultPlugins`] 中；自建 App 时手动添加。
///
/// [`DdgiGrid`]: iris_render::ddgi::DdgiGrid
/// [`DdgiSettings`]: iris_render::ddgi::DdgiSettings
#[derive(Default)]
pub struct DdgiPlugin;

impl Plugin for DdgiPlugin {
    fn build(&self, app: &mut App) {
        use iris_core::prelude::{Access, Stage};
        use iris_core::prelude::{Collider, GlobalTransform, RigidBody};
        use iris_render::ddgi::{DdgiGrid, DdgiSettings};
        use iris_render::light::DirectionalLight;
        use iris_render::material::{MaterialHandle, MaterialStore};
        use iris_render::mesh::MeshHandle;
        use iris_render::skin::Skin;
        use iris_render::visibility::Visibility;
        // Startup 插入空网格：`&World` 的系统无法注册新资源类型
        //（`insert_resource` 要 `&mut World`），故落在这里而不是系统体
        if !app.world.contains_resource::<DdgiGrid>() {
            app.world.insert_resource(DdgiGrid::new());
        }
        // 访问集：只写 DdgiGrid，其余全读。几何收集（`collect_bake_triangles`）
        // 读的组件类型逐一声明 ⇒ 调度器把它排在层级传播之后（GlobalTransform
        // 当帧就位），与 DayNight 的 DirectionalLight 写入不冲突（一读一写
        // 同一类型 ⇒ 分批串行，注册序即执行序）。
        app.schedule.add_parallel_system(
            Stage::Update,
            Access::new()
                .write::<DdgiGrid>()
                .read::<DdgiSettings>()
                .read::<DirectionalLight>()
                .read::<GlobalTransform>()
                .read::<MeshHandle>()
                .read::<MaterialHandle>()
                .read::<RigidBody>()
                .read::<Collider>()
                .read::<Skin>()
                .read::<Visibility>()
                .read::<MeshStore>()
                .read::<MaterialStore>(),
            crate::ddgi::tick_ddgi,
        );
    }
}

/// 场景参数热重载插件（C5b · §8.4 场景半边闭合）：Update 阶段注册
/// [`hot_reload_scene_params`] 独占系统。
///
/// **资源缺席语义**：[`SceneHotReload`] 未插入 ⇒ 系统空转一次资源查询
/// 即过、**零文件读取**（缺席即逐位不变的兼容形态；生产档不插入即
/// 零成本）。开发期 `world.insert_resource(SceneHotReload::watch(path))`
/// 开启「手改场景 JSON → 半秒内材质/光照数值生效」。参数子集边界
/// （无结构编辑 / 按 Name 匹配 / 基线轮不应用）见 §8.65。
#[derive(Default)]
pub struct HotReloadPlugin;

impl Plugin for HotReloadPlugin {
    fn build(&self, app: &mut App) {
        app.schedule.add_system(
            iris_core::prelude::Stage::Update,
            crate::hot_reload::hot_reload_scene_params,
        );
    }
}

/// 默认插件组：窗口 + 时间 + 输入 + 渲染 + 音频 + 动画 + 物理 + 层级 +
/// 粒子 + 空间音频 + UI + 命令队列。
///
/// ```ignore
/// App::new().add_plugins(DefaultPlugins::new("我的游戏")).run();
/// ```
pub struct DefaultPlugins {
    window: WindowConfig,
}

impl DefaultPlugins {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            window: WindowConfig::new(title, 1280, 720),
        }
    }

    /// 自定义窗口尺寸。
    pub fn with_size(mut self, width: u32, height: u32) -> Self {
        self.window.width = width;
        self.window.height = height;
        self
    }

    pub fn with_resizable(mut self, resizable: bool) -> Self {
        self.window.resizable = resizable;
        self
    }
}

impl Plugin for DefaultPlugins {
    fn build(&self, app: &mut App) {
        WindowPlugin {
            config: self.window.clone(),
        }
        .build(app);
        TimePlugin.build(app);
        InputPlugin.build(app);
        RenderPlugin.build(app);
        // 场景参数热重载（C5b）：Update 最前——本帧写回材质/光照数值，
        // 同帧下游系统即消费新值。昼夜循环在场时其当帧写入覆盖热重载
        // 的方向光值（共存语义：循环为真值源，§8.65⑥）
        HotReloadPlugin.build(app);
        // 昼夜循环（R80）：默认方向光就位后注册派生系统（缺席 cycle =
        // 零触碰，见 DayNightPlugin 文档）
        DayNightPlugin.build(app);
        // DDGI 动态全局光照（R88）：**必须在昼夜之后**——昼夜当帧写的方向光
        // 当帧被重照明口径消费（缺席 DdgiSettings = 零触碰，见 DdgiPlugin 文档）
        DdgiPlugin.build(app);
        AudioPlugin.build(app);
        AnimationPlugin.build(app);
        // 演出时间线（R47）：动画之后、物理之前（编排理由见 TimelinePlugin
        // 文档）——相机 / UI 写入当帧传播与渲染前生效
        TimelinePlugin.build(app);
        PhysicsPlugin.build(app);
        HierarchyPlugin.build(app);
        // 粒子在层级传播之后推进（发射原点取当帧全局位置）
        ParticlePlugin.build(app);
        // 空间音频与粒子同层（层级传播之后）：听者 / 声源读当帧全局位姿，
        // 当帧混音增益即跟随（访问集与粒子不相交，可同批并行）
        SpatialPlugin.build(app);
        // UI 编排（R41）：三系统注册到 PostUpdate（渲染之前），执行序
        // layout → menus → focus（理由见 crate::ui 模块头）
        UiPlugin.build(app);
        // 命令队列（打磨 V）：PostUpdate 尾部独占 flush——本帧命令本帧
        // 生效（渲染前），见 CommandsPlugin 文档
        CommandsPlugin.build(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 视口资源：缺省 1280×720（与 `DefaultPlugins` 缺省窗口一致），
    /// `WindowPlugin` 以窗口配置尺寸首次插入。
    #[test]
    fn viewport_defaults_and_follows_window_config() {
        let viewport = Viewport::default();
        assert_eq!(
            (viewport.width, viewport.height),
            (1280, 720),
            "缺省视口应与缺省窗口尺寸一致"
        );
        let mut app = App::new();
        WindowPlugin {
            config: WindowConfig::new("视口测试", 800, 600),
        }
        .build(&mut app);
        let viewport = app.world.try_resource::<Viewport>().expect("应已首次插入");
        assert_eq!(
            (viewport.width, viewport.height),
            (800, 600),
            "Viewport 应以窗口初始尺寸插入"
        );
    }

    // ---- 浮力水面同步（R93）----

    /// 跨界同步系统：`follow_water` 三档（资源缺席 / 显式关 / 开）+ 海洋优先
    /// + 池塘按实体位模式首盏 + 无水时零触碰。
    #[test]
    fn buoyancy_sync_follows_ocean_then_pond_and_respects_opt_in() {
        use iris_core::buoyancy::BuoyancyPlane;
        use iris_core::prelude::World;
        use iris_render::{ocean::OceanSurface, water::WaterSurface};

        // 1) 资源缺席：不报错也不凭空插入（缺席 = 无浮力）。
        let mut world = World::new();
        sync_buoyancy_water_surface(&mut world);
        assert!(
            world.try_resource::<BuoyancyPlane>().is_none(),
            "同步系统不得替游戏创建资源"
        );

        // 2) 在场但 follow_water = false（默认）：手设高度逐位不被覆盖。
        let mut world = World::new();
        world.insert_resource(BuoyancyPlane {
            height: -7.25,
            ..BuoyancyPlane::default()
        });
        world.spawn((OceanSurface::new(3.0),));
        sync_buoyancy_water_surface(&mut world);
        assert_eq!(
            world
                .try_resource::<BuoyancyPlane>()
                .unwrap()
                .height
                .to_bits(),
            (-7.25f32).to_bits(),
            "opt-in 未开 ⇒ 引擎不得改写游戏手设的水位"
        );

        // 3) 开跟随 + 海洋在场 ⇒ 用海平面。
        world.resource_mut::<BuoyancyPlane>().follow_water = true;
        sync_buoyancy_water_surface(&mut world);
        assert_eq!(
            world
                .try_resource::<BuoyancyPlane>()
                .unwrap()
                .height
                .to_bits(),
            3.0f32.to_bits(),
            "follow_water ⇒ 取 OceanSurface.height"
        );

        // 4) 海与池塘同挂 ⇒ 仍取海（与渲染器的互斥裁定同序）。
        world.spawn((WaterSurface::new(1.0, glam::Vec2::new(8.0, 8.0)),));
        sync_buoyancy_water_surface(&mut world);
        assert_eq!(
            world
                .try_resource::<BuoyancyPlane>()
                .unwrap()
                .height
                .to_bits(),
            3.0f32.to_bits(),
            "两路同挂时物理必须跟渲染走（海洋优先）"
        );

        // 5) 只有池塘 ⇒ 多水面按实体位模式升序取首盏（与 select_water_surface 同键）。
        let mut world2 = World::new();
        world2.insert_resource(BuoyancyPlane {
            height: 0.0,
            follow_water: true,
            ..BuoyancyPlane::default()
        });
        let first = world2.spawn((WaterSurface::new(1.5, glam::Vec2::splat(4.0)),));
        world2.spawn((WaterSurface::new(9.0, glam::Vec2::splat(4.0)),));
        assert!(
            first.index() < 9_000_000,
            "位模式首盏判据前提（先 spawn 者位模式更小）"
        );
        sync_buoyancy_water_surface(&mut world2);
        assert_eq!(
            world2
                .try_resource::<BuoyancyPlane>()
                .unwrap()
                .height
                .to_bits(),
            1.5f32.to_bits(),
            "多池塘时取位模式首盏"
        );

        // 6) 开跟随但世界无水 ⇒ 保持原值（不写 0、不 NaN）。
        let mut world3 = World::new();
        world3.insert_resource(BuoyancyPlane {
            height: 2.5,
            follow_water: true,
            ..BuoyancyPlane::default()
        });
        sync_buoyancy_water_surface(&mut world3);
        assert_eq!(
            world3
                .try_resource::<BuoyancyPlane>()
                .unwrap()
                .height
                .to_bits(),
            2.5f32.to_bits(),
            "无水时零写入"
        );
    }
}
