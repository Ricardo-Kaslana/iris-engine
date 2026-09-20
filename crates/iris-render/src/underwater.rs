//! 水下（R94）：`UnderwaterFog` 资源 + 相机入水判定 + 水下列长 / 吸收 /
//! 焦散的确定性纯函数（屏幕空间后处理链的一级 pass）。
//!
//! ## 语义摘要
//!
//! - 资源 [`UnderwaterFog`] 在场**且**相机眼点低于选中水面 ⇒ 后处理链在
//!   雾之后、DOF 之前插入一级全屏水下 pass（吸收 + 太阳内散射 + 焦散 +
//!   抖动）。**资源缺席 = 整段不入链**：不建参数缓冲、零 `write_buffer`、
//!   零 pass、链槽位拓扑与 R93 逐字段相同 ⇒ 像素与命令流逐位不变（与
//!   `Weather` / `DdgiSettings` 同族语义——引擎不猜「用户想不想要水下」）。
//! - **水下列长是解析算的，不改水体 pass 的深度写入**（裁定 1）：
//!   [`water_column_distance`] 把「视线穿出水面」这件事解析掉——由
//!   scene_depth 重建的世界点 `world_y` 与相机 `eye_y` 给出射线竖直分量，
//!   与平面 `plane_y` 求交即得穿出距离，列长 = `min(到几何, 到水面)`。
//!   视线水平或向下 ⇒ 永不出射 ⇒ 列长 = 到几何的距离。这样水面像素
//!   （其 scene_depth 是水面**之后**的远平面）不会被误算成整段深水，
//!   从水下仰望的 Snell 窗因此可见——而水 pass 依旧「深度只测不写、
//!   水像素不写 scene_depth」（§8.50⑥ 原样维持，一串既有像素锁不动）。
//! - **确定性**：本模块全是纯函数（无状态、无随机、无全局态），同输入
//!   `to_bits()` 逐位一致；焦散 [`caustic_intensity`] 与 `underwater.wgsl`
//!   的 `uw_caustic` **逐式镜像**（同式同序，两侧同步修改，R92 涟漪 /
//!   fog 同款纪律；CPU↔GPU 不做位级等价声明——sin/cos 库实现不同）。
//! - **时间源** = 世界 `Time` 资源的 `elapsed_secs`（缺席 = 0 ⇒ 焦散静止，
//!   与 R81 波形 / R92 涟漪同语义）。
//!
//! # 诚实限制清单（v1 明确不做）
//!
//! - **波面当平面**：列长用常量 `height` 求交，浪高不参与（裁定 1 的代价；
//!   近水面处观感差一档——水面起伏与焦散条纹无联动）；
//! - **焦散是屏幕空间加性项**：由深度重建的世界 XZ 上算，**无朝向判定**
//!   （垂直墙面也带亮斑）、不参与光照与阴影、无能量守恒（裁定 3）；
//! - **无体积光柱**（god rays 要第二遍 raymarch，另档）；
//! - **抖动是屏幕空间 uv 偏移**，不是折射光追（`wobble` 只做「水不平静」
//!   的扭曲感，边界处会拉扯出边缘重影）；
//! - **单水面**：判定用的是渲染器选中的那一块水面（海洋优先、池塘次之，
//!   与 R93 互斥同序），多水面各自的入水状态不做；
//! - **穿越水面是硬切换**：跨平面的那一帧不插值（无过渡淡入淡出）；
//! - **不做水下音效 / 滤波**（iris-audio 侧另轮）、**不做浮体入水溅射**
//!   （R92「无落地交互」的既有边界）；
//! - **不感知水体颜色**：`UnderwaterFog.color` 由用户给，不与
//!   `OceanSurface::deep_color` 自动同源（两处配色不一致是配置责任，
//!   引擎不猜——自动同源会让「换海色」隐式改水下，反直觉）。

use glam::{Vec2, Vec3};

use iris_core::Resource;

