//! 真实 OGG/Vorbis 资产的集成测试（R44）。
//!
//! fixture：`tests/fixtures/tone_2s.ogg`（48 kHz 立体声、2 s、440 Hz 正弦 +
//! 白噪声经限幅，实测 93 kb/s / 23 KB / 峰值 ≈ 0.5）。
//!
//! 覆盖：布局与长度、解码确定性（逐位）、流式与一次性解码路径逐位一致、
//! 流式经混音器输出的采样级直通、截断输入的容错。
//!
//! 重生成命令（资产丢失或要换内容时；需要 ffmpeg 带 libvorbis）：
//! ```sh
//! ffmpeg -y -loglevel error -f lavfi -i "sine=frequency=440:sample_rate=48000:duration=2" \
//!   -f lavfi -i "anoisesrc=color=white:amplitude=0.3:sample_rate=48000:duration=2" \
//!   -filter_complex "amix=inputs=2:duration=longest:normalize=0,alimiter=limit=0.9" \
//!   -ac 2 -c:a libvorbis -q:a 3 crates/iris-audio/tests/fixtures/tone_2s.ogg
//! ```

use std::path::PathBuf;

use iris_audio::{AudioClip, AudioMixer, AudioStream, OggVorbisStream, load_ogg, parse_ogg};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("tone_2s.ogg")
}

fn fixture_bytes() -> Vec<u8> {
    std::fs::read(fixture()).expect("OGG fixture 应存在（见模块注释的重生成命令）")
}

/// 布局与长度：48 kHz 立体声；帧数落在 2 s 附近（Vorbis 编码窗不做 gapless
/// 修剪 ⇒ 首尾各多几毫秒，实测值见断言失败消息）。
#[test]
fn decodes_layout_and_length() {
    let bytes = fixture_bytes();
    let clip = parse_ogg(&bytes).expect("fixture 应可解码");
    assert_eq!((clip.channels(), clip.sample_rate()), (2, 48_000));
    let frames = clip.frame_count();
    println!(
        "OGG 实测：{} 字节 → {frames} 帧（{:.3} s）",
        bytes.len(),
        frames as f32 / 48_000.0
    );
    assert!(
        (96_000..=98_000).contains(&frames),
        "2 s @ 48 kHz fixture 应解出 96000 帧附近（实测恰为 96000：symphonia 按 \
         Ogg 粒位置处理了首尾窗口），实际 {frames}"
    );
    // 内容 sanity：有限、满幅级别、确实在振动
    let samples = clip.samples();
    assert_eq!(samples.len(), frames * 2, "交错布局长度 = 帧数 × 声道数");
    assert!(
        samples.iter().all(|s| s.is_finite()),
        "解码不得产出 NaN / Inf"
    );
    let peak = samples.iter().fold(0.0f32, |acc, s| acc.max(s.abs()));
    // fixture 实测 max_volume −6.1 dB ⇒ 峰值约 0.495（留一半余量防编码微调）
    assert!(peak > 0.3, "应有可闻信号，峰值 {peak}");
    assert!(peak <= 1.05, "Vorbis 峰值应不超满幅太多，实际 {peak}");
    let (mut min, mut max) = (f32::MAX, f32::MIN);
    for s in samples {
        min = min.min(*s);
        max = max.max(*s);
    }
    assert!(max - min > 0.6, "全曲应有明显摆动，实际 [{min}, {max}]");
    // 440 Hz 单音成分 ⇒ 前 0.1 s（4800 帧）理论过零约 88 次（噪声会增加）
    let crossings = {
        let left: Vec<f32> = samples
            .chunks_exact(2)
            .take(4_800)
            .map(|frame| frame[0])
            .collect();
        left.windows(2)
            .filter(|w| (w[0] > 0.0) != (w[1] > 0.0))
            .count()
    };
    println!("前 0.1 s 左声道过零次数：{crossings}");
    assert!(
        (800..=3_000).contains(&crossings),
        "fixture 是正弦 + 白噪声，过零密度应接近宽带信号理论值（实测 1660），\
         区间外说明解码出了静音 / 直流 / 乱码：实际 {crossings}"
    );
}

/// 解码确定性：同一字节两次解码逐位一致（同机器上可复现，是快照比对的前提）。
#[test]
fn decoding_is_bit_deterministic() {
    let bytes = fixture_bytes();
    let a = parse_ogg(&bytes).expect("第一次解码");
    let b = parse_ogg(&bytes).expect("第二次解码");
    assert_eq!(
        a.samples().iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
        b.samples().iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
        "同输入两次解码应逐位一致"
    );
}

