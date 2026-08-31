//! 组件查询：`( &A, &mut B, ... )` 形式的元组查询。
//!
//! 通过 GAT（泛型关联类型）实现 **零 unsafe** 的多组件访问：
//! 每个组件存储的 RwLock 尝试型守卫保存在查询守卫元组中，
//! 生命周期在类型系统内闭合；同一组件类型的重复可变借用
//! 触发明确的 panic 而非未定义行为（借用检查跨线程生效，
//! 见 [`crate::sync`]）。`Option<&T>` / `Option<&mut T>` 为可选访问：
//! 缺组件时产出 `None` 而非跳过实体。

use crate::component::{Component, StoreRef, StoreRefMut};
use crate::entity::Entity;
use crate::world::World;

/// 可被查询的组件访问模式。
///
/// 为 `&T`（只读）、`&mut T`（可写）以及它们的元组实现。
pub trait QueryPart {
    /// 持有的存储借用守卫。
    type Guard<'w>;
    /// 每次访问产出的项（含组件引用）。
    type Item<'a>;

    /// 从世界借用存储；组件类型从未注册时返回 `None`（等价于空查询结果）。
    fn guard<'w>(world: &'w World) -> Option<Self::Guard<'w>>;

    /// 迭代基准长度（取元组第一个成员的存储长度）。
    fn len(guard: &Self::Guard<'_>) -> usize;

    /// 迭代基准中的第 `index` 个实体。
    fn entity_at<'a>(guard: &'a Self::Guard<'_>, index: usize) -> Option<Entity>;

    /// 取出该实体的访问项；实体缺少此组件时返回 `None`（该实体被跳过）。
    fn get<'a>(guard: &'a mut Self::Guard<'_>, entity: Entity) -> Option<Self::Item<'a>>;
}

impl<T: Component> QueryPart for &T {
    type Guard<'w> = StoreRef<'w, T>;
    type Item<'a> = &'a T;

    fn guard<'w>(world: &'w World) -> Option<Self::Guard<'w>> {
        world.components.borrow::<T>()
    }

    fn len(guard: &Self::Guard<'_>) -> usize {
        guard.len()
    }

    fn entity_at<'a>(guard: &'a Self::Guard<'_>, index: usize) -> Option<Entity> {
        guard.entities().get(index).copied()
    }

    fn get<'a>(guard: &'a mut Self::Guard<'_>, entity: Entity) -> Option<Self::Item<'a>> {
        guard.get(entity)
    }
}

impl<T: Component> QueryPart for &mut T {
    type Guard<'w> = StoreRefMut<'w, T>;
    type Item<'a> = &'a mut T;

    fn guard<'w>(world: &'w World) -> Option<Self::Guard<'w>> {
        world.components.borrow_mut::<T>()
    }

    fn len(guard: &Self::Guard<'_>) -> usize {
        guard.len()
    }

    fn entity_at<'a>(guard: &'a Self::Guard<'_>, index: usize) -> Option<Entity> {
        guard.entities().get(index).copied()
    }

    fn get<'a>(guard: &'a mut Self::Guard<'_>, entity: Entity) -> Option<Self::Item<'a>> {
        guard.get_mut(entity)
    }
}

/// 可选只读访问：`Option<&T>`——实体缺少组件时产出 `None` 而非跳过
/// （与必需访问 `&T` 的「缺即跳过」相对）。组件类型从未注册时同样
/// 对每个实体产出 `None`（等价于所有实体都没有该组件）。典型用法：
/// 渲染收集中的 `Visibility` 过滤——组件缺席 = 可见（默认行为零变化）。
impl<T: Component> QueryPart for Option<&T> {
    type Guard<'w> = Option<StoreRef<'w, T>>;
    type Item<'a> = Option<&'a T>;

