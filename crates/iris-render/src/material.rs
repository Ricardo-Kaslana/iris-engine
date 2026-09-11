//! 材质：PBR 金属度/粗糙度工作流（Cook-Torrance）、自发光、纹理引用、
//! 法线 / ORM 数据贴图槽位（R42）与透明模式（alpha 混合走独立透明
//! pass，见 `renderer.rs`）。
//!
//! # 法线 / ORM 槽位与逐位一致性（R42）
//!
//!
//! [`MaterialDef::normal_texture`]（切线空间法线，**线性**采样）与
//! [`MaterialDef::orm_texture`]（ORM 打包：R = AO、G = 粗糙度、
//! B = 金属度，**线性**采样）缺省为 `None`。渲染器对 `None` 槽位绑定
//! **常驻中性 1×1 纹理**（法线 = (0.5, 0.5, 1)、ORM = (1, 1, 1)，均为
//! 线性格式），着色器**无任何按材质分支**——不带这两张贴图的材质与
//! R41 之前（无此二槽位）的渲染结果**逐位一致**，论证链如下：
//!
//! 1. 中性法线纹理用 `Rgba16Float` 存储（0.5 与 1.0 在 binary16 中精确
//!    表示；8-bit unorm 无法表示 0.5——128/255 ≈ 0.502，*2−1 后残留
//!    0.0039 的扰动）。着色器采样后 `*2−1` 恰得 (0, 0, 1)；
//! 2. 扰动法线按 `T·nx + B·ny + N·nz` 计算（TBN 的第三列取**未归一化**
//!    的插值世界法线）：代入 (0, 0, 1) 得 `0·T + 0·B + 1·N = N`——
//!    浮点下 `x·0 = ±0` 且 `±0 + x = x`，逐位等于 R41 直用的插值法线；
//! 3. 之后的 `normalize` 输入与 R41 同一串位模式，输出逐位相同；
//! 4. 中性 ORM 采样值恰为 (1, 1, 1)：`metallic = clamp(标量 × 1.0)`、
//!    `roughness = clamp(标量 × 1.0)`（`x·1.0 = x` 精确）、`AO × 1.0`
//!    乘在环境项上（`x·1.0 = x` 精确）——全部与 R41 标量路径逐位相同。
//!
//! 该不变量由 GPU 集成测试 `normal_render.rs` 的 R41 基线像素哈希锁定。
//!
//! # 逐像素表面参数
//!
//! roughness / metallic 从标量 uniform 升级为「**标量倍率 × 贴图采样**」：
//! `roughness = clamp(roughness 标量 × orm.g)`、`metallic = clamp(metallic
//! 标量 × orm.b)`——与 glTF 规范的 factor × texel 语义一致；无 ORM 贴图
//! 时采样值为 1，退化为纯标量（见上）。AO 只乘环境项（IBL + 常量补光），
//! 不乘直接光。
//!
//! # 预积分皮肤 SSS（R63）
//!
//! [`MaterialDef::subsurface_strength`] / [`MaterialDef::subsurface_scale`]
//! 是 R63 新增的**直射漫反射**包裹参数，缺省 `0.0 / 0.1`。**兼容铁律**：
//! 强度缺省 0 时着色器的 SSS 分支（`if (material.sss.x > 0.0)`，uniform
//! 条件）整段不进入——不比 `x·1.0 + y·0.0`，而是**根本不执行任何算术**
//! （R52c `select` 先例的强化形态：「把逐位不变交给语言语义，而不是浮点
//! 恒等式」）。故全部既有材质（`..MaterialDef::default()` 构造）渲染结果
//! 与 R62b **逐位一致**，由既有 GPU FNV 基线原值通过看门。

use crate::texture::TextureHandle;

/// 材质句柄，指向 `MaterialStore` 中的条目，也作为 ECS 组件挂载到实体上。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, iris_core::Component)]
pub struct MaterialHandle(pub u32);

