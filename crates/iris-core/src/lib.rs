//! # iris-core
//!
//! Iris 引擎核心：实体组件系统（ECS）、系统调度、事件、时间管理与数学库。
//!
//! 设计原则：
//! - **内存安全**：实体采用「索引 + 代数」标识，悬垂实体访问返回 `None` 而非未定义行为；
//!   组件与资源存储基于尝试型 RwLock 的运行期借用检查（跨线程生效），
//!   任何读/写冲突立刻 panic 而非数据竞争或死锁。
//! - **高性能**：组件存于稀疏集（sparse set），`dense` 数组保证遍历内存连续、缓存友好；
//!   查询通过泛型单态化实现静态分发，热路径无动态分发；
//!   调度器按系统声明的组件读写集冲突分批，无冲突系统并行执行。
//! - **低占用**：无垃圾回收，组件销毁即时、确定性地归还内存。

// 让 `#[derive(Component)]` 生成的 `::iris_core::...` 路径在本 crate 内部测试同样可用。
extern crate self as iris_core;

pub mod behavior;
pub mod broadphase;
pub mod character;
pub mod commands;
pub mod component;
pub mod daynight;
pub mod entity;
pub mod event;
pub mod hierarchy;
pub mod instant;
pub mod name;
pub mod nav;
pub mod perception;
pub mod physics;
pub mod query;
pub mod ragdoll;
pub mod resource;
pub mod schedule;
pub(crate) mod sync;
pub mod time;
pub mod transform;
pub mod wind;
pub mod world;

/// 派生宏：`#[derive(Component)]` / `#[derive(Resource)]` / `#[derive(Event)]`。
/// 与同名 trait 分属不同命名空间，可同时导入。
pub use iris_macros::{Component, Event, Resource};

pub mod prelude {
    // 同名 trait 与派生宏分属不同命名空间，可同时导出：
    pub use crate::behavior::{
        ActionId, BehaviorAgent, BehaviorContext, BehaviorState, BehaviorTree, BlackValue,
        Blackboard, BtStatus, Node, NodeId, NodeSpec, ParallelPolicy, tick_behavior_agents,
    };
    // `use iris_core::prelude::*;` 后即可直接 `#[derive(Component)]`
    pub use crate::character::{
        CharacterOutput, CharacterParams, move_and_slide, move_and_slide_ccd,
        move_and_slide_statics, move_and_slide_statics_rotated, move_and_slide_statics_rotated_ccd,
    };
    pub use crate::commands::{Command, CommandQueue, apply_commands};
    pub use crate::component::Component;
    // 昼夜循环（R80）：相位资源 + 太阳方向 / 光照包络纯函数
    pub use crate::daynight::{DayNightCycle, ambient_envelope, is_night, sun_direction};
    pub use crate::entity::Entity;
    pub use crate::event::{AppExit, Event, Events};
    pub use crate::hierarchy::{ChildOf, Children, GlobalTransform, propagate_transforms};
    // 跨平台时间点封装（wasm 上 std::time::Instant 会运行时 panic，见 instant.rs）
    pub use crate::instant::Instant;
    pub use crate::name::Name;
    pub use crate::nav::{NavGrid, NavGridParams};
    pub use crate::perception::{
        Perception, PerceptionEvent, PerceptionTarget, can_see, flat_statics, has_line_of_sight,
        in_view_cone, perception_system, within_hearing,
    };
    pub use crate::physics::{
        BodyType, BrokenJoints, Ccd, Collider, ColliderShape, CollisionEnded, CollisionStarted,
        ContactSolverV2, ContactTracker, DistanceJoint, HingeJoint, ImpulseParams, JointBreak,
        JointBroken, PHYSICS_GRAVITY, RigidBody, RigidContactState, StaticObstacle, SwingJoint,
        TargetJoint, contact_point_rotated, effective_static, inv_inertia_world,
        prune_broken_joints, shape_inertia, solve_contact_impulse, step_physics,
    };
    pub use crate::query::QueryPart;
    pub use crate::ragdoll::{
        RAGDOLL_GROUP, RagdollDesc, RagdollLimbDef, RagdollLink, release_ragdoll,
        spawn_ragdoll_kinematic,
    };
    pub use crate::resource::Resource;
    pub use crate::schedule::{
        Access, MIN_SHARD_ITEMS, ParallelSystem, Schedule, Stage, System, shard_count_for,
        shard_range,
    };
    pub use crate::time::Time;
    pub use crate::transform::Transform;
    // 全局风场（R80）：接口轮——粒子 v2 / 毛发 / 降水 / 布料植被四线共用
    pub use crate::wind::{Wind, wind_sample};
    pub use crate::world::{Bundle, World};
    pub use crate::{Component, Event, Resource};
    pub use glam::{Mat3, Mat4, Quat, Vec2, Vec3, Vec3A, Vec4};
}
