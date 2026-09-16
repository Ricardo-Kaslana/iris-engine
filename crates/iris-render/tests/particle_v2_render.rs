//! 集成测试：粒子 v2（R82）——GPU compute 模拟 + 拖尾条带渲染。
//!
//! 四条主线（无 GPU 适配器时优雅跳过）：
//! 1. **v1 不变（缺席语义）**：仅 v1 发射器的场景，粒子统计与画面与
//!    v1 已知行为一致（沿用 v1 测试断言形态新写一份，不改既有文件）；
//!    v2 机器全程零参与——`draw_calls` 恰为 v1 粒子段的贡献；**v2
//!    组件在场但从未推进 = 零计算零绘制**（alive = 0 → 无 dispatch
//!    无 draw，帧与仅有 v1 时同构）。
//! 2. **v2 生效**：v2 发射器推进后画面出现成片粒子像素（与 0 粒子
//!    基线差分成片）；再推进一步 → 像素分布改变（模拟在 GPU 上真实
//!    推进，非静止贴图）。
//! 3. **拖尾**：同场景 `trail_length = 1`（无拖尾，只有头部 billboard）
//!    vs `trail_length = 8`（头部 + 7 节条带）——拖尾帧的**发光像素**
//!    显著更多、三角数恰为 8 倍。
//! 4. **确定性（会话内逐位）**：同输入双会话（独立渲染器、完整重建
//!    世界、同推进序列）帧 FNV 逐字节一致；**恒定 dispatch 路径下
//!    重复渲染同一状态不改变帧**（帧队列排水后零 dispatch——重绘
//!    幂等）；换种子（发射器实体 bits 不同）→ 帧哈希变化（防恒等
//!    假通过）。
//!
//! # 度量学：为什么不能用「全帧 R 通道阈值计数」
//!
//! 本场景的天空是程序化大气（R79）**恒亮**（sRGB R ≈ 208），全帧
//! `R > 200` 计数恒为 ≈ 30400 px（65536 的 46%）——粒子加光被淹没。
//! 且粒子走 additive 混合进 HDR 目标、经基调映射压缩：条带峰值贡献
//! 在 sRGB 域仅 12–30 级（实测 `count_changed` 阈值 30 时新增像素为
//! **0**，阈值 12 时 2107）。故本文件一律用**与空场景 / 仅头部帧的
//! 逐像素差分**作判据（避开恒亮背景这一混淆源）。
//!
//! # 阈值依据（实测于本机适配器；跨适配器位级不可保，见 §8.51）
//!
//! 场景 = 静态发射器、方向抖动 0.35、条带全宽 0.45 m、粒速 8 m/s、
//! 20 粒/秒、寿命 0.6 s（稳态 12 粒），相机 (0,2,7) 看 (0,1.2,0)。
//! 条带解析面积：节长 = 8/60 = 0.1333 m、宽 0.45 m @ 7 m，投影 y 缩放
//! 1/tan(30°) = 1.732、256 px 视口 ⇒ 4.22 px × 14.26 px = 60 px/节，
//! 12 粒 × 7 节 = **5047 px**。实测 `added(阈值 8) = 3144`（解析面积的
//! 62%，余量来自重叠与软边衰减）——阈值取 1500（实测的 48%、解析面积
//! 的 30%）。
//!
//! # 陷阱（本轮实测踩到，写下来免得重犯）
//!
//! - **换种子的场景必须让发射器实体 bits 真的不同**：先前用例把占位
//!   实体 spawn 在发射器**之后**，实体 bits 不变 ⇒ 种子不变 ⇒ 帧哈希
//!   不变（该断言曾以「不同」为期望而静默失败）。占位必须在发射器
//!   之前生成。
//! - **`velocity_jitter = 0` 时整条哈希流被乘零**：方向恒为 +Y，出生
//!   哈希完全不参与输出 ⇒ 换种子对画面零影响。判别种子的场景必须
//!   带抖动。
//!
//! 同文件多测试并行共用单适配器会挂死（AGENTS.md 测试规约）——文件内
//! `GPU_SERIAL` 互斥串行化（ssr_render 同款）。

use std::sync::Mutex;
use std::time::Duration;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Entity, Time, Transform, World};
use iris_render::prelude::*;