/// **流式与一次性解码路径逐位一致**：两条路径共用同一解码器与同一
/// 平面→交错重排，因此块拼接后的序列必须与整条解码完全相同。
#[test]
fn streaming_blocks_match_full_decode() {
    let bytes = fixture_bytes();
    let whole = parse_ogg(&bytes).expect("一次性解码");
    let mut stream = OggVorbisStream::from_bytes(bytes).expect("流式构造");
    assert_eq!(
        (stream.channels(), stream.sample_rate()),
        (whole.channels(), whole.sample_rate()),
        "布局回读应与一次性解码一致"
    );
    stream = stream.with_block_frames(1_000);
    let mut heard = Vec::new();
    let mut calls = 0;
    while stream.next_block(&mut heard) {
        calls += 1;
        assert!(calls < 400, "应在有限步内到达流尾（防死转）");
    }
    assert_eq!(
        heard.len() % whole.channels() as usize,
        0,
        "交错采样长度应为声道数整数倍"
    );
    let bits = |v: &[f32]| -> Vec<u32> { v.iter().map(|s| s.to_bits()).collect() };
    assert_eq!(
        bits(&heard),
        bits(whole.samples()),
        "流式分块拼接应与一次性整条解码逐位一致（{calls} 次调用）"
    );
    // rewind 回到起点（循环能力的真实资产验证）
    assert!(stream.rewind(), "OGG 流应支持 rewind");
    let mut again = Vec::new();
    stream.next_block(&mut again);
    assert!(!again.is_empty(), "rewind 后应能重新产出");
    assert_eq!(
        bits(&again),
        bits(&heard[..again.len()]),
        "rewind 后首块应与原首块逐位一致"
    );
}

/// **接进混音器的采样级直通**：48 kHz 流 → 48 kHz 输出、音量 1.0、无淡变，
/// 第一个混音缓冲（不超过 play 时预探的那一块）必须逐位等于解码采样。
#[test]
fn music_through_mixer_is_sample_exact() {
    let bytes = fixture_bytes();
    let clip = parse_ogg(&bytes).expect("一次性解码作参照");
    let mut mixer = AudioMixer::new();
    let handle = mixer.play_music(
        OggVorbisStream::from_bytes(bytes)
            .expect("流式构造")
            .with_block_frames(256),
        1.0,
        0.0,
        false,
    );
    assert!(mixer.music_is_playing(), "OGG 流应占用音乐总线");
    assert_eq!(mixer.music_handle(), Some(handle));

    // 256 帧 < 预探块 ⇒ 无需等待后台线程，数据必已就位
    let mut out = vec![7.0f32; 256 * 2];
    mixer.mix(&mut out, 2, 48_000);
    for (i, (got, want)) in out.iter().zip(clip.samples()).enumerate() {
        assert_eq!(
            got.to_bits(),
            want.to_bits(),
            "OGG 流式经混音器输出应逐位等于解码采样 @{i}"
        );
    }
    assert!(mixer.stop_music(0.0));
    assert!(mixer.join_music_producer(), "应回收后台解码线程");
}

/// `load_ogg`（文件路径）与 `parse_ogg`（内存字节）同结果。
#[test]
fn load_from_path_matches_parse() {
    let by_path = load_ogg(fixture()).expect("按路径加载");
    let by_bytes = parse_ogg(&fixture_bytes()).expect("按字节加载");
    assert_eq!(by_path.frame_count(), by_bytes.frame_count());
    assert_eq!(
        by_path
            .samples()
            .iter()
            .map(|s| s.to_bits())
            .collect::<Vec<_>>(),
        by_bytes
            .samples()
            .iter()
            .map(|s| s.to_bits())
            .collect::<Vec<_>>(),
        "两条加载路径应产出完全相同的采样"
    );
    assert!((by_path.duration_secs() - 2.0).abs() < 0.1);
}

/// 截断的码流：不得 panic；要么明确报错（实测为 `Io(UnexpectedEof)`——
/// symphonia 的「读到流尾」就是 Io 类信号，截断文件在建 demuxer 阶段即撞上），
/// 要么只解出更少帧（绝不臆造出完整内容）。
#[test]
fn truncated_stream_is_reported_not_panicking() {
    let bytes = fixture_bytes();
    let full = parse_ogg(&bytes).expect("完整 fixture");
    for cut in [bytes.len() / 8, bytes.len() / 2] {
        match parse_ogg(&bytes[..cut]) {
            Ok(clip) => assert!(
                clip.frame_count() < full.frame_count(),
                "截断到 {cut} 字节却解出完整长度（{} 帧）",
                clip.frame_count()
            ),
            Err(err) => {
                let text = err.to_string();
                assert!(!text.is_empty(), "错误必须带可读信息（截断到 {cut} 字节）");
            }
        }
    }
}

/// `AudioClip::from_ogg` 门面入口与 `parse_ogg` 同语义（走既有音效路径）。
#[test]
fn audio_clip_from_ogg_matches_parse_ogg() {
    let bytes = fixture_bytes();
    let via_clip = AudioClip::from_ogg(&bytes).expect("from_ogg 应成功");
    let via_parse = parse_ogg(&bytes).expect("parse_ogg 应成功");
    assert_eq!(via_clip.samples().len(), via_parse.samples().len());
    assert_eq!(
        via_clip
            .samples()
            .iter()
            .map(|s| s.to_bits())
            .collect::<Vec<_>>(),
        via_parse
            .samples()
            .iter()
            .map(|s| s.to_bits())
            .collect::<Vec<_>>(),
        "两个入口应完全同源"
    );
}
