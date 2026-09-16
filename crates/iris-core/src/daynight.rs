//! 昼夜循环（R80）：太阳弧解析解 + 光照包络（纯 CPU，零渲染依赖）。
//!
//! **语义**：[`DayNightCycle`] 资源持有一个归一化相位 `phase ∈ [0, 1)`，
//! 约定 **0 = 日出、0.25 = 正午、0.5 = 日落、0.75 = 午夜**；每帧按
//! `advance(delta_seconds)` 推进（`phase + dt × cycles_per_second` 对 1
//! 取模回绕）。相位是唯一状态——太阳方向 / 色温 / 强度 / 环境光全部是
//! 相位的**纯函数**（本模块的自由函数），同输入逐位一致，无累积漂移
//! （f64 相位推进 + 固定步长推荐；可变步长下同 dt 序列仍逐位一致，
//! 但长会话的可变 dt 序列本身不可复现——确定性回放请走固定步长）。
//!
//! **消费形态**：`iris-engine` 的 `DayNightPlugin`（Update 阶段）每帧把
//! [`sun_direction`] / [`sun_envelope`] / [`ambient_envelope`] 写进渲染器
//! 的 `DirectionalLight` 资源——注意换算：本模块的太阳方向是**指向太阳**
//! 的单位向量（天上那一点），而 `DirectionalLight.direction` 是**光传播
//! 方向**（从光源指向场景），故写入值 = `−sun_direction(phase)`。
//! `DirectionalLight` 资源缺席时相位照常推进（相位演化是模拟事实，与
//! 渲染资产无关），灯光写入跳过。**资源缺席语义**：`DayNightCycle`
//! 未注册 = 插件零操作，`DirectionalLight` 一个字段都不被触碰。
//!
//! # 诚实限制清单（v1 明确不做，ARCHITECTURE §8.49）
//!
//! - **星空 / 月亮不做**：夜间光照只有 [`ambient_envelope`] 的下限底光，
//!   强度归零的太阳项不发光（无月光光源、无星空天空盒——天空渲染属
//!   `iris-render` 的 `SkySettings`，不随相位联动）；
//! - **SkySettings / FogSettings 不自动联动**：大气强度、雾密度、泛光
//!   等渲染参数是用户手动资源，昼夜只驱动方向光四字段
//!   （direction / color / intensity / ambient）；
//! - **太阳弧是单平面圆弧**：太阳在固定的倾斜平面内做圆周运动（无季节
//!   倾角模型、无纬度 / 日期输入，方位年不变）；
//! - **包络是艺术化分段线性曲线**：色温 / 强度按太阳正弦仰角分段线性
//!   插值，非物理辐射量纲（调参以观感为准）。

use glam::Vec3;

use crate::Resource;

/// 圆周率 × 2（f64，相位 → 弧度的换算系数）。
const TAU_F64: f64 = std::f64::consts::TAU;

/// 太阳弧平面的默认倾角（弧度）：35°。
///
/// 倾角把「过天顶的正东西弧」绕日出-日落轴整体压斜——正午太阳仰角 =
/// 90° − 35° = 55°，不在正头顶（阴影可见，画面不呆板），日出 / 日落
/// 位置仍在地平线上不动。
pub const DEFAULT_ARC_TILT_RADIANS: f32 = 35.0f32.to_radians();

/// 晨昏过渡带的仰角半宽（正弦仰角单位）：太阳正弦仰角从 0 升到该值
/// 的区间内，强度与环境光从夜间值线性爬到全昼值、色温从暖橙过渡到正午
/// 白。取 0.5（≈ 30° 仰角）：真实大气消光主要集中在低仰角段，30° 以上
/// 色温基本恒定，与观感一致。
pub const DAY_RAMP: f32 = 0.5;

/// 地平暖橙色温（线性空间）：日出 / 日落与夜侧的太阳色。
pub const WARM_COLOR: Vec3 = Vec3::new(1.0, 0.5, 0.2);

