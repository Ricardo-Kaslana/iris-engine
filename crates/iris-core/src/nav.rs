//! 导航网格与寻路 v1（R35 2.5D → R49 高度场语义）：静态场景的
//! **3D 地形导航图**（[`NavGrid`]）+ A* 寻路（8 邻域、防穿角、攀爬
//! 惩罚、确定性 tie-break）+ string pulling 路径平滑（3D 视线）。
//!
//! 「硬编码航点敌人」时代终结：游戏侧把场景静态体喂给 [`NavGrid::build`]
//! （入参 `&[(Vec3, ColliderShape)]` 与 [`crate::physics::
//! resolve_against_statics`] 的静态体收集语义**同源**），得到一张逐 cell
//! 标记「可站表面高度 + 可站 / 不可站」的导航图；之后
//! [`NavGrid::find_path`] 回答「从这里到那里怎么走」（航点自带表面
//! 高度 y）。纯 CPU、零 unsafe、同输入两次构建与寻路结果逐位一致
//! （确定性论证见「寻路」章节，单测守护）。AI 行为结构（巡逻 / 状态机
//! / 行为树）不在本模块职责内——游戏侧按需消费路径航点即可。
//!
//! # 高度场语义（R49 升级主线）
//!
//! 每个 cell 有一个**可站立表面高度**（[`NavGrid::surface_height`]）：
//! build 时对该 cell 中心（XZ 点）做**覆盖体顶面解析**，规则（文档即
//! 规范，单测锁定）：
//!
//! - **Box**：cell 中心落在盒 XZ 投影内 → 表面高 = 盒顶 y（顶面平坦，
//!   坡度恒 0，任何 [`NavGridParams::max_slope_radians`] 下都可站立）；
//! - **Sphere / Capsule**：cell 中心落在 XZ 投影圆内（半径 `radius`；
//!   胶囊恒竖直、只取其**上端帽**——帽心在中心上方
//!   `half_height − radius` 处）→ 表面高 = 球面 / 帽面上该点的 y，且该
//!   点**法线坡度 ≤ `max_slope_radians` 才可站立**（法线 = 径向：解析
//!   几何上，球心到表面点方向与竖直的夹角即坡度；等价判定
//!   `ρ ≤ radius·sin(max_slope)`，ρ = 中心到投影轴的水平距；capsule 的
//!   `half_height` 只抬高帽心、圆柱段侧壁不可站立）；
//! - **多静态体重叠取最高表面**：解析候选 = 覆盖该点的全部静态体顶面
//!   中**最高**者（高度相同取遍历序先者——对高度结果无影响，此规则
//!   只保证确定性，见「确定性」）；最高者不可站立（坡度超限）→ 该点
//!   **无表面**（不回落解析更低的重叠体——被上覆几何压住的可站面属于
//!   多层语义，见 v1 限制）；
//! - **埋没体不覆盖**：顶面低于隐式地面（y < 0）的体在该点视同不存在
//!   ——地面 0 胜出。这正是「大埋深球 = 露出地面的丘」的语义：球面冒
//!   出地面（顶面 ≥ 0）的坡段参与解析，埋在地下的部分既不覆盖也不
//!   侧挡（侧挡判定见「光栅化判定」）；
//! - **无任何覆盖 → 隐式地面 y = 0**：导航域架设在 y = 0 平面上
//!   （R35 平地世界语义的延续——旧世界没有任何几何充当「地板」而全部
//!   地面 cell 可走，正是此隐式面；场景地板不在 y = 0 时，请用 Box
//!   静态体把地板顶面显式喂进来，或平移世界使地板顶面 = 0）。**覆盖
//!   情形不回落隐式地面**（浮空台面下方、陡面下方的地面不可走——桥上
//!   桥下不分层，v1 限制）。
//!
//! # 光栅化判定（cell 何时 blocked——地面 / 顶面双层 + 可达顶面雕刻）
//!
//! blocked 由**两阶段**决定：
//!
//! **① 表面解析与侧挡（双层判定）**：
//!
//! - **覆盖格**（中心落在某静态体 XZ 投影内、且该体顶面在隐式地面
//!   之上）：按「高度场语义」的 winner 规则给出 offered（可站立顶面）
//!   或被阻（顶面坡度超限——被覆坡挡死，不回落地面）。顶面低于地面
//!   的埋没体不覆盖该点（地面 0 胜出——大埋深球只冒出一个「丘」）；
//! - **地面格**（无上覆可站面）：被阻 ⇔ 到某静态体 XZ 投影的**最近点
//!   距离 ≤ `agent_radius`** 且该体在 agent 圆盘最内缘的**侧障高度 >
//!   `step_height`**（> step 的立面是绕不过的墙；≤ step 的矮体 / 缓丘
//!   可迈可不设防）。最近点判定而非中心点包含（投影与 cell 面积相交
//!   即阻，对 agent 保守——多阻只绕行、漏阻即穿墙）；相切算阻（`≤`）；
//!   Sphere / Capsule → 圆、Box → 矩形（角点按真实最近点距离 = 圆角
//!   膨胀）。悬浮于空中的体的侧挡按 R35 柱体语义保留（桥下行走属
//!   多层，见 v1 限制）。**经验规则 `cell_size` 应 ≤ `agent_radius`**
//!   （中心采样的离散判定，窄于 cell 的障碍可能整条漏阻——单测锁定该
//!   走样语义，非数学保证）。
//!
//! **兼容性论证**：既有测试与两个游戏的全部场景里，几何都是「底贴 /
//! 埋入地面、顶面离地 > `step_height`」的立墙形态（盒顶 ≥ 1 m、球 /
//! 胶囊的帽与环都在地面或以上），上述双层判定与 R35 的纯柱体位图
//! **逐位一致**；可站立顶面全是离地 > `step_height` 的孤岛（见 ②），
//! 雕刻结果为空集 ⇒ 高度场在可走格上恒 0 ⇒ 边权与路径完全不变——
//! 既有测试零改动全过即验收（单测守护）。
//!
//! **② 可达顶面雕刻（R49 新增）**：中心落在**可站立顶面**上的被阻 cell
//! （如台阶顶、平台面），若经「|Δh| ≤ `step_height` 的 8 邻域边链」
//! 与任一地面 cell 连通 → 从 blocked 中**释放**（可站于表面高度）。若
//! 顶面与四周地面高差超过 `step_height`（孤立高台）→ 保持被阻（孤岛
//! 不可达）。实现 = 确定性 BFS 泛滥：种子 = 全部非被阻地面格（隐式
//! 地面 0），沿 8 邻域边（对角步进要求两正交邻同为「有可站表面的潜在
//! 可走格」且高度与两端点互差 ≤ `step_height`——防穿角规则的高度化
//! 版本）释放顶面格。
//!
//! # 连通性与攀爬
//!
//! 相邻（8 邻域）两个可走 cell 的**边可走**条件 = 两 cell 都可站
//! （非 blocked）且 **|Δh| ≤ `step_height`**——低→高是迈台阶、高→低是
//! 自然小落差（`step_height` 默认 0.3，与角色控制器
//! [`crate::character::CharacterParams::step_height`] 同值同语义：
//! 「角色一步能迈的坎」）；落差更大 = 跳 / 坠落语义，v1 不做（见限制）。
//! 对角步进沿用 R35 防穿角规则：**两正交邻必须可走**（原语义保留）。
//!
//! # 寻路（A*，8 邻域，防穿角，攀爬惩罚）
//!
//! [`NavGrid::find_path`] 的五步语义（文档即规范）：
//!
//! 1. **越界即 `None`**：from / to 任一超出覆盖域 → `None`（覆盖域即
//!    导航域，不做外扩修正——游戏侧应让覆盖域包住全部活动区）；
//! 2. **极近直达**：from 与 to 的水平距离 < 1e-6 → `Some(vec![to])`
//!    （同一点无需网格路径；短路分支保持入参原样——v1 兼容）；
//! 3. **就近可达性修正**：from / to 所在 cell 被阻（典型：出生点贴墙、
//!    目标点在障碍膨胀区内）→ 从被阻 cell 出发做 8 邻域 BFS（**穿过
//!    被阻格也在所不辞**——找的是几何最近的 free cell，可达性交给后续
//!    A* 裁决），半径 ≤ 2 cell 内按 BFS 发现序取第一个 free cell；半径
//!    内无 free cell → `None`。修正只保证「cell 本身可站」，不保证两侧
//!    可达——不可达仍 `None`；
//! 4. **A\***：直线代价 1.0、对角 √2；**攀爬惩罚**——上坡边
//!    （Δh > 0）代价 ×(1 + `2·Δh / step_height`)（Δh = 0.28 ≈ 台阶上限
//!    时 ×2.87：爬台阶贵于绕平路，语义上「能不爬就不爬」；代价公式与
//!    理由写入本文档即规范）。对角步进要求两正交邻 free（防穿角）且
//!    两端 |Δh| ≤ `step_height`。**启发 = octile 距离（cell 单位），
//!    可采纳且一致**：惩罚使每条边实际代价 ≥ 纯步距（1 或 √2），而
//!    octile 距离正是「按纯步距计算的最短距离」= 任意路径水平代价的
//!    下界，故 h ≤ 真实剩余代价（可采纳）；且 octile 是步距度量
//!    （三角不等式 ⇒ 相邻两点 h 差 ≤ 步距 ≤ 边代价）——一致 ⇒ 首次
//!    弹出 goal 即加权图最优，closed 集无二次松弛（与 R35 相同的证明
//!    骨架，惩罚不破坏它）；
//! 5. **平滑**：string pulling 贪心 3D 视线，见下节。
//!
//! **航点带 y**：A* 输出的逐 cell 航点 y = 该 cell 的**表面高度**（非
//! R35 的恒 0——游戏侧无需再覆写）；末航点 = 目标点精确 XZ + 目标 cell
//! 的表面高度 y。
//!
//! **确定性 tie-break（本模块确定性的核心论证）**：open 集 =
//! `BinaryHeap<Reverse<(排序键, 节点序号)>>`，排序键 = (f, h)——f32 无
//! `Ord`，用 `f32::total_cmp` 全序（寻路全程 f / h 非负有限、无 NaN，
//! total_cmp 在无 NaN 时即数值序）；比较序 = f 升序 → h 升序 → 节点
//! 序号（行主序 cell 索引 = 栅格扫描序）升序。g / parent 只在**严格
//! 更优**时写入（并列保留先到者）——弹出序由该全序唯一决定；堆内可能
//! 出现完全相等的重复条目（同节点同 f 同 h 的重复入堆），但重复条目
//! 携带的状态转移完全相同（closed 检查与严格更优检查使第二次处理幂等），
//! 弹出先后不影响结果。故同输入两次 find_path 的航点序列逐位一致。
//! 高度解析 / 雕刻 BFS 同样无迭代序敏感（雕刻边的门条件只读静态的
//! 解析结果与位图，不依赖发现序；等值顶面取遍历序先者）——同输入
//! 两次 build 的位图与高度场逐位一致（单测守护）。
//!
//! # 路径平滑（string pulling + 3D 视线）
//!
//! A* 输出的逐 cell 折线首先转成世界航点（各 cell 中心的表面高度，
//! 末航点用精确目标点替代其 cell 中心），再做 string pulling：从锚点起
//! 尽量远地找**视线畅通**的最后一个航点作为下个折点，贪心前进——平滑
//! 后点数与总长均 ≤ 平滑前（欧氏下界）。
//!
//! 视线测试 = **cell 光栅 DDA（Amanatides–Woo，R35 原样）+ 高度场逐格
//! 校验**：线段经过的每个 cell（含两端所在 cell）逐一查 blocked（任一
//! 被阻即无视线）；再对该 cell 在**线段进入点**处的插值 y 做
//! `|y − 表面高度| ≤ step_height` 校验（任一点不满足 → 挡）。**选光栅 +
//! 边界点采样的理由**（对比「投影到 2D 检测」）：XZ 可走性与 blocked
//! 判定**同源**（同一张位图，无二次膨胀不一致——R35 论证原样成立）；
//! 高度场逐 cell 恒定、线段 y 线性 ⇒ |y − h| 在 cell 内部的最大值必在
//! 进入 / 离开边界点上——按 DDA 步进点采样是**数学等价**的完备采样
//! （不会像固定大步长均匀采样那样漏掉窄高台，也无「采样相位」这类
//! 逐位不确定因素），且 DDA 本身逐位确定、O(穿越 cell 数)。DDA 走样
//! 语义保留：斜线恰穿格角时 t 并列 → 两轴同步步进、角点两格都查（宁
//! 保守不放行）。容差 ≤ `step_height` 的含义：直线航段贴着地表差 ≤ 一
//! 步坎的起伏（含 ≤ step 的台阶、浅坡、自然下落）都算视线畅通——消费
//! 侧角色控制器本身就能迈 ≤ step 的坎（两参数同源对齐）；更大的穿台 /
//! 悬空会被挡下，折点因此保留。
//!
//! # 动态障碍策略（v1：纯静态 + 便宜重建）
//!
//! `build` 一次成图，**不做增量更新 / 单元标记**（v1 限制）。`build`
//! 是毫秒级纯函数（逐 cell 中心到逐静态体的解析与最近点距离，无哈希、
//! 无随机），动态障碍的推荐姿势是**便宜重建**：场景布局变化时（门开合、
//! 箱子推动后）或按固定节拍（如每 0.5~1 s）重新 `build` 一张整图换入
//! （[`NavGrid`] 是 [`Resource`]，`insert_resource` 整图替换原子生效）
//! ——正在执行的航点由游戏侧自行决定何时重算（AI 行为结构属游戏侧
//! 职责）。
//!
//! # v1 限制（诚实清单）
//!
//! - **多层导航留白（桥洞 / 上下叠层）**：高度场是「每 XZ 点一个可站
//!   面」的单层模型——被上覆几何压住的面不可解析、悬浮物下方地面不可
//!   走（一律按被阻）；「桥上走 / 桥下钻」的跨层语义 v1 不做（R49
//!   只覆盖单层地形的坡面 / 台阶 / 丘）；`|Δh| > step_height` 的落差
//!   不可跨越（无跳跃 / 无自由落体下落语义）；浮空台面若顶面可站且
//!   经台阶链可达则可上（顶面是同一层），但其下方的地面不可走；
//! - **隐式地面 y = 0**：无覆盖点的可站面 = 0 平面（见「高度场语义」），
//!   与场景真实地板不符时需显式喂 Box 或平移场景；
//! - **无动态障碍增量更新**：`blocked` 位图构建后只读；动态物要么整图
//!   重建（推荐，见上节）、要么游戏侧在消费航点时自行避让；
//! - **窄障碍可能漏阻**：cell 中心采样的离散性（见「光栅化判定」）；
//! - **修正半径 ≤ 2 cell**：被阻端点在 2 cell 邻域内找不到 free cell
//!   即 `None`；
//! - **最优性是 cell 图上的**：A* 在 8 邻域栅格图上最优（加权代价 +
//!   octile 一致启发），连续空间的最短折线可以更短——平滑输出是栅格
//!   最优解的贪心视线近似，不保证全局最短；攀爬惩罚只改变边权、不改变
//!   图结构，最优性论证不因惩罚失效。
//!
//! # 用法
//!
//! ```ignore
//! use iris_core::prelude::*;
//!
//! // 收集场景静态体（与 resolve_against_statics 的收集语义同源：
//! // is_static 或只有 Collider 无 RigidBody 的实体）。例：平地 + 一级
//! // 0.28 m 台阶（≤ step_height 0.3，可迈上）——R49 斜坡地形接入示例：
//! // 台阶 / 坡面只要把「地表几何」按真实高度喂进来，导航图自动解析
//! // 表面高度并允许 ≤ step_height 的逐级攀升（台阶每级 ≤ 0.3、坡面
//! // 每 cell 落差 ≤ 0.3 且表面坡度 ≤ max_slope 45° 时畅通；更陡的段
//! // 会按不可站处理，路径绕行）。
//! let statics: Vec<(Vec3, ColliderShape)> = vec![
//!     // 0.28 m 台阶（x∈[0,1]、z∈[-0.5,0.5]，顶面 y = 0.28）
//!     (
//!         Vec3::new(0.5, 0.14, 0.0),
//!         ColliderShape::Box { half_extents: Vec3::new(0.5, 0.14, 0.5) },
//!     ),
//! ];
//!
//! let nav = NavGrid::build(
//!     NavGridParams::new(Vec3::new(-20.0, 0.0, -20.0), Vec3::new(20.0, 0.0, 20.0)),
//!     &statics,
//! );
//! // 可作资源存入 World（NavGrid 实现 Resource）：
//! // world.insert_resource(nav);
//! if let Some(path) = nav.find_path(enemy_pos, player_pos) {
//!     // 逐航点走向：path[0] 为起点 cell 中心、path[末] = 目标点精确
//!     // XZ + 目标 cell 表面高；各航点 y = 对应 cell 的表面高度
//!     //（台阶顶上的航点 y = 0.28，游戏侧无需再覆写）
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
/// cell 采样间距不超过 agent 半径，见模块文档「光栅化判定」的经验规则）、
/// `step_height` 默认 0.3 与 `max_slope_radians` 默认 45°（与角色控制器
/// [`crate::character::CharacterParams`] 同值对齐，见模块文档「连通性与
/// 攀爬」）。
#[derive(Clone, Copy, Debug)]
pub struct NavGridParams {
    /// 覆盖域 XZ 下角（y 忽略——高度由静态体解析，见模块文档）。
    pub bounds_min: Vec3,
    /// 覆盖域 XZ 上角（y 忽略）。
    pub bounds_max: Vec3,
    /// cell 边长（米），必须为正有限。
    pub cell_size: f32,
    /// agent 膨胀半径（米），必须非负有限。
    pub agent_radius: f32,
    /// **可迈台阶高（米）**：相邻可站 cell 的 |Δh| 不超过它才可连通
    /// （低→高是迈台阶、高→低是自然小落差；上坡边另有攀爬代价惩罚，
    /// 见模块文档「连通性与攀爬」/「寻路」）。必须为非负有限；取 0 =
    /// 只允许绝对平地的边。默认 0.3，与角色控制器
    /// [`crate::character::CharacterParams::step_height`] 语义对齐
    /// （导航允许的落差 = 角色一步能迈的坎，两参数同源）。
    pub step_height: f32,
    /// **可站立表面最大坡度（弧度）**：Sphere / Capsule 顶面的表面点
    /// 法线与竖直的夹角超过它 → 该点不可站立（cell 被阻）；Box 顶面
    /// 恒平坦不受限。必须为有限且 ∈ [0, π/2]。默认 45°（
    /// [`std::f32::consts::FRAC_PI_4`]）。
    pub max_slope_radians: f32,
}

