//! 体素沙盒窗口入口。

use iris_engine::prelude::*;
use iris_voxel::VoxelPlugin;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins::new(
            "Iris Voxel · 左键捕获/挖掘，右键放置，Esc 释放后再退出",
        ))
        .add_plugin(VoxelPlugin::default())
        .run();
}
