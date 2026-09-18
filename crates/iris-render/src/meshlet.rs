//! 虚拟几何体 V1+V2（R84/R85）：meshlet 簇的运行期表示 + 版本化资产
//! 容器（IRML）+ V2 的 GPU compute 剔除谓词。
//!
//! meshlet（簇）= 网格三角形的近似凸连通小块（≤ [`MESHLET_CLUSTER_MAX_TRIANGLES`]
//! 个三角、≤ [`MESHLET_CLUSTER_MAX_VERTICES`] 个簇内顶点），是本仓
//! 「Nanite 式子集」路线（R84–R87）的基本处理单元。本模块提供四件事：
//!
//! 1. **运行期表示**：[`MeshletMesh`]（全局顶点 / 全局索引 / 逐簇元数据）
//!    与 [`MeshletClusterRaw`]（48 B POD，布局即 R85 GPU storage 的逐字
//!    排布——`meshlet_cull.wgsl` / `meshlet_draw.wgsl` 以 12×u32 词阵列
//!    消费，见下文「GPU storage 词布局」）；
//! 2. **展开**：[`MeshletMesh::expand`] 纯函数——把簇表示还原为既有渲染
//!    路径可消费的 [`MeshData`](crate::mesh::MeshData)。V1 的正确性锚点：
//!    顶点重排不改像素（覆盖集与逐角插值不变），同一几何「常规路径 vs
//!    meshlet 构建后展开路径」渲染逐字节一致（GPU 佐证见
//!    `tests/meshlet_render.rs`；R85 中继续作为对照锚点与文档兜底路径）；
//! 3. **资产容器（IRML）**：版本化 + CRC32 的单文件格式，`write_to` /
//!    `write_file` / `from_bytes` / `open` 与 `iris-engine::pack` 同构；
//! 4. **V2 剔除（R85）**：组件 [`MeshletInstance`] / 目录
//!    [`MeshletMeshStore`] 与剔除谓词纯函数（背面锥
//!    [`cluster_is_backfacing`]、紧凑化扫描镜像 [`chunked_prefix_compact`]、
//!    顶点 storage 词重排 [`vertex_storage_words`]、网格包围盒
//!    [`mesh_bounds`]）。GPU 侧 `shaders/meshlet_cull.wgsl`（compute：
//!    逐簇视锥 + 背面锥判定 → u32 存活标志 → 前缀和 + 稳定紧凑化）与
//!    `shaders/meshlet_draw.wgsl`（vertex-pull 绘制）是其镜像消费端，
//!    接线见 `renderer.rs` 的 meshlet 段与 ARCHITECTURE §4.41。
//!
//! # GPU storage 词布局（R85 影子契约）
//!
//! WGSL storage 地址空间里 `vec3<f32>` 的对齐是 16 B，48 B 的
//! [`MeshletClusterRaw`] 与 32 B 的 [`Vertex`](crate::mesh::Vertex) 都
//! **不能**作为 WGSL struct 原样映射。定稿：两个数组都以扁平 u32 词
//! 消费，字节布局与 CPU 结构**逐字一致**（`bytemuck::cast_slice` 直接
//! 上传，零重排）：
//!
//! - 簇 = 12 词（48 B）：`w0 = vertex_offset`、`w1 = index_offset`、
//!   `w2 = vertex_count | triangle_count << 8 | reserved << 16`、
//!   `w3..w5 = cone_axis`（f32 位型）、`w6 = cone_cutoff_cos`、
//!   `w7..w10 = bounds`、`w11 = error`。WGSL 侧以 `bitcast<f32>` 解码，
//!   词下标由 `cluster_raw_word_layout`（单测）锚定；
//! - 顶点 = 2 词（8 B... 即 32 B 中的 2×16 B vec4）：`[px,py,pz,nx]` /
//!   `[ny,nz,u,v]`——这是唯一需要重排的数组（[`vertex_storage_words`]，
//!   注册期一次性）；索引 = `array<u32>` 原样透传。
//!
//! # 前缀和确定性（R85 定稿要点）
//!
//! u32 整数加法**结合精确**（无舍入、无重排序自由）⇒ 无论扫描树形状
//! （Hillis-Steele / 顺序 / 分段）结果恒等——与浮点扫描的本质区别，也是
//! 「紧凑化无需原子、无需固定线程调度即会话内逐位确定」的根据。紧凑序
//! = 全局簇号升序（stable compaction）⇒ 绘制序确定。
//!
//! # 全案 MVP 边界（R84–R87 红线修订声明，越界即 bug）
//!
//! # 全案 MVP 边界（R84–R87 红线修订声明，越界即 bug）
//!
//! - **无 visbuffer**：不经软件光栅、不记逐像素三角 id——V1 经既有常规
//!   网格路径渲染（compute 剔除是 R85 的事）；
//! - **opaque-only**：透明 / 蒙皮 / 带切线 / 带光照贴图 UV 的网格不适用
//!   （容器只载 32 B 标准顶点）；
//! - **无软件光栅回退**；
//! - **单 LOD（R84/R85 口径）**——R86 起层级 LOD 在本文件落地（簇 DAG +
//!   屏幕空间误差选择，见下节），但簇级 `error` 字段仍恒 0.0：误差记录
//!   在**组级**（[`MeshletLodGroup`]），簇字段留待 GPU 侧选择启用；
//! - **非流式加载（R84/R85/R86 口径）→ R87 起收窄为「GPU 显存分页」**：
//!   IRML 整文件仍先读进内存（磁盘级异步 IO 不做），但页划分（IRML v3
//!   页区段）+ 驻留状态机 + 驻留感知选择已落地（[`MeshletPageTable`] /
//!   `meshlet_stream` 模块）——数据在内存的前提下按页上传 / 驱逐 /
//!   门控绘制；pack 源的最小读取入口随 R87 兑现（`iris_engine::pack`）。
//!
//! # R87 分页流送（GPU 显存分页 + 驻留感知选择 + 遮挡整合）
//!
//! - **页划分**：固定簇数页（[`crate::meshlet_stream::MESHLET_PAGE_CLUSTERS`]），
//!   构建期写进 IRML v3 页区段（页数 / 逐页顶点与索引区间 / 逐簇所属
//!   层级组号，读侧与导出值逐字段核对）；v1/v2 文件照常读取，页表在
//!   流送激活时按同参数现算（纯派生、逐位等价）；
//! - **驻留管理**：[`crate::meshlet_stream::MeshletStreamSettings`] 资源
//!   **缺席 / 预算 `None` = 全驻留 = R86 行为逐位不变**；有预算 ⇒ 页按
//!   固定全序（粗层优先的偏离定稿见 `meshlet_stream` 模块头）分帧上传、
//!   预算满时最低优先级让位、本帧请求页受保护（无抖动）；
//! - **驻留感知选择**：[`crate::meshlet_stream::select_lod_cluster_ranges_resident`]
//!   ——选中组页缺时沿 DAG 上取驻留父组，根链全缺 ⇒ 实例整帧跳过
//!   （层级天然提供粗化回退，无缝洞）；
//! - **遮挡整合（R53 两阶段，实体级）**：`MeshletInstance` 的实例 AABB
//!   （簇 bounds 联合，注册期预计算）参与遮挡查询，被遮挡实体整帧
//!   跳过（不进 cull dispatch 输入）；与流送正交——未驻留实例在选择
//!   期已整帧跳过，遮挡路径不双重处理。逐簇 GPU 遮挡（Hi-Z 类）不做。
//!
//! # R86 层级 LOD（簇 DAG + 屏幕空间误差选择）
//!
//! - **簇 DAG**（[`MeshletHierarchy`]）：构建期自底向上——叶簇按固定
//!   贪心归并成组、组内几何经体素网格顶点聚类简化（Rossignac；简化器
//!   本体在 `iris_engine::meshlet_builder`，选型理由见其模块文档）后
//!   重新聚类为父组的自有簇，逐组记录 children / 簇范围 / 误差 /
//!   bounds / cone。`group_error = max(own, 所有 children)` 沿 DAG 向上
//!   单调增。v1 容器 / `--no-lod` 产物 = 空层级 = 单层（全簇即叶）。
//! - **逐帧选择**（[`select_lod_clusters`]）：从根按固定序遍历，组的
//!   屏幕空间误差 = `group_error × error_scale × (screen_h × 0.5) /
//!   (dist × tan_half_fov)` ≤ `threshold_px` ⇒ 选中本组簇范围、停止
//!   下钻；否则下钻 children（叶组恒选自身）。选择是**逐组布尔** ⇒ 与
//!   遍历实现无关；输出 = 选中簇号升序（相邻区间合并）。选择在 CPU
//!   （GPU 选择留后续轮），产物 = 帧实例表的 run 形条目——R85 的
//!   cull / compact / draw 管线零改动，只换输入集合。
//! - **缺席语义**：[`MeshletLodSettings`] 资源缺席 / `enabled = false`
//!   = 全叶表 = R85 行为逐位不变；`threshold_px = 0`（或负）恒细化到
//!   叶 = 同一列表。
//!
//! # 簇元数据口径（R85 GPU 剔除 / R87 分页的前置契约）
//!
//! - `vertex_offset` / `index_offset`：指向 [`MeshletMesh`] 全局缓冲的
//!   绝对偏移（簇数据在全局缓冲中**按簇连续、按构建序排布**——读侧
//!   校验连续性，见 [`MeshletAssetError::Corrupt`]）；
//! - `cone_axis` / `cone_cutoff_cos`：背面簇剔除边界锥。axis 为簇内三角
//!   面法线均值的归一化向量；`cone_cutoff_cos = min(axis·n_i)`（截断到
//!   [-1, 1]）。剔除判定（R85 定稿的精确谓词，见
//!   [`cluster_is_backfacing`] 的推导）：视线方向与轴的夹角越过
//!   90° + acos(cutoff) 才判整簇背面——R84 头注释里「点积 < cutoff 即
//!   背面」是松散表述（对近平坦簇会误剔正面），V1 无剔除消费者未暴露，
//!   R85 以本模块纯函数为准。退化防护：簇内无有效面法线（全退化三角）
//!   或均值长度 < 1e-6（法线完全对消，如立方体六向簇）⇒ 锥退化为全向
//!   （axis = [0,0,1]、cutoff = −1，**永不剔除**，保守方向）；
//! - `bounds`：簇包围球。**xyz = 球心相对网格包围盒的归一化坐标**
//!   （`(center − bbox_min) / bbox_extent`，退化轴 extent = 0 时除数取
//!   1.0），**w = 半径（世界单位，米，不归一）**。归一化基（网格包围盒）
//!   不入容器——消费方从网格级数据自取（R85 的 mesh 级 uniform 已有
//!   包围盒位）；
//! - `error`：本簇相对父簇的简化误差（SCREEN_SPACE 语义预留）。V1 无
//!   简化器恒 0.0。
//!
//! # 确定性
//!
//! 元数据纯函数（[`cluster_bounding_sphere`] / [`cluster_cone`] /
//! [`cluster_bounds_normalized`]）与容器编解码均为无状态纯函数：同输入
//! 两跑输出逐位一致（`to_bits` 断言在单测）。归一化用 `sqrt`（glam
//! `length` / `normalize` 内部即 sqrt，无 `hypot`——libm 无位级保证，
//! 见 AGENTS.md 确定性硬文化）。聚类算法本身在
//! `iris-engine::meshlet_builder`（构建器与容器分离：渲染 crate 只
//! 消费构建产物，构建期算法留在资产侧）。
//!
//! # v1 诚实限制
//!
//! 见模块头 MVP 边界；另：容器无压缩（顶点 32 B / 索引 4 B 原样存储，
//! R87 评估后维持不压缩——页粒度流送下压缩会破坏「页 = 固定字节区间」
//! 的直接寻址，留格式 v4 评估）、簇跨簇顶点不去重（meshlet 标准形态：
//! 簇间边界顶点各簇各存一份，总量换缓存局部性）、CRC32 只防损坏不防
//! 篡改（与 pack 同一立场）。

use std::fmt;
use std::fs::File;
use std::io::Write;
use std::path::Path;

use glam::Vec3;
use iris_core::Resource;

use crate::mesh::{MeshData, Vertex};
use crate::meshlet_stream::{MESHLET_NO_GROUP, MeshletPageTable};

/// 单簇三角形数上限。
pub const MESHLET_CLUSTER_MAX_TRIANGLES: usize = 128;

/// 单簇顶点数上限（簇内局部重映射后）。
pub const MESHLET_CLUSTER_MAX_VERTICES: usize = 256;

/// 背面锥退化时的兜底轴（全向锥——永不背面剔除，保守方向）。
pub const MESHLET_CONE_FALLBACK_AXIS: [f32; 3] = [0.0, 0.0, 1.0];

/// 背面锥退化时的兜底 cutoff（−1 = 整个方向半球都算正面）。
pub const MESHLET_CONE_FALLBACK_CUTOFF_COS: f32 = -1.0;

/// 单帧 meshlet 剔除 / 绘制的全局簇容量上限（R85）。
///
/// 三处消费者同一常量：IRML 读取（单文件簇数超限 = 构建期拒绝）、渲染器
/// 注册处（arena 总量超限 = warn + 该网格跳过注册）、GPU storage 缓冲
/// 尺寸（簇词 16384 × 48 B = 768 KB；存活标志 / 载荷 / 紧凑列表同容量）。
pub const MAX_MESHLET_CLUSTERS: usize = 16384;

/// 紧凑化 compute pass 的工作组线程数（WGSL `@workgroup_size` 同值）。
pub const MESHLET_COMPACT_THREADS: u32 = 256;

/// 紧凑化 pass 每线程顺序扫描的 flag 数（段长）。
///
/// 线程数 × 段长 = [`MAX_MESHLET_CLUSTERS`]（const 断言锁定）：单工作组
/// 两遍扫描——每线程先把本段的存活数顺序累加，再对 256 个段总和做
/// Hillis-Steele 包含式扫描得到段基址，第二遍把存活者写到「段基址 +
/// 段内游标」的最终槽位。无原子、无跨组传递，紧凑序 = 全局簇号升序。
pub const MESHLET_COMPACT_CHUNK: u32 = 64;

const _: () = assert!(
    MESHLET_COMPACT_THREADS as usize * MESHLET_COMPACT_CHUNK as usize == MAX_MESHLET_CLUSTERS,
    "紧凑化单工作组容量必须恰为簇上限（256 × 64 = 16384）"
);

/// meshlet 网格句柄（R85），指向 [`MeshletMeshStore`] 中的条目。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, iris_core::Component)]
pub struct MeshletMeshHandle(pub u32);

/// CPU 侧 meshlet 网格目录（World 资源，R85）。与 [`crate::mesh::MeshStore`]
/// 同构的句柄注册表：GPU 上传（簇 / 顶点 / 索引 arena）由渲染器按需懒执行。
#[derive(Default, Resource)]
pub struct MeshletMeshStore {
    meshes: Vec<MeshletMesh>,
}

impl MeshletMeshStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册 meshlet 网格；新句柄从 0 开始递增。
    pub fn add(&mut self, mesh: MeshletMesh) -> MeshletMeshHandle {
        self.meshes.push(mesh);
        MeshletMeshHandle(self.meshes.len() as u32 - 1)
    }

    pub fn get(&self, handle: MeshletMeshHandle) -> Option<&MeshletMesh> {
        self.meshes.get(handle.0 as usize)
    }

    /// 已注册 meshlet 网格数。
    pub fn len(&self) -> usize {
        self.meshes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.meshes.is_empty()
    }
}

/// meshlet 实例组件（R85）：把 [`MeshletMeshHandle`] 指向的簇表示挂上
/// `GlobalTransform` 实体参与渲染。缺本组件 = 零 dispatch、零回读、
/// 零绘制（既有像素与命令流逐位不变）。
///
/// v1 消费口径（诚实清单见 ARCHITECTURE §8.54）：材质恒用默认材质
/// （句柄 0——单 instanced draw 单材质，逐实例材质需按材质切段，留
/// 后续轮）；不投影阴影、不进 scene_depth 副本；间接光只吃
/// irradiance_map（vertex-pull 管线无 loc14 探针实例属性路径）。
#[derive(Clone, Copy, Debug, iris_core::Component)]
pub struct MeshletInstance {
    /// 簇表示网格（须已注册进 [`MeshletMeshStore`]，未注册 = warn + 跳过）。
    pub mesh: MeshletMeshHandle,
}

