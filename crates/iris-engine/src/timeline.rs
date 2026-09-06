//! 演出时间线 Sequencer-lite（R47）：写实动作游戏剧情演出的最小骨架——
//! 轨道时间线（相机 spline 轨道 + 事件轨道 + 对话文本 + 黑边条）。
//!
//! # 轨道模型
//!
//! [`Timeline`] 是纯数据结构（零渲染依赖之外的逻辑，采样全部为可脱离
//! ECS 单测的纯函数）：
//!
//! - **相机轨道** [`CameraTrack`]：`(time, eye, target)` 关键帧序列
//!   （≥ 2，眼点与注视点各为一条 spline）。`ease = true` 走
//!   Catmull-Rom（张力固定 0.5，过全部关键帧、段间 C1 连续，端点用
//!   重复端点夹持）；`ease = false` 关键帧间线性插值。时间轴按关键帧
//!   `time` 非均匀分段，段内用归一化参数 `u ∈ [0, 1]` 求值（段内均匀
//!   参数化，C1 连续性在 spline 参数空间成立）；
//! - **事件轨道** [`TimelineEvent`]：`(time, name)` 标记，跨过即发——
//!   跨界检测复用动画事件轨道的公共数学
//!   [`collect_crossings`](iris_render::animation::collect_crossings)
//!   （单一来源，回绕 / multi-fire / 末端不重复语义与动画逐位一致），
//!   命中的事件经 `Events<TimelineEvent>` 产出（消费契约见下）；
//! - **对话轨道** [`DialogueEntry`]：`[time, end_time)` 活动窗口内的文本；
//!   v1 无打字机、无发言人头像（诚实限制）；
//! - **黑边条**（letterbox）：`TimelinePlayer::letterbox` 开启时播放期间
//!   在屏幕上下各拉一条全宽黑边（高度 = 视口高 × `letterbox_ratio`），
//!   实现为两条固定 [`Name`] 的 UI 矩形实体显隐（复用既有 UI 矩形管线，
//!   不新增渲染路径；不做圆角 / 进出动画）。
//!
//! # 播放器形态（Resource）
//!
//! [`TimelinePlayer`] 是 **World 资源**（每世界至多一个）：v1 定位为
//! 「剧情演出独占主相机与对话 UI」，多条时间线并发播放需要多个相机 /
//! 黑边写入方，语义上必然互相打架——并发演出 v1 明确不支持（诚实限制），
//! 选 Resource 而非 Component 让「每世界一个」成为类型系统外的一行
//! 文档约定 + `start_timeline` 直接可用的 API（Component 形态要引入
//! 「哪个播放器说了算」的仲裁问题，v1 无解）。
//!
//! # 相机接管与恢复
//!
//! [`start_timeline`] 快照主相机原姿态（**优先 `ActiveCamera` 实体**
//! 的局部 `Transform`，回落 `Camera` 资源的 `eye / target`——与渲染器的
//! 相机解析优先级一致）；播放期间每帧以轨道采样值**覆写**主相机
//! （实体模式写 `Transform.position` + 注视旋转，资源模式写
//! `eye / target`）；[`stop_timeline`] 或播放完成时把快照**逐位写回**
//! （f32 值拷贝无舍入，演出后无缝回游戏）。快照在恢复之后才对新一轮
//! start 生效（先还旧债再借新债）。演出期间**不自动拦截游戏输入**：
//! 是否忽略输入由游戏侧决定（v1 无输入屏蔽层，诚实限制）；同理，游戏
//! 侧的相机跟随系统（如 SmoothFollow）在演出期间应暂停，否则会与时间
//! 线争写主相机（Update 内后写的赢）。
//!
//! # 对话与黑边的 UI 手段
//!
//! 复用既有 UI 叠加层：对话框 = 一条固定 [`Name`]
//! （`iris/timeline_dialogue`）的 `UiRect + UiText + UiColor + Visibility`
//! 实体，活动窗口内有条目则写文本 + 显示（水平居中、底部黑边上方），
//! 无条目 / 停止 / 播完隐藏；黑边 = `iris/timeline_letterbox_top` /
//! `_bottom` 两条全宽黑色矩形实体。三类实体**缺席时惰性生成**（独占
//! 系统可 spawn； despawn 后下一帧自动重建——句柄不跨帧缓存，按
//! [`World::entity_named`] 查找，O(n) 线性扫描对每帧三条实体可接受）。
//! 位图字体仅 ASCII（非 ASCII 字符跳过绘制——既有 `UiText` 语义），
//! 中文对白 v1 显示为空，请用本地化后的 ASCII 文本或等游戏侧自行扩展。
//!
//! # 编排
//!
//! [`update_timeline`] 由 [`crate::plugin::TimelinePlugin`] 注册为
//! **Update 阶段独占系统**（动画之后、物理之前）：时间线写相机局部
//! `Transform` / UI 组件，必须早于 PostUpdate 的层级传播（实体相机
//! `GlobalTransform` 当帧新鲜）与渲染收集；独占（`&mut World`）而非
//! 并行的理由：① 对话 / 黑边实体可能需要惰性 spawn（结构性修改）；
//! ② 相机接管是「演出独占」语义，独占系统的天然屏障让本帧时间线
//! 写入对其后的并行系统立即可见。独占 = Update 批间屏障，v1 接受
//! （演出期间物理 / 动画照常并行，只多一次批切换）。
//!
//! # 事件消费契约
//!
//! 命中的 [`TimelineEvent`] 经 `Events<TimelineEvent>` 产出（插件注册，
//! 游戏侧不注册即得）。生命周期按事件双代缓冲的通用契约：**Update 产出
//! → 同帧更晚系统与下一帧全程可读，消费方 `drain`**——每帧应恰好一个
//! drain 消费方（或消费逻辑对重复可见幂等）。示范见
//! `examples/timeline_snapshot.rs`（第 3 秒 `door_open` 事件 → 游戏侧
//! 消费后开门）。
//!
//! # 确定性
//!
//! 采样与推进全为纯 f32 运算（无 rand、无全局状态、无哈希容器迭代），
//! 同一 timeline 在同一帧序下两次播放逐位一致（单测锁定）。

use glam::{Mat3, Quat, Vec3};
use iris_core::entity::Entity;
use iris_core::prelude::{Name, Time, Transform};
use iris_core::world::World;
use iris_render::animation::collect_crossings;
use iris_render::ui::{UiColor, UiRect, UiText, text_width};
use iris_render::visibility::Visibility;

/// 对话框实体名（约定：游戏侧按此名识别 / 预生成）。
pub const DIALOGUE_NAME: &str = "iris/timeline_dialogue";
/// 上黑边实体名。
pub const LETTERBOX_TOP_NAME: &str = "iris/timeline_letterbox_top";
/// 下黑边实体名。
pub const LETTERBOX_BOTTOM_NAME: &str = "iris/timeline_letterbox_bottom";

