//! `Audio` 资源：对 [`iris_audio`] 混音器与设备层的引擎侧薄封装。
//!
//! [`Audio::new`] 尝试打开真实输出设备；无设备（无声卡 / 设备格式不支持 /
//! wasm32）时优雅降级为纯混音器——`play / stop / set_volume` 语义完全
//! 不变，只是不会发声（测试、无声卡服务器与 Web 构建统一走该路径，
//! 全程无 panic）。
//!
//! 有设备时混音由 cpal 实时回调驱动；无设备时（如离线快照）可通过
//! [`Audio::mixer`] 手动推进。

use std::sync::{Arc, Mutex, MutexGuard};

use iris_audio::{AudioClip, AudioMixer, AudioOutput, SoundId};

use iris_core::Resource;

/// 音频资源：优先接真实输出设备，失败降级为纯混音器。
#[derive(Resource)]
pub struct Audio {
    /// 混音器共享句柄（有设备时即设备实时回调使用的同一实例）。
    mixer: Arc<Mutex<AudioMixer>>,
    /// 输出设备会话；`None` = 纯混音器模式（不出声）。
    device: Option<AudioOutput>,
}

impl Audio {
    /// 创建：优先建设备（混音由实时回调驱动）；失败则纯混音器模式。
    /// 无设备时 `log::warn` 恰好一次（本函数只在 `AudioPlugin` 里调用一次）。
    pub fn new() -> Self {
        match AudioOutput::device() {
            Some(device) => {
                let mixer = device.mixer_arc();
                Self {
                    mixer,
                    device: Some(device),
                }
            }
            None => {
                log::warn!(
                    "无可用音频输出设备，音频降级为纯混音器模式（play / stop / set_volume \
                     正常工作，但不会发声）"
                );
                Self::headless()
            }
        }
    }

    /// 强制纯混音器模式：**绝不打开输出设备**（headless 测试 / CI 专用）。
    ///
    /// `Audio::new()` 在有声卡的机器上会短暂占用真实设备——`cargo test`
    /// 并发跑多个测试时可能触发设备争抢或无人值守的出声。headless 测试
    /// 预插入本构造的资源后，`AudioPlugin` / 资源兜底逻辑遇到已存在的
    /// `Audio` 不会覆盖（设备句柄独占，覆盖即泄漏），即可保证测试全程
    /// 零设备访问；`play / stop / set_volume / mixer` 语义与有设备时一致。
    pub fn headless() -> Self {
        Self {
            mixer: Arc::new(Mutex::new(AudioMixer::new())),
            device: None,
        }
    }

    /// 是否接有真实输出设备。
    pub fn has_device(&self) -> bool {
        self.device.is_some()
    }

    /// 输出设备的协商声道数（无设备时 0）。
    pub fn channels(&self) -> u16 {
        self.device.as_ref().map(|d| d.channels()).unwrap_or(0)
    }

    /// 输出设备的协商采样率（无设备时 0）。
    pub fn sample_rate(&self) -> u32 {
        self.device.as_ref().map(|d| d.sample_rate()).unwrap_or(0)
    }

    /// 播放剪辑，返回防悬垂句柄（无设备时同样入表，供离线 / 测试混音）。
    pub fn play(&self, clip: AudioClip, volume: f32, looping: bool) -> SoundId {
        self.mixer().play(clip, volume, looping)
    }

    /// 停止一个声音（旧世代句柄安全返回 `false`，见 `iris_audio::SoundId`）。
    pub fn stop(&self, id: SoundId) -> bool {
        self.mixer().stop(id)
    }

    /// 调整一个声音的音量（下一次混音即时生效）。句柄失效返回 `false`。
    pub fn set_volume(&self, id: SoundId, volume: f32) -> bool {
        self.mixer().set_volume(id, volume)
    }

    /// 混音器可变访问：无设备模式下手动推进混音（离线渲染 / 快照），
    /// 或查询 `active_count` 等状态。
    pub fn mixer(&self) -> MutexGuard<'_, AudioMixer> {
        self.mixer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for Audio {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_audio::sine;

    /// headless 构造：绝不触碰设备（`has_device` 为 false、协商参数为 0），
    /// 混音器照常可用、play 不 panic。
    #[test]
    fn headless_never_touches_device_and_mixer_still_works() {
        let audio = Audio::headless();
        assert!(!audio.has_device(), "headless 模式不得持有设备会话");
        assert_eq!(audio.channels(), 0, "无设备时协商声道数为 0");
        assert_eq!(audio.sample_rate(), 0, "无设备时协商采样率为 0");

        let clip = sine(440.0, 0.05, 48_000);
        let id = audio.play(clip, 0.5, false);
        assert_eq!(
            audio.mixer().active_count(),
            1,
            "纯混音器模式下 play 照常入表"
        );
        assert!(audio.set_volume(id, 0.2), "句柄在活跃表内，调整音量应成功");
        assert!(audio.stop(id), "停止应成功");
        assert_eq!(audio.mixer().active_count(), 0);
        assert!(!audio.stop(id), "旧世代句柄安全返回 false");
    }
}
