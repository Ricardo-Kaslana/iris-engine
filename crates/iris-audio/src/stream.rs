//! 流式音频通道：拉取式音频流 + 有界块队列 + 音乐总线（R44）。
//!
//! 动机：`AudioClip` 是**全量常驻内存**的 f32 PCM（60 s 立体声 48 kHz ≈
//! 22 MB），30 分钟配乐用 WAV 是几百 MB——「低占用」卖点在音频侧破功。
//! OGG/Vorbis 把压缩态留在磁盘 / 压缩缓冲里，解码线程分块产出采样，
//! 混音回调按需取块 ⇒ 常驻内存与曲目长度**无关**（只有一块队列 + 一块待播缓冲）。
//!
//! ```text
//!   ┌────────────────┐  push(块)    ┌───────────────────┐  poll(非阻塞)  ┌─────────────┐
//!   │ 解码线程        │ ───────────► │ 有界块队列 8×4096  │ ────────────► │ MusicBus    │──► mix
//!   │ (AudioStream)   │ ◄─────────── │ Condvar 满则等待   │               │ 拼接 + 包络  │
//!   └────────────────┘  close 信号   └───────────────────┘               └─────────────┘
//! ```
//!
//! - **实时原则不破**：混音回调侧 `BlockQueue::poll` 只做一次 `try_lock`，
//!   忙 / 空即判为**下溢**（该缓冲剩余帧不写音乐采样、游标不推进，下次混音
//!   从原处续播），绝不等待；只有生产者侧（后台线程）在满队列时阻塞。
//! - **块拼接是采样级直通**：取出的块 `extend_from_slice` 接到待播缓冲尾部，
//!   重采样游标跨块**连续**（每帧混音后把已消费的整数帧从待播缓冲与游标里
//!   精确剪掉——f64 减整数在实际块长下无舍入），块边界不出现在采样序列里。
//!   因此不下溢时「流式输出的采样序列 == 源采样序列」逐位成立（单测 `to_bits`
//!   锁定，且与混音分块大小无关）。
//! - **包络确定性**：淡入 / 淡出是**线性 f32 斜坡**，按已混音的输出帧推进
//!   （静音的下溢帧与零增益帧不消耗采样，但**都**消耗包络一帧——包络时间表
//!   只由混音帧数决定，与解码快慢无关），同参数同帧序列必逐位一致。
//! - **循环在生产者侧**：流耗尽后由解码线程调 [`AudioStream::rewind`] 重播
//!   （队列深度即抖动预算，接缝听不见也看不见）；不支持 rewind 的流播完即止。
//! - **单轨音乐总线**：v1 一次只放一条音乐（切轨 = 硬拆上一轨 + 淡入新轨，
//!   不做 crossfade / 多轨混音）。
//!
//! 已知限制（v1，诚实清单）：
//! - 音乐不参与空间化（无距离衰减 / 声像，恒为总线增益 × 包络）；
//! - 队列按**块**计量深度，8 × 4096 帧 @ 48 kHz ≈ 0.68 s 抖动预算；持续下溢
//!   表现为卡顿（缺块静音）而非爆音，是解码跟不上实时消费的信号；
//! - 淡出到位 / 播完后拆轨，但**混响尾巴继续**（混响状态挂在混音器的
//!   `Reverb` 上，见 `reverb` 模块）；
//! - 后台解码线程创建失败（wasm32 / 线程资源耗尽）时退化为**混音调用内同步
//!   解码**（`MusicBus::stream` 留着流本体自己拉块），此时解码耗时计入该次
//!   混音——native 有设备时不该发生（wasm 本就没有 cpal 实时回调，混音由主线程推进）。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::clip::AudioClip;
use crate::mixer::{FrameGains, SourceFrames, sanitize_volume};

/// 流式通道默认块长（帧）。4096 帧 @ 48 kHz ≈ 85 ms，立体声 f32 一块 32 KB
/// ——解码唤醒频率与队列拷贝粒度的折中。
pub const DEFAULT_BLOCK_FRAMES: usize = 4096;
/// 块队列默认深度（块数）。8 × 4096 帧 ≈ 0.68 s 抖动预算、约 256 KB 常驻。
pub const DEFAULT_QUEUE_BLOCKS: usize = 8;
/// 连续多少次 `next_block` 空转即判为流结束（不信任外部实现的进展保证）。
const MAX_EMPTY_STREAK: usize = 8;

