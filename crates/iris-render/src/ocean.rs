//! 海洋 v2（R93）：CPU 频谱合成 × 预烘关键帧位移图 × 相机跟随双层海面。
//!
//! ## 语义摘要
//!
//! - 实体挂 [`OceanSurface`] 即得**无限海面**：网格锚定到相机的整数格
//!   （[`ocean_rings`]），走到哪海到哪——R81 有限矩形的「网格边缘露缝」
//!   （§8.50⑧）在海面上天然消失（边缘恒在视野外的远海）。与
//!   [`WaterSurface`](crate::water::WaterSurface) **互斥**：海洋在场走海洋路径
//!   并 `warn` 一次忽略池塘；海洋缺席则 R81 语义**逐位不变**（不烘图、不建
//!   纹理、不建管线、零 draw）。
//! - **波形 = 解析 Gerstner 大波 + 频谱合成细节层**。大波沿用 R81 的
//!   `gerstner_evaluate`（同一 WGSL 文件，零复制；海洋档给 30 / 16.5 / 10.5 /
//!   7.5 m 的涌浪）；细节层是本模块的 [`ocean_spectral_planes`]：Phillips 型
//!   `|k|^{-4}` 能量谱（振幅谱 `∝ |k|^{-2}`）在整数波数**圆形格点**上的有限
//!   余弦和，每平面一个哈希相位。这是路线图裁定 2 的兑现形态：**放弃 GPU FFT**
//!   （跨适配器不可逐位），改成「CPU 一次烘好的周期位移图 + GPU 时间插值」。
//! - **时间演化 = 预烘关键帧**（裁定 6）：角频率量化到 `2π/T` 的整数倍
//!   （[`SpectralPlane::omega`]），整个波形场以 `T` 为周期严格回绕，`M` 张关键帧
//!   首尾**结构上同一张**（[`ocean_frame_pair`] 层号取模）⇒ 无缝不依赖浮点周期性。
//!   运行期零 CPU 生成成本，只有 GPU 一次重心式双层插值。
//! - **细节层只属于近场环**：远场网格步距 4 m，承载不了 0.25 m 尺度的谱细节
//!   （逐片元采样它会混叠成噪声），故远场只有 Gerstner 涌浪 + 反射 / Fresnel /
//!   深度吸收。这是本轮最主要的观感折衷，明文进 §8.72。
//! - **确定性**：平面表与烘图都是纯函数（固定格点枚举序 + 无状态整数哈希，禁
//!   rand / 禁全局状态），同输入 `to_bits()` 逐位一致（单测锁）；烘图在首次需要
//!   时一次完成，之后是只读 GPU 资源。`Time` 资源缺席 ⇒ t = 0 = 第 0 关键帧
//!   （与 R81 水体「缺席即静止」同语义）。
//!
//! ## 位移图契约（WGSL 侧影子）
//!
//! `texture_2d_array<Rgba16Float>`，**层号 = 关键帧号**，每层 `64²` 纹素，纹素
//! 四通道 = `(高度, ∂h/∂x, ∂h/∂z, 0)`——梯度解析求和而非有限差分（与 R81 法线
//! 用解析偏导同一哲学，且省掉每片元 4 次邻域采样）。世界 → 图坐标
//! `uv = world_xz / OCEAN_TILE_PERIOD_M` + `Repeat` 寻址（周期平铺是构造的一部
//! 分；纹素中心 [`ocean_texel_center`]）。帧数 / 通道语义 / 时间重心式 / 淡出
//! 公式在 `shaders/water_ocean.wgsl` 的 `ocean_*` 段有镜像声明，由本模块测试
//! `wgsl_ocean_contract_matches_cpu` 源码层看门（R61/R90/R92 同款纪律）。
//!
//! # 诚实限制清单（v1 明确不做）
//!
//! - **细节层只在 24 m 内**：远场无谱细节（见上），近场细节位移另按半径
//!   smoothstep 淡出到 0（淡出终点 = 远场起点 ⇒ 两环交界**无竖直台阶**）；
//! - **海不是真无限**：可见范围 = [`OceanSurface::far_extent`]（默认 320 m），
//!   之外是天空 / 雾——「无限」指**网格跟随相机**（走不到边），不是画到地平线
//!   以外。要真无限海需要多层 clipmap（v1 两环），代价是每加一环一次全场景
//!   网格 draw；两环之间的接缝半径圆在极低机位下可辨（构图上让雾遮住）；
//! - **图样周期 16 m**：谱格点建在一个 tile 上，细节在 16 m 尺度平铺重复
//!   （纹素 0.25 m、波长 2–16 m）；
//! - **6 s 时间循环**：`T = 6 s` 是角频率量化步 `2π/T` 与时间 Nyquist
//!   （[`ocean_temporal_margin`]）的交点——更长周期需要更多关键帧（成本线性涨）；
//! - **关键帧线性插值不是谱精确解**：帧间一次插值 ≈ 对每平面余弦做弦近似
//!   （无走样，但波形在帧间略「圆」）；
//! - **不做 FFT / 不做 GPU 端生成 / 不做各向异性风区谱**（裁定 2）：平面数
//!   （[`ocean_plane_count`]）是表达力上限，无顺风谱偏置、无 swell 长尾、无
//!   白帽（whitecap）/ 泡沫 / 浪花；`choppiness` 上限只保证法线不卷绕；
//! - **浮力不耦合浪高**：[`BuoyancyPlane`](iris_core::buoyancy::BuoyancyPlane)
//!   认常量水平面，物理不读位移图（跨 crate + 逐帧确定性论证另立项）；
//! - **不做水下**：相机入水仍无处理（§8.50⑤ 原样维持，R94 闭合）；
//! - **不做折射图**：无限海没有「水面以下看得见的池底」可折射，片元用
//!   scene_depth 走同一 Beer-Lambert 吸收（近岸仍有效，远海读远平面 ⇒ 深水色）；
//! - **单海面**：多 `OceanSurface` 仍按实体位模式升序取首盏（§8.50① 不变）。

