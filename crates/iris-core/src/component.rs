//! 组件存储：稀疏集（sparse set）。
//!
//! 内存布局：
//! ```text
//! sparse: [u32::MAX, 2, u32::MAX, 0, ...]   实体索引 -> dense 下标（O(1) 随机访问）
//! dense:  [组件数据 ...]                     连续数组（遍历缓存友好）
//! entities: [与 dense 平行的实体标识 ...]
//! ```
//!
//! 删除采用 swap_remove，`dense` 始终紧凑，无空洞、无碎片。

use std::any::{Any, TypeId};
use std::cell::{Ref, RefCell, RefMut};
use std::collections::HashMap;

use crate::entity::Entity;

/// 组件：附加到实体上的数据。
///
/// 显式实现（推荐 `#[derive(Component)]`），约束 `Send + Sync + 'static`
/// 保证未来多线程调度时的数据竞争安全。
pub trait Component: 'static + Send + Sync {}

const EMPTY: u32 = u32::MAX;

pub struct SparseSet<T> {
    dense: Vec<T>,
    entities: Vec<Entity>,
    sparse: Vec<u32>,
}

impl<T> SparseSet<T> {
    pub fn new() -> Self {
        Self { dense: Vec::new(), entities: Vec::new(), sparse: Vec::new() }
    }

    /// 插入或覆盖组件，返回旧值（若存在）。
    pub fn insert(&mut self, entity: Entity, value: T) -> Option<T> {
        let index = entity.index() as usize;
        if index >= self.sparse.len() {
            self.sparse.resize(index + 1, EMPTY);
        }
        let dense_index = self.sparse[index];
        if dense_index != EMPTY && self.entities[dense_index as usize] == entity {
            return Some(std::mem::replace(&mut self.dense[dense_index as usize], value));
        }
        debug_assert!(
            dense_index == EMPTY,
            "稀疏集内部不一致：实体 {entity:?} 的槽位指向 {:?} 的数据",
            self.entities.get(dense_index as usize),
        );
        self.sparse[index] = self.dense.len() as u32;
        self.dense.push(value);
        self.entities.push(entity);
        None
    }

    pub fn get(&self, entity: Entity) -> Option<&T> {
        let dense_index = self.lookup(entity)?;
        self.dense.get(dense_index)
    }

    pub fn get_mut(&mut self, entity: Entity) -> Option<&mut T> {
        let dense_index = self.lookup(entity)?;
        self.dense.get_mut(dense_index)
    }

    /// 移除组件（swap_remove），返回被移除的值。
    pub fn remove(&mut self, entity: Entity) -> Option<T> {
        let dense_index = self.lookup(entity)?;
        let removed = self.dense.swap_remove(dense_index);
        self.entities.swap_remove(dense_index);
        if dense_index < self.dense.len() {
            // 末尾元素被移到了 dense_index，修正其反向映射
            let moved = self.entities[dense_index];
            self.sparse[moved.index() as usize] = dense_index as u32;
        }
        self.sparse[entity.index() as usize] = EMPTY;
        Some(removed)
    }

    pub fn contains(&self, entity: Entity) -> bool {
        self.lookup(entity).is_some()
    }

    fn lookup(&self, entity: Entity) -> Option<usize> {
        let dense_index = *self.sparse.get(entity.index() as usize)?;
        if dense_index == EMPTY {
            return None;
        }
        // 代数校验：同一索引的旧实体无法读取新实体的数据
        if self.entities[dense_index as usize] != entity {
            return None;
        }
        Some(dense_index as usize)
    }

    pub fn len(&self) -> usize {
        self.dense.len()
    }

    pub fn is_empty(&self) -> bool {
        self.dense.is_empty()
    }

    pub fn entities(&self) -> &[Entity] {
        &self.entities
    }

    pub fn values(&self) -> &[T] {
        &self.dense
    }

    pub fn iter(&self) -> impl Iterator<Item = (Entity, &T)> {
        self.entities.iter().copied().zip(self.dense.iter())
    }
}

impl<T> Default for SparseSet<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// 类型擦除的存储接口：`World` 层面按 `TypeId` 索引，无需知晓具体组件类型。
pub(crate) trait ErasedStore: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn remove_entity(&mut self, entity: Entity) -> bool;
}

