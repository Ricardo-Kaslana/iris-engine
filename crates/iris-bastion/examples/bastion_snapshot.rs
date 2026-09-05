//! 工具：Iris Bastion（iris-bastion）中期战局视觉验收（离屏渲染）。
//!
//! 用与 headless 测试**完全相同的方式**驱动真实游戏世界（BastionPlugin
//! 系统链 + `HierarchyPlugin` / `ParticlePlugin` 注册 + `Time::advance`
//! 确定性推进，不建窗口），定格**中期战局**：
//! - 四座塔已就位（两哨兵 + 两臼炮，池槽位呈现双形态）；
//! - 第 1 波敌群在途（疾行虫沿航点推进，哨兵交火弹道在飞）；
//! - 至少一次击杀已发生（死亡爆炸粒子在画面上扩散）；
//! - 幽灵预览可见（臼炮形态、蓝调可放 + 半透明射程盘，R23）；
//! - HUD（CREDITS / WAVE / CORE HP / 建造提示）完整。
//!
//! 输出 `iris_bastion_midgame.bmp`（640×360 一帧）。转 PNG：
//! ```sh
//! ffmpeg -y -i iris_bastion_midgame.bmp docs/bastion.png
//! ```
//!
//! 同时打印该帧 FrameStats 并断言 `culled_instances > 0`——界外岩柱是
//! 视锥剔除在真实场景生效的量化证据。
//!
//! ```sh
//! cargo run -p iris-bastion --example bastion_snapshot --release
//! ```

use std::time::Duration;

use glam::IVec2;

use iris_bastion::{
    BastionPlugin, BastionState, CORE_POS, GhostKind, TowerKind, TowerPool, cell_center,
};
use iris_core::prelude::Stage;
use iris_engine::prelude::{
    App, Audio, Camera, DirectionalLight, HierarchyPlugin, Input, MaterialStore, MeshStore,
    OffscreenRenderer, ParticlePlugin, Plugin, Time,
};

/// 与 tests/headless.rs 同款测试台：虚拟时钟 + 确定性 1/60 s 推进。
struct Harness {
    app: App,
}

impl Harness {
    fn new() -> Self {
        let mut app = App::new();
        app.world.insert_resource(Time::default());
        app.world.insert_resource(Input::new());
        app.world.insert_resource(MeshStore::new());
        app.world.insert_resource(MaterialStore::new());
        app.world.insert_resource(Camera::default());
        // 与 arena_battle.scene.json 同款的柔和日光（缺省灯会把中亮色材质
        // 洗白）
        app.world.insert_resource(DirectionalLight {
            direction: glam::Vec3::new(-0.45, -1.0, -0.35),
            color: glam::Vec3::new(0.95, 0.93, 0.9),
            intensity: 0.55,
            ambient: glam::Vec3::new(0.12, 0.12, 0.14),
            ..DirectionalLight::default()
        });
        app.world.insert_resource(Audio::headless());
        HierarchyPlugin.build(&mut app);
        ParticlePlugin.build(&mut app);
        BastionPlugin.build_schedule(&mut app.schedule);
        let mut harness = Self { app };
        harness
            .app
            .schedule
            .run(Stage::Startup, &mut harness.app.world);
        iris_core::hierarchy::propagate_transforms(&mut harness.app.world);
        harness
    }

    fn frame(&mut self) {
        // 帧首事件轮换（镜像 App::tick 的新生命周期：上帧事件降级为仍可读的
        // 上一代、更早一代丢弃——打磨 V）
        self.app.world.clear_all_events();
        self.app
            .world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(1.0 / 60.0));
        self.app.schedule.run(Stage::Update, &mut self.app.world);
        self.app
            .schedule
            .run(Stage::PostUpdate, &mut self.app.world);
        self.app.world.resource_mut::<Input>().end_frame();
    }

    fn frames(&mut self, count: usize) {
        for _ in 0..count {
            self.frame();
        }
    }

    /// 光标挪到目标格中心（像素经 world_to_screen 逆换算，幽灵跟随）。
    fn move_cursor_to_cell(&mut self, cell: IVec2) {
        let center = cell_center(cell);
        let (px, py) = iris_bastion::world_to_screen(
            &self.app.world,
            glam::Vec3::new(center.x, 0.0, center.y),
        )
        .expect("场内格应可投影到屏幕");
        self.app
            .world
            .resource_mut::<Input>()
            .on_cursor_moved((px, py));
    }
}

