//! 毛发组件与壳层实例展开（R90；R91 增风场摆动 / LOD 卡片）。
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
//!   底皮照常走普通不透明实例路径。
//!
//! # R91 增量
//!
//! - **风场摆动**（[`HairDynamics`] 组件 opt-in）：CPU Verlet 链积分
//!   （世界空间模拟、局部空间写回；`wind_sample` 逐束根位置单点采样
//!   ——粒子 v2 同款口径；纯函数 [`strand_step`] 提纯单测）。缺席 =
//!   静态发带（R90 行为逐位不变，GPU VBO 缓存零触碰）。
//! - **远景卡片 LOD**（[`HairLodSettings`] 资源 opt-in + 组件
//!   `HairStrands::card`）：距离超阈值（滞回 20% 防抖，`lod_v2_memory`
//!   同款跨帧记忆）时发带换**交叉卡片网格**（groom 的
//!   `generate_hair_cards` 产物，标准顶点格式）走标准不透明管线——
//!   KK 材质照常生效。资源 / 卡片缺席 = 恒发带 = R90 行为。
//!
//! 蒙皮：两条路线均不跟随骨骼（发带实体挂 Transform 层级即可整体
//! 跟随；逐顶点蒙皮无 in-repo 消费者，诚实限制 §8.69① 维持）。

use crate::instance::InstanceRaw;
use crate::mesh::MeshHandle;
use iris_core::prelude::{GlobalTransform, Time, Vec3};

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
    /// 远景卡片网格（R91 LOD）：`Some` 且 [`HairLodSettings`] 资源在场
    /// 时，实体距离超阈值 ⇒ 画卡片（标准不透明管线，KK 材质照常）而非
    /// 发带。`generate_hair_cards` 产物挂进 `MeshStore` 后填入句柄；
    /// `None`（缺省）= 恒发带。
    pub card: Option<MeshHandle>,
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
                card: None,
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
            card: None,
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

// ---- 风场摆动（R91：CPU Verlet 链，确定性）----

/// 单束链的模拟状态（组件内部持有，`pub` 字段仅供测试检视）。
#[derive(Clone, Debug, Default)]
pub struct StrandState {
    /// 当前点（世界空间）。
    pub pos: Vec<[f32; 3]>,
    /// 上一步点（世界空间；Verlet 速度 = pos − prev）。
    pub prev: Vec<[f32; 3]>,
    /// 静止形状（实体局部空间——回弹目标，首帧从 groom 网格捕获）。
    pub rest: Vec<[f32; 3]>,
    /// 段长（rest 相邻点距，发束等距——groom 逐段等跨度）。
    pub seg_len: f32,
}

/// 发带风场摆动参数（R91）。挂上即启用（opt-in——缺席 = 静态发带，
/// R90 行为逐位不变）。
#[derive(Clone, Debug, iris_core::Component)]
pub struct HairDynamics {
    /// 回弹刚度 [0, 1]：每秒朝静止形状恢复的比例（0 = 自由飘，
    /// 1 = 瞬间静止——刚度是「风速归零后回到发型」的时间尺度）。
    pub stiffness: f32,
    /// 速度阻尼 [0, 0.99]：Verlet 速度项的每步保留率（0 = 全阻
    /// 无摆动，0.99 近乎无阻尼长摆）。
    pub damping: f32,
    /// 风力倍率（1 = wind_sample 输出直通为加速度，m/s² 量纲由
    /// 调用方按观感定标）。
    pub wind_scale: f32,
    /// 逐束模拟状态（惰性初始化；`points_per_strand` × 束数）。
    pub strands_state: Vec<StrandState>,
    /// 逐束控制点数（惰性初始化时捕获——顶点布局变更是结构级编辑，
    /// 重建组件即重置发型）。
    pub points_per_strand: usize,
    /// 逐束随机字（mix64 流——风力相位差 / 束间去相关）。
    pub strand_seeds: Vec<u64>,
}

impl Default for HairDynamics {
    fn default() -> Self {
        Self {
            stiffness: 0.5,
            damping: 0.9,
            wind_scale: 1.0,
            strands_state: Vec::new(),
            points_per_strand: 0,
            strand_seeds: Vec::new(),
        }
    }
}

