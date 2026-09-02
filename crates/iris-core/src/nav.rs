//! 导航网格与寻路 v1：静态场景的 2.5D 导航图（[`NavGrid`]）+ A* 寻路
//! （8 邻域、防穿角、确定性 tie-break）+ string pulling 路径平滑。
//!
//! 「硬编码航点敌人」时代终结：游戏侧把场景静态体喂给 [`NavGrid::build`]
//! （入参 `&[(Vec3, ColliderShape)]` 与 [`crate::physics::
//! resolve_against_statics`] 的静态体收集语义**同源**），得到一张逐 cell
//! 标记「可站 / 不可站」的导航图；之后 [`NavGrid::find_path`] 回答「从
//! 这里到那里怎么走」。纯 CPU、零 unsafe、同输入两次构建与寻路结果逐位
//! 一致（确定性论证见「寻路」章节，单测守护）。AI 行为结构（巡逻 /
//! 状态机 / 行为树）不在本模块职责内——游戏侧按需消费路径航点即可。
//!
//! # 2.5D 语义（地面平面导航）
//!
//! 导航图是 **2.5D** 的：只在地面 XZ 平面上导航，**完全忽略 y 与高度
//! 语义**——无跳跃、无高层平台、无桥上桥下。具体体现：
//!
//! - 静态体的阻挡判定取其 **XZ 投影**（竖直方向无限延伸的柱体语义，
//!   与俯视 arena 的语义一致）：高处的台面 / 悬浮物同样阻挡其正下方
//!   的地面；
//! - [`ColliderShape::Capsule`]（恒竖直，见 physics）的 XZ 投影 =
//!   圆（半径 `radius`——竖直圆柱俯视即圆，`half_height` 不参与）；
//! - 路径航点 y 恒为 0（[`NavGrid::cell_center`]）——游戏侧按地面
//!   高度自行覆写 y（v1 不做高度采样）。
//!
//! # 光栅化判定（cell 何时 blocked）
//!
//! 逐 cell 检查其**中心点**到每个静态体 XZ 投影的**最近点距离**：
//! 距离 ≤ `agent_radius` → 该 cell blocked。语义要点（实现与文档一一
//! 对应，单测锁定）：
//!
//! - **最近点判定而非中心点包含**：cell 中心在投影外、但投影与 cell
//!   面积相交（最近距离 ≤ `agent_radius`）时同样 blocked——对 agent
//!   保守（多阻的代价只是绕行，漏阻的代价是穿墙）；
//! - **投影形状**：Sphere → 圆（`radius`）；Box → AABB 矩形（角点区域
//!   按真实最近点距离判定，等价圆角膨胀）；Capsule → 圆（`radius`，
//!   竖直轴俯视即圆）；
//! - **相切算 blocked**（`≤` 判定）：恰在 `agent_radius` 处的 cell 一并
//!   阻挡——同上，保守优先；
//! - **经验规则：`cell_size` 应 ≤ `agent_radius`**。cell 中心采样是
//!   离散判定，XZ 任一方向窄于 `cell_size` 的障碍经 `agent_radius` 膨胀
//!   后仍可能与全部邻近 cell 中心相距更远而整条漏阻（单测
//!   `narrow_obstacle_miss_is_documented_semantics` 锁定该走样语义）——
//!   经验规则能覆盖常规障碍但**非数学保证**；要严格闭合需每 cell 多点
//!   采样或距离场，v1 不做（见 v1 限制）。
//!
//! # 寻路（A*，8 邻域，防穿角）
//!
//! [`NavGrid::find_path`] 的五步语义（文档即规范）：
//!
//! 1. **越界即 `None`**：from / to 任一超出覆盖域 → `None`（覆盖域即
//!    导航域，不做外扩修正——游戏侧应让覆盖域包住全部活动区）；
//! 2. **极近直达**：from 与 to 的水平距离 < 1e-6 → `Some(vec![to])`
//!    （同一点无需网格路径）；
//! 3. **就近可达性修正**：from / to 所在 cell 被阻（典型：出生点贴墙、
//!    目标点在障碍膨胀区内）→ 从被阻 cell 出发做 8 邻域 BFS（**穿过
//!    被阻格也在所不辞**——找的是几何最近的 free cell，可达性交给后续
//!    A* 裁决），半径 ≤ 2 cell 内按 BFS 发现序取第一个 free cell；半径
//!    内无 free cell → `None`。修正让游戏侧不必自己兜底「端点被阻」；
//!    修正只保证「cell 本身可站」，不保证两侧可达——不可达仍 `None`；
//! 4. **A\***：8 邻域，直线代价 1.0、对角 √2 且**两正交邻必须 free**
//!    （防穿角：贴着阻档格角点斜穿在连续世界里就是擦墙——cell 图上一致
//!    地禁止，单测锁定）；启发 = octile 距离（对 8 邻域代价**可采纳且
//!    一致**——首次弹出 goal 即 cell 图最优）；
//! 5. **平滑**：string pulling 贪心视线，见下节。
//!
//! **确定性 tie-break（本模块确定性的核心论证）**：open 集 =
//! `BinaryHeap<Reverse<(排序键, 节点序号)>>`，排序键 = (f, h)——f32 无
//! `Ord`，用 `f32::total_cmp` 全序（寻路全程 f / h 非负有限、无 NaN，
//! total_cmp 在无 NaN 时即数值序）；比较序 = f 升序 → h 升序 → 节点
//! 序号（行主序 cell 索引 = 栅格扫描序）升序。g / parent 只在**严格
//! 更优**时写入（并列保留先到者）——弹出序由该全序唯一决定；堆内可能
//! 出现完全相等的重复条目（同节点同 f 同 h 的重复入堆），但重复条目
//! 携带的状态转移完全相同（closed 检查与严格更优检查使第二次处理幂等），
//! 弹出先后不影响结果。故同输入两次 find_path 的航点序列逐位一致
//! （单测 `find_path_is_bitwise_deterministic` 守护）。
//!
//! # 路径平滑（string pulling + DDA 视线）
//!
//! A* 输出的逐 cell 折线首先转成世界航点（各 cell 中心，末航点用精确
//! 目标点替代其 cell 中心），再做 string pulling：从锚点起尽量远地找
//! **视线畅通**的最后一个航点作为下个折点，贪心前进——平滑后点数与
//! 总长均 ≤ 平滑前（欧氏下界，单测锁定）。
//!
//! 视线测试 = **cell 光栅 DDA**（Amanatides–Woo 网格遍历）：线段经过的
//! 每个 cell（含两端所在 cell）逐一查 `blocked`，任一被阻即挡。**选
//! 光栅而非几何线段-膨胀体求交的理由**：与 blocked 判定**同源**（同一
//! 张位图）——不存在「光栅化说能走、几何求交说撞墙」的二次膨胀不一致
//! 问题；O(穿越 cell 数) 廉价；整数 / 定点步进天然逐位确定。代价是
//! 走样：斜线恰好穿过格角时 t 并列，**两轴同步步进**——角点相邻的两格
//! 都检查（宁保守不放行），平滑因此可能少合并几段，正确性不受影响。
//!
//! # 动态障碍策略（v1：纯静态 + 便宜重建）
//!
//! `build` 一次成图，**不做增量更新 / 单元标记**（v1 限制）。`build`
//! 是毫秒级纯函数（逐 cell 中心到逐静态体的最近点距离，无哈希、无
//! 随机），动态障碍的推荐姿势是**便宜重建**：场景布局变化时（门开合、
//! 箱子推动后）或按固定节拍（如每 0.5~1 s）重新 `build` 一张整图换入
//! （[`NavGrid`] 是 [`Resource`]，`insert_resource` 整图替换原子生效）
//! ——正在执行的航点由游戏侧自行决定何时重算（AI 行为结构属游戏侧
//! 职责）。
//!
//! # v1 限制（诚实清单）
//!
//! - **无高度 / 跳跃语义**：2.5D 地面平面，静态体按 XZ 投影作无限高
//!   柱阻挡——高架平台 / 悬浮物阻挡其正下方地面，桥上桥下不分层；
//! - **无动态障碍增量更新**：`blocked` 位图构建后只读；动态物要么整图
//!   重建（推荐，见上节）、要么游戏侧在消费航点时自行避让（如配合
//!   物理推离查询）；
//! - **窄障碍可能漏阻**：cell 中心采样的离散性，宽 < `cell_size` 的
//!   障碍可能整条漏阻（见「光栅化判定」的经验规则与诚实边界）；
//! - **修正半径 ≤ 2 cell**：被阻端点在 2 cell 邻域内找不到 free cell
//!   即 `None`——更深的坑请游戏侧自行挪动端点；
//! - **最优性是 cell 图上的**：A* 在 8 邻域栅格图上最优（octile 一致
//!   启发），连续平面上的最短折线可以更短——平滑输出是栅格最优解的
//!   贪心视线近似，不保证全局最短。
//!
//! # 用法
//!
//! ```ignore
//! use iris_core::prelude::*;
//!
//! // 收集场景静态体（与 resolve_against_statics 的收集语义同源：
//! // is_static 或只有 Collider 无 RigidBody 的实体）
//! let statics: Vec<(Vec3, ColliderShape)> = vec![(
//!     Vec3::new(0.0, 0.0, 0.0),
//!     ColliderShape::Box { half_extents: Vec3::new(2.0, 1.0, 0.5) },
//! )];
//!
//! let nav = NavGrid::build(
//!     NavGridParams::new(Vec3::new(-20.0, 0.0, -20.0), Vec3::new(20.0, 0.0, 20.0)),
//!     &statics,
//! );
//! // 可作资源存入 World（NavGrid 实现 Resource）：
//! // world.insert_resource(nav);
//! if let Some(path) = nav.find_path(enemy_pos, player_pos) {
//!     // 逐航点走向：path[0] 为起点 cell 中心、path[末] = 目标点精确坐标
//!     // （y 恒 0，游戏侧按地面高度覆写）
//! }
//! ```

