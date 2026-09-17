//! 虚拟几何体 V4（R87）：meshlet 分页流送——页划分表、驻留状态机与
//! 驻留感知 LOD 选择。
//!
//! # v1 边界（诚实清单，越界即 bug）
//!
//! - **数据已在内存**：分页是「GPU 显存分页」——驻留裁决与按页上传/驱逐
//!   是真实的（`write_buffer` 逐页拷贝 / 清零），但页数据源是进程内的
//!   [`MeshletMesh`]，**磁盘级异步 IO 不做**（IRML 整文件仍先读进内存，
//!   见 `meshlet` 模块头）；
//! - **页槽 = arena 规范区间**：页 p 的 GPU 落点恒为它在簇 / 顶点 / 索引
//!   三个 arena 里的固有字节区间（页内簇数固定 ⇒ 区间由页号唯一决定），
//!   **不引入 packed 页池 + 页间重定向**——`meshlet_cull.wgsl` /
//!   `meshlet_draw.wgsl` 按（实例基址 + 局部号）直读 arena，零改动是
//!   R84–R86 兼容硬约束的直接推论。arena 缓冲按全量容量预留 = v1 的
//!   VRAM 诚实成本（裁决只决定「哪些字节有效可读」）；
//! - **逐实例驻留查询是 CPU 侧门控**：选择只产出「页已驻留」的区间，
//!   未驻留实例整帧跳过（无逐页 GPU 故障反馈）。
//!
//! # 页划分（[`MeshletPageTable`]）
//!
//! 固定簇数页（[`MESHLET_PAGE_CLUSTERS`]，构建期写进 IRML v3，v1/v2
//! 文件缺席时运行期按同参数现算——纯派生、逐位等价）。页 p 的簇区间
//! = `[p × cpp, min((p+1) × cpp, 簇总数))`，顶点 / 索引字节区间由簇
//! 元数据的连续衔接性直接导出（容器读侧校验）。逐簇所属层级组号
//! （[`MeshletPageTable::cluster_group`]）是优先级评定的输入：组号 =
//! 升序扫描组表时**首个**覆盖该簇的组（组范围按容器不变量互斥，重叠
//! 时最低组号赢——防御式定序）。
//!
//! 页大小定稿 16 簇/页（规格草案「如 256」的偏离，理由见 REPORT）：256
//! 簇/页下典型中等资产（数百至一两千簇）整网格只落 1–8 页，叶层与粗层
//! 几乎必然同页——粗化回退与字节预算都失去页粒度；16 簇/页 ≈ 2k 三角
//! /页 ≈ 百 KB 量级，页表开销（每页 4×u32 区间 + 逐簇组号）可忽略。
//!
//! # 驻留状态机（[`ResidencyManager`]，确定性铁律）
//!
//! CPU 纯逻辑，每帧一次 [`ResidencyManager::tick`]：
//!
//! - **优先级 = 固定字典序全序**：页键 =（页**紧迫度**的**倒序位映射**，
//!   页号升序收尾）——`BTreeSet` 升序迭代即「高优先级先驻留」，**请求
//!   顺序与队列无关**（乱序请求同队列）。**页紧迫度 = 该页内「被请求
//!   范围覆盖的簇」的簇紧迫度最小值**（不是页内全部簇——见下条）；倒序
//!   位映射（大误差优先）是对规格「最小屏幕误差升序」的**偏离声明**：
//!   按字面升序实现时叶页（组误差恒 0）恒最先驻留、粗页恒被逐出，推演
//!   结果是「预算紧张 ⇒ 叶页挤占粗页 ⇒ 选中的叶组沿 DAG 找不到任何
//!   驻留祖先 ⇒ 实例整帧消失」，与本轮「小预算 ⇒ 粗层渲染、无洞」的
//!   验收硬标准直接矛盾；倒序（粗层 base-mip 先行）让「页缺 → 父组
//!   回退」的无缝洞机制在任意预算下都有落点，「越需要越先驻留」按
//!   「缺页即物体消失」的口径理解——粗页是物体存在的下界，最需要。
//! - **簇紧迫度（误差口径）**：有所属层级组 ⇒ 该组屏幕误差（越大越
//!   紧急）；**无所属组 ⇒ `f32::INFINITY`**（最紧急）——无组簇只出现在
//!   单层网格（空层级）与「层级未覆盖」的防御态，二者都**没有父组可
//!   回退**，缺页即整块消失 ⇒ 按「缺页即物体消失」的口径取最紧急，
//!   倒序键下 `invert(∞)` 为最小键（最高优先级）。
//! - **预算**：`gpu_byte_budget`（驻留总字节）与 `pages_per_frame`
//!   （每帧上传页数）双重约束；字节预算只在驻留准入 / 驻留收缩时裁决。
//! - **驱逐**：预算放不下新页时，未驻留请求页之外**优先级最低**的驻留
//!   页让位（同全序裁决）；本帧请求的页受保护（**无抖动**——满足当前
//!   帧请求的驻留页不动）。预算收缩（运行期调低）同样先逐未请求页。
//! - **页号**：跨网格全局编号 =（网格槽基址 + 网格内页号），由渲染器
//!   注册期分配；本模块只认 u32 页号，页号 → 字节区间由调用方的闭包
//!   提供（渲染器持页表）。
//!
//! # 驻留感知 LOD 选择（[`select_lod_cluster_ranges_resident`]）
//!
//! R86 选择函数的扩展：遍历与阈值判定逐式同构（全驻留时输出与
//! `select_lod_cluster_ranges_in` 逐位一致），选中组若其簇区间覆盖的页
//! **未全部驻留** ⇒ **沿 DAG 上取父组**（父页驻留则用父，逐级上溯）；
//! 链上无任何驻留祖先（含根）⇒ `skipped = true`，实例整帧跳过。双子组
//! 回退到同一父组只产出一条区间（合并段对相同区间去重）。同时输出
//! **请求页集** =（理想集 ∪ 可绘区间 ∪ **回退链沿途祖先区间**）覆盖的
//! 页，逐页带**紧迫度**（页内被这些请求区间覆盖的簇的最小簇紧迫度）——
//! 渲染器将其喂给下一帧的 [`ResidencyManager::tick`]，理想页由此逐步
//! 流入（本帧粗化、数帧后细化收敛）。
//!
//! # 与遮挡整合的次序（R53）
//!
//! 遮挡测试用实例 AABB，与页驻留正交：未驻留实例在本模块就整帧跳过
//! （不进实例表 = 不进 cull dispatch 输入），遮挡路径无需也不得对其
//! 双重处理；遮挡隐藏的实例同样不产生流送请求（不为看不见的物体
//! 预热页）。见 `renderer.rs` 的 R87 接线段。

use std::collections::{BTreeMap, BTreeSet};

use glam::Vec3;
use iris_core::Resource;

use crate::meshlet::{
    MeshletClusterRaw, MeshletHierarchy, MeshletLodGroup, MeshletLodView, MeshletMesh,
};

/// 每页簇数（固定簇数页；见模块头「页划分」的定稿理由）。
pub const MESHLET_PAGE_CLUSTERS: u32 = 16;

/// 「无所属组」哨兵：单层网格（空层级）的簇与未被任何组范围覆盖的簇
/// （畸形层级防御）记此值——优先级评定按叶簇口径（误差 0）处理。
pub const MESHLET_NO_GROUP: u32 = u32::MAX;

// ---------------------------------------------------------------------------
// 页划分表
// ---------------------------------------------------------------------------