use glam::{Vec2, Vec3};

use iris_core::Component;

// ---------------------------------------------------------------------------
// 谱常量（WGSL 侧 water_ocean.wgsl 的 ocean_* 段逐值同步——改这边必须改那边）
// ---------------------------------------------------------------------------

/// 细节位移图的**图样周期**（米）。整数波数格点 = `2π(i,j)/L`，故 `L` 同时是
/// 纹理的平铺周期（`Repeat` 寻址下细节每 16 m 自重复——诚实限制）。
pub const OCEAN_TILE_PERIOD_M: f32 = 16.0;

/// 位移图边长（纹素数）⇒ 纹素 `L / 64 = 0.25 m`。
pub const OCEAN_TEXELS: usize = 64;

/// 纹素尺寸（米）。
pub const OCEAN_TEXEL_SIZE_M: f32 = OCEAN_TILE_PERIOD_M / (OCEAN_TEXELS as f32);

/// 波数格点半径（圆形掩膜：保留 `i² + j² ≤ K²` 的整数格点、排除原点）。
/// `K = 8` ⇒ ≈200 个平面、波长范围 `L/K = 2 m` 到 `16 m`。
pub const OCEAN_LATTICE_K: i32 = 8;

/// 时间循环周期 `T`（秒）。关键帧数 [`OCEAN_FRAME_COUNT`]，`Δt = T/M`。
///
/// `T` 决定角频率量化步 `2π/T`（越大谱越准、循环越长）；`M` 由**时间 Nyquist**
/// 决定：最高频平面 `ω_max = √(g·2π/λ_min) ≈ 5.5 rad/s`（周期 ≈1.13 s）必须满足
/// `ω_max·Δt < π` ⇒ `Δt < 0.57 s` ⇒ `M ≥ T/0.57`。定稿 `T = 6 s`、`M = 16`
/// （`Δt = 0.375 s`，余量实测见 [`ocean_temporal_margin`]）。
pub const OCEAN_LOOP_SECONDS: f32 = 6.0;

/// 关键帧数 = 纹理数组长度。
pub const OCEAN_FRAME_COUNT: usize = 16;

/// 每关键帧时长（秒）= `T / M`。
pub const OCEAN_FRAME_STEP_SECONDS: f32 = OCEAN_LOOP_SECONDS / (OCEAN_FRAME_COUNT as f32);

/// 位移图通道数（`Rgba16Float`：高度 / x 斜率 / z 斜率 / 保留）。
pub const OCEAN_CHANNELS: usize = 4;

/// 烘图所用谱斜率（与 [`OceanSurface::spectral_slope`] 的默认值同锚；改它
/// 即改波形基 ⇒ 重烘，运行期不随组件变——见 §8.72「谱斜率是烘期参数」）。
pub const OCEAN_SLOPE: f32 = 1.5;

/// 重力幅值（单一事实源 = `iris_core::physics::PHYSICS_GRAVITY`，与浮力侧共用
/// 同一个 g，不留第二个常数）。
pub const OCEAN_GRAVITY: f32 = -iris_core::physics::PHYSICS_GRAVITY.y;

/// 深水重力波色散 `ω₀ = √(g·|k|)`（表面张力项不做，v1 声明）。
fn ocean_omega_raw(k_norm: f32) -> f32 {
    (OCEAN_GRAVITY * k_norm).sqrt()
}

// ---------------------------------------------------------------------------
// 组件
// ---------------------------------------------------------------------------

/// 无限海面组件（R93）：世界水平面 + 频谱细节层 + 双层网格范围 + 配色。
///
/// 挂上即启用（渲染器烘一次位移图、每帧一份反射捕获 + 两次环绘制）；
/// **缺席 = R81 逐位不变**。与 [`WaterSurface`](crate::water::WaterSurface)
/// 同挂时海洋优先并 `warn` 一次（单海面仍是 §8.50① 边界）。
#[derive(Clone, Copy, Debug, Component)]
pub struct OceanSurface {
    /// 海面**世界 y 高度**（米）。与 R81 同语义：水平面、不随 `Transform`。
    pub height: f32,
    /// 细节层 RMS 波高（米）。烘好的图是**单位 RMS**，本值是 GPU 侧乘子 ⇒
    /// 实时可调、不触发重烘。默认 0.09 m（≈18 cm 峰峰；与涌浪层 0.5 m 主波
    /// 振幅相比是「表面毛刺」量级——首版 0.06 在 480×320 验收图里与关闭档
    /// 几乎无别，目检判「细节层不可辨 = 旋钮没到位」后抬到 0.09）。
    pub detail_amplitude: f32,
    /// Gerstner 主波振幅（米）——海洋档默认 0.5 m（沿用 R81 四波口径）。
    pub wave_amplitude: f32,
    /// Gerstner 主波波长（米）。四波按 `WAVE_LENGTH_DECAY` 缩短到
    /// 30 / 16.5 / 10.5 / 7.5 m。
    pub wave_length: f32,
    /// Gerstner 主波相速度（米/秒，深水色散 `c ∝ √λ` 锚定档）。
    pub wave_speed: f32,
    /// Gerstner 陡度 Q（≤ 1 保证法线不卷绕）。
    pub choppiness: f32,
    /// 谱斜率（振幅 `∝ k^{-slope}`）。默认 1.5；2.0 = Phillips 能量谱档
    /// （更平滑、长波主导），1.0 = 各波段等能档（最糙，短波噪声感强）。
    /// **只影响烘图内容**，改它要重烘（`OCEAN_SLOPE` 是烘图键）。
    pub spectral_slope: f32,
    /// 近场环半宽（米）：细节层几何位移的覆盖范围，边缘 smoothstep 淡出到 0。
    pub near_extent: f32,
    /// 远场环半宽（米）：只有涌浪层 + 反射 / 吸收。默认 320 m——海平线的
    /// 可见范围上限（之外是天空 / 雾），也是两环步距（4 → 6.7 m）的来源。
    pub far_extent: f32,
    /// 浅水色（线性 HDR）。
    pub shallow_color: Vec3,
    /// 深水色（线性 HDR）。
    pub deep_color: Vec3,
    /// 深度吸收特征深度（米，下限 1e-3）。
    pub absorb_depth: f32,
}