/// 对话文本字形放大倍数（位图字体基准 8×8 像素，2 = 16 px 字高）。
const DIALOGUE_SCALE: f32 = 2.0;
/// 对话文本底部留白（像素，位于下黑边上方——黑边高度由播放器动态传入）。
const DIALOGUE_BOTTOM_MARGIN: f32 = 24.0;
/// 对话文本颜色（白）。
const DIALOGUE_COLOR: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// 黑边颜色（不透明黑）。
const LETTERBOX_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// 视口资源缺席时的兜底尺寸（与 `Viewport::default()` 一致）。
const DEFAULT_VIEWPORT: [f32; 2] = [1280.0, 720.0];

/// 时间线：演出脚本的纯数据载体（轨道模型见模块文档）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Timeline {
    /// 总时长（秒）；`<= 0` 视为空时间线（启动即播完）。
    pub duration: f32,
    /// 相机轨道（None = 纯事件 / 对话演出，相机完全不动）。
    pub camera: Option<CameraTrack>,
    /// 事件轨道（时间任意序均可，产出恒按触发时刻排序；
    /// 事件时刻建议落在 `(0, duration]`——恰在 0 的事件永不触发，
    /// 因为命中区间是 `(prev, new]` 左开右闭）。
    pub events: Vec<TimelineEvent>,
    /// 对话轨道（时间任意序均可；活动窗口重叠时声明序后者胜）。
    pub dialogue: Vec<DialogueEntry>,
}

impl Timeline {
    /// 空时间线（指定时长；轨道后续按需填充）。
    pub fn new(duration: f32) -> Self {
        Self {
            duration,
            ..Default::default()
        }
    }
}

/// 时间线事件：跨过 `time` 即发（跨界语义与动画事件轨道逐位一致，
/// 见 [`collect_crossings`](iris_render::animation::collect_crossings)）。
#[derive(Clone, Debug, PartialEq, iris_core::Event)]
pub struct TimelineEvent {
    /// 触发时刻（秒，相对时间线起点）。
    pub time: f32,
    /// 事件名（消费方按名分发）。
    pub name: String,
}

/// 对话条目：`[time, end_time)` 活动窗口内显示的文本。
#[derive(Clone, Debug, PartialEq)]
pub struct DialogueEntry {
    /// 开始显示时刻（秒，含）。
    pub time: f32,
    /// 结束显示时刻（秒，不含——同刻「结束」与「另一条开始」不闪帧）。
    pub end_time: f32,
    /// 文本内容（位图字体仅 ASCII，见模块文档）。
    pub text: String,
}

/// 相机关键帧：`time` 时刻的眼点与注视点。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraKey {
    /// 关键帧时刻（秒，相对时间线起点；应随下标非递减）。
    pub time: f32,
    /// 眼点（世界系）。
    pub eye: Vec3,
    /// 注视点（世界系）。
    pub target: Vec3,
}

/// 相机轨道：眼点与注视点各过一条 spline 的「锁定注视」相机路径。
///
/// 关键帧 ≥ 2（单关键帧 / 空轨道采样为恒定值，不 panic——坏数据不报废
/// 演出）；`ease = true` 关键帧间 Catmull-Rom 平滑，`false` 线性。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CameraTrack {
    /// 关键帧序列（`time` 随下标非递减）。
    pub keys: Vec<CameraKey>,
    /// 关键帧间平滑（Catmull-Rom）vs 线性插值。
    pub ease: bool,
}

/// Catmull-Rom 样条采样（纯函数，张力固定 0.5）。
///
/// `points` 为控制点序列，`t` 为**段参数**：`t = i` 恰过 `points[i]`
/// （`t ∈ [0, n-1]`，越界钳制到两端）。首尾端点用**重复端点夹持**——
/// 首段的 `p0` 取 `points[0]`、末段的 `p3` 取 `points[n-1]`（切向退化为
/// 过端点的零切向缓入缓出，不外推）。段内均匀参数化（不按弦长 / 向心
/// 重参数化——相机关键帧间距人工调校，均匀已够；v1 诚实限制）。
///
/// 退化：单点恒返回该点；空表返回 `Vec3::ZERO`（调用方保证 ≥ 1 时可
/// 忽略此分支）。`points.len() == 2` 时退化为零端切向的三次缓动
/// （中点恰为线性中点，单测锁定）。
pub fn sample_catmull_rom(points: &[Vec3], t: f32) -> Vec3 {
    let n = points.len();
    if n == 0 {
        return Vec3::ZERO;
    }
    if n == 1 {
        return points[0];
    }
    let seg = t.clamp(0.0, (n - 1) as f32);
    // 端点精确早退：u = 0 / 1 处的多项式求值有 f32 舍入（0.5·(...) 各项
    // 不精确对消），直接返回端点保证「t = i 恰过 points[i]」的准点性质
    if seg <= 0.0 {
        return points[0];
    }
    if seg >= (n - 1) as f32 {
        return points[n - 1];
    }
    let i = (seg.floor() as usize).min(n - 2);
    let u = seg - i as f32;
    // 重复端点夹持：首段缺 p0 取 points[0]，末段缺 p3 取 points[n-1]
    let p0 = if i == 0 { points[0] } else { points[i - 1] };
    let p1 = points[i];
    let p2 = points[i + 1];
    let p3 = if i + 2 >= n {
        points[n - 1]
    } else {
        points[i + 2]
    };
    // 标准 Catmull-Rom（张力 0.5）的三次多项式形式
    0.5 * ((2.0 * p1)
        + (-p0 + p2) * u
        + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * (u * u)
        + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * (u * u * u))
}

/// 相机轨道采样（纯函数）：返回 `(eye, target)`，眼点与注视点分别
/// 过自己的 spline。`t` 越界钳制到 `[0, 末关键帧 time]`；`ease = false`
/// 时关键帧间线性插值。
///
/// 时间 → 段映射按关键帧 `time` 非均匀分段（`i` 满足
/// `keys[i].time <= t <= keys[i+1].time`），段内参数
/// `u = (t - keys[i].time) / span` 归一化后交给
/// [`sample_catmull_rom`]（段参数 `i + u`）——关键帧时刻准点、段内
/// 均匀参数化（C1 连续性在 spline 参数空间，见其文档）。
pub fn sample_camera(track: &CameraTrack, t: f32) -> (Vec3, Vec3) {
    let keys = &track.keys;
    if keys.is_empty() {
        return (Vec3::ZERO, Vec3::ZERO);
    }
    if keys.len() == 1 {
        return (keys[0].eye, keys[0].target);
    }
    let duration = keys[keys.len() - 1].time.max(0.0);
    let t = t.clamp(0.0, duration);
    // 段查找：i 满足 keys[i].time <= t（末关键帧恰落在末段右端点）
    let i = keys
        .partition_point(|key| key.time <= t)
        .saturating_sub(1)
        .min(keys.len() - 2);
    let span = (keys[i + 1].time - keys[i].time).max(1e-9);
    let u = ((t - keys[i].time) / span).clamp(0.0, 1.0);
    let eyes: Vec<Vec3> = keys.iter().map(|key| key.eye).collect();
    let targets: Vec<Vec3> = keys.iter().map(|key| key.target).collect();
    if track.ease {
        (
            sample_catmull_rom(&eyes, i as f32 + u),
            sample_catmull_rom(&targets, i as f32 + u),
        )
    } else {
        (
            eyes[i].lerp(eyes[i + 1], u),
            targets[i].lerp(targets[i + 1], u),
        )
    }
}

