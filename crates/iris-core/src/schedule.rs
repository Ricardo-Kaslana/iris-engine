//! 系统调度：按阶段（Startup / FixedUpdate / Update）执行系统。
//!
//! 同一阶段内系统按注册顺序组织为**批次**：
//! - 独占系统（`fn(&mut World)`）与一切冲突，独占一个批次（屏障语义）；
//! - 并行系统（`fn(&World)`）声明组件读写集（[`Access`]），与当前批次
//!   内全部系统无冲突时并入，否则开启新批次——批次间严格保持注册顺序，
//!   批次内系统在多个线程上并行执行（`std::thread::scope`）。
//!
//! 冲突判定是保守的类型级分析：只要一个系统声明写 `T`，任何读写 `T` 的
//! 其他系统都不能与它同批。因此运行期锁竞争必然是访问声明不诚实——
//! 存储层的尝试型借用检查会立即 panic（见 [`crate::sync`]），绝不死锁。

use std::any::TypeId;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::world::World;

/// 执行阶段。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    /// 进入主循环前执行一次（资源初始化、场景搭建）。
    Startup,
    /// 固定步长逻辑（物理、确定性模拟），每帧可能执行 0..N 次。
    FixedUpdate,
    /// 可变步长更新（输入、AI、动画），每帧一次。
    Update,
}

/// 系统：消费整个世界的一次性闭包或函数（独占访问，串行执行）。
///
/// 函数与闭包均可：`fn(&mut World)` 自动转换。
pub struct System {
    pub name: &'static str,
    f: Box<dyn FnMut(&mut World) + Send>,
}

impl System {
    pub fn new<F>(name: &'static str, f: F) -> Self
    where
        F: FnMut(&mut World) + Send + 'static,
    {
        Self { name, f: Box::new(f) }
    }

    pub fn run(&mut self, world: &mut World) {
        (self.f)(world);
    }
}

impl<F> From<F> for System
where
    F: FnMut(&mut World) + Send + 'static,
{
    fn from(f: F) -> Self {
        Self { name: std::any::type_name::<F>(), f: Box::new(f) }
    }
}

impl std::fmt::Debug for System {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("System").field("name", &self.name).finish()
    }
}

/// 并行系统的组件访问集：冲突分批的依据。
///
/// ```ignore
/// Access::new().read::<Time>().write::<Transform>()
/// ```
#[derive(Clone, Default, Debug)]
pub struct Access {
    reads: Vec<TypeId>,
    writes: Vec<TypeId>,
}

impl Access {
    pub fn new() -> Self {
        Self::default()
    }

    /// 声明只读访问某组件 / 资源类型。
    pub fn read<T: 'static>(mut self) -> Self {
        self.reads.push(TypeId::of::<T>());
        self
    }

    /// 声明可写访问某组件 / 资源类型。
    pub fn write<T: 'static>(mut self) -> Self {
        self.writes.push(TypeId::of::<T>());
        self
    }

    /// 两个访问集是否冲突（一方写 ∩ 另一方读写 ≠ ∅）。
    fn conflicts(&self, other: &Access) -> bool {
        self.writes.iter().any(|w| other.reads.contains(w) || other.writes.contains(w))
            || other.writes.iter().any(|w| self.reads.contains(w))
    }

    fn union_with(&mut self, other: &Access) {
        self.reads.extend(other.reads.iter().copied());
        self.writes.extend(other.writes.iter().copied());
    }
}

/// 并行系统：只读世界（`&World`），通过世界内部的尝试型锁获得组件写访问；
/// 与其他并行系统的并发安全性由声明的 [`Access`] 决定。
pub struct ParallelSystem {
    pub name: &'static str,
    access: Access,
    f: Box<dyn Fn(&World) + Send + Sync>,
}

impl ParallelSystem {
    pub fn run(&self, world: &World) {
        (self.f)(world);
    }
}

impl std::fmt::Debug for ParallelSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParallelSystem").field("name", &self.name).finish()
    }
}

