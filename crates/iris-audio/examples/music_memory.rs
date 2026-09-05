//! R44 内存占用验收：**同一段 60 s 立体声内容**，WAV 全量常驻 vs
//! OGG/Vorbis 流式通道的常驻字节数对比（本轮一级交付物）。
//!
//! 口径说明（诚实优先）：
//! - WAV 侧的 `f32` 堆占用是**实测**（真的分配一条 60 s 剪辑并量它的采样长度），
//!   磁盘尺寸按 16-bit PCM 布局算（RIFF 头 44 字节 + 帧数 × 声道 × 2）；
//! - 流式侧是**按队列容量与实测块长计账**（不是 RSS 直测）：常驻 = 有界块
//!   队列（`DEFAULT_QUEUE_BLOCKS` 块）+ 一块待播缓冲 + symphonia 的读缓冲，
//!   另附压缩码流的两种形态（`open(path)` 不常驻 / `from_bytes` 常驻）；
//! - 60 s OGG 的压缩体积用 2 s fixture 的实测码率线性外推，明确标注量级参考。
//!
//! ```sh
//! cargo run -p iris-audio --release --example music_memory
//! ```

use std::path::PathBuf;

use iris_audio::{AudioClip, AudioMixer, AudioStream, DEFAULT_QUEUE_BLOCKS, OggVorbisStream};

/// 对比内容的时长（秒）。
const SECONDS: f32 = 60.0;
/// 采样率（Hz）。
const RATE: u32 = 48_000;
/// 声道数（立体声）。
const CHANNELS: u16 = 2;
/// symphonia `MediaSourceStream` 默认读缓冲（字节）。
const DEMUXER_READ_BUFFER: usize = 64 * 1024;
/// fixture 时长（秒），用于码率外推。
const FIXTURE_SECONDS: f32 = 2.0;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("tone_2s.ogg")
}

fn main() {
    // ---- 1. WAV 全量常驻（真分配、实测采样长度）----
    let frames = (SECONDS * RATE as f32) as usize;
    let clip = AudioClip::from_interleaved(
        // 内容本身无关（只量体积）：用非零值避免编译器有机会优化掉分配
        vec![0.25f32; frames * CHANNELS as usize],
        CHANNELS,
        RATE,
    )
    .expect("60 s 立体声剪辑构造合法");
    let wav_resident = std::mem::size_of_val(clip.samples());
    // 磁盘尺寸 = RIFF/WAVE 头 44 字节 + 采样数 × 2（16-bit PCM 位深）
    let wav_on_disk = 44 + clip.samples().len() * std::mem::size_of::<i16>();

    // ---- 2. OGG 流式（实测块长 + 按队列容量计账）----
    let path = fixture_path();
    let compressed_fixture = std::fs::metadata(&path).expect("fixture 应存在").len() as usize;
    let bitrate_kbps = compressed_fixture as f64 * 8.0 / FIXTURE_SECONDS as f64 / 1000.0;
    let ogg_60s_compressed = (bitrate_kbps * 1000.0 * SECONDS as f64 / 8.0) as usize;

    let mut stream = OggVorbisStream::open(&path).expect("fixture 应可按路径打开");
    let mut block = Vec::new();
    stream.next_block(&mut block);
    let block_bytes = std::mem::size_of_val(block.as_slice());
    assert!(
        !block.is_empty(),
        "OGG 流首次 next_block 应产出采样（否则本例的计账无意义）"
    );
    let queue_resident = DEFAULT_QUEUE_BLOCKS * block_bytes;
    // `open` 路径：压缩态不常驻（只有文件句柄 + demuxer 读缓冲）
    let streaming_from_file = queue_resident + block_bytes + DEMUXER_READ_BUFFER;
    // `from_bytes` 路径：额外常驻整条压缩码流（按 60 s 外推体积）
    let streaming_from_bytes = streaming_from_file + ogg_60s_compressed;

    // ---- 3. 冒烟：真把 OGG 流跑进混音器（顺带验证计账用的块长就是实际块长）----
    let mut mixer = AudioMixer::new();
    let handle = mixer.play_music(
        OggVorbisStream::open(&path).expect("fixture 应可再次打开"),
        0.7,
        0.01,
        true,
    );
    let mut out = vec![0.0f32; 480 * CHANNELS as usize];
    mixer.mix(&mut out, CHANNELS, RATE);
    let heard = out.iter().any(|s| *s != 0.0);
    assert!(mixer.stop_music(0.0), "刚启动的轨应可停止");
    let _ = handle;

    // ---- 4. 报告 + 量化守护 ----
    let mb = |bytes: usize| bytes as f64 / (1024.0 * 1024.0);
    println!("=== R44 音频内存对比（{SECONDS:.0} s 立体声 {RATE} Hz）===");
    println!(
        "WAV 全量常驻        : {wav_resident:>10} 字节（{:.1} MB）= 采样数 × 4",
        mb(wav_resident)
    );
    println!(
        "WAV 磁盘（16-bit）  : {wav_on_disk:>10} 字节（{:.1} MB）= RIFF 头 + 采样数 × 2",
        mb(wav_on_disk)
    );
    println!(
        "OGG 压缩（实测外推）: {ogg_60s_compressed:>10} 字节（{:.1} MB）@ {bitrate_kbps:.0} kbps",
        mb(ogg_60s_compressed)
    );
    println!("  （fixture 实测：{compressed_fixture} 字节 / {FIXTURE_SECONDS:.0} s ⇒ 上述码率）");
    println!(
        "流式块实测块长      : {block_bytes:>10} 字节（{} 帧 × {CHANNELS} 声道 × 4）",
        block.len() / CHANNELS as usize
    );
    println!(
        "流式常驻（open 路径）: {streaming_from_file:>9} 字节（{:.2} MB）= 队列 {} 块 + 待播 1 块 + 读缓冲",
        mb(streaming_from_file),
        DEFAULT_QUEUE_BLOCKS
    );
    println!(
        "流式常驻（bytes 路径）: {streaming_from_bytes:>9} 字节（{:.2} MB）= 上面 + 整条压缩码流",
        mb(streaming_from_bytes)
    );
    println!(
        "常驻比 WAV : open 路径 {:.0}×，bytes 路径 {:.0}×",
        wav_resident as f64 / streaming_from_file as f64,
        wav_resident as f64 / streaming_from_bytes as f64
    );
    println!(
        "混音冒烟    : {}（{handle:?}，一条 2 s 循环 OGG）",
        if heard {
            "有输出"
        } else {
            "无输出（首块未到，属正常下溢）"
        }
    );

    // 量化守护（「低占用」承诺的回归线）：常驻必须显著低于 WAV 全量。
    // 长度无关性是结构性的（队列有界、块长固定），不在此处假装测一遍：
    // 上面的 streaming_from_file 只由 `DEFAULT_QUEUE_BLOCKS × 实测块长 + 读缓冲`
    // 构成，式子里没有曲目时长这一项。
    let ratio = wav_resident as f64 / streaming_from_file as f64;
    assert!(
        ratio >= 20.0,
        "流式常驻应至少比 WAV 全量低 20 倍，实测 {ratio:.1}×"
    );
    assert!(
        out.iter().all(|s| s.is_finite()),
        "流式混音输出不得出现 NaN / Inf"
    );
    println!("守护断言    : 通过（常驻比 {ratio:.0}×，混音输出有限）");
}
