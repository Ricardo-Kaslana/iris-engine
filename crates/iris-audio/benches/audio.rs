//! iris-audio 混音基准（criterion，R33）。
//!
//! - `mix_32_voices_1s`：32 声道（含 8 个空间声源）44100 Hz 立体声输出
//!   单次 **1024 帧块**——真实音频回调粒度（约 23 ms），决定混音的每块
//!   CPU 预算（超了就是爆音）；
//! - `spatial_gain_10k`：空间增益纯函数 1 万次——衰减 + 恒功率声像的
//!   单源成本，混音器每块每空间声音各调一次，大量声源时的上限参考。
//!
//! 声音全部循环播放：块间状态持续推进（游标回绕），多次迭代语义稳定。
//!
//! ```sh
//! cargo bench -p iris-audio
//! ```

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use glam::Vec3;
use iris_audio::mixer::{AudioMixer, ListenerParams, spatial_gain};
use iris_audio::procedural::sine;

/// 确定性 LCG（与既有测试同款，不引依赖）。
struct Lcg(u64);

impl Lcg {
    fn next_f32(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// 32 声道混音器：24 个非空间 + 8 个空间声源（全部循环、不同频率 / 音量 /
/// 位置，覆盖上混单声道源与空间增益两条路径）。
fn mixer_32_voices() -> AudioMixer {
    let mut mixer = AudioMixer::new();
    mixer.set_listener(ListenerParams::from_view(
        Vec3::new(0.0, 1.5, 0.0),
        -Vec3::Z,
        Vec3::Y,
    ));
    let mut rng = Lcg(0x5EED_2026);
    for i in 0..32u32 {
        let clip = sine(110.0 * (1.0 + (i % 8) as f32), 0.5, 44_100);
        let volume = 0.2 + rng.next_f32() * 0.3;
        if i % 4 == 0 {
            // 每 4 声道 1 个空间声源（32 声道 → 8 个）
            let position = Vec3::new(
                rng.next_f32() * 16.0 - 8.0,
                rng.next_f32() * 2.0,
                rng.next_f32() * 16.0 - 8.0,
            );
            mixer.play_spatial(clip, position, volume, true);
        } else {
            mixer.play(clip, volume, true);
        }
    }
    mixer
}

fn bench_audio(c: &mut Criterion) {
    // ---- 混音：32 声道 × 1024 帧块（44100 Hz 立体声，回调粒度）----
    let mut mixer = mixer_32_voices();
    let mut out = vec![0.0f32; 1024 * 2];
    c.bench_function("mix_32_voices_1s", |b| {
        b.iter(|| {
            mixer.mix(black_box(&mut out), 2, 44_100);
            black_box(out[0])
        })
    });

    // ---- 空间增益：1 万次（不同方位的声源，衰减 + 声像全路径）----
    let listener = ListenerParams::from_view(Vec3::new(0.0, 1.5, 0.0), -Vec3::Z, Vec3::Y);
    let mut rng = Lcg(0xA0D1_0001);
    let sources: Vec<Vec3> = (0..10_000)
        .map(|_| {
            Vec3::new(
                rng.next_f32() * 40.0 - 20.0,
                rng.next_f32() * 8.0 - 2.0,
                rng.next_f32() * 40.0 - 20.0,
            )
        })
        .collect();
    c.bench_function("spatial_gain_10k", |b| {
        b.iter(|| {
            let mut sum = 0.0f32;
            for source in &sources {
                // 总 / 左 / 右三路增益全消费：防止常量折叠
                let (total, left, right) = spatial_gain(&listener, *source, 4.0);
                sum += total + left + right;
            }
            black_box(sum)
        })
    });
}

criterion_group!(benches, bench_audio);
criterion_main!(benches);
