//! 系统调度：按阶段（Startup / FixedUpdate / Update / PostUpdate）执行系统。
//!
//! 同一阶段内系统按注册顺序组织为**批次**：
//! - 独占系统（`fn(&mut World)`）与一切冲突，独占一个批次（屏障语义）；
//! - 并行系统（`fn(&World)`）声明组件读写集（[`Access`]），与当前批次
//!   内全部系统无冲突时并入，否则开启新批次——批次间严格保持注册顺序，
//!   批次内系统在多个线程上并行执行（`std::thread::scope`）；
//! - 分片并行系统（`Fn(&World, &[Entity]) -> Vec<(Entity, T)>`，R73）：
//!   调度器把实体列表按固定连续区间切片，每片一次调用（只读世界、返回
//!   写缓冲）——**数据并行**，单个重系统也能吃满多核；join 后按稳定全序
//!   串行写回 ⇒ 结果与分片数 / 线程数 / 机器核数无关。
//!
//! 冲突判定是保守的类型级分析：只要一个系统声明写 `T`，任何读写 `T` 的
//! 其他系统都不能与它同批。因此运行期锁竞争必然是访问声明不诚实——
//! 存储层的尝试型借用检查会立即 panic（见 [`crate::sync`]），绝不死锁。

use std::any::{Any, TypeId};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::component::Component;
use crate::entity::Entity;
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

/// 分片并行的单分片最小规模（实体数下限，R73）。分片数到达上限时每片
/// 仍至少处理这么多实体，保证线程创建的微秒级开销被真实工作量摊薄
/// （1024 实体的分量遍历远贵于一次线程 spawn）。
pub const MIN_SHARD_ITEMS: usize = 1024;

/// 分片数纯函数：`items <= MIN_SHARD_ITEMS` 时不分片（单任务内联），
/// 否则取 `min(cores, items / MIN_SHARD_ITEMS)`（下限 1）。
///
/// 确定性要点：输出只依赖 (实体数, 核数) 两个入参——同一次执行内两者
/// 恒定，全部分片对片数的取值一致。核数跨机器不同只改变分片数，不改变
/// 结果（分片写集不相交，见 [`Schedule::add_sharded_system`]）。
pub fn shard_count_for(items: usize, cores: usize) -> usize {
    if items <= MIN_SHARD_ITEMS {
        1
    } else {
        cores.min(items / MIN_SHARD_ITEMS).max(1)
    }
}

/// 第 `shard` 片（共 `shards` 片）负责的实体区间 `[start, end)`：
/// 连续等分、尾片吸收余数，相邻片首尾相接（`start_{i+1} == end_i`）。
/// 纯函数——调度器与测试共用同一实现。
pub fn shard_range(shard: usize, shards: usize, items: usize) -> (usize, usize) {
    let start = shard * items / shards;
    let end = (shard + 1) * items / shards;
    (start, end)
}

/// 分片体的执行段：以实体区间切片调用用户闭包，返回类型擦除的写缓冲。
type ShardRunFn = dyn Fn(&World, &[Entity]) -> Box<dyn Any + Send> + Send + Sync;
/// 分片体的写回段：下行转换写缓冲到具体组件类型并按序插入。
type ShardApplyFn = dyn Fn(&mut World, Box<dyn Any + Send>) + Send + Sync;

/// 并行系统的执行体：整体执行（一次调用跑完，v1 形态）或分片执行
/// （每个实体区间各调一次，R73）。
///
/// 分片体的两段式由存储模型决定：组件列的写锁是**列级** RwLock
/// （`sync.rs`），分片线程并发写同一列必冲突——因此分片闭包只读世界、
/// 返回类型擦除的写缓冲（`Box<dyn Any + Send>`，实际是
/// `Vec<(Entity, T)>`），调度器在 join 后按稳定全序串行写回。`apply`
/// 是注册期合成的下行转换 + 写回闭包（泛型 `T` 在此处具象化）。
enum SystemBody {
    Whole(Box<dyn Fn(&World) + Send + Sync>),
    Sharded {
        run: Box<ShardRunFn>,
        apply: Box<ShardApplyFn>,
    },
}

