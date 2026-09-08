//! 编程式 glTF / GLB 构造器（打磨 IV 摩擦②）：「**记账归 builder，人只
//! 声明几何 / 层级 / 关键帧**」。
//!
//! 此前资产生成工具（`gen_asset` 示例的四个 `write_*_glb` 函数）手写
//! JSON 字符串：bufferView 偏移 / accessor 下标 / sampler 编号全靠手工
//! 记账，错一位即加载失败或姿势错乱且无定位信息。[`GltfBuilder`] 收敛
//! 为游戏资产需要的子集，把全部簿记收进 `build`：
//!
//! - [`GltfBuilder::mesh`]：单 primitive 网格（POSITION / NORMAL /
//!   TEXCOORD_0，蒙皮网格追加 JOINTS_0 / WEIGHTS_0）——二进制段自动
//!   追加、**4 字节对齐自动填充**（旧手写文件「times 声明在偏移 112」
//!   类的隐式对齐假设由此消化）、POSITION 包围盒（min / max）自动计算、
//!   索引按顶点数自动选 u16 / u32；
//! - [`GltfBuilder::material`]：baseColorFactor / metallic / roughness /
//!   emissive；
//! - [`GltfBuilder::material_textured`]：在以上之外挂纹理通道
//!   （baseColor / 法线 / ORM 打包图，打磨 VI 摩擦③）——CPU 端
//!   [`CpuTexture`] 像素在 build 时 PNG 编码 + base64 写成 `data:` URI
//!   嵌进 GLB（自包含资产，`spawn_gltf_instance_from_bytes` 的内存路径
//!   可直接消费）；内容相同的图共享同一 image / texture 条目；
//! - [`GltfBuilder::node`] + [`GltfBuilder::add_child`]：TRS + 层级；
//!   **场景顶层自动推断**（未被任何 children 引用的节点）；
//! - [`GltfBuilder::skin`]：关节节点下标 + 逆绑定矩阵（列主序 MAT4
//!   accessor）；
//! - [`GltfBuilder::animation`]：每通道直接给关键帧——时间轴内容完全
//!   一致的通道自动共享 times accessor（蠕虫双通道共用时间轴的旧布局
//!   即由此免费获得）；
//! - [`GltfBuilder::build_glb`]：GLB 容器字节；
//!   [`GltfBuilder::build_json_and_bin`]：外部 buffer 的 `.gltf` JSON +
//!   `.bin` 字节。
//!
//! # 用法
//!
//! ```ignore
//! let mut builder = GltfBuilder::new();
//! let mat = builder.material([0.2, 0.65, 0.6, 1.0], 0.1, 0.5, None);
//! let mesh = builder.mesh("Body", &body_mesh_data, mat);
//! let root = builder.node(GltfNodeDesc::new("BotRoot"));
//! let body = builder.node(GltfNodeDesc::new("Body").with_mesh(mesh).at([0.0, 0.9, 0.0]));
//! builder.add_child(root, body);
//! builder.animation("idle", &[AnimChannel {
//!     node: body,
//!     path: AnimationPath::Scale,
//!     interpolation: AnimationInterpolation::Linear,
//!     times: vec![0.0, 1.0, 2.0],
//!     values: vec![1.0, 1.0, 1.0, 1.0, 0.93, 1.0, 1.0, 1.0, 1.0],
//! }]);
//! std::fs::write("bot.glb", builder.build_glb()).unwrap();
//! ```
//!
//! 产物与 iris-render 的 glTF 加载器（[`spawn_gltf_instance`]）互为逆
//! 操作：单测覆盖「builder 构造 → 加载 → 剪辑 / 节点映射 / 蒙皮正确」。

use glam::Mat4;
use iris_render::animation::{AnimationInterpolation, AnimationPath};
use iris_render::asset::{base64, write_glb};
use iris_render::mesh::MeshData;
use iris_render::texture::CpuTexture;
use serde_json::{Value, json};

// glTF 规范字面量（componentType）。
const COMPONENT_UNSIGNED_SHORT: u32 = 5123;
const COMPONENT_UNSIGNED_INT: u32 = 5125;
const COMPONENT_FLOAT: u32 = 5126;

/// 节点声明：名称 + 可选网格 / 蒙皮引用 + TRS。链式便捷方法见
/// [`GltfNodeDesc::with_mesh`] / [`GltfNodeDesc::with_skin`] /
/// [`GltfNodeDesc::at`] / [`GltfNodeDesc::rotated`] / [`GltfNodeDesc::scaled`]。
#[derive(Clone, Debug)]
pub struct GltfNodeDesc {
    /// 节点名（导出用；加载器不消费）。
    pub name: String,
    /// 引用的网格下标（[`GltfBuilder::mesh`] 返回值）。
    pub mesh: Option<usize>,
    /// 引用的蒙皮下标（[`GltfBuilder::skin`] 返回值）。
    pub skin: Option<usize>,
    /// 平移。
    pub translation: [f32; 3],
    /// 旋转（glTF 四元数，(x, y, z, w)，缺省恒等）。
    pub rotation: [f32; 4],
    /// 缩放（缺省 (1, 1, 1)）。
    pub scale: [f32; 3],
}

impl GltfNodeDesc {
    /// 恒等 TRS 的命名节点。
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            mesh: None,
            skin: None,
            translation: [0.0; 3],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0; 3],
        }
    }

    /// 挂网格。
    pub fn with_mesh(mut self, mesh: usize) -> Self {
        self.mesh = Some(mesh);
        self
    }

    /// 挂蒙皮。
    pub fn with_skin(mut self, skin: usize) -> Self {
        self.skin = Some(skin);
        self
    }

    /// 设平移。
    pub fn at(mut self, translation: [f32; 3]) -> Self {
        self.translation = translation;
        self
    }

    /// 设旋转（glTF 四元数 (x, y, z, w)）。
    pub fn rotated(mut self, rotation: [f32; 4]) -> Self {
        self.rotation = rotation;
        self
    }

    /// 设缩放。
    pub fn scaled(mut self, scale: [f32; 3]) -> Self {
        self.scale = scale;
        self
    }
}

