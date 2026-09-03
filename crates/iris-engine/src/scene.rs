//! 场景序列化：JSON 场景描述文件 ↔ 世界实体（关卡数据与游戏代码分离）。
//!
//! 场景文件描述两类内容：**环境段**（场景级资源形态：方向光 + 天空源）与
//! **实体段**（逐实体挂载内置组件集）。缺省值全部与引擎默认一致——
//! `Transform` 恒等、材质取 [`MaterialDef::default`]、点光字段照
//! [`PointLight::default`]，手写关卡文件时只需写与默认不同的字段。
//!
//! ```jsonc
//! {
//!   "environment": {                              // 可选；缺席不改既有资源
//!     "directional_light": {                      // 各字段可选，缺省 = 引擎默认
//!       "direction": [-0.5, -1.0, -0.4], "color": [1.0, 0.98, 0.92],
//!       "intensity": 1.0, "ambient": [0.14, 0.14, 0.14], "shadows": true
//!     },
//!     "sky_source": "assets/sunset.hdr"           // 可选；缺席 = 程序化天空
//!   },
//!   "entities": [                                 // 必需
//!     {
//!       "name": "wall-north",                     // 可选 → Name 组件
//!       "visible": false,                         // 可选，缺省可见 → Visibility
//!       "transform": { "position": [0,0,0], "rotation": [0,0,0,1], "scale": [1,1,1] },
//!       "mesh":    { "primitive": { "kind": "cube" } },   // 三态之一（见下）
//!       "material": { "color": [0.8,0.2,0.1,1], "metallic": 0.0,
//!                     "roughness": 0.5, "emissive": [0,0,0],
//!                     "texture": "assets/tex.png",
//!                     "alpha_mode": "Blend" },       // 全可选；缺省 Opaque
//!       "point_light": { "color": [1,1,1], "intensity": 20, "range": 30,
//!                        "shadows": false },
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
//! # 往返（save ↔ load）
//!
//! [`save_scene`] 查询内置组件集（Transform + 可选 Name / Visibility /
//! MeshHandle / MaterialHandle / PointLight / Collider / RigidBody）全量写出，
//! [`load_scene`] 逐实体还原，构成不变量 `load(save(load(f)))` 与 `load(f)`
//! 实体组件语义等价（实体顺序 = spawn 顺序，稳定）。mesh / material /
//! 天空源的**来源**（图元参数、glTF 文件路径、贴图路径）由 [`SceneAssetRegistry`]
//! 资源在 load 时登记、save 时还原——`MeshStore` / `TextureStore` 本身没有
//! 「文件路径」概念，登记表是数值句柄与场景文件之间的桥：
//!
//! - mesh 句柄在登记表 → 还原图元段 / glTF 段；不在（用户代码 `MeshStore::add`）
//!   → mesh 段省略 + `log::warn`（数量统计，重新加载后该实体没有网格）；
//! - material 句柄在登记表 → 数值字段照 [`MaterialDef`] 写出 + 登记的贴图路径；
//!   不在 → 只写数值字段 + 贴图路径丢失警告（纯数值材质无损）；
//!   句柄 0（默认材质，隐式）→ 整段省略；
//! - 天空源为 `EquirectHdr` 且登记了路径 → 写 `sky_source`；无登记（用户手工
//!   挂入）→ 省略 + 警告（v1 简化：HDR 图像内容不回写文件）。
//!
//! glTF 实例生成的内部实体（根 + 后代）带 [`GltfMounted`] 标记，save 时跳过
//! ——它们由 glTF 段在下次加载时整体重建，不属于场景文件本身。
//!
//! # v1 不含（诚实清单）
//!
//! - UI 组件（HUD 属游戏逻辑，`UiRect` / `UiText` 实体不参与往返）与相机
//!   （资源级，由代码或平台层配置）；
//! - 克隆 / 预设 / 组件插件扩展点（自定义组件不经序列化）；
//! - `RigidBody.velocity` 与 `DirectionalLight.shadow_range` 不写入文件
//!   （物体从静止开始、阴影覆盖半径取默认）；
//! - 热重载：场景文件为手工编写，加载后与文件的关联即断开；
//! - wasm32 注意：场景 / glTF / HDR / 贴图读取走 `std::fs`——native 可用，
//!   **web 上不可用**（与引擎既有资产管线的平台约定一致），web 场景需自行
//!   经 fetch 取字节后走内存解析入口（v1 未提供）。
//!
//! # 用法
//!
//! ```ignore
//! use iris_engine::prelude::*;
//!
//! // 加载：插入缺失的资源兜底 → 写环境 → 逐实体 spawn
//! let entities = load_scene(&mut world, "assets/arena.scene.json")?;
//!
//! // 保存：内置组件集 + 登记的资产来源全量写出，返回实体数
//! let count = save_scene(&mut world, "assets/arena_out.json")?;
//! ```

