//! 灯光资源与组件。

use glam::{Mat4, Vec2, Vec3, Vec3Swizzles};
use iris_core::{Component, Resource};

use crate::pipeline::{SHADOW_CASCADES, SHADOW_DISTANCE, SHADOW_MAP_SIZE};

/// 单帧允许的最大点光数量（uniform 数组容量，超出部分被忽略并记录日志）。
pub const MAX_POINT_LIGHTS: usize = 8;

/// 点光阴影**预算**（R61）：允许同时投射 cubemap 阴影的点光盏数上限。
///
/// **取 4 的理由**：单盏 cubemap 的分辨率与 v1 完全一致（512² × 6 面，
/// 逐灯质量零回归），代价只随盏数线性增长——4 盏 = 每帧 24 个阴影 pass
/// 与 48 MiB 显存（24 MiB `R32Float` 颜色 + 24 MiB `Depth32Float`，
/// v1 单盏 12 MiB，净增 +36 MiB）；对照方向光 CSM 的固定开销
/// （2048² × 3 层双附件 = 96 MiB），点光阴影总预算控制在 CSM 的一半，
/// 「低占用」卖点不失守。预算**常量而非资源**：显存分配发生在
/// `Renderer::new`（纹理数组层数固定），做成运行时可调需要重建纹理与
/// 全部 24 个面视图 / 绑定组，收益（多一盏少一盏）远小于复杂度；
/// 超出预算的灯由 [`select_point_shadow_casters`] 按确定性选择键裁掉并
/// 记录日志（不是静默丢弃）。
pub const POINT_SHADOW_BUDGET: usize = 4;

// ═══════════════════════════════════════════════════════════════════════
// 阴影偏移 v2（R61）：坡度缩放 bias + 法线偏移
//
// R60 之前两处阴影都是**常数 bias**（CSM 0.0015 NDC、点光 0.03 m）：
// 常数必须按最坏情况（掠射角）取值，于是平地上吃下与掠射角同样的偏移
// ——peter-panning 与「接触阴影浮起」的根源（ARCHITECTURE §8.8 曾列为
// 已知限制）。v2 把偏移拆成两项：
//
//   1. **坡度缩放 bias**：`bias ∝ tan(acos(NdotL))`——
//      经典推导：接收面相对光源平面的倾角 θ 使「一个阴影图纹素跨度内的
//      深度变化」= `texel_world · tanθ`；用最近邻采样时片段与纹素中心的
//      光空间 XY 相差最多半纹素、3×3 PCF 最多 1.5 纹素（对角样本到
//      1.41 纹素），所以要压住自遮挡，NDC 深度容差须 ≥
//      `SLOPE_SCALE · (texel_world / depth_window) · tanθ`。
//      与常数项取 max/相加并 clamp 到 [下限, 上限]：下限 = 常数 bias
//      （平地与数值噪声的兜底，完全去掉常数会复发平地 acne），上限
//      限制 tanθ → ∞（掠射极端）时的 peter-panning。
//   2. **法线偏移（normal offset）**：采样点沿**几何法线**推移
//      `texel_world · NORMAL_SCALE · (1 − NdotL)`。它同时移动采样点的
//      光空间 XY 与深度：对掠射面（NdotL → 0）把采样点推离接收面，
//      等效深度余量随 tanθ 增长，正是坡度项之外的第二道保险；
//      `(1 − NdotL)` 因子保证垂直入射时偏移归零（不引入接触阴影浮起）。
//      上限 `SHADOW_NORMAL_MAX_WORLD` 约束远级联（纹素世界尺寸 ~0.09 m）
//      的偏移量，避免远级联接触阴影被推走。
//
// 几何法线（而非法线贴图扰动后的着色法线）参与这两项：深度误差由
// **光栅化几何**决定，扰动后的法线逐纹素高频抖动只会给偏移量注入噪声
// （凹凸面上出现漏光条纹）。
// ═══════════════════════════════════════════════════════════════════════

/// 方向光阴影 bias 的常数下限（NDC 深度单位）。
///
/// v1 的常数 0.0015 必须按掠射角取值；v2 把它降到 0.0004 —— 掠射角由
/// 坡度项与法线偏移承担，平地上的固定 peter-panning 随之缩小到约 1/4。
/// 保留下限而非归零：R32Float 深度虽精确，但「原地平面对自身的最近邻
/// 比较」仍有浮点与光栅化量化残差（实测归零后平地上出现零星 acne）。
pub const SHADOW_BIAS_MIN: f32 = 0.0004;

/// v1 的常数 bias（NDC）：R61 起仅由**对照模式**使用
/// （`Renderer::set_legacy_shadow_bias`，归因测试与验收图左侧面板）。
pub const SHADOW_BIAS_LEGACY: f32 = 0.0015;

/// 坡度缩放 bias 的系数（无量纲）：乘 `texel_world / depth_window · tanθ`。
///
/// 1.5 的来历：3×3 PCF 的对角样本距片段最多 1.41 纹素（`√2`），取 1.5
/// 留浮点与纹素对齐（`snap_aabb_to_texels` 只扩不缩，实际纹素略大于
/// 拟合值）的余量；理论下界 `√2` 与实测（掠射角场景 0 像素 acne）
/// 一致。
pub const SHADOW_SLOPE_SCALE: f32 = 1.5;

/// 方向光阴影 bias 的上限（NDC 深度单位）。
///
/// `tanθ` 在 NdotL → 0 时发散（法线偏移同样发散），不设上界会让掠射面
/// 的阴影整体退化成「无影」。0.02 ≈ 下限的 50 倍，实测只在大入射角
/// （NdotL < 0.1）逼近。
pub const SHADOW_BIAS_MAX: f32 = 0.02;

/// 法线偏移系数（无量纲）：乘 `texel_world · (1 − NdotL)`。
pub const SHADOW_NORMAL_SCALE: f32 = 1.5;

/// 法线偏移的世界空间上限（米）：约束远级联（纹素 ~0.09 m）的偏移量。
pub const SHADOW_NORMAL_MAX_WORLD: f32 = 0.15;

/// `tan(acos(NdotL))` 的数值上限（防 0 除与 Inf 传播：NdotL → 0 时
/// `sqrt(1−x²)/x` 发散，clamp 后保证 bias 有界且 `0 × Inf` 不产生 NaN
/// ——对照模式下坡度系数为 0，NaN 会让整帧变黑）。
pub const SHADOW_TAN_MAX: f32 = 64.0;

/// 点光阴影 bias 的常数下限（米）。
///
/// v1 的 0.03 m 是为「距离值域大 + 掠射面」一次取足的常数；v2 降到
/// 0.005 m（归一化值域下 0.005/range），掠射角由坡度项承担。
pub const POINT_SHADOW_BIAS_MIN: f32 = 0.005;

