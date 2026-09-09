//! 场景序列化：JSON 场景描述文件 ↔ 世界实体（关卡数据与游戏代码分离）。
//!
//! **格式 v2（R57）= 关卡档**：v1 纯场景档的超集——顶层 `version` 字段 +
//! `level` 段（地形 / 导航构建参数 / 光照贴图烘焙参数，类型与语义全文见
//! [`crate::level`]）。`version` 缺席 = v1 档无损直读（迁移钩子加默认空
//! `level` 段），save 恒写当前版本。本模块是加载 / 保存的**单通路**：
//! 两代格式共用同一 IO 与实体段逻辑。
//!
//! 场景文件描述两类内容：**环境段**（场景级资源形态：方向光 + 天空源）与
//! **实体段**（逐实体挂载内置组件集）。缺省值全部与引擎默认一致——
//! `Transform` 恒等、材质取 [`MaterialDef::default`]、点光字段照
//! [`PointLight::default`]、粒子字段照 `ParticleEmitter::new(Alpha)`，
//! 手写关卡文件时只需写与默认不同的字段。
//!
//! ```jsonc
//! {
//!   "version": 2,                                   // 可选；缺席 = v1
//!   "environment": {                              // 可选；缺席不改既有资源
//!     "directional_light": {                      // 各字段可选，缺省 = 引擎默认
//!       "direction": [-0.5, -1.0, -0.4], "color": [1.0, 0.98, 0.92],
//!       "intensity": 1.0, "ambient": [0.14, 0.14, 0.14], "shadows": true,
//!       "shadow_range": 24.0                      // 可选；缺省省略（v2 补齐）
//!     },
//!     "sky_source": "assets/sunset.hdr"           // 可选；缺席 = 程序化天空
//!   },
//!   "level": { /* 地形 / nav / bake，见 crate::level */ },
//!   "entities": [                                 // 必需
//!     {
//!       "name": "wall-north",                     // 可选 → Name 组件
//!       "visible": false,                         // 可选，缺省可见 → Visibility
//!       "transform": { "position": [0,0,0], "rotation": [0,0,0,1], "scale": [1,1,1] },
//!       "mesh":    { "primitive": { "kind": "cube" } },   // 三态之一（见下）
//!       "material": { "color": [0.8,0.2,0.1,1], "metallic": 0.0,
//!                     "roughness": 0.5, "emissive": [0,0,0],
//!                     "texture": "assets/tex.png",        // 或程序化源（见下）
//!                     "normal_texture": { "file": "assets/n.png" },
//!                     "orm_texture": { "brick_normal_orm": { "rows": 8,
//!                                        "cols": 5, "size": 256,
//!                                        "seam": 0.05, "bump": 2.4 } },
//!                     "alpha_mode": "Blend" },       // 全可选；缺省 Opaque
//!       "point_light": { "color": [1,1,1], "intensity": 20, "range": 30,
//!                        "shadows": false },
//!       "particles": { "blend": "alpha", "spawn_rate": 90.0, /* 全可选 */ },
//!       "collider":  { "shape": { "sphere": { "radius": 0.5 } },
//!                      "static": true, "restitution": 0.2 },
//!       "rigidbody": { "gravity_scale": 1.0, "friction": 0.3,
//!                      "linear_damping": 0.0 }
//!     }
//!   ]
//! }
//! ```
//!
//! **mesh 三态**（`mesh` 字段缺席 = 纯逻辑 / 灯光实体）：
//! - `{"primitive": {"kind": "cube"}}`——图元：`cube` / `plane`（size）/
//!   `sphere`（radius, segments, rings）/ `cylinder`（radius, height, segments）；
//! - `{"gltf": "assets/x.glb"}`——整实例挂载：[`spawn_gltf_instance`] 生成的
//!   顶层实体作为该实体的**子实体**（本实体 `transform` 作用于整棵子树）；
//! - 缺席。
//!
//! **纹理源两态**（材质段的 `texture` / `normal_texture` / `orm_texture`，
//! v2 新增后两槽）：字符串 = 文件路径（PNG / JPEG，v1 语义原样）；对象 =
//! 程序化源（`{"grass": {size}}` 三件套 / `{"brick_normal_orm": {rows, cols,
//! size, seam, bump}}` 法线 + ORM 对——引擎侧确定性生成器全参数，同参逐位
//! 一致）。sRGB 语义按槽位：基色 = sRGB，法线 / ORM = 线性数据。程序化源
//! 让写实材质**零二进制资产**入档（关卡文件保持纯文本自包含）。
//!
//! **particles 段**（v2 新增）：[`ParticleEmitter`] 的全部配置字段
//! （blend / spawn_rate / burst_count / emitter_lifetime / particle_lifetime /
//! initial_speed / velocity_jitter / gravity_scale / size_start / size_end /
//! color_start / color_end / max_particles），缺省 = `new(Alpha)`。运行期
//! 状态（粒子池 / 计数器）不入档——重载后发射器从新鲜状态开始。
//!
//! # 往返（save ↔ load）
//!
//! [`save_scene`] 查询内置组件集（Transform + 可选 Name / Visibility /
//! MeshHandle / MaterialHandle / PointLight / ParticleEmitter / Collider /
//! RigidBody）全量写出，[`load_scene`] 逐实体还原，构成不变量
//! `load(save(load(f)))` 与 `load(f)` 实体组件语义等价（实体顺序 =
//! spawn 顺序，稳定）。mesh / material / 天空源的**来源**（图元参数、
//! glTF 文件路径、贴图路径 / 程序化参数）由 [`SceneAssetRegistry`]
//! 资源在 load 时登记、save 时还原——`MeshStore` / `TextureStore` 本身没有
//! 「文件路径」概念，登记表是数值句柄与场景文件之间的桥：
//!
//! - mesh 句柄在登记表 → 还原图元段 / glTF 段；不在（用户代码 `MeshStore::add`）
//!   → mesh 段省略 + `log::warn`（数量统计，重新加载后该实体没有网格）；
//! - material 句柄在登记表 → 数值字段照 [`MaterialDef`] 现读 + 登记的
//!   纹理来源（基色 / 法线 / ORM 三槽，文件路径或程序化参数）；不在 →
//!   只写数值字段 + 纹理来源丢失警告（纯数值材质无损）；
//!   句柄 0（默认材质，隐式）→ 整段省略；
//! - 天空源为 `EquirectHdr` 且登记了路径 → 写 `sky_source`；无登记（用户手工
//!   挂入）→ 省略 + 警告（v1 简化：HDR 图像内容不回写文件）。
//!
//! **同源去重（v2）**：一次加载内，参数相同的图元 mesh、同路径 / 同参数
//! 的纹理、解析结果相同的材质**共享同一句柄**（首见注册、后见复用）——
//! 「文件内同源 ⇒ 世界内同句柄」让手写关卡（九面墙同引用一个 cube）与
//! 代码侧共享网格的布局一致，批处理 / 内存与逐位复现都以此为前提。
//! glTF 例外：整实例挂载每次引用独立解析（v1 语义原样，见 §4.15）。
//!
//! glTF 实例生成的内部实体（根 + 后代）带 [`GltfMounted`] 标记、
//! `level.terrain` 段派生的实体（根 / chunk / 碰撞拼块）带
//! [`LevelGenerated`](crate::level::LevelGenerated) 标记，save 时跳过
//! ——它们由对应段在下次加载时整体重建，不属于实体段本身。`level` 段
//! 与 `version` 的回写由 [`LevelRegistry`](crate::level::LevelRegistry)
//! 登记驱动（加载时快照，语义见 [`crate::level`] 模块文档）。
//!
//! # 诚实清单（格式限制）
//!
//! - UI 组件（HUD 属游戏逻辑，`UiRect` / `UiText` 实体不参与往返）与相机
//!   （资源级，由代码或平台层配置）；
//! - 克隆 / 预设 / 组件插件扩展点（自定义组件不经序列化）；
//! - `RigidBody.velocity` 不写入文件（物体从静止开始——把模拟中途的速度
//!   冻结进关卡文件不是想要的行为）；`DirectionalLight.shadow_range`
//!   v2 起可选序列化（等于引擎默认时省略，v1 文件重存字节级不变）；
//! - 粒子段只写配置字段，运行期状态（粒子池 / 计数器）不入档；
//! - `level` 段的格式侧限制（双源常量 / 登记快照语义 / 外置高度场缺席）
//!   见 [`crate::level`] 模块文档诚实清单；
//! - 热重载：场景文件为手工编写，加载后与文件的关联即断开；
//! - wasm32 注意：`load_scene` 的场景 / glTF / HDR / 贴图读取走
//!   `std::fs`——native 可用，**web 上不可用**；web 场景用 [`load_scene_from`]
//!   + 嵌入源（R45，文本类场景 JSON + 自包含 GLB 走挂载通路）。
//!
//! # 用法
//!
//! ```ignore
//! use iris_engine::prelude::*;
//!
//! // 加载：插入缺失的资源兜底 → 写环境 → 逐实体 spawn
//! let entities = load_scene(&mut world, "assets/arena.scene.json")?;
//!
//! // 挂载变体（R45）：场景与子资产从 mount 顺序查找读取（pack / 嵌入）
//! let entities = load_scene_from(&mut world, &mount, "scenes/arena.scene.json")?;
//!
//! // 保存：内置组件集 + 登记的资产来源全量写出，返回实体数
//! let count = save_scene(&mut world, "assets/arena_out.json")?;
//! ```

use crate::level::{LevelRegistry, LevelSection, apply_bake, apply_nav, apply_terrain};
use crate::vfs::AssetMount;
use iris_core::name::Name;
use iris_core::physics::{BodyType, Collider, ColliderShape, RigidBody};
use iris_core::prelude::{Entity, Quat, Transform, Vec3, World};
use iris_core::{Component, Resource};
use iris_render::asset::{
    GltfError, GltfInstance, HdrError, HdrImage, load_hdr, parse_hdr, spawn_gltf_instance,
    spawn_gltf_instance_from_bytes,
};
use iris_render::prelude::{
    AlphaMode, CpuTexture, DirectionalLight, GrassTextures, MaterialDef, MaterialHandle,
    MaterialStore, MeshData, MeshHandle, MeshStore, ParticleBlend, ParticleEmitter, PointLight,
    SkySource, SkySourceKind, TextureHandle, TextureStore, Visibility, grass_textures,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// 场景加载 / 保存错误（风格同 [`GltfError`]）。
#[derive(Debug)]
pub enum SceneError {
    /// 文件读取 / 写入失败。
    Io(std::io::Error),
    /// JSON 解析失败（坏 JSON / 字段类型不符 / 缺少必需的 JSON 字段）。
    Json(serde_json::Error),
    /// 场景结构缺少必需的段落（如 `entities`）。
    Missing(String),
    /// 合法 JSON 但引用了不支持的特性或资产（未知图元 / 坏 HDR / glTF 不支持项）。
    Unsupported(String),
    /// 参数越界（如球体分段数过低会触底图元生成器的断言，提前拦截为错误）。
    OutOfRange(String),
    /// 子资产（glTF / 贴图 / HDR / 场景本体）经挂载（[`AssetMount`]）
    /// 读取失败：未命中或源内损坏（R45）。
    Asset(String),
}

impl fmt::Display for SceneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO 错误：{e}"),
            Self::Json(e) => write!(f, "JSON 解析错误：{e}"),
            Self::Missing(what) => write!(f, "场景缺少必需字段：{what}"),
            Self::Unsupported(what) => write!(f, "暂不支持的场景特性或资产：{what}"),
            Self::OutOfRange(what) => write!(f, "场景参数越界：{what}"),
            Self::Asset(what) => write!(f, "挂载子资产读取失败：{what}"),
        }
    }
}

impl std::error::Error for SceneError {}

impl From<std::io::Error> for SceneError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for SceneError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl From<GltfError> for SceneError {
    fn from(e: GltfError) -> Self {
        match e {
            GltfError::Io(e) => Self::Io(e),
            GltfError::Json(e) => Self::Json(e),
            // 场景错误枚举没有图片专用项：解码失败归入 Unsupported（诚实报错）
            GltfError::Image(e) => Self::Unsupported(format!("glTF 内嵌图片解码失败：{e}")),
            GltfError::Unsupported(what) => Self::Unsupported(what),
            GltfError::Missing(what) => Self::Missing(what),
            GltfError::OutOfRange(what) => Self::OutOfRange(what),
        }
    }
}

impl From<HdrError> for SceneError {
    fn from(e: HdrError) -> Self {
        match e {
            HdrError::Io(e) => Self::Io(e),
            HdrError::Format(what) => Self::Unsupported(format!("HDR 数据损坏：{what}")),
            HdrError::Unsupported(what) => Self::Unsupported(what),
        }
    }
}

// ---------------------------------------------------------------------------
// 登记表与标记
// ---------------------------------------------------------------------------

/// 场景 mesh 段的来源（登记表用）：图元参数或 glTF 文件路径。
#[derive(Clone, Debug, PartialEq)]
pub enum MeshRef {
    /// 图元网格（参数在 load 时登记，save 时原样还原）。
    Primitive(PrimitiveSection),
    /// glTF 文件（路径为场景文件内的原始相对路径，save 时原样写出）。
    Gltf(String),
}

/// 场景 material 段的来源：`TextureStore` 没有路径 / 生成参数概念，load
/// 时把三个纹理槽（基色 / 法线 / ORM）的来源登记在此，save 时据此还原
/// 对应字段（数值字段照 [`MaterialDef`] 现读）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MaterialRef {
    /// 基础色贴图的场景内相对路径（**仅文件来源**登记于此，v1 字段语义
    /// 原样）；程序化基色源登记在 [`Self::base_source`]，两者互斥。
    pub texture: Option<String>,
    /// 基础色的程序化来源（与 [`Self::texture`] 互斥；均为 `None` =
    /// 无基色贴图，白色兜底句柄 0）。
    pub base_source: Option<ProceduralTextureSection>,
    /// 法线贴图来源（文件路径或程序化参数；`None` = 无该槽）。
    pub normal_source: Option<TextureSourceSection>,
    /// ORM 贴图来源（同上）。
    pub orm_source: Option<TextureSourceSection>,
}

/// 纹理源两态（材质段三槽共用）：字符串 = 文件路径（PNG / JPEG，v1
/// `texture` 字段语义原样）；对象 = 程序化源。serde `untagged`——旧档的
/// `"texture": "x.png"` 字符串形式直读为 [`Self::File`]（向后兼容），
/// 序列化同样还原为裸字符串（旧档重存字节级稳定）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum TextureSourceSection {
    /// 场景文件内相对路径（PNG / JPEG）。
    File(String),
    /// 程序化生成器参数（引擎侧纯函数，同参逐位一致）。
    Procedural(ProceduralTextureSection),
}

