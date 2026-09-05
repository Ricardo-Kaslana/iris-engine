//! 简易 Schroeder 混响（Freeverb 精简版）：4 条 comb + 2 条 allpass 串联。
//!
//! 定位是**给音乐总线加一条可确定复现的空间尾巴**，不是房间声学模拟——
//! v1 不接空间音效、不做逐声音 send 量（诚实限制清单见文末）。
//!
//! 信号流（每声道一条链，左右用同一延迟整列表的不同置换实现立体声展宽）：
//!
//! ```text
//!          ┌─► comb(1557) ─┐
//!   dry ──┼─► comb(1617) ─┤ ×COMB_GAIN 求和 ─► allpass(225) ─► allpass(556) ─► wet
//!          ├─► comb(1491) ─┤
//!          └─► comb(1422) ─┘
//!   dst += dry + settings.wet × wet
//! ```
//!
//! - **comb**（有反馈延迟线，[`DelayLine::comb`]）：`y[n] = b[n − M]`、
//!   `b[n] = x[n] + g · y[n]`，往返再生系数 `g = COMB_FEEDBACK[k] × room_size`。
//!   零初始化 + 单位冲激 ⇒ `y[k·M] = g^(k−1)`（k ≥ 1）严格成立，衰减时间解析
//!   可算：`RT60 = ln(0.001) / ln(g) × M / rate` 秒。
//! - **allpass**（[`DelayLine::allpass`]）：Schroeder 全通
//!   `y[n] = b[n − M] − g·x[n]`、`b[n] = x[n] + g·y[n]`，等价于
//!   `y[n] = −g·x[n] + x[n − M] + g·y[n − M]`，故单位冲激的响应是
//!   `[−g, 0…M, 1 − g², 0…, g(1 − g²), 0…, g²(1 − g²)…]`——抽头幅度不等但
//!   **Σy² = 1**（能量守恒、只重排相位）：把 comb 的梳状谱打散成扩散尾。
//!   增益固定 [`ALLPASS_GAIN`]。
//! - **立体声展宽**：左右各 4 条 comb 的 `延迟 ↔ 再生系数` 配对不同
//!   （`COMB_TUNING_L` / `COMB_TUNING_R`），allpass 表亦不同；2 声道以上按
//!   声道下标奇偶交替取表。
//! - **采样率缩放**：表以 44100 Hz 为基准，`len = round(base × rate / 44100)`
//!   （至少 1 采样）——换采样率保持**时间**尺度而非采样数尺度。
//!
//! 确定性（本仓库硬约束）：全部为 f32 乘加 + **整数环下标**（无浮点取模、
//! 无超越函数、无随机源、无全局状态），同参数同输入序列必逐位一致；延迟线
//! 状态跨调用连续 ⇒ **混音分块大小不影响输出序列**（单测锁定）。
//! `wet = 0` 时 [`Reverb::add`] 退化为 `dst += src` 的纯干叠加（旁路、逐位
//! 透明），默认参数即旁路——不显式 `set_reverb` 时既有混音输出零改动。
//!
//! 已知限制（v1，诚实清单）：
//! - 不做 Freeverb 的**阻尼低通**（反馈环里只有纯增益 `g`）：高频与低频同速
//!   衰减，尾巴偏「亮 / 金属感」，`room_size` 只改衰减时长不改明暗；
//! - 无预延迟（pre-delay）与早期反射抽头：冲激后立刻开始扩散；
//! - 单一 `wet` 控制整条总线，不支持 dry/wet 分轨与逐声音 send；
//! - 只作用于**音乐总线**（[`crate::AudioMixer::play_music`]）：普通音效与
//!   空间音效不受 `set_reverb` 影响（空间音效接混响留待后续轮次）。

use crate::mixer::sanitize_volume;

/// comb 条数。
const COMBS: usize = 4;
/// allpass 条数。
const ALLPASS: usize = 2;
/// 延迟表基准采样率（Hz）。
const TUNING_RATE: f64 = 44_100.0;

