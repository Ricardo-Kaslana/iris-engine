//! 集成测试：粒子系统（R24）——billboard 实例化渲染 + additive / alpha
//! 双混合 + 发射器包围球剔除统计。
//!
//! 三条主线（无 GPU 适配器时优雅跳过）：
//! 1. **喷泉可见性与统计对账**：fountain 预设推进 60 步后渲染，画面出现
//!    粒子像素（与 0 粒子基线帧差分成片），`FrameStats.particles_drawn`
//!    等于各发射器池内活粒子数之和（渲染口径 = 模拟池状态）；
//! 2. **additive 亮度**：40 颗完全重叠的同色粒子分别以 additive / alpha
//!    两种混合渲染——加法逐层**累加**（N·rgb·a，HDR 值远超 1），alpha
//!    混合逐层**收敛**于单层颜色（合成系数 1−(1−a)ⁿ → 1，结果 ≈ rgb）。
//!    ACES 压缩后 additive 中心像素仍显著亮于 alpha（解析：alpha 收敛到
//!    rgb=0.35 → ACES ≈ 0.49 → sRGB ≈ 186；additive = 40×0.35×0.5 = 7.0
//!    HDR → ACES ≈ 0.997 → sRGB ≈ 254；间隔 ≈ 68，容差 40 覆盖 8-bit
//!    量化与 ACES 近似误差）。若混合状态错配（两条管线互换 / REPLACE），
//!    间隔坍缩到个位数，断言必然区分；
//! 3. **爆炸 one-shot**：`burst_count = 320` 首次 update 一次性触发 →
//!    `particles_drawn == 320`；推进超过粒子寿命（1.4 s）后 → 0 且不再
//!    补发（burst 只触发一次、spawn_rate = 0）。
//!
//! 每个场景变体用独立 `OffscreenRenderer`（FrameStats 互不串扰，
//! 与 transparent_render.rs 同款约定）。

use std::time::Duration;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Time, Transform, World};
use iris_render::prelude::*;

/// 固定模拟步长（1/60 s，与 Time::advance 确定性驱动配套）。
const STEP: Duration = Duration::from_nanos(1_000_000_000 / 60);

/// 基础世界：空目录 + 相机（+Z 轴上看向原点）+ 关阴影方向光 + 泛光关闭
/// + Time 资源（`update_particles` 的 dt 来源）。
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
        camera.eye = Vec3::new(0.0, 1.8, 6.0);
        camera.target = Vec3::new(0.0, 1.0, 0.0);
    }
    world
}

/// 确定性推进 N 步（Time::advance 驱动 delta + update_particles 消费）。
fn simulate(world: &World, steps: usize) {
    for _ in 0..steps {
        world.resource_mut::<Time>().advance(STEP);
        update_particles(world);
    }
}

/// 各发射器池内活粒子总数（与 FrameStats.particles_drawn 对账用）。
fn live_particles(world: &World) -> usize {
    let mut total = 0usize;
    world.query::<&ParticleEmitter>(|e| total += e.live_count());
    total
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

/// 中心像素 [r, g, b]。
fn center_pixel(px: &[u8], w: u32, h: u32) -> [i32; 3] {
    let i = ((h / 2 * w + w / 2) * 4) as usize;
    [px[i] as i32, px[i + 1] as i32, px[i + 2] as i32]
}

/// 用例 1：喷泉可见性 + 统计与池状态对账。
#[test]
fn fountain_renders_visible_particles_matching_pool_stats() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = base_world();
    world.spawn((
        Transform::from_xyz(0.0, 0.0, 0.0),
        ParticleEmitter::fountain(),
    ));
    propagate_transforms(&mut world);

    // 基线帧：尚未模拟 → 0 粒子、无粒子 pass
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(stats.particles_drawn, 0, "未模拟时不应有粒子");
    let baseline = renderer.read_pixels();

    // 持续 60 步（1 s）：rate=90、寿命 1.6 s → 无粒子死亡、无池满
    simulate(&world, 60);
    let alive = live_particles(&world);
    assert!(
        (85..=95).contains(&alive),
        "90 粒/秒 × 1 s 应约 90 颗活粒子（累计分数法 ±1），实际 {alive}"
    );

    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(
        stats.particles_drawn as usize, alive,
        "particles_drawn 必须与发射器池状态一致"
    );
    assert!(stats.draw_calls >= 1, "粒子段应产生 1 次 draw call");
    assert_eq!(
        stats.triangles_drawn,
        2 * stats.particles_drawn,
        "每粒一个 quad = 2 三角形"
    );

    // 画面出现成片粒子像素（喷泉位于画面中心区域，与基线差分显著）
    let frame = renderer.read_pixels();
    let diff = count_changed(&baseline, &frame, 12);
    assert!(diff > 200, "喷泉粒子应成片出现在画面中，差分像素 {diff}");
}