/// 簇元数据（48 B，POD；布局为 R85 GPU storage 的前置——字段排布即未来
/// 的 WGSL struct 排布，改一字段必须过 R85 的并行布局看门）。
///
/// 对齐注释（repr(C)，全字段自然对齐；`reserved` 显式占位消除隐式填充
/// ——Pod 契约要求无填充字节，也为 R85 预留语义位）：
///
/// ```text
/// 偏移 0   vertex_offset  u32    簇顶点在全局顶点缓冲的起始下标
/// 偏移 4   index_offset   u32    簇索引在全局索引缓冲的起始下标
/// 偏移 8   vertex_count   u8     簇内顶点数（≤ 256）
/// 偏移 9   triangle_count u8     簇内三角形数（≤ 128）
/// 偏移 10  reserved       u8×2   恒 0（读侧校验，非零 = 损坏）
/// 偏移 12  cone_axis      f32×3  边界锥轴（归一化；退化 = [0,0,1] 全向）
/// 偏移 24  cone_cutoff_cos f32   min(axis·n_i)，截断 [-1,1]；退化 = −1
/// 偏移 28  bounds         f32×4  xyz = 球心（相对网格包围盒归一）、w = 半径（米）
/// 偏移 44  error          f32    简化误差（V1 恒 0.0，R86 启用）
/// 偏移 48  （结尾）
/// ```
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MeshletClusterRaw {
    /// 簇顶点在全局顶点缓冲的起始下标。
    pub vertex_offset: u32,
    /// 簇索引在全局索引缓冲的起始下标（索引为簇内局部顶点号）。
    pub index_offset: u32,
    /// 簇内顶点数（≤ [`MESHLET_CLUSTER_MAX_VERTICES`]）。
    pub vertex_count: u8,
    /// 簇内三角形数（≤ [`MESHLET_CLUSTER_MAX_TRIANGLES`]）。
    pub triangle_count: u8,
    /// 保留位，恒 0（显式占位：消除结构体隐式填充满足 Pod，读侧校验）。
    pub reserved: [u8; 2],
    /// 边界锥轴（归一化；退化 = [`MESHLET_CONE_FALLBACK_AXIS`] 全向锥）。
    pub cone_axis: [f32; 3],
    /// 边界锥 cutoff 的 cos 值：视线·axis < cutoff ⇒ 簇整体背面。
    /// 退化 = [`MESHLET_CONE_FALLBACK_CUTOFF_COS`]（−1，永不剔除）。
    pub cone_cutoff_cos: f32,
    /// 包围球：xyz = 球心（相对网格包围盒归一）、w = 半径（米）。
    pub bounds: [f32; 4],
    /// 简化误差（V1 恒 0.0；R86 简化器启用后 = 本簇相对父簇的误差）。
    pub error: f32,
}

// 布局契约：48 B（R85 storage 前置——布局一动，这里先红）。
const _: () = assert!(std::mem::size_of::<MeshletClusterRaw>() == 48);

impl Default for MeshletClusterRaw {
    fn default() -> Self {
        Self {
            vertex_offset: 0,
            index_offset: 0,
            vertex_count: 0,
            triangle_count: 0,
            reserved: [0; 2],
            cone_axis: MESHLET_CONE_FALLBACK_AXIS,
            cone_cutoff_cos: MESHLET_CONE_FALLBACK_CUTOFF_COS,
            bounds: [0.0; 4],
            error: 0.0,
        }
    }
}

/// 层级 LOD 的一个组（R86）：DAG 节点 = 一段自有簇范围 + 子组索引 +
/// 屏幕空间误差选择的全部输入。
///
/// 构建口径（`iris_engine::meshlet_builder`）：`own_error` = 本组简化时
/// 实际使用的体素 cell × √3/2（格对角半长 = 合并位移上界）；
/// `group_error = max(own_error, 所有 children 的 group_error)` 沿 DAG
/// 向上单调增——父组误差不低于任何子组，选择的「误差单调 ⇒ 距离单调
/// ⇒ 粗细单调」性质的根据。`bounds` / cone 与簇元数据同口径（xyz =
/// 球心相对网格包围盒归一、w = 半径世界单位；锥 = 自有几何面法线均值）。
#[derive(Clone, Debug, PartialEq)]
pub struct MeshletLodGroup {
    /// 子组索引（升序；叶组 = 空）。子组号恒小于父组号（构建序——低层
    /// 先建），容器读侧校验，遍历深度天然有界。
    pub children: Vec<u32>,
    /// 本组自有簇在全局簇数组的连续范围 `[cluster_start, cluster_start +
    /// cluster_count)`（叶组 = 单个叶簇；父组 = 简化几何的重聚类产物）。
    pub cluster_start: u32,
    /// 自有簇数（≥ 1——空组没有绘制语义，构建期不产生）。
    pub cluster_count: u32,
    /// 本组简化引入的误差（相对 children 几何的位移上界，世界单位）。
    pub own_error: f32,
    /// 组误差 = max(own, 所有 children 的 group_error)——选择的标量。
    pub group_error: f32,
    /// 自有几何包围球：xyz = 球心（相对网格包围盒归一）、w = 半径（米）。
    pub bounds: [f32; 4],
    /// 自有几何边界锥轴（归一化；口径同 [`MeshletClusterRaw::cone_axis`]，
    /// R86 CPU 选择不消费，随容器持久化供后续 GPU 选择）。
    pub cone_axis: [f32; 3],
    /// 自有几何边界锥 cutoff（同上——持久化字段，CPU 选择不消费）。
    pub cone_cutoff_cos: f32,
}

/// 簇 DAG（R86）：组数组 + 根索引 + 叶簇数。
///
/// 空（groups 为空）= 单层 = 全部簇都是叶簇（v1 容器 / `--no-lod`），
/// 此时选择恒等于「全叶表」。组在 `groups` 中按**构建层序**排列：低层
/// 组在前、父组追加在后 ⇒ 「子组号 < 父组号」是容器读侧校验的不变量
/// （遍历无需访问集合、深度天然有界）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshletHierarchy {
    /// 全部组（构建层序；见类型文档的「子组号 < 父组号」不变量）。
    pub groups: Vec<MeshletLodGroup>,
    /// 根组索引（升序）。单根 = 递归到单组收敛；多根 = 提前停在三角数
    /// 阈值 / 层数上限——选择从根按升序遍历。
    pub roots: Vec<u32>,
    /// 叶簇数（叶层 = 全局簇号 `[0, 本值)`；全叶表的簇范围）。
    pub leaf_cluster_count: u32,
}

/// meshlet 网格：全局顶点 + 全局索引 + 逐簇元数据（+ R86 层级 DAG）。
///
/// - 顶点与既有网格同格式（[`Vertex`]，32 B：position / normal / uv），
///   材质与光照路径零改动复用；**簇间边界顶点不去重**（各簇各存一份，
///   索引为簇内局部号）；
/// - 簇数据在两个全局缓冲里都按簇连续、按簇构建序排布
///   （`vertex_offset` / `index_offset` 严格递增衔接）——容器读侧校验
///   此不变量；
/// - **层级布局（R86）**：叶簇占据簇号 `[0, leaf_count)`；每层父组的
///   自有簇（简化几何重聚类产物）按构建序**追加**在更低层之后 ⇒ 每个
///   组的簇范围连续、选择输出的区间合并后仍连续。`hierarchy` 为空 =
///   单层（v1 容器 / `--no-lod`），全簇即叶。
#[derive(Clone, Debug, Default)]
pub struct MeshletMesh {
    /// 全局顶点缓冲（簇拼接序；同位置顶点可能跨簇重复）。
    pub vertices: Vec<Vertex>,
    /// 全局索引缓冲（簇拼接序；值 = 簇内局部顶点号 0..vertex_count）。
    pub indices: Vec<u32>,
    /// 逐簇元数据（按簇构建序；叶簇在前，父组自有簇按层追加）。
    pub clusters: Vec<MeshletClusterRaw>,
    /// 簇 DAG（R86）。空 = 单层：全部簇都是叶簇（R84/R85 语义）。
    pub hierarchy: MeshletHierarchy,
    /// 页划分表（R87）。`None` = 未划分（v1/v2 容器 / 内存态手搓网格）
    /// ——写出落 v1/v2 布局，流送激活时按 [`MESHLET_PAGE_CLUSTERS`]
    /// 现算（与构建期写入逐位等价）。`Some` ⇒ 写出为 v3（页区段持久化）。
    pub pages: Option<MeshletPageTable>,
}

impl MeshletMesh {
    /// 叶簇数（单层 = 全部簇数；层级 = DAG 登记的叶层数量）。
    ///
    /// 渲染器「全叶表」（LOD 缺席 / 关闭）以此为簇范围——v1 容器与 R85
    /// 行为逐位一致的根据。
    pub fn leaf_count(&self) -> u32 {
        if self.hierarchy.groups.is_empty() {
            self.clusters.len() as u32
        } else {
            self.hierarchy.leaf_cluster_count
        }
    }

    /// 展开为既有渲染路径可消费的 [`MeshData`]（纯函数）。
    ///
    /// 索引翻译：全局索引缓冲存的是**簇内局部顶点号**，展开 = 逐簇加上
    /// `vertex_offset` 还原为全局号（顶点缓冲本身原样透传）。V1 的正确性
    /// 锚点：展开只是「顶点重排 + 索引重排」，三角形集合（含环绕方向）
    /// 与逐角属性值与构建输入完全一致 ⇒ 不透明几何渲染**逐字节一致**
    /// （覆盖集不变、逐角插值不变——GPU 佐证见 `tests/meshlet_render.rs`）。
    /// 蒙皮 / 切线 / UV2 缺席（V1 容器不载这些属性，见模块头 MVP 边界）。
    pub fn expand(&self) -> MeshData {
        let mut indices = Vec::with_capacity(self.indices.len());
        for cluster in &self.clusters {
            let start = cluster.index_offset as usize;
            let end = start + cluster.triangle_count as usize * 3;
            for &local in &self.indices[start..end] {
                indices.push(cluster.vertex_offset + local);
            }
        }
        MeshData {
            vertices: self.vertices.clone(),
            indices,
            skin: None,
            tangents: None,
            uv2: None,
            morph: None,
        }
    }

    /// 写出为 IRML 字节流（含头 + CRC），返回总字节数。
    ///
    /// 版本裁决（R87）：页表在场 ⇒ v3（40 B 头 + v2 区段 + 页区段——
    /// 构建期页划分的持久化形态）；否则 `hierarchy` 为空（单层）⇒ v1
    /// 字节布局（与 R84 逐字节同构——单层容器零漂移）；带层级 ⇒ v2
    ///（36 B 头 + 组区段 + 根区段）。「`--no-lod` 单层写出」因此天然
    /// 落回 v1 布局。
    pub fn write_to(&self, mut writer: impl Write) -> std::io::Result<u64> {
        if self.pages.is_some() {
            self.write_v3(&mut writer)
        } else if self.hierarchy.groups.is_empty() {
            self.write_v1(&mut writer)
        } else {
            self.write_v2(&mut writer)
        }
    }

    /// 便捷：写出为磁盘文件，返回总字节数。
    pub fn write_file(&self, path: impl AsRef<Path>) -> std::io::Result<u64> {
        self.write_to(File::create(path)?)
    }

    /// 从内存字节解析（校验魔数 / 版本 / 长度 / CRC / 索引与簇范围）。
    ///
    /// 版本矩阵（R87）：v1 = R84 单层布局（照常读取，层级为空 = 全叶，
    /// 页表 = 无——流送激活时按同参数现算）；v2 = 层级布局（头 36 B +
    /// 组 / 根区段，页表同 v1 现算）；v3 = v2 + 页区段（构建期页划分
    /// 持久化，逐字段校验后原样载入）；其余版本一律拒绝（R84 语义——
    /// 拒读而非误读）。
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, MeshletAssetError> {
        if bytes.len() < HEADER_LEN {
            return Err(MeshletAssetError::Corrupt(format!(
                "文件不足 {HEADER_LEN} B 头部长度（实际 {} B）",
                bytes.len()
            )));
        }
        if bytes[0..4] != MAGIC {
            return Err(MeshletAssetError::BadMagic);
        }
        let version = u32::from_le_bytes(bytes[4..8].try_into().expect("版本字段 4 字节"));
        match version {
            VERSION_V1 => Self::decode_v1(bytes),
            VERSION => Self::decode_v2(bytes),
            VERSION_V3 => Self::decode_v3(bytes),
            other => Err(MeshletAssetError::UnsupportedVersion(other)),
        }
    }

    /// v1（R84 单层布局）解码：24 B 头 + 顶点 | 索引 | 簇。层级置空 =
    /// 单层（全簇即叶，[`MeshletMesh::leaf_count`] 回落簇总数）。
    fn decode_v1(bytes: &[u8]) -> Result<Self, MeshletAssetError> {
        let vertex_count =
            u32::from_le_bytes(bytes[8..12].try_into().expect("顶点数字段 4 字节")) as usize;
        let index_count =
            u32::from_le_bytes(bytes[12..16].try_into().expect("索引数字段 4 字节")) as usize;
        let cluster_count =
            u32::from_le_bytes(bytes[16..20].try_into().expect("簇数字段 4 字节")) as usize;
        let expected_crc = u32::from_le_bytes(bytes[20..24].try_into().expect("CRC 字段 4 字节"));
        let data = &bytes[HEADER_LEN..];
        let (vertices, indices, clusters, _) = Self::decode_common(
            data,
            vertex_count,
            index_count,
            cluster_count,
            expected_crc,
            true,
        )?;
        Ok(Self {
            vertices,
            indices,
            clusters,
            hierarchy: MeshletHierarchy::default(),
            pages: None,
        })
    }

    /// v2（R86 层级布局）解码：36 B 头（v1 字段 + 叶簇数 / 组数 / 根数）
    /// + 顶点 | 索引 | 簇 | 组 | 根。
    fn decode_v2(bytes: &[u8]) -> Result<Self, MeshletAssetError> {
        let (mesh, _cursor) = Self::decode_v2_body(bytes, false)?;
        Ok(mesh)
    }

    /// v3（R87 分页布局）解码：40 B 头（v2 头 + 每页簇数，CRC 后移）
    /// + 顶点 | 索引 | 簇 | 组 | 根 | 页区段。
    fn decode_v3(bytes: &[u8]) -> Result<Self, MeshletAssetError> {
        let (mesh, _) = Self::decode_v2_body(bytes, true)?;
        Ok(mesh)
    }

