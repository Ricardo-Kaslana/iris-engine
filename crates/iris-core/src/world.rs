//! 世界：实体 + 组件 + 资源 + 事件的统一容器，引擎一切状态的家。
//!
//! `World` 是 `Send + Sync` 的：组件 / 资源 / 事件存储都在
//! `std::sync::RwLock` 之后（尝试型获取、冲突即 panic，见 [`crate::sync`]），
//! 因此 `&World` 可以安全地跨线程共享——这是并行系统调度的地基。

use std::any::TypeId;
use std::collections::HashMap;
use std::fmt;
use std::sync::RwLock;

use crate::component::{Component, Components, Ref, RefMut};
use crate::entity::{Entities, Entity};
use crate::event::{ErasedEvents, Event, EventRef, Events};
use crate::query::QueryPart;
use crate::resource::{Resource, ResourceRef, ResourceRefMut, Resources};
use crate::sync::{read_lock, write_lock};

/// 组件包：可一次性 spawn 的一组组件。元组形式 `(A, B, C)`；
/// 单个组件请写成一元组 `(A,)`。
pub trait Bundle {
    fn insert_into(self, world: &mut World, entity: Entity);
}

macro_rules! impl_bundle_for_tuple {
    ($($name:ident),+) => {
        #[allow(non_snake_case)]
        impl<$($name: Component),+> Bundle for ($($name,)+) {
            fn insert_into(self, world: &mut World, entity: Entity) {
                let ($($name,)+) = self;
                $(world.insert(entity, $name);)+
            }
        }
    };
}

impl_bundle_for_tuple!(A);
impl_bundle_for_tuple!(A, B);
impl_bundle_for_tuple!(A, B, C);
impl_bundle_for_tuple!(A, B, C, D);
impl_bundle_for_tuple!(A, B, C, D, E);
impl_bundle_for_tuple!(A, B, C, D, E, F);
impl_bundle_for_tuple!(A, B, C, D, E, F, G);
impl_bundle_for_tuple!(A, B, C, D, E, F, G, H);

/// 世界：引擎的全部状态。
pub struct World {
    pub(crate) entities: Entities,
    pub(crate) components: Components,
    resources: Resources,
    events: HashMap<TypeId, RwLock<Box<dyn ErasedEvents>>>,
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    pub fn new() -> Self {
        Self {
            entities: Entities::new(),
            components: Components::default(),
            resources: Resources::default(),
            events: HashMap::new(),
        }
    }

    // ---- 实体 ----

    /// 生成实体并挂载组件包。
    pub fn spawn<B: Bundle>(&mut self, bundle: B) -> Entity {
        let entity = self.entities.alloc();
        bundle.insert_into(self, entity);
        entity
    }

    /// 生成不带组件的实体。
    pub fn spawn_empty(&mut self) -> Entity {
        self.entities.alloc()
    }

    /// 销毁实体及其全部组件。返回 `false` 表示实体已失效。
    pub fn despawn(&mut self, entity: Entity) -> bool {
        if !self.entities.free(entity) {
            return false;
        }
        self.components.remove_all_for(entity);
        true
    }