use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};

use glam::Vec3;

use crate::Resource;
use crate::physics::ColliderShape;

/// from 与 to 视为「同一点」的水平距离容差（米）。
const SAME_POINT_EPSILON: f32 = 1e-6;

/// 就近可达性修正的 BFS 半径（cell 数，见模块文档「寻路」第 3 步）。
const CORRECTION_RADIUS: usize = 2;

/// 8 邻域步进方向（**固定顺序**——修正 BFS 与 A* 的邻居展开发现序由
/// 此唯一决定，是确定性 tie-break 的组成部分）。
const NEIGHBOR_ORDER: [(isize, isize); 8] = [
    (1, 0),
    (-1, 0),
    (0, 1),
    (0, -1),
    (1, 1),
    (1, -1),
    (-1, 1),
    (-1, -1),
];

/// 导航图参数。`cell_size` 默认 0.5、`agent_radius` 默认 0.4（经验值：
/// cell 采样间距不超过 agent 半径，见模块文档「光栅化判定」的经验规则）。
#[derive(Clone, Copy, Debug)]
pub struct NavGridParams {
    /// 覆盖域 XZ 下角（y 忽略）。
    pub bounds_min: Vec3,
    /// 覆盖域 XZ 上角（y 忽略）。
    pub bounds_max: Vec3,
    /// cell 边长（米），必须为正有限。
    pub cell_size: f32,
    /// agent 膨胀半径（米），必须非负有限。
    pub agent_radius: f32,
}

impl NavGridParams {
    /// 覆盖域参数：`cell_size` 0.5 / `agent_radius` 0.4 默认值，其余
    /// 两项按需改写。
    pub fn new(bounds_min: Vec3, bounds_max: Vec3) -> Self {
        Self {
            bounds_min,
            bounds_max,
            cell_size: 0.5,
            agent_radius: 0.4,
        }
    }
}

/// 静态场景的 2.5D 导航图（语义见模块文档）：参数 + 行主序 blocked
/// 位图。纯数据结构（无系统、不进 `DefaultPlugins`），实现
/// [`Resource`] 可整图存入 World 按需换入。
#[derive(Clone, Debug, Resource)]
pub struct NavGrid {
    params: NavGridParams,
    /// X 方向 cell 数。
    cols: usize,
    /// Z 方向 cell 数。
    rows: usize,
    /// blocked 位图，行主序 `[row * cols + col]`。
    blocked: Vec<bool>,
}

/// A* open 集条目的排序键 (f, h) 的全序封装：f32 无 `Ord`，用
/// `f32::total_cmp` 手写全序——寻路全程 f / h 非负有限（g ≥ 0、octile
/// h ≥ 0），无 NaN 时 total_cmp 即数值序；即使出现 NaN 它也是全序，
/// 只会数值无意义、不会破坏确定性（见模块文档「确定性 tie-break」）。
#[derive(Clone, Copy, Debug, PartialEq)]
struct OpenKey {
    f: f32,
    h: f32,
}

// f32 无 `Eq`（NaN），但 `Ord` 的 supertrait 要求它——本结构全程无 NaN
// （f / h 非负有限，见模块文档「确定性 tie-break」），相等即 `total_cmp`
// 相等，手动实现是安全的。
impl Eq for OpenKey {}

impl Ord for OpenKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.f.total_cmp(&other.f).then(self.h.total_cmp(&other.h))
    }
}

impl PartialOrd for OpenKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl NavGrid {
    /// 构建导航图：逐 cell 中心判「到任一静态体 XZ 投影的最近点距离
    /// ≤ `agent_radius`」（语义见模块文档「光栅化判定」）。`statics`
    /// 的判定与遍历序无关（逐 cell 独立的任一命中），同输入恒同位图。
    ///
    /// 断言（中文消息）：`cell_size` 正有限、`agent_radius` 非负有限、
    /// bounds 在 XZ 上严格 min < max。`cols` / `rows` = 跨度除以
    /// `cell_size` **向上取整**（至少 1）——覆盖域完整包住 bounds，
    /// 有效域的 max 侧可能略超出 `bounds_max`（ceiling 的外沿余量）。
    pub fn build(params: NavGridParams, statics: &[(Vec3, ColliderShape)]) -> Self {
        assert!(
            params.cell_size.is_finite() && params.cell_size > 0.0,
            "NavGrid 的 cell_size 必须为正有限数，实际 {}",
            params.cell_size
        );
        assert!(
            params.agent_radius.is_finite() && params.agent_radius >= 0.0,
            "NavGrid 的 agent_radius 必须为非负有限数，实际 {}",
            params.agent_radius
        );
        assert!(
            params.bounds_min.x < params.bounds_max.x && params.bounds_min.z < params.bounds_max.z,
            "NavGrid bounds 在 XZ 上须 min < max，实际 min {:?} max {:?}",
            params.bounds_min,
            params.bounds_max
        );
        let cols = ((params.bounds_max.x - params.bounds_min.x) / params.cell_size)
            .ceil()
            .max(1.0) as usize;
        let rows = ((params.bounds_max.z - params.bounds_min.z) / params.cell_size)
            .ceil()
            .max(1.0) as usize;
        assert!(
            cols * rows <= u32::MAX as usize,
            "NavGrid 网格 {}×{} 过大：cell 序号需 u32 可索引",
            cols,
            rows
        );
        let mut blocked = Vec::with_capacity(cols * rows);
        for row in 0..rows {
            for col in 0..cols {
                let center = cell_center_of(&params, col, row);
                let mut hit = false;
                for (pos, shape) in statics {
                    if xz_clearance(center, *pos, shape) <= params.agent_radius {
                        hit = true;
                        break;
                    }
                }
                blocked.push(hit);
            }
        }
        Self {
            params,
            cols,
            rows,
            blocked,
        }
    }

