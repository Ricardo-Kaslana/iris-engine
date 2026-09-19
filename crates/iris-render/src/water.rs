//! 平面水体 v1（R81）：`WaterSurface` 组件 + Gerstner 波纯函数核心 +
//! 水面网格生成 + 多水面确定性选择键。
//!
//! ## 语义摘要
//!
//! - 实体挂 [`WaterSurface`] 声明一块**世界空间水平面**（`height` 为平面
//!   的世界 y，矩形半边长 [`WaterSurface::half_extent`] 以实体原点为中心
//!   ——**v1 不随 `Transform` 平移 / 旋转，`height` 就是世界坐标**，
//!   挂不挂 `Transform` 均可）；渲染器主 pass 之后把水体 alpha 合成进 HDR
//!   （反射 / 折射捕获 + Fresnel + 深度吸收 + 太阳镜面，见
//!   `shaders/water.wgsl`）。
//! - **单水面**：多个 `WaterSurface` 实体按确定性选择键
//!   [`select_water_surface`]（**实体位模式升序**，与
//!   `select_point_shadow_casters` 同哲学——结果只取决于实体集合、与
//!   查询 / 插入顺序无关）取首盏，其余 `log::warn` 忽略；反射 / 折射
//!   捕获只为首盏做。
//! - **资源缺席语义**：世界中无 `WaterSurface` 实体 = 无捕获 pass、无
//!   额外 write_buffer、无水体 draw——像素与命令流与引入前**逐位一致**
//!   （零开销默认路径；`DefaultPlugins` 无需注册任何东西，组件本身就是
//!   开关）。
//! - **确定性**：波形是 4 条固定方向 Gerstner 波的**固定序**求和
//!   （[`gerstner_evaluate`]），同输入 `to_bits()` 逐位一致；时间 t 来自
//!   世界 `Time` 资源的 `elapsed_secs`（资源缺席 = t = 0，波形静止在
//!   初始相位——不引入任何渲染器内部时钟）。无 rand、无全局状态。
//! - **WGSL 镜像契约**：[`gerstner_single_wave`] / [`gerstner_evaluate`]
//!   与 `shaders/water_common.wgsl` 的 `water_single_wave` / `water_evaluate`
//!   **逐式镜像（同式同序，两侧必须同步修改）**——R93 起波形段抽进
//!   `water_common.wgsl`，由 `pipeline.rs` 分别拼进池塘（`water.wgsl`）与
//!   海洋（`water_ocean.wgsl`）两个 shader module ⇒ 两路共用同一份求和逻辑
//!   （见 ARCHITECTURE §4.52）——CPU 单测即着色器波形
//!   的解析锚点（与 `fog.rs` / `fog.wgsl` 同款纪律；CPU↔GPU 不做位级
//!   等价声明——sin/cos 的库实现不同，位级一致只对「同侧同输入」成立）。

use glam::{Vec2, Vec3};

use iris_core::Component;

// ---------- 组件（World 查询直读，渲染器按需消费） ----------

