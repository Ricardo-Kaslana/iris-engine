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

//! # 层级构建（R86，虚拟几何体 V3）
//!
//! [`build_meshlet_hierarchy`] 在 R84 聚类的上游加一层自底向上的簇 DAG：
//! 叶簇按固定贪心归并成组、组内几何经体素网格顶点聚类简化
//! （[`simplify_by_vertex_clustering`]，Rossignac）后重新聚类为父组的
//! 自有簇，递归到根（单组 / 三角数阈值 / 层数上限）。选型裁定：QEM 边
//! 坍缩质量更高，但堆序 / 半边结构的确定性成本过高；顶点聚类对三角汤
//! 天然友好、简化 = 空间网格函数 ⇒ **逐位确定免费**（质量折衷诚实入档
//! §8）。要点：
//!
//! - **边界顶点锁定**：与本块（chunk）之外的组共享的顶点（构建期固定
//!   扫描判定）永不合并——压住组间接缝裂缝；残余 T 型连接仍可能，诚实
//!   限制；
//! - **误差口径**：own_error = 实际使用的 cell × √3/2（格对角半长 =
//!   合并位移上界）；cell(L) = 叶簇包围球半径均值 × 系数 × 2^L，三角数
//!   未达「组内一半」时有限次倍增（≤ 4 次）；group_error =
//!   max(own, 所有 children) 沿 DAG 向上单调增；
//! - **簇层布局**：叶簇在前，每层父组自有簇按构建序追加 ⇒ 组范围连续、
//!   「子组号 < 父组号」；全部簇 / 组 bounds 在收尾时相对**整网格**
//!   包围盒统一归一（与渲染器注册口径一致）。

use std::collections::{BTreeSet, HashMap};

use glam::Vec3;
use iris_render::mesh::{MeshData, Vertex};
use iris_render::meshlet::{
    MESHLET_CLUSTER_MAX_TRIANGLES, MeshletClusterRaw, MeshletHierarchy, MeshletLodGroup,
    MeshletMesh, cluster_bounding_sphere, cluster_bounds_normalized, cluster_cone,
};
use iris_render::meshlet_stream::{MESHLET_PAGE_CLUSTERS, MeshletPageTable};

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

    // 页划分（R87）：构建期定稿并随产物携带（写出落 IRML v3 页区段；
    // 渲染器注册期直接消费，v1/v2 旧产物按同参数现算等价）。
    mesh.pages = Some(MeshletPageTable::derive(&mesh, MESHLET_PAGE_CLUSTERS));
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
// R86：体素网格顶点聚类简化 + 层级（簇 DAG）构建
// ---------------------------------------------------------------------------

/// 简化误差系数：cell × √3/2 = 格对角半长（格内任意点到格心的最大位移）。
/// √3/2 ≈ 0.8660254（const 环境不可调 sqrt，字面值 + 单测锁近似误差）。
const CLUSTERING_ERROR_FACTOR: f32 = 0.866_025_4_f32;

/// 三角数目标未达时 cell 的最大倍增次数（有界自适应——确定性不受影响，
/// 倍增序列由输入唯一决定）。
const SIMPLIFY_MAX_CELL_DOUBLINGS: usize = 4;

/// 层级构建默认层数上限（含叶层）。
pub const MESHLET_HIERARCHY_MAX_LEVELS: u32 = 8;

/// 退化判定阈值（叉积长度），与聚类元数据的面法线预过滤同值。
const DEGENERATE_CROSS: f32 = 1e-12;

/// 顶点属性键：32 B 标准顶点的 8 × f32 位型串行（position 3 + normal 3 +
/// uv 2）。跨簇去重靠**逐位相等**——同一源顶点在各簇中的副本逐位一致
/// （构建期拷贝），位级键 = 语义相等；HashMap 只查不迭代（确定性硬文化）。
type VertKey = [u32; 8];

fn vertex_key(v: &Vertex) -> VertKey {
    let mut key = [0u32; 8];
    for (k, f) in v
        .position
        .iter()
        .chain(v.normal.iter())
        .chain(v.uv.iter())
        .enumerate()
    {
        key[k] = f.to_bits();
    }
    key
}

/// [`simplify_by_vertex_clustering`] 的产物（规格三元组「新 positions /
/// 新 indices / 误差」+ 属性携带与诊断计数）。
pub struct SimplifiedGeometry {
    /// 新顶点位置（位级确定：合并桶 = 格心，单点桶 / 锁定顶点 = 原位）。
    pub positions: Vec<Vec3>,
    /// 新索引（指向 `positions`；退化与重复三角已剔除）。
    pub indices: Vec<u32>,
    /// 误差 = 实际使用的 cell × √3/2（发生合并时）；未发生任何合并 = 0。
    pub error: f32,
    /// 新顶点 k 的属性来源 = 首见代表旧顶点号（法线 / UV 原样搬运；
    /// 位置被吸附时代表顶点的原位置不保留）。
    pub representative: Vec<u32>,
    /// 退化剔除计数（合并后重复顶点槽 / 零面积）。
    pub degenerate_dropped: usize,
    /// 同格重复三角去重计数（规范序键 = 三顶点号升序）。
    pub duplicate_dropped: usize,
}