impl NavGridParams {
    /// 覆盖域参数：`cell_size` 0.5 / `agent_radius` 0.4 / `step_height`
    /// 0.3 / `max_slope_radians` 45° 默认值，其余两项按需改写。
    pub fn new(bounds_min: Vec3, bounds_max: Vec3) -> Self {
        Self {
            bounds_min,
            bounds_max,
            cell_size: 0.5,
            agent_radius: 0.4,
            step_height: 0.3,
            max_slope_radians: std::f32::consts::FRAC_PI_4,
        }
    }
}

/// 静态场景的 3D 地形导航图（语义见模块文档）：参数 + 行主序 blocked
/// 位图 + 每 cell 的可站立表面高度场。纯数据结构（无系统、不进
/// `DefaultPlugins`），实现 [`Resource`] 可整图存入 World 按需换入。
#[derive(Clone, Debug, Resource)]
pub struct NavGrid {
    params: NavGridParams,
    /// X 方向 cell 数。
    cols: usize,
    /// Z 方向 cell 数。
    rows: usize,
    /// blocked 位图，行主序 `[row * cols + col]`（R35 柱体语义 +
    /// 可达顶面雕刻后的最终可走位图）。
    blocked: Vec<bool>,
    /// 每 cell 的可站立表面高度，行主序：无覆盖的隐式地面 0、可站立
    /// 覆盖体顶面的解析高度；被阻格也携带解析值（仅供查询 / 调试，
    /// 不构成通行语义——模块文档「高度场语义」）。
    heights: Vec<f32>,
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
    /// 构建导航图，三阶段（语义见模块文档）：
    ///
    /// 1. **表面解析 + 侧挡判定**：逐 cell 解析覆盖体顶面最高可站面 /
    ///    隐式地面 0，并对地面格判侧挡（R35 柱体判定在平地场景的逐位
    ///    等价形态，细节见模块文档「高度场语义」/「光栅化判定」）；
    /// 2. **可达顶面雕刻**：|Δh| ≤ `step_height` 边链与地面连通的顶面
    ///    cell 从 blocked 释放（确定性 BFS，见模块文档「光栅化判定」）；
    /// 3. 位图与高度场定型。
    ///
    /// 同输入恒同位图与高度场（遍历序无关的逐 cell 判定 + 雕刻边的门
    /// 条件只读静态数据；等值顶面取遍历序先者，见模块文档「确定性」）。
    ///
    /// 断言（中文消息）：`cell_size` 正有限、`agent_radius` 非负有限、
    /// `step_height` 非负有限、`max_slope_radians` 有限且 ∈ [0, π/2]、
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
            params.step_height.is_finite() && params.step_height >= 0.0,
            "NavGrid 的 step_height 必须为非负有限数，实际 {}",
            params.step_height
        );
        assert!(
            params.max_slope_radians.is_finite()
                && (0.0..=std::f32::consts::FRAC_PI_2).contains(&params.max_slope_radians),
            "NavGrid 的 max_slope_radians 必须为有限且 ∈ [0, π/2]，实际 {}",
            params.max_slope_radians
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
        // 阶段 1：表面解析 + 侧挡判定（逐 cell 逐体扫描）。
        //
        // 覆盖解析：候选 = XZ 投影含中心的体、且其**顶面在隐式地面 y = 0
        // 之上**（顶面 ≤ 0 = 体被地面埋住——不覆盖该点，地面 0 胜出，这
        // 是大埋深球的「丘」语义：只有冒出地面的坡段参与解析）。候选取
        // 顶面**最高**者（等值保留遍历序先者——确定性）；最高者可站立 →
        // offered（表面高度 = 其顶面）；最高者不可站立（坡度超限）→ 该点
        // **无表面**（被覆坡度挡住，不回落地面——模块文档「高度场语义」）。
        // 地面格（无 offered 覆盖）的侧挡：到某体 XZ 投影最近点距离 ≤
        // agent_radius 且该体在 agent 圆盘内的**侧障高度** > step_height
        // → 被阻（> step 的立面是墙；≤ step 的矮体 / 缓坡可迈可不设防，
        // 与 R35 在平地场景的位图逐位一致——旧场景全部立面都 > step）。
        let sin_slope = params.max_slope_radians.sin();
        let step = params.step_height;
        let n = cols * rows;
        let mut blocked35 = vec![false; n];
        let mut heights = vec![0.0f32; n];
        // offered：该点存在「可站立覆盖体顶面」（≠ 隐式地面）——雕刻阶段
        // 只释放 offered 格，隐式地面永不产出新格。
        let mut offered = vec![false; n];
        for row in 0..rows {
            for col in 0..cols {
                let idx = row * cols + col;
                let center = cell_center_of(&params, col, row);
                let mut floor_block = false;
                // best：覆盖候选里顶面最高者（surface, 是否可站立）
                let mut best: Option<(f32, bool)> = None;
                for (pos, shape) in statics {
                    if let Some(surface) = covering_surface(center, *pos, shape) {
                        // 覆盖候选只收顶面 ≥ 0 的体（低于隐式地面 → 埋没）
                        if surface >= 0.0 {
                            let standable = surface_standable(center, *pos, shape, sin_slope);
                            let higher = match best {
                                None => true,
                                Some((h, _)) => surface > h,
                            };
                            if higher {
                                best = Some((surface, standable));
                            }
                        }
                    }
                    if xz_clearance(center, *pos, shape) <= params.agent_radius
                        && side_obstacle_height(center, *pos, shape, params.agent_radius) > step
                    {
                        floor_block = true;
                    }
                }
                match best {
                    // 覆盖：可站立 → offered（blocked35 由雕刻前位图承担：
                    // 覆盖格 clearance = 0 恒命中）；不可站立 → 无表面。
                    Some((surface, true)) => {
                        blocked35[idx] = true;
                        heights[idx] = surface;
                        offered[idx] = true;
                    }
                    Some((_, false)) => {
                        blocked35[idx] = true;
                    }
                    None => {
                        // 地面格：侧挡决定被阻与否
                        blocked35[idx] = floor_block;
                    }
                }
            }
        }
        // 阶段 2：可达顶面雕刻（确定性 BFS）。种子 = 全部非被阻地面格
        //（隐式地面，高度 0）；释放 = 被阻但 offered 且与已走区域存在
        // |Δh| ≤ step_height 边（对角边另须两正交邻为潜在可走格且高度
        // 与两端互差 ≤ step_height——防穿角规则的高度化，门条件全静态，
        // 结果与发现序无关）。孤岛顶面（如 2 m 高台顶）保持被阻。
        let mut walkable = blocked35.iter().map(|&b| !b).collect::<Vec<bool>>();
        let mut queue = VecDeque::with_capacity(n);
        for (idx, &w) in walkable.iter().enumerate() {
            if w {
                queue.push_back(idx);
            }
        }
        while let Some(idx) = queue.pop_front() {
            let (col, row) = (idx % cols, idx / cols);
            let h_cur = heights[idx];
            for &(dc, dr) in &NEIGHBOR_ORDER {
                let (ncol, nrow) = (col as isize + dc, row as isize + dr);
                if ncol < 0 || nrow < 0 || ncol >= cols as isize || nrow >= rows as isize {
                    continue;
                }
                let nidx = nrow as usize * cols + ncol as usize;
                if walkable[nidx] {
                    continue; // 地面种子或已雕刻
                }
                if !offered[nidx] {
                    continue; // 无覆盖或顶面不可站立 → 永不释放
                }
                let h_n = heights[nidx];
                if (h_n - h_cur).abs() > step {
                    continue; // 落差超过一步坎
                }
                if dc != 0 && dr != 0 {
                    // 防穿角（高度化）：两正交邻须为潜在可走格（地面
                    // free 或 offered），且其高度与对角两端互差 ≤ step
                    let mut ok = true;
                    for &(oc, or) in &[(dc, 0), (0, dr)] {
                        let (ocol, orow) = (col as isize + oc, row as isize + or);
                        if ocol < 0 || orow < 0 || ocol >= cols as isize || orow >= rows as isize {
                            ok = false;
                            break;
                        }
                        let oidx = orow as usize * cols + ocol as usize;
                        let potential = !blocked35[oidx] || offered[oidx];
                        if !potential {
                            ok = false;
                            break;
                        }
                        let h_o = heights[oidx];
                        if (h_o - h_n).abs() > step || (h_o - h_cur).abs() > step {
                            ok = false;
                            break;
                        }
                    }
                    if !ok {
                        continue;
                    }
                }
                walkable[nidx] = true;
                queue.push_back(nidx);
            }
        }
        let blocked = walkable.into_iter().map(|w| !w).collect::<Vec<bool>>();
        Self {
            params,
            cols,
            rows,
            blocked,
            heights,
        }
    }

    /// 构建参数（只读出口，R57）：关卡格式的 `level.nav` 段往返对照与
    /// 重建核对用。参数按值返回（`NavGridParams` 是 `Copy`）。
    pub fn params(&self) -> NavGridParams {
        self.params
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

    /// cell 中心的世界坐标（**XZ 中心，y 恒 0**——R35 兼容的几何查询；
    /// 高度语义请用 [`NavGrid::surface_height`] / 寻路输出的航点 y）。
    /// 越界断言 panic（中文消息）。
    pub fn cell_center(&self, col: usize, row: usize) -> Vec3 {
        assert!(
            col < self.cols && row < self.rows,
            "cell ({col}, {row}) 越界：网格 {}×{}",
            self.cols,
            self.rows
        );
        cell_center_of(&self.params, col, row)
    }

    /// cell 中心的可站立表面高度（米）：无覆盖 → 隐式地面 0；覆盖体顶面
    /// 可站立 → 顶面在该点的 y（模块文档「高度场语义」）。**被阻 cell 也
    /// 返回其解析值**（孤岛高台顶、坡度过滤面等——仅供查询 / 调试，不
    /// 构成通行语义；可走性一律看 [`NavGrid::is_blocked`]）。越界断言
    /// panic（中文消息）。
    pub fn surface_height(&self, col: usize, row: usize) -> f32 {
        assert!(
            col < self.cols && row < self.rows,
            "cell ({col}, {row}) 越界：网格 {}×{}",
            self.cols,
            self.rows
        );
        self.heights[row * self.cols + col]
    }

    /// 寻路：from → to 的平滑航点序列（首点 = 起点 cell 中心、末点 =
    /// 目标点精确 XZ + 目标 cell 表面高度；各航点 y = 所在 cell 的表面
    /// 高度——模块文档「寻路」）；不可达 / 端点越界 / 端点被阻且 2 cell
    /// 内无 free cell → `None`。五步语义与确定性论证见模块文档「寻路」。
    /// 极近直达 / 修正后同 cell 的短路分支返回 `Some(vec![to])`（入参
    /// 原样保留——R35 兼容语义）。
    pub fn find_path(&self, from: Vec3, to: Vec3) -> Option<Vec<Vec3>> {
        // ① 越界 → None（覆盖域即导航域，不做外扩修正）
        let start = self.world_to_cell(from)?;
        let goal = self.world_to_cell(to)?;
        // ② 极近两点：直接到达（水平距离判定，y 无关）
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
            waypoints.push(self.cell_waypoint(col, row));
        }
        // 末航点用精确目标点替代其 cell 中心（平滑的落点是调用方要去的
        // 点）：XZ 取入参、y 校正为目标 cell 的表面高度
        waypoints.push(Vec3::new(
            to.x,
            self.heights[goal.1 * self.cols + goal.0],
            to.z,
        ));
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

    /// 可走 cell 的航点世界坐标：XZ 中心 + **表面高度 y**（寻路航点的
    /// 唯一来源）。调用方保证 cell 可走（越界 / 被阻格无航点语义）。
    fn cell_waypoint(&self, col: usize, row: usize) -> Vec3 {
        let mut p = cell_center_of(&self.params, col, row);
        p.y = self.heights[row * self.cols + col];
        p
    }

    /// 从 (col, row) 步进到 (ncol, nrow) 是否被禁止：目标格越界 / 被阻、
    /// **两端 |Δh| > `step_height`**（落差超过一步坎，见模块文档「连通性
    /// 与攀爬」），或对角步进时**两正交邻任一被阻**（防穿角，原语义
    /// 保留，见模块文档「寻路」）。调用方保证 (col, row) 界内可走。
    fn step_blocked(&self, col: isize, row: isize, ncol: isize, nrow: isize) -> bool {
        if self.blocked_isize(ncol, nrow) {
            return true;
        }
        if ncol >= 0 && nrow >= 0 && ncol < self.cols as isize && nrow < self.rows as isize {
            let h_cur = self.heights[row as usize * self.cols + col as usize];
            let h_nxt = self.heights[nrow as usize * self.cols + ncol as usize];
            if (h_nxt - h_cur).abs() > self.params.step_height {
                return true;
            }
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

    /// A*（8 邻域、防穿角、攀爬惩罚边权、octile 启发、确定性 tie-break
    /// ——论证见模块文档「寻路」）。返回 cell 路径（含起终点；起终点必
    /// free）。octile 启发对**加权边集**一致（攀爬惩罚因子 ≥ 1 ⇒ 边代价
    /// ≥ 纯步距；octile 距离相对相邻 cell 的步距满足三角不等式 ⇒ 每步
    /// h 的下降量 ≤ 边代价），首次弹出 goal 即加权图最优，无需二次松弛。
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
                // 边代价 = 纯步距（直线 1.0 / 对角 √2）× 攀爬惩罚因子
                // （上坡 Δh > 0 时 ×(1 + 2·Δh/step_height)；平地 / 下坡
                // 因子恰 1.0——平地上与 R35 代价逐位一致；可采纳性论证
                // 见模块文档「寻路」）。step_height == 0 时上坡边被
                // step_blocked 的 |Δh| ≤ 0 挡下，此处 Δh > 0 不出现。
                let base = if dc != 0 && dr != 0 {
                    std::f32::consts::SQRT_2
                } else {
                    1.0
                };
                let climb = (self.heights[nidx] - self.heights[node]).max(0.0);
                let factor = if climb > 0.0 {
                    1.0 + 2.0 * climb / self.params.step_height
                } else {
                    1.0
                };
                let new_g = g_cur + base * factor;
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

    /// 线段视线判定（cell 光栅 DDA + 高度场逐格校验，选择理由见模块
    /// 文档「路径平滑」）：Amanatides–Woo 网格遍历，逐一检查线段经过的
    /// cell（含两端所在 cell）；t 并列（斜线恰穿格角）时两轴同步步进——
    /// 角点相邻的两格都检查（确定性 + 保守）。任一被阻 cell（含越界）
    /// → 无视线；**3D 高度校验**：线段在各 cell 进入点的插值 y（起点 =
    /// 起点本身、终点另加末点 b）距该 cell 表面高度超过 `step_height`
    /// → 无视线（cell 内 |y − h| 的最大值必在进入 / 离开边界上——按
    /// DDA 步进点采样是完备采样，不会漏掉窄高台）。
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
        // 起点 y 校验：a 须贴近其所在 cell 的表面（差 ≤ step_height）
        if !self.los_height_ok(col as usize, row as usize, a.y) {
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
                // 末点 b 的 y 校验（其所在 cell 的表面差 ≤ step_height；
                // 进入点校验在下方跨入时完成）
                return self.los_height_ok(col as usize, row as usize, b.y);
            }
            // 本次跨格线的 t（并列时两轴同刻，取同一 t）
            let t_cross = t_max_col.min(t_max_row);
            let y_cross = a.y + (b.y - a.y) * t_cross;
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
            // 3D 高度校验：线段在进入该 cell 处的 y 距表面 > step_height
            // → 挡（起点已校验；末点由上方返回分支校验）
            if !self.los_height_ok(col as usize, row as usize, y_cross) {
                return false;
            }
        }
        false
    }

    /// 3D 视线高度校验：点 y 距 (col, row) 表面高度 ≤ `step_height` 才
    /// 放行（调用方保证 cell 界内可走——被阻格在进入前已被挡下）。
    fn los_height_ok(&self, col: usize, row: usize, y: f32) -> bool {
        (y - self.heights[row * self.cols + col]).abs() <= self.params.step_height
    }
}

