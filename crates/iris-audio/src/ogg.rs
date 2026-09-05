//! OGG / Vorbis 解码（R44）：一次性解码进内存（小音效）+ 分块解码流（背景音乐）。
//!
//! **依赖选型：`symphonia`（`ogg` + `vorbis` 子集），不用 `lewton`。**理由：
//!
//! | 维度 | symphonia 0.5 | lewton 0.10 |
//! |---|---|---|
//! | unsafe | 全家族 crate 顶部 `#![forbid(unsafe_code)]`——**编译器强制**，本仓库零 unsafe 定位可以直接依赖其保证 | 大量 `unsafe`（io / lz4 / 单元测试外的裸指针转换） |
//! | 解码形态 | `FormatReader::next_packet` + `Decoder::decode` 拉取式状态机，一次调用出一块 ⇒ 天然匹配「后台线程分块喂混音」 | `PacketRead` 迭代器 + 需自行拼页 / 处理 bos，接口偏底 |
//! | 依赖树 | `ogg + vorbis` 子集：arrayvec / bitflags / bytemuck / lazy_static / encoding_rs（无 rustfft / regex） | 自带 huffman / crc 实现，依赖较少但含 unsafe |
//! | 维护 | 上游活跃（0.5.x 稳定线 + 0.6 演进中） | 近数年基本停更 |
//!
//! 本仓库硬约束「全库零 unsafe，无例外」在这里的落地方式：iris-audio 自身
//! 代码零 `unsafe`，第三方解码器的安全性由 `forbid(unsafe_code)` 在编译器层面
//! 兜住（不公开裸指针 C API），因此不引入 `unsafe` 传递风险。
//!
//! 两条消费路径：
//! - [`parse_ogg`] / [`load_ogg`]：整条码流一次性解成交错 f32 采样，产出
//!   [`AudioClip`]——短音效走既有混音路径，零改动即可用 OGG 资产；
//! - [`OggVorbisStream`]：实现 [`AudioStream`]，按块解码喂给
//!   [`crate::AudioMixer::play_music`] 的流式通道 ⇒ **常驻内存与曲目长度无关**
//!   （见 `stream` 模块）。`from_bytes` 把压缩态留在内存（60 s 立体声
//!   ≈ 0.7 MB 量级），`open` 走 `File`（压缩态也不常驻，代价是解码线程持有
//!   一个文件句柄 + symphonia 内部 64 KB 读缓冲）。
//!
//! 采样口径：Vorbis 解码恒为 f32 平面 → 本模块按帧交错重排（`L0 R0 L1 R1…`），
//! 值原样搬运、不做增益缩放（逐采样直通，无浮点舍入差异）。
//!
//! 已知限制（v1，诚实清单）：
//! - **只支持 OGG 容器里的 Vorbis**（feature 只开了 `ogg + vorbis`）：其他
//!   容器 / 编码（MP3 / FLAC / Opus）报 [`AudioError::Unsupported`]，
//!   不做静默降级；声道数限 1..=8；
//! - **未做显式 gapless 裁剪**：`FormatOptions` 用默认值（`enable_gapless`
//!   未开），首尾是否带 Vorbis 编码窗的前振 / 填充取决于容器粒位置。
//!   实测本仓库 fixture（ffmpeg libvorbis 编码的 2 s 资产）解出**恰好 96000
//!   帧**、与时长精确相符 ⇒ 该路径下无额外首尾静音；不含粒位置提示的码流
//!   理论上可保留约 2 k 采样的头尾冗余（正确修法是按
//!   `codec_params.start_rendering` / `n_frames` 裁剪，留待后续轮次）；
//! - 解码中段的坏包（`DecodeError`）跳过该包继续（丢一小段而非整体失败），
//!   其他错误（IO / 容器损坏）记 `log::warn` 并按流结束处理——`next_block`
//!   不返回错误，只有 `parse_ogg` 这类一次性路径能把错误回传给调用方。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use symphonia::core::audio::{AudioBuffer, AudioBufferRef, Signal};
use symphonia::core::codecs::{CodecRegistry, Decoder, DecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::{Hint, Probe};

use crate::AudioError;
use crate::clip::AudioClip;
use crate::stream::{AudioStream, DEFAULT_BLOCK_FRAMES};

/// 本解码器接受的声道数上限（Vorbis 规范上限即 8）。
const MAX_CHANNELS: u16 = 8;

/// 从文件加载 OGG/Vorbis 并整条解码进内存（短音效路径）。
pub fn load_ogg(path: impl AsRef<Path>) -> Result<AudioClip, AudioError> {
    let bytes = std::fs::read(path)?;
    parse_ogg(&bytes)
}

/// 解析内存中的 OGG/Vorbis 字节，整条解码成交错 f32 剪辑。
///
/// 长配乐请改用 [`OggVorbisStream`]（流式），本函数的内存占用 =
/// 采样数 × 4 字节（60 s 立体声 48 kHz ≈ 22 MB）。
pub fn parse_ogg(bytes: &[u8]) -> Result<AudioClip, AudioError> {
    let mut stream = OggVorbisStream::from_bytes(bytes.to_vec())?;
    let mut samples = Vec::new();
    stream.drain_to_end(&mut samples)?;
    AudioClip::from_interleaved(samples, stream.channels, stream.sample_rate)
}

/// 压缩码流的来源：内存字节（可多处共享）或磁盘文件。
enum OggSource {
    /// 整条压缩码流常驻（约 128 kbps ⇒ 60 s ≈ 1 MB）；rewind 时重建
    /// demuxer 需复制一次字节（`MediaSourceStream` 接管所有权）。
    Bytes(Arc<Vec<u8>>),
    /// 只持有路径：压缩态不常驻，代价是每次打开一个 `File`。
    File(PathBuf),
}

impl OggSource {
    fn open(&self) -> Result<Box<dyn MediaSource>, AudioError> {
        match self {
            Self::Bytes(bytes) => Ok(Box::new(std::io::Cursor::new(bytes.as_ref().clone()))),
            Self::File(path) => Ok(Box::new(std::fs::File::open(path)?)),
        }
    }
}

/// 一条已建好的码流（demuxer + decoder + 目标轨）。
struct Decoded {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
}

/// 单包解码的进展。
enum Progress {
    /// 解出了一块采样。
    Decoded,
    /// 码流读到尽头。
    End,
}

/// OGG/Vorbis 流式解码源（实现 [`AudioStream`]，喂音乐总线）。
///
/// 构造即完成 probe + 建解码器（因此「文件不存在 / 不是 OGG / 编码不支持」
/// 在构造点就报 [`AudioError`]，不会拖到第一次混音）；此后只按需解码。
pub struct OggVorbisStream {
    source: OggSource,
    channels: u16,
    sample_rate: u32,
    /// 目标块长（帧）。实际块以解码包为界，可能略多或略少于该值。
    block_frames: usize,
    /// `None` = 已解到尽头（`rewind` 会重建）。
    decoded: Option<Decoded>,
}

impl OggVorbisStream {
    /// 从内存中的 OGG 字节构造。
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, AudioError> {
        Self::new(OggSource::Bytes(Arc::new(bytes)))
    }

    /// 从磁盘路径构造（压缩态不常驻内存，见 [`OggSource`]）。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AudioError> {
        Self::new(OggSource::File(path.as_ref().to_path_buf()))
    }

    /// 覆写目标块长（帧，0 归一为 1）：调小降低延迟、调大减少唤醒次数。
    pub fn with_block_frames(mut self, block_frames: usize) -> Self {
        self.block_frames = block_frames.max(1);
        self
    }

    /// 解码到尽头（一次性路径，错误回传调用方）。
    fn drain_to_end(&mut self, out: &mut Vec<f32>) -> Result<(), AudioError> {
        while self.fill(out, usize::MAX)? {}
        Ok(())
    }

    fn new(source: OggSource) -> Result<Self, AudioError> {
        let (decoded, channels, sample_rate) = build(&source)?;
        Ok(Self {
            source,
            channels,
            sample_rate,
            block_frames: DEFAULT_BLOCK_FRAMES,
            decoded: Some(decoded),
        })
    }

    /// 继续解码并**追加**采样到 `out`，直到本次追加满 `limit` 帧或码流尽头。
    /// 返回 `true` = 已满一块（后面还有内容），`false` = 码流结束。
    ///
    /// 上限针对本次追加量（而非 `out` 的总长）⇒ 调用方可以把多块累积进
    /// 同一个缓冲（`drain_to_end` 与集成测试皆依赖此语义）。
    fn fill(&mut self, out: &mut Vec<f32>, limit: usize) -> Result<bool, AudioError> {
        let channels = self.channels as usize;
        let start = out.len() / channels;
        loop {
            if out.len() / channels - start >= limit {
                return Ok(true);
            }
            let decoded = match self.decoded.as_mut() {
                Some(decoded) => decoded,
                None => return Ok(false),
            };
            match decode_packet(decoded, out) {
                Ok(Progress::Decoded) => {}
                Ok(Progress::End) => {
                    self.decoded = None;
                    return Ok(false);
                }
                Err(err) => {
                    self.decoded = None;
                    return Err(err);
                }
            }
        }
    }
}

