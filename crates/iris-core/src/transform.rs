//! 变换组件：位置 + 旋转 + 缩放。

use glam::{Mat4, Quat, Vec3};

use crate::Component;

/// 实体的空间变换。矩阵按需惰性计算（`matrix()`），无每帧维护成本。
#[derive(Debug, Clone, Copy, PartialEq, Component)]
pub struct Transform {
    pub position: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Transform {
    pub const IDENTITY: Self = Self {
        position: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    };

    pub fn from_xyz(x: f32, y: f32, z: f32) -> Self {
        Self {
            position: Vec3::new(x, y, z),
            ..Self::IDENTITY
        }
    }

    pub fn from_translation(translation: Vec3) -> Self {
        Self {
            position: translation,
            ..Self::IDENTITY
        }
    }

    pub fn from_rotation_y(angle: f32) -> Self {
        Self {
            rotation: Quat::from_rotation_y(angle),
            ..Self::IDENTITY
        }
    }

    /// 模型矩阵（缩放 * 旋转 * 平移）。
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.position)
    }

    pub fn translate(&mut self, delta: Vec3) {
        self.position += delta;
    }

    /// 绕世界 Y 轴旋转。
    pub fn rotate_y(&mut self, angle: f32) {
        self.rotation = Quat::from_rotation_y(angle) * self.rotation;
    }

    /// 局部 +Z 的反方向（右手系约定下的「前方」）。
    pub fn forward(&self) -> Vec3 {
        self.rotation * -Vec3::Z
    }

    pub fn right(&self) -> Vec3 {
        self.rotation * Vec3::X
    }

    pub fn up(&self) -> Vec3 {
        self.rotation * Vec3::Y
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec4;

    #[test]
    fn identity_matrix() {
        assert_eq!(Transform::IDENTITY.matrix(), Mat4::IDENTITY);
    }

    #[test]
    fn translation_only_matrix() {
        let t = Transform::from_xyz(1.0, 2.0, 3.0);
        let m = t.matrix();
        assert_eq!(m.w_axis, Vec4::new(1.0, 2.0, 3.0, 1.0), "平移应落在第 4 列");
    }

    #[test]
    fn rotation_moves_forward() {
        // 绕 Y 轴 +90°（右手系）：-Z 前方转向 -X
        let mut t = Transform::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let f1 = t.forward();
        assert!((f1.x + 1.0).abs() < 1e-5 && f1.y.abs() < 1e-5 && f1.z.abs() < 1e-5);
        // 再转 +90°：指向 +Z，且与 f1 正交
        t.rotate_y(std::f32::consts::FRAC_PI_2);
        let f2 = t.forward();
        assert!((f2.z - 1.0).abs() < 1e-5 && f2.y.abs() < 1e-5 && f2.x.abs() < 1e-5);
        assert!(f1.dot(f2).abs() < 1e-5);
    }

    #[test]
    fn scale_applies_to_matrix() {
        let t = Transform {
            scale: Vec3::splat(2.0),
            ..Transform::from_xyz(0.0, 0.0, 0.0)
        };
        let corner = t.matrix() * Vec4::new(1.0, 0.0, 0.0, 1.0);
        assert_eq!(corner, Vec4::new(2.0, 0.0, 0.0, 1.0));
    }
}