/// 固定模拟步长（1/60 s）。
const STEP: Duration = Duration::from_nanos(1_000_000_000 / 60);

/// 差分判据的通道阈值（sRGB 级；见模块头的度量学说明）。
const DIFF: i32 = 8;

/// 文件内 GPU 互斥（同文件多测试并行共用单适配器会挂死——AGENTS.md）。
static GPU_SERIAL: Mutex<()> = Mutex::new(());

/// 基础世界：空目录 + 相机（+Z 轴上看向发射区）+ 关阴影方向光 + 泛光
/// 关闭 + Time 资源（dt 来源）。
fn base_world() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight {
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    world.insert_resource(Time::default());
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 2.0, 7.0);
        camera.target = Vec3::new(0.0, 1.2, 0.0);
    }
    world
}

/// v2 场景：静态发射器 + 方向抖动扇面（**抖动是判别度的来源**——无抖动
/// 时全部粒子轨迹重合、条带彼此完全重叠，且出生哈希流被乘零；见模块头
/// 陷阱）。参数见模块头「阈值依据」。
fn v2_world(trail_length: u32) -> World {
    v2_world_with_dummy(trail_length, false)
}

/// `dummy_first = true` 时**先**生成一个占位实体再生成发射器——发射器
/// 实体 bits 改变 ⇒ 出生哈希种子改变（换种子用例；顺序不能反，见模块头
/// 陷阱）。
fn v2_world_with_dummy(trail_length: u32, dummy_first: bool) -> World {
    let mut world = base_world();
    if dummy_first {
        world.spawn((Transform::from_xyz(50.0, 50.0, 50.0),));
    }
    let mut emitter = ParticleEmitterV2::trail_fountain();
    emitter.trail_length = trail_length;
    emitter.trail_width = 0.45;
    emitter.velocity_jitter = 0.35;
    emitter.initial_speed = 8.0;
    emitter.spawn_rate = 20.0;
    emitter.particle_lifetime = 0.6;
    emitter.size_start = 0.05;
    emitter.size_end = 0.02;
    emitter.gravity_scale = 0.0;
    world.spawn((Transform::IDENTITY, emitter));
    world
}

fn emitter_of(world: &mut World) -> Entity {
    let mut found = None;
    world.query::<(Entity, &ParticleEmitterV2)>(|(e, _)| found = Some(e));
    found.expect("v2 发射器应在场")
}

/// 推进 `steps` 步：每步同步层级传播 + v2 推进（与 DefaultPlugins 编排
/// 同形态——`update_particles_v2` 必须在 `propagate_transforms` 之后）。
fn simulate(world: &mut World, steps: usize, emitter: Entity) {
    let _ = emitter;
    for _ in 0..steps {
        world.resource_mut::<Time>().advance(STEP);
        propagate_transforms(world);
        update_particles_v2(world);
    }
}