/// v1 的点光常数 bias（米）：R61 起仅由对照模式使用。
pub const POINT_SHADOW_BIAS_LEGACY: f32 = 0.03;

/// 点光阴影 bias 的米制上限（防掠射极端退化为无影）。
pub const POINT_SHADOW_BIAS_MAX: f32 = 0.5;

/// 点光坡度缩放系数（无量纲）：乘 `texel_world(dist) · tanθ`（米）。
///
/// 点光阴影是**单采样**（无 PCF），纹素跨度内的深度变化上限就是
/// 1 纹素 · tanθ，故系数取 1.0（方向光因 3×3 PCF 取 1.5）。
pub const POINT_SHADOW_SLOPE_SCALE: f32 = 1.0;

/// 点光法线偏移系数（无量纲）：乘 `texel_world(dist) · (1 − NdotL)`。
pub const POINT_SHADOW_NORMAL_SCALE: f32 = 1.0;

/// 点光法线偏移的世界空间上限（米）。
pub const POINT_SHADOW_NORMAL_MAX_WORLD: f32 = 0.15;

/// `tan(acos(NdotL))` 的保守取值（clamp 到 [`SHADOW_TAN_MAX`]）。
///
/// 与 WGSL 侧 `slope_tan` 同式同序（同样的 `sqrt` + `min/max`），
/// 单测钉住分段行为（单调、端点、clamp）。
pub fn slope_tan(nol: f32) -> f32 {
    let n = nol.max(0.0);
    ((1.0 - n * n).max(0.0).sqrt() / n.max(1e-4)).min(SHADOW_TAN_MAX)
}

/// 方向光阴影的 NDC 深度容差（bias）——坡度缩放 + 常数下限 + 上限。
///
/// ```text
/// bias = clamp(BIAS_MIN + SLOPE_SCALE · (texel_world / depth_window) · tanθ,
///              BIAS_MIN, BIAS_MAX)
/// ```
///
/// `texel_world` = 该级联阴影图一个纹素的世界边长（米），
/// `depth_window` = 该级联正交投影的深度窗口宽度（米，NDC 0..1 跨一个
/// 窗口）——两者都由 [`cascade_fit_params`] 逐级联给出（级联越远纹素越
/// 大、窗口越宽，比值随之变化，这正是「偏移量逐级联缩放」的载体）。
pub fn shadow_bias_ndc(nol: f32, texel_world: f32, depth_window: f32) -> f32 {
    let slope = SHADOW_SLOPE_SCALE * (texel_world / depth_window.max(1e-6)) * slope_tan(nol);
    (SHADOW_BIAS_MIN + slope).clamp(SHADOW_BIAS_MIN, SHADOW_BIAS_MAX)
}

/// 方向光阴影的法线偏移量（世界米）：`NORMAL_SCALE · texel_world · (1 − NdotL)`，
/// 上限 [`SHADOW_NORMAL_MAX_WORLD`]。
pub fn shadow_normal_offset_world(nol: f32, texel_world: f32) -> f32 {
    (SHADOW_NORMAL_SCALE * texel_world * (1.0 - nol.max(0.0))).min(SHADOW_NORMAL_MAX_WORLD)
}

/// 点光 cubemap 的纹素世界边长（米）：90° 面视锥摊在
/// [`crate::pipeline::POINT_SHADOW_MAP_SIZE`] 个纹素上 ⇒ 距离 d 处
/// `d · 2 / 512 = d / 256`。
pub fn point_shadow_texel_world(dist: f32) -> f32 {
    dist * 2.0 / crate::pipeline::POINT_SHADOW_MAP_SIZE as f32
}

/// 点光阴影的米制 bias——与 [`shadow_bias_ndc`] 同构，只是「深度」为
/// 线性距离（米）而非 NDC：`clamp(MIN + SLOPE · texel_world · tanθ, MIN, MAX)`。
/// 着色器侧把它除以 range 归一化到贴图存储的距离值域（口径与 v1 一致）。
pub fn point_shadow_bias_meters(nol: f32, dist: f32) -> f32 {
    let slope = POINT_SHADOW_SLOPE_SCALE * point_shadow_texel_world(dist) * slope_tan(nol);
    (POINT_SHADOW_BIAS_MIN + slope).clamp(POINT_SHADOW_BIAS_MIN, POINT_SHADOW_BIAS_MAX)
}

/// 点光阴影的法线偏移量（世界米）。
pub fn point_shadow_normal_offset_world(nol: f32, dist: f32) -> f32 {
    (POINT_SHADOW_NORMAL_SCALE * point_shadow_texel_world(dist) * (1.0 - nol.max(0.0)))
        .min(POINT_SHADOW_NORMAL_MAX_WORLD)
}

/// 点光阴影投射器的一个候选（选择键的输入）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointShadowCandidate {
    /// 灯实体句柄的位模式（`Entity::to_bits`）——选择键的**最终**并列断言，
    /// 保证全序与收集顺序无关。
    pub entity_bits: u64,
    /// 灯世界位置到相机 eye 的距离（米）。
    pub eye_distance: f32,
}

/// 预算化点光阴影的**确定性选择键**（R61）。
///
/// 键 = （相机距离升序, 实体位模式升序）——距离越近的灯其阴影越可能落在
/// 画面中心与近处（点光 cubemap 的角度分辨率与距离无关，屏幕空间重要性
/// 却随距离迅速下降），距离并列时以实体位模式收尾使比较器成为**全序**：
/// 结果只取决于灯集合本身，与查询 / 插入顺序无关（乱序插入的两次世界
/// 选中同一批灯，`shadow_v2_render.rs` 的确定性测试守护）。
///
/// 返回选中候选在入参切片中的下标，已按选择键升序（= cubemap 数组槽位
/// 序）。`candidates.len() <= budget` 时全选——单盏阴影场景因此与 v1
/// 选中同一盏灯（v1 取查询序首盏；只有一盏时两者必然一致）。
pub fn select_point_shadow_casters(
    candidates: &[PointShadowCandidate],
    budget: usize,
) -> Vec<usize> {
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    order.sort_by(|&a, &b| {
        candidates[a]
            .eye_distance
            .total_cmp(&candidates[b].eye_distance)
            .then_with(|| candidates[a].entity_bits.cmp(&candidates[b].entity_bits))
    });
    order.truncate(budget);
    order
}

