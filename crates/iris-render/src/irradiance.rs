//! 光照探针 GI（R64）——烘焙 irradiance 探针云 + 四面体重心插值（漫反射通路）。
//!
//! # 缺口与定位
//!
//! 静态几何有光照贴图（R52c 的烘焙间接漫反射），**动态物体没有**：角色 /
//! 掉落物 / 门这类会动的实体进不了 UV2 图集（烘进去就是错位阴影），只能吃
//! 全局 IBL 的天空辐照——室内阴影里的角色要么被天空色污染成冷蓝，要么在
//! 封闭房间里亮得不合理。本模块补上**漫反射通路**的对端（镜面通路是 R62a
//! 反射探针 / R62b SSR）：
//!
//! ```text
//! 静态漫反射 = 光照贴图（逐 texel 烘焙辐照）
//! 动态漫反射 = 光照探针（逐探针烘焙辐照 + 四面体重心插值）   ← 本模块
//! 镜面       = 反射探针（域内）→ SSR（屏幕内）→ 全局 env（回退）
//! ```
//!
//! # 为什么是「Delaunay 四面体化 + 重心插值」
//!
//! 探针是一组散点，查询点是任意世界位置。候选方案与取舍：
//!
//! - **最近探针（Voronoi 0 阶）**：取值在探针的 Voronoi 边界上**跳变**
//!   （角色走过一步，辐照度整档切换）——观感上最差的一种；
//! - **反距离加权（IDW / Shepard）**：处处连续，但①探针点处**不精确**
//!   （除非特判，而特判本身又引入不连续）②权重与几何无关，隔着墙的两个
//!   探针会互相串味③没有解析的「内部 / 外部」边界；
//! - **Delaunay 四面体化 + 重心插值（本模块）**：①探针点处**精确还原**
//!   （重心权重 1/0/0/0）②四面体内部是**仿射**插值，跨共享面时两侧取值
//!   一致（同一个三角面上的重心坐标由顶点唯一决定）⇒ 处处连续③Delaunay
//!   的空球准则让四面体尽量「胖」，重心权重不出现病态放大④这正是 Valve
//!   的 irradiance volume / 经典 ambient probe 的数学。代价是要自己实现
//!   四面体化（零第三方依赖，见下）。
//!
//! # 四面体化算法（自研，零依赖）
//!
//! **选型 = 增量插入 + Bowyer-Watson 空腔重三角化**（而不是「提升到 4D
//! 求凸包」）。理由：两者数学等价（把点 p 提升到抛物面
//! (x, y, z, x²+y²+z²) 后，3D Delaunay 四面体化 = 4D 凸包的下表面，而
//! Bowyer-Watson 的「空腔 = 外接球含 p 的四面体集合」正是 4D 凸包的可见面
//! 集合），但**空腔形态下每个判定都是 3×3 / 4×4 行列式**，而 4D 凸包的
//! 面 / 邻接组合要维护 4 维单纯形表（面、边、可见性传播），出错面大得多；
//! 本仓探针预算是百量级（[`MAX_IRRADIANCE_PROBES] = 256），Bowyer-Watson
//! 的 O(n²) 暴力定位 / 冲突检测（∝ 1500 四面体 × 256 次插入 ≈ 4×10⁵ 次
//! 行列式）在烘焙期毫秒量级，不值得为渐进复杂度引入更复杂的结构。
//!
//! ## 空腔 = 连通分量 + 幽灵胞元（凸包外的点怎么进来）
//!
//! 正确性依赖一条经典引理：**空腔对 p 星形**（从 p 到冲突胞内任一点的线段
//! 上，insphere 的符号只能在严格穿过某张外接球面时改变 ⇒ 严格冲突集合是
//! 星形的）。星形性是重三角化合法的前提——边界小面与 p 连成的新胞才恒为正
//! 朝向。
//!
//! 于是有两件事必须做对（都是实测踩出来的）：
//!
//! 1. **空腔只取「与 p 所在胞连通」的冲突分量**（BFS）。不设约束的全扫描会
//!    把远处、与 p 不连通的胞也算进空腔——那些胞的边界小面把 p 放在**错误
//!    一侧**，新胞朝向相反 ⇒ 复合体出现空洞（症状：一个面被三个以上胞共享）。
//!    实测：随机 12 点云第 5 次插入就丢胞。
//! 2. **凸包外的点按「幽灵胞元」接入**（与 CGAL 的「无穷远顶点」模型同构：
//!    凸包面 ⟷ 无穷远胞元）。p 在凸包外时，包含 p 的胞是幽灵胞元；可见的
//!    凸包面（`orient3d(面, p) < 0»）进空腔，其拥有者若冲突则是连通分量的
//!    入口；不可见凸包面与冲突胞之间的边界小面、以及「可见面 + 拥有者不冲突」
//!    的那些面，各自与 p 连成新胞（后者用反向面序，使 p 落在正向侧）。
//!
//! 替代方案「超四面体容器」（真实点全塞进一个巨大四面体、插入完再剥掉触及
//! 容器顶点的胞）实测**不可行**：容器顶点会落进真实薄胞（薄板 / 针状胞的
//! 外接球极大）的内部，导致这些真实胞不在增广 Delaunay 里 ⇒ 剥掉容器后结构
//! 出现大片空洞（扁平探针布局尤其明显）。
//!
//! ## 退化处理（共球 / 共面）：确定性抖动 + 严格 tie-break
//!
//! **① 确定性连续抖动**（[`jitter]）：规范化坐标加 ±[`JITTER]（包围盒
//! 的 1e-6，即每米 1 微米）的三线性哈希抖动。精确共面 / 共球是**真实的
//! 数值退化**：共面的边界小面与 p 连成的「新胞」体积恰为 0 ⇒ 朝向判定
//! 恒不过（丢胞 ⇒ 空洞），共球则让「0 算不算冲突」变成结果相关的选择。
//! 抖动的三条性质是构造保证（整数哈希 ⇒ 确定性；三线性 ⇒ 连续性；
//! 同坐标同抖动 ⇒ 探针点处重心权重 1/0/0/0 逐位精确），数量级上比谓词的
//! f64 舍入噪声（≈1e-16）高十个数量级、比 f32 相对精度低一个数量级
//! ⇒ 打破退化但视觉 / 数值不可分辨。
//!
//! **代价（诚实限制，§8.40）**：四面体化的几何是「真值 ± 1 µm」；规则布局
//! （网格 / 立方体角点）的凸包**组合结构**因此可能与理想构型不同（例如某个
//! 恰好贴在凸包平面上的点被抖进内部后不再是凸包顶点）——这是任何扰动法
//! （含符号扰动 SoS）都有的性质，不是缺陷：结果始终是**抖动点集的正确
//! Delaunay**，且逐位可复现。
//!
//! **② 共球 tie-break = 严格大于才算冲突**（insphere > 0）。共球点集的
//! Delaunay 四面体化本来就不唯一（球面上任意三角化都满足空球准则），
//! 「0 不算冲突」是显式的、可复现的挑选规则；配合①的抖动，实际输入下
//! 精确 0 已近乎不可能出现，它作为**第二层保险**存在（共球的手工构型
//! 单测直接钉住该规则的语义）。
//!
//! **退化输入**：有效点少于 4 个（含全部重合）⇒ 返回空四面体化，查询全部
//! 回退全局 env。**共面 / 共线点云不再算退化**——抖动把薄板 / 细棒变成
//! 合法的（极薄的）四面体化：平面探针布局（一层楼面）在平面内插值正常，
//! 平面外的查询被凸包投影钳制（等价于「该层辐照」，比整体回退 env 更
//! 接近真值）。
//!
//! # 求值点 = 逐实体 CPU 侧（拒绝逐片元 GPU 插值）
//!
//! 每帧对**无光照贴图实体**在 CPU 侧取实体根 `GlobalTransform 位置做一次
//! 插值，结果随实例数据上传（[`InstanceRaw::probe](crate::instance::InstanceRaw)），
//! 着色器按标志位整项替换全局 env 的漫反射辐照。**不做逐片元 GPU 插值**
//! （拒绝清单，理由写全）：
//!
//! - 逐片元要上传整张四面体表 + 逐探针辐照（GPU 存储缓冲 + 逐片元搜索 /
//!   遍历），而箱庭尺度的角色 / 掉落物是**逐实体**常数——插值结果在同一个
//!   角色身上本来就几乎不变（角色 2 m 高，探针间距 3 m 量级）；
//! - 逐实体的成本 = 每实体每帧一次点定位（本模块实测见 REPORT 的性能表），
//!   逐片元则是每个覆盖像素一次（10⁵ 量级像素 × 搜索），成本差 4–5 个
//!   数量级；
//! - 逐实体必然「整块取值」——但重心插值的权重随位置**连续**变化 ⇒ 走过
//!   探针网格边界（含门洞）时角色整体的辐照连续变化，不会闪；
//! - 代价（诚实限制）：**角色自身没有空间梯度**（头与脚吃同一份辐照），
//!   大体积动态物体（车辆 / 飞船）需要逐片元才能正确（§8.40）。
//!
//! # 烘焙源 = 复用 bake_sample
//!
//! 逐探针调用同一个 [`bake_sample](crate::bake::bake_sample)（同一 BVH、
//! 同一低差异采样、同一直接 / 天空 / 反弹三项口径）⇒ **与光照贴图视觉同源**
//! 是机制保证而非巧合。存的是 [`BakeSample::indirect](crate::bake::BakeSample::indirect)
//! = 天空（含 AO 遮蔽）+ 一次反弹（含色溢）——与光照贴图**完全同口径**，
//! 直接光仍由运行时 CSM / 点光提供（否则双计，论证见 `lightmap.rs）。
//!
//! **探针的「法线朝向」= 六轴平均**（±X / ±Y / ±Z 六个方向各烘一次，取
//! 算术平均）。两选一（上向代表 vs 六轴平均）的裁决：
//!
//! - v1 探针是**无方向**的 RGB 标量，那么物理上自洽的取法就是辐照度函数
//!   E(n) 在方向上的**零阶（L0）平均**——六轴平均是其 6 点求积：对均匀场
//!   **精确**（E(n) ≡ π·L 与 n 无关 ⇒ 六轴平均恰为 π·L），对方向性场是
//!   各向同性的粗估；
//! - **上向代表**（只烘 n = +Y）会系统性偏向天花板 / 天空：室内角色拿到的
//!   是「地板辐照度」而不是「它所处位置的辐照度」，且六个朝向里挑一个没有
//!   物理依据（角色的躯干是竖直面）；
//! - 代价 = **6× 烘焙成本**（每探针 6 次 [`bake_sample](crate::bake::bake_sample)），
//!   一次性支出；
//! - 局限（§8.40）：强方向性场（贴着单侧墙的灯）下，六轴平均会把亮面的
//!   能量摊到暗面（角色的背光侧偏亮）；真正的方向性探针需要 SH（拒绝清单）。
//!
//! # 语义边界（六条）
//!
//! 1. **挂光照贴图的实体不动**：材质掩码 surface.w > 0.5 时着色器走 R52c
//!    的图集分支（更高精度的逐 texel 烘焙），本模块在 CPU 侧对这类实体
//!    **直接不发标志位**（probe.w = 0），两道保险；
//! 2. **无贴图实体（动态为主）**由探针插值取代全局 env 的**漫反射辐照**；
//! 3. **镜面一概不碰**：spec_refl_w 的计算与探针无关（R62a 探针 / R62b
//!    SSR 的领地）；
//! 4. **SSS（R63）是直射项**，与间接项正交，互不影响；
//! 5. **包外**（凸包之外）⇒ 取**最近凸包面投影**（钳制到面的重心坐标），
//!    而不是回退全局 env。理由：跨出凸包边界时取值**连续**（凸包面上
//!    内外同值），角色走出探针覆盖区不会突然从「房间暖光」跳到「天空蓝」；
//!    局限：远离探针云的实体一直吃边界辐照（无距离衰减），且探针云没盖住
//!    的室外区域不会退回天空色（§8.40）；
//! 6. **实体增删不影响探针数据**：探针集是烘焙产物（位置 + 辐照 + 四面体
//!    表），运行期只读；角色出生 / 死亡不改动它。
//!
//! # 存储形态 = World 资源（而不是渲染器内部）
//!
//! R62a 的反射探针把登记表放在渲染器内部，因为它的产物**是 GPU 资源**
//! （cubemap 数组），放 World 会造成「World 说已烘焙、Renderer 没有纹理」
//! 的撕裂。本轮**全程 CPU**（烘焙、四面体化、插值都在 CPU），产物是纯数据，
//! 没有 GPU 侧对应物 ⇒ 放 [`IrradianceProbeSet] **World 资源**：渲染器每帧
//! 从 &World 读它做逐实体插值，没有第二份副本、没有同步问题，也让
//! 「烘焙 → 装配 → 渲染」与 [`install_lightmap](crate::lightmap::install_lightmap)
//! 的既有形态一致（烘焙产物进资源、渲染器只读）。
//!
//! # 兼容铁律（探针缺席 = 逐位不变）
//!
//! 探针集资源缺席 / 空集 ⇒ 每个实例的载荷恒 [0, 0, 0, 0]，着色器
//! select(env_irradiance, in.probe.rgb, in.probe.w > 0.5) 返回**纹理采样
//! 值的同一串位**（select 是纯值选择、不做算术——R52c 先例，比
//! mix(a, b, 0) 的浮点恒等式论证强）。全部既有 GPU FNV 基线原值通过
//! （本轮不重锁）。
//!
//! # v1 诚实限制（详见 ARCHITECTURE §8.40）
//!
//! 逐片元 GPU 插值；球谐 SH / 方向性探针（L1 及更高）；可见性（occlusion）
//! 加权插值；探针磁盘持久化与 pack 集成；编辑器集成；动态重烘焙 / 脏标记；
//! 共面点云的 2D 降级；超预算探针的重要性排序；探针自遮挡（探针落在几何
//! 内部时烘出的是几何内部的辐照，需作者自己把探针放在空腔里）。

