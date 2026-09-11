//! 行为树：**数据驱动节点 + 逐代理黑板**（R65）。
//!
//! 敌人 AI 的游戏侧手写状态机在能力变多后必然爆炸（状态 × 转移的笛卡尔
//! 增长，最初评估的第二个瓶颈）。本模块给出引擎级的最小行为树框架：
//! **结构是纯数据**（可序列化、可测试、结构即文档），叶子的具体实现由
//! 游戏侧按**具名动作 id** 分发——引擎不认识任何游戏动作。
//!
//! # 表示：嵌套规格 → 扁平竞技场（无 Box<dyn>）
//!
//! - 游戏侧用 [`NodeSpec`]（嵌套、可读）写树；[`BehaviorTree::new`] 把它
//!   **前序展开**成 [`Node`] 扁平数组，子节点只存 [`NodeId`]（下标）——
//!   运行期零动态分发、遍历期零分配，节点 id 在构造期一次定死；
//! - 复合节点的 `children` 是 **Vec，遍历序 = 声明序**（下标升序），没有
//!   任何哈希容器参与决策；需要按键序迭代的黑板用 `BTreeMap`（选型理由
//!   照 `broadphase.rs`：迭代序确定 = 确定性的一部分）。
//!
//! # tick 模型：带记忆（Running 恢复）
//!
//! 每帧每个代理 tick 一次（`dt` 由调用方统一给出，通常来自 [`crate::time::Time`]）。
//! 每个节点在 [`BehaviorState`] 里有一份运行态记忆；**上帧返回 `Running` 的
//! 节点下帧从记忆处恢复**（复合节点记「当前子节点游标」，`Repeat` 记已完成
//! 次数，`Cooldown` 记剩余冷却）——不会从头重新评估。逐节点语义：
//!
//! | 节点 | 成功条件 | 失败条件 | 记忆 |
//! |---|---|---|---|
//! | `Sequence` | 全部子节点成功（空 = 成功） | 任一失败即**短路** | 当前子游标 |
//! | `Selector` | 任一成功即**短路**（空 = 失败） | 全部失败 | 当前子游标 |
//! | `Parallel` | 本轮成功数 ≥ `success_threshold` | 本轮失败数 ≥ `failure_threshold`（**失败优先**） | 已终结子位图 + 本轮计数 |
//! | `Inverter` | 子失败 | 子成功 | 透传（子自行记忆） |
//! | `Repeat` | 子成功 `times` 次 | 子任一失败即短路并清计数 | 已完成次数 |
//! | `Cooldown` | 子成功（并进入冷却） | 子失败 / **冷却中恒失败且不 tick 子** | 剩余冷却秒数 |
//! | `Condition` | 谓词为真 | 谓词为假 | 无 |
//! | `Action` | 由游戏侧返回 | 由游戏侧返回 | 由游戏侧返回 `Running` 表达 |
//!
//! `Parallel` 的「轮」= 从非 `Running` 重新开始的一次评估；一旦达到任一
//! 阈值，本轮立即结束（未 tick 的子节点本轮不再 tick），下帧开新一轮。同帧
//! 同时满足两个阈值时判 **Failure**（失败优先，可预期）。`Cooldown` 的计时
//! 在**本次 tick 递减**：归零的当帧仍返回失败，下一帧恢复子节点
//! （「冷却 ≥ seconds 秒」的保守语义）。
//!
//! # 黑板：[`Blackboard`]（键值，逐代理）
//!
//! 值域 [`BlackValue`] 五型（Bool / Float / Entity / Vec3 / Text）——**够用
//! 即止**：无类型魔法、无反射、无字符串求值。感知结果与动作间通信走黑板
//! （「唯一通道」的纪律）；遍历序 = `BTreeMap` 键序（UTF-8 字节序）。类型
//! 不匹配的读取返回 `None`（不 panic、不隐式转换）——诚实而可预测。
//!
//! # 生命周期（状态保持与清理）
//!
//! 树运行态与黑板都是**逐代理**的：代理实体 despawn 时随组件一起消亡
//! （[`BehaviorAgent`] = 树 + 运行态 + 黑板三合一组件），无需任何手工清理；
//! **不做跨代理共享黑板**（拒绝清单：共享状态会让确定性论证与生命周期
//! 双双失控——跨代理通信请走事件，见 [`crate::event`]）。
//!
//! # 系统接入（Update 阶段）
//!
//! 引擎不认识游戏动作，故不为行为树提供 `DefaultPlugins` 内置系统，而是
//! 提供系统体 [`tick_behavior_agents`]：游戏侧按既有注册序惯例挂到
//! `Stage::Update`（并行系统，访问集 `write::<BehaviorAgent>()`）：
//!
//! ```ignore
//! schedule.add_parallel_system(
//!     Stage::Update,
//!     Access::new().write::<BehaviorAgent>().read::<Transform>(),
//!     |world| {
//!         let dt = world.try_resource::<Time>().map(|t| t.delta_secs()).unwrap_or(0.0);
//!         tick_behavior_agents(world, dt, |entity, world| GameCtx::new(entity, world));
//!     },
//! );
//! ```
//!
//! # 诚实限制清单（v1）
//!
//! - **不做**：GOAP / 效用 AI / 层级 FSM、跨代理共享黑板、子树引用与热重载、
//!   可视化调试器与编辑器集成、树序列化入关卡档（结构是纯数据，序列化留
//!   后续轮）、装饰器全集（只做 Inverter / Cooldown / Repeat 三个）、黑板键
//!   的编译期校验（键是运行时字符串）；
//! - `Condition` 只读黑板（有状态的判断请写成 `Action`）；
//! - 无节点级「超时 / 重试预算」装饰器，需要时用 `Cooldown` + 黑板计时；
//! - 树不可变：运行期换树 = 换组件（运行态与树必须同源，见
//!   [`BehaviorTree::tick`] 的同源断言）；
//! - 无分支级线程并行（`Parallel` 是**语义**并行，帧内仍按声明序串行 tick）。
//!
//! # 确定性论证
//!
//! 同输入（树 + 运行态 + 黑板 + `dt` + 上下文返回值）两次 tick 逐位一致：
//! ①遍历序 = `Vec` 下标升序（声明序），无哈希容器参与决策；②黑板键序 =
//! `BTreeMap`；③浮点只在 `Cooldown` 里做「减法 + `max(0.0)`」，运算序固定；
//! ④无全局状态、无随机源、无时钟读取（`dt` 由调用方给）。

use std::collections::BTreeMap;

use glam::Vec3;

use crate::Component;
use crate::entity::Entity;
use crate::world::World;
// ---------------------------------------------------------------------------
// 状态 / 具名动作 id
// ---------------------------------------------------------------------------

/// 节点 tick 结果（三值逻辑）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BtStatus {
    /// 成功（本轮结束）。
    Success,
    /// 失败（本轮结束）。
    Failure,
    /// 仍在进行（下帧从记忆处恢复）。
    Running,
}