    /// X 方向 cell 数。
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Z 方向 cell 数。
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// cell (col, row) 是否被阻。**越界 cell 视为被阻**（网格边界外
    /// 不可通行——保守，寻路永远不会走出覆盖域）。
    pub fn is_blocked(&self, col: usize, row: usize) -> bool {
        if col >= self.cols || row >= self.rows {
            return true;
        }
        self.blocked[row * self.cols + col]
    }

    /// 世界坐标 → cell 索引（floor 取整，负坐标正确向负取整）。越界
    /// （含负侧、超出有效域、非有限输入）返回 `None`。有效域 =
    /// [bounds_min.x, bounds_min.x + cols·cell_size) × 同 z（ceiling
    /// 外沿见 [`NavGrid::build`]）。
    pub fn world_to_cell(&self, p: Vec3) -> Option<(usize, usize)> {
        if !p.x.is_finite() || !p.z.is_finite() {
            return None;
        }
        let col = ((p.x - self.params.bounds_min.x) / self.params.cell_size).floor();
        let row = ((p.z - self.params.bounds_min.z) / self.params.cell_size).floor();
        if col < 0.0 || row < 0.0 {
            return None;
        }
        let (col, row) = (col as usize, row as usize);
        if col >= self.cols || row >= self.rows {
            return None;
        }
        Some((col, row))
    }

    /// cell 中心的世界坐标（**y 恒 0**——2.5D 地面平面，游戏侧按地面
    /// 高度覆写）。越界断言 panic（中文消息）。
    pub fn cell_center(&self, col: usize, row: usize) -> Vec3 {
        assert!(
            col < self.cols && row < self.rows,
            "cell ({col}, {row}) 越界：网格 {}×{}",
            self.cols,
            self.rows
        );
        cell_center_of(&self.params, col, row)
    }

    /// 寻路：from → to 的平滑航点序列（首点 = 起点 cell 中心、末点 =
    /// `to` 精确坐标，y 恒 0）；不可达 / 端点越界 / 端点被阻且 2 cell
    /// 内无 free cell → `None`。五步语义与确定性论证见模块文档「寻路」。
    pub fn find_path(&self, from: Vec3, to: Vec3) -> Option<Vec<Vec3>> {
        // ① 越界 → None（覆盖域即导航域，不做外扩修正）
        let start = self.world_to_cell(from)?;
        let goal = self.world_to_cell(to)?;
        // ② 极近两点：直接到达（水平距离判定，y 无关——2.5D）
        if Vec3::new(to.x - from.x, 0.0, to.z - from.z).length_squared()
            <= SAME_POINT_EPSILON * SAME_POINT_EPSILON
        {
            return Some(vec![to]);
        }
        // ③ 端点就近修正（任一端修正失败 → None）
        let start = self.correct_cell(start)?;
        let goal = self.correct_cell(goal)?;
        // ④ 修正后同 cell：cell 可站即同格内直线可达
        if start == goal {
            return Some(vec![to]);
        }
        // ⑤ A* 求栅格最优 cell 路径 → 世界航点 → 贪心视线平滑
        let cells = self.astar(start, goal)?;
        let mut waypoints: Vec<Vec3> = Vec::with_capacity(cells.len() + 1);
        for &(col, row) in &cells {
            waypoints.push(self.cell_center(col, row));
        }
        // 末航点用精确目标点替代其 cell 中心（平滑的落点是调用方要去的点）
        waypoints.push(to);
        Some(self.smooth(&waypoints))
    }

    /// 可达性判定（[`NavGrid::find_path`] 的轻量版）：同样的端点语义
    /// （越界 `false`、极近 `true`、被阻就近修正）+ 与 A* **同边集**
    /// 的 8 邻域 BFS（含防穿角）——同一张图同一条边集，A* 可达 ⇔ BFS
    /// 可达（find_path 为 `Some` ⇔ 本函数为 `true`），但不构造路径、
    /// 无堆操作，更便宜。
    pub fn is_reachable(&self, from: Vec3, to: Vec3) -> bool {
        let Some(start) = self.world_to_cell(from) else {
            return false;
        };
        let Some(goal) = self.world_to_cell(to) else {
            return false;
        };
        if Vec3::new(to.x - from.x, 0.0, to.z - from.z).length_squared()
            <= SAME_POINT_EPSILON * SAME_POINT_EPSILON
        {
            return true;
        }
        let Some(start) = self.correct_cell(start) else {
            return false;
        };
        let Some(goal) = self.correct_cell(goal) else {
            return false;
        };
        if start == goal {
            return true;
        }
        let mut visited = vec![false; self.cols * self.rows];
        visited[start.1 * self.cols + start.0] = true;
        let mut queue = VecDeque::new();
        queue.push_back(start);
        while let Some((col, row)) = queue.pop_front() {
            for &(dc, dr) in &NEIGHBOR_ORDER {
                let (ncol, nrow) = (col as isize + dc, row as isize + dr);
                if self.step_blocked(col as isize, row as isize, ncol, nrow) {
                    continue;
                }
                let nidx = nrow as usize * self.cols + ncol as usize;
                if visited[nidx] {
                    continue;
                }
                if (ncol as usize, nrow as usize) == goal {
                    return true;
                }
                visited[nidx] = true;
                queue.push_back((ncol as usize, nrow as usize));
            }
        }
        false
    }

    // -----------------------------------------------------------------
    // 内部：就近修正 / A* / 平滑 / DDA 视线（私有，供单测直接验证语义）
    // -----------------------------------------------------------------

    /// 越界敏感的 cell 阻挡查询（isize 版，DDA / 邻居展开用）：越界 =
    /// 被阻（保守，与 [`NavGrid::is_blocked`] 同语义）。
    fn blocked_isize(&self, col: isize, row: isize) -> bool {
        if col < 0 || row < 0 || col >= self.cols as isize || row >= self.rows as isize {
            return true;
        }
        self.blocked[row as usize * self.cols + col as usize]
    }

