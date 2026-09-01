//! 纯软件混音器：声音表 + 线性叠加 + 线性插值重采样 + 软限幅。
//!
//! 设计约束：**全部纯函数、零设备依赖**——采样推进由调用方驱动（每帧调
//! [`AudioMixer::mix`] 填充输出缓冲），因此混音数学可在单测中确定性断言；
//! 设备层（`device.rs`）只是在实时回调里调用 `mix` 的薄壳。
//!
//! - **混合模型**：各活动声音的采样 × 音量后逐帧线性叠加（f32 加法无削
//!   顶），叠加结果做 `[-1, 1]` 钳制软限幅。选**硬钳制而非 tanh**：tanh
//!   在未超限时也压缩动态（音色失真）且每采样多一次超越函数开销；钳制
//!   对 |x| ≤ 1 的信号完全透明，只在叠加爆表的极端时刻兜底。
//! - **重采样**：声音采样率 ≠ 输出采样率时按 `src_rate / out_rate` 的
//!   步进做线性插值（f64 游标累积，长缓冲无漂移）。v1 不做抗混叠滤波
//!   ——主要场景是 48k ↔ 44.1k 的轻微比率，插值误差可忽略。
//! - **通道映射**：等声道直取；单声道源复制到全部输出通道；输出单声道
//!   时平均全部源通道；其余罕见布局回退为逐通道取 `min(c, 源声道-1)`。
//! - **生命周期**：非循环声音播放到末尾自动移除；循环声音游标回绕。
//!   [`SoundId`] 携带槽位世代，stop 旧句柄安全失配（与 ECS 实体同款防悬垂）。

use crate::clip::AudioClip;

/// 声音句柄：低 32 位槽位下标 + 高 32 位世代。
///
/// 世代随槽位复用递增：stop 一个已失效的旧句柄只会安全失配返回 `false`，
/// 绝不会误伤复用同一槽位的新声音（防悬垂 stop，语义同 `iris_core` 的
/// 实体「索引 + 代数」）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SoundId(u64);

impl SoundId {
    fn new(index: u32, generation: u32) -> Self {
        Self(((generation as u64) << 32) | index as u64)
    }
    fn index(self) -> usize {
        self.0 as u32 as usize
    }
    fn generation(self) -> u32 {
        (self.0 >> 32) as u32
    }
}

/// 一个活动声音：共享剪辑 + 播放状态。
struct Voice {
    clip: AudioClip,
    volume: f32,
    looping: bool,
    /// 源采样游标（单位：源帧；f64 累积以支持非整数的重采样步进）。
    cursor: f64,
}

/// 混音器槽位：世代驻留在槽位上（复用时 +1），声音本体可空。
struct VoiceSlot {
    generation: u32,
    voice: Option<Voice>,
}

/// 纯软件混音器。
///
/// 声音表用「槽位 + 空闲链」而非紧凑 Vec：stop / 自然播完只把槽位置空，
/// 下次 `play` 复用槽位并递增世代——长会话不反复搬移内存，句柄防悬垂。
pub struct AudioMixer {
    slots: Vec<VoiceSlot>,
    free: Vec<u32>,
}

impl Default for AudioMixer {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioMixer {
    /// 创建空混音器。
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }

    /// 播放一段剪辑（立即从开头开始），返回防悬垂句柄。
    /// 音量钳制到 ≥ 0（负音量与 NaN 一律按静音处理）。
    pub fn play(&mut self, clip: AudioClip, volume: f32, looping: bool) -> SoundId {
        let index = match self.free.pop() {
            Some(i) => i as usize,
            None => {
                self.slots.push(VoiceSlot {
                    generation: 0,
                    voice: None,
                });
                self.slots.len() - 1
            }
        };
        let slot = &mut self.slots[index];
        // 世代随每次复用递增（u32 回绕需同一槽位 2^32 次复用，实际不可达）
        slot.generation = slot.generation.wrapping_add(1);
        slot.voice = Some(Voice {
            clip,
            volume: sanitize_volume(volume),
            looping,
            cursor: 0.0,
        });
        SoundId::new(index as u32, slot.generation)
    }

