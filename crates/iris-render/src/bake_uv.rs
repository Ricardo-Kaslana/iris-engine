//! 烘焙光照（R52b：三阶段之二）——UV2 参数化与图集布局。
//!
//! 把 [`bake`](crate::bake)（R52a）的「世界空间采样点列表」接口接到
//! **光照贴图 texel** 上：世界几何 → chart 参数化（主轴投影）→ 图集
//! 装箱（shelf）→ texel 世界位回溯 → 逐 texel 辐照度（复用 R52a 的
//! [`bake_points`](crate::bake::bake_points) / BVH）→ 图集预览导出。
//! 组装入口是 [`BakedLightmap`](crate::bake::BakedLightmap) /
//! [`bake_lightmap`](crate::bake::bake_lightmap)（按规范放在 `bake.rs`）。
//!
//! # chart 参数化（主轴投影，v1 = 解析式展开，非共形）
//!
//! **chart 粒度 = (实体, 主轴)**。规范草稿「每 (实体, mesh 句柄) 一个
//! chart」在本引擎里等价于「每实体一个」`MeshHandle`（组件唯一），但同一
//! 实体的不同朝向三角形投影平面不同（盒子的顶面与侧面），必须拆——故
//! 每实体按 [`classify_axis`] 至多拆 3 个 chart。
//!
//! **分类是完备划分**（[`classify_axis`]）：`|n.y| > 0.7`
//! （[`FLOOR_NORMAL_THRESHOLD`]）→ 水平面 [`ChartAxis::Floor`]（投影 XZ）；
//! 否则墙面——`|n.x| ≥ |n.z|` → [`ChartAxis::WallX`]（投影 ZY），
//! 否则 [`ChartAxis::WallZ`]（投影 XY）。并列（`|n.x| == |n.z|`，45° 墙角）
//! 取 X（低轴号，与 BVH 分割轴的并列规则同风）。任何单位法线恰落一类：
//! 水平类 `|n.y| > 0.7`；墙面类被丢弃轴分量的下界
//! `max(|n.x|,|n.z|) ≥ sqrt((1 − 0.49)/2) ≈ 0.505` ⇒ **投影面积 ≥ 0.5 ×
//! 世界面积**，投影永不退化（回溯的 barycentric 分母有下界）。
//!
//! chart 内 UV = 投影坐标（米）× [`BakeUvParams::texels_per_meter`]，
//! 平移到 chart 局部系（bbox 最小角落在 `CHART_PAD_TEXELS` 内边距处）——
//! UV 与投影坐标是**纯线性**关系（缩放 + 平移，单测钉住解析值）。
//!
//! # 投影重叠（串光风险）与贪心偏移
//!
//! 同 chart 内两个三角形投影**正面积重叠** = 同一 texel 有两个世界位候选
//! （双层地板 / 盒子 ±X 两面投到同一 ZY 平面）——不处理则后者的 texel 被
//! 前者顶掉（串光）。v1 方案：按 chart 内三角形序（= 输入序，确定性）
//! 逐个放置，与已放置三角形正面积重叠则沿 chart 局部 **+x 贪心偏移**
//! （跳到冲突者右缘 + [`OVERLAP_GAP_TEXELS`]）直到空位；chart 尺寸取
//! 偏移后全体 bbox ⇒ 「放不下」不存在（矩形自然放大）。终止性：每轮偏移
//! 把 `min_x` 推到至少一个已放置三角形的 `max_x` 之右，已放置集合有限 ⇒
//! 至多 n 轮。
//!
//! **重叠判定 = 正面积相交**（顶点严格内点测试 + 边对严格穿越测试，
//! 边界相触 / 共边不算）——**不是 bbox 相交**：quad 的两个三角恒共斜边、
//! bbox 大面积重叠，按 bbox 判定会把每个 quad 撕成两块（面积翻倍 + 图集
//! 碎片化）；共边相邻在 texel 归属上由「先者得」解决（见下），不是串光。
//!
//! # 图集装箱（shelf，面积启发）
//!
//! [`pack_atlas`]：全部 chart 按 `(面积降序, 实体位序升序, 轴序号升序,
//! 原 chart 序)` **全序**排序（面积是 `u32 × u32` 精确整数，无浮点比较），
//! shelf 算法逐层放置（层内从左到右、放不下开新层、层高 = 该层最高 chart），
//! 任何 chart 超出图集尺寸即 panic（**尺寸不足报错语义**，调用方加大
//! `atlas_size` 或调低 `texels_per_meter`）。输出 [`LightmapAtlas`]：
//! 每 chart 一条 [`AtlasEntry`]（实体 / mesh 句柄 / chart 矩形 uv_rect /
//! texel 原点与尺寸）+ **每三角形一条** [`AtlasTriangle`]（自身图集矩形 +
//! chart 局部三顶点坐标——回溯直接查表，不再走 chart 中间结构）。
//! 利用率口径 [`LightmapAtlas::utilization`] = Σ chart 面积 ÷
//! (图集宽 × **已用高度**)：末层之下的整片空白是调用方 `atlas_size` 的选择，
//! 不是装箱算法的浪费（规则网格实测 0.75，单测钉住 > 0.5）。
//!
//! # texel 世界位回溯（[`bake_texels`]）
//!
//! 逐 entry 逐三角形光栅化其图集 bbox：texel 中心 → chart 局部坐标 →
//! 2D 重心（投影是仿射 ⇒ 2D 重心 = 3D 重心）→ 世界位 = 重心 · 世界顶点、
//! 法线 = **三角面法线**（v1 不做顶点法线插值：chart 投影用面法线分类、
//! 烘焙采样也用面法线，口径自洽；光滑表面的法线插值留 c 阶段评估）。
//!
//! **归属规则（确定性）**：点在三角形内 = 三条边函数与面积同号且
//! `≥ −1e-4`（**含边界**）；一个 texel 命中多个三角形时**先者得**
//! （entry 序 = 装箱序、三角形序 = chart 内输入序）——quad 斜边上的 texel
//! 恒归序号小的三角（单测钉住 8×8 格点的 36/28 解析计数）。未覆盖 texel
//! 不产出源、辐照度恒 `Vec3::ZERO`（[`bake_lightmap`] 里零填充）。
//!
//! 输出 [`TexelSource`] 按 **texel 行主序**升序（`index = y × size + x`，
//! v 与行序一致：行 0 = v 0）——这就是 R52a 预留的确定性切分点：逐 texel
//! [`bake_points`](crate::bake::bake_points) 按序拼接，分段并行不破确定性。
//!
//! # 图集预览（[`write_lightmap_preview`]）
//!
//! 辐照 → [`aces_tonemap`]（Narkowicz 系数，与 `shaders/blit.wgsl` 的
//! `aces_tonemap` **同款公式**——CPU 侧独立实现，单测对照解析值）→
//! sRGB 编码（blit 输出线性色、由 sRGB 目标格式做硬件编码；CPU 预览必须
//! 显式做同款编码，否则图偏暗）→ PPM **P6**（二进制 RGB8）。示例用
//! `ffmpeg -y -i *.ppm docs/bake_preview.png` 转 PNG。
//!
//! # v1 限制（诚实清单）
//!
//! - **主轴投影不是共形展开**：曲面（球 / 圆柱）按面法线逐三角分类后投影
//!   有拉伸与撕裂（相邻三角形分到不同轴时 UV 不连续）；解析式展开对
//!   平面 / 盒状建筑足够，LSCM / xatlas 类参数化在路线图；
//! - chart 内重叠偏移是**贪心 +x**（非最优装箱），双层地板等重叠几何的
//!   chart 面积可膨胀至 2× 以上；
//! - 无 chart 旋转（主轴投影已轴对齐，旋转收益留给共形参数化再谈）；
//! - texel 法线 = 面法线（无顶点法线插值，光滑曲面上 AO 有 faceting）；
//! - `texels_per_meter` 全图集单一值（无按实体 / 按重要度分配分辨率）；
//! - 内边距 [`CHART_PAD_TEXELS`] = 1 texel：c 阶段双线性采样的 bleed 防护
//!   靠 pad + **dilation 填充**（R52c 已由
//!   [`dilate_atlas`](crate::lightmap::dilate_atlas) 闭合：未覆盖 texel 按
//!   4 邻域均值扩散填充，黑 pad 不再被双线性拉进 chart 边缘）；
//! - 只认 [`MeshHandle`](crate::mesh::MeshHandle) 静态几何（R52a 收集
//!   口径原样复用：蒙皮 / 隐藏 / LOD 句柄几何不参与）。
//!
//! **c 阶段（R52c，已落地，[`lightmap`](crate::lightmap) 模块）**：GPU 纹理
//! 上传（Rgba16Float 单 mip + 自带 IEEE 754 half 转换）、`MeshData` UV2
//! 顶点属性（loc13 独立缓冲）与逐实体注入（跨 chart 冲突按需拆分顶点）、
//! shader 采样（间接项 `select` 替换 IBL 环境项 = 「静态 GI 补动态直射」）、
//! dilation 填充。**仍未做**：BC 压缩上传、按实体 / 重要度分配分辨率、
//! mipmap 链（清单见 `lightmap.rs` 模块文档与 ARCHITECTURE §8.31）。

