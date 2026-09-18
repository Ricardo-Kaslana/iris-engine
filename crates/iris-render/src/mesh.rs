//! 网格：CPU 数据目录 + 基础图元生成器 + 距离 LOD 组件。

use glam::Vec3;
use iris_core::Resource;
use wgpu::util::DeviceExt;

/// 网格句柄。指向 `MeshStore` 中的条目，也作为 ECS 组件挂载到实体上。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, iris_core::Component)]
pub struct MeshHandle(pub u32);

/// 距离 LOD 组件（R30）：同一实体的高模 / 低模网格对 + 切换距离。
///
/// 挂载实体时**替换** `MeshHandle` 参与渲染收集（本组件自带两个句柄，
/// 不与 `MeshHandle` 并存）；收集期按「实体原点到相机 eye 的欧氏距离」
/// 选择：`dist <= switch_distance` 用 `high`，否则 `low`——选中的句柄
/// 参与合批排序键、视锥剔除、透明排序与 `triangles_drawn` 统计，对
/// 下游完全透明。v1 距离用**实体原点**而非包围球中心（省每帧包围盒
/// 展开；对称网格原点即中心，偏心网格远处剔除边界略保守，误差可接受）。
///
/// v1 已知限制（诚实清单）：
/// - ~~**无滞回**（hysteresis）~~ **C1 已闭合**：世界带 [`LodSettings`]
///   资源时主相机收集走双阈值滞回（[`LodMesh::select_hysteresis`]），
///   资源缺席 = 本节原文语义逐位不变；
/// - 高低模一般共享材质（切换只换几何）；两句柄指向不同材质时切换帧
///   批次重排是合法行为，但可能触发额外 draw call。
#[derive(Clone, Copy, Debug, PartialEq, iris_core::Component)]
pub struct LodMesh {
    /// 近距网格（`dist <= switch_distance` 时使用）。
    pub high: MeshHandle,
    /// 远距网格（`dist > switch_distance` 时使用）。
    pub low: MeshHandle,
    /// 切换距离（米，实体原点到相机 eye 的欧氏距离）。
    pub switch_distance: f32,
}

impl LodMesh {
    /// 距离选择纯函数：`eye` 为观察点（相机眼点），`origin` 为实体原点
    /// （GlobalTransform 平移）。距离恰等于 `switch_distance` 时用 high
    /// （边界归近侧——近处宁可用高模）。
    pub fn select(&self, eye: Vec3, origin: Vec3) -> MeshHandle {
        if eye.distance(origin) <= self.switch_distance {
            self.high
        } else {
            self.low
        }
    }

    /// LOD v2 选择纯函数（C1 · 闭合 §8.16「无滞回」条）：**双阈值滞回**。
    ///
    /// 口径（`h =` 入参 `hysteresis` 消毒后 ∈ [0, 0.999]；非有限输入回 0）：
    /// - `prev_high = Some(true)`：留在 high 当且仅当 `dist <= s·(1+h)`
    ///   （越出外阈值才降 low）；
    /// - `prev_high = Some(false)`：升 high 当且仅当 `dist <= s·(1−h)`
    ///   （必须进入内阈值——滞回「带」内维持原档，两阈值之间无跳变）；
    /// - `prev_high = None`（无历史：首帧 / 新实体 / 无记忆上下文）：
    ///   `dist <= s`，与 [`LodMesh::select`] 同式。
    ///
    /// **`h = 0` 时三分支全部退化为 `dist <= s`，与 v1 `select` 逐位一致**
    /// （滞回带零宽 ⇒ 记忆不参与判定）——这是「新设置默认无害」的兼容
    /// 论证，由单测逐位钉住。边界归近侧口径在全部 prev 形态保持。
    /// 返回（选中句柄，是否 high）——布尔供调用侧写回跨帧记忆。
    pub fn select_hysteresis(
        &self,
        dist: f32,
        prev_high: Option<bool>,
        hysteresis: f32,
    ) -> (MeshHandle, bool) {
        let s = self.switch_distance;
        let h = if hysteresis.is_finite() {
            hysteresis.clamp(0.0, 0.999)
        } else {
            0.0
        };
        let high = match prev_high {
            Some(true) => dist <= s * (1.0 + h),
            Some(false) => dist <= s * (1.0 - h),
            None => dist <= s,
        };
        (if high { self.high } else { self.low }, high)
    }
}