/// 与参考帧逐像素差分超过阈值的像素数（任一 RGB 通道）。
fn count_changed(a: &[u8], b: &[u8], threshold: i32) -> u32 {
    let mut n = 0u32;
    for (ca, cb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        let dr = (ca[0] as i32 - cb[0] as i32).abs();
        let dg = (ca[1] as i32 - cb[1] as i32).abs();
        let db = (ca[2] as i32 - cb[2] as i32).abs();
        if dr.max(dg).max(db) > threshold {
            n += 1;
        }
    }
    n
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 用例 1：v1 零改动——仅 v1 发射器的场景行为与 v1 已知形态一致
///（沿用 particles_render.rs 用例 1 的断言形态新写；v2 机器零参与，
/// 并验证「v2 组件在场但未推进 = 零计算零绘制」）。
#[test]
fn v1_scene_unchanged_and_v2_absence_is_inert() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = base_world();
    world.spawn((
        Transform::from_xyz(0.0, 0.0, 0.0),
        ParticleEmitter::fountain(),
    ));
    // v2 发射器同场**但从不推进**：缺席语义 = 零计算零绘制（alive = 0
    // → 无 dispatch、无 draw、无 uniform 上传——帧与仅有 v1 时同构）
    world.spawn((
        Transform::from_xyz(3.0, 0.0, 0.0),
        ParticleEmitterV2::trail_fountain(),
    ));
    propagate_transforms(&mut world);

    // v2 发射器同场但**只推进 v1**（update_particles 单独调用——对应
    // 「未挂 update_particles_v2 的编排」）：v2 帧队列不排水、alive = 0
    // → 无 dispatch、无 draw——帧与仅有 v1 时同构（缺席语义）
    for _ in 0..60 {
        world.resource_mut::<Time>().advance(STEP);
        update_particles(&world);
    }
    let mut alive = 0usize;
    world.query::<&ParticleEmitter>(|e| alive += e.live_count());
    assert!(
        (85..=95).contains(&alive),
        "v1 喷泉 90 粒/秒 × 1 s 应约 90 颗"
    );

    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(
        stats.particles_drawn as usize, alive,
        "particles_drawn 必须与 v1 发射器池状态一致（v2 未推进不贡献）"
    );
    assert_eq!(
        stats.triangles_drawn,
        2 * stats.particles_drawn,
        "v1 每粒一个 quad = 2 三角形（v2 未贡献任何几何）"
    );
    assert_eq!(
        stats.draw_calls, 1,
        "v1 粒子段恰 1 次 draw（additive/alpha 其一）；v2 零参与不得加 draw"
    );

    // 重复渲染同一状态（帧队列已排空）：v1 与 v2 都不应再步进，
    // 帧逐位一致（重绘幂等——CPU 模拟不依赖渲染 / GPU 帧队列已空）
    let frame_a = renderer.read_pixels();
    renderer.render(&world);
    let frame_b = renderer.read_pixels();
    assert_eq!(
        fnv1a(&frame_a),
        fnv1a(&frame_b),
        "同状态重复渲染必须逐位一致（v2 帧队列排水后零 dispatch）"
    );
}

/// 用例 2：v2 生效——推进后画面出现成片粒子像素；再推进一步像素分布
/// 改变（GPU 模拟真实推进）。
#[test]
fn v2_particles_render_and_move() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = v2_world(8);
    let emitter = emitter_of(&mut world);
    propagate_transforms(&mut world);

    // 基线帧：未推进 → alive 0 → 无粒子 pass（零计算零绘制）
    renderer.render(&world);
    assert_eq!(
        renderer.renderer().stats().particles_drawn,
        0,
        "未推进时 v2 不应贡献任何粒子（缺席语义）"
    );
    let baseline = renderer.read_pixels();

    // 推进 60 步（1 s）：rate 20 / 寿命 0.6 s → 稳态约 12 颗活粒
    //（池状态对账——存活估计与 GPU 死亡判定同式同序）
    simulate(&mut world, 60, emitter);
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert!(
        (8..=20).contains(&stats.particles_drawn),
        "rate 20 / 寿命 0.6 s 应存活约 12 颗，实际 {}",
        stats.particles_drawn
    );
    let frame = renderer.read_pixels();
    let diff = count_changed(&baseline, &frame, DIFF);
    assert!(
        diff > 1000,
        "v2 粒子应成片出现在画面中（实测约 3166，差分像素 {diff}）"
    );

    // 再推进一步：分布改变（GPU 模拟在跑——粒子位移 + 新出生）
    simulate(&mut world, 1, emitter);
    renderer.render(&world);
    let frame_next = renderer.read_pixels();
    let moved = count_changed(&frame, &frame_next, DIFF);
    assert!(
        moved > 200,
        "推进一帧后像素分布应改变（实测约 610，实际 {moved}）——模拟不得冻结"
    );
}