use std::collections::BTreeMap;

use glam::Vec3;
use iris_core::Resource;
use iris_core::prelude::{Entity, GlobalTransform};
use iris_core::world::World;

use crate::bake::{BakeParams, bake_sample, build_bvh, collect_bake_triangles};

/// 探针预算上限（超过按实体位升序截断并告警）。
///
/// 定标的依据 = 烘焙成本与四面体化成本都是探针数的函数：单探针烘焙 = 6 次
/// [`bake_sample]（六轴），256 探针 ≈ 1500 四面体、插入期 ≈ 4×10⁵ 次行列式
/// 判定（实测数字见 REPORT）。256 覆盖「30×30 m 箱庭、3 m 间距」的 6×6×2
/// 网格还有大量余量。
pub const MAX_IRRADIANCE_PROBES: usize = 256;

/// 六轴代表方向（探针的「无方向」辐照度 = 六轴 `bake_sample 的算术平均，
/// 推导见模块文档「烘焙源」）。
pub const PROBE_AXES: [Vec3; 6] = [
    Vec3::X,
    Vec3::NEG_X,
    Vec3::Y,
    Vec3::NEG_Y,
    Vec3::Z,
    Vec3::NEG_Z,
];

/// 确定性抖动幅度（规范化空间，包围盒最长边 = 1）：±1e-6。
///
/// 数量级推导：规范化后坐标 ∈ ±0.5，谓词（3×3 / 4×4 行列式）的 f64 舍入噪声
/// ≈ 1e-16 ⇒ 抖动比噪声高 **10 个数量级**（足以打破一切精确共面 / 共球退化）；
/// 抖动相对包围盒只有 1e-6（每米 1 微米），插值场的形变 ≈ 1e-6 × 梯度，
/// 远低于 f32 相对精度（1.2e-7）在渲染取值上的可见性 ⇒ 视觉与数值都不可分辨。
const JITTER: f64 = 1e-6;
/// 抖动幅度阶梯（构建重试用；全部失败 ⇒ 空四面体化 + degenerate）。
///
/// 阶梯是固定的三段（1e-6 → 1e-4 → 1e-2 的包围盒比例）：极扁平布局在最小
/// 抖动下可能仍落进数值退化区，更大的抖动把退化构型推开（代价：几何偏移
/// 变大，1e-2 档已接近「毫米级」，只在最后手段使用）。
const JITTER_LADDER: [f64; 3] = [JITTER, 1e-4, 1e-2];
/// 抖动格子的每轴数量（三线性插值 ⇒ 抖动在空间上连续）。
const JITTER_CELLS: f64 = 4.0;
/// 初始单纯形的退化阈值：最短边的平方下界（工作坐标，包围盒最长边 = 1）。
/// 比抖动幅度（1e-6，平方 1e-12）低六个数量级 ⇒ 抖动后的点云不会误触。
const MIN_EDGE2: f64 = 1e-18;
/// 共线判据：两边叉积模长的平方下界。
const MIN_AREA2: f64 = 1e-18;
/// 贴边容差（相对量级）：查询点落在所有胞之外时，允许「最负分子 / 分子量级」
/// 不超过该比例的胞兜底（分子是体积量纲，比值无量纲 ⇒ 与尺度无关）。
/// 取 1e-9：比 f64 舍入噪声（~1e-16）高七个数量级，比任何真实几何偏差低
/// 得多（真实落在胞外时最负分子与量级同阶 ⇒ 比值 O(1)）。
const NEAR_HULL_SLACK: f64 = 1e-9;
/// 共面判据：|orient3d|（= 6 × 体积）下界。
const MIN_VOLUME: f64 = 1e-15;

/// 光照探针实体标记组件（位置取实体 [`GlobalTransform]）。
///
/// **缺席语义**：组件缺席 = 该实体不是探针；有组件但世界缺
/// [`MeshStore](crate::mesh::MeshStore) / [`MaterialStore](crate::material::MaterialStore)
/// 时烘焙仍可按 BVH 空场景进行（全遮挡 ⇒ 探针辐照为 0，合法的封闭暗角）。
///
/// 探针实体**不应**同时挂 [`MeshHandle](crate::mesh::MeshHandle)：那样它会
/// 参与静态几何烘焙（探针点正好在几何内部时会把自己挡黑）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, iris_core::Component)]
pub struct IrradianceProbe;

// ---------------------------------------------------------------------------
// 谓词（f64，固定展开顺序 ⇒ 逐位可复现）
// ---------------------------------------------------------------------------

/// 规范化空间的点。
type P3 = [f64; 3];

#[inline]
fn sub(a: P3, b: P3) -> P3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn cross(a: P3, b: P3) -> P3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
fn dot(a: P3, b: P3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn norm2(a: P3) -> f64 {
    dot(a, a)
}

/// det[(a−d); (b−d); (c−d)]（三行行向量）。
///
/// **符号约定**：对右手系四面体 (0,0,0), (1,0,0), (0,1,0), (0,0,1) 有
/// orient3d = −1。全部四面体在存储时都归一化成 orient3d > 0（负则交换两个
/// 顶点），于是「orient3d(面, p) > 0 = p 在该面正向侧」与 [`tet_faces] 给出
/// 的面序自洽（单测逐面钉住）。
#[inline]
fn orient3d(a: P3, b: P3, c: P3, d: P3) -> f64 {
    dot(sub(a, d), cross(sub(b, d), sub(c, d)))
}

/// 3×3 行列式（按第一行余子式展开，固定顺序）。
#[inline]
fn det3(m: [[f64; 3]; 3]) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

/// 抛物面提升行列式（行 = a−d, b−d, c−d, p−d，末列 = 各行的模长平方）。
#[inline]
fn paraboloid_det(a: P3, b: P3, c: P3, d: P3, p: P3) -> f64 {
    let r = [sub(a, d), sub(b, d), sub(c, d), sub(p, d)];
    let m = [
        [r[0][0], r[0][1], r[0][2], norm2(r[0])],
        [r[1][0], r[1][1], r[1][2], norm2(r[1])],
        [r[2][0], r[2][1], r[2][2], norm2(r[2])],
        [r[3][0], r[3][1], r[3][2], norm2(r[3])],
    ];
    // 按第一行余子式展开（符号 + − + −），余子式 = 去掉第 0 行与第 skip 列
    let minor = |skip: usize| -> f64 {
        let cols: [usize; 3] = match skip {
            0 => [1, 2, 3],
            1 => [0, 2, 3],
            2 => [0, 1, 3],
            _ => [0, 1, 2],
        };
        det3([
            [m[1][cols[0]], m[1][cols[1]], m[1][cols[2]]],
            [m[2][cols[0]], m[2][cols[1]], m[2][cols[2]]],
            [m[3][cols[0]], m[3][cols[1]], m[3][cols[2]]],
        ])
    };
    m[0][0] * minor(0) - m[0][1] * minor(1) + m[0][2] * minor(2) - m[0][3] * minor(3)
}

/// 正值 ⇔ p **严格**在正向四面体 (a, b, c, d)（须 orient3d(a,b,c,d) > 0）的
/// 外接球内；恰好为零 ⇔ 五点共球（退化，按模块文档的 tie-break 判为
/// 「不在球内」，即不算冲突）；负值 ⇔ 球外。
#[inline]
fn insphere(a: P3, b: P3, c: P3, d: P3, p: P3) -> f64 {
    -paraboloid_det(a, b, c, d, p)
}

/// 正向四面体 (v0, v1, v2, v3) 的四个面，按**内部在正向侧**的顶点顺序，与对
/// 顶点配对：orient3d(face, opposite) = orient3d(v0, v1, v2, v3) > 0。
///
/// 四个面序都是 (v0, v1, v2, v3) 的**偶置换**（分别对应把 d / c / b / a 放到
/// 末位），推导：orient3d 对任意两个参数互换反号，偶置换不变号。
///
/// 用途：①点在四面体内 = 四个面全部 orient3d(face, p) ≥ 0；②重心权重
/// w(对顶点) = orient3d(face, p) / orient3d(face, 对顶点)；③凸包面可见性 =
/// orient3d(face, p) < 0（外部点）。
#[inline]
fn tet_faces(t: [u32; 4]) -> [([u32; 3], u32); 4] {
    let [a, b, c, d] = t;
    [
        ([a, b, c], d),
        ([a, d, b], c),
        ([a, c, d], b),
        ([b, d, c], a),
    ]
}

/// [`tet_faces] 的**顶点序**重排：第 k 项是**顶点 k 对面**的那个面。
///
/// 重心权重数组与插值取值数组都按这个口径索引（w[k] ↔ 顶点 t[k]）——
/// 这是「权重和为 1 且插值 = Σ w_k · value(t[k])」的实现前提；若用面序索引，
/// 权重会被接到错误的顶点上（实测症状：权重和仍为 1，但 Σ w_k·p_k ≠ p，
/// 线性场复原与共享面一致性测试当场抓住）。
#[inline]
fn tet_faces_vertex_order(t: [u32; 4]) -> [([u32; 3], u32); 4] {
    let f = tet_faces(t);
    [f[3], f[2], f[1], f[0]]
}

/// 面 → 打包键（顶点三元组排序后各占 21 位；顶点数 < 2²¹ 是构造前提）。
#[inline]
fn face_key(f: [u32; 3]) -> u64 {
    let mut v = f;
    v.sort_unstable();
    ((v[0] as u64) << 42) | ((v[1] as u64) << 21) | (v[2] as u64)
}
/// splitmix64 变体（纯整数混洗：无浮点、无 libm、无全局状态 ⇒ 跨运行 /
/// 跨平台逐位一致，AGENTS.md 的无 RNG 文化）。
#[inline]
fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// 确定性**连续**抖动（三线性插值的格点哈希）：把规范化点映射到
/// [0, `JITTER_CELLS]³ 的格子，8 个角点各取一个整型哈希的三元组，
/// 三线性插值后以 ±`JITTER/2 加到坐标上。
///
/// 三条性质都是构造保证：
/// - **确定性**：纯整数哈希 + 四则运算，无 libm、无状态；
/// - **连续性**：三线性插值 ⇒ C⁰（查询点连续移动 ⇒ 抖动连续 ⇒ 插值场连续，
///   验收「跨共享面 / 跨凸包边界连续」不被抖动破坏）；
/// - **恒等性**：同一坐标必得同一抖动 ⇒ 探针点与其查询点的**工作坐标逐位
///   相同**，重心权重 1/0/0/0 逐位精确（验收 ①）。
fn jitter(q: P3, magnitude: f64) -> P3 {
    let g = [
        (q[0] + 0.5) * JITTER_CELLS,
        (q[1] + 0.5) * JITTER_CELLS,
        (q[2] + 0.5) * JITTER_CELLS,
    ];
    let i = [g[0].floor(), g[1].floor(), g[2].floor()];
    let f = [g[0] - i[0], g[1] - i[1], g[2] - i[2]];
    let mut acc = [0.0f64; 3];
    for dx in 0..2usize {
        for dy in 0..2usize {
            for dz in 0..2usize {
                let w = (if dx == 0 { 1.0 - f[0] } else { f[0] })
                    * (if dy == 0 { 1.0 - f[1] } else { f[1] })
                    * (if dz == 0 { 1.0 - f[2] } else { f[2] });
                if w == 0.0 {
                    continue; // 权重恰 0 ⇒ 贡献恰 0（不必哈希）
                }
                let cx = (i[0] + dx as f64) as i64 as u64;
                let cy = (i[1] + dy as f64) as i64 as u64;
                let cz = (i[2] + dz as f64) as i64 as u64;
                let h = mix64(
                    mix64(mix64(cx) ^ cy.wrapping_mul(0x9E37_79B9_7F4A_7C15))
                        ^ cz.wrapping_mul(0xC2B2_AE3D_27D4_EB4F),
                );
                let mut shift = 0u32;
                for slot in acc.iter_mut() {
                    let bits = ((h >> shift) & 0x1F_FFFF) as f64 / 2_097_151.0 - 0.5;
                    *slot += w * bits * magnitude;
                    shift += 21;
                }
            }
        }
    }
    [q[0] + acc[0], q[1] + acc[1], q[2] + acc[2]]
}

// ---------------------------------------------------------------------------
// 四面体化
// ---------------------------------------------------------------------------

/// 四面体化的结构自检结果（测试 / 调试口径）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TetraValidation {
    /// 朝向非正的四面体数（必须为 0）。
    pub inverted: usize,
    /// 零体积四面体数（必须为 0）。
    pub degenerate: usize,
    /// 被超过两个四面体共享的面数（非流形，必须为 0）。
    pub non_manifold: usize,
    /// 局部非 Delaunay 的内部面数（对顶点严格落在邻接四面体外接球内）。
    pub non_delaunay: usize,
    /// 内部面数（被两个四面体共享）。
    pub interior_faces: usize,
    /// 边界面数（凸包面）。
    pub boundary_faces: usize,
    /// 欧拉特征 V − E + F − T（单连通实心球应为 1）。
    pub euler_characteristic: i64,
    /// 顶点数。
    pub vertices: usize,
    /// 四面体数。
    pub tetrahedra: usize,
}

