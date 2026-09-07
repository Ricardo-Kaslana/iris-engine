//! 地形（R51）：heightmap 地形——程序化高度场 + 分块网格（3 级距离 LOD）
//! + 自动碰撞拼块，写实路线「复杂地形」的正式解。
//!
//! # 组成
//!
//! - [`Heightmap`]：规则网格顶点高度（行主序 f32），[`generate_hills`]
//!   多频正弦叠加生成器（**无 RNG**——splitmix64 哈希派生频率方向 / 相位，
//!   同 seed 逐位一致，确定性文化见 AGENTS.md）；
//!   [`Heightmap::height_at`] 双线性插值查询（域外 `None`），
//!   [`Heightmap::normal_at`] 中心差分法线；
//! - [`chunk_mesh`]：把高度场切成 chunk（[`TerrainOptions::chunk_cells`]
//!   见方，默认 32 cell）按 LOD 级烘焙顶点（位置 / 法线 / UV），返回
//!   可直接入 [`MeshStore`](crate::mesh::MeshStore) 的
//!   [`MeshData`](crate::mesh::MeshData)——标准管线零新 shader；
//! - [`LodMesh3`]：3 级距离 LOD 组件（[`LodMesh`](crate::mesh::LodMesh)
//!   的一般化），渲染收集期按「chunk 原点到相机 eye 的欧氏距离」在
//!   3 个网格句柄间选择（与 R30 同口径，同样**无滞回**）；
//! - [`build_terrain`]：一次性把地形铺进世界——根实体挂 [`Terrain`]
//!   组件（高度场 + 参数 + chunk / 碰撞体实体清单），每个 chunk 一个
//!   实体（Transform 角点 + LodMesh3 + 材质），碰撞拼块见下。
//!
//! # 碰撞 = 自动生成粗 AABB 拼块（零 API 扩展的集成策略）
//!
//! 每 [`TerrainOptions::collider_stride`]² 个 cell 一个
//! [`Collider::static_box`](iris_core::physics::Collider::static_box)
//! 静态体（只有 Collider 无 RigidBody = 静态，组件缺席语义见
//! [`iris_core::physics`] 模块文档）：顶面 =
//! 该块 4×4 角点**最高角**的高度、向下延伸到全场地形最低点以下——
//! 相邻块顶面高差即「台阶」。这样角色控制器（
//! [`move_and_slide`](iris_core::character::move_and_slide) 的世界静态体
//! 收集）、射线查询与 R49 导航图（[`NavGrid::build`](iris_core::nav::NavGrid::build)
//! 的 Box 表面解析吃盒顶）全部**零改动**可用：
//! - 台阶语义：相邻块顶面高差 ≤ `step_height`（角色 0.3 / 导航 0.3，
//!   两者同值同语义）→ 可迈 / 可连通；> 即墙；
//! - 衔接域：导航 `cell_size` 建议取地形 `cell_size × collider_stride`
//!   的整数倍（每导航格完整落在拼块内 → 表面高度解析无亚格歧义）；
//!   且 nav 边界须**外扩于地形域**（哪怕 1～2 圈格）——R49 顶面雕刻
//!   BFS 的种子是「未被任何盒覆盖的隐式地面格」，被拼块全覆盖的域
//!   没有种子，整图会判为不可走（实测教训，单测
//!   `navgrid_paths_over_terrain_ridge` 锁定该口径）；
//! - 规模上限：拼块数 = cell 数 / stride²。典型 64×64 cell stride 1 =
//!   4096 盒（世界查询与控制器每次线性扫过——v1 无空间索引，这是
//!   「小地形直接全量」的口径）；128×128 = 16k 盒是 broadphase / 扫掠
//!   仍能承受的极限，再大的场景请调大 `collider_stride` 或分区按需
//!   建造（v1 不做动态流式装卸，见诚实限制）。
//!
//! # LOD 接缝不变量（无 skirt 的做法）
//!
//! 分块 + 距离 LOD 的经典裂缝问题：相邻 chunk 选到不同级时共享边上的
//! 顶点集不一致 ⇒ 裂缝。本实现**不做裙边（skirt）**，改用更强的采样
//! 对齐不变量：
//!
//! > **每级 LOD 的 chunk 边界线（局部 i = 0 / n 与 j = 0 / n 四条线）
//! > 恒按全分辨率采样**；只有内部按 `stride = 1 / 2 / 4` 抽行抽列。
//!
//! 于是任意两级相邻 chunk 的共享边多边形线**顶点集完全一致**——位置
//! 的 y、法线、UV 都取自相同的全局格点索引（同一高度数组元素与同一
//! 差分表达式）⇒ **逐位一致**；x/z 因「角点原点 + 小整数 × cell」烘焙，
//! `cell_size` 为 2 的幂（0.25 / 0.5 / 1）时同样逐位一致，否则 ≤ 1 ULP
//! （亚毫米，且不涉裂缝的垂直轴）。裂缝的语义轴是垂直方向，其量恒为 0，
//! 因此相邻 LOD 差**无需 ≤ 1 的限制**（规范给的「差 ≤ 1 + 抽样式对齐」
//! 两条件被该不变量吸收——见提交 body 的偏离说明）。
//!
//! 代价：粗级 chunk 在边界内侧出现长条三角形带与 T 型接点（接缝折线
//! 本身两侧共点，水密不漏；T 型接点只有法线插值的轻微视觉差，v1 接受）。
//!
//! # 顶点属性口径
//!
//! - 位置：chunk **局部**（实体原点在 chunk 角点世界位置，y 恒为 0 ⇒
//!   顶点 y = 世界高度本体，接缝不变量的垂直轴因此逐位一致）；
//! - 法线：格点中心差分（边界单边差分），顶点焊接平滑；
//! - UV：世界 XZ ÷ [`TerrainOptions::uv_tile`]（米/纹理重复）——由
//!   全局格点索引推导 ⇒ 跨 chunk 采样无缝（纹理 Repeat 寻址，见
//!   [`CpuTexture`] 上传路径）；
//! - 索引 u32。
//!
//! # 材质
//!
//! [`grass_textures`]：程序化草地法线 / ORM / 基色三件套（周期性值噪声
//! 高度场 → 中心差分法线 + AO / 粗糙度，splitmix64 哈希、同输入逐位
//! 一致，口径照 [`CpuTexture::brick_normal_orm`]）。**斜率染色 v1 不做**
//! （单纹理平铺——草地 / 岩石混合需要逐顶点色或着色器分支，路线图）。
//!
//! # 诚实限制（v1）
//!
//! 1. **碰撞是台阶不是光滑坡面**：视觉网格连续，碰撞盒顶按 cell 阶梯
//!    ——角色 / 导航在坡面上走的是「≤ 每 cell 高差的小台阶」，单 cell
//!    高差超过 `step_height` 的陡面会被挡（写实陡崖的正确行为，缓坡的
//!    可接受近似）；
//! 2. **无 skirt**：靠上述边界全采样不变量防水密（论证见上），粗级
//!    chunk 边界带几何退化（长条三角形）在掠射角可见轻微 T 型接点差；
//! 3. **无滞回**：chunk 距离在切换阈值附近抖动时逐级来回切（与 R30
//!    [`LodMesh`](crate::mesh::LodMesh) 同限制）；
//! 4. **无地形流式装卸**：全部 chunk 常驻；`generate_hills` 域外查询
//!    返回 `None`，游戏侧边界处理自理；
//! 5. **无坡度材质混合**：单材质单纹理平铺（见「材质」节）；
//! 6. **LOD 级数固定 3**（stride 1 / 2 / 4 嵌套，`switch_distances`
//!    两级阈值）——更多级数待有场景需求再泛化。

use glam::Vec3;
use iris_core::prelude::{Collider, ColliderShape, Entity, Transform, World};

use crate::material::MaterialHandle;
use crate::mesh::{MeshData, MeshHandle, MeshStore, Vertex};
use crate::texture::CpuTexture;

/// LOD 级数（stride 嵌套表长度）。
pub const TERRAIN_LOD_LEVELS: usize = 3;
/// 各级抽行/抽列步长（0 = 全分辨率，1 = ½，2 = ¼）。
pub const TERRAIN_LOD_STRIDES: [usize; TERRAIN_LOD_LEVELS] = [1, 2, 4];
/// 碰撞拼块向下的额外深度余量（米）：盒底 = 全场最低高度 − 本值。
/// 保证平地 cell（四角等高）不退化为零厚度 AABB，且落体从侧上方
/// 入射时拼块侧面有足够高度可供阻挡。
pub const COLLIDER_DEPTH_MARGIN: f32 = 0.25;