/// 叶子节点的**具名动作 id**：`&'static str` 新类型。
///
/// 用字符串常量而不是枚举 / 函数指针的原因：①引擎不认识游戏动作，字符串
/// 是双方唯一需要共享的东西；②零分配、比较便宜（内容比较，不需要 `Hash`）；
/// ③树的 `Debug` 输出即文档（`Action("guard.patrol")`）。未知 id 由游戏侧的
/// `match` 兜底（建议 `debug_assert!(false)` 显式报错，引擎不替它猜）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActionId(pub &'static str);

impl ActionId {
    /// 构造（const 上下文可用——树可以在 `const` 里声明）。
    pub const fn new(name: &'static str) -> Self {
        Self(name)
    }

    /// 名字（游戏侧 `match` 的分发键）。
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl From<&'static str> for ActionId {
    fn from(name: &'static str) -> Self {
        Self(name)
    }
}

impl std::fmt::Display for ActionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

// ---------------------------------------------------------------------------
// 黑板
// ---------------------------------------------------------------------------

/// 黑板值（五型，够用即止）。
#[derive(Clone, Debug, PartialEq)]
pub enum BlackValue {
    /// 布尔。
    Bool(bool),
    /// 浮点（引擎不做隐式整型——需要下标就存浮点或文本，语义自明）。
    Float(f32),
    /// 实体句柄。
    Entity(Entity),
    /// 三维向量（世界坐标 / 方向 / 目标点）。
    Vec3(Vec3),
    /// 文本（模式名 / 阶段名等调试与分派用）。
    Text(String),
}

/// 逐代理黑板：键值存储，**迭代序 = 键序**（`BTreeMap`，UTF-8 字节序）。
///
/// 选型理由照 `broadphase.rs`：决策系统里「按不确定顺序遍历」等于把确定性
/// 交给实现细节；`BTreeMap` 的迭代序是键的确定函数，且不依赖哈希种子。键用
/// `String`（运行时可变），读路径收 `&str`（零键分配）。
///
/// 类型语义：`get_*` 键缺席**或类型不符**都返回 `None`（不 panic、不隐式
/// 转换）；`get_*_or` 给默认值。写入同名键即覆盖。
#[derive(Clone, Debug, Default, PartialEq, Component)]
pub struct Blackboard {
    entries: BTreeMap<String, BlackValue>,
}

impl Blackboard {
    /// 空黑板。
    pub fn new() -> Self {
        Self::default()
    }

    /// 条目数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 无条目？
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 原始值读取（类型判别用）。
    pub fn get(&self, key: &str) -> Option<&BlackValue> {
        self.entries.get(key)
    }

    /// 移除条目，返回旧值。
    pub fn remove(&mut self, key: &str) -> Option<BlackValue> {
        self.entries.remove(key)
    }

    /// 清空（换局 / 复用代理时用）。
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// 按**键序**遍历（确定性：见类型文档）。
    pub fn iter(&self) -> impl Iterator<Item = (&str, &BlackValue)> + '_ {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_str(), value))
    }

    /// 写入布尔。
    pub fn set_bool(&mut self, key: &str, value: bool) {
        self.entries
            .insert(key.to_string(), BlackValue::Bool(value));
    }

    /// 写入浮点。
    pub fn set_f32(&mut self, key: &str, value: f32) {
        self.entries
            .insert(key.to_string(), BlackValue::Float(value));
    }

    /// 写入实体句柄。
    pub fn set_entity(&mut self, key: &str, value: Entity) {
        self.entries
            .insert(key.to_string(), BlackValue::Entity(value));
    }

    /// 写入向量。
    pub fn set_vec3(&mut self, key: &str, value: Vec3) {
        self.entries
            .insert(key.to_string(), BlackValue::Vec3(value));
    }

    /// 写入文本。
    pub fn set_text(&mut self, key: &str, value: &str) {
        self.entries
            .insert(key.to_string(), BlackValue::Text(value.to_string()));
    }

    /// 读布尔（键缺席 / 类型不符 → `None`）。
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        match self.entries.get(key) {
            Some(BlackValue::Bool(value)) => Some(*value),
            _ => None,
        }
    }

    /// 读浮点。
    pub fn get_f32(&self, key: &str) -> Option<f32> {
        match self.entries.get(key) {
            Some(BlackValue::Float(value)) => Some(*value),
            _ => None,
        }
    }

    /// 读实体句柄。
    pub fn get_entity(&self, key: &str) -> Option<Entity> {
        match self.entries.get(key) {
            Some(BlackValue::Entity(value)) => Some(*value),
            _ => None,
        }
    }

    /// 读向量。
    pub fn get_vec3(&self, key: &str) -> Option<Vec3> {
        match self.entries.get(key) {
            Some(BlackValue::Vec3(value)) => Some(*value),
            _ => None,
        }
    }

    /// 读文本。
    pub fn get_text(&self, key: &str) -> Option<&str> {
        match self.entries.get(key) {
            Some(BlackValue::Text(value)) => Some(value.as_str()),
            _ => None,
        }
    }

    /// 读布尔（缺席 / 类型不符 → `default`）。
    pub fn get_bool_or(&self, key: &str, default: bool) -> bool {
        self.get_bool(key).unwrap_or(default)
    }

    /// 读浮点（缺席 / 类型不符 → `default`）。
    pub fn get_f32_or(&self, key: &str, default: f32) -> f32 {
        self.get_f32(key).unwrap_or(default)
    }
}
// ---------------------------------------------------------------------------
// 树结构（纯数据）
// ---------------------------------------------------------------------------

/// 扁平树里的节点下标（构造期前序分配，稳定不变）。
pub type NodeId = usize;

/// `Parallel` 的阈值策略：**成功数 / 失败数双阈值**。
///
/// 语义：本轮成功数 ≥ `success_threshold` → Success；本轮失败数 ≥
/// `failure_threshold` → Failure（**失败优先**）；都不满足 → Running。
/// 两个阈值都必须 ≥ 1（构造期校验），否则节点永不终结。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParallelPolicy {
    /// 成功阈值（≥ 1）。
    pub success_threshold: usize,
    /// 失败阈值（≥ 1）。
    pub failure_threshold: usize,
}

impl ParallelPolicy {
    /// 双阈值策略。
    pub const fn new(success_threshold: usize, failure_threshold: usize) -> Self {
        Self {
            success_threshold,
            failure_threshold,
        }
    }

    /// 「全部子节点成功才算成功、任一失败即失败」：调用方按子节点数给
    /// 成功阈值。
    pub const fn all(children: usize) -> Self {
        Self::new(children, 1)
    }
}