/// 平面水体组件（R81）：世界空间水平矩形面 + Gerstner 波形参数 +
/// 深度吸收配色。语义与诚实限制见模块文档与 ARCHITECTURE §4.37 / §8.50。
///
/// **组件缺席 = 无水体**（渲染路径零变化）；组件在场 = 渲染器每帧为其
/// 做反射 / 折射捕获并在主 pass 后合成。多个实体同挂时按
/// [`select_water_surface`] 取首盏。
#[derive(Clone, Copy, Debug, Component)]
pub struct WaterSurface {
    /// 水平面的**世界空间** y 高度（米）。v1 不随 `Transform` 平移 /
    /// 旋转、不可倾斜（诚实限制——倾斜水面的镜像矩阵不再是对角反射）。
    pub height: f32,
    /// 矩形半边长（米，XZ 两轴；水面以世界原点为中心铺开
    /// `[−half_extent, +half_extent]²`）。
    pub half_extent: Vec2,
    /// Gerstner 主波振幅（米）。四波的振幅按 [`WAVE_AMPLITUDE_DECAY`]
    /// 衰减，总波高上界 = 本值 × Σ衰减（Default 0.12 × 2.1）。
    pub wave_amplitude: f32,
    /// Gerstner 主波波长（米）。后续波按 [`WAVE_LENGTH_DECAY`] 缩短。
    pub wave_length: f32,
    /// 主波相速度（米/秒）：各波角频率 ω = k·speed·√(λ_i/λ_0)
    /// （深水色散 c ∝ √λ 锚定在主波速度上）。
    pub wave_speed: f32,
    /// Gerstner 陡度系数 Q（0 = 正弦波无卷曲；≤ 1 保证法线 y 分量不翻
    /// ——Q 按波数均分到各波后每波单项 ≤ Q/4，见 [`gerstner_single_wave`]）。
    pub choppiness: f32,
    /// 浅水水体色（线性 HDR）：深度差 → 0（岸边）时水体本色趋于此色。
    pub shallow_color: Vec3,
    /// 深水水体色（线性 HDR）：深度差 ≥ 数倍特征深度时趋于此色。
    pub deep_color: Vec3,
    /// 深度渐变特征深度（米）：水下厚度 = absorb_depth 时 Beer-Lambert
    /// 透过率恰为 1/e ≈ 0.368（颜色向深水色过半的转折点）。
    pub absorb_depth: f32,
}

impl Default for WaterSurface {
    fn default() -> Self {
        Self {
            height: 0.0,
            half_extent: Vec2::new(16.0, 16.0),
            wave_amplitude: 0.12,
            wave_length: 6.0,
            wave_speed: 1.2,
            choppiness: 0.6,
            // 浅青 / 深蓝（线性 HDR 域的常规水体配色，调参以观感为准）
            shallow_color: Vec3::new(0.10, 0.42, 0.40),
            deep_color: Vec3::new(0.012, 0.075, 0.14),
            absorb_depth: 2.5,
        }
    }
}

impl WaterSurface {
    /// 按字段构造（高度 + 半边长必填，波形 / 配色走 Default）。
    pub fn new(height: f32, half_extent: Vec2) -> Self {
        Self {
            height,
            half_extent,
            ..Self::default()
        }
    }
}

// ---------- 波形常量（WGSL 侧 water.wgsl 逐值同步，修改必须两侧同步） ----------

/// 波数（固定 4 波，等角方向 + 逐波衰减——无频谱采样，v1 定稿）。
pub const WATER_WAVE_COUNT: usize = 4;

/// 除法用波数（f32；避免热路径 usize→f32 转换散落各处）。
const WATER_WAVE_COUNT_F: f32 = 4.0;

/// 四波的传播方向（单位向量，45° 起步的等角分布）：对角方向让波峰走向
/// 与世界轴 / 网格边都斜交，视觉上比 0°/90° 轴对齐更少「十字网格感」。
pub const WAVE_DIRECTIONS: [Vec2; WATER_WAVE_COUNT] = [
    Vec2::new(
        std::f32::consts::FRAC_1_SQRT_2,
        std::f32::consts::FRAC_1_SQRT_2,
    ),
    Vec2::new(
        -std::f32::consts::FRAC_1_SQRT_2,
        std::f32::consts::FRAC_1_SQRT_2,
    ),
    Vec2::new(
        -std::f32::consts::FRAC_1_SQRT_2,
        -std::f32::consts::FRAC_1_SQRT_2,
    ),
    Vec2::new(
        std::f32::consts::FRAC_1_SQRT_2,
        -std::f32::consts::FRAC_1_SQRT_2,
    ),
];