/// 正午白色温（线性空间）：与 `DirectionalLight::default` 的颜色一致
/// （全昼包络顶值 = 引擎默认日光，兼容观感）。
pub const NOON_COLOR: Vec3 = Vec3::new(1.0, 0.98, 0.92);

/// 夜间环境光下限（线性空间，暗蓝底光）：午夜不黑屏的保底照明——
/// 月光量级的艺术化取值，供 `_FLOOR` 断言与「夜间可辨轮廓」验收。
pub const NIGHT_AMBIENT: Vec3 = Vec3::new(0.035, 0.045, 0.07);

/// 全昼环境光顶值（线性空间，微冷白）：与体积雾快照场景的既用环境光
/// 同值（`0.16, 0.18, 0.22`），白天观感与既有场景连续。
pub const DAY_AMBIENT: Vec3 = Vec3::new(0.16, 0.18, 0.22);

/// 昼夜循环资源：归一化相位 + 速率 + 暂停开关。
///
/// 字段公开（与引擎资源风格一致）；不变式：[`Self::phase`] 恒在
/// `[0, 1)`（[`Self::advance`] 与手工赋值都应维持——手工塞入越界值时
/// 下一次 `advance` 的取模回绕会把它拉回，但包络求值对越界相位同样
/// 安全：三角函数周期延拓，无 panic / 无 NaN）。
///
/// Default：phase 0.08（清晨，低角度暖光开画）、速率 1/600（10 分钟
/// 一个游戏日）、不暂停。
#[derive(Debug, Clone, Resource)]
pub struct DayNightCycle {
    /// 归一化相位（0 = 日出、0.25 = 正午、0.5 = 日落、0.75 = 午夜）。
    pub phase: f64,
    /// 游戏日速率：每现实秒推进的相位（1/600 = 10 分钟一天；
    /// 1/60 = 一分钟一天）。
    pub cycles_per_second: f64,
    /// 暂停：true 时 [`Self::advance`] 不推进相位（灯光派生值仍每帧按
    /// 当前相位重算——暂停冻结的是时间，不是画面）。
    pub paused: bool,
}

impl Default for DayNightCycle {
    fn default() -> Self {
        Self {
            phase: 0.08,
            cycles_per_second: 1.0 / 600.0,
            paused: false,
        }
    }
}

impl DayNightCycle {
    /// 推进相位：`phase += delta_seconds × cycles_per_second`，对 1 取模
    /// 回绕到 `[0, 1)`（`rem_euclid`——负 `delta_seconds` 同样安全，倒拨
    /// 跨 0 点回绕到高端）。f64 运算，同 dt 序列两次推进逐位一致。
    /// `paused = true` 时整段跳过（含浮点运算）。
    pub fn advance(&mut self, delta_seconds: f64) {
        if self.paused {
            return;
        }
        self.phase = (self.phase + delta_seconds * self.cycles_per_second).rem_euclid(1.0);
    }

    /// 是否处于夜间（太阳严格低于地平线，见 [`is_night`]）。
    pub fn is_night(&self) -> bool {
        is_night(self.phase)
    }
}

/// 太阳正弦仰角：`sin(2π × phase)`。
///
/// 包络曲线的公共自变量——日出 / 日落为 0、正午为 1、午夜为 −1，
/// 与「仰角 = 正弦弧」的约定一致（相位在弧上均匀，正弦仰角随之摆动）。
pub fn sun_elevation_factor(phase: f64) -> f64 {
    (phase * TAU_F64).sin()
}

/// 太阳方向（**指向太阳**的单位向量，默认倾角
/// [`DEFAULT_ARC_TILT_RADIANS`]）。
///
/// 解析式：基准弧 `D(θ) = (cos θ, sin θ, 0)`（θ = 2π × phase——日出
/// +X 地平、正午天顶、日落 −X 地平）绕 +X 轴旋转倾角 φ 后
/// `= (cos θ, sin θ·cos φ, sin θ·sin φ)`——模长恒 1（正交分解），结果
/// 经 `normalize_or_zero` 收整（语义兜底：任何退化输入不产生 NaN）。
/// 夜间（phase ∈ (0.5, 1)）y 为负——太阳在地平线下，方向仍连续。
pub fn sun_direction(phase: f64) -> Vec3 {
    sun_direction_with_tilt(phase, DEFAULT_ARC_TILT_RADIANS)
}