impl Default for OceanSurface {
    fn default() -> Self {
        Self {
            height: 0.0,
            detail_amplitude: 0.12,
            wave_amplitude: 0.5,
            wave_length: 30.0,
            wave_speed: 3.0,
            choppiness: 0.8,
            spectral_slope: 1.5,
            near_extent: 24.0,
            far_extent: 320.0,
            shallow_color: Vec3::new(0.06, 0.30, 0.30),
            deep_color: Vec3::new(0.006, 0.045, 0.09),
            absorb_depth: 4.0,
        }
    }
}

impl OceanSurface {
    /// 按高度构造（其余走 `Default`）。
    pub fn new(height: f32) -> Self {
        Self {
            height,
            ..Self::default()
        }
    }
}

// ---------------------------------------------------------------------------
// 谱平面表与波形场
// ---------------------------------------------------------------------------

/// 一个谱平面（二维波）：传播方向 `dir`（单位）、波数 `k_norm`、量化角频率
/// `omega`、振幅 `amp`、初相 `phase`。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpectralPlane {
    /// 水平传播方向（单位向量，`(x, z)`）。
    pub dir: Vec2,
    /// 波数 `|k|`（rad/m）。
    pub k_norm: f32,
    /// 量化后的角频率（rad/s）= `2π·round(ω₀·T/2π)/T`。
    pub omega: f32,
    /// 振幅（米；单位 RMS 表下为「米 / 单位 RMS」）。
    pub amp: f32,
    /// 初相（弧度，`[0, 2π)`，来自 [`ocean_phase_hash`]）。
    pub phase: f32,
}

