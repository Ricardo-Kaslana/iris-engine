//! 前向光源分箱（R78）：CPU 屏幕空间 tile 分箱的纯函数核心。
//!
//! 突破 `MAX_POINT_LIGHTS = 8` 的全局均摊并引入 [`crate::light::SpotLight`]：
//! 每帧把全部点光 + 锥光按「世界空间 range 球 → 屏幕 tile 矩形」投影进
//! `LIGHT_TILE_SIZE` 像素的 tile，GPU 侧片元按 `@builtin(position)` 查
//! 本 tile 的灯表下标求和（`shaders/standard.wgsl` 的 bins 分支）。
//!
//! **双路径位等价**：≤ 8 盏点光且无锥光时走 Globals 均摊旧路径、超出或
//! 有锥光时走分箱新路径，两条路径的逐灯贡献共用着色器侧同一个
//! `point_light_contrib` 函数（逐式同序提取自旧循环体），tile 覆盖是
//! range 球投影矩形的**保守超集**（3D 包含 ⇒ 投影包含，见 [`bin_lights`]）
//! ⇒ 同一灯集合两条路径逐像素加出同一串位。兼容形态与 R77 GTAO 同款：
//! 灯表资源缺席/关闭 = 绑定全零哑缓冲（mode = 0），分支死码，既有像素
//! 逐位不变。
//!
//! **确定性**：tile → 灯下标的映射用 [`std::collections::BTreeMap`]——
//! 输出展平顺序必须只取决于灯集合本身，与构建时的插入次序无关；
//! `BTreeMap` 按键（tile 线性下标）升序迭代给出**确定的展平序**，
//! `HashMap` 的迭代序则随 RandomState 逐进程漂移（会直接烧穿「同输入
//! 两次跑逐位一致」的确定性铁律）。tile 内灯下标按灯数组序升序插入
//! （灯数组序即求和序），保证与 Globals 旧路径同序累加。

use std::collections::BTreeMap;

use glam::{Mat4, Vec3, Vec4Swizzles};
use iris_core::Resource;

use crate::light::{MAX_POINT_LIGHTS, POINT_SHADOW_NEAR};

/// 灯表容量（GPU storage 数组定长，512 × 80 B）：超出按查询序截断并
/// `log::warn`（口径与 Globals 点光超限一致——不是静默丢弃）。
pub const MAX_BINNED_LIGHTS: usize = 512;

/// 屏幕 tile 边长（像素）：32² tile 在 1080p 下约 2040 个（1 MiB 索引
/// 缓冲余量充裕），单个 tile 的灯数在典型场景 ≤ 个位数。
pub const LIGHT_TILE_SIZE: u32 = 32;

/// 分箱输入的中性灯条目：只带分箱需要的两个字段（世界位置 + 有效半径）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightBinInput {
    /// 灯的世界空间位置。
    pub position: Vec3,
    /// 有效半径（世界单位，同 `PointLight::range` / `SpotLight::range`）。
    pub range: f32,
}

/// 分箱输出：`ranges[i] = [start, count]` 是 tile i（行主序，
/// `tile_y * tiles_x + tile_x`，tile_y 自屏幕顶端起算——与着色器
/// `@builtin(position)` 同向）在 `indices` 里的切片。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LightBins {
    /// 水平 tile 数（`ceil(宽 / LIGHT_TILE_SIZE)`）。
    pub tiles_x: u32,
    /// 垂直 tile 数（`ceil(高 / LIGHT_TILE_SIZE)`）。
    pub tiles_y: u32,
    /// 每 tile 的 `[start, count]`，长度恰为 `tiles_x * tiles_y`。
    pub ranges: Vec<[u32; 2]>,
    /// 灯下标展平区（灯数组下标，tile 内升序、tile 间按 tile 下标升序）。
    pub indices: Vec<u32>,
}

