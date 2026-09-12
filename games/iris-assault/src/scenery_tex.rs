//! 庭院材质库（程序化 · 可平铺 · 确定性）。
//!
//! 每种材质一张**独立**贴图（不是图集）：建筑表面要按米平铺，图集与平铺
//! 互斥（UV 越界会采到邻格）。分材质 = 分 draw call，但庭院是静态几何、
//! 材质 8 种，代价可忽略。
//!
//! 噪声沿用 [`crate::soldier_tex`] 的可平铺值噪声（格点取模），法线由
//! 高度场中心差分（与引擎 `brick_normal_orm` 同口径）。全部纯函数：
//! 同参数逐位同输出（单测锁定），无 rand、无全局状态。
//!
//! # 诚实限制
//! - 石头 / 木头的「纹理方向」由 UV 决定：盒投投影在斜面上会拉伸（庭院
//!   构件几乎全是轴对齐体块，实际看不到）；
//! - 砖缝是**高度场**画出来的，没有逐砖几何（远景无差异，贴面 10 cm 内
//!   能看出是平面）；
//! - 没有 mipmap（引擎纹理路径 v1）：远处石地会有细闪，由 TAA 压制。

use crate::soldier_tex::{fbm, ridged};
use crate::textures::normal_from_height;
use iris_engine::prelude::CpuTexture;

/// 庭院材质种类（= GLB 的材质数 = 静态几何的 draw call 上界）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Surface {
    /// 石灰岩（铺地 / 基座 / 檐口 / 柱身）。
    Stone = 0,
    /// 砖（墙体）。
    Brick = 1,
    /// 大理石（雕像 / 喷泉）。
    Marble = 2,
    /// 木（大门 / 旗帜杆）。
    Wood = 3,
    /// 铁（火盆 / 支架 / 格栅 / 铁箍）。
    Iron = 4,
    /// 布（旗帜）。
    Cloth = 5,
    /// 水（喷泉水面）。
    Water = 6,
    /// 炭火（发光）。
    Coal = 7,
    /// 铺地（石板网格画在贴图里：290 块独立石板几何要 ~19k 三角形，
    /// 贴图方案 2 个三角形/格且石板缝由法线贴图给出）。
    Paving = 8,
}

impl Surface {
    /// 全部材质（GLB 材质下标 = 本表顺序）。
    pub const ALL: [Surface; 9] = [
        Surface::Stone,
        Surface::Brick,
        Surface::Marble,
        Surface::Wood,
        Surface::Iron,
        Surface::Cloth,
        Surface::Water,
        Surface::Coal,
        Surface::Paving,
    ];

    /// GLB 材质下标。
    pub fn index(self) -> usize {
        self as usize
    }

    /// 贴图边长（0 = 无贴图，纯参数材质）。
    pub fn size(self) -> u32 {
        match self {
            Surface::Stone | Surface::Brick | Surface::Paving => 512,
            Surface::Marble | Surface::Wood | Surface::Iron | Surface::Cloth => 256,
            Surface::Water => 0,
            Surface::Coal => 128,
        }
    }
}

/// 单材质贴图组（法线 / ORM 可缺席：纯色材质不需要）。
#[derive(Clone, Debug)]
pub struct TexSet {
    /// 基色（sRGB 编码）。
    pub base: CpuTexture,
    /// 切线空间法线（线性）。
    pub normal: Option<CpuTexture>,
    /// R = AO、G = 粗糙度、B = 金属度（线性）。
    pub orm: Option<CpuTexture>,
}

/// 材质常数（无贴图材质由它给出 PBR 参数）。
#[derive(Clone, Copy, Debug)]
pub struct SurfaceParams {
    /// 基色（linear）。
    pub base_color: [f32; 4],
    /// 金属度。
    pub metallic: f32,
    /// 粗糙度。
    pub roughness: f32,
    /// 自发光（linear，None = 不发光）。
    pub emissive: Option<[f32; 3]>,
}