/// 水下效果资源（R94）：**在场即开启**（还需相机在水面下才生效）。
///
/// 六个公开旋钮各有一条非默认值断言（R89 铁律）。全字段须有限，
/// `absorb_depth` 有 1e-3 下限防御（÷0 ⇒ inf 会把画面整片吞成吸收色）。
#[derive(Clone, Copy, Debug, PartialEq, Resource)]
pub struct UnderwaterFog {
    /// 水体吸收色（线性 HDR）：远处像素向此色收敛（Beer-Lambert 的
    /// 「水色」角色，与 R81 的 `deep_color` 同族但不自动同源）。
    pub color: Vec3,
    /// 吸收特征深度（米）：水下列长 = 本值时透过率恰 1/e ≈ 0.368。
    pub absorb_depth: f32,
    /// 太阳内散射强度（乘在「水体色 × 太阳色 × 朝向 × (1−透过率)」上）。
    /// 0 = 纯水色无光感；默认 2.4（带水色着色后约 0.1 的线性辉光）。
    pub sun_inscatter: f32,
    /// 水面方向补光强度（模拟 Snell 窗的亮区：视线越朝上越亮，乘
    /// 「水体色 × 太阳色 × up² × 透过率」）。0 = 无方向性补光；默认 3.2。
    pub sky_window: f32,
    /// 焦散强度（加性，按列长衰减加权，**不带水色**——那是透过波面
    /// 聚焦的日光）。0 = 分支不执行（逐位不变档）。
    pub caustic_strength: f32,
    /// 屏幕空间折射抖动幅度（uv 偏移比例）。0 = 不抖动（逐位不变档）。
    pub wobble: f32,
}

impl Default for UnderwaterFog {
    fn default() -> Self {
        Self {
            // 近岸温海水的吸收色（青绿偏蓝，线性域低值——水下整体压暗是
            // 物理预期，亮度靠太阳内散射与 Snell 窗补回）
            color: Vec3::new(0.018, 0.085, 0.115),
            absorb_depth: 6.0,
            // 三项亮度旋钮的量级按「水体色 × 太阳色」着色后的结果标定
            // （验收图实测：不带水色着色时 0.35 / 0.5 就会把画面洗白）
            sun_inscatter: 2.4,
            sky_window: 3.2,
            caustic_strength: 0.35,
            wobble: 0.004,
        }
    }
}

impl UnderwaterFog {
    /// 字段健全性（加载 / 配置校验用）：全有限 + 非负强度 + 正特征深度。
    pub fn is_valid(&self) -> bool {
        let c = self.color;
        self.absorb_depth > 0.0
            && c.x.is_finite()
            && c.y.is_finite()
            && c.z.is_finite()
            && [
                self.sun_inscatter,
                self.sky_window,
                self.caustic_strength,
                self.wobble,
            ]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.0)
    }

    /// 钳制到可用档（渲染器侧消费前的消毒口，与 `Weather::clamped` 同族）：
    /// 非有限 → 默认值；负强度 → 0；`absorb_depth` 下限 1e-3。
    pub fn clamped(&self) -> Self {
        let fix = |v: f32, dflt: f32| if v.is_finite() { v } else { dflt };
        let d = Self::default();
        Self {
            color: Vec3::new(
                fix(self.color.x, d.color.x).max(0.0),
                fix(self.color.y, d.color.y).max(0.0),
                fix(self.color.z, d.color.z).max(0.0),
            ),
            absorb_depth: fix(self.absorb_depth, d.absorb_depth).max(1e-3),
            sun_inscatter: fix(self.sun_inscatter, d.sun_inscatter).max(0.0),
            sky_window: fix(self.sky_window, d.sky_window).max(0.0),
            caustic_strength: fix(self.caustic_strength, d.caustic_strength).max(0.0),
            wobble: fix(self.wobble, d.wobble).max(0.0),
        }
    }
}

/// 相机是否在水面以下。**等号算水上**（`eye_y == height` 时不进水下——
/// 相机恰贴水面时更常见的意图是「在水上看着水面」，且硬切换帧只发生一次）。
pub fn submerged(eye_y: f32, surface_height: f32) -> bool {
    eye_y < surface_height
}

