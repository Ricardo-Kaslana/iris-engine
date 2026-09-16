//! 烘焙反射探针（R62a）：局部长方体影响域的 cubemap 捕获 + GGX 预滤波 +
//! 逐片元视差修正的镜面反射。
//!
//! ## 解决什么问题
//!
//! 全局 IBL 环境贴图只能反射**天空**（§4.12）：房间里的金属球与地板反射
//! 不到周围的墙、家具与灯箱。反射探针把「场景中一点向 6 面看到的辐射度」
//! 烘成一张 cubemap（按粗糙度预滤波，与 env 同一套 split-sum 链），片元
//! 落在探针影响域内时用探针取代全局 env 做**镜面**采样，漫反射仍走全局
//! 辐照度。
//!
//! ## 为什么影响域是**盒**而不是球
//!
//! 1. **室内房间是盒**：探针的典型用例是矩形房间 / 走廊 / 货架间，长方体
//!    影响域能贴着墙、地板、天花板铺满空间，球形域要么包住墙外（把墙外
//!    的辐射度也算进来）、要么缩进房间中央留一圈无探针的边角。
//! 2. **视差修正的投影几何就是盒**：反射射线与**轴对齐长方体**求交得到的
//!    出射点方向，正是「假设房间是一个盒、墙在盒面上」的解析解
//!    （[`parallax_direction`]）；球面投影的解析解不存在（球与射线的交点
//!    方向与房间几何无关），只能退化为「按距离缩放方向」的近似。
//! 3. **权重是逐轴可分离的**：盒的过渡带可以逐轴定义（[`influence_weight`]），
//!    球只能给出径向衰减——贴墙的探针在墙侧会留下一条权重不满的缝。
//!
//! ## 语义摘要
//!
//! - 位置取实体 `GlobalTransform`；`half_extents` = 影响域半边长（盒中心 =
//!   实体原点），`falloff` = 边界**向内的**过渡带宽度（域内 1 / 带内线性
//!   降到 0 / 域外 0）。
//! - 多探针重叠：逐片元取**影响权重最大者**，并列用实体位模式 tie-break
//!   （uniform 数组按实体位模式升序排列，着色器取严格更大者 ⇒ 位模式小者
//!   胜出）。**不做多探针混合**（拒绝清单，见 §8.37）。
//! - 组件缺席 / 探针未烘焙 ⇒ 与 R61 逐位一致（全局 env 原样）。
//!
//! ## 诚实限制（v1）
//!
//! - **单探针硬切换**：重叠区按权重取一个探针，两条通道之间没有混合——
//!   权重接近 1 的两个探针在交界处会出现反射内容的硬切（权重过渡带只淡化
//!   「探针 vs 全局 env」的切换，不淡化「探针 vs 探针」）。多探针加权混合
//!   在拒绝清单内。
//! - **无磁盘持久化 / 无 pack 集成**：探针每次启动重新烘焙（重捕获的
//!   确定性由本轮测试守护）；持久化留后续轮。
//! - **静态烘焙**：不做每帧重捕获（动态探针在拒绝清单内），场景变化后需
//!   调用方重新 [`crate::Renderer::bake_reflection_probe`]。
//! - **无平面镜**：探针内不做平面反射（planar reflection）——地板反射是
//!   盒投影近似（视差修正让反射贴合盒几何，但不含逐像素精确的镜像）。
//!   逐像素精确的平面反射自 R81 起由水面专属路径提供（`water` 模块，
//!   捕获范围只限 WaterSurface 邻域，不进探针 cubemap）。
//! - **捕获不含透明物 / 粒子 / 贴花**（与点光阴影捕获同一取舍：只画不透明
//!   普通 + 不透明蒙皮）；捕获含天空（决策与取舍见 §8.37）。
//! - **无探针编辑入口**（R58b 编辑器集成留游戏工具轮）。

use std::collections::BTreeMap;

use glam::{Vec3, Vec3Swizzles};
use iris_core::Component;

use crate::light::CascadeFit;
use crate::pipeline::SHADOW_MAP_SIZE;

/// 同时**烘焙并上传**的反射探针预算（uniform 数组容量 / cubemap 数组槽位
/// 数）。超出预算的探针不分配槽位（`log::warn`），与点光阴影的
/// [`crate::light::POINT_SHADOW_BUDGET`] 同款「预算做成常量而非资源」取舍：
/// cubemap 数组层数在 `Renderer::new` 固定，运行时调整需重建纹理与全部面
/// 视图 / 绑定组，收益远小于复杂度。
pub const MAX_REFLECTION_PROBES: usize = 4;

/// 探针 cubemap 单面边长（与全局 env 贴图同分辨率：同一套预滤波链、
/// 同一档 mip 语义）。
pub const PROBE_MAP_SIZE: u32 = 128;

