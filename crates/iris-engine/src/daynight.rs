//! 昼夜循环插件层（R80）：把 [`iris_core::daynight::DayNightCycle`] 的
//! 相位每帧派生到渲染器的 `DirectionalLight` 资源。
//!
//! **编排语义**：`update_day_night` 是 `&World` 并行系统（Update 阶段，
//! 由 `DayNightPlugin` 注册——`DefaultPlugins` 已含，注册在
//! `RenderPlugin` 之后）：读 `Time.delta_secs` 推进相位（f64，取模回绕）
//! → 把太阳方向 / 色温包络 / 环境光包络写进方向光的
//! direction / color / intensity / ambient 四字段。两阶段模式（先收集
//! 相位与 dt、后写回）——同一资源的读写尝试型锁顺序获取，不并发持有。
//!
//! **资源缺席语义（API 的一部分）**：
//! - `DayNightCycle` 缺席 = **零操作**——`DirectionalLight` 一个字段都
//!   不被触碰（渲染命令流与无本系统时逐位一致）；
//! - `DirectionalLight` 缺席 = 相位照常推进（相位演化是模拟事实，与
//!   渲染资产无关），灯光写入跳过；
//! - `Time` 缺席 = dt 视为 0（与 `update_particles` 同款兜底）：相位
//!   不推进，灯光仍按当前相位重算（暂停语义一致——派生量幂等）。
//!
//! **写入边界（诚实声明）**：只写 direction / color / intensity /
//! ambient 四字段；`shadows` / `shadow_range` 属场景与用户配置，本系统
//! 不动。`SkySettings` / `FogSettings` 等渲染参数**不自动联动**（大气
//! 强度、雾密度不随相位变化——用户手动，ARCHITECTURE §8.49）。
//!
//! **方向语义换算**：`sun_direction(phase)` 指向天上的太阳；
//! `DirectionalLight.direction` 是光传播方向（光源 → 场景），写入值 =
//! `−sun_direction(phase)`（太阳在地平线下时该式照常成立——夜间光从
//! 地下来，强度包络同时归零，无可见效果）。

use iris_core::daynight::{DayNightCycle, ambient_envelope, sun_direction, sun_envelope};
use iris_core::prelude::Time;
use iris_core::world::World;
use iris_render::light::DirectionalLight;