/// 网格尺寸 `cell` 的顶点聚类简化（Rossignac；R86 简化器，纯函数）。
///
/// 顶点按空间格吸附：同一格内的**可合并**顶点收敛为一个代表（首见顶点
/// 供属性），代表位置 = 格心 `(floor(p/cell) + 0.5) × cell`；格内仅单个
/// 顶点时**原位保留**（无合并不位移）。`boundary` 中的顶点（组边界集，
/// 构建期固定扫描判定）**永不合并**——即使同格也各占独立桶、位置原样。
/// 合并后重复顶点槽 / 零面积（叉积 < 1e-12）三角剔除；三顶点号升序
/// 键相同的重复三角只保留首见。误差 = cell × √3/2（格对角半长 = 合并
/// 位移上界）；未发生任何合并 = 0.0（诚实口径：几何未动即无误差）。
///
/// 确定性：格吸附是位置的空间函数、代表取首见索引、去重键为规范序、
/// 浮点仅加减乘除 floor 与 sqrt ⇒ 同输入两跑输出逐位一致。
///
/// # 输入约束
///
/// `indices` 必须是 3 的倍数且全部在 `positions` 范围内（断言拦截）；
/// `cell` 必须有限且为正。
///
/// # 诚实限制（f32 量化）
///
/// 格坐标 = `floor(p / cell)`：`p / cell` 超出 f32 精确整数范围（量级
/// ≳ 2²³）时近邻顶点的商会被舍入到同一值 ⇒ 极小 cell（相对坐标量级）
/// 反而可能发生意外合并。构建期 cell 由几何尺寸导出（叶簇半径量级），
/// 正常输入远离该区域；逐位确定性不受影响（舍入本身是确定的）。
pub fn simplify_by_vertex_clustering(
    positions: &[Vec3],
    indices: &[u32],
    boundary: &BTreeSet<u32>,
    cell: f32,
) -> SimplifiedGeometry {
    assert!(
        cell.is_finite() && cell > 0.0,
        "体素 cell 必须为正的有限值：{cell}"
    );
    assert!(
        indices.len() % 3 == 0,
        "索引数必须是 3 的倍数（截断的三角形无法简化）：{}",
        indices.len()
    );
    for (slot, &idx) in indices.iter().enumerate() {
        assert!(
            (idx as usize) < positions.len(),
            "索引 #{slot} 越界：{idx} ≥ 顶点数 {}",
            positions.len()
        );
    }

    // 桶键 =（格坐标，锁定哨兵）：可合并顶点共享格键；边界顶点加自身
    // 序号哨兵——同格也互不合并（永不合并语义）。
    let cell_key = |p: Vec3| -> [i64; 3] {
        [
            (p.x / cell).floor() as i64,
            (p.y / cell).floor() as i64,
            (p.z / cell).floor() as i64,
        ]
    };
    let mut bucket_of: HashMap<([i64; 3], u32), u32> = HashMap::new();
    // 桶 =（首见代表，成员数，格键）。成员数 ≥ 2 的桶吸附到格心。
    let mut buckets: Vec<(u32, usize, [i64; 3])> = Vec::new();
    let mut vertex_map = vec![0u32; positions.len()];
    for (vi, &p) in positions.iter().enumerate() {
        let grid = cell_key(p);
        let sentinel = if boundary.contains(&(vi as u32)) {
            vi as u32 + 1
        } else {
            0
        };
        let next = buckets.len() as u32;
        match bucket_of.get(&(grid, sentinel)) {
            Some(&b) => {
                vertex_map[vi] = b;
                buckets[b as usize].1 += 1;
            }
            None => {
                bucket_of.insert((grid, sentinel), next);
                vertex_map[vi] = next;
                buckets.push((vi as u32, 1, grid));
            }
        }
    }
    // 桶 → 输出顶点：单成员桶原位保留（位级原样），多成员桶 = 格心。
    let mut positions_out = Vec::with_capacity(buckets.len());
    let mut representative = Vec::with_capacity(buckets.len());
    let mut merged_any = false;
    for &(rep, members, grid) in &buckets {
        representative.push(rep);
        if members > 1 {
            merged_any = true;
            positions_out.push(Vec3::new(
                (grid[0] as f32 + 0.5) * cell,
                (grid[1] as f32 + 0.5) * cell,
                (grid[2] as f32 + 0.5) * cell,
            ));
        } else {
            positions_out.push(positions[rep as usize]);
        }
    }

    // 三角重映射 + 退化剔除 + 规范序去重。
    let mut indices_out = Vec::with_capacity(indices.len());
    let mut dedup: HashMap<[u32; 3], ()> = HashMap::new();
    let mut degenerate_dropped = 0usize;
    let mut duplicate_dropped = 0usize;
    for tri in indices.chunks_exact(3) {
        let a = vertex_map[tri[0] as usize];
        let b = vertex_map[tri[1] as usize];
        let c = vertex_map[tri[2] as usize];
        let collapsed = a == b || b == c || a == c;
        let pa = positions_out[a as usize];
        let pb = positions_out[b as usize];
        let pc = positions_out[c as usize];
        if collapsed || (pb - pa).cross(pc - pa).length() < DEGENERATE_CROSS {
            degenerate_dropped += 1;
            continue;
        }
        let mut key = [a, b, c];
        key.sort_unstable();
        if dedup.insert(key, ()).is_some() {
            duplicate_dropped += 1;
            continue;
        }
        indices_out.extend_from_slice(&[a, b, c]);
    }

    SimplifiedGeometry {
        positions: positions_out,
        indices: indices_out,
        error: if merged_any {
            cell * CLUSTERING_ERROR_FACTOR
        } else {
            0.0
        },
        representative,
        degenerate_dropped,
        duplicate_dropped,
    }
}

