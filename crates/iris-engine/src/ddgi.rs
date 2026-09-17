//! DDGI 每帧编排（R88）：把 [`DdgiSettings`] + [`DirectionalLight`] 派生成
//! [`BakeParams`]，喂给 [`DdgiGrid`] 的时间片滚动重照明。
//!
//! **算法本体全在 `iris_render::ddgi` 的纯函数里**（网格生成 / 轮转 /
//! 重照明 / 深度矩 / 混合镜像），本模块只做「读世界 → 组装口径 → 写回资源」
//! 的编排 ⇒ headless 可单测（不需要 GPU）。
//!
//! **编排语义**：[`tick_ddgi`] 是 `&World` 并行系统（Update 阶段，由
//! `DdgiPlugin` 注册——`DefaultPlugins` 已含，注册在 `DayNightPlugin`
//! **之后** ⇒ 昼夜当帧写的方向光当帧被 DDGI 消费）。两阶段模式：
//! ①收集（读设置 / 方向光，需要重建时才 `collect_bake_triangles`）→
//! ②写回（`resource_mut::<DdgiGrid>()`，锁冲突即 panic，消息含
//! `already borrowed`）。
//!
//! **资源缺席语义（API 的一部分）**：
//! - [`DdgiSettings`] 缺席 = **零操作**（零分配零成本，仓库「缺席 = 关」文化）；
//! - [`DdgiGrid`] 缺席 = 零操作（`DdgiPlugin` 在 Startup 插入空网格；自建
//!   App 时忘记加插件不会 panic，只是 DDGI 不生效）；
//! - [`DirectionalLight`] 缺席 = 用其 `Default`（与渲染器的兜底同源）；
//! - 静态几何为空 **且** `bounds = None` ⇒ 网格空 ⇒ 渲染器视为 DDGI 缺席
//!   （mode 回落 R83 裁决，既有像素逐位不变）。
//!
//! **成本面**：稳态帧（无脏探针）只读两个资源 + 一次指纹比对，
//! `collect_bake_triangles` **一次都不跑**——几何变化靠宿主的
//! `geometry_generation` 显式宣告（`iris_render::ddgi` 的诚实限制④）。
//! 脏帧的 CPU 成本 ≈ `probes_per_frame × 0.83 ms`（`rays_per_face = 16`、
//! 1000 三角 BVH 基线；随三角数线性上浮）。

use iris_core::world::World;
use iris_render::bake::{BakeParams, collect_bake_triangles};
use iris_render::ddgi::{DdgiGrid, DdgiSettings};
use iris_render::light::DirectionalLight;

/// 由设置 + 方向光组装重照明口径（纯函数，可单测）。
///
/// 映射依据见 `iris_render::ddgi` 模块文档「运行期光照口径」：
/// - `light_dir` = `DirectionalLight.direction`（传播方向，同口径）；
/// - `light_color` = `color × intensity`（约定「已含强度」）；
/// - `sky_color` = `settings.sky_color.unwrap_or(ambient)`（裁定 3：R80 昼夜
///   免费联动），**不乘 `EnvironmentSettings.intensity`**——`irradiance` 在
///   `standard.wgsl` 的 `ibl_env` 已被 `env_params.y` 缩放，预乘会双计；
/// - `shadow_softness` = 0（重照明走 `allow_soft = false` 的硬影单射线）；
/// - `sky_samples` = 1（本路径不做半球采样：`probe_ray_radiance_depth` 每
///   方向只发 1 条主射线 + 命中点的 1 条硬影射线）；
/// - `bounces` = `settings.bounces`（≤ 1，`BakeParams` 现断言）。
pub fn ddgi_bake_params(settings: &DdgiSettings, light: &DirectionalLight) -> BakeParams {
    BakeParams {
        light_dir: light.direction,
        light_color: light.color * light.intensity,
        sky_color: settings.sky_color.unwrap_or(light.ambient),
        shadow_softness: 0.0,
        sky_samples: 1,
        bounces: settings.bounces,
    }
}