impl<T: Component> ErasedStore for SparseSet<T> {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn remove_entity(&mut self, entity: Entity) -> bool {
        self.remove(entity).is_some()
    }
}

/// 全部组件存储的注册表。`RefCell` 提供运行期借用检查：
/// 同一组件类型被两次可变借用时会立刻 panic（而非产生数据竞争）。
#[derive(Default)]
pub(crate) struct Components {
    stores: HashMap<TypeId, RefCell<Box<dyn ErasedStore>>>,
}

impl Components {
    pub fn register<C: Component>(&mut self) {
        self.stores
            .entry(TypeId::of::<C>())
            .or_insert_with(|| RefCell::new(Box::new(SparseSet::<C>::new())));
    }

    fn cell<C: Component>(&self) -> Option<&RefCell<Box<dyn ErasedStore>>> {
        self.stores.get(&TypeId::of::<C>())
    }

    pub fn borrow<C: Component>(&self) -> Option<Ref<'_, SparseSet<C>>> {
        let cell = self.cell::<C>()?;
        Some(Ref::map(cell.borrow(), |store| {
            store.as_any().downcast_ref::<SparseSet<C>>().expect("存储类型与注册类型一致")
        }))
    }

    pub fn borrow_mut<C: Component>(&self) -> Option<RefMut<'_, SparseSet<C>>> {
        let cell = self.cell::<C>()?;
        Some(RefMut::map(cell.borrow_mut(), |store| {
            store.as_any_mut().downcast_mut::<SparseSet<C>>().expect("存储类型与注册类型一致")
        }))
    }

    pub fn insert<C: Component>(&mut self, entity: Entity, value: C) -> Option<C> {
        self.register::<C>();
        self.borrow_mut::<C>().expect("刚刚注册").insert(entity, value)
    }

    pub fn remove<C: Component>(&mut self, entity: Entity) -> Option<C> {
        self.borrow_mut::<C>()?.remove(entity)
    }

    /// 实体销毁时移除其全部组件。
    pub fn remove_all_for(&mut self, entity: Entity) {
        for cell in self.stores.values() {
            cell.borrow_mut().remove_entity(entity);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(index: u32, generation: u32) -> Entity {
        Entity::new(index, generation)
    }

    #[test]
    fn insert_get_remove() {
        let mut set = SparseSet::new();
        assert_eq!(set.insert(e(0, 0), 10i32), None);
        assert_eq!(set.insert(e(1, 0), 20), None);
        assert_eq!(set.get(e(0, 0)), Some(&10));
        assert_eq!(set.get(e(1, 0)), Some(&20));
        assert_eq!(set.get(e(2, 0)), None);
        assert_eq!(set.remove(e(0, 0)), Some(10));
        assert_eq!(set.get(e(0, 0)), None);
        assert_eq!(set.get(e(1, 0)), Some(&20), "swap_remove 后其余数据应完好");
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn overwrite_returns_old_value() {
        let mut set = SparseSet::new();
        set.insert(e(3, 0), 'a');
        assert_eq!(set.insert(e(3, 0), 'b'), Some('a'));
        assert_eq!(set.get(e(3, 0)), Some(&'b'));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn stale_generation_cannot_access() {
        let mut set = SparseSet::new();
        set.insert(e(0, 0), 1u8);
        assert_eq!(set.get(e(0, 1)), None, "旧代数实体不得访问新代数数据");
        assert!(!set.contains(e(0, 1)));
    }

    #[test]
    fn swap_remove_keeps_mapping_consistent() {
        let mut set = SparseSet::new();
        for i in 0..8u32 {
            set.insert(e(i, 0), i * 100);
        }
        set.remove(e(2, 0));
        set.remove(e(6, 0));
        assert_eq!(set.len(), 6);
        // 全量读取校验映射一致性
        for (entity, value) in set.iter() {
            assert_eq!(set.get(entity), Some(value));
        }
        let remaining: Vec<_> = set.values().iter().copied().collect();
        assert_eq!(remaining, vec![0, 100, 700, 300, 400, 500]);
    }

    #[test]
    fn remove_nonexistent_is_none() {
        let mut set = SparseSet::new();
        set.insert(e(1, 0), 1);
        assert_eq!(set.remove(e(0, 0)), None);
        assert_eq!(set.remove(e(1, 0)), Some(1));
        assert_eq!(set.remove(e(1, 0)), None, "重复移除应返回 None");
    }
}