    /// 停止一个声音。句柄有效且声音仍在播放时返回 `true`；
    /// 旧世代句柄 / 已停止 / 已自然播完一律安全返回 `false`。
    pub fn stop(&mut self, id: SoundId) -> bool {
        let Some(slot) = self.slots.get_mut(id.index()) else {
            return false;
        };
        if slot.generation != id.generation() {
            return false; // 槽位已复用：旧句柄失配
        }
        if slot.voice.take().is_some() {
            self.free.push(id.index() as u32);
            true
        } else {
            false // 已自然播完或重复 stop
        }
    }

    /// 调整一个声音的音量（下一次 `mix` 即时生效）。句柄失效返回 `false`。
    pub fn set_volume(&mut self, id: SoundId, volume: f32) -> bool {
        let Some(slot) = self.slots.get_mut(id.index()) else {
            return false;
        };
        if slot.generation != id.generation() {
            return false;
        }
        match &mut slot.voice {
            Some(voice) => {
                voice.volume = sanitize_volume(volume);
                true
            }
            None => false,
        }
    }

    /// 当前活动声音数（含尚未被 `mix` 推进过的）。
    pub fn active_count(&self) -> usize {
        self.slots.iter().filter(|s| s.voice.is_some()).count()
    }

    /// 混音到输出帧缓冲（交错帧，帧数 = `out.len() / channels_out`）。
    ///
    /// 语义为**填充**（先清零再叠加），活动声音按各自采样率线性插值到
    /// `sample_rate_out`；播放完毕的声音在本缓冲渲染后自动移除。
    /// `channels_out` / `sample_rate_out` 为 0 时安全返回（输出保持清零）。
    pub fn mix(&mut self, out: &mut [f32], channels_out: u16, sample_rate_out: u32) {
        out.fill(0.0);
        if channels_out == 0 || sample_rate_out == 0 {
            return;
        }
        let channels_out = channels_out as usize;

        // 逐声音渲染：游标只前进、声音互不干扰
        for slot in &mut self.slots {
            let Some(voice) = &mut slot.voice else {
                continue;
            };
            let Voice {
                clip,
                volume,
                looping,
                cursor,
            } = voice;
            let frames = clip.frame_count();
            if frames == 0 {
                continue; // 空剪辑：随后的移除清扫按「已播完」处理
            }
            let channels_src = clip.channels() as usize;
            let samples = clip.samples();
            let ratio = clip.sample_rate() as f64 / sample_rate_out as f64;
            let ratio = if ratio.is_finite() && ratio > 0.0 {
                ratio
            } else {
                1.0
            };

            for out_frame in out.chunks_exact_mut(channels_out) {
                if *looping {
                    // 循环回绕：保留游标小数部分，相位连续
                    if *cursor >= frames as f64 {
                        *cursor %= frames as f64;
                    }
                } else if *cursor >= frames as f64 {
                    break; // 已播完（本缓冲渲染后由移除清扫回收）
                }
                let i0 = *cursor as usize;
                let frac = (*cursor - i0 as f64) as f32;
                // 线性插值的右邻：末端非循环钳到最后一帧，循环回绕到帧 0
                let i1 = if i0 + 1 < frames {
                    i0 + 1
                } else if *looping {
                    0
                } else {
                    frames - 1
                };
                for (c, dst) in out_frame.iter_mut().enumerate() {
                    let value = if channels_src == channels_out {
                        lerp_channel(samples, channels_src, i0, i1, frac, c)
                    } else if channels_src == 1 {
                        // 上混：单声道源复制到全部输出通道
                        lerp_channel(samples, channels_src, i0, i1, frac, 0)
                    } else if channels_out == 1 {
                        // 下混：平均全部源通道
                        let mut sum = 0.0;
                        for k in 0..channels_src {
                            sum += lerp_channel(samples, channels_src, i0, i1, frac, k);
                        }
                        sum / channels_src as f32
                    } else {
                        // 罕见布局回退：逐输出通道取 min(c, 源声道-1)
                        lerp_channel(samples, channels_src, i0, i1, frac, c.min(channels_src - 1))
                    };
                    *dst += value * *volume;
                }
                *cursor += ratio;
            }
        }

        // 移除清扫：播完的声音回收槽位（游标越过末帧或空剪辑）。
        // 用下标遍历避免 slots 与 free 同时可变借用。
        for index in 0..self.slots.len() {
            let done = matches!(
                &self.slots[index].voice,
                Some(voice) if !voice.looping && voice.cursor >= voice.clip.frame_count() as f64
            );
            if done {
                self.slots[index].voice = None;
                self.free.push(index as u32);
            }
        }

        // 软限幅（硬钳制，见模块注释）：对未超限信号完全透明
        for sample in out.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }
    }
}

