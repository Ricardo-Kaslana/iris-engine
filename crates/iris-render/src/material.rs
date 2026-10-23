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
//!
//! # 受控材质扩展
//!
//! [`MaterialExtensions`] 提供两个引擎白名单内的开关：顶点色消费方式
//! （`Ignore` / `Multiply`）与着色模式（`Lit` / `Unlit`）。两个字段
//! 缺省关闭，材质 uniform 的扩展槽写零，着色器分支整段不进入。该入口
//! 不是任意 WGSL 注入点；新增扩展模式必须改引擎并补像素测试。
//!
//! # 材质修订号
//!
//! [`MaterialStore`] 为每个材质槽位维护修订号。`get_mut` 与
//! `set_contents` 推进修订号，渲染器据此在下一帧重建材质绑定组。
//! 默认场景没有修改，缓存命中行为与引入修订号之前一致。

use crate::texture::TextureHandle;

/// 材质句柄，指向 `MaterialStore` 中的条目，也作为 ECS 组件挂载到实体上。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, iris_core::Component)]
pub struct MaterialHandle(pub u32);

/// 材质透明模式。
///
/// - `Opaque`（默认）：走不透明主 pass——REPLACE 写入 + 深度写入，
///   参与阴影投射；
/// - `Blend`：走透明 pass——标准 alpha 混合（`src α, 1 − src α`）+
///   深度只测不写，按视深降序（远 → 近）绘制，**不投影**；
/// - `Mask`（R90）：alpha cutout——仍走不透明分区（深度写、批处理键、
///   剔除语义与 `Opaque` 完全一致），片元侧在着色前按
///   [`MaterialDef::alpha_cutoff`] 逐像素 `discard`（glTF `alphaMode:
///   "MASK"` 的引擎等价物）。**阴影为实心近似**：阴影深度 pass 不感知
///   alpha（cutout 孔洞在投影中闭合）——alpha 感知阴影与发带 / 壳层
///   毛发阴影一起留待 R91（诚实限制 §8）。发带 / 壳层管线独立于实例
///   区间绘制，天然不投影。
///
/// 混合系数取 `base_color.a`（与着色器输出 alpha 一致）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AlphaMode {
    #[default]
    Opaque,
    Blend,
    Mask,
}

/// Kajiya-Kay 各向异性毛发着色参数（R90；R96 增高光染色）。
///
/// 发丝方向取顶点切线（发带管线的 VS 写入 `VertexOutput.tangent.xyz`，
/// 普通网格则取 glTF tangent——两条通路同一消费点）。模型边界（声明，
/// 详见 `kajiya_kay.rs` 模块文档）：
///
/// - 漫反射 = `max(0, sin(T·L))`（KK 1989 原式，无 Scheuermann 根梢
///   混合）；
/// - 高光 = `pow(sin(T'·H), exponent)`，`T' = normalize(T + N·shift·(2t−1))`
///   沿伪法线偏移切线，`t` = 沿发丝参数（UV.x），产生根梢高光移位；
/// - 高光颜色 = `mix(白, albedo, 0.5) × highlight_tint`——固定式之外的
///   唯一旋钮是本染色乘子（R89：有非默认断言看管，见 `kajiya_kay.rs`
///   的 `highlight_color` 测试）。缺省 `[1, 1, 1]` = 与 R90 固定式**逐位
///   一致**（IEEE ×1.0 精确，无分支无重排）。动机：固定式在近黑毛发
///   （albedo ≈ 0.06）上把高光钳到 ≈0.53 灰白，实机目检读作漂白；染色
///   乘子让深色毛发把高光压回发色域（如 `[0.4, 0.42, 0.55]` 的冷灰蓝）；
/// - 伪法线 = 视线在垂直于 T 平面上的投影（片元侧重算），保证
///   NdotV/NdotL 域良好，同时供 IBL / 探针间接项消费。
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct KajiyaKay {
    /// 切线沿伪法线的偏移幅度（根梢高光移位）。0 = 不移位；典型
    /// |shift| ∈ [0.05, 0.3]。符号翻转 = 高光带反向倾斜。
    pub shift: f32,
    /// 高光锐度指数（越大高光带越窄）。典型 ∈ [20, 200]。
    pub exponent: f32,
    /// 高光染色乘子（线性 RGB，逐分量乘在高光色上）。缺省
    /// `[1, 1, 1]` = R90 固定式逐位不变；反序列化旧场景文件（无此字段）
    /// 时按缺省补齐（`serde(default)`）。直射光（方向光 + 点光 + 锥光）
    /// 的 KK 瓣统一消费；间接项不经过高光色，不受影响。
    #[serde(default = "KajiyaKay::default_highlight_tint")]
    pub highlight_tint: [f32; 3],
}