/// 逐波振幅衰减（主波承载轮廓、高阶波只添细节的能量递减经验值）：
/// 波形有界上界 = 主振幅 × 本数组之和（单测锁定）。
pub const WAVE_AMPLITUDE_DECAY: [f32; WATER_WAVE_COUNT] = [1.0, 0.5, 0.35, 0.25];

/// 逐波波长缩短比（短波叠加细节；深水色散 c ∝ √λ 随之让短波更「碎、快」）。
pub const WAVE_LENGTH_DECAY: [f32; WATER_WAVE_COUNT] = [1.0, 0.55, 0.35, 0.25];

/// 波形总振幅上界（Σ衰减 × 主振幅）：波形有界性单测的解析依据。
pub const WAVE_AMPLITUDE_SUM: f32 = 2.1;

const _: () = assert!(
    WAVE_AMPLITUDE_DECAY[0]
        + WAVE_AMPLITUDE_DECAY[1]
        + WAVE_AMPLITUDE_DECAY[2]
        + WAVE_AMPLITUDE_DECAY[3]
        == WAVE_AMPLITUDE_SUM,
    "WAVE_AMPLITUDE_SUM 必须等于衰减数组之和（波形有界性的解析依据）"
);

/// 水的 Fresnel 反射率基准 F0（物理值：垂直入射时水的反射率 ≈ 2%）。
pub const WATER_F0: f32 = 0.02;

// ---------- Gerstner 纯函数（water.wgsl 逐式镜像，两侧同步修改） ----------

/// 单条 Gerstner 波的贡献（位移增量 + 法线累加项）。
///
/// 输入 `p` 是**未位移**的网格点（Lagrangian 视角：顶点从静态格点出发被
/// 波推走）。公式（GPU Gems ch.1 的解析式）：
/// - 相位 φ = k·(d·p) − ω·t，k = 2π/λ，ω = k·speed·√(λ/λ₀)
///   （深水色散 c = ω/k = speed·√(λ/λ₀)——短波传播更慢，锚定主波速度）；
/// - 位移：x/z 各加 Q·A·d⊥·cos φ（choppy 卷曲），y 加 A·sin φ；
/// - 陡度均分：Q_i = choppiness/(k·A·4)——choppiness ≤ 1 时每波的
///   Q_i·k·A ≤ 1/4，四波合计 ≤ 1 ⇒ 法线 y 分量（1 − ΣQ·k·A·sin φ）
///   恒正，波形不产生卷绕（法线翻转）退化；
/// - 法线用**解析偏导**（不是差分）：N = normalize((−Σd⊥·k·A·cos φ,
///   1 − ΣQ·k·A·sin φ, …))——单波只返回自己的累加项，求和交给
///   [`gerstner_evaluate`]（固定波序 = 确定性）。
///
/// 返回 `(位移增量, 法线累加项)`——位移增量加到格点上得波形位置，法线
/// 累加项逐波加到 (0, 1, 0) 上后归一化得表面法线。
#[allow(clippy::too_many_arguments)] // 波形参数逐项显式入参——镜像契约要求与 WGSL 同形
pub fn gerstner_single_wave(
    p: Vec2,
    t: f32,
    dir: Vec2,
    wave_length: f32,
    base_length: f32,
    amplitude: f32,
    speed: f32,
    choppiness: f32,
) -> (Vec3, Vec3) {
    let k = std::f32::consts::TAU / wave_length;
    let omega = k * speed * (wave_length / base_length).sqrt();
    let phase = k * dir.dot(p) - omega * t;
    let (s, c) = phase.sin_cos();
    // 陡度均分到 4 波（WATER_WAVE_COUNT_F）：见函数文档的「不卷绕」界
    let steep = choppiness / (k * amplitude * WATER_WAVE_COUNT_F);
    let disp = Vec3::new(
        steep * amplitude * dir.x * c,
        amplitude * s,
        steep * amplitude * dir.y * c,
    );
    let wa = k * amplitude;
    let normal_acc = Vec3::new(-dir.x * wa * c, -steep * wa * s, -dir.y * wa * c);
    (disp, normal_acc)
}

