//! 体素沙盒窗口入口。

use iris_engine::prelude::*;
use iris_voxel::VoxelPlugin;

fn main() {
    // 125% DPI 下缺省 1280×720 会放大成 1600×900，在 1536×864 屏幕上
    // 越界并裁掉底部快捷栏；1152×648 的物理尺寸仍能完整显示 HUD。
    // 摩擦记录见 ARCHITECTURE §8.78 第 ⑩ 条。
    App::new()
        .add_plugins(
            DefaultPlugins::new("Iris Voxel · 左键捕获/挖掘，右键放置，Esc 释放后再退出")
                .with_size(1152, 648),
        )
        .add_plugin(VoxelPlugin::default())
        .run();
}