/// 材质透明模式。
///
/// - `Opaque`（默认）：走不透明主 pass——REPLACE 写入 + 深度写入，
///   参与阴影投射；
/// - `Blend`：走透明 pass——标准 alpha 混合（`src α, 1 − src α`）+
///   深度只测不写，按视深降序（远 → 近）绘制，**不投影**。
///
/// 混合系数取 `base_color.a`（与着色器输出 alpha 一致）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AlphaMode {
    #[default]
    Opaque,
    Blend,
}

/// 材质定义（CPU 侧）。GPU 侧 uniform 由渲染器按需创建。
#[derive(Clone, Debug)]
pub struct MaterialDef {
    /// 基础色（线性空间，RGB + 透明度）；金属度 1 时作为 F0 反射率。
    pub base_color: [f32; 4],
    /// 自发光（叠加在光照结果上，不受光照与阴影影响）。
    pub emissive: [f32; 4],
    /// 金属度 0（绝缘体）..1（金属），Cook-Torrance 工作流。
    pub metallic: f32,
    /// 粗糙度 0（镜面）..1（完全粗糙）。
    pub roughness: f32,
    /// 基础色贴图（线性采样，值与 base_color 相乘）。`TextureHandle(0)` 为白色兜底。
    pub texture: TextureHandle,
    /// 切线空间法线贴图（**线性**数据贴图，R42）。`None` 时渲染器绑定
    /// 中性 (0.5, 0.5, 1) 纹理——法线扰动为恒等变换，见模块文档的逐位
    /// 一致性论证。网格无顶点切线时着色器在屏幕空间推导 TBN。
    pub normal_texture: Option<TextureHandle>,
    /// ORM 打包贴图（**线性**数据贴图，R42）：R = AO、G = 粗糙度、
    /// B = 金属度（glTF metallicRoughness 的 G/B 约定 + glTF occlusion
    /// 的 R 约定共用一张打包图；加载器把 occlusion 合并进 R，见
    /// `asset/gltf.rs`）。`None` 时渲染器绑定中性 (1, 1, 1) 纹理，
    /// 表面参数退化为标量直通，见模块文档。
    pub orm_texture: Option<TextureHandle>,
    /// 透明模式：`Opaque` 走不透明主 pass，`Blend` 走透明 pass（见 `AlphaMode`）。
    pub alpha_mode: AlphaMode,
    /// 预积分皮肤次表面散射强度（R63，0..1）。**缺省 0 = 关闭**：直射
    /// 漫反射与引入本字段前**逐位一致**（着色器整段分支不进入，无任何
    /// 算术）；1 = 漫反射的 NdotL 完全取预积分 LUT 值（光包裹进终结线
    /// 背光侧）。中间值线性混合。只在直射漫反射项生效——镜面、IBL /
    /// 光照贴图间接项、自发光都不受影响；金属（metallic = 1）经
    /// `kd = (1 − F)(1 − metallic)` 权重自动归零。见 `crate::sss`。
    pub subsurface_strength: f32,
    /// 预积分 SSS 的曲率尺度半径（R63，世界单位 / 米）：**半径越小（越
    /// 弯曲）包裹越强**，映射 `t = clamp(0.05 / scale, 0, 1)`（见
    /// [`crate::sss::wrap_param`] 的调参表）。缺省 0.1 m = 头 / 前臂
    /// 量级（t = 0.5）。只在 `subsurface_strength > 0` 时被消费。
    pub subsurface_scale: f32,
}

impl Default for MaterialDef {
    fn default() -> Self {
        Self {
            base_color: [1.0, 1.0, 1.0, 1.0],
            emissive: [0.0, 0.0, 0.0, 1.0],
            metallic: 0.0,
            roughness: 0.5,
            texture: TextureHandle(0),
            normal_texture: None,
            orm_texture: None,
            alpha_mode: AlphaMode::Opaque,
            subsurface_strength: 0.0,
            subsurface_scale: crate::sss::SSS_DEFAULT_SCALE,
        }
    }
}

