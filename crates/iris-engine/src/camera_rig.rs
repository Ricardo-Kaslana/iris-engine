//! 平滑追随组件（打磨 IV 摩擦④）：「子实体相机 + 帧率无关平滑追随」
//! 的引擎现成范式——把 iris-ascent 曾在游戏侧手工积分约 30 行的
//! `CameraRig` 资源收编为组件 + 系统。
//!
//! # 范式（三层玩家结构，iris-game / iris-ascent 先例）
//!
//! ```text
//! 根实体（逻辑位置，rotation 恒等）
//! ├── 模型子实体（动画 / 朝向）
//! └── 相机子实体（ActiveCamera + SmoothFollow + 固定俯仰的 rotation）
//! ```
//!
//! [`SmoothFollow`] 记忆**平滑世界位** `pos`，每帧朝「目标世界位 +
//! `offset`」指数趋近，并把追随者的**局部**平移写成
//! `parent⁻¹ × pos`——层级传播（PostUpdate）还原出平滑世界位。分量
//! 数学（第三人称机位的惯用装配）：追随者的父级即目标实体、旋转恒等
//! 时，局部平移 = `pos − 目标位置`，传播后 `目标位置 + 局部 = pos`，
//! 与「平滑量写在子实体的局部位置上」的手工范式逐位一致。
//!
//! # 帧率无关
//!
//! 趋近系数取 `1 − exp(−lerp_per_sec × dt)`（连续指数衰减的离散精确
//! 解）：同一段真实时间切成任意帧长组合，收敛轨迹一致（单测守护）。
//! `lerp_per_sec ≤ 0` 视为直接贴合（无平滑）。
//!
//! # 编排契约（务必阅读）
//!
//! - **注册时机**：`update_smooth_follow` 在 Update 阶段、**追随目标
//!   的写入系统之后**注册（iris-ascent：`run_ascent` 之后的独立并行
//!   系统）——目标本帧的位移当帧即被追随；若注册在目标写入之前也可
//!   用，代价是目标本帧位移迟到一帧（与直接读 `GlobalTransform` 的
//!   滞后等价，文档口径见下）。
//! - **目标世界位的口径**：系统沿父链用**局部 `Transform` 现值**合成
//!   目标当帧全局位（不读 `GlobalTransform`——它是上一帧传播的产物，
//!   滞后一帧会把目标本帧位移整段迟到一帧、叠加在平滑误差上）。若
//!   目标的局部变换在系统运行后才被写入，等效于读上帧
//!   `GlobalTransform`——**GlobalTransform 滞后一帧在此场景可接受**
//!   （60 Hz 下匀速位移误差 ≈ 每帧步长，收敛后相机仍精确跟随）。
//! - **与层级传播的先后**：本系统在 Update 写局部 `Transform`，
//!   PostUpdate（`HierarchyPlugin`）当帧完成全局传播——粒子 / 空间
//!   音频等传播之后的消费方读到的相机全局位已含本帧平滑结果。
//! - **只管理位置**：追随者自身的旋转 / 缩放不动（机位俯仰等朝向由
//!   调用方维护）；目标父链存在旋转 / 缩放时，偏移经父系逆矩阵精确
//!   还原（局部 = `parent⁻¹ × pos`），语义仍精确。
//! - **吸附**：复活 / 传送等瞬移调用 [`SmoothFollow::snap`]，下一次
//!   更新直接对齐目标位（不做长距离甩镜）；`snap_on_first_frame`
//!   让刚挂载的组件首帧即吸附（不从原点甩镜）。
//!
//! # 用法
//!
//! ```ignore
//! // Update 阶段、在写玩家位置的系统之后注册：
//! schedule.add_parallel_system(
//!     Stage::Update,
//!     Access::new()
//!         .read::<Time>()
//!         .write::<SmoothFollow>()
//!         .write::<Transform>(),
//!     update_smooth_follow,
//! );
//!
//! // 相机子实体（父级 = 玩家根）：
//! let camera = world.spawn((
//!     Transform { rotation: Quat::from_rotation_x(pitch), ..IDENTITY },
//!     ActiveCamera::default(),
//!     SmoothFollow::new(player_root, Vec3::new(0.0, 4.2, 8.5), 6.0),
//! ));
//! // 复活瞬移帧：
//! world.get_mut::<SmoothFollow>(camera).unwrap().snap();
//! ```

use glam::{Mat4, Vec3};

