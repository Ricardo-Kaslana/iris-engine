//! 士兵材质图集（程序化 · 确定性 · 单材质单 draw）。
//!
//! 角色的**全部**外观（布料 / 护甲 / 皮革 / 钢 / 玻璃 / 织带 / 橡胶）压进
//! 一张 `512²` 图集的 16 个 128² 分区，配 base / normal / ORM 三张
//! 纹理、一个材质：整具角色（含刀与枪）是一张蒙皮网格 = **一次 draw**。
//! 分区制是「单材质」与「材质多样性」的和解：金属度 / 粗糙度由 ORM 图的
//! **逐纹素**值表达（glTF 规范允许，引擎按 texel 采样），故钢刃、玻璃面罩、
//! 帆布弹匣包可以共享同一材质而互不串味。
//!
//! 生成全部是纯函数（splitmix64 无状态哈希 + 可平铺值噪声），同种子逐位
//! 同输出（单测锁定）；**无 rand、无全局状态**（确定性文化，见 AGENTS.md）。
//!
//! # 诚实限制
//! - 噪声是**可平铺**的（格点坐标取模），故分区内 UV 任意平铺无缝；分区
//!   之间不保证连续（本来就不该跨区采样，UV 侧由 [`crate::geom::Tile`] 的
//!   inset 边距兜住双线性越界）；
//! - 细节是**程序化图案**而非手绘贴图：没有布料接缝、绣章、个性化涂装，
//!   近景（< 30 cm）能看出噪声的统计性；远景与游戏内视距（本作相机距角色
//!   ~11 m）不可辨；
//! - 法线由高度场中心差分而来（与引擎 `brick_normal_orm` 同口径），
//!   故凹凸与颜色是同一个高度场的两个投影，不存在手绘法线的「假细节」；
//! - **法线坡度的量纲（本轮修）**：高度差分取「跨 2 纹素的高度差」，**不除以 UV
//!   步长**。除之则坡度被放大 ~32 倍（分区只有 128 纹素），法线贴图全饱和 ⇒
//!   渲染成「砂纸 / 木纹」；`bump` 的语义是「高度差 → 法线倾斜」的增益。
//! - **规则图案在 128² 分区上必然摩尔纹化**：织物 / 皮革走大尺度噪声（折痕 +
//!   细绒），织纹只留幅度 8% 的斜向细纹；旧版正交正弦斜纹在柱面 UV 下读作
//!   「蛇皮 / 向日葵」。
//! - 未做 mipmap（引擎纹理路径 v1 限制）：远处细密织纹会有轻微闪烁，由
//!   TAA 压制。

use iris_engine::prelude::CpuTexture;

use crate::geom::Tile;
use crate::textures::{hash01, smooth};

/// 图集边长（像素）。四分区 × 四行，每分区 128²。
///
/// 512² 是「仓库资产体积 vs 近景质感」的折中：1024² 的 PNG 内嵌后
/// ≈5.5 MB（GLB 单文件 6 MB，且每个实例各解析一份），512² 约 1/4 体积
/// 而游戏内视距（相机距角色 ~11 m）不可辨。
pub const ATLAS_SIZE: u32 = 512;
/// 分区列数。
pub const TILE_COLS: u32 = 4;
/// 分区行数。
pub const TILE_ROWS: u32 = 4;
/// 单分区边长（像素）。
pub const TILE_SIZE: u32 = ATLAS_SIZE / TILE_COLS;
/// UV 内缩边距（归一化）：≈ 4 纹素，供双线性采样不越界取到邻区。
pub const TILE_INSET: f32 = 0.004;
/// 图集内容种子（换种子 = 换一整套做旧分布）。
pub const ATLAS_SEED: u64 = 0x1A15_A55A_1170;

/// 一个纹素的材质采样结果（linear 空间）。
#[derive(Clone, Copy, Debug)]
pub struct MaterialSample {
    /// 基色（linear RGB，0..1）。
    pub base: [f32; 3],
    /// 高度场（法线来源；值域不敏感，只看差分）。
    pub height: f32,
    /// 法线强度（中心差分系数，越大凹凸越强）。
    pub bump: f32,
    /// 粗糙度（0..1）。
    pub roughness: f32,
    /// 金属度（0..1）。
    pub metallic: f32,
    /// 环境光遮蔽（0..1，1 = 无遮蔽）。
    pub ao: f32,
}

