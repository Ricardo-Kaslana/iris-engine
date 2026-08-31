//! 程序化音效生成器：正弦、方波、伪随机噪声。
//!
//! 全部生成器**确定性**（禁用真实随机源）：同参数必产出逐采样相同的
//! 剪辑；噪声由调用方传入的固定 `seed` 经 xorshift64\\* 流水线生成
//! （可复现回放、快照比对）。波形相位直接由解析式逐采样计算（非增量
//! 振荡器），无累积漂移，单测可对照解析解逐采样验证。
//!
//! 所有音效首尾各加 5ms 线性淡入淡出包络：首尾采样的突跳（直流阶跃）
//! 会在混音链路与换能器上产生「啪」声（爆音），淡入淡出把首尾压回 0；
//! 时长不足 10ms 时包络自动缩至一半帧数，超短音效同样安全。

use crate::clip::AudioClip;

/// 首尾淡入淡出长度（秒）。
const FADE_SECS: f32 = 0.005;

/// 正弦音：`sample[i] = sin(2π f t) × 包络(t)`，t = i / sample_rate。
pub fn sine(freq_hz: f32, duration_secs: f32, sample_rate: u32) -> AudioClip {
    generate(duration_secs, sample_rate, |t| {
        (core::f32::consts::TAU * freq_hz * t).sin()
    })
}

/// 方波音：相位 `fract(f t) < 0.5` 输出 +1、否则 -1（同样带首尾包络）。
pub fn square(freq_hz: f32, duration_secs: f32, sample_rate: u32) -> AudioClip {
    generate(duration_secs, sample_rate, |t| {
        if (freq_hz * t) % 1.0 < 0.5 { 1.0 } else { -1.0 }
    })
}

/// 伪随机白噪声：xorshift64\\* 输出取高 24 位均匀映射到 [-1, 1)。
/// 固定 `seed` 可复现；`seed = 0` 会被非零化（xorshift 全零态锁死）。
pub fn noise(duration_secs: f32, sample_rate: u32, seed: u64) -> AudioClip {
    let mut state = if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed };
    generate(duration_secs, sample_rate, move |_| {
        let x = next_xorshift(&mut state);
        // 高 24 位 / 2^24 → [0, 1)，× 2 − 1 → [-1, 1)
        (x >> 40) as f32 / 16_777_216.0 * 2.0 - 1.0
    })
}

