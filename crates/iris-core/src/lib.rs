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

pub mod broadphase;
pub mod character;
pub mod component;
pub mod entity;
pub mod event;
pub mod hierarchy;
pub mod instant;
pub mod name;
pub mod nav;
pub mod physics;
pub mod query;
pub mod ragdoll;
pub mod resource;
pub mod schedule;
pub(crate) mod sync;
pub mod time;
pub mod transform;
pub mod world;

/// 派生宏：`#[derive(Component)]` / `#[derive(Resource)]` / `#[derive(Event)]`。
/// 与同名 trait 分属不同命名空间，可同时导入。
pub use iris_macros::{Component, Event, Resource};

pub mod prelude {
    // 同名 trait 与派生宏分属不同命名空间，可同时导出：
    // `use iris_core::prelude::*;` 后即可直接 `#[derive(Component)]`
    pub use crate::character::{
        CharacterOutput, CharacterParams, move_and_slide, move_and_slide_statics,
    };
    pub use crate::component::Component;
    pub use crate::entity::Entity;
    pub use crate::event::{AppExit, Event, Events};
    pub use crate::hierarchy::{ChildOf, Children, GlobalTransform, propagate_transforms};
    // 跨平台时间点封装（wasm 上 std::time::Instant 会运行时 panic，见 instant.rs）
    pub use crate::instant::Instant;
    pub use crate::name::Name;
    pub use crate::nav::{NavGrid, NavGridParams};
    pub use crate::physics::{
        Collider, ColliderShape, CollisionEnded, CollisionStarted, ContactTracker, DistanceJoint,
        PHYSICS_GRAVITY, RigidBody, TargetJoint, step_physics,
    };
    pub use crate::query::QueryPart;
    pub use crate::ragdoll::{
        RAGDOLL_GROUP, RagdollDesc, RagdollLimbDef, release_ragdoll, spawn_ragdoll_kinematic,
    };
    pub use crate::resource::Resource;
    pub use crate::schedule::{Access, Schedule, Stage, System};
    pub use crate::time::Time;
    pub use crate::transform::Transform;
    pub use crate::world::{Bundle, World};
    pub use crate::{Component, Event, Resource};
    pub use glam::{Mat3, Mat4, Quat, Vec2, Vec3, Vec3A, Vec4};
}
