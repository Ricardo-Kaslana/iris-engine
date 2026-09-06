//! 角色骨架节点表：GLB 节点下标的**单一事实来源**。
//!
//! 资产生成器（`examples/gen_asset.rs`）按下表顺序声明节点、把动画
//! 通道指向这些下标；插件层按同一组常量把
//! [`GltfInstance::node_entities`](iris_render::asset::GltfInstance::node_entities)
//! 下标解析为实体（加载器不携带节点名——按名查找不可用，下标即契约）。
//! **两处必须同步修改**；守护测试（`tests/headless.rs`）断言加载产物
//! 层级形状（节点数 / 腿链父子关系 / 剪辑数），错位会在测试期爆炸而非
//! 在画面上漂移。
//!
//! # 节点树（20 节点；Y 负向为臂腿自然下垂方向）
//!
//! ```text
//! 0  root                        模型根（插件层挂朝向旋转）
//! 1  └ pelvis     T(0, 0.95, 0)            [下半身]
//! 2  ·   └ torso  T(0, 0.28, 0)            [上半身]
//! 3  ·     └ chest T(0, 0.30, 0)           [上半身]
//! 4  ·       └ head T(0, 0.32, 0)          [上半身]
//! 5  ·   ├ l_thigh T(-0.17,-0.05,0)        [下半身]
//! 6  ·   │ └ l_shin T(0,-0.45,0)           [下半身]
//! 7  ·   │   └ l_foot T(0,-0.45,0)         [下半身]
//! 8  ·   ├ r_thigh T(0.17,-0.05,0)         [下半身]
//! 9  ·   │ └ r_shin T(0,-0.45,0)           [下半身]
//! 10 ·   │   └ r_foot T(0,-0.45,0)         [下半身]
//! 11 ·     ├ l_upperarm T(-0.34,0.22,0)    [上半身]
//! 12 ·     │ └ l_forearm T(0,-0.32,0)      [上半身]
//! 13 ·     │   └ l_hand T(0,-0.30,0)       [上半身]
//! 14 ·     ├ r_upperarm T(0.34,0.22,0)     [上半身]
//! 15 ·     │ └ r_forearm T(0,-0.32,0)      [上半身]
//! 16 ·     │   └ r_hand T(0,-0.30,0)       [上半身]
//! 17 ·     │     ├ sword（刀格挡网格）     [上半身]
//! 18 ·     │     │ └ sword_blade（刀刃）  [上半身]
//! 19 ·     └ rifle T(0.15,0.10,-0.28)（枪身）[上半身]
//! 20 ·       ├ rifle_grip T(0,-0.05,0.12)（握把，IkTarget）[上半身]
//! 21 ·       └ foregrip  T(0,-0.03,-0.18)（护木，IkTarget）[上半身]
//! ```
//!
//! 腿链长 0.45 + 0.45（thigh→shin→foot 各 0.45），静止时脚底恰在
//! y = 0（pelvis 0.95 − 0.05 − 0.45 − 0.45 = 0）——与插件层足部 IK 的
//! 「平地拉踝贴地」基线吻合。臂链长 0.32 + 0.30。
//!
//! 节点集合划分（`LOWER_BODY` ∪ `UPPER_BODY` ∪ {`ROOT`} = 全部 20 节点）：
//! 步态剪辑只驱动下身、动作剪辑只驱动上身——两层各自的
//! `AnimationPlayer` 写入的节点集不相交，组合姿势（跑动中挥砍 / 瞄准
//! 不打断腿）由这一划分保证。