use iris_core::prelude::{Entity, Time, Transform};
use iris_core::world::World;

/// 平滑追随组件：挂**追随者**实体（通常是相机子实体），逐帧朝
/// 「[`SmoothFollow::target`] 的当帧世界位 + [`SmoothFollow::offset`]」
/// 指数趋近。语义与编排契约见模块文档。
#[derive(Clone, Copy, Debug, PartialEq, iris_core::Component)]
pub struct SmoothFollow {
    /// 追随目标实体。
    pub target: Entity,
    /// 目标世界位附加的偏移（第三人称机位等）。
    pub offset: Vec3,
    /// 平滑刚度（1/秒）：指数趋近的时间常数倒数。≤ 0 视为直接贴合。
    pub lerp_per_sec: f32,
    /// 首次更新直接吸附（组件刚挂载、`pos` 尚无意义时不从原点甩镜）。
    pub snap_on_first_frame: bool,
    /// 当前平滑世界位（记忆态，由 [`update_smooth_follow`] 维护；
    /// 调用方按只读约定消费——headless 断言吸附 / 收敛即读它）。
    pub pos: Vec3,
    /// 待吸附标记（[`SmoothFollow::snap`] 置位，下一次更新消费后清除）。
    pub pending_snap: bool,
    /// 是否已执行过首次更新（内部状态；`snap_on_first_frame` 的判定
    /// 依据）。
    pub initialized: bool,
}

impl SmoothFollow {
    /// 构造：首帧吸附开启（`snap_on_first_frame = true`）——刚挂载的
    /// 追随者第一次更新即对齐目标位，无须调用方再初始化 `pos`。
    pub fn new(target: Entity, offset: Vec3, lerp_per_sec: f32) -> Self {
        Self {
            target,
            offset,
            lerp_per_sec,
            snap_on_first_frame: true,
            pos: Vec3::ZERO,
            pending_snap: false,
            initialized: false,
        }
    }

    /// 请求下一次更新直接吸附到目标位（复活 / 传送用，避免长距离
    /// 甩镜）。标记在下一次 `update_smooth_follow` 消费后清除。
    pub fn snap(&mut self) {
        self.pending_snap = true;
    }
}

/// 一帧的追随任务（阶段一只读收集、阶段二写入——与动画系统同款
/// 两阶段模式，避免读写交错）。
struct FollowJob {
    follower: Entity,
    /// 新的平滑世界位（写回组件记忆态）。
    pos: Vec3,
    /// 新的局部平移（写回追随者 Transform；旋转 / 缩放不动）。
    local: Vec3,
    done: bool,
}

/// 每帧平滑追随系统：推进所有 [`SmoothFollow`]。注册编排见模块文档
/// （Update 阶段、目标写入系统之后；只读世界签名 + 尝试型锁，可注册
/// 为**并行系统**，访问集：写 SmoothFollow / Transform，读 Time）。
///
/// `Time` 资源缺席时 dt 视为 0（不做插值，只结算吸附 / 首帧初始化）。
pub fn update_smooth_follow(world: &World) {
    let dt = world
        .try_resource::<Time>()
        .map(|time| time.delta_secs())
        .unwrap_or(0.0);

    // 阶段一（只读）：计算每个追随者的新世界位与新局部平移
    let mut jobs: Vec<FollowJob> = Vec::new();
    world.query::<(Entity, &SmoothFollow)>(|(follower, follow)| {
        // 目标当帧世界位：沿父链用局部 Transform 现值合成（口径见模块
        // 文档——不读滞后一帧的 GlobalTransform）
        let target_world = current_world_position(world, follow.target) + follow.offset;
        let (pos, done) =
            if follow.pending_snap || (follow.snap_on_first_frame && !follow.initialized) {
                (target_world, true)
            } else if dt > 0.0 {
                // 帧率无关指数趋近：p ← p + (target − p)·(1 − e^(−k·dt))
                let t = if follow.lerp_per_sec <= 0.0 {
                    1.0
                } else {
                    1.0 - (-follow.lerp_per_sec * dt).exp()
                };
                (follow.pos + (target_world - follow.pos) * t, false)
            } else {
                (follow.pos, false)
            };
        // 局部平移 = 父级当帧全局矩阵的逆 × 世界点（追随者自身旋转 /
        // 缩放不动）
        let local = world_to_local_position(world, follower, pos);
        jobs.push(FollowJob {
            follower,
            pos,
            local,
            done,
        });
    });

    // 阶段二（写入）：更新记忆态与追随者局部 Transform
    for job in jobs {
        if let Some(mut follow) = world.get_mut::<SmoothFollow>(job.follower) {
            follow.pos = job.pos;
            follow.pending_snap = false;
            follow.initialized = follow.initialized || job.done || dt > 0.0;
        }
        if let Some(mut transform) = world.get_mut::<Transform>(job.follower) {
            transform.position = job.local;
        }
    }
}

