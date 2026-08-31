//! 插件系统：以可组合的方式向 `App` 注入资源与配置。

use iris_platform::input::Input;
use iris_platform::window::WindowConfig;
use iris_core::prelude::Time;
use iris_render::prelude::{Camera, DirectionalLight, MaterialStore, MeshStore, TextureStore};

use crate::app::App;
use crate::audio::Audio;

/// 插件：一次 `build` 机会，可修改世界的资源、系统与窗口配置。
pub trait Plugin {
    fn build(&self, app: &mut App);
}

/// 窗口配置插件。
#[derive(Clone, Debug)]
pub struct WindowPlugin {
    pub config: WindowConfig,
}

impl Plugin for WindowPlugin {
    fn build(&self, app: &mut App) {
        app.window_config = self.config.clone();
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

/// 层级插件：每帧在用户 Update 系统之前执行全局变换传播。
///
/// 已包含在 [`DefaultPlugins`] 中；自建 App（不用 DefaultPlugins）时需要
/// 手动添加，否则渲染器读取的 `GlobalTransform` 不会被更新。
#[derive(Default)]
pub struct HierarchyPlugin;

impl Plugin for HierarchyPlugin {
    fn build(&self, app: &mut App) {
        app.schedule
            .add_system(iris_core::prelude::Stage::Update, iris_core::hierarchy::propagate_transforms);
    }
}

/// 动画插件：在层级传播之前推进所有 [`iris_render::animation::AnimationPlayer`]。
///
/// 已包含在 [`DefaultPlugins`] 中（先于 [`HierarchyPlugin`] 执行，
/// 动画写入的局部变换当帧即完成全局传播）。
/// 注册为**并行系统**：只读世界 + 组件写访问走尝试型锁，
/// 调度器据其访问集（写 AnimationPlayer / Transform，读 Time）分批。
#[derive(Default)]
pub struct AnimationPlugin;

impl Plugin for AnimationPlugin {
    fn build(&self, app: &mut App) {
        use iris_core::prelude::{Access, Stage, Transform};
        use iris_render::prelude::AnimationPlayer;
        app.schedule.add_parallel_system(
            Stage::Update,
            Access::new()
                .write::<AnimationPlayer>()
                .write::<Transform>()
                .read::<Time>(),
            iris_render::animation::update_animations,
        );
    }
}

/// 默认插件组：窗口 + 时间 + 输入 + 渲染 + 音频。
///
/// ```ignore
/// App::new().add_plugins(DefaultPlugins::new("我的游戏")).run();
/// ```
pub struct DefaultPlugins {
    window: WindowConfig,
}

impl DefaultPlugins {
    pub fn new(title: impl Into<String>) -> Self {
        Self { window: WindowConfig::new(title, 1280, 720) }
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
        WindowPlugin { config: self.window.clone() }.build(app);
        TimePlugin.build(app);
        InputPlugin.build(app);
        RenderPlugin.build(app);
        AudioPlugin.build(app);
        AnimationPlugin.build(app);
        HierarchyPlugin.build(app);
    }
}
