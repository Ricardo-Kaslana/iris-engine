//! 程序化方块纹理图集。
//!
//! 图集是 128×128 的 sRGB 纹理，划分为 4×4 个 32 像素槽位。每槽图案
//! 仍是 16×16 像素，四周留 8 像素 gutter。gutter 必须足够宽：mip 链是
//! 整张图集一起降采样的，1 像素 gutter 在第 3 级就会让相邻槽位互相串色，
//! 表面在掠射角换 mip 时出现逐方块的颜色跳变（摩尔纹式闪烁）。8 像素
//! gutter 把串色推迟到第 4 级以后，方块色在 mip 之间基本稳定。
//!
//! 16 像素是 Minecraft 经典材质的工作分辨率，配合最近邻放大得到硬边
//! 像素画。
//!
//! 每个槽位用四档色板加少量固定特征生成：草方块有锯齿草边与簇状深浅，
//! 石头有两道折线裂纹，木板有板缝与错位拼缝，原木顶面有方形年轮，
//! 矿石是石头底上散布的 2×2 矿斑。图案不使用逐像素白噪声，避免远看
//! 像雪花屏。
//!
//! 树叶是唯一带 alpha 的槽位：图案里挖出约一成三的孔洞，配合
//! `AlphaMode::Mask` 的 alpha 测试做成 Minecraft「流畅」画质那种镂空树冠。
//! 孔洞是 4×4 格子里 3×3 的整块缺口，整体覆盖率刻意压在 8% 到 22% 之间
//! ——太低远看只是脏点，太高树冠会散成筛子。
//!
//! 图集仍是无压缩 sRGB 纹理，mip 链由渲染器按 `MipSettings` 生成；
//! 最近邻放大由 `TextureFilterSettings` 控制（缺省线性，游戏侧显式开启）。

use iris_engine::prelude::CpuTexture;

use crate::logic::block::Tile;

pub const ATLAS_SIZE: u32 = 128;
pub const TILE_SIZE: u32 = 32;
pub const GUTTER: u32 = 8;
const CONTENT: u32 = TILE_SIZE - GUTTER * 2;

/// 生成整张图集。纹理由固定哈希派生，同输入逐字节一致。
pub fn build_atlas() -> CpuTexture {
    let mut rgba = vec![255u8; (ATLAS_SIZE * ATLAS_SIZE * 4) as usize];
    for tile in [
        Tile::GrassTop,
        Tile::GrassSide,
        Tile::Dirt,
        Tile::Stone,
        Tile::Sand,
        Tile::WoodSide,
        Tile::WoodTop,
        Tile::Planks,
        Tile::Bedrock,
        Tile::Glowstone,
        Tile::CoalOre,
        Tile::IronOre,
        Tile::GoldOre,
        Tile::Leaves,
    ] {
        let (col, row) = tile.slot();
        let origin_x = col * TILE_SIZE;
        let origin_y = row * TILE_SIZE;
        for py in 0..TILE_SIZE {
            for px in 0..TILE_SIZE {
                // gutter 采样被钳制到图案边缘，形成天然的同色扩边。
                let inner_x = px.clamp(GUTTER, GUTTER + CONTENT - 1) - GUTTER;
                let inner_y = py.clamp(GUTTER, GUTTER + CONTENT - 1) - GUTTER;
                let [r, g, b] = pattern(tile, inner_x, inner_y);
                let index = (((origin_y + py) * ATLAS_SIZE + origin_x + px) * 4) as usize;
                rgba[index] = r;
                rgba[index + 1] = g;
                rgba[index + 2] = b;
                rgba[index + 3] = coverage(tile, inner_x, inner_y);
            }
        }
    }
    CpuTexture {
        width: ATLAS_SIZE,
        height: ATLAS_SIZE,
        rgba,
        srgb: true,
    }
}

/// 槽位内 UV。`u` 自左向右，`v` 自面底部向上（内部翻转图像行）。
pub fn tile_uv(tile: Tile, u: f32, v: f32) -> [f32; 2] {
    let (col, row) = tile.slot();
    let cell = TILE_SIZE as f32 / ATLAS_SIZE as f32;
    let inset = GUTTER as f32 / ATLAS_SIZE as f32;
    let inner = CONTENT as f32 / ATLAS_SIZE as f32;
    let u0 = col as f32 * cell + inset;
    let v0 = row as f32 * cell + inset;
    [
        u0 + u.clamp(0.0, 1.0) * inner,
        v0 + (1.0 - v.clamp(0.0, 1.0)) * inner,
    ]
}