/// 实体的**当帧**世界平移：沿父链收集局部 `Transform` 后自上而下合成
/// （根的局部即全局；不读 `GlobalTransform`，口径见模块文档）。
/// 父链上缺席 `Transform` 的实体按恒等处理（防御性）。
fn current_world_position(world: &World, entity: Entity) -> Vec3 {
    let matrix = current_world_matrix(world, entity);
    Vec3::new(matrix.w_axis.x, matrix.w_axis.y, matrix.w_axis.z)
}

/// 实体的当帧全局矩阵（局部 `Transform` 链合成）。
fn current_world_matrix(world: &World, entity: Entity) -> Mat4 {
    let mut chain: Vec<Transform> = Vec::new();
    let mut current = Some(entity);
    while let Some(entity) = current {
        current = world.parent(entity);
        match world.get::<Transform>(entity) {
            Some(transform) => chain.push(*transform),
            None => break,
        }
    }
    chain
        .iter()
        .rev()
        .fold(Mat4::IDENTITY, |matrix, transform| {
            matrix * transform.matrix()
        })
}

/// 把期望的世界坐标转换为追随者在其父系下的局部平移：
/// `parent⁻¹ × world_point`（点变换，含平移项）。
fn world_to_local_position(world: &World, follower: Entity, world_pos: Vec3) -> Vec3 {
    let parent_matrix = world
        .parent(follower)
        .map(|parent| current_world_matrix(world, parent))
        .unwrap_or(Mat4::IDENTITY);
    parent_matrix.inverse().transform_point3(world_pos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core::hierarchy::propagate_transforms;
    use iris_core::prelude::GlobalTransform;
    use std::time::Duration;

    const OFFSET: Vec3 = Vec3::new(0.0, 4.2, 8.5);
    const STIFFNESS: f32 = 6.0;

    /// 确定性推进一帧：追随 + 传播。
    fn step_mut(world: &mut World, dt: f64) {
        world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(dt));
        update_smooth_follow(world);
        propagate_transforms(world);
    }

    /// 首帧吸附：snap_on_first_frame 组件挂上后第一次更新即对齐目标位
    /// （局部 = offset、传播后全局 = offset、记忆态 = offset）。
    #[test]
    fn first_frame_snaps_when_configured() {
        let mut world = World::new();
        world.insert_resource(Time::default());
        let target = world.spawn((Transform::IDENTITY,));
        let camera = world.spawn((
            Transform::IDENTITY,
            SmoothFollow::new(target, OFFSET, STIFFNESS),
        ));
        world.add_child(target, camera);
        step_mut(&mut world, 1.0 / 60.0);
        let follow = *world.get::<SmoothFollow>(camera).unwrap();
        assert_eq!(follow.pos, OFFSET, "首帧应吸附到目标位 + 偏移");
        assert!(follow.initialized, "首帧后应标记已初始化");
        let local = world.get::<Transform>(camera).unwrap().position;
        assert_eq!(local, OFFSET, "目标在原点时局部平移 = offset");
        let global = world.get::<GlobalTransform>(camera).unwrap().position();
        assert_eq!(global, OFFSET, "传播后的全局位应恰为平滑世界位");
    }

    /// 收敛与帧率无关：同一段真实时间（1 s）按 1/60 × 60 帧与 1/20 × 20
    /// 帧两种切法，收敛终点一致（指数衰减的离散精确解）。
    #[test]
    fn convergence_is_frame_rate_independent() {
        // 同一「瞬移 → 恒定不动」场景按给定帧长推进 1 秒，返回记忆态
        let run = |frame_dt: f64, frames: usize| {
            let mut world = World::new();
            world.insert_resource(Time::default());
            let target = world.spawn((Transform::IDENTITY,));
            let camera = world.spawn((
                Transform::IDENTITY,
                SmoothFollow::new(target, OFFSET, STIFFNESS),
            ));
            world.add_child(target, camera);
            // 目标瞬移到 (2, 0, 0)，随后恒定不动——只比较收敛轨迹
            world.get_mut::<Transform>(target).unwrap().position = Vec3::new(2.0, 0.0, 0.0);
            for _ in 0..frames {
                step_mut(&mut world, frame_dt);
            }
            world.get::<SmoothFollow>(camera).unwrap().pos
        };
        let fine = run(1.0 / 60.0, 60);
        let coarse = run(1.0 / 20.0, 20);
        let expected = Vec3::new(2.0, 0.0, 0.0) + OFFSET;
        assert!(
            fine.distance(expected) < 1e-3,
            "1 秒后应几乎收敛到目标位：{fine} vs {expected}"
        );
        assert!(
            fine.distance(coarse) < 1e-3,
            "不同帧长切法的收敛终点应一致：{fine} vs {coarse}"
        );
    }

    /// 吸附（复活 / 传送）：snap() 后下一次更新直接对齐目标位（记忆态
    /// 与目标 + offset 逐位一致），不走逐帧趋近。
    #[test]
    fn snap_teleports_without_lerp() {
        let mut world = World::new();
        world.insert_resource(Time::default());
        let target = world.spawn((Transform::IDENTITY,));
        let camera = world.spawn((
            Transform::IDENTITY,
            SmoothFollow::new(target, OFFSET, STIFFNESS),
        ));
        world.add_child(target, camera);
        step_mut(&mut world, 1.0 / 60.0);
        // 目标远距离传送
        let destination = Vec3::new(40.0, 30.0, -100.0);
        world.get_mut::<Transform>(target).unwrap().position = destination;
        world.get_mut::<SmoothFollow>(camera).unwrap().snap();
        step_mut(&mut world, 1.0 / 60.0);
        let follow = *world.get::<SmoothFollow>(camera).unwrap();
        assert_eq!(
            follow.pos,
            destination + OFFSET,
            "吸附帧记忆态应逐位等于目标 + offset"
        );
        // 全局位同样精确（父系无旋转缩放）
        let global = world.get::<GlobalTransform>(camera).unwrap().position();
        assert_eq!(global, destination + OFFSET, "吸附帧全局位应精确对齐");
    }

    /// 只管理位置：追随者自身的旋转（机位俯仰）在追随过程中不动。
    #[test]
    fn keeps_follower_rotation() {
        let mut world = World::new();
        world.insert_resource(Time::default());
        let target = world.spawn((Transform::IDENTITY,));
        let pitch = glam::Quat::from_rotation_x(-25.0_f32.to_radians());
        let camera = world.spawn((
            Transform {
                rotation: pitch,
                ..Transform::IDENTITY
            },
            SmoothFollow::new(target, OFFSET, STIFFNESS),
        ));
        world.add_child(target, camera);
        for _ in 0..10 {
            step_mut(&mut world, 1.0 / 60.0);
        }
        assert_eq!(
            world.get::<Transform>(camera).unwrap().rotation,
            pitch,
            "追随者的旋转不应被平滑追随改动"
        );
    }

    /// 目标本帧位移当帧即被追随（系统注册在目标写入之后时）：无平滑
    /// 滞后残差的第一步——目标挪动后首帧 pos 就朝新目标趋近（而非朝
    /// 上一帧位置）。
    #[test]
    fn uses_this_frame_target_position() {
        let mut world = World::new();
        world.insert_resource(Time::default());
        let target = world.spawn((Transform::IDENTITY,));
        let camera = world.spawn((
            Transform::IDENTITY,
            SmoothFollow::new(target, OFFSET, STIFFNESS),
        ));
        world.add_child(target, camera);
        step_mut(&mut world, 1.0 / 60.0); // 首帧吸附到原点 + offset
        // 目标本帧挪到 (1, 0, 0)（模拟游戏系统先写、追随系统后跑）
        world.get_mut::<Transform>(target).unwrap().position = Vec3::new(1.0, 0.0, 0.0);
        step_mut(&mut world, 1.0 / 60.0);
        let follow = *world.get::<SmoothFollow>(camera).unwrap();
        // 首帧朝 (1,0,0) + offset 趋近：pos 应已向新目标移动（若误读
        // 上一帧目标，pos 仍停在原 offset 不动）
        assert!(
            follow.pos.x > OFFSET.x + 1e-4,
            "追随应使用目标当帧位置：pos = {}",
            follow.pos
        );
    }
}