    /// 从 (col, row) 步进到 (ncol, nrow) 是否被禁止：目标格越界 / 被阻，
    /// 或对角步进时**两正交邻任一被阻**（防穿角，见模块文档「寻路」）。
    fn step_blocked(&self, col: isize, row: isize, ncol: isize, nrow: isize) -> bool {
        if self.blocked_isize(ncol, nrow) {
            return true;
        }
        let diagonal = ncol != col && nrow != row;
        if diagonal && (self.blocked_isize(ncol, row) || self.blocked_isize(col, nrow)) {
            return true;
        }
        false
    }

    /// 就近可达性修正：`cell` 被阻时从它出发做 8 邻域 BFS（半径 ≤
    /// [`CORRECTION_RADIUS`]，**穿过被阻格也在所不辞**——找几何最近的
    /// free cell，可达性交给后续寻路裁决），BFS 发现序（固定邻居顺序）
    /// 第一个 free cell 胜出；半径内无 free cell → `None`。free cell
    /// 原样返回。
    fn correct_cell(&self, cell: (usize, usize)) -> Option<(usize, usize)> {
        if !self.is_blocked(cell.0, cell.1) {
            return Some(cell);
        }
        let mut visited = vec![false; self.cols * self.rows];
        visited[cell.1 * self.cols + cell.0] = true;
        let mut queue = VecDeque::new();
        queue.push_back((cell.0 as isize, cell.1 as isize, 0usize));
        while let Some((col, row, depth)) = queue.pop_front() {
            // 深度 0 是被阻的出发格本身（前置条件），free 判定从深度 1 起
            if depth > 0 && !self.blocked_isize(col, row) {
                return Some((col as usize, row as usize));
            }
            if depth == CORRECTION_RADIUS {
                continue;
            }
            for &(dc, dr) in &NEIGHBOR_ORDER {
                let (ncol, nrow) = (col + dc, row + dr);
                if ncol < 0 || nrow < 0 || ncol >= self.cols as isize || nrow >= self.rows as isize
                {
                    continue;
                }
                let idx = nrow as usize * self.cols + ncol as usize;
                if visited[idx] {
                    continue;
                }
                visited[idx] = true;
                queue.push_back((ncol, nrow, depth + 1));
            }
        }
        None
    }

    /// A*（8 邻域、防穿角、octile 启发、确定性 tie-break——论证见模块
    /// 文档「寻路」）。返回 cell 路径（含起终点；起终点必 free）。
    /// octile 启发对该边集**一致**（每步 h 的下降量 ≤ 步代价），首次
    /// 弹出 goal 即最优，无需二次松弛。
    fn astar(&self, start: (usize, usize), goal: (usize, usize)) -> Option<Vec<(usize, usize)>> {
        let goal_idx = goal.1 * self.cols + goal.0;
        // octile 距离：(max(dx, dz) − min) · 1 + min · √2
        let heuristic = |cell: (usize, usize)| -> f32 {
            let dx = cell.0.abs_diff(goal.0);
            let dz = cell.1.abs_diff(goal.1);
            let (lo, hi) = if dx <= dz { (dx, dz) } else { (dz, dx) };
            hi as f32 + (std::f32::consts::SQRT_2 - 1.0) * lo as f32
        };
        let n = self.cols * self.rows;
        let mut g = vec![f32::INFINITY; n];
        let mut closed = vec![false; n];
        let mut parent = vec![u32::MAX; n];
        // open 集：Reverse((f, h, 节点序号)) 的最大堆 = (f, h, 序号) 的
        // 最小堆——f 最小优先，f 相同比 h，再比节点序号（确定性 tie-break）
        let mut open: BinaryHeap<Reverse<(OpenKey, u32)>> = BinaryHeap::new();
        let start_idx = start.1 * self.cols + start.0;
        g[start_idx] = 0.0;
        let h0 = heuristic(start);
        open.push(Reverse((OpenKey { f: h0, h: h0 }, start_idx as u32)));

        while let Some(Reverse((_, node))) = open.pop() {
            let node = node as usize;
            if closed[node] {
                continue; // 懒惰删除：同一节点的旧条目
            }
            closed[node] = true;
            if node == goal_idx {
                // 回溯 parent 链（行主序索引 ↔ (col, row)）
                let mut cells = Vec::new();
                let mut cur = node;
                loop {
                    cells.push((cur % self.cols, cur / self.cols));
                    let p = parent[cur];
                    if p == u32::MAX {
                        break;
                    }
                    cur = p as usize;
                }
                cells.reverse();
                return Some(cells);
            }
            let (col, row) = (node % self.cols, node / self.cols);
            let g_cur = g[node];
            for &(dc, dr) in &NEIGHBOR_ORDER {
                let (ncol, nrow) = (col as isize + dc, row as isize + dr);
                if self.step_blocked(col as isize, row as isize, ncol, nrow) {
                    continue;
                }
                let nidx = nrow as usize * self.cols + ncol as usize;
                if closed[nidx] {
                    continue;
                }
                let cost = if dc != 0 && dr != 0 {
                    std::f32::consts::SQRT_2
                } else {
                    1.0
                };
                let new_g = g_cur + cost;
                // 严格更优才写入（并列保留先到者——确定性的一部分）
                if new_g < g[nidx] {
                    g[nidx] = new_g;
                    parent[nidx] = node as u32;
                    let h = heuristic((ncol as usize, nrow as usize));
                    open.push(Reverse((OpenKey { f: new_g + h, h }, nidx as u32)));
                }
            }
        }
        None
    }

    /// string pulling 贪心视线平滑：从锚点尽量远找视线畅通的最后一个
    /// 航点作为下个折点（见模块文档「路径平滑」）。首航点保留、末航点
    /// 为精确目标点。平滑后点数与总长均 ≤ 平滑前（每段都是原路径的
    /// 直线化，欧氏距离三角不等式保证）。
    fn smooth(&self, waypoints: &[Vec3]) -> Vec<Vec3> {
        debug_assert!(
            waypoints.len() >= 2,
            "平滑输入至少两个航点，实际 {}",
            waypoints.len()
        );
        let mut out = Vec::with_capacity(waypoints.len());
        out.push(waypoints[0]);
        let mut anchor = 0usize;
        let mut next = 1usize;
        while next < waypoints.len() {
            let mut far = next;
            while far + 1 < waypoints.len()
                && self.line_of_sight(waypoints[anchor], waypoints[far + 1])
            {
                far += 1;
            }
            out.push(waypoints[far]);
            anchor = far;
            next = far + 1;
        }
        out
    }