/// 扁平树节点（[`BehaviorTree::new`] 由 [`NodeSpec`] 展开而来）。
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// 顺序：子节点依次求值，任一失败即短路失败；全部成功才成功。
    Sequence {
        /// 子节点 id（声明序）。
        children: Vec<NodeId>,
    },
    /// 选择：子节点依次求值，任一成功即短路成功；全部失败才失败。
    Selector {
        /// 子节点 id（声明序）。
        children: Vec<NodeId>,
    },
    /// 并行：每帧 tick 全部**未终结**子节点，按阈值裁决（见 [`ParallelPolicy`]）。
    Parallel {
        /// 子节点 id（声明序）。
        children: Vec<NodeId>,
        /// 阈值策略。
        policy: ParallelPolicy,
    },
    /// 反转：成功 ↔ 失败，`Running` 透传。
    Inverter {
        /// 子节点 id。
        child: NodeId,
    },
    /// 重复：子节点成功 `times` 次后成功；任一失败即短路失败（清计数）。
    Repeat {
        /// 子节点 id。
        child: NodeId,
        /// 次数（≥ 1）。
        times: u32,
    },
    /// 冷却：子节点成功即进入 `seconds` 秒冷却；**冷却期内恒返回失败且不
    /// tick 子节点**（冷却由 tick 的 `dt` 递减——无时钟读取）。
    Cooldown {
        /// 子节点 id。
        child: NodeId,
        /// 冷却秒数（≥ 0；0 = 无冷却）。
        seconds: f32,
    },
    /// 条件叶子（只读黑板）。
    Condition(ActionId),
    /// 动作叶子。
    Action(ActionId),
}

/// 写树用的嵌套规格（构造期形态；[`BehaviorTree::new`] 展开成扁平 [`Node`]）。
///
/// 用 `Box` 承载单子装饰器（**不是** `Box<dyn>`——无动态分发、无 trait
/// 对象）：嵌套规格只在构造期存在，展开后即丢弃。
#[derive(Clone, Debug, PartialEq)]
pub enum NodeSpec {
    /// 顺序（见 [`Node::Sequence`]）。
    Sequence(Vec<NodeSpec>),
    /// 选择（见 [`Node::Selector`]）。
    Selector(Vec<NodeSpec>),
    /// 并行（见 [`Node::Parallel`]）。
    Parallel {
        /// 子节点。
        children: Vec<NodeSpec>,
        /// 阈值策略。
        policy: ParallelPolicy,
    },
    /// 反转（见 [`Node::Inverter`]）。
    Inverter(Box<NodeSpec>),
    /// 重复（见 [`Node::Repeat`]）。
    Repeat {
        /// 子节点。
        child: Box<NodeSpec>,
        /// 次数（≥ 1，否则构造期 panic）。
        times: u32,
    },
    /// 冷却（见 [`Node::Cooldown`]）。
    Cooldown {
        /// 子节点。
        child: Box<NodeSpec>,
        /// 冷却秒数（≥ 0，否则构造期 panic）。
        seconds: f32,
    },
    /// 条件叶子。
    Condition(ActionId),
    /// 动作叶子。
    Action(ActionId),
}

impl NodeSpec {
    /// 动作叶子：`NodeSpec::action("guard.patrol")`。
    pub fn action(id: impl Into<ActionId>) -> Self {
        Self::Action(id.into())
    }

    /// 条件叶子：`NodeSpec::condition("guard.stunned")`。
    pub fn condition(id: impl Into<ActionId>) -> Self {
        Self::Condition(id.into())
    }

    /// 顺序节点。
    pub fn sequence(children: impl IntoIterator<Item = NodeSpec>) -> Self {
        Self::Sequence(children.into_iter().collect())
    }

    /// 选择节点。
    pub fn selector(children: impl IntoIterator<Item = NodeSpec>) -> Self {
        Self::Selector(children.into_iter().collect())
    }

    /// 并行节点。
    pub fn parallel(children: impl IntoIterator<Item = NodeSpec>, policy: ParallelPolicy) -> Self {
        Self::Parallel {
            children: children.into_iter().collect(),
            policy,
        }
    }

    /// 反转装饰器。
    pub fn inverter(child: NodeSpec) -> Self {
        Self::Inverter(Box::new(child))
    }

    /// 重复装饰器（`times` ≥ 1）。
    pub fn repeat(child: NodeSpec, times: u32) -> Self {
        Self::Repeat {
            child: Box::new(child),
            times,
        }
    }

    /// 冷却装饰器（`seconds` ≥ 0）。
    pub fn cooldown(child: NodeSpec, seconds: f32) -> Self {
        Self::Cooldown {
            child: Box::new(child),
            seconds,
        }
    }
}

/// 行为树：扁平节点数组 + 根下标（[`BehaviorAgent`] 组件的一部分）。
///
/// 不可变纯数据：构造后只读（运行期换树 = 换组件；热重载在拒绝清单）。
/// `Clone` 是结构拷贝——同一棵树给多个代理时克隆一次即可（节点数很小）。
#[derive(Clone, Debug, PartialEq)]
pub struct BehaviorTree {
    nodes: Vec<Node>,
    root: NodeId,
}

impl BehaviorTree {
    /// 由嵌套规格展开（前序分配 id）。以下规格在构造期 panic（fail loud，
    /// 不静默降级）：`Repeat.times == 0`、`Cooldown.seconds < 0`、`Parallel`
    /// 空子节点或任一阈值为 0。
    pub fn new(root: NodeSpec) -> Self {
        let mut tree = Self {
            nodes: Vec::new(),
            root: 0,
        };
        tree.root = tree.push_spec(root);
        tree
    }