impl MaterialSample {
    fn new(base: [f32; 3], height: f32, bump: f32, roughness: f32, metallic: f32, ao: f32) -> Self {
        Self {
            base,
            height,
            bump,
            roughness,
            metallic,
            ao,
        }
    }
}

/// 图集分区（角色外观的全部材质面）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    /// 躯干战斗服（橄榄绿粗布）。
    ClothTorso = 0,
    /// 裤子（深灰斜纹）。
    ClothPants = 1,
    /// 护甲钢（枪灰，划痕 + 边缘磨损）。
    Armor = 2,
    /// 头盔漆面（哑光军绿，掉漆露钢）。
    Helmet = 3,
    /// 皮革（腰带 / 挂具，棕色）。
    Leather = 4,
    /// 战靴皮（近黑）。
    Boot = 5,
    /// 刀身钢（纵向拉丝 + 崩口）。
    Blade = 6,
    /// 枪械金属（磷化黑，细划痕）。
    Gunmetal = 7,
    /// 缠柄胶带（斜向条带）。
    Grip = 8,
    /// 战术手套（黑皮 + 指节护垫）。
    Glove = 9,
    /// 面罩玻璃（近黑镜面）。
    Visor = 10,
    /// 黄铜饰件（徽记 / 卡扣）。
    Trim = 11,
    /// 尼龙织带（横向罗纹）。
    Webbing = 12,
    /// 橡胶（鞋底 / 缓冲垫）。
    Rubber = 13,
    /// 内衬紧身衣（深色针织）。
    Undersuit = 14,
    /// 帆布弹匣包。
    Pouch = 15,
}

impl Surface {
    /// 全部分区（顺序 = 图集格序：先行后列）。
    pub const ALL: [Surface; 16] = [
        Surface::ClothTorso,
        Surface::ClothPants,
        Surface::Armor,
        Surface::Helmet,
        Surface::Leather,
        Surface::Boot,
        Surface::Blade,
        Surface::Gunmetal,
        Surface::Grip,
        Surface::Glove,
        Surface::Visor,
        Surface::Trim,
        Surface::Webbing,
        Surface::Rubber,
        Surface::Undersuit,
        Surface::Pouch,
    ];

    /// 分区下标（= 图集格序）。
    pub fn index(self) -> usize {
        self as usize
    }

    /// 分区对应的 UV 矩形（已内缩边距）。
    pub fn tile(self) -> Tile {
        let index = self.index() as u32;
        Tile::grid(TILE_COLS, TILE_ROWS, index % TILE_COLS, index / TILE_COLS).inset(TILE_INSET)
    }