/// 单束链一步模拟（**纯函数**，单测锚）：半隐式 Verlet 积分 → 根部
/// 钉扎 → 刚度回弹 → 距离约束（从根到梢逐段定长，1 次迭代）。
#[allow(clippy::too_many_arguments)]
///
/// 确定性契约：全部 f32 逐式、固定迭代序（点序 = 链序）；`rest_world`
/// 由调用方变换到世界空间（回弹目标随实体移动）。
pub fn strand_step(
    pos: &mut [[f32; 3]],
    prev: &mut [[f32; 3]],
    rest_world: &[[f32; 3]],
    seg_len: f32,
    stiffness: f32,
    damping: f32,
    acc: [f32; 3],
    dt: f32,
) {
    let n = pos.len().min(prev.len()).min(rest_world.len());
    if n == 0 {
        return;
    }
    // 1. Verlet 积分（跳过根点——钉扎在 rest_world[0]）
    let keep = 1.0 - damping;
    let dt2 = dt * dt;
    for i in 1..n {
        let p = pos[i];
        for k in 0..3 {
            let vel = (pos[i][k] - prev[i][k]) * keep;
            pos[i][k] = p[k] + vel + acc[k] * dt2;
        }
        prev[i] = p;
    }
    // 2. 根部钉扎（rest_world 由实体变换给出——实体移动时发根跟随）
    pos[0] = rest_world[0];
    prev[0] = rest_world[0];
    // 3. 刚度回弹：每点朝静止形状拉近 stiffness × dt（钳 [0, 1]——
    // dt 大步进不会过冲）
    let k_recoil = (stiffness * dt).clamp(0.0, 1.0);
    for i in 1..n {
        for k in 0..3 {
            pos[i][k] += (rest_world[i][k] - pos[i][k]) * k_recoil;
        }
    }
    // 4. 距离约束：从根到梢逐段恢复 rest 段长（顺序单遍——发链
    // 「根部优先」的确定性序；零长度段防御跳过）
    for i in 0..n - 1 {
        let dx = pos[i + 1][0] - pos[i][0];
        let dy = pos[i + 1][1] - pos[i][1];
        let dz = pos[i + 1][2] - pos[i][2];
        let len = (dx * dx + dy * dy + dz * dz).sqrt();
        if len < 1e-8 {
            continue;
        }
        let scale = seg_len / len;
        pos[i + 1][0] = pos[i][0] + dx * scale;
        pos[i + 1][1] = pos[i][1] + dy * scale;
        pos[i + 1][2] = pos[i][2] + dz * scale;
    }
}

/// splitmix64（`particles.rs` 同款——束间去相关种子）。
fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// 从发带控制网格抽取第 `strand` 束的控制点（局部空间）。
///
/// 段四边形布局下，非末控制点 j 的坐标 = 段 j 顶点 0 的 position；
/// **末控制点没有「段 points−1」**，其坐标 = 段 points−2 顶点 2
///（顶点 position 存控制点本身——展宽在 VS；关节共享保证相邻段的
/// 副本逐位同值，取首个副本即真值）。
fn strand_points(
    mesh: &crate::groom::HairStrandMesh,
    strand: usize,
    points: usize,
) -> Vec<[f32; 3]> {
    let segments = points - 1;
    let base = strand * segments * 4;
    (0..points)
        .map(|j| {
            if j < segments {
                mesh.vertices[base + j * 4].position
            } else {
                mesh.vertices[base + (j - 1) * 4 + 2].position
            }
        })
        .collect()
}