    /// 节点数。
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// 无节点？（构造器恒产生 ≥ 1 个节点，故恒 `false`——API 完备性保留。）
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// 根节点 id。
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// 按 id 取节点（诊断 / 测试）。
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// 全部节点（前序 id 序）。
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    fn alloc(&mut self, node: Node) -> NodeId {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    fn push_spec(&mut self, spec: NodeSpec) -> NodeId {
        match spec {
            NodeSpec::Sequence(children) => {
                let id = self.alloc(Node::Sequence {
                    children: Vec::new(),
                });
                let ids = self.push_children(children);
                self.nodes[id] = Node::Sequence { children: ids };
                id
            }
            NodeSpec::Selector(children) => {
                let id = self.alloc(Node::Selector {
                    children: Vec::new(),
                });
                let ids = self.push_children(children);
                self.nodes[id] = Node::Selector { children: ids };
                id
            }
            NodeSpec::Parallel { children, policy } => {
                assert!(
                    policy.success_threshold >= 1 && policy.failure_threshold >= 1,
                    "Parallel 的成功/失败阈值须 ≥ 1（当前 {} / {}）——阈值为 0 的节点永不终结",
                    policy.success_threshold,
                    policy.failure_threshold
                );
                assert!(
                    !children.is_empty(),
                    "Parallel 须至少一个子节点（空并行的双阈值都无从满足）"
                );
                let id = self.alloc(Node::Parallel {
                    children: Vec::new(),
                    policy,
                });
                let ids = self.push_children(children);
                self.nodes[id] = Node::Parallel {
                    children: ids,
                    policy,
                };
                id
            }
            NodeSpec::Inverter(child) => {
                let id = self.alloc(Node::Inverter { child: 0 });
                let child = self.push_spec(*child);
                self.nodes[id] = Node::Inverter { child };
                id
            }
            NodeSpec::Repeat { child, times } => {
                assert!(
                    times >= 1,
                    "Repeat 的 times 须 ≥ 1（当前 {times}）——0 次重复无成功语义"
                );
                let id = self.alloc(Node::Repeat { child: 0, times });
                let child = self.push_spec(*child);
                self.nodes[id] = Node::Repeat { child, times };
                id
            }
            NodeSpec::Cooldown { child, seconds } => {
                assert!(
                    seconds >= 0.0,
                    "Cooldown 的 seconds 须 ≥ 0（当前 {seconds}）——负冷却无意义"
                );
                let id = self.alloc(Node::Cooldown { child: 0, seconds });
                let child = self.push_spec(*child);
                self.nodes[id] = Node::Cooldown { child, seconds };
                id
            }
            NodeSpec::Condition(id) => self.alloc(Node::Condition(id)),
            NodeSpec::Action(id) => self.alloc(Node::Action(id)),
        }
    }

    fn push_children(&mut self, children: Vec<NodeSpec>) -> Vec<NodeId> {
        let mut ids = Vec::with_capacity(children.len());
        for child in children {
            ids.push(self.push_spec(child));
        }
        ids
    }
    /// tick 一次：`state` 是逐代理运行态（必须由 [`BehaviorState::new`] 与
    /// **同一棵树**创建），`blackboard` 是逐代理黑板，`ctx` 分发叶子，
    /// `dt` 是本帧步长（秒）。返回根节点的状态。
    ///
    /// # Panics
    /// 运行态与树不同源（节点数不符）时 panic——静默错位会让记忆恢复到别的
    /// 节点上，属于必须暴露的接线错误。
    pub fn tick<C: BehaviorContext>(
        &self,
        state: &mut BehaviorState,
        blackboard: &mut Blackboard,
        ctx: &mut C,
        dt: f32,
    ) -> BtStatus {
        assert_eq!(
            state.memory.len(),
            self.nodes.len(),
            "行为树与运行态必须同源（BehaviorState::new(树)）：树 {} 节点 / 运行态 {} 项",
            self.nodes.len(),
            state.memory.len()
        );
        self.tick_node(self.root, state, blackboard, ctx, dt)
    }

    fn tick_node<C: BehaviorContext>(
        &self,
        id: NodeId,
        state: &mut BehaviorState,
        blackboard: &mut Blackboard,
        ctx: &mut C,
        dt: f32,
    ) -> BtStatus {
        let was_running = state.memory[id].status == Some(BtStatus::Running);
        let status = match &self.nodes[id] {
            Node::Sequence { children } => {
                let mut cursor = if was_running {
                    state.memory[id].cursor
                } else {
                    0
                };
                let mut result = BtStatus::Success;
                while cursor < children.len() {
                    match self.tick_node(children[cursor], state, blackboard, ctx, dt) {
                        BtStatus::Failure => {
                            result = BtStatus::Failure;
                            cursor = 0;
                            break;
                        }
                        BtStatus::Running => {
                            result = BtStatus::Running;
                            break;
                        }
                        BtStatus::Success => cursor += 1,
                    }
                }
                state.memory[id].cursor = cursor;
                result
            }
            Node::Selector { children } => {
                let mut cursor = if was_running {
                    state.memory[id].cursor
                } else {
                    0
                };
                let mut result = BtStatus::Failure;
                while cursor < children.len() {
                    match self.tick_node(children[cursor], state, blackboard, ctx, dt) {
                        BtStatus::Success => {
                            result = BtStatus::Success;
                            cursor = 0;
                            break;
                        }
                        BtStatus::Running => {
                            result = BtStatus::Running;
                            break;
                        }
                        BtStatus::Failure => cursor += 1,
                    }
                }
                state.memory[id].cursor = cursor;
                result
            }
            Node::Parallel { children, policy } => {
                let count = children.len();
                let (mut successes, mut failures) = if was_running {
                    (
                        state.memory[id].parallel_success,
                        state.memory[id].parallel_failure,
                    )
                } else {
                    state.memory[id].child_done.clear();
                    state.memory[id].child_done.resize(count, false);
                    (0, 0)
                };
                for (index, child) in children.iter().enumerate() {
                    if state.memory[id].child_done[index] {
                        continue; // 本轮已终结的子节点不再 tick
                    }
                    match self.tick_node(*child, state, blackboard, ctx, dt) {
                        BtStatus::Success => {
                            state.memory[id].child_done[index] = true;
                            successes += 1;
                        }
                        BtStatus::Failure => {
                            state.memory[id].child_done[index] = true;
                            failures += 1;
                        }
                        BtStatus::Running => {}
                    }
                    // 阈值即时裁决：失败优先；达到即结束本轮（余下子节点本轮不再 tick）
                    if failures >= policy.failure_threshold {
                        break;
                    }
                    if successes >= policy.success_threshold {
                        break;
                    }
                }
                let result = if failures >= policy.failure_threshold {
                    BtStatus::Failure
                } else if successes >= policy.success_threshold {
                    BtStatus::Success
                } else {
                    BtStatus::Running
                };
                if result != BtStatus::Running {
                    state.memory[id].child_done.clear();
                    state.memory[id].child_done.resize(count, false);
                    successes = 0;
                    failures = 0;
                }
                state.memory[id].parallel_success = successes;
                state.memory[id].parallel_failure = failures;
                result
            }
            Node::Inverter { child } => match self.tick_node(*child, state, blackboard, ctx, dt) {
                BtStatus::Success => BtStatus::Failure,
                BtStatus::Failure => BtStatus::Success,
                BtStatus::Running => BtStatus::Running,
            },
            Node::Repeat { child, times } => {
                let mut done = if was_running {
                    state.memory[id].repeat_done
                } else {
                    0
                };
                let mut result = BtStatus::Running;
                while done < *times {
                    match self.tick_node(*child, state, blackboard, ctx, dt) {
                        BtStatus::Failure => {
                            result = BtStatus::Failure;
                            done = 0;
                            break;
                        }
                        BtStatus::Success => done += 1,
                        BtStatus::Running => {
                            result = BtStatus::Running;
                            break;
                        }
                    }
                }
                if done >= *times {
                    result = BtStatus::Success;
                    done = 0;
                }
                state.memory[id].repeat_done = done;
                result
            }
            Node::Cooldown { child, seconds } => {
                let remaining = state.memory[id].cooldown;
                if remaining > 0.0 {
                    // 冷却中：不 tick 子节点；本帧递减（归零的当帧仍判失败，
                    // 下一帧恢复子节点——「冷却 ≥ seconds 秒」的保守语义）
                    state.memory[id].cooldown = (remaining - dt).max(0.0);
                    BtStatus::Failure
                } else {
                    let status = self.tick_node(*child, state, blackboard, ctx, dt);
                    if status == BtStatus::Success {
                        state.memory[id].cooldown = *seconds;
                    }
                    status
                }
            }
            Node::Condition(id) => {
                if ctx.condition(*id, blackboard, dt) {
                    BtStatus::Success
                } else {
                    BtStatus::Failure
                }
            }
            Node::Action(id) => ctx.action(*id, blackboard, dt),
        };
        state.memory[id].status = Some(status);
        status
    }
}
// ---------------------------------------------------------------------------
// 运行态（带记忆）
// ---------------------------------------------------------------------------

/// 单个节点的运行态记忆（引擎内部形态；对外只经 [`BehaviorState`] 读摘要）。
#[derive(Clone, Debug, Default, PartialEq)]
struct NodeMemory {
    /// 上帧该节点返回的状态（`Running` 时本帧从记忆恢复）。
    status: Option<BtStatus>,
    /// 复合节点：当前子节点游标。
    cursor: usize,
    /// `Repeat`：本轮已完成次数。
    repeat_done: u32,
    /// `Cooldown`：剩余冷却秒数。
    cooldown: f32,
    /// `Parallel`：本轮各子节点是否已终结。
    child_done: Vec<bool>,
    /// `Parallel`：本轮成功数。
    parallel_success: usize,
    /// `Parallel`：本轮失败数。
    parallel_failure: usize,
}

/// 逐代理运行态：与树同源（节点数一致）的记忆数组。
///
/// 由 [`BehaviorState::new`] 绑定一棵树；[`BehaviorTree::tick`] 会断言两者
/// 同源。整份运行态是 `Clone` 的纯数据（无句柄、无借用），随组件生命周期
/// 消亡。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BehaviorState {
    memory: Vec<NodeMemory>,
}

