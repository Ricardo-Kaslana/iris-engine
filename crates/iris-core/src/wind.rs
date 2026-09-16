//! 全局风场（R80）：`Wind` 资源 + 确定性风采样纯函数（零渲染依赖）。
//!
//! **接口轮定位（诚实声明在案）**：本模块是路线图 v5 立项的**共用接口
//! 轮**——R82 粒子 v2（GPU 化 + 拖尾）、R90–R91 毛发、R92 降水与闭合池
//! 的布料 / 植被四条下游线都从这里读风。v1 引擎内**没有消费者**
//! （`DefaultPlugins` 不注册、不自动驱动任何渲染表现）——这不是摆设
//! API，接口形状按下游需求定稿：消费形态 = 下游系统读取 `Wind` 资源并
//! 调用 [`wind_sample`]，典型姿势：
//!
//! ```ignore
//! // 下游系统内（&World 并行系统）：
//! let Some(wind) = world.try_resource::<Wind>() else { return; };
//! let gust = wind_sample(&wind, world_pos, time.elapsed_secs());
//! // gust 是含阵风调制的风速向量（m/s 量级）——驱动粒子漂移 / 毛发
//! // 摆动 / 植被弯曲时自行决定消费哪个分量（幅值 / 方向）
//! ```
//!
//! **采样语义**：[`wind_sample`] = 恒风分量（单位风向 × strength）+
//! 阵风调制（1 + gustiness × g，g ∈ [−1, 1]）——g 是 (x, z, t) 格点上
//! 的三线性值噪声，格点值来自 splitmix64 哈希流（**参照
//! `iris-render::particles` 的 mix64 范式**：无 rand、无全局状态，同
//! 输入任何时候重跑逐位一致）。量化步长定稿：水平格 [`GRID_METERS`]
//! = 2 m、时间格 [`TIME_STEP_SECS`] = 0.25 s——阵风结构的空间尺度与
//! 秒级起伏的折中（更细的格哈希成本不变但下游视觉趋白噪声）。
//!
//! # 诚实限制清单（v1 明确不做）
//!
//! - **阵风只调制幅值，不旋转风向**：风向恒为资源给定的水平方向
//!   （下游需要方向扰动时自行在消费侧加摆动，或留后续轮扩展）；
//! - **高度无关**：采样只用 (x, z, t)，同一垂直线上风速相同（无风切
//!   变 / 边界层剖面）；
//! - **无湍流细节**：单个八度的三线性值噪声（无分形叠加——下游毛发
//!   / 降水若嫌过于平滑再立项加倍频程）；
//! - **坐标量级**：位置 / 时间量化到 i64 格点，f32 坐标在 ±2²⁴ m 内
//!   量化无损（游戏场景远低于此）；超出后 as 转换饱和，仍确定但不
//!   再保证格点唯一性。

use glam::Vec3;

use crate::Resource;

/// 水平量化步长（米）：阵风场的空间格尺寸。
pub const GRID_METERS: f32 = 2.0;

/// 时间量化步长（秒）：阵风场的时间格尺寸。
pub const TIME_STEP_SECS: f32 = 0.25;

/// 全局风场资源。
///
/// Default：+X 恒向 3 m/s 微风、阵风度 0.5、种子 = 黄金比率常数
/// （0x9E37…——与 `particles.rs` 第二随机路的偏移常数同源，纯惯例）。
#[derive(Debug, Clone, Copy, Resource)]
pub struct Wind {
    /// 水平风向（世界空间）。采样时**先取水平分量（y 置 0）再单位化**
    /// ——塞入带俯仰的向量只保留其水平投影；水平分量退化（零向量）
    /// 时恒风分量为零向量（无 NaN）。非单位长度无妨（采样时归一）。
    pub direction: Vec3,
    /// 恒风强度（m/s 量纲，消费方自行解释）。
    pub strength: f32,
    /// 阵风度：0 = 纯恒风（输出与时间 / 位置无关）；1 = 阵风可从
    /// 完全平息（g = −1）吹到两倍恒风（g = +1）。
    pub gustiness: f32,
    /// 哈希流种子：区分不同风场（同种子同风场，逐位可复现）。
    pub seed: u64,
}

impl Default for Wind {
    fn default() -> Self {
        Self {
            direction: Vec3::X,
            strength: 3.0,
            gustiness: 0.5,
            seed: 0x9E37_79B9_7F4A_7C15,
        }
    }
}

