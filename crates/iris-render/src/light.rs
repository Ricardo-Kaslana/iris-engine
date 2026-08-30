//! 灯光资源与组件。

use glam::Vec3;
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
}

impl Default for DirectionalLight {
    fn default() -> Self {
        Self {
            direction: Vec3::new(-0.5, -1.0, -0.4),
            color: Vec3::new(1.0, 0.98, 0.92),
            intensity: 1.0,
            ambient: Vec3::splat(0.14),
        }
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