impl KajiyaKay {
    /// 缺省高光染色 = 恒等乘子（serde 缺省入口 + CPU 镜像的解析锚）。
    pub fn default_highlight_tint() -> [f32; 3] {
        [1.0, 1.0, 1.0]
    }

    /// 以缺省染色构造（R90 字面量补齐字段的最短路径；shift / exponent
    /// 逐字段直传）。
    pub fn new(shift: f32, exponent: f32) -> Self {
        Self {
            shift,
            exponent,
            highlight_tint: Self::default_highlight_tint(),
        }
    }
}

/// 顶点色消费方式（受控材质扩展的一部分）。
///
/// - `Ignore`（默认）：网格即使携带顶点色也不参与着色。着色器分支整段
///   不执行，默认路径与引入顶点色之前逐位一致。
/// - `Multiply`：片元把顶点色的 RGB 乘到反照率上。颜色是线性乘子，
///   255 为恒等；网格缺席颜色时渲染器绑定白色垫，因此该模式对无颜色
///   网格是空操作。alpha 通道当前不参与任何计算。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VertexColorMode {
    #[default]
    Ignore,
    Multiply,
}

/// 着色模式（受控材质扩展的一部分）。
///
/// `Lit`（默认）走完整 PBR 着色。`Unlit` 在 alpha cutout 之后提前返回
/// “反照率与自发光”，跳过直射光、IBL、光照贴图、探针、SSR、AO、天气、
/// SSS 与 Kajiya-Kay。透明模式、OIT 深度测试、场景深度、阴影投射、雾
/// 与后处理链不受影响。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShadingMode {
    #[default]
    Lit,
    Unlit,
}

/// 材质受控扩展入口：引擎白名单内的功能开关，不是任意 WGSL 注入点。
///
/// 字段全部关闭时，材质 uniform 的扩展槽写零，着色器分支不进入，渲染
/// 结果与引入扩展之前逐位一致。场景格式在整组缺省时省略本字段，旧场景
/// 反序列化回落到默认值。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct MaterialExtensions {
    /// 顶点色消费方式（见 [`VertexColorMode`]）。
    pub vertex_color: VertexColorMode,
    /// 着色模式（见 [`ShadingMode`]）。
    pub shading: ShadingMode,
    /// 像素画放大吸附：采样足迹小于一个纹素时，把基础色 UV 吸附到
    /// 纹素中心（显式传入原梯度，纹理缩小仍走采样器的 mip 与各向异性）。
    /// 用于「放大要硬边、缩小要稳定」的像素画材质——最近邻放大与各向
    /// 异性过滤在 WebGPU 里不能共存，本开关用着色器补回放大侧的硬边。
    pub pixel_snap: bool,
}

