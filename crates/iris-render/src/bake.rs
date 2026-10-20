//! 烘焙光照（R52a：三阶段之一）——CPU 光线跟踪核心 + 辐照度烘焙器。
//!
//! # 三阶段规划（a / b / c 已全部落地）
//!
//! | 阶段 | 内容 | 状态 |
//! |---|---|---|
//! | **a** | 跟踪器（BVH + 三角求交）、无 RNG 低差异采样、辐照度积分器；输出接口 = **世界空间采样点列表**（[`bake_points`]） | 本文件（R52a） |
//! | **b** | UV2 参数化（主轴投影 chart + shelf 图集）+ 采样点 ↔ texel 布局（把「采样点列表」换成「texel 网格」）+ 全链组装 [`bake_lightmap`] 与 PPM 预览导出 | [`bake_uv`](crate::bake_uv)（R52b） |
//! | c | 烘焙结果上传纹理（Rgba16Float）+ 着色器消费（间接项 `select` 替换 IBL 环境项）+ `MeshData` UV2 顶点属性与逐实体注入 | [`lightmap`](crate::lightmap)（R52c） |
//!
//! a 阶段刻意不做 texel 网格化：正确性（解析对照）、性能（是否必须并行）、
//! 确定性（逐位复现）三个风险全部集中在跟踪器与积分器上，而这两块与 GPU /
//! 资产管线零耦合——先把口径钉死，b/c 只是把它接到纹理上。
//!
//! # 跟踪器
//!
//! - [`BakeTriangle`]：世界空间三角形（三顶点 + 法线 + 反照率 + 来源实体）；
//! - [`build_bvh`]：中值分割 BVH。**构建确定性**：分割轴 = 质心包围盒最长轴
//!   （并列取低轴号），子区间按 `(质心沿轴分量的 total_cmp, 原三角形序号)`
//!   全序排序——全序 ⇒ 同一输入数组的两次构建产生逐位相同的节点数组与
//!   三角形重排顺序（**构建序即结果的一部分**，见 [`Bvh`] 文档）；
//! - [`raycast_bvh`]：栈式遍历（近子先出栈）+ AABB slab 前进测试剪枝 +
//!   [`ray_triangle_intersection`]（Möller–Trumbore）。**并列处理**：
//!   `t` 逐位相等时取**重排后内部索引小者**（严格 `<` 才替换、等值再比索引），
//!   而内部索引由上面的确定性构建序唯一决定 ⇒ 命中结果不依赖遍历次序。
//!
//! 求交是**双面**的（正反面都算命中——烘焙必须能回答「薄板背面看到什么」）；
//! 命中携带的 `normal` 是存储的表面法线，积分器算反弹时把法线翻向射线来向
//! （见 [`bake_sample`] 里的 `m`）。
//!
//! # 采样（无 RNG，确定性文化见 AGENTS.md）
//!
//! [`radical_inverse_2`] = 基 2 低差异序列（纯位运算，无 `rand`、无全局状态、
//! 与调用次序无关）；[`cosine_hemisphere`] / [`uniform_cone`] 都用 Hammersley
//! 点集 `(u1 = 根数反序, u2 = (i + 0.5)/n)` 生成第 `i` 个方向：
//!
//! - 余弦加权半球（Malley 投影：单位圆盘均匀点投到半球 ⇒ pdf = `cosθ/π`），
//!   `u1` 给方位角、`u2` 给圆盘半径 ⇒ `u2` 均匀即 cos²θ 均匀，极角严格分层；
//! - 球冠均匀（cosθ 在 `[cosθ_max, 1]` 线性分层）——软阴影锥。
//!
//! 两者都是「同 `(index, count, 轴向)` 输入 ⇒ 逐位同输出」的纯函数。单测用
//! **解析矩**验证分布真实生效：余弦加权半球 E[cosθ] = 2/3（σ_mean =
//! sqrt((1/2 − 4/9)/n)）、球冠 E[cosθ] = (1 + cosθ_max)/2（σ_mean =
//! sqrt((cosθ_max − 1)²/12n)），容差各取 ≈3σ。
//!
//! # 积分器（[`bake_sample`]）
//!
//! 辐照度 = 三项之和，逐采样点输出（[`BakeSample`]）：
//!
//! 1. **直接光**：`max(N·L, 0) × light_color`，遮挡判定 = 1 条（硬影）或
//!    [`SHADOW_SOFT_SAMPLES`] 条软影锥方向的**未遮挡比例**（= 半影亮度系数）。
//!    锥只用于可见性、余弦项仍取光心方向 ⇒ 全亮区的软影值与硬影**逐位相同**
//!    （见 `direct_irradiance` 文档），半影带则是 `k/8 × 全亮值` 的离散中间值；
//! 2. **天空 / AO**：余弦加权半球射线，命中即计遮蔽。pdf = `cosθ/π` 的无偏
//!    估计量是 `(π/n)Σ L(ωᵢ)` ⇒ **开阔均匀天空的解析值恰为 `π × sky_color`**
//!    且与法线朝向无关（本阶段最干净的一条解析对照），AO = 被挡样本比例；
//! 3. **一次反弹**（即时辐射度）：命中点出射辐射取 `albedo/π × (其直接光 +
//!    π × sky_color)`，代回 2 的估计量后 π 约掉 ⇒ 每样本贡献
//!    `albedo × (直接光 + π × sky) / n`——色溢（红地板染白墙）的最小验证，
//!    其解析值 = `albedo ⊙ (E_direct + π·E_sky)`。
//!
//! # v1 限制（诚实清单）
//!
//! - **只支持 0 / 1 次反弹**（`bounces > 1` 在 [`bake_sample`] 断言）；
//! - 反弹命中点的**二次阴影走硬影单射线**（软影锥只在第一级展开：每级再乘
//!   8 条射线对 CPU 烘焙不值，见 `benches/render_cpu.rs` 的基准数字）；
//! - 命中点的天空项用**开阔近似**（不再递归采样该点半球）⇒ 深凹角的反弹
//!   偏亮；
//! - 无面光 / 点光（[`PointLight`](crate::light::PointLight) 不参与）、无
//!   emissive 材质、天空是单色常量（不做
//!   [`SkySource`](crate::environment::SkySource) 卷积）；
//! - 半透明材质按**不透明**处理（玻璃照样遮光）；
//! - 蒙皮实体整体排除（姿态每帧由 [`Skin`] 驱动，绑定姿态烘焙没有意义）；
//!   只认 [`MeshHandle`] ⇒ 挂 [`LodMesh`](crate::mesh::LodMesh) /
//!   [`LodMesh3`](crate::terrain::LodMesh3) 的几何（R51 地形 chunk）
//!   **暂不参与烘焙**，「取最高细节级」接入推迟（b 阶段规范未含，见
//!   ARCHITECTURE §4.24）；
//! - 单线程。实测（release，i7-13620H）：1000 三角的 BVH 构建 217 µs、
//!   1000 采样点 × 64 sky 样本 + 1 反弹的积分 34.3 ms
//!   （`benches/render_cpu.rs` 的 `bake_bvh_build_1k_tris` /
//!   `bake_irradiance_1k_points`，烟雾报警见 `tests/budgets.rs`）。b/c 阶段
//!   若因 texel 数量放大而越界，并行点唯一且平凡：切 `bake_points` 的
//!   `points` 分段、按序拼接（各点结果互不依赖 ⇒ 确定性不破）。

use glam::{Mat3, Vec2, Vec3};
use iris_core::prelude::{BodyType, Collider, Entity, GlobalTransform, RigidBody};
use iris_core::world::World;

use crate::material::{MaterialHandle, MaterialStore};
use crate::mesh::{MeshHandle, MeshStore};
use crate::skin::Skin;
use crate::visibility::Visibility;

/// 烘焙排除标记（R89）：挂了本组件的实体，其几何**不进入**
/// [`collect_bake_triangles`] 的三角收集——该收集是光照贴图烘焙 / 辐照
/// 探针 / DDGI **三个消费者共用**的世界封装（BVH 挡光 + `bounds = None`
/// 时的 AABB 派生都吃它），排除语义三处同时生效。
///
/// **缺席语义（API 的一部分）= 收录**：不带本组件的实体与 R52a 以来逐位
/// 一致 ⇒ 既有场景的全部烘焙 / 探针 / DDGI 输出零变化。
///
/// 典型用户是**编辑器叠加层**（网格线 / 选中框 / Nav blocked 红片 /
/// DDGI 探针标记球）：它们是普通渲染实体（走主 pass），但正铺在被烘焙 /
/// 被探针照明的空间里——不排除会自我污染 GI（blocked 红片挡光）并撑大
/// `bounds = None` 的自动派生范围。这是显式**声明式**排除：不做几何类型
/// 自动判定（透明 / 粒子 / 贴花仍进收集，与 R52a 同立场，§8.58⑨）。
///
/// 形态与 [`IrradianceProbe`](crate::irradiance::IrradianceProbe) 同款
/// marker：单元结构体、无字段、`EditorTransient` 之类的编辑器标记可以
/// 与它共存。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, iris_core::Component)]
pub struct BakeExcluded;

/// BVH 叶内最大三角形数（中值分割到该规模即停）。4 是摊销甜点：再小则节点
/// 数组与遍历栈占比上升，再大则剪枝收益下降。
pub const BVH_LEAF_SIZE: usize = 4;

/// 遍历栈槽数。栈深上界 = 树高（中值分割恒平衡，每层至多留一个待访问兄弟），
/// 64 层覆盖 2^64 量级三角形数——不可能触及；越界即索引 panic（真 bug 信号）。
const BVH_STACK_DEPTH: usize = 64;

/// 退化三角判据：`|det|` 不超过此值视为零面积或射线与平面平行。`det` 按两条
/// 边长乘积缩放，故只有亚毫米级三角才可能误触。
const DEGENERATE_DETERMINANT: f32 = 1e-12;

/// 自交保护：`t <= BAKE_RAY_MIN_T` 的命中一律忽略（射线原点贴在几何表面上时
/// 不算命中它自己）。
pub const BAKE_RAY_MIN_T: f32 = 1e-4;

/// 阴影 / 半球射线的原点沿法线偏置（米）。与 [`BAKE_RAY_MIN_T`] 配对：偏置把
/// 原点抬离表面，`t` 下限吃掉偏置的浮点残差。太小会漏光（自交条纹），太大会
/// 在薄几何上漏影——2 mm 对应本引擎角色 / 建筑尺度。
///
/// **对闭区间外的采样点有语义影响**：偏置沿法线方向，故「面朝墙面、贴在墙上」
/// 的点会被推到墙的另一侧（单面薄板没有厚度时必然如此）。测试与 b 阶段的
/// 采样点都应落在表面上方，而不是正好坐在面上。
pub const BAKE_SHADOW_BIAS: f32 = 0.002;

/// 软阴影锥每点的方向样本数。规范未在 [`BakeParams`] 里给独立字段，故取模块
/// 常量：软影宽度由 [`BakeParams::shadow_softness`] 决定，样本数只影响噪声。
pub const SHADOW_SOFT_SAMPLES: u32 = 8;

/// 烘焙几何的最小单元：世界空间三角形 + 表面属性。
///
/// `normal` 在 [`build_bvh`] 里归一化后被 [`BvhHit::normal`] 直接返回；
/// `albedo` 是直接光与天空的乘子、也是反弹携带的颜色（没有它就没有色溢）。
/// `entity` 只做来源标记（结果回溯到实体），不参与求交。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BakeTriangle {
    /// 世界空间三顶点。
    pub vertices: [Vec3; 3],
    /// 表面法线（双面求交，见模块文档）。
    pub normal: Vec3,
    /// 漫反射反照率（线性空间 RGB，惯常 0..1，不钳制）。
    pub albedo: Vec3,
    /// 来源实体（悬垂 `Entity` 合法——仅作标记）。
    pub entity: Entity,
}

impl BakeTriangle {
    /// 质心（BVH 分割键）。
    fn centroid(&self) -> Vec3 {
        (self.vertices[0] + self.vertices[1] + self.vertices[2]) * (1.0 / 3.0)
    }

    /// 零面积三角（两边叉积为 0）。
    fn is_degenerate(&self) -> bool {
        let e1 = self.vertices[1] - self.vertices[0];
        let e2 = self.vertices[2] - self.vertices[0];
        e1.cross(e2).length_squared() <= f32::EPSILON
    }
}

/// BVH 节点：包围盒 + （内部）左右子下标 / （叶）三角区间。
#[derive(Clone, Copy, Debug)]
struct BvhNode {
    min: Vec3,
    max: Vec3,
    /// 叶节点为 [`NO_CHILD`]。
    left: u32,
    right: u32,
    /// 叶：`triangles` 起始下标。
    start: u32,
    /// 叶：三角数；内部节点为 0。
    count: u32,
}

