//! 工具：Iris Arena（iris-arena）双时刻视觉验收（离屏渲染）。
//!
//! 用与 headless 测试**完全相同的方式**驱动真实游戏世界（ArenaPlugin 系统
//! 链 + `AnimationPlugin` 等价注册 + `HierarchyPlugin` + `Time::advance`
//! 确定性推进 + `Input::press / release` 脚本化按键注入 + PostUpdate 层级
//! 传播，不建窗口），关卡来自 `assets/arena_battle.scene.json`（地面 /
//! 四面围墙 / 七根障碍柱 / 中央点光经 `load_scene` 载入），分别渲染：
//!
//! - 左：**wave 2 混战**——开局后脚本驱敌清掉第 1 波、间歇进入第 2 波
//!   （7 只蒙皮蠕虫从场边围拢），自动开火弹道在飞，HUD（WAVE / SCORE /
//!   COMBO / HP 条）完整；
//! - 右：**GAME OVER 横幅**——构造 hp 1 + 敌人贴脸，接触归零后横幅切换
//!   "GAME OVER - PRESS R TO RESTART"、HP 条清空。
//!
//! 两时刻相隔真实推进的帧（AnimationPlayer 经 `update_animations` 逐帧
//! 推进扭动剪辑），蒙皮蠕虫的扭动相位在两帧间应有可见差异。
//!
//! 输出 `iris_arena_wave2.bmp` / `iris_arena_gameover.bmp`（1280×720 各
//! 一帧）。拼图命令（hstack 后删 BMP）：
//! ```sh
//! ffmpeg -y -i iris_arena_wave2.bmp -i iris_arena_gameover.bmp \
//!     -filter_complex hstack docs/arena_preview.png
//! ```
//!
//! ```sh
//! cargo run -p iris-engine --example arena_snapshot --release
//! ```

use std::time::Duration;