/// 时间线事件跨界检测（纯函数）：播放时间从 `prev_time` 原始推进到
/// `new_time`（循环时为取模前），返回本帧命中的事件（按触发时刻
/// 升序；一事件多次跨越则重复出现）。跨界数学复用
/// [`collect_crossings`](iris_render::animation::collect_crossings)
/// ——动画与时间线共享跨界语义的单一来源（选抽公共而非本地复制：
/// 两类轨道的回绕 / multi-fire / 末端语义必须逐位一致，分叉必然漂移）。
pub fn collect_timeline_events(
    events: &[TimelineEvent],
    prev_time: f32,
    new_time: f32,
    duration: f32,
    looping: bool,
) -> Vec<TimelineEvent> {
    let times: Vec<f32> = events.iter().map(|event| event.time).collect();
    collect_crossings(&times, prev_time, new_time, duration, looping)
        .into_iter()
        .map(|i| events[i].clone())
        .collect()
}

/// 对话活动条目（纯函数）：`t ∈ [time, end_time)` 的条目；多条活动时
/// **声明序后者胜**（确定性：同刻并列 / 窗口重叠都按声明顺序取最后）。
pub fn active_dialogue_entry(entries: &[DialogueEntry], t: f32) -> Option<&DialogueEntry> {
    entries
        .iter()
        .rev()
        .find(|entry| entry.time <= t && t < entry.end_time)
}

/// 相机接管快照：start 时的主相机原姿态（恢复语义见模块文档）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum CameraSnapshot {
    /// 无主相机可接管（演出照常播事件 / 对话，相机不动）。
    #[default]
    None,
    /// 实体相机：快照局部 `Transform` 三通道（逐位恢复）。
    Entity {
        entity: Entity,
        position: Vec3,
        rotation: Quat,
        scale: Vec3,
    },
    /// 资源相机：快照 `eye / target`（逐位恢复）。
    Resource { eye: Vec3, target: Vec3 },
}

/// 时间线播放器（World 资源，形态选择理由见模块文档）。
///
/// 直接改字段的用法（游戏侧自行编排暂停 / 循环）与
/// [`start_timeline`] / [`stop_timeline`] 的托管用法并存——托管入口
/// 负责快照 / 恢复，手改字段不触发快照（诚实边界：绕过入口后恢复
/// 语义由调用方自理）。
#[derive(Clone, Debug, iris_core::Resource)]
pub struct TimelinePlayer {
    /// 待播放的时间线（None = 空闲；播完后保留以便重播 /
    /// 读取，`start_timeline` 时整体替换）。
    pub timeline: Option<Timeline>,
    /// 播放时间（秒，非循环钳制在 `[0, duration]`、循环取模）。
    pub time: f32,
    /// 是否播放中（播完 / stop 后为 false；空闲资源默认 false）。
    pub playing: bool,
    /// 到达末尾后是否循环（循环时事件 multi-fire、相机轨道回绕重播；
    /// 默认 false。托管入口 `start_timeline` 每次重置为 false，
    /// 循环播放请置位本字段后由 `update_timeline` 维持）。
    pub looped: bool,
    /// 上一帧的播放时间（事件跨界检测基，跨界区间 `(previous_time, time]`）。
    pub previous_time: f32,
    /// 播放期间是否拉黑边条（上下两条全宽黑边，见模块文档）。
    pub letterbox: bool,
    /// 单条黑边高度占视口高的比例（0..1；2.35:1 电影黑边约 0.12）。
    pub letterbox_ratio: f32,
    /// 相机接管快照（私有：快照 / 恢复只经托管入口，保证成对出现）。
    snapshot: CameraSnapshot,
}

impl Default for TimelinePlayer {
    /// 空闲播放器：不循环、无黑边、比例 0.12（2.35:1 观感基线）。
    fn default() -> Self {
        Self {
            timeline: None,
            time: 0.0,
            playing: false,
            looped: false,
            previous_time: 0.0,
            letterbox: false,
            letterbox_ratio: 0.12,
            snapshot: CameraSnapshot::None,
        }
    }
}

/// 启动演出（托管入口）：快照主相机 → 装载时间线 → 从 0 开始播放。
///
/// - 已有演出在播：先恢复旧快照、隐藏对话 / 黑边（演出不嵌套——
///   新 start 整体替换），再快照**恢复后**的姿态；
/// - `looped` 每次重置为 false（循环播放请在启动后置位
///   [`TimelinePlayer::looped`]）；
/// - 主相机解析：查询序第一个挂 `Transform` 的 `ActiveCamera` 实体，
///   否则 `Camera` 资源，都没有则 [`CameraSnapshot::None`]（演出照常，
///   相机不动）。
pub fn start_timeline(world: &mut World, timeline: Timeline) {
    let mut player = world
        .remove_resource::<TimelinePlayer>()
        .unwrap_or_default();
    // 先还旧债（旧演出可能占着相机 / UI），再借新债（快照恢复后姿态）
    if player.playing {
        restore_camera(world, player.snapshot);
        hide_timeline_ui(world);
    }
    player.snapshot = snapshot_camera(world);
    player.timeline = Some(timeline);
    player.time = 0.0;
    player.previous_time = 0.0;
    player.playing = true;
    player.looped = false;
    world.insert_resource(player);
}

/// 停止演出（托管入口）：恢复相机快照、隐藏对话 / 黑边、停表。
/// 时间线保留在播放器上（可读取 / 手动重播）；空闲时调用为 no-op。
pub fn stop_timeline(world: &mut World) {
    let Some(mut player) = world.remove_resource::<TimelinePlayer>() else {
        return;
    };
    if !player.playing {
        world.insert_resource(player);
        return;
    }
    restore_camera(world, player.snapshot);
    hide_timeline_ui(world);
    player.playing = false;
    world.insert_resource(player);
}

