//! 硬件遮挡查询剔除（R53）：两阶段、延迟一帧、保守剔除。
//!
//! 语义规范全文见 ARCHITECTURE §4.25；本模块文档 = 数据通路 + 保守性论证
//! 与诚实限制清单。渲染器侧的 GPU 状态机在 `renderer.rs`（遮挡查询小节），
//! 本模块只携带**设置资源、代理几何数据与纯函数**（可脱离 World 与 GPU
//! 单测的部分全部提纯到此）。
//!
//! # 两阶段数据通路
//!
//! ```text
//! 帧 N：collect（视锥剔除后）→ 全部普通不透明实例的世界 AABB
//!     → 查询 pass（主 pass 之后、复用本帧不透明深度，Load 不写）
//!     → 每实例一个遮挡查询（画 36 索引盒，LessEqual 测试计通过样本数：
//!     等深必须通过——可见者与自己写的深度共面，见 pipeline.rs 注释）
//!     → resolve → copy → 异步读回（本帧提交后 map_async）
//! 帧 N+1：draw 开头 poll + try_recv 消费 → 样本数 = 0 的实体记入
//!     隐藏表 → 主 pass 只绘制未隐藏的实例（`occlusion_culled` 计数）
//! ```
//!
//! 读回是异步的，剔除决策天然**滞后一帧**（硬件查询剔除的标准做法）。
//! 消费到的结果必须来自上一帧的查询（帧号匹配），GPU 拥塞导致的迟到结果
//! 一律丢弃不剔除——宁可漏剔不误剔。隐藏表每帧重建，只查不画的对象
//! 持续被查询，恢复可见至多滞后一帧。
//!
//! **读回节拍**：`map_async` 回调经 `device.poll` 派发，且需要 GPU 真正
//! 退休该批提交（Vulkan 实测约需毫秒级真实时间）。60fps 事件循环的帧
//! 间隔天然满足；headless 工具若背靠背连跑 `draw`（零间隔），读回会
//! 迟到、功能自动退化为「全部可见」（保守）——测试/示例在帧间做阻塞
//! `read_pixels()` 模拟帧节拍。
//!
//! # 保守性论证（为什么像素可以不变）
//!
//! - 查询几何 = 实例世界 AABB（局部包围盒 8 角点过模型变换再取轴对齐
//!   并集，见 [`world_aabb`]）：仿射变换下几何 ⊆ 局部盒 ⇒ 旋转盒 ⊆
//!   变换角点并集 AABB ⊆ 查询盒。被查询盒**完全挡住**的实例，其全部
//!   采样深度测试必败 ⇒ 该实例对本帧颜色与深度的贡献恒为零；
//! - 遮挡查询语义是**保守下界**：WebGPU 规范允许硬件多报通过样本数、
//!   禁止少报。计数 = 0 ⇒ 没有任何样本通过深度测试 ⇒ 剔除它不改变
//!   任何像素（静止场景下逐位一致；相机/场景运动的滞后误差是声明的
//!   近似，见下）；
//! - 剔除只作用于**主 pass 绘制**：阴影 pass、点光阴影与场景深度副本
//!   仍按完整实例列表绘制——被遮挡者的投影与深度贡献保留，画面与
//!   关闭本功能逐位一致的保证因此不被阴影侧打破。
//!
//! # 缺席与回退语义
//!
//! [`OcclusionCulling`] 资源**缺席 = 关闭**（默认路径零开销、命令流与
//! 引入本功能前逐位一致）；`enabled = false` 等价。wgpu 30 起遮挡查询
//! 成为核心能力（见 [`crate::context::Gpu::supports_occlusion_query`]），「无特性平台
//! 全部可见」的回退由「读回不新鲜 ⇒ 本帧不剔除」的执行路径天然覆盖。
//!
//! # 诚实限制（v1）
//!
//! 1. **延迟一帧**：剔除决策基于上一帧深度与上一帧相机——相机快速
//!    移动时可能有一帧的漏画/多画（硬件查询剔除的固有近似，v1 接受）；
//! 2. **只剔普通不透明实例**：蒙皮 / 透明 / 贴花 / 粒子不参与查询，
//!    永不被遮挡剔除（蒙皮的保守并集界计算在收集期，接入门槛是每帧
//!    关节 AABB 的读回映射，留待后续）；
//! 3. **每实例一次 draw_indexed**：查询阶段无实例化合批（遮挡查询索引
//!    与 draw 一一对应），数千实例时查询 pass 的 draw call 开销可能
//!    超过收益——v1 按 4096 槽封顶（超出部分恒可见，保守方向）；
//! 4. **被剔实体不再遮挡更远的对象**（其深度不写进主 pass）：链式
//!    遮挡场景下一帧内可能把「只被被剔者挡住」的对象一并多留一帧
//!    或少画一帧——属 ①的滞后类别，静止场景无此问题；
//! 5. **AABB 不是逐像素代理**：细长/凹形几何的包围盒空隙让它长期
//!    「未被完全遮挡」——只漏剔，不误剔；
//! 6. **RT 相机不参与**：离屏视口各自视角不同，共享一份隐藏表会互相
//!    误剔（v1 只对主相机生效）。

