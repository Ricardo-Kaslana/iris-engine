//! 光照贴图运行时（R52c：三阶段之三）——GPU 集成与混合语义。
//!
//! 把 [`bake`](crate::bake)（R52a 跟踪器 / 积分器）+ [`bake_uv`](crate::bake_uv)
//! （R52b UV2 参数化 / 图集）的 CPU 产物接到渲染器上：
//!
//! ```text
//! bake_lightmap(world)                    ← R52b 全链组装
//!   └─ BakedLightmap { atlas, radiance, indirect }
//!        ├─ install_lightmap(world, baked)          （本模块）
//!        │    ├─ LightmapStore::insert → LightmapHandle（CPU 目录）
//!        │    ├─ 逐实体 build_uv2_mesh → MeshStore 新句柄（UV2 注入 + 顶点拆分）
//!        │    └─ 逐实体挂 MeshHandle（换成副本）+ LightmapBinding
//!        └─ 渲染器懒上传：indirect → Rgba16Float 图集纹理（ensure_lightmap）
//!             └─ standard.wgsl：间接项 = select(IBL 环境, 光照贴图辐照, mask)
//! ```
//!
//! # 混合语义（「静态 GI 补动态直射」）
//!
//! 光照贴图存的是 [`BakeSample::indirect`] = **天空（含 AO 遮蔽）+ 一次反弹
//! （含色溢）**，不含烘焙直接光：直接光由运行时的动态方向光（CSM 阴影）与
//! 点光提供，把烘焙直射一起上传会与实时直射**双计**（受光面过曝、烘焙阴影
//! 与 CSM 阴影叠成两层）。片元叠加次序（`standard.wgsl` 的 `fs_main`）：
//!
//! ```text
//! 直接项 = 方向光（CSM 阴影）+ Σ 点光（cubemap 阴影）   ← 光照贴图不参与
//! 间接项 = mask ? 光照贴图辐照 × albedo × kd/π : IBL 环境 × ao
//! 输出   = 直接项 + 间接项 + 美术补光 ambient × ao + emissive
//! ```
//!
//! `mask` 是**材质常量**（`MaterialParams.surface.w`，随材质绑定组按
//! 「材质 × 图集」缓存）：挂 [`LightmapBinding`] 且网格有 UV2 的实体 = 1，
//! 其余 = 0。间接项用 `select(ibl, lightmap, mask > 0.5)` 而非
//! `ibl × (1 − mask) + lightmap × mask`——**`select` 不做任何算术**，
//! mask = 0 时返回值与引入本槽位前是**同一串位**（`x × 1.0 + y × 0.0`
//! 虽然数学上恒等，但把「逐位不变」交给浮点恒等式不如交给语言语义）。
//! 这是 R52c 硬约束「未挂光照贴图的场景渲染逐位不变」的实现根据，
//! 由 `tests/lightmap_render.rs` 的 FNV-1a 全帧基线锁永久看门。
//!
//! 采样零常量：UV2 顶点属性直接存**图集归一化坐标**（不是 chart 局部
//! 坐标），着色器 `textureSample(lightmap, sampler, uv2)` 即可，不需要
//! per-instance 的 `uv_rect` 逆变换 ⇒ 同一图集的全部实体共用一份绑定组，
//! 分批只按图集句柄切段（见 `renderer.rs` 的 `run_length`）。
//!
//! # UV2 注入与顶点拆分（[`build_uv2_mesh`]）
//!
//! UV2 是**逐顶点**属性，而烘焙的图集坐标是**逐三角形角点**的：同一顶点
//! 被分进不同 chart（盒子顶面与侧面）、或同 chart 内因投影重叠被贪心偏移
//! 到不同位置（盒子 ±Y 两面投到同一 XZ 平面）时，一个顶点需要两份 UV2。
//! 处理 = **按需拆分顶点**（复制位置 / 法线 / UV / 切线 / 蒙皮属性后追加，
//! 索引改指新顶点）：拓扑不变（索引数不变 ⇒ `triangles_drawn` 不变）、
//! 逐三角形的 UV2 恒正确（无跨 chart 拉伸 / 串色）。判定用 f32 精确相等
//! （同 chart 同角点 ⇒ 逐位同值 ⇒ 不拆）。
//!
//! 网格是**逐实体**资源而 UV2 是逐实体数据 ⇒ [`install_lightmap`] 为每个
//! 烘焙实体造一份网格副本（共享网格的两个实体各自持有不同图集坐标，
//! 原地写入必然串味）。副本内存计入 [`MeshStore::memory_bytes`]。
//!
//! 局部三角形序号的推导（[`triangle_uvs_by_entity`]）：图集表项只带
//! **全局**三角下标（[`BakeTriangle`] 不含网格内序号），而
//! [`collect_bake_triangles`](crate::bake::collect_bake_triangles) 逐实体
//! 连续、按 `indices.chunks(3)` 顺序 push，[`build_charts`] 不丢三角 ⇒
//! 某实体的全局下标集合是一段连续升序区间，**区间内的秩 = 网格内序号**
//! （函数内断言连续性，收集口径若变则此处 panic 而非静默错位）。
//!
//! # dilation 填充（[`dilate_atlas`]）——chart 边缘黑线的对策
//!
//! chart 角点的 UV2 **恰落在 texel 边界**上（局部坐标是整数 texel）：
//! 双线性采样在边界处对两侧 texel 各取一半权重，而外侧那一半是
//! [`CHART_PAD_TEXELS`] 内边距或 shelf 间隙的**零值** ⇒ 每条 chart 边
//! 一道半 texel 宽的暗线（地板与墙交接处最显眼）。R52b 把这个决定留给
//! c 阶段「视目检」，本轮实测确认必须处理。
//!
//! 做法 = 迭代扩散：每轮把**上一轮已有值**的 texel 的 4 邻域均值写入尚无
//! 值的 texel（双缓冲，轮内不串味），至多 [`LIGHTMAP_DILATE_ITERATIONS`]
//! 轮。跨 chart 串光是安全的：pad ≥ 1 且只读已填值 ⇒ 第一轮里 A 的 pad
//! 只能看到 A 自己的内容（B 的 pad 同轮仍未填），各 chart 独立向外扩。
//!
//! 「有值」的判据是 [`BakedLightmap::covered`] 位图而**不是** `!= Vec3::ZERO`：
//! 封闭暗角（半球全遮 + 无反弹）的真黑值必须原样保留，按零判会被邻域
//! 亮度填掉（漏光）。填充只写未覆盖 texel，已覆盖 texel 逐位不变（单测
//! 钉住）。
//!
//! # GPU 资源
//!
//! - 图集纹理 = **Rgba16Float**（线性 HDR 辐照度，无 sRGB 解码；半精度
//!   对辐照度足够——量级 10⁻²..10¹，相对误差 2⁻¹¹）；[`f32_to_f16`] 是
//!   本模块自带的 IEEE 754 binary16 转换纯函数（RNE 舍入，免引 `half`
//!   依赖，解析值单测钉住）；
//! - 懒上传 + 句柄缓存（`ensure_lightmap`，照 `ensure_texture` 模式）；
//!   未挂图集的材质绑**常驻 1×1 黑**兜底纹理（零初始化即黑，无需写入）；
//! - 采样器 ClampToEdge + 双线性：光照贴图是低频信号，双线性比最近邻
//!   平滑得多；chart 边缘的黑边由 [`dilate_atlas`] 消除（见下）；
//! - 顶点缓冲 UV2 独立一路（[`UV2_LAYOUT`](crate::mesh::UV2_LAYOUT)，
//!   loc13），缺席网格绑常驻零垫（与切线同款）。
//!
//! # v1 限制（诚实清单）
//!
//! - **动态物体不进烘焙**：[`collect_bake_triangles`] 只收静态几何，会动的
//!   实体没有光照贴图（挂上也是错位阴影）⇒ 移动物体吃 IBL 环境间接光，
//!   与静态物体并存时有光照风格差（路线图：光照探针 / SH 体积补动态物体）；
//! - **蒙皮实体不烘焙**（R52a 口径），但蒙皮通路**同样绑 UV2 槽位**——
//!   手工注入 UV2 的静态蒙皮网格能采样，绑定姿势之外的姿态会错位；
//! - **透明路径不采样光照贴图**（透明项按视深排序合批，图集分批与深度序
//!   冲突；玻璃的间接光仍走 IBL）；
//! - 挂光照贴图的实体**失去 IBL 镜面反射项**（光照贴图只存漫反射辐照度，
//!   `select` 是整项替换）⇒ 金属表面在烘焙场景里没有环境反射。
//!   **R62a 起在反射探针覆盖范围内闭合**：片元落在某个已烘焙探针的影响域内
//!   时，着色器在 `select` 之后追加探针镜面项（间接漫反射仍走光照贴图，
//!   「漫反射走贴图 / 镜面走探针」，见 `probe` 模块与 ARCHITECTURE §4.30）；
//!   无探针时本限制原样成立（R52c 语义与逐位兼容不变）。SSR 仍在路线图；
//! - 一次烘焙 = 一张图集 = 一份 [`LightmapHandle`]；多图集场景按句柄切段
//!   绘制（段数上升 ⇒ draw call 上升），无跨图集合并；
//! - ~~**无 mipmap**（单 mip 上传）⇒ 掠射角 / 远处有 texel 走样~~
//!   **C2 闭合**：[`LightmapItem::mip_chain_half_bytes`] 生成线性域 2×2
//!   box 降采样链（光照贴图存线性 HDR ⇒ 线性平均即正确定义，无
//!   `texture.rs::downsample_rgba8` 自认的「编码空间求均值」失真问题），
//!   渲染器随 [`MipSettings`](crate::texture::MipSettings) 在场时多级上传 +
//!   三线性采样器；**MipSettings 缺席 = 单 mip + 采样器逐字段与 R52c
//!   一致，既有基线逐位不变**（与 R66/R66d 纹理路径同纪律）。
//! - dilation 是**各向同性 4 邻域均值扩散**、至多
//!   [`LIGHTMAP_DILATE_ITERATIONS`] 轮 ⇒ 距已覆盖 texel 超过该轮数的空洞
//!   中心仍为黑（chart 内边距 1 texel、shelf 无间隙 ⇒ 实际 1 轮即闭合，
//!   余量留给共形参数化后可能出现的空洞）；
//! - UV2 注入后网格副本常驻（无「卸载光照贴图」路径：恢复原网格需重新
//!   挂回旧 [`MeshHandle`]，调用方自己留档）。