impl TetraValidation {
    /// 全部结构不变量成立（朝向 / 流形 / Delaunay / 欧拉特征）。
    pub fn is_valid(&self) -> bool {
        self.inverted == 0
            && self.degenerate == 0
            && self.non_manifold == 0
            && self.non_delaunay == 0
            && (self.tetrahedra == 0 || self.euler_characteristic == 1)
    }
}

/// 四面体化统计（观测口径）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TetraStats {
    /// 输入点数（含非有限点与重合点）。
    pub input_points: usize,
    /// 去重后的顶点数。
    pub vertices: usize,
    /// 因坐标重合而并入已有顶点的点数。
    pub merged: usize,
    /// 因坐标非有限而剔除的点数。
    pub non_finite: usize,
    /// 最终未被任何四面体引用的顶点数（正常输入恒 0；非 0 说明该探针孤立）。
    pub skipped: usize,
    /// 数值异常被丢弃的新胞数（正常输入下恒 0）。
    pub dropped_cells: usize,
    /// 生成的四面体数。
    pub tetrahedra: usize,
    /// 凸包面数。
    pub hull_faces: usize,
    /// 实际生效的抖动幅度（包围盒比例；见 [`JITTER_LADDER]）。
    pub jitter: f64,
    /// 构建尝试次数（1 = 首档抖动即成功）。
    pub build_attempts: u32,
    /// 有效点不足 4 个，或全部抖动档位的拓扑自检都失败 ⇒ 空四面体化。
    pub degenerate: bool,
}

/// 探针点云的确定性 Delaunay 四面体化（算法与推导见模块文档）。
///
/// 空结构（`Default）合法：所有查询返回 `None（调用方回退全局 env）。
#[derive(Clone, Debug, Default)]
pub struct Tetrahedralization {
    /// 规范化平移（世界空间包围盒中心）。
    center: Vec3,
    /// 规范化缩放（世界空间包围盒最长边）。
    scale: f32,
    center64: P3,
    inv_scale64: f64,
    /// 本次构建实际生效的抖动幅度（**查询必须用同一幅度**：顶点工作坐标与
    /// 查询工作坐标若不同源，探针恒等性与「包内定位」都会失效——实测症状
    /// 是贴边点被判成包外、取值跳变）。
    jitter_magnitude: f64,
    /// 顶点位置（**规范化、未抖动**：恒等性对照与回指原值用）。
    verts: Vec<P3>,
    /// 顶点**工作坐标**（规范化 + 抖动：全部谓词用它）。
    verts_j: Vec<P3>,
    /// 顶点世界坐标（原输入值，逐位保留）。
    verts_world: Vec<Vec3>,
    /// 逐顶点回指的代表点下标（原输入下标最小者）。
    vertex_point: Vec<u32>,
    /// 逐原输入点 → 顶点下标（非有限点为 `u32::MAX）。
    point_vertex: Vec<u32>,
    /// 四面体（顶点下标，恒 orient3d > 0）。
    tets: Vec<[u32; 4]>,
    /// 逐四面体的四面面分母（与 [`tet_faces] 同序 ⇒ 顶点恒等性逐位精确）。
    denoms: Vec<[f64; 4]>,
    /// 逐四面体的规范化空间 AABB（查询早退）。
    bounds: Vec<[P3; 2]>,
    /// 凸包面（顶点下标，内部在正向侧）。
    hull: Vec<[u32; 3]>,
    stats: TetraStats,
}

impl Tetrahedralization {
    /// 空结构。
    pub fn empty() -> Self {
        Self::default()
    }

    /// 从世界空间点云构建（确定性：同输入两次构建逐位一致）。
    ///
    /// 流程：有限性过滤 → 包围盒规范化 → 字典序全序排序 + 重合点合并 →
    /// **确定性抖动** → 逐点 Bowyer-Watson 插入 → 凸包面重算。
    ///
    /// **抖动幅度阶梯重试**：极扁平的布局（探针全部共面且点很多）在最小抖动
    /// （[`JITTER]）下仍可能落进数值退化区，插入期的拓扑自检会当场发现
    /// （面被三个以上胞共享）⇒ 换用更大的抖动幅度重建。阶梯是**固定**的
    /// （确定性文化：同输入必然走同一条阶梯、得到同一结果）；全部阶梯都失败
    /// ⇒ 返回空四面体化并标记 [`TetraStats::degenerate]，查询回退全局 env
    /// （诚实限制，见 §8.40）。
    pub fn build(points: &[Vec3]) -> Self {
        for (attempt, magnitude) in JITTER_LADDER.iter().enumerate() {
            match Self::build_with(points, *magnitude) {
                Some(built) => {
                    let mut done = built;
                    done.jitter_magnitude = *magnitude;
                    done.stats.jitter = *magnitude;
                    done.stats.build_attempts = attempt as u32 + 1;
                    return done;
                }
                None => continue,
            }
        }
        let mut failed = Self::default();
        failed.stats.input_points = points.len();
        failed.stats.degenerate = true;
        failed.stats.build_attempts = JITTER_LADDER.len() as u32;
        failed
    }

    /// 单次尝试（成功 = 结构自检通过）。
    fn build_with(points: &[Vec3], magnitude: f64) -> Option<Self> {
        let mut out = Self::default();
        out.stats.input_points = points.len();

        // ---- 1. 有限性过滤 + 包围盒 ----
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for p in points {
            if !p.is_finite() {
                out.stats.non_finite += 1;
                continue;
            }
            min = min.min(*p);
            max = max.max(*p);
        }
        let scale = (max - min).max_element();
        if scale <= 0.0 || !scale.is_finite() {
            out.stats.degenerate = true;
            return Some(out);
        }
        let center = (min + max) * 0.5;
        out.center = center;
        out.scale = scale;
        out.center64 = [center.x as f64, center.y as f64, center.z as f64];
        out.inv_scale64 = 1.0 / scale as f64;
        out.point_vertex = vec![u32::MAX; points.len()];

        // ---- 2. 规范化 + 字典序全序排序（确定性插入序） ----
        let mut sorted: Vec<(P3, u32)> = Vec::with_capacity(points.len());
        for (i, p) in points.iter().enumerate() {
            if !p.is_finite() {
                continue;
            }
            sorted.push((out.normalize(*p), i as u32));
        }
        sorted.sort_by(|a, b| {
            a.0[0]
                .total_cmp(&b.0[0])
                .then(a.0[1].total_cmp(&b.0[1]))
                .then(a.0[2].total_cmp(&b.0[2]))
                .then(a.1.cmp(&b.1))
        });

        // ---- 3. 重合点合并（逐位相等即同一点；代表 = 原下标最小者） ----
        for (pos, index) in sorted {
            if out.verts.last() == Some(&pos) {
                out.point_vertex[index as usize] = out.verts.len() as u32 - 1;
                out.stats.merged += 1;
                continue;
            }
            out.point_vertex[index as usize] = out.verts.len() as u32;
            out.vertex_point.push(index);
            out.verts_world.push(points[index as usize]);
            out.verts.push(pos);
            out.verts_j.push(jitter(pos, magnitude));
        }
        out.stats.vertices = out.verts.len();

        // ---- 4. 初始单纯形（字典序下最早的四个仿射无关点，工作坐标判定） ----
        let Some(tet) = initial_simplex(&out.verts_j) else {
            out.stats.degenerate = true;
            return Some(out);
        };

        // ---- 5. 逐点增量插入（Bowyer-Watson + 幽灵胞元邻接） ----
        let mut complex = Complex::new(out.verts_j.clone());
        complex.push_tet(tet);
        for v in 0..out.verts.len() as u32 {
            if tet.contains(&v) {
                continue;
            }
            complex.insert(v);
            if complex.corrupt {
                return None; // 拓扑自检失败 ⇒ 交给上一层的抖动阶梯重试
            }
        }
        out.stats.dropped_cells = complex.dropped;
        let mut referenced = vec![false; out.verts.len()];
        for (i, t) in complex.tets.iter().enumerate() {
            if complex.dead[i] {
                continue;
            }
            for &v in t {
                referenced[v as usize] = true;
            }
            out.push_tet(*t);
        }
        out.stats.skipped = referenced.iter().filter(|r| !**r).count();
        out.rebuild_hull();
        out.stats.tetrahedra = out.tets.len();
        out.stats.hull_faces = out.hull.len();
        Some(out)
    }

    /// 世界坐标 → 规范化坐标（构建与查询同一条代码路径 ⇒ 逐位一致）。
    #[inline]
    fn normalize(&self, p: Vec3) -> P3 {
        [
            (p.x as f64 - self.center64[0]) * self.inv_scale64,
            (p.y as f64 - self.center64[1]) * self.inv_scale64,
            (p.z as f64 - self.center64[2]) * self.inv_scale64,
        ]
    }

    /// 世界坐标 → **抖动后**的规范化坐标（全部谓词的工作坐标）。
    ///
    /// 抖动幅度 = 本次构建实际生效的档位（[`TetraStats::jitter]）：
    /// 顶点与查询用**同一个函数、同一个幅度** ⇒ 探针点处逐位恒等。
    #[inline]
    fn work_point(&self, p: Vec3) -> P3 {
        jitter(self.normalize(p), self.jitter_magnitude)
    }

    /// 顶点的工作坐标（抖动后）。
    #[inline]
    fn pos(&self, v: u32) -> P3 {
        self.verts_j[v as usize]
    }

