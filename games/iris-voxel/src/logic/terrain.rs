//! 确定性体素地形生成。
//!
//! 用 splitmix64 哈希派生格点值噪声，两个频率叠加成高度场。生成过程
//! 不依赖随机数生成器与全局状态，同 seed 逐字节一致。世界中心压平出
//! 一块出生平台，平台外围用切比雪夫距离线性过渡到自然高度。

use glam::Vec3;

use super::block::Block;
use super::world::{CHUNK_SIZE, VoxelWorld, WORLD_CHUNKS, WORLD_SIZE};

/// 出生平台高度（顶层实心方块的 y）。
pub const PLATEAU_HEIGHT: usize = 14;
/// 平台完全压平的切比雪夫半径（格）。
pub const PLATEAU_FLAT_RADIUS: i32 = 4;
/// 平台向自然地形过渡的半径（格）。
pub const PLATEAU_BLEND_RADIUS: i32 = 12;

/// 出生点脚底所在的方块坐标（平台中心）。
pub const SPAWN_BLOCK: [i32; 3] = [
    (WORLD_SIZE / 2) as i32,
    PLATEAU_HEIGHT as i32,
    (WORLD_SIZE / 2) as i32,
];

/// 出生点胶囊中心的建议位置（脚底在平台上表面上方 2 cm）。
pub fn spawn_position() -> Vec3 {
    Vec3::new(
        SPAWN_BLOCK[0] as f32 + 0.5,
        SPAWN_BLOCK[1] as f32 + 1.0 + super::player::PLAYER_HALF_HEIGHT + 0.02,
        SPAWN_BLOCK[2] as f32 + 0.5,
    )
}

/// splitmix64 终混函数（与引擎粒子系统的 `mix64` 同构，无全局状态）。
fn mix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// 格点哈希：返回 `[0, 1)`。
fn lattice(x: i32, z: i32, seed: u64) -> f32 {
    let key = (x as i64 as u64)
        .wrapping_mul(0xD6E8_FEB8_6659_FD93)
        .rotate_left(31)
        ^ (z as i64 as u64).wrapping_mul(0xA24B_AED4_963E_E407)
        ^ seed;
    ((mix64(key) >> 40) as f32) / ((1u64 << 24) as f32)
}

fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// 双线性平滑的值噪声。
fn value_noise(x: f32, z: f32, seed: u64) -> f32 {
    let x0 = x.floor() as i32;
    let z0 = z.floor() as i32;
    let tx = smooth(x - x0 as f32);
    let tz = smooth(z - z0 as f32);
    let a = lattice(x0, z0, seed);
    let b = lattice(x0 + 1, z0, seed);
    let c = lattice(x0, z0 + 1, seed);
    let d = lattice(x0 + 1, z0 + 1, seed);
    let ab = a + (b - a) * tx;
    let cd = c + (d - c) * tx;
    ab + (cd - ab) * tz
}

/// 自然高度场（未含出生平台压平）。
fn natural_height(x: i32, z: i32, seed: u64) -> f32 {
    let coarse = value_noise(x as f32 / 24.0, z as f32 / 24.0, seed);
    let fine = value_noise(x as f32 / 8.0, z as f32 / 8.0, seed ^ 0x51ED_2701_9C2B_8E4F);
    let n = coarse * 0.7 + fine * 0.3;
    (12.0 + (n - 0.5) * 12.0).clamp(4.0, 26.0)
}

/// 某一列的表层实心方块 y（含出生平台过渡）。
pub fn column_height(x: i32, z: i32, seed: u64) -> usize {
    let center = (WORLD_SIZE / 2) as i32;
    let distance = (x - center).abs().max((z - center).abs());
    if distance <= PLATEAU_FLAT_RADIUS {
        return PLATEAU_HEIGHT;
    }
    let natural = natural_height(x, z, seed);
    if distance >= PLATEAU_BLEND_RADIUS {
        return natural.round() as usize;
    }
    let t = (distance - PLATEAU_FLAT_RADIUS) as f32
        / (PLATEAU_BLEND_RADIUS - PLATEAU_FLAT_RADIUS) as f32;
    (PLATEAU_HEIGHT as f32 + (natural - PLATEAU_HEIGHT as f32) * t)
        .round()
        .clamp(4.0, 26.0) as usize
}

/// 生成整个世界的方块数据。
pub(super) fn generate(seed: u64) -> VoxelWorld {
    let mut world = VoxelWorld::empty(seed);
    for cz in 0..WORLD_CHUNKS {
        for cx in 0..WORLD_CHUNKS {
            let chunk = world
                .chunk_mut(glam::IVec2::new(cx as i32, cz as i32))
                .expect("生成的区块坐标必在世界内");
            for z in 0..CHUNK_SIZE {
                for x in 0..CHUNK_SIZE {
                    let world_x = (cx * CHUNK_SIZE + x) as i32;
                    let world_z = (cz * CHUNK_SIZE + z) as i32;
                    let height = column_height(world_x, world_z, seed);
                    fill_column(chunk, x, z, height);
                }
            }
        }
    }
    world
}