fn hash(x: u32, y: u32, salt: u32) -> u32 {
    let mut z = (x as u64) | ((y as u64) << 32) ^ ((salt as u64) << 16);
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 31)) as u32
}

// 色板取自 BLOCKWILD 的观感：低饱和、偏灰的泥土与石材，草绿更深一档。
// 每格图案 = 底色 + 逐像素颗粒（±4）+ 结构化细节（簇、层、脊），这是
// 参考项目「远看干净、近看有细节」的关键配方。
const GRASS_BASE: [[u8; 3]; 3] = [[138, 176, 104], [150, 188, 116], [144, 182, 110]];
const DIRT_BASE: [u8; 3] = [146, 112, 86];
const DIRT_PEBBLE: [u8; 3] = [168, 140, 114];
const STONE_BASE: [u8; 3] = [140, 148, 154];
const STONE_PALE: [u8; 3] = [168, 168, 156];
const SAND_BASE: [u8; 3] = [230, 214, 166];
const BARK_BASE: [u8; 3] = [132, 98, 62];
const RINGS_BASE: [u8; 3] = [192, 158, 104];
const BEDROCK_BASE: [u8; 3] = [68, 74, 80];
const PLANK_BASE: [[u8; 3]; 4] = [
    [172, 132, 84],
    [182, 142, 92],
    [164, 124, 78],
    [176, 136, 88],
];
const GLOW_BASE: [u8; 3] = [247, 210, 110];
const GLOW_SPOT: [u8; 3] = [222, 171, 74];
const LEAF_BASE: [[u8; 3]; 4] = [[40, 76, 28], [56, 98, 36], [74, 120, 46], [96, 144, 60]];

fn tile_salt(tile: Tile) -> u32 {
    match tile {
        Tile::GrassTop => 0,
        Tile::GrassSide => 1,
        Tile::Dirt => 2,
        Tile::Stone => 3,
        Tile::Sand => 4,
        Tile::WoodSide => 5,
        Tile::WoodTop => 6,
        Tile::Planks => 11,
        Tile::Bedrock => 8,
        Tile::Glowstone => 9,
        Tile::CoalOre => 10,
        Tile::IronOre => 12,
        Tile::GoldOre => 13,
        Tile::Leaves => 7,
    }
}

fn pattern(tile: Tile, x: u32, y: u32) -> [u8; 3] {
    let grain = (hash(x, y, 90 + tile_salt(tile)) % 9) as i16 - 4;
    let (base, detail) = match tile {
        Tile::GrassTop => grass_top(x, y),
        Tile::GrassSide => {
            // 草皮覆盖上方三到五像素，边缘按列抖动出锯齿。
            let fringe = 3 + hash(x / 2, 0, 11) % 3;
            if y < fringe {
                grass_top(x, y)
            } else if y == fringe {
                let (base, detail) = dirt(x, y);
                (base, detail - 12)
            } else {
                dirt(x, y)
            }
        }
        Tile::Dirt => dirt(x, y),
        Tile::Stone => stone(x, y),
        Tile::Sand => {
            let ripple = (x / 4 + y) % 6;
            (SAND_BASE, if ripple == 0 { -6 } else { 2 })
        }
        Tile::WoodSide => {
            let ridge = (x + hash(0, y / 5, 43) % 2) % 6;
            let knot = (x as i32 - 10).abs() * 2 + (y as i32 - 9).abs();
            let detail = if (3..=4).contains(&knot) {
                -24
            } else {
                match ridge {
                    0 => -27,
                    1 => -12,
                    2 => 15,
                    3 => 7,
                    _ => 0,
                }
            };
            (BARK_BASE, detail)
        }
        Tile::WoodTop => {
            let dx = (x as i32 - 7).abs();
            let dy = (y as i32 - 8).abs();
            let ring = dx.max(dy) + dx.min(dy) / 2;
            let detail = if dx.max(dy) >= 7 {
                -39
            } else if ring % 3 == 0 {
                -21
            } else {
                7
            };
            (RINGS_BASE, detail)
        }
        Tile::Planks => {
            let board = (y / 4) as usize;
            let seam = y % 4 == 0;
            let joint = (x + board as u32 * 5) % 8 == 0;
            let grain = (hash(x / 2, y, 67) % 13) as i16 - 6;
            let detail = if seam {
                -28
            } else if joint {
                -19
            } else {
                grain
            };
            (PLANK_BASE[board % 4], detail)
        }
        Tile::Bedrock => {
            let fracture = (hash(x / 3, y / 2, 53) % 25) as i16 - 12;
            (BEDROCK_BASE, fracture)
        }
        Tile::Glowstone => {
            let cluster = hash(x / 2, y / 2, 57) % 3;
            let base = if cluster == 0 { GLOW_SPOT } else { GLOW_BASE };
            let detail = if hash(x, y, 59) % 17 == 0 { 12 } else { 0 };
            (base, detail)
        }
        Tile::CoalOre => ore(x, y, [28, 28, 30], [58, 58, 62], 61),
        Tile::IronOre => ore(x, y, [186, 142, 112], [222, 182, 148], 63),
        Tile::GoldOre => ore(x, y, [218, 172, 42], [252, 228, 92], 65),
        Tile::Leaves => {
            let (base, detail, _) = leaf(x, y);
            (base, detail)
        }
    };
    base.map(|channel| (channel as i16 + grain + detail).clamp(0, 255) as u8)
}