/// 无状态相位哈希：整数格点 → `[0, 2π)`。splitmix64 式混淆（与
/// `particles::mix64` / `weather::weather_cell_hash` 同族，禁 rand、无全局态），
/// 取高 24 位为分数（1/16 777 216 分辨率——相位精度远超观感需要且量化稳定）。
pub fn ocean_phase_hash(i: i32, j: i32) -> f32 {
    let mut z = (i as u32 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (j as u32 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB2F);
    z = z.wrapping_add(0x1656_67B1_9E37_79F9);
    z ^= z >> 30;
    z = z.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z ^= z >> 27;
    z = z.wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    let frac = (z >> 40) as f32 * (1.0 / 16_777_216.0);
    frac * std::f32::consts::TAU
}

/// 格点枚举（固定序：`j` 升外层、`i` 升内层；圆形掩膜 `i² + j² ≤ K²`、排除
/// 原点）。返回 `(k_norm, dir, i, j)`，与 [`ocean_raw_amps`] 一一对应。
fn ocean_lattice() -> Vec<(f32, Vec2, i32, i32)> {
    let mut v = Vec::new();
    let k_tile = std::f32::consts::TAU / OCEAN_TILE_PERIOD_M;
    let lim = OCEAN_LATTICE_K * OCEAN_LATTICE_K;
    for j in -OCEAN_LATTICE_K..=OCEAN_LATTICE_K {
        for i in -OCEAN_LATTICE_K..=OCEAN_LATTICE_K {
            if i == 0 && j == 0 {
                continue;
            }
            if i * i + j * j > lim {
                continue;
            }
            let kx = k_tile * i as f32;
            let kz = k_tile * j as f32;
            // sqrt 而非 hypot：libm 的 hypot 无位级保证（本仓浮点确定性细则）。
            let k_norm = (kx * kx + kz * kz).sqrt();
            v.push((k_norm, Vec2::new(kx / k_norm, kz / k_norm), i, j));
        }
    }
    v
}

/// 圆形格点平面数（表达力上限；文档与测试共用）。
pub fn ocean_plane_count() -> usize {
    ocean_lattice().len()
}

/// 未归一化振幅谱 `a(k) = (k₀/k)^slope`（`k₀ = 2π/L` = 格点基频，最长波
/// 取 1），与 [`ocean_lattice`] 同序同长。
///
/// `slope = 2` 即 Phillips 能量谱 `∝ k^{-4}` 的振幅形式；但**斜率谱**
/// `Σ(A·k)² ∝ k^{2-2·slope}` 在 `slope = 2` 时按 `k^{-2}` 快速衰减 ⇒ 法线
/// 扰动被最长波吃掉，短波「毛刺」在观感上几乎不存在（R93 首版验收图实测：
/// 细节开 / 关两面板近处扫描线亮度差 < 1/255，目检判「细节层不可辨」）。
/// 定稿默认 `slope = 1.5`（斜率谱 `∝ k^{-1}`，对数铺开 ⇒ 各波段都有份，
/// 2–16 m 的短波终于可见），并把 Phillips 档留作可调旋钮。
fn ocean_raw_amps(slope: f32) -> Vec<f32> {
    let k_ref = std::f32::consts::TAU / OCEAN_TILE_PERIOD_M;
    ocean_lattice()
        .into_iter()
        .map(|(k_norm, ..)| (k_ref / k_norm).powf(slope))
        .collect()
}

/// 谱平面表（纯函数）。
///
/// - **归一化**：`C = detail_rms·√(2/Σa²)` ⇒ `Σ½(C·a)² = detail_rms²`，即「细节层
///   RMS 波高恰为 `detail_rms`」；传 `1.0` 得单位 RMS 表（烘图用它，米级缩放在
///   GPU 侧乘 ⇒ 调参不重烘）；
/// - **谱斜率** `slope`：振幅 `∝ k^{-slope}`（2 = Phillips；默认 1.5，见
///   [`ocean_raw_amps`] 文档的观感实测理由）；
/// - **带限**：`k_cap = π / texel_size`（该分辨率的 Nyquist），只保留
///   `|k| ≤ k_cap` 的平面参与求值——超出的平面会在格点上假写成混叠噪声。
///   归一化恒按**全格点** Σ（与分辨率无关），故低分辨率子集 RMS 自然更小；
/// - **色散量化**：`ω₀ = √(g|k|)` → `ω = 2π·round(ω₀T/2π)/T`（周期回绕的前提）。
pub fn ocean_spectral_planes(detail_rms: f32, texel_size: f32, slope: f32) -> Vec<SpectralPlane> {
    let all = ocean_raw_amps(slope.max(0.5));
    let sum_sq: f64 = all.iter().map(|a| (*a as f64) * (*a as f64)).sum();
    let scale = if sum_sq > 0.0 {
        (2.0 * detail_rms as f64 * detail_rms as f64 / sum_sq).sqrt() as f32
    } else {
        0.0
    };
    let k_cap = if texel_size > 0.0 {
        std::f32::consts::PI / texel_size
    } else {
        f32::INFINITY
    };
    let mut out = Vec::new();
    for (idx, (k_norm, dir, i, j)) in ocean_lattice().into_iter().enumerate() {
        if k_norm > k_cap {
            continue;
        }
        let harmonics =
            (ocean_omega_raw(k_norm) * OCEAN_LOOP_SECONDS / std::f32::consts::TAU).round();
        out.push(SpectralPlane {
            dir,
            k_norm,
            omega: std::f32::consts::TAU * harmonics / OCEAN_LOOP_SECONDS,
            amp: scale * all[idx],
            phase: ocean_phase_hash(i, j),
        });
    }
    out
}

/// 波形场在 `(世界 xz, 时间 t)` 处的高度与**解析**梯度：
/// `h = Σ A·cos θ`、`∂h/∂x = Σ −A·k·d.x·sin θ`、`∂h/∂z = Σ −A·k·d.y·sin θ`，
/// `θ = k(d·p) − ω·t + φ`，**固定平面序求和**（同输入逐位一致）。这就是烘进
/// 位移图的量（图 = 本函数在纹素中心 × 关键帧时刻的采样表）。
pub fn ocean_field_at(planes: &[SpectralPlane], p: Vec2, t: f32) -> (f32, Vec2) {
    let mut h = 0.0f32;
    let mut gx = 0.0f32;
    let mut gz = 0.0f32;
    for pl in planes {
        let theta = pl.k_norm * pl.dir.dot(p) - pl.omega * t + pl.phase;
        let (s, c) = theta.sin_cos();
        h += pl.amp * c;
        let w = pl.amp * pl.k_norm * s;
        gx -= w * pl.dir.x;
        gz -= w * pl.dir.y;
    }
    (h, Vec2::new(gx, gz))
}

/// 纹素中心的世界 XZ（图样坐标 `[0, L)²`；`Repeat` 平铺由消费侧取模完成）。
pub fn ocean_texel_center(col: usize, row: usize) -> Vec2 {
    Vec2::new(
        (col as f32 + 0.5) * OCEAN_TEXEL_SIZE_M,
        (row as f32 + 0.5) * OCEAN_TEXEL_SIZE_M,
    )
}

/// 烘位移图：`frames × TEXELS² × 4` 的 f32（层主序 `[frame][row][col][ch]`，
/// 通道 = `(h, ∂h/∂x, ∂h/∂z, 0)`）。帧 `f` 的时刻 = `f·Δt`；「第 `M` 帧」在取模
/// 下与第 0 帧同一张 ⇒ **循环无缝是结构性质**，不依赖浮点周期性。
pub fn ocean_bake(planes: &[SpectralPlane], frames: usize) -> Vec<f32> {
    let n = OCEAN_TEXELS;
    let m = frames.max(1);
    let mut out = vec![0.0f32; m * n * n * OCEAN_CHANNELS];
    for f in 0..m {
        let t = f as f32 * OCEAN_FRAME_STEP_SECONDS;
        for row in 0..n {
            for col in 0..n {
                let (h, g) = ocean_field_at(planes, ocean_texel_center(col, row), t);
                let base = ((f * n + row) * n + col) * OCEAN_CHANNELS;
                out[base] = h;
                out[base + 1] = g.x;
                out[base + 2] = g.y;
            }
        }
    }
    out
}

/// 烘图的 f16 打包（上传格式 `Rgba16Float` 的位准备）。CPU 侧一次完成 ⇒ GPU 见
/// 到的就是「解析场经 f16 量化」的同一份数据，声明里不含「GPU 再算一遍」的歧义。
pub fn ocean_bake_f16(data: &[f32]) -> Vec<u16> {
    data.iter()
        .map(|&v| crate::environment::f32_to_f16_bits(v))
        .collect()
}

/// 一帧数据的切片（层主序布局的读取器，消费侧与单测共用）。
pub fn ocean_bake_slice(data: &[f32], frame: usize) -> &[f32] {
    let block = OCEAN_TEXELS * OCEAN_TEXELS * OCEAN_CHANNELS;
    let start = (frame * block).min(data.len());
    let end = (start + block).min(data.len());
    &data[start..end]
}

/// 时间 → `(关键帧 a, 关键帧 b, 插值权重 u)`，`b = (a+1) mod M`（回绕）。
/// 负时间与超周期都良定义（先取周期内相位）。
pub fn ocean_frame_pair(t: f32) -> (usize, usize, f32) {
    let m = OCEAN_FRAME_COUNT;
    let dt = OCEAN_FRAME_STEP_SECONDS;
    let period = dt * m as f32;
    let phase = (t - period * (t / period).floor()).clamp(0.0, period);
    let idx = phase / dt;
    // 取模而非 min(m−1)：`phase == period` 的边界档（t 恰为整周期）落在
    // idx = m ⇒ 取模回 0 帧 + 权重 0，正是回绕起点（钳到 m−1 会停在末帧）。
    let a = (idx.floor().max(0.0) as usize) % m;
    let u = (idx - idx.floor()).clamp(0.0, 1.0);
    (a, (a + 1) % m, u)
}

/// 关键帧间的**重心式**混合：`x·(1−a) + y·a`。
///
/// 不写 `mix`、也不写线性插值式 `x + (y−x)·a`——后者在 `a = 1` 档实测有 2 ULP
/// 残差（R92 湿表面同款教训），而重心式在 `a = 0/1` 两端点**逐位复现**单帧值
/// ⇒「停在关键帧上」可以写成逐位断言。
pub fn ocean_blend(x: f32, y: f32, a: f32) -> f32 {
    x * (1.0 - a) + y * a
}

/// 时间 Nyquist 余量：`ω_max·Δt`（rad）。**必须 < π**，否则关键帧采样把高频平面
/// 假写成低频走样。配套一条 `> π/4` 的下界旁证（否则「海面几乎不动」的退化配置
/// 也能满足上界，测试无判别力）。
pub fn ocean_temporal_margin() -> f32 {
    ocean_spectral_planes(1.0, OCEAN_TEXEL_SIZE_M, 1.5)
        .iter()
        .map(|p| p.omega * OCEAN_FRAME_STEP_SECONDS)
        .fold(0.0f32, f32::max)
}

// ---------------------------------------------------------------------------
// 双层网格
// ---------------------------------------------------------------------------

/// 一层网格的参数（纯函数产出，直接进 `OceanParamsRaw`）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OceanRing {
    /// 网格中心的世界 XZ（**量化到该环步距的整数格**——相机亚步移动不改任何
    /// 顶点位型，海面不随相机抖动）。
    pub anchor: Vec2,
    /// 半边长（米）。
    pub half_extent: f32,
    /// 网格步距（米）= `2·half_extent / WATER_GRID_SUBDIVISIONS`。
    pub grid_step: f32,
    /// 细节层**几何位移**权重（近场 1 → 边缘 0；远场恒 0）。
    pub vertex_detail: f32,
    /// 细节层**法线**权重（远场恒 0——本轮细节只属于近场，§8.72）。
    pub normal_detail: f32,
}