/// comb 整延迟基准表（样本 @ 44100 Hz）。左右为同一集合的不同置换，
/// 与 [`COMB_FEEDBACK`] 的配对错位即产生去相关。
const COMB_TUNING_L: [usize; COMBS] = [1557, 1617, 1491, 1422];
const COMB_TUNING_R: [usize; COMBS] = [1491, 1422, 1617, 1557];
/// allpass 整延迟基准表（样本 @ 44100 Hz）。
const ALLPASS_TUNING_L: [usize; ALLPASS] = [225, 556];
const ALLPASS_TUNING_R: [usize; ALLPASS] = [501, 613];
/// 各 comb 在 `room_size = 1` 时的往返再生系数（略微错开以去相关）。
const COMB_FEEDBACK: [f32; COMBS] = [0.900, 0.910, 0.890, 0.905];
/// comb 并联求和归一（取平均：冲激后首波不超过干信号幅度）。
const COMB_GAIN: f32 = 1.0 / COMBS as f32;
/// allpass 固定增益（Schroeder 经典取 0.5）。
const ALLPASS_GAIN: f32 = 0.5;

/// 混响参数。经 [`Reverb::set_settings`] 卫生化：非有限输入按 0（= 旁路 /
/// 无再生）处理，有限输入钳到 `[0, 1]`。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReverbSettings {
    /// 湿信号叠加量：`dst += dry + wet × 混响输出`。`0.0` = 旁路（逐位透明）。
    pub wet: f32,
    /// 房间尺度（决定 comb 再生系数，即尾巴长短）。`0.0` = 无再生（只剩
    /// 稀疏抽头），`1.0` ≈ 2.3 s RT60（第一条 comb @ 44.1 kHz）。
    pub room_size: f32,
}

impl Default for ReverbSettings {
    /// 默认旁路（`wet = 0`）：不显式 `set_reverb` 时混音输出与 R44 之前逐位一致。
    fn default() -> Self {
        Self {
            wet: 0.0,
            room_size: 0.7,
        }
    }
}

/// 混响状态机（延迟线 + 参数）。**有状态**，但状态推进完全确定：同参数、
/// 同输入序列 ⇒ 逐位相同的输出序列，且与调用方的分块方式无关。
pub struct Reverb {
    settings: ReverbSettings,
    /// 卫生化后的各 comb 再生系数（随 `set_settings` 重算，逐采样不再乘 room_size）。
    feedback: [f32; COMBS],
    /// 当前延迟线布局（声道数, 采样率）；不匹配时 [`Reverb::add`] 重建。
    layout: Option<(usize, u32)>,
    chains: Vec<ChannelChain>,
}

impl Default for Reverb {
    fn default() -> Self {
        Self::new(ReverbSettings::default())
    }
}

impl Reverb {
    /// 以给定参数新建（延迟线惰性到第一次 [`Reverb::add`] 才分配）。
    pub fn new(settings: ReverbSettings) -> Self {
        let mut this = Self {
            settings: ReverbSettings {
                wet: 0.0,
                room_size: 0.0,
            },
            feedback: [0.0; COMBS],
            layout: None,
            chains: Vec::new(),
        };
        this.set_settings(settings);
        this
    }

    /// 当前参数（卫生化后的值）。
    pub fn settings(&self) -> ReverbSettings {
        self.settings
    }