/// 探针预滤波 mip 级数：**与全局 env 相同**（粗糙度 0..1 线性映射到
/// mip 层，着色器用同一个 `roughness × env_params.x` 表达式取 mip）。
pub const PROBE_MIP_COUNT: u32 = crate::environment::ENV_MIP_COUNT;

const _: () = assert!(
    PROBE_MIP_COUNT == crate::environment::ENV_MIP_COUNT,
    "探针与全局 env 必须共用同一套粗糙度 → mip 映射（mip 级数与语义一致）"
);

/// 捕获相机的近平面（与点光阴影同一取值：贴上探针点且极近的几何直接裁掉）。
pub const PROBE_CAPTURE_NEAR: f32 = 0.05;

/// 捕获相机的远平面 = 捕获半径（米）。6 个 90° 面的并集已覆盖**完整球域**，
/// 逐面视锥剔除没有收益，故捕获期的收集用「全通过」视锥、远平面取关卡量级
/// 的保守半径；超出该半径的几何不参与捕获（远景只影响掠射方向的微弱贡献）。
pub const PROBE_CAPTURE_FAR: f32 = 1000.0;

/// 过渡带默认宽度（米）：从域边界向内衰减到 1 的距离。
pub const PROBE_DEFAULT_FALLOFF: f32 = 0.5;

/// 反射探针组件：位置取同实体 `GlobalTransform`，影响域 = 以实体原点为中心
/// 的**轴对齐长方体**（局部轴 = 世界轴，不支持旋转——见模块文档「为什么
/// 影响域是盒」；旋转盒的视差投影不再是解析解）。
///
/// 组件缺席 = 该实体不是探针（默认路径零开销）。组件在场但未烘焙 =
/// [`crate::Renderer::bake_reflection_probe`] 未调用过 ⇒ 着色完全走全局 env，
/// 与没有该组件时逐位一致。
#[derive(Clone, Copy, Debug, Component)]
pub struct ReflectionProbe {
    /// 影响域半边长（米，三分量须为正且有限）。
    pub half_extents: Vec3,
    /// 边界过渡带宽度（米，≥ 0）：域边界向内 `falloff` 米内权重线性衰减，
    /// 用于把「探针 → 全局 env」的切换摊开成一条渐变带（0 = 硬边界）。
    pub falloff: f32,
}

impl Default for ReflectionProbe {
    fn default() -> Self {
        Self {
            half_extents: Vec3::splat(5.0),
            falloff: PROBE_DEFAULT_FALLOFF,
        }
    }
}

impl ReflectionProbe {
    /// 以半边长构造探针（过渡带取 [`PROBE_DEFAULT_FALLOFF`]）。
    pub fn new(half_extents: Vec3) -> Self {
        Self {
            half_extents,
            falloff: PROBE_DEFAULT_FALLOFF,
        }
    }

    /// 设置过渡带宽度（负值钳到 0）。
    pub fn with_falloff(mut self, falloff: f32) -> Self {
        self.falloff = falloff.max(0.0);
        self
    }

    /// 影响域是否可用：半边长三分量都为正且有限。退化的探针在收集期被
    /// 跳过并告警（着色器因此永远看不到零体积盒，`t = min t_i` 不会除零）。
    pub fn is_valid(&self) -> bool {
        let e = self.half_extents;
        e.x.is_finite() && e.y.is_finite() && e.z.is_finite() && e.x > 0.0 && e.y > 0.0 && e.z > 0.0
    }
}

/// 影响域权重（纯函数）：域内 1、过渡带内单调降到 0、域外 0。
///
/// 逐轴定义 `f_i = clamp((E_i − |p_i|) / falloff, 0, 1)` 后取三轴乘积：
///
/// - `|p_i| ≤ E_i − falloff` ⇒ `f_i = 1`（内盒，恒等）；
/// - `E_i − falloff < |p_i| ≤ E_i` ⇒ `f_i` 随 `|p_i|` 线性降到 0；
/// - `|p_i| > E_i` ⇒ `f_i = 0`（**乘积因此为 0**，域外必不生效）。
///
/// 乘积形式（而非三轴取 min）在角区给出二次衰减，权重等高面是圆角盒——
/// 与「盒面附近的两面墙都会削弱探针可信度」的直觉一致，且仍满足单调性与
/// 值域 [0, 1]。`falloff = 0` 时退化为二值判定（域内 1 / 域外 0），
/// 不产生除零。
///
/// `local` = 片元位置 − 探针中心（盒是轴对齐的，无需旋转）。
pub fn influence_weight(local: Vec3, half_extents: Vec3, falloff: f32) -> f32 {
    let m = falloff.max(0.0);
    let mut weight = 1.0f32;
    for axis in 0..3 {
        let d = half_extents[axis] - local[axis].abs();
        let f = if m > 0.0 {
            (d / m).clamp(0.0, 1.0)
        } else if d >= 0.0 {
            1.0
        } else {
            0.0
        };
        weight *= f;
    }
    weight
}

