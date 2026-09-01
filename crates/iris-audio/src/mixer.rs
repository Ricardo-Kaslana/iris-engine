//! 纯软件混音器：声音表 + 线性叠加 + 线性插值重采样 + 软限幅 + 空间化 v1。
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
//! - **空间化 v1**（距离衰减 + 恒功率立体声声像）：[`AudioMixer::play_spatial`]
//!   在槽位上额外记 `spatial` 标记与声源世界坐标，[`AudioMixer::mix`] 每帧按
//!   **当帧听者位姿**（[`AudioMixer::set_listener`]）重算增益——移动听者或
//!   声源（[`AudioMixer::set_sound_position`]）下一个混音缓冲即生效。增益
//!   模型见 [`spatial_gain`]（纯函数、单独导出可单测）：
//!   - 距离衰减 `att = ref / (ref + d)`：软倒数（d = 0 → 1，远处平滑趋零、
//!     无硬截断），`ref` 为距离参考（米，默认 [`DEFAULT_REFERENCE_DISTANCE`]，
//!     [`AudioMixer::set_reference_distance`] 可配）；
//!   - 恒功率声像：`pan = clamp(dot(normalize(源 − 听者), 听者右轴), −1, 1)`，
//!     `θ = (pan + 1) × π/4`，左 = `att·cos θ`、右 = `att·sin θ`（pan = −1
//!     全左、0 居中 0.707/0.707、+1 全右，总功率不随声像变化）；
//!   - **立体声剪辑 v1 只乘总增益**、不做逐声道声像（源本身已有左右内容，
//!     再做声像会叠加二次定位，v1 选择保真原样）；
//!   - **已知限制——身后不消音**：无 HRTF / 耳间延迟差，源在正后方与正前方
//!     对称（pan 同为 0、衰减只随距离）——前后靠本模型不可区分，仅左右与
//!     远近有空间线索；非空间 `play` 恒为居中声像、无衰减，听者位姿不影响。

use glam::Vec3;

use crate::clip::AudioClip;

/// 空间声音的默认距离参考（米）：`att = ref / (ref + d)` 在 d = ref 处衰减
/// 恰为一半——4 m 适合第一人称 / 角色脚边尺度的声源；俯视相机等悬空听者
/// 应按场域尺度放大（[`AudioMixer::set_reference_distance`]）。
pub const DEFAULT_REFERENCE_DISTANCE: f32 = 4.0;

/// 听者位姿：世界坐标位置 + 前方 / 右轴单位向量。
///
/// **契约**：`forward` / `right` 必须为单位向量且相互正交，由调用方保证
/// （手搓位姿不满足时声像会失真但不会 panic——投影被钳制在 [-1, 1]）；
/// 用 [`ListenerParams::from_view`] 构造可自动正交化。默认位姿与引擎右手系
/// 一致：原点、前方 -Z、右轴 +X。v1 的声像只依赖 `right`（源方向在右轴上
/// 的投影），`forward` 参与语义表达、为 HRTF 等后续扩展占位。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ListenerParams {
    /// 世界坐标位置。
    pub position: Vec3,
    /// 前方单位向量（调用方保证单位、与 `right` 正交）。
    pub forward: Vec3,
    /// 右轴单位向量（调用方保证单位、与 `forward` 正交）。
    pub right: Vec3,
}

impl Default for ListenerParams {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            forward: Vec3::NEG_Z,
            right: Vec3::X,
        }
    }
}

impl ListenerParams {
    /// 由「位置 + 观察方向 + 世界-up」构造，自动正交化：
    /// `right = normalize(forward × up)`，再把 `forward` 重正交化为
    /// `up × right`（严格单位、与 right 正交）。典型用法是把实体 / 相机的
    /// 前方向量与世界上方喂进来——俯仰俯视的相机经此得到**水平**右轴，
    /// 声像只随水平方位摆动（俯视游戏空间音的合理姿态）。
    ///
    /// 退化输入（forward 与 up 共线）时 right 兜底为 +X、forward 兜底为
    /// 全零向量（不产生 NaN；声像退化为居中）。
    pub fn from_view(position: Vec3, forward: Vec3, up: Vec3) -> Self {
        let forward = forward.normalize_or_zero();
        let up = up.normalize_or_zero();
        let mut right = forward.cross(up);
        right = if right.length_squared() > 1e-8 {
            right.normalize()
        } else {
            Vec3::X // forward ∥ up：右轴无定义，兜底 +X
        };
        let forward = up.cross(right).normalize_or_zero();
        Self {
            position,
            forward,
            right,
        }
    }
}

