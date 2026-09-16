//! 虚拟几何体 V1（R84）：meshlet 簇的运行期表示 + 版本化资产容器（IRML）。
//!
//! meshlet（簇）= 网格三角形的近似凸连通小块（≤ [`MESHLET_CLUSTER_MAX_TRIANGLES`]
//! 个三角、≤ [`MESHLET_CLUSTER_MAX_VERTICES`] 个簇内顶点），是本仓
//! 「Nanite 式子集」路线（R84–R87）的基本处理单元。本模块提供三件事：
//!
//! 1. **运行期表示**：[`MeshletMesh`]（全局顶点 / 全局索引 / 逐簇元数据）
//!    与 [`MeshletClusterRaw`]（48 B POD，布局为 R85 GPU storage 的前置）；
//! 2. **展开**：[`MeshletMesh::expand`] 纯函数——把簇表示还原为既有渲染
//!    路径可消费的 [`MeshData`](crate::mesh::MeshData)。V1 的正确性锚点：
//!    顶点重排不改像素（覆盖集与逐角插值不变），同一几何「常规路径 vs
//!    meshlet 构建后展开路径」渲染逐字节一致（GPU 佐证见
//!    `tests/meshlet_render.rs`）；
//! 3. **资产容器（IRML）**：版本化 + CRC32 的单文件格式，`write_to` /
//!    `write_file` / `from_bytes` / `open` 与 `iris-engine::pack` 同构。
//!
//! # 全案 MVP 边界（R84–R87 红线修订声明，越界即 bug）
//!
//! - **无 visbuffer**：不经软件光栅、不记逐像素三角 id——V1 经既有常规
//!   网格路径渲染（compute 剔除是 R85 的事）；
//! - **opaque-only**：透明 / 蒙皮 / 带切线 / 带光照贴图 UV 的网格不适用
//!   （容器只载 32 B 标准顶点）；
//! - **无软件光栅回退**；
//! - **单 LOD**：簇的 `error` 字段恒 0.0（量化编码就位，R86 简化器启用），
//!   无簇 DAG / 无层级；
//! - **非流式加载**：整文件进内存，分页流送是 R87 的事（届时再做 pack
//!   集成）。
//!
//! # 簇元数据口径（R85 GPU 剔除 / R87 分页的前置契约）
//!
//! - `vertex_offset` / `index_offset`：指向 [`MeshletMesh`] 全局缓冲的
//!   绝对偏移（簇数据在全局缓冲中**按簇连续、按构建序排布**——读侧
//!   校验连续性，见 [`MeshletAssetError::Corrupt`]）；
//! - `cone_axis` / `cone_cutoff_cos`：背面簇剔除边界锥。axis 为簇内三角
//!   面法线均值的归一化向量；`cone_cutoff_cos = min(axis·n_i)`（截断到
//!   [-1, 1]）——视线方向与 axis 点积小于 cutoff 时簇整体背面。退化防护：
//!   簇内无有效面法线（全退化三角）或均值长度 < 1e-6（法线完全对消，
//!   如立方体六向簇）⇒ 锥退化为全向（axis = [0,0,1]、cutoff = −1，
//!   **永不剔除**，保守方向）；
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
//! 压缩随 R87 流送评估）、簇跨簇顶点不去重（meshlet 标准形态：簇间
//! 边界顶点各簇各存一份，总量换缓存局部性）、CRC32 只防损坏不防篡改
//! （与 pack 同一立场）。

use std::fmt;
use std::fs::File;
use std::io::Write;
use std::path::Path;

use glam::Vec3;

use crate::mesh::{MeshData, Vertex};

/// 单簇三角形数上限。
pub const MESHLET_CLUSTER_MAX_TRIANGLES: usize = 128;

/// 单簇顶点数上限（簇内局部重映射后）。
pub const MESHLET_CLUSTER_MAX_VERTICES: usize = 256;

/// 背面锥退化时的兜底轴（全向锥——永不背面剔除，保守方向）。
pub const MESHLET_CONE_FALLBACK_AXIS: [f32; 3] = [0.0, 0.0, 1.0];

/// 背面锥退化时的兜底 cutoff（−1 = 整个方向半球都算正面）。
pub const MESHLET_CONE_FALLBACK_CUTOFF_COS: f32 = -1.0;

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

