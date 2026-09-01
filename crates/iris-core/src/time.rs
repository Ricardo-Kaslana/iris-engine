//! 时间管理：可变步长渲染 + 固定步长逻辑（fixed timestep）。
//!
//! 主循环模式：
//! ```text
//! begin_frame(now) -> 可变 delta 供渲染插值/视觉系统
//!                 -> accumulator 累积，fixed_steps() 返回本帧需执行的固定步数
//! ```

use std::time::Duration;

// 平台封装：wasm 上 std::time::Instant 运行时 panic，见 crate::instant
use crate::Resource;
use crate::instant::Instant;

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

    /// 确定性推进一帧（测试 / 回放 / headless 驱动）：语义与 `begin_frame`
    /// 完全一致，只是步长显式给定、不取自单调时钟——调用方无须再手工
    /// 构造 `Instant` 序列（dogfooding：headless 测试的时间基准样板）。
    ///
    /// `delta = min(dt, max_delta)`，elapsed / ticks / accumulator 同步累加；
    /// 与 `begin_frame` 不同，首帧 delta 即为 `min(dt, max_delta)` 而非 0
    /// （确定性驱动没有「第一次采样无差分」的问题）。
    ///
    /// 调用后单调时钟基准归位：与 `begin_frame` 混用时，下一次
    /// `begin_frame` 重新建立基准（delta = 0），不会把 advance 的时长
    /// 二次计入真实时钟差分。
    pub fn advance(&mut self, dt: Duration) {
        self.last_instant = None;
        self.delta = dt.min(self.max_delta);
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

    #[test]
    fn advance_drives_deterministic_frames() {
        let mut time = Time::default();
        let step = Duration::from_secs_f64(1.0 / 60.0);
        // 首帧 delta 即为步长（不依赖 Instant，测试无须手构单调时间点）
        time.advance(step);
        assert_eq!(time.delta(), step);
        assert_eq!(time.delta_secs(), 1.0 / 60.0);
        assert_eq!(time.tick(), 1);
        time.advance(step);
        assert_eq!(time.tick(), 2);
        assert!((time.elapsed_secs() - 2.0 / 60.0).abs() < 1e-6);
    }

    #[test]
    fn advance_is_clamped_and_feeds_accumulator() {
        let mut time = Time::default();
        time.set_fixed_hz(60.0);
        time.advance(Duration::from_secs(3600));
        assert_eq!(
            time.delta(),
            time.max_delta(),
            "advance 与 begin_frame 同受钳制"
        );
        assert_eq!(time.fixed_steps(), 5, "钳制后的预算进入固定步长累积器");
        assert_eq!(time.fixed_steps(), 0);
    }

    #[test]
    fn advance_rebases_monotonic_clock() {
        let mut time = Time::default();
        let t0 = at(0);
        time.begin_frame(t0);
        time.advance(Duration::from_millis(100));
        // 混用：advance 之后 begin_frame 重新建立基准（不把 advance 计入差分）
        time.begin_frame(at(10));
        assert_eq!(time.delta(), Duration::ZERO);
        assert_eq!(time.tick(), 3);
    }
}