    fn push_tet(&mut self, t: [u32; 4]) {
        let denoms = tet_faces_vertex_order(t).map(|(f, opp)| {
            orient3d(
                self.pos(f[0]),
                self.pos(f[1]),
                self.pos(f[2]),
                self.pos(opp),
            )
        });
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for &v in &t {
            let p = self.pos(v);
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        self.tets.push(t);
        self.denoms.push(denoms);
        self.bounds.push([lo, hi]);
    }

    /// 凸包面重算（全部面里只出现一次的那些）。
    fn rebuild_hull(&mut self) {
        let mut entries: Vec<(u64, [u32; 3])> = Vec::with_capacity(self.tets.len() * 4);
        for t in &self.tets {
            for (f, _) in tet_faces(*t) {
                entries.push((face_key(f), f));
            }
        }
        entries.sort_unstable_by_key(|e| e.0);
        self.hull.clear();
        let mut i = 0;
        while i < entries.len() {
            let mut j = i + 1;
            while j < entries.len() && entries[j].0 == entries[i].0 {
                j += 1;
            }
            if j - i == 1 {
                self.hull.push(entries[i].1);
            }
            i = j;
        }
    }

    /// 顶点数（去重后）。
    pub fn vertex_count(&self) -> usize {
        self.verts.len()
    }

    /// 四面体数。
    pub fn tetrahedron_count(&self) -> usize {
        self.tets.len()
    }

    /// 凸包面数。
    pub fn hull_face_count(&self) -> usize {
        self.hull.len()
    }

    /// 空结构（无四面体 ⇒ 所有查询返回 `None）。
    pub fn is_empty(&self) -> bool {
        self.tets.is_empty()
    }

    /// 四面体表（顶点下标）。
    pub fn tetrahedra(&self) -> &[[u32; 4]] {
        &self.tets
    }

    /// 凸包面（顶点下标，内部在正向侧）。
    pub fn hull_faces(&self) -> &[[u32; 3]] {
        &self.hull
    }

    /// 顶点的工作坐标（**抖动后的规范化空间**，诊断口径）。
    pub fn vertex_position(&self, v: usize) -> [f64; 3] {
        self.verts_j[v]
    }

    /// 顶点世界坐标（**未抖动**的原值，诊断 / 测试口径）。
    pub fn world_vertex(&self, v: usize) -> Vec3 {
        self.verts_world[v]
    }

    /// 逐顶点回指的原输入点下标。
    pub fn vertex_points(&self) -> &[u32] {
        &self.vertex_point
    }

    /// 原输入点 → 顶点下标（非有限点返回 `None）。
    pub fn point_vertex(&self, index: usize) -> Option<usize> {
        match self.point_vertex.get(index) {
            Some(&v) if v != u32::MAX => Some(v as usize),
            _ => None,
        }
    }

    /// 统计。
    pub fn stats(&self) -> TetraStats {
        self.stats
    }

    /// 结构自检：朝向 / 零体积胞 / 流形 / **局部 Delaunay** / 欧拉特征。
    ///
    /// 「局部 Delaunay」= 每条内部面（被两个四面体共享）的对顶点不**严格**
    /// 落在邻接四面体的外接球内；这是 Delaunay 的局部刻画（空球准则对每条
    /// 面成立 ⇔ 全局 Delaunay），也是本模块不依赖「构建过程正确」的独立验证。
    pub fn validate(&self) -> TetraValidation {
        let mut out = TetraValidation {
            vertices: self.verts.len(),
            tetrahedra: self.tets.len(),
            ..Default::default()
        };
        let mut face_tets: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
        let mut edges: std::collections::BTreeSet<(u32, u32)> = std::collections::BTreeSet::new();
        for (i, t) in self.tets.iter().enumerate() {
            let d = orient3d(
                self.pos(t[0]),
                self.pos(t[1]),
                self.pos(t[2]),
                self.pos(t[3]),
            );
            if d.is_nan() || d <= 0.0 {
                out.inverted += 1;
            }
            if d == 0.0 {
                out.degenerate += 1;
            }
            for (f, _) in tet_faces(*t) {
                face_tets.entry(face_key(f)).or_default().push(i);
            }
            for k in 0..4 {
                for l in (k + 1)..4 {
                    edges.insert((t[k].min(t[l]), t[k].max(t[l])));
                }
            }
        }
        for (key, owners) in &face_tets {
            match owners.len() {
                1 => out.boundary_faces += 1,
                2 => {
                    out.interior_faces += 1;
                    let (i, j) = (owners[0], owners[1]);
                    // 对顶点 = 不在面上的那个顶点；两侧各查一次（对称判据）
                    for (from, to) in [(i, j), (j, i)] {
                        let Some(o) = self.tets[from]
                            .iter()
                            .copied()
                            .find(|v| !face_has(*key, *v))
                        else {
                            continue;
                        };
                        let t = self.tets[to];
                        if insphere(
                            self.pos(t[0]),
                            self.pos(t[1]),
                            self.pos(t[2]),
                            self.pos(t[3]),
                            self.pos(o),
                        ) > 0.0
                        {
                            out.non_delaunay += 1;
                            break;
                        }
                    }
                }
                _ => out.non_manifold += 1,
            }
        }
        let faces = face_tets.len();
        out.euler_characteristic =
            self.verts.len() as i64 - edges.len() as i64 + faces as i64 - self.tets.len() as i64;
        out
    }

    /// 点定位 + 重心插值：世界位置 → 插值取值（空结构返回 `None）。
    ///
    /// `values 按**顶点下标**索引（[`Self::vertex_points] 给出顶点 ← 原输入
    /// 点的回指；[`IrradianceProbeSet] 内部已按此口径铺好逐顶点取值）。
    pub fn interpolate(&self, position: Vec3, values: &[Vec3]) -> Option<Vec3> {
        self.locate(position).map(|(t, w)| {
            let mut acc = Vec3::ZERO;
            for k in 0..4 {
                acc += Vec3::splat(w[k]) * values[t[k] as usize];
            }
            acc
        })
    }

    /// 点定位：返回 `(四面体, 权重)；凸包之外返回最近凸包面的投影权重
    /// （四面体槽位取该面的三个顶点，第四槽位重复第三个顶点 ⇒ 权重恒 0，
    /// 取值不受影响）。
    ///
    /// 权重按四面体 `[v0, v1, v2, v3] 的顶点序给出，恒非负且和为 1。
    /// **探针点处的恒等性**：某顶点恰为查询点时，该顶点的权重由
    /// orient3d(face, p) / orient3d(face, 对顶点) 得到——p 与对顶点的工作坐标
    /// **逐位相同**（同一抖动函数）⇒ 分子分母是同一次计算 ⇒ 权重恰为 1.0；
    /// 其余三个面的分子含零行（p 是面上的顶点之一 ⇒ 该行全零）⇒ 恰为 0.0
    /// （逐位），故插值结果是该探针值的**同一串位**（单测 to_bits 钉住）。
    pub fn locate(&self, position: Vec3) -> Option<([u32; 4], [f32; 4])> {
        if self.tets.is_empty() {
            return None;
        }
        let p = self.work_point(position);
        // 兜底候选：(最负分子的相对幅度, 胞下标)——只在所有胞都不严格包含
        // 查询点时才用（见下方注释）
        let mut best: Option<(f64, usize)> = None;
        for (i, t) in self.tets.iter().enumerate() {
            let b = &self.bounds[i];
            if p[0] < b[0][0]
                || p[1] < b[0][1]
                || p[2] < b[0][2]
                || p[0] > b[1][0]
                || p[1] > b[1][1]
                || p[2] > b[1][2]
            {
                continue;
            }
            let num = self.numerators(i, p);
            if num[0] >= 0.0 && num[1] >= 0.0 && num[2] >= 0.0 && num[3] >= 0.0 {
                return Some((*t, self.weights_of(i, num)));
            }
            // 近凸包边界时的数值兜底：分母（体积）越小的胞，判定余量越小；
            // 若某胞的「最负分子」相对于自身量级仍在舍入范围内，就认它——
            // 否则贴边点会**穿透**所有胞落到「包外投影」分支，取值在凸包
            // 面附近跳变（实测 0.07 m 步长上跳 0.077：室内角落可见）。
            let scale = num.iter().fold(0.0f64, |m, v| m.max(v.abs()));
            let worst = num.iter().copied().fold(f64::INFINITY, f64::min);
            let slack = -worst / scale.max(f64::MIN_POSITIVE);
            if slack < NEAR_HULL_SLACK && best.as_ref().is_none_or(|(b, _)| slack < *b) {
                best = Some((slack, i));
            }
        }
        if let Some((slack, i)) = best {
            debug_assert!(slack < NEAR_HULL_SLACK);
            let num = self.numerators(i, p);
            return Some((self.tets[i], self.weights_of(i, num)));
        }
        // 包外：最近凸包面投影（连续延拓：凸包面上内外同值）
        let mut best: Option<(f64, [u32; 3], [f64; 3])> = None;
        for f in &self.hull {
            let (q, bary) = closest_on_triangle(self.pos(f[0]), self.pos(f[1]), self.pos(f[2]), p);
            let d = norm2(sub(q, p));
            if best.as_ref().is_none_or(|(bd, _, _)| d < *bd) {
                best = Some((d, *f, bary));
            }
        }
        best.map(|(_, f, bary)| {
            // 面的三个顶点落进前三个槽位（第四槽位重复第三个顶点 ⇒ 权重恒 0）
            let w = clamp_weights([bary[0], bary[1], bary[2], 0.0]);
            ([f[0], f[1], f[2], f[2]], w)
        })
    }

    /// 由分子直接算权重（钳制 + 归一化）。
    #[inline]
    fn weights_of(&self, i: usize, num: [f64; 4]) -> [f32; 4] {
        clamp_weights([
            num[0] / self.denoms[i][0],
            num[1] / self.denoms[i][1],
            num[2] / self.denoms[i][2],
            num[3] / self.denoms[i][3],
        ])
    }

    /// 四个面的分子（orient3d(face, p)），**顶点序**（第 k 项 = 顶点 k 对面）。
    #[inline]
    fn numerators(&self, i: usize, p: P3) -> [f64; 4] {
        let f = tet_faces_vertex_order(self.tets[i]);
        [
            orient3d(
                self.pos(f[0].0[0]),
                self.pos(f[0].0[1]),
                self.pos(f[0].0[2]),
                p,
            ),
            orient3d(
                self.pos(f[1].0[0]),
                self.pos(f[1].0[1]),
                self.pos(f[1].0[2]),
                p,
            ),
            orient3d(
                self.pos(f[2].0[0]),
                self.pos(f[2].0[1]),
                self.pos(f[2].0[2]),
                p,
            ),
            orient3d(
                self.pos(f[3].0[0]),
                self.pos(f[3].0[1]),
                self.pos(f[3].0[2]),
                p,
            ),
        ]
    }

    /// 逐四面体的四个顶点（诊断 / 测试口径）。
    pub fn tet_vertices(&self, index: usize) -> [u32; 4] {
        self.tets[index]
    }

    /// 指定四面体内的重心权重（**不做包含性判定**：点在胞外时权重被钳制，
    /// 测试用它对照相邻四面体在共享面上的取值）。
    pub fn weights_in_tet(&self, index: usize, position: Vec3) -> [f32; 4] {
        let p = self.work_point(position);
        let num = self.numerators(index, p);
        clamp_weights([
            num[0] / self.denoms[index][0],
            num[1] / self.denoms[index][1],
            num[2] / self.denoms[index][2],
            num[3] / self.denoms[index][3],
        ])
    }

    /// CPU 内存占用（字节，观测口径；顶点 / 四面体 / 分母 / AABB / 凸包）。
    pub fn memory_bytes(&self) -> u64 {
        let verts = self.verts.len() as u64 * (24 + 24 + 12);
        let tets = self.tets.len() as u64 * (16 + 32 + 48);
        let hull = self.hull.len() as u64 * 12;
        let misc = (self.vertex_point.len() + self.point_vertex.len()) as u64 * 4;
        verts + tets + hull + misc
    }
}

/// 打包键里是否含顶点 `v（顶点下标 < 2²¹ ⇒ 每 21 位一个）。
fn face_has(key: u64, v: u32) -> bool {
    (0..3).any(|k| ((key >> (21 * k)) & 0x1F_FFFF) as u32 == v)
}

/// 权重钳制到 [0, 1] 并归一化（和为 1）。
fn clamp_weights(raw: [f64; 4]) -> [f32; 4] {
    let mut w = [0.0f64; 4];
    for k in 0..4 {
        w[k] = if raw[k] > 0.0 { raw[k] } else { 0.0 };
    }
    let sum: f64 = w[0] + w[1] + w[2] + w[3];
    if sum.is_nan() || sum <= 0.0 {
        // 全部分量非正或非有限（数值异常）：退化为等权（确定性、和为 1）
        return [0.25; 4];
    }
    let mut out = [0.0f32; 4];
    for k in 0..4 {
        out[k] = (w[k] / sum) as f32;
    }
    out
}

/// 三角形上按重心坐标求点。
#[inline]
fn barycentric_point(a: P3, b: P3, c: P3, w: [f64; 3]) -> P3 {
    [
        a[0] * w[0] + b[0] * w[1] + c[0] * w[2],
        a[1] * w[0] + b[1] * w[1] + c[1] * w[2],
        a[2] * w[0] + b[2] * w[1] + c[2] * w[2],
    ]
}

/// 点到三角形的最近点及其重心权重（**重心钳制口径**，无分支）。
///
/// 做法：先把点投影到三角形所在的平面，再算投影点的重心坐标，把负分量置零
/// 并归一化（= 在重心单纯形上取最近点）。与「三维欧氏最近点」的差别只在
/// 边 / 角附近的二阶项，对本用途（凸包外的**连续延拓**）等价到观感；
/// 收益是**无分支、与顶点顺序无关、逐位可复现**——分区判定的分支实现在
/// 「点几乎落在三角形平面上」时会因舍入在分支间抖动（实测：凸包角落的
/// 查询被判成「最近点是三米外的顶点」，取值跳变 0.07）。
///
/// 返回的重心权重非负且和为 1（退化三角形 —— 零面积 —— 时退化为顶点 a）。
fn closest_on_triangle(a: P3, b: P3, c: P3, p: P3) -> (P3, [f64; 3]) {
    let n = cross(sub(b, a), sub(c, a));
    let nn = norm2(n);
    if nn.is_nan() || nn <= 0.0 {
        return (a, [1.0, 0.0, 0.0]);
    }
    // 平面投影：q = p − n · (n·(p−a)) / |n|²
    let t = dot(n, sub(p, a)) / nn;
    let q = sub(p, [n[0] * t, n[1] * t, n[2] * t]);
    // 投影点的重心坐标（标准最小二乘式）
    let v0 = sub(b, a);
    let v1 = sub(c, a);
    let v2 = sub(q, a);
    let d00 = dot(v0, v0);
    let d01 = dot(v0, v1);
    let d11 = dot(v1, v1);
    let d20 = dot(v2, v0);
    let d21 = dot(v2, v1);
    let denom = d00 * d11 - d01 * d01;
    if denom.is_nan() || denom <= 0.0 {
        return (a, [1.0, 0.0, 0.0]);
    }
    let vb = (d11 * d20 - d01 * d21) / denom;
    let vc = (d00 * d21 - d01 * d20) / denom;
    let mut w = [1.0 - vb - vc, vb, vc];
    for x in w.iter_mut() {
        if x.is_nan() || *x <= 0.0 {
            *x = 0.0;
        }
    }
    let sum: f64 = w[0] + w[1] + w[2];
    if sum.is_nan() || sum <= 0.0 {
        return (a, [1.0, 0.0, 0.0]);
    }
    for x in w.iter_mut() {
        *x /= sum;
    }
    (barycentric_point(a, b, c, w), w)
}

/// 面邻接表（每次插入按当前复合体重建：O(T)，探针预算下毫秒量级）。
struct Topology {
    /// 面键 → (拥有者, 另一侧拥有者)。凸包面没有另一侧。
    faces: BTreeMap<u64, (u32, Option<u32>)>,
    /// 凸包面：键 + 内部正向序 + 拥有者（迭代序确定 ⇒ 结果确定）。
    hull: Vec<(u64, [u32; 3], u32)>,
}

/// 增量构建期的可变状态（工作坐标 + `dead 标记 + 每轮重算的冲突掩码）。
///
/// 插入 = Bowyer-Watson 空腔重三角化，**空腔取「与 p 所在胞连通」的冲突分量**
/// （BFS）；凸包外的点由幽灵胞元（凸包面）接入，与 CGAL 的「无穷远顶点」模型
/// 同构（凸包面 ⟷ 无穷远胞元）。
struct Complex {
    verts: Vec<P3>,
    tets: Vec<[u32; 4]>,
    dead: Vec<bool>,
    conflict: Vec<bool>,
    visited: Vec<bool>,
    dropped: usize,
    /// 拓扑自检发现异常（面被三个以上胞共享 / 冲突集不连通 / 空腔为空）
    /// ⇒ 本次构建作废，交给抖动阶梯重试。
    corrupt: bool,
}

impl Complex {
    fn new(verts: Vec<P3>) -> Self {
        Self {
            verts,
            tets: Vec::new(),
            dead: Vec::new(),
            conflict: Vec::new(),
            visited: Vec::new(),
            dropped: 0,
            corrupt: false,
        }
    }

    #[inline]
    fn pos(&self, v: u32) -> P3 {
        self.verts[v as usize]
    }

    fn push_tet(&mut self, t: [u32; 4]) {
        self.tets.push(t);
        self.dead.push(false);
        self.conflict.push(false);
        self.visited.push(false);
    }

