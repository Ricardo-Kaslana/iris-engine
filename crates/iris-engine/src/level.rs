//! 关卡格式 v2（R57）：场景文件的 `level` 段——静态世界的数据地基。
//!
//! **关卡档 = 场景档（v1）的超集**：顶层 `version` 字段 + `level` 段由
//! [`crate::scene`] 的加载 / 保存主体统一处理（单通路，无第二套 IO）。
//! 分工边界：
//!
//! - **关卡档管静态世界**：地形（heightmap + 建造参数）、静态碰撞体、
//!   装饰网格（图元 / glTF 实例，v1 实体段既有能力）、光照（方向光 =
//!   `environment` 资源段先例；点光 = 实体段 `point_light`——核实结论：
//!   v1 已全覆盖 [`PointLight`](iris_render::prelude::PointLight) 四字段
//!   （color / intensity / range / shadows），无缺口；本轮补齐方向光
//!   `shadow_range` 的可选序列化）、天空源（`environment.sky_source`）、
//!   NavGrid 构建参数、光照贴图烘焙参数（产物外置文件）、命名锚点
//!   （出生点 / 触发区 = 实体段的 `name` + `transform`——**关卡格式保持
//!   游戏语义中立**，没有 "player_spawn" 这类引擎级专用字段）；
//! - **游戏代码管动态实体**：玩家、敌人、HUD、粒子池等仍由 plugin 侧 spawn。
//!
//! ```jsonc
//! {
//!   "version": 2,                                   // 缺席 = v1 纯场景档
//!   "environment": { /* 方向光 / 天空源，v1 既有 */ },
//!   "level": {                                      // 全部子段可选
//!     "terrain": {
//!       "heightmap": { "hills": {                   // 程序化生成器参数
//!         "width": 65, "depth": 65, "cell_size": 0.5,
//!         "seed": 2026, "base": 0.8, "amplitude": 1.2 } },
//!         // 或 { "inline": { "width", "depth", "cell_size", "heights": [..] } }
//!       "material": { /* 实体段同款材质段（含程序化纹理源） */ },
//!       "options": { "origin": [0,0,0], "chunk_cells": 32,
//!                    "collider_stride": 1, "uv_tile": 4.0,
//!                    "switch_distances": [16.0, 40.0] }
//!     },
//!     "nav": {                                      // 只存参数不存烘焙格子
//!       "bounds_min": [-10, 0, -10], "bounds_max": [10, 0, 10],
//!       "cell_size": 0.5, "agent_radius": 0.4,
//!       "step_height": 0.3, "max_slope_radians": 0.7853982
//!     },
//!     "bake": {                                     // 烘焙参数 + 产物引用
//!       "light_dir": [0,-1,0], "light_color": [1,1,1], "sky_color": [0.4,0.4,0.4],
//!       "shadow_softness": 0.0, "sky_samples": 64, "bounces": 1,
//!       "texels_per_meter": 4.0, "atlas_size": 512, "dilate_iterations": 8,
//!       "artifact": "courtyard.ilmap"               // 缺失 = 只存参数
//!     }
//!   },
//!   "entities": [ /* v1 实体段 + 本轮新增 particles / 材质纹理槽 */ ]
//! }
//! ```
//!
//! # 各子段的加载语义
//!
//! - **terrain**：heightmap 两态——`hills`（[`generate_hills`] 生成器全
//!   参数，无 RNG、同 seed 逐位一致）或 `inline`（行主序高度数组）。
//!   经 [`build_terrain`] 铺进世界：根 / chunk / 碰撞拼块实体全部打
//!   [`LevelGenerated`] 标记——save 时跳过（由 terrain 段在下次加载时
//!   整体重建，与 glTF 子树的 [`GltfMounted`](crate::scene::GltfMounted)
//!   同一模式）。取舍：**不引用外置高度场文件**——v1 引擎没有高度场
//!   文件装载器（.r16 / PNG 高度图均无），生成器参数与内联数组是与现状
//!   最贴合的两种表达；大地形内联 JSON 体积大（129² ≈ 16k 浮点数），
//!   外置二进制高度场留待编辑器轮；
//! - **nav**：**只存 [`NavGridParams`] 全字段，不存烘焙格子**——加载后
//!   由世界内的静态碰撞体（无 `RigidBody` 或 `is_static`，排除传感器；
//!   收集序 = 组件存储序，确定性）经 [`NavGrid::build`] 重建并存入 World
//!   资源。烘焙格子不入库的理由：位图是静态几何的纯函数（毫秒级重建，
//!   §3.9「便宜重建」策略），存格子 = 存冗余 + 引入「格子与几何失配」
//!   这一整类陈旧态。已知边界（§3.9 实测教训的格式侧对应）：被静态盒
//!   **全覆盖**的导航域没有顶面雕刻种子（如整块地面盒）——此类关卡
//!   的地面应经 terrain 段或隐式地面表达，nav 段照常工作；
//! - **bake**：烘焙**参数**（[`BakeParams`] + [`BakeUvParams`] 全字段，
//!   对齐 `bake_lightmap` 的实际入参）+ `dilate_iterations`（记录性字段：
//!   dilation 轮数是引擎常量 [`LIGHTMAP_DILATE_ITERATIONS`]，非运行期
//!   入参——与常量不符时告警）+ **产物文件名引用**。烘焙产物存独立
//!   外置文件（[`save_lightmap_artifact`]，二进制 IRISLMAP 格式见下）；
//!   加载时产物**存在则挂**（校验通过后 [`install_lightmap`]），**缺失
//!   则诚实降级为无光照贴图并告警**（不报错——参数仍可驱动重烘）。
//!
//! # 光照贴图产物文件（IRISLMAP v1，全部小端）
//!
//! ```text
//! ┌───────────┬──────────┬────────────┬────────────┬───────────┬──────────────┬──────────┐
//! │ "IRISLMAP"│ 版本 u32 │ 图集边长u32│ 已用高度u32│ 表项数 u32│ 布局表       │ texel 数 │
//! │ 8 B       │ = 1      │            │            │           │ 见下         │ u64      │
//! └───────────┴──────────┴────────────┴────────────┴───────────┴──────────────┴──────────┘
//! 布局表（逐 chart 表项，紧密排列）：
//!   entity_bits u64 │ 有 mesh u8 + mesh u32 │ axis u8（0=Floor 1=WallX 2=WallZ，
//!   ChartAxis 声明序）│ uv_rect [f32;4] │ origin [u32;2] │ size [u32;2] │
//!   texels_per_meter f32 │ 三角数 u32 │ 逐三角 { 全局下标 u64, uv_rect [f32;4],
//!   local [[f32;2];3] }
//! 数据区：radiance texel数×[f32;3] │ indirect texel数×[f32;3] │ covered texel数×u8
//! ```
//!
//! [`BakedLightmap`] 全字段无损（`radiance` 是预览 / 调试口径，运行时
//! 只用 `indirect`——完整存储保住 `write_lightmap_preview` 等工具通路）。
//! 同输入两次 [`encode_lightmap_artifact`] 产出**逐位相同**的字节
//! （纯函数、无时间戳 / 哈希序——确定性文化，测试锁定）。选型：自定义
//! 小端二进制而非 JSON——辐照数组是 `size²` 量级的浮点体（512² ≈ 13 MB
//! 十进制文本 vs 6 MB 二进制），且位保真无需十进制往返论证（f32 原样
//! 字节）；风格照 pack.rs 的格式文档规约（魔数 + 版本 + 布局图）。
//!
//! **产物与关卡几何的一致性**：布局表按 `Entity::to_bits` 与 `MeshHandle`
//! 引用实体——产物必须产自「同一关卡档加载出的世界」（确定性加载 ⇒
//! 实体位模式与句柄可复现）。挂载前逐项校验（实体在世、当前
//! `MeshHandle` 与表项一致、三角数与网格一致、全局下标逐实体连续），
//! 任一不符报 [`SceneError::Unsupported`]（**陈旧产物是错误不是降级**——
//! 静默挂错位的阴影比不挂更糟）；校验通过才 [`install_lightmap`]（其
//! 内部断言因此不可达）。挂载后把 UV2 副本句柄的来源登记回
//! [`SceneAssetRegistry`](crate::scene::SceneAssetRegistry)（照抄原句柄
//! 的图元 / glTF 来源）——save 时实体 mesh 段仍还原为原始来源，重载后
//! 产物重挂，往返闭环。
//!
//! # 版本迁移
//!
//! `version` 缺席 = v1（纯场景档，无损直读）。迁移钩子函数表
//! `scene::MIGRATIONS`：v1 → v2 = 加默认（空）`level` 段。
//! 高于当前版本 → [`SceneError::Unsupported`]（旧引擎拒读新档，
//! 与 pack 格式「版本号 +1 旧 reader 拒读而非误读」同一策略）。
//! save 恒写当前版本；空 `level` 段省略（v1 档重存只多 `version` 字段）。
//!
//! # v1 不含（诚实清单）
//!
//! - **关卡档与游戏侧常量可能双源**：assault 的墙体 / 石柱同时存在于
//!   关卡档（视觉 + 物理）与 `logic.rs` 常量（纯逻辑碰撞 / 导航）——
//!   本轮以对照测试钉住一致性，单一事实来源留待编辑器轮（R58）；
//! - [`LevelRegistry`] 是**加载时快照**：代码侧改 `Terrain` / `NavGrid`
//!   / 烘焙参数不回写（save 还原登记值，与材质「现读数值」语义不同——
//!   level 段的来源数据没有可现读的世界侧完整形态，如 heightmap 生成器
//!   参数与产物文件名）；
//! - 外置高度场文件（.r16 / PNG）与烘焙产物的 pack 打包工具链（产物经
//!   [`AssetMount`](crate::vfs::AssetMount) 可读，打包入口未接）；
//! - `level` 段无地形材质混合 / 植被散布（编辑器轮的地形笔刷数据）；
//! - 光照探针（§8.31① 的路线图项）不入档；反射探针（R62a，§4.30）**同样
//!   不入档**——`ReflectionProbe` 组件由宿主在运行时挂载并调用
//!   `Renderer::bake_reflection_probe` 烘焙，关卡档既不写探针也不写探针
//!   烘焙产物（持久化与 pack 集成是 R62a 拒绝清单里的后续轮项）。

use std::collections::BTreeMap;
use std::path::Path;

use iris_core::nav::{NavGrid, NavGridParams};
use iris_core::physics::{Collider, ColliderShape, RigidBody};
use iris_core::prelude::{Entity, Transform, Vec3, World};
use iris_core::{Component, Resource};
use iris_render::prelude::{
    AtlasEntry, AtlasTriangle, BakeParams, BakeUvParams, BakedLightmap, ChartAxis,
    DDGI_RAYS_PER_FACE_CHOICES, DdgiSettings, Heightmap, LIGHTMAP_DILATE_ITERATIONS, LightmapAtlas,
    MaterialHandle, MeshHandle, MeshStore, TerrainOptions, build_terrain, generate_hills,
    install_lightmap,
};
use serde::{Deserialize, Serialize};

use crate::scene::{
    LoadCache, MaterialSection, SceneAssetRegistry, SceneError, SubAssets, resolve_material,
};

// ---------------------------------------------------------------------------
// level 段
// ---------------------------------------------------------------------------

