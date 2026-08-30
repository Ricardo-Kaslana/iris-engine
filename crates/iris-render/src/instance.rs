//! 实例数据：每实例的模型矩阵与法线矩阵（GPU 顶点缓冲布局）。

use glam::{Mat3, Mat4};
use iris_core::prelude::Transform;

/// GPU 实例数据（112 字节，POD）。
///
/// `model` 为列主序 4x4 模型矩阵；`normal` 为模型矩阵逆转置的 3x3
/// 法线矩阵（每列补零填充到 vec4，规避 WGSL 对齐陷阱）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct InstanceRaw {
    pub model: [[f32; 4]; 4],
    pub normal: [[f32; 4]; 3],
}

impl InstanceRaw {
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<InstanceRaw>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &[
            wgpu::VertexAttribute { offset: 0, shader_location: 3, format: wgpu::VertexFormat::Float32x4 },
            wgpu::VertexAttribute { offset: 16, shader_location: 4, format: wgpu::VertexFormat::Float32x4 },
            wgpu::VertexAttribute { offset: 32, shader_location: 5, format: wgpu::VertexFormat::Float32x4 },
            wgpu::VertexAttribute { offset: 48, shader_location: 6, format: wgpu::VertexFormat::Float32x4 },
            wgpu::VertexAttribute { offset: 64, shader_location: 7, format: wgpu::VertexFormat::Float32x4 },
            wgpu::VertexAttribute { offset: 80, shader_location: 8, format: wgpu::VertexFormat::Float32x4 },
            wgpu::VertexAttribute { offset: 96, shader_location: 9, format: wgpu::VertexFormat::Float32x4 },
        ],
    };

    /// 从世界空间模型矩阵构造实例数据。
    ///
    /// 注意：非均匀缩放且缩放分量为 0 时矩阵不可逆，将产生 NaN ——
    /// 调用方应保证缩放合法。
    pub fn from_matrix(model: Mat4) -> Self {
        let normal = Mat3::from_mat4(model).inverse().transpose();
        let mc = model.to_cols_array_2d();
        let nc = normal.to_cols_array_2d();
        Self {
            model: mc,
            normal: [
                [nc[0][0], nc[0][1], nc[0][2], 0.0],
                [nc[1][0], nc[1][1], nc[1][2], 0.0],
                [nc[2][0], nc[2][1], nc[2][2], 0.0],
            ],
        }
    }

    /// 从本地变换构造实例数据（等价于 `from_matrix(transform.matrix())`）。
    pub fn from_transform(transform: &Transform) -> Self {
        Self::from_matrix(transform.matrix())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core::prelude::Vec3;

    #[test]
    fn instance_transform_roundtrip() {
        let t = Transform::from_xyz(1.0, 2.0, 3.0);
        let raw = InstanceRaw::from_transform(&t);
        // 列主序：第 4 列是平移
        assert_eq!(raw.model[3], [1.0, 2.0, 3.0, 1.0]);
        // 单位旋转 + 均匀缩放 → 法线矩阵 = 旋转部分（逆转置后仍为旋转）
        assert_eq!(raw.normal[0][0], 1.0);
        assert_eq!(raw.normal[1][1], 1.0);
        assert_eq!(raw.normal[2][2], 1.0);
        assert_eq!(raw.normal[0][3], 0.0);
    }

    #[test]
    fn normal_matrix_correct_under_uniform_scale() {
        let scale = 3.0;
        let t = Transform { scale: Vec3::splat(scale), ..Transform::from_rotation_y(1.23) };
        let raw = InstanceRaw::from_transform(&t);
        // 逆转置下均匀缩放 s 使每列长度变为 1/s（着色器 normalize 后恢复单位长度）；
        // 关键不变量是各列长度一致（保持角度关系）
        for col in &raw.normal {
            let len = (col[0] * col[0] + col[1] * col[1] + col[2] * col[2]).sqrt();
            assert!((len - 1.0 / scale).abs() < 1e-5, "列长 {len} 应为 1/s");
        }
    }
}