    /// v2 / v3 共享主体：头读取（长度按 `v3` 分档）+ 数据区 + 层级区段
    ///（v3 续读页区段）。返回（网格, 数据区消费游标）。
    fn decode_v2_body(bytes: &[u8], v3: bool) -> Result<(Self, usize), MeshletAssetError> {
        let header_len = if v3 { HEADER_LEN_V3 } else { HEADER_LEN_V2 };
        if bytes.len() < header_len {
            return Err(MeshletAssetError::Corrupt(format!(
                "v{} 文件不足 {header_len} B 头部长度（实际 {} B）",
                if v3 { 3 } else { 2 },
                bytes.len()
            )));
        }
        let read_u32 = |offset: usize| {
            u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("头字段 4 字节"))
        };
        let vertex_count = read_u32(8) as usize;
        let index_count = read_u32(12) as usize;
        let cluster_count = read_u32(16) as usize;
        let leaf_cluster_count = read_u32(20);
        let group_count = read_u32(24) as usize;
        let root_count = read_u32(28) as usize;
        let clusters_per_page = if v3 { read_u32(32) } else { 0 };
        let expected_crc = read_u32(if v3 { 36 } else { 32 });
        if group_count > 2 * MAX_MESHLET_CLUSTERS {
            return Err(MeshletAssetError::Corrupt(format!(
                "组数 {group_count} 超出上限（2 × {MAX_MESHLET_CLUSTERS}）"
            )));
        }
        if v3 && clusters_per_page == 0 {
            return Err(MeshletAssetError::Corrupt(
                "每页簇数为 0（页划分无意义）".to_string(),
            ));
        }
        let data = &bytes[header_len..];
        let (vertices, indices, clusters, cursor) = Self::decode_common(
            data,
            vertex_count,
            index_count,
            cluster_count,
            expected_crc,
            false,
        )?;
        // 叶簇数校验：叶层是全叶表的簇范围，必须落在簇数组内。
        let cluster_total = clusters.len() as u32;
        let leaf_ok = if cluster_total == 0 {
            leaf_cluster_count == 0
        } else {
            (1..=cluster_total).contains(&leaf_cluster_count)
        };
        if !leaf_ok {
            return Err(MeshletAssetError::Corrupt(format!(
                "叶簇数 {leaf_cluster_count} 越界（簇总数 {cluster_total}）"
            )));
        }
        // 组区段：变长记录（children 数 + children + 定长字段），全程
        // 边界校验；CRC 兜底损坏，「结构合法但语义错位」在此拦死。
        let (groups, mut cursor) = decode_group_section(data, group_count, cluster_total, cursor)?;
        // 根区段 + 长度闭合：v2 在根区段后精确闭合（尾随垃圾 = 损坏，
        // 校验先于根解析 = R86 错误优先级）；v3 在页区段后闭合。
        if !v3 && cursor + root_count * 4 != data.len() {
            return Err(MeshletAssetError::Corrupt(format!(
                "数据区长度不符：组/根区段消费 {} B，实际 {} B",
                cursor + root_count * 4,
                data.len()
            )));
        }
        let mut roots = Vec::with_capacity(root_count);
        for r in 0..root_count {
            let root =
                u32::from_le_bytes(data[cursor..cursor + 4].try_into().expect("根字段 4 字节"));
            cursor += 4;
            if root as usize >= group_count {
                return Err(MeshletAssetError::Corrupt(format!(
                    "根 #{r} 组号 {root} 越界（组总数 {group_count}）"
                )));
            }
            roots.push(root);
        }
        if !groups.is_empty() && roots.is_empty() {
            return Err(MeshletAssetError::Corrupt(
                "层级非空但根列表为空（选择无从遍历）".to_string(),
            ));
        }
        // v3 页区段：页数 + 逐页（顶点 / 索引）区间 + 逐簇组号——全部
        // 与簇元数据 / 层级的导出值核对（可导出字段的错位在此拦死）。
        let pages = if v3 {
            let table = decode_page_section(
                data,
                &mut cursor,
                &clusters,
                clusters_per_page,
                group_count,
                &groups,
            )?;
            Some(table)
        } else {
            None
        };
        if cursor != data.len() {
            return Err(MeshletAssetError::Corrupt(format!(
                "数据区长度不符：{}区段消费 {} B，实际 {} B",
                if v3 { "组/根/页" } else { "组/根" },
                cursor,
                data.len()
            )));
        }
        let mesh = Self {
            vertices,
            indices,
            clusters,
            hierarchy: MeshletHierarchy {
                groups,
                roots,
                leaf_cluster_count,
            },
            pages,
        };
        Ok((mesh, cursor + header_len))
    }

    /// v1 / v2 共享的顶点 | 索引 | 簇区段解码（R84 校验链逐条保留：
    /// 长度精确 → CRC → 簇上限 → 偏移连续 / 越界 / 局部索引 / 保留位）。
    /// 返回三区段 + 消费后的游标（v2 从此续读组 / 根）。`exact_len` =
    /// v1（数据区在簇区段后精确闭合；长度检查先于 CRC，R84 错误优先
    /// 级）；v2 传 false（组 / 根区段变长，CRC 覆盖整个数据区，尾部闭
    /// 合由 decode_v2 解析后校验）。
    #[allow(clippy::type_complexity)] // 区段三元组 + 游标——一次性解码产物
    fn decode_common(
        data: &[u8],
        vertex_count: usize,
        index_count: usize,
        cluster_count: usize,
        expected_crc: u32,
        exact_len: bool,
    ) -> Result<(Vec<Vertex>, Vec<u32>, Vec<MeshletClusterRaw>, usize), MeshletAssetError> {
        if cluster_count > MAX_MESHLET_CLUSTERS {
            return Err(MeshletAssetError::Corrupt(format!(
                "簇数 {cluster_count} 超出单帧剔除容量上限 {MAX_MESHLET_CLUSTERS}（R85 storage \
                 与紧凑化缓冲按上限定长）"
            )));
        }

        let expect_len = vertex_count * std::mem::size_of::<Vertex>()
            + index_count * std::mem::size_of::<u32>()
            + cluster_count * std::mem::size_of::<MeshletClusterRaw>();
        let length_bad = if exact_len {
            data.len() != expect_len
        } else {
            data.len() < expect_len
        };
        if length_bad {
            return Err(MeshletAssetError::Corrupt(format!(
                "数据区长度不符：期望 {}{expect_len} B（顶点 {vertex_count} × 32 + 索引 \
                 {index_count} × 4 + 簇 {cluster_count} × 48），实际 {} B",
                if exact_len { "" } else { "≥ " },
                data.len()
            )));
        }
        let actual_crc = crc32(data);
        if actual_crc != expected_crc {
            return Err(MeshletAssetError::ChecksumMismatch {
                expected: expected_crc,
                actual: actual_crc,
            });
        }

        // 顶点区段：逐顶点 8 × f32 小端解码（position 3 + normal 3 + uv 2）。
        let mut vertices = Vec::with_capacity(vertex_count);
        let mut cursor = 0usize;
        for _ in 0..vertex_count {
            let mut v = Vertex {
                position: [0.0; 3],
                normal: [0.0; 3],
                uv: [0.0; 2],
            };
            for slot in v
                .position
                .iter_mut()
                .chain(v.normal.iter_mut())
                .chain(v.uv.iter_mut())
            {
                *slot = f32::from_le_bytes(
                    data[cursor..cursor + 4]
                        .try_into()
                        .expect("f32 字段 4 字节"),
                );
                cursor += 4;
            }
            vertices.push(v);
        }

        // 索引区段：u32 小端解码（范围校验在逐簇循环里做——全局校验挡不住
        // 「局部号越过本簇顶点数」的错位，那才是会渲染出野数据的地方）。
        let mut indices = Vec::with_capacity(index_count);
        for _ in 0..index_count {
            let idx = u32::from_le_bytes(
                data[cursor..cursor + 4]
                    .try_into()
                    .expect("索引字段 4 字节"),
            );
            cursor += 4;
            indices.push(idx);
        }

        // 簇区段：逐字段小端解码 + 范围 / 连续性校验。
        let mut clusters = Vec::with_capacity(cluster_count);
        let mut expect_vertex_offset = 0u32;
        let mut expect_index_offset = 0u32;
        for c in 0..cluster_count {
            let read_u32 = |offset: usize| {
                u32::from_le_bytes(
                    data[cursor + offset..cursor + offset + 4]
                        .try_into()
                        .expect("簇字段 4 字节"),
                )
            };
            let read_u8 = |offset: usize| data[cursor + offset];
            let read_f32 = |offset: usize| {
                f32::from_le_bytes(
                    data[cursor + offset..cursor + offset + 4]
                        .try_into()
                        .expect("簇字段 4 字节"),
                )
            };
            let cluster = MeshletClusterRaw {
                vertex_offset: read_u32(0),
                index_offset: read_u32(4),
                vertex_count: read_u8(8),
                triangle_count: read_u8(9),
                reserved: [read_u8(10), read_u8(11)],
                cone_axis: [read_f32(12), read_f32(16), read_f32(20)],
                cone_cutoff_cos: read_f32(24),
                bounds: [read_f32(28), read_f32(32), read_f32(36), read_f32(40)],
                error: read_f32(44),
            };
            cursor += std::mem::size_of::<MeshletClusterRaw>();

            if cluster.reserved != [0; 2] {
                return Err(MeshletAssetError::Corrupt(format!(
                    "簇 #{c} 保留位非零（{:?}）——损坏或来自更高版本的格式",
                    cluster.reserved
                )));
            }

            if cluster.vertex_count as usize > MESHLET_CLUSTER_MAX_VERTICES {
                return Err(MeshletAssetError::Corrupt(format!(
                    "簇 #{c} 顶点数 {} 超上限 {MESHLET_CLUSTER_MAX_VERTICES}",
                    cluster.vertex_count
                )));
            }
            if cluster.triangle_count as usize > MESHLET_CLUSTER_MAX_TRIANGLES {
                return Err(MeshletAssetError::Corrupt(format!(
                    "簇 #{c} 三角形数 {} 超上限 {MESHLET_CLUSTER_MAX_TRIANGLES}",
                    cluster.triangle_count
                )));
            }
            // 连续性：簇按文件序严丝合缝衔接（写侧承诺——偏离即损坏，
            // 也保证 R85 逐簇绘制无需查表）。
            if cluster.vertex_offset != expect_vertex_offset
                || cluster.index_offset != expect_index_offset
            {
                return Err(MeshletAssetError::Corrupt(format!(
                    "簇 #{c} 偏移不连续：期望顶点 {expect_vertex_offset} / 索引 \
                     {expect_index_offset}，实际 {} / {}",
                    cluster.vertex_offset, cluster.index_offset
                )));
            }
            if cluster.vertex_offset as usize + cluster.vertex_count as usize > vertex_count {
                return Err(MeshletAssetError::Corrupt(format!(
                    "簇 #{c} 顶点区间越界：{} + {} > {vertex_count}",
                    cluster.vertex_offset, cluster.vertex_count
                )));
            }
            let index_start = cluster.index_offset as usize;
            let index_end = index_start + cluster.triangle_count as usize * 3;
            if index_end > index_count {
                return Err(MeshletAssetError::Corrupt(format!(
                    "簇 #{c} 索引区间越界：{} + {} × 3 > {index_count}",
                    cluster.index_offset, cluster.triangle_count
                )));
            }
            // 簇内局部索引必须落在本簇顶点数内（展开 = 局部号 + vertex_offset，
            // 越界局部号会还原成野全局号——在读侧拦死）。
            for &local in &indices[index_start..index_end] {
                if local as usize >= cluster.vertex_count as usize {
                    return Err(MeshletAssetError::Corrupt(format!(
                        "簇 #{c} 局部索引 {local} 越界（簇顶点数 {}）",
                        cluster.vertex_count
                    )));
                }
            }
            expect_vertex_offset += cluster.vertex_count as u32;
            expect_index_offset += cluster.triangle_count as u32 * 3;
            clusters.push(cluster);
        }

        Ok((vertices, indices, clusters, cursor))
    }

    /// v1 写出（R84 单层布局，24 B 头）——与 R84 逐字节同构。
    fn write_v1(&self, mut writer: impl Write) -> std::io::Result<u64> {
        let data = self.encode_data_region();
        writer.write_all(&MAGIC)?;
        writer.write_all(&VERSION_V1.to_le_bytes())?;
        writer.write_all(&(self.vertices.len() as u32).to_le_bytes())?;
        writer.write_all(&(self.indices.len() as u32).to_le_bytes())?;
        writer.write_all(&(self.clusters.len() as u32).to_le_bytes())?;
        writer.write_all(&crc32(&data).to_le_bytes())?;
        writer.write_all(&data)?;
        Ok(HEADER_LEN as u64 + data.len() as u64)
    }

    /// v2 写出（R86 层级布局，36 B 头 + 组区段 + 根区段）。
    fn write_v2(&self, mut writer: impl Write) -> std::io::Result<u64> {
        assert!(
            self.hierarchy.groups.len() <= 2 * MAX_MESHLET_CLUSTERS,
            "IRML v2 组数计数字段超出防护上限"
        );
        let mut data = self.encode_data_region();
        data.extend_from_slice(&self.encode_hierarchy_region());
        for &root in &self.hierarchy.roots {
            data.extend_from_slice(&root.to_le_bytes());
        }
        writer.write_all(&MAGIC)?;
        writer.write_all(&VERSION.to_le_bytes())?;
        writer.write_all(&(self.vertices.len() as u32).to_le_bytes())?;
        writer.write_all(&(self.indices.len() as u32).to_le_bytes())?;
        writer.write_all(&(self.clusters.len() as u32).to_le_bytes())?;
        writer.write_all(&self.hierarchy.leaf_cluster_count.to_le_bytes())?;
        writer.write_all(&(self.hierarchy.groups.len() as u32).to_le_bytes())?;
        writer.write_all(&(self.hierarchy.roots.len() as u32).to_le_bytes())?;
        writer.write_all(&crc32(&data).to_le_bytes())?;
        writer.write_all(&data)?;
        Ok(HEADER_LEN_V2 as u64 + data.len() as u64)
    }

    /// v3 写出（R87 分页布局，40 B 头 + 组区段 + 根区段 + 页区段）。
    ///
    /// 叶簇数归一：单层网格（空层级）的叶数写**簇总数**——读侧叶校验
    /// 要求叶 ∈ [1, 簇总数]，而空层级的字段默认值是 0（R84 遗留）；
    /// 归一只影响 v3 字节流，运行期 [`MeshletMesh::leaf_count`] 对空
    /// 层级本就回落簇总数，语义零漂移。
    fn write_v3(&self, mut writer: impl Write) -> std::io::Result<u64> {
        let pages = self.pages.as_ref().expect("v3 写出前提：页表在场");
        assert!(
            self.hierarchy.groups.len() <= 2 * MAX_MESHLET_CLUSTERS,
            "IRML v3 组数计数字段超出防护上限"
        );
        assert!(
            pages.cluster_count() as usize == self.clusters.len()
                && pages.cluster_group().len() == self.clusters.len(),
            "页表与簇数组不一致（v3 写出前提破坏）"
        );
        let mut data = self.encode_data_region();
        data.extend_from_slice(&self.encode_hierarchy_region());
        for &root in &self.hierarchy.roots {
            data.extend_from_slice(&root.to_le_bytes());
        }
        // 页区段：页数 + 逐页（顶点 / 索引）区间 + 逐簇组号（全 LE，
        // 与 decode_v2_body 的页区段解析逐字段对偶）。
        data.extend_from_slice(&pages.page_count().to_le_bytes());
        for p in 0..pages.page_count() {
            let (vs, vc) = pages.page_vertex_range(p);
            let (is, ic) = pages.page_index_range(p);
            data.extend_from_slice(&vs.to_le_bytes());
            data.extend_from_slice(&vc.to_le_bytes());
            data.extend_from_slice(&is.to_le_bytes());
            data.extend_from_slice(&ic.to_le_bytes());
        }
        for &g in pages.cluster_group() {
            data.extend_from_slice(&g.to_le_bytes());
        }
        let leaf = if self.hierarchy.groups.is_empty() {
            self.clusters.len() as u32
        } else {
            self.hierarchy.leaf_cluster_count
        };
        writer.write_all(&MAGIC)?;
        writer.write_all(&VERSION_V3.to_le_bytes())?;
        writer.write_all(&(self.vertices.len() as u32).to_le_bytes())?;
        writer.write_all(&(self.indices.len() as u32).to_le_bytes())?;
        writer.write_all(&(self.clusters.len() as u32).to_le_bytes())?;
        writer.write_all(&leaf.to_le_bytes())?;
        writer.write_all(&(self.hierarchy.groups.len() as u32).to_le_bytes())?;
        writer.write_all(&(self.hierarchy.roots.len() as u32).to_le_bytes())?;
        writer.write_all(&pages.clusters_per_page().to_le_bytes())?;
        writer.write_all(&crc32(&data).to_le_bytes())?;
        writer.write_all(&data)?;
        Ok(HEADER_LEN_V3 as u64 + data.len() as u64)
    }

    /// 组区段编码（v2 / v3 共享，逐字节同构）。
    fn encode_hierarchy_region(&self) -> Vec<u8> {
        let mut data = Vec::new();
        for g in &self.hierarchy.groups {
            data.extend_from_slice(&(g.children.len() as u32).to_le_bytes());
            for &child in &g.children {
                data.extend_from_slice(&child.to_le_bytes());
            }
            data.extend_from_slice(&g.cluster_start.to_le_bytes());
            data.extend_from_slice(&g.cluster_count.to_le_bytes());
            data.extend_from_slice(&g.own_error.to_le_bytes());
            data.extend_from_slice(&g.group_error.to_le_bytes());
            for slot in g.bounds {
                data.extend_from_slice(&slot.to_le_bytes());
            }
            for slot in g.cone_axis {
                data.extend_from_slice(&slot.to_le_bytes());
            }
            data.extend_from_slice(&g.cone_cutoff_cos.to_le_bytes());
        }
        data
    }

    /// 打开磁盘上的 IRML 文件（整文件读入——非流式，见模块头）。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MeshletAssetError> {
        let bytes = std::fs::read(path)?;
        Self::from_bytes(&bytes)
    }

    /// 数据区编码：顶点（逐字段小端）| 索引（u32 小端）| 簇（逐字段小端）。
    fn encode_data_region(&self) -> Vec<u8> {
        assert!(
            self.vertices.len() <= u32::MAX as usize
                && self.indices.len() <= u32::MAX as usize
                && self.clusters.len() <= u32::MAX as usize,
            "IRML 计数字段为 u32：区段长度超界"
        );
        let mut data = Vec::with_capacity(
            self.vertices.len() * std::mem::size_of::<Vertex>()
                + self.indices.len() * 4
                + self.clusters.len() * std::mem::size_of::<MeshletClusterRaw>(),
        );
        // 顶点：32 B = position(3) + normal(3) + uv(2)，逐 f32 小端。
        for v in &self.vertices {
            for slot in v.position.iter().chain(v.normal.iter()).chain(v.uv.iter()) {
                data.extend_from_slice(&slot.to_le_bytes());
            }
        }
        for &idx in &self.indices {
            data.extend_from_slice(&idx.to_le_bytes());
        }
        for c in &self.clusters {
            data.extend_from_slice(&c.vertex_offset.to_le_bytes());
            data.extend_from_slice(&c.index_offset.to_le_bytes());
            data.push(c.vertex_count);
            data.push(c.triangle_count);
            data.extend_from_slice(&c.reserved);
            for slot in c.cone_axis {
                data.extend_from_slice(&slot.to_le_bytes());
            }
            data.extend_from_slice(&c.cone_cutoff_cos.to_le_bytes());
            for slot in c.bounds {
                data.extend_from_slice(&slot.to_le_bytes());
            }
            data.extend_from_slice(&c.error.to_le_bytes());
        }
        data
    }
}

// ---------------------------------------------------------------------------
// 簇元数据纯函数（构建器与 R85 剔除共用；无状态、逐位确定）
// ---------------------------------------------------------------------------