/// 阶段内的调度单元：独占系统或并行系统。
enum Entry {
    Exclusive(System),
    Parallel(ParallelSystem),
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exclusive(s) => f.debug_struct("Exclusive").field("name", &s.name).finish(),
            Self::Parallel(s) => f.debug_struct("Parallel").field("name", &s.name).finish(),
        }
    }
}

/// 一个批次：独占（单系统，屏障）或并行（多个无冲突系统）。
#[derive(Debug)]
enum Batch {
    Solo(usize),
    Parallel(Vec<usize>),
}

#[derive(Default)]
pub struct Schedule {
    startup: Vec<Entry>,
    fixed: Vec<Entry>,
    update: Vec<Entry>,
}

impl Schedule {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册独占系统到指定阶段。相同阶段内按注册顺序执行（保守串行路径，
    /// 与并行系统之间形成屏障）。
    pub fn add_system(&mut self, stage: Stage, system: impl Into<System>) -> &mut Self {
        let system = system.into();
        log::trace!("注册独占系统 {} 到阶段 {stage:?}", system.name);
        self.entry_mut(stage).push(Entry::Exclusive(system));
        self
    }

    /// 注册并行系统：声明组件读写集后，调度器将其与同阶段其他并行系统
    /// 按冲突分批，无冲突者并行执行。系统内不得做结构性修改
    /// （spawn / despawn / insert_resource 需要 `&mut World`，类型系统已排除）。
    pub fn add_parallel_system<F>(
        &mut self,
        stage: Stage,
        access: Access,
        f: F,
    ) -> &mut Self
    where
        F: Fn(&World) + Send + Sync + 'static,
    {
        let name = std::any::type_name::<F>();
        log::trace!("注册并行系统 {name} 到阶段 {stage:?}");
        self.entry_mut(stage).push(Entry::Parallel(ParallelSystem {
            name,
            access,
            f: Box::new(f),
        }));
        self
    }

    fn entry_mut(&mut self, stage: Stage) -> &mut Vec<Entry> {
        match stage {
            Stage::Startup => &mut self.startup,
            Stage::FixedUpdate => &mut self.fixed,
            Stage::Update => &mut self.update,
        }
    }

    /// 把阶段内系统按注册顺序贪心分批：与当前批次全部成员无冲突的
    /// 并行系统并入当前批，否则开启新批；独占系统独占批（屏障）。
    fn build_batches(systems: &[Entry]) -> Vec<Batch> {
        let mut batches = Vec::new();
        let mut current: Vec<usize> = Vec::new();
        let mut current_access = Access::default();
        for (index, entry) in systems.iter().enumerate() {
            match entry {
                Entry::Exclusive(_) => {
                    if !current.is_empty() {
                        batches.push(Batch::Parallel(std::mem::take(&mut current)));
                        current_access = Access::default();
                    }
                    batches.push(Batch::Solo(index));
                }
                Entry::Parallel(parallel) => {
                    if current.is_empty() || !current_access.conflicts(&parallel.access) {
                        current_access.union_with(&parallel.access);
                        current.push(index);
                    } else {
                        batches.push(Batch::Parallel(std::mem::take(&mut current)));
                        current_access = parallel.access.clone();
                        current.push(index);
                    }
                }
            }
        }
        if !current.is_empty() {
            batches.push(Batch::Parallel(current));
        }
        batches
    }

    /// 执行某阶段全部系统（批次间顺序、批内并行）。
    pub fn run(&mut self, stage: Stage, world: &mut World) {
        let systems = self.entry_mut(stage);
        let batches = Self::build_batches(systems);
        for batch in batches {
            match batch {
                Batch::Solo(index) => {
                    if let Entry::Exclusive(system) = &mut systems[index] {
                        system.run(world);
                    }
                }
                Batch::Parallel(indices) => {
                    if indices.len() == 1 {
                        if let Entry::Parallel(parallel) = &systems[indices[0]] {
                            parallel.run(world);
                        }
                        continue;
                    }
                    run_parallel(systems, &indices, world);
                }
            }
        }
    }

    pub fn system_count(&self, stage: Stage) -> usize {
        match stage {
            Stage::Startup => self.startup.len(),
            Stage::FixedUpdate => self.fixed.len(),
            Stage::Update => self.update.len(),
        }
    }