/// splitmix64 终结函数（与 `particles.rs::mix64` / `texture.rs` 同源）：
/// 无状态哈希流，确定性文化的标准随机性替身。
fn mix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// 高度场：规则网格上的顶点高度（行主序，`heights[j * width + i]`，
/// i = 世界 X 方向列、j = Z 方向行）。局部坐标域 `[0, span_x] × [0, span_z]`
/// （米），世界摆放由 [`Terrain::origin`] 平移。
#[derive(Clone, Debug, PartialEq)]
pub struct Heightmap {
    /// 顶点列数（cell 数 + 1）。
    pub width: usize,
    /// 顶点行数（cell 数 + 1）。
    pub depth: usize,
    /// 格距（米/边）。
    pub cell_size: f32,
    /// 顶点高度（米），行主序，长度必须为 `width × depth`。
    pub heights: Vec<f32>,
}

impl Heightmap {
    /// 构造：断言宽高 ≥ 2、高度数组长度匹配、cell_size 正有限。
    pub fn new(width: usize, depth: usize, cell_size: f32, heights: Vec<f32>) -> Self {
        assert!(
            width >= 2 && depth >= 2,
            "heightmap 至少 2×2 顶点，实际 {width}×{depth}"
        );
        assert!(
            cell_size.is_finite() && cell_size > 0.0,
            "cell_size 必须为正的有限数，实际 {cell_size}"
        );
        assert_eq!(
            heights.len(),
            width * depth,
            "heights 长度须为 width × depth"
        );
        Self {
            width,
            depth,
            cell_size,
            heights,
        }
    }

    /// X 方向 cell 数。
    pub fn cell_cols(&self) -> usize {
        self.width - 1
    }

    /// Z 方向 cell 数。
    pub fn cell_rows(&self) -> usize {
        self.depth - 1
    }

    /// X 方向跨度（米）。
    pub fn span_x(&self) -> f32 {
        self.cell_cols() as f32 * self.cell_size
    }

    /// Z 方向跨度（米）。
    pub fn span_z(&self) -> f32 {
        self.cell_rows() as f32 * self.cell_size
    }

    /// 格点索引读高（越界 panic——网格几何推导处的防御断言）。
    pub fn vertex_height(&self, i: usize, j: usize) -> f32 {
        assert!(
            i < self.width && j < self.depth,
            "heightmap 格点 ({i}, {j}) 越界 {}×{}",
            self.width,
            self.depth
        );
        self.heights[j * self.width + i]
    }

    /// 双线性插值高度查询（局部坐标，米）。域外（含 NaN / 恰越界）返回
    /// `None`；恰在边界上按钳位后的格点区间正常插值（权重 0/1 处即
    /// 格点值本身）。
    pub fn height_at(&self, x: f32, z: f32) -> Option<f32> {
        let (span_x, span_z) = (self.span_x(), self.span_z());
        if !x.is_finite() || !z.is_finite() || x < 0.0 || x > span_x || z < 0.0 || z > span_z {
            return None;
        }
        // 单元坐标 = 坐标 / 格距：先除后减 i0（域内除法可能给 3.9999996
        // 这类略小于整数的值，floor 后 i 落在正确格点、frac 权重 ≈ 1，
        // 结果按双线性连续收敛到格点值，不需要病态修正）。
        let gx = x / self.cell_size;
        let gz = z / self.cell_size;
        let i0 = (gx.floor() as usize).min(self.cell_cols() - 1);
        let j0 = (gz.floor() as usize).min(self.cell_rows() - 1);
        let tx = gx - i0 as f32;
        let tz = gz - j0 as f32;
        let h00 = self.vertex_height(i0, j0);
        let h10 = self.vertex_height(i0 + 1, j0);
        let h01 = self.vertex_height(i0, j0 + 1);
        let h11 = self.vertex_height(i0 + 1, j0 + 1);
        // 与 mesh 烘焙同一算式家族：lerp(a, b, t) = a + (b − a)·t
        let a = h00 + (h10 - h00) * tx;
        let b = h01 + (h11 - h01) * tx;
        Some(a + (b - a) * tz)
    }

    /// 中心差分法线（局部坐标，单位长、y > 0）。步长 = 1 个 cell；
    /// 贴域边界时退化为可用的单边差分。域外返回 `None`。
    ///
    /// 数学：`n = normalize(−∂h/∂x, 1, −∂h/∂z)`——高度场隐式曲面
    /// `F(x, y, z) = h(x, z) − y = 0` 的梯度 `(∂h/∂x, −1, ∂h/∂z)` 取负
    /// 归一（朝上侧）。
    pub fn normal_at(&self, x: f32, z: f32) -> Option<Vec3> {
        let (span_x, span_z) = (self.span_x(), self.span_z());
        if !x.is_finite() || !z.is_finite() || x < 0.0 || x > span_x || z < 0.0 || z > span_z {
            return None;
        }
        let e = self.cell_size;
        let (xa, xb) = if x - e < 0.0 {
            (x, (x + e).min(self.span_x()))
        } else if x + e > self.span_x() {
            ((x - e).max(0.0), x)
        } else {
            (x - e, x + e)
        };
        let (za, zb) = if z - e < 0.0 {
            (z, (z + e).min(self.span_z()))
        } else if z + e > self.span_z() {
            ((z - e).max(0.0), z)
        } else {
            (z - e, z + e)
        };
        let dhdx = (self.height_at(xb, z)? - self.height_at(xa, z)?) / (xb - xa);
        let dhdz = (self.height_at(x, zb)? - self.height_at(x, za)?) / (zb - za);
        let n = Vec3::new(-dhdx, 1.0, -dhdz);
        Some(n / n.length())
    }

    /// 格点法线（边界单边差分）——网格烘焙专用。同一全局格点在任何
    /// chunk 任何 LOD 下读本函数 ⇒ 逐位一致（接缝不变量的法线轴）。
    pub fn vertex_normal(&self, i: usize, j: usize) -> Vec3 {
        let (imax, jmax) = (self.width - 1, self.depth - 1);
        let (i0, i1) = if i == 0 {
            (0, 1)
        } else if i == imax {
            (imax - 1, imax)
        } else {
            (i - 1, i + 1)
        };
        let (j0, j1) = if j == 0 {
            (0, 1)
        } else if j == jmax {
            (jmax - 1, jmax)
        } else {
            (j - 1, j + 1)
        };
        let dhdx = (self.vertex_height(i1, j) - self.vertex_height(i0, j))
            / ((i1 - i0) as f32 * self.cell_size);
        let dhdz = (self.vertex_height(i, j1) - self.vertex_height(i, j0))
            / ((j1 - j0) as f32 * self.cell_size);
        let n = Vec3::new(-dhdx, 1.0, -dhdz);
        let len = n.length();
        if len == 0.0 || !len.is_finite() {
            Vec3::Y
        } else {
            n / len
        }
    }
}

