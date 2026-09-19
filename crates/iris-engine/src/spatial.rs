//! 空间音频 v1/v3：听者 / 声源组件与每帧同步系统（混音数学在
//! [`iris_audio::mixer`]，本模块只负责 ECS → 混音器的接线）。
//!
//! v3（C6）在既有位置同步之上追加两件事，**默认全关 = 逐位不变**：
//! - **多普勒**：`SpatialSource::doppler` 开启后按帧间位置差分推估声源
//!   速度，听者速度同样差分（首帧 / 无 `Time` ⇒ 零速 = 因子恒 1.0）；
//! - **遮挡**：`SpatialSource::occlusion` 开启后逐帧做「听者→源」线段 ×
//!   静态阻挡体的二值判定（`effective_static` 口径同守卫视线），命中即
//!   推 `muffle` 强度（混音侧一阶低通，见 `iris_audio::muffle_coef`）。
//!   判定在引擎、滤波在混音核心——混音器保持纯函数零世界依赖。
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

use iris_core::physics::{
    Collider, ColliderShape, RigidBody, effective_static, segment_hits_shape_rotated,
};
use iris_core::prelude::{Component, Entity, GlobalTransform, Quat, Time, Transform};

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
///
/// v3（C6）两开关，**默认全关 = 与 v1/v2 逐位一致（关时系统根本不触碰
/// 混音器对应槽位）**：
/// - `doppler`：开启后系统按**帧间位置差分**自动推估声源速度并逐帧推送
///   （无显式速度输入——传送/瞬移会产生一帧尖峰，多普勒因子有钳界兜底）；
/// - `occlusion`：开启后每帧做「听者→源」线段 × 静态阻挡体
///   （`effective_static` 口径：无 `RigidBody` / `is_static` / Kinematic，
///   **sensor 不豁免**——与守卫视线同一事实源）的二值遮挡判定，
///   命中 ⇒ 该声音 `muffle = occluded_muffle`，未命中 ⇒ 0（旁路）；
///   源自身实体上的碰撞体不参与判定（贴身墙恒挡的病理排除）。
///
/// 运行中把开关从 true 翻回 false 不会自动清零已推送的速度/闷声
/// （v1 边界，§8.66——需要即翻即净走 `Audio::set_sound_*` 手动口）。
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
    /// 多普勒开关（v3）：开启后按帧间位置差分推估速度。默认 `false`。
    pub doppler: bool,
    /// 遮挡开关（v3）：开启后逐帧射线判定并推 `muffle`。默认 `false`。
    pub occlusion: bool,
    /// 遮挡命中时的闷声强度（v3，[0, 1]；仅 `occlusion` 开启时被读）。
    pub occluded_muffle: f32,
    /// 内部状态：已起播的防悬垂句柄（`None` = 尚未起播）。
    /// Clone 会复制句柄（两个组件共享同一声音）——克隆后如需独立播放，
    /// 先经 [`SpatialSource::detach`] 清空。
    started: Option<SoundId>,
}