/// `level` 段：全部子段可选（缺席 = 该能力不启用）。序列化顺序 =
/// 字段声明序（terrain → nav → bake → ddgi），save 输出确定。
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct LevelSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terrain: Option<TerrainSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nav: Option<NavSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bake: Option<BakeSection>,
    /// DDGI 网格参数（R89）：缺席 = DDGI 不启用（能力不启用，与 nav/bake
    /// 同款语义）。加载 → `DdgiSettings` 资源；保存 → 从活动资源回写
    /// （`save_scene`，照 `DirectionalLight` 的资源回写先例）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ddgi: Option<DdgiSection>,
}

impl LevelSection {
    /// 全子段缺席（v1 迁移产出的默认段）：save 时整段省略。
    pub fn is_empty(&self) -> bool {
        self.terrain.is_none() && self.nav.is_none() && self.bake.is_none() && self.ddgi.is_none()
    }
}

/// 关卡档加载登记（World 资源）：`level` 段的加载时快照，
/// [`crate::scene::save_scene`] 据此原样回写。快照语义（代码侧修改不
/// 回写）见模块文档诚实清单。
#[derive(Clone, Debug, Default, Resource)]
pub struct LevelRegistry {
    /// 加载（或迁移）后的 `level` 段。
    pub section: LevelSection,
}

/// level 段派生实体标记：terrain 段经 [`build_terrain`] 生成的全部实体
/// （根 + chunk + 碰撞拼块）挂上，save 时跳过——它们由 terrain 段在下次
/// 加载时整体重建，不属于实体段（与 [`GltfMounted`](crate::scene::GltfMounted)
/// 同一模式）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Component)]
pub struct LevelGenerated;

// ---------------------------------------------------------------------------
// terrain 子段
// ---------------------------------------------------------------------------

/// terrain 子段：heightmap 来源 + 材质（可选，缺席 = 默认材质句柄 0）+
/// 建造参数（缺省 = [`TerrainOptions::default`]）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TerrainSection {
    pub heightmap: HeightmapSection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<MaterialSection>,
    #[serde(default)]
    pub options: TerrainOptionsSection,
}

/// heightmap 两态（外部标签，键名 `hills` / `inline`）：程序化生成器参数
/// 或内联高度数组。取舍论证见模块文档。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum HeightmapSection {
    /// [`generate_hills`] 全参数（无 RNG，同 seed 逐位一致）。`width` /
    /// `depth` / `cell_size` 必需；`seed` 缺省 0、`base` 缺省 0.0、
    /// `amplitude` 缺省 1.0。生成器的 `width.max(2)` 截断语义原样保留。
    Hills {
        width: u32,
        depth: u32,
        cell_size: f32,
        #[serde(default)]
        seed: u64,
        #[serde(default)]
        base: f32,
        #[serde(default = "default_hill_amplitude")]
        amplitude: f32,
    },
    /// 内联高度数据：行主序 `heights[j * width + i]`，长度必须恰为
    /// `width × depth`（不符 → [`SceneError::OutOfRange`]）。
    Inline {
        width: u32,
        depth: u32,
        cell_size: f32,
        heights: Vec<f32>,
    },
}

fn default_hill_amplitude() -> f32 {
    1.0
}

impl HeightmapSection {
    /// 构造高度场。断言前提先校验为 [`SceneError::OutOfRange`]
    /// （而非让 [`Heightmap::new`] panic）。
    fn to_heightmap(&self) -> Result<Heightmap, SceneError> {
        match self {
            Self::Hills {
                width,
                depth,
                cell_size,
                seed,
                base,
                amplitude,
            } => {
                if !(cell_size.is_finite() && *cell_size > 0.0) {
                    return Err(SceneError::OutOfRange(format!(
                        "terrain.heightmap.cell_size 必须为正的有限数，实际 {cell_size}"
                    )));
                }
                if !base.is_finite() || !amplitude.is_finite() || *amplitude < 0.0 {
                    return Err(SceneError::OutOfRange(format!(
                        "terrain.heightmap 要求 base 有限、amplitude 非负有限，\
                         实际 {base} / {amplitude}"
                    )));
                }
                Ok(generate_hills(
                    *width as usize,
                    *depth as usize,
                    *cell_size,
                    *seed,
                    *base,
                    *amplitude,
                ))
            }
            Self::Inline {
                width,
                depth,
                cell_size,
                heights,
            } => {
                if *width < 2 || *depth < 2 {
                    return Err(SceneError::OutOfRange(format!(
                        "heightmap 至少 2×2 顶点，实际 {width}×{depth}"
                    )));
                }
                if !(cell_size.is_finite() && *cell_size > 0.0) {
                    return Err(SceneError::OutOfRange(format!(
                        "terrain.heightmap.cell_size 必须为正的有限数，实际 {cell_size}"
                    )));
                }
                let expected = *width as usize * *depth as usize;
                if heights.len() != expected {
                    return Err(SceneError::OutOfRange(format!(
                        "terrain.heightmap.heights 长度 {} 须为 width × depth = {expected}",
                        heights.len()
                    )));
                }
                Ok(Heightmap::new(
                    *width as usize,
                    *depth as usize,
                    *cell_size,
                    heights.clone(),
                ))
            }
        }
    }
}

/// 建造参数段：字段缺省 = [`TerrainOptions::default`]（origin 零、
/// chunk_cells 32、collider_stride 1、uv_tile 4.0、切换距离 [16, 40]）。
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(default)]
pub struct TerrainOptionsSection {
    pub origin: [f32; 3],
    pub chunk_cells: u32,
    pub collider_stride: u32,
    pub uv_tile: f32,
    pub switch_distances: [f32; 2],
}

impl Default for TerrainOptionsSection {
    fn default() -> Self {
        let options = TerrainOptions::default();
        Self {
            origin: options.origin.to_array(),
            chunk_cells: options.chunk_cells as u32,
            collider_stride: options.collider_stride as u32,
            uv_tile: options.uv_tile,
            switch_distances: options.switch_distances,
        }
    }
}

impl TerrainOptionsSection {
    /// 转引擎建造参数并校验 [`build_terrain`] 的全部断言前提
    /// （整除性依赖 heightmap cell 数，故收 heightmap 入参）。
    fn to_options(self, heightmap: &Heightmap) -> Result<TerrainOptions, SceneError> {
        let (cell_cols, cell_rows) = (heightmap.cell_cols(), heightmap.cell_rows());
        let n = self.chunk_cells as usize;
        if n < 4 || n % 4 != 0 {
            return Err(SceneError::OutOfRange(format!(
                "terrain.options.chunk_cells {} 须 ≥ 4 且被最大 LOD 步长 4 整除",
                self.chunk_cells
            )));
        }
        if cell_cols % n != 0 || cell_rows % n != 0 {
            return Err(SceneError::OutOfRange(format!(
                "heightmap cell 数 ({cell_cols}, {cell_rows}) 须被 chunk_cells {} 整除",
                self.chunk_cells
            )));
        }
        let st = self.collider_stride as usize;
        if !(1..=4).contains(&st) {
            return Err(SceneError::OutOfRange(format!(
                "terrain.options.collider_stride {} 须在 1..=4",
                self.collider_stride
            )));
        }
        if cell_cols % st != 0 || cell_rows % st != 0 {
            return Err(SceneError::OutOfRange(format!(
                "heightmap cell 数 ({cell_cols}, {cell_rows}) 须被 collider_stride {} 整除",
                self.collider_stride
            )));
        }
        let [s0, s1] = self.switch_distances;
        if !(s0.is_finite() && s1.is_finite() && s0 > 0.0 && s1 > s0) {
            return Err(SceneError::OutOfRange(format!(
                "terrain.options.switch_distances {:?} 须为正的有限数且严格递增",
                self.switch_distances
            )));
        }
        if !(self.uv_tile.is_finite() && self.uv_tile > 0.0) {
            return Err(SceneError::OutOfRange(format!(
                "terrain.options.uv_tile 须为正的有限数，实际 {}",
                self.uv_tile
            )));
        }
        if heightmap
            .heights
            .iter()
            .any(|h| !h.is_finite() || *h < -1.0e8 || *h > 1.0e8)
        {
            return Err(SceneError::OutOfRange(
                "terrain.heightmap 含 NaN / ±Inf / 病态量级高度（|h| > 1e8）".into(),
            ));
        }
        Ok(TerrainOptions {
            origin: self.origin.into(),
            chunk_cells: n,
            collider_stride: st,
            uv_tile: self.uv_tile,
            switch_distances: self.switch_distances,
        })
    }
}

