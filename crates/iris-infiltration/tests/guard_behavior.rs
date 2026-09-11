//! R65 行为树演示能力：**逃跑**（低血量 / 被逼近 → 远离玩家的路点）与
//! **呼叫增援**（发现玩家 → Alert 事件 → 半径内守卫支援）的轨迹与状态断言。
//!
//! 为什么单开一个文件：两个能力都**默认关闭**（[`GuardCapabilities`] 全 `false`
//! = 旧行为）——既有 `tests/headless.rs` 与 `logic.rs` 单测的固定剧本（自动玩家
//! 通关、守卫巡逻环、被发现预算）在能力开启后会被改写轨迹，故「缺席 = 旧行为」
//! 是硬约束（组件缺席语义），新能力的验收只能另起剧本。本文件里的每一条断言
//! 都是**精确轨迹 / 精确状态**（不是「有变化」式弱断言）。
//!
//! 断言口径：
//! - 逃跑：最远路点由 [`GUARD_ROUTES`] 与玩家位置解析算出（见各测试注释），
//!   终态位置与距离都是解析解（±1e-3 容差 = 60 Hz 步进的浮点积累量级）；
//! - 增援：警报点 = 发现帧的玩家位置（逐位相等），支援期内的位移方向与
//!   巡逻环冻结（`route_next` 不变）都是硬断言；支援态**不进** `GuardState`
//!   枚举（黑板承载），这正是行为树相对 FSM 的扩展力。

use glam::{Vec2, Vec3};

use iris_core::behavior::Node;
use iris_infiltration::{
    BB_ALERT_POS, BB_MODE, BB_SUPPORT_TIMER, CHASE_MEMORY, GUARD_ROUTES, GuardCapabilities,
    GuardState, HALF_HEIGHT, InfiltrationEvent, InfiltrationInput, InfiltrationState, Phase,
    REINFORCE_SECS, action, guard_behavior_tree, mode,
};

/// 固定步长（与游戏主循环一致：60 Hz）。
const DT: f32 = 1.0 / 60.0;

/// 静止输入（空推一帧）。
fn rest() -> InfiltrationInput {
    InfiltrationInput {
        move_dir: Vec2::ZERO,
        sneak: false,
        interact: false,
        dt: DT,
    }
}

/// 空推 `frames` 帧。
fn idle(state: &mut InfiltrationState, frames: usize) {
    for _ in 0..frames {
        state.step(&rest());
    }
}

/// 水平（XZ）距离——本作的感知 / 玩法判定全是俯视语义。
fn horizontal_distance(a: Vec3, b: Vec3) -> f32 {
    Vec2::new(a.x - b.x, a.z - b.z).length()
}

/// 巡逻环里距 `from` 最远的路线点（解析解；严格更优 → 并列取下标小者，
/// 与 `GuardCtx::flee` 的 tie-break 同式——测试不从实现里抄结果）。
fn farthest_waypoint(route_index: usize, from: Vec3) -> Vec2 {
    let route = GUARD_ROUTES[route_index];
    let mut best = route[0];
    let mut best_distance = f32::NEG_INFINITY;
    for waypoint in route.iter() {
        let dx = waypoint.x - from.x;
        let dz = waypoint.y - from.z;
        let distance = dx * dx + dz * dz;
        if distance > best_distance {
            best_distance = distance;
            best = *waypoint;
        }
    }
    best
}

/// 守卫当前黑板模式名（行为树分支的可见证据）。
fn mode_of(state: &InfiltrationState, index: usize) -> Option<String> {
    state.guard_brains[index]
        .blackboard
        .get_text(BB_MODE)
        .map(|text| text.to_string())
}

/// 把守卫 0 钉在走廊原点面朝 -Z、玩家钉在其正前方 1.5 m（既有警觉场景）。
fn pin_corridor_scene(state: &mut InfiltrationState, player: Vec3) {
    state.guards[0].pos = Vec3::new(0.0, HALF_HEIGHT, 0.0);
    state.guards[0].facing = 0.0;
    state.pos = player;
    state.vel = Vec3::ZERO;
}

// ---------------------------------------------------------------------------
// 能力①：逃跑
// ---------------------------------------------------------------------------

