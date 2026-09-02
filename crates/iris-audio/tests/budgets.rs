//! release 预算测试（R33）：混音路径的宽松上限断言。
//!
//! 定位与噪声容忍见 `iris-core/tests/budgets.rs` 模块文档与
//! ARCHITECTURE §7.1：预算 = criterion 基准 mean × 约 5 的烟雾报警，
//! 精确回归检测靠 `cargo bench` 的基线对比。debug 构建跳过（说明性
//! 测试给出原因）。

#[cfg(not(debug_assertions))]
mod release_budgets {
    use std::time::{Duration, Instant};

    use glam::Vec3;
    use iris_audio::mixer::{AudioMixer, ListenerParams};
    use iris_audio::procedural::sine;

    /// 实测 N 次取中位数（抗离群值）。
    fn median_of(runs: usize, mut measure: impl FnMut() -> Duration) -> Duration {
        let mut samples: Vec<Duration> = (0..runs).map(|_| measure()).collect();
        samples.sort();
        samples[runs / 2]
    }

    /// 32 声道混音器（24 非空间 + 8 空间，全部循环），同 benches/audio.rs。
    fn mixer_32_voices() -> AudioMixer {
        let mut mixer = AudioMixer::new();
        mixer.set_listener(ListenerParams::from_view(
            Vec3::new(0.0, 1.5, 0.0),
            -Vec3::Z,
            Vec3::Y,
        ));
        for i in 0..32u32 {
            let clip = sine(110.0 * (1.0 + (i % 8) as f32), 0.5, 44_100);
            let volume = 0.2 + (i % 5) as f32 * 0.06;
            if i % 4 == 0 {
                let position =
                    Vec3::new((i % 7) as f32 * 2.5 - 7.5, 0.5, (i % 11) as f32 * 1.8 - 9.0);
                mixer.play_spatial(clip, position, volume, true);
            } else {
                mixer.play(clip, volume, true);
            }
        }
        mixer
    }

    /// mix_32_voices_1s：32 声道 44100 Hz 立体声单次 1024 帧块（真实
    /// 回调粒度，超预算即实时音频爆音）。
    /// 预算 = 基准 mean 92 µs × 约 5.4（测量机器：Windows 11 / i7-13620H
    /// 笔记本，2026-09-03）。
    #[test]
    fn budget_mix_32_voices_1s() {
        let mut mixer = mixer_32_voices();
        let mut out = vec![0.0f32; 1024 * 2];
        let median = median_of(7, || {
            let start = Instant::now();
            mixer.mix(&mut out, 2, 44_100);
            start.elapsed()
        });
        assert!(
            median <= Duration::from_micros(500),
            "mix_32_voices_1s 中位数 {median:?} 超出预算 500 µs——\
             若非偶发噪声，说明混音路径发生了数量级劣化（实时回调将爆音）"
        );
    }
}

/// debug 构建说明（原因见模块文档）。
#[cfg(debug_assertions)]
#[test]
fn budgets_only_run_in_release() {
    println!(
        "预算测试仅在 release 构建生效：请用 `cargo test --workspace --release` \
         （CI 的 test-release job 即此配置）。debug 下的耗时没有参照意义。"
    );
}