/// 级联分割的 PFS（Practical Split Scheme）混合系数 λ：`split_i` 由
/// 对数分割（近处密、远处疏，精度最优）与线性分割（均匀，远级联不至
/// 过宽）按 `λ` 权重混合——0.6 是大场景下近级联分辨率与远级联覆盖的
/// 常用折中（v1 常量，不随场景自适应）。
pub const SHADOW_SPLIT_LAMBDA: f32 = 0.6;

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
    /// 是否投射阴影（方向光级联阴影 CSM，PCF 软化）。
    pub shadows: bool,
    /// 阴影覆盖半径（世界单位）：以相机注视点为中心的正方形边长的一半。
    ///
    /// 仅被 v1 单级联工具方法 [`DirectionalLight::shadow_view_proj`] 消费；
    /// R29 起生产路径为级联阴影（CSM），覆盖距离由
    /// [`crate::pipeline::SHADOW_DISTANCE`] 常量决定、级联范围由相机视锥
    /// 深度片拟合，本字段不再参与每帧渲染（保留供场景序列化兼容与
    /// 单级联参照实现）。
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
    /// 半边长 `range` 的立方区域（v1 单级联阴影的参照实现）。
    ///
    /// R29 起方向光阴影的生产路径是级联阴影（CSM）：每帧按相机视锥
    /// 深度片用 [`cascade_fit`] 逐级联拟合（见 ARCHITECTURE §4.7.2），
    /// 本方法不再是每帧渲染路径，保留作纯数学工具与既有单测的基线。
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

/// 级联深度分割点（实用分割 PFS，Practical Split Scheme）。
///
/// 返回 `SHADOW_CASCADES` 个**沿相机 forward 的距离**（正数，递增），
/// 第 i 级级联覆盖视锥深度片 `[splits[i-1], splits[i]]`（`splits[-1]`
/// 即 `near`）。分割点 = 对数分割与线性分割按
/// `λ = [`SHADOW_SPLIT_LAMBDA`]` 混合：
///
/// ```text
/// log_i = near · (far/near)^(i/N)      （近处密，精度最优）
/// lin_i = near + (far − near) · i/N    （均匀，远级联不失控）
/// split_i = (1 − λ) · log_i + λ · lin_i
/// ```
///
/// 级联只铺到阴影覆盖距离：`far` 先按
/// [`SHADOW_DISTANCE`]（120 m）钳制，覆盖带之外的片元不投影、保持受光。
/// `near` 取 `min(z_near, 1.0)`：过小的近平面会把对数分割全部压向
/// 零点，首级联退化（1 m 内的几何直接并入次近平面，阴影质量无感差异）。
pub fn cascade_splits(z_near: f32, z_far: f32) -> [f32; SHADOW_CASCADES] {
    // 下限 1e-4 防御 z_near=0 的除零 NaN（正常相机不会触发）。
    // 用 min/max 而非 clamp：f32::min/max 遇 NaN 返回另一操作数，
    // NaN 相机参数在这里收敛为 1.0 而非向外传播 NaN。
    #[allow(clippy::manual_clamp)]
    let near = z_near.min(1.0).max(1e-4);
    let far = z_far.min(SHADOW_DISTANCE);
    let count = SHADOW_CASCADES as f32;
    let mut splits = [0.0f32; SHADOW_CASCADES];
    for (i, split) in splits.iter_mut().enumerate() {
        let fi = (i + 1) as f32;
        let log = near * (far / near).powf(fi / count);
        let lin = near + (far - near) * fi / count;
        *split = (1.0 - SHADOW_SPLIT_LAMBDA) * log + SHADOW_SPLIT_LAMBDA * lin;
    }
    splits
}

/// 把光空间 AABB 的 XY 边界对齐到纹素网格（min 向下取整、max 向上取整，
/// 盒只扩不缩）。
///
/// **纹素对齐 = 消相机移动阴影闪烁的关键**：正交投影下片元的阴影图 UV
/// 为 `uv = (dot(p, u) − min_xy) / extent_xy`（u 为光空间右轴）。相机
/// 平移时 AABB 随之连续平移，`min_xy` 若不量化，UV 会以亚纹素步进连续
/// 变化——阴影图采样相位逐帧漂移，阴影边缘出现爬行锯齿（shimmer）。
/// 把 `min_xy / max_xy` round 到 `texel = extent / SHADOW_MAP_SIZE` 的
/// 整数格点上后，投影矩阵的平移量恒为纹素整数倍：世界点对应的 UV 只会
/// 整纹素跳变，采样相位稳定（本函数的关键性质：对齐后
/// `max − min` 恰为 texel 的整数倍，由 min 向下 / max 向上取整保证，
/// 顺带杜绝「round 到最近格点」可能出现的半纹素收缩裁掉边缘几何）。
/// Z（深度）不对齐——深度只影响比较精度，不产生可见相位漂移。
/// `extent` 为 0 的退化轴原样返回（除零保护，正常视锥片不会出现）。
pub fn snap_aabb_to_texels(min: Vec2, max: Vec2, map_size: u32) -> (Vec2, Vec2) {
    let size = map_size as f32;
    let extent = max - min;
    let texel = extent / size;
    let snap = |v: f32, t: f32, round_down: bool| {
        if t > 0.0 {
            let cells = v / t;
            let grid = if round_down {
                cells.floor()
            } else {
                cells.ceil()
            };
            grid * t
        } else {
            v
        }
    };
    (
        Vec2::new(snap(min.x, texel.x, true), snap(min.y, texel.y, true)),
        Vec2::new(snap(max.x, texel.x, false), snap(max.y, texel.y, false)),
    )
}

/// 单级联拟合的完整产物（R61）：光空间矩阵 + 阴影偏移 v2 需要的两个
/// 世界尺度。
///
/// 矩阵与 R29 的 [`cascade_fit`] 返回值逐位相同（同一算式同一顺序）；
/// 新增的两个尺度只服务于坡度缩放 bias 与法线偏移——**级联越远纹素世界
/// 尺寸越大、深度窗口越宽**，两者相除得到该级联「每单位 tanθ 的 NDC
/// 深度误差」，是偏移量逐级联缩放的唯一依据。
#[derive(Clone, Copy, Debug)]
pub struct CascadeFit {
    /// 光空间视图投影矩阵（世界 → 本级联光源裁剪空间）。
    pub view_proj: Mat4,
    /// 一个阴影图纹素的世界边长（米）：XY 两轴取较大者（保守——3×3 PCF
    /// 的偏移在 UV 两轴上同尺度，世界位移由较大的纹素主导），
    /// 用纹素对齐**之后**的 AABB 跨度计算。
    pub texel_world: f32,
    /// 正交投影深度窗口宽度（米）：NDC 0..1 对应 `depth_window` 米，
    /// 故世界偏移 → NDC 偏移的换算系数是 `1 / depth_window`。
    pub depth_window: f32,
}

