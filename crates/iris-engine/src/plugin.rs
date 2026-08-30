//! 插件系统：以可组合的方式向 `App` 注入资源与配置。

use iris_platform::input::Input;
use iris_platform::window::WindowConfig;
use iris_core::prelude::Time;
use iris_render::prelude::{Camera, DirectionalLight, MaterialStore, MeshStore, TextureStore};

use crate::app::App;

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

/// 默认插件组：窗口 + 时间 + 输入 + 渲染。
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
        HierarchyPlugin.build(app);
    }
}
