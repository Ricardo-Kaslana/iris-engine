//! 透视相机：资源形态（全局唯一）与实体挂载形态（可跟随实体运动）。

use glam::{Mat4, Vec3};
use iris_core::Resource;
use iris_core::prelude::GlobalTransform;

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

/// 挂载在实体上的活动相机：位置与朝向来自该实体的 `GlobalTransform`
/// （局部 **-Z 为前方**，+Y 为上方），跟随父级运动（第三人称 / 载具相机）。
///
/// 世界中存在 `ActiveCamera` 实体时，渲染器优先使用它；否则回退到
/// `Camera` 资源。多个实体同时挂载时取查询序中的第一个。
#[derive(Clone, Copy, Debug, iris_core::Component)]
pub struct ActiveCamera {
    pub fov_y_radians: f32,
    pub z_near: f32,
    pub z_far: f32,
}

impl Default for ActiveCamera {
    fn default() -> Self {
        Self {
            fov_y_radians: std::f32::consts::FRAC_PI_4,
            z_near: 0.1,
            z_far: 1000.0,
        }
    }
}

/// 由实体的全局变换构造视图矩阵（-Z 前方、+Y 上方）。
pub fn view_from_global(transform: &GlobalTransform) -> Mat4 {
    let matrix = transform.matrix();
    let eye = transform.position();
    let forward = -matrix.z_axis.truncate().normalize_or_zero();
    let up = matrix.y_axis.truncate().normalize_or_zero();
    glam::camera::rh::view::look_at_mat4(eye, eye + forward, up)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core::prelude::Transform;

    #[test]
    fn view_proj_maps_target_to_center() {
        let camera = Camera::default();
        let vp = camera.view_proj(16.0 / 9.0);
        let clip = vp * camera.target.extend(1.0);
        assert!(
            clip.x.abs() < 1e-5 && clip.y.abs() < 1e-5,
            "注视点应投影到屏幕中心"
        );
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

    #[test]
    fn mounted_camera_at_origin_looks_down_minus_z() {
        let gt = GlobalTransform::from_transform(Transform::from_xyz(0.0, 0.0, 5.0));
        let view = view_from_global(&gt);
        // 前方 1 米处的点应位于视图正前方（-Z）
        let ahead = view * Vec3::new(0.0, 0.0, 4.0).extend(1.0);
        assert!((ahead.x).abs() < 1e-5 && (ahead.y).abs() < 1e-5);
        assert!(
            ahead.z < 0.0 && ahead.w > 0.0,
            "相机前方点应在视图 -Z 半空间"
        );
    }

    #[test]
    fn mounted_camera_rotation_carries_view() {
        // 相机绕 Y 旋转 180° 后，原本在 +Z 的点来到正前方
        let gt = GlobalTransform::from_transform(Transform {
            position: Vec3::new(0.0, 0.0, -5.0),
            rotation: glam::Quat::from_rotation_y(std::f32::consts::PI),
            scale: Vec3::ONE,
        });
        let view = view_from_global(&gt);
        let ahead = view * Vec3::new(0.0, 0.0, -4.0).extend(1.0);
        assert!(ahead.x.abs() < 1e-5 && ahead.y.abs() < 1e-5);
        assert!(ahead.z < 0.0 && ahead.w > 0.0);
    }

    #[test]
    fn mounted_camera_follows_parent() {
        // 父级平移 10 单位后，挂载相机的 eye 应跟随
        use iris_core::hierarchy::propagate_transforms;
        use iris_core::prelude::World;
        let mut world = World::new();
        let rig = world.spawn((Transform::from_xyz(10.0, 0.0, 0.0), ActiveCamera::default()));
        let cam = world.spawn((Transform::from_xyz(0.0, 2.0, 3.0), ActiveCamera::default()));
        world.add_child(rig, cam);
        propagate_transforms(&mut world);

        let gt = *world.get::<GlobalTransform>(cam).unwrap();
        let view = view_from_global(&gt);
        let eye = gt.position();
        assert_eq!(eye, Vec3::new(10.0, 2.0, 3.0), "相机应继承父级平移");
        // 相机无旋转朝 -Z：正前 / 正后方的点分别投影到视图 -Z / +Z
        let ahead = view * Vec3::new(10.0, 2.0, 0.0).extend(1.0);
        let behind = view * Vec3::new(10.0, 2.0, 6.0).extend(1.0);
        assert!(
            ahead.z < 0.0 && ahead.w > 0.0,
            "正前方的点应在视图 -Z 半空间"
        );
        assert!(behind.z > 0.0, "正后方的点应在视图 +Z 半空间（被裁剪侧）");
    }
}
