//! 命令队列：并行系统安全 spawn / despawn 的延迟执行通道（打磨 V）。
//!
//! # 为什么需要
//!
//! 实体分配 / 销毁是结构性修改，需要 `&mut World`——并行系统
//! （`fn(&World)`）做不到（v1 起 `World::spawn` 的签名约束）。缺此通道
//! 时，游戏被迫在 Update 里插**独占系统**（屏障语义，打断并行批次）做
//! 运行期 spawn（iris-infiltration 的 ragdoll 生成，见 ARCHITECTURE
//! §8.22③）。命令队列把「结构性修改」从并行系统体内挪到帧末的独占
//! flush 点，并行系统的吞吐得以保留。
//!
//! # 用法
//!
//! ```ignore
//! use iris_core::prelude::*;
//!
//! // 并行系统内（&World）：入队（队列资源由 DefaultPlugins / CommandsPlugin
//! // 注册；缺席时 try_resource_mut 返回 None——照 try_send_event 的
//! // 未注册语义，静默放弃并 warn）
//! fn stun_system(world: &World) {
//!     if let Some(mut queue) = world.try_resource_mut::<CommandQueue>() {
//!         queue.spawn_deferred(move |world: &mut World| {
//!             world.spawn((Transform::from_xyz(0.0, 1.0, 0.0),));
//!         });
//!         queue.despawn_deferred(corpse);
//!     }
//! }
//!
//! // 独占上下文（&mut World）：缺席自动注册
//! world.command_queue().despawn_deferred(entity);
//!
//! // DefaultPlugins / CommandsPlugin 在 PostUpdate 尾部注册独占系统
//! // `apply_commands`：本帧入队的命令本帧生效（渲染读到最终实体态）。
//! ```
//!
//! # 语义与确定性（诚实清单）
//!
//! - **按入队序执行**：[`CommandQueue::apply`] 逐条执行，spawn / despawn
//!   的实体分配序 = 入队序（确定性部分）；队列内部用 `Mutex` 只为保证
//!   `&World` 入队路径，不改变「执行恒在独占上下文按序进行」的语义；
//! - **执行结果依赖 World 状态**：闭包命令的产物取决于执行那一刻的
//!   世界（已有实体、组件值）——同一命令序列在不同世界状态下产物不同。
//!   **确定性回放场景请把「状态差」编进命令数据（闭包捕获确定性的值
//!   而非引用可变外部状态），或干脆避开命令队列**——命令队列是便利
//!   通道，不是确定性原语；
//! - **单遍语义**：[`apply_commands`] 执行期间新入队的命令**不会**在本
//!   轮处理，留给下一次 apply（下一帧）——闭包自激励不会无限循环；
//! - **despawn 幂等**：对已失效实体的 despawn 是 no-op（与
//!   `World::despawn` 返回 `false` 一致，世代安全不悬垂）。

use std::sync::{Mutex, MutexGuard};

use crate::entity::Entity;
use crate::resource::Resource;
use crate::world::World;

/// 一条延迟执行的命令。
pub enum Command {
    /// 任意独占写入（spawn / 组件插拔 / 建层级等，由闭包自行完成）。
    /// 闭包须 `Send`：入队可能发生在并行系统的其他线程。
    Exec(Box<dyn FnOnce(&mut World) + Send>),
    /// 销毁实体（已失效则 no-op）。
    Despawn(Entity),
}

/// 命令队列资源：并行系统入队、帧末独占 flush（[`apply_commands`]）。
///
/// 内部 `Mutex<Vec<Command>>`：`&World` 即可入队（并行系统路径，照
/// `try_send_event` 的写锁先例）；执行恒在独占上下文按入队序进行。
#[derive(Default)]
pub struct CommandQueue {
    commands: Mutex<Vec<Command>>,
}

// 手动实现（derive 会把 Mutex 的 Poisoning 细节暴露进约束，这里显式写明
// 资源语义：'static + Send + Sync；Command 闭包为 Send，Vec+Mutex 即 Sync）。
impl Resource for CommandQueue {}

impl CommandQueue {
    /// 入队「生成实体」命令：闭包在帧末 [`apply_commands`] 中以
    /// `&mut World` 执行，自行 `world.spawn((...))` 并可继续插组件 /
    /// 建层级（需要的场景数据以值捕获进闭包）。
    pub fn spawn_deferred(&self, spawn: impl FnOnce(&mut World) + Send + 'static) {
        self.lock().push(Command::Exec(Box::new(spawn)));
    }

    /// 入队「销毁实体」命令（已失效实体 no-op，世代安全）。
    pub fn despawn_deferred(&self, entity: Entity) {
        self.lock().push(Command::Despawn(entity));
    }

    /// 当前待执行命令数（诊断用；不消费）。
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// 队列是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 取走全部待执行命令（apply 的取件步骤：队列锁即刻释放，执行期间
    /// 新入队者留在队列、留给下一轮——单遍语义的实现基础）。
    pub fn take_commands(&self) -> Vec<Command> {
        std::mem::take(&mut *self.lock())
    }

    /// 独占执行：按入队序执行全部命令并清空。
    ///
    /// **调用警示**：执行期间闭包可能重新借队列资源（`world.command_queue()`
    /// 等）——调用方**不得**持有队列的 `ResourceRefMut` 借用；引擎入口
    /// [`apply_commands`] 已规避（先取走命令、释放资源锁，再逐条执行）。
    pub fn apply(&self, world: &mut World) {
        Self::execute(self.take_commands(), world);
    }