use std::collections::BTreeMap;
use std::io::{BufWriter, Write};
use std::path::Path;

use glam::{Vec2, Vec3};
use iris_core::prelude::Entity;

use crate::bake::{BakeTriangle, BakedLightmap};
use crate::mesh::MeshHandle;

/// 水平面分类阈值：`|n.y| > 0.7`（≈ 45.6° 倾角以内）判水平。完备性论证
/// 见模块文档（墙面类被丢弃轴分量 ≥ 0.505 ⇒ 投影面积 ≥ 0.5 × 世界面积）。
pub const FLOOR_NORMAL_THRESHOLD: f32 = 0.7;

/// chart 四周内边距（texel）：c 阶段双线性采样的 bleed 防护（见模块文档
/// v1 限制——黑 pad 不做 dilation）。
pub const CHART_PAD_TEXELS: u32 = 1;

/// 重叠偏移后三角形之间的水平间隙（texel）：两侧各留 1 texel 的 bleed
/// 防护，与 [`CHART_PAD_TEXELS`] 同口径。
pub const OVERLAP_GAP_TEXELS: f32 = 2.0;

/// 点在三角形内的边界带宽（texel²，边函数值的绝对容差）：含边界归属 +
/// 先者得（模块文档「texel 世界位回溯」）。
const EDGE_EPS: f32 = 1e-4;

/// chart 的投影平面（分类口径与完备性见 [`classify_axis`]）。
///
/// 派生 `Ord` 的声明序（Floor < WallX < WallZ）参与装箱 tie-break 与
/// chart 分组键排序——声明序即语义序，勿重排。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ChartAxis {
    /// 水平面（`|n.y| > 0.7`）：投影 XZ，`(u, v) = (x, z)`。
    Floor,
    /// X 主导墙面：投影 ZY，`(u, v) = (z, y)`。
    WallX,
    /// Z 主导墙面：投影 XY，`(u, v) = (x, y)`。
    WallZ,
}

/// 世界法线 → 投影平面分类（完备划分 + 并列取低轴号，确定性论证见
/// 模块文档）。
pub fn classify_axis(normal: Vec3) -> ChartAxis {
    assert!(
        normal.is_finite() && normal.length_squared() > 0.0,
        "分类法线非法：{normal:?}"
    );
    let n = normal.normalize();
    if n.y.abs() > FLOOR_NORMAL_THRESHOLD {
        ChartAxis::Floor
    } else if n.x.abs() >= n.z.abs() {
        ChartAxis::WallX
    } else {
        ChartAxis::WallZ
    }
}

/// 主轴投影（米制 2D 坐标，未乘 `texels_per_meter`）。
fn project_point(p: Vec3, axis: ChartAxis) -> [f32; 2] {
    match axis {
        ChartAxis::Floor => [p.x, p.z],
        ChartAxis::WallX => [p.z, p.y],
        ChartAxis::WallZ => [p.x, p.y],
    }
}

/// UV2 参数化与图集参数。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BakeUvParams {
    /// 光照贴图分辨率（texel / 米）。全图集单一值（v1 限制）。
    pub texels_per_meter: f32,
    /// 图集边长（texel，正方形）。chart 或 shelf 放不下即 panic。
    pub atlas_size: u32,
}

impl Default for BakeUvParams {
    fn default() -> Self {
        Self {
            texels_per_meter: 4.0,
            atlas_size: 512,
        }
    }
}

impl BakeUvParams {
    fn validate(&self) {
        assert!(
            self.texels_per_meter.is_finite() && self.texels_per_meter > 0.0,
            "texels_per_meter 须为正有限值：{}",
            self.texels_per_meter
        );
        assert!(self.atlas_size >= 1, "图集尺寸须 ≥ 1：{}", self.atlas_size);
    }
}

/// 一个 chart：同实体 + 同投影轴的三角形组，已完成投影、重叠偏移与
/// chart 局部平移（局部坐标单位 = texel，原点 = chart 角）。
#[derive(Clone, Debug, PartialEq)]
pub struct Chart {
    /// 来源实体。
    pub entity: Entity,
    /// 投影平面。
    pub axis: ChartAxis,
    /// 成员三角形（**输入三角数组**的下标，升序 = 输入序）。
    pub triangles: Vec<usize>,
    /// 每三角形三顶点的 chart 局部坐标（texel，含重叠偏移；与
    /// `triangles` 等长同序）。
    pub local: Vec<[[f32; 2]; 3]>,
    /// chart 尺寸（texel，投影 bbox 向上取整 + 2 × [`CHART_PAD_TEXELS`]）。
    pub size: [u32; 2],
    /// 分辨率口径（回溯 / c 阶段换算米制用）。
    pub texels_per_meter: f32,
}

/// 二维叉积（`u × v` 的 z 分量）。
fn cross2(u: [f32; 2], v: [f32; 2]) -> f32 {
    u[0] * v[1] - u[1] * v[0]
}

