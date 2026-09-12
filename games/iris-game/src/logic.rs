//! 纯游戏逻辑核心：确定性状态机，**只依赖 glam、零引擎依赖**。
//!
//! 「收集金币」的全部规则（移动 / 跳跃 / 边界 / 拾取 / 计分 / 胜负判定）
//! 都在这个模块里以纯函数推进：输入 [`MoveInput`] 与步长 `dt`，输出状态
//! 变化，不读时钟、不做随机、不触碰引擎。因此：
//! - 全部规则可毫秒级单测（本模块底部 11 项）；
//! - 插件层（`crate::plugin`）只负责把 `Input` / `Time` 翻译成
//!   [`MoveInput`] 与 `dt`，再把结果写回实体组件；
//! - headless 测试与离屏快照共享同一条确定性推进路径。
//!
//! 坐标约定（与引擎一致）：世界 XZ 平面为地面（y 向上），朝向 `facing`
//! 为绕 Y 的偏航角，`facing = 0` 时前方是 -Z（`Transform::forward` 约定）。

use glam::Vec2;

/// 移动速度（米/秒）。
pub const MOVE_SPEED: f32 = 5.0;
/// 跳跃初速度（米/秒）。重力 [`GRAVITY`] 下腾空约 0.7 s、最高约 1.225 m。
pub const JUMP_SPEED: f32 = 7.0;
/// 重力加速度（米/秒²）。
pub const GRAVITY: f32 = -20.0;
/// 地面高度（米）。
pub const GROUND_Y: f32 = 0.0;
/// 拾取半径（米，XZ 平面距离）。
pub const PICKUP_RADIUS: f32 = 0.9;
/// 拾取的高度上限（米）：玩家 y 达不到该值才可拾取（跳跃顶点约 1.225，
/// 常规移动与小幅跳跃均满足，规则上为「贴地金币」留出扩展空间）。
pub const PICKUP_MAX_Y: f32 = 1.5;

/// 转身锐度：朝向平滑的指数逼近速率（1/秒）。越大转身越快，
/// 12 ≈ 0.2 s 完成约 90% 转身——第三人称的常规手感区间。
const TURN_SHARPNESS: f32 = 12.0;

/// 游戏阶段：进行中 / 全部收集获胜 / 超时落败。
///
/// 进入 [`Phase::Won`] / [`Phase::Lost`] 后 [`GameState::step`] 冻结
/// （输入不再生效、时间不再流逝），由外部负责展示结算画面。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Playing,
    Won,
    Lost,
}

/// 玩家状态：XZ 平面位置 + 离地高度 + 垂直速度 + 朝向偏航角（弧度）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerState {
    pub x: f32,
    pub z: f32,
    pub y: f32,
    pub y_vel: f32,
    /// 朝向（绕 Y 偏航角，弧度）：最后非零移动方向；`facing()` 即前方。
    pub facing: f32,
}

impl PlayerState {
    fn new() -> Self {
        Self {
            x: 0.0,
            z: 0.0,
            y: GROUND_Y,
            y_vel: 0.0,
            facing: 0.0,
        }
    }

    /// 前方单位向量（XZ 平面）。与引擎 `Transform::forward` 同约定：
    /// `facing = 0` 指向 -Z，正值绕 Y 逆时针。
    pub fn forward(&self) -> Vec2 {
        Vec2::new(-self.facing.sin(), -self.facing.cos())
    }
}

/// 金币状态：XZ 平面位置 + 是否已被收集。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PickupState {
    pub x: f32,
    pub z: f32,
    pub collected: bool,
}

/// 一帧的移动输入：世界系期望方向（未归一化）+ 是否起跳。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MoveInput {
    /// 世界 X 方向期望（+1 = +X，-1 = -X）。
    pub x: f32,
    /// 世界 Z 方向期望（+1 = +Z，-1 = -Z；注意 -Z 是「屏幕里侧」）。
    pub z: f32,
    /// 本帧是否按下跳跃。
    pub jump: bool,
}

impl MoveInput {
    /// 无输入（静止）。
    pub const IDLE: Self = Self {
        x: 0.0,
        z: 0.0,
        jump: false,
    };