/// 视线方向的水下列长（米）。
///
/// - `d` = 该像素到几何的线性视距（scene_depth 口径）；
/// - `world_y` = 该几何点的世界 y（由 `d` + `inv_view_proj` 重建）；
/// - `eye_y` / `plane_y` = 相机高度与水面高度。
///
/// 视线向上（`world_y > eye_y`）且相机在水面下（`plane_y > eye_y`）时，
/// 射线在 `d · (plane_y − eye_y) / (world_y − eye_y)` 处穿出水面 ⇒ 列长取
/// 该值与 `d` 的较小者；其余情形（水平 / 向下 / 相机本就在水面上）列长 = `d`。
/// 分母用 1e-6 下限防除零（水平视线 ⇒ 列长趋 `d`，物理上「一路水平游」也
/// 不会出射）。
pub fn water_column_distance(d: f32, world_y: f32, eye_y: f32, plane_y: f32) -> f32 {
    let rise = world_y - eye_y;
    let head = plane_y - eye_y;
    if rise > 1e-6 && head > 0.0 {
        (d * head / rise).min(d)
    } else {
        d
    }
}

/// Beer-Lambert 标量透过率 `exp(−depth / absorb_depth)`（与 R81 水体的
/// 深度吸收同一形式；谱选择性由 `color` 单色承担，v1 不做三通道分开衰减）。
pub fn absorption_through(depth: f32, absorb_depth: f32) -> f32 {
    (-depth / absorb_depth.max(1e-3)).exp()
}

/// 焦散强度 ∈ [0, 1]（世界 XZ + 时间的无状态纯函数，`underwater.wgsl` 的
/// `uw_caustic` 逐式镜像）。
///
/// 形态：两个不同方向的相位场各被一次正弦扭曲，取余弦后求和，正部三次幂
/// ——`a, b ∈ [−1, 1]` ⇒ `((a+b)/2)₊³ ∈ [0, 1]` **有界是构造保证**（单测锁）。
/// 三次幂把能量压成窄亮筋 + 宽暗区，即水底光斑的典型形态；时间只进相位
/// （无频率变化）⇒ 条纹是流动而非闪烁。
pub fn caustic_intensity(p: Vec2, t: f32) -> f32 {
    let q = p + Vec2::new(t.sin() * 0.35, t.cos() * 0.28);
    let a = (q.x * 2.1 + (q.y * 1.3 + t).sin()).cos();
    let b = (q.y * 1.9 - (q.x * 1.7 - t * 1.3).sin()).cos();
    let c = ((a + b) * 0.5).max(0.0);
    c * c * c
}

/// 焦散随深度的衰减（亮斑只在水底附近可辨）：`exp(−column / 衰减深度)`，
/// 衰减深度取 `absorb_depth`（与吸收同一尺度，不留第二个旋钮）。
pub fn caustic_fade(column: f32, absorb_depth: f32) -> f32 {
    absorption_through(column, absorb_depth)
}

/// 水下 pass 的 GPU 参数（176 B POD；与 `shaders/underwater.wgsl` 的
/// `UnderwaterParams` 逐字段对齐，字段序看门见 `wgsl_underwater_mirrors_cpu_formulas`）。
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct UnderwaterParamsRaw {
    /// 主相机（proj × view）⁻¹（NDC → 世界，射线方向重建）。
    pub inv_view_proj: [[f32; 4]; 4],
    /// xyz 相机世界位，w 保留。
    pub eye: [f32; 4],
    /// xyz 单位太阳方向（表面 → 光源）。
    pub sun_dir: [f32; 4],
    /// rgb 太阳辐照色（线性 HDR）。
    pub sun_color: [f32; 4],
    /// rgb 水体吸收色, w 吸收特征深度。
    pub a: [f32; 4],
    /// x 太阳内散射, y Snell 窗补光, z 焦散强度, w 抖动幅度。
    pub b: [f32; 4],
    /// x 波形时间 t, y z_far（scene_depth 天空清空值）。
    pub c: [f32; 4],
    /// x 水面高度。
    pub d: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<UnderwaterParamsRaw>() == 176,
    "UnderwaterParams 必须与 WGSL 布局一致（176B：1×mat4 + 7×vec4）"
);