/// DDGI 每帧系统（见模块文档）。本帧重照明的探针数写进
/// `DdgiGrid::stats().last_updated`（渲染器抄进 `FrameStats::ddgi_updated`）。
pub fn tick_ddgi(world: &World) {
    // 阶段一（收集）：读毕即放锁——`collect_bake_triangles` 与阶段二的写锁
    // 都不得与这些读锁并存（AGENTS.md 的两阶段规约）
    let Some(settings) = world.try_resource::<DdgiSettings>().map(|s| *s) else {
        return; // 缺席 = DDGI 关（零分配零成本）
    };
    settings.validate();
    let Some(grid) = world.try_resource::<DdgiGrid>() else {
        return; // DdgiPlugin 未安装 ⇒ 无处写回（零操作，不 panic）
    };
    let needs_rebuild = grid.needs_rebuild(&settings);
    drop(grid);
    let light = world
        .try_resource::<DirectionalLight>()
        .map(|l| l.clone())
        .unwrap_or_default();
    assert!(
        light.direction.is_finite() && light.direction.length_squared() > 0.0,
        "DirectionalLight.direction 必须是有限的非零向量（DDGI 重照明用它做硬影射线）：{:?}",
        light.direction
    );
    let params = ddgi_bake_params(&settings, &light);
    // 只有（重）建帧才收集三角形：稳态帧一次都不跑（诚实限制④的成本面）
    let triangles = if needs_rebuild {
        Some(collect_bake_triangles(world))
    } else {
        None
    };
    // 阶段二（写回）：尝试型写锁，冲突即 panic（消息含 "already borrowed"）
    let mut grid = world.resource_mut::<DdgiGrid>();
    if let Some(triangles) = triangles {
        grid.install(&settings, &params, triangles);
    }
    grid.tick(&settings, &params);
}