/// 是否需要启用分箱路径：点光超出 Globals 均摊容量，或场景里有锥光。
///
/// 阈值边界（`MAX_POINT_LIGHTS`）：恰 8 盏点光 + 0 锥光 = 旧路径
/// （false）；第 9 盏点光或任意 1 盏锥光即翻到分箱（true）。
pub fn light_bins_enabled(point_count: usize, spot_count: usize) -> bool {
    point_count > MAX_POINT_LIGHTS || spot_count > 0
}

/// 锥光强度因子：`cos_theta` 为「片元方向 · 锥轴」的余弦，`cos_inner` /
/// `cos_outer` 为全强角 / 归零角的余弦（由 CPU 预计算上传）。
///
/// ```text
/// denom = cos_inner - cos_outer
/// denom <= 1e-6 → 硬边：cos_theta >= cos_outer 取 1 否则 0
/// 否则          → clamp((cos_theta - cos_outer) / denom, 0, 1)
/// ```
///
/// `inner ≥ outer`（含相等）时 `denom ≤ 0`，落入硬边分支——这正是
/// 「`inner > outer` 钳到硬边」的实现机制，无需独立开关。NaN 输入按
/// 0 贡献处理（不外溢：硬边分支的 `>=` 对 NaN 恒假；软边分支显式
/// `is_nan` 判定）。**WGSL 侧 `standard.wgsl::spot_cone_factor` 逐行
/// 镜像（同式同序），两侧必须同步修改**。
pub fn spot_cone_factor(cos_theta: f32, cos_inner: f32, cos_outer: f32) -> f32 {
    let denom = cos_inner - cos_outer;
    if denom <= 1e-6 {
        // 硬边（含 inner >= outer 的钳制形态）；NaN 比较恒假 ⇒ 0 贡献
        return if cos_theta >= cos_outer { 1.0 } else { 0.0 };
    }
    let t = (cos_theta - cos_outer) / denom;
    if t.is_nan() {
        return 0.0;
    }
    t.clamp(0.0, 1.0)
}