const NO_CHILD: u32 = u32::MAX;

impl BvhNode {
    fn is_leaf(&self) -> bool {
        self.left == NO_CHILD
    }
}

/// 静态场景的三角加速结构（射线查询）。
///
/// **构建序是结果的一部分**：`triangles` 按 BVH 叶区间重排过，
/// [`BvhHit::triangle`] 是重排后的下标。同输入数组两次 [`build_bvh`] 逐位
/// 一致（单测锁定），于是「射线并列命中」与「遍历次序」都不引入不确定性。
/// 回指来源用 [`Bvh::triangle`]（里面有 `entity`）。
#[derive(Clone, Debug, Default)]
pub struct Bvh {
    triangles: Vec<BakeTriangle>,
    nodes: Vec<BvhNode>,
}

impl Bvh {
    /// 空场景（无三角）：所有射线查询返回 `None`。
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// 三角形数（= 构建输入数）。
    pub fn triangle_count(&self) -> usize {
        self.triangles.len()
    }

    /// 节点数——用来钉住分割策略的测试断言，也是烘焙前的规模观测口径。
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// 重排后第 `index` 个三角（`index` 取自 [`BvhHit::triangle`]）。
    pub fn triangle(&self, index: usize) -> &BakeTriangle {
        &self.triangles[index]
    }
}

/// 射线命中结果。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BvhHit {
    /// 沿**单位**方向的距离（米）。
    pub t: f32,
    /// 命中三角的表面法线（存储值，未翻向射线来向，见模块文档）。
    pub normal: Vec3,
    /// `Bvh` 内部（重排后）三角下标。
    pub triangle: usize,
    /// 重心坐标 `(u, v)` = 顶点 1 / 2 的权重（顶点 0 的权重 = `1 − u − v`）。
    /// 命中点恒可由 `origin + dir·t` 复原（单测双向核对）；带出重心是为了
    /// b/c 阶段能插值任意顶点属性。
    pub barycentric: Vec2,
}

/// 由三角列表构建 BVH（消费输入并**重排**其顺序，确定性论证见 [`Bvh`]）。
///
/// 断言：顶点 / 法线 / 反照率有限，法线非零，三角不退化。退化三角在真实
/// 网格里 = 上游数据 bug（`primitives` 与 GLB 导入都不产出），静默丢弃会
/// 变成「射线穿过本该挡住它的面」这种无法解释的症状，故直接拒绝。
pub fn build_bvh(triangles: Vec<BakeTriangle>) -> Bvh {
    for (index, tri) in triangles.iter().enumerate() {
        assert!(
            tri.vertices.iter().all(|v| v.is_finite())
                && tri.normal.is_finite()
                && tri.normal.length_squared() > 0.0
                && tri.albedo.is_finite(),
            "烘焙三角 #{index} 数据非法：{tri:?}"
        );
        assert!(
            !tri.is_degenerate(),
            "烘焙三角 #{index} 退化（零面积），来源实体 {:?}",
            tri.entity
        );
    }
    if triangles.is_empty() {
        return Bvh::empty();
    }
    let count = triangles.len();
    let mut builder = BvhBuilder {
        centroids: triangles.iter().map(BakeTriangle::centroid).collect(),
        triangle_aabbs: aabbs_of(&triangles),
        order: (0..count as u32).collect(),
        nodes: Vec::with_capacity(count * 2),
    };
    builder.build_range(0, count);
    let reordered: Vec<BakeTriangle> = builder
        .order
        .iter()
        .map(|&i| BakeTriangle {
            normal: triangles[i as usize].normal.normalize(),
            ..triangles[i as usize]
        })
        .collect();
    Bvh {
        triangles: reordered,
        nodes: builder.nodes,
    }
}

/// 逐三角 AABB（构建期包围盒并集的基础，避免重复求 min/max）。
fn aabbs_of(triangles: &[BakeTriangle]) -> Vec<(Vec3, Vec3)> {
    triangles
        .iter()
        .map(|tri| {
            let mut min = tri.vertices[0];
            let mut max = tri.vertices[0];
            for v in &tri.vertices[1..] {
                min = min.min(*v);
                max = max.max(*v);
            }
            (min, max)
        })
        .collect()
}

/// BVH 递归构建器（中值分割 + 全序比较 ⇒ 确定性）。
struct BvhBuilder {
    centroids: Vec<Vec3>,
    triangle_aabbs: Vec<(Vec3, Vec3)>,
    /// 三角下标的当前排列（构建完即 `Bvh::triangles` 的顺序）。
    order: Vec<u32>,
    nodes: Vec<BvhNode>,
}

impl BvhBuilder {
    /// 把 `order[start..end]` 建成一棵子树，返回其根节点下标。
    fn build_range(&mut self, start: usize, end: usize) -> u32 {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for &i in &self.order[start..end] {
            let (lo, hi) = self.triangle_aabbs[i as usize];
            min = min.min(lo);
            max = max.max(hi);
        }
        let index = self.nodes.len() as u32;
        self.nodes.push(BvhNode {
            min,
            max,
            left: NO_CHILD,
            right: NO_CHILD,
            start: 0,
            count: 0,
        });
        if end - start <= BVH_LEAF_SIZE {
            self.nodes[index as usize].start = start as u32;
            self.nodes[index as usize].count = (end - start) as u32;
            return index;
        }
        let axis = self.split_axis(start, end);
        self.sort_range(start, end, axis);
        let mid = (start + end) / 2;
        let left = self.build_range(start, mid);
        let right = self.build_range(mid, end);
        let node = &mut self.nodes[index as usize];
        node.left = left;
        node.right = right;
        index
    }

    /// 分割轴 = 质心包围盒最长轴（并列取低轴号——全序规则的前一半）。
    fn split_axis(&self, start: usize, end: usize) -> usize {
        let mut cmin = Vec3::splat(f32::INFINITY);
        let mut cmax = Vec3::splat(f32::NEG_INFINITY);
        for &i in &self.order[start..end] {
            let c = self.centroids[i as usize];
            cmin = cmin.min(c);
            cmax = cmax.max(c);
        }
        let extent = cmax - cmin;
        if extent.x >= extent.y && extent.x >= extent.z {
            0
        } else if extent.y >= extent.z {
            1
        } else {
            2
        }
    }

    /// 按「质心沿 `axis` 分量的 `total_cmp` → 原三角序号」全序排序子区间
    /// （全序规则的后一半：并列不再依赖输入次序，也不依赖稳定排序的实现）。
    fn sort_range(&mut self, start: usize, end: usize, axis: usize) {
        let Self {
            order, centroids, ..
        } = self;
        order[start..end].sort_by(|&a, &b| {
            let ka = centroids[a as usize][axis];
            let kb = centroids[b as usize][axis];
            ka.total_cmp(&kb).then(a.cmp(&b))
        });
    }
}

/// AABB slab 前进测试：射线与盒的进入 / 离开参数（`None` = 全不相交）。
///
/// 方向分量为 0 时显式按「平行板」判定，不走 `1/0 = inf` 的除法：原点在板内
/// 时 `0 × inf = NaN`，而 NaN 会被随后的 min/max 静默吞掉，症状是「恰落在盒
/// 面上的射线被误判为不相交」——比算错更难查。
fn slab_range(min: Vec3, max: Vec3, origin: Vec3, dir: Vec3) -> Option<(f32, f32)> {
    let axis_slab = |o: f32, d: f32, bmin: f32, bmax: f32| -> (f32, f32) {
        if d == 0.0 {
            if o < bmin || o > bmax {
                (f32::INFINITY, f32::NEG_INFINITY)
            } else {
                (f32::NEG_INFINITY, f32::INFINITY)
            }
        } else {
            let inv = 1.0 / d;
            let t0 = (bmin - o) * inv;
            let t1 = (bmax - o) * inv;
            if t0 <= t1 { (t0, t1) } else { (t1, t0) }
        }
    };
    let (mut lo, mut hi) = axis_slab(origin.x, dir.x, min.x, max.x);
    for (o, d, bmin, bmax) in [
        (origin.y, dir.y, min.y, max.y),
        (origin.z, dir.z, min.z, max.z),
    ] {
        let (l, h) = axis_slab(o, d, bmin, bmax);
        lo = lo.max(l);
        hi = hi.min(h);
    }
    if lo <= hi && hi >= 0.0 {
        Some((lo, hi))
    } else {
        None
    }
}

/// Möller–Trumbore 三角求交（纯函数，脱离 BVH 可单测 / 可做暴力对照）。
///
/// 双面：不分正反面（`det` 只按绝对值判退化）。返回 `(t, u, v)`，`u`/`v` 是
/// 顶点 1 / 2 的重心权重，命中点 = `origin + dir·t`
/// = `v0·(1−u−v) + v1·u + v2·v`。`dir` 须为**单位**方向（否则 `t` 不是米）——
/// 本原语不做静默归一化，[`raycast_bvh`] 已替调用方归好。
pub fn ray_triangle_intersection(
    origin: Vec3,
    dir: Vec3,
    vertices: &[Vec3; 3],
    max_t: f32,
) -> Option<(f32, f32, f32)> {
    let e1 = vertices[1] - vertices[0];
    let e2 = vertices[2] - vertices[0];
    let p = dir.cross(e2);
    let det = e1.dot(p);
    if det.abs() <= DEGENERATE_DETERMINANT {
        return None;
    }
    let inv_det = 1.0 / det;
    let s = origin - vertices[0];
    let u = s.dot(p) * inv_det;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = dir.dot(q) * inv_det;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv_det;
    if t > BAKE_RAY_MIN_T && t <= max_t {
        Some((t, u, v))
    } else {
        None
    }
}

/// 最近命中查询。`origin` / `dir` 须有限，`dir` 须为**单位方向**（`t` 的米
/// 口径前提），`max_t` 为上界（`<= BAKE_RAY_MIN_T` 视为无效查询直接空）。
///
/// 入口只做单位守卫、不静默归一化：再归一化会把 1 ULP 的方向扰动插进调用方
/// 与本函数之间，破坏「BVH 结果 == 暴力扫描结果」的逐位可比性（本模块的
/// 跟踪器测试正依赖这条）。
///
/// 遍历：近子先出栈，命中后用其 `t` 收紧后续子树与三角的上界；`t` 并列按
/// 内部索引小者胜出（确定性论证见模块文档「跟踪器」）。
pub fn raycast_bvh(bvh: &Bvh, origin: Vec3, dir: Vec3, max_t: f32) -> Option<BvhHit> {
    assert!(
        origin.is_finite() && dir.is_finite() && dir.length_squared() > 0.0,
        "射线参数非法：origin {origin:?} dir {dir:?}"
    );
    let unit_error = (dir.length() - 1.0).abs();
    assert!(
        unit_error <= 1e-3,
        "raycast_bvh 需要单位方向（t 以米计），实际长度误差 {unit_error}"
    );
    // NaN 上界与过小上界同待遇：无效查询（NaN 参与比较恒 false，须显式挡住）
    if bvh.is_empty() || max_t.is_nan() || max_t <= BAKE_RAY_MIN_T {
        return None;
    }
    let mut best: Option<BvhHit> = None;
    let mut limit = max_t;
    let mut stack: [(u32, f32); BVH_STACK_DEPTH] = [(0, f32::NEG_INFINITY); BVH_STACK_DEPTH];
    let mut depth = 0usize;
    stack[depth] = (0, f32::NEG_INFINITY);
    depth += 1;
    while depth > 0 {
        depth -= 1;
        let (node_index, _) = stack[depth];
        let node = &bvh.nodes[node_index as usize];
        let Some((lo, _)) = slab_range(node.min, node.max, origin, dir) else {
            continue;
        };
        if lo > limit {
            continue;
        }
        if node.is_leaf() {
            for offset in 0..node.count {
                let k = (node.start + offset) as usize;
                let tri = &bvh.triangles[k];
                let Some((t, u, v)) = ray_triangle_intersection(origin, dir, &tri.vertices, limit)
                else {
                    continue;
                };
                let better = match best {
                    None => true,
                    Some(hit) => t < hit.t || (t == hit.t && k < hit.triangle),
                };
                if better {
                    best = Some(BvhHit {
                        t,
                        normal: tri.normal,
                        triangle: k,
                        barycentric: Vec2::new(u, v),
                    });
                    limit = t;
                }
            }
            continue;
        }
        // 两个子节点各测一次 slab：远的先入栈、近的先出栈（首个命中即接近
        // 最优，之后的剪枝才有意义）。无效子节点留 +inf 哨兵，被 `<= limit`
        // 的比较自然挡在栈外。
        let mut pending = [(node.left, f32::INFINITY), (node.right, f32::INFINITY)];
        for slot in pending.iter_mut() {
            let child = &bvh.nodes[slot.0 as usize];
            if let Some((lo, _)) = slab_range(child.min, child.max, origin, dir)
                && lo <= limit
            {
                slot.1 = lo;
            }
        }
        if pending[0].1 > pending[1].1 {
            pending.swap(0, 1);
        }
        for slot in pending.iter().rev() {
            if slot.1 <= limit {
                stack[depth] = *slot;
                depth += 1;
            }
        }
    }
    best
}