/// 非零安全归一化（零向量返回零向量，不产生 NaN——WGSL 侧
/// `water_normalize_safe` 逐式镜像，退化参数下的行为两侧一致）。
fn normalize_safe(v: Vec3) -> Vec3 {
    let l = v.length();
    if l > 0.0 { v / l } else { Vec3::ZERO }
}

/// 四波 Gerstner 求和（**固定波序 0..4，固定求和序**——同输入
/// `to_bits()` 逐位一致的确定性来源；无 rand、无全局状态）。
///
/// 返回 `(波形后的世界点, 表面法线)`：`p` 为未位移格点的 XZ 坐标，波形
/// 点 = (p.x + Σdx, Σdy, p.y + Σdz)，法线 = 归一化的逐波解析偏导和。
/// 波长 / 振幅按衰减数组逐波派生（见模块头常量）。
pub fn gerstner_evaluate(
    p: Vec2,
    t: f32,
    amplitude: f32,
    wave_length: f32,
    speed: f32,
    choppiness: f32,
) -> (Vec3, Vec3) {
    let mut disp = Vec3::ZERO;
    let mut acc = Vec3::new(0.0, 1.0, 0.0);
    for i in 0..WATER_WAVE_COUNT {
        let (d, n) = gerstner_single_wave(
            p,
            t,
            WAVE_DIRECTIONS[i],
            wave_length * WAVE_LENGTH_DECAY[i],
            wave_length,
            amplitude * WAVE_AMPLITUDE_DECAY[i],
            speed,
            choppiness,
        );
        disp += d;
        acc += n;
    }
    let pos = Vec3::new(p.x + disp.x, disp.y, p.y + disp.z);
    (pos, normalize_safe(acc))
}

// ---------- 水面网格生成（CPU 一次生成 upload；纯函数 + 单测） ----------

/// 水面网格顶点：XZ 格点（`[−1, 1]²` 归一化域，绘制期在顶点着色器里乘
/// `half_extent` 展开——同一份 GPU 网格服务任意尺寸的水面）+ 预留 UV
/// （v1 着色不消费，为泡沫 / 波法线贴图留位）。16 字节，POD。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WaterVertex {
    pub pos_xz: [f32; 2],
    pub uv: [f32; 2],
}

impl WaterVertex {
    /// 顶点缓冲布局：location 0 = pos_xz（Float32x2）、location 1 = uv。
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<WaterVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[
            wgpu::VertexAttribute {
                offset: 0,
                shader_location: 0,
                format: wgpu::VertexFormat::Float32x2,
            },
            wgpu::VertexAttribute {
                offset: 8,
                shader_location: 1,
                format: wgpu::VertexFormat::Float32x2,
            },
        ],
    };
}

/// 水面网格剖分密度（每边 quad 数——静态常量非设置项：波形细节由顶点
/// 位移承担，96² quad = 18k 三角形在中低端 GPU 上可忽略）。
pub const WATER_GRID_SUBDIVISIONS: u32 = 96;