/// 拉取式音频流：把「解码（或任何采样来源）」与「消费（混音回调）」解体的契约。
///
/// **契约**（实现者必须满足，混音路径据此免做校验）：
/// - [`channels`](AudioStream::channels) / [`sample_rate`](AudioStream::sample_rate)
///   在流的生命周期内恒定且 ≥ 1（非法流不会被启动）；
/// - [`next_block`](AudioStream::next_block) 向 `out` **追加**若干交错帧
///   （长度是 `channels` 的整数倍），返回 `true` 表示后面还有内容；返回
///   `false` 时 `out` 里**可能仍带最后一段采样**，消费者必须消费掉；块长
///   不限（按解码包边界产出、"约 DEFAULT_BLOCK_FRAMES 帧" 是常态）；
/// - 每次调用必须有进展（产出采样或返回 `false`）：连续 8 次空转会被解码
///   线程判为流结束，防止外部实现「返回 true + 空块」把后台线程打死转。
pub trait AudioStream: Send {
    /// 声道数（恒定，≥ 1）。
    fn channels(&self) -> u16;
    /// 采样率（Hz，恒定）。与输出采样率不一致时由混音器线性插值重采样。
    fn sample_rate(&self) -> u32;
    /// 追加下一块采样到 `out`；返回 `true` = 还有更多。
    fn next_block(&mut self, out: &mut Vec<f32>) -> bool;
    /// 回到起点（循环播放用）。返回 `true` = 已重置、可继续产出；
    /// 默认 `false` = 该流不可循环（播完即止）。
    fn rewind(&mut self) -> bool {
        false
    }
}

/// 内存剪辑包装成的流：把已在内存的 `AudioClip` 按块喂给流式通道。
///
/// 两处用途：① 流式机制的确定性测试（机制与文件格式无关，见模块文档）；
/// ② 让一段短音效走「总线 + 淡入淡出 + 循环」路径而不额外复制采样
/// （剪辑本体已常驻，`SampleStream` 只克隆 `Arc` 指针）。
pub struct SampleStream {
    clip: AudioClip,
    frame_pos: usize,
    block_frames: usize,
}

impl SampleStream {
    /// 以给定块长包装剪辑（`block_frames = 0` 归一为
    /// [`DEFAULT_BLOCK_FRAMES`]——0 会让 `next_block` 永不产出）。
    pub fn new(clip: AudioClip, block_frames: usize) -> Self {
        Self {
            clip,
            frame_pos: 0,
            block_frames: block_frames.max(1),
        }
    }

    /// 以默认块长包装剪辑。
    pub fn from_clip(clip: AudioClip) -> Self {
        Self::new(clip, DEFAULT_BLOCK_FRAMES)
    }

    /// 下一个待产出帧的位置（测试与调试用）。
    pub fn position(&self) -> usize {
        self.frame_pos
    }
}

impl AudioStream for SampleStream {
    fn channels(&self) -> u16 {
        self.clip.channels()
    }

    fn sample_rate(&self) -> u32 {
        self.clip.sample_rate()
    }

    fn next_block(&mut self, out: &mut Vec<f32>) -> bool {
        let channels = self.clip.channels() as usize;
        let total = self.clip.frame_count();
        let start = self.frame_pos.min(total);
        let end = (start + self.block_frames).min(total);
        out.extend_from_slice(&self.clip.samples()[start * channels..end * channels]);
        self.frame_pos = end;
        self.frame_pos < total
    }

    fn rewind(&mut self) -> bool {
        self.frame_pos = 0;
        true
    }
}

/// 音乐轨句柄：总线自增序号。
///
/// 单轨总线上「当前轨是哪一轨」由 `AudioMixer::music_handle` 回读，调用方
/// 拿句柄与之比对即可判断「我放的那条还在不在播」（比对相同 ≠ 一定在播：
/// 序号不回收复用，2^64 首曲目后才会撞号）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MusicHandle(u64);

/// 队列状态（受 `BlockQueue` 的互斥锁保护）。
struct QueueState {
    blocks: VecDeque<Vec<f32>>,
    capacity: usize,
    /// 生产者已产出最后一条流（自然结束 / 出错 / panic）。
    finished: bool,
    /// 消费者已拆轨（切轨 / `stop_music` / 混音器析构）→ 生产者应尽快退出。
    closed: bool,
}

/// 解码线程 → 混音回调的单向有界块通道。
struct BlockQueue {
    inner: Mutex<QueueState>,
    not_full: Condvar,
    /// 生产者线程已退出（正常 / 关闭 / panic 均置位）——消费者据此判定
    /// 「不会再有新块」，不会因为生产者异常而永久停在静音态。
    producer_done: AtomicBool,
}

