//! 第一人称玩家状态与纯数学。碰撞、重力积分与贴地吸附由引擎角色控制器
//! 负责，这里定义尺寸、移动与跳跃的手感常量、朝向换算与视角积分。
//! 重力的「重量」由 [`GRAVITY_SCALE`] 交给 `CharacterParams::gravity_scale`，
//! 下落上限 [`TERMINAL_SPEED`] 由移动系统在积分前钳制。

use glam::Vec3;

use super::terrain;

pub const PLAYER_RADIUS: f32 = 0.3;
pub const PLAYER_HALF_HEIGHT: f32 = 0.9;
/// 脚底到眼睛的高度。
pub const EYE_HEIGHT: f32 = 1.62;
/// 行走速度（米/秒）：与 Minecraft 的 4.317 同量级。
pub const WALK_SPEED: f32 = 4.3;
/// 奔跑速度（米/秒）：比行走快三成，对应 Minecraft 冲刺的 5.612。
pub const SPRINT_SPEED: f32 = 5.6;
/// 重力缩放：引擎按标准重力 9.81 m/s² 积分，真实重力放进方块世界会显得
/// 飘——每秒只落不到十米。体素沙盒的手感来自「重」：2.8 倍即约
/// 27.5 m/s²，从三格高处落地约 0.47 秒（标准重力要 0.78 秒）。
pub const GRAVITY_SCALE: f32 = 2.8;
/// 跳跃初速（米/秒）。与 [`GRAVITY_SCALE`] 一起定出跳跃高度约 1.24 格、
/// 滞空约 0.58 秒：起跳 0.31 秒到顶，落回用同样时间。
pub const JUMP_SPEED: f32 = 8.5;
/// 下落速度上限（米/秒）：约 16 格后到达，防止高处坠落时单帧位移过大
/// 与落地速度失真。
pub const TERMINAL_SPEED: f32 = 30.0;
pub const LOOK_SENSITIVITY: f32 = 0.0025;
pub const PITCH_LIMIT: f32 = 89.0_f32 * std::f32::consts::PI / 180.0;

// 编译期守住手感常量之间的关系：下落上限不能压住跳跃初速，否则跑跳的
// 上升段会被当成「高速下落」钳掉。
const _: () = assert!(TERMINAL_SPEED > JUMP_SPEED * 2.0);

/// 胶囊中心偏移到眼睛的竖直距离。
pub const EYE_OFFSET: f32 = EYE_HEIGHT - PLAYER_HALF_HEIGHT;

/// 玩家运动状态。`position` 是胶囊中心，不是脚底。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerState {
    pub position: Vec3,
    pub velocity: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub grounded: bool,
    /// 是否处于奔跑状态（按住 Shift 且有前进输入）。
    pub sprinting: bool,
}

impl PlayerState {
    /// 出生在平台中心上方，面朝 -Z。
    pub fn spawn() -> Self {
        Self {
            position: terrain::spawn_position(),
            velocity: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            grounded: false,
            sprinting: false,
        }
    }

    pub fn eye(&self) -> Vec3 {
        self.position + Vec3::Y * EYE_OFFSET
    }

    /// 当前水平目标速度：奔跑比行走快三成。
    pub fn speed(&self) -> f32 {
        if self.sprinting {
            SPRINT_SPEED
        } else {
            WALK_SPEED
        }
    }
}

/// 视线方向：yaw 绕世界 Y 轴，0 朝 -Z；pitch 向上为正。
pub fn forward(yaw: f32, pitch: f32) -> Vec3 {
    let (sin_yaw, cos_yaw) = yaw.sin_cos();
    let (sin_pitch, cos_pitch) = pitch.sin_cos();
    Vec3::new(-sin_yaw * cos_pitch, sin_pitch, -cos_yaw * cos_pitch)
}

/// 水平前向（地面移动用）。
pub fn flat_forward(yaw: f32) -> Vec3 {
    let (sin_yaw, cos_yaw) = yaw.sin_cos();
    Vec3::new(-sin_yaw, 0.0, -cos_yaw)
}

/// 水平右向。
pub fn flat_right(yaw: f32) -> Vec3 {
    let (sin_yaw, cos_yaw) = yaw.sin_cos();
    Vec3::new(cos_yaw, 0.0, -sin_yaw)
}

/// 应用一帧相对鼠标位移，返回新的 `(yaw, pitch)`。右移减小 yaw，
/// 下移减小 pitch，俯仰钳制在 ±89°。
pub fn apply_look(yaw: f32, pitch: f32, delta: (f64, f64)) -> (f32, f32) {
    let yaw = yaw - delta.0 as f32 * LOOK_SENSITIVITY;
    let pitch = (pitch - delta.1 as f32 * LOOK_SENSITIVITY).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    (yaw, pitch)
}

/// 由前后与左右轴得到水平移动方向（已归一化；无输入为零）。
pub fn wish_direction(yaw: f32, forward_axis: f32, right_axis: f32) -> Vec3 {
    let dir = flat_forward(yaw) * forward_axis + flat_right(yaw) * right_axis;
    dir.normalize_or_zero()
}