impl MaterialExtensions {
    /// 整组是否等于缺省值（场景序列化的省略条件）。
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// 写入材质 uniform 扩展槽的旗标：x = 顶点色乘入，y = unlit，
    /// z = 像素画放大吸附，w 保留。
    pub(crate) fn shader_flags(self) -> [f32; 4] {
        [
            f32::from(self.vertex_color == VertexColorMode::Multiply),
            f32::from(self.shading == ShadingMode::Unlit),
            f32::from(self.pixel_snap),
            0.0,
        ]
    }
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
    /// 双面材质（glTF `doubleSided`，R66d）：`true` = 关闭背面剔除
    /// （管线 `cull_mode: None` 变体），发片 / 叶片 / 薄片类资产的两个
    /// 朝向都可见；`false`（缺省）= 维持背面剔除（`Cull Back`）。
    ///
    /// **兼容形态**：缺省 false 的材质走与引入本字段前**同一条管线**
    /// （剔除状态是管线级而非着色器级，无逐像素差异）——既有场景与
    /// FNV 基线逐位不变；glTF 加载器按规范直读该字段
    /// （见 `asset/gltf.rs`），引擎原生资产不声明即单面。
    pub double_sided: bool,
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
    /// alpha cutout 阈值（R90，glTF `alphaCutoff` 语义）：`alpha_mode =
    /// Mask` 时，着色前 `base.a < alpha_cutoff` 的片元整段丢弃。缺省
    /// 0.5 = glTF 规范默认值；`Opaque` / `Blend` 模式下**不消费**该字段
    /// （着色器 cutoff 分支不进入）。
    pub alpha_cutoff: f32,
    /// Kajiya-Kay 各向异性着色参数（R90）。`None`（缺省）= 标准
    /// Cook-Torrance PBR——着色器 KK 分支整段不执行，既有像素逐位不变；
    /// `Some` = 直射光（方向光 + 点光 + 聚光）换 KK 瓣，间接项（IBL /
    /// 探针 / 光照贴图）仍走各向同性伪法线路径。见 [`KajiyaKay`]。
    pub kajiya_kay: Option<KajiyaKay>,
    /// 受控材质扩展（本轮）：顶点色消费方式与着色模式。缺省全部关闭，
    /// 与引入该字段之前逐位一致。见 [`MaterialExtensions`]。
    pub extensions: MaterialExtensions,
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
            double_sided: false,
            subsurface_strength: 0.0,
            subsurface_scale: crate::sss::SSS_DEFAULT_SCALE,
            alpha_cutoff: 0.5,
            kajiya_kay: None,
            extensions: MaterialExtensions::default(),
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

    /// 开启双面（builder 风格，R66d）：关闭背面剔除，发片 / 薄片类资产
    /// 两朝向可见（glTF `doubleSided: true` 的引擎侧等价物）。
    pub fn with_double_sided(mut self) -> Self {
        self.double_sided = true;
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

    /// 开启 alpha cutout（builder 风格，R90）：`alpha_mode` 置 `Mask`，
    /// 片元 `base.a < cutoff` 整段丢弃。`cutoff` 自动 clamp 到 [0, 1]
    /// （0 = 恒不裁、1 = 全透明贴图裁光）。分区 / 深度 / 批处理语义与
    /// `Opaque` 完全一致（不透明段），只是多了逐像素 discard。
    pub fn with_alpha_mask(mut self, cutoff: f32) -> Self {
        self.alpha_mode = AlphaMode::Mask;
        self.alpha_cutoff = cutoff.clamp(0.0, 1.0);
        self
    }

    /// 开启 Kajiya-Kay 各向异性毛发着色（builder 风格，R90）。发带 /
    /// 毛发材质用；参数语义见 [`KajiyaKay`]。
    pub fn with_kajiya_kay(mut self, params: KajiyaKay) -> Self {
        self.kajiya_kay = Some(params);
        self
    }

    /// 设置顶点色消费方式（builder 风格）。`Multiply` 要求网格侧提供
    /// `MeshData::colors`；网格缺席颜色时渲染器绑白色垫，乘性恒等。
    pub fn with_vertex_color(mut self, mode: VertexColorMode) -> Self {
        self.extensions.vertex_color = mode;
        self
    }

    /// 切换为无光照输出（builder 风格）：反照率与自发光直接出图，跳过
    /// 全部场景光照与间接项。见 [`ShadingMode::Unlit`]。
    pub fn with_unlit(mut self) -> Self {
        self.extensions.shading = ShadingMode::Unlit;
        self
    }

    /// 开启像素画放大吸附（builder 风格）。与各向异性过滤配合时，
    /// 放大保持纹素硬边、缩小保持稳定，见
    /// [`MaterialExtensions::pixel_snap`]。
    pub fn with_pixel_snap(mut self) -> Self {
        self.extensions.pixel_snap = true;
        self
    }
}

/// CPU 侧材质目录（World 资源）。0 号槽位固定为默认材质。
///
/// # 修订号
///
/// 每个材质槽位记录最近一次变更的修订号，目录维护单调推进的 `epoch`。
/// `add` 为新材质分配新修订号；`get_mut` 推进被编辑材质的修订号；
/// `set_contents` 为全部材质分配新修订号。渲染器按修订号决定是否重建
/// 该材质的 GPU 绑定组，因此运行期修改数值字段或扩展开关会在下一帧
/// 生效。修订号只用于失效判断，不进入任何着色计算。
#[derive(iris_core::Resource)]
pub struct MaterialStore {
    materials: Vec<MaterialDef>,
    revisions: Vec<u64>,
    epoch: u64,
}

impl Default for MaterialStore {
    fn default() -> Self {
        Self {
            materials: vec![MaterialDef::default()],
            revisions: vec![1],
            epoch: 1,
        }
    }
}

impl MaterialStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 推进目录纪元并返回新值。
    fn next_epoch(&mut self) -> u64 {
        self.epoch = self.epoch.wrapping_add(1);
        self.epoch
    }

