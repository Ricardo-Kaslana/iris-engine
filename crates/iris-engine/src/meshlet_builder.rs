//! meshlet 构建器（R84，虚拟几何体 V1）：既有网格 → [`MeshletMesh`] 的
//! 确定性贪心聚类（纯 CPU，构建期一次性支出，渲染路径零参与）。
//!
//! # 算法（确定性贪心聚类）
//!
//! 三角邻接图上的种子生长，与 meshoptimizer / Nanite 公开口径同族，
//! 全程无随机性、无哈希迭代序依赖：
//!
//! 1. **邻接表**：顶点 → 引用它的三角形列表（按三角形索引升序构建；
//!    共享边与共享点统一为「共享顶点」邻接——贪心聚类的目标是顶点
//!    重用最大化，共享点次优于共享边，统一处理让候选集天然覆盖两者）；
//! 2. **种子**：未分配三角形中**索引序最小**者（输入即索引序——乱序
//!    输入不适用本保证，但同输入两跑输出逐位一致）；
//! 3. **生长**：维护候选集（`BTreeSet`——迭代序恒为三角索引升序，确定性
//!    硬文化的选型理由），每步对全部候选按**固定字典序全序**评分：
//!    ①与当前簇已用顶点的共享数**降序**（顶点重用最大化 → 簇内连通且
//!    顶点数最省）；②三角索引**升序**收尾（`BTreeSet` 升序扫描 + 严格
//!    大于比较 = 同分取索引最小者，total_cmp 级全序、无并列歧义）；
//! 4. **封簇**：三角满 [`MESHLET_CLUSTER_MAX_TRIANGLES`]（128）或候选集
//!    无可生长者（都未连通 / 已分配 / 会顶破
//!    [`MESHLET_CLUSTER_MAX_VERTICES`] 顶点上限）即封簇，取下一种子。
//!
//! 簇内顶点做**局部重映射**（索引序即发现序——簇内按三角形加入序、
//! 三角形内按角槽序首次引用编号），索引改写为簇内局部号；全局顶点 /
//! 索引缓冲按簇拼接序排布（`vertex_offset` / `index_offset` 指向全局
//! 缓冲，读侧校验连续性）。簇间边界顶点不去重（各簇各存一份）——
//! meshlet 标准形态，顶点总量换逐簇自包含。
//!
//! # 逐簇元数据
//!
//! 全部走 `iris_render::meshlet` 的无状态纯函数（口径详见该模块文档）：
//! 包围球 = 均值球心 + 最大半径（选型：单遍、必然包含、逐位确定；
//! Ritter 双遍近似并无紧度优势）；边界锥 = 面法线均值归一 +
//! `cutoff = min(axis·n_i)`（退化三角按叉积长度 < 1e-12 预过滤，全退化
//! / 法线完全对消的簇得全向锥 = 永不背面剔除，保守方向）；包围球球心
//! 相对**输入顶点的包围盒**归一（extent 为 0 的退化轴除数取 1.0）；
//! `error` 恒 0.0（V1 无简化器，R86 启用）。
//!
//! # 输入约束
//!
//! 输入即索引序：同一网格的三角形排列顺序变化会得到不同（但各自确定）
//! 的聚类——GLB 加载产物天然满足。索引必须 3 的倍数且全部在顶点范围内
//! （断言拦截）。[`is_meshlet_eligible`] 判定 V1 支持的网格形态：
//! 无蒙皮 / 切线 / 光照贴图 UV（容器只载 32 B 标准顶点，opaque-only
//! 边界见 `iris_render::meshlet` 模块头）。
//!
//! # 确定性声明
//!
//! 同输入两跑输出逐位一致（`to_bits` 断言）：无随机源、迭代序全部来自
//! `BTreeSet` 升序 / Vec 构建序 / 三角索引序，浮点仅加减乘与 `sqrt`
//! （glam 内部即 sqrt，无 `hypot`），求和序 = 发现序固定。

use std::collections::{BTreeSet, HashMap};

use glam::Vec3;
use iris_render::mesh::{MeshData, Vertex};
use iris_render::meshlet::{
    MESHLET_CLUSTER_MAX_TRIANGLES, MeshletClusterRaw, MeshletMesh, cluster_bounding_sphere,
    cluster_bounds_normalized, cluster_cone,
};