/// 动画通道声明：某节点的某属性的关键帧序列。写入缓冲、对齐、accessor
/// / sampler 编号由 builder 记账——人只给数据。
#[derive(Clone, Debug)]
pub struct AnimChannel {
    /// 目标节点下标（[`GltfBuilder::node`] 返回值）。
    pub node: usize,
    /// 驱动的属性路径（复用运行时枚举，与 [`AnimationPath`] 的采样语义
    /// 同源）。
    pub path: AnimationPath,
    /// 插值方式。
    pub interpolation: AnimationInterpolation,
    /// 关键帧时间（秒，非递减）。
    pub times: Vec<f32>,
    /// 平铺关键帧值：平移 / 缩放每帧 3 分量，旋转每帧 4 分量 (x, y, z, w)。
    pub values: Vec<f32>,
}

/// 材质的纹理通道声明（[`GltfBuilder::material_textured`] 用）：
/// 每个通道给一张 CPU 端 [`CpuTexture`]（RGBA8 像素），`None` = 该通道
/// 不写。三张图在 build 时各自 PNG 编码为 `data:image/png;base64,…` URI
/// 嵌进 GLB（**自包含**：内存加载路径 `spawn_gltf_instance_from_bytes`
/// 无 base_dir，外部 uri 引用会显式报错）。
///
/// 色彩空间由**通道**决定（不由入参的 `srgb` 字段决定——加载器按通道
/// 语义标注）：baseColor 是颜色数据（sRGB），法线与 ORM 是数据贴图
/// （线性）。入参里与像素无关的 `srgb` 字段被忽略。
#[derive(Clone, Debug, Default)]
pub struct GltfMaterialTextures {
    /// baseColor 贴图（glTF `pbrMetallicRoughness.baseColorTexture`）。
    pub base_color: Option<CpuTexture>,
    /// 切线空间法线贴图（glTF `normalTexture`，线性）。
    pub normal: Option<CpuTexture>,
    /// ORM 打包图（R = AO、G = 粗糙度、B = 金属度，线性）：写入 glTF
    /// `metallicRoughnessTexture` **与** `occlusionTexture` 的同一
    /// texture 下标——加载器对同下标直接引用、不做 CPU 通道重打包
    /// （R42 合并策略），登记的像素即最终采样到的 ORM。
    pub orm: Option<CpuTexture>,
}

#[derive(Default)]
struct MaterialDef {
    base_color: [f32; 4],
    metallic: f32,
    roughness: f32,
    emissive: Option<[f32; 3]>,
    /// 通道 → 文件内 texture 下标（[`GltfBuilder::register_texture`] 分配，
    /// None = 该通道不写引用）。
    base_color_texture: Option<usize>,
    normal_texture: Option<usize>,
    /// ORM 打包图（同时充当 `occlusionTexture`，见 [`GltfMaterialTextures`]）。
    orm_texture: Option<usize>,
}

struct MeshDef {
    name: String,
    position: usize,
    normal: usize,
    uv: usize,
    indices: usize,
    joints: Option<usize>,
    weights: Option<usize>,
    /// 顶点切线 accessor（R42，`data.tangents` 为 `Some` 时写入
    /// TANGENT 属性——VEC4 f32，w = 手性符号）。
    tangents: Option<usize>,
    material: usize,
}

struct NodeDef {
    desc: GltfNodeDesc,
    children: Vec<usize>,
}

struct SkinDef {
    name: String,
    joints: Vec<usize>,
    inverse_bind_matrices: usize,
}

struct SamplerDef {
    input: usize,
    output: usize,
    interpolation: AnimationInterpolation,
}

struct ChannelDef {
    sampler: usize,
    node: usize,
    path: AnimationPath,
}

struct AnimationDef {
    name: String,
    channels: Vec<ChannelDef>,
    samplers: Vec<SamplerDef>,
}

struct BufferViewDef {
    offset: usize,
    length: usize,
}

struct AccessorDef {
    buffer_view: usize,
    component_type: u32,
    count: usize,
    attribute_type: &'static str,
    /// 仅 POSITION 写入的包围盒（分量数随 attribute_type）。
    min: Option<Vec<f64>>,
    max: Option<Vec<f64>>,
}

/// glTF / GLB 构造器：声明式 API 收集几何 / 层级 / 关键帧，二进制记账
/// （偏移 / 对齐 / 下标）在 `build` 时一次性完成。
#[derive(Default)]
pub struct GltfBuilder {
    bin: Vec<u8>,
    buffer_views: Vec<BufferViewDef>,
    accessors: Vec<AccessorDef>,
    materials: Vec<MaterialDef>,
    meshes: Vec<MeshDef>,
    nodes: Vec<NodeDef>,
    skins: Vec<SkinDef>,
    animations: Vec<AnimationDef>,
    /// 时间轴去重缓存：(times 内容, accessor 下标)。
    times_cache: Vec<(Vec<f32>, usize)>,
    /// 内嵌图像（RGBA8 像素；build 时 PNG → base64 写成 data URI，
    /// 下标 = glTF `images` 数组序）。
    images: Vec<CpuTexture>,
    /// `textures` 下标 → `images` 下标（同一张图被多通道 / 多材质引用时
    /// 只嵌一份）。
    texture_sources: Vec<usize>,
}

fn component_count(attribute_type: &str) -> usize {
    match attribute_type {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" => 4,
        "MAT4" => 16,
        other => unreachable!("GltfBuilder 内部类型字面量出现 {other}"),
    }
}