/// 固定簇数页的划分表（R87）：页号 → 簇 / 顶点 / 索引区间 + 逐簇所属
/// 层级组号。页内簇区间恒为 `[p × cpp, min((p+1) × cpp, 簇总数))`（cpp
/// = [`Self::clusters_per_page`]），顶点 / 索引区间由簇元数据连续衔接性
/// 导出（区间衔接性 = IRML 读侧校验的容器不变量）。
///
/// 确定性：纯派生结构，同输入两跑逐位一致（单测锁定）；`cluster_group`
/// 的「最低组号赢」规则消除组范围重叠时的歧义（容器按不变量本应互斥，
/// 内存态手搓层级防御式定序）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshletPageTable {
    /// 每页簇数（≥ 1；IRML v3 持久化字段）。
    clusters_per_page: u32,
    /// 簇总数（页区间的边界基准；= 构建时簇数组长度）。
    cluster_count: u32,
    /// 每页顶点区间起点（全局顶点号；页 p → [起点, 起点 + 页顶点数)）。
    page_vertex_start: Vec<u32>,
    /// 每页顶点数。
    page_vertex_count: Vec<u32>,
    /// 每页索引区间起点（全局索引号）。
    page_index_start: Vec<u32>,
    /// 每页索引数。
    page_index_count: Vec<u32>,
    /// 逐簇所属层级组号（[`MESHLET_NO_GROUP`] = 无组——单层 / 未覆盖）。
    cluster_group: Vec<u32>,
}

impl MeshletPageTable {
    /// 从网格现算页表（v1/v2 容器 / 内存态网格的等价路径——与构建期
    /// 写入 v3 的产物逐位一致，见单测）。
    pub fn derive(mesh: &MeshletMesh, clusters_per_page: u32) -> Self {
        Self::build(&mesh.clusters, &mesh.hierarchy, clusters_per_page)
    }

    /// 页表构建核心：簇元数据 + 层级 + 每页簇数。
    ///
    /// panic：`clusters_per_page = 0`（页划分无意义）。
    pub fn build(
        clusters: &[MeshletClusterRaw],
        hierarchy: &MeshletHierarchy,
        clusters_per_page: u32,
    ) -> Self {
        assert!(clusters_per_page >= 1, "每页簇数必须 ≥ 1（页划分无意义）");
        let total = clusters.len() as u32;
        let page_count = total.div_ceil(clusters_per_page) as usize;
        let mut page_vertex_start = Vec::with_capacity(page_count);
        let mut page_vertex_count = Vec::with_capacity(page_count);
        let mut page_index_start = Vec::with_capacity(page_count);
        let mut page_index_count = Vec::with_capacity(page_count);
        for p in 0..page_count {
            let c0 = (p as u32 * clusters_per_page) as usize;
            let c1 = (c0 + clusters_per_page as usize).min(clusters.len());
            // 簇数据按构建序连续衔接（容器读侧校验的不变量）⇒ 页区间 =
            // 首簇起点 + 末簇终点。
            let first = &clusters[c0];
            let last = &clusters[c1 - 1];
            page_vertex_start.push(first.vertex_offset);
            page_vertex_count
                .push(last.vertex_offset + last.vertex_count as u32 - first.vertex_offset);
            page_index_start.push(first.index_offset);
            page_index_count
                .push(last.index_offset + last.triangle_count as u32 * 3 - first.index_offset);
        }
        // 逐簇所属组号：升序扫描组表，首个覆盖者赢（见类型文档）。
        let mut cluster_group = vec![MESHLET_NO_GROUP; clusters.len()];
        for (g, grp) in hierarchy.groups.iter().enumerate() {
            let start = (grp.cluster_start as usize).min(clusters.len());
            let end = (start + grp.cluster_count as usize).min(clusters.len());
            for slot in &mut cluster_group[start..end] {
                if *slot == MESHLET_NO_GROUP {
                    *slot = g as u32;
                }
            }
        }
        Self {
            clusters_per_page,
            cluster_count: total,
            page_vertex_start,
            page_vertex_count,
            page_index_start,
            page_index_count,
            cluster_group,
        }
    }

    /// 从已校验字段直接构建（IRML v3 页区段解码的装配口；字段合法性
    /// 由读侧校验链保证，此处不再复核）。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_parts(
        clusters_per_page: u32,
        cluster_count: u32,
        page_vertex_start: Vec<u32>,
        page_vertex_count: Vec<u32>,
        page_index_start: Vec<u32>,
        page_index_count: Vec<u32>,
        cluster_group: Vec<u32>,
    ) -> Self {
        Self {
            clusters_per_page,
            cluster_count,
            page_vertex_start,
            page_vertex_count,
            page_index_start,
            page_index_count,
            cluster_group,
        }
    }

    /// 每页簇数。
    pub fn clusters_per_page(&self) -> u32 {
        self.clusters_per_page
    }

    /// 簇总数（划分基准）。
    pub fn cluster_count(&self) -> u32 {
        self.cluster_count
    }

    /// 页数（`ceil(簇总数 / 每页簇数)`；空网格 = 0）。
    pub fn page_count(&self) -> u32 {
        self.cluster_count.div_ceil(self.clusters_per_page)
    }

    /// 页 p 的簇区间 `[start, start + count)`（`page < page_count` 前置）。
    pub fn page_cluster_range(&self, page: u32) -> (u32, u32) {
        let start = page * self.clusters_per_page;
        let count = self.clusters_per_page.min(self.cluster_count - start);
        (start, count)
    }

    /// 页 p 的顶点区间（全局顶点号空间）。
    pub fn page_vertex_range(&self, page: u32) -> (u32, u32) {
        (
            self.page_vertex_start[page as usize],
            self.page_vertex_count[page as usize],
        )
    }

    /// 页 p 的索引区间（全局索引号空间）。
    pub fn page_index_range(&self, page: u32) -> (u32, u32) {
        (
            self.page_index_start[page as usize],
            self.page_index_count[page as usize],
        )
    }

    /// 逐簇所属组号视图。
    pub fn cluster_group(&self) -> &[u32] {
        &self.cluster_group
    }

    /// 页 p 的字节占用 = 簇元数据（48 B/簇）+ 顶点（32 B/个）+ 索引
    /// （4 B/个）——GPU arena 三区间的页内切片合计，驻留预算的记账口径。
    pub fn page_bytes(&self, page: u32) -> u64 {
        let (_, cc) = self.page_cluster_range(page);
        let (_, vc) = self.page_vertex_range(page);
        let (_, ic) = self.page_index_range(page);
        cc as u64 * std::mem::size_of::<MeshletClusterRaw>() as u64
            + vc as u64 * std::mem::size_of::<crate::mesh::Vertex>() as u64
            + ic as u64 * 4
    }
}

// ---------------------------------------------------------------------------
// 流送设置资源
// ---------------------------------------------------------------------------

/// meshlet 分页流送设置（R87，World 资源）。
///
/// **缺席语义**：资源缺席，或 [`Self::gpu_byte_budget`] = `None`（无预算）
/// ⇒ 流送整段不激活 = 全驻留 = R86 行为逐位不变（默认兼容形态：页表仍
/// 随注册构建，但不做门控、不做逐页上传，arena 全量上传照旧）。预算为
/// `Some` ⇒ 页按优先级分帧上传（[`ResidencyManager`] 裁决），驻留感知
/// 选择门控绘制。
#[derive(Clone, Copy, Debug, Resource)]
pub struct MeshletStreamSettings {
    /// GPU 驻留字节预算（`None` = 无预算 = 全驻留）。
    pub gpu_byte_budget: Option<u64>,
    /// 每帧最多上传（准入）页数（速率上限；收敛速度 = 预算 ÷ 页均大小
    /// 与本值的较大者）。运行期取 `max(1)`。
    pub pages_per_frame: usize,
}

