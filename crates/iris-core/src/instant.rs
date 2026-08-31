//! 跨平台时间点封装：[`Instant`]。
//!
//! 平台原因（wasm32-unknown-unknown）：std 的 `std::time::Instant::now()` 在
//! 该目标上**运行时 panic**（std 未为 unknown-unknown 实现任何时间源；实测
//! Node 与浏览器中调用均触发 unreachable 陷阱）。因此 wasm 分支用
//! `performance.now()`（毫秒 f64，单调、相对页面导航起点）手写最小实现，
//! native 分支直接转发 `std::time::Instant`，零包装开销。
//!
//! 实现范围仅覆盖引擎实际用到的 API：`now` / `elapsed` / `duration_since` /
//! `Instant - Instant -> Duration`（帧间隔）/ `Instant + Duration`（测试构造
//! 确定性时间点）。未实现的 std 方法（如 `checked_duration_since`）在
//! 引擎内没有调用方，暂不提供。

/// 单调时间点。native 为 `std::time::Instant` 的别名；wasm 为 `performance.now()`
/// 封装（见模块文档的平台原因）。
#[cfg(not(target_arch = "wasm32"))]
pub use std::time::Instant;

/// wasm 分支的最小实现（平台原因见模块文档）。
#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Instant {
    /// `performance.now()` 的原始毫秒值（f64）。单调递增，仅用于与同源
    /// 时间点作差，f64 的比较/减法精度（毫秒级 ~2^53 内）远超引擎需求。
    millis: f64,
}

#[cfg(target_arch = "wasm32")]
impl Instant {
    /// 当前时间点。
    pub fn now() -> Self {
        Self { millis: now_millis() }
    }

    /// 距当前时刻的时长（`now() - self`）。
    pub fn elapsed(&self) -> std::time::Duration {
        Self::now().duration_since(*self)
    }

    /// `self` 与更早时间点 `earlier` 的间隔。`earlier` 晚于 `self` 时返回
    /// 零（对齐 std 的饱和语义；性能计时不允许负时长）。
    pub fn duration_since(&self, earlier: Instant) -> std::time::Duration {
        duration_from_millis((self.millis - earlier.millis).max(0.0))
    }
}

#[cfg(target_arch = "wasm32")]
impl std::ops::Add<std::time::Duration> for Instant {
    type Output = Instant;

    fn add(self, rhs: std::time::Duration) -> Instant {
        Instant { millis: self.millis + rhs.as_secs_f64() * 1000.0 }
    }
}

/// `Instant - Instant -> Duration`：帧间隔计算（`Time::begin_frame` 的热路径）。
#[cfg(target_arch = "wasm32")]
impl std::ops::Sub for Instant {
    type Output = std::time::Duration;

    fn sub(self, rhs: Instant) -> std::time::Duration {
        self.duration_since(rhs)
    }
}

#[cfg(target_arch = "wasm32")]
impl std::ops::Sub<std::time::Duration> for Instant {
    type Output = Instant;

    fn sub(self, rhs: std::time::Duration) -> Instant {
        Instant { millis: self.millis - rhs.as_secs_f64() * 1000.0 }
    }
}

/// 读取 `window.performance.now()`（毫秒 f64）。
///
/// 需要 `Window` + `Performance` 两个 web-sys feature（见 iris-core 的
/// `[target.'cfg(target_arch = "wasm32")'.dependencies]`）；web-sys 本身已在
/// wgpu / winit 的 wasm 依赖树内，不引入新编译单元。
#[cfg(target_arch = "wasm32")]
fn now_millis() -> f64 {
    web_sys::window()
        .expect("Instant::now() 需要 Window 上下文（wasm 入口必须在浏览器中运行）")
        .performance()
        .expect("浏览器未提供 performance 时间源")
        .now()
}

/// 毫秒 f64 -> Duration（亚毫秒精度保留；负值已在调用侧钳制为 0）。
#[cfg(target_arch = "wasm32")]
fn duration_from_millis(millis: f64) -> std::time::Duration {
    std::time::Duration::from_secs_f64(millis / 1000.0)
}