/// meshlet 网格：全局顶点 + 全局索引 + 逐簇元数据。
///
/// - 顶点与既有网格同格式（[`Vertex`]，32 B：position / normal / uv），
///   材质与光照路径零改动复用；**簇间边界顶点不去重**（各簇各存一份，
///   索引为簇内局部号）；
/// - 簇数据在两个全局缓冲里都按簇连续、按簇构建序排布
///   （`vertex_offset` / `index_offset` 严格递增衔接）——容器读侧校验
///   此不变量。
#[derive(Clone, Debug, Default)]
pub struct MeshletMesh {
    /// 全局顶点缓冲（簇拼接序；同位置顶点可能跨簇重复）。
    pub vertices: Vec<Vertex>,
    /// 全局索引缓冲（簇拼接序；值 = 簇内局部顶点号 0..vertex_count）。
    pub indices: Vec<u32>,
    /// 逐簇元数据（按簇构建序）。
    pub clusters: Vec<MeshletClusterRaw>,
}

impl MeshletMesh {
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
        }
    }

    /// 写出为 IRML 字节流（含头 + CRC），返回总字节数。
    pub fn write_to(&self, mut writer: impl Write) -> std::io::Result<u64> {
        let data = self.encode_data_region();
        writer.write_all(&MAGIC)?;
        writer.write_all(&VERSION.to_le_bytes())?;
        writer.write_all(&(self.vertices.len() as u32).to_le_bytes())?;
        writer.write_all(&(self.indices.len() as u32).to_le_bytes())?;
        writer.write_all(&(self.clusters.len() as u32).to_le_bytes())?;
        writer.write_all(&crc32(&data).to_le_bytes())?;
        writer.write_all(&data)?;
        Ok(HEADER_LEN as u64 + data.len() as u64)
    }

    /// 便捷：写出为磁盘文件，返回总字节数。
    pub fn write_file(&self, path: impl AsRef<Path>) -> std::io::Result<u64> {
        self.write_to(File::create(path)?)
    }

    /// 从内存字节解析（校验魔数 / 版本 / 长度 / CRC / 索引与簇范围）。
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
        if version != VERSION {
            return Err(MeshletAssetError::UnsupportedVersion(version));
        }
        let vertex_count =
            u32::from_le_bytes(bytes[8..12].try_into().expect("顶点数字段 4 字节")) as usize;
        let index_count =
            u32::from_le_bytes(bytes[12..16].try_into().expect("索引数字段 4 字节")) as usize;
        let cluster_count =
            u32::from_le_bytes(bytes[16..20].try_into().expect("簇数字段 4 字节")) as usize;
        let expected_crc = u32::from_le_bytes(bytes[20..24].try_into().expect("CRC 字段 4 字节"));

        let data = &bytes[HEADER_LEN..];
        let expect_len = vertex_count * std::mem::size_of::<Vertex>()
            + index_count * std::mem::size_of::<u32>()
            + cluster_count * std::mem::size_of::<MeshletClusterRaw>();
        if data.len() != expect_len {
            return Err(MeshletAssetError::Corrupt(format!(
                "数据区长度不符：期望 {expect_len} B（顶点 {vertex_count} × 32 + 索引 \
                 {index_count} × 4 + 簇 {cluster_count} × 48），实际 {} B",
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

        Ok(Self {
            vertices,
            indices,
            clusters,
        })
    }

    /// 打开磁盘上的 IRML 文件（整文件读入——v1 非流式，见模块头）。
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
// 簇元数据纯函数（构建器与未来 R85/R87 共用；无状态、逐位确定）
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
// IRML 容器：魔数 / 版本 / CRC32（与 pack.rs 同一 IEEE 802.3 定义）
// ---------------------------------------------------------------------------

/// 文件头魔数（"IRML" = IRis MeshLet）。
const MAGIC: [u8; 4] = *b"IRML";

/// 当前格式版本。reader 拒读一切非当前版本（v1 无历史版本可向后兼容；
/// 将来升版时在此加逐版本分支，旧 reader 拒读而非误读）。
const VERSION: u32 = 1;

/// 文件头固定长度：魔数 4 + 版本 4 + 顶点数 4 + 索引数 4 + 簇数 4 + CRC 4。
const HEADER_LEN: usize = 24;

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
                write!(f, "IRML 版本 {v} 不受支持（本实现支持版本 {VERSION}）")
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

        for v in [0u32, 2, 999] {
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
            bytes.extend_from_slice(&MAGIC);
            bytes.extend_from_slice(&VERSION.to_le_bytes());
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
}
