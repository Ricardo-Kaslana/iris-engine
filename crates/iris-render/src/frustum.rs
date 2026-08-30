//! 视锥体：从视图投影矩阵提取 6 个裁剪平面，用于渲染前的快速剔除。
//!
//! 提取方法为 Gribb-Hartmann（«Fast Extraction of Viewing Frustum Planes
//! from the World-View-Projection Matrix»），适配 z ∈ [0, 1] 的
//! DirectX/WebGPU 深度约定。

use glam::{Mat4, Vec3, Vec4};

/// 视锥体（世界空间，平面法线朝内）。
#[derive(Clone, Copy, Debug)]
pub struct Frustum {
    /// [left, right, bottom, top, near, far]，每个平面 `Vec4 = (normal.xyz, d)`，
    /// 点在锥内满足 `normal·p + d >= 0`。
    planes: [Vec4; 6],
}

impl Frustum {
    /// 从视图投影矩阵提取视锥平面（每个平面按法线长度归一化，
    /// 使后续的距离比较与实际世界单位一致）。
    pub fn from_view_proj(view_proj: &Mat4) -> Self {
        // 列主序存储下第 r 行由各列向量的第 r 个分量组成
        let row = |r: usize| {
            Vec4::new(
                view_proj.x_axis[r],
                view_proj.y_axis[r],
                view_proj.z_axis[r],
                view_proj.w_axis[r],
            )
        };
        let r0 = row(0);
        let r1 = row(1);
        let r2 = row(2);
        let r3 = row(3);
        let mut planes = [
            r3 + r0, // left
            r3 - r0, // right
            r3 + r1, // bottom
            r3 - r1, // top
            r3,      // near（z ∈ [0,1] 约定下近平面即 row3）
            r3 - r2, // far
        ];
        for plane in &mut planes {
            let len = plane.truncate().length();
            if len > f32::EPSILON {
                *plane /= len;
            }
        }
        Self { planes }
    }

    /// 球体是否与视锥相交（或完全在内）。用于保守剔除。
    pub fn intersects_sphere(&self, center: Vec3, radius: f32) -> bool {
        self.planes
            .iter()
            .all(|p| p.truncate().dot(center) + p.w >= -radius)
    }

    /// 轴对齐包围盒是否与视锥相交（p-vertex 测试）。
    pub fn intersects_aabb(&self, min: Vec3, max: Vec3) -> bool {
        self.planes.iter().all(|p| {
            let n = p.truncate();
            // 正顶点：法线方向上离平面最远的角
            let p_vertex = Vec3::new(
                if n.x >= 0.0 { max.x } else { min.x },
                if n.y >= 0.0 { max.y } else { min.y },
                if n.z >= 0.0 { max.z } else { min.z },
            );
            n.dot(p_vertex) + p.w >= 0.0
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    fn default_frustum() -> Frustum {
        let view = glam::camera::rh::view::look_at_mat4(
            Vec3::new(0.0, 0.0, 5.0),
            Vec3::ZERO,
            Vec3::Y,
        );
        let proj = glam::camera::rh::proj::directx::perspective(
            std::f32::consts::FRAC_PI_4,
            1.0,
            0.1,
            100.0,
        );
        Frustum::from_view_proj(&(proj * view))
    }

    #[test]
    fn point_inside_near_origin_is_visible() {
        let frustum = default_frustum();
        assert!(frustum.intersects_sphere(Vec3::ZERO, 0.5));
        assert!(frustum.intersects_aabb(Vec3::new(-0.5, -0.5, -0.5), Vec3::splat(0.5)));
    }

    #[test]
    fn point_behind_camera_is_culled() {
        let frustum = default_frustum();
        // 相机在 +Z 看向原点：背后是更远的 +Z
        assert!(!frustum.intersects_sphere(Vec3::new(0.0, 0.0, 20.0), 0.5));
        assert!(!frustum.intersects_aabb(
            Vec3::new(-0.5, -0.5, 19.5),
            Vec3::new(0.5, 0.5, 20.5),
        ));
    }

    #[test]
    fn point_far_beyond_far_plane_is_culled() {
        let frustum = default_frustum();
        assert!(!frustum.intersects_sphere(Vec3::new(0.0, 0.0, -500.0), 0.5));
    }

    #[test]
    fn point_far_to_the_side_is_culled() {
        let frustum = default_frustum();
        assert!(!frustum.intersects_sphere(Vec3::new(1000.0, 0.0, -5.0), 0.5));
    }

    #[test]
    fn large_sphere_partially_outside_is_kept() {
        let frustum = default_frustum();
        // 巨大球体即使中心在视锥外也应保守保留（防止边界闪烁）
        assert!(frustum.intersects_sphere(Vec3::new(0.0, 0.0, 20.0), 100.0));
    }
}