/// cell 中心的世界坐标（自由函数，[`NavGrid::build`] 与
/// [`NavGrid::cell_center`] 共用；y 恒 0——XZ 中心查询，高度语义见
/// [`NavGrid::surface_height`]）。
fn cell_center_of(params: &NavGridParams, col: usize, row: usize) -> Vec3 {
    Vec3::new(
        params.bounds_min.x + (col as f32 + 0.5) * params.cell_size,
        0.0,
        params.bounds_min.z + (row as f32 + 0.5) * params.cell_size,
    )
}

/// 覆盖体顶面解析（自由函数，build 逐 cell 调用）：cell 中心（XZ 点，
/// y 忽略）落在静态体 XZ 投影内 → 返回 `顶面在该点的表面高`；不覆盖 →
/// `None`。Box：矩形投影，顶面 = pos.y + half_extents.y；Sphere /
/// Capsule（恒竖直）：圆投影 + 上端帽球面（帽心 = 中心 +
/// `(half_height − radius)·Y`，球取 `half_height == radius`），表面高 =
/// 帽心 y + √(r²−ρ²)（先比平方后开方——无 NaN）。返回的高度**可低于
/// 隐式地面 0**（体被地面埋没的部分）——是否参与解析由调用方按
/// 「≥ 0」过滤（模块文档「高度场语义」）。
fn covering_surface(point: Vec3, pos: Vec3, shape: &ColliderShape) -> Option<f32> {
    let dx = point.x - pos.x;
    let dz = point.z - pos.z;
    match *shape {
        ColliderShape::Box { half_extents } => {
            if dx.abs() <= half_extents.x && dz.abs() <= half_extents.z {
                Some(pos.y + half_extents.y)
            } else {
                None
            }
        }
        // OBB（C3d）：nav 的静态体快照只有 (位置, 形状)、**无旋转**——
        // 斜置 OBB 的顶面投影无法如实判定，保守不供顶面（宁缺勿错：
        // 误供矩形投影会让寻路踩到旋转平台之外的虚空）。需要导航
        // 顶面用 Box / 恒竖直胶囊 / 球（§8.60 声明）。
        ColliderShape::OrientedBox { .. } => None,
        ColliderShape::Sphere { radius } => sphere_cap_surface(dx, dz, pos.y, radius, 0.0),
        ColliderShape::Capsule {
            radius,
            half_height,
        } => sphere_cap_surface(dx, dz, pos.y, radius, half_height - radius),
    }
}

