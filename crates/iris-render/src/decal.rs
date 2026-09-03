//! 贴花渲染（R39）：盒投影贴花（弹孔 / 血迹 / 地面标记）——正向前向
//! 管线的标准方案（decal cube projection）。
//!
//! ## 盒投影原理
//!
//! [`Decal`] 组件的实体变换定义一个**投影盒**：局部 Z 轴 = 投影方向
//! （垂直于贴花平面指向外），XY 平面 = 贴花平面，[`Decal::size`] 为盒
//! 三轴尺寸（Z 通常很薄）。渲染时绘制单位盒的 **±Z 两块端盖**（专用
//! 代理几何，见 [`proxy_geometry`]），顶点经逐实例世界盒矩阵变换；片元
//! 把插值世界位经**逆世界盒矩阵**（CPU 预计算，着色器不做矩阵求逆）变
//! 回盒局部 `local`，`|local| > 0.5` 即盒外 discard（盒裁剪，防御端盖
//! 边缘的浮点误差），`UV = local.xy + 0.5` 采样贴花纹理 × tint 后按
//! 标准 alpha 混合输出。端盖与被贴表面**共面**（推荐摆放见下）时映射
//! 是精确的平面投影——弹孔 / 血迹 / 地面标记类语义完全够用。
//!
//! **为什么代理几何只保留端盖、不画整盒侧面**：侧面的局部坐标恒在盒表
//! 面（如 +X 面 `local.x ≡ 0.5`），UV = local.xy + 0.5 会把纹理边缘
//! texel 拉伸成一圈「光晕」（ClampToEdge 下的经典贴花伪影）。端盖即投
//! 影平面，盒裁剪测试保留在着色器中作为边缘浮点误差的防御与语义声明。
//!
//! **推荐摆放**：盒中心沿局部 +Z 抬高 `size.z / 2`，使 **+Z 端盖与被贴
//! 表面重合**——深度测试为 LessEqual（共面等深必须通过，`Less` 会因浮
//! 点抖动闪烁）、深度写入关闭（贴花贴在已有表面上，不得污染后续透明 /
//! 粒子 pass 读取的不透明深度）。不抬高则端盖悬浮在表面上方 `z/2`，
//! 斜视角下出现视差错位（「悬浮」）。
//!
//! ## 绘制序（蒙皮之后、透明之前）
//!
//! 贴花 pass 位于主 pass（不透明 + 蒙皮）之后、透明 pass 之前：贴花是
//! **不透明表面的装饰**，语义上属于场景「实体」的一部分；玻璃等透明物
//! 应盖在贴花的**混合结果**之上（透过玻璃看到的墙面弹孔应带玻璃色）。
//! 「后画者在上」由 pass 顺序统一裁决，与 R23 透明、R24 粒子同构——
//! 透明 pass 的 alpha 混合自然把玻璃合成在贴花之上，无需任何特殊处理。
//!
//! ## 分批策略（按纹理）
//!
//! 收集后按纹理句柄排序分组，**同纹理的连续实例区间合并为 1 draw**（实
//! 例区间连续、批内仅切换组 1 纹理绑定组）。tint 在实例数据里逐实例携
//! 带，同纹理不同乘色的贴花**不拆批**。批内次序按实体 bits 并列序（确
//! 定性，与收集顺序无关）；同批重叠贴花的叠放先后未做视深排序——贴花
//! 常为不透明深色标记，v1 接受并列序（与透明几何的先后由 pass 顺序保
//! 证，不受此影响）。无贴花实例时整个 pass 跳过（零开销默认路径）。
//!
//! ## v1 限制（诚实清单）
//!
//! - **无光照**：输出 = 纹理 × tint，不参与 PBR / 方向光 / 阴影 / IBL。
//!   弹孔、血迹类深色标记语义足够；受光贴花（随表面明暗、接受阴影）需
//!   要把贴花混合移入光照方程或改写为延迟 decals，v1 不做——半亮环境
//!   下贴花会比表面「亮一档」，深色标记上不可察觉。
//! - **平面假设**：端盖是投影平面，被贴表面须在盒内近似共面；深盒贴斜
//!   面时端盖与表面的偏差上界为 `size.z / 2`（薄盒下不可见）。基于深度
//!   缓冲重投影的屏幕空间 decals（任意曲面精确贴合）留待后续。
//! - **不进阴影 pass**：贴花是表面装饰而非几何（同透明 / 粒子的理由）。
//! - 贴花之间不做逐对视深排序（见分批策略）。

use crate::texture::TextureHandle;
use glam::{Mat4, Vec3};