    /// 采样一个纹素（u / v 为分区内 [0,1) 坐标）。
    pub fn sample(self, u: f32, v: f32, seed: u64) -> MaterialSample {
        let s = seed ^ (self.index() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        match self {
            Surface::ClothTorso => cloth(u, v, s, [0.118, 0.130, 0.088], 11.0, 0.55),
            Surface::ClothPants => cloth(u, v, s, [0.070, 0.074, 0.078], 13.0, 0.40),
            Surface::Armor => armor(u, v, s),
            Surface::Helmet => painted_metal(u, v, s),
            Surface::Leather => leather(u, v, s, [0.120, 0.072, 0.038], 9.0),
            // 战靴偏棕（与缠柄胶带的近黑拉开距离：两者亮度接近，靠色相区分）
            Surface::Boot => leather(u, v, s, [0.086, 0.065, 0.048], 14.0),
            Surface::Blade => blade(u, v, s),
            Surface::Gunmetal => gunmetal(u, v, s),
            Surface::Grip => grip_tape(u, v, s),
            Surface::Glove => glove(u, v, s),
            Surface::Visor => {
                MaterialSample::new([0.108, 0.140, 0.188], 0.5, 0.16, 0.130, 1.0, 1.0)
            }
            Surface::Trim => trim(u, v, s),
            Surface::Webbing => webbing(u, v, s),
            Surface::Rubber => rubber(u, v, s),
            Surface::Undersuit => undersuit(u, v, s),
            Surface::Pouch => cloth(u, v, s, [0.108, 0.094, 0.064], 15.0, 0.75),
        }
    }
}

/// 士兵材质图集三张纹理（base sRGB / normal 线性 / ORM 线性）。
#[derive(Clone, Debug)]
pub struct SoldierAtlas {
    /// 基色（sRGB 编码，硬件解码到线性）。
    pub base: CpuTexture,
    /// 切线空间法线（线性数据）。
    pub normal: CpuTexture,
    /// R = AO、G = 粗糙度、B = 金属度（线性数据）。
    pub orm: CpuTexture,
}

/// 生成图集（纯函数：同 seed 逐位同输出）。
pub fn soldier_atlas(seed: u64) -> SoldierAtlas {
    build_atlas(ATLAS_SIZE, seed)
}

/// 指定边长的图集构建（测试用小块跑确定性；`size` 须被分区列数整除）。
fn build_atlas(size: u32, seed: u64) -> SoldierAtlas {
    assert!(
        size % TILE_COLS == 0 && size / TILE_COLS >= 4,
        "图集边长须被 {TILE_COLS} 整除且分区至少 4 纹素"
    );
    let tile = size / TILE_COLS;
    let mut base = Vec::with_capacity((size * size * 4) as usize);
    let mut normal = Vec::with_capacity((size * size * 4) as usize);
    let mut orm = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let col = x / tile;
            let row = y / tile;
            let surface = Surface::ALL[(row * TILE_COLS + col) as usize];
            let u = (x % tile) as f32 / tile as f32;
            let v = (y % tile) as f32 / tile as f32;
            let sample = surface.sample(u, v, seed);
            let srgb = linear_to_srgb(sample.base);
            base.extend_from_slice(&[
                (srgb[0] * 255.0 + 0.5) as u8,
                (srgb[1] * 255.0 + 0.5) as u8,
                (srgb[2] * 255.0 + 0.5) as u8,
                255,
            ]);
            // 法线：分区内绕回的中心差分（步长落在本分区内，不跨区取样）。
            //
            // **量纲陷阱（本轮修）**：坡度取「跨 2 纹素的高度差」，不再除以 UV 步长。
            // 高度场值域 0..1、分区仅 128 纹素 ⇒ 除以 2*step 会把坡度放大 ~32 倍，
            // 法线贴图全饱和（每纹素随机倾斜 45°+），渲染出来就是「砂纸 / 木纹」；
            // 现在的 bump = 「高度差 → 法线倾斜」的增益，1.0 约对应 45°。
            let step = 2.0 / tile as f32;
            let x0 = wrap_tile(u - step);
            let x1 = wrap_tile(u + step);
            let y0 = wrap_tile(v - step);
            let y1 = wrap_tile(v + step);
            let dhdu = surface.sample(x1, v, seed).height - surface.sample(x0, v, seed).height;
            let dhdv = surface.sample(u, y1, seed).height - surface.sample(u, y0, seed).height;
            let bump = sample.bump;
            let len = (dhdu * dhdu * bump * bump + dhdv * dhdv * bump * bump + 1.0).sqrt();
            let n = [-dhdu * bump / len, -dhdv * bump / len, 1.0 / len];
            normal.extend_from_slice(&[
                ((n[0] * 0.5 + 0.5) * 255.0 + 0.5) as u8,
                ((n[1] * 0.5 + 0.5) * 255.0 + 0.5) as u8,
                ((n[2] * 0.5 + 0.5) * 255.0 + 0.5) as u8,
                255,
            ]);
            orm.extend_from_slice(&[
                (sample.ao.clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                (sample.roughness.clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                (sample.metallic.clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                255,
            ]);
        }
    }
    SoldierAtlas {
        base: CpuTexture {
            width: size,
            height: size,
            rgba: base,
            srgb: true,
        },
        normal: CpuTexture {
            width: size,
            height: size,
            rgba: normal,
            srgb: false,
        },
        orm: CpuTexture {
            width: size,
            height: size,
            rgba: orm,
            srgb: false,
        },
    }
}

// ---------------------------------------------------------------------------
// 噪声（可平铺）
// ---------------------------------------------------------------------------

/// 可平铺值噪声：格点坐标对 freq 取模 ⇒ 分区内任意平铺无缝。
pub fn lattice(u: f32, v: f32, freq: u32, seed: u64) -> f32 {
    assert!(freq > 0, "频率至少 1");
    let f = freq as f32;
    let (x, y) = (u * f, v * f);
    let (xi, yi) = (x.floor(), y.floor());
    let (tx, ty) = (smooth(x - xi), smooth(y - yi));
    let at = |i: f32, j: f32| {
        let ii = (i as i64).rem_euclid(freq as i64) as u64;
        let jj = (j as i64).rem_euclid(freq as i64) as u64;
        hash01(seed ^ (ii << 32) ^ jj)
    };
    let top = at(xi, yi) + (at(xi + 1.0, yi) - at(xi, yi)) * tx;
    let bottom = at(xi, yi + 1.0) + (at(xi + 1.0, yi + 1.0) - at(xi, yi + 1.0)) * tx;
    top + (bottom - top) * ty
}

/// 多倍频噪声（octaves 层，幅值减半、频率加倍），归一化到 [0,1]。
pub fn fbm(u: f32, v: f32, freq: u32, octaves: u32, seed: u64) -> f32 {
    let mut sum = 0.0f32;
    let mut norm = 0.0f32;
    let mut amp = 1.0f32;
    let mut f = freq;
    for k in 0..octaves {
        sum += amp
            * lattice(
                u,
                v,
                f,
                seed.wrapping_add((k as u64).wrapping_mul(0x2545_f491_4f6c_dd1d)),
            );
        norm += amp;
        amp *= 0.5;
        f = (f * 2).min(2048);
    }
    sum / norm
}

/// 脊状噪声（1 − |2·fbm − 1|）：皮革颗粒 / 岩石裂纹的常用形态。
pub fn ridged(u: f32, v: f32, freq: u32, octaves: u32, seed: u64) -> f32 {
    1.0 - (2.0 * fbm(u, v, freq, octaves, seed) - 1.0).abs()
}

// ---------------------------------------------------------------------------
// 材质
// ---------------------------------------------------------------------------

/// 织物：经纬交织（两个正交正弦的乘积）+ 纤维绒面 + 大尺度脏污。
/// 战斗服布料：**无规则图案**的软织物（大尺度折痕 + 细绒 + 污渍）。
///
/// 第一版用两组正交正弦做斜纹，在 128² 分区 + 柱面 UV 下读作「蛇皮 / 向日葵」
/// ——规则图案落在近 Nyquist 的频率上必然摩尔纹化。织物在游戏视距下的正确
/// 读法是**统计性的**：折痕走大尺度噪声，织纹只留极轻的各向异性细纹；
/// 法线强度随之压到 0.30（第一版 1.3 的「砂纸」感）。
fn cloth(u: f32, v: f32, seed: u64, base: [f32; 3], threads: f32, dirt: f32) -> MaterialSample {
    let tau = std::f32::consts::TAU;
    let fold = fbm(u, v, 3, 4, seed ^ 0x11);
    let grime = fbm(u, v, 5, 4, seed ^ 0x22);
    let fuzz = fbm(u, v, 18, 3, seed ^ 0x33);
    // 织纹：幅度 8% 的斜向细纹，只在掠射角下提供「这是布不是塑料」的暗示
    let warp = ((u + v) * threads * tau).sin() * 0.5 + 0.5;
    let weft = ((u - v) * threads * tau).sin() * 0.5 + 0.5;
    let weave = ((warp * weft).sqrt() - 0.5) * 0.16 + 0.5;
    let height = fold * 0.52 + grime * 0.26 + fuzz * 0.14 + (weave - 0.5) * 0.08;
    let shade = (0.95 + 0.05 * (1.0 - grime)) * (0.93 + 0.14 * fold);
    let wear = 1.0 + 0.04 * (fuzz - 0.5);
    MaterialSample::new(
        [
            base[0] * shade * wear,
            base[1] * shade * wear,
            base[2] * shade * wear,
        ],
        height,
        0.90,
        (0.80 + 0.12 * grime + 0.05 * dirt).min(1.0),
        0.0,
        0.90 + 0.10 * fold,
    )
}

/// 护甲钢：板面微凹 + 细划痕 + 边缘磨损（露出更亮的裸钢）。
fn armor(u: f32, v: f32, seed: u64) -> MaterialSample {
    let dents = fbm(u, v, 3, 4, seed ^ 0x31);
    let scratches = scratch_field(u, v, 26, seed ^ 0x32);
    let wear = fbm(u, v, 6, 4, seed ^ 0x33);
    let worn = smooth((wear - 0.62) / 0.16);
    let base = [
        0.074 + 0.038 * worn + 0.018 * dents,
        0.080 + 0.042 * worn + 0.020 * dents,
        0.088 + 0.046 * worn + 0.022 * dents,
    ];
    let height = dents * 0.6 + (1.0 - scratches) * 0.25 + 0.15;
    MaterialSample::new(
        base,
        height,
        0.85,
        (0.38 + 0.26 * scratches + 0.16 * worn).min(1.0),
        0.32 - 0.04 * worn,
        0.88 + 0.12 * dents,
    )
}

/// 漆面金属（头盔）：哑光军绿漆 + 掉漆斑块（露出裸钢）。
fn painted_metal(u: f32, v: f32, seed: u64) -> MaterialSample {
    let chips = fbm(u, v, 9, 4, seed ^ 0x41);
    let rough_paint = fbm(u, v, 20, 3, seed ^ 0x42);
    let chipped = smooth((chips - 0.76) / 0.14);
    let paint = [0.112, 0.122, 0.100];
    let steel = [0.300, 0.312, 0.330];
    let base = [
        paint[0] * (1.0 - chipped) + steel[0] * chipped,
        paint[1] * (1.0 - chipped) + steel[1] * chipped,
        paint[2] * (1.0 - chipped) + steel[2] * chipped,
    ];
    MaterialSample::new(
        base,
        chips * 0.8 + 0.2,
        0.90,
        0.54 - 0.18 * chipped + 0.10 * rough_paint,
        0.12 + 0.78 * chipped,
        0.90 + 0.10 * chips,
    )
}

/// 皮革（腰带 / 挂具 / 战靴）：柔和粒面 + 折痕。
///
/// 第一版用 Ridged 噪声做粒面，在 128² 下是锐利高频 ⇒ 靴子读作树皮。
/// 皮革的正确读法是「大面积平滑 + 稀疏深折痕」。
fn leather(u: f32, v: f32, seed: u64, base: [f32; 3], grain: f32) -> MaterialSample {
    let cells = fbm(u, v, grain as u32, 4, seed ^ 0x51);
    let creases = fbm(u, v, 3, 3, seed ^ 0x52);
    let rub = fbm(u, v, 6, 4, seed ^ 0x53);
    // 高度场向**细粒**倾斜：低频折痕占比过大（0.40）时，法线贴图在近景会读成
    // 「大块斑纹」（靴子特写实测像玳瑁皮），而皮革的正确读法是「均匀细粒 + 稀疏折痕」。
    let height = cells * 0.62 + creases * 0.22 + rub * 0.16;
    let shade = 0.92 + 0.08 * (0.4 + 0.6 * creases) * (0.85 + 0.15 * rub);
    MaterialSample::new(
        [base[0] * shade, base[1] * shade, base[2] * shade],
        height,
        0.55,
        (0.42 + 0.25 * (1.0 - rub) + 0.10 * cells).min(1.0),
        0.0,
        0.86 + 0.14 * height,
    )
}

/// 刀身钢：纵向拉丝 + 稀疏崩口。
fn blade(u: f32, v: f32, seed: u64) -> MaterialSample {
    let brush = scratch_field_v(u, v, 120, seed ^ 0x61);
    let wide = fbm(u, v, 2, 4, seed ^ 0x62);
    let nick = smooth((fbm(u, v, 40, 2, seed ^ 0x63) - 0.86) / 0.06);
    let base = [
        0.480 + 0.070 * wide - 0.14 * nick,
        0.495 + 0.070 * wide - 0.14 * nick,
        0.515 + 0.070 * wide - 0.14 * nick,
    ];
    MaterialSample::new(
        base,
        brush * 0.5 + wide * 0.5 - nick * 0.6,
        0.70,
        (0.10 + 0.14 * brush + 0.20 * nick).min(1.0),
        1.0,
        0.94 + 0.06 * wide,
    )
}

/// 枪械金属：磷化黑 + 细划痕 + 轻微哑光。
fn gunmetal(u: f32, v: f32, seed: u64) -> MaterialSample {
    let fine = fbm(u, v, 14, 3, seed ^ 0x71);
    let scratches = scratch_field(u, v, 40, seed ^ 0x72);
    // 磷化黑（均值约 0.035 线性）——护甲钢是 0.081，两者若同亮度则
    // 「护甲 / 枪械」在整身像里读作同一材质（tiles_are_visually_distinct 实测距离 1.65）。
    let base = [
        0.026 + 0.018 * fine,
        0.028 + 0.019 * fine,
        0.032 + 0.021 * fine,
    ];
    MaterialSample::new(
        base,
        fine * 0.6 + (1.0 - scratches) * 0.4,
        0.70,
        (0.40 + 0.20 * scratches).min(1.0),
        0.55,
        0.90 + 0.10 * fine,
    )
}

/// 缠柄胶带：45 度斜向条带（条带内细密罗纹）。
fn grip_tape(u: f32, v: f32, seed: u64) -> MaterialSample {
    let band = ((u + v) * 9.0).fract();
    let ridge = 1.0 - (2.0 * band - 1.0).abs();
    let rib = ((v * 26.0) * std::f32::consts::TAU).sin() * 0.5 + 0.5;
    let dirt = fbm(u, v, 6, 3, seed ^ 0x81);
    let height = ridge * 0.7 + rib * 0.3;
    let base = [0.052, 0.049, 0.043];
    let shade = 0.78 + 0.22 * (0.5 + 0.5 * ridge) * (0.88 + 0.12 * dirt);
    MaterialSample::new(
        [base[0] * shade, base[1] * shade, base[2] * shade],
        height,
        0.60,
        0.78 + 0.10 * dirt,
        0.0,
        0.82 + 0.18 * height,
    )
}

/// 战术手套：黑皮 + 指节护垫（周期性格点上的凸起）。
fn glove(u: f32, v: f32, seed: u64) -> MaterialSample {
    let grain = ridged(u, v, 12, 4, seed ^ 0x91);
    let cell = 3.0;
    let (cx, cy) = ((u * cell).fract() - 0.5, (v * cell).fract() - 0.5);
    let pad = smooth(1.0 - (cx * cx + cy * cy).sqrt() / 0.42);
    let base = [0.098, 0.094, 0.088];
    let shade = 0.88 + 0.20 * pad + 0.10 * (grain - 0.5);
    MaterialSample::new(
        [base[0] * shade, base[1] * shade, base[2] * shade],
        grain * 0.4 + pad * 0.9,
        0.80,
        (0.44 + 0.18 * (1.0 - pad)).min(1.0),
        0.0,
        0.84 + 0.16 * pad.max(grain),
    )
}

/// 黄铜饰件：亮铜 + 氧化暗斑。
fn trim(u: f32, v: f32, seed: u64) -> MaterialSample {
    let patina = fbm(u, v, 7, 4, seed ^ 0xa1);
    let fine = fbm(u, v, 15, 2, seed ^ 0xa2);
    let base = [
        0.26 * (0.7 + 0.5 * patina),
        0.18 * (0.7 + 0.5 * patina),
        0.065 * (0.6 + 0.6 * patina),
    ];
    MaterialSample::new(
        base,
        patina * 0.7 + fine * 0.3,
        1.00,
        0.30 + 0.32 * (1.0 - patina),
        1.0,
        0.90 + 0.10 * patina,
    )
}

/// 尼龙织带：横向罗纹 + 编织细纹。
fn webbing(u: f32, v: f32, seed: u64) -> MaterialSample {
    let rib = ((v * 14.0) * std::f32::consts::TAU).sin() * 0.5 + 0.5;
    let weave = fbm(u, v, 24, 2, seed ^ 0xb1);
    let height = rib * 0.7 + weave * 0.3;
    let base = [0.098, 0.086, 0.058];
    let shade = 0.86 + 0.14 * (0.4 + 0.6 * weave) * (0.85 + 0.15 * rib);
    MaterialSample::new(
        [base[0] * shade, base[1] * shade, base[2] * shade],
        height,
        0.70,
        0.74 + 0.12 * weave,
        0.0,
        0.86 + 0.14 * rib,
    )
}

/// 橡胶：细密颗粒。
fn rubber(u: f32, v: f32, seed: u64) -> MaterialSample {
    let bumps = fbm(u, v, 22, 3, seed ^ 0xc1);
    let base = [
        0.034 + 0.018 * bumps,
        0.034 + 0.018 * bumps,
        0.036 + 0.018 * bumps,
    ];
    MaterialSample::new(
        base,
        bumps,
        0.55,
        0.90 - 0.06 * bumps,
        0.0,
        0.88 + 0.12 * bumps,
    )
}

/// 内衬紧身衣：细针织（斜向小格）。
fn undersuit(u: f32, v: f32, seed: u64) -> MaterialSample {
    let knit = ((u * 22.0 + v * 22.0) * std::f32::consts::TAU).sin() * 0.5 + 0.5;
    let knit2 = ((u * 22.0 - v * 22.0) * std::f32::consts::TAU).sin() * 0.5 + 0.5;
    let fuzz = fbm(u, v, 20, 3, seed ^ 0xd1);
    let height = knit * knit2 * 0.7 + fuzz * 0.3;
    let base = [0.046, 0.049, 0.055];
    let shade = 0.88 + 0.16 * height;
    MaterialSample::new(
        [base[0] * shade, base[1] * shade, base[2] * shade],
        height,
        0.70,
        0.66 + 0.12 * fuzz,
        0.05,
        0.84 + 0.16 * height,
    )
}

/// 各向同性划痕场：随机方向的细划痕（返回被划伤程度 0..1，1 = 划痕内）。
fn scratch_field(u: f32, v: f32, count: u32, seed: u64) -> f32 {
    let mut depth = 0.0f32;
    for i in 0..count {
        let s = seed ^ (i as u64).wrapping_mul(0x2545_f491_4f6c_dd1d);
        let angle = hash01(s) * std::f32::consts::PI;
        let offset = hash01(s ^ 0x9e37);
        let half = 0.0015 + 0.004 * hash01(s ^ 0x51ed);
        let d = (u * angle.cos() + v * angle.sin() - offset).abs();
        if d < half {
            let t = (d / half).clamp(0.0, 1.0);
            depth = depth.max(1.0 - smooth(t));
        }
    }
    depth
}

/// 纵向拉丝场（沿 v 走向的密集细纹；u 向为高频）。
fn scratch_field_v(u: f32, v: f32, count: u32, seed: u64) -> f32 {
    let mut acc = 0.0f32;
    let mut weight = 0.0f32;
    for i in 0..count {
        let s = seed ^ (i as u64).wrapping_mul(0x2545_f491_4f6c_dd1d);
        let lane = hash01(s);
        let d = (u - lane).abs();
        let half = 0.0008 + 0.0025 * hash01(s ^ 0x77);
        if d < half {
            let wobble = 0.15 * (v * 60.0 + lane * 40.0).sin();
            let t = ((d + wobble * 0.001) / half).clamp(0.0, 1.0);
            acc += (1.0 - smooth(t)) * hash01(s ^ 0x1f);
            weight += 1.0;
        }
    }
    if weight > 0.0 {
        (acc / weight).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// linear → sRGB 编码（基色写盘用；硬件在采样时解码回线性）。
fn linear_to_srgb(c: [f32; 3]) -> [f32; 3] {
    let f = |x: f32| {
        let x = x.clamp(0.0, 1.0);
        if x <= 0.003_130_8 {
            12.92 * x
        } else {
            1.055 * x.powf(1.0 / 2.4) - 0.055
        }
    };
    [f(c[0]), f(c[1]), f(c[2])]
}

/// 分区内绕回（[0,1) 周期）。
fn wrap_tile(t: f32) -> f32 {
    t.rem_euclid(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atlas_bytes() -> usize {
        (ATLAS_SIZE * ATLAS_SIZE * 4) as usize
    }

    fn tile_offset(surface: Surface, x: u32, y: u32) -> usize {
        let index = surface.index() as u32;
        let px = (index % TILE_COLS) * TILE_SIZE + x;
        let py = (index / TILE_COLS) * TILE_SIZE + y;
        (((py * ATLAS_SIZE) + px) * 4) as usize
    }

    /// 全尺寸图集共享 fixture（每测各建一次在 debug 下是秒级开销）。
    fn atlas() -> &'static SoldierAtlas {
        static ATLAS: std::sync::OnceLock<SoldierAtlas> = std::sync::OnceLock::new();
        ATLAS.get_or_init(|| soldier_atlas(ATLAS_SEED))
    }

    #[test]
    fn atlas_is_deterministic_bitwise() {
        // 小块图集跑确定性：像素数 1/64，断言强度不变（逐位）
        let a = build_atlas(128, ATLAS_SEED);
        let b = build_atlas(128, ATLAS_SEED);
        assert_eq!(a.base.rgba, b.base.rgba, "基色应逐位一致");
        assert_eq!(a.normal.rgba, b.normal.rgba, "法线应逐位一致");
        assert_eq!(a.orm.rgba, b.orm.rgba, "ORM 应逐位一致");
    }

    #[test]
    fn atlas_color_spaces_and_sizes_are_declared() {
        let atlas = atlas();
        assert!(atlas.base.srgb, "基色是颜色数据，须标 sRGB");
        assert!(!atlas.normal.srgb, "法线是线性数据");
        assert!(!atlas.orm.srgb, "ORM 是线性数据");
        for tex in [&atlas.base, &atlas.normal, &atlas.orm] {
            assert_eq!(tex.width, ATLAS_SIZE);
            assert_eq!(tex.height, ATLAS_SIZE);
            assert_eq!(tex.rgba.len(), atlas_bytes());
        }
    }

    #[test]
    fn tiles_are_visually_distinct() {
        // 分区均值两两不同（防守「所有分区采到同一材质」的退化）。判据用
        // **颜色向量距离**而不是亮度差：深灰裤与棕皮革亮度本就接近
        // （亮度差 0.05），但色相完全不同——亮度判据会误报。
        let atlas = atlas();
        let mut means = Vec::new();
        for surface in Surface::ALL {
            let mut sum = [0.0f32; 3];
            for y in 0..TILE_SIZE {
                for x in 0..TILE_SIZE {
                    let offset = tile_offset(surface, x, y);
                    for (channel, value) in sum.iter_mut().enumerate() {
                        *value += atlas.base.rgba[offset + channel] as f32;
                    }
                }
            }
            let count = (TILE_SIZE * TILE_SIZE) as f32;
            means.push([sum[0] / count, sum[1] / count, sum[2] / count]);
        }
        for (i, a) in means.iter().enumerate() {
            for (j, b) in means.iter().enumerate() {
                if i == j {
                    continue;
                }
                let distance =
                    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
                assert!(
                    distance > 4.0,
                    "分区 {i} 与 {j} 的基色距离 {distance:.2} 过小（材质退化）"
                );
            }
        }
    }

    #[test]
    fn metal_tiles_are_metallic_and_cloth_is_not() {
        let atlas = atlas();
        let metallic_mean = |surface: Surface| {
            let mut sum = 0.0f32;
            for y in 0..TILE_SIZE {
                for x in 0..TILE_SIZE {
                    sum += atlas.orm.rgba[tile_offset(surface, x, y) + 2] as f32;
                }
            }
            sum / (TILE_SIZE * TILE_SIZE) as f32
        };
        assert!(metallic_mean(Surface::Blade) > 240.0, "刀身金属度应接近 1");
        assert!(metallic_mean(Surface::Visor) > 240.0, "面罩金属度应接近 1");
        assert!(metallic_mean(Surface::ClothTorso) < 4.0, "布料金属度应为 0");
        assert!(metallic_mean(Surface::Leather) < 4.0, "皮革金属度应为 0");
    }

    #[test]
    fn lattice_is_tileable() {
        for freq in [1u32, 4, 13] {
            let a = lattice(0.31, 0.77, freq, 7);
            let b = lattice(1.31, 0.77, freq, 7);
            let c = lattice(0.31, 5.77, freq, 7);
            assert!((a - b).abs() < 1e-6, "u 向应可平铺（freq {freq}）");
            assert!((a - c).abs() < 1e-6, "v 向应可平铺（freq {freq}）");
        }
    }

    #[test]
    fn normal_map_is_neutral_where_height_is_flat() {
        // 面罩分区高度恒定 ⇒ 法线恒为 (0,0,1)
        let atlas = atlas();
        // 分区内采样（分区边长随 ATLAS_SIZE 变化，硬编码像素会采到邻区）
        let probes = [TILE_SIZE / 8, TILE_SIZE / 2, TILE_SIZE - TILE_SIZE / 8];
        for y in probes {
            for x in probes {
                let offset = tile_offset(Surface::Visor, x, y);
                let n = [
                    atlas.normal.rgba[offset],
                    atlas.normal.rgba[offset + 1],
                    atlas.normal.rgba[offset + 2],
                ];
                assert!(
                    (n[0] as i32 - 128).abs() <= 1 && (n[1] as i32 - 128).abs() <= 1 && n[2] >= 253,
                    "平高度处法线应中性，实际 {n:?}"
                );
            }
        }
    }

    #[test]
    fn every_surface_tile_is_inside_the_atlas() {
        for surface in Surface::ALL {
            let tile = surface.tile();
            assert!(tile.u0 >= 0.0 && tile.v0 >= 0.0);
            assert!(tile.u0 + tile.du <= 1.0 + 1e-6 && tile.v0 + tile.dv <= 1.0 + 1e-6);
            assert!(tile.du > 0.2 && tile.dv > 0.2, "分区应占据 1/4 图集");
        }
    }
}