use std::collections::BTreeMap;

use glam::Vec3;
use iris_core::Resource;
use iris_core::prelude::Entity;
use iris_core::world::World;

use crate::bake::BakedLightmap;
use crate::bake_uv::LightmapAtlas;
use crate::mesh::{MeshData, MeshHandle, MeshStore};

/// 「无光照贴图」哨兵槽位值（绘制项的 `lightmap` 字段与材质绑定键用它
/// 表示缺席；[`LightmapHandle`] 的合法值域不含它）。
pub const NO_LIGHTMAP: u32 = u32::MAX;

/// 光照贴图句柄：指向 [`LightmapStore`] 中的一次烘焙产物，也作为
/// [`LightmapBinding`] 的载荷挂到实体上。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, iris_core::Component)]
pub struct LightmapHandle(pub u32);

/// 实体的光照贴图绑定（R52c）：该实体的网格 UV2 从这张图集采样。
///
/// **缺席语义**：组件缺席 = 该实体不吃光照贴图（间接光走 IBL 环境，
/// 与引入本组件前逐位一致）。挂载但网格无 UV2 / 句柄悬垂时同样退回
/// IBL 路径（渲染器 `ensure_material` 的 `mask` 由「图集是否已上传」
/// 决定，见 `renderer.rs`），并各告警一次。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, iris_core::Component)]
pub struct LightmapBinding {
    /// 采样哪一张图集。
    pub lightmap: LightmapHandle,
}

/// dilation 的最大迭代轮数（每轮向外扩 1 texel）。chart 内边距
/// [`CHART_PAD_TEXELS`](crate::bake_uv::CHART_PAD_TEXELS) = 1、shelf 装箱
/// 无间隙 ⇒ 1 轮即闭合，余量留给共形参数化后可能出现的大空洞（见模块
/// 文档诚实清单）。
pub const LIGHTMAP_DILATE_ITERATIONS: usize = 8;

/// 未覆盖 texel 的 dilation 填充（就地修改 `values`，`filled` 同步推进）。
///
/// 每轮：对每个**未填** texel 取其 4 邻域中**已填**者的均值写入（双缓冲
/// ⇒ 同一轮内新填的值不参与本轮扩散，结果与扫描序无关 = 确定性）。
/// 已填 texel 的值逐位不变；无已填邻域的空洞 texel 本轮跳过（下一轮再由
/// 更外的邻居覆盖），`iterations` 轮后仍未填的保持原值。
///
/// `values` / `filled` 长度须为 `size × size`（行主序），否则 panic。
pub fn dilate_atlas(values: &mut [Vec3], filled: &mut [bool], size: usize, iterations: usize) {
    assert_eq!(
        values.len(),
        size * size,
        "辐照数组长度 {} 与图集 {size}×{size} 不符",
        values.len()
    );
    assert_eq!(filled.len(), size * size, "覆盖位图长度与图集不符");
    if size == 0 {
        return;
    }
    let mut scratch = values.to_vec();
    for _ in 0..iterations {
        let mut pending: Vec<(usize, Vec3)> = Vec::new();
        for y in 0..size {
            for x in 0..size {
                let index = y * size + x;
                if filled[index] {
                    continue;
                }
                let mut sum = Vec3::ZERO;
                let mut count = 0u32;
                // 4 邻域：短路求值保证越界下标不被计算（index = 0 时
                // index - 1 会下溢），只读上一轮的 filled 状态
                if x > 0 && filled[index - 1] {
                    sum += scratch[index - 1];
                    count += 1;
                }
                if x + 1 < size && filled[index + 1] {
                    sum += scratch[index + 1];
                    count += 1;
                }
                if y > 0 && filled[index - size] {
                    sum += scratch[index - size];
                    count += 1;
                }
                if y + 1 < size && filled[index + size] {
                    sum += scratch[index + size];
                    count += 1;
                }
                if count > 0 {
                    pending.push((index, sum / count as f32));
                }
            }
        }
        if pending.is_empty() {
            break; // 全部填满（或剩余空洞无已填邻域）⇒ 提前收敛
        }
        for (index, value) in pending {
            scratch[index] = value;
            filled[index] = true;
        }
    }
    values.copy_from_slice(&scratch);
}

/// 目录条目：一次烘焙的 CPU 侧描述（图集布局 + 逐 texel 间接辐照度）。
#[derive(Clone, Debug)]
pub struct LightmapItem {
    /// 图集布局（尺寸 / 已用高度 / chart 与三角形表项——调试与 UV2 复核用）。
    pub atlas: LightmapAtlas,
    /// 逐 texel **间接**辐照度（= [`BakedLightmap::indirect`] 经
    /// [`dilate_atlas`] 填充未覆盖 texel 后的结果；行主序
    /// `index = y × size + x`，行 0 = v 0）。已覆盖 texel 逐位等于烘焙值。
    pub irradiance: Vec<Vec3>,
}

impl LightmapItem {
    /// 图集边长（texel）。
    pub fn size(&self) -> u32 {
        self.atlas.size
    }

    /// RGBA16F 字节流（行主序，8 B/texel，alpha 恒 1.0）——GPU 上传的数据源。
    ///
    /// 行数不足 `size²` 即 panic（[`BakedLightmap`] 的口径由
    /// [`bake_lightmap`](crate::bake::bake_lightmap) 保证对齐）。
    pub fn half_bytes(&self) -> Vec<u8> {
        let size = self.size() as usize;
        assert_eq!(
            self.irradiance.len(),
            size * size,
            "辐照数组长度 {} 与图集 {size}×{size} 不符",
            self.irradiance.len()
        );
        let mut out = Vec::with_capacity(size * size * 8);
        for texel in &self.irradiance {
            for channel in [texel.x, texel.y, texel.z, 1.0] {
                out.extend_from_slice(&f32_to_f16(channel).to_le_bytes());
            }
        }
        out
    }