fn sub2(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

/// 有向面积 × 2（正 = CCW）。
fn area2(t: &[[f32; 2]; 3]) -> f32 {
    cross2(sub2(t[1], t[0]), sub2(t[2], t[0]))
}

/// 点是否在三角形内（**含边界**：边函数与面积同号且 ≥ −[`EDGE_EPS`]）。
/// 归属并列由调用方的「先者得」解决（模块文档）。
fn point_in_triangle(p: [f32; 2], t: &[[f32; 2]; 3]) -> bool {
    let a2 = area2(t);
    if a2.abs() <= EDGE_EPS {
        return false;
    }
    let s = a2.signum();
    cross2(sub2(t[1], t[0]), sub2(p, t[0])) * s >= -EDGE_EPS
        && cross2(sub2(t[2], t[1]), sub2(p, t[1])) * s >= -EDGE_EPS
        && cross2(sub2(t[0], t[2]), sub2(p, t[2])) * s >= -EDGE_EPS
}

/// 点是否**严格**在三角形内（三边函数同号且 > [`EDGE_EPS`]，边界不算）：
/// 重叠检测的顶点分支——共边相邻三角形的顶点落在边上，必须判否。
fn point_strictly_in_triangle(p: [f32; 2], t: &[[f32; 2]; 3]) -> bool {
    let a2 = area2(t);
    if a2.abs() <= EDGE_EPS {
        return false;
    }
    let s = a2.signum();
    cross2(sub2(t[1], t[0]), sub2(p, t[0])) * s > EDGE_EPS
        && cross2(sub2(t[2], t[1]), sub2(p, t[1])) * s > EDGE_EPS
        && cross2(sub2(t[0], t[2]), sub2(p, t[2])) * s > EDGE_EPS
}

/// 两线段是否**严格穿越**（交点在双方内部，端点相触 / 共线重叠判否）。
fn segments_properly_cross(a0: [f32; 2], a1: [f32; 2], b0: [f32; 2], b1: [f32; 2]) -> bool {
    let ab = sub2(b1, b0);
    let d1 = cross2(ab, sub2(a0, b0));
    let d2 = cross2(ab, sub2(a1, b0));
    let ba = sub2(a1, a0);
    let d3 = cross2(ba, sub2(b0, a0));
    let d4 = cross2(ba, sub2(b1, a0));
    ((d1 > EDGE_EPS && d2 < -EDGE_EPS) || (d1 < -EDGE_EPS && d2 > EDGE_EPS))
        && ((d3 > EDGE_EPS && d4 < -EDGE_EPS) || (d3 < -EDGE_EPS && d4 > EDGE_EPS))
}

/// 两三角形是否**正面积重叠**（串光判定）。
///
/// = 任一顶点严格落在对方内部，或任一边对严格穿越。边界相触（共边 /
/// 顶点在边上）判否——quad 的两半恒共斜边，判真会把每个 quad 撕开
/// （模块文档「投影重叠」）。
fn triangles_overlap(a: &[[f32; 2]; 3], b: &[[f32; 2]; 3]) -> bool {
    for v in a {
        if point_strictly_in_triangle(*v, b) {
            return true;
        }
    }
    for v in b {
        if point_strictly_in_triangle(*v, a) {
            return true;
        }
    }
    let edges_a = [(a[0], a[1]), (a[1], a[2]), (a[2], a[0])];
    let edges_b = [(b[0], b[1]), (b[1], b[2]), (b[2], b[0])];
    for (a0, a1) in edges_a {
        for (b0, b1) in edges_b {
            if segments_properly_cross(a0, a1, b0, b1) {
                return true;
            }
        }
    }
    false
}

fn min_x(t: &[[f32; 2]; 3]) -> f32 {
    t[0][0].min(t[1][0]).min(t[2][0])
}

fn max_x(t: &[[f32; 2]; 3]) -> f32 {
    t[0][0].max(t[1][0]).max(t[2][0])
}

/// 三角形组 → chart：投影 + 重叠偏移 + 局部平移 + 尺寸（算法与确定性
/// 论证见模块文档）。
fn chart_of(
    triangles: &[BakeTriangle],
    entity: Entity,
    axis: ChartAxis,
    members: Vec<usize>,
    params: &BakeUvParams,
) -> Chart {
    let tpm = params.texels_per_meter;
    let mut projected: Vec<[[f32; 2]; 3]> = members
        .iter()
        .map(|&i| {
            let tri = &triangles[i];
            [0, 1, 2].map(|k| {
                let p = project_point(tri.vertices[k], axis);
                [p[0] * tpm, p[1] * tpm]
            })
        })
        .collect();
    // 重叠偏移：按输入序逐个放置，正面积冲突则 +x 跳到冲突者右缘之外
    let mut placed: Vec<[[f32; 2]; 3]> = Vec::with_capacity(projected.len());
    for t in &mut projected {
        while let Some(conflict) = placed.iter().find(|p| triangles_overlap(t, p)) {
            let shift = max_x(conflict) - min_x(t) + OVERLAP_GAP_TEXELS;
            for v in t.iter_mut() {
                v[0] += shift;
            }
        }
        placed.push(*t);
    }
    // bbox → 局部平移（min 角落在 pad 处）+ 尺寸（向上取整 + 2 × pad）
    let mut min = [f32::INFINITY; 2];
    let mut max = [f32::NEG_INFINITY; 2];
    for t in &projected {
        for v in t {
            min = [min[0].min(v[0]), min[1].min(v[1])];
            max = [max[0].max(v[0]), max[1].max(v[1])];
        }
    }
    let pad = CHART_PAD_TEXELS as f32;
    let local = projected
        .iter()
        .map(|t| [0, 1, 2].map(|k| [t[k][0] - min[0] + pad, t[k][1] - min[1] + pad]))
        .collect();
    let size = [
        ((max[0] - min[0]).ceil() as u32).max(1) + 2 * CHART_PAD_TEXELS,
        ((max[1] - min[1]).ceil() as u32).max(1) + 2 * CHART_PAD_TEXELS,
    ];
    Chart {
        entity,
        axis,
        triangles: members,
        local,
        size,
        texels_per_meter: tpm,
    }
}

/// 三角形数组 → chart 列表（每 (实体, 主轴) 一个）。
///
/// 分组用 `BTreeMap<(实体位, 轴)>`（选型理由：迭代序 = 键序 = 实体位升序，
/// 与输入序无关地确定；输入序本身已确定，两者一致，见 AGENTS.md 确定性
/// 文化）。chart 内三角形序 = 输入序升序。
pub fn build_charts(triangles: &[BakeTriangle], params: &BakeUvParams) -> Vec<Chart> {
    params.validate();
    let mut groups: BTreeMap<(u64, ChartAxis), Vec<usize>> = BTreeMap::new();
    for (index, tri) in triangles.iter().enumerate() {
        groups
            .entry((tri.entity.to_bits(), classify_axis(tri.normal)))
            .or_default()
            .push(index);
    }
    groups
        .into_iter()
        .map(|((bits, axis), members)| {
            chart_of(triangles, Entity::from_bits(bits), axis, members, params)
        })
        .collect()
}

/// 图集内一个三角形的回溯表项。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtlasTriangle {
    /// **输入三角数组**（[`build_charts`] 的入参）下标。
    pub triangle: usize,
    /// 自身在图集内的矩形（归一化 `u0, v0, u1, v1`，v 与 texel 行序一致）：
    /// c 阶段 UV2 写回 / 调试可视化直接查表。
    pub uv_rect: [f32; 4],
    /// 三顶点的 chart 局部坐标（texel，含重叠偏移；与 `uv_rect` 同一份
    /// 数据的两种口径——回溯用局部坐标算重心，避免归一化往返）。
    pub local: [[f32; 2]; 3],
}

/// 图集内一个 chart 的表项。
#[derive(Clone, Debug, PartialEq)]
pub struct AtlasEntry {
    /// 来源实体。
    pub entity: Entity,
    /// 来源网格句柄。[`pack_atlas`] 纯函数不查世界 ⇒ 恒 `None`；
    /// [`bake_lightmap`](crate::bake::bake_lightmap) 组装时补齐（c 阶段
    /// UV2 写回按此索引网格）。
    pub mesh: Option<MeshHandle>,
    /// 投影平面。
    pub axis: ChartAxis,
    /// chart 矩形（归一化 `u0, v0, u1, v1`）。
    pub uv_rect: [f32; 4],
    /// chart 在图集内的 texel 原点。
    pub origin: [u32; 2],
    /// chart 尺寸（texel，含 [`CHART_PAD_TEXELS`] 内边距）。
    pub size: [u32; 2],
    /// 分辨率口径（与 chart 同值，冗余存放让 entry 自足）。
    pub texels_per_meter: f32,
    /// 成员三角形表项（序 = chart 内输入序）。
    pub triangles: Vec<AtlasTriangle>,
}