/// 球帽顶面高度（Sphere 与 Capsule 上端帽共用）：半径 `radius`、帽心
/// y = `pos_y + cap_offset`（球取 0、胶囊取 `half_height − radius`）。
/// 水平距平方 `dx²+dz² ≤ r²` 才覆盖；`(r² − ρ²).sqrt()` 恒为 sqrt(≥0)
/// （先比平方后开方——无 NaN）。
fn sphere_cap_surface(dx: f32, dz: f32, pos_y: f32, radius: f32, cap_offset: f32) -> Option<f32> {
    let rho_sq = dx * dx + dz * dz;
    let r_sq = radius * radius;
    if rho_sq > r_sq {
        return None;
    }
    Some(pos_y + cap_offset + (r_sq - rho_sq).sqrt())
}

/// 覆盖点处表面是否可站立（坡度 ≤ max_slope）：Box 顶面平坦恒可站立；
/// Sphere / Capsule 的帽面法线 = 径向，坡度 = 法线与竖直的夹角，判定
/// 用平方形式 `ρ² ≤ (r·sin(max_slope))²`（`sin_slope` 为 build 预计算
/// 的 `max_slope_radians.sin()`，避免逐 cell 三角函数；先比平方后比较
/// ——无开方边界）。模块文档「高度场语义」。
fn surface_standable(point: Vec3, pos: Vec3, shape: &ColliderShape, sin_slope: f32) -> bool {
    let dx = point.x - pos.x;
    let dz = point.z - pos.z;
    match *shape {
        ColliderShape::Box { .. } => true,
        // OBB：covering_surface 不供顶面（无旋转快照，见其注释），
        // 本判定随之恒 false（不可达配对不会进到这里，写全防语义漂移）
        ColliderShape::OrientedBox { .. } => false,
        ColliderShape::Sphere { radius } => {
            let rho_sq = dx * dx + dz * dz;
            let limit = radius * sin_slope;
            rho_sq <= limit * limit
        }
        ColliderShape::Capsule { radius, .. } => {
            let rho_sq = dx * dx + dz * dz;
            let limit = radius * sin_slope;
            rho_sq <= limit * limit
        }
    }
}

