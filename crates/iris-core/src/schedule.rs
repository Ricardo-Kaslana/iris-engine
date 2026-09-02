//! 系统调度：按阶段（Startup / FixedUpdate / Update / PostUpdate）执行系统。
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
    /// 后更新（层级传播等收尾系统），每帧一次且**不在 FixedUpdate
    /// 循环内**。编排：Update 写局部变换 → PostUpdate 传播 → 渲染读取
    /// ——用户 Update 系统写入的变换当帧即完成全局传播，无须关心
    /// 自己与 HierarchyPlugin 的注册顺序（dogfooding 缺口：无系统排序
    /// API 时插件无法排到传播之前，见 §8.12）。
    PostUpdate,
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
        Self {
            name,
            f: Box::new(f),
        }
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
        Self {
            name: std::any::type_name::<F>(),
            f: Box::new(f),
        }
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
        self.writes
            .iter()
            .any(|w| other.reads.contains(w) || other.writes.contains(w))
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
        f.debug_struct("ParallelSystem")
            .field("name", &self.name)
            .finish()
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
    post_update: Vec<Entry>,
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
    pub fn add_parallel_system<F>(&mut self, stage: Stage, access: Access, f: F) -> &mut Self
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
            Stage::PostUpdate => &mut self.post_update,
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
            Stage::PostUpdate => self.post_update.len(),
        }
    }

    /// 某阶段各批的系统数（诊断 / 基准入口，R33）。
    ///
    /// 对当前注册系统跑一遍贪心分批（[`Self::build_batches`]），返回每批
    /// 的成员数（独占批恒为 1）——批次划分本身的只读视图，**不执行任何
    /// 系统**。criterion 基准 `schedule_build_batches` 用它测量「30 个
    /// 系统读写集的冲突分批」的纯构建开销。
    pub fn batch_sizes(&self, stage: Stage) -> Vec<usize> {
        Self::build_batches(self.entry(stage))
            .into_iter()
            .map(|batch| match batch {
                Batch::Solo(_) => 1,
                Batch::Parallel(members) => members.len(),
            })
            .collect()
    }

    /// 某阶段的批次划分（测试与诊断用：各批 (是否并行, 系统名)）。
    #[cfg(test)]
    fn batch_layout(&self, stage: Stage) -> Vec<(bool, Vec<&'static str>)> {
        let systems = self.entry(stage);
        Self::build_batches(systems)
            .into_iter()
            .map(|batch| match batch {
                Batch::Solo(i) => (false, vec![Self::name_of(&systems[i])]),
                Batch::Parallel(v) => (
                    true,
                    v.iter().map(|&i| Self::name_of(&systems[i])).collect(),
                ),
            })
            .collect()
    }

    fn entry(&self, stage: Stage) -> &Vec<Entry> {
        match stage {
            Stage::Startup => &self.startup,
            Stage::FixedUpdate => &self.fixed,
            Stage::Update => &self.update,
            Stage::PostUpdate => &self.post_update,
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
///
/// 平台原因（wasm32）：std 线程在 wasm 上不可用——`thread::spawn` 运行时
/// panic（实测探针：`thread::scope` + spawn 1 线程即触发 unreachable 陷阱），
/// 且 `available_parallelism` 返回 `Err`（兜底为 1）。因此可用线程数 ≤ 1 时
/// 不进 scope、不 spawn，直接内联顺序执行（native 单核同样受益：避免
/// 无谓的线程创建，语义与顺序执行完全等价）。
fn run_parallel(systems: &[Entry], indices: &[usize], world: &World) {
    let fns: Vec<&(dyn Fn(&World) + Send + Sync)> = indices
        .iter()
        .filter_map(|&i| match &systems[i] {
            Entry::Parallel(parallel) => Some(&*parallel.f),
            Entry::Exclusive(_) => None,
        })
        .collect();
    let threads = fns.len().min(
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
    );
    if threads <= 1 {
        // 单线程可用：内联顺序执行（wasm 无 std 线程 / native 单核）
        run_inline(&fns, world);
        return;
    }
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

/// 顺序执行一批并行系统（单线程 fallback 路径，注册顺序保序）。
fn run_inline(fns: &[&(dyn Fn(&World) + Send + Sync)], world: &World) {
    for f in fns {
        f(world);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Component, Resource};
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    #[derive(Component)]
    struct Counter(i32);
    #[derive(Component)]
    struct MarkerA;
    #[derive(Component)]
    struct MarkerB;
    /// 读者系统的观测记录（并行系统不能 insert_resource，用组件承载结果）。
    #[derive(Component)]
    struct Observed(i32);

    #[test]
    fn systems_run_in_registration_order() {
        let mut world = World::new();
        let entity = world.spawn((Counter(0),));

        fn add_ten(world: &mut World) {
            let mut counter = world
                .get_mut::<Counter>(world.entities().next().unwrap())
                .unwrap();
            counter.0 += 10;
        }
        let add_hundred = |world: &mut World| {
            let mut counter = world
                .get_mut::<Counter>(world.entities().next().unwrap())
                .unwrap();
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

    /// 阶段编排契约：PostUpdate 在 Update 之后执行（App 主循环按
    /// FixedUpdate × N → Update → PostUpdate 推进，见 app.rs）。
    /// Update 写局部状态，PostUpdate 收尾读取——层级传播的新家。
    #[test]
    fn post_update_runs_after_update() {
        let mut world = World::new();
        let entity = world.spawn((Counter(0), Observed(-1)));

        let mut schedule = Schedule::new();
        schedule
            .add_system(Stage::Update, |world: &mut World| {
                let e = world.entities().next().unwrap();
                world.get_mut::<Counter>(e).unwrap().0 = 7;
            })
            .add_system(Stage::PostUpdate, |world: &mut World| {
                let e = world.entities().next().unwrap();
                let value = world.get::<Counter>(e).unwrap().0;
                world.get_mut::<Observed>(e).unwrap().0 = value;
            });

        assert_eq!(schedule.system_count(Stage::PostUpdate), 1);
        schedule.run(Stage::PostUpdate, &mut world);
        // 单独跑 PostUpdate 时 Update 未执行：观察到的是 Counter 初值（先决条件）
        assert_eq!(world.get::<Observed>(entity).unwrap().0, 0);
        schedule.run(Stage::Update, &mut world);
        schedule.run(Stage::PostUpdate, &mut world);
        assert_eq!(
            world.get::<Observed>(entity).unwrap().0,
            7,
            "PostUpdate 应读到 Update 的结果"
        );
    }

    fn spawn_two_markers(world: &mut World) {
        world.spawn((MarkerA,));
        world.spawn((MarkerB,));
    }

    /// 无冲突并行系统确实并发执行：两系统各自把「当前并发数」
    /// 抬到原子计数器上，观测到的最大并发数应 ≥ 2。
    /// 高负载下线程调度可能错过交错窗口（两个 sleep 不重叠），
    /// 因此允许重跑数轮取观测峰值——峰值只增不减，跨轮累积。
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
        for _ in 0..5 {
            if MAX_ACTIVE.load(Ordering::SeqCst) >= 2 {
                break;
            }
            schedule.run(Stage::Update, &mut world);
        }
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
        assert_eq!(
            layout.len(),
            3,
            "应为 [并行AB, 独占, 并行A]，实际 {layout:?}"
        );
        assert_eq!(layout[0], (true, vec![layout[0].1[0], layout[0].1[1]]));
        assert!(!layout[1].0, "第二批应为独占系统");
        assert!(
            !layout[2].0 || layout[2].1.len() == 1,
            "第三批应只含最后一个系统"
        );
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

    /// 单线程可用时并行系统仍全部执行：run_parallel 的内联 fallback 路径
    /// （wasm 无 std 线程时 threads 兜底为 1，落到 run_inline）按注册顺序
    /// 执行批内全部系统。直接驱动 run_inline 验证批处理结果。
    #[test]
    fn single_thread_fallback_runs_all_systems_in_order() {
        let mut world = World::new();
        let a = world.spawn((MarkerA, Counter(0)));
        let b = world.spawn((MarkerB, Counter(0)));

        // 两个互不冲突的系统：各自的 Counter += 自己的步长，并把执行序
        // 写进自己实体的 Observed（0 = 先执行，1 = 后执行）。
        let order = std::sync::atomic::AtomicUsize::new(0);
        let record = |world: &World, e: iris_core::entity::Entity| {
            let n = order.fetch_add(1, Ordering::SeqCst) as i32;
            world.get_mut::<Observed>(e).unwrap().0 = n;
        };
        let sys_a = |world: &World| {
            let e = world.entities().next().unwrap();
            world.get_mut::<Counter>(e).unwrap().0 += 1;
            record(world, e);
        };
        let sys_b = |world: &World| {
            let e = world.entities().nth(1).unwrap();
            world.get_mut::<Counter>(e).unwrap().0 += 2;
            record(world, e);
        };
        let fns: Vec<&(dyn Fn(&World) + Send + Sync)> = vec![&sys_a, &sys_b];
        world.insert(a, Observed(-1));
        world.insert(b, Observed(-1));

        run_inline(&fns, &world);

        // 全部系统都执行了，且批内顺序 = 注册顺序
        assert_eq!(world.get::<Counter>(a).unwrap().0, 1, "系统一必须执行");
        assert_eq!(world.get::<Counter>(b).unwrap().0, 2, "系统二必须执行");
        assert_eq!(world.get::<Observed>(a).unwrap().0, 0, "系统一先执行");
        assert_eq!(world.get::<Observed>(b).unwrap().0, 1, "系统二后执行");
    }
}
