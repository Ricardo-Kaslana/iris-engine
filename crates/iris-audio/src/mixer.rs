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
//! - **空间化 v3（C6 · 多普勒 / 遮挡 / 逐声音滤波）**——三项均为
//!   **缺席 = 逐位不变**的 opt-in 增量（HRTF 按用户裁定本轮裁出回池，
//!   上一条「身后不消音」限制原样保留）：
//!   - 多普勒：声源 / 听者速度（[`AudioMixer::set_sound_velocity`] /
//!     [`ListenerParams::with_velocity`]）经 [`doppler_factor`]（双向径向、
//!     钳 [⅓, 3]、声速全局可配 [`AudioMixer::set_speed_of_sound`]）乘进
//!     播放步长；双方零速 ⇒ 因子精确 1.0，游标推进逐位不变；
//!   - 遮挡：判定在引擎侧（射线：听者→源线段被静态体挡 ⇒ 二值遮挡），
//!     混音器只吃 [`AudioMixer::set_sound_muffle`] 的 [0, 1] 强度标量，对
//!     **增益前源值**做逐声道一阶低通（[`muffle_coef`]）；`muffle = 0` 整体
//!     旁路（滤波分支不进、状态不触碰，与 v1/v2 逐位一致）；滤波状态跨帧
//!     持久在槽位上（实时回调路径不新分配）。
//! - **流式音乐总线**（R44，[`AudioMixer::play_music`]）：常驻采样量与曲目
//!   长度**无关**的一条独立通道（OGG/Vorbis 分块解码在专用线程，混音回调按
//!   块取数，见 `stream` 模块文档）。总线上一次只放一条音乐：淡入淡出是
//!   采样级线性斜坡、循环在生产者侧 rewind、下溢填静音且游标不推进。
//!   音乐**不参与空间化**（恒总线增益），也不占 `slots` / `active_count`。
//! - **混响**（R44，[`AudioMixer::set_reverb`]）：简化 Schroeder 链（4 comb +
//!   2 allpass，见 `reverb` 模块），当前 v1 **只挂在音乐总线上**，普通与空间
//!   音效不受影响；`wet = 0`（默认）为逐位透明的旁路，此时音乐与既有路径
//!   完全同形（不多一次拷贝、不多一次浮点运算）。

use glam::Vec3;

use crate::clip::AudioClip;
use crate::reverb::{Reverb, ReverbSettings};
use crate::stream::{AudioStream, MusicBus, MusicHandle};

/// 空间声音的默认距离参考（米）：`att = ref / (ref + d)` 在 d = ref 处衰减
/// 恰为一半——4 m 适合第一人称 / 角色脚边尺度的声源；俯视相机等悬空听者
/// 应按场域尺度放大（[`AudioMixer::set_reference_distance`]）。
pub const DEFAULT_REFERENCE_DISTANCE: f32 = 4.0;

/// 听者位姿：世界坐标位置 + 前方 / 右轴单位向量 + 世界速度（v3 多普勒）。
///
/// **契约**：`forward` / `right` 必须为单位向量且相互正交，由调用方保证
/// （手搓位姿不满足时声像会失真但不会 panic——投影被钳制在 [-1, 1]）；
/// 用 [`ListenerParams::from_view`] 构造可自动正交化。默认位姿与引擎右手系
/// 一致：原点、前方 -Z、右轴 +X。v1 的声像只依赖 `right`（源方向在右轴上
/// 的投影），`forward` 参与语义表达、为 HRTF 等后续扩展占位。
/// `velocity`（v3）只被多普勒项消费（[`doppler_factor`]）：默认
/// [`Vec3::ZERO`] = 静止听者，因子恒 1.0，v1/v2 路径逐位不变。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ListenerParams {
    /// 世界坐标位置。
    pub position: Vec3,
    /// 前方单位向量（调用方保证单位、与 `right` 正交）。
    pub forward: Vec3,
    /// 右轴单位向量（调用方保证单位、与 `forward` 正交）。
    pub right: Vec3,
    /// 听者世界速度（米/秒，v3 多普勒输入；ZERO = 静止）。
    pub velocity: Vec3,
}

