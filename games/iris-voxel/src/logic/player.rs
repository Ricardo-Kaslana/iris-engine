//! 第一人称玩家状态与纯数学。碰撞与重力由引擎角色控制器负责，
//! 这里只定义尺寸、朝向换算与视角积分。

use glam::Vec3;

use super::terrain;

pub const PLAYER_RADIUS: f32 = 0.3;
pub const PLAYER_HALF_HEIGHT: f32 = 0.9;
/// 脚底到眼睛的高度。
pub const EYE_HEIGHT: f32 = 1.62;
pub const WALK_SPEED: f32 = 4.3;
pub const JUMP_SPEED: f32 = 5.2;
pub const LOOK_SENSITIVITY: f32 = 0.0025;
pub const PITCH_LIMIT: f32 = 89.0_f32 * std::f32::consts::PI / 180.0;

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
        }
    }

    pub fn eye(&self) -> Vec3 {
        self.position + Vec3::Y * EYE_OFFSET
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
        };
        assert!((player.eye() - Vec3::new(1.0, 2.0 + 0.72, 3.0)).length() < 1e-6);
        assert!((EYE_OFFSET - 0.72).abs() < 1e-6);
    }
}