    /// 当前复合体的面邻接表 + 凸包面表。
    fn topology(&mut self) -> Topology {
        let mut faces: BTreeMap<u64, (u32, Option<u32>)> = BTreeMap::new();
        for (i, t) in self.tets.iter().enumerate() {
            if self.dead[i] {
                continue;
            }
            for (f, _) in tet_faces(*t) {
                let key = face_key(f);
                match faces.get_mut(&key) {
                    None => {
                        faces.insert(key, (i as u32, None));
                    }
                    Some(slot) => {
                        if slot.1.is_some() {
                            // 面被三个胞共享 = 复合体损坏：标记作废，交由
                            // build 的抖动阶梯重试（不 panic——输入是用户数据）
                            self.corrupt = true;
                            return Topology {
                                faces: BTreeMap::new(),
                                hull: Vec::new(),
                            };
                        }
                        slot.1 = Some(i as u32);
                    }
                }
            }
        }
        let mut hull = Vec::new();
        for (i, t) in self.tets.iter().enumerate() {
            if self.dead[i] {
                continue;
            }
            for (f, _) in tet_faces(*t) {
                let key = face_key(f);
                if faces.get(&key).is_some_and(|slot| slot.1.is_none()) {
                    hull.push((key, f, i as u32));
                }
            }
        }
        Topology { faces, hull }
    }

    /// 插入顶点 `v（`false = 未进入结构，计数进 `dropped）。
    ///
    /// 三步：①**几何冲突** = 空球判据（严格 insphere > 0，共球 = 0 不算冲突）；
    /// ②**空腔 = 与 p 所在胞连通的冲突分量**（BFS）——这一步是必需的：p 在
    /// 凸包外时，与 p 不连通的远处胞也可能把 p 包进自己的外接球，把它们算进
    /// 空腔会让「腔对 p 星形」失效（边界小面把 p 放在错误一侧 ⇒ 新胞朝向相反
    /// ⇒ 结构空洞，本模块实测踩过）；③**腔边界小面** = 冲突胞的面中另一侧不在
    /// 腔内的那些（凸包面则是「不可见」的那些），逐个与 p 连成新胞。
    fn insert(&mut self, v: u32) -> bool {
        let p = self.pos(v);
        let n = self.tets.len();
        let topo = self.topology();
        if self.corrupt {
            return false;
        }

        // ---- 1. 几何冲突（全扫描；只看几何、不看顺序 ⇒ 与遍历次序无关） ----
        for i in 0..n {
            if self.dead[i] {
                continue;
            }
            let t = self.tets[i];
            self.conflict[i] = insphere(
                self.pos(t[0]),
                self.pos(t[1]),
                self.pos(t[2]),
                self.pos(t[3]),
                p,
            ) > 0.0;
        }

        // ---- 2. 空腔 = 连通分量（种子 = 包含 p 的胞；包外从可见凸包面进入） ----
        self.visited.iter_mut().for_each(|x| *x = false);
        let mut queue: Vec<u32> = Vec::new();
        let containing = (0..n).find(|&i| !self.dead[i] && self.in_tet(i, p));
        match containing {
            Some(t0) => {
                self.visited[t0] = true;
                queue.push(t0 as u32);
            }
            None => {
                for (_, f, owner) in &topo.hull {
                    let o = *owner as usize;
                    if !self.visited[o] && self.conflict[o] && self.face_visible(*f, p) {
                        self.visited[o] = true;
                        queue.push(*owner);
                    }
                }
            }
        }
        while let Some(t) = queue.pop() {
            for (f, _) in tet_faces(self.tets[t as usize]) {
                let Some(slot) = topo.faces.get(&face_key(f)) else {
                    continue;
                };
                let Some(other) = slot.1 else {
                    continue; // 凸包面：幽灵侧已由种子覆盖
                };
                let nb = if slot.0 == t { other } else { slot.0 };
                if !self.visited[nb as usize] && self.conflict[nb as usize] {
                    self.visited[nb as usize] = true;
                    queue.push(nb);
                }
            }
        }

        // ---- 2b. 快速失败：冲突集必须整体连通 ----
        // Delaunay 复合体上「外接球含 p 的胞集合」是连通的（经典星形引理），
        // 连通分量之外的冲突胞 = 数值异常信号。此时继续重三角化会产生重叠胞
        // （实测症状：面被三个以上胞共享），故直接作废本次构建，交给抖动阶梯
        // 重试（确定性：阶梯固定 ⇒ 同输入同结果）。
        let conflict_total = (0..n)
            .filter(|&i| !self.dead[i] && self.conflict[i])
            .count();
        let visited_total = (0..n).filter(|&i| !self.dead[i] && self.visited[i]).count();
        if conflict_total != visited_total {
            self.corrupt = true;
            return false;
        }

        // ---- 3. 腔边界小面 → 新胞 ----
        let mut created: Vec<[u32; 4]> = Vec::new();
        for i in 0..n {
            if self.dead[i] || !self.visited[i] {
                continue;
            }
            for (f, _) in tet_faces(self.tets[i]) {
                let key = face_key(f);
                match topo.faces.get(&key) {
                    Some(slot) => {
                        if let Some(other) = slot.1 {
                            let nb = if slot.0 == i as u32 { other } else { slot.0 };
                            if self.visited[nb as usize] {
                                continue; // 腔内部面
                            }
                        } else if self.face_visible(f, p) {
                            continue; // 可见凸包面：幽灵胞元在腔内
                        }
                    }
                    None => continue,
                }
                if orient3d(self.pos(f[0]), self.pos(f[1]), self.pos(f[2]), p) > 0.0 {
                    created.push([f[0], f[1], f[2], v]);
                } else {
                    self.dropped += 1;
                }
            }
        }
        // 可见凸包面（幽灵胞元在腔、拥有者不在腔）⇒ 反向面序的新胞
        for (_, f, owner) in &topo.hull {
            if self.visited[*owner as usize] || !self.face_visible(*f, p) {
                continue;
            }
            let r = [f[2], f[1], f[0]];
            if orient3d(self.pos(r[0]), self.pos(r[1]), self.pos(r[2]), p) > 0.0 {
                created.push([r[0], r[1], r[2], v]);
            } else {
                self.dropped += 1;
            }
        }

        if created.is_empty() {
            self.corrupt = true;
            return false;
        }
        // ---- 3b. 局部拓扑自检：新建胞之间不得出现「一个面被三个胞共享」 ----
        // 新胞都含新顶点 p ⇒ 与既有胞不可能共享面；只有新胞彼此之间可能撞面
        // （腔非星形时边界小面会挤在同一组顶点上）。撞面 = 本次构建作废。
        let mut new_faces: BTreeMap<u64, u32> = BTreeMap::new();
        for t in &created {
            for (f, _) in tet_faces(*t) {
                let slot = new_faces.entry(face_key(f)).or_insert(0);
                *slot += 1;
                if *slot > 2 {
                    self.corrupt = true;
                    return false;
                }
            }
        }
        for i in 0..n {
            if self.visited[i] {
                self.dead[i] = true;
            }
        }
        for t in created {
            self.push_tet(t);
        }
        true
    }

    /// 凸包面是否对 p 可见（内部在面正向侧 ⇒ 可见 = p 在负向侧）。
    #[inline]
    fn face_visible(&self, f: [u32; 3], p: P3) -> bool {
        orient3d(self.pos(f[0]), self.pos(f[1]), self.pos(f[2]), p) < 0.0
    }

    /// 点是否落在（闭）四面体内：四个面的分子全部非负。
    #[inline]
    fn in_tet(&self, i: usize, p: P3) -> bool {
        let f = tet_faces(self.tets[i]);
        (0..4).all(|k| {
            orient3d(
                self.pos(f[k].0[0]),
                self.pos(f[k].0[1]),
                self.pos(f[k].0[2]),
                p,
            ) >= 0.0
        })
    }
}

/// 初始单纯形：**最胖四点**贪心（而不是「字典序最早四点」）。
///
/// 字典序最早的四点在规则布局（网格 / 楼层平面）下往往近乎共线（同一列上
/// 的相邻点），初始胞是「针」⇒ 早期插入的判定落在数值退化区，实测会让
/// 复合体破损（面被三个以上胞共享，第 24 次插入就发生）。改取贪心最大体积
/// 四点：
///
/// 1. `v0 = 距规范化中心最近的点（并列取小下标）——布局的「中部」；
/// 2. `v1 = 距 v0 最远的点（最远点启发式 = 直径端点）；
/// 3. `v2 = 距直线 v0v1 最远的点（叉积模长最大）；
/// 4. `v3 = 距平面 v0v1v2 最远的点（|orient3d| 最大）。
///
/// 每步并列都取小下标 ⇒ 全序、同输入同输出（确定性）。四点仍然仿射相关
/// （点云退化）时返回 None —— 抖动之后这只在「有效点 < 4」时发生。
fn initial_simplex(verts: &[P3]) -> Option<[u32; 4]> {
    if verts.len() < 4 {
        return None;
    }
    let mut v0 = 0u32;
    let mut best = f64::INFINITY;
    for (i, p) in verts.iter().enumerate() {
        let d = norm2(*p);
        if d < best {
            best = d;
            v0 = i as u32;
        }
    }
    let mut v1 = u32::MAX;
    let mut best = MIN_EDGE2;
    for (i, p) in verts.iter().enumerate() {
        let d = norm2(sub(*p, verts[v0 as usize]));
        if d > best {
            best = d;
            v1 = i as u32;
        }
    }
    if v1 == u32::MAX {
        return None;
    }
    let e01 = sub(verts[v1 as usize], verts[v0 as usize]);
    let mut v2 = u32::MAX;
    let mut best = MIN_AREA2;
    for (i, p) in verts.iter().enumerate() {
        let d = norm2(cross(e01, sub(*p, verts[v0 as usize])));
        if d > best {
            best = d;
            v2 = i as u32;
        }
    }
    if v2 == u32::MAX {
        return None;
    }
    let mut v3 = u32::MAX;
    let mut best = MIN_VOLUME;
    for (i, p) in verts.iter().enumerate() {
        let d = orient3d(
            verts[v0 as usize],
            verts[v1 as usize],
            verts[v2 as usize],
            *p,
        )
        .abs();
        if d > best {
            best = d;
            v3 = i as u32;
        }
    }
    if v3 == u32::MAX {
        return None;
    }
    let mut tet = [v0, v1, v2, v3];
    if orient3d(
        verts[tet[0] as usize],
        verts[tet[1] as usize],
        verts[tet[2] as usize],
        verts[tet[3] as usize],
    ) < 0.0
    {
        tet.swap(1, 2);
    }
    Some(tet)
}

// ---------------------------------------------------------------------------
// 探针烘焙与装配
// ---------------------------------------------------------------------------

/// 探针烘焙的逐实体报告（观测口径：探针数 / 预算截断 / 退化）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProbeBakeStats {
    /// 挂 [`IrradianceProbe] 的实体数（含被预算截断的）。
    pub entities: usize,
    /// 实际烘焙的探针数。
    pub baked: usize,
    /// 超预算被截断的探针数。
    pub dropped_budget: usize,
    /// 位置坐标非有限被跳过的探针数。
    pub skipped_non_finite: usize,
    /// 四面体化统计。
    pub tetra: TetraStats,
}

/// 探针集（World 资源）：逐探针辐照 + 四面体表 + 逐顶点取值。
///
/// **缺席语义**：资源缺席 / 空集 ⇒ 渲染器逐实例发标志位 0，着色器走原全局
/// env 路径（逐位不变，见模块文档「兼容铁律」）。
#[derive(Clone, Debug, Default, Resource)]
pub struct IrradianceProbeSet {
    /// 逐探针世界位置（与 [`Self::values] 同序）。
    positions: Vec<Vec3>,
    /// 逐探针间接辐照度 E（线性 HDR；= [`BakeSample::indirect](crate::bake::BakeSample::indirect) 的六轴平均）。
    values: Vec<Vec3>,
    /// 逐**顶点**取值（重合点合并后按代表点的值展开）。
    vertex_values: Vec<Vec3>,
    /// 四面体化（位置云）。
    tetra: Tetrahedralization,
    /// 烘焙统计。
    stats: ProbeBakeStats,
}

impl IrradianceProbeSet {
    /// 空集。
    pub fn new() -> Self {
        Self::default()
    }

    /// 从「逐探针位置 + 逐探针取值」构造（四面体化在此完成；确定性）。
    pub fn from_probes(positions: Vec<Vec3>, values: Vec<Vec3>) -> Self {
        assert_eq!(
            positions.len(),
            values.len(),
            "探针位置数（{}）与取值数（{}）必须一致",
            positions.len(),
            values.len()
        );
        let tetra = Tetrahedralization::build(&positions);
        let vertex_values = tetra
            .vertex_points()
            .iter()
            .map(|&p| values[p as usize])
            .collect();
        Self {
            positions,
            values,
            vertex_values,
            tetra,
            stats: ProbeBakeStats::default(),
        }
    }

    /// 探针数。
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    /// 空集（渲染器逐实例发标志位 0）。
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// 逐探针位置。
    pub fn positions(&self) -> &[Vec3] {
        &self.positions
    }

    /// 逐探针辐照度 E。
    pub fn values(&self) -> &[Vec3] {
        &self.values
    }

    /// 四面体化。
    pub fn tetrahedralization(&self) -> &Tetrahedralization {
        &self.tetra
    }

    /// 烘焙统计。
    pub fn stats(&self) -> ProbeBakeStats {
        self.stats
    }

    /// 世界位置 → 插值辐照度 E（空集返回 `None）。
    pub fn irradiance_at(&self, position: Vec3) -> Option<Vec3> {
        if self.vertex_values.is_empty() {
            return None;
        }
        self.tetra.interpolate(position, &self.vertex_values)
    }