/// LOD v2 选择参数（C1 · 闭合 §8.16）：World 资源，**缺席 = v1 口径
/// 逐位不变**（照 `TaaSettings` / `SsrSettings` 的缺席语义范式）。
///
/// 生效范围（诚实边界）：只作用于 [`LodMesh`] 的**主相机**收集——
/// 滞回是跨帧记忆，而每帧存在多个收集上下文（RT 相机 / 探针捕获 /
/// 水体捕获，各自视锥独立），记忆按相机分表会放大状态面且语义可疑
/// （辅助相机的历史不该污染主视图），故辅助上下文一律维持当帧纯距离
/// 判定（v1 式）；地形 [`LodMesh3`](crate::terrain::LodMesh3) 的三级
/// 选择不在本轮范围（chunk 有滞回需求的场景请走实体级 LodMesh）。
/// `Renderer::reset_taa` 不清本记忆（与探针域同立场：相机无关的场景
/// 侧时域状态）。
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct LodSettings {
    /// 滞回带相对宽度，消毒后钳 [0, 0.999]（非有限输入回 0 = 单阈值）。
    /// 0.15 ≈ 切换点两侧各 15% 的缓冲带，可吸收边界抖动的典型量级。
    pub hysteresis: f32,
    /// 距离参照 = **high 网格包围球中心**（世界系，经模型矩阵变换）
    /// 而非实体原点。偏心网格（原点不在几何中部）的切换边界更准确；
    /// 网格无 CPU 包围盒时回退实体原点（v1 口径，保守方向）。
    /// `false` = 实体原点（v1 口径）。
    pub bounds_center: bool,
}

impl Default for LodSettings {
    /// 默认「开」：资源**在场**即是作者的启用声明（缺席才是关闭）。
    fn default() -> Self {
        Self {
            hysteresis: 0.15,
            bounds_center: true,
        }
    }
}

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
            wgpu::VertexAttribute {
                offset: 0,
                shader_location: 0,
                format: wgpu::VertexFormat::Float32x3,
            },
            wgpu::VertexAttribute {
                offset: 12,
                shader_location: 1,
                format: wgpu::VertexFormat::Float32x3,
            },
            wgpu::VertexAttribute {
                offset: 24,
                shader_location: 2,
                format: wgpu::VertexFormat::Float32x2,
            },
        ],
    };
}

/// 蒙皮顶点属性：4 个关节索引 + 4 个混合权重（16 字节，POD）。
///
/// 与主顶点数据分离为独立缓冲：非蒙皮网格不为蒙皮属性付出 16 字节/顶点。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SkinVertex {
    /// 关节索引（指向 `Skin::joints`，进而指向关节矩阵缓冲）。
    pub joints: [u32; 4],
    /// 混合权重（约定和为 1；加载时归一化）。
    pub weights: [f32; 4],
}

impl SkinVertex {
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<SkinVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[
            wgpu::VertexAttribute {
                offset: 0,
                shader_location: 10,
                format: wgpu::VertexFormat::Uint32x4,
            },
            wgpu::VertexAttribute {
                offset: 16,
                shader_location: 11,
                format: wgpu::VertexFormat::Float32x4,
            },
        ],
    };
}

/// 顶点切线缓冲布局（R42）：每顶点 16 字节 vec4（xyz = 切线方向，
/// w = 手性符号 ±1）。独立顶点缓冲——与蒙皮属性同理，无切线的网格
/// 不为切线付出 16 B/顶点，渲染器垫常驻零缓冲（零向量 = 「无顶点
/// 切线」标记，着色器据此走屏幕空间 TBN 路径，见 standard.wgsl）。
/// shader_location 取 12：3–9 被实例数据占用、10–11 被蒙皮占用。
pub const TANGENT_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: 16,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &[wgpu::VertexAttribute {
        offset: 0,
        shader_location: 12,
        format: wgpu::VertexFormat::Float32x4,
    }],
};

/// 光照贴图 UV（UV2）顶点缓冲布局（R52c）：每顶点 8 字节 `vec2`，值是
/// **图集归一化坐标**（0..1，v 与 texel 行序一致——行 0 = v 0，与
/// [`bake_uv`](crate::bake_uv) 的 `uv_rect` 同口径），着色器直接采样、
/// 零常量变换。独立顶点缓冲（与蒙皮 / 切线同理）：无光照贴图的网格不为
/// UV2 付出 8 B/顶点，渲染器垫常驻零缓冲（全零 = 「无光照贴图 UV」，
/// 材质侧的 `lightmap_mask` 恒 0 ⇒ 采样结果被丢弃，见 `standard.wgsl`）。
/// shader_location 取 13：0–2 主顶点、3–9 实例、10–11 蒙皮、12 切线。
pub const UV2_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: 8,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &[wgpu::VertexAttribute {
        offset: 0,
        shader_location: 13,
        format: wgpu::VertexFormat::Float32x2,
    }],
};