/// 视差修正（经典 box projection，纯函数）：把反射方向重定向到「从探针中心
/// 看向盒面出射点」的方向。
///
/// ## 推导
///
/// 盒是轴对齐的，取片元相对探针中心的局部坐标 `p`（盒 = `[−E, E]`），反射
/// 射线为 `p + t·d`（`t ≥ 0`）。逐轴求与盒**出射面**的交点参数：
///
/// ```text
/// d_i ≠ 0 :  t_i = (sign(d_i)·E_i − p_i) / d_i
/// d_i = 0 :  |p_i| ≤ E_i ⇒ 该轴不约束（t_i = +∞）
///            |p_i| > E_i ⇒ 射线恒在该轴的板外 ⇒ 无交点
/// t = min_i t_i,   q = p + t·d,   d' = normalize(q)
/// ```
///
/// 性质（单测逐条锁定）：
/// - **中心恒等**：`p = 0` ⇒ `q = t·d ∝ d`，任意方向都不被改写；
/// - 域内任意点 `q` 必落在盒面上（`max_i |q_i| / E_i = 1`，容差内）；
/// - `d_i` 分量精确为 0 时**不做除法**（避免 0/0 产生 NaN）；
/// - 片元在盒外且射线背离盒（或 `t ≤ 0`、方向为零向量）⇒ **原样返回**，
///   由权重把该片元交给全局 env。
///
/// 返回值是单位向量（回退分支原样返回入参，调用方传入的是 `reflect()` 的
/// 单位向量）。
pub fn parallax_direction(direction: Vec3, local: Vec3, half_extents: Vec3) -> Vec3 {
    let mut t_min = f32::INFINITY;
    let mut miss = false;
    for axis in 0..3 {
        let d = direction[axis];
        if d == 0.0 {
            // 该轴并行：片段落在该轴板的范围内则无约束，否则射线永不进入盒
            if local[axis].abs() > half_extents[axis] {
                miss = true;
            }
            continue;
        }
        let bound = if d > 0.0 {
            half_extents[axis]
        } else {
            -half_extents[axis]
        };
        t_min = t_min.min((bound - local[axis]) / d);
    }
    // `t_min` 非有限（三轴全并行）或非正（射线背离盒）都回退原方向
    if miss || t_min <= 0.0 || !t_min.is_finite() {
        return direction;
    }
    let hit = local + t_min * direction;
    if hit.length_squared() <= 0.0 {
        return direction;
    }
    hit.normalize()
}

/// 点到轴对齐盒的距离（米，盒内 0）——探针预算选择键的第一分量。
pub fn distance_to_box(point: Vec3, center: Vec3, half_extents: Vec3) -> f32 {
    let d = (point - center).abs() - half_extents;
    d.max(Vec3::ZERO).length()
}

/// 预算选择的一个候选（选择键的输入）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProbeCandidate {
    /// 探针实体句柄的位模式（`Entity::to_bits`）——选择键的**最终**并列
    /// 断言，保证全序与收集顺序无关。
    pub entity_bits: u64,
    /// 相机 eye 到探针**影响域**的距离（米，域内 0）。
    pub camera_distance: f32,
}

/// 探针预算的**确定性选择键**：相机到影响域的距离升序，并列用实体位模式
/// 升序（与点光阴影 [`crate::light::select_point_shadow_casters`] 同款纪律：
/// 比较器是全序 ⇒ 结果只取决于探针集合本身，与查询 / 插入顺序无关）。
///
/// 返回值 = 被选中候选在入参切片中的下标，按**实体位模式升序**排列——
/// 该序就是 uniform 数组序，也是着色器「权重并列时取先出现者」的 tie-break
/// 序（位模式小者胜出）。`candidates.len() <= budget` 时全选。
pub fn select_active_probes(candidates: &[ProbeCandidate], budget: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    order.sort_by(|&a, &b| {
        candidates[a]
            .camera_distance
            .total_cmp(&candidates[b].camera_distance)
            .then_with(|| candidates[a].entity_bits.cmp(&candidates[b].entity_bits))
    });
    order.truncate(budget);
    // uniform 数组序 = 实体位模式升序（tie-break 序），与选择键序解耦
    order.sort_by_key(|&i| candidates[i].entity_bits);
    order
}

