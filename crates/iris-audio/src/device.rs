//! 音频设备层：把 [`AudioMixer`] 接到真实输出设备的薄壳（cpal）。
//!
//! 平台原因（wasm32，**本文件是全 crate 唯一的 cfg 分支点**）：cpal 的
//! Web 后端在 `wasm32-unknown-unknown` 上不可用（依赖 Web Audio 的回调
//! 模型与 wasm-bindgen 链路，当前无法编译），因此设备实现整体收敛在
//! `cfg(not(target_arch = "wasm32"))` 之下；wasm32 分支只保留同名占位
//! 类型——[`AudioOutput::device`] 恒返回 `None`，调用方（iris-engine 的
//! `Audio` 资源）按「无设备」路径降级为纯混音器，play / stop /
//! set_volume 语义完全不变，只是不会发声。
//!
//! 实时原则：cpal 回调跑在设备驱动的音频线程上，任何阻塞都会变成可闻
//! 的爆音 / 断流。因此回调内用 `try_lock` 获取混音器——锁正被主线程持有
//! （play / stop / 手动 mix）时**立即放弃并填静音**，绝不等待；主线程
//! 侧的锁操作都应是短临界区（一帧的编排，不是长混音）。
//!
//! 线程模型：cpal 0.15 的 `Stream` 是 `!Send`（平台音频 COM / 回调句柄
//! 不允许跨线程迁移），无法由调用方线程代为保活。因此设备枚举、流构建、
//! `play` 与流保活全部发生在专属后台线程内；本模块只跨线程传回协商的
//! 输出格式。`AudioOutput` 被丢弃（或显式发送关闭信号）后，后台线程从
//! `recv` 返回、`Stream` 随之析构、输出回调停止。

use std::sync::{Arc, Mutex, MutexGuard};

use crate::mixer::AudioMixer;

// ---- native 完整实现（cpal 仅在 native 目标编译，见模块注释） ----

/// 真实音频输出设备会话（native 实现）。
///
/// 构造见 [`AudioOutput::device`]；`Drop` 自动停止输出流。
#[cfg(not(target_arch = "wasm32"))]
pub struct AudioOutput {
    /// 与实时回调共享的混音器。
    mixer: Arc<Mutex<AudioMixer>>,
    /// 协商自设备默认配置的输出声道数。
    channels: u16,
    /// 协商自设备默认配置的输出采样率。
    sample_rate: u32,
    /// 关闭信号发送端：被丢弃后后台线程退出、Stream 析构、回调停止。
    _shutdown: std::sync::mpsc::Sender<()>,
}

#[cfg(not(target_arch = "wasm32"))]
impl AudioOutput {
    /// 打开默认输出设备并把一个新建混音器接到它的实时回调上。
    ///
    /// 无设备 / 设备默认格式不是 f32 / 流构建或启动失败时返回 `None`
    /// （调用方应降级为纯混音器，见模块注释——全部失败路径只 log，不 panic）。
    pub fn device() -> Option<Self> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let mixer = Arc::new(Mutex::new(AudioMixer::new()));
        // 协商结果回传（声道数, 采样率）；后台线程失败 / 无设备时发送端
        // 被丢弃，本线程 recv 报错 → 返回 None
        let (info_tx, info_rx) = std::sync::mpsc::channel::<(u16, u32)>();
        let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel::<()>();
        let cb_mixer = Arc::clone(&mixer);