/// 把灯列表按屏幕 tile 分箱（纯函数，可脱离 `World` 单测）。
///
/// **逐灯算法**（输入 = 灯数组 + 相机 `view_proj` + 屏幕尺寸 + tile 边长）：
/// 1. **近平面门**：视空间球心深度 `z_view` 满足 `z_view < -near`（near =
///    [`POINT_SHADOW_NEAR`] = 0.05）才可能可见。DirectX/RH 透视下
///    `clip.w = -z_view`，故门条件即 `clip.w >= near`；球心在近平面之后
///    （含相机背后）的灯**整盏跳过**。
/// 2. **投影包围盒**：球心 ± `range` 沿相机 right / up / back 三轴的
///    包围盒 8 角逐个投影取 NDC 包围矩形。三轴取自 `view_proj.inverse()`
///    的列（推导：`view_proj⁻¹ = view⁻¹ · proj⁻¹`，`proj⁻¹` 除缩放外
///    只在 z/w 列交换，方向列（0/1/3 列）保持与 `view⁻¹` 的列平行，而
///    `view⁻¹` 的列恰是相机轴——正交框架下球沿任一组正交轴的包围盒都
///    是「球心 ± r」，精确不保守）。8 角的视空间深度各自钳到近平面
///    （`clip.w = max(w, near)` 再除）：球可跨越近平面，越过相机的角
///    直接除会 w 翻转出伪矩形；钳到近平面后给出的是屏幕上的**保守外扩**
///    （分箱允许超覆盖，不允许漏覆盖）。
/// 3. **tile 展开**：NDC 矩形 clamp 到 [-1, 1] 后换算像素（y 向下，与
///    着色器同向），`floor(px / tile)` 得覆盖 tile 集合。
///
/// 覆盖的**保守性**（双路径位等价的根据）：包围盒在 3D 包含 range 球
/// ⇒ 投影矩形的凸包包含球的剪影 ⇒ 任何与球相交的片元（即 `dist < range`
/// 有贡献的片元）所在 tile 必在覆盖集合内——分箱路径不会漏灯；多覆盖
/// 的 tile 只多做「dist ≥ range 早退」的无效求和，不影响数值。
///
/// **输出确定性**：[`BTreeMap`] 按 tile 下标升序展平（理由见模块头）；
/// tile 内下标按灯数组序升序——同一灯数组重复调用输出逐字节一致，
/// 灯集合的任意重排输出按各自下标仍确定一致（同一灯的下标集合不变）。
pub fn bin_lights(
    lights: &[LightBinInput],
    view_proj: Mat4,
    screen_width: u32,
    screen_height: u32,
    tile_size: u32,
) -> LightBins {
    let tiles_x = screen_width.max(1).div_ceil(tile_size.max(1));
    let tiles_y = screen_height.max(1).div_ceil(tile_size.max(1));
    let mut bins = LightBins {
        tiles_x,
        tiles_y,
        ranges: vec![[0, 0]; tiles_x as usize * tiles_y as usize],
        indices: Vec::new(),
    };
    if lights.is_empty() {
        return bins;
    }
    // 相机轴（世界空间）：view_proj⁻¹ 的方向列（推导见函数文档）。
    // normalize_or_zero：退化矩阵下轴归零 ⇒ 包围盒塌到球心（近平面门
    // 仍然生效，不会外溢错误覆盖）。
    let inv = view_proj.inverse();
    let right = inv.x_axis.truncate().normalize_or_zero();
    let up = inv.y_axis.truncate().normalize_or_zero();
    let back = inv.z_axis.truncate().normalize_or_zero();
    // tile → 灯下标（构建期容器；BTreeMap 保确定性，理由见模块头）
    let mut map: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (i, light) in lights.iter().enumerate() {
        let radius = light.range.max(0.0);
        // 近平面门：DirectX/RH 透视 clip.w = -z_view（见函数文档）。
        let center_clip = view_proj * light.position.extend(1.0);
        if center_clip.w < POINT_SHADOW_NEAR {
            continue; // 球心在近平面之后（含相机背后）：整盏跳过
        }
        // 8 角投影取 NDC 包围矩形（角深度钳到近平面：保守外扩）
        let mut ndc_min = glam::Vec2::splat(f32::INFINITY);
        let mut ndc_max = glam::Vec2::splat(f32::NEG_INFINITY);
        for sx in [-1.0f32, 1.0] {
            for sy in [-1.0, 1.0] {
                for sz in [-1.0, 1.0] {
                    let corner = light.position + (right * sx + up * sy + back * sz) * radius;
                    let clip = view_proj * corner.extend(1.0);
                    let w = clip.w.max(POINT_SHADOW_NEAR);
                    let ndc = clip.xy() / w;
                    ndc_min = ndc_min.min(ndc);
                    ndc_max = ndc_max.max(ndc);
                }
            }
        }
        // NDC → 像素（y 向下）→ tile 区间，clamp 到屏幕内
        let px_min =
            (ndc_min * 0.5 + 0.5) * glam::Vec2::new(screen_width as f32, screen_height as f32);
        let px_max =
            (ndc_max * 0.5 + 0.5) * glam::Vec2::new(screen_width as f32, screen_height as f32);
        let tx0 = ((px_min.x / tile_size as f32).floor() as i64).clamp(0, tiles_x as i64 - 1);
        let tx1 = ((px_max.x / tile_size as f32).floor() as i64).clamp(0, tiles_x as i64 - 1);
        let ty0 = ((px_min.y / tile_size as f32).floor() as i64).clamp(0, tiles_y as i64 - 1);
        let ty1 = ((px_max.y / tile_size as f32).floor() as i64).clamp(0, tiles_y as i64 - 1);
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                let tile_id = (ty as u32) * tiles_x + tx as u32;
                map.entry(tile_id).or_default().push(i as u32);
            }
        }
    }
    // 展平：BTreeMap 升序迭代 ⇒ ranges/indices 的顺序只取决于 tile 下标
    // 与灯数组序（确定性；range 区全量铺满，未覆盖 tile 恒 [0, 0]）
    for (tile_id, list) in map {
        let start = bins.indices.len() as u32;
        bins.indices.extend(list);
        bins.ranges[tile_id as usize] = [start, bins.indices.len() as u32 - start];
    }
    bins
}

