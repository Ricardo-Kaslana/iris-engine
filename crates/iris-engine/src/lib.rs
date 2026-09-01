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
pub mod audio;
pub mod plugin;
pub mod scene;

// 纯音频库随门面一并可用：`iris_engine::iris_audio::...` 全路径直达
pub use iris_audio;

pub mod prelude {
    pub use iris_core::prelude::*;
    pub use iris_platform::prelude::*;
    pub use iris_render::prelude::*;
    pub use crate::app::App;
    pub use crate::audio::Audio;
    pub use crate::plugin::{
        AudioPlugin, DefaultPlugins, HierarchyPlugin, InputPlugin, PhysicsPlugin, Plugin,
        RenderPlugin, TimePlugin, WindowPlugin,
    };
    // 场景序列化（JSON 双向）：加载 / 保存、登记表与 glTF 内部实体标记
    pub use crate::scene::{
        load_scene, save_scene, GltfMounted, MaterialRef, MeshRef, SceneAssetRegistry, SceneError,
    };
    // 音频类型（混音器 / 剪辑 / 句柄 / 错误 / 程序化生成与 WAV 加载）
    pub use iris_audio::{
        load_wav, noise, parse_wav, sine, square, AudioClip, AudioError, AudioMixer, AudioOutput,
        SoundId,
    };
}