/// 层级构建配置（R86）。缺省值：块上限 8 / 层数上限
/// [`MESHLET_HIERARCHY_MAX_LEVELS`] / 停止三角数 128 / 误差系数 1.0。
#[derive(Clone, Copy, Debug)]
pub struct MeshletHierarchyConfig {
    /// 每轮归并的块大小上限（连续贪心分组；组沿叶簇构建序天然空间连续
    /// ——「固定字典序评分」的 R84 风格落地面：评分 = 组序邻接。余 1
    /// 组的尾块允许 max+1，块大小恒 ≥ 2）。
    pub max_groups_per_merge: usize,
    /// 层数上限（含叶层；递归到达即停）。
    pub max_levels: u32,
    /// 当层全部组自有三角合计低于此值 ⇒ 停止递归（其余组即为根）。
    pub stop_triangles: u32,
    /// 误差基数系数：cell(L0) = 叶簇包围球半径均值 × 本系数。依据：叶簇
    /// 半径 ≈ 网格在该处的特征尺度，以此起步每层翻倍与块合并（4-8 组
    /// 一块）的线性尺度增长同阶；折衷常数，非精调。
    pub error_coefficient: f32,
}

impl Default for MeshletHierarchyConfig {
    fn default() -> Self {
        Self {
            max_groups_per_merge: 8,
            max_levels: MESHLET_HIERARCHY_MAX_LEVELS,
            stop_triangles: 128,
            error_coefficient: 1.0,
        }
    }
}

/// 构建期间组（最终 [`MeshletLodGroup`] 的构建态；bounds / cone 在收尾
/// 归一化时统一由自有簇几何重算）。
struct GroupBuild {
    children: Vec<u32>,
    cluster_start: u32,
    cluster_count: u32,
    own_error: f32,
    group_error: f32,
}

/// 便捷入口：[`MeshData`] → 带层级的 [`MeshletMesh`]（要求
/// [`is_meshlet_eligible`]，缺省配置）。
pub fn build_meshlet_hierarchy_from_mesh_data(data: &MeshData) -> MeshletMesh {
    assert!(
        is_meshlet_eligible(data),
        "meshlet 层级构建只支持标准网格（无蒙皮 / 切线 / 光照贴图 UV）"
    );
    build_meshlet_hierarchy(&data.vertices, &data.indices)
}

/// 便捷入口（缺省配置）：顶点 + 索引 → 带层级的 [`MeshletMesh`]。
pub fn build_meshlet_hierarchy(vertices: &[Vertex], indices: &[u32]) -> MeshletMesh {
    build_meshlet_hierarchy_with_config(vertices, indices, &MeshletHierarchyConfig::default())
}

