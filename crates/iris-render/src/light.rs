//! 灯光资源与组件。

use glam::{Mat4, Vec3};
use iris_core::{Component, Resource};

/// 单帧允许的最大点光数量（uniform 数组容量，超出部分被忽略并记录日志）。
pub const MAX_POINT_LIGHTS: usize = 8;

/// 方向光（太阳光）。作为 `World` 资源存在，未注册时使用默认值。
#[derive(Clone, Debug, Resource)]
pub struct DirectionalLight {
    /// 光的传播方向（从光源指向场景，如太阳照射方向）。
    pub direction: Vec3,
    /// 光的颜色（线性空间）。
    pub color: Vec3,
    /// 强度倍率。
    pub intensity: f32,
    /// 环境光（各方向均匀的底色照明）。
    pub ambient: Vec3,
    /// 是否投射阴影（方向光阴影贴图，PCF 软化）。
    pub shadows: bool,
    /// 阴影覆盖半径（世界单位）：以相机注视点为中心的正方形边长的一半。
    pub shadow_range: f32,
}

impl Default for DirectionalLight {
    fn default() -> Self {
        Self {
            direction: Vec3::new(-0.5, -1.0, -0.4),
            color: Vec3::new(1.0, 0.98, 0.92),
            intensity: 1.0,
            ambient: Vec3::splat(0.14),
            shadows: true,
            shadow_range: 24.0,
        }
    }
}

impl DirectionalLight {
    /// 从光源视角构造正交视图投影矩阵，覆盖以 `center` 为中心、
    /// 半边长 `range` 的立方区域（用于阴影贴图渲染）。
    ///
    /// 返回 `(view_proj, 深度范围远平面)`。光传播方向接近竖直时
    /// 上方向自动回退到 +X，避免 look_at 退化。
    pub fn shadow_view_proj(&self, center: Vec3, range: f32) -> Mat4 {
        let dir = self.direction.normalize_or_zero();
        let distance = range * 2.0;
        let eye = center - dir * distance;
        // 上方向与光方向接近平行时换用 +X
        let up = if dir.y.abs() > 0.99 { Vec3::X } else { Vec3::Y };
        let view = glam::camera::rh::view::look_at_mat4(eye, center, up);
        let proj = glam::camera::rh::proj::directx::orthographic(
            -range,
            range,
            -range,
            range,
            0.1,
            distance + range,
        );
        proj * view
    }
}

/// 点光源组件。位置取自同一实体的 `GlobalTransform`（层级系统每帧更新）。
///
/// 衰减模型：`saturate(1 - d/range)²`，在 `range` 处平滑归零，
/// 颜色 × 强度决定亮度。超过 [`MAX_POINT_LIGHTS`] 的点光会被忽略。
#[derive(Clone, Copy, Debug, Component)]
pub struct PointLight {
    /// 光的颜色（线性空间）。
    pub color: Vec3,
    /// 强度倍率。
    pub intensity: f32,
    /// 有效半径（世界单位），超出后无贡献。
    pub range: f32,
}

impl Default for PointLight {
    fn default() -> Self {
        Self { color: Vec3::ONE, intensity: 8.0, range: 12.0 }
    }
}

impl PointLight {
    pub fn new(color: Vec3, intensity: f32, range: f32) -> Self {
        Self { color, intensity, range }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_matrix_maps_center_to_ndc_center() {
        let light = DirectionalLight::default();
        let center = Vec3::new(2.0, 0.0, -3.0);
        let range = 24.0;
        let vp = light.shadow_view_proj(center, range);
        let clip = vp * center.extend(1.0);
        let ndc = clip.truncate() / clip.w;
        assert!(ndc.x.abs() < 1e-4 && ndc.y.abs() < 1e-4, "覆盖中心应投影到 NDC 原点");
        // 光源相机沿 -dir 拉远 2×range，覆盖中心的深度应精确落在
        // (distance - near) / (far - near) —— 保证 center ± range 全程在 [0, 1]
        let distance = range * 2.0;
        let expected_z = (distance - 0.1) / (distance + range - 0.1);
        assert!((ndc.z - expected_z).abs() < 1e-4, "深度 {ndc:?} 应为 {expected_z}");
    }

    #[test]
    fn shadow_matrix_keeps_unit_cube_inside() {
        let light = DirectionalLight::default();
        let vp = light.shadow_view_proj(Vec3::ZERO, 24.0);
        // 覆盖范围内任意角点都应落在 NDC 盒内
        for corner in [
            Vec3::new(10.0, 10.0, 10.0),
            Vec3::new(-10.0, -10.0, -10.0),
            Vec3::new(10.0, -10.0, 10.0),
        ] {
            let clip = vp * corner.extend(1.0);
            let ndc = clip.truncate() / clip.w;
            assert!(ndc.x.abs() <= 1.0 && ndc.y.abs() <= 1.0 && (0.0..=1.0).contains(&ndc.z),
                "角点 {corner:?} 超出阴影覆盖范围：{ndc:?}");
        }
    }

    #[test]
    fn vertical_light_direction_falls_back_up_vector() {
        // 光方向几乎竖直时 look_at 会退化，不应产生 NaN
        let light = DirectionalLight { direction: Vec3::NEG_Y, ..Default::default() };
        let vp = light.shadow_view_proj(Vec3::ZERO, 24.0);
        assert!(vp.to_cols_array().iter().all(|v| v.is_finite()), "矩阵不应包含 NaN");
    }
}