/// 叶片的一格：四档绿由可平铺的值噪声决定，噪声低谷处压暗成叶簇之间的
/// 暗缝，也是唯一可能挖出缺口的位置。
///
/// 返回（底色、明暗偏移、alpha）。颜色与 alpha 出自同一份噪声，
/// 缺口因此总落在暗缝上，读起来像叶簇间的空隙而不是均匀的筛点。
fn leaf(x: u32, y: u32) -> ([u8; 3], i16, u8) {
    let shade = leaf_noise(x, y, 4, 91);
    let level = if shade < 0.34 {
        0
    } else if shade < 0.52 {
        1
    } else if shade < 0.72 {
        2
    } else {
        3
    };
    let detail = if shade < 0.12 { -16 } else { 0 };
    // 最外一圈不挖洞：gutter 复制的是边缘像素，边缘透明会让 mip 链
    // 把叶块的边界啃掉，方块之间出现漏光的缝。
    let rim = x == 0 || y == 0 || x == CONTENT - 1 || y == CONTENT - 1;
    // 缺口是 3×3 的块，落在 4×4 格子的随机一角：比噪声阈值更成块，
    // 又不会排成整齐的格点。四分之一的格子带缺口，总覆盖率约 13%。
    let (gap_x, gap_y) = (x % 4, y % 4);
    let corner_x = hash(x / 4, y / 4, 97) % 2;
    let corner_y = hash(x / 4, y / 4, 99) % 2;
    let opened = hash(x / 4, y / 4, 101) % 100 < 26;
    let in_gap =
        gap_x >= corner_x && gap_x < corner_x + 3 && gap_y >= corner_y && gap_y < corner_y + 3;
    let hole = !rim && opened && in_gap;
    (LEAF_BASE[level], detail, if hole { 0 } else { 255 })
}

/// 可平铺的值噪声：格点数 `cells` 沿图案区环绕，相邻方块之间没有接缝。
fn leaf_noise(x: u32, y: u32, cells: i32, salt: u32) -> f32 {
    let scale = CONTENT as f32 / cells as f32;
    let fx = x as f32 / scale;
    let fy = y as f32 / scale;
    let x0 = fx.floor() as i32;
    let y0 = fy.floor() as i32;
    let tx = smooth(fx - x0 as f32);
    let ty = smooth(fy - y0 as f32);
    let wrap = |index: i32| index.rem_euclid(cells) as u32;
    let sample =
        |dx: i32, dy: i32| (hash(wrap(x0 + dx), wrap(y0 + dy), salt) >> 8) as f32 / 16_777_216.0;
    let top = sample(0, 0) + (sample(1, 0) - sample(0, 0)) * tx;
    let bottom = sample(0, 1) + (sample(1, 1) - sample(0, 1)) * tx;
    top + (bottom - top) * ty
}

fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// 叶片的 alpha 覆盖：只有树叶挖洞，其余槽位完全不透明。
fn coverage(tile: Tile, x: u32, y: u32) -> u8 {
    if tile != Tile::Leaves {
        return 255;
    }
    leaf(x, y).2
}

/// 草方块顶面：4×4 簇的三档底色，加斜向排列的草叶亮纹。
fn grass_top(x: u32, y: u32) -> ([u8; 3], i16) {
    let patch = hash(x / 4, y / 4, 19) % 3;
    let blade = hash(x / 2, y / 3, 23);
    let clump = hash(x / 3, y / 3, 21);
    let detail = if clump % 9 == 0 {
        -22
    } else if x % 2 == blade % 2 && y % 3 < 2 {
        12
    } else {
        -4
    };
    (GRASS_BASE[patch as usize], detail)
}