fn animation_path_name(path: AnimationPath) -> &'static str {
    match path {
        AnimationPath::Translation => "translation",
        AnimationPath::Rotation => "rotation",
        AnimationPath::Scale => "scale",
    }
}

fn interpolation_name(interpolation: AnimationInterpolation) -> &'static str {
    match interpolation {
        AnimationInterpolation::Linear => "LINEAR",
        AnimationInterpolation::Step => "STEP",
    }
}

/// CPU 纹理 → `data:image/png;base64,…` URI（glTF 图像的自包含内嵌形态；
/// 加载器 `load_image` 的 uri 分支直接解码，不需 base_dir）。
fn png_data_uri(cpu: &CpuTexture) -> String {
    let mut png: Vec<u8> = Vec::new();
    image::RgbaImage::from_raw(cpu.width, cpu.height, cpu.rgba.clone())
        .expect("GltfBuilder 纹理像素缓冲已在登记时校验（宽 × 高 × 4）")
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("PNG 编码写入内存缓冲，不应失败");
    format!("data:image/png;base64,{}", base64::encode(&png))
}

impl GltfBuilder {
    /// 空构造器。
    pub fn new() -> Self {
        Self::default()
    }

    // ---- 二进制记账（私有）----

    /// 追加数据前补齐 4 字节对齐（accessor 分量最大 4 字节，满足 glTF
    /// 「accessor 起址须为分量大小整数倍」的规范约束）。
    fn append_aligned(&mut self) {
        while self.bin.len() % 4 != 0 {
            self.bin.push(0);
        }
    }

    fn push_view(&mut self, bytes: &[u8]) -> usize {
        self.append_aligned();
        let offset = self.bin.len();
        self.bin.extend_from_slice(bytes);
        self.buffer_views.push(BufferViewDef {
            offset,
            length: bytes.len(),
        });
        self.buffer_views.len() - 1
    }

    fn push_accessor(
        &mut self,
        data: &[u8],
        component_type: u32,
        count: usize,
        attribute_type: &'static str,
    ) -> usize {
        let buffer_view = self.push_view(data);
        self.accessors.push(AccessorDef {
            buffer_view,
            component_type,
            count,
            attribute_type,
            min: None,
            max: None,
        });
        self.accessors.len() - 1
    }

    /// 浮点 accessor（无包围盒）。
    fn push_f32_accessor(&mut self, data: &[f32], attribute_type: &'static str) -> usize {
        let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.push_accessor(
            &bytes,
            COMPONENT_FLOAT,
            data.len() / component_count(attribute_type),
            attribute_type,
        )
    }

    /// POSITION accessor：自动计算 min / max 包围盒（glTF 规范建议、
    /// 旧手写文件手工填写的字段）。
    fn push_position_accessor(&mut self, positions: &[f32]) -> usize {
        let mut min = [f64::INFINITY; 3];
        let mut max = [f64::NEG_INFINITY; 3];
        for vertex in positions.chunks_exact(3) {
            for (axis, &value) in vertex.iter().enumerate() {
                min[axis] = min[axis].min(value as f64);
                max[axis] = max[axis].max(value as f64);
            }
        }
        let bytes: Vec<u8> = positions.iter().flat_map(|v| v.to_le_bytes()).collect();
        let accessor = self.push_accessor(&bytes, COMPONENT_FLOAT, positions.len() / 3, "VEC3");
        if !positions.is_empty() {
            let def = &mut self.accessors[accessor];
            def.min = Some(min.to_vec());
            def.max = Some(max.to_vec());
        }
        accessor
    }

    fn push_u16_accessor(&mut self, data: &[u16], attribute_type: &'static str) -> usize {
        let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.push_accessor(
            &bytes,
            COMPONENT_UNSIGNED_SHORT,
            data.len() / component_count(attribute_type),
            attribute_type,
        )
    }

    fn push_u32_accessor(&mut self, data: &[u32], attribute_type: &'static str) -> usize {
        let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.push_accessor(
            &bytes,
            COMPONENT_UNSIGNED_INT,
            data.len() / component_count(attribute_type),
            attribute_type,
        )
    }

    /// 时间轴 accessor（内容一致的通道自动共享——去重）。
    fn times_accessor(&mut self, times: &[f32]) -> usize {
        if let Some(&(_, accessor)) = self.times_cache.iter().find(|(cached, _)| cached == times) {
            return accessor;
        }
        let accessor = self.push_f32_accessor(times, "SCALAR");
        self.times_cache.push((times.to_vec(), accessor));
        accessor
    }

    // ---- 声明式 API ----

    /// 声明材质（PBR 金属度 / 粗糙度工作流），返回材质下标。
    /// `emissive` 为 `Some` 时写入 `emissiveFactor`（自发光）。
    /// 需要纹理通道见 [`GltfBuilder::material_textured`]（本函数 = 其三
    /// 通道全 None 的特例）。
    pub fn material(
        &mut self,
        base_color: [f32; 4],
        metallic: f32,
        roughness: f32,
        emissive: Option<[f32; 3]>,
    ) -> usize {
        self.material_textured(
            base_color,
            metallic,
            roughness,
            emissive,
            GltfMaterialTextures::default(),
        )
    }