/// 消费者一次取数的三态结果。
#[derive(Debug)]
enum Polled {
    /// 拿到一块交错采样。
    Block(Vec<f32>),
    /// 流彻底结束（生产者标记完成且队列已空，或队列已关闭）。
    End,
    /// 暂无数据（锁忙 = 下溢，或生产者还在解）。
    Empty,
}

impl BlockQueue {
    fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(QueueState {
                blocks: VecDeque::new(),
                capacity: capacity.max(1),
                finished: false,
                closed: false,
            }),
            not_full: Condvar::new(),
            producer_done: AtomicBool::new(false),
        }
    }

    /// **消费者（实时回调）**：一次 `try_lock` 定去留，绝不等待。
    fn poll(&self) -> Polled {
        let Ok(mut state) = self.inner.try_lock() else {
            return Polled::Empty; // 锁忙：按静音处理（实时线程不阻塞原则）
        };
        if let Some(block) = state.blocks.pop_front() {
            // 空位腾出 → 唤醒可能阻塞在满队列上的生产者（离开锁后再通知）
            drop(state);
            self.not_full.notify_one();
            return Polled::Block(block);
        }
        if state.finished || state.closed {
            Polled::End
        } else {
            Polled::Empty
        }
    }

    /// **生产者（解码线程）**：队满则阻塞到消费者取走或队列关闭。
    /// 返回 `false` = 队列已关闭，应立即退出。
    fn push(&self, block: Vec<f32>) -> bool {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while state.blocks.len() >= state.capacity && !state.closed {
            state = self
                .not_full
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        if state.closed {
            return false;
        }
        state.blocks.push_back(block);
        true
    }

    /// 消费者主动放弃：置关闭位并丢弃未消费的块（生产者到安全点后自行退出）。
    fn close(&self) {
        {
            let mut state = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.closed = true;
            state.blocks.clear();
        }
        self.not_full.notify_all();
    }

    /// 消费者：取走当前已入队的全部采样（内联退化路径用——调用时后台线程
    /// 尚未启动，无并发）。
    fn drain_all(&self) -> Vec<f32> {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut out = Vec::new();
        while let Some(block) = state.blocks.pop_front() {
            out.extend_from_slice(&block);
        }
        out
    }

    fn mark_finished(&self) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .finished = true;
    }

    fn producer_done(&self) -> bool {
        self.producer_done.load(Ordering::Acquire)
    }
}

/// 生产者退出守卫：无论正常返回、`close` 早退还是 panic，都要让消费者知道
/// 「不会再有新块」——否则混音回调会无限期停在静音而不是判流结束。
struct ProducerGuard(Arc<BlockQueue>);

impl Drop for ProducerGuard {
    fn drop(&mut self) {
        self.0.mark_finished();
        self.0.producer_done.store(true, Ordering::Release);
        self.0.not_full.notify_all();
    }
}

/// 线性增益斜坡（按**输出帧**推进，f32 累加——同帧序列必逐位一致）。
#[derive(Clone, Copy, Debug, PartialEq)]
struct GainRamp {
    value: f32,
    target: f32,
    /// 每输出帧增量（0 = 恒定；`secs <= 0` 的瞬时变化直接落到目标值）。
    step: f32,
}

impl GainRamp {
    fn constant(value: f32) -> Self {
        Self {
            value,
            target: value,
            step: 0.0,
        }
    }

    /// 在已知输出采样率时落地斜坡：`secs <= 0` / 非有限 → 瞬时到目标。
    fn set(&mut self, target: f32, secs: f32, sample_rate_out: u32) {
        let frames = if secs.is_finite() {
            (f64::from(secs.max(0.0)) * f64::from(sample_rate_out)).round()
        } else {
            0.0
        };
        if frames <= 0.0 {
            *self = Self::constant(target);
        } else {
            self.target = target;
            self.step = (target - self.value) / frames as f32;
        }
    }

    /// 返回本帧增益并把斜坡推进一帧。
    fn advance(&mut self) -> f32 {
        let current = self.value;
        if self.step > 0.0 {
            self.value = (current + self.step).min(self.target);
        } else if self.step < 0.0 {
            self.value = (current + self.step).max(self.target);
        }
        current
    }

    /// 斜坡是否已到位（恒定视为已到位）。
    fn settled(&self) -> bool {
        self.step == 0.0
            || (self.step > 0.0 && self.value >= self.target)
            || (self.step < 0.0 && self.value <= self.target)
    }
}

