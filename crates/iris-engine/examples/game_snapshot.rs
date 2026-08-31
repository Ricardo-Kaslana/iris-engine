//! 工具：收集小游戏（iris-game）双时刻视觉验收（离屏渲染）。
//!
//! 用与 headless 测试**完全相同的方式**驱动真实游戏世界（GamePlugin 系统
//! 链 + Time 固定步进 + Input 脚本化按键注入，不建窗口），分别渲染：
//!
//! - 左：开局——玩家在场地中心，10 枚金币的金环可见，HUD "SCORE 0/10"；
//! - 右：直行拾取 1 枚 + 标记 2 枚已收（模拟拾取 3 枚后）——金币减少、
//!   HUD 变为 "SCORE 3/10"、玩家与相机（子实体层级跟随）已移动。
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

use iris_core::prelude::{Instant, Schedule, Stage, World};
use iris_engine::prelude::{
    Audio, Camera, DirectionalLight, Input, MaterialStore, MeshStore, OffscreenRenderer,
    TextureStore, Time,
};
use iris_game::{GamePlugin, GameState, Phase};

/// 与 tests/headless.rs 同款测试台：虚拟时钟 + Input 注入 + 插件系统链。
struct Harness {
    world: World,
    schedule: Schedule,
    elapsed: f64,
    origin: Instant,
}

impl Harness {
    fn new() -> Self {
        let mut world = World::new();
        // DefaultPlugins 等价的资源（离屏路径无 App / 窗口）；
        // GamePlugin 的 setup_resources 对已存在资源一律不覆盖
        world.insert_resource(Time::default());
        world.insert_resource(Input::new());
        world.insert_resource(MeshStore::new());
        world.insert_resource(MaterialStore::new());
        world.insert_resource(TextureStore::new());
        world.insert_resource(Camera::default());
        world.insert_resource(DirectionalLight::default());
        world.insert_resource(Audio::new());

        let mut schedule = Schedule::new();
        GamePlugin.build_schedule(&mut schedule);

        let mut harness =
            Self { world, schedule, elapsed: 0.0, origin: Instant::now() };
        harness.schedule.run(Stage::Startup, &mut harness.world);
        harness.frame(&[]); // 首帧 delta = 0，只建立时间基准
        harness
    }

    fn frame(&mut self, held: &[winit::keyboard::KeyCode]) {
        use winit::event::ElementState;
        use winit::keyboard::PhysicalKey;

        self.elapsed += 1.0 / 60.0;
        self.world
            .resource_mut::<Time>()
            .begin_frame(self.origin + Duration::from_secs_f64(self.elapsed));
        {
            let mut input = self.world.resource_mut::<Input>();
            for &key in held {
                input.on_key(PhysicalKey::Code(key), ElementState::Pressed);
            }
        }
        self.schedule.run(Stage::Update, &mut self.world);
        self.world.clear_all_events();
        self.world.resource_mut::<Input>().end_frame();
    }

    fn frames(&mut self, count: usize, held: &[winit::keyboard::KeyCode]) {
        for _ in 0..count {
            self.frame(held);
        }
    }

    /// 释放按键：`Input` 的「按住」状态跨帧保持（end_frame 只清一次性
    /// 状态），换方向前必须显式发 Released，否则新旧按键叠加相消。
    fn release(&mut self, held: &[winit::keyboard::KeyCode]) {
        use winit::event::ElementState;
        use winit::keyboard::PhysicalKey;

        let mut input = self.world.resource_mut::<Input>();
        for &key in held {
            input.on_key(PhysicalKey::Code(key), ElementState::Released);
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
    renderer.render(&game.world);
    renderer.save_bmp("iris_game_start.bmp").expect("写 BMP");

    // ---- 驱动到「拾取 3 枚后」----
    // 1) 脚本化按键直行：真实走完 9 m 拾取环上 (9, 0) 金币
    game.frames(110, &[winit::keyboard::KeyCode::KeyD]);
    game.release(&[winit::keyboard::KeyCode::KeyD]);
    // 2) 折返 1 s（朝向转回场地中心，相机随层级带回 wide 机位）
    game.frames(60, &[winit::keyboard::KeyCode::KeyA]);
    game.release(&[winit::keyboard::KeyCode::KeyA]);
    // 3) 直接操纵 GameState 标记另外 2 枚已收（快照允许跳过寻路）
    {
        let mut state = game.world.resource_mut::<GameState>();
        for i in [5, 6] {
            state.pickups[i].collected = true;
        }
        state.score += 2;
        assert_eq!(state.phase, Phase::Playing, "拾取 3 枚后应仍在对局中");
    }
    game.frames(2, &[]); // 表现系统追平：金币沉底、HUD 刷新

    renderer.render(&game.world);
    renderer.save_bmp("iris_game_after.bmp").expect("写 BMP");

    println!("已输出 iris_game_start.bmp / iris_game_after.bmp");
    println!("拼图：ffmpeg -y -i iris_game_start.bmp -i iris_game_after.bmp -filter_complex hstack docs/game_preview.png");
}