/// 指定倾角的太阳方向（见 [`sun_direction`]；倾角 0 = 过天顶的东西弧，
/// 倾角越大正午太阳越低）。
pub fn sun_direction_with_tilt(phase: f64, tilt_radians: f32) -> Vec3 {
    let (s, c) = (phase * TAU_F64).sin_cos();
    let (st, ct) = (tilt_radians as f64).sin_cos();
    Vec3::new(c as f32, (s * ct) as f32, (s * st) as f32).normalize_or_zero()
}

/// 太阳光照包络：`(色温, 强度)`。
///
/// 分段线性解析曲线（自变量 = 正弦仰角 e，公共过渡斜坡
/// `k = clamp(e / DAY_RAMP, 0, 1)`）：
///
/// ```text
/// 强度   = k                          （夜间 e ≤ 0 恒 0；正午 e = 1 恒 1）
/// 色温   = lerp(暖橙, 正午白, k)      （夜间驻留暖橙——强度 0 不可见，
///                                       保持连续不跳变）
/// ```
///
/// 形状依据：真实低仰角阳光的大气消光使色温偏橙、强度骤降，30° 以上
/// 色温基本恒定——过渡带半宽取 `DAY_RAMP = 0.5`（正弦仰角 ≈ 30°）。
pub fn sun_envelope(phase: f64) -> (Vec3, f32) {
    let k = day_factor(phase);
    (warm_to_noon(k), k)
}

/// 环境光包络：昼间微冷白 ↔ 夜间暗蓝底光（**夜间不黑屏**）。
///
/// `lerp(NIGHT_AMBIENT, DAY_AMBIENT, k)`——与太阳项同一条过渡斜坡：
/// 晨昏同时起落，夜间驻留 [`NIGHT_AMBIENT`] 下限（值写明：
/// `(0.035, 0.045, 0.07)`，月光量级的暗蓝底光）。
pub fn ambient_envelope(phase: f64) -> Vec3 {
    NIGHT_AMBIENT.lerp(DAY_AMBIENT, day_factor(phase))
}

/// 是否夜间：太阳**严格**低于地平线（正弦仰角 < 0，即
/// phase ∈ (0.5, 1)）。
///
/// 日出 / 日落瞬间（正弦仰角恰为 0）不算夜——晨昏过渡归昼侧。
pub fn is_night(phase: f64) -> bool {
    sun_elevation_factor(phase) < 0.0
}

/// 公共昼间斜坡：`clamp(正弦仰角 / DAY_RAMP, 0, 1)`。
///
/// 夜间（e ≤ 0）为 0，`e ≥ DAY_RAMP` 后恒 1；包络三函数共用同一斜坡
/// ——强度 / 色温 / 环境光同相同步起落。
fn day_factor(phase: f64) -> f32 {
    (sun_elevation_factor(phase) as f32 / DAY_RAMP).clamp(0.0, 1.0)
}