/// 丘陵地形生成器：三组方向正弦叠加（频率 / 方向 / 相位由 seed 经
/// splitmix64 派生，**无 RNG 无依赖**）。归一化叠加 ∈ [−1, 1]，输出
/// `height = base + amplitude × 叠加值`，即值域 `[base − amplitude,
/// base + amplitude]`（闭区间——三频同相取等，测试可据此断言）。
///
/// 波长固定 48 / 24 / 12 m（振幅权重 1 : 0.5 : 0.25）：丘陵起伏的
/// 「写实量级」，seed 只改方向与相位（地形变体），不改尺度。
/// 确定性：同参数两次调用逐位一致（`to_bits` 测试锁定）。
pub fn generate_hills(
    width: usize,
    depth: usize,
    cell_size: f32,
    seed: u64,
    base: f32,
    amplitude: f32,
) -> Heightmap {
    assert!(
        base.is_finite() && amplitude.is_finite() && amplitude >= 0.0,
        "generate_hills 要求 base 有限、amplitude 非负有限，实际 {base} / {amplitude}"
    );
    const OCTAVES: usize = 3;
    const WEIGHT_SUM: f32 = 1.0 + 0.5 + 0.25;
    // 每倍频程：传播方向角 θ、相位 φ、角波数 k（波长 48 / 24 / 12）
    let mut oct = [(0.0f32, 0.0f32, 0.0f32, 0.0f32); OCTAVES]; // (kx, kz, phase, weight)
    let weights = [1.0f32, 0.5, 0.25];
    let mut hash = seed;
    for o in 0..OCTAVES {
        hash = mix64(hash);
        let theta = (hash >> 40) as f32 / (1u32 << 24) as f32 * std::f32::consts::TAU;
        hash = mix64(hash);
        let phase = (hash >> 40) as f32 / (1u32 << 24) as f32 * std::f32::consts::TAU;
        let wavelength = 48.0f32 / (1u32 << o) as f32;
        let k = std::f32::consts::TAU / wavelength;
        oct[o] = (theta.cos() * k, theta.sin() * k, phase, weights[o]);
    }
    let hm_width = width.max(2);
    let hm_depth = depth.max(2);
    let mut heights = Vec::with_capacity(hm_width * hm_depth);
    for j in 0..hm_depth {
        let z = j as f32 * cell_size;
        for i in 0..hm_width {
            let x = i as f32 * cell_size;
            let mut acc = 0.0f32;
            for &(kx, kz, phase, w) in &oct {
                acc += (kx * x + kz * z + phase).sin() * w;
            }
            heights.push(base + amplitude * (acc / WEIGHT_SUM));
        }
    }
    Heightmap::new(hm_width, hm_depth, cell_size, heights)
}

/// 3 级距离 LOD 组件（R51）：同一实体的三个网格句柄 + 两级切换距离，
/// [`LodMesh`](crate::mesh::LodMesh)（2 级）的一般化。挂载时**替换**
/// `MeshHandle` 参与收集（收集期优先级：本组件 > LodMesh > MeshHandle），
/// 距离 = 实体原点到相机 eye 的欧氏距离，阈值边界归近侧（近处宁用
/// 高模，与 R30 同口径）。地形 chunk 的 LOD 落点即本组件；v1 固定
/// 3 级、与 [`TERRAIN_LOD_STRIDES`] 的嵌套采样配套。
#[derive(Clone, Copy, Debug, PartialEq, iris_core::Component)]
pub struct LodMesh3 {
    /// 三级网格句柄（0 = 最近最细）。
    pub levels: [MeshHandle; TERRAIN_LOD_LEVELS],
    /// 切换距离（米）：`dist ≤ [0]` 用级 0，`≤ [1]` 用级 1，否则级 2。
    pub switch_distances: [f32; TERRAIN_LOD_LEVELS - 1],
}

impl LodMesh3 {
    /// 距离选择纯函数（口径与 `LodMesh::select` 一致：欧氏距离、
    /// 边界归近侧、与观察方向无关）。
    pub fn select(&self, eye: Vec3, origin: Vec3) -> MeshHandle {
        let dist = eye.distance(origin);
        if dist <= self.switch_distances[0] {
            self.levels[0]
        } else if dist <= self.switch_distances[1] {
            self.levels[1]
        } else {
            self.levels[2]
        }
    }
}

/// 地形组件（挂根实体）：高度场本体 + 世界摆放 + chunk / 碰撞拼块
/// 实体清单。游戏侧经 [`Terrain::height_at_world`] 做贴地查询
/// （投射物、放置物、脚步声检测等），经 [`terrain_statics`] 取碰撞
/// 拼块的 `(位置, 形状)` 列表喂 `NavGrid::build`。
#[derive(Clone, Debug, iris_core::Component)]
pub struct Terrain {
    /// 高度场（局部域 `[0, span] × [0, span]`）。
    pub heightmap: Heightmap,
    /// 高度场局部原点 → 世界坐标的平移（y 分量 = 地形整体垫高）。
    pub origin: Vec3,
    /// 每 chunk 的 cell 边长（顶点 = chunk_cells + 1）。
    pub chunk_cells: usize,
    /// UV 平铺：每 `uv_tile` 米一个纹理重复（世界 XZ）。
    pub uv_tile: f32,
    /// LOD 切换距离（两级阈值，米）——构建时已抄写到每个 chunk 的
    /// [`LodMesh3`]，此处保留供查询 / 重建。
    pub switch_distances: [f32; TERRAIN_LOD_LEVELS - 1],
    /// chunk 实体（构建序：Z 行 × X 列行主序）。
    pub chunks: Vec<Entity>,
    /// 碰撞拼块实体（构建序同上，仅 Collider 静态体，不渲染）。
    pub colliders: Vec<Entity>,
}

impl Terrain {
    /// 世界坐标双线性高度查询（含 [`Terrain::origin`] 平移的 y 垫高）。
    /// 局部域外返回 `None`（v1 限制：不做域外延拓）。
    pub fn height_at_world(&self, x: f32, z: f32) -> Option<f32> {
        let local = self
            .heightmap
            .height_at(x - self.origin.x, z - self.origin.z)?;
        Some(local + self.origin.y)
    }
}

/// 建造参数。
#[derive(Clone, Debug)]
pub struct TerrainOptions {
    /// 地形根实体位置 = 高度场局部原点（y = 整体垫高）。
    pub origin: Vec3,
    /// 每 chunk 的 cell 边长：须为 4 的正倍数（LOD2 步长 4 整除）、
    /// 且整除 heightmap 的 cell 数。默认 32（顶点 33×33）。
    pub chunk_cells: usize,
    /// 碰撞拼块的 cell 合并边长（1 = 每 cell 一盒；2 = 每 2×2 一盒）。
    pub collider_stride: usize,
    /// UV 平铺（米/纹理重复）。
    pub uv_tile: f32,
    /// LOD 两级切换距离（米），要求 `s[0] < s[1]` 且均为正的有限数。
    pub switch_distances: [f32; TERRAIN_LOD_LEVELS - 1],
}

impl Default for TerrainOptions {
    fn default() -> Self {
        Self {
            origin: Vec3::ZERO,
            chunk_cells: 32,
            collider_stride: 1,
            uv_tile: 4.0,
            switch_distances: [16.0, 40.0],
        }
    }
}

/// [`build_terrain`] 的产出摘要。
#[derive(Clone, Debug)]
pub struct TerrainBuild {
    /// 根实体（挂 [`Terrain`] 组件）。
    pub root: Entity,
    /// chunk 实体（顺序与 `Terrain::chunks` 一致）。
    pub chunks: Vec<Entity>,
    /// 碰撞拼块实体（顺序与 `Terrain::colliders` 一致）。
    pub colliders: Vec<Entity>,
    /// 每级每 chunk 的顶点数（chunk 网格同构，逐级递减）。
    pub lod_vertices: [usize; TERRAIN_LOD_LEVELS],
    /// 每级每 chunk 的三角形数（`FrameStats::triangles_drawn` 对账口径）。
    pub lod_triangles: [u32; TERRAIN_LOD_LEVELS],
    /// chunk 列数 × 行数（`chunks.len() == chunks_x * chunks_z`）。
    pub chunks_x: usize,
    pub chunks_z: usize,
}