/// 材质的 PBR 常数（贴图材质取 [1,1,1,1] 因子，由纹素表达）。
pub fn surface_params(surface: Surface) -> SurfaceParams {
    match surface {
        Surface::Water => SurfaceParams {
            base_color: [0.020, 0.055, 0.075, 1.0],
            metallic: 0.0,
            roughness: 0.045,
            emissive: None,
        },
        Surface::Coal => SurfaceParams {
            base_color: [1.0, 1.0, 1.0, 1.0],
            metallic: 0.0,
            roughness: 0.75,
            emissive: Some([3.2, 1.05, 0.22]),
        },
        _ => SurfaceParams {
            base_color: [1.0, 1.0, 1.0, 1.0],
            metallic: 1.0,
            roughness: 1.0,
            emissive: None,
        },
    }
}

/// 生成材质贴图（纯函数：同参数逐位同输出）。
pub fn surface_texture(surface: Surface) -> TexSet {
    match surface {
        Surface::Stone => build(512, 2.2, stone),
        Surface::Paving => build(512, 2.4, paving),
        Surface::Brick => build(512, 2.6, brick),
        Surface::Marble => build(256, 1.2, marble),
        Surface::Wood => build(256, 1.8, wood),
        Surface::Iron => build(256, 2.0, iron),
        Surface::Cloth => build(256, 2.2, cloth),
        Surface::Coal => build(128, 2.6, coal),
        Surface::Water => TexSet {
            base: CpuTexture::solid([6, 14, 20, 255]),
            normal: None,
            orm: None,
        },
    }
}

/// 纹素采样结果（linear）。
#[derive(Clone, Copy, Debug)]
struct Mat {
    base: [f32; 3],
    height: f32,
    roughness: f32,
    metallic: f32,
    ao: f32,
}