use iris_core::name::Name;
use iris_core::physics::{Collider, ColliderShape, RigidBody};
use iris_core::prelude::{Entity, Quat, Transform, Vec3, World};
use iris_core::{Component, Resource};
use iris_render::asset::{GltfError, HdrError, load_hdr, spawn_gltf_instance};
use iris_render::prelude::{
    AlphaMode, CpuTexture, DirectionalLight, MaterialDef, MaterialHandle, MaterialStore, MeshData,
    MeshHandle, MeshStore, PointLight, SkySource, SkySourceKind, TextureHandle, TextureStore,
    Visibility,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::Path;

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
}

impl fmt::Display for SceneError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO 错误：{e}"),
            Self::Json(e) => write!(f, "JSON 解析错误：{e}"),
            Self::Missing(what) => write!(f, "场景缺少必需字段：{what}"),
            Self::Unsupported(what) => write!(f, "暂不支持的场景特性或资产：{what}"),
            Self::OutOfRange(what) => write!(f, "场景参数越界：{what}"),
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

/// 场景 material 段的来源：`TextureStore` 没有路径概念，load 时把贴图路径
/// 登记在此，save 时据此还原 `texture` 字段（数值字段照 `MaterialDef` 现读）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MaterialRef {
    /// 基础色贴图的场景内相对路径；无贴图（白色兜底句柄 0）为 `None`。
    pub texture: Option<String>,
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