/// 低血量触发：守卫 0 在巡逻环 0 号点（-8,-6.5），玩家贴在其正东 4 m 的
/// 1 号点。路线环四个点到玩家的距离分别是 4 / 0 / 3 / 5 —— **最远点是
/// (-8,-3.5)**，逃跑应当把守卫精确送到那里（终态距离 = 5.0，解析解）。
#[test]
fn flee_capability_low_health_sends_guard_to_the_farthest_waypoint() {
    let mut state = InfiltrationState::new();
    state.guards[0].capabilities.flee = true;
    state.guards[0].health = 0.1; // 低血量（≤ FLEE_HEALTH）：不依赖视线
    state.guards[0].pos = Vec3::new(-8.0, HALF_HEIGHT, -6.5);
    state.guards[0].facing = 0.0;
    state.pos = Vec3::new(-4.0, HALF_HEIGHT, -6.5);
    let before = horizontal_distance(state.guards[0].pos, state.pos);
    assert!(
        (before - 4.0).abs() < 1e-5,
        "初始距离应为 4.0，实际 {before}"
    );
    // 3 秒（3.2 m/s × 3 s = 9.6 m）足够走完 (-8,-6.5) → (-8,-3.5) 的 3 m
    idle(&mut state, 180);
    assert_eq!(mode_of(&state, 0).as_deref(), Some(mode::FLEE));
    let guard = state.guards[0].pos;
    let expected = farthest_waypoint(0, state.pos);
    assert_eq!(
        (expected.x, expected.y),
        (-8.0, -3.5),
        "剧本前提：最远路点是 (-8,-3.5)"
    );
    assert!(
        (guard.x - expected.x).abs() < 1e-3 && (guard.z - expected.y).abs() < 1e-3,
        "应精确停在最远路点 {expected:?}，实际 {guard:?}"
    );
    let after = horizontal_distance(guard, state.pos);
    assert!(
        (after - 5.0).abs() < 1e-3,
        "终态距玩家应为解析解 5.0，实际 {after}"
    );
    assert!(
        after > before + 0.5,
        "逃跑必须显著远离玩家（{before} → {after}）"
    );
    assert_eq!(
        state.guards[0].state,
        GuardState::Patrol,
        "逃跑不改 GuardState"
    );
}

/// 被逼近触发：满血守卫在巡逻环 2 号点（-4,-3.5）面朝 -Z，玩家在其正前方
/// 3 m（≤ FLEE_RADIUS = 4.5，且看得见）。最远点同样是 (-8,-3.5)（距玩家 5）。
#[test]
fn flee_capability_triggers_when_approached_and_visible() {
    let mut state = InfiltrationState::new();
    state.guards[0].capabilities.flee = true;
    state.guards[0].pos = Vec3::new(-4.0, HALF_HEIGHT, -3.5);
    state.guards[0].facing = 0.0; // 前方 -Z
    state.pos = Vec3::new(-4.0, HALF_HEIGHT, -6.5); // 正前方 3 m
    assert!(
        state.guard_sees_player(0),
        "3 m 正前方应看得见（被逼近的前提）"
    );
    let before = horizontal_distance(state.guards[0].pos, state.pos);
    idle(&mut state, 180);
    assert_eq!(mode_of(&state, 0).as_deref(), Some(mode::FLEE));
    let expected = farthest_waypoint(0, state.pos);
    assert!(
        (state.guards[0].pos.x - expected.x).abs() < 1e-3
            && (state.guards[0].pos.z - expected.y).abs() < 1e-3,
        "终态应停在最远点 {expected:?}，实际 {:?}",
        state.guards[0].pos
    );
    let after = horizontal_distance(state.guards[0].pos, state.pos);
    assert!(
        (after - 5.0).abs() < 1e-3,
        "终态距玩家 5.0（解析解），实际 {after}"
    );
    assert!(
        after > before + 1.5,
        "被逼近时应显著拉开距离（{before} → {after}）"
    );
    assert_eq!(state.detections, 0, "逃跑及时转身，不应被发现");
}