/// 并行系统：只读世界（`&World`），通过世界内部的尝试型锁获得组件写访问；
/// 与其他并行系统的并发安全性由声明的 [`Access`] 决定。执行统一走
/// [`Schedule::run`] 的批执行器（整体体 = 单任务；分片体 = 按区间展开）。
pub struct ParallelSystem {
    pub name: &'static str,
    access: Access,
    body: SystemBody,
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
            body: SystemBody::Whole(Box::new(f)),
        }));
        self
    }

    /// 注册分片并行系统（R73）：调度器把当前世界的实体列表（稀疏集
    /// 插入序，同一世界状态下恒定）按固定连续区间切成
    /// [`shard_count_for`] 片，每片以不相交的实体子切片调用一次闭包，
    /// 单个重系统也能吃满多核（数据并行）。
    ///
    /// **执行模型（两段式）**：组件列的写锁是列级 RwLock（`sync.rs`），
    /// 分片线程并发写同一列必冲突——因此闭包**只读**世界，逐实体返回
    /// `(Entity, T)` 写缓冲；并行段结束后调度器按
    /// **成员序 × 分片序 × 缓冲序**（= 实体列表序的稳定全序）串行写回。
    /// 稳定全序 ⇒ 结果与分片数、线程数、机器核数无关——native 多线程
    /// 与 wasm 单线程（分片数退化为 1，内联路径）逐位一致。
    ///
    /// ```ignore
    /// // 典型形态：读 Vel，写 Pos（dt 为捕获的步长）
    /// schedule.add_sharded_system(
    ///     Stage::FixedUpdate,
    ///     Access::new().read::<Vel>().read::<Pos>(),
    ///     |world: &World, slice: &[Entity]| {
    ///         slice.iter().filter_map(|&e| {
    ///             let pos = world.get::<Pos>(e)?;
    ///             let vel = world.get::<Vel>(e)?;
    ///             Some((e, Pos(pos.0 + vel.0 * dt)))
    ///         }).collect()
    ///     },
    /// );
    /// ```
    ///
    /// **契约与诚实边界**：
    /// - 写组件 `T` 由签名给定并**自动并入** [`Access`] 写集（与其他系统
    ///   的冲突分批按「一方写 ∩ 另一方读写 ≠ ∅」照常判定）；`access`
    ///   参数声明其余只读访问；
    /// - 闭包对世界只读：并发 `get_mut` / 资源写会在运行期被尝试型锁
    ///   当场击穿（panic 消息含 "already borrowed"），绝不静默串数据；
    /// - 跨实体全局聚合（求和 / 求最值）请走两阶段：分片把部分结果作为
    ///   写缓冲落到本区间实体上，再由后续独立系统归并；
    /// - 写回段是串行的：**写密集、计算轻**的变换（纯拷贝级）分片收益
    ///   有限，此类场景更适合整体并行系统按列拆分；
    /// - 同一实体在缓冲中出现多次时按缓冲序重复插入（最后一次生效）——
    ///   请勿依赖此行为。
    pub fn add_sharded_system<T, F>(&mut self, stage: Stage, access: Access, f: F) -> &mut Self
    where
        T: Component,
        F: Fn(&World, &[Entity]) -> Vec<(Entity, T)> + Send + Sync + 'static,
    {
        let name = std::any::type_name::<F>();
        log::trace!("注册分片并行系统 {name} 到阶段 {stage:?}");
        // 写组件 T 自动并入访问集（分批口径必须覆盖闭包的实际写）
        let mut access = access;
        access.writes.push(TypeId::of::<T>());
        self.entry_mut(stage).push(Entry::Parallel(ParallelSystem {
            name,
            access,
            body: SystemBody::Sharded {
                run: Box::new(move |world: &World, slice: &[Entity]| {
                    Box::new(f(world, slice)) as Box<dyn Any + Send>
                }),
                apply: Box::new(|world: &mut World, buffer: Box<dyn Any + Send>| {
                    let buffer = buffer
                        .downcast::<Vec<(Entity, T)>>()
                        .expect("分片写缓冲类型错位（内部不变量破坏）");
                    for (entity, component) in *buffer {
                        world.insert(entity, component);
                    }
                }),
            },
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
                    run_batch_parallel(systems, &indices, world);
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

/// 并行批执行（R73 重构）：把批内成员展开成**任务列表**——普通并行系统
/// 1 个任务；分片系统按 [`shard_count_for`] 展开 W 个任务，各绑一个实体
/// 区间切片与一个结果槽。线程数取 `min(任务数, 可用核数)`，工作线程以
/// 原子计数器自取任务；单任务或可用线程数 ≤ 1 直接内联。
///
/// **两段式写回**：分片闭包只读世界、把写缓冲写入自己的结果槽（Mutex
/// 槽互不争用）；join 后按 **成员序 × 分片序 × 缓冲序**（= 实体列表序的
/// 稳定全序）串行写回——写回顺序确定 ⇒ 结果与分片数、线程数、机器核数
/// 无关，native 多线程与 wasm 单线程逐位一致。
///
/// 任务序 = 注册序 × 分片序——只约束内联路径的执行顺序；并行段内任务
/// 间无共享写（列级读锁并存、写全部延迟到写回段），任何顺序下结果一致。
///
/// 平台原因（wasm32）：std 线程在 wasm 上不可用——`thread::spawn` 运行时
/// panic（实测探针：`thread::scope` + spawn 1 线程即触发 unreachable 陷阱），
/// 且 `available_parallelism` 返回 `Err`（兜底为 1）。因此可用线程数 ≤ 1 时
/// 不进 scope、不 spawn，直接内联顺序执行（native 单核同样受益：避免
/// 无谓的线程创建，语义与顺序执行完全等价；分片数同步退化为 1）。
fn run_batch_parallel(systems: &[Entry], indices: &[usize], world: &mut World) {
    // 任务类型带生命周期参数：分片闭包借用本函数栈上的实体列表，
    // 不得默认 'static（Box<dyn ...> 省略生命周期时默认 'static）。
    type TaskFn<'a> = dyn Fn(&World) + Send + Sync + 'a;
    type ResultSlot = Mutex<Option<Box<dyn Any + Send>>>;

    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    // 三路平行集合（按注册序，每个分片成员一组）：实体列表（数据）、
    // 闭包体 run 引用、结果槽（W 个，与分片一一对应）。
    let mut shard_ents: Vec<Vec<Entity>> = Vec::new();
    let mut shard_runs: Vec<&ShardRunFn> = Vec::new();
    let mut shard_results: Vec<Vec<ResultSlot>> = Vec::new();
    for &i in indices {
        if let Entry::Parallel(parallel) = &systems[i] {
            if let SystemBody::Sharded { run, .. } = &parallel.body {
                let ents: Vec<Entity> = world.entities().collect();
                let shards = shard_count_for(ents.len(), cores);
                shard_ents.push(ents);
                shard_runs.push(&**run);
                shard_results.push((0..shards).map(|_| Mutex::new(None)).collect());
            }
        }
    }

    // 分片闭包从最终存储位置不可变借用（实体列表 / run / 结果槽），
    // 闭包组单独持有，避免穿过 &mut 的长寿命重借用。
    let mut shard_groups: Vec<Vec<Box<TaskFn<'_>>>> = Vec::with_capacity(shard_results.len());
    for ((slots, ents), run) in shard_results
        .iter()
        .zip(shard_ents.iter())
        .zip(shard_runs.iter().copied())
    {
        let shards = slots.len();
        let mut group: Vec<Box<TaskFn<'_>>> = Vec::with_capacity(shards);
        for (s, slot) in slots.iter().enumerate() {
            let (start, end) = shard_range(s, shards, ents.len());
            let slice = &ents[start..end];
            group.push(Box::new(move |world: &World| {
                let buffer = run(world, slice);
                *slot.lock().unwrap() = Some(buffer);
            }) as Box<TaskFn<'_>>);
        }
        shard_groups.push(group);
    }

    let mut groups = shard_groups.iter();
    let mut tasks: Vec<&TaskFn<'_>> = Vec::new();
    for &i in indices {
        match &systems[i] {
            Entry::Parallel(parallel) => match &parallel.body {
                SystemBody::Whole(f) => tasks.push(&**f),
                SystemBody::Sharded { .. } => {
                    let group = groups.next().expect("分片闭包组与分片成员一一对应");
                    tasks.extend(group.iter().map(|b| &**b));
                }
            },
            // 批内不会有独占系统（build_batches 保证），防御性兜底。
            Entry::Exclusive(_) => {}
        }
    }

    if tasks.len() <= 1 {
        // 单任务：内联执行（wasm / 单核 / 单系统小批，避免无谓线程创建）
        run_inline(&tasks, world);
    } else {
        let next = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..tasks.len().min(cores) {
                scope.spawn(|| {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        if index >= tasks.len() {
                            break;
                        }
                        tasks[index](world);
                    }
                });
            }
        });
    }

    // 写回段（串行，稳定全序）：成员序 × 分片序 × 缓冲序
    let mut member = 0usize;
    for &i in indices {
        if let Entry::Parallel(parallel) = &systems[i] {
            if let SystemBody::Sharded { apply, .. } = &parallel.body {
                for slot in &shard_results[member] {
                    let buffer = slot
                        .lock()
                        .unwrap()
                        .take()
                        .expect("分片未写入结果缓冲（内部不变量破坏）");
                    apply(world, buffer);
                }
                member += 1;
            }
        }
    }
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

    /// 分片测试用的载荷组件：f32 值 + 收敛变换（×1.0001 + 0.001，
    /// 与 benches 的 step_column 同款——反复应用收敛到定点，不溢出）。
    #[derive(Component)]
    struct Payload(f32);

    fn transform_payload(x: f32) -> f32 {
        x * 1.0001 + 0.001
    }

    /// 分片数与区间划分的精确边界：纯函数输出只依赖入参。
    #[test]
    fn shard_count_and_range_are_exact() {
        assert_eq!(shard_count_for(0, 16), 1);
        assert_eq!(shard_count_for(1024, 16), 1, "恰好下限不分片");
        assert_eq!(shard_count_for(1025, 16), 1, "1025/1024 = 1");
        assert_eq!(shard_count_for(2049, 16), 2);
        assert_eq!(shard_count_for(10_000, 2), 2);
        assert_eq!(
            shard_count_for(10_000, 64),
            9,
            "10000/1024 = 9，核数截断前先到分片上限"
        );
        assert_eq!(shard_count_for(100_000, 16), 16, "核数封顶");
        assert_eq!(shard_count_for(100_000, 1), 1, "单核退化为不分片");

        // 区间划分性质：连续、不相交、并集恰为 [0, items)，且每片非空
        let (shards, items) = (7, 10_000);
        let mut covered = 0;
        for s in 0..shards {
            let (start, end) = shard_range(s, shards, items);
            assert_eq!(start, covered, "片 {s} 首与前片尾相接");
            assert!(end > start, "片 {s} 非空");
            covered = end;
        }
        assert_eq!(covered, items, "并集恰为全域");
    }

    /// 分片执行与分片数无关（写回是稳定全序 ⇒ 顺序不可观测）：三个不同
    /// 规模的世界走真实调度路径（分片数随实体数变化，线程数随机器核数
    /// 变化），结果逐实体与「直接按实体序应用同一变换」的参考实现对账
    /// to_bits。
    #[test]
    fn sharded_system_matches_reference_regardless_of_shard_count() {
        for n in [3000usize, 9000, 30000] {
            let mut world = World::new();
            for i in 0..n {
                world.spawn((Payload(i as f32 + 0.25),));
            }
            let mut schedule = Schedule::new();
            schedule.add_sharded_system(
                Stage::Update,
                Access::new().read::<Payload>(),
                |world: &World, slice: &[Entity]| {
                    slice
                        .iter()
                        .filter_map(|&e| {
                            let p = world.get::<Payload>(e)?.0;
                            Some((e, Payload(transform_payload(p))))
                        })
                        .collect()
                },
            );
            schedule.run(Stage::Update, &mut world);

            // 参考实现：初值 i + 0.25（实体列表插入序恒定）应用同一变换
            for (i, e) in world.entities().enumerate() {
                let expected = transform_payload(i as f32 + 0.25);
                let actual = world.get::<Payload>(e).unwrap().0;
                assert_eq!(
                    actual.to_bits(),
                    expected.to_bits(),
                    "实体 {i} 值不符（n={n}，分片路径与参考实现偏离）"
                );
            }
        }
    }

    /// 分片系统与无冲突普通并行系统同批执行，两者效果都在；
    /// Access 冲突判定对分片成员与整体成员同口径。
    #[test]
    fn sharded_and_whole_systems_share_a_batch() {
        let mut world = World::new();
        let first = world.spawn((Payload(1.0), Observed(-1)));
        for i in 1..3000 {
            world.spawn((Payload(i as f32),));
        }

        let mut schedule = Schedule::new();
        schedule
            .add_sharded_system(
                Stage::Update,
                Access::new().read::<Payload>(),
                |world: &World, slice: &[Entity]| {
                    slice
                        .iter()
                        .filter_map(|&e| {
                            let p = world.get::<Payload>(e)?.0;
                            Some((e, Payload(transform_payload(p))))
                        })
                        .collect()
                },
            )
            .add_parallel_system(
                Stage::Update,
                Access::new().write::<Observed>(),
                |world: &World| {
                    let e = world.entities().next().unwrap();
                    world.get_mut::<Observed>(e).unwrap().0 = 42;
                },
            );

        let layout = schedule.batch_layout(Stage::Update);
        assert_eq!(layout.len(), 1, "无冲突应同批，实际 {layout:?}");
        schedule.run(Stage::Update, &mut world);
        assert_eq!(
            world.get::<Payload>(first).unwrap().0.to_bits(),
            transform_payload(1.0).to_bits(),
            "分片系统效果必须在"
        );
        assert_eq!(
            world.get::<Observed>(first).unwrap().0,
            42,
            "同批并行系统效果必须在"
        );
    }

    /// 分片系统与冲突并行系统异批保序：写者（分片）先、读者后，
    /// 读者看到写者的完整结果——分片成员的 Access 按整体参与分批。
    #[test]
    fn sharded_system_conflicts_serialize_in_order() {
        let mut world = World::new();
        let first = world.spawn((Payload(2.0), Observed(-1)));
        for i in 1..3000 {
            world.spawn((Payload(i as f32),));
        }

        let mut schedule = Schedule::new();
        schedule
            .add_sharded_system(
                Stage::Update,
                Access::new().read::<Payload>(),
                |world: &World, slice: &[Entity]| {
                    slice
                        .iter()
                        .filter_map(|&e| {
                            let p = world.get::<Payload>(e)?.0;
                            Some((e, Payload(transform_payload(p))))
                        })
                        .collect()
                },
            )
            .add_parallel_system(
                Stage::Update,
                Access::new().read::<Payload>().write::<Observed>(),
                |world: &World| {
                    let e = world.entities().next().unwrap();
                    let value = world.get::<Payload>(e).unwrap().0;
                    world.get_mut::<Observed>(e).unwrap().0 = value.to_bits() as i32;
                },
            );

        let layout = schedule.batch_layout(Stage::Update);
        assert_eq!(layout.len(), 2, "读写冲突应异批，实际 {layout:?}");
        schedule.run(Stage::Update, &mut world);
        assert_eq!(
            world.get::<Observed>(first).unwrap().0,
            transform_payload(2.0).to_bits() as i32,
            "读者必须看到分片写者的完整结果"
        );
    }
}