/// 泥土：3×3 土块起伏，每六行一道横向层理，稀疏的浅色小石子。
fn dirt(x: u32, y: u32) -> ([u8; 3], i16) {
    let clod = (hash(x / 3, y / 3, 29) % 17) as i16 - 8;
    // 只保留极淡的层理；明显的横行会在大面积土墙上读成砌砖。
    let layer = (y + hash(x / 4, 0, 31) % 3) % 7;
    let detail = clod
        + match layer {
            0 => -4,
            1 => 2,
            _ => 0,
        };
    let base = if hash(x, y, 33) % 47 < 2 {
        DIRT_PEBBLE
    } else {
        DIRT_BASE
    };
    (base, detail)
}

/// 石头：5×4 斑块 + 每七行一道浅层理，偶见浅色石粒。
fn stone(x: u32, y: u32) -> ([u8; 3], i16) {
    let patch = (hash((x + y / 4 * 2) / 5, y / 4, 37) % 17) as i16 - 8;
    let layer = (y + hash(x / 6, 0, 41) % 3) % 7;
    let detail = patch
        + match layer {
            0 => -11,
            1 => 6,
            _ => 0,
        };
    let base = if hash(x, y, 39) % 59 < 2 {
        STONE_PALE
    } else {
        STONE_BASE
    };
    (base, detail)
}