/// 分支优先级：守卫已处于 `Chase`（旧状态机里会一路追下去），但逃跑分支
/// 在树上位于追逐分支**之前**且条件成立 → 走逃跑。这是行为树相对状态机的
/// 扩展力（新增能力不必改动既有状态转移表）。
#[test]
fn flee_branch_preempts_the_chase_branch() {
    let mut state = InfiltrationState::new();
    state.guards[0].capabilities.flee = true;
    state.guards[0].state = GuardState::Chase; // 已被发现
    state.guards[0].last_known = Vec3::new(-4.0, HALF_HEIGHT, -6.5);
    state.guards[0].pos = Vec3::new(-4.0, HALF_HEIGHT, -3.5);
    state.guards[0].facing = 0.0;
    state.pos = Vec3::new(-4.0, HALF_HEIGHT, -6.5);
    let before = horizontal_distance(state.guards[0].pos, state.pos);
    idle(&mut state, 120);
    assert_eq!(
        mode_of(&state, 0).as_deref(),
        Some(mode::FLEE),
        "逃跑分支优先于追逐分支"
    );
    let after = horizontal_distance(state.guards[0].pos, state.pos);
    assert!(after > before, "应远离而不是扑向玩家（{before} → {after}）");
}
// ---------------------------------------------------------------------------
// 能力②：呼叫增援
// ---------------------------------------------------------------------------

/// 发现玩家 → `GuardAlert` 事件 → 半径内的另一名守卫进入**支援态**
/// （黑板记支援计时与警报点）：
/// - 接收判定：水平距离 ≤ REINFORCE_RADIUS = 12（本测试 4 m，宽裕）；
/// - 支援态**不进** `GuardState` 枚举——`state` 仍是 `Patrol`，行为由黑板
///   驱动（行为树相对 FSM 的扩展力：不需要新增状态与转移）；
/// - 支援期间巡逻环冻结（`route_next` 不变），朝警报点推进；
/// - 支援计时逐帧递减，到期自动回巡逻。
#[test]
fn reinforcement_alert_reaches_guard_in_radius_and_drives_support() {
    let mut state = InfiltrationState::new();
    state.guards[0].capabilities.reinforcement = true;
    state.guards[1].capabilities.reinforcement = true;
    // 发现者（守卫 0）钉在走廊原点面朝 -Z；玩家钉在正前方 1.5 m
    let alert_pos = Vec3::new(0.0, HALF_HEIGHT, -1.5);
    // 增援者（守卫 1）摆在走廊东段：距警报点 4 m（< 12），且不在发现者视线上
    state.guards[1].pos = Vec3::new(4.0, HALF_HEIGHT, 0.0);
    state.guards[1].path.clear();
    state.guards[1].path_index = 0;
    let mut detection_frame = None;
    for frame in 0..180usize {
        pin_corridor_scene(&mut state, alert_pos);
        state.step(&rest());
        if state.guards[0].state == GuardState::Chase {
            detection_frame = Some(frame);
            break;
        }
    }
    let detection_frame = detection_frame.expect("持续可见应在 3 秒内发现");
    assert!(
        detection_frame > 30,
        "警觉满格需要约 50 帧（实测 {detection_frame}）"
    );
    assert!(
        state
            .frame_events
            .iter()
            .any(|event| matches!(event, InfiltrationEvent::GuardAlert { from: 0, .. })),
        "发现帧应产生 GuardAlert 事件"
    );
    {
        let bb = &state.guard_brains[1].blackboard;
        assert_eq!(
            bb.get_f32(BB_SUPPORT_TIMER),
            Some(REINFORCE_SECS),
            "接收者应拿到完整支援时长"
        );
        assert_eq!(
            bb.get_vec3(BB_ALERT_POS),
            Some(alert_pos),
            "警报点应逐位等于发现帧的玩家位置"
        );
    }
    assert_eq!(
        state.guard_brains[0].blackboard.get_f32(BB_SUPPORT_TIMER),
        None,
        "发现者自己不入支援（它在追逐）"
    );
    // 玩家瞬移到压力门后的 NE 室（两名守卫的视线都被墙挡住）——支援轨迹干净
    state.pos = Vec3::new(10.0, HALF_HEIGHT, -7.0);
    state.vel = Vec3::ZERO;
    let before = horizontal_distance(state.guards[1].pos, alert_pos);
    let route_next_before = state.guards[1].route_next;
    idle(&mut state, 30); // 0.5 s × 3.2 m/s ≈ 1.6 m
    assert_eq!(mode_of(&state, 1).as_deref(), Some(mode::SUPPORT));
    let after = horizontal_distance(state.guards[1].pos, alert_pos);
    assert!(
        after < before - 1.0,
        "支援者应朝警报点推进（{before} → {after}）"
    );
    assert_eq!(
        state.guards[1].route_next, route_next_before,
        "支援期间巡逻环必须冻结"
    );
    assert_eq!(
        state.guards[1].state,
        GuardState::Patrol,
        "支援态不进 GuardState 枚举（黑板承载）"
    );
    // 支援计时到期 → 自动回巡逻（8 s = 480 帧，多给 60 帧余量）
    idle(&mut state, (REINFORCE_SECS * 60.0) as usize + 60);
    assert!(
        state.guard_brains[1]
            .blackboard
            .get_f32(BB_SUPPORT_TIMER)
            .is_some_and(|remaining| remaining <= 0.0)
    );
    assert_eq!(
        mode_of(&state, 1).as_deref(),
        Some(mode::PATROL),
        "支援到期应回巡逻"
    );
}