/// 分箱开关资源（R78）：**资源缺席 = 自动阈值**
/// （[`light_bins_enabled`]）。`force` 为调试 / 验收入口——强制走分箱
/// 路径（双路径位等价测试用它：≤ 8 盏点光场景 force 与缺席必须逐位
/// 一致）。`DefaultPlugins` 不注册本资源。
#[derive(Clone, Copy, Debug, Default, Resource)]
pub struct LightBinningSettings {
    /// 强制启用分箱路径（`false` = 缺省，自动阈值；`true` 时无视
    /// [`light_bins_enabled`]）。
    pub force: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试相机：eye 在原点看向 -Z（90° 垂直 fov、宽高比 1）。
    fn test_view_proj() -> Mat4 {
        let view =
            glam::camera::rh::view::look_at_mat4(Vec3::ZERO, Vec3::new(0.0, 0.0, -1.0), Vec3::Y);
        let proj = glam::camera::rh::proj::directx::perspective(
            std::f32::consts::FRAC_PI_2,
            1.0,
            0.1,
            100.0,
        );
        proj * view
    }

    fn light_at(x: f32, y: f32, z: f32, range: f32) -> LightBinInput {
        LightBinInput {
            position: Vec3::new(x, y, z),
            range,
        }
    }

    /// tile 下标 → 该 tile 的世界方向余弦域（用于反查覆盖判定）。
    fn covered_tiles(bins: &LightBins) -> Vec<(u32, Vec<u32>)> {
        bins.ranges
            .iter()
            .enumerate()
            .filter(|(_, r)| r[1] > 0)
            .map(|(i, r)| {
                (
                    i as u32,
                    bins.indices[r[0] as usize..(r[0] + r[1]) as usize].to_vec(),
                )
            })
            .collect()
    }

    /// 单灯居中：覆盖集合是一个非空矩形、包含投影中心所在 tile、
    /// 远离投影的四角 tile 不被覆盖；重复调用输出逐字节一致。
    #[test]
    fn bin_lights_covers_projected_rect_and_is_deterministic() {
        let vp = test_view_proj();
        // 球心 (0,0,-5)，r = 2：NDC 半宽 ≈ 2/5 = 0.4 → 屏幕 320 上
        // 半宽 64 px → 覆盖 tile 横竖各约 4 个，中心 tile (4, 4) 必命中。
        let bins = bin_lights(&[light_at(0.0, 0.0, -5.0, 2.0)], vp, 320, 320, 32);
        assert_eq!(bins.tiles_x, 10);
        assert_eq!(bins.tiles_y, 10);
        assert_eq!(bins.ranges.len(), 100);
        let covered = covered_tiles(&bins);
        assert!(!covered.is_empty(), "视野中央的灯必须覆盖至少一个 tile");
        // 中心 tile：像素 (160,160) → tile (5*32=160 起点 → tile 5？)
        // 160/32 = 5.0 → tile 5（行主序 tile_id = 5*10+5 = 55）
        let center = bins.ranges[5 * 10 + 5];
        assert_eq!(center[1], 1, "中心 tile 必须命中该灯（[start, count=1]）");
        assert_eq!(bins.indices[center[0] as usize], 0);
        // 四角 tile 不覆盖（投影矩形 ≈ ±64 px，远小于 160 px 的半屏）
        for corner in [0usize, 9, 90, 99] {
            assert_eq!(
                bins.ranges[corner][1], 0,
                "屏幕角 tile {corner} 不应被视野中央的灯覆盖"
            );
        }
        // 覆盖集合构成连续矩形（每行覆盖数相同且连续）
        let ids: Vec<u32> = covered.iter().map(|(id, _)| *id).collect();
        let cols: Vec<u32> = ids.iter().map(|id| id % 10).collect();
        let rows: Vec<u32> = ids.iter().map(|id| id / 10).collect();
        assert_eq!(
            cols.iter().max().unwrap() - cols.iter().min().unwrap() + 1,
            covered.len() as u32 / (rows.iter().max().unwrap() - rows.iter().min().unwrap() + 1),
            "覆盖集合应为连续矩形（单灯居中）"
        );
        // 确定性：同输入重复调用逐字节一致
        let again = bin_lights(&[light_at(0.0, 0.0, -5.0, 2.0)], vp, 320, 320, 32);
        assert_eq!(bins, again, "同输入两次分箱必须逐字节一致");
    }

