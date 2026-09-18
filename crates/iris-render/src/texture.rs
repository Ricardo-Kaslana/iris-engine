//! 纹理：程序化生成的 CPU 纹理目录（零外部图像解码依赖）。
//!
//! **色彩空间（R42）**：[`CpuTexture::srgb`] 标注数据的语义解释——`true`
//! 为 sRGB 编码（baseColor / 贴花等「颜色」数据，GPU 采样时硬件自动做
//! sRGB → 线性解码，上传格式 `Rgba8UnormSrgb`）；`false` 为线性数据
//! （法线 / ORM 等数据贴图，采样即原值，上传格式 `Rgba8Unorm`）。
//! 程序化构造器（`solid` / `checkerboard`）产生的是颜色数据，恒
//! `srgb = true`——与 R42 之前所有纹理一律按 sRGB 上传的既有行为
//! 逐位一致；程序化砖墙法线 / ORM 对（[`CpuTexture::brick_normal_orm`]）
//! 是数据贴图，恒 `srgb = false`。
//!
//! **BC3 声明（R59）**：[`TextureStore`] 维护一张「经理入期 manifest
//! 声明走 BC3 压缩」的句柄侧表（[`TextureStore::declare_bc3`] /
//! [`TextureStore::is_bc3_declared`]）——**声明的准入闸门**，渲染器
//! 只对声明过的纹理启用 BC3 上传路径（[`crate::texture_bcn::bc3_eligible`]），
//! 未声明纹理维持既有 RGBA8 / BC1 决策树逐位不变。声明表用
//! `BTreeSet`（确定性文化：迭代序 = 句柄序，与 broadphase 同规约）。
//!
//! **无压缩声明（R66e）**：[`TextureStore::declare_uncompressed`] /
//! [`TextureStore::is_uncompressed_declared`] 是 BC 决策树的**前置否决**——
//! 声明句柄跳过 BC1/BC3 直走 RGBA8 + mip 链。动机：BC1 的通道耦合端点
//! 插值在**法线图**上产生近距块状伪梯度（iris-exp frictions ⑨：皮肤近距
//! 块斑；同图 Blender 无压缩渲染干净、摘法线离屏对照干净 ⇒ 压缩为病灶）。
//! glTF 加载器为法线 / ORM **自动声明**（外部资产质量优先）；引擎原生资产
//! 不声明 = 决策树逐位不变（内存优先，既有基线零漂移）。显存估算两口径
//! 同步（`texture_gpu_bytes` 前置分支 + `gpu_bytes_estimate_mipped` 谓词）。
//!
//! **Mip 链与各向异性过滤（R66）**：[`MipSettings`] 资源是全库材质纹理
//! mip 链 + 各向异性过滤的**总开关**——资源缺席 = 单 mip 上传 + 采样器
//! 逐字段与 R66 之前一致（默认像素零变化的兼容形态，与 TaaSettings /
//! SsrSettings / OcclusionCulling 的「缺席 = 关」范式同款）。在场时：
//! RGBA8 路径上传完整 CPU 生成 mip 链（[`mip_chain_rgba8`]，2×2 box
//! 降采样整数运算、跨平台位级确定），采样器三线性 + 各向异性过滤
//! （值域 [1, 16]，适配器能力由 wgpu-core 二次收敛，见 [`MipSettings`]）。**诚实限制清单**：
//! - mip 在 **sRGB 编码空间**过滤（对 u8 编码值直接求均值），非线性光
//!   空间的物理正确降采样——高对比边缘的半级行为与线性空间过滤略有
//!   差异；线性空间过滤需要浮点中间格式 + 逐级编解码，v1 不做；
//! - **BC1 / BC3 压缩路径的 mip 链以 `MipSettings` 在场为前提（R66d）**：
//!   缺席 = 单 mip（与 R66 之前逐位一致）；在场 = 逐级 RGBA8 降采样后编码
//!   成块流链（[`mip_chain_bc`]）——R66 只做 RGBA8 路径时，全不透明数据
//!   贴图（法线 / ORM）命中 BC1 恒单 mip，远距无法线平滑出点状噪点
//!   （iris-exp frictions ⑧）；`gpu_bytes_estimate_mipped` 口径一致）；
//! - 生效时机 = **上传时**：渲染器按句柄值缓存 GPU 纹理与采样器，
//!   资源在纹理首次上传后修改/移除**不追溯**已上传纹理；
//! - 奇数维降采样语义见 [`downsample_rgba8`] 文档（floor 链维数公式，
//!   末行/列源像素不参与下一级）。

use std::collections::BTreeSet;

use iris_core::Resource;

/// 各向异性过滤 / mip 链设置（R66，全库材质纹理采样策略的总开关）。
///
/// **缺席语义 = 与 R66 之前逐位一致**：单 mip 上传、采样器逐字段不变
/// （`mipmap_filter: Nearest`、`anisotropy_clamp: 1`）——默认渲染像素
/// 零变化。在场时材质纹理走「CPU mip 链 + 三线性 + 各向异性」上传
/// （RGBA8 路径；BC1 / BC3 压缩路径维持单 mip，见模块文档诚实限制）。
///
/// 生效时机为**纹理首次上传时**：渲染器按句柄值缓存 GPU 资源，运行中
/// 修改本资源只影响之后新上传的纹理，不追溯已上传者。
#[derive(Resource, Clone, Copy, Debug)]
pub struct MipSettings {
    /// 各向异性过滤上限（1 = 关；典型 4 / 8）。实际生效值先夹到
    /// wgpu hal 接口边界 `[1, 16]`，再由 wgpu-core 按适配器能力二次
    /// 收敛（`DownlevelFlags::ANISOTROPIC_FILTERING` 不支持时恒 1，
    /// 不会创建失败）。
    pub anisotropy: u8,
}

impl Default for MipSettings {
    fn default() -> Self {
        Self { anisotropy: 8 }
    }
}

/// 完整 mip 链的级数（含第 0 级原图）：`1 + floor(log2(max(w, h)))`，
/// 每级维数 `max(1, dim / 2)` 直到 1×1。1×1 纹理链长恒 1（渲染器白色
/// 兜底纹理无需特判的依据）。
pub fn mip_level_count(width: u32, height: u32) -> u32 {
    32 - width.max(height).max(1).leading_zeros()
}

