//! 空间音频 v1：听者 / 声源组件与每帧同步系统（混音数学在
//! [`iris_audio::mixer`]，本模块只负责 ECS → 混音器的接线）。
//!
//! - [`SpatialListener`]：标记组件，挂相机 / 听者实体——每帧取查询序
//!   **第一个**，用其 `GlobalTransform` 位姿写混音器听者（前方取实体 -Z
//!   轴、以世界 +Y 为上参考做正交化：翻滚不倾斜声像平面，俯仰俯视的
//!   相机得到水平右轴，声像只随水平方位摆动）。
//! - [`SpatialSource`]：持续声源组件（v1 定位为**循环环境声**），
//!   [`update_spatial_audio`] 负责「开始播放 + 每帧同步位置 + 组件移除
//!   时停止」；一次性空间音效不走组件，游戏代码直接
//!   `audio.play_spatial(clip, pos, vol, false)`（iris-arena 即此用法）。
//!
//! # 组件移除检测 = 帧间差分
//!
//! 「Query 里检测不到即 stop」需要跨帧记忆：上一帧还活跃的
//! `(实体 bits → SoundId)` 快照挂在 [`Audio`](crate::audio::Audio) 资源上
//! （见其字段文档），系统每帧三步：
//! 1. 查询全部 `(Entity, GlobalTransform, &mut SpatialSource)`——组件内
//!    记句柄：未播放过则 `play_spatial` 起播，已播放则 `set_sound_position`
//!    同步位置；
//! 2. 用本帧实体 bits 集合与上一帧快照做差：快照里有、本帧查无（组件被
//!    移除或实体 despawn）→ `stop` 对应句柄；
//! 3. 用本帧列表替换快照。
//!
//! 实体 bits 作键是防悬垂安全的（实体「索引 + 代数」与 `SoundId` 同款：
//! despawn 后同下标新实体的代数必变，绝不会误停新源）。`GlobalTransform`
//! 缺席的源不进查询、不参与差分（视为未挂载）。
//!
//! # 调用契约
//!
//! `update_spatial_audio` 必须在 `propagate_transforms` **之后**执行（听者
//! 与声源都读当帧全局位置——用户在 Update 挪动实体，本帧混音增益即跟随），
//! [`crate::plugin::SpatialPlugin`] 已按此注册在 DefaultPlugins 的
//! PostUpdate（与 [`crate::plugin::ParticlePlugin`] 同层）；自建 App /
//! headless 路径须手动按此顺序调用（与 `HierarchyPlugin` 同款约定）。

use glam::Vec3;

use iris_audio::{ListenerParams, SoundId};

use iris_core::prelude::{Component, Entity, GlobalTransform};

use crate::audio::Audio;

/// 听者标记组件：每世界取查询序第一个挂载 `GlobalTransform` 的实体。
///
/// 标记型（无字段）：位姿完全来自实体 `GlobalTransform`——移动听者就是
/// 挪实体，系统每帧自动跟随。
#[derive(Clone, Copy, Debug, Default, Component)]
pub struct SpatialListener;

/// 持续空间声源组件：开始播放 + 每帧跟随实体位置 + 移除时停止。
///
/// v1 定位为**持续环境声源**（典型 `looping: true` 的循环音）：非循环源
/// 播完自然移除后，系统对死句柄的同步是安全 no-op（世代校验失败即忽略），
/// 不会自动重启——重启语义交给上层（重新插入本组件）。
#[derive(Clone, Debug, Component)]
pub struct SpatialSource {
    /// 声源剪辑（播放时以 `Arc` 共享，组件持有引用计数）。
    pub clip: iris_audio::AudioClip,
    /// 音量（≥ 0；负值与 NaN 在混音侧按静音处理）。
    pub volume: f32,
    /// 是否循环播放（环境声通常为 `true`，见结构体文档）。
    pub looping: bool,
    /// 距离参考（米）：衰减 `att = ref / (ref + d)` 的 ref 项，按声源尺度
    /// 配置（默认 [`iris_audio::DEFAULT_REFERENCE_DISTANCE`]）。
    pub reference_distance: f32,
    /// 内部状态：已起播的防悬垂句柄（`None` = 尚未起播）。
    /// Clone 会复制句柄（两个组件共享同一声音）——克隆后如需独立播放，
    /// 先经 [`SpatialSource::detach`] 清空。
    started: Option<SoundId>,
}

