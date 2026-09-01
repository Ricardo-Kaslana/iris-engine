//! 工具：收集小游戏（iris-game）双时刻视觉验收（离屏渲染）。
//!
//! 用与 headless 测试**完全相同的方式**驱动真实游戏世界（GamePlugin 系统
//! 链 + `Time::advance` 确定性推进 + `Input::press / release` 脚本化按键
//! 注入 + PostUpdate 层级传播，不建窗口、不依赖 winit 类型），关卡来自
//! 场景文件（`assets/collect_arena.scene.json`：四围墙 + 障碍柱 + 中央
//! 点光经 `load_scene` 载入），分别渲染：
//!
//! - 左：开局——玩家在场地中心，围墙与障碍柱在位，10 枚金币的金环可见，
//!   HUD "SCORE 0/10"；
//! - 右：直行拾取 1 枚 + 标记 2 枚已收（模拟拾取 3 枚后）——已收金币切
//!   `Visibility::HIDDEN`（渲染跳过）、HUD 变为 "SCORE 3/10"、玩家与相机
//!   （子实体层级跟随）已移动。
//!
//! 输出 `iris_game_start.bmp` / `iris_game_after.bmp`（1280×720 各一帧）。
//! 拼图命令（hstack 后删 BMP）：
//! ```sh
//! ffmpeg -y -i iris_game_start.bmp -i iris_game_after.bmp \
//!     -filter_complex hstack docs/game_preview.png
//! ```
//!
//! ```sh
//! cargo run --example game_snapshot --release
//! ```

use std::time::Duration;

use iris_core::prelude::Stage;
use iris_engine::prelude::{
    App, Audio, Camera, DirectionalLight, HierarchyPlugin, Input, KeyCode, MaterialStore,
    MeshStore, OffscreenRenderer, Plugin, TextureStore, Time,
};
use iris_game::{GamePlugin, GameState, Phase, DEFAULT_SCENE_PATH};

/// 与 tests/headless.rs 同款测试台：虚拟时钟 + Input 注入 + 插件系统链。
/// 承载于无窗口的 `App`（`App::new` 不建窗口 / 不碰 GPU，`run` 才会）。
struct Harness {
    app: App,
}

impl Harness {
    fn new() -> Self {
        let mut app = App::new();
        // DefaultPlugins 等价的资源（离屏路径无窗口）；
        // 音频强制 headless（纯混音器，绝不打开设备）；
        // GamePlugin 的 setup_resources 对已存在资源一律不覆盖
        app.world.insert_resource(Time::default());
        app.world.insert_resource(Input::new());
        app.world.insert_resource(MeshStore::new());
        app.world.insert_resource(MaterialStore::new());
        app.world.insert_resource(TextureStore::new());
        app.world.insert_resource(Camera::default());
        app.world.insert_resource(DirectionalLight::default());
        app.world.insert_resource(Audio::headless());

        // 层级传播：HierarchyPlugin 注册的 PostUpdate 阶段（与 DefaultPlugins
        // 同一插件——Update 写局部变换 → PostUpdate 传播 → 渲染读取）
        HierarchyPlugin.build(&mut app);
        // 关卡来自场景文件（显式给默认路径——示例的 cwd 即 workspace 根）
        GamePlugin::new(DEFAULT_SCENE_PATH).build_schedule(&mut app.schedule);

        let mut harness = Self { app };
        harness.app.schedule.run(Stage::Startup, &mut harness.app.world);
        harness
    }

    fn frame(&mut self, held: &[KeyCode]) {
        // Time::advance：确定性推进（首帧即有效 delta，无须单调时钟基准）
        self.app
            .world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(1.0 / 60.0));
        {
            let mut input = self.app.world.resource_mut::<Input>();
            for &key in held {
                input.press(key);
            }
        }
        // 与 App::tick 同序：Update → PostUpdate（层级传播当帧生效）
        self.app.schedule.run(Stage::Update, &mut self.app.world);
        self.app.schedule.run(Stage::PostUpdate, &mut self.app.world);
        self.app.world.clear_all_events();
        self.app.world.resource_mut::<Input>().end_frame();
    }

    fn frames(&mut self, count: usize, held: &[KeyCode]) {
        for _ in 0..count {
            self.frame(held);
        }
    }

    /// 释放按键：`Input` 的「按住」状态跨帧保持（end_frame 只清一次性
    /// 状态），换方向前必须显式 release，否则新旧按键叠加相消。
    fn release(&mut self, held: &[KeyCode]) {
        let mut input = self.app.world.resource_mut::<Input>();
        for &key in held {
            input.release(key);
        }
    }
}

fn main() {
    env_logger::init();
    let Some(mut renderer) = OffscreenRenderer::new(1280, 720) else {
        eprintln!("无 GPU");
        std::process::exit(1);
    };

    let mut game = Harness::new();

    // ---- 时刻一：开局（玩家居中、金币环完整、SCORE 0/10）----
    game.frames(3, &[]); // 空跑 3 帧让浮动/自转动画离开 t=0 基准位
    renderer.render(&game.app.world);
    renderer.save_bmp("iris_game_start.bmp").expect("写 BMP");

    // ---- 驱动到「拾取 3 枚后」----
    // 1) 脚本化按键直行：真实走完 9 m 拾取环上 (9, 0) 金币
    game.frames(110, &[KeyCode::KeyD]);
    game.release(&[KeyCode::KeyD]);
    // 2) 折返 1 s（朝向转回场地中心，相机随层级带回 wide 机位）
    game.frames(60, &[KeyCode::KeyA]);
    game.release(&[KeyCode::KeyA]);
    // 3) 直接操纵 GameState 标记另外 2 枚已收（快照允许跳过寻路）
    {
        let mut state = game.app.world.resource_mut::<GameState>();
        for i in [5, 6] {
            state.pickups[i].collected = true;
        }
        state.score += 2;
        assert_eq!(state.phase, Phase::Playing, "拾取 3 枚后应仍在对局中");
    }
    game.frames(2, &[]); // 表现系统追平：金币隐藏、HUD 刷新

    renderer.render(&game.app.world);
    renderer.save_bmp("iris_game_after.bmp").expect("写 BMP");

    println!("已输出 iris_game_start.bmp / iris_game_after.bmp");
    println!("拼图：ffmpeg -y -i iris_game_start.bmp -i iris_game_after.bmp -filter_complex hstack docs/game_preview.png");
}