/// 通用构建：基色 sRGB 编码、法线中心差分、ORM 打包。
fn build(size: u32, bump: f32, sample: impl Fn(f32, f32) -> Mat) -> TexSet {
    let mut base = Vec::with_capacity((size * size * 4) as usize);
    let mut orm = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let u = (x as f32 + 0.5) / size as f32;
            let v = (y as f32 + 0.5) / size as f32;
            let m = sample(u, v);
            let s = linear_to_srgb(m.base);
            base.extend_from_slice(&[
                (s[0] * 255.0 + 0.5) as u8,
                (s[1] * 255.0 + 0.5) as u8,
                (s[2] * 255.0 + 0.5) as u8,
                255,
            ]);
            orm.extend_from_slice(&[
                (m.ao.clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                (m.roughness.clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                (m.metallic.clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
                255,
            ]);
        }
    }
    let normal = normal_from_height(size, bump, |u, v| {
        sample(u.rem_euclid(1.0), v.rem_euclid(1.0)).height
    });
    TexSet {
        base: CpuTexture {
            width: size,
            height: size,
            rgba: base,
            srgb: true,
        },
        normal: Some(CpuTexture {
            width: size,
            height: size,
            rgba: normal,
            srgb: false,
        }),
        orm: Some(CpuTexture {
            width: size,
            height: size,
            rgba: orm,
            srgb: false,
        }),
    }
}

/// 石灰岩铺地：细颗粒 + 坑洼 + 几道裂纹。
fn stone(u: f32, v: f32) -> Mat {
    let grain = fbm(u, v, 48, 4, 0x5701);
    let pits = fbm(u, v, 9, 4, 0x5702);
    let cracks = ridged(u, v, 5, 4, 0x5703);
    let crack = smooth((cracks - 0.78) / 0.10);
    let patch = fbm(u, v, 3, 4, 0x5704);
    // 斑驳与坑洼要够狠：第一版「几乎平的灰面」在游戏里看着像塑料
    let shade = 0.70 + 0.34 * patch + 0.16 * (grain - 0.5);
    // 石灰岩不是白石膏：linear 0.40 在直射阳光 + IBL 下会顶到削波
    let base = [0.252 * shade, 0.246 * shade, 0.228 * shade];
    Mat {
        base,
        height: grain * 0.30 + pits * 0.55 + patch * 0.30 - crack * 0.70,
        roughness: (0.72 + 0.22 * pits - 0.06 * crack).clamp(0.0, 1.0),
        metallic: 0.0,
        ao: (0.82 + 0.18 * pits - 0.34 * crack).clamp(0.0, 1.0),
    }
}

/// 砖墙：错缝砌法 + 逐砖色差 + 灰浆缝（缝内凹、更粗糙）。
fn brick(u: f32, v: f32) -> Mat {
    const ROWS: f32 = 8.0;
    const COLS: f32 = 3.0;
    const SEAM: f32 = 0.055;
    let row = (v * ROWS).floor();
    let offset = if (row as i32) % 2 == 0 { 0.0 } else { 0.5 };
    let bx = (u * COLS + offset).fract();
    let by = (v * ROWS).fract();
    // 砖缝：到砖块边界的距离
    let edge = bx.min(1.0 - bx).min(by.min(1.0 - by));
    let mortar = 1.0 - smooth((edge - SEAM) / 0.035);
    // 逐砖色差（哈希到砖格）
    let brick_id = (row * 7.0 + (u * COLS + offset).floor()) * 0.618;
    let tint = 0.86 + 0.28 * (brick_id.fract());
    let wear = fbm(u, v, 24, 3, 0x5801);
    let grime = fbm(u, v, 4, 4, 0x5802);
    let brick_color = [0.185 * tint, 0.092 * tint, 0.068 * tint];
    let mortar_color = [0.290, 0.278, 0.256];
    let soot = 1.0 - 0.28 * smooth((grime - 0.55) / 0.25);
    let base = [
        (brick_color[0] * (1.0 - mortar) + mortar_color[0] * mortar) * soot,
        (brick_color[1] * (1.0 - mortar) + mortar_color[1] * mortar) * soot,
        (brick_color[2] * (1.0 - mortar) + mortar_color[2] * mortar) * soot,
    ];
    Mat {
        base,
        height: (1.0 - mortar) * 1.0 + wear * 0.18 - mortar * 0.35,
        roughness: (0.72 + 0.18 * mortar + 0.10 * wear).clamp(0.0, 1.0),
        metallic: 0.0,
        ao: (0.98 - 0.22 * mortar - 0.08 * grime).clamp(0.0, 1.0),
    }
}

/// 铺地：4×4 方形石板（单块 1.15 m）+ 灰缝 + 逐块色差 + 磨损。
fn paving(u: f32, v: f32) -> Mat {
    const N: f32 = 4.0;
    let cell_x = (u * N).floor();
    let cell_y = (v * N).floor();
    let fx = (u * N).fract();
    let fy = (v * N).fract();
    let edge = fx.min(1.0 - fx).min(fy.min(1.0 - fy));
    let joint = 1.0 - smooth((edge - 0.028) / 0.020);
    let id = cell_x * 7.0 + cell_y * 13.0;
    let tint = 0.84 + 0.30 * (id * 0.618).fract();
    let grain = fbm(u * 2.0, v * 2.0, 36, 4, 0x6001);
    let wear = fbm(u * 2.0, v * 2.0, 6, 4, 0x6002);
    let pits = fbm(u * 4.0, v * 4.0, 22, 3, 0x6003);
    // 灰缝：更暗、更粗糙、有砂砾
    let joint_color = [0.160, 0.150, 0.134];
    let slab = 0.268 * tint * (0.86 + 0.20 * wear) * (0.92 + 0.16 * (grain - 0.5));
    let base = [
        slab * (1.0 - joint) + joint_color[0] * joint,
        slab * 0.982 * (1.0 - joint) + joint_color[1] * joint,
        slab * 0.930 * (1.0 - joint) + joint_color[2] * joint,
    ];
    Mat {
        base,
        height: (1.0 - joint) * (0.85 + 0.15 * grain) + pits * 0.12 - joint * 0.55,
        roughness: (0.70 + 0.16 * wear + 0.16 * joint + 0.08 * pits).clamp(0.0, 1.0),
        metallic: 0.0,
        ao: (0.94 + 0.06 * wear - 0.26 * joint).clamp(0.0, 1.0),
    }
}

/// 大理石：底色近白 + 脉络 + 轻微斑驳。
fn marble(u: f32, v: f32) -> Mat {
    // 脉络：低频脊状噪声沿一个方向拉伸
    let vein = ridged(u * 0.6 + v * 0.2, v * 1.6, 4, 4, 0x5901);
    let vein2 = ridged(u * 1.7 - v * 0.4, v * 0.8, 7, 3, 0x5902);
    let mottle = fbm(u, v, 16, 4, 0x5903);
    let vein_mask = smooth((vein2 - 0.84) / 0.05) * 0.7 + smooth((vein - 0.93) / 0.04) * 0.5;
    let shade = 0.92 + 0.08 * mottle - 0.22 * vein_mask.min(1.0);
    let base = [0.430 * shade, 0.418 * shade, 0.392 * shade];
    Mat {
        base,
        height: mottle * 0.7 + vein * 0.3 - vein_mask * 0.25,
        roughness: (0.30 + 0.16 * mottle + 0.10 * vein_mask).clamp(0.0, 1.0),
        metallic: 0.0,
        ao: (0.94 + 0.06 * mottle).clamp(0.0, 1.0),
    }
}

/// 木：顺纹木板 + 板缝 + 结疤。
fn wood(u: f32, v: f32) -> Mat {
    const PLANKS: f32 = 4.0;
    let plank = (u * PLANKS).floor();
    let within = (u * PLANKS).fract();
    let gap = 1.0 - smooth((within.min(1.0 - within) - 0.02) / 0.03);
    let grain = ridged(u * 0.15, v * 9.0, 5, 4, 0x5A01);
    let knot = smooth((fbm(u, v, 9, 3, 0x5A02) - 0.78) / 0.08);
    let tint = 0.85 + 0.30 * ((plank * 0.37).fract());
    let base = [
        (0.145 * tint) * (0.75 + 0.5 * grain) * (1.0 - 0.5 * knot),
        (0.098 * tint) * (0.75 + 0.5 * grain) * (1.0 - 0.5 * knot),
        (0.060 * tint) * (0.75 + 0.5 * grain) * (1.0 - 0.5 * knot),
    ];
    Mat {
        base,
        height: grain * 0.6 + knot * 0.5 - gap * 0.6,
        roughness: (0.62 + 0.18 * grain + 0.12 * gap).clamp(0.0, 1.0),
        metallic: 0.0,
        ao: (0.94 - 0.30 * gap - 0.10 * knot).clamp(0.0, 1.0),
    }
}

/// 铁：锻打面 + 锈斑（锈处金属度掉、粗糙度上）。
fn iron(u: f32, v: f32) -> Mat {
    let hammer = fbm(u, v, 14, 3, 0x5B01);
    let rust = fbm(u, v, 5, 4, 0x5B02);
    let rusty = smooth((rust - 0.56) / 0.18);
    let base = [
        0.055 + 0.050 * hammer + rusty * 0.220,
        0.052 + 0.048 * hammer + rusty * 0.105,
        0.055 + 0.050 * hammer + rusty * 0.045,
    ];
    Mat {
        base,
        height: hammer + rusty * 0.4,
        roughness: (0.42 + 0.20 * hammer + 0.32 * rusty).clamp(0.0, 1.0),
        metallic: (0.92 - 0.70 * rusty).clamp(0.0, 1.0),
        ao: (0.92 + 0.08 * hammer).clamp(0.0, 1.0),
    }
}

/// 旗帜布：斜纹织 + 褪色 + 破损（低频斑块）。
fn cloth(u: f32, v: f32) -> Mat {
    let tau = std::f32::consts::TAU;
    let warp = ((u + v) * 26.0 * tau).sin() * 0.5 + 0.5;
    let weft = ((u - v) * 26.0 * tau).sin() * 0.5 + 0.5;
    let weave = (warp * weft).sqrt();
    let fade = fbm(u, v, 3, 5, 0x5C01);
    let stain = fbm(u, v, 9, 3, 0x5C02);
    let shade = 0.62 + 0.38 * fade;
    let base = [0.235 * shade, 0.038 * shade, 0.042 * shade];
    Mat {
        base,
        height: weave * 0.35 + stain * 0.65,
        roughness: (0.78 + 0.14 * stain).clamp(0.0, 1.0),
        metallic: 0.0,
        ao: (0.88 + 0.12 * weave).clamp(0.0, 1.0),
    }
}

/// 炭火：暗炭块 + 缝隙里的亮炭（自发光由材质常数给）。
fn coal(u: f32, v: f32) -> Mat {
    let chunks = fbm(u, v, 18, 3, 0x5D01);
    let glow = ridged(u, v, 7, 3, 0x5D02);
    let hot = smooth((glow - 0.62) / 0.12);
    let base = [
        0.035 + 0.030 * chunks + hot * 0.60,
        0.030 + 0.024 * chunks + hot * 0.22,
        0.028 + 0.020 * chunks + hot * 0.05,
    ];
    Mat {
        base,
        height: chunks,
        roughness: (0.82 - 0.20 * hot).clamp(0.0, 1.0),
        metallic: 0.0,
        ao: 1.0,
    }
}

/// linear → sRGB 编码。
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

/// `smoothstep`（两端钳制）。
fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn textures_are_deterministic_and_sized() {
        for surface in Surface::ALL {
            let a = surface_texture(surface);
            let b = surface_texture(surface);
            assert_eq!(a.base.rgba, b.base.rgba, "{surface:?} 基色应逐位一致");
            let size = surface.size();
            assert_eq!(a.base.width, size.max(1));
            if size > 0 {
                let normal = a.normal.as_ref().expect("应带法线");
                let orm = a.orm.as_ref().expect("应带 ORM");
                assert_eq!(normal.rgba.len(), (size * size * 4) as usize);
                assert_eq!(orm.rgba.len(), (size * size * 4) as usize);
                assert!(!normal.srgb && !orm.srgb, "数据贴图须标线性");
                assert!(a.base.srgb, "基色须标 sRGB");
            }
        }
    }

    #[test]
    fn stone_and_marble_are_dielectric_iron_is_metal() {
        let mean_metal = |surface: Surface| {
            let orm = surface_texture(surface).orm.expect("应有 ORM");
            let sum: u32 = orm.rgba.chunks_exact(4).map(|p| p[2] as u32).sum();
            sum as f32 / (orm.rgba.len() / 4) as f32
        };
        assert!(mean_metal(Surface::Stone) < 2.0, "石头金属度应为 0");
        assert!(mean_metal(Surface::Brick) < 2.0, "砖金属度应为 0");
        assert!(mean_metal(Surface::Iron) > 120.0, "铁金属度应较高");
    }

    #[test]
    fn brick_rows_produce_height_variation() {
        let set = surface_texture(Surface::Brick);
        let normal = set.normal.expect("应有法线");
        let mut max_dev = 0i32;
        for px in normal.rgba.chunks_exact(4) {
            max_dev = max_dev.max((px[0] as i32 - 128).abs());
        }
        assert!(
            max_dev > 20,
            "砖缝应产生明显法线起伏，实际最大偏移 {max_dev}"
        );
    }

    #[test]
    fn water_has_no_texture_but_params() {
        let set = surface_texture(Surface::Water);
        assert!(set.normal.is_none() && set.orm.is_none());
        let params = surface_params(Surface::Water);
        assert!(params.roughness < 0.1, "水面应接近镜面");
        assert_eq!(params.metallic, 0.0);
    }
}