    /// 是否存在移动期望。
    pub fn is_moving(&self) -> bool {
        self.x != 0.0 || self.z != 0.0
    }
}

/// 游戏全局状态：玩家 + 金币 + 计分 + 剩余时间 + 阶段。
///
/// 字段全部公开：测试可以直接构造 / 修改中间状态（如「假定已收 9 枚」），
/// 插件层按只读约定消费（每帧由 [`GameState::step`] 独占推进）。
#[derive(Clone, Debug)]
pub struct GameState {
    pub player: PlayerState,
    pub pickups: Vec<PickupState>,
    pub score: u32,
    /// 剩余时间（秒）；进入终局后冻结。
    pub time_left: f32,
    pub phase: Phase,
    /// 竞技场半边长（米）：玩家位置被钳制在 ±arena_half 内。
    pub arena_half: f32,
}

impl GameState {
    /// 新对局：玩家在原点、金币按 [`GameState::spawn_positions`] 布点、
    /// 计时器满、阶段 Playing。
    pub fn new(arena_half: f32, duration_secs: f32) -> Self {
        Self {
            player: PlayerState::new(),
            pickups: Self::spawn_positions()
                .into_iter()
                .map(|(x, z)| PickupState {
                    x,
                    z,
                    collected: false,
                })
                .collect(),
            score: 0,
            time_left: duration_secs,
            phase: Phase::Playing,
            arena_half,
        }
    }

    /// 10 枚金币的确定性布点：环形 5 枚（半径 9，每 72° 一枚）+
    /// 散布 5 枚。坐标写死（面向 arena_half = 18 设计，全部落在 ±18 内），
    /// 保证单测、headless 集成与离屏快照看到完全相同的布局。
    pub fn spawn_positions() -> Vec<(f32, f32)> {
        const RING_RADIUS: f32 = 9.0;
        let mut positions = Vec::new();
        for i in 0..5 {
            let angle = (i as f32) * std::f32::consts::TAU / 5.0;
            positions.push((RING_RADIUS * angle.cos(), RING_RADIUS * angle.sin()));
        }
        // 散布 5 枚：避开环带与过原点的主行进线，兼顾客场四象限
        positions.push((4.5, 4.5));
        positions.push((-4.2, 3.8));
        positions.push((-3.5, -4.9));
        positions.push((5.1, -3.6));
        positions.push((0.0, 12.0));
        positions
    }