    /// 声明**带纹理引用**的材质（打磨 VI 摩擦③），返回材质下标。
    ///
    /// 前四参与 [`GltfBuilder::material`] 同款（因子与贴图在 glTF 侧共存：
    /// 贴图像素与 `baseColorFactor` / `metallicFactor` / `roughnessFactor`
    /// **相乘**，故挂 ORM 图时通常把两因子留 1.0、挂 baseColor 图时把
    /// 颜色因子留白——直通语义由加载器决定，builder 不改写）。
    /// `textures` 声明哪几个通道挂哪张图（像素在 build 时 PNG 编码内嵌）。
    pub fn material_textured(
        &mut self,
        base_color: [f32; 4],
        metallic: f32,
        roughness: f32,
        emissive: Option<[f32; 3]>,
        textures: GltfMaterialTextures,
    ) -> usize {
        let base_color_texture = textures
            .base_color
            .as_ref()
            .map(|texture| self.register_texture(texture));
        let normal_texture = textures
            .normal
            .as_ref()
            .map(|texture| self.register_texture(texture));
        let orm_texture = textures
            .orm
            .as_ref()
            .map(|texture| self.register_texture(texture));
        self.materials.push(MaterialDef {
            base_color,
            metallic,
            roughness,
            emissive,
            base_color_texture,
            normal_texture,
            orm_texture,
        });
        self.materials.len() - 1
    }

    /// 登记一张内嵌图（尺寸与像素缓冲校验 + **内容去重**：同一张图被多
    /// 通道 / 多材质引用时只嵌一份），返回 `textures` 下标。
    fn register_texture(&mut self, cpu: &CpuTexture) -> usize {
        assert!(
            cpu.width > 0 && cpu.height > 0,
            "GltfBuilder 纹理尺寸必须非零（实际 {}×{}）",
            cpu.width,
            cpu.height
        );
        assert_eq!(
            cpu.rgba.len(),
            cpu.width as usize * cpu.height as usize * 4,
            "GltfBuilder 纹理像素缓冲长度应 = 宽 × 高 × 4（RGBA8 行主序）"
        );
        let image = self
            .images
            .iter()
            .position(|existing| {
                existing.width == cpu.width
                    && existing.height == cpu.height
                    && existing.rgba == cpu.rgba
            })
            .unwrap_or_else(|| {
                self.images.push(cpu.clone());
                self.images.len() - 1
            });
        self.texture_sources
            .iter()
            .position(|&source| source == image)
            .unwrap_or_else(|| {
                self.texture_sources.push(image);
                self.texture_sources.len() - 1
            })
    }

    /// 声明单 primitive 网格，返回网格下标。
    ///
    /// 几何来自 [`MeshData`]：POSITION / NORMAL / TEXCOORD_0 三属性 +
    /// 索引；`data.skin` 为 `Some` 时追加 JOINTS_0（u16 VEC4，加载器
    /// 规范子集）与 WEIGHTS_0（f32 VEC4）；`data.tangents` 为 `Some`
    /// 时追加 TANGENT（f32 VEC4，glTF 手性约定，R42）。索引按最大顶点
    /// 下标自动选 u16 / u32。材质下标必填（单 primitive 一材质）。
    pub fn mesh(&mut self, name: &str, data: &MeshData, material: usize) -> usize {
        let mut positions = Vec::with_capacity(data.vertices.len() * 3);
        let mut normals = Vec::with_capacity(data.vertices.len() * 3);
        let mut uvs = Vec::with_capacity(data.vertices.len() * 2);
        for vertex in &data.vertices {
            positions.extend_from_slice(&vertex.position);
            normals.extend_from_slice(&vertex.normal);
            uvs.extend_from_slice(&vertex.uv);
        }
        let position = self.push_position_accessor(&positions);
        let normal = self.push_f32_accessor(&normals, "VEC3");
        let uv = self.push_f32_accessor(&uvs, "VEC2");
        let tangents = data.tangents.as_ref().map(|tangents| {
            let mut flat = Vec::with_capacity(tangents.len() * 4);
            for t in tangents {
                flat.extend_from_slice(t);
            }
            self.push_f32_accessor(&flat, "VEC4")
        });
        // 索引宽度自动选型：顶点数 ≤ 65536 用 u16（多数游戏资产）
        let indices = if data.indices.iter().any(|&i| i > u16::MAX as u32) {
            self.push_u32_accessor(&data.indices, "SCALAR")
        } else {
            let narrow: Vec<u16> = data.indices.iter().map(|&i| i as u16).collect();
            self.push_u16_accessor(&narrow, "SCALAR")
        };
        let (joints, weights) = match &data.skin {
            Some(skin) => {
                let mut joint_indices = Vec::with_capacity(skin.len() * 4);
                let mut joint_weights = Vec::with_capacity(skin.len() * 4);
                for attributes in skin {
                    joint_indices.extend(attributes.joints.iter().map(|&j| j as u16));
                    joint_weights.extend_from_slice(&attributes.weights);
                }
                (
                    Some(self.push_u16_accessor(&joint_indices, "VEC4")),
                    Some(self.push_f32_accessor(&joint_weights, "VEC4")),
                )
            }
            None => (None, None),
        };
        self.meshes.push(MeshDef {
            name: name.to_string(),
            position,
            normal,
            uv,
            indices,
            joints,
            weights,
            tangents,
            material,
        });
        self.meshes.len() - 1
    }

    /// 声明节点，返回节点下标（= 声明顺序，供 [`GltfBuilder::add_child`] /
    /// [`GltfBuilder::skin`] / [`AnimChannel`] 引用）。
    pub fn node(&mut self, desc: GltfNodeDesc) -> usize {
        self.nodes.push(NodeDef {
            desc,
            children: Vec::new(),
        });
        self.nodes.len() - 1
    }

    /// 挂父子边：子节点自动退出场景顶层（顶层 = 未被任何 children 引用
    /// 的节点，`build` 时推断——无须手工记账）。
    pub fn add_child(&mut self, parent: usize, child: usize) {
        self.nodes[parent].children.push(child);
    }