/// 风场采样（纯函数）：位置 `world_pos` 与时刻 `t` 处的风速向量。
///
/// 返回 `单位风向 × strength × (1 + gustiness × g)`，其中 g 为
/// [`gust_scalar`] 给出的阵风标量 ∈ [−1, 1]——上界
/// `|输出| ≤ strength × (1 + gustiness)` 由「单位方向 × 非负标量」的
/// 构造直接保证（`gustiness < 0` 视为反向调制，界相应为
/// `|1 + gustiness|`，同样有界；测试按通用式
/// `strength × (1 + |gustiness|)` 断言）。
pub fn wind_sample(wind: &Wind, world_pos: Vec3, t: f32) -> Vec3 {
    // 水平投影 → 单位化（零向量安全：normalize_or_zero，下游乘法不产生 NaN）
    let dir = Vec3::new(wind.direction.x, 0.0, wind.direction.z).normalize_or_zero();
    let g = gust_scalar(world_pos, t, wind.seed);
    dir * (wind.strength * (1.0 + wind.gustiness * g))
}

/// 阵风标量场 g ∈ [−1, 1]：(x, z, t) 三维格点值噪声。
///
/// 位置 / 时间除以步长量化到格，角落值 = mix64 哈希 → [−1, 1)，格内
/// 五次平滑插值（quintic fade，C2 连续——下游消费的加速度不跳变）。
/// 同 (pos, t, seed) 任何时候求值逐位一致。
fn gust_scalar(pos: Vec3, t: f32, seed: u64) -> f32 {
    let x = pos.x / GRID_METERS;
    let z = pos.z / GRID_METERS;
    let s = t / TIME_STEP_SECS;
    let ix = x.floor() as i64;
    let iz = z.floor() as i64;
    let it = s.floor() as i64;
    let (fx, fz, fs) = (
        smootherstep(x - ix as f32),
        smootherstep(z - iz as f32),
        smootherstep(s - it as f32),
    );
    // 三线性插值（角落求值次序固定——确定性不依赖浮点结合序的巧合）
    let c00 = flerp(
        flerp(corner(ix, iz, it, seed), corner(ix + 1, iz, it, seed), fx),
        flerp(
            corner(ix, iz + 1, it, seed),
            corner(ix + 1, iz + 1, it, seed),
            fx,
        ),
        fz,
    );
    let c01 = flerp(
        flerp(
            corner(ix, iz, it + 1, seed),
            corner(ix + 1, iz, it + 1, seed),
            fx,
        ),
        flerp(
            corner(ix, iz + 1, it + 1, seed),
            corner(ix + 1, iz + 1, it + 1, seed),
            fx,
        ),
        fz,
    );
    flerp(c00, c01, fs)
}