/// 源通道 `k` 在游标处的线性插值采样。
fn lerp_channel(
    samples: &[f32],
    channels: usize,
    i0: usize,
    i1: usize,
    frac: f32,
    k: usize,
) -> f32 {
    let a = samples[i0 * channels + k];
    let b = samples[i1 * channels + k];
    a + (b - a) * frac
}

/// 音量卫生：钳到 ≥ 0，NaN / 无穷按静音处理（混音路径不传播病态值）。
fn sanitize_volume(volume: f32) -> f32 {
    if volume.is_finite() {
        volume.max(0.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单声道测试剪辑。
    fn mono(samples: &[f32]) -> AudioClip {
        AudioClip::from_interleaved(samples.to_vec(), 1, 48_000).expect("测试剪辑合法")
    }

    /// 单声音 volume 1.0、采样率一致：混音输出 == 剪辑采样（逐采样相等）。
    #[test]
    fn single_voice_passthrough_is_exact() {
        const SRC: [f32; 6] = [0.1, -0.2, 0.3, -0.4, 0.5, -0.6];
        let mut mixer = AudioMixer::new();
        mixer.play(mono(&SRC), 1.0, false);
        let mut out = [0.0f32; SRC.len()];
        mixer.mix(&mut out, 1, 48_000);
        assert_eq!(out, SRC, "采样率一致时应逐采样相等（插值退化为直通）");
        // 再次混入更长的缓冲：声音已播完，输出全静音
        let mut out = [9.0f32; 8];
        mixer.mix(&mut out, 1, 48_000);
        assert_eq!(out, [0.0; 8], "mix 为填充语义（清零后叠加）");
    }

    /// volume 0.5 输出减半；set_volume 即时生效（同一缓冲内前后对比）。
    #[test]
    fn volume_scaling_and_immediate_set_volume() {
        let src = [0.8, -0.6, 0.4, -0.2];
        let mut mixer = AudioMixer::new();
        mixer.play(mono(&src), 0.5, false);
        let mut out = [0.0f32; 4];
        mixer.mix(&mut out, 1, 48_000);
        for (got, want) in out.iter().zip(src) {
            assert!((got - want * 0.5).abs() < 1e-6, "{got} vs {}", want * 0.5);
        }

        // 重新播放并在中途调音量：前半 0.5 倍，后半 0.25 倍
        let id = mixer.play(mono(&src), 0.5, false);
        let mut out = [0.0f32; 2];
        mixer.mix(&mut out, 1, 48_000);
        assert!(mixer.set_volume(id, 0.25), "活声音上 set_volume 应成功");
        let mut rest = [0.0f32; 2];
        mixer.mix(&mut rest, 1, 48_000);
        for (got, want) in rest.iter().zip(&src[2..]) {
            assert!((got - want * 0.25).abs() < 1e-6);
        }
        // 病态音量被卫生化：负音量 = 静音（只混半程，声音仍在表中）
        let id = mixer.play(mono(&src), -3.0, false);
        let mut out = [0.0f32; 2];
        mixer.mix(&mut out, 1, 48_000);
        assert!(out.iter().all(|s| *s == 0.0));
        assert!(mixer.stop(id));
    }

    /// 两声音线性叠加：混音输出 == 各自单独混音之和（相位对齐构造）。
    #[test]
    fn two_voices_sum_linearly() {
        let a = [0.5f32, -0.25, 0.1, 0.75];
        let b = [0.25f32, 0.25, -0.4, -0.5];

        let mut mixed = AudioMixer::new();
        mixed.play(mono(&a), 1.0, false);
        mixed.play(mono(&b), 1.0, false);
        let mut out = [0.0f32; 4];
        mixed.mix(&mut out, 1, 48_000);

        // 参照：两声音各自由独立混音器单独混音
        let mut only_a = AudioMixer::new();
        only_a.play(mono(&a), 1.0, false);
        let mut pa = [0.0f32; 4];
        only_a.mix(&mut pa, 1, 48_000);
        let mut only_b = AudioMixer::new();
        only_b.play(mono(&b), 1.0, false);
        let mut pb = [0.0f32; 4];
        only_b.mix(&mut pb, 1, 48_000);

        for i in 0..4 {
            assert_eq!(out[i], pa[i] + pb[i], "线性叠加应可分解（i={i}）");
            assert!((out[i] - (a[i] + b[i])).abs() < 1e-6);
        }
    }

    /// 非循环声音播放完毕后自动移除（active_count 归零、槽位可复用）。
    #[test]
    fn finished_voices_are_removed_automatically() {
        let mut mixer = AudioMixer::new();
        let id = mixer.play(mono(&[0.5; 3]), 1.0, false);
        assert_eq!(mixer.active_count(), 1);
        let mut out = [0.0f32; 3];
        mixer.mix(&mut out, 1, 48_000);
        assert_eq!(out, [0.5, 0.5, 0.5]);
        assert_eq!(mixer.active_count(), 0, "越过末帧后应自动移除");
        assert!(!mixer.stop(id), "自然播完后 stop 应返回 false");

        // 空闲槽位复用：新声音拿到同一槽位、世代递增
        let id2 = mixer.play(mono(&[0.5; 3]), 1.0, false);
        assert_ne!(id, id2, "世代必须变化");
        assert_eq!(mixer.active_count(), 1);
    }

    /// 循环声音游标回绕：播放时长 > 剪辑时长后输出仍按相位重复，可 stop。
    #[test]
    fn looping_wraps_around() {
        let mut mixer = AudioMixer::new();
        let id = mixer.play(mono(&[0.1, 0.2, 0.3, 0.4]), 1.0, true);
        let mut out = [0.0f32; 10];
        mixer.mix(&mut out, 1, 48_000);
        assert_eq!(out, [0.1, 0.2, 0.3, 0.4, 0.1, 0.2, 0.3, 0.4, 0.1, 0.2]);
        assert_eq!(mixer.active_count(), 1, "循环声音永不自动移除");
        assert!(mixer.stop(id), "循环声音可 stop");
        let mut out = [1.0f32; 4];
        mixer.mix(&mut out, 1, 48_000);
        assert_eq!(out, [0.0; 4], "stop 后循环声音立即静音");
    }

    /// stop 后输出静音；旧世代句柄绝不误伤复用槽位的新声音。
    #[test]
    fn stop_mutes_and_generation_guard_is_dangling_safe() {
        let mut mixer = AudioMixer::new();
        let first = mixer.play(mono(&[0.5; 4]), 1.0, false);
        assert!(mixer.stop(first), "有效句柄 stop 应成功");
        let mut out = [1.0f32; 4];
        mixer.mix(&mut out, 1, 48_000);
        assert_eq!(out, [0.0; 4], "stop 后输出静音");
        assert!(!mixer.stop(first), "重复 stop 应返回 false");

        // 槽位复用 + 世代失配：旧句柄 stop/set_volume 均不生效
        let second = mixer.play(mono(&[0.25; 4]), 1.0, false);
        assert_ne!(first, second);
        assert!(!mixer.stop(first), "旧世代句柄必须失配");
        assert!(!mixer.set_volume(first, 0.0), "旧世代句柄调音量也必须失配");
        assert_eq!(mixer.active_count(), 1);
        let mut out = [0.0f32; 2];
        mixer.mix(&mut out, 1, 48_000);
        assert_eq!(out, [0.25, 0.25], "新声音不应被旧句柄误伤");
    }

    /// 通道映射：单声道→立体声复制、立体声→单声道平均、立体声→立体声直通。
    #[test]
    fn channel_mapping_upmix_downmix_and_passthrough() {
        // 单声道 → 立体声：复制
        let mut mixer = AudioMixer::new();
        mixer.play(mono(&[0.5, -0.5]), 1.0, false);
        let mut out = [0.0f32; 4];
        mixer.mix(&mut out, 2, 48_000);
        assert_eq!(out, [0.5, 0.5, -0.5, -0.5]);

        // 立体声 → 立体声：直通
        let stereo =
            AudioClip::from_interleaved(vec![0.5, -0.25, -1.0, 0.75], 2, 48_000).expect("合法");
        let mut mixer = AudioMixer::new();
        mixer.play(stereo, 1.0, false);
        let mut out = [0.0f32; 4];
        mixer.mix(&mut out, 2, 48_000);
        assert_eq!(out, [0.5, -0.25, -1.0, 0.75]);

        // 立体声 → 单声道：平均
        let stereo =
            AudioClip::from_interleaved(vec![0.5, -0.5, -1.0, 1.0], 2, 48_000).expect("合法");
        let mut mixer = AudioMixer::new();
        mixer.play(stereo, 1.0, false);
        let mut out = [0.0f32; 2];
        mixer.mix(&mut out, 1, 48_000);
        assert_eq!(out, [0.0, 0.0], "L/R 相反时下混应完全抵消");
    }

    /// 重采样：440Hz 剪辑以 2 倍采样率混出，过零点数与解析解一致。
    #[test]
    fn resampling_to_double_rate_preserves_zero_crossings() {
        let zero_crossings = |s: &[f32]| {
            s.windows(2)
                .filter(|w| (w[0] > 0.0) != (w[1] > 0.0))
                .count()
        };
        // 24kHz 源 → 48kHz 输出（比率 0.5 步进的线性插值）
        let mut mixer = AudioMixer::new();
        mixer.play(crate::procedural::sine(440.0, 0.5, 24_000), 1.0, false);
        let mut out = vec![0.0f32; 24_000];
        mixer.mix(&mut out, 1, 48_000);
        assert_eq!(mixer.active_count(), 0, "0.5s 后应播完");
        // 参照：直接以 48kHz 生成的解析信号
        let direct = crate::procedural::sine(440.0, 0.5, 48_000);
        let (a, b) = (zero_crossings(&out), zero_crossings(direct.samples()));
        // 440Hz × 0.5s = 440 个真实过零点；淡入淡出边界 + 浮点正弦在零点
        // 附近的 ±1 抖动用小窗口吸收，重采样不应改变过零点结构
        assert!((439..=443).contains(&a), "上混输出的过零点数 {a}");
        assert!((439..=443).contains(&b), "参照信号的过零点数 {b}");
        assert!(
            (a as i32 - b as i32).abs() <= 2,
            "重采样不应改变过零点数（{a} vs {b}）"
        );
    }

    /// 软限幅：两路满幅声音叠加输出钳制在 [-1, 1]，不产生爆表采样。
    #[test]
    fn limiter_clamps_full_scale_sum() {
        let mut mixer = AudioMixer::new();
        let full = mono(&[1.0; 8]);
        let deep = mono(&[-1.0; 8]);
        mixer.play(full.clone(), 1.0, false);
        mixer.play(full, 1.0, false);
        mixer.play(deep.clone(), 0.5, false);
        let mut out = [0.0f32; 8];
        mixer.mix(&mut out, 1, 48_000);
        assert!(
            out.iter().all(|s| *s == 1.0),
            "2.0 叠加应钳制为 1.0：{out:?}"
        );
        assert!(out.iter().all(|s| s.abs() <= 1.0));

        let mut mixer = AudioMixer::new();
        mixer.play(deep.clone(), 1.0, false);
        mixer.play(deep, 1.0, false);
        let mut out = [0.0f32; 8];
        mixer.mix(&mut out, 1, 48_000);
        assert!(out.iter().all(|s| *s == -1.0), "-2.0 叠加应钳制为 -1.0");
    }

    /// 参数防御：零声道 / 零采样率安全返回（输出清零），空混音器同样。
    #[test]
    fn degenerate_parameters_are_safe() {
        let mut mixer = AudioMixer::new();
        let mut out = [9.0f32; 6];
        mixer.mix(&mut out, 0, 48_000);
        assert_eq!(out, [0.0; 6]);
        mixer.mix(&mut out, 1, 0);
        assert_eq!(out, [0.0; 6]);
        mixer.mix(&mut out, 2, 48_000); // 无声音：输出保持清零
        assert_eq!(out, [0.0; 6]);
    }
}