/// 簇包围球：均值球心 + 最大半径。
///
/// 选型依据：Ritter 迭代近似也要两遍扫描且不保证更紧；「均值 + 最大半径」
/// 单遍、天然包含全部簇顶点（簇三角形在顶点凸包内 ⇒ 全含）、纯 f32
/// 加减乘 `sqrt` 逐位确定。求和序 = 输入序（簇内顶点发现序），同输入
/// 两跑逐位一致。
///
/// 空簇 panic（簇至少含一个种子的 3 个顶点，调用方保证）。
pub fn cluster_bounding_sphere(positions: &[Vec3]) -> (Vec3, f32) {
    assert!(!positions.is_empty(), "空簇没有包围球");
    let mut sum = Vec3::ZERO;
    for p in positions {
        sum += *p;
    }
    let center = sum / positions.len() as f32;
    let mut radius = 0.0f32;
    for p in positions {
        radius = radius.max(center.distance(*p));
    }
    (center, radius)
}

/// 簇边界锥：面法线均值归一 + `cutoff = min(axis·n_i)`（截断 [-1, 1]）。
///
/// 输入为簇内**有效**面法线（调用方预过滤退化三角——叉积长度近零者），
/// 顺序 = 簇内三角形序（确定性累加序）。退化防护（模块头口径）：
/// 空输入或均值长度 < 1e-6（法线完全对消）⇒ 返回全向锥
/// （[`MESHLET_CONE_FALLBACK_AXIS`]，cutoff = −1）——永不背面剔除，
/// 保守方向。
pub fn cluster_cone(face_normals: &[Vec3]) -> ([f32; 3], f32) {
    if face_normals.is_empty() {
        return (MESHLET_CONE_FALLBACK_AXIS, MESHLET_CONE_FALLBACK_CUTOFF_COS);
    }
    let mut sum = Vec3::ZERO;
    for n in face_normals {
        sum += *n;
    }
    let mean = sum / face_normals.len() as f32;
    if mean.length() < 1e-6 {
        return (MESHLET_CONE_FALLBACK_AXIS, MESHLET_CONE_FALLBACK_CUTOFF_COS);
    }
    let axis = mean.normalize();
    let mut cutoff = f32::INFINITY;
    for n in face_normals {
        cutoff = cutoff.min(axis.dot(*n));
    }
    (axis.to_array(), cutoff.clamp(-1.0, 1.0))
}

/// 簇包围球归一化：球心相对网格包围盒归一到 [0, 1]^3，半径保持世界单位。
///
/// 退化轴防护：网格在某轴 extent = 0（全平面 / 全线）时该轴除数取 1.0
/// （除零保护，归一化值落 0——该轴本无区分度）。
pub fn cluster_bounds_normalized(
    center: Vec3,
    radius: f32,
    mesh_bounds_min: Vec3,
    mesh_bounds_extent: Vec3,
) -> [f32; 4] {
    let safe_extent = Vec3::new(
        if mesh_bounds_extent.x > 0.0 {
            mesh_bounds_extent.x
        } else {
            1.0
        },
        if mesh_bounds_extent.y > 0.0 {
            mesh_bounds_extent.y
        } else {
            1.0
        },
        if mesh_bounds_extent.z > 0.0 {
            mesh_bounds_extent.z
        } else {
            1.0
        },
    );
    let normalized = (center - mesh_bounds_min) / safe_extent;
    [normalized.x, normalized.y, normalized.z, radius]
}

// ---------------------------------------------------------------------------
// R85 GPU 剔除谓词（CPU 锚点实现；`shaders/meshlet_cull.wgsl` 逐式镜像，
// 两侧必须同步修改——与 spot_cone_factor 的同步契约同款纪律）
// ---------------------------------------------------------------------------

/// 背面簇判定（一线程一簇的布尔谓词，逐簇独立 ⇒ 逐簇布尔稳定）。
///
/// 输入 `to_eye` = 簇包围球世界球心 → 相机眼点的方向，`axis` = 世界空间
/// 锥轴（模型矩阵旋转部分变换后**归一化**）——两者都必须已归一化；
/// `cutoff_cos` = 簇元数据的 `cone_cutoff_cos`（局部空间口径，随刚体旋转
/// 不变量）。
///
/// 语义推导（R84 模块头「视线方向与 axis 点积小于 cutoff 时整体背面」的
/// 松散表述在此定稿为可执行的精确判定）：`cutoff = min_i(axis·n_i)` ⇒
/// 全部簇内面法线与轴的夹角 ≤ θc = acos(cutoff)。视线方向 d 下簇内法线
/// 的最大前途 = max_i(d·n_i) = cos(max(0, ∠(d, axis) − θc))（球冠上的
/// 最大点积在 d 与轴所张平面内、靠 d 一侧取得）。**全部背面 ⇔ max ≤ 0
/// ⇔ ∠(d, axis) ≥ 90° + θc ⇔ d·axis ≤ −sin(θc)**。因此：
///
/// - `cutoff ≤ 0`（θc ≥ 90°，含全向退化锥）时条件恒假 ⇒ **永不剔除**
///   （保守方向，退化防护口径）；
/// - 阈值取严格小于（相等点判存活）：`d·axis == −sin θc` 时最接近的前途
///   法线恰好垂直视线（零面积投影），剔除与否像素等价，取保守一侧。
///
/// 局部轴经**非均匀缩放**变换后圆形锥不再是圆锥，归一化 + 原 cutoff 的
/// 组合可能不再严格保守——v1 按均匀 / 近均匀缩放假设使用（诚实限制
/// §8.54；GPU 像素等价测试用均匀缩放场景）。
pub fn cluster_is_backfacing(to_eye: Vec3, axis: Vec3, cutoff_cos: f32) -> bool {
    if cutoff_cos <= 0.0 {
        return false;
    }
    let sin_theta_c = (1.0 - cutoff_cos * cutoff_cos).max(0.0).sqrt();
    to_eye.dot(axis) < -sin_theta_c
}

/// 紧凑化扫描的 CPU 逐式镜像（WGSL `main_compact` 同形）：单工作组
/// [`MESHLET_COMPACT_THREADS`] 线程，每线程顺序扫描本段
/// （[`MESHLET_COMPACT_CHUNK`] 个 flag），段总和做 Hillis-Steele 包含式
/// 扫描得段基址，第二遍按「段基址 + 段内游标」输出存活者的全局簇号。
///
/// 返回值 = 存活簇号**升序**排列（stable compaction——绘制序确定性的
/// 根据）。u32 加法结合精确 ⇒ 扫描树形状不影响结果（与浮点扫描的本质
/// 区别，见模块头）；`flag` 以 `& 1` 钳制消费（与 WGSL 同式——非 0/1
/// 输入只可能来自越界写，钳制保证不外溢）。
pub fn chunked_prefix_compact(flags: &[u32]) -> Vec<u32> {
    assert!(
        flags.len() <= MAX_MESHLET_CLUSTERS,
        "flag 数组超出单工作组扫描容量 {MAX_MESHLET_CLUSTERS}"
    );
    const THREADS: usize = MESHLET_COMPACT_THREADS as usize;
    const CHUNK: usize = MESHLET_COMPACT_CHUNK as usize;
    // 第一遍：逐段顺序累加（段内顺序 = 全局簇号升序的局部切片）
    let mut totals = [0u32; THREADS];
    for (t, total) in totals.iter_mut().enumerate() {
        let base = t * CHUNK;
        let mut run = 0u32;
        for k in 0..CHUNK {
            if base + k < flags.len() {
                run += flags[base + k] & 1;
            }
        }
        *total = run;
    }
    // 第二遍前置：段总和的包含式树形扫描（双缓冲——每步完整读上一步，
    // 与 WGSL 共享内存 + barrier 的逐步形态同构）
    let mut incl = totals;
    let mut next = [0u32; THREADS];
    let mut d = 1usize;
    while d < THREADS {
        for i in 0..THREADS {
            next[i] = if i >= d {
                incl[i].wrapping_add(incl[i - d])
            } else {
                incl[i]
            };
        }
        std::mem::swap(&mut incl, &mut next);
        d *= 2;
    }
    // 第三遍：段基址（包含式 − 自身段和 = 排他前缀）+ 段内游标写最终槽位
    let mut out = Vec::with_capacity(incl[THREADS - 1] as usize);
    for t in 0..THREADS {
        let mut slot = incl[t] - totals[t];
        let base = t * CHUNK;
        for k in 0..CHUNK {
            let g = base + k;
            if g < flags.len() && flags[g] & 1 == 1 {
                out.push(g as u32);
                slot += 1;
            }
        }
        // 镜像自检：段基址 + 段内游标恰收敛到「包含式前缀」——扫描树数学
        // 与输出排列自洽（u32 加法精确 ⇒ 树形状无关，见模块头）
        assert_eq!(slot, incl[t], "段 {t} 的最终槽位应等于其包含式前缀");
    }
    out
}

/// 顶点 → GPU storage 词重排（注册期一次性；`meshlet_draw.wgsl` 的读取
/// 下标同式镜像）。CPU 顶点 32 B 在 WGSL storage 里没有合法的 struct 映射
/// （`vec3<f32>` 对齐 16 会撑爆逐字排布），故以 `array<vec4<f32>>` 消费：
/// 每顶点 2 词 `[px,py,pz,nx]` / `[ny,nz,u,v]`（总字节数与原布局相同，
/// 仅分组不同）。索引数组无对齐问题，原样 `array<u32>` 透传。
pub fn vertex_storage_words(vertices: &[Vertex]) -> Vec<[f32; 4]> {
    let mut out = Vec::with_capacity(vertices.len() * 2);
    for v in vertices {
        out.push([v.position[0], v.position[1], v.position[2], v.normal[0]]);
        out.push([v.normal[1], v.normal[2], v.uv[0], v.uv[1]]);
    }
    out
}

/// 网格包围盒 `(min, extent)`（注册期写进逐实例条目，供剔除端把归一化
/// 球心反归一化回局部空间）。空网格返回 `None`（该网格不可实例化）。
pub fn mesh_bounds(vertices: &[Vertex]) -> Option<([f32; 3], [f32; 3])> {
    let mut iter = vertices.iter().map(|v| v.position);
    let first = iter.next()?;
    let mut min = first;
    let mut max = first;
    for p in iter {
        for i in 0..3 {
            min[i] = min[i].min(p[i]);
            max[i] = max[i].max(p[i]);
        }
    }
    Some((min, [max[0] - min[0], max[1] - min[1], max[2] - min[2]]))
}

// ---------------------------------------------------------------------------
// R86 层级 LOD：逐帧选择（CPU 纯函数）与设置资源
// ---------------------------------------------------------------------------

/// 层级 LOD 设置（R86，World 资源）。
///
/// **缺席语义**：资源缺席 / [`Self::enabled`] = false ⇒ 渲染器按「全叶
/// 表」组装帧实例表 = R85 行为逐位不变。启用后 `threshold_px` 为选中
/// 阈值（屏幕空间像素）：组的屏幕误差 ≤ 阈值 ⇒ 选中本组（停止下钻），
/// 否则下钻 children；叶组恒选自身 ⇒ `threshold_px = 0`（或负）恒细化
/// 到叶 = 全叶列表。
#[derive(Clone, Copy, Debug, iris_core::Resource)]
pub struct MeshletLodSettings {
    /// 选中阈值（屏幕空间像素）。
    pub threshold_px: f32,
    /// 是否启用层级选择（false = 全叶 = R85 行为）。
    pub enabled: bool,
}

impl Default for MeshletLodSettings {
    fn default() -> Self {
        Self {
            threshold_px: 2.0,
            enabled: false,
        }
    }
}

/// 一次选择的视口 / 实例参数（[`select_lod_cluster_ranges_in`] 输入）。
///
/// 选择在**网格局部空间**做：眼点由调用方经模型逆矩阵变换为
/// `eye_local`，组球心按「归一化球心 × 网格 extent + min」反归一化（与
/// GPU 剔除端 `meshlet_cull.wgsl` 的球心口径逐式一致）；非均匀缩放的
/// 误差放大用 `error_scale`（模型最大轴缩放——与剔除端半径放大同口径，
/// 保守方向）。
#[derive(Clone, Copy, Debug)]
pub struct MeshletLodView {
    /// 网格包围盒 min（簇 / 组球心归一化基）。
    pub bounds_min: Vec3,
    /// 网格包围盒 extent（退化轴 = 0，与剔除端同式：分量乘积归零）。
    pub bounds_extent: Vec3,
    /// 相机眼点（网格局部空间）。
    pub eye_local: Vec3,
    /// 实例最大轴缩放（误差放大，与剔除半径口径一致）。
    pub error_scale: f32,
    /// 渲染目标高度（像素）——屏幕误差的换算基准。
    pub screen_height: f32,
    /// tan(垂直半视场角)——屏幕误差的换算基准。
    pub tan_half_fov: f32,
}

/// 层级选择的区间形态：从根遍历 + 升序规范化，返回选中簇的**连续区间**
/// 列表（`[(start, count)]`，升序、互不相连、不相邻——相邻已合并）。
///
/// 区间形态是渲染器帧实例表（run 形条目）的直接输入：一个区间 = 一条
/// 实例表条目的簇范围，R85 cull / compact / draw 管线零改动消费。
pub fn select_lod_cluster_ranges_in(
    hierarchy: &MeshletHierarchy,
    total_clusters: u32,
    view: &MeshletLodView,
    threshold_px: f32,
) -> Vec<(u32, u32)> {
    // 空层级 = 单层 = 全簇即叶（v1 容器 / --no-lod 的恒等表）。
    if hierarchy.groups.is_empty() {
        return if total_clusters > 0 {
            vec![(0, total_clusters)]
        } else {
            Vec::new()
        };
    }
    let groups = &hierarchy.groups;
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    // 显式栈 DFS：根升序、子组升序访问（入栈反序弹出）。栈深度受 DAG
    // 高度约束（读侧校验「子组号 < 父组号」⇒ 天然有界），不用递归。
    let mut stack: Vec<u32> = hierarchy.roots.iter().rev().copied().collect();
    while let Some(gi) = stack.pop() {
        let Some(g) = groups.get(gi as usize) else {
            continue; // 内存态手搓层级防御式跳过（容器读侧已拦越界）
        };
        if g.children.is_empty() {
            // 叶组：恒选自身（误差为 0 也无需判定——它是绘制精度的下界）。
            ranges.push((g.cluster_start, g.cluster_count));
            continue;
        }
        // 球心反归一化（与 meshlet_cull.wgsl 同式：min + n × extent），
        // 屏幕误差 = 组误差 × 缩放 × (H/2) / (dist × tan_half)。
        let center =
            view.bounds_min + Vec3::new(g.bounds[0], g.bounds[1], g.bounds[2]) * view.bounds_extent;
        let dist = (center - view.eye_local).length().max(1e-6);
        let screen_error = g.group_error * view.error_scale * (view.screen_height * 0.5)
            / (dist * view.tan_half_fov);
        if screen_error <= threshold_px {
            ranges.push((g.cluster_start, g.cluster_count));
        } else {
            for &child in g.children.iter().rev() {
                stack.push(child);
            }
        }
    }
    // 升序规范化：选择是逐组布尔 ⇒ 输出**集合**与遍历序无关；按簇起点
    // 升序排序（u32 全序，sort_unstable_by_key 无并列歧义）后合并相邻
    // 区间 ⇒ 输出序 = 绘制紧凑序（升序），两跑逐位一致。
    ranges.sort_unstable_by_key(|&(start, _)| start);
    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
    for &(start, count) in &ranges {
        match merged.last_mut() {
            Some(last) if last.0 + last.1 == start => last.1 += count,
            _ => merged.push((start, count)),
        }
    }
    merged
}

/// 层级选择的展开形态（规格签名）：返回选中的簇号**升序**列表。
/// 空层级 = 单层 = `[0, clusters.len())` 恒等列表（= R85 全叶路径）。
pub fn select_lod_clusters(
    mesh: &MeshletMesh,
    view: &MeshletLodView,
    threshold_px: f32,
) -> Vec<u32> {
    let mut out = Vec::new();
    for (start, count) in select_lod_cluster_ranges_in(
        &mesh.hierarchy,
        mesh.clusters.len() as u32,
        view,
        threshold_px,
    ) {
        out.extend(start..start + count);
    }
    out
}

// ---------------------------------------------------------------------------
// IRML 容器：魔数 / 版本 / CRC32（与 pack.rs 同一 IEEE 802.3 定义）
// ---------------------------------------------------------------------------

/// 文件头魔数（"IRML" = IRis MeshLet）。
const MAGIC: [u8; 4] = *b"IRML";