    /// 某阶段的批次划分（测试与诊断用：各批 (是否并行, 系统名)）。
    #[cfg(test)]
    fn batch_layout(&self, stage: Stage) -> Vec<(bool, Vec<&'static str>)> {
        let systems = self.entry(stage);
        Self::build_batches(systems)
            .into_iter()
            .map(|batch| match batch {
                Batch::Solo(i) => (false, vec![Self::name_of(&systems[i])]),
                Batch::Parallel(v) => {
                    (true, v.iter().map(|&i| Self::name_of(&systems[i])).collect())
                }
            })
            .collect()
    }

    #[cfg(test)]
    fn entry(&self, stage: Stage) -> &Vec<Entry> {
        match stage {
            Stage::Startup => &self.startup,
            Stage::FixedUpdate => &self.fixed,
            Stage::Update => &self.update,
        }
    }

    #[cfg(test)]
    fn name_of(entry: &Entry) -> &'static str {
        match entry {
            Entry::Exclusive(s) => s.name,
            Entry::Parallel(s) => s.name,
        }
    }
}

/// 并行批次执行：线程数取 min(系统数, 可用核数)，
/// 工作线程以原子计数器自取任务，全部完成后 scope 返回。
fn run_parallel(systems: &[Entry], indices: &[usize], world: &World) {
    let fns: Vec<&(dyn Fn(&World) + Send + Sync)> = indices
        .iter()
        .filter_map(|&i| match &systems[i] {
            Entry::Parallel(parallel) => Some(&*parallel.f),
            Entry::Exclusive(_) => None,
        })
        .collect();
    let threads = fns
        .len()
        .min(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1));
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= fns.len() {
                        break;
                    }
                    fns[index](world);
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Component, Resource};
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    #[derive(Component)] struct Counter(i32);
    #[derive(Component)] struct MarkerA;
    #[derive(Component)] struct MarkerB;
    /// 读者系统的观测记录（并行系统不能 insert_resource，用组件承载结果）。
    #[derive(Component)] struct Observed(i32);

    #[test]
    fn systems_run_in_registration_order() {
        let mut world = World::new();
        let entity = world.spawn((Counter(0),));

        fn add_ten(world: &mut World) {
            let mut counter = world.get_mut::<Counter>(world.entities().next().unwrap()).unwrap();
            counter.0 += 10;
        }
        let add_hundred = |world: &mut World| {
            let mut counter = world.get_mut::<Counter>(world.entities().next().unwrap()).unwrap();
            counter.0 += 100;
        };

        let mut schedule = Schedule::new();
        schedule
            .add_system(Stage::Update, add_hundred)
            .add_system(Stage::Update, add_ten);
        assert_eq!(schedule.system_count(Stage::Update), 2);
        schedule.run(Stage::Update, &mut world);
        assert_eq!(world.get::<Counter>(entity).unwrap().0, 110);
    }

    #[test]
    fn startup_runs_once_via_explicit_call() {
        #[derive(Resource)]
        struct Stamp(u32);

        let mut world = World::new();
        let mut schedule = Schedule::new();
        schedule.add_system(Stage::Startup, |world: &mut World| {
            world.insert_resource(Stamp(0));
        });
        schedule.run(Stage::Startup, &mut world);
        assert_eq!(world.resource::<Stamp>().0, 0);
    }

    fn spawn_two_markers(world: &mut World) {
        world.spawn((MarkerA,));
        world.spawn((MarkerB,));
    }

    /// 无冲突并行系统确实并发执行：两系统各自把「当前并发数」
    /// 抬到原子计数器上，观测到的最大并发数应 ≥ 2。
    #[test]
    fn disjoint_parallel_systems_run_concurrently() {
        static ACTIVE: AtomicUsize = AtomicUsize::new(0);
        static MAX_ACTIVE: AtomicUsize = AtomicUsize::new(0);

        fn enter() {
            let now = ACTIVE.fetch_add(1, Ordering::SeqCst) + 1;
            MAX_ACTIVE.fetch_max(now, Ordering::SeqCst);
            // 交错窗口：让对方有时间进入
            std::thread::sleep(Duration::from_millis(30));
        }
        fn leave() {
            ACTIVE.fetch_sub(1, Ordering::SeqCst);
        }

        let mut world = World::new();
        spawn_two_markers(&mut world);

        let mut schedule = Schedule::new();
        schedule
            .add_parallel_system(
                Stage::Update,
                Access::new().write::<MarkerA>(),
                |_: &World| {
                    enter();
                    leave();
                },
            )
            .add_parallel_system(
                Stage::Update,
                Access::new().write::<MarkerB>(),
                |_: &World| {
                    enter();
                    leave();
                },
            );
        schedule.run(Stage::Update, &mut world);
        assert!(
            MAX_ACTIVE.load(Ordering::SeqCst) >= 2,
            "两个无冲突系统应并发执行（观测最大并发 {}）",
            MAX_ACTIVE.load(Ordering::SeqCst)
        );
    }

    /// 读写冲突的系统分属不同批次，注册顺序（写者先、读者后）得到保持。
    #[test]
    fn conflicting_systems_serialize_in_order() {
        let mut world = World::new();
        let entity = world.spawn((Counter(0), Observed(-1)));

        let mut schedule = Schedule::new();
        schedule
            .add_parallel_system(
                Stage::Update,
                Access::new().write::<Counter>(),
                |world: &World| {
                    let e = world.entities().next().unwrap();
                    world.get_mut::<Counter>(e).unwrap().0 += 100;
                },
            )
            .add_parallel_system(
                Stage::Update,
                Access::new().read::<Counter>().write::<Observed>(),
                |world: &World| {
                    let e = world.entities().next().unwrap();
                    let value = world.get::<Counter>(e).unwrap().0;
                    world.get_mut::<Observed>(e).unwrap().0 = value;
                },
            );
        schedule.run(Stage::Update, &mut world);
        // 读者必须看到写者的结果（分批保序），而非 -1
        assert_eq!(world.get::<Observed>(entity).unwrap().0, 100);
        assert_eq!(world.get::<Counter>(entity).unwrap().0, 100);
    }

    /// 批次划分：无冲突者同批、读写冲突者异批、独占系统成屏障。
    #[test]
    fn batch_layout_reflects_conflicts() {
        let mut schedule = Schedule::new();
        fn exclusive(_: &mut World) {}
        let echo = |_: &World| {};
        schedule
            .add_parallel_system(Stage::Update, Access::new().write::<MarkerA>(), echo)
            .add_parallel_system(Stage::Update, Access::new().write::<MarkerB>(), echo)
            .add_system(Stage::Update, exclusive)
            .add_parallel_system(Stage::Update, Access::new().write::<MarkerA>(), echo);

        let layout = schedule.batch_layout(Stage::Update);
        assert_eq!(layout.len(), 3, "应为 [并行AB, 独占, 并行A]，实际 {layout:?}");
        assert_eq!(layout[0], (true, vec![layout[0].1[0], layout[0].1[1]]));
        assert!(!layout[1].0, "第二批应为独占系统");
        assert!(!layout[2].0 || layout[2].1.len() == 1, "第三批应只含最后一个系统");
    }

    /// 独占系统屏障：前一批并行的效果对后续系统可见。
    #[test]
    fn exclusive_system_acts_as_barrier() {
        let mut world = World::new();
        let entity = world.spawn((MarkerA, Counter(0)));
        let mut schedule = Schedule::new();
        schedule
            .add_parallel_system(
                Stage::Update,
                Access::new().write::<MarkerA>().write::<Counter>(),
                |world: &World| {
                    let e = world.entities().next().unwrap();
                    world.get_mut::<Counter>(e).unwrap().0 += 1;
                },
            )
            .add_system(Stage::Update, |world: &mut World| {
                let e = world.entities().next().unwrap();
                world.get_mut::<Counter>(e).unwrap().0 *= 10;
            });
        schedule.run(Stage::Update, &mut world);
        // 并行系统 +1，屏障后独占系统 ×10：结果证明两步严格按序发生
        assert_eq!(world.get::<Counter>(entity).unwrap().0, 10);
    }
}