/// 生成 `[−1, 1]²` 归一化水面网格（`(sub+1)²` 顶点 + `6·sub²` 索引）。
///
/// 索引环绕方向 = 从 +Y 向下看 CCW（与 `MeshData::plane` / 圆柱顶帽同一
/// 约定，法线朝上；水体管线用 cull None，环绕方向只作数据规范性存在）。
/// 同输入两次生成逐字节一致（确定性单测锁定）。
pub fn water_grid_mesh(subdivisions: u32) -> (Vec<WaterVertex>, Vec<u32>) {
    let n = subdivisions.max(1);
    let mut vertices = Vec::with_capacity(((n + 1) * (n + 1)) as usize);
    for iz in 0..=n {
        for ix in 0..=n {
            let u = ix as f32 / n as f32;
            let v = iz as f32 / n as f32;
            vertices.push(WaterVertex {
                pos_xz: [u * 2.0 - 1.0, v * 2.0 - 1.0],
                uv: [u, v],
            });
        }
    }
    let stride = n + 1;
    let mut indices = Vec::with_capacity((6 * n * n) as usize);
    for iz in 0..n {
        for ix in 0..n {
            let a = iz * stride + ix; // (x_lo, z_lo)
            let b = a + 1; // (x_hi, z_lo)
            let c = a + stride; // (x_lo, z_hi)
            let d = c + 1; // (x_hi, z_hi)
            // +Y 俯视 CCW：(c, d, b) 与 (c, b, a)——与 plane 图元的
            // 「z 大 → z 小、x 低 → x 高」环绕一致（推导见函数文档）
            indices.extend_from_slice(&[c, d, b, c, b, a]);
        }
    }
    (vertices, indices)
}

// ---------- 多水面确定性选择键 ----------

/// 多个 `WaterSurface` 实体的**确定性选择键**（R81）：返回**实体位模式
/// 最小**者的下标——最小值与遍历顺序无关（min 的交换律 ⇒ 结果只取决于
/// 实体集合本身，与查询 / 插入顺序无关），与
/// [`crate::light::select_point_shadow_casters`] 的「实体位模式收尾全序」
/// 同哲学（此处距离项都不需要：单水面无预算裁剪，位模式本身就是全序——
/// `Entity::to_bits` 在同一世界内唯一）。空切片返回 `None`（无水体）。
pub fn select_water_surface(entity_bits: &[u64]) -> Option<usize> {
    let mut best: Option<(u64, usize)> = None;
    for (i, &bits) in entity_bits.iter().enumerate() {
        best = Some(match best {
            None => (bits, i),
            Some((b, j)) => {
                if bits < b {
                    (bits, i)
                } else {
                    (b, j)
                }
            }
        });
    }
    best.map(|(_, i)| i)
}

// ---------- 波形参数打包（uniform 布局与 renderer.rs WaterParamsRaw
// 逐字段对齐；wave_a / wave_b 的语义注释见 water.wgsl） ----------

#[cfg(test)]
mod tests {
    use super::*;

    /// 逐位相等断言的辅助（f32 集合逐位比较）。
    fn bits_of(v: Vec3) -> [u32; 3] {
        [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()]
    }

    #[test]
    fn defaults_match_spec() {
        let w = WaterSurface::default();
        assert_eq!(w.height, 0.0);
        assert_eq!(w.half_extent, Vec2::new(16.0, 16.0));
        assert_eq!(w.wave_amplitude, 0.12);
        assert_eq!(w.wave_length, 6.0);
        assert_eq!(w.wave_speed, 1.2);
        assert_eq!(w.choppiness, 0.6);
        assert_eq!(w.absorb_depth, 2.5);
        // 构造器透传
        let w = WaterSurface::new(2.5, Vec2::new(8.0, 3.0));
        assert_eq!(w.height, 2.5);
        assert_eq!(w.half_extent, Vec2::new(8.0, 3.0));
        assert_eq!(w.wave_length, 6.0, "构造器只覆盖几何字段，波形走默认");
    }

    /// t=0、原点：四波相位全 0 ⇒ sin 项为 0（**y 位移恰为 0**——sin(0)
    /// = 0 逐波精确）、cos 项为 1，四条对角方向两两近似抵消 ⇒ 法线近 +Y
    ///（x/z 的 choppy 位移不抵消为精确零：各波波长衰减 ⇒ k_i 不同 ⇒
    /// Q/(4k_i) 项不相等——Gerstner 在原点的固有横向错位，非缺陷）。
    #[test]
    fn gerstner_origin_at_t0_is_undisplaced_with_up_normal() {
        let (pos, normal) = gerstner_evaluate(Vec2::ZERO, 0.0, 0.12, 6.0, 1.2, 0.6);
        assert_eq!(pos.y, 0.0, "原点 t=0 的波高应精确为 0：{pos:?}");
        assert!(
            pos.x.abs() < 0.15 && pos.z.abs() < 0.15,
            "横向 choppy 错位应有界（量级 = ΣQ/(4k)：{pos:?}）"
        );
        assert!(
            normal.y > 0.99 && normal.x.abs() < 0.02 && normal.z.abs() < 0.02,
            "四波对角对称 ⇒ 法线应近 +Y：{normal:?}"
        );
    }