// ---------------------------------------------------------------------------
// 缺席语义：能力默认关 = 旧行为
// ---------------------------------------------------------------------------

/// 同一发现剧本在**能力全关**（默认构造）下的表现：无 `GuardAlert`、无支援
/// 黑板键、无挂起警报，且行为树只走 `patrol` / `chase` 两条既有分支——
/// 「能力缺席 = 旧行为」的正面证据（既有 headless / 单测的固定剧本据此不变）。
#[test]
fn capabilities_absent_reproduce_the_legacy_behaviour() {
    let mut state = InfiltrationState::new();
    assert_eq!(state.guards[0].capabilities, GuardCapabilities::default());
    assert_eq!(state.guards[1].capabilities, GuardCapabilities::default());
    let mut modes: Vec<String> = Vec::new();
    let mut detection_frame = None;
    for frame in 0..180usize {
        pin_corridor_scene(&mut state, Vec3::new(0.0, HALF_HEIGHT, -1.5));
        state.step(&rest());
        if let Some(mode) = mode_of(&state, 0) {
            if !modes.contains(&mode) {
                modes.push(mode);
            }
        }
        if state.guards[0].state == GuardState::Chase {
            detection_frame = Some(frame);
            break; // 首次发现即停：否则警觉重新积累会连续发现三次并终局冻结
        }
    }
    assert!(
        detection_frame.is_some(),
        "剧本应真的产生一次发现（否则证据为空转）"
    );
    assert_eq!(state.detections, 1, "停帧时应恰好发现一次");
    assert!(state.pending_alerts.is_empty(), "能力关：不应有挂起警报");
    assert!(
        state
            .guard_brains
            .iter()
            .all(|brain| brain.blackboard.get_f32(BB_SUPPORT_TIMER).is_none())
    );
    assert!(
        !state
            .frame_events
            .iter()
            .any(|event| matches!(event, InfiltrationEvent::GuardAlert { .. })),
        "能力关：不应产生 GuardAlert"
    );
    modes.sort();
    assert_eq!(modes, ["chase", "patrol"], "能力关：只应出现既有两条分支");
    // 玩家躲进压力门后的 NE 室：追逐记忆（5 s）耗尽 → 回巡逻（旧行为）
    state.pos = Vec3::new(10.0, HALF_HEIGHT, -7.0);
    state.vel = Vec3::ZERO;
    idle(&mut state, (CHASE_MEMORY * 60.0) as usize + 60);
    assert_eq!(
        state.guards[0].state,
        GuardState::Patrol,
        "记忆耗尽应回巡逻"
    );
    assert_eq!(state.phase, Phase::Playing, "本剧本不应终局");
}
// ---------------------------------------------------------------------------
// 确定性（能力开启后仍逐位可复现）
// ---------------------------------------------------------------------------