/// 是否有演出正在播放（无播放器资源 = false）。
pub fn is_playing(world: &World) -> bool {
    world
        .try_resource::<TimelinePlayer>()
        .map(|player| player.playing)
        .unwrap_or(false)
}

/// 每帧时间线系统（Update 阶段独占，编排理由见模块文档）：
/// 推进时间 → 相机轨道写主相机 → 对话写 UI → 黑边显隐 → 事件产出。
///
/// 播放器资源缺席（自建 App 未加 [`crate::plugin::TimelinePlugin`]）
/// 或未在播放时为 no-op；`Time` 资源缺席时 dt 视为 0（可手动设置
/// `player.time` 定位到指定时刻，照 `update_animations` 同款约定）。
pub fn update_timeline(world: &mut World) {
    let Some(mut player) = world.remove_resource::<TimelinePlayer>() else {
        return;
    };
    if !player.playing {
        world.insert_resource(player);
        return;
    }
    let dt = world
        .try_resource::<Time>()
        .map(|time| time.delta_secs())
        .unwrap_or(0.0);
    let Some(timeline) = player.timeline.clone() else {
        // 无时间线却标记播放：视为空闲（防御性，不 panic 不报废）
        player.playing = false;
        world.insert_resource(player);
        return;
    };
    let duration = timeline.duration;
    if duration <= 0.0 {
        // 空时间线：启动即播完（恢复语义与正常播完一致）
        restore_camera(world, player.snapshot);
        hide_timeline_ui(world);
        player.playing = false;
        world.insert_resource(player);
        return;
    }
    // 推进：循环取模 / 非循环钳制；事件检测用取模前的原始时间
    let prev = player.time;
    player.previous_time = prev;
    let raw = prev + dt;
    let new_time = if player.looped {
        raw % duration
    } else {
        raw.min(duration)
    };
    let finished = !player.looped && raw >= duration;
    player.time = new_time;

    // 相机轨道：覆写主相机（演出期间时间线拥有主相机）
    if let Some(track) = timeline.camera.as_ref() {
        drive_camera(world, track, new_time);
    }

    // 对话 UI：活动窗口内写文本 + 显示，无条目隐藏（底部让开黑边）
    let bottom_bar_height = if player.letterbox {
        player.letterbox_ratio.clamp(0.0, 1.0) * viewport_size(world)[1]
    } else {
        0.0
    };
    update_dialogue_ui(
        world,
        active_dialogue_entry(&timeline.dialogue, new_time),
        bottom_bar_height,
    );
    // 黑边条：播放期间显隐 + 随视口尺寸重排
    update_letterbox_ui(world, &player);

    if finished {
        // 播完：恢复相机快照、隐藏演出 UI、停表（timeline 保留）
        restore_camera(world, player.snapshot);
        hide_timeline_ui(world);
        player.playing = false;
    }
    let looped = player.looped;
    world.insert_resource(player);

    // 事件产出（本帧播放状态落账后发出；消费方 drain 见模块文档）。
    // 循环时传取模前的 raw（multi-fire 检测，与动画 collect_events 同款）
    for event in collect_timeline_events(&timeline.events, prev, raw, duration, looped) {
        world.send_event(event);
    }
}

/// 相机注视旋转（纯函数）：局部 **-Z 指向 eye → target**、以 +Y 为上
/// 参考（与渲染器 `view_from_global` 的相机朝向口径一致）。直视正上 /
/// 正下（前向与 +Y 平行）时改以 +Z 为上参考正交化（不退化 NaN——
/// 竖直俯仰的极端机位也能得到合法旋转，唯一不留神的是滚转自由度）。
fn look_at_quat(eye: Vec3, target: Vec3) -> Quat {
    let forward = (target - eye).normalize_or_zero();
    if forward.length_squared() < 0.5 {
        // 前向退化（eye == target）：恒等兜底，不产 NaN
        return Quat::IDENTITY;
    }
    let up_ref = if forward.dot(Vec3::Y).abs() > 0.999 {
        Vec3::Z
    } else {
        Vec3::Y
    };
    // gluLookAt 的 RH 正交基：x = f × up、y = x × f、z = -f
    let x = forward.cross(up_ref).normalize_or_zero();
    let y = x.cross(forward);
    Quat::from_mat3(&Mat3::from_cols(x, y, -forward))
}

/// 主相机解析（与渲染器优先级一致）：查询序第一个挂 `Transform` 的
/// `ActiveCamera` 实体；无则 None（资源相机由调用方另行回落）。
fn first_camera_entity(world: &World) -> Option<Entity> {
    let mut found = None;
    world.query::<(Entity, &iris_render::camera::ActiveCamera)>(|(entity, _)| {
        if found.is_none() && world.get::<Transform>(entity).is_some() {
            found = Some(entity);
        }
    });
    found
}

/// 快照主相机原姿态（start 时调用，语义见 [`start_timeline`]）。
fn snapshot_camera(world: &World) -> CameraSnapshot {
    if let Some(entity) = first_camera_entity(world) {
        if let Some(transform) = world.get::<Transform>(entity) {
            return CameraSnapshot::Entity {
                entity,
                position: transform.position,
                rotation: transform.rotation,
                scale: transform.scale,
            };
        }
    }
    if let Some(camera) = world.try_resource::<iris_render::camera::Camera>() {
        return CameraSnapshot::Resource {
            eye: camera.eye,
            target: camera.target,
        };
    }
    CameraSnapshot::None
}

/// 逐位恢复相机快照（f32 值拷贝无舍入；快照实体已 despawn 时静默跳过
/// ——悬垂恢复没有意义，与代数校验的缺席语义一致）。
fn restore_camera(world: &World, snapshot: CameraSnapshot) {
    match snapshot {
        CameraSnapshot::Entity {
            entity,
            position,
            rotation,
            scale,
        } => {
            if let Some(mut transform) = world.get_mut::<Transform>(entity) {
                transform.position = position;
                transform.rotation = rotation;
                transform.scale = scale;
            }
        }
        CameraSnapshot::Resource { eye, target } => {
            if let Some(mut camera) = world.try_resource_mut::<iris_render::camera::Camera>() {
                camera.eye = eye;
                camera.target = target;
            }
        }
        CameraSnapshot::None => {}
    }
}

/// 播放期间覆写主相机：优先实体相机（写 `Transform.position` + 注视
/// 旋转），回落 `Camera` 资源（写 `eye / target`）。都缺席则不动。
fn drive_camera(world: &World, track: &CameraTrack, t: f32) {
    let (eye, target) = sample_camera(track, t);
    if let Some(entity) = first_camera_entity(world) {
        if let Some(mut transform) = world.get_mut::<Transform>(entity) {
            transform.position = eye;
            transform.rotation = look_at_quat(eye, target);
        }
        return;
    }
    if let Some(mut camera) = world.try_resource_mut::<iris_render::camera::Camera>() {
        camera.eye = eye;
        camera.target = target;
    }
}