impl Default for MeshletStreamSettings {
    fn default() -> Self {
        Self {
            gpu_byte_budget: None,
            pages_per_frame: 4,
        }
    }
}

// ---------------------------------------------------------------------------
// 驻留状态机
// ---------------------------------------------------------------------------

/// 一帧驻留裁决（[`ResidencyManager::tick`] 的输出）：渲染器按序执行
/// ——先逐 `evict`（页数据清零），后传 `upload`（页数据写规范区间）。
/// 两表互斥（驱逐只出未请求页、准入只进未驻留页），表内已按执行序
/// 排好（确定性）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResidencyOps {
    /// 本帧要驱逐的页（执行序 = 优先级降序——最低优先级先让位）。
    pub evict: Vec<u32>,
    /// 本帧要上传的页（执行序 = 优先级升序——最需要先落盘）。
    pub upload: Vec<u32>,
}

/// GPU 显存页驻留状态机（R87，CPU 纯逻辑）。
///
/// 状态 = 驻留页集合（`BTreeSet`，页号全序）+ 驻留字节记账 + 逐页
/// 优先级键缓存。**不持久待传队列**：请求集每帧由收集期重建（选理想
/// 集），「乱序请求同队列」由键排序天然成立。确定性：全部迭代序来自
/// `BTreeMap` / `BTreeSet` 键序，浮点键经 [`order_key_bits`] 位映射成
/// 全序 u64——同输入两跑裁决逐位一致（单测锁定）。
#[derive(Default)]
pub struct ResidencyManager {
    resident: BTreeSet<u32>,
    resident_bytes: u64,
    /// 页号 → 倒序优先级键（小 = 高优先级；见模块头「优先级」）。
    /// 从未请求过的驻留页（防御态）按 [`u64::MAX`]（最低优先级）裁决。
    keys: BTreeMap<u32, u64>,
}

/// f32 紧迫度 → 全序 u64 位键（非负浮点位型单调 ⇒ 直接位比较；NaN 归一
/// 到 +∞——误差上界口径）。**正键**（紧迫度升序 = 紧急度降序）。
fn order_key_bits(err: f32) -> u64 {
    let e = if err.is_nan() { f32::INFINITY } else { err };
    e.max(0.0).to_bits() as u64
}

/// 倒序键（驻留优先级）：大紧迫度键小 = 先驻留；叶页（误差 0）键 =
/// `u64::MAX` = 最后驻留、最先被逐（见模块头偏离声明的定稿理由）。
///
/// `+∞` 是**合法输入**（无组簇 = 无回退 = 最紧急）：`invert_order_key(∞)
/// = 0xFFFF_FFFF_807F_FFFF`，小于任何有限误差的倒序键 ⇒ 最高优先级。
fn invert_order_key(err: f32) -> u64 {
    !order_key_bits(err)
}

impl ResidencyManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 驻留页集合视图（渲染器的选择门控按此裁决页可用性）。
    pub fn resident(&self) -> &BTreeSet<u32> {
        &self.resident
    }

    /// 当前驻留字节记账。
    pub fn resident_bytes(&self) -> u64 {
        self.resident_bytes
    }

    /// 一帧裁决。`requests` = 本帧收集的请求（全局页号, 页最小屏幕误差）
    /// ——同页多请求取最小误差（最急视口赢，min 与请求顺序无关）；
    /// `page_bytes` = 页号 → 字节占用（调用方持页表）。
    ///
    /// 步骤（顺序即实现序）：①键合并（保护集 = 请求页并集）②预算收缩
    /// 强制执行（超预算先逐未请求页）③按优先级准入（`pages_per_frame`
    /// 封顶；放不下时逐未请求页腾位，腾不出则本页延后——下帧请求重建
    /// 时自然重试）。
    pub fn tick(
        &mut self,
        budget: u64,
        pages_per_frame: usize,
        requests: &[(u32, f32)],
        page_bytes: &dyn Fn(u32) -> u64,
    ) -> ResidencyOps {
        let pages_per_frame = pages_per_frame.max(1);
        // ① 键合并 + 队列组装：同帧同页多请求取最小误差（最急视口赢，
        //    max 与请求顺序无关）；先归并出每页最终键，再据此组装队列
        //    ——队列中每页恰一条目（陈旧键目会导致重复准入）。
        let mut requested: BTreeSet<u32> = BTreeSet::new();
        let mut latest: BTreeMap<u32, u64> = BTreeMap::new();
        for &(page, err) in requests {
            let key = invert_order_key(err);
            let merged = match latest.get(&page) {
                Some(&old) => old.max(key),
                None => key,
            };
            latest.insert(page, merged);
            requested.insert(page);
        }
        let mut queue: BTreeSet<(u64, u32)> = BTreeSet::new();
        for (&page, &key) in &latest {
            self.keys.insert(page, key); // 本帧口径覆盖（历史键只服务于未请求页的驱逐排序）
            if !self.resident.contains(&page) {
                queue.insert((key, page));
            }
        }
        let mut uploaded: BTreeSet<u32> = BTreeSet::new();
        let mut evicted: BTreeSet<u32> = BTreeSet::new();
        let mut evict = Vec::new();
        // ② 预算收缩：驻留字节超预算 ⇒ 先逐「未请求、非本帧已逐」中的
        //    最低优先级（全驻留页都被请求保护时接受暂时超预算——诚实
        //    限制：抖动比缺页贵）。
        while self.resident_bytes > budget {
            match self.pick_evictable(&requested, &evicted, &uploaded) {
                Some(page) => {
                    self.do_evict(page, page_bytes);
                    evicted.insert(page);
                    evict.push(page);
                }
                None => break,
            }
        }
        // ③ 按优先级准入：升序迭代 = 高优先级先；预算不足逐最低优先级
        //    腾位；封顶 `pages_per_frame`（队列键唯一 ⇒ 不会重复准入，
        //    防御式再查一层驻留集）。
        let mut upload = Vec::new();
        for &(_key, page) in &queue {
            if upload.len() >= pages_per_frame || self.resident.contains(&page) {
                continue;
            }
            let bytes = page_bytes(page);
            while self.resident_bytes + bytes > budget {
                match self.pick_evictable(&requested, &evicted, &uploaded) {
                    Some(victim) => {
                        self.do_evict(victim, page_bytes);
                        evicted.insert(victim);
                        evict.push(victim);
                    }
                    None => break,
                }
            }
            if self.resident_bytes + bytes <= budget {
                self.do_admit(page, bytes);
                uploaded.insert(page);
                upload.push(page);
            }
        }
        ResidencyOps { upload, evict }
    }

    /// 驱逐候选：驻留 ∧ 非本帧请求 ∧ 未在裁决中动过的页中键最大者
    /// （最低优先级）。全为保护页 ⇒ None。
    fn pick_evictable(
        &self,
        requested: &BTreeSet<u32>,
        evicted: &BTreeSet<u32>,
        uploaded: &BTreeSet<u32>,
    ) -> Option<u32> {
        let mut best: Option<(u64, u32)> = None;
        for &page in &self.resident {
            if requested.contains(&page) || evicted.contains(&page) || uploaded.contains(&page) {
                continue;
            }
            let key = self.keys.get(&page).copied().unwrap_or(u64::MAX);
            if best.is_none_or(|(k, _)| key > k) {
                best = Some((key, page));
            }
        }
        best.map(|(_, page)| page)
    }

    fn do_evict(&mut self, page: u32, page_bytes: &dyn Fn(u32) -> u64) {
        if self.resident.remove(&page) {
            self.resident_bytes -= page_bytes(page);
        }
    }

    fn do_admit(&mut self, page: u32, bytes: u64) {
        self.resident.insert(page);
        self.resident_bytes += bytes;
    }
}