/// 层级构建（R86，自底向上簇 DAG）。
///
/// Level 0 = [`build_meshlets`] 的叶簇（每簇一组）；每轮把当前层组按
/// 连续贪心分块（≤ `max_groups_per_merge` 块，尾块保底 ≥
/// `min_groups_per_merge`），块内几何合并 → [`simplify_by_vertex_clustering`]
/// → 重聚类为父组自有簇，追加到全局缓冲。组间**边界顶点锁定**（块外
/// 共享顶点永不合并）。停止条件：单组 / 当层三角合计 < `stop_triangles`
/// / 层数到达 `max_levels`。
///
/// 诚实限制：父组几何由**子组自有几何**（已简化）继续简化——误差沿
/// DAG 复合（group_error 的 max 链忠实记录上界）；体素聚类的简化质量
/// 折衷见模块文档。
pub fn build_meshlet_hierarchy_with_config(
    vertices: &[Vertex],
    indices: &[u32],
    config: &MeshletHierarchyConfig,
) -> MeshletMesh {
    assert!(
        config.max_groups_per_merge >= 2,
        "归并块上限必须 ≥ 2（块大小恒 ≥ 2）"
    );
    assert!(config.max_levels >= 1, "层数上限必须 ≥ 1（叶层本身算一层）");
    assert!(
        config.error_coefficient.is_finite() && config.error_coefficient > 0.0,
        "误差基数系数必须为正的有限值"
    );
    let mut mesh = build_meshlets(vertices, indices);
    if mesh.clusters.is_empty() {
        return mesh;
    }
    let leaf_clusters = mesh.clusters.len() as u32;

    // 误差基数 = 叶簇包围球半径均值（簇构建序累加——确定序）× 系数。
    let mut radius_sum = 0.0f32;
    for c in &mesh.clusters {
        let base = c.vertex_offset as usize;
        let positions: Vec<Vec3> = mesh.vertices[base..base + c.vertex_count as usize]
            .iter()
            .map(|v| Vec3::from(v.position))
            .collect();
        radius_sum += cluster_bounding_sphere(&positions).1;
    }
    let base_cell = (radius_sum / mesh.clusters.len() as f32) * config.error_coefficient;

    // 组表初始化：叶层 = 每簇一组（误差 0——原始精度）。
    let mut groups: Vec<GroupBuild> = (0..leaf_clusters)
        .map(|i| GroupBuild {
            children: Vec::new(),
            cluster_start: i,
            cluster_count: 1,
            own_error: 0.0,
            group_error: 0.0,
        })
        .collect();
    let mut level_groups: Vec<u32> = (0..leaf_clusters).collect();
    let mut level: u32 = 0;

    while level_groups.len() > 1 {
        let level_tris: u32 = level_groups
            .iter()
            .map(|&g| {
                let grp = &groups[g as usize];
                (grp.cluster_start..grp.cluster_start + grp.cluster_count)
                    .map(|c| mesh.clusters[c as usize].triangle_count as u32)
                    .sum::<u32>()
            })
            .sum();
        if level_tris < config.stop_triangles || level + 1 >= config.max_levels {
            break;
        }

        // 连续贪心分块（块上限 / 尾块借用保底——块划分由组数唯一决定）。
        let chunks = chunk_consecutive(level_groups.len(), config.max_groups_per_merge);

        // 本层顶点键的块间共享判定（owner = 首见块；≥ 2 块共享 = contested
        // ——块边界顶点，简化时锁定）。块内共享（组间缝合）不锁定。
        let mut owner: HashMap<VertKey, usize> = HashMap::new();
        let mut contested: BTreeSet<VertKey> = BTreeSet::new();
        for (ci, &(start, take)) in chunks.iter().enumerate() {
            for &g in &level_groups[start..start + take] {
                let grp = &groups[g as usize];
                for c in grp.cluster_start..grp.cluster_start + grp.cluster_count {
                    let cluster = &mesh.clusters[c as usize];
                    let vbase = cluster.vertex_offset as usize;
                    for v in &mesh.vertices[vbase..vbase + cluster.vertex_count as usize] {
                        let key = vertex_key(v);
                        match owner.get(&key) {
                            None => {
                                owner.insert(key, ci);
                            }
                            Some(&prev) if prev != ci => {
                                contested.insert(key);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // 逐块构建父组。
        let mut next_level = Vec::with_capacity(chunks.len());
        for &(start, take) in &chunks {
            let members: Vec<u32> = level_groups[start..start + take].to_vec();

            // 块内几何合并（跨组按位去重；发现序 = 组升序 × 簇升序 ×
            // 簇内顶点序）。
            let mut dedup: HashMap<VertKey, u32> = HashMap::new();
            let mut merged_vertices: Vec<Vertex> = Vec::new();
            let mut merged_indices: Vec<u32> = Vec::new();
            let mut boundary: BTreeSet<u32> = BTreeSet::new();
            for &g in &members {
                let grp = &groups[g as usize];
                for c in grp.cluster_start..grp.cluster_start + grp.cluster_count {
                    let cluster = &mesh.clusters[c as usize];
                    let vbase = cluster.vertex_offset as usize;
                    let ibase = cluster.index_offset as usize;
                    let icount = cluster.triangle_count as usize * 3;
                    for local in &mesh.indices[ibase..ibase + icount] {
                        let global = vbase + *local as usize;
                        let key = vertex_key(&mesh.vertices[global]);
                        let local_id = match dedup.get(&key) {
                            Some(&id) => id,
                            None => {
                                let id = merged_vertices.len() as u32;
                                dedup.insert(key, id);
                                merged_vertices.push(mesh.vertices[global]);
                                if contested.contains(&key) {
                                    boundary.insert(id);
                                }
                                id
                            }
                        };
                        merged_indices.push(local_id);
                    }
                }
            }

            // 简化：cell = 基数 × 2^level；未达「组内一半」时有限次倍增。
            let target = merged_indices.len() / 2;
            let mut cell = base_cell;
            let mut pow = level;
            while pow > 0 {
                cell *= 2.0;
                pow -= 1;
            }
            let positions: Vec<Vec3> = merged_vertices
                .iter()
                .map(|v| Vec3::from(v.position))
                .collect();
            let mut out =
                simplify_by_vertex_clustering(&positions, &merged_indices, &boundary, cell);
            let mut doublings = 0usize;
            while out.indices.len() / 3 > target && doublings < SIMPLIFY_MAX_CELL_DOUBLINGS {
                cell *= 2.0;
                doublings += 1;
                out = simplify_by_vertex_clustering(&positions, &merged_indices, &boundary, cell);
            }
            // 退化护栏：全退化（输出 0 三角）时回退未简化几何（own_error
            // = 0——无简化即无误差）。
            let (new_vertices, new_indices, own_error) = if out.indices.is_empty() {
                (merged_vertices.clone(), merged_indices.clone(), 0.0)
            } else {
                let new_vertices: Vec<Vertex> = out
                    .positions
                    .iter()
                    .enumerate()
                    .map(|(k, p)| Vertex {
                        position: [p.x, p.y, p.z],
                        ..merged_vertices[out.representative[k] as usize]
                    })
                    .collect();
                (new_vertices, out.indices.clone(), out.error)
            };

            // 重聚类（复用 R84 聚类）并追加到全局缓冲（偏移重定基）。
            let clustered = build_meshlets(&new_vertices, &new_indices);
            if clustered.clusters.is_empty() {
                // 理论不可达（三角护栏已保非空）；防御式回退同上。
                continue;
            }
            let cluster_start = mesh.clusters.len() as u32;
            let vertex_base = mesh.vertices.len() as u32;
            let index_base = mesh.indices.len() as u32;
            for c in &clustered.clusters {
                mesh.clusters.push(MeshletClusterRaw {
                    vertex_offset: c.vertex_offset + vertex_base,
                    index_offset: c.index_offset + index_base,
                    ..*c
                });
            }
            mesh.vertices.extend(clustered.vertices);
            mesh.indices.extend(clustered.indices);

            let group_error = members
                .iter()
                .map(|&g| groups[g as usize].group_error)
                .fold(own_error, f32::max);
            next_level.push(groups.len() as u32);
            groups.push(GroupBuild {
                children: members,
                cluster_start,
                cluster_count: clustered.clusters.len() as u32,
                own_error,
                group_error,
            });
        }
        level_groups = next_level;
        level += 1;
    }

    // 收尾归一化：全部簇 / 组的 bounds / cone 相对**整网格**包围盒重算
    // （渲染器注册口径 = 整网格 mesh_bounds——层级追加改包围盒后仍一致）。
    let (bbox_min, bbox_extent) = mesh_bounds(&mesh.vertices);
    for c in &mut mesh.clusters {
        let base = c.vertex_offset as usize;
        let positions: Vec<Vec3> = mesh.vertices[base..base + c.vertex_count as usize]
            .iter()
            .map(|v| Vec3::from(v.position))
            .collect();
        let (center, radius) = cluster_bounding_sphere(&positions);
        c.bounds = cluster_bounds_normalized(center, radius, bbox_min, bbox_extent);
    }
    let group_bounds = |grp: &GroupBuild| -> ([f32; 4], [f32; 3], f32) {
        let mut positions: Vec<Vec3> = Vec::new();
        let mut normals: Vec<Vec3> = Vec::new();
        for c in grp.cluster_start..grp.cluster_start + grp.cluster_count {
            let cluster = &mesh.clusters[c as usize];
            let vbase = cluster.vertex_offset as usize;
            positions.extend(
                mesh.vertices[vbase..vbase + cluster.vertex_count as usize]
                    .iter()
                    .map(|v| Vec3::from(v.position)),
            );
            let ibase = cluster.index_offset as usize;
            for tri in
                mesh.indices[ibase..ibase + cluster.triangle_count as usize * 3].chunks_exact(3)
            {
                let a = Vec3::from(mesh.vertices[vbase + tri[0] as usize].position);
                let b = Vec3::from(mesh.vertices[vbase + tri[1] as usize].position);
                let c3 = Vec3::from(mesh.vertices[vbase + tri[2] as usize].position);
                let cross = (b - a).cross(c3 - a);
                if cross.length() >= DEGENERATE_CROSS {
                    normals.push(cross.normalize());
                }
            }
        }
        let (center, radius) = cluster_bounding_sphere(&positions);
        let (axis, cutoff) = cluster_cone(&normals);
        (
            cluster_bounds_normalized(center, radius, bbox_min, bbox_extent),
            axis,
            cutoff,
        )
    };
    mesh.hierarchy = MeshletHierarchy {
        groups: groups
            .iter()
            .map(|grp| {
                let (bounds, cone_axis, cone_cutoff_cos) = group_bounds(grp);
                MeshletLodGroup {
                    children: grp.children.clone(),
                    cluster_start: grp.cluster_start,
                    cluster_count: grp.cluster_count,
                    own_error: grp.own_error,
                    group_error: grp.group_error,
                    bounds,
                    cone_axis,
                    cone_cutoff_cos,
                }
            })
            .collect(),
        roots: level_groups,
        leaf_cluster_count: leaf_clusters,
    };
    // 页划分重导出（R87）：层级构建追加了父簇，build_meshlets 阶段的
    // 页表已过期——按最终簇布局 + 层级现算（含逐簇所属组号）。
    mesh.pages = Some(MeshletPageTable::derive(&mesh, MESHLET_PAGE_CLUSTERS));
    mesh
}

/// 连续贪心分块：把 `len` 个组切成若干连续块（每块 ∈ [2, max]；余 1 组
/// 时本块多吞 1——块划分由 `len` 与 `max` 唯一决定，永不留孤组块）。
fn chunk_consecutive(len: usize, max: usize) -> Vec<(usize, usize)> {
    assert!(max >= 2, "块上限必须 ≥ 2");
    let mut chunks = Vec::new();
    let mut i = 0usize;
    while i < len {
        let remaining = len - i;
        let mut take = remaining.min(max);
        if remaining - take == 1 {
            take += 1; // 尾块借用：本块多吃 1（≤ max+1），不给下一块留孤组
        }
        chunks.push((i, take));
        i += take;
    }
    chunks
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

    // ---- R86：体素聚类简化器 ----

    /// Vec3 位型三元组（to_bits 逐分量——glam 无整体 to_bits）。
    fn bits3(v: Vec3) -> [u32; 3] {
        [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()]
    }

    /// 同输入两跑输出逐位一致（确定性硬账：位置 / 误差 / 代表位型）。
    #[test]
    fn simplify_deterministic_two_runs_bit_identical() {
        let sphere = MeshData::uv_sphere(1.0, 20, 14);
        let positions: Vec<Vec3> = sphere
            .vertices
            .iter()
            .map(|v| Vec3::from(v.position))
            .collect();
        let boundary = BTreeSet::new();
        let a = simplify_by_vertex_clustering(&positions, &sphere.indices, &boundary, 0.25);
        let b = simplify_by_vertex_clustering(&positions, &sphere.indices, &boundary, 0.25);
        assert_eq!(a.positions.len(), b.positions.len());
        for (x, y) in a.positions.iter().zip(b.positions.iter()) {
            assert_eq!(bits3(*x), bits3(*y), "位置位级一致");
        }
        assert_eq!(a.indices, b.indices, "索引逐项一致");
        assert_eq!(a.error.to_bits(), b.error.to_bits(), "误差位级一致");
        assert_eq!(a.representative, b.representative, "代表一致");
        assert_eq!(a.degenerate_dropped, b.degenerate_dropped);
        assert_eq!(a.duplicate_dropped, b.duplicate_dropped);
    }

    /// 三角数递减 + 误差公式（cell × √3/2，字面常数与 f32::sqrt 计算的
    /// 解析值差 < 1e-6）+ 零 cell（极小格 = 无合并）误差为 0。
    #[test]
    fn simplify_reduces_triangles_and_error_formula() {
        let sphere = MeshData::uv_sphere(1.0, 24, 16);
        let tri_before = sphere.indices.len() / 3;
        let positions: Vec<Vec3> = sphere
            .vertices
            .iter()
            .map(|v| Vec3::from(v.position))
            .collect();
        let boundary = BTreeSet::new();
        let out = simplify_by_vertex_clustering(&positions, &sphere.indices, &boundary, 0.3);
        assert!(
            out.indices.len() / 3 < tri_before,
            "cell 0.3 必须减面（{} → {}）",
            tri_before,
            out.indices.len() / 3
        );
        let expect = 0.3f32 * 3.0f32.sqrt() / 2.0;
        assert!(
            (out.error - expect).abs() < 1e-6,
            "误差 = cell × √3/2 = {expect}，实际 {}",
            out.error
        );
        assert!(
            out.degenerate_dropped > 0,
            "球面网格大 cell 合并必然产生塌缩退化：{}",
            out.degenerate_dropped
        );
        // 无合并（cell 远小于顶点间距且坐标可被 f32 精确量化）：误差诚实
        // 归零、位置原样。极小 cell（如 1e-6）反而不保证无合并——p/cell
        // 达 1e6 量级时 f32 量化会把近邻顶点的格坐标舍入到同格（诚实限制，
        // 见 simplify 文档）。
        let plane = MeshData::plane(2.0);
        let plane_positions: Vec<Vec3> = plane
            .vertices
            .iter()
            .map(|v| Vec3::from(v.position))
            .collect();
        let tiny = simplify_by_vertex_clustering(&plane_positions, &plane.indices, &boundary, 1e-3);
        assert_eq!(
            tiny.indices.len() / 3,
            plane.indices.len() / 3,
            "cell 1e-3 无合并：三角数不变"
        );
        assert_eq!(tiny.error, 0.0, "无合并 ⇒ 无误差");
        assert_eq!(
            tiny.positions.len(),
            plane_positions.len(),
            "无合并 ⇒ 顶点数不变"
        );
        for (x, y) in tiny.positions.iter().zip(plane_positions.iter()) {
            assert_eq!(bits3(*x), bits3(*y), "无合并 ⇒ 位置原样（位级）");
        }
    }

    /// 边界顶点锁定：边界集内的顶点即使同格也互不合并、位置原样（位级
    /// ）——组间接缝不裂的根据；格心吸附只发生在可合并顶点上。
    #[test]
    fn simplify_boundary_locked_exact_and_merged_snap_to_cell_center() {
        // 4 顶点正方形（cell = 10 ⇒ 全落同格）
        let positions = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
        ];
        let indices = vec![0, 1, 2, 0, 2, 3];
        // 全部锁定：无合并，位置原样
        let locked: BTreeSet<u32> = (0..4).collect();
        let out = simplify_by_vertex_clustering(&positions, &indices, &locked, 10.0);
        assert_eq!(out.positions.len(), 4, "全锁定 ⇒ 无合并");
        for (i, p) in out.positions.iter().enumerate() {
            assert_eq!(bits3(*p), bits3(positions[i]), "锁定顶点 #{i} 位置必须原样");
        }
        // 部分锁定（顶点 3）：0-2 同格合并到格心 (0.5,0.5,0)，3 原样
        let boundary: BTreeSet<u32> = [3u32].into_iter().collect();
        let out = simplify_by_vertex_clustering(&positions, &indices, &boundary, 10.0);
        assert_eq!(out.positions.len(), 2, "0-2 合并 + 3 锁定 = 2 顶点");
        assert!(out.indices.is_empty(), "两三角合并后三点同桶 → 全塌缩剔除");
        assert_eq!(out.degenerate_dropped, 2, "合并后两三角全退化");
        // cell = 10、格键 [0,0,0] ⇒ 格心 (5,5,5)（三轴同式）
        let cell_center = Vec3::new(5.0, 5.0, 5.0);
        assert_eq!(
            bits3(out.positions[0]),
            bits3(cell_center),
            "合并桶位置 = 格心 (floor+0.5)×cell"
        );
        assert_eq!(
            bits3(out.positions[1]),
            bits3(positions[3]),
            "锁定顶点位置原样"
        );
        assert!(out.error > 0.0, "发生合并 ⇒ 误差 = cell × √3/2");
    }

    /// 同格重复三角去重（规范序键）：三格（A/B/C）各含 2 个近距顶点
    /// （cell = 0.5 内同格合并），原始两三角 (a1,b1,c1) 与 (a2,b2,c2)
    /// 合并后映射出同一规范键 (A,B,C) ⇒ 第二个只记去重。
    #[test]
    fn simplify_dedups_duplicate_triangles() {
        let positions = vec![
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(0.1, 0.0, 0.0), // A 格 ×2
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 1.1, 0.0), // B 格 ×2
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(1.1, 0.0, 0.0), // C 格 ×2
        ];
        let indices = vec![0, 2, 4, 1, 3, 5];
        let out = simplify_by_vertex_clustering(&positions, &indices, &BTreeSet::new(), 0.5);
        assert_eq!(out.positions.len(), 3, "三格各合并一桶");
        assert_eq!(out.indices.len(), 3, "第二三角 = 规范键重复，只留首见");
        assert_eq!(out.duplicate_dropped, 1, "去重计数 = 1");
        assert_eq!(out.degenerate_dropped, 0, "本形态无退化");
    }

    /// should_panic：cell 非法 / 索引截断（普通 assert，双 profile 同拦）。
    #[test]
    #[should_panic(expected = "必须为正")]
    fn simplify_rejects_nonpositive_cell() {
        simplify_by_vertex_clustering(&[Vec3::ZERO], &[0, 0, 0], &BTreeSet::new(), 0.0);
    }

    #[test]
    #[should_panic(expected = "3 的倍数")]
    fn simplify_rejects_truncated_indices() {
        simplify_by_vertex_clustering(
            &[Vec3::ZERO, Vec3::ONE, Vec3::Y, Vec3::X],
            &[0, 1, 2, 3],
            &BTreeSet::new(),
            1.0,
        );
    }

    // ---- R86：层级（簇 DAG）构建 ----

    /// 构建两跑逐位一致（顶点 / 索引 / 簇 / 层级全字段位级）。
    #[test]
    fn hierarchy_build_two_runs_bit_identical() {
        let sphere = MeshData::uv_sphere(1.0, 32, 24);
        let a = build_meshlet_hierarchy(&sphere.vertices, &sphere.indices);
        let b = build_meshlet_hierarchy(&sphere.vertices, &sphere.indices);
        assert_eq!(a.clusters.len(), b.clusters.len());
        assert_eq!(a.vertices.len(), b.vertices.len());
        assert_eq!(a.indices.len(), b.indices.len());
        for (x, y) in a.vertices.iter().zip(b.vertices.iter()) {
            for (p, q) in x.position.iter().zip(y.position) {
                assert_eq!(p.to_bits(), q.to_bits());
            }
        }
        assert_eq!(a.indices, b.indices, "索引逐项一致");
        assert_eq!(a.clusters, b.clusters, "簇元数据逐字段一致");
        assert_eq!(a.hierarchy, b.hierarchy, "层级（组 / 根 / 叶数）逐字段一致");
    }

    /// DAG 良构性：children 升序且 < 父号；group_error = max(own,
    /// children)（单调向上）；叶簇覆盖 [0, leaf_count) 且叶组一一对应；
    /// 根非空；误差沿层向上不降。
    #[test]
    fn hierarchy_dag_wellformed_error_monotone_leaf_coverage() {
        let sphere = MeshData::uv_sphere(1.0, 48, 32);
        let mesh = build_meshlet_hierarchy(&sphere.vertices, &sphere.indices);
        let h = &mesh.hierarchy;
        assert!(!h.groups.is_empty() && !h.roots.is_empty(), "层级非空");
        assert!(
            h.leaf_cluster_count as usize <= mesh.clusters.len(),
            "叶数 ≤ 簇总数（本网格必然合并出父簇：叶 {}，总 {}）",
            h.leaf_cluster_count,
            mesh.clusters.len()
        );
        for (gi, g) in h.groups.iter().enumerate() {
            assert!(g.cluster_count >= 1, "组 #{gi} 自有簇数 ≥ 1");
            assert!(
                g.cluster_start + g.cluster_count <= mesh.clusters.len() as u32,
                "组 #{gi} 簇范围越界"
            );
            let mut last = None;
            for &c in &g.children {
                assert!((c as usize) < gi, "组 #{gi} 子组号 {c} 必须 < 父号");
                assert!(last.is_none_or(|p| c > p), "组 #{gi} children 必须升序");
                last = Some(c);
            }
            if !g.children.is_empty() {
                let max_child = g
                    .children
                    .iter()
                    .map(|&c| h.groups[c as usize].group_error)
                    .fold(g.own_error, f32::max);
                assert_eq!(
                    g.group_error, max_child,
                    "组 #{gi} group_error = max(own, children)"
                );
            } else {
                assert_eq!(g.own_error, 0.0, "叶组无简化误差");
                assert_eq!(g.group_error, 0.0);
            }
        }
        // 叶组覆盖：前 leaf_count 组一一对应叶簇
        for (i, g) in h.groups[..h.leaf_cluster_count as usize].iter().enumerate() {
            assert!(
                g.children.is_empty() && g.cluster_start == i as u32 && g.cluster_count == 1,
                "组 #{i} 应为叶组（单簇）"
            );
        }
        // 根：无任何组把它们当 children
        let mut is_child = vec![false; h.groups.len()];
        for g in &h.groups {
            for &c in &g.children {
                is_child[c as usize] = true;
            }
        }
        for &r in &h.roots {
            assert!(!is_child[r as usize], "根 #{r} 不得同时是子组");
        }
    }

    /// 层数上限：max_levels = 2 ⇒ 只有一层合并（所有非叶组的 children
    /// 全是叶组）；max_levels = 1 ⇒ 层级退化为叶组列表（等同单层语义，
    /// 全叶选择仍成立）。
    #[test]
    fn hierarchy_respects_level_cap() {
        let sphere = MeshData::uv_sphere(1.0, 48, 32);
        let cfg = MeshletHierarchyConfig {
            max_levels: 2,
            ..MeshletHierarchyConfig::default()
        };
        let mesh = build_meshlet_hierarchy_with_config(&sphere.vertices, &sphere.indices, &cfg);
        let h = &mesh.hierarchy;
        assert!(
            h.groups.iter().all(|g| {
                g.children
                    .iter()
                    .all(|&c| h.groups[c as usize].children.is_empty())
            }),
            "两层封顶：任何组的 children 必须是叶组"
        );
        let cfg1 = MeshletHierarchyConfig {
            max_levels: 1,
            ..MeshletHierarchyConfig::default()
        };
        let mesh1 = build_meshlet_hierarchy_with_config(&sphere.vertices, &sphere.indices, &cfg1);
        assert!(
            mesh1.hierarchy.groups.iter().all(|g| g.children.is_empty()),
            "单层封顶：全为叶组"
        );
        assert_eq!(mesh1.hierarchy.roots.len(), mesh1.hierarchy.groups.len());
    }

    /// 阈值 0 全叶选择（构建 × 选择联动）：LOD 关闭语义下的簇列表 =
    /// R85 全叶路径；大阈值选中根（自有簇数 < 叶簇数——层级真正生效）。
    #[test]
    fn hierarchy_selection_zero_threshold_all_leaves_and_coarse_root() {
        let sphere = MeshData::uv_sphere(1.0, 48, 32);
        let mesh = build_meshlet_hierarchy(&sphere.vertices, &sphere.indices);
        let h = &mesh.hierarchy;
        assert!(
            mesh.clusters.len() > h.leaf_cluster_count as usize,
            "测试前提：层级确实合并出父簇（叶 {} < 总 {}）",
            h.leaf_cluster_count,
            mesh.clusters.len()
        );
        let (min, extent) = (Vec3::new(-1.0, -1.0, -1.0), Vec3::new(2.0, 2.0, 2.0));
        let view = iris_render::meshlet::MeshletLodView {
            bounds_min: min,
            bounds_extent: extent,
            eye_local: Vec3::new(0.0, 0.0, 1.0e4),
            error_scale: 1.0,
            screen_height: 1080.0,
            tan_half_fov: 0.5,
        };
        // threshold 0：恒细化到叶（父组误差 > 0 ⇒ 不过阈）
        let fine = iris_render::meshlet::select_lod_clusters(&mesh, &view, 0.0);
        assert_eq!(
            fine,
            (0..h.leaf_cluster_count).collect::<Vec<u32>>(),
            "threshold 0 = 全叶列表（升序恒等表）"
        );
        // 极大阈值：根（自有簇）入选，簇数 < 叶簇数
        let coarse = iris_render::meshlet::select_lod_clusters(&mesh, &view, 1.0e9);
        let coarse_count = coarse.len();
        assert!(
            coarse_count < h.leaf_cluster_count as usize,
            "极大阈值选中根：{coarse_count} < 叶 {}",
            h.leaf_cluster_count
        );
        // 单调：中等阈值落在两者之间
        let mid = iris_render::meshlet::select_lod_clusters(&mesh, &view, 1.0);
        let mid_count = mid.len();
        assert!(
            mid_count >= coarse_count && mid_count <= fine.len(),
            "阈值单调：粗细簇数单调（粗 {coarse_count} ≤ 中 {mid_count} ≤ 细 {}）",
            fine.len()
        );
    }

    /// 空输入合法：层级为空、叶数为 0。
    #[test]
    fn hierarchy_empty_input_yields_empty() {
        let mesh = build_meshlet_hierarchy(&[], &[]);
        assert!(mesh.clusters.is_empty());
        assert!(mesh.hierarchy.groups.is_empty() && mesh.hierarchy.roots.is_empty());
        assert_eq!(mesh.hierarchy.leaf_cluster_count, 0);
        assert_eq!(mesh.leaf_count(), 0);
    }

    /// 连续贪心分块形状：整块 / 尾块保底 / 借用不吃孤组。
    #[test]
    fn chunk_consecutive_shapes() {
        assert_eq!(
            chunk_consecutive(16, 8),
            vec![(0, 8), (8, 8)],
            "整除：两满块"
        );
        assert_eq!(chunk_consecutive(10, 8), vec![(0, 8), (8, 2)], "尾块 2");
        assert_eq!(chunk_consecutive(9, 8), vec![(0, 9)], "余 1 ⇒ 本块 9");
        assert_eq!(chunk_consecutive(17, 8), vec![(0, 8), (8, 9)], "17 = 8+9");
        assert_eq!(chunk_consecutive(3, 2), vec![(0, 3)], "max=2 余 1 ⇒ 3");
        assert_eq!(chunk_consecutive(2, 8), vec![(0, 2)]);
        for &(max, len) in &[(8usize, 1usize), (8, 5), (3, 7), (8, 130)] {
            let chunks = chunk_consecutive(len, max);
            assert_eq!(
                chunks.iter().map(|&(_, t)| t).sum::<usize>(),
                len,
                "分块覆盖守恒（max {max}, len {len}）"
            );
            assert!(
                chunks.iter().all(|&(_, t)| t >= 2 || (t == 1 && len == 1)),
                "无孤组块（max {max}, len {len}）"
            );
        }
    }
}
