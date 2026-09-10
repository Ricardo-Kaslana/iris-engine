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

    /// 「全通过」视锥（R62a 反射探针捕获用）：覆盖以 `center` 为中心、
    /// 半边长 `radius` 的**立方体**的正交视锥（六个平面全部朝外扩到该盒）。
    ///
    /// 用途：探针的 6 个 90° 面并集就是**完整球域**，逐面视锥剔除没有收益，
    /// 而漏画几何会让探针把远景记成天空（不可接受的错误）。捕获期因此用
    /// 一个足够大的正交视锥让既有收集路径「全部保留」——剔不剔都不影响
    /// 正确性，只是给 `collect_instances` 一个不裁剪的边界（半径取关卡
    /// 量级的常量，见 `probe::PROBE_CAPTURE_FAR`）。
    pub fn capture_all(center: Vec3, radius: f32) -> Self {
        // 视图眼点 = 盒中心、看向 −Z（正交下只影响符号无关的轴向映射）
        let view = glam::camera::rh::view::look_at_mat4(center, center - Vec3::Z, Vec3::Y);
        // near/far 取 ±radius：RH 视图空间 z 落在 [−radius, radius] 的点
        // （即世界空间 z ∈ center ± radius）全部进入 NDC 深度 [0, 1]
        let proj = glam::camera::rh::proj::directx::orthographic(
            -radius, radius, -radius, radius, -radius, radius,
        );
        Self::from_view_proj(&(proj * view))
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
        let view =
            glam::camera::rh::view::look_at_mat4(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
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
        assert!(!frustum.intersects_aabb(Vec3::new(-0.5, -0.5, 19.5), Vec3::new(0.5, 0.5, 20.5),));
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

    /// 捕获视锥（R62a）：立方体内的点全部保留（无假阴性），侧向越界被剔除，
    /// 沿视轴方向**保守多留**（正交矩阵的 w 行退化 ⇒ Gribb-Hartmann 的近
    /// 平面提取不产生有效约束——对「6 面并集 = 全方向」的捕获而言，多留正是
    /// 我们要的方向，少留才会把几何记成天空）。
    #[test]
    fn capture_all_contains_the_requested_cube_conservatively() {
        let center = Vec3::new(-3.0, 1.5, 8.0);
        let radius = 50.0;
        let frustum = Frustum::capture_all(center, radius);
        // 立方体内（含边界内的 0.9R）必须全部保留
        for axis in 0..3 {
            for sign in [-1.0f32, 1.0] {
                let mut inside = center;
                inside[axis] += sign * radius * 0.9;
                assert!(
                    frustum.intersects_sphere(inside, 0.0),
                    "轴 {axis} 方向 {sign} 的 0.9R 点应在捕获视锥内：{inside:?}"
                );
            }
        }
        // 侧向（±X / ±Y）越界必须剔除：这两轴由正交投影的 XY 范围约束
        for axis in [0usize, 1] {
            for sign in [-1.0f32, 1.0] {
                let mut outside = center;
                outside[axis] += sign * radius * 1.1;
                assert!(
                    !frustum.intersects_sphere(outside, 0.0),
                    "轴 {axis} 方向 {sign} 的 1.1R 点应被捕获视锥剔除：{outside:?}"
                );
            }
        }
        // 沿视轴（世界 ±Z）：正交矩阵 w 行退化 ⇒ 近平面无约束，1.1R 仍被保留
        // （保守方向，见测试文档）；但**远端**由 far 平面约束住
        let behind = center + Vec3::new(0.0, 0.0, radius * 1.1);
        assert!(
            frustum.intersects_sphere(behind, 0.0),
            "视轴方向的多留是刻意的保守行为（不产生假阴性）"
        );
        let beyond_far = center + Vec3::new(0.0, 0.0, -radius * 1.5);
        assert!(
            !frustum.intersects_sphere(beyond_far, 0.0),
            "远端越界（−Z 方向 1.5R）应被 far 平面剔除：{beyond_far:?}"
        );
        // 角点（三分量同时 0.9R）仍在锥内
        let corner = center + Vec3::splat(radius * 0.9);
        assert!(frustum.intersects_aabb(corner, corner + Vec3::splat(0.1)));
    }
}