/// 把高度场铺进世界（`&mut World` 一次性构建，口径照
/// `spawn_ragdoll_kinematic`）：
///
/// 1. 每 chunk × 每 LOD 烘焙 [`chunk_mesh`] 入 `MeshStore`（懒 GPU
///    上传——远处从不用到的细模不占显存）；
/// 2. 每 chunk 一个实体：`Transform`（角点世界位置，y = origin.y）+
///    [`LodMesh3`] + 材质句柄——**收集期按距离选级**（见 [`LodMesh3`]），
///    `FrameStats::triangles_drawn` 天然反映 LOD 生效；
/// 3. 每 `collider_stride²` cell 一个 [`Collider::static_box`] 实体
///    （无 MeshHandle / MaterialHandle ⇒ 不进渲染收集，无需
///    `Visibility` 隐藏）；
/// 4. 根实体挂 [`Terrain`]（组件 + 清单），返回 [`TerrainBuild`]。
///
/// chunk 实体的 `GlobalTransform` 由游戏侧层级传播补齐
/// （`HierarchyPlugin` / [`propagate_transforms`](iris_core::hierarchy::propagate_transforms)），
/// 本函数只写 `Transform`。
///
/// # Panics
/// 参数不满足「chunk_cells 为 4 的正倍数且整除 cell 数、collider_stride
/// ∈ 1..=4 且整除 cell 数、切换距离正有限递增、高度场无 NaN」（中文断言）。
pub fn build_terrain(
    world: &mut World,
    heightmap: Heightmap,
    material: MaterialHandle,
    options: &TerrainOptions,
) -> TerrainBuild {
    let (cell_cols, cell_rows) = (heightmap.cell_cols(), heightmap.cell_rows());
    let n = options.chunk_cells;
    assert!(
        n >= 4 && n % TERRAIN_LOD_STRIDES[TERRAIN_LOD_LEVELS - 1] == 0,
        "chunk_cells {n} 须 ≥ 4 且被最大 LOD 步长 {} 整除（格点边界对齐不变量前提）",
        TERRAIN_LOD_STRIDES[TERRAIN_LOD_LEVELS - 1]
    );
    assert!(
        cell_cols % n == 0 && cell_rows % n == 0,
        "heightmap cell 数 ({cell_cols}, {cell_rows}) 须被 chunk_cells {n} 整除（v1 不做残缺 chunk）"
    );
    let st = options.collider_stride;
    assert!((1..=4).contains(&st), "collider_stride {st} 须在 1..=4");
    assert!(
        cell_cols % st == 0 && cell_rows % st == 0,
        "heightmap cell 数 ({cell_cols}, {cell_rows}) 须被 collider_stride {st} 整除"
    );
    let [s0, s1] = options.switch_distances;
    assert!(
        s0.is_finite() && s1.is_finite() && s0 > 0.0 && s1 > s0,
        "switch_distances {:?} 须为正的有限数且严格递增",
        options.switch_distances
    );
    assert!(
        options.uv_tile.is_finite() && options.uv_tile > 0.0,
        "uv_tile 须为正的有限数，实际 {}",
        options.uv_tile
    );
    assert!(
        heightmap
            .heights
            .iter()
            .all(|h| h.is_finite() && *h >= -1.0e8 && *h <= 1.0e8),
        "heightmap 含 NaN / ±Inf / 病态量级高度，拒绝建网"
    );

    let cell = heightmap.cell_size;
    let origin = options.origin;
    let chunks_x = cell_cols / n;
    let chunks_z = cell_rows / n;

    // 全场最低高度（拼块盒底基准）：先扫一遍，确定性 = min 折叠序固定
    let mut global_min = f32::INFINITY;
    for &h in &heightmap.heights {
        global_min = global_min.min(h);
    }

    let mut lod_vertices = [0usize; TERRAIN_LOD_LEVELS];
    let mut lod_triangles = [0u32; TERRAIN_LOD_LEVELS];
    let mut chunk_entities = Vec::with_capacity(chunks_x * chunks_z);
    for cz in 0..chunks_z {
        for cx in 0..chunks_x {
            let mut levels = [MeshHandle(0); TERRAIN_LOD_LEVELS]; // 占位，三级逐槽覆写
            for lod in 0..TERRAIN_LOD_LEVELS {
                let mesh = chunk_mesh(&heightmap, cx, cz, n, lod, options.uv_tile);
                lod_vertices[lod] = mesh.vertices.len();
                lod_triangles[lod] = (mesh.indices.len() / 3) as u32;
                levels[lod] = world.resource_mut::<MeshStore>().add(mesh);
            }
            let gx0 = cx * n;
            let gz0 = cz * n;
            let entity = world.spawn((
                Transform::from_xyz(
                    origin.x + gx0 as f32 * cell,
                    origin.y,
                    origin.z + gz0 as f32 * cell,
                ),
                LodMesh3 {
                    levels,
                    switch_distances: options.switch_distances,
                },
                material,
            ));
            chunk_entities.push(entity);
        }
    }
    // 各级网格同构（chunk 尺寸一致），顶点 / 三角形数逐级递减断言
    assert!(
        lod_triangles[0] > lod_triangles[1] && lod_triangles[1] > lod_triangles[2],
        "LOD 三角形数须逐级递减：{lod_triangles:?}"
    );

    // 碰撞拼块：每 stride² cell 一盒，顶面 = 块内最高角点，底 = 全场最低 − 余量
    let mut colliders = Vec::with_capacity(cell_cols / st * cell_rows / st);
    let half_xz = st as f32 * cell * 0.5;
    for j0 in (0..cell_rows).step_by(st) {
        for i0 in (0..cell_cols).step_by(st) {
            let mut top = f32::NEG_INFINITY;
            for dj in 0..=st {
                for di in 0..=st {
                    top = top.max(heightmap.vertex_height(i0 + di, j0 + dj));
                }
            }
            let bottom = global_min - COLLIDER_DEPTH_MARGIN;
            let half_y = (top - bottom) * 0.5;
            let center = Vec3::new(
                origin.x + (i0 as f32 + st as f32 * 0.5) * cell,
                origin.y + top - half_y,
                origin.z + (j0 as f32 + st as f32 * 0.5) * cell,
            );
            let entity = world.spawn((
                Transform::from_translation(center),
                Collider::static_box(Vec3::new(half_xz, half_y, half_xz)),
            ));
            colliders.push(entity);
        }
    }

    let terrain = Terrain {
        heightmap,
        origin,
        chunk_cells: n,
        uv_tile: options.uv_tile,
        switch_distances: options.switch_distances,
        chunks: chunk_entities.clone(),
        colliders: colliders.clone(),
    };
    let root = world.spawn((Transform::from_translation(origin), terrain));
    TerrainBuild {
        root,
        chunks: chunk_entities,
        colliders,
        lod_vertices,
        lod_triangles,
        chunks_x,
        chunks_z,
    }
}

/// 该 LOD 级、局部格点 `(i, j)`（0..=n）是否采样（接缝不变量定义）。
fn lod_vertex_present(i: usize, j: usize, n: usize, lod: usize) -> bool {
    if i == 0 || i == n || j == 0 || j == n {
        return true; // chunk 边界线全分辨率（共享边两侧一致）
    }
    let s = TERRAIN_LOD_STRIDES[lod];
    i % s == 0 && j % s == 0
}

/// 该行在给定 LOD 下的列采样表（升序；恒含 0 与 n）——与
/// [`lod_vertex_present`] 严格互逆（拉链三角化的前提）。
fn lod_cols(j: usize, n: usize, lod: usize) -> Vec<usize> {
    if j == 0 || j == n {
        return (0..=n).collect(); // 边界行全分辨率
    }
    let s = TERRAIN_LOD_STRIDES[lod];
    if j % s == 0 {
        (0..=n).step_by(s).collect() // 内部：落在抽行格上的行取抽列格
    } else {
        vec![0, n] // 其余行只含两条边界列（边界线全采样的垂直向对应物）
    }
}