use glam::{Mat4, Vec3};

/// 遮挡剔除开关。作为 `World` 资源存在，**缺席 = 关闭**（与
/// `VignetteSettings` / `DepthOfFieldSettings` 同款「缺席零开销」语义：
/// 缺席帧的命令流与引入本功能前逐位一致——全部既有 GPU 测试的兼容
/// 硬约束即锚在此处）。
///
/// 开启语义见模块文档：查询阶段每帧对「视锥内全部普通不透明实例」
/// 画 AABB 代理，读回结果延迟一帧裁决主 pass 绘制。两帧的相机 / 场景
/// 剧变可能引入一帧的滞后误差（声明的近似）。
#[derive(Clone, Copy, Debug, Default, iris_core::Resource)]
pub struct OcclusionCulling {
    /// 是否启用遮挡剔除。
    pub enabled: bool,
}

impl OcclusionCulling {
    /// 开启遮挡剔除的便捷构造（`world.insert_resource(OcclusionCulling::ENABLED)`）。
    pub const ENABLED: Self = Self { enabled: true };
}

/// 单位立方体角点（±0.5，8 顶点）。查询 pass 的顶点缓冲：顶点着色器把
/// 角点分量 `+0.5` 映射为 `[0,1]` 插值权重，与逐实例的 `min`/`max` 做
/// `mix` 展开成实例世界 AABB 的角点（见 `shaders/occlusion.wgsl`）。
pub(crate) const AABB_CORNERS: [[f32; 3]; 8] = [
    [-0.5, -0.5, -0.5],
    [0.5, -0.5, -0.5],
    [0.5, 0.5, -0.5],
    [-0.5, 0.5, -0.5],
    [-0.5, -0.5, 0.5],
    [0.5, -0.5, 0.5],
    [0.5, 0.5, 0.5],
    [-0.5, 0.5, 0.5],
];

/// 单位立方体 12 三角形索引（36 索引，绕向无所谓——查询管线 `cull None`，
/// 背面也参与深度计数是保守方向：宁可多报可见）。
pub(crate) const AABB_INDICES: [u16; 36] = [
    // −Z 面
    0, 1, 2, 0, 2, 3, // +Z 面
    4, 6, 5, 4, 7, 6, // −Y 面
    0, 5, 1, 0, 4, 5, // +Y 面
    3, 2, 6, 3, 6, 7, // −X 面
    0, 3, 7, 0, 7, 4, // +X 面
    1, 5, 6, 1, 6, 2,
];

/// 索引数（`draw_indexed(0..AABB_INDEX_COUNT, …)` 常量口径）。
pub(crate) const AABB_INDEX_COUNT: u32 = 36;

/// 角点顶点布局（location 0，`[[f32;3];8]` 直传）。
pub(crate) const AABB_CORNER_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: std::mem::size_of::<[f32; 3]>() as wgpu::BufferAddress,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &[wgpu::VertexAttribute {
        offset: 0,
        shader_location: 0,
        format: wgpu::VertexFormat::Float32x3,
    }],
};

/// 查询 pass 的逐实例数据（32 字节，POD）：世界 AABB 的 min/max，
/// 各占 vec4 槽（xyz 有效、w 保留 0——对齐 WGSL uniform/vertex 惯用形）。
#[repr(C)]
#[derive(Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct AabbInstanceRaw {
    pub min: [f32; 4],
    pub max: [f32; 4],
}

impl AabbInstanceRaw {
    /// 实例属性 location 1/2（与角点的 location 0 错开）。
    pub(crate) const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
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
        ],
    };
}

