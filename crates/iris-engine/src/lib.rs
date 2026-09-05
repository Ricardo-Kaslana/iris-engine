//! # iris-engine
//!
//! Iris 3D 游戏引擎门面：App 框架、插件系统与主循环。
//!
//! ```ignore
//! use iris_engine::prelude::*;
//!
//! fn main() {
//!     App::new()
//!         .add_plugins(DefaultPlugins::new("我的游戏"))
//!         .add_system(Stage::Startup, setup_scene)
//!         .add_system(Stage::Update, gameplay)
//!         .run();
//! }
//! ```

pub mod app;
pub mod assets;
pub mod audio;
pub mod camera_rig;
pub mod gltf_builder;
pub mod plugin;
pub mod scene;
pub mod settings;
pub mod spatial;
pub mod ui;

// 纯音频库随门面一并可用：`iris_engine::iris_audio::...` 全路径直达
pub use iris_audio;

pub mod prelude {
    pub use crate::app::App;
    pub use crate::audio::Audio;
    pub use iris_core::prelude::*;
    pub use iris_platform::prelude::*;
    pub use iris_render::prelude::*;
    // 资产路径解析：默认资产在 cargo run / cargo test 两种 cwd 下都能命中
    pub use crate::assets::asset_path;
    pub use crate::plugin::{
        AnimationPlugin, AudioPlugin, CommandsPlugin, DefaultPlugins, HierarchyPlugin, InputPlugin,
        ParticlePlugin, PhysicsPlugin, Plugin, RenderPlugin, SpatialPlugin, TimePlugin, Viewport,
        WindowPlugin,
    };
    // 场景序列化（JSON 双向）：加载 / 保存、登记表与 glTF 内部实体标记
    pub use crate::scene::{
        GltfMounted, MaterialRef, MeshRef, SceneAssetRegistry, SceneError, load_scene, save_scene,
    };
    // 编程式 glTF / GLB 构造器（打磨 IV）：记账归 builder，人只声明
    // 几何 / 层级 / 关键帧
    pub use crate::gltf_builder::{AnimChannel, GltfBuilder, GltfNodeDesc};
    // 平滑追随范式（打磨 IV）：子实体相机 + 帧率无关指数趋近
    pub use crate::camera_rig::{SmoothFollow, update_smooth_follow};
    // 空间音频 v1（听者 / 声源组件与每帧同步系统）
    pub use crate::spatial::{SpatialListener, SpatialSource, update_spatial_audio};
    // UI 编排（R41）：锚定布局系统 + 注册 UI 资源 / 事件 / 三系统的插件
    pub use crate::ui::{UiPlugin, update_ui_layout};
    // 设置持久化（R41）：加载 / 保存全局设置
    pub use crate::settings::Settings;
    // 音频类型（混音器 / 剪辑 / 句柄 / 错误 / 程序化生成与 WAV 加载）
    pub use iris_audio::{
        AudioClip, AudioError, AudioMixer, AudioOutput, ListenerParams, SoundId, load_wav, noise,
        parse_wav, sine, square,
    };
}