/// 光照贴图图集：正方形 `size × size` texel + chart 表项（装箱序 =
/// 面积降序全序，见 [`pack_atlas`]）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LightmapAtlas {
    /// 图集边长（texel）。
    pub size: u32,
    /// shelf 已用高度（texel）：最后一层的 `y + 层高`；空图集 = 0。
    pub used_height: u32,
    /// chart 表项（装箱放置序）。
    pub entries: Vec<AtlasEntry>,
}

impl LightmapAtlas {
    /// 装箱利用率 = Σ chart 面积 ÷ (图集宽 × 已用高度)。
    ///
    /// 分母不含末层以下的整片空白——那是调用方 `atlas_size` 的选择而非
    /// 装箱算法的浪费（口径论证见模块文档）。空图集 = 0.0。
    pub fn utilization(&self) -> f32 {
        if self.entries.is_empty() {
            return 0.0;
        }
        let used: u64 = self
            .entries
            .iter()
            .map(|e| u64::from(e.size[0]) * u64::from(e.size[1]))
            .sum();
        used as f32 / (self.size as f32 * self.used_height.max(1) as f32)
    }
}

/// shelf 装箱：chart 列表 → 图集（消费 charts，投影数据搬进表项）。
///
/// 排序全序 = `(面积降序, 实体位升序, 轴序号升序, 原 chart 序)`——面积是
/// 精确整数比较（`u32 × u32 → u64`），无浮点 tie 问题；同输入两次装箱
/// 逐位一致（单测锁定）。chart 超出图集或 shelf 高度耗尽 ⇒ panic
/// （**尺寸不足报错语义**：加大 `atlas_size` 或调低 `texels_per_meter`）。
pub fn pack_atlas(charts: Vec<Chart>, atlas_size: u32) -> LightmapAtlas {
    assert!(atlas_size >= 1, "图集尺寸须 ≥ 1：{atlas_size}");
    let mut order: Vec<(u64, u64, ChartAxis, usize)> = charts
        .iter()
        .enumerate()
        .map(|(i, c)| {
            (
                u64::from(c.size[0]) * u64::from(c.size[1]),
                c.entity.to_bits(),
                c.axis,
                i,
            )
        })
        .collect();
    order.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
            .then(a.3.cmp(&b.3))
    });
    let inv = 1.0 / atlas_size as f32;
    let (mut cursor_x, mut shelf_y, mut shelf_h) = (0u32, 0u32, 0u32);
    let mut entries = Vec::with_capacity(charts.len());
    for &(_, _, _, ci) in &order {
        let chart = &charts[ci];
        let [w, h] = chart.size;
        assert!(
            w <= atlas_size && h <= atlas_size,
            "图集尺寸不足：chart {w}×{h}（实体 {:?} / {:?}）超出图集 {atlas_size}×{atlas_size}",
            chart.entity,
            chart.axis
        );
        if cursor_x + w > atlas_size {
            shelf_y += shelf_h;
            cursor_x = 0;
            shelf_h = 0;
        }
        assert!(
            shelf_y + h <= atlas_size,
            "图集尺寸不足：shelf 已用到 y = {shelf_y}，新层高 {h} 超出图集 \
             {atlas_size}×{atlas_size}（加大 atlas_size 或调低 texels_per_meter）"
        );
        let triangles = chart
            .triangles
            .iter()
            .zip(&chart.local)
            .map(|(&ti, local)| {
                let mut min = [f32::INFINITY; 2];
                let mut max = [f32::NEG_INFINITY; 2];
                for v in local {
                    min = [min[0].min(v[0]), min[1].min(v[1])];
                    max = [max[0].max(v[0]), max[1].max(v[1])];
                }
                let rect = |lo: [f32; 2], hi: [f32; 2]| {
                    [
                        (cursor_x as f32 + lo[0]) * inv,
                        (shelf_y as f32 + lo[1]) * inv,
                        (cursor_x as f32 + hi[0]) * inv,
                        (shelf_y as f32 + hi[1]) * inv,
                    ]
                };
                AtlasTriangle {
                    triangle: ti,
                    uv_rect: rect(min, max),
                    local: *local,
                }
            })
            .collect();
        entries.push(AtlasEntry {
            entity: chart.entity,
            mesh: None,
            axis: chart.axis,
            uv_rect: [
                cursor_x as f32 * inv,
                shelf_y as f32 * inv,
                (cursor_x + w) as f32 * inv,
                (shelf_y + h) as f32 * inv,
            ],
            origin: [cursor_x, shelf_y],
            size: [w, h],
            texels_per_meter: chart.texels_per_meter,
            triangles,
        });
        cursor_x += w;
        shelf_h = shelf_h.max(h);
    }
    LightmapAtlas {
        size: atlas_size,
        used_height: shelf_y + shelf_h,
        entries,
    }
}

/// 一个被覆盖 texel 的回溯结果（= R52a [`bake_points`]
/// (crate::bake::bake_points) 的输入点 + 来源标注）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TexelSource {
    /// texel 行主序下标（`= texel[1] × size + texel[0]`，升序输出）。
    pub index: usize,
    /// texel 坐标 `(x, y)`（y = 行，行 0 = v 0）。
    pub texel: [u32; 2],
    /// 世界空间采样位（重心插值，精确落在三角面上——阴影偏置由
    /// [`bake_sample`](crate::bake::bake_sample) 负责，此处不加）。
    pub position: Vec3,
    /// 世界法线（**三角面法线**，已归一化；v1 不做顶点法线插值）。
    pub normal: Vec3,
    /// 三角反照率（来自 [`BakeTriangle`]，c 阶段调试 / 预览染色备用）。
    pub albedo: Vec3,
    /// 来源三角形（输入三角数组下标）。
    pub triangle: usize,
    /// 2D 重心 `(u, v)` = 顶点 1 / 2 的权重（与 [`BvhHit::barycentric`]
    /// (crate::bake::BvhHit::barycentric) 同口径；顶点 0 权重 = 1 − u − v）。
    pub barycentric: Vec2,
}

/// 2D 重心坐标 `(λ1, λ2)`（λ0 = 1 − λ1 − λ2）。投影是仿射映射 ⇒
/// 2D 重心 = 3D 重心，世界位可直接用输入三角的世界顶点复原。
fn barycentric_2d(p: [f32; 2], t: &[[f32; 2]; 3]) -> (f32, f32) {
    let a2 = area2(t);
    assert!(
        a2.abs() > 1e-12,
        "投影三角退化（零面积）——分类的投影面积下界（≥ 0.5 × 世界面积）\
         应已排除此情况，出现即上游 bug"
    );
    (
        cross2(sub2(p, t[0]), sub2(t[2], t[0])) / a2,
        cross2(sub2(t[1], t[0]), sub2(p, t[0])) / a2,
    )
}