/// 混音器持有的音乐总线：一条当前轨 + 块队列 + 待播缓冲 + 包络。
///
/// 不对外暴露构造（`AudioMixer::play_music` 是唯一入口），类型对 `crate`
/// 可见以便 `mixer.rs` 直接驱动。
pub(crate) struct MusicBus {
    /// 当前轨句柄（`None` = 总线空闲，`render_into` 原样跳过）。
    handle: Option<MusicHandle>,
    next_id: u64,
    /// 后台解码线程（供 `join_music_producer` 显式回收；`None` = 内联模式）。
    producer: Option<std::thread::JoinHandle<()>>,
    queue: Option<Arc<BlockQueue>>,
    /// 流本体句柄。**线程模式**下解码线程持有自己的 `Arc` 克隆、这里置
    /// `None`（总线不再需要访问它）；**内联模式**（线程创建失败）下保留，
    /// 由混音调用锁着同步解码。
    stream: Option<Arc<Mutex<dyn AudioStream>>>,
    channels: u16,
    sample_rate: u32,
    volume: f32,
    gain: GainRamp,
    /// 待落地的斜坡（目标增益, 秒）——输出采样率要到 `mix` 才知道，故推迟落地。
    pending_ramp: Option<(f32, f32)>,
    /// 淡出中：包络到位后拆轨。
    fading_out: bool,
    /// 已取出、未播放的采样（交错）。头部恒对应 `cursor` 的整数部分。
    pending: Vec<f32>,
    /// 待播缓冲内的源帧游标（f64 累积，跨块连续）。
    cursor: f64,
    /// 流已结束（队列判 `End`）。
    finished: bool,
}

impl Default for MusicBus {
    fn default() -> Self {
        Self {
            handle: None,
            next_id: 0,
            producer: None,
            queue: None,
            stream: None,
            channels: 1,
            sample_rate: 48_000,
            volume: 1.0,
            gain: GainRamp::constant(1.0),
            pending_ramp: None,
            fading_out: false,
            pending: Vec::new(),
            cursor: 0.0,
            finished: false,
        }
    }
}

impl MusicBus {
    /// 当前轨句柄（`None` = 空闲）。
    pub(crate) fn handle(&self) -> Option<MusicHandle> {
        self.handle
    }

    /// 是否在播（含淡出中——淡出也是「还在响」）。
    pub(crate) fn is_playing(&self) -> bool {
        self.handle.is_some()
    }

    /// 后台解码线程是否已退出（内联模式恒 `true`：本来就没有线程）。
    pub(crate) fn producer_finished(&self) -> bool {
        match &self.queue {
            Some(queue) => queue.producer_done(),
            None => true,
        }
    }