/// 单级联的正交拟合（CSM 核心，纯函数）：取相机视锥深度片
/// `[z_start, z_end]`（沿相机 forward 的距离，正数）的 8 个角点，
/// 变换到光空间求 AABB，纹素对齐 XY 后构造正交视图投影矩阵。
///
/// - **光视图朝向**与 v1 `shadow_view_proj` 完全一致（look_at 沿光方向、
///   接近竖直时光方向回退到 +X），且光源眼点固定在世界原点——正交投影
///   沿光轴平移眼点只改变深度窗口的绝对位置、不改变 XY 映射，而深度
///   窗口宽度（决定 bias 的世界偏移与精度）只由 AABB 尺寸决定，与相机
///   离世界原点多远无关；
/// - **纹素对齐**：XY 走 [`snap_aabb_to_texels`]（原理见该函数文档），
///   对齐网格锚定在世界原点的光空间投影上，与相机位置解耦；
/// - **深度窗口**：以 AABB 包围球半径 `r` 为pad，向光方向与背离光方向
///   各外扩 `r`（`near = l_min − r`、`far = l_max + r`，`l` 为沿光方向
///   深度）。外扩保证切片内几何之外的遮挡物（如高塔向切片内投影）与
///   切片 XY 足印内的地面都落在深度窗口内；窗口宽 = 切片深度跨度 +
///   2r，与常数偏移相乘的世界偏移量因此有界。
///
/// R61 起返回 [`CascadeFit`]（矩阵 + 两个世界尺度）；只要矩阵的调用方用
/// [`cascade_fit`]（保持 R29 签名与返回值逐位不变）。
pub fn cascade_fit_params(
    light_direction: Vec3,
    view: &Mat4,
    fov_y_radians: f32,
    aspect: f32,
    z_start: f32,
    z_end: f32,
) -> CascadeFit {
    let dir = light_direction.normalize_or_zero();
    // 上方向与光方向接近平行时换用 +X（与 shadow_view_proj 同规则）
    let up = if dir.y.abs() > 0.99 { Vec3::X } else { Vec3::Y };
    // 眼点 = 世界原点、注视方向 = 光方向的纯旋转视图
    let light_view = glam::camera::rh::view::look_at_mat4(Vec3::ZERO, dir, up);
    let inv_cam = view.inverse();
    let tan_half = (fov_y_radians * 0.5).tan();
    // 深度片 8 角：视图空间 z = −d（DirectX/RH 约定），按 fov/aspect 撑开
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for d in [z_start, z_end] {
        let half_h = tan_half * d;
        let half_w = half_h * aspect;
        for sx in [-1.0, 1.0] {
            for sy in [-1.0, 1.0] {
                let world = inv_cam.transform_point3(Vec3::new(sx * half_w, sy * half_h, -d));
                let ls = light_view.transform_point3(world);
                min = min.min(ls);
                max = max.max(ls);
            }
        }
    }
    // 纹素对齐 XY（Z 不对齐，见 snap_aabb_to_texels 文档）
    let (min_xy, max_xy) = snap_aabb_to_texels(min.xy(), max.xy(), SHADOW_MAP_SIZE);
    let min = Vec3::new(min_xy.x, min_xy.y, min.z);
    let max = Vec3::new(max_xy.x, max_xy.y, max.z);
    // 深度窗口：向光 / 背光各外扩一个包围球半径（l = 沿光方向深度 = −视图 z）
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

/// 单级联正交拟合（R29 签名，返回光空间矩阵）——[`cascade_fit_params`]
/// 的薄包装，R61 之后生产路径改用后者（多取两个世界尺度）。
pub fn cascade_fit(
    light_direction: Vec3,
    view: &Mat4,
    fov_y_radians: f32,
    aspect: f32,
    z_start: f32,
    z_end: f32,
) -> Mat4 {
    cascade_fit_params(light_direction, view, fov_y_radians, aspect, z_start, z_end).view_proj
}

/// 点光源组件。位置取自同一实体的 `GlobalTransform`（层级系统每帧更新）。
///
/// 衰减模型：`saturate(1 - d/range)²`，在 `range` 处平滑归零，
/// 颜色 × 强度决定亮度。超过 [`MAX_POINT_LIGHTS`] 的点光会被忽略。
///
/// 阴影（R61 起为**预算化多盏**）：最多 [`POINT_SHADOW_BUDGET`] 盏
/// `shadows == true` 的点光同时投射 cubemap 阴影，逐灯 6 个捕获 pass +
/// 一张 512² 面；超出预算的灯由（相机距离升序, 实体位模式升序）选择键
/// 裁掉并 `log::warn`（选择键定义见 [`select_point_shadow_casters`]）。
/// 只有一盏开启时选中结果与 v1 的「查询序首盏」一致。
#[derive(Clone, Copy, Debug, Component)]
pub struct PointLight {
    /// 光的颜色（线性空间）。
    pub color: Vec3,
    /// 强度倍率。
    pub intensity: f32,
    /// 有效半径（世界单位），超出后无贡献。
    pub range: f32,
    /// 是否投射 cubemap 阴影（预算 [`POINT_SHADOW_BUDGET`] 盏内生效）。
    pub shadows: bool,
}

impl Default for PointLight {
    fn default() -> Self {
        Self {
            color: Vec3::ONE,
            intensity: 8.0,
            range: 12.0,
            shadows: false,
        }
    }
}

impl PointLight {
    pub fn new(color: Vec3, intensity: f32, range: f32) -> Self {
        Self {
            color,
            intensity,
            range,
            shadows: false,
        }
    }

    /// 点光某一面的视图投影矩阵（90° 视锥、宽高比 1、near 0.05、far = range）。
    ///
    /// 面序与观察方向取 [`POINT_SHADOW_FACES`]（wgpu 立方体贴图层序）。
    /// 返回矩阵已对 NDC y 取反：wgpu 光栅化把 NDC y=+1 写进贴图第 0 行，
    /// 而 Cube 采样按 OpenGL 面内约定 `t = (1 − dir_分量)/2` 查 texel——
    /// 不翻转会让每个面的内容垂直镜像，遮挡物落错 texel（推导见
    /// ARCHITECTURE §4.7.1；翻转只翻 y 行，深度 z 与横向 u 均不受影响，
    /// 三角形环绕方向反转由阴影管线的 cull None 吸收）。
    pub fn face_view_proj(light_pos: Vec3, face: usize, range: f32) -> Mat4 {
        cube_face_view_proj(light_pos, face, POINT_SHADOW_NEAR, range)
    }
}

/// 锥光源组件（R78，前向光源分箱路径）。位置取自同一实体的
/// `GlobalTransform`（层级系统每帧更新）。
///
/// 形状语义：光只落在以 `direction` 为轴的圆锥内——`inner_cutoff` 内全强，
/// `inner_cutoff` → `outer_cutoff` 之间线性衰减到 0，锥外无贡献
/// （`spot_cone_factor`，CPU/WGSL 同式）。衰减与覆盖同 [`PointLight`]：
/// `saturate(1 - d/range)²` 平方衰减 + `range` 平滑窗口。
///
/// **诚实限制（v1，详见 ARCHITECTURE §8.47）**：
/// - `direction` 是**显式世界空间指向**，不随 `Transform` 旋转（跟随旋转
///   需要逐帧读层级，v1 让作者直接改本字段）；
/// - 不投影阴影（点光 cubemap 阴影不覆盖锥光）；
/// - 经屏幕空间分箱路径渲染（[`crate::light_bins`]），反射探针与 RT 相机
///   的捕获视口不渲染锥光（捕获恒绑哑灯表）。
/// - 零长度 `direction` = 不发光（渲染器把颜色置零，锥光因子无从定义）；
/// - `inner_cutoff > outer_cutoff` 由实现层钳到硬边（inner = outer），
///   组件本身原样存储两个值。
#[derive(Clone, Copy, Debug, Component)]
pub struct SpotLight {
    /// 光的颜色（线性空间）。
    pub color: Vec3,
    /// 强度倍率。
    pub intensity: f32,
    /// 有效半径（世界单位），超出后无贡献（同 [`PointLight`] 衰减模型）。
    pub range: f32,
    /// 锥轴的**世界空间**单位化前的指向（上传时归一化；零长度 = 不发光）。
    pub direction: Vec3,
    /// 全强锥角（弧度）。
    pub inner_cutoff: f32,
    /// 锥外归零角（弧度）。
    pub outer_cutoff: f32,
}

impl Default for SpotLight {
    fn default() -> Self {
        Self {
            color: Vec3::ONE,
            intensity: 8.0,
            range: 12.0,
            direction: Vec3::new(0.0, -1.0, 0.0),
            inner_cutoff: 30.0f32.to_radians(),
            outer_cutoff: 45.0f32.to_radians(),
        }
    }
}

impl SpotLight {
    /// 逐字段构造（角度收弧度）。
    pub fn new(
        color: Vec3,
        intensity: f32,
        range: f32,
        direction: Vec3,
        inner_cutoff: f32,
        outer_cutoff: f32,
    ) -> Self {
        Self {
            color,
            intensity,
            range,
            direction,
            inner_cutoff,
            outer_cutoff,
        }
    }
}

/// 立方体某一面的视图投影矩阵（点光阴影与反射探针捕获共用）：
/// 90° 透视、宽高比 1，面序与观察方向取 [`POINT_SHADOW_FACES`]
/// （wgpu 立方体贴图层序），返回矩阵已对 NDC y 取反。
///
/// **NDC y 翻转的推导**（R61，反射探针捕获同源复用）：wgpu 光栅化把
/// NDC y=+1 写进贴图第 0 行，而 Cube 采样按 OpenGL 面内约定
/// `t = (1 − dir_分量)/2` 查 texel——不翻转会让每个面的内容垂直镜像，
/// 遮挡物落错 texel（详见 ARCHITECTURE §4.7.1）。翻转只翻 y 行，深度 z 与
/// 横向 u 均不受影响，三角形环绕方向反转由调用方（cull None / 背面剔除
/// 与本翻转的既有组合）吸收。
///
/// 天空盒射线重建同源：`inv(vp) * (ndc, 1, 1)` 恰为「该像素在立方体面上
/// 的方向」——翻转在逆矩阵里被自动抵消，故捕获期的天空与几何严格自洽。
pub fn cube_face_view_proj(center: Vec3, face: usize, near: f32, far: f32) -> Mat4 {
    let (dir, up) = POINT_SHADOW_FACES[face];
    let view = glam::camera::rh::view::look_at_mat4(center, center + dir, up);
    let proj =
        glam::camera::rh::proj::directx::perspective(std::f32::consts::FRAC_PI_2, 1.0, near, far);
    Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0)) * (proj * view)
}

