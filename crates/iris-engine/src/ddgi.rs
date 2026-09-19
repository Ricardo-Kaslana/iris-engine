//! DDGI 每帧编排（R88 + C7a v3）：把 [`DdgiSettings`] + [`DirectionalLight`]
//! 派生成 [`BakeParams`]，连同 **C7a 重照明输入**（[`ddgi_relight_inputs`]：
//! `lights_in_gi` 门控的灯集 + `sky_from_source` 门控的同源天空快照）
//! 喂给 [`DdgiGrid`] 的时间片滚动重照明。
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
use iris_render::ddgi::{DdgiGrid, DdgiSettings, DdgiSky, GiLight, RelightInputs};
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

/// 收集本帧参与 GI 的动态灯集（C7a，§8.57② 的闭合件）。
///
/// 组装序与主渲染灯表同构：**点光查询序 → 聚光查询序追加**，合计裁剪到
/// [`DDGI_MAX_GI_LIGHTS`] 盏（超出 warn——主渲染分箱可容 512 盏，GI 侧受
/// 射线预算独立封顶，>8 盏时两侧事实源可不同，§8.67③）。逐字段口径镜像
/// `renderer.rs` 的 Globals/binned 组装：`color × intensity`、聚光
/// `inner.min(outer)` 钳制、零长度方向 = 颜色置零（不发光）。
/// `shadows` 由宿主从 `DdgiSettings::point_light_shadows` 扇入逐灯。
pub fn ddgi_gi_lights(world: &World, shadows: bool) -> Vec<GiLight> {
    use iris_core::prelude::GlobalTransform;
    use iris_render::light::{PointLight, SpotLight};
    let mut lights: Vec<GiLight> = Vec::new();
    let mut total = 0usize;
    world.query::<(&GlobalTransform, &PointLight)>(|(gt, pl)| {
        total += 1;
        if lights.len() >= iris_render::ddgi::DDGI_MAX_GI_LIGHTS {
            return;
        }
        lights.push(GiLight::point(
            gt.position(),
            pl.color * pl.intensity,
            pl.range,
            shadows,
        ));
    });
    world.query::<(&GlobalTransform, &SpotLight)>(|(gt, sl)| {
        total += 1;
        if lights.len() >= iris_render::ddgi::DDGI_MAX_GI_LIGHTS {
            return;
        }
        let color = if sl.direction.length_squared() == 0.0 {
            glam::Vec3::ZERO
        } else {
            sl.color * sl.intensity
        };
        let inner = sl.inner_cutoff.min(sl.outer_cutoff);
        lights.push(GiLight::spot(
            gt.position(),
            color,
            sl.range,
            sl.direction.normalize_or_zero(),
            inner.cos(),
            sl.outer_cutoff.cos(),
            shadows,
        ));
    });
    if total > iris_render::ddgi::DDGI_MAX_GI_LIGHTS {
        log::warn!(
            "场景有 {total} 盏动态灯，GI 侧容量 {} 盏（超出部分不进间接光；主渲染直接光不受影响）",
            iris_render::ddgi::DDGI_MAX_GI_LIGHTS
        );
    }
    lights
}

/// 同源天空快照（C7a 裁决 B1 甲的优先级链，见
/// [`DdgiSettings::sky_from_source`] 文档）：
///
/// 1. 开关关 **或** `settings.sky_color = Some`（作者覆盖恒胜）⇒ `None`
///    （未命中射线吃常量，现口径逐位不变）；
/// 2. `SkySettings.atmosphere = true` ⇒ `None` + 进程级 warn 一次
///    （大气 CPU 镜像未实现，拒绝用渐变冒充，§8.57① 对该场景维持）；
/// 3. `SkySource` 缺席 ⇒ [`DdgiSky::Procedural`]（可见天空同为程序化渐变，
///    同源成立）；`EquirectHdr` ⇒ Arc 快照（零拷贝）+ generation 入指纹。
pub fn ddgi_sky_snapshot(world: &World, settings: &DdgiSettings) -> Option<DdgiSky> {
    use iris_render::environment::{SkySource, SkySourceKind};
    use iris_render::post::SkySettings;
    if !settings.sky_from_source || settings.sky_color.is_some() {
        return None;
    }
    let atmosphere = world
        .try_resource::<SkySettings>()
        .is_some_and(|s| s.atmosphere);
    if atmosphere {
        static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            log::warn!(
                "DDGI sky_from_source 在 SkySettings.atmosphere = true 的场景不生效\
                 （大气散射 CPU 镜像未实现，§8.57① 维持）：回退常量天空底"
            );
        }
        return None;
    }
    Some(match world.try_resource::<SkySource>() {
        None => DdgiSky::Procedural,
        Some(sky) => match &sky.kind {
            SkySourceKind::ProceduralSky => DdgiSky::Procedural,
            SkySourceKind::EquirectHdr(image) => DdgiSky::Hdr {
                image: image.clone(),
                generation: sky.generation,
            },
        },
    })
}

