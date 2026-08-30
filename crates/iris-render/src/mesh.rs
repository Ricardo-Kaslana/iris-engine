//! 网格：CPU 数据目录 + 基础图元生成器。

use iris_core::Resource;
use wgpu::util::DeviceExt;

/// 网格句柄。指向 `MeshStore` 中的条目，也作为 ECS 组件挂载到实体上。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, iris_core::Component)]
pub struct MeshHandle(pub u32);

/// 顶点：位置 + 法线 + 纹理坐标（32 字节，POD）。
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

impl Vertex {
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[
            wgpu::VertexAttribute { offset: 0, shader_location: 0, format: wgpu::VertexFormat::Float32x3 },
            wgpu::VertexAttribute { offset: 12, shader_location: 1, format: wgpu::VertexFormat::Float32x3 },
            wgpu::VertexAttribute { offset: 24, shader_location: 2, format: wgpu::VertexFormat::Float32x2 },
        ],
    };
}

/// CPU 侧网格数据（顶点 + 索引）。
#[derive(Clone, Debug, Default)]
pub struct MeshData {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

impl MeshData {
    /// 单位立方体。见 [`primitives::cube`]。
    pub fn cube() -> Self {
        primitives::cube()
    }

    /// XZ 平面地面。见 [`primitives::plane`]。
    pub fn plane(size: f32) -> Self {
        primitives::plane(size)
    }

    /// UV 球体。见 [`primitives::uv_sphere`]。
    pub fn uv_sphere(radius: f32, segments: u32, rings: u32) -> Self {
        primitives::uv_sphere(radius, segments, rings)
    }

    /// 计算包围盒（用于将来视锥剔除）。
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let mut iter = self.vertices.iter().map(|v| v.position);
        let first = iter.next()?;
        let mut min = first;
        let mut max = first;
        for p in iter {
            for i in 0..3 {
                min[i] = min[i].min(p[i]);
                max[i] = max[i].max(p[i]);
            }
        }
        Some((min, max))
    }
}

/// 基础图元生成器。
pub mod primitives {
    use super::{MeshData, Vertex};

    /// 向网格追加一个面：`u`/`v` 为两条半边向量，且 `u × v == normal`（保证 CCW 正面朝外）。
    fn add_face(data: &mut MeshData, center: [f32; 3], normal: [f32; 3], u: [f32; 3], v: [f32; 3]) {
        let base = data.vertices.len() as u32;
        let corners = [
            [center[0] - u[0] - v[0], center[1] - u[1] - v[1], center[2] - u[2] - v[2]],
            [center[0] + u[0] - v[0], center[1] + u[1] - v[1], center[2] + u[2] - v[2]],
            [center[0] + u[0] + v[0], center[1] + u[1] + v[1], center[2] + u[2] + v[2]],
            [center[0] - u[0] + v[0], center[1] - u[1] + v[1], center[2] - u[2] + v[2]],
        ];
        let uvs = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        for (c, uv) in corners.into_iter().zip(uvs) {
            data.vertices.push(Vertex { position: c, normal, uv });
        }
        data.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    /// 单位立方体（边长 1，中心在原点）：24 顶点 / 36 索引。
    pub fn cube() -> MeshData {
        let mut data = MeshData::default();
        let h = 0.5;
        // +Z / -Z
        add_face(&mut data, [0.0, 0.0, h], [0.0, 0.0, 1.0], [h, 0.0, 0.0], [0.0, h, 0.0]);
        add_face(&mut data, [0.0, 0.0, -h], [0.0, 0.0, -1.0], [-h, 0.0, 0.0], [0.0, h, 0.0]);
        // +X / -X
        add_face(&mut data, [h, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -h], [0.0, h, 0.0]);
        add_face(&mut data, [-h, 0.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, h], [0.0, h, 0.0]);
        // +Y / -Y
        add_face(&mut data, [0.0, h, 0.0], [0.0, 1.0, 0.0], [h, 0.0, 0.0], [0.0, 0.0, -h]);
        add_face(&mut data, [0.0, -h, 0.0], [0.0, -1.0, 0.0], [h, 0.0, 0.0], [0.0, 0.0, h]);
        data
    }

    /// XZ 平面上的方形地面（法线 +Y）。
    pub fn plane(size: f32) -> MeshData {
        let mut data = MeshData::default();
        let h = size * 0.5;
        add_face(&mut data, [0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [h, 0.0, 0.0], [0.0, 0.0, -h]);
        data
    }

    /// UV 球体。
    pub fn uv_sphere(radius: f32, segments: u32, rings: u32) -> MeshData {
        assert!(segments >= 3 && rings >= 2, "球体分段数过低");
        let mut data = MeshData::default();
        for ring in 0..=rings {
            let phi = std::f32::consts::PI * ring as f32 / rings as f32;
            let (sin_phi, cos_phi) = phi.sin_cos();
            for seg in 0..=segments {
                let theta = std::f32::consts::TAU * seg as f32 / segments as f32;
                let (sin_theta, cos_theta) = theta.sin_cos();
                let normal = [cos_theta * sin_phi, cos_phi, sin_theta * sin_phi];
                data.vertices.push(Vertex {
                    position: [normal[0] * radius, normal[1] * radius, normal[2] * radius],
                    normal,
                    uv: [seg as f32 / segments as f32, ring as f32 / rings as f32],
                });
            }
        }
        let stride = segments + 1;
        for ring in 0..rings {
            for seg in 0..segments {
                let a = ring * stride + seg;
                let b = a + 1;
                let c = a + stride;
                let d = c + 1;
                // 环绕方向与 add_face 约定一致（从外侧看 CCW）：(∂φ)×(∂θ) 指向
                // 球内，因此索引须按 [a,d,c / a,b,d] 反向——此前为 [a,c,d / a,d,b]，
                // 正面被剔除后渲染的是内壁（法线背向相机，直接光照全为 0，
                // 实测症状：球体在强光下仍是纯黑剪影）。
                data.indices.extend_from_slice(&[a, d, c, a, b, d]);
            }
        }
        data
    }
}

/// CPU 侧网格目录（World 资源）。GPU 上传由渲染器按需懒执行。
#[derive(Default, Resource)]
pub struct MeshStore {
    meshes: Vec<MeshData>,
}

impl MeshStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, data: MeshData) -> MeshHandle {
        self.meshes.push(data);
        MeshHandle(self.meshes.len() as u32 - 1)
    }