        std::thread::Builder::new()
            .name("iris-audio".into())
            .spawn(move || {
                // 设备枚举与流构建必须在本线程内完成（cpal Stream 非 Send）
                let Some(device) = cpal::default_host().default_output_device() else {
                    log::info!("无默认音频输出设备");
                    return; // info_tx 随作用域丢弃 → 主线程收到断开 → None
                };
                let Ok(config) = device.default_output_config() else {
                    log::warn!("读取音频设备默认输出配置失败");
                    return;
                };
                // v1 仅支持 f32 采样（各平台共享模式的默认格式；其他格式
                // 显式降级而非逐格式手写转换）
                if config.sample_format() != cpal::SampleFormat::F32 {
                    log::warn!(
                        "音频设备默认格式为 {:?} 而非 F32，音频降级为纯混音器模式",
                        config.sample_format()
                    );
                    return;
                }
                let stream_config = config.config().clone();
                // SampleRate 是 newtype（.0 为 u32 Hz）
                let (channels, sample_rate) =
                    (stream_config.channels, stream_config.sample_rate.0);

                let stream = device.build_output_stream(
                    &stream_config,
                    move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                        // 实时线程不阻塞原则：锁忙（主线程正在编排）→ 填静音
                        match cb_mixer.try_lock() {
                            Ok(mut mixer) => mixer.mix(data, channels, sample_rate),
                            Err(_busy) => data.fill(0.0),
                        }
                    },
                    |err| log::error!("音频输出流出错：{err}"),
                    // 无超时：回调错误仅上报，由设备层自行恢复
                    None,
                );
                let Ok(stream) = stream else {
                    log::warn!("构建音频输出流失败");
                    return;
                };
                if let Err(err) = stream.play() {
                    log::warn!("启动音频输出流失败：{err}");
                    return;
                }
                if info_tx.send((channels, sample_rate)).is_err() {
                    return; // 调用方已放弃（device() 返回路径中断）
                }
                // 保活直到关闭信号（AudioOutput Drop）或发送端消失
                let _ = shutdown_rx.recv();
                drop(stream); // 显式停止输出回调
            })
            .ok()?;

        let (channels, sample_rate) = info_rx.recv().ok()?;
        Some(Self { mixer, channels, sample_rate, _shutdown: shutdown_tx })
    }

    /// 混音器可变访问（编排 play / stop / set_volume 或手动混音）。
    ///
    /// 锁中毒时仍取回内部数据：音频路径不因一次 panic 永久失声。
    pub fn mixer(&self) -> MutexGuard<'_, AudioMixer> {
        self.mixer.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 共享混音器句柄（iris-engine 的 `Audio` 资源用它统一 play / stop 路径）。
    pub fn mixer_arc(&self) -> Arc<Mutex<AudioMixer>> {
        Arc::clone(&self.mixer)
    }

    /// 协商的输出声道数。
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// 协商的输出采样率（Hz）。
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for AudioOutput {
    fn drop(&mut self) {
        // 关闭信号发送端在此析构 → 后台线程 recv 返回 → Stream 析构停流
        log::info!(
            "音频输出流已停止（{} Hz × {} 声道）",
            self.sample_rate,
            self.channels
        );
    }
}

// ---- wasm32 占位分支：见模块注释（全 crate 唯一 cfg 分支点的另一半） ----

/// wasm32 占位：设备层不可用，[`AudioOutput::device`] 恒返回 `None`，
/// 本类型实际无法被构造；字段与 native 分支同构，两端方法签名完全一致。
#[cfg(target_arch = "wasm32")]
pub struct AudioOutput {
    mixer: Arc<Mutex<AudioMixer>>,
}

#[cfg(target_arch = "wasm32")]
impl AudioOutput {
    /// wasm32 无设备层（cpal 不可编译，见模块注释）：恒 `None`。
    pub fn device() -> Option<Self> {
        None
    }

    /// 与 native 同签名（实际不可达：本类型无法被构造）。
    pub fn mixer(&self) -> MutexGuard<'_, AudioMixer> {
        self.mixer.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 与 native 同签名（实际不可达：本类型无法被构造）。
    pub fn mixer_arc(&self) -> Arc<Mutex<AudioMixer>> {
        Arc::clone(&self.mixer)
    }

    /// wasm32 无协商格式，恒 0。
    pub fn channels(&self) -> u16 {
        0
    }

    /// wasm32 无协商格式，恒 0。
    pub fn sample_rate(&self) -> u32 {
        0
    }
}