/// 把控制点写回网格顶点（控制点 j 的全部副本：段 j−1 顶点 2/3 与
/// 段 j 顶点 0/1——越界段跳过），随后重算逐控制点切线（中心差分）
/// 并同步到副本。
fn write_strand_points(mesh: &mut crate::groom::HairStrandMesh, strand: usize, pts: &[[f32; 3]]) {
    let points = pts.len();
    let segments = points - 1;
    let base = strand * segments * 4;
    for (j, p) in pts.iter().enumerate() {
        let p = *p;
        // 段 j 顶点 0/1（存在的段）
        if j < segments {
            for k in 0..2 {
                mesh.vertices[base + j * 4 + k].position = p;
            }
        }
        // 段 j−1 顶点 2/3（存在的段）
        if j > 0 {
            for k in 2..4 {
                mesh.vertices[base + (j - 1) * 4 + k].position = p;
            }
        }
    }
    // 切线重算（中心差分，与 groom 同式）后同步副本
    for (j, p) in pts.iter().enumerate() {
        let a = pts[j.saturating_sub(1)];
        let b = pts[(j + 1).min(points - 1)];
        let _ = p;
        let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let t = [d[0] / len, d[1] / len, d[2] / len];
        if j < segments {
            for k in 0..2 {
                let v = &mut mesh.vertices[base + j * 4 + k];
                v.dir_width[0] = t[0];
                v.dir_width[1] = t[1];
                v.dir_width[2] = t[2];
            }
        }
        if j > 0 {
            for k in 2..4 {
                let v = &mut mesh.vertices[base + (j - 1) * 4 + k];
                v.dir_width[0] = t[0];
                v.dir_width[1] = t[1];
                v.dir_width[2] = t[2];
            }
        }
    }
}

/// 发带风场摆动系统（R91）。注册在 PostUpdate（`update_particles`
/// 同层——层级传播之后、渲染之前；`DefaultPlugins` 已含）。
///
/// - `Time` 缺席 / dt = 0 ⇒ 不动（与粒子同款兜底）；
/// - `Wind` 缺席 ⇒ 无风（零加速度——纯阻尼回弹收敛到静止形状）；
/// - 风力 = `wind_sample` 在**发束根部世界位置**单点采样（粒子 v2
///   同款口径——逐控制点采样是后续增量）× `wind_scale` × 逐束去相关
///   调制（mix64 种子，幅度 ±33%——束间不齐步摆）。
///
/// 世界空间模拟、局部空间写回（`inverse(model) × pos`）——实体移动时
/// 惯性自然（发梢甩动），静止形状随实体变换（发根跟随）。
///
/// **网格布局前提**：`HairStrandMesh::points_per_strand ≥ 2`（groom
/// 产物自带；自定义网格未声明 ⇒ warn 跳过——顶点数对（束 × 段 × 4）
/// 的分解有歧义，不猜）。顶点数与声明不符（结构级编辑）⇒ 重新捕获
/// 静止形状（发型重置）。
pub fn update_hair_dynamics(world: &iris_core::world::World) {
    let dt = world
        .try_resource::<Time>()
        .map(|t| t.delta_secs())
        .unwrap_or(0.0);
    if dt == 0.0 {
        return;
    }
    let elapsed = world
        .try_resource::<Time>()
        .map(|t| t.elapsed_secs())
        .unwrap_or(0.0);
    let wind = world.try_resource::<iris_core::wind::Wind>();
    world.query::<(
        iris_core::prelude::Entity,
        &GlobalTransform,
        &mut HairStrands,
        &mut HairDynamics,
    )>(|(_e, gt, strands, dynamics)| {
        let model = gt.matrix();
        let inv = model.inverse();
        let points = strands.mesh.points_per_strand;
        if points < 2 {
            log::warn!("发带动态：控制网格未声明 points_per_strand（自定义网格需手填），跳过");
            return;
        }
        let segments = points - 1;
        let strands_count = strands.mesh.vertices.len() / (segments * 4);
        if strands_count == 0 {
            return;
        }
        // 惰性初始化（首帧 / 结构级网格变更后）：捕获静止形状与种子
        if dynamics.points_per_strand != points || dynamics.strands_state.len() != strands_count {
            dynamics.points_per_strand = points;
            dynamics.strand_seeds = (0..strands_count as u64)
                .map(|s| mix64(s.wrapping_add(0x5EED_0000_0000_0091)))
                .collect();
            dynamics.strands_state = (0..strands_count)
                .map(|s| {
                    let rest = strand_points(&strands.mesh, s, points);
                    let pos: Vec<[f32; 3]> = rest
                        .iter()
                        .map(|p| {
                            let w = model.transform_point3(Vec3::from(*p));
                            [w.x, w.y, w.z]
                        })
                        .collect();
                    // 段长 = rest 首段（groom 逐段等跨度）
                    let a = Vec3::from(rest[0]);
                    let b = Vec3::from(rest[1]);
                    StrandState {
                        seg_len: (b - a).length(),
                        prev: pos.clone(),
                        pos,
                        rest,
                    }
                })
                .collect();
        }
        let stiffness = dynamics.stiffness;
        let damping = dynamics.damping;
        let wind_scale = dynamics.wind_scale;
        for s in 0..strands_count {
            let state = &mut dynamics.strands_state[s];
            // 静止形状变换到世界（回弹目标 + 发根跟随）
            let rest_world: Vec<[f32; 3]> = state
                .rest
                .iter()
                .map(|p| {
                    let w = model.transform_point3(Vec3::from(*p));
                    [w.x, w.y, w.z]
                })
                .collect();
            // 风采样（发根单点）× 逐束去相关调制
            let acc = if let Some(w) = wind.as_deref() {
                let root_world = Vec3::from(rest_world[0]);
                let v = iris_core::wind::wind_sample(w, root_world, elapsed);
                let f = mix64(dynamics.strand_seeds[s]);
                let mod_f = 1.0 + (f as f32 / u64::MAX as f32 - 0.5) * 0.66;
                [
                    v.x * wind_scale * mod_f,
                    v.y * wind_scale * mod_f,
                    v.z * wind_scale * mod_f,
                ]
            } else {
                [0.0; 3]
            };
            strand_step(
                &mut state.pos,
                &mut state.prev,
                &rest_world,
                state.seg_len,
                stiffness,
                damping,
                acc,
                dt,
            );
            // 世界 → 局部写回（顶点 position / 切线）
            let local: Vec<[f32; 3]> = state
                .pos
                .iter()
                .map(|p| {
                    let l = inv.transform_point3(Vec3::from(*p));
                    [l.x, l.y, l.z]
                })
                .collect();
            write_strand_points(&mut strands.mesh, s, &local);
        }
    });
}