impl SpatialSource {
    /// 构造未起播的声源。
    pub fn new(
        clip: iris_audio::AudioClip,
        volume: f32,
        looping: bool,
        reference_distance: f32,
    ) -> Self {
        Self {
            clip,
            volume,
            looping,
            reference_distance,
            started: None,
        }
    }

    /// 已起播的句柄（测试与调试用；`None` = 尚未被系统起播）。
    pub fn started(&self) -> Option<SoundId> {
        self.started
    }

    /// 清空内部句柄状态（克隆后独立播放用）：下次系统运行时重新起播。
    pub fn detach(&mut self) {
        self.started = None;
    }
}

/// 每帧空间音频同步系统（见模块文档的差分三步与调用契约）。
///
/// `Audio` 资源缺席时静默返回（无音频的世界照常运行）；注册为**并行
/// 系统**：写 `SpatialSource`、读 `SpatialListener` / `GlobalTransform` /
/// `Audio`（播放编排经 `Arc<Mutex>` 混音器完成，`&Audio` 即可，与
/// `run_arena` 等既有系统同款声明）。
pub fn update_spatial_audio(world: &iris_core::world::World) {
    let Some(audio) = world.try_resource::<Audio>() else {
        return;
    };

    // ---- 听者：查询序第一个（SpatialListener, GlobalTransform）----
    // 前方 = 全局 -Z 轴，经 from_view 以世界 +Y 重正交化（见模块文档）。
    let mut listener_done = false;
    world.query::<(&SpatialListener, &GlobalTransform)>(|(_, global)| {
        if listener_done {
            return;
        }
        listener_done = true;
        let matrix = global.matrix();
        let forward = -Vec3::new(matrix.z_axis.x, matrix.z_axis.y, matrix.z_axis.z);
        audio.set_listener(ListenerParams::from_view(
            global.position(),
            forward,
            Vec3::Y,
        ));
    });

    // ---- 声源差分：起播 / 逐帧同步 / 移除停止（三步见模块文档）----
    let mut active: Vec<(u64, SoundId)> = Vec::new();
    world.query::<(Entity, &GlobalTransform, &mut SpatialSource)>(|(entity, global, source)| {
        let position = global.position();
        let id = match source.started {
            Some(id) => {
                // 已起播：只同步位置（死句柄 = 非循环源自然播完，
                // 世代校验安全失败，见 SpatialSource 文档）
                audio.set_sound_position(id, position);
                id
            }
            None => {
                let id = audio.play_spatial(
                    source.clip.clone(),
                    position,
                    source.volume,
                    source.looping,
                );
                audio.set_sound_reference_distance(id, source.reference_distance);
                source.started = Some(id);
                id
            }
        };
        active.push((entity.to_bits(), id));
    });
    // 差分：上一帧活跃、本帧查无 → 组件已移除 / 实体已 despawn → 停止
    for (bits, id) in audio.spatial_active() {
        if !active.iter().any(|(current, _)| current == &bits) {
            audio.stop(id);
        }
    }
    audio.set_spatial_active(active);
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_audio::sine;
    use iris_core::prelude::{Transform, World};

    /// 循环单声道测试剪辑（循环声跨帧存活，差分断言不受自然播完干扰）。
    fn clip() -> iris_audio::AudioClip {
        sine(440.0, 0.05, 48_000)
    }

    /// 建带 headless Audio 的世界。
    fn world_with_audio() -> World {
        let mut world = World::new();
        world.insert_resource(Audio::headless());
        world
    }

    /// 左右声道能量（混一段缓冲后按声道求平方和；headless 混音断言用）。
    fn channel_energies(world: &World) -> (f32, f32) {
        let audio = world.resource::<Audio>();
        let mut out = vec![0.0f32; 4_800];
        audio.mixer().mix(&mut out, 2, 48_000);
        let (mut left, mut right) = (0.0f32, 0.0f32);
        for frame in out.chunks_exact(2) {
            left += frame[0] * frame[0];
            right += frame[1] * frame[1];
        }
        (left, right)
    }

    /// 添加 → 播放：首帧起播（组件记句柄、混音器入表、快照登记），
    /// 次帧不重复起播。
    #[test]
    fn added_source_starts_playing_on_first_update() {
        let mut world = world_with_audio();
        world.spawn((
            Transform::from_xyz(3.0, 0.0, -2.0),
            GlobalTransform::from_transform(Transform::from_xyz(3.0, 0.0, -2.0)),
            SpatialSource::new(clip(), 0.5, true, 4.0),
        ));
        update_spatial_audio(&world);

        {
            let audio = world.resource::<Audio>();
            assert_eq!(audio.mixer().active_count(), 1, "首帧应起播");
            assert_eq!(audio.spatial_active().len(), 1, "快照应登记活跃源");
        }
        let mut handles = Vec::new();
        world.query::<&SpatialSource>(|source| {
            handles.push(source.started.expect("首帧后组件应记有句柄"));
        });
        assert_eq!(handles.len(), 1);
        update_spatial_audio(&world);
        assert_eq!(
            world.resource::<Audio>().mixer().active_count(),
            1,
            "第二帧不得重复起播（同一声音继续同步）"
        );
    }

    /// 位置逐帧同步：源从正右方挪到正左方，下一帧混音的左右声道能量
    /// 随之镜像（headless Audio 槽位状态断言，而非实听）。
    #[test]
    fn source_position_syncs_every_frame() {
        let mut world = world_with_audio();
        let entity = world.spawn((
            Transform::from_xyz(4.0, 0.0, 0.0),
            GlobalTransform::from_transform(Transform::from_xyz(4.0, 0.0, 0.0)),
            SpatialSource::new(clip(), 1.0, true, 4.0),
        ));
        update_spatial_audio(&world);

        // 正右方：右声道能量绝对主导
        let (left, right) = channel_energies(&world);
        assert!(right > left * 100.0, "正右源应右响：{left} vs {right}");

        // 挪到正左方 → 下一帧镜像
        world.insert(
            entity,
            GlobalTransform::from_transform(Transform::from_xyz(-4.0, 0.0, 0.0)),
        );
        update_spatial_audio(&world);
        let (left, right) = channel_energies(&world);
        assert!(left > right * 100.0, "挪到正左后应左响：{left} vs {right}");
    }

    /// 移除 → 停止：组件消失（despawn / remove_component 均使查询检不到）
    /// 的下一帧，差分停止对应声音、快照归零。
    #[test]
    fn removed_source_stops_on_next_update() {
        let mut world = world_with_audio();
        let entity = world.spawn((
            Transform::from_xyz(0.0, 0.0, -4.0),
            GlobalTransform::from_transform(Transform::from_xyz(0.0, 0.0, -4.0)),
            SpatialSource::new(clip(), 0.5, true, 4.0),
        ));
        update_spatial_audio(&world);
        assert_eq!(world.resource::<Audio>().mixer().active_count(), 1);

        // despawn：查询检不到 → 下一帧差分停止
        world.despawn(entity);
        update_spatial_audio(&world);
        {
            let audio = world.resource::<Audio>();
            assert_eq!(
                audio.mixer().active_count(),
                0,
                "despawn 后源应在下一帧被停止"
            );
            assert!(audio.spatial_active().is_empty(), "快照应随差分归零");
        }

        // remove_component 同语义：起播 → 摘组件 → 下一帧停止
        let entity = world.spawn((
            Transform::from_xyz(0.0, 0.0, -4.0),
            GlobalTransform::from_transform(Transform::from_xyz(0.0, 0.0, -4.0)),
            SpatialSource::new(clip(), 0.5, true, 4.0),
        ));
        update_spatial_audio(&world);
        assert_eq!(world.resource::<Audio>().mixer().active_count(), 1);
        assert!(world.remove_component::<SpatialSource>(entity).is_some());
        update_spatial_audio(&world);
        assert_eq!(
            world.resource::<Audio>().mixer().active_count(),
            0,
            "移除组件后源应在下一帧被停止"
        );
    }

    /// 听者：查询序第一个 `(SpatialListener, GlobalTransform)` 写入混音器
    /// （恒等旋转 → 默认朝向、位置取实体全局平移）；挪动听者下一帧跟随；
    /// 无 Audio 资源的世界静默返回。
    #[test]
    fn listener_follows_first_marker_and_absent_audio_is_safe() {
        let mut world = world_with_audio();
        world.spawn((
            SpatialListener,
            Transform::from_xyz(1.0, 2.0, 3.0),
            GlobalTransform::from_transform(Transform::from_xyz(1.0, 2.0, 3.0)),
        ));
        world.spawn((
            SpatialListener,
            Transform::from_xyz(9.0, 9.0, 9.0),
            GlobalTransform::from_transform(Transform::from_xyz(9.0, 9.0, 9.0)),
        ));
        update_spatial_audio(&world);
        let expected = ListenerParams::from_view(Vec3::new(1.0, 2.0, 3.0), -Vec3::Z, Vec3::Y);
        assert_eq!(
            world.resource::<Audio>().mixer().listener(),
            expected,
            "应取查询序第一个听者（恒等旋转 → 默认朝向）"
        );

        // 挪第一个听者：下一帧跟随（第二个听者不夺位）
        let first = world.entities().next().expect("应有听者实体");
        world.insert(
            first,
            GlobalTransform::from_transform(Transform::from_xyz(5.0, 2.0, 3.0)),
        );
        update_spatial_audio(&world);
        assert_eq!(
            world.resource::<Audio>().mixer().listener().position,
            Vec3::new(5.0, 2.0, 3.0),
            "听者位置应逐帧跟随实体"
        );

        // 无 Audio：静默返回（不 panic、无副作用）
        let bare = World::new();
        update_spatial_audio(&bare);
    }

    /// GlobalTransform 缺席的源被跳过（不播放、不进差分），补挂后下一帧
    /// 起播；clone 共享句柄、detach 重置为待起播。
    #[test]
    fn source_without_global_transform_skipped_and_detach_resets() {
        let mut world = world_with_audio();
        let entity = world.spawn((SpatialSource::new(clip(), 0.5, true, 4.0),));
        update_spatial_audio(&world);
        assert_eq!(
            world.resource::<Audio>().mixer().active_count(),
            0,
            "GlobalTransform 缺席的源应被跳过"
        );
        assert!(world.resource::<Audio>().spatial_active().is_empty());

        // 补挂 GlobalTransform：下一帧起播
        world.insert(
            entity,
            GlobalTransform::from_transform(Transform::from_xyz(0.0, 0.0, -4.0)),
        );
        update_spatial_audio(&world);
        assert_eq!(world.resource::<Audio>().mixer().active_count(), 1);

        // clone 携带句柄；detach 清空（克隆体下次系统运行会另起一个声音）
        let mut cloned = {
            let mut found = None;
            world.query::<&SpatialSource>(|source| found = Some(source.clone()));
            found.expect("应有源组件")
        };
        assert!(cloned.started().is_some(), "clone 应携带句柄");
        cloned.detach();
        assert!(cloned.started().is_none(), "detach 应清空句柄");
    }
}