impl MaterialDef {
    /// 纯色无贴图材质的便捷构造（绝缘体、中等粗糙度、不透明）。
    pub fn solid(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self {
            base_color: [r, g, b, a],
            ..Default::default()
        }
    }

    /// 金属材质的便捷构造（金属度 1，F0 取基础色）。
    pub fn metal(r: f32, g: f32, b: f32, roughness: f32) -> Self {
        Self {
            base_color: [r, g, b, 1.0],
            metallic: 1.0,
            roughness: roughness.clamp(0.0, 1.0),
            ..Default::default()
        }
    }

    /// 玻璃类透明材质的便捷构造（alpha 混合，`a` 为混合系数，
    /// 低粗糙度绝缘体）。走透明 pass：按视深排序、深度只测不写、不投影。
    pub fn glass(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self {
            base_color: [r, g, b, a],
            roughness: 0.08,
            alpha_mode: AlphaMode::Blend,
            ..Default::default()
        }
    }

    /// 挂法线贴图（builder 风格，R42）。`tex` 必须是线性数据贴图
    /// （`CpuTexture.srgb == false`），切线空间编码。
    pub fn with_normal(mut self, tex: TextureHandle) -> Self {
        self.normal_texture = Some(tex);
        self
    }

    /// 挂 ORM 打包贴图（builder 风格，R42）。`tex` 必须是线性数据贴图
    /// （`CpuTexture.srgb == false`），R = AO / G = 粗糙度 / B = 金属度。
    pub fn with_orm(mut self, tex: TextureHandle) -> Self {
        self.orm_texture = Some(tex);
        self
    }

    /// 开启预积分皮肤 SSS（builder 风格，R63）。`strength` 自动 clamp 到
    /// [0, 1]，`scale` 为曲率尺度半径（米，越小包裹越强——见
    /// [`crate::sss::wrap_param`]）。
    pub fn with_subsurface(mut self, strength: f32, scale: f32) -> Self {
        self.subsurface_strength = strength.clamp(0.0, 1.0);
        self.subsurface_scale = scale;
        self
    }
}

/// CPU 侧材质目录（World 资源）。0 号槽位固定为默认材质。
#[derive(iris_core::Resource)]
pub struct MaterialStore {
    materials: Vec<MaterialDef>,
}

impl Default for MaterialStore {
    fn default() -> Self {
        Self {
            materials: vec![MaterialDef::default()],
        }
    }
}