/// terrain 子段应用：构建 heightmap → 材质 → [`build_terrain`] → 派生
/// 实体打 [`LevelGenerated`] 标记。
pub(crate) fn apply_terrain(
    world: &mut World,
    sub: &SubAssets<'_>,
    section: &TerrainSection,
    cache: &mut LoadCache,
) -> Result<(), SceneError> {
    let heightmap = section.heightmap.to_heightmap()?;
    let options = section.options.to_options(&heightmap)?;
    let material = match &section.material {
        Some(material) => resolve_material(world, sub, material, cache)?,
        None => MaterialHandle(0),
    };
    let build = build_terrain(world, heightmap, material, &options);
    for entity in std::iter::once(build.root)
        .chain(build.chunks.iter().copied())
        .chain(build.colliders.iter().copied())
    {
        world.insert(entity, LevelGenerated);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// nav 子段
// ---------------------------------------------------------------------------

fn default_nav_cell_size() -> f32 {
    0.5
}
fn default_nav_agent_radius() -> f32 {
    0.4
}
fn default_nav_step_height() -> f32 {
    0.3
}
fn default_nav_max_slope() -> f32 {
    std::f32::consts::FRAC_PI_4
}

/// nav 子段：[`NavGridParams`] 全字段。bounds 必需（缺席 →
/// [`SceneError::Missing`]）；其余缺省与 [`NavGridParams::new`] 同值
/// （cell 0.5 / 膨胀 0.4 / 台阶 0.3 / 坡度 45°）。
/// **只存参数不存烘焙格子**——加载后由静态碰撞体重建（模块文档）。
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct NavSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds_min: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds_max: Option<[f32; 3]>,
    #[serde(default = "default_nav_cell_size")]
    pub cell_size: f32,
    #[serde(default = "default_nav_agent_radius")]
    pub agent_radius: f32,
    #[serde(default = "default_nav_step_height")]
    pub step_height: f32,
    #[serde(default = "default_nav_max_slope")]
    pub max_slope_radians: f32,
}

impl NavSection {
    /// 转引擎参数（校验 [`NavGrid::build`] 的全部断言前提——那些断言是
    /// panic 语义，加载路径按 [`SceneError`] 口径前置拦截）。
    pub fn to_params(self) -> Result<NavGridParams, SceneError> {
        let bounds_min = self
            .bounds_min
            .ok_or_else(|| SceneError::Missing("level.nav.bounds_min".into()))?;
        let bounds_max = self
            .bounds_max
            .ok_or_else(|| SceneError::Missing("level.nav.bounds_max".into()))?;
        if !(self.cell_size.is_finite() && self.cell_size > 0.0) {
            return Err(SceneError::OutOfRange(format!(
                "level.nav.cell_size 必须为正有限数，实际 {}",
                self.cell_size
            )));
        }
        if !(self.agent_radius.is_finite() && self.agent_radius >= 0.0) {
            return Err(SceneError::OutOfRange(format!(
                "level.nav.agent_radius 必须为非负有限数，实际 {}",
                self.agent_radius
            )));
        }
        if !(self.step_height.is_finite() && self.step_height >= 0.0) {
            return Err(SceneError::OutOfRange(format!(
                "level.nav.step_height 必须为非负有限数，实际 {}",
                self.step_height
            )));
        }
        if !(self.max_slope_radians.is_finite()
            && (0.0..=std::f32::consts::FRAC_PI_2).contains(&self.max_slope_radians))
        {
            return Err(SceneError::OutOfRange(format!(
                "level.nav.max_slope_radians 必须有限且 ∈ [0, π/2]，实际 {}",
                self.max_slope_radians
            )));
        }
        if !(bounds_min[0] < bounds_max[0] && bounds_min[2] < bounds_max[2]) {
            return Err(SceneError::OutOfRange(format!(
                "level.nav bounds 在 XZ 上须 min < max，实际 min {bounds_min:?} max {bounds_max:?}"
            )));
        }
        let params = NavGridParams {
            bounds_min: bounds_min.into(),
            bounds_max: bounds_max.into(),
            cell_size: self.cell_size,
            agent_radius: self.agent_radius,
            step_height: self.step_height,
            max_slope_radians: self.max_slope_radians,
        };
        // 网格规模上限（cell 序号 u32 可索引）——build 的最后一个断言
        let cols = ((params.bounds_max.x - params.bounds_min.x) / params.cell_size)
            .ceil()
            .max(1.0) as usize;
        let rows = ((params.bounds_max.z - params.bounds_min.z) / params.cell_size)
            .ceil()
            .max(1.0) as usize;
        if cols * rows > u32::MAX as usize {
            return Err(SceneError::OutOfRange(format!(
                "level.nav 网格 {cols}×{rows} 过大：cell 序号需 u32 可索引"
            )));
        }
        Ok(params)
    }
}

/// nav 子段应用：世界静态碰撞体收集 → [`NavGrid::build`] → World 资源。
///
/// 静态判定与物理缺席语义同源：无 `RigidBody` 组件 = 静态体；有则看
/// `Collider::is_static`。传感器（`is_sensor`）排除——它们不参与物理
/// 阻挡，也不该阻挡导航。收集序 = 组件存储序（同档两次加载逐位一致）。
pub(crate) fn apply_nav(world: &mut World, section: &NavSection) -> Result<(), SceneError> {
    let params = section.to_params()?;
    let mut statics: Vec<(Vec3, ColliderShape)> = Vec::new();
    world.query::<(&Transform, &Collider, Option<&RigidBody>)>(|(transform, collider, body)| {
        if collider.is_sensor {
            return;
        }
        if body.is_none() || collider.is_static {
            statics.push((transform.position, collider.shape));
        }
    });
    world.insert_resource(NavGrid::build(params, &statics));
    Ok(())
}

// ---------------------------------------------------------------------------
// bake 子段
// ---------------------------------------------------------------------------

fn default_bake_light_dir() -> [f32; 3] {
    BakeParams::default().light_dir.to_array()
}
fn default_bake_light_color() -> [f32; 3] {
    BakeParams::default().light_color.to_array()
}
fn default_bake_sky_color() -> [f32; 3] {
    BakeParams::default().sky_color.to_array()
}
fn default_bake_sky_samples() -> u32 {
    BakeParams::default().sky_samples
}
fn default_bake_bounces() -> u32 {
    BakeParams::default().bounces
}
fn default_texels_per_meter() -> f32 {
    BakeUvParams::default().texels_per_meter
}
fn default_atlas_size() -> u32 {
    BakeUvParams::default().atlas_size
}
fn default_dilate_iterations() -> u32 {
    LIGHTMAP_DILATE_ITERATIONS as u32
}

/// bake 子段：光照贴图烘焙参数（[`BakeParams`] + [`BakeUvParams`] 全
/// 字段，缺省 = 两者的 `default`）+ 记录性 `dilate_iterations` + 产物
/// 文件引用 `artifact`（缺席 = 只存参数，加载不挂产物）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct BakeSection {
    #[serde(default = "default_bake_light_dir")]
    pub light_dir: [f32; 3],
    #[serde(default = "default_bake_light_color")]
    pub light_color: [f32; 3],
    #[serde(default = "default_bake_sky_color")]
    pub sky_color: [f32; 3],
    #[serde(default)]
    pub shadow_softness: f32,
    #[serde(default = "default_bake_sky_samples")]
    pub sky_samples: u32,
    #[serde(default = "default_bake_bounces")]
    pub bounces: u32,
    #[serde(default = "default_texels_per_meter")]
    pub texels_per_meter: f32,
    #[serde(default = "default_atlas_size")]
    pub atlas_size: u32,
    #[serde(default = "default_dilate_iterations")]
    pub dilate_iterations: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
}

impl Default for BakeSection {
    fn default() -> Self {
        Self {
            light_dir: default_bake_light_dir(),
            light_color: default_bake_light_color(),
            sky_color: default_bake_sky_color(),
            shadow_softness: 0.0,
            sky_samples: default_bake_sky_samples(),
            bounces: default_bake_bounces(),
            texels_per_meter: default_texels_per_meter(),
            atlas_size: default_atlas_size(),
            dilate_iterations: default_dilate_iterations(),
            artifact: None,
        }
    }
}

impl BakeSection {
    /// 转引擎烘焙参数（`BakeParams::validate` / `BakeUvParams::validate`
    /// 是私有 panic 口径，此处按同口径前置拦截为 [`SceneError`]）。
    pub fn to_bake_params(&self) -> Result<(BakeParams, BakeUvParams), SceneError> {
        let finite3 = |v: &[f32; 3]| v.iter().all(|f| f.is_finite());
        let dir = Vec3::from_array(self.light_dir);
        if !finite3(&self.light_dir) || dir.length_squared() <= 0.0 {
            return Err(SceneError::OutOfRange(format!(
                "level.bake.light_dir 须为有限的非零向量，实际 {:?}",
                self.light_dir
            )));
        }
        if !finite3(&self.light_color) || !finite3(&self.sky_color) {
            return Err(SceneError::OutOfRange(
                "level.bake 的 light_color / sky_color 须为有限值".into(),
            ));
        }
        if !(self.shadow_softness.is_finite()
            && self.shadow_softness >= 0.0
            && self.shadow_softness < std::f32::consts::FRAC_PI_2)
        {
            return Err(SceneError::OutOfRange(format!(
                "level.bake.shadow_softness 须在 [0, π/2)，实际 {}",
                self.shadow_softness
            )));
        }
        if self.sky_samples < 1 {
            return Err(SceneError::OutOfRange(
                "level.bake.sky_samples 至少 1（半球至少 1 个样本）".into(),
            ));
        }
        if self.bounces > 1 {
            return Err(SceneError::OutOfRange(format!(
                "level.bake.bounces v1 支持 0 / 1，实际 {}",
                self.bounces
            )));
        }
        if !(self.texels_per_meter.is_finite() && self.texels_per_meter > 0.0) {
            return Err(SceneError::OutOfRange(format!(
                "level.bake.texels_per_meter 须为正有限值，实际 {}",
                self.texels_per_meter
            )));
        }
        if self.atlas_size < 1 {
            return Err(SceneError::OutOfRange(format!(
                "level.bake.atlas_size 须 ≥ 1，实际 {}",
                self.atlas_size
            )));
        }
        Ok((
            BakeParams {
                light_dir: dir,
                light_color: self.light_color.into(),
                sky_color: self.sky_color.into(),
                shadow_softness: self.shadow_softness,
                sky_samples: self.sky_samples,
                bounces: self.bounces,
            },
            BakeUvParams {
                texels_per_meter: self.texels_per_meter,
                atlas_size: self.atlas_size,
            },
        ))
    }
}