/// xorshift64\\*（Marsaglia 2003）：一轮移位异或 + 黄金常数乘法。
fn next_xorshift(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

/// 共用骨架：按波形解析式逐采样生成单声道剪辑，再套首尾 5ms 线性包络。
fn generate(duration_secs: f32, sample_rate: u32, mut wave: impl FnMut(f32) -> f32) -> AudioClip {
    let sample_rate = sample_rate.max(1);
    let frame_count = (duration_secs.max(0.0) * sample_rate as f32).round() as usize;
    let mut samples: Vec<f32> =
        (0..frame_count).map(|i| wave(i as f32 / sample_rate as f32)).collect();
    apply_fade(&mut samples, sample_rate);
    // 生成器内部构造：单声道、帧数必整除，参数恒合法
    AudioClip::from_interleaved(samples, 1, sample_rate).expect("程序化生成参数恒合法")
}

/// 首尾 [`FADE_SECS`] 线性淡入淡出：包络 `min(t/0.005, 1)`，与解析式一致
/// （单测据此逐采样对照）。时长不足时 fade 缩至一半帧数。
fn apply_fade(samples: &mut [f32], sample_rate: u32) {
    let fade = ((FADE_SECS * sample_rate as f32) as usize).min(samples.len() / 2);
    for i in 0..fade {
        let gain = i as f32 / fade as f32;
        samples[i] *= gain;
        let tail = samples.len() - 1 - i;
        samples[tail] *= gain;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 apply_fade 同式的包络解析解（首尾 fade 帧，末帧精确归零）。
    fn envelope(t: f32, duration: f32, sample_rate: u32) -> f32 {
        let n = (duration * sample_rate as f32).round() as f32;
        let fade = ((FADE_SECS * sample_rate as f32) as usize).max(1) as f32;
        let i = t * sample_rate as f32;
        (i / fade).min(1.0).min((n - 1.0 - i) / fade).max(0.0)
    }

    #[test]
    fn sine_matches_analytic_solution() {
        let (freq, dur, rate) = (440.0f32, 0.05f32, 48_000u32);
        let clip = sine(freq, dur, rate);
        assert_eq!((clip.channels(), clip.sample_rate()), (1, rate));
        let n = clip.frame_count();
        assert_eq!(n, (dur * rate as f32).round() as usize);
        for (i, got) in clip.samples().iter().enumerate() {
            let t = i as f32 / rate as f32;
            let want = (core::f32::consts::TAU * freq * t).sin() * envelope(t, dur, rate);
            assert!((got - want).abs() < 1e-5, "sine[{i}] = {got}，解析解 {want}");
        }
    }

    #[test]
    fn square_matches_analytic_solution() {
        let (freq, dur, rate) = (440.0f32, 0.02f32, 48_000u32);
        let clip = square(freq, dur, rate);
        for (i, got) in clip.samples().iter().enumerate() {
            let t = i as f32 / rate as f32;
            let want = (if (freq * t) % 1.0 < 0.5 { 1.0 } else { -1.0 })
                * envelope(t, dur, rate);
            assert!((got - want).abs() < 1e-5, "square[{i}] = {got}，解析解 {want}");
        }
    }

    /// 固定 seed 可复现：两次生成逐采样相等；不同 seed 产出不同波形。
    #[test]
    fn noise_is_deterministic_by_seed() {
        let a = noise(0.01, 44_100, 42);
        let b = noise(0.01, 44_100, 42);
        assert_eq!(a.samples(), b.samples(), "同 seed 必须逐采样相同");
        let c = noise(0.01, 44_100, 7);
        assert_ne!(a.samples(), c.samples(), "不同 seed 应产出不同波形");
        // 值域 [-1, 1)，且确实在动（非常量）
        assert!(a.samples().iter().all(|s| s.is_finite() && s.abs() <= 1.0));
        let (mut min, mut max) = (f32::MAX, f32::MIN);
        for &s in a.samples() {
            min = min.min(s);
            max = max.max(s);
        }
        assert!(max - min > 0.5, "白噪声应有明显摆动，实际摆幅 {}", max - min);
    }

    /// 首尾包络：首尾采样精确为 0，淡入结束后恢复满幅（防爆音）。
    #[test]
    fn fade_envelope_protects_edges() {
        let rate = 48_000u32;
        let clip = sine(220.0, 0.2, rate);
        let samples = clip.samples();
        assert_eq!(samples[0], 0.0, "首采样应淡入为 0");
        assert_eq!(*samples.last().expect("非空"), 0.0, "尾采样应淡出为 0");
        // fade 区之后包络应为 1：与无包络解析解逐点一致
        let fade = (0.005 * rate as f32) as usize;
        let t = fade as f32 / rate as f32;
        let want = (core::f32::consts::TAU * 220.0 * t).sin();
        assert!(
            (samples[fade] - want).abs() < 1e-5,
            "淡入结束后应为满幅解析值：{} vs {want}",
            samples[fade]
        );
    }

    /// 超短时长（不足两倍 fade）：帧数安全缩包络，不 panic、首尾仍为 0。
    #[test]
    fn tiny_duration_is_safe() {
        let clip = sine(440.0, 0.000_5, 48_000); // 24 帧 < 2×240
        assert_eq!(clip.frame_count(), 24);
        let s = clip.samples();
        assert_eq!(s[0], 0.0);
        assert_eq!(*s.last().expect("非空"), 0.0);
        // 包络缩至一半帧数（fade = 12）：除首尾 12 帧外均为满幅
        assert!(s[12].abs() > 0.3, "第 12 帧应仍有有效信号：{}", s[12]);
        // 零 / 负时长 → 空剪辑
        assert_eq!(sine(440.0, 0.0, 48_000).frame_count(), 0);
        assert_eq!(noise(-1.0, 48_000, 1).frame_count(), 0);
    }
}
