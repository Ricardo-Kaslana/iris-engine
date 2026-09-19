//! 毛发组件与壳层实例展开（R90）。
//!
//! 两条渲染路线的 ECS 侧挂载点（缺席 = 零计算零绘制，与
//! `ParticleEmitterV2` / `OcclusionCulling` 同款语义）：
//!
//! - [`HairStrands`]（发带路线）：实体挂本组件 + `MaterialHandle`
//!   （Kajiya-Kay 材质）+ `Transform`。groom 产物（或自定义控制网格）
//!   由渲染器按实体缓存 GPU 顶点 / 索引缓冲（句柄值缓存——**同一
//!   渲染器上渲染「同句柄不同内容」的多个世界会串味**，跨世界对照
//!   必须各用独立渲染器，R53/R62 同款纪律），主 pass 不透明段之后
//!   以 cutout 语义（深度写）绘制。
//! - [`ShellFur`]（壳层路线）：实体挂本组件 + 既有 `MeshHandle` /
//!   `MaterialHandle`（密度 / 颜色 = base 贴图的 alpha / RGB——`L(uv)`
//!   即毛长图，值 d 处毛长 = d × height）+ `Transform`。渲染器把该
//!   实体展开为 N 个壳层实例（[`ShellInstanceRaw`]，层比 t = k/N），
//!   VS 沿法线膨胀 t × height、FS 按 `base.a < t` 丢弃——毛尖渐隐。
//!   底皮（原网格）照常走普通不透明实例路径。
//!
//! 蒙皮：v1 两条路线均不跟随骨骼（发带挂静态实体；壳层实体不蒙皮）
//! ——R91 与风场摆动 / 阴影一起处理（诚实限制 §8）。

use crate::instance::InstanceRaw;

/// 发带组件（相机面向条带路线）。
#[derive(Clone, Debug, iris_core::Component)]
pub struct HairStrands {
    /// groom 控制网格（[`crate::groom::generate_hair_strands`] 产物，
    /// 或调用方自建——顶点语义见 [`crate::groom::HairStrandVertex`]）。
    pub mesh: crate::groom::HairStrandMesh,
    /// 包围球（实体局部空间：center + radius），视锥剔除用。由
    /// [`Self::new`] 从网格顶点派生；自定义网格也可显式给定（保守值
    /// 即可——只影响剔除，不影响绘制正确性）。
    pub bounds_center: [f32; 3],
    pub bounds_radius: f32,
}

impl HairStrands {
    /// 从 groom 网格构建（包围球从顶点派生：AABB 中心 + 对角半径）。
    pub fn new(mesh: crate::groom::HairStrandMesh) -> Self {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for v in &mesh.vertices {
            for k in 0..3 {
                min[k] = min[k].min(v.position[k]);
                max[k] = max[k].max(v.position[k]);
            }
        }
        if mesh.vertices.is_empty() {
            return Self {
                mesh,
                bounds_center: [0.0; 3],
                bounds_radius: 0.0,
            };
        }
        let center = [
            (min[0] + max[0]) * 0.5,
            (min[1] + max[1]) * 0.5,
            (min[2] + max[2]) * 0.5,
        ];
        let radius = ((max[0] - center[0]).powi(2)
            + (max[1] - center[1]).powi(2)
            + (max[2] - center[2]).powi(2))
        .sqrt();
        Self {
            mesh,
            bounds_center: center,
            bounds_radius: radius,
        }
    }
}

/// 壳层毛组件（shell fur 路线）。
#[derive(Clone, Copy, Debug, PartialEq, iris_core::Component)]
pub struct ShellFur {
    /// 附加壳层数 N（≥ 1；不含底皮）。层比 t_k = k/N，k = 1..=N
    ///（顶皮 t = 1）。层数 = 毛长量化级数——密度贴图值 d 的毛在
    /// 第 ⌈d·N⌉ 层以上消失，毛长 = d × height。
    pub layers: u32,
    /// 毛高（世界单位，从表面沿法线挤出的最大距离）。
    pub height: f32,
}

impl Default for ShellFur {
    fn default() -> Self {
        Self {
            layers: 8,
            height: 0.05,
        }
    }
}

/// 壳层实例数据（GPU 实例缓冲布局，[`SHELL_INSTANCE_LAYOUT`]）：
/// [`InstanceRaw`] 前缀 + 壳层参数（loc 15）。
///
/// `shell.x` = 层比 t ∈ (0, 1]（VS 膨胀比例 / FS 逐层 alpha 阈值），
/// `shell.y` = 毛高（世界单位），`z/w` 保留。
#[repr(C)]
#[derive(Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ShellInstanceRaw {
    pub base: InstanceRaw,
    pub shell: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<ShellInstanceRaw>() == 144);

