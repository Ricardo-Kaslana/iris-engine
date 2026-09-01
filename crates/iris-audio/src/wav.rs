//! WAV（RIFF 容器 + PCM 编码）加载：手写解析器，零新依赖（风格同
//! `iris-render/src/asset/hdr.rs`）。
//!
//! 支持：
//! - 魔数 `"RIFF"` / `"WAVE"`；块（chunk）按 RIFF 规范 2 字节对齐
//!   （奇数长度补 1 填充字节），未知块（LIST / fact 等）跳过；
//! - `fmt ` 块：无压缩 PCM（format = 1）、16-bit 位深、声道 1/2、
//!   任意采样率。其他编码 / 位深 / 声道数 → [`AudioError::Unsupported`]
//!   （诚实报错而非静默出错，风格同 `GltfError`）；
//! - `data` 块：16-bit 小端采样 → f32 / 32768（-32768 映射 -1.0、
//!   32767 映射 0.99997，全值域映射无削顶）；多声道帧交错布局原样保留。
//!
//! 容错约定：RIFF 头的总长字段仅跳过不校验（现实中的文件常写错该字段）；
//! 块声明长度超出文件时按文件实际长度截断，随后 `data` 的块对齐校验
//! 仍会拦住真正损坏的文件。

use std::path::Path;

use crate::AudioError;
use crate::clip::AudioClip;

/// `fmt ` 块中本实现关心的字段。
struct FmtChunk {
    channels: u16,
    sample_rate: u32,
}

/// 从文件加载 WAV。
pub fn load_wav(path: impl AsRef<Path>) -> Result<AudioClip, AudioError> {
    let bytes = std::fs::read(path)?;
    parse_wav(&bytes)
}

/// 解析内存中的 WAV 字节（测试与工具链入口）。
pub fn parse_wav(bytes: &[u8]) -> Result<AudioClip, AudioError> {
    // RIFF 头：12 字节 = "RIFF" + u32 总长 + "WAVE"
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        let head = bytes
            .get(0..4)
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_else(|| "<空>".into());
        return Err(AudioError::Format(format!(
            "RIFF/WAVE 魔数不符（文件大小 {} 字节，前 4 字节 {head:?}）",
            bytes.len()
        )));
    }

    // 单遍扫描块：fmt 记录参数、data 记录字节区间（两者先后顺序不作要求）
    let (mut fmt, mut data): (Option<FmtChunk>, Option<(usize, usize)>) = (None, None);
    let mut pos = 12usize;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let start = pos + 8;
        // 声明长度超出文件时截断到文件尾（容错损坏的长度字段）
        let end = start.saturating_add(size).min(bytes.len());
        match id {
            b"fmt " => fmt = Some(parse_fmt(&bytes[start..end])?),
            b"data" => data = Some((start, end)),
            _ => {} // LIST / fact / 未知块：跳过
        }
        // 块按 2 字节对齐：奇数长度补 1 填充字节（RIFF 规范）
        pos = start.saturating_add(size).saturating_add(size & 1);
    }

    let fmt = fmt.ok_or_else(|| AudioError::Format("缺少 fmt 块".into()))?;
    let (start, end) = data.ok_or_else(|| AudioError::Format("缺少 data 块".into()))?;
    let raw = &bytes[start..end];
    let block_align = fmt.channels as usize * 2;
    if raw.is_empty() {
        return Err(AudioError::Format("data 块为空（无采样数据）".into()));
    }
    if raw.len() % block_align != 0 {
        return Err(AudioError::Format(format!(
            "data 块长度 {} 不是块对齐 {block_align} 的整数倍（数据被截断）",
            raw.len()
        )));
    }
    // 16-bit 小端 → f32 / 32768：全值域映射（-32768 → -1.0，32767 → 0.99997）
    let mut samples = Vec::with_capacity(raw.len() / 2);
    for pair in raw.chunks_exact(2) {
        let s = i16::from_le_bytes([pair[0], pair[1]]);
        samples.push(s as f32 / 32768.0);
    }
    AudioClip::from_interleaved(samples, fmt.channels, fmt.sample_rate)
}

