//! 跨线程借用原语：尝试型 RwLock 获取与冲突即 panic 的语义。
//!
//! 与 `RefCell` 的语义逐条对应，但借用检查跨线程生效：
//! - 多个**只读借用**可并存（跨线程并发读者、同线程递归读均合法）；
//! - **可变借用**与一切冲突；
//! - 任何冲突**立即 panic**（绝不阻塞）——本引擎的调度器按类型级读写集
//!   分批，运行期并行批内的组件访问必然不相交，因此锁竞争必然是
//!   访问规则违规。快速失败给出明确 panic，而非死锁或未定义行为。
//!
//! 全部实现为安全代码：`std::sync::RwLock` 的 `try_read/try_write` 提供
//! 跨线程内部可变性，不依赖 `UnsafeCell` 手写逻辑。
//!
//! 关于 poison：线程 panic 会毒化锁，但本引擎的安全模型不依赖 poison
//! 标志（冲突已由 try 获取拦截），因此获取时把 poison 视为正常持有。

use std::sync::{RwLock, TryLockError};

/// 读锁获取：冲突（写持有）时 panic，消息含 `already mutably borrowed`。
pub(crate) fn read_lock<'a, T>(
    lock: &'a RwLock<T>,
    what: &str,
) -> std::sync::RwLockReadGuard<'a, T> {
    match lock.try_read() {
        Ok(guard) => guard,
        // 线程 panic 毒化的锁照常可读（安全模型不依赖 poison）
        Err(TryLockError::Poisoned(err)) => err.into_inner(),
        Err(TryLockError::WouldBlock) => panic!(
            "{what} 已被可变借用（already mutably borrowed）：读/写访问冲突"
        ),
    }
}

/// 写锁获取：冲突（读或写持有）时 panic，消息含 `already borrowed`。
pub(crate) fn write_lock<'a, T>(
    lock: &'a RwLock<T>,
    what: &str,
) -> std::sync::RwLockWriteGuard<'a, T> {
    match lock.try_write() {
        Ok(guard) => guard,
        Err(TryLockError::Poisoned(err)) => err.into_inner(),
        Err(TryLockError::WouldBlock) => panic!(
            "{what} 已被借用（already borrowed）：重复访问或读/写访问冲突"
        ),
    }
}

/// 解出可能被毒化的锁内的值（`remove` 等取得锁所有权后使用）。
pub(crate) fn into_inner<T>(lock: RwLock<T>) -> T {
    lock.into_inner().unwrap_or_else(|err| err.into_inner())
}