/// 基 2 低差异序列（van der Corput）：第 `index` 个一维样本。
///
/// 纯位运算（`reverse_bits` + 定标）⇒ 无 `rand`、无全局状态、与调用次序无关。
/// `+ 2^-33` 让样本落在各自 `2^-32` 分层区间的中点，不压到 0 / 1 边界。
/// 有效分层位数 = f32 尾数 24 位（再高的 `index` 序列退化，烘焙量级远达不到）。
pub fn radical_inverse_2(index: u32) -> f32 {
    let bits = index.reverse_bits() as u64;
    bits as f32 * (1.0 / 4_294_967_296.0) + 1.0 / 8_589_934_592.0
}

/// 以 `n` 为主轴的右手正交基（切线 / 副切线）。
///
/// 辅助轴选取规则固定（`|n.x| > 0.9` 用 +Y，否则 +X），两支都不可能与 `n`
/// 平行（单位 `n` 满足 `|n.x| > 0.9` 时 `|n.y| ≤ 0.44`）⇒ 叉积长度有下界、
/// 归一化稳定。同一法线必得同一基 ⇒ 采样方向逐位可复现。
fn orthonormal_basis(n: Vec3) -> (Vec3, Vec3) {
    let helper = if n.x.abs() > 0.9 { Vec3::Y } else { Vec3::X };
    let tangent = n.cross(helper).normalize();
    let bitangent = n.cross(tangent);
    (tangent, bitangent)
}

/// Hammersley 点集第 `index` 对 `(u1, u2)`：低差异 + 分层。
fn hammersley(index: u32, count: u32) -> (f32, f32) {
    assert!(
        count >= 1 && index < count,
        "采样序号 {index} 越界（本组共 {count} 个样本）"
    );
    (
        radical_inverse_2(index),
        (index as f32 + 0.5) / count as f32,
    )
}

/// 余弦加权半球方向（Malley 投影 ⇒ pdf = `cosθ/π`，见模块文档「采样」）。
///
/// 返回值单位长，且恒在 `normal` 的闭半球内：`dot(normalize(normal), dir)`
/// = `sqrt(1 − u2) ≥ 0`（构造保证，单测逐样本断言）。
pub fn cosine_hemisphere(sample_index: u32, count: u32, normal: Vec3) -> Vec3 {
    assert!(
        normal.is_finite() && normal.length_squared() > 0.0,
        "采样法线非法：{normal:?}"
    );
    let n = normal.normalize();
    let (tangent, bitangent) = orthonormal_basis(n);
    let (u1, u2) = hammersley(sample_index, count);
    let (sin_az, cos_az) = (std::f32::consts::TAU * u1).sin_cos();
    let radius = u2.sqrt();
    let up = (1.0 - u2).max(0.0).sqrt();
    let local = Vec3::new(radius * cos_az, radius * sin_az, up);
    (tangent * local.x + bitangent * local.y + n * local.z).normalize()
}

/// 球冠（轴 `axis`、半角余弦 `cos_theta`）内的均匀方向：cosθ 线性分层
/// ⇒ 立体角均匀（软阴影锥的正确口径）。
///
/// `cos_theta >= 1` 退化为轴本身（硬影路径，零噪声）。返回值单位长。
pub fn uniform_cone(sample_index: u32, count: u32, axis: Vec3, cos_theta: f32) -> Vec3 {
    assert!(
        axis.is_finite() && axis.length_squared() > 0.0,
        "锥轴非法：{axis:?}"
    );
    assert!(
        (0.0..=1.0).contains(&cos_theta),
        "cos_theta 须在 [0, 1]：{cos_theta}"
    );
    let a = axis.normalize();
    if cos_theta >= 1.0 {
        return a;
    }
    let (tangent, bitangent) = orthonormal_basis(a);
    let (u1, u2) = hammersley(sample_index, count);
    let cos = (1.0 - u2) + u2 * cos_theta;
    let sin = (1.0 - cos * cos).max(0.0).sqrt();
    let (sin_az, cos_az) = (std::f32::consts::TAU * u1).sin_cos();
    let local = Vec3::new(sin * cos_az, sin * sin_az, cos);
    (tangent * local.x + bitangent * local.y + a * local.z).normalize()
}

/// 辐照度烘焙参数（输入口径对齐
/// [`DirectionalLight`](crate::light::DirectionalLight) 与
/// [`EnvironmentSettings`](crate::environment::EnvironmentSettings)）。
#[derive(Clone, Copy, Debug)]
pub struct BakeParams {
    /// 方向光的**传播方向**（自光源指向场景，与 `DirectionalLight::direction`
    /// 同口径）；指向光的向量由本字段取反归一化得到。
    pub light_dir: Vec3,
    /// 光的辐照度颜色（已含强度，线性空间）。
    pub light_color: Vec3,
    /// 天空辐照度（环境底色，逐通道常数）。
    pub sky_color: Vec3,
    /// 软影锥半角（弧度）：`0` = 硬影（单条阴影射线），`> 0` 按
    /// [`SHADOW_SOFT_SAMPLES`] 个方向取平均 = 半影。定义域 `[0, π/2)`。
    pub shadow_softness: f32,
    /// 半球采样数（天空 / AO / 反弹共用同一组方向）。
    pub sky_samples: u32,
    /// 间接反弹次数：v1 支持 0 / 1（更大值断言，见模块文档）。
    pub bounces: u32,
}

impl Default for BakeParams {
    fn default() -> Self {
        Self {
            light_dir: Vec3::new(0.0, -1.0, 0.0),
            light_color: Vec3::splat(1.0),
            sky_color: Vec3::splat(0.4),
            shadow_softness: 0.0,
            sky_samples: 64,
            bounces: 1,
        }
    }
}

impl BakeParams {
    fn validate(&self) {
        assert!(
            self.light_dir.is_finite()
                && self.light_dir.length_squared() > 0.0
                && self.light_color.is_finite()
                && self.sky_color.is_finite(),
            "光照参数非法：{self:?}"
        );
        assert!(
            self.shadow_softness >= 0.0 && self.shadow_softness < std::f32::consts::FRAC_PI_2,
            "软影半角须在 [0, π/2)：{}",
            self.shadow_softness
        );
        assert!(self.sky_samples >= 1, "半球至少 1 个样本");
        assert!(
            self.bounces <= 1,
            "v1 只支持 0/1 次反弹（多 bounce 在路线图），收到 {}",
            self.bounces
        );
    }
}

/// 一个世界空间采样点的烘焙结果（分量口径与 [`BakeParams`] 一一对应，
/// b/c 阶段线性叠加即可）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BakeSample {
    /// 采样点位置（原样回填，便于 b 阶段对齐 texel）。
    pub position: Vec3,
    /// 采样点法线（**已归一化**）。
    pub normal: Vec3,
    /// 直接光辐照度（阴影后的 `NdotL × light_color`）。
    pub direct: Vec3,
    /// 天空辐照度（开阔均匀天空解析值 = `π × sky_color`，按遮蔽比例扣除）。
    pub sky: Vec3,
    /// 一次反弹辐照度（`bounces = 0` 时恒为 `Vec3::ZERO`）。
    pub bounce: Vec3,
    /// 半球遮蔽比例（AO 之补：0 = 完全开阔、1 = 半球全被挡）。
    pub occlusion: f32,
}

impl BakeSample {
    /// 总辐照度（c 阶段送进 shader 的那一项）。
    pub fn total(&self) -> Vec3 {
        self.direct + self.sky + self.bounce
    }

    /// **间接**辐照度 = 天空（含 AO 遮蔽）+ 一次反弹（含色溢）。
    ///
    /// c 阶段（R52c）上传到光照贴图的是这一份，不是 [`Self::total`]：
    /// 直接光由运行时的动态方向光 / 点光提供（混合语义 = 「静态 GI 补
    /// 动态直射」，见 `lightmap.rs` 模块文档），把烘焙的直接光一起上传
    /// 会与实时直射**双计**（受光面过曝、且烘焙阴影与 CSM 阴影叠成两层）。
    pub fn indirect(&self) -> Vec3 {
        self.sky + self.bounce
    }
}

/// 单点烘焙：直接光 + 天空/AO + 可选一次反弹（数学口径见模块文档）。
pub fn bake_sample(bvh: &Bvh, position: Vec3, normal: Vec3, params: &BakeParams) -> BakeSample {
    params.validate();
    assert!(
        position.is_finite() && normal.is_finite() && normal.length_squared() > 0.0,
        "采样点非法：position {position:?} normal {normal:?}"
    );
    let n = normal.normalize();
    let origin = position + n * BAKE_SHADOW_BIAS;
    let direct = direct_irradiance(bvh, position, n, params, params.shadow_softness > 0.0);

    let count = params.sky_samples;
    let inv_count = 1.0 / count as f32;
    // pdf = cosθ/π ⇒ 估计量 = (π/n)·Σ L(ωᵢ)：未遮挡的天空样本每条恰为 π·sky/n
    let weight = std::f32::consts::PI * inv_count;
    let mut sky = Vec3::ZERO;
    let mut bounce = Vec3::ZERO;
    let mut blocked = 0u32;
    for i in 0..count {
        let dir = cosine_hemisphere(i, count, n);
        match raycast_bvh(bvh, origin, dir, f32::INFINITY) {
            None => sky += params.sky_color * weight,
            Some(hit) => {
                blocked += 1;
                if params.bounces > 0 {
                    let hit_point = origin + dir * hit.t;
                    // 双面：反弹辐射用「翻向射线来向」的法线（薄板背面照实受光）
                    let m = if hit.normal.dot(-dir) >= 0.0 {
                        hit.normal
                    } else {
                        -hit.normal
                    };
                    let albedo = bvh.triangle(hit.triangle).albedo;
                    let incident = direct_irradiance(bvh, hit_point, m, params, false)
                        + params.sky_color * std::f32::consts::PI;
                    // L = albedo/π × E，与 (π/n) 的估计量系数相消
                    bounce += albedo * incident * inv_count;
                }
            }
        }
    }
    BakeSample {
        position,
        normal: n,
        direct,
        sky,
        bounce,
        occlusion: blocked as f32 * inv_count,
    }
}

/// 直接光辐照度。`allow_soft = false` 时忽略软影参数走单射线（反弹命中点的
/// 二次阴影用这条——见模块文档 v1 限制）。
///
/// 软影口径：锥内方向的**未遮挡比例**当半影亮度系数，余弦项仍取光心方向
/// （`ndotl`）。于是全遮挡 → 0、全开阔 → `light_color × ndotl`（与硬影分支
/// 逐位同值，8/8 = 1.0 是精确的浮点常数），半影带 → `k/8` 档中间值。
/// 不做「锥内 cos 平均」：那种写法会让一切受光面随软影角度整体变暗，把
/// 半影的代价摊到全场景，且失去与硬影的逐位对照点。
///
/// `pub(crate)`（R83）：探针的 SH cube 投影要按**同一口径**求命中点的入射
/// 辐照（`irradiance::probe_sh_radiance`），复用本函数而不是复制一份公式。
pub(crate) fn direct_irradiance(
    bvh: &Bvh,
    position: Vec3,
    normal: Vec3,
    params: &BakeParams,
    allow_soft: bool,
) -> Vec3 {
    let to_light = -params.light_dir.normalize();
    let ndotl = normal.dot(to_light);
    if ndotl <= 0.0 {
        // 背光面零贡献且一条射线都不发：这是 NdotL 钳 0 的直接结果，也让
        // 「影子内 = 0」的断言是解析 0 而非「恰好没采到」
        return Vec3::ZERO;
    }
    let origin = position + normal * BAKE_SHADOW_BIAS;
    if !allow_soft || params.shadow_softness <= 0.0 {
        return match raycast_bvh(bvh, origin, to_light, f32::INFINITY) {
            None => params.light_color * ndotl,
            Some(_) => Vec3::ZERO,
        };
    }
    let cos_apex = params.shadow_softness.cos();
    let mut visible = 0u32;
    for i in 0..SHADOW_SOFT_SAMPLES {
        let dir = uniform_cone(i, SHADOW_SOFT_SAMPLES, to_light, cos_apex);
        if raycast_bvh(bvh, origin, dir, f32::INFINITY).is_none() {
            visible += 1;
        }
    }
    params.light_color * ndotl * (visible as f32 / SHADOW_SOFT_SAMPLES as f32)
}

