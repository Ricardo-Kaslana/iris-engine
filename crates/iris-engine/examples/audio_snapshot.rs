//! 音频子系统视觉验收：软件混音器编排三段程序化音效，把混音波形画成
//! 240 根青→橙渐变垂直条（复用 UI 矩形系统），离屏渲染输出 `iris_audio.bmp`。
//!
//! 同时验证：
//! - 程序化生成器（55Hz boom / 880Hz blip / 噪声 burst，确定性生成）；
//! - [`AudioMixer`] 的时间编排（按时间点 `play` + 分块 `mix` 推进时钟）；
//! - UI 批处理规模能力（481 个 UiRect 实体 + 7 个文本实体，单批渲染）；
//! - 真实音频设备链路（有设备时用同一编排真声播放一遍；无设备打印降级信息）。
//!
//! 波形包络应清晰呈现三段（起始强峰 boom、中段小峰 blip、后段尖峰噪声），
//! 且无爆音削顶的平顶（生成器首尾 5ms 淡入淡出 + 峰值 0.95 < 限幅阈值）。
//!
//! ```sh
//! cargo run --example audio_snapshot --release
//! ffmpeg -y -loglevel error -i iris_audio.bmp docs/audio_preview.png
//! ```

use std::time::{Duration, Instant};

use iris_engine::prelude::*;

/// 输出混音的采样率（Hz）。
const OUT_RATE: u32 = 48_000;
/// 输出混音的声道数（立体声）。
const OUT_CHANNELS: u16 = 2;
/// 编排总时长（秒）。
const TOTAL_SECS: f32 = 2.0;
/// 波形条数（每条覆盖 400 帧 = 8.33ms）。
const BARS: usize = 240;