    /// 双灯分居：各 tile 的灯表恰含覆盖它的灯（下标升序）；两灯都覆盖
    /// 的 tile 下标为 [0, 1]；全局 indices 展平区内每 tile 升序。
    #[test]
    fn bin_lights_assigns_exactly_covering_lights_in_ascending_order() {
        let vp = test_view_proj();
        // 左右分居：r = 1（NDC 半宽 0.2 → 64 px ≈ 2 tile），球心 NDC x = ±0.4
        let bins = bin_lights(
            &[
                light_at(-2.0, 0.0, -5.0, 1.0),
                light_at(2.0, 0.0, -5.0, 1.0),
            ],
            vp,
            320,
            320,
            32,
        );
        let mut left_only = 0;
        let mut right_only = 0;
        let mut both = 0;
        for (id, list) in covered_tiles(&bins) {
            let mut sorted = list.clone();
            sorted.sort_unstable();
            assert_eq!(list, sorted, "tile {id} 的灯下标必须升序");
            // 球心屏幕位置：NDC x = ∓0.4 → px x = 96 / 224 → tile 3 / 7
            match (list.contains(&0), list.contains(&1)) {
                (true, false) => left_only += 1,
                (false, true) => right_only += 1,
                (true, true) => both += 1,
                (false, false) => unreachable!("covered_tiles 只回非空 tile"),
            }
        }
        assert!(
            left_only > 0 && right_only > 0,
            "两盏灯必须各覆盖自己的半边"
        );
        // r = 1 → NDC 半宽 0.2 → px 半宽 64 = 2 tile：两灯矩形不相交
        assert_eq!(both, 0, "本构型下两灯覆盖矩形不应重叠");
        // 全局 indices 展平：覆盖 tile 升序 ⇒ 每个 range 的 start 单调
        // 接续（未覆盖 tile 的 [0, 0] 不参与前缀链）
        let mut prev_end = 0u32;
        for r in &bins.ranges {
            if r[1] == 0 {
                continue;
            }
            assert_eq!(r[0], prev_end, "ranges 展平必须紧致（start = 前缀和）");
            prev_end = r[0] + r[1];
        }
        assert_eq!(prev_end as usize, bins.indices.len());
    }

    /// 球心在近平面之后（含相机背后）的灯整盏跳过：输出无该灯任何痕迹。
    #[test]
    fn bin_lights_skips_lights_behind_near_plane() {
        let vp = test_view_proj();
        // 相机背后
        let behind = bin_lights(&[light_at(0.0, 0.0, 5.0, 2.0)], vp, 320, 320, 32);
        assert!(covered_tiles(&behind).is_empty(), "相机背后的灯必须全弃");
        // 眼前但在近平面之前（z_view = -0.03 > -0.05）
        let before_near = bin_lights(&[light_at(0.0, 0.0, -0.03, 2.0)], vp, 320, 320, 32);
        assert!(
            covered_tiles(&before_near).is_empty(),
            "近平面之前的灯必须全弃"
        );
        // 混合场景：一盏在前一盏在后 → 只有前面的灯进表（下标保持原位）
        let mixed = bin_lights(
            &[light_at(0.0, 0.0, 5.0, 2.0), light_at(0.0, 0.0, -5.0, 1.0)],
            vp,
            320,
            320,
            32,
        );
        for (_, list) in covered_tiles(&mixed) {
            assert_eq!(list, vec![1], "只应剩第 2 盏灯（下标 1），第 1 盏全弃");
        }
    }