    /// 更新参数。`wet` 与音量同口径（[`sanitize_volume`]：负数 / NaN → 0），
    /// 再钳到 ≤ 1；`room_size` 非有限按 0、有限钳到 `[0, 1]`。
    pub fn set_settings(&mut self, settings: ReverbSettings) {
        let wet = sanitize_volume(settings.wet).min(1.0);
        let room_size = if settings.room_size.is_finite() {
            settings.room_size.clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.settings = ReverbSettings { wet, room_size };
        self.feedback = comb_feedback(room_size);
    }

    /// 是否处于旁路（`wet == 0`）。旁路时 [`Reverb::add`] 只做 `dst += src`，
    /// 且 [`crate::AudioMixer`] 直接跳过整条混响路径（零额外拷贝、零分配）。
    pub fn is_bypassed(&self) -> bool {
        self.settings.wet == 0.0
    }

    /// 把干信号 `src` 以 `dst += src + wet × 混响(src)` 叠加进 `dst`。
    ///
    /// 两缓冲须等长且长度为 `channels` 的整数倍；参数退化（`channels == 0`、
    /// `sample_rate == 0`、长度不等或不整除）时安全返回、不触碰 `dst`
    /// （口径同 `AudioMixer::mix` 的退化参数处理）。
    pub fn add(&mut self, src: &[f32], dst: &mut [f32], channels: u16, sample_rate: u32) {
        let channels = channels as usize;
        if channels == 0 || sample_rate == 0 || src.len() != dst.len() || src.len() % channels != 0
        {
            return;
        }
        if self.layout != Some((channels, sample_rate)) {
            self.rebuild(channels, sample_rate);
        }
        let wet = self.settings.wet;
        let feedback = self.feedback;
        for (dst_frame, src_frame) in dst
            .chunks_exact_mut(channels)
            .zip(src.chunks_exact(channels))
        {
            for (c, chain) in self.chains.iter_mut().enumerate() {
                let reverberated = chain.process(src_frame[c], &feedback);
                dst_frame[c] += src_frame[c] + wet * reverberated;
            }
        }
    }

    /// 按（声道数, 采样率）重建延迟线（即清空全部混响状态）。
    fn rebuild(&mut self, channels: usize, sample_rate: u32) {
        self.chains = (0..channels)
            .map(|c| ChannelChain::new(sample_rate, c % 2 == 1))
            .collect();
        self.layout = Some((channels, sample_rate));
    }
}

/// 一声道一条：4 comb 并联求和 → 2 allpass 串联。
struct ChannelChain {
    combs: Vec<DelayLine>,
    allpass: Vec<DelayLine>,
}

impl ChannelChain {
    fn new(sample_rate: u32, right: bool) -> Self {
        let comb_tuning: &[usize; COMBS] = if right {
            &COMB_TUNING_R
        } else {
            &COMB_TUNING_L
        };
        let allpass_tuning: &[usize; ALLPASS] = if right {
            &ALLPASS_TUNING_R
        } else {
            &ALLPASS_TUNING_L
        };
        Self {
            combs: comb_tuning
                .iter()
                .map(|&base| DelayLine::new(scale(base, sample_rate)))
                .collect(),
            allpass: allpass_tuning
                .iter()
                .map(|&base| DelayLine::new(scale(base, sample_rate)))
                .collect(),
        }
    }

    /// 一个采样的链推进（湿项输出；干叠加由 [`Reverb::add`] 负责）。
    fn process(&mut self, input: f32, feedback: &[f32; COMBS]) -> f32 {
        let mut sum = 0.0;
        for (k, line) in self.combs.iter_mut().enumerate() {
            sum += line.comb(input, feedback[k]);
        }
        let mut value = sum * COMB_GAIN;
        for line in self.allpass.iter_mut() {
            value = line.allpass(value, ALLPASS_GAIN);
        }
        value
    }
}

/// 一条整数环延迟线（comb 与 allpass 共用的存储与推进）。
#[derive(Clone, Debug)]
pub struct DelayLine {
    buffer: Vec<f32>,
    index: usize,
}

impl DelayLine {
    /// 零初始化的延迟线（长度至少 1 采样）。
    pub fn new(delay: usize) -> Self {
        Self {
            buffer: vec![0.0; delay.max(1)],
            index: 0,
        }
    }

    /// 延迟样本数 M。
    pub fn delay(&self) -> usize {
        self.buffer.len()
    }