/// 模型根节点（插件层在此挂朝向旋转与模型局部 y 偏移）。
pub const ROOT: usize = 0;
/// 骨盆（下半身剪辑的根：位移 bob / 摇摆挂这里）。
pub const PELVIS: usize = 1;
/// 躯干下段（上半身剪辑的根）。
pub const TORSO: usize = 2;
/// 胸（呼吸 / 瞄准俯仰 / 挥砍拧腰）。
pub const CHEST: usize = 3;
/// 头。
pub const HEAD: usize = 4;
/// 左大腿。
pub const L_THIGH: usize = 5;
/// 左小腿。
pub const L_SHIN: usize = 6;
/// 左脚。
pub const L_FOOT: usize = 7;
/// 右大腿。
pub const R_THIGH: usize = 8;
/// 右小腿。
pub const R_SHIN: usize = 9;
/// 右脚。
pub const R_FOOT: usize = 10;
/// 左上臂（肩）。
pub const L_UPPERARM: usize = 11;
/// 左前臂（肘）。
pub const L_FOREARM: usize = 12;
/// 左手。
pub const L_HAND: usize = 13;
/// 右上臂（肩）。
pub const R_UPPERARM: usize = 14;
/// 右前臂（肘）。
pub const R_FOREARM: usize = 15;
/// 右手。
pub const R_HAND: usize = 16;
/// 刀（右手的子节点：刀格挡 / 握把段网格——跟随手骨，非 IK 对象）。
pub const SWORD: usize = 17;
/// 刀刃（刀的子节点：刃段网格——写实材质通道的划痕金属挂这里）。
pub const SWORD_BLADE: usize = 18;
/// 步枪（胸的子节点：枪身网格——挂持枪位，手 IK 追它的挂点）。
pub const RIFLE: usize = 19;
/// 步枪握把（步枪的子节点；插件层在对应实体上挂
/// `IkTarget`，右手 IK 以其全局位为目标）。
pub const RIFLE_GRIP: usize = 20;
/// 步枪护木（步枪的子节点；`IkTarget`，左手 IK 的 ADS 目标）。
pub const FOREGRIP: usize = 21;

/// 节点总数（加载后 `node_entities.len()` 应恰为此值）。
pub const NODE_COUNT: usize = 22;

/// 下半身节点集合（步态剪辑只驱动这些节点；共 7 项：骨盆 + 双腿各三节）。
pub const LOWER_BODY: [usize; 7] = [PELVIS, L_THIGH, L_SHIN, L_FOOT, R_THIGH, R_SHIN, R_FOOT];

/// 上半身节点集合（动作剪辑只驱动这些节点；共 14 项：躯干三节 + 双臂各
/// 三节 + 刀 / 刀刃 / 枪身 / 握把 / 护木）。`AnimationMask` 的节点集取
/// 本表——遮罩外的节点即使出现在通道里也不覆写（与「剪辑本身只写上身」
/// 构成双重保险）。
pub const UPPER_BODY: [usize; 14] = [
    TORSO,
    CHEST,
    HEAD,
    L_UPPERARM,
    L_FOREARM,
    L_HAND,
    R_UPPERARM,
    R_FOREARM,
    R_HAND,
    SWORD,
    SWORD_BLADE,
    RIFLE,
    RIFLE_GRIP,
    FOREGRIP,
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// 节点集合划分不变量：下身 ∪ 上身 ∪ {root} 恰好覆盖 0..NODE_COUNT
    /// 且互不重叠——两层动画播放器写入集不相交的硬前提。
    #[test]
    fn body_partition_covers_all_nodes_exactly() {
        let mut seen = BTreeSet::new();
        seen.insert(ROOT);
        for node in LOWER_BODY.iter().chain(UPPER_BODY.iter()) {
            assert!(seen.insert(*node), "节点 {node} 在划分表中重复出现");
        }
        assert_eq!(seen.len(), NODE_COUNT, "划分应恰好覆盖全部节点");
        assert_eq!(*seen.last().expect("非空"), NODE_COUNT - 1);
    }

    /// 几何不变量：腿链两节各 0.45、静止脚底贴地（0.95 − 0.05 − 0.45
    /// − 0.45 = 0）——足部 IK 平地基线。
    #[test]
    fn leg_chain_reaches_ground_at_rest() {
        let pelvis_y: f32 = 0.95;
        let thigh_drop: f32 = 0.05;
        let bone: f32 = 0.45;
        assert!((pelvis_y - thigh_drop - bone - bone).abs() < 1e-6);
        // 上身挂在骨盆上：躯干 0.28 + 胸 0.30 + 头 0.32 ≈ 全高 1.85
        assert!((pelvis_y + 0.28 + 0.30 + 0.32 - 1.85).abs() < 1e-6);
    }
}