    /// NDC 越界 clamp：灯投影出屏时矩形钳到屏幕边——边缘 tile 命中、
    /// 对侧不命中、tile 下标永不越界。
    #[test]
    fn bin_lights_clamps_out_of_screen_rects() {
        let vp = test_view_proj();
        // 屏幕右侧远处：NDC x ≈ 20 → 整个矩形出屏，clamp 到右缘
        let bins = bin_lights(&[light_at(100.0, 0.0, -5.0, 1.0)], vp, 320, 320, 32);
        let covered = covered_tiles(&bins);
        assert!(!covered.is_empty(), "出屏灯 clamp 后应覆盖右缘 tile");
        for (id, _) in &covered {
            assert_eq!(id % 10, 9, "出屏右侧灯只应覆盖最右列 tile（id={id}）");
        }
        // 全出屏（相机背后 + 出屏方向）不会 panic 且为空
        let none = bin_lights(&[light_at(100.0, 0.0, 5.0, 1.0)], vp, 320, 320, 32);
        assert!(covered_tiles(&none).is_empty());
    }

    /// 构建序无关性：同一灯集合的任意排列，输出按各自下标仍确定一致
    /// ——tile 的成员资格跟着「灯」走而不是跟着「处理次序」走。
    #[test]
    fn bin_lights_is_order_independent_over_light_permutations() {
        let vp = test_view_proj();
        // L0 居左、L1 居右（与 test 2 同构型但 r 加大到 1.6 让矩形有交叠带）
        let a = bin_lights(
            &[
                light_at(-2.0, 0.0, -5.0, 1.6),
                light_at(2.0, 0.0, -5.0, 1.6),
            ],
            vp,
            320,
            320,
            32,
        );
        let b = bin_lights(
            &[
                light_at(2.0, 0.0, -5.0, 1.6),
                light_at(-2.0, 0.0, -5.0, 1.6),
            ],
            vp,
            320,
            320,
            32,
        );
        assert_eq!(a.ranges.len(), b.ranges.len());
        for (tile, (ra, rb)) in a.ranges.iter().zip(b.ranges.iter()).enumerate() {
            let la = &a.indices[ra[0] as usize..(ra[0] + ra[1]) as usize];
            let lb = &b.indices[rb[0] as usize..(rb[0] + rb[1]) as usize];
            // 成员资格按灯重标定：A 的灯 0 ⟺ B 的灯 1；A 的灯 1 ⟺ B 的灯 0
            let remapped: Vec<u32> = la.iter().map(|i| 1 - i).collect();
            assert_eq!(remapped, lb, "tile {tile} 的成员资格必须按灯重标定后一致");
            // 且各 tile 内升序（重排后仍保持灯数组序）
            let mut sorted = lb.to_vec();
            sorted.sort_unstable();
            assert_eq!(lb, &sorted, "tile {tile} 的灯下标必须升序");
        }
        // 重复调用（同一排列）逐字节一致
        let b2 = bin_lights(
            &[
                light_at(2.0, 0.0, -5.0, 1.6),
                light_at(-2.0, 0.0, -5.0, 1.6),
            ],
            vp,
            320,
            320,
            32,
        );
        assert_eq!(b, b2);
    }