    pub fn contains(&self, entity: Entity) -> bool {
        self.entities.contains(entity)
    }

    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }

    /// 按索引升序遍历所有存活实体。
    pub fn entities(&self) -> impl Iterator<Item = Entity> + '_ {
        self.entities.iter()
    }

    // ---- 组件 ----

    /// 为实体插入组件，返回旧值（若已有同类型组件）。
    pub fn insert<C: Component>(&mut self, entity: Entity, component: C) -> Option<C> {
        if !self.contains(entity) {
            log::warn!(
                "向已失效的实体 {entity:?} 插入组件 {}，已忽略",
                std::any::type_name::<C>()
            );
            return None;
        }
        self.components.insert(entity, component)
    }

    /// 只读访问实体的组件。
    pub fn get<C: Component>(&self, entity: Entity) -> Option<Ref<'_, C>> {
        let store = self.components.borrow::<C>()?;
        store.get(entity).is_some().then(|| Ref::new(store, entity))
    }

    /// 可变访问实体的组件（尝试型 RwLock 借用检查，冲突即 panic）。
    pub fn get_mut<C: Component>(&self, entity: Entity) -> Option<RefMut<'_, C>> {
        let store = self.components.borrow_mut::<C>()?;
        store.get(entity).is_some().then(|| RefMut::new(store, entity))
    }

    pub fn remove_component<C: Component>(&mut self, entity: Entity) -> Option<C> {
        self.components.remove::<C>(entity)
    }

    /// 遍历查询 `( &A, &mut B, ... )`：对每个拥有全部所需组件的实体调用 `f`。
    ///
    /// ```ignore
    /// world.query::<(&Position, &mut Velocity)>(|pos, vel| {
    ///     pos.position += vel.0 * dt;
    /// });
    /// ```
    ///
    /// 注意：迭代期间不得对世界做结构性修改（spawn / despawn / insert），
    /// `RefCell` 借用检查会拦截这类操作并给出明确的 panic。
    pub fn query<Q: QueryPart>(&self, mut f: impl FnMut(Q::Item<'_>)) {
        let Some(mut guard) = Q::guard(self) else {
            return;
        };
        let len = Q::len(&guard);
        for index in 0..len {
            let Some(entity) = Q::entity_at(&guard, index) else {
                continue;
            };
            if let Some(item) = Q::get(&mut guard, entity) {
                f(item);
            }
        }
    }

    // ---- 资源 ----

    pub fn insert_resource<R: Resource>(&mut self, resource: R) -> Option<R> {
        self.resources.insert(resource)
    }

    /// 读取资源；未注册时 panic 并给出类型名。
    pub fn resource<R: Resource>(&self) -> ResourceRef<'_, R> {
        self.try_resource().unwrap_or_else(|| {
            panic!("资源 {} 未注册，请先 insert_resource", std::any::type_name::<R>())
        })
    }

    pub fn try_resource<R: Resource>(&self) -> Option<ResourceRef<'_, R>> {
        self.resources.borrow::<R>()
    }

    pub fn resource_mut<R: Resource>(&self) -> ResourceRefMut<'_, R> {
        self.try_resource_mut().unwrap_or_else(|| {
            panic!("资源 {} 未注册，请先 insert_resource", std::any::type_name::<R>())
        })
    }

    pub fn try_resource_mut<R: Resource>(&self) -> Option<ResourceRefMut<'_, R>> {
        self.resources.borrow_mut::<R>()
    }

    pub fn remove_resource<R: Resource>(&mut self) -> Option<R> {
        self.resources.remove::<R>()
    }

    pub fn contains_resource<R: Resource>(&self) -> bool {
        self.resources.contains::<R>()
    }

    // ---- 事件 ----

    /// 注册事件类型（发送时自动注册，显式注册用于「先读后发」的场景）。
    pub fn register_event<E: Event>(&mut self) {
        self.events
            .entry(TypeId::of::<E>())
            .or_insert_with(|| RwLock::new(Box::new(Events::<E>::default())));
    }

    /// 发送事件（当帧内有效，帧尾清空）。
    pub fn send_event<E: Event>(&mut self, event: E) {
        self.register_event::<E>();
        let cell = self.events.get(&TypeId::of::<E>()).expect("事件刚刚注册");
        write_lock(cell, &format!("事件 {}", std::any::type_name::<E>()))
            .as_any_mut()
            .downcast_mut::<Events<E>>()
            .expect("事件存储类型与注册类型一致")
            .send(event);
    }

    /// 读取事件缓冲；类型从未注册时返回 `None`。
    pub fn try_events<E: Event>(&self) -> Option<EventRef<'_, E>> {
        let cell = self.events.get(&TypeId::of::<E>())?;
        Some(EventRef::new(read_lock(
            cell,
            &format!("事件 {}", std::any::type_name::<E>()),
        )))
    }

    pub fn events<E: Event>(&self) -> EventRef<'_, E> {
        self.try_events()
            .unwrap_or_else(|| panic!("事件 {} 未注册", std::any::type_name::<E>()))
    }

    /// 帧尾清空全部事件缓冲（由引擎主循环调用）。
    pub fn clear_all_events(&mut self) {
        for cell in self.events.values() {
            write_lock(cell, "事件缓冲（帧尾清理）").clear();
        }
    }
}

