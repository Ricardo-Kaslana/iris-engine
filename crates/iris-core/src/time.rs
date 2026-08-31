//! 时间管理：可变步长渲染 + 固定步长逻辑（fixed timestep）。
//!
//! 主循环模式：
//! ```text
//! begin_frame(now) -> 可变 delta 供渲染插值/视觉系统
//!                 -> accumulator 累积，fixed_steps() 返回本帧需执行的固定步数
//! ```

use std::time::Duration;

// 平台封装：wasm 上 std::time::Instant 运行时 panic，见 crate::instant
use crate::instant::Instant;
use crate::Resource;

/// 时间资源：可变步长渲染 + 固定步长逻辑（fixed timestep）。
#[derive(Debug, Clone, Resource)]
pub struct Time {
    delta: Duration,
    elapsed: Duration,
    ticks: u64,
    fixed_delta: Duration,
    accumulator: Duration,
    max_delta: Duration,
    last_instant: Option<Instant>,
}

impl Default for Time {
    fn default() -> Self {
        Self {
            delta: Duration::ZERO,
            elapsed: Duration::ZERO,
            ticks: 0,
            // 纳秒向内截断（见 set_fixed_hz 的舍入说明）
            fixed_delta: Duration::from_nanos(1_000_000_000 / 60),
            accumulator: Duration::ZERO,
            max_delta: Duration::from_secs_f64(0.25),
            last_instant: None,
        }
    }
}

impl Time {
    /// 每帧开始时调用：计算可变 delta 并累积固定步长预算。
    pub fn begin_frame(&mut self, now: Instant) {
        match self.last_instant.replace(now) {
            Some(last) => {
                self.delta = (now - last).min(self.max_delta);
            }
            None => self.delta = Duration::ZERO,
        }
        self.elapsed += self.delta;
        self.ticks += 1;
        self.accumulator += self.delta;
    }

    /// 上一帧到本帧的可变时长（已钳制，防止后台切回时的巨大步长）。
    pub fn delta(&self) -> Duration {
        self.delta
    }

    pub fn delta_secs(&self) -> f32 {
        self.delta.as_secs_f32()
    }

    /// 引擎运行总时长。
    pub fn elapsed_secs(&self) -> f32 {
        self.elapsed.as_secs_f32()
    }

    /// 已运行的帧数。
    pub fn tick(&self) -> u64 {
        self.ticks
    }

    /// 固定步长（默认 1/60 秒）。
    pub fn fixed_delta(&self) -> Duration {
        self.fixed_delta
    }

    /// 单帧可变 delta 的钳制上限。
    pub fn max_delta(&self) -> Duration {
        self.max_delta
    }

    /// 以赫兹设置固定逻辑帧率。
    ///
    /// 舍入说明：`Duration::from_secs_f64(1/hz)` 会四舍五入到纳秒——
    /// 60Hz 时 16,666,666.67ns 被舍入为 16,666,667ns，于是「3/60 秒的一帧」
    /// 只够 2 个固定步（3 × 16,666,667 > 50,000,000ns），名义帧率与实际
    /// 步数出现差一。此处**向内截断**到纳秒，保证 N 个名义步 ≤ N/hz 秒，
    /// 确定性模拟与回放不会因舍入方向漂移；代价是每步少 0.67ns（每分钟
    /// 累积约 40μs，可忽略）。
    pub fn set_fixed_hz(&mut self, hz: f64) {
        assert!(hz > 0.0, "固定帧率必须为正，实际 {hz}");
        self.fixed_delta = Duration::from_nanos((1.0e9 / hz) as u64);
    }

    /// 消耗累积器，返回本帧应执行的固定逻辑步数（含防死亡螺旋上限）。
    pub fn fixed_steps(&mut self) -> u32 {
        const MAX_STEPS_PER_FRAME: u32 = 5;
        let mut steps = 0;
        while self.accumulator >= self.fixed_delta && steps < MAX_STEPS_PER_FRAME {
            self.accumulator -= self.fixed_delta;
            steps += 1;
        }
        if steps == MAX_STEPS_PER_FRAME {
            // 逻辑帧跟不上：丢弃积压，避免死亡螺旋
            self.accumulator = Duration::ZERO;
        }
        steps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> Instant {
        Instant::now() + Duration::from_secs(secs)
    }

    #[test]
    fn first_frame_has_zero_delta() {
        let mut time = Time::default();
        time.begin_frame(at(0));
        assert_eq!(time.delta(), Duration::ZERO);
    }

    #[test]
    fn delta_tracks_elapsed() {
        let mut time = Time::default();
        let t0 = at(0);
        time.begin_frame(t0);
        time.begin_frame(t0 + Duration::from_millis(100));
        assert_eq!(time.delta(), Duration::from_millis(100));
        assert_eq!(time.elapsed_secs(), 0.1);
        assert_eq!(time.tick(), 2);
    }

    #[test]
    fn fixed_steps_consume_accumulator() {
        let mut time = Time::default();
        time.set_fixed_hz(60.0);
        // 两帧时间点必须源自同一次 Instant::now()：分别调用 now() 会把
        // 测试进程被抢占的真实墙钟间隔算进 delta（并行测试争抢 CPU 时
        // 偶发超过一个固定步长，步数断言随机挂）
        let t0 = Instant::now();
        time.begin_frame(t0);
        // 3/60 秒的一帧 → 3 个固定步
        time.begin_frame(t0 + Duration::from_secs_f64(3.0 / 60.0));
        assert_eq!(time.fixed_steps(), 3);
        assert_eq!(time.fixed_steps(), 0, "累积器应耗尽");
    }

    #[test]
    fn delta_is_clamped_against_huge_gaps() {
        let mut time = Time::default();
        time.begin_frame(at(0));
        time.begin_frame(at(3600));
        assert!(time.delta() <= time.max_delta(), "大间隔应被钳制");
        assert_eq!(time.fixed_steps(), 5, "最多 5 步防死亡螺旋");
    }
}