/// texel 世界位回溯：图集 + 输入三角数组 → 被覆盖 texel 的采样点列表
/// （**texel 行主序升序**；归属规则「含边界 + 先者得」见模块文档）。
///
/// 这就是 R52a 预留的接口对接点：输出的 `(position, normal)` 序列直接喂
/// [`bake_points`](crate::bake::bake_points)，分段并行按 `index` 切分保序。
pub fn bake_texels(atlas: &LightmapAtlas, triangles: &[BakeTriangle]) -> Vec<TexelSource> {
    let size = atlas.size;
    let mut slots: Vec<Option<TexelSource>> = vec![None; (size as usize) * (size as usize)];
    for entry in &atlas.entries {
        let origin = [entry.origin[0] as f32, entry.origin[1] as f32];
        for at in &entry.triangles {
            let tri = triangles
                .get(at.triangle)
                .expect("AtlasTriangle.triangle 越界：图集与三角数组不配套");
            assert!(
                tri.normal.is_finite() && tri.normal.length_squared() > 0.0,
                "三角 #{} 法线非法：{:?}",
                at.triangle,
                tri.normal
            );
            // 图集坐标 bbox（局部 + 原点）→ texel 遍历范围（保守取整）
            let mut min = [f32::INFINITY; 2];
            let mut max = [f32::NEG_INFINITY; 2];
            for v in &at.local {
                let a = [v[0] + origin[0], v[1] + origin[1]];
                min = [min[0].min(a[0]), min[1].min(a[1])];
                max = [max[0].max(a[0]), max[1].max(a[1])];
            }
            let bound = i64::from(size);
            let x0 = (min[0].floor() as i64).clamp(0, bound) as u32;
            let x1 = (max[0].ceil() as i64).clamp(0, bound) as u32;
            let y0 = (min[1].floor() as i64).clamp(0, bound) as u32;
            let y1 = (max[1].ceil() as i64).clamp(0, bound) as u32;
            let normal = tri.normal.normalize();
            for ty in y0..y1 {
                for tx in x0..x1 {
                    // texel 中心 → chart 局部坐标
                    let p = [tx as f32 + 0.5 - origin[0], ty as f32 + 0.5 - origin[1]];
                    if !point_in_triangle(p, &at.local) {
                        continue;
                    }
                    let index = ty as usize * size as usize + tx as usize;
                    if slots[index].is_some() {
                        continue; // 先者得（entry 序 = 装箱序、三角形序 = 输入序）
                    }
                    let (u, v) = barycentric_2d(p, &at.local);
                    let w = 1.0 - u - v;
                    slots[index] = Some(TexelSource {
                        index,
                        texel: [tx, ty],
                        position: tri.vertices[0] * w + tri.vertices[1] * u + tri.vertices[2] * v,
                        normal,
                        albedo: tri.albedo,
                        triangle: at.triangle,
                        barycentric: Vec2::new(u, v),
                    });
                }
            }
        }
    }
    slots.into_iter().flatten().collect()
}

/// ACES 色调映射（Narkowicz 解析近似）：与 `shaders/blit.wgsl` 的
/// `aces_tonemap` **同款公式同款系数**（a = 2.51, b = 0.03, c = 2.43,
/// d = 0.59, e = 0.14，输出 clamp [0, 1]）——预览图与引擎画面同观感。
pub fn aces_tonemap(x: Vec3) -> Vec3 {
    assert!(x.is_finite(), "色调映射输入非法：{x:?}");
    const A: f32 = 2.51;
    const B: f32 = 0.03;
    const C: f32 = 2.43;
    const D: f32 = 0.59;
    const E: f32 = 0.14;
    let f = |c: f32| ((c * (A * c + B)) / (c * (C * c + D) + E)).clamp(0.0, 1.0);
    Vec3::new(f(x.x), f(x.y), f(x.z))
}