/// 壳层实例布局：InstanceRaw 的 loc 3–9 / 14 + 壳层参数 loc 15。
pub const SHELL_INSTANCE_LAYOUT: wgpu::VertexBufferLayout<'static> = {
    const ATTRS: &[wgpu::VertexAttribute; 9] = &[
        wgpu::VertexAttribute {
            offset: 0,
            shader_location: 3,
            format: wgpu::VertexFormat::Float32x4,
        },
        wgpu::VertexAttribute {
            offset: 16,
            shader_location: 4,
            format: wgpu::VertexFormat::Float32x4,
        },
        wgpu::VertexAttribute {
            offset: 32,
            shader_location: 5,
            format: wgpu::VertexFormat::Float32x4,
        },
        wgpu::VertexAttribute {
            offset: 48,
            shader_location: 6,
            format: wgpu::VertexFormat::Float32x4,
        },
        wgpu::VertexAttribute {
            offset: 64,
            shader_location: 7,
            format: wgpu::VertexFormat::Float32x4,
        },
        wgpu::VertexAttribute {
            offset: 80,
            shader_location: 8,
            format: wgpu::VertexFormat::Float32x4,
        },
        wgpu::VertexAttribute {
            offset: 96,
            shader_location: 9,
            format: wgpu::VertexFormat::Float32x4,
        },
        wgpu::VertexAttribute {
            offset: 112,
            shader_location: 14,
            format: wgpu::VertexFormat::Float32x4,
        },
        wgpu::VertexAttribute {
            offset: 128,
            shader_location: 15,
            format: wgpu::VertexFormat::Float32x4,
        },
    ];
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<ShellInstanceRaw>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: ATTRS,
    }
};

/// 壳层层比序列：`k/N`，k = 1..=N（升序——实例序即绘制序，确定性）。
///
/// 纯函数（渲染器收集期消费，单测锁定口径）。
pub fn shell_layer_ratios(layers: u32) -> Vec<f32> {
    assert!(layers >= 1, "壳层数至少 1");
    (1..=layers).map(|k| k as f32 / layers as f32).collect()
}

/// 展开一个实体的壳层实例：底皮实例（调用方已进普通路径）之外，
/// 每层一个 [`ShellInstanceRaw`]，层比升序。
pub fn expand_shell_instances(fur: &ShellFur, base: InstanceRaw) -> Vec<ShellInstanceRaw> {
    shell_layer_ratios(fur.layers)
        .into_iter()
        .map(|t| ShellInstanceRaw {
            base,
            shell: [t, fur.height, 0.0, 0.0],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 层比序列口径：升序、首项 1/N、末项恰 1、数量 = N。
    #[test]
    fn layer_ratios_formula() {
        assert_eq!(shell_layer_ratios(1), vec![1.0]);
        assert_eq!(shell_layer_ratios(4), vec![0.25, 0.5, 0.75, 1.0]);
        assert_eq!(shell_layer_ratios(8).len(), 8);
        let ratios = shell_layer_ratios(8);
        assert_eq!(*ratios.first().unwrap(), 0.125);
        assert_eq!(*ratios.last().unwrap(), 1.0);
        // 升序（实例序 = 层序的确定性根据）
        for w in ratios.windows(2) {
            assert!(w[0] < w[1]);
        }
    }

    /// 展开实例：层数一致、层比 / 毛高写入正确、基座实例逐字段保真。
    #[test]
    fn expand_writes_ratios_and_preserves_base() {
        let fur = ShellFur {
            layers: 5,
            height: 0.12,
        };
        let base =
            InstanceRaw::from_matrix(glam::Mat4::from_translation(glam::Vec3::new(1.0, 2.0, 3.0)));
        let shells = expand_shell_instances(&fur, base);
        assert_eq!(shells.len(), 5);
        for (k, s) in shells.iter().enumerate() {
            assert_eq!(s.shell[0], (k + 1) as f32 / 5.0);
            assert_eq!(s.shell[1], 0.12);
            assert_eq!(s.base.model[3], [1.0, 2.0, 3.0, 1.0], "基座矩阵逐字段保真");
        }
    }

    /// 缺省层数 / 毛高的量级（草地 demo 用的保守值）。
    #[test]
    fn shell_fur_default_magnitude() {
        let d = ShellFur::default();
        assert_eq!(d.layers, 8);
        assert_eq!(d.height, 0.05);
    }
}
