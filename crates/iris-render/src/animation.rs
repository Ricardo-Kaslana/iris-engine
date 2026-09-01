//! glTF 动画运行时：剪辑采样、[`AnimationPlayer`] 组件与每帧推进系统。
//!
//! glTF 动画模型：每条通道绑定「某节点 + 某属性（平移/旋转/缩放）」，
//! 按关键帧时间轴驱动。采样时被动画的属性**覆写**目标实体 `Transform` 的
//! 对应字段，未动画的字段保持当前值（即加载时的静止姿势）。
//!
//! ```ignore
//! let instance = spawn_gltf_instance(&mut world, "assets/model.glb")?;
//! let root = instance.roots[0];
//! world.insert(root, instance.player()); // 挂载后默认循环播放第一个剪辑
//! // DefaultPlugins 已注册 update_animations（在层级传播之前执行）
//! ```

use glam::Quat;
use iris_core::prelude::{Entity, Time, Transform};

/// 动画通道作用的节点属性。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationPath {
    Translation,
    Rotation,
    Scale,
}

/// 关键帧插值方式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnimationInterpolation {
    /// 线性插值；旋转用最短路径 slerp。
    #[default]
    Linear,
    /// 阶跃：保持上一关键帧值直到下一关键帧时刻。
    Step,
}

/// 单条动画通道：某节点的某属性随时间的关键帧序列。
#[derive(Clone, Debug)]
pub struct AnimationChannel {
    /// 目标节点索引（对应 [`crate::asset::GltfInstance::node_entities`] 下标）。
    pub node: usize,
    pub path: AnimationPath,
    pub interpolation: AnimationInterpolation,
    /// 关键帧时间（秒，非递减）。
    pub times: Vec<f32>,
    /// 平铺关键帧值：平移/缩放每帧 3 分量，旋转每帧 4 分量 (x, y, z, w)。
    pub values: Vec<f32>,
}

impl AnimationChannel {
    fn components(&self) -> usize {
        match self.path {
            AnimationPath::Rotation => 4,
            _ => 3,
        }
    }

    /// 把 t 时刻的插值结果写入 out（长度须为分量数）。
    fn sample_into(&self, t: f32, out: &mut [f32]) {
        let comps = self.components();
        let keys = self.times.len();
        if keys == 0 {
            return;
        }
        let last = keys - 1;
        let copy_key = |k: usize, out: &mut [f32]| {
            out[..comps].copy_from_slice(&self.values[k * comps..k * comps + comps]);
        };
        if keys == 1 || t <= self.times[0] {
            copy_key(0, out);
            return;
        }
        if t >= self.times[last] {
            copy_key(last, out);
            return;
        }
        // 段查找：i 满足 times[i] <= t < times[i+1]
        let i = self
            .times
            .partition_point(|&tt| tt <= t)
            .saturating_sub(1)
            .min(last - 1);
        if self.interpolation == AnimationInterpolation::Step {
            copy_key(i, out);
            return;
        }
        let span = (self.times[i + 1] - self.times[i]).max(1e-9);
        let f = (t - self.times[i]) / span;
        // 旋转必须 slerp（分量线性插值会破坏归一化且路径错误）；
        // glTF 规范要求点积为负时翻转短路径
        if self.path == AnimationPath::Rotation {
            let qa = Quat::from_slice(&self.values[i * 4..i * 4 + 4]);
            let mut qb = Quat::from_slice(&self.values[(i + 1) * 4..(i + 1) * 4 + 4]);
            if qa.dot(qb) < 0.0 {
                qb = -qb;
            }
            let q = qa.slerp(qb, f);
            out[..4].copy_from_slice(&[q.x, q.y, q.z, q.w]);
            return;
        }
        // comps ≤ out.len()（各属性分量数 3 / 4 / 4）：只覆写前 comps 个槽
        for (c, slot) in out.iter_mut().enumerate().take(comps) {
            let a = self.values[i * comps + c];
            let b = self.values[(i + 1) * comps + c];
            *slot = a + (b - a) * f;
        }
    }

    /// t 时刻的属性值（覆写到目标实体 `Transform` 用）。
    pub fn sample(&self, t: f32) -> AnimatedField {
        let mut v = [0.0f32; 4];
        self.sample_into(t, &mut v);
        match self.path {
            AnimationPath::Translation => AnimatedField::Translation([v[0], v[1], v[2]]),
            AnimationPath::Scale => AnimatedField::Scale([v[0], v[1], v[2]]),
            AnimationPath::Rotation => {
                let a = Quat::from_xyzw(v[0], v[1], v[2], v[3]);
                AnimatedField::Rotation(a)
            }
        }
    }
}