/// V1 支持的网格形态：无蒙皮 / 切线 / 光照贴图 UV（容器只载标准 32 B
/// 顶点）。不满足时用 [`build_meshlets_from_mesh_data`] 会断言拦截。
pub fn is_meshlet_eligible(data: &MeshData) -> bool {
    data.skin.is_none() && data.tangents.is_none() && data.uv2.is_none()
}

/// 便捷入口：[`MeshData`] → [`MeshletMesh`]（要求
/// [`is_meshlet_eligible`]，V1 只载标准顶点）。
pub fn build_meshlets_from_mesh_data(data: &MeshData) -> MeshletMesh {
    assert!(
        is_meshlet_eligible(data),
        "meshlet V1 只支持标准网格（无蒙皮 / 切线 / 光照贴图 UV）——\
         蒙皮与带切线网格的簇化留待后续轮次"
    );
    build_meshlets(&data.vertices, &data.indices)
}

/// 确定性贪心聚类：顶点 + 索引 → [`MeshletMesh`]。
///
/// 算法与元数据口径见模块文档。空输入（0 三角）合法：产出全空
/// [`MeshletMesh`]。
///
/// **顶点上限的诚实口径**：预算常量 [`MESHLET_CLUSTER_MAX_VERTICES`]
/// = 256，但簇元数据的 `vertex_count` 字段是 u8（R85 storage 布局
/// 承诺），物理容量 255——构建器按 **255** 卡（`u8::MAX`），否则
/// `as u8` 会回绕成 0。两处口径差异已写入两边的文档。
pub fn build_meshlets(vertices: &[Vertex], indices: &[u32]) -> MeshletMesh {
    assert!(
        indices.len() % 3 == 0,
        "索引数必须是 3 的倍数（截断的三角形无法聚类）：{}",
        indices.len()
    );
    for (slot, &idx) in indices.iter().enumerate() {
        assert!(
            (idx as usize) < vertices.len(),
            "索引 #{slot} 越界：{idx} ≥ 顶点数 {}",
            vertices.len()
        );
    }
    let tri_count = indices.len() / 3;
    let mut mesh = MeshletMesh::default();
    if tri_count == 0 {
        return mesh;
    }

    // 输入顶点包围盒（簇包围球归一化的基）。
    let (bbox_min, bbox_extent) = mesh_bounds(vertices);

    // 顶点 → 三角形邻接表：Vec 下标 = 顶点号，内层列表按三角形升序
    // push（构建序即确定序，无哈希迭代）。
    let mut vertex_tris: Vec<Vec<u32>> = vec![Vec::new(); vertices.len()];
    for tri in 0..tri_count as u32 {
        for corner in 0..3 {
            vertex_tris[indices[tri as usize * 3 + corner] as usize].push(tri);
        }
    }

    let mut assigned = vec![false; tri_count];
    let mut next_vertex_offset = 0u32;
    let mut next_index_offset = 0u32;

    while let Some(seed) = (0..tri_count as u32).find(|&tri| !assigned[tri as usize]) {
        // 种子：未分配三角中索引序最小者（线性扫表——assigned 是紧凑
        // Vec，扫描本身即确定序）。

        // 簇内状态。局部重映射 HashMap 只做查（迭代序从未消费——发现序
        // 单独记在 Vec 里）；候选集 BTreeSet 升序迭代（确定性选型见模块头）。
        let mut local_remap: HashMap<u32, u8> = HashMap::with_capacity(64);
        let mut discovery: Vec<u32> = Vec::with_capacity(64); // 发现序全局顶点号
        let mut cluster_tris: Vec<u32> = Vec::with_capacity(MESHLET_CLUSTER_MAX_TRIANGLES);
        let mut candidates: BTreeSet<u32> = BTreeSet::new();

        add_triangle(
            seed,
            indices,
            &vertex_tris,
            &mut assigned,
            &mut local_remap,
            &mut discovery,
            &mut cluster_tris,
            &mut candidates,
            &mut mesh.indices,
        );

        loop {
            if cluster_tris.len() >= MESHLET_CLUSTER_MAX_TRIANGLES {
                break; // 满簇封口
            }
            // 全序评分：共享已用顶点数降序；同分取索引最小（BTreeSet 升序
            // 扫描 + 严格大于 = 首个同分者胜，全序无并列歧义）。
            let mut best: Option<(u32, usize)> = None;
            for &cand in &candidates {
                if assigned[cand as usize] {
                    continue;
                }
                let base = cand as usize * 3;
                let mut shared = 0usize;
                let mut fresh = 0usize;
                for corner in 0..3 {
                    if local_remap.contains_key(&indices[base + corner]) {
                        shared += 1;
                    } else {
                        fresh += 1;
                    }
                }
                // 顶点容量：新顶点不得顶破 u8 物理上限 255（口径见函数
                // 文档）；装不下的候选跳过——其他候选仍可生长，真无可
                // 生长才封簇。
                if local_remap.len() + fresh > MESHLET_CLUSTER_U8_VERTEX_CAP {
                    continue;
                }
                if best.is_none_or(|(_, best_shared)| shared > best_shared) {
                    best = Some((cand, shared));
                }
            }
            let Some((chosen, _)) = best else {
                break; // 无可生长候选：封簇
            };
            add_triangle(
                chosen,
                indices,
                &vertex_tris,
                &mut assigned,
                &mut local_remap,
                &mut discovery,
                &mut cluster_tris,
                &mut candidates,
                &mut mesh.indices,
            );
        }

        // ---- 簇元数据（纯函数口径见模块文档）----
        debug_assert!(cluster_tris.len() <= MESHLET_CLUSTER_MAX_TRIANGLES);
        debug_assert!(discovery.len() <= MESHLET_CLUSTER_U8_VERTEX_CAP);
        let positions: Vec<Vec3> = discovery
            .iter()
            .map(|&g| Vec3::from(vertices[g as usize].position))
            .collect();
        let (center, radius) = cluster_bounding_sphere(&positions);
        let mut face_normals: Vec<Vec3> = Vec::with_capacity(cluster_tris.len());
        for &tri in &cluster_tris {
            let base = tri as usize * 3;
            let a = Vec3::from(vertices[indices[base] as usize].position);
            let b = Vec3::from(vertices[indices[base + 1] as usize].position);
            let c = Vec3::from(vertices[indices[base + 2] as usize].position);
            let cross = (b - a).cross(c - a);
            if cross.length() >= 1e-12 {
                face_normals.push(cross.normalize());
            }
            // 零面积退化三角：无面法线，锥统计预过滤（模块文档口径）。
        }
        let (cone_axis, cone_cutoff_cos) = cluster_cone(&face_normals);
        let cluster = MeshletClusterRaw {
            vertex_offset: next_vertex_offset,
            index_offset: next_index_offset,
            vertex_count: discovery.len() as u8,
            triangle_count: cluster_tris.len() as u8,
            reserved: [0; 2],
            cone_axis,
            cone_cutoff_cos,
            bounds: cluster_bounds_normalized(center, radius, bbox_min, bbox_extent),
            error: 0.0, // V1 无简化器，恒 0（R86 启用）
        };
        // 簇顶点按发现序追加到全局缓冲（跨簇重复不去重）。
        for &g in &discovery {
            mesh.vertices.push(vertices[g as usize]);
        }
        next_vertex_offset += cluster.vertex_count as u32;
        next_index_offset += cluster.triangle_count as u32 * 3;
        mesh.clusters.push(cluster);
    }

    mesh
}