/// 组区段解码（v2 / v3 共享，与 write 侧 `encode_hierarchy_region`
/// 逐字段对偶）：children 数 + children + 簇范围 ×2 + 误差 ×2 +
/// bounds ×4 + cone_axis ×3 + cutoff，全程边界校验；CRC 兜底损坏，
/// 「结构合法但语义错位」在此拦死。返回（组表, 消费后游标）。
fn decode_group_section(
    data: &[u8],
    group_count: usize,
    cluster_total: u32,
    mut cursor: usize,
) -> Result<(Vec<MeshletLodGroup>, usize), MeshletAssetError> {
    let mut groups = Vec::with_capacity(group_count);
    for g in 0..group_count {
        let read_u32 = |offset: usize| {
            u32::from_le_bytes(data[offset..offset + 4].try_into().expect("组字段 4 字节"))
        };
        let read_f32 = |offset: usize| {
            f32::from_le_bytes(data[offset..offset + 4].try_into().expect("组字段 4 字节"))
        };
        if cursor + 4 > data.len() {
            return Err(MeshletAssetError::Corrupt(format!("组 #{g} 截断")));
        }
        let children_len = read_u32(cursor) as usize;
        cursor += 4;
        if children_len > group_count {
            return Err(MeshletAssetError::Corrupt(format!(
                "组 #{g} 子组数 {children_len} 超出组总数 {group_count}"
            )));
        }
        if cursor + children_len * 4 + 32 > data.len() {
            return Err(MeshletAssetError::Corrupt(format!("组 #{g} 截断")));
        }
        let mut children = Vec::with_capacity(children_len);
        for slot in 0..children_len {
            let child = read_u32(cursor + slot * 4);
            // 「子组号 < 父组号」不变量（构建层序；见 MeshletHierarchy
            // 文档）——越界引用与回边在此拦死，遍历深度天然有界。
            if child as usize >= g {
                return Err(MeshletAssetError::Corrupt(format!(
                    "组 #{g} 子组号 {child} 越界（必须 < {g}）"
                )));
            }
            children.push(child);
        }
        cursor += children_len * 4;
        let cluster_start = read_u32(cursor);
        let cluster_range = read_u32(cursor + 4);
        let own_error = read_f32(cursor + 8);
        let group_error = read_f32(cursor + 12);
        let bounds = [
            read_f32(cursor + 16),
            read_f32(cursor + 20),
            read_f32(cursor + 24),
            read_f32(cursor + 28),
        ];
        cursor += 32;
        if cursor + 16 > data.len() {
            return Err(MeshletAssetError::Corrupt(format!("组 #{g} 截断")));
        }
        let cone_axis = [read_f32(cursor), read_f32(cursor + 4), read_f32(cursor + 8)];
        let cone_cutoff_cos = read_f32(cursor + 12);
        cursor += 16;
        if cluster_start as usize + cluster_range as usize > cluster_total as usize {
            return Err(MeshletAssetError::Corrupt(format!(
                "组 #{g} 簇范围越界：{} + {} > {cluster_total}",
                cluster_start, cluster_range
            )));
        }
        if cluster_range == 0 {
            return Err(MeshletAssetError::Corrupt(format!(
                "组 #{g} 自有簇数为 0（空组没有绘制语义）"
            )));
        }
        groups.push(MeshletLodGroup {
            children,
            cluster_start,
            cluster_count: cluster_range,
            own_error,
            group_error,
            bounds,
            cone_axis,
            cone_cutoff_cos,
        });
    }
    Ok((groups, cursor))
}

/// 页区段解码（v3）：页数 + 逐页（顶点 / 索引）区间 + 逐簇组号。全部
/// 字段与簇元数据 / 层级的**导出值**核对（可导出字段的错位在此拦死）：
/// 页数必须恰为 `ceil(簇总数 / 每页簇数)`，逐页区间必须与簇拼接序导出
/// 的区间逐字段一致，逐簇组号必须是哨兵或落在所指组的自有簇范围内。
/// 解析后游标前移（调用方据此做数据区闭合校验）。
fn decode_page_section(
    data: &[u8],
    cursor: &mut usize,
    clusters: &[MeshletClusterRaw],
    clusters_per_page: u32,
    group_count: usize,
    groups: &[MeshletLodGroup],
) -> Result<MeshletPageTable, MeshletAssetError> {
    let read_u32 = |offset: usize| {
        u32::from_le_bytes(data[offset..offset + 4].try_into().expect("页字段 4 字节"))
    };
    let cluster_total = clusters.len() as u32;
    let expect_pages = cluster_total.div_ceil(clusters_per_page) as usize;
    if *cursor + 4 > data.len() {
        return Err(MeshletAssetError::Corrupt("页区段截断".to_string()));
    }
    let page_count = read_u32(*cursor) as usize;
    *cursor += 4;
    if page_count != expect_pages {
        return Err(MeshletAssetError::Corrupt(format!(
            "页数 {page_count} 与簇划分不符（每页 {clusters_per_page} 簇、\
             簇总数 {cluster_total} ⇒ 期望 {expect_pages} 页）"
        )));
    }
    let mut page_vertex_start = Vec::with_capacity(page_count);
    let mut page_vertex_count = Vec::with_capacity(page_count);
    let mut page_index_start = Vec::with_capacity(page_count);
    let mut page_index_count = Vec::with_capacity(page_count);
    for p in 0..page_count {
        if *cursor + 16 > data.len() {
            return Err(MeshletAssetError::Corrupt(format!("页 #{p} 截断")));
        }
        let vs = read_u32(*cursor);
        let vc = read_u32(*cursor + 4);
        let is = read_u32(*cursor + 8);
        let ic = read_u32(*cursor + 12);
        *cursor += 16;
        // 与簇元数据导出区间核对（区间衔接性 = 容器不变量的推论）。
        let c0 = p as u32 * clusters_per_page;
        let cc = clusters_per_page.min(cluster_total - c0);
        let first = &clusters[c0 as usize];
        let last = &clusters[(c0 + cc - 1) as usize];
        let expect_vs = first.vertex_offset;
        let expect_vc = last.vertex_offset + last.vertex_count as u32 - first.vertex_offset;
        let expect_is = first.index_offset;
        let expect_ic = last.index_offset + last.triangle_count as u32 * 3 - first.index_offset;
        if vs != expect_vs || vc != expect_vc {
            return Err(MeshletAssetError::Corrupt(format!(
                "页 #{p} 顶点区间与簇元数据不符：登记 {vs}+{vc}，导出 \
                 {expect_vs}+{expect_vc}"
            )));
        }
        if is != expect_is || ic != expect_ic {
            return Err(MeshletAssetError::Corrupt(format!(
                "页 #{p} 索引区间与簇元数据不符：登记 {is}+{ic}，导出 \
                 {expect_is}+{expect_ic}"
            )));
        }
        page_vertex_start.push(vs);
        page_vertex_count.push(vc);
        page_index_start.push(is);
        page_index_count.push(ic);
    }
    let mut cluster_group = Vec::with_capacity(clusters.len());
    for c in 0..clusters.len() {
        if *cursor + 4 > data.len() {
            return Err(MeshletAssetError::Corrupt("簇组号区段截断".to_string()));
        }
        let g = read_u32(*cursor);
        *cursor += 4;
        if g != MESHLET_NO_GROUP {
            if g as usize >= group_count {
                return Err(MeshletAssetError::Corrupt(format!(
                    "簇 #{c} 组号 {g} 越界（组总数 {group_count}）"
                )));
            }
            let grp = &groups[g as usize];
            let in_range =
                c as u32 >= grp.cluster_start && (c as u32) < grp.cluster_start + grp.cluster_count;
            if !in_range {
                return Err(MeshletAssetError::Corrupt(format!(
                    "簇 #{c} 组号 {g} 与组自有簇范围 {}+{} 不符",
                    grp.cluster_start, grp.cluster_count
                )));
            }
        }
        cluster_group.push(g);
    }
    Ok(MeshletPageTable::from_parts(
        clusters_per_page,
        cluster_total,
        page_vertex_start,
        page_vertex_count,
        page_index_start,
        page_index_count,
        cluster_group,
    ))
}

/// 当前格式版本（R86 起为 2 = 层级布局；R87 起为 3 = 层级 + 页区段，
/// 见 [`VERSION_V3`]）。reader 按版本分支：v1（R84 单层布局）照常读取
/// （层级置空 = 全叶），v2 读取组 / 根区段，v3 另读页区段，其余版本
/// 拒读而非误读（R84 语义——「将来升版时在此加逐版本分支」的落地面）。
const VERSION: u32 = 2;

/// v1（R84 单层布局）版本号——写出单层容器与读取历史文件共用。
const VERSION_V1: u32 = 1;

/// v3（R87 分页布局）版本号：v2 全部区段 + 页区段（构建期页划分的
/// 持久化——页数 / 逐页顶点与索引区间 / 逐簇所属组号）。运行期页表
/// 本可由簇元数据 + 层级纯导出，持久化的价值 = 划分参数（每页簇数）
/// 随资产定稿 + 读侧免导出直接校验；v1/v2 文件照常读取，页表在流送
/// 激活时按 [`crate::meshlet_stream::MESHLET_PAGE_CLUSTERS`] 现算
///（与构建期写入逐位等价，见 `meshlet_stream` 单测）。
const VERSION_V3: u32 = 3;

/// v1 文件头固定长度：魔数 4 + 版本 4 + 顶点数 4 + 索引数 4 + 簇数 4 + CRC 4。
const HEADER_LEN: usize = 24;

/// v2 文件头固定长度：v1 头 + 叶簇数 4 + 组数 4 + 根数 4（CRC 随之后移）。
const HEADER_LEN_V2: usize = 36;

/// v3 文件头固定长度：v2 头 + 每页簇数 4（CRC 随之后移）。
const HEADER_LEN_V3: usize = 40;

/// CRC-32（IEEE 802.3，反射多项式 0xEDB88320），与 `iris-engine::pack`
/// 同一定义（跨 crate 复制而非共享依赖：pack 在 iris-engine，反向依赖
/// 会成环；表驱动 const fn 十余行，漂移风险由往返 + 损坏单测兜住）。
fn crc32(bytes: &[u8]) -> u32 {
    // 编译期生成 256 项反射表（与 pack::build_crc_table 同式）。
    const fn build_table() -> [u32; 256] {
        let mut table = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut crc = i as u32;
            let mut bit = 0;
            while bit < 8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
                bit += 1;
            }
            table[i] = crc;
            i += 1;
        }
        table
    }
    static TABLE: [u32; 256] = build_table();
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc = TABLE[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// IRML 打包 / 读取错误。
#[derive(Debug)]
pub enum MeshletAssetError {
    /// 磁盘 IO 失败。
    Io(std::io::Error),
    /// 不是 IRML 文件（魔数不符）。
    BadMagic,
    /// 文件版本不是本实现支持的版本。
    UnsupportedVersion(u32),
    /// 结构损坏：截断 / 长度不符 / 索引越界 / 簇偏移不连续或越界。
    Corrupt(String),
    /// 数据损坏：CRC-32 校验和不符（防损坏）。
    ChecksumMismatch {
        /// 头部登记的校验和。
        expected: u32,
        /// 实际算出的校验和。
        actual: u32,
    },
}

impl fmt::Display for MeshletAssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO 错误：{e}"),
            Self::BadMagic => write!(f, "不是 IRML 文件（魔数不符，期望 \"IRML\"）"),
            Self::UnsupportedVersion(v) => {
                write!(
                    f,
                    "IRML 版本 {v} 不受支持（本实现支持版本 {VERSION_V1} 单层、\
                     {VERSION} 层级与 {VERSION_V3} 分页）"
                )
            }
            Self::Corrupt(what) => write!(f, "IRML 结构损坏：{what}"),
            Self::ChecksumMismatch { expected, actual } => write!(
                f,
                "数据损坏（CRC 校验和不符）：期望 {expected:#010x} 实际 {actual:#010x}"
            ),
        }
    }
}

impl std::error::Error for MeshletAssetError {}