/// 采样结果：要覆写到目标实体 `Transform` 的单个字段。
#[derive(Clone, Copy, Debug)]
pub enum AnimatedField {
    Translation([f32; 3]),
    Rotation(Quat),
    Scale([f32; 3]),
}

/// 一段动画剪辑：若干通道的集合，时长取各通道最后关键帧时间的最大值。
#[derive(Clone, Debug, Default)]
pub struct AnimationClip {
    pub name: String,
    pub channels: Vec<AnimationChannel>,
    /// 剪辑时长（秒）；无通道时为 0。
    pub duration: f32,
}

/// 动画播放器组件：挂在任意实体上（通常是 glTF 根实体），
/// 持有剪辑与「节点索引 → 实体」映射，由 [`update_animations`] 每帧推进。
#[derive(Clone, Debug, iris_core::Component)]
pub struct AnimationPlayer {
    pub clips: Vec<AnimationClip>,
    /// 节点索引 → 目标实体；未在 active scene 中的节点为 None。
    pub node_entities: Vec<Option<Entity>>,
    /// 当前播放的剪辑索引。
    pub active: usize,
    /// 当前播放时间（秒）。
    pub time: f32,
    /// 播放速度倍率（1 = 原速）。
    pub speed: f32,
    pub playing: bool,
    /// 到达剪辑末尾后是否循环。
    pub looping: bool,
}

impl AnimationPlayer {
    /// 默认播放器：循环播放第一个剪辑，原速，自动开始。
    pub fn new(clips: Vec<AnimationClip>, node_entities: Vec<Option<Entity>>) -> Self {
        Self {
            clips,
            node_entities,
            active: 0,
            time: 0.0,
            speed: 1.0,
            playing: true,
            looping: true,
        }
    }

    /// 推进后的时间与播放状态（不修改自身，供系统两阶段处理）。
    pub fn advanced(&self, dt: f32) -> (f32, bool) {
        if !self.playing {
            return (self.time, false);
        }
        let Some(clip) = self.clips.get(self.active) else {
            return (self.time, false);
        };
        if clip.duration <= 0.0 {
            return (self.time, false);
        }
        let t = self.time + dt * self.speed;
        if self.looping {
            (t % clip.duration, true)
        } else if t >= clip.duration {
            (clip.duration, false)
        } else {
            (t, true)
        }
    }
}

/// 单个动画根实体的采样任务包（`update_animations` 阶段一产出、阶段二
/// 消费）：根实体 + 推进后的时间 + 循环标记 + 「目标实体 → 覆写字段」列表。
type AnimationJob = (Entity, f32, bool, Vec<(Entity, AnimatedField)>);