/// u8 字段的物理顶点上限（255）：预算常量 256 被 u8 容量收窄——
/// `as u8` 回绕防线，构建器与读侧的实际闸门。
const MESHLET_CLUSTER_U8_VERTEX_CAP: usize = u8::MAX as usize;

/// 把一个三角形种进当前簇：标记分配、局部重映射（发现序编号）、局部
/// 索引追加、邻接三角入候选集。所有容器均为确定序（Vec push 序 /
/// BTreeSet 升序），HashMap 只查不迭代。
#[allow(clippy::too_many_arguments)]
fn add_triangle(
    tri: u32,
    indices: &[u32],
    vertex_tris: &[Vec<u32>],
    assigned: &mut [bool],
    local_remap: &mut HashMap<u32, u8>,
    discovery: &mut Vec<u32>,
    cluster_tris: &mut Vec<u32>,
    candidates: &mut BTreeSet<u32>,
    mesh_indices: &mut Vec<u32>,
) {
    assigned[tri as usize] = true;
    cluster_tris.push(tri);
    let base = tri as usize * 3;
    for corner in 0..3 {
        let global = indices[base + corner];
        let local = match local_remap.get(&global) {
            Some(&local) => local,
            None => {
                let local = local_remap.len() as u8;
                local_remap.insert(global, local);
                discovery.push(global);
                local
            }
        };
        mesh_indices.push(local as u32);
    }
    for corner in 0..3 {
        for &adj in &vertex_tris[indices[base + corner] as usize] {
            candidates.insert(adj);
        }
    }
}

