//! 灯光资源与组件。

use glam::{Mat4, Vec2, Vec3, Vec3Swizzles};
use iris_core::{Component, Resource};

use crate::pipeline::{SHADOW_CASCADES, SHADOW_DISTANCE, SHADOW_MAP_SIZE};

/// 单帧允许的最大点光数量（uniform 数组容量，超出部分被忽略并记录日志）。
pub const MAX_POINT_LIGHTS: usize = 8;

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
///   2r，与 SHADOW_BIAS（NDC 常数偏移）相乘的世界偏移量因此有界。
pub fn cascade_fit(
    light_direction: Vec3,
    view: &Mat4,
    fov_y_radians: f32,
    aspect: f32,
    z_start: f32,
    z_end: f32,
) -> Mat4 {
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
    proj * light_view
}

/// 点光源组件。位置取自同一实体的 `GlobalTransform`（层级系统每帧更新）。
///
/// 衰减模型：`saturate(1 - d/range)²`，在 `range` 处平滑归零，
/// 颜色 × 强度决定亮度。超过 [`MAX_POINT_LIGHTS`] 的点光会被忽略。
///
/// 阴影：v1 只允许**一盏**点光投射 cubemap 阴影——多盏开启时取查询
/// 顺序中的第一盏并记录日志（cubemap 每灯 6 个 pass，成本高）。
#[derive(Clone, Copy, Debug, Component)]
pub struct PointLight {
    /// 光的颜色（线性空间）。
    pub color: Vec3,
    /// 强度倍率。
    pub intensity: f32,
    /// 有效半径（世界单位），超出后无贡献。
    pub range: f32,
    /// 是否投射 cubemap 阴影（仅查询顺序中第一盏生效）。
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
        let (dir, up) = POINT_SHADOW_FACES[face];
        let view = glam::camera::rh::view::look_at_mat4(light_pos, light_pos + dir, up);
        let proj = glam::camera::rh::proj::directx::perspective(
            std::f32::consts::FRAC_PI_2,
            1.0,
            POINT_SHADOW_NEAR,
            range,
        );
        Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0)) * (proj * view)
    }
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
}