    /// 声明蒙皮，返回蒙皮下标。
    ///
    /// `joints` 为关节节点下标列表；`inverse_bind_matrices` 为逆绑定
    /// 矩阵（**列主序**写入 MAT4 accessor，与 glTF 规范及加载器的
    /// `mat4_list_from_flat` 一致）。数量必须与 `joints` 一致。
    pub fn skin(&mut self, name: &str, joints: &[usize], inverse_bind_matrices: &[Mat4]) -> usize {
        assert_eq!(
            joints.len(),
            inverse_bind_matrices.len(),
            "蒙皮关节数（{}）与逆绑定矩阵数（{}）必须一致",
            joints.len(),
            inverse_bind_matrices.len()
        );
        let mut flat = Vec::with_capacity(inverse_bind_matrices.len() * 16);
        for matrix in inverse_bind_matrices {
            for column in matrix.to_cols_array_2d() {
                flat.extend_from_slice(&column);
            }
        }
        let accessor = self.push_f32_accessor(&flat, "MAT4");
        self.skins.push(SkinDef {
            name: name.to_string(),
            joints: joints.to_vec(),
            inverse_bind_matrices: accessor,
        });
        self.skins.len() - 1
    }

    /// 给已声明的节点补挂蒙皮（[`GltfNodeDesc::with_skin`] 的后置变体）：
    /// 关节节点须在 [`GltfBuilder::skin`] 之前声明，而蒙皮网格节点希望
    /// 保持在节点 0 时用本方法补上引用。
    pub fn attach_skin(&mut self, node: usize, skin: usize) {
        self.nodes[node].desc.skin = Some(skin);
    }

    /// 声明动画剪辑，返回动画下标。
    ///
    /// 每条 [`AnimChannel`] 一个采样器；时间轴内容完全一致的通道自动
    /// 共享 times accessor。
    ///
    /// # Panics
    /// `values` 长度 ≠ `times` 长度 × 路径分量数（3 / 4）时断言失败
    /// （中文断言消息）——关键帧与时间轴不齐是构造错误，立即暴露。
    pub fn animation(&mut self, name: &str, channels: &[AnimChannel]) -> usize {
        let mut samplers = Vec::with_capacity(channels.len());
        let mut channel_defs = Vec::with_capacity(channels.len());
        for channel in channels {
            let components = match channel.path {
                AnimationPath::Rotation => 4,
                _ => 3,
            };
            assert_eq!(
                channel.values.len(),
                channel.times.len() * components,
                "动画「{name}」通道 values 长度 {} 应 = times 长度 {} × 分量数 {components}",
                channel.values.len(),
                channel.times.len()
            );
            let input = self.times_accessor(&channel.times);
            let output = self.push_f32_accessor(
                &channel.values,
                if components == 4 { "VEC4" } else { "VEC3" },
            );
            samplers.push(SamplerDef {
                input,
                output,
                interpolation: channel.interpolation,
            });
            channel_defs.push(ChannelDef {
                sampler: samplers.len() - 1,
                node: channel.node,
                path: channel.path,
            });
        }
        self.animations.push(AnimationDef {
            name: name.to_string(),
            channels: channel_defs,
            samplers,
        });
        self.animations.len() - 1
    }

    // ---- 组装 ----

    /// 组装 GLB 容器字节（JSON chunk 空格补齐 + BIN chunk 零补齐，
    /// 经 iris-render 的 `write_glb`）。
    pub fn build_glb(&self) -> Vec<u8> {
        let (json, bin) = self.build_json_and_bin(None);
        write_glb(&json, &bin)
    }