use iris_core::prelude::{Access, Stage, Vec2};
use iris_engine::prelude::{
    AnimationPlayer, App, Audio, Camera, DirectionalLight, HierarchyPlugin, Input, KeyCode,
    MaterialStore, MeshStore, OffscreenRenderer, Plugin, TextureStore, Time, Transform,
    update_animations,
};
use iris_arena::{ArenaPlugin, ArenaState};

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
        // ArenaPlugin 的 setup_resources 对已存在资源一律不覆盖
        app.world.insert_resource(Time::default());
        app.world.insert_resource(Input::new());
        app.world.insert_resource(MeshStore::new());
        app.world.insert_resource(MaterialStore::new());
        app.world.insert_resource(TextureStore::new());
        app.world.insert_resource(Camera::default());
        app.world.insert_resource(DirectionalLight::default());
        app.world.insert_resource(Audio::headless());

        // 层级传播（PostUpdate，HierarchyPlugin）+ 动画推进（Update，
        // AnimationPlugin 的注册原样内联——该插件未列入 iris-engine
        // prelude，headless 路径无法直接 build，见 ARCHITECTURE §8.13）
        HierarchyPlugin.build(&mut app);
        app.schedule.add_parallel_system(
            Stage::Update,
            Access::new()
                .write::<AnimationPlayer>()
                .write::<Transform>()
                .read::<Time>(),
            update_animations,
        );
        // 关卡来自场景文件（显式给默认路径——示例的 cwd 即 workspace 根）
        ArenaPlugin::default().build_schedule(&mut app.schedule);

        let mut harness = Self { app };
        harness.app.schedule.run(Stage::Startup, &mut harness.app.world);
        harness
    }

    /// 推进一帧（确定性 1/60 s），`held` 持续按住、`taps` 强制新边沿。
    fn frame(&mut self, held: &[KeyCode], taps: &[KeyCode]) {
        self.app
            .world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(1.0 / 60.0));
        {
            let mut input = self.app.world.resource_mut::<Input>();
            for &key in taps {
                input.release(key);
            }
            for &key in held {
                input.press(key); // 已按住会被去重：不产生新边沿
            }
            for &key in taps {
                input.press(key);
            }
        }
        self.app.schedule.run(Stage::Update, &mut self.app.world);
        self.app.schedule.run(Stage::PostUpdate, &mut self.app.world);
        self.app.world.clear_all_events();
        self.app.world.resource_mut::<Input>().end_frame();
    }

    /// 连续推进多帧；`nofire` 时每帧把开火计时拨到未来（step 先减一个
    /// dt，仍 > 0）——快照脚本用于让敌群贴身（自动开火会在远处拦截敌人，
    /// 单靠它永远打不出近身混战）。
    fn frames_nofire(&mut self, count: usize) {
        for _ in 0..count {
            if let Some(mut state) = self.app.world.try_resource_mut::<ArenaState>() {
                state.player.fire_timer = 1.0;
            }
            self.frame(&[], &[]);
        }
    }

    fn frames(&mut self, count: usize) {
        for _ in 0..count {
            self.frame(&[], &[]);
        }
    }

    /// 释放按键（「按住」跨帧保持，换向前必须显式释放）。
    fn release(&mut self, keys: &[KeyCode]) {
        let mut input = self.app.world.resource_mut::<Input>();
        for &key in keys {
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

    // ---- 时刻一：wave 2 混战 ----
    let mut game = Harness::new();
    // 任意键开局（W 点击确认，随即释放——后续脚本不按移动键，站桩开火）
    game.frame(&[], &[KeyCode::KeyW]);
    game.release(&[KeyCode::KeyW]);
    // 玩家挪到出生环内侧（快照允许跳过寻路）：几秒内接敌进入混战
    game.app.world.resource_mut::<ArenaState>().player.pos = Vec2::new(16.0, 6.0);
    game.frames(150); // 2.5 s：第 1 波接敌、开火
    // 清掉第 1 波 → 间歇倒计时 → 第 2 波自动登场（7 只）
    game.app.world.resource_mut::<ArenaState>().enemies.clear();
    game.frames(200); // 间歇 3 s + 第 2 波从出生环登场
    // 快照定向混战：关火让整波敌群无拦截地围拢 2.5 s（自动开火会在远处
    // 逐个拦下敌人，永远到不了贴身），随后回满血、恢复开火——定格时刻
    // 同时有贴身蠕虫群与在飞弹道
    game.frames_nofire(220); // 3.7 s：整波围拢到深度贴身
    game.app.world.resource_mut::<ArenaState>().player.hp = 5;
    let shots_before_reenable =
        game.app.world.try_resource::<ArenaState>().map(|s| s.total_shots).unwrap_or(0);
    game.app.world.resource_mut::<ArenaState>().player.fire_timer = 0.0;
    game.frames(10); // 恢复自动开火：贴身点射（弹道近，命中即逝）
    {
        let mut state = game.app.world.resource_mut::<ArenaState>();
        assert!(
            state.total_shots > shots_before_reenable,
            "恢复开火后应继续自动开火"
        );
        // 构图定向：把玩家挪到整群敌军的**南侧** 2.2 m（蠕虫在北 = 追击
        // 方向朝南 = 朝画面近处，整群入画、经典「被涌进画面」的混战构图）
        let (avg_x, max_z, count) = state.enemies.iter().fold(
            (0.0f32, f32::MIN, 0usize),
            |(sx, mz, n), e| (sx + e.pos.x, mz.max(e.pos.y), n + 1),
        );
        state.player.pos = Vec2::new(avg_x / count.max(1) as f32, max_z + 2.2);
        // 摆拍弹道：贴身点射的弹道当帧即命中消失，画面上补 4 条 6~9 m 外
        // 的在飞弹道（id 与逻辑分配器错开；逻辑行为已由 headless 集成测试
        // 覆盖，此处只为定格画面）
        let center = state.player.pos;
        let yaw = state.player.facing;
        for i in 0..4u32 {
            let angle = yaw + i as f32 * std::f32::consts::TAU / 4.0;
            let (s, c) = angle.sin_cos();
            state.projectiles.push(iris_arena::ProjectileState {
                id: 9500 + i,
                pos: center + Vec2::new(c * 7.0, s * 7.0),
                vel: -Vec2::new(c, s) * 22.0,
                life: 1.0,
            });
        }
    }
    game.frame(&[], &[]);
    {
        let state = game.app.world.try_resource::<ArenaState>().expect("状态在位");
        assert_eq!(state.wave, 2, "应已进入第 2 波");
        assert!(state.enemies.len() >= 4, "第 2 波应仍有敌群在场，实际 {}", state.enemies.len());
        assert!(!state.projectiles.is_empty(), "应有弹道在飞");
        assert!(state.total_damage >= 2, "混战应已发生接触伤害");
        assert_eq!(state.phase, iris_arena::Phase::Playing, "应仍在对局中");
    }
    // 回满血呈现完整 HP 条
    game.app.world.resource_mut::<ArenaState>().player.hp = 5;
    game.frame(&[], &[]);
    renderer.render(&game.app.world);
    renderer.save_bmp("iris_arena_wave2.bmp").expect("写 BMP");

    // ---- 时刻二：GAME OVER 横幅（相隔真实推进的帧：扭动相位已不同）----
    // hp 1 + 两只敌人贴脸与侧翼（快照允许直接构造中间态；终局定格在
    // 玩家倒下的蠕虫包围中）
    {
        let mut state = game.app.world.resource_mut::<ArenaState>();
        state.player.pos = Vec2::ZERO;
        state.player.invuln = 0.0;
        state.player.hp = 1;
        state.enemies.clear();
        for (i, pos) in [Vec2::new(0.5, 0.2), Vec2::new(-1.4, -1.3)].into_iter().enumerate() {
            state.enemies.push(iris_arena::EnemyState {
                id: 9990 + i as u32,
                pos,
                hp: 999,
                hit_flash: 0.0,
                kb: Vec2::ZERO,
            });
        }
    }
    game.frame(&[], &[]); // 接触 → hp 归零 → GameOver
    {
        let state = game.app.world.try_resource::<ArenaState>().expect("状态在位");
        assert_eq!(state.phase, iris_arena::Phase::GameOver, "接触伤害应触发终局");
        assert_eq!(state.player.hp, 0);
    }
    renderer.render(&game.app.world);
    renderer.save_bmp("iris_arena_gameover.bmp").expect("写 BMP");

    println!("已输出 iris_arena_wave2.bmp / iris_arena_gameover.bmp");
    println!("拼图：ffmpeg -y -i iris_arena_wave2.bmp -i iris_arena_gameover.bmp -filter_complex hstack docs/arena_preview.png");
}