/// 烘焙单个 chunk 的网格（纯函数——脱离世界可单测）。顶点为 chunk
/// 局部：`x = i·cell_size`、`y = 世界高度本体`、`z = j·cell_size`
/// （实体 `Transform` 放在 chunk 角点 ⇒ 顶点 y 无平移误差，接缝
/// 不变量的垂直轴逐位一致）。UV 由**全局**格点索引推导（跨 chunk
/// 平铺无缝且共享顶点逐位一致）。
///
/// 三角化为两行采样列表的「拉链」条带（zipper triangulation）：行带
/// 之间逐区间推进列指针，每个新区间发一个三角形（同列时发一个 quad
/// 的两半），任意行对（含边界全采样行与内部抽行）都成立；朝向由
/// [`push_tri`] 按几何法线 y 分量现场裁决（高度场单值 ⇒ 良定义，
/// 退化 = 0 时保持给定序——确定性规则）。
pub fn chunk_mesh(
    heightmap: &Heightmap,
    chunk_col: usize,
    chunk_row: usize,
    chunk_cells: usize,
    lod: usize,
    uv_tile: f32,
) -> MeshData {
    assert!(
        lod < TERRAIN_LOD_LEVELS,
        "lod {lod} 超出 {TERRAIN_LOD_LEVELS} 级"
    );
    let n = chunk_cells;
    assert!(
        n % TERRAIN_LOD_STRIDES[lod] == 0,
        "chunk_cells {n} 须被 LOD{lod} 步长 {} 整除（边界格点须落在抽样格上）",
        TERRAIN_LOD_STRIDES[lod]
    );
    let cell = heightmap.cell_size;
    let gx0 = chunk_col * n;
    let gz0 = chunk_row * n;
    assert!(
        gx0 + n < heightmap.width && gz0 + n < heightmap.depth,
        "chunk ({chunk_col}, {chunk_row}) 越出 heightmap {}×{}",
        heightmap.width,
        heightmap.depth
    );
    let mut data = MeshData::default();
    let mut table = vec![u32::MAX; (n + 1) * (n + 1)];
    for j in 0..=n {
        for i in 0..=n {
            if !lod_vertex_present(i, j, n, lod) {
                continue;
            }
            let (gi, gj) = (gx0 + i, gz0 + j);
            let h = heightmap.vertex_height(gi, gj);
            let normal = heightmap.vertex_normal(gi, gj);
            let wx = gi as f32 * cell;
            let wz = gj as f32 * cell;
            let index = data.vertices.len() as u32;
            data.vertices.push(Vertex {
                position: [i as f32 * cell, h, j as f32 * cell],
                normal: [normal.x, normal.y, normal.z],
                uv: [wx / uv_tile, wz / uv_tile],
            });
            table[j * (n + 1) + i] = index;
        }
    }
    // 行带拉链三角化（zipper）：两行列指针各锚在已消费列，每次推进
    // 「下一列较小」的一侧并发三角形（同列先 A 后 B ⇒ quad 对角一致）
    for j in 0..n {
        let cols_a = lod_cols(j, n, lod);
        let cols_b = lod_cols(j + 1, n, lod);
        let mut ia = 0usize;
        let mut ib = 0usize;
        loop {
            let na = (ia + 1 < cols_a.len()).then(|| cols_a[ia + 1]);
            let nb = (ib + 1 < cols_b.len()).then(|| cols_b[ib + 1]);
            match (na, nb) {
                (None, None) => break,
                (Some(a), None) => {
                    ia += 1;
                    push_tri(
                        &mut data,
                        &table,
                        n,
                        (cols_a[ia - 1], j),
                        (a, j),
                        (cols_b[ib], j + 1),
                    );
                }
                (None, Some(b)) => {
                    ib += 1;
                    push_tri(
                        &mut data,
                        &table,
                        n,
                        (cols_b[ib - 1], j + 1),
                        (b, j + 1),
                        (cols_a[ia], j),
                    );
                }
                (Some(a), Some(b)) => {
                    if a <= b {
                        ia += 1;
                        push_tri(
                            &mut data,
                            &table,
                            n,
                            (cols_a[ia - 1], j),
                            (a, j),
                            (cols_b[ib], j + 1),
                        );
                    } else {
                        ib += 1;
                        push_tri(
                            &mut data,
                            &table,
                            n,
                            (cols_b[ib - 1], j + 1),
                            (b, j + 1),
                            (cols_a[ia], j),
                        );
                    }
                }
            }
        }
    }
    data
}

/// 发一个三角形：三个顶点按 (列, 行) 查采样表取句柄，几何法线 y
/// 分量现场裁决环绕向（高度场单值 ⇒ 符号良定义；y = 0 的退化三点
/// 共线 ⇒ 发序保留、面积为 0 无害）。
fn push_tri(
    data: &mut MeshData,
    table: &[u32],
    n: usize,
    a: (usize, usize),
    b: (usize, usize),
    c: (usize, usize),
) {
    let vid = |p: (usize, usize)| -> u32 {
        let v = table[p.1 * (n + 1) + p.0];
        assert!(v != u32::MAX, "三角化引用了未采样的格点 {:?}", p);
        v
    };
    let (p, q, r) = (vid(a), vid(b), vid(c));
    let pos = |v: u32| Vec3::from(data.vertices[v as usize].position);
    let (pp, pq, pr) = (pos(p), pos(q), pos(r));
    if (pq - pp).cross(pr - pp).y >= 0.0 {
        data.indices.extend_from_slice(&[p, q, r]);
    } else {
        data.indices.extend_from_slice(&[p, r, q]);
    }
}

/// 程序化草地三件套（基色 sRGB / 法线 / ORM 线性，R51）：周期性值噪声
/// 高度场（splitmix64 哈希格点 + smoothstep 双线性）→ 中心差分切线空间
/// 法线；AO / 粗糙度由噪声与坡度驱动，金属度恒 0。周期 = 哈希格点
/// 按 `k` 取模 ⇒ **纹理跨接缝无缝**（uv > 1 平铺即草地大面）。
///
/// 口径照 [`CpuTexture::brick_normal_orm`]：纯函数、同输入逐位一致、
/// 无状态无随机源。
pub fn grass_textures(size: u32) -> GrassTextures {
    assert!(size >= 8, "草地纹理边长至少 8 像素，实际 {size}");
    let lattice = |ix: i64, iy: i64, k: i64| -> f32 {
        let wrapped = (((ix % k) + k) % k) as u64 * 4096 + (((iy % k) + k) % k) as u64;
        (mix64(wrapped ^ 0x517c_c1b7_2722_0a95) >> 40) as f32 / (1u32 << 24) as f32
    };
    let smooth = |t: f32| t * t * (3.0 - 2.0 * t);
    // 周期值噪声（格点 k×k，双线性 smoothstep）
    let vnoise = |u: f32, v: f32, k: i64| -> f32 {
        let x = u * k as f32;
        let y = v * k as f32;
        let ix = x.floor() as i64;
        let iy = y.floor() as i64;
        let fx = smooth(x - ix as f32);
        let fy = smooth(y - iy as f32);
        let a = lattice(ix, iy, k);
        let b = lattice(ix + 1, iy, k);
        let c = lattice(ix, iy + 1, k);
        let d = lattice(ix + 1, iy + 1, k);
        let top = a + (b - a) * fx;
        let bot = c + (d - c) * fx;
        top + (bot - top) * fy
    };
    // 两倍频程高度（8 格细粒 + 3 格大斑）
    let height = |u: f32, v: f32| -> f32 {
        let h = 0.65 * vnoise(u, v, 8) + 0.35 * vnoise(u, v, 3);
        h.clamp(0.0, 1.0)
    };
    let eps = 1.0 / size as f32;
    let bump = 1.6f32;
    let mut normal = Vec::with_capacity((size * size * 4) as usize);
    let mut orm = Vec::with_capacity((size * size * 4) as usize);
    let mut albedo = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let u = (x as f32 + 0.5) / size as f32;
            let v = (y as f32 + 0.5) / size as f32;
            let dhdu =
                (height((u + eps) % 1.0, v) - height((u - eps + 1.0) % 1.0, v)) / (2.0 * eps);
            let dhdv =
                (height(u, (v + eps) % 1.0) - height(u, (v - eps + 1.0) % 1.0)) / (2.0 * eps);
            let len = (dhdu * dhdu * bump * bump + dhdv * dhdv * bump * bump + 1.0).sqrt();
            let nx = -dhdu * bump / len;
            let ny = -dhdv * bump / len;
            let nz = 1.0 / len;
            let to_u8 = |t: f32| ((t.clamp(0.0, 1.0)) * 255.0) as u8;
            normal.extend_from_slice(&[
                to_u8(nx * 0.5 + 0.5),
                to_u8(ny * 0.5 + 0.5),
                to_u8(nz * 0.5 + 0.5),
                255,
            ]);
            let h = height(u, v);
            // ORM：R = AO（低斑遮蔽）、G = 粗糙度（草叶磨面高）、B = 金属度 0
            let ao = 0.72 + 0.28 * h;
            let roughness = 0.70 + 0.22 * h;
            orm.extend_from_slice(&[to_u8(ao), to_u8(roughness), 0, 255]);
            // 基色：深苔 → 亮草按噪声混合，另叠一点土色
            let speck = vnoise(u, v, 16);
            let c0 = [0.086f32, 0.173, 0.055];
            let c1 = [0.30, 0.46, 0.13];
            let soil = [0.24f32, 0.19, 0.11];
            let g = (0.35 * h + 0.65 * speck).clamp(0.0, 1.0);
            let s = (0.5 - 0.5 * h).clamp(0.0, 1.0) * 0.45;
            for k in 0..3 {
                let base = c0[k] + (c1[k] - c0[k]) * g;
                let mixed = base + (soil[k] - base) * s;
                albedo.extend_from_slice(&[to_u8(mixed.sqrt())]); // sRGB 编码
            }
            albedo.push(255);
        }
    }
    GrassTextures {
        albedo: CpuTexture {
            width: size,
            height: size,
            rgba: albedo,
            srgb: true,
        },
        normal: CpuTexture {
            width: size,
            height: size,
            rgba: normal,
            srgb: false,
        },
        orm: CpuTexture {
            width: size,
            height: size,
            rgba: orm,
            srgb: false,
        },
    }
}