    /// 组装 glTF 文档：`buffer_uri = Some("cube0.bin")` 时为外部 buffer
    /// 的 `.gltf`（返回 JSON 文本 + `.bin` 字节，两个文件分别落盘）；
    /// `None` 时 buffer 无 uri（GLB 内嵌 buffer 语义，JSON 供检查 /
    /// 测试）。BIN 返回值已按 4 字节补齐（GLB chunk 对齐要求）。
    pub fn build_json_and_bin(&self, buffer_uri: Option<&str>) -> (String, Vec<u8>) {
        let mut bin = self.bin.clone();
        while bin.len() % 4 != 0 {
            bin.push(0);
        }
        let buffer = match buffer_uri {
            Some(uri) => json!({ "uri": uri, "byteLength": self.bin.len() }),
            None => json!({ "byteLength": self.bin.len() }),
        };
        // 场景顶层推断：未被任何 children 引用的节点
        let mut has_parent = vec![false; self.nodes.len()];
        for node in &self.nodes {
            for &child in &node.children {
                has_parent[child] = true;
            }
        }
        let top_level: Vec<usize> = (0..self.nodes.len())
            .filter(|&index| !has_parent[index])
            .collect();

        let buffer_views: Vec<Value> = self
            .buffer_views
            .iter()
            .map(|view| {
                json!({
                    "buffer": 0,
                    "byteOffset": view.offset,
                    "byteLength": view.length,
                })
            })
            .collect();
        let accessors: Vec<Value> = self
            .accessors
            .iter()
            .map(|accessor| {
                let mut value = json!({
                    "bufferView": accessor.buffer_view,
                    "componentType": accessor.component_type,
                    "count": accessor.count,
                    "type": accessor.attribute_type,
                });
                if let (Some(min), Some(max)) = (&accessor.min, &accessor.max) {
                    value["min"] = json!(min);
                    value["max"] = json!(max);
                }
                value
            })
            .collect();
        let materials: Vec<Value> = self
            .materials
            .iter()
            .map(|material| {
                let mut value = json!({
                    "pbrMetallicRoughness": {
                        "baseColorFactor": material.base_color,
                        "metallicFactor": material.metallic,
                        "roughnessFactor": material.roughness,
                    },
                });
                if let Some(emissive) = material.emissive {
                    value["emissiveFactor"] = json!(emissive);
                }
                if let Some(texture) = material.base_color_texture {
                    value["pbrMetallicRoughness"]["baseColorTexture"] = json!({ "index": texture });
                }
                if let Some(texture) = material.normal_texture {
                    value["normalTexture"] = json!({ "index": texture });
                }
                if let Some(texture) = material.orm_texture {
                    // ORM 打包图同下标充当两通道：加载器据此直接引用
                    // 像素，不做 CPU 重打包（R42 合并策略）
                    value["pbrMetallicRoughness"]["metallicRoughnessTexture"] =
                        json!({ "index": texture });
                    value["occlusionTexture"] = json!({ "index": texture });
                }
                value
            })
            .collect();
        // 内嵌图像：PNG → base64 data URI（不走 bufferView——加载器的
        // 图像读取子集只认 uri，见 `asset::gltf::load_image`）
        let images: Vec<Value> = self
            .images
            .iter()
            .map(|cpu| json!({ "uri": png_data_uri(cpu) }))
            .collect();
        let textures: Vec<Value> = self
            .texture_sources
            .iter()
            .map(|&source| json!({ "source": source }))
            .collect();
        let meshes: Vec<Value> = self
            .meshes
            .iter()
            .map(|mesh| {
                let mut attributes = json!({
                    "POSITION": mesh.position,
                    "NORMAL": mesh.normal,
                    "TEXCOORD_0": mesh.uv,
                });
                if let (Some(joints), Some(weights)) = (mesh.joints, mesh.weights) {
                    attributes["JOINTS_0"] = json!(joints);
                    attributes["WEIGHTS_0"] = json!(weights);
                }
                if let Some(tangents) = mesh.tangents {
                    attributes["TANGENT"] = json!(tangents);
                }
                json!({
                    "name": mesh.name,
                    "primitives": [{
                        "attributes": attributes,
                        "indices": mesh.indices,
                        "material": mesh.material,
                    }],
                })
            })
            .collect();
        let nodes: Vec<Value> = self
            .nodes
            .iter()
            .map(|node| {
                let mut value = json!({
                    "name": node.desc.name,
                    "translation": node.desc.translation,
                    "rotation": node.desc.rotation,
                    "scale": node.desc.scale,
                });
                if let Some(mesh) = node.desc.mesh {
                    value["mesh"] = json!(mesh);
                }
                if let Some(skin) = node.desc.skin {
                    value["skin"] = json!(skin);
                }
                if !node.children.is_empty() {
                    value["children"] = json!(node.children);
                }
                value
            })
            .collect();
        let skins: Vec<Value> = self
            .skins
            .iter()
            .map(|skin| {
                json!({
                    "name": skin.name,
                    "joints": skin.joints,
                    "inverseBindMatrices": skin.inverse_bind_matrices,
                })
            })
            .collect();
        let animations: Vec<Value> = self
            .animations
            .iter()
            .map(|animation| {
                json!({
                    "name": animation.name,
                    "channels": animation.channels.iter().map(|channel| {
                        json!({
                            "sampler": channel.sampler,
                            "target": {
                                "node": channel.node,
                                "path": animation_path_name(channel.path),
                            },
                        })
                    }).collect::<Vec<Value>>(),
                    "samplers": animation.samplers.iter().map(|sampler| {
                        json!({
                            "input": sampler.input,
                            "output": sampler.output,
                            "interpolation": interpolation_name(sampler.interpolation),
                        })
                    }).collect::<Vec<Value>>(),
                })
            })
            .collect();

        let mut document = json!({
            "asset": { "version": "2.0", "generator": "iris GltfBuilder" },
            "scene": 0,
            "scenes": [{ "nodes": top_level }],
            "buffers": [buffer],
            "bufferViews": buffer_views,
            "accessors": accessors,
            "materials": materials,
            "meshes": meshes,
            "skins": skins,
            "nodes": nodes,
            "animations": animations,
        });
        // 空表不写键：无纹理资产的 JSON 与既有产物逐字一致（差异只出现在
        // 真正声明了纹理的材质上）
        if !images.is_empty() {
            document["images"] = Value::Array(images);
        }
        if !textures.is_empty() {
            document["textures"] = Value::Array(textures);
        }
        (
            serde_json::to_string(&document).expect("builder 数据恒为合法 JSON"),
            bin,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core::prelude::{Time, Transform, World};
    use iris_render::animation::update_animations;
    use iris_render::asset::spawn_gltf_instance;
    use iris_render::mesh::{SkinVertex, Vertex};
    use std::time::Duration;

    /// 三角形网格（XY 平面，法线 +Z）。
    fn triangle() -> MeshData {
        MeshData {
            vertices: vec![
                Vertex {
                    position: [0.0, 0.0, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    uv: [0.0, 0.0],
                },
                Vertex {
                    position: [1.0, 0.0, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    uv: [1.0, 0.0],
                },
                Vertex {
                    position: [0.0, 1.0, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    uv: [0.0, 1.0],
                },
            ],
            indices: vec![0, 1, 2],
            skin: None,
            tangents: None,
            uv2: None,
        }
    }

    /// 确定性推进 Time 并跑一帧动画。
    fn step(world: &World, dt: f64) {
        world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(dt));
        update_animations(world);
    }

    /// 摩擦②核心验收：builder 构造最小 glTF（三角 + 旋转动画）→
    /// write_glb 落盘 → 引擎加载器加载 → 剪辑时长 / 节点映射正确，
    /// 采样推进驱动节点旋转。
    #[test]
    fn built_triangle_glb_loads_and_animates() {
        let mut builder = GltfBuilder::new();
        let material = builder.material([0.9, 0.15, 0.1, 1.0], 0.0, 0.9, None);
        let mesh = builder.mesh("Tri", &triangle(), material);
        builder.node(GltfNodeDesc::new("TriRoot").with_mesh(mesh));
        let (s, c) = (45f32).to_radians().sin_cos();
        builder.animation(
            "spin",
            &[AnimChannel {
                node: 0,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 1.0],
                values: vec![0.0, 0.0, 0.0, 1.0, 0.0, s, 0.0, c],
            }],
        );
        let glb = builder.build_glb();

        let path =
            std::env::temp_dir().join(format!("iris-builder-tri-{}.glb", std::process::id()));
        std::fs::write(&path, &glb).unwrap();
        let mut world = World::new();
        world.insert_resource(iris_render::mesh::MeshStore::new());
        world.insert_resource(iris_render::material::MaterialStore::new());
        world.insert_resource(iris_render::texture::TextureStore::new());
        world.insert_resource(Time::default());
        let instance = spawn_gltf_instance(&mut world, &path).expect("builder 产物应可加载");
        // 剪辑时长与节点映射
        assert_eq!(instance.clips.len(), 1, "应解析出 1 条剪辑");
        assert!(
            (instance.clips[0].duration - 1.0).abs() < 1e-6,
            "时长取最后关键帧"
        );
        assert_eq!(instance.clips[0].channels[0].node, 0, "通道应指向节点 0");
        assert_eq!(instance.node_entities.len(), 1, "节点 0 应在场景内");
        let root = instance.roots[0];
        world.insert(root, instance.player());
        // 采样推进：0.5 s 处 yaw 22.5°
        step(&world, 0.5);
        let rotation = world.get::<Transform>(root).unwrap().rotation;
        let expected = glam::Quat::from_rotation_y(22.5f32.to_radians());
        let angle = 2.0 * rotation.dot(expected).abs().clamp(0.0, 1.0).acos();
        assert!(angle < 1e-4, "0.5 s 应为 yaw 22.5°，实际 {rotation:?}");
        std::fs::remove_file(&path).ok();
    }

    /// 4×4 确定性色板（PNG 无损，往返后可逐字节比对）。
    fn swatch(seed: u8) -> CpuTexture {
        let rgba: Vec<u8> = (0..16u8)
            .flat_map(|i| {
                [
                    seed.wrapping_mul(7).wrapping_add(i * 16),
                    seed,
                    255u8.wrapping_sub(seed).wrapping_add(i),
                    255,
                ]
            })
            .collect();
        CpuTexture {
            width: 4,
            height: 4,
            rgba,
            srgb: true,
        }
    }

    /// 摩擦③ 核心验收：`material_textured` 的三通道声明 → GLB（PNG
    /// data URI 自包含）→ **内存入口** `spawn_gltf_instance_from_bytes`
    /// 读回。锁定四件事：
    /// - 材质三通道句柄有效且**像素逐字节一致**（PNG 无损 + 加载器直通）；
    /// - 色彩空间由通道决定：baseColor = sRGB、法线 / ORM = 线性；
    /// - builder 记账去重：同图被两材质引用 ⇒ `images` / `textures`
    ///   各只多一份，且加载后两材质共享同一 TextureStore 条目；
    /// - ORM 打包图同下标写进 `metallicRoughnessTexture` 与
    ///   `occlusionTexture`（加载器据此免重打包）。
    #[test]
    fn built_textured_material_roundtrips_through_the_memory_loader() {
        let (albedo, cloth, pack) = (swatch(10), swatch(200), swatch(80));
        let mut builder = GltfBuilder::new();
        let cloth_mat = builder.material_textured(
            [1.0, 1.0, 1.0, 1.0],
            1.0,
            1.0,
            None,
            GltfMaterialTextures {
                base_color: Some(albedo.clone()),
                normal: Some(cloth.clone()),
                orm: Some(pack.clone()),
            },
        );
        // 只引用同一张法线图的次材质：图像登记必须命中去重
        let trim_mat = builder.material_textured(
            [1.0, 1.0, 1.0, 1.0],
            1.0,
            1.0,
            None,
            GltfMaterialTextures {
                normal: Some(cloth.clone()),
                ..Default::default()
            },
        );
        let body = builder.mesh("Body", &triangle(), cloth_mat);
        let trim = builder.mesh("Trim", &triangle(), trim_mat);
        let root = builder.node(GltfNodeDesc::new("Root").with_mesh(body));
        let trim_node = builder.node(GltfNodeDesc::new("Trim").with_mesh(trim));
        builder.add_child(root, trim_node);
        let glb = builder.build_glb();

        // ---- builder 侧记账 ----
        let (json, _) = builder.build_json_and_bin(None);
        let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            doc["images"].as_array().expect("有纹理就应写 images").len(),
            3,
            "三张不同的图各嵌一份，重复引用不重复嵌图"
        );
        assert_eq!(doc["textures"].as_array().expect("应写 textures").len(), 3);
        let mats = doc["materials"].as_array().unwrap();
        assert_ne!(
            mats[0]["pbrMetallicRoughness"]["baseColorTexture"]["index"],
            mats[0]["pbrMetallicRoughness"]["metallicRoughnessTexture"]["index"],
            "baseColor 与 ORM 是两张不同的图（同值说明通道串位）"
        );
        assert_eq!(
            mats[0]["normalTexture"]["index"], mats[1]["normalTexture"]["index"],
            "共用同一张法线图的材质应指向同一 texture"
        );
        assert_eq!(
            mats[0]["pbrMetallicRoughness"]["metallicRoughnessTexture"]["index"],
            mats[0]["occlusionTexture"]["index"],
            "ORM 打包图应同下标充当 metallicRoughness 与 occlusion 两通道"
        );

        // ---- 加载器侧读回 ----
        let mut world = World::new();
        world.insert_resource(iris_render::mesh::MeshStore::new());
        world.insert_resource(iris_render::material::MaterialStore::new());
        world.insert_resource(iris_render::texture::TextureStore::new());
        let instance = iris_render::asset::spawn_gltf_instance_from_bytes(&mut world, glb)
            .expect("自包含纹理 GLB 应可经内存入口加载");
        let def_of = |world: &World, node: usize| -> iris_render::material::MaterialDef {
            let entity = instance.node_entities[node].expect("节点应在场景内");
            let part = world.children(entity)[0];
            let handle = *world
                .get::<iris_render::material::MaterialHandle>(part)
                .expect("primitive 应挂材质");
            world
                .resource::<iris_render::material::MaterialStore>()
                .get(handle)
                .expect("材质应在 store 内")
                .clone()
        };
        let def = def_of(&world, 0);
        let store = world.resource::<iris_render::texture::TextureStore>();
        let base = store.get(def.texture).expect("baseColor 贴图句柄应有效");
        assert!(base.srgb, "baseColor 是颜色数据 → sRGB 标注");
        assert_eq!(base.rgba, albedo.rgba, "baseColor 像素应逐字节一致");
        let normal_handle = def.normal_texture.expect("法线通道应有纹理");
        let normal = store.get(normal_handle).expect("法线句柄应有效");
        assert!(!normal.srgb, "法线是数据贴图 → 线性标注");
        assert_eq!(normal.rgba, cloth.rgba, "法线像素应逐字节一致");
        let orm = store
            .get(def.orm_texture.expect("ORM 通道应有纹理"))
            .expect("ORM 句柄应有效");
        assert!(!orm.srgb, "ORM 是数据贴图 → 线性标注");
        assert_eq!(
            orm.rgba, pack.rgba,
            "ORM 像素应逐字节一致（R=AO G=粗糙 B=金属直通）"
        );
        assert_eq!(
            def_of(&world, 1).normal_texture,
            Some(normal_handle),
            "同一张法线图在两材质间应共享 store 条目（去重生效）"
        );
    }

    /// 蒙皮声明：builder 产物加载后 Skin 组件挂到 primitive 实体、
    /// 关节映射与逆绑定矩阵逐值正确。
    #[test]
    fn built_skinned_glb_loads_skin_and_ibm() {
        let mut data = triangle();
        data.skin = Some(vec![
            SkinVertex {
                joints: [0, 0, 0, 0],
                weights: [1.0, 0.0, 0.0, 0.0],
            };
            3
        ]);
        let mut builder = GltfBuilder::new();
        let material = builder.material([0.8, 0.8, 0.8, 1.0], 0.0, 1.0, None);
        let mesh = builder.mesh("Skinned", &data, material);
        let joint = builder.node(GltfNodeDesc::new("joint").at([0.0, 1.0, 0.0]));
        let skin = builder.skin(
            "Skin",
            &[joint],
            &[glam::Mat4::from_translation(glam::Vec3::new(
                0.0, -1.0, 0.0,
            ))],
        );
        builder.node(
            GltfNodeDesc::new("MeshRoot")
                .with_mesh(mesh)
                .with_skin(skin),
        );
        let glb = builder.build_glb();

        let path =
            std::env::temp_dir().join(format!("iris-builder-skin-{}.glb", std::process::id()));
        std::fs::write(&path, &glb).unwrap();
        let mut world = World::new();
        world.insert_resource(iris_render::mesh::MeshStore::new());
        world.insert_resource(iris_render::material::MaterialStore::new());
        world.insert_resource(iris_render::texture::TextureStore::new());
        let instance = spawn_gltf_instance(&mut world, &path).expect("蒙皮产物应可加载");
        // 声明顺序：关节是节点 0、网格节点是节点 1
        let joint_entity = instance.node_entities[0].expect("关节节点应生成实体");
        let mesh_root = instance.node_entities[1].expect("节点 1 应生成实体");
        // 蒙皮挂在 mesh 节点的 primitive 子实体上
        let part = world.children(mesh_root)[0];
        let skin = world
            .get::<iris_render::skin::Skin>(part)
            .expect("primitive 应挂 Skin");
        assert_eq!(skin.joints, vec![joint_entity], "关节应映射到节点实体");
        let expected = glam::Mat4::from_translation(glam::Vec3::new(0.0, -1.0, 0.0));
        assert!(
            skin.inverse_bind_matrices[0].abs_diff_eq(expected, 1e-6),
            "逆绑定矩阵应逐值一致，实际 {:?}",
            skin.inverse_bind_matrices[0]
        );
        std::fs::remove_file(&path).ok();
    }

    /// 外部 buffer 的 `.gltf`（build_json_and_bin）：JSON + .bin 落盘后
    /// 同一条加载路径可用。
    #[test]
    fn built_json_and_bin_roundtrips() {
        let mut builder = GltfBuilder::new();
        let material = builder.material([0.1, 0.2, 0.3, 1.0], 1.0, 1.0, Some([0.1, 0.2, 0.3]));
        let mesh = builder.mesh("Tri", &triangle(), material);
        builder.node(GltfNodeDesc::new("Root").with_mesh(mesh));
        let (json, bin) = builder.build_json_and_bin(Some("tri.bin"));
        let dir = std::env::temp_dir().join(format!("iris-builder-json-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tri.gltf"), &json).unwrap();
        std::fs::write(dir.join("tri.bin"), &bin).unwrap();
        let mut world = World::new();
        world.insert_resource(iris_render::mesh::MeshStore::new());
        world.insert_resource(iris_render::material::MaterialStore::new());
        world.insert_resource(iris_render::texture::TextureStore::new());
        let instance = spawn_gltf_instance(&mut world, dir.join("tri.gltf"))
            .expect("外部 buffer 产物应可加载");
        assert_eq!(instance.node_entities.len(), 1);
        assert_eq!(instance.clips.len(), 0, "无动画声明则无剪辑");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 关键帧与时间轴不齐是构造错误：animation 立即断言（中文消息）。
    #[test]
    #[should_panic(expected = "values 长度")]
    fn animation_rejects_mismatched_keyframes() {
        let mut builder = GltfBuilder::new();
        builder.animation(
            "broken",
            &[AnimChannel {
                node: 0,
                path: AnimationPath::Translation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 1.0],
                values: vec![0.0, 0.0, 0.0],
            }],
        );
    }
}
