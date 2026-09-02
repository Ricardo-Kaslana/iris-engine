//! 透视相机：资源形态（全局唯一）与实体挂载形态（可跟随实体运动）。
//!
//! 屏幕反投影（打磨 III）：[`Camera::screen_ray`] /
//! [`screen_ray_from_global`] 把屏幕 NDC 点映射回世界射线（拾取的基础
//! 操作），配套的 [`view_proj_from_global`] 公开实体相机的 view-proj
//! 拼装（世界 → 屏幕的正向投影同样不必手工拼矩阵）。视口宽高比由调用
//! 方给出——窗口程序的实时尺寸见 `iris_engine` 的 `Viewport` 资源
//! （`DefaultPlugins` 插入、`Resized` 事件自动同步）。

use glam::{Mat4, Vec2, Vec3, Vec4};
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

    /// 屏幕射线（资源相机版）：NDC 点 → 世界射线 `(origin, dir)`——
    /// 原点 = eye、方向经 view-proj 逆映射（数学推导见 [`ray_from_vp`]）。
    /// NDC 约定：x / y ∈ [-1, 1]、y 向上、屏幕中心 (0, 0)；DirectX 深度
    /// z ∈ [0, 1] 与渲染器一致。屏幕像素 → NDC 的换算在调用方
    /// （`ndc.x = px / 宽 × 2 − 1`、`ndc.y = 1 − py / 高 × 2` 的 y 翻转），
    /// 实时视口尺寸见 `iris_engine` 的 `Viewport` 资源。像素在视口外
    /// 同样成立（射线照算，合法性由调用方裁决）。
    pub fn screen_ray(&self, ndc: Vec2, aspect: f32) -> (Vec3, Vec3) {
        ray_from_vp(self.view_proj(aspect), self.eye, ndc)
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

/// 实体挂载相机的 view-proj 矩阵（DirectX 深度约定 z ∈ [0, 1]，与渲染器
/// 同款）：视图取 [`view_from_global`]（上一帧层级传播结果，静止相机无
/// 滞后），投影取 `ActiveCamera` 参数，宽高比由调用方给出（窗口程序取
/// `iris_engine` 的 `Viewport` 资源宽 / 高）。世界 → 屏幕的正向投影与
/// [`screen_ray_from_global`] 的反投影共用本矩阵——引擎外不再有手工拼
/// vp 的必要。
pub fn view_proj_from_global(transform: &GlobalTransform, cam: &ActiveCamera, aspect: f32) -> Mat4 {
    let view = view_from_global(transform);
    let proj = glam::camera::rh::proj::directx::perspective(
        cam.fov_y_radians,
        aspect,
        cam.z_near,
        cam.z_far,
    );
    proj * view
}

/// 实体挂载相机的屏幕射线（[`Camera::screen_ray`] 的实体版）：原点 =
/// 相机实体全局位置，视图复用 [`view_from_global`]。多个 `ActiveCamera`
/// 实体并存时由调用方决定取哪一个（渲染器取查询序第一个，拾取宜同源）。
pub fn screen_ray_from_global(
    transform: &GlobalTransform,
    cam: &ActiveCamera,
    ndc: Vec2,
    aspect: f32,
) -> (Vec3, Vec3) {
    ray_from_vp(
        view_proj_from_global(transform, cam, aspect),
        transform.position(),
        ndc,
    )
}

/// NDC 点 → 世界射线的公共数学：视锥内 NDC 点 (x, y, z, 1) 经 vp 逆映射
/// 回世界齐次点 p = inv(vp)·(x, y, z, 1)，透视除法（p / p.w）后得世界
/// 点。对近 / 远平面（DirectX 深度 NDC z = 0 / 1）各取一点，两点连线即
/// 过眼点的射线（针孔模型）——原点取眼点、方向取「远点 − 近点」归一
/// （与「远点 − 眼点」共线，同一几何射线；近远两点同在视锥内，数值上
/// 比跨 z_far ≈ 1000 m 的远-眼差分更平衡）。w 分量趋零（透视除法奇异，
/// 仅退化相机可达）时该点退回原点、方向 `normalize_or_zero` 归零——
/// 零方向由调用方按「射线无效」兜底。
fn ray_from_vp(vp: Mat4, eye: Vec3, ndc: Vec2) -> (Vec3, Vec3) {
    let inv = vp.inverse();
    let unproject = |z: f32| {
        let p = inv * Vec4::new(ndc.x, ndc.y, z, 1.0);
        if p.w.abs() < 1e-9 {
            Vec3::ZERO
        } else {
            p.truncate() / p.w
        }
    };
    let near = unproject(0.0);
    let far = unproject(1.0);
    (eye, (far - near).normalize_or_zero())
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

    /// 屏幕射线（打磨 III）：NDC 中心射线 ≈ eye→target 方向（原点 = eye）。
    #[test]
    fn screen_ray_center_points_at_target() {
        let camera = Camera::default();
        let (origin, dir) = camera.screen_ray(Vec2::ZERO, 16.0 / 9.0);
        assert_eq!(origin, camera.eye, "射线原点应为 eye");
        let want = (camera.target - camera.eye).normalize();
        assert!(
            dir.dot(want) > 1.0 - 1e-5,
            "中心射线应指向注视点方向：{dir:?} vs {want:?}"
        );
    }

    /// 屏幕射线：左缘 / 右缘射线方向发散（点积可区分），且各自朝向与
    /// NDC x 的符号一致（x < 0 偏左、x > 0 偏右）。
    #[test]
    fn screen_ray_edge_directions_are_distinguishable() {
        let camera = Camera::default();
        let (_, left) = camera.screen_ray(Vec2::new(-0.9, 0.0), 16.0 / 9.0);
        let (_, right) = camera.screen_ray(Vec2::new(0.9, 0.0), 16.0 / 9.0);
        assert!(
            left.dot(right) < 0.99,
            "左右缘射线应明显发散（fov 45°、16:9 下夹角 ~60°）：{left:?} vs {right:?}"
        );
        // 射线相对中心方向的水平偏移应与 NDC x 符号一致（右手系 +X 向右）
        let right_axis = camera.view_matrix().x_axis.truncate().normalize_or_zero();
        assert!(
            left.dot(right_axis) < 0.0 && right.dot(right_axis) > 0.0,
            "左缘射线应偏向相机左方、右缘偏向右方"
        );
    }

    /// 实体相机版屏幕射线与正向投影互逆：中心射线指向 -Z（无旋转相机
    /// 前方），前方世界点经 [`view_proj_from_global`] 投影回 NDC 中心。
    #[test]
    fn mounted_screen_ray_matches_forward_projection() {
        let gt = GlobalTransform::from_transform(Transform::from_xyz(10.0, 2.0, 3.0));
        let cam = ActiveCamera::default();
        let (origin, dir) = screen_ray_from_global(&gt, &cam, Vec2::ZERO, 16.0 / 9.0);
        assert_eq!(origin, Vec3::new(10.0, 2.0, 3.0), "原点应为实体全局位置");
        assert!(
            dir.dot(Vec3::NEG_Z) > 1.0 - 1e-5,
            "无旋转相机的中心射线应指向 -Z 前方"
        );
        // 正向投影互逆：相机前方 10 m 的点应落回 NDC (0, 0)
        let vp = view_proj_from_global(&gt, &cam, 16.0 / 9.0);
        let clip = vp * Vec3::new(10.0, 2.0, -7.0).extend(1.0);
        let ndc = clip.truncate() / clip.w;
        assert!(
            ndc.x.abs() < 1e-5 && ndc.y.abs() < 1e-5 && (0.0..=1.0).contains(&ndc.z),
            "相机正前方的点应投影到 NDC 中心：{ndc:?}"
        );
    }
}