    /// 单波 t=0 平移对称：沿波向平移一个波长，相位恰加 2π（浮点舍入内
    /// ±1e-5 容差，依据：sin/cos 周期性 + k·λ = 2π 的解析恒等）。
    #[test]
    fn gerstner_single_wave_translates_by_wavelength_at_t0() {
        let dir = WAVE_DIRECTIONS[0];
        let len = 6.0f32;
        let p0 = Vec2::new(1.3, -0.7);
        let p1 = p0 + dir * len;
        let eval = |p: Vec2| gerstner_single_wave(p, 0.0, dir, len, len, 0.12, 1.2, 0.6);
        let (d0, n0) = eval(p0);
        let (d1, n1) = eval(p1);
        assert!(
            (d0 - d1).length() < 1e-5,
            "平移一个波长后位移应不变：{d0:?} vs {d1:?}"
        );
        assert!(
            (n0 - n1).length() < 1e-5,
            "平移一个波长后法线应不变：{n0:?} vs {n1:?}"
        );
    }

    /// 单波振幅上界：|dy| ≤ A（波形扫描，含 choppy 的 x/z 位移不改变 y 界）。
    #[test]
    fn gerstner_single_wave_height_bounded_by_amplitude() {
        let dir = WAVE_DIRECTIONS[2];
        for ix in 0..64 {
            for it in 0..64 {
                let p = Vec2::new(ix as f32 * 0.37 - 11.0, it as f32 * 0.29 + 2.0);
                let t = it as f32 * 0.11;
                let (d, _) = gerstner_single_wave(p, t, dir, 5.0, 5.0, 0.4, 2.0, 0.9);
                assert!(
                    d.y.abs() <= 0.4 + 1e-6,
                    "单波 y 必须有界于 A：{d:?} @ {p:?}, t={t}"
                );
            }
        }
    }

    /// 四波波形有界：|y| ≤ Σ衰减 × 主振幅（解析依据
    /// [`WAVE_AMPLITUDE_SUM`]，扫描网格 + 时间确认无越界）。
    #[test]
    fn gerstner_total_height_bounded_by_amplitude_sum() {
        let bound = WAVE_AMPLITUDE_SUM * 0.5;
        assert!((WAVE_AMPLITUDE_SUM - 2.1).abs() < 1e-6, "衰减和应为 2.1");
        for ix in 0..32 {
            for it in 0..32 {
                let p = Vec2::new(ix as f32 * 0.83 - 13.0, it as f32 * 1.17 - 9.0);
                let t = it as f32 * 0.23;
                let (pos, _) = gerstner_evaluate(p, t, 0.5, 7.0, 1.5, 0.8);
                assert!(
                    pos.y.abs() <= bound + 1e-5,
                    "波形必须有界：y={} > {bound} @ {p:?}, t={t}",
                    pos.y
                );
            }
        }
    }

    /// 法线单位化（解析偏导 + 归一化的直接推论，±1e-5 容差）。
    #[test]
    fn gerstner_normal_is_unit_length() {
        for ix in 0..32 {
            for it in 0..32 {
                let p = Vec2::new(ix as f32 * 0.91 - 7.0, it as f32 * 0.53 + 1.0);
                let t = it as f32 * 0.31;
                let (_, n) = gerstner_evaluate(p, t, 0.3, 4.0, 1.0, 0.75);
                assert!(
                    (n.length() - 1.0).abs() < 1e-5,
                    "法线应单位化：{n:?} @ {p:?}"
                );
                assert!(
                    n.y > 0.0,
                    "choppiness ≤ 1 时法线 y 分量恒正（不卷绕界）：{n:?}"
                );
            }
        }
    }