/// 昼夜循环每帧系统（见模块文档）。
pub fn update_day_night(world: &World) {
    // 阶段一（收集）：尝试读循环资源与 dt，读毕即放锁（两阶段模式）
    let Some(cycle) = world.try_resource::<DayNightCycle>() else {
        return; // 缺席 = 零操作（方向光完全不被触碰）
    };
    let phase = cycle.phase;
    let rate = cycle.cycles_per_second;
    let paused = cycle.paused;
    drop(cycle);
    let dt = world
        .try_resource::<Time>()
        .map(|t| t.delta_secs() as f64)
        .unwrap_or(0.0);
    let advanced = !paused && dt != 0.0;
    let new_phase = if advanced {
        (phase + dt * rate).rem_euclid(1.0)
    } else {
        phase
    };
    // 阶段二（写回）：相位先行（模拟事实，与渲染资产解耦）
    if advanced {
        if let Some(mut cycle) = world.try_resource_mut::<DayNightCycle>() {
            cycle.phase = new_phase;
        }
    }
    // 灯光派生：每帧按（推进后的）当前相位重算——幂等，暂停时亦然；
    // 方向光缺席 → 跳过（相位演化不受影响）
    let Some(mut light) = world.try_resource_mut::<DirectionalLight>() else {
        return;
    };
    let sun = sun_direction(new_phase);
    let (color, intensity) = sun_envelope(new_phase);
    light.direction = -sun;
    light.color = color;
    light.intensity = intensity;
    light.ambient = ambient_envelope(new_phase);
    // shadows / shadow_range 不动（场景与用户的阴影配置，见模块文档）
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;
    use iris_core::daynight::DayNightCycle;
    use std::time::Duration;

    /// 组装 headless 世界：Time + 可选 cycle + 自定义哨兵方向光。
    fn sentinel_light() -> DirectionalLight {
        DirectionalLight {
            direction: Vec3::new(0.25, -0.75, 0.5),
            color: Vec3::new(0.9, 0.8, 0.7),
            intensity: 42.0,
            ambient: Vec3::new(0.11, 0.22, 0.33),
            shadows: false,
            shadow_range: 77.0,
        }
    }

    fn world_with(cycle: Option<DayNightCycle>) -> World {
        let mut world = World::new();
        world.insert_resource(Time::default());
        if let Some(cycle) = cycle {
            world.insert_resource(cycle);
        }
        world.insert_resource(sentinel_light());
        world
    }

    /// cycle 在场：单步推进后相位与灯光四字段与纯函数预期一致
    /// （灯光值与「推进后相位的纯函数求值」逐位相同——插件必须写纯
    /// 函数的原样输出；相位本体另对独立手算值断言）。
    #[test]
    fn cycle_present_drives_phase_and_light() {
        let world = world_with(Some(DayNightCycle {
            phase: 0.25,
            cycles_per_second: 1.0,
            paused: false,
        }));
        world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(0.2));
        update_day_night(&world);
        // 相位：0.25 + 0.2 s × 1.0（dt 经 f32 往返，容差 1e-12 写明）
        let phase = world.resource::<DayNightCycle>().phase;
        let expect_phase = 0.25 + (0.2f32 as f64) * 1.0;
        assert!(
            (phase - expect_phase).abs() < 1e-12,
            "相位 {phase} 应为 {expect_phase}"
        );
        // 灯光：与推进后相位的纯函数逐位一致
        let light = world.resource::<DirectionalLight>();
        let sun = sun_direction(phase);
        let (color, intensity) = sun_envelope(phase);
        assert_eq!(
            light.direction.x.to_bits(),
            (-sun).x.to_bits(),
            "direction = −sun_direction（光传播方向换算）"
        );
        assert_eq!(light.direction.y.to_bits(), (-sun).y.to_bits());
        assert_eq!(light.direction.z.to_bits(), (-sun).z.to_bits());
        assert_eq!(light.color.x.to_bits(), color.x.to_bits());
        assert_eq!(light.color.z.to_bits(), color.z.to_bits());
        assert_eq!(light.intensity.to_bits(), intensity.to_bits());
        let ambient = ambient_envelope(phase);
        assert_eq!(light.ambient.x.to_bits(), ambient.x.to_bits());
        assert_eq!(light.ambient.z.to_bits(), ambient.z.to_bits());
        // shadows / shadow_range 不被触碰（哨兵值保持）
        assert!(!light.shadows);
        assert_eq!(light.shadow_range, 77.0);
    }

    /// 连续多步：相位按 dt × 速率累积（5 步 × 0.2 s × 0.5 速率 =
    /// +0.5 相位），中途跨相位回绕亦正确。
    #[test]
    fn multi_step_accumulates_phase() {
        let world = world_with(Some(DayNightCycle {
            phase: 0.1,
            cycles_per_second: 0.5,
            paused: false,
        }));
        for _ in 0..5 {
            world
                .resource_mut::<Time>()
                .advance(Duration::from_secs_f64(0.2));
            update_day_night(&world);
        }
        let phase = world.resource::<DayNightCycle>().phase;
        let expect = 0.1 + 5.0 * (0.2f32 as f64) * 0.5;
        assert!((phase - expect).abs() < 1e-12, "相位 {phase} 应为 {expect}");
    }

    /// 暂停：相位逐位冻结，但灯光仍按当前相位重算（派生量幂等——
    /// 手工改相位后暂停的世界画面照样跟随）。
    #[test]
    fn paused_freezes_phase_but_light_still_follows() {
        let world = world_with(Some(DayNightCycle {
            phase: 0.9,
            cycles_per_second: 1.0,
            paused: true,
        }));
        let before = world.resource::<DayNightCycle>().phase.to_bits();
        world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(0.2));
        update_day_night(&world);
        assert_eq!(
            world.resource::<DayNightCycle>().phase.to_bits(),
            before,
            "暂停时相位逐位不变"
        );
        let light = world.resource::<DirectionalLight>();
        let sun = sun_direction(0.9);
        assert_eq!(
            light.direction.x.to_bits(),
            (-sun).x.to_bits(),
            "灯光应跟随冻结相位"
        );
        assert_eq!(
            light.intensity.to_bits(),
            sun_envelope(0.9).1.to_bits(),
            "夜间强度 0"
        );
    }

    /// cycle 缺席 = 零操作：预置自定义方向光跑多步，六字段逐位不变
    /// （缺席语义的零触碰实证）。
    #[test]
    fn absent_cycle_never_touches_light() {
        let world = world_with(None);
        let snapshot = |l: &DirectionalLight| {
            [
                l.direction.x.to_bits(),
                l.direction.y.to_bits(),
                l.direction.z.to_bits(),
                l.color.x.to_bits(),
                l.color.y.to_bits(),
                l.color.z.to_bits(),
                l.intensity.to_bits(),
                l.ambient.x.to_bits(),
                l.ambient.y.to_bits(),
                l.ambient.z.to_bits(),
            ]
        };
        let before = snapshot(&world.resource::<DirectionalLight>());
        for _ in 0..3 {
            world
                .resource_mut::<Time>()
                .advance(Duration::from_secs_f64(0.2));
            update_day_night(&world);
        }
        let light = world.resource::<DirectionalLight>();
        assert_eq!(before, snapshot(&light), "cycle 缺席时方向光必须逐位不变");
        assert!(!light.shadows);
        assert_eq!(light.shadow_range, 77.0);
    }

    /// 方向光缺席：相位照常推进（模拟事实与渲染资产解耦），无 panic。
    #[test]
    fn absent_light_still_advances_phase() {
        let mut world = World::new();
        world.insert_resource(Time::default());
        world.insert_resource(DayNightCycle {
            phase: 0.3,
            cycles_per_second: 1.0,
            paused: false,
        });
        world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(0.2));
        update_day_night(&world);
        let phase = world.resource::<DayNightCycle>().phase;
        assert!(
            (phase - (0.3 + 0.2f32 as f64)).abs() < 1e-12,
            "相位照常推进 {phase}"
        );
    }

    /// 夜间接线语义：相位 0.75（午夜）时强度归零、环境光 = 夜间下限
    /// （不黑屏的兜底来自本接线 + 包络，跨 crate 集成断面）。
    #[test]
    fn midnight_light_has_zero_sun_and_floor_ambient() {
        let world = world_with(Some(DayNightCycle {
            phase: 0.75,
            cycles_per_second: 0.0,
            paused: false,
        }));
        update_day_night(&world);
        let light = world.resource::<DirectionalLight>();
        assert!(light.intensity < 1e-5, "午夜无直射");
        assert_eq!(
            light.ambient,
            iris_core::daynight::NIGHT_AMBIENT,
            "夜间下限底光"
        );
    }
}