#[cfg(test)]
mod tests {
    use super::*;
    use glam::vec2;

    #[test]
    fn submerged_boundary_is_above_water() {
        assert!(!submerged(0.0, 0.0), "等号算水上（硬切换只发生一次）");
        assert!(submerged(-0.001, 0.0));
        assert!(!submerged(0.001, 0.0));
        // 非默认水面高度档
        assert!(submerged(2.9, 3.0));
        assert!(!submerged(3.1, 3.0));
    }

    #[test]
    fn column_distance_matches_analytic_crossing() {
        // 相机 y=0、水面 y=2；视线打到 (d=10, world_y=4) ⇒ 穿出点 = 10·2/4 = 5
        let c = water_column_distance(10.0, 4.0, 0.0, 2.0);
        assert!((c - 5.0).abs() < 1e-5, "解析锚：5.0，实得 {c}");
        // 视线向上但几何点在水面之下 ⇒ 未出射 ⇒ 列长 = d
        assert_eq!(water_column_distance(10.0, 1.0, 0.0, 2.0), 10.0);
        // 水平 / 向下视线 ⇒ d（永不出射）
        assert_eq!(water_column_distance(10.0, 0.0, 0.0, 2.0), 10.0);
        assert_eq!(water_column_distance(10.0, -3.0, 0.0, 2.0), 10.0);
        // 相机本在水面上（head ≤ 0）⇒ 不钳制（防御档，判定层已排除）
        assert_eq!(water_column_distance(10.0, 20.0, 5.0, 2.0), 10.0);
        // 单调：越朝上（world_y 越大）穿出越早
        let mut prev = f32::INFINITY;
        for k in 0..8 {
            let wy = 3.0 + k as f32 * 0.5;
            let v = water_column_distance(20.0, wy, 0.0, 2.0);
            assert!(v <= prev + 1e-6, "朝上越多列长越短（world_y={wy}）");
            prev = v;
        }
    }

    #[test]
    fn absorption_is_exponential_and_bounded() {
        assert_eq!(absorption_through(0.0, 6.0).to_bits(), 1.0f32.to_bits());
        let e = absorption_through(6.0, 6.0);
        assert!((e - 1.0 / std::f32::consts::E).abs() < 1e-6, "1/e 锚：{e}");
        // 100 倍特征深度 ⇒ e^-100 ≈ 3.7e-44（首稿写 60 m = 10 倍，
        // 实测 e^-10 = 4.5e-5，被自己的阈值当场抓住）
        assert!(absorption_through(600.0, 6.0) < 1e-20);
        // 单调递减 + 非正特征深度的下限防御（不得 NaN / inf）
        let mut prev = 2.0f32;
        for k in 0..10 {
            let v = absorption_through(k as f32, 6.0);
            assert!(v <= prev + 1e-7 && v.is_finite());
            prev = v;
        }
        assert!(
            absorption_through(3.0, 0.0).is_finite(),
            "absorb_depth=0 须有界"
        );
        assert!(absorption_through(3.0, -5.0).is_finite());
    }