    /// 线段视线判定（cell 光栅 DDA，选择理由见模块文档「路径平滑」）：
    /// Amanatides–Woo 网格遍历，逐一检查线段经过的 cell（含两端所在
    /// cell）；t 并列（斜线恰穿格角）时两轴同步步进——角点相邻的两格
    /// 都检查（确定性 + 保守）。任一被阻 cell（含越界）→ 无视线。
    fn line_of_sight(&self, a: Vec3, b: Vec3) -> bool {
        let to_cell_f = |v: f32, min: f32| (v - min) / self.params.cell_size;
        let (ac, ar) = (
            to_cell_f(a.x, self.params.bounds_min.x),
            to_cell_f(a.z, self.params.bounds_min.z),
        );
        let (bc, br) = (
            to_cell_f(b.x, self.params.bounds_min.x),
            to_cell_f(b.z, self.params.bounds_min.z),
        );
        let (mut col, mut row) = (ac.floor() as isize, ar.floor() as isize);
        let (end_col, end_row) = (bc.floor() as isize, br.floor() as isize);
        // 起点所在 cell 被阻（或越界）→ 无视线
        if self.blocked_isize(col, row) {
            return false;
        }
        let dcol = bc - ac;
        let drow = br - ar;
        // 步进方向取浮点分量的符号（f32 → isize 的 as 转换会截断小数，
        // 不能用于符号判定）
        let step_col = if dcol > 0.0 {
            1
        } else if dcol < 0.0 {
            -1
        } else {
            0
        };
        let step_row = if drow > 0.0 {
            1
        } else if drow < 0.0 {
            -1
        } else {
            0
        };
        // 到下一条竖 / 横格线的 t（零分量 = 恒不跨该轴，INFINITY）
        let mut t_max_col = match dcol {
            d if d > 0.0 => (col as f32 + 1.0 - ac) / d,
            d if d < 0.0 => (col as f32 - ac) / d,
            _ => f32::INFINITY,
        };
        let mut t_max_row = match drow {
            d if d > 0.0 => (row as f32 + 1.0 - ar) / d,
            d if d < 0.0 => (row as f32 - ar) / d,
            _ => f32::INFINITY,
        };
        let t_delta_col = if dcol != 0.0 {
            1.0 / dcol.abs()
        } else {
            f32::INFINITY
        };
        let t_delta_row = if drow != 0.0 {
            1.0 / drow.abs()
        } else {
            f32::INFINITY
        };
        // 步数上界：每步至少跨一列或一行（t 并列时双跨），数学上至多
        // cols + rows 步到达终点 cell；上限加裕度作防御兜底（不可达分支
        // 按无视线处理——保守，永不触发）
        let limit = self.cols + self.rows + 2;
        for _ in 0..limit {
            if col == end_col && row == end_row {
                return true; // 终点 cell 已在进入时（下方 blocked_isize）检查
            }
            if t_max_col < t_max_row {
                col += step_col;
                t_max_col += t_delta_col;
            } else if t_max_row < t_max_col {
                row += step_row;
                t_max_row += t_delta_row;
            } else {
                // t 并列：斜线恰好穿过格角——两轴同步步进，角点两侧的
                // cell 都检查（宁保守不放行，见模块文档）
                col += step_col;
                row += step_row;
                t_max_col += t_delta_col;
                t_max_row += t_delta_row;
            }
            if self.blocked_isize(col, row) {
                return false;
            }
        }
        false
    }
}

/// cell 中心的世界坐标（自由函数，[`NavGrid::build`] 与
/// [`NavGrid::cell_center`] 共用；y 恒 0，见模块文档「2.5D 语义」）。
fn cell_center_of(params: &NavGridParams, col: usize, row: usize) -> Vec3 {
    Vec3::new(
        params.bounds_min.x + (col as f32 + 0.5) * params.cell_size,
        0.0,
        params.bounds_min.z + (row as f32 + 0.5) * params.cell_size,
    )
}