/// 单级 2×2 box 降采样（RGBA8，纯函数，整数运算跨平台位级确定）。
///
/// 输出维数 `w' = max(1, w / 2)`、`h' = max(1, h / 2)`（整数除法 =
/// floor，与 GPU mip 链维数公式一致）。输出纹素 (x, y) = 源纹素
/// (2x, 2y)、(2x+1, 2y)、(2x, 2y+1)、(2x+1, 2y+1) 四采样**逐通道整数
/// 均值（floor 舍入）**；越界下标 clamp 到边缘（一维为 1 时该维两采样
/// 同纹素——边缘像素权重加倍，即「边缘 clamp」语义）。
///
/// **奇数维的诚实语义**：floor 维数公式下，奇数维的末行/列源像素不
/// 落进任何输出纹素的采样窗（如 5 宽 → 2 宽，采样窗为列 (0,1) 与
/// (2,3)，列 4 丢弃）——与 GPU 硬件 mip 链的维数推进一致，信息损失
/// 每级至多一行/列，三线性插值下不可感知。
///
/// 注意：均值在**编码空间**直接求（调用方负责 sRGB 语义标注，限制见
/// 模块文档），非线性光空间过滤。
pub fn downsample_rgba8(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    assert_eq!(
        (width as u64) * (height as u64) * 4,
        rgba.len() as u64,
        "降采样输入尺寸与数据长度不匹配"
    );
    let w2 = (width / 2).max(1);
    let h2 = (height / 2).max(1);
    let mut out = vec![0u8; (w2 * h2 * 4) as usize];
    let w = width as usize;
    // 采样下标 clamp：一维为 1 时 2x+1 越界 → 复制边缘（权重加倍）；
    // floor 维数公式下其余情形采样窗天然不越界。
    let clamp_x = |i: u32| (i as usize).min(w - 1);
    let clamp_y = |i: u32| (i as usize).min(height as usize - 1);
    for y in 0..h2 {
        let sy0 = clamp_y(2 * y);
        let sy1 = clamp_y(2 * y + 1);
        for x in 0..w2 {
            let sx0 = clamp_x(2 * x);
            let sx1 = clamp_x(2 * x + 1);
            for c in 0..4 {
                let sum = rgba[(sy0 * w + sx0) * 4 + c] as u32
                    + rgba[(sy0 * w + sx1) * 4 + c] as u32
                    + rgba[(sy1 * w + sx0) * 4 + c] as u32
                    + rgba[(sy1 * w + sx1) * 4 + c] as u32;
                out[((y * w2 + x) * 4) as usize + c] = (sum / 4) as u8;
            }
        }
    }
    out
}

/// 生成完整 mip 链（RGBA8，纯函数）：返回逐级像素数据，第 0 级 =
/// 源数据副本，逐级 [`downsample_rgba8`] 直到 1×1（[`mip_level_count`]
/// 级）。链的每一级可脱离 World 精确值单测；同输入两次调用逐位一致。
pub fn mip_chain_rgba8(width: u32, height: u32, rgba: &[u8]) -> Vec<Vec<u8>> {
    let mut levels = vec![rgba.to_vec()];
    let mut w = width.max(1);
    let mut h = height.max(1);
    while w > 1 || h > 1 {
        let next = downsample_rgba8(w, h, levels.last().expect("链至少含第 0 级"));
        w = (w / 2).max(1);
        h = (h / 2).max(1);
        levels.push(next);
    }
    levels
}

/// 生成完整 mip 链（BC 压缩路径，纯函数，R66d）：先按 [`mip_chain_rgba8`]
/// 逐级降采样 RGBA8，再把每一级编码为 BC1 / BC3 块流（编码器内部做 4
/// 对齐补边）。**链止于 4×4**：wgpu 压缩纹理的 copy extent 必须为块宽
/// （4）整数倍，2×2 / 1×1 级上传非法（Validation Error）——4×4 已足够
/// 远距平滑，更深层级由采样器钳到末级。
///
/// 动机：法线 / ORM 等**全不透明数据贴图**命中 BC1 压缩路径（R30 决策
/// 树），R66 的 mip 链只覆盖 RGBA8 回退路径 ⇒ 压缩贴图恒单 mip，远距缩小
/// 采样无法线平滑 ⇒ 皮肤点状噪点（iris-exp frictions ⑧ 归因实验：关法线
/// 噪点消失、关 mip 不变）。**兼容形态与 R66 同款**：`MipSettings` 缺席 =
/// 单 mip 上传逐位不变；在场才走本链（上传分支见 `renderer.rs`）。
/// 第 0 级 = 源图直接编码的块流（与单 mip 路径同函数同输入，逐位一致）。
pub fn mip_chain_bc(width: u32, height: u32, rgba: &[u8], bc3: bool) -> Vec<Vec<u8>> {
    mip_chain_rgba8(width, height, rgba)
        .into_iter()
        .enumerate()
        .take_while(|(level, _)| {
            let w = (width >> level).max(1);
            let h = (height >> level).max(1);
            w >= 4 && h >= 4
        })
        .map(|(level, px)| {
            let w = (width >> level).max(1);
            let h = (height >> level).max(1);
            if bc3 {
                crate::texture_bcn::encode_bc3(&px, w, h)
            } else {
                crate::texture_bcn::encode_bc1(&px, w, h)
            }
        })
        .collect()
}

/// 纹理句柄，指向 `TextureStore` 中的条目。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct TextureHandle(pub u32);

/// CPU 侧 RGBA8 纹理。
#[derive(Clone, Debug)]
pub struct CpuTexture {
    pub width: u32,
    pub height: u32,
    /// RGBA8，行主序，每像素 4 字节。
    pub rgba: Vec<u8>,
    /// 色彩空间标注：`true` = sRGB 编码（颜色数据，硬件解码到线性），
    /// `false` = 线性数据（法线 / ORM 等数据贴图）。见模块文档。
    pub srgb: bool,
}

impl CpuTexture {
    /// 1x1 纯色纹理（常用于无贴图的纯色材质）。
    pub fn solid(color: [u8; 4]) -> Self {
        Self {
            width: 1,
            height: 1,
            rgba: color.to_vec(),
            srgb: true,
        }
    }

    /// 程序化棋盘格纹理。
    pub fn checkerboard(tiles: u32, cell: u32, a: [u8; 4], b: [u8; 4]) -> Self {
        let size = tiles * cell;
        let mut rgba = Vec::with_capacity((size * size * 4) as usize);
        for y in 0..size {
            for x in 0..size {
                let c = if ((x / cell) + (y / cell)) % 2 == 0 {
                    a
                } else {
                    b
                };
                rgba.extend_from_slice(&c);
            }
        }
        Self {
            width: size,
            height: size,
            rgba,
            srgb: true,
        }
    }