/// 程序化纹理源（外部标签，键名即生成器名）：参数与引擎生成器一一对应，
/// **不复制粘贴字段语义**——`grass` = [`grass_textures`]（三件套：基色
/// sRGB + 法线 / ORM 线性），`brick_normal_orm` =
/// [`CpuTexture::brick_normal_orm`]（法线 + ORM 对，无基色产物——用于
/// 基色槽报 [`SceneError::Unsupported`]）。
///
/// 同参数在一次加载内只生成一次（[`LoadCache`]），逐槽按需登记进
/// `TextureStore`——槽位语义决定 sRGB 标志（基色 sRGB / 法线 ORM 线性）。
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ProceduralTextureSection {
    /// [`grass_textures`]：`size` ≥ 8（生成器断言前置校验为
    /// [`SceneError::OutOfRange`]）。
    Grass { size: u32 },
    /// [`CpuTexture::brick_normal_orm`]：`rows` / `cols` ≥ 1、`size` ≥ 4
    /// （同上）；`seam` 越出 [0.01, 0.45] 时生成器钳位（引擎语义原样）。
    BrickNormalOrm {
        rows: u32,
        cols: u32,
        size: u32,
        seam: f32,
        bump: f32,
    },
}

/// 纹理槽位（sRGB 语义与程序化产物选择的判据）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextureSlot {
    /// 基色（sRGB 编码）。
    Base,
    /// 法线（线性数据）。
    Normal,
    /// ORM 打包（线性数据）。
    Orm,
}

/// 场景资产登记表（World 资源）：记录「场景创建的句柄 ↔ 场景文件内的来源」。
///
/// `MeshStore` / `MaterialStore` / `TextureStore` 只存数值与像素，不存
/// 「这份数据来自哪个文件」；[`save_scene`] 需要把 mesh 段还原成
/// `{"primitive": ...}` / `{"gltf": ...}`、把材质还原出 `texture` 路径、
/// 把 HDR 天空源还原成 `sky_source` 路径——这些信息全部在 [`load_scene`]
/// 时登记于此。非场景来源（用户代码直接 add）的句柄不进登记表，save 时
/// 按各字段的降级规则省略并告警。
#[derive(Default, Resource)]
pub struct SceneAssetRegistry {
    /// MeshHandle 数值 → 场景来源（图元参数 / glTF 路径）。
    pub meshes: HashMap<u32, MeshRef>,
    /// MaterialHandle 数值 → 贴图路径登记。
    pub materials: HashMap<u32, MaterialRef>,
    /// glTF 挂载实体的 scene 实体 → 文件路径（该实体本身无 MeshHandle，
    /// glTF 实例树整体作为其子实体存在）。
    pub gltfs: HashMap<Entity, String>,
    /// 天空源路径（原样字符串）。仅在 `SkySource` 由场景文件载入时登记。
    pub sky_source: Option<String>,
}

/// glTF 子场景内部实体标记：[`load_scene`] 挂载 glTF 实例时给实例生成的
/// 全部实体（根 + 后代）挂上，[`save_scene`] 据此跳过——这些实体由 mesh 段的
/// `{"gltf": ...}` 在下次加载时整体重建，不属于场景文件本身。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Component)]
pub struct GltfMounted;

// ---------------------------------------------------------------------------
// JSON 格式定义（serde；全蛇形命名，与模块文档的格式样例一致）
// ---------------------------------------------------------------------------

/// 当前场景 / 关卡格式版本（R57 = v2：`level` 段与 `version` 字段）。
pub const SCENE_FORMAT_VERSION: u32 = 2;

/// 场景文档根：版本（可选，缺席 = v1）+ 环境段（可选）+ level 段
/// （可选，v2）+ 实体段（必需）。字段声明序 = JSON 键序（save 输出
/// 规范化：固定顺序）。
#[derive(Serialize, Deserialize, Debug)]
struct SceneDoc {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    environment: Option<EnvironmentSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    level: Option<LevelSection>,
    /// save 恒为 `Some`；load 缺席 → [`SceneError::Missing`]。
    entities: Option<Vec<EntitySection>>,
}

/// 版本迁移钩子：把 `version = 下标 + 1` 的文档就地升到下一版。
/// 表长恒 = [`SCENE_FORMAT_VERSION`] − 1（每版一步，顺序施加）。
type Migration = fn(&mut SceneDoc) -> Result<(), SceneError>;

/// 迁移钩子函数表（索引 = 源版本 − 1）。
const MIGRATIONS: &[Migration] = &[migrate_v1_to_v2];

/// v1 → v2：加默认（空）`level` 段——v1 纯场景档无损直读，这就是第一条
/// 迁移路径（空段在 save 时省略，v1 内容重存只多 `version` 字段）。
fn migrate_v1_to_v2(doc: &mut SceneDoc) -> Result<(), SceneError> {
    if doc.level.is_none() {
        doc.level = Some(LevelSection::default());
    }
    Ok(())
}

/// 版本校验 + 顺序迁移：返回迁移后的文档（`version` 归一为当前版）。
fn migrate_doc(doc: &mut SceneDoc) -> Result<(), SceneError> {
    let version = doc.version.unwrap_or(1);
    if version == 0 || version > SCENE_FORMAT_VERSION {
        return Err(SceneError::Unsupported(format!(
            "场景文件版本 = {version}，本引擎支持 1..={SCENE_FORMAT_VERSION}\
             （高于当前版本的档请升级引擎——拒读而非误读）"
        )));
    }
    for migration in &MIGRATIONS[(version - 1) as usize..] {
        migration(doc)?;
    }
    doc.version = Some(SCENE_FORMAT_VERSION);
    Ok(())
}

/// 环境段：场景级资源形态（非实体）。
#[derive(Serialize, Deserialize, Debug, Default)]
struct EnvironmentSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    directional_light: Option<DirectionalLightSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sky_source: Option<String>,
}

/// 方向光段：各字段缺省 = [`DirectionalLight::default`]。
/// `shadow_range` v2 起可选序列化（等于引擎默认时省略——v1 文件重存
/// 字节级不变；该字段自 R29 起不参与生产渲染路径，序列化是为档面完整）。
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
struct DirectionalLightSection {
    direction: [f32; 3],
    color: [f32; 3],
    intensity: f32,
    ambient: [f32; 3],
    shadows: bool,
    #[serde(skip_serializing_if = "shadow_range_is_default")]
    shadow_range: f32,
}

/// `shadow_range` 序列化省略条件：等于引擎默认（旧档重存字节级稳定，
/// 与 `alpha_mode` 的省略策略同款）。
fn shadow_range_is_default(value: &f32) -> bool {
    *value == DirectionalLight::default().shadow_range
}

impl Default for DirectionalLightSection {
    fn default() -> Self {
        let light = DirectionalLight::default();
        Self {
            direction: light.direction.to_array(),
            color: light.color.to_array(),
            intensity: light.intensity,
            ambient: light.ambient.to_array(),
            shadows: light.shadows,
            shadow_range: light.shadow_range,
        }
    }
}

impl DirectionalLightSection {
    fn to_light(&self) -> DirectionalLight {
        DirectionalLight {
            direction: self.direction.into(),
            color: self.color.into(),
            intensity: self.intensity,
            ambient: self.ambient.into(),
            shadows: self.shadows,
            shadow_range: self.shadow_range,
        }
    }

    fn from_light(light: &DirectionalLight) -> Self {
        Self {
            direction: light.direction.to_array(),
            color: light.color.to_array(),
            intensity: light.intensity,
            ambient: light.ambient.to_array(),
            shadows: light.shadows,
            shadow_range: light.shadow_range,
        }
    }
}

/// 实体段：全部字段可选（最小实体 = `{}`，恒等变换纯逻辑实体）。
/// serde 默认忽略未知字段（前向兼容：新版本文件在旧引擎上可加载）。
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
struct EntitySection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// 只在隐藏时写出（缺省可见，与 [`Visibility`] 的缺席语义一致）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    visible: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transform: Option<TransformSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mesh: Option<MeshSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    material: Option<MaterialSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    point_light: Option<PointLightSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    particles: Option<ParticleSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    collider: Option<ColliderSection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rigidbody: Option<RigidBodySection>,
}

/// 变换段：全缺省 = [`Transform::IDENTITY`]。四元数为 (x, y, z, w)（glam 序）。
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(default)]
struct TransformSection {
    position: [f32; 3],
    rotation: [f32; 4],
    scale: [f32; 3],
}

impl Default for TransformSection {
    fn default() -> Self {
        Self {
            position: [0.0; 3],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0; 3],
        }
    }
}

impl TransformSection {
    fn to_transform(self) -> Transform {
        Transform {
            position: self.position.into(),
            rotation: Quat::from_xyzw(
                self.rotation[0],
                self.rotation[1],
                self.rotation[2],
                self.rotation[3],
            ),
            scale: self.scale.into(),
        }
    }

    /// 恒等变换省略整段（文件更可读，且保证 save 输出稳定）。
    fn from_transform(transform: &Transform) -> Option<Self> {
        if *transform == Transform::IDENTITY {
            return None;
        }
        Some(Self {
            position: transform.position.to_array(),
            rotation: transform.rotation.to_array(),
            scale: transform.scale.to_array(),
        })
    }
}

/// mesh 段三态：图元 / glTF / 缺席（外部标签形式，键名 `primitive` / `gltf`）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
enum MeshSection {
    Primitive(PrimitiveSection),
    Gltf(String),
}

/// 图元段：内部标签 `kind`；参数缺省见各变体（单位立方 / 单位平面 / 半径 0.5
/// 的 32×16 球 / 半径 0.5 高 1 的 32 段圆柱）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrimitiveSection {
    Cube,
    Plane {
        #[serde(default = "default_plane_size")]
        size: f32,
    },
    Sphere {
        #[serde(default = "default_radius")]
        radius: f32,
        #[serde(default = "default_sphere_segments")]
        segments: u32,
        #[serde(default = "default_rings")]
        rings: u32,
    },
    Cylinder {
        #[serde(default = "default_radius")]
        radius: f32,
        #[serde(default = "default_height")]
        height: f32,
        #[serde(default = "default_cylinder_segments")]
        segments: u32,
    },
}

fn default_plane_size() -> f32 {
    1.0
}
fn default_radius() -> f32 {
    0.5
}
fn default_height() -> f32 {
    1.0
}
fn default_sphere_segments() -> u32 {
    32
}
fn default_cylinder_segments() -> u32 {
    32
}
fn default_rings() -> u32 {
    16
}

impl PrimitiveSection {
    /// 生成图元网格。分段数低于图元生成器断言下限时提前报
    /// [`SceneError::OutOfRange`]（而非让引擎 panic）。
    ///
    /// 公开（R58a）：编辑器摆放图元时走同一份「段 → 网格」映射——
    /// 摆放产物与存档段一一对应，避免两处生成参数漂移。
    pub fn to_mesh_data(&self) -> Result<MeshData, SceneError> {
        match self {
            Self::Cube => Ok(MeshData::cube()),
            Self::Plane { size } => Ok(MeshData::plane(*size)),
            Self::Sphere {
                radius,
                segments,
                rings,
            } => {
                if *segments < 3 || *rings < 2 {
                    return Err(SceneError::OutOfRange(format!(
                        "球体分段数过低：segments = {segments}（≥3）、rings = {rings}（≥2）"
                    )));
                }
                Ok(MeshData::uv_sphere(*radius, *segments, *rings))
            }
            Self::Cylinder {
                radius,
                height,
                segments,
            } => {
                if *segments < 3 {
                    return Err(SceneError::OutOfRange(format!(
                        "圆柱分段数过低：segments = {segments}（≥3）"
                    )));
                }
                Ok(MeshData::cylinder(*radius, *height, *segments))
            }
        }
    }
}

/// 材质段：全缺省 = [`MaterialDef::default`]。`emissive` 为 RGB 三元组
/// （引擎侧 alpha 恒为 1.0）；三个纹理槽（`texture` 基色 /
/// `normal_texture` 法线 / `orm_texture` ORM 打包）各为纹理源两态
/// （[`TextureSourceSection`]：文件路径字符串或程序化生成器参数）；
/// `alpha_mode` 缺省 = `Opaque`（R23 之前的旧场景文件没有该字段，
/// 反序列化回退不透明——向后兼容）；`Blend` 走透明 pass（R23）。
///
/// 法线 / ORM 槽是 v2 新增（v1 只写基色路径，两槽恒 `None` = 中性纹理
/// 路径）——旧档直读零变化，旧档重存字节级不变（两槽 `None` 时省略）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct MaterialSection {
    pub color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub emissive: [f32; 3],
    pub texture: Option<TextureSourceSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normal_texture: Option<TextureSourceSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orm_texture: Option<TextureSourceSection>,
    /// 序列化省略不透明（旧场景文件往返字节级不变，见下方断言函数）。
    #[serde(skip_serializing_if = "alpha_mode_is_opaque")]
    pub alpha_mode: AlphaMode,
}

impl Default for MaterialSection {
    fn default() -> Self {
        Self {
            color: [1.0, 1.0, 1.0, 1.0],
            metallic: 0.0,
            roughness: 0.5,
            emissive: [0.0; 3],
            texture: None,
            normal_texture: None,
            orm_texture: None,
            alpha_mode: AlphaMode::Opaque,
        }
    }
}

/// `alpha_mode` 序列化省略条件：不透明（与引擎默认一致）。
/// 旧场景（无该字段）重存后文件保持不含该字段（往返字节级稳定）。
fn alpha_mode_is_opaque(mode: &AlphaMode) -> bool {
    matches!(mode, AlphaMode::Opaque)
}

impl MaterialSection {
    /// 转引擎材质定义：三个纹理槽传**已解析**的句柄（解析与去重在
    /// [`resolve_material`]，本函数是纯组装）。
    fn to_def(
        &self,
        texture: TextureHandle,
        normal_texture: Option<TextureHandle>,
        orm_texture: Option<TextureHandle>,
    ) -> MaterialDef {
        MaterialDef {
            base_color: self.color,
            emissive: [self.emissive[0], self.emissive[1], self.emissive[2], 1.0],
            metallic: self.metallic,
            roughness: self.roughness,
            texture,
            normal_texture,
            orm_texture,
            alpha_mode: self.alpha_mode,
        }
    }

    /// 从引擎材质 + 登记表还原材质段：数值字段照 `def` 现读，纹理来源照
    /// 登记表 `refs` 还原。**只在 `def` 实际持有该槽时写出来源**——代码侧
    /// 摘掉的槽不因登记残留而复现（登记表是来源记录，不是槽位真值）。
    fn from_def(def: &MaterialDef, refs: Option<&MaterialRef>) -> Self {
        let base = match refs {
            Some(r) if def.texture.0 != 0 => match (&r.texture, &r.base_source) {
                (Some(path), _) => Some(TextureSourceSection::File(path.clone())),
                (None, Some(procedural)) => Some(TextureSourceSection::Procedural(*procedural)),
                (None, None) => None,
            },
            _ => None,
        };
        let normal_texture = match refs {
            Some(r) if def.normal_texture.is_some() => r.normal_source.clone(),
            _ => None,
        };
        let orm_texture = match refs {
            Some(r) if def.orm_texture.is_some() => r.orm_source.clone(),
            _ => None,
        };
        Self {
            color: def.base_color,
            metallic: def.metallic,
            roughness: def.roughness,
            emissive: [def.emissive[0], def.emissive[1], def.emissive[2]],
            texture: base,
            normal_texture,
            orm_texture,
            alpha_mode: def.alpha_mode,
        }
    }
}