/// [`grass_textures`] 的三件套返回值。
pub struct GrassTextures {
    /// 基色（sRGB 编码）。
    pub albedo: CpuTexture,
    /// 切线空间细节法线（线性数据）。
    pub normal: CpuTexture,
    /// AO / 粗糙度 / 金属度打包（线性数据）。
    pub orm: CpuTexture,
}

/// 收集地形碰撞拼块的 `(位置, 形状)` 列表——`NavGrid::build` 与
/// [`move_and_slide_statics`](iris_core::character::move_and_slide_statics)
/// 的直接入参口径（已 despawn 的实体静默跳过）。
pub fn terrain_statics(world: &World, terrain: &Terrain) -> Vec<(Vec3, ColliderShape)> {
    let mut out = Vec::with_capacity(terrain.colliders.len());
    for &entity in &terrain.colliders {
        if let (Some(transform), Some(collider)) = (
            world.get::<Transform>(entity),
            world.get::<Collider>(entity),
        ) {
            out.push((transform.position, collider.shape));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material::{MaterialDef, MaterialStore};
    use crate::mesh::MeshStore;
    use iris_core::character::{CharacterParams, move_and_slide_statics};
    use iris_core::nav::{NavGrid, NavGridParams};

    /// 测试常用小配置：8 cell chunk / 2 chunk 横向（接缝断言用）。
    fn hills(width: usize, depth: usize, seed: u64) -> Heightmap {
        generate_hills(width, depth, 0.5, seed, 1.0, 0.5)
    }

    /// 平坦 / 缓丘基准场：显式高度数组（解析对照不依赖生成器）。
    fn planar(width: usize, depth: usize, a: f32, b: f32) -> Heightmap {
        let cell = 0.5f32;
        let heights: Vec<f32> = (0..depth)
            .flat_map(|j| (0..width).map(move |i| a * (i as f32 * cell) + b * (j as f32 * cell)))
            .collect();
        Heightmap::new(width, depth, cell, heights)
    }

    // ---- 生成器 ----

    /// 同 seed 两次逐位一致（无 RNG 无全局态）；不同 seed 场不同。
    #[test]
    fn generate_hills_is_bitwise_deterministic() {
        let a = generate_hills(33, 33, 0.5, 42, 1.0, 1.2);
        let b = generate_hills(33, 33, 0.5, 42, 1.0, 1.2);
        for (x, y) in a.heights.iter().zip(&b.heights) {
            assert_eq!(x.to_bits(), y.to_bits(), "同 seed 须逐位一致");
        }
        let c = generate_hills(33, 33, 0.5, 43, 1.0, 1.2);
        assert!(
            a.heights
                .iter()
                .zip(&c.heights)
                .any(|(x, y)| x.to_bits() != y.to_bits()),
            "不同 seed 的高度场应有差异"
        );
    }

    /// 值域 [base − amplitude, base + amplitude]（闭区间论证见生成器文档），
    /// 且默认写实量级下相邻格点高差 < 0.2 m（walkable：远低于角色 /
    /// 导航的 0.3 台阶上限——地形开箱即用不打断行走）。
    #[test]
    fn generate_hills_stays_in_range_with_gentle_steps() {
        let (base, amp) = (1.0f32, 1.2f32);
        let hm = generate_hills(65, 65, 0.5, 2024, base, amp);
        for &h in &hm.heights {
            assert!(
                h >= base - amp - 1e-4 && h <= base + amp + 1e-4,
                "高度 {h} 超值域 [{}, {}]",
                base - amp,
                base + amp
            );
        }
        let mut max_diff = 0.0f32;
        for j in 0..hm.depth {
            for i in 0..hm.width {
                let h = hm.vertex_height(i, j);
                if i + 1 < hm.width {
                    max_diff = max_diff.max((hm.vertex_height(i + 1, j) - h).abs());
                }
                if j + 1 < hm.depth {
                    max_diff = max_diff.max((hm.vertex_height(i, j + 1) - h).abs());
                }
            }
        }
        assert!(
            max_diff < 0.2,
            "相邻格点高差 {max_diff} 应 < 0.2 m（默认参数 ≤ 台阶上限 0.3 的依据）"
        );
    }

    // ---- 查询：双线性 / 中心差分 ----

    /// 平面场 h = a·x + b·z 的双线性插值在角 / 边 / 心都解析精确
    /// （容差 1e-4 = f32 舍入依据）；格点处逐位等于数组元素（权重 0/1）。
    #[test]
    fn height_at_bilinear_matches_analytics() {
        let (a, b) = (0.6f32, -0.25f32);
        let hm = planar(9, 9, a, b);
        let cell = hm.cell_size;
        // 角：格点逐位
        let exact = hm.height_at(4.0 * cell, 6.0 * cell).expect("域内");
        assert_eq!(exact.to_bits(), hm.vertex_height(4, 6).to_bits());
        // 边中点：双线性退化为两角点均值（tx = 0.5、tz = 0）
        let mid = hm.height_at(4.5 * cell, 6.0 * cell).expect("域内");
        let want = (hm.vertex_height(4, 6) + hm.vertex_height(5, 6)) * 0.5;
        assert!((mid - want).abs() <= 1e-4, "边中点 {mid} vs {want}");
        // 任意内部点：解析式对照（双线性对平面场精确到 f32 舍入）
        for (x, z) in [(1.7f32, 0.9f32), (3.3, 2.8), (0.05, 1.95)] {
            let got = hm.height_at(x * cell, z * cell).expect("域内");
            let want = a * (x * cell) + b * (z * cell);
            assert!(
                (got - want).abs() < 1e-4,
                "({x}, {z}) 双线性 {got} vs 解析 {want}"
            );
        }
        // 域外 None（含恰越界与 NaN）
        assert!(hm.height_at(-1e-6, 0.0).is_none());
        assert!(hm.height_at(0.0, hm.span_z() + 1e-6).is_none());
        assert!(hm.height_at(f32::NAN, 0.0).is_none());
        assert!(hm.height_at(f32::INFINITY, 0.0).is_none());
    }

    /// 中心差分法线解析对照：平面场处处 = normalize(−a, 1, −b)
    /// （差分对线性场精确 ⇒ 内部与边界单边两支都成立）。
    #[test]
    fn normal_at_matches_central_difference() {
        let (a, b) = (0.6f32, -0.25f32);
        let hm = planar(9, 9, a, b);
        let want = Vec3::new(-a, 1.0, -b).normalize();
        for (x, z) in [
            (2.0f32, 2.0f32),           // 内部
            (0.0, 3.0),                 // 左边界（单边差分）
            (hm.span_x(), hm.span_z()), // 角点
        ] {
            let got = hm.normal_at(x, z).expect("域内");
            assert!(
                (got - want).length() < 1e-4,
                "({x}, {z}) 法线 {got:?} vs 解析 {want:?}"
            );
        }
        assert!(hm.normal_at(-0.1, 0.1).is_none(), "域外 None");
    }

    // ---- chunk 网格与 LOD ----

    /// n=8 的精确拓扑：LOD0 全格 81 顶点 / 128 三角；LOD1 边界全采样 +
    /// 内部抽 ½ = 41 / 48；LOD2 抽 ¼ = 33 / 32（逐级递减，手推计数见下）。
    /// 法线全部单位长；三角形几何法线 y ≥ 0（正面朝上，口径照
    /// cylinder_faces_cull_correctly）。
    #[test]
    fn chunk_mesh_topology_matches_expected_counts() {
        let hm = hills(17, 9, 7); // cell 16 × 8 → 横向 2 chunk
        let expected = [
            (81usize, 128usize), // LOD0：9×9 顶点、8×8×2 三角
            (41, 48),            // LOD1：边界 4 线全采样 + 内部偶格（18+8+15）
            (33, 32),            // LOD2：边界 + 唯一内部 4 倍数格点 (4,4)
        ];
        for (lod, &(verts, tris)) in expected.iter().enumerate() {
            let mesh = chunk_mesh(&hm, 0, 0, 8, lod, 4.0);
            assert_eq!(mesh.vertices.len(), verts, "LOD{lod} 顶点数");
            assert_eq!(mesh.indices.len() / 3, tris, "LOD{lod} 三角形数");
            // 索引全部在顶点范围内
            assert!(
                mesh.indices
                    .iter()
                    .all(|&i| (i as usize) < mesh.vertices.len())
            );
            // 法线单位长（容差 = f32 normalize 舍入）
            for v in &mesh.vertices {
                let n = Vec3::from(v.normal);
                assert!((n.length() - 1.0).abs() < 1e-4, "法线 {n:?} 非单位长");
            }
            // 几何法线朝上（高度场单值 ⇒ y ≥ 0；退化三点允许 = 0）
            for tri in mesh.indices.chunks(3) {
                let p = |i: u32| Vec3::from(mesh.vertices[i as usize].position);
                let (pa, pb, pc) = (p(tri[0]), p(tri[1]), p(tri[2]));
                assert!(
                    (pb - pa).cross(pc - pa).y >= -1e-6,
                    "三角形 {tri:?} 朝向朝下：{pa:?} {pb:?} {pc:?}"
                );
            }
            // 顶点位置全在 chunk 局部格点上（x/z = 整数 × cell、|y| 合理）
            for v in &mesh.vertices {
                let fx = v.position[0] / hm.cell_size;
                let fz = v.position[2] / hm.cell_size;
                assert!(
                    (fx - fx.round()).abs() < 1e-4 && (fz - fz.round()).abs() < 1e-4,
                    "顶点局部坐标 {v:?} 不在格点上"
                );
            }
        }
    }

    /// **接缝不变量**（无 skirt 防水密的核心）：相邻 chunk 任意 LOD 组合
    /// 下共享边折线的顶点世界位置 / 法线 / UV **逐位一致**
    /// （cell = 0.5 为 2 的幂 ⇒ x/z 角点加法顺序不引入舍入）。
    #[test]
    fn adjacent_chunks_share_bitwise_identical_edges() {
        let hm = hills(17, 9, 7);
        let cell = hm.cell_size;
        for lod_a in 0..TERRAIN_LOD_LEVELS {
            for lod_b in 0..TERRAIN_LOD_LEVELS {
                let a = chunk_mesh(&hm, 0, 0, 8, lod_a, 4.0);
                let b = chunk_mesh(&hm, 1, 0, 8, lod_b, 4.0);
                // A 的东边界（局部 x = 8·cell）vs B 的西边界（局部 x = 0），
                // 世界 x 均为 4 m（chunk 角点平移已含）
                for j in 0..=8usize {
                    let z = j as f32 * cell;
                    let ea: Vec<&Vertex> = a
                        .vertices
                        .iter()
                        .filter(|v| v.position[0] == 8.0 * cell && v.position[2] == z)
                        .collect();
                    let eb: Vec<&Vertex> = b
                        .vertices
                        .iter()
                        .filter(|v| v.position[0] == 0.0 && v.position[2] == z)
                        .collect();
                    assert_eq!(ea.len(), 1, "A(LOD{lod_a}) 边点 (8, {j}) 应唯一");
                    assert_eq!(eb.len(), 1, "B({lod_b}) 边点 (0, {j}) 应唯一");
                    let (va, vb) = (ea[0], eb[0]);
                    // 世界 y 直接相等（局部 y = 世界高度本体，无平移）
                    assert_eq!(
                        va.position[1].to_bits(),
                        vb.position[1].to_bits(),
                        "LOD{lod_a}/LOD{lod_b} 接缝 y 须逐位一致（裂缝 = 垂直轴）"
                    );
                    for k in 0..3 {
                        assert_eq!(
                            va.normal[k].to_bits(),
                            vb.normal[k].to_bits(),
                            "LOD{lod_a}/LOD{lod_b} 接缝法线分量 {k} 须逐位一致"
                        );
                    }
                    for k in 0..2 {
                        assert_eq!(
                            va.uv[k].to_bits(),
                            vb.uv[k].to_bits(),
                            "LOD{lod_a}/LOD{lod_b} 接缝 uv 须逐位一致（跨块平铺无缝）"
                        );
                    }
                }
            }
        }
    }

    /// chunk_mesh 纯函数确定性：同输入两次 to_bits 逐位。
    #[test]
    fn chunk_mesh_is_bitwise_deterministic() {
        let hm = hills(17, 9, 7);
        let a = chunk_mesh(&hm, 0, 0, 8, 1, 4.0);
        let b = chunk_mesh(&hm, 0, 0, 8, 1, 4.0);
        let bits = |v: &Vertex| {
            let mut out = Vec::new();
            out.extend(v.position.iter().map(|x| x.to_bits()));
            out.extend(v.normal.iter().map(|x| x.to_bits()));
            out.extend(v.uv.iter().map(|x| x.to_bits()));
            out
        };
        for pair in a.vertices.iter().zip(&b.vertices) {
            assert_eq!(bits(pair.0), bits(pair.1));
        }
        assert_eq!(a.indices, b.indices);
    }

    // ---- 构建（实体铺世界 + 碰撞拼块）----

    /// 64×64 cell 默认配置：4 chunk（各挂 LodMesh3 + 材质，无裸
    /// MeshHandle）+ 4096 碰撞盒（仅 Collider、不渲染）+ 根 Terrain；
    /// 碰撞盒顶面 = cell 最高角点（台阶语义）逐盒验证。
    #[test]
    fn build_terrain_spawns_chunks_and_collision_tiles() {
        let mut world = World::new();
        world.insert_resource(MeshStore::new());
        world.insert_resource(MaterialStore::new());
        let material = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.3, 0.5, 0.2, 1.0));
        let hm = hills(65, 65, 42); // 64×64 cell
        let build = build_terrain(&mut world, hm.clone(), material, &TerrainOptions::default());
        assert_eq!(build.chunks_x * build.chunks_z, 4);
        assert_eq!(build.chunks.len(), 4);
        assert_eq!(build.colliders.len(), 64 * 64);
        assert_eq!(
            world.resource::<MeshStore>().len(),
            4 * 3,
            "每 chunk 三级网格"
        );
        // 每 chunk：LodMesh3 + 材质，无裸 MeshHandle / Collider
        for &chunk in &build.chunks {
            let lod3 = world.get::<LodMesh3>(chunk).expect("chunk 应挂 LodMesh3");
            assert_eq!(lod3.switch_distances, [16.0, 40.0]);
            for &level in &lod3.levels {
                assert!(world.resource::<MeshStore>().get(level).is_some());
            }
            assert!(world.get::<Collider>(chunk).is_none());
        }
        // 碰撞盒：顶面 = cell 最高角点；无网格句柄 ⇒ 不进渲染收集
        let mut index = 0usize;
        for j0 in 0..64usize {
            for i0 in 0..64usize {
                let entity = build.colliders[index];
                index += 1;
                let transform = world.get::<Transform>(entity).expect("盒 Transform");
                let collider = world.get::<Collider>(entity).expect("盒 Collider");
                let ColliderShape::Box { half_extents } = collider.shape else {
                    panic!("拼块须为 Box");
                };
                assert!(collider.is_static, "拼块是静态体");
                assert!(world.get::<MeshHandle>(entity).is_none(), "拼块不渲染");
                let top = hm
                    .vertex_height(i0, j0)
                    .max(hm.vertex_height(i0 + 1, j0))
                    .max(hm.vertex_height(i0, j0 + 1))
                    .max(hm.vertex_height(i0 + 1, j0 + 1));
                assert!(
                    (transform.position.y + half_extents.y - top).abs() < 1e-4,
                    "拼块 ({i0}, {j0}) 顶面应贴 cell 最高角 {top}"
                );
                assert!(
                    (half_extents.x - 0.25).abs() < 1e-6 && (half_extents.z - 0.25).abs() < 1e-6
                );
            }
        }
        let terrain = world.get::<Terrain>(build.root).expect("根挂 Terrain");
        assert_eq!(terrain.chunks.len(), 4);
        assert_eq!(terrain.colliders.len(), 4096);
        // height_at_world 与 origin 垫高自洽
        let got = terrain.height_at_world(10.0, 12.0).expect("域内世界查询");
        assert!((got - hm.height_at(10.0, 12.0).unwrap()).abs() < 1e-6);
    }

    // ---- 角色控制器：拼块台阶语义 ----

    /// 手动单丘 heightmap：`x ≥ 2 m` 抬 rise（0.28 ≤ step 0.3 可迈 /
    /// 0.9 = 墙照撞）。12×4 cell、chunk_cells 4（须被最大 LOD 步长整除）。
    fn step_field(rise: f32) -> (Heightmap, TerrainOptions) {
        let (width, depth) = (13usize, 5usize); // 12×4 cell，6×2 m
        let heights: Vec<f32> = (0..depth)
            .flat_map(|_j| (0..width).map(move |i| if i >= 4 { rise } else { 0.0 }))
            .collect();
        let hm = Heightmap::new(width, depth, 0.5, heights);
        let options = TerrainOptions {
            chunk_cells: 4,
            ..TerrainOptions::default()
        };
        (hm, options)
    }

    fn build_with_colliders(
        hm: Heightmap,
        options: &TerrainOptions,
    ) -> (World, Vec<(Vec3, ColliderShape)>) {
        let mut world = World::new();
        world.insert_resource(MeshStore::new());
        world.insert_resource(MaterialStore::new());
        let material = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::default());
        let build = build_terrain(&mut world, hm, material, options);
        let terrain = world.get::<Terrain>(build.root).expect("Terrain").clone();
        let statics = terrain_statics(&world, &terrain);
        (world, statics)
    }

    /// 0.28 m 台阶（≤ step_height 0.3）：角色沿 +X 走过接缝并被贴地
    /// 吸附抬到盒顶（y = 0.28 + 半高 0.9，±2 cm）。
    #[test]
    fn character_climbs_low_terrain_step() {
        let (hm, options) = step_field(0.28);
        let (_world, statics) = build_with_colliders(hm, &options);
        let params = CharacterParams::default();
        let mut pos = Vec3::new(0.5, params.half_height, 0.75);
        let mut vel = Vec3::new(2.0, 0.0, 0.0);
        let mut grounded = false;
        for _ in 0..90 {
            let out = move_and_slide_statics(pos, vel, 1.0 / 60.0, &params, &statics);
            pos = out.position;
            vel = Vec3::new(2.0, out.velocity.y, 0.0);
            grounded = out.grounded;
        }
        assert!(pos.x > 3.0, "应已越过台阶（x = {}）", pos.x);
        assert!(
            grounded && (pos.y - (0.28 + params.half_height)).abs() < 0.02,
            "应站在盒顶：y = {} grounded = {grounded}",
            pos.y
        );
    }

    /// 0.9 m 陡差（> step_height）：拼块立面是墙，水平位移被挡在
    /// 迎风面 − 半径之前。
    #[test]
    fn character_blocked_by_steep_terrain_rise() {
        let (hm, options) = step_field(0.9);
        let (_world, statics) = build_with_colliders(hm, &options);
        let params = CharacterParams::default();
        let mut pos = Vec3::new(0.5, params.half_height, 0.75);
        let vel = Vec3::new(2.0, 0.0, 0.0);
        for _ in 0..90 {
            let out = move_and_slide_statics(pos, vel, 1.0 / 60.0, &params, &statics);
            pos = out.position;
        }
        assert!(
            pos.x < 1.5 + 0.5,
            "陡差应被挡（x = {}，迎风面 x = 1.5 左侧附近）",
            pos.x
        );
    }

    // ---- 导航（R49 高度场吃拼块盒顶）----

    /// 拼块丘（0.25 ≤ step 0.3）：NavGrid 表面解析出盒顶，find_path
    /// 翻丘——中间航点贴丘顶面 y ≈ 0.25、全程相邻高差 ≤ step_height。
    /// 注意 nav 边界**外扩于地形域**：R49 顶面雕刻 BFS 的种子是「未被
    /// 任何盒覆盖的隐式地面格」，域全覆盖则无种子（地形衔接域约束，
    /// 见模块文档）。
    #[test]
    fn navgrid_paths_over_terrain_ridge() {
        let (hm, options) = step_field(0.25);
        let (world, statics) = build_with_colliders(hm.clone(), &options);
        let params = NavGridParams::new(
            Vec3::new(-2.0, 0.0, -2.0),
            Vec3::new(hm.span_x() + 2.0, 3.0, hm.span_z() + 2.0),
        );
        let nav = NavGrid::build(params, &statics);
        let path = nav
            .find_path(Vec3::new(0.5, 0.0, 0.5), Vec3::new(4.5, 0.0, 0.5))
            .expect("0.25 ≤ step_height 0.3，应可翻丘");
        assert!(path.len() >= 3, "路径应有多段：{path:?}");
        // 丘顶区域（x ∈ [1.5, 6] 的拼块顶面 0.25）的航点贴 0.25 面；
        // 端点贴地（cell [1.5,2.0] 含抬升角点，盒顶即 0.25）
        let on_ridge = path.iter().filter(|p| p.x > 2.0 && p.x < 6.0).count();
        assert!(on_ridge >= 1, "应有落在丘顶 x∈(2,6) 的航点：{path:?}");
        for point in &path {
            let expected = if point.x >= 1.5 { 0.25 } else { 0.0 };
            assert!(
                (point.y - expected).abs() < 1e-3,
                "航点 {point:?} 应贴拼块顶面 {expected}"
            );
        }
        for pair in path.windows(2) {
            assert!(
                (pair[1].y - pair[0].y).abs() <= params.step_height + 1e-3,
                "相邻航点高差超 step_height：{:?}",
                pair.iter().map(|p| p.y).collect::<Vec<_>>()
            );
        }
        let _ = &world;
    }

    // ---- 材质程序化 ----

    /// 草地三件套：确定性逐位、色彩空间标注、法线 z 半（≥ 0.5）、
    /// ORM 金属度通道恒 0。
    #[test]
    fn grass_textures_are_deterministic_and_typed() {
        let a = grass_textures(32);
        let b = grass_textures(32);
        assert_eq!(a.albedo.rgba, b.albedo.rgba, "基色同输入逐位");
        assert_eq!(a.normal.rgba, b.normal.rgba);
        assert_eq!(a.orm.rgba, b.orm.rgba);
        assert_eq!(a.albedo.width, 32);
        assert!(a.albedo.srgb, "基色 = sRGB 颜色数据");
        assert!(!a.normal.srgb && !a.orm.srgb, "法线 / ORM = 线性数据");
        for px in a.normal.rgba.chunks(4) {
            assert!(px[2] >= 127, "切线空间法线 z ≥ 0.5（编码后）");
        }
        for px in a.orm.rgba.chunks(4) {
            assert_eq!(px[2], 0, "草地金属度恒 0");
        }
        for px in a.albedo.rgba.chunks(4) {
            assert!(px[1] >= px[0] && px[1] >= px[2], "草地基色应绿主导");
        }
    }

    // ---- LodMesh3 选择口径 ----

    #[test]
    fn lod3_select_picks_by_distance_boundaries_near_side() {
        let lod = LodMesh3 {
            levels: [MeshHandle(5), MeshHandle(6), MeshHandle(7)],
            switch_distances: [10.0, 20.0],
        };
        assert_eq!(
            lod.select(Vec3::new(0.0, 0.0, 9.99), Vec3::ZERO),
            MeshHandle(5)
        );
        assert_eq!(
            lod.select(Vec3::new(0.0, 0.0, 10.0), Vec3::ZERO),
            MeshHandle(5)
        );
        assert_eq!(
            lod.select(Vec3::new(0.0, 0.0, 10.01), Vec3::ZERO),
            MeshHandle(6)
        );
        assert_eq!(
            lod.select(Vec3::new(0.0, 0.0, 20.0), Vec3::ZERO),
            MeshHandle(6)
        );
        assert_eq!(
            lod.select(Vec3::new(0.0, 0.0, 20.01), Vec3::ZERO),
            MeshHandle(7)
        );
        assert_eq!(
            lod.select(Vec3::new(0.0, 0.0, 900.0), Vec3::ZERO),
            MeshHandle(7)
        );
    }
}