/// 用例 2：additive 与 alpha 混合的可区分性——40 颗完全重叠的同色粒子
///（jitter=0、初速 0、无重力 → 位置逐位相同），additive 中心像素必须
/// 显著亮于 alpha（解析依据见模块文档）。
#[test]
fn additive_particles_outshine_alpha_blending() {
    let stacked = |blend: ParticleBlend| {
        let mut e = ParticleEmitter::new(blend);
        e.burst_count = 40;
        e.spawn_rate = 0.0;
        e.particle_lifetime = 2.0;
        e.initial_speed = 0.0;
        e.velocity_jitter = 0.0;
        e.gravity_scale = 0.0;
        e.size_start = 0.8;
        e.size_end = 0.8;
        // 暗红 + 半透明：alpha 收敛于 rgb=0.35；additive 累加到 7.0 HDR
        e.color_start = [0.35, 0.06, 0.05, 0.5];
        e.color_end = [0.35, 0.06, 0.05, 0.5];
        e.max_particles = 256;
        e
    };
    let render = |blend: ParticleBlend| {
        let mut renderer = OffscreenRenderer::new(256, 256)?;
        let mut world = base_world();
        // 粒子恰好位于注视点：中心像素被全部 40 颗重叠 quad 覆盖
        world.spawn((Transform::from_xyz(0.0, 1.0, 0.0), stacked(blend)));
        propagate_transforms(&mut world);
        simulate(&world, 1);
        renderer.render(&world);
        let stats = renderer.renderer().stats();
        assert_eq!(stats.particles_drawn, 40, "重叠粒子全部计入统计");
        Some((renderer.read_pixels(), stats))
    };
    let (Some((additive_px, _)), Some((alpha_px, _))) = (
        render(ParticleBlend::Additive),
        render(ParticleBlend::Alpha),
    ) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let (w, h) = (256, 256);
    let additive = center_pixel(&additive_px, w, h);
    let alpha = center_pixel(&alpha_px, w, h);
    // additive：层层累加至 HDR 饱和 → 接近满量程
    assert!(
        additive[0] > 230,
        "additive 中心应累加到饱和（>230），实际 {additive:?}"
    );
    // alpha：合成收敛于单层颜色 → 明显低于 additive（间隔解析值 ≈ 68，
    // 容差 40 覆盖量化与 ACES 近似误差；若管线混合错配，间隔坍缩到个位）
    assert!(
        additive[0] >= alpha[0] + 40,
        "additive（{}）应显著亮于 alpha（{}）——加法累加 vs 收敛于单层色",
        additive[0],
        alpha[0]
    );
    assert!(alpha[0] > 100, "alpha 粒子本身也应可见，实际 {alpha:?}");
}

/// 用例 3：爆炸 one-shot——burst 首次 update 一次性触发，粒子全部死亡后
/// 不再补发。
#[test]
fn explosion_burst_once_then_all_dead() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = base_world();
    world.spawn((Transform::IDENTITY, ParticleEmitter::explosion()));
    propagate_transforms(&mut world);

    // 首步：burst 触发（320 颗，max_particles=1024 不裁剪）
    simulate(&world, 1);
    assert_eq!(live_particles(&world), 320, "burst 应一次性出生 320 颗");
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(
        stats.particles_drawn, 320,
        "首帧后 particles_drawn 应恰为 burst_count"
    );
    assert!(stats.instances >= 320, "粒子计入 instances 统计");

    // 推进 2 s（> 寿命 1.4 s）：全部死亡、burst 不重复、rate=0 不补发
    simulate(&world, 120);
    assert_eq!(live_particles(&world), 0, "超过寿命后池应清空");
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(stats.particles_drawn, 0, "死亡后不应再绘制任何粒子");

    // 再推进仍为 0（burst 已消耗、无持续发射源）
    simulate(&world, 60);
    renderer.render(&world);
    assert_eq!(renderer.renderer().stats().particles_drawn, 0, "不得补发");
}
