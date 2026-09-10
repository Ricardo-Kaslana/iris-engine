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

use std::collections::BTreeSet;

use iris_core::Resource;

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
}

impl Default for TextureStore {
    fn default() -> Self {
        Self {
            textures: vec![CpuTexture::solid([255, 255, 255, 255])],
            bc3_declared: BTreeSet::new(),
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
            .map(|(index, t)| {
                let handle = TextureHandle(index as u32);
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
}