/// 批量烘焙：输入世界空间采样点列表 `(position, normal)`，输出逐点结果。
///
/// 这就是 a 阶段的对外接口——b 阶段把 texel 中心映射成同样的点列表、按序
/// 写回图集，本函数无需改动；分段并行也只需切 `points`（结果按段拼接保序）。
pub fn bake_points(bvh: &Bvh, points: &[(Vec3, Vec3)], params: &BakeParams) -> Vec<BakeSample> {
    points
        .iter()
        .map(|&(position, normal)| bake_sample(bvh, position, normal, params))
        .collect()
}

/// 从世界收集可烘焙的静态几何（a 阶段的世界封装，最小口径）。
///
/// 收录条件（全部满足）：有 [`MeshHandle`] + 有 [`GlobalTransform`] +
/// **静态**（无 [`RigidBody`]——组件缺席 = 静态；或有 [`Collider`] 且
/// `is_static` 且 `body_type != Kinematic`）+ 非蒙皮 + 未隐藏 +
/// **无 [`BakeExcluded`] 标记**（R89，缺席 = 收录 ⇒ 既有场景逐位不变）。
///
/// - **动态 / 运动学体排除**：烘焙结果绑定在固定位置，会动的几何（升降台、
///   弹簧门）烘进去就是错位阴影；`Collider::is_static` 是「不移动」的显式
///   声明，故带 Dynamic 刚体但标静态碰撞体的实体仍收录（与物理侧
///   [`effective_static`](iris_core::physics::effective_static) 同口径）；
/// - **蒙皮排除**：姿态每帧由骨骼驱动，绑定姿态烘焙没有意义（v1 限制）；
/// - **`BakeExcluded` 排除（R89）**：编辑器叠加层等「可视但非场景内容」
///   的实体在收集阶段即出局（查询元组已满 7 项 ⇒ 在阶段二用
///   `world.get` 逐实体判定——顺序不变，只是多一路 skip，输出序 =
///   收录实体的查询序，确定性不受影响）；
/// - 只认 `MeshHandle` ⇒ `LodMesh` / `LodMesh3` 几何（R51 地形 chunk）暂不
///   参与，见模块文档；
/// - 无 `GlobalTransform` 的实体自然落选：层级同步未跑过的实体没有确定世界
///   位置（与渲染收集同口径，见 ARCHITECTURE §4.11）；
/// - 反照率取材质 `base_color` 的 RGB（无材质 / 悬垂句柄 / 无 `MaterialStore`
///   资源 = 白，与渲染器的白色兜底同口径）；透明材质按不透明处理。
///
/// 法线变换走法线矩阵 `(M⁻¹)ᵀ`（与 [`InstanceRaw`](crate::instance::InstanceRaw)
/// 给 GPU 的那份同式）；三角法线 = 三顶点法线平均，若平均法线与几何法线
/// （边叉积）反号则翻转——负缩放镜像实例的正面朝向由环绕方向决定，法线必须
/// 跟着翻，否则镜像墙的受光方向是反的。
///
/// 输出次序 = 查询次序（实体索引升序，确定性文化见 `iris_core::query`），
/// 故同世界两次收集 + 两次 [`build_bvh`] 逐位一致。
pub fn collect_bake_triangles(world: &World) -> Vec<BakeTriangle> {
    // 两阶段（收集 → 处理）：查询闭包内不再借任何存储，见 AGENTS.md 系统规约。
    // 查询元组上限是 7（iris_core::query），故材质句柄留到阶段二逐实体取；
    // BakeExcluded（R89）同理在阶段二判定（元组无空位，且世界级点查与
    // 收集语义等价）。
    let mut sources: Vec<(Entity, GlobalTransform, MeshHandle)> = Vec::new();
    world.query::<(
        Entity,
        &GlobalTransform,
        &MeshHandle,
        Option<&RigidBody>,
        Option<&Collider>,
        Option<&Skin>,
        Option<&Visibility>,
    )>(
        |(entity, global, mesh, rigid, collider, skin, visibility)| {
            if skin.is_some() || visibility.is_some_and(|v| !v.visible) {
                return;
            }
            let static_geometry = match rigid {
                None => true,
                Some(body) => {
                    body.body_type != BodyType::Kinematic && collider.is_some_and(|c| c.is_static)
                }
            };
            if !static_geometry {
                return;
            }
            sources.push((entity, *global, *mesh));
        },
    );
    let Some(mesh_store) = world.try_resource::<MeshStore>() else {
        return Vec::new();
    };
    let materials = world.try_resource::<MaterialStore>();
    let mut out = Vec::new();
    for (entity, global, mesh) in sources {
        // R89：编辑器叠加层等标记实体不进 lightmap / 探针 / DDGI 共用的
        // 三角收集（缺席 = 收录，既有场景逐位不变）
        if world.get::<BakeExcluded>(entity).is_some() {
            continue;
        }
        let Some(data) = mesh_store.get(mesh) else {
            continue; // 悬垂网格句柄：与渲染器同语义（不可见即不可挡光）
        };
        assert!(
            data.indices.len() % 3 == 0
                && data
                    .indices
                    .iter()
                    .all(|&i| (i as usize) < data.vertices.len()),
            "网格 {mesh:?} 索引非法（长度非 3 的倍数或越界）"
        );
        let albedo = world
            .get::<MaterialHandle>(entity)
            .and_then(|handle| materials.as_ref().and_then(|store| store.get(*handle)))
            .map(|def| Vec3::new(def.base_color[0], def.base_color[1], def.base_color[2]))
            .unwrap_or(Vec3::ONE);
        let model = global.matrix();
        let normal_matrix = Mat3::from_mat4(model).inverse().transpose();
        for tri in data.indices.chunks(3) {
            let mut vertices = [Vec3::ZERO; 3];
            let mut normal = Vec3::ZERO;
            for (slot, &index) in tri.iter().enumerate() {
                let vertex = &data.vertices[index as usize];
                vertices[slot] = model.transform_point3(Vec3::from_array(vertex.position));
                normal += normal_matrix * Vec3::from_array(vertex.normal);
            }
            let geometric = (vertices[1] - vertices[0]).cross(vertices[2] - vertices[0]);
            if normal.length_squared() <= f32::EPSILON {
                normal = geometric;
            }
            if normal.dot(geometric) < 0.0 {
                normal = -normal;
            }
            out.push(BakeTriangle {
                vertices,
                normal,
                albedo,
                entity,
            });
        }
    }
    out
}

/// 烘焙产物：图集布局 + 逐 texel 辐照度（R52b）。
///
/// 两份辐照数组同为**行主序**、与 `atlas.size²` 对齐（`index = y × size + x`，
/// 行 0 = v 0）；未覆盖 texel（chart 间隙 / 内边距）恒 `Vec3::ZERO`：
///
/// - [`Self::radiance`] = [`BakeSample::total`]（直接 + 天空 + 反弹）——
///   预览 / 调试口径（影带在这一份里才看得见）；
/// - [`Self::indirect`] = [`BakeSample::indirect`]（天空 + 反弹）——
///   **c 阶段上传口径**：直接光由运行时动态光提供，双计论证见该方法文档。
///
/// 辐照度是线性 HDR，预览 / 上传前各自做色调映射或半精度转换。
#[derive(Clone, Debug)]
pub struct BakedLightmap {
    /// 图集布局（chart / 三角形矩形表——c 阶段 UV2 写回与纹理上传的索引）。
    pub atlas: crate::bake_uv::LightmapAtlas,
    /// 逐 texel 总辐照度（行主序，`atlas.size²` 个）。
    pub radiance: Vec<Vec3>,
    /// 逐 texel **间接**辐照度（行主序，同上；光照贴图纹理的数据源）。
    pub indirect: Vec<Vec3>,
    /// 逐 texel 覆盖位图（行主序，同上）：`true` = 该 texel 被
    /// [`bake_texels`](crate::bake_uv::bake_texels) 回溯命中（= 辐照数组里
    /// 的值是真烘焙结果，不是零填充）。c 阶段的 dilation 填充据此区分
    /// 「真黑（封闭暗角）」与「没烘到（chart 内边距 / 间隙）」——按
    /// `!= Vec3::ZERO` 判会把前者误当后者填掉。
    pub covered: Vec<bool>,
}