impl Default for ListenerParams {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            forward: Vec3::NEG_Z,
            right: Vec3::X,
            velocity: Vec3::ZERO,
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
            velocity: Vec3::ZERO,
        }
    }

    /// 覆写听者速度（v3 多普勒输入；链式）。
    #[must_use]
    pub fn with_velocity(mut self, velocity: Vec3) -> Self {
        self.velocity = velocity;
        self
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

/// 声音在空气中的传播速度默认值（米/秒，v3 多普勒）：常温干燥空气常数，
/// 游戏尺度无需随温度修正（[`AudioMixer::set_speed_of_sound`] 全局可配）。
pub const DEFAULT_SPEED_OF_SOUND: f32 = 343.0;

/// 多普勒因子上下界（用户裁定 2026-09-19 病态防护）：相对径向速度达到 /
/// 超过音速的极端场景（超音速弹丸、传送跳变）不做物理正确——那属冲击波
/// 模拟——只保证**因子上有界**：播放步长最多 ×3 / ×⅓，音高畸变有顶。
pub const DOPPLER_FACTOR_MIN: f32 = 1.0 / 3.0;
pub const DOPPLER_FACTOR_MAX: f32 = 3.0;

/// 多普勒因子纯函数（v3）：`f = (c + v_o) / (c − v_s)`，双向径向合成。
///
/// - `offset` = 声源 − 听者（世界坐标差；内部归一取径向，零长 / 退化 ⇒
///   因子恒 1.0——源听者重叠时径向无定义）；
/// - `v_o = dot(听者速度, d̂)`（听者**趋近**声源为正 → 频率升高）；
/// - `v_s = −dot(声源速度, d̂)`（声源**趋近**听者为正 → 频率升高）；
/// - 分母 ≤ 0（声源趋近达音速）⇒ 直接钳 [`DOPPLER_FACTOR_MAX`]；分子 ≤ 0
///   （听者逃离达音速）⇒ 钳 [`DOPPLER_FACTOR_MIN`]；其余钳制到 [⅓, 3]；
/// - **静止锚**：双方零速时 `c / c` 在 f32 下**精确**得 1.0，消费方
///   `ratio × 1.0`（f64 乘 1.0 精确）⇒ 播放步长逐位不变——v3 缺席形态
///   与 v1/v2 像素级同构的「逐位级」锚；
/// - 病态守卫：`c` 非有限 / ≤ 0、速度含 NaN ⇒ 1.0（混音路径不传播病态值）。
#[must_use]
pub fn doppler_factor(
    offset: Vec3,
    listener_velocity: Vec3,
    source_velocity: Vec3,
    speed_of_sound: f32,
) -> f32 {
    if !(speed_of_sound.is_finite() && speed_of_sound > 0.0)
        || !listener_velocity.is_finite()
        || !source_velocity.is_finite()
    {
        return 1.0;
    }
    let distance = offset.length();
    if distance <= 1e-6 {
        return 1.0; // 源听者重叠：径向无定义，无多普勒
    }
    let dir = offset / distance; // 听者 → 声源单位向量
    let v_o = listener_velocity.dot(dir); // 听者趋近分量（正 = 靠近声源）
    let v_s = -source_velocity.dot(dir); // 声源趋近分量（正 = 靠近听者）
    let num = speed_of_sound + v_o;
    let den = speed_of_sound - v_s;
    if den <= 0.0 {
        return DOPPLER_FACTOR_MAX; // 声源径向趋近 ≥ 音速：冲击波区，钳上限
    }
    if num <= 0.0 {
        return DOPPLER_FACTOR_MIN; // 听者径向逃离 ≥ 音速：追不上声波，钳下限
    }
    (num / den).clamp(DOPPLER_FACTOR_MIN, DOPPLER_FACTOR_MAX)
}

/// 遮挡闷声系数（v3）：`muffle ∈ [0, 1]` → 一阶低通跨导系数
/// `k = 1 − 0.9·muffle`（`y += k·(x − y)`）。
///
/// - `muffle = 0` ⇒ k = 1 ⇒ y = x 恒等——但消费方对 0 走**整体旁路**
///   （不进滤波分支、不触碰状态），保证与 v1/v2 路径逐位一致；
/// - `muffle = 1` ⇒ k = 0.1 ⇒ −3 dB 截止 ≈ 810 Hz @ 48 kHz（门后隔墙的
///   主观感受档：高频梳齿先没、低音残留）；
/// - 非有限输入 ⇒ 1.0（透明；病态值不进混音）。
#[must_use]
pub fn muffle_coef(muffle: f32) -> f32 {
    if !muffle.is_finite() {
        return 1.0;
    }
    1.0 - 0.9 * muffle.clamp(0.0, 1.0)
}

/// 逐声音一阶低通（v3 遮挡滤波本体）：跨混音调用持久的每声道状态。
///
/// `coef` 由 [`muffle_coef`] 算出、每声音每缓冲算一次；`step` 是纯计算，
/// 状态实际存于 [`Voice::lp_state`]（本结构只是无状态的系数持有 + 单步式）。
/// 声道 > 2 的罕见输出布局共用前两路状态槽（诚实限制 §8.66——本引擎输出
/// 恒单 / 双声道）。`pub(crate)`：`SourceFrames::add_frame` 签名消费。
pub(crate) struct Muffle {
    /// 跨导系数（[`muffle_coef`]）。
    coef: f32,
}

impl Muffle {
    /// 单声道一步：`y = prev + coef·(x − prev)`（全 f32 定点序，确定性）。
    fn step(&self, prev: f32, x: f32) -> f32 {
        prev + self.coef * (x - prev)
    }
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

/// 一路声音在**一帧**上的增益因子（音量 / 空间衰减 / 包络的合成结果）。
///
/// 立体声输出用 `left` / `right`（恒功率声像拆分），其余声道数用 `mono`。
pub(crate) struct FrameGains {
    pub(crate) left: f32,
    pub(crate) right: f32,
    pub(crate) mono: f32,
}

/// 交错源采样视图（剪辑或流式块通用），把「游标 → 采样 → 声道映射 → 增益
/// 叠加」这段混音数学收敛成**一处实现**：`AudioMixer` 的槽位声音与
/// `MusicBus` 的流式声音走同一份逐位一致的路径（不复制第二份数学）。
pub(crate) struct SourceFrames<'a> {
    samples: &'a [f32],
    channels: usize,
}

impl<'a> SourceFrames<'a> {
    pub(crate) fn new(samples: &'a [f32], channels: usize) -> Self {
        Self { samples, channels }
    }

    /// 把源帧 `[i0, i1]` 按 `frac` 线性插值后叠加进 `dst`（一帧输出）。
    ///
    /// 声道映射口径（与 R44 之前的 `mix` 逐位一致）：等声道直取；单声道源
    /// 复制到全部输出通道；输出单声道时平均全部源通道；其余罕见布局逐通道
    /// 取 `min(c, 源声道 − 1)`。
    ///
    /// `lp`（v3 遮挡）：`Some((muffle, state))` 时对**增益前的源值**逐声道
    /// 走一阶低通（state 各槽 = 本声音该声道的滤波记忆，跨帧持久；声道 > 2
    /// 共用前两槽）；`None` = 旁路——与引入本参数前**逐位一致**（音乐总线
    /// 与未遮挡声音恒 `None`，单一事实源不分叉）。低通在声道映射之后、
    /// 增益之前：遮挡是声学现象（先闷后 pan），与声像 / 音量正交。
    pub(crate) fn add_frame(
        &self,
        dst: &mut [f32],
        i0: usize,
        i1: usize,
        frac: f32,
        gains: &FrameGains,
        lp: Option<(&Muffle, &mut [f32; 2])>,
    ) {
        let channels_out = dst.len();
        let channels_src = self.channels;
        let mut lp = lp;
        for (c, out) in dst.iter_mut().enumerate() {
            let value = if channels_src == channels_out {
                lerp_channel(self.samples, channels_src, i0, i1, frac, c)
            } else if channels_src == 1 {
                // 上混：单声道源复制到全部输出通道
                lerp_channel(self.samples, channels_src, i0, i1, frac, 0)
            } else if channels_out == 1 {
                // 下混：平均全部源通道
                let mut sum = 0.0;
                for k in 0..channels_src {
                    sum += lerp_channel(self.samples, channels_src, i0, i1, frac, k);
                }
                sum / channels_src as f32
            } else {
                // 罕见布局回退：逐输出通道取 min(c, 源声道-1)
                lerp_channel(
                    self.samples,
                    channels_src,
                    i0,
                    i1,
                    frac,
                    c.min(channels_src - 1),
                )
            };
            let value = match &mut lp {
                Some((muffle, state)) => {
                    let slot = if c < 2 { c } else { 0 };
                    let y = muffle.step(state[slot], value);
                    state[slot] = y;
                    y
                }
                None => value,
            };
            let factor = if channels_out == 2 {
                if c == 0 { gains.left } else { gains.right }
            } else {
                gains.mono
            };
            *out += value * factor;
        }
    }
}

/// 一个活动声音：共享剪辑 + 播放状态（+ 空间化 v1 的声源定位字段 +
/// v3 的多普勒速度 / 遮挡低通字段）。
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
    /// 声源世界速度（米/秒，v3 多普勒；[`AudioMixer::set_sound_velocity`]
    /// 逐帧更新）。ZERO = 静止 ⇒ 因子恒 1.0（逐位不变锚）；
    /// 仅空间声音消费（非空间无位置径向可言，不产生多普勒）。
    velocity: Vec3,
    /// 遮挡闷声强度 ∈ [0, 1]（v3；0 = 未遮挡 = 滤波整体旁路）。
    /// 判定在引擎侧（射线），混音器只吃标量（见 `set_sound_muffle`）。
    muffle: f32,
    /// 一阶低通的逐声道滤波记忆（v3；仅 `muffle > 0` 时被触碰）。
    lp_state: [f32; 2],
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
/// 流式音乐**不进槽位表**（它没有常驻剪辑可引用），单独挂在 [`Self::music`]
/// 音乐总线上，因此 `active_count` 只统计音效声音，不含音乐。
pub struct AudioMixer {
    slots: Vec<VoiceSlot>,
    free: Vec<u32>,
    /// 当帧听者位姿（mix 时对空间声音重算增益）。
    listener: ListenerParams,
    /// 空间声音的距离参考默认值（米），`play_spatial` 时固化进声音槽位。
    reference_distance: f32,
    /// 流式音乐总线（R44）。
    music: MusicBus,
    /// 音乐总线混响（R44）；`wet = 0` 为旁路，此时音乐走零拷贝直通叠加。
    reverb: Reverb,
    /// 混响启用时暂存音乐干信号的可复用缓冲（避免实时路径每次分配）。
    music_scratch: Vec<f32>,
    /// 声速（米/秒，v3 多普勒；[`AudioMixer::set_speed_of_sound`]，
    /// 默认 [`DEFAULT_SPEED_OF_SOUND`]）。全局单值：同场景同介质。
    speed_of_sound: f32,
}

impl Default for AudioMixer {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioMixer {
    /// 创建空混音器（听者默认：原点、前方 -Z、右轴 +X；音乐总线空闲；
    /// 混响旁路）。
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            listener: ListenerParams::default(),
            reference_distance: DEFAULT_REFERENCE_DISTANCE,
            music: MusicBus::default(),
            reverb: Reverb::default(),
            music_scratch: Vec::new(),
            speed_of_sound: DEFAULT_SPEED_OF_SOUND,
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
            velocity: Vec3::ZERO,
            muffle: 0.0,
            lp_state: [0.0; 2],
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

    // ---- 空间化 v3（C6）：多普勒速度 + 遮挡闷声 ----

    /// 更新一个声音的世界速度（米/秒，v3 多普勒；下一帧 `mix` 生效）。
    /// 句柄失效返回 `false`。**逐帧推送**口径与 [`Self::set_sound_position`]
    /// 同款（混音器不推时间轴，速度是输入不是导数——引擎侧按位置差分或
    /// 游戏侧直接给）。非空间声音存而不用（无径向 ⇒ 恒无多普勒）。
    pub fn set_sound_velocity(&mut self, id: SoundId, velocity: Vec3) -> bool {
        let Some(slot) = self.slots.get_mut(id.index()) else {
            return false;
        };
        if slot.generation != id.generation() {
            return false;
        }
        match &mut slot.voice {
            Some(voice) => {
                voice.velocity = velocity;
                true
            }
            None => false,
        }
    }

    /// 设置一个声音的遮挡闷声强度（v3；`muffle` 钳到 [0, 1]，NaN 按 0 =
    /// 旁路）。判定（射线听不见 = 遮挡）在引擎侧（`iris_engine::spatial`），
    /// 混音器只吃标量做一阶低通（[`muffle_coef`]）。句柄失效返回 `false`。
    pub fn set_sound_muffle(&mut self, id: SoundId, muffle: f32) -> bool {
        let muffle = if muffle.is_finite() {
            muffle.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let Some(slot) = self.slots.get_mut(id.index()) else {
            return false;
        };
        if slot.generation != id.generation() {
            return false;
        }
        match &mut slot.voice {
            Some(voice) => {
                voice.muffle = muffle;
                true
            }
            None => false,
        }
    }

    /// 一个声音当前的闷声强度（观测 / 测试口；句柄失效 `None`）。
    pub fn sound_muffle(&self, id: SoundId) -> Option<f32> {
        self.slots
            .get(id.index())
            .filter(|s| s.generation == id.generation())
            .and_then(|s| s.voice.as_ref())
            .map(|v| v.muffle)
    }

    /// 一个声音当前的世界速度（观测 / 测试口；句柄失效 `None`）。
    pub fn sound_velocity(&self, id: SoundId) -> Option<Vec3> {
        self.slots
            .get(id.index())
            .filter(|s| s.generation == id.generation())
            .and_then(|s| s.voice.as_ref())
            .map(|v| v.velocity)
    }

    /// 覆写听者速度（v3 多普勒的听者侧输入；语义即
    /// `set_listener(params.with_velocity(v))` 的便捷口——
    /// 已含在 [`Self::set_listener`] 的位姿里，本口只改 velocity 分量）。
    pub fn set_listener_velocity(&mut self, velocity: Vec3) {
        self.listener.velocity = velocity;
    }

    /// 设置声速（米/秒，v3 多普勒全局参数）。非有限或 ≤ 0 被忽略
    /// （保持原值，口径同 [`Self::set_reference_distance`]）。
    pub fn set_speed_of_sound(&mut self, speed_of_sound: f32) {
        if speed_of_sound.is_finite() && speed_of_sound > 0.0 {
            self.speed_of_sound = speed_of_sound;
        }
    }

    /// 当前声速（观测口）。
    pub fn speed_of_sound(&self) -> f32 {
        self.speed_of_sound
    }

    /// 当前活动声音数（含尚未被 `mix` 推进过的）。**不含**流式音乐
    /// （音乐在独立总线上，用 [`AudioMixer::music_is_playing`] 查询）。
    pub fn active_count(&self) -> usize {
        self.slots.iter().filter(|s| s.voice.is_some()).count()
    }

    // ---- 流式音乐总线（R44）----

    /// 在音乐总线上播放一条流（OGG/Vorbis 或任何 [`AudioStream`] 实现）。
    ///
    /// 与 [`AudioMixer::play`] 的根本差别：采样**不全量常驻**——解码在专用
    /// 后台线程分块产出，混音回调按需取块（机制与语义见 `stream` 模块文档）。
    ///
    /// - `volume` 钳制到 ≥ 0（负数 / NaN 按静音，口径同 `play`）；
    /// - `fade_in` 秒线性淡入（≤ 0 或 NaN 为立即满音量）；
    /// - `looping = true` 时流耗尽后调 [`AudioStream::rewind`] 重播；不支持
    ///   rewind 的流播完即止；
    /// - **单轨**：新轨启动会立即硬拆上一轨（v1 不做 crossfade），上一轨已
    ///   排队的块被丢弃；
    /// - 音乐不参与空间化（无距离衰减 / 声像），也不进 `slots`（`stop` /
    ///   `active_count` 管不到它）。
    ///
    /// 返回 [`MusicHandle`]：与 [`AudioMixer::music_handle`] 比对可知「我放的
    /// 这条是否仍是当前轨」。病态流（0 声道 / 0 采样率）不启动、句柄立即失效。
    pub fn play_music<S: AudioStream + 'static>(
        &mut self,
        stream: S,
        volume: f32,
        fade_in: f32,
        looping: bool,
    ) -> MusicHandle {
        self.music.play(stream, volume, fade_in, looping)
    }

    /// 停止当前音乐轨。`fade_out > 0` 时按秒线性淡出、到位后拆轨（混响尾巴
    /// 继续衰减）；`fade_out <= 0` / NaN 立即拆轨。此前有轨在播返回 `true`。
    pub fn stop_music(&mut self, fade_out: f32) -> bool {
        self.music.stop(fade_out)
    }

    /// 调整音乐总线音量（下一次 `mix` 即时生效；负数 / NaN 按静音）。
    /// 总线音量与淡入淡出包络相乘，二者独立。
    pub fn set_music_volume(&mut self, volume: f32) {
        self.music.set_volume(volume);
    }

    /// 音乐总线是否占用中（含淡出中）。
    pub fn music_is_playing(&self) -> bool {
        self.music.is_playing()
    }

    /// 当前音乐轨句柄（`None` = 空闲）。
    pub fn music_handle(&self) -> Option<MusicHandle> {
        self.music.handle()
    }

    /// 后台解码线程是否已退出（`stop_music` 后轮询本方法可确认线程真正结束；
    /// 内联解码模式恒 `true`——本来就没有线程）。
    pub fn music_producer_finished(&self) -> bool {
        self.music.producer_finished()
    }

    /// 阻塞等待最近一条音乐的后台解码线程退出（`stop_music` 之后调用即为
    /// 干净的收尾）。**会阻塞**，不要在实时音频回调里调用。返回 `true` =
    /// 确实等到并回收了一个正常退出的线程；`false` = 无句柄可回收（内联解码
    /// 模式、已回收过），或该线程 panic（音频路径不因一次 panic 永久失能，
    /// 但这里如实报 false）。
    pub fn join_music_producer(&mut self) -> bool {
        self.music.join_producer()
    }

    // ---- 混响（R44）----

    /// 设置音乐总线混响（简化 Schroeder 链，算法与限制见 `reverb` 模块）。
    /// 参数被卫生化（非有限 → 0，有限值钳到 `[0, 1]`），读回用
    /// [`AudioMixer::reverb_settings`]。
    ///
    /// `wet = 0`（默认）为旁路：音乐走零拷贝直通叠加、逐位与不启用混响一致。
    /// **v1 只作用于音乐总线**——普通音效与空间音效不受本方法影响。
    pub fn set_reverb(&mut self, settings: ReverbSettings) {
        self.reverb.set_settings(settings);
    }

    /// 当前混响参数（卫生化后的值）。
    pub fn reverb_settings(&self) -> ReverbSettings {
        self.reverb.settings()
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
        let speed_of_sound = self.speed_of_sound;

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
                velocity,
                muffle,
                lp_state,
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
            // 多普勒（v3）：因子乘进播放步长（>1 趋近 = 消费更快 = 音高升高）。
            // 仅空间声音消费（非空间无径向可言）；源与听者皆静止时
            // `doppler_factor` 恒返回精确 1.0 ⇒ `ratio × 1.0`（f64 乘 1.0
            // 精确）⇒ 游标推进逐位不变（v1/v2 锚）。
            let doppler = if *spatial {
                doppler_factor(
                    *position - listener.position,
                    listener.velocity,
                    *velocity,
                    speed_of_sound,
                )
            } else {
                1.0
            };
            let ratio = ratio * doppler as f64;
            // 遮挡低通（v3）：muffle = 0 走整体旁路（lp = None，与引入前
            // 逐位一致）；> 0 时算一次系数，逐采样在声道映射后滤波。
            let muffle_filter = if *muffle > 0.0 {
                Some(Muffle {
                    coef: muffle_coef(*muffle),
                })
            } else {
                None
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

            let gains = FrameGains {
                left: factor_l,
                right: factor_r,
                mono: factor_mono,
            };
            let source = SourceFrames::new(samples, channels_src);

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
                let lp = muffle_filter
                    .as_ref()
                    .map(|m| (m, &mut *lp_state as &mut [f32; 2]));
                source.add_frame(out_frame, i0, i1, frac, &gains, lp);
                *cursor += ratio;
            }
        }

        // 流式音乐总线（R44）：音效之后、限幅之前叠加（音乐与音效同样受软限幅）。
        {
            let AudioMixer {
                music,
                reverb,
                music_scratch,
                ..
            } = self;
            if reverb.is_bypassed() {
                // 直通路径：零额外拷贝；总线空闲时完全不触碰 out
                music.render_into(out, channels_out, sample_rate_out);
            } else {
                // 混响路径：干信号先进可复用 scratch，再由混响以 dry + wet 叠加。
                // 即便音乐已拆轨（无产出、scratch 全零），混响尾巴也继续衰减。
                music_scratch.resize(out.len(), 0.0);
                music_scratch.fill(0.0);
                let frames = out.len();
                music.render_into(&mut music_scratch[..frames], channels_out, sample_rate_out);
                reverb.add(
                    &music_scratch[..frames],
                    out,
                    channels_out as u16,
                    sample_rate_out,
                );
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
/// `pub(crate)`：音乐总线音量与混响湿量共用同一口径。
pub(crate) fn sanitize_volume(volume: f32) -> f32 {
    if volume.is_finite() {
        volume.max(0.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procedural::sine;

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

    // ---- 流式音乐总线与混响（R44）----

    use crate::stream::SampleStream;

    /// 恒非零的测试源采样（`0.25 … 0.75` 区间抖动）：于是「静音」可与「源里
    /// 恰好有 0 采样」区分开，过滤 / 断言都不依赖巧合。
    fn ramp_source(frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| 0.25 + (i % 71) as f32 * 0.005 + (i % 13) as f32 * 0.01)
            .collect()
    }

    /// 等后台解码线程退出：把「整条流已在队列里」变成确定的前置条件，
    /// 而不是碰运气赌生产者跑得过消费者（要求各块总量不超过队列容量）。
    fn wait_producer_finished(mixer: &AudioMixer) {
        for _ in 0..400 {
            if mixer.music_producer_finished() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("后台解码线程未在 2 s 内退出");
    }

    /// **块拼接逐位直通**：采样率一致、音量 1.0、无淡变时，流式输出的采样
    /// 序列与源序列逐位相同——跨 5 个块边界（末块不足一块）、并跨两次混音
    /// （第二次混音的游标要接上第一次剪掉的前缀）。
    #[test]
    fn music_block_splices_are_sample_exact() {
        let src = ramp_source(5_000);
        let mut mixer = AudioMixer::new();
        let handle = mixer.play_music(SampleStream::new(mono(&src), 1_024), 1.0, 0.0, false);
        assert_eq!(mixer.music_handle(), Some(handle));
        assert!(mixer.music_is_playing());
        // 5 块 < 队列容量 8 块 ⇒ 线程退出即全部数据就位，不可能下溢
        wait_producer_finished(&mixer);

        let mut heard = Vec::with_capacity(src.len() + 8);
        let mut first = vec![7.0f32; 2_000];
        mixer.mix(&mut first, 1, 48_000);
        heard.extend_from_slice(&first);
        let mut second = vec![7.0f32; src.len() - 2_000 + 8];
        mixer.mix(&mut second, 1, 48_000);
        heard.extend_from_slice(&second);

        for (i, (got, want)) in heard[..src.len()].iter().zip(&src).enumerate() {
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "第 {i} 帧应逐位等于源采样（跨块、跨混音调用都不得引入误差）"
            );
        }
        assert_eq!(&heard[src.len()..], &[0.0; 8][..], "播完后的富余帧应为静音");
        assert!(
            !mixer.music_is_playing(),
            "非循环流播完且待播缓冲耗尽后应自动拆轨"
        );
        assert_eq!(mixer.music_handle(), None);
        assert_eq!(mixer.active_count(), 0, "音乐不占音效槽位");
    }

    /// **下溢语义**：数据没到时本缓冲剩余帧填静音、**游标停住不推进**；数据
    /// 到位后从原处精确续播（两段合起来 == 完整源序列）。
    #[test]
    fn music_underrun_is_silence_and_resumes_exactly() {
        use std::sync::{Arc, Condvar, Mutex};

        /// 第一块立刻产出、第二块等测试放行的工具流（可控下溢）。
        struct Gated {
            first: Vec<f32>,
            second: Vec<f32>,
            sent_first: bool,
            gate: Arc<(Mutex<bool>, Condvar)>,
        }
        impl AudioStream for Gated {
            fn channels(&self) -> u16 {
                1
            }
            fn sample_rate(&self) -> u32 {
                48_000
            }
            fn next_block(&mut self, out: &mut Vec<f32>) -> bool {
                if !self.sent_first {
                    out.extend_from_slice(&self.first);
                    self.sent_first = true;
                    return true;
                }
                let mut guard = self
                    .gate
                    .0
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                while !*guard {
                    guard = self
                        .gate
                        .1
                        .wait(guard)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                out.extend_from_slice(&self.second);
                false
            }
        }

        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let first = ramp_source(1_000);
        let second = ramp_source(1_000);
        let mut mixer = AudioMixer::new();
        mixer.play_music(
            Gated {
                first: first.clone(),
                second: second.clone(),
                sent_first: false,
                gate: Arc::clone(&gate),
            },
            1.0,
            0.0,
            false,
        );

        // 第一次混音只有第一块可用 ⇒ 前 1000 帧逐位、其余 500 帧静音
        let mut out = vec![9.0f32; 1_500];
        mixer.mix(&mut out, 1, 48_000);
        for (i, (got, want)) in out[..first.len()].iter().zip(&first).enumerate() {
            assert_eq!(got.to_bits(), want.to_bits(), "已到位的块应逐位输出 @{i}");
        }
        assert!(
            out[first.len()..].iter().all(|s| *s == 0.0),
            "下溢的剩余帧应填静音"
        );
        assert!(mixer.music_is_playing(), "下溢不是结束：轨仍在总线上");

        // 放行第二块 → 等线程退出 → 续播应精确接在第一块之后
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        wait_producer_finished(&mixer);
        let mut rest = vec![9.0f32; second.len()];
        mixer.mix(&mut rest, 1, 48_000);
        for (i, (got, want)) in rest.iter().zip(&second).enumerate() {
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "游标必须在静音期间停住、续播逐位接上 @{i}"
            );
        }
        assert!(!mixer.music_is_playing(), "两块都播完后应拆轨");
    }

    /// **循环回绕**：循环轨持续产出并按源长回绕，采样级无缝（比对只取非静音
    /// 帧 ⇒ 不依赖生产者与消费者的相对速度，队列取空的下溢帧被自然跳过）。
    #[test]
    fn music_looping_wraps_without_gaps() {
        let src = ramp_source(500);
        let mut mixer = AudioMixer::new();
        mixer.play_music(SampleStream::new(mono(&src), 500), 1.0, 0.0, true);
        let want = src.len() * 3;
        let mut heard: Vec<f32> = Vec::new();
        // 块长 401 与源长 500 互质：跨块、跨圈都不会刻意对齐
        let mut chunk = vec![0.0f32; 401];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while heard.len() < want && std::time::Instant::now() < deadline {
            mixer.mix(&mut chunk, 1, 48_000);
            heard.extend(chunk.iter().filter(|s| **s != 0.0).copied());
            if heard.len() < want {
                // 下溢静音是设计行为：给后台线程一点时间（断言只看非静音帧，
                // 因此不依赖这里的等待是否恰好足够）
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        assert!(
            heard.len() >= want,
            "循环应能连续产出 3 圈，实际 {}",
            heard.len()
        );
        for (i, got) in heard[..want].iter().enumerate() {
            assert_eq!(
                got.to_bits(),
                src[i % src.len()].to_bits(),
                "第 {i} 个非静音采样应等于源的圈回绕值"
            );
        }
        assert!(mixer.stop_music(0.0), "循环轨应可立即停止");
        assert!(!mixer.music_is_playing());
        assert!(
            mixer.join_music_producer(),
            "close 信号应让后台解码线程自行退出"
        );
        wait_producer_finished(&mixer);
    }

    /// **淡入 / 淡出包络**：满幅常量源上的增益斜坡解析可算（线性、到位后恒定
    /// 直通），淡出到位自动拆轨。
    #[test]
    fn music_fade_in_and_out_are_linear_and_teardown() {
        // 8 块 × 400 帧 = 整条流可一次性排进队列（等待线程退出即无下溢）
        let src = vec![0.8f32; 3_200];
        let mut mixer = AudioMixer::new();
        mixer.play_music(SampleStream::new(mono(&src), 400), 1.0, 0.01, false); // 10 ms = 480 帧
        wait_producer_finished(&mixer);

        let mut out = vec![0.0f32; 1_000];
        mixer.mix(&mut out, 1, 48_000);
        assert_eq!(out[0], 0.0, "淡入首帧静音");
        assert!(
            out.iter().all(|s| (0.0..=0.8).contains(s)),
            "包络不得过冲满幅"
        );
        // 线性斜坡：第 i 帧增益 = i / 480。容差按 f32 每帧累加的最坏舍入给
        // （480 步 × ulp(1.0)/2 ≈ 3e-5 的增益误差，×0.8 后 2.4e-5）
        for i in [1usize, 120, 240, 479] {
            let want = 0.8 * (i as f32 / 480.0);
            assert!(
                (out[i] - want).abs() < 5e-5,
                "淡入第 {i} 帧应为 {want}，实际 {}",
                out[i]
            );
        }
        assert!(out.windows(2).all(|w| w[1] >= w[0]), "淡入包络必须单调不降");
        // 到位帧：f32 累加落到 1.0 的确切帧号允许 ±4 帧抖动
        let full = out
            .iter()
            .position(|s| *s == 0.8)
            .expect("斜坡应在缓冲内到位");
        assert!(
            (476..=484).contains(&full),
            "480 帧斜坡的到位帧应为 {full}（允许 ±4 帧舍入）"
        );
        for (i, got) in out[full..].iter().enumerate() {
            assert_eq!(
                got.to_bits(),
                src[full + i].to_bits(),
                "到位后应满增益逐位直通（首个越界下标 {}）",
                full + i
            );
        }

        // 淡出 5 ms = 240 帧：从 0.8 线性降到 0，之后静音并拆轨
        assert!(mixer.stop_music(0.005));
        let mut tail = vec![0.0f32; 600];
        mixer.mix(&mut tail, 1, 48_000);
        assert_eq!(tail[0], 0.8, "淡出首帧沿用当前包络");
        let want = 0.8 * (1.0 - 30.0 / 240.0);
        assert!(
            (tail[30] - want).abs() < 5e-5,
            "淡出第 30 帧应为 {want}，实际 {}",
            tail[30]
        );
        assert!(
            tail.windows(2).all(|w| w[1] <= w[0]),
            "淡出包络必须单调不升"
        );
        let done = tail
            .iter()
            .position(|s| *s == 0.0)
            .expect("淡出应在缓冲内归零");
        assert!(
            (236..=244).contains(&done),
            "240 帧淡出的归零帧应为 {done}（允许 ±4 帧舍入）"
        );
        assert!(
            tail[done..].iter().all(|s| *s == 0.0),
            "归零后应保持静音（钳住而不是穿过 0 变负）"
        );
        assert!(!mixer.music_is_playing(), "淡出到位应自动拆轨");
    }

    /// 总线音量：即时生效、与包络相乘；负数 / NaN 按静音（口径同 `play`）。
    #[test]
    fn music_volume_is_immediate_and_sanitized() {
        let src = ramp_source(2_000);
        let mut mixer = AudioMixer::new();
        mixer.play_music(SampleStream::new(mono(&src), 500), 0.5, 0.0, false);
        wait_producer_finished(&mixer);

        let mut out = vec![0.0f32; 1_000];
        mixer.mix(&mut out, 1, 48_000);
        for (i, got) in out.iter().enumerate() {
            assert!(
                (got - src[i] * 0.5).abs() <= src[i] * 1e-7 + 1e-7,
                "音量 0.5 应减半 @{i}"
            );
        }
        mixer.set_music_volume(0.0);
        let mut muted = vec![9.0f32; 100];
        mixer.mix(&mut muted, 1, 48_000);
        assert!(muted.iter().all(|s| *s == 0.0), "音量 0 应静音");
        mixer.set_music_volume(f32::NAN);
        let mut nan_out = vec![0.0f32; 100];
        mixer.mix(&mut nan_out, 1, 48_000);
        assert!(
            nan_out.iter().all(|s| *s == 0.0),
            "NaN 音量应被卫生化为静音而不是传播 NaN"
        );
        assert!(mixer.stop_music(0.0));
    }

    /// 音乐与音效线性叠加、同样受软限幅；声道映射复用同一条路径
    /// （立体声源 → 单声道输出为平均下混）。
    #[test]
    fn music_and_effects_sum_linearly_and_share_channel_mapping() {
        let frames = 512usize;
        let stereo: Vec<f32> = [0.6f32, 0.2].repeat(frames);
        let clip = AudioClip::from_interleaved(stereo.clone(), 2, 48_000).expect("合法");
        let mut mixer = AudioMixer::new();
        mixer.play(mono(&[0.1; 512]), 1.0, false);
        mixer.play_music(SampleStream::new(clip, 128), 1.0, 0.0, false);
        wait_producer_finished(&mixer);
        let mut out = vec![0.0f32; frames * 2];
        mixer.mix(&mut out, 2, 48_000);
        for frame in out.chunks_exact(2) {
            assert!((frame[0] - 0.7).abs() < 1e-6, "左 = 音效 0.1 + 音乐 0.6");
            assert!((frame[1] - 0.3).abs() < 1e-6, "右 = 音效 0.1 + 音乐 0.2");
        }

        // 立体声音乐 → 单声道输出：下混为平均（(0.6 + 0.2) / 2 = 0.4）
        let mut mixer = AudioMixer::new();
        mixer.play_music(
            SampleStream::new(
                AudioClip::from_interleaved(stereo, 2, 48_000).expect("合法"),
                128,
            ),
            1.0,
            0.0,
            false,
        );
        wait_producer_finished(&mixer);
        let mut mono_out = vec![0.0f32; frames];
        mixer.mix(&mut mono_out, 1, 48_000);
        assert!(
            mono_out.iter().all(|s| (s - 0.4).abs() < 1e-6),
            "立体声音乐下混应平均两声道，实际 {:?}",
            &mono_out[..4]
        );

        // 软限幅对音乐同样有效：两路满幅叠加钳到 1.0
        let mut mixer = AudioMixer::new();
        mixer.play(mono(&[1.0; 256]), 1.0, false);
        mixer.play_music(SampleStream::new(mono(&[1.0; 256]), 64), 1.0, 0.0, false);
        wait_producer_finished(&mixer);
        let mut hot = vec![0.0f32; 256];
        mixer.mix(&mut hot, 1, 48_000);
        assert!(hot.iter().all(|s| *s == 1.0), "2.0 叠加应钳到 1.0");
    }

    /// 重采样：44.1 kHz 音乐流混到 48 kHz 输出，与「同一源做成常驻剪辑走音效
    /// 路径」数值一致（块游标会周期性剪前缀，f64 舍入路径不同 ⇒ 只主张数值
    /// 等价、不主张逐位；采样率一致时的逐位等价由上面的直通测试锁定）。
    #[test]
    fn music_resamples_to_output_rate_like_voices() {
        let src = ramp_source(4_410); // 0.1 s @ 44.1 kHz
        let mut mixer = AudioMixer::new();
        mixer.play_music(
            SampleStream::new(
                AudioClip::from_interleaved(src.clone(), 1, 44_100).expect("合法"),
                1_000,
            ),
            1.0,
            0.0,
            false,
        );
        wait_producer_finished(&mixer);
        let mut out = vec![0.0f32; 4_800];
        mixer.mix(&mut out, 1, 48_000);

        let mut reference = AudioMixer::new();
        reference.play(
            AudioClip::from_interleaved(src, 1, 44_100).expect("合法"),
            1.0,
            false,
        );
        let mut want = vec![0.0f32; 4_800];
        reference.mix(&mut want, 1, 48_000);
        let worst = out
            .iter()
            .zip(&want)
            .fold(0.0f32, |acc, (a, b)| acc.max((a - b).abs()));
        assert!(
            worst < 1e-6,
            "流式与剪辑路径的重采样结果应数值一致，最大偏差 {worst}"
        );
        assert!(out.iter().any(|s| *s != 0.0), "重采样后仍应有信号");
    }

    /// 单轨语义 + 句柄识别：新轨硬拆上一轨、旧句柄不再是当前轨；病态流不启动。
    #[test]
    fn music_bus_is_single_track_and_handles_are_identifiable() {
        let mut mixer = AudioMixer::new();
        let first = mixer.play_music(
            SampleStream::new(mono(&ramp_source(600)), 300),
            1.0,
            0.0,
            true,
        );
        let second = mixer.play_music(
            SampleStream::new(mono(&ramp_source(600)), 300),
            1.0,
            0.0,
            true,
        );
        assert_ne!(first, second, "轨序号必须递增");
        assert_eq!(mixer.music_handle(), Some(second), "总线上只留最后一轨");
        assert!(mixer.stop_music(0.0));
        assert!(!mixer.stop_music(0.0), "空闲总线 stop 返回 false");
        assert_eq!(mixer.music_handle(), None);
        // stop 不销毁 JoinHandle（留作干净收尾）：第一次 join 回收线程、第二次无事可做
        wait_producer_finished(&mixer);
        assert!(mixer.join_music_producer(), "应能回收上一轨的解码线程");
        assert!(
            !mixer.join_music_producer(),
            "已回收后再次 join 返回 false（无新轨）"
        );

        /// 0 声道 / 0 采样率的病态流：不得占用总线。
        struct Broken;
        impl AudioStream for Broken {
            fn channels(&self) -> u16 {
                0
            }
            fn sample_rate(&self) -> u32 {
                0
            }
            fn next_block(&mut self, _out: &mut Vec<f32>) -> bool {
                false
            }
        }
        let broken = mixer.play_music(Broken, 1.0, 0.0, false);
        assert_eq!(mixer.music_handle(), None, "病态流不应占用总线");
        assert!(!mixer.music_is_playing());
        assert_ne!(mixer.music_handle(), Some(broken));
    }

    /// **混响接线**：不调 `set_reverb`、`wet = 0`、`wet = NaN`（卫生化为 0）
    /// 三条路径逐位一致；`wet > 0` 必须真的改变音乐输出且不产出病态值；
    /// 音效路径完全不受 `set_reverb` 影响（v1 声明的限制）。
    #[test]
    fn reverb_bypass_is_bit_exact_and_wet_affects_only_music() {
        let run = |settings: Option<ReverbSettings>| -> Vec<u32> {
            let src = ramp_source(4_000);
            let mut mixer = AudioMixer::new();
            if let Some(settings) = settings {
                mixer.set_reverb(settings);
            }
            mixer.play_music(SampleStream::new(mono(&src), 1_000), 1.0, 0.0, false);
            wait_producer_finished(&mixer);
            // 8 kHz 输出：comb / allpass 延迟缩到几十到几百帧，尾巴落在缓冲内
            let mut out = vec![0.0f32; src.len() + 2_000];
            mixer.mix(&mut out, 1, 8_000);
            out.iter().map(|s| s.to_bits()).collect()
        };
        let unset = run(None);
        let zero = run(Some(ReverbSettings {
            wet: 0.0,
            room_size: 0.7,
        }));
        let nan = run(Some(ReverbSettings {
            wet: f32::NAN,
            room_size: f32::INFINITY,
        }));
        assert_eq!(unset, zero, "wet = 0 应与「不启用混响」逐位一致");
        assert_eq!(zero, nan, "病态参数卫生化后应同为旁路");
        let wet = run(Some(ReverbSettings {
            wet: 0.4,
            room_size: 0.7,
        }));
        assert_ne!(unset, wet, "wet > 0 必须改变音乐总线输出");
        assert!(
            wet.iter().all(|bits| f32::from_bits(*bits).is_finite()),
            "混响不得产出 NaN / Inf"
        );

        // 音效路径不受 set_reverb 影响
        let src = ramp_source(64);
        let mut plain = AudioMixer::new();
        plain.play(mono(&src), 0.9, false);
        let mut out_plain = vec![0.0f32; 64];
        plain.mix(&mut out_plain, 1, 48_000);
        let mut reverbed = AudioMixer::new();
        reverbed.set_reverb(ReverbSettings {
            wet: 1.0,
            room_size: 1.0,
        });
        reverbed.play(mono(&src), 0.9, false);
        let mut out_reverbed = vec![0.0f32; 64];
        reverbed.mix(&mut out_reverbed, 1, 48_000);
        assert_eq!(
            out_plain, out_reverbed,
            "v1 混响不作用于音效（模块文档声明的限制）"
        );
        assert_eq!(
            reverbed.reverb_settings(),
            ReverbSettings {
                wet: 1.0,
                room_size: 1.0
            },
            "set_reverb 应可回读"
        );
    }

    /// 默认参数即旁路（不显式设置时混音行为与 R44 之前一致）。
    #[test]
    fn reverb_defaults_to_bypass() {
        let mixer = AudioMixer::new();
        assert_eq!(
            mixer.reverb_settings(),
            ReverbSettings::default(),
            "新建混音器的混响应为默认参数"
        );
        assert_eq!(mixer.reverb_settings().wet, 0.0, "默认 wet = 0 = 旁路");
    }

    /// 音乐路径确定性：同一源两次独立播放（含混响）逐位一致——块拼接与包络
    /// 都不引入与运行时机相关的量。
    #[test]
    fn music_playback_is_bit_deterministic_across_runs() {
        let run = || -> Vec<u32> {
            let src = ramp_source(3_000);
            let mut mixer = AudioMixer::new();
            mixer.set_reverb(ReverbSettings {
                wet: 0.3,
                room_size: 0.6,
            });
            mixer.play_music(SampleStream::new(mono(&src), 700), 0.7, 0.002, false);
            wait_producer_finished(&mixer);
            let mut out = vec![0.0f32; 3_000];
            mixer.mix(&mut out, 2, 48_000);
            out.iter().map(|s| s.to_bits()).collect()
        };
        assert_eq!(run(), run(), "同输入两次流式混音应逐位一致");
    }

    // ---- 空间化 v3（C6）：多普勒 + 遮挡低通 ----

    /// 多普勒静止锚：双方零速 ⇒ 因子**逐位** = 1.0（343/343 精确），
    /// 且听者速度默认位姿（from_view / Default）velocity 恒 ZERO。
    #[test]
    fn doppler_stationary_is_bitwise_one() {
        let f = doppler_factor(Vec3::X * 10.0, Vec3::ZERO, Vec3::ZERO, 343.0);
        assert_eq!(f.to_bits(), 1.0f32.to_bits(), "静止因子必须逐位 1.0");
        assert_eq!(
            ListenerParams::default().velocity,
            Vec3::ZERO,
            "默认听者速度 = 静止（v3 缺席形态）"
        );
        assert_eq!(
            ListenerParams::from_view(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y).velocity,
            Vec3::ZERO,
            "from_view 不引入速度"
        );
    }

    /// 多普勒双向径向合成（解析值 ±1e-5）：声源趋近 c 的 10% ⇒
    /// f = 343/(343−34.3) ≈ 1.1111；听者趋近同速 ⇒ f = (343+34.3)/343 = 1.1。
    /// 两者**不相等**是声学不对称的本体（介质参考系），不是实现噪声。
    #[test]
    fn doppler_two_way_radial_composition_matches_analytic() {
        let offset = Vec3::X * 10.0; // 源在听者 +X 方向
        let toward_source = Vec3::X * 34.3; // 听者趋近（v_o = +34.3）
        let toward_listener = Vec3::NEG_X * 34.3; // 声源趋近（v_s = +34.3）
        let f_src = doppler_factor(offset, Vec3::ZERO, toward_listener, 343.0);
        assert!(
            (f_src - 343.0 / (343.0 - 34.3)).abs() < 1e-5,
            "声源趋近因子解析界：{f_src}"
        );
        let f_lst = doppler_factor(offset, toward_source, Vec3::ZERO, 343.0);
        assert!(
            (f_lst - (343.0 + 34.3) / 343.0).abs() < 1e-5,
            "听者趋近因子解析界：{f_lst}"
        );
        assert!(f_src > 1.0 && f_lst > 1.0, "趋近 ⇒ 升频");
        assert!(
            (f_src - f_lst).abs() > 0.005,
            "双向合成是两条独立通道（不对称可测）"
        );
        // 逃离取反：横向运动（径向分量为零）⇒ 因子恒 1.0
        let lateral = doppler_factor(offset, Vec3::NEG_Z * 100.0, Vec3::Z * 100.0, 343.0);
        assert_eq!(lateral.to_bits(), 1.0f32.to_bits(), "纯横向无多普勒");
        let recede = doppler_factor(offset, Vec3::ZERO, Vec3::X * 34.3, 343.0);
        assert!(recede < 1.0, "声源逃离 ⇒ 降频：{recede}");
    }

    /// 多普勒病态防护（用户裁定钳 [⅓, 3]）：音速 / 超音速、NaN 速度、
    /// 零距重叠、坏声速一律有界或恒等，绝不传播病态值。
    #[test]
    fn doppler_clamps_pathologies() {
        let offset = Vec3::X * 10.0;
        // 声源以音速迎面冲来（den = 0）⇒ 钳上限 3.0
        assert_eq!(
            doppler_factor(offset, Vec3::ZERO, Vec3::NEG_X * 343.0, 343.0),
            DOPPLER_FACTOR_MAX
        );
        // 超音速（den < 0）同样钳上限
        assert_eq!(
            doppler_factor(offset, Vec3::ZERO, Vec3::NEG_X * 700.0, 343.0),
            DOPPLER_FACTOR_MAX
        );
        // 听者以超音速逃离（源在 +X，逃离 = 向 −X）⇒ 钳下限
        assert_eq!(
            doppler_factor(offset, Vec3::NEG_X * 400.0, Vec3::ZERO, 343.0),
            DOPPLER_FACTOR_MIN
        );
        // 一般超钳制场景（300 m/s 迎面，未超音速但因子 > 3）⇒ 钳 3.0
        assert_eq!(
            doppler_factor(offset, Vec3::ZERO, Vec3::NEG_X * 300.0, 343.0),
            DOPPLER_FACTOR_MAX
        );
        // 病态输入 ⇒ 恒 1.0（不 panic、不 NaN）
        let nan = Vec3::new(f32::NAN, 0.0, 0.0);
        assert_eq!(
            doppler_factor(offset, nan, Vec3::ZERO, 343.0).to_bits(),
            1.0f32.to_bits()
        );
        assert_eq!(
            doppler_factor(Vec3::ZERO, Vec3::ZERO, Vec3::X, 343.0).to_bits(),
            1.0f32.to_bits(),
            "重叠源径向无定义"
        );
        assert_eq!(
            doppler_factor(offset, Vec3::ZERO, Vec3::ZERO, 0.0).to_bits(),
            1.0f32.to_bits()
        );
        assert_eq!(
            doppler_factor(offset, Vec3::ZERO, Vec3::ZERO, f32::NAN).to_bits(),
            1.0f32.to_bits()
        );
    }

    /// 遮挡系数解析锚：0 ⇒ 1（恒等，但消费方走旁路）、1 ⇒ 0.1、
    /// 越界钳 [0,1]、NaN ⇒ 透明。
    #[test]
    fn muffle_coef_analytic_anchors() {
        assert_eq!(muffle_coef(0.0).to_bits(), 1.0f32.to_bits());
        assert!(
            (muffle_coef(1.0) - 0.1).abs() < 1e-6,
            "k(1) = 0.1：{k}",
            k = muffle_coef(1.0)
        );
        assert!((muffle_coef(0.5) - 0.55).abs() < 1e-6);
        assert!(
            (muffle_coef(2.0) - muffle_coef(1.0)).abs() < 1e-9,
            "越界钳 1"
        );
        assert_eq!(
            muffle_coef(-3.0).to_bits(),
            muffle_coef(0.0).to_bits(),
            "负越界钳 0"
        );
        assert_eq!(
            muffle_coef(f32::NAN).to_bits(),
            1.0f32.to_bits(),
            "病态 = 透明"
        );
    }

    /// 遮挡低通端到端（阶跃解析）：muffle=1（k=0.1）播全 1 阶跃，
    /// 滤波输出逐样本 = 1−(1−k)^n 的一阶爬坡——前四样本逐位对解析式；
    /// 同时验证**跨 mix 调用状态持久**（分两半缓冲推进，接缝处连续）。
    #[test]
    fn occlusion_lowpass_ramp_is_analytic_and_stateful_across_mixes() {
        let step = vec![1.0f32; 8];
        let mut mixer = AudioMixer::new();
        let id = mixer.play(mono(&step), 1.0, false);
        assert!(mixer.set_sound_muffle(id, 1.0));
        assert_eq!(mixer.sound_muffle(id), Some(1.0), "观测口回读强度");
        let k = muffle_coef(1.0);
        let mut first = vec![0.0f32; 2 * 4]; // 4 帧立体声
        mixer.mix(&mut first, 2, 48_000);
        // 解析爬坡：y_n = 1-(1-k)^n（n 从 1 起）
        let mut prev = 0.0f32;
        for n in 0..4 {
            let expect_l = prev + k * (1.0 - prev);
            assert_eq!(
                first[2 * n].to_bits(),
                expect_l.to_bits(),
                "第 {n} 样本应为一阶爬坡（左声道）"
            );
            assert_eq!(first[2 * n], first[2 * n + 1], "单声道源两输出声道同滤波");
            prev = expect_l;
        }
        // 第二半缓冲：状态从上一缓冲末值继续（跨调用持久 ⇒ 无缝）
        let mut second = vec![0.0f32; 2 * 4];
        mixer.mix(&mut second, 2, 48_000);
        for n in 0..4 {
            let expect_l = prev + k * (1.0 - prev);
            assert_eq!(second[2 * n].to_bits(), expect_l.to_bits(), "接缝爬坡连续");
            prev = expect_l;
        }
    }

    /// 旁路逐位锚：muffle=0（显式设 0 与从不设两型）与未走滤波分支的
    /// 对照混音器**逐位一致**；「先闷后解」后新输出与「从未闷、只跳过
    /// 同样帧数」的对照逐位一致——旁路不读不写状态，被污染的滤波记忆
    /// 绝不回渗直通路径。
    #[test]
    fn muffle_bypass_is_bit_identical_and_release_restores_passthrough() {
        let src: Vec<f32> = (0..32).map(|i| (i as f32 * 0.4).sin()).collect();
        let bits = |v: &[f32]| -> Vec<u32> { v.iter().map(|s| s.to_bits()).collect() };
        // ① 显式 0 与从不设：整段逐位一致
        let mut plain = AudioMixer::new();
        plain.play(mono(&src), 0.8, false);
        let mut with_zero = AudioMixer::new();
        let id = with_zero.play(mono(&src), 0.8, false);
        assert!(with_zero.set_sound_muffle(id, 0.0), "显式 0 句柄有效");
        let mut a = vec![0.0f32; 2 * 32];
        let mut b = vec![0.0f32; 2 * 32];
        plain.mix(&mut a, 2, 48_000);
        with_zero.mix(&mut b, 2, 48_000);
        assert_eq!(bits(&a), bits(&b), "muffle=0 必须与旁路逐位一致");
        // ② 闷 8 帧 → 放开 24 帧：与「从未闷、跳 8 帧混 24 帧」对照逐位一致
        //   （若放开后仍读旧滤波记忆，输出起点会被爬坡残值污染 ⇒ 失配）
        let mut muted = AudioMixer::new();
        let id = muted.play(mono(&src), 0.8, false);
        assert!(muted.set_sound_muffle(id, 1.0));
        let mut damp = vec![0.0f32; 2 * 8];
        muted.mix(&mut damp, 2, 48_000);
        assert!(muted.set_sound_muffle(id, 0.0));
        let mut released = vec![0.0f32; 2 * 24];
        muted.mix(&mut released, 2, 48_000);
        let mut reference = AudioMixer::new();
        reference.play(mono(&src), 0.8, false);
        let mut skip = vec![0.0f32; 2 * 8];
        reference.mix(&mut skip, 2, 48_000);
        let mut rest = vec![0.0f32; 2 * 24];
        reference.mix(&mut rest, 2, 48_000);
        assert_eq!(bits(&released), bits(&rest), "放开遮挡后不得有滤波残值回渗");
    }

    /// 多普勒端到端：声源以 34.3 m/s 迎面趋近（因子 ≈1.111），440 Hz
    /// 正弦的过零率实测升高；静止对照逐位 = 默认路径（零速写入不改行为），
    /// 且两次运行逐位确定。
    #[test]
    fn doppler_shifts_pitch_end_to_end_and_stationary_anchor_holds() {
        let clip = mono(sine(440.0, 0.05, 48_000).samples());
        let zero_crossings = |mixer: &mut AudioMixer| -> usize {
            let mut out = vec![0.0f32; 2 * 1_200];
            mixer.mix(&mut out, 2, 48_000);
            let mut prev: Option<f32> = None;
            let mut count = 0;
            for frame in out.chunks_exact(2) {
                let s = frame[0];
                if let Some(p) = prev {
                    if (p < 0.0) != (s < 0.0) {
                        count += 1;
                    }
                }
                prev = Some(s);
            }
            count
        };
        let build = |velocity: Vec3| {
            let mut mixer = AudioMixer::new();
            mixer.set_listener(ListenerParams::default());
            let id = mixer.play_spatial(clip.clone(), Vec3::X * 10.0, 1.0, true);
            assert!(mixer.set_sound_velocity(id, velocity));
            mixer
        };
        let mut still = build(Vec3::ZERO);
        let mut approaching = build(Vec3::NEG_X * 34.3);
        let base = zero_crossings(&mut still);
        let shifted = zero_crossings(&mut approaching);
        assert!(shifted > base, "趋近源过零率应更高：{shifted} > {base}");
        // 非空间声音不吃多普勒（无径向定义）：给速度后输出逐位 = 对照
        let mut ns_plain = AudioMixer::new();
        ns_plain.play(clip.clone(), 1.0, true);
        let mut ns_vel = AudioMixer::new();
        let id = ns_vel.play(clip.clone(), 1.0, true);
        assert!(
            ns_vel.set_sound_velocity(id, Vec3::NEG_X * 34.3),
            "存而不用仍受理"
        );
        let mut x = vec![0.0f32; 2 * 600];
        let mut y = vec![0.0f32; 2 * 600];
        ns_plain.mix(&mut x, 2, 48_000);
        ns_vel.mix(&mut y, 2, 48_000);
        assert_eq!(
            x.iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
            y.iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
            "非空间声音速度存而不用（逐位对照）"
        );
        // 声速旋钮非默认值断言：c = 171.5（半速介质）同速源因子翻倍于 c=343
        let f_half = doppler_factor(Vec3::X * 10.0, Vec3::ZERO, Vec3::NEG_X * 34.3, 171.5);
        let f_full = doppler_factor(Vec3::X * 10.0, Vec3::ZERO, Vec3::NEG_X * 34.3, 343.0);
        assert!(f_half > f_full, "c 越低多普勒越剧烈：{f_half} vs {f_full}");
        let mut mixer = AudioMixer::new();
        mixer.set_speed_of_sound(f32::NAN);
        assert_eq!(
            mixer.speed_of_sound(),
            DEFAULT_SPEED_OF_SOUND,
            "病态声速输入被忽略（默认 343）"
        );
        mixer.set_speed_of_sound(171.5);
        assert_eq!(mixer.speed_of_sound(), 171.5);
        // 两次运行逐位确定（同输入同序）
        let run1 = {
            let mut m = build(Vec3::NEG_X * 34.3);
            let mut out = vec![0.0f32; 2 * 1_200];
            m.mix(&mut out, 2, 48_000);
            out
        };
        let run2 = {
            let mut m = build(Vec3::NEG_X * 34.3);
            let mut out = vec![0.0f32; 2 * 1_200];
            m.mix(&mut out, 2, 48_000);
            out
        };
        assert_eq!(
            run1.iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
            run2.iter().map(|s| s.to_bits()).collect::<Vec<_>>()
        );
    }

    /// v3 句柄卫生：死句柄 / 旧世代的所有新 setter 一律 false；观测口
    /// `None`；`set_sound_muffle` 的 NaN 按 0（旁路）、越界钳 [0,1]。
    #[test]
    fn v3_setters_are_generation_guarded_and_sanitized() {
        let clip = mono(&[0.5; 8]);
        let mut mixer = AudioMixer::new();
        let id = mixer.play_spatial(clip.clone(), Vec3::Z * -3.0, 1.0, false);
        assert!(mixer.set_sound_velocity(id, Vec3::X));
        assert_eq!(mixer.sound_velocity(id), Some(Vec3::X));
        mixer.set_sound_muffle(id, 0.5);
        assert_eq!(mixer.sound_muffle(id), Some(0.5));
        mixer.set_sound_muffle(id, f32::NAN);
        assert_eq!(mixer.sound_muffle(id), Some(0.0), "NaN 按旁路");
        mixer.set_sound_muffle(id, 4.0);
        assert_eq!(mixer.sound_muffle(id), Some(1.0), "越界钳 1");
        assert!(mixer.stop(id));
        assert!(!mixer.set_sound_velocity(id, Vec3::ZERO), "死句柄 false");
        assert!(!mixer.set_sound_muffle(id, 1.0), "死句柄 false");
        assert_eq!(mixer.sound_muffle(id), None);
        assert_eq!(mixer.sound_velocity(id), None);
    }
}
