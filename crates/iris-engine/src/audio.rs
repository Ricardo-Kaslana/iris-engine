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

use glam::Vec3;

use iris_audio::{AudioClip, AudioMixer, AudioOutput, ListenerParams, SoundId};

use iris_core::Resource;

/// 音频资源：优先接真实输出设备，失败降级为纯混音器。
///
/// 除混音器句柄外，本资源还承载**空间源的帧间差分状态**（上一帧活跃的
/// `SpatialSource` 快照，见 `crate::spatial`）：组件移除检测需要跨帧记忆
/// 「上一帧还在播的句柄」，挂在 Audio 上即可让任意系统以 `&World` 只读
/// 访问完成差分（混音器本体在 `Arc<Mutex>` 后，`&Audio` 即可完成全部
/// 播放编排；快照自身的并发安全由独立的 `Mutex` 保证）。
#[derive(Resource)]
pub struct Audio {
    /// 混音器共享句柄（有设备时即设备实时回调使用的同一实例）。
    mixer: Arc<Mutex<AudioMixer>>,
    /// 输出设备会话；`None` = 纯混音器模式（不出声）。
    device: Option<AudioOutput>,
    /// 空间源差分快照：上一帧活跃的（实体 bits → SoundId）列表，
    /// 由 `update_spatial_audio` 每帧替换（组件移除 / 实体 despawn 检测）。
    spatial_active: Mutex<Vec<(u64, SoundId)>>,
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
                    spatial_active: Mutex::new(Vec::new()),
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
            spatial_active: Mutex::new(Vec::new()),
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

    /// 空间化播放：声音定位在世界坐标 `position`，混音按当帧听者位姿施加
    /// 距离衰减 + 恒功率立体声声像（模型见 `iris_audio::mixer`）。无设备时
    /// 语义不变（照常入表，离线 / 测试可混音断言）。
    pub fn play_spatial(
        &self,
        clip: AudioClip,
        position: Vec3,
        volume: f32,
        looping: bool,
    ) -> SoundId {
        self.mixer().play_spatial(clip, position, volume, looping)
    }

    /// 停止一个声音（旧世代句柄安全返回 `false`，见 `iris_audio::SoundId`）。
    pub fn stop(&self, id: SoundId) -> bool {
        self.mixer().stop(id)
    }

    /// 调整一个声音的音量（下一次混音即时生效）。句柄失效返回 `false`。
    pub fn set_volume(&self, id: SoundId, volume: f32) -> bool {
        self.mixer().set_volume(id, volume)
    }

    /// 更新听者位姿（下一帧混音对全部空间声音生效）。headless 与设备模式
    /// 同语义：只是写混音器状态，回调线程的 try_lock 机制不变。
    pub fn set_listener(&self, listener: ListenerParams) {
        self.mixer().set_listener(listener)
    }

    /// 移动一个空间声音的声源位置。句柄失效返回 `false`。
    pub fn set_sound_position(&self, id: SoundId, position: Vec3) -> bool {
        self.mixer().set_sound_position(id, position)
    }

    /// 设置空间声音的距离参考默认值（米；后续 `play_spatial` 生效）。
    pub fn set_reference_distance(&self, reference_distance: f32) {
        self.mixer().set_reference_distance(reference_distance)
    }

    /// 覆写一个空间声音的距离参考（米）。句柄失效返回 `false`。
    pub fn set_sound_reference_distance(&self, id: SoundId, reference_distance: f32) -> bool {
        self.mixer()
            .set_sound_reference_distance(id, reference_distance)
    }

    /// 读取空间源差分快照（上一帧活跃列表；`crate::spatial` 的差分管理用）。
    pub(crate) fn spatial_active(&self) -> Vec<(u64, SoundId)> {
        self.spatial_active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 替换空间源差分快照（每帧由 `update_spatial_audio` 调用一次）。
    pub(crate) fn set_spatial_active(&self, active: Vec<(u64, SoundId)>) {
        *self
            .spatial_active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = active;
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

    /// 空间化门面：headless 模式下 play_spatial / set_listener /
    /// set_sound_position 与混音器直操同语义（照常入表、增益按听者位姿
    /// 生效），槽位状态可离线断言。
    #[test]
    fn spatial_facade_forwards_to_mixer() {
        use iris_audio::ListenerParams;

        let audio = Audio::headless();
        audio.set_listener(ListenerParams::from_view(Vec3::ZERO, -Vec3::Z, Vec3::Y));
        assert_eq!(
            audio.mixer().listener(),
            ListenerParams::default(),
            "from_view(原点, -Z, Y) 应正交化回默认位姿"
        );

        // 空间播放：正右方（d = 4、默认 ref）→ 左静右响
        let clip = sine(440.0, 0.02, 48_000);
        let id = audio.play_spatial(clip, Vec3::new(4.0, 0.0, 0.0), 1.0, true);
        assert_eq!(audio.mixer().active_count(), 1, "play_spatial 应照常入表");
        assert!(audio.set_sound_position(id, Vec3::new(-4.0, 0.0, 0.0)));
        let mut out = vec![0.0; 960];
        audio.mixer().mix(&mut out, 2, 48_000);
        let left: f32 = out.chunks_exact(2).map(|f| f[0].abs()).sum();
        let right: f32 = out.chunks_exact(2).map(|f| f[1].abs()).sum();
        assert!(
            left > right * 10.0,
            "挪到正左后左声道应占主导：{left} vs {right}"
        );
        assert!(audio.stop(id));
        // 差分快照初值：headless 构造即空表，替换 / 读回对称
        assert!(audio.spatial_active().is_empty());
        audio.set_spatial_active(Vec::new());
    }
}