fn main() {
    let Some(mut renderer) = OffscreenRenderer::new(640, 360) else {
        eprintln!("无 GPU，无法离屏渲染");
        std::process::exit(1);
    };

    let mut game = Harness::new();

    // 先空场推进 2 s：第 1 波敌群出笼并聚成纵队（塔后置，避免哨兵把
    // 首波在射程边逐个截杀、画面空场——arena_snapshot 的 nofire 同款
    // 「摆拍窗口」思路，只是改序为「先聚敌、后置塔」）
    game.frames(120);
    {
        let state = game.app.world.resource::<BastionState>();
        assert!(
            state.enemies.len() >= 3,
            "第 1 波敌群应在途，实际 {} 只",
            state.enemies.len()
        );
        assert_eq!(state.total_shots, 0, "摆拍静置阶段无塔不开火");
    }

    // ---- 摆拍中期战局（快照允许直接构造中间态，同 arena_snapshot 约定；
    // 战斗推进本身仍走真实模拟）----
    {
        let mut state = game.app.world.resource_mut::<BastionState>();
        // 四座塔：两哨兵压西段与长纵段、两臼炮守双弯与东段
        for &(cell, kind) in [
            (IVec2::new(2, 3), TowerKind::Sentry),
            (IVec2::new(6, 5), TowerKind::Sentry),
            (IVec2::new(11, 4), TowerKind::Mortar),
            (IVec2::new(16, 7), TowerKind::Mortar),
        ]
        .iter()
        {
            state.towers.push(iris_bastion::TowerState {
                cell,
                kind,
                cooldown: 0.0,
            });
        }
        // 积分呈现中期存钱状态（初始 100 + 首批击杀收入 - 建造成本）
        state.credits = 130;
    }

    // 快照定向：选种臼炮（射程盘更大更醒目）+ 光标悬停在战局旁的空格
    game.app.world.resource_mut::<GhostKind>().0 = TowerKind::Mortar;
    game.move_cursor_to_cell(IVec2::new(12, 7));

    // 定格时机：等一个「已有击杀（爆炸在扩散）+ 弹道在飞（枪口闪光
    // 新鲜）」的帧
    let mut framed = false;
    for _ in 0..120 {
        game.frame();
        let state = game.app.world.resource::<BastionState>();
        if state.total_kills >= 1 && !state.bullets.is_empty() {
            framed = true;
            break;
        }
    }
    assert!(
        framed,
        "置塔后 2 s 内应出现「击杀爆炸 + 弹道在飞」的定格窗口"
    );

    {
        let state = game.app.world.resource::<BastionState>();
        assert_eq!(state.wave, 1, "定格在第 1 波");
        assert!(!state.enemies.is_empty(), "敌群应在途");
        assert!(
            state
                .enemies
                .iter()
                .all(|enemy| enemy.pos().distance(CORE_POS) > 1.0),
            "敌人应尚在途（未抵达核心）"
        );
        assert!(
            state.total_kills >= 1 && state.total_kills <= 3,
            "定格时应有 1~3 次击杀（爆炸可见），实际 {}",
            state.total_kills
        );
    }

    // 渲染 + 落盘
    renderer.render(&game.app.world);
    renderer
        .save_bmp("iris_bastion_midgame.bmp")
        .expect("写 BMP");
    let stats = renderer.renderer().stats();
    println!("=== 定格帧 FrameStats（640×360）===");
    println!(
        "draw_calls = {}，instances = {}，culled_instances = {}，triangles = {}，particles_drawn = {}",
        stats.draw_calls,
        stats.instances,
        stats.culled_instances,
        stats.triangles_drawn,
        stats.particles_drawn
    );
    assert!(
        stats.culled_instances > 0,
        "界外岩柱应产生视锥剔除（culled_instances > 0），实际 {}",
        stats.culled_instances
    );
    assert!(
        stats.particles_drawn > 0,
        "死亡爆炸 / 枪口闪光粒子应在画面上（particles_drawn > 0），实际 {}",
        stats.particles_drawn
    );
    let _ = game
        .app
        .world
        .try_resource::<TowerPool>()
        .expect("塔池在位")
        .slots
        .len();
    println!("已输出 iris_bastion_midgame.bmp");
    println!("转 PNG：ffmpeg -y -i iris_bastion_midgame.bmp docs/bastion.png");
}