    /// mip 链（C2 · 闭合「单 mip 上传」条）：级 0 = [`Self::half_bytes`]
    /// 逐位一致；其后逐级 2×2 线性域 box 平均（截断维数序列，奇数维
    /// clamp 边缘样本，与 `downsample_rgba8` 同口径）直到 1×1。
    ///
    /// **线性域求均值即正确定义**：光照贴图存的是线性 HDR 辐照度
    /// （非 sRGB 编码存储），无 `texture.rs::downsample_rgba8` 自认的
    /// 「编码空间平均」失真——这也是纹理侧 BC 链要先解码再重编码的同款
    /// 因果在这里天然免掉。降采样累加序固定（行主序，r 后 c，样本数
    /// 显式除）⇒ 逐位确定。
    pub fn mip_chain_half_bytes(&self) -> Vec<Vec<u8>> {
        let size = self.size() as usize;
        let mut chain = Vec::new();
        chain.push(self.half_bytes());
        let mut current = self.irradiance.clone();
        let mut w = size.max(1);
        let mut h = size.max(1);
        // 级尺寸 = (dim/2).max(1) 截断序列直到 1×1（与 mip_chain_rgba8 /
        // GPU mip 布局同口径；光照图集实际尺寸为 2 的幂，本分支为防御）
        while w > 1 || h > 1 {
            let (next, nw, nh) = downsample_rgb(&current, w, h);
            let mut bytes = Vec::with_capacity(nw * nh * 8);
            for texel in &next {
                for channel in [texel.x, texel.y, texel.z, 1.0] {
                    bytes.extend_from_slice(&f32_to_f16(channel).to_le_bytes());
                }
            }
            chain.push(bytes);
            current = next;
            w = nw;
            h = nh;
        }
        chain
    }
}

/// 线性辐照数组的 2×2 box 降采样。返回（结果，新宽，新高）。新维数 =
/// `(dim/2).max(1)`（截断，与 [`downsample_rgba8`](crate::texture::downsample_rgba8)
/// / GPU mip 布局同式）；采样窗 `2x+{0,1}` / `2y+{0,1}` 按既有口径
/// **clamp 边缘**（维数为 1 时边缘样本权重加倍），权重恒 1、除以 4——
/// 累加序固定（y 行内 x），无浮点重结合自由度，逐位确定。
pub fn downsample_rgb(values: &[Vec3], w: usize, h: usize) -> (Vec<Vec3>, usize, usize) {
    let nw = (w / 2).max(1);
    let nh = (h / 2).max(1);
    let mut out = Vec::with_capacity(nw * nh);
    let clamp_x = |i: usize| i.min(w - 1);
    let clamp_y = |i: usize| i.min(h - 1);
    for y in 0..nh {
        let sy0 = clamp_y(2 * y);
        let sy1 = clamp_y(2 * y + 1);
        for x in 0..nw {
            let sx0 = clamp_x(2 * x);
            let sx1 = clamp_x(2 * x + 1);
            out.push(
                (values[sy0 * w + sx0]
                    + values[sy0 * w + sx1]
                    + values[sy1 * w + sx0]
                    + values[sy1 * w + sx1])
                    / 4.0,
            );
        }
    }
    (out, nw, nh)
}

/// 光照贴图目录（World 资源）。GPU 纹理由渲染器按句柄懒上传
/// （照 `TextureStore` / `RenderTargetStore` 的句柄 + 懒上传模式）。
#[derive(Default, Resource)]
pub struct LightmapStore {
    items: Vec<LightmapItem>,
}

impl LightmapStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 收录一次烘焙产物，返回句柄（**消费** `baked`：`radiance` 是预览 /
    /// 调试口径，运行时只需要 `indirect`，不留双份）。收录时即做
    /// [`dilate_atlas`] 填充（一次烘焙只填一次，上传路径不再重复计算）。
    pub fn insert(&mut self, baked: BakedLightmap) -> LightmapHandle {
        let mut irradiance = baked.indirect;
        let mut filled = baked.covered;
        let size = baked.atlas.size as usize;
        dilate_atlas(
            &mut irradiance,
            &mut filled,
            size,
            LIGHTMAP_DILATE_ITERATIONS,
        );
        self.items.push(LightmapItem {
            atlas: baked.atlas,
            irradiance,
        });
        LightmapHandle(self.items.len() as u32 - 1)
    }

    pub fn get(&self, handle: LightmapHandle) -> Option<&LightmapItem> {
        self.items.get(handle.0 as usize)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// CPU 源内存占用（字节）：Σ 辐照数组 × 12 B（Vec3）。
    /// 「低占用」卖点的观测口径之一（GPU 侧见 [`Self::gpu_bytes_estimate`]）。
    pub fn memory_bytes(&self) -> u64 {
        self.items
            .iter()
            .map(|item| item.irradiance.len() as u64 * 12)
            .sum()
    }

    /// GPU 侧显存占用（字节，精确值）：Σ `size² × 8`（Rgba16Float 单 mip）。
    pub fn gpu_bytes_estimate(&self) -> u64 {
        self.items
            .iter()
            .map(|item| u64::from(item.size()) * u64::from(item.size()) * 8)
            .sum()
    }

    /// 多 mip 口径的 GPU 显存占用（字节，精确值，C2）：Σ 各图集逐 mip 级
    /// `w×h×8` 求和（级尺寸 = `downsample_rgb` 的截断序列，止于 1×1）。
    /// `MipSettings` 在场时与实际上传一致；缺席时实际为
    /// [`Self::gpu_bytes_estimate`]。2 的幂图集链增量为几何级数 ≈ 基级 × 4/3。
    pub fn gpu_bytes_estimate_mipped(&self) -> u64 {
        self.items
            .iter()
            .map(|item| {
                let mut w = item.size().max(1);
                let mut h = item.size().max(1);
                let mut bytes = 0u64;
                loop {
                    bytes += u64::from(w) * u64::from(h) * 8;
                    if w == 1 && h == 1 {
                        break;
                    }
                    w = (w / 2).max(1);
                    h = (h / 2).max(1);
                }
                bytes
            })
            .sum()
    }
}

/// `f32` → IEEE 754 binary16（half）位型，**round-to-nearest-even**。
///
/// 免引 `half` 依赖的纯函数（与 `renderer.rs` 里手写中性法线 f16 字节的
/// 做法同源，但覆盖全值域）。语义：
///
/// - ±0 / ±Inf / NaN（保 quiet 位）原样映射；
/// - half 正规区间 `[2⁻¹⁴, 65504]`：尾数 23 → 10 位 RNE，进位可推到指数，
///   指数溢出 ⇒ ±Inf；
/// - half 次正规区间 `(0, 2⁻¹⁴)`：隐含位参与舍入（`e ∈ [−10, 0]`），
///   低于 `2⁻²⁵` 量级 ⇒ ±0；
/// - `f32` 次正规（`< 2⁻¹²⁶`）恒远小于 half 最小次正规 ⇒ ±0。
pub fn f32_to_f16(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let mant = bits & 0x007f_ffff;
    match exp {
        0 => sign, // ±0 与 f32 次正规：量级 ≤ 2⁻¹²⁶ ⇒ half 舍入到 ±0
        255 => {
            if mant == 0 {
                sign | 0x7c00 // ±Inf
            } else {
                // NaN：保留高位有效位并强制 quiet 位（否则退化成 Inf）
                sign | 0x7c00 | ((mant >> 13) as u16) | 0x0200
            }
        }
        _ => {
            // half 偏差指数：e = E + 15（E = exp − 127）
            let mut e = exp - 127 + 15;
            if e >= 0x1f {
                return sign | 0x7c00; // ≥ 2¹⁶ 必然溢出（> 65520 的溢出阈值）
            }
            if e <= 0 {
                if e < -10 {
                    return sign; // < 2⁻²⁵ ⇒ 舍入到 ±0
                }
                // 次正规：以 2⁻²⁴ 为量子，含隐含位的 24 位有效数右移舍入
                let m = mant | 0x0080_0000;
                let shift = (-e + 14) as u32;
                let half_bit = 1u32 << (shift - 1);
                let mut q = m >> shift;
                if m & half_bit != 0 && (m & (half_bit - 1) != 0 || q & 1 != 0) {
                    q += 1;
                }
                return sign | q as u16;
            }
            // 正规：尾数 23 → 10 位 RNE
            let mut q = mant >> 13;
            let rem = mant & 0x1fff;
            if rem > 0x1000 || (rem == 0x1000 && q & 1 != 0) {
                q += 1;
                if q == 0x400 {
                    q = 0;
                    e += 1;
                    if e >= 0x1f {
                        return sign | 0x7c00; // 尾数进位推到指数溢出
                    }
                }
            }
            sign | ((e as u16) << 10) | q as u16
        }
    }
}