/// 探针捕获的方向光阴影拟合（纯函数）：把**探针盒**当作单级联阴影的覆盖
/// 域，得到一个 2048² 的光空间正交矩阵。
///
/// 复用 [`crate::light::cascade_fit_params`] 的口径：光视图 = 以世界原点为
/// 眼点、沿光方向的纯旋转（`look_at`），盒 8 个角点变换到光空间求 AABB，
/// XY 走纹素对齐（[`crate::light::snap_aabb_to_texels`]，相位稳定），深度
/// 窗口以 AABB 包围球半径向光 / 背光各外扩（保证盒外投射物与盒底面都在
/// 窗口内）。
///
/// 与相机级联的区别只在**覆盖域**：级联取相机视锥深度片，探针取探针盒。
/// 用途见 [`crate::Renderer::bake_reflection_probe`] 的捕获阴影说明。
pub fn fit_probe_shadow(light_direction: Vec3, center: Vec3, half_extents: Vec3) -> CascadeFit {
    let dir = light_direction.normalize_or_zero();
    let up = if dir.y.abs() > 0.99 { Vec3::X } else { Vec3::Y };
    let light_view = glam::camera::rh::view::look_at_mat4(Vec3::ZERO, dir, up);

    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for sx in [-1.0f32, 1.0] {
        for sy in [-1.0f32, 1.0] {
            for sz in [-1.0f32, 1.0] {
                let corner = center + half_extents * Vec3::new(sx, sy, sz);
                let ls = light_view.transform_point3(corner);
                min = min.min(ls);
                max = max.max(ls);
            }
        }
    }
    let (min_xy, max_xy) = crate::light::snap_aabb_to_texels(min.xy(), max.xy(), SHADOW_MAP_SIZE);
    let min = Vec3::new(min_xy.x, min_xy.y, min.z);
    let max = Vec3::new(max_xy.x, max_xy.y, max.z);
    let radius = (max - min).length() * 0.5;
    let near = -max.z - radius;
    let far = -min.z + radius;
    let proj = glam::camera::rh::proj::directx::orthographic(min.x, max.x, min.y, max.y, near, far);
    let span = max_xy - min_xy;
    CascadeFit {
        view_proj: proj * light_view,
        texel_world: span.x.max(span.y) / SHADOW_MAP_SIZE as f32,
        depth_window: far - near,
    }
}

/// 探针烘焙登记表（CPU 侧句柄登记，渲染器内部持有）。
///
/// ## 为什么不是 `LightmapStore` 那样的 World 资源
///
/// `LightmapStore` 登记的是**CPU 侧像素数据**（烘焙图集本体），渲染器按句柄
/// 懒上传。探针的烘焙产物没有 CPU 侧副本：捕获 6 面 → 预滤波 → 全程在 GPU
/// 上完成（回读 cubemap 只会引入一次昂贵的同步读回与无损往返问题）。需要
/// 登记的只有两件事：**实体 → cubemap 槽位**，以及**该槽位是否已烘焙**。
/// 后者是写路径（烘焙发生在 `Renderer::bake_reflection_probe`），而
/// `Renderer::draw` 只拿 `&World`——放进 World 资源会需要 `&mut World` 或
/// 内部可变性，且会带来「World 说已烘焙、Renderer 里没有对应纹理」的撕裂
/// 可能。故登记表与 GPU 资源同生命周期，放在渲染器内部。
///
/// ## 槽位分配（确定性）
///
/// 槽位按**首次烘焙调用序**递增分配（0..[`MAX_REFLECTION_PROBES`]），同一
/// 实体重复烘焙复用原槽位（幂等重捕获）。超出预算的实体拿不到槽位（返回
/// `None`，调用方 warn）。`BTreeMap` 保证迭代序确定（统计 / 测试用），
/// 槽位本身与迭代序无关——着色器只认 uniform 里逐探针携带的槽位号。
#[derive(Default)]
pub struct ProbeStore {
    by_entity: BTreeMap<u64, usize>,
    next_slot: usize,
}

impl ProbeStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 已烘焙探针数（已分配槽位的实体数）。
    pub fn len(&self) -> usize {
        self.by_entity.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_entity.is_empty()
    }

    /// 该实体是否已有槽位（= 已烘焙）。
    pub fn slot_of(&self, entity_bits: u64) -> Option<usize> {
        self.by_entity.get(&entity_bits).copied()
    }

    /// 取该实体的 cubemap 槽位；首次调用时分配，预算耗尽返回 `None`。
    pub fn ensure_slot(&mut self, entity_bits: u64) -> Option<usize> {
        if let Some(&slot) = self.by_entity.get(&entity_bits) {
            return Some(slot);
        }
        if self.next_slot >= MAX_REFLECTION_PROBES {
            return None;
        }
        let slot = self.next_slot;
        self.next_slot += 1;
        self.by_entity.insert(entity_bits, slot);
        Some(slot)
    }

    /// 槽位向量占用的 CPU 字节数（登记表本体，不含 GPU 纹理）。
    pub fn memory_bytes(&self) -> u64 {
        (self.by_entity.len() * (std::mem::size_of::<u64>() + std::mem::size_of::<usize>())) as u64
    }
}

/// 单个探针的 GPU 上传布局（与 WGSL `ReflectionProbeGpu` 逐字段对齐，32 B）。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct ReflectionProbeRaw {
    /// xyz: 探针世界位置（盒中心），w: 过渡带宽度（米）。
    pub center_falloff: [f32; 4],
    /// xyz: 影响域半边长（米），w: cubemap 数组槽位。
    pub extents_slot: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<ReflectionProbeRaw>() == 32,
    "ReflectionProbeGpu 必须与 WGSL 布局一致（2×vec4）"
);