/// 空间增益纯函数：源在给定听者位姿下的 `(总增益, 左增益, 右增益)`。
///
/// - 总增益 = 距离衰减 `att = ref / (ref + d)`（软倒数：d = 0 → 1，远处
///   平滑趋零、无硬截断）——调用方再乘音量得最终总增益；
/// - 左 / 右增益 = `att × 恒功率声像项`：`pan = clamp(dot(单位源方向,
///   listener.right), −1, 1)`、`θ = (pan + 1) × π/4`，左 = `cos θ`、
///   右 = `sin θ`。pan = −1 全左、0 居中（0.707 / 0.707，总功率守恒）、
///   +1 全右；
/// - 源与听者**重叠或位于正上 / 正下**（源方向在右轴上无投影）时
///   pan = 0 居中；
/// - `reference_distance` 非有限或 ≤ 0 时按
///   [`DEFAULT_REFERENCE_DISTANCE`] 兜底（混音路径不传播病态值）。
///
/// 单独导出：衰减与声像的解析行为可脱离混音器单测（ref = 4、d = 0 / 4 / 12
/// → att = 1 / 0.5 / 0.25 等，见本模块测试）。
pub fn spatial_gain(
    listener: &ListenerParams,
    source: Vec3,
    reference_distance: f32,
) -> (f32, f32, f32) {
    let reference = if reference_distance.is_finite() && reference_distance > 0.0 {
        reference_distance
    } else {
        DEFAULT_REFERENCE_DISTANCE
    };
    let offset = source - listener.position;
    let distance = offset.length();
    let attenuation = reference / (reference + distance);
    // 声像：单位源方向在听者右轴上的投影；零向量（重叠 / 正上）→ 居中
    let pan = if distance > 0.0 {
        (offset / distance).dot(listener.right).clamp(-1.0, 1.0)
    } else {
        0.0
    };
    // 恒功率：pan ∈ [-1, 1] 线性映射到相位 [0, π/2]（左 = cos、右 = sin）
    let theta = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
    (
        attenuation,
        attenuation * theta.cos(),
        attenuation * theta.sin(),
    )
}

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

/// 一个活动声音：共享剪辑 + 播放状态（+ 空间化 v1 的声源定位字段）。
struct Voice {
    clip: AudioClip,
    volume: f32,
    looping: bool,
    /// 源采样游标（单位：源帧；f64 累积以支持非整数的重采样步进）。
    cursor: f64,
    /// 是否空间化播放（`false` = 居中声像、无衰减，听者位姿不影响）。
    spatial: bool,
    /// 声源世界坐标（每帧 mix 按当帧听者位姿重算增益；`set_sound_position`
    /// 可在播放中移动）。非空间声音不读此字段。
    position: Vec3,
    /// 距离参考（米）：`play_spatial` 时取混音器当前默认值，
    /// [`AudioMixer::set_sound_reference_distance`] 可按声音覆写。
    reference_distance: f32,
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
    /// 当帧听者位姿（mix 时对空间声音重算增益）。
    listener: ListenerParams,
    /// 空间声音的距离参考默认值（米），`play_spatial` 时固化进声音槽位。
    reference_distance: f32,
}