    /// 逐条执行（入队序 = 执行序，见模块文档「语义与确定性」）。
    fn execute(commands: Vec<Command>, world: &mut World) {
        for command in commands {
            match command {
                Command::Exec(exec) => exec(world),
                Command::Despawn(entity) => {
                    world.despawn(entity);
                }
            }
        }
    }

    /// 锁获取（毒锁自愈：闭包 panic 不应让整个队列永久不可用）。
    fn lock(&self) -> MutexGuard<'_, Vec<Command>> {
        self.commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 帧末独占系统：按入队序执行全部命令（`DefaultPlugins` /
/// `CommandsPlugin` 在 **PostUpdate 尾部**注册——渲染之前，本帧产出的
/// 命令本帧生效，渲染读到最终实体态）。
///
/// 实现顺序（单遍语义，见模块文档）：先经短命资源锁**取走**命令，锁
/// 随即释放；再逐条执行——执行期间闭包重新入队的命令进入队列资源、
/// 留给下一帧（不会在本轮二次处理，防闭包自激励无限循环）。
pub fn apply_commands(world: &mut World) {
    let commands = match world.try_resource::<CommandQueue>() {
        Some(queue) => queue.take_commands(),
        None => return, // 队列资源未注册：无命令可执行（自建 App 的缺席语义）
    };
    CommandQueue::execute(commands, world);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Component;
    use crate::transform::Transform;

    /// 执行序记录（并行系统不能 insert_resource，用组件承载结果）。
    #[derive(Component)]
    struct Log(Vec<u32>);

    #[test]
    fn commands_execute_in_enqueue_order() {
        let mut world = World::new();
        let entity = world.spawn((Log(vec![]),));
        let queue = CommandQueue::default();
        for value in [10u32, 20, 30] {
            queue.spawn_deferred(move |world: &mut World| {
                world.get_mut::<Log>(entity).unwrap().0.push(value);
            });
        }
        assert_eq!(queue.len(), 3, "入队后应有 3 条待执行命令");
        queue.apply(&mut world);
        assert_eq!(
            world.get::<Log>(entity).unwrap().0,
            vec![10, 20, 30],
            "命令应严格按入队序执行"
        );
        assert!(queue.is_empty(), "apply 后队列应为空");
    }

    #[test]
    fn despawn_deferred_removes_components_after_apply() {
        let mut world = World::new();
        let entity = world.spawn((Transform::IDENTITY,));
        let queue = CommandQueue::default();
        // apply 前组件仍可访问（延迟语义）
        assert!(world.get::<Transform>(entity).is_some());
        queue.despawn_deferred(entity);
        queue.apply(&mut world);
        assert!(
            world.get::<Transform>(entity).is_none(),
            "apply 后组件访问应为 None"
        );
        assert!(!world.contains(entity), "实体应已销毁");
    }

    #[test]
    fn despawn_of_dead_entity_is_noop() {
        let mut world = World::new();
        let entity = world.spawn((Transform::IDENTITY,));
        let queue = CommandQueue::default();
        queue.despawn_deferred(entity);
        queue.despawn_deferred(entity); // 同一实体入队两次
        queue.apply(&mut world);
        assert!(
            !world.contains(entity),
            "实体应恰被销毁一次（第二条 no-op）"
        );
        assert!(queue.is_empty());
    }

    /// 单遍语义：apply 执行期间闭包新入队的命令**不在本轮处理**，留给
    /// 下一轮 apply（防闭包自激励无限循环）。
    #[test]
    fn commands_enqueued_during_apply_wait_for_next_pass() {
        let mut world = World::new();
        let entity = world.spawn((Log(vec![]),));
        world.insert_resource(CommandQueue::default());
        world
            .try_resource::<CommandQueue>()
            .unwrap()
            .spawn_deferred(move |world: &mut World| {
                world.get_mut::<Log>(entity).unwrap().0.push(1);
                // 执行期间再入队一条（经资源路径，锁语义与世界路径一致）
                if let Some(queue) = world.try_resource::<CommandQueue>() {
                    queue.spawn_deferred(move |world: &mut World| {
                        world.get_mut::<Log>(entity).unwrap().0.push(2);
                    });
                }
            });
        // 第一轮：单遍——只执行最初入队的命令
        apply_commands(&mut world);
        assert_eq!(
            world.get::<Log>(entity).unwrap().0,
            vec![1],
            "单遍语义：本轮只执行最初入队的命令"
        );
        assert_eq!(
            world.resource::<CommandQueue>().len(),
            1,
            "执行期间入队的命令应留在队列资源中"
        );
        // 第二轮：处理上一轮入队的命令
        apply_commands(&mut world);
        assert_eq!(
            world.get::<Log>(entity).unwrap().0,
            vec![1, 2],
            "下一轮 apply 应执行上一轮入队的命令"
        );
    }

    /// 引擎入口：`apply_commands` 对未注册队列的世界是 no-op（自建 App
    /// 的缺席语义），且经资源路径取件（执行期间不持队列资源锁）。
    #[test]
    fn apply_commands_works_through_world_resource_path() {
        let mut world = World::new();
        // 未注册：no-op 不 panic
        apply_commands(&mut world);
        // 注册后：按序执行
        let entity = world.spawn((Log(vec![]),));
        world.insert_resource(CommandQueue::default());
        world
            .try_resource::<CommandQueue>()
            .unwrap()
            .spawn_deferred(move |world: &mut World| {
                world.get_mut::<Log>(entity).unwrap().0.push(7);
            });
        apply_commands(&mut world);
        assert_eq!(world.get::<Log>(entity).unwrap().0, vec![7]);
        assert!(
            world.resource::<CommandQueue>().is_empty(),
            "apply 后应清空"
        );
    }
}