/// 组装本帧重照明扩展输入（灯集 + 同源天空），供 [`tick_ddgi`] /
/// [`converge_ddgi`] 在收集阶段调用（全部只读锁，与两阶段规约兼容）。
pub fn ddgi_relight_inputs(world: &World, settings: &DdgiSettings) -> RelightInputs {
    RelightInputs {
        lights: if settings.lights_in_gi {
            ddgi_gi_lights(world, settings.point_light_shadows)
        } else {
            Vec::new()
        },
        sky: ddgi_sky_snapshot(world, settings),
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
    // C7a：灯集 / 同源天空快照（阶段一只读收集，阶段二消费克隆好的值）
    let inputs = ddgi_relight_inputs(world, &settings);
    // 只有（重）建帧才收集三角形：稳态帧一次都不跑（诚实限制④的成本面）
    let triangles = if needs_rebuild {
        Some(collect_bake_triangles(world))
    } else {
        None
    };
    // 阶段二（写回）：尝试型写锁，冲突即 panic（消息含 "already borrowed"）
    let mut grid = world.resource_mut::<DdgiGrid>();
    if let Some(triangles) = triangles {
        grid.install_with(&settings, &params, triangles, &inputs);
    }
    grid.tick_with(&settings, &params, &inputs);
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
    let inputs = ddgi_relight_inputs(world, &settings);
    let mut grid = DdgiGrid::new();
    grid.install_with(&settings, &params, collect_bake_triangles(world), &inputs);
    let n = grid.probe_count();
    let want = frames.unwrap_or_else(|| n.div_ceil(settings.probes_per_frame.max(1)));
    for _ in 0..want {
        grid.tick_with(&settings, &params, &inputs);
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

    // ------------------------------------------------------------------
    // C7a · 灯集收集 / 天空快照 / 端到端（c7-design-v1 裁定甲案）
    // ------------------------------------------------------------------

    /// 灯集收集与主渲染灯表逐字段同口径：`color × intensity`、聚光
    /// `inner.min(outer)` 钳制、零长度方向 = 颜色置零、点先聚光后、容量 8。
    #[test]
    fn gi_lights_collection_mirrors_main_table_semantics() {
        use iris_render::light::{PointLight, SpotLight};
        let mut world = room_world();
        world.spawn((
            Transform::from_translation(Vec3::new(-2.0, 1.0, -2.0)),
            PointLight {
                color: Vec3::new(1.0, 0.5, 0.2),
                intensity: 3.0,
                range: 7.0,
                shadows: false,
            },
        ));
        // 倒挂锥（inner 60° > outer 30° ⇒ 钳硬边）+ 零方向锥（不发光）
        world.spawn((
            Transform::from_translation(Vec3::new(2.0, 3.0, 2.0)),
            SpotLight {
                color: Vec3::splat(2.0),
                intensity: 1.5,
                range: 9.0,
                direction: Vec3::new(0.0, -2.0, 0.0), // 未归一化（收集侧 normalize_or_zero）
                inner_cutoff: 60f32.to_radians(),
                outer_cutoff: 30f32.to_radians(),
            },
        ));
        world.spawn((
            Transform::from_translation(Vec3::new(0.0, 2.0, 4.0)),
            SpotLight {
                direction: Vec3::ZERO,
                ..SpotLight::default()
            },
        ));
        propagate_transforms(&mut world);
        let lights = ddgi_gi_lights(&world, true);
        assert_eq!(lights.len(), 3);
        let p = &lights[0];
        assert_eq!(p.color, Vec3::new(3.0, 1.5, 0.6), "= color × intensity");
        assert_eq!(p.range, 7.0);
        assert!(!p.is_spot);
        assert!(lights.iter().all(|l| l.shadows), "shadows 由宿主扇入");
        let s = &lights[1];
        assert!(s.is_spot);
        assert_eq!(s.axis, Vec3::NEG_Y, "轴归一化");
        assert_eq!(
            s.cos_inner.to_bits(),
            s.cos_outer.to_bits(),
            "inner > outer 钳到硬边（= cos 30°）"
        );
        assert_eq!(s.color, Vec3::splat(3.0));
        assert_eq!(
            lights[2].color,
            Vec3::ZERO,
            "零方向锥 = 不发光（颜色置零镜像）"
        );
        // 容量 8：9 盏点光 ⇒ 前 8 盏
        let mut dense = room_world();
        for i in 0..9 {
            dense.spawn((
                Transform::from_translation(Vec3::new(i as f32, 1.0, 0.0)),
                PointLight::default(),
            ));
        }
        propagate_transforms(&mut dense);
        assert_eq!(ddgi_gi_lights(&dense, false).len(), 8, "GI 容量 8 盏裁剪");
    }

    /// 天空快照优先级链：关 / 作者覆盖 / atmosphere ⇒ None；开且无
    /// SkySource ⇒ Procedural；EquirectHdr ⇒ Arc 快照带 generation。
    #[test]
    fn sky_snapshot_priority_chain() {
        use iris_render::asset::hdr::HdrImage;
        use iris_render::environment::{SkySource, SkySourceKind};
        use iris_render::post::SkySettings;
        let mut world = room_world();
        let on = DdgiSettings {
            sky_from_source: true,
            ..DdgiSettings::default()
        };
        // ① 开关关 ⇒ None
        assert!(ddgi_sky_snapshot(&world, &DdgiSettings::default()).is_none());
        // ② 作者显式 sky_color ⇒ 覆盖恒胜
        let override_sky = DdgiSettings {
            sky_from_source: true,
            sky_color: Some(Vec3::splat(0.3)),
            ..DdgiSettings::default()
        };
        assert!(ddgi_sky_snapshot(&world, &override_sky).is_none());
        // ③ atmosphere ⇒ 拒绝冒充，None（§8.57① 维持）
        world.insert_resource(SkySettings {
            atmosphere: true,
            ..SkySettings::default()
        });
        assert!(ddgi_sky_snapshot(&world, &on).is_none());
        // ④ atmosphere 关 + 无 SkySource ⇒ 程序化镜像
        world.insert_resource(SkySettings::default());
        assert!(matches!(
            ddgi_sky_snapshot(&world, &on),
            Some(DdgiSky::Procedural)
        ));
        // ⑤ HDR 全景 ⇒ Arc 快照 + generation 透传
        let sky = SkySource::equirect_hdr(HdrImage::new(4, 2));
        let next_gen = sky.generation;
        assert_ne!(next_gen, 0, "构造口必须发新代数");
        let generation = match &sky.kind {
            SkySourceKind::EquirectHdr(_) => next_gen,
            _ => panic!("构造口应产 EquirectHdr"),
        };
        world.insert_resource(sky);
        match ddgi_sky_snapshot(&world, &on) {
            Some(DdgiSky::Hdr {
                image,
                generation: g,
            }) => {
                assert_eq!(g, generation);
                assert_eq!(image.width, 4);
            }
            other => panic!("期望 Hdr 快照，得到 {other:?}"),
        }
    }

    /// 端到端：`lights_in_gi` 开 ⇒ 火把当帧进表（全表标脏 + L0 上升可判别）；
    /// 开关开但世界无灯 ⇒ 与开关关**逐位一致**（收集侧空集旁路的引擎级证据）。
    #[test]
    fn torch_enters_table_when_gated_on_and_bitwise_absent_when_empty() {
        let mut world = room_world();
        world.insert_resource(settings());
        world.insert_resource(DdgiGrid::new());
        tick_ddgi(&world); // 安装 + 全表精修（k = MAX）
        let off = l0_of(&world);
        let torch_pos = Vec3::new(3.0, 1.6, 3.0);
        world.spawn((
            Transform::from_translation(torch_pos),
            iris_render::light::PointLight {
                color: Vec3::splat(1.0),
                intensity: 8.0,
                range: 6.0,
                shadows: false,
            },
        ));
        propagate_transforms(&mut world);
        // 开关仍关 ⇒ 与 off 逐位一致（灯在场但门没开）
        tick_ddgi(&world);
        assert_eq!(off, l0_of(&world), "lights_in_gi 关 ⇒ 灯在场也逐位不变");
        // 开门 ⇒ 全表标脏重照明
        world.insert_resource(DdgiSettings {
            lights_in_gi: true,
            ..settings()
        });
        tick_ddgi(&world);
        let on = l0_of(&world);
        let brighter = off.iter().zip(&on).filter(|(a, b)| *b > *a).count();
        assert!(
            brighter >= off.len() / 2,
            "火把应抬亮过半探针的 L0：{brighter}/{}",
            off.len()
        );
        // 空集旁路逐位锚：无灯世界开门 ≡ 关门的同世界表
        let mut bare = room_world();
        bare.insert_resource(DdgiSettings {
            lights_in_gi: true,
            ..settings()
        });
        bare.insert_resource(DdgiGrid::new());
        tick_ddgi(&bare);
        let mut closed = room_world();
        closed.insert_resource(settings());
        closed.insert_resource(DdgiGrid::new());
        tick_ddgi(&closed);
        let bits = |w: &World| -> Vec<u32> {
            w.resource::<DdgiGrid>()
                .sh()
                .iter()
                .flat_map(|c| c.iter().flat_map(|v| v.iter().map(|f| f.to_bits())))
                .collect()
        };
        assert_eq!(bits(&bare), bits(&closed), "空灯集 ⇒ 逐位等于开关关");
    }

    /// `sky_from_source` 端到端：无 SkySource 场景开同源 ⇒ 程序化渐变进表。
    /// 判别口径：蓝通道多数探针上升（天空半球蓝 ≫ ambient 0.14）+ 角落探针
    /// 反向变暗（地板外沿的下方射线落程序化地面色，蓝 ≈0.054）——方向性
    /// 生效的双向证据；整表逐位 ≠ 关档。关档逐位由 render 侧薄壳锚定。
    #[test]
    fn sky_from_source_brightens_table_vs_ambient_constant() {
        let bits_of = |w: &World| -> Vec<u32> {
            w.resource::<DdgiGrid>()
                .sh()
                .iter()
                .flat_map(|c| c.iter().flat_map(|v| v.iter().map(|f| f.to_bits())))
                .collect()
        };
        let mut off_world = room_world();
        off_world.insert_resource(settings());
        off_world.insert_resource(DdgiGrid::new());
        tick_ddgi(&off_world);
        let off_bits = bits_of(&off_world);

        let mut on_world = room_world();
        on_world.insert_resource(DdgiSettings {
            sky_from_source: true,
            ..settings()
        });
        on_world.insert_resource(DdgiGrid::new());
        tick_ddgi(&on_world);
        let on_bits = bits_of(&on_world);

        assert_ne!(on_bits, off_bits, "天空同源开/关整表必须可判别");
        // 判据取蓝通道 L0：程序化天空蓝（地平 0.94 / 天顶 0.94）在**任何**
        // 未命中方向都 ≫ ambient 0.14；红通道不做全升断言——程序化地面色
        // 红 ≈0.132 略低于 ambient 红，贴地探针朝下看会真的变暗（可判别的
        // 方向性正是同源的意义）。
        let blue_l0 = |w: &World| -> Vec<f32> {
            w.resource::<DdgiGrid>()
                .sh()
                .iter()
                .map(|c| c[0][2])
                .collect()
        };
        let off_b = blue_l0(&off_world);
        let on_b = blue_l0(&on_world);
        let brighter = on_b.iter().zip(&off_b).filter(|(a, b)| **a > **b).count();
        // 过半探针蓝 L0 上升（天空半球 ≫ ambient），而**地板外沿的角落探针
        // 反而变暗**——越出 12 m 平面的下方射线落在程序化地面色（蓝 ≈0.054
        // < ambient 0.14）：方向性生效的正当物理证据，不是缺陷。
        assert!(
            brighter >= off_b.len() * 4 / 5,
            "多数探针蓝 L0 应上升：{brighter}/{}",
            off_b.len()
        );
        assert!(
            brighter < off_b.len(),
            "角落探针应被地面色拉暗（方向性证据）"
        );
        assert!(
            on_b.iter().sum::<f32>() > off_b.iter().sum::<f32>(),
            "整表蓝能量必须净上升"
        );
    }
}