/// 逐三角形的图集角点 UV → 带 UV2 的网格副本（冲突处拆分顶点）。
///
/// `tri_uvs[t]` = 网格第 `t` 个三角形（`indices.chunks(3)` 序）三个角点的
/// **图集归一化坐标**，角点序与索引序一致。算法与拆分理由见模块文档。
///
/// 纯函数：同输入逐位同输出（拆分只按三角形序 / 角点序追加，无哈希、无
/// 浮点比较之外的判据）。返回的网格 `vertices` 可能比输入长（拆分），
/// `indices` 长度不变（拓扑不变）。
pub fn build_uv2_mesh(data: &MeshData, tri_uvs: &[[[f32; 2]; 3]]) -> MeshData {
    assert_eq!(
        data.indices.len() % 3,
        0,
        "网格索引数须为 3 的倍数：{}",
        data.indices.len()
    );
    let triangles = data.indices.len() / 3;
    assert_eq!(
        tri_uvs.len(),
        triangles,
        "UV2 三角形数（{}）与网格三角形数（{triangles}）不符",
        tri_uvs.len()
    );
    let mut out = data.clone();
    out.uv2 = None;
    let mut uv2 = vec![[0.0f32; 2]; data.vertices.len()];
    // 已写入的 UV2（None = 未写）：冲突检测的判据
    let mut assigned: Vec<Option<[f32; 2]>> = vec![None; data.vertices.len()];
    for (t, corners) in tri_uvs.iter().enumerate() {
        for (slot, &uv) in corners.iter().enumerate() {
            let index = data.indices[t * 3 + slot] as usize;
            assert!(
                index < data.vertices.len(),
                "网格索引越界：{index}（顶点数 {}）",
                data.vertices.len()
            );
            match assigned[index] {
                None => {
                    assigned[index] = Some(uv);
                    uv2[index] = uv;
                }
                // f32 精确相等：同 chart 同角点 ⇒ 逐位同值 ⇒ 共用顶点
                Some(prev) if prev == uv => {}
                Some(_) => {
                    // 跨 chart / 重叠偏移 ⇒ 同一顶点要两份 UV2：拆分
                    out.vertices.push(data.vertices[index]);
                    if let (Some(src), Some(dst)) = (&data.skin, &mut out.skin) {
                        dst.push(src[index]);
                    }
                    if let (Some(src), Some(dst)) = (&data.tangents, &mut out.tangents) {
                        dst.push(src[index]);
                    }
                    uv2.push(uv);
                    out.indices[t * 3 + slot] = (out.vertices.len() - 1) as u32;
                }
            }
        }
    }
    out.uv2 = Some(uv2);
    out
}

/// [`install_lightmap`] 的结果（观测口径：注入了多少实体、拆了多少顶点）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LightmapInstall {
    /// 新建的图集句柄（= 挂到实体上的 [`LightmapBinding::lightmap`]）。
    pub lightmap: LightmapHandle,
    /// 注入 UV2 的实体数（= 图集覆盖的实体数）。
    pub entities: usize,
    /// 因跨 chart / 重叠偏移而拆分新增的顶点数。
    pub vertices_added: usize,
}

/// 一个三角形的三顶点图集归一化坐标（角点序 = 索引序）。
type TriCorners = [[f32; 2]; 3];

/// 一个实体的 UV2 注入计划：网格句柄 + 逐**局部**三角形序号的角点坐标。
type EntityUvPlan = (MeshHandle, Vec<TriCorners>);

/// 分组中间态：(全局三角下标, 角点坐标) 列表，排序后秩即局部序号。
type RankedCorners = Vec<(usize, TriCorners)>;

/// 图集表项 → 「实体 → (网格句柄, 逐局部三角形的角点 UV)」（`BTreeMap`：
/// 迭代序 = 实体位升序，确定性文化见 AGENTS.md）。
///
/// 局部三角形序号 = 全局下标在该实体区间内的**秩**（推导与断言见模块文档）。
fn triangle_uvs_by_entity(baked: &BakedLightmap) -> BTreeMap<u64, EntityUvPlan> {
    let inv = 1.0 / baked.atlas.size as f32;
    let mut grouped: BTreeMap<u64, (MeshHandle, RankedCorners)> = BTreeMap::new();
    for entry in &baked.atlas.entries {
        let Some(mesh) = entry.mesh else {
            panic!(
                "图集表项缺 mesh 句柄（实体 {:?}）——install_lightmap 只接受 \
                 bake_lightmap 的组装产物（pack_atlas 纯函数不查世界）",
                entry.entity
            );
        };
        let slot = grouped
            .entry(entry.entity.to_bits())
            .or_insert((mesh, Vec::new()));
        assert_eq!(
            slot.0, mesh,
            "同一实体 {:?} 的图集表项来自不同网格（{:?} 与 {mesh:?}）",
            entry.entity, slot.0
        );
        for tri in &entry.triangles {
            // chart 局部 texel → 图集归一化坐标（v 与 texel 行序一致）
            let corners = [0, 1, 2].map(|k| {
                [
                    (entry.origin[0] as f32 + tri.local[k][0]) * inv,
                    (entry.origin[1] as f32 + tri.local[k][1]) * inv,
                ]
            });
            slot.1.push((tri.triangle, corners));
        }
    }
    grouped
        .into_iter()
        .map(|(bits, (mesh, mut list))| {
            list.sort_unstable_by_key(|&(global, _)| global);
            let mut uvs = vec![[[0.0f32; 2]; 3]; list.len()];
            for (rank, item) in list.iter().enumerate() {
                if rank > 0 {
                    assert_eq!(
                        item.0,
                        list[rank - 1].0 + 1,
                        "实体 {bits:#x} 的图集三角下标不连续（{} 之后是 {}）——\
                         收集口径变了，「秩即网格内序号」的推导失效",
                        list[rank - 1].0,
                        item.0
                    );
                }
                uvs[rank] = item.1;
            }
            (bits, (mesh, uvs))
        })
        .collect()
}