/// 视口尺寸（`Viewport` 资源缺席兜底 1280×720）。
fn viewport_size(world: &World) -> [f32; 2] {
    world
        .try_resource::<crate::plugin::Viewport>()
        .map(|viewport| [viewport.width as f32, viewport.height as f32])
        .unwrap_or(DEFAULT_VIEWPORT)
}

/// 对话 UI 写入：有条目则写文本 + 定位 + 显示，无条目隐藏。
/// 实体缺席惰性生成（见模块文档「UI 手段」）；`bottom_bar_height` 为
/// 下黑边高度（无黑边传 0），文本定位在其上方留白处。
fn update_dialogue_ui(world: &mut World, entry: Option<&DialogueEntry>, bottom_bar_height: f32) {
    let entity = match world.entity_named(DIALOGUE_NAME) {
        Some(entity) => entity,
        None => world.spawn((
            Name(DIALOGUE_NAME.into()),
            UiRect {
                position: [0.0, 0.0],
                size: [0.0, 0.0],
            },
            UiText {
                string: String::new(),
                scale: DIALOGUE_SCALE,
            },
            UiColor(DIALOGUE_COLOR),
            Visibility::HIDDEN,
        )),
    };
    let Some(entry) = entry else {
        if let Some(mut visibility) = world.get_mut::<Visibility>(entity) {
            visibility.visible = false;
        }
        return;
    };
    let (viewport_w, viewport_h) = {
        let [w, h] = viewport_size(world);
        (w, h)
    };
    if let Some(mut text) = world.get_mut::<UiText>(entity) {
        text.string = entry.text.clone();
    }
    // 水平居中（按等宽步进测宽，见 text_width）+ 下黑边上方
    let width = world
        .get::<UiText>(entity)
        .map(|text| text_width(&text))
        .unwrap_or(0.0);
    if let Some(mut rect) = world.get_mut::<UiRect>(entity) {
        rect.position = [
            ((viewport_w - width) * 0.5).max(0.0),
            viewport_h - bottom_bar_height - DIALOGUE_BOTTOM_MARGIN,
        ];
    }
    if let Some(mut visibility) = world.get_mut::<Visibility>(entity) {
        visibility.visible = true;
    }
}

/// 黑边 UI：播放中且 `letterbox` 开启 → 上下两条全宽黑边随视口重排并
/// 显示；否则隐藏（实体保留，显隐切换而非重建——复用 UI 矩形管线，
/// 实现选型见模块文档）。
fn update_letterbox_ui(world: &mut World, player: &TimelinePlayer) {
    let show = player.playing && player.letterbox;
    let [viewport_w, viewport_h] = viewport_size(world);
    let bar_height = player.letterbox_ratio.clamp(0.0, 1.0) * viewport_h;
    for (name, y) in [
        (LETTERBOX_TOP_NAME, 0.0),
        (LETTERBOX_BOTTOM_NAME, viewport_h - bar_height),
    ] {
        let entity = match world.entity_named(name) {
            Some(entity) => entity,
            None => world.spawn((
                Name(name.into()),
                UiRect {
                    position: [0.0, 0.0],
                    size: [viewport_w, bar_height],
                },
                UiColor(LETTERBOX_COLOR),
                Visibility::HIDDEN,
            )),
        };
        if show {
            if let Some(mut rect) = world.get_mut::<UiRect>(entity) {
                rect.position = [0.0, y];
                rect.size = [viewport_w, bar_height];
            }
        }
        if let Some(mut visibility) = world.get_mut::<Visibility>(entity) {
            visibility.visible = show;
        }
    }
}