/// 输入顶点的包围盒（min, extent）。空输入不会到达（调用方已拦 0 三角）。
fn mesh_bounds(vertices: &[Vertex]) -> (Vec3, Vec3) {
    let first = Vec3::from(vertices[0].position);
    let mut min = first;
    let mut max = first;
    for v in vertices {
        let p = Vec3::from(v.position);
        min = min.min(p);
        max = max.max(p);
    }
    (min, max - min)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use iris_render::meshlet::MESHLET_CLUSTER_MAX_VERTICES;

    /// 展开后的三角形集合（属性三元组位型串行化，排序后比较）——聚类与
    /// 重映射不改三角形集合（含环绕方向）。位型比较（to_bits）比浮点
    /// 相等更严：连 −0.0 / NaN 载荷位都算差异。
    fn sorted_triangle_keys(data: &MeshData) -> Vec<Vec<u32>> {
        let vertex_key = |v: &Vertex| {
            let mut key = Vec::with_capacity(8);
            key.extend(
                v.position
                    .iter()
                    .chain(v.normal.iter())
                    .chain(v.uv.iter())
                    .map(|f| f.to_bits()),
            );
            key
        };
        let mut keys: Vec<Vec<u32>> = data
            .indices
            .chunks_exact(3)
            .map(|tri| {
                let mut key = Vec::with_capacity(24);
                for &i in tri {
                    key.extend(vertex_key(&data.vertices[i as usize]));
                }
                key
            })
            .collect();
        keys.sort();
        keys
    }

    /// 同输入两跑输出逐位一致（确定性硬账：to_bits 全字段）。
    #[test]
    fn determinism_same_input_bit_identical() {
        let sphere = MeshData::uv_sphere(1.0, 16, 12);
        let a = build_meshlets(&sphere.vertices, &sphere.indices);
        let b = build_meshlets(&sphere.vertices, &sphere.indices);
        assert_eq!(a.clusters.len(), b.clusters.len());
        assert_eq!(a.vertices.len(), b.vertices.len());
        assert_eq!(a.indices.len(), b.indices.len());
        for (x, y) in a.vertices.iter().zip(b.vertices.iter()) {
            for (p, q) in x.position.iter().zip(y.position) {
                assert_eq!(p.to_bits(), q.to_bits());
            }
            for (p, q) in x.normal.iter().zip(y.normal) {
                assert_eq!(p.to_bits(), q.to_bits());
            }
            for (p, q) in x.uv.iter().zip(y.uv) {
                assert_eq!(p.to_bits(), q.to_bits());
            }
        }
        assert_eq!(a.indices, b.indices, "索引逐项一致");
        assert_eq!(a.clusters, b.clusters, "簇元数据逐字段一致");
    }

    /// 三角覆盖恰一次：簇三角数合计 = 输入三角数，索引长度一致，且展开
    /// 三角形集合（属性排序比较）与原网格相同（可判别——若重映射错位，
    /// 属性三元组必然对不上）。
    #[test]
    fn triangle_coverage_exactly_once_with_same_set() {
        let sphere = MeshData::uv_sphere(1.0, 24, 16);
        let mesh = build_meshlets(&sphere.vertices, &sphere.indices);
        let total_tris = sphere.indices.len() / 3;
        let clustered: usize = mesh
            .clusters
            .iter()
            .map(|c| c.triangle_count as usize)
            .sum();
        assert_eq!(clustered, total_tris, "簇三角数合计必须等于输入三角数");
        assert_eq!(mesh.indices.len(), sphere.indices.len());
        let expanded = mesh.expand();
        assert_eq!(
            sorted_triangle_keys(&expanded),
            sorted_triangle_keys(&sphere),
            "展开三角形集合（含环绕）必须与原网格逐三角相同"
        );
    }

    /// 簇上限与连续性：大网格多簇，逐簇三角 ≤ 128 / 顶点 ≤ 256，
    /// `vertex_offset` / `index_offset` 严丝合缝衔接全局缓冲。
    #[test]
    fn cluster_limits_and_contiguous_offsets() {
        // 2976 三角 ⇒ 必然多簇
        let sphere = MeshData::uv_sphere(1.0, 48, 32);
        let mesh = build_meshlets(&sphere.vertices, &sphere.indices);
        assert!(
            mesh.clusters.len() > 1,
            "2976 三角必须产生多簇（实际 {}）",
            mesh.clusters.len()
        );
        let mut expect_vertex_offset = 0u32;
        let mut expect_index_offset = 0u32;
        for (i, c) in mesh.clusters.iter().enumerate() {
            assert!(
                (c.triangle_count as usize) <= MESHLET_CLUSTER_MAX_TRIANGLES,
                "簇 #{i} 三角数 {} 超上限",
                c.triangle_count
            );
            assert!(
                (c.vertex_count as usize) <= MESHLET_CLUSTER_MAX_VERTICES,
                "簇 #{i} 顶点数 {} 超上限",
                c.vertex_count
            );
            assert!(
                (c.triangle_count as usize) >= 1,
                "空簇不可能出现（种子先行）"
            );
            assert_eq!(
                c.vertex_offset, expect_vertex_offset,
                "簇 #{i} 顶点偏移不连续"
            );
            assert_eq!(
                c.index_offset, expect_index_offset,
                "簇 #{i} 索引偏移不连续"
            );
            // 簇内索引都是局部号
            let start = c.index_offset as usize;
            let end = start + c.triangle_count as usize * 3;
            assert!(
                mesh.indices[start..end]
                    .iter()
                    .all(|&l| (l as usize) < c.vertex_count as usize),
                "簇 #{i} 出现越界局部索引"
            );
            expect_vertex_offset += c.vertex_count as u32;
            expect_index_offset += c.triangle_count as u32 * 3;
        }
        assert_eq!(mesh.vertices.len(), expect_vertex_offset as usize);
        assert_eq!(mesh.indices.len(), expect_index_offset as usize);
    }

    /// 簇内连通性：每簇三角形经共享顶点可达（贪心生长的产物自证——
    /// 若生长逻辑漏接邻接表，簇会裂成多个连通分量）。
    #[test]
    fn clusters_are_vertex_connected() {
        let sphere = MeshData::uv_sphere(1.0, 48, 32);
        let mesh = build_meshlets(&sphere.vertices, &sphere.indices);
        for (ci, c) in mesh.clusters.iter().enumerate() {
            let index_range =
                c.index_offset as usize..c.index_offset as usize + c.triangle_count as usize * 3;
            let tris: Vec<[u8; 3]> = mesh.indices[index_range]
                .chunks_exact(3)
                .map(|t| [t[0] as u8, t[1] as u8, t[2] as u8])
                .collect();
            // BFS：从首三角出发，经共享顶点遍历
            let mut seen = vec![false; tris.len()];
            seen[0] = true;
            let mut queue = vec![0usize];
            while let Some(ti) = queue.pop() {
                for (uj, other) in tris.iter().enumerate() {
                    if seen[uj] {
                        continue;
                    }
                    if other.iter().any(|v| tris[ti].contains(v)) {
                        seen[uj] = true;
                        queue.push(uj);
                    }
                }
            }
            assert!(
                seen.iter().all(|&s| s),
                "簇 #{ci} 不连通：{} / {} 个三角可达",
                seen.iter().filter(|&&s| s).count(),
                tris.len()
            );
        }
    }

    /// 边界锥有效且朝外：单位球的每簇轴单位化、cutoff ∈ [-1, 1]，
    /// 且轴与「球心 → 簇中心」方向同向（点积 > 0.5——凸球面局部片元的
    /// 面法线均值必然径向朝外；背面锥若指内即此断言先红）。
    #[test]
    fn cone_valid_and_outward_on_sphere() {
        let sphere = MeshData::uv_sphere(1.0, 48, 32);
        let mesh = build_meshlets(&sphere.vertices, &sphere.indices);
        for (ci, c) in mesh.clusters.iter().enumerate() {
            let len = (c.cone_axis[0] * c.cone_axis[0]
                + c.cone_axis[1] * c.cone_axis[1]
                + c.cone_axis[2] * c.cone_axis[2])
                .sqrt();
            assert!(
                (len - 1.0).abs() < 1e-5,
                "簇 #{ci} 锥轴未单位化：长度 {len}"
            );
            assert!(
                (-1.0..=1.0).contains(&c.cone_cutoff_cos),
                "簇 #{ci} cutoff 越界：{}",
                c.cone_cutoff_cos
            );
            let axis = Vec3::from_array(c.cone_axis);
            // 簇中心 = 归一化 bounds 逆变换到世界（bbox [-1,1] ⇒ (n−0.5)×2）
            let center = Vec3::new(c.bounds[0], c.bounds[1], c.bounds[2]) * 2.0 - Vec3::ONE;
            let radial = center.normalize();
            assert!(
                axis.dot(radial) > 0.5,
                "簇 #{ci} 锥轴应径向朝外：axis·radial = {}",
                axis.dot(radial)
            );
            assert!(
                c.bounds[3] > 0.0 && c.bounds[3] <= 2.0,
                "簇 #{ci} 包围球半径应 ∈ (0, 2]：{}",
                c.bounds[3]
            );
        }
    }

    /// 立方体解析解：24 顶点在顶点图上是 6 个孤立面片（面间法线分裂
    /// 不共享）⇒ 恰 6 簇、每簇 2 三角 / 4 顶点；+Z 面簇逐项解析对照
    /// （锥轴 = 面法线精确、cutoff = 1 精确、球心归一化 (0.5,0.5,1.0)、
    /// 半径 = √0.5）。
    #[test]
    fn cube_face_clusters_metadata_analytic() {
        let cube = MeshData::cube();
        let mesh = build_meshlets(&cube.vertices, &cube.indices);
        assert_eq!(mesh.clusters.len(), 6, "6 个孤立面片各成一簇");
        let tris: usize = mesh
            .clusters
            .iter()
            .map(|c| c.triangle_count as usize)
            .sum();
        let verts: usize = mesh.clusters.iter().map(|c| c.vertex_count as usize).sum();
        assert_eq!((tris, verts), (12, 24), "覆盖守恒：12 三角 / 24 顶点");
        for c in &mesh.clusters {
            assert_eq!((c.triangle_count, c.vertex_count), (2, 4));
            assert_eq!(c.error, 0.0, "V1 无简化器：error 恒 0");
        }
        // +Z 面 = cube() 的第一个面 → 首簇。全簇解析对照：
        let c = &mesh.clusters[0];
        assert_eq!(c.cone_axis, [0.0, 0.0, 1.0], "+Z 面锥轴 = 面法线（精确）");
        assert_eq!(c.cone_cutoff_cos, 1.0, "正对面 cutoff = 1（精确）");
        assert!(
            (c.bounds[0] - 0.5).abs() < 1e-6
                && (c.bounds[1] - 0.5).abs() < 1e-6
                && (c.bounds[2] - 1.0).abs() < 1e-6,
            "球心归一化应 = (0.5, 0.5, 1.0)：{:?}",
            &c.bounds[..3]
        );
        let expect_radius = 0.5f32.sqrt();
        assert!(
            (c.bounds[3] - expect_radius).abs() < 1e-5,
            "半径应 = √0.5 ≈ {expect_radius}（面心到角），实际 {}",
            c.bounds[3]
        );
        assert_eq!(c.vertex_offset, 0);
        assert_eq!(c.index_offset, 0);
        // 展开三角形集合不变
        let expanded = mesh.expand();
        assert_eq!(sorted_triangle_keys(&expanded), sorted_triangle_keys(&cube));
    }

    /// 构建器侧的锥退化兜底：两三角共边但法线 +Z / −Z 完全对消 ⇒
    /// 单簇且全向锥（cutoff = −1，永不背面剔除）。
    #[test]
    fn builder_cone_degenerates_when_normals_cancel() {
        let v = |p: [f32; 3]| Vertex {
            position: p,
            normal: [0.0, 0.0, 1.0],
            uv: [0.0; 2],
        };
        let vertices = vec![v([-1.0, 0.0, 0.0]), v([1.0, 0.0, 0.0]), v([0.0, 1.0, 0.0])];
        // 同三个顶点、相反环绕：面法线 +Z 与 −Z，均值恰零
        let indices = vec![0, 1, 2, 0, 2, 1];
        let mesh = build_meshlets(&vertices, &indices);
        assert_eq!(mesh.clusters.len(), 1, "共享全部顶点 ⇒ 单簇");
        assert_eq!(mesh.clusters[0].cone_axis, [0.0, 0.0, 1.0]);
        assert_eq!(mesh.clusters[0].cone_cutoff_cos, -1.0);
    }

    /// 平面网格：全簇法线同向 ⇒ cutoff ≈ 1（正对面簇，背面剔除收益
    /// 最大的形态）。
    #[test]
    fn flat_plane_cluster_has_cutoff_one() {
        let plane = MeshData::plane(4.0);
        let mesh = build_meshlets(&plane.vertices, &plane.indices);
        assert_eq!(mesh.clusters.len(), 1);
        assert!(
            (mesh.clusters[0].cone_cutoff_cos - 1.0).abs() < 1e-5,
            "平面簇 cutoff ≈ 1，实际 {}",
            mesh.clusters[0].cone_cutoff_cos
        );
        // Y 轴 extent = 0（退化轴）：归一化 y 分量除数取 1.0（= 0）
        assert!((mesh.clusters[0].bounds[1] - 0.0).abs() < 1e-6);
    }

    /// 单三角网格：1 簇 3 顶点，偏移全 0。
    #[test]
    fn single_triangle_mesh() {
        let tri = MeshData {
            vertices: vec![
                Vertex {
                    position: [0.0, 0.0, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    uv: [0.0, 0.0],
                },
                Vertex {
                    position: [1.0, 0.0, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    uv: [1.0, 0.0],
                },
                Vertex {
                    position: [0.0, 1.0, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    uv: [0.0, 1.0],
                },
            ],
            indices: vec![0, 1, 2],
            skin: None,
            tangents: None,
            uv2: None,
        };
        let mesh = build_meshlets(&tri.vertices, &tri.indices);
        assert_eq!(mesh.clusters.len(), 1);
        let c = mesh.clusters[0];
        assert_eq!((c.vertex_count, c.triangle_count), (3, 1));
        assert_eq!((c.vertex_offset, c.index_offset), (0, 0));
        assert_eq!(mesh.indices, vec![0, 1, 2], "单三角重映射 = 恒等");
    }

    /// 空输入合法：全空产出，展开为空 MeshData。
    #[test]
    fn empty_input_yields_empty_mesh() {
        let mesh = build_meshlets(&[], &[]);
        assert!(mesh.vertices.is_empty() && mesh.indices.is_empty() && mesh.clusters.is_empty());
        let expanded = mesh.expand();
        assert!(expanded.vertices.is_empty() && expanded.indices.is_empty());
    }

    /// 非法输入断言拦截：索引越界 / 截断三角（调试与 release 同拦——
    /// 普通 assert 非 debug_assert，无需 cfg 门控）。
    #[test]
    #[should_panic(expected = "越界")]
    fn rejects_out_of_range_index() {
        let verts = [Vertex {
            position: [0.0; 3],
            normal: [0.0, 1.0, 0.0],
            uv: [0.0; 2],
        }; 3];
        build_meshlets(&verts, &[0, 1, 7]);
    }

    #[test]
    #[should_panic(expected = "3 的倍数")]
    fn rejects_truncated_triangle() {
        let verts = [Vertex {
            position: [0.0; 3],
            normal: [0.0, 1.0, 0.0],
            uv: [0.0; 2],
        }; 4];
        build_meshlets(&verts, &[0, 1, 2, 3]);
    }

    /// 资格判定：蒙皮 / 切线 / UV2 任一存在即不合格（V1 容器只载标准顶点）。
    #[test]
    fn eligibility_rejects_extended_attributes() {
        let mut data = MeshData::cube();
        assert!(is_meshlet_eligible(&data));
        data.skin = Some(vec![
            iris_render::mesh::SkinVertex {
                joints: [0; 4],
                weights: [0.0; 4]
            };
            24
        ]);
        assert!(!is_meshlet_eligible(&data));
        data.skin = None;
        data.tangents = Some(vec![[0.0, 0.0, 1.0, 1.0]; 24]);
        assert!(!is_meshlet_eligible(&data));
        data.tangents = None;
        data.uv2 = Some(vec![[0.0, 0.0]; 24]);
        assert!(!is_meshlet_eligible(&data));
    }
}
