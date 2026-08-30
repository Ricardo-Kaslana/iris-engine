//! 透视相机。

use glam::{Mat4, Vec3};
use iris_core::Resource;

#[derive(Clone, Debug, Resource)]
pub struct Camera {
    /// 相机世界位置。
    pub eye: Vec3,
    /// 注视点。
    pub target: Vec3,
    /// 上方向。
    pub up: Vec3,
    /// 垂直视场角（弧度）。
    pub fov_y_radians: f32,
    pub z_near: f32,
    pub z_far: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            eye: Vec3::new(0.0, 1.8, 4.5),
            target: Vec3::ZERO,
            up: Vec3::Y,
            fov_y_radians: std::f32::consts::FRAC_PI_4,
            z_near: 0.1,
            z_far: 1000.0,
        }
    }
}

impl Camera {
    pub fn view_matrix(&self) -> Mat4 {
        glam::camera::rh::view::look_at_mat4(self.eye, self.target, self.up)
    }

    pub fn proj_matrix(&self, aspect: f32) -> Mat4 {
        // DirectX / Vulkan 深度约定：z ∈ [0, 1]，与 wgpu 一致
        glam::camera::rh::proj::directx::perspective(
            self.fov_y_radians,
            aspect,
            self.z_near,
            self.z_far,
        )
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        self.proj_matrix(aspect) * self.view_matrix()
    }

    /// 平移相机（eye 与 target 同步移动，保持朝向）。
    pub fn fly(&mut self, delta: Vec3) {
        self.eye += delta;
        self.target += delta;
    }

    /// 调整视场角（滚轮变焦），限制在 1°..170°。
    pub fn zoom(&mut self, fov_delta_radians: f32) {
        const MIN: f32 = 1.0f32.to_radians();
        const MAX: f32 = 170.0f32.to_radians();
        self.fov_y_radians = (self.fov_y_radians + fov_delta_radians).clamp(MIN, MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_proj_maps_target_to_center() {
        let camera = Camera::default();
        let vp = camera.view_proj(16.0 / 9.0);
        let clip = vp * camera.target.extend(1.0);
        assert!(clip.x.abs() < 1e-5 && clip.y.abs() < 1e-5, "注视点应投影到屏幕中心");
        assert!(clip.w > 0.0, "注视点应在相机前方");
    }

    #[test]
    fn zoom_is_clamped() {
        let mut camera = Camera::default();
        camera.zoom(-100.0);
        assert!(camera.fov_y_radians >= 1.0f32.to_radians() - 1e-6);
        camera.zoom(1000.0);
        assert!(camera.fov_y_radians <= 170.0f32.to_radians() + 1e-6);
    }
}