/// 一列：基岩 → 石头 → 泥土 → 表层（草地或沙）。
fn fill_column(chunk: &mut super::world::Chunk, x: usize, z: usize, height: usize) {
    chunk.set(x, 0, z, Block::Bedrock);
    let stone_top = height.saturating_sub(2);
    for y in 1..stone_top {
        chunk.set(x, y, z, Block::Stone);
    }
    for y in stone_top..height {
        chunk.set(x, y, z, Block::Dirt);
    }
    let surface = if height <= 7 {
        Block::Sand
    } else {
        Block::Grass
    };
    chunk.set(x, height, z, surface);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logic::world::{CHUNK_COUNT, CHUNK_HEIGHT, Chunk};

    /// 同 seed 两次生成逐字节一致；不同 seed 至少一列不同。
    #[test]
    fn generation_is_deterministic() {
        let a = VoxelWorld::generate(20261022);
        let b = VoxelWorld::generate(20261022);
        for index in 0..CHUNK_COUNT {
            assert_eq!(a.chunks()[index].raw(), b.chunks()[index].raw());
        }
        let c = VoxelWorld::generate(20261023);
        let mut differs = false;
        for index in 0..CHUNK_COUNT {
            if a.chunks()[index].raw() != c.chunks()[index].raw() {
                differs = true;
                break;
            }
        }
        assert!(differs, "不同 seed 应生成不同地形");
    }

    /// 出生平台完全压平，且中心列的表层是草地、脚下是实心。
    #[test]
    fn spawn_plateau_is_flat_and_solid() {
        let world = VoxelWorld::generate(7);
        let center = (WORLD_SIZE / 2) as i32;
        for dz in -PLATEAU_FLAT_RADIUS..=PLATEAU_FLAT_RADIUS {
            for dx in -PLATEAU_FLAT_RADIUS..=PLATEAU_FLAT_RADIUS {
                let height = column_height(center + dx, center + dz, 7);
                assert_eq!(height, PLATEAU_HEIGHT);
                assert_eq!(
                    world.surface_height(center + dx, center + dz),
                    Some(PLATEAU_HEIGHT)
                );
            }
        }
        let spawn = SPAWN_BLOCK;
        assert_eq!(
            world.block(glam::IVec3::new(spawn[0], spawn[1], spawn[2])),
            Block::Grass
        );
        assert_eq!(
            world.block(glam::IVec3::new(spawn[0], spawn[1] + 1, spawn[2])),
            Block::Air
        );
        assert!(spawn_position().y > PLATEAU_HEIGHT as f32 + 1.0);
    }

    /// 每列都有基岩底、连续实心柱与高于表层的空气。
    #[test]
    fn columns_are_well_formed() {
        let world = VoxelWorld::generate(99);
        for z in (0..WORLD_SIZE).step_by(13) {
            for x in (0..WORLD_SIZE).step_by(17) {
                let height = world.surface_height(x as i32, z as i32).unwrap();
                assert!(height < CHUNK_HEIGHT - 1);
                assert_eq!(
                    world.block(glam::IVec3::new(x as i32, 0, z as i32)),
                    Block::Bedrock
                );
                for y in 0..=height {
                    assert!(
                        world
                            .block(glam::IVec3::new(x as i32, y as i32, z as i32))
                            .is_solid(),
                        "列 ({x},{z}) 在 y={y} 处断裂"
                    );
                }
                assert_eq!(
                    world.block(glam::IVec3::new(x as i32, height as i32 + 1, z as i32)),
                    Block::Air
                );
            }
        }
    }

    /// 低海拔列的表层是沙，高海拔是草。
    #[test]
    fn surface_material_depends_on_height() {
        let mut chunk = Chunk::empty();
        fill_column(&mut chunk, 0, 0, 5);
        assert_eq!(chunk.get(0, 5, 0), Block::Sand);
        fill_column(&mut chunk, 1, 0, 15);
        assert_eq!(chunk.get(1, 15, 0), Block::Grass);
    }

    /// 平台过渡带高度单调靠近自然高度，最终在边界外与自然高度一致。
    #[test]
    fn plateau_blends_to_natural_height() {
        let seed = 12345;
        let center = (WORLD_SIZE / 2) as i32;
        let outer = center + PLATEAU_BLEND_RADIUS;
        assert_eq!(
            column_height(outer, center, seed),
            natural_height(outer, center, seed).round() as usize
        );
        let inner = column_height(center + PLATEAU_FLAT_RADIUS + 1, center, seed);
        assert!((inner as i32 - PLATEAU_HEIGHT as i32).abs() <= 6);
    }

    /// 噪声在 [0, 1) 内，且对相同坐标稳定。
    #[test]
    fn value_noise_is_bounded_and_stable() {
        for i in 0..64 {
            let x = i as f32 * 0.37 - 7.0;
            let z = i as f32 * -0.23 + 3.0;
            let value = value_noise(x, z, 42);
            assert!((0.0..1.0).contains(&value), "噪声越界：{value}");
            assert_eq!(value.to_bits(), value_noise(x, z, 42).to_bits());
        }
    }
}