impl fmt::Debug for World {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("World")
            .field("entities", &self.entity_count())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::{Schedule, Stage};
    use crate::time::Time;
    // derive 宏与同名 trait 分属不同命名空间，可同时导入
    use crate::{Component, Event, Resource};

    #[derive(Component)] struct Position(f32);
    #[derive(Component)] struct Velocity(f32);
    #[derive(Debug, PartialEq, Clone, Copy, Event)] struct Message(&'static str);

    fn integrate(world: &mut World) {
        let dt = world.resource::<Time>();
        let dt = dt.delta_secs();
        world.query::<(&mut Position, &Velocity)>(|(p, v)| p.0 += v.0 * dt);
    }

    #[test]
    fn spawn_query_despawn_roundtrip() {
        let mut world = World::new();
        let a = world.spawn((Position(1.0), Velocity(2.0)));
        let b = world.spawn((Position(3.0),));
        assert_eq!(world.entity_count(), 2);

        let mut total = 0.0;
        world.query::<&Position>(|p| total += p.0);
        assert_eq!(total, 4.0);

        assert!(world.despawn(b));
        assert!(!world.despawn(b), "重复销毁返回 false");
        assert_eq!(world.entity_count(), 1);
        assert!(world.get::<Position>(b).is_none(), "销毁后组件不可访问");

        world.insert(a, Position(9.0));
        assert_eq!(world.get::<Position>(a).unwrap().0, 9.0);
    }

    #[test]
    fn systems_share_world_via_scheduler() {
        let mut world = World::new();
        world.insert_resource(Time::default());
        // 推进两帧，使 delta 生效（首帧 delta 为 0）
        let t0 = std::time::Instant::now();
        world.resource_mut::<Time>().begin_frame(t0);
        world
            .resource_mut::<Time>()
            .begin_frame(t0 + std::time::Duration::from_secs_f64(1.0 / 60.0));
        world.spawn((Position(0.0), Velocity(60.0)));

        let mut schedule = Schedule::new();
        schedule.add_system(Stage::Update, integrate);
        schedule.run(Stage::Update, &mut world);

        assert_eq!(
            world.get::<Position>(world.entities().next().unwrap()).unwrap().0,
            1.0
        );
    }

    #[test]
    fn events_live_within_a_frame() {
        let mut world = World::new();
        world.send_event(Message("hello"));
        world.send_event(Message("world"));
        assert_eq!(world.events::<Message>().len(), 2);
        let collected: Vec<_> = world.events::<Message>().iter().copied().collect();
        assert_eq!(collected, vec![Message("hello"), Message("world")]);
        world.clear_all_events();
        assert_eq!(world.events::<Message>().len(), 0);
    }

    #[test]
    #[should_panic(expected = "未注册")]
    fn missing_resource_panics_with_type_name() {
        #[derive(Resource)]
        struct Missing;
        let world = World::new();
        let _ = world.resource::<Missing>();
    }

    /// `&World` 可跨线程共享（并行系统调度的类型层地基）；
    /// 借用守卫可被多线程共享（Sync），但自身不跨线程移动（无需 Send）。
    #[test]
    fn world_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        fn assert_sync<T: Sync>() {}
        assert_send_sync::<World>();
        assert_sync::<crate::component::Ref<'static, Position>>();
        assert_sync::<crate::component::StoreRef<'static, Position>>();
    }

    /// 同线程递归读合法（查询守卫存活期间再次读取同类型组件——
    /// 渲染器收集闭包内的既有模式）。
    #[test]
    fn recursive_read_is_allowed() {
        let mut world = World::new();
        let e = world.spawn((Position(7.0),));
        world.query::<&Position>(|_| {
            let p = world.get::<Position>(e).unwrap();
            assert_eq!(p.0, 7.0);
        });
    }
}