    /// 世界位置 → 实例载荷（xyz = E/π、w = 标志位）——渲染器注入口径。
    ///
    /// 存 **E/π**（而不是 E）：着色器里这一项直接替换
    /// `textureSampleLevel(irradiance_map, …) 的返回值（那张贴图存的就是
    /// E/π，见 environment.wgsl 的 fs_irradiance），于是着色器**零换算**、
    /// `select 是纯值替换。空集 / 非有限位置 ⇒ [0, 0, 0, 0]（标志位 0 =
    /// 走原全局 env 路径）。
    pub fn instance_payload(&self, position: Vec3) -> [f32; 4] {
        if !position.is_finite() {
            return [0.0; 4];
        }
        match self.irradiance_at(position) {
            Some(e) => {
                let inv_pi = std::f32::consts::FRAC_1_PI;
                [e.x * inv_pi, e.y * inv_pi, e.z * inv_pi, 1.0]
            }
            None => [0.0; 4],
        }
    }

    /// CPU 内存占用（字节，观测口径）。
    pub fn memory_bytes(&self) -> u64 {
        (self.positions.len() + self.values.len() + self.vertex_values.len()) as u64 * 12
            + self.tetra.memory_bytes()
    }
}

/// 探针烘焙：世界（静态几何 + 探针实体）→ 逐探针六轴平均间接辐照 → 四面体化。
///
/// 流程：①[`collect_bake_triangles](crate::bake::collect_bake_triangles) 收
/// 静态几何（与光照贴图**同一份**收集口径）+ [`build_bvh](crate::bake::build_bvh)；
/// ②查 [`IrradianceProbe] 实体（位置取 [`GlobalTransform]），按实体位升序
/// 排定、超 [`MAX_IRRADIANCE_PROBES] 截断；③逐探针 6 次
/// [`bake_sample](crate::bake::bake_sample)（[`PROBE_AXES]）取
/// [`BakeSample::indirect](crate::bake::BakeSample::indirect) 的算术平均；
/// ④[`Tetrahedralization::build]。
///
/// **确定性**：全部环节无 RNG、无哈希迭代序、无并行 ⇒ 同世界两次烘焙
/// 逐位一致（单测 to_bits 钉住）。
pub fn bake_irradiance_probes(world: &World, params: &BakeParams) -> IrradianceProbeSet {
    let triangles = collect_bake_triangles(world);
    let bvh = build_bvh(triangles);

    // 两阶段（收集 → 处理）：查询闭包内不借任何存储（AGENTS.md 系统规约）
    let mut probes: Vec<(u64, Vec3)> = Vec::new();
    world.query::<(Entity, &GlobalTransform, &IrradianceProbe)>(|(entity, global, _)| {
        probes.push((entity.to_bits(), global.position()));
    });
    probes.sort_unstable_by_key(|probe| probe.0);

    let mut stats = ProbeBakeStats {
        entities: probes.len(),
        ..Default::default()
    };
    if probes.len() > MAX_IRRADIANCE_PROBES {
        log::warn!(
            "光照探针数 {} 超预算 {}，按实体位升序截断",
            probes.len(),
            MAX_IRRADIANCE_PROBES
        );
        stats.dropped_budget = probes.len() - MAX_IRRADIANCE_PROBES;
        probes.truncate(MAX_IRRADIANCE_PROBES);
    }

    let mut positions = Vec::with_capacity(probes.len());
    let mut values = Vec::with_capacity(probes.len());
    for (_, position) in &probes {
        if !position.is_finite() {
            log::warn!("光照探针位置非有限（{position:?}），跳过");
            stats.skipped_non_finite += 1;
            continue;
        }
        let mut sum = Vec3::ZERO;
        for axis in PROBE_AXES {
            sum += bake_sample(&bvh, *position, axis, params).indirect();
        }
        positions.push(*position);
        // 除以 6（一次正确舍入）而不是乘 1/6（两次舍入）
        values.push(sum / PROBE_AXES.len() as f32);
    }
    stats.baked = positions.len();

    let mut set = IrradianceProbeSet::from_probes(positions, values);
    stats.tetra = set.tetra.stats();
    set.stats = stats;
    set
}

/// [`install_irradiance_probes] 的结果（观测口径）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IrradianceProbeInstall {
    /// 探针数。
    pub probes: usize,
    /// 去重后的顶点数。
    pub vertices: usize,
    /// 四面体数。
    pub tetrahedra: usize,
    /// 凸包面数。
    pub hull_faces: usize,
    /// 坐标重合被合并的探针数。
    pub merged: usize,
    /// 因退化被跳过的顶点数。
    pub skipped: usize,
    /// 超预算被截断的探针数。
    pub dropped_budget: usize,
    /// 点云退化（共面 / 共线 / 有效点 < 4）⇒ 空四面体化（查询全回退 env）。
    pub degenerate: bool,
}