/// 同输入两遍：逐帧 dump（守卫位置 bits / 警觉 / 状态 / 黑板模式 / 支援计时
/// bits）必须逐位一致——行为树 + 黑板不引入任何不确定源（无哈希容器参与
/// 决策、无时钟读取、无随机）。
#[test]
fn capability_scenarios_are_bit_identical_across_runs() {
    fn trace() -> Vec<String> {
        let mut state = InfiltrationState::new();
        for guard in state.guards.iter_mut() {
            guard.capabilities = GuardCapabilities {
                flee: true,
                reinforcement: true,
            };
        }
        let mut dump = Vec::new();
        for frame in 0..600usize {
            if frame < 120 {
                // 走廊场景：守卫 0 发现玩家（触发增援警报）
                pin_corridor_scene(&mut state, Vec3::new(0.0, HALF_HEIGHT, -1.5));
            } else if frame == 120 {
                // 之后玩家躲进压力门后的 NE 室：支援窗口不被新的发现打断
                state.pos = Vec3::new(10.0, HALF_HEIGHT, -7.0);
                state.vel = Vec3::ZERO;
            }
            state.step(&rest());
            let mut line = format!("f{frame}");
            for (index, guard) in state.guards.iter().enumerate() {
                line.push_str(&format!(
                    " g{index}=[{},{},{}] alert={} state={:?} mode={:?} support={:?}",
                    guard.pos.x.to_bits(),
                    guard.pos.y.to_bits(),
                    guard.pos.z.to_bits(),
                    guard.alert.to_bits(),
                    guard.state,
                    mode_of(&state, index),
                    state.guard_brains[index]
                        .blackboard
                        .get_f32(BB_SUPPORT_TIMER)
                        .map(f32::to_bits),
                ));
            }
            dump.push(line);
        }
        dump
    }
    let a = trace();
    let b = trace();
    assert_eq!(a.len(), 600);
    assert_eq!(a, b, "能力开启后的剧本也应逐位一致");
    assert!(
        a.iter().any(|line| line.contains(mode::SUPPORT)),
        "样本里应真的出现过支援态（否则确定性断言是空转）"
    );
}

// ---------------------------------------------------------------------------
// 树结构 = 文档（结构性改动必须显式改这里）
// ---------------------------------------------------------------------------

/// 守卫行为树的前序展开结构：根 Selector 两条分支（击晕冻结 / 感知+决策），
/// 决策 Selector 四条分支按「逃跑 → 追逐 → 支援 → 巡逻」优先级排列。
/// 这是在**结构层**钉住优先级语义——比只测行为更难被无意改坏。
#[test]
fn guard_tree_structure_matches_the_documented_priority() {
    let tree = guard_behavior_tree();
    assert_eq!(
        tree.len(),
        19,
        "前序展开的节点总数（结构变更请同步文档与本断言）"
    );
    let Some(Node::Selector { children }) = tree.node(tree.root()) else {
        panic!("根节点应是 Selector（优先级选择）");
    };
    assert_eq!(children, &[1, 4], "两条顶层分支：击晕冻结 / 感知与决策");
    assert!(matches!(tree.node(1), Some(Node::Sequence { children }) if children == &[2, 3]));
    assert!(matches!(tree.node(2), Some(Node::Condition(id)) if id.as_str() == action::STUNNED));
    assert!(matches!(tree.node(3), Some(Node::Action(id)) if id.as_str() == action::FREEZE));
    assert!(matches!(tree.node(4), Some(Node::Sequence { children }) if children == &[5, 6]));
    assert!(matches!(tree.node(5), Some(Node::Action(id)) if id.as_str() == action::SENSE));
    let Some(Node::Selector { children }) = tree.node(6) else {
        panic!("决策节点应是 Selector");
    };
    assert_eq!(
        children,
        &[7, 10, 13, 16],
        "决策优先级：逃跑 → 追逐 → 支援 → 巡逻"
    );
    for (sequence, condition, leaf, condition_id, action_id) in [
        (7usize, 8usize, 9usize, action::THREATENED, action::FLEE),
        (10, 11, 12, action::CHASING, action::CHASE),
        (13, 14, 15, action::SUPPORTING, action::SUPPORT),
        (16, 17, 18, action::PATROLLING, action::PATROL),
    ] {
        assert!(
            matches!(tree.node(sequence), Some(Node::Sequence { children }) if children == &[condition, leaf])
        );
        assert!(
            matches!(tree.node(condition), Some(Node::Condition(id)) if id.as_str() == condition_id)
        );
        assert!(matches!(tree.node(leaf), Some(Node::Action(id)) if id.as_str() == action_id));
    }
}