// ---------------------------------------------------------------------------
// 驻留感知 LOD 选择（R86 选择函数的扩展）
// ---------------------------------------------------------------------------

/// 驻留感知选择的一次输出。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResidencySelection {
    /// 实际可绘的簇区间（升序、相邻合并、相同区间去重——run 形实例表
    /// 直接输入，口径与 R86 输出一致）。
    pub ranges: Vec<(u32, u32)>,
    /// 本帧请求页（网格局部页号）与逐页**紧迫度**，按页号升序——渲染器
    /// 换算全局页号后喂下一帧 [`ResidencyManager::tick`]。
    /// 页集 =（理想集 ∪ 可绘区间 ∪ 回退链祖先区间）覆盖的页；紧迫度 =
    /// 页内**被这些请求区间覆盖**的簇的最小簇紧迫度（页内恰同页但未被
    /// 请求的簇不参与——叶 / 父同页时不会把该页拖到叶口径）；无组簇
    /// ⇒ [`f32::INFINITY`]（无回退，最紧急）。见 `page_requests`。
    pub requests: Vec<(u32, f32)>,
    /// 选中集沿 DAG 上溯找不到任何驻留祖先 ⇒ 实例整帧跳过
    ///（`ranges` 为空，`requests` 仍含理想页——页流入后恢复）。
    pub skipped: bool,
}

/// 驻留感知层级选择：遍历与阈值判定同 [`crate::meshlet::select_lod_cluster_ranges_in`]
/// 逐式同构（全驻留 ⇒ 输出逐位一致），选中组的簇区间未全驻留时沿
/// DAG 上取第一个**区间全驻留**的祖先；链上无驻留祖先（根也未驻留）
/// ⇒ 实例整帧跳过（`skipped`）。空层级（单层网格）= 全簇单区间整块
/// 门控（无父可回退——诚实限制：流送资产须带层级才有粗化回退）。
///
/// `resident` = 网格局部页号 → 是否驻留（渲染器换算全局页号后注入）。
pub fn select_lod_cluster_ranges_resident(
    hierarchy: &MeshletHierarchy,
    total_clusters: u32,
    view: &MeshletLodView,
    threshold_px: f32,
    pages: &MeshletPageTable,
    resident: &dyn Fn(u32) -> bool,
) -> ResidencySelection {
    let mut out = ResidencySelection::default();
    if total_clusters == 0 {
        return out;
    }
    // 空层级 = 单层 = 全簇单区间整块门控（v1 容器 / --no-lod 语义）。
    if hierarchy.groups.is_empty() {
        out.requests = page_requests(pages, &[(0, total_clusters)], hierarchy, view, &[]);
        if range_resident(pages, 0, total_clusters, resident) {
            out.ranges.push((0, total_clusters));
        } else {
            out.skipped = true;
        }
        return out;
    }
    let groups = &hierarchy.groups;
    // 父组映射（children 反转；根 = NO_GROUP）——回退链的载体。
    let mut parent = vec![MESHLET_NO_GROUP; groups.len()];
    for (g, grp) in groups.iter().enumerate() {
        for &child in &grp.children {
            parent[child as usize] = g as u32;
        }
    }
    let mut ideal: Vec<(u32, u32)> = Vec::new();
    let mut emitted: Vec<(u32, u32)> = Vec::new();
    // 回退链**区间**：选中组页缺时沿途拜访的祖先组区间——必须进请求集
    // （页与紧迫度两用），否则「粗页从未被请求 ⇒ 回退永不成功 ⇒ 实例
    // 永不出现」的死锁（理想集只含叶页、预算只容粗页时的自锁）。驻留
    // 祖先的链区间与 emitted 重叠，min 合并幂等。
    let mut chain_ranges: Vec<(u32, u32)> = Vec::new();
    let mut skipped = false;
    // 显式栈 DFS（根升序、子组升序——与 R86 同序）：skip 锁存后继续
    // 收集理想集（请求完整性 ⇒ 页流入后实例恢复），只停发可绘区间。
    let mut stack: Vec<u32> = hierarchy.roots.iter().rev().copied().collect();
    while let Some(gi) = stack.pop() {
        let Some(g) = groups.get(gi as usize) else {
            continue; // 内存态手搓层级防御式跳过（容器读侧已拦越界）
        };
        let selected = if g.children.is_empty() {
            true // 叶组：恒选自身（误差为 0 也无需判定——绘制精度下界）
        } else {
            group_screen_error(g, view) <= threshold_px
        };
        if !selected {
            for &child in g.children.iter().rev() {
                stack.push(child);
            }
            continue;
        }
        ideal.push((g.cluster_start, g.cluster_count));
        if skipped {
            continue; // 已判实例跳过：只收理想集，不再产出可绘区间
        }
        if range_resident(pages, g.cluster_start, g.cluster_count, resident) {
            emitted.push((g.cluster_start, g.cluster_count));
            continue;
        }
        // 页缺：沿 DAG 上取第一个区间全驻留的祖先（父页驻留则用父）。
        let mut p = parent[gi as usize];
        loop {
            if p == MESHLET_NO_GROUP {
                skipped = true; // 根也未驻留 ⇒ 实例整帧跳过
                break;
            }
            let gp = &groups[p as usize];
            chain_ranges.push((gp.cluster_start, gp.cluster_count));
            if range_resident(pages, gp.cluster_start, gp.cluster_count, resident) {
                emitted.push((gp.cluster_start, gp.cluster_count));
                break;
            }
            p = parent[p as usize];
        }
    }
    out.skipped = skipped;
    // 区间规范化：升序排序 + 相同区间去重（双子组回退同一父组只产出
    // 一条——组范围按容器不变量互斥 ⇒ 相同区间只可能来自同一组的重复
    // 回退；去重必须先于相邻合并，否则合并段会吞掉相同的重复项）+
    // 相邻合并。
    emitted.sort_unstable_by_key(|&(start, _)| start);
    emitted.dedup();
    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(emitted.len());
    for &(start, count) in &emitted {
        match merged.last_mut() {
            Some(last) if last.0 + last.1 == start => last.1 += count,
            _ => merged.push((start, count)),
        }
    }
    out.ranges = merged;
    // 请求区间 = 理想集 ∪ 回退链（可绘区间 ⊆ 二者：驻留的选中组来自
    // 理想集、驻留的祖先来自链 ⇒ 页与紧迫度都已被覆盖，无需另列）。
    out.requests = page_requests(pages, &ideal, hierarchy, view, &chain_ranges);
    out
}

/// 全叶 run 的整块驻留门（容量退化路径的流送变体）：`[0, leaf_count)`
/// 覆盖的页全部驻留 ⇒ 恒等 run；否则 `None`（实例跳过——退化的全叶
/// 集合同样不越驻留边界）。组级细粒度回退走
/// [`select_lod_cluster_ranges_resident`]，本函数只服务超容量退化。
pub fn gate_full_leaf(
    pages: &MeshletPageTable,
    leaf_count: u32,
    resident: &dyn Fn(u32) -> bool,
) -> Option<Vec<(u32, u32)>> {
    if leaf_count == 0 {
        return Some(Vec::new());
    }
    if range_resident(pages, 0, leaf_count, resident) {
        Some(vec![(0, leaf_count)])
    } else {
        None
    }
}

/// 组屏幕误差（与 `select_lod_cluster_ranges_in` 的内联式逐式一致：
/// 球心反归一化 → `error × scale × (H/2) / (dist × tan_half)`）。
fn group_screen_error(g: &MeshletLodGroup, view: &MeshletLodView) -> f32 {
    let center =
        view.bounds_min + Vec3::new(g.bounds[0], g.bounds[1], g.bounds[2]) * view.bounds_extent;
    let dist = (center - view.eye_local).length().max(1e-6);
    g.group_error * view.error_scale * (view.screen_height * 0.5) / (dist * view.tan_half_fov)
}