impl MaterialStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册材质；新句柄从 1 开始（0 为默认材质）。
    pub fn add(&mut self, def: MaterialDef) -> MaterialHandle {
        self.materials.push(def);
        MaterialHandle(self.materials.len() as u32 - 1)
    }

    pub fn get(&self, handle: MaterialHandle) -> Option<&MaterialDef> {
        self.materials.get(handle.0 as usize)
    }

    /// 已注册材质数（含 0 号默认材质）。
    pub fn len(&self) -> usize {
        self.materials.len()
    }

    pub fn is_empty(&self) -> bool {
        self.materials.is_empty()
    }

    /// 目录内容只读视图（下标 = 句柄号）。
    pub fn contents(&self) -> &[MaterialDef] {
        &self.materials
    }

    /// 整体替换目录内容（下标 = 句柄号重新按序分配）。
    ///
    /// **安全前提在调用方**：替换后，旧内容中未保留的句柄不得再被任何
    /// 实体引用（当前唯一消费者是 iris-editor 的 undo 快照恢复，见
    /// `MeshStore::set_contents` 同款论证）。
    pub fn set_contents(&mut self, materials: Vec<MaterialDef>) {
        self.materials = materials;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_handle_is_default_material() {
        let mut store = MaterialStore::new();
        assert_eq!(
            store.get(MaterialHandle(0)).unwrap().base_color,
            [1.0, 1.0, 1.0, 1.0]
        );
        let handle = store.add(MaterialDef::solid(1.0, 0.0, 0.0, 1.0));
        assert_eq!(handle, MaterialHandle(1));
        assert_eq!(store.get(handle).unwrap().base_color, [1.0, 0.0, 0.0, 1.0]);
        assert!(store.get(MaterialHandle(99)).is_none());
    }

    /// 便捷构造的透明模式：solid / metal / default 恒不透明，
    /// glass 为 Blend 且混合系数取 alpha 分量。
    #[test]
    fn convenience_constructors_set_alpha_mode() {
        assert_eq!(MaterialDef::default().alpha_mode, AlphaMode::Opaque);
        assert_eq!(
            MaterialDef::solid(1.0, 1.0, 1.0, 0.5).alpha_mode,
            AlphaMode::Opaque,
            "solid 即便 alpha < 1 也保持不透明（分区只看 alpha_mode）"
        );
        assert_eq!(
            MaterialDef::metal(1.0, 1.0, 1.0, 0.2).alpha_mode,
            AlphaMode::Opaque
        );
        let glass = MaterialDef::glass(0.6, 0.8, 1.0, 0.35);
        assert_eq!(glass.alpha_mode, AlphaMode::Blend);
        assert_eq!(glass.base_color, [0.6, 0.8, 1.0, 0.35]);
    }

    /// 法线 / ORM 槽位（R42）：默认缺省 None（中性纹理路径 = 逐位一致
    /// 兼容），builder 链式挂载后槽位就位、其余字段不受影响。
    #[test]
    fn normal_and_orm_slots_default_none_and_builder_sets() {
        assert_eq!(MaterialDef::default().normal_texture, None);
        assert_eq!(MaterialDef::default().orm_texture, None);
        let def = MaterialDef::metal(0.9, 0.85, 0.8, 0.35)
            .with_normal(TextureHandle(7))
            .with_orm(TextureHandle(9));
        assert_eq!(def.normal_texture, Some(TextureHandle(7)));
        assert_eq!(def.orm_texture, Some(TextureHandle(9)));
        assert_eq!(def.metallic, 1.0, "builder 不得改写其他字段");
        assert_eq!(def.roughness, 0.35);
    }

    /// 预积分 SSS 参数（R63）：缺省 = 关闭（强度 0）+ 头 / 前臂量级曲率
    /// 尺度；builder 链式设定且强度自动 clamp（越界输入不产生未定义效果）。
    #[test]
    fn subsurface_defaults_off_and_builder_clamps() {
        let def = MaterialDef::default();
        assert_eq!(def.subsurface_strength, 0.0, "缺省 0 = 关闭（逐位兼容）");
        assert_eq!(def.subsurface_scale, crate::sss::SSS_DEFAULT_SCALE);
        assert_eq!(crate::sss::wrap_param(def.subsurface_scale), 0.5);
        // 便捷构造器不携带 SSS（solid / metal / glass 全部关闭）
        for d in [
            MaterialDef::solid(1.0, 0.5, 0.4, 1.0),
            MaterialDef::metal(1.0, 1.0, 1.0, 0.3),
            MaterialDef::glass(1.0, 1.0, 1.0, 0.4),
        ] {
            assert_eq!(d.subsurface_strength, 0.0);
        }
        let skin = MaterialDef::solid(0.92, 0.72, 0.62, 1.0).with_subsurface(0.85, 0.1);
        assert_eq!(skin.subsurface_strength, 0.85);
        assert_eq!(skin.subsurface_scale, 0.1);
        assert_eq!(
            MaterialDef::solid(1.0, 1.0, 1.0, 1.0)
                .with_subsurface(3.0, 0.1)
                .subsurface_strength,
            1.0,
            "强度越界必须 clamp 到 1"
        );
        assert_eq!(
            MaterialDef::solid(1.0, 1.0, 1.0, 1.0)
                .with_subsurface(-2.0, 0.1)
                .subsurface_strength,
            0.0,
            "负强度必须 clamp 到 0（= 关闭，绝不产生负光）"
        );
    }
}