/// 标量线性插值（std f32 无 lerp；`a + (b − a) t` 的权端点精确退化：
/// t = 0 / 1 时逐位回到 a / b——格点角点锚定性质的前提）。
fn flerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// 格点哈希 → 阵风值 ∈ [−1, 1)：整数格坐标与种子经大素数异或扩散后
/// 过 splitmix64 终混合（`particles.rs::mix64` 同式——见该处文档）。
fn corner(ix: i64, iz: i64, it: i64, seed: u64) -> f32 {
    let n = (ix as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (iz as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (it as u64).wrapping_mul(0x1656_67B1_9E37_79F9)
        ^ seed.wrapping_mul(0x27D4_EB2F_1656_67C5);
    fraction(mix64(n)) * 2.0 - 1.0
}

/// splitmix64 终混合函数（确定性伪随机原语，`iris-render::particles`
/// 同式同常数——两处必须同步修改，出处与雪崩性质见彼处文档）。
fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// 64 位随机字 → [0, 1) 均匀小数（取高 53 位，`particles.rs` 同式）。
fn fraction(u: u64) -> f32 {
    (u >> 11) as f32 * (1.0 / (1u64 << 53) as f32)
}

/// 五次平滑插值权（smootherstep，C2 连续）：u ∈ [0, 1] 端点一阶 / 二阶
/// 导数归零——阵风在格点间平滑起伏，无折角。
fn smootherstep(u: f32) -> f32 {
    let c = u.clamp(0.0, 1.0);
    c * c * c * (c * (c * 6.0 - 15.0) + 10.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 采样确定性：同 (wind, pos, t) 两次求值全部分量逐位一致
    /// （to_bits 断言——确定性铁律的形态锚点）。
    #[test]
    fn wind_sample_is_bitwise_deterministic() {
        let wind = Wind::default();
        let positions = [
            Vec3::ZERO,
            Vec3::new(13.7, -2.5, 101.3),
            Vec3::new(-45.2, 8.0, -0.4),
        ];
        for p in positions {
            for t in [0.0f32, 0.3, 12.75, 1000.5] {
                let a = wind_sample(&wind, p, t);
                let b = wind_sample(&wind, p, t);
                assert_eq!(
                    a.x.to_bits(),
                    b.x.to_bits(),
                    "x 分量逐位不一致 @ {p:?} t={t}"
                );
                assert_eq!(a.y.to_bits(), b.y.to_bits());
                assert_eq!(a.z.to_bits(), b.z.to_bits());
            }
        }
    }

    /// strength = 0 → 精确零向量（恒风 × 0 = 0，阵风调制乘零向量仍是 0，
    /// 全程无 NaN）。
    #[test]
    fn zero_strength_is_exact_zero() {
        let wind = Wind {
            strength: 0.0,
            ..Wind::default()
        };
        for t in [0.0f32, 1.0, 7.3] {
            let v = wind_sample(&wind, Vec3::new(3.3, 1.0, -9.0), t);
            assert_eq!(v, Vec3::ZERO, "strength 0 时必须为精确零向量");
            assert!(v.is_finite());
        }
    }

    /// gustiness = 0 → 纯恒风：输出与「水平单位风向 × strength」逐位
    /// 一致，且与位置 / 时间无关（格点哈希不参与）。
    #[test]
    fn zero_gustiness_is_constant_wind() {
        let dir = Vec3::new(2.0, 5.0, -1.0); // 非单位、带俯仰——水平投影后归一
        let wind = Wind {
            direction: dir,
            strength: 4.0,
            gustiness: 0.0,
            seed: 42,
        };
        let expect = Vec3::new(dir.x, 0.0, dir.z).normalize_or_zero() * 4.0;
        for p in [
            Vec3::ZERO,
            Vec3::new(100.0, -3.0, 55.0),
            Vec3::new(-7.7, 2.0, 0.1),
        ] {
            for t in [0.0f32, 3.7, 999.0] {
                let v = wind_sample(&wind, p, t);
                assert_eq!(
                    v.x.to_bits(),
                    expect.x.to_bits(),
                    "恒风 x 逐位 @ {p:?} t={t}"
                );
                assert_eq!(v.y.to_bits(), expect.y.to_bits());
                assert_eq!(v.z.to_bits(), expect.z.to_bits());
            }
        }
    }

    /// 不同 seed → 不同风场：64 个采样点 × 3 时刻的网格里至少一处
    /// 逐位不同（splitmix64 雪崩下全同概率 ≪ 2⁻⁶⁰，工程上即「必然
    /// 不同」）。
    #[test]
    fn different_seed_yields_different_field() {
        let base = Wind::default();
        let other = Wind {
            seed: base.seed ^ 0x0123_4567_89AB_CDEF,
            ..base
        };
        let mut any_diff = false;
        'outer: for i in 0..8 {
            for j in 0..8 {
                for t in [0.0f32, 0.5, 2.25] {
                    let p = Vec3::new(i as f32 * 0.7, 0.0, j as f32 * 1.3);
                    let a = wind_sample(&base, p, t);
                    let b = wind_sample(&other, p, t);
                    if a != b {
                        any_diff = true;
                        break 'outer;
                    }
                }
            }
        }
        assert!(any_diff, "不同种子必须产生不同风场");
    }

    /// 量纲有界：|输出| ≤ strength × (1 + |gustiness|)（构造上界：
    /// 单位方向 × 非负标量 × g ∈ [−1,1] 的插值闭包；1e-4 为浮点累计
    /// 舍入余量）。覆盖正负坐标 / 大坐标 / 长时间。
    #[test]
    fn output_is_bounded_by_strength_times_gust() {
        let wind = Wind {
            strength: 5.0,
            gustiness: 0.8,
            ..Wind::default()
        };
        let bound = wind.strength * (1.0 + wind.gustiness.abs()) * 1.0001;
        for i in 0..24 {
            let p = Vec3::new(
                (i as f32 - 12.0) * 17.31,
                0.0,
                (i as f32 * 13.0 - 7.0) * 3.77,
            );
            for t in [i as f32 * 0.37, 1234.5] {
                let v = wind_sample(&wind, p, t);
                assert!(v.is_finite(), "非有限值 @ {p:?} t={t}");
                assert!(
                    v.length() <= bound,
                    "风速 {} 超出构造上界 {bound} @ {p:?} t={t}",
                    v.length()
                );
                assert_eq!(v.y, 0.0, "水平风向的输出 y 必须精确为 0");
            }
        }
    }

    /// 零方向风 → 精确零输出（水平投影退化 → normalize_or_zero →
    /// 零向量 × 任何标量 = 0，不产生 NaN）。
    #[test]
    fn zero_direction_is_safe_zero() {
        let wind = Wind {
            direction: Vec3::ZERO,
            ..Wind::default()
        };
        let v = wind_sample(&wind, Vec3::new(1.0, 1.0, 1.0), 2.0);
        assert_eq!(v, Vec3::ZERO, "零方向必须安全退化为零向量");
        // 纯竖直方向同样只留水平投影 = 退化
        let wind = Wind {
            direction: Vec3::Y,
            ..Wind::default()
        };
        assert_eq!(wind_sample(&wind, Vec3::ZERO, 0.0), Vec3::ZERO);
    }

    /// 时间连续性：t 与 t + ε（不跨时间格）输出差有界且极小——
    /// 五次平滑插值生效（防「只有最近邻格点、无插值」的退化实现）。
    #[test]
    fn sampling_is_continuous_in_time() {
        let wind = Wind::default();
        let p = Vec3::new(3.13, 0.0, -7.7);
        // 取格内中点，两侧各偏 0.01 s（格宽 0.25 s，不跨格）
        let a = wind_sample(&wind, p, 1.125);
        let b = wind_sample(&wind, p, 1.135);
        let speed = wind.strength * (1.0 + wind.gustiness);
        assert!(
            (a - b).length() < 0.05 * speed,
            "微小时移的输出差 {:#?} 应近零（连续性）",
            (a - b).length()
        );
        // 跨格采样也应连续：同点在格边界两侧的值随 fade 权重平滑过渡
        let u = wind_sample(&wind, p, 0.2499);
        let v = wind_sample(&wind, p, 0.2501);
        assert!((u - v).length() < 0.05 * speed, "跨格边界不得跳变");
    }

    /// 空间连续性：位置微移（不跨格）输出近零差；格点角点处值与
    /// 哈希原始值一致（插值在角点退化为恒等——锚定哈希流本身）。
    #[test]
    fn sampling_is_continuous_in_space_and_anchors_corners() {
        let wind = Wind {
            strength: 1.0,
            gustiness: 1.0,
            ..Wind::default()
        };
        let p = Vec3::new(3.0, 0.0, -5.0); // 格（1, -3）内
        let a = wind_sample(&wind, p, 0.5);
        let b = wind_sample(&wind, p + Vec3::new(0.02, 0.0, -0.02), 0.5);
        assert!(
            (a - b).length() < 0.05 * wind.strength * 2.0,
            "微小位移的输出差应近零"
        );
        // 角点锚定：格点角（整格坐标、整时刻）上插值权重全在单角，
        // 输出 = 单位方向 × strength × (1 + gustiness × corner)
        // ——t = 2.0 s 量化到时间格 it = 2.0 / 0.25 = 8
        let t = 2.0;
        let origin = Vec3::ZERO;
        let v = wind_sample(&wind, origin, t);
        let c = corner(0, 0, 8, wind.seed);
        let expect = Vec3::X * (wind.strength * (1.0 + wind.gustiness * c));
        assert!(
            (v - expect).length() < 1e-6,
            "原点角点应锚定到格点哈希值：{v:?} vs {expect:?}"
        );
    }

    /// Default 核对：+X 3 m/s、阵风度 0.5、种子 = 黄金比率常数。
    #[test]
    fn defaults_hold() {
        let w = Wind::default();
        assert_eq!(w.direction, Vec3::X);
        assert_eq!(w.strength, 3.0);
        assert_eq!(w.gustiness, 0.5);
        assert_eq!(w.seed, 0x9E37_79B9_7F4A_7C15);
        assert_eq!(GRID_METERS, 2.0);
        assert_eq!(TIME_STEP_SECS, 0.25);
    }

    /// smootherstep：端点 0 / 1 精确、单调、对称性 u→1−u（值对称）。
    #[test]
    fn smootherstep_endpoints_and_monotonic() {
        assert_eq!(smootherstep(-0.5), 0.0, "越界下钳 0");
        assert_eq!(smootherstep(0.0), 0.0);
        assert_eq!(smootherstep(1.0), 1.0);
        assert_eq!(smootherstep(1.5), 1.0, "越界上钳 1");
        let mut prev = 0.0;
        for i in 0..=20 {
            let v = smootherstep(i as f32 / 20.0);
            assert!(v >= prev, "u={i} 处 {v} < 前值 {prev}（应单调）");
            prev = v;
        }
        // 对称：s(0.3) = 1 − s(0.7)
        let a = smootherstep(0.3);
        let b = smootherstep(0.7);
        assert!(
            (a + b - 1.0).abs() < 1e-6,
            "五次权应关于中点对称：{a} + {b}"
        );
    }
}