/// CPU 侧网格数据（顶点 + 索引 + 可选蒙皮 / 切线 / 光照贴图 UV / 形变目标属性）。
///
/// 组件缺席语义：`skin` 缺席 = 非蒙皮网格；`tangents` 缺席 = 网格无
/// 顶点切线——法线贴图扰动所需的 TBN 由着色器在屏幕空间用
/// `dpdx/dpdy` 推导（`standard.wgsl`），两种路径对无切线消费的材质
/// （法线槽位缺省）渲染结果零影响；`uv2` 缺席 = 网格无光照贴图 UV——
/// 渲染器绑常驻零垫，配合材质侧 `lightmap_mask = 0`，间接光走原 IBL
/// 路径**逐位不变**（R52c 硬约束，论证见 `lightmap.rs` 模块文档）；
/// `morph` 缺席 = 网格无形变目标（权重组件在场也无效）——Some 时
/// `vertices` 恒为绑定姿势，形变顶点是渲染期按权重混合的临时产物，
/// 不回写本表（C4a，见 `morph.rs` 模块文档）。
#[derive(Clone, Debug, Default)]
pub struct MeshData {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    /// 蒙皮属性；Some 时长度必须与 `vertices` 一致。
    pub skin: Option<Vec<SkinVertex>>,
    /// 顶点切线（R42，glTF TANGENT 语义）：xyz = 切线方向（网格局部
    /// 空间，着色器用法线矩阵变换到世界空间），w = 手性符号（±1，
    /// 副切线 = cross(N, T)·w）；Some 时长度必须与 `vertices` 一致。
    pub tangents: Option<Vec<[f32; 4]>>,
    /// 光照贴图 UV（R52c，图集归一化坐标 0..1）；Some 时长度必须与
    /// `vertices` 一致。**烘焙产物、不进 glTF / 场景序列化**：运行时由
    /// [`install_lightmap`](crate::lightmap::install_lightmap) 按实体注入
    /// （每实体一份网格副本——UV2 是逐实体的图集坐标，共享网格必须分裂，
    /// 见该函数文档）。
    pub uv2: Option<Vec<[f32; 2]>>,
    /// 形变目标表（C4a，glTF morphTargets 语义）：`vertices` 保持绑定
    /// 姿势，增量在 [`morph`](crate::morph) 侧表；Some 时各目标增量长度
    /// 必须与 `vertices` 一致。**渲染期只对蒙皮实体生效**——逐实体权重
    /// [`MorphWeights`](crate::morph::MorphWeights) 在场时 CPU 混合上传
    /// 专用顶点缓冲（缺席 = 绑定姿势 = 原路径逐位不变，口径见模块文档）。
    pub morph: Option<crate::morph::MorphData>,
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

    /// 圆柱体。见 [`primitives::cylinder`]。
    pub fn cylinder(radius: f32, height: f32, segments: u32) -> Self {
        primitives::cylinder(radius, height, segments)
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
            [
                center[0] - u[0] - v[0],
                center[1] - u[1] - v[1],
                center[2] - u[2] - v[2],
            ],
            [
                center[0] + u[0] - v[0],
                center[1] + u[1] - v[1],
                center[2] + u[2] - v[2],
            ],
            [
                center[0] + u[0] + v[0],
                center[1] + u[1] + v[1],
                center[2] + u[2] + v[2],
            ],
            [
                center[0] - u[0] + v[0],
                center[1] - u[1] + v[1],
                center[2] - u[2] + v[2],
            ],
        ];
        let uvs = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        for (c, uv) in corners.into_iter().zip(uvs) {
            data.vertices.push(Vertex {
                position: c,
                normal,
                uv,
            });
        }
        data.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    /// 单位立方体（边长 1，中心在原点）：24 顶点 / 36 索引。
    pub fn cube() -> MeshData {
        let mut data = MeshData::default();
        let h = 0.5;
        // +Z / -Z
        add_face(
            &mut data,
            [0.0, 0.0, h],
            [0.0, 0.0, 1.0],
            [h, 0.0, 0.0],
            [0.0, h, 0.0],
        );
        add_face(
            &mut data,
            [0.0, 0.0, -h],
            [0.0, 0.0, -1.0],
            [-h, 0.0, 0.0],
            [0.0, h, 0.0],
        );
        // +X / -X
        add_face(
            &mut data,
            [h, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, -h],
            [0.0, h, 0.0],
        );
        add_face(
            &mut data,
            [-h, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 0.0, h],
            [0.0, h, 0.0],
        );
        // +Y / -Y
        add_face(
            &mut data,
            [0.0, h, 0.0],
            [0.0, 1.0, 0.0],
            [h, 0.0, 0.0],
            [0.0, 0.0, -h],
        );
        add_face(
            &mut data,
            [0.0, -h, 0.0],
            [0.0, -1.0, 0.0],
            [h, 0.0, 0.0],
            [0.0, 0.0, h],
        );
        data
    }