/// 贴花：实体变换定义投影盒（局部 Z 轴 = 投影方向，XY 为贴花平面），
/// `size` = 盒三轴尺寸（Z 常较薄），纹理 RGBA 的 A 通道控制覆盖。
///
/// ```ignore
/// // 墙面弹孔：盒 Z 轴沿墙面法线（世界 +Z），端盖与墙面重合
/// world.spawn((
///     Transform {
///         position: hit + normal * (thickness * 0.5),
///         rotation: Quat::from_rotation_arc(Vec3::Z, normal),
///         scale: Vec3::ONE,
///     },
///     Decal { texture, size: Vec3::new(0.3, 0.3, 0.05), tint: [1.0; 4] },
/// ));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, iris_core::Component)]
pub struct Decal {
    /// 贴花纹理（RGBA，A 通道控制覆盖；`TextureHandle(0)` = 白色兜底）。
    pub texture: TextureHandle,
    /// 投影盒三轴尺寸（实体局部轴上；三轴必须为正——零/负分量使盒矩阵
    /// 不可逆）。
    pub size: Vec3,
    /// 乘色（纹理采样值逐分量相乘；含整体 alpha 缩放）。
    pub tint: [f32; 4],
}

/// GPU 贴花实例数据（144 字节，POD）。
///
/// `model` 为世界盒矩阵（实体全局矩阵 × size 缩放，列主序）；`inv_model`
/// 为其逆——**CPU 预计算传入**，着色器不做矩阵求逆（片元需把插值世界位
/// 变回盒局部做盒裁剪与 UV 推导，顶点级求逆不可行，逐片元求逆更不可
/// 行）；`tint` 为乘色（含整体 alpha 缩放）。
///
/// 与规范的偏离说明：规范给定的实例布局为 model + inv_model = 128B，
/// 未含 tint 通道；而 tint 是 [`Decal`] 的**逐实例**语义（同纹理不同乘
/// 色的贴花并存），进实例数据是唯一不破坏「按纹理分批」的承载方式（按
/// (纹理, tint) 分批则同纹理异色贴花无法合批，且需引入每批 uniform 池）。
/// 144B = 64 + 64 + 16，9 × `Float32x4` 实例属性（location 1..=9）。
///
/// `PartialEq` 预留给将来的帧级实例比对（与 [`crate::instance::InstanceRaw`]
/// 同语义：逐字段 IEEE 相等；NaN 恒判不等 → 退化为重传，不影响正确性）。
#[repr(C)]
#[derive(Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct DecalInstanceRaw {
    pub model: [[f32; 4]; 4],
    pub inv_model: [[f32; 4]; 4],
    pub tint: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<DecalInstanceRaw>() == 144,
    "DecalInstanceRaw 必须为 144 字节（model 64B + inv_model 64B + tint 16B）"
);

impl DecalInstanceRaw {
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<DecalInstanceRaw>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &[
            wgpu::VertexAttribute {
                offset: 0,
                shader_location: 1,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 16,
                shader_location: 2,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 32,
                shader_location: 3,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 48,
                shader_location: 4,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 64,
                shader_location: 5,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 80,
                shader_location: 6,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 96,
                shader_location: 7,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 112,
                shader_location: 8,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 128,
                shader_location: 9,
                format: wgpu::VertexFormat::Float32x4,
            },
        ],
    };

    /// 从世界盒矩阵与乘色构造实例数据（逆矩阵在此求出——调用方每实例
    /// 一次，着色器零求逆）。
    ///
    /// 注意：`model` 含零缩放分量（如 `size` 出现 0）时不可逆，将产生
    /// NaN——调用方应保证 size 三轴为正（收集路径有 debug 断言）。
    pub fn from_model(model: Mat4, tint: [f32; 4]) -> Self {
        Self {
            model: model.to_cols_array_2d(),
            inv_model: model.inverse().to_cols_array_2d(),
            tint,
        }
    }
}

/// 代理几何顶点数（单位盒 ±Z 两块端盖，8 顶点）。
pub const PROXY_VERTEX_COUNT: usize = 8;
/// 代理几何索引数（每端盖 2 三角形，共 4 三角形 / 12 索引）。
pub const PROXY_INDEX_COUNT: usize = 12;

/// 代理几何顶点布局：仅位置（12 字节，location 0）——贴花着色器只需要
/// 单位盒端点的局部位置，法线 / UV 对投影无意义（专用几何不进
/// [`crate::mesh`] 顶点管线，省 20 字节/顶点）。
pub const PROXY_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: 12,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &[wgpu::VertexAttribute {
        offset: 0,
        shader_location: 0,
        format: wgpu::VertexFormat::Float32x3,
    }],
};