fn main() {
    env_logger::init();

    // ---- 1. 程序化生成三段音效（确定性，无随机源） ----
    let boom = sine(55.0, 0.5, OUT_RATE); // 低频重音 → 起始强峰
    let blip = sine(880.0, 0.15, OUT_RATE); // 高频短音 → 中段小峰
    let burst = noise(0.1, OUT_RATE, 0x5EED_2026); // 噪声 → 后段尖峰
    let clips = [boom, blip, burst];
    let starts = [0.0f32, 0.4, 0.7]; // 编排时间点
    // 音量之和 < 1.0（boom+blip 重叠段 = 0.95）：全程不触软限幅，
    // 波形图不出现削顶平顶——线性叠加在图上直接可读
    let volumes = [0.65f32, 0.3, 0.5];

    // ---- 2. AudioMixer 编排：按时间点 play，分块 mix 推进（每块 10ms） ----
    let mut mixer = AudioMixer::new();
    mixer.play(clips[0].clone(), volumes[0], false);
    let mut played = 1usize;
    let total_frames = (TOTAL_SECS * OUT_RATE as f32) as usize;
    let chunk_frames = 480usize; // 10ms @ 48kHz
    let mut chunk = vec![0.0f32; chunk_frames * OUT_CHANNELS as usize];
    let mut mixdown = Vec::with_capacity(total_frames * OUT_CHANNELS as usize);
    let mut done_frames = 0usize;
    while done_frames < total_frames {
        // 时钟到达事件点 → 入表新声音（下一块混音即刻带上它）
        let t = done_frames as f32 / OUT_RATE as f32;
        while played < clips.len() && t >= starts[played] {
            mixer.play(clips[played].clone(), volumes[played], false);
            played += 1;
        }
        let frames = chunk_frames.min(total_frames - done_frames);
        let buf = &mut chunk[..frames * OUT_CHANNELS as usize];
        mixer.mix(buf, OUT_CHANNELS, OUT_RATE);
        mixdown.extend_from_slice(buf);
        done_frames += frames;
    }
    assert_eq!(mixer.active_count(), 0, "2 秒后全部非循环声音应自然播完");
    println!(
        "离线混音完成：{} 帧（{TOTAL_SECS}s @ {OUT_RATE}Hz × {OUT_CHANNELS}ch），3 个事件点",
        mixdown.len() / OUT_CHANNELS as usize
    );

    // ---- 3. 波形 → 240 根归一化垂直条 ----
    let frames_per_bar = total_frames / BARS;
    let mut peaks = [0.0f32; BARS];
    for (bar, peak) in peaks.iter_mut().enumerate() {
        for f in bar * frames_per_bar..(bar + 1) * frames_per_bar {
            let l = mixdown[f * 2].abs();
            let r = mixdown[f * 2 + 1].abs();
            *peak = peak.max(l).max(r);
        }
    }
    let max_peak = peaks.iter().cloned().fold(1e-6f32, f32::max);

    // ---- 4. UI 场景：暗底 + 双层条（暗色底条 + 渐变波形条）+ 标注 ----
    let Some(mut renderer) = OffscreenRenderer::new(1280, 720) else {
        eprintln!("无 GPU 适配器，视觉验收跳过（混音结果已在内存中验证）");
        std::process::exit(1);
    };
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    // 纯图表场景：泛光与天空会污染色块读数，显式关闭泛光（背景由全屏暗底矩形覆盖）
    world.insert_resource(BloomSettings { enabled: false, ..BloomSettings::default() });

    const W: f32 = 1280.0;
    const H: f32 = 720.0;
    const BASELINE: f32 = H - 96.0; // 波形基线（底条上沿）
    const MAX_BAR_H: f32 = 460.0; // 满幅条高
    const BASE_H: f32 = 6.0; // 底条固定高度（双层模式，让波形可读）
    const SLOT_W: f32 = W / BARS as f32;
    const BAR_W: f32 = SLOT_W - 1.5;

    // 全屏暗底（UI pass 在色调映射后叠加，直接盖住天空背景）
    world.spawn((
        UiRect { position: [0.0, 0.0], size: [W, H] },
        UiColor([0.016, 0.024, 0.04, 1.0]),
    ));

    // 240 组双层条：底条固定暗色，波形条青(0.2,0.9,0.9)→橙(1.0,0.55,0.15)
    // 随幅值渐变，并按幅值乘亮度（UiColor 是线性 RGBA，直接相乘即可压暗）
    for (bar, peak) in peaks.iter().enumerate() {
        let amp = peak / max_peak; // 归一化 [0, 1]
        let x = bar as f32 * SLOT_W + (SLOT_W - BAR_W) * 0.5;
        world.spawn((
            UiRect { position: [x, BASELINE - BASE_H], size: [BAR_W, BASE_H] },
            UiColor([0.09, 0.11, 0.15, 1.0]),
        ));
        let glow = 0.3 + 0.7 * amp; // 幅值越低越暗
        let color = [
            lerp(0.2, 1.0, amp) * glow,
            lerp(0.9, 0.55, amp) * glow,
            lerp(0.9, 0.15, amp) * glow,
            1.0,
        ];
        let bar_h = (amp * MAX_BAR_H).max(1.0);
        world.spawn((
            UiRect { position: [x, BASELINE - BASE_H - bar_h], size: [BAR_W, bar_h] },
            UiColor(color),
        ));
    }

    // 标注文本（实体的 UiRect 仅作定位载体，不绘制矩形）
    world.spawn((
        UiRect { position: [24.0, 24.0], size: [0.0, 0.0] },
        UiText { string: "IRIS AUDIO MIXER".into(), scale: 4.0 },
        UiColor([0.85, 0.9, 0.95, 1.0]),
    ));
    world.spawn((
        UiRect { position: [24.0, 68.0], size: [0.0, 0.0] },
        UiText { string: "boom 55hz @ 0.0s   blip 880hz @ 0.4s   noise burst @ 0.7s".into(), scale: 2.0 },
        UiColor([0.45, 0.52, 0.6, 1.0]),
    ));
    for (i, label) in ["0", "0.5", "1.0", "1.5", "2.0s"].iter().enumerate() {
        // 末刻度按自身宽度（4 字形 × 8px × scale 2）左移，避免裁出右边缘
        let x = 24.0 + (W - 48.0) * i as f32 / 4.0
            - if i == 4 { 4.0 * 8.0 * 2.0 } else { 0.0 };
        world.spawn((
            UiRect { position: [x, BASELINE + 10.0], size: [0.0, 0.0] },
            UiText { string: (*label).into(), scale: 2.0 },
            UiColor([0.4, 0.46, 0.54, 1.0]),
        ));
    }

    propagate_transforms(&mut world);
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    renderer.save_bmp("iris_audio.bmp").expect("写 BMP");
    println!(
        "已输出 iris_audio.bmp：{} 个矩形实体 + {} 个文本实体 → {} draw call（UI 单批合包）",
        BARS * 2 + 1,
        2 + 5,
        stats.draw_calls
    );

    // ---- 5. 真实设备播放同一编排（无设备时打印降级信息） ----
    let audio = Audio::new();
    if audio.has_device() {
        println!(
            "输出设备：{} Hz × {} 声道，真实播放编排一遍…",
            audio.sample_rate(),
            audio.channels()
        );
        let t0 = Instant::now();
        audio.play(clips[0].clone(), volumes[0], false);
        for i in 1..clips.len() {
            let wait = starts[i] - t0.elapsed().as_secs_f32();
            if wait > 0.0 {
                std::thread::sleep(Duration::from_secs_f32(wait));
            }
            audio.play(clips[i].clone(), volumes[i], false);
        }
        let tail = TOTAL_SECS - t0.elapsed().as_secs_f32() + 0.15;
        std::thread::sleep(Duration::from_secs_f32(tail.max(0.0)));
        println!("真实设备播放完毕");
    } else {
        println!("无可用音频输出设备：已降级为纯混音器模式（play 正常但不发声）");
    }
}

/// 线性插值。
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