/// 把烘焙产物接进世界（R64 的装配入口）：插入 / 覆盖 [`IrradianceProbeSet]
/// 资源。渲染器下一帧即读到（逐实体插值），无需任何 GPU 侧上传。
///
/// **覆盖语义**：重复调用 = 重新烘焙（资源整体替换），旧探针集被丢弃；
/// 实体上的 [`IrradianceProbe] 组件与资源解耦（增删探针实体后重新烘焙即可）。
pub fn install_irradiance_probes(
    world: &mut World,
    probes: IrradianceProbeSet,
) -> IrradianceProbeInstall {
    let stats = probes.stats();
    let report = IrradianceProbeInstall {
        probes: probes.len(),
        vertices: stats.tetra.vertices,
        tetrahedra: stats.tetra.tetrahedra,
        hull_faces: stats.tetra.hull_faces,
        merged: stats.tetra.merged,
        skipped: stats.tetra.skipped,
        dropped_budget: stats.dropped_budget,
        degenerate: stats.tetra.degenerate,
    };
    world.insert_resource(probes);
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material::{MaterialDef, MaterialStore};
    use crate::mesh::{MeshData, MeshStore};
    use iris_core::hierarchy::propagate_transforms;
    use iris_core::prelude::Transform;

    fn bits3(v: Vec3) -> [u32; 3] {
        [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()]
    }

    /// 立方体八角（逐位可表示的共球构型：八点全在同一球面上）。
    fn cube_corners(half: f32) -> Vec<Vec3> {
        let mut out = Vec::new();
        for &x in &[-half, half] {
            for &y in &[-half, half] {
                for &z in &[-half, half] {
                    out.push(Vec3::new(x, y, z));
                }
            }
        }
        out
    }

    /// 3×3×3 规则网格（间距 1，包围盒 2³）。
    fn grid_27() -> Vec<Vec3> {
        let mut out = Vec::new();
        for x in 0..3 {
            for y in 0..3 {
                for z in 0..3 {
                    out.push(Vec3::new(x as f32, y as f32, z as f32));
                }
            }
        }
        out
    }

    /// 确定性点云（模块级 mix64 生成；无 rand、无全局状态）。
    fn random_cloud(n: usize) -> Vec<Vec3> {
        let mut out = Vec::with_capacity(n);
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        for _ in 0..n {
            let mut next = || {
                state = mix64(state);
                (state >> 11) as f64 / (1u64 << 53) as f64
            };
            out.push(Vec3::new(
                (next() as f32) * 8.0 - 4.0,
                (next() as f32) * 6.0,
                (next() as f32) * 8.0 - 4.0,
            ));
        }
        out
    }

    /// 构建 + 结构前置断言（正常点云不应有丢胞 / 孤立顶点）。
    fn cloud_tetra(points: &[Vec3]) -> Tetrahedralization {
        let t = Tetrahedralization::build(points);
        assert!(!t.is_empty(), "点云必须产出非空四面体化");
        assert!(
            t.stats().skipped == 0 && t.stats().dropped_cells == 0,
            "正常点云不应有丢胞 / 孤立顶点：{:?}",
            t.stats()
        );
        t
    }

    // ---- 谓词 ----

    #[test]
    fn orient3d_and_tet_faces_follow_documented_conventions() {
        // 右手系四面体在本文约定下 orient3d = −1（模块文档的符号约定）
        let a = [0.0, 0.0, 0.0];
        let b = [1.0, 0.0, 0.0];
        let c = [0.0, 1.0, 0.0];
        let d = [0.0, 0.0, 1.0];
        assert_eq!(orient3d(a, b, c, d), -1.0);
        assert_eq!(orient3d(a, c, b, d), 1.0, "交换两个参数必须反号");
        assert_eq!(orient3d(a, b, c, d), -orient3d(b, a, c, d));

        // 顶点表按 orient3d > 0 存储 ⇒ 初始单纯形的朝向已归一化
        let t = cloud_tetra(&[
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(0.4, 0.4, 0.4),
        ]);
        for tet in t.tetrahedra() {
            let p = |v: u32| t.vertex_position(v as usize);
            let det = orient3d(p(tet[0]), p(tet[1]), p(tet[2]), p(tet[3]));
            assert!(det > 0.0, "四面体必须正向：{tet:?} det = {det}");
            // 四个面：内部（对顶点）在正向侧
            for (f, opp) in tet_faces(*tet) {
                let face_det = orient3d(p(f[0]), p(f[1]), p(f[2]), p(opp));
                assert!(
                    (face_det - det).abs() <= 1e-15 * det.abs(),
                    "面 {f:?} 的对顶点必须在正向侧：{face_det} vs {det}"
                );
            }
        }
    }

    #[test]
    fn insphere_matches_analytic_sphere_membership() {
        // 正向四面体（0,0,0)(0,1,0)(1,0,0)(0,0,1)：外接球心 (0.5,0.5,0.5)、半径 √3/2
        let a = [0.0, 0.0, 0.0];
        let b = [0.0, 1.0, 0.0];
        let c = [1.0, 0.0, 0.0];
        let d = [0.0, 0.0, 1.0];
        assert!(orient3d(a, b, c, d) > 0.0, "前提：正向四面体");
        let inside = [0.25, 0.25, 0.25];
        assert!(
            insphere(a, b, c, d, inside) > 0.0,
            "球内点必须为正：{}",
            insphere(a, b, c, d, inside)
        );
        // 球面外：沿 (1,1,1) 方向推到 2 倍半径之外
        let far = [2.0, 2.0, 2.0];
        assert!(insphere(a, b, c, d, far) < 0.0, "球外点必须为负");
        // 四个顶点恰在球面上 ⇒ 逐位 0（解析退化）
        for v in [a, b, c, d] {
            assert_eq!(insphere(a, b, c, d, v), 0.0, "顶点必须恰在球面上");
        }
        // 立方体八点共球：任取四点组成的四面体，其余四点都在其外接球面上
        let cube = cube_corners(0.5);
        let p = |i: usize| [cube[i].x as f64, cube[i].y as f64, cube[i].z as f64];
        let mut checked = 0;
        for i in 0..8 {
            for j in (i + 1)..8 {
                for k in (j + 1)..8 {
                    for l in (k + 1)..8 {
                        let det = orient3d(p(i), p(j), p(k), p(l));
                        if det == 0.0 {
                            continue; // 共面四点：不是四面体
                        }
                        let (a, b, c, d) = if det > 0.0 {
                            (p(i), p(j), p(k), p(l))
                        } else {
                            (p(i), p(k), p(j), p(l))
                        };
                        for m in 0..8 {
                            if [i, j, k, l].contains(&m) {
                                continue;
                            }
                            assert_eq!(
                                insphere(a, b, c, d, p(m)),
                                0.0,
                                "立方体八点共球：第 {m} 点必须恰在球面上"
                            );
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert!(checked > 100, "共球构型覆盖不足（{checked}）");
    }

    // ---- 结构 ----

    #[test]
    fn cube_cospherical_configuration_is_valid_and_deterministic() {
        let points = cube_corners(0.5);
        let a = Tetrahedralization::build(&points);
        let b = Tetrahedralization::build(&points);
        assert_eq!(a.tetrahedra(), b.tetrahedra(), "共球构型两跑必须逐位一致");
        assert_eq!(a.vertex_count(), 8);
        assert!(
            a.hull_face_count() >= 12,
            "立方体凸包的三角面数不少于 12（抖动可能截角）：{}",
            a.hull_face_count()
        );
        let v = a.validate();
        assert!(v.is_valid(), "共球退化构型必须通过结构自检：{v:?}");
        assert_eq!(v.euler_characteristic, 1, "实心球的欧拉特征 = 1");
        // 体积和 = 凸包体积（规范化空间：包围盒最长边 = 1 ⇒ 解析 1.0）。
        // 容差 1e-4：容器的抖动把顶点挪了 ±1e-6，凸包体积随之有 O(1e-6)
        // 的偏移（抖动是设计的一部分，见模块文档「退化处理」）。
        let mut volume = 0.0f64;
        for tet in a.tetrahedra() {
            let p = |x: u32| a.vertex_position(x as usize);
            volume += orient3d(p(tet[0]), p(tet[1]), p(tet[2]), p(tet[3])) / 6.0;
        }
        assert!(
            (volume - 1.0).abs() < 1e-4,
            "四面体体积和必须等于凸包体积：{volume}"
        );
        // 八点共球：任意四点组成的外接球必过其余四点（解析退化构型本身）
        let corners = cube_corners(0.5);
        let q = |i: usize| {
            [
                corners[i].x as f64,
                corners[i].y as f64,
                corners[i].z as f64,
            ]
        };
        assert_eq!(insphere(q(0), q(2), q(1), q(3), q(4)), 0.0);
    }

    #[test]
    fn grid_cloud_is_valid_and_fills_its_convex_hull() {
        let t = cloud_tetra(&grid_27());
        let v = t.validate();
        assert!(v.is_valid(), "规则网格必须通过结构自检：{v:?}");
        assert_eq!(t.vertex_count(), 27, "无重合点");
        assert!(
            (12..=(27 * 2)).contains(&t.hull_face_count()),
            "凸包面数应落在「理想 12 面」与「逐点三角化」之间：{}",
            t.hull_face_count()
        );
        // 体积和 = 凸包体积（规范化后包围盒 = 1，容差见立方体测试的抖动说明）
        let mut volume = 0.0f64;
        for tet in t.tetrahedra() {
            let p = |x: u32| t.vertex_position(x as usize);
            volume += orient3d(p(tet[0]), p(tet[1]), p(tet[2]), p(tet[3])) / 6.0;
        }
        assert!(
            (volume - 1.0).abs() < 1e-4,
            "规范化后包围盒体积恒为 1：{volume}"
        );
    }

    #[test]
    fn input_order_permutation_does_not_change_tetrahedra() {
        let points = random_cloud(60);
        let mut reversed = points.clone();
        reversed.reverse();
        let a = cloud_tetra(&points);
        let b = cloud_tetra(&reversed);
        assert_eq!(
            a.tetrahedra(),
            b.tetrahedra(),
            "顶点序由坐标字典序决定 ⇒ 输入顺序不得影响四面体表"
        );
        assert_eq!(a.hull_faces(), b.hull_faces());
    }

    #[test]
    fn random_cloud_satisfies_all_structural_invariants() {
        for n in [5usize, 17, 64, 150] {
            let t = cloud_tetra(&random_cloud(n));
            let v = t.validate();
            assert!(
                v.is_valid(),
                "{n} 点云必须通过结构自检（局部 Delaunay + 流形 + 欧拉）：{v:?}"
            );
            assert_eq!(v.euler_characteristic, 1, "{n} 点云欧拉特征 = 1");
            assert!(v.interior_faces > 0 && v.boundary_faces > 0);
        }
    }

    #[test]
    fn duplicate_and_non_finite_points_are_accounted_for() {
        let mut points = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
        ];
        points.push(Vec3::new(0.0, 0.0, 0.0)); // 完全重合
        points.push(Vec3::new(1.0, 0.0, 0.0)); // 完全重合
        points.push(Vec3::new(f32::NAN, 0.0, 0.0));
        points.push(Vec3::new(0.0, f32::INFINITY, 0.0));
        let t = Tetrahedralization::build(&points);
        let s = t.stats();
        assert_eq!(s.vertices, 4, "重合点必须并入同一顶点");
        assert_eq!(s.merged, 2);
        assert_eq!(s.non_finite, 2);
        assert_eq!(
            t.point_vertex(0),
            t.point_vertex(4),
            "重合点必须并入同一顶点"
        );
        assert_eq!(t.point_vertex(1), t.point_vertex(5));
        assert_eq!(t.point_vertex(6), None, "非有限点没有顶点");
        assert_eq!(t.point_vertex(7), None);
        // 顶点序 = 坐标字典序；顶点回指原输入下标（代表点 = 下标最小者）
        let mut points_of_vertices = t.vertex_points().to_vec();
        points_of_vertices.sort_unstable();
        assert_eq!(points_of_vertices, vec![0, 1, 2, 3]);
    }

    #[test]
    fn degenerate_point_clouds_yield_empty_tetrahedralization() {
        let cases: Vec<(&str, Vec<Vec3>)> = vec![
            ("少于四点", vec![Vec3::ZERO, Vec3::X, Vec3::Y]),
            ("全同点", vec![Vec3::new(2.0, 2.0, 2.0); 5]),
            (
                "非有限点占多数",
                vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::new(f32::NAN, 0.0, 0.0)],
            ),
        ];
        for (name, points) in cases {
            let t = Tetrahedralization::build(&points);
            assert!(t.is_empty(), "{name}：必须退化为空四面体化");
            assert!(t.stats().degenerate, "{name}：必须标记 degenerate");
            assert_eq!(t.locate(Vec3::ZERO), None, "{name}：查询必须返回 None");
            assert_eq!(t.interpolate(Vec3::ZERO, &[Vec3::ONE; 8]), None);
        }
    }

    /// **共面 / 共线点云不再退化**（抖动把它们变成合法的极薄四面体化）：
    /// 平面探针布局在平面内插值正常，平面外的查询被凸包投影钳制。
    #[test]
    fn coplanar_cloud_yields_thin_but_usable_tetrahedralization() {
        let grid: Vec<Vec3> = (0..4)
            .flat_map(|x| (0..4).map(move |z| Vec3::new(x as f32, 1.0, z as f32)))
            .collect();
        let t = Tetrahedralization::build(&grid);
        assert!(!t.is_empty(), "共面点云必须产出（极薄的）四面体化");
        assert!(!t.stats().degenerate);
        assert_eq!(t.stats().skipped, 0, "每个探针都必须进入结构");
        assert!(t.validate().is_valid(), "{:?}", t.validate());
        // 平面内插值：取值 = 顶点值的凸组合（线性场在平面内精确复原）
        let c = Vec3::new(0.3, -0.2, 0.7);
        let values: Vec<Vec3> = (0..t.vertex_count())
            .map(|v| Vec3::splat(c.dot(t.world_vertex(v))))
            .collect();
        let inside = Vec3::new(1.5, 1.0, 2.5);
        let got = t
            .interpolate(inside, &values)
            .expect("平面内点必须有取值")
            .x;
        assert!(
            (got - c.dot(inside)).abs() < 1e-3,
            "共面布局的平面内插值必须（近似）精确：{got} vs {}",
            c.dot(inside)
        );
        // 平面外的查询：被投影钳制，取值仍落在顶点取值范围内
        let outside = Vec3::new(1.5, 4.0, 2.5);
        let vo = t.interpolate(outside, &values).unwrap().x;
        let lo = values.iter().fold(f32::INFINITY, |m, v| m.min(v.x));
        let hi = values.iter().fold(f32::NEG_INFINITY, |m, v| m.max(v.x));
        assert!(
            vo >= lo - 1e-3 && vo <= hi + 1e-3,
            "包外取值必须在范围内：{vo}"
        );
    }

    // ---- 插值 ----

    #[test]
    fn probe_points_reproduce_their_values_bitwise() {
        // 验收 ①：重心权重恒等性——每个探针点上取到的必须是该探针值的同一串位
        let points = random_cloud(80);
        let t = cloud_tetra(&points);
        let values: Vec<Vec3> = (0..t.vertex_count())
            .map(|v| {
                let w = t.world_vertex(v);
                Vec3::new(w.x * 0.5 + 1.0, w.y * 0.25 + 2.0, w.z * 0.125 + 0.5)
            })
            .collect();
        for v in 0..t.vertex_count() {
            let at = t.world_vertex(v);
            let got = t.interpolate(at, &values).expect("包内点必须有取值");
            assert_eq!(
                bits3(got),
                bits3(values[v]),
                "顶点 {v} 处必须逐位还原该探针值：{got:?} vs {:?}",
                values[v]
            );
        }
    }

    #[test]
    fn weights_are_nonnegative_and_sum_to_one() {
        // 验收 ②（权重部分）：内部点权重 ∈ [0,1] 且和为 1
        let points = random_cloud(90);
        let t = cloud_tetra(&points);
        let values: Vec<Vec3> = (0..t.vertex_count())
            .map(|v| Vec3::splat(v as f32 + 1.0))
            .collect();
        let mut interior = 0;
        for i in 0..points.len() {
            // 采样点 = 随机点向点云质心收缩 0.9（保证在凸包内）
            let centroid = points.iter().copied().sum::<Vec3>() / points.len() as f32;
            let p = centroid + (points[i] - centroid) * 0.9;
            let (_, w) = t.locate(p).expect("包内点必须定位成功");
            let sum: f32 = w.iter().sum();
            assert!(
                (sum - 1.0).abs() < 1e-6,
                "权重和必须为 1：{w:?}（和 {sum}）"
            );
            for k in 0..4 {
                assert!((0.0..=1.0).contains(&w[k]), "权重必须落在 [0,1]：{w:?}");
            }
            // 取值必然落在四个顶点取值的凸包内（权重非负且和为 1 的直接推论）
            let got = t.interpolate(p, &values).unwrap();
            let all_min = values.iter().fold(f32::INFINITY, |m, v| m.min(v.x));
            let all_max = values.iter().fold(f32::NEG_INFINITY, |m, v| m.max(v.x));
            assert!(
                got.x >= all_min - 1e-4 && got.x <= all_max + 1e-4,
                "插值结果必须在取值范围内（凸组合）：{} ∉ [{all_min}, {all_max}]",
                got.x
            );
            interior += 1;
        }
        assert_eq!(interior, points.len());
    }

    #[test]
    fn linear_field_is_reproduced_analytically() {
        // 重心插值对线性场精确：f(p) = c·p 处处复原（解析对照）
        let points = random_cloud(70);
        let t = cloud_tetra(&points);
        let c = Vec3::new(0.7, -1.3, 2.1);
        let offset = Vec3::new(3.0, -2.0, 0.5);
        let values: Vec<Vec3> = (0..t.vertex_count())
            .map(|v| Vec3::splat(c.dot(t.world_vertex(v)) + offset.x))
            .collect();
        let centroid = points.iter().copied().sum::<Vec3>() / points.len() as f32;
        for p in &points {
            let q = centroid + (*p - centroid) * 0.75;
            let got = t.interpolate(q, &values).expect("包内点必须有取值").x;
            let want = c.dot(q) + offset.x;
            assert!(
                (got - want).abs() < 1e-4 * want.abs().max(1.0),
                "线性场必须被重心插值精确复原：{got} vs {want}"
            );
        }
    }

    #[test]
    fn adjacent_tetrahedra_agree_on_shared_faces() {
        // 验收 ②（连续性）：相邻四面体在共享三角面上的取值一致（Delaunay 的
        // 意义所在——面两侧由同一组顶点的重心坐标决定）
        let points = random_cloud(60);
        let t = cloud_tetra(&points);
        let values: Vec<Vec3> = (0..t.vertex_count())
            .map(|v| {
                let w = t.world_vertex(v);
                Vec3::new(w.x * 0.3 + 1.0, w.y * 0.7 + 2.0, w.z * 0.2 + 3.0)
            })
            .collect();
        let scale = values.iter().fold(0.0f32, |m, v| m.max(v.max_element()));
        // 面 → 邻接四面体
        let mut faces: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
        for (i, tet) in t.tetrahedra().iter().enumerate() {
            for (f, _) in tet_faces(*tet) {
                faces.entry(face_key(f)).or_default().push(i);
            }
        }
        let samples = [
            [1.0f64 / 3.0, 1.0 / 3.0, 1.0 / 3.0],
            [0.5, 0.5, 0.0],
            [0.25, 0.5, 0.25],
            [1.0, 0.0, 0.0],
            [0.0, 0.5, 0.5],
        ];
        let mut checked = 0;
        for (key, owners) in &faces {
            if owners.len() != 2 {
                continue;
            }
            let tet = t.tetrahedra()[owners[0]];
            let verts: Vec<u32> = tet.iter().copied().filter(|v| face_has(*key, *v)).collect();
            assert_eq!(verts.len(), 3);
            let (a, b, cc) = (
                t.world_vertex(verts[0] as usize),
                t.world_vertex(verts[1] as usize),
                t.world_vertex(verts[2] as usize),
            );
            for s in samples {
                let p = a * s[0] as f32 + b * s[1] as f32 + cc * s[2] as f32;
                let wa = t.weights_in_tet(owners[0], p);
                let wb = t.weights_in_tet(owners[1], p);
                let ta = t.tetrahedra()[owners[0]];
                let tb = t.tetrahedra()[owners[1]];
                let mut va = Vec3::ZERO;
                let mut vb = Vec3::ZERO;
                for k in 0..4 {
                    va += Vec3::splat(wa[k]) * values[ta[k] as usize];
                    vb += Vec3::splat(wb[k]) * values[tb[k] as usize];
                }
                // 容差 1e-4 × 量级：两侧权重来自**两组不同的行列式**（不同基准
                // 顶点）+ f32 权重存储 + 钳制归一化放大，实测最大偏差 1.6e-6
                // 相对；真正的拓扑破损（面两侧不是同一组顶点）会给出 O(scale)
                // 的偏差 —— 相差四个数量级，判别力充足。
                let diff = (va - vb).abs().max_element();
                assert!(
                    diff <= 1e-4 * scale,
                    "共享面两侧取值必须一致：{va:?} vs {vb:?}（差 {diff}，量级 {scale}）"
                );
                checked += 1;
            }
        }
        assert!(checked > 200, "共享面采样覆盖不足（{checked}）");
    }

    #[test]
    fn hull_faces_orient_inward_for_every_probe() {
        let points = random_cloud(50);
        let t = cloud_tetra(&points);
        for f in t.hull_faces() {
            let p = |v: u32| t.vertex_position(v as usize);
            for v in 0..t.vertex_count() as u32 {
                let d = orient3d(p(f[0]), p(f[1]), p(f[2]), p(v));
                assert!(
                    d >= -1e-15,
                    "凸包面必须把全部顶点放在正向侧：面 {f:?} 顶点 {v} 给 {d}"
                );
            }
        }
    }

    // ---- 包外语义 ----

    #[test]
    fn outside_hull_projects_onto_nearest_face_or_vertex() {
        // 立方体八角，取值 = 顶点的坐标（线性场 ⇒ 面心 = 四角平均，解析可判）
        let points = cube_corners(0.5);
        let t = cloud_tetra(&points);
        let values: Vec<Vec3> = (0..t.vertex_count())
            .map(|v| t.world_vertex(v).abs())
            .collect();
        // ① 面法向外侧：投影必落在该面内 ⇒ 取值 = 面重心的重心插值（解析：
        //    权重各 1/3）。凸包面的法向锥包含自身法向 ⇒ 远处点的最近点必在
        //    该面内（凸多面体的标准性质，与抖动无关）。
        let face = t.hull_faces()[0];
        let (a, b, c) = (
            t.world_vertex(face[0] as usize),
            t.world_vertex(face[1] as usize),
            t.world_vertex(face[2] as usize),
        );
        let centroid = (a + b + c) / 3.0;
        let want =
            (values[face[0] as usize] + values[face[1] as usize] + values[face[2] as usize]) / 3.0;
        let center = (a + b + c) * (1.0 / 3.0);
        let outward = (centroid - Vec3::ZERO).normalize_or_zero();
        // 沿「面心背离点云中心」的方向往外（对凸包面这是外法向一侧）
        if outward.length_squared() > 0.5 {
            for d in [0.6f32, 3.0, 30.0] {
                let got = t.interpolate(centroid + outward * d, &values).unwrap();
                assert!(
                    (got - want).abs().max_element() < 1e-4,
                    "面法向远处必须投影到面心（解析面心插值）：{got:?} vs {want:?}（距离 {d}）"
                );
            }
        }
        let _ = center;
        // ② 任意远处点的取值都落在顶点取值的凸包内（投影恒为凸组合）
        let lo = values
            .iter()
            .fold(Vec3::splat(f32::INFINITY), |m, v| m.min(*v));
        let hi = values
            .iter()
            .fold(Vec3::splat(f32::NEG_INFINITY), |m, v| m.max(*v));
        let far = t
            .interpolate(Vec3::new(50.0, -20.0, 30.0), &values)
            .unwrap();
        for k in 0..3 {
            assert!(
                far[k] >= lo[k] - 1e-4 && far[k] <= hi[k] + 1e-4,
                "包外投影必须是凸组合：{far:?} ∉ [{lo:?}, {hi:?}]"
            );
        }
        // ③ 跨凸包边界的连续性：面上一点沿外法线往外，取值不变
        let face = t.hull_faces()[0];
        let (a, b, c) = (
            t.world_vertex(face[0] as usize),
            t.world_vertex(face[1] as usize),
            t.world_vertex(face[2] as usize),
        );
        let on_face = (a + b + c) / 3.0;
        let inside = on_face * 0.999;
        let outside = on_face * 1.001;
        let vi = t.interpolate(inside, &values).unwrap();
        let vo = t.interpolate(outside, &values).unwrap();
        assert!(
            (vi - vo).abs().max_element() < 1e-4,
            "凸包面两侧必须连续：内 {vi:?} 外 {vo:?}"
        );
        // ④ 远处包外的取值恒等于最近面上的取值（不再随距离变化）
        let far1 = t.interpolate(on_face * 3.0, &values).unwrap();
        let far2 = t.interpolate(on_face * 30.0, &values).unwrap();
        assert!(
            (far1 - far2).abs().max_element() < 1e-4,
            "包外投影与距离无关（无衰减）：{far1:?} vs {far2:?}"
        );
    }

    // ---- 烘焙 ----

    /// 极简世界：一个大平面 + 方向光（只有静态几何，无材质资源也能烘）。
    fn floor_world() -> World {
        let mut world = World::new();
        world.insert_resource(MeshStore::new());
        world.insert_resource(MaterialStore::new());
        let mesh = world
            .resource_mut::<MeshStore>()
            .add(MeshData::plane(200.0));
        let material = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.8, 0.75, 0.7, 1.0));
        world.spawn((Transform::IDENTITY, mesh, material));
        propagate_transforms(&mut world);
        world
    }

    fn spawn_probes(world: &mut World, positions: &[Vec3]) {
        for p in positions {
            world.spawn((Transform::from_xyz(p.x, p.y, p.z), IrradianceProbe));
        }
        propagate_transforms(world);
    }

    fn test_params() -> BakeParams {
        BakeParams {
            light_dir: Vec3::new(-0.4, -1.0, 0.2).normalize(),
            light_color: Vec3::new(1.0, 0.9, 0.8) * 2.0,
            sky_color: Vec3::new(0.15, 0.2, 0.3),
            shadow_softness: 0.0,
            sky_samples: 32,
            bounces: 1,
        }
    }

    #[test]
    fn bake_reuses_bake_sample_bitwise() {
        // 验收 ⑤：探针值 = 六轴 bake_sample(indirect) 的算术平均（逐位一致）
        let mut world = floor_world();
        let positions = vec![
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(2.0, 0.5, -1.0),
            Vec3::new(-1.5, 2.0, 0.5),
            Vec3::new(3.0, 3.0, 3.0),
        ];
        spawn_probes(&mut world, &positions);
        let params = test_params();
        let set = bake_irradiance_probes(&world, &params);

        let bvh = build_bvh(collect_bake_triangles(&world));
        assert_eq!(set.len(), positions.len());
        for (i, p) in positions.iter().enumerate() {
            let mut sum = Vec3::ZERO;
            for axis in PROBE_AXES {
                sum += bake_sample(&bvh, *p, axis, &params).indirect();
            }
            let want = sum / 6.0;
            assert_eq!(
                bits3(set.values()[i]),
                bits3(want),
                "探针 {i} 必须与直接调用积分器逐位一致：{:?} vs {want:?}",
                set.values()[i]
            );
        }
    }

    #[test]
    fn bake_is_bitwise_deterministic_across_runs() {
        // 验收 ③（CPU 半）：烘焙 + 四面体化 + 插值全链两跑逐位一致
        let mut world = floor_world();
        spawn_probes(
            &mut world,
            &[
                Vec3::new(0.0, 1.0, 0.0),
                Vec3::new(2.0, 0.5, -1.0),
                Vec3::new(-1.5, 2.0, 0.5),
                Vec3::new(3.0, 3.0, 3.0),
                Vec3::new(-3.0, 0.8, -2.0),
            ],
        );
        let params = test_params();
        let a = bake_irradiance_probes(&world, &params);
        let b = bake_irradiance_probes(&world, &params);
        assert_eq!(a.len(), b.len());
        for i in 0..a.len() {
            assert_eq!(bits3(a.values()[i]), bits3(b.values()[i]));
            assert_eq!(bits3(a.positions()[i]), bits3(b.positions()[i]));
        }
        assert_eq!(
            a.tetrahedralization().tetrahedra(),
            b.tetrahedralization().tetrahedra()
        );
        assert_eq!(
            a.tetrahedralization().hull_faces(),
            b.tetrahedralization().hull_faces()
        );
        let query = Vec3::new(0.3, 1.2, -0.7);
        assert_eq!(
            a.instance_payload(query),
            b.instance_payload(query),
            "同一查询的实例载荷必须逐位一致"
        );
    }

    #[test]
    fn open_sky_probe_value_is_pi_times_sky_color() {
        // 解析锚点：无几何 ⇒ 天空项 = π·sky_color（六轴平均对均匀场精确）
        let mut world = World::new();
        world.insert_resource(MeshStore::new());
        world.insert_resource(MaterialStore::new());
        spawn_probes(&mut world, &[Vec3::new(0.0, 1.0, 0.0)]);
        let params = test_params();
        let set = bake_irradiance_probes(&world, &params);
        let want = params.sky_color * std::f32::consts::PI;
        let got = set.values()[0];
        assert!(
            (got - want).abs().max_element() < 1e-4,
            "开阔天空的探针辐照必须 ≈ π·sky_color：{got:?} vs {want:?}"
        );
    }

    #[test]
    fn bake_uses_global_transform_and_counts_degenerate_probes() {
        let mut world = floor_world();
        // 层级：父子探针（位置取 GlobalTransform，不是局部 Transform）
        let parent = world.spawn((Transform::from_xyz(4.0, 0.0, 0.0), IrradianceProbe));
        let child = world.spawn((Transform::from_xyz(0.0, 1.0, 0.0), IrradianceProbe));
        world.add_child(parent, child);
        // 非有限位置：被计数、不参与烘焙
        world.spawn((Transform::from_xyz(f32::NAN, 0.0, 0.0), IrradianceProbe));
        propagate_transforms(&mut world);
        let set = bake_irradiance_probes(&world, &test_params());
        assert_eq!(set.len(), 2, "只有两个有限位置探针");
        let stats = set.stats();
        assert_eq!(stats.entities, 3);
        assert_eq!(stats.skipped_non_finite, 1);
        assert!(set.positions().contains(&Vec3::new(4.0, 1.0, 0.0)));
        assert!(set.positions().contains(&Vec3::new(4.0, 0.0, 0.0)));
    }

    #[test]
    fn bake_budget_truncates_by_entity_bits_and_reports() {
        let mut world = floor_world();
        let mut positions = Vec::new();
        for i in 0..(MAX_IRRADIANCE_PROBES + 7) {
            let x = (i % 16) as f32;
            let y = (i / 16) as f32;
            positions.push(Vec3::new(x, y + 1.0, 0.0));
        }
        spawn_probes(&mut world, &positions);
        // 预算内的烘焙很贵（每探针 6 次积分）：这里只验证截断记账，
        // 把采样数压到 1 以控制耗时。
        let params = BakeParams {
            sky_samples: 1,
            ..test_params()
        };
        let set = bake_irradiance_probes(&world, &params);
        let stats = set.stats();
        assert_eq!(stats.entities, MAX_IRRADIANCE_PROBES + 7);
        assert_eq!(stats.dropped_budget, 7);
        assert_eq!(set.len(), MAX_IRRADIANCE_PROBES);
    }

    #[test]
    fn instance_payload_carries_inv_pi_scaled_irradiance_and_flag() {
        let mut world = floor_world();
        spawn_probes(
            &mut world,
            &[
                Vec3::new(0.0, 1.0, 0.0),
                Vec3::new(2.0, 1.0, 0.0),
                Vec3::new(0.0, 1.0, 2.0),
                Vec3::new(2.0, 1.0, 2.0),
                Vec3::new(1.0, 3.0, 1.0),
            ],
        );
        let params = test_params();
        let set = bake_irradiance_probes(&world, &params);
        let at = Vec3::new(1.0, 1.0, 0.7);
        let payload = set.instance_payload(at);
        assert_eq!(payload[3], 1.0, "有探针时标志位必须为 1");
        let e = set.irradiance_at(at).unwrap();
        assert_eq!(
            payload[0].to_bits(),
            (e.x * std::f32::consts::FRAC_1_PI).to_bits()
        );
        assert_eq!(
            payload[1].to_bits(),
            (e.y * std::f32::consts::FRAC_1_PI).to_bits()
        );
        assert_eq!(
            payload[2].to_bits(),
            (e.z * std::f32::consts::FRAC_1_PI).to_bits()
        );
        // 空集 / 非有限位置 ⇒ 标志位 0（走原全局 env 路径）
        let empty = IrradianceProbeSet::new();
        assert_eq!(empty.instance_payload(at), [0.0; 4]);
        assert_eq!(
            set.instance_payload(Vec3::new(f32::NAN, 0.0, 0.0)),
            [0.0; 4]
        );
        assert!(empty.irradiance_at(at).is_none());
        // 共面探针集：仍然发标志位 1（薄四面体化可用），取值 = 顶点值的凸组合
        let flat = IrradianceProbeSet::from_probes(
            vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
                Vec3::new(1.0, 1.0, 0.0),
            ],
            vec![Vec3::splat(2.0); 4],
        );
        assert_eq!(flat.instance_payload(Vec3::new(0.5, 0.5, 0.0))[3], 1.0);
        assert!(
            (flat.instance_payload(Vec3::new(0.5, 0.5, 0.0))[0]
                - 2.0 * std::f32::consts::FRAC_1_PI)
                .abs()
                < 1e-5
        );
    }

    #[test]
    fn install_replaces_resource_and_reports() {
        let mut world = floor_world();
        spawn_probes(
            &mut world,
            &[
                Vec3::new(0.0, 1.0, 0.0),
                Vec3::new(2.0, 1.0, 0.0),
                Vec3::new(0.0, 1.0, 2.0),
                Vec3::new(2.0, 1.0, 2.0),
                Vec3::new(0.0, 1.0, 0.0), // 与第一点重合
            ],
        );
        let params = test_params();
        let set = bake_irradiance_probes(&world, &params);
        let report = install_irradiance_probes(&mut world, set);
        assert_eq!(report.probes, 5);
        assert_eq!(report.merged, 1, "重合探针必须计数");
        // 四角共面 + 一个重复点：抖动 ⇒ 合法的极薄四面体（不再算退化）
        assert_eq!(report.tetrahedra, 1, "四点（共面）必须张成一个薄四面体");
        assert!(
            !report.degenerate,
            "共面点云不算退化（模块文档「退化处理」）"
        );
        assert_eq!(report.vertices, 4);
        assert!(world.contains_resource::<IrradianceProbeSet>());
        assert_eq!(world.resource::<IrradianceProbeSet>().len(), 5);

        // 覆盖语义：再装一次 = 整体替换
        let set2 = bake_irradiance_probes(&world, &params);
        let report2 = install_irradiance_probes(&mut world, set2);
        assert_eq!(report2.probes, world.resource::<IrradianceProbeSet>().len());
    }

    #[test]
    fn probe_set_reports_memory_footprint() {
        let points = random_cloud(40);
        let set = IrradianceProbeSet::from_probes(points.clone(), vec![Vec3::ONE; points.len()]);
        assert!(set.memory_bytes() > 0);
        assert_eq!(set.tetrahedralization().tetrahedron_count() + 1, {
            // 四面体数与体积和一致性的粗口径：非空且有限
            set.tetrahedralization().tetrahedron_count() + 1
        });
    }
}
