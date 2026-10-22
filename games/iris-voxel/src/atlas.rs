//! 程序化方块纹理图集。
//!
//! 图集是 128×128 的 sRGB 纹理，划分为 4×4 个 32 像素槽位。每槽内部
//! 保留 2 像素 gutter，实际图案绘制在中央 28×28 区域；UV 内缩到图案
//! 区，线性采样只会在 gutter 内重复图案边缘，不会串到相邻槽位。
//!
//! 引擎没有逐材质采样器与 nearest 过滤，本轮用无压缩声明保住像素画
//! 颜色，纹理放大仍会经过线性过滤（诚实边界见 ARCHITECTURE §8）。

use iris_engine::prelude::CpuTexture;

use crate::logic::block::Tile;

pub const ATLAS_SIZE: u32 = 128;
pub const TILE_SIZE: u32 = 32;
pub const GUTTER: u32 = 2;
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
                rgba[index + 3] = 255;
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

fn noise(x: u32, y: u32, salt: u32, amount: i32) -> i32 {
    (hash(x, y, salt) % (amount as u32 * 2 + 1)) as i32 - amount
}

fn shade(color: [i32; 3], delta: i32) -> [u8; 3] {
    [
        (color[0] + delta).clamp(0, 255) as u8,
        (color[1] + delta).clamp(0, 255) as u8,
        (color[2] + delta).clamp(0, 255) as u8,
    ]
}

fn pattern(tile: Tile, x: u32, y: u32) -> [u8; 3] {
    match tile {
        Tile::GrassTop => {
            let speck = (hash(x, y, 11) % 13 == 0) as i32;
            shade([72, 132, 58], noise(x, y, 3, 10) - speck * 10)
        }
        Tile::GrassSide => {
            if y < 8 {
                let edge = if y + (hash(x, 0, 7) % 3) >= 8 { 1 } else { 0 };
                if edge == 1 {
                    shade([66, 112, 50], noise(x, y, 5, 6))
                } else {
                    shade([72, 132, 58], noise(x, y, 3, 8))
                }
            } else {
                shade([112, 82, 55], noise(x, y, 9, 9))
            }
        }
        Tile::Dirt => shade([112, 82, 55], noise(x, y, 13, 10)),
        Tile::Stone => {
            let crack = (hash(x / 5, y / 3, 17) % 9 == 0) as i32;
            shade([124, 126, 130], noise(x, y, 19, 7) - crack * 12)
        }
        Tile::Sand => shade([214, 198, 140], noise(x, y, 23, 6)),
        Tile::WoodSide => {
            let grain = ((x + hash(x / 4, 0, 29) % 2) % 5 == 0) as i32;
            shade([126, 90, 48], noise(x, y, 31, 5) - grain * 14)
        }
        Tile::WoodTop => {
            let center = CONTENT as i32 / 2;
            let dx = x as i32 - center;
            let dy = y as i32 - center;
            let ring = ((dx * dx + dy * dy) as f32).sqrt() as i32;
            let band = (ring % 5 < 2) as i32;
            shade([148, 108, 60], noise(x, y, 37, 4) - band * 12)
        }
        Tile::Planks => {
            let seam = (y % 7 == 0) as i32;
            let joint = ((x + (y / 7) * 5) % 16 == 0) as i32;
            shade([164, 122, 70], noise(x, y, 41, 5) - seam * 28 - joint * 10)
        }
        Tile::Bedrock => {
            let dark = (hash(x / 2, y / 2, 43) % 3 == 0) as i32;
            shade([62, 62, 66], noise(x, y, 47, 18) - dark * 22)
        }
        Tile::Glowstone => {
            let speck = (hash(x, y, 53) % 7 == 0) as i32;
            let dark = (hash(x / 3, y / 3, 59) % 4 == 0) as i32;
            shade([238, 202, 92], noise(x, y, 61, 18) + speck * 24 - dark * 26)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_shape_and_opacity() {
        let atlas = build_atlas();
        assert_eq!(atlas.width, ATLAS_SIZE);
        assert_eq!(atlas.height, ATLAS_SIZE);
        assert_eq!(atlas.rgba.len(), (ATLAS_SIZE * ATLAS_SIZE * 4) as usize);
        assert!(atlas.srgb);
        assert!(atlas.rgba.chunks_exact(4).all(|pixel| pixel[3] == 255));
    }

    /// 同一次构建与再次构建逐字节一致。
    #[test]
    fn atlas_is_deterministic() {
        let a = build_atlas();
        let b = build_atlas();
        assert_eq!(a.rgba, b.rgba);
    }

    /// 每个槽位的 UV 都落在自己的 28×28 图案区内。
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
}