    fn guard<'w>(world: &'w World) -> Option<Self::Guard<'w>> {
        // 存储缺席不是查询失败：守卫恒为 Some，内层按实体逐个判缺
        Some(world.components.borrow::<T>())
    }

    fn len(guard: &Self::Guard<'_>) -> usize {
        guard.as_ref().map_or(0, |store| store.len())
    }

    fn entity_at<'a>(guard: &'a Self::Guard<'_>, index: usize) -> Option<Entity> {
        guard
            .as_ref()
            .and_then(|store| store.entities().get(index).copied())
    }

    fn get<'a>(guard: &'a mut Self::Guard<'_>, entity: Entity) -> Option<Self::Item<'a>> {
        Some(guard.as_ref().and_then(|store| store.get(entity)))
    }
}

/// 可选可写访问：`Option<&mut T>`（语义同 [`QueryPart for Option<&T>`]，多出写权限）。
impl<T: Component> QueryPart for Option<&mut T> {
    type Guard<'w> = Option<StoreRefMut<'w, T>>;
    type Item<'a> = Option<&'a mut T>;

    fn guard<'w>(world: &'w World) -> Option<Self::Guard<'w>> {
        Some(world.components.borrow_mut::<T>())
    }

    fn len(guard: &Self::Guard<'_>) -> usize {
        guard.as_ref().map_or(0, |store| store.len())
    }

    fn entity_at<'a>(guard: &'a Self::Guard<'_>, index: usize) -> Option<Entity> {
        guard
            .as_ref()
            .and_then(|store| store.entities().get(index).copied())
    }

    fn get<'a>(guard: &'a mut Self::Guard<'_>, entity: Entity) -> Option<Self::Item<'a>> {
        Some(guard.as_mut().and_then(|store| store.get_mut(entity)))
    }
}

/// 以实体本身作为查询项：`query::<(Entity, &Transform)>(|entity, transform| ...)`。
impl QueryPart for Entity {
    type Guard<'w> = &'w crate::entity::Entities;
    type Item<'a> = Entity;

    fn guard<'w>(world: &'w World) -> Option<Self::Guard<'w>> {
        Some(&world.entities)
    }

    fn len(guard: &Self::Guard<'_>) -> usize {
        guard.slot_count()
    }

    fn entity_at<'a>(guard: &'a Self::Guard<'_>, index: usize) -> Option<Entity> {
        (index < guard.slot_count()).then(|| guard.entity_at_slot(index))
    }

    fn get<'a>(guard: &'a mut Self::Guard<'_>, entity: Entity) -> Option<Self::Item<'a>> {
        guard.contains(entity).then_some(entity)
    }
}

macro_rules! impl_query_part_for_tuple {
    ($first:ident, $($rest:ident),+) => {
        #[allow(non_snake_case)]
        impl<$first: QueryPart, $($rest: QueryPart),+> QueryPart for ($first, $($rest,)+) {
            type Guard<'w> = ($first::Guard<'w>, $($rest::Guard<'w>,)+);
            type Item<'a> = ($first::Item<'a>, $($rest::Item<'a>,)+);

            fn guard<'w>(world: &'w World) -> Option<Self::Guard<'w>> {
                Some(($first::guard(world)?, $($rest::guard(world)?,)+))
            }

            fn len(guard: &Self::Guard<'_>) -> usize {
                <$first as QueryPart>::len(&guard.0)
            }

            fn entity_at<'a>(guard: &'a Self::Guard<'_>, index: usize) -> Option<Entity> {
                <$first as QueryPart>::entity_at(&guard.0, index)
            }

            fn get<'a>(guard: &'a mut Self::Guard<'_>, entity: Entity) -> Option<Self::Item<'a>> {
                let ($first, $($rest,)+) = guard;
                Some((
                    <$first as QueryPart>::get($first, entity)?,
                    $(<$rest as QueryPart>::get($rest, entity)?,)+
                ))
            }
        }
    };
}

impl_query_part_for_tuple!(A, B);
impl_query_part_for_tuple!(A, B, C);
impl_query_part_for_tuple!(A, B, C, D);
impl_query_part_for_tuple!(A, B, C, D, E);
impl_query_part_for_tuple!(A, B, C, D, E, F);