impl From<std::io::Error> for MeshletAssetError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 一段手动组装的标准顶点（位置 / 法线 / UV 各不相同，可抓错位）。
    fn sample_vertex(i: usize) -> Vertex {
        let f = i as f32;
        Vertex {
            position: [f, -f * 0.5, f * 2.0],
            normal: [0.0, 1.0, 0.0],
            uv: [f * 0.25, f * 0.125],
        }
    }

    /// 手动组装一个多簇 meshlet 网格（绕开构建器，容器单测自持）。
    /// 簇 0：1 个三角（顶点 0-2）；簇 1：2 个三角（顶点 3-6，其中索引
    /// 引用簇内局部号）。
    fn sample_mesh() -> MeshletMesh {
        let mut mesh = MeshletMesh::default();
        for i in 0..7 {
            mesh.vertices.push(sample_vertex(i));
        }
        // 簇 0：三角 (0,1,2)
        mesh.indices.extend_from_slice(&[0, 1, 2]);
        mesh.clusters.push(MeshletClusterRaw {
            vertex_offset: 0,
            index_offset: 0,
            vertex_count: 3,
            triangle_count: 1,
            reserved: [0; 2],
            cone_axis: [0.0, 0.0, 1.0],
            cone_cutoff_cos: 1.0,
            bounds: [0.25, 0.5, 0.75, 1.5],
            error: 0.0,
        });
        // 簇 1：三角 (0,1,3) 与 (0,3,2)（局部号），占全局顶点 3..7
        mesh.indices.extend_from_slice(&[0, 1, 3, 0, 3, 2]);
        mesh.clusters.push(MeshletClusterRaw {
            vertex_offset: 3,
            index_offset: 3,
            vertex_count: 4,
            triangle_count: 2,
            reserved: [0; 2],
            cone_axis: [1.0, 0.0, 0.0],
            cone_cutoff_cos: -0.5,
            bounds: [0.5, 0.5, 0.5, 2.5],
            error: 0.0,
        });
        mesh
    }

    /// 48 B 布局契约 + Pod 位级往返（R85 storage 前置）。
    #[test]
    fn cluster_raw_is_48_bytes_pod() {
        assert_eq!(std::mem::size_of::<MeshletClusterRaw>(), 48);
        let raw = MeshletClusterRaw {
            vertex_offset: 7,
            index_offset: 21,
            vertex_count: 200,
            triangle_count: 128,
            reserved: [0; 2],
            cone_axis: [0.5, -0.5, std::f32::consts::FRAC_1_SQRT_2],
            cone_cutoff_cos: 0.25,
            bounds: [0.1, 0.2, 0.3, 4.5],
            error: 0.0,
        };
        let bytes: &[u8] = bytemuck::bytes_of(&raw);
        assert_eq!(bytes.len(), 48);
        let back: MeshletClusterRaw = *bytemuck::from_bytes(bytes);
        assert_eq!(back, raw, "Pod 位级往返应恒等");
        // 默认值 = 全向锥兜底（保守方向，永不剔除）
        let d = MeshletClusterRaw::default();
        assert_eq!(d.cone_axis, MESHLET_CONE_FALLBACK_AXIS);
        assert_eq!(d.cone_cutoff_cos, MESHLET_CONE_FALLBACK_CUTOFF_COS);
        assert_eq!(d.error, 0.0, "V1 简化误差恒 0");
    }

    // ---- 元数据纯函数：解析解对照 ----

    /// 单三角包围球：边长 2 的等边三角，均值球心 = 重心，半径 = 重心到
    /// 顶点 = 2/√3（解析解 ±1e-5）。
    #[test]
    fn bounding_sphere_single_triangle_analytic() {
        let h = (3.0f32).sqrt(); // 高 = √3（边长 2）
        let positions = vec![
            Vec3::new(-1.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 0.0, h),
        ];
        let (center, radius) = cluster_bounding_sphere(&positions);
        assert!((center.x - 0.0).abs() < 1e-6);
        assert!((center.y - 0.0).abs() < 1e-6);
        assert!((center.z - h / 3.0).abs() < 1e-6, "球心 z = 重心 = √3/3");
        let expect = 2.0f32 / 3.0f32.sqrt(); // 2/√3
        assert!(
            (radius - expect).abs() < 1e-5,
            "半径应为重心到顶点 2/√3 = {expect}，实际 {radius}"
        );
    }

    /// 正方形四角：球心 = 对角线交点，半径 = 半对角线 √2/2（边长 1）。
    #[test]
    fn bounding_sphere_square_analytic() {
        let positions = vec![
            Vec3::new(-0.5, 0.0, -0.5),
            Vec3::new(0.5, 0.0, -0.5),
            Vec3::new(0.5, 0.0, 0.5),
            Vec3::new(-0.5, 0.0, 0.5),
        ];
        let (center, radius) = cluster_bounding_sphere(&positions);
        assert!(center.abs_diff_eq(Vec3::ZERO, 1e-6));
        let expect = (2.0f32).sqrt() / 2.0;
        assert!((radius - expect).abs() < 1e-6, "半径 = 半对角线 {expect}");
    }

    /// 平面法线簇：轴归一化、cutoff ≈ 1（正对面——背面剔除收益最大的形态）。
    #[test]
    fn cone_flat_normals_give_unit_axis_and_cutoff_one() {
        let n = Vec3::new(0.0, 0.0, 1.0);
        let normals = vec![n; 7];
        let (axis, cutoff) = cluster_cone(&normals);
        let len = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
        assert!((len - 1.0).abs() < 1e-5, "轴必须单位化：长度 {len}");
        assert!(
            (cutoff - 1.0).abs() < 1e-5,
            "正对面簇 cutoff ≈ 1，实际 {cutoff}"
        );
    }

    /// 斜面簇：cutoff 恰为 min 点积（关系断言，不预知数值）。
    #[test]
    fn cone_cutoff_equals_min_dot_of_normals() {
        let normals = vec![
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
        ];
        let (axis, cutoff) = cluster_cone(&normals);
        let axis = Vec3::from_array(axis);
        let min_dot = normals
            .iter()
            .map(|n| axis.dot(*n))
            .fold(f32::INFINITY, f32::min);
        assert!(
            (cutoff - min_dot).abs() < 1e-6,
            "cutoff 应恰为 min 点积：{cutoff} vs {min_dot}"
        );
        assert!((-1.0..=1.0).contains(&cutoff));
    }

    /// 法线完全对消（+Z 与 −Z 等量）：均值长度 0 ⇒ 全向锥兜底（永不剔除）。
    #[test]
    fn cone_opposing_normals_degenerate_to_omnidirectional() {
        let normals = vec![Vec3::Z, Vec3::NEG_Z, Vec3::Z, Vec3::NEG_Z];
        let (axis, cutoff) = cluster_cone(&normals);
        assert_eq!(axis, MESHLET_CONE_FALLBACK_AXIS);
        assert_eq!(cutoff, MESHLET_CONE_FALLBACK_CUTOFF_COS);
        // 空输入（全退化三角被预过滤）同兜底
        let (axis, cutoff) = cluster_cone(&[]);
        assert_eq!(axis, MESHLET_CONE_FALLBACK_AXIS);
        assert_eq!(cutoff, MESHLET_CONE_FALLBACK_CUTOFF_COS);
    }

    /// 包围球归一化：立方体面心 (0,0,0.5) 在 bbox [-0.5,0.5]³ 下 →
    /// (0.5, 0.5, 1.0)，半径原样透传；退化轴（extent = 0）除数取 1.0。
    #[test]
    fn bounds_normalization_maps_center_into_unit_cube() {
        let (x, y, z, r) = {
            let [a, b, c, d] = cluster_bounds_normalized(
                Vec3::new(0.0, 0.0, 0.5),
                std::f32::consts::FRAC_1_SQRT_2,
                Vec3::new(-0.5, -0.5, -0.5),
                Vec3::ONE,
            );
            (a, b, c, d)
        };
        assert!((x - 0.5).abs() < 1e-6 && (y - 0.5).abs() < 1e-6 && (z - 1.0).abs() < 1e-6);
        assert!(
            (r - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6,
            "半径不归一（世界单位）"
        );
        // 退化轴：Y extent = 0 的地面网格 → Y 除数 1.0，y 归一化值 = 0
        let [_, y, _, _] = cluster_bounds_normalized(
            Vec3::new(5.0, 0.0, -3.0),
            1.0,
            Vec3::new(0.0, 0.0, -10.0),
            Vec3::new(10.0, 0.0, 10.0),
        );
        assert!((y - 0.0).abs() < 1e-6, "退化轴除数 1.0：y = (0-0)/1 = 0");
    }

    // ---- 容器 ----

    /// 写 → 读逐字段（含位级）相等：多簇、负坐标、非零 error 位型。
    #[test]
    fn container_roundtrip_bit_exact() {
        let mesh = sample_mesh();
        let mut bytes = Vec::new();
        let total = mesh.write_to(&mut bytes).expect("写出");
        assert_eq!(total, bytes.len() as u64, "返回值应等于实际字节数");
        // 头：24 B = 数据区之前；数据区 = 7×32 + 9×4 + 2×48
        assert_eq!(bytes.len(), 24 + 7 * 32 + 9 * 4 + 2 * 48);

        let back = MeshletMesh::from_bytes(&bytes).expect("读回");
        assert_eq!(back.clusters, mesh.clusters, "簇元数据逐字段相等");
        assert_eq!(back.indices, mesh.indices, "索引逐项相等");
        assert_eq!(back.vertices.len(), mesh.vertices.len());
        for (a, b) in back.vertices.iter().zip(mesh.vertices.iter()) {
            for (x, y) in a.position.iter().zip(b.position) {
                assert_eq!(x.to_bits(), y.to_bits(), "位置位级相等");
            }
            for (x, y) in a.normal.iter().zip(b.normal) {
                assert_eq!(x.to_bits(), y.to_bits(), "法线位级相等");
            }
            for (x, y) in a.uv.iter().zip(b.uv) {
                assert_eq!(x.to_bits(), y.to_bits(), "UV 位级相等");
            }
        }
    }

    /// 磁盘路径 open 与内存 from_bytes 语义一致（唯一临时文件名：进程 id +
    /// 调用序计数——AGENTS.md 测试文件竞态纪律）。
    #[test]
    fn container_open_matches_from_bytes() {
        static CALL_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let mesh = sample_mesh();
        let dir = std::env::temp_dir().join("iris-meshlet-test");
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let seq = CALL_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = dir.join(format!("roundtrip_{}_{}.imesh", std::process::id(), seq));
        mesh.write_file(&path).expect("写文件");
        let disk = MeshletMesh::open(&path).expect("打开");
        let bytes = std::fs::read(&path).expect("读字节");
        let memory = MeshletMesh::from_bytes(&bytes).expect("内存解析");
        assert_eq!(disk.clusters, memory.clusters);
        assert_eq!(disk.indices, memory.indices);
        assert_eq!(disk.vertices.len(), memory.vertices.len());
        std::fs::remove_file(&path).ok();
    }

    /// 魔数不符 → BadMagic；版本不符 → UnsupportedVersion（拒绝而不是误读）。
    ///
    /// R87 起版本矩阵 = {1, 2, 3}（v1 单层照常读取 + v2 层级 + v3 分页），
    /// 版本 3 从拒绝名单移入兼容名单（R86 同款先例：原名单 [0, 3, 999]
    /// 的 3 位由 4 顶替）。
    #[test]
    fn container_rejects_bad_magic_and_wrong_version() {
        let mesh = sample_mesh();
        let mut bytes = Vec::new();
        mesh.write_to(&mut bytes).expect("写出");

        let mut bad_magic = bytes.clone();
        bad_magic[0] = b'X';
        assert!(matches!(
            MeshletMesh::from_bytes(&bad_magic),
            Err(MeshletAssetError::BadMagic)
        ));

        for v in [0u32, 4, 999] {
            let mut bad_version = bytes.clone();
            bad_version[4..8].copy_from_slice(&v.to_le_bytes());
            assert!(
                matches!(
                    MeshletMesh::from_bytes(&bad_version),
                    Err(MeshletAssetError::UnsupportedVersion(seen)) if seen == v
                ),
                "版本 {v} 必须被拒绝"
            );
        }
    }

    /// 数据区翻一字节 → ChecksumMismatch（期望 / 实际可判别）；截断 → Corrupt。
    #[test]
    fn container_detects_corruption_and_truncation() {
        let mesh = sample_mesh();
        let mut bytes = Vec::new();
        mesh.write_to(&mut bytes).expect("写出");

        // 翻数据区最后一字节（簇 error 字段位）
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        match MeshletMesh::from_bytes(&bytes) {
            Err(MeshletAssetError::ChecksumMismatch { expected, actual }) => {
                assert_ne!(expected, actual, "校验和应不同");
                assert_ne!(expected, 0, "期望值应来自头部登记");
            }
            other => panic!("应报 ChecksumMismatch，实际 {other:?}"),
        }

        // 截断：砍掉数据区尾部
        let truncated = bytes[..bytes.len() - 3].to_vec();
        assert!(matches!(
            MeshletMesh::from_bytes(&truncated),
            Err(MeshletAssetError::Corrupt(_))
        ));
        // 超长：尾随垃圾字节 → 长度不符 → Corrupt
        let mut bloated = bytes.clone();
        bloated.push(0);
        assert!(matches!(
            MeshletMesh::from_bytes(&bloated),
            Err(MeshletAssetError::Corrupt(_))
        ));
        // 连头都不足（10 B）
        assert!(matches!(
            MeshletMesh::from_bytes(&bytes[..10]),
            Err(MeshletAssetError::Corrupt(_))
        ));
    }

    /// 语义校验（CRC 正确的手工容器）：索引越界 / 簇顶点数超限 /
    /// 簇偏移不连续，全部 Corrupt 且可判别。
    #[test]
    fn container_rejects_semantically_invalid_data() {
        // 手工拼一个「1 簇 1 三角、但索引越界」的容器（CRC 按数据区现算）。
        let craft = |cluster: MeshletClusterRaw, indices: &[u32]| {
            let mut data = Vec::new();
            for v in 0..3 {
                let vertex = sample_vertex(v);
                for slot in vertex
                    .position
                    .iter()
                    .chain(vertex.normal.iter())
                    .chain(vertex.uv.iter())
                {
                    data.extend_from_slice(&slot.to_le_bytes());
                }
            }
            for &i in indices {
                data.extend_from_slice(&i.to_le_bytes());
            }
            data.extend_from_slice(&cluster.vertex_offset.to_le_bytes());
            data.extend_from_slice(&cluster.index_offset.to_le_bytes());
            data.push(cluster.vertex_count);
            data.push(cluster.triangle_count);
            data.extend_from_slice(&cluster.reserved);
            for slot in cluster.cone_axis {
                data.extend_from_slice(&slot.to_le_bytes());
            }
            data.extend_from_slice(&cluster.cone_cutoff_cos.to_le_bytes());
            for slot in cluster.bounds {
                data.extend_from_slice(&slot.to_le_bytes());
            }
            data.extend_from_slice(&cluster.error.to_le_bytes());

            let mut bytes = Vec::new();
            // 手工拼 legacy v1 布局（R86 起 VERSION 常量 = 2，此处按 v1
            // 字节格式现拼——被测校验链在 v1 分支内）。
            bytes.extend_from_slice(&MAGIC);
            bytes.extend_from_slice(&1u32.to_le_bytes());
            bytes.extend_from_slice(&3u32.to_le_bytes());
            bytes.extend_from_slice(&(indices.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&1u32.to_le_bytes());
            bytes.extend_from_slice(&crc32(&data).to_le_bytes());
            bytes.extend_from_slice(&data);
            bytes
        };

        // ① 索引越界（顶点只有 3 个，索引 3）
        let bytes = craft(
            MeshletClusterRaw {
                vertex_offset: 0,
                index_offset: 0,
                vertex_count: 3,
                triangle_count: 1,
                ..MeshletClusterRaw::default()
            },
            &[0, 1, 3],
        );
        assert!(matches!(
            MeshletMesh::from_bytes(&bytes),
            Err(MeshletAssetError::Corrupt(msg)) if msg.contains("越界")
        ));

        // ② 簇三角形数超上限（200 > 128；vertex_count 上限 256 对 u8 字段
        // 恒不可越——读侧保留该校验是常量契约的防御，此处测可触发的三角路）
        let bytes = craft(
            MeshletClusterRaw {
                vertex_offset: 0,
                index_offset: 0,
                vertex_count: 3,
                triangle_count: 200,
                ..MeshletClusterRaw::default()
            },
            &[0, 1, 2],
        );
        assert!(matches!(
            MeshletMesh::from_bytes(&bytes),
            Err(MeshletAssetError::Corrupt(msg)) if msg.contains("超上限")
        ));

        // ③ 簇偏移不连续（vertex_offset = 1）
        let bytes = craft(
            MeshletClusterRaw {
                vertex_offset: 1,
                index_offset: 0,
                vertex_count: 3,
                triangle_count: 1,
                ..MeshletClusterRaw::default()
            },
            &[0, 1, 2],
        );
        assert!(matches!(
            MeshletMesh::from_bytes(&bytes),
            Err(MeshletAssetError::Corrupt(msg)) if msg.contains("不连续")
        ));

        // ④ 保留位非零（来自损坏或更高版本格式）
        let bytes = craft(
            MeshletClusterRaw {
                vertex_offset: 0,
                index_offset: 0,
                vertex_count: 3,
                triangle_count: 1,
                reserved: [0, 1],
                ..MeshletClusterRaw::default()
            },
            &[0, 1, 2],
        );
        assert!(matches!(
            MeshletMesh::from_bytes(&bytes),
            Err(MeshletAssetError::Corrupt(msg)) if msg.contains("保留位")
        ));
    }

    /// 展开纯函数：手搓双簇网格展开后三角形集合（属性三元组排序比较）
    /// 与全局索引所指一致；蒙皮 / 切线 / UV2 缺席（V1 边界）。
    #[test]
    fn expand_produces_mesh_data_with_same_triangles() {
        let mesh = sample_mesh();
        let expanded = mesh.expand();
        assert!(expanded.skin.is_none() && expanded.tangents.is_none() && expanded.uv2.is_none());
        assert_eq!(expanded.vertices.len(), 7);
        assert_eq!(expanded.indices.len(), 9);
        // 展开的索引直接指向全局顶点缓冲，逐三角属性串行化后与手工
        // 期望一致（排序比较——三角形集合不变）。
        let triangle_key = |data: &MeshData, tri: &[u32]| {
            let mut key = Vec::new();
            for &i in tri {
                let v = data.vertices[i as usize];
                key.extend_from_slice(bytemuck::bytes_of(&v));
            }
            key
        };
        let mut got: Vec<Vec<u8>> = expanded
            .indices
            .chunks_exact(3)
            .map(|t| triangle_key(&expanded, t))
            .collect();
        got.sort();
        let mut want: Vec<Vec<u8>> = vec![
            triangle_key(&expanded, &[0, 1, 2]),
            triangle_key(&expanded, &[3, 4, 6]),
            triangle_key(&expanded, &[3, 6, 5]),
        ];
        want.sort();
        assert_eq!(got, want, "展开三角形集合（含环绕）应与簇定义一致");
    }

    // ---- R85：剔除谓词与紧凑化（CPU 锚点，WGSL 逐式镜像的对照极）----

    /// 48 B 簇布局的 GPU 词映射锚点（`meshlet_cull.wgsl` /
    /// `meshlet_draw.wgsl` 以 12×u32 词消费，词下标契约由本测试锁定）。
    #[test]
    fn cluster_raw_word_layout_matches_gpu_contract() {
        let raw = MeshletClusterRaw {
            vertex_offset: 7,
            index_offset: 21,
            vertex_count: 200,
            triangle_count: 128,
            reserved: [0; 2],
            cone_axis: [0.25, -0.5, 0.75],
            cone_cutoff_cos: 0.125,
            bounds: [0.1, 0.2, 0.3, 4.5],
            error: 0.0,
        };
        let words: &[u32] = bytemuck::cast_ref::<MeshletClusterRaw, [u32; 12]>(&raw);
        assert_eq!(words.len(), 12, "48 B = 12 × u32 词");
        assert_eq!(words[0], 7, "w0 = vertex_offset");
        assert_eq!(words[1], 21, "w1 = index_offset");
        assert_eq!(
            words[2],
            200 | (128u32 << 8),
            "w2 = vertex_count 低字节 | triangle_count 次字节"
        );
        for (k, expect) in [(3, 0.25f32), (4, -0.5), (5, 0.75)] {
            assert_eq!(f32::from_bits(words[k]), expect, "w{k} = cone_axis 分量");
        }
        assert_eq!(f32::from_bits(words[6]), 0.125, "w6 = cone_cutoff_cos");
        for (k, expect) in [(7, 0.1f32), (8, 0.2), (9, 0.3), (10, 4.5)] {
            assert_eq!(f32::from_bits(words[k]), expect, "w{k} = bounds 分量");
        }
        assert_eq!(f32::from_bits(words[11]), 0.0, "w11 = error");
    }

    /// 视锥平面提取与球判定解析解：正交视锥（轴对齐平面）下核对提取出的
    /// 平面常数，以及「球在锥内 / 相切 / 恰好越界」三态。平面提取
    /// （Gribb-Hartmann）复用 `Frustum::from_view_proj`（WGSL
    /// `meshlet_cull.wgsl` 的提取式与其逐式镜像，同步契约），球判定同式
    /// （所有平面 `dist >= −r` 才存活）。
    #[test]
    fn frustum_plane_extraction_and_sphere_analytic() {
        // 单位正交盒 x,y ∈ [-1,1]、z ∈ [0,1]（identity 视图 ⇒ 平面轴对齐，
        // 距离全为精确 f32）
        let proj = glam::camera::rh::proj::directx::orthographic(-1.0, 1.0, -1.0, 1.0, 0.0, 1.0);
        let frustum = crate::frustum::Frustum::from_view_proj(&proj);
        // 平面提取解析解（序 = left, right, bottom, top）。正交投影的
        // w 行 = (0,0,0,1)，left = row3 + row0 = (1,0,0,1)（语义：x ≥ −1），
        // 其余三面按「法线朝内」对称取反——以提取式逐面核对而非假设符号
        let expect = [
            ([1.0f32, 0.0, 0.0], 1.0),
            ([-1.0, 0.0, 0.0], 1.0),
            ([0.0, 1.0, 0.0], 1.0),
            ([0.0, -1.0, 0.0], 1.0),
        ];
        for (i, (n, d)) in expect.into_iter().enumerate() {
            let p = frustum.planes()[i];
            assert!(
                (p.x - n[0]).abs() < 1e-6
                    && (p.y - n[1]).abs() < 1e-6
                    && (p.z - n[2]).abs() < 1e-6
                    && (p.w - d).abs() < 1e-6,
                "平面 #{i} 应为 ({n:?}, {d})，实际 {:?}",
                (p.x, p.y, p.z, p.w)
            );
        }
        // 球在锥内（余量充足）
        assert!(frustum.intersects_sphere(Vec3::ZERO, 0.5), "中心球应存活");
        // 恰在右平面（x = 1）相切：球心 x = 1.5、r = 0.5 ⇒
        // dist = −1.5 + 1 = −0.5 = −r ⇒ 相切存活（边界归内侧）
        assert!(
            frustum.intersects_sphere(Vec3::new(1.5, 0.0, 0.5), 0.5),
            "与右平面精确相切的球应存活"
        );
        // 恰好完全越出右平面：球心 x = 1.6、r = 0.5 ⇒ dist = −0.6 < −0.5
        assert!(
            !frustum.intersects_sphere(Vec3::new(1.6, 0.0, 0.5), 0.5),
            "完全越出右平面的球应剔除"
        );
        // 四个轴向平面各验一次相切 / 越界（对称性）
        for axis in [Vec3::X, Vec3::NEG_X, Vec3::Y, Vec3::NEG_Y] {
            let tangent = axis * 1.5 + Vec3::Z * 0.5;
            assert!(
                frustum.intersects_sphere(tangent, 0.5),
                "轴 {axis:?} 平面相切球应存活"
            );
            let outside = axis * 1.6 + Vec3::Z * 0.5;
            assert!(
                !frustum.intersects_sphere(outside, 0.5),
                "轴 {axis:?} 外侧球应剔除"
            );
        }
    }

    /// 背面锥符号约定（R85 定稿谓词）：正对存活、背对剔除、阈值角 =
    /// 90° + acos(cutoff)、退化全向锥永不剔除、相等点判存活。
    #[test]
    fn backfacing_cone_sign_convention() {
        let axis = Vec3::Z;
        // 平面簇（cutoff ≈ 1，θc ≈ 0）：视线沿 +Z（正对）存活，−Z 背对剔除
        assert!(!cluster_is_backfacing(Vec3::Z, axis, 1.0 - 1e-6));
        assert!(cluster_is_backfacing(Vec3::NEG_Z, axis, 1.0 - 1e-6));
        // 垂直视线（90°）：θc = 0 时阈值恰为 90°，相等点判存活（保守侧）
        assert!(!cluster_is_backfacing(Vec3::X, axis, 1.0));
        // θc = 10°（cutoff = cos10°）：阈值角 100° —— 99° 存活、101° 剔除
        let cutoff = 10.0f32.to_radians().cos();
        let at99 = 99.0f32.to_radians();
        let at101 = 101.0f32.to_radians();
        let dir99 = Vec3::Z * at99.cos() + Vec3::X * at99.sin();
        let dir101 = Vec3::Z * at101.cos() + Vec3::X * at101.sin();
        assert!(!cluster_is_backfacing(dir99, axis, cutoff));
        assert!(cluster_is_backfacing(dir101, axis, cutoff));
        // 退化全向锥（cutoff ≤ 0）：永不剔除
        for cutoff in [-1.0f32, 0.0, -0.5] {
            assert!(
                !cluster_is_backfacing(Vec3::NEG_Z, axis, cutoff),
                "cutoff {cutoff} 退化锥必须永不剔除"
            );
        }
        // 阈值等式逐点核对：d·axis == −sin(θc) 判存活（严格小于才剔除）
        let theta = 20.0f32.to_radians();
        let cutoff = theta.cos();
        let edge = Vec3::Z * (90.0f32 + 20.0).to_radians().cos()
            + Vec3::X * (90.0f32 + 20.0).to_radians().sin();
        assert!(
            !cluster_is_backfacing(edge, axis, cutoff),
            "阈值角 110° 相等点存活"
        );
        let beyond =
            Vec3::Z * (111.0f32).to_radians().cos() + Vec3::X * (111.0f32).to_radians().sin();
        assert!(
            cluster_is_backfacing(beyond, axis, cutoff),
            "111° > 110° 阈值剔除"
        );
    }

    /// 紧凑化扫描镜像：与朴素过滤（期望排列）对照——覆盖空输入 / 段边界 /
    /// 全存活 / 全死亡 / 容量满档，并用无状态哈希流生成伪随机 flag 序列
    /// （确定性硬文化：splitmix64 + 计数器，禁 rand）。
    #[test]
    fn chunked_prefix_compact_matches_naive_reference() {
        let naive = |flags: &[u32]| -> Vec<u32> {
            flags
                .iter()
                .enumerate()
                .filter(|&(_, &f)| f & 1 == 1)
                .map(|(i, _)| i as u32)
                .collect()
        };
        // 手工形态
        assert_eq!(chunked_prefix_compact(&[]), Vec::<u32>::new(), "空输入");
        assert_eq!(
            chunked_prefix_compact(&[0; MAX_MESHLET_CLUSTERS]),
            Vec::<u32>::new(),
            "全死"
        );
        let all_alive = [1u32; MAX_MESHLET_CLUSTERS];
        assert_eq!(
            chunked_prefix_compact(&all_alive),
            naive(&all_alive),
            "全存活 = 恒等排列"
        );
        // 段边界（63/64/65、线程边界 255/256/257、末尾）
        for len in [1usize, 63, 64, 65, 255, 256, 257, MAX_MESHLET_CLUSTERS - 1] {
            let mut flags = vec![0u32; len];
            for i in (0..len).step_by(3) {
                flags[i] = 1;
            }
            assert_eq!(chunked_prefix_compact(&flags), naive(&flags), "len = {len}");
        }
        // 伪随机批量（splitmix64 流）
        let mut z = 0x243F_6A88_85A3_08D3u64;
        for round in 0..8 {
            let len = 1024 * (round + 1);
            let mut flags = Vec::with_capacity(len);
            for _ in 0..len {
                z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                z ^= z >> 31;
                flags.push((z >> 63) as u32);
            }
            assert_eq!(
                chunked_prefix_compact(&flags),
                naive(&flags),
                "round {round} 伪随机 flag 序列"
            );
        }
    }

    /// 顶点 storage 词重排往返：解码回 (position, normal, uv) 与原顶点
    /// 位级一致（WGSL 读取下标契约的 CPU 锚点）。
    #[test]
    fn vertex_storage_words_roundtrip_bit_exact() {
        let vertices: Vec<Vertex> = (0..5).map(sample_vertex).collect();
        let words = vertex_storage_words(&vertices);
        assert_eq!(words.len(), vertices.len() * 2);
        for (i, v) in vertices.iter().enumerate() {
            let a = words[i * 2];
            let b = words[i * 2 + 1];
            assert_eq!(&a[0..3], &v.position[..], "position 逐位");
            assert_eq!(a[3].to_bits(), v.normal[0].to_bits());
            assert_eq!(b[0].to_bits(), v.normal[1].to_bits());
            assert_eq!(b[1].to_bits(), v.normal[2].to_bits());
            assert_eq!(&b[2..4], &v.uv[..], "uv 逐位");
        }
        assert!(vertex_storage_words(&[]).is_empty());
    }

    /// mesh_bounds：min/extent 解析解 + 空网格 None。
    #[test]
    fn mesh_bounds_returns_min_and_extent() {
        let vertices: Vec<Vertex> = (0..4)
            .map(|i| {
                let f = i as f32;
                Vertex {
                    position: [f - 1.0, -2.0 * f, 0.5 * f],
                    normal: [0.0, 1.0, 0.0],
                    uv: [0.0; 2],
                }
            })
            .collect();
        let (min, extent) = mesh_bounds(&vertices).expect("非空网格有包围盒");
        assert_eq!(min, [-1.0, -6.0, 0.0]);
        assert_eq!(extent, [3.0, 6.0, 1.5]);
        assert!(mesh_bounds(&[]).is_none(), "空网格无包围盒");
    }

    /// IRML 构建期簇上限拒绝：头部登记超限簇数（数据区尚未读）即拒。
    #[test]
    fn container_rejects_cluster_capacity_overflow() {
        let mesh = sample_mesh();
        let mut bytes = Vec::new();
        mesh.write_to(&mut bytes).expect("写出");
        let mut bloated = bytes.clone();
        bloated[16..20].copy_from_slice(&((MAX_MESHLET_CLUSTERS + 1) as u32).to_le_bytes());
        match MeshletMesh::from_bytes(&bloated) {
            Err(MeshletAssetError::Corrupt(msg)) => {
                assert!(msg.contains("超出单帧剔除容量上限"), "应报容量超限：{msg}");
            }
            other => panic!("超限簇数必须被拒绝，实际 {other:?}"),
        }
        // 恰在上限内不被此检查拒绝（该容器数据区不匹配，走后续长度校验）
        let mut at_cap = bytes;
        at_cap[16..20].copy_from_slice(&(MAX_MESHLET_CLUSTERS as u32).to_le_bytes());
        assert!(
            matches!(
                MeshletMesh::from_bytes(&at_cap),
                Err(MeshletAssetError::Corrupt(msg)) if msg.contains("数据区长度不符")
            ),
            "上限内的簇数应进入后续校验（长度不符）"
        );
    }

    // ---- R86：层级 LOD 选择（解析解 + 单调性 + 恒等表）----

    /// 手搓一棵最小 DAG：8 个叶组（各占叶簇 i）、2 个中层组（自有簇
    /// 8/9）与 1 个根（自有簇 10）。几何数据不参与选择，簇元数据填最小
    /// 占位；组球心按「bbox min = 0 / extent = 8」的归一化口径摆放（叶
    /// i 球心世界 x = i + 0.5；中层 A/B 球心 2.5 / 6.5；根 4.5）。
    fn lod_test_mesh() -> MeshletMesh {
        let mut mesh = MeshletMesh::default();
        // 11 个占位簇（每簇 1 三角 3 顶点，偏移连续——选择不消费但保持
        // 良构）
        for i in 0..11u32 {
            mesh.vertices.push(Vertex {
                position: [i as f32, 0.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [0.0; 2],
            });
            mesh.vertices.push(Vertex {
                position: [i as f32 + 1.0, 0.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [0.0; 2],
            });
            mesh.vertices.push(Vertex {
                position: [i as f32 + 0.5, 1.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [0.0; 2],
            });
            mesh.indices.extend_from_slice(&[0, 1, 2]);
            mesh.clusters.push(MeshletClusterRaw {
                vertex_offset: i * 3,
                index_offset: i * 3,
                vertex_count: 3,
                triangle_count: 1,
                bounds: [(i as f32 + 0.5) / 8.0, 0.5, 0.5, 0.2],
                error: 0.0,
                ..MeshletClusterRaw::default()
            });
        }
        let group = |children: Vec<u32>, start: u32, own: f32, group_err: f32, cx: f32, r: f32| {
            MeshletLodGroup {
                children,
                cluster_start: start,
                cluster_count: 1,
                own_error: own,
                group_error: group_err,
                // 球心摆在 x 轴上（y/z 归一化 0 = 世界 0）——dist 解析式
                // 不含杂项偏移
                bounds: [cx / 8.0, 0.0, 0.0, r],
                cone_axis: [0.0, 0.0, 1.0],
                cone_cutoff_cos: 1.0,
            }
        };
        // 组 0..7 = 叶组（簇 i）；组 8 = A（叶 0-3，簇 8）；组 9 = B（叶
        // 4-7，簇 9）；组 10 = 根（A/B，簇 10）。组误差沿 DAG 单调：叶 0
        // < 中层 1.0 < 根 2.0。
        mesh.hierarchy.groups = vec![
            group(Vec::new(), 0, 0.0, 0.0, 0.5, 0.2),
            group(Vec::new(), 1, 0.0, 0.0, 1.5, 0.2),
            group(Vec::new(), 2, 0.0, 0.0, 2.5, 0.2),
            group(Vec::new(), 3, 0.0, 0.0, 3.5, 0.2),
            group(Vec::new(), 4, 0.0, 0.0, 4.5, 0.2),
            group(Vec::new(), 5, 0.0, 0.0, 5.5, 0.2),
            group(Vec::new(), 6, 0.0, 0.0, 6.5, 0.2),
            group(Vec::new(), 7, 0.0, 0.0, 7.5, 0.2),
            group(vec![0, 1, 2, 3], 8, 1.0, 1.0, 2.5, 2.0),
            group(vec![4, 5, 6, 7], 9, 1.0, 1.0, 6.5, 2.0),
            group(vec![8, 9], 10, 2.0, 2.0, 4.5, 4.0),
        ];
        mesh.hierarchy.roots = vec![10];
        mesh.hierarchy.leaf_cluster_count = 8;
        mesh
    }

    /// 选择视口：眼点 (4.5, 0, dist)、H = 200 px、tan_half = 0.5、单位
    /// 缩放、bbox [0,8]×[0,1]×[0,1]。屏幕误差解析式（error_scale = 1）：
    /// err_px(g) = group_error × 100 / (dist × 0.5) = group_error × 200 / dist。
    fn lod_view(dist: f32) -> MeshletLodView {
        MeshletLodView {
            bounds_min: Vec3::new(0.0, 0.0, 0.0),
            bounds_extent: Vec3::new(8.0, 1.0, 1.0),
            eye_local: Vec3::new(4.5, 0.0, dist),
            error_scale: 1.0,
            screen_height: 200.0,
            tan_half_fov: 0.5,
        }
    }

    /// threshold = 0 恒细化到叶：输出 = 全叶列表 = R85 恒等表（升序）。
    #[test]
    fn lod_selection_threshold_zero_returns_all_leaves() {
        let mesh = lod_test_mesh();
        let selected = select_lod_clusters(&mesh, &lod_view(100.0), 0.0);
        assert_eq!(
            selected,
            (0..8).collect::<Vec<u32>>(),
            "threshold 0 必须下钻到全部叶簇（= R85 全叶列表）"
        );
        // 负阈值同语义（误差 ≥ 0 恒大于负阈值 ⇒ 恒下钻）
        let selected_neg = select_lod_clusters(&mesh, &lod_view(100.0), -1.0);
        assert_eq!(selected_neg, selected, "负阈值与 0 同为全叶");
    }

    /// threshold 极大：根的屏幕误差必过阈 ⇒ 只选根的自有簇。
    #[test]
    fn lod_selection_huge_threshold_selects_root_only() {
        let mesh = lod_test_mesh();
        let selected = select_lod_clusters(&mesh, &lod_view(100.0), 1.0e9);
        assert_eq!(selected, vec![10u32], "根误差必过阈 ⇒ 仅根自有簇");
    }

    /// 解析解（按 lod_view 的 err_px 公式逐档精确判定）：
    /// dist = 100 ⇒ 根 err_px = 2×200/100 = 4.0、中层 = 200/√(100²+2²) ≈ 2.0。
    /// threshold 4.0（恰等）：根选中（相等点判选中，f32 精确整除）；
    /// threshold 3.0：根下钻、A/B 选中 ⇒ 输出 = 自有簇 8/9（升序）；
    /// threshold 1.0：全下钻到叶 ⇒ 输出 = 全叶。
    #[test]
    fn lod_selection_analytic_thresholds_and_ascending_output() {
        let mesh = lod_test_mesh();
        // 恰等边界：4.0 ≤ 4.0 ⇒ 根选中（阈值语义 = ≤）
        assert_eq!(
            select_lod_clusters(&mesh, &lod_view(100.0), 4.0),
            vec![10u32],
            "屏幕误差恰等于阈值判选中（≤ 语义）"
        );
        // 中档：根下钻、A/B 双双选中（各自的自有簇 8 / 9，叶不再出现）
        let mid = select_lod_clusters(&mesh, &lod_view(100.0), 3.0);
        assert_eq!(
            mid,
            vec![8u32, 9],
            "A/B 选中 = 自有簇 8/9（叶组不下钻不出现）；输出必须升序"
        );
        // 低档：全下钻
        assert_eq!(
            select_lod_clusters(&mesh, &lod_view(100.0), 1.0),
            (0..8).collect::<Vec<u32>>(),
            "threshold 1.0 下中层（≈2.0）也不过阈 ⇒ 全叶"
        );
    }

    /// 距离单调：threshold 固定，眼点拉近 ⇒ 选择变细（根/中层不过阈），
    /// 拉远 ⇒ 变粗（中层/根选中）。解析锚点：dist = 100 时中层 ≈ 2.0
    /// （过阈 2.0）；dist = 50 时中层 = 200/√(50²+2²) ≈ 4.0（不过阈）。
    #[test]
    fn lod_selection_distance_monotonic() {
        let mesh = lod_test_mesh();
        let far = select_lod_cluster_ranges_in(&mesh.hierarchy, 11, &lod_view(100.0), 2.0);
        assert_eq!(
            far,
            vec![(8, 2)],
            "远距：双中层选中，两簇范围相邻 ⇒ 合并为单条 run"
        );
        let near = select_lod_cluster_ranges_in(&mesh.hierarchy, 11, &lod_view(50.0), 2.0);
        assert_eq!(near, vec![(0, 8)], "近距：全下钻到叶（单条合并 run）");
        assert!(
            far[0].1 < near[0].1,
            "远距选择的簇数不得多于近距（远 {} vs 近 {}）",
            far[0].1,
            near[0].1
        );
    }

    /// 区间升序规范化 + 非对称选择：眼点对齐 A（x = 2.5, z = 30）⇒
    /// errA = 100/15 ≈ 6.667（不过阈，下钻到叶 0-3）、errB =
    /// 100/(√916×0.5) ≈ 6.608（过阈，选中自有簇 9）。threshold 6.63 居中
    /// ⇒ 输出 = 两个**不相邻**区间 [(0,4), (9,1)]（簇 4..9 是空档），
    /// 展开后升序。
    #[test]
    fn lod_selection_asymmetric_ranges_sorted() {
        let mesh = lod_test_mesh();
        let mut view = lod_view(30.0);
        view.eye_local = Vec3::new(2.5, 0.0, 30.0);
        let ranges = select_lod_cluster_ranges_in(&mesh.hierarchy, 11, &view, 6.63);
        assert_eq!(
            ranges,
            vec![(0, 4), (9, 1)],
            "A 下钻到叶（区间 (0,4)）、B 选中自有簇（区间 (9,1)）——升序、不相邻"
        );
        let selected = select_lod_clusters(&mesh, &view, 6.63);
        assert_eq!(selected, vec![0u32, 1, 2, 3, 9], "展开形态升序");
    }

    /// 空层级（v1 容器 / 手搓单层网格）= 恒等表：`select_lod_clusters`
    /// 返回全部簇号升序（R85 全叶路径的 CPU 锚点）。
    #[test]
    fn lod_selection_empty_hierarchy_identity() {
        let mesh = sample_mesh(); // 2 簇、无层级
        let selected = select_lod_clusters(&mesh, &lod_view(100.0), 1.0e9);
        assert_eq!(
            selected,
            vec![0u32, 1],
            "空层级恒等表：阈值不影响输出（单层 = 全簇即叶）"
        );
        assert_eq!(mesh.leaf_count(), 2, "v1 网格叶数 = 簇总数");
    }

    // ---- R86：IRML v2 容器 ----

    /// v2 往返位级：层级网格 write（v2 布局：36 B 头）→ read 逐字段
    /// 相等（组 children / 范围 / 误差 / bounds / cone + 根 + 叶数，f32
    /// 位级比较）；单层网格 write 仍为 v1 布局（24 B 头）。
    #[test]
    fn container_v2_roundtrip_bit_exact_and_v1_layout_unchanged() {
        let mut mesh = lod_test_mesh();
        // 给误差 / bounds 填非平凡位型（负数 / 非整值——抓编解码错位）
        mesh.hierarchy.groups[8].own_error = -0.375;
        mesh.hierarchy.groups[8].group_error = 1.125;
        mesh.hierarchy.groups[10].bounds = [0.5125, 0.4, 0.6, 4.25];

        let mut bytes = Vec::new();
        let total = mesh.write_to(&mut bytes).expect("写出 v2");
        assert_eq!(total, bytes.len() as u64, "返回值 = 实际字节数");
        // v2 布局 = 头 36 + 顶点 33×32 + 索引 33×4 + 簇 11×48
        //   + 组（8 叶组 × 52 + 2 中层组 × 68 + 根 60 = 612）+ 根 1×4
        assert_eq!(bytes.len(), 36 + 1056 + 132 + 528 + 612 + 4, "v2 总长");
        assert_eq!(
            u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            2,
            "版本 = 2"
        );

        let back = MeshletMesh::from_bytes(&bytes).expect("读回 v2");
        assert_eq!(
            back.hierarchy.leaf_cluster_count, mesh.hierarchy.leaf_cluster_count,
            "叶簇数逐位"
        );
        assert_eq!(back.hierarchy.roots, mesh.hierarchy.roots, "根列表");
        assert_eq!(
            back.hierarchy.groups.len(),
            mesh.hierarchy.groups.len(),
            "组数"
        );
        for (a, b) in back
            .hierarchy
            .groups
            .iter()
            .zip(mesh.hierarchy.groups.iter())
        {
            assert_eq!(a.children, b.children, "children");
            assert_eq!(a.cluster_start, b.cluster_start, "簇起点");
            assert_eq!(a.cluster_count, b.cluster_count, "簇数");
            assert_eq!(
                a.own_error.to_bits(),
                b.own_error.to_bits(),
                "own_error 位级"
            );
            assert_eq!(
                a.group_error.to_bits(),
                b.group_error.to_bits(),
                "group_error 位级"
            );
            for (x, y) in a.bounds.iter().zip(b.bounds.iter()) {
                assert_eq!(x.to_bits(), y.to_bits(), "组 bounds 位级");
            }
            for (x, y) in a.cone_axis.iter().zip(b.cone_axis.iter()) {
                assert_eq!(x.to_bits(), y.to_bits(), "组锥轴位级");
            }
            assert_eq!(
                a.cone_cutoff_cos.to_bits(),
                b.cone_cutoff_cos.to_bits(),
                "组锥 cutoff 位级"
            );
        }
        assert_eq!(back.clusters, mesh.clusters, "簇元数据不受 v2 区段影响");

        // 单层网格仍走 v1 布局（R84 字节格式零漂移）
        let plain = sample_mesh();
        let mut v1_bytes = Vec::new();
        plain.write_to(&mut v1_bytes).expect("写出 v1");
        assert_eq!(
            u32::from_le_bytes(v1_bytes[4..8].try_into().unwrap()),
            1,
            "单层容器版本 = 1"
        );
        assert_eq!(v1_bytes.len(), 24 + 7 * 32 + 9 * 4 + 2 * 48, "v1 头长 24 B");
        let back_v1 = MeshletMesh::from_bytes(&v1_bytes).expect("读回 v1");
        assert!(
            back_v1.hierarchy.groups.is_empty(),
            "v1 容器层级为空（单层 = 全叶）"
        );
        assert_eq!(back_v1.leaf_count(), 2, "v1 叶数 = 簇总数");
    }

    /// v1 容器照常读取（R86 版本矩阵的兼容面）：R84 时代字节流 → 单层
    /// 网格（层级空、全簇即叶、展开三角形集合不变）。
    #[test]
    fn container_v1_file_still_reads_single_layer() {
        // R84 布局的字节流 = 单层网格 write_to 产物（版本字段 1）
        let mesh = sample_mesh();
        let mut bytes = Vec::new();
        mesh.write_to(&mut bytes).expect("写出");
        let back = MeshletMesh::from_bytes(&bytes).expect("v1 容器必须照常读取");
        assert!(back.hierarchy.groups.is_empty() && back.hierarchy.roots.is_empty());
        assert_eq!(back.clusters, mesh.clusters);
        assert_eq!(back.indices, mesh.indices);
        let expanded = back.expand();
        assert_eq!(expanded.indices.len(), 9, "展开不受层级字段影响");
    }

    /// v2 语义校验：子组号越界 / 组簇范围越界 / 根越界 / 截断，全部
    /// Corrupt 且可判别（良构 v2 数据区按偏移改写 + CRC 现算）。
    #[test]
    fn container_v2_rejects_semantically_invalid_hierarchy() {
        // 数据区布局：顶点（33×32）| 索引（33×4）| 簇区段 11×48 | 组区段
        // （8 叶组各 52、中层 2 组各 68、根 60）| 根区段 4。
        let common_section = 33 * 32 + 33 * 4;
        let cluster_section = 11 * 48;
        let leaf_groups = 8 * 52;
        let mid_groups = 2 * 68;
        let root_group_off = common_section + cluster_section + leaf_groups + mid_groups;
        let roots_off = root_group_off + 60;
        let craft = |patch: &dyn Fn(&mut [u8])| {
            let mesh = lod_test_mesh();
            let mut bytes = Vec::new();
            mesh.write_to(&mut bytes).expect("写出良构 v2");
            let mut data = bytes[36..].to_vec();
            patch(&mut data);
            let mut out = Vec::new();
            out.extend_from_slice(&MAGIC);
            out.extend_from_slice(&2u32.to_le_bytes());
            out.extend_from_slice(&(mesh.vertices.len() as u32).to_le_bytes());
            out.extend_from_slice(&(mesh.indices.len() as u32).to_le_bytes());
            out.extend_from_slice(&(mesh.clusters.len() as u32).to_le_bytes());
            out.extend_from_slice(&mesh.hierarchy.leaf_cluster_count.to_le_bytes());
            out.extend_from_slice(&(mesh.hierarchy.groups.len() as u32).to_le_bytes());
            out.extend_from_slice(&(mesh.hierarchy.roots.len() as u32).to_le_bytes());
            out.extend_from_slice(&crc32(&data).to_le_bytes());
            out.extend_from_slice(&data);
            out
        };
        // ① 子组号越界（根的 children 首项改成 10 ≥ 父序号 10）
        let bytes = craft(&mut |data| {
            let off = root_group_off + 4;
            data[off..off + 4].copy_from_slice(&10u32.to_le_bytes());
        });
        assert!(matches!(
            MeshletMesh::from_bytes(&bytes),
            Err(MeshletAssetError::Corrupt(msg)) if msg.contains("子组号")
        ));
        // ② 组簇范围越界（根 cluster_start = 100 > 簇总数 11）
        let bytes = craft(&mut |data| {
            let off = root_group_off + 4 + 8;
            data[off..off + 4].copy_from_slice(&100u32.to_le_bytes());
        });
        assert!(matches!(
            MeshletMesh::from_bytes(&bytes),
            Err(MeshletAssetError::Corrupt(msg)) if msg.contains("簇范围越界")
        ));
        // ③ 根越界（roots[0] = 99 ≥ 组总数 11）
        let bytes = craft(&mut |data| {
            data[roots_off..roots_off + 4].copy_from_slice(&99u32.to_le_bytes());
        });
        assert!(matches!(
            MeshletMesh::from_bytes(&bytes),
            Err(MeshletAssetError::Corrupt(msg)) if msg.contains("根 #0 组号")
        ));
        // ④ 区段长度闭合：头部谎报 root_count = 2（头不受 CRC 保护，
        // 头布局：魔数/版本/顶点/索引/簇/叶/组/根 = 0/4/8/12/16/20/24/28）
        // ⇒ 组 / 根区段消费长度与数据区不符。单纯截断会被整区段 CRC 先
        // 拦（ChecksumMismatch），闭合校验在此单独覆盖。
        let mut bytes = craft(&mut |_| {});
        bytes[28..32].copy_from_slice(&2u32.to_le_bytes());
        assert!(matches!(
            MeshletMesh::from_bytes(&bytes),
            Err(MeshletAssetError::Corrupt(msg)) if msg.contains("组/根区段消费")
        ));
    }

    // ---- R87：IRML v3 容器（页区段）----

    /// v3 往返位级：带页表的层级网格写出版本 3（40 B 头）→ 读回全部
    /// 字段（含页表 PartialEq）相等；单层网格带页表同样落 v3（叶数
    /// 归一为簇总数）；二次写出逐字节一致（幂等）。
    #[test]
    fn container_v3_roundtrip_bit_exact() {
        let mut mesh = lod_test_mesh();
        mesh.pages = Some(crate::meshlet_stream::MeshletPageTable::derive(&mesh, 3));
        let mut bytes = Vec::new();
        let total = mesh.write_to(&mut bytes).expect("写出 v3");
        assert_eq!(total, bytes.len() as u64);
        assert_eq!(
            u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            3,
            "带页表 ⇒ 版本 3"
        );
        // v3 布局 = 头 40 + 数据区 1716 + 组 612 + 根 4 + 页区段
        //   （页数 4 + 4 页 × 16 B + 逐簇组号 11 × 4 B = 112）
        assert_eq!(
            bytes.len(),
            40 + 1056 + 132 + 528 + 612 + 4 + 112,
            "v3 总长"
        );
        assert_eq!(
            u32::from_le_bytes(bytes[32..36].try_into().unwrap()),
            3,
            "头内每页簇数 = 3"
        );

        let back = MeshletMesh::from_bytes(&bytes).expect("读回 v3");
        assert_eq!(back.clusters, mesh.clusters);
        assert_eq!(back.hierarchy.roots, mesh.hierarchy.roots);
        assert_eq!(
            back.hierarchy.leaf_cluster_count, mesh.hierarchy.leaf_cluster_count,
            "层级非空 ⇒ 叶数字段原样"
        );
        assert_eq!(back.pages, mesh.pages, "页表逐字段相等（PartialEq 位级）");
        assert_eq!(back.leaf_count(), mesh.leaf_count());

        // 二次写出幂等（读回的叶数已归一，再写应得同一字节流）
        let mut bytes2 = Vec::new();
        back.write_to(&mut bytes2).expect("二次写出");
        assert_eq!(bytes, bytes2, "v3 往返后写出必须幂等");

        // 单层网格 + 页表 ⇒ 同样 v3，叶数字段归一为簇总数
        let mut plain = sample_mesh();
        plain.pages = Some(crate::meshlet_stream::MeshletPageTable::derive(&plain, 16));
        let mut pb = Vec::new();
        plain.write_to(&mut pb).expect("单层 v3");
        assert_eq!(u32::from_le_bytes(pb[4..8].try_into().unwrap()), 3);
        assert_eq!(
            u32::from_le_bytes(pb[20..24].try_into().unwrap()),
            2,
            "单层叶数归一 = 簇总数"
        );
        let pback = MeshletMesh::from_bytes(&pb).expect("读回单层 v3");
        assert!(pback.hierarchy.groups.is_empty(), "层级仍为空（单层语义）");
        assert_eq!(pback.leaf_count(), 2, "空层级 leaf_count 回落簇总数");
        assert_eq!(pback.pages, plain.pages);
    }

    /// v3 页区段语义校验：页数与划分不符 / 页区间与簇元数据不符 /
    /// 逐簇组号越界 / 组号与组范围不符，全部 Corrupt 且可判别
    ///（良构 v3 数据区按偏移改写 + CRC 现算）。
    #[test]
    fn container_v3_rejects_invalid_page_section() {
        // 页区段在数据区内的偏移：顶点+索引+簇 1716 | 组 612 | 根 4。
        let pages_off = 1716 + 612 + 4;
        let craft = |patch: &dyn Fn(&mut [u8])| {
            let mut mesh = lod_test_mesh();
            mesh.pages = Some(crate::meshlet_stream::MeshletPageTable::derive(&mesh, 3));
            let mut bytes = Vec::new();
            mesh.write_to(&mut bytes).expect("写出良构 v3");
            let mut data = bytes[40..].to_vec();
            patch(&mut data);
            let mut out = Vec::new();
            out.extend_from_slice(&MAGIC);
            out.extend_from_slice(&3u32.to_le_bytes());
            out.extend_from_slice(&(mesh.vertices.len() as u32).to_le_bytes());
            out.extend_from_slice(&(mesh.indices.len() as u32).to_le_bytes());
            out.extend_from_slice(&(mesh.clusters.len() as u32).to_le_bytes());
            out.extend_from_slice(&mesh.hierarchy.leaf_cluster_count.to_le_bytes());
            out.extend_from_slice(&(mesh.hierarchy.groups.len() as u32).to_le_bytes());
            out.extend_from_slice(&(mesh.hierarchy.roots.len() as u32).to_le_bytes());
            out.extend_from_slice(&3u32.to_le_bytes()); // 每页簇数
            out.extend_from_slice(&crc32(&data).to_le_bytes());
            out.extend_from_slice(&data);
            out
        };
        // ① 页数与划分不符（4 页改为 5 页）
        let bytes = craft(&mut |data| {
            data[pages_off..pages_off + 4].copy_from_slice(&5u32.to_le_bytes());
        });
        assert!(matches!(
            MeshletMesh::from_bytes(&bytes),
            Err(MeshletAssetError::Corrupt(msg)) if msg.contains("页数")
        ));
        // ② 首页顶点区间与簇元数据不符（vertex_start 改 999）
        let bytes = craft(&mut |data| {
            data[pages_off + 4..pages_off + 8].copy_from_slice(&999u32.to_le_bytes());
        });
        assert!(matches!(
            MeshletMesh::from_bytes(&bytes),
            Err(MeshletAssetError::Corrupt(msg)) if msg.contains("顶点区间与簇元数据不符")
        ));
        // ③ 逐簇组号越界（簇 0 组号 99 ≥ 组总数 11）
        let group_ids_off = pages_off + 4 + 4 * 16;
        let bytes = craft(&mut |data| {
            data[group_ids_off..group_ids_off + 4].copy_from_slice(&99u32.to_le_bytes());
        });
        assert!(matches!(
            MeshletMesh::from_bytes(&bytes),
            Err(MeshletAssetError::Corrupt(msg)) if msg.contains("组号 99 越界")
        ));
        // ④ 组号与组自有簇范围不符（簇 0 指到根组 10——范围 [10, 11)）
        let bytes = craft(&mut |data| {
            data[group_ids_off..group_ids_off + 4].copy_from_slice(&10u32.to_le_bytes());
        });
        assert!(matches!(
            MeshletMesh::from_bytes(&bytes),
            Err(MeshletAssetError::Corrupt(msg)) if msg.contains("组号 10 与组自有簇范围")
        ));
    }
}
