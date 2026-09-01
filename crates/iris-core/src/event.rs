//! 事件：系统间的帧内消息通道。
//!
//! 事件在每帧渲染结束后清空——它们是「当帧消息」，不跨帧累积。
//! 需要跨帧的状态请使用资源（`Resource`）。

use std::any::Any;

use crate::Event;

/// 事件类型标记。显式实现（推荐 `#[derive(Event)]`）。
pub trait Event: 'static + Send + Sync {}

/// 请求退出应用。引擎主循环检测到此事件后结束运行。
#[derive(Event)]
pub struct AppExit;

/// 某一类型事件的缓冲。
pub struct Events<E> {
    items: Vec<E>,
}

impl<E> Default for Events<E> {
    fn default() -> Self {
        Self { items: Vec::new() }
    }
}

impl<E: Event> Events<E> {
    pub fn send(&mut self, event: E) {
        self.items.push(event);
    }

    /// 本帧已累积的全部事件。
    pub fn iter(&self) -> std::slice::Iter<'_, E> {
        self.items.iter()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// 取走全部事件（drain 语义）。
    pub fn drain(&mut self) -> std::vec::Drain<'_, E> {
        self.items.drain(..)
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

pub(crate) trait ErasedEvents: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn clear(&mut self);
}

impl<E: Event> ErasedEvents for Events<E> {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn clear(&mut self) {
        self.items.clear();
    }
}

/// 事件缓冲只读守卫：解引用为 [`Events<E>`]。
pub struct EventRef<'a, E> {
    guard: std::sync::RwLockReadGuard<'a, Box<dyn ErasedEvents>>,
    _marker: std::marker::PhantomData<fn() -> E>,
}

impl<'a, E: Event> EventRef<'a, E> {
    pub(crate) fn new(guard: std::sync::RwLockReadGuard<'a, Box<dyn ErasedEvents>>) -> Self {
        Self {
            guard,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<E: Event> std::ops::Deref for EventRef<'_, E> {
    type Target = Events<E>;
    fn deref(&self) -> &Events<E> {
        self.guard
            .as_any()
            .downcast_ref::<Events<E>>()
            .expect("事件存储类型与注册类型一致")
    }
}

#[cfg(test)]
mod tests {
    use crate::Event;
    use crate::event::Events;

    #[derive(Debug, Clone, Copy, PartialEq, Event)]
    struct Hit(u32);

    #[test]
    fn send_read_clear() {
        let mut events = Events::<Hit>::default();
        assert!(events.is_empty());
        events.send(Hit(1));
        events.send(Hit(2));
        assert_eq!(events.len(), 2);
        assert_eq!(
            events.iter().copied().collect::<Vec<_>>(),
            vec![Hit(1), Hit(2)]
        );
        events.clear();
        assert!(events.is_empty());
    }

    #[test]
    fn drain_takes_all() {
        let mut events = Events::<Hit>::default();
        events.send(Hit(7));
        let drained: Vec<_> = events.drain().collect();
        assert_eq!(drained, vec![Hit(7)]);
        assert!(events.is_empty());
    }
}