    /// 启动一条新轨：硬拆上一轨（v1 不做 crossfade），淡入 `fade_in` 秒。
    ///
    /// 病态流（0 声道 / 0 采样率）不启动：返回的句柄立即失效（`music_handle`
    /// 仍指向上一轨或 `None`），当前轨不受影响。
    pub(crate) fn play<S: AudioStream + 'static>(
        &mut self,
        stream: S,
        volume: f32,
        fade_in: f32,
        looping: bool,
    ) -> MusicHandle {
        let channels = stream.channels();
        let sample_rate = stream.sample_rate();
        let handle = MusicHandle(self.next_id);
        if channels == 0 || sample_rate == 0 {
            log::warn!("音乐流参数非法（{channels} 声道 @ {sample_rate} Hz），已忽略该次播放");
            return handle;
        }
        self.next_id += 1;
        self.teardown();
        self.handle = Some(handle);
        self.channels = channels;
        self.sample_rate = sample_rate;
        self.volume = sanitize_volume(volume);
        self.gain = GainRamp::constant(0.0);
        self.pending_ramp = Some((1.0, fade_in));
        self.fading_out = false;
        self.pending.clear();
        self.cursor = 0.0;
        self.finished = false;

        // 流本体放进共享句柄：解码线程持自己的 `Arc` 克隆，总线这一份只在
        // 内联模式下使用（线程模式立即释放，避免与解码线程争锁）
        let stream: Arc<Mutex<dyn AudioStream>> = Arc::new(Mutex::new(stream));
        let queue = Arc::new(BlockQueue::new(DEFAULT_QUEUE_BLOCKS));
        // 预探一块：新轨第一次混音就有数据可取（不必等后台线程被调度），
        // 并识别「一上来就是空流」的退化轨（真正的格式 / IO 错误在流构造点
        // 就已经回传给调用方了，见 `ogg::OggVorbisStream::open`）。
        let mut probe = Vec::new();
        let probed_ok = {
            let mut guard = stream
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let produced = guard.next_block(&mut probe);
            produced || !probe.is_empty()
        };
        if !probe.is_empty() {
            queue.push(probe);
        }
        let spawn = {
            let thread_queue = Arc::clone(&queue);
            let thread_stream = Arc::clone(&stream);
            std::thread::Builder::new()
                .name("iris-audio-music".into())
                .spawn(move || {
                    let _guard = ProducerGuard(Arc::clone(&thread_queue));
                    let mut stream = thread_stream
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    decode_loop(&mut *stream, &thread_queue, looping);
                })
        };
        match spawn {
            Ok(join) => {
                self.queue = Some(queue);
                self.producer = Some(join);
                self.stream = None;
                if !probed_ok {
                    // 首块即空且无更多数据：空轨，立刻按已耗尽处理（下次混音拆轨）
                    self.finished = true;
                }
            }
            Err(err) => {
                // wasm32 / 线程资源不足：退化为混音调用内同步解码（见模块文档）
                log::warn!("音乐解码线程启动失败（{err}），改为混音调用内同步解码");
                self.pending = queue.drain_all();
                queue.close();
                drop(queue);
                self.queue = None;
                self.stream = Some(stream);
                if !probed_ok {
                    self.finished = true;
                }
            }
        }
        handle
    }

    /// 停止当前轨：`fade_out <= 0` 立即拆轨，否则淡出到 0 后拆。
    /// 返回此前是否有轨在播。
    pub(crate) fn stop(&mut self, fade_out: f32) -> bool {
        if self.handle.is_none() {
            return false;
        }
        if fade_out.is_finite() && fade_out > 0.0 {
            self.pending_ramp = Some((0.0, fade_out));
            self.fading_out = true;
        } else {
            self.teardown();
        }
        true
    }

    /// 总线音量（下一次混音即时生效；负数 / NaN 按静音，口径同 `play`）。
    pub(crate) fn set_volume(&mut self, volume: f32) {
        self.volume = sanitize_volume(volume);
    }

    /// 把音乐总线叠加进 `out`（**叠加**语义：调用方保证 `out` 已清零或已含
    /// 其他声音）。空闲总线不触碰 `out`——不播音乐时混音输出与 R44 之前
    /// 逐位一致。
    pub(crate) fn render_into(
        &mut self,
        out: &mut [f32],
        channels_out: usize,
        sample_rate_out: u32,
    ) {
        if self.handle.is_none() || channels_out == 0 || sample_rate_out == 0 {
            return;
        }
        if let Some((target, secs)) = self.pending_ramp.take() {
            self.gain.set(target, secs, sample_rate_out);
        }
        let channels_src = self.channels as usize;
        let ratio = {
            let ratio = f64::from(self.sample_rate) / f64::from(sample_rate_out);
            if ratio.is_finite() && ratio > 0.0 {
                ratio
            } else {
                1.0
            }
        };
        {
            // 解构出互不相交的字段借用：`pull` 动 queue / stream / pending，
            // 循环体读 pending、写 cursor / gain。
            let MusicBus {
                pending,
                cursor,
                gain,
                volume,
                finished,
                queue,
                stream,
                ..
            } = self;
            for out_frame in out.chunks_exact_mut(channels_out) {
                let frame_gain = gain.advance();
                if frame_gain == 0.0 {
                    continue; // 淡入起点 / 淡出末端：不消费采样，只走包络
                }
                // 保证游标右侧还有一帧可插值；取不到即下溢（或流结束）
                loop {
                    let available = pending.len() / channels_src;
                    if *cursor as usize + 1 < available || *finished {
                        break;
                    }
                    if !pull(pending, queue, stream, finished) {
                        break;
                    }
                }
                let available = pending.len() / channels_src;
                let i0 = *cursor as usize;
                if i0 >= available {
                    break; // 下溢：本缓冲剩余帧不写音乐，游标不推进（下次续播）
                }
                let i1 = (i0 + 1).min(available - 1);
                let frac = (*cursor - i0 as f64) as f32;
                let gains = FrameGains {
                    left: *volume * frame_gain,
                    right: *volume * frame_gain,
                    mono: *volume * frame_gain,
                };
                // 音乐总线恒不做遮挡低通（v3 lp = None）：走与引入滤波前
                // 逐位一致的直通路径。
                SourceFrames::new(pending, channels_src)
                    .add_frame(out_frame, i0, i1, frac, &gains, None);
                *cursor += ratio;
            }
            // 剪掉已消费前缀（整数帧，f64 减整数精确 ⇒ 游标相位跨块不漂移）。
            // 钳到可用帧数：源采样率高于输出时游标一步可跨过若干帧，本缓冲
            // 末帧之后游标会越界若干步——越界量留在游标里，下次混音续播时消化。
            let available = pending.len() / channels_src;
            let consumed = (*cursor as usize).min(available);
            if consumed > 0 {
                pending.drain(..consumed * channels_src);
                *cursor -= consumed as f64;
            }
        }
        // 拆轨条件：淡出到位，或流已结束且待播缓冲耗尽
        let drained = self.finished && self.pending.is_empty();
        if drained || (self.fading_out && self.pending_ramp.is_none() && self.gain.settled()) {
            self.teardown();
        }
    }

    /// 回收后台解码线程（`stop_music` / 切轨后调用可确保线程真正退出）。
    /// **会阻塞**，不要在实时回调里调用。返回是否确实 join 了一个线程。
    pub(crate) fn join_producer(&mut self) -> bool {
        match self.producer.take() {
            Some(join) => join.join().is_ok(),
            None => false,
        }
    }

    /// 拆轨：关闭队列（解码线程自行退出）、清空状态与待播缓冲。
    fn teardown(&mut self) {
        if let Some(queue) = self.queue.take() {
            queue.close();
        }
        self.stream = None;
        self.pending.clear();
        self.cursor = 0.0;
        self.finished = false;
        self.fading_out = false;
        self.pending_ramp = None;
        self.gain = GainRamp::constant(1.0);
        self.handle = None;
        // producer 的 JoinHandle 留着供 join_producer 回收；play 中被替换即分离
        // （线程已收到 close，会在一个块解码时间内自行退出）
    }
}