impl AudioStream for OggVorbisStream {
    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn next_block(&mut self, out: &mut Vec<f32>) -> bool {
        let limit = self.block_frames;
        match self.fill(out, limit) {
            Ok(more) => more,
            Err(err) => {
                // 流式路径没有回传错误的通道（解码跑在后台线程）：记录并按结束处理
                log::warn!("OGG 解码中断，音乐流按结束处理：{err}");
                false
            }
        }
    }

    fn rewind(&mut self) -> bool {
        // 循环 = 重建 demuxer / decoder（不走 seek：省掉「seek 后解码器状态
        // 一致性」这一类容易出错的空间；代价是一次 re-probe）
        match build(&self.source) {
            Ok((decoded, _, _)) => {
                self.decoded = Some(decoded);
                true
            }
            Err(err) => {
                log::warn!("OGG 循环重播失败：{err}");
                false
            }
        }
    }
}

/// probe 容器 + 建解码器，并回读布局（声道数 / 采样率）。
fn build(source: &OggSource) -> Result<(Decoded, u16, u32), AudioError> {
    let mss = MediaSourceStream::new(source.open()?, Default::default());
    let mut probe = Probe::default();
    symphonia::default::register_enabled_formats(&mut probe);
    let format = match probe.format(
        &Hint::new(),
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) {
        Ok(result) => result.format,
        Err(err) => return Err(classify(err)),
    };
    let (track_id, params) = match format.default_track() {
        Some(track) => (track.id, track.codec_params.clone()),
        None => return Err(AudioError::Format("OGG 容器内没有音频轨".into())),
    };
    let channels = params
        .channels
        .map(|channels| channels.count() as u16)
        .ok_or_else(|| AudioError::Format("容器未声明声道数".into()))?;
    let sample_rate = params
        .sample_rate
        .ok_or_else(|| AudioError::Format("容器未声明采样率".into()))?;
    if channels > MAX_CHANNELS {
        return Err(AudioError::Unsupported(format!(
            "声道数 {channels} 超出支持上限 {MAX_CHANNELS}"
        )));
    }
    let mut codecs = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut codecs);
    let decoder = match codecs.make(&params, &DecoderOptions::default()) {
        Ok(decoder) => decoder,
        Err(err) => return Err(classify(err)),
    };
    Ok((
        Decoded {
            reader: format,
            decoder,
            track_id,
        },
        channels,
        sample_rate,
    ))
}