    /// comb 推进：`y = b[i]`（M 采样前的抽头）、`b[i] = input + y × feedback`。
    ///
    /// 零初始化 + 单位冲激 ⇒ `y[k·M] = feedback^(k−1)`（k ≥ 1）、其余采样为 0
    /// ——本方法的解析行为由单测逐采样锁定。
    pub fn comb(&mut self, input: f32, feedback: f32) -> f32 {
        let tapped = self.buffer[self.index];
        self.buffer[self.index] = input + tapped * feedback;
        self.index = (self.index + 1) % self.buffer.len();
        tapped
    }

    /// Schroeder 全通推进：`y = b[i] − gain × input`、`b[i] = input + gain × y`。
    ///
    /// 零初始化 + 单位冲激 ⇒ 冲激响应 `[−gain, 0…M, 1 − gain², 0…,
    /// gain(1 − gain²), …]`（等价递推 `y[n] = −g·x[n] + x[n−M] + g·y[n−M]`）。
    pub fn allpass(&mut self, input: f32, gain: f32) -> f32 {
        let tapped = self.buffer[self.index];
        let output = tapped - gain * input;
        self.buffer[self.index] = input + gain * output;
        self.index = (self.index + 1) % self.buffer.len();
        output
    }
}

/// `room_size` → 各 comb 的再生系数。
fn comb_feedback(room_size: f32) -> [f32; COMBS] {
    let mut out = [0.0; COMBS];
    for (slot, &base) in out.iter_mut().zip(&COMB_FEEDBACK) {
        *slot = base * room_size;
    }
    out
}

/// 把 44100 Hz 基准的延迟表缩放到目标采样率（四舍五入）。
fn scale(base: usize, sample_rate: u32) -> usize {
    // base / rate 均为有限整数值，上转 f64 后乘除不产生 NaN / 溢出
    (base as f64 * sample_rate as f64 / TUNING_RATE).round() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    /// comb 解析解：零初始化 + 单位冲激 ⇒ 第 k·M 个采样恰为 g^(k−1)，其余为 0。
    #[test]
    fn comb_impulse_response_is_exact_geometric_series() {
        let delay = 5usize;
        let g = 0.8f32;
        let mut line = DelayLine::new(delay);
        assert_eq!(line.delay(), delay);
        for n in 0..16u32 {
            let input = if n == 0 { 1.0 } else { 0.0 };
            let got = line.comb(input, g);
            let k = n / delay as u32;
            let want = if n > 0 && (n as usize) % delay == 0 {
                g.powi(k as i32 - 1)
            } else {
                0.0
            };
            assert_eq!(got, want, "comb[{n}] 解析解 {want}，实际 {got}");
        }
    }

    /// allpass 解析解：冲激响应 `[−g, …, 1, …, g, …, g²…]`（幅度守恒）。
    #[test]
    fn allpass_impulse_response_matches_analytic_form() {
        let delay = 4usize;
        let g = 0.5f32;
        let mut line = DelayLine::new(delay);
        let out: Vec<f32> = (0..21)
            .map(|n| line.allpass(if n == 0 { 1.0 } else { 0.0 }, g))
            .collect();
        assert_eq!(out[0], -g, "直通项应为 −gain × 冲激");
        assert_eq!(
            out[delay],
            1.0 - g * g,
            "主抽头 = 1 − g²（递推 y[M] = 1 + g·(−g)）"
        );
        assert_eq!(out[delay * 2], g * (1.0 - g * g));
        assert_eq!(out[delay * 3], g * g * (1.0 - g * g));
        for (n, &value) in out.iter().enumerate() {
            if n != 0 && n % delay != 0 {
                assert_eq!(value, 0.0, "非抽头位置 {n} 应为 0，实际 {value}");
            }
        }
        // 「全通」的含义是能量守恒：Σy² = g² + (1−g²)²/(1−g²) = 1
        let energy: f32 = out.iter().map(|s| s * s).sum();
        assert!(
            (energy - 1.0).abs() < 0.01,
            "冲激能量应守恒在 1.0，实际 {energy}"
        );
    }

    /// wet = 0 旁路：与「无混响的纯干叠加」逐位一致（含 −0.0 与满幅采样）。
    #[test]
    fn wet_zero_bypass_is_bit_exact() {
        let src = [0.5f32, -0.25, -0.0, 1.0, -1.0, 0.125];
        // 参照 = 「叠加进已清零缓冲」的结果：`0.0 + s`（−0.0 归一为 +0.0，
        // 与既有 Voice 的 `*dst += value × factor` 同口径，非混响引入的差异）
        let dry: Vec<f32> = src.iter().map(|s| 0.0f32 + s).collect();
        let mut got = vec![0.0f32; 6];
        let mut reverb = Reverb::new(ReverbSettings {
            wet: 0.0,
            room_size: 0.9,
        });
        assert!(reverb.is_bypassed());
        reverb.add(&src, &mut got, 2, 48_000);
        assert_eq!(
            got.iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
            dry.iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
            "wet = 0 必须逐位透明"
        );
    }

    /// 冲激尾巴：干直通项解析可算（首帧 = 1.0，湿项首帧为 0），峰值包络
    /// 随时间单调衰减，且湿项严格随 `wet` 线性缩放（延迟线状态与 wet 无关）。
    #[test]
    fn impulse_tail_decays_and_scales_linearly_with_wet() {
        let frames = 32_768usize;
        let run = |wet: f32| -> Vec<f32> {
            let mut reverb = Reverb::new(ReverbSettings {
                wet,
                room_size: 0.8,
            });
            let mut src = vec![0.0; frames];
            src[0] = 1.0;
            let mut dst = vec![0.0; frames];
            reverb.add(&src, &mut dst, 1, 44_100);
            dst
        };
        let one = run(1.0);
        let half = run(0.5);
        assert!(one.iter().all(|s| s.is_finite()));
        // 首帧：4 条 comb 的抽头在读后才写 ⇒ 湿项为 0，输出 = 纯干
        assert_eq!(one[0], 1.0, "首帧应只有干直通项");
        assert_eq!(half[0], 1.0);
        // 尾段无干信号（冲激只在 0 帧）⇒ 湿项随 wet 等比
        for n in 4096..frames {
            assert!(
                (one[n] * 0.5 - half[n]).abs() <= 1e-7 * one[n].abs().max(1e-6),
                "wet 应线性缩放湿项 @{n}：{} vs {}",
                half[n],
                one[n] * 0.5
            );
        }
        let energy = |range: std::ops::Range<usize>, v: &[f32]| -> f32 {
            range.map(|n| v[n] * v[n]).sum::<f32>()
        };
        // 宽窗能量（4096 帧 = 93 ms）对 comb 抽头相位不敏感，比窗口峰值稳健
        let early = energy(2_000..6_096, &one);
        let middle = energy(10_000..14_096, &one);
        let late = energy(26_000..30_096, &one);
        assert!(
            middle < early,
            "能量应随时间衰减：中段 {middle} ≥ 早段 {early}"
        );
        assert!(
            late < middle,
            "能量应随时间衰减：尾段 {late} ≥ 中段 {middle}"
        );
        // g ≈ 0.72、第一条 comb 延迟 1557 ⇒ 每往返 ×0.72；8000 采样 ≈ 5.1
        // 往返 → 幅度约 0.18 倍、能量约 0.03 倍。0.5 是宽松上限（只挡「不衰减」）
        assert!(
            late < early * 0.5,
            "衰减速度应与解析 RT60 同量级：{late} vs {early}"
        );
        assert!(late > 0.0, "尾巴尚未完全消失（区分「衰减」与「直接静音」）");
    }

    /// 确定性 + 分块无关性：同参数两条链，整块推进 vs 37 帧小块推进，
    /// 输出逐位一致（延迟线跨调用连续）。
    #[test]
    fn is_bit_deterministic_and_block_size_independent() {
        // 8 kHz：comb / allpass 延迟缩到 41..293 帧，2048 采样（1024 帧）内湿项
        // 一定起来（48 kHz 下最短 comb 延迟 ≈ 1695 帧，缓冲里湿项恒零、无从断言）
        const RATE: u32 = 8_000;
        let src: Vec<f32> = (0..2048)
            .map(|n| {
                let x = n as f32;
                ((x * 0.011).sin() * 0.5) + ((x * 0.37).cos() * 0.25)
            })
            .collect();
        let settings = ReverbSettings {
            wet: 0.35,
            room_size: 0.75,
        };
        let mut whole = Reverb::new(settings);
        let mut dst_a = vec![0.0; src.len()];
        whole.add(&src, &mut dst_a, 2, RATE);

        let mut chunked = Reverb::new(settings);
        let mut dst_b = Vec::with_capacity(src.len());
        for block in src.chunks(37 * 2) {
            let mut out = vec![0.0; block.len()];
            chunked.add(block, &mut out, 2, RATE);
            dst_b.extend_from_slice(&out);
        }
        let bits = |v: &[f32]| -> Vec<u32> { v.iter().map(|s| s.to_bits()).collect() };
        // 同参数独立重建整块重跑：逐位一致（确定性）
        let mut repeat = Reverb::new(settings);
        let mut dst_c = vec![0.0; src.len()];
        repeat.add(&src, &mut dst_c, 2, RATE);
        assert_eq!(bits(&dst_a), bits(&dst_c), "同参数两次推进应逐位一致");
        assert_eq!(bits(&dst_a), bits(&dst_b), "混响输出必须与混音分块大小无关");
        // 湿项确实生效：同输入下 wet=0 的旁路输出应与之不同（否则等于「挂了
        // 混响但什么都没做」，比「有非零采样」强得多的断言）
        let mut bypassed = Reverb::new(ReverbSettings {
            wet: 0.0,
            room_size: settings.room_size,
        });
        let mut dst_dry = vec![0.0; src.len()];
        bypassed.add(&src, &mut dst_dry, 2, RATE);
        assert_eq!(
            bits(&dst_dry),
            bits(&src.iter().map(|s| 0.0f32 + s).collect::<Vec<_>>()),
            "wet = 0 时输出应就是干信号"
        );
        assert_ne!(bits(&dst_a), bits(&dst_dry), "wet > 0 必须真的改变输出");
    }

    /// 参数卫生化：NaN / 负 / 超限全部被钳制；`room_size` 线性决定再生系数。
    #[test]
    fn settings_are_sanitized() {
        let default = Reverb::new(ReverbSettings::default());
        assert_eq!(default.settings(), ReverbSettings::default());
        assert!(default.is_bypassed(), "默认参数应为旁路");

        let mut reverb = Reverb::new(ReverbSettings::default());
        reverb.set_settings(ReverbSettings {
            wet: f32::NAN,
            room_size: f32::INFINITY,
        });
        assert_eq!(reverb.settings().wet, 0.0, "NaN 湿量按旁路处理");
        assert_eq!(reverb.settings().room_size, 0.0, "非有限房间尺度按 0 处理");

        reverb.set_settings(ReverbSettings {
            wet: -2.0,
            room_size: 3.0,
        });
        assert_eq!(reverb.settings().wet, 0.0, "负湿量按静音处理");
        assert_eq!(reverb.settings().room_size, 1.0, "房间尺度钳到 1");
        assert_eq!(reverb.feedback, COMB_FEEDBACK);

        reverb.set_settings(ReverbSettings {
            wet: 1.5,
            room_size: 0.5,
        });
        assert_eq!(reverb.settings().wet, 1.0, "湿量钳到 1");
        for (slot, &base) in reverb.feedback.iter().zip(&COMB_FEEDBACK) {
            assert!((*slot - base * 0.5).abs() < 1e-7, "room_size 线性缩放 g");
        }
    }

    /// 退化参数安全：零声道 / 零采样率 / 长度不匹配 / 不整除时不触碰 dst。
    #[test]
    fn degenerate_inputs_are_no_ops() {
        let mut reverb = Reverb::new(ReverbSettings {
            wet: 0.5,
            room_size: 0.5,
        });
        let src = [0.5f32, -0.5, 0.25];
        let mut dst = [9.0f32; 3];
        reverb.add(&src, &mut dst, 0, 48_000);
        assert_eq!(dst, [9.0; 3], "零声道应原样返回");
        reverb.add(&src, &mut dst, 2, 48_000);
        assert_eq!(dst, [9.0; 3], "长度不整除应原样返回");
        let mut short = [9.0f32; 2];
        reverb.add(&src, &mut short, 1, 48_000);
        assert_eq!(short, [9.0; 2], "src/dst 长度不等应原样返回");
        reverb.add(&src, &mut dst, 1, 0);
        assert_eq!(dst, [9.0; 3], "零采样率应原样返回");
    }

    /// 采样率缩放：44100 基准表按 `round(base × rate / 44100)` 缩放，
    /// 极低采样率有 1 采样下限保护。
    #[test]
    fn delay_tables_scale_with_sample_rate() {
        assert_eq!(scale(1557, 44_100), 1557);
        assert_eq!(scale(1557, 48_000), 1695); // 1557 × 48/44.1 = 1694.65
        assert_eq!(scale(225, 8_000), 41); // 225 × 8/44.1 = 40.81
        assert_eq!(scale(0, 44_100), 0);
        // 链构造走 max(1) 下限：1 Hz 也不产生零长度延迟线
        let chain = ChannelChain::new(1, false);
        assert!(chain.combs.iter().all(|line| line.delay() >= 1));
        assert!(chain.allpass.iter().all(|line| line.delay() >= 1));
    }

    /// 立体声展宽 + 布局切换重建：左右尾巴序列不同（同集合置换生效），
    /// 声道数 / 采样率变化时重建延迟线且输出仍有限。
    #[test]
    fn stereo_is_decorrelated_and_layout_switch_rebuilds() {
        let mut reverb = Reverb::new(ReverbSettings {
            wet: 1.0,
            room_size: 0.8,
        });
        let frames = 4096usize;
        let mut src = vec![0.0; frames * 2];
        src[0] = 1.0;
        src[1] = 1.0;
        let mut dst = vec![0.0; frames * 2];
        reverb.add(&src, &mut dst, 2, 48_000);
        let left: Vec<f32> = dst.iter().step_by(2).copied().collect();
        let right: Vec<f32> = dst[1..].iter().step_by(2).copied().collect();
        assert!(dst.iter().all(|s| s.is_finite()));
        assert_ne!(
            left[COMBS * 400..].to_vec(),
            right[COMBS * 400..].to_vec(),
            "左右混响尾应不同（同延迟集合的置换 + 不同 allpass 表）"
        );

        // 切布局（2 → 1 声道、48k → 8k）：重建后输出仍有限
        let mut mono = vec![0.0; frames];
        mono[0] = 1.0;
        let mut mono_dst = vec![0.0; frames];
        reverb.add(&mono, &mut mono_dst, 1, 8_000);
        assert_eq!(reverb.layout, Some((1, 8_000)), "布局应被记录");
        assert_eq!(reverb.chains.len(), 1);
        assert!(mono_dst.iter().all(|s| s.is_finite()));
        // 8 kHz 下最短 comb 延迟 = round(1422 × 8/44.1) = 257：该采样之前的湿项
        // 必为 0（重建即清空状态，证明切布局会清尾）
        let first_echo = scale(*COMB_TUNING_L.iter().min().expect("comb 表非空"), 8_000);
        assert!(
            mono_dst[1..first_echo].iter().all(|s| *s == 0.0),
            "重建后不应残留上一段的尾巴（首个湿抽头应在 {first_echo} 帧）"
        );
        assert!(
            mono_dst[first_echo..].iter().any(|s| *s != 0.0),
            "越过最短延迟后湿项应起来"
        );
    }
}
