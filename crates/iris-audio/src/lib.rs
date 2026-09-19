//! # iris-audio
//!
//! Iris 音频子系统：纯软件混音器 + cpal 设备层 + WAV 加载 + 程序化音效生成。
//!
//! 分层设计（可测性优先）：
//! - **混音核心**（[`AudioMixer`]）全部纯函数、不接触任何设备——采样推进
//!   由调用方每帧调用 [`AudioMixer::mix`] 填充输出缓冲驱动，采样级数学
//!   可在单测中确定性断言（音量、叠加、循环回绕、重采样、限幅、空间化
//!   衰减 / 声像，后者见 [`spatial_gain`]）；
//! - **设备层**（[`AudioOutput`]）只是把 [`AudioMixer`] 接到 cpal 输出流的
//!   薄壳：实时回调内 `try_lock` 混音器，拿不到锁立即填静音（实时线程
//!   绝不阻塞）；无设备 / wasm32 下优雅降级（[`AudioOutput::device`]
//!   返回 `None`，混音核心照常可用）。
//!
//! ```ignore
//! let clip = iris_audio::load_wav("assets/boom.wav")?;
//! let mut mixer = AudioMixer::new();
//! let id = mixer.play(clip, 0.8, false);
//! let mut buf = vec![0.0; 480];          // 10ms 立体声 @ 48kHz
//! mixer.mix(&mut buf, 2, 48_000);        // 推进混音（或由设备回调驱动）
//! ```
//!
//! 空间化 v1：[`AudioMixer::set_listener`] + [`AudioMixer::play_spatial`] +
//! [`AudioMixer::set_sound_position`]——距离软倒数衰减 + 恒功率立体声声像
//! （模型与已知限制见 `mixer` 模块文档）。
//!
//! 空间化 v3（C6）：多普勒（[`doppler_factor`] +
//! [`AudioMixer::set_sound_velocity`] / [`ListenerParams::with_velocity`]）
//! 与遮挡闷声（[`AudioMixer::set_sound_muffle`]，逐声音一阶低通，
//! [`muffle_coef`]）——两件全 opt-in，静止 / 未遮挡 = 与 v1/v2 逐位一致；
//! HRTF 维持裁出（「前后不可辨」限制原样）。遮挡**判定**（射线）在引擎侧
//! （`iris_engine::spatial`），混音器只吃强度标量。
//!
//! 音频 v2（R44）：OGG/Vorbis 解码（[`AudioClip::from_ogg`] 小音效进内存、
//! [`OggVorbisStream`] 分块解码喂流式通道）+ 流式音乐总线
//! （[`AudioMixer::play_music`] / [`AudioMixer::stop_music`]，常驻内存与曲目
//! 长度无关：8×4096 帧有界队列 + 待播块 + demuxer 读缓冲，60 s 立体声实测
//! ≈ 0.4 MB，而同内容 WAV 全量常驻 22 MB）+ 音乐总线混响
//! （[`AudioMixer::set_reverb`]，简化 Schroeder 链）。机制与诚实限制分别见
//! `stream` / `reverb` / `ogg` 模块文档。

use std::fmt;

pub mod clip;
pub mod device;
pub mod mixer;
pub mod ogg;
pub mod procedural;
pub mod reverb;
pub mod stream;
pub mod wav;

pub use clip::AudioClip;
pub use device::AudioOutput;
pub use mixer::{
    AudioMixer, DEFAULT_REFERENCE_DISTANCE, DEFAULT_SPEED_OF_SOUND, DOPPLER_FACTOR_MAX,
    DOPPLER_FACTOR_MIN, ListenerParams, SoundId, doppler_factor, muffle_coef, spatial_gain,
};
pub use ogg::{OggVorbisStream, load_ogg, parse_ogg};
pub use procedural::{noise, sine, square};
pub use reverb::{DelayLine, Reverb, ReverbSettings};
pub use stream::{
    AudioStream, DEFAULT_BLOCK_FRAMES, DEFAULT_QUEUE_BLOCKS, MusicHandle, SampleStream,
};
pub use wav::{load_wav, parse_wav};

/// 音频子系统错误。风格与 iris-render 的 `GltfError` / `HdrError` 一致：
/// Io 是真实的 IO 失败；Format 表示数据损坏（结构与规范不符）；
/// Unsupported 表示文件合法但用了本实现不支持的特性（诚实报错而非静默出错）。
#[derive(Debug)]
pub enum AudioError {
    Io(std::io::Error),
    /// 数据损坏：结构不符或参数非法。
    Format(String),
    /// 文件合法但使用了本加载器不支持的特性（如非 PCM 编码、非 16-bit 位深）。
    Unsupported(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO 错误：{e}"),
            Self::Format(what) => write!(f, "音频格式错误：{what}"),
            Self::Unsupported(what) => write!(f, "暂不支持的音频特性：{what}"),
        }
    }
}

impl std::error::Error for AudioError {}

impl From<std::io::Error> for AudioError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