    /// XZ 平面上的方形地面（法线 +Y）。
    pub fn plane(size: f32) -> MeshData {
        let mut data = MeshData::default();
        let h = size * 0.5;
        add_face(
            &mut data,
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [h, 0.0, 0.0],
            [0.0, 0.0, -h],
        );
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
                //
                // 极点带（ring 0 / rings−1）只发**一个**三角：极点环的所有
                // 顶点位置重合（φ = 0 / π），quad 剖分的另一半（a,b,d /
                // a,d,c）是零面积退化三角——渲染上无贡献（零面积永不
                // 光栅化），但烘焙 BVH 的「退化 = 上游数据 bug」断言会拒绝
                // 整个场景（R58b 编辑器烘焙入口实测踩坑）。三角扇 = 极点
                // 带的正确拓扑。
                if ring == 0 {
                    data.indices.extend_from_slice(&[a, d, c]);
                } else if ring == rings - 1 {
                    data.indices.extend_from_slice(&[a, b, d]);
                } else {
                    data.indices.extend_from_slice(&[a, d, c, a, b, d]);
                }
            }
        }
        data
    }

    /// 圆柱体（中心在原点，轴沿 +Y）：侧面 quad 环 + 上下圆帽（三角扇）。
    ///
    /// 侧面法线径向朝外、帽面法线 ±Y；环绕方向与 `add_face` / `uv_sphere`
    /// 的约定一致（从外侧看 CCW，正面朝外）。首尾重复顶点闭合侧环（UV
    /// 无缝），帽面独立携带 ±Y 法线的顶点（与侧面的径向法线分离，
    /// 顶点不共享——受光方向不同，共享会产生插值瑕疵）。
    pub fn cylinder(radius: f32, height: f32, segments: u32) -> MeshData {
        assert!(segments >= 3, "圆柱分段数过低");
        let mut data = MeshData::default();
        let half = height * 0.5;

        // ---- 侧面：两行顶点（0 = 底 -Y，1 = 顶 +Y）× segments 个 quad ----
        for ring in 0..=1u32 {
            let y = if ring == 0 { -half } else { half };
            for seg in 0..=segments {
                let theta = std::f32::consts::TAU * seg as f32 / segments as f32;
                let (sin_theta, cos_theta) = theta.sin_cos();
                let normal = [cos_theta, 0.0, sin_theta];
                data.vertices.push(Vertex {
                    position: [cos_theta * radius, y, sin_theta * radius],
                    normal,
                    uv: [seg as f32 / segments as f32, ring as f32],
                });
            }
        }
        let stride = segments + 1;
        for seg in 0..segments {
            let a = seg; // 底环当前
            let b = a + 1; // 底环下一点（θ 增大）
            let c = a + stride; // 顶环当前
            let d = c + 1; // 顶环下一点
            // 从外侧看 CCW（正面朝外）：a→c 沿 +Y 上升、c→d 沿 θ 增大方向，
            // (c−a)×(d−c) 指向外侧（环绕方向教训见 uv_sphere 的索引注释）
            data.indices.extend_from_slice(&[a, c, d, a, d, b]);
        }

        // ---- 圆帽：中心 + 闭合环（法线 ±Y），三角扇 ----
        let cap_rim = |data: &mut MeshData, y: f32| {
            let base = data.vertices.len() as u32;
            for seg in 0..=segments {
                let theta = std::f32::consts::TAU * seg as f32 / segments as f32;
                let (sin_theta, cos_theta) = theta.sin_cos();
                data.vertices.push(Vertex {
                    position: [cos_theta * radius, y, sin_theta * radius],
                    normal: [0.0, y.signum(), 0.0],
                    uv: [cos_theta * 0.5 + 0.5, sin_theta * 0.5 + 0.5],
                });
            }
            base
        };
        // 顶帽（+Y）：中心在轴上，扇面从上方看须为 CCW——θ 增大方向自上方
        // 看是顺时针，故索引按 (中心, seg+1, seg) 逆排
        let top_center = data.vertices.len() as u32;
        data.vertices.push(Vertex {
            position: [0.0, half, 0.0],
            normal: [0.0, 1.0, 0.0],
            uv: [0.5, 0.5],
        });
        let top_base = cap_rim(&mut data, half);
        // 底帽（-Y）：θ 增大方向自下方看恰为 CCW，索引按 (中心, seg, seg+1)
        let bottom_center = data.vertices.len() as u32;
        data.vertices.push(Vertex {
            position: [0.0, -half, 0.0],
            normal: [0.0, -1.0, 0.0],
            uv: [0.5, 0.5],
        });
        let bottom_base = cap_rim(&mut data, -half);
        for seg in 0..segments {
            let t0 = top_base + seg;
            let t1 = t0 + 1;
            data.indices.extend_from_slice(&[top_center, t1, t0]);
            let b0 = bottom_base + seg;
            let b1 = b0 + 1;
            data.indices.extend_from_slice(&[bottom_center, b0, b1]);
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

    /// 目录内容只读视图（下标 = 句柄号）。
    pub fn contents(&self) -> &[MeshData] {
        &self.meshes
    }

    /// 整体替换目录内容（下标 = 句柄号重新按序分配）。
    ///
    /// **安全前提在调用方**：替换后，旧内容中未保留的句柄不得再被任何
    /// 实体引用（当前唯一消费者是 iris-editor 的 undo 快照恢复——快照存
    /// 整库克隆、恢复先换库再重建实体，前提天然成立；对比截断式恢复的
    /// 论证：整库克隆同时是 redo 可逆的前提——截断丢数据无法 redo）。
    pub fn set_contents(&mut self, meshes: Vec<MeshData>) {
        self.meshes = meshes;
    }

    /// CPU 源内存占用（字节）：Σ（顶点 × 32B + 索引 × 4B + 蒙皮属性 ×
    /// 32B + 切线 × 16B + 光照贴图 UV × 8B）（不含 Vec 分配器开销）。
    /// 「低占用」卖点的观测口径之一；GPU 侧顶点 / 索引缓冲与源同尺寸
    /// （蒙皮 / 切线 / UV2 缓冲同），故该值亦即 GPU 网格占用的准确上界
    /// 估算（切线与 UV2 缺席的网格只绑定渲染器常驻的零填充垫缓冲，
    /// 无逐网格开销）。
    pub fn memory_bytes(&self) -> u64 {
        self.meshes
            .iter()
            .map(|data| {
                data.vertices.len() as u64 * std::mem::size_of::<Vertex>() as u64
                    + data.indices.len() as u64 * std::mem::size_of::<u32>() as u64
                    + data.skin.as_ref().map_or(0, |s| {
                        s.len() as u64 * std::mem::size_of::<SkinVertex>() as u64
                    })
                    + data.tangents.as_ref().map_or(0, |t| (t.len() * 16) as u64)
                    + data.uv2.as_ref().map_or(0, |u| (u.len() * 8) as u64)
                    + data.morph.as_ref().map_or(0, |m| m.memory_bytes())
            })
            .sum()
    }
}

/// 便捷上传：由渲染器调用，将 `MeshData` 变为 GPU 缓冲。
/// 蒙皮属性存在时附带第三张顶点缓冲；切线存在时附带切线顶点缓冲；
/// 光照贴图 UV（R52c）存在时附带 UV2 顶点缓冲（`None` 均由渲染器绑定
/// 常驻零垫——切线零向量 = 「无顶点切线」标记走屏幕空间 TBN 路径，
/// UV2 零值配合材质 `lightmap_mask = 0` 走原 IBL 路径）。
pub(crate) fn upload_mesh(
    gpu: &crate::context::Gpu,
    data: &MeshData,
) -> (
    wgpu::Buffer,
    wgpu::Buffer,
    u32,
    Option<wgpu::Buffer>,
    Option<wgpu::Buffer>,
    Option<wgpu::Buffer>,
) {
    if let Some(skin) = &data.skin {
        assert_eq!(
            skin.len(),
            data.vertices.len(),
            "蒙皮属性数量必须与顶点数量一致"
        );
    }
    if let Some(tangents) = &data.tangents {
        assert_eq!(
            tangents.len(),
            data.vertices.len(),
            "切线数量必须与顶点数量一致"
        );
    }
    if let Some(uv2) = &data.uv2 {
        assert_eq!(
            uv2.len(),
            data.vertices.len(),
            "光照贴图 UV 数量必须与顶点数量一致"
        );
    }
    let vertex_buf = gpu
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("iris.mesh.vbo"),
            contents: bytemuck::cast_slice(&data.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
    let index_buf = gpu
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("iris.mesh.ibo"),
            contents: bytemuck::cast_slice(&data.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
    let skin_buf = data.skin.as_ref().map(|skin| {
        gpu.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("iris.mesh.skin_vbo"),
                contents: bytemuck::cast_slice(skin),
                usage: wgpu::BufferUsages::VERTEX,
            })
    });
    let tangent_buf = data.tangents.as_ref().map(|tangents| {
        gpu.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("iris.mesh.tangent_vbo"),
                contents: bytemuck::cast_slice(tangents),
                usage: wgpu::BufferUsages::VERTEX,
            })
    });
    let uv2_buf = data.uv2.as_ref().map(|uv2| {
        gpu.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("iris.mesh.uv2_vbo"),
                contents: bytemuck::cast_slice(uv2),
                usage: wgpu::BufferUsages::VERTEX,
            })
    });
    (
        vertex_buf,
        index_buf,
        data.indices.len() as u32,
        skin_buf,
        tangent_buf,
        uv2_buf,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// LodMesh 距离选择纯函数（R30）：边界归近侧（≤ 用 high）、远离用
    /// low；与观察方向无关（纯欧氏距离）。
    #[test]
    fn lod_select_picks_by_distance_with_near_boundary() {
        let lod = LodMesh {
            high: MeshHandle(7),
            low: MeshHandle(3),
            switch_distance: 10.0,
        };
        let origin = Vec3::new(2.0, 1.0, 2.0); // 到原点距离 = 3
        assert_eq!(
            lod.select(Vec3::ZERO, origin),
            MeshHandle(7),
            "3 ≤ 10 → high"
        );
        assert_eq!(
            lod.select(Vec3::new(2.0, 1.0, 11.0), origin),
            MeshHandle(7),
            "距离 9 ≤ 10 → high"
        );
        // 恰在边界（距离 = switch_distance）归近侧
        let at_edge = Vec3::new(2.0, 1.0, 12.0); // 距离恰 10
        assert_eq!(lod.select(at_edge, origin), MeshHandle(7), "边界归近侧");
        // 超界 0.001 m 用 low
        let past_edge = Vec3::new(2.0, 1.0, 13.0); // 距离 11 > 10
        assert_eq!(lod.select(past_edge, origin), MeshHandle(3), "超界用 low");
    }

    /// LOD v2 兼容锚（C1）：**h = 0 时三档 prev 与 v1 `select` 逐位一致**
    /// ——滞回带零宽 ⇒ 跨帧记忆不参与判定 ⇒ 「在场但保守」的新设置无害。
    #[test]
    fn lod_select_hysteresis_zero_band_equals_v1_bitwise() {
        let lod = LodMesh {
            high: MeshHandle(7),
            low: MeshHandle(3),
            switch_distance: 10.0,
        };
        for dist in [0.0f32, 9.9, 10.0, 10.000_001, 11.0, 1e9, f32::NAN] {
            let v1 = lod.select(Vec3::ZERO, Vec3::new(0.0, 0.0, dist));
            for prev in [None, Some(true), Some(false)] {
                let (got, high) = lod.select_hysteresis(dist, prev, 0.0);
                assert_eq!(
                    (got, high),
                    (v1, v1 == MeshHandle(7)),
                    "dist = {dist}, prev = {prev:?}：h=0 必须逐位等于 v1"
                );
            }
        }
    }

    /// LOD v2 滞回有效性（C1 · 非默认值断言，R89 规约）：s = 10、h = 0.2
    /// ⇒ 外阈值 12 / 内阈值 8。带内（8, 12）维持原档——同一距离因 prev
    /// 不同得到不同档位，这正是防边界抖动的机制；出带即切换，边界归近侧。
    #[test]
    fn lod_select_hysteresis_band_holds_previous_level() {
        let lod = LodMesh {
            high: MeshHandle(7),
            low: MeshHandle(3),
            switch_distance: 10.0,
        };
        // prev=high：11 在带内 → 维持 high；13 越外阈值 → low；恰 12 归近侧
        assert_eq!(
            lod.select_hysteresis(11.0, Some(true), 0.2),
            (MeshHandle(7), true)
        );
        assert_eq!(
            lod.select_hysteresis(12.0, Some(true), 0.2),
            (MeshHandle(7), true)
        );
        assert_eq!(
            lod.select_hysteresis(13.0, Some(true), 0.2),
            (MeshHandle(3), false)
        );
        // prev=low：9 在带内 → 维持 low；7 进内阈值 → high；恰 8 归近侧
        assert_eq!(
            lod.select_hysteresis(9.0, Some(false), 0.2),
            (MeshHandle(3), false)
        );
        assert_eq!(
            lod.select_hysteresis(8.0, Some(false), 0.2),
            (MeshHandle(7), true)
        );
        assert_eq!(
            lod.select_hysteresis(7.0, Some(false), 0.2),
            (MeshHandle(7), true)
        );
        // 无历史（首帧/新实体）：单阈值 s=10
        assert_eq!(
            lod.select_hysteresis(11.0, None, 0.2),
            (MeshHandle(3), false)
        );
        assert_eq!(
            lod.select_hysteresis(10.0, None, 0.2),
            (MeshHandle(7), true)
        );
        // 消毒：负值钳 0（= v1 单阈值）、NaN/inf 回 0、1.0 钳 0.999
        assert_eq!(
            lod.select_hysteresis(10.0, Some(false), -3.0),
            (MeshHandle(7), true)
        );
        assert_eq!(
            lod.select_hysteresis(10.0, Some(false), f32::NAN),
            (MeshHandle(7), true)
        );
        assert_eq!(
            lod.select_hysteresis(10.0, Some(false), f32::INFINITY),
            (MeshHandle(7), true)
        );
        let (h, _) = lod.select_hysteresis(10.9, Some(true), 1.0); // 内阈值 ≈0.01×10 → 10.9 带外?
        assert_eq!(
            h,
            MeshHandle(7),
            "prev=high 且 h 钳 0.999 ⇒ 外阈值 19.99，10.9 维持 high"
        );
    }

    /// 内存统计（R30）：顶点 32B + 索引 4B + 蒙皮属性 32B 逐网格累加。
    #[test]
    fn mesh_store_memory_bytes_sums_buffers() {
        let mut store = MeshStore::new();
        store.add(primitives::cube()); // 24 顶点 / 36 索引
        assert_eq!(store.memory_bytes(), (24 * 32 + 36 * 4) as u64);
        let mut skinned = primitives::cube();
        skinned.skin = Some(vec![
            SkinVertex {
                joints: [0; 4],
                weights: [0.0; 4],
            };
            24
        ]);
        store.add(skinned);
        assert_eq!(
            store.memory_bytes(),
            2 * (24 * 32 + 36 * 4) as u64 + 24 * 32,
            "蒙皮属性字节必须计入"
        );
    }

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
        assert!(
            cube.indices
                .iter()
                .all(|&i| (i as usize) < cube.vertices.len())
        );
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
        // R58b：极点带改三角扇（消除极点零面积退化三角）——
        // 索引数 = 中间带 (rings−2) × segments × 6 + 上下极点扇 2 × segments × 3
        let (segments, rings) = (16u32, 12u32);
        assert_eq!(
            sphere.indices.len() as u32,
            (rings - 2) * segments * 6 + 2 * segments * 3
        );
        for v in &sphere.vertices {
            let len = (v.position[0] * v.position[0]
                + v.position[1] * v.position[1]
                + v.position[2] * v.position[2])
                .sqrt();
            assert!((len - 1.0).abs() < 1e-4, "球面顶点应位于半径 1 上");
        }
    }

    const CYL_SEGMENTS: u32 = 8;

    #[test]
    fn cylinder_topology() {
        let (radius, height) = (1.5, 2.0);
        let cyl = primitives::cylinder(radius, height, CYL_SEGMENTS);
        // 侧面 2 行 × (segments+1) 闭合环 + 两个帽面各 (segments+1) 环 + 各 1 中心
        assert_eq!(cyl.vertices.len(), 2 * 9 + 2 * 9 + 2);
        // 侧面 segments 个 quad（6 索引）+ 两帽各 segments 个三角（3 索引）
        assert_eq!(
            cyl.indices.len(),
            (CYL_SEGMENTS * 6 + CYL_SEGMENTS * 3 * 2) as usize
        );
        // 索引全部在顶点范围内
        assert!(
            cyl.indices
                .iter()
                .all(|&i| (i as usize) < cyl.vertices.len())
        );
        // 包围盒：XZ 压在 ±radius、Y 压在 ±height/2
        let (min, max) = cyl.bounds().unwrap();
        for i in 0..3 {
            let limit = if i == 1 { height * 0.5 } else { radius };
            assert!(
                min[i] >= -limit - 1e-5 && max[i] <= limit + 1e-5,
                "轴 {i} 超界"
            );
        }
    }

    #[test]
    fn cylinder_normals_and_positions_match_surface() {
        let (radius, height) = (1.5, 2.0);
        let cyl = primitives::cylinder(radius, height, CYL_SEGMENTS);
        let half = height * 0.5;
        let mut cap_centers = 0;
        for v in &cyl.vertices {
            let n = v.normal;
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-5, "法线必须归一化：{n:?}");
            let horizontal = (v.position[0] * v.position[0] + v.position[2] * v.position[2]).sqrt();
            if n[1].abs() > 0.99 {
                // 帽面：中心在轴上（半径 0）、缘点恰在 radius 上，法线纯 ±Y
                if horizontal < 1e-6 {
                    cap_centers += 1;
                    assert_eq!(v.position[1], if n[1] > 0.0 { half } else { -half });
                } else {
                    assert!(
                        (horizontal - radius).abs() < 1e-5,
                        "帽缘应在半径 {radius} 上"
                    );
                }
            } else {
                // 侧面：法线径向（无 Y 分量）且位置在柱面上
                assert!(n[1].abs() < 1e-6, "侧面法线不应有 Y 分量：{n:?}");
                assert!((horizontal - radius).abs() < 1e-5, "侧面顶点应在柱面上");
                assert!(v.position[1].abs() <= half + 1e-6);
            }
        }
        assert_eq!(cap_centers, 2, "应有上下两个帽心");
    }

    #[test]
    fn cylinder_faces_cull_correctly() {
        // 环绕方向与既有约定一致（从外侧看 CCW）：每个三角形的几何法线
        // （边叉积）必须与顶点法线同向——与 uv_sphere 的索引方向教训同源
        let cyl = primitives::cylinder(1.0, 2.0, CYL_SEGMENTS);
        for tri in cyl.indices.chunks(3) {
            let (a, b, c) = (
                cyl.vertices[tri[0] as usize].position,
                cyl.vertices[tri[1] as usize].position,
                cyl.vertices[tri[2] as usize].position,
            );
            let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let e2 = [c[0] - b[0], c[1] - b[1], c[2] - b[2]];
            let cross = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ];
            let normal = cyl.vertices[tri[0] as usize].normal;
            let dot = cross[0] * normal[0] + cross[1] * normal[1] + cross[2] * normal[2];
            assert!(
                dot > 0.0,
                "三角形应从外侧看为 CCW（法线 {normal:?}，叉积 {cross:?}）"
            );
        }
    }

    #[test]
    fn skin_vertex_is_pod_and_layout_matches() {
        // GPU 布局契约：32 字节（16B 关节 + 16B 权重），
        // 两个属性（loc10 Uint32x4 + loc11 Float32x4）
        assert_eq!(std::mem::size_of::<SkinVertex>(), 32);
        assert_eq!(SkinVertex::LAYOUT.array_stride, 32);
        let attrs = SkinVertex::LAYOUT.attributes;
        assert_eq!(attrs.len(), 2);
        assert_eq!((attrs[0].shader_location, attrs[0].offset), (10, 0));
        assert_eq!((attrs[1].shader_location, attrs[1].offset), (11, 16));
        // 位级往返（Pod 契约）
        let v = SkinVertex {
            joints: [7, 0, 3, 1],
            weights: [0.5, 0.25, 0.125, 0.125],
        };
        let bytes: &[u8] = bytemuck::bytes_of(&v);
        let back: SkinVertex = *bytemuck::from_bytes(bytes);
        assert_eq!(back, v);
    }

    #[test]
    #[should_panic(expected = "蒙皮属性数量必须与顶点数量一致")]
    fn upload_mesh_rejects_mismatched_skin_attributes() {
        let gpu = crate::context::Gpu::offscreen();
        let Some(gpu) = gpu else {
            return; // 无 GPU 环境：跳过
        };
        let data = MeshData {
            vertices: primitives::cube().vertices,
            indices: primitives::cube().indices,
            skin: Some(vec![
                SkinVertex {
                    joints: [0; 4],
                    weights: [0.0; 4]
                };
                3
            ]),
            tangents: None,
            uv2: None,
            morph: None,
        };
        let _ = upload_mesh(&gpu, &data);
    }

    /// 切线字节计入内存统计（R42）：16 B/顶点；缺席时零开销。
    #[test]
    fn tangent_bytes_count_into_memory() {
        let mut store = MeshStore::new();
        let mut data = primitives::cube();
        data.tangents = Some(vec![[0.0, 0.0, 1.0, 1.0]; 24]);
        store.add(data);
        assert_eq!(
            store.memory_bytes(),
            (24 * 32 + 36 * 4 + 24 * 16) as u64,
            "切线缓冲 16 B/顶点必须计入"
        );
    }

    /// 光照贴图 UV 字节计入内存统计（R52c）：8 B/顶点；缺席时零开销。
    #[test]
    fn uv2_bytes_count_into_memory() {
        let mut store = MeshStore::new();
        let mut data = primitives::cube();
        assert!(data.uv2.is_none(), "基础图元默认不带光照贴图 UV");
        store.add(data.clone());
        assert_eq!(store.memory_bytes(), (24 * 32 + 36 * 4) as u64);
        data.uv2 = Some(vec![[0.25, 0.75]; 24]);
        store.add(data);
        assert_eq!(
            store.memory_bytes(),
            2 * (24 * 32 + 36 * 4) as u64 + 24 * 8,
            "UV2 缓冲 8 B/顶点必须计入"
        );
    }

    /// GPU 布局契约（R52c）：UV2 = 8 B/顶点的单属性缓冲，
    /// shader_location 13（0–2 主顶点 / 3–9 实例 / 10–11 蒙皮 / 12 切线之后）。
    #[test]
    fn uv2_layout_matches_gpu_contract() {
        assert_eq!(UV2_LAYOUT.array_stride, 8);
        assert_eq!(UV2_LAYOUT.step_mode, wgpu::VertexStepMode::Vertex);
        let attrs = UV2_LAYOUT.attributes;
        assert_eq!(attrs.len(), 1);
        assert_eq!((attrs[0].shader_location, attrs[0].offset), (13, 0));
        assert_eq!(attrs[0].format, wgpu::VertexFormat::Float32x2);
        // 与既有槽位不冲突（切线 12 / 蒙皮 10–11）
        assert_eq!(TANGENT_LAYOUT.attributes[0].shader_location, 12);
    }

    #[test]
    #[should_panic(expected = "光照贴图 UV 数量必须与顶点数量一致")]
    fn upload_mesh_rejects_mismatched_uv2() {
        let gpu = crate::context::Gpu::offscreen();
        let Some(gpu) = gpu else {
            return; // 无 GPU 环境：跳过
        };
        let mut data = primitives::cube();
        data.uv2 = Some(vec![[0.0, 0.0]; 5]);
        let _ = upload_mesh(&gpu, &data);
    }
}