/// bake 子段应用：参数校验恒做（段的声明意图即「本关卡按此参数烘焙」）；
/// `artifact` 缺席 = 到此为止。产物**缺失 → 告警降级**（无光照贴图，
/// 加载继续）；**存在但损坏 / 与关卡几何失配 → 报错**（模块文档）。
pub(crate) fn apply_bake(
    world: &mut World,
    sub: &SubAssets<'_>,
    section: &BakeSection,
) -> Result<(), SceneError> {
    // 参数校验：陈旧 / 手错的参数在加载期即暴露，不等重烘时 panic
    section.to_bake_params()?;
    if section.dilate_iterations != LIGHTMAP_DILATE_ITERATIONS as u32 {
        log::warn!(
            "level.bake.dilate_iterations = {} 与引擎常量 {} 不符\
             （记录性字段：dilation 实际按引擎常量执行）",
            section.dilate_iterations,
            LIGHTMAP_DILATE_ITERATIONS
        );
    }
    let Some(rel) = &section.artifact else {
        return Ok(());
    };
    let bytes = match sub.read(rel) {
        Ok(bytes) => bytes,
        Err(SceneError::Io(ref e)) if e.kind() == std::io::ErrorKind::NotFound => {
            log::warn!(
                "光照贴图产物「{rel}」不存在——降级为无光照贴图\
                 （level.bake 参数仍在，可据此重烘：bake_lightmap + \
                 save_lightmap_artifact）"
            );
            return Ok(());
        }
        Err(SceneError::Asset(_)) => {
            log::warn!(
                "光照贴图产物「{rel}」经挂载未命中——降级为无光照贴图\
                 （level.bake 参数仍在，可据此重烘）"
            );
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    let baked = parse_lightmap_artifact(&bytes)?;
    attach_lightmap_artifact(world, baked)
}

// ---------------------------------------------------------------------------
// ddgi 子段（R89）
// ---------------------------------------------------------------------------

fn default_ddgi_spacing() -> f32 {
    DdgiSettings::default().spacing
}
fn default_ddgi_probes_per_frame() -> usize {
    DdgiSettings::default().probes_per_frame
}
fn default_ddgi_rays_per_face() -> u32 {
    DdgiSettings::default().rays_per_face
}
fn default_ddgi_hysteresis() -> f32 {
    DdgiSettings::default().hysteresis
}
fn default_ddgi_bounces() -> u32 {
    DdgiSettings::default().bounces
}

/// ddgi 子段：DDGI 探针网格的**作者参数**（[`DdgiSettings`] 的序列化投影；
/// `geometry_generation` 不入档——它是运行期记账，不是作者意图）。逐字段
/// `#[serde(default = "…")]` 缺省 = 引擎默认；`bounds` 两项都缺席 =
/// `None`（从静态几何自动派生，与引擎同语义）。**探针表内容（SH / 深度
/// 矩 / 脏位 / cursor）不入档**（§8.57⑭ 维持：重算成本可控，入档引入
/// 「表与几何失配」的校验面）。
///
/// 加载：[`apply_ddgi`] 段 → `DdgiSettings` 资源（校验失败 = 加载报错，
/// 与 nav/bake 同口径）。保存：`save_scene` 从**活动资源**回写（资源缺席
/// ⇒ 不写段 ⇒ DDGI 关）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct DdgiSection {
    #[serde(default = "default_ddgi_spacing")]
    pub spacing: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds_min: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds_max: Option<[f32; 3]>,
    #[serde(default = "default_ddgi_probes_per_frame")]
    pub probes_per_frame: usize,
    #[serde(default = "default_ddgi_rays_per_face")]
    pub rays_per_face: u32,
    #[serde(default = "default_ddgi_hysteresis")]
    pub hysteresis: f32,
    /// 天空辐射率覆盖（缺席 = 取 `DirectionalLight.ambient`，裁定 3）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sky_color: Option<[f32; 3]>,
    /// 天空同源开关（C7a；缺席 = `false` = 常量天空底，逐位现口径）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sky_from_source: Option<bool>,
    /// 点光/聚光进 GI 总开关（C7a；缺席 = `false` = 灯不进 GI）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lights_in_gi: Option<bool>,
    /// 点光/聚光进 GI 的逐灯硬影开关（C7a；缺席 = `true` = 硬影单射线）。
    /// 仅当场景有动态灯进 GI 时被消费。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point_light_shadows: Option<bool>,
    #[serde(default = "default_ddgi_bounces")]
    pub bounces: u32,
}

impl Default for DdgiSection {
    fn default() -> Self {
        Self {
            spacing: default_ddgi_spacing(),
            bounds_min: None,
            bounds_max: None,
            probes_per_frame: default_ddgi_probes_per_frame(),
            rays_per_face: default_ddgi_rays_per_face(),
            hysteresis: default_ddgi_hysteresis(),
            sky_color: None,
            sky_from_source: None,
            lights_in_gi: None,
            point_light_shadows: None,
            bounces: default_ddgi_bounces(),
        }
    }
}

impl DdgiSection {
    /// 转引擎设置（校验 `DdgiSettings::validate` 的全部断言前提——那些
    /// 断言是 panic 语义，加载路径按 [`SceneError`] 口径前置拦截）。
    pub fn to_settings(&self) -> Result<DdgiSettings, SceneError> {
        let bounds = match (self.bounds_min, self.bounds_max) {
            (None, None) => None,
            (Some(min), Some(max)) => {
                if !min.iter().all(|v| v.is_finite()) || !max.iter().all(|v| v.is_finite()) {
                    return Err(SceneError::OutOfRange(format!(
                        "level.ddgi bounds 须有限：{min:?} .. {max:?}"
                    )));
                }
                if !(max[0] >= min[0] && max[1] >= min[1] && max[2] >= min[2]) {
                    return Err(SceneError::OutOfRange(format!(
                        "level.ddgi bounds 的 max 须逐分量 ≥ min：{min:?} .. {max:?}"
                    )));
                }
                Some((Vec3::from_array(min), Vec3::from_array(max)))
            }
            (min, max) => {
                return Err(SceneError::Missing(format!(
                    "level.ddgi.bounds_min / bounds_max 必须成对出现（min = {min:?}，max = {max:?}）"
                )));
            }
        };
        let settings = DdgiSettings {
            spacing: self.spacing,
            bounds,
            probes_per_frame: self.probes_per_frame,
            rays_per_face: self.rays_per_face,
            hysteresis: self.hysteresis,
            sky_color: self.sky_color.map(Vec3::from_array),
            sky_from_source: self.sky_from_source.unwrap_or(false),
            lights_in_gi: self.lights_in_gi.unwrap_or(false),
            point_light_shadows: self.point_light_shadows.unwrap_or(true),
            bounces: self.bounces,
            geometry_generation: 0,
        };
        // 与引擎校验同口径的逐条前置拦截（`DdgiSettings::validate` 是 panic
        // 语义；加载路径按 SceneError 报告——照 nav/bake 的处置）
        if !(settings.spacing.is_finite() && settings.spacing > 0.0) {
            return Err(SceneError::OutOfRange(format!(
                "level.ddgi.spacing 必须为正有限值，实际 {}",
                settings.spacing
            )));
        }
        if !DDGI_RAYS_PER_FACE_CHOICES.contains(&settings.rays_per_face) {
            return Err(SceneError::OutOfRange(format!(
                "level.ddgi.rays_per_face 只能取 {DDGI_RAYS_PER_FACE_CHOICES:?}，实际 {}",
                settings.rays_per_face
            )));
        }
        if !settings.hysteresis.is_finite() {
            return Err(SceneError::OutOfRange(format!(
                "level.ddgi.hysteresis 须有限，实际 {}",
                settings.hysteresis
            )));
        }
        if settings.bounces > 1 {
            return Err(SceneError::OutOfRange(format!(
                "level.ddgi.bounces v1 支持 0 / 1，实际 {}",
                settings.bounces
            )));
        }
        if let Some(sky) = settings.sky_color {
            if !sky.is_finite() {
                return Err(SceneError::OutOfRange(format!(
                    "level.ddgi.sky_color 须有限：{sky:?}"
                )));
            }
        }
        // 理论不可达的兜底（上面穷尽了 validate 的全部断言前提）
        settings.validate();
        Ok(settings)
    }

    /// 从活动资源投影（`save_scene` 的回写侧）。
    pub fn from_settings(settings: &DdgiSettings) -> Self {
        Self {
            spacing: settings.spacing,
            bounds_min: settings.bounds.map(|(min, _)| min.to_array()),
            bounds_max: settings.bounds.map(|(_, max)| max.to_array()),
            probes_per_frame: settings.probes_per_frame,
            rays_per_face: settings.rays_per_face,
            hysteresis: settings.hysteresis,
            sky_color: settings.sky_color.map(|v| v.to_array()),
            // C7a 两旋钮只回写**非默认值**（默认档不落字段，旧档形态不变）
            sky_from_source: settings.sky_from_source.then_some(true),
            lights_in_gi: settings.lights_in_gi.then_some(true),
            point_light_shadows: (!settings.point_light_shadows).then_some(false),
            bounces: settings.bounces,
        }
    }
}

/// ddgi 子段应用：段 → `DdgiSettings` 资源（缺席段 = 不插资源 = DDGI 关）。
pub(crate) fn apply_ddgi(world: &mut World, section: &DdgiSection) -> Result<(), SceneError> {
    let settings = section.to_settings()?;
    world.insert_resource(settings);
    Ok(())
}

/// 产物挂载：一致性校验 → [`install_lightmap`] → 登记表回补。
/// 校验项见模块文档；任一不符 = [`SceneError::Unsupported`]（校验穷尽了
/// `install_lightmap` 的全部 panic 前提，安装本身不再失败）。
fn attach_lightmap_artifact(world: &mut World, baked: BakedLightmap) -> Result<(), SceneError> {
    // ---- 校验（BTreeMap：迭代序 = 实体位升序，错误消息按稳定序报第一处
    // 失配——确定性选型理由见 AGENTS.md）----
    let mut per_entity: BTreeMap<u64, (Option<MeshHandle>, Vec<u64>)> = BTreeMap::new();
    for entry in &baked.atlas.entries {
        let Some(mesh) = entry.mesh else {
            return Err(SceneError::Unsupported(
                "光照贴图产物的图集表项缺 mesh 句柄——非 bake_lightmap 组装产物".into(),
            ));
        };
        let slot = per_entity.entry(entry.entity.to_bits()).or_default();
        match slot.0 {
            None => slot.0 = Some(mesh),
            Some(known) if known != mesh => {
                return Err(SceneError::Unsupported(format!(
                    "产物中实体 {:#x} 的图集表项来自不同网格（{known:?} 与 {mesh:?}）",
                    entry.entity.to_bits()
                )));
            }
            Some(_) => {}
        }
        for tri in &entry.triangles {
            slot.1.push(tri.triangle as u64);
        }
    }
    {
        let meshes = world.try_resource::<MeshStore>().ok_or_else(|| {
            SceneError::Unsupported("世界缺 MeshStore 资源——无法挂载光照贴图产物".into())
        })?;
        for (&bits, (mesh, globals)) in &per_entity {
            let mesh = mesh.expect("上方循环已保证 Some");
            let entity = Entity::from_bits(bits);
            if !world.contains(entity) {
                return Err(stale_artifact(format!(
                    "实体 {bits:#x} 不在当前世界（关卡几何与烘焙失配）"
                )));
            }
            let current = world.get::<MeshHandle>(entity).ok_or_else(|| {
                stale_artifact(format!(
                    "实体 {bits:#x} 无 MeshHandle（关卡几何与烘焙失配）"
                ))
            })?;
            if *current != mesh {
                return Err(stale_artifact(format!(
                    "实体 {bits:#x} 的网格句柄 {:?} 与产物表项 {mesh:?} 不符",
                    *current
                )));
            }
            let data = meshes.get(mesh).ok_or_else(|| {
                stale_artifact(format!("网格句柄 {mesh:?} 不在 MeshStore（产物悬垂引用）"))
            })?;
            if data.indices.len() / 3 != globals.len() {
                return Err(stale_artifact(format!(
                    "实体 {bits:#x} 的产物三角数 {} 与网格三角数 {} 不符",
                    globals.len(),
                    data.indices.len() / 3
                )));
            }
            // 全局下标逐实体连续升序（install 的「秩即网格内序号」推导前提）
            let mut sorted = globals.clone();
            sorted.sort_unstable();
            for pair in sorted.windows(2) {
                if pair[1] != pair[0] + 1 {
                    return Err(SceneError::Unsupported(format!(
                        "产物中实体 {bits:#x} 的图集三角下标不连续（{} 之后是 {}）——文件损坏",
                        pair[0], pair[1]
                    )));
                }
            }
        }
    }

    // 原句柄快照（登记表回补用；install 会替换实体的 MeshHandle）
    let originals: Vec<(Entity, MeshHandle)> = per_entity
        .iter()
        .map(|(&bits, (mesh, _))| (Entity::from_bits(bits), mesh.expect("上方循环已保证 Some")))
        .collect();

    // ---- 安装（校验已排除其全部 panic 前提）----
    install_lightmap(world, baked);

    // ---- 登记表回补：UV2 副本句柄继承原句柄的场景来源（save 还原用）。
    // 两阶段（先读组件再写资源）——AGENTS.md 的尝试型锁规约。----
    let new_handles: Vec<(MeshHandle, MeshHandle)> = originals
        .iter()
        .filter_map(|&(entity, original)| {
            world
                .get::<MeshHandle>(entity)
                .map(|current| (original, *current))
        })
        .collect();
    let mut registry = world.resource_mut::<SceneAssetRegistry>();
    for (original, new_handle) in new_handles {
        if new_handle == original {
            continue;
        }
        if let Some(source) = registry.meshes.get(&original.0) {
            let source = source.clone();
            registry.meshes.insert(new_handle.0, source);
        }
    }
    Ok(())
}

/// 陈旧 / 失配产物的统一错误构造（消息含重烘指引）。
fn stale_artifact(what: String) -> SceneError {
    SceneError::Unsupported(format!(
        "光照贴图产物与关卡几何失配：{what}——请重烘（bake_lightmap + \
         save_lightmap_artifact）"
    ))
}

// ---------------------------------------------------------------------------
// 光照贴图产物文件（IRISLMAP v1）
// ---------------------------------------------------------------------------

/// 产物文件魔数。
pub const LIGHTMAP_ARTIFACT_MAGIC: &[u8; 8] = b"IRISLMAP";
/// 产物文件格式版本（独立于关卡档版本——数据布局自己的演进线）。
pub const LIGHTMAP_ARTIFACT_VERSION: u32 = 1;

/// 把烘焙产物写为外置文件（关卡档 `level.bake.artifact` 引用的对象）。
pub fn save_lightmap_artifact(
    baked: &BakedLightmap,
    path: impl AsRef<Path>,
) -> Result<(), SceneError> {
    std::fs::write(path.as_ref(), encode_lightmap_artifact(baked))?;
    Ok(())
}

/// 从外置文件读取烘焙产物。
pub fn load_lightmap_artifact(path: impl AsRef<Path>) -> Result<BakedLightmap, SceneError> {
    let bytes = std::fs::read(path.as_ref())?;
    parse_lightmap_artifact(&bytes)
}

/// 编码为 IRISLMAP v1 字节（布局图见模块文档）。纯函数：同输入两次
/// 编码逐位一致（无时间戳 / 迭代序依赖——表项序 = 图集装箱序原样）。
pub fn encode_lightmap_artifact(baked: &BakedLightmap) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(LIGHTMAP_ARTIFACT_MAGIC);
    out.extend_from_slice(&LIGHTMAP_ARTIFACT_VERSION.to_le_bytes());
    out.extend_from_slice(&baked.atlas.size.to_le_bytes());
    out.extend_from_slice(&baked.atlas.used_height.to_le_bytes());
    out.extend_from_slice(&(baked.atlas.entries.len() as u32).to_le_bytes());
    for entry in &baked.atlas.entries {
        out.extend_from_slice(&entry.entity.to_bits().to_le_bytes());
        match entry.mesh {
            Some(handle) => {
                out.push(1);
                out.extend_from_slice(&handle.0.to_le_bytes());
            }
            None => {
                out.push(0);
                out.extend_from_slice(&0u32.to_le_bytes());
            }
        }
        // ChartAxis 声明序即数值序（bake_uv.rs「勿重排」注释的格式侧对应）
        let axis = match entry.axis {
            ChartAxis::Floor => 0u8,
            ChartAxis::WallX => 1,
            ChartAxis::WallZ => 2,
        };
        out.push(axis);
        for f in entry.uv_rect {
            out.extend_from_slice(&f.to_le_bytes());
        }
        for v in entry.origin {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in entry.size {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&entry.texels_per_meter.to_le_bytes());
        out.extend_from_slice(&(entry.triangles.len() as u32).to_le_bytes());
        for tri in &entry.triangles {
            out.extend_from_slice(&(tri.triangle as u64).to_le_bytes());
            for f in tri.uv_rect {
                out.extend_from_slice(&f.to_le_bytes());
            }
            for corner in tri.local {
                for f in corner {
                    out.extend_from_slice(&f.to_le_bytes());
                }
            }
        }
    }
    let texels = (baked.atlas.size as u64) * (baked.atlas.size as u64);
    out.extend_from_slice(&texels.to_le_bytes());
    for array in [&baked.radiance, &baked.indirect] {
        for texel in array {
            for channel in [texel.x, texel.y, texel.z] {
                out.extend_from_slice(&channel.to_le_bytes());
            }
        }
    }
    for covered in &baked.covered {
        out.push(u8::from(*covered));
    }
    out
}

/// 解码游标（越界 / 截断统一报错，不 panic）。
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], SceneError> {
        let end = self.pos.checked_add(n).ok_or_else(truncated)?;
        let slice = self.bytes.get(self.pos..end).ok_or_else(truncated)?;
        self.pos = end;
        Ok(slice)
    }
    fn u8(&mut self) -> Result<u8, SceneError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, SceneError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, SceneError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32, SceneError> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
}