impl Default for AudioMixer {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioMixer {
    /// 创建空混音器（听者默认：原点、前方 -Z、右轴 +X）。
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            listener: ListenerParams::default(),
            reference_distance: DEFAULT_REFERENCE_DISTANCE,
        }
    }

    /// 播放一段剪辑（立即从开头开始），返回防悬垂句柄。
    /// 音量钳制到 ≥ 0（负音量与 NaN 一律按静音处理）。
    pub fn play(&mut self, clip: AudioClip, volume: f32, looping: bool) -> SoundId {
        self.play_internal(clip, volume, looping, false, Vec3::ZERO)
    }

    /// 空间化播放：声音定位在世界坐标 `position`，每帧混音按当帧听者位姿
    /// 施加距离衰减 + 恒功率立体声声像（模型见 [`spatial_gain`] 与模块
    /// 文档）。距离参考取混音器当前默认值
    /// （[`AudioMixer::set_reference_distance`]），可按声音覆写
    /// （[`AudioMixer::set_sound_reference_distance`]）。
    pub fn play_spatial(
        &mut self,
        clip: AudioClip,
        position: Vec3,
        volume: f32,
        looping: bool,
    ) -> SoundId {
        self.play_internal(clip, volume, looping, true, position)
    }

    /// 播放入表公共路径：槽位取用 + 世代递增，空间字段按 `spatial` 就位。
    fn play_internal(
        &mut self,
        clip: AudioClip,
        volume: f32,
        looping: bool,
        spatial: bool,
        position: Vec3,
    ) -> SoundId {
        let reference_distance = self.reference_distance;
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
            spatial,
            position,
            reference_distance,
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

    /// 更新听者位姿（下一帧 `mix` 对全部空间声音生效）。
    /// 位姿契约见 [`ListenerParams`]；非空间声音不受影响。
    pub fn set_listener(&mut self, listener: ListenerParams) {
        self.listener = listener;
    }

    /// 当前听者位姿（测试与调试用）。
    pub fn listener(&self) -> ListenerParams {
        self.listener
    }

    /// 设置空间声音的距离参考默认值（米；后续 `play_spatial` 生效，
    /// 已在播的声音不受影响）。非有限或 ≤ 0 的输入被忽略（保持原值）。
    pub fn set_reference_distance(&mut self, reference_distance: f32) {
        if reference_distance.is_finite() && reference_distance > 0.0 {
            self.reference_distance = reference_distance;
        }
    }

    /// 移动一个空间声音的声源位置（下一帧 `mix` 按新位置重算增益）。
    /// 句柄失效返回 `false`；非空间声音存而不用（恒居中、无衰减）。
    pub fn set_sound_position(&mut self, id: SoundId, position: Vec3) -> bool {
        let Some(slot) = self.slots.get_mut(id.index()) else {
            return false;
        };
        if slot.generation != id.generation() {
            return false;
        }
        match &mut slot.voice {
            Some(voice) => {
                voice.position = position;
                true
            }
            None => false,
        }
    }

    /// 覆写一个空间声音的距离参考（米）。句柄失效返回 `false`；
    /// 非有限或 ≤ 0 的输入按 [`DEFAULT_REFERENCE_DISTANCE`] 兜底。
    pub fn set_sound_reference_distance(&mut self, id: SoundId, reference_distance: f32) -> bool {
        let reference = if reference_distance.is_finite() && reference_distance > 0.0 {
            reference_distance
        } else {
            DEFAULT_REFERENCE_DISTANCE
        };
        let Some(slot) = self.slots.get_mut(id.index()) else {
            return false;
        };
        if slot.generation != id.generation() {
            return false;
        }
        match &mut slot.voice {
            Some(voice) => {
                voice.reference_distance = reference;
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
        // 听者位姿拷出：混音循环持有 slots 的可变借用
        let listener = self.listener;

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
                spatial,
                position,
                reference_distance,
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

            // 每声音的声道增益因子（空间增益每声音每缓冲算一次，非每采样）：
            // - 非空间：三路因子同为音量（居中、无衰减）；
            // - 空间：spatial_gain 返回值已含距离衰减（总 = att、左/右 =
            //   att × 恒功率项），最终因子 = 音量 × 对应增益；立体声输出
            //   时左右按声像拆分（仅单声道源），其余输出布局无左右可言、
            //   退化为总增益；立体声源 v1 只乘总增益（见模块文档）。
            let (factor_l, factor_r, factor_mono) = if *spatial {
                let (total, left, right) = spatial_gain(&listener, *position, *reference_distance);
                let (total, left, right) = (*volume * total, *volume * left, *volume * right);
                if channels_src == 1 {
                    (left, right, total)
                } else {
                    (total, total, total)
                }
            } else {
                let volume = *volume;
                (volume, volume, volume)
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
                    let factor = if channels_out == 2 {
                        if c == 0 { factor_l } else { factor_r }
                    } else {
                        factor_mono
                    };
                    *dst += value * factor;
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

    // ---- 空间化 v1（spatial_gain 解析 + 混音路径）----

    /// 衰减解析：ref = 4、d = 0 / 4 / 12 → att = 1 / 0.5 / 0.25（软倒数
    /// `ref / (ref + d)`，d = ref 处恰为一半）。取正前方声源使 pan = 0，
    /// 总增益即纯衰减项（4 / 8 / 16 均为 2 的幂，f32 精确表示）。
    #[test]
    fn attenuation_analytic_reference_distances() {
        let listener = ListenerParams::default(); // 原点、右轴 +X
        for (d, want) in [(0.0, 1.0), (4.0, 0.5), (12.0, 0.25)] {
            let (total, left, right) = spatial_gain(&listener, Vec3::new(0.0, 0.0, -d), 4.0);
            assert_eq!(total, want, "ref=4 d={d} 应衰减为 {want}");
            // 前方：恒功率居中项 0.7071（±1e-4 容差，π/4 非精确可表示）
            assert!((left - want * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-4);
            assert!((right - want * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-4);
        }
        // 病态参考距离兜底为默认 4 m（混音路径不传播病态值）
        for bad in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let (total, _, _) = spatial_gain(&listener, Vec3::new(0.0, 0.0, -4.0), bad);
            assert_eq!(total, 0.5, "病态 ref 应按默认 4 m 计算");
        }
    }

    /// 恒功率声像：正右 → (0, 总)、正左 → (总, 0)、正前 → 居中均分、
    /// 重叠 → 居中；d = 2·ref 时总增益恒为 1/3，逐项对照。
    #[test]
    fn constant_power_panning_sides_front_and_overlap() {
        let listener = ListenerParams::default(); // 原点、右轴 +X、前方 -Z
        // 正右 / 正左：d = 8、ref = 4 → att = 1/3
        let (total, left, right) = spatial_gain(&listener, Vec3::new(8.0, 0.0, 0.0), 4.0);
        assert_eq!(total, 1.0 / 3.0);
        assert!(left.abs() < 1e-4, "正右方左耳应无声，实际 {left}");
        assert!((right - 1.0 / 3.0).abs() < 1e-4, "正右方右耳应得全额增益");

        let (total, left, right) = spatial_gain(&listener, Vec3::new(-8.0, 0.0, 0.0), 4.0);
        assert_eq!(total, 1.0 / 3.0);
        assert!((left - 1.0 / 3.0).abs() < 1e-4, "正左方左耳应得全额增益");
        assert!(right.abs() < 1e-4, "正左方右耳应无声，实际 {right}");

        // 正前 / 重叠：居中 0.7071 / 0.7071；重叠时 att = 1
        let (total, left, right) = spatial_gain(&listener, Vec3::new(0.0, 0.0, -8.0), 4.0);
        assert_eq!(total, 1.0 / 3.0);
        assert!((left - total * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-4);
        assert!((right - total * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-4);

        let (total, left, right) = spatial_gain(&listener, Vec3::ZERO, 4.0);
        assert_eq!(total, 1.0, "重叠时距离为零、无衰减");
        assert!((left - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-4);
        assert!((right - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-4);

        // 正上 / 正下：源方向与右轴正交 → 同样居中（衰减只随高度）
        let (total, left, right) = spatial_gain(&listener, Vec3::new(0.0, 8.0, 0.0), 4.0);
        assert_eq!(total, 1.0 / 3.0);
        assert!((left - total * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-4);
        assert!((right - total * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-4);
    }

    /// 听者位姿辅助：from_view 正交化（俯仰相机 → 水平右轴）、默认位姿
    /// 与引擎约定一致、退化输入不产生 NaN。
    #[test]
    fn listener_params_from_view_orthogonalizes() {
        let default = ListenerParams::default();
        assert_eq!(default.position, Vec3::ZERO);
        assert_eq!(default.forward, Vec3::NEG_Z, "默认前方 -Z（引擎右手系）");
        assert_eq!(default.right, Vec3::X, "默认右轴 +X");

        // 俯视 3/4 机位（Arena 相机同款）：-59° 俯仰的前方经 from_view
        // 得到水平右轴 +X、水平前方 -Z——声像只随水平方位摆动
        let pitch = -59.0_f32.to_radians();
        let forward = Vec3::new(0.0, pitch.sin(), -pitch.cos());
        let pose = ListenerParams::from_view(Vec3::new(1.0, 20.0, 12.0), forward, Vec3::Y);
        assert_eq!(pose.position, Vec3::new(1.0, 20.0, 12.0));
        assert!(pose.right.x > 0.99, "俯仰相机的右轴应回正为水平 +X");
        assert!(pose.right.y.abs() < 1e-4 && pose.right.z.abs() < 1e-4);
        assert!(
            (pose.forward.length() - 1.0).abs() < 1e-4,
            "前方应为单位向量"
        );
        assert!(
            pose.forward.dot(pose.right).abs() < 1e-4,
            "前方应与右轴正交"
        );

        // 朝向 -X（绕 Y +90°）：右轴应转到 -Z
        let pose = ListenerParams::from_view(Vec3::ZERO, -Vec3::X, Vec3::Y);
        assert!((pose.right + Vec3::Z).length() < 1e-4);

        // 退化输入（forward ∥ up）：不 panic、不产生 NaN
        let pose = ListenerParams::from_view(Vec3::ZERO, Vec3::Y, Vec3::Y);
        assert!(pose.forward.is_finite() && pose.right.is_finite());
    }

    /// 混音路径增益合成：volume × att × 恒功率项逐项对照（单声道源 →
    /// 立体声输出，确定性采样 + 精确数值断言）。
    #[test]
    fn spatial_mix_composes_volume_attenuation_and_pan() {
        let src = [0.5f32, -0.25, 0.75, -1.0];
        // 正前方 d = 4、ref 默认 4 → att = 0.5；居中项 0.7071
        let mut mixer = AudioMixer::new();
        mixer.play_spatial(mono(&src), Vec3::new(0.0, 0.0, -4.0), 0.8, false);
        let mut out = [0.0f32; 8];
        mixer.mix(&mut out, 2, 48_000);
        let center = 0.8 * 0.5 * std::f32::consts::FRAC_1_SQRT_2;
        for (i, frame) in out.chunks_exact(2).enumerate() {
            assert!(
                (frame[0] - src[i] * center).abs() < 1e-4,
                "左声道 {} vs {}",
                frame[0],
                src[i] * center
            );
            assert!(
                (frame[1] - src[i] * center).abs() < 1e-4,
                "右声道 {}",
                frame[1]
            );
        }

        // 正右方 d = 8、ref = 4 → att = 1/3；左 ≈ 0、右 = 源 × 0.8 × 1/3
        let mut mixer = AudioMixer::new();
        mixer.set_reference_distance(4.0);
        mixer.play_spatial(mono(&src), Vec3::new(8.0, 0.0, 0.0), 0.8, false);
        let mut out = [0.0f32; 8];
        mixer.mix(&mut out, 2, 48_000);
        for (i, frame) in out.chunks_exact(2).enumerate() {
            assert!(frame[0].abs() < 1e-4, "正右方左声道应无声：{}", frame[0]);
            let want = src[i] * 0.8 / 3.0;
            assert!(
                (frame[1] - want).abs() < 1e-4,
                "右声道 {} vs {want}",
                frame[1]
            );
        }

        // 移动声源（set_sound_position）下一缓冲即生效：挪到正左方后镜像
        let id = mixer.play_spatial(mono(&src), Vec3::new(8.0, 0.0, 0.0), 0.8, true);
        assert!(mixer.set_sound_position(id, Vec3::new(-8.0, 0.0, 0.0)));
        let mut out = [0.0f32; 8];
        mixer.mix(&mut out, 2, 48_000);
        for frame in out.chunks_exact(2) {
            assert!(
                frame[1].abs() < 1e-4,
                "挪到正左后右声道应无声：{}",
                frame[1]
            );
        }
        assert!(mixer.stop(id), "循环声音应可 stop 回收");
    }

    /// 空间混音确定性：同配置两个混音器逐位一致（听者位姿参与增益计算，
    /// 不引入任何不确定源）。
    #[test]
    fn spatial_mix_is_bit_deterministic() {
        let run = || {
            let mut mixer = AudioMixer::new();
            mixer.set_listener(ListenerParams::from_view(
                Vec3::new(0.5, 0.25, 0.75),
                -Vec3::Z,
                Vec3::Y,
            ));
            mixer.play_spatial(
                mono(&[0.5, -0.5, 0.25, 0.75]),
                Vec3::new(3.0, 1.0, -2.0),
                0.7,
                false,
            );
            mixer.play_spatial(
                AudioClip::from_interleaved(vec![0.4, -0.4, -0.6, 0.6], 2, 48_000).expect("合法"),
                Vec3::new(-2.0, 0.0, -5.0),
                0.5,
                true,
            );
            let mut out = vec![0.0f32; 64];
            mixer.mix(&mut out, 2, 48_000);
            out.iter().map(|s| s.to_bits()).collect::<Vec<_>>()
        };
        assert_eq!(run(), run(), "同配置两次混音应逐位一致");
    }

    /// 世代防悬垂：槽位复用后旧句柄的 set_sound_position /
    /// set_sound_reference_distance 必须失配返回 `false`，不误伤新声音。
    /// （循环声音跨 mix 存活，保证断言不因自然播完而失真。）
    #[test]
    fn set_sound_position_is_generation_guarded() {
        let mut mixer = AudioMixer::new();
        let first = mixer.play_spatial(mono(&[0.5; 4]), Vec3::new(8.0, 0.0, 0.0), 1.0, true);
        assert!(mixer.stop(first), "有效句柄 stop 应成功");
        let second = mixer.play_spatial(mono(&[0.5; 4]), Vec3::ZERO, 1.0, true);
        assert_ne!(first, second, "槽位复用必须换代");
        assert!(
            !mixer.set_sound_position(first, Vec3::ONE),
            "旧世代句柄移动声源应失配"
        );
        assert!(
            !mixer.set_sound_reference_distance(first, 8.0),
            "旧世代句柄改距离参考应失配"
        );
        // 新声音不受旧句柄影响：居中声像、参考距离仍为默认 4
        let mut out = [0.0f32; 8];
        mixer.mix(&mut out, 2, 48_000);
        for frame in out.chunks_exact(2) {
            let want = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
            assert!((frame[0] - want).abs() < 1e-4 && (frame[1] - want).abs() < 1e-4);
        }
        assert!(mixer.set_sound_position(second, Vec3::new(4.0, 0.0, 0.0)));
        assert!(mixer.set_sound_reference_distance(second, 8.0));
        assert!(mixer.stop(second), "收尾 stop 回收循环声音");
    }

    /// play_spatial 后左右声道能量比与声源方位一致（重叠加权后仍可分辨）。
    #[test]
    fn spatial_mix_channel_energies_match_source_direction() {
        let clip = crate::procedural::sine(440.0, 0.05, 48_000);
        let energy = |out: &[f32]| -> (f32, f32) {
            let mut pair = (0.0, 0.0);
            for frame in out.chunks_exact(2) {
                pair.0 += frame[0] * frame[0];
                pair.1 += frame[1] * frame[1];
            }
            pair
        };
        // 源在右侧：右声道能量显著大于左
        let mut mixer = AudioMixer::new();
        mixer.play_spatial(clip.clone(), Vec3::new(12.0, 0.0, 0.0), 1.0, true);
        let mut out = vec![0.0f32; 4_800];
        mixer.mix(&mut out, 2, 48_000);
        let (left, right) = energy(&out);
        assert!(
            right > left * 100.0,
            "右侧声源右声道能量应占绝对主导 {left} vs {right}"
        );

        // 镜像源在左侧：能量比反转（同混音器复用，只换位置重播）
        let mut mixer = AudioMixer::new();
        mixer.play_spatial(clip, Vec3::new(-12.0, 0.0, 0.0), 1.0, true);
        let mut out = vec![0.0f32; 4_800];
        mixer.mix(&mut out, 2, 48_000);
        let (left, right) = energy(&out);
        assert!(
            left > right * 100.0,
            "左侧声源左声道能量应占绝对主导 {left} vs {right}"
        );
    }

    /// 立体声剪辑的空间槽位 v1 只乘总增益、不做逐声道声像（语义锁定：
    /// 换到任一侧方位，输出左右比例仍与源一致）。
    #[test]
    fn stereo_clip_spatial_applies_total_gain_only() {
        // 源本身左右不对称：L = 0.5、R = -0.25
        let stereo =
            AudioClip::from_interleaved(vec![0.5, -0.25, 0.5, -0.25], 2, 48_000).expect("合法");
        let mut mixer = AudioMixer::new();
        // 正右方（若做了逐声道声像，左声道会被压到 0——此处必须保持源比例）
        mixer.play_spatial(stereo.clone(), Vec3::new(8.0, 0.0, 0.0), 0.6, false);
        let mut out = [0.0f32; 4];
        mixer.mix(&mut out, 2, 48_000);
        // d = 8、ref = 4 → att = 1/3；总增益 = 0.6 × 1/3 = 0.2
        for frame in out.chunks_exact(2) {
            assert!(
                (frame[0] - 0.5 * 0.2).abs() < 1e-4,
                "立体声源应原样衰减 {}",
                frame[0]
            );
            assert!(
                (frame[1] + 0.25 * 0.2).abs() < 1e-4,
                "立体声源应原样衰减 {}",
                frame[1]
            );
        }
        // 正前方同样只乘总增益（0.6 × 0.5 = 0.3）
        let mut mixer = AudioMixer::new();
        mixer.play_spatial(stereo, Vec3::new(0.0, 0.0, -4.0), 0.6, false);
        let mut out = [0.0f32; 4];
        mixer.mix(&mut out, 2, 48_000);
        for frame in out.chunks_exact(2) {
            assert!((frame[0] - 0.5 * 0.3).abs() < 1e-4);
            assert!((frame[1] + 0.25 * 0.3).abs() < 1e-4);
        }
    }

    /// 非空间播放不受听者位姿影响：恒居中、无衰减（听者挪到任意远处，
    /// 输出与 `play` 逐位一致）。
    #[test]
    fn non_spatial_play_ignores_listener() {
        let src = [0.5f32, -0.5];
        let mut plain = AudioMixer::new();
        plain.play(mono(&src), 0.7, false);
        let mut expected = [0.0f32; 4];
        plain.mix(&mut expected, 2, 48_000);

        let mut mixer = AudioMixer::new();
        mixer.set_listener(ListenerParams::from_view(
            Vec3::new(1000.0, 1000.0, 1000.0),
            Vec3::X,
            Vec3::Y,
        ));
        mixer.play(mono(&src), 0.7, false);
        let mut out = [0.0f32; 4];
        mixer.mix(&mut out, 2, 48_000);
        assert_eq!(out, expected, "非空间声音不应被听者位姿调制");
        assert_eq!(
            mixer.listener(),
            ListenerParams::from_view(Vec3::new(1000.0, 1000.0, 1000.0), Vec3::X, Vec3::Y,),
            "listener() 应回读当前位姿"
        );
    }

    /// 距离参考默认值与覆写：默认 4 m；非有限 / ≤ 0 忽略；play_spatial
    /// 播放时刻固化参考，后改默认值不影响已在播的声音。
    #[test]
    fn reference_distance_default_override_and_per_sound() {
        let mut mixer = AudioMixer::new();
        mixer.set_reference_distance(0.0);
        mixer.set_reference_distance(f32::NAN);
        mixer.set_reference_distance(-2.0);
        // 全部被忽略：仍在默认 4 m 下（d = 4 → 衰减一半；循环声跨 mix 存活）
        let id = mixer.play_spatial(mono(&[0.5; 2]), Vec3::new(0.0, 0.0, -4.0), 1.0, true);
        let mut out = [0.0f32; 4];
        mixer.mix(&mut out, 2, 48_000);
        for frame in out.chunks_exact(2) {
            let want = 0.5 * 0.5 * std::f32::consts::FRAC_1_SQRT_2;
            assert!((frame[0] - want).abs() < 1e-4, "默认 ref=4、d=4 应衰减一半");
        }
        // 按声音覆写：ref = 12 → d = 4 衰减 12/16 = 0.75
        assert!(mixer.set_sound_reference_distance(id, 12.0));
        let mut out = [0.0f32; 4];
        mixer.mix(&mut out, 2, 48_000);
        for frame in out.chunks_exact(2) {
            let want = 0.5 * 0.75 * std::f32::consts::FRAC_1_SQRT_2;
            assert!((frame[0] - want).abs() < 1e-4, "覆写 ref=12 后应衰减 0.75");
        }
        assert!(mixer.stop(id), "收尾 stop 回收循环声音");
    }
}