/// 局部 AABB 过模型矩阵的**保守世界 AABB**：8 个局部角点变换后取轴对齐
/// 并集。旋转后的几何落在旋转盒内，而变换角点的 AABB 正是旋转盒的轴对齐
/// 外包络 ⇒ 世界 AABB ⊇ 几何（只可能多留、不可能误剔，见模块文档的
/// 保守性论证）。与 `skin_world_aabb` 同款角点枚举（无蒙皮关节维）。
pub(crate) fn world_aabb(min: Vec3, max: Vec3, model: &Mat4) -> (Vec3, Vec3) {
    let mut world_min = Vec3::splat(f32::INFINITY);
    let mut world_max = Vec3::splat(f32::NEG_INFINITY);
    for corner in 0..8u32 {
        let local = Vec3::new(
            if corner & 1 != 0 { max.x } else { min.x },
            if corner & 2 != 0 { max.y } else { min.y },
            if corner & 4 != 0 { max.z } else { min.z },
        );
        let world = model.transform_point3(local);
        world_min = world_min.min(world);
        world_max = world_max.max(world);
    }
    (world_min, world_max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_model_is_exact() {
        let (min, max) = world_aabb(
            Vec3::new(-1.0, 0.0, 2.0),
            Vec3::new(1.0, 3.0, 4.0),
            &Mat4::IDENTITY,
        );
        assert_eq!(min, Vec3::new(-1.0, 0.0, 2.0));
        assert_eq!(max, Vec3::new(1.0, 3.0, 4.0));
    }

    /// 旋转 90°（绕 Z）：局部盒 (−1,0,2)..(1,3,4) 的角点集恰为
    /// (−3,−1,2)..(0,1,4)——轴对齐盒绕轴 90° 仍是精确的轴对齐盒
    /// （`from_rotation_z` 的 sin/cos 有 ~1e-8 浮点噪声，容差 ±1e-5）。
    #[test]
    fn quarter_turn_z_is_conformal() {
        let (min, max) = world_aabb(
            Vec3::new(-1.0, 0.0, 2.0),
            Vec3::new(1.0, 3.0, 4.0),
            &Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2),
        );
        let expect = |got: Vec3, want: Vec3| {
            assert!(
                (got - want).length() <= 1e-5,
                "旋转 90° 后应为 {want:?}，实际 {got:?}"
            );
        };
        expect(min, Vec3::new(-3.0, -1.0, 2.0));
        expect(max, Vec3::new(0.0, 1.0, 4.0));
    }

    /// 任意角 + 缩放 + 平移：世界 AABB 必须包含局部盒的 6 个面中心
    /// （盒内代表点）——保守性抽查（绕 Y 0.7 rad + 非均匀缩放 + 平移）。
    #[test]
    fn oblique_transform_contains_face_centers() {
        let min = Vec3::new(-2.0, -1.0, 0.5);
        let max = Vec3::new(2.0, 1.0, 3.5);
        let model = Mat4::from_rotation_y(0.7)
            * Mat4::from_scale(Vec3::new(1.5, 0.5, 2.0))
            * Mat4::from_translation(Vec3::new(3.0, -1.0, 0.5));
        let (lo, hi) = world_aabb(min, max, &model);
        let center = (min + max) * 0.5;
        let half = (max - min) * 0.5;
        for axis in 0..3 {
            for sign in [-1.0, 1.0] {
                let mut local_face = center;
                local_face[axis] += sign * half[axis];
                let world = model.transform_point3(local_face);
                assert!(
                    (world - lo).min(Vec3::ZERO) == Vec3::ZERO
                        && (hi - world).min(Vec3::ZERO) == Vec3::ZERO,
                    "面中心 {local_face} 的世界位置 {world} 必须落在保守盒 {lo:?}..{hi:?} 内"
                );
            }
        }
    }

    #[test]
    fn aabb_proxy_tables_are_consistent() {
        assert_eq!(AABB_INDICES.len() as u32, AABB_INDEX_COUNT);
        // 角点表恰 8 项且各分量为 ±0.5（mix 权重 0/1 的前提）
        for corner in AABB_CORNERS.iter() {
            for v in corner {
                assert!(v.abs() == 0.5, "角点分量必须恰为 ±0.5，实际 {v}");
            }
        }
        // 索引全部落在角点表内
        for i in AABB_INDICES.iter() {
            assert!((*i as usize) < AABB_CORNERS.len());
        }
    }
}