/// 侧障高度（自由函数，地面格侧挡判定用）：站在地面（y = 0）的 agent
/// 以 `agent_radius` 为圆盘半径逼近某静态体时，该体在圆盘最内缘上方
/// 的实体高度（≥ 0 才有实体冒出地面）。Box：顶面高（pos.y +
/// half_extents.y——立面是竖直墙，最内缘即顶面）；Sphere / Capsule：
/// 帽面在半径 `max(0, 距轴水平距 − agent_radius)` 处的高度（最内缘
/// 恰是帽面坡——圆盘切入球体多深，冒出的坡就有多高）。判定与 R35 的
/// 「中心距 ≤ agent_radius 即阻」在**平地场景逐位等价**（旧场景里全部
/// 侧障都 > step_height，见模块文档「光栅化判定」的兼容性论证）。
fn side_obstacle_height(point: Vec3, pos: Vec3, shape: &ColliderShape, agent_radius: f32) -> f32 {
    let dx = point.x - pos.x;
    let dz = point.z - pos.z;
    let rho_sq = dx * dx + dz * dz;
    match *shape {
        ColliderShape::Box { half_extents } => pos.y + half_extents.y,
        // OBB（无旋转快照）：取任意姿态下的最高界 = 半边长向量长度
        // （包围球顶）——侧障判定只会**偏保守**（多挡不漏放），安全方向
        ColliderShape::OrientedBox { half_extents } => pos.y + half_extents.length(),
        ColliderShape::Sphere { radius } => {
            let rho_inner = (rho_sq.sqrt() - agent_radius).max(0.0);
            sphere_cap_surface(rho_inner, 0.0, pos.y, radius, 0.0).unwrap_or(0.0)
        }
        ColliderShape::Capsule {
            radius,
            half_height,
        } => {
            let rho_inner = (rho_sq.sqrt() - agent_radius).max(0.0);
            sphere_cap_surface(rho_inner, 0.0, pos.y, radius, half_height - radius).unwrap_or(0.0)
        }
    }
}