impl SpatialSource {
    /// 构造未起播的声源（v3 两开关默认关 = v1 行为逐位不变）。
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
            doppler: false,
            occlusion: false,
            occluded_muffle: 1.0,
            started: None,
        }
    }

    /// 开启多普勒（链式）。
    #[must_use]
    pub fn with_doppler(mut self) -> Self {
        self.doppler = true;
        self
    }

    /// 开启遮挡判定（链式），并指定命中强度（[0, 1]；NaN / 越界在混音侧
    /// 卫生化）。
    #[must_use]
    pub fn with_occlusion(mut self, occluded_muffle: f32) -> Self {
        self.occlusion = true;
        self.occluded_muffle = occluded_muffle;
        self
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

/// 每帧空间音频同步系统（见模块文档的差分三步；v3 追加多普勒 / 遮挡）。
///
/// `Audio` 资源缺席时静默返回（无音频的世界照常运行）；注册为**并行
/// 系统**：写 `SpatialSource`、读 `SpatialListener` / `GlobalTransform` /
/// `Audio` / `Time` / `Collider` / `RigidBody`（播放编排经 `Arc<Mutex>`
/// 混音器完成，`&Audio` 即可，与 `run_arena` 等既有系统同款声明）。
///
/// **v3 速度 = 帧间位置差分**：本系统读上一帧快照位置 + `Time::delta_secs`
/// 推估源 / 听者速度——游戏侧无需显式给速（代价：瞬移产生一帧尖峰，混音侧
/// 多普勒因子钳界兜底）。遮挡判定吃静态碰撞体（`effective_static` 口径），
/// 仅在有 `occlusion` 源时惰性收集一次静态列表（全关 = 零收集开销）。
pub fn update_spatial_audio(world: &iris_core::world::World) {
    let Some(audio) = world.try_resource::<Audio>() else {
        return;
    };
    let dt = world.try_resource::<Time>().map_or(0.0, |t| t.delta_secs());

    // ---- 遮挡静态体（仅当有源开启遮挡才收集，存储序遍历 + entity 升序；
    // 位姿读 `&Transform`——口径同 perception 视线静态体，见其模块文档）----
    let mut obstacles: Vec<(Entity, Vec3, Quat, ColliderShape)> = Vec::new();
    let mut any_occlusion = false;
    world.query::<&SpatialSource>(|source| any_occlusion |= source.occlusion);
    if any_occlusion {
        world.query::<(Entity, &Transform, &Collider, Option<&RigidBody>)>(
            |(entity, transform, collider, rigid)| {
                if effective_static(collider, rigid) {
                    obstacles.push((
                        entity,
                        transform.position,
                        transform.rotation,
                        collider.shape,
                    ));
                }
            },
        );
        obstacles.sort_unstable_by_key(|(entity, ..)| *entity);
    }

    // ---- 听者：查询序第一个（SpatialListener, GlobalTransform）----
    // 前方 = 全局 -Z 轴（矩阵第三列取负），经 from_view 以世界 +Y 重正交化
    // （见模块文档）；v3 速度 = 与上一帧听者位置的差分（dt 病态 ⇒ 零速，
    // 退化为 v1 无多普勒）。
    let prev_listener = audio.spatial_listener_prev();
    let mut listener_done = false;
    let mut new_listener = Vec3::ZERO;
    let mut have_listener = false;
    world.query::<(&SpatialListener, &GlobalTransform)>(|(_, global)| {
        if listener_done {
            return;
        }
        listener_done = true;
        let matrix = global.matrix();
        let forward = -Vec3::new(matrix.z_axis.x, matrix.z_axis.y, matrix.z_axis.z);
        let position = global.position();
        let velocity = match prev_listener {
            Some(prev) if dt.is_finite() && dt > 0.0 => (position - prev) / dt,
            _ => Vec3::ZERO,
        };
        audio.set_listener(
            ListenerParams::from_view(position, forward, Vec3::Y).with_velocity(velocity),
        );
        new_listener = position;
        have_listener = true;
    });
    if have_listener {
        audio.set_spatial_listener_prev(Some(new_listener));
    }

    // ---- 声源差分：起播 / 逐帧同步 / 移除停止（三步见模块文档）----
    let prev_active = audio.spatial_active();
    let mut active: Vec<(u64, SoundId, Vec3)> = Vec::new();
    world.query::<(Entity, &GlobalTransform, &mut SpatialSource)>(|(entity, global, source)| {
        let position = global.position();
        let bits = entity.to_bits();
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
        // v3 多普勒：源速度 = 帧间差分（上一帧快照查本实体 bits）。
        // 开关全关时**不触碰混音器**（默认路径零额外锁，缺席 = 逐位不变）。
        if source.doppler {
            let vel = match prev_active.iter().find(|(b, ..)| *b == bits) {
                Some((_, _, prev)) if dt.is_finite() && dt > 0.0 => (position - *prev) / dt,
                _ => Vec3::ZERO,
            };
            audio.set_sound_velocity(id, vel);
        }
        // v3 遮挡：听者→源线段被任一无关静态体命中 ⇒ 满强度闷声。排除
        // 源自身实体（贴身墙恒挡）；本帧无听者 ⇒ 无从判定，按未遮挡。
        if source.occlusion {
            let blocked = have_listener
                && obstacles.iter().any(|&(e, op, orot, oshape)| {
                    e != entity
                        && segment_hits_shape_rotated(new_listener, position, op, orot, &oshape)
                            .is_some()
                });
            audio.set_sound_muffle(id, if blocked { source.occluded_muffle } else { 0.0 });
        }
        active.push((bits, id, position));
    });
    // 差分：上一帧活跃、本帧查无 → 组件已移除 / 实体已 despawn → 停止
    for (bits, id, _) in audio.spatial_active() {
        if !active.iter().any(|(current, ..)| current == &bits) {
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

    // ---- v3（C6）：多普勒差分 + 遮挡射线 ----

    use iris_core::physics::Collider;
    use iris_core::prelude::Time;
    use std::time::Duration;

    /// 带 Time（可 advance 出非零 delta）+ Audio headless 的世界。
    fn world_with_time_audio() -> World {
        let mut world = world_with_audio();
        world.insert_resource(Time::default());
        world
    }

    /// 多普勒差分：首帧无历史 ⇒ 速度精确 ZERO；源挪 +2、听者挪 −1（X 轴）
    /// 后次帧按 delta=0.25s（`max_delta` 钳位边界，二进制精确分数）推估
    /// ⇒ 源 8 m/s、听者 −4 m/s（解析值逐位）。
    #[test]
    fn doppler_velocity_derived_from_frame_deltas() {
        let mut world = world_with_time_audio();
        world.spawn((
            SpatialListener,
            Transform::from_xyz(0.0, 0.0, 0.0),
            GlobalTransform::from_transform(Transform::from_xyz(0.0, 0.0, 0.0)),
        ));
        let source = world.spawn((
            Transform::from_xyz(8.0, 0.0, 0.0),
            GlobalTransform::from_transform(Transform::from_xyz(8.0, 0.0, 0.0)),
            SpatialSource::new(clip(), 1.0, true, 4.0).with_doppler(),
        ));
        update_spatial_audio(&world);
        let id = {
            let mut found = None;
            world.query::<&SpatialSource>(|s| found = s.started());
            found.expect("首帧起播")
        };
        assert_eq!(
            world.resource::<Audio>().mixer().sound_velocity(id),
            Some(Vec3::ZERO),
            "首帧无差分历史 ⇒ 零速（因子恒 1.0）"
        );

        // 帧 2：源 +X 挪 2、听者 −X 挪 1，delta = 0.5 s
        world.insert_resource(Time::default());
        world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f32(0.25));
        world.insert(
            source,
            GlobalTransform::from_transform(Transform::from_xyz(10.0, 0.0, 0.0)),
        );
        let listener = {
            let mut found = None;
            world.query::<(Entity, &SpatialListener)>(|(e, _)| found = Some(e));
            found.expect("听者实体")
        };
        world.insert(
            listener,
            GlobalTransform::from_transform(Transform::from_xyz(-1.0, 0.0, 0.0)),
        );
        update_spatial_audio(&world);
        let audio_ref = world.resource::<Audio>();
        let mixer = audio_ref.mixer();
        assert_eq!(
            mixer.sound_velocity(id),
            Some(Vec3::new(8.0, 0.0, 0.0)),
            "源速度 = Δpos/dt 解析值（(10−8)/0.25）"
        );
        assert_eq!(
            mixer.listener().velocity,
            Vec3::new(-4.0, 0.0, 0.0),
            "听者速度 = Δpos/dt 解析值（(−1−0)/0.25）"
        );
    }

    /// 遮挡二值：墙（静态 Box）在听者与源之间 ⇒ muffle = occluded_muffle；
    /// 挪出视线 ⇒ 回 0；未开 occlusion 的源在墙后也恒 0（opt-in）；
    /// 源自身挂碰撞体不自挡。
    #[test]
    fn occlusion_is_binary_ray_and_opt_in() {
        let mut world = world_with_time_audio();
        world.spawn((
            SpatialListener,
            Transform::IDENTITY,
            GlobalTransform::from_transform(Transform::IDENTITY),
        ));
        // 墙：听者正后方 −Z 6 m 处的静态盒（半宽 1）
        world.spawn((
            Transform::from_xyz(0.0, 0.0, -6.0),
            GlobalTransform::from_transform(Transform::from_xyz(0.0, 0.0, -6.0)),
            Collider::static_box(Vec3::splat(1.0)),
        ));
        // 遮挡源：−Z 12 m（墙后）；对照源：+X 12 m（无遮挡方向）
        let occluded = world.spawn((
            Transform::from_xyz(0.0, 0.0, -12.0),
            GlobalTransform::from_transform(Transform::from_xyz(0.0, 0.0, -12.0)),
            SpatialSource::new(clip(), 1.0, true, 4.0).with_occlusion(1.0),
        ));
        let clear = world.spawn((
            Transform::from_xyz(12.0, 0.0, 0.0),
            GlobalTransform::from_transform(Transform::from_xyz(12.0, 0.0, 0.0)),
            SpatialSource::new(clip(), 1.0, true, 4.0).with_occlusion(1.0),
        ));
        // 未开遮挡的源也在墙后 ⇒ 恒 0
        let flag_off = world.spawn((
            Transform::from_xyz(0.0, 0.0, -12.0),
            GlobalTransform::from_transform(Transform::from_xyz(0.0, 0.0, -12.0)),
            SpatialSource::new(clip(), 1.0, true, 4.0),
        ));
        update_spatial_audio(&world);
        let ids: Vec<(u64, SoundId)> = {
            let mut pairs = Vec::new();
            world.query::<(Entity, &SpatialSource)>(|(e, s)| {
                pairs.push((e.to_bits(), s.started.expect("首帧起播")))
            });
            pairs
        };
        let id_of = |entity: Entity| -> SoundId {
            ids.iter()
                .find(|(bits, _)| *bits == entity.to_bits())
                .map(|(_, id)| *id)
                .expect("快照含本实体")
        };
        {
            let audio_ref = world.resource::<Audio>();
            let mixer = audio_ref.mixer();
            assert_eq!(
                mixer.sound_muffle(id_of(occluded)),
                Some(1.0),
                "墙后源应满强度闷声"
            );
            assert_eq!(mixer.sound_muffle(id_of(clear)), Some(0.0), "通视源应旁路");
            assert_eq!(
                mixer.sound_muffle(id_of(flag_off)),
                Some(0.0),
                "未开遮挡的源即使墙后也不闷（opt-in）"
            );
        }
        // 挪到侧向脱墙 ⇒ 回 0（强度随帧跟随几何）
        world.insert(
            occluded,
            GlobalTransform::from_transform(Transform::from_xyz(8.0, 0.0, -1.0)),
        );
        update_spatial_audio(&world);
        assert_eq!(
            world
                .resource::<Audio>()
                .mixer()
                .sound_muffle(id_of(occluded)),
            Some(0.0),
            "脱墙后应回旁路"
        );
    }

    /// 自碰撞体不自挡：源实体自带静态盒（枪口挂在带碰撞体的角色上），
    /// 射线排除自身 ⇒ 不闷。
    #[test]
    fn source_own_collider_does_not_self_occlude() {
        let mut world = world_with_time_audio();
        world.spawn((
            SpatialListener,
            Transform::IDENTITY,
            GlobalTransform::from_transform(Transform::IDENTITY),
        ));
        world.spawn((
            Transform::from_xyz(0.0, 0.0, -5.0),
            GlobalTransform::from_transform(Transform::from_xyz(0.0, 0.0, -5.0)),
            Collider::static_box(Vec3::splat(1.0)),
            SpatialSource::new(clip(), 1.0, true, 4.0).with_occlusion(1.0),
        ));
        update_spatial_audio(&world);
        let id = {
            let mut found = None;
            world.query::<&SpatialSource>(|s| found = s.started());
            found.expect("起播")
        };
        assert_eq!(
            world.resource::<Audio>().mixer().sound_muffle(id),
            Some(0.0),
            "源自身碰撞体应被排除（否则带碰撞体的角色声源自闷）"
        );
    }
}