/// 解一个包并把采样按帧交错追加到 `out`（坏包跳过、继续读下一包）。
fn decode_packet(decoded: &mut Decoded, out: &mut Vec<f32>) -> Result<Progress, AudioError> {
    let Decoded {
        reader,
        decoder,
        track_id,
    } = decoded;
    loop {
        let packet = match reader.next_packet() {
            Ok(packet) => packet,
            // symphonia 的尽头信号：OggReader 读到流尾以 UnexpectedEof 表达
            Err(Error::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(Progress::End);
            }
            Err(err) => return Err(classify(err)),
        };
        if packet.track_id() != *track_id {
            continue; // 多轨容器：只解默认轨
        }
        match decoder.decode(&packet) {
            Ok(buffer) => {
                let AudioBufferRef::F32(planar) = &buffer else {
                    return Err(AudioError::Unsupported(
                        "解码输出不是 f32 平面格式（本模块只接 Vorbis）".into(),
                    ));
                };
                interleave(planar, out);
                return Ok(Progress::Decoded);
            }
            // 坏包 / 需要重置：按 symphonia 约定丢弃该包继续（丢一小段而非整体失败）
            Err(Error::DecodeError(_)) | Err(Error::ResetRequired) => continue,
            Err(err) => return Err(classify(err)),
        }
    }
}

/// 平面 f32 → 交错帧（`L0 R0 L1 R1…`），值原样搬运。
fn interleave(planar: &AudioBuffer<f32>, out: &mut Vec<f32>) {
    let frames = planar.frames();
    let planes = planar.planes();
    let planes = planes.planes();
    for frame in 0..frames {
        for plane in planes.iter() {
            out.push(plane[frame]);
        }
    }
}

/// symphonia 错误 → [`AudioError`] 分类（口径同 `wav.rs`：损坏 = Format、
/// 合法但不支持的特性 = Unsupported、真实 IO 失败 = Io）。
fn classify(err: Error) -> AudioError {
    match err {
        Error::IoError(err) => AudioError::Io(err),
        Error::Unsupported(feature) => AudioError::Unsupported(feature.to_string()),
        Error::DecodeError(desc) | Error::LimitError(desc) => AudioError::Format(desc.to_string()),
        Error::SeekError(kind) => AudioError::Format(format!("容器定位失败：{kind:?}")),
        Error::ResetRequired => AudioError::Format("解码器需要重置（流结构异常）".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 非 OGG 字节（空 / 垃圾 / WAV 头）必须报错而不是 panic 或静默产出空剪辑。
    #[test]
    fn rejects_non_ogg_input() {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF\x24\x00\x00\x00WAVEfmt \x10\x00\x00\x00");
        for bytes in [&[][..], b"not an ogg file at all", wav.as_slice()] {
            assert!(
                matches!(
                    parse_ogg(bytes),
                    Err(AudioError::Format(_)) | Err(AudioError::Unsupported(_))
                ),
                "非 OGG 输入应报错（前 4 字节 {:?}）",
                &bytes[..bytes.len().min(4)]
            );
            // 构造点即报错：流式路径没有回传错误的通道，不能让坏文件活到第一次混音
            assert!(OggVorbisStream::from_bytes(bytes.to_vec()).is_err());
        }
        assert!(OggVorbisStream::open("不存在的文件.ogg").is_err());
    }

    // 真实 OGG 资产（采样率 / 声道 / 长度 / 确定性 / 流式一致性）的验证需要
    // fixture 文件，放在 `crates/iris-audio/tests/ogg_vorbis.rs` 集成测试里。
}