impl BehaviorState {
    /// 按树创建（节点数 = 树节点数）。
    pub fn new(tree: &BehaviorTree) -> Self {
        Self {
            memory: vec![NodeMemory::default(); tree.len()],
        }
    }

    /// 节点数（= 所绑树的节点数）。
    pub fn node_count(&self) -> usize {
        self.memory.len()
    }

    /// 某节点上帧的状态（诊断 / 测试；从未 tick 过为 `None`）。
    pub fn status_of(&self, id: NodeId) -> Option<BtStatus> {
        self.memory.get(id).and_then(|memory| memory.status)
    }

    /// 某节点上帧是否 `Running`。
    pub fn is_running(&self, id: NodeId) -> bool {
        self.status_of(id) == Some(BtStatus::Running)
    }

    /// 清空全部记忆（重开 / 复用代理；树不变）。
    pub fn reset(&mut self) {
        for memory in &mut self.memory {
            *memory = NodeMemory::default();
        }
    }
}

// ---------------------------------------------------------------------------
// 上下文（游戏侧的叶子分发）
// ---------------------------------------------------------------------------

/// 叶子分发上下文：游戏侧实现这个 trait，把具名 id 映射到自己的动作 / 条件
/// 实现（`match id.as_str()` 或注册表皆可）。
///
/// 契约：
/// - `action` 返回 `Running` 表示跨帧进行（引擎负责记忆恢复，游戏侧只需
///   按黑板里的进度继续）；
/// - `condition` **只读**黑板（有状态的判断写成 `action`）；
/// - 两个方法都按**声明序**被调用（确定性的一部分），不得引入随机 / 时钟
///   （时间从 `dt` 走，状态从黑板走）。
pub trait BehaviorContext {
    /// 执行动作叶子。
    fn action(&mut self, id: ActionId, blackboard: &mut Blackboard, dt: f32) -> BtStatus;

    /// 求值条件叶子。
    fn condition(&mut self, id: ActionId, blackboard: &Blackboard, dt: f32) -> bool;
}

// ---------------------------------------------------------------------------
// 代理组件 / ECS 接入
// ---------------------------------------------------------------------------

/// 逐代理「大脑」组件：树 + 运行态 + 黑板三合一。
///
/// 代理实体 despawn 时三者一起消亡（生命周期天然闭合，无跨代理共享状态）。
/// 同一棵树给多个代理时用 `BehaviorAgent::new(tree.clone())`——树是不可变
/// 纯数据，克隆即结构拷贝。
#[derive(Clone, Debug, PartialEq, Component)]
pub struct BehaviorAgent {
    /// 不可变树结构。
    pub tree: BehaviorTree,
    /// 运行态记忆。
    pub state: BehaviorState,
    /// 黑板。
    pub blackboard: Blackboard,
}

impl BehaviorAgent {
    /// 按树创建空运行态与空黑板。
    pub fn new(tree: BehaviorTree) -> Self {
        let state = BehaviorState::new(&tree);
        Self {
            tree,
            state,
            blackboard: Blackboard::new(),
        }
    }

    /// tick 一次（内部按字段解构借用，见 [`BehaviorTree::tick`]）。
    pub fn tick<C: BehaviorContext>(&mut self, ctx: &mut C, dt: f32) -> BtStatus {
        let Self {
            tree,
            state,
            blackboard,
        } = self;
        tree.tick(state, blackboard, ctx, dt)
    }

    /// 根节点上帧状态（诊断）。
    pub fn root_status(&self) -> Option<BtStatus> {
        self.state.status_of(self.tree.root())
    }

    /// 重开（清运行态与黑板，树不变）。
    pub fn reset(&mut self) {
        self.state.reset();
        self.blackboard.clear();
    }
}

