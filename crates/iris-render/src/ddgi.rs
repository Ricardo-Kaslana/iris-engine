//! DDGI 动态全局光照 v1（R88）：**规则探针网格** + **CPU 光线追踪的时间片
//! 滚动重照明** + **深度感知混合**（chebyshev 单侧上界权重）。
//!
//! # 闭合的缺口
//!
//! `ARCHITECTURE.md` §8.52① —— R83 的径向权重 `(1 − (d/r)²)²` **不感知遮挡**：
//! 隔着薄墙的两个探针在墙两侧互相串味，影响半径是唯一的隔离手段。本轮以
//! 「逐探针方向深度 + chebyshev 权重」消灭该串味：着色点到探针的距离超过
//! 该方向的平均可见深度时，权重按「真实深度 ≥ 该距离」的概率上界衰减，
//! 墙后片元吃到的是天空兜底而不是墙另一侧探针的辐照。附带缓解 §8.52⑩
//! （探针布局是作者责任）——规则网格自动生成布局。
//!
//! # 三个组成部分
//!
//! 1. **探针网格**（[`DdgiSettings`] → [`plan_layout`] → [`DdgiGrid`]）：纯资源
//!    态、**不落实体**（避免 256 实体的 ECS 开销与 `&mut World` spawn 限制）。
//!    规则格心点阵，索引序 **x 最快**（`index = ix + nx·(iy + ny·iz)`）⇒
//!    滚动刷新的空间推进方向是 x 行扫描。点数超 [`MAX_IRRADIANCE_PROBES`]
//!    时做**确定性抽稀**（[`stride_for_capacity`]）：最小整数 k 使
//!    `⌈nx/k⌉·⌈ny/k⌉·⌈nz/k⌉ ≤ 256`，`spacing_eff = spacing × k`——网格仍
//!    规则无洞、覆盖范围不变（对比「按序截断」会砍掉远端一整块空间 ⇒ 该块
//!    GI 直接回退 env，出现可见的空间断界）。
//! 2. **时间片滚动重照明**（[`DdgiGrid::tick`] + [`probe_relight`]）：复用 R52a
//!    的 BVH 追踪器（[`crate::bake`]），每帧只重照明 k =
//!    `probes_per_frame` 个脏探针（环形轮转，[`advance`]）。光照 / 几何指纹
//!    变化 ⇒ 全表标脏但**不清空旧值**（旧值继续用、逐帧刷新 ⇒ 无闪烁，
//!    这正是 DDGI 的时域行为）。收敛帧数 = `⌈n_dirty / k⌉`（解析可断言）。
//! 3. **深度感知混合**（[`ddgi_depth_weight`] + [`ddgi_blend`]，GPU 侧
//!    `standard.wgsl::ddgi_irradiance`）：权重 = R83 的径向窗 × chebyshev
//!    深度项，双剪枝（`radial > 0` 且 `w > 0`）后才做 9 项 SH 点积。
//!
//! # 深度表的数据形态（决策 5 / 6 / 7）
//!
//! 每探针存 **cube 面参数化**的两阶矩：6 面 × [`DDGI_DEPTH_GRID`]²（= 8²）
//! 纹素 × (μ, σ²) 两个 f32 = 384 × 2 = 768 个 f32 = **3072 B/探针**，满载
//! 256 探针 = **768 KiB**（binding 20 的 `array<f32>` 标量数组）。
//!
//! - **cube 面参数化而非八面体图集**（DDGI 论文原味是 oct atlas）：与 SH
//!   投影**同批射线** ⇒ 零额外光线成本；WGSL 查表 = 主轴判定 + uv，省掉
//!   oct 编解码的 CPU/WGSL 双侧镜像与看门面（确定性面更小）。
//! - **标量数组而非 `array<vec4>`**：WGSL 无动态 swizzle ⇒ vec4 打包要 3 次
//!   select/取值，正是 [`ShProbeRaw`] 的偏离声明记录过的坑。
//! - **σ² 在 CPU 用 f64 求差后钳下限**：`E[t²] − μ²` 在 f32 里会灾难性抵消；
//!   钳 `σ² ≥ (DDGI_SIGMA_REL·μ)²` 同时消掉「单纹素零方差 ⇒ chebyshev 退化
//!   为硬阶跃」。着色器侧因此**零减法、零钳位**。
//!
//! 索引式（CPU/GPU 同式，看门单测钉住）：
//!
//! ```text
//! 索引 = (probe × DDGI_DEPTH_TEXELS + texel) × 2 + moment
//! texel = face × DDGI_DEPTH_GRID² + row × DDGI_DEPTH_GRID + col
//! moment: 0 = μ（平均可见深度，米）、1 = σ²（方差，米²）
//! ```
//!
//! # 运行期光照口径
//!
//! 重照明复用 [`BakeParams`]，映射（宿主系统体负责，见
//! `iris_engine::ddgi`）：`light_dir` = `DirectionalLight.direction`；
//! `light_color` = `color × intensity`（约定「已含强度」）；`sky_color` =
//! `settings.sky_color.unwrap_or(DirectionalLight.ambient)`（R80 昼夜免费
//! 联动），**不乘 `EnvironmentSettings.intensity`**——`irradiance` 在
//! `standard.wgsl` 的 `ibl_env` 已被 `env_params.y` 缩放，预乘会双计；
//! `shadow_softness` = 0（硬影单射线）；`sky_samples` = 1（本路径不做半球
//! 采样）；`bounces` = `settings.bounces`（≤ 1，受 [`BakeParams`] 现断言约束）。
//!
//! # 确定性
//!
//! 全部环节都是纯函数 + 固定序：轮转序 = 索引环形扫描（[`advance`]，无哈希
//! 迭代序）；单探针重照明 = 固定面序（[`PROBE_AXES`]）× 固定行列序 × f64
//! 定序累加（与 [`project_probe_sh`] 同构）；深度矩 = 固定子格序 f64 累加，
//! σ² 的求差与钳位在 CPU f64 域完成；BVH 的 tie-break 已就位
//! （`bake.rs::build_bvh` / `raycast_bvh`）；指纹 = `to_bits` 固定序位组合
//! ⇒「是否变化」逐位可判；hysteresis = 固定 α 的一阶确定性递推。
//!
//! **两条等价锚点**（单测钉住）：
//! ① `hysteresis = 0` 且单帧 `k = n` ⇒ 表逐位等于「全量重照明一遍」；
//! ② `rays_per_face = SH_CUBE_GRID`（= 32）⇒ [`probe_relight`] 的 SH 表
//!    **逐位等于** [`probe_sh_coefficients`]（跨模块锚点，同时证明
//!    [`probe_sh_radiance`] 改薄壳未漂移）。
//!
//! 跨适配器位级不可保（GPU 编译器有 fma / 重结合自由），同 §8.51 / §8.52⑤
//! 边界；CPU 对照轨（[`ddgi_blend`]）是镜像纯函数的**解析对照**，不是位级
//! 等价声明。
//!
//! # 诚实限制（§8.57）
//!
//! 1. 单色天空，不吃 `SkySource`（渐变 / HDR 全景被平均成常量）。
//! 2. 无点光 / 聚光 / emissive 参与重照明（[`BakeParams`] 无此输入）。
//! 3. 1 次反弹上限（`BakeParams.bounces ≤ 1` 现断言）。
//! 4. 几何变化靠宿主 `geometry_generation` **显式宣告**（不自动侦测：自动
//!    侦测要每帧全量收集三角形（ms 级）或遍历算指纹；照 `SkySource.generation`
//!    / `ensure_environment` 先例。DDGI 的卖点是「光照动态」而非「几何动态」）。
//! 5. 光照突变后 `⌈n/k⌉` 帧收敛（默认 256/2 = 128 帧 ≈ 2.1 s @60fps）。
//! 6. 深度角分辨率 6×8²（面心 ≈11.25°/纹素，面角因切向拉伸更宽）+
//!    nearest-texel 查表，无双线性插值。
//! 7. σ² 来自每纹素 `(rays_per_face / 8)²` 条射线（默认档 4 条）+ 相对 10%
//!    下限钳 ⇒ 厚度 < σ 的薄墙仍可能漏。
//! 8. 嵌入几何的探针不做 relocation（贡献被深度项抑制，但白占预算）。
//! 9. hysteresis 默认关（α = 0）⇒ 滚动窗口推进时相邻探针新旧差异可能有
//!    轻微空间撕裂。
//! 10. 跨适配器位级不可保。
//! 11. CPU 单线程内联重照明（未用 R73 分片并行——并行改累加序 ⇒ 需另做
//!     固定分片归约序论证）。
//! 12. 与光照贴图 / 反射探针 / SSR 的合成语义未修订（R89）。
//! 13. `sky_color` 默认取 `DirectionalLight.ambient`，而 ambient 同时是
//!     `standard.wgsl` 常量补光项的来源 ⇒ 同一旋钮两处消费、天空底色会双计
//!     （配平手段：显式覆盖 `sky_color` 或调低 ambient）。
//! 14. 无探针持久化（每次启用重新粗填 + 滚动精修）。
//! 15. 成本随场景三角数线性上浮（`rays_per_face = 16` ⇒ ≈0.83 ms/探针
//!     @1000 三角基线；默认 k = 2 ⇒ ≈1.7 ms/脏帧）。昼夜循环场景是**持续
//!     脏表**（光照每帧变）⇒ 这是常态开销而非峰值。
//! 16. 片元阶段只读 storage 缓冲占满 **4/4**（binding 16/17/18/20 = WebGPU
//!     默认的 `maxStorageBuffersPerShaderStage` 上限）⇒ 下一轮再加片元
//!     storage 必须先申请特性或改用纹理。
//! 17. 着色器全表线性扫描（≤ 256 探针，无逐探针空间加速结构，同 R83）：
//!     径向 × 深度双剪枝使 `w > 0` 才做 9 项 SH 点积，但扫描本身与探针数
//!     线性。
//! 18. 探针**不落实体**（纯资源态）⇒ 编辑器里看不见、也不能逐个调；
//!     R89 的放置入口再议实体化。
//! 19. **网格覆盖范围外回退 env ⇒ 可见等值线**：`Σw ≤ ε` 的兜底是 env 天空
//!     常量（见「兜底语义」），而探针影响半径是有限球 ⇒ 越过某条等值线后
//!     整片切到 env，边界是「半径球面并集」的截面 ⇒ **扇贝形亮带**。验收图
//!     `docs/ddgi.png` 里天花板与墙交接处那条波浪带就是它（探针层高 1.3 +
//!     半径 2.4 ⇒ 只够到 y = 3.7，墙顶 y = 4 以上无人接管）。诚实修法：把
//!     `bounds` 抬到盖住所有会被着色的表面（`bounds = None` 的自动派生只
//!     外扩 1×spacing，不保证盖住顶面）。
//!
//! [`ShProbeRaw`]: crate::irradiance::ShProbeRaw
//! [`PROBE_AXES`]: crate::irradiance::PROBE_AXES
//! [`project_probe_sh`]: crate::irradiance::project_probe_sh
//! [`probe_sh_coefficients`]: crate::irradiance::probe_sh_coefficients
//! [`probe_sh_radiance`]: crate::irradiance::probe_sh_radiance

use glam::Vec3;

use crate::bake::{BakeParams, BakeTriangle, Bvh, build_bvh};
use crate::irradiance::{
    MAX_IRRADIANCE_PROBES, PROBE_RADIUS_NEIGHBOR_FACTOR, SH_BLEND_EPS, SH_COEFFS, SH_CUBE_FACES,
    ShCoeffs, ShProbeRaw, cube_face_direction, cube_texel_solid_angle, probe_influence_weight,
    probe_ray_radiance_depth, sh_basis, sh_blend_l0, sh_eval, sh_lambert_convolve,
};

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 深度表的每面网格边长（**编译期常量，不进 settings**）。
///
/// 不进 settings 的理由：①缓冲定长可算（768 KiB，惰性创建）；②WGSL 侧可
/// 写成常数表达式；③少一个可配错的旋钮。可调项是
/// [`DdgiSettings::rays_per_face`]——它只影响 CPU 成本与质量，不影响 GPU 布局
/// （校验项：必须能被本常量整除，否则射线格无法整除映射到深度纹素）。
///
/// 角分辨率：每面 90° / 8 ⇒ 面心 **≈11.25°/纹素**（面角因切向拉伸更宽）。
pub const DDGI_DEPTH_GRID: u32 = 8;

/// 每面深度纹素数（`DDGI_DEPTH_GRID²` = 64）。
pub const DDGI_DEPTH_TEXELS_PER_FACE: usize = (DDGI_DEPTH_GRID * DDGI_DEPTH_GRID) as usize;

/// 每探针深度纹素数（6 面 × 64 = 384）。
pub const DDGI_DEPTH_TEXELS: usize = SH_CUBE_FACES * DDGI_DEPTH_TEXELS_PER_FACE;

/// 每探针深度表的 f32 数（384 纹素 × 2 矩 = 768）。
pub const DDGI_DEPTH_FLOATS: usize = DDGI_DEPTH_TEXELS * 2;

/// 每探针深度表的字节数（768 × 4 = **3072 B**）——增量上传的段长。
pub const DDGI_DEPTH_PROBE_BYTES: u64 = DDGI_DEPTH_FLOATS as u64 * 4;

/// 深度表总字节数（256 × 3072 = **786432 B = 768 KiB**，惰性创建）。
pub const DDGI_DEPTH_TABLE_BYTES: u64 = DDGI_DEPTH_PROBE_BYTES * MAX_IRRADIANCE_PROBES as u64;

const _: () = assert!(
    DDGI_DEPTH_TABLE_BYTES == 786_432,
    "深度表定长必须是 768 KiB（256 × 6 × 8² × 2 × 4 B）"
);

/// 天空方向的**有限**深度哨兵（10 km）。
///
/// 不用 `f32::INFINITY`：无穷进矩累加会把 Σt / Σt² 变成 inf，
/// `E[t²] − μ²` 直接 NaN，整个纹素的 σ² 报废。10 km 远大于任何箱庭场景的
/// 对角线 ⇒ chebyshev 的 `dist > μ` 恒假 ⇒ 权重恒 1（= 「无遮挡证据」的
/// 正确语义），且矩累加保持有限。
pub const DDGI_SKY_DEPTH: f32 = 1.0e4;

/// σ² 的**相对**下限系数：钳 `σ² ≥ (DDGI_SIGMA_REL × μ)²`。
///
/// 两个作用：①消掉「单纹素零方差 ⇒ chebyshev 退化为硬阶跃」（`dist` 刚过
/// μ 一点点权重就掉到 0，墙面出现锯齿状分界）；②给薄墙留容差——每纹素只有
/// `(rays_per_face / 8)²` 条射线（默认档 4 条），样本方差本身是噪声量级。
/// 10% 的依据：让「距离超出 μ 一成」时权重仍有 0.5（`σ²/(σ² + (0.1μ)²)`），
/// 超出三成时降到 ≈0.1 ⇒ 衰减带宽度与探针间距同量级，既不漏成硬阶跃也不糊
/// 成无遮挡。
pub const DDGI_SIGMA_REL: f32 = 0.1;

/// 安装帧**粗填**的方向网格边长（6 面 × 2² = **24 方向/探针**）。
///
/// 避免前 `⌈n/k⌉` 帧出现「SH = 0 的黑波按网格索引序扫过场景」——粗填给全表
/// 一个低频但非零的初值，滚动精修再逐探针替换成真值。满载一次性成本
/// ≈ 3.3 ms（24 方向 vs 精修档 1536 方向 = 1/64）。
///
/// 粗填**只填 SH，深度矩置中性**（μ = [`DDGI_SKY_DEPTH`]、σ² = 0 ⇒ 深度项
/// 恒 1、不抑制）：粗填档 2 < [`DDGI_DEPTH_GRID`] 无法整除映射；且保守方向
/// 正确（粗填期可能短暂漏光，`⌈n/k⌉` 帧内被真深度取代，优于黑波）。
pub const DDGI_PREFILL_GRID: u32 = 2;

/// [`DdgiSettings::rays_per_face`] 的合法档位。
///
/// 8 = 省成本档（≈0.21 ms/探针）；16 = **默认质量档**（≈0.83 ms/探针）；
/// 32 = `SH_CUBE_GRID`（与 R83 烘焙投影器同档，等价锚点②的取值）；
/// 64 = 上限（σ² 的每纹素样本数 64，成本 4× 默认档）。
pub const DDGI_RAYS_PER_FACE_CHOICES: [u32; 4] = [8, 16, 32, 64];

/// 档位表的编译期前提：每档都能被 [`DDGI_DEPTH_GRID`] 整除（深度矩的整除
/// 映射 `row / (grid / 8)` 的前提）。放在编译期而不是运行期校验里——加新
/// 档位时若忘了这条，`probe_relight` 的深度累积会静默错位。
const _: () = assert!(
    DDGI_RAYS_PER_FACE_CHOICES[0] % DDGI_DEPTH_GRID == 0
        && DDGI_RAYS_PER_FACE_CHOICES[1] % DDGI_DEPTH_GRID == 0
        && DDGI_RAYS_PER_FACE_CHOICES[2] % DDGI_DEPTH_GRID == 0
        && DDGI_RAYS_PER_FACE_CHOICES[3] % DDGI_DEPTH_GRID == 0,
    "rays_per_face 的每个档位都必须能被 DDGI_DEPTH_GRID 整除"
);

/// splitmix64 变体（纯整数混洗：无浮点、无 libm、无全局状态 ⇒ 跨运行 /
/// 跨平台逐位一致，AGENTS.md 的无 RNG 文化；`irradiance.rs` / `particles.rs`
/// 同款私有副本）。
#[inline]
fn mix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

// ---------------------------------------------------------------------------
// 设置
// ---------------------------------------------------------------------------

/// DDGI 配置（World 资源）。
///
/// **缺席语义（API 的一部分）**：资源缺席 = **DDGI 关**——渲染器一个
/// `write_buffer` 都不发、binding 20 绑 4 B 全零哑缓冲、GI 参数维持 R83 的
/// 裁决结果（mode 0 或 1）⇒ 既有全部 FNV 基线原值通过。
///
/// **与 R83 烘焙探针集互斥（DDGI 优先）**：[`DdgiGrid`] 非空 ⇒ 整表替换
/// binding 18/19 并**忽略** [`IrradianceProbeSet`](crate::irradiance::IrradianceProbeSet)
/// （渲染器 warn 一次）。理由：两份间接漫反射叠加会**双重计入**天空与色溢
/// （物理错误）。
#[derive(Clone, Copy, Debug, PartialEq, iris_core::Resource)]
pub struct DdgiSettings {
    /// 网格间距（米）；须正有限。
    pub spacing: f32,
    /// 网格范围 (min, max)。
    ///
    /// `None` ⇒ 取 [`collect_bake_triangles`](crate::bake::collect_bake_triangles)
    /// 的静态几何 AABB 向外扩 1×spacing，**且 Y 下限额外抬 0.5×spacing**
    /// （防探针埋进地板下 ⇒ 探到的是地板底下的「空」，整片 GI 被抑制）；
    /// 空几何 ⇒ 网格空 ⇒ 视为缺席。
    pub bounds: Option<(Vec3, Vec3)>,
    /// 每帧重照明探针数 k（滚动窗口宽度）。0 ⇒ 永不更新（只吃粗填值）。
    pub probes_per_frame: usize,
    /// SH 投影的每面格数 G（方向数 = `6 × G²`）；取值限
    /// [`DDGI_RAYS_PER_FACE_CHOICES`]，且须能被 [`DDGI_DEPTH_GRID`] 整除。
    pub rays_per_face: u32,
    /// 时域滤波的**旧值权重** α（`sh = old × α + fresh × (1 − α)`）；
    /// 钳 `[0, 1]`（见 [`Self::alpha`]）。0 = 关（默认 ⇒「收敛帧数 =
    /// `⌈n/k⌉`」可解析断言）；1 = 冻结（表永不更新）。
    pub hysteresis: f32,
    /// 天空辐射率覆盖。`None` ⇒ 取 `DirectionalLight.ambient`（R80 昼夜免费
    /// 联动；已知代价 = ambient 双计，见模块文档限制⑬）。
    pub sky_color: Option<Vec3>,
    /// 间接反弹次数（0 = 只天空）；受 [`BakeParams`] 的 `≤ 1` 断言约束。
    pub bounces: u32,
    /// 宿主递增 = 宣告场景几何变化（⇒ 重建 BVH + 全表标脏 + 重新粗填）。
    pub geometry_generation: u64,
}