/// 解析 `fmt ` 块（至少 16 字节 PCM 头；扩展字段只忽略）。
fn parse_fmt(payload: &[u8]) -> Result<FmtChunk, AudioError> {
    if payload.len() < 16 {
        return Err(AudioError::Format(format!(
            "fmt 块不足 16 字节（实际 {} 字节）",
            payload.len()
        )));
    }
    let u16_at = |i: usize| u16::from_le_bytes([payload[i], payload[i + 1]]);
    let format = u16_at(0);
    if format != 1 {
        return Err(AudioError::Unsupported(format!(
            "仅支持无压缩 PCM（format=1），实际 format={format}"
        )));
    }
    let channels = u16_at(2);
    let sample_rate = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let bits = u16_at(14);
    if bits != 16 {
        return Err(AudioError::Unsupported(format!(
            "仅支持 16-bit 位深，实际 {bits}-bit"
        )));
    }
    if channels != 1 && channels != 2 {
        return Err(AudioError::Unsupported(format!(
            "仅支持单声道 / 立体声，实际 {channels} 声道"
        )));
    }
    if sample_rate == 0 {
        return Err(AudioError::Format("采样率为 0".into()));
    }
    Ok(FmtChunk {
        channels,
        sample_rate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手写标准 WAV 字节：12 字节 RIFF 头 + 24 字节 fmt + 8 字节 data 头 + PCM。
    fn wav_bytes(channels: u16, sample_rate: u32, bits: u16, format: u16, pcm: &[i16]) -> Vec<u8> {
        let data_bytes = (pcm.len() * 2) as u32;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        b.extend_from_slice(b"WAVE");
        b.extend_from_slice(b"fmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&format.to_le_bytes());
        b.extend_from_slice(&channels.to_le_bytes());
        b.extend_from_slice(&sample_rate.to_le_bytes());
        b.extend_from_slice(&(sample_rate * channels as u32 * 2).to_le_bytes()); // byte_rate
        b.extend_from_slice(&(channels * 2).to_le_bytes()); // block_align
        b.extend_from_slice(&bits.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_bytes.to_le_bytes());
        for s in pcm {
            b.extend_from_slice(&s.to_le_bytes());
        }
        b
    }

    /// 单声道值校验：16-bit → f32 / 32768 的全值域映射逐点核对。
    #[test]
    fn parse_mono_values() {
        let pcm = [0i16, 16384, -16384, 32767, -32768];
        let bytes = wav_bytes(1, 8_000, 16, 1, &pcm);
        let clip = parse_wav(&bytes).expect("解析应成功");
        assert_eq!((clip.channels(), clip.sample_rate()), (1, 8_000));
        assert_eq!(clip.frame_count(), 5);
        let expected = [0.0, 0.5, -0.5, 32_767.0 / 32_768.0, -1.0];
        for (got, want) in clip.samples().iter().zip(expected) {
            assert!((got - want).abs() < 1e-6, "{got} vs {want}");
        }
        assert!((clip.duration_secs() - 5.0 / 8_000.0).abs() < 1e-6);
    }

    /// 立体声帧交错布局原样保留（L/R/L/R…）。
    #[test]
    fn parse_stereo_keeps_interleave() {
        let pcm = [100i16, -100, 200, -200];
        let clip = parse_wav(&wav_bytes(2, 44_100, 16, 1, &pcm)).expect("解析应成功");
        assert_eq!((clip.channels(), clip.frame_count()), (2, 2));
        for (got, want) in clip.samples().iter().zip(pcm) {
            let want = want as f32 / 32768.0;
            assert!((got - want).abs() < 1e-6);
        }
    }

    /// 任意采样率直通（含非标准值）；未知块（奇数长度 + 填充字节）跳过。
    #[test]
    fn parses_odd_sized_unknown_chunks_and_custom_rate() {
        let mut bytes = wav_bytes(1, 22_054, 16, 1, &[16384i16, -16384]);
        // 在 fmt 之后插入 LIST 块（3 字节负载 + 1 填充字节），data 完整性不受影响
        let insert_at = 12 + 8 + 16;
        let mut junk = Vec::new();
        junk.extend_from_slice(b"LIST");
        junk.extend_from_slice(&3u32.to_le_bytes());
        junk.extend_from_slice(b"abc");
        junk.push(0); // 奇数长度的对齐填充
        bytes.splice(insert_at..insert_at, junk);
        let clip = parse_wav(&bytes).expect("未知块应被跳过");
        assert_eq!(clip.sample_rate(), 22_054);
        assert_eq!(clip.frame_count(), 2);
    }

    #[test]
    fn bad_magic_is_format_error() {
        let mut bytes = wav_bytes(1, 8_000, 16, 1, &[0]);
        bytes[0..4].copy_from_slice(b"RIFX");
        assert!(matches!(parse_wav(&bytes), Err(AudioError::Format(_))));
        assert!(matches!(parse_wav(b"tiny"), Err(AudioError::Format(_))));
        assert!(matches!(parse_wav(b""), Err(AudioError::Format(_))));
        // RIFF 但不是 WAVE
        let mut bytes = wav_bytes(1, 8_000, 16, 1, &[0]);
        bytes[8..12].copy_from_slice(b"AVI ");
        assert!(matches!(parse_wav(&bytes), Err(AudioError::Format(_))));
    }

    #[test]
    fn unsupported_features_are_reported() {
        // 24-bit 位深
        assert!(matches!(
            parse_wav(&wav_bytes(1, 48_000, 24, 1, &[0])),
            Err(AudioError::Unsupported(what)) if what.contains("24")
        ));
        // IEEE float 编码（format = 3）
        assert!(matches!(
            parse_wav(&wav_bytes(1, 48_000, 16, 3, &[0])),
            Err(AudioError::Unsupported(what)) if what.contains("format=3")
        ));
        // 3 声道
        assert!(matches!(
            parse_wav(&wav_bytes(3, 48_000, 16, 1, &[0, 0, 0])),
            Err(AudioError::Unsupported(what)) if what.contains("3")
        ));
    }

    /// data 块截断（非块对齐）与缺失分别报 Format。
    #[test]
    fn broken_data_chunk_is_format_error() {
        let mut bytes = wav_bytes(1, 48_000, 16, 1, &[0, 0, 0]);
        bytes.truncate(bytes.len() - 3); // 截掉半个帧
        assert!(matches!(parse_wav(&bytes), Err(AudioError::Format(_))));

        let mut bytes = wav_bytes(1, 48_000, 16, 1, &[0]);
        let data_at = bytes.len() - 2 - 8;
        bytes.truncate(data_at); // 掐掉整个 data 块
        assert!(matches!(parse_wav(&bytes), Err(AudioError::Format(_))));
    }

    /// load_wav 走临时文件（fixture 写入 std::env::temp_dir() 子目录）。
    #[test]
    fn load_wav_roundtrip_via_file() {
        let bytes = wav_bytes(1, 48_000, 16, 1, &[0i16, 8192, -8192]);
        let dir = std::env::temp_dir().join("iris_audio_tests");
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let path = dir.join("iris_wav_unit.wav");
        std::fs::write(&path, &bytes).expect("写入临时 .wav");

        let clip = load_wav(&path).expect("加载临时 .wav");
        assert_eq!(clip.frame_count(), 3);
        assert!((clip.samples()[1] - 0.25).abs() < 1e-6);
        let _ = std::fs::remove_file(&path);
    }

    /// AudioError 实现 Display + Error（风格与 GltfError / HdrError 一致）。
    #[test]
    fn error_display() {
        let e = AudioError::Format("测试".into());
        assert!(e.to_string().contains("测试"));
        let _: &dyn std::error::Error = &e;
        let e = AudioError::Unsupported("xyz".into());
        assert!(e.to_string().contains("xyz"));
    }
}