/// 线性 → sRGB 编码（IEC 61966-2-1 标准段）。blit 输出线性色、由 sRGB
/// 目标格式做硬件编码；CPU 预览必须显式同款编码（模块文档）。
fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// 图集预览导出：辐照 → [`aces_tonemap`] → sRGB 编码 → 8 bit → PPM P6。
///
/// 行序 = texel 行主序（行 0 = v 0）。PPM 选 P6（二进制）：头三行 ASCII
/// （魔数 / 尺寸 / 最大值）+ `size² × 3` 字节 RGB，零依赖可测（单测钉住
/// 头格式与解析像素值），`ffmpeg -i *.ppm *.png` 直转。
pub fn write_lightmap_preview(lightmap: &BakedLightmap, path: &Path) -> std::io::Result<()> {
    let size = lightmap.atlas.size as usize;
    assert!(
        lightmap.radiance.len() == size * size,
        "辐照数组长度 {} 与图集尺寸 {size}×{size} 不一致",
        lightmap.radiance.len()
    );
    let file = std::fs::File::create(path)?;
    let mut out = BufWriter::new(file);
    writeln!(out, "P6")?;
    writeln!(out, "{size} {size}")?;
    writeln!(out, "255")?;
    let mut pixels = Vec::with_capacity(size * size * 3);
    for &irradiance in &lightmap.radiance {
        let mapped = aces_tonemap(irradiance);
        for channel in mapped.to_array() {
            pixels.push((linear_to_srgb(channel) * 255.0).round().clamp(0.0, 255.0) as u8);
        }
    }
    out.write_all(&pixels)?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试积木：显式法线的三角（不依赖环绕方向——分类 / 投影只读法线）。
    fn tri_n(v0: Vec3, v1: Vec3, v2: Vec3, normal: Vec3, entity: Entity) -> BakeTriangle {
        BakeTriangle {
            vertices: [v0, v1, v2],
            normal,
            albedo: Vec3::ONE,
            entity,
        }
    }

    /// XZ 水平 quad（法线 +Y，对角线 = min 角 → max 角，与回溯测试的
    /// 「斜边 texel 归先者」解析计数配套）。
    fn floor_quad(min: [f32; 2], max: [f32; 2], y: f32, entity: Entity) -> Vec<BakeTriangle> {
        let a = Vec3::new(min[0], y, min[1]);
        let b = Vec3::new(max[0], y, min[1]);
        let c = Vec3::new(max[0], y, max[1]);
        let d = Vec3::new(min[0], y, max[1]);
        vec![
            tri_n(a, b, c, Vec3::Y, entity),
            tri_n(a, c, d, Vec3::Y, entity),
        ]
    }

    fn uv_params(tpm: f32) -> BakeUvParams {
        BakeUvParams {
            texels_per_meter: tpm,
            atlas_size: 64,
        }
    }

    // ───────────────────────────── 分类与参数化 ─────────────────────────────

    /// 分类解析对照（六轴向 / 45° 倾斜 / 阈值边界 / 并列取低轴）+ 哈希
    /// 方向扫描验证「被丢弃轴分量下界」（= 投影面积 ≥ 0.5 × 世界面积的
    /// 完备性论证，模块文档）。
    #[test]
    fn classify_axis_covers_all_directions_with_analytic_cases() {
        assert_eq!(classify_axis(Vec3::Y), ChartAxis::Floor);
        assert_eq!(classify_axis(Vec3::NEG_Y), ChartAxis::Floor);
        assert_eq!(classify_axis(Vec3::X), ChartAxis::WallX);
        assert_eq!(classify_axis(Vec3::NEG_X), ChartAxis::WallX);
        assert_eq!(classify_axis(Vec3::Z), ChartAxis::WallZ);
        assert_eq!(classify_axis(Vec3::NEG_Z), ChartAxis::WallZ);
        // 45° 倾斜：|y| = 1/√2 ≈ 0.7071 > 0.7 ⇒ 水平类
        assert_eq!(
            classify_axis(Vec3::new(1.0, 1.0, 0.0)),
            ChartAxis::Floor,
            "45° 斜面 |n.y| = 0.707 > 阈值 ⇒ Floor"
        );
        // 水平并列（|x| = |z|）⇒ 低轴号 X；三维等倾同理
        assert_eq!(classify_axis(Vec3::new(1.0, 0.0, 1.0)), ChartAxis::WallX);
        assert_eq!(classify_axis(Vec3::new(1.0, 1.0, 1.0)), ChartAxis::WallX);
        // 阈值两侧（严格 > 语义；「恰 0.7」用例不可测——归一化的 1 ULP
        // 舍入决定落哪侧，故两侧各留 ~0.01 余量钉住语义）
        let just_below = Vec3::new(0.0, 0.69, 0.7238); // |n.y| ≈ 0.693
        assert_eq!(
            classify_axis(just_below),
            ChartAxis::WallZ,
            "阈下 ⇒ 墙面类（|x| = 0 < |z| ⇒ WallZ）"
        );
        let just_above = Vec3::new(0.0, 0.71, 0.7042); // |n.y| ≈ 0.712
        assert_eq!(classify_axis(just_above), ChartAxis::Floor, "阈上 ⇒ 水平类");

        // 哈希方向扫描：分类不 panic、倍长同分类（尺度无关）、被丢弃轴
        // 分量下界（Floor ⇒ |y| > 0.7；Wall ⇒ max(|x|,|z|) ≥ 0.504）
        for stream in 0..256u64 {
            let mix = |k: u64| {
                let z = (stream * 31 + k).wrapping_mul(0x9e37_79b9_7f4a_7c15);
                let z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                ((z ^ (z >> 31)) >> 40) as f32 / (1u64 << 24) as f32 - 0.5
            };
            let raw = Vec3::new(mix(1), mix(2), mix(3));
            if raw.length_squared() < 1e-6 {
                continue;
            }
            let n = raw.normalize();
            let axis = classify_axis(n);
            assert_eq!(classify_axis(n * 3.5), axis, "分类须尺度无关");
            match axis {
                ChartAxis::Floor => assert!(n.y.abs() > FLOOR_NORMAL_THRESHOLD),
                ChartAxis::WallX => {
                    assert!(n.x.abs() >= n.z.abs(), "并列须取 X：{n:?}");
                    assert!(
                        n.x.abs() >= 0.504,
                        "墙面类被丢弃轴分量下界 0.505（留 1e-3 余量）：{n:?}"
                    );
                }
                ChartAxis::WallZ => {
                    assert!(n.z.abs() > n.x.abs());
                    assert!(n.z.abs() >= 0.504, "下界同上：{n:?}");
                }
            }
        }
    }

    /// UV 与投影坐标的线性关系（解析精确值）：Floor 投影 XZ、WallZ 投影
    /// XY，局部坐标 = (投影 − chart min) × tpm + pad，尺寸 = ceil + 2·pad。
    #[test]
    fn chart_uvs_are_linear_in_projection_coordinates() {
        let floor = tri_n(
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(2.0, 1.0, 0.0),
            Vec3::new(0.0, 1.0, 3.0),
            Vec3::Y,
            Entity::from_bits(7),
        );
        // WallZ 墙（法线 +Z）：投影 XY
        let wall = tri_n(
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(1.0, 0.0, -1.0),
            Vec3::new(0.0, 2.0, -1.0),
            Vec3::Z,
            Entity::from_bits(8),
        );
        let charts = build_charts(&[floor, wall], &uv_params(2.0));
        assert_eq!(charts.len(), 2, "两实体两轴 ⇒ 两 chart");
        // BTreeMap 键序 = 实体位升序：floor(7) 在前
        let floor_chart = &charts[0];
        assert_eq!(floor_chart.axis, ChartAxis::Floor);
        assert_eq!(floor_chart.triangles, vec![0]);
        // 投影 ×2：v0 (0,0)、v1 (4,0)、v2 (0,6)；min = (0,0) ⇒ 局部 = +pad
        assert_eq!(
            floor_chart.local[0],
            [[1.0, 1.0], [5.0, 1.0], [1.0, 7.0]],
            "局部坐标 = 投影坐标 × tpm 平移（纯线性，解析值）"
        );
        assert_eq!(floor_chart.size, [6, 8], "ceil(4)+2 × ceil(6)+2");
        assert_eq!(floor_chart.texels_per_meter, 2.0);

        let wall_chart = &charts[1];
        assert_eq!(wall_chart.axis, ChartAxis::WallZ, "法线 +Z ⇒ 投影 XY");
        assert_eq!(wall_chart.triangles, vec![1]);
        // 投影 (x, y) ×2：(0,0)、(2,0)、(0,4) ⇒ 局部 +pad
        assert_eq!(wall_chart.local[0], [[1.0, 1.0], [3.0, 1.0], [1.0, 5.0]]);
        assert_eq!(wall_chart.size, [4, 6]);
    }

    /// 串光防护的正反两面：①同实体双层重叠 quad ⇒ 后放置者被 +x 偏移到
    /// 空位（两三角同位移、chart 矩形放大、全体两两无正面积重叠）；
    /// ②单 quad 的共斜边两半 bbox 大面积重叠但零面积相交 ⇒ **不偏移**
    /// （bbox 判定会把它撕开——正面积判定的存在意义）。
    #[test]
    fn overlapping_coplanar_triangles_are_shifted_apart_and_quads_stay_intact() {
        let lower = Entity::from_bits(5);
        let intact = Entity::from_bits(6);
        let mut triangles = floor_quad([0.0, 0.0], [2.0, 2.0], 0.0, lower);
        // 双层地板：投影 [1,3]² 与 [0,2]² 正面积重叠（y 不同世界位不同）
        triangles.extend(floor_quad([1.0, 1.0], [3.0, 3.0], 3.0, lower));
        triangles.extend(floor_quad([10.0, 10.0], [12.0, 12.0], 0.0, intact));
        let charts = build_charts(&triangles, &uv_params(1.0));
        assert_eq!(charts.len(), 2);
        let chart = charts
            .iter()
            .find(|c| c.entity == lower)
            .expect("双层实体 chart");
        assert_eq!(chart.triangles, vec![0, 1, 2, 3]);
        // ① 下层 quad 不偏移：局部 = 投影 + pad
        assert_eq!(chart.local[0], [[1.0, 1.0], [3.0, 1.0], [3.0, 3.0]]);
        assert_eq!(chart.local[1], [[1.0, 1.0], [3.0, 3.0], [1.0, 3.0]]);
        // 上层 quad 整体右移：偏移 = 冲突者 max_x(2) − 自身 min_x(1) + gap(2)
        // = 3 ⇒ 投影 [4,6]×[1,3] ⇒ 局部 [5,7]×[2,4]（两三角同位移）
        assert_eq!(chart.local[2], [[5.0, 2.0], [7.0, 2.0], [7.0, 4.0]]);
        assert_eq!(chart.local[3], [[5.0, 2.0], [7.0, 4.0], [5.0, 4.0]]);
        // chart 矩形放大到偏移后 bbox：x [0,6] → 6+2、y [0,3] → 3+2
        assert_eq!(chart.size, [8, 5]);
        // 全体两两无正面积重叠（串光判据自身复核）
        for i in 0..chart.local.len() {
            for j in (i + 1)..chart.local.len() {
                assert!(
                    !triangles_overlap(&chart.local[i], &chart.local[j]),
                    "偏移后三角 {i} / {j} 仍正面积重叠"
                );
            }
        }
        // ② 共斜边 quad 不偏移：局部 = 投影 − min + pad（无 x 位移）
        let intact_chart = charts
            .iter()
            .find(|c| c.entity == intact)
            .expect("完整 quad 实体 chart");
        assert_eq!(
            intact_chart.local[0],
            [[1.0, 1.0], [3.0, 1.0], [3.0, 3.0]],
            "共边相邻不是串光，不许偏移"
        );
        assert_eq!(intact_chart.local[1], [[1.0, 1.0], [3.0, 3.0], [1.0, 3.0]]);
        assert_eq!(intact_chart.size, [4, 4]);
    }

    // ───────────────────────────── 装箱 ─────────────────────────────

    /// 16 个 4×4 m 平面实体（规则几何）：chart 18×18、图集 128 ⇒ shelf
    /// 每层 7 个（7×18 = 126 ≤ 128）、三层 7/7/2、已用高度 54、利用率
    /// 解析值 16×324/(128×54) = **0.75 > 0.5**；矩形两两不相交、并列
    /// （等面积）按实体位升序、纯函数装箱 mesh 恒 None。
    #[test]
    fn pack_atlas_is_disjoint_sorted_and_utilization_above_half() {
        let mut triangles = Vec::new();
        let mut entities = Vec::new();
        for e in 0..16u64 {
            let entity = Entity::from_bits(e + 1);
            entities.push(entity);
            triangles.extend(floor_quad(
                [(e % 4) as f32 * 6.0, (e / 4) as f32 * 6.0],
                [(e % 4) as f32 * 6.0 + 4.0, (e / 4) as f32 * 6.0 + 4.0],
                0.0,
                entity,
            ));
        }
        let charts = build_charts(&triangles, &uv_params(4.0));
        assert_eq!(charts.len(), 16);
        assert!(
            charts.iter().all(|c| c.size == [18, 18]),
            "16 texel + 2 pad"
        );
        let atlas = pack_atlas(charts, 128);
        assert_eq!(atlas.entries.len(), 16);
        assert_eq!(atlas.used_height, 54, "三层 shelf：18 × 3");
        // 排序：等面积 ⇒ 实体位升序
        let packed: Vec<u64> = atlas.entries.iter().map(|e| e.entity.to_bits()).collect();
        let mut expected: Vec<u64> = entities.iter().map(|e| e.to_bits()).collect();
        expected.sort_unstable();
        assert_eq!(packed, expected, "等面积并列按实体位升序");
        // shelf 结构：第 0 个在 (0,0)、第 7 个开新层 (0,18)
        assert_eq!(atlas.entries[0].origin, [0, 0]);
        assert_eq!(atlas.entries[7].origin, [0, 18], "每层 7 个、层高 18");
        assert_eq!(atlas.entries[7].uv_rect[1], 18.0 / 128.0);
        // 两两不相交（整数矩形：正面积交集判无）
        for i in 0..atlas.entries.len() {
            for j in (i + 1)..atlas.entries.len() {
                let a = &atlas.entries[i];
                let b = &atlas.entries[j];
                let ow = (a.origin[0] + a.size[0])
                    .min(b.origin[0] + b.size[0])
                    .saturating_sub(a.origin[0].max(b.origin[0]));
                let oh = (a.origin[1] + a.size[1])
                    .min(b.origin[1] + b.size[1])
                    .saturating_sub(a.origin[1].max(b.origin[1]));
                assert!(
                    ow == 0 || oh == 0,
                    "chart {i} / {j} 矩形正面积相交：{:?} vs {:?}",
                    a.uv_rect,
                    b.uv_rect
                );
            }
        }
        // 利用率解析值：16 × 18² / (128 × 54) = 0.75（全整数 ⇒ f32 精确）
        let utilization = atlas.utilization();
        assert!(
            (utilization - 0.75).abs() < 1e-7,
            "利用率应为解析值 0.75，实际 {utilization}"
        );
        assert!(utilization > 0.5, "规则几何利用率须 > 50%");
        assert!(
            atlas.entries.iter().all(|e| e.mesh.is_none()),
            "纯函数装箱不查世界 ⇒ mesh 由 bake_lightmap 补齐"
        );
    }

    #[test]
    #[should_panic(expected = "图集尺寸不足")]
    fn pack_atlas_rejects_chart_larger_than_atlas() {
        let triangles = floor_quad([0.0, 0.0], [4.0, 4.0], 0.0, Entity::from_bits(1));
        let charts = build_charts(&triangles, &uv_params(4.0)); // 18×18
        let _ = pack_atlas(charts, 16);
    }

    #[test]
    #[should_panic(expected = "图集尺寸不足")]
    fn pack_atlas_rejects_when_shelves_overflow() {
        // 4 个 18×18 chart、图集 20：每层 1 个 ⇒ 第三层 y = 36 > 20
        let mut triangles = Vec::new();
        for e in 0..4u64 {
            triangles.extend(floor_quad(
                [0.0, e as f32 * 6.0],
                [4.0, e as f32 * 6.0 + 4.0],
                0.0,
                Entity::from_bits(e + 1),
            ));
        }
        let charts = build_charts(&triangles, &uv_params(4.0));
        let _ = pack_atlas(charts, 20);
    }

    // ───────────────────────────── texel 回溯 ─────────────────────────────

    /// 规则格点解析回溯：4×4 m 地板（y = 1）、tpm = 2 ⇒ 8×8 = 64 个覆盖
    /// texel、格距 0.5 m；首 / 末 / 中心 texel 的世界位对照解析值；pad 与
    /// 图集角落不覆盖；斜边 texel 恒归三角 0 ⇒ 两三角覆盖计数解析值
    /// n(n+1)/2 = 36 与 n(n−1)/2 = 28。
    #[test]
    fn bake_texels_recovers_analytic_world_positions_on_regular_grid() {
        let triangles = floor_quad([0.0, 0.0], [4.0, 4.0], 1.0, Entity::from_bits(3));
        let charts = build_charts(&triangles, &uv_params(2.0));
        let atlas = pack_atlas(charts, 16);
        let sources = bake_texels(&atlas, &triangles);
        assert_eq!(sources.len(), 64, "8×8 覆盖 texel（pad 与外围不覆盖）");
        // 行主序升序
        for pair in sources.windows(2) {
            assert!(pair[0].index < pair[1].index);
        }
        let first = &sources[0];
        assert_eq!(first.index, 17, "texel (1,1) = pad 后首个覆盖 texel");
        assert_eq!(first.texel, [1, 1]);
        assert!(
            (first.position - Vec3::new(0.25, 1.0, 0.25)).length() < 1e-6,
            "texel 中心 (1.5,1.5) − pad = 投影 (0.5,0.5) ⇒ 世界 (0.25,1,0.25)，实际 {:?}",
            first.position
        );
        assert_eq!(first.normal, Vec3::Y);
        assert_eq!(first.triangle, 0, "(1.5,1.5) 在斜边上 ⇒ 先者得（三角 0）");
        let last = sources.last().expect("非空");
        assert_eq!(last.index, 8 * 16 + 8, "texel (8,8)");
        assert!(
            (last.position - Vec3::new(3.75, 1.0, 3.75)).length() < 1e-6,
            "末覆盖 texel 解析值，实际 {:?}",
            last.position
        );
        // 重心自洽：position == v0(1−u−v) + v1·u + v2·v
        for s in &sources {
            let tri = &triangles[s.triangle];
            let w = 1.0 - s.barycentric.x - s.barycentric.y;
            let by_bary = tri.vertices[0] * w
                + tri.vertices[1] * s.barycentric.x
                + tri.vertices[2] * s.barycentric.y;
            assert!(
                (by_bary - s.position).length() < 1e-6,
                "texel {} 重心复原不一致",
                s.index
            );
            assert_eq!(s.position.y, 1.0, "地板 texel 恒在 y = 1");
        }
        // 未覆盖：pad texel (0,0)、行首 (0,1)、图集角落 (15,15)
        let indices: Vec<usize> = sources.iter().map(|s| s.index).collect();
        for absent in [0usize, 16, 255] {
            assert!(!indices.contains(&absent), "texel {absent} 不应覆盖");
        }
        // 斜边解析计数：8×8 格、对角线 8 个 texel 归三角 0
        let count0 = sources.iter().filter(|s| s.triangle == 0).count();
        let count1 = sources.iter().filter(|s| s.triangle == 1).count();
        assert_eq!(count0, 36, "n(n+1)/2 = 8×9/2（含斜边先者得）");
        assert_eq!(count1, 28, "n(n−1)/2 = 8×7/2");
    }

    /// 边界归属规则钉死：2×2 m quad、tpm = 1 ⇒ 2×2 覆盖格，斜边恰过两个
    /// texel 中心 (1.5,1.5) / (2.5,2.5) ⇒ 恒归三角 0；非斜边 texel 各归
    /// 其半；世界位对照解析格点。
    #[test]
    fn texels_on_shared_edges_go_to_first_triangle() {
        let triangles = floor_quad([0.0, 0.0], [2.0, 2.0], 0.0, Entity::from_bits(4));
        let charts = build_charts(&triangles, &uv_params(1.0));
        let atlas = pack_atlas(charts, 8);
        let sources = bake_texels(&atlas, &triangles);
        assert_eq!(sources.len(), 4);
        let at = |tx: u32, ty: u32| -> &TexelSource {
            sources
                .iter()
                .find(|s| s.texel == [tx, ty])
                .unwrap_or_else(|| panic!("texel ({tx},{ty}) 应覆盖"))
        };
        // 斜边两端 texel：先者得 ⇒ 三角 0
        assert_eq!(at(1, 1).triangle, 0, "(1.5,1.5) 在斜边上");
        assert_eq!(at(2, 2).triangle, 0, "(2.5,2.5) 在斜边上");
        assert_eq!(at(2, 1).triangle, 0, "斜边下方 = 三角 0 内部");
        assert_eq!(at(1, 2).triangle, 1, "斜边上方 = 三角 1 内部");
        // 世界位 = 0.5 m 格点（tpm 1、pad 1 ⇒ texel (1,1) 中心 → (0.5,0.5)）
        assert!(
            (at(1, 1).position - Vec3::new(0.5, 0.0, 0.5)).length() < 1e-6,
            "{:?}",
            at(1, 1).position
        );
        assert!(
            (at(1, 2).position - Vec3::new(0.5, 0.0, 1.5)).length() < 1e-6,
            "{:?}",
            at(1, 2).position
        );
    }

    // ───────────────────────────── 预览导出 ─────────────────────────────

    /// ACES（Narkowicz）解析对照：f(0) = 0 精确、f(1) = 2.54/3.16、
    /// f(∞ 大值) = 2.51/2.43 > 1 ⇒ clamp 恰 1；逐通道独立、正半轴单调。
    #[test]
    fn aces_tonemap_matches_narkowicz_analytic_values() {
        assert_eq!(aces_tonemap(Vec3::ZERO), Vec3::ZERO);
        let one = aces_tonemap(Vec3::ONE);
        let expected = 2.54 / 3.16; // (2.51 + 0.03) / (2.43 + 0.59 + 0.14)
        assert!(
            (one.x - expected).abs() < 1e-6
                && one.x.to_bits() == one.y.to_bits()
                && one.y.to_bits() == one.z.to_bits(),
            "f(1) = 2.54/3.16 = {expected}，实际 {:?}",
            one
        );
        assert_eq!(
            aces_tonemap(Vec3::splat(100.0)),
            Vec3::ONE,
            "f(100) = 25103/24359.14 ≈ 1.03 ⇒ clamp 恰 1"
        );
        // 逐通道独立
        let mixed = aces_tonemap(Vec3::new(0.0, 1.0, 100.0));
        assert_eq!(mixed.x, 0.0);
        assert!((mixed.y - expected).abs() < 1e-6);
        assert_eq!(mixed.z, 1.0);
        // 正半轴单调（clamp 饱和前）
        let mut prev = 0.0f32;
        for k in 1..=64u32 {
            let x = k as f32 / 16.0; // 至 4.0，f(4) ≈ 0.973 < 1
            let f = aces_tonemap(Vec3::splat(x)).x;
            assert!(f > prev, "ACES 在 x = {x} 处失去单调性");
            prev = f;
        }
    }

    /// PPM P6 头格式 + 像素字节解析：黑（0）→ 0、超亮（100）→ clamp 255、
    /// 辐照 1.0 → ACES 0.8038 → sRGB 0.9082 → 232（±1，手推对数余量）。
    /// 临时文件名全仓唯一（AGENTS.md 测试竞态规则），用后即删。
    #[test]
    fn write_lightmap_preview_emits_valid_ppm() {
        let lightmap = BakedLightmap {
            atlas: LightmapAtlas {
                size: 2,
                used_height: 0,
                entries: Vec::new(),
            },
            radiance: vec![Vec3::ZERO, Vec3::splat(100.0), Vec3::ONE, Vec3::splat(0.5)],
            // 预览只消费 radiance；indirect / covered 填自洽值
            indirect: vec![Vec3::ZERO, Vec3::splat(100.0), Vec3::ONE, Vec3::splat(0.5)],
            covered: vec![false, true, true, true],
        };
        let path = Path::new("iris_bake_uv_ppm_test.ppm");
        write_lightmap_preview(&lightmap, path).expect("写临时 PPM");
        let bytes = std::fs::read(path).expect("读回临时 PPM");
        std::fs::remove_file(path).expect("删除临时 PPM");
        let header = b"P6\n2 2\n255\n";
        assert!(
            bytes.starts_with(header),
            "PPM 头应为 {header:?}，实际 {:?}",
            &bytes[..header.len().min(bytes.len())]
        );
        assert_eq!(
            bytes.len(),
            header.len() + 2 * 2 * 3,
            "P6 = 头 + W×H×3 字节"
        );
        let pixel = |i: usize| -> [u8; 3] {
            let o = header.len() + i * 3;
            [bytes[o], bytes[o + 1], bytes[o + 2]]
        };
        assert_eq!(pixel(0), [0, 0, 0], "零辐照 = 黑");
        assert_eq!(pixel(1), [255, 255, 255], "ACES clamp + sRGB 编码 = 白");
        // f(1.0) = 0.8038 → sRGB = 1.055 × 0.8038^(1/2.4) − 0.055 ≈ 0.9082 → 232
        let mid = pixel(2);
        assert!(
            mid.iter().all(|&b| (b as i32 - 232).abs() <= 1),
            "辐照 1.0 应编码为 232（±1 手推余量），实际 {mid:?}"
        );
        // f(0.5) = (0.5×1.285)/(0.5×1.805+0.14) = 0.6425/1.0425 ≈ 0.6163
        // → sRGB = 1.055 × 0.6163^(1/2.4) − 0.055 ≈ 0.8074 → 206（±1）
        let dim = pixel(3);
        assert!(
            dim.iter().all(|&b| (b as i32 - 206).abs() <= 1),
            "辐照 0.5 应编码为 206（±1 手推余量），实际 {dim:?}"
        );
    }
}