/// 点光段：各字段缺省 = [`PointLight::default`]（白光、强度 8、半径 12、无阴影）。
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(default)]
struct PointLightSection {
    color: [f32; 3],
    intensity: f32,
    range: f32,
    shadows: bool,
}

impl Default for PointLightSection {
    fn default() -> Self {
        let light = PointLight::default();
        Self {
            color: light.color.to_array(),
            intensity: light.intensity,
            range: light.range,
            shadows: light.shadows,
        }
    }
}

/// 粒子段（v2 新增）：[`ParticleEmitter`] 的**全部配置字段**，各字段
/// 缺省 = `ParticleEmitter::new(Alpha)`（60 粒/秒、1 秒寿命、竖直上抛
/// 无抖动、标准重力）。运行期状态（粒子池 / 累计余量 / 计数器）是私有
/// 字段也不入档——重载后发射器从新鲜状态开始（与 `RigidBody.velocity`
/// 不序列化同一哲学：关卡文件不冻结模拟中途状态）。
///
/// `max_particles` 以 u64 承载（wasm32 的 `usize` 是 32 位——超界报
/// [`SceneError::OutOfRange`] 而非静默截断）。
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(default)]
pub struct ParticleSection {
    pub blend: ParticleBlendSection,
    pub spawn_rate: f32,
    pub burst_count: u32,
    pub emitter_lifetime: f32,
    pub particle_lifetime: f32,
    pub initial_speed: f32,
    pub velocity_jitter: f32,
    pub gravity_scale: f32,
    pub size_start: f32,
    pub size_end: f32,
    pub color_start: [f32; 4],
    pub color_end: [f32; 4],
    pub max_particles: u64,
}

/// 混合模式段（[`ParticleBlend`] 无 serde derive——渲染层组件不带格式
/// 依赖，映射在本层；蛇形命名 `alpha` / `additive`）。
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ParticleBlendSection {
    #[default]
    Alpha,
    Additive,
}

impl Default for ParticleSection {
    fn default() -> Self {
        Self::from_emitter(&ParticleEmitter::new(ParticleBlend::Alpha))
    }
}

impl ParticleSection {
    fn to_emitter(self) -> Result<ParticleEmitter, SceneError> {
        let blend = match self.blend {
            ParticleBlendSection::Alpha => ParticleBlend::Alpha,
            ParticleBlendSection::Additive => ParticleBlend::Additive,
        };
        let max_particles = usize::try_from(self.max_particles).map_err(|_| {
            SceneError::OutOfRange(format!(
                "particles.max_particles = {} 超出本平台 usize 值域",
                self.max_particles
            ))
        })?;
        let mut emitter = ParticleEmitter::new(blend);
        emitter.spawn_rate = self.spawn_rate;
        emitter.burst_count = self.burst_count;
        emitter.emitter_lifetime = self.emitter_lifetime;
        emitter.particle_lifetime = self.particle_lifetime;
        emitter.initial_speed = self.initial_speed;
        emitter.velocity_jitter = self.velocity_jitter;
        emitter.gravity_scale = self.gravity_scale;
        emitter.size_start = self.size_start;
        emitter.size_end = self.size_end;
        emitter.color_start = self.color_start;
        emitter.color_end = self.color_end;
        emitter.max_particles = max_particles;
        Ok(emitter)
    }

    fn from_emitter(emitter: &ParticleEmitter) -> Self {
        Self {
            blend: match emitter.blend {
                ParticleBlend::Alpha => ParticleBlendSection::Alpha,
                ParticleBlend::Additive => ParticleBlendSection::Additive,
            },
            spawn_rate: emitter.spawn_rate,
            burst_count: emitter.burst_count,
            emitter_lifetime: emitter.emitter_lifetime,
            particle_lifetime: emitter.particle_lifetime,
            initial_speed: emitter.initial_speed,
            velocity_jitter: emitter.velocity_jitter,
            gravity_scale: emitter.gravity_scale,
            size_start: emitter.size_start,
            size_end: emitter.size_end,
            color_start: emitter.color_start,
            color_end: emitter.color_end,
            max_particles: emitter.max_particles as u64,
        }
    }
}

/// 碰撞体段：`shape` 必需；`static` / `restitution` 缺省为动态体 / 弹性 0
/// （与引擎动态构造器一致；「只有 Collider 无 RigidBody = 静态」的缺席语义
/// 由物理系统按组件缺席处理，无需文件层表达）。
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
struct ColliderSection {
    shape: ColliderShapeSection,
    /// Rust 关键字：serde rename 到 JSON 键 `static`。
    #[serde(rename = "static", default, skip_serializing_if = "std::ops::Not::not")]
    is_static: bool,
    #[serde(default)]
    restitution: f32,
}

/// 碰撞形状段：`{"sphere": {"radius": ..}}` / `{"box": {"half_extents": [..]}}`
/// / `{"capsule": {"radius": .., "half_height": ..}}`（外部标签形式）。
/// AABB 不随实体旋转、胶囊轴恒竖直（物理 v1 约定，见 iris-core::physics）。
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ColliderShapeSection {
    Sphere { radius: f32 },
    Box { half_extents: [f32; 3] },
    Capsule { radius: f32, half_height: f32 },
}

impl ColliderShapeSection {
    fn to_shape(self) -> ColliderShape {
        match self {
            Self::Sphere { radius } => ColliderShape::Sphere { radius },
            Self::Box { half_extents } => ColliderShape::Box {
                half_extents: half_extents.into(),
            },
            Self::Capsule {
                radius,
                half_height,
            } => ColliderShape::Capsule {
                radius,
                // 加载鲁棒性：半高 < 半径的退化文件按球（half_height =
                // radius 的胶囊）处理，不在加载路径 panic
                half_height: half_height.max(radius),
            },
        }
    }

    fn from_shape(shape: &ColliderShape) -> Self {
        match shape {
            ColliderShape::Sphere { radius } => Self::Sphere { radius: *radius },
            ColliderShape::Box { half_extents } => Self::Box {
                half_extents: half_extents.to_array(),
            },
            ColliderShape::Capsule {
                radius,
                half_height,
            } => Self::Capsule {
                radius: *radius,
                half_height: *half_height,
            },
        }
    }
}

/// 刚体段：缺省 = [`RigidBody::default`]（标准重力、无摩擦、无阻尼）。
/// `velocity` v1 不序列化——场景文件里的物体从静止开始
/// （把模拟中途的速度冻结进关卡文件不是想要的行为，见模块文档 v1 清单）。
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(default)]
struct RigidBodySection {
    gravity_scale: f32,
    friction: f32,
    linear_damping: f32,
}

impl Default for RigidBodySection {
    fn default() -> Self {
        let body = RigidBody::default();
        Self {
            gravity_scale: body.gravity_scale,
            friction: body.friction,
            linear_damping: body.linear_damping,
        }
    }
}

// ---------------------------------------------------------------------------
// load
// ---------------------------------------------------------------------------

/// 子资产解析策略（内部，`crate::level` 共用）：场景 JSON 本体之外的
/// 引用（glTF / 贴图 / HDR 天空 / 光照贴图产物）都经它解析。`Fs` =
/// [`load_scene`] 的既有 base_dir 文件系统语义；`Mount` =
/// [`load_scene_from`] 的挂载顺序查找。
pub(crate) enum SubAssets<'a> {
    Fs(PathBuf),
    Mount(&'a AssetMount),
}

impl<'a> SubAssets<'a> {
    /// 读子资产字节。
    pub(crate) fn read(&self, rel: &str) -> Result<Vec<u8>, SceneError> {
        match self {
            Self::Fs(dir) => std::fs::read(dir.join(rel)).map_err(SceneError::Io),
            Self::Mount(mount) => mount
                .read(rel)
                .map_err(|e| SceneError::Asset(format!("「{rel}」：{e}"))),
        }
    }

    /// glTF 段：路径入口（外部 uri 相对 base_dir 解析）或内存入口
    /// （须自包含 GLB / data URI——内存来源没有 base_dir，外部 uri
    /// 引用显式报错，见 `spawn_gltf_instance_from_bytes`）。
    fn spawn_gltf(&self, world: &mut World, rel: &str) -> Result<GltfInstance, SceneError> {
        match self {
            Self::Fs(dir) => spawn_gltf_instance(world, dir.join(rel)).map_err(SceneError::from),
            Self::Mount(_) => {
                let bytes = self.read(rel)?;
                spawn_gltf_instance_from_bytes(world, bytes).map_err(SceneError::from)
            }
        }
    }