/// 全链烘焙（R52b 组装入口）：世界 → 静态三角收集 → BVH → chart 参数化
/// → shelf 装箱 → texel 世界位回溯 → 逐 texel [`bake_sample`] → 图集。
///
/// 各环节语义与确定性论证见 [`bake_uv`](crate::bake_uv) 模块文档；texel
/// 序列按行主序升序喂 [`bake_points`]——这就是 R52a 预留的确定性切分点
/// （分段并行按 `TexelSource::index` 切、按序拼接即可）。`AtlasEntry::mesh`
/// 在本函数内补齐（纯函数 [`pack_atlas`](crate::bake_uv::pack_atlas) 不查
/// 世界）。空世界合法：空图集 + 全零辐照。
pub fn bake_lightmap(
    world: &World,
    params: &BakeParams,
    uv: &crate::bake_uv::BakeUvParams,
) -> BakedLightmap {
    use crate::bake_uv::{bake_texels, build_charts, pack_atlas};

    let triangles = collect_bake_triangles(world);
    let bvh = build_bvh(triangles.clone());
    let charts = build_charts(&triangles, uv);
    let mut atlas = pack_atlas(charts, uv.atlas_size);
    for entry in &mut atlas.entries {
        entry.mesh = world.get::<MeshHandle>(entry.entity).map(|handle| *handle);
    }
    let sources = bake_texels(&atlas, &triangles);
    let points: Vec<(Vec3, Vec3)> = sources
        .iter()
        .map(|source| (source.position, source.normal))
        .collect();
    let samples = bake_points(&bvh, &points, params);
    let size = uv.atlas_size as usize;
    let mut radiance = vec![Vec3::ZERO; size * size];
    let mut indirect = vec![Vec3::ZERO; size * size];
    let mut covered = vec![false; size * size];
    for (source, sample) in sources.iter().zip(&samples) {
        radiance[source.index] = sample.total();
        indirect[source.index] = sample.indirect();
        covered[source.index] = true;
    }
    BakedLightmap {
        atlas,
        radiance,
        indirect,
        covered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core::prelude::{Mat4, Quat, Transform};

    use crate::material::MaterialDef;
    use crate::mesh::{MeshData, Vertex};

    /// 测试积木：按环绕方向求法线的三角（与 `build_bvh` /
    /// `collect_bake_triangles` 的定则一致，避免手写数据自相矛盾）。
    fn tri(v0: Vec3, v1: Vec3, v2: Vec3) -> BakeTriangle {
        let normal = (v1 - v0).cross(v2 - v0);
        BakeTriangle {
            vertices: [v0, v1, v2],
            normal,
            albedo: Vec3::ONE,
            entity: Entity::from_bits(0),
        }
    }

    fn tri_at(v0: Vec3, v1: Vec3, v2: Vec3, albedo: Vec3) -> BakeTriangle {
        let mut t = tri(v0, v1, v2);
        t.albedo = albedo;
        t
    }

    /// 矩形面 = 两个三角（`u` / `v` 为半边向量，`u × v` = 法线，与
    /// `primitives::add_face` 的 CCW 口径一致）。
    fn quad_at(center: Vec3, u: Vec3, v: Vec3, albedo: Vec3) -> Vec<BakeTriangle> {
        let normal = u.cross(v);
        let c = [
            center - u - v,
            center + u - v,
            center + u + v,
            center - u + v,
        ];
        let a = tri_at(c[0], c[1], c[2], albedo);
        let b = tri_at(c[0], c[2], c[3], albedo);
        assert!(
            a.normal.dot(normal) > 0.0 && b.normal.dot(normal) > 0.0,
            "quad 的环绕方向与 u × v 矛盾"
        );
        vec![a, b]
    }

    fn quad(center: Vec3, u: Vec3, v: Vec3) -> Vec<BakeTriangle> {
        quad_at(center, u, v, Vec3::ONE)
    }

    /// 轴对齐盒的 12 个三角（外法线）：每面 `(偏移, u, v)` 满足 `u × v` = 外法线。
    fn box_at(min: Vec3, max: Vec3, albedo: Vec3) -> Vec<BakeTriangle> {
        let center = (min + max) * 0.5;
        let h = (max - min) * 0.5;
        let faces = [
            (Vec3::X * h.x, Vec3::Y * h.y, Vec3::Z * h.z),
            (Vec3::NEG_X * h.x, Vec3::Z * h.z, Vec3::Y * h.y),
            (Vec3::Y * h.y, Vec3::Z * h.z, Vec3::X * h.x),
            (Vec3::NEG_Y * h.y, Vec3::X * h.x, Vec3::Z * h.z),
            (Vec3::Z * h.z, Vec3::X * h.x, Vec3::Y * h.y),
            (Vec3::NEG_Z * h.z, Vec3::Y * h.y, Vec3::X * h.x),
        ];
        let mut out = Vec::with_capacity(12);
        for (offset, u, v) in faces {
            out.extend(quad_at(center + offset, u, v, albedo));
        }
        out
    }

    fn box_triangles(min: Vec3, max: Vec3) -> Vec<BakeTriangle> {
        box_at(min, max, Vec3::ONE)
    }

    /// 大地面（200 × 200 m，法线 +Y）：大到所有测试用的半球射线都落在面内
    /// （最斜的 64 样本射线在 2 m 高度上横向跑出 22.6 m）。
    fn ground() -> Vec<BakeTriangle> {
        quad_at(
            Vec3::ZERO,
            Vec3::new(100.0, 0.0, 0.0),
            Vec3::new(0.0, 0.0, -100.0),
            Vec3::ONE,
        )
    }

    fn scene(triangles: Vec<BakeTriangle>) -> Bvh {
        build_bvh(triangles)
    }

    /// splitmix64 终结函数（与 `texture.rs` / `particles.rs` 同源）：测试场景
    /// 的抖动源，无 `rand`、无全局状态。
    fn mix64(mut z: u64) -> u64 {
        z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// [0, 1) 均匀哈希数。
    fn hash_unit(seed: u64) -> f32 {
        (mix64(seed) >> 40) as f32 / (1u64 << 24) as f32
    }

    /// 抖动散落的三角网格场景（`side²` 个 quad = `2·side²` 三角）：位置 /
    /// 高度 / 朝向都带哈希抖动 ⇒ 无共面、无对称重合（暴力对照与重排断言都
    /// 依赖这一点）。
    fn scattered(side: u32) -> Vec<BakeTriangle> {
        let mut out = Vec::new();
        for gz in 0..side {
            for gx in 0..side {
                let base = (gz * side + gx) as u64;
                let center = Vec3::new(
                    gx as f32 * 2.0 - side as f32 + hash_unit(base * 3 + 1),
                    hash_unit(base * 3 + 2) * 4.0 - 2.0,
                    gz as f32 * 2.0 - side as f32 + hash_unit(base * 3 + 3),
                );
                let u = Vec3::new(
                    0.6 + hash_unit(base * 7 + 1),
                    hash_unit(base * 7 + 2) - 0.5,
                    hash_unit(base * 7 + 3) - 0.5,
                );
                let v = Vec3::new(
                    hash_unit(base * 11 + 1) - 0.5,
                    0.6 + hash_unit(base * 11 + 2),
                    hash_unit(base * 11 + 3) - 0.5,
                );
                let mut tris = quad_at(center, u, v, Vec3::splat(hash_unit(base * 13 + 1)));
                for tri in tris.iter_mut() {
                    tri.entity = Entity::from_bits(base + 1);
                }
                out.extend(tris);
            }
        }
        out
    }

    /// 暴力对照：全量线性扫描取最近命中（并列取原序号小者）。
    fn brute_force(
        triangles: &[BakeTriangle],
        origin: Vec3,
        dir: Vec3,
    ) -> Option<(&BakeTriangle, f32)> {
        let mut best: Option<(usize, f32)> = None;
        for (index, tri) in triangles.iter().enumerate() {
            let Some((t, _, _)) =
                ray_triangle_intersection(origin, dir, &tri.vertices, f32::INFINITY)
            else {
                continue;
            };
            match best {
                None => best = Some((index, t)),
                Some((_, bt)) if t < bt => best = Some((index, t)),
                Some(_) => {}
            }
        }
        best.map(|(index, t)| (&triangles[index], t))
    }

    /// 把 `entity` 标记到若干三角上（测试用身份识别，BVH 重排后仍能对上）。
    fn tag(triangles: &mut [BakeTriangle], entity: Entity) {
        for tri in triangles.iter_mut() {
            tri.entity = entity;
        }
    }

    fn count_of(triangles: &[BakeTriangle], entity: Entity) -> usize {
        triangles.iter().filter(|t| t.entity == entity).count()
    }

    // ───────────────────────────── 跟踪器 ─────────────────────────────

    /// v0 原点、v1 = +X·2、v2 = +Z·2 ⇒ 平面 y = 0。从 (0.5, 1, 0.5) 竖直向下：
    /// 手算 `det = e1·(d×e2) = (2,0,0)·(0,−2,0)… = −4`、`u = v = 0.25`、`t = 1`。
    #[test]
    fn ray_triangle_intersection_matches_analytic_t_and_barycentrics() {
        let (v0, v1, v2) = (Vec3::ZERO, Vec3::X * 2.0, Vec3::Z * 2.0);
        let origin = Vec3::new(0.5, 1.0, 0.5);
        let (t, u, v) =
            ray_triangle_intersection(origin, Vec3::NEG_Y, &[v0, v1, v2], f32::INFINITY)
                .expect("应命中三角形内部");
        assert!((t - 1.0).abs() < 1e-6, "t 应为 1，实际 {t}");
        assert!(
            (u - 0.25).abs() < 1e-6 && (v - 0.25).abs() < 1e-6,
            "重心应为 (0.25, 0.25)，实际 ({u}, {v})"
        );
        // 两条命中点公式必须互洽：origin + dir·t == v0(1−u−v) + v1·u + v2·v
        let by_t = origin + Vec3::NEG_Y * t;
        let by_bary = v0 * (1.0 - u - v) + v1 * u + v2 * v;
        assert!(
            (by_t - by_bary).length() < 1e-6,
            "命中点复原不一致：{by_t:?} vs {by_bary:?}"
        );
    }

    #[test]
    fn ray_triangle_intersection_rejects_miss_behind_and_self_hit() {
        let tri = [Vec3::ZERO, Vec3::X * 2.0, Vec3::Z * 2.0];
        assert!(
            ray_triangle_intersection(Vec3::new(3.0, 1.0, 3.0), Vec3::NEG_Y, &tri, f32::INFINITY)
                .is_none(),
            "三角形外（u + v > 1）不应命中"
        );
        assert!(
            ray_triangle_intersection(Vec3::new(0.5, -1.0, 0.5), Vec3::NEG_Y, &tri, f32::INFINITY)
                .is_none(),
            "反向（t < 0）不应命中"
        );
        assert!(
            ray_triangle_intersection(Vec3::new(0.5, 1.0, 0.5), Vec3::NEG_Y, &tri, 0.5).is_none(),
            "超出 max_t 不应命中"
        );
        // 自交保护：原点恰在面上、方向沿平面 → t = 0 被 BAKE_RAY_MIN_T 挡掉
        assert!(
            ray_triangle_intersection(Vec3::new(0.5, 0.0, 0.5), Vec3::NEG_Y, &tri, f32::INFINITY)
                .is_none(),
            "贴在面上的原点不应命中自己"
        );
        // 与平面平行的射线：det = 0 走退化分支
        assert!(
            ray_triangle_intersection(Vec3::new(0.5, 0.0, 0.5), Vec3::X, &tri, f32::INFINITY)
                .is_none(),
            "平行射线不应命中"
        );
    }

    #[test]
    fn raycast_bvh_hits_with_analytic_values_and_unit_t() {
        let bvh = scene(vec![tri(Vec3::ZERO, Vec3::X * 2.0, Vec3::Z * 2.0)]);
        assert_eq!(
            bvh.node_count(),
            1,
            "不超过 BVH_LEAF_SIZE 的输入只有一个叶节点"
        );
        assert_eq!(bvh.triangle_count(), 1);
        let hit = raycast_bvh(&bvh, Vec3::new(0.5, 1.0, 0.5), Vec3::NEG_Y, f32::INFINITY)
            .expect("应命中");
        assert!((hit.t - 1.0).abs() < 1e-6);
        assert_eq!(hit.triangle, 0);
        // 环绕法线 = (v1−v0)×(v2−v0) = −Y，构建时归一化后原样返回
        assert!(
            (hit.normal - Vec3::NEG_Y).length() < 1e-6,
            "法线应为环绕方向的 −Y，实际 {:?}",
            hit.normal
        );
        // 无命中口径：max_t 过小 / 空 BVH
        assert!(raycast_bvh(&bvh, Vec3::new(0.5, 1.0, 0.5), Vec3::NEG_Y, 0.5).is_none());
        assert!(raycast_bvh(&Bvh::empty(), Vec3::ZERO, Vec3::NEG_Y, f32::INFINITY).is_none());
    }

    /// 单位方向是入口契约：非单位方向直接断言，而不是静默归一化后给出一个
    /// 与调用方预期不同的 `t`。
    #[test]
    #[should_panic(expected = "需要单位方向")]
    fn raycast_bvh_rejects_non_unit_direction() {
        let bvh = scene(vec![tri(Vec3::ZERO, Vec3::X * 2.0, Vec3::Z * 2.0)]);
        let _ = raycast_bvh(
            &bvh,
            Vec3::new(0.5, 1.0, 0.5),
            Vec3::NEG_Y * 7.5,
            f32::INFINITY,
        );
    }

    /// AABB 剪枝正确性 + 并列口径：远近两三角取近者；撤掉近者后同一射线
    /// 打到远者；共面重合的两三角 `t` 逐位相等 ⇒ 取内部索引小者。
    #[test]
    fn raycast_bvh_prunes_to_nearest_and_breaks_ties_by_index() {
        let near = tri_at(Vec3::ZERO, Vec3::X * 2.0, Vec3::Z * 2.0, Vec3::X);
        let far = tri_at(
            Vec3::NEG_Y,
            Vec3::X * 2.0 - Vec3::Y,
            Vec3::Z * 2.0 - Vec3::Y,
            Vec3::Y,
        );
        let mut both = vec![near, far];
        tag(&mut both, Entity::from_bits(1));
        both[1].entity = Entity::from_bits(2);
        let bvh = scene(both.clone());
        let hit = raycast_bvh(&bvh, Vec3::new(0.5, 1.0, 0.5), Vec3::NEG_Y, f32::INFINITY)
            .expect("应命中");
        assert!((hit.t - 1.0).abs() < 1e-6, "应取近三角，t = {}", hit.t);
        assert_eq!(bvh.triangle(hit.triangle).entity, Entity::from_bits(1));

        let only_far = scene(vec![both[1]]);
        let hit = raycast_bvh(
            &only_far,
            Vec3::new(0.5, 1.0, 0.5),
            Vec3::NEG_Y,
            f32::INFINITY,
        )
        .expect("撤掉遮挡后应命中远三角");
        assert!(
            (hit.t - 2.0).abs() < 1e-6,
            "远三角距离应为 2，实际 {}",
            hit.t
        );

        // 完全共面重合 ⇒ t 逐位相等，并列按内部索引（构建序）定死
        let mut twin = vec![both[0], both[0]];
        twin[0].entity = Entity::from_bits(10);
        twin[1].entity = Entity::from_bits(11);
        let bvh = scene(twin);
        let hit = raycast_bvh(&bvh, Vec3::new(0.5, 1.0, 0.5), Vec3::NEG_Y, f32::INFINITY)
            .expect("重合三角应命中其一");
        assert_eq!(
            bvh.triangle(hit.triangle).entity,
            Entity::from_bits(10),
            "t 并列时必须取内部索引小者"
        );
        // 并列只决定「算作谁」，不改变命中距离：与单三角场景逐位相同
        let single = scene(vec![both[0]]);
        let reference = raycast_bvh(
            &single,
            Vec3::new(0.5, 1.0, 0.5),
            Vec3::NEG_Y,
            f32::INFINITY,
        )
        .expect("单三角应命中");
        assert_eq!(hit.t.to_bits(), reference.t.to_bits());
        assert_eq!(hit.barycentric.to_array(), reference.barycentric.to_array());
    }

    #[test]
    fn raycast_bvh_matches_brute_force_on_scattered_scene() {
        let triangles = scattered(16); // 512 三角（> 规范要求的 300+）
        let bvh = scene(triangles.clone());
        let mut hits = 0usize;
        for stream in 0..64u64 {
            let origin = Vec3::new(
                hash_unit(stream * 5 + 1) * 20.0 - 10.0,
                hash_unit(stream * 5 + 2) * 10.0 - 5.0,
                hash_unit(stream * 5 + 3) * 20.0 - 10.0,
            );
            let dir = Vec3::new(
                hash_unit(stream * 7 + 11) * 2.0 - 1.0,
                hash_unit(stream * 7 + 12) * 2.0 - 1.0,
                hash_unit(stream * 7 + 13) * 2.0 - 1.0,
            )
            .normalize();
            let brute = brute_force(&triangles, origin, dir);
            let traced = raycast_bvh(&bvh, origin, dir, f32::INFINITY);
            match (brute, traced) {
                (None, None) => {}
                (Some((tri, t)), Some(hit)) => {
                    assert_eq!(
                        t.to_bits(),
                        hit.t.to_bits(),
                        "射线 {origin:?}→{dir:?}：BVH 命中距离必须逐位等于暴力扫描"
                    );
                    assert_eq!(tri.entity, bvh.triangle(hit.triangle).entity);
                    hits += 1;
                }
                (Some(_), None) => panic!("BVH 漏命中（AABB 剪枝过激）：{origin:?}→{dir:?}"),
                (None, Some(_)) => panic!("BVH 假命中（剪枝漏判）：{origin:?}→{dir:?}"),
            }
        }
        assert!(hits > 20, "64 条射线应有多次命中，实际 {hits}");
    }

    /// 中值分割策略钉死：≤ 叶容量 ⇒ 只有根叶；9 个三角 ⇒ 根 + 左(4) +
    /// 右(5 → 再分 2/3) = 5 节点。
    #[test]
    fn bvh_median_split_yields_expected_node_counts() {
        let triangles = scattered(8);
        assert_eq!(scene(triangles[..4].to_vec()).node_count(), 1);
        assert_eq!(scene(triangles[..5].to_vec()).node_count(), 3);
        assert_eq!(scene(triangles[..8].to_vec()).node_count(), 3);
        assert_eq!(scene(triangles[..9].to_vec()).node_count(), 5);
    }

    #[test]
    fn bvh_build_and_raycast_are_bitwise_deterministic() {
        let triangles = scattered(32); // 2048 三角
        assert!(triangles.len() > 1000);
        let a = build_bvh(triangles.clone());
        let b = build_bvh(triangles.clone());
        assert_eq!(a.node_count(), b.node_count());
        let order_a: Vec<u64> = (0..a.triangle_count())
            .map(|i| a.triangle(i).entity.to_bits())
            .collect();
        let order_b: Vec<u64> = (0..b.triangle_count())
            .map(|i| b.triangle(i).entity.to_bits())
            .collect();
        assert_eq!(order_a, order_b, "两次构建的三角形重排序列必须逐位一致");
        let input_order: Vec<u64> = triangles.iter().map(|t| t.entity.to_bits()).collect();
        assert_ne!(
            order_a, input_order,
            "构建确实重排了顺序（若此断言失败说明输入恰按分割轴有序，换测试场景）"
        );
        for stream in 0..64u64 {
            let origin = Vec3::splat(hash_unit(stream + 500) * 4.0 - 2.0);
            let dir = Vec3::new(
                hash_unit(stream * 3 + 17) - 0.5,
                hash_unit(stream * 3 + 18) - 0.5,
                hash_unit(stream * 3 + 19) - 0.5,
            )
            .normalize();
            match (
                raycast_bvh(&a, origin, dir, f32::INFINITY),
                raycast_bvh(&b, origin, dir, f32::INFINITY),
            ) {
                (None, None) => {}
                (Some(x), Some(y)) => {
                    assert_eq!(x.t.to_bits(), y.t.to_bits());
                    assert_eq!(x.triangle, y.triangle);
                    assert_eq!(x.barycentric.x.to_bits(), y.barycentric.x.to_bits());
                    assert_eq!(x.barycentric.y.to_bits(), y.barycentric.y.to_bits());
                    assert_eq!(x.normal.to_array(), y.normal.to_array());
                }
                _ => panic!("两次构建的命中与否都不同：{origin:?}→{dir:?}"),
            }
        }
    }

    #[test]
    #[should_panic(expected = "退化（零面积）")]
    fn build_bvh_rejects_degenerate_triangle() {
        let mut flat = tri(Vec3::ZERO, Vec3::X, Vec3::X * 2.0);
        // 法线合法、三点共线 ⇒ 触发的必须是退化断言而非数据非法断言
        flat.normal = Vec3::Y;
        let _ = build_bvh(vec![flat]);
    }

    // ───────────────────────────── 采样 ─────────────────────────────

    /// 余弦加权半球（pdf = cosθ/π）的解析一阶矩 E[ω] = (2/3)·n。
    /// 容差依据：Var(cosθ) = E[cos²] − E[cos]² = 1/2 − 4/9 = 1/18 ⇒
    /// σ_mean = sqrt(1/18/4096) = 0.0037；切向 Var = 2/15 ⇒ σ = 0.0057；
    /// 三分量合成的 3σ ≈ 0.03，容差取 0.035 留平台 libm 余量。
    #[test]
    fn cosine_hemisphere_stays_in_hemisphere_and_matches_expected_mean() {
        const N: u32 = 4096;
        for normal in [Vec3::Y, Vec3::X, Vec3::new(0.3, -0.5, 0.8)] {
            let n = normal.normalize();
            let mut sum = Vec3::ZERO;
            for i in 0..N {
                let d = cosine_hemisphere(i, N, normal);
                assert!((d.length() - 1.0).abs() < 1e-6, "方向须单位长");
                assert!(
                    n.dot(d) >= 0.0,
                    "样本 {i} 落在开半球外（dot = {}）",
                    n.dot(d)
                );
                sum += d;
            }
            let mean = sum / N as f32;
            let expected = n * (2.0 / 3.0);
            assert!(
                (mean - expected).length() < 0.035,
                "均值 {mean:?} 偏离解析值 {expected:?} 超过 3σ"
            );
        }
    }

    /// 球冠均匀的解析一阶矩 E[cosθ] = (1 + cos_apex)/2；容差依据
    /// Var = (1 − cos_apex)²/12 = 0.0208 ⇒ σ_mean = 0.0023，取 3σ ≈ 0.0068
    /// 再放宽到 0.01。退化锥（cos_apex = 1）必须逐位返回轴本身。
    #[test]
    fn uniform_cone_stays_in_cap_and_matches_expected_mean_cos() {
        const N: u32 = 4096;
        let axis = Vec3::Y;
        let cos_apex = 0.5;
        let mut sum = Vec3::ZERO;
        for i in 0..N {
            let d = uniform_cone(i, N, axis, cos_apex);
            assert!(
                axis.dot(d) >= cos_apex - 1e-6,
                "样本 {i} 越出球冠：cos = {}",
                axis.dot(d)
            );
            sum += d;
        }
        let mean = sum / N as f32;
        assert!(
            (axis.dot(mean) - 0.75).abs() < 0.01,
            "E[cosθ] 应为 0.75，实际 {}",
            axis.dot(mean)
        );
        assert!(
            Vec3::new(mean.x, 0.0, mean.z).length() < 0.01,
            "方位对称性要求切向均值为 0，实际 {mean:?}"
        );
        // 退化锥 = 轴（硬影路径零噪声）
        assert_eq!(
            uniform_cone(0, SHADOW_SOFT_SAMPLES, axis, 1.0).to_array(),
            axis.to_array()
        );
        assert_eq!(
            uniform_cone(SHADOW_SOFT_SAMPLES - 1, SHADOW_SOFT_SAMPLES, axis, 1.0).to_array(),
            axis.to_array()
        );
    }

    /// 采样是纯函数：同 `(index, count, 轴向)` 逐位同输出，与调用次序无关
    /// （低差异序列常被误当成「必须顺序推进」的流——这里把它逆序重采一遍）。
    #[test]
    fn sampling_is_bitwise_stable_and_order_independent() {
        let n = Vec3::new(0.2, 0.7, -0.4);
        let count = 64u32;
        let cosine: Vec<[f32; 3]> = (0..count)
            .map(|i| cosine_hemisphere(i, count, n).to_array())
            .collect();
        let cone: Vec<[f32; 3]> = (0..SHADOW_SOFT_SAMPLES)
            .map(|i| uniform_cone(i, SHADOW_SOFT_SAMPLES, n, 0.6).to_array())
            .collect();
        for i in (0..count).rev() {
            assert_eq!(
                cosine_hemisphere(i, count, n).to_array(),
                cosine[i as usize],
                "余弦半球样本 {i} 随调用次序漂移"
            );
        }
        for i in (0..SHADOW_SOFT_SAMPLES).rev() {
            assert_eq!(
                uniform_cone(i, SHADOW_SOFT_SAMPLES, n, 0.6).to_array(),
                cone[i as usize],
                "球冠样本 {i} 随调用次序漂移"
            );
        }
        // 同一 count 下的样本集合不随 count 变化的部分不该出现（口径提示：
        // u2 = (i + 0.5)/count 依赖 count ⇒ 换样本数即换整套点，这是分层序列
        // 的正常行为，此处只钉住「同参数逐位稳定」这一条）
        assert_ne!(
            cosine_hemisphere(0, count * 2, n).to_array(),
            cosine[0],
            "u2 分层随 count 变化 ⇒ 换样本数应得到不同的点"
        );
    }

    // ───────────────────────────── 积分器 ─────────────────────────────

    /// 开阔平面（无任何几何）：直接光 = 光色 × NdotL（解析逐位）、
    /// 天空 = π × sky_color（余弦加权估计量的精确期望）、AO = 0、反弹 = 0。
    #[test]
    fn open_plane_direct_and_sky_match_analytic() {
        let params = BakeParams {
            light_dir: Vec3::new(0.0, -1.0, 0.0),
            light_color: Vec3::new(1.0, 0.5, 0.25),
            sky_color: Vec3::new(0.2, 0.4, 0.6),
            shadow_softness: 0.0,
            sky_samples: 64,
            bounces: 0,
        };
        let empty = Bvh::empty();
        let up = bake_sample(&empty, Vec3::new(0.0, 1.0, 0.0), Vec3::Y, &params);
        assert_eq!(up.direct, params.light_color, "NdotL = 1 ⇒ 逐位等于光色");
        let analytic_sky = params.sky_color * std::f32::consts::PI;
        assert!(
            (up.sky - analytic_sky).length() < 1e-4,
            "天空应为 π×sky = {analytic_sky:?}，实际 {:?}",
            up.sky
        );
        assert_eq!(up.occlusion, 0.0);
        assert_eq!(up.bounce, Vec3::ZERO, "bounces = 0 ⇒ 反弹恒零");
        assert_eq!(up.total(), up.direct + up.sky);

        // 倾斜 60°（法线 = (0, 1, √3)/2 ⇒ NdotL 恰为 0.5）：直接光减半、
        // 天空不变（余弦加权估计量与朝向无关，这是开阔天空的强不变量）
        let tilt = Vec3::new(0.0, 1.0, 3.0f32.sqrt()).normalize();
        let tilted = bake_sample(&empty, Vec3::new(0.0, 1.0, 0.0), tilt, &params);
        assert_eq!(tilted.direct, params.light_color * 0.5);
        assert!(
            (tilted.sky - analytic_sky).length() < 1e-4,
            "天空项应与法线朝向无关，实际 {:?}",
            tilted.sky
        );
        // 背光面：直射是解析零（不发射线，因此不依赖任何采样运气）
        let down = bake_sample(&empty, Vec3::ZERO, Vec3::NEG_Y, &params);
        assert_eq!(down.direct, Vec3::ZERO);
    }

    /// 遮挡盒的影子里：直射 = 解析零、天空严格低于开阔值、AO 落在开区间。
    #[test]
    fn box_shadow_zeroes_direct_and_lowers_sky() {
        let params = BakeParams {
            light_dir: Vec3::new(-1.0, -4.0, 0.0),
            light_color: Vec3::new(1.0, 0.9, 0.8),
            sky_color: Vec3::new(0.2, 0.4, 0.6),
            shadow_softness: 0.0,
            sky_samples: 64,
            bounces: 0,
        };
        let open = scene(ground());
        let mut blocked = ground();
        blocked.extend(box_triangles(
            Vec3::new(-1.0, -1.0, -1.0),
            Vec3::new(1.0, 2.0, 1.0),
        ));
        let blocked = scene(blocked);
        // 盒顶 y = 2、光斜率 4:1 ⇒ 地面影带在 x ∈ [−1.49, −1]，取 −1.2
        let probe = Vec3::new(-1.2, 0.02, 0.0);
        let lit = bake_sample(&open, probe, Vec3::Y, &params);
        let shadow = bake_sample(&blocked, probe, Vec3::Y, &params);
        assert!(lit.direct.length() > 0.0, "参照点应在影外");
        assert_eq!(shadow.direct, Vec3::ZERO, "影子里的直射必须是解析零");
        assert!(
            shadow.sky.length() < lit.sky.length() && shadow.sky.length() > 0.0,
            "天空应被部分遮蔽：{:?} vs {:?}",
            shadow.sky,
            lit.sky
        );
        assert!(
            shadow.occlusion > 0.0 && shadow.occlusion < 1.0,
            "盒边 AO 应在开区间，实际 {}",
            shadow.occlusion
        );
    }

    /// 半空间解析对照（「面朝墙 vs 面朝天」）：单一大面上方 0.5 m 处，
    /// 法线朝下 ⇒ 64/64 全遮挡（occlusion 恰为 1、天空恰为 0）；
    /// 法线朝上 ⇒ 逐位 0 遮挡、天空恰为 π × sky_color。
    #[test]
    fn half_space_occlusion_is_analytic_zero_or_one() {
        let bvh = scene(ground());
        let params = BakeParams {
            sky_samples: 64,
            bounces: 1,
            ..Default::default()
        };
        let point = Vec3::new(0.0, 0.5, 0.0);
        let facing_floor = bake_sample(&bvh, point, Vec3::NEG_Y, &params);
        assert_eq!(facing_floor.occlusion, 1.0, "朝地半球应全被遮挡");
        assert_eq!(facing_floor.sky, Vec3::ZERO);
        assert_eq!(
            facing_floor.direct,
            Vec3::ZERO,
            "光自上而下、法线朝下 ⇒ 解析零"
        );
        assert!(
            facing_floor.bounce.length() > 0.0,
            "一次反弹把地面的入射光带回来"
        );

        let facing_sky = bake_sample(&bvh, point, Vec3::Y, &params);
        assert_eq!(facing_sky.occlusion, 0.0, "朝天半球无任何遮挡");
        assert!(
            (facing_sky.sky - params.sky_color * std::f32::consts::PI).length() < 1e-4,
            "开阔天空应为 π×sky，实际 {:?}",
            facing_sky.sky
        );
        assert_eq!(facing_sky.bounce, Vec3::ZERO, "无命中 ⇒ 无反弹");
    }

    /// 色溢的最小验证：2 m 高处朝下的采样点，其反弹项解析等于
    /// `红地板 albedo ⊙ (E_direct + π·E_sky)`——中性光 + 中性天空时逐样本
    /// 系数相同 ⇒ G/B 通道还须逐位相等。
    #[test]
    fn one_bounce_reproduces_hit_surface_albedo() {
        let red = Vec3::new(0.9, 0.1, 0.1);
        let floor = quad_at(Vec3::ZERO, Vec3::X * 100.0, Vec3::NEG_Z * 100.0, red);
        let bvh = scene(floor);
        let params = BakeParams {
            light_dir: Vec3::new(0.0, -1.0, 0.0),
            light_color: Vec3::splat(1.0),
            sky_color: Vec3::splat(0.5),
            shadow_softness: 0.0,
            sky_samples: 64,
            bounces: 1,
        };
        let point = Vec3::new(0.0, 2.0, 0.0);
        let sample = bake_sample(&bvh, point, Vec3::NEG_Y, &params);
        assert_eq!(sample.occlusion, 1.0, "全部样本都应打在红地板上");
        assert_eq!(sample.sky, Vec3::ZERO);
        let expected = red * (1.0 + std::f32::consts::PI * 0.5);
        assert!(
            (sample.bounce - expected).length() < 1e-4,
            "反弹应为 albedo×(E_direct + π·E_sky) = {expected:?}，实际 {:?}",
            sample.bounce
        );
        // 线性 RGB 按 R = x、G = y、B = z 对应（glam 无 r/g/b 访问器）
        assert!(
            sample.bounce.x > sample.bounce.y * 3.0,
            "色溢特征：反弹项被地板染红，实际 {:?}",
            sample.bounce
        );
        assert_eq!(
            sample.bounce.y.to_bits(),
            sample.bounce.z.to_bits(),
            "中性光 + 中性天空下每样本的 G/B 系数相同 ⇒ 应逐位相等"
        );
        // bounces = 0：反弹逐位零，其余分量不受影响（无副作用）
        let zero = BakeParams {
            bounces: 0,
            ..params
        };
        let plain = bake_sample(&bvh, point, Vec3::NEG_Y, &zero);
        assert_eq!(plain.bounce, Vec3::ZERO);
        assert_eq!(plain.direct, sample.direct);
        assert_eq!(plain.sky, sample.sky);
        assert_eq!(plain.occlusion, sample.occlusion);
    }

    /// 硬影 vs 软影：本影 0、半影带 = `k/8 × 全亮值` 的离散中间值、
    /// 全亮区与硬影逐位同值（锥只改可见性、不改余弦项的口径）。
    ///
    /// 几何：y = 2 处 x ∈ [−1, 1] 的 2×2 板（法线 −Y）、受光点在 y = 0 平面上；
    /// 锥半角 0.35 rad ⇒ 半影半径 = 2·tan(0.35) = 0.727 m ⇒ 全影 |x| ≤ 0.273、
    /// 半影带 x ∈ [0.273, 1.727]、x > 1.727 全亮。
    #[test]
    fn soft_shadow_gives_discrete_penumbra_and_matches_hard_elsewhere() {
        let bvh = scene(quad(Vec3::new(0.0, 2.0, 0.0), Vec3::X, Vec3::Z));
        let hard = BakeParams {
            shadow_softness: 0.0,
            sky_samples: 16,
            bounces: 0,
            ..Default::default()
        };
        let soft = BakeParams {
            shadow_softness: 0.35,
            ..hard
        };
        let penumbra_point = Vec3::new(0.9, 0.0, 0.0);
        let open = bake_sample(&Bvh::empty(), penumbra_point, Vec3::Y, &soft);
        assert_eq!(open.direct, Vec3::splat(1.0), "默认参数 = 竖直光 + 白光");
        let umbra = bake_sample(&bvh, Vec3::ZERO, Vec3::Y, &soft);
        assert_eq!(umbra.direct, Vec3::ZERO, "原点在全影内 ⇒ 软影也为 0");
        let penumbra = bake_sample(&bvh, penumbra_point, Vec3::Y, &soft);
        assert!(
            penumbra.direct.x > 0.0 && penumbra.direct.x < open.direct.x,
            "半影应为严格中间值，实际 {:?}",
            penumbra.direct
        );
        let eighths = penumbra.direct.x / (open.direct.x / SHADOW_SOFT_SAMPLES as f32);
        assert!(
            (eighths - eighths.round()).abs() < 1e-4,
            "半影必须落在 k/8 档上，实际 {eighths}"
        );
        assert!(
            eighths > 1.0 && eighths < 7.0,
            "探针应落在半影带中段，实际 k = {eighths}"
        );
        assert_eq!(
            bake_sample(&bvh, penumbra_point, Vec3::Y, &hard).direct,
            Vec3::ZERO,
            "同一点硬影（单条光心射线）应全挡"
        );
        let far_hard = bake_sample(&bvh, Vec3::new(5.0, 0.0, 0.0), Vec3::Y, &hard).direct;
        let far_soft = bake_sample(&bvh, Vec3::new(5.0, 0.0, 0.0), Vec3::Y, &soft).direct;
        assert_eq!(
            far_soft.to_array(),
            far_hard.to_array(),
            "全亮区软影与硬影须逐位同值"
        );
    }

    /// 同场景同参数两次烘焙（各自重新构建 BVH）⇒ 全部采样点所有字段逐位一致。
    #[test]
    fn bake_sample_is_bitwise_deterministic_across_runs() {
        let triangles = {
            let mut t = ground();
            t.extend(box_at(
                Vec3::new(-1.0, 0.0, -1.0),
                Vec3::new(1.0, 2.0, 1.0),
                Vec3::new(0.7, 0.2, 0.2),
            ));
            t.extend(scattered(4));
            t
        };
        let points: Vec<(Vec3, Vec3)> = (0..24u64)
            .map(|stream| {
                (
                    Vec3::new(
                        hash_unit(stream * 5 + 1) * 10.0 - 5.0,
                        hash_unit(stream * 5 + 2) * 3.0 + 0.5,
                        hash_unit(stream * 5 + 3) * 10.0 - 5.0,
                    ),
                    Vec3::new(
                        hash_unit(stream * 11 + 1) - 0.5,
                        hash_unit(stream * 11 + 2),
                        hash_unit(stream * 11 + 3) - 0.5,
                    )
                    .normalize(),
                )
            })
            .collect();
        let params = BakeParams {
            sky_samples: 32,
            shadow_softness: 0.3,
            bounces: 1,
            ..Default::default()
        };
        let first = bake_points(&build_bvh(triangles.clone()), &points, &params);
        let second = bake_points(&build_bvh(triangles.clone()), &points, &params);
        for (a, b) in first.iter().zip(&second) {
            assert_eq!(a.normal.to_array(), b.normal.to_array());
            assert_eq!(a.direct.to_array(), b.direct.to_array());
            assert_eq!(a.sky.to_array(), b.sky.to_array());
            assert_eq!(a.bounce.to_array(), b.bounce.to_array());
            assert_eq!(a.occlusion.to_bits(), b.occlusion.to_bits());
            assert_eq!(a.total().to_array(), b.total().to_array());
        }
        assert_eq!(first.len(), points.len());
        assert!(
            first.iter().any(|s| s.occlusion > 0.0),
            "测试场景应至少一处被遮挡（否则本测试没覆盖到遮蔽路径）"
        );
        assert!(
            first.iter().any(|s| s.bounce.length() > 0.0),
            "测试场景应至少一处有反弹命中"
        );
    }

    #[test]
    #[should_panic(expected = "v1 只支持 0/1 次反弹")]
    fn bake_rejects_more_than_one_bounce() {
        let params = BakeParams {
            bounces: 2,
            ..Default::default()
        };
        let _ = bake_sample(&Bvh::empty(), Vec3::ZERO, Vec3::Y, &params);
    }

    // ───────────────────────────── 世界封装 ─────────────────────────────

    fn bake_world() -> World {
        let mut world = World::new();
        world.insert_resource(MeshStore::new());
        world.insert_resource(MaterialStore::new());
        world
    }

    #[test]
    fn collect_bake_triangles_takes_static_meshes_only() {
        let mut world = bake_world();
        let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(2.0));
        let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
        let material = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.5, 0.5, 0.5, 1.0));
        let decor = world.spawn((
            GlobalTransform::from_transform(Transform::from_xyz(0.0, 0.0, 0.0)),
            plane,
            material,
        ));
        let dynamic = world.spawn((
            GlobalTransform::from_transform(Transform::from_xyz(4.0, 0.0, 0.0)),
            plane,
            material,
            RigidBody::default(),
        ));
        let kinematic = world.spawn((
            GlobalTransform::from_transform(Transform::from_xyz(8.0, 0.0, 0.0)),
            plane,
            material,
            RigidBody {
                body_type: BodyType::Kinematic,
                ..Default::default()
            },
        ));
        let static_collider_on_body = world.spawn((
            GlobalTransform::from_transform(Transform::from_xyz(12.0, 0.0, 0.0)),
            plane,
            material,
            RigidBody::default(),
            Collider::static_box(Vec3::ONE),
        ));
        let hidden = world.spawn((
            GlobalTransform::from_transform(Transform::from_xyz(16.0, 0.0, 0.0)),
            plane,
            material,
            Visibility::HIDDEN,
        ));
        let joint = world.spawn((GlobalTransform::from_transform(Transform::IDENTITY),));
        let skinned = world.spawn((
            GlobalTransform::from_transform(Transform::from_xyz(20.0, 0.0, 0.0)),
            plane,
            material,
            Skin::new(vec![joint], vec![Mat4::IDENTITY]),
        ));
        // 只挂 LodMesh3（R51 地形 chunk 的形状）：无 MeshHandle ⇒ 查询不命中
        let lod_only = world.spawn((
            GlobalTransform::from_transform(Transform::from_xyz(24.0, 0.0, 0.0)),
            material,
            crate::terrain::LodMesh3 {
                levels: [plane, plane, plane],
                switch_distances: [10.0, 20.0],
            },
        ));

        let triangles = collect_bake_triangles(&world);
        assert_eq!(count_of(&triangles, decor), 2, "无刚体的网格装饰 = 静态");
        assert_eq!(count_of(&triangles, dynamic), 0, "动态体不烘");
        assert_eq!(
            count_of(&triangles, kinematic),
            0,
            "运动学体由游戏驱动，不烘"
        );
        assert_eq!(
            count_of(&triangles, static_collider_on_body),
            2,
            "静态碰撞体 = 显式声明不动，照物理口径收录"
        );
        assert_eq!(count_of(&triangles, hidden), 0, "隐藏几何不遮光");
        assert_eq!(
            count_of(&triangles, skinned),
            0,
            "蒙皮网格不参与烘焙（v1 限制）"
        );
        assert_eq!(
            count_of(&triangles, lod_only),
            0,
            "只有 LodMesh3 句柄时不收集"
        );
        assert_eq!(triangles.len(), 4);
        // cube 句柄没被任何实体引用 ⇒ 三角总数里不含它（顺带钉住口径）
        let _ = cube;
    }

    /// R89：`BakeExcluded` 标记实体不进收集（编辑器叠加层语义）——
    /// 同世界带 / 不带标记 ⇒ 三角集差异可判别；缺席 = 收录（既有场景
    /// 逐位不变的收集面保证）。
    #[test]
    fn bake_excluded_marker_removes_geometry_from_collection() {
        let mut world = bake_world();
        let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(2.0));
        let material = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.5, 0.5, 0.5, 1.0));
        let decor = world.spawn((
            GlobalTransform::from_transform(Transform::from_xyz(0.0, 0.0, 0.0)),
            plane,
            material,
        ));
        // 编辑器叠加层的形态：普通渲染实体（可见、静态、带网格）+ 排除标记
        let overlay = world.spawn((
            GlobalTransform::from_transform(Transform::from_xyz(4.0, 0.0, 0.0)),
            plane,
            material,
            BakeExcluded,
        ));
        let with = collect_bake_triangles(&world);
        assert_eq!(
            count_of(&with, decor),
            2,
            "未标记实体照常收录（缺席 = 收录）"
        );
        assert_eq!(with.len(), 2, "标记实体整体出局（2 三角的 plane）");
        // 撤标记 ⇒ 同一实体回到收集（差异可判别，不是「查不到」式假通过）
        world.remove_component::<BakeExcluded>(overlay);
        let without = collect_bake_triangles(&world);
        assert_eq!(without.len(), 4, "撤标记后两个实体都收录（2 + 2 三角）");
        assert_eq!(count_of(&without, overlay), 2);
    }

    /// 变换应用：旋转后的顶点 / 法线解析值、非均匀缩放的法线矩阵口径
    /// （`M⁻ᵀ n` 而非 `M n`）、负缩放镜像时法线跟着环绕方向翻。
    #[test]
    fn collect_bake_triangles_applies_global_transform() {
        // (1) 地面绕 X 转 90°：平面落到 z = 0、法线 +Y → +Z
        let mut world = bake_world();
        let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(2.0));
        let rotated = world.spawn((
            GlobalTransform::from_transform(Transform {
                rotation: Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                ..Default::default()
            }),
            plane,
        ));
        let triangles = collect_bake_triangles(&world);
        assert_eq!(count_of(&triangles, rotated), 2);
        for tri in &triangles {
            assert_eq!(tri.normal.to_array()[2].signum(), 1.0, "法线应翻到 +Z");
            for v in &tri.vertices {
                assert!(v.z.abs() < 1e-6, "旋转后应落在 z = 0 平面，实际 {v:?}");
                assert!(v.x.abs() <= 1.0 + 1e-6 && v.y.abs() <= 1.0 + 1e-6, "{v:?}");
            }
        }

        // (2) 单三角、顶点法线 (−1,−1,0)/√2、非均匀缩放 (2,1,1)：
        //     法线矩阵给 normalize(n/s) = (−0.4472, −0.8944, 0)，
        //     直接把法线乘模型矩阵会得到 (−0.8944, −0.4472, 0)——可分辨
        let mut world = bake_world();
        let root2 = std::f32::consts::FRAC_1_SQRT_2;
        let mesh = MeshData {
            vertices: vec![
                Vertex {
                    position: [0.0, 0.0, 0.0],
                    normal: [-root2, -root2, 0.0],
                    uv: [0.0, 0.0],
                },
                Vertex {
                    position: [1.0, -1.0, 0.0],
                    normal: [-root2, -root2, 0.0],
                    uv: [1.0, 0.0],
                },
                Vertex {
                    position: [0.0, 0.0, 2.0],
                    normal: [-root2, -root2, 0.0],
                    uv: [0.0, 1.0],
                },
            ],
            indices: vec![0, 1, 2],
            skin: None,
            tangents: None,
            uv2: None,
            colors: None,
            morph: None,
        };
        let handle = world.resource_mut::<MeshStore>().add(mesh);
        let scaled = world.spawn((
            GlobalTransform::from_transform(Transform {
                scale: Vec3::new(2.0, 1.0, 1.0),
                ..Default::default()
            }),
            handle,
        ));
        let triangles = collect_bake_triangles(&world);
        assert_eq!(count_of(&triangles, scaled), 1);
        let tri = &triangles[0];
        assert_eq!(
            tri.vertices,
            [
                Vec3::ZERO,
                Vec3::new(2.0, -1.0, 0.0),
                Vec3::new(0.0, 0.0, 2.0)
            ]
        );
        let expected = Vec3::new(-0.447_213_6, -0.894_427_2, 0.0);
        assert!(
            (tri.normal.normalize() - expected).length() < 1e-5,
            "法线矩阵结果应为 {expected:?}，实际 {:?}",
            tri.normal.normalize()
        );

        // (3) 负缩放镜像：环绕方向翻转 ⇒ 法线跟着翻（否则镜像墙受光方向反）
        let mut world = bake_world();
        let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(2.0));
        let mirrored = world.spawn((
            GlobalTransform::from_transform(Transform {
                scale: Vec3::new(-1.0, 1.0, 1.0),
                ..Default::default()
            }),
            plane,
        ));
        let triangles = collect_bake_triangles(&world);
        assert_eq!(count_of(&triangles, mirrored), 2);
        for tri in &triangles {
            let geometric =
                (tri.vertices[1] - tri.vertices[0]).cross(tri.vertices[2] - tri.vertices[0]);
            assert!(
                tri.normal.dot(geometric) > 0.0,
                "镜像后法线必须与几何法线同号，实际 {:?} / {:?}",
                tri.normal,
                geometric
            );
        }
    }

    /// 反照率来自材质 `base_color`；无材质句柄 / 悬垂句柄 / 无 `MaterialStore`
    /// 资源三种缺省路径都落回白色（与渲染器的白色兜底同口径）。
    #[test]
    fn collect_bake_triangles_reads_material_albedo_and_survives_missing_stores() {
        let mut world = bake_world();
        let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(2.0));
        let material = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.8, 0.1, 0.2, 1.0));
        let with_material = world.spawn((
            GlobalTransform::from_matrix(Mat4::IDENTITY),
            plane,
            material,
        ));
        let without_material = world.spawn((
            GlobalTransform::from_transform(Transform::from_xyz(3.0, 0.0, 0.0)),
            plane,
        ));
        let dangling_material = world.spawn((
            GlobalTransform::from_transform(Transform::from_xyz(6.0, 0.0, 0.0)),
            plane,
            MaterialHandle(9_999),
        ));
        let triangles = collect_bake_triangles(&world);
        let albedo_of = |entity| {
            triangles
                .iter()
                .find(|t| t.entity == entity)
                .expect("应有该实体的三角")
                .albedo
        };
        assert_eq!(albedo_of(with_material), Vec3::new(0.8, 0.1, 0.2));
        assert_eq!(albedo_of(without_material), Vec3::ONE, "无材质 = 白");
        assert_eq!(albedo_of(dangling_material), Vec3::ONE, "悬垂句柄 = 白");
        assert!(
            triangles.iter().all(|t| t.normal.is_finite()),
            "收集结果不应有 NaN"
        );

        // 无 MeshStore 资源：返回空（组件注册了但目录不在 = 无可展开几何）
        let mut bare = World::new();
        bare.spawn((
            GlobalTransform::from_matrix(Mat4::IDENTITY),
            plane,
            material,
        ));
        assert!(collect_bake_triangles(&bare).is_empty());
    }

    // ───────────────────────────── 全链组装（R52b） ─────────────────────────────

    /// 全链两次烘焙逐位一致（收集 → BVH → chart 参数化 → 装箱 → texel
    /// 回溯 → 积分器 → 行主序回填）：房间场景 = 地板 + 侧墙 + 遮挡柱。
    /// 顺带钉住：`AtlasEntry::mesh` 由 `bake_lightmap` 补齐（纯装箱为
    /// None）、覆盖与未覆盖 texel 并存、柱影 / AO 产生辐照对比。
    #[test]
    fn bake_lightmap_is_bitwise_deterministic_across_runs() {
        use crate::bake_uv::BakeUvParams;

        let mut world = bake_world();
        let floor_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(6.0));
        let wall_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(6.0));
        let column_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
        let material = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.7, 0.7, 0.72, 1.0));
        let floor = world.spawn((
            GlobalTransform::from_matrix(Mat4::IDENTITY),
            floor_mesh,
            material,
        ));
        // 侧墙：plane 绕 X 转 90°（法线 +Y → +Z）、局部 z 缩放 0.5 ⇒ 高 3 m
        let wall = world.spawn((
            GlobalTransform::from_transform(Transform {
                position: Vec3::new(0.0, 1.5, -3.0),
                rotation: Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                scale: Vec3::new(1.0, 1.0, 0.5),
            }),
            wall_mesh,
            material,
        ));
        let column = world.spawn((
            GlobalTransform::from_transform(Transform {
                position: Vec3::new(1.2, 1.0, 0.8),
                scale: Vec3::new(0.6, 2.0, 0.6),
                ..Default::default()
            }),
            column_mesh,
            material,
        ));
        let params = BakeParams {
            light_dir: Vec3::new(-0.6, -1.0, 0.25),
            light_color: Vec3::new(2.0, 1.8, 1.5),
            sky_color: Vec3::new(0.25, 0.3, 0.45),
            shadow_softness: 0.2,
            sky_samples: 16,
            bounces: 1,
        };
        let uv = BakeUvParams {
            texels_per_meter: 4.0,
            atlas_size: 64,
        };
        let first = bake_lightmap(&world, &params, &uv);
        let second = bake_lightmap(&world, &params, &uv);
        assert_eq!(first.radiance.len(), 64 * 64, "行主序、与 size² 对齐");
        assert_eq!(first.atlas.size, 64);
        assert_eq!(first.atlas.entries.len(), second.atlas.entries.len());
        assert_eq!(first.atlas.used_height, second.atlas.used_height);
        for (a, b) in first.atlas.entries.iter().zip(&second.atlas.entries) {
            assert_eq!(a.entity, b.entity);
            assert_eq!(a.mesh, b.mesh);
            assert_eq!(a.axis, b.axis);
            assert_eq!(a.origin, b.origin);
            assert_eq!(a.size, b.size);
            assert_eq!(a.uv_rect, b.uv_rect);
            assert_eq!(a.triangles.len(), b.triangles.len());
            for (ta, tb) in a.triangles.iter().zip(&b.triangles) {
                assert_eq!(ta.triangle, tb.triangle);
                assert_eq!(ta.uv_rect, tb.uv_rect);
                assert_eq!(ta.local, tb.local);
            }
        }
        for (a, b) in first.radiance.iter().zip(&second.radiance) {
            assert_eq!(
                a.to_array().map(f32::to_bits),
                b.to_array().map(f32::to_bits),
                "两次烘焙的逐 texel 辐照必须逐位一致"
            );
        }
        // sanity：三实体全部入册、mesh 句柄补齐
        let entities: Vec<Entity> = first.atlas.entries.iter().map(|e| e.entity).collect();
        for expected in [floor, wall, column] {
            assert!(entities.contains(&expected), "实体 {expected:?} 应入册");
        }
        assert!(
            first.atlas.entries.iter().all(|e| e.mesh.is_some()),
            "bake_lightmap 须补齐 mesh 句柄"
        );
        // sanity：覆盖与未覆盖并存、阴影 / AO 产生对比
        let nonzero: Vec<Vec3> = first
            .radiance
            .iter()
            .copied()
            .filter(|c| *c != Vec3::ZERO)
            .collect();
        assert!(
            nonzero.len() > 500,
            "覆盖 texel 应数百（地板 24² + 墙 24×12 + 柱），实际 {}",
            nonzero.len()
        );
        assert!(
            nonzero.len() < first.radiance.len(),
            "须存在未覆盖（恒零）texel"
        );
        let max = nonzero.iter().map(|c| c.length()).fold(0.0f32, f32::max);
        let min = nonzero
            .iter()
            .map(|c| c.length())
            .fold(f32::INFINITY, f32::min);
        assert!(
            max > 2.0 * min,
            "柱影 / AO 应产生 ≥ 2× 辐照对比：max {max}、min {min}"
        );
    }
}