    /// 空灯表 / 空屏幕的退化路径：ranges 全零、indices 空、不 panic。
    #[test]
    fn bin_lights_handles_empty_input() {
        let vp = test_view_proj();
        let bins = bin_lights(&[], vp, 320, 320, 32);
        assert_eq!(bins.ranges, vec![[0, 0]; 100]);
        assert!(bins.indices.is_empty());
        // 1×1 屏幕（离屏最小值防御）
        let tiny = bin_lights(&[light_at(0.0, 0.0, -5.0, 1.0)], vp, 1, 1, 32);
        assert_eq!((tiny.tiles_x, tiny.tiles_y), (1, 1));
    }

    /// 锥光因子解析解：θ=inner → 1、θ=outer → 0、中点线性、硬边分支、
    /// cos_inner ≤ cos_outer 分支、NaN 不外溢、单调性。
    #[test]
    fn spot_cone_factor_matches_analytic_solution() {
        let (inner, outer) = (30.0f32.to_radians(), 45.0f32.to_radians());
        let (ci, co) = (inner.cos(), outer.cos());
        // 端点
        assert_eq!(spot_cone_factor(ci, ci, co), 1.0, "θ=inner 必须全强");
        assert_eq!(spot_cone_factor(co, ci, co), 0.0, "θ=outer 必须归零");
        // 中点线性：θ = (inner+outer)/2 → (cos θ − cos outer)/(cos inner − cos outer)
        let mid = 0.5 * (inner + outer);
        let expected = (mid.cos() - co) / (ci - co);
        assert!(
            (spot_cone_factor(mid.cos(), ci, co) - expected).abs() < 1e-6,
            "中点应为线性插值 {expected}"
        );
        // 锥内/锥外样点
        assert_eq!(spot_cone_factor(1.0, ci, co), 1.0, "锥轴上全强");
        assert_eq!(spot_cone_factor(-1.0, ci, co), 0.0, "锥轴反向归零");
        // 硬边分支：inner == outer（denom = 0）
        assert_eq!(spot_cone_factor(0.9, co, co), 1.0, "硬边内取 1");
        assert_eq!(spot_cone_factor(0.5, co, co), 0.0, "硬边外取 0");
        // inner < outer（denom < 0）：同硬边
        assert_eq!(spot_cone_factor(0.9, 0.5, 0.9), 1.0);
        assert_eq!(spot_cone_factor(0.3, 0.5, 0.9), 0.0);
        // NaN 不外溢：软边 / 硬边两条分支都收敛到 0
        assert_eq!(spot_cone_factor(f32::NAN, ci, co), 0.0, "软边 NaN → 0");
        assert_eq!(spot_cone_factor(f32::NAN, co, co), 0.0, "硬边 NaN → 0");
        assert_eq!(
            spot_cone_factor(0.8, f32::NAN, co),
            0.0,
            "cos_inner NaN → 0"
        );
        // 单调性：θ 增大（cos 减小）因子不增
        let mut prev = 1.0;
        for i in 0..=40 {
            let t = inner + (outer - inner) * (i as f32 / 40.0);
            let f = spot_cone_factor(t.cos(), ci, co);
            assert!(f <= prev, "锥光因子必须随 θ 单调不增：{t} → {f}");
            prev = f;
        }
    }

    /// 阈值边界：8 点光 + 0 锥光 = 旧路径；第 9 盏点光或任意锥光 = 分箱。
    #[test]
    fn light_bins_enabled_thresholds() {
        assert!(!light_bins_enabled(0, 0), "空场景不需要分箱");
        assert!(!light_bins_enabled(8, 0), "恰 8 盏点光走 Globals 均摊");
        assert!(light_bins_enabled(9, 0), "第 9 盏点光必须翻到分箱");
        assert!(!light_bins_enabled(0, 0), "0 锥光 + 0 点光不启用");
        assert!(light_bins_enabled(0, 1), "1 盏锥光即启用");
        assert!(light_bins_enabled(8, 1), "8 点光 + 锥光启用");
        assert!(
            light_bins_enabled(512, 512),
            "超容量照常启用（截断在渲染器）"
        );
    }
}