/// 总线析构（随 `AudioMixer` 一起）：确保后台解码线程不会在无人消费时
/// 把整条长曲解到底。
impl Drop for MusicBus {
    fn drop(&mut self) {
        if let Some(queue) = self.queue.take() {
            queue.close();
        }
    }
}

/// 从队列 / 内联流取块补进待播缓冲。返回 `false` = 没补到（下溢或流结束）。
fn pull(
    pending: &mut Vec<f32>,
    queue: &mut Option<Arc<BlockQueue>>,
    stream: &mut Option<Arc<Mutex<dyn AudioStream>>>,
    finished: &mut bool,
) -> bool {
    if let Some(queue) = queue.as_ref() {
        match queue.poll() {
            Polled::Block(block) => {
                pending.extend_from_slice(&block);
                true
            }
            Polled::End => {
                *finished = true;
                false
            }
            Polled::Empty => false,
        }
    } else if let Some(holder) = stream.as_mut() {
        // 内联模式：解码耗时直接计入本次混音（无后台线程可用的平台，见模块文档）
        let mut source = holder
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut block = Vec::new();
        let more = source.next_block(&mut block);
        let had_data = !block.is_empty();
        pending.extend_from_slice(&block);
        if !more {
            *finished = true;
        }
        had_data
    } else {
        *finished = true;
        false
    }
}