    /// 注册材质；新句柄从 1 开始（0 为默认材质）。
    pub fn add(&mut self, def: MaterialDef) -> MaterialHandle {
        let revision = self.next_epoch();
        self.materials.push(def);
        self.revisions.push(revision);
        MaterialHandle(self.materials.len() as u32 - 1)
    }

    pub fn get(&self, handle: MaterialHandle) -> Option<&MaterialDef> {
        self.materials.get(handle.0 as usize)
    }

    /// 可变视图（C5b 热重载局部的写入口）：就地改既有句柄的数值参数或
    /// 扩展开关。本次调用推进该材质的修订号，渲染器在下一帧重建它的
    /// 绑定组并重新读取全部字段。纹理像素与网格顶点等内容级替换仍走
    /// 各自的目录修订号（见 ARCHITECTURE §4.9 / §8.4）。
    pub fn get_mut(&mut self, handle: MaterialHandle) -> Option<&mut MaterialDef> {
        let index = handle.0 as usize;
        if index >= self.materials.len() {
            return None;
        }
        let revision = self.next_epoch();
        self.revisions[index] = revision;
        self.materials.get_mut(index)
    }

    /// 材质槽位的当前修订号；无效句柄返回 `None`。
    pub fn revision(&self, handle: MaterialHandle) -> Option<u64> {
        self.revisions.get(handle.0 as usize).copied()
    }