/// 相机眼点 → `[近场, 远场]` 两环参数。步距由网格剖分密度推出（`96²` 网格 ⇒
/// 近场 24 m → 0.5 m、远场 192 m → 4 m）。
///
/// `near_extent` 钳到 `[2·far/sub, far]`（配置反了不崩，只是合并为一环）；两环
/// 几何在 `near_extent` 处衔接：近场细节位移在那里恰淡出到 0、远场无细节位移
/// ⇒ 无竖直台阶。
pub fn ocean_rings(eye: Vec3, near_extent: f32, far_extent: f32) -> [OceanRing; 2] {
    let sub = crate::water::WATER_GRID_SUBDIVISIONS as f32;
    let far = far_extent.max(1e-3);
    let near = near_extent.clamp(2.0 * far / sub, far);
    let near_step = 2.0 * near / sub;
    let far_step = 2.0 * far / sub;
    let eye_xz = Vec2::new(eye.x, eye.z);
    [
        OceanRing {
            anchor: quantize_anchor(eye_xz, near_step),
            half_extent: near,
            grid_step: near_step,
            vertex_detail: 1.0,
            normal_detail: 1.0,
        },
        OceanRing {
            anchor: quantize_anchor(eye_xz, far_step),
            half_extent: far,
            grid_step: far_step,
            vertex_detail: 0.0,
            normal_detail: 0.0,
        },
    ]
}

/// 锚点量化：`round(p/step)·step`（同一步距下同一格 ⇒ 同结果；不足一步的相机
/// 移动被吸收）。`step ≤ 0` 原样返回（退化配置不炸）。
pub fn quantize_anchor(p: Vec2, step: f32) -> Vec2 {
    if step <= 0.0 {
        return p;
    }
    Vec2::new((p.x / step).round() * step, (p.y / step).round() * step)
}