    /// HDR 天空源。
    fn load_sky(&self, rel: &str) -> Result<HdrImage, SceneError> {
        match self {
            Self::Fs(dir) => load_hdr(dir.join(rel)).map_err(SceneError::from),
            Self::Mount(_) => {
                let bytes = self.read(rel)?;
                parse_hdr(&bytes).map_err(SceneError::from)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 同源去重（一次加载内）
// ---------------------------------------------------------------------------

/// 一次加载内的同源去重缓存（语义与动机见模块文档「同源去重（v2）」）。
///
/// 选型：**Vec 线性扫描而非 HashMap**——去重键含 f32（图元参数 / 材质
/// 数值，无 `Hash` 且 `Eq` 不成立），位模式元组比较是确定性文化的对照
/// 口径；规模 = 单文件内的来源数（手写关卡数十量级），线性扫描足够且
/// 首见序天然确定（BTree 类容器在此无额外收益，选型理由照
/// `broadphase.rs` 的规约写明：需要迭代序确定性时用有序结构，这里
/// 迭代序不参与语义、仅查找）。
#[derive(Default)]
pub(crate) struct LoadCache {
    /// 图元参数 → 网格句柄（glTF 不去重：整实例每次独立解析，v1 语义）。
    meshes: Vec<(PrimitiveSection, MeshHandle)>,
    /// 解析后的材质位模式 → 材质句柄。
    materials: Vec<(MaterialKey, MaterialHandle)>,
    /// (文件路径, srgb) → 纹理句柄（同一文件用于基色与法线槽 = 两个
    /// 条目——sRGB 语义按槽位，数据虽同、上传解释不同）。
    file_textures: Vec<(String, bool, TextureHandle)>,
    /// 程序化源 → 生成产物 + 逐槽已登记句柄（同参数只生成一次）。
    procedural: Vec<(ProceduralTextureSection, ProcCache)>,
}

/// 程序化源的生成产物与逐槽登记状态（槽位按需登记，未引用的槽不进
/// `TextureStore`——句柄布局紧凑且由文件序唯一决定）。
struct ProcCache {
    grass: Option<GrassTextures>,
    brick: Option<(CpuTexture, CpuTexture)>,
    base: Option<TextureHandle>,
    normal: Option<TextureHandle>,
    orm: Option<TextureHandle>,
}

/// 材质去重键：解析后 [`MaterialDef`] 的位模式（f32 逐位、句柄与
/// `AlphaMode` 本身 `Eq`）。
type MaterialKey = (
    [u32; 4],
    [u32; 4],
    u32,
    u32,
    TextureHandle,
    Option<TextureHandle>,
    Option<TextureHandle>,
    AlphaMode,
);

fn material_key(def: &MaterialDef) -> MaterialKey {
    (
        def.base_color.map(f32::to_bits),
        def.emissive.map(f32::to_bits),
        def.metallic.to_bits(),
        def.roughness.to_bits(),
        def.texture,
        def.normal_texture,
        def.orm_texture,
        def.alpha_mode,
    )
}

impl LoadCache {
    /// 图元网格（去重）：首见生成 + 登记来源，后见复用同句柄。
    fn primitive_mesh(
        &mut self,
        world: &mut World,
        primitive: &PrimitiveSection,
    ) -> Result<MeshHandle, SceneError> {
        if let Some((_, handle)) = self.meshes.iter().find(|(section, _)| section == primitive) {
            return Ok(*handle);
        }
        let data = primitive.to_mesh_data()?;
        let handle = world.resource_mut::<MeshStore>().add(data);
        world
            .resource_mut::<SceneAssetRegistry>()
            .meshes
            .insert(handle.0, MeshRef::Primitive(primitive.clone()));
        self.meshes.push((primitive.clone(), handle));
        Ok(handle)
    }

    /// 文件纹理（去重，键含槽位 sRGB 语义）。
    fn file_texture(
        &mut self,
        world: &mut World,
        sub: &SubAssets<'_>,
        path: &str,
        slot: TextureSlot,
    ) -> Result<TextureHandle, SceneError> {
        let srgb = slot == TextureSlot::Base;
        if let Some((_, _, handle)) = self
            .file_textures
            .iter()
            .find(|(p, s, _)| p == path && *s == srgb)
        {
            return Ok(*handle);
        }
        let handle = load_texture(world, sub, path, srgb)?;
        self.file_textures.push((path.to_string(), srgb, handle));
        Ok(handle)
    }

    /// 程序化纹理（同参数只生成一次；逐槽按需登记）。`brick_normal_orm`
    /// 无基色产物——用于基色槽报 [`SceneError::Unsupported`]。
    fn procedural_texture(
        &mut self,
        world: &mut World,
        source: &ProceduralTextureSection,
        slot: TextureSlot,
    ) -> Result<TextureHandle, SceneError> {
        // 生成器断言前置校验（加载路径不 panic）
        match source {
            ProceduralTextureSection::Grass { size } => {
                if *size < 8 {
                    return Err(SceneError::OutOfRange(format!(
                        "grass.size 至少 8 像素（草地纹理生成器下限），实际 {size}"
                    )));
                }
            }
            ProceduralTextureSection::BrickNormalOrm {
                rows, cols, size, ..
            } => {
                if *rows < 1 || *cols < 1 {
                    return Err(SceneError::OutOfRange(format!(
                        "brick_normal_orm 行列数至少 1，实际 {rows}×{cols}"
                    )));
                }
                if *size < 4 {
                    return Err(SceneError::OutOfRange(format!(
                        "brick_normal_orm.size 至少 4 像素（砖墙纹理生成器下限），实际 {size}"
                    )));
                }
            }
        }
        if !self.procedural.iter().any(|(s, _)| s == source) {
            let cache = match *source {
                ProceduralTextureSection::Grass { size } => ProcCache {
                    grass: Some(grass_textures(size)),
                    brick: None,
                    base: None,
                    normal: None,
                    orm: None,
                },
                ProceduralTextureSection::BrickNormalOrm {
                    rows,
                    cols,
                    size,
                    seam,
                    bump,
                } => ProcCache {
                    grass: None,
                    brick: Some(CpuTexture::brick_normal_orm(rows, cols, size, seam, bump)),
                    base: None,
                    normal: None,
                    orm: None,
                },
            };
            self.procedural.push((*source, cache));
        }
        let (_, cache) = self
            .procedural
            .iter_mut()
            .find(|(s, _)| s == source)
            .expect("上方刚插入");
        let slot_handle = match slot {
            TextureSlot::Base => &mut cache.base,
            TextureSlot::Normal => &mut cache.normal,
            TextureSlot::Orm => &mut cache.orm,
        };
        if let Some(handle) = *slot_handle {
            return Ok(handle);
        }
        let texture = match (slot, &cache.grass, &cache.brick) {
            (TextureSlot::Base, Some(grass), _) => grass.albedo.clone(),
            (TextureSlot::Normal, Some(grass), _) => grass.normal.clone(),
            (TextureSlot::Orm, Some(grass), _) => grass.orm.clone(),
            (TextureSlot::Normal, _, Some(brick)) => brick.0.clone(),
            (TextureSlot::Orm, _, Some(brick)) => brick.1.clone(),
            (TextureSlot::Base, None, Some(_)) => {
                return Err(SceneError::Unsupported(
                    "brick_normal_orm 程序化源产出法线 + ORM 两张纹理，不能用于基色槽\
                     （基色用 grass 源或文件路径）"
                        .into(),
                ));
            }
            // 生成产物与源一一对应（上方构造保证），此支不可达
            (_, None, None) => unreachable!("程序化缓存必持有其源的生成产物"),
        };
        let handle = world.resource_mut::<TextureStore>().add(texture);
        *slot_handle = Some(handle);
        Ok(handle)
    }
}

/// 单槽纹理来源解析（文件 / 程序化 / 缺席三态）。
fn resolve_texture_source(
    world: &mut World,
    sub: &SubAssets<'_>,
    cache: &mut LoadCache,
    source: &Option<TextureSourceSection>,
    slot: TextureSlot,
) -> Result<Option<TextureHandle>, SceneError> {
    match source {
        Some(TextureSourceSection::File(path)) => {
            Ok(Some(cache.file_texture(world, sub, path, slot)?))
        }
        Some(TextureSourceSection::Procedural(procedural)) => {
            Ok(Some(cache.procedural_texture(world, procedural, slot)?))
        }
        None => Ok(None),
    }
}

/// 解析材质段（实体段与 `level.terrain` 共用）：三槽纹理逐一解析（去重）
/// → 组装 [`MaterialDef`] → 材质级去重 → 首见入 `MaterialStore` 并登记
/// 来源（[`MaterialRef`]），后见复用同句柄。
pub(crate) fn resolve_material(
    world: &mut World,
    sub: &SubAssets<'_>,
    section: &MaterialSection,
    cache: &mut LoadCache,
) -> Result<MaterialHandle, SceneError> {
    let base = resolve_texture_source(world, sub, cache, &section.texture, TextureSlot::Base)?;
    let normal = resolve_texture_source(
        world,
        sub,
        cache,
        &section.normal_texture,
        TextureSlot::Normal,
    )?;
    let orm = resolve_texture_source(world, sub, cache, &section.orm_texture, TextureSlot::Orm)?;
    let def = section.to_def(base.unwrap_or(TextureHandle(0)), normal, orm);
    let key = material_key(&def);
    if let Some((_, handle)) = cache.materials.iter().find(|(k, _)| *k == key) {
        return Ok(*handle);
    }
    let handle = world.resource_mut::<MaterialStore>().add(def);
    world.resource_mut::<SceneAssetRegistry>().materials.insert(
        handle.0,
        MaterialRef {
            texture: match &section.texture {
                Some(TextureSourceSection::File(path)) => Some(path.clone()),
                _ => None,
            },
            base_source: match &section.texture {
                Some(TextureSourceSection::Procedural(procedural)) => Some(*procedural),
                _ => None,
            },
            normal_source: section.normal_texture.clone(),
            orm_source: section.orm_texture.clone(),
        },
    );
    cache.materials.push((key, handle));
    Ok(handle)
}

/// 从 JSON 场景文件加载实体：插入缺失的资源兜底（`MeshStore` /
/// `MaterialStore` / `TextureStore` / [`SceneAssetRegistry`]，已存在不覆盖，
/// 照 `RenderPlugin` 的资源模式）→ 写环境段资源（缺席不改）→ 逐实体 spawn。
///
/// 返回按文件内顺序生成的实体列表。相对路径（`gltf` / `texture` /
/// `sky_source`）一律相对**场景文件所在目录**解析；登记表记原始字符串，
/// save 时原样写出。
///
/// # 平台注意
/// 文件读取走 `std::fs`：native 可用，web（wasm32）上不可用（见模块文档）。
/// 挂载来源（pack / 嵌入）请用 [`load_scene_from`]。
pub fn load_scene(world: &mut World, path: impl AsRef<Path>) -> Result<Vec<Entity>, SceneError> {
    let path = path.as_ref();
    let bytes = std::fs::read(path)?;
    load_scene_bytes(
        world,
        &bytes,
        SubAssets::Fs(path.parent().unwrap_or(Path::new(".")).to_path_buf()),
    )
}

/// 挂载变体（R45）：场景 JSON 与其引用的全部子资产（glTF / 贴图 /
/// HDR 天空）都经 [`AssetMount`] 顺序查找读取——pack 单文件或嵌入文本
/// 的场景加载入口。[`load_scene`]（文件路径语义）保持零变化，两者共用
/// 同一加载主体，实体生成逻辑逐位一致。
///
/// 相对路径按**挂载内的路径约定**解析（pack 内即打包时写入的路径，
/// 无 base_dir 概念）；glTF 须自包含（GLB 容器或 data URI——内存来源
/// 不支持外部 uri 引用，显式报 [`SceneError`]）。
///
/// # 平台注意
/// 配合 `Embedded` 源可在 wasm32 上加载场景（文件系统语义在 web 不可用，
/// 这是该平台下场景加载的替代通路）。
pub fn load_scene_from(
    world: &mut World,
    mount: &AssetMount,
    path: impl AsRef<str>,
) -> Result<Vec<Entity>, SceneError> {
    let path = path.as_ref();
    let bytes = mount
        .read(path)
        .map_err(|e| SceneError::Asset(format!("「{path}」：{e}")))?;
    load_scene_bytes(world, &bytes, SubAssets::Mount(mount))
}

/// 两入口共用的加载主体：解析 JSON → 版本迁移 → 兜底资源 → 环境 →
/// `level.terrain` → 逐实体 spawn → `level.nav` → `level.bake` →
/// [`LevelRegistry`] 登记。
///
/// 应用顺序的设计依据：terrain 先于实体段（「地基先于摆设」——地形
/// 派生实体占据实体序低段，编辑器心智模型）；nav / bake 后于实体段
/// （两者消费世界内静态碰撞体 / 几何的**全集**，实体段与 terrain 都
/// 就位后才有完整输入）。
fn load_scene_bytes(
    world: &mut World,
    bytes: &[u8],
    sub: SubAssets<'_>,
) -> Result<Vec<Entity>, SceneError> {
    let mut doc: SceneDoc = serde_json::from_slice(bytes)?;
    migrate_doc(&mut doc)?;
    let defs = doc
        .entities
        .ok_or_else(|| SceneError::Missing("entities".into()))?;

    ensure_resources(world);
    if let Some(environment) = doc.environment {
        apply_environment(world, &sub, environment)?;
    }

    let level = doc.level.unwrap_or_default();
    let mut cache = LoadCache::default();
    if let Some(terrain) = &level.terrain {
        apply_terrain(world, &sub, terrain, &mut cache)?;
    }

    let mut spawned = Vec::with_capacity(defs.len());
    for section in defs {
        spawned.push(spawn_section(world, &sub, section, &mut cache)?);
    }

    if let Some(nav) = &level.nav {
        apply_nav(world, nav)?;
    }
    if let Some(bake) = &level.bake {
        apply_bake(world, &sub, bake)?;
    }
    // 登记加载时快照：save 回写 version / level 段的依据（语义见
    // crate::level 模块文档「诚实清单」）
    world.insert_resource(LevelRegistry { section: level });
    Ok(spawned)
}

/// 插入场景加载所需的缺失资源（已存在不覆盖——不清掉用户代码注册的内容）。
fn ensure_resources(world: &mut World) {
    if !world.contains_resource::<MeshStore>() {
        world.insert_resource(MeshStore::new());
    }
    if !world.contains_resource::<MaterialStore>() {
        world.insert_resource(MaterialStore::new());
    }
    if !world.contains_resource::<TextureStore>() {
        world.insert_resource(TextureStore::new());
    }
    if !world.contains_resource::<SceneAssetRegistry>() {
        world.insert_resource(SceneAssetRegistry::default());
    }
}

/// 应用环境段：方向光与天空源均为「存在才写」——缺席时不动既有资源。
fn apply_environment(
    world: &mut World,
    sub: &SubAssets<'_>,
    environment: EnvironmentSection,
) -> Result<(), SceneError> {
    if let Some(light) = environment.directional_light {
        world.insert_resource(light.to_light());
    }
    if let Some(rel) = environment.sky_source {
        let image = sub.load_sky(&rel)?;
        world.insert_resource(SkySource::equirect_hdr(image));
        // 登记原始路径字符串：save 时原样写出（roundtrip 稳定）
        world.resource_mut::<SceneAssetRegistry>().sky_source = Some(rel);
    }
    Ok(())
}

/// 生成单个实体：Transform 先行 spawn，其余组件按段逐一插入。
fn spawn_section(
    world: &mut World,
    sub: &SubAssets<'_>,
    section: EntitySection,
    cache: &mut LoadCache,
) -> Result<Entity, SceneError> {
    let transform = section
        .transform
        .map(|t| t.to_transform())
        .unwrap_or(Transform::IDENTITY);
    let entity = world.spawn((transform,));

    if let Some(name) = section.name {
        world.insert(entity, Name(name));
    }
    // visible 缺省 / true 均为可见（不插组件，与 Visibility 缺席语义一致）
    if section.visible == Some(false) {
        world.insert(entity, Visibility::HIDDEN);
    }

    if let Some(mesh) = &section.mesh {
        match mesh {
            MeshSection::Primitive(primitive) => {
                let handle = cache.primitive_mesh(world, primitive)?;
                world.insert(entity, handle);
                // 渲染器收集查询要求 (MeshHandle, MaterialHandle) 成对：
                // 无材质段时挂默认材质（0 号槽位 = 纯白，引擎既有兜底），
                // 该句柄不入登记表（save 时整段省略，roundtrip 稳定）。
                if section.material.is_none() {
                    world.insert(entity, MaterialHandle(0));
                }
            }
            MeshSection::Gltf(rel) => {
                // glTF 整实例挂载：实例顶层实体作为本实体的子实体，
                // 本实体的 transform 作用于整棵子树。
                let instance = sub.spawn_gltf(world, rel)?;
                for root in instance.roots {
                    mark_gltf_tree(world, root);
                    world.add_child(entity, root);
                }
                world
                    .resource_mut::<SceneAssetRegistry>()
                    .gltfs
                    .insert(entity, rel.clone());
            }
        }
    }

    if let Some(material) = &section.material {
        let handle = resolve_material(world, sub, material, cache)?;
        world.insert(entity, handle);
    }

    if let Some(light) = section.point_light {
        world.insert(
            entity,
            PointLight {
                color: light.color.into(),
                intensity: light.intensity,
                range: light.range,
                shadows: light.shadows,
            },
        );
    }

    if let Some(particles) = &section.particles {
        world.insert(entity, particles.to_emitter()?);
    }

    if let Some(collider) = section.collider {
        world.insert(
            entity,
            Collider {
                shape: collider.shape.to_shape(),
                is_static: collider.is_static,
                restitution: collider.restitution,
                is_sensor: false,
                groups: u32::MAX,
                mask: u32::MAX,
            },
        );
    }

    if let Some(body) = section.rigidbody {
        // velocity / 角速度 v1 不序列化：物体从静止、不旋转开始（模块文档 v1 清单）
        world.insert(
            entity,
            RigidBody {
                velocity: Vec3::ZERO,
                gravity_scale: body.gravity_scale,
                friction: body.friction,
                linear_damping: body.linear_damping,
                angular_velocity: Vec3::ZERO,
                angular_damping: 0.0,
                body_type: BodyType::Dynamic,
            },
        );
    }

    Ok(entity)
}

/// 递归给 glTF 实例生成的整棵实体树打标记（save 时跳过）。
fn mark_gltf_tree(world: &mut World, entity: Entity) {
    world.insert(entity, GltfMounted);
    for child in world.children(entity) {
        mark_gltf_tree(world, child);
    }
}

/// 读取并解码贴图文件（PNG / JPEG，复用依赖树内的 image crate）。
/// `srgb` 按材质槽位：基色 = 颜色数据（sRGB 上传），法线 / ORM =
/// 线性数据（原样上传）——数据字节相同、GPU 采样解释不同。
fn load_texture(
    world: &mut World,
    sub: &SubAssets<'_>,
    rel: &str,
    srgb: bool,
) -> Result<TextureHandle, SceneError> {
    let bytes = sub.read(rel)?;
    let decoded = image::load_from_memory(&bytes)
        .map_err(|e| SceneError::Unsupported(format!("贴图解码失败（支持 PNG / JPEG）：{e}")))?;
    let rgba = decoded.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(world.resource_mut::<TextureStore>().add(CpuTexture {
        width,
        height,
        rgba: rgba.into_raw(),
        srgb,
    }))
}

// ---------------------------------------------------------------------------
// save
// ---------------------------------------------------------------------------

/// 把世界的内置组件集保存为 JSON 场景 / 关卡文件（格式 v2），返回写出的
/// 实体数。
///
/// - 查询 `(Transform + 可选 Name / Visibility / MeshHandle / MaterialHandle /
///   PointLight / ParticleEmitter / Collider / RigidBody)` 全量写出；带
///   [`GltfMounted`] 标记的 glTF 内部实体与带
///   [`LevelGenerated`](crate::level::LevelGenerated) 标记的 terrain 派生
///   实体跳过；无 `Transform` 的实体（如 UI）不参与往返；
/// - mesh / material / 天空源的来源按 [`SceneAssetRegistry`] 登记还原；
///   非场景来源的句柄按模块文档的降级规则省略并 `log::warn`；
/// - `version` 恒写当前版本（[`SCENE_FORMAT_VERSION`]）；`level` 段按
///   [`LevelRegistry`] 的加载时快照原样回写（空段省略——v1 档重存只多
///   `version` 字段）；
/// - 实体顺序 = 组件存储序（= spawn 顺序），稳定；`save(load(f))` 再
///   `save` 一次产出逐字节相同的文件（同输入确定性输出——字段序 =
///   结构体声明序，浮点 = serde_json 最短往返表示，位保真由
///   `serde_json_f32_roundtrip_is_bitwise_exact` 测试钉住）。
///
/// # 平台注意
/// 文件写入走 `std::fs`：native 可用，web（wasm32）上不可用。
pub fn save_scene(world: &mut World, path: impl AsRef<Path>) -> Result<usize, SceneError> {
    let world = &*world;
    let registry_guard = world.try_resource::<SceneAssetRegistry>();
    let registry_empty = SceneAssetRegistry::default();
    let registry: &SceneAssetRegistry = registry_guard.as_deref().unwrap_or(&registry_empty);
    let materials_guard = world.try_resource::<MaterialStore>();

    let mut sections: Vec<EntitySection> = Vec::new();
    let mut foreign_meshes = 0usize;
    let mut lost_textures = 0usize;
    world.query::<(Entity, &Transform)>(|(entity, transform)| {
        // glTF 内部实体由 mesh 段整体重建，不属于场景文件
        if world.get::<GltfMounted>(entity).is_some() {
            return;
        }
        // terrain 派生实体由 level.terrain 段整体重建，同理跳过
        if world.get::<crate::level::LevelGenerated>(entity).is_some() {
            return;
        }
        sections.push(build_entity_section(
            world,
            registry,
            materials_guard.as_deref(),
            entity,
            transform,
            &mut foreign_meshes,
            &mut lost_textures,
        ));
    });
    if foreign_meshes > 0 {
        log::warn!(
            "save_scene：{foreign_meshes} 个实体的网格来自非场景来源（用户代码注册），\
             mesh 段已省略——重新加载后这些实体没有网格"
        );
    }
    if lost_textures > 0 {
        log::warn!(
            "save_scene：{lost_textures} 个材质的纹理（基色 / 法线 / ORM）无来源登记 \
             （TextureStore 无路径 / 生成参数概念，非场景加载的纹理无法回写），\
             对应字段已省略"
        );
    }

    // 环境段：方向光资源存在则写；天空源为外部 HDR 且登记了路径才写
    let mut environment = EnvironmentSection::default();
    if let Some(light) = world.try_resource::<DirectionalLight>() {
        environment.directional_light = Some(DirectionalLightSection::from_light(&light));
    }
    if let Some(sky) = world.try_resource::<SkySource>() {
        if matches!(sky.kind, SkySourceKind::EquirectHdr(_)) {
            match &registry.sky_source {
                Some(rel) => environment.sky_source = Some(rel.clone()),
                None => log::warn!(
                    "save_scene：SkySource 为外部 HDR 但登记表无来源路径\
                     （非场景加载来源，HDR 内容不回写文件），sky_source 已省略"
                ),
            }
        }
    }
    let has_environment =
        environment.directional_light.is_some() || environment.sky_source.is_some();

    // level 段：加载时快照原样回写（空段省略）
    let level = world
        .try_resource::<LevelRegistry>()
        .map(|registry| registry.section.clone())
        .filter(|section| !section.is_empty());

    let count = sections.len();
    let doc = SceneDoc {
        version: Some(SCENE_FORMAT_VERSION),
        environment: has_environment.then_some(environment),
        level,
        entities: Some(sections),
    };
    let json = serde_json::to_string_pretty(&doc)?;
    std::fs::write(path.as_ref(), json)?;
    Ok(count)
}

/// 从单个实体构建场景段（登记表驱动的 mesh / material 来源还原在此）。
#[allow(clippy::too_many_arguments)]
fn build_entity_section(
    world: &World,
    registry: &SceneAssetRegistry,
    materials: Option<&MaterialStore>,
    entity: Entity,
    transform: &Transform,
    foreign_meshes: &mut usize,
    lost_textures: &mut usize,
) -> EntitySection {
    let mut section = EntitySection::default();

    if let Some(name) = world.get::<Name>(entity) {
        section.name = Some(name.0.clone());
    }
    if let Some(visibility) = world.get::<Visibility>(entity) {
        if !visibility.visible {
            section.visible = Some(false);
        }
    }
    section.transform = TransformSection::from_transform(transform);

    // mesh：glTF 挂载优先（该实体本身无 MeshHandle，来源记在登记表）
    if let Some(gltf_path) = registry.gltfs.get(&entity) {
        section.mesh = Some(MeshSection::Gltf(gltf_path.clone()));
    } else if let Some(handle) = world.get::<MeshHandle>(entity) {
        match registry.meshes.get(&handle.0) {
            Some(MeshRef::Primitive(primitive)) => {
                section.mesh = Some(MeshSection::Primitive(primitive.clone()));
            }
            // 非场景来源（用户代码 MeshStore::add）：省略 + 统一告警
            Some(MeshRef::Gltf(_)) | None => *foreign_meshes += 1,
        }
    }

    // material：句柄 0 = 默认材质（隐式，整段省略）；登记表还原三槽纹理
    // 来源，数值字段照 MaterialDef 现读（保存「当前值」而非「加载时的值」）
    if let Some(handle) = world.get::<MaterialHandle>(entity) {
        if handle.0 != 0 {
            if let Some(def) = materials.and_then(|m| m.get(*handle)) {
                let refs = registry.materials.get(&handle.0);
                let base_lost = def.texture.0 != 0
                    && refs.is_none_or(|r| r.texture.is_none() && r.base_source.is_none());
                let normal_lost =
                    def.normal_texture.is_some() && refs.is_none_or(|r| r.normal_source.is_none());
                let orm_lost =
                    def.orm_texture.is_some() && refs.is_none_or(|r| r.orm_source.is_none());
                if base_lost || normal_lost || orm_lost {
                    *lost_textures += 1;
                }
                section.material = Some(MaterialSection::from_def(def, refs));
            }
        }
    }

    if let Some(light) = world.get::<PointLight>(entity) {
        section.point_light = Some(PointLightSection {
            color: light.color.to_array(),
            intensity: light.intensity,
            range: light.range,
            shadows: light.shadows,
        });
    }

    // particles：只写配置字段（运行期状态是私有字段也不入档——重载后
    // 发射器从新鲜状态开始，模块文档「诚实清单」）
    if let Some(emitter) = world.get::<ParticleEmitter>(entity) {
        section.particles = Some(ParticleSection::from_emitter(&emitter));
    }

    if let Some(collider) = world.get::<Collider>(entity) {
        section.collider = Some(ColliderSection {
            shape: ColliderShapeSection::from_shape(&collider.shape),
            is_static: collider.is_static,
            restitution: collider.restitution,
        });
    }

    if let Some(body) = world.get::<RigidBody>(entity) {
        section.rigidbody = Some(RigidBodySection {
            gravity_scale: body.gravity_scale,
            friction: body.friction,
            linear_damping: body.linear_damping,
        });
    }

    section
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core::physics::ColliderShape;
    use iris_render::asset::{HdrImage, encode_hdr, write_glb};
    use std::path::PathBuf;

    /// 测试固定目录（与 gltf.rs / hdr.rs 测试同款约定）。
    fn test_dir() -> PathBuf {
        let dir = std::env::temp_dir().join("iris-scene-test");
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        dir
    }

    /// 全资源兜底的最小世界（load_scene 自带兜底，无需预插）。
    fn fresh_world() -> World {
        World::new()
    }

    /// 写入场景文件并加载，返回（实体列表, 文件路径）。
    fn load_str(world: &mut World, name: &str, json: &str) -> (Vec<Entity>, PathBuf) {
        let path = test_dir().join(name);
        std::fs::write(&path, json).expect("写场景文件");
        let entities = load_scene(world, &path).expect("加载场景");
        (entities, path)
    }

    /// 内嵌三角形 .gltf fixture（data URI 自包含，与 gltf.rs 测试同款构造）。
    fn triangle_gltf() -> String {
        let mut buffer: Vec<u8> = Vec::new();
        for p in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            buffer.extend_from_slice(&p.map(f32::to_le_bytes).concat());
        }
        buffer.extend_from_slice(&[0u16, 1, 2].map(u16::to_le_bytes).concat());
        format!(
            r#"{{
                "asset": {{ "version": "2.0" }},
                "buffers": [{{ "uri": "data:application/octet-stream;base64,{}", "byteLength": {} }}],
                "bufferViews": [
                    {{ "buffer": 0, "byteOffset": 0, "byteLength": 36 }},
                    {{ "buffer": 0, "byteOffset": 36, "byteLength": 6 }}
                ],
                "accessors": [
                    {{ "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3" }},
                    {{ "bufferView": 1, "componentType": 5123, "count": 3, "type": "SCALAR" }}
                ],
                "meshes": [{{ "primitives": [{{ "attributes": {{ "POSITION": 0 }}, "indices": 1 }}] }}],
                "nodes": [{{ "mesh": 0 }}],
                "scenes": [{{ "nodes": [0] }}],
                "scene": 0
            }}"#,
            iris_render::asset::base64::encode(&buffer),
            buffer.len()
        )
    }

    // ---- 解析 ----

    /// 最小场景：`{"entities": [{}]}` → 1 实体、恒等变换、无其他组件。
    #[test]
    fn minimal_scene_spawns_single_identity_entity() {
        let mut world = fresh_world();
        let (entities, _) = load_str(&mut world, "min.scene.json", r#"{"entities": [{}]}"#);
        assert_eq!(entities.len(), 1);
        let entity = entities[0];
        assert_eq!(
            *world.get::<Transform>(entity).unwrap(),
            Transform::IDENTITY
        );
        assert!(world.get::<Name>(entity).is_none());
        assert!(world.get::<MeshHandle>(entity).is_none());
        assert!(world.get::<MaterialHandle>(entity).is_none());
        assert!(world.get::<PointLight>(entity).is_none());
        assert!(world.get::<Collider>(entity).is_none());
        assert!(world.get::<RigidBody>(entity).is_none());
        assert!(
            world.get::<Visibility>(entity).is_none(),
            "缺席 = 可见，不插组件"
        );
    }

    /// 字段缺省值与引擎默认一致：部分段只写个别字段，其余落引擎默认。
    #[test]
    fn field_defaults_match_engine() {
        let mut world = fresh_world();
        let (entities, _) = load_str(
            &mut world,
            "defaults.scene.json",
            r#"{
                "entities": [{
                    "transform": { "position": [1.0, 2.0, 3.0] },
                    "material": { "color": [1.0, 1.0, 1.0, 1.0] },
                    "point_light": { "intensity": 8.0 },
                    "collider": { "shape": { "sphere": { "radius": 0.5 } } },
                    "rigidbody": { "gravity_scale": 1.0 }
                }]
            }"#,
        );
        let entity = entities[0];
        let transform = *world.get::<Transform>(entity).unwrap();
        assert_eq!(transform.position, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(transform.rotation, Quat::IDENTITY, "rotation 缺省 = 恒等");
        assert_eq!(transform.scale, Vec3::ONE);

        let light = *world.get::<PointLight>(entity).unwrap();
        assert_eq!(light.color, PointLight::default().color, "点光缺省色 = 白");
        assert_eq!(light.intensity, PointLight::default().intensity);
        assert_eq!(light.range, PointLight::default().range);
        assert!(!light.shadows);

        let collider = *world.get::<Collider>(entity).unwrap();
        assert_eq!(
            collider,
            Collider {
                shape: ColliderShape::Sphere { radius: 0.5 },
                is_static: false,
                restitution: 0.0,
                is_sensor: false,
                groups: u32::MAX,
                mask: u32::MAX,
            },
            "collider 缺省 = 动态球、弹性 0（引擎动态构造器语义）"
        );

        let body = *world.get::<RigidBody>(entity).unwrap();
        assert_eq!(
            body,
            RigidBody::default(),
            "rigidbody 缺省 = RigidBody::default"
        );
    }

    /// 未知字段容错：serde 默认忽略未知字段（前向兼容），任意层级均可。
    #[test]
    fn unknown_fields_are_tolerated() {
        let mut world = fresh_world();
        let (entities, _) = load_str(
            &mut world,
            "unknown.scene.json",
            r#"{
                "format": "iris-scene-v1",
                "environment": { "fog": true },
                "entities": [{
                    "banana": 42,
                    "transform": { "position": [0.0, 1.0, 0.0], "tilt": 3.0 },
                    "mesh": { "primitive": { "kind": "cube", "extra": true } }
                }]
            }"#,
        );
        assert_eq!(entities.len(), 1);
        assert_eq!(
            world.get::<Transform>(entities[0]).unwrap().position,
            Vec3::new(0.0, 1.0, 0.0),
            "已知字段照常生效"
        );
        assert!(world.get::<MeshHandle>(entities[0]).is_some());
    }

    /// 坏 JSON → Json；缺 entities → Missing；文件不存在 → Io。
    #[test]
    fn broken_json_reports_json_error() {
        let mut world = fresh_world();
        let path = test_dir().join("broken.scene.json");
        std::fs::write(&path, "{ entities: [").unwrap();
        let err = load_scene(&mut world, &path).unwrap_err();
        assert!(matches!(err, SceneError::Json(_)), "实际错误：{err:?}");
    }

    #[test]
    fn missing_entities_reports_missing_error() {
        let mut world = fresh_world();
        let err_path = test_dir().join("no_entities.scene.json");
        std::fs::write(&err_path, r#"{ "environment": {} }"#).unwrap();
        let err = load_scene(&mut world, &err_path).unwrap_err();
        assert!(matches!(err, SceneError::Missing(_)), "实际错误：{err:?}");
        assert!(
            err.to_string().contains("entities"),
            "错误信息应指名缺失字段：{err}"
        );
    }

    #[test]
    fn missing_file_reports_io_error() {
        let mut world = fresh_world();
        let err = load_scene(&mut world, test_dir().join("不存在.scene.json")).unwrap_err();
        assert!(matches!(err, SceneError::Io(_)), "实际错误：{err:?}");
    }

    // ---- spawn 正确性 ----

    /// 完整实体段逐字段进组件：name / visible / transform / material /
    /// point_light / collider / rigidbody。
    #[test]
    fn sections_map_to_engine_components() {
        let mut world = fresh_world();
        let (entities, _) = load_str(
            &mut world,
            "full.scene.json",
            r#"{
                "entities": [{
                    "name": "coin-01",
                    "visible": false,
                    "transform": {
                        "position": [1.0, 5.0, -2.0],
                        "rotation": [0.0, 0.3826834, 0.0, 0.9238795],
                        "scale": [2.0, 2.0, 2.0]
                    },
                    "mesh": { "primitive": { "kind": "sphere", "radius": 0.4, "segments": 12, "rings": 8 } },
                    "material": {
                        "color": [0.8, 0.2, 0.1, 1.0],
                        "metallic": 1.0,
                        "roughness": 0.25,
                        "emissive": [0.5, 0.1, 0.0]
                    },
                    "point_light": { "color": [1.0, 0.5, 0.2], "intensity": 20.0, "range": 30.0, "shadows": true },
                    "collider": { "shape": { "box": { "half_extents": [1.0, 2.0, 3.0] } }, "static": true, "restitution": 0.2 },
                    "rigidbody": { "gravity_scale": 0.5, "friction": 0.3, "linear_damping": 0.1 }
                }]
            }"#,
        );
        let entity = entities[0];

        assert_eq!(world.get::<Name>(entity).unwrap().0, "coin-01");
        assert_eq!(
            *world.get::<Visibility>(entity).unwrap(),
            Visibility::HIDDEN,
            "visible: false → Visibility::HIDDEN"
        );

        let transform = *world.get::<Transform>(entity).unwrap();
        assert_eq!(transform.position, Vec3::new(1.0, 5.0, -2.0));
        assert_eq!(transform.scale, Vec3::splat(2.0));
        let expected_yaw = Quat::from_rotation_y(std::f32::consts::FRAC_PI_4);
        assert!(
            (transform.rotation.dot(expected_yaw) - 1.0).abs() < 1e-5,
            "四元数 (x,y,z,w) 应还原 yaw 45°，实际 {:?}",
            transform.rotation
        );

        let handle = *world.get::<MeshHandle>(entity).unwrap();
        let meshes = world.resource::<MeshStore>();
        let sphere = meshes.get(handle).expect("网格应已注册");
        assert_eq!(
            sphere.vertices.len(),
            (12 + 1) * (8 + 1),
            "球体分段数应按段生成"
        );
        assert!(sphere.vertices.iter().all(|v| {
            let len = (v.position[0] * v.position[0]
                + v.position[1] * v.position[1]
                + v.position[2] * v.position[2])
                .sqrt();
            (len - 0.4).abs() < 1e-4
        }));

        let material = world
            .resource::<MaterialStore>()
            .get(*world.get::<MaterialHandle>(entity).unwrap())
            .expect("材质应已注册")
            .clone();
        assert_eq!(material.base_color, [0.8, 0.2, 0.1, 1.0]);
        assert_eq!(material.metallic, 1.0);
        assert_eq!(material.roughness, 0.25);
        assert_eq!(
            material.emissive,
            [0.5, 0.1, 0.0, 1.0],
            "RGB 三元组补 alpha=1"
        );
        assert_eq!(material.texture, TextureHandle(0), "无贴图段 = 白色兜底");

        let light = *world.get::<PointLight>(entity).unwrap();
        assert_eq!(light.color, Vec3::new(1.0, 0.5, 0.2));
        assert_eq!(light.intensity, 20.0);
        assert_eq!(light.range, 30.0);
        assert!(light.shadows);

        let collider = *world.get::<Collider>(entity).unwrap();
        assert_eq!(
            collider.shape,
            ColliderShape::Box {
                half_extents: Vec3::new(1.0, 2.0, 3.0)
            }
        );
        assert!(collider.is_static);
        assert_eq!(collider.restitution, 0.2);

        let body = *world.get::<RigidBody>(entity).unwrap();
        assert_eq!(body.gravity_scale, 0.5);
        assert_eq!(body.friction, 0.3);
        assert_eq!(body.linear_damping, 0.1);
        assert_eq!(
            body.velocity,
            Vec3::ZERO,
            "场景物体从静止开始（v1 不序列化速度）"
        );
    }

    /// 胶囊形状段（R31）：`{"capsule": {"radius", "half_height"}}` 加载为
    /// [`ColliderShape::Capsule`]，save ↔ load 往返保持（外部标签
    /// snake_case）；旧格式文件（无 capsule 段）不受影响。
    #[test]
    fn capsule_shape_loads_and_roundtrips() {
        let mut world = fresh_world();
        let (entities, _) = load_str(
            &mut world,
            "capsule.scene.json",
            r#"{
                "entities": [{
                    "transform": { "position": [0.0, 0.9, 0.0] },
                    "collider": { "shape": { "capsule": { "radius": 0.4, "half_height": 0.9 } }, "static": true }
                }]
            }"#,
        );
        let collider = *world.get::<Collider>(entities[0]).unwrap();
        assert_eq!(
            collider.shape,
            ColliderShape::Capsule {
                radius: 0.4,
                half_height: 0.9
            }
        );
        assert!(collider.is_static);

        // save ↔ load 往返：再加载的形状 bit 级一致
        let out = test_dir().join("capsule_out.json");
        save_scene(&mut world, &out).expect("保存");
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(
            text.contains(r#""capsule""#),
            "保存的 JSON 应含 capsule 形状段：{text}"
        );
        let mut world_b = fresh_world();
        let (entities_b, _) = load_str(&mut world_b, "capsule_in.json", &text);
        assert_eq!(
            world_b.get::<Collider>(entities_b[0]).unwrap().shape,
            collider.shape,
            "往返后形状应 bit 级一致"
        );
    }

    /// 静态体语义：`static: true` 且无 rigidbody → 物理按静态处理；
    /// 无 collider 段则不插 Collider 组件。
    #[test]
    fn static_collider_without_rigidbody_is_static() {
        let mut world = fresh_world();
        let (entities, _) = load_str(
            &mut world,
            "static.scene.json",
            r#"{
                "entities": [
                    { "name": "ground", "collider": { "shape": { "box": { "half_extents": [20.0, 1.0, 20.0] } }, "static": true } },
                    { "name": "ghost", "rigidbody": {} }
                ]
            }"#,
        );
        let ground = entities[0];
        let collider = *world.get::<Collider>(ground).unwrap();
        assert!(collider.is_static, "static: true 应进组件");
        assert!(
            world.get::<RigidBody>(ground).is_none(),
            "静态体无 RigidBody（物理缺席语义）"
        );

        let ghost = entities[1];
        assert!(world.get::<Collider>(ghost).is_none());
        assert_eq!(
            *world.get::<RigidBody>(ghost).unwrap(),
            RigidBody::default(),
            "空 rigidbody 段 = 引擎默认刚体"
        );
    }