/// 代理几何端点（单位盒的 ±Z 端盖，坐标 ∈ [-0.5, 0.5]³）。
///
/// 前 4 点 = +Z 端盖（从 +Z 侧看 CCW，正面朝 +Z 外侧），后 4 点 = −Z
/// 端盖（从 −Z 侧看 CCW）——背面剔除（Cull Back）下相机在哪一侧就只见
/// 哪一侧的端盖，双侧贴花语义自然成立。不画侧面：侧面的 UV 会退化为
/// 纹理边缘 texel 拉伸（「边缘光晕」伪影，见模块文档）。
pub const PROXY_POSITIONS: [[f32; 3]; PROXY_VERTEX_COUNT] = [
    // +Z 端盖（法线 +Z）
    [-0.5, -0.5, 0.5],
    [0.5, -0.5, 0.5],
    [0.5, 0.5, 0.5],
    [-0.5, 0.5, 0.5],
    // -Z 端盖（法线 -Z，x 镜像使从 -Z 看为 CCW）
    [0.5, -0.5, -0.5],
    [-0.5, -0.5, -0.5],
    [-0.5, 0.5, -0.5],
    [0.5, 0.5, -0.5],
];

/// 代理几何索引（每端盖 [0,1,2, 0,2,3] 两三角形，环绕方向与
/// `mesh::primitives::add_face` 的 CCW 约定一致）。
pub const PROXY_INDICES: [u32; PROXY_INDEX_COUNT] = [
    0, 1, 2, 0, 2, 3, // +Z 端盖
    4, 5, 6, 4, 6, 7, // -Z 端盖
];

#[cfg(test)]
mod tests {
    use super::*;

    /// 布局断言：144 字节 / 实例，stride 与属性偏移一致（location 1..=9
    /// 每 16 字节一个 Float32x4）。
    #[test]
    fn instance_layout_is_144_bytes_with_nine_vec4_attributes() {
        assert_eq!(std::mem::size_of::<DecalInstanceRaw>(), 144);
        assert_eq!(DecalInstanceRaw::LAYOUT.array_stride, 144);
        assert_eq!(
            DecalInstanceRaw::LAYOUT.step_mode,
            wgpu::VertexStepMode::Instance
        );
        let attrs = DecalInstanceRaw::LAYOUT.attributes;
        assert_eq!(attrs.len(), 9, "9 × Float32x4 = 144B");
        for (i, attr) in attrs.iter().enumerate() {
            assert_eq!(attr.shader_location, (i + 1) as u32, "location 从 1 起");
            assert_eq!(attr.offset, (i * 16) as u64);
            assert_eq!(attr.format, wgpu::VertexFormat::Float32x4);
        }
    }

    /// 代理几何：坐标全在单位盒内、端盖 z = ±0.5、两盖法线朝外（首三角
    /// 形叉积 = 端盖法线）、CCW 环绕（与 Cull Back + FrontFace::Ccw 配套）。
    #[test]
    fn proxy_geometry_is_unit_box_caps_facing_outward() {
        for p in PROXY_POSITIONS {
            assert!(
                p.iter().all(|c| c.abs() <= 0.5),
                "代理顶点 {p:?} 应在单位盒内"
            );
        }
        // 前 4 点 z = +0.5，后 4 点 z = -0.5
        assert!(PROXY_POSITIONS[..4].iter().all(|p| p[2] == 0.5));
        assert!(PROXY_POSITIONS[4..].iter().all(|p| p[2] == -0.5));
        for (start, normal) in [(0usize, Vec3::Z), (4, Vec3::NEG_Z)] {
            let (a, b, c) = (
                Vec3::from(PROXY_POSITIONS[start]),
                Vec3::from(PROXY_POSITIONS[start + 1]),
                Vec3::from(PROXY_POSITIONS[start + 2]),
            );
            // CCW 环绕下 (b-a)×(c-a) 指向正面外侧
            let cross = (b - a).cross(c - a);
            assert!(
                cross.dot(normal) > 0.0,
                "端盖 {start} 首三角形叉积 {cross} 应朝 {normal}"
            );
        }
        assert_eq!(PROXY_INDICES.len(), PROXY_INDEX_COUNT);
        assert!(
            PROXY_INDICES
                .iter()
                .all(|&i| (i as usize) < PROXY_VERTEX_COUNT)
        );
    }

    /// from_model：inv_model 确为 model 之逆（复合 ≈ 恒等），tint 原样
    /// 透传——着色器「CPU 求逆」契约的数值验证。
    #[test]
    fn from_model_inverts_on_cpu_and_carries_tint() {
        let model = Mat4::from_scale_rotation_translation(
            Vec3::new(2.0, 3.0, 0.5),
            glam::Quat::from_rotation_y(1.1),
            Vec3::new(4.0, -1.0, 2.0),
        );
        let tint = [0.5, 1.0, 0.25, 0.8];
        let raw = DecalInstanceRaw::from_model(model, tint);
        let m = Mat4::from_cols_array_2d(&raw.model);
        let inv = Mat4::from_cols_array_2d(&raw.inv_model);
        let identity = m * inv;
        for r in 0..4 {
            for c in 0..4 {
                let want = if r == c { 1.0 } else { 0.0 };
                assert!(
                    (identity.to_cols_array_2d()[c][r] - want).abs() < 1e-5,
                    "model × inv_model 应为恒等，实际 {identity}"
                );
            }
        }
        assert_eq!(raw.tint, tint);
    }
}