/// 点（按 XZ 平面点处理，y 忽略）到静态体 XZ 投影的最近点距离（R35
/// 柱体阶段判定的投影语义，见模块文档「光栅化判定」）：Sphere / Capsule
/// → 圆（竖直胶囊俯视即圆，y 与 `half_height` 不参与）；Box → 矩形（角
/// 点按真实最近点距离，等价圆角膨胀）。投影内部距离 0。sqrt 为 IEEE
/// 精确舍入（跨平台逐位一致，不用 hypot——libm 实现无跨平台位级保证）。
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
        // OBB（无旋转快照）：XZ 投影的外接圆（√(hx²+hz²)）保守近似——
        // 间隙只会**偏小**（多阻不漏放），与侧障高度同款安全方向
        ColliderShape::OrientedBox { half_extents } => {
            let r = (half_extents.x * half_extents.x + half_extents.z * half_extents.z).sqrt();
            ((dx * dx + dz * dz).sqrt() - r).max(0.0)
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
                    // R49 新字段（默认值——平地世界语义与 R35 逐位一致，
                    // 见模块文档「光栅化判定」的兼容性论证）
                    step_height: 0.3,
                    max_slope_radians: std::f32::consts::FRAC_PI_4,
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

    #[test]
    #[should_panic(expected = "step_height 必须为非负")]
    fn negative_step_height_panics() {
        let mut params = NavGridParams::new(Vec3::new(-1.0, 0.0, -1.0), Vec3::new(1.0, 0.0, 1.0));
        params.step_height = -0.1;
        let _ = NavGrid::build(params, &[]);
    }

    #[test]
    #[should_panic(expected = "max_slope_radians 必须为有限")]
    fn out_of_range_max_slope_panics() {
        let mut params = NavGridParams::new(Vec3::new(-1.0, 0.0, -1.0), Vec3::new(1.0, 0.0, 1.0));
        params.max_slope_radians = 2.0; // > π/2
        let _ = NavGrid::build(params, &[]);
    }

    // ---- R49 高度场：台阶 / 丘（测试组 8）----

    /// 三段式矮台阶丘（R49 台阶语义的构造事实）：五列盒顶从 0.28 逐列
    /// 抬到 0.84 再降回 0.28（每列顶面离地 = 中心 y + 半高 y，底贴
    /// 隐式地面；每列盒顶差 0.28 ≤ step_height 0.3）。列 x 域按 1.0 cell
    /// 错位 0.1（半宽 0.5 的盒从 cell 中心错开）——相邻列盒顶在列边界
    /// 附近重叠成无地缝的连续台阶，而西 / 东端头盒面离邻近地面格中心
    /// > agent_radius（不被 R35 侧挡吞掉入口）。
    fn stair_mound() -> Vec<(Vec3, ColliderShape)> {
        let tier = |cx: f32, top: f32| {
            let (center_y, half_y) = (top * 0.5, top * 0.5);
            (
                Vec3::new(cx, center_y, 0.0),
                ColliderShape::Box {
                    half_extents: Vec3::new(0.5, half_y, 9.5),
                },
            )
        };
        vec![
            tier(-4.1, 0.28),
            tier(-3.1, 0.56),
            tier(-2.1, 0.84),
            tier(-1.1, 0.56),
            tier(-0.1, 0.28),
        ]
    }

    /// 台阶丘横穿：顶面 cell 被雕刻释放（is_blocked false、表面高度 =
    /// 盒顶 y），A* 逐列攀升且每条边 |Δh| ≤ step_height，平滑路径保留
    /// 丘顶折点且逐段 3D 视线畅通、末航点 y = 目标 cell 表面高。
    #[test]
    fn stair_mound_climbs_tier_by_tier_with_surface_heights() {
        let grid = test_grid(&stair_mound());
        // 雕刻释放 + 高度解析（五列、任意行同高——z 向整列覆盖）
        let expected_col_h = |col: usize| -> f32 {
            match col {
                5 => 0.28,
                6 => 0.56,
                7 => 0.84,
                8 => 0.56,
                9 => 0.28,
                _ => 0.0,
            }
        };
        for row in 0..20 {
            for col in 0..20 {
                let expected = expected_col_h(col);
                if expected > 0.0 {
                    assert!(
                        !grid.is_blocked(col, row),
                        "台阶顶 cell ({col}, {row}) 应被雕刻释放"
                    );
                }
                assert!(
                    (grid.surface_height(col, row) - expected).abs() <= 1e-6,
                    "cell ({col}, {row}) 表面高度应 ≈ {expected}，实际 {}",
                    grid.surface_height(col, row)
                );
            }
        }
        // A* 逐列攀升：raw 路径每格高度对得上列预期、逐边 |Δh| ≤ 0.3
        let cells = grid.astar((2, 10), (11, 10)).expect("台阶丘横穿必有路");
        let mut max_h = 0.0f32;
        for &(col, row) in &cells {
            let h = grid.surface_height(col, row);
            assert!(
                (h - expected_col_h(col)).abs() <= 1e-6,
                "raw 路径格 ({col}, {row}) 高度应匹配所在列台阶"
            );
            max_h = max_h.max(h);
        }
        assert!(
            (max_h - 0.84).abs() <= 1e-6,
            "raw 路径应爬到丘顶 0.84，实际 {max_h}"
        );
        for pair in cells.windows(2) {
            let dh = (grid.surface_height(pair[1].0, pair[1].1)
                - grid.surface_height(pair[0].0, pair[0].1))
            .abs();
            assert!(
                dh <= 0.3 + 1e-6,
                "raw 路径逐边 |Δh| = {dh} 应 ≤ step_height"
            );
        }
        // 全链路：平滑路径的逐段 3D 视线复验 + 保留丘体中段折点（丘顶
        // 0.84 与两侧 0.56 的弦差 ≤ 0.3 时顶格可被合法合并——贴丘弦仍在
        // 一步容差内；但任何 ≤ 0.3 高度的两端弦穿过 0.56/0.84 中段都会
        // 超出容差 → 平滑后必保留 ≥ 0.56 的折点），末航点 y = 目标面
        let from = center(2, 10);
        let to = center(11, 10);
        let path = grid.find_path(from, to).expect("find_path 必有路");
        assert!(!grid.line_of_sight(from, to), "跨丘直线弦应无视线（穿台）");
        assert!(path.len() >= 3, "平滑路径应保留中间折点，实际 {:#?}", path);
        let max_y = path.iter().map(|w| w.y).fold(f32::MIN, f32::max);
        assert!(
            (0.56 - 1e-6..=0.84 + 1e-6).contains(&max_y),
            "平滑路径应保留丘体中段航点（y ∈ {{0.56, 0.84}}），实际 {max_y}"
        );
        for window in path.windows(2) {
            assert!(
                grid.line_of_sight(window[0], window[1]),
                "平滑段 {:?} → {:?} 应 3D 视线畅通",
                window[0],
                window[1]
            );
        }
        assert_eq!(*path.last().expect("非空"), to, "末航点 = 目标 XZ + 表面高");
    }

    /// 超 step_height 的单级高台（1.0 m、z 向全跨）：顶面离地面
    /// 1.0 > step_height 0.3 → 孤岛不雕刻，整列被阻 → 横穿 None、
    /// is_reachable false（「None 或绕行」的 None 支）。
    #[test]
    fn single_high_step_above_step_height_blocks_column() {
        let high = (
            Vec3::new(-4.5, 0.5, 0.0),
            ColliderShape::Box {
                half_extents: Vec3::new(0.5, 0.5, 9.5),
            },
        );
        let grid = test_grid(&[high]);
        // 顶面 cell 全部保持被阻（孤岛不雕刻），但高度解析仍在（供查询）
        for row in 0..20 {
            assert!(grid.is_blocked(5, row), "高台列 (5, {row}) 应被阻");
            assert!(
                (grid.surface_height(5, row) - 1.0).abs() <= 1e-6,
                "高台顶解析高度应 = 1.0"
            );
        }
        assert!(
            grid.find_path(center(2, 10), center(8, 10)).is_none(),
            "全跨高台不可翻越 → None"
        );
        assert!(
            !grid.is_reachable(center(2, 10), center(8, 10)),
            "is_reachable 同语义 false"
        );
    }

    /// 高台只占一段 z（1.0 m 顶、z ∈ [−2, 2]）：路径绕行不翻越——全部
    /// 航点 y = 0 且存在 |z| ≥ 3.4 的绕行段（高台列被阻到 |z| ≤ 3.0，
    /// 见侧挡 / 顶面孤岛语义）。
    #[test]
    fn high_step_short_band_detours_on_ground() {
        let high = (
            Vec3::new(-4.5, 0.5, 0.0),
            ColliderShape::Box {
                half_extents: Vec3::new(0.5, 0.5, 2.0),
            },
        );
        let grid = test_grid(&[high]);
        let path = grid
            .find_path(center(2, 10), center(8, 10))
            .expect("高台只挡一段 z，绕行必有路");
        let mut detoured = false;
        for w in &path {
            assert_eq!(w.y, 0.0, "绕行航点 y 恒 0（不翻越）");
            if w.z.abs() >= 3.4 {
                detoured = true;
            }
        }
        assert!(detoured, "路径应绕到 |z| ≥ 3.4 的通道穿越");
    }

    /// 大埋深球 = 缓丘（R = 60、中心 y = −58.5：露出地面 1.5 m 的浅帽，
    /// 冒出环半径 ≈ 13.33 < 网格 16，环外是隐式地面）：环边顶面
    /// (≈ 0.2 起坡、每 cell 落差 ≤ 0.3) 与地面连通 → 全丘可走，路径
    /// 沿球面（航点 y 与解析球面逐位贴合），最高点 ≈ 1.5。
    #[test]
    fn buried_ball_dome_path_follows_surface() {
        let mut params =
            NavGridParams::new(Vec3::new(-16.0, 0.0, -16.0), Vec3::new(16.0, 0.0, 16.0));
        params.cell_size = 1.0;
        params.agent_radius = 0.5;
        let ball = (
            Vec3::new(0.0, -58.5, 0.0),
            ColliderShape::Sphere { radius: 60.0 },
        );
        let grid = NavGrid::build(params, &[ball]);
        // 丘顶 cell 已雕刻：中心 cell 可走、高度 ≈ 解析值 1.4958
        assert!(!grid.is_blocked(16, 15), "丘顶 cell 应可走");
        let surface = |w: Vec3| {
            let rho_sq = w.x * w.x + w.z * w.z;
            -58.5 + (3600.0 - rho_sq).sqrt()
        };
        let top_expected = surface(center_w(-16.0, 16, 15));
        assert!(
            (grid.surface_height(16, 15) - top_expected).abs() <= 1e-6,
            "丘顶高度应 = 解析球面"
        );
        // 横穿浅丘：全部航点 y 贴合球面（覆盖区）或 = 0（环外地面）
        let from = center_w(-16.0, 1, 15); // x = -14.5（环外地面）
        let to = center_w(-16.0, 26, 15); // x = 10.5（丘上）
        let path = grid.find_path(from, to).expect("缓丘横穿必有路");
        let mut max_y = 0.0f32;
        for w in &path {
            max_y = max_y.max(w.y);
            let rho_sq = w.x * w.x + w.z * w.z;
            if rho_sq <= 177.75 {
                assert!(
                    (w.y - surface(*w)).abs() <= 1e-6,
                    "丘上航点 {w:?} 应贴合球面（差 {}）",
                    (w.y - surface(*w)).abs()
                );
            } else {
                assert_eq!(w.y, 0.0, "环外航点 {w:?} y 应 = 0");
            }
        }
        assert!(max_y >= 1.4, "路径应翻过丘顶附近（最高 {max_y}）");
        for window in path.windows(2) {
            assert!(
                grid.line_of_sight(window[0], window[1]),
                "平滑段应 3D 视线畅通"
            );
        }
    }

    /// 网格坐标辅助（自由起点 bounds 的 cell 中心世界坐标；y = 0）。
    fn center_w(bounds_min: f32, col: isize, row: isize) -> Vec3 {
        Vec3::new(
            bounds_min + col as f32 + 0.5,
            0.0,
            bounds_min + row as f32 + 0.5,
        )
    }

    /// 陡球（R = 2.2、中心 y = −1.4：坡度 ≤ 45° 的顶帽高悬成孤岛，
    /// 帽外环坡度 > 45° 全阻）——横穿路径绕行不翻越：全部航点 y = 0
    /// 且离球心 ≥ 2.05（绕出被阻带）；帽顶孤岛 cell 保持被阻、高度
    /// 仍解析 ≈ 0.6833。
    #[test]
    fn steep_ball_detours_and_cap_stays_island() {
        let ball = (
            Vec3::new(0.0, -1.4, 0.0),
            ColliderShape::Sphere { radius: 2.2 },
        );
        let grid = test_grid(&[ball]);
        // 帽顶孤岛：cell (10,10)（中心 (0.5, 0.5)，ρ = 0.707 ≤
        // 2.2·sin45 ≈ 1.556 可站）→ offered 但四面高差 > step → 被阻
        assert!(grid.is_blocked(10, 10), "帽顶 cell 应为孤岛被阻");
        let expected = -1.4_f32 + (4.84_f32 - 0.5).sqrt();
        assert!(
            (grid.surface_height(10, 10) - expected).abs() <= 1e-6,
            "帽顶解析高度应 ≈ {expected}"
        );
        // 坡度过滤带（ρ ≈ 1.58 > 1.556，坡度 > 45°）无表面
        assert!(grid.is_blocked(11, 10), "陡坡带 cell 应被阻");
        assert_eq!(grid.surface_height(11, 10), 0.0, "陡坡带无表面（高度 0）");
        // 绕行：y 恒 0、不进入被阻带（cell 中心离球心 ≥ 2.05）
        let path = grid
            .find_path(center(2, 10), center(17, 10))
            .expect("绕行必有路");
        for w in &path {
            assert_eq!(w.y, 0.0, "绕行航点 y 恒 0（不翻越球面）");
            let dist_sq = w.x * w.x + w.z * w.z;
            assert!(dist_sq >= 2.05 * 2.05, "航点 {w:?} 不应进入球被阻带");
        }
    }

    // ---- R49 高度场：攀爬惩罚与 3D 视线（测试组 9）----

    /// 矮台阶跨步（b1/b2/b3 是 penalty 与平滑测试共用的构造事实）：
    /// (9,10) 顶 0.28、(10,10) 顶 0.56、(11,10) 顶 0.28——三级台阶
    /// （≤ step_height 逐级可迈，0.56 顶的下行只能经两侧 0.28 级）。
    /// 盒 z ∈ [0.1, 0.9]（半宽 0.4，中心 z = 0.5 = row 10 中心）：只盖
    /// row 10，row 9/11（z = −0.5 / 1.5）离侧障 ≥ 0.6 保持畅通的平路
    /// 绕行带。
    fn penalty_steps() -> Vec<(Vec3, ColliderShape)> {
        let step = |cx: f32, top: f32| {
            (
                Vec3::new(cx, top * 0.5, 0.5),
                ColliderShape::Box {
                    half_extents: Vec3::new(0.5, top * 0.5, 0.4),
                },
            )
        };
        vec![step(-0.5, 0.28), step(0.5, 0.56), step(1.5, 0.28)]
    }

    /// 攀爬惩罚锁定：三级短台阶（直穿 15 格中爬 0.28/0.56/0.28）vs
    /// 平路绕行（多 2 格对角）——无惩罚时直穿最优（15.0 < 15.83），
    /// 带惩罚（上坡 ×(1 + 2·Δh/step)）时绕行胜出（15.83 < 18.73）：
    /// A* 选平路（raw 全格高度 0）、平滑路径 y 恒 0；对照组把
    /// step_height 调到 10.0（惩罚因子 ≈ 1）→ A* 改选直穿台阶（raw
    /// 含 0.56 顶格）——惩罚正是选择翻转的唯一原因。
    #[test]
    fn climb_penalty_prefers_flat_detour() {
        let statics = penalty_steps();
        let mut params =
            NavGridParams::new(Vec3::new(-10.0, 0.0, -10.0), Vec3::new(10.0, 0.0, 10.0));
        params.cell_size = 1.0;
        params.agent_radius = 0.5;
        let grid = NavGrid::build(params, &statics);
        // 台阶顶被雕刻释放、高度正确（可达顶面）
        assert!(!grid.is_blocked(9, 10) && !grid.is_blocked(10, 10) && !grid.is_blocked(11, 10));
        assert!(
            (grid.surface_height(9, 10) - 0.28).abs() <= 1e-6
                && (grid.surface_height(10, 10) - 0.56).abs() <= 1e-6
        );
        let from = center(2, 10);
        let to = center(17, 10);
        let cells = grid.astar((2, 10), (17, 10)).expect("必有路");
        for &(col, row) in &cells {
            assert_eq!(
                grid.surface_height(col, row),
                0.0,
                "带惩罚时 A* 应选平路绕行（raw 格 ({col}, {row}) 高度 0），不爬台阶"
            );
        }
        let path = grid.find_path(from, to).expect("必有路");
        for w in &path {
            assert_eq!(w.y, 0.0, "绕行平滑路径航点 y 恒 0");
        }
        // 对照组：step_height 10.0 → 惩罚因子 ≈ 1，直穿（15 格）优于
        // 绕行（≈ 15.83）→ A* 爬台阶（含 0.56 顶格）——翻转锁定惩罚
        let mut lax = params;
        lax.step_height = 10.0;
        let lax_grid = NavGrid::build(lax, &statics);
        let lax_cells = lax_grid.astar((2, 10), (17, 10)).expect("对照组必有路");
        assert!(
            lax_cells
                .iter()
                .any(|&(c, r)| (lax_grid.surface_height(c, r) - 0.56).abs() <= 1e-6),
            "无惩罚时 A* 应直穿 0.56 级台阶"
        );
    }

    /// 无平路替代时的必要攀爬：走廊（两道 2.5 m 全宽墙 + 切线侧挡把
    /// 活动域夹成 row 10..11 两行）中央横一条 0.28 m 平台（cols
    /// 6..12 全盖两行）——绕行无门，A* 必须爬平台（raw 路径含 0.28
    /// 顶格）且逐边 |Δh| ≤ step_height。
    #[test]
    fn climb_used_when_no_flat_alternative() {
        let statics = vec![
            // 两道横墙：z∈[−2,−1.25] 与 z∈[1.25,2]（顶 2.5 m > step）
            (
                Vec3::new(0.0, 1.25, -1.625),
                ColliderShape::Box {
                    half_extents: Vec3::new(10.5, 1.25, 0.375),
                },
            ),
            (
                Vec3::new(0.0, 1.25, 1.625),
                ColliderShape::Box {
                    half_extents: Vec3::new(10.5, 1.25, 0.375),
                },
            ),
            // 0.28 m 平台：x∈[−4,3]、z∈[−1,1]（row 10..11 全盖）
            (
                Vec3::new(-0.5, 0.14, 0.0),
                ColliderShape::Box {
                    half_extents: Vec3::new(3.5, 0.14, 1.0),
                },
            ),
        ];
        let grid = test_grid(&statics);
        let cells = grid.astar((2, 10), (17, 10)).expect("走廊内横穿必有路");
        assert!(
            cells
                .iter()
                .any(|&(c, r)| (grid.surface_height(c, r) - 0.28).abs() <= 1e-6),
            "无平路替代时必须爬 0.28 平台"
        );
        for pair in cells.windows(2) {
            let dh = (grid.surface_height(pair[1].0, pair[1].1)
                - grid.surface_height(pair[0].0, pair[0].1))
            .abs();
            assert!(dh <= 0.3 + 1e-6, "raw 逐边 |Δh| = {dh} 应 ≤ step_height");
        }
        assert!(
            grid.is_reachable(center(2, 10), center(17, 10)),
            "is_reachable 同语义"
        );
    }

    /// 3D 视线平滑（跨台阶不穿台）：台阶丘两侧地面的直线弦穿过丘体
    /// 中段时与表面差超 step_height → 无视线（直穿被挡）；string
    /// pulling 结果逐航点 DDA 复验畅通且必保留 ≥ 0.56 的中段折点
    /// （≤ step 的贴丘弦可合法合并顶格——角色可一步迈过；更大的穿台
    /// 弦一律被高度校验挡下）。
    #[test]
    fn smoothing_rechecks_3d_los_on_stair_worlds() {
        let grid = test_grid(&stair_mound());
        let from = center(2, 10);
        let to = center(11, 10);
        assert!(!grid.line_of_sight(from, to), "跨丘地面弦应被高度校验挡下");
        let path = grid.find_path(from, to).expect("必有路");
        assert!(path.len() >= 3, "平滑不得退化成跨台直线");
        let max_y = path.iter().map(|w| w.y).fold(f32::MIN, f32::max);
        assert!(
            (0.56 - 1e-6..=0.84 + 1e-6).contains(&max_y),
            "平滑应保留丘体中段折点，实际 max y = {max_y}"
        );
        for window in path.windows(2) {
            assert!(
                grid.line_of_sight(window[0], window[1]),
                "逐航点 DDA 复验：{:?} → {:?} 应畅通",
                window[0],
                window[1]
            );
        }
    }

    // ---- R49 高度场：混合地形确定性与表面解析（测试组 10）----

    /// 复杂地形（五级台阶丘 + 陡球 + 0.75 m 高台）两次 build +
    /// find_path / is_reachable：位图、高度场、航点逐位一致（高度场
    /// 的确定性由解析遍历序 + 雕刻静态门条件保证，见模块文档）。
    #[test]
    fn height_field_deterministic_on_mixed_terrain() {
        let mut statics = stair_mound();
        statics.push((
            Vec3::new(6.0, -1.4, 0.0),
            ColliderShape::Sphere { radius: 2.2 },
        ));
        statics.push((
            Vec3::new(6.0, 0.375, -6.5),
            ColliderShape::Box {
                half_extents: Vec3::new(0.5, 0.375, 0.5),
            },
        ));
        let build = || {
            NavGrid::build(
                NavGridParams {
                    bounds_min: Vec3::new(-10.0, 0.0, -10.0),
                    bounds_max: Vec3::new(10.0, 0.0, 10.0),
                    cell_size: 1.0,
                    agent_radius: 0.5,
                    step_height: 0.3,
                    max_slope_radians: std::f32::consts::FRAC_PI_4,
                },
                &statics,
            )
        };
        let (a, b) = (build(), build());
        assert_eq!(a.blocked, b.blocked, "两次 build 位图逐位一致");
        assert_eq!(a.heights, b.heights, "两次 build 高度场逐位一致");
        let queries = [
            (center(2, 10), center(11, 10)), // 横穿台阶丘
            (center(2, 10), center(17, 10)), // 横穿全场（陡球绕行）
            (center(8, 10), center(12, 3)),  // 北绕陡球 + 高台区
            (center(7, 14), center(13, 14)), // 贴高台行（y = 0.75 孤岛）
        ];
        for (from, to) in queries {
            let (pa, pb) = (a.find_path(from, to), b.find_path(from, to));
            assert_eq!(pa, pb, "两次 find_path({from:?} → {to:?}) 逐位一致");
            assert_eq!(
                a.is_reachable(from, to),
                b.is_reachable(from, to),
                "两次 is_reachable 一致"
            );
        }
    }

    /// 表面解析单测（对照解析解）：盒顶 / 球面 / 重叠取最高 / 坡度
    /// 过滤 / 胶囊上端帽——高度值与解析表达式逐位一致（同一浮点运算
    /// 序）、陡坡与孤岛 cell 的被阻语义正确。
    #[test]
    fn surface_resolution_box_sphere_overlap_and_slope() {
        // (a) 盒顶：覆盖格表面高 = pos.y + half_extents.y（恒可站立）
        let box_grid = test_grid(&[(
            Vec3::new(-4.5, 0.5, 0.0),
            ColliderShape::Box {
                half_extents: Vec3::new(0.5, 0.5, 9.5),
            },
        )]);
        assert_eq!(box_grid.surface_height(5, 10), 1.0, "盒顶高应 = 1.0");
        assert!(box_grid.is_blocked(5, 10), "1.0 m 孤岛顶应被阻");
        // (b) 球面：球心 (0, 1, 0) r = 2，cell (10,10)（ρ² = 0.5）表面
        // 高 = pos.y + √(r²−ρ²) = 1 + √3.5；坡度 ≤ 45°（ρ ≤ r·sin45）
        // 可站 → offered（孤岛被阻）；坡度 > 45° 的带（ρ ≈ 1.58）无表面
        let sphere_grid = test_grid(&[(
            Vec3::new(0.0, 1.0, 0.0),
            ColliderShape::Sphere { radius: 2.0 },
        )]);
        let sphere_h = 1.0_f32 + (4.0_f32 - 0.5).sqrt();
        assert!(
            (sphere_grid.surface_height(10, 10) - sphere_h).abs() <= 1e-6,
            "球面解析高应 = 1 + √3.5"
        );
        assert!(sphere_grid.is_blocked(10, 10), "高悬球顶孤岛应被阻");
        assert_eq!(
            sphere_grid.surface_height(11, 10),
            0.0,
            "坡度 > max_slope 处无表面（高度 0）"
        );
        assert!(sphere_grid.is_blocked(11, 10), "陡坡带应被阻");
        // (c) 重叠取最高：两盒同 footprint 叠放（顶 0.5 与 1.0）→ 表面
        // 高取 1.0（最高顶面胜出；0.5 m 盒子的侧挡 / 覆盖不参与）
        let stack_grid = test_grid(&[
            (
                Vec3::new(-6.5, 0.25, 0.0),
                ColliderShape::Box {
                    half_extents: Vec3::new(0.5, 0.25, 9.5),
                },
            ),
            (
                Vec3::new(-6.5, 0.75, 0.0),
                ColliderShape::Box {
                    half_extents: Vec3::new(0.5, 0.25, 9.5),
                },
            ),
        ]);
        assert_eq!(stack_grid.surface_height(3, 10), 1.0, "重叠应取最高表面");
        assert_eq!(stack_grid.surface_height(4, 10), 0.0, "盒外地面高 0");
        assert!(stack_grid.is_blocked(3, 10), "1.0 m 叠顶孤岛应被阻");
        // (d) 胶囊上端帽：帽心 = pos.y + (half_height − radius)，表面高 =
        // 帽心 y + √(r²−ρ²)；坡度 ≤ 45° 的中央帽区可站（细网格采样）
        let mut fine = NavGridParams::new(Vec3::new(-4.0, 0.0, -4.0), Vec3::new(4.0, 0.0, 4.0));
        fine.cell_size = 0.25;
        fine.agent_radius = 0.5;
        let capsule_grid = NavGrid::build(
            fine,
            &[(
                Vec3::new(0.0, 1.5, 0.0),
                ColliderShape::Capsule {
                    radius: 1.0,
                    half_height: 3.0,
                },
            )],
        );
        // cell (16,16) 中心 (0.125, 0.125)：ρ² = 0.03125 ≤ 1
        let cap_h = 3.5_f32 + (1.0_f32 - 0.03125).sqrt();
        assert!(
            (capsule_grid.surface_height(16, 16) - cap_h).abs() <= 1e-6,
            "胶囊帽面解析高应 = 3.5 + √0.96875"
        );
        assert!(
            capsule_grid.is_blocked(16, 16),
            "胶囊（立柱）顶帽孤岛应被阻"
        );
    }
}