/// 行为树驱动系统体：世界中每个挂 [`BehaviorAgent`] 的实体 tick 一次。
///
/// - **迭代序 = 实体 id 升序**（显式排序，与插入序 / dense 数组顺序无关）；
/// - 每实体调用一次 `make_ctx` 构造执行上下文（游戏侧按实体取它需要的组件
///   与只读世界借用）；引擎不缓存上下文；
/// - 两阶段借用：先收集实体列表（读锁随即释放），再逐实体 `get_mut` 取
///   [`BehaviorAgent`]（写锁）；`make_ctx` 期间**不得**再借用
///   `BehaviorAgent`（同类型借用冲突即 panic，见 `crate::sync`）。
///
/// 注册姿势（Update 阶段并行系统）见模块文档「系统接入」。
pub fn tick_behavior_agents<C: BehaviorContext>(
    world: &World,
    dt: f32,
    mut make_ctx: impl FnMut(Entity, &World) -> C,
) {
    let mut agents: Vec<Entity> = Vec::new();
    world.query::<(Entity, &BehaviorAgent)>(|(entity, _)| agents.push(entity));
    agents.sort_unstable();
    for entity in agents {
        let Some(mut agent) = world.get_mut::<BehaviorAgent>(entity) else {
            continue; // 收集与写回之间被 despawn：跳过（无结构修改则不会发生）
        };
        let mut ctx = make_ctx(entity, world);
        agent.tick(&mut ctx, dt);
    }
}
// ---------------------------------------------------------------------------
// 单测（节点语义逐条 / 记忆恢复 / 黑板 / 确定性 / ECS 接入）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    use crate::transform::Transform;

    /// 脚本化上下文：按调用序返回预设状态，并记录调用序（短路断言的证据）；
    /// 每次动作调用把 id 写进黑板（黑板内容随运行变化的确定性样本）。
    struct Scripted {
        log: Vec<&'static str>,
        actions: BTreeMap<&'static str, VecDeque<BtStatus>>,
        conditions: BTreeMap<&'static str, bool>,
        fallback: BtStatus,
    }

    impl Default for Scripted {
        fn default() -> Self {
            Self {
                log: Vec::new(),
                actions: BTreeMap::new(),
                conditions: BTreeMap::new(),
                fallback: BtStatus::Success,
            }
        }
    }

    impl Scripted {
        fn new() -> Self {
            Self::default()
        }

        fn script(
            mut self,
            id: &'static str,
            statuses: impl IntoIterator<Item = BtStatus>,
        ) -> Self {
            self.actions.insert(id, statuses.into_iter().collect());
            self
        }

        fn cond(mut self, id: &'static str, value: bool) -> Self {
            self.conditions.insert(id, value);
            self
        }

        fn fallback(mut self, status: BtStatus) -> Self {
            self.fallback = status;
            self
        }
    }

    impl BehaviorContext for Scripted {
        fn action(&mut self, id: ActionId, blackboard: &mut Blackboard, _dt: f32) -> BtStatus {
            self.log.push(id.0);
            blackboard.set_text("last", id.0);
            self.actions
                .get_mut(id.0)
                .and_then(|queue| queue.pop_front())
                .unwrap_or(self.fallback)
        }

        fn condition(&mut self, id: ActionId, _blackboard: &Blackboard, _dt: f32) -> bool {
            self.log.push(id.0);
            *self.conditions.get(id.0).unwrap_or(&false)
        }
    }

    /// 运行态 + 黑板的确定性样本（逐位：浮点与实体都取 bits）。
    fn dump(state: &BehaviorState, blackboard: &Blackboard) -> Vec<String> {
        let mut out: Vec<String> = (0..state.node_count())
            .map(|id| format!("{:?}", state.status_of(id)))
            .collect();
        for (key, value) in blackboard.iter() {
            out.push(match value {
                BlackValue::Bool(v) => format!("{key}=b{v}"),
                BlackValue::Float(v) => format!("{key}=f{}", v.to_bits()),
                BlackValue::Entity(e) => format!("{key}=e{}", e.to_bits()),
                BlackValue::Vec3(v) => format!(
                    "{key}=v{},{},{}",
                    v.x.to_bits(),
                    v.y.to_bits(),
                    v.z.to_bits()
                ),
                BlackValue::Text(t) => format!("{key}=t{t}"),
            });
        }
        out
    }

    // ---- 复合节点短路语义 ----

    #[test]
    fn sequence_short_circuits_on_first_failure() {
        let tree = BehaviorTree::new(NodeSpec::sequence([
            NodeSpec::action("a"),
            NodeSpec::action("b"),
            NodeSpec::action("c"),
        ]));
        let mut state = BehaviorState::new(&tree);
        let mut blackboard = Blackboard::new();
        let mut ctx = Scripted::new()
            .script("a", [BtStatus::Success])
            .script("b", [BtStatus::Failure])
            .script("c", [BtStatus::Success]);
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 0.1),
            BtStatus::Failure
        );
        assert_eq!(ctx.log, ["a", "b"], "Sequence 失败后应短路（c 不得被调用）");
        assert_eq!(state.status_of(tree.root()), Some(BtStatus::Failure));
    }

    #[test]
    fn selector_short_circuits_on_first_success() {
        let tree = BehaviorTree::new(NodeSpec::selector([
            NodeSpec::action("a"),
            NodeSpec::action("b"),
            NodeSpec::action("c"),
        ]));
        let mut state = BehaviorState::new(&tree);
        let mut blackboard = Blackboard::new();
        let mut ctx = Scripted::new()
            .script("a", [BtStatus::Failure])
            .script("b", [BtStatus::Success])
            .script("c", [BtStatus::Success]);
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 0.1),
            BtStatus::Success
        );
        assert_eq!(ctx.log, ["a", "b"], "Selector 成功后应短路（c 不得被调用）");
    }

    #[test]
    fn empty_composites_have_declared_semantics() {
        let empty_sequence = BehaviorTree::new(NodeSpec::sequence([]));
        let empty_selector = BehaviorTree::new(NodeSpec::selector([]));
        let mut ctx = Scripted::new();
        let mut blackboard = Blackboard::new();
        let mut state = BehaviorState::new(&empty_sequence);
        assert_eq!(
            empty_sequence.tick(&mut state, &mut blackboard, &mut ctx, 0.1),
            BtStatus::Success,
            "空 Sequence = 成功（真空中全部子节点都成功）"
        );
        let mut state = BehaviorState::new(&empty_selector);
        assert_eq!(
            empty_selector.tick(&mut state, &mut blackboard, &mut ctx, 0.1),
            BtStatus::Failure,
            "空 Selector = 失败（没有可选分支）"
        );
    }

    #[test]
    fn parallel_thresholds_and_failure_priority() {
        // 成功阈值 2：两个成功 → Success；失败阈值 2：两个失败 → Failure
        let tree = BehaviorTree::new(NodeSpec::parallel(
            [
                NodeSpec::action("a"),
                NodeSpec::action("b"),
                NodeSpec::action("c"),
            ],
            ParallelPolicy::new(2, 2),
        ));
        let mut state = BehaviorState::new(&tree);
        let mut blackboard = Blackboard::new();
        // 第一帧：a 成功、b 仍在跑、c 失败 → 计数 1/1，两阈值都不满足 → Running
        let mut ctx = Scripted::new()
            .script("a", [BtStatus::Success])
            .script("b", [BtStatus::Running])
            .script("c", [BtStatus::Failure]);
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 0.1),
            BtStatus::Running
        );
        assert_eq!(
            ctx.log,
            ["a", "b", "c"],
            "未达阈值时本轮全部子节点都应被 tick"
        );
        // 第二帧：a 已终结不再 tick；b 成功 → 成功数 2 达阈值 → Success
        let mut ctx = Scripted::new()
            .script("a", [BtStatus::Failure]) // 若被再次调用会翻转结论（断言不被调用）
            .script("b", [BtStatus::Success]);
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 0.1),
            BtStatus::Success
        );
        assert_eq!(ctx.log, ["b"], "本轮已终结的子节点不得被重复 tick");
        // 新一轮：两个失败即 Failure（失败优先于成功阈值）
        let mut ctx = Scripted::new()
            .script("a", [BtStatus::Failure])
            .script("b", [BtStatus::Success])
            .script("c", [BtStatus::Failure]);
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 0.1),
            BtStatus::Failure
        );
        assert_eq!(
            ctx.log,
            ["a", "b", "c"],
            "失败数达阈值前的子节点都应被 tick"
        );
    }
    // ---- 装饰器 ----

    #[test]
    fn inverter_flips_terminal_status_and_passes_running_through() {
        let tree = BehaviorTree::new(NodeSpec::inverter(NodeSpec::action("a")));
        let mut state = BehaviorState::new(&tree);
        let mut blackboard = Blackboard::new();
        let mut ctx = Scripted::new().script(
            "a",
            [BtStatus::Success, BtStatus::Failure, BtStatus::Running],
        );
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 0.1),
            BtStatus::Failure
        );
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 0.1),
            BtStatus::Success
        );
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 0.1),
            BtStatus::Running,
            "Running 应透传（反转只作用于终结状态）"
        );
    }

    #[test]
    fn repeat_runs_child_exactly_times_and_clears_on_failure() {
        let tree = BehaviorTree::new(NodeSpec::repeat(NodeSpec::action("a"), 3));
        let mut state = BehaviorState::new(&tree);
        let mut blackboard = Blackboard::new();
        // 单帧内跑满 3 次：调用序 = a a a
        let mut ctx = Scripted::new().script(
            "a",
            [BtStatus::Success, BtStatus::Success, BtStatus::Success],
        );
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 0.1),
            BtStatus::Success
        );
        assert_eq!(ctx.log, ["a", "a", "a"], "Repeat(3) 应恰好调用 3 次");
        // 跨帧：Running 中断后从已完成次数恢复（不是重头再来）
        let mut ctx = Scripted::new().script(
            "a",
            [
                BtStatus::Success,
                BtStatus::Running,
                BtStatus::Success,
                BtStatus::Success,
            ],
        );
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 0.1),
            BtStatus::Running
        );
        assert_eq!(ctx.log, ["a", "a"], "第一帧：1 次成功 + 1 次 Running");
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 0.1),
            BtStatus::Success
        );
        assert_eq!(
            ctx.log,
            ["a", "a", "a", "a"],
            "第二帧：从中断处恢复（再跑 2 次即满 3 次），不得重头再来"
        );
        // 失败短路：已完成计数清零（下一帧重新数）；换新运行态让日志独立
        let mut state = BehaviorState::new(&tree);
        let mut ctx = Scripted::new()
            .script(
                "a",
                [BtStatus::Success, BtStatus::Failure, BtStatus::Success],
            )
            .fallback(BtStatus::Running);
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 0.1),
            BtStatus::Failure
        );
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 0.1),
            BtStatus::Running,
            "失败清计数：下一帧从 0 重新数（1 次成功后再次 Running）"
        );
        assert_eq!(ctx.log, ["a", "a", "a", "a"], "失败帧 2 次 + 恢复帧 2 次");
    }

    #[test]
    fn cooldown_blocks_child_until_elapsed() {
        let tree = BehaviorTree::new(NodeSpec::cooldown(NodeSpec::action("a"), 1.0));
        let mut state = BehaviorState::new(&tree);
        let mut blackboard = Blackboard::new();
        let mut ctx = Scripted::new().fallback(BtStatus::Success);
        // 第 1 帧：子节点成功 → 进入 1.0 s 冷却
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 1.0),
            BtStatus::Success
        );
        // 第 2 帧：冷却中 → Failure（子节点不得被调用）
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 1.0),
            BtStatus::Failure
        );
        assert_eq!(ctx.log, ["a"], "冷却期内不得 tick 子节点");
        // 第 3 帧：冷却已归零 → 子节点恢复
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 1.0),
            BtStatus::Success
        );
        assert_eq!(ctx.log, ["a", "a"], "冷却归零后子节点应恢复");
        // 子节点失败不触发冷却（连续两帧都跑）
        let mut ctx = Scripted::new().script("a", [BtStatus::Failure, BtStatus::Failure]);
        let mut state = BehaviorState::new(&tree);
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 1.0),
            BtStatus::Failure
        );
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 1.0),
            BtStatus::Failure
        );
        assert_eq!(ctx.log, ["a", "a"], "失败不进入冷却");
    }

    // ---- Running 记忆恢复（效率标准形态）----

    #[test]
    fn running_memory_resumes_from_the_running_child_not_the_first() {
        // Sequence[a, b, c]：a 成功、b 连跑两帧、c 收尾。
        // 无记忆的实现会在第二帧重跑 a（log 出现两次 a 在 c 之前）——本断言
        // 正是「记忆恢复」的判别式。
        let tree = BehaviorTree::new(NodeSpec::sequence([
            NodeSpec::action("a"),
            NodeSpec::action("b"),
            NodeSpec::action("c"),
        ]));
        let mut state = BehaviorState::new(&tree);
        let mut blackboard = Blackboard::new();
        let mut ctx = Scripted::new()
            .script("a", [BtStatus::Success, BtStatus::Failure])
            .script("b", [BtStatus::Running, BtStatus::Success])
            .script("c", [BtStatus::Success]);
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 0.1),
            BtStatus::Running
        );
        assert!(state.is_running(tree.root()), "根节点应记为 Running");
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 0.1),
            BtStatus::Success
        );
        assert_eq!(
            ctx.log,
            ["a", "b", "b", "c"],
            "第二帧应从 b（Running 的子节点）恢复：不得重跑 a"
        );
        assert!(!state.is_running(tree.root()), "完成后不再是 Running");
    }

    #[test]
    fn running_memory_survives_in_parallel_branches() {
        let tree = BehaviorTree::new(NodeSpec::parallel(
            [NodeSpec::action("a"), NodeSpec::action("b")],
            ParallelPolicy::new(2, 2),
        ));
        let mut state = BehaviorState::new(&tree);
        let mut blackboard = Blackboard::new();
        let mut ctx = Scripted::new()
            .script("a", [BtStatus::Running, BtStatus::Success])
            .script("b", [BtStatus::Running, BtStatus::Success]);
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 0.1),
            BtStatus::Running
        );
        assert_eq!(
            tree.tick(&mut state, &mut blackboard, &mut ctx, 0.1),
            BtStatus::Success
        );
        assert_eq!(ctx.log, ["a", "b", "a", "b"], "两个 Running 分支都应恢复");
    }
    // ---- 黑板 ----

    #[test]
    fn blackboard_typed_access_and_key_order() {
        let mut blackboard = Blackboard::new();
        // 故意乱序写入：迭代序必须由键决定，而不是写入序
        blackboard.set_f32("z", 1.5);
        blackboard.set_bool("a", true);
        blackboard.set_text("m", "mode");
        blackboard.set_vec3("v", Vec3::new(1.0, 2.0, 3.0));
        blackboard.set_entity("e", Entity::from_bits(9));
        assert_eq!(blackboard.len(), 5);
        let keys: Vec<&str> = blackboard.iter().map(|(key, _)| key).collect();
        assert_eq!(
            keys,
            ["a", "e", "m", "v", "z"],
            "迭代序 = 键序（UTF-8 字节序）"
        );
        assert_eq!(blackboard.get_f32("z"), Some(1.5));
        assert_eq!(
            blackboard.get_f32("a"),
            None,
            "类型不符 → None（不隐式转换）"
        );
        assert_eq!(
            blackboard.get_bool("z"),
            None,
            "类型不符 → None（反向亦然）"
        );
        assert!(blackboard.get_bool_or("missing", true));
        assert_eq!(blackboard.get_f32_or("a", -1.0), -1.0);
        assert_eq!(blackboard.get_text("m"), Some("mode"));
        assert_eq!(blackboard.get_entity("e"), Some(Entity::from_bits(9)));
        assert_eq!(blackboard.get_vec3("v"), Some(Vec3::new(1.0, 2.0, 3.0)));
        assert!(matches!(blackboard.get("a"), Some(BlackValue::Bool(true))));
        assert!(blackboard.remove("a").is_some());
        assert_eq!(blackboard.get("a"), None);
        blackboard.clear();
        assert!(blackboard.is_empty());
    }

    // ---- 确定性（同输入双跑逐位）----

    #[test]
    fn same_input_two_runs_are_bit_identical() {
        fn trace() -> (Vec<String>, Vec<&'static str>) {
            // 覆盖全部节点种类的一棵树（行为树的「结构即文档」）
            let tree = BehaviorTree::new(NodeSpec::sequence([
                NodeSpec::condition("c1"),
                NodeSpec::action("a1"),
                NodeSpec::inverter(NodeSpec::condition("c2")),
                NodeSpec::cooldown(NodeSpec::action("a4"), 0.25),
                NodeSpec::parallel(
                    [NodeSpec::action("a2"), NodeSpec::action("a3")],
                    ParallelPolicy::new(2, 2),
                ),
                NodeSpec::repeat(NodeSpec::action("a5"), 2),
            ]));
            let mut state = BehaviorState::new(&tree);
            let mut blackboard = Blackboard::new();
            let mut ctx = Scripted::new()
                .cond("c1", true)
                .cond("c2", false)
                .fallback(BtStatus::Running)
                .script(
                    "a1",
                    [
                        BtStatus::Success,
                        BtStatus::Success,
                        BtStatus::Running,
                        BtStatus::Success,
                    ],
                )
                .script("a4", [BtStatus::Success])
                .script(
                    "a2",
                    [BtStatus::Success, BtStatus::Running, BtStatus::Success],
                )
                .script(
                    "a3",
                    [BtStatus::Success, BtStatus::Failure, BtStatus::Success],
                );
            let mut samples: Vec<String> = Vec::new();
            for _ in 0..16 {
                tree.tick(&mut state, &mut blackboard, &mut ctx, 1.0 / 60.0);
                samples.extend(dump(&state, &blackboard));
            }
            (samples, ctx.log)
        }
        let a = trace();
        let b = trace();
        assert_eq!(a, b, "同输入两次 tick 的运行态 / 黑板摘要应逐位一致");
        assert!(a.1.len() > 16, "样本应覆盖一段真实推进（非空转）");
    }
    // ---- 构造期校验（fail loud）----

    #[test]
    #[should_panic(expected = "Repeat 的 times 须 ≥ 1")]
    fn repeat_zero_times_panics() {
        let _ = BehaviorTree::new(NodeSpec::repeat(NodeSpec::action("a"), 0));
    }

    #[test]
    #[should_panic(expected = "Parallel 的成功/失败阈值须 ≥ 1")]
    fn parallel_zero_threshold_panics() {
        let _ = BehaviorTree::new(NodeSpec::parallel(
            [NodeSpec::action("a")],
            ParallelPolicy::new(0, 1),
        ));
    }

    #[test]
    #[should_panic(expected = "Parallel 须至少一个子节点")]
    fn parallel_without_children_panics() {
        let _ = BehaviorTree::new(NodeSpec::parallel([], ParallelPolicy::new(1, 1)));
    }

    #[test]
    #[should_panic(expected = "Cooldown 的 seconds 须 ≥ 0")]
    fn cooldown_negative_seconds_panics() {
        let _ = BehaviorTree::new(NodeSpec::cooldown(NodeSpec::action("a"), -0.5));
    }

    #[test]
    #[should_panic(expected = "行为树与运行态必须同源")]
    fn mismatched_runtime_state_panics() {
        let small = BehaviorTree::new(NodeSpec::action("a"));
        let big = BehaviorTree::new(NodeSpec::sequence([
            NodeSpec::action("a"),
            NodeSpec::action("b"),
        ]));
        let mut state = BehaviorState::new(&small);
        let mut blackboard = Blackboard::new();
        let mut ctx = Scripted::new();
        let _ = big.tick(&mut state, &mut blackboard, &mut ctx, 0.1);
    }
    // ---- ECS 接入（组件 / 系统体 / 乱序 spawn 的插入序无关性）----

    /// 测试用标签组件（跨世界比较的归一化键——实体 id 随插入序变化，见下）。
    #[derive(Component)]
    struct Tag(&'static str);

    /// 测试动作集：计数写进黑板（黑板是唯一的跨帧记忆通道）。
    struct Counter;

    impl BehaviorContext for Counter {
        fn action(&mut self, id: ActionId, blackboard: &mut Blackboard, _dt: f32) -> BtStatus {
            match id.as_str() {
                "count.step" => {
                    let calls = blackboard.get_f32_or("calls", 0.0) + 1.0;
                    blackboard.set_f32("calls", calls);
                    if calls >= 3.0 {
                        BtStatus::Success
                    } else {
                        BtStatus::Running
                    }
                }
                other => panic!("测试未登记的动作 id：{other}"),
            }
        }

        fn condition(&mut self, id: ActionId, _blackboard: &Blackboard, _dt: f32) -> bool {
            panic!("测试未登记的条件 id：{id}");
        }
    }

    #[test]
    fn ecs_agents_tick_once_per_frame_and_are_insertion_order_independent() {
        /// 按插入序建世界 → 跑 5 帧 → 按标签归一收集摘要。
        fn run(order: [&'static str; 2]) -> Vec<(String, u32, String)> {
            let mut world = World::new();
            for tag in order {
                let tree = BehaviorTree::new(NodeSpec::sequence([NodeSpec::action("count.step")]));
                world.spawn((Tag(tag), BehaviorAgent::new(tree)));
            }
            for _ in 0..5 {
                tick_behavior_agents(&world, 1.0 / 60.0, |_entity, _world| Counter);
            }
            let mut out = Vec::new();
            world.query::<(&Tag, &BehaviorAgent)>(|(tag, agent)| {
                out.push((
                    tag.0.to_string(),
                    agent
                        .blackboard
                        .get_f32("calls")
                        .expect("计数器应存在")
                        .to_bits(),
                    format!("{:?}", agent.root_status()),
                ));
            });
            out.sort();
            out
        }

        let forward = run(["alpha", "beta"]);
        let reversed = run(["beta", "alpha"]);
        assert_eq!(
            forward, reversed,
            "乱序 spawn 的两个世界应按标签归一后逐位一致（实体 id 序是插入序的确定函数）"
        );
        let (_, calls, status) = &forward[0];
        assert_eq!(*calls, 5.0f32.to_bits(), "5 帧应恰好 tick 5 次（每帧一次）");
        assert_eq!(status, "Some(Success)", "第 3 帧起动作成功（黑板计数驱动）");
    }

    #[test]
    fn ents_without_agent_component_are_skipped() {
        let mut world = World::new();
        world.spawn((Tag("naked"), Transform::default()));
        let tree = BehaviorTree::new(NodeSpec::action("count.step"));
        let agent = world.spawn((Tag("agent"), BehaviorAgent::new(tree)));
        tick_behavior_agents(&world, 0.1, |_entity, _world| Counter);
        assert_eq!(
            world
                .get::<BehaviorAgent>(agent)
                .expect("代理组件应在位")
                .blackboard
                .get_f32("calls"),
            Some(1.0),
            "只有挂 BehaviorAgent 的实体被 tick"
        );
    }

    #[test]
    fn agent_reset_clears_runtime_state_and_blackboard_but_keeps_tree() {
        let tree = BehaviorTree::new(NodeSpec::action("count.step"));
        let mut agent = BehaviorAgent::new(tree.clone());
        let mut ctx = Counter;
        for _ in 0..3 {
            agent.tick(&mut ctx, 0.1);
        }
        assert_eq!(agent.blackboard.get_f32("calls"), Some(3.0));
        assert_eq!(agent.root_status(), Some(BtStatus::Success));
        agent.reset();
        assert!(agent.blackboard.is_empty(), "reset 应清黑板");
        assert_eq!(agent.root_status(), None, "reset 应清运行态记忆");
        assert_eq!(agent.tree, tree, "reset 不动树（结构是不可变纯数据）");
    }
}