/// 矿石：石头底上按 2×2 矿斑成簇，斑心亮、外圈暗。
fn ore(x: u32, y: u32, dark: [u8; 3], light: [u8; 3], salt: u32) -> ([u8; 3], i16) {
    if hash(x / 2, y / 2, salt) % 5 == 0 {
        let base = if x % 2 == 0 && y % 2 == 0 {
            light
        } else {
            dark
        };
        (base, 0)
    } else {
        stone(x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_shape_and_alpha() {
        let atlas = build_atlas();
        assert_eq!(atlas.width, ATLAS_SIZE);
        assert_eq!(atlas.height, ATLAS_SIZE);
        assert_eq!(atlas.rgba.len(), (ATLAS_SIZE * ATLAS_SIZE * 4) as usize);
        assert!(atlas.srgb);
        // 除树叶外全部不透明：裁切材质只服务树叶，其余方块不能被误裁。
        let (leaf_col, leaf_row) = Tile::Leaves.slot();
        for y in 0..ATLAS_SIZE {
            for x in 0..ATLAS_SIZE {
                let index = ((y * ATLAS_SIZE + x) * 4) as usize;
                let alpha = atlas.rgba[index + 3];
                let in_leaves = (leaf_col * TILE_SIZE..(leaf_col + 1) * TILE_SIZE).contains(&x)
                    && (leaf_row * TILE_SIZE..(leaf_row + 1) * TILE_SIZE).contains(&y);
                if !in_leaves {
                    assert_eq!(alpha, 255, "非树叶像素 ({x},{y}) 不应透明");
                }
            }
        }
    }

    /// 树叶的孔洞覆盖率落在 8% 到 22%：低于下界远看只是脏点，高于
    /// 上界树冠会散成筛子。
    #[test]
    fn leaf_cutout_coverage_is_in_range() {
        let mut holes = 0;
        for y in 0..CONTENT {
            for x in 0..CONTENT {
                if coverage(Tile::Leaves, x, y) == 0 {
                    holes += 1;
                }
            }
        }
        let share = holes as f32 / (CONTENT * CONTENT) as f32;
        println!("树叶孔洞覆盖率 {share}");
        assert!(
            (0.08..=0.22).contains(&share),
            "树叶孔洞覆盖率越界：{share}（{holes} 像素）"
        );
    }

    /// 孔洞必须成簇：不能出现整片均匀的棋盘式筛网。
    #[test]
    fn leaf_holes_are_clustered() {
        let mut lone = 0;
        let mut total = 0;
        for y in 0..CONTENT {
            for x in 0..CONTENT {
                if coverage(Tile::Leaves, x, y) == 0 {
                    total += 1;
                    let neighbours = [(1, 0), (-1, 0), (0, 1), (0, -1)];
                    let touching = neighbours.iter().any(|(dx, dy)| {
                        let nx = x as i32 + dx;
                        let ny = y as i32 + dy;
                        nx >= 0
                            && ny >= 0
                            && (nx as u32) < CONTENT
                            && (ny as u32) < CONTENT
                            && coverage(Tile::Leaves, nx as u32, ny as u32) == 0
                    });
                    if !touching {
                        lone += 1;
                    }
                }
            }
        }
        assert!(total > 0);
        assert!(
            lone * 3 <= total,
            "孤立孔洞过多：{lone}/{total}，孔洞应成簇"
        );
    }

    /// 同一次构建与再次构建逐字节一致。
    #[test]
    fn atlas_is_deterministic() {
        let a = build_atlas();
        let b = build_atlas();
        assert_eq!(a.rgba, b.rgba);
    }

    /// 每个槽位的 UV 都落在自己的 14×14 图案区内。
    #[test]
    fn uvs_stay_inside_tile_content() {
        for tile in [Tile::GrassTop, Tile::Stone, Tile::Bedrock] {
            let (col, row) = tile.slot();
            let cell = TILE_SIZE as f32 / ATLAS_SIZE as f32;
            let inset = GUTTER as f32 / ATLAS_SIZE as f32;
            let u0 = col as f32 * cell + inset;
            let u1 = u0 + CONTENT as f32 / ATLAS_SIZE as f32;
            let v0 = row as f32 * cell + inset;
            let v1 = v0 + CONTENT as f32 / ATLAS_SIZE as f32;

            let [u, v] = tile_uv(tile, 0.0, 0.0);
            assert!((u - u0).abs() < 1e-6 && (v - v1).abs() < 1e-6);
            let [u, v] = tile_uv(tile, 1.0, 1.0);
            assert!((u - u1).abs() < 1e-6 && (v - v0).abs() < 1e-6);
            for (u, v) in [(0.25, 0.25), (0.5, 0.5), (0.9, 0.1)] {
                let [su, sv] = tile_uv(tile, u, v);
                assert!((u0..=u1).contains(&su));
                assert!((v0..=v1).contains(&sv));
            }
        }
    }

    /// gutter 像素复制中央图案的边缘，不与相邻槽位混色。
    #[test]
    fn gutter_repeats_tile_edge() {
        let atlas = build_atlas();
        let (col, row) = Tile::Stone.slot();
        let ox = col * TILE_SIZE;
        let oy = row * TILE_SIZE;
        let pixel = |x: u32, y: u32| {
            let index = (((oy + y) * ATLAS_SIZE + ox + x) * 4) as usize;
            [
                atlas.rgba[index],
                atlas.rgba[index + 1],
                atlas.rgba[index + 2],
            ]
        };
        for y in 0..TILE_SIZE {
            assert_eq!(pixel(0, y), pixel(GUTTER, y));
            assert_eq!(pixel(TILE_SIZE - 1, y), pixel(TILE_SIZE - 1 - GUTTER, y));
        }
    }

    /// 新增槽位都有可见图案：矿石含两级矿斑，树叶以绿色为主且有明暗变化。
    #[test]
    fn new_tiles_have_visible_patterns() {
        let atlas = build_atlas();
        for tile in [Tile::CoalOre, Tile::IronOre, Tile::GoldOre, Tile::Leaves] {
            let (col, row) = tile.slot();
            let mut colors = std::collections::BTreeSet::new();
            let mut channel_sum = [0u32; 3];
            for y in GUTTER..(TILE_SIZE - GUTTER) {
                for x in GUTTER..(TILE_SIZE - GUTTER) {
                    let index =
                        (((row * TILE_SIZE + y) * ATLAS_SIZE + col * TILE_SIZE + x) * 4) as usize;
                    let rgb = [
                        atlas.rgba[index],
                        atlas.rgba[index + 1],
                        atlas.rgba[index + 2],
                    ];
                    colors.insert(rgb);
                    for channel in 0..3 {
                        channel_sum[channel] += rgb[channel] as u32;
                    }
                }
            }
            assert!(colors.len() >= 4, "{tile:?} 图案过于单调");
            if tile == Tile::Leaves {
                assert!(
                    channel_sum[1] > channel_sum[0] && channel_sum[1] > channel_sum[2],
                    "树叶应以绿色为主"
                );
            }
        }
    }

    /// 矿石斑点必须换色，不能只是石头底纹的重复。
    #[test]
    fn ore_tiles_differ_from_plain_stone() {
        for tile in [Tile::CoalOre, Tile::IronOre, Tile::GoldOre] {
            let mut differing = 0;
            for y in 0..CONTENT {
                for x in 0..CONTENT {
                    if pattern(tile, x, y) != pattern(Tile::Stone, x, y) {
                        differing += 1;
                    }
                }
            }
            assert!(
                differing > CONTENT * CONTENT / 20,
                "{tile:?} 与石头的差异像素不足二十分之一：{differing}"
            );
        }
    }
}