/// 隐藏全部演出 UI（stop / 播完时调用；实体缺席静默跳过）。
fn hide_timeline_ui(world: &mut World) {
    for name in [DIALOGUE_NAME, LETTERBOX_TOP_NAME, LETTERBOX_BOTTOM_NAME] {
        if let Some(entity) = world.entity_named(name) {
            if let Some(mut visibility) = world.get_mut::<Visibility>(entity) {
                visibility.visible = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core::prelude::World;
    use iris_render::camera::{ActiveCamera, Camera};
    use std::time::Duration;

    /// 逐位比较 Vec3（f32 to_bits）。
    fn assert_bits_eq(a: Vec3, b: Vec3, message: &str) {
        assert_eq!(
            (a.x.to_bits(), a.y.to_bits(), a.z.to_bits()),
            (b.x.to_bits(), b.y.to_bits(), b.z.to_bits()),
            "{message}"
        );
    }

    /// 推进 Time 并跑一帧时间线系统。
    fn step(world: &mut World, dt: f64) {
        world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(dt));
        update_timeline(world);
    }

    fn key(time: f32, eye: [f32; 3], target: [f32; 3]) -> CameraKey {
        CameraKey {
            time,
            eye: eye.into(),
            target: target.into(),
        }
    }

    /// 相机轨道：t=0 广角 → t=6 中段 → t=12 特写（ease 开）。
    fn track() -> CameraTrack {
        CameraTrack {
            keys: vec![
                key(0.0, [0.0, 6.0, 18.0], [0.0, 1.0, 0.0]),
                key(6.0, [3.0, 2.0, 9.0], [1.0, 1.2, 0.0]),
                key(12.0, [0.0, 1.6, 4.5], [0.0, 1.2, 0.0]),
            ],
            ease: true,
        }
    }

    // ---- Catmull-Rom 采样 ----

    #[test]
    fn catmull_rom_passes_through_keys_exactly() {
        let points = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 2.0, 0.0),
            Vec3::new(3.0, 2.0, 4.0),
            Vec3::new(4.0, 0.0, 1.0),
        ];
        for (i, p) in points.iter().enumerate() {
            assert_eq!(
                sample_catmull_rom(&points, i as f32),
                *p,
                "段参数 {i} 应精确过第 {i} 个控制点"
            );
        }
    }

    /// C1 连续性：内点两侧的单边差分导数一致（均匀参数化下
    /// Catmull-Rom 的解析切向 0.5·(p[i+1] − p[i−1]) 两侧相同）。
    #[test]
    fn catmull_rom_is_c1_across_segments() {
        let points = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 3.0, -2.0),
            Vec3::new(4.0, 1.0, 2.0),
            Vec3::new(2.0, -1.0, 5.0),
            Vec3::new(6.0, 2.0, 1.0),
        ];
        let eps = 1e-3;
        for k in 1..points.len() as i32 - 1 {
            let k = k as f32;
            let left =
                (sample_catmull_rom(&points, k) - sample_catmull_rom(&points, k - eps)) / eps;
            let right =
                (sample_catmull_rom(&points, k + eps) - sample_catmull_rom(&points, k)) / eps;
            assert!(
                (left - right).length() < 5e-2,
                "内点 {k} 处两侧差分导数应一致（解析切向 0.5·(p[k+1]−p[k−1]) 两侧相同，\
                 差分误差 O(eps)·|tangent|）：{left:?} vs {right:?}"
            );
        }
    }

    #[test]
    fn catmull_rom_clamps_outside_and_degenerates() {
        let points = [Vec3::new(1.0, 2.0, 3.0), Vec3::new(4.0, 5.0, 6.0)];
        assert_eq!(
            sample_catmull_rom(&points, -1.0),
            points[0],
            "越界下端应钳制到首点"
        );
        assert_eq!(
            sample_catmull_rom(&points, 9.0),
            points[1],
            "越界上端应钳制到末点"
        );
        // 单点退化：恒返回该点
        let single = [Vec3::new(7.0, 8.0, 9.0)];
        assert_eq!(sample_catmull_rom(&single, 0.0), single[0]);
        assert_eq!(sample_catmull_rom(&single, 5.0), single[0]);
        // 两点：中点恰为线性中点（零端切向缓动的对称性）
        let mid = sample_catmull_rom(&points, 0.5);
        assert!(
            (mid - points[0].midpoint(points[1])).length() < 1e-6,
            "两点段中点应为线性中点，实际 {mid:?}"
        );
        assert_eq!(sample_catmull_rom(&[], 1.0), Vec3::ZERO, "空表按零点兜底");
    }

    // ---- 相机轨道采样 ----

    #[test]
    fn sample_camera_linear_and_key_hits() {
        // 线性（ease = false）：段内均匀
        let linear = CameraTrack {
            keys: vec![
                key(0.0, [0.0, 0.0, 0.0], [0.0, 0.0, 10.0]),
                key(4.0, [8.0, 4.0, 0.0], [0.0, 2.0, 10.0]),
            ],
            ease: false,
        };
        let (eye, target) = sample_camera(&linear, 2.0);
        assert_eq!(eye, Vec3::new(4.0, 2.0, 0.0), "线性中点应精确");
        assert_eq!(target, Vec3::new(0.0, 1.0, 10.0), "注视点独立插值");

        // 平滑（ease = true）：关键帧准点 + 越界钳制
        let t = track();
        assert_eq!(sample_camera(&t, 0.0).0, Vec3::new(0.0, 6.0, 18.0));
        assert_eq!(sample_camera(&t, 6.0).0, Vec3::new(3.0, 2.0, 9.0));
        assert_eq!(sample_camera(&t, 12.0).0, Vec3::new(0.0, 1.6, 4.5));
        assert_eq!(
            sample_camera(&t, -3.0).0,
            Vec3::new(0.0, 6.0, 18.0),
            "t < 0 应钳制到首关键帧"
        );
        assert_eq!(
            sample_camera(&t, 99.0).0,
            Vec3::new(0.0, 1.6, 4.5),
            "t 超末帧应钳制到末关键帧"
        );
        // 单关键帧 / 空轨道：恒定不 panic（坏数据不报废演出）
        let single = CameraTrack {
            keys: vec![key(5.0, [1.0, 1.0, 1.0], [0.0, 0.0, 0.0])],
            ease: true,
        };
        assert_eq!(sample_camera(&single, 0.0).0, Vec3::new(1.0, 1.0, 1.0));
        assert_eq!(
            sample_camera(&CameraTrack::default(), 1.0),
            (Vec3::ZERO, Vec3::ZERO)
        );
    }

    // ---- 事件跨界（接线与排序；回绕 / multi-fire 全覆盖在动画侧）----

    #[test]
    fn timeline_events_wire_shared_math_and_sort() {
        let events = |times: &[(&str, f32)]| -> Vec<TimelineEvent> {
            times
                .iter()
                .map(|&(name, time)| TimelineEvent {
                    time,
                    name: name.into(),
                })
                .collect()
        };
        // 非循环：区间 (prev, new] 命中、输出按触发时刻升序（事件表乱序）
        let evs = events(&[("c", 3.0), ("a", 1.0), ("b", 2.0)]);
        assert_eq!(
            collect_timeline_events(&evs, 0.5, 2.5, 10.0, false)
                .iter()
                .map(|event| event.name.clone())
                .collect::<Vec<_>>(),
            vec!["a".to_string(), "b".to_string()],
            "命中按触发时刻升序，且携带各自的触发时刻"
        );
        assert_eq!(
            collect_timeline_events(&evs, 2.5, 3.5, 10.0, false)[0].time,
            3.0,
            "命中事件应携带自己的 time 字段"
        );
        // 末端不重复：停在末端后 prev == new，区间为空
        assert!(collect_timeline_events(&evs, 3.0, 3.0, 10.0, false).is_empty());
        // 循环回绕：事件时刻 10.0 ≡ 0，跨末端触发
        let wrap = events(&[("wrap", 10.0)]);
        assert_eq!(
            collect_timeline_events(&wrap, 9.5, 10.5, 10.0, true)
                .iter()
                .map(|event| event.name.clone())
                .collect::<Vec<_>>(),
            vec!["wrap".to_string()],
            "循环回绕应命中"
        );
        // duration <= 0 恒空
        assert!(collect_timeline_events(&evs, 0.0, 5.0, 0.0, false).is_empty());
    }

    // ---- 对话活动窗口 ----

    #[test]
    fn dialogue_window_and_deterministic_tiebreak() {
        let entries = |pairs: &[(f32, f32, &str)]| -> Vec<DialogueEntry> {
            pairs
                .iter()
                .map(|&(time, end_time, text)| DialogueEntry {
                    time,
                    end_time,
                    text: text.into(),
                })
                .collect()
        };
        let d = entries(&[(2.0, 4.0, "A"), (3.0, 5.0, "B")]);
        assert!(active_dialogue_entry(&d, 1.9).is_none(), "开始前应无条目");
        assert_eq!(active_dialogue_entry(&d, 2.0).unwrap().text, "A", "左端含");
        assert_eq!(active_dialogue_entry(&d, 2.5).unwrap().text, "A");
        // 两条同时活跃：声明序后者胜（确定性）
        assert_eq!(active_dialogue_entry(&d, 3.5).unwrap().text, "B");
        // 前条已结束：右开区间（end_time == t 不显示）
        assert_eq!(active_dialogue_entry(&d, 4.5).unwrap().text, "B");
        assert!(active_dialogue_entry(&d, 5.0).is_none(), "右端不含");
    }

    // ---- 接管与恢复 ----

    /// 实体相机逐位断言（位置 / 旋转 / 缩放）。
    fn assert_entity_camera_bits(
        world: &World,
        entity: Entity,
        position: Vec3,
        rotation: Quat,
        scale: Vec3,
    ) {
        let t = world.get::<Transform>(entity).unwrap();
        assert_bits_eq(t.position, position, "实体相机位置应逐位恢复");
        assert_eq!(
            (
                t.rotation.x.to_bits(),
                t.rotation.y.to_bits(),
                t.rotation.z.to_bits(),
                t.rotation.w.to_bits()
            ),
            (
                rotation.x.to_bits(),
                rotation.y.to_bits(),
                rotation.z.to_bits(),
                rotation.w.to_bits()
            ),
            "实体相机旋转应逐位恢复"
        );
        assert_bits_eq(t.scale, scale, "实体相机缩放应逐位恢复");
    }

    #[test]
    fn entity_camera_takeover_and_restore_bitwise() {
        let mut world = World::new();
        world.insert_resource(Time::default());
        let original_pos = Vec3::new(1.0, 2.0, 3.0);
        let original_rot = Quat::from_rotation_y(0.5);
        let original_scale = Vec3::new(1.0, 1.0, 1.0);
        let cam = world.spawn((
            Transform {
                position: original_pos,
                rotation: original_rot,
                scale: original_scale,
            },
            ActiveCamera::default(),
        ));
        let timeline = Timeline {
            duration: 1.0,
            camera: Some(track()),
            events: vec![TimelineEvent {
                time: 0.5,
                name: "ping".into(),
            }],
            dialogue: Vec::new(),
        };

        // 播放到完成：相机逐位恢复
        start_timeline(&mut world, timeline.clone());
        assert!(is_playing(&world));
        for _ in 0..12 {
            step(&mut world, 0.1);
        }
        {
            let player = world.resource::<TimelinePlayer>();
            assert!(!player.playing, "非循环应播完停表");
            assert_eq!(player.time, 1.0, "非循环应钳在末端");
        }
        assert_entity_camera_bits(&world, cam, original_pos, original_rot, original_scale);

        // 中途 stop：同样逐位恢复
        start_timeline(&mut world, timeline);
        for _ in 0..4 {
            step(&mut world, 0.1);
        }
        assert!(is_playing(&world));
        stop_timeline(&mut world);
        assert!(!is_playing(&world));
        assert_entity_camera_bits(&world, cam, original_pos, original_rot, original_scale);
    }

    #[test]
    fn resource_camera_takeover_and_restore_bitwise() {
        let mut world = World::new();
        world.insert_resource(Time::default());
        // 无 ActiveCamera 实体 → 回落资源相机
        let original_eye = Vec3::new(0.0, 1.8, 4.5);
        let original_target = Vec3::new(1.0, 0.5, 0.0);
        world.insert_resource(Camera {
            eye: original_eye,
            target: original_target,
            ..Default::default()
        });
        start_timeline(
            &mut world,
            Timeline {
                duration: 1.0,
                camera: Some(track()),
                ..Default::default()
            },
        );
        step(&mut world, 0.1);
        let eye = world.try_resource::<Camera>().map(|c| c.eye).unwrap();
        assert_ne!(eye, original_eye, "播放中资源相机应被轨道覆写");
        // 中途 stop：eye / target 逐位恢复
        stop_timeline(&mut world);
        let camera = world.resource::<Camera>();
        assert_bits_eq(camera.eye, original_eye, "资源相机 eye 应逐位恢复");
        assert_bits_eq(camera.target, original_target, "资源相机 target 应逐位恢复");
    }

    // ---- 播放中的相机写入 ----

    #[test]
    fn camera_track_drives_active_camera_entity_each_frame() {
        let mut world = World::new();
        world.insert_resource(Time::default());
        let cam = world.spawn((Transform::default(), ActiveCamera::default()));
        start_timeline(
            &mut world,
            Timeline {
                duration: 12.0,
                camera: Some(track()),
                ..Default::default()
            },
        );
        for _ in 0..20 {
            step(&mut world, 0.3);
        }
        let time = world.resource::<TimelinePlayer>().time;
        let (eye, target) = sample_camera(&track(), time);
        let t = world.get::<Transform>(cam).unwrap();
        assert!(
            (t.position - eye).length() < 1e-5,
            "实体相机位置应等于轨道采样值：{:?} vs {eye:?}",
            t.position
        );
        // 旋转口径：局部 -Z 指向 eye → target（与 view_from_global 一致）
        let forward = -glam::Mat3::from_quat(t.rotation).z_axis;
        let want = (target - eye).normalize();
        assert!(
            forward.dot(want) > 1.0 - 1e-5,
            "相机前向应指向注视点：{forward:?} vs {want:?}"
        );
        // 注视点应投影到视图中心（-Z 半空间）
        let view = iris_render::camera::view_from_global(
            &iris_core::prelude::GlobalTransform::from_transform(*t),
        );
        let clip = view * target.extend(1.0);
        assert!(
            clip.x.abs() < 1e-4 && clip.y.abs() < 1e-4 && clip.z < 0.0,
            "注视点应落在视图正前方：{clip:?}"
        );
    }

    #[test]
    fn no_camera_track_leaves_camera_bitwise_untouched() {
        let mut world = World::new();
        world.insert_resource(Time::default());
        let pos = Vec3::new(2.0, 3.0, 4.0);
        let cam = world.spawn((
            Transform::from_xyz(pos.x, pos.y, pos.z),
            ActiveCamera::default(),
        ));
        world.insert_resource(Camera {
            eye: Vec3::new(9.0, 9.0, 9.0),
            target: Vec3::ZERO,
            ..Default::default()
        });
        // 纯事件演出（camera: None）：两种相机都逐位不动
        start_timeline(
            &mut world,
            Timeline {
                duration: 1.0,
                events: vec![TimelineEvent {
                    time: 0.2,
                    name: "boom".into(),
                }],
                ..Default::default()
            },
        );
        step(&mut world, 0.3);
        step(&mut world, 0.3);
        assert_entity_camera_bits(&world, cam, pos, Quat::IDENTITY, Vec3::ONE);
        let camera = world.resource::<Camera>();
        assert_bits_eq(
            camera.eye,
            Vec3::new(9.0, 9.0, 9.0),
            "资源相机不应被无轨道时间线触碰",
        );
    }

    // ---- 对话 / 黑边 UI ----

    #[test]
    fn dialogue_ui_shows_within_window_and_hides_outside() {
        let mut world = World::new();
        world.insert_resource(Time::default());
        start_timeline(
            &mut world,
            Timeline {
                duration: 6.0,
                dialogue: vec![DialogueEntry {
                    time: 0.4,
                    end_time: 2.0,
                    text: "Hold the line!".into(),
                }],
                ..Default::default()
            },
        );
        // 窗口前：隐藏（步长 ≤ Time::max_delta 0.25，见 step 的钳制语义）
        step(&mut world, 0.1);
        let entity = world
            .entity_named(DIALOGUE_NAME)
            .expect("对话框实体应已惰性生成");
        assert!(
            !world.get::<Visibility>(entity).unwrap().visible,
            "窗口前应隐藏"
        );
        // 窗口内（t = 0.45）：文本写入 + 可见
        step(&mut world, 0.15);
        step(&mut world, 0.2);
        assert_eq!(
            world.get::<UiText>(entity).unwrap().string,
            "Hold the line!"
        );
        assert!(
            world.get::<Visibility>(entity).unwrap().visible,
            "窗口内应显示"
        );
        // 窗口后（t = 0.45 + 9×0.25 = 2.7 > end_time 2.0）：隐藏
        for _ in 0..9 {
            step(&mut world, 0.25);
        }
        assert!(
            !world.get::<Visibility>(entity).unwrap().visible,
            "窗口外应隐藏"
        );
        // 播完（duration 6.0）：保持隐藏
        for _ in 0..20 {
            step(&mut world, 0.25);
        }
        assert!(!is_playing(&world));
        assert!(!world.get::<Visibility>(entity).unwrap().visible);
    }

    #[test]
    fn letterbox_bars_follow_flag_and_viewport() {
        let mut world = World::new();
        world.insert_resource(Time::default());
        world.insert_resource(crate::plugin::Viewport::new(800, 600));
        let timeline = Timeline {
            duration: 1.0,
            ..Default::default()
        };
        start_timeline(&mut world, timeline);
        world.resource_mut::<TimelinePlayer>().letterbox = true;
        step(&mut world, 0.1);
        let top = world
            .entity_named(LETTERBOX_TOP_NAME)
            .expect("上黑边应已惰性生成");
        let bottom = world
            .entity_named(LETTERBOX_BOTTOM_NAME)
            .expect("下黑边应已惰性生成");
        let height = 0.12 * 600.0;
        for (entity, y) in [(top, 0.0), (bottom, 600.0 - height)] {
            assert!(
                world.get::<Visibility>(entity).unwrap().visible,
                "黑边应显示"
            );
            let rect = world.get::<UiRect>(entity).unwrap();
            assert_eq!(rect.size, [800.0, height], "黑边应全宽 × 配置高度");
            assert_eq!(rect.position, [0.0, y], "黑边应贴上下缘");
            assert_eq!(world.get::<UiColor>(entity).unwrap().0, LETTERBOX_COLOR);
        }
        // 关闭 letterbox：下一帧隐藏
        world.resource_mut::<TimelinePlayer>().letterbox = false;
        step(&mut world, 0.1);
        assert!(
            !world.get::<Visibility>(top).unwrap().visible,
            "关闭后上黑边应隐藏"
        );
        assert!(
            !world.get::<Visibility>(bottom).unwrap().visible,
            "关闭后下黑边应隐藏"
        );
        // stop：隐藏（实体保留）
        stop_timeline(&mut world);
        assert!(!world.get::<Visibility>(top).unwrap().visible);
    }

    // ---- 事件产出与消费契约 ----

    #[test]
    fn events_are_sent_and_drainable() {
        let mut world = World::new();
        world.insert_resource(Time::default());
        world.register_event::<TimelineEvent>();
        start_timeline(
            &mut world,
            Timeline {
                duration: 3.0,
                events: vec![TimelineEvent {
                    time: 1.0,
                    name: "door_open".into(),
                }],
                ..Default::default()
            },
        );
        // 未跨过的帧：无事件（t = 0.8，事件在 1.0）
        for _ in 0..4 {
            step(&mut world, 0.2);
        }
        assert!(
            world.events::<TimelineEvent>().is_empty(),
            "未跨过事件时刻不应产出"
        );
        // 跨过 t=1.0 的帧（t = 1.0 恰入右闭区间）：产出且可 drain 消费
        step(&mut world, 0.2);
        let drained: Vec<TimelineEvent> = world.events_mut::<TimelineEvent>().drain().collect();
        assert_eq!(drained.len(), 1, "跨越后应恰好产出一次");
        assert_eq!(drained[0].name, "door_open");
        // drain 后不可见（消费契约）
        assert!(world.events::<TimelineEvent>().is_empty());
    }

    // ---- 确定性 ----

    #[test]
    fn same_timeline_plays_bitwise_identically() {
        let build = || {
            let mut world = World::new();
            world.insert_resource(Time::default());
            world.spawn((
                Name("test_camera".into()),
                Transform::default(),
                ActiveCamera::default(),
            ));
            start_timeline(
                &mut world,
                Timeline {
                    duration: 2.0,
                    camera: Some(track()),
                    events: vec![TimelineEvent {
                        time: 0.7,
                        name: "x".into(),
                    }],
                    dialogue: vec![DialogueEntry {
                        time: 0.5,
                        end_time: 1.5,
                        text: "go".into(),
                    }],
                },
            );
            world
        };
        let mut world_a = build();
        let mut world_b = build();
        for _ in 0..25 {
            step(&mut world_a, 0.1);
            step(&mut world_b, 0.1);
            // 每帧逐位比对：相机实体位置（时间线采样写入）与
            // 对话文本内容（时间线 UI 写入）
            let ta = *world_a
                .get::<Transform>(world_a.entity_named("test_camera").unwrap())
                .unwrap();
            let tb = *world_b
                .get::<Transform>(world_b.entity_named("test_camera").unwrap())
                .unwrap();
            assert_bits_eq(ta.position, tb.position, "两世界相机位置应逐位一致");
            assert_eq!(
                (
                    ta.rotation.x.to_bits(),
                    ta.rotation.y.to_bits(),
                    ta.rotation.z.to_bits(),
                    ta.rotation.w.to_bits()
                ),
                (
                    tb.rotation.x.to_bits(),
                    tb.rotation.y.to_bits(),
                    tb.rotation.z.to_bits(),
                    tb.rotation.w.to_bits()
                ),
                "两世界相机旋转应逐位一致"
            );
            let sa = world_a
                .get::<UiText>(world_a.entity_named(DIALOGUE_NAME).unwrap())
                .unwrap();
            let sb = world_b
                .get::<UiText>(world_b.entity_named(DIALOGUE_NAME).unwrap())
                .unwrap();
            assert_eq!(sa.string, sb.string, "两世界对话文本应一致");
        }
        // 播放进度亦逐位一致
        let ta = world_a.resource::<TimelinePlayer>().time.to_bits();
        let tb = world_b.resource::<TimelinePlayer>().time.to_bits();
        assert_eq!(ta, tb, "两世界播放进度应逐位一致");
    }
}