/// 把烘焙产物接入世界（R52c 的一次性装配入口）：
///
/// 1. [`LightmapStore`] 资源缺席则新建，收录 `baked` → [`LightmapHandle`]；
/// 2. 逐烘焙实体用 [`build_uv2_mesh`] 造带 UV2 的网格副本并写入
///    [`MeshStore`]（**每实体一份**——UV2 是逐实体的图集坐标，共享网格
///    必须分裂，见模块文档）；
/// 3. 逐实体替换 [`MeshHandle`] 组件为副本句柄 + 挂 [`LightmapBinding`]。
///
/// 之后照常 `propagate_transforms` 无关（变换不变）、渲染器下一帧懒上传
/// 图集纹理即生效。**烘焙时的实体变换就是光照贴图的定位**：装配后移动
/// 实体会让烘焙阴影错位（静态 GI 的固有语义）。
///
/// 分四阶段借用（读网格 → 写网格 → 写目录 → 写组件），资源守卫与
/// `&mut World` 不重叠（AGENTS.md 的两阶段规约）。空图集合法（返回
/// `entities = 0`，仍建一条目录项）。
pub fn install_lightmap(world: &mut World, baked: BakedLightmap) -> LightmapInstall {
    let by_entity = triangle_uvs_by_entity(&baked);

    // 阶段 1：读网格 → 逐实体造 UV2 副本（记录原顶点数以便统计拆分数）
    let mut plans: Vec<(Entity, usize, MeshData)> = Vec::with_capacity(by_entity.len());
    {
        let meshes = world
            .try_resource::<MeshStore>()
            .expect("世界缺 MeshStore 资源——install_lightmap 需要网格目录");
        for (&bits, (mesh, uvs)) in &by_entity {
            let data = meshes.get(*mesh).unwrap_or_else(|| {
                panic!("网格句柄 {mesh:?} 不在 MeshStore（实体 {bits:#x}）——悬垂句柄")
            });
            assert_eq!(
                uvs.len(),
                data.indices.len() / 3,
                "实体 {bits:#x} 的图集三角数（{}）与网格三角数（{}）不符",
                uvs.len(),
                data.indices.len() / 3
            );
            let base = data.vertices.len();
            plans.push((Entity::from_bits(bits), base, build_uv2_mesh(data, uvs)));
        }
    }
    let vertices_added: usize = plans
        .iter()
        .map(|(_, base, data)| data.vertices.len() - base)
        .sum();

    // 阶段 2：副本写入网格目录
    let handles: Vec<(Entity, MeshHandle)> = {
        let mut meshes = world.resource_mut::<MeshStore>();
        plans
            .into_iter()
            .map(|(entity, _, data)| (entity, meshes.add(data)))
            .collect()
    };

    // 阶段 3：目录收录（资源缺席则新建）
    if !world.contains_resource::<LightmapStore>() {
        world.insert_resource(LightmapStore::new());
    }
    let lightmap = world.resource_mut::<LightmapStore>().insert(baked);

    // 阶段 4：换网格句柄 + 挂绑定（失效实体由 World::insert 告警忽略）
    for &(entity, handle) in &handles {
        world.insert(entity, handle);
        world.insert(entity, LightmapBinding { lightmap });
    }

    LightmapInstall {
        lightmap,
        entities: handles.len(),
        vertices_added,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bake::{BakeParams, bake_lightmap};
    use crate::bake_uv::BakeUvParams;
    use crate::material::{MaterialDef, MaterialStore};
    use crate::mesh::primitives;
    use iris_core::hierarchy::propagate_transforms;
    use iris_core::prelude::{Quat, Transform};

    /// half 位型的解析对照（IEEE 754 binary16，RNE）：零 / 次正规边界 /
    /// 正规 / 尾数进位 / 溢出 / Inf / NaN 逐条手推。
    #[test]
    fn f32_to_f16_matches_ieee754_analytic_values() {
        // ±0
        assert_eq!(f32_to_f16(0.0), 0x0000);
        assert_eq!(f32_to_f16(-0.0), 0x8000, "符号位必须保留");
        // 2 的幂：1.0 = 0x3C00、0.5 = 0x3800、2.0 = 0x4000
        assert_eq!(f32_to_f16(1.0), 0x3C00);
        assert_eq!(f32_to_f16(0.5), 0x3800);
        assert_eq!(f32_to_f16(2.0), 0x4000);
        assert_eq!(f32_to_f16(-1.5), 0xBE00, "1.5 = 0x3E00，符号位 0x8000");
        // 最小正规 2⁻¹⁴ = 0x0400、最大正规 65504 = 0x7BFF
        assert_eq!(f32_to_f16(2.0f32.powi(-14)), 0x0400);
        assert_eq!(f32_to_f16(65504.0), 0x7BFF);
        // 次正规：最小 2⁻²⁴ = 0x0001；2⁻²⁵ 恰为舍入中点 ⇒ RNE 取偶（0）
        assert_eq!(f32_to_f16(2.0f32.powi(-24)), 0x0001);
        assert_eq!(f32_to_f16(2.0f32.powi(-25)), 0x0000, "中点取偶 ⇒ ±0");
        assert_eq!(
            f32_to_f16(3.0 * 2.0f32.powi(-25)),
            0x0002,
            "1.5 量子中点取偶 ⇒ 2"
        );
        assert_eq!(f32_to_f16(2.0f32.powi(-26)), 0x0000);
        // f32 次正规（2⁻¹²⁶ 量级）⇒ 0
        assert_eq!(f32_to_f16(f32::from_bits(1)), 0x0000);
        // 溢出：≥ 2¹⁶ ⇒ Inf；(65504, 65520) 区间仍取最大有限值
        assert_eq!(f32_to_f16(65536.0), 0x7C00);
        assert_eq!(f32_to_f16(-70000.0), 0xFC00);
        assert_eq!(f32_to_f16(65510.0), 0x7BFF, "距 65504 更近 ⇒ 不进 Inf");
        assert_eq!(f32_to_f16(65520.0), 0x7C00, "中点取偶（Inf 尾数 0 为偶）");
        // Inf / NaN（quiet 位必须置起）
        assert_eq!(f32_to_f16(f32::INFINITY), 0x7C00);
        assert_eq!(f32_to_f16(f32::NEG_INFINITY), 0xFC00);
        let nan = f32_to_f16(f32::NAN);
        assert_eq!(nan & 0x7C00, 0x7C00, "NaN 指数全 1");
        assert_ne!(nan & 0x03FF, 0, "NaN 尾数非零");
        assert_ne!(nan & 0x0200, 0, "quiet 位必须保留");
        // π：0x4248（= 3.140625，比 0x4249 = 3.142578 更近）
        assert_eq!(f32_to_f16(std::f32::consts::PI), 0x4248);
    }

    /// half 往返：可精确表示的值转回 f32 逐位相等（用 half → f32 的解析
    /// 式反查，覆盖正规区间的一批 2 的幂与小数）。
    #[test]
    fn f32_to_f16_is_exact_for_representable_values() {
        // half 正规值 = (1 + q/1024) × 2^(e−15)，q ∈ [0,1023]、e ∈ [1,30]
        for e in [1i32, 2, 15, 16, 29, 30] {
            for q in [0u32, 1, 511, 512, 1023] {
                let value = (1.0 + q as f32 / 1024.0) * 2.0f32.powi(e - 15);
                let bits = f32_to_f16(value);
                assert_eq!(
                    bits,
                    ((e as u16) << 10) | q as u16,
                    "{value} 应精确编码为 e={e} q={q}"
                );
            }
        }
    }

    /// UV2 注入：单 chart 平面网格逐顶点写入图集坐标（无拆分）；
    /// 跨 chart 共享顶点时拆分（顶点数增加、索引数不变、原顶点属性保留）。
    #[test]
    fn build_uv2_mesh_writes_corners_and_splits_conflicts() {
        let plane = primitives::plane(2.0); // 4 顶点 / 2 三角
        let uvs = [
            [[0.1, 0.2], [0.3, 0.2], [0.3, 0.4]],
            [[0.1, 0.2], [0.3, 0.4], [0.1, 0.4]],
        ];
        let out = build_uv2_mesh(&plane, &uvs);
        assert_eq!(out.vertices.len(), 4, "对角线两侧角点 UV 互不冲突 ⇒ 不拆分");
        assert_eq!(out.indices, plane.indices, "拓扑不变");
        let uv2 = out.uv2.expect("UV2 已注入");
        assert_eq!(uv2.len(), 4);
        for (i, expected) in [uvs[0][0], uvs[0][1], uvs[0][2], uvs[1][2]]
            .into_iter()
            .enumerate()
        {
            // 索引序 = [0,1,2, 0,2,3] ⇒ 顶点 0/1/2 来自三角 0，顶点 3 = 三角 1 的第三角
            assert_eq!(uv2[i], expected, "顶点 {i} 的 UV2");
        }

        // 冲突：两个三角都要顶点 0，但 UV 不同 ⇒ 顶点 0 保留首份、
        // 第二份拆分成新顶点（索引改指）
        let conflict = [
            [[0.1, 0.2], [0.3, 0.2], [0.3, 0.4]],
            [[0.9, 0.9], [0.3, 0.4], [0.1, 0.4]],
        ];
        let split = build_uv2_mesh(&plane, &conflict);
        assert_eq!(split.vertices.len(), 5, "冲突角点拆出一个新顶点");
        assert_eq!(split.indices.len(), plane.indices.len(), "索引数不变");
        assert_eq!(split.indices[3], 4, "第二三角的首角改指新顶点");
        assert_eq!(split.uv2.as_ref().unwrap()[0], [0.1, 0.2], "首者得");
        assert_eq!(split.uv2.as_ref().unwrap()[4], [0.9, 0.9], "拆出者带新值");
        // 拆分顶点复制全部属性（位置 / 法线 / UV / 切线 / 蒙皮）
        assert_eq!(split.vertices[4].position, plane.vertices[0].position);
        assert_eq!(split.vertices[4].normal, plane.vertices[0].normal);
        assert_eq!(split.vertices[4].uv, plane.vertices[0].uv);
        let mut with_tangents = plane.clone();
        with_tangents.tangents = Some(vec![[1.0, 0.0, 0.0, 1.0]; 4]);
        let split_t = build_uv2_mesh(&with_tangents, &conflict);
        assert_eq!(
            split_t.tangents.as_ref().unwrap().len(),
            5,
            "切线属性必须跟着拆分"
        );
        // 确定性：同输入两次逐位一致
        let again = build_uv2_mesh(&plane, &conflict);
        assert_eq!(again.vertices.len(), split.vertices.len());
        assert_eq!(again.uv2, split.uv2);
        assert_eq!(again.indices, split.indices);
    }

    /// UV2 三角形数与网格不符 ⇒ panic（注入错位会静默串光，必须报错）。
    #[test]
    #[should_panic(expected = "UV2 三角形数（1）与网格三角形数（2）不符")]
    fn build_uv2_mesh_rejects_triangle_count_mismatch() {
        let plane = primitives::plane(1.0);
        build_uv2_mesh(&plane, &[[[0.0, 0.0]; 3]]);
    }

    /// 测试场景：地板 plane(4)（网格句柄 0）、共享同一 wall 网格的两面墙
    /// （句柄 1，共享网格必须各自分裂副本）、立方柱（句柄 2，三 chart）、
    /// 圆柱（句柄 3，侧面相邻 quad 的面法线分落 WallX / WallZ 两类 ⇒
    /// 共享环顶点触发跨 chart 拆分；立方体的逐面独立顶点不会触发。不用
    /// UV 球——两极的三角形零面积，`build_bvh` 会按退化断言 panic）。
    fn room() -> World {
        let mut world = World::new();
        world.insert_resource(MeshStore::new());
        world.insert_resource(MaterialStore::new());
        let floor = world
            .resource_mut::<MeshStore>()
            .add(primitives::plane(4.0));
        let wall = world
            .resource_mut::<MeshStore>()
            .add(primitives::plane(4.0));
        let column = world.resource_mut::<MeshStore>().add(primitives::cube());
        let cyl = world
            .resource_mut::<MeshStore>()
            .add(primitives::cylinder(0.4, 0.8, 8));
        let mat = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.7, 0.7, 0.7, 1.0));
        world.spawn((Transform::IDENTITY, floor, mat));
        // 两面墙共享 wall 网格（各 4 m 宽 × 2 m 高）
        world.spawn((
            Transform {
                position: Vec3::new(0.0, 1.0, -2.0),
                rotation: Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
                scale: Vec3::new(1.0, 1.0, 0.5),
            },
            wall,
            mat,
        ));
        world.spawn((
            Transform {
                position: Vec3::new(-2.0, 1.0, 0.0),
                rotation: Quat::from_rotation_z(-std::f32::consts::FRAC_PI_2),
                scale: Vec3::new(0.5, 1.0, 1.0),
            },
            wall,
            mat,
        ));
        world.spawn((
            Transform {
                position: Vec3::new(0.8, 0.75, 0.0),
                scale: Vec3::new(0.5, 1.5, 0.5),
                ..Transform::IDENTITY
            },
            column,
            mat,
        ));
        world.spawn((
            Transform {
                position: Vec3::new(-0.8, 0.4, 0.8),
                ..Transform::IDENTITY
            },
            cyl,
            mat,
        ));
        propagate_transforms(&mut world);
        world
    }

    fn bake(world: &World) -> BakedLightmap {
        bake_lightmap(
            world,
            &BakeParams {
                light_dir: Vec3::new(-0.5, -1.0, 0.3),
                sky_color: Vec3::splat(0.3),
                sky_samples: 32,
                // 不做反弹：一次反弹项在墙角反而**更大**（半球命中几何多 ⇒
                // 色溢强），会盖掉天空 AO 的对比；本测试要的是纯天空遮蔽的
                // 解析对照（开阔处 = π × sky_color，见 bake.rs 模块文档）
                bounces: 0,
                ..BakeParams::default()
            },
            &BakeUvParams {
                texels_per_meter: 4.0,
                atlas_size: 64,
            },
        )
    }

    /// 端到端装配：目录建项、UV2 注入到每实体副本、组件挂载、
    /// 共享网格分裂（两面墙拿到不同句柄）、立方柱拆分顶点。
    #[test]
    fn install_lightmap_injects_uv2_and_binds_entities() {
        let mut world = room();
        let baked = bake(&world);
        let entities: Vec<Entity> = baked
            .atlas
            .entries
            .iter()
            .map(|e| e.entity)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        // 安装前的原网格顶点数（拆分数 = 副本顶点数 − 原顶点数）
        let original: Vec<(Entity, usize)> = {
            let meshes = world.resource::<MeshStore>();
            entities
                .iter()
                .map(|&e| {
                    let handle = *world.get::<MeshHandle>(e).expect("网格句柄");
                    (e, meshes.get(handle).expect("网格").vertices.len())
                })
                .collect()
        };
        let before = world.resource::<MeshStore>().len();
        let report = install_lightmap(&mut world, baked);

        assert_eq!(report.entities, entities.len(), "图集覆盖的实体全部注入");
        assert_eq!(report.lightmap, LightmapHandle(0));
        assert_eq!(
            world.resource::<LightmapStore>().len(),
            1,
            "目录缺席时新建并收录一项"
        );
        assert_eq!(
            world.resource::<MeshStore>().len(),
            before + entities.len(),
            "每实体一份网格副本"
        );

        // 共享网格的两面墙：副本句柄必须互不相同（各自图集坐标）
        let handles: Vec<MeshHandle> = entities
            .iter()
            .map(|&e| *world.get::<MeshHandle>(e).expect("网格句柄"))
            .collect();
        let mut unique: Vec<u32> = handles.iter().map(|h| h.0).collect();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), handles.len(), "副本句柄互不相同：{handles:?}");

        let meshes = world.resource::<MeshStore>();
        let mut splits = 0usize;
        for &(entity, base) in &original {
            assert_eq!(
                world.get::<LightmapBinding>(entity).map(|b| b.lightmap),
                Some(report.lightmap),
                "实体 {entity:?} 必须挂上绑定"
            );
            let handle = *world.get::<MeshHandle>(entity).expect("网格句柄");
            let data = meshes.get(handle).expect("副本存在");
            let uv2 = data.uv2.as_ref().expect("UV2 已注入");
            assert_eq!(uv2.len(), data.vertices.len(), "UV2 与顶点等长");
            assert!(
                uv2.iter()
                    .all(|c| c[0] >= 0.0 && c[0] <= 1.0 && c[1] >= 0.0 && c[1] <= 1.0),
                "UV2 须在图集归一化域内：{uv2:?}"
            );
            assert!(
                uv2.iter().any(|c| c[0] > 0.0 || c[1] > 0.0),
                "UV2 不得全零（全零 = 缺席标记，mask 会退回 IBL）"
            );
            assert!(
                data.indices
                    .iter()
                    .all(|&i| (i as usize) < data.vertices.len()),
                "实体 {entity:?} 的索引越界（拆分不得破坏拓扑）"
            );
            splits += data.vertices.len() - base;
        }
        // 球面顶点被相邻三角形分进不同主轴 chart ⇒ 必有拆分
        //（立方体的逐面独立顶点不会触发——那正是「不拆」的对照）
        assert!(
            report.vertices_added > 0,
            "球面跨 chart 共享顶点必须被拆分（实际 {}）",
            report.vertices_added
        );
        assert_eq!(
            report.vertices_added, splits,
            "报告的拆分数与逐网格顶点增量一致"
        );
    }

    /// UV2 与图集辐照的一致性（解析对照）：地板 chart 的角点 UV2 逐位等于
    /// `(chart 原点 + 三角形角点局部坐标) / 图集边长`；角点恰落在 texel
    /// **边界**上（双线性各取一半权重，外侧那一半是 pad）⇒ 断言口径是
    /// 「dilation 之后角点 texel 非零」+「已覆盖 texel 的值 dilation 前后
    /// 逐位不变」（填充只写没烘到的 texel）。
    #[test]
    fn injected_uv2_points_at_baked_texels() {
        let mut world = room();
        let baked = bake(&world);
        let size = baked.atlas.size as usize;
        // 地板实体 = 网格句柄 0（plane(4)）的 Floor chart（2 个三角形）
        let floor_entry = baked
            .atlas
            .entries
            .iter()
            .find(|e| {
                e.mesh == Some(MeshHandle(0))
                    && e.axis == crate::bake_uv::ChartAxis::Floor
                    && e.triangles.len() == 2
            })
            .expect("地板 chart 存在");
        let floor_entity = floor_entry.entity;
        let floor_origin = floor_entry.origin;
        let expected: Vec<[[f32; 2]; 3]> = floor_entry
            .triangles
            .iter()
            .map(|tri| {
                [0, 1, 2].map(|k| {
                    [
                        (floor_entry.origin[0] as f32 + tri.local[k][0]) / baked.atlas.size as f32,
                        (floor_entry.origin[1] as f32 + tri.local[k][1]) / baked.atlas.size as f32,
                    ]
                })
            })
            .collect();
        let indirect = baked.indirect.clone();
        let covered = baked.covered.clone();
        let report = install_lightmap(&mut world, baked);
        assert!(
            covered.iter().any(|&c| c),
            "地板 chart 必须有被覆盖的 texel"
        );

        let handle = *world.get::<MeshHandle>(floor_entity).expect("地板网格句柄");
        let data = world
            .resource::<MeshStore>()
            .get(handle)
            .expect("副本")
            .clone();
        let uv2 = data.uv2.expect("UV2");
        assert_eq!(data.vertices.len(), 4, "地板平面不拆分");
        // 全部 6 个 (三角形, 角点) 对：uv2[indices[3t+s]] 必须逐位等于图集
        // 角点坐标。共享顶点（索引序 [0,1,2, 0,2,3] 里的顶点 0 与 2）被两个
        // 三角形各查一次，两次值一致 = 平面网格无跨 chart 冲突（不拆分）。
        for (t, corners) in expected.iter().enumerate() {
            for (slot, &expected_uv) in corners.iter().enumerate() {
                let index = data.indices[t * 3 + slot] as usize;
                assert_eq!(
                    uv2[index], expected_uv,
                    "三角 {t} 角点 {slot}（顶点 {index}）的 UV2 必须逐位等于图集坐标"
                );
            }
        }

        // dilation 后的目录数据：已覆盖 texel 逐位保留烘焙值
        let store = world.resource::<LightmapStore>();
        let item = store.get(report.lightmap).expect("目录条目");
        for (index, &was_covered) in covered.iter().enumerate() {
            if was_covered {
                assert_eq!(
                    item.irradiance[index], indirect[index],
                    "已覆盖 texel {index} 的值必须逐位不变"
                );
            }
        }
        // 角点 UV2 所在的 texel（含边界 ⇒ 取包含该点的 texel）必须有值：
        // 未填充的黑 pad 会被双线性拉进 chart 边缘（dilation 的验收口径）
        for uv in &uv2 {
            let tx = ((uv[0] * size as f32) as usize).min(size - 1);
            let ty = ((uv[1] * size as f32) as usize).min(size - 1);
            let index = ty * size + tx;
            assert!(
                item.irradiance[index] != Vec3::ZERO,
                "角点 texel ({tx}, {ty}) 填充后不得为黑（UV2 {uv:?}）"
            );
        }
        // AO 语义对照：chart 局部 texel = 世界投影 × tpm − min + pad
        //（地板 plane(4) ⇒ local = 4·(x, z) + 9），贴墙角 texel(2,2) 的
        // 半球被两墙遮挡 ⇒ 间接辐照必须显著暗于地板中心 texel(9,9)
        let origin = floor_origin;
        let at_local =
            |lx: usize, ly: usize| (origin[1] as usize + ly) * size + origin[0] as usize + lx;
        let center = indirect[at_local(9, 9)];
        let corner = indirect[at_local(2, 2)];
        assert!(
            center != Vec3::ZERO && corner != Vec3::ZERO,
            "地板 chart 内部 texel 必须被覆盖：中心 {center:?} / 墙角 {corner:?}"
        );
        assert!(
            corner.length() < 0.9 * center.length(),
            "贴墙角的间接辐照 {corner:?} 必须暗于地板中心 {center:?}（AO 生效）"
        );
    }

    /// dilation 的解析对照：十字形已覆盖区向外扩，1 轮填 4 邻域均值、
    /// 2 轮填对角（= 两个已填邻居的均值），已填值逐位不变，空洞无邻域时
    /// 保持原值；结果与扫描序无关（双缓冲）。
    #[test]
    fn dilate_atlas_fills_by_neighbor_average() {
        // 3×3：中心已覆盖（值 4），其余 8 个未填
        let mut values = vec![Vec3::ZERO; 9];
        values[4] = Vec3::splat(4.0);
        let mut filled = vec![false; 9];
        filled[4] = true;
        dilate_atlas(&mut values, &mut filled, 3, 1);
        // 一轮：4 邻域（上 1 / 下 7 / 左 3 / 右 5）= 4，四角仍无已填邻域 ⇒ 0
        assert_eq!(values[1], Vec3::splat(4.0));
        assert_eq!(values[3], Vec3::splat(4.0));
        assert_eq!(values[5], Vec3::splat(4.0));
        assert_eq!(values[7], Vec3::splat(4.0));
        assert_eq!(values[0], Vec3::ZERO, "四角一轮内无已填邻域");
        assert!(filled[1] && filled[3] && filled[5] && filled[7]);
        assert!(!filled[0], "四角本轮未填");
        // 二轮（再跑一轮）：四角 = 两个已填邻居（各 4）的均值 = 4
        dilate_atlas(&mut values, &mut filled, 3, 1);
        assert_eq!(values[0], Vec3::splat(4.0));
        assert_eq!(values[8], Vec3::splat(4.0));
        assert_eq!(values[4], Vec3::splat(4.0), "已填值逐位不变");
        assert!(filled.iter().all(|&f| f), "3×3 两轮填满");

        // 均值语义：3×3 的左右两列已填不同值，中间一列一轮后 = 两邻均值
        let mut v2 = vec![Vec3::ZERO; 9];
        let mut f2 = vec![false; 9];
        for y in 0..3usize {
            v2[y * 3] = Vec3::new(1.0, 2.0, 3.0);
            f2[y * 3] = true;
            v2[y * 3 + 2] = Vec3::new(3.0, 2.0, 1.0);
            f2[y * 3 + 2] = true;
        }
        dilate_atlas(&mut v2, &mut f2, 3, 1);
        for y in 0..3usize {
            assert_eq!(
                v2[y * 3 + 1],
                Vec3::new(2.0, 2.0, 2.0),
                "第 {y} 行中列应为两邻算术均值"
            );
        }
        assert!(f2.iter().all(|&f| f), "一轮填满中列");

        // 无已填 texel ⇒ 全部保持原值（不 panic、不写）
        let mut v3 = vec![Vec3::ZERO; 4];
        let mut f3 = vec![false; 4];
        dilate_atlas(&mut v3, &mut f3, 2, LIGHTMAP_DILATE_ITERATIONS);
        assert_eq!(v3, vec![Vec3::ZERO; 4]);
        assert!(!f3.iter().any(|&f| f));

        // 确定性：同输入两次逐位一致
        let mut a = vec![Vec3::ZERO; 9];
        a[4] = Vec3::new(0.25, 0.5, 1.0);
        let mut fa = vec![false; 9];
        fa[4] = true;
        let mut b = a.clone();
        let mut fb = fa.clone();
        dilate_atlas(&mut a, &mut fa, 3, LIGHTMAP_DILATE_ITERATIONS);
        dilate_atlas(&mut b, &mut fb, 3, LIGHTMAP_DILATE_ITERATIONS);
        assert_eq!(a, b, "dilation 必须确定性");
        assert_eq!(fa, fb);
    }

    /// 目录统计与上传字节：辐照 12 B/texel（CPU）、Rgba16F 8 B/texel（GPU）；
    /// `insert` 收录时做 dilation（只有 texel 1 被覆盖 ⇒ 其 4 邻域被填成
    /// 同值），`half_bytes` 的长度与逐通道半精度位型按解析值断言。
    #[test]
    fn lightmap_store_accounting_and_half_bytes() {
        let mut store = LightmapStore::new();
        assert!(store.is_empty());
        store.insert(BakedLightmap {
            atlas: LightmapAtlas {
                size: 2,
                used_height: 2,
                entries: Vec::new(),
            },
            radiance: vec![Vec3::ZERO; 4],
            indirect: vec![
                Vec3::ZERO,
                Vec3::new(1.0, 0.5, 0.25),
                Vec3::ZERO,
                Vec3::ZERO,
            ],
            covered: vec![false, true, false, false],
        });
        assert_eq!(store.len(), 1);
        assert_eq!(store.memory_bytes(), 4 * 12);
        assert_eq!(store.gpu_bytes_estimate(), 2 * 2 * 8);
        let item = store.get(LightmapHandle(0)).expect("条目存在");
        assert_eq!(item.size(), 2);
        // dilation：texel 1 的三个邻居（0 / 2 / 3）被填成同值
        let filled = Vec3::new(1.0, 0.5, 0.25);
        assert_eq!(item.irradiance, vec![filled; 4], "一轮填满 2×2 图集");
        let bytes = item.half_bytes();
        assert_eq!(bytes.len(), 2 * 2 * 8, "4 texel × 4 通道 × 2 B");
        // (1.0, 0.5, 0.25, 1.0) → 0x3C00 / 0x3800 / 0x3400 / 0x3C00（小端）
        assert_eq!(
            &bytes[0..8],
            &[0x00, 0x3c, 0x00, 0x38, 0x00, 0x34, 0x00, 0x3c]
        );
        assert_eq!(&bytes[8..16], &bytes[0..8], "四个 texel 同值");
    }

    /// 悬垂 mesh 句柄的图集（`pack_atlas` 纯函数产物未补齐）⇒ panic，
    /// 而不是静默把 UV2 写进错误的网格。
    #[test]
    #[should_panic(expected = "图集表项缺 mesh 句柄")]
    fn install_lightmap_rejects_atlas_without_mesh_handles() {
        let mut world = room();
        let mut baked = bake(&world);
        for entry in &mut baked.atlas.entries {
            entry.mesh = None;
        }
        install_lightmap(&mut world, baked);
    }

    fn item_atlas(size: u32, irradiance: Vec<Vec3>) -> LightmapItem {
        LightmapItem {
            atlas: LightmapAtlas {
                size,
                used_height: 0,
                entries: Vec::new(),
            },
            irradiance,
        }
    }

    /// mip 链（C2 · 闭合 §8.31⑤）：棋盘辐照 (0|2) 的 4×4 图集——
    /// 每个 2×2 块恰含两 0 两 2 ⇒ 级 1 恒 1.0、级 2 恒 1.0（f32 精确
    /// 解析值，无需容差）；链长 = 3、各级字节 = 128/32/8；级 0 与
    /// `half_bytes` 逐字节一致（兼容形态）；f16 位型 0x3C00 校验。
    #[test]
    fn mip_chain_box_averages_match_analytic_values() {
        let checker: Vec<Vec3> = (0..16)
            .map(|i| {
                let (x, y) = (i % 4, i / 4);
                if (x + y) % 2 == 0 {
                    Vec3::ZERO
                } else {
                    Vec3::splat(2.0)
                }
            })
            .collect();
        let item = item_atlas(4, checker);
        let chain = item.mip_chain_half_bytes();
        assert_eq!(chain.len(), 3, "4×4 链止于 1×1");
        assert_eq!(
            chain[0],
            item.half_bytes(),
            "级 0 必须逐字节等于单 mip 上传源"
        );
        assert_eq!(
            (chain[1].len(), chain[2].len()),
            (2 * 2 * 8, 8),
            "行步长口径 size×8"
        );
        for level in [&chain[1], &chain[2]] {
            for texel in level.chunks(8) {
                assert_eq!(
                    texel,
                    &[0x00, 0x3c, 0x00, 0x3c, 0x00, 0x3c, 0x00, 0x3c],
                    "棋盘 2×2 块均值 = 1.0 精确（f16 0x3C00 小端），rgb 全通道、a 恒 1"
                );
            }
        }
        // 奇数维 3×3 全 2.0：截断序列 3 → 1（(3/2).max(1)），单样本 =
        // clamp 窗四取同值仍恒 2.0；链长 2 = mip_level_count(3,3)
        let odd = item_atlas(3, vec![Vec3::splat(2.0); 9]);
        let odd_chain = odd.mip_chain_half_bytes();
        assert_eq!(odd_chain.len(), 2, "3×3 → 1×1");
        assert_eq!(odd_chain[1].len(), 8);
        assert_eq!(&odd_chain[1][0..2], &[0x00, 0x40], "2.0 = f16 0x4000 小端");
    }

    /// mip 链确定性 + 显存对账（C2）：目录口径 `gpu_bytes_estimate_mipped`
    /// 恰等于逐图集 mip 链字节和（解析值 4×4 链 168、2×2 链 40），单 mip
    /// 口径不受波及；链两次生成逐字节一致。
    #[test]
    fn mip_chain_is_deterministic_and_estimate_mipped_reconciles() {
        let mk_baked = |size: u32, v: Vec3| BakedLightmap {
            atlas: LightmapAtlas {
                size,
                used_height: size,
                entries: Vec::new(),
            },
            radiance: vec![v; (size * size) as usize],
            indirect: vec![v; (size * size) as usize],
            // 全覆盖 ⇒ dilation 无事可做，辐照数组逐位保持烘焙值
            covered: vec![true; (size * size) as usize],
        };
        let mut store = LightmapStore::new();
        let h4 = store.insert(mk_baked(4, Vec3::splat(0.5)));
        let h2 = store.insert(mk_baked(2, Vec3::splat(3.0)));
        let chain_sum: u64 = store
            .get(h4)
            .unwrap()
            .mip_chain_half_bytes()
            .iter()
            .chain(store.get(h2).unwrap().mip_chain_half_bytes().iter())
            .map(|l| l.len() as u64)
            .sum();
        assert_eq!(
            store.gpu_bytes_estimate_mipped(),
            chain_sum,
            "对账：估算 ≡ 链字节和"
        );
        assert_eq!(store.gpu_bytes_estimate_mipped(), (128 + 32 + 8) + (32 + 8));
        assert_eq!(store.gpu_bytes_estimate(), 128 + 32, "单 mip 口径原样");
        // 确定性：两次生成逐字节一致
        assert_eq!(
            store.get(h4).unwrap().mip_chain_half_bytes(),
            store.get(h4).unwrap().mip_chain_half_bytes()
        );
    }
}