/// 探针 uniform 块（与 WGSL `ProbeParams` 对齐）：
/// `count: vec4<f32>` + `probes: array<ReflectionProbeGpu, MAX>`。
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct ProbeParamsRaw {
    /// x: 本帧生效探针数（0 = 全部片元走全局 env，逐位等于 R61 路径）。
    pub count: [f32; 4],
    pub probes: [ReflectionProbeRaw; MAX_REFLECTION_PROBES],
}

const _: () = assert!(
    std::mem::size_of::<ProbeParamsRaw>() == 16 + MAX_REFLECTION_PROBES * 32,
    "ProbeParams 必须与 WGSL 布局一致（vec4 + N×2×vec4）"
);

impl Default for ProbeParamsRaw {
    fn default() -> Self {
        Self {
            count: [0.0; 4],
            probes: [ReflectionProbeRaw {
                center_falloff: [0.0; 4],
                extents_slot: [0.0; 4],
            }; MAX_REFLECTION_PROBES],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() <= 1e-5
    }

    fn close3(a: Vec3, b: Vec3) -> bool {
        close(a.x, b.x) && close(a.y, b.y) && close(a.z, b.z)
    }

    // ---- 影响域权重 ----

    /// 域内（内盒）权重恒 1：含中心与内盒角点（恰在带上界）。
    #[test]
    fn weight_is_one_inside_inner_box() {
        let e = Vec3::new(2.0, 3.0, 4.0);
        let m = 1.0;
        assert_eq!(influence_weight(Vec3::ZERO, e, m), 1.0);
        // 内盒角点：(E_i − m)，逐轴恰好到达带的上界 ⇒ 仍是 1
        assert_eq!(influence_weight(Vec3::new(1.0, 2.0, 3.0), e, m), 1.0);
        assert_eq!(influence_weight(Vec3::new(-1.0, -2.0, -3.0), e, m), 1.0);
    }

    /// 过渡带：单轴精确线性（半带 0.5）、双轴乘积 0.25、三轴 0.125。
    #[test]
    fn weight_decays_linearly_in_falloff_band() {
        let e = Vec3::splat(2.0);
        let m = 1.0;
        // |x| = 1.5 ⇒ f_x = (2 − 1.5)/1 = 0.5，其余轴 1 ⇒ 0.5
        assert_eq!(influence_weight(Vec3::new(1.5, 0.0, 0.0), e, m), 0.5);
        assert_eq!(influence_weight(Vec3::new(-1.5, 0.0, 0.0), e, m), 0.5);
        assert_eq!(
            influence_weight(Vec3::new(1.5, 1.5, 0.0), e, m),
            0.25,
            "双轴过渡带 = 逐轴因子乘积"
        );
        assert_eq!(influence_weight(Vec3::new(1.5, 1.5, 1.5), e, m), 0.125);
        // 恰在盒面：因子 0 ⇒ 权重 0（域边界权重为 0 的定义）
        assert_eq!(influence_weight(Vec3::new(2.0, 0.0, 0.0), e, m), 0.0);
    }

    /// 过渡带内沿任意外扩路径**单调不增**（含域外为 0）。
    #[test]
    fn weight_is_monotonic_towards_boundary() {
        let e = Vec3::splat(2.0);
        let m = 0.75;
        let mut prev = 1.0f32;
        for step in 0..=20 {
            let x = step as f32 * 0.2; // 0 .. 4（穿过内盒 / 带 / 域外）
            let w = influence_weight(Vec3::new(x, 0.3 * x, 0.0), e, m);
            assert!(w <= prev + 1e-6, "x = {x} 处权重回升：{prev} → {w}");
            prev = w;
        }
        assert_eq!(prev, 0.0, "域外（|x| > E）权重必须归零");
    }

    /// 域外 0：任一轴越界即整项为 0（乘积语义），且不依赖越界量大小。
    #[test]
    fn weight_is_zero_outside_domain() {
        let e = Vec3::new(2.0, 2.0, 2.0);
        let m = 1.0;
        assert_eq!(influence_weight(Vec3::new(2.0001, 0.0, 0.0), e, m), 0.0);
        assert_eq!(influence_weight(Vec3::new(0.0, 0.0, -9.0), e, m), 0.0);
        assert_eq!(influence_weight(Vec3::new(0.0, 3.0, 0.0), e, m), 0.0);
    }

    /// falloff = 0 退化为二值（域内 1 / 域外 0），无除零、无 NaN。
    #[test]
    fn weight_with_zero_falloff_is_binary() {
        let e = Vec3::splat(2.0);
        assert_eq!(influence_weight(Vec3::new(1.999, 0.0, 0.0), e, 0.0), 1.0);
        assert_eq!(influence_weight(Vec3::new(2.0, 0.0, 0.0), e, 0.0), 1.0);
        assert_eq!(influence_weight(Vec3::new(2.001, 0.0, 0.0), e, 0.0), 0.0);
        let w = influence_weight(Vec3::new(1.0, 1.0, 1.0), e, -3.0);
        assert!(w.is_finite() && w == 1.0, "负 falloff 钳到 0（二值语义）");
    }

    // ---- 视差修正（box projection）----

    /// 中心恒等：盒中心处任意反射方向都不被改写（q = t·d ∝ d）。
    #[test]
    fn parallax_is_identity_at_box_center() {
        let e = Vec3::new(1.0, 2.0, 3.0);
        for dir in [
            Vec3::new(1.0, 2.0, 3.0),
            Vec3::new(-1.0, 0.0, 0.0),
            Vec3::new(0.0, -1.0, 0.2),
            Vec3::new(0.3, -0.9, 0.31),
        ] {
            let d = dir.normalize();
            let out = parallax_direction(d, Vec3::ZERO, e);
            assert!(close3(out, d), "中心处 {d:?} 被改写为 {out:?}");
        }
    }

    /// 偏心解析解：p = (0.5, 0, 0)、d = normalize(1, 1, 0)、E = (1, 1, 1)
    /// ⇒ t = 0.5√2、q = (1, 0.5, 0)、d' = normalize(1, 0.5, 0)。
    #[test]
    fn parallax_redirects_to_box_exit_point() {
        let e = Vec3::splat(1.0);
        let p = Vec3::new(0.5, 0.0, 0.0);
        let d = Vec3::new(1.0, 1.0, 0.0).normalize();
        let out = parallax_direction(d, p, e);
        let expected = Vec3::new(1.0, 0.5, 0.0).normalize();
        assert!(close3(out, expected), "期望 {expected:?}，得到 {out:?}");
        // 与未修正方向确实不同（否则这个测试形同虚设）
        assert!((out - d).length() > 0.2, "重定向方向应与原反射方向显著不同");
    }

    /// 逐轴特例：反射方向平行于某轴（该轴分量精确为 0）时不除零，
    /// 出射点落在该轴的盒面上并据此偏折。
    #[test]
    fn parallax_handles_axis_parallel_rays() {
        let e = Vec3::splat(1.0);
        // d = +X、p = (0.5, 0.9, 0)：t = 0.5，q = (1, 0.9, 0)
        let out = parallax_direction(Vec3::X, Vec3::new(0.5, 0.9, 0.0), e);
        assert!(close3(out, Vec3::new(1.0, 0.9, 0.0).normalize()));
        // d = +Z（另外两轴并行）：q = (0.2, -0.4, 1)
        let out = parallax_direction(Vec3::Z, Vec3::new(0.2, -0.4, 0.3), e);
        assert!(close3(out, Vec3::new(0.2, -0.4, 1.0).normalize()));
    }

    /// 盒外平行射线（永在板外）⇒ 原样返回（回退全局 env 的路径）。
    #[test]
    fn parallax_falls_back_when_ray_misses_box() {
        let e = Vec3::splat(1.0);
        let outside = Vec3::new(2.0, 0.0, 0.0);
        assert_eq!(parallax_direction(Vec3::Y, outside, e), Vec3::Y);
        // 背离盒：t 全为负 ⇒ 原样返回
        assert_eq!(parallax_direction(Vec3::X, outside, e), Vec3::X);
        // 零方向向量：原样返回，不产生 NaN
        let zero = parallax_direction(Vec3::ZERO, Vec3::new(0.1, 0.2, 0.3), e);
        assert_eq!(zero, Vec3::ZERO);
    }

    /// 盒外但射线朝向盒：出射点仍解析（用于过渡带的边缘片元）。
    #[test]
    fn parallax_solves_for_outside_fragments_facing_box() {
        let e = Vec3::splat(1.0);
        let out = parallax_direction(-Vec3::X, Vec3::new(2.0, 0.0, 0.0), e);
        assert!(
            close3(out, -Vec3::X),
            "命中 −X 面 ⇒ 方向为 −X，得到 {out:?}"
        );
        // 斜射：p = (2, 0.5, 0)、d = normalize(-1, 0, 0.5)：+Z 面在 t = √5 ≈ 2.236
        // 先被穿过（t_+Z < t_−X = 1.5√5 ≈ 3.354）⇒ q = (0, 0.5, 1)
        let d = Vec3::new(-1.0, 0.0, 0.5).normalize();
        let out = parallax_direction(d, Vec3::new(2.0, 0.5, 0.0), e);
        let expected = Vec3::new(0.0, 0.5, 1.0).normalize();
        assert!(close3(out, expected), "期望 {expected:?}，得到 {out:?}");
        assert!(out.z > 0.0 && out.x.abs() < 1e-6, "出射面应为 +Z 面");
    }

    /// 域内任意点：出射点必在盒面上且返回单位向量（域内不变量）。
    #[test]
    fn parallax_exit_point_lies_on_box_surface() {
        let e = Vec3::new(1.5, 1.0, 2.0);
        for p in [
            Vec3::new(0.2, 0.1, -0.3),
            Vec3::new(-1.4, 0.9, 1.9),
            Vec3::new(0.0, 0.0, 1.99),
        ] {
            for d in [
                Vec3::new(1.0, 1.0, 1.0),
                Vec3::new(-1.0, 0.5, 0.25),
                Vec3::new(0.0, -1.0, 0.0),
            ] {
                let dir = d.normalize();
                let out = parallax_direction(dir, p, e);
                assert!(close(out.length(), 1.0), "返回方向未归一化：{out:?}");
                // 反推出射点：q = s·out，s 由「命中面」反解；直接验证方向与
                // 解析交点方向一致更稳——重新求一次 t 并比较
                let mut t_min = f32::INFINITY;
                for axis in 0..3 {
                    let dd = dir[axis];
                    if dd == 0.0 {
                        continue;
                    }
                    let bound = if dd > 0.0 { e[axis] } else { -e[axis] };
                    t_min = t_min.min((bound - p[axis]) / dd);
                }
                let q = p + t_min * dir;
                assert!(close3(out, q.normalize()));
                let ratio = (q.x.abs() / e.x).max(q.y.abs() / e.y).max(q.z.abs() / e.z);
                assert!(close(ratio, 1.0), "出射点不在盒面：q = {q:?}，比值 {ratio}");
            }
        }
    }

    // ---- 选择裁决 ----

    #[test]
    fn distance_to_box_is_zero_inside_and_exact_outside() {
        let e = Vec3::new(1.0, 2.0, 3.0);
        assert_eq!(distance_to_box(Vec3::ZERO, Vec3::ZERO, e), 0.0);
        assert_eq!(
            distance_to_box(Vec3::new(0.9, 1.9, 2.9), Vec3::ZERO, e),
            0.0
        );
        // 单轴外扩：距离 = 越界量
        assert_eq!(
            distance_to_box(Vec3::new(4.0, 0.0, 0.0), Vec3::ZERO, e),
            3.0
        );
        // 双轴越界：欧氏合成（3-4-5）
        assert_eq!(
            distance_to_box(Vec3::new(4.0, 6.0, 0.0), Vec3::ZERO, e),
            5.0
        );
        // 带中心偏移的调用口径
        assert_eq!(
            distance_to_box(Vec3::new(10.0, 0.0, 0.0), Vec3::new(5.0, 0.0, 0.0), e),
            4.0
        );
    }

    /// 选择键：（相机距离升序, 实体位模式升序），返回序 = 实体位模式升序。
    #[test]
    fn probe_selection_is_order_independent() {
        let candidates = [
            ProbeCandidate {
                entity_bits: 0x30,
                camera_distance: 1.0,
            },
            ProbeCandidate {
                entity_bits: 0x10,
                camera_distance: 1.0,
            },
            ProbeCandidate {
                entity_bits: 0x20,
                camera_distance: 0.5,
            },
        ];
        let picked = select_active_probes(&candidates, 2);
        // 距离最近的是 0x20（0.5），其次是并列 1.0 的 0x10 / 0x30 中位模式小者
        assert_eq!(picked, vec![1, 2], "选中 {{0x10, 0x20}} 且按位模式升序");
        // 乱序插入的等价世界：同一集合（顺序不同）选中同一批
        let shuffled = [candidates[2], candidates[0], candidates[1]];
        let picked_shuffled = select_active_probes(&shuffled, 2);
        let mut bits: Vec<u64> = picked_shuffled
            .iter()
            .map(|&i| shuffled[i].entity_bits)
            .collect();
        let mut expected: Vec<u64> = picked.iter().map(|&i| candidates[i].entity_bits).collect();
        bits.sort_unstable();
        expected.sort_unstable();
        assert_eq!(bits, expected, "乱序插入选中同一批探针");
    }

    #[test]
    fn probe_selection_keeps_all_within_budget() {
        let candidates: Vec<ProbeCandidate> = (0..3)
            .map(|i| ProbeCandidate {
                entity_bits: 0x100 - i,
                camera_distance: i as f32,
            })
            .collect();
        // 预算充足：全选，仍按实体位模式升序（0xFE < 0xFF < 0x100 ⇒ 2,1,0）
        let picked = select_active_probes(&candidates, MAX_REFLECTION_PROBES);
        assert_eq!(picked, vec![2, 1, 0]);
        // 预算不足：只取相机距离最近的（i = 0，距离 0）
        let picked = select_active_probes(&candidates, 1);
        assert_eq!(picked, vec![0], "预算 1 时取距离最近的探针");
    }

    // ---- 槽位登记 ----

    #[test]
    fn probe_store_allocates_slots_once_and_is_idempotent() {
        let mut store = ProbeStore::new();
        assert!(store.is_empty());
        assert_eq!(store.ensure_slot(0xaa), Some(0));
        assert_eq!(store.ensure_slot(0xbb), Some(1));
        assert_eq!(store.ensure_slot(0xaa), Some(0), "重复烘焙复用原槽位");
        assert_eq!(store.slot_of(0xbb), Some(1));
        assert_eq!(store.slot_of(0xcc), None);
        assert_eq!(store.len(), 2);
        // 预算耗尽后不再分配（warn 由调用方负责）
        for i in 2..MAX_REFLECTION_PROBES {
            assert_eq!(store.ensure_slot(0x1000 + i as u64), Some(i));
        }
        assert_eq!(store.ensure_slot(0xffff), None);
        assert_eq!(store.len(), MAX_REFLECTION_PROBES);
        assert!(store.memory_bytes() > 0);
    }

    // ---- 组件语义 ----

    #[test]
    fn probe_component_validity_and_defaults() {
        let probe = ReflectionProbe::new(Vec3::new(3.0, 2.5, 4.0));
        assert!(probe.is_valid());
        assert_eq!(probe.falloff, PROBE_DEFAULT_FALLOFF);
        assert!(!ReflectionProbe::new(Vec3::new(0.0, 1.0, 1.0)).is_valid());
        assert!(!ReflectionProbe::new(Vec3::new(-1.0, 1.0, 1.0)).is_valid());
        assert!(!ReflectionProbe::new(Vec3::new(f32::NAN, 1.0, 1.0)).is_valid());
        // 负 falloff 钳到 0
        assert_eq!(ReflectionProbe::default().with_falloff(-2.0).falloff, 0.0);
    }

    // ---- 捕获阴影拟合 ----

    /// 拟合出的光空间矩阵把探针盒 8 角映射进 NDC 立方体（[−1,1]² × [0,1]）。
    #[test]
    fn probe_shadow_fit_contains_box_corners() {
        let center = Vec3::new(1.0, 2.0, -3.0);
        let e = Vec3::new(3.0, 2.0, 4.0);
        let fit = fit_probe_shadow(Vec3::new(-0.5, -1.0, -0.3), center, e);
        for sx in [-1.0f32, 1.0] {
            for sy in [-1.0f32, 1.0] {
                for sz in [-1.0f32, 1.0] {
                    let corner = center + e * Vec3::new(sx, sy, sz);
                    let clip = fit.view_proj * corner.extend(1.0);
                    let ndc = clip.truncate() / clip.w;
                    assert!(
                        ndc.x.abs() <= 1.0 + 1e-4
                            && ndc.y.abs() <= 1.0 + 1e-4
                            && (0.0..=1.0 + 1e-4).contains(&ndc.z),
                        "角点 {corner:?} 落在拟合域外：ndc = {ndc:?}"
                    );
                }
            }
        }
        assert!(fit.texel_world > 0.0 && fit.texel_world.is_finite());
        assert!(fit.depth_window > 0.0);
    }

    /// 纹素世界尺寸 = 纹素对齐后的 XY 跨度最大值 / 2048：取接近竖直的光方向
    /// （上方向回退到 +X 的正交面集），光空间 XY 恰为世界 (z, x) 跨度
    /// (8, 6) 米 ⇒ 纹素世界尺寸 = 8 / 2048（x 向 extnet = 8/2048 = 1/256 是
    /// 2 的幂，整数坐标精确落格，故是可断言的精确值）。
    #[test]
    fn probe_shadow_fit_texel_size_matches_aligned_span() {
        let center = Vec3::new(1.0, 2.0, -3.0);
        let e = Vec3::new(3.0, 2.0, 4.0);
        let fit = fit_probe_shadow(Vec3::new(0.0, -1.0, 0.0), center, e);
        assert_eq!(fit.texel_world, 8.0 / SHADOW_MAP_SIZE as f32);
        // 深度窗口 = 盒沿光轴跨度（y: 4 m）+ 两侧各一个包围球半径（保守外扩）。
        // 纹素对齐只把 XY 的 min 向下 / max 向上取整（各最多一个纹素，
        // y 向纹素 = 6/2048 ≈ 0.0029 m），故窗口**不小于**未对齐值、且最多
        // 多出约 2 × 对角纹素 ≈ 0.012 m。
        let radius = Vec3::new(8.0, 6.0, 4.0).length() * 0.5;
        let ideal = 4.0 + 2.0 * radius;
        assert!(
            fit.depth_window >= ideal && fit.depth_window <= ideal + 0.02,
            "深度窗口 {} 应在 [{ideal}, {}] 内（纹素对齐只增不减）",
            fit.depth_window,
            ideal + 0.02
        );
    }
}
