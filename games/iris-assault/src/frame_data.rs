//! 战斗帧数据（R67）：刀剑三连段与敌人挥砍的**时间权威**——判定帧 /
//! 取消窗口 / 表现曲线的单一事实来源。
//!
//! # 为什么表是权威（而不是逻辑常量或剪辑关键帧）
//!
//! R67 之前同一份时序存在三处手工复刻：`logic.rs` 的相位常量（模拟）、
//! `gen_assault_assets.rs` 的剪辑关键帧时间（表现，如 sword1 的
//! `[0, 0.18, 0.25, 0.32, 0.54]` 恰为 0.18 前摇 + 0.14 横扫 + 0.22 后摇）、
//! 插件层的「刚起手」启发式（`t <= 1.5/60`）。三处各自漂移的隐患随
//! 任何一处改动成立。R67 起本表是唯一手写处：
//!
//! 1. **模拟侧**：`logic.rs` 的 `SWORD_WINDUP` 等常量**从本表派生**
//!    （const 索引表达式，零运行期成本）——相位推进、取消窗口判定、
//!    敌人硬直全部读表；
//! 2. **表现侧**：插件层装配时把本表**生成** R74 帧数据（窗口
//!    `ClipWindow` / 事件 `AnimationEvent` / 曲线 `ParameterTrack`）
//!    注入上身动作播放器的 sword1..3 剪辑（`clips_mut()`，setup 期
//!    一次性）——剪辑窗口与模拟相位因此**构造性一致**；
//! 3. 资产生成器的关键帧时间仍手工维护（GLB 不携带 R74 帧数据，
//!    重生成属离线流程），但其值本就取自同一批数字；偏差只影响
//!    动画观感、不影响判定（判定权威在表）。
//!
//! # 取消窗口语义
//!
//! `cancel: Option<(f32, f32)>` = 以**动作起点**为参照的半开区间
//! `[start, end)`：处于该动作时按下攻击键（边沿）**立即接续下一段**
//! （不等后摇走完）。横扫判定窗内的按键仍是旧「预约」语义（记
//! `queued`，后摇结束接续）——预约在先、取消在后，两种接续并存：
//! - 第 1/2 段：cancel = `[前摇+横扫, 动作总长)`——判定窗一关整个
//!   后摇期都可立即取消（最响应的 v1 取法）；
//! - 第 3 段：`None`——三连段终结，后摇不可取消（收刀归鞘）。
//!
//! # 确定性
//!
//! 纯 `const` 数据 + `const fn` 查询，无时钟无随机；模拟读表与表现
//! 注入同源，headless 测试与离屏快照天然逐位一致。

/// 一段刀剑动作的帧数据（玩家三连段的每一段）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SwordStageData {
    /// 前摇（秒）：起手到判定窗开启。起手时自动锁敌面向。
    pub windup: f32,
    /// 横扫判定窗（秒）：窗内每帧对扇形区域判定，同代次每敌人至多
    /// 命中一次。
    pub sweep: f32,
    /// 后摇（秒）：判定窗关闭到动作结束（或被取消 / 预约接续）。
    pub recover: f32,
    /// 取消窗口（以动作起点为参照的秒区间，半开；`None` = 不可取消，
    /// 见模块文档「取消窗口语义」）。
    pub cancel: Option<(f32, f32)>,
}

impl SwordStageData {
    /// 动作总长（秒）＝ 三相位之和。资产生成器的剪辑时长与插件层注入
    /// 的 recover 窗口末端都取此值。
    pub const fn total(&self) -> f32 {
        self.windup + self.sweep + self.recover
    }

    /// 判定窗开启时刻（= 前摇长，动作起点参照）。
    pub const fn sweep_start(&self) -> f32 {
        self.windup
    }

    /// 判定窗关闭时刻（= 前摇 + 横扫，动作起点参照；也是取消窗口起点）。
    pub const fn recover_start(&self) -> f32 {
        self.windup + self.sweep
    }

    /// 动作时刻 `t`（动作起点参照）是否落在取消窗口内。`None` 恒 false；
    /// 区间语义与引擎 [`iris_render::animation::window_active`] 一致
    /// （半开、start 含 end 不含）。
    pub const fn cancel_active(&self, t: f32) -> bool {
        match self.cancel {
            None => false,
            Some((start, end)) => t >= start && t < end,
        }
    }
}

/// 玩家刀剑三连段帧数据（值 = R67 之前 `SWORD_WINDUP` / `SWORD_SWEEP` /
/// `SWORD_RECOVER` 三组常量的原值——迁移零位移，既有测试与手感不变）。
/// 取消窗口写成与 [`SwordStageData::recover_start`] / [`total`](SwordStageData::total)
/// 相同的 const 算术（非预约合数字面量）——f32 舍入下与派生式**位级
/// 一致**（0.18+0.14+0.22 的 f32 和 ≠ 手写 0.54，见 R67 实测教训）。
pub const PLAYER_SWORD: [SwordStageData; 3] = [
    // 第 1 段：右→左横扫；取消窗 [0.32, 0.54)
    SwordStageData {
        windup: 0.18,
        sweep: 0.14,
        recover: 0.22,
        cancel: Some((0.18 + 0.14, 0.18 + 0.14 + 0.22)),
    },
    // 第 2 段：左→右回扫；取消窗 [0.30, 0.54)
    SwordStageData {
        windup: 0.16,
        sweep: 0.14,
        recover: 0.24,
        cancel: Some((0.16 + 0.14, 0.16 + 0.14 + 0.24)),
    },
    // 第 3 段：重劈（命中附带击退）；终结段无取消窗口
    SwordStageData {
        windup: 0.22,
        sweep: 0.16,
        recover: 0.42,
        cancel: None,
    },
];