/// 解码线程主体：拉流 → 入队（满则阻塞），直到流结束 / 队列关闭。
fn decode_loop(stream: &mut dyn AudioStream, queue: &BlockQueue, looping: bool) {
    let mut empty_streak = 0usize;
    loop {
        let mut block = Vec::new();
        let more = stream.next_block(&mut block);
        if block.is_empty() {
            empty_streak += 1;
            if empty_streak >= MAX_EMPTY_STREAK {
                log::warn!("音频流连续 {empty_streak} 次无产出，按流结束处理（违反进展契约）");
                return;
            }
        } else {
            empty_streak = 0;
        }
        let had_data = !block.is_empty();
        if had_data && !queue.push(block) {
            return; // 消费者已关闭：立即退出（不再解后面的块）
        }
        if !more {
            if looping && stream.rewind() {
                empty_streak = 0;
                continue;
            }
            return; // 自然播完（ProducerGuard 负责标记 finished）
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn mono(samples: &[f32]) -> AudioClip {
        AudioClip::from_interleaved(samples.to_vec(), 1, 48_000).expect("测试剪辑合法")
    }

    /// SampleStream 按块切分：拼接后与源逐采样一致，末块（不足一块）仍被交付。
    #[test]
    fn sample_stream_cuts_clip_into_lossless_blocks() {
        let src: Vec<f32> = (0..10).map(|i| i as f32 * 0.1 - 0.4).collect();
        let mut stream = SampleStream::new(mono(&src), 3);
        assert_eq!((stream.channels(), stream.sample_rate()), (1, 48_000));
        let mut got = Vec::new();
        let mut calls = 0;
        while stream.next_block(&mut got) {
            calls += 1;
        }
        calls += 1; // 最后一次返回 false，但末块采样已交付
        assert_eq!(calls, 4, "10 帧 / 3 帧每块 = 4 次调用");
        assert_eq!(got.len(), src.len());
        for (i, (got, want)) in got.iter().zip(&src).enumerate() {
            assert_eq!(got.to_bits(), want.to_bits(), "分块拼接必须逐位无损 @{i}");
        }
        assert_eq!(stream.position(), src.len(), "耗尽后位置停在末尾");
        // rewind 复位后可原样再产一遍（循环能力）
        assert!(stream.rewind());
        assert_eq!(stream.position(), 0);
        let mut again = Vec::new();
        stream.next_block(&mut again);
        assert_eq!(again, vec![src[0], src[1], src[2]], "rewind 后从头产出");
        // 块长 0 归一为 1（不会永不产出）
        let mut zero = SampleStream::new(mono(&src), 0);
        let mut one = Vec::new();
        assert!(zero.next_block(&mut one));
        assert_eq!(one, vec![src[0]]);
    }

    /// 满队语义：`try_lock` 之外的阻塞只发生在生产者侧——第 capacity+1 次
    /// `push` 卡住，消费者取走一块后放行。
    #[test]
    fn queue_blocks_producer_when_full() {
        let queue = Arc::new(BlockQueue::new(2));
        assert!(queue.push(vec![1.0]), "第 1 块应直接入队");
        assert!(queue.push(vec![2.0]), "第 2 块应直接入队");
        let late = Arc::clone(&queue);
        let thread = std::thread::spawn(move || {
            let start = Instant::now();
            let ok = late.push(vec![3.0]);
            (ok, start.elapsed())
        });
        std::thread::sleep(Duration::from_millis(60));
        assert!(
            matches!(queue.poll(), Polled::Block(_)),
            "消费者先取走一块腾容量"
        );
        let (ok, waited) = thread.join().expect("生产者线程不应 panic");
        assert!(ok, "等待后入队应成功（未被误判为关闭）");
        assert!(
            waited >= Duration::from_millis(40),
            "满队时生产者必须阻塞，实际只等了 {waited:?}"
        );
        assert!(
            matches!(queue.poll(), Polled::Block(b) if b == vec![2.0]),
            "FIFO：第二块先于被阻塞的第三块"
        );
        assert!(
            matches!(queue.poll(), Polled::Block(b) if b == vec![3.0]),
            "被唤醒的生产者应已把第三块排到队尾"
        );
    }

    /// 消费者不阻塞：锁被他人持有时 `poll` 立即判空（实时回调口径）。
    #[test]
    fn poll_never_waits_on_the_lock() {
        let queue = BlockQueue::new(4);
        assert!(queue.push(vec![0.0; 8]));
        let held = queue
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let start = Instant::now();
        assert!(matches!(queue.poll(), Polled::Empty), "锁忙应按空处理");
        assert!(
            start.elapsed() < Duration::from_millis(50),
            "poll 不得等待锁：耗时 {:?}",
            start.elapsed()
        );
        drop(held);
        assert!(matches!(queue.poll(), Polled::Block(_)), "解锁后应能取到块");
    }

    /// close 语义：入队返回 false、poll 判 End、未消费块被丢弃。
    #[test]
    fn close_stops_producer_and_ends_stream() {
        let queue = BlockQueue::new(2);
        assert!(queue.push(vec![1.0]));
        queue.close();
        assert!(
            !queue.push(vec![2.0]),
            "关闭后 push 应返回 false 让生产者退出"
        );
        assert!(
            matches!(queue.poll(), Polled::End),
            "close 丢弃残留块并判流结束"
        );
    }

    /// drain_all：把已入队采样一次性取回（线程未启动的内联退化路径）。
    #[test]
    fn drain_all_collects_queued_blocks() {
        let queue = BlockQueue::new(4);
        assert!(queue.push(vec![1.0, 2.0]));
        assert!(queue.push(vec![3.0]));
        assert_eq!(queue.drain_all(), vec![1.0, 2.0, 3.0]);
        assert!(queue.drain_all().is_empty(), "取空后应返回空");
        assert!(matches!(queue.poll(), Polled::Empty));
    }

    /// ProducerGuard：线程退出即标记 finished + producer_done（消费者不再等）。
    #[test]
    fn producer_guard_marks_end_on_exit() {
        let queue = Arc::new(BlockQueue::new(2));
        {
            let guard = ProducerGuard(Arc::clone(&queue));
            assert!(!queue.producer_done());
            drop(guard);
        }
        assert!(queue.producer_done());
        assert!(matches!(queue.poll(), Polled::End));
    }

    /// 线性斜坡解析：`secs` 帧内从当前值等步长到目标，到位后恒定（并钳住不冲过头）。
    #[test]
    fn gain_ramp_is_linear_and_clamps_at_target() {
        let mut ramp = GainRamp::constant(0.0);
        ramp.set(1.0, 0.001, 48_000); // 48 帧
        assert!((ramp.step - 1.0 / 48.0).abs() < 1e-7, "步长 = 1/帧数");
        // advance 先返回后步进 ⇒ 第 i 帧增益 = i 次累加
        let values: Vec<f32> = (0..60).map(|_| ramp.advance()).collect();
        assert_eq!(values[0], 0.0, "淡入首帧静音");
        assert!(
            (values[24] - 0.5).abs() < 1e-6,
            "中点应为 0.5，实际 {}",
            values[24]
        );
        assert!(values.iter().all(|v| *v <= 1.0), "斜坡不得冲过头");
        let full = values
            .iter()
            .position(|v| *v == 1.0)
            .expect("48 帧斜坡应在 60 帧内到位");
        assert!(
            (48..=49).contains(&full),
            "到位帧号应约为 48（f32 累加允许 ±1 帧），实际 {full}"
        );
        assert!(values[full..].iter().all(|v| *v == 1.0), "到位后应钳住恒定");
        assert!(ramp.settled());

        // 淡出：从 1 线性降到 0，负步长
        let mut down = GainRamp::constant(1.0);
        down.set(0.0, 1.0 / 48_000.0, 48_000); // 1 帧
        assert!(down.step < 0.0);
        assert_eq!(down.advance(), 1.0, "首帧仍是起始值");
        assert!(down.settled(), "1 帧后到位");
        assert_eq!(down.value, 0.0);

        // 非有限时长 = 瞬时到目标（不产生 NaN 步长）
        let mut instant = GainRamp::constant(0.3);
        instant.set(0.8, f32::NAN, 48_000);
        assert_eq!(instant.value, 0.8);
        assert!(instant.settled());
    }

    /// 解码线程的进展保护：外部流「返回 true 但零产出」被打转上限拦住。
    #[test]
    fn decode_loop_stops_on_starving_stream() {
        struct Starving;
        impl AudioStream for Starving {
            fn channels(&self) -> u16 {
                1
            }
            fn sample_rate(&self) -> u32 {
                48_000
            }
            fn next_block(&mut self, _out: &mut Vec<f32>) -> bool {
                true
            }
        }
        let queue = BlockQueue::new(2);
        let start = Instant::now();
        decode_loop(&mut Starving, &queue, false);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "空转上限应让解码线程自行退出"
        );
        assert!(
            matches!(queue.poll(), Polled::Empty),
            "未标结束时判空不是判结束"
        );
    }

    /// 不可 rewind 的流：`looping = true` 也照样播完即止（默认实现返回 false）。
    #[test]
    fn non_rewindable_stream_ends_even_when_looping() {
        struct Once(Vec<f32>);
        impl AudioStream for Once {
            fn channels(&self) -> u16 {
                1
            }
            fn sample_rate(&self) -> u32 {
                48_000
            }
            fn next_block(&mut self, out: &mut Vec<f32>) -> bool {
                out.extend_from_slice(&std::mem::take(&mut self.0));
                false
            }
        }
        let queue = BlockQueue::new(2);
        decode_loop(&mut Once(vec![0.5, -0.5]), &queue, true);
        match queue.poll() {
            Polled::Block(block) => assert_eq!(block, vec![0.5, -0.5]),
            other => panic!("首块应被交付，实际 {other:?}"),
        }
        assert!(matches!(queue.poll(), Polled::Empty));
        queue.mark_finished();
        assert!(matches!(queue.poll(), Polled::End));
    }
}