/// 近场细节位移的半径权重：`r ≤ 0.6·near` 全额 1、`r ≥ near` 归 0（smoothstep
/// 下降，一阶导连续）。远场几何位移恒 0 ⇒ 两环交界无竖直台阶（代价：24 m 外的
/// 浪「摸不着」，§8.72）。
pub fn ocean_detail_fade(radius: f32, near_extent: f32) -> f32 {
    let edge = near_extent.max(1e-3);
    let x = ((radius - 0.6 * edge) / (0.4 * edge)).clamp(0.0, 1.0);
    1.0 - x * x * (3.0 - 2.0 * x)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    #[test]
    fn plane_table_is_deterministic_and_within_nyquist() {
        let a = ocean_spectral_planes(1.0, OCEAN_TEXEL_SIZE_M, 1.5);
        let b = ocean_spectral_planes(1.0, OCEAN_TEXEL_SIZE_M, 1.5);
        assert_eq!(a.len(), ocean_plane_count());
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.amp.to_bits(), y.amp.to_bits());
            assert_eq!(x.phase.to_bits(), y.phase.to_bits());
            assert_eq!(x.omega.to_bits(), y.omega.to_bits());
        }
        for p in &a {
            assert!((p.dir.length() - 1.0).abs() < 1e-5, "方向须单位化");
            assert!(p.k_norm > 0.0 && p.amp > 0.0);
            assert!((0.0..std::f32::consts::TAU).contains(&p.phase));
        }
        // 圆形掩膜 + Nyquist：格点最高波数须在图的采样上限内（否则「无混叠」
        // 声明作废）。
        let k_max = a.iter().map(|p| p.k_norm).fold(0.0f32, f32::max);
        assert!(
            k_max <= PI / OCEAN_TEXEL_SIZE_M + 1e-4,
            "最高波数 {k_max} 须 ≤ Nyquist {}",
            PI / OCEAN_TEXEL_SIZE_M
        );
        let k_min = a.iter().map(|p| p.k_norm).fold(f32::INFINITY, f32::min);
        assert!(
            (2.0 * PI / k_min - OCEAN_TILE_PERIOD_M).abs() < 0.5,
            "最长波长应接近 tile 周期（格点基频 = 1/L），实测 {}",
            2.0 * PI / k_min
        );
    }

    #[test]
    fn normalization_makes_rms_match_and_band_limit_reduces_energy() {
        let all = ocean_spectral_planes(1.0, OCEAN_TEXEL_SIZE_M, 1.5);
        let energy: f64 = all.iter().map(|p| 0.5 * p.amp as f64 * p.amp as f64).sum();
        assert!(
            (energy - 1.0).abs() < 1e-3,
            "单位 RMS 归一化应给 Σ½A² = 1，实测 {energy}"
        );
        // 粗纹素 ⇒ 带限更严，平面更少、能量更低（同一归一化常数的推论）。
        let coarse = ocean_spectral_planes(1.0, 2.0, 1.5);
        assert!(coarse.len() < all.len(), "粗格点应滤掉高频平面");
        let coarse_energy: f64 = coarse
            .iter()
            .map(|p| 0.5 * p.amp as f64 * p.amp as f64)
            .sum();
        assert!(
            coarse_energy < energy,
            "带限子集能量更小（{coarse_energy} < {energy}）"
        );
        // 非默认值断言：RMS 减半 ⇒ 能量 ¼（线性乘子的平方律）。
        let half = ocean_spectral_planes(0.5, OCEAN_TEXEL_SIZE_M, 1.5);
        let half_energy: f64 = half.iter().map(|p| 0.5 * p.amp as f64 * p.amp as f64).sum();
        assert!((half_energy - 0.25).abs() < 1e-3, "RMS 减半应给 ¼ 能量");
    }

    #[test]
    fn omega_quantization_is_a_multiple_of_the_loop_frequency() {
        let planes = ocean_spectral_planes(1.0, OCEAN_TEXEL_SIZE_M, 1.5);
        let w0 = std::f32::consts::TAU / OCEAN_LOOP_SECONDS;
        let mut worst = 0.0f32;
        for p in &planes {
            let n = p.omega / w0;
            assert!(
                (n - n.round()).abs() < 2e-3,
                "ω={} 必须落在 2π/T 的整数倍格点上",
                p.omega
            );
            assert!(n.round() >= 1.0, "非零平面至少一次谐波（否则该平面静止）");
            let raw = ocean_omega_raw(p.k_norm);
            worst = worst.max((p.omega - raw).abs() / raw.max(1e-6));
        }
        assert!(
            worst < 0.25,
            "色散量化相对误差须有界（最坏档 = 最低频），实测 {worst}"
        );
    }

    #[test]
    fn temporal_nyquist_margin_is_satisfied() {
        let margin = ocean_temporal_margin();
        assert!(
            margin < PI,
            "ω_max·Δt 必须小于 π（关键帧时间采样 Nyquist），实测 {margin}"
        );
        assert!(
            margin > PI / 4.0,
            "判别力旁证：采样确实吃满（否则海面几乎不动也满足上界），实测 {margin}"
        );
    }

    #[test]
    fn steeper_slope_shifts_energy_to_short_waves_and_flattens_rms() {
        // 谱斜率是活旋钮：slope 越大短波衰减越狠。判据用「斜率谱」而非高度
        // 谱——法线扰动才是观感量：Σ½(A·k)² 随 slope 单调下降。
        let slope_energy = |p: f32| -> f64 {
            ocean_spectral_planes(1.0, OCEAN_TEXEL_SIZE_M, p)
                .iter()
                .map(|pl| 0.5 * (pl.amp as f64 * pl.k_norm as f64).powi(2))
                .sum()
        };
        let phillips = slope_energy(2.0);
        let mid = slope_energy(1.5);
        let flat = slope_energy(1.0);
        assert!(
            phillips < mid && mid < flat,
            "斜率谱须随 slope 单调：Phillips {phillips} < 1.5 档 {mid} < 1.0 档 {flat}"
        );
        // 但 RMS 高度恒 = 1（归一化与 slope 无关 ⇒ 抬短波不会把海抬高）。
        for p in [1.0, 1.5, 2.0, 3.0] {
            let rms: f64 = ocean_spectral_planes(1.0, OCEAN_TEXEL_SIZE_M, p)
                .iter()
                .map(|pl| 0.5 * pl.amp as f64 * pl.amp as f64)
                .sum();
            assert!(
                (rms - 1.0).abs() < 1e-3,
                "slope={p} 时 RMS 仍须为 1，实测 {rms}"
            );
        }
        // 退化保护：slope ≤ 0.5 按 0.5 处理（避免振幅谱发散）。
        assert_eq!(
            ocean_spectral_planes(1.0, OCEAN_TEXEL_SIZE_M, 0.0).len(),
            ocean_spectral_planes(1.0, OCEAN_TEXEL_SIZE_M, 0.5).len()
        );
    }

    #[test]
    fn bake_is_bitwise_deterministic_and_wraps_structurally() {
        let planes = ocean_spectral_planes(1.0, OCEAN_TEXEL_SIZE_M, 1.5);
        let a = ocean_bake(&planes, OCEAN_FRAME_COUNT);
        let b = ocean_bake(&planes, OCEAN_FRAME_COUNT);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.to_bits(), y.to_bits(), "同输入烘两次必须逐位一致");
        }
        assert_eq!(
            a.len(),
            OCEAN_FRAME_COUNT * OCEAN_TEXELS * OCEAN_TEXELS * OCEAN_CHANNELS
        );
        let (last_a, last_b, _) =
            ocean_frame_pair((OCEAN_FRAME_COUNT as f32 - 1.0) * OCEAN_FRAME_STEP_SECONDS);
        assert_eq!(last_a, OCEAN_FRAME_COUNT - 1);
        assert_eq!(last_b, 0, "末帧的下一帧必须回绕到 0（无缝是索引性质）");
        // 帧 0 的图 = t = 0 解析场（通道布局锚）。
        let s = ocean_bake_slice(&a, 0);
        let p = ocean_texel_center(5, 9);
        let (h, g) = ocean_field_at(&planes, p, 0.0);
        let base = (9 * OCEAN_TEXELS + 5) * OCEAN_CHANNELS;
        assert_eq!(s[base].to_bits(), h.to_bits());
        assert_eq!(s[base + 1].to_bits(), g.x.to_bits());
        assert_eq!(s[base + 2].to_bits(), g.y.to_bits());
        assert_eq!(s[base + 3], 0.0, "保留通道恒 0");
    }

    #[test]
    fn f16_packing_matches_analytic_field_quantized() {
        let planes = ocean_spectral_planes(1.0, OCEAN_TEXEL_SIZE_M, 1.5);
        let data = ocean_bake(&planes, 2);
        let packed = ocean_bake_f16(&data);
        assert_eq!(packed.len(), data.len());
        for (v, b) in data.iter().zip(packed.iter()).take(4096) {
            assert_eq!(*b, crate::environment::f32_to_f16_bits(*v));
        }
        // 单位 RMS 的图远在 f16 动态范围内（否则量化本身吞掉细节）。
        let peak = data.iter().fold(0.0f32, |m: f32, &v| m.max(v.abs()));
        assert!(peak < 64.0, "f16 富余（峰 {peak} ≪ 65504）");
    }

    #[test]
    fn frame_interpolation_endpoints_are_bitwise_and_midpoint_is_between() {
        let planes = ocean_spectral_planes(1.0, OCEAN_TEXEL_SIZE_M, 1.5);
        let data = ocean_bake(&planes, OCEAN_FRAME_COUNT);
        let probe = (3 * OCEAN_TEXELS + 7) * OCEAN_CHANNELS;
        let f0 = ocean_bake_slice(&data, 0)[probe];
        let f1 = ocean_bake_slice(&data, 1)[probe];
        assert_ne!(
            f0.to_bits(),
            f1.to_bits(),
            "相邻两帧须真的不同（否则时间轴无意义）"
        );
        assert_eq!(ocean_blend(f0, f1, 0.0).to_bits(), f0.to_bits());
        assert_eq!(ocean_blend(f0, f1, 1.0).to_bits(), f1.to_bits());
        let mid = ocean_blend(f0, f1, 0.5);
        assert!(
            mid >= f0.min(f1) - 1e-9 && mid <= f0.max(f1) + 1e-9,
            "无过冲"
        );
    }

    #[test]
    fn frame_pair_walks_the_whole_loop_and_handles_edges() {
        let dt = OCEAN_FRAME_STEP_SECONDS;
        for k in 0..OCEAN_FRAME_COUNT {
            let (a, b, u) = ocean_frame_pair(k as f32 * dt);
            assert_eq!(a, k, "整数帧时刻应恰落在该帧");
            assert_eq!(u.to_bits(), 0.0f32.to_bits(), "整数帧处权重须为 0");
            assert_eq!(b, (k + 1) % OCEAN_FRAME_COUNT);
        }
        assert_eq!(ocean_frame_pair(OCEAN_LOOP_SECONDS).0, 0, "整周期回绕到 0");
        assert_eq!(
            ocean_frame_pair(-dt).0,
            OCEAN_FRAME_COUNT - 1,
            "负时间向前回绕"
        );
        assert!((ocean_frame_pair(0.5 * dt).2 - 0.5).abs() < 1e-6);
    }

    #[test]
    fn rings_step_from_grid_density_and_split_detail_by_range() {
        let eye = Vec3::new(13.5, 1.0, -8.0);
        let [near, far] = ocean_rings(eye, 24.0, 192.0);
        assert_eq!(near.grid_step, 0.5, "2·24/96");
        assert_eq!(far.grid_step, 4.0, "2·192/96");
        assert_eq!(near.half_extent, 24.0);
        assert_eq!(far.half_extent, 192.0);
        assert_eq!(near.vertex_detail, 1.0);
        assert_eq!(far.vertex_detail, 0.0, "远场不做细节几何位移（§8.72）");
        assert_eq!(far.normal_detail, 0.0, "本轮细节只属于近场");
        // 锚点是步距整数倍；亚步移动吸收；越一步换格。
        let [n0, _] = ocean_rings(eye, 24.0, 192.0);
        let [n1, _] = ocean_rings(eye + Vec3::new(0.2, 0.0, -0.15), 24.0, 192.0);
        assert_eq!(bits2(n0.anchor), bits2(n1.anchor), "同格内不得抖动");
        let [n2, _] = ocean_rings(eye + Vec3::new(0.7, 0.0, 0.0), 24.0, 192.0);
        assert_ne!(n0.anchor.x, n2.anchor.x, "越一步必换格（否则量化是假安全）");
        // 反配置不崩：near > far ⇒ 钳到 far。
        let [n, f] = ocean_rings(eye, 300.0, 100.0);
        assert!(n.half_extent <= f.half_extent + 1e-6);
    }

    #[test]
    fn detail_fade_is_full_inside_zero_at_edge_and_monotone() {
        assert_eq!(ocean_detail_fade(0.0, 24.0), 1.0);
        assert_eq!(ocean_detail_fade(14.4, 24.0), 1.0, "0.6R 内全额");
        assert_eq!(ocean_detail_fade(24.0, 24.0), 0.0, "边缘必归零（无缝前提）");
        assert_eq!(ocean_detail_fade(99.0, 24.0), 0.0);
        let mut prev = 2.0;
        for k in 0..13 {
            let w = ocean_detail_fade(10.0 + k as f32, 24.0);
            assert!(w <= prev + 1e-6, "淡出须单调不增");
            prev = w;
        }
    }

    #[test]
    fn height_field_is_bounded_and_zero_mean_over_the_tile() {
        let planes = ocean_spectral_planes(1.0, OCEAN_TEXEL_SIZE_M, 1.5);
        let data = ocean_bake(&planes, OCEAN_FRAME_COUNT);
        let peak = data
            .iter()
            .step_by(OCEAN_CHANNELS)
            .fold(0.0f32, |m: f32, &h| m.max(h.abs()));
        let bound: f32 = planes.iter().map(|p| p.amp).sum();
        assert!(
            peak <= bound + 1e-4,
            "峰高必 ≤ Σ振幅（解析上界）：{peak} vs {bound}"
        );
        let sum: f64 = data.iter().step_by(OCEAN_CHANNELS).map(|&h| h as f64).sum();
        let mean = sum / (data.len() as f64 / OCEAN_CHANNELS as f64);
        assert!(
            mean.abs() < 5e-3,
            "整周期均值应近零（图不整体抬升海面），实测 {mean}"
        );
    }

    #[test]
    fn component_defaults_are_sane_and_off_by_absence() {
        let o = OceanSurface::default();
        assert_eq!(o.height, 0.0);
        assert!(o.detail_amplitude > 0.0 && o.detail_amplitude < o.wave_amplitude);
        assert_eq!(o.far_extent, 320.0, "海平线可见范围（§8.72 声明值）");
        assert_eq!(o.spectral_slope, 1.5, "默认档观感定稿（Phillips 2.0 偏平）");
        assert_eq!(o.detail_amplitude, 0.12);
        assert_eq!(o.near_extent, 24.0, "细节层几何覆盖范围（§8.72 声明值）");
        assert!(o.near_extent < o.far_extent);
        assert!(o.choppiness <= 1.0, "Q ≤ 1 是法线不卷绕的前提");
        assert_eq!(OceanSurface::new(3.0).height, 3.0);
    }

    #[test]
    fn wgsl_ocean_contract_matches_cpu() {
        // 影子契约：WGSL 侧的 ocean 常量 / 通道语义 / 两条镜像公式必须与本模块
        // 逐值同式（R61/R90/R92 源码层看门口径）。
        let src = include_str!("../shaders/water_ocean.wgsl");
        assert_eq!(OCEAN_FRAME_COUNT, 16);
        assert_eq!(OCEAN_CHANNELS, 4);
        assert_eq!(OCEAN_TILE_PERIOD_M, 16.0);
        for expect in [
            "const OCEAN_TILE_PERIOD: f32 = 16.0;",
            "const OCEAN_FRAMES: u32 = 16u;",
            "const OCEAN_TEXEL_H: u32 = 0u;",
            "const OCEAN_TEXEL_DX: u32 = 1u;",
            "const OCEAN_TEXEL_DZ: u32 = 2u;",
            // 时间混合须重心式（R92 教训）；淡出须与 ocean_detail_fade 同式。
            "h_a * (1.0 - u) + h_b * u",
            "1.0 - x * x * (3.0 - 2.0 * x)",
        ] {
            assert!(
                src.contains(expect),
                "water_ocean.wgsl 缺 ocean 契约片段 `{expect}`（改 CPU 侧必须同步 WGSL）"
            );
        }
        // 拼接契约：Gerstner 求和只在 `water_common.wgsl` 里有一份，海洋文件
        // **不得自带一份**（自带即脱离单源，拼接方案的意义消失）。
        let common = include_str!("../shaders/water_common.wgsl");
        assert!(
            common.contains("fn water_evaluate(") && common.contains("struct WaterVertexInput {"),
            "Gerstner 求和与水面顶点格式必须在 water_common.wgsl（单一事实源）"
        );
        assert!(
            !src.contains("fn water_evaluate("),
            "water_ocean.wgsl 不得自带 Gerstner 拷贝（改由 pipeline.rs 拼接 common）"
        );
        assert!(
            src.contains("fn vs_ocean(") && src.contains("fn fs_ocean("),
            "海洋 entry point 必须齐备"
        );
    }

    /// `Vec2` 的位型（glam 无 `Vec2::to_bits`）。
    fn bits2(v: Vec2) -> [u32; 2] {
        [v.x.to_bits(), v.y.to_bits()]
    }
}