impl Default for DdgiSettings {
    fn default() -> Self {
        Self {
            spacing: 2.0,
            bounds: None,
            probes_per_frame: 2,
            rays_per_face: 16,
            hysteresis: 0.0,
            sky_color: None,
            bounces: 1,
            geometry_generation: 0,
        }
    }
}

impl DdgiSettings {
    /// 参数合法性（宿主系统体每帧调用一次；非法即 panic，不静默降级——
    /// 与 [`BakeParams`] 的校验文化一致）。
    pub fn validate(&self) {
        assert!(
            self.spacing.is_finite() && self.spacing > 0.0,
            "DDGI 网格间距须为正有限值：{}",
            self.spacing
        );
        if let Some((min, max)) = self.bounds {
            assert!(
                min.is_finite() && max.is_finite(),
                "DDGI bounds 须有限：{min:?} .. {max:?}"
            );
            assert!(
                max.x >= min.x && max.y >= min.y && max.z >= min.z,
                "DDGI bounds 的 max 须逐分量 ≥ min：{min:?} .. {max:?}"
            );
        }
        assert!(
            DDGI_RAYS_PER_FACE_CHOICES.contains(&self.rays_per_face),
            "rays_per_face 只能取 {DDGI_RAYS_PER_FACE_CHOICES:?}（每档都能被 \
             DDGI_DEPTH_GRID = {DDGI_DEPTH_GRID} 整除，见档位表的编译期断言），收到 {}",
            self.rays_per_face
        );
        assert!(
            self.bounces <= 1,
            "v1 只支持 0/1 次反弹（BakeParams 现断言），收到 {}",
            self.bounces
        );
        if let Some(sky) = self.sky_color {
            assert!(sky.is_finite(), "DDGI sky_color 须有限：{sky:?}");
        }
    }

    /// 钳后的 hysteresis α（`[0, 1]`；NaN ⇒ 0 = 关）。
    ///
    /// 上界含 1 = 冻结（`sh = old × 1 + fresh × 0`）：α ≤ 1 不放大任何系数
    /// ⇒ 设计稿「钳 [0,1)」防发散的意图完整保留。
    pub fn alpha(&self) -> f32 {
        if self.hysteresis.is_nan() {
            return 0.0;
        }
        self.hysteresis.clamp(0.0, 1.0)
    }
}

// ---------------------------------------------------------------------------
// 网格生成（纯函数，可 headless 单测）
// ---------------------------------------------------------------------------

/// 规则探针网格的布局参数（[`plan_layout`] 的产物）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DdgiLayout {
    /// 网格原点（= 有效 bounds 的 min）。
    pub min: Vec3,
    /// 声明间距（米，抽稀前）。
    pub spacing: f32,
    /// 抽稀**前**的每轴点数 `[nx, ny, nz]`。
    pub counts: [usize; 3],
    /// 确定性抽稀步长 k（1 = 未抽稀）。
    pub stride: usize,
    /// 抽稀**后**的每轴点数 `⌈n/k⌉`。
    pub counts_eff: [usize; 3],
    /// 有效间距 = `spacing × stride`（米）。
    pub spacing_eff: f32,
    /// 探针总数 = `counts_eff` 三者之积（≤ `capacity`）。
    pub total: usize,
}

/// 单轴点数：`max(1, floor(span / spacing))`。
///
/// 边界语义：span = 0（扁平 bounds）⇒ 1 点；NaN ⇒ 1 点（`f32::max` 对 NaN
/// 返回另一操作数）；span/spacing 溢出 ⇒ `as usize` 饱和到 `usize::MAX`
/// （由 [`stride_for_capacity`] 的二分 + `checked_mul` 吸收，不 panic）。
pub fn axis_points(span: f32, spacing: f32) -> usize {
    ((span / spacing).floor().max(1.0)) as usize
}