/// 点（按 XZ 平面点处理，y 忽略）到静态体 XZ 投影的最近点距离（2.5D
/// 投影语义，见模块文档「光栅化判定」）：Sphere / Capsule → 圆（竖直
/// 胶囊俯视即圆，y 与 `half_height` 不参与）；Box → 矩形（角点按真实
/// 最近点距离，等价圆角膨胀）。投影内部距离 0。sqrt 为 IEEE 精确舍入
/// （跨平台逐位一致，不用 hypot——libm 实现无跨平台位级保证）。
fn xz_clearance(point: Vec3, pos: Vec3, shape: &ColliderShape) -> f32 {
    let dx = point.x - pos.x;
    let dz = point.z - pos.z;
    match *shape {
        ColliderShape::Sphere { radius } | ColliderShape::Capsule { radius, .. } => {
            ((dx * dx + dz * dz).sqrt() - radius).max(0.0)
        }
        ColliderShape::Box { half_extents } => {
            let ox = (dx.abs() - half_extents.x).max(0.0);
            let oz = (dz.abs() - half_extents.z).max(0.0);
            (ox * ox + oz * oz).sqrt()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试网格：bounds (-10, 0, -10)..(10, 0, 10)、cell 1.0、agent 0.5
    /// → 20×20 网格，cell (col, row) 的中心 = (-10 + col + 0.5, 0, -10 + row + 0.5)。
    fn test_grid(statics: &[(Vec3, ColliderShape)]) -> NavGrid {
        let mut params =
            NavGridParams::new(Vec3::new(-10.0, 0.0, -10.0), Vec3::new(10.0, 0.0, 10.0));
        params.cell_size = 1.0;
        params.agent_radius = 0.5;
        NavGrid::build(params, statics)
    }

    /// 测试网格的 (col, row) → 中心世界坐标。
    fn center(col: isize, row: isize) -> Vec3 {
        Vec3::new(-10.0 + col as f32 + 0.5, 0.0, -10.0 + row as f32 + 0.5)
    }

    fn box_shape(half_x: f32, half_z: f32) -> ColliderShape {
        ColliderShape::Box {
            half_extents: Vec3::new(half_x, 1.0, half_z),
        }
    }

    fn path_len(path: &[Vec3]) -> f32 {
        path.windows(2).map(|w| (w[1] - w[0]).length()).sum::<f32>()
    }

    /// 线段与 z = 0 的交点 x（调用方保证两端异号）。
    fn crossing_x(a: Vec3, b: Vec3) -> f32 {
        let t = a.z / (a.z - b.z);
        a.x + (b.x - a.x) * t
    }

    // ---- 光栅化（测试组 1）----

    /// 单盒膨胀光栅化：盒 half (1, 1) + agent 0.5 → 中心与盒 XZ 距离
    /// ≤ 0.5 的 cell 全 blocked（含相切），角点 cell（对角距离 √0.5²+
    /// 0.5² ≈ 0.707 > 0.5）free——最近点判定的精确断言。
    #[test]
    fn box_inflation_rasterizes_expected_cells() {
        let grid = test_grid(&[(Vec3::ZERO, box_shape(1.0, 1.0))]);
        // 被阻 = 中心距盒 XZ（[-1,1]²）≤ 0.5 的 cell：cols/rows 8..=11
        // 的 4×4 块去掉 4 个角点 cell（cell (c, r) 中心 = (c-9.5, r-9.5)）
        for &(col, row) in &[
            (9, 9),
            (9, 10),
            (10, 9),
            (10, 10), // 中心 4 格（中心 (±0.5, ±0.5) 在盒内）
            (8, 9),
            (8, 10),
            (11, 9),
            (11, 10), // 轴向相切（中心 (±1.5, ±0.5)，距 0.5 = agent_radius）
            (9, 8),
            (10, 8),
            (9, 11),
            (10, 11), // 轴向相切（中心 (±0.5, ±1.5)）
        ] {
            assert!(grid.is_blocked(col, row), "cell ({col}, {row}) 应被阻");
        }
        // 角点 cell（中心 (±1.5, ±1.5)，对角距 √(0.5²+0.5²) ≈ 0.707）
        // 与外圈 free
        for &(col, row) in &[
            (8, 8),
            (11, 8),
            (8, 11),
            (11, 11), // 4 个角点 cell
            (12, 10),
            (10, 12),
            (12, 12),
            (7, 10), // 外圈（距 ≥ 1.5）
        ] {
            assert!(
                !grid.is_blocked(col, row),
                "cell ({col}, {row}) 应 free（角点/外圈）"
            );
        }
        // 全图被阻数 = 12（4×4 中心块去掉 4 个角点 cell）
        let blocked_count = (0..grid.rows())
            .flat_map(|row| (0..grid.cols()).map(move |col| (col, row)))
            .filter(|&(col, row)| grid.is_blocked(col, row))
            .count();
        assert_eq!(blocked_count, 12, "被阻 cell 应恰为 12 个");
    }

    /// 球投影：r = 1 的球俯视是半径 1 的圆，膨胀 0.5 → 中心距离 ≤ 1.5
    /// 的 cell blocked——采样格下恰为中心 4 格（(±0.5, ±0.5) 距 0.707），
    /// (±1.5, ±0.5) 距 1.581 free。
    #[test]
    fn sphere_projection_uses_circle() {
        let grid = test_grid(&[(Vec3::ZERO, ColliderShape::Sphere { radius: 1.0 })]);
        // blocked：中心距球心 ≤ 1.5 的 cell——采样格下恰为中心 4 格
        // ((±0.5, ±0.5) 距 0.707)；(±1.5, ±0.5) 距 1.581 free
        for &(col, row) in &[(10, 10), (9, 10), (10, 9), (9, 9)] {
            assert!(grid.is_blocked(col, row), "cell ({col}, {row}) 应被阻");
        }
        for &(col, row) in &[(11, 10), (10, 11), (11, 11), (12, 10), (10, 12)] {
            assert!(!grid.is_blocked(col, row), "cell ({col}, {row}) 应 free");
        }
        let blocked_count = (0..grid.rows())
            .flat_map(|row| (0..grid.cols()).map(move |col| (col, row)))
            .filter(|&(col, row)| grid.is_blocked(col, row))
            .count();
        assert_eq!(blocked_count, 4, "球投影应恰阻 4 格");
    }

    /// 胶囊投影：竖直胶囊俯视即圆（`half_height` 不参与）——radius 0.5
    /// 的胶囊（超高半高）与同半径球的光栅化结果逐位一致。
    #[test]
    fn capsule_projection_is_circle_ignoring_height() {
        let tall_capsule = test_grid(&[(
            Vec3::ZERO,
            ColliderShape::Capsule {
                radius: 0.5,
                half_height: 50.0,
            },
        )]);
        let sphere = test_grid(&[(Vec3::ZERO, ColliderShape::Sphere { radius: 0.5 })]);
        for row in 0..tall_capsule.rows() {
            for col in 0..tall_capsule.cols() {
                assert_eq!(
                    tall_capsule.is_blocked(col, row),
                    sphere.is_blocked(col, row),
                    "胶囊（half_height=50）与同半径球的光栅化应在 cell ({col}, {row}) 一致"
                );
            }
        }
        // 圆语义抽查：中心 4 格 blocked、(±1.5, ±0.5)（距 1.581 > 1.0）free
        assert!(tall_capsule.is_blocked(10, 10));
        assert!(!tall_capsule.is_blocked(11, 10));
        assert!(!tall_capsule.is_blocked(10, 11));
    }

    /// 窄障碍语义（所选判定锁定）：cell 1.0 > agent 0.5 时，half 0.1
    /// 的细柱与最近 cell 中心相距 √(0.4² + 0.4²) ≈ 0.566 > 0.5 →
    /// **整条漏阻**（走样语义，模块文档「光栅化判定」诚实边界）；
    /// cell 0.25 ≤ agent 0.5 时同一障碍被正确阻住——经验规则的实证。
    #[test]
    fn narrow_obstacle_miss_is_documented_semantics() {
        let thin = (Vec3::ZERO, box_shape(0.1, 0.1));
        // 粗网格：中心 cell (10, 10) 漏阻
        let coarse = test_grid(&[thin]);
        assert!(
            !coarse.is_blocked(10, 10),
            "粗采样下窄障碍漏阻是文档化语义（cell_size > agent_radius）"
        );
        // 细网格（cell 0.25 ≤ agent 0.5）：bounds (-2,-2)..(2,2)，
        // cell (8, 8) 中心 (0.125, 0.125) 距细柱 0.035 ≤ 0.5 → blocked
        let mut fine_params =
            NavGridParams::new(Vec3::new(-2.0, 0.0, -2.0), Vec3::new(2.0, 0.0, 2.0));
        fine_params.cell_size = 0.25;
        fine_params.agent_radius = 0.5;
        let fine = NavGrid::build(fine_params, &[thin]);
        assert!(fine.is_blocked(8, 8), "细采样下窄障碍应被阻住");
    }

    /// world_to_cell / cell_center 往返：cell 中心映射回自身；负坐标
    /// floor 正确；越界（负侧 / 超出 / 非有限）返回 None。
    #[test]
    fn world_to_cell_and_cell_center_roundtrip() {
        let grid = test_grid(&[]);
        // 原点 world (0,0,0) → col/row = floor(10 / 1) = 10
        assert_eq!(grid.world_to_cell(Vec3::ZERO), Some((10, 10)));
        assert_eq!(grid.world_to_cell(center(3, 7)), Some((3, 7)));
        // 中心往返：cell_center 落回同一 cell
        let (col, row) = grid.world_to_cell(center(13, 2)).expect("界内点应有 cell");
        assert_eq!(grid.cell_center(col, row), center(13, 2));
        // 负侧与越界
        assert_eq!(
            grid.world_to_cell(Vec3::new(-10.5, 0.0, 0.0)),
            None,
            "负侧越界"
        );
        assert_eq!(
            grid.world_to_cell(Vec3::new(0.0, 0.0, 10.5)),
            None,
            "正侧越界"
        );
        assert_eq!(
            grid.world_to_cell(Vec3::new(f32::NAN, 0.0, 0.0)),
            None,
            "非有限输入返回 None"
        );
        // cell_center 越界断言
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| grid.cell_center(20, 0)));
        assert!(result.is_err(), "越界 cell_center 应 panic");
    }

    /// 越界 cell 视为被阻（is_blocked 的保守边界语义）。
    #[test]
    fn out_of_bounds_cell_is_blocked() {
        let grid = test_grid(&[]);
        assert!(grid.is_blocked(20, 0), "列越界应视为被阻");
        assert!(grid.is_blocked(0, 20), "行越界应视为被阻");
        assert!(!grid.is_blocked(19, 19), "末列末行在界内");
    }

    // ---- 绕墙（测试组 2）----

    /// 横贯墙留一道门：墙 x∈[-8, 3] ∪ [7, 8]（门 x∈[3, 7]），起点
    /// (-5, -5)、终点 (5, 5) 的直线穿过墙——路径必经门：每段与 z=0 的
    /// 交点 x 都落在门内 [3, 7]。
    #[test]
    fn wall_with_door_path_must_cross_inside_door() {
        let grid = test_grid(&[
            (Vec3::new(-2.5, 0.0, 0.0), box_shape(5.5, 0.5)),
            (Vec3::new(7.5, 0.0, 0.0), box_shape(0.5, 0.5)),
        ]);
        let path = grid
            .find_path(Vec3::new(-5.0, 0.0, -5.0), Vec3::new(5.0, 0.0, 5.0))
            .expect("有门必有路");
        assert!(path.len() >= 2, "路径至少起终两点");
        for window in path.windows(2) {
            let (a, b) = (window[0], window[1]);
            if (a.z < 0.0) != (b.z < 0.0) {
                let x = crossing_x(a, b);
                assert!(
                    (3.0..=7.0).contains(&x),
                    "穿越 z=0 的 x = {x} 应落在门内 [3, 7]"
                );
            }
        }
    }

    /// L 形墙：横墙 x∈[-4, 4] + 竖墙 x∈[3.5, 4.5]（z∈[0.5, 5.5]），
    /// 起点 (-3, -3)、终点 (-3, 3)——绕**左外角**（短于绕 L 的长边）：
    /// 全部 z=0 交点 x < -4.5。
    #[test]
    fn l_shaped_wall_path_goes_around_outer_corner() {
        let grid = test_grid(&[
            (Vec3::ZERO, box_shape(4.0, 0.5)),
            (Vec3::new(4.0, 0.0, 3.0), box_shape(0.5, 2.5)),
        ]);
        let path = grid
            .find_path(Vec3::new(-3.0, 0.0, -3.0), Vec3::new(-3.0, 0.0, 3.0))
            .expect("L 形墙两侧皆可绕，必有路");
        let mut crossings = 0;
        for window in path.windows(2) {
            let (a, b) = (window[0], window[1]);
            if (a.z < 0.0) != (b.z < 0.0) {
                crossings += 1;
                let x = crossing_x(a, b);
                assert!(
                    x < -4.5,
                    "穿越 z=0 的 x = {x} 应在左外角（< -4.5）而非穿墙或绕长边"
                );
            }
        }
        assert!(crossings >= 1, "路径应至少穿越 z=0 一次");
    }

    // ---- 防穿角（测试组 3）----

    /// 对角缝防穿角：阻档格 (13, 12) 与 (12, 13)（half 0.4 细盒恰好
    /// 各阻一格）夹出对角缝——(12,12) → (13,13) 的对角步进两正交邻
    /// 皆阻，被禁止；A* 必须绕行（代价恰 6.0，若允许穿角则对角直穿
    /// 仅 2.83——代价断言即防穿角规则的实证），raw 路径无「正交邻被阻
    /// 的对角步进」，平滑段也穿不过对角缝（长 ≥ 4.0）。
    #[test]
    fn diagonal_steps_never_cut_corners() {
        let thin_at = |col: f32, row: f32| {
            (
                Vec3::new(-10.0 + col + 0.5, 0.0, -10.0 + row + 0.5),
                box_shape(0.4, 0.4),
            )
        };
        let grid = test_grid(&[thin_at(13.0, 12.0), thin_at(12.0, 13.0)]);
        // 光栅化预检：恰好各阻一格
        assert!(grid.is_blocked(13, 12) && grid.is_blocked(12, 13));
        assert!(!grid.is_blocked(12, 12) && !grid.is_blocked(13, 13));

        let cells = grid
            .astar((12, 12), (14, 14))
            .expect("绕行路径存在（对角缝被防穿角规则封死，两侧直路通）");
        assert_eq!(cells.first(), Some(&(12, 12)), "起点在路径首");
        assert_eq!(cells.last(), Some(&(14, 14)), "终点在路径末");
        // 逐步检查：8 邻域 + 对角步进的两正交邻必须 free
        let mut total_cost = 0.0f32;
        for pair in cells.windows(2) {
            let (dc, dr) = (
                pair[1].0 as isize - pair[0].0 as isize,
                pair[1].1 as isize - pair[0].1 as isize,
            );
            assert!(
                dc.abs() <= 1 && dr.abs() <= 1 && (dc != 0 || dr != 0),
                "步进 ({dc}, {dr}) 应为 8 邻域内"
            );
            if dc != 0 && dr != 0 {
                assert!(
                    !grid.is_blocked((pair[0].0 as isize + dc) as usize, pair[0].1)
                        && !grid.is_blocked(pair[0].0, (pair[0].1 as isize + dr) as usize),
                    "对角步进 ({:?}→{:?}) 的两正交邻必须 free（防穿角）",
                    pair[0],
                    pair[1]
                );
                total_cost += std::f32::consts::SQRT_2;
            } else {
                total_cost += 1.0;
            }
        }
        assert!(
            (total_cost - 6.0).abs() < 1e-5,
            "绕行最小代价应恰为 6.0（直路代价），实际 {total_cost}"
        );

        // 平滑输出：穿不过对角缝（总长 ≥ 4.0 >> 直线 2.828），且逐段
        // DDA 畅通
        let path = grid
            .find_path(center(12, 12), center(14, 14))
            .expect("find_path 应成功");
        assert!(path_len(&path) >= 4.0, "平滑路径不得直线穿越对角缝");
        for window in path.windows(2) {
            assert!(
                grid.line_of_sight(window[0], window[1]),
                "平滑段 {:?}→{:?} 应 DDA 畅通",
                window[0],
                window[1]
            );
        }
    }

    // ---- 平滑（测试组 4）----

    /// 平滑性质：竖墙场景下 raw A* 折线被 string pulling 缩短——点数
    /// 不增、总长不增（欧氏下界）、逐段 DDA 复验畅通。
    #[test]
    fn smoothing_shortens_and_keeps_segments_clear() {
        // 竖墙：盒 half (0.5, 4) 于原点 → 列 14、15 的行 5..=14 被阻
        let grid = test_grid(&[(Vec3::ZERO, box_shape(0.5, 4.0))]);
        let from = center(5, 5);
        let to = center(15, 15);
        let cells = grid.astar((5, 5), (15, 15)).expect("绕墙上方可达");
        let mut raw: Vec<Vec3> = cells.iter().map(|&(c, r)| grid.cell_center(c, r)).collect();
        raw.push(to);
        let smoothed = grid.smooth(&raw);
        assert!(
            smoothed.len() <= raw.len(),
            "平滑后点数 {} 应 ≤ 原始 {}",
            smoothed.len(),
            raw.len()
        );
        let raw_len = path_len(&raw);
        let smoothed_len = path_len(&smoothed);
        assert!(
            smoothed_len <= raw_len + 1e-4,
            "平滑后总长 {smoothed_len} 应 ≤ 原始 {raw_len}"
        );
        assert!(
            smoothed_len < raw_len,
            "本场景平滑应实际缩短（{smoothed_len} < {raw_len}）"
        );
        assert_eq!(smoothed[0], raw[0], "首航点保留");
        assert_eq!(*smoothed.last().expect("非空"), to, "末航点为精确目标点");
        for window in smoothed.windows(2) {
            assert!(
                grid.line_of_sight(window[0], window[1]),
                "平滑段 {:?}→{:?} 应 DDA 畅通",
                window[0],
                window[1]
            );
        }
        // find_path 全链路同样给出缩短且畅通的路径
        let path = grid.find_path(from, to).expect("必有路");
        assert!(path.len() <= raw.len());
        assert!(path_len(&path) <= raw_len + 1e-4);
    }

    // ---- 不可达与就近修正（测试组 5）----

    /// 全封闭盒：四面墙（含角部搭接）围出封闭房间，内 → 外 None、
    /// is_reachable false；外 → 内同样 None。
    #[test]
    fn unreachable_enclosed_room_returns_none() {
        let walls = [
            (Vec3::new(0.0, 0.0, 3.5), box_shape(4.0, 0.5)), // 北
            (Vec3::new(0.0, 0.0, -3.5), box_shape(4.0, 0.5)), // 南
            (Vec3::new(3.5, 0.0, 0.0), box_shape(0.5, 4.0)), // 东
            (Vec3::new(-3.5, 0.0, 0.0), box_shape(0.5, 4.0)), // 西
        ];
        let grid = test_grid(&walls);
        assert!(
            grid.find_path(Vec3::ZERO, Vec3::new(8.0, 0.0, 8.0))
                .is_none(),
            "封闭盒内 → 外应 None"
        );
        assert!(
            grid.find_path(Vec3::new(8.0, 0.0, 8.0), Vec3::ZERO)
                .is_none(),
            "外 → 封闭盒内应 None"
        );
        assert!(
            !grid.is_reachable(Vec3::ZERO, Vec3::new(8.0, 0.0, 8.0)),
            "封闭盒内不可达"
        );
        assert!(
            grid.is_reachable(Vec3::new(-5.0, 0.0, -5.0), Vec3::new(5.0, 0.0, 5.0)),
            "外圈两点可达"
        );
    }

    /// 就近修正：from 落在薄墙膨胀区内（cell 被阻），1 cell 外即 free
    /// ——BFS 半径 2 内修正成功，find_path 返回 Some（游戏侧无需自己
    /// 兜底「出生点贴墙」）。
    #[test]
    fn blocked_endpoint_is_corrected_within_radius() {
        // 薄墙：x∈[-2, 2]、z∈[-0.5, 0.5]，膨胀后 z∈[-1, 1] 全带被阻
        let grid = test_grid(&[(Vec3::ZERO, box_shape(2.0, 0.5))]);
        assert!(grid.is_blocked(10, 10), "from 所在 cell (10, 10) 被阻");
        assert!(!grid.is_blocked(10, 8), "1 cell 外 (10, 8) free");
        let path = grid
            .find_path(Vec3::new(0.5, 0.0, 0.5), Vec3::new(0.5, 0.0, -3.5))
            .expect("被阻端点 1 cell 内有 free cell → 修正后成功");
        assert!(path.len() >= 2, "修正后路径至少起终两点");
        assert!(
            grid.is_reachable(Vec3::new(0.5, 0.0, 0.5), Vec3::new(0.5, 0.0, -3.5)),
            "is_reachable 与 find_path 同语义"
        );
    }

    // ---- 确定性（测试组 6）----

    /// 复杂多障碍场景：同输入两次 build + find_path，航点序列逐位一致
    ///（含被阻端点修正、绕墙、可达与不可达各案例——确定性 tie-break
    /// 与构建序无关性的端到端守护）。
    #[test]
    fn find_path_is_bitwise_deterministic() {
        let statics = [
            (Vec3::new(-2.5, 0.0, 0.0), box_shape(5.5, 0.5)),
            (Vec3::new(7.5, 0.0, 0.0), box_shape(0.5, 0.5)),
            (Vec3::ZERO, box_shape(1.0, 1.0)),
            (
                Vec3::new(-6.0, 0.0, 6.0),
                ColliderShape::Sphere { radius: 1.2 },
            ),
            (
                Vec3::new(4.0, 0.0, 6.0),
                ColliderShape::Capsule {
                    radius: 0.6,
                    half_height: 2.0,
                },
            ),
            (Vec3::new(0.0, 0.0, -7.0), box_shape(3.0, 0.4)),
        ];
        let queries = [
            (Vec3::new(-5.0, 0.0, -5.0), Vec3::new(5.0, 0.0, 5.0)),
            (Vec3::new(0.5, 0.0, 0.5), Vec3::new(-8.0, 0.0, 8.0)), // from 被阻 → 修正
            (Vec3::new(-5.0, 0.0, -5.0), Vec3::new(-6.0, 0.0, 6.0)), // to 在球内 → 修正
            (Vec3::new(8.5, 0.0, -8.5), Vec3::new(-8.5, 0.0, -8.5)),
            (Vec3::new(-5.0, 0.0, -5.0), Vec3::ZERO), // 直线被中心盒挡 → 绕行
        ];
        let build = || {
            NavGrid::build(
                NavGridParams {
                    bounds_min: Vec3::new(-10.0, 0.0, -10.0),
                    bounds_max: Vec3::new(10.0, 0.0, 10.0),
                    cell_size: 1.0,
                    agent_radius: 0.5,
                },
                &statics,
            )
        };
        let grid_a = build();
        let grid_b = build();
        for (from, to) in queries {
            let path_a = grid_a.find_path(from, to);
            let path_b = grid_b.find_path(from, to);
            assert_eq!(path_a, path_b, "同输入两次 find_path 应逐位一致");
            assert_eq!(
                grid_a.is_reachable(from, to),
                grid_b.is_reachable(from, to),
                "同输入两次 is_reachable 应一致"
            );
        }
    }

    // ---- 边界（测试组 7）----

    /// from == to / 极近两点 → `Some(vec![to])`（所选语义：单航点 =
    /// 目标点精确坐标）；越界端点 → None。
    #[test]
    fn same_point_and_out_of_bounds_edges() {
        let grid = test_grid(&[]);
        let p = Vec3::new(3.0, 0.0, 4.0);
        assert_eq!(
            grid.find_path(p, p),
            Some(vec![p]),
            "from == to 应返回单航点 [to]"
        );
        let q = Vec3::new(3.0 + 1e-7, 0.0, 4.0);
        assert_eq!(
            grid.find_path(p, q),
            Some(vec![q]),
            "极近两点（< 1e-6）应返回单航点 [to]"
        );
        assert_eq!(
            grid.find_path(Vec3::new(-20.0, 0.0, 0.0), Vec3::ZERO),
            None,
            "from 越界 → None"
        );
        assert_eq!(
            grid.find_path(Vec3::ZERO, Vec3::new(0.0, 0.0, 20.0)),
            None,
            "to 越界 → None"
        );
        assert_eq!(grid.find_path(p, p), grid.find_path(p, p), "同输入恒同输出");
    }

    /// 参数断言：cell_size 非正、agent_radius 负、bounds 倒置——全部
    /// 中文断言消息 panic。
    #[test]
    #[should_panic(expected = "cell_size 必须为正")]
    fn non_positive_cell_size_panics() {
        let mut params = NavGridParams::new(Vec3::new(-1.0, 0.0, -1.0), Vec3::new(1.0, 0.0, 1.0));
        params.cell_size = 0.0;
        let _ = NavGrid::build(params, &[]);
    }

    #[test]
    #[should_panic(expected = "agent_radius 必须为非负")]
    fn negative_agent_radius_panics() {
        let mut params = NavGridParams::new(Vec3::new(-1.0, 0.0, -1.0), Vec3::new(1.0, 0.0, 1.0));
        params.agent_radius = -0.1;
        let _ = NavGrid::build(params, &[]);
    }

    #[test]
    #[should_panic(expected = "bounds 在 XZ 上须 min < max")]
    fn inverted_bounds_panics() {
        let params = NavGridParams::new(Vec3::new(1.0, 0.0, -1.0), Vec3::new(-1.0, 0.0, 1.0));
        let _ = NavGrid::build(params, &[]);
    }
}