/// 把 DDGI 装进世界并跑到收敛（example / GPU 测试的一次性装配入口）。
///
/// `frames` 省略（`None`）⇒ 取 `⌈n/k⌉`（解析收敛帧数，见
/// `iris_render::ddgi` 的确定性论证）。返回装配好的网格，由调用方
/// `world.insert_resource(grid)` 装回（渲染器下一帧即走 mode = 2）。
///
/// **注意**：这是**测试 / 快照装配**用的同步入口，不是生产路径——生产路径
/// 走 [`tick_ddgi`] 每帧滚动（安装帧只有粗填值，前 `⌈n/k⌉` 帧逐步精修）。
pub fn converge_ddgi(world: &World, frames: Option<usize>) -> DdgiGrid {
    let settings = world
        .try_resource::<DdgiSettings>()
        .map(|s| *s)
        .expect("converge_ddgi 需要世界带 DdgiSettings");
    settings.validate();
    let light = world
        .try_resource::<DirectionalLight>()
        .map(|l| l.clone())
        .unwrap_or_default();
    let params = ddgi_bake_params(&settings, &light);
    let mut grid = DdgiGrid::new();
    grid.install(&settings, &params, collect_bake_triangles(world));
    let n = grid.probe_count();
    let want = frames.unwrap_or_else(|| n.div_ceil(settings.probes_per_frame.max(1)));
    for _ in 0..want {
        grid.tick(&settings, &params);
    }
    assert_eq!(
        grid.stats().dirty_count,
        0,
        "{want} 帧后必须收敛（n = {n}、k = {}）",
        settings.probes_per_frame
    );
    grid
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Quat, Vec3};
    use iris_core::hierarchy::propagate_transforms;
    use iris_core::prelude::Transform;
    use iris_render::ddgi::{
        DDGI_DEPTH_GRID, DDGI_DEPTH_TEXELS, DDGI_SKY_DEPTH, ddgi_depth_weight_at,
    };
    use iris_render::material::{MaterialDef, MaterialStore};
    use iris_render::mesh::{MeshData, MeshStore};

    fn scaled(position: Vec3, scale: Vec3) -> Transform {
        Transform {
            position,
            rotation: Quat::IDENTITY,
            scale,
        }
    }

    /// 12 m 见方的地板 + x = 0 处的隔墙（z ∈ [−6,−2] 与 [2,6] 两段 ⇒
    /// z ∈ (−2,2) 是唯一门口）。墙两侧同一 z 的两个探针之间**没有**直射
    /// 视线 ⇒ 深度项有真实的遮挡信号可测（§8.52① 的最小复现场景）。
    fn room_world() -> World {
        let mut world = World::new();
        world.insert_resource(MeshStore::new());
        world.insert_resource(MaterialStore::new());
        world.insert_resource(DirectionalLight::default());
        let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(12.0));
        let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
        let gray = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.8, 0.78, 0.74, 1.0));
        world.spawn((Transform::IDENTITY, plane, gray));
        for z in [-4.0f32, 4.0] {
            world.spawn((
                scaled(Vec3::new(0.0, 2.0, z), Vec3::new(0.2, 4.0, 4.0)),
                cube,
                gray,
            ));
        }
        propagate_transforms(&mut world);
        world
    }

    /// 5×1×5 = 25 探针（格心 x/z ∈ {−4,−2,0,2,4}、y = 1.5），k = usize::MAX
    /// ⇒ 安装后首帧即全表精修（单测不必等 `⌈n/k⌉` 帧）。
    fn settings() -> DdgiSettings {
        DdgiSettings {
            spacing: 2.0,
            bounds: Some((Vec3::new(-5.0, 0.5, -5.0), Vec3::new(5.0, 3.5, 5.0))),
            probes_per_frame: usize::MAX,
            rays_per_face: 8,
            ..DdgiSettings::default()
        }
    }

    fn l0_of(world: &World) -> Vec<f32> {
        world
            .try_resource::<DdgiGrid>()
            .map(|g| g.sh().iter().map(|c| c[0][0]).collect())
            .expect("网格必须在场")
    }

    /// 口径映射：六个字段的解析对照（含「不乘 env intensity」与 ambient 兜底）。
    #[test]
    fn bake_params_follow_the_documented_mapping() {
        let light = DirectionalLight {
            direction: Vec3::new(-0.4, -1.0, 0.2).normalize(),
            color: Vec3::new(1.0, 0.8, 0.6),
            intensity: 2.5,
            ambient: Vec3::new(0.1, 0.2, 0.3),
            ..DirectionalLight::default()
        };
        let s = DdgiSettings::default();
        let p = ddgi_bake_params(&s, &light);
        assert_eq!(p.light_dir, light.direction, "传播方向同口径（不再归一化）");
        assert_eq!(
            p.light_color,
            Vec3::new(2.5, 2.0, 1.5),
            "= color × intensity"
        );
        assert_eq!(p.sky_color, light.ambient, "None ⇒ 取 ambient（裁定 3）");
        assert_eq!(p.shadow_softness, 0.0, "硬影单射线");
        assert_eq!(p.sky_samples, 1, "本路径不做半球采样");
        assert_eq!(p.bounces, 1);
        // 显式覆盖 sky_color
        let overridden = DdgiSettings {
            sky_color: Some(Vec3::new(0.5, 0.25, 0.125)),
            ..s
        };
        assert_eq!(
            ddgi_bake_params(&overridden, &light).sky_color,
            Vec3::new(0.5, 0.25, 0.125)
        );
        // bounces 透传（0 = 只天空）
        assert_eq!(
            ddgi_bake_params(&DdgiSettings { bounces: 0, ..s }, &light).bounces,
            0
        );
        // DirectionalLight 缺席 ⇒ 用 Default（ambient = 0.14）
        let fallback = ddgi_bake_params(&s, &DirectionalLight::default());
        assert_eq!(fallback.sky_color, Vec3::splat(0.14));
    }

    /// 缺席语义：settings / grid 任一缺席 ⇒ 零操作（不 panic、不改世界）。
    #[test]
    fn absent_resources_are_zero_ops() {
        let mut world = room_world();
        tick_ddgi(&world);
        assert!(
            !world.contains_resource::<DdgiGrid>(),
            "settings 缺席 ⇒ 不得凭空插入网格"
        );

        world.insert_resource(settings());
        tick_ddgi(&world);
        assert!(
            !world.contains_resource::<DdgiGrid>(),
            "grid 缺席 ⇒ 零操作（DdgiPlugin 未装）"
        );

        world.insert_resource(DdgiGrid::new());
        tick_ddgi(&world);
        let n = world.try_resource::<DdgiGrid>().map(|g| g.probe_count());
        assert_eq!(n, Some(25), "5×1×5 格心 ⇒ 25 探针");
        assert_eq!(
            world
                .try_resource::<DdgiGrid>()
                .map(|g| g.stats().last_updated),
            Some(25),
            "k = usize::MAX ⇒ 安装后首帧即全表精修"
        );
        // 稳态：第二帧零重照明（恒定光照 + 静止几何）
        tick_ddgi(&world);
        assert_eq!(
            world
                .try_resource::<DdgiGrid>()
                .map(|g| g.stats().last_updated),
            Some(0),
            "稳态必须零重照明（决策 8）"
        );
        assert_eq!(
            world.try_resource::<DdgiGrid>().map(|g| g.stats().ticks),
            Some(3),
            "tick 计数照常累加（观测口径）：安装帧占 1（粗填的 stamp 时钟）+ 两次 tick = 3"
        );
    }

    /// 几何 generation 递增 ⇒ 下一帧重建；稳态帧不重建（收集一次都不跑）。
    #[test]
    fn geometry_generation_triggers_rebuild() {
        let mut world = room_world();
        world.insert_resource(settings());
        world.insert_resource(DdgiGrid::new());
        tick_ddgi(&world);
        let (structure, triangles) = world
            .try_resource::<DdgiGrid>()
            .map(|g| (g.structure_generation(), g.bvh().triangle_count()))
            .unwrap();
        assert_eq!(structure, 1, "安装帧 ⇒ 结构版本 1");
        // plane = 1 个面片 = 2 三角；cube = 6 面 × 2 = 12 三角 ⇒ 地板 + 两面墙 = 26
        assert_eq!(triangles, 2 + 2 * 12, "BVH 必须收齐静态几何：{triangles}");
        tick_ddgi(&world);
        assert_eq!(
            world
                .try_resource::<DdgiGrid>()
                .map(|g| g.structure_generation()),
            Some(structure),
            "稳态帧不得改结构版本"
        );
        // 宣告几何变化 ⇒ 重建
        world.insert_resource(DdgiSettings {
            geometry_generation: 7,
            ..settings()
        });
        tick_ddgi(&world);
        assert_eq!(
            world
                .try_resource::<DdgiGrid>()
                .map(|g| g.structure_generation()),
            Some(structure + 1),
            "generation 变 ⇒ 重建（结构版本递增 ⇒ 渲染器全量上传）"
        );
        assert_eq!(
            world
                .try_resource::<DdgiGrid>()
                .map(|g| g.geometry_generation()),
            Some(7)
        );
        assert_eq!(
            world
                .try_resource::<DdgiGrid>()
                .map(|g| g.stats().dirty_count),
            Some(0),
            "重建当帧即全表精修（k = MAX）⇒ 无残留脏位"
        );
    }

    /// 昼夜联动（R80）：方向光当帧变化 ⇒ 当帧被 DDGI 消费（全表标脏 + 跟随）。
    #[test]
    fn light_change_is_consumed_same_frame() {
        let mut world = room_world();
        world.insert_resource(settings());
        world.insert_resource(DdgiGrid::new());
        tick_ddgi(&world);
        let before = l0_of(&world);
        tick_ddgi(&world);
        assert_eq!(before, l0_of(&world), "前提：已收敛且稳态不改表");
        // 当帧写方向光（DayNightPlugin 的行为）
        world.resource_mut::<DirectionalLight>().ambient = Vec3::splat(1.5);
        tick_ddgi(&world);
        assert_eq!(
            world
                .try_resource::<DdgiGrid>()
                .map(|g| g.stats().last_updated),
            Some(25),
            "光照变 ⇒ 全表标脏 ⇒ 本帧全表重照明（k = MAX）"
        );
        let after = l0_of(&world);
        let brighter = before.iter().zip(&after).filter(|(a, b)| *b > *a).count();
        assert_eq!(
            brighter,
            before.len(),
            "环境光调亮 ⇒ 全部探针的 L0 必须上升"
        );
        // 单调跟随：再调暗一档 ⇒ 全部下降（不是「一次性跳变」）
        let mid = after.clone();
        world.resource_mut::<DirectionalLight>().ambient = Vec3::splat(0.05);
        tick_ddgi(&world);
        let dim = l0_of(&world);
        assert!(
            mid.iter().zip(&dim).all(|(a, b)| *b < *a),
            "环境光调暗 ⇒ 全部探针的 L0 必须下降"
        );
    }

    /// `converge_ddgi` 的解析收敛帧数 + 墙两侧串味被深度项压制（端到端）。
    #[test]
    fn converge_and_depth_suppression_end_to_end() {
        let mut world = room_world();
        world.insert_resource(DirectionalLight {
            // 从 +X 上方照进来 ⇒ 墙的东侧亮、西侧暗（漏光的方向性可判读）
            direction: Vec3::new(-1.0, -0.6, 0.0).normalize(),
            intensity: 3.0,
            ambient: Vec3::splat(0.08),
            ..DirectionalLight::default()
        });
        // 滚动档 k = 2 ⇒ 收敛帧数 = ⌈25/2⌉ = 13
        world.insert_resource(DdgiSettings {
            probes_per_frame: 2,
            ..settings()
        });
        let grid = converge_ddgi(&world, None);
        assert_eq!(grid.probe_count(), 25);
        assert_eq!(grid.stats().dirty_count, 0, "⌈n/k⌉ 帧后必须收敛");
        assert_eq!(grid.depth().len(), 25 * DDGI_DEPTH_TEXELS);
        // 深度表有真实信号（不是整表中性的假通过）
        let real = grid
            .depth()
            .iter()
            .filter(|m| m[0] < DDGI_SKY_DEPTH)
            .count();
        assert!(
            real > 0,
            "有墙有地板 ⇒ 必须有命中方向（否则深度项无信号可测）"
        );
        assert!(
            grid.depth().iter().all(|m| m[1] > 0.0),
            "σ² 必须被下限钳成非零（防 chebyshev 退化成硬阶跃）"
        );
        // 与「显式帧数」路径逐位一致（同一纯函数链、同一收敛点）
        let sh_bits = |g: &DdgiGrid| -> Vec<u32> {
            g.sh()
                .iter()
                .flat_map(|c| c.iter().flat_map(|v| v.iter().map(|f| f.to_bits())))
                .collect()
        };
        assert_eq!(
            sh_bits(&converge_ddgi(&world, Some(13))),
            sh_bits(&grid),
            "⌈n/k⌉ = 13 与显式 13 帧必须逐位一致"
        );
        // 墙两侧同一 z 的两个探针：x = −4 与 x = +4、z = −4（墙在 z ∈ [−6,−2]）
        let positions = grid.positions();
        let west = positions
            .iter()
            .position(|p| (p.x + 4.0).abs() < 1e-3 && (p.z + 4.0).abs() < 1e-3)
            .expect("西侧探针必须在网格上");
        let east = positions
            .iter()
            .position(|p| (p.x - 4.0).abs() < 1e-3 && (p.z + 4.0).abs() < 1e-3)
            .expect("东侧探针必须在网格上");
        let delta = positions[east] - positions[west];
        let dist = delta.length();
        assert_eq!(dist, 8.0, "两探针隔 8 m（4 格 × 2 m 间距）");
        let on = ddgi_depth_weight_at(grid.depth(), west, delta, dist, DDGI_DEPTH_GRID, true);
        let off = ddgi_depth_weight_at(grid.depth(), west, delta, dist, DDGI_DEPTH_GRID, false);
        println!("穿墙权重：开 {on} / 关 {off}（距离 {dist}）");
        assert_eq!(off, 1.0, "关掉深度项 ⇒ 恒 1（= R83 的串味现状）");
        assert!(
            on < 0.25,
            "隔墙方向的深度权重必须被显著抑制（μ ≈ 4 m ≪ dist = 8 m）：{on}"
        );
        // 门口方向（z = 0，无墙）不该被同等抑制 ⇒ 抑制是**方向性**的，
        // 不是「一律衰减」（否则整片 GI 都会变暗）
        let doorway = positions
            .iter()
            .position(|p| (p.x - 4.0).abs() < 1e-3 && p.z.abs() < 1e-3)
            .expect("门口方向的探针必须在网格上");
        let d2 = positions[doorway] - positions[west];
        let on_doorway =
            ddgi_depth_weight_at(grid.depth(), west, d2, d2.length(), DDGI_DEPTH_GRID, true);
        println!("门口方向权重：{on_doorway}（距离 {}）", d2.length());
        assert!(
            on_doorway > on * 2.0,
            "门口方向的权重必须显著高于穿墙方向：{on_doorway} vs {on}"
        );
        // 装回世界（渲染器下一帧即走 mode = 2）
        world.insert_resource(grid);
        assert_eq!(
            world.try_resource::<DdgiGrid>().map(|g| g.probe_count()),
            Some(25)
        );
    }

    /// 非法设置必须 panic 而不是静默降级（与 `BakeParams` 的校验文化一致）。
    #[test]
    #[should_panic(expected = "网格间距须为正有限值")]
    fn illegal_settings_panic_in_system() {
        let mut world = room_world();
        world.insert_resource(DdgiSettings {
            spacing: -1.0,
            ..settings()
        });
        world.insert_resource(DdgiGrid::new());
        tick_ddgi(&world);
    }

    /// 零长度方向光必须给出可解释的 panic（而不是 NaN 悄悄污染整表）。
    #[test]
    #[should_panic(expected = "有限的非零向量")]
    fn zero_light_direction_panics() {
        let mut world = room_world();
        world.insert_resource(settings());
        world.insert_resource(DdgiGrid::new());
        world.resource_mut::<DirectionalLight>().direction = Vec3::ZERO;
        tick_ddgi(&world);
    }
}