    /// 登记表：场景创建的 mesh / material 句柄都记有来源。
    #[test]
    fn scene_handles_are_registered() {
        let mut world = fresh_world();
        let (entities, _) = load_str(
            &mut world,
            "registry.scene.json",
            r#"{
                "entities": [{
                    "mesh": { "primitive": { "kind": "plane", "size": 4.0 } }
                }]
            }"#,
        );
        let handle = *world.get::<MeshHandle>(entities[0]).unwrap();
        let registry = world.resource::<SceneAssetRegistry>();
        assert_eq!(
            registry.meshes.get(&handle.0),
            Some(&MeshRef::Primitive(PrimitiveSection::Plane { size: 4.0 })),
            "mesh 句柄应登记图元来源（含参数）"
        );
    }

    /// 分段数过低 → OutOfRange（提前拦截图元生成器的断言 panic）。
    #[test]
    fn low_segment_counts_report_out_of_range() {
        let mut world = fresh_world();
        let path = test_dir().join("lowseg.scene.json");
        std::fs::write(
            &path,
            r#"{ "entities": [{ "mesh": { "primitive": { "kind": "sphere", "segments": 2, "rings": 2 } } }] }"#,
        )
        .unwrap();
        let err = load_scene(&mut world, &path).unwrap_err();
        assert!(
            matches!(err, SceneError::OutOfRange(_)),
            "实际错误：{err:?}"
        );

        std::fs::write(
            &path,
            r#"{ "entities": [{ "mesh": { "primitive": { "kind": "cylinder", "segments": 1 } } }] }"#,
        )
        .unwrap();
        let err = load_scene(&mut world, &path).unwrap_err();
        assert!(
            matches!(err, SceneError::OutOfRange(_)),
            "实际错误：{err:?}"
        );
    }

    // ---- glTF 子场景 ----

    /// glTF 引用：实例顶层实体挂为本实体子实体（带 GltfMounted 标记），
    /// transform 作用于父实体。
    #[test]
    fn gltf_reference_mounts_instance_tree() {
        let dir = test_dir();
        std::fs::write(dir.join("mount_triangle.gltf"), triangle_gltf()).unwrap();
        let mut world = fresh_world();
        let (entities, _) = load_str(
            &mut world,
            "gltf_mount.scene.json",
            r#"{
                "entities": [{
                    "name": "prop",
                    "transform": { "position": [3.0, 0.0, 0.0] },
                    "mesh": { "gltf": "mount_triangle.gltf" }
                }]
            }"#,
        );
        let parent = entities[0];
        assert_eq!(world.get::<Name>(parent).unwrap().0, "prop");
        let children = world.children(parent);
        assert_eq!(children.len(), 1, "glTF 顶层节点应挂为子实体");
        assert_eq!(
            world.get::<Transform>(parent).unwrap().position,
            Vec3::new(3.0, 0.0, 0.0),
            "transform 留在父实体（作用于整棵子树）"
        );
        for &child in &children {
            assert_eq!(
                *world.get::<GltfMounted>(child).unwrap(),
                GltfMounted,
                "子树应带标记"
            );
            // 深层（primitive 子实体）也应带标记
            for &grandchild in &world.children(child) {
                assert_eq!(*world.get::<GltfMounted>(grandchild).unwrap(), GltfMounted);
            }
        }
        let registry = world.resource::<SceneAssetRegistry>();
        assert_eq!(
            registry.gltfs.get(&parent).map(String::as_str),
            Some("mount_triangle.gltf"),
            "glTF 路径应登记在实体上"
        );
    }

    // ---- 环境段 ----

    /// directional_light 进资源（各字段生效）；缺席环境段不改既有资源。
    #[test]
    fn environment_section_applies_directional_light() {
        let mut world = fresh_world();
        let (entities, _) = load_str(
            &mut world,
            "env_light.scene.json",
            r#"{
                "environment": {
                    "directional_light": {
                        "direction": [-1.0, -2.0, 0.0],
                        "color": [1.0, 0.5, 0.5],
                        "intensity": 2.5,
                        "ambient": [0.05, 0.05, 0.1],
                        "shadows": false
                    }
                },
                "entities": [{}]
            }"#,
        );
        assert_eq!(entities.len(), 1);
        let light = world.resource::<DirectionalLight>();
        assert_eq!(light.direction, Vec3::new(-1.0, -2.0, 0.0));
        assert_eq!(light.color, Vec3::new(1.0, 0.5, 0.5));
        assert_eq!(light.intensity, 2.5);
        assert_eq!(light.ambient, Vec3::new(0.05, 0.05, 0.1));
        assert!(!light.shadows);
        assert_eq!(light.shadow_range, 24.0, "shadow_range v1 不序列化，取默认");
        drop(light);

        // 缺席环境段：既有资源不被覆盖
        let mut world = fresh_world();
        world.insert_resource(DirectionalLight {
            intensity: 7.0,
            ..DirectionalLight::default()
        });
        let _ = load_str(&mut world, "env_absent.scene.json", r#"{"entities": []}"#);
        assert_eq!(
            world.resource::<DirectionalLight>().intensity,
            7.0,
            "缺席不改"
        );
    }

    /// sky_source 指向合成 .hdr → SkySource 为 EquirectHdr + 登记路径。
    #[test]
    fn sky_source_loads_hdr_and_registers_path() {
        let dir = test_dir();
        let mut image = HdrImage::new(2, 1);
        image.set_pixel(0, 0, [2.0, 1.0, 0.5]);
        image.set_pixel(1, 0, [0.0, 0.0, 0.0]);
        std::fs::write(dir.join("unit_sky.hdr"), encode_hdr(&image)).unwrap();

        let mut world = fresh_world();
        let (entities, _) = load_str(
            &mut world,
            "env_sky.scene.json",
            r#"{
                "environment": { "sky_source": "unit_sky.hdr" },
                "entities": [{}]
            }"#,
        );
        assert_eq!(entities.len(), 1);
        let sky = world.resource::<SkySource>();
        assert!(
            matches!(sky.kind, SkySourceKind::EquirectHdr(_)),
            "sky_source 应产生 EquirectHdr 天空源"
        );
        assert_ne!(
            sky.generation, 0,
            "外部源 generation 必须与程序化天空（0）区分"
        );
        drop(sky);
        assert_eq!(
            world.resource::<SceneAssetRegistry>().sky_source.as_deref(),
            Some("unit_sky.hdr"),
            "路径应原样登记"
        );
    }

    /// 材质贴图段：从相对路径载入 PNG 进 TextureStore，路径进登记表。
    #[test]
    fn material_texture_loads_from_file() {
        let dir = test_dir();
        // 测试内生成 2x2 纯红 PNG（image crate 是本 crate 依赖，直接编码）
        let png_path = dir.join("unit_tex.png");
        image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]))
            .save(&png_path)
            .expect("写测试 PNG");

        let mut world = fresh_world();
        let (entities, _) = load_str(
            &mut world,
            "tex_material.scene.json",
            r#"{
                "entities": [{
                    "material": { "color": [1.0, 1.0, 1.0, 1.0], "texture": "unit_tex.png" }
                }]
            }"#,
        );
        let handle = *world.get::<MaterialHandle>(entities[0]).unwrap();
        let material = world
            .resource::<MaterialStore>()
            .get(handle)
            .expect("材质应已注册")
            .clone();
        assert_ne!(
            material.texture,
            TextureHandle(0),
            "贴图应已载入（非白色兜底）"
        );
        let textures = world.resource::<TextureStore>();
        let texture = textures.get(material.texture).expect("纹理应已注册");
        assert_eq!((texture.width, texture.height), (2, 2));
        assert_eq!(texture.rgba, [255u8, 0, 0, 255].repeat(4));
        assert_eq!(
            world
                .resource::<SceneAssetRegistry>()
                .materials
                .get(&handle.0)
                .unwrap()
                .texture,
            Some("unit_tex.png".to_string()),
            "贴图路径应登记（save 还原用）"
        );
    }

    // ---- 往返 ----

    /// 丰富场景（全部组件类型 + glTF 引用 + 贴图）→ save → load → save：
    /// 二次保存与首次保存**逐字节一致**，且两次加载的世界逐实体语义等价
    /// （数值 1e-6 容差、mesh ref 相等、实体顺序稳定）。
    #[test]
    fn save_load_roundtrip_is_stable() {
        let dir = test_dir();
        std::fs::write(dir.join("round_triangle.gltf"), triangle_gltf()).unwrap();
        image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 255, 0, 255]))
            .save(dir.join("round_tex.png"))
            .expect("写测试 PNG");

        let scene_path = dir.join("roundtrip.scene.json");
        std::fs::write(
            &scene_path,
            r#"{
                "environment": {
                    "directional_light": {
                        "direction": [-0.4, -1.0, -0.2],
                        "color": [1.0, 0.9, 0.8],
                        "intensity": 1.4,
                        "ambient": [0.1, 0.1, 0.12],
                        "shadows": true
                    },
                    "sky_source": "round_sky.hdr"
                },
                "entities": [
                    {
                        "name": "floor",
                        "transform": { "position": [0.0, -1.0, 0.0] },
                        "mesh": { "primitive": { "kind": "plane", "size": 20.0 } },
                        "collider": { "shape": { "box": { "half_extents": [10.0, 1.0, 10.0] } }, "static": true }
                    },
                    {
                        "name": "coin",
                        "visible": false,
                        "transform": { "position": [1.0, 2.0, 3.0], "rotation": [0.0, 0.3826834, 0.0, 0.9238795], "scale": [1.5, 1.5, 1.5] },
                        "mesh": { "primitive": { "kind": "cylinder", "radius": 0.3, "height": 0.1, "segments": 16 } },
                        "material": { "color": [0.9, 0.7, 0.1, 1.0], "metallic": 1.0, "roughness": 0.3, "emissive": [0.4, 0.2, 0.0], "texture": "round_tex.png" },
                        "collider": { "shape": { "sphere": { "radius": 0.3 } }, "restitution": 0.6 },
                        "rigidbody": { "gravity_scale": 1.0, "friction": 0.4, "linear_damping": 0.2 }
                    },
                    {
                        "name": "lamp",
                        "point_light": { "color": [1.0, 0.8, 0.6], "intensity": 30.0, "range": 25.0, "shadows": true }
                    },
                    {
                        "name": "prop",
                        "transform": { "position": [-3.0, 0.5, 0.0] },
                        "mesh": { "gltf": "round_triangle.gltf" }
                    },
                    { "name": "logic-only" }
                ]
            }"#,
        )
        .unwrap();
        // 天空源 fixture
        let mut sky = HdrImage::new(2, 1);
        sky.set_pixel(0, 0, [1.0, 1.0, 1.0]);
        sky.set_pixel(1, 0, [0.5, 0.5, 0.5]);
        std::fs::write(dir.join("round_sky.hdr"), encode_hdr(&sky)).unwrap();

        // 第一次加载 → 保存
        let mut world_a = fresh_world();
        let entities_a = load_scene(&mut world_a, &scene_path).expect("首次加载");
        assert_eq!(entities_a.len(), 5, "5 个场景实体（glTF 子树不计）");
        let out_a = dir.join("roundtrip_out_a.json");
        let count = save_scene(&mut world_a, &out_a).expect("首次保存");
        assert_eq!(count, 5, "save 返回写出实体数");

        // 第二次加载 → 保存：文件必须逐字节一致（确定性输出）
        let mut world_b = fresh_world();
        let entities_b = load_scene(&mut world_b, &out_a).expect("二次加载");
        let out_b = dir.join("roundtrip_out_b.json");
        save_scene(&mut world_b, &out_b).expect("二次保存");
        let text_a = std::fs::read_to_string(&out_a).unwrap();
        let text_b = std::fs::read_to_string(&out_b).unwrap();
        assert_eq!(
            text_a, text_b,
            "save(load(save(load(f)))) 必须与 save(load(f)) 逐字节一致"
        );

        // 语义逐实体比对（顺序 = spawn 顺序，稳定）
        assert_eq!(entities_a.len(), entities_b.len());
        for (&a, &b) in entities_a.iter().zip(&entities_b) {
            let name_a = world_a.get::<Name>(a).map(|n| n.0.clone());
            let name_b = world_b.get::<Name>(b).map(|n| n.0.clone());
            assert_eq!(name_a, name_b, "实体顺序应稳定（按 name 对齐）");

            let ta = *world_a.get::<Transform>(a).unwrap();
            let tb = *world_b.get::<Transform>(b).unwrap();
            assert!(
                (ta.position - tb.position).length() < 1e-6,
                "位置 1e-6 容差"
            );
            assert!(
                (ta.rotation.dot(tb.rotation) - 1.0).abs() < 1e-6,
                "旋转语义等价"
            );
            assert!((ta.scale - tb.scale).length() < 1e-6);

            assert_eq!(
                world_a.get::<Visibility>(a).is_some_and(|v| !v.visible),
                world_b.get::<Visibility>(b).is_some_and(|v| !v.visible),
                "可见性往返一致"
            );

            // mesh ref 相等（登记表来源逐项比对）
            let mesh_a = world_a
                .get::<MeshHandle>(a)
                .and_then(|h| {
                    world_a
                        .resource::<SceneAssetRegistry>()
                        .meshes
                        .get(&h.0)
                        .cloned()
                })
                .or_else(|| {
                    world_a
                        .resource::<SceneAssetRegistry>()
                        .gltfs
                        .get(&a)
                        .map(|p| MeshRef::Gltf(p.clone()))
                });
            let mesh_b = world_b
                .get::<MeshHandle>(b)
                .and_then(|h| {
                    world_b
                        .resource::<SceneAssetRegistry>()
                        .meshes
                        .get(&h.0)
                        .cloned()
                })
                .or_else(|| {
                    world_b
                        .resource::<SceneAssetRegistry>()
                        .gltfs
                        .get(&b)
                        .map(|p| MeshRef::Gltf(p.clone()))
                });
            assert_eq!(mesh_a, mesh_b, "mesh 来源应往返相等（{name_a:?}）");

            // material 数值 + 贴图路径
            let mat_a = world_a.get::<MaterialHandle>(a).and_then(|h| {
                world_a.resource::<MaterialStore>().get(*h).map(|d| {
                    (
                        d.base_color,
                        d.metallic,
                        d.roughness,
                        d.emissive,
                        d.texture.0 != 0,
                        world_a
                            .resource::<SceneAssetRegistry>()
                            .materials
                            .get(&h.0)
                            .and_then(|m| m.texture.clone()),
                    )
                })
            });
            let mat_b = world_b.get::<MaterialHandle>(b).and_then(|h| {
                world_b.resource::<MaterialStore>().get(*h).map(|d| {
                    (
                        d.base_color,
                        d.metallic,
                        d.roughness,
                        d.emissive,
                        d.texture.0 != 0,
                        world_b
                            .resource::<SceneAssetRegistry>()
                            .materials
                            .get(&h.0)
                            .and_then(|m| m.texture.clone()),
                    )
                })
            });
            assert_eq!(mat_a, mat_b, "材质数值与贴图路径应往返相等（{name_a:?}）");

            // 点光（组件无 PartialEq，逐字段比对）
            let light_fields = |world: &World, e: Entity| {
                world
                    .get::<PointLight>(e)
                    .map(|l| (l.color, l.intensity, l.range, l.shadows))
            };
            assert_eq!(
                light_fields(&world_a, a),
                light_fields(&world_b, b),
                "点光应往返相等（{name_a:?}）"
            );

            // collider（数值 1e-6）
            let col_a = world_a.get::<Collider>(a).map(|c| *c);
            let col_b = world_b.get::<Collider>(b).map(|c| *c);
            assert_eq!(col_a.is_some(), col_b.is_some());
            if let (Some(ca), Some(cb)) = (col_a, col_b) {
                assert_eq!(ca.shape, cb.shape, "碰撞形状应往返相等（{name_a:?}）");
                assert_eq!(ca.is_static, cb.is_static);
                assert!((ca.restitution - cb.restitution).abs() < 1e-6);
            }

            // rigidbody
            assert_eq!(
                world_a.get::<RigidBody>(a).map(|r| *r),
                world_b.get::<RigidBody>(b).map(|r| *r),
                "刚体应往返相等（{name_a:?}）"
            );
        }

        // 环境段往返
        let sky_a = world_a.resource::<SkySource>();
        let sky_b = world_b.resource::<SkySource>();
        assert!(matches!(sky_a.kind, SkySourceKind::EquirectHdr(_)));
        assert!(matches!(sky_b.kind, SkySourceKind::EquirectHdr(_)));
        assert_eq!(
            world_a.resource::<SceneAssetRegistry>().sky_source,
            world_b.resource::<SceneAssetRegistry>().sky_source,
        );
        let light_a = world_a.resource::<DirectionalLight>();
        let light_b = world_b.resource::<DirectionalLight>();
        assert_eq!(light_a.direction, light_b.direction);
        assert!((light_a.intensity - light_b.intensity).abs() < 1e-6);
    }

    /// glTF 内部实体不进场景文件：save 计数只含场景实体，重载后树结构重建。
    #[test]
    fn save_skips_gltf_internal_entities() {
        let dir = test_dir();
        std::fs::write(dir.join("skip_triangle.gltf"), triangle_gltf()).unwrap();
        let mut world = fresh_world();
        let _ = load_str(
            &mut world,
            "gltf_skip.scene.json",
            r#"{ "entities": [{ "mesh": { "gltf": "skip_triangle.gltf" } }] }"#,
        );
        let scene_entities = world.entity_count();
        assert!(
            scene_entities > 1,
            "glTF 子树应真实生成实体（父 + 根 + primitive）"
        );

        let out = dir.join("gltf_skip_out.json");
        let count = save_scene(&mut world, &out).expect("保存");
        assert_eq!(count, 1, "只写 1 个场景实体（glTF 内部实体被标记跳过）");
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains(r#""gltf""#), "mesh 段应还原为 glTF 引用");
        assert!(
            !text.contains(r#""primitive""#),
            "glTF 子树实体不应泄漏为图元段"
        );

        // 重载：又是 1 个场景实体 + 重建的子树
        let mut world2 = fresh_world();
        let reloaded = load_scene(&mut world2, &out).expect("重载");
        assert_eq!(reloaded.len(), 1);
        assert_eq!(world2.children(reloaded[0]).len(), 1, "glTF 子树应整体重建");
    }

    /// 非场景来源的 mesh：save 不 panic、该实体 mesh 段省略（warn 降级）、
    /// 其余实体照写，产出文件可再次加载。
    #[test]
    fn save_degrades_foreign_mesh_gracefully() {
        let mut world = fresh_world();
        let _ = load_str(
            &mut world,
            "foreign.scene.json",
            r#"{ "entities": [{ "name": "scene-cube", "mesh": { "primitive": { "kind": "cube" } } }] }"#,
        );
        // 用户代码直接注册的网格（不经场景加载 → 无登记）
        let foreign = world
            .resource_mut::<MeshStore>()
            .add(MeshData::uv_sphere(1.0, 8, 4));
        world.spawn((
            Transform::from_xyz(5.0, 0.0, 0.0),
            foreign,
            MaterialHandle(0),
        ));

        let out = test_dir().join("foreign_out.json");
        let count = save_scene(&mut world, &out).expect("保存不 panic");
        assert_eq!(count, 2, "两个实体都写（外来 mesh 实体降级而非丢弃）");
        let text = std::fs::read_to_string(&out).unwrap();
        let doc: SceneDoc = serde_json::from_str(&text).unwrap();
        let defs = doc.entities.expect("实体段应存在");
        assert_eq!(defs.len(), 2);
        let degraded = defs
            .iter()
            .find(|d| d.name.is_none())
            .expect("应有降级的无名实体");
        assert!(degraded.mesh.is_none(), "非场景 mesh 段应省略");
        let kept = defs
            .iter()
            .find(|d| d.name.as_deref() == Some("scene-cube"))
            .unwrap();
        assert!(kept.mesh.is_some(), "场景实体照写");

        // 产出文件可再次加载（降级实体变成纯逻辑实体）
        let mut world2 = fresh_world();
        let reloaded = load_scene(&mut world2, &out).expect("降级文件应可加载");
        assert_eq!(reloaded.len(), 2);
    }

    /// 用户手工创建的材质（非场景来源）：数值字段照写、texture 省略降级。
    #[test]
    fn save_writes_unregistered_material_numerics() {
        let mut world = fresh_world();
        ensure_resources(&mut world);
        let mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
        let mat = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.2, 0.4, 0.8, 1.0));
        world.spawn((Transform::IDENTITY, mesh, mat));

        let out = test_dir().join("foreign_mat_out.json");
        save_scene(&mut world, &out).expect("保存");
        let text = std::fs::read_to_string(&out).unwrap();
        let doc: SceneDoc = serde_json::from_str(&text).unwrap();
        let defs = doc.entities.expect("实体段应存在");
        let material = defs[0].material.as_ref().expect("材质段应保留数值");
        assert_eq!(
            material.color,
            [0.2, 0.4, 0.8, 1.0],
            "数值字段照 MaterialDef 写出"
        );
        assert_eq!(
            material.texture, None,
            "无登记路径 → texture 省略（warn 降级）"
        );
        assert!(defs[0].mesh.is_none(), "非场景 mesh 段省略");
    }

    /// 默认材质（句柄 0，隐式兜底）整段省略：save 输出稳定、无冗余段。
    #[test]
    fn save_omits_default_material() {
        let mut world = fresh_world();
        let (entities, _) = load_str(
            &mut world,
            "default_mat.scene.json",
            r#"{ "entities": [{ "mesh": { "primitive": { "kind": "cube" } } }] }"#,
        );
        assert_eq!(
            *world.get::<MaterialHandle>(entities[0]).unwrap(),
            MaterialHandle(0)
        );
        let out = test_dir().join("default_mat_out.json");
        save_scene(&mut world, &out).expect("保存");
        let text = std::fs::read_to_string(&out).unwrap();
        let doc: SceneDoc = serde_json::from_str(&text).unwrap();
        assert!(
            doc.entities.unwrap()[0].material.is_none(),
            "句柄 0 整段省略"
        );
    }

    /// 材质 alpha_mode 序列化（R23）：旧场景文件（无该字段）加载回退
    /// `Opaque`；显式 `"Blend"` 加载为 `Blend` 且 save ↔ load 往返保持；
    /// 不透明材质 save 时省略该字段（旧文件重存字节级不变）。
    #[test]
    fn alpha_mode_legacy_default_and_blend_roundtrip() {
        // 旧格式：无 alpha_mode 字段 → Opaque
        let mut world = fresh_world();
        let (entities, _) = load_str(
            &mut world,
            "legacy_opaque.scene.json",
            r#"{ "entities": [{
                    "mesh": { "primitive": { "kind": "cube" } },
                    "material": { "color": [0.8, 0.2, 0.1, 1.0] }
                }] }"#,
        );
        let handle = *world.get::<MaterialHandle>(entities[0]).unwrap();
        let def = world
            .resource::<MaterialStore>()
            .get(handle)
            .unwrap()
            .clone();
        assert_eq!(def.alpha_mode, AlphaMode::Opaque, "旧文件缺字段 = Opaque");
        // 重存：字段应被省略（与旧格式字节级一致）
        let out = test_dir().join("legacy_opaque_out.json");
        save_scene(&mut world, &out).expect("保存旧格式场景");
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(
            !text.contains("alpha_mode"),
            "不透明材质 save 省略 alpha_mode，旧文件重存保持字节级稳定"
        );

        // 新格式：显式 Blend → 加载为 Blend，且 save → load 往返保持
        let mut world = fresh_world();
        let (entities, _) = load_str(
            &mut world,
            "blend_mat.scene.json",
            r#"{ "entities": [{
                    "mesh": { "primitive": { "kind": "cube" } },
                    "material": { "color": [0.6, 0.8, 1.0, 0.4], "alpha_mode": "Blend" }
                }] }"#,
        );
        let handle = *world.get::<MaterialHandle>(entities[0]).unwrap();
        let def = world
            .resource::<MaterialStore>()
            .get(handle)
            .unwrap()
            .clone();
        assert_eq!(def.alpha_mode, AlphaMode::Blend);
        assert_eq!(def.base_color, [0.6, 0.8, 1.0, 0.4]);
        let out_a = test_dir().join("blend_mat_out_a.json");
        save_scene(&mut world, &out_a).expect("保存 Blend 场景");
        let text = std::fs::read_to_string(&out_a).unwrap();
        assert!(text.contains(r#""alpha_mode": "Blend""#), "Blend 必须写出");

        // 二次加载：往返后材质仍为 Blend（load(save(load(f))) 语义等价）
        let mut world_b = fresh_world();
        let entities_b = load_scene(&mut world_b, &out_a).expect("二次加载");
        let handle_b = *world_b.get::<MaterialHandle>(entities_b[0]).unwrap();
        let def_b = world_b
            .resource::<MaterialStore>()
            .get(handle_b)
            .unwrap()
            .clone();
        assert_eq!(def_b.alpha_mode, AlphaMode::Blend, "往返后仍为 Blend");
        assert_eq!(def_b.base_color, [0.6, 0.8, 1.0, 0.4]);
    }

    /// 错误 Display 风格与 GltfError 一致（含上下文、可作 dyn Error）。
    #[test]
    fn error_display() {
        let e = SceneError::Missing("entities".into());
        assert!(e.to_string().contains("entities"));
        let e = SceneError::OutOfRange("segments = 1（≥3）".into());
        assert!(e.to_string().contains("segments"));
        let _: &dyn std::error::Error = &e;
    }

    /// GLB 容器也能作为 glTF 段引用（write_glb 组装的容器走同一加载路径）。
    #[test]
    fn glb_container_reference_mounts() {
        let mut buffer: Vec<u8> = Vec::new();
        for p in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            buffer.extend_from_slice(&p.map(f32::to_le_bytes).concat());
        }
        buffer.extend_from_slice(&[0u16, 1, 2].map(u16::to_le_bytes).concat());
        let json = r#"{
            "asset": {"version": "2.0"},
            "buffers": [{"byteLength": 36}],
            "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": 36}],
            "accessors": [{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "indices": null}]}],
            "nodes": [{"mesh": 0}],
            "scenes": [{"nodes": [0]}],
            "scene": 0
        }"#;
        // indices 字段不能为 null：去掉（无索引 = 顺序展开）
        let json = json.replace(r#", "indices": null"#, "");
        let dir = test_dir();
        std::fs::write(dir.join("unit_box.glb"), write_glb(&json, &buffer)).unwrap();

        let mut world = fresh_world();
        let (entities, _) = load_str(
            &mut world,
            "glb_ref.scene.json",
            r#"{ "entities": [{ "mesh": { "gltf": "unit_box.glb" } }] }"#,
        );
        assert_eq!(
            world.children(entities[0]).len(),
            1,
            "GLB 顶层节点应挂为子实体"
        );
    }

    // ---- 挂载（R45：pack / mount 通路） ----

    /// 场景经 pack 挂载加载 == 直读加载：≥3 文件演示资产集（GltfBuilder
    /// 生成的自包含 GLB + 场景 JSON + OGG）打包 → AssetMount(Pack) 读回
    /// 加载，两世界的实体 / 组件 / glTF 子树 / 网格逐项一致，JSON 字节
    /// 逐位一致；OGG 字节经 mount 读回可走 iris-audio 内存入口。
    #[test]
    fn scene_loads_from_pack_mount_identically_to_fs() {
        use crate::pack::{PackReader, PackWriter};
        use crate::vfs::{AssetMount, AssetSource};
        use iris_audio::OggVorbisStream;

        let dir = test_dir();

        // ① 演示资产集三件：GLB（GltfBuilder 自包含）、场景 JSON、OGG
        let mut builder = crate::gltf_builder::GltfBuilder::new();
        let material = builder.material([0.9, 0.2, 0.1, 1.0], 0.0, 0.6, None);
        let mesh = builder.mesh("crate", &MeshData::cube(), material);
        builder.node(crate::gltf_builder::GltfNodeDesc::new("crate").with_mesh(mesh));
        let glb = builder.build_glb();
        std::fs::write(dir.join("pack_demo.glb"), &glb).unwrap();

        let scene_json = r#"{
            "entities": [{
                "name": "crate",
                "transform": { "position": [1.0, 0.5, 0.0] },
                "mesh": { "gltf": "pack_demo.glb" }
            }]
        }"#;
        std::fs::write(dir.join("pack_demo.scene.json"), scene_json).unwrap();

        // OGG fixture（iris-audio 测试资产；存在性由仓库布局保证）
        let ogg_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../iris-audio/tests/fixtures/tone_2s.ogg");
        let ogg = std::fs::read(&ogg_path).expect("读 iris-audio OGG fixture");

        // ② 打包（mount 内路径约定 = 打包时写入的路径）
        let mut writer = PackWriter::new();
        writer
            .add("pack_demo.scene.json", scene_json.as_bytes().to_vec())
            .unwrap();
        writer.add("pack_demo.glb", glb.clone()).unwrap();
        writer.add("audio/ambient.ogg", ogg.clone()).unwrap();
        let pack_path = dir.join(format!("pack_roundtrip_{}.ipak", std::process::id()));
        writer.write_file(&pack_path).expect("写 pack");

        let mut mount = AssetMount::new();
        mount.push(AssetSource::Pack(
            PackReader::open(&pack_path).expect("打开 pack"),
        ));
        // ③ JSON 字节逐位一致；场景经 pack 加载 == 直读加载
        assert_eq!(
            mount.read("pack_demo.scene.json").unwrap(),
            scene_json.as_bytes(),
            "挂载读回的 JSON 应与源逐位一致"
        );

        let mut world_fs = fresh_world();
        let entities_fs =
            load_scene(&mut world_fs, dir.join("pack_demo.scene.json")).expect("文件系统直读加载");
        let mut world_mount = fresh_world();
        let entities_mount = load_scene_from(&mut world_mount, &mount, "pack_demo.scene.json")
            .expect("pack 挂载加载");
        assert_eq!(entities_fs.len(), entities_mount.len(), "实体数一致");

        let (a, b) = (entities_fs[0], entities_mount[0]);
        assert_eq!(
            world_fs.get::<Name>(a).unwrap().0,
            world_mount.get::<Name>(b).unwrap().0
        );
        assert_eq!(
            *world_fs.get::<Transform>(a).unwrap(),
            *world_mount.get::<Transform>(b).unwrap()
        );
        // glTF 子树经 from_bytes 通路整体重建
        assert_eq!(world_fs.children(a).len(), 1, "直读：glTF 子树");
        assert_eq!(
            world_mount.children(b).len(),
            1,
            "挂载：glTF 子树（from_bytes）"
        );
        let mesh_of = |world: &World, parent: Entity| {
            // 层级：场景实体 → glTF 节点实体 → primitive 实体（持 MeshHandle）
            let node = world.children(parent)[0];
            let primitive = world.children(node)[0];
            let handle = *world.get::<MeshHandle>(primitive).unwrap();
            let data = world.resource::<MeshStore>().get(handle).unwrap().clone();
            data.vertices
                .iter()
                .map(|v| (v.position, v.normal, v.uv))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            mesh_of(&world_fs, a),
            mesh_of(&world_mount, b),
            "两入口网格顶点应逐位一致"
        );

        // ④ OGG 经 mount 读回 → iris-audio 内存入口（mount 抽象覆盖音频
        // 文件 IO 的接缝验证）
        let stream = OggVorbisStream::from_bytes(mount.read("audio/ambient.ogg").unwrap())
            .expect("OGG 内存入口");
        assert_eq!(iris_audio::AudioStream::channels(&stream), 2);
        assert_eq!(iris_audio::AudioStream::sample_rate(&stream), 48_000);

        std::fs::remove_file(&pack_path).ok();
    }

    /// 未挂载默认语义零变化：`load_scene` 走文件系统（全量既有测试的
    /// 覆盖之外，这里显式断言挂载资源默认不进 World——缺席 = 旧语义）。
    #[test]
    fn load_scene_without_mount_keeps_fs_semantics() {
        use crate::vfs::AssetMount;
        let mut world = fresh_world();
        let (entities, _) = load_str(&mut world, "no_mount.scene.json", r#"{"entities": [{}]}"#);
        assert_eq!(entities.len(), 1);
        assert!(
            !world.contains_resource::<AssetMount>(),
            "默认不注册挂载资源（缺席 = asset_path 文件系统语义零变化）"
        );

        // mount 里的资产对默认 load_scene 不可见：路径语义不混用
        static EMBEDDED: &[(&str, &str)] = &[("mount_only.scene.json", r#"{ "entities": [] }"#)];
        let mut mount = AssetMount::new();
        mount.push(crate::vfs::AssetSource::Embedded(EMBEDDED));
        let err = load_scene(&mut world, test_dir().join("mount_only.scene.json")).unwrap_err();
        assert!(
            matches!(err, SceneError::Io(ref e) if e.kind() == std::io::ErrorKind::NotFound),
            "未挂载时挂载内资产不可达（文件系统语义），实际 {err:?}"
        );
        let _ = mount; // 挂载仅存在于调用方，未入 World
    }

    /// 挂载内未命中 / 损坏 → SceneError::Asset（不与 Io / Missing 混淆）。
    #[test]
    fn mount_miss_and_corruption_report_asset_error() {
        use crate::pack::{PackReader, PackWriter};
        use crate::vfs::{AssetMount, AssetSource};

        // 空 pack：场景未命中
        let mut writer = PackWriter::new();
        writer.add("placeholder.bin", vec![1]).unwrap();
        let mut pack_bytes = Vec::new();
        writer.write_to(&mut pack_bytes).unwrap();
        let dir = std::env::temp_dir().join("iris-scene-test");
        std::fs::create_dir_all(&dir).unwrap();
        let pack_path = dir.join(format!("mount_miss_{}.ipak", std::process::id()));
        std::fs::write(&pack_path, &pack_bytes).unwrap();

        let mut mount = AssetMount::new();
        mount.push(AssetSource::Pack(
            PackReader::open(&pack_path).expect("打开 pack"),
        ));
        let mut world = fresh_world();
        let err = load_scene_from(&mut world, &mount, "missing.scene.json").unwrap_err();
        assert!(
            matches!(err, SceneError::Asset(ref what) if what.contains("未命中")),
            "未命中应报 Asset：{err}"
        );
        std::fs::remove_file(&pack_path).ok();
    }

    // ---- v2（R57）：粒子段与同源去重 ----

    /// particles 段：配置字段进组件（缺省 = `ParticleEmitter::new(Alpha)`），
    /// save ↔ load 往返逐字段位级一致；运行期状态不入档（重载后新鲜）。
    #[test]
    fn particles_section_loads_and_roundtrips() {
        let mut world = fresh_world();
        let (entities, _) = load_str(
            &mut world,
            "particles.scene.json",
            r#"{
                "entities": [
                    { "name": "jet", "transform": { "position": [0.0, 0.72, 0.0] },
                      "particles": { "blend": "alpha", "spawn_rate": 90.0,
                                     "particle_lifetime": 1.6, "initial_speed": 6.0,
                                     "velocity_jitter": 0.35, "size_start": 0.22,
                                     "size_end": 0.05,
                                     "color_start": [0.6, 0.8, 1.0, 0.85],
                                     "color_end": [0.15, 0.3, 0.8, 0.0],
                                     "max_particles": 1024 } },
                    { "name": "boom", "particles": { "blend": "additive", "burst_count": 32 } }
                ]
            }"#,
        );
        let jet = world
            .get::<ParticleEmitter>(entities[0])
            .expect("粒子段应进组件");
        assert_eq!(jet.blend, ParticleBlend::Alpha);
        assert_eq!(jet.spawn_rate.to_bits(), 90.0f32.to_bits());
        assert_eq!(jet.particle_lifetime.to_bits(), 1.6f32.to_bits());
        assert_eq!(jet.initial_speed.to_bits(), 6.0f32.to_bits());
        assert_eq!(jet.velocity_jitter.to_bits(), 0.35f32.to_bits());
        assert_eq!(jet.size_start.to_bits(), 0.22f32.to_bits());
        assert_eq!(jet.size_end.to_bits(), 0.05f32.to_bits());
        assert_eq!(
            jet.color_start.map(f32::to_bits),
            [0.6f32, 0.8, 1.0, 0.85].map(f32::to_bits)
        );
        assert_eq!(
            jet.color_end.map(f32::to_bits),
            [0.15f32, 0.3, 0.8, 0.0].map(f32::to_bits)
        );
        assert_eq!(jet.max_particles, 1024);
        assert_eq!(jet.live_count(), 0, "加载即新鲜状态（无运行期残留）");

        // 缺省字段 = new(Alpha)：burst 段只写两个字段，其余照默认
        let boom = world.get::<ParticleEmitter>(entities[1]).unwrap();
        assert_eq!(boom.blend, ParticleBlend::Additive);
        assert_eq!(boom.burst_count, 32);
        assert_eq!(boom.spawn_rate.to_bits(), 60.0f32.to_bits(), "缺省发射率");
        assert_eq!(boom.max_particles, 1024, "缺省池上限");
        // 组件守卫析构会延长 &world 借用——save 前显式释放
        drop(jet);
        drop(boom);

        // save → load：配置逐字段位级一致
        let out = test_dir().join("particles_out.json");
        save_scene(&mut world, &out).expect("保存");
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains(r#""particles""#), "particles 段应写出");
        let mut world_b = fresh_world();
        let entities_b = load_scene(&mut world_b, &out).expect("重载");
        for (&a, &b) in entities.iter().zip(&entities_b) {
            let ea = world.get::<ParticleEmitter>(a).unwrap();
            let eb = world_b.get::<ParticleEmitter>(b).unwrap();
            assert_eq!(
                ParticleSection::from_emitter(&ea),
                ParticleSection::from_emitter(&eb),
                "配置字段应往返一致"
            );
        }
    }

    /// 同源去重（v2）：同图元参数 / 同材质段 / 同贴图文件在一次加载内
    /// 共享句柄（首见注册、后见复用）；异源不共享。这是「文件内同源 ⇒
    /// 世界内同句柄」的规范化保证（模块文档「同源去重」）。
    #[test]
    fn identical_sources_share_handles_within_one_load() {
        let dir = test_dir();
        image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 255, 255]))
            .save(dir.join("dedup_tex.png"))
            .expect("写测试 PNG");
        let mut world = fresh_world();
        let (entities, _) = load_str(
            &mut world,
            "dedup.scene.json",
            r#"{
                "entities": [
                    { "name": "a", "mesh": { "primitive": { "kind": "cube" } },
                      "material": { "color": [0.8, 0.2, 0.1, 1.0], "texture": "dedup_tex.png" } },
                    { "name": "b", "mesh": { "primitive": { "kind": "cube" } },
                      "material": { "color": [0.8, 0.2, 0.1, 1.0], "texture": "dedup_tex.png" } },
                    { "name": "c", "mesh": { "primitive": { "kind": "cube" } },
                      "material": { "color": [0.1, 0.2, 0.8, 1.0] } },
                    { "name": "d", "mesh": { "primitive": { "kind": "sphere", "segments": 8, "rings": 4 } } }
                ]
            }"#,
        );
        let mesh_of = |e: Entity| *world.get::<MeshHandle>(e).unwrap();
        let mat_of = |e: Entity| *world.get::<MaterialHandle>(e).unwrap();
        assert_eq!(mesh_of(entities[0]), mesh_of(entities[1]), "同图元共享");
        assert_eq!(mesh_of(entities[0]), mesh_of(entities[2]), "同图元共享");
        assert_ne!(mesh_of(entities[0]), mesh_of(entities[3]), "异图元不共享");
        assert_eq!(mat_of(entities[0]), mat_of(entities[1]), "同材质共享");
        assert_ne!(mat_of(entities[0]), mat_of(entities[2]), "异材质不共享");

        // Store 布局：mesh 2 条（cube + sphere）、材质 2 条、纹理 2 条
        // （0 号白兜底 + 同一 PNG 只解码登记一次）
        assert_eq!(world.resource::<MeshStore>().len(), 2);
        assert_eq!(world.resource::<TextureStore>().len(), 2);
        let registry = world.resource::<SceneAssetRegistry>();
        assert_eq!(registry.meshes.len(), 2, "登记表按句柄一条");
        assert_eq!(registry.materials.len(), 2);
    }
}