/// 用例 3：拖尾——trail_length 1（无拖尾）vs 8（头部 + 7 节条带），
/// 其余配置逐位相同；拖尾帧发光像素显著更多、三角数恰为 8 倍。
///
/// **跨世界对照各用独立渲染器**（AGENTS.md：渲染器按句柄值缓存 GPU
/// 资源，同一渲染器上渲染同句柄不同内容的多个世界会串味）。
#[test]
fn trail_renders_significantly_more_glow_than_head_only() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut r_empty) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut r_head) = OffscreenRenderer::new(256, 256) else {
        panic!("仅头部会话的渲染器应能创建（空场景会话已成功）");
    };
    let Some(mut r_trail) = OffscreenRenderer::new(256, 256) else {
        panic!("拖尾会话的渲染器应能创建（空场景会话已成功）");
    };

    // 空场景基线（无 v2 组件 = 零计算零绘制）
    let mut empty = base_world();
    propagate_transforms(&mut empty);
    r_empty.render(&empty);
    let empty_px = r_empty.read_pixels();

    let steps = 60;
    let run = |mut world: World, renderer: &mut OffscreenRenderer| {
        let emitter = emitter_of(&mut world);
        propagate_transforms(&mut world);
        simulate(&mut world, steps, emitter);
        renderer.render(&world);
        let stats = renderer.renderer().stats();
        (renderer.read_pixels(), stats)
    };
    let (head_px, head_stats) = run(v2_world(1), &mut r_head);
    let (trail_px, trail_stats) = run(v2_world(8), &mut r_trail);

    let head_lit = count_changed(&empty_px, &head_px, DIFF);
    let trail_lit = count_changed(&empty_px, &trail_px, DIFF);
    assert!(
        head_lit > 20,
        "无拖尾帧应有头部粒子在画（实测约 46，实际 {head_lit}）"
    );
    // 发光像素：条带面积远大于头部圆点面积（解析 5047 px vs 12 个
    // 半径约 1.6 px 的头部 ≈ 46 px）——实测 3166 vs 46，69 倍
    assert!(
        trail_lit > head_lit * 10,
        "拖尾帧发光像素（{trail_lit}）应显著多于仅头部（{head_lit}）——\
         条带未生成的症状是该比值塌缩到 1 附近"
    );
    // 相对仅头部帧的**新增**像素 = 条带在头部未覆盖位置加光的直接证据
    // （解析面积 5047 px，实测 3144；阈值取实测的 48%）
    let added = count_changed(&head_px, &trail_px, DIFF);
    assert!(
        added > 1500,
        "拖尾帧相对仅头部帧应有成片新增像素（实测约 3144，实际 {added}）"
    );
    // 结构性断言：几何量必须精确随 trail_length 线性（池 × t × 6 顶点，
    // 死粒子零面积退化不改变三角数）
    assert_eq!(
        trail_stats.triangles_drawn,
        head_stats.triangles_drawn * 8,
        "拖尾三角数应恰为仅头部的 8 倍（固定顶点数 = 池 × trail_length × 6）"
    );
    assert_eq!(
        trail_stats.draw_calls, head_stats.draw_calls,
        "两者 draw call 数应一致（同一发射器同一条管线，拖尾只多顶点）"
    );
}

/// 用例 4：会话内确定性——同输入双会话帧逐字节一致；换种子（占位实体
/// 先于发射器生成 → 发射器实体 bits 不同 → 出生哈希流不同）帧哈希变化。
#[test]
fn dual_session_bitwise_and_seed_sensitivity() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(r_a) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(r_b) = OffscreenRenderer::new(256, 256) else {
        panic!("确定性会话 B 的渲染器应能创建（会话 A 已成功）");
    };
    let Some(r_c) = OffscreenRenderer::new(256, 256) else {
        panic!("种子会话的渲染器应能创建（会话 A 已成功）");
    };
    let run = |mut renderer: OffscreenRenderer, dummy_first: bool| {
        let mut world = v2_world_with_dummy(8, dummy_first);
        let emitter = emitter_of(&mut world);
        propagate_transforms(&mut world);
        simulate(&mut world, 45, emitter);
        renderer.render(&world);
        fnv1a(&renderer.read_pixels())
    };
    // 双会话（独立渲染器 + 完整重建世界 + 同推进序列）
    let ha = run(r_a, false);
    let hb = run(r_b, false);
    assert_eq!(ha, hb, "同输入双会话必须逐位一致（FNV-1a 全帧哈希）");
    // 换种子：占位实体先占首个实体位 → 发射器 bits 不同 → 出生哈希流
    // 不同 → 画面不同（防「恒等哈希」假通过）
    let hc = run(r_c, true);
    assert_ne!(ha, hc, "不同 seed（实体 bits）必须产生不同帧");
}