    #[test]
    fn caustics_are_bounded_moving_and_deterministic() {
        let mut seen = std::collections::BTreeSet::new();
        for i in 0..64 {
            for j in 0..64 {
                let p = vec2(i as f32 * 0.37, j as f32 * 0.41);
                let v = caustic_intensity(p, 1.5);
                assert!(
                    (0.0..=1.0).contains(&v),
                    "有界是构造保证（p={p:?} 实得 {v}）"
                );
                seen.insert(v.to_bits());
            }
        }
        // 阈值依据：`max(·, 0)` 把约一半定义域钳成**同一个** 0.0（暗区是设计
        // 而非缺陷），故 4096 个采样里唯一值数只要求过半——2029 实测。
        assert!(
            seen.len() > 1500,
            "空间上必须非常数（唯一值数），实得 {}",
            seen.len()
        );
        // 时间维度：同一位置不同时刻要变（否则焦散静止）
        let p = vec2(3.0, -2.0);
        assert_ne!(
            caustic_intensity(p, 0.0).to_bits(),
            caustic_intensity(p, 1.0).to_bits()
        );
        // 逐位确定性：同输入两次全等
        for k in 0..16 {
            let a = caustic_intensity(p, k as f32 * 0.31);
            let b = caustic_intensity(p, k as f32 * 0.31);
            assert_eq!(a.to_bits(), b.to_bits());
        }
        // 非零覆盖：至少存在亮筋（最大值显著 > 0，否则整段是死项）
        let peak = (0..200)
            .map(|i| caustic_intensity(vec2(i as f32 * 0.13, i as f32 * 0.21), 0.7))
            .fold(0.0f32, f32::max);
        assert!(peak > 0.5, "焦散须有可辨亮筋，实测峰 {peak}");
    }

    #[test]
    fn caustic_fade_follows_column_depth() {
        assert_eq!(caustic_fade(0.0, 6.0), 1.0);
        assert!(caustic_fade(30.0, 6.0) < 0.01, "深水处焦散应衰减殆尽");
        assert!(caustic_fade(6.0, 6.0) < caustic_fade(3.0, 6.0));
    }

    #[test]
    fn resource_defaults_are_valid_and_clamp_sanitizes() {
        let u = UnderwaterFog::default();
        assert!(u.is_valid(), "默认档必须自带健全");
        assert!(u.absorb_depth > 0.0);
        assert!(
            u.caustic_strength > 0.0 && u.wobble > 0.0,
            "默认要能看到效果"
        );
        // 非默认值档 + 消毒口
        let bad = UnderwaterFog {
            absorb_depth: f32::NAN,
            caustic_strength: -3.0,
            wobble: f32::INFINITY,
            color: Vec3::new(-1.0, 0.0, 0.0),
            ..UnderwaterFog::default()
        };
        assert!(!bad.is_valid());
        let ok = bad.clamped();
        assert!(ok.is_valid(), "clamped 后必须可用");
        assert_eq!(ok.caustic_strength, 0.0, "负强度钳到 0（= 分支不执行）");
        assert_eq!(ok.color.x, 0.0);
        assert!(ok.absorb_depth >= 1e-3);
    }

    #[test]
    fn wgsl_underwater_mirrors_cpu_formulas() {
        // 影子契约：焦散与列长的 WGSL 拷贝必须与本模块同式（R92 涟漪口径）。
        let src = include_str!("../shaders/underwater.wgsl");
        for expect in [
            // caustic_intensity 的三条骨架式（改 CPU 必须同步 WGSL）
            // WGSL 无方法调用语法（`t.sin()` 直接解析失败）⇒ 镜像是
            // 「同式不同形」：自由函数式 sin/cos，参数序与运算序一致。
            "let q = p + vec2(sin(t) * 0.35, cos(t) * 0.28);",
            "let a = cos(q.x * 2.1 + sin(q.y * 1.3 + t));",
            "let b = cos(q.y * 1.9 - sin(q.x * 1.7 - t * 1.3));",
            "let c = max((a + b) * 0.5, 0.0);",
            // 列长钳制（water_column_distance 同式）
            "d * head / rise",
            // 参数布局影子（改 Rust POD 必须同步 WGSL struct）
            "struct UnderwaterParams {",
            "a: vec4<f32>,               // rgb 水体吸收色, w 吸收特征深度（米）",
        ] {
            assert!(
                src.contains(expect),
                "underwater.wgsl 缺镜像片段 `{expect}`（改 CPU 侧必须同步着色器）"
            );
        }
    }
}