    /// 确定性：同输入两次求值 to_bits 逐位一致（固定波序固定求和序）。
    #[test]
    fn gerstner_is_bitwise_deterministic() {
        let p = Vec2::new(3.7, -2.1);
        let (pa, na) = gerstner_evaluate(p, 1.234, 0.12, 6.0, 1.2, 0.6);
        let (pb, nb) = gerstner_evaluate(p, 1.234, 0.12, 6.0, 1.2, 0.6);
        assert_eq!(bits_of(pa), bits_of(pb), "位置必须逐位一致");
        assert_eq!(bits_of(na), bits_of(nb), "法线必须逐位一致");
        // 时间与参数不同 ⇒ 结果不同（防恒等假通过）
        let (pc, _) = gerstner_evaluate(p, 1.235, 0.12, 6.0, 1.2, 0.6);
        assert_ne!(bits_of(pa), bits_of(pc), "时间推进必须改变波形");
    }

    /// 网格生成：尺寸公式、坐标域、索引界、环绕方向（+Y 俯视 CCW）与
    /// 确定性（两次生成逐位一致）。
    #[test]
    fn grid_mesh_is_wellformed_deterministic_and_wound_up() {
        let (va, ia) = water_grid_mesh(8);
        let (vb, ib) = water_grid_mesh(8);
        assert_eq!(va.len(), 81, "（8+1）² 顶点");
        assert_eq!(ia.len(), 6 * 64, "6·8² 索引");
        assert_eq!(va, vb, "顶点两次生成必须一致");
        assert_eq!(ia, ib, "索引两次生成必须一致");
        for v in &va {
            assert!(v.pos_xz[0].abs() <= 1.0 && v.pos_xz[1].abs() <= 1.0);
            assert!((0.0..=1.0).contains(&v.uv[0]) && (0.0..=1.0).contains(&v.uv[1]));
        }
        for &i in &ia {
            assert!((i as usize) < va.len(), "索引越界：{i}");
        }
        // 环绕方向：取一个内部 quad，叉积指向 +Y（与 plane 图元同约定）
        let idx = |ix: u32, iz: u32| iz * 9 + ix;
        let tri = [idx(0, 1), idx(1, 1), idx(1, 0)];
        let p = |i: u32| Vec3::new(va[i as usize].pos_xz[0], 0.0, va[i as usize].pos_xz[1]);
        let cross = (p(tri[1]) - p(tri[0])).cross(p(tri[2]) - p(tri[0]));
        assert!(cross.y > 0.0, "三角形必须从 +Y 看为 CCW：{cross:?}");
    }

    /// 选择键：乱序插入同一首盏（min 的交换律）；空集 None；单盏 0。
    #[test]
    fn water_surface_selection_is_order_independent() {
        let ordered = [0x10u64, 0x20, 0x30, 0x40];
        assert_eq!(select_water_surface(&ordered), Some(0));
        let mut scrambled = ordered;
        scrambled.reverse();
        assert_eq!(
            select_water_surface(&scrambled),
            Some(3),
            "逆序排列后位模式最小者在下标 3——选中的实体集合不变"
        );
        // 两种排列选中的位模式一致
        let pick = |xs: &[u64], i: usize| xs[i];
        assert_eq!(
            pick(&ordered, select_water_surface(&ordered).unwrap()),
            pick(&scrambled, select_water_surface(&scrambled).unwrap()),
        );
        assert_eq!(select_water_surface(&[]), None, "空集 = 无水体");
        assert_eq!(select_water_surface(&[0x99]), Some(0));
    }
}