    /// 程序化砖墙法线 / ORM 纹理对（R42，写实材质通道验收数据）。
    ///
    /// 纯函数：同一输入逐位同一输出（确定性测试可直接 `to_bits` 断言）。
    /// 从解析式**高度场** `h(u, v)` 出发：
    /// - 砖体内部高度 1，砖缝（半宽 `seam`，交错排列 running bond）平滑
    ///   下降到 0，过渡带用 smoothstep（`t²(3−2t)`）避免硬边；
    /// - 每砖振幅 / 粗糙度经 splitmix64 哈希（`mix64`，无状态、无全局
    ///   随机源——确定性文化，见 AGENTS.md）做微小变化；
    /// - 法线 = 高度场中心差分 `normalize(−k·∂h/∂u, −k·∂h/∂v, 1)`
    ///   编码到 RGB（*0.5+0.5 → u8），A 恒 255；
    /// - ORM = R: AO（砖缝遮蔽，缝内变暗）、G: 粗糙度（缝大磨面小）、
    ///   B: 金属度（砖墙恒 0），A 恒 255。
    ///
    /// 两张纹理均为**线性数据贴图**（`srgb = false`，见模块文档）。
    /// 已知限制：行交错偏移 0.5 使纹理左右边界不无缝（u = 0/1 处砖列
    /// 截断）；u 方向平铺大于 1 时边界会看到缝——墙面按单块铺设或接受
    /// 边缝即可，无缝环绕留待按需补。
    ///
    /// `rows × cols` 块砖、边长 `size` 像素；`bump` 为法线强度
    /// （中心差分步长系数，0.5～4 合理；0 退化为平整法线）。
    pub fn brick_normal_orm(rows: u32, cols: u32, size: u32, seam: f32, bump: f32) -> (Self, Self) {
        assert!(size >= 4, "砖墙纹理边长至少 4 像素");
        assert!(rows >= 1 && cols >= 1, "砖行列数至少 1");
        let seam = seam.clamp(0.01, 0.45);
        let normal_bytes = (size * size * 4) as usize;
        let mut normal = Vec::with_capacity(normal_bytes);
        let mut orm = Vec::with_capacity(normal_bytes);

        // smoothstep（f32 纯函数；`sqrt` 直接用 f32::sqrt——本函数为
        // 程序化美术数据生成，非物理确定性路径，库函数差异不涉门禁）
        let smooth = |t: f32| {
            let t = t.clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        // splitmix64 终结函数（与 particles.rs 的 mix64 同源）：无状态哈希流
        let mix64 = |mut z: u64| {
            z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        };
        // 砖格定位：(u, v) → (到最近砖缘的距离, 行号, 列号)。
        // 行号用于奇数行半砖交错（running bond）。
        let brick_coords = |u: f32, v: f32| -> (f32, u64, u64) {
            let row = (v * rows as f32).floor() as u64;
            let shifted = u * cols as f32 + (row & 1) as f32 * 0.5;
            let col = shifted.floor();
            let fu = shifted - col; // 砖内横向 [0, 1)
            let fv = v * rows as f32 - row as f32; // 砖内纵向 [0, 1)
            let d = fu.min(1.0 - fu).min(fv.min(1.0 - fv));
            (d, row, col as u64)
        };
        // 每砖微小变化的哈希种子（振幅 / 粗糙度）
        let brick_seed =
            |row: u64, col: u64| -> u64 { mix64(row.wrapping_mul(0x517c_c1b7_2722_0a95) ^ col) };
        // 高度场：u ∈ [0, 1) 横向、v ∈ [0, 1) 纵向；返回值域 [0, 1]
        let height = |u: f32, v: f32| -> f32 {
            let (d, row, col) = brick_coords(u, v);
            // 每砖微小变化（振幅 0.9..1.0）
            let brick = brick_seed(row, col);
            let amp = 0.9 + 0.1 * ((brick & 0xff) as f32 / 255.0);
            let t = (d / seam).min(1.0); // 缝内过渡 0→1
            if d < seam {
                0.35 * amp * smooth(t)
            } else {
                let rise = ((d - seam) / 0.25).min(1.0);
                (0.35 + 0.65 * smooth(rise)) * amp
            }
        };
        let eps = 1.0 / size as f32; // 中心差分半步（1 像素）
        let step = 1.5 * eps;
        for y in 0..size {
            for x in 0..size {
                let u = (x as f32 + 0.5) / size as f32;
                let v = (y as f32 + 0.5) / size as f32;
                // 高度中心差分 → 切线空间法线（∂h/∂v 取负：凹缝应表现为
                // 下陷而非凸起）
                let dhdu = (height(u + step, v) - height(u - step, v)) / (2.0 * step);
                let dhdv = (height(u, v + step) - height(u, v - step)) / (2.0 * step);
                let len = (dhdu * dhdu * bump * bump + dhdv * dhdv * bump * bump + 1.0).sqrt();
                let nx = -dhdu * bump / len;
                let ny = -dhdv * bump / len;
                let nz = 1.0 / len;
                normal.extend_from_slice(&[
                    ((nx * 0.5 + 0.5) * 255.0) as u8,
                    ((ny * 0.5 + 0.5) * 255.0) as u8,
                    ((nz * 0.5 + 0.5) * 255.0) as u8,
                    255,
                ]);
                // ORM：AO（缝暗）+ 粗糙度（缝大磨面小）+ 金属度 0
                let (d_edge, row, col) = brick_coords(u, v);
                let brick = brick_seed(row, col);
                let mortar = smooth((seam - d_edge) / seam); // 1 = 缝心
                let rough_brick = 0.45 + 0.2 * (((brick >> 8) & 0xff) as f32 / 255.0);
                let rough = mortar * 0.95 + (1.0 - mortar) * rough_brick;
                let ao = 1.0 - 0.55 * mortar;
                orm.extend_from_slice(&[(ao * 255.0) as u8, (rough * 255.0) as u8, 0, 255]);
            }
        }
        (
            Self {
                width: size,
                height: size,
                rgba: normal,
                srgb: false,
            },
            Self {
                width: size,
                height: size,
                rgba: orm,
                srgb: false,
            },
        )
    }
}

/// CPU 侧纹理目录（World 资源）。0 号槽位固定为白色纯色纹理。
#[derive(Resource)]
pub struct TextureStore {
    textures: Vec<CpuTexture>,
    /// 经 manifest 声明走 BC3 上传的句柄表（R59，见模块文档）。
    /// 缺席 = 既有 RGBA8 / BC1 决策树逐位不变——默认路径零改动。
    bc3_declared: BTreeSet<u32>,
    /// 声明**不走 BC 块压缩**的句柄表（R66e）：数据贴图（法线 / ORM，
    /// `srgb = false`）走 RGBA8 + mip 链。BC1 的通道耦合端点插值在法线
    /// 图上产生近距块状伪梯度（iris-exp frictions ⑨：皮肤近距块斑，
    /// 同图 Blender 无压缩渲染干净 ⇒ 压缩为病灶）；缺席 = BC 决策树逐位
    /// 不变（引擎原生资产内存优先），glTF 加载器为法线 / ORM 自动声明
    /// （外部资产质量优先）。
    uncompressed: BTreeSet<u32>,
}

impl Default for TextureStore {
    fn default() -> Self {
        Self {
            textures: vec![CpuTexture::solid([255, 255, 255, 255])],
            bc3_declared: BTreeSet::new(),
            uncompressed: BTreeSet::new(),
        }
    }
}

impl TextureStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, texture: CpuTexture) -> TextureHandle {
        self.textures.push(texture);
        TextureHandle(self.textures.len() as u32 - 1)
    }

    /// 登记纹理并**同时声明 BC3 压缩**（R59）：等价 `add` + `declare_bc3`，
    /// 一步完成「导入期 manifest 标志 → 入库声明」。典型消费方是清单
    /// 驱动的 GLB 导入路径。
    pub fn add_bc3_declared(&mut self, texture: CpuTexture) -> TextureHandle {
        let handle = self.add(texture);
        self.bc3_declared.insert(handle.0);
        handle
    }

    /// 登记纹理并**同时声明无压缩**（C1 · 闭合 §8.45）：等价 `add` +
    /// `declare_uncompressed` 一步完成。面向**引擎原生数据贴图**——程序化
    /// 生成的法线 / ORM（如 [`CpuTexture::brick_normal_orm`]）不经 glTF
    /// 加载器、拿不到自动声明，此前只能命中 BC1 决策树（近距块状伪梯度，
    /// frictions ⑨ 同款机理）。与 R66e 声明门同纪律：**前置否决**只影响
    /// 该句柄的上传路径，未声明的原生资产维持 BC1 口径（既有基线零漂移）。
    pub fn add_uncompressed_declared(&mut self, texture: CpuTexture) -> TextureHandle {
        let handle = self.add(texture);
        self.uncompressed.insert(handle.0);
        handle
    }

    /// 为已存在的句柄补 BC3 声明（R59）：GLB / 场景先经常规入口 spawn
    /// （纹理已入目录），消费方再按 manifest 标志逐句柄补声明——声明
    /// 只需发生在**首次 GPU 上传前**（渲染器懒上传，首帧 draw 才编码）。
    /// 返回是否新插入（重复声明幂等，返回 false）。
    pub fn declare_bc3(&mut self, handle: TextureHandle) -> bool {
        self.bc3_declared.insert(handle.0)
    }

    /// 该句柄是否声明了 BC3 压缩（渲染器决策树与显存估算共用的查询口）。
    pub fn is_bc3_declared(&self, handle: TextureHandle) -> bool {
        self.bc3_declared.contains(&handle.0)
    }

    /// 为已存在的句柄补**无压缩**声明（R66e）：数据贴图（法线 / ORM）
    /// 不走 BC1/BC3 块压缩（BC1 对法线的近距块状伪梯度，frictions ⑨）。
    /// 声明只需发生在**首次 GPU 上传前**（渲染器懒上传）；重复声明幂等。
    pub fn declare_uncompressed(&mut self, handle: TextureHandle) {
        self.uncompressed.insert(handle.0);
    }

    /// 该句柄是否声明了无压缩（渲染器决策树与显存估算共用的查询口）。
    pub fn is_uncompressed_declared(&self, handle: TextureHandle) -> bool {
        self.uncompressed.contains(&handle.0)
    }

    pub fn get(&self, handle: TextureHandle) -> Option<&CpuTexture> {
        self.textures.get(handle.0 as usize)
    }

    pub fn len(&self) -> usize {
        self.textures.len()
    }

    pub fn is_empty(&self) -> bool {
        self.textures.is_empty()
    }

    /// CPU 源内存占用（字节）：Σ 宽×高×4（RGBA8 源数据，不含 Vec 的
    /// 分配器开销）。「低占用」卖点的观测口径之一。
    pub fn memory_bytes(&self) -> u64 {
        self.textures
            .iter()
            .map(|t| t.width as u64 * t.height as u64 * 4)
            .sum()
    }

    /// 单张纹理的上传口径字节数（`gpu_bytes_estimate` 与 mip 感知估算
    /// 共用的决策树——两口径之差恒等于 mip 链的增量，不含其他因素）。
    fn texture_gpu_bytes(&self, index: usize, t: &CpuTexture, feature_bc: bool) -> u64 {
        let handle = TextureHandle(index as u32);
        // 无压缩声明（R66e）优先于一切 BC 路径（数据贴图质量优先）
        if self.uncompressed.contains(&handle.0) {
            return t.width as u64 * t.height as u64 * 4;
        }
        if feature_bc
            && self.bc3_declared.contains(&handle.0)
            && crate::texture_bcn::bc3_eligible(t.width, t.height, &t.rgba, feature_bc)
        {
            (t.width as u64 / 4) * (t.height as u64 / 4) * 16
        } else if crate::texture_bcn::bc1_eligible(t.width, t.height, &t.rgba, feature_bc) {
            (t.width as u64 / 4) * (t.height as u64 / 4) * 8
        } else {
            t.width as u64 * t.height as u64 * 4
        }
    }

    /// GPU 侧显存占位**估算**（非精确值：驱动可能对齐 / 复制分配，
    /// 采样器等附属对象不计）。按渲染器上传决策树逐纹理计算（R30 / R59）：
    /// - BC3 路径（声明过且 `feature_bc` 为真、尺寸 4 对齐、带 alpha，
    ///   决策见 [`crate::texture_bcn::bc3_eligible`]）：4×4 块 16 字节
    ///   = 1 B/px；
    /// - BC1 路径（`feature_bc` 为真且尺寸 4 对齐且全不透明）：4×4 块
    ///   8 字节 = 0.5 B/px；
    /// - 其余（含 web / 无特性 / 非对齐 / 未声明的半透明回退）：RGBA8
    ///   4 B/px。
    ///
    /// mipmap v1 无（单 mip 计）；同一 `feature_bc` 取值下与渲染器实际上传
    /// 格式一致（决策函数共用），`false` / `true` 两次调用的差即压缩节省。
    /// 分层互斥（全不透明即使声明 BC3 也归 BC1，0.5 B/px 优于 1 B/px）。
    pub fn gpu_bytes_estimate(&self, feature_bc: bool) -> u64 {
        self.textures
            .iter()
            .enumerate()
            .map(|(index, t)| self.texture_gpu_bytes(index, t, feature_bc))
            .sum()
    }

    /// mip 感知显存估算（R66 / R66d）：[`Self::gpu_bytes_estimate`] 口径
    /// 之上补算完整 mip 链增量——RGBA8 路径 Σ w_k·h_k·4（几何级数 ≈ 基级
    /// +1/3）；BC1 / BC3 路径 Σ 各级块流字节（R66d：压缩路径 mip 化，
    /// 增量 ≈ 基级的 +1/3 × 压缩率）。**假设 `MipSettings` 在场**（缺席
    /// 时上传为单 mip，估算偏保守）；1×1 纹理链长 1，无增量。旧
    /// [`Self::gpu_bytes_estimate`] 签名与数值口径不动（既有测试零改动）。
    pub fn gpu_bytes_estimate_mipped(&self, feature_bc: bool) -> u64 {
        self.textures
            .iter()
            .enumerate()
            .map(|(index, t)| {
                let base = self.texture_gpu_bytes(index, t, feature_bc);
                let handle = TextureHandle(index as u32);
                // 无压缩声明（R66e）= RGBA8 链口径
                let raw = self.uncompressed.contains(&handle.0);
                let bc3 = !raw
                    && feature_bc
                    && self.bc3_declared.contains(&handle.0)
                    && crate::texture_bcn::bc3_eligible(t.width, t.height, &t.rgba, feature_bc);
                let bc1 = !raw
                    && !bc3
                    && crate::texture_bcn::bc1_eligible(t.width, t.height, &t.rgba, feature_bc);
                let mut extra = 0u64;
                let (mut w, mut h) = (t.width.max(1), t.height.max(1));
                while w > 1 || h > 1 {
                    w = (w / 2).max(1);
                    h = (h / 2).max(1);
                    // BC 链止于 4×4（压缩 copy extent 块宽整数倍约束，
                    // 与 mip_chain_bc 同口径）；RGBA8 链到 1×1
                    if (bc3 || bc1) && (w < 4 || h < 4) {
                        break;
                    }
                    extra += if bc3 {
                        crate::texture_bcn::encoded_len_bc3(w, h)
                    } else if bc1 {
                        crate::texture_bcn::encoded_len(w, h)
                    } else {
                        w as u64 * h as u64 * 4
                    };
                }
                base + extra
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkerboard_dimensions_and_colors() {
        let tex = CpuTexture::checkerboard(2, 4, [0, 0, 0, 255], [255, 255, 255, 255]);
        assert_eq!(tex.width, 8);
        assert_eq!(tex.height, 8);
        assert_eq!(tex.rgba.len(), 8 * 8 * 4);
        // 左上角第一格为 a
        assert_eq!(&tex.rgba[0..4], &[0, 0, 0, 255]);
        // (4,0) 属于第二列 → b（像素起始字节 = (y×8+x)×4 = 16）
        let idx = 4 * 4;
        assert_eq!(&tex.rgba[idx..idx + 4], &[255, 255, 255, 255]);
    }

    #[test]
    fn store_zero_is_white_fallback() {
        let mut store = TextureStore::new();
        assert_eq!(
            store.get(TextureHandle(0)).unwrap().rgba,
            vec![255, 255, 255, 255]
        );
        let handle = store.add(CpuTexture::solid([255, 0, 0, 255]));
        assert_eq!(handle, TextureHandle(1));
        assert_eq!(store.get(handle).unwrap().rgba, vec![255, 0, 0, 255]);
        assert!(store.get(TextureHandle(99)).is_none());
    }

    /// 内存统计（R30）：memory_bytes = Σ 源 RGBA 字节；gpu_bytes_estimate
    /// 按上传决策树计——BC 特性下 4 对齐不透明纹理 0.5 B/px，其余 4 B/px；
    /// 两次估算之差即压缩节省。
    #[test]
    fn memory_stats_track_source_and_gpu_estimate() {
        let mut store = TextureStore::new();
        // 0 号兜底：1×1 纯色（非 4 对齐 → 恒走 RGBA 路径）
        assert_eq!(store.memory_bytes(), 4);
        assert_eq!(store.gpu_bytes_estimate(true), 4);
        // 8×8 棋盘（不透明、4 对齐）：特性开启走 BC1 = 2 块×2 块×8 B = 32 B
        let checker = CpuTexture::checkerboard(2, 4, [0, 0, 0, 255], [255, 255, 255, 255]);
        assert!(checker.rgba.iter().all(|&b| b == 255 || b == 0));
        store.add(checker);
        assert_eq!(store.memory_bytes(), 4 + 8 * 8 * 4);
        assert_eq!(store.gpu_bytes_estimate(true), 4 + 32);
        // 同一 store 关闭特性：全 RGBA
        assert_eq!(store.gpu_bytes_estimate(false), 4 + 8 * 8 * 4);
    }

    /// BC3 声明（R59）：声明过的句柄在显存估算中按 1 B/px 计（4 对齐 +
    /// 带 alpha），未声明纹理的估算与既有口径逐字节一致（默认路径零
    /// 改动）；全不透明纹理即使声明 BC3 也归 BC1 口径（分层互斥）；
    /// 重复声明幂等。
    #[test]
    fn bc3_declaration_gates_gpu_estimate() {
        let mut store = TextureStore::new();
        // 64×64 半透明棋盘：未声明 → RGBA8 4 B/px = 16 384
        let mut translucent = CpuTexture::checkerboard(8, 8, [0, 0, 0, 40], [255, 255, 255, 255]);
        for px in translucent.rgba.chunks_exact_mut(4).step_by(2) {
            px[3] = 40;
        }
        let handle = store.add(translucent.clone());
        assert!(!store.is_bc3_declared(handle), "新登记纹理无声明");
        assert_eq!(
            store.gpu_bytes_estimate(true),
            4 + 64 * 64 * 4,
            "未声明半透明纹理恒走 RGBA8 口径"
        );
        // 补声明 → BC3 口径：64×64 = 16×16 = 256 块 × 16 B = 4 096（1 B/px，
        // 源 RGBA8 16 384 B 的 1/4）
        assert!(store.declare_bc3(handle), "首次声明应报告新插入");
        assert!(!store.declare_bc3(handle), "重复声明幂等");
        assert_eq!(store.gpu_bytes_estimate(true), 4 + 256 * 16);
        // 无 BC 特性：声明无效，回 RGBA8
        assert_eq!(store.gpu_bytes_estimate(false), 4 + 64 * 64 * 4);
        // 一步登记 + 声明
        let h2 = store.add_bc3_declared(translucent);
        assert!(store.is_bc3_declared(h2));
        // 全不透明声明 BC3：分层规则归 BC1 口径（256 块 × 8 B）而非 BC3
        let opaque = CpuTexture::checkerboard(8, 8, [0, 0, 0, 255], [255, 255, 255, 255]);
        let h3 = store.add_bc3_declared(opaque);
        assert_eq!(
            store.gpu_bytes_estimate(true) - 4 - 2 * 256 * 16,
            256 * 8,
            "全不透明已声明纹理应按 BC1 口径计"
        );
        assert!(store.is_bc3_declared(h3));
        let _ = h2;
    }

    /// 色彩空间标注（R42）：程序化颜色纹理恒 sRGB（与 R42 前一律按
    /// sRGB 上传的既有行为逐位一致）；砖墙法线 / ORM 对恒线性。
    #[test]
    fn color_space_flags() {
        assert!(CpuTexture::solid([1, 2, 3, 4]).srgb);
        assert!(CpuTexture::checkerboard(2, 2, [0; 4], [255; 4]).srgb);
        let (normal, orm) = CpuTexture::brick_normal_orm(4, 2, 64, 0.1, 2.0);
        assert!(!normal.srgb, "法线是数据贴图，必须线性采样");
        assert!(!orm.srgb, "ORM 是数据贴图，必须线性采样");
    }

    /// 砖墙纹理对（R42）：尺寸与确定性（同输入逐位同输出）；砖心法线
    /// 应为「无扰动」（±1，编码 127/128 —— 0.5 编码到 u8 截断为 127，
    /// 容差 ±1 的依据）；缝心 AO 应显著暗于砖心、粗糙度高于砖心。
    #[test]
    fn brick_normal_orm_is_deterministic_and_semantically_correct() {
        let (n1, o1) = CpuTexture::brick_normal_orm(4, 2, 64, 0.1, 2.0);
        let (n2, o2) = CpuTexture::brick_normal_orm(4, 2, 64, 0.1, 2.0);
        assert_eq!(n1.width, 64);
        assert_eq!(n1.height, 64);
        assert_eq!(n1.rgba, n2.rgba, "同输入必须逐位同输出（确定性）");
        assert_eq!(o1.rgba, o2.rgba);
        assert_eq!(n1.rgba.len(), 64 * 64 * 4);
        // 砖心（远离缝）：法线 ≈ (0, 0, 1)。取 v = 0.5 附近、第一行砖中央
        // u ≈ 0.25×n 格的像素——直接扫全图验证「远离缝的像素 B 通道 ≥ 250
        // 且 R/G 接近中性」这一整体语义，避免依赖具体哈希值。
        for (i, px) in n1.rgba.chunks_exact(4).enumerate() {
            let (x, y) = ((i % 64) as f32, (i / 64) as f32);
            let d = edge_dist(x, y, 4, 2);
            if d > 0.42 {
                let r = px[0] as i32;
                let g = px[1] as i32;
                let b = px[2] as i32;
                assert!((r - 127).abs() <= 1, "远缝像素法线 R 应中性：{r}");
                assert!((g - 127).abs() <= 1, "远缝像素法线 G 应中性：{g}");
                assert!(b >= 250, "远缝像素法线 B 应≈1：{b}");
            }
        }
        // 缝心 vs 砖心：AO 显著暗、粗糙度显著大
        let mut seam_ao = 255;
        let mut brick_ao = 0;
        let mut seam_rough = 0;
        let mut brick_rough = 255;
        for (i, px) in o1.rgba.chunks_exact(4).enumerate() {
            let (x, y) = ((i % 64) as f32, (i / 64) as f32);
            let d = edge_dist(x, y, 4, 2);
            if d < 0.02 {
                seam_ao = seam_ao.min(px[0]);
                seam_rough = seam_rough.max(px[1]);
            } else if d > 0.42 {
                brick_ao = brick_ao.max(px[0]);
                brick_rough = brick_rough.min(px[1]);
            }
        }
        assert!(
            seam_ao + 40 < brick_ao,
            "缝心 AO（{seam_ao}）必须显著暗于砖心（{brick_ao}）"
        );
        assert!(
            seam_rough > brick_rough + 40,
            "缝心粗糙度（{seam_rough}）必须显著大于砖心（{brick_rough}）"
        );
        // B 通道（金属度）：砖墙恒 0
        assert!(o1.rgba.chunks_exact(4).all(|p| p[2] == 0));
    }

    /// 测试辅助：与 `brick_normal_orm` 内部 `brick_coords` 一致的砖缘
    /// 距离（行交错半砖），供语义断言定位「缝心 / 砖心」像素。
    fn edge_dist(x: f32, y: f32, rows: u32, cols: u32) -> f32 {
        let u = (x + 0.5) / 64.0;
        let v = (y + 0.5) / 64.0;
        let row = (v * rows as f32).floor() as u64;
        let shifted = u * cols as f32 + (row & 1) as f32 * 0.5;
        let col = shifted.floor();
        let fu = shifted - col;
        let fv = v * rows as f32 - row as f32;
        fu.min(1.0 - fu).min(fv.min(1.0 - fv))
    }

    /// 链长公式（R66）：1 + floor(log2(max(w, h)))；1×1 终止于单级，
    /// 长方形按长边计。
    #[test]
    fn mip_level_count_formula() {
        assert_eq!(mip_level_count(1, 1), 1, "1×1 链长恒 1（白色兜底无需特判）");
        assert_eq!(mip_level_count(2, 2), 2);
        assert_eq!(mip_level_count(4, 4), 3);
        assert_eq!(mip_level_count(8, 8), 4);
        assert_eq!(mip_level_count(1024, 64), 11, "长方形按长边计");
        assert_eq!(mip_level_count(5, 5), 3, "奇数：5→2→1");
        assert_eq!(mip_level_count(3, 3), 2, "奇数：3→1");
    }

    /// 2×2 box 降采样精确值（R66）：均匀场不变；黑白格均值 floor 舍入；
    /// 奇数维末行/列丢弃（floor 链语义）；一维为 1 时边缘 clamp 权重
    /// 加倍。
    #[test]
    fn downsample_rgba8_exact_values() {
        // 均匀场恒等（R 通道 100、其余 0）
        let mut flat = vec![0u8; 2 * 2 * 4];
        for px in flat.chunks_exact_mut(4) {
            px[0] = 100;
        }
        let out = downsample_rgba8(2, 2, &flat);
        assert_eq!(out, vec![100, 0, 0, 0]);
        // 2×2 棋盘：四像素 R = 0/255/255/0 → floor(510/4) = 127
        let mut tex = vec![0u8; 2 * 2 * 4];
        for (i, px) in tex.chunks_exact_mut(4).enumerate() {
            px[0] = if i % 2 == 0 { 0 } else { 255 };
        }
        let out = downsample_rgba8(2, 2, &tex);
        assert_eq!(out[0], 127, "floor(510/4) = 127（精确舍入依据）");
        // 4×1 → 2×1：奇数高度 = 1，两行采样 clamp 到同一行（边缘权重
        // 加倍不改变均值口径）；列对 (0,1)、(2,3) 均值
        let mut row = vec![0u8; 4 * 4];
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            px[0] = [10, 20, 30, 40][x];
        }
        let out = downsample_rgba8(4, 1, &row);
        assert_eq!(
            out,
            vec![15, 0, 0, 0, 35, 0, 0, 0],
            "行对均值 (10+20)/2、(30+40)/2"
        );
        // 3×1 → 1×1：奇数宽，列 2 丢弃——均值 = (10+20)/2 = 15 而非
        // (10+20+30)/3；这是「奇数维末列不参与」的可判别证据
        let mut row3 = vec![0u8; 3 * 4];
        for (x, px) in row3.chunks_exact_mut(4).enumerate() {
            px[0] = [10, 20, 30][x];
        }
        let out = downsample_rgba8(3, 1, &row3);
        assert_eq!(out, vec![15, 0, 0, 0], "末列 30 应被丢弃（floor 链语义）");
        // 1×4 → 1×2：宽为 1，列采样 clamp（两列同列，权重加倍）；
        // 行对 (0,1)、(2,3) 均值不受影响
        let mut col = vec![0u8; 4 * 4];
        for (y, px) in col.chunks_exact_mut(4).enumerate() {
            px[0] = [8, 16, 32, 64][y];
        }
        let out = downsample_rgba8(1, 4, &col);
        assert_eq!(
            out,
            vec![12, 0, 0, 0, 48, 0, 0, 0],
            "1×N：列 clamp + 行对均值"
        );
        // 1×1：链终止，输出 = 输入
        let one = vec![7u8, 8, 9, 255];
        assert_eq!(downsample_rgba8(1, 1, &one), one);
    }

    /// mip 链生成（R66）：级数 = 公式值、逐级维数 max(1, dim/2)、末级
    /// 1×1、第 0 级 = 源数据；同输入双跑逐位一致（确定性）。
    #[test]
    fn mip_chain_shape_and_determinism() {
        let src = CpuTexture::checkerboard(4, 4, [0, 0, 0, 255], [255, 255, 255, 255]);
        let levels = mip_chain_rgba8(src.width, src.height, &src.rgba);
        assert_eq!(levels.len(), mip_level_count(16, 16) as usize);
        assert_eq!(levels[0], src.rgba, "第 0 级 = 源数据");
        let (mut w, mut h) = (16u32, 16u32);
        for level in &levels {
            assert_eq!(level.len(), (w * h * 4) as usize, "{w}×{h} 级尺寸");
            w = (w / 2).max(1);
            h = (h / 2).max(1);
        }
        assert_eq!((w, h), (1, 1), "链终止于 1×1");
        let again = mip_chain_rgba8(src.width, src.height, &src.rgba);
        assert_eq!(levels, again, "同输入双跑逐位一致（确定性）");
        // 奇数尺寸：5×5 → 2×2 → 1×1
        let odd = mip_chain_rgba8(5, 5, &[128u8; 5 * 5 * 4]);
        assert_eq!(odd.len(), 3);
        assert_eq!(odd[1].len(), 2 * 2 * 4);
        assert_eq!(odd[2], vec![128; 4], "均匀场 1×1 末级（全通道 128）");
        // alpha 通道逐级独立均值（2×2 全部同色 → 1×1 保值，不串色）
        let chain = mip_chain_rgba8(2, 2, &[0u8, 0, 0, 200].repeat(4));
        assert_eq!(chain[1], vec![0, 0, 0, 200], "alpha 通道独立均值");
    }

    /// mip 感知显存估算（R66 / R66d）：各上传路径均补链增量——RGBA8
    /// 4×4 → +16+4 = +20；BC1 4×4 → 2×2 单块 +8、1×1 单块 +8 = +16；
    /// BC3 同款块大小 ×2 = +32。旧估算方法数值与既有测试口径逐字节一致。
    #[test]
    fn mipped_gpu_estimate_adds_chain_per_path() {
        let mut store = TextureStore::new();
        // 0 号兜底 1×1：链长 1，两口径一致
        assert_eq!(store.gpu_bytes_estimate(false), 4);
        assert_eq!(store.gpu_bytes_estimate_mipped(false), 4);
        // 4×4 半透明纹理（非全不透明 → RGBA 路径）：基级 64，
        // 链增量 = 2×2×4 + 1×1×4 = 20 → 84
        let translucent = CpuTexture::checkerboard(1, 4, [0, 0, 0, 40], [255, 255, 255, 40]);
        store.add(translucent);
        assert_eq!(store.gpu_bytes_estimate(false), 4 + 64);
        assert_eq!(store.gpu_bytes_estimate_mipped(false), 4 + 64 + 16 + 4);
        // 同一纹理开 BC 特性：不透明棋盘走 BC1；8×8 基级 4 块 32 B，
        // 链增量 = 4×4 单块 8 B（链止于 4×4）
        let mut store2 = TextureStore::new();
        let opaque = CpuTexture::checkerboard(1, 8, [0, 0, 0, 255], [255, 255, 255, 255]);
        store2.add(opaque);
        assert_eq!(store2.gpu_bytes_estimate(true), 4 + 32);
        assert_eq!(
            store2.gpu_bytes_estimate_mipped(true),
            4 + 32 + 8,
            "BC1 路径链增量 = 4×4 单块（R66d 压缩路径 mip 化，止于 4×4）"
        );
        // BC3 声明 + alpha：8×8 基级 64 B，链增量 = 4×4 块 16 B
        let mut store3 = TextureStore::new();
        let h = store3.add_bc3_declared(CpuTexture::checkerboard(
            1,
            8,
            [0, 0, 0, 40],
            [255, 255, 255, 255],
        ));
        assert!(store3.is_bc3_declared(h));
        assert_eq!(store3.gpu_bytes_estimate(true), 4 + 64);
        assert_eq!(store3.gpu_bytes_estimate_mipped(true), 4 + 64 + 16);
    }

    /// BC 路径 mip 链（R66d）：级数 = 维数 halve 至 4×4 止（wgpu 压缩
    /// copy extent 块宽整数倍约束）；各级块流长度 = 补边后块数 × 块字节；
    /// 第 0 级与「源图直接单编码」逐位一致（同函数同输入）；同输入双跑
    /// 逐位一致（确定性）。
    #[test]
    fn mip_chain_bc_levels_match_encoder_and_are_deterministic() {
        let src = CpuTexture::checkerboard(1, 8, [10, 20, 30, 255], [200, 180, 160, 255]);
        let chain = mip_chain_bc(src.width, src.height, &src.rgba, false);
        assert_eq!(chain.len(), 2, "8×8 链 = 8×8 + 4×4（止于 4×4）");
        assert_eq!(
            chain[0],
            crate::texture_bcn::encode_bc1(&src.rgba, src.width, src.height),
            "第 0 级 = 源图单编码块流（与单 mip 路径逐位一致）"
        );
        assert_eq!(chain[1].len() as u64, crate::texture_bcn::encoded_len(4, 4));
        let again = mip_chain_bc(src.width, src.height, &src.rgba, false);
        assert_eq!(chain, again, "同输入双跑逐位一致（确定性）");
        // BC3 变体：块 16 B，级数同
        let chain3 = mip_chain_bc(src.width, src.height, &src.rgba, true);
        assert_eq!(chain3.len(), chain.len());
        assert_eq!(
            chain3[0],
            crate::texture_bcn::encode_bc3(&src.rgba, src.width, src.height)
        );
        assert_eq!(
            chain3[1].len() as u64,
            crate::texture_bcn::encoded_len_bc3(4, 4)
        );
        // 4×4 源：链长 1（无增量级），单 mip 形态
        let tiny = CpuTexture::checkerboard(1, 4, [0, 0, 0, 255], [255, 255, 255, 255]);
        assert_eq!(mip_chain_bc(4, 4, &tiny.rgba, false).len(), 1);
    }

    /// 无压缩声明（R66e）：BC1 合格的全不透明纹理声明后，显存估算切
    /// RGBA8 口径（基级 4 B/px + mip 链增量），未声明者维持 BC1 口径；
    /// 重复声明幂等。
    #[test]
    fn uncompressed_declaration_switches_estimate_to_rgba8() {
        let mut store = TextureStore::new();
        let opaque = CpuTexture::checkerboard(1, 8, [0, 0, 0, 255], [255, 255, 255, 255]);
        let h = store.add(opaque);
        assert!(!store.is_uncompressed_declared(h));
        assert_eq!(store.gpu_bytes_estimate(true), 4 + 32, "未声明 = BC1 口径");
        assert_eq!(
            store.gpu_bytes_estimate_mipped(true),
            4 + 32 + 8,
            "未声明 = BC1 链口径（止于 4×4）"
        );
        store.declare_uncompressed(h);
        assert!(store.is_uncompressed_declared(h));
        store.declare_uncompressed(h);
        assert_eq!(
            store.gpu_bytes_estimate(true),
            4 + 8 * 8 * 4,
            "声明后 = RGBA8 基级（8×8×4）"
        );
        assert_eq!(
            store.gpu_bytes_estimate_mipped(true),
            4 + 8 * 8 * 4 + (64 + 16 + 4),
            "声明后 = RGBA8 完整链增量（到 1×1）"
        );
    }

    /// 一步式无压缩登记（C1 · 闭合 §8.45）：`add_uncompressed_declared`
    /// 与「`add` + `declare_uncompressed`」在目录形态 / 声明位 / 显存估算
    /// 三口径下完全等价；未走该口的常规 `add` 不受影响（既有 BC1 基线）。
    #[test]
    fn add_uncompressed_declared_matches_add_plus_declare() {
        let opaque = CpuTexture::checkerboard(1, 8, [0, 0, 0, 255], [255, 255, 255, 255]);
        let mut one_step = TextureStore::new();
        let h1 = one_step.add_uncompressed_declared(opaque.clone());
        let mut two_step = TextureStore::new();
        let h2 = two_step.add(opaque.clone());
        two_step.declare_uncompressed(h2);
        assert_eq!(h1, h2, "句柄分配口径与 add 一致");
        assert_eq!(one_step.len(), two_step.len());
        assert_eq!(
            one_step.get(h1).map(|t| &t.rgba),
            two_step.get(h2).map(|t| &t.rgba),
            "目录内容等价"
        );
        assert!(one_step.is_uncompressed_declared(h1));
        assert_eq!(
            one_step.gpu_bytes_estimate(true),
            two_step.gpu_bytes_estimate(true),
            "一步式与两步式显存估算逐位一致"
        );
        assert_eq!(
            one_step.gpu_bytes_estimate_mipped(true),
            two_step.gpu_bytes_estimate_mipped(true)
        );
        // 对照组：常规 add 的原生数据贴图仍走 BC1 口径（未声明不受波及）
        let mut plain = TextureStore::new();
        let h3 = plain.add(opaque);
        assert!(!plain.is_uncompressed_declared(h3));
        assert_eq!(plain.gpu_bytes_estimate(true), 4 + 32, "未声明 = BC1 口径");
    }
}