    /// 当前目录纪元。任何 `add`、`get_mut` 与 `set_contents` 都会推进它。
    pub fn epoch(&self) -> u64 {
        self.epoch
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

    /// 整体替换目录内容（下标 = 句柄号重新按序分配），并为全部材质分配
    /// 新修订号，强制渲染器丢弃旧绑定组。
    ///
    /// **安全前提在调用方**：替换后，旧内容中未保留的句柄不得再被任何
    /// 实体引用（当前消费者是 iris-editor 的 undo 快照恢复；网格侧的
    /// 同类快照用 `MeshStore::restore_from`，同样会刷新修订号）。
    pub fn set_contents(&mut self, materials: Vec<MaterialDef>) {
        let revision = self.next_epoch();
        self.revisions = vec![revision; materials.len()];
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

    /// 双面标志（R66d）：缺省与全部便捷构造器恒 false（单面 = 既有背面
    /// 剔除行为，逐位兼容）；builder 开启且不改写其他字段。
    #[test]
    fn double_sided_defaults_false_and_builder_sets() {
        assert!(!MaterialDef::default().double_sided);
        for d in [
            MaterialDef::solid(1.0, 1.0, 1.0, 0.5),
            MaterialDef::metal(1.0, 1.0, 1.0, 0.2),
            MaterialDef::glass(0.6, 0.8, 1.0, 0.35),
        ] {
            assert!(!d.double_sided, "便捷构造器不携带双面");
        }
        let def = MaterialDef::glass(0.6, 0.8, 1.0, 0.35).with_double_sided();
        assert!(def.double_sided);
        assert_eq!(def.alpha_mode, AlphaMode::Blend, "builder 不得改写其他字段");
        assert_eq!(def.roughness, 0.08);
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

    /// alpha cutout（R90）：缺省 cutoff = 0.5（glTF 规范默认）但不被
    /// Opaque/Blend 消费；builder 置 Mask 且 cutoff clamp 到 [0,1]；
    /// 全部便捷构造器保持 Opaque（Mask 只能显式开启）。
    #[test]
    fn alpha_mask_defaults_and_builder_clamps() {
        let def = MaterialDef::default();
        assert_eq!(def.alpha_mode, AlphaMode::Opaque);
        assert_eq!(def.alpha_cutoff, 0.5, "缺省 cutoff = glTF 规范默认");
        for d in [
            MaterialDef::solid(1.0, 1.0, 1.0, 0.5),
            MaterialDef::metal(1.0, 1.0, 1.0, 0.2),
            MaterialDef::glass(0.6, 0.8, 1.0, 0.35),
        ] {
            assert_ne!(d.alpha_mode, AlphaMode::Mask, "便捷构造器不携带 Mask");
        }
        let masked = MaterialDef::solid(0.2, 0.5, 0.15, 1.0).with_alpha_mask(0.66);
        assert_eq!(masked.alpha_mode, AlphaMode::Mask);
        assert_eq!(masked.alpha_cutoff, 0.66);
        assert_eq!(
            MaterialDef::solid(1.0, 1.0, 1.0, 1.0)
                .with_alpha_mask(1.7)
                .alpha_cutoff,
            1.0,
            "cutoff 越界必须 clamp 到 1（全透明贴图裁光）"
        );
        assert_eq!(
            MaterialDef::solid(1.0, 1.0, 1.0, 1.0)
                .with_alpha_mask(-0.3)
                .alpha_cutoff,
            0.0,
            "负 cutoff 必须 clamp 到 0（恒不裁 = Mask 的空操作）"
        );
    }

    /// Kajiya-Kay 参数（R90）：缺省 None（标准 PBR，着色器分支整段不进）；
    /// builder 链式设定且不改写其他字段；全部便捷构造器不携带 KK。
    #[test]
    fn kajiya_kay_defaults_none_and_builder_sets() {
        assert_eq!(MaterialDef::default().kajiya_kay, None);
        for d in [
            MaterialDef::solid(0.1, 0.08, 0.05, 1.0),
            MaterialDef::metal(1.0, 1.0, 1.0, 0.2),
            MaterialDef::glass(0.6, 0.8, 1.0, 0.35),
        ] {
            assert_eq!(d.kajiya_kay, None, "便捷构造器不携带 KK");
        }
        let hair =
            MaterialDef::solid(0.12, 0.09, 0.06, 1.0).with_kajiya_kay(KajiyaKay::new(0.15, 96.0));
        assert_eq!(hair.kajiya_kay, Some(KajiyaKay::new(0.15, 96.0)));
        assert_eq!(
            hair.alpha_mode,
            AlphaMode::Opaque,
            "builder 不得改写其他字段"
        );
        assert_eq!(hair.roughness, 0.5);
    }

    /// 受控材质扩展：整组缺省关闭；两个 builder 只改目标字段；uniform
    /// 旗标按枚举映射为 0/1，缺省全零保证着色器分支不进入。
    #[test]
    fn extensions_default_off_and_builders_map_flags() {
        let def = MaterialDef::default();
        assert_eq!(def.extensions, MaterialExtensions::default());
        assert!(def.extensions.is_default());
        assert_eq!(def.extensions.vertex_color, VertexColorMode::Ignore);
        assert_eq!(def.extensions.shading, ShadingMode::Lit);
        assert_eq!(def.extensions.shader_flags(), [0.0; 4]);

        let colored =
            MaterialDef::solid(0.8, 0.8, 0.8, 1.0).with_vertex_color(VertexColorMode::Multiply);
        assert_eq!(colored.extensions.vertex_color, VertexColorMode::Multiply);
        assert_eq!(colored.extensions.shading, ShadingMode::Lit);
        assert_eq!(colored.extensions.shader_flags(), [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(
            colored.base_color,
            [0.8, 0.8, 0.8, 1.0],
            "builder 不改其他字段"
        );

        let unlit = MaterialDef::solid(0.3, 0.4, 0.5, 1.0).with_unlit();
        assert_eq!(unlit.extensions.shading, ShadingMode::Unlit);
        assert_eq!(unlit.extensions.vertex_color, VertexColorMode::Ignore);
        assert_eq!(unlit.extensions.shader_flags(), [0.0, 1.0, 0.0, 0.0]);

        let both = MaterialDef::solid(1.0, 1.0, 1.0, 1.0)
            .with_vertex_color(VertexColorMode::Multiply)
            .with_unlit();
        assert_eq!(both.extensions.shader_flags(), [1.0, 1.0, 0.0, 0.0]);
        assert!(!both.extensions.is_default());

        // 像素画吸附单独占 z 位，且不影响其他旗标。
        let snapped = MaterialDef::solid(1.0, 1.0, 1.0, 1.0).with_pixel_snap();
        assert!(snapped.extensions.pixel_snap);
        assert_eq!(snapped.extensions.shader_flags(), [0.0, 0.0, 1.0, 0.0]);
        assert!(!snapped.extensions.is_default());
    }

    /// 修订号：`add` 给新材质分配新值且不扰动既有材质；`get_mut` 只推进
    /// 被编辑材质；`set_contents` 为全部材质刷新。无效句柄不推进纪元。
    #[test]
    fn material_store_revisions_track_edits() {
        let mut store = MaterialStore::new();
        assert_eq!(store.epoch(), 1);
        assert_eq!(store.revision(MaterialHandle(0)), Some(1));
        let a = store.add(MaterialDef::solid(1.0, 0.0, 0.0, 1.0));
        let b = store.add(MaterialDef::solid(0.0, 1.0, 0.0, 1.0));
        assert_eq!((a, b), (MaterialHandle(1), MaterialHandle(2)));
        assert_eq!(store.revision(a), Some(2));
        assert_eq!(store.revision(b), Some(3));
        assert_eq!(
            store.revision(MaterialHandle(0)),
            Some(1),
            "新材质不得改写既有修订号"
        );
        assert_eq!(store.epoch(), 3);

        store.get_mut(a).unwrap().roughness = 0.1;
        assert_eq!(store.revision(a), Some(4), "get_mut 推进被编辑材质");
        assert_eq!(store.revision(b), Some(3), "未编辑材质保持不变");
        assert_eq!(store.revision(MaterialHandle(0)), Some(1));
        assert_eq!(store.epoch(), 4);

        assert!(store.get_mut(MaterialHandle(99)).is_none());
        assert_eq!(store.epoch(), 4, "无效句柄不推进纪元");

        store.set_contents(store.contents().to_vec());
        assert_eq!(store.revision(a), Some(5), "整体替换刷新全部材质");
        assert_eq!(store.revision(b), Some(5));
        assert_eq!(store.revision(MaterialHandle(0)), Some(5));
        assert_eq!(store.epoch(), 5);
    }
}