/// 每帧动画系统：推进所有 [`AnimationPlayer`]，把采样结果覆写到目标实体的
/// `Transform`。必须在 `propagate_transforms` 之前执行（DefaultPlugins 已保证）；
/// `Time` 资源缺席时 dt 视为 0（可手动设置 `player.time` 后调用以定位到指定时刻）。
///
/// 只读世界签名（`&World`）：组件写访问走世界内部的尝试型锁，
/// 因此可注册为**并行系统**（访问集：写 AnimationPlayer / Transform，读 Time）。
pub fn update_animations(world: &iris_core::world::World) {
    let dt = world
        .try_resource::<Time>()
        .map(|t| t.delta_secs())
        .unwrap_or(0.0);

    // 阶段一（只读采样）：推进时间并把每条通道的采样值收集为覆写任务
    let mut jobs: Vec<AnimationJob> = Vec::new();
    world.query::<(Entity, &AnimationPlayer)>(|(entity, player)| {
        let (time, playing) = player.advanced(dt);
        let Some(clip) = player.clips.get(player.active) else {
            return;
        };
        let mut fields = Vec::with_capacity(clip.channels.len());
        for channel in &clip.channels {
            let target = player.node_entities.get(channel.node).copied().flatten();
            let Some(target) = target else {
                continue;
            };
            fields.push((target, channel.sample(time)));
        }
        jobs.push((entity, time, playing, fields));
    });

    // 阶段二（写入）：更新播放状态，覆写目标实体的局部变换
    for (player_entity, time, playing, fields) in jobs {
        if let Some(mut player) = world.get_mut::<AnimationPlayer>(player_entity) {
            player.time = time;
            player.playing = playing;
        }
        for (target, field) in fields {
            let Some(mut transform) = world.get_mut::<Transform>(target) else {
                continue;
            };
            match field {
                AnimatedField::Translation(v) => transform.position = v.into(),
                AnimatedField::Rotation(q) => transform.rotation = q,
                AnimatedField::Scale(v) => transform.scale = v.into(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translation_channel(interpolation: AnimationInterpolation) -> AnimationChannel {
        AnimationChannel {
            node: 0,
            path: AnimationPath::Translation,
            interpolation,
            times: vec![0.0, 1.0, 2.0],
            values: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 2.0, 0.0, 0.0],
        }
    }

    fn sample_translation(channel: &AnimationChannel, t: f32) -> [f32; 3] {
        match channel.sample(t) {
            AnimatedField::Translation(v) => v,
            _ => panic!("应为平移通道"),
        }
    }

    #[test]
    fn linear_interpolates_between_keys() {
        let ch = translation_channel(AnimationInterpolation::Linear);
        assert_eq!(sample_translation(&ch, 0.5), [0.5, 0.0, 0.0]);
        assert_eq!(sample_translation(&ch, 1.5), [1.5, 0.0, 0.0]);
    }

    #[test]
    fn sample_clamps_outside_range() {
        let ch = translation_channel(AnimationInterpolation::Linear);
        assert_eq!(sample_translation(&ch, -1.0), [0.0, 0.0, 0.0]);
        assert_eq!(sample_translation(&ch, 9.0), [2.0, 0.0, 0.0]);
    }

    #[test]
    fn step_holds_previous_key() {
        let ch = translation_channel(AnimationInterpolation::Step);
        assert_eq!(sample_translation(&ch, 0.99), [0.0, 0.0, 0.0]);
        assert_eq!(sample_translation(&ch, 1.0), [1.0, 0.0, 0.0]);
    }

    #[test]
    fn single_key_channel_is_constant() {
        let ch = AnimationChannel {
            node: 0,
            path: AnimationPath::Translation,
            interpolation: AnimationInterpolation::Linear,
            times: vec![0.5],
            values: vec![1.0, 2.0, 3.0],
        };
        assert_eq!(sample_translation(&ch, 0.0), [1.0, 2.0, 3.0]);
        assert_eq!(sample_translation(&ch, 10.0), [1.0, 2.0, 3.0]);
    }

    #[test]
    fn rotation_slerp_takes_shortest_path() {
        let ch = AnimationChannel {
            node: 0,
            path: AnimationPath::Rotation,
            interpolation: AnimationInterpolation::Linear,
            times: vec![0.0, 1.0],
            values: vec![
                0.0,
                0.0,
                0.0,
                1.0, // 单位四元数（yaw 0°）
                0.0,
                std::f32::consts::FRAC_1_SQRT_2,
                0.0,
                std::f32::consts::FRAC_1_SQRT_2, // yaw 90°
            ],
        };
        match ch.sample(0.5) {
            AnimatedField::Rotation(q) => {
                let expected = glam::Quat::from_rotation_y(45f32.to_radians());
                // 四元数双覆盖：取与期望夹角的一半判断
                let dot = (q.dot(expected)).abs();
                assert!(
                    (dot - 1.0).abs() < 1e-4,
                    "0.5s 应为 yaw 45°，实际 {q:?} vs {expected:?}"
                );
            }
            _ => panic!("应为旋转通道"),
        }
    }

    #[test]
    fn player_loops_and_stops() {
        let clip = AnimationClip {
            name: "c".into(),
            channels: vec![],
            duration: 1.0,
        };
        let mut player = AnimationPlayer::new(vec![clip], vec![]);
        assert_eq!(player.advanced(0.3), (0.3, true));
        player.time = 0.9;
        let (t, playing) = player.advanced(0.3);
        assert!((t - 0.2).abs() < 1e-5 && playing, "循环应回绕，实际 {t}");
        player.time = t;
        player.looping = false;
        let (t, playing) = player.advanced(0.9);
        assert!(
            (t - 1.0).abs() < 1e-5 && !playing,
            "非循环应停在末尾，实际 {t}"
        );
        player.time = t;
        player.playing = playing;
        assert_eq!(player.advanced(0.3), (1.0, false), "暂停不应推进");
    }
}