#[cfg(test)]
mod tests {
    use crate::Component;
    use crate::world::World;

    #[derive(Component)] struct Position(f32);
    #[derive(Component)] struct Velocity(f32);
    #[derive(Component)] struct Tag;

    #[test]
    fn single_component_query() {
        let mut world = World::new();
        world.spawn((Position(1.0),));
        world.spawn((Position(2.0), Velocity(1.0)));

        let mut seen = vec![];
        world.query::<&Position>(|p| seen.push(p.0));
        assert_eq!(seen, vec![1.0, 2.0]);
    }

    #[test]
    fn tuple_query_skips_entities_missing_components() {
        let mut world = World::new();
        world.spawn((Position(1.0), Velocity(10.0)));
        world.spawn((Position(2.0),)); // 缺 Velocity，应被跳过
        world.spawn((Velocity(30.0),)); // 缺 Position，应被跳过
        world.spawn((Position(3.0), Velocity(30.0)));

        let mut sum = 0.0;
        world.query::<(&Position, &Velocity)>(|(p, v)| sum += p.0 * v.0);
        assert_eq!(sum, 1.0 * 10.0 + 3.0 * 30.0);
    }

    #[test]
    fn mutable_query_modifies_components() {
        let mut world = World::new();
        world.spawn((Position(0.0), Velocity(2.0)));
        world.spawn((Position(0.0), Velocity(5.0)));

        world.query::<(&mut Position, &Velocity)>(|(p, v)| p.0 += v.0);
        let mut values = vec![];
        world.query::<&Position>(|p| values.push(p.0));
        assert_eq!(values, vec![2.0, 5.0]);
    }

    #[test]
    fn query_on_unregistered_type_is_empty() {
        let mut world = World::new();
        world.spawn((Position(1.0),));
        let mut count = 0;
        world.query::<&Tag>(|_| count += 1);
        assert_eq!(count, 0);
    }

    #[test]
    fn optional_query_yields_none_instead_of_skipping() {
        let mut world = World::new();
        world.spawn((Position(1.0), Velocity(10.0)));
        world.spawn((Position(2.0),)); // 缺 Velocity：Option 应产出 None 而非跳过

        let mut seen = vec![];
        world.query::<(&Position, Option<&Velocity>)>(|(p, v)| {
            seen.push((p.0, v.map(|vel| vel.0)))
        });
        assert_eq!(seen, vec![(1.0, Some(10.0)), (2.0, None)]);
    }

    #[test]
    fn optional_query_on_unregistered_type_yields_all_none() {
        let mut world = World::new();
        world.spawn((Position(1.0),));
        world.spawn((Position(2.0),));
        // Tag 从未注册：守卫不失败，所有实体产出 None
        let mut count = 0;
        world.query::<(&Position, Option<&Tag>)>(|(_, v)| {
            count += 1;
            assert!(v.is_none(), "实体不应有 Tag");
        });
        assert_eq!(count, 2, "实体不应被跳过");
    }

    #[test]
    fn optional_mut_query_writes_only_present_components() {
        let mut world = World::new();
        world.spawn((Position(0.0), Velocity(1.0)));
        world.spawn((Position(0.0),)); // 缺 Velocity

        world.query::<(&mut Position, Option<&mut Velocity>)>(|(p, v)| {
            p.0 = 1.0;
            if let Some(v) = v {
                v.0 += 10.0;
            }
        });
        let mut velocities = vec![];
        world.query::<(&Position, Option<&Velocity>)>(|(p, v)| {
            velocities.push((p.0, v.map(|vel| vel.0)));
        });
        assert_eq!(velocities, vec![(1.0, Some(11.0)), (1.0, None)]);
    }

    #[test]
    #[should_panic(expected = "already")]
    fn duplicate_mutable_component_in_query_panics() {
        let mut world = World::new();
        world.spawn((Position(0.0),));
        // 同一类型两次 &mut —— 运行期借用检查给出明确 panic，而非未定义行为
        world.query::<(&mut Position, &mut Position)>(|(_, _)| {});
    }
}