fn truncated() -> SceneError {
    SceneError::Unsupported("光照贴图产物文件截断（长度与布局表不符）".into())
}

/// 解析 IRISLMAP v1 字节。魔数 / 版本 / 长度逐项校验（错误消息中文，
/// 走 [`SceneError::Unsupported`]）。容量预分配设上限（损坏的计数字段
/// 不应触发巨额分配——超限即报截断 / 损坏）。
pub fn parse_lightmap_artifact(bytes: &[u8]) -> Result<BakedLightmap, SceneError> {
    let mut cur = Cursor { bytes, pos: 0 };
    if cur.take(8)? != LIGHTMAP_ARTIFACT_MAGIC {
        return Err(SceneError::Unsupported(
            "光照贴图产物魔数不符（期望 IRISLMAP）".into(),
        ));
    }
    let version = cur.u32()?;
    if version != LIGHTMAP_ARTIFACT_VERSION {
        return Err(SceneError::Unsupported(format!(
            "光照贴图产物版本 = {version}，本引擎支持 {}",
            LIGHTMAP_ARTIFACT_VERSION
        )));
    }
    let size = cur.u32()?;
    let used_height = cur.u32()?;
    let entry_count = cur.u32()?;
    let mut entries = Vec::new();
    for _ in 0..entry_count {
        let entity = Entity::from_bits(cur.u64()?);
        let has_mesh = cur.u8()?;
        let mesh_raw = cur.u32()?;
        let mesh = if has_mesh != 0 {
            Some(MeshHandle(mesh_raw))
        } else {
            None
        };
        let axis = match cur.u8()? {
            0 => ChartAxis::Floor,
            1 => ChartAxis::WallX,
            2 => ChartAxis::WallZ,
            other => {
                return Err(SceneError::Unsupported(format!(
                    "光照贴图产物的 chart 轴序号 {other} 非法（0..=2）"
                )));
            }
        };
        let mut uv_rect = [0.0f32; 4];
        for slot in &mut uv_rect {
            *slot = cur.f32()?;
        }
        let origin = [cur.u32()?, cur.u32()?];
        let entry_size = [cur.u32()?, cur.u32()?];
        let texels_per_meter = cur.f32()?;
        let tri_count = cur.u32()?;
        let mut triangles = Vec::new();
        for _ in 0..tri_count {
            let triangle = cur.u64()? as usize;
            let mut tri_rect = [0.0f32; 4];
            for slot in &mut tri_rect {
                *slot = cur.f32()?;
            }
            let mut local = [[0.0f32; 2]; 3];
            for corner in &mut local {
                for slot in corner {
                    *slot = cur.f32()?;
                }
            }
            triangles.push(AtlasTriangle {
                triangle,
                uv_rect: tri_rect,
                local,
            });
        }
        entries.push(AtlasEntry {
            entity,
            mesh,
            axis,
            uv_rect,
            origin,
            size: entry_size,
            texels_per_meter,
            triangles,
        });
    }
    let texels = cur.u64()?;
    if texels != (size as u64) * (size as u64) {
        return Err(SceneError::Unsupported(format!(
            "光照贴图产物 texel 数 {texels} 与图集边长 {size} 不符（须为 size²）"
        )));
    }
    // 数据区长度先整体核对（radiance + indirect 各 12 B/texel + covered 1 B/texel）
    let expected_tail = texels.checked_mul(25).ok_or_else(truncated)?;
    if (bytes.len() - cur.pos) as u64 != expected_tail {
        return Err(truncated());
    }
    let read_array = |cur: &mut Cursor<'_>| -> Result<Vec<Vec3>, SceneError> {
        let mut out = Vec::with_capacity(texels.min(1 << 26) as usize);
        for _ in 0..texels {
            out.push(Vec3::new(cur.f32()?, cur.f32()?, cur.f32()?));
        }
        Ok(out)
    };
    let radiance = read_array(&mut cur)?;
    let indirect = read_array(&mut cur)?;
    let mut covered = Vec::with_capacity(texels.min(1 << 26) as usize);
    for _ in 0..texels {
        covered.push(cur.u8()? != 0);
    }
    Ok(BakedLightmap {
        atlas: LightmapAtlas {
            size,
            used_height,
            entries,
        },
        radiance,
        indirect,
        covered,
    })
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{load_scene, save_scene};
    use iris_render::prelude::{
        LightmapBinding, LightmapStore, MaterialStore, Terrain, TextureHandle, TextureStore,
        bake_lightmap, grass_textures, write_lightmap_preview,
    };
    use std::path::PathBuf;

    /// 测试固定目录（进程 id 唯一名防并行竞态——AGENTS.md 测试规约）。
    fn test_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iris-level-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        dir
    }

    /// 写关卡文件并加载。
    fn load_str(world: &mut World, name: &str, json: &str) -> Vec<Entity> {
        let path = test_dir().join(name);
        std::fs::write(&path, json).expect("写关卡文件");
        load_scene(world, &path).expect("加载关卡")
    }

    /// NavGridParams 位模式摘要（无 PartialEq，逐字段取位——确定性测试
    /// 模式见 AGENTS.md）。
    fn params_bits(params: &NavGridParams) -> ([u32; 3], [u32; 3], u32, u32, u32, u32) {
        (
            params.bounds_min.to_array().map(f32::to_bits),
            params.bounds_max.to_array().map(f32::to_bits),
            params.cell_size.to_bits(),
            params.agent_radius.to_bits(),
            params.step_height.to_bits(),
            params.max_slope_radians.to_bits(),
        )
    }

    // ---- terrain ----

    /// hills 生成器参数进档：Terrain 组件的高度场与直接调用
    /// generate_hills 逐位一致；派生实体全部带 LevelGenerated 标记；
    /// save 跳过派生实体、level 段原样回写；重载等价。
    #[test]
    fn terrain_hills_builds_and_roundtrips() {
        let json = r#"{
            "version": 2,
            "level": {
                "terrain": {
                    "heightmap": { "hills": { "width": 9, "depth": 9, "cell_size": 0.5,
                                              "seed": 7, "base": 0.0, "amplitude": 0.5 } },
                    "options": { "chunk_cells": 4, "collider_stride": 2 }
                }
            },
            "entities": [{ "name": "marker" }]
        }"#;
        let mut world = World::new();
        let entities = load_str(&mut world, "terrain_hills.level.json", json);
        assert_eq!(entities.len(), 1, "返回实体列表只含实体段");

        // Terrain 组件：高度场与生成器直调逐位一致
        let mut roots = Vec::new();
        world.query::<(Entity, &Terrain)>(|(entity, terrain)| {
            roots.push((
                entity,
                terrain.heightmap.clone(),
                terrain.chunks.len(),
                terrain.colliders.len(),
            ));
        });
        assert_eq!(roots.len(), 1, "terrain 根实体应恰一个");
        let (root, heightmap, chunks, colliders) = roots.pop().unwrap();
        let expected = generate_hills(9, 9, 0.5, 7, 0.0, 0.5);
        assert_eq!(heightmap, expected, "高度场应与生成器直调逐位一致");
        assert_eq!(chunks, 4, "8×8 cell ÷ chunk 4 = 2×2 chunk");
        assert_eq!(colliders, 16, "8×8 cell ÷ stride 2 = 4×4 拼块");
        assert_eq!(
            *world.get::<LevelGenerated>(root).unwrap(),
            LevelGenerated,
            "根实体应带派生标记"
        );

        // save：派生实体跳过（只写实体段的 1 个），level 段原样回写
        let out = test_dir().join("terrain_hills_out.json");
        let count = save_scene(&mut world, &out).expect("保存");
        assert_eq!(count, 1, "terrain 派生实体不进实体段");
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains(r#""version": 2"#), "save 恒写当前版本");
        assert!(text.contains(r#""hills""#), "terrain 段应回写");
        assert!(text.contains(r#""chunk_cells": 4"#));

        // 重载：世界等价（高度场与 chunk 数逐位一致）
        let mut world_b = World::new();
        let entities_b = load_scene(&mut world_b, &out).expect("重载");
        assert_eq!(entities_b.len(), 1);
        let mut heights_b = Vec::new();
        let mut chunks_b = 0usize;
        world_b.query::<&Terrain>(|terrain| {
            heights_b.push(terrain.heightmap.clone());
            chunks_b = terrain.chunks.len();
        });
        assert_eq!(heights_b[0], expected, "往返后高度场逐位一致");
        assert_eq!(chunks_b, 4);
    }

    /// inline 高度数组进档 + 校验：长度不符 / 尺寸过小 / chunk_cells
    /// 不整除 → OutOfRange（不 panic）。
    #[test]
    fn terrain_inline_heights_and_validation() {
        let json = r#"{
            "version": 2,
            "level": { "terrain": {
                "heightmap": { "inline": {
                    "width": 5, "depth": 5, "cell_size": 1.0,
                    "heights": [0.0, 0.0, 0.0, 0.0, 0.0,
                                0.0, 0.25, 0.5, 0.25, 0.0,
                                0.0, 0.5, 1.0, 0.5, 0.0,
                                0.0, 0.25, 0.5, 0.25, 0.0,
                                0.0, 0.0, 0.0, 0.0, 0.0] } },
                "options": { "chunk_cells": 4 } } },
            "entities": []
        }"#;
        let mut world = World::new();
        let _ = load_str(&mut world, "terrain_inline.level.json", json);
        let mut heights = Vec::new();
        world.query::<&Terrain>(|terrain| heights.extend_from_slice(&terrain.heightmap.heights));
        assert_eq!(heights.len(), 25);
        assert_eq!(heights[12], 1.0, "中央峰顶原样入档");
        assert_eq!(heights[6].to_bits(), 0.25f32.to_bits());

        // 长度不符
        let path = test_dir().join("terrain_bad_len.json");
        std::fs::write(
            &path,
            r#"{ "version": 2, "level": { "terrain": { "heightmap": { "inline": {
                "width": 2, "depth": 2, "cell_size": 1.0, "heights": [0.0, 1.0] } } } },
                "entities": [] }"#,
        )
        .unwrap();
        let err = load_scene(&mut World::new(), &path).unwrap_err();
        assert!(
            matches!(err, SceneError::OutOfRange(ref w) if w.contains("width × depth")),
            "{err}"
        );

        // 尺寸过小
        std::fs::write(
            &path,
            r#"{ "version": 2, "level": { "terrain": { "heightmap": { "inline": {
                "width": 1, "depth": 2, "cell_size": 1.0, "heights": [0.0, 1.0] } } } },
                "entities": [] }"#,
        )
        .unwrap();
        let err = load_scene(&mut World::new(), &path).unwrap_err();
        assert!(
            matches!(err, SceneError::OutOfRange(ref w) if w.contains("2×2")),
            "{err}"
        );

        // chunk_cells 不整除 cell 数
        std::fs::write(
            &path,
            r#"{ "version": 2, "level": { "terrain": {
                "heightmap": { "inline": { "width": 3, "depth": 3, "cell_size": 1.0,
                    "heights": [0.0,0.0,0.0, 0.0,0.5,0.0, 0.0,0.0,0.0] } },
                "options": { "chunk_cells": 4 } } }, "entities": [] }"#,
        )
        .unwrap();
        let err = load_scene(&mut World::new(), &path).unwrap_err();
        assert!(
            matches!(err, SceneError::OutOfRange(ref w) if w.contains("chunk_cells")),
            "{err}"
        );
    }

    /// terrain 材质段走程序化草地三源（基色 / 法线 / ORM 同一 grass 参数
    /// 只生成一次、逐槽登记）：材质三槽句柄齐全、线性槽 srgb = false、
    /// 像素与生成器直调逐位一致。
    #[test]
    fn terrain_material_uses_grass_procedural_sources() {
        let json = r#"{
            "version": 2,
            "level": { "terrain": {
                "heightmap": { "hills": { "width": 5, "depth": 5, "cell_size": 1.0, "seed": 3,
                                          "base": 0.0, "amplitude": 0.4 } },
                "material": {
                    "texture": { "grass": { "size": 16 } },
                    "normal_texture": { "grass": { "size": 16 } },
                    "orm_texture": { "grass": { "size": 16 } }
                },
                "options": { "chunk_cells": 4 }
            } },
            "entities": []
        }"#;
        let mut world = World::new();
        load_str(&mut world, "terrain_grass.level.json", json);
        // chunk 实体持材质句柄（Terrain 清单）
        let mut chunk = None;
        world.query::<&Terrain>(|terrain| chunk = terrain.chunks.first().copied());
        let chunk = chunk.expect("应有 chunk 实体");
        let material_handle = *world.get::<MaterialHandle>(chunk).unwrap();
        let def = world
            .resource::<MaterialStore>()
            .get(material_handle)
            .expect("草地材质应已登记")
            .clone();
        let normal = def.normal_texture.expect("法线槽应已解析");
        let orm = def.orm_texture.expect("ORM 槽应已解析");
        assert_ne!(def.texture, TextureHandle(0), "基色槽应非白兜底");
        assert_eq!(
            world
                .resource::<SceneAssetRegistry>()
                .materials
                .get(&material_handle.0)
                .and_then(|r| r.base_source),
            Some(crate::scene::ProceduralTextureSection::Grass { size: 16 }),
            "程序化基色源应入登记表（save 还原用）"
        );
        let textures = world.resource::<TextureStore>();
        // 槽位 0（白兜底）之外恰 3 张：三件套各登记一次（同参数不重复生成）
        assert_eq!(textures.len(), 4, "白兜底 + albedo + normal + orm");
        assert!(textures.get(def.texture).unwrap().srgb, "基色 sRGB");
        assert!(!textures.get(normal).unwrap().srgb, "法线线性");
        assert!(!textures.get(orm).unwrap().srgb, "ORM 线性");
        let grass = grass_textures(16);
        assert_eq!(textures.get(normal).unwrap().rgba, grass.normal.rgba);
        assert_eq!(textures.get(orm).unwrap().rgba, grass.orm.rgba);
        assert_eq!(textures.get(def.texture).unwrap().rgba, grass.albedo.rgba);
    }

    // ---- nav ----

    /// nav 段：参数进档 → NavGrid 资源由世界静态碰撞体重建（参数逐位
    /// 往返）。开阔域可达、路径绕柱；动态体不参与重建。
    #[test]
    fn nav_section_rebuilds_grid_from_static_colliders() {
        let json = r#"{
            "version": 2,
            "level": { "nav": { "bounds_min": [-5.0, 0.0, -5.0], "bounds_max": [5.0, 0.0, 5.0],
                                "cell_size": 0.5, "agent_radius": 0.4 } },
            "entities": [
                { "name": "post", "transform": { "position": [0.0, 1.0, 0.0] },
                  "collider": { "shape": { "box": { "half_extents": [0.5, 1.0, 0.5] } },
                                "static": true } }
            ]
        }"#;
        let mut world = World::new();
        load_str(&mut world, "nav_open.level.json", json);
        let grid = world
            .try_resource::<NavGrid>()
            .expect("nav 段应重建 NavGrid 资源");
        assert_eq!(grid.cols(), 20);
        assert_eq!(grid.rows(), 20);
        assert_eq!(
            params_bits(&grid.params()),
            params_bits(&NavGridParams {
                bounds_min: Vec3::new(-5.0, 0.0, -5.0),
                bounds_max: Vec3::new(5.0, 0.0, 5.0),
                cell_size: 0.5,
                agent_radius: 0.4,
                step_height: 0.3,
                max_slope_radians: std::f32::consts::FRAC_PI_4,
            }),
            "参数应逐位进 NavGrid"
        );
        assert!(
            grid.is_reachable(Vec3::new(-4.0, 0.0, -4.0), Vec3::new(4.0, 0.0, 4.0)),
            "开阔域（中央一柱）对角应可达"
        );
        let path = grid
            .find_path(Vec3::new(-4.0, 0.0, 0.0), Vec3::new(4.0, 0.0, 0.0))
            .expect("应有路径");
        assert!(
            path.iter().any(|p| p.z.abs() > 0.9),
            "路径应绕开中央柱（航点离开柱体膨胀域），实际 {path:?}"
        );
        drop(grid);

        // 动态体（有 RigidBody 且非 static）不参与重建
        let json_dynamic = r#"{
            "version": 2,
            "level": { "nav": { "bounds_min": [-2.0, 0.0, -2.0], "bounds_max": [2.0, 0.0, 2.0] } },
            "entities": [
                { "collider": { "shape": { "box": { "half_extents": [1.5, 1.0, 1.5] } } },
                  "rigidbody": {} }
            ]
        }"#;
        let mut world = World::new();
        load_str(&mut world, "nav_dynamic.level.json", json_dynamic);
        let grid = world.try_resource::<NavGrid>().unwrap();
        assert!(
            grid.is_reachable(Vec3::new(-1.5, 0.0, 0.0), Vec3::new(1.5, 0.0, 0.0)),
            "动态体不是导航静态障碍"
        );
    }

    /// nav 校验：bounds 缺席 → Missing；cell_size 非法 / bounds 反向 →
    /// OutOfRange（全部不 panic）。
    #[test]
    fn nav_validation_errors() {
        let path = test_dir().join("nav_bad.json");
        std::fs::write(
            &path,
            r#"{ "version": 2, "level": { "nav": { "cell_size": 0.5 } }, "entities": [] }"#,
        )
        .unwrap();
        let err = load_scene(&mut World::new(), &path).unwrap_err();
        assert!(
            matches!(err, SceneError::Missing(ref w) if w.contains("bounds_min")),
            "{err}"
        );

        std::fs::write(
            &path,
            r#"{ "version": 2, "level": { "nav": { "bounds_min": [-1,0,-1], "bounds_max": [1,0,1],
                "cell_size": 0.0 } }, "entities": [] }"#,
        )
        .unwrap();
        let err = load_scene(&mut World::new(), &path).unwrap_err();
        assert!(
            matches!(err, SceneError::OutOfRange(ref w) if w.contains("cell_size")),
            "{err}"
        );

        std::fs::write(
            &path,
            r#"{ "version": 2, "level": { "nav": { "bounds_min": [1,0,1], "bounds_max": [-1,0,-1] } },
                "entities": [] }"#,
        )
        .unwrap();
        let err = load_scene(&mut World::new(), &path).unwrap_err();
        assert!(
            matches!(err, SceneError::OutOfRange(ref w) if w.contains("min < max")),
            "{err}"
        );
    }

    // ---- bake ----

    /// 烘焙 → 产物文件 → 关卡档引用挂载的闭环：小场景（单立方）真实走
    /// bake_lightmap，产物存盘，新世界加载关卡档后 LightmapStore /
    /// LightmapBinding / UV2 副本网格全部就位；产物字节确定性（两次编码
    /// 逐位一致）；save → 重载闭环（mesh 段还原为图元、产物重挂、副本
    /// 句柄可复现）。
    #[test]
    fn bake_artifact_roundtrip_and_attach() {
        // ① 烘焙源世界：实体段单立方（静态碰撞体 = collect 口径内）
        let mut source = World::new();
        load_str(
            &mut source,
            "bake_source.level.json",
            r#"{ "version": 2, "entities": [
                { "name": "block", "mesh": { "primitive": { "kind": "cube" } },
                  "collider": { "shape": { "box": { "half_extents": [0.5, 0.5, 0.5] } },
                                "static": true } } ] }"#,
        );
        iris_core::hierarchy::propagate_transforms(&mut source);
        let section = BakeSection {
            sky_samples: 4,
            bounces: 0,
            texels_per_meter: 4.0,
            atlas_size: 32,
            ..BakeSection::default()
        };
        let (params, uv) = section.to_bake_params().expect("默认参数合法");
        let baked = bake_lightmap(&source, &params, &uv);
        assert!(!baked.atlas.entries.is_empty(), "立方应产出 chart");

        // ② 编码确定性 + 解析往返逐位一致
        let bytes_a = encode_lightmap_artifact(&baked);
        let bytes_b = encode_lightmap_artifact(&baked);
        assert_eq!(bytes_a, bytes_b, "同输入两次编码应逐位一致");
        let parsed = parse_lightmap_artifact(&bytes_a).expect("解析");
        assert_artifact_eq(&parsed, &baked);

        // ③ 产物存盘 + 关卡档引用 → 新世界挂载
        let artifact = test_dir().join("bake_cube.ilmap");
        save_lightmap_artifact(&baked, &artifact).expect("写产物");
        let level_with_bake = r#"{
            "version": 2,
            "level": { "bake": { "sky_samples": 4, "bounces": 0, "atlas_size": 32,
                                 "artifact": "bake_cube.ilmap" } },
            "entities": [
                { "name": "block", "mesh": { "primitive": { "kind": "cube" } },
                  "collider": { "shape": { "box": { "half_extents": [0.5, 0.5, 0.5] } },
                                "static": true } }
            ]
        }"#;
        let path = test_dir().join("bake_attach.level.json");
        std::fs::write(&path, level_with_bake).unwrap();
        let mut world = World::new();
        let entities = load_scene(&mut world, &path).expect("带产物的关卡应加载成功");
        assert_eq!(entities.len(), 1);
        assert!(
            world.contains_resource::<LightmapStore>(),
            "产物存在应挂光照贴图"
        );
        assert!(
            world.get::<LightmapBinding>(entities[0]).is_some(),
            "烘焙实体应挂 LightmapBinding"
        );
        let handle = *world.get::<MeshHandle>(entities[0]).unwrap();
        let has_uv2 = world
            .resource::<MeshStore>()
            .get(handle)
            .map(|mesh| mesh.uv2.is_some())
            .unwrap_or(false);
        assert!(has_uv2, "网格应已换 UV2 副本");
        // 登记表回补：副本句柄继承图元来源（save 还原 mesh 段）
        assert!(
            matches!(
                world.resource::<SceneAssetRegistry>().meshes.get(&handle.0),
                Some(crate::scene::MeshRef::Primitive(_))
            ),
            "UV2 副本句柄应登记原图元来源"
        );

        // ④ save → 重载：mesh 段还原为图元、产物重挂（往返闭环）
        let out = test_dir().join("bake_attach_out.json");
        save_scene(&mut world, &out).expect("保存");
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains(r#""primitive""#), "mesh 段应还原为图元来源");
        assert!(text.contains("bake_cube.ilmap"), "artifact 引用应回写");
        let mut world_b = World::new();
        let entities_b = load_scene(&mut world_b, &out).expect("重载");
        assert!(world_b.get::<LightmapBinding>(entities_b[0]).is_some());
        let handle_b = *world_b.get::<MeshHandle>(entities_b[0]).unwrap();
        assert_eq!(handle_b, handle, "UV2 副本句柄应可复现（同序加载）");
    }

    /// 产物缺失 → 诚实降级：加载成功、无 LightmapStore / LightmapBinding
    /// （告警走 log，测试断言世界状态）。
    #[test]
    fn bake_artifact_missing_degrades_to_no_lightmap() {
        let json = r#"{
            "version": 2,
            "level": { "bake": { "artifact": "不存在的产物.ilmap" } },
            "entities": [
                { "name": "block", "mesh": { "primitive": { "kind": "cube" } },
                  "collider": { "shape": { "box": { "half_extents": [0.5, 0.5, 0.5] } },
                                "static": true } }
            ]
        }"#;
        let mut world = World::new();
        let entities = load_str(&mut world, "bake_missing.level.json", json);
        assert_eq!(entities.len(), 1, "降级不阻断加载");
        assert!(
            !world.contains_resource::<LightmapStore>(),
            "产物缺失应无光照贴图目录"
        );
        assert!(world.get::<LightmapBinding>(entities[0]).is_none());
    }

    /// 陈旧产物（实体位模式失配）→ Unsupported 错误而非静默挂错。
    #[test]
    fn stale_bake_artifact_reports_error() {
        // 源世界烘焙（实体 0 = 立方）
        let mut source = World::new();
        load_str(
            &mut source,
            "stale_source.level.json",
            r#"{ "version": 2, "entities": [
                { "name": "block", "mesh": { "primitive": { "kind": "cube" } },
                  "collider": { "shape": { "box": { "half_extents": [0.5,0.5,0.5] } },
                                "static": true } } ] }"#,
        );
        iris_core::hierarchy::propagate_transforms(&mut source);
        let (params, uv) = BakeSection {
            sky_samples: 1,
            bounces: 0,
            atlas_size: 32,
            ..BakeSection::default()
        }
        .to_bake_params()
        .unwrap();
        let baked = bake_lightmap(&source, &params, &uv);
        let artifact = test_dir().join("stale.ilmap");
        save_lightmap_artifact(&baked, &artifact).expect("写产物");

        // 目标关卡在立方前多一个实体 → 立体的位模式整体后移（失配）
        let json = r#"{
            "version": 2,
            "level": { "bake": { "artifact": "stale.ilmap" } },
            "entities": [
                { "name": "shift" },
                { "name": "block", "mesh": { "primitive": { "kind": "cube" } },
                  "collider": { "shape": { "box": { "half_extents": [0.5,0.5,0.5] } },
                                "static": true } }
            ]
        }"#;
        let path = test_dir().join("stale_level.json");
        std::fs::write(&path, json).unwrap();
        let err = load_scene(&mut World::new(), &path).unwrap_err();
        assert!(
            matches!(err, SceneError::Unsupported(ref w) if w.contains("失配")),
            "陈旧产物应报 Unsupported：{err}"
        );
    }

    /// 产物文件损坏：魔数 / 版本 / 截断 → Unsupported（中文消息）。
    #[test]
    fn corrupt_artifact_reports_error() {
        let err = parse_lightmap_artifact(b"NOTIRIS!xxxxxxxx").unwrap_err();
        assert!(
            matches!(err, SceneError::Unsupported(ref w) if w.contains("魔数")),
            "{err}"
        );

        let mut bytes = Vec::new();
        bytes.extend_from_slice(LIGHTMAP_ARTIFACT_MAGIC);
        bytes.extend_from_slice(&99u32.to_le_bytes());
        let err = parse_lightmap_artifact(&bytes).unwrap_err();
        assert!(
            matches!(err, SceneError::Unsupported(ref w) if w.contains("版本")),
            "{err}"
        );

        let mut bytes = Vec::new();
        bytes.extend_from_slice(LIGHTMAP_ARTIFACT_MAGIC);
        bytes.extend_from_slice(&LIGHTMAP_ARTIFACT_VERSION.to_le_bytes());
        bytes.extend_from_slice(&4u32.to_le_bytes()); // size
        let err = parse_lightmap_artifact(&bytes).unwrap_err();
        assert!(
            matches!(err, SceneError::Unsupported(ref w) if w.contains("截断")),
            "{err}"
        );
    }

    /// bake 参数校验：bounces > 1 / sky_samples = 0 / atlas_size = 0 /
    /// light_dir 零向量 → OutOfRange（加载期即暴露，不等重烘 panic）。
    #[test]
    fn bake_params_validation() {
        for (json, needle) in [
            (
                r#"{ "version": 2, "level": { "bake": { "bounces": 2 } }, "entities": [] }"#,
                "bounces",
            ),
            (
                r#"{ "version": 2, "level": { "bake": { "sky_samples": 0 } }, "entities": [] }"#,
                "sky_samples",
            ),
            (
                r#"{ "version": 2, "level": { "bake": { "atlas_size": 0 } }, "entities": [] }"#,
                "atlas_size",
            ),
            (
                r#"{ "version": 2, "level": { "bake": { "light_dir": [0,0,0] } }, "entities": [] }"#,
                "light_dir",
            ),
        ] {
            let path = test_dir().join("bake_bad_params.json");
            std::fs::write(&path, json).unwrap();
            let err = load_scene(&mut World::new(), &path).unwrap_err();
            assert!(
                matches!(err, SceneError::OutOfRange(ref w) if w.contains(needle)),
                "应报 {needle} 越界：{err}"
            );
        }
    }

    /// 解析后的产物与原件逐位一致（全字段 to_bits 对照）。
    fn assert_artifact_eq(a: &BakedLightmap, b: &BakedLightmap) {
        assert_eq!(a.atlas.size, b.atlas.size);
        assert_eq!(a.atlas.used_height, b.atlas.used_height);
        assert_eq!(a.atlas.entries.len(), b.atlas.entries.len());
        for (ea, eb) in a.atlas.entries.iter().zip(&b.atlas.entries) {
            assert_eq!(ea.entity.to_bits(), eb.entity.to_bits());
            assert_eq!(ea.mesh, eb.mesh);
            assert_eq!(ea.axis, eb.axis);
            assert_eq!(ea.uv_rect.map(f32::to_bits), eb.uv_rect.map(f32::to_bits));
            assert_eq!(ea.origin, eb.origin);
            assert_eq!(ea.size, eb.size);
            assert_eq!(ea.texels_per_meter.to_bits(), eb.texels_per_meter.to_bits());
            assert_eq!(ea.triangles.len(), eb.triangles.len());
            for (ta, tb) in ea.triangles.iter().zip(&eb.triangles) {
                assert_eq!(ta.triangle, tb.triangle);
                assert_eq!(ta.uv_rect.map(f32::to_bits), tb.uv_rect.map(f32::to_bits));
                for (ca, cb) in ta.local.iter().zip(&tb.local) {
                    assert_eq!(ca.map(f32::to_bits), cb.map(f32::to_bits));
                }
            }
        }
        for (name, xa, xb) in [
            ("radiance", &a.radiance, &b.radiance),
            ("indirect", &a.indirect, &b.indirect),
        ] {
            assert_eq!(xa.len(), xb.len(), "{name} 长度");
            for (va, vb) in xa.iter().zip(xb) {
                assert_eq!(
                    [va.x.to_bits(), va.y.to_bits(), va.z.to_bits()],
                    [vb.x.to_bits(), vb.y.to_bits(), vb.z.to_bits()],
                    "{name} 逐 texel 位模式"
                );
            }
        }
        assert_eq!(a.covered, b.covered);
    }

    /// 产物文件对 preview 工具通路可用（radiance 完整存储的意义）：
    /// 解析回的产物能直接喂 write_lightmap_preview（不 panic、出文件）。
    #[test]
    fn parsed_artifact_feeds_preview_tooling() {
        let mut source = World::new();
        load_str(
            &mut source,
            "preview_source.level.json",
            r#"{ "version": 2, "entities": [
                { "mesh": { "primitive": { "kind": "cube" } },
                  "collider": { "shape": { "box": { "half_extents": [0.5,0.5,0.5] } },
                                "static": true } } ] }"#,
        );
        iris_core::hierarchy::propagate_transforms(&mut source);
        let (params, uv) = BakeSection {
            sky_samples: 1,
            bounces: 0,
            atlas_size: 32,
            ..BakeSection::default()
        }
        .to_bake_params()
        .unwrap();
        let baked = bake_lightmap(&source, &params, &uv);
        let parsed = parse_lightmap_artifact(&encode_lightmap_artifact(&baked)).unwrap();
        let out = test_dir().join("artifact_preview.png");
        write_lightmap_preview(&parsed, &out).expect("预览导出");
        assert!(out.exists() && std::fs::metadata(&out).unwrap().len() > 0);
    }

    /// 版本字段：0 / 高于当前 → Unsupported；显式 version = 1 走迁移
    /// （空 level 段）；重存升为 v2 且省略空 level 段。
    #[test]
    fn version_validation_and_migration() {
        let path = test_dir().join("version_bad.json");
        std::fs::write(&path, r#"{ "version": 0, "entities": [] }"#).unwrap();
        let err = load_scene(&mut World::new(), &path).unwrap_err();
        assert!(
            matches!(err, SceneError::Unsupported(ref w) if w.contains("版本")),
            "{err}"
        );

        std::fs::write(&path, r#"{ "version": 3, "entities": [] }"#).unwrap();
        let err = load_scene(&mut World::new(), &path).unwrap_err();
        assert!(
            matches!(err, SceneError::Unsupported(ref w) if w.contains("版本")),
            "{err}"
        );

        // 显式 v1：迁移后与 v2 空档等价（LevelRegistry 段为空）
        std::fs::write(&path, r#"{ "version": 1, "entities": [{}] }"#).unwrap();
        let mut world = World::new();
        let entities = load_scene(&mut world, &path).expect("v1 显式版本可加载");
        assert_eq!(entities.len(), 1);
        assert!(world.resource::<LevelRegistry>().section.is_empty());
        // 重存：version 升到 2，空 level 段省略
        let out = test_dir().join("version_migrated_out.json");
        save_scene(&mut world, &out).expect("保存");
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains(r#""version": 2"#));
        assert!(!text.contains(r#""level""#), "空 level 段应省略");
    }

    /// serde_json 的 f32 往返逐位保真（规格实测项）：确定性伪随机位模式
    /// （splitmix64 流，排除 NaN / Inf——JSON 非法值）+ 边界值全集，
    /// 经「序列化 → 反序列化」后 to_bits 相等 ⇒ 关卡档不需要自定义浮点
    /// 输出策略（结论记录于 CHANGELOG 与 REPORT）。
    #[test]
    fn serde_json_f32_roundtrip_is_bitwise_exact() {
        fn mix64(mut z: u64) -> u64 {
            z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }
        let mut samples: Vec<f32> = vec![
            0.0,
            -0.0,
            1.0,
            -1.0,
            0.1,
            0.42,
            -7.1,
            f32::MIN_POSITIVE,
            f32::MIN_POSITIVE / 2.0, // 次正规
            f32::from_bits(1),       // 最小次正规
            f32::MAX,
            f32::MIN,
            1e-40,
            1e38,
            std::f32::consts::FRAC_PI_4,
            std::f32::consts::PI,
        ];
        let mut hash = 0x5eed_u64;
        for _ in 0..4096 {
            hash = mix64(hash);
            let value = f32::from_bits((hash >> 16) as u32);
            if value.is_finite() {
                samples.push(value);
            }
        }
        for value in samples {
            let json = serde_json::to_string(&value).expect("有限 f32 可序列化");
            let back: f32 = serde_json::from_str(&json).expect("回读");
            assert_eq!(
                value.to_bits(),
                back.to_bits(),
                "f32 经 serde_json 往返应逐位一致：{value}（{json}）"
            );
        }
        // 数组字段路径（TransformSection / NavSection 的实际形态）
        let array = [0.1f32, -7.1, 1e-38, f32::MAX];
        let json = serde_json::to_string(&array).unwrap();
        let back: [f32; 4] = serde_json::from_str(&json).unwrap();
        assert_eq!(array.map(f32::to_bits), back.map(f32::to_bits));
    }

    /// 关卡档内浮点字段的端到端位保真：手写十进制（含 f32 最短表示的
    /// 长小数）→ 加载 → save 回写 → 再加载，Transform 与 NavGridParams
    /// 全字段 to_bits 逐位一致。
    #[test]
    fn level_float_fields_survive_roundtrip_bitwise() {
        let json = r#"{
            "version": 2,
            "level": { "nav": { "bounds_min": [-10.399999618530273, 0.0, -7.400000095367432],
                                "bounds_max": [10.4, 0.0, 7.4],
                                "cell_size": 0.5, "agent_radius": 0.45,
                                "step_height": 0.3,
                                "max_slope_radians": 0.7853981633974483 } },
            "entities": [{ "transform": { "position": [0.30000001192092896, 1.5, -7.1],
                                           "scale": [0.6000000238418579, 3.0, 14.8] } }]
        }"#;
        let path = test_dir().join("float_bits.level.json");
        std::fs::write(&path, json).unwrap();
        let mut world_a = World::new();
        let entities_a = load_scene(&mut world_a, &path).expect("首次加载");
        let out = test_dir().join("float_bits_out.json");
        save_scene(&mut world_a, &out).expect("保存");
        let mut world_b = World::new();
        let entities_b = load_scene(&mut world_b, &out).expect("二次加载");

        let ta = *world_a.get::<Transform>(entities_a[0]).unwrap();
        let tb = *world_b.get::<Transform>(entities_b[0]).unwrap();
        assert_eq!(
            ta.position.to_array().map(f32::to_bits),
            tb.position.to_array().map(f32::to_bits),
            "position 逐位"
        );
        assert_eq!(
            ta.scale.to_array().map(f32::to_bits),
            tb.scale.to_array().map(f32::to_bits),
            "scale 逐位"
        );
        let bits_a = {
            let grid = world_a.resource::<NavGrid>();
            params_bits(&grid.params())
        };
        let bits_b = {
            let grid = world_b.resource::<NavGrid>();
            params_bits(&grid.params())
        };
        assert_eq!(bits_a, bits_b, "NavGridParams 全字段位模式往返一致");
        assert_eq!(bits_a.2, 0.5f32.to_bits(), "cell_size 解析值");
    }

    // ------------------------------------------------------------------
    // R89：ddgi 子段
    // ------------------------------------------------------------------

    /// `level.ddgi` 段：逐字段 serde 往返逐位（f32 位模式 + bounds 成对
    /// 缺席 = 自动派生）+ 加载侧段 → 资源 + 保存侧资源 → 段（save_scene）。
    #[test]
    fn ddgi_section_roundtrip_is_bitwise() {
        // 手写段（非默认值覆盖每个字段）
        let json = r#"{
            "version": 2,
            "entities": [],
            "level": { "ddgi": {
                "spacing": 2.5,
                "bounds_min": [-8.0, 0.0, -8.0],
                "bounds_max": [8.0, 3.5, 8.0],
                "probes_per_frame": 4,
                "rays_per_face": 32,
                "hysteresis": 0.25,
                "sky_color": [0.1, 0.2, 0.3],
                "bounces": 0
            } }
        }"#;
        let mut world = World::new();
        load_str(&mut world, "ddgi_rt.json", json);
        let settings = world
            .try_resource::<DdgiSettings>()
            .expect("ddgi 段 ⇒ DdgiSettings 资源");
        assert_eq!(settings.spacing.to_bits(), 2.5f32.to_bits());
        assert_eq!(
            settings.bounds,
            Some((Vec3::new(-8.0, 0.0, -8.0), Vec3::new(8.0, 3.5, 8.0)))
        );
        assert_eq!(settings.probes_per_frame, 4);
        assert_eq!(settings.rays_per_face, 32);
        assert_eq!(settings.hysteresis.to_bits(), 0.25f32.to_bits());
        assert_eq!(settings.sky_color, Some(Vec3::new(0.1, 0.2, 0.3)));
        assert_eq!(settings.bounces, 0);
        // geometry_generation 不入档 ⇒ 恒 0（运行期记账，非作者意图）
        assert_eq!(settings.geometry_generation, 0);

        // bounds 双缺席 = None（自动派生口径透传）
        let json_auto = r#"{
            "version": 2,
            "entities": [],
            "level": { "ddgi": { "spacing": 3.0 } }
        }"#;
        let mut world = World::new();
        load_str(&mut world, "ddgi_auto.json", json_auto);
        let settings = world.try_resource::<DdgiSettings>().expect("在场");
        assert_eq!(settings.spacing.to_bits(), 3.0f32.to_bits());
        assert_eq!(settings.bounds, None, "双缺席 = 自动派生");
        assert_eq!(
            settings.probes_per_frame,
            DdgiSettings::default().probes_per_frame,
            "其余字段缺省 = 引擎默认"
        );
    }

    /// `level.ddgi` 段的校验拦截：半 bounds（只有 min）、非法 spacing、
    /// 非法 rays 档、bounces > 1 → `SceneError`（不 panic）。
    #[test]
    fn ddgi_section_validates_before_engine_panics() {
        let cases = [
            (r#""ddgi": { "bounds_min": [0,0,0] }"#, "成对"),
            (r#""ddgi": { "spacing": -1.0 }"#, "正有限"),
            (r#""ddgi": { "rays_per_face": 24 }"#, "rays_per_face"),
            (r#""ddgi": { "bounces": 2 }"#, "bounces"),
        ];
        for (i, (ddgi_json, expect_msg)) in cases.iter().enumerate() {
            let json = format!(r#"{{ "version": 2, "entities": [], "level": {{ {ddgi_json} }} }}"#);
            let mut world = World::new();
            let path = test_dir().join(format!("ddgi_bad{i}.json"));
            std::fs::write(&path, json).expect("写关卡文件");
            let err = load_scene(&mut world, &path).expect_err("必须报错");
            assert!(
                err.to_string().contains(expect_msg),
                "用例 {i}：{err} 应包含 {expect_msg:?}"
            );
            assert!(
                !world.contains_resource::<DdgiSettings>(),
                "校验失败不插资源"
            );
        }
    }

    /// 保存侧：`DdgiSettings` 资源在场 ⇒ `level.ddgi` 段写出；二次保存
    /// 逐字节一致（资源 → 段的回写稳定）。无资源 ⇒ 无段（由
    /// `scene.rs` 与编辑器测试覆盖，此处只钉资源面）。
    #[test]
    fn save_writes_ddgi_section_from_live_resource() {
        let mut world = World::new();
        world.insert_resource(DdgiSettings {
            spacing: 3.0,
            bounds: Some((Vec3::new(-4.0, 0.0, -4.0), Vec3::new(4.0, 2.0, 4.0))),
            probes_per_frame: 5,
            rays_per_face: 64,
            hysteresis: 0.5,
            sky_color: Some(Vec3::new(0.2, 0.2, 0.25)),
            sky_from_source: true,
            lights_in_gi: true,
            point_light_shadows: false,
            bounces: 1,
            geometry_generation: 7,
        });
        world.insert_resource(crate::level::LevelRegistry::default());
        let path = test_dir().join("ddgi_save.json");
        crate::scene::save_scene(&mut world, &path).expect("保存");
        let text = std::fs::read_to_string(&path).expect("读");
        assert!(text.contains("\"ddgi\""), "ddgi 段必须写出：{text}");
        assert!(!text.contains("geometry_generation"), "运行期记账不入档");
        // 逐字节重存
        crate::scene::save_scene(&mut world, &path).expect("重存");
        let text2 = std::fs::read_to_string(&path).expect("读");
        assert_eq!(text, text2, "二次保存逐字节一致");
        // 重载资源复原（geometry_generation 归零是既有语义）
        let mut reloaded = World::new();
        load_str(&mut reloaded, "ddgi_save_reload.json", &text);
        let settings = reloaded.try_resource::<DdgiSettings>().expect("在场");
        assert_eq!(settings.spacing.to_bits(), 3.0f32.to_bits());
        assert_eq!(settings.rays_per_face, 64);
        assert_eq!(settings.probes_per_frame, 5);
        assert_eq!(settings.hysteresis.to_bits(), 0.5f32.to_bits());
        assert_eq!(
            settings.bounds,
            Some((Vec3::new(-4.0, 0.0, -4.0), Vec3::new(4.0, 2.0, 4.0)))
        );
        assert_eq!(settings.sky_color, Some(Vec3::new(0.2, 0.2, 0.25)));
        assert!(settings.sky_from_source, "C7a 同源开关须往返");
        assert!(settings.lights_in_gi, "C7a 灯进 GI 总开关须往返");
        assert!(!settings.point_light_shadows, "C7a 硬影逃生门须往返");
    }
}