    /// 推进一帧。`dt <= 0` 或已进入终局时为无操作（冻结）。
    ///
    /// 帧内顺序：移动 → 跳跃/重力 → 边界钳制 → 拾取 → 胜利判定 →
    /// 倒计时 → 超时判定。最后一帧「捡到全部金币」优先于「同时归零」
    /// （胜利先于超时结算，对玩家更友好）。
    pub fn step(&mut self, input: &MoveInput, dt: f32) {
        if dt <= 0.0 || self.phase != Phase::Playing {
            return;
        }

        // ---- 移动：输入归一化后按速度积分；朝向向移动方向平滑转身 ----
        let wish = Vec2::new(input.x, input.z);
        let len = wish.length();
        if len > 1e-6 {
            let dir = wish / len;
            self.player.x += dir.x * MOVE_SPEED * dt;
            self.player.z += dir.y * MOVE_SPEED * dt;
            // facing 的前方是 -Z：由 forward = (-sin f, -cos f) 反解
            // （sin f = -dx、cos f = -dz → f = atan2(-dx, -dz)）
            let target = (-dir.x).atan2(-dir.y);
            // 最短角路径指数逼近：转身是渐进动画而非瞬时跳变；
            // 跨 ±π 边界（如 170° → -170°）必须走 20° 短路径而非 340° 长路
            let mut delta = target - self.player.facing;
            delta = delta.rem_euclid(std::f32::consts::TAU);
            if delta > std::f32::consts::PI {
                delta -= std::f32::consts::TAU;
            }
            let t = (TURN_SHARPNESS * dt).clamp(0.0, 1.0);
            self.player.facing += delta * t;
        }

        // ---- 跳跃与重力：仅落地状态可起跳；穿地即着陆 ----
        if input.jump && self.player.y <= GROUND_Y {
            self.player.y_vel = JUMP_SPEED;
        }
        self.player.y += self.player.y_vel * dt;
        self.player.y_vel += GRAVITY * dt;
        if self.player.y <= GROUND_Y {
            self.player.y = GROUND_Y;
            self.player.y_vel = 0.0;
        }

        // ---- 竞技场边界钳制 ----
        self.player.x = self.player.x.clamp(-self.arena_half, self.arena_half);
        self.player.z = self.player.z.clamp(-self.arena_half, self.arena_half);

        // ---- 拾取：XZ 距离达标且玩家足够贴地 ----
        if self.player.y < PICKUP_MAX_Y {
            let px = Vec2::new(self.player.x, self.player.z);
            for pickup in &mut self.pickups {
                if pickup.collected {
                    continue;
                }
                let d = Vec2::new(pickup.x, pickup.z).distance(px);
                if d <= PICKUP_RADIUS {
                    pickup.collected = true;
                    self.score += 1;
                }
            }
        }

        // ---- 胜利判定：全部收集 ----
        if self.score as usize == self.pickups.len() {
            self.phase = Phase::Won;
            return;
        }

        // ---- 倒计时与超时判定 ----
        self.time_left = (self.time_left - dt).max(0.0);
        if self.time_left <= 0.0 {
            self.phase = Phase::Lost;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARENA: f32 = 18.0;
    const DURATION: f32 = 60.0;

    /// 以固定步长推进 `seconds` 秒，期间持续按住给定输入。
    fn run(state: &mut GameState, input: MoveInput, seconds: f32, dt: f32) {
        let steps = (seconds / dt).round() as u32;
        for _ in 0..steps {
            state.step(&input, dt);
        }
    }

    #[test]
    fn spawn_layout_is_ten_distinct_in_bounds() {
        let positions = GameState::spawn_positions();
        assert_eq!(positions.len(), 10, "应为 10 枚金币");
        for (i, &(x, z)) in positions.iter().enumerate() {
            assert!(
                x.abs() <= ARENA && z.abs() <= ARENA,
                "金币 {i} ({x},{z}) 越界"
            );
            for &(ox, oz) in positions.iter().take(i) {
                assert!(
                    Vec2::new(x - ox, z - oz).length() > 2.0,
                    "金币 ({x},{z}) 与 ({ox},{oz}) 距离过近"
                );
            }
        }
    }

    #[test]
    fn move_integrates_at_five_mps_and_updates_facing() {
        let mut state = GameState::new(ARENA, DURATION);
        // +X 匀速移动 1 s → x = 5，朝向**逼近** +X（facing = atan2(0, -1) = -π/2）。
        // 转身是指数平滑动画而非瞬时赋值：60 帧（t=0.2/帧）后残差 < 1e-7，
        // 但恒不严格等于目标。
        run(
            &mut state,
            MoveInput {
                x: 1.0,
                z: 0.0,
                jump: false,
            },
            1.0,
            1.0 / 60.0,
        );
        assert!(
            (state.player.x - 5.0).abs() < 1e-4,
            "x 应为 5，实际 {}",
            state.player.x
        );
        assert!(state.player.z.abs() < 1e-5);
        let want = -std::f32::consts::FRAC_PI_2;
        assert!(
            (state.player.facing - want).abs() < 1e-4,
            "朝向应逼近 -π/2（+X），实际 {}",
            state.player.facing
        );
        // 首帧后应可见转身过程（未到位）：平滑生效的直接证据
        let mut probe = GameState::new(ARENA, DURATION);
        run(
            &mut probe,
            MoveInput {
                x: 1.0,
                z: 0.0,
                jump: false,
            },
            1.0 / 60.0,
            1.0 / 60.0,
        );
        assert!(
            (probe.player.facing - want).abs() > 1e-3,
            "单帧后朝向不应已到位（转身应是渐进的），实际 {}",
            probe.player.facing
        );
        // 前方单位向量应指向 +X
        let f = state.player.forward();
        assert!((f.x - 1.0).abs() < 1e-4 && f.y.abs() < 1e-4);
    }

    /// 朝向跨 ±π 边界必须走最短角路径：facing = +170° 时向目标 -170°
    /// （输入方向约 (0.176, 1.0)）转身——短路径为 20°（经由 180°），
    /// 无边界处理的朴素插值会走 340° 长路、中途扫过 -90°。
    #[test]
    fn facing_takes_shortest_path_across_pi_boundary() {
        let mut state = GameState::new(ARENA, DURATION);
        state.player.facing = 170.0_f32.to_radians();
        // 目标朝向 = atan2(-0.176, -1.0) ≈ -170°：与起点夹 20°（经 ±180°）
        let input = MoveInput {
            x: 0.1763,
            z: 1.0,
            jump: false,
        };
        run(&mut state, input, 0.1, 1.0 / 60.0);
        // 0.1 s（6 帧，平滑累计 ≈ 0.74）后：短路径应停在 ±180° 附近
        // （|facing| > 155°）；长路径会扫到 ≈ -81°，|facing| 远小于 90°
        let normalized = state.player.facing.rem_euclid(std::f32::consts::TAU);
        let distance_to_pi = (normalized - std::f32::consts::PI).abs();
        assert!(
            distance_to_pi < 25.0_f32.to_radians(),
            "跨 ±π 应走短路径（停在 ±180° 附近），实际 facing = {}（{}°）",
            state.player.facing,
            state.player.facing.to_degrees()
        );
        // 继续推进到 1 s：应到位并稳定逼近 -170°
        run(&mut state, input, 1.0, 1.0 / 60.0);
        let want = -170.0_f32.to_radians();
        let end_delta = (state.player.facing - want).rem_euclid(std::f32::consts::TAU);
        let end_delta = end_delta.min(std::f32::consts::TAU - end_delta);
        assert!(
            end_delta < 0.05,
            "终态应逼近 -170°，实际 {}（{}°）",
            state.player.facing,
            state.player.facing.to_degrees()
        );
    }

    #[test]
    fn diagonal_input_is_normalized() {
        let mut state = GameState::new(ARENA, DURATION);
        // 斜向输入 (1,1) 归一化后 1 s 位移仍为 5 m（不是 5√2）
        run(
            &mut state,
            MoveInput {
                x: 1.0,
                z: 1.0,
                jump: false,
            },
            1.0,
            1.0 / 60.0,
        );
        let want = 5.0 / std::f32::consts::SQRT_2;
        assert!((state.player.x - want).abs() < 1e-4);
        assert!((state.player.z - want).abs() < 1e-4);
    }

    #[test]
    fn jump_rises_then_lands() {
        let mut state = GameState::new(ARENA, DURATION);
        let dt = 1.0 / 120.0;
        // 起跳一帧后松开
        state.step(
            &MoveInput {
                x: 0.0,
                z: 0.0,
                jump: true,
            },
            dt,
        );
        assert!(state.player.y > 0.0, "起跳后应离地");
        let mut max_y = state.player.y;
        for _ in 0..120 {
            state.step(&MoveInput::IDLE, dt);
            max_y = max_y.max(state.player.y);
        }
        // 解析顶点 v²/2g = 49/40 = 1.225；显式积分的离散化误差
        // ≈ ½·|g|·t_apex·dt（dt = 1/120 时约 +0.029），放宽覆盖
        assert!(
            (max_y - 1.225).abs() < 0.05,
            "最高点应约 1.225 m，实际 {max_y}"
        );
        assert_eq!(state.player.y, GROUND_Y, "0.7 s 后应已落地");
        assert_eq!(state.player.y_vel, 0.0, "落地后垂直速度归零");
        // 落地后按住跳跃可再次起跳
        state.step(
            &MoveInput {
                x: 0.0,
                z: 0.0,
                jump: true,
            },
            dt,
        );
        assert!(state.player.y > 0.0, "落地后应可再次起跳");
    }

    #[test]
    fn boundary_clamps_to_arena() {
        let mut state = GameState::new(ARENA, DURATION);
        run(
            &mut state,
            MoveInput {
                x: 1.0,
                z: 1.0,
                jump: false,
            },
            10.0,
            1.0 / 60.0,
        );
        assert_eq!(state.player.x, ARENA, "x 应被钳制在 +arena_half");
        assert_eq!(state.player.z, ARENA, "z 应被钳制在 +arena_half");
        // 越界方向继续输入不会穿墙
        run(
            &mut state,
            MoveInput {
                x: 1.0,
                z: 1.0,
                jump: false,
            },
            5.0,
            1.0 / 60.0,
        );
        assert_eq!(state.player.x, ARENA);
    }

    #[test]
    fn pickup_within_radius_collects_and_scores() {
        let mut state = GameState::new(ARENA, DURATION);
        // 0 号金币在 (9, 0)：站到 0.9 m 半径内（8.5, 0）
        state.player.x = 8.5;
        state.step(&MoveInput::IDLE, 1.0 / 60.0);
        assert!(state.pickups[0].collected);
        assert_eq!(state.score, 1);
    }

    #[test]
    fn pickup_outside_radius_or_too_high_is_ignored() {
        let mut state = GameState::new(ARENA, DURATION);
        // 距 0 号金币 1.1 m：圈外不收集
        state.player.x = 7.9;
        state.step(&MoveInput::IDLE, 1.0 / 60.0);
        assert!(!state.pickups[0].collected);
        assert_eq!(state.score, 0);
        // 距离达标但玩家在 1.5 m 以上：高处不收集
        state.player.x = 9.0;
        state.player.y = 1.6;
        state.step(&MoveInput::IDLE, 1.0 / 60.0);
        assert!(!state.pickups[0].collected, "y >= 1.5 时不可拾取");
        assert_eq!(state.score, 0);
    }

    #[test]
    fn collecting_all_pickups_wins() {
        let mut state = GameState::new(ARENA, DURATION);
        // 直接标记 9 枚已收集（字段公开，测试可构造中间态），
        // 再把玩家传送到最后一枚（环上 (9,0)）旁收下制胜分
        for (i, pickup) in state.pickups.iter_mut().enumerate() {
            pickup.collected = i != 0;
        }
        state.score = 9;
        state.player.x = 9.0;
        state.step(&MoveInput::IDLE, 1.0 / 60.0);
        assert_eq!(state.score, 10);
        assert_eq!(state.phase, Phase::Won);
        // 胜利时刻剩余时间应保留（不是超时归零）
        assert!(state.time_left > 0.0);
    }

    #[test]
    fn timeout_loses() {
        let mut state = GameState::new(ARENA, DURATION);
        state.time_left = 0.05;
        state.step(
            &MoveInput {
                x: 1.0,
                z: 0.0,
                jump: false,
            },
            0.1,
        );
        assert_eq!(state.phase, Phase::Lost);
        assert_eq!(state.time_left, 0.0);
        assert_eq!(state.score, 0, "超时结算时未收集任何金币");
    }

    #[test]
    fn terminal_phase_freezes_state() {
        for terminal in [Phase::Won, Phase::Lost] {
            let mut state = GameState::new(ARENA, DURATION);
            state.phase = terminal;
            let before = (state.player.x, state.player.z, state.time_left, state.score);
            run(
                &mut state,
                MoveInput {
                    x: 1.0,
                    z: 0.0,
                    jump: true,
                },
                1.0,
                1.0 / 60.0,
            );
            assert_eq!(
                (state.player.x, state.player.z, state.time_left, state.score),
                before,
                "终局 {terminal:?} 后 step 应冻结一切"
            );
            assert_eq!(state.phase, terminal, "终局不应被改变");
        }
    }

    #[test]
    fn win_on_last_frame_beats_timeout() {
        // 倒计时与最后一枚金币同帧结算：胜利优先
        let mut state = GameState::new(ARENA, DURATION);
        for (i, pickup) in state.pickups.iter_mut().enumerate() {
            pickup.collected = i != 0;
        }
        state.score = 9;
        state.time_left = 0.016; // 恰好约一个 1/60 步
        state.player.x = 9.0;
        state.step(&MoveInput::IDLE, 1.0 / 60.0);
        assert_eq!(state.phase, Phase::Won, "同帧先判胜利、后判超时");
    }
}
