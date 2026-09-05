//! 事件：系统间的帧内消息通道。
//!
//! # 生命周期（打磨 V 起：双代轮换）
//!
//! 事件产出后的**可读窗口 = 产出帧的余下部分 + 下一帧全程**：每帧帧首
//! 主循环的轮换（[`World::clear_all_events`]，实现为「上一代丢弃、本帧
//! 缓冲降级为上一代」）把上帧事件降级为仍可读的「上一代」、丢弃连续两
//! 帧都未被消费的更早一代。因此：
//!
//! - **PostUpdate 产出的事件下一帧 Update 全程可读**——UI 点击等
//!   「晚阶段产出 → 早阶段消费」的跨阶段投递不再需要游戏侧信箱资源
//!   （ARCHITECTURE §8.22① 的闭合）；
//! - 帧首被丢弃的只是**连续两帧未被 drain 消费**的陈旧残留——正常
//!   「当帧产出、下帧（或当帧更晚系统）消费」的事件流不受影响；
//! - 需要跨两帧以上的状态请使用资源（`Resource`）。
//!
//! # 消费契约
//!
//! - `iter` 是**非破坏性**读取：同一事件在其可读窗口内每次读取都可见
//!   （帧内可重复读；不 drain 时上一帧的事件在下一帧还会再出现一次）；
//! - `drain` 即消费：取走全部事件（两代一起），此后不可见；
//! - 每条事件流每帧应**恰好有一个 drain 消费方**（或消费逻辑对重复可见
//!   幂等）；只 `iter` 不 `drain` 的消费方必须自行容忍「上帧事件再出现
//!   一次」的语义（接触事件等 begin/end 计数型消费方**必须 drain**，
//!   否则 begin 会被重复计数）。

use std::any::Any;

use crate::Event;

/// 事件类型标记。显式实现（推荐 `#[derive(Event)]`）。
pub trait Event: 'static + Send + Sync {}

/// 请求退出应用。引擎主循环检测到此事件后结束运行。
#[derive(Event)]
pub struct AppExit;

/// 某一类型事件的缓冲（双代：`previous` = 上帧事件，本帧仍可读；
/// `current` = 本帧新产出，见模块文档「生命周期」）。
pub struct Events<E> {
    previous: Vec<E>,
    current: Vec<E>,
}

impl<E> Default for Events<E> {
    fn default() -> Self {
        Self {
            previous: Vec::new(),
            current: Vec::new(),
        }
    }
}

impl<E: Event> Events<E> {
    pub fn send(&mut self, event: E) {
        self.current.push(event);
    }

    /// 可读窗口内的全部事件（上一代在前、本帧在后——按产出时间序）。
    pub fn iter(&self) -> std::iter::Chain<std::slice::Iter<'_, E>, std::slice::Iter<'_, E>> {
        self.previous.iter().chain(self.current.iter())
    }

    pub fn len(&self) -> usize {
        self.previous.len() + self.current.len()
    }

    pub fn is_empty(&self) -> bool {
        self.previous.is_empty() && self.current.is_empty()
    }

    /// 取走全部事件（drain 语义，两代一起——消费后不可见）。
    pub fn drain(&mut self) -> std::vec::IntoIter<E> {
        let mut all = std::mem::take(&mut self.previous);
        all.append(&mut self.current);
        all.into_iter()
    }

    /// 帧首轮换（由 [`World::clear_all_events`] 每帧调用）：上一代丢弃、
    /// 本帧缓冲降级为上一代——事件可读窗口据此覆盖「产出帧 + 下一帧」。
    pub fn rotate(&mut self) {
        self.previous = std::mem::take(&mut self.current);
    }

    /// 全量清空（两代一起）。
    pub fn clear(&mut self) {
        self.previous.clear();
        self.current.clear();
    }
}

pub(crate) trait ErasedEvents: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    /// 帧首轮换（见 [`Events::rotate`]）。
    fn rotate(&mut self);
}

impl<E: Event> ErasedEvents for Events<E> {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn rotate(&mut self) {
        self.rotate();
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

/// 事件缓冲可变守卫：解引用为 `&mut Events<E>`——`drain` 消费事件用
/// （每帧取走全部，与帧首 [`World::clear_all_events`] 的轮换兜底同语义，
/// 见模块文档「消费契约」）。
///
/// `&World` 即可获取（照 `try_send_event` 的写锁先例）：并行系统可以
/// drain 已注册类型的事件——**同一事件流的 drain 方每帧应恰好一个**
/// （多个 drain 方并发时，事件在它们之间的归属顺序不确定）。
pub struct EventRefMut<'a, E> {
    guard: std::sync::RwLockWriteGuard<'a, Box<dyn ErasedEvents>>,
    _marker: std::marker::PhantomData<fn() -> E>,
}

impl<'a, E: Event> EventRefMut<'a, E> {
    pub(crate) fn new(guard: std::sync::RwLockWriteGuard<'a, Box<dyn ErasedEvents>>) -> Self {
        Self {
            guard,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<E: Event> std::ops::Deref for EventRefMut<'_, E> {
    type Target = Events<E>;
    fn deref(&self) -> &Events<E> {
        self.guard
            .as_any()
            .downcast_ref::<Events<E>>()
            .expect("事件存储类型与注册类型一致")
    }
}

impl<E: Event> std::ops::DerefMut for EventRefMut<'_, E> {
    fn deref_mut(&mut self) -> &mut Events<E> {
        self.guard
            .as_any_mut()
            .downcast_mut::<Events<E>>()
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

    /// 帧首轮换：上帧事件降级为上一代后仍可读；drain 消费过的事件不因
    /// 轮换复活（「帧内 drain 后轮换不再误删 / 误留」的语义锚点）。
    #[test]
    fn rotation_keeps_last_frame_readable_and_drained_events_stay_gone() {
        let mut events = Events::<Hit>::default();
        events.send(Hit(1));
        events.rotate(); // 帧首：Hit(1) 进入上一代
        assert_eq!(
            events.iter().copied().collect::<Vec<_>>(),
            vec![Hit(1)],
            "上帧事件经轮换后仍应可读"
        );
        events.send(Hit(2));
        assert_eq!(
            events.iter().copied().collect::<Vec<_>>(),
            vec![Hit(1), Hit(2)],
            "上一代在前、本帧在后（产出时间序）"
        );
        // drain 消费两代全部
        let drained: Vec<_> = events.drain().collect();
        assert_eq!(drained, vec![Hit(1), Hit(2)]);
        assert!(events.is_empty());
        // 已 drain 的事件不因下一次轮换复活
        events.send(Hit(3));
        events.rotate();
        assert_eq!(
            events.iter().copied().collect::<Vec<_>>(),
            vec![Hit(3)],
            "轮换后只应剩下本帧产出（已消费的不得复活）"
        );
    }

    /// 连续两帧都未被 drain 消费的残留，在第二次轮换时被丢弃——事件
    /// 可读窗口严格为「产出帧 + 下一帧」，不无限累积。
    #[test]
    fn unconsumed_events_are_dropped_after_second_rotation() {
        let mut events = Events::<Hit>::default();
        events.send(Hit(9));
        events.rotate();
        assert_eq!(events.len(), 1, "产出帧 + 下一帧内仍可读");
        events.rotate();
        assert!(events.is_empty(), "连续两帧未消费的残留应被帧首轮换丢弃");
    }
}
