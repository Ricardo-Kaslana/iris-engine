//! 全局资源：单例数据（相机、输入状态、配置等），按类型存取。

use std::any::{Any, TypeId};
use std::cell::{Ref, RefCell, RefMut};
use std::collections::HashMap;

/// 资源：全局唯一的单例数据。显式实现（推荐 `#[derive(Resource)]`）。
pub trait Resource: 'static + Send + Sync {}

#[derive(Default)]
pub(crate) struct Resources {
    map: HashMap<TypeId, RefCell<Box<dyn Any + Send + Sync>>>,
}

impl Resources {
    /// 插入资源，若已存在则覆盖并返回旧值。
    pub fn insert<R: Resource>(&mut self, value: R) -> Option<R> {
        match self.map.get_mut(&TypeId::of::<R>()) {
            Some(cell) => {
                let mut slot = cell.borrow_mut();
                let old = std::mem::replace(
                    &mut *slot,
                    Box::new(value) as Box<dyn Any + Send + Sync>,
                );
                Some(*old.downcast::<R>().expect("存储类型与注册类型一致"))
            }
            None => {
                self.map
                    .insert(TypeId::of::<R>(), RefCell::new(Box::new(value)));
                None
            }
        }
    }

    pub fn borrow<R: Resource>(&self) -> Option<Ref<'_, R>> {
        let cell = self.map.get(&TypeId::of::<R>())?;
        Some(Ref::map(cell.borrow(), |value| {
            value.downcast_ref::<R>().expect("存储类型与注册类型一致")
        }))
    }

    pub fn borrow_mut<R: Resource>(&self) -> Option<RefMut<'_, R>> {
        let cell = self.map.get(&TypeId::of::<R>())?;
        Some(RefMut::map(cell.borrow_mut(), |value| {
            value.downcast_mut::<R>().expect("存储类型与注册类型一致")
        }))
    }

    pub fn remove<R: Resource>(&mut self) -> Option<R> {
        let cell = self.map.remove(&TypeId::of::<R>())?;
        Some(*cell.into_inner().downcast::<R>().expect("存储类型与注册类型一致"))
    }

    pub fn contains<R: Resource>(&self) -> bool {
        self.map.contains_key(&TypeId::of::<R>())
    }
}

#[cfg(test)]
mod tests {
    use crate::resource::Resources;
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
            // 共享借用可并存
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