/// 是否进入奔跑：按住奔跑键且有前进分量。横移与后退不算，静止也不算
/// ——与 Minecraft 一致；松开前进键立即退回行走。
pub fn wants_sprint(forward_axis: f32, sprint_held: bool) -> bool {
    sprint_held && forward_axis > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_axes() {
        let front = forward(0.0, 0.0);
        assert!((front - Vec3::new(0.0, 0.0, -1.0)).length() < 1e-6);
        let right = forward(-std::f32::consts::FRAC_PI_2, 0.0);
        assert!((right - Vec3::new(1.0, 0.0, 0.0)).length() < 1e-6);
        let up = forward(0.0, std::f32::consts::FRAC_PI_2);
        assert!((up - Vec3::Y).length() < 1e-6);
    }

    /// 右移鼠标使 yaw 减小（向右转），上移使 pitch 增大；超限被钳制。
    #[test]
    fn look_direction_and_clamp() {
        let (yaw, pitch) = apply_look(0.0, 0.0, (100.0, -50.0));
        assert!((yaw - (-100.0 * LOOK_SENSITIVITY)).abs() < 1e-6);
        assert!((pitch - (50.0 * LOOK_SENSITIVITY)).abs() < 1e-6);

        let (_, clamped) = apply_look(0.0, 0.0, (0.0, -100_000.0));
        assert_eq!(clamped, PITCH_LIMIT);
        let (_, clamped) = apply_look(0.0, 0.0, (0.0, 100_000.0));
        assert_eq!(clamped, -PITCH_LIMIT);
    }

    /// 视角换算与水平移动方向一致：朝 -Z 时 W 向 -Z、D 向 +X；
    /// 对角输入归一化。
    #[test]
    fn wish_direction_follows_yaw() {
        let forward_only = wish_direction(0.0, 1.0, 0.0);
        assert!((forward_only - Vec3::new(0.0, 0.0, -1.0)).length() < 1e-6);
        let right_only = wish_direction(0.0, 0.0, 1.0);
        assert!((right_only - Vec3::new(1.0, 0.0, 0.0)).length() < 1e-6);
        let diagonal = wish_direction(0.0, 1.0, 1.0);
        assert!((diagonal.length() - 1.0).abs() < 1e-6);
        assert_eq!(wish_direction(0.0, 0.0, 0.0), Vec3::ZERO);
    }

    /// 转向 90° 后前向与右向随之旋转。
    #[test]
    fn yaw_rotates_flat_axes() {
        let yaw = std::f32::consts::FRAC_PI_2;
        assert!((flat_forward(yaw) + Vec3::X).length() < 1e-6);
        assert!((flat_right(yaw) + Vec3::Z).length() < 1e-6);
    }

    #[test]
    fn eye_is_above_capsule_center() {
        let player = PlayerState {
            position: Vec3::new(1.0, 2.0, 3.0),
            velocity: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            grounded: true,
            sprinting: false,
        };
        assert!((player.eye() - Vec3::new(1.0, 2.0 + 0.72, 3.0)).length() < 1e-6);
        assert!((EYE_OFFSET - 0.72).abs() < 1e-6);
    }

    /// 奔跑只加在水平速度上：比行走快三成左右，其余状态量不变。
    #[test]
    fn sprint_speed_is_about_a_third_faster() {
        let mut player = PlayerState::spawn();
        assert_eq!(player.speed(), WALK_SPEED);
        player.sprinting = true;
        assert_eq!(player.speed(), SPRINT_SPEED);
        let ratio = SPRINT_SPEED / WALK_SPEED;
        assert!(
            (1.25..=1.35).contains(&ratio),
            "奔跑应比行走快约三成，实际 {ratio}"
        );
    }

    /// 奔跑要求按住键且有前进分量：后退、横移、静止都不算。
    #[test]
    fn sprint_needs_forward_input_and_key() {
        assert!(wants_sprint(1.0, true));
        assert!(!wants_sprint(1.0, false));
        assert!(!wants_sprint(0.0, true));
        assert!(!wants_sprint(-1.0, true));
        // 前进加斜向（A/D 同时按）仍是前进。
        assert!(wants_sprint(1.0, true));
    }

    /// 重力倍率定出的手感：从三格高处落地约半秒，标准重力要接近一秒。
    /// 这里只校验常量之间的关系，真实积分由引擎角色控制器负责。
    #[test]
    fn gravity_and_jump_tuning_stay_in_voxel_range() {
        let gravity = 9.81 * GRAVITY_SCALE;
        assert!(gravity > 25.0, "体素重力应明显重于标准重力：{gravity}");
        // 跳跃顶点高度 v² / 2g，目标是一格出头（能跨上一格台阶）。
        let apex = JUMP_SPEED * JUMP_SPEED / (2.0 * gravity);
        assert!(
            (1.1..=1.4).contains(&apex),
            "跳跃高度应在一格出头，实际 {apex}"
        );
        // 三格自由落体时间。
        let fall = (2.0 * 3.0 / gravity).sqrt();
        assert!((0.4..=0.55).contains(&fall), "三格下落应约半秒：{fall}");
    }
}