/// 发带远景卡片 LOD 设置（R91，资源 opt-in——缺席 = 恒发带）。
#[derive(Clone, Copy, Debug, iris_core::Resource)]
pub struct HairLodSettings {
    /// 切换距离（米）：实体到相机 eye 超过该值 ⇒ 画卡片（若组件配了
    /// `card`）；滞回带 = 距离 × 0.8（远→近回到 0.8× 以内才切回发带
    /// ——防边界抖动，`lod_v2_memory` 同款跨帧记忆）。
    pub card_distance: f32,
}

impl Default for HairLodSettings {
    fn default() -> Self {
        Self {
            card_distance: 12.0,
        }
    }
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

    // ---- R91：Verlet 摆动锚 ----

    /// 恒风渐倾：恒定加速度下链段被风吹偏、刚度把它拉回——多次步进后
    /// 达到动态平衡（末点水平偏移 ∈ (0, 数值上界)，且不再增长）。
    #[test]
    fn strand_step_steady_state_under_constant_wind() {
        let n = 5;
        let rest: Vec<[f32; 3]> = (0..n).map(|i| [0.0, -(i as f32) * 0.1, 0.0]).collect();
        let mut pos = rest.clone();
        let mut prev = rest.clone();
        let acc = [0.5, 0.0, 0.0];
        for _ in 0..200 {
            strand_step(&mut pos, &mut prev, &rest, 0.1, 0.3, 0.9, acc, 1.0 / 60.0);
        }
        let mut last_offset = pos[n - 1][0];
        let mut converged = true;
        for _ in 0..20 {
            strand_step(&mut pos, &mut prev, &rest, 0.1, 0.3, 0.9, acc, 1.0 / 60.0);
            if (pos[n - 1][0] - last_offset).abs() > 1e-4 {
                converged = false;
            }
            last_offset = pos[n - 1][0];
        }
        assert!(last_offset > 1e-3, "恒风下末点应被吹偏（得 {last_offset}）");
        assert!(converged, "200 步后应达动态平衡（偏移不再显著变化）");
        // 根部恒钉扎
        assert_eq!(pos[0], rest[0]);
    }