/// 敌人挥砍帧数据（值 = R67 之前 `ENEMY_WINDUP` / `ENEMY_STRIKE_WINDOW` /
/// `ENEMY_RECOVER` / `ENEMY_STAGGER` 常量原值）。敌人是逻辑驱动的状态机
/// （其动作剪辑 0.54 s 短于 1.22 s 的动作总长，表现侧本来就滞后收刀
/// ——见模块文档第 3 点），故敌人侧**只取模拟时序**，不注入剪辑窗口。
pub const ENEMY_STRIKE: SwordStageData = SwordStageData {
    windup: 0.5,
    sweep: 0.12,
    recover: 0.6,
    cancel: None,
};
/// 敌人被击硬直（秒；重置型——硬直中再被击重置为此值，不叠加）。
pub const ENEMY_STAGGER: f32 = 0.35;

/// 表现曲线 `dmg_scale`（插件层注入 sword 剪辑的 R74 参数曲线）：横扫
/// 判定窗上的「甜区」形状——起刀 1.0 → 窗中点 1.5 → 收刀 1.0。**纯
/// 表现用途**（刀光粒子 burst 数量缩放），不进模拟（伤害仍是整型常量
/// [`crate::logic::SWORD_DAMAGE`]）。
pub const DMG_SCALE_CURVE: &[(f32, f32)] = &[(0.0, 1.0), (0.5, 1.5), (1.0, 1.0)];

#[cfg(test)]
mod tests {
    use super::*;

    /// 表 ↔ 派生常量一致（单一事实源的双向锁定：改表不改常量派生、
    /// 或手改常量绕过表，都会在此炸）。
    #[test]
    fn tables_match_derived_constants() {
        for (i, stage) in PLAYER_SWORD.iter().enumerate() {
            assert_eq!(
                stage.windup,
                crate::logic::SWORD_WINDUP[i],
                "SWORD_WINDUP[{i}] 应从表派生"
            );
            assert_eq!(
                stage.sweep,
                crate::logic::SWORD_SWEEP[i],
                "SWORD_SWEEP[{i}] 应从表派生"
            );
            assert_eq!(
                stage.recover,
                crate::logic::SWORD_RECOVER[i],
                "SWORD_RECOVER[{i}] 应从表派生"
            );
        }
        assert_eq!(ENEMY_STRIKE.windup, crate::logic::ENEMY_WINDUP);
        assert_eq!(ENEMY_STRIKE.sweep, crate::logic::ENEMY_STRIKE_WINDOW);
        assert_eq!(ENEMY_STRIKE.recover, crate::logic::ENEMY_RECOVER);
        assert_eq!(ENEMY_STAGGER, crate::logic::ENEMY_STAGGER);
    }

    /// 取消窗口边界：第 1/2 段 = [前摇+横扫, 总长)，第 3 段 None；
    /// 半开语义（start 含、end 不含）。
    #[test]
    fn cancel_windows_are_half_open_from_sweep_end() {
        for (i, stage) in PLAYER_SWORD.iter().enumerate() {
            assert_eq!(
                stage.cancel,
                if i < 2 {
                    Some((stage.recover_start(), stage.total()))
                } else {
                    None
                },
                "第 {} 段取消窗口应恰为判定窗关闭到动作末尾",
                i + 1
            );
        }
        let s1 = &PLAYER_SWORD[0];
        assert!(!s1.cancel_active(s1.sweep_start()), "判定窗内不是取消窗");
        assert!(
            !s1.cancel_active(s1.recover_start() - 1e-6),
            "判定窗关闭前不是取消窗"
        );
        assert!(s1.cancel_active(s1.recover_start()), "start 含");
        assert!(s1.cancel_active(s1.total() - 1e-6), "末尾前仍在窗内");
        assert!(!s1.cancel_active(s1.total()), "end 不含（动作已结束）");
        assert!(!PLAYER_SWORD[2].cancel_active(0.5), "第 3 段不可取消");
    }

    /// 总长与资产生成器注释的剪辑时长一致（sword1/2 = 0.54 s、
    /// sword3 = 0.80 s——表现关键帧与帧数据同源的最后防线）。
    #[test]
    fn totals_match_clip_durations() {
        assert!((PLAYER_SWORD[0].total() - 0.54).abs() < 1e-6);
        assert!((PLAYER_SWORD[1].total() - 0.54).abs() < 1e-6);
        assert!((PLAYER_SWORD[2].total() - 0.80).abs() < 1e-6);
    }
}
