//! 插件系统：以可组合的方式向 `App` 注入资源与配置。

use iris_core::prelude::Time;
use iris_platform::input::Input;
use iris_platform::window::WindowConfig;
use iris_render::prelude::{Camera, DirectionalLight, MaterialStore, MeshStore, TextureStore};

use crate::app::App;
use crate::audio::Audio;

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
/// 并在同一阶段执行足部 IK（v3，按 `FootIk` 组件开关）。
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
/// 均注册为**并行系统**（只读世界 + 组件写访问走尝试型锁）。
#[derive(Default)]
pub struct AnimationPlugin;

impl Plugin for AnimationPlugin {
    fn build(&self, app: &mut App) {
        use iris_core::prelude::{Access, ChildOf, Collider, RigidBody, Stage, Transform};
        use iris_render::prelude::{AnimationPlayer, FootIk};
        app.schedule.add_parallel_system(
            Stage::Update,
            Access::new()
                .write::<AnimationPlayer>()
                .write::<Transform>()
                .read::<Time>(),
            iris_render::animation::update_animations,
        );
        // 足部 IK（v3）：动画之后、层级传播之前（顺序理由见插件文档与
        // foot_ik 模块文档「编排」）
        app.schedule.add_parallel_system(
            Stage::Update,
            Access::new()
                .write::<Transform>()
                .read::<AnimationPlayer>()
                .read::<FootIk>()
                .read::<ChildOf>()
                .read::<Collider>()
                .read::<RigidBody>(),
            iris_render::foot_ik::update_foot_ik,
        );
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
#[derive(Default)]
pub struct PhysicsPlugin;

impl Plugin for PhysicsPlugin {
    fn build(&self, app: &mut App) {
        use iris_core::physics::{Collider, ContactTracker, RigidBody, step_physics};
        use iris_core::prelude::{Access, CollisionEnded, CollisionStarted, Stage, Transform};
        // 接触事件资源 + 差分基线：游戏侧不注册即得（事件资源缺席时
        // step_physics 静默跳过产出，自建 App 也可按需手动注册）
        app.world.register_event::<CollisionStarted>();
        app.world.register_event::<CollisionEnded>();
        app.world.insert_resource(ContactTracker::default());
        app.schedule.add_parallel_system(
            Stage::Update,
            Access::new()
                .write::<Transform>()
                .write::<Collider>()
                .write::<RigidBody>()
                .read::<Time>(),
            step_physics,
        );
    }
}

/// 粒子插件：在 **PostUpdate** 阶段（层级传播之后）推进所有
/// [`iris_render::particles::ParticleEmitter`]。
///
/// 编排语义：发射原点 = `GlobalTransform` 平移——必须在
/// [`HierarchyPlugin`] 的传播之后执行才能取到当帧全局位置（用户在
/// Update 移动发射器，当帧新粒子即从新位置出生）；渲染发生在
/// PostUpdate 全部结束之后，本帧模拟出的活粒子当帧即成像（调用契约
/// 见 `iris_render::particles` 模块文档）。注册为**并行系统**：
/// 访问集写 ParticleEmitter、读 GlobalTransform / Time。
#[derive(Default)]
pub struct ParticlePlugin;

impl Plugin for ParticlePlugin {
    fn build(&self, app: &mut App) {
        use iris_core::prelude::{Access, GlobalTransform, Stage, Time};
        use iris_render::prelude::{ParticleEmitter, update_particles};
        app.schedule.add_parallel_system(
            Stage::PostUpdate,
            Access::new()
                .write::<ParticleEmitter>()
                .read::<GlobalTransform>()
                .read::<Time>(),
            update_particles,
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
        use iris_core::prelude::{Access, GlobalTransform, Stage};
        app.schedule.add_parallel_system(
            Stage::PostUpdate,
            Access::new()
                .write::<SpatialSource>()
                .read::<SpatialListener>()
                .read::<GlobalTransform>()
                .read::<Audio>(),
            update_spatial_audio,
        );
    }
}

/// 默认插件组：窗口 + 时间 + 输入 + 渲染 + 音频 + 动画 + 物理 + 层级 +
/// 粒子 + 空间音频。
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
        AudioPlugin.build(app);
        AnimationPlugin.build(app);
        PhysicsPlugin.build(app);
        HierarchyPlugin.build(app);
        // 粒子在层级传播之后推进（发射原点取当帧全局位置）
        ParticlePlugin.build(app);
        // 空间音频与粒子同层（层级传播之后）：听者 / 声源读当帧全局位姿，
        // 当帧混音增益即跟随（访问集与粒子不相交，可同批并行）
        SpatialPlugin.build(app);
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
}
