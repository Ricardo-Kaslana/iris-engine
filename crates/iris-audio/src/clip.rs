//! 音频剪辑：不可变的交错 f32 PCM 采样缓冲（归一化 [-1, 1]）。
//!
//! 采样按帧交错存储（一帧 = 一个时间点上的全部通道采样）。`Arc` 包裹
//! 采样缓冲：同一份大缓冲可被混音器中的多个声音共享，`play` 只克隆指针
//! 而非复制数据；剪辑创建后永不修改，`Clone` 即共享。

use std::sync::Arc;

use crate::AudioError;

/// 单声道 / 立体声交错 f32 采样剪辑。
#[derive(Clone, Debug)]
pub struct AudioClip {
    samples: Arc<Vec<f32>>,
    channels: u16,
    sample_rate: u32,
}

impl AudioClip {
    /// 由交错采样构造。`channels` 必须 ≥ 1、`sample_rate` 必须 > 0 且
    /// `samples.len()` 必须是 `channels` 的整数倍（否则 [`AudioError::Format`]）。
    pub fn from_interleaved(
        samples: Vec<f32>,
        channels: u16,
        sample_rate: u32,
    ) -> Result<Self, AudioError> {
        if channels == 0 {
            return Err(AudioError::Format("声道数必须 ≥ 1".into()));
        }
        if sample_rate == 0 {
            return Err(AudioError::Format("采样率必须 > 0".into()));
        }
        if samples.len() % channels as usize != 0 {
            return Err(AudioError::Format(format!(
                "采样数 {} 不是声道数 {channels} 的整数倍（交错布局损坏）",
                samples.len()
            )));
        }
        Ok(Self { samples: Arc::new(samples), channels, sample_rate })
    }

    /// 声道数（1 = 单声道，2 = 立体声）。
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// 采样率（Hz）。
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// 帧数（一帧 = 一个时间点上的全部 `channels` 个采样）。
    pub fn frame_count(&self) -> usize {
        self.samples.len() / self.channels as usize
    }

    /// 时长（秒）。
    pub fn duration_secs(&self) -> f32 {
        self.frame_count() as f32 / self.sample_rate as f32
    }

    /// 交错采样切片（长度 = 帧数 × 声道数）。
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_interleaved_validates_layout() {
        let ok = AudioClip::from_interleaved(vec![0.0; 6], 2, 48_000).expect("6 采样 × 2 声道合法");
        assert_eq!(ok.frame_count(), 3);
        assert!(matches!(
            AudioClip::from_interleaved(vec![0.0; 5], 2, 48_000),
            Err(AudioError::Format(_))
        ));
        assert!(matches!(
            AudioClip::from_interleaved(vec![0.0], 0, 48_000),
            Err(AudioError::Format(_))
        ));
        assert!(matches!(
            AudioClip::from_interleaved(vec![0.0], 1, 0),
            Err(AudioError::Format(_))
        ));
    }

    #[test]
    fn accessors_report_layout() {
        let clip = AudioClip::from_interleaved(vec![0.0; 96_000], 2, 48_000).expect("合法");
        assert_eq!((clip.channels(), clip.sample_rate()), (2, 48_000));
        assert_eq!(clip.frame_count(), 48_000);
        assert!((clip.duration_secs() - 1.0).abs() < 1e-6);
        assert_eq!(clip.samples().len(), 96_000);
    }

    /// Clone 必须共享底层缓冲（Arc）：采样切片指针不变，大剪辑零复制。
    #[test]
    fn clone_shares_buffer() {
        let clip = AudioClip::from_interleaved(vec![0.25; 8], 1, 8_000).expect("合法");
        let cloned = clip.clone();
        assert_eq!(clipped_ptr(&clip), clipped_ptr(&cloned), "Clone 应共享 Arc 缓冲");
        assert_eq!(clip.samples(), cloned.samples());
    }

    fn clipped_ptr(clip: &AudioClip) -> *const f32 {
        clip.samples().as_ptr()
    }
}