/// 暖橙 → 正午白的线性插值（k = 0 暖橙、k = 1 正午白）。
fn warm_to_noon(k: f32) -> Vec3 {
    WARM_COLOR.lerp(NOON_COLOR, k)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 相位约定常量：0 / 0.25 / 0.5 / 0.75 的正弦仰角解析解为
    /// 0 / 1 / 0 / −1（f64 三角函数在精确象限点的舍入 ≤ 1 ulp，
    /// ±1e-5 容差远宽于舍入）。
    #[test]
    fn elevation_factor_at_quarter_points() {
        assert!((sun_elevation_factor(0.0) - 0.0).abs() < 1e-5, "日出仰角 0");
        assert!(
            (sun_elevation_factor(0.25) - 1.0).abs() < 1e-5,
            "正午仰角顶点"
        );
        assert!((sun_elevation_factor(0.5) - 0.0).abs() < 1e-5, "日落仰角 0");
        assert!(
            (sun_elevation_factor(0.75) - (-1.0)).abs() < 1e-5,
            "午夜仰角谷底"
        );
    }

    /// 太阳方向四象限解析解（手算：倾角 35°，D = (cosθ, sinθ·cosφ,
    /// sinθ·sinφ)）：日出 +X 地平、正午仰角 = 90° − 35° = 55° 偏 +Z、
    /// 日落 −X 地平、午夜在地平线下对称位置。
    #[test]
    fn sun_direction_quarter_points_match_closed_form() {
        let tilt = DEFAULT_ARC_TILT_RADIANS;
        let (st, ct) = (tilt.sin(), tilt.cos());
        // 日出：(+1, 0, 0)——地平线上正东
        let d = sun_direction(0.0);
        assert!((d - Vec3::new(1.0, 0.0, 0.0)).length() < 1e-5, "日出 {d:?}");
        // 正午：(0, cos35°, sin35°)——仰角 55°
        let d = sun_direction(0.25);
        assert!(d.x.abs() < 1e-5, "正午无东西分量：{d:?}");
        assert!((d.y - ct).abs() < 1e-5, "正午 y = cos 35°：{d:?}");
        assert!((d.z - st).abs() < 1e-5, "正午 z = sin 35°：{d:?}");
        let elev = d.y.asin();
        assert!(
            (elev - (std::f32::consts::FRAC_PI_2 - tilt)).abs() < 1e-5,
            "正午仰角应 = 90° − 倾角：{elev}"
        );
        // 日落：(−1, 0, 0)
        let d = sun_direction(0.5);
        assert!(
            (d - Vec3::new(-1.0, 0.0, 0.0)).length() < 1e-5,
            "日落 {d:?}"
        );
        // 午夜：(0, −cos35°, −sin35°)——地平线下
        let d = sun_direction(0.75);
        assert!(
            (d.y + ct).abs() < 1e-5 && (d.z + st).abs() < 1e-5,
            "午夜方向 {d:?} 应在地平线下"
        );
        assert!(sun_direction(0.75).y < 0.0, "夜间仰角为负");
    }

    /// 模长恒 1、全程无 NaN（1024 步扫相位；倾角 0 / 默认 / 90° 三组
    /// ——normalize_or_zero 语义下任何有限输入不得产生 NaN）。
    #[test]
    fn sun_direction_is_unit_and_finite_over_full_cycle() {
        for tilt in [
            0.0f32,
            DEFAULT_ARC_TILT_RADIANS,
            std::f32::consts::FRAC_PI_2,
        ] {
            for i in 0..1024 {
                let phase = i as f64 / 1024.0;
                let d = sun_direction_with_tilt(phase, tilt);
                assert!(d.is_finite(), "相位 {phase} 倾角 {tilt} 出现非有限值 {d:?}");
                assert!(
                    (d.length() - 1.0).abs() < 1e-5,
                    "相位 {phase} 模长 {} 应为 1",
                    d.length()
                );
            }
        }
    }

    /// 倾角几何：倾角 0 → 正午在天顶（方向 = +Y）；倾角 90° → 正午太阳
    /// 压到地平线（方向水平）；日出 / 日落点不随倾角移动（旋转轴过
    /// 日出日落点）。
    #[test]
    fn tilt_controls_noon_elevation_only() {
        let noon = sun_direction_with_tilt(0.25, 0.0);
        assert!(
            (noon - Vec3::Y).length() < 1e-5,
            "倾角 0 的正午应在天顶：{noon:?}"
        );
        let noon = sun_direction_with_tilt(0.25, std::f32::consts::FRAC_PI_2);
        assert!(noon.y.abs() < 1e-5, "倾角 90° 的正午应压到地平线：{noon:?}");
        for tilt in [0.0f32, 0.3, 1.2] {
            let rise = sun_direction_with_tilt(0.0, tilt);
            assert!(
                (rise - Vec3::X).length() < 1e-5,
                "倾角 {tilt} 的日出点应仍在 +X：{rise:?}"
            );
        }
    }

    /// 包络端点精确值：正午 =（正午白, 1）；日出 / 日落 / 午夜 =
    /// （暖橙, 0）——强度夜间归零、色温驻留暖橙保持连续。容差依据：
    /// f64 sin 在象限点的舍入（sin(π) = 1.22e-16 著名伪影）经斜坡放大
    /// 后 ≤ 1e-15，±1e-5 远宽于舍入。
    #[test]
    fn sun_envelope_endpoints_exact() {
        let (color, intensity) = sun_envelope(0.25);
        assert_eq!(intensity, 1.0, "正午全强（clamp 上界精确）");
        assert_eq!(color, NOON_COLOR, "正午白");
        for phase in [0.0f64, 0.5, 0.75] {
            let (color, intensity) = sun_envelope(phase);
            assert!(intensity < 1e-5, "相位 {phase} 无直射（实际 {intensity}）");
            assert!(
                (color - WARM_COLOR).length() < 1e-5,
                "相位 {phase} 色温驻留暖橙：{color:?}"
            );
        }
    }

    /// 过渡带中点解析对照：相位 1/48 → θ = 2π/48 = 7.5°，正弦仰角
    /// e = sin 7.5° ≈ 0.130526，斜坡 k = e / 0.5 ≈ 0.261052——强度与
    /// 色温逐分量按定义式手算（±1e-5 写明：e 的 f64 舍入 ≤ 1 ulp）。
    #[test]
    fn sun_envelope_mid_ramp_matches_closed_form() {
        let phase = 1.0 / 48.0;
        let k = sun_elevation_factor(phase) as f32 / DAY_RAMP;
        let (color, intensity) = sun_envelope(phase);
        assert!((intensity - k).abs() < 1e-5, "强度 = 斜坡 {k}");
        // 手算期望：逐分量 lerp(WARM, NOON, k)
        let expect = Vec3::new(
            WARM_COLOR.x + (NOON_COLOR.x - WARM_COLOR.x) * k,
            WARM_COLOR.y + (NOON_COLOR.y - WARM_COLOR.y) * k,
            WARM_COLOR.z + (NOON_COLOR.z - WARM_COLOR.z) * k,
        );
        assert!(
            (color - expect).length() < 1e-5,
            "色温 {color:?} 应为 {expect:?}"
        );
    }

    /// 强度包络的单调性：升段（日出 → 正午）非降、降段（正午 → 日落）
    /// 非增、夜间（日落 → 次日日出）恒 0。
    #[test]
    fn sun_intensity_monotonic_segments() {
        let steps = 256;
        let mut prev = sun_envelope(0.0).1;
        for i in 1..=steps {
            let phase = 0.25 * i as f64 / steps as f64;
            let v = sun_envelope(phase).1;
            assert!(v >= prev, "升段相位 {phase} 强度 {v} < 前值 {prev}");
            prev = v;
        }
        let mut prev = sun_envelope(0.25).1;
        for i in 1..=steps {
            let phase = 0.25 + 0.25 * i as f64 / steps as f64;
            let v = sun_envelope(phase).1;
            assert!(v <= prev, "降段相位 {phase} 强度 {v} > 前值 {prev}");
            prev = v;
        }
        for i in 0..=steps {
            let phase = 0.5 + 0.5 * i as f64 / steps as f64;
            // sin(π) = 1.22e-16 伪影 → 强度 ≤ 1e-15，容差判据见端点测试
            assert!(sun_envelope(phase).1 < 1e-5, "夜间相位 {phase} 强度应恒 0");
        }
    }

    /// 环境光包络：端点精确（正午 = 全昼顶值；日出 / 午夜 = 夜间下限
    /// ——下限生效不黑屏），中点 = 线性插值解析值。
    #[test]
    fn ambient_envelope_endpoints_and_floor() {
        assert_eq!(ambient_envelope(0.25), DAY_AMBIENT, "正午全昼环境光");
        for phase in [0.0f64, 0.5, 0.75] {
            assert_eq!(
                ambient_envelope(phase),
                NIGHT_AMBIENT,
                "相位 {phase} 应为夜间下限底光（不黑屏）"
            );
        }
        // 中点：相位 1/48 与太阳项同一斜坡 k
        let k = sun_elevation_factor(1.0 / 48.0) as f32 / DAY_RAMP;
        let expect = NIGHT_AMBIENT.lerp(DAY_AMBIENT, k);
        assert!(
            (ambient_envelope(1.0 / 48.0) - expect).length() < 1e-6,
            "环境光中点应与斜坡插值一致"
        );
    }

    /// 夜间判定：严格低于地平线才算夜；日出 / 日落瞬间（= 0）归昼侧。
    #[test]
    fn is_night_strictly_below_horizon() {
        assert!(!is_night(0.0), "日出瞬间不算夜");
        assert!(!is_night(0.25));
        assert!(!is_night(0.5), "日落瞬间不算夜");
        assert!(is_night(0.75));
        assert!(is_night(0.6));
        assert!(is_night(0.999));
        assert!(!is_night(0.49));
    }

    /// 推进确定性：同 dt 序列两次推进，phase 逐位一致（to_bits 断言
    /// ——确定性铁律的形态锚点）。
    #[test]
    fn advance_is_bitwise_deterministic() {
        let dt = [1.0f64 / 60.0; 600];
        let mut a = DayNightCycle::default();
        let mut b = DayNightCycle::default();
        for step in dt {
            a.advance(step);
        }
        for step in dt {
            b.advance(step);
        }
        assert_eq!(a.phase.to_bits(), b.phase.to_bits(), "同输入必须逐位一致");
    }

    /// 回绕：0.999 + 0.002（速率 1）→ rem_euclid 回绕到 ≈ 0.001；
    /// 负步长倒拨跨 0 点回绕到高端；恰好一整圈落在 [0, 1) 内。
    #[test]
    fn advance_wraps_at_phase_boundaries() {
        let mut cycle = DayNightCycle {
            phase: 0.999,
            cycles_per_second: 1.0,
            paused: false,
        };
        cycle.advance(0.002);
        assert!(
            (cycle.phase - 0.001).abs() < 1e-12,
            "正向回绕 {:#?}",
            cycle.phase
        );
        cycle.advance(-0.002);
        assert!(
            (cycle.phase - 0.999).abs() < 1e-12,
            "负向倒拨应回绕到高端 {:#?}",
            cycle.phase
        );
        // 恰好一整圈：0.25 + 1.0 = 1.25（f64 精确），rem_euclid 后精确
        // 回到 0.25——整圈推进的值恒等（无漂移的直接证据）
        let mut cycle = DayNightCycle {
            phase: 0.25,
            cycles_per_second: 1.0,
            paused: false,
        };
        cycle.advance(1.0);
        assert!((0.0..1.0).contains(&cycle.phase), "相位必须回绕进 [0,1)");
        assert_eq!(cycle.phase, 0.25, "整圈推进应精确回到原相位");
    }

    /// 暂停：advance 整段跳过（相位逐位不变）；默认值核对
    /// （phase 0.08 清晨 / 速率 1/600 / 不暂停）。
    #[test]
    fn advance_respects_pause_and_defaults_hold() {
        let mut cycle = DayNightCycle {
            paused: true,
            ..Default::default()
        };
        let before = cycle.phase.to_bits();
        cycle.advance(1000.0);
        assert_eq!(cycle.phase.to_bits(), before, "暂停时相位不得推进");
        let d = DayNightCycle::default();
        assert_eq!(d.phase, 0.08, "默认清晨相位");
        assert!(
            (d.cycles_per_second - 1.0 / 600.0).abs() < 1e-15,
            "默认 10 分钟一天"
        );
        assert!(!d.paused);
        assert!(!d.is_night(), "默认相位 0.08 在昼侧");
    }
}