/// 场景文档根：环境段（可选）+ 实体段（必需）。
#[derive(Serialize, Deserialize, Debug)]
struct SceneDoc {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    environment: Option<EnvironmentSection>,
    /// save 恒为 `Some`；load 缺席 → [`SceneError::Missing`]。
    entities: Option<Vec<EntitySection>>,
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
/// `shadow_range` v1 不序列化（取默认 24.0，见模块文档 v1 清单）。
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
struct DirectionalLightSection {
    direction: [f32; 3],
    color: [f32; 3],
    intensity: f32,
    ambient: [f32; 3],
    shadows: bool,
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
            // shadow_range v1 不序列化：取引擎默认（模块文档 v1 清单）
            ..DirectionalLight::default()
        }
    }

    fn from_light(light: &DirectionalLight) -> Self {
        Self {
            direction: light.direction.to_array(),
            color: light.color.to_array(),
            intensity: light.intensity,
            ambient: light.ambient.to_array(),
            shadows: light.shadows,
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
    fn to_mesh_data(&self) -> Result<MeshData, SceneError> {
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
/// （引擎侧 alpha 恒为 1.0）；`texture` 为场景文件内的相对路径（PNG / JPEG）；
/// `alpha_mode` 缺省 = `Opaque`（R23 之前的旧场景文件没有该字段，
/// 反序列化回退不透明——向后兼容）；`Blend` 走透明 pass（R23）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
struct MaterialSection {
    color: [f32; 4],
    metallic: f32,
    roughness: f32,
    emissive: [f32; 3],
    texture: Option<String>,
    /// 序列化省略不透明（旧场景文件往返字节级不变，见下方断言函数）。
    #[serde(skip_serializing_if = "alpha_mode_is_opaque")]
    alpha_mode: AlphaMode,
}

impl Default for MaterialSection {
    fn default() -> Self {
        Self {
            color: [1.0, 1.0, 1.0, 1.0],
            metallic: 0.0,
            roughness: 0.5,
            emissive: [0.0; 3],
            texture: None,
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
    fn to_def(&self, texture: TextureHandle) -> MaterialDef {
        MaterialDef {
            base_color: self.color,
            emissive: [self.emissive[0], self.emissive[1], self.emissive[2], 1.0],
            metallic: self.metallic,
            roughness: self.roughness,
            texture,
            alpha_mode: self.alpha_mode,
        }
    }

    fn from_def(def: &MaterialDef, texture: Option<String>) -> Self {
        Self {
            color: def.base_color,
            metallic: def.metallic,
            roughness: def.roughness,
            emissive: [def.emissive[0], def.emissive[1], def.emissive[2]],
            texture,
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
pub fn load_scene(world: &mut World, path: impl AsRef<Path>) -> Result<Vec<Entity>, SceneError> {
    let path = path.as_ref();
    let bytes = std::fs::read(path)?;
    let doc: SceneDoc = serde_json::from_slice(&bytes)?;
    let defs = doc
        .entities
        .ok_or_else(|| SceneError::Missing("entities".into()))?;
    let base_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();

    ensure_resources(world);
    if let Some(environment) = doc.environment {
        apply_environment(world, &base_dir, environment)?;
    }

    let mut spawned = Vec::with_capacity(defs.len());
    for section in defs {
        spawned.push(spawn_section(world, &base_dir, section)?);
    }
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
    base_dir: &Path,
    environment: EnvironmentSection,
) -> Result<(), SceneError> {
    if let Some(light) = environment.directional_light {
        world.insert_resource(light.to_light());
    }
    if let Some(rel) = environment.sky_source {
        let image = load_hdr(base_dir.join(&rel))?;
        world.insert_resource(SkySource::equirect_hdr(image));
        // 登记原始路径字符串：save 时原样写出（roundtrip 稳定）
        world.resource_mut::<SceneAssetRegistry>().sky_source = Some(rel);
    }
    Ok(())
}

/// 生成单个实体：Transform 先行 spawn，其余组件按段逐一插入。
fn spawn_section(
    world: &mut World,
    base_dir: &Path,
    section: EntitySection,
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
                let data = primitive.to_mesh_data()?;
                let handle = world.resource_mut::<MeshStore>().add(data);
                world
                    .resource_mut::<SceneAssetRegistry>()
                    .meshes
                    .insert(handle.0, MeshRef::Primitive(primitive.clone()));
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
                let instance = spawn_gltf_instance(world, base_dir.join(rel))?;
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
        let texture = match &material.texture {
            Some(rel) => Some(load_texture(world, base_dir, rel)?),
            None => None,
        };
        let handle = world
            .resource_mut::<MaterialStore>()
            .add(material.to_def(texture.unwrap_or(TextureHandle(0))));
        world.resource_mut::<SceneAssetRegistry>().materials.insert(
            handle.0,
            MaterialRef {
                texture: material.texture.clone(),
            },
        );
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
        // velocity v1 不序列化：物体从静止开始（模块文档 v1 清单）
        world.insert(
            entity,
            RigidBody {
                velocity: Vec3::ZERO,
                gravity_scale: body.gravity_scale,
                friction: body.friction,
                linear_damping: body.linear_damping,
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
fn load_texture(
    world: &mut World,
    base_dir: &Path,
    rel: &str,
) -> Result<TextureHandle, SceneError> {
    let bytes = std::fs::read(base_dir.join(rel))?;
    let decoded = image::load_from_memory(&bytes)
        .map_err(|e| SceneError::Unsupported(format!("贴图解码失败（支持 PNG / JPEG）：{e}")))?;
    let rgba = decoded.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(world.resource_mut::<TextureStore>().add(CpuTexture {
        width,
        height,
        rgba: rgba.into_raw(),
    }))
}

// ---------------------------------------------------------------------------
// save
// ---------------------------------------------------------------------------

/// 把世界的内置组件集保存为 JSON 场景文件，返回写出的实体数。
///
/// - 查询 `(Transform + 可选 Name / Visibility / MeshHandle / MaterialHandle /
///   PointLight / Collider / RigidBody)` 全量写出；带 [`GltfMounted`] 标记的
///   glTF 内部实体跳过；无 `Transform` 的实体（如 UI）不参与往返；
/// - mesh / material / 天空源的来源按 [`SceneAssetRegistry`] 登记还原；
///   非场景来源的句柄按模块文档的降级规则省略并 `log::warn`；
/// - 实体顺序 = 组件存储序（= spawn 顺序），稳定；`save(load(f))` 再
///   `save` 一次产出逐字节相同的文件（同输入确定性输出）。
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
            "save_scene：{lost_textures} 个材质的贴图无来源路径登记 \
             （TextureStore 无路径概念，非场景加载的贴图无法回写），texture 字段已省略"
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

    let count = sections.len();
    let doc = SceneDoc {
        environment: has_environment.then_some(environment),
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

    // material：句柄 0 = 默认材质（隐式，整段省略）；登记表还原贴图路径，
    // 数值字段照 MaterialDef 现读（保存「当前值」而非「加载时的值」）
    if let Some(handle) = world.get::<MaterialHandle>(entity) {
        if handle.0 != 0 {
            if let Some(def) = materials.and_then(|m| m.get(*handle)) {
                let texture = registry
                    .materials
                    .get(&handle.0)
                    .and_then(|m| m.texture.clone());
                if def.texture.0 != 0 && texture.is_none() {
                    *lost_textures += 1;
                }
                section.material = Some(MaterialSection::from_def(def, texture));
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
}