    pub fn get(&self, handle: MeshHandle) -> Option<&MeshData> {
        self.meshes.get(handle.0 as usize)
    }

    pub fn len(&self) -> usize {
        self.meshes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.meshes.is_empty()
    }
}

/// 便捷上传：由渲染器调用，将 `MeshData` 变为 GPU 缓冲。
pub(crate) fn upload_mesh(gpu: &crate::context::Gpu, data: &MeshData) -> (wgpu::Buffer, wgpu::Buffer, u32) {
    let vertex_buf = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("iris.mesh.vbo"),
        contents: bytemuck::cast_slice(&data.vertices),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let index_buf = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("iris.mesh.ibo"),
        contents: bytemuck::cast_slice(&data.indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    (vertex_buf, index_buf, data.indices.len() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_topology() {
        let cube = primitives::cube();
        assert_eq!(cube.vertices.len(), 24);
        assert_eq!(cube.indices.len(), 36);
        // 法线必须全部归一化
        for v in &cube.vertices {
            let n = v.normal;
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-5);
        }
        // 索引必须在顶点范围内
        assert!(cube.indices.iter().all(|&i| (i as usize) < cube.vertices.len()));
        // 包围盒为 [-0.5, 0.5]^3
        let (min, max) = cube.bounds().unwrap();
        assert!(min.iter().all(|&x| (x + 0.5).abs() < 1e-5));
        assert!(max.iter().all(|&x| (x - 0.5).abs() < 1e-5));
    }

    #[test]
    fn cube_faces_cull_correctly() {
        // 每个面的四个顶点必须满足 u × v == normal（CCW 正面朝外）：
        // 直接验证顶点法线与（几何中心 -> 顶点）的外向性一致
        let cube = primitives::cube();
        for chunk in cube.vertices.chunks(4) {
            let normal = chunk[0].normal;
            let center = [
                chunk.iter().map(|v| v.position[0]).sum::<f32>() / 4.0,
                chunk.iter().map(|v| v.position[1]).sum::<f32>() / 4.0,
                chunk.iter().map(|v| v.position[2]).sum::<f32>() / 4.0,
            ];
            let (u0, u1) = (chunk[0].position, chunk[1].position);
            let (u2, u3) = (chunk[2].position, chunk[3].position);
            let edge1 = [u1[0] - u0[0], u1[1] - u0[1], u1[2] - u0[2]];
            let edge2 = [u2[0] - u1[0], u2[1] - u1[1], u2[2] - u1[2]];
            let cross = [
                edge1[1] * edge2[2] - edge1[2] * edge2[1],
                edge1[2] * edge2[0] - edge1[0] * edge2[2],
                edge1[0] * edge2[1] - edge1[1] * edge2[0],
            ];
            let dot = cross[0] * normal[0] + cross[1] * normal[1] + cross[2] * normal[2];
            assert!(dot > 0.0, "面环绕顺序必须为 CCW（法线 {normal:?}）");
            // 面中心应位于单位立方体表面
            let _ = u3;
            assert!(
                center.iter().any(|&c| c.abs() > 0.49 && c.abs() < 0.51),
                "面中心 {center:?} 应位于立方体表面"
            );
        }
    }

    #[test]
    fn sphere_topology() {
        let sphere = primitives::uv_sphere(1.0, 16, 12);
        assert_eq!(sphere.vertices.len(), (16 + 1) * (12 + 1) as usize);
        assert_eq!(sphere.indices.len(), 16 * 12 * 6);
        for v in &sphere.vertices {
            let len = (v.position[0] * v.position[0]
                + v.position[1] * v.position[1]
                + v.position[2] * v.position[2])
                .sqrt();
            assert!((len - 1.0).abs() < 1e-4, "球面顶点应位于半径 1 上");
        }
    }
}