    /// 距离约束：任意风向下链长守恒（相邻点距 = rest 段长，1e-5 容差
    /// ——f32 约束迭代的单遍残差）。
    #[test]
    fn strand_step_preserves_segment_length() {
        let rest: Vec<[f32; 3]> = (0..6).map(|i| [0.0, -(i as f32) * 0.2, 0.0]).collect();
        let mut pos = rest.clone();
        let mut prev = rest.clone();
        for step in 0..50 {
            let acc = [step as f32 * 0.1, -0.3, (step as f32 % 7.0) * 0.2];
            strand_step(&mut pos, &mut prev, &rest, 0.2, 0.05, 0.95, acc, 1.0 / 30.0);
        }
        for i in 0..5 {
            let d = [
                pos[i + 1][0] - pos[i][0],
                pos[i + 1][1] - pos[i][1],
                pos[i + 1][2] - pos[i][2],
            ];
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            assert!(
                (len - 0.2).abs() < 1e-5,
                "段 {i} 长 {len} 应守恒 0.2（±1e-5）"
            );
        }
    }

    /// 确定性铁律：同输入序列两份状态并行步进，逐位一致；阻尼风止后
    /// 链回静止（stiffness 1 = 瞬间回弹锚）。
    #[test]
    fn strand_step_deterministic_and_recovers() {
        let rest: Vec<[f32; 3]> = (0..4).map(|i| [0.0, -(i as f32) * 0.15, 0.0]).collect();
        let run = || {
            let mut pos = rest.clone();
            let mut prev = rest.clone();
            for step in 0..30 {
                strand_step(
                    &mut pos,
                    &mut prev,
                    &rest,
                    0.15,
                    0.4,
                    0.85,
                    [0.7, 0.0, 0.3],
                    1.0 / 60.0 + step as f32 * 1e-6,
                );
            }
            // 风止 + 满刚度：回静止形状
            for _ in 0..5 {
                strand_step(
                    &mut pos,
                    &mut prev,
                    &rest,
                    0.15,
                    1.0,
                    0.85,
                    [0.0; 3],
                    1.0 / 60.0,
                );
            }
            (pos, prev)
        };
        let (p1, _v1) = run();
        let (p2, _v2) = run();
        assert_eq!(p1, p2, "同输入序列双跑逐位一致");
        // stiffness 1 × dt 钳 1 ⇒ 每步全量回弹 + 距离约束保持段长
        for (i, p) in p1.iter().enumerate() {
            let d = (p[0] - rest[i][0])
                .abs()
                .max((p[1] - rest[i][1]).abs())
                .max((p[2] - rest[i][2]).abs());
            assert!(d < 0.16, "风止后点 {i} 应回静止附近（偏差 {d}）");
        }
    }

    /// 写回往返：strands 控制点 →（模拟扰动）→ 写回 → 再抽取一致；
    /// 切线归一保持；邻束不受影响。
    #[test]
    fn strand_write_read_roundtrip() {
        let params = crate::groom::HairGroomParams {
            strands: 3,
            points: 5,
            ..Default::default()
        };
        let mut mesh = crate::groom::generate_hair_strands(&params);
        let segments = 4;
        let strand0_original = strand_points(&mesh, 0, 5);
        let original = strand_points(&mesh, 1, 5);
        // 扰动：整体 x 偏移 + 写回
        let perturbed: Vec<[f32; 3]> = original.iter().map(|p| [p[0] + 0.05, p[1], p[2]]).collect();
        write_strand_points(&mut mesh, 1, &perturbed);
        let reread = strand_points(&mesh, 1, 5);
        assert_eq!(reread, perturbed, "写回后重读一致");
        // 切线仍归一（束 1 的顶点区间 [segments×4, 2×segments×4)）
        for v in &mesh.vertices[segments * 4..segments * 4 * 2] {
            let t = v.dir_width;
            let len = (t[0] * t[0] + t[1] * t[1] + t[2] * t[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-5, "切线必须归一，得 {len}");
        }
        // 邻束不受影响
        assert_eq!(
            strand_points(&mesh, 0, 5),
            strand0_original,
            "束 0 的控制点不受束 1 写回影响"
        );
    }
}