/// 簇区间覆盖的页是否全部驻留（整组门控——部分驻留按未驻留裁决，
/// 回退路径保证可绘区间字节完整）。
fn range_resident(
    pages: &MeshletPageTable,
    start: u32,
    count: u32,
    resident: &dyn Fn(u32) -> bool,
) -> bool {
    if count == 0 {
        return true;
    }
    let cpp = pages.clusters_per_page().max(1);
    let p0 = start / cpp;
    let p1 = (start + count - 1) / cpp;
    (p0..=p1).all(resident)
}

/// 请求页汇总（R87 定稿口径）：理想集区间 ∪ 回退链区间覆盖的页，页号
/// 升序去重；**逐页紧迫度 = 该页内「被请求区间覆盖的簇」的簇紧迫度最小
/// 值**。
///
/// 两条口径要点（皆为「页为什么被请求」的直接推论）：
///
/// - **只统计请求方覆盖的簇**：页内恰同页但未被任何请求区间覆盖的簇不
///   参与。容器布局是叶簇在前、父簇按层追加 ⇒ 叶 / 父边界那一页通常
///   同时含两类簇；若请求方只要父簇（远景粗化的理想集），该页就必须按
///   **父簇口径**计，否则会被同页叶簇拖到最低优先级、粗化回退在该页上
///   失去落点；
/// - **无组簇（[`MESHLET_NO_GROUP`]）⇒ [`f32::INFINITY`]**：无组簇只出现
///   在单层网格（空层级）与「层级未覆盖」的防御态，二者都**没有父组可
///   回退**，缺页即整块消失 ⇒ 按「缺页即物体消失 ⇒ 最需要」的口径取
///   最紧急（倒序键下 `invert(∞)` 为最小键，优先级高于任何有限误差）。
///
/// 同页多来源取 min（min 与来源顺序无关 ⇒ 请求表与遍历序无关）。
fn page_requests(
    pages: &MeshletPageTable,
    ranges: &[(u32, u32)],
    hierarchy: &MeshletHierarchy,
    view: &MeshletLodView,
    chain_ranges: &[(u32, u32)],
) -> Vec<(u32, f32)> {
    let cpp = pages.clusters_per_page().max(1);
    let total = pages.cluster_count();
    let groups = &hierarchy.groups;
    let cluster_group = pages.cluster_group();
    let page_count = pages.page_count();
    // 簇紧迫度：有所属组 ⇒ 组屏幕误差；无组 / 组号越界（防御）⇒ +∞。
    let urgency = |c: u32| -> f32 {
        match cluster_group.get(c as usize).copied() {
            Some(g) if g != MESHLET_NO_GROUP => groups
                .get(g as usize)
                .map(|grp| group_screen_error(grp, view))
                .unwrap_or(f32::INFINITY),
            _ => f32::INFINITY,
        }
    };
    // BTreeMap：页号升序去重；同页多来源取 min。
    let mut acc: BTreeMap<u32, f32> = BTreeMap::new();
    let touch = |start: u32, count: u32, acc: &mut BTreeMap<u32, f32>| {
        if count == 0 {
            return;
        }
        let end = start.saturating_add(count).min(total);
        if end <= start {
            return;
        }
        for p in start / cpp..=(end - 1) / cpp {
            if p >= page_count {
                continue;
            }
            // 本页内属于本范围的簇段（`seg0 < seg1` 恒成立：p 由本范围
            // 的页区间枚举而来，且 start ≤ seg0、seg1 ≤ end）。
            let seg0 = (p * cpp).max(start);
            let seg1 = ((p + 1) * cpp).min(end);
            let mut m = f32::INFINITY;
            for c in seg0..seg1 {
                m = m.min(urgency(c));
            }
            match acc.get(&p) {
                Some(&old) if old <= m => {}
                _ => {
                    acc.insert(p, m);
                }
            }
        }
    };
    for &(start, count) in ranges {
        touch(start, count, &mut acc);
    }
    for &(start, count) in chain_ranges {
        touch(start, count, &mut acc);
    }
    acc.into_iter().collect()
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::Vertex;
    use crate::meshlet::{MeshletClusterRaw, MeshletLodGroup, MeshletMesh};

    /// 手搓一棵最小 DAG（与 meshlet.rs 的 lod_test_mesh 同构）：8 叶组
    ///（簇 0–7）+ 2 中层组（自有簇 8/9）+ 1 根（自有簇 10）。组球心按
    ///「bbox min = 0 / extent = 8」的归一化口径摆在 x 轴上，组误差沿
    /// DAG 单调：叶 0 < 中层 1.0 < 根 2.0。
    fn dag_mesh() -> MeshletMesh {
        let mut mesh = MeshletMesh::default();
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
                ..MeshletClusterRaw::default()
            });
        }
        let group = |children: Vec<u32>, start: u32, group_err: f32, cx: f32| MeshletLodGroup {
            children,
            cluster_start: start,
            cluster_count: 1,
            own_error: group_err,
            group_error: group_err,
            bounds: [cx / 8.0, 0.0, 0.0, 0.5],
            cone_axis: [0.0, 0.0, 1.0],
            cone_cutoff_cos: 1.0,
        };
        mesh.hierarchy.groups = vec![
            group(Vec::new(), 0, 0.0, 0.5),
            group(Vec::new(), 1, 0.0, 1.5),
            group(Vec::new(), 2, 0.0, 2.5),
            group(Vec::new(), 3, 0.0, 3.5),
            group(Vec::new(), 4, 0.0, 4.5),
            group(Vec::new(), 5, 0.0, 5.5),
            group(Vec::new(), 6, 0.0, 6.5),
            group(Vec::new(), 7, 0.0, 7.5),
            group(vec![0, 1, 2, 3], 8, 1.0, 2.5),
            group(vec![4, 5, 6, 7], 9, 1.0, 6.5),
            group(vec![8, 9], 10, 2.0, 4.5),
        ];
        mesh.hierarchy.roots = vec![10];
        mesh.hierarchy.leaf_cluster_count = 8;
        mesh
    }

    /// 选择视口（眼点在网格中轴上方 dist 处；H = 200、tan_half = 0.5、
    /// 单位缩放 ⇒ err_px(g) = group_error × 200 / dist）。
    fn view(dist: f32) -> MeshletLodView {
        MeshletLodView {
            bounds_min: Vec3::ZERO,
            bounds_extent: Vec3::new(8.0, 1.0, 1.0),
            eye_local: Vec3::new(4.5, 0.0, dist),
            error_scale: 1.0,
            screen_height: 200.0,
            tan_half_fov: 0.5,
        }
    }

    /// cpp = 3 的页表划分：p0 = 簇 0–2（叶组 0/1/2）、p1 = 3–5、
    /// p2 = 6–8（叶 6/7 + 中层 A 自有簇 8）、p3 = 9–10（中层 B 9 + 根 10）。
    fn page_table_3() -> MeshletPageTable {
        MeshletPageTable::derive(&dag_mesh(), 3)
    }

    /// 本地页驻留谓词（测试夹具）。
    fn res_of(set: &[u32]) -> impl Fn(u32) -> bool + '_ {
        let set: BTreeSet<u32> = set.iter().copied().collect();
        move |p| set.contains(&p)
    }

    // ---- 页划分 ----

    /// 划分守恒：页覆盖簇 / 顶点 / 索引恰一次（区间平铺无缝无叠），
    /// 字节合计 = 全量字节；组号落在覆盖组的范围或哨兵。
    #[test]
    fn page_partition_conserves_and_tiles() {
        let mesh = dag_mesh();
        let table = MeshletPageTable::derive(&mesh, 3);
        assert_eq!(table.page_count(), 4, "11 簇 / 每页 3 = 4 页");
        assert_eq!(table.clusters_per_page(), 3);
        // 簇平铺
        let mut covered = 0u32;
        for p in 0..table.page_count() {
            let (start, count) = table.page_cluster_range(p);
            assert_eq!(start, covered, "页 {p} 簇起点必须无缝衔接");
            covered += count;
        }
        assert_eq!(covered, 11, "全部簇恰被覆盖一次");
        // 顶点 / 索引平铺（每簇 3 顶点 3 索引 ⇒ 每页区间 = 9 的倍数段）
        for p in 0..table.page_count() {
            let (vs, vc) = table.page_vertex_range(p);
            let (is, ic) = table.page_index_range(p);
            let (c0, cc) = table.page_cluster_range(p);
            assert_eq!(vc, cc * 3, "页 {p} 顶点数 = 簇数 × 3");
            assert_eq!(ic, cc * 3, "页 {p} 索引数 = 簇数 × 3");
            assert_eq!(vs, c0 * 3, "页 {p} 顶点起点 = 簇拼接序");
            assert_eq!(is, c0 * 3, "页 {p} 索引起点 = 簇拼接序");
        }
        // 字节守恒：逐页合计 = 全量（簇 48 + 顶点 32 + 索引 4）
        let total: u64 = (0..table.page_count()).map(|p| table.page_bytes(p)).sum();
        assert_eq!(total, 11 * 48 + 33 * 32 + 33 * 4, "页字节合计 = 全量字节");
        // 组号：簇 0–7 = 叶组 0–7、8 = 组 8、9 = 组 9、10 = 组 10
        let cg = table.cluster_group();
        assert_eq!(&cg[..8], &(0u32..8).collect::<Vec<_>>()[..]);
        assert_eq!(cg[8], 8);
        assert_eq!(cg[9], 9);
        assert_eq!(cg[10], 10);
        // 空网格：0 页；单簇网格：1 页
        assert_eq!(
            MeshletPageTable::build(&[], &Default::default(), 3).page_count(),
            0
        );
    }

    /// 末页短页：17 簇 / cpp 16 ⇒ 2 页，末页 1 簇；页字节合计守恒。
    #[test]
    fn page_partition_tail_page_short() {
        let mut mesh = MeshletMesh::default();
        for i in 0..17u32 {
            mesh.clusters.push(MeshletClusterRaw {
                vertex_offset: i * 3,
                index_offset: i * 3,
                vertex_count: 3,
                triangle_count: 1,
                ..MeshletClusterRaw::default()
            });
        }
        let table = MeshletPageTable::build(&mesh.clusters, &Default::default(), 16);
        assert_eq!(table.page_count(), 2);
        assert_eq!(table.page_cluster_range(1), (16, 1), "末页短页");
        let total: u64 = (0..2).map(|p| table.page_bytes(p)).sum();
        assert_eq!(total, 17 * 48 + 51 * 32 + 51 * 4);
    }

    /// derive 与手工 build 一致（同一纯函数的两个入口）。
    #[test]
    fn derive_matches_build() {
        let mesh = dag_mesh();
        assert_eq!(MeshletPageTable::derive(&mesh, 3), {
            MeshletPageTable::build(&mesh.clusters, &mesh.hierarchy, 3)
        });
    }

    // ---- 驻留感知选择 ----

    /// 全驻留 ⇒ 与 R86 选择逐位一致（阈值 4.0：远距仅根；阈值 3.0：
    /// 中层 A/B；阈值 0：全叶）。
    #[test]
    fn resident_selection_all_pages_matches_r86() {
        let mesh = dag_mesh();
        let table = page_table_3();
        let all = res_of(&[0, 1, 2, 3]);
        for threshold in [4.0f32, 3.0, 0.0] {
            let want = crate::meshlet::select_lod_cluster_ranges_in(
                &mesh.hierarchy,
                mesh.clusters.len() as u32,
                &view(100.0),
                threshold,
            );
            let got = select_lod_cluster_ranges_resident(
                &mesh.hierarchy,
                mesh.clusters.len() as u32,
                &view(100.0),
                threshold,
                &table,
                &all,
            );
            assert!(!got.skipped, "全驻留不可能跳过");
            assert_eq!(
                got.ranges, want,
                "threshold {threshold} 全驻留必须与 R86 一致"
            );
            // 请求页覆盖全部可绘区间的页
            for &(start, count) in &got.ranges {
                let p0 = start / 3;
                let p1 = (start + count - 1) / 3;
                for p in p0..=p1 {
                    assert!(
                        got.requests.iter().any(|&(rp, _)| rp == p),
                        "可绘区间页 {p} 必须在请求集内"
                    );
                }
            }
        }
    }

    /// 粗页驻留、叶页缺 ⇒ 精确回退到父组 / 根：resident = {3}（中层 B
    /// 簇 9 + 根簇 10）时 threshold 3.0（理想 = A/B 双中层）的产出 =
    /// [(9, 2)]——A 回退到根（簇 10）、B 直接可绘（簇 9），升序合并。
    #[test]
    fn resident_selection_falls_back_to_parent_exactly() {
        let mesh = dag_mesh();
        let table = page_table_3();
        let got = select_lod_cluster_ranges_resident(
            &mesh.hierarchy,
            mesh.clusters.len() as u32,
            &view(100.0),
            3.0,
            &table,
            &res_of(&[3]),
        );
        assert!(!got.skipped);
        assert_eq!(
            got.ranges,
            vec![(9, 2)],
            "A 沿 DAG 上溯至根（10）、B 自身（9）"
        );
        // 理想请求 = 理想集（簇 8/9）覆盖的页 {2, 3} + 回退链页 {3}
        let pages: Vec<u32> = got.requests.iter().map(|&(p, _)| p).collect();
        assert_eq!(pages, vec![2, 3], "请求含理想页（簇 8 的 p2）+ 回退链页");
        // 紧迫度口径（R87 定稿）：逐页键 = **请求方覆盖簇**的紧迫度最小值。
        // 本场景请求方 = 理想集（A 簇 8 → p2；B 簇 9 → p3）+ 回退链（根簇
        // 10 → p3）⇒ p2 键 = A 的组误差、p3 键 = min(B, 根) = B 的组误差；
        // A / B 球心到眼点距离相同（|2.5 − 4.5| = |6.5 − 4.5| = 2）⇒ 两键
        // **逐位相等**，解析解 = 1.0 × (200 × 0.5) / (√10004 × 0.5) ≈ 2.0。
        for &(page, err) in &got.requests {
            assert!(
                (err - 2.0).abs() < 1e-3,
                "页 {page} 键 {err} 应为中层组误差 ≈ 2.0（旧口径下 p2 会被同页叶簇拖到 0）"
            );
        }
    }

    /// 双子组回退同一父组只产出一条（去重）：resident = {2, 3} 时
    /// threshold 0（理想 = 全叶）：叶 0–5 全部沿链回退到 A/B，叶 6/7
    /// 直接可绘 ⇒ 可绘 = 叶 6/7 + A(8) + B(9) = [(6, 4)]。
    #[test]
    fn resident_selection_dedupes_parent_fallback() {
        let mesh = dag_mesh();
        let table = page_table_3();
        let got = select_lod_cluster_ranges_resident(
            &mesh.hierarchy,
            mesh.clusters.len() as u32,
            &view(100.0),
            0.0,
            &table,
            &res_of(&[2, 3]),
        );
        assert!(!got.skipped);
        assert_eq!(
            got.ranges,
            vec![(6, 4)],
            "叶 6/7 + 回退 A/B，合并去重后单 run"
        );
    }

    /// 根也未驻留 ⇒ 实例整帧跳过（ranges 空），但理想请求仍完整（页
    /// 流入后恢复的根据）。
    #[test]
    fn resident_selection_skips_when_no_resident_ancestor() {
        let mesh = dag_mesh();
        let table = page_table_3();
        let got = select_lod_cluster_ranges_resident(
            &mesh.hierarchy,
            mesh.clusters.len() as u32,
            &view(100.0),
            3.0,
            &table,
            &res_of(&[0, 1]), // 粗页 p2/p3 全缺：所有选中组回退链断裂
        );
        assert!(got.skipped, "根未驻留必须整帧跳过");
        assert!(got.ranges.is_empty());
        let pages: Vec<u32> = got.requests.iter().map(|&(p, _)| p).collect();
        assert_eq!(pages, vec![2, 3], "跳过帧仍请求理想页（恢复路径）");
    }

    /// 单层网格（空层级）：整块门控——页全驻留 = 恒等表；任一页缺 =
    /// 跳过（无父可回退）。
    #[test]
    fn resident_selection_single_layer_gates_whole_block() {
        let mut mesh = MeshletMesh::default();
        for i in 0..5u32 {
            mesh.clusters.push(MeshletClusterRaw {
                vertex_offset: i * 3,
                index_offset: i * 3,
                vertex_count: 3,
                triangle_count: 1,
                ..MeshletClusterRaw::default()
            });
        }
        let table = MeshletPageTable::build(&mesh.clusters, &Default::default(), 2);
        assert_eq!(table.page_count(), 3);
        let full = select_lod_cluster_ranges_resident(
            &mesh.hierarchy,
            5,
            &view(100.0),
            1.0e9,
            &table,
            &res_of(&[0, 1, 2]),
        );
        assert_eq!(full.ranges, vec![(0, 5)], "全驻留 = 恒等表");
        assert!(!full.skipped);
        let partial = select_lod_cluster_ranges_resident(
            &mesh.hierarchy,
            5,
            &view(100.0),
            1.0e9,
            &table,
            &res_of(&[0, 2]),
        );
        assert!(partial.skipped, "页 1 缺 ⇒ 单层网格无回退，整块跳过");
        assert!(partial.ranges.is_empty());
    }

    /// 页请求键口径（R87 定稿）：请求表按页号升序、值 = **请求方覆盖簇**
    /// 的紧迫度最小值。极大阈值 ⇒ 理想 = 仅根（簇 10）⇒ 页 3 只被根的
    /// 区间请求 ⇒ 键 = 根误差（**不是**页内中层 B 的 2.0——B 未被请求）。
    /// 眼点对齐 B 球心（6.5, 0, 100）：B 的 dist = 100 精确、根的 dist =
    /// √(2² + 100²) = √10004 ⇒ 根误差 = 2.0 × 100 / (√10004 × 0.5) ≈ 3.999。
    #[test]
    fn page_request_keys_use_requested_clusters_only() {
        let mesh = dag_mesh();
        let table = page_table_3();
        let mut eye_view = view(100.0);
        eye_view.eye_local = Vec3::new(6.5, 0.0, 100.0); // 对齐 B 球心（6.5,0,0）
        let got = select_lod_cluster_ranges_resident(
            &mesh.hierarchy,
            mesh.clusters.len() as u32,
            &eye_view,
            1.0e9,
            &table,
            &res_of(&[0, 1, 2, 3]),
        );
        assert_eq!(got.requests.len(), 1, "极大阈值 ⇒ 仅根的自有簇页");
        assert_eq!(got.requests[0].0, 3);
        let root_err = 2.0 * 100.0 / (10004f32.sqrt() * 0.5);
        assert!(
            (got.requests[0].1 - root_err).abs() < 1e-3,
            "页键应为根误差 {root_err}（旧口径会取同页 B 的 2.0），实际 {}",
            got.requests[0].1
        );
        // 同页 B（组误差 2.0）未被请求 ⇒ 不参与 ⇒ 键严格大于 B 的误差
        assert!(
            got.requests[0].1 > 2.0,
            "同页未被请求的簇不得把该页拖到更低优先级"
        );
    }

    /// 无组簇（单层网格）的紧迫度 = `+∞`：无父组可回退 ⇒ 缺页即整块
    /// 消失 ⇒ 倒序键下取**最小键**（最高优先级），严格高于任何有限误差
    /// 的页（层级网格的粗页 / 叶页）。对照同一 `view` 下的层级网格。
    #[test]
    fn page_request_keys_prioritize_no_fallback_clusters() {
        // 单层网格：5 簇 / 每页 2 ⇒ 3 页，全部簇无所属组。
        let mut flat = MeshletMesh::default();
        for i in 0..5u32 {
            flat.clusters.push(MeshletClusterRaw {
                vertex_offset: i * 3,
                index_offset: i * 3,
                vertex_count: 3,
                triangle_count: 1,
                ..MeshletClusterRaw::default()
            });
        }
        let flat_table = MeshletPageTable::build(&flat.clusters, &Default::default(), 2);
        let flat_sel = select_lod_cluster_ranges_resident(
            &flat.hierarchy,
            5,
            &view(100.0),
            1.0e9,
            &flat_table,
            &res_of(&[0, 1, 2]),
        );
        assert_eq!(flat_sel.ranges, vec![(0, 5)], "全驻留 ⇒ 恒等表");
        assert_eq!(flat_sel.requests.len(), 3, "5 簇 / 每页 2 = 3 页");
        for &(page, err) in &flat_sel.requests {
            assert!(
                err.is_infinite() && err.is_sign_positive(),
                "单层网格页 {page} 的紧迫度应为 +∞（无回退 ⇒ 最紧急），实际 {err}"
            );
        }

        // 对照：层级网格（dag_mesh）极大阈值 ⇒ 仅根的页，键为有限值。
        let mesh = dag_mesh();
        let table = page_table_3();
        let hier_sel = select_lod_cluster_ranges_resident(
            &mesh.hierarchy,
            mesh.clusters.len() as u32,
            &view(100.0),
            1.0e9,
            &table,
            &res_of(&[0, 1, 2, 3]),
        );
        let hier_err = hier_sel.requests[0].1;
        assert!(hier_err.is_finite(), "层级网格的页键应为有限误差");
        // 倒序键全序：+∞ 的键严格小于有限误差的键 ⇒ 单层页优先驻留。
        assert!(
            invert_order_key(f32::INFINITY) < invert_order_key(hier_err),
            "+∞（无回退）必须严格优先于有限误差页：{} vs {}",
            invert_order_key(f32::INFINITY),
            invert_order_key(hier_err)
        );
    }

    // ---- 驻留状态机 ----

    /// 页字节夹具：页 p 占 (p + 1) × 100 字节（页号可判别）。
    fn bytes_fixture() -> impl Fn(u32) -> u64 {
        |p: u32| (p as u64 + 1) * 100
    }

    /// 乱序请求同队列：同一请求集任意排列 ⇒ 裁决（上传/驱逐表）逐位
    /// 一致；上传序 = 优先级序（粗页先）。
    #[test]
    fn residency_out_of_order_requests_same_queue() {
        // 页 0 = 误差 3.0（粗）、页 1 = 误差 1.0、页 2 = 误差 0.0（叶）
        let run = |shuffle: &[(u32, f32)]| {
            let mut mgr = ResidencyManager::new();
            mgr.tick(u64::MAX, 8, shuffle, &bytes_fixture())
        };
        let a = run(&[(0, 3.0), (1, 1.0), (2, 0.0)]);
        let b = run(&[(2, 0.0), (0, 3.0), (1, 1.0)]);
        let c = run(&[(1, 1.0), (2, 0.0), (0, 3.0)]);
        assert_eq!(a, b, "乱序请求同队列");
        assert_eq!(b, c);
        // 倒序键（粗页优先）：上传序 = 页 0（误差 3.0）→ 页 1 → 页 2
        assert_eq!(a.upload, vec![0, 1, 2], "粗页（大误差）先驻留");
        assert!(a.evict.is_empty());
    }

    /// 预算尊重：驻留字节恒 ≤ 预算、每帧上传页数恒 ≤ pages_per_frame；
    /// 预算不足的低优先级页延后（下帧重试——由调用方重建请求实现，
    /// 这里验证封顶）。
    #[test]
    fn residency_respects_byte_and_page_budgets() {
        let mut mgr = ResidencyManager::new();
        // 页 0/1/2 各 100/200/300 B，预算 350：粗页优先 0(100)+1(200)=300，
        // 页 2(300) 放不下（300+300 > 350 且无驱逐候选——页 0/1 都是本帧
        // 准入，受保护）⇒ 延后。
        let ops = mgr.tick(350, 8, &[(0, 3.0), (1, 1.0), (2, 0.0)], &bytes_fixture());
        assert_eq!(ops.upload, vec![0, 1], "预算 350 只容页 0/1");
        assert_eq!(mgr.resident_bytes(), 300);
        // 每帧页数封顶 = 1：第二帧续传页 1（队列重建口径）
        let mut mgr2 = ResidencyManager::new();
        let ops2 = mgr2.tick(
            u64::MAX,
            1,
            &[(0, 3.0), (1, 1.0), (2, 0.0)],
            &bytes_fixture(),
        );
        assert_eq!(ops2.upload, vec![0], "pages_per_frame = 1 封顶");
        assert_eq!(mgr2.resident_bytes(), 100);
        // 字节预算恒被尊重（驱逐腾位后准入不越界）
        let mut mgr3 = ResidencyManager::new();
        let ops3 = mgr3.tick(250, 8, &[(0, 3.0), (1, 1.0), (2, 0.0)], &bytes_fixture());
        assert!(mgr3.resident_bytes() <= 250, "驻留字节不得超预算");
        assert_eq!(ops3.upload, vec![0], "250 只容页 0");
    }

    /// 驱逐选最低优先级：预算满后新请求高优先级粗页 ⇒ 叶页（键最大）
    /// 先让位；本帧请求的驻留页受保护（无抖动）。
    #[test]
    fn residency_evicts_lowest_priority_and_protects_requested() {
        let mut mgr = ResidencyManager::new();
        // 第一帧：驻留页 2（叶，300 B）与页 1（200 B），预算 500
        mgr.tick(500, 8, &[(1, 1.0), (2, 0.0)], &bytes_fixture());
        assert_eq!(mgr.resident(), &BTreeSet::from([1, 2]));
        // 第二帧：预算 450 + 新请求页 0（粗，100 B）：300 + 100 > 450 ⇒
        // 逐最低优先级 = 页 2（叶，键最大；页 1 受本帧请求保护）
        let ops = mgr.tick(450, 8, &[(0, 3.0), (1, 1.0)], &bytes_fixture());
        assert_eq!(ops.evict, vec![2], "叶页最低优先级先让位");
        assert_eq!(ops.upload, vec![0]);
        assert_eq!(
            mgr.resident(),
            &BTreeSet::from([0, 1]),
            "请求页保护（无抖动）"
        );
        assert_eq!(mgr.resident_bytes(), 300);
    }

    /// 收敛无抖动：同一请求集反复 tick ⇒ 首帧后零操作（驻留集不动）。
    #[test]
    fn residency_converges_without_thrash() {
        let mut mgr = ResidencyManager::new();
        let req = [(0u32, 3.0f32), (1, 1.0), (2, 0.0)];
        let first = mgr.tick(u64::MAX, 8, &req, &bytes_fixture());
        assert_eq!(first.upload, vec![0, 1, 2]);
        for frame in 0..5 {
            let ops = mgr.tick(u64::MAX, 8, &req, &bytes_fixture());
            assert!(
                ops.upload.is_empty() && ops.evict.is_empty(),
                "帧 {frame} 应零操作"
            );
        }
    }

    /// 同页多请求取最小误差（min 与顺序无关）：页 0 被两个视口请求
    ///（误差 3.0 / 1.0）⇒ 键取 1.0 的（更急）——与另一个「只请求 1.0」
    /// 的会话裁决一致。
    #[test]
    fn residency_multi_request_takes_min_error() {
        let combined = {
            let mut mgr = ResidencyManager::new();
            mgr.tick(
                u64::MAX,
                8,
                &[(0, 3.0), (0, 1.0), (1, 2.0)],
                &bytes_fixture(),
            );
            (mgr.resident().clone(), mgr.resident_bytes())
        };
        let single = {
            let mut mgr = ResidencyManager::new();
            mgr.tick(u64::MAX, 8, &[(0, 1.0), (1, 2.0)], &bytes_fixture());
            (mgr.resident().clone(), mgr.resident_bytes())
        };
        assert_eq!(combined, single, "同页多请求 min 合并后裁决一致");
    }

    /// 预算收缩：驻留超预算（运行期调低）⇒ 未请求页先被逐；全被请求
    /// 保护时接受暂时超预算（诚实限制）。
    #[test]
    fn residency_budget_shrink_evicts_unrequested_first() {
        let mut mgr = ResidencyManager::new();
        mgr.tick(600, 8, &[(0, 3.0), (1, 1.0), (2, 0.0)], &bytes_fixture());
        assert_eq!(mgr.resident_bytes(), 600);
        // 预算降到 250、只请求页 0：页 2（叶，最低优先级）先逐，再页 1
        let ops = mgr.tick(250, 8, &[(0, 3.0)], &bytes_fixture());
        assert_eq!(ops.evict, vec![2, 1], "收缩逐出序 = 优先级降序（叶最先）");
        assert_eq!(mgr.resident(), &BTreeSet::from([0]));
        assert_eq!(mgr.resident_bytes(), 100);
        // 全保护：预算降到 50、页 0 仍被请求 ⇒ 接受超预算
        let ops = mgr.tick(50, 8, &[(0, 3.0)], &bytes_fixture());
        assert!(ops.evict.is_empty());
        assert_eq!(
            mgr.resident_bytes(),
            100,
            "请求保护页不逐（接受暂时超预算）"
        );
    }

    /// 位键映射：非负浮点位型单调（0 < 1 < +∞），NaN 归 +∞。
    #[test]
    fn order_key_bits_total_order() {
        assert!(order_key_bits(0.0) < order_key_bits(1.0));
        assert!(order_key_bits(1.0) < order_key_bits(f32::INFINITY));
        assert_eq!(order_key_bits(f32::NAN), order_key_bits(f32::INFINITY));
        assert!(order_key_bits(-1.0) == order_key_bits(0.0), "负误差钳到 0");
        // 倒序键反转全序
        assert!(invert_order_key(3.0) < invert_order_key(1.0));
        assert_eq!(invert_order_key(0.0), u64::MAX);
    }

    /// 容量上界防御：页号超页表的请求在键合并时不 panic（调用方页表
    /// 一致性由注册期保证，这里锁防御行为）。
    #[test]
    fn residency_unknown_page_bytes_default() {
        let mut mgr = ResidencyManager::new();
        let ops = mgr.tick(u64::MAX, 8, &[(7, 1.0)], &|_| 64);
        assert_eq!(ops.upload, vec![7]);
        assert_eq!(mgr.resident_bytes(), 64);
    }
}
