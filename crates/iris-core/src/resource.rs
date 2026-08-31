//! 全局资源：单例数据（相机、输入状态、配置等），按类型存取。
//!
//! 借用模型与组件存储一致（见 [`crate::sync`]）：每个资源位于
//! `std::sync::RwLock` 之后，尝试型获取——多读者并存，任何冲突立即 panic。

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::RwLock;

use crate::sync::{into_inner, read_lock, write_lock};

/// 资源：全局唯一的单例数据。显式实现（推荐 `#[derive(Resource)]`）。
pub trait Resource: 'static + Send + Sync {}

#[derive(Default)]
pub(crate) struct Resources {
    map: HashMap<TypeId, RwLock<Box<dyn Any + Send + Sync>>>,
}

impl Resources {
    /// 插入资源，若已存在则覆盖并返回旧值。
    pub fn insert<R: Resource>(&mut self, value: R) -> Option<R> {
        match self.map.get_mut(&TypeId::of::<R>()) {
            Some(cell) => {
                let mut slot = write_lock::<Box<dyn Any + Send + Sync>>(
                    cell,
                    &format!("资源 {}", std::any::type_name::<R>()),
                );
                let old = std::mem::replace(
                    &mut *slot,
                    Box::new(value) as Box<dyn Any + Send + Sync>,
                );
                Some(*old.downcast::<R>().expect("存储类型与注册类型一致"))
            }
            None => {
                self.map
                    .insert(TypeId::of::<R>(), RwLock::new(Box::new(value)));
                None
            }
        }
    }

    pub fn borrow<R: Resource>(&self) -> Option<ResourceRef<'_, R>> {
        let cell = self.map.get(&TypeId::of::<R>())?;
        Some(ResourceRef {
            guard: read_lock(cell, &format!("资源 {}", std::any::type_name::<R>())),
            _marker: std::marker::PhantomData,
        })
    }

    pub fn borrow_mut<R: Resource>(&self) -> Option<ResourceRefMut<'_, R>> {
        let cell = self.map.get(&TypeId::of::<R>())?;
        Some(ResourceRefMut {
            guard: write_lock(cell, &format!("资源 {}", std::any::type_name::<R>())),
            _marker: std::marker::PhantomData,
        })
    }

    pub fn remove<R: Resource>(&mut self) -> Option<R> {
        let (_, cell) = self.map.remove_entry(&TypeId::of::<R>())?;
        Some(*into_inner(cell).downcast::<R>().expect("存储类型与注册类型一致"))
    }

    pub fn contains<R: Resource>(&self) -> bool {
        self.map.contains_key(&TypeId::of::<R>())
    }
}

/// 资源只读守卫：解引用为资源值。
pub struct ResourceRef<'a, R> {
    guard: std::sync::RwLockReadGuard<'a, Box<dyn Any + Send + Sync>>,
    _marker: std::marker::PhantomData<fn() -> R>,
}

impl<R: Resource> Deref for ResourceRef<'_, R> {
    type Target = R;
    fn deref(&self) -> &R {
        self.guard
            .downcast_ref::<R>()
            .expect("存储类型与注册类型一致")
    }
}

/// 资源可变守卫：解引用为资源值。
pub struct ResourceRefMut<'a, R> {
    guard: std::sync::RwLockWriteGuard<'a, Box<dyn Any + Send + Sync>>,
    _marker: std::marker::PhantomData<fn() -> R>,
}

impl<R: Resource> Deref for ResourceRefMut<'_, R> {
    type Target = R;
    fn deref(&self) -> &R {
        self.guard
            .downcast_ref::<R>()
            .expect("存储类型与注册类型一致")
    }
}

impl<R: Resource> DerefMut for ResourceRefMut<'_, R> {
    fn deref_mut(&mut self) -> &mut R {
        self.guard
            .downcast_mut::<R>()
            .expect("存储类型与注册类型一致")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Resource;

    #[derive(Debug, PartialEq, Resource)]
    struct Score(u32);
    #[derive(Resource)]
    struct Name(&'static str);

    #[test]
    fn insert_borrow_mutate() {
        let mut resources = Resources::default();
        assert_eq!(resources.insert(Score(0)), None);
        assert_eq!(resources.insert(Score(42)), Some(Score(0)), "覆盖应返回旧值");

        {
            let score = resources.borrow::<Score>().unwrap();
            assert_eq!(score.0, 42);
            // 共享借用可并存（包括同线程递归读）
            let again = resources.borrow::<Score>().unwrap();
            assert_eq!(again.0, 42);
        }

        {
            let mut score = resources.borrow_mut::<Score>().unwrap();
            score.0 += 1;
        }
        assert_eq!(resources.borrow::<Score>().unwrap().0, 43);

        resources.insert(Name("iris"));
        assert!(resources.contains::<Name>());
        assert_eq!(resources.remove::<Name>().unwrap().0, "iris");
        assert!(!resources.contains::<Name>());
    }

    #[test]
    #[should_panic(expected = "already borrowed")]
    fn double_mutable_borrow_panics_not_ub() {
        let mut resources = Resources::default();
        resources.insert(Score(0));
        let _a = resources.borrow_mut::<Score>().unwrap();
        let _b = resources.borrow_mut::<Score>().unwrap(); // 运行期借用检查：立刻 panic
    }
}