/// 点光 cubemap 阴影的面集：wgpu 立方体贴图 6 层的（观察方向， 上方向）。
///
/// 层序 = OpenGL / wgpu 约定：[+X, −X, +Y, −Y, +Z, −Z]；每面的观察方向为
/// 该层正对的轴向，上方向为标准正交面集——面方向必须与 GPU Cube 采样的
/// 方向约定精确一致，镜像/旋转会让遮挡物落错 texel。
pub const POINT_SHADOW_FACES: [(Vec3, Vec3); 6] = [
    (Vec3::new(1.0, 0.0, 0.0), Vec3::new(0.0, -1.0, 0.0)),
    (Vec3::new(-1.0, 0.0, 0.0), Vec3::new(0.0, -1.0, 0.0)),
    (Vec3::new(0.0, 1.0, 0.0), Vec3::new(0.0, 0.0, 1.0)),
    (Vec3::new(0.0, -1.0, 0.0), Vec3::new(0.0, 0.0, -1.0)),
    (Vec3::new(0.0, 0.0, 1.0), Vec3::new(0.0, -1.0, 0.0)),
    (Vec3::new(0.0, 0.0, -1.0), Vec3::new(0.0, -1.0, 0.0)),
];

/// 点光阴影的近平面（贴上灯面且极近的几何直接裁掉，避免深度精度塌缩）。
pub const POINT_SHADOW_NEAR: f32 = 0.05;

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
        assert!(
            ndc.x.abs() < 1e-4 && ndc.y.abs() < 1e-4,
            "覆盖中心应投影到 NDC 原点"
        );
        // 光源相机沿 -dir 拉远 2×range，覆盖中心的深度应精确落在
        // (distance - near) / (far - near) —— 保证 center ± range 全程在 [0, 1]
        let distance = range * 2.0;
        let expected_z = (distance - 0.1) / (distance + range - 0.1);
        assert!(
            (ndc.z - expected_z).abs() < 1e-4,
            "深度 {ndc:?} 应为 {expected_z}"
        );
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
            assert!(
                ndc.x.abs() <= 1.0 && ndc.y.abs() <= 1.0 && (0.0..=1.0).contains(&ndc.z),
                "角点 {corner:?} 超出阴影覆盖范围：{ndc:?}"
            );
        }
    }

    #[test]
    fn vertical_light_direction_falls_back_up_vector() {
        // 光方向几乎竖直时 look_at 会退化，不应产生 NaN
        let light = DirectionalLight {
            direction: Vec3::NEG_Y,
            ..Default::default()
        };
        let vp = light.shadow_view_proj(Vec3::ZERO, 24.0);
        assert!(
            vp.to_cols_array().iter().all(|v| v.is_finite()),
            "矩阵不应包含 NaN"
        );
    }

    /// 构造测试相机视图：eye → target 的 look_at（与渲染器同约定）。
    fn test_view(eye: Vec3, target: Vec3) -> Mat4 {
        glam::camera::rh::view::look_at_mat4(eye, target, Vec3::Y)
    }

    /// 级联深度片的世界空间 8 角（与 cascade_fit 内部同构，供断言用）。
    fn slice_corners(view: &Mat4, fov: f32, aspect: f32, z_start: f32, z_end: f32) -> [Vec3; 8] {
        let inv = view.inverse();
        let tan_half = (fov * 0.5).tan();
        let mut corners = [Vec3::ZERO; 8];
        let mut n = 0;
        for d in [z_start, z_end] {
            let hh = tan_half * d;
            let hw = hh * aspect;
            for sx in [-1.0, 1.0] {
                for sy in [-1.0, 1.0] {
                    corners[n] = inv.transform_point3(Vec3::new(sx * hw, sy * hh, -d));
                    n += 1;
                }
            }
        }
        corners
    }

    /// 分割点严格递增、末点恰为覆盖距离（far 被钳制到 120 m）；far 小于
    /// 覆盖距离时末点 = far（小场景不被硬拉到 120 m）；λ = 0.6 偏向线性
    /// 的混合公式真实生效（首点远大于纯对数分割）。
    #[test]
    fn cascade_splits_are_monotonic_and_bounded() {
        let splits = cascade_splits(0.1, 1000.0);
        assert_eq!(splits.len(), SHADOW_CASCADES);
        for w in splits.windows(2) {
            assert!(w[0] < w[1], "分割点必须严格递增：{splits:?}");
        }
        assert!(splits[0] > 0.1, "首分割点应在近平面之后");
        assert!(
            (splits[SHADOW_CASCADES - 1] - SHADOW_DISTANCE).abs() < 1e-3,
            "远平面被钳制到覆盖距离时末分割点应恰为 {SHADOW_DISTANCE}，实际 {}",
            splits[SHADOW_CASCADES - 1]
        );
        // 小场景：far < 覆盖距离 → 末点 = far，级联不被拉到无谓的 120 m
        let small = cascade_splits(0.1, 50.0);
        assert!((small[SHADOW_CASCADES - 1] - 50.0).abs() < 1e-3);
        // λ = 0.6 偏向线性：首分割点应显著大于纯对数分割（对数分割把
        // 全部密度压向零点），验证混合公式真的生效
        let (near, far) = (0.1f32, SHADOW_DISTANCE);
        let pure_log = near * (far / near).powf(1.0 / SHADOW_CASCADES as f32);
        assert!(
            splits[0] > pure_log * 10.0,
            "PFS(λ=0.6) 首点应远大于纯对数分割 {pure_log}，实际 {}",
            splits[0]
        );
    }

    /// 纹素对齐后 (max − min) 恰为 texel 整数倍、盒只扩不缩、且幂等
    /// （对已对齐盒重复对齐结果不变——渲染器逐帧调用的稳定性前提）。
    #[test]
    fn snap_aabb_to_texels_yields_integer_texel_extents() {
        let cases = [
            (Vec2::new(-13.2721, 4.9182), Vec2::new(7.4415, 21.0337)),
            (Vec2::new(102.5, -3.25), Vec2::new(240.75, 66.125)),
            (Vec2::new(-0.000_4, -0.000_2), Vec2::new(0.000_9, 0.001_1)),
        ];
        for (min, max) in cases {
            let (smin, smax) = snap_aabb_to_texels(min, max, SHADOW_MAP_SIZE);
            let texel = (max - min) / SHADOW_MAP_SIZE as f32;
            for axis in 0..2 {
                // 关键断言：对齐后跨度是 texel 的整数倍
                let span = smax[axis] - smin[axis];
                let cells = span / texel[axis];
                assert!(
                    (cells - cells.round()).abs() < 1e-3,
                    "轴 {axis} 跨度 {span} 应为 texel {texel:?} 的整数倍"
                );
                // 盒只扩不缩：角点采样相位只会整纹素跳变，边缘几何不被裁
                assert!(smin[axis] <= min[axis] + 1e-6 && smax[axis] >= max[axis] - 1e-6);
            }
            // 幂等性（浮点误差界内）：已对齐盒重复对齐，漂移不超过一个
            // texel（格点边界处的除法舍入可能翻到相邻格点，不会更远）
            let (smin2, smax2) = snap_aabb_to_texels(smin, smax, SHADOW_MAP_SIZE);
            let bound = texel + Vec2::splat(1e-6);
            assert!((smin2 - smin).abs().cmple(bound).all());
            assert!((smax2 - smax).abs().cmple(bound).all());
        }
    }

    /// 拟合含住角点：深度片 8 角经拟合矩阵投影后必须落在 NDC 盒内
    /// （XY ∈ [−1,1]、z ∈ [0,1]；ε 容纳纹素对齐的取整与浮点误差）；
    /// 且沿光方向平移的世界点光空间深度单调增大（遮挡比较的前提）。
    #[test]
    fn cascade_fit_contains_slice_corners() {
        let dir = Vec3::new(-0.5, -1.0, -0.4);
        let view = test_view(Vec3::new(2.0, 6.0, 10.0), Vec3::new(-3.0, 0.0, -20.0));
        let (fov, aspect, z0, z1) = (std::f32::consts::FRAC_PI_4, 1.777_8, 0.5, 30.0);
        let vp = cascade_fit(dir, &view, fov, aspect, z0, z1);
        for c in slice_corners(&view, fov, aspect, z0, z1) {
            let clip = vp * c.extend(1.0);
            let ndc = clip.truncate() / clip.w;
            assert!(
                ndc.x.abs() <= 1.0 + 1e-3
                    && ndc.y.abs() <= 1.0 + 1e-3
                    && (-1e-3..=1.0 + 1e-3).contains(&ndc.z),
                "角点 {c:?} 超出级联覆盖：{ndc:?}"
            );
        }
        // 深度单调性：沿光方向平移 → 光空间 ndc z 增大
        let p1 = vp * Vec3::new(0.0, 3.0, 2.0).extend(1.0);
        let p2 = vp * (Vec3::new(0.0, 3.0, 2.0) + dir * 5.0).extend(1.0);
        let z1_ndc = p1.z / p1.w;
        let z2_ndc = p2.z / p2.w;
        assert!(
            z2_ndc > z1_ndc,
            "沿光方向平移应增大光空间深度：{z1_ndc} -> {z2_ndc}"
        );
    }

    /// 纹素对齐的相位稳定性：相机亚纹素平移后，同一世界点的阴影图 UV
    /// 漂移不得超过 2 texel（对齐保证漂移只能整纹素发生；extent 本身的
    /// 连续变化另贡献 ~0.1 texel，2 texel 为含浮点余量的经验上界）——
    /// 这是「相机平移不引起阴影边缘闪烁」的数学形态。
    #[test]
    fn cascade_fit_is_texel_stable_under_subtexel_shift() {
        let dir = Vec3::new(-0.5, -1.0, -0.4);
        let fov = std::f32::consts::FRAC_PI_4;
        let (z0, z1) = (0.5f32, 25.0f32);
        let base_eye = Vec3::new(2.0, 6.0, 10.0);
        let target = Vec3::new(0.0, 0.0, 0.0);
        let vp0 = cascade_fit(dir, &test_view(base_eye, target), fov, 1.0, z0, z1);
        // 切片内的世界参考点（约在深度片中部）
        let probe = base_eye + (target - base_eye).normalize() * ((z0 + z1) * 0.5);
        let project = |vp: &Mat4| {
            let clip = vp * probe.extend(1.0);
            let ndc = clip.truncate() / clip.w;
            ndc.xy() * 0.5 + 0.5 // UV
        };
        let uv0 = project(&vp0);
        let texel = 1.0 / SHADOW_MAP_SIZE as f32;
        // 一组亚纹素量级的相机平移（各方向，幅度 ≪ texel 尺寸 ~8 mm）
        for delta in [
            Vec3::new(0.001, 0.0, 0.0),
            Vec3::new(0.0, 0.001, 0.0),
            Vec3::new(0.0, 0.0, -0.001),
            Vec3::new(-0.0007, 0.0005, 0.0006),
        ] {
            let vp1 = cascade_fit(
                dir,
                &test_view(base_eye + delta, target + delta),
                fov,
                1.0,
                z0,
                z1,
            );
            let uv1 = project(&vp1);
            let drift = (uv1 - uv0).length() / texel;
            assert!(
                drift <= 2.0,
                "亚纹素平移 {delta:?} 后 UV 漂移 {drift:.3} texel，超过 2 texel 上界（采样相位不稳定）"
            );
        }
    }

    // ═══════════════════════════════════════════════════════════════════
    // R61：阴影偏移 v2（坡度缩放 bias + 法线偏移）与预算化选择键
    // ═══════════════════════════════════════════════════════════════════

    /// `cascade_fit_params` 与 R29 的 `cascade_fit` 返回**同一矩阵**
    /// （逐位）：R61 只多取两个世界尺度，拟合算式一个字节都没动。
    #[test]
    fn cascade_fit_params_preserves_matrix_bitwise() {
        let dir = Vec3::new(-0.5, -1.0, -0.4);
        let view = test_view(Vec3::new(2.0, 6.0, 10.0), Vec3::new(-3.0, 0.0, -20.0));
        for (z0, z1) in [(0.1f32, 24.5f32), (24.5, 52.5), (52.5, 120.0)] {
            let m = cascade_fit(dir, &view, std::f32::consts::FRAC_PI_4, 1.777_8, z0, z1);
            let fit = cascade_fit_params(dir, &view, std::f32::consts::FRAC_PI_4, 1.777_8, z0, z1);
            assert_eq!(
                m.to_cols_array(),
                fit.view_proj.to_cols_array(),
                "R61 的拟合产物矩阵必须与 R29 逐位相同"
            );
        }
    }

    /// 级联越远 ⇒ 纹素世界尺寸越大、深度窗口越宽（逐级联缩放的物理
    /// 依据）；texel 恰为「对齐后 XY 跨度 / 2048」（口径自洽）。
    #[test]
    fn cascade_fit_params_exposes_world_scales() {
        let dir = Vec3::new(-0.5, -1.0, -0.4);
        let view = test_view(Vec3::new(0.0, 5.0, 40.0), Vec3::new(0.0, 0.0, -15.0));
        let fits: Vec<CascadeFit> = [(0.1f32, 24.5f32), (24.5, 52.5), (52.5, 120.0)]
            .into_iter()
            .map(|(z0, z1)| {
                cascade_fit_params(dir, &view, std::f32::consts::FRAC_PI_4, 1.0, z0, z1)
            })
            .collect();
        for w in fits.windows(2) {
            assert!(
                w[1].texel_world > w[0].texel_world,
                "远级联纹素世界尺寸必须更大：{} vs {}",
                w[1].texel_world,
                w[0].texel_world
            );
            assert!(
                w[1].depth_window > w[0].depth_window,
                "远级联深度窗口必须更宽：{} vs {}",
                w[1].depth_window,
                w[0].depth_window
            );
        }
        // 近级联纹素 ≈ 视锥切片 2048 分之一量级（36 m 级跨度 → ~17 mm）
        assert!(
            (0.005..0.05).contains(&fits[0].texel_world),
            "近级联纹素世界尺寸 {} 应在毫米~厘米量级",
            fits[0].texel_world
        );
        // 深度窗口恒正（bias 换算的分母）
        assert!(fits.iter().all(|f| f.depth_window > 0.0));
    }

    /// `slope_tan` 的分段行为：端点、单调性、clamp（tan 发散被
    /// SHADOW_TAN_MAX 截断，保证 `系数 × tan` 不产生 Inf/NaN）。
    #[test]
    fn slope_tan_is_monotonic_and_clamped() {
        assert!(slope_tan(1.0) < 1e-3, "垂直入射时 tanθ 应趋于 0");
        // 解析解对照：NdotL = 0.5 → tan(60°) = √3
        assert!((slope_tan(0.5) - 3f32.sqrt()).abs() < 1e-5);
        assert!(
            (slope_tan(0.8) - 0.75).abs() < 1e-5,
            "tan(acos(0.8)) = 0.75"
        );
        let mut prev = f32::INFINITY;
        for i in 0..=20 {
            let nol = i as f32 / 20.0;
            let t = slope_tan(nol);
            assert!(t <= prev, "tanθ 应随 NdotL 增大而减小：{nol} → {t}");
            prev = t;
        }
        assert_eq!(slope_tan(0.0), SHADOW_TAN_MAX, "掠射极端应被 clamp");
        assert!(slope_tan(-1.0).is_finite(), "负 NdotL 不得产生 NaN/Inf");
    }

    /// 方向光 bias：常数下限恒定成立（平地不复发 acne 的硬保证）、
    /// 掠射角单调增长、上限封顶；法线偏移垂直入射归零、掠射增大且有界。
    #[test]
    fn shadow_bias_respects_floor_and_ceiling() {
        let (texel, window) = (0.0175f32, 82.0f32); // 近级联量级
        // 垂直入射 = 恰好常数下限（v1 的 0.0015 降到 0.0004）
        assert_eq!(shadow_bias_ndc(1.0, texel, window), SHADOW_BIAS_MIN);
        const { assert!(SHADOW_BIAS_MIN < SHADOW_BIAS_LEGACY) };
        // 掠射角显著抬高 bias，但不超过上限
        let grazing = shadow_bias_ndc(0.118, texel, window);
        assert!(
            grazing > SHADOW_BIAS_LEGACY,
            "掠射角 bias {grazing} 应超过 v1 常数（否则压不住 acne）"
        );
        for i in 0..=100 {
            let nol = i as f32 / 100.0;
            let b = shadow_bias_ndc(nol, texel, window);
            assert!((SHADOW_BIAS_MIN..=SHADOW_BIAS_MAX).contains(&b), "越界 {b}");
        }
        // 上限生效：极大纹素（远级联）+ 掠射
        assert_eq!(shadow_bias_ndc(0.01, 4.0, 1.0), SHADOW_BIAS_MAX, "上限封顶");
        // 法线偏移：垂直入射归零（接触阴影不浮起的硬保证）、掠射有界
        assert_eq!(shadow_normal_offset_world(1.0, texel), 0.0);
        assert!(
            (shadow_normal_offset_world(0.5, texel) - 1.5 * texel * 0.5).abs() < 1e-9,
            "偏移量 = 1.5 · texel · (1 − NdotL)"
        );
        assert_eq!(
            shadow_normal_offset_world(0.0, 4.0),
            SHADOW_NORMAL_MAX_WORLD,
            "远级联上限"
        );
    }

    /// 点光 bias/偏移：纹素世界尺寸 = d/256（90° 面 512 texel），
    /// bias 以米为单位且下限/上限成立；归一化口径 = 除以 range。
    #[test]
    fn point_shadow_bias_scales_with_distance() {
        assert!((point_shadow_texel_world(3.0) - 3.0 / 256.0).abs() < 1e-9);
        const { assert!(POINT_SHADOW_BIAS_MIN < POINT_SHADOW_BIAS_LEGACY) };
        // 垂直入射 = 常数下限
        assert_eq!(point_shadow_bias_meters(1.0, 3.0), POINT_SHADOW_BIAS_MIN);
        // 掠射角下 bias 增长且不超过上限
        let grazing = point_shadow_bias_meters(0.118, 3.0);
        assert!(grazing > POINT_SHADOW_BIAS_MIN);
        assert!(grazing <= POINT_SHADOW_BIAS_MAX);
        // 归一化：range = 12 时下限 bias 的归一化值 = 0.005/12
        assert!(
            (point_shadow_bias_meters(1.0, 3.0) / 12.0 - POINT_SHADOW_BIAS_MIN / 12.0).abs() < 1e-9
        );
        // 距离越远纹素越大 → bias 越大（同一入射角）
        assert!(
            point_shadow_bias_meters(0.3, 10.0) > point_shadow_bias_meters(0.3, 2.0),
            "远距离纹素更大，bias 应随之增长"
        );
        assert_eq!(point_shadow_normal_offset_world(1.0, 5.0), 0.0);
        assert!(point_shadow_normal_offset_world(0.0, 5.0) <= POINT_SHADOW_NORMAL_MAX_WORLD);
    }

    /// 预算选择键的确定性：同灯集合乱序插入 ⇒ 选中同一批灯（同一槽位
    /// 序列）；距离并列由实体位模式收尾（全序）；预算内全选（单盏阴影
    /// 与 v1 同灯）；超预算按距离裁掉远处的灯。NaN 距离也有确定位置
    /// （total_cmp），不破坏全序。
    #[test]
    fn point_shadow_caster_selection_is_order_independent() {
        let candidates = [
            PointShadowCandidate {
                entity_bits: 0x40,
                eye_distance: 9.0,
            },
            PointShadowCandidate {
                entity_bits: 0x10,
                eye_distance: 3.0,
            },
            PointShadowCandidate {
                entity_bits: 0x20,
                eye_distance: 3.0,
            },
            PointShadowCandidate {
                entity_bits: 0x30,
                eye_distance: 7.5,
            },
            PointShadowCandidate {
                entity_bits: 0x50,
                eye_distance: 3.0,
            },
        ];
        let chosen = select_point_shadow_casters(&candidates, 4);
        // 距离升序 → 同距按位模式升序：3.0(0x10), 3.0(0x20), 3.0(0x50), 7.5(0x30)
        assert_eq!(chosen, vec![1, 2, 4, 3]);
        // 乱序插入：逆序 + 中间插入的两种排列必须得到同一批灯（同槽位序）
        let mut reversed = candidates;
        reversed.reverse();
        assert_eq!(select_point_shadow_casters(&reversed, 4), vec![3, 2, 0, 1]);
        let picked = |list: &[PointShadowCandidate], idx: &[usize]| -> Vec<u64> {
            let mut v: Vec<u64> = idx.iter().map(|&i| list[i].entity_bits).collect();
            v.sort_unstable();
            v
        };
        assert_eq!(
            picked(&candidates, &chosen),
            picked(&reversed, &select_point_shadow_casters(&reversed, 4)),
            "乱序插入必须选中同一批灯"
        );
        // 单盏：预算内全选 = v1 的「唯一一盏」
        let single = [PointShadowCandidate {
            entity_bits: 0x99,
            eye_distance: 42.0,
        }];
        assert_eq!(
            select_point_shadow_casters(&single, POINT_SHADOW_BUDGET),
            vec![0]
        );
        // 空集合：无投射器（零开销默认路径）
        assert!(select_point_shadow_casters(&[], POINT_SHADOW_BUDGET).is_empty());
    }

    // ═══════════════════════════════════════════════════════════════════
    // R78：SpotLight 组件
    // ═══════════════════════════════════════════════════════════════════

    /// 默认值与构造器逐字段核对：白光 / 强度 8 / 半径 12 / 垂直下照 /
    /// 内角 30°、外角 45°（弧度值精确到 f32 最近表示）。
    #[test]
    fn spot_light_defaults_and_constructor() {
        let d = SpotLight::default();
        assert_eq!(d.color, Vec3::ONE, "默认颜色应为白");
        assert_eq!(d.intensity, 8.0);
        assert_eq!(d.range, 12.0);
        assert_eq!(d.direction, Vec3::new(0.0, -1.0, 0.0), "默认垂直下照");
        assert_eq!(d.inner_cutoff, 30.0f32.to_radians(), "默认全强锥角 30°");
        assert_eq!(d.outer_cutoff, 45.0f32.to_radians(), "默认锥外归零角 45°");
        assert!(
            d.inner_cutoff < d.outer_cutoff,
            "默认内角必须小于外角（软边语义）"
        );
        // 构造器逐字段透传（不做钳制——inner > outer 由渲染器上传时钳到
        // 硬边，组件层保留作者原值）
        let s = SpotLight::new(
            Vec3::new(1.0, 0.5, 0.25),
            20.0,
            6.0,
            Vec3::new(0.0, 0.0, -1.0),
            0.2,
            0.9,
        );
        assert_eq!(s.color, Vec3::new(1.0, 0.5, 0.25));
        assert_eq!(s.intensity, 20.0);
        assert_eq!(s.range, 6.0);
        assert_eq!(s.direction, Vec3::new(0.0, 0.0, -1.0));
        assert_eq!(s.inner_cutoff, 0.2);
        assert_eq!(s.outer_cutoff, 0.9);
    }
}