/// 确定性抽稀步长：**最小整数 k ≥ 1** 使 `⌈nx/k⌉·⌈ny/k⌉·⌈nz/k⌉ ≤ capacity`。
///
/// 整数二分求解（无浮点搜索）⇒ 逐位确定。谓词单调（k 增大 ⇒ 各 `⌈n/k⌉`
/// 不增 ⇒ 积不增）⇒ 二分给出的就是最小 k。上界取 `max(nx, ny, nz)`：此时
/// 每个 `⌈n/k⌉ = 1`、积 = 1 ≤ capacity ⇒ 解恒存在。乘积用 `checked_mul`
/// （溢出视为「太大」⇒ 需要更大的 k），故 nx 饱和到 `usize::MAX` 也不 panic。
pub fn stride_for_capacity(nx: usize, ny: usize, nz: usize, capacity: usize) -> usize {
    assert!(capacity >= 1, "容量至少 1，收到 {capacity}");
    let fits = |k: usize| -> bool {
        nx.div_ceil(k)
            .checked_mul(ny.div_ceil(k))
            .and_then(|v| v.checked_mul(nz.div_ceil(k)))
            .is_some_and(|v| v <= capacity)
    };
    if fits(1) {
        return 1;
    }
    let (mut lo, mut hi) = (1usize, nx.max(ny).max(nz).max(1));
    while lo + 1 < hi {
        let mid = lo + (hi - lo) / 2;
        if fits(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hi
}

/// 规划规则网格布局（格点数 + 抽稀 + 有效间距）。
///
/// `capacity` = [`MAX_IRRADIANCE_PROBES`]（GPU 探针表的定长）。倒置 bounds
/// （max < min）按 span = 0 处理（每轴 1 点），不 panic——布局是纯几何派生，
/// 合法性由 [`DdgiSettings::validate`] 把关。
pub fn plan_layout(min: Vec3, max: Vec3, spacing: f32, capacity: usize) -> DdgiLayout {
    assert!(
        spacing.is_finite() && spacing > 0.0,
        "spacing 须为正有限值：{spacing}"
    );
    assert!(
        min.is_finite() && max.is_finite(),
        "bounds 须有限：{min:?} .. {max:?}"
    );
    let span = (max - min).max(Vec3::ZERO);
    let counts = [
        axis_points(span.x, spacing),
        axis_points(span.y, spacing),
        axis_points(span.z, spacing),
    ];
    let stride = stride_for_capacity(counts[0], counts[1], counts[2], capacity);
    let counts_eff = [
        counts[0].div_ceil(stride),
        counts[1].div_ceil(stride),
        counts[2].div_ceil(stride),
    ];
    let spacing_eff = spacing * stride as f32;
    let total = counts_eff[0] * counts_eff[1] * counts_eff[2];
    assert!(
        total <= capacity,
        "抽稀后点数 {total} 仍超容量 {capacity}（stride_for_capacity 失效）"
    );
    DdgiLayout {
        min,
        spacing,
        counts,
        stride,
        counts_eff,
        spacing_eff,
        total,
    }
}

/// 布局 → 探针世界位置（**格心**，索引序 x 最快）。
///
/// `index = ix + nx·(iy + ny·iz)` ⇒ 轮转序在空间上是 x 行扫描（滚动刷新的
/// 空间推进方向由此决定）。格点 = `min + (i + 0.5) × spacing_eff`——抽稀后
/// 取的是**粗化网格自己的格心**，`counts_eff × spacing_eff ≈ 原 span` ⇒
/// 覆盖范围不变。
pub fn layout_positions(layout: &DdgiLayout) -> Vec<Vec3> {
    let mut out = Vec::with_capacity(layout.total);
    for iz in 0..layout.counts_eff[2] {
        for iy in 0..layout.counts_eff[1] {
            for ix in 0..layout.counts_eff[0] {
                let offset = Vec3::new(
                    (ix as f32 + 0.5) * layout.spacing_eff,
                    (iy as f32 + 0.5) * layout.spacing_eff,
                    (iz as f32 + 0.5) * layout.spacing_eff,
                );
                let p = layout.min + offset;
                assert!(
                    p.is_finite(),
                    "DDGI 格点非有限（spacing = {}、stride = {}、bounds 过大）：\
                     请增大 spacing 或收紧 bounds",
                    layout.spacing,
                    layout.stride
                );
                out.push(p);
            }
        }
    }
    out
}

/// 三角集的 AABB（空集 ⇒ `None`）。
pub fn triangles_bounds(triangles: &[BakeTriangle]) -> Option<(Vec3, Vec3)> {
    if triangles.is_empty() {
        return None;
    }
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for tri in triangles {
        for v in &tri.vertices {
            min = min.min(*v);
            max = max.max(*v);
        }
    }
    Some((min, max))
}

/// `bounds = None` 时的默认范围：静态几何 AABB 向外扩 1×spacing，
/// **Y 下限额外抬 0.5×spacing**（净效果 `min.y − 0.5×spacing`）。
///
/// 抬 Y 下限的理由：探针埋在地板**下方**时它看到的全是地板背面 ⇒ 深度项把
/// 地板上的着色点判成「墙后」，整片 GI 被抑制成天空兜底。
pub fn default_bounds(triangles: &[BakeTriangle], spacing: f32) -> Option<(Vec3, Vec3)> {
    let (min, max) = triangles_bounds(triangles)?;
    let pad = Vec3::splat(spacing);
    let lift = Vec3::new(0.0, spacing * 0.5, 0.0);
    Some((min - pad + lift, max + pad))
}

// ---------------------------------------------------------------------------
// 时间片滚动（纯函数）
// ---------------------------------------------------------------------------

/// 环形轮转：自 `cursor` 起扫描至多 n 个位置，取前 k 个 dirty 者。
///
/// 返回 `(本帧更新的下标（轮转序）, 新 cursor)`。`cursor'` = **最后被更新者
/// 的下一位**；无更新 ⇒ cursor 不动（稳态零操作的前提）。O(n) 最坏
/// （n ≤ 256，可忽略）。`cursor ≥ n` 时钳到 `n − 1`（纯防御：生产路径的
/// cursor 恒由本函数产出 ⇒ 恒 < n）。
///
/// **确定性**：纯索引环形扫描，无哈希迭代序、无 BTree 依赖 ⇒ 同 (cursor,
/// dirty, k) 两次调用逐位同结果（单测钉住解析对照）。
pub fn advance(cursor: usize, dirty: &[bool], k: usize, n: usize) -> (Vec<usize>, usize) {
    let n = n.min(dirty.len());
    if n == 0 || k == 0 {
        return (Vec::new(), cursor);
    }
    let cursor = cursor.min(n - 1);
    let take = k.min(n);
    let mut updated = Vec::with_capacity(take);
    let mut next = cursor;
    for step in 0..n {
        let i = (cursor + step) % n;
        if dirty[i] {
            updated.push(i);
            next = (i + 1) % n;
            if updated.len() == take {
                break;
            }
        }
    }
    if updated.is_empty() {
        (Vec::new(), cursor)
    } else {
        (updated, next)
    }
}

/// 时域滤波（hysteresis）：`old × α + fresh × (1 − α)` 逐系数。
///
/// α = 0 ⇒ 逐位等于纯替换（`fresh × 1.0` 精确、`old × 0.0` 是 ±0.0，加法
/// 不改变另一操作数——唯一例外是 `fresh = −0.0` 且 `old ≥ 0`：IEEE 给出
/// `+0.0`。SH 系数由 f64 累加正确舍入而来，负零只在「累加和恰为负零」时
/// 出现，而 IEEE 加法不产出负零；单测在真实重照明数据上钉住逐位相等）。
/// α = 1 ⇒ 冻结（`old × 1.0 + fresh × 0.0`）。
pub fn sh_hysteresis(old: &ShCoeffs, fresh: &ShCoeffs, alpha: f32) -> ShCoeffs {
    let beta = 1.0 - alpha;
    std::array::from_fn(|k| {
        [
            old[k][0] * alpha + fresh[k][0] * beta,
            old[k][1] * alpha + fresh[k][1] * beta,
            old[k][2] * alpha + fresh[k][2] * beta,
        ]
    })
}

/// 深度矩的同款一阶递推（μ 与 σ² 各线性混合）。
///
/// μ ≥ 0、σ² ≥ 0 ⇒ α = 0 时逐位等于纯替换（无负零边界，见 [`sh_hysteresis`]）。
pub fn depth_hysteresis(old: [f32; 2], fresh: [f64; 2], alpha: f32) -> [f32; 2] {
    let beta = 1.0 - alpha;
    [
        old[0] * alpha + fresh[0] as f32 * beta,
        old[1] * alpha + fresh[1] as f32 * beta,
    ]
}

/// 重照明口径指纹：对 (`light_dir`, `light_color`, `sky_color`, `bounces`,
/// `rays_per_face`) 的 `to_bits` 做**固定序**位组合。
///
/// `rays_per_face` 一并入指纹（设计稿 light_fp 四项之外的追加项）：它决定
/// 每探针的射线预算，变了旧表就不再是同一口径的产物 ⇒ 必须全表标脏。
/// 变化 ⇒ 全表标脏（**不清空旧值**——旧值继续用、逐帧刷新 ⇒ 无闪烁）。
///
/// **不消费的字段不入指纹**（`shadow_softness` / `sky_samples`）：本路径走
/// `allow_soft = false` 的硬影单射线、不做半球采样，把它们算进去会让无关
/// 旋钮误判全表脏（单测钉住）。
pub fn relight_fingerprint(params: &BakeParams, rays_per_face: u32) -> u64 {
    let mut h = mix64(0);
    let values = [
        params.light_dir.x,
        params.light_dir.y,
        params.light_dir.z,
        params.light_color.x,
        params.light_color.y,
        params.light_color.z,
        params.sky_color.x,
        params.sky_color.y,
        params.sky_color.z,
    ];
    for v in values {
        h = mix64(h ^ u64::from(v.to_bits()));
    }
    h = mix64(h ^ u64::from(params.bounces));
    h = mix64(h ^ u64::from(rays_per_face));
    h
}

/// 网格布局指纹：对 (`spacing`, `bounds`) 的 `to_bits` 做固定序位组合。
///
/// 变化 ⇒ 重建网格（位置 / 半径 / 粗填 / 全表标脏 / `structure_generation`
/// 递增 ⇒ 渲染器全量上传）。`bounds = None`（从几何派生）只占一位标志——
/// 派生结果的变化由 `geometry_generation` 覆盖。
pub fn grid_fingerprint(settings: &DdgiSettings) -> u64 {
    let mut h = mix64(1);
    h = mix64(h ^ u64::from(settings.spacing.to_bits()));
    match settings.bounds {
        None => h = mix64(h ^ 1),
        Some((min, max)) => {
            for v in [min.x, min.y, min.z, max.x, max.y, max.z] {
                h = mix64(h ^ u64::from(v.to_bits()));
            }
        }
    }
    h
}

// ---------------------------------------------------------------------------
// 单探针重照明（SH 投影 + 深度两阶矩，同一批射线）
// ---------------------------------------------------------------------------

/// 深度矩的出口：`(Σt, Σt², 计数)` → `(μ, σ²)`，σ² **在 f64 域求差后钳下限**。
///
/// `E[t²] − μ²` 必须在 f64 里做：两项同量级（μ² 与 E[t²] 相差 σ²，而 σ² 通常
/// 比 μ² 小几个数量级），f32 下会灾难性抵消成 0。求差后 `.max(0.0)` 吸收
/// 残余的舍入负值，再钳 `σ² ≥ (DDGI_SIGMA_REL × μ)²`。计数 0（不可达：grid
/// 能被 8 整除 ⇒ 每纹素恒 `(grid/8)²` 条射线）⇒ 中性。
pub fn depth_moment_finish(sum_t: f64, sum_t2: f64, count: u32) -> [f64; 2] {
    if count == 0 {
        return neutral_depth_moment();
    }
    let n = f64::from(count);
    let mu = sum_t / n;
    let variance = (sum_t2 / n - mu * mu).max(0.0);
    let floor = f64::from(DDGI_SIGMA_REL) * mu;
    [mu, variance.max(floor * floor)]
}

/// 中性深度矩：`μ = DDGI_SKY_DEPTH`、`σ² = 0` ⇒ chebyshev 的 `dist > μ`
/// 恒假 ⇒ 权重恒 1（「无遮挡证据」= 不抑制）。粗填阶段与空纹素用。
pub fn neutral_depth_moment() -> [f64; 2] {
    [f64::from(DDGI_SKY_DEPTH), 0.0]
}

/// 射线格下标 → 深度纹素格下标（`row / (grid / DDGI_DEPTH_GRID)`）。
///
/// 与 [`ddgi_depth_texel`] 的 `floor((t + 1) × 0.5 × 8)` 在**射线中心方向**上
/// 恒等（单测 `depth_texel_matches_accumulation_cell` 逐格穷举钉住）：
/// 射线中心的 `t = −1 + (2/grid)(i + 0.5)` ⇒ `(t+1)·0.5·8 = (i + 0.5)/sub`，
/// 其 floor 恒 = `i / sub`（与整数边界差 `0.5/sub ≥ 0.0625`，f32 舍入翻不
/// 过去）。这就是「CPU 累积格 ↔ GPU 查表格同格」的硬证据。
pub fn depth_cell_of_ray(index: u32, grid: u32) -> u32 {
    let sub = (grid / DDGI_DEPTH_GRID).max(1);
    index / sub
}

/// 单探针重照明：一次遍历同时产出 **SH 系数**（E/π 直读）与 **深度两阶矩**。
///
/// 单遍循环 = 6 面（[`PROBE_AXES`] 序）× `grid²`（行列序固定），每方向调用
/// [`probe_ray_radiance_depth`] 拿 (辐射率 L, 深度 t)。**同一批射线**同时喂
/// SH 投影与深度矩 ⇒ 深度项零额外光线成本（决策 5 的兑现）。
///
/// **SH 累加与 [`project_probe_sh`](crate::irradiance::project_probe_sh) 逐式
/// 同构**（f64 × [`cube_texel_solid_angle`] 精确固体角权，**不是**等权 × 4π/N；
/// 基函数用 f32 的 [`sh_basis`]；出口 [`sh_lambert_convolve`] ⇒ 存 E/π 直读）
/// ⇒ `grid = SH_CUBE_GRID` 时两者**逐位相等**（等价锚点②，单测钉住）。
///
/// **深度矩**：`grid²` 方向按 [`depth_cell_of_ray`] 映射到 8² 纹素，每纹素
/// f64 定序累加 `Σt`、`Σt²`、计数，出口走 [`depth_moment_finish`]。
///
/// `grid < DDGI_DEPTH_GRID`（粗填档 [`DDGI_PREFILL_GRID`] = 2）⇒ 无法整除
/// 映射，深度矩整表返回 [`neutral_depth_moment`]（见该常量的文档）。
///
/// 返回 `(SH 系数, 深度矩)`，深度矩长度恒 [`DDGI_DEPTH_TEXELS`]（= 384）。
///
/// [`PROBE_AXES`]: crate::irradiance::PROBE_AXES
pub fn probe_relight(
    bvh: &Bvh,
    position: Vec3,
    params: &BakeParams,
    grid: u32,
) -> (ShCoeffs, Vec<[f64; 2]>) {
    assert!(position.is_finite(), "探针位置非有限：{position:?}");
    assert!(grid >= 1, "方向网格至少 1，收到 {grid}");
    let step = 2.0 / f64::from(grid);
    // 0 ⇒ 粗填档：射线格细于深度格，无法整除映射（深度矩返回中性）
    let depth_sub = (grid / DDGI_DEPTH_GRID) as usize;
    let mut acc = [[0.0f64; 3]; SH_COEFFS];
    let mut moments: Vec<[f64; 3]> = if depth_sub > 0 {
        vec![[0.0; 3]; DDGI_DEPTH_TEXELS]
    } else {
        Vec::new()
    };
    for face in 0..SH_CUBE_FACES {
        for row in 0..grid {
            let v0 = -1.0 + step * f64::from(row);
            let v1 = v0 + step;
            let v = (0.5 * (v0 + v1)) as f32;
            let drow = (row as usize / depth_sub.max(1)) * DDGI_DEPTH_GRID as usize;
            for col in 0..grid {
                let u0 = -1.0 + step * f64::from(col);
                let u1 = u0 + step;
                let u = (0.5 * (u0 + u1)) as f32;
                let dw = cube_texel_solid_angle(u0, u1, v0, v1);
                let dir = cube_face_direction(face, u, v);
                let (l, t) = probe_ray_radiance_depth(bvh, position, dir, params);
                let y = sh_basis(dir);
                for (k, c) in acc.iter_mut().enumerate() {
                    c[0] += f64::from(l.x) * f64::from(y[k]) * dw;
                    c[1] += f64::from(l.y) * f64::from(y[k]) * dw;
                    c[2] += f64::from(l.z) * f64::from(y[k]) * dw;
                }
                if depth_sub > 0 {
                    let texel =
                        face * DDGI_DEPTH_TEXELS_PER_FACE + drow + col as usize / depth_sub.max(1);
                    let tf = f64::from(t);
                    let m = &mut moments[texel];
                    m[0] += tf;
                    m[1] += tf * tf;
                    m[2] += 1.0;
                }
            }
        }
    }
    let projected = std::array::from_fn(|k| [acc[k][0] as f32, acc[k][1] as f32, acc[k][2] as f32]);
    let depth = if depth_sub == 0 {
        vec![neutral_depth_moment(); DDGI_DEPTH_TEXELS]
    } else {
        moments
            .iter()
            .map(|m| depth_moment_finish(m[0], m[1], m[2] as u32))
            .collect()
    };
    (sh_lambert_convolve(projected), depth)
}

// ---------------------------------------------------------------------------
// 深度感知混合（CPU 镜像；WGSL：standard.wgsl 的 ddgi_* 四函数）
// ---------------------------------------------------------------------------

/// 方向 → cube 面下标（与 [`cube_face_direction`] 严格互逆的面序判定）。
///
/// 主轴 = `|x|, |y|, |z|` 最大者，**并列取 [`PROBE_AXES`] 序低者**（严格 `>`
/// 扫描 ⇒ x → y → z 的优先序）；符号决定 ±（`comp < 0` ⇒ 负面，NaN 落正面支
/// ——WGSL 侧同式同极性，且 NaN 在两侧都不可达，见 [`ddgi_depth_weight_at`]）。
///
/// [`PROBE_AXES`]: crate::irradiance::PROBE_AXES
pub fn ddgi_depth_face(dir: Vec3) -> usize {
    let abs = [dir.x.abs(), dir.y.abs(), dir.z.abs()];
    let mut axis = 0usize;
    for i in 1..3 {
        if abs[i] > abs[axis] {
            axis = i;
        }
    }
    let comp = [dir.x, dir.y, dir.z][axis];
    if comp < 0.0 { axis * 2 + 1 } else { axis * 2 }
}

/// 方向 → 深度纹素下标（`0 .. 6 × grid²`）——**WGSL `ddgi_depth_texel` 的
/// CPU 镜像**（同式同序，改一侧必须改另一侧；源码层看门单测
/// `wgsl_ddgi_source_matches_cpu_contract` 钉住常数与关键表达式）。
///
/// ```text
/// face = 主轴 + 符号（ddgi_depth_face）
/// (u, v) = 另两轴分量 / 主轴分量（符号按面约定；局部轴序同 cube_face_direction）
/// row = floor((v + 1) × 0.5 × grid)，钳 [0, grid−1]；col 同（用 u）
/// texel = face × grid² + row × grid + col
/// ```
///
/// 只用分量的**比值与符号** ⇒ 传单位方向或未归一化的 `Δ` 同结果（WGSL 侧
/// 传 `delta / dist`）。nearest-texel，无双线性插值（诚实限制⑥）。
/// NaN 分量 ⇒ row/col 落 0 支（`!(cell >= 0)` 与 WGSL 的 `cell >= 0.0` 同义），
/// 但调用方（[`ddgi_depth_weight_at`]）已用 `dist > 0` 把 NaN 挡在外面。
pub fn ddgi_depth_texel(dir: Vec3, grid: u32) -> usize {
    assert!(grid >= 1, "深度网格至少 1，收到 {grid}");
    let face = ddgi_depth_face(dir);
    let main = [dir.x.abs(), dir.y.abs(), dir.z.abs()][face / 2];
    let (au, av) = match face {
        // ±X 面：u → y、v → z（同 cube_face_direction 的局部轴约定）
        0 | 1 => (dir.y, dir.z),
        // ±Y 面：u → x、v → z
        2 | 3 => (dir.x, dir.z),
        // ±Z 面：u → x、v → y
        _ => (dir.x, dir.y),
    };
    // NaN 语义靠**取反的比较**表达（`!(cell >= 0.0)` 与 WGSL 同式 ⇒ NaN 落 0
    // 支）；写成 `cell < 0.0` 会把 NaN 推到另一支 ⇒ 保守方向反转。
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    let cell_of = |t: f32| -> usize {
        let cell = ((t / main + 1.0) * 0.5 * grid as f32).floor();
        if !(cell >= 0.0) {
            0
        } else if cell >= grid as f32 {
            grid as usize - 1
        } else {
            cell as usize
        }
    };
    let col = cell_of(au);
    let row = cell_of(av);
    let g = grid as usize;
    face * g * g + row * g + col
}

/// chebyshev 单侧上界权重（DDGI 原式）——**WGSL `ddgi_depth_weight` 的
/// 数值核**（WGSL 侧多了 `enabled` 与 `dist > 0` 两道守卫，二者都不改变
/// 取值，见 [`ddgi_depth_weight_at`]）。
///
/// ```text
/// dist ≤ μ ⇒ 1                                   （着色点在探针可见范围内）
/// dist > μ ⇒ σ² / (σ² + (dist − μ)²)             （「真实深度 ≥ dist」的概率上界）
/// ```
///
/// 墙后片元的 `dist` 远超该方向的 μ ⇒ 权重趋 0，这正是 §8.52① 串味的解药。
/// `!(dist > μ)` 而不是 `dist <= μ`：NaN 同样落进「无遮挡证据」支（保守方向：
/// 不抑制）。σ² = 0 且 dist > μ ⇒ 0（硬阶跃——CPU 侧的 [`DDGI_SIGMA_REL`]
/// 下限钳保证真实数据不出现 σ² = 0）。
#[allow(clippy::neg_cmp_op_on_partial_ord)] // 见下：NaN 必须落「不抑制」支
pub fn ddgi_depth_weight(distance: f32, mu: f32, sigma2: f32) -> f32 {
    if !(distance > mu) {
        return 1.0;
    }
    let excess = distance - mu;
    sigma2 / (sigma2 + excess * excess)
}

/// 探针 `probe` 在方向 `delta`（着色点 − 探针）、距离 `dist` 上的深度权重
/// ——**WGSL `ddgi_depth_weight` 的完整 CPU 镜像**（含两道守卫）。
///
/// 守卫的取值中性论证：
/// - `enabled = false` ⇒ 恒 1（对照模式 `Renderer::set_ddgi_depth_weight_enabled`，
///   漏光抑制有效性断言的必要开关）；
/// - `!(dist > 0)` ⇒ 恒 1：着色点即探针位置（`delta` 退化成零向量、归一化
///   得 NaN），此时「无遮挡证据」是唯一正确语义；同时挡住 NaN 进
///   [`ddgi_depth_texel`]。径向窗对 NaN 距离已给 0（R83 的 `!(d < r)` 约定）
///   ⇒ 乘积恒 0，本支的取值不影响结果。
#[allow(clippy::neg_cmp_op_on_partial_ord)] // 见下：NaN 的 dist 必须落「不抑制」支
pub fn ddgi_depth_weight_at(
    depth: &[[f32; 2]],
    probe: usize,
    delta: Vec3,
    dist: f32,
    grid: u32,
    enabled: bool,
) -> f32 {
    if !enabled || !(dist > 0.0) {
        return 1.0;
    }
    let texel = ddgi_depth_texel(delta / dist, grid);
    let m = depth[probe * DDGI_DEPTH_TEXELS + texel];
    ddgi_depth_weight(dist, m[0], m[1])
}

/// 逐片元 DDGI 求值的 **CPU 镜像**（WGSL：`standard.wgsl::ddgi_irradiance`）。
///
/// ```text
/// w_i   = gi_probe_weight(d_i, r_i) × ddgi_depth_weight(d_i, μ_i, σ²_i)
/// E/π   = Σ(w_i · SH_i(n)) / Σw_i     （双剪枝：radial > 0 且 w > 0 才做 9 项点积）
/// Σw ≤ ε ⇒ None                        （调用方回退 env_irradiance）
/// 结果钳 ≥ 0
/// ```
///
/// **兜底语义对 R83 的显式偏离**：mode 1 的 `Σw ≤ ε ⇒ 回退最近探针` 在
/// mode 2 **改为回退 `env_irradiance`**（天空兜底，本函数返回 `None`）。
/// 理由：回退最近探针会**绕过深度项** ⇒ 墙后片元吃到墙另一侧探针的辐照 =
/// 正是本轮要消灭的漏光。
///
/// 固定循环序 = 固定累加序 ⇒ 会话内逐位确定。空表 / 非有限位置 ⇒ `None`。
#[allow(clippy::too_many_arguments)] // 签名 = WGSL `ddgi_irradiance` 的入参镜像，不为 lint 合并
pub fn ddgi_blend(
    positions: &[Vec3],
    radii: &[f32],
    sh: &[ShCoeffs],
    depth: &[[f32; 2]],
    position: Vec3,
    normal: Vec3,
    grid: u32,
    depth_weight_enabled: bool,
) -> Option<Vec3> {
    assert!(
        positions.len() == radii.len()
            && positions.len() == sh.len()
            && depth.len() == positions.len() * DDGI_DEPTH_TEXELS,
        "DDGI 四表长度必须一致：{} / {} / {} / {}（深度表须为探针数 × {DDGI_DEPTH_TEXELS}）",
        positions.len(),
        radii.len(),
        sh.len(),
        depth.len()
    );
    if positions.is_empty() || !position.is_finite() {
        return None;
    }
    let mut acc = Vec3::ZERO;
    let mut wsum = 0.0f32;
    for (i, p) in positions.iter().enumerate() {
        let delta = position - *p;
        let d = delta.length();
        let radial = probe_influence_weight(d, radii[i]);
        if radial <= 0.0 {
            continue;
        }
        let w = radial * ddgi_depth_weight_at(depth, i, delta, d, grid, depth_weight_enabled);
        if w > 0.0 {
            acc += sh_eval(&sh[i], normal) * w;
            wsum += w;
        }
    }
    if wsum > SH_BLEND_EPS {
        Some((acc / wsum).max(Vec3::ZERO))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// 网格资源
// ---------------------------------------------------------------------------

/// 全零 SH（[`DdgiGrid`] 的初值形态；粗填会立即覆盖）。
const ZERO_SH: ShCoeffs = [[0.0; 3]; SH_COEFFS];

/// DDGI 观测统计（**不进任何排序键**）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DdgiStats {
    /// 最近一次 tick 重照明的探针数（0 = 稳态）。渲染器把它抄进
    /// `FrameStats::ddgi_updated`。
    pub last_updated: u32,
    /// 网格探针总数（0 = 未安装 / 空几何）。
    pub probe_count: u32,
    /// 当前脏探针数（收敛进度 = `⌈dirty / k⌉` 帧）。
    pub dirty_count: u32,
    /// 累计 tick 次数（含稳态空转）。
    pub ticks: u64,
    /// 确定性抽稀步长 k（1 = 未抽稀）。
    pub stride: u32,
    /// 有效间距（米）= `spacing × stride`。
    pub spacing_eff: f32,
}

/// DDGI 探针网格（World 资源；`DdgiPlugin` 在 Startup 插入空网格）。
///
/// 探针**不落实体**（纯资源态）：避免 256 实体的 ECS 开销与 `&mut World`
/// spawn 限制（`&World` 的系统无法注册新组件类型）。R89 的编辑器放置入口
/// 再议实体化。
///
/// **跨帧持久状态**：`sh` / `depth` 是上一帧的产物（滚动刷新**不清空** ⇒
/// 无闪烁）；`probe_stamp` 是逐探针的「最后一次被重照明的 tick 号」，渲染器
/// 据此做**增量上传**（只写本帧变化的探针段）。
#[derive(Clone, Debug, Default, iris_core::Resource)]
pub struct DdgiGrid {
    positions: Vec<Vec3>,
    radii: Vec<f32>,
    sh: Vec<ShCoeffs>,
    /// 长度 = `positions.len() × DDGI_DEPTH_TEXELS`，元素 = `(μ, σ²)`。
    depth: Vec<[f32; 2]>,
    dirty: Vec<bool>,
    cursor: usize,
    /// 逐探针「最后一次被重照明的 tick 号」（增量上传的比对键）。
    probe_stamp: Vec<u64>,
    bvh: Bvh,
    installed: bool,
    geometry_generation: u64,
    grid_fp: u64,
    relight_fp: u64,
    rays_per_face: u32,
    /// 网格结构版本：每次 [`Self::install`] 递增 ⇒ 渲染器全量上传 + 重同步
    /// stamp 记账。
    structure_generation: u64,
    /// tick 计数（兼作 `probe_stamp` 的时钟）。
    tick_count: u64,
    stats: DdgiStats,
}

impl DdgiGrid {
    /// 空网格（未安装；渲染器视为 DDGI 缺席）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 探针数（0 = 未安装 / 空几何）。
    pub fn probe_count(&self) -> usize {
        self.positions.len()
    }

    /// 网格是否为空（渲染器的三态裁决键：非空 ⇒ mode = 2）。
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    pub fn positions(&self) -> &[Vec3] {
        &self.positions
    }

    pub fn radii(&self) -> &[f32] {
        &self.radii
    }

    /// 逐探针 SH 系数（**E/π 直读**，与 binding 18 现布局同口径）。
    pub fn sh(&self) -> &[ShCoeffs] {
        &self.sh
    }

    /// 深度两阶矩表（`(μ, σ²)`，长度 = 探针数 × [`DDGI_DEPTH_TEXELS`]）。
    pub fn depth(&self) -> &[[f32; 2]] {
        &self.depth
    }

    pub fn dirty(&self) -> &[bool] {
        &self.dirty
    }

    /// 当前轮转游标。
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// 逐探针的上传比对键（渲染器增量上传用）。
    pub fn probe_stamp(&self) -> &[u64] {
        &self.probe_stamp
    }

    pub fn structure_generation(&self) -> u64 {
        self.structure_generation
    }

    pub fn geometry_generation(&self) -> u64 {
        self.geometry_generation
    }

    pub fn grid_fp(&self) -> u64 {
        self.grid_fp
    }

    /// 已建 BVH（重照明与单测对照用）。
    pub fn bvh(&self) -> &Bvh {
        &self.bvh
    }

    pub fn stats(&self) -> DdgiStats {
        self.stats
    }

    /// 是否需要（重）建网格：未安装 / 几何 generation 变 / 布局指纹变。
    ///
    /// 宿主据此决定是否调用
    /// [`collect_bake_triangles`](crate::bake::collect_bake_triangles)——
    /// 稳态帧一次都不收集（限制④「不自动侦测」的成本面）。
    pub fn needs_rebuild(&self, settings: &DdgiSettings) -> bool {
        !self.installed
            || self.geometry_generation != settings.geometry_generation
            || self.grid_fp != grid_fingerprint(settings)
    }

    /// 单探针的 GPU 上传载荷（binding 18 的 160 B 段）。
    pub fn probe_raw(&self, index: usize) -> ShProbeRaw {
        ShProbeRaw::new(self.positions[index], self.radii[index], &self.sh[index])
    }

    /// 全表 GPU 载荷（首次启用 / 结构变化时的全量上传）。
    pub fn gpu_probes(&self) -> Vec<ShProbeRaw> {
        (0..self.positions.len())
            .map(|i| self.probe_raw(i))
            .collect()
    }

    /// 单探针的深度段（binding 20 的 [`DDGI_DEPTH_PROBE_BYTES`] 段，增量上传用）。
    pub fn depth_segment(&self, index: usize) -> &[f32] {
        let range = index * DDGI_DEPTH_TEXELS..(index + 1) * DDGI_DEPTH_TEXELS;
        bytemuck::cast_slice(&self.depth[range])
    }

    /// 全表深度（f32 展平，长度 = 探针数 × [`DDGI_DEPTH_FLOATS`]）。
    pub fn depth_floats(&self) -> &[f32] {
        bytemuck::cast_slice(&self.depth)
    }

    /// 世界位置 → 实例载荷（xyz = E/π、w = 标志位）——渲染器注入口径。
    ///
    /// 与 R83 带 SH 的探针集**同口径**（[`sh_blend_l0`] 的零阶径向混合 +
    /// 标志位对任何有限位置恒 1）：这份 xyz 只被 **mode = 0 的路径**消费
    /// （探针捕获 / RT 相机恒 mode 0，§8.52⑦ 同款立场），主视图在 mode = 2
    /// 时只读它的标志位（= 「本实体的间接漫反射走探针而非光照贴图」）。
    /// 空网格 / 非有限位置 ⇒ `[0; 4]`（走原全局 env 路径，逐位不变）。
    pub fn instance_payload(&self, position: Vec3) -> [f32; 4] {
        if !position.is_finite() || self.positions.is_empty() {
            return [0.0; 4];
        }
        match sh_blend_l0(&self.positions, &self.radii, &self.sh, position) {
            Some(v) => [v.x, v.y, v.z, 1.0],
            None => [0.0; 4],
        }
    }

    /// 安装 / 重建网格：布局 → 半径 → BVH → **粗填**（同步一次）。
    ///
    /// `triangles` 由宿主收集
    /// （[`collect_bake_triangles`](crate::bake::collect_bake_triangles)），
    /// 既是 BVH 的输入也是 `bounds = None` 时的 AABB 来源。空几何 +
    /// `bounds = None` ⇒ 网格空（视为 DDGI 缺席）。
    ///
    /// 副作用：`structure_generation += 1`（⇒ 渲染器全量上传 + 重同步 stamp
    /// 记账）、全表标脏、cursor 归零、`k > 1` 时 `log::warn!` 一次（报 k 与
    /// 有效间距）。
    pub fn install(
        &mut self,
        settings: &DdgiSettings,
        params: &BakeParams,
        triangles: Vec<BakeTriangle>,
    ) {
        settings.validate();
        self.installed = true;
        self.tick_count += 1;
        self.structure_generation += 1;
        self.geometry_generation = settings.geometry_generation;
        self.grid_fp = grid_fingerprint(settings);
        self.relight_fp = relight_fingerprint(params, settings.rays_per_face);
        self.rays_per_face = settings.rays_per_face;
        self.cursor = 0;
        self.bvh = build_bvh(triangles.clone());
        let bounds = match settings.bounds {
            Some(b) => Some(b),
            None => default_bounds(&triangles, settings.spacing),
        };
        let Some((min, max)) = bounds else {
            self.clear_probes();
            return;
        };
        let layout = plan_layout(min, max, settings.spacing, MAX_IRRADIANCE_PROBES);
        if layout.stride > 1 {
            log::warn!(
                "DDGI 规则网格 {}×{}×{} = {} 点超出探针表容量 {MAX_IRRADIANCE_PROBES}，\
                 已确定性抽稀（步长 k = {}，有效间距 {} m ⇒ {}×{}×{} = {} 点）",
                layout.counts[0],
                layout.counts[1],
                layout.counts[2],
                layout.counts[0] * layout.counts[1] * layout.counts[2],
                layout.stride,
                layout.spacing_eff,
                layout.counts_eff[0],
                layout.counts_eff[1],
                layout.counts_eff[2],
                layout.total
            );
        }
        let positions = layout_positions(&layout);
        let n = positions.len();
        // 解析半径（决策 16）：规则网格的最近邻恒为 spacing_eff ⇒ 不跑
        // O(n²) 的 derive_probe_radii；单测断言二者在该点云上逐位一致。
        let radius = Self::analytic_radius(layout.spacing_eff);
        // 粗填（决策 9）：安装帧同步一次，只填 SH、深度置中性。
        let bvh = std::mem::take(&mut self.bvh);
        let mut sh = vec![ZERO_SH; n];
        for (i, p) in positions.iter().enumerate() {
            sh[i] = probe_relight(&bvh, *p, params, DDGI_PREFILL_GRID).0;
        }
        self.bvh = bvh;
        self.positions = positions;
        self.radii = vec![radius; n];
        self.sh = sh;
        self.depth = vec![[DDGI_SKY_DEPTH, 0.0]; n * DDGI_DEPTH_TEXELS];
        self.dirty = vec![true; n];
        self.probe_stamp = vec![self.tick_count; n];
        self.stats = DdgiStats {
            last_updated: 0,
            probe_count: n as u32,
            dirty_count: n as u32,
            ticks: self.tick_count,
            stride: layout.stride as u32,
            spacing_eff: layout.spacing_eff,
        };
    }

    /// 清空探针（保留 BVH 与 generation 记账 ⇒ 不会每帧重试收集）。
    fn clear_probes(&mut self) {
        self.positions.clear();
        self.radii.clear();
        self.sh.clear();
        self.depth.clear();
        self.dirty.clear();
        self.probe_stamp.clear();
        self.stats = DdgiStats {
            last_updated: 0,
            probe_count: 0,
            dirty_count: 0,
            ticks: self.tick_count,
            stride: 1,
            spacing_eff: 0.0,
        };
    }

    /// 单帧滚动重照明：返回本帧重照明的探针数。
    ///
    /// 顺序：①指纹比对（重照明口径变 ⇒ 全表标脏，**不清空旧值**）→
    /// ②[`advance`] 取本帧窗口 → ③**无脏探针 ⇒ 立即 return**（决策 8：
    /// 静止场景 + 恒定光照的稳态 CPU 与上传成本 = 0）→ ④逐探针
    /// [`probe_relight`] + [`sh_hysteresis`] / [`depth_hysteresis`] 写回。
    ///
    /// 未安装 / 空网格 ⇒ 0（宿主应先 [`Self::install`]）。
    pub fn tick(&mut self, settings: &DdgiSettings, params: &BakeParams) -> usize {
        settings.validate();
        self.tick_count += 1;
        let n = self.positions.len();
        if n == 0 {
            self.stats.ticks = self.tick_count;
            self.stats.last_updated = 0;
            return 0;
        }
        let fp = relight_fingerprint(params, settings.rays_per_face);
        if fp != self.relight_fp {
            self.relight_fp = fp;
            self.dirty.iter_mut().for_each(|d| *d = true);
        }
        if settings.rays_per_face != self.rays_per_face {
            self.rays_per_face = settings.rays_per_face;
            self.dirty.iter_mut().for_each(|d| *d = true);
        }
        let (updated, cursor) = advance(self.cursor, &self.dirty, settings.probes_per_frame, n);
        if updated.is_empty() {
            self.stats.ticks = self.tick_count;
            self.stats.last_updated = 0;
            self.stats.dirty_count = self.dirty.iter().filter(|d| **d).count() as u32;
            return 0;
        }
        let alpha = settings.alpha();
        let grid = settings.rays_per_face;
        // BVH 临时取出：重照明只读它，写回的是 sh / depth / dirty（同一
        // &mut self 内的不相交字段无法直接分裂借用，mem::take 只搬两个
        // Vec 的胖指针，零内存拷贝）
        let bvh = std::mem::take(&mut self.bvh);
        for &i in &updated {
            let (fresh, moments) = probe_relight(&bvh, self.positions[i], params, grid);
            self.sh[i] = sh_hysteresis(&self.sh[i], &fresh, alpha);
            let base = i * DDGI_DEPTH_TEXELS;
            for (t, m) in moments.iter().enumerate() {
                self.depth[base + t] = depth_hysteresis(self.depth[base + t], *m, alpha);
            }
            self.dirty[i] = false;
            self.probe_stamp[i] = self.tick_count;
        }
        self.bvh = bvh;
        self.cursor = cursor;
        self.stats.ticks = self.tick_count;
        self.stats.last_updated = updated.len() as u32;
        self.stats.dirty_count = self.dirty.iter().filter(|d| **d).count() as u32;
        updated.len()
    }

    /// 全表标脏（宿主显式失效用；不清空旧值 ⇒ 无闪烁）。
    pub fn invalidate_all(&mut self) {
        self.dirty.iter_mut().for_each(|d| *d = true);
        self.stats.dirty_count = self.dirty.len() as u32;
    }

    /// 半径的**解析式**（决策 16）：`spacing_eff × PROBE_RADIUS_NEIGHBOR_FACTOR`。
    ///
    /// 单测断言它与 [`derive_probe_radii`](crate::irradiance::derive_probe_radii)
    /// 在规则点云上逐位一致（防口径漂移）。
    pub fn analytic_radius(spacing_eff: f32) -> f32 {
        spacing_eff * PROBE_RADIUS_NEIGHBOR_FACTOR
    }
}

/// 中性深度整表（`probes × DDGI_DEPTH_TEXELS`；μ = 天空哨兵 ⇒ 权重恒 1）。
///
/// 单测与快照的对照构造用（生产路径由 [`DdgiGrid::install`] 自己填）。
pub fn neutral_depth_table(probes: usize) -> Vec<[f32; 2]> {
    vec![[DDGI_SKY_DEPTH, 0.0]; probes * DDGI_DEPTH_TEXELS]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::irradiance::{PROBE_AXES, SH_CUBE_GRID, derive_probe_radii, probe_sh_coefficients};
    use iris_core::prelude::Entity;

    fn bits3(v: Vec3) -> [u32; 3] {
        [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()]
    }

    fn sh_bits(c: &ShCoeffs) -> Vec<u32> {
        c.iter()
            .flat_map(|v| v.iter().map(|f| f.to_bits()))
            .collect()
    }

    fn depth_bits(d: &[[f32; 2]]) -> Vec<u32> {
        d.iter()
            .flat_map(|m| [m[0].to_bits(), m[1].to_bits()])
            .collect()
    }

    fn test_params() -> BakeParams {
        BakeParams {
            light_dir: Vec3::new(-0.4, -1.0, 0.2).normalize(),
            light_color: Vec3::new(1.0, 0.9, 0.8) * 2.0,
            sky_color: Vec3::new(0.15, 0.2, 0.3),
            shadow_softness: 0.0,
            sky_samples: 1,
            bounces: 1,
        }
    }

    /// 矩形面片 → 两个烘焙三角（法线按环绕方向，`probe_ray_radiance_depth`
    /// 会翻向射线来向 ⇒ 双面都受光）。
    fn quad(a: Vec3, b: Vec3, c: Vec3, d: Vec3, albedo: Vec3) -> Vec<BakeTriangle> {
        let normal = (b - a).cross(c - a).normalize();
        vec![
            BakeTriangle {
                vertices: [a, b, c],
                normal,
                albedo,
                entity: Entity::from_bits(1),
            },
            BakeTriangle {
                vertices: [a, c, d],
                normal,
                albedo,
                entity: Entity::from_bits(1),
            },
        ]
    }

    /// 20 m 见方地板 + 一面 x = 0 的隔墙（把空间分成两半）⇒ 深度项有真实
    /// 的遮挡信号可测（§8.52① 串味的最小复现场景）。
    fn room_triangles() -> Vec<BakeTriangle> {
        let mut out = quad(
            Vec3::new(-10.0, 0.0, -10.0),
            Vec3::new(-10.0, 0.0, 10.0),
            Vec3::new(10.0, 0.0, 10.0),
            Vec3::new(10.0, 0.0, -10.0),
            Vec3::new(0.8, 0.75, 0.7),
        );
        out.extend(quad(
            Vec3::new(0.0, 0.0, -6.0),
            Vec3::new(0.0, 4.0, -6.0),
            Vec3::new(0.0, 4.0, 6.0),
            Vec3::new(0.0, 0.0, 6.0),
            Vec3::splat(0.6),
        ));
        out
    }

    fn test_settings() -> DdgiSettings {
        DdgiSettings {
            spacing: 2.0,
            bounds: Some((Vec3::new(-6.0, 0.5, -6.0), Vec3::new(6.0, 3.5, 6.0))),
            probes_per_frame: 4,
            rays_per_face: 8,
            ..DdgiSettings::default()
        }
    }

    fn installed_grid(settings: &DdgiSettings) -> DdgiGrid {
        let mut grid = DdgiGrid::new();
        grid.install(settings, &test_params(), room_triangles());
        grid
    }

    // ------------------------------------------------------------------
    // ① 格点数与格心解析解
    // ------------------------------------------------------------------

    #[test]
    fn grid_points_and_centers_are_analytic() {
        let layout = plan_layout(
            Vec3::new(-6.0, 0.5, -6.0),
            Vec3::new(6.0, 3.5, 6.0),
            2.0,
            MAX_IRRADIANCE_PROBES,
        );
        assert_eq!(
            layout.counts,
            [6, 1, 6],
            "y span 3.0 / 2.0 = 1.5 ⇒ floor = 1"
        );
        assert_eq!(layout.stride, 1, "36 点 ≤ 256 ⇒ 不抽稀");
        assert_eq!(layout.counts_eff, [6, 1, 6]);
        assert_eq!(layout.spacing_eff, 2.0);
        assert_eq!(layout.total, 36);
        let positions = layout_positions(&layout);
        assert_eq!(positions.len(), 36);
        // 格心解析解 = min + (i + 0.5) × spacing_eff
        assert_eq!(positions[0], Vec3::new(-5.0, 1.5, -5.0));
        assert_eq!(positions[1], Vec3::new(-3.0, 1.5, -5.0), "x 是最快轴");
        assert_eq!(positions[6], Vec3::new(-5.0, 1.5, -3.0), "z 是第二快轴");
        assert_eq!(positions[35], Vec3::new(5.0, 1.5, 5.0));
        // 覆盖：首末格心各留半个有效间距
        assert_eq!(positions[0].x - layout.min.x, 1.0);
        assert_eq!(6.0 - positions[35].x, 1.0);
        // 索引式 index = ix + nx·(iy + ny·iz) 与三重循环序一致
        for iz in 0..6 {
            for ix in 0..6 {
                let index = ix + 6 * iz;
                assert_eq!(
                    positions[index],
                    Vec3::new(-5.0 + 2.0 * ix as f32, 1.5, -5.0 + 2.0 * iz as f32),
                    "下标 {index} 必须对应 (ix = {ix}, iy = 0, iz = {iz})"
                );
            }
        }
        // 单轴点数解析解
        assert_eq!(axis_points(12.0, 2.0), 6);
        assert_eq!(axis_points(3.0, 2.0), 1, "floor(1.5) = 1");
        assert_eq!(axis_points(0.0, 2.0), 1, "扁平 bounds ⇒ 1 点");
        assert_eq!(axis_points(1.9, 2.0), 1, "floor(0.95) = 0 ⇒ max(1, ·)");
        assert_eq!(axis_points(f32::NAN, 2.0), 1, "NaN ⇒ 1 点（f32::max 吸收）");
        // 空 span ⇒ 每轴 1 点、格心 = min + 0.5·spacing
        let flat = plan_layout(Vec3::ZERO, Vec3::ZERO, 2.0, MAX_IRRADIANCE_PROBES);
        assert_eq!(flat.counts, [1, 1, 1]);
        assert_eq!(layout_positions(&flat), vec![Vec3::splat(1.0)]);
        // 倒置 bounds 按 span = 0 处理（合法性由 settings.validate 把关）
        let inverted = plan_layout(Vec3::ONE, Vec3::ZERO, 2.0, MAX_IRRADIANCE_PROBES);
        assert_eq!(inverted.counts, [1, 1, 1]);
    }

    // ------------------------------------------------------------------
    // ② 抽稀 k 的最小性（穷举）
    // ------------------------------------------------------------------

    #[test]
    fn stride_for_capacity_is_minimal() {
        // 穷举 1..=40 的立方网格：k 必须满足容量，且 k−1 必须不满足（最小性）
        for n in 1..=40usize {
            let k = stride_for_capacity(n, n, n, 256);
            let product = |kk: usize| n.div_ceil(kk).pow(3);
            assert!(
                product(k) <= 256,
                "n = {n}：k = {k} 未满足容量（积 {}）",
                product(k)
            );
            if k > 1 {
                assert!(
                    product(k - 1) > 256,
                    "n = {n}：k = {k} 不是最小的（k−1 的积 {} 已满足）",
                    product(k - 1)
                );
            }
            // 解析对照：⌈n/k⌉³ ≤ 256 ⇔ ⌈n/k⌉ ≤ 6 ⇔ k ≥ ⌈n/6⌉
            assert_eq!(k, n.div_ceil(6), "n = {n} 的最小 k 必须 = ⌈n/6⌉");
        }
        // 非立方例子
        assert_eq!(stride_for_capacity(4, 4, 16, 256), 1, "4×4×16 = 256 恰好");
        assert_eq!(stride_for_capacity(4, 4, 17, 256), 2, "4×4×17 = 272 > 256");
        assert_eq!(
            stride_for_capacity(1, 1, 1000, 256),
            4,
            "⌈1000/4⌉ = 250 ≤ 256，k=3 ⇒ 334 > 256"
        );
        assert_eq!(stride_for_capacity(3, 5, 7, 1), 7, "容量 1 ⇒ k = max(n)");
        assert_eq!(stride_for_capacity(1, 1, 1, 256), 1);
        // 病态大值不 panic（饱和 + checked_mul 吸收）
        let huge = stride_for_capacity(usize::MAX, usize::MAX, usize::MAX, 256);
        assert!(
            usize::MAX.div_ceil(huge).pow(3) <= 256,
            "usize::MAX 的立方网格必须被抽稀到容量内（k = {huge}）"
        );
    }

    // ------------------------------------------------------------------
    // ③ 抽稀后覆盖范围不变（对比「按序截断」会砍掉远端一整块空间）
    // ------------------------------------------------------------------

    #[test]
    fn decimation_preserves_coverage() {
        // 40 m 见方 / 0.5 m 间距 ⇒ 80×1×80 = 6400 点，抽稀到 ≤ 256
        let layout = plan_layout(
            Vec3::new(-20.0, 0.0, -20.0),
            Vec3::new(20.0, 0.5, 20.0),
            0.5,
            MAX_IRRADIANCE_PROBES,
        );
        assert_eq!(layout.counts, [80, 1, 80]);
        assert_eq!(
            layout.stride, 5,
            "⌈80/5⌉² = 256 恰好；k=4 ⇒ 20² = 400 > 256"
        );
        assert_eq!(layout.counts_eff, [16, 1, 16]);
        assert_eq!(layout.total, 256);
        assert_eq!(layout.spacing_eff, 2.5, "= 0.5 × 5");
        let positions = layout_positions(&layout);
        assert_eq!(positions.len(), 256);
        // 覆盖：首末格心各留半个**有效**间距 ⇒ 远端没被砍掉
        let half = layout.spacing_eff * 0.5;
        let first = positions[0];
        let last = positions[positions.len() - 1];
        assert_eq!(first, Vec3::new(-18.75, 1.25, -18.75));
        assert_eq!(last, Vec3::new(18.75, 1.25, 18.75));
        assert!(
            (first.x + 20.0 - half).abs() < 1e-4,
            "x 起点覆盖：{first:?}"
        );
        assert!((20.0 - last.x - half).abs() < 1e-4, "x 终点覆盖：{last:?}");
        assert!(
            (first.z + 20.0 - half).abs() < 1e-4,
            "z 起点覆盖：{first:?}"
        );
        assert!((20.0 - last.z - half).abs() < 1e-4, "z 终点覆盖：{last:?}");
        // 网格仍规则无洞：相邻点距恒为 spacing_eff（行末折回下一 z 行）
        let nx = layout.counts_eff[0];
        for i in 0..positions.len() - 1 {
            let d = positions[i + 1] - positions[i];
            let expected = if (i + 1) % nx == 0 {
                Vec3::new(
                    -(nx as f32 - 1.0) * layout.spacing_eff,
                    0.0,
                    layout.spacing_eff,
                )
            } else {
                Vec3::new(layout.spacing_eff, 0.0, 0.0)
            };
            assert!(
                (d - expected).length() < 1e-4,
                "点 {i} → {} 的步长必须规则：{d:?} vs {expected:?}",
                i + 1
            );
        }
    }

    // ------------------------------------------------------------------
    // ④ 解析半径 ≡ derive_probe_radii 逐位（决策 16 的防漂移看门）
    // ------------------------------------------------------------------

    #[test]
    fn analytic_radius_matches_derive_probe_radii_bitwise() {
        // 逐位可精确表示的构型：spacing 2.0、min 在原点 ⇒ 格心 1, 3, 5, 7，
        // 相邻差恰为 2.0（平方与开方都不引入舍入）
        for (max, spacing) in [
            (Vec3::new(8.0, 8.0, 8.0), 2.0f32),
            (Vec3::new(16.0, 4.0, 16.0), 4.0),
            (Vec3::new(64.0, 4.0, 64.0), 1.0), // 触发抽稀（k = 4）
        ] {
            let layout = plan_layout(Vec3::ZERO, max, spacing, MAX_IRRADIANCE_PROBES);
            let positions = layout_positions(&layout);
            let analytic = DdgiGrid::analytic_radius(layout.spacing_eff);
            let derived = derive_probe_radii(&positions, &vec![0.0; positions.len()]);
            assert_eq!(derived.len(), positions.len());
            for (i, r) in derived.iter().enumerate() {
                assert_eq!(
                    r.to_bits(),
                    analytic.to_bits(),
                    "spacing {spacing}（k = {}）探针 {i}：解析半径 {analytic} 必须与 \
                     derive_probe_radii 的 {r} 逐位一致（位置 {:?}）",
                    layout.stride,
                    positions[i]
                );
            }
            assert_eq!(
                analytic,
                layout.spacing_eff * 1.5,
                "PROBE_RADIUS_NEIGHBOR_FACTOR = 1.5"
            );
        }
    }

    // ------------------------------------------------------------------
    // ⑤ advance 轮转序解析对照（k=1 / k=n / k>n / 无脏 / 全脏 / 稀疏 / 回绕）
    // ------------------------------------------------------------------

    #[test]
    fn advance_rotation_matches_analytic() {
        let n = 8;
        let dirty = vec![true; n];
        // k = 1：逐帧推进一格，n 帧回绕
        let mut cursor = 0;
        for step in 0..n {
            let (updated, next) = advance(cursor, &dirty, 1, n);
            assert_eq!(updated, vec![step], "第 {step} 帧必须只更新下标 {step}");
            cursor = next;
            assert_eq!(cursor, (step + 1) % n, "cursor = 被更新者的下一位");
        }
        assert_eq!(cursor, 0, "n 帧后必须回绕到 0");
        // k = 3：窗口 [0,1,2] → [3,4,5] → [6,7,0]（环形扫描至多 n 个位置，
        // 全脏时尾部**照样绕回来**取满 k 个——不是「不足 k 就少取」）
        let (u0, c0) = advance(0, &dirty, 3, n);
        assert_eq!(u0, vec![0, 1, 2]);
        assert_eq!(c0, 3);
        let (u1, c1) = advance(c0, &dirty, 3, n);
        assert_eq!(u1, vec![3, 4, 5]);
        assert_eq!(c1, 6);
        let (u2, c2) = advance(c1, &dirty, 3, n);
        assert_eq!(u2, vec![6, 7, 0], "环形绕回取满 k");
        assert_eq!(c2, 1, "cursor = 最后被更新者（0）的下一位");
        // 稀疏脏时才会「不足 k」：只有两个脏探针 ⇒ 扫遍全表取 2 个
        let mut two = vec![false; n];
        two[6] = true;
        two[7] = true;
        assert_eq!(
            advance(6, &two, 3, n),
            (vec![6, 7], 0),
            "不足 k ⇒ 只取剩下的"
        );
        // k = n：一次全表
        assert_eq!(advance(0, &dirty, n, n), ((0..n).collect::<Vec<_>>(), 0));
        // k > n：截到 n（不重复更新）
        assert_eq!(
            advance(0, &dirty, n * 3 + 5, n),
            ((0..n).collect::<Vec<_>>(), 0),
            "k > n 必须截到 n"
        );
        // 环形跨越游标回绕
        assert_eq!(advance(6, &dirty, 4, n), (vec![6, 7, 0, 1], 2));
        // 无脏 ⇒ cursor 不动、返回空（稳态零操作的前提）
        let clean = vec![false; n];
        for start in 0..n {
            assert_eq!(
                advance(start, &clean, 4, n),
                (Vec::new(), start),
                "无脏 ⇒ cursor 不动"
            );
        }
        // 稀疏脏：扫遍全表只取脏的，cursor 落在最后一个脏探针的下一位
        let mut sparse = vec![false; n];
        sparse[2] = true;
        sparse[5] = true;
        assert_eq!(advance(0, &sparse, 4, n), (vec![2, 5], 6));
        assert_eq!(
            advance(6, &sparse, 4, n),
            (vec![2, 5], 6),
            "环形再扫一圈同样两个"
        );
        assert_eq!(
            advance(3, &sparse, 1, n),
            (vec![5], 6),
            "游标在 2 之后 ⇒ 先命中 5"
        );
        // 退化入参
        assert_eq!(advance(0, &[], 4, 0), (Vec::new(), 0));
        assert_eq!(advance(3, &clean, 0, n), (Vec::new(), 3), "k = 0 ⇒ 零操作");
        assert_eq!(
            advance(99, &dirty, 2, n),
            (vec![7, 0], 1),
            "cursor 越界 ⇒ 钳到 n−1 再环形扫描"
        );
        // 纯函数确定性：双跑逐位
        assert_eq!(advance(3, &sparse, 2, n), advance(3, &sparse, 2, n));
    }

    // ------------------------------------------------------------------
    // ⑥ 收敛帧数 = ⌈n/k⌉
    // ------------------------------------------------------------------

    #[test]
    fn convergence_frame_count_is_ceil_n_over_k() {
        for (n, k) in [
            (1usize, 1usize),
            (8, 1),
            (8, 3),
            (8, 7),
            (8, 8),
            (8, 99),
            (7, 2),
            (36, 4),
            (256, 2),
        ] {
            let mut remaining = vec![true; n];
            let mut cursor = 0;
            let mut frames = 0usize;
            loop {
                let (updated, next) = advance(cursor, &remaining, k, n);
                if updated.is_empty() {
                    break;
                }
                assert!(
                    updated.len() == k.min(n) || frames == n.div_ceil(k) - 1,
                    "n = {n} k = {k} 第 {frames} 帧：窗口宽度必须是 k（尾帧除外）"
                );
                for i in &updated {
                    assert!(remaining[*i], "不得重复更新已干净的探针");
                    remaining[*i] = false;
                }
                cursor = next;
                frames += 1;
                assert!(frames <= n, "n = {n} k = {k}：帧数失控（{frames}）");
            }
            assert_eq!(
                frames,
                n.div_ceil(k),
                "n = {n}、k = {k} 的收敛帧数必须 = ⌈n/k⌉"
            );
            assert_eq!(remaining.iter().filter(|d| **d).count(), 0, "必须全表收敛");
        }
        // 默认参数的账：256 探针 / k = 2 ⇒ 128 帧 ≈ 2.1 s @60fps
        assert_eq!(256usize.div_ceil(2), 128);
    }

    // ------------------------------------------------------------------
    // ⑦ 粗填非零 + 深度中性
    // ------------------------------------------------------------------

    #[test]
    fn prefill_is_nonzero_with_neutral_depth() {
        let settings = test_settings();
        let grid = installed_grid(&settings);
        let n = grid.probe_count();
        assert_eq!(n, 36, "12 m 见方 / 2 m 间距、y span 3 / 2 = 1 层 ⇒ 6×1×6");
        assert_eq!(DDGI_PREFILL_GRID, 2, "粗填档 = 6 × 2² = 24 方向/探针");
        // SH 非零（不是「安装帧一片黑波」）且有限
        let mut nonzero = 0;
        for c in grid.sh() {
            assert_eq!(c.len(), SH_COEFFS);
            assert!(
                c.iter().all(|v| v.iter().all(|f| f.is_finite())),
                "系数必须有限：{c:?}"
            );
            if c[0] != [0.0; 3] {
                nonzero += 1;
            }
        }
        assert_eq!(nonzero, n, "全部探针的 L0 都必须被粗填成非零");
        // 深度中性 ⇒ chebyshev 恒 1（不抑制）
        assert_eq!(grid.depth().len(), n * DDGI_DEPTH_TEXELS);
        for m in grid.depth() {
            assert_eq!(m[0].to_bits(), DDGI_SKY_DEPTH.to_bits(), "μ 必须是天空哨兵");
            assert_eq!(
                m[1].to_bits(),
                0.0f32.to_bits(),
                "σ² 必须是 0（粗填不写深度）"
            );
        }
        // 全表标脏 + cursor 归零 ⇒ 滚动精修会逐步替换
        assert_eq!(grid.dirty().iter().filter(|d| **d).count(), n);
        assert_eq!(grid.stats().dirty_count as usize, n);
        assert_eq!(grid.stats().probe_count as usize, n);
        assert_eq!(grid.stats().stride, 1);
        assert_eq!(grid.stats().spacing_eff, 2.0);
        assert_eq!(grid.cursor(), 0);
        assert_eq!(grid.structure_generation(), 1, "首次安装 ⇒ 结构版本 1");
        assert_eq!(grid.probe_stamp().len(), n);
        // 中性深度下「开深度项」与「关深度项」逐位同值（不抑制的另一种表述）
        let p = Vec3::new(1.0, 1.0, 1.0);
        let with = ddgi_blend(
            grid.positions(),
            grid.radii(),
            grid.sh(),
            grid.depth(),
            p,
            Vec3::Y,
            DDGI_DEPTH_GRID,
            true,
        );
        let without = ddgi_blend(
            grid.positions(),
            grid.radii(),
            grid.sh(),
            grid.depth(),
            p,
            Vec3::Y,
            DDGI_DEPTH_GRID,
            false,
        );
        assert_eq!(with.map(bits3), without.map(bits3), "中性深度必须不抑制");
        assert!(with.is_some(), "网格内的点必须有有效探针");
        // 实例载荷：有限位置 ⇒ 标志位 1；NaN ⇒ 全零
        assert_eq!(grid.instance_payload(p)[3], 1.0);
        assert_eq!(grid.instance_payload(Vec3::NAN), [0.0; 4]);
    }

    // ------------------------------------------------------------------
    // ⑧ 跨模块等价锚点②：grid = SH_CUBE_GRID ⇒ 逐位 ≡ probe_sh_coefficients
    // ------------------------------------------------------------------

    #[test]
    fn relight_at_cube_grid_matches_probe_sh_coefficients_bitwise() {
        let bvh = build_bvh(room_triangles());
        let params = test_params();
        assert_eq!(SH_CUBE_GRID, 32, "锚点的档位前提");
        for p in [
            Vec3::new(-3.0, 1.5, 2.0),
            Vec3::new(3.0, 1.5, -2.0),
            Vec3::new(0.5, 2.5, 0.5),
        ] {
            let (ddgi, depth) = probe_relight(&bvh, p, &params, SH_CUBE_GRID);
            let r83 = probe_sh_coefficients(&bvh, p, &params);
            assert_eq!(
                sh_bits(&ddgi),
                sh_bits(&r83),
                "位置 {p:?}：rays_per_face = SH_CUBE_GRID 时 DDGI 的 SH 表必须逐位等于 \
                 probe_sh_coefficients（等价锚点②，同时证明 probe_sh_radiance 改薄壳未漂移）"
            );
            // 非平凡性：系数不是全零（否则「逐位相等」是恒等假通过）
            assert!(
                ddgi[0].iter().any(|&v| v != 0.0),
                "位置 {p:?} 的 L0 必须非零"
            );
            // 深度表有真实信号
            assert_eq!(depth.len(), DDGI_DEPTH_TEXELS);
            assert!(
                depth.iter().all(|m| m[0].is_finite()),
                "全部纹素的 μ 必须有限（天空哨兵而非 inf）"
            );
            assert!(
                depth.iter().any(|m| m[0] < DDGI_SKY_DEPTH as f64),
                "位置 {p:?}：房间几何必须有命中方向（否则深度项无信号可测）"
            );
            assert!(
                depth.iter().all(|m| m[1] > 0.0),
                "σ² 必须被下限钳成非零（防硬阶跃）"
            );
        }
    }

    // ------------------------------------------------------------------
    // ⑨ 等价锚点①：α = 0 且单帧 k ≥ n ⇒ 逐位 ≡ 全量重照明一遍
    // ------------------------------------------------------------------

    #[test]
    fn single_frame_full_relight_matches_rolling_bitwise() {
        let settings = DdgiSettings {
            probes_per_frame: usize::MAX, // k > n ⇒ 一次全表
            rays_per_face: 8,
            hysteresis: 0.0,
            ..test_settings()
        };
        let mut grid = installed_grid(&settings);
        let n = grid.probe_count();
        assert_eq!(
            grid.tick(&settings, &test_params()),
            n,
            "k > n 必须一帧全表重照明"
        );
        assert_eq!(grid.stats().dirty_count, 0, "全表重照明后无脏探针");
        assert_eq!(grid.stats().last_updated as usize, n);
        // 解析对照：直接逐探针跑一遍 probe_relight（同一 BVH、同一参数）
        let bvh = grid.bvh().clone();
        let params = test_params();
        for i in 0..n {
            let (fresh, moments) = probe_relight(&bvh, grid.positions()[i], &params, 8);
            assert_eq!(
                sh_bits(&grid.sh()[i]),
                sh_bits(&fresh),
                "探针 {i}：α = 0 的滚动结果必须逐位等于全量重照明（等价锚点①）"
            );
            for (t, m) in moments.iter().enumerate() {
                let got = grid.depth()[i * DDGI_DEPTH_TEXELS + t];
                assert_eq!(
                    got[0].to_bits(),
                    (m[0] as f32).to_bits(),
                    "探针 {i} 纹素 {t} 的 μ"
                );
                assert_eq!(
                    got[1].to_bits(),
                    (m[1] as f32).to_bits(),
                    "探针 {i} 纹素 {t} 的 σ²"
                );
            }
        }
        // 粗填值确实被替换掉了（不是「粗填 == 精修」的恒等假通过）
        let mut coarse = DdgiGrid::new();
        coarse.install(&settings, &params, room_triangles());
        assert_ne!(
            depth_bits(coarse.depth()),
            depth_bits(grid.depth()),
            "精修必须真的改写深度表（粗填是中性的）"
        );
    }

    // ------------------------------------------------------------------
    // ⑩⑪⑫ hysteresis：α=0 纯替换 / α=1 冻结 / α=0.5 解析中点
    // ------------------------------------------------------------------

    #[test]
    fn hysteresis_alpha_endpoints_and_midpoint() {
        let bvh = build_bvh(room_triangles());
        let params = test_params();
        let old = probe_relight(&bvh, Vec3::new(-3.0, 1.5, 2.0), &params, 8).0;
        let fresh = probe_relight(&bvh, Vec3::new(3.0, 1.5, -2.0), &params, 8).0;
        assert_ne!(
            sh_bits(&old),
            sh_bits(&fresh),
            "两侧数据必须不同（否则断言无意义）"
        );
        // α = 0 ⇒ 逐位等于纯替换（决策 17）
        assert_eq!(sh_bits(&sh_hysteresis(&old, &fresh, 0.0)), sh_bits(&fresh));
        // α = 1 ⇒ 逐位冻结
        assert_eq!(sh_bits(&sh_hysteresis(&old, &fresh, 1.0)), sh_bits(&old));
        // α = 0.5 ⇒ 解析中点（容差 1e-6 相对：f32 乘加的两步舍入）
        let half = sh_hysteresis(&old, &fresh, 0.5);
        for k in 0..SH_COEFFS {
            for c in 0..3 {
                let expected = 0.5 * (old[k][c] + fresh[k][c]);
                let tol = 1e-6 * expected.abs().max(1.0);
                assert!(
                    (half[k][c] - expected).abs() <= tol,
                    "α = 0.5 必须是解析中点：系数 {k}[{c}] {} vs {expected}（容差 {tol}）",
                    half[k][c]
                );
            }
        }
        // α = 0.25 ⇒ 解析加权（一阶递推的线性性）
        let quarter = sh_hysteresis(&old, &fresh, 0.25);
        for k in 0..SH_COEFFS {
            for c in 0..3 {
                let expected = 0.25 * old[k][c] + 0.75 * fresh[k][c];
                assert!(
                    (quarter[k][c] - expected).abs() <= 1e-6 * expected.abs().max(1.0),
                    "α = 0.25 的解析加权：系数 {k}[{c}]"
                );
            }
        }
        // 深度矩同款（μ / σ² 各线性混合，解析精确）
        let d_old = [1.0f32, 4.0];
        let d_fresh = [3.0f64, 16.0];
        assert_eq!(
            depth_hysteresis(d_old, d_fresh, 0.0),
            [3.0, 16.0],
            "α=0 ⇒ 纯替换"
        );
        assert_eq!(
            depth_hysteresis(d_old, d_fresh, 1.0),
            [1.0, 4.0],
            "α=1 ⇒ 冻结"
        );
        assert_eq!(
            depth_hysteresis(d_old, d_fresh, 0.5),
            [2.0, 10.0],
            "α=0.5 ⇒ 中点"
        );
        assert_eq!(
            depth_hysteresis(d_old, d_fresh, 0.25),
            [1.0 * 0.25 + 3.0 * 0.75, 4.0 * 0.25 + 16.0 * 0.75],
            "α=0.25 ⇒ old×α + fresh×(1−α) = [2.5, 13.0]"
        );
        // settings 的钳位：NaN ⇒ 0、越界 ⇒ 钳到端点
        assert_eq!(
            DdgiSettings {
                hysteresis: f32::NAN,
                ..DdgiSettings::default()
            }
            .alpha(),
            0.0
        );
        assert_eq!(
            DdgiSettings {
                hysteresis: -3.0,
                ..DdgiSettings::default()
            }
            .alpha(),
            0.0
        );
        assert_eq!(
            DdgiSettings {
                hysteresis: 7.0,
                ..DdgiSettings::default()
            }
            .alpha(),
            1.0
        );
        assert_eq!(DdgiSettings::default().alpha(), 0.0, "默认关（决策 17）");
    }

    /// α > 0 时表确实被时域滤波（不是一帧就跳到新值）。
    #[test]
    fn hysteresis_slows_convergence_when_enabled() {
        let base = DdgiSettings {
            probes_per_frame: usize::MAX,
            rays_per_face: 8,
            ..test_settings()
        };
        let params = test_params();
        let mut pure = installed_grid(&base);
        pure.tick(&base, &params);
        let filtered_settings = DdgiSettings {
            hysteresis: 0.5,
            ..base
        };
        let mut filtered = installed_grid(&filtered_settings);
        let coarse = filtered.sh().to_vec();
        filtered.tick(&filtered_settings, &params);
        // α = 0.5 的结果必须严格落在「粗填值」与「精修值」之间（不等于任一端）
        let mut strictly_between = 0;
        for (i, coarse_probe) in coarse.iter().enumerate() {
            for (k, coarse_coef) in coarse_probe.iter().enumerate() {
                for (c, &start) in coarse_coef.iter().enumerate() {
                    let target = pure.sh()[i][k][c];
                    let got = filtered.sh()[i][k][c];
                    if (target - start).abs() > 1e-6 {
                        let lo = target.min(start);
                        let hi = target.max(start);
                        assert!(
                            got >= lo - 1e-6 && got <= hi + 1e-6,
                            "α = 0.5 必须落在两端之间：探针 {i} 系数 {k}[{c}] = {got}，\
                             区间 [{lo}, {hi}]"
                        );
                        assert_ne!(got.to_bits(), target.to_bits(), "不得等于纯替换的结果");
                        strictly_between += 1;
                    }
                }
            }
        }
        assert!(
            strictly_between > 0,
            "α = 0.5 必须真的在滤波（否则断言无意义）"
        );
    }

    // ------------------------------------------------------------------
    // ⑬ chebyshev 权重解析解
    // ------------------------------------------------------------------

    #[test]
    fn chebyshev_weight_matches_analytic_solution() {
        let (mu, sigma2) = (10.0f32, 4.0);
        // dist ≤ μ ⇒ 恒 1（逐位）
        for d in [0.0f32, 1.0, 5.0, 9.999, 10.0] {
            assert_eq!(
                ddgi_depth_weight(d, mu, sigma2).to_bits(),
                1.0f32.to_bits(),
                "dist = {d} ≤ μ = {mu} 必须给出权重 1"
            );
        }
        // dist > μ ⇒ σ²/(σ² + (dist−μ)²) 的解析解
        for d in [11.0f32, 12.0, 14.0, 20.0, 110.0] {
            let expected = sigma2 / (sigma2 + (d - mu) * (d - mu));
            let got = ddgi_depth_weight(d, mu, sigma2);
            assert!(
                (got - expected).abs() < 1e-6,
                "dist = {d}：{got} vs 解析解 {expected}"
            );
        }
        // dist = μ + σ ⇒ 恰 0.5（chebyshev 的半高点）
        assert!(
            (ddgi_depth_weight(mu + sigma2.sqrt(), mu, sigma2) - 0.5).abs() < 1e-6,
            "dist = μ + σ 必须给出 0.5"
        );
        // 单调不增且趋 0
        let mut prev = 1.0f32;
        for i in 0..40 {
            let w = ddgi_depth_weight(mu + 0.25 * i as f32, mu, sigma2);
            assert!(
                w <= prev,
                "权重必须单调不增（第 {i} 档 {w} > 前一档 {prev}）"
            );
            prev = w;
        }
        assert!(
            ddgi_depth_weight(1.0e6, mu, sigma2) < 1e-5,
            "dist ≫ μ 必须趋 0（墙后片元被抑制）"
        );
        // σ² = 0 且 dist > μ ⇒ 退化为硬 0（下限钳的存在理由）
        assert_eq!(ddgi_depth_weight(10.5, mu, 0.0), 0.0);
        // NaN 语义：dist 为 NaN ⇒ 落「无遮挡证据」支（保守：不抑制）
        assert_eq!(ddgi_depth_weight(f32::NAN, mu, sigma2), 1.0);
        assert_eq!(ddgi_depth_weight(f32::NAN, f32::NAN, sigma2), 1.0);
        // 相对下限钳的量化：σ² = (DDGI_SIGMA_REL·μ)² ⇒ dist = 1.1μ 处权重恰 0.5、
        // 1.3μ 处 ≈0.1（单射线 ⇒ 样本方差 0 ⇒ 整条 σ² 都是钳出来的，这正是
        // 防硬阶跃的用途）
        let rel = f64::from(DDGI_SIGMA_REL);
        let clamped = depth_moment_finish(100.0, 100.0 * 100.0, 1);
        assert_eq!(clamped[0], 100.0, "μ = Σt/1");
        assert_eq!(
            clamped[1],
            (rel * 100.0).powi(2),
            "零方差 ⇒ σ² 必须被钳到 (DDGI_SIGMA_REL × μ)²"
        );
        let w_110 = ddgi_depth_weight(110.0, clamped[0] as f32, clamped[1] as f32);
        assert!(
            (w_110 - 0.5).abs() < 1e-3,
            "超出 μ 一成 ⇒ 权重 ≈ 0.5，实得 {w_110}"
        );
        let w_130 = ddgi_depth_weight(130.0, clamped[0] as f32, clamped[1] as f32);
        assert!(
            (w_130 - 0.1).abs() < 1e-2,
            "超出 μ 三成 ⇒ 权重 ≈ 0.1（衰减带宽度与探针间距同量级），实得 {w_130}"
        );
    }

    // ------------------------------------------------------------------
    // ⑭ σ² 下限钳 + f64 求差（灾难性抵消的量化依据）
    // ------------------------------------------------------------------

    #[test]
    fn sigma_variance_floor_and_f64_differencing() {
        assert_eq!(
            depth_moment_finish(0.0, 0.0, 0),
            neutral_depth_moment(),
            "计数 0 ⇒ 中性"
        );
        // 相对系数在 f64 域的口径（`f64::from(0.1f32)` ≠ 字面量 0.1，差 1.5e-9
        // ⇒ 钳值必须按同一口径算，否则「精确相等」的断言会假失败）
        let rel = f64::from(DDGI_SIGMA_REL);
        // 单射线（零样本方差）⇒ 钳到 (DDGI_SIGMA_REL·μ)²，不是 0
        let m = depth_moment_finish(5.0, 25.0, 1);
        assert_eq!(m[0], 5.0, "μ = Σt / 1");
        assert_eq!(
            m[1],
            (rel * 5.0).powi(2),
            "σ² 钳到 (0.1 × 5)² ≈ 0.25（防硬阶跃）"
        );
        assert!(m[1] > 0.0, "钳后必须为正（否则 chebyshev 退化成硬阶跃）");
        // 真实方差大于下限 ⇒ 用真实值：t ∈ {3, 5} ⇒ μ = 4、E[t²] = 17、σ² = 1
        let m = depth_moment_finish(8.0, 34.0, 2);
        assert_eq!(m[0], 4.0);
        assert_eq!(
            m[1], 1.0,
            "σ² = E[t²] − μ² = 17 − 16 = 1（未被钳，1 > (0.4)²）"
        );
        // 灾难性抵消的量化：μ = 1e4、σ = 1e-3 ⇒ f32 求差归零，f64 保住信号
        let mu = 1.0e4f64;
        let tiny = 1.0e-3f64;
        let sum_t = 2.0 * mu;
        let sum_t2 = (mu - tiny).powi(2) + (mu + tiny).powi(2);
        let m = depth_moment_finish(sum_t, sum_t2, 2);
        assert_eq!(m[0], mu, "μ = Σt/2 = 1e4");
        let f32_var = {
            let mu32 = (sum_t as f32) / 2.0;
            let e32 = (sum_t2 as f32) / 2.0;
            (e32 - mu32 * mu32).max(0.0)
        };
        assert_eq!(f32_var, 0.0, "前提校验：f32 求差必须灾难性抵消成 0");
        assert_eq!(
            m[1],
            (f64::from(DDGI_SIGMA_REL) * mu).powi(2),
            "真实 σ² = 1e-6 ≪ (0.1 × 1e4)² = 1e6 ⇒ 钳到下限（远天空纹素的常态）"
        );
        assert!(
            m[1] > 0.0,
            "钳后 σ² 必须为正（否则 chebyshev 退化成硬阶跃）"
        );
        // 舍入残余的负方差被 .max(0.0) 吸收后再钳
        let m = depth_moment_finish(4.0, 15.999_999_999, 1);
        assert!(m[1] >= 0.0, "σ² 必须非负：{}", m[1]);
        assert_eq!(
            m[1],
            (rel * 4.0).powi(2),
            "负方差被吸收成 0 后钳到 (0.1 × 4)²"
        );
        // 中性矩 ⇒ 权重恒 1（不抑制）
        let neutral = neutral_depth_moment();
        assert_eq!(neutral[0], f64::from(DDGI_SKY_DEPTH));
        assert_eq!(neutral[1], 0.0);
        for d in [0.0f32, 1.0, 100.0, 9999.0] {
            assert_eq!(
                ddgi_depth_weight(d, neutral[0] as f32, neutral[1] as f32),
                1.0
            );
        }
    }

    // ------------------------------------------------------------------
    // ⑮ 纹素往返：cube_face_direction ↔ ddgi_depth_texel 双射
    // ------------------------------------------------------------------

    #[test]
    fn depth_texel_roundtrip_is_identity() {
        let grid = DDGI_DEPTH_GRID;
        let step = 2.0 / grid as f32;
        let mut seen = vec![false; SH_CUBE_FACES * DDGI_DEPTH_TEXELS_PER_FACE];
        for face in 0..SH_CUBE_FACES {
            for row in 0..grid {
                for col in 0..grid {
                    // 纹素中心（与 probe_relight 的中点规则同式）
                    let u = -1.0 + step * (col as f32 + 0.5);
                    let v = -1.0 + step * (row as f32 + 0.5);
                    let dir = cube_face_direction(face, u, v);
                    let texel = ddgi_depth_texel(dir, grid);
                    let expected = face * DDGI_DEPTH_TEXELS_PER_FACE
                        + row as usize * grid as usize
                        + col as usize;
                    assert_eq!(
                        texel, expected,
                        "面 {face} 纹素 ({row},{col}) 的中心方向 {dir:?} 必须回到同一格（得到 {texel}）"
                    );
                    assert!(!seen[texel], "纹素 {texel} 被命中两次 ⇒ 映射不是双射");
                    seen[texel] = true;
                    assert_eq!(
                        ddgi_depth_face(dir),
                        face,
                        "方向 {dir:?} 的面判定必须是 {face}"
                    );
                }
            }
        }
        assert!(
            seen.iter().all(|&s| s),
            "全部 {} 个纹素必须被覆盖（无重复 + 无遗漏 = 双射）",
            seen.len()
        );
        // 六轴主方向落在面心纹素（t = 0 ⇒ cell = floor(grid/2)）
        for (face, axis) in PROBE_AXES.iter().enumerate() {
            let texel = ddgi_depth_texel(*axis, grid);
            let half = grid as usize / 2;
            assert_eq!(
                texel,
                face * DDGI_DEPTH_TEXELS_PER_FACE + half * grid as usize + half,
                "主轴 {axis:?} 必须落在面 {face} 的面心纹素"
            );
        }
    }

    /// 并列主轴取 [`PROBE_AXES`] 序低者 + 局部轴约定与 `cube_face_direction` 一致。
    #[test]
    fn depth_face_tiebreak_follows_probe_axes_order() {
        assert_eq!(
            ddgi_depth_face(Vec3::new(1.0, 1.0, 1.0).normalize()),
            0,
            "|x|=|y|=|z| ⇒ +X"
        );
        assert_eq!(
            ddgi_depth_face(Vec3::new(-1.0, 1.0, 1.0).normalize()),
            1,
            "负 x ⇒ −X"
        );
        assert_eq!(
            ddgi_depth_face(Vec3::new(0.0, 1.0, 1.0).normalize()),
            2,
            "|y|=|z| ⇒ +Y"
        );
        assert_eq!(
            ddgi_depth_face(Vec3::new(0.0, -1.0, 1.0).normalize()),
            3,
            "−Y"
        );
        assert_eq!(ddgi_depth_face(Vec3::new(0.0, 0.0, 1.0)), 4, "+Z");
        assert_eq!(ddgi_depth_face(Vec3::new(0.0, 0.0, -1.0)), 5, "−Z");
        assert_eq!(
            ddgi_depth_face(Vec3::new(1.0, 2.0, 0.0)),
            2,
            "|y| 最大 ⇒ +Y"
        );
        assert_eq!(
            ddgi_depth_face(Vec3::new(1.0, -2.0, 0.0)),
            3,
            "|y| 最大且负 ⇒ −Y"
        );
        assert_eq!(
            ddgi_depth_face(Vec3::new(0.5, 0.4, 0.9)),
            4,
            "|z| 最大 ⇒ +Z"
        );
        // 局部轴约定（±Y 面：u → x、v → z）：u = 0.5 ⇒ col = 6，v = −0.25 ⇒ row = 3
        let dir = cube_face_direction(2, 0.5, -0.25);
        assert!(dir.y > 0.0 && dir.x > 0.0 && dir.z < 0.0, "{dir:?}");
        let texel = ddgi_depth_texel(dir, DDGI_DEPTH_GRID);
        assert_eq!(texel / DDGI_DEPTH_TEXELS_PER_FACE, 2);
        assert_eq!(
            texel % DDGI_DEPTH_TEXELS_PER_FACE,
            3 * DDGI_DEPTH_GRID as usize + 6,
            "u → col（floor((0.5+1)·0.5·8) = 6）、v → row（floor(0.375·8) = 3）"
        );
        // 边界钳位：|t| 略超主轴分量（数值噪声）不得越界
        let edge = Vec3::new(1.0, 1.0000002, 0.0);
        let texel = ddgi_depth_texel(edge, DDGI_DEPTH_GRID);
        assert!(
            texel < SH_CUBE_FACES * DDGI_DEPTH_TEXELS_PER_FACE,
            "越界：{texel}"
        );
        // 未归一化方向同结果（只用比值与符号）
        let scaled = ddgi_depth_texel(Vec3::new(0.3, -1.0, 0.2), DDGI_DEPTH_GRID);
        let normalized = ddgi_depth_texel(Vec3::new(0.3, -1.0, 0.2).normalize(), DDGI_DEPTH_GRID);
        assert_eq!(
            scaled, normalized,
            "归一化与否必须同格（WGSL 传 delta/dist）"
        );
    }

    // ------------------------------------------------------------------
    // ⑯ CPU 累积格 ↔ WGSL 查表格同格（逐格穷举，全部合法档位）
    // ------------------------------------------------------------------

    #[test]
    fn depth_texel_matches_accumulation_cell() {
        for grid in DDGI_RAYS_PER_FACE_CHOICES {
            assert_eq!(grid % DDGI_DEPTH_GRID, 0, "档位 {grid} 必须能被 8 整除");
            let sub = grid / DDGI_DEPTH_GRID;
            let step = 2.0 / grid as f32;
            for face in 0..SH_CUBE_FACES {
                for row in 0..grid {
                    for col in 0..grid {
                        let u = -1.0 + step * (col as f32 + 0.5);
                        let v = -1.0 + step * (row as f32 + 0.5);
                        let dir = cube_face_direction(face, u, v);
                        let looked_up = ddgi_depth_texel(dir, DDGI_DEPTH_GRID);
                        let accumulated = face * DDGI_DEPTH_TEXELS_PER_FACE
                            + depth_cell_of_ray(row, grid) as usize * DDGI_DEPTH_GRID as usize
                            + depth_cell_of_ray(col, grid) as usize;
                        assert_eq!(
                            looked_up, accumulated,
                            "档位 {grid}、面 {face}、射线格 ({row},{col})：查表格与累积格必须同格"
                        );
                        assert_eq!(row / sub, depth_cell_of_ray(row, grid));
                        assert_eq!(col / sub, depth_cell_of_ray(col, grid));
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // ⑰ 深度表的 GPU 索引式（(probe × 384 + texel) × 2 + moment）
    // ------------------------------------------------------------------

    #[test]
    fn depth_table_layout_matches_gpu_index_formula() {
        assert_eq!(DDGI_DEPTH_TEXELS, 384, "6 × 8²");
        assert_eq!(DDGI_DEPTH_FLOATS, 768);
        assert_eq!(DDGI_DEPTH_PROBE_BYTES, 3072);
        assert_eq!(DDGI_DEPTH_TABLE_BYTES, 786_432, "= 768 KiB");
        let settings = DdgiSettings {
            spacing: 4.0,
            bounds: Some((Vec3::ZERO, Vec3::new(8.0, 4.0, 8.0))),
            rays_per_face: 8,
            ..test_settings()
        };
        let mut grid = installed_grid(&settings);
        grid.tick(
            &DdgiSettings {
                probes_per_frame: usize::MAX,
                ..settings
            },
            &test_params(),
        );
        let n = grid.probe_count();
        assert_eq!(grid.depth().len(), n * DDGI_DEPTH_TEXELS);
        assert_eq!(grid.depth_floats().len(), n * DDGI_DEPTH_FLOATS);
        for i in 0..n {
            let segment = grid.depth_segment(i);
            assert_eq!(segment.len(), DDGI_DEPTH_FLOATS, "每探针段必须 768 个 f32");
            for texel in [0usize, 36, 100, 383] {
                for moment in [0usize, 1] {
                    let flat = (i * DDGI_DEPTH_TEXELS + texel) * 2 + moment;
                    assert_eq!(
                        grid.depth_floats()[flat].to_bits(),
                        segment[texel * 2 + moment].to_bits(),
                        "探针 {i} 纹素 {texel} 矩 {moment}：展平索引式必须与分段视图同值"
                    );
                    assert_eq!(
                        grid.depth_floats()[flat].to_bits(),
                        grid.depth()[i * DDGI_DEPTH_TEXELS + texel][moment].to_bits()
                    );
                }
            }
        }
        // GPU 载荷（binding 18）与 CPU 表同值
        let raws = grid.gpu_probes();
        assert_eq!(raws.len(), n);
        for (i, raw) in raws.iter().enumerate() {
            assert_eq!(*raw, grid.probe_raw(i));
            assert_eq!(raw.pos_radius[3], grid.radii()[i], "半径必须原样上传");
            for k in 0..SH_COEFFS {
                assert_eq!(raw.sh[k][..3], grid.sh()[i][k], "SH 必须原样上传");
                assert_eq!(raw.sh[k][3], 0.0, "w 分量保留 0");
            }
        }
    }

    // ------------------------------------------------------------------
    // ⑱ WGSL 源码层看门
    // ------------------------------------------------------------------

    #[test]
    fn wgsl_ddgi_source_matches_cpu_contract() {
        let src = include_str!("../shaders/standard.wgsl");
        // binding 20 = 深度表（f32 标量数组，决策 6）
        assert!(
            src.contains("@group(0) @binding(20) var<storage, read> gi_depth: array<f32>;"),
            "standard.wgsl 必须声明 binding 20 的 f32 标量深度表"
        );
        // 每探针纹素数常数（索引式的契约面）
        let key = "const DDGI_DEPTH_TEXELS: u32 = ";
        let at = src
            .find(key)
            .unwrap_or_else(|| panic!("standard.wgsl 缺少 DDGI_DEPTH_TEXELS"));
        let literal = src[at + key.len()..]
            .split(';')
            .next()
            .expect("字面量必须有分号");
        // WGSL 的无符号字面量带 `u` 后缀（384u），Rust 的 parse 不认
        let wgsl: u32 = literal
            .trim()
            .trim_end_matches('u')
            .parse()
            .expect("DDGI_DEPTH_TEXELS 必须是整数字面量（可带 u 后缀）");
        assert_eq!(
            wgsl as usize, DDGI_DEPTH_TEXELS,
            "DDGI_DEPTH_TEXELS 两侧必须同值：WGSL {literal} vs CPU {DDGI_DEPTH_TEXELS}"
        );
        // 四个函数都在（缺一个 ⇒ mode 2 分支编译不过）
        for f in [
            "fn ddgi_depth_face(",
            "fn ddgi_depth_texel(",
            "fn ddgi_depth_weight(",
            "fn ddgi_irradiance(",
        ] {
            assert!(src.contains(f), "standard.wgsl 缺少 {f}");
        }
        // chebyshev 数值核必须同式同序
        assert!(
            src.contains("return sigma2 / (sigma2 + excess * excess);"),
            "standard.wgsl 的 chebyshev 必须与 CPU 侧 ddgi_depth_weight 同式同序"
        );
        assert!(
            src.contains("if (!(dist > mu)) {"),
            "「无遮挡证据」支必须用 !(dist > mu)（NaN 落此支，与 CPU 同义）"
        );
        // 索引式 (probe × 384 + texel) × 2 + moment
        assert!(
            src.contains("(probe * DDGI_DEPTH_TEXELS + texel) * 2u"),
            "standard.wgsl 的深度索引式必须与 CPU 展平序同式"
        );
        // 纹素式 face × grid² + row × grid + col
        assert!(
            src.contains("return face * grid * grid + row * grid + col;"),
            "standard.wgsl 的纹素式必须与 CPU 侧 ddgi_depth_texel 同式"
        );
        // 兜底语义：mode 2 的 Σw ≤ ε 回退 env（决策 11，不回退最近探针）
        assert!(
            src.contains("in.probe.w > 0.5 && ddgi.w > 0.5"),
            "mode 2 必须用 ddgi.w 作有效标志回退 env_irradiance（不回退最近探针）"
        );
        assert!(
            src.contains("if (wsum > GI_BLEND_EPS)"),
            "mode 2 必须沿用 GI_BLEND_EPS（不新立混合下限常数）"
        );
        // mode 门三支：0 / 1 / 2（R64 与 R83 的语句逐条不动）
        assert!(src.contains("if (gi_params.params.x < 0.5) {"), "mode 0 门");
        assert!(
            src.contains("} else if (gi_params.params.x < 1.5) {"),
            "mode 1 门"
        );
        assert!(
            src.contains("irradiance = select(env_irradiance, in.probe.rgb, in.probe.w > 0.5);"),
            "mode 0 分支的语句必须与 R64 逐条相同"
        );
        assert!(
            src.contains("gi_sh_irradiance(in.world_pos, n), in.probe.w > 0.5);"),
            "mode 1 分支的语句必须与 R83 逐条相同"
        );
        // 径向窗沿用 R83 的 gi_probe_weight（不另写一份）
        assert!(
            src.contains("let radial = gi_probe_weight(d, p.pos_radius.w);"),
            "mode 2 必须复用 R83 的径向窗 gi_probe_weight"
        );
    }

    // ------------------------------------------------------------------
    // ⑲ 指纹位组合（稳定 + 敏感 + 不消费的字段不入指纹）
    // ------------------------------------------------------------------

    #[test]
    fn fingerprints_are_bitwise_stable_and_sensitive() {
        let params = test_params();
        let a = relight_fingerprint(&params, 16);
        assert_eq!(
            relight_fingerprint(&params, 16),
            a,
            "同输入两次必须逐位一致"
        );
        assert_ne!(a, 0, "指纹不得退化成常数 0");
        let mut dir = params;
        dir.light_dir = Vec3::new(0.4, -1.0, 0.2).normalize();
        assert_ne!(relight_fingerprint(&dir, 16), a, "light_dir 变 ⇒ 指纹变");
        let mut color = params;
        color.light_color = params.light_color * 1.0000001;
        assert_ne!(
            relight_fingerprint(&color, 16),
            a,
            "light_color 变 ⇒ 指纹变"
        );
        let mut sky = params;
        sky.sky_color = params.sky_color * 1.0000001;
        assert_ne!(relight_fingerprint(&sky, 16), a, "sky_color 变 ⇒ 指纹变");
        let mut bounces = params;
        bounces.bounces = 0;
        assert_ne!(relight_fingerprint(&bounces, 16), a, "bounces 变 ⇒ 指纹变");
        assert_ne!(
            relight_fingerprint(&params, 8),
            a,
            "rays_per_face 变 ⇒ 指纹变"
        );
        // 本路径不消费的字段不得入指纹（否则会误判全表脏）
        let mut ignored = params;
        ignored.sky_samples = 64;
        ignored.shadow_softness = 0.3;
        assert_eq!(
            relight_fingerprint(&ignored, 16),
            a,
            "sky_samples / shadow_softness 不入指纹（本路径走硬影单射线、不做半球采样）"
        );
        // 布局指纹
        let s = DdgiSettings::default();
        assert_eq!(grid_fingerprint(&s), grid_fingerprint(&s));
        assert_ne!(
            grid_fingerprint(&DdgiSettings {
                spacing: 2.0000002,
                ..s
            }),
            grid_fingerprint(&s),
            "spacing 变 ⇒ 布局指纹变"
        );
        let bounded = DdgiSettings {
            bounds: Some((Vec3::ZERO, Vec3::ONE)),
            ..s
        };
        assert_ne!(
            grid_fingerprint(&bounded),
            grid_fingerprint(&s),
            "bounds 有无 ⇒ 布局指纹变"
        );
        assert_ne!(
            grid_fingerprint(&DdgiSettings {
                bounds: Some((Vec3::ZERO, Vec3::splat(1.0000001))),
                ..bounded
            }),
            grid_fingerprint(&bounded),
            "bounds 值变 ⇒ 布局指纹变"
        );
        assert_ne!(
            grid_fingerprint(&DdgiSettings {
                bounds: Some((Vec3::splat(0.5), Vec3::ONE)),
                ..bounded
            }),
            grid_fingerprint(&bounded),
            "bounds 的 min 变 ⇒ 布局指纹变"
        );
        // 不影响布局的字段 ⇒ 布局指纹不变
        let other = DdgiSettings {
            probes_per_frame: 99,
            rays_per_face: 32,
            hysteresis: 0.5,
            bounces: 0,
            geometry_generation: 77,
            sky_color: Some(Vec3::ONE),
            ..s
        };
        assert_eq!(
            grid_fingerprint(&other),
            grid_fingerprint(&s),
            "布局指纹只吃 spacing + bounds（其余由 relight 指纹 / generation 管）"
        );
    }

    // ------------------------------------------------------------------
    // ⑳ generation 失效 ⇒ 全表标脏 + BVH 重建
    // ------------------------------------------------------------------

    #[test]
    fn geometry_generation_invalidates_and_rebuilds_bvh() {
        let settings = test_settings();
        let mut grid = installed_grid(&settings);
        assert!(!grid.needs_rebuild(&settings), "刚装好不该重建");
        let full = DdgiSettings {
            probes_per_frame: usize::MAX,
            ..settings
        };
        grid.tick(&full, &test_params());
        assert_eq!(grid.stats().dirty_count, 0, "一帧全表 ⇒ 已收敛");
        let structure_before = grid.structure_generation();
        let triangles_before = grid.bvh().triangle_count();
        assert_eq!(triangles_before, 4, "地板 2 + 墙 2");

        // 几何 generation 变 ⇒ 需要重建
        let bumped = DdgiSettings {
            geometry_generation: settings.geometry_generation + 1,
            ..settings
        };
        assert!(grid.needs_rebuild(&bumped), "generation 变必须触发重建");
        let mut triangles = room_triangles();
        triangles.extend(quad(
            Vec3::new(-4.0, 4.0, -4.0),
            Vec3::new(-4.0, 4.0, 4.0),
            Vec3::new(4.0, 4.0, 4.0),
            Vec3::new(4.0, 4.0, -4.0),
            Vec3::splat(0.9),
        ));
        grid.install(&bumped, &test_params(), triangles);
        assert_eq!(
            grid.bvh().triangle_count(),
            triangles_before + 2,
            "BVH 必须按新三角集重建"
        );
        assert_eq!(
            grid.structure_generation(),
            structure_before + 1,
            "结构版本必须递增（渲染器据此全量上传）"
        );
        assert_eq!(
            grid.stats().dirty_count as usize,
            grid.probe_count(),
            "重建后必须全表标脏"
        );
        assert_eq!(grid.cursor(), 0, "重建后游标归零");
        assert!(!grid.needs_rebuild(&bumped));
        // 布局指纹变（spacing）⇒ 同样重建，点数随之变
        let coarse = DdgiSettings {
            spacing: 4.0,
            ..bumped
        };
        assert!(grid.needs_rebuild(&coarse), "spacing 变必须触发重建");
        let before = grid.probe_count();
        grid.install(&coarse, &test_params(), room_triangles());
        assert!(
            grid.probe_count() < before,
            "间距翻倍 ⇒ 点数减少：{before} → {}",
            grid.probe_count()
        );
        // 空几何 + bounds = None ⇒ 网格空（视为缺席），且记账后不每帧重试
        let derived = DdgiSettings {
            bounds: None,
            ..coarse
        };
        grid.install(&derived, &test_params(), Vec::new());
        assert!(grid.is_empty(), "空几何 + 无显式 bounds ⇒ 网格空");
        assert_eq!(grid.probe_count(), 0);
        assert_eq!(grid.tick(&derived, &test_params()), 0, "空网格 tick 零操作");
        assert!(
            !grid.needs_rebuild(&derived),
            "空结果也要记账，不得每帧重试收集"
        );
        assert_eq!(grid.instance_payload(Vec3::new(1.0, 1.0, 1.0)), [0.0; 4]);
        assert!(grid.gpu_probes().is_empty());
        assert!(grid.depth_floats().is_empty());
        // 空 BVH 下重照明：全部方向未命中 ⇒ μ = 天空哨兵、σ² = 下限钳值
        let params = test_params();
        let empty = build_bvh(Vec::new());
        let (_, depth) = probe_relight(&empty, Vec3::ZERO, &params, 8);
        for m in &depth {
            assert_eq!(
                m[0],
                f64::from(DDGI_SKY_DEPTH),
                "未命中 ⇒ 天空哨兵（有限，不是 inf）"
            );
            assert_eq!(
                m[1],
                (f64::from(DDGI_SIGMA_REL) * f64::from(DDGI_SKY_DEPTH)).powi(2)
            );
        }
    }

    /// `bounds = None` 的默认范围派生：AABB 外扩 1×spacing、Y 下限额外抬 0.5×。
    #[test]
    fn default_bounds_pads_aabb_and_lifts_floor() {
        let triangles = room_triangles();
        assert_eq!(
            triangles_bounds(&triangles),
            Some((Vec3::new(-10.0, 0.0, -10.0), Vec3::new(10.0, 4.0, 10.0)))
        );
        let (min, max) = default_bounds(&triangles, 2.0).expect("非空三角集");
        assert_eq!(
            min,
            Vec3::new(-12.0, -1.0, -12.0),
            "x/z 外扩 2.0；y 外扩 2.0 后抬 1.0"
        );
        assert_eq!(max, Vec3::new(12.0, 6.0, 12.0));
        // 抬 Y 下限的语义：探针不得埋到地板（y = 0）下方超过半个间距
        assert!(
            min.y > -2.0,
            "地板下的探针会看到地板背面 ⇒ 必须抬起：{min:?}"
        );
        assert_eq!(triangles_bounds(&[]), None, "空三角集无 AABB");
        assert_eq!(
            default_bounds(&[], 2.0),
            None,
            "空几何 ⇒ 网格空（视为缺席）"
        );
    }

    // ------------------------------------------------------------------
    // ㉑ 稳态零操作（无脏 ⇒ tick 不改任何字节）
    // ------------------------------------------------------------------

    #[test]
    fn steady_state_tick_touches_nothing() {
        let settings = DdgiSettings {
            probes_per_frame: usize::MAX,
            rays_per_face: 8,
            ..test_settings()
        };
        let params = test_params();
        let mut grid = installed_grid(&settings);
        grid.tick(&settings, &params);
        assert_eq!(grid.stats().dirty_count, 0, "一帧全表 ⇒ 已收敛");
        let sh_before: Vec<u32> = grid.sh().iter().flat_map(sh_bits).collect();
        let depth_before = depth_bits(grid.depth());
        let stamps_before = grid.probe_stamp().to_vec();
        let (cursor_before, structure_before, ticks_before) = (
            grid.cursor(),
            grid.structure_generation(),
            grid.stats().ticks,
        );
        // 稳态：同参数再跑 5 帧
        for _ in 0..5 {
            assert_eq!(
                grid.tick(&settings, &params),
                0,
                "稳态必须零重照明（决策 8）"
            );
        }
        assert_eq!(
            sh_before,
            grid.sh().iter().flat_map(sh_bits).collect::<Vec<_>>(),
            "SH 表不得改动任何一位"
        );
        assert_eq!(
            depth_before,
            depth_bits(grid.depth()),
            "深度表不得改动任何一位"
        );
        assert_eq!(
            stamps_before,
            grid.probe_stamp(),
            "上传戳不得改动 ⇒ 渲染器零上传"
        );
        assert_eq!(cursor_before, grid.cursor(), "游标不得推进");
        assert_eq!(
            structure_before,
            grid.structure_generation(),
            "结构版本不得变"
        );
        assert_eq!(grid.stats().last_updated, 0);
        assert_eq!(
            grid.stats().ticks,
            ticks_before + 5,
            "tick 计数照常累加（观测口径）"
        );
        // 显式失效 ⇒ 全表标脏但**不清空旧值**（DDGI 的时域行为）
        grid.invalidate_all();
        assert_eq!(grid.stats().dirty_count as usize, grid.probe_count());
        assert_eq!(
            sh_before,
            grid.sh().iter().flat_map(sh_bits).collect::<Vec<_>>(),
            "标脏不得清空旧值"
        );
    }

    // ------------------------------------------------------------------
    // ㉒ 光照变化 ⇒ 全表标脏但旧值保留 + 滚动收敛
    // ------------------------------------------------------------------

    #[test]
    fn light_change_marks_all_dirty_without_clearing() {
        let settings = DdgiSettings {
            probes_per_frame: 4,
            rays_per_face: 8,
            ..test_settings()
        };
        let mut params = test_params();
        let mut grid = installed_grid(&settings);
        // 收敛
        let n = grid.probe_count();
        let frames = n.div_ceil(settings.probes_per_frame);
        for _ in 0..frames {
            assert!(grid.tick(&settings, &params) > 0);
        }
        assert_eq!(grid.stats().dirty_count, 0);
        assert_eq!(grid.tick(&settings, &params), 0, "收敛后稳态");
        let converged = grid.sh().to_vec();
        let stamps = grid.probe_stamp().to_vec();

        // 光照变（天空色）⇒ 全表标脏，但旧值一个字节都不清（无闪烁）
        params.sky_color *= 2.0;
        assert_eq!(
            sh_bits(&converged[0]),
            sh_bits(&grid.sh()[0]),
            "前提：表未被触碰"
        );
        assert_eq!(grid.tick(&settings, &params), settings.probes_per_frame);
        assert_eq!(
            grid.stats().dirty_count as usize,
            n - settings.probes_per_frame,
            "本帧只清 k 个脏位"
        );
        // 未被本帧重照明的探针保持旧值（这就是「不清空」的可观测面）
        let updated: Vec<bool> = grid
            .probe_stamp()
            .iter()
            .zip(&stamps)
            .map(|(a, b)| a != b)
            .collect();
        assert_eq!(
            updated.iter().filter(|u| **u).count(),
            settings.probes_per_frame
        );
        for (i, u) in updated.iter().enumerate() {
            if !u {
                assert_eq!(
                    sh_bits(&converged[i]),
                    sh_bits(&grid.sh()[i]),
                    "未重照明的探针 {i} 必须保留旧值"
                );
            } else {
                assert_ne!(
                    sh_bits(&converged[i]),
                    sh_bits(&grid.sh()[i]),
                    "重照明的探针 {i} 必须跟随新光照"
                );
            }
        }
        // ⌈n/k⌉ 帧后重新收敛，且新表的 L0 随天空色翻倍而上升
        for _ in 0..frames {
            grid.tick(&settings, &params);
        }
        assert_eq!(grid.stats().dirty_count, 0, "⌈n/k⌉ 帧后必须重新收敛");
        let mut brighter = 0;
        for (i, old) in converged.iter().enumerate() {
            if grid.sh()[i][0][0] > old[0][0] {
                brighter += 1;
            }
        }
        assert!(
            brighter * 2 > n,
            "天空色翻倍 ⇒ 多数探针的 L0 必须上升（{brighter}/{n}）"
        );
        // rays_per_face 变 ⇒ 同样全表标脏（口径变了）
        let other = DdgiSettings {
            rays_per_face: 16,
            ..settings
        };
        assert_eq!(grid.tick(&other, &params), settings.probes_per_frame);
        assert!(grid.stats().dirty_count > 0, "档位变化必须触发全表标脏");
    }

    // ------------------------------------------------------------------
    // ㉓ 双跑 to_bits（重照明 + 滚动序的确定性）
    // ------------------------------------------------------------------

    #[test]
    fn relight_and_rolling_are_bitwise_deterministic() {
        let settings = DdgiSettings {
            probes_per_frame: 3,
            rays_per_face: 8,
            ..test_settings()
        };
        let params = test_params();
        // 两次独立会话逐帧推进 12 帧，SH + 深度矩的 to_bits 序列必须完全一致
        let mut runs = Vec::new();
        for _ in 0..2 {
            let mut grid = installed_grid(&settings);
            let mut trace = Vec::new();
            for _ in 0..12 {
                let updated = grid.tick(&settings, &params);
                trace.push(updated as u64);
                trace.push(grid.cursor() as u64);
                trace.extend(grid.sh().iter().flat_map(sh_bits).map(u64::from));
                trace.extend(depth_bits(grid.depth()).into_iter().map(u64::from));
            }
            runs.push(trace);
        }
        assert_eq!(
            runs[0], runs[1],
            "双会话逐帧逐位必须一致（R60/R62b 范式的 CPU 半）"
        );
        assert!(!runs[0].is_empty());
        // 单探针重照明双跑逐位
        let bvh = build_bvh(room_triangles());
        for p in [Vec3::new(-2.0, 1.0, 1.0), Vec3::new(2.0, 1.0, -1.0)] {
            let a = probe_relight(&bvh, p, &params, 8);
            let b = probe_relight(&bvh, p, &params, 8);
            assert_eq!(
                sh_bits(&a.0),
                sh_bits(&b.0),
                "位置 {p:?} 的 SH 双跑必须逐位一致"
            );
            assert_eq!(
                a.1.iter()
                    .flat_map(|m| [m[0].to_bits(), m[1].to_bits()])
                    .collect::<Vec<_>>(),
                b.1.iter()
                    .flat_map(|m| [m[0].to_bits(), m[1].to_bits()])
                    .collect::<Vec<_>>(),
                "位置 {p:?} 的深度矩双跑必须逐位一致"
            );
        }
        // BVH 同输入两次构建逐位一致（重照明的确定性前提）
        let bvh2 = build_bvh(room_triangles());
        assert_eq!(bvh.node_count(), bvh2.node_count());
        assert_eq!(
            probe_relight(&bvh, Vec3::new(1.5, 1.5, 1.5), &params, 8).0,
            probe_relight(&bvh2, Vec3::new(1.5, 1.5, 1.5), &params, 8).0
        );
        // 换光照 ⇒ 结果变化（防「恒定假通过」）
        let dark = BakeParams {
            sky_color: Vec3::ZERO,
            light_color: Vec3::ZERO,
            ..params
        };
        assert_ne!(
            sh_bits(&probe_relight(&bvh, Vec3::new(1.5, 1.5, 1.5), &params, 8).0),
            sh_bits(&probe_relight(&bvh, Vec3::new(1.5, 1.5, 1.5), &dark, 8).0),
            "全黑光照必须给出不同的 SH"
        );
    }

    // ------------------------------------------------------------------
    // ㉔ 深度项抑制漏光（§8.52① 的 CPU 半，解析构型）
    // ------------------------------------------------------------------

    /// 手工构造的两探针 + 手写深度表：A 被墙挡（μ 小）、B 可见（μ 大）⇒
    /// 深度项开启时 A 的串味被压制到可判别的量级。
    #[test]
    fn depth_weight_suppresses_leakage_behind_wall() {
        // 「常量场」SH：E/π(n) ≡ c（只有 L0 项，d_0 = c / Y_0）
        let y0 = crate::irradiance::SH_Y0;
        let const_sh = |c: Vec3| {
            let mut out = [[0.0f32; 3]; SH_COEFFS];
            out[0] = [c.x / y0, c.y / y0, c.z / y0];
            out
        };
        let positions = vec![Vec3::new(0.0, 0.0, 0.0), Vec3::new(4.0, 0.0, 0.0)];
        let radii = vec![8.0, 8.0];
        // A = 墙另一侧的探针（红），B = 同侧的探针（绿）
        let sh = vec![
            const_sh(Vec3::new(1.0, 0.0, 0.0)),
            const_sh(Vec3::new(0.0, 1.0, 0.0)),
        ];
        let mut depth = neutral_depth_table(2);
        // A 朝 +X（着色点方向）的纹素：μ = 1（墙在 1 m 处）、σ² = 0.01
        // （表按探针分段 ⇒ 探针 1 的段偏移是 `DDGI_DEPTH_TEXELS`，探针 0 为 0）
        let texel_px = ddgi_depth_texel(Vec3::X, DDGI_DEPTH_GRID);
        depth[texel_px] = [1.0, 0.01];
        // B 朝 −X（着色点方向）的纹素：μ = 10（无遮挡）、σ² = 1
        let texel_nx = ddgi_depth_texel(Vec3::NEG_X, DDGI_DEPTH_GRID);
        depth[DDGI_DEPTH_TEXELS + texel_nx] = [10.0, 1.0];
        assert_ne!(
            texel_px / DDGI_DEPTH_TEXELS_PER_FACE,
            texel_nx / DDGI_DEPTH_TEXELS_PER_FACE
        );

        let point = Vec3::new(2.0, 0.0, 0.0); // 两探针等距（径向权同值 ⇒ 差异只来自深度项）
        let normal = Vec3::Y;
        let on = ddgi_blend(
            &positions,
            &radii,
            &sh,
            &depth,
            point,
            normal,
            DDGI_DEPTH_GRID,
            true,
        )
        .expect("网格内必须有有效探针");
        let off = ddgi_blend(
            &positions,
            &radii,
            &sh,
            &depth,
            point,
            normal,
            DDGI_DEPTH_GRID,
            false,
        )
        .expect("关掉深度项同样有效");
        println!("深度项开 {on:?} / 关 {off:?}");
        // 关掉深度项：等距等权 ⇒ 各半（R83 的串味现状）
        assert!(
            (off.x - 0.5).abs() < 1e-5 && (off.y - 0.5).abs() < 1e-5,
            "对照支必须各半：{off:?}"
        );
        // 开启深度项：A 的贡献被 chebyshev 压到 ≈0.01/(0.01+1) ≈ 0.0099
        let w_a = 0.01 / (0.01 + 1.0);
        let expected_x = w_a / (w_a + 1.0);
        assert!(
            (on.x - expected_x).abs() < 1e-5,
            "红通道必须等于 chebyshev 解析解 {expected_x}，实得 {}",
            on.x
        );
        assert!(on.y > 0.99, "绿通道必须几乎全占：{}", on.y);
        // 可判别性：漏光被压制 ≥ 20 倍（指标要在分布高端可判别，R53/R60 度量学教训）
        let ratio = off.x / on.x;
        assert!(
            ratio > 20.0,
            "墙后串味必须被压制 20 倍以上：关 {off} → 开 {on}（比值 {ratio}）"
        );
        // 单调性：着色点越深入墙后，A 的权重越低
        let mut prev = on.x;
        for x in [2.5f32, 3.0, 3.5] {
            let p = Vec3::new(x, 0.0, 0.0);
            let got = ddgi_blend(
                &positions,
                &radii,
                &sh,
                &depth,
                p,
                normal,
                DDGI_DEPTH_GRID,
                true,
            )
            .map_or(0.0, |v| v.x);
            assert!(
                got <= prev,
                "越深入墙后红通道必须不增：x = {x} 处 {got} > {prev}"
            );
            prev = got;
        }
        // 全部探针都被抑制 ⇒ Σw ≤ ε ⇒ None（回退 env，**不回退最近探针**，决策 11）
        let mut blocked = neutral_depth_table(2);
        for probe in 0..2 {
            for texel in 0..DDGI_DEPTH_TEXELS {
                blocked[probe * DDGI_DEPTH_TEXELS + texel] = [0.001, 0.0];
            }
        }
        assert_eq!(
            ddgi_blend(
                &positions,
                &radii,
                &sh,
                &blocked,
                point,
                normal,
                DDGI_DEPTH_GRID,
                true
            )
            .map(bits3),
            None,
            "全屏蔽 ⇒ 必须回退 env（None），而不是回退最近探针把漏光放回来"
        );
        // 同一构型关掉深度项 ⇒ 仍有值（证明上面的 None 来自深度项而非径向窗）
        assert!(
            ddgi_blend(
                &positions,
                &radii,
                &sh,
                &blocked,
                point,
                normal,
                DDGI_DEPTH_GRID,
                false
            )
            .is_some(),
            "关掉深度项必须有值（归因：None 只能来自 chebyshev 抑制）"
        );
    }

    // ------------------------------------------------------------------
    // ㉕ 混合的边界语义（径向窗 + 兜底 + 钳负 + 方向性）
    // ------------------------------------------------------------------

    #[test]
    fn blend_boundaries_and_directionality() {
        let y0 = crate::irradiance::SH_Y0;
        let y1 = crate::irradiance::SH_Y1;
        let mut dipole = [[0.0f32; 3]; SH_COEFFS];
        dipole[0] = [0.5 / y0, 0.5 / y0, 0.5 / y0];
        dipole[1] = [0.3 / y1, 0.0, 0.0]; // L1 的序是 y, z, x ⇒ k = 1 是 y 项
        let positions = vec![Vec3::ZERO];
        let radii = vec![4.0];
        let sh = vec![dipole];
        let depth = neutral_depth_table(1);
        // 方向性：E/π(n) = 0.5 + 0.3·n_y ⇒ 上亮下暗
        let up = ddgi_blend(
            &positions,
            &radii,
            &sh,
            &depth,
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::Y,
            DDGI_DEPTH_GRID,
            true,
        )
        .expect("球内");
        let down = ddgi_blend(
            &positions,
            &radii,
            &sh,
            &depth,
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::NEG_Y,
            DDGI_DEPTH_GRID,
            true,
        )
        .expect("球内");
        assert!(
            (up.x - 0.8).abs() < 1e-5,
            "E/π(+Y) = 0.5 + 0.3 = 0.8，实得 {}",
            up.x
        );
        assert!(
            (down.x - 0.2).abs() < 1e-5,
            "E/π(−Y) = 0.5 − 0.3 = 0.2，实得 {}",
            down.x
        );
        // 径向窗外 ⇒ None（回退 env）
        assert_eq!(
            ddgi_blend(
                &positions,
                &radii,
                &sh,
                &depth,
                Vec3::new(10.0, 0.0, 0.0),
                Vec3::Y,
                DDGI_DEPTH_GRID,
                true
            ),
            None,
            "影响球外必须回退 env（mode 2 不回退最近探针）"
        );
        // 边界上 w = 0 ⇒ 同样 None
        assert_eq!(
            ddgi_blend(
                &positions,
                &radii,
                &sh,
                &depth,
                Vec3::new(4.0, 0.0, 0.0),
                Vec3::Y,
                DDGI_DEPTH_GRID,
                true
            ),
            None,
            "d = r 的边界上径向窗为 0 ⇒ None"
        );
        // 着色点即探针位置（delta = 0 ⇒ dist = 0）⇒ 深度项守卫给 1，不 NaN
        let at_probe = ddgi_blend(
            &positions,
            &radii,
            &sh,
            &depth,
            Vec3::ZERO,
            Vec3::Y,
            DDGI_DEPTH_GRID,
            true,
        )
        .expect("dist = 0 必须仍有值");
        assert!(at_probe.is_finite(), "dist = 0 不得产出 NaN：{at_probe:?}");
        assert!(
            (at_probe.x - 0.8).abs() < 1e-5,
            "dist = 0 ⇒ 权 1 ⇒ 纯 SH 求值：{at_probe:?}"
        );
        // 空表 / 非有限位置 ⇒ None
        assert_eq!(
            ddgi_blend(
                &[],
                &[],
                &[],
                &[],
                Vec3::ZERO,
                Vec3::Y,
                DDGI_DEPTH_GRID,
                true
            ),
            None
        );
        assert_eq!(
            ddgi_blend(
                &positions,
                &radii,
                &sh,
                &depth,
                Vec3::NAN,
                Vec3::Y,
                DDGI_DEPTH_GRID,
                true
            ),
            None
        );
        // 钳 ≥ 0：负场不得泄出负值（与 R83 的出口钳同语义）
        let mut negative = [[0.0f32; 3]; SH_COEFFS];
        negative[0] = [-1.0 / y0, 0.0, 0.0];
        let got = ddgi_blend(
            &positions,
            &radii,
            &[negative],
            &depth,
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::Y,
            DDGI_DEPTH_GRID,
            true,
        )
        .expect("球内");
        assert_eq!(got, Vec3::ZERO, "混合出口必须钳 ≥ 0：{got:?}");
        // 表长度失配必须 panic（契约面）
        assert_eq!(depth.len(), DDGI_DEPTH_TEXELS);
    }

    #[test]
    #[should_panic(expected = "四表长度必须一致")]
    fn blend_rejects_mismatched_tables() {
        let positions = vec![Vec3::ZERO];
        let radii = vec![4.0];
        let sh = vec![[[0.0f32; 3]; SH_COEFFS]];
        // 深度表短一截 ⇒ panic（不静默读越界）
        let depth = neutral_depth_table(2);
        ddgi_blend(
            &positions,
            &radii,
            &sh,
            &depth,
            Vec3::ZERO,
            Vec3::Y,
            DDGI_DEPTH_GRID,
            true,
        );
    }

    // ------------------------------------------------------------------
    // ㉖ 参数校验
    // ------------------------------------------------------------------

    #[test]
    fn settings_validation_rejects_illegal_values() {
        DdgiSettings::default().validate();
        test_settings().validate();
        for grid in DDGI_RAYS_PER_FACE_CHOICES {
            DdgiSettings {
                rays_per_face: grid,
                ..DdgiSettings::default()
            }
            .validate();
        }
        // 默认值即设计稿口径
        let d = DdgiSettings::default();
        assert_eq!(d.spacing, 2.0);
        assert_eq!(d.probes_per_frame, 2);
        assert_eq!(d.rays_per_face, 16, "裁定 1 的质量档");
        assert_eq!(d.hysteresis, 0.0, "决策 17：默认关");
        assert_eq!(d.bounces, 1);
        assert_eq!(d.bounds, None);
        assert_eq!(d.sky_color, None);
        assert_eq!(d.geometry_generation, 0);
    }

    #[test]
    #[should_panic(expected = "网格间距须为正有限值")]
    fn settings_reject_bad_spacing() {
        DdgiSettings {
            spacing: 0.0,
            ..DdgiSettings::default()
        }
        .validate();
    }

    #[test]
    #[should_panic(expected = "rays_per_face 只能取")]
    fn settings_reject_rays_off_tier() {
        // 12 能被 8 整除吗？不能；48 能，但不在档位表里 ⇒ 两种非法值同一条断言
        DdgiSettings {
            rays_per_face: 12,
            ..DdgiSettings::default()
        }
        .validate();
    }

    #[test]
    #[should_panic(expected = "rays_per_face 只能取")]
    fn settings_reject_divisible_but_off_tier() {
        DdgiSettings {
            rays_per_face: 48,
            ..DdgiSettings::default()
        }
        .validate();
    }

    #[test]
    #[should_panic(expected = "只支持 0/1 次反弹")]
    fn settings_reject_two_bounces() {
        DdgiSettings {
            bounces: 2,
            ..DdgiSettings::default()
        }
        .validate();
    }

    #[test]
    #[should_panic(expected = "max 须逐分量 ≥ min")]
    fn settings_reject_inverted_bounds() {
        DdgiSettings {
            bounds: Some((Vec3::ONE, Vec3::ZERO)),
            ..DdgiSettings::default()
        }
        .validate();
    }

    // ------------------------------------------------------------------
    // ㉗ 真实重照明的深度信号（解析对照：无限地板上的探针）
    // ------------------------------------------------------------------

    #[test]
    fn relit_depths_match_analytic_floor_distances() {
        // 只有地板（y = 0）的空旷场景 ⇒ −Y 面的深度有解析解，
        // 其余朝上的面全部命中天空
        let floor = quad(
            Vec3::new(-50.0, 0.0, -50.0),
            Vec3::new(-50.0, 0.0, 50.0),
            Vec3::new(50.0, 0.0, 50.0),
            Vec3::new(50.0, 0.0, -50.0),
            Vec3::splat(0.7),
        );
        let bvh = build_bvh(floor);
        let params = test_params();
        let h = 2.0f32;
        // 深度口径的直证：`hit.t` 从**偏移后的射线原点**起算，加回偏移量才与
        // 「从探针位置起算」的着色器侧 `distance(x, p)` 同原点 ⇒ 垂直向下到
        // 地板的深度必须恰为 h（偏移往返精确抵消，不是「差 2 mm 也无所谓」）
        let (radiance, depth_down) =
            probe_ray_radiance_depth(&bvh, Vec3::new(0.0, h, 0.0), Vec3::NEG_Y, &params);
        assert!(
            (depth_down - h).abs() < 1e-5,
            "垂直向下的深度必须 = h = {h}（bias 往返抵消），实得 {depth_down}"
        );
        assert!(radiance.x > 0.0, "地板反光必须非零：{radiance:?}");
        let (sh, depth) = probe_relight(&bvh, Vec3::new(0.0, h, 0.0), &params, 8);
        // +Y 面（face 2）：全部命中天空 ⇒ μ = 哨兵、σ² = 下限钳值
        let sky_floor = (f64::from(DDGI_SIGMA_REL) * f64::from(DDGI_SKY_DEPTH)).powi(2);
        for texel in 0..DDGI_DEPTH_TEXELS_PER_FACE {
            let m = depth[2 * DDGI_DEPTH_TEXELS_PER_FACE + texel];
            assert_eq!(
                m[0],
                f64::from(DDGI_SKY_DEPTH),
                "+Y 面纹素 {texel} 必须是天空"
            );
            assert_eq!(m[1], sky_floor, "+Y 面纹素 {texel} 的 σ² 必须是下限钳值");
        }
        // −Y 面（face 3）：dir = (u, −1, v)/L（L = √(1+u²+v²)）⇒ 从高度 h 到
        // 地板的**几何**距离 = h·L。射线原点被偏移了 BAKE_SHADOW_BIAS，
        // `probe_ray_radiance_depth` 又把它加回来 ⇒ 两者**精确抵消**（这正是
        // 「深度须加回 bias」那条口径的意义：μ 与着色器侧 distance(x, p) 同原点）。
        // 单样本纹素（grid = 8 ⇒ sub = 1）⇒ μ 精确等于该式。
        let step = 2.0 / 8.0f32;
        for row in 0..8u32 {
            for col in 0..8u32 {
                let u = -1.0 + step * (col as f32 + 0.5);
                let v = -1.0 + step * (row as f32 + 0.5);
                let expected = h * (1.0 + u * u + v * v).sqrt();
                let m = depth[3 * DDGI_DEPTH_TEXELS_PER_FACE + row as usize * 8 + col as usize];
                assert!(
                    (m[0] as f32 - expected).abs() < 1e-4,
                    "−Y 面纹素 ({row},{col}) 的 μ 必须 = h·√(1+u²+v²)（bias 往返抵消）：\
                     {} vs {expected}",
                    m[0]
                );
                // 单样本 ⇒ 零方差 ⇒ 钳到 (DDGI_SIGMA_REL·μ)²（下限钳的可观测面）
                let floor = (f64::from(DDGI_SIGMA_REL) * m[0]).powi(2);
                assert!(
                    (m[1] - floor).abs() < 1e-9 * floor.max(1.0),
                    "单样本文素的 σ² 必须被钳到 (0.1μ)²：{} vs {floor}",
                    m[1]
                );
            }
        }
        // 面心纹素（|u|, |v| 最小）的 μ 最小、面角最大（角分辨率的量化面）
        let center = depth[3 * DDGI_DEPTH_TEXELS_PER_FACE + 3 * 8 + 3][0];
        let corner = depth[3 * DDGI_DEPTH_TEXELS_PER_FACE + 7 * 8 + 7][0];
        assert!(
            corner > center * 1.4,
            "面角的深度必须显著大于面心：{center} vs {corner}"
        );
        // 深度加权的实际效果：地板上的着色点（dist ≈ h）不被抑制，
        // 而「地板下方 h' 处」的假想着色点被强烈抑制
        let positions = [Vec3::new(0.0, h, 0.0)];
        let depth32: Vec<[f32; 2]> = depth.iter().map(|m| [m[0] as f32, m[1] as f32]).collect();
        let on_floor = Vec3::new(0.0, 0.0, 0.0);
        let w_floor = ddgi_depth_weight_at(
            &depth32,
            0,
            on_floor - positions[0],
            (on_floor - positions[0]).length(),
            DDGI_DEPTH_GRID,
            true,
        );
        assert!(w_floor > 0.5, "地板上的着色点不该被抑制：{w_floor}");
        let below = Vec3::new(0.0, -3.0, 0.0);
        let delta = below - positions[0];
        let w_below =
            ddgi_depth_weight_at(&depth32, 0, delta, delta.length(), DDGI_DEPTH_GRID, true);
        assert!(
            w_below < w_floor * 0.5,
            "地板下方（= 地板背面）的着色点必须被强烈抑制：{w_below} vs 地板上 {w_floor}"
        );
        // SH 非零且有限（地板反光 + 天空）
        assert!(
            sh[0].iter().all(|f| f.is_finite()),
            "系数必须有限：{:?}",
            sh[0]
        );
        assert!(
            sh[0][0] > 0.0,
            "L0 必须为正（有天空与地板反光）：{:?}",
            sh[0]
        );
    }
}
