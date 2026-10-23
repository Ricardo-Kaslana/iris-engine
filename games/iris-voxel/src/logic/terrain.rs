//! 确定性体素地形生成。
//!
//! 生成顺序固定为高度场、洞穴、洞口坡道、矿脉、树木与洞穴荧光石。
//! 每一步只读方块数据并直接写区块数组，光照在
//! [`VoxelWorld::generate`] 的最后一次性全量重算。
//!
//! 高度场用 splitmix64 哈希派生的两倍频值噪声。洞穴用三维值噪声的
//! 两种形态：低频洞厅取高值区域，两条独立噪声同时接近中值的等值面
//! 交线形成隧道。洞穴只挖石头，并在表面块下方保留三层地壳，因此地表
//! 轮廓、出生平台与树木立足的草地不受影响；洞口坡道是唯一贯穿地壳的
//! 通道。
//!
//! 矿脉按区块做确定性的短随机游走，只替换石头，因此不会悬在洞穴
//! 空气里；先挖洞穴再放矿脉，洞穴壁上会露出矿石。树木只种在草地，
//! 与出生点和彼此保持固定间距。洞穴底部的荧光石晶簇是地下唯一的
//! 光源，也是本轮光照系统的验收对象。
//!
//! 已知限制：没有群系与生物群系过渡，洞穴只有噪声形态、没有地下
//! 水体与岩浆，树木只有一种固定冠形且不会生长，矿脉没有开采掉落与
//! 合成用途。

use glam::{IVec2, IVec3, Vec3};

use super::block::Block;
use super::mix64;
use super::world::{CHUNK_SIZE, VoxelWorld, WORLD_CHUNKS, WORLD_SIZE};

/// 出生平台高度（顶层实心方块的 y）。
pub const PLATEAU_HEIGHT: usize = 14;
/// 平台完全压平的切比雪夫半径（格）。
pub const PLATEAU_FLAT_RADIUS: i32 = 4;
/// 平台向自然地形过渡的半径（格）。
pub const PLATEAU_BLEND_RADIUS: i32 = 12;

/// 世界中心列（出生点所在列）。
const CENTER: i32 = (WORLD_SIZE / 2) as i32;

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

/// 洞穴挖掘的最低方块层。0 层是基岩，1 层保留为基岩上方的石头垫层。
pub const CAVE_MIN_Y: i32 = 2;
/// 表面块下方保留的实心层数。洞穴最深只到 `表面高度 - 4`。
pub const CAVE_CRUST_BELOW_SURFACE: i32 = 3;

/// 洞口坡道的起点与朝向。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntranceSite {
    pub mouth_x: i32,
    pub mouth_z: i32,
    pub dir_x: i32,
    pub dir_z: i32,
}

/// 洞口候选方位：出生点外环的四个正方向。
const ENTRANCE_DIRECTIONS: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
/// 洞口候选的半径范围。取环上最高的一列，保证坡道有足够的下降空间。
const ENTRANCE_RADIUS_MIN: i32 = 22;
const ENTRANCE_RADIUS_MAX: i32 = 34;
/// 洞口数量。
pub const ENTRANCE_COUNT: usize = 3;

/// 按种子在出生点外环选洞口：四个正方向各取半径范围内最高的一列，
/// 按高度降序取前三个，朝向取该方位的切线方向。
///
/// 高度优先是为了让坡道真的向下走：低洼处的洞口只能挖出一条水平沟，
/// 既不像洞口，也到不了洞穴层。同高度的候选按坐标排序，保证确定性。
pub fn entrance_sites(seed: u64) -> Vec<EntranceSite> {
    let mut candidates: Vec<(usize, i32, i32, i32, i32)> = Vec::new();
    for (dx, dz) in ENTRANCE_DIRECTIONS {
        let mut best: Option<(usize, i32, i32)> = None;
        for radius in ENTRANCE_RADIUS_MIN..=ENTRANCE_RADIUS_MAX {
            let x = CENTER + dx * radius;
            let z = CENTER + dz * radius;
            if !clear_of_world_edge(x, z) {
                continue;
            }
            let height = column_height(x, z, seed);
            if best.is_none_or(|entry| height > entry.0) {
                best = Some((height, x, z));
            }
        }
        if let Some((height, x, z)) = best {
            candidates.push((height, x, z, dx, dz));
        }
    }
    candidates.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    candidates.truncate(ENTRANCE_COUNT);
    candidates
        .into_iter()
        .map(|(_, x, z, dx, dz)| EntranceSite {
            mouth_x: x,
            mouth_z: z,
            dir_x: -dz,
            dir_z: dx,
        })
        .collect()
}

/// 坡道下降到的最低层高。
const ENTRANCE_FLOOR_Y: i32 = 8;
/// 坡道的最大步数（每步沿朝向前进一格）。
const ENTRANCE_MAX_STEPS: i32 = 44;
/// 坡道前段露天切口的步数，之后转为有顶隧道。
const ENTRANCE_OPEN_STEPS: i32 = 12;
/// 坡道触底后向前探洞的最大步数。
const ENTRANCE_PROBE_STEPS: i32 = 18;

/// 树木与出生点的最小切比雪夫距离。
pub const TREE_CLEAR_RADIUS: i32 = 10;
/// 两棵树的树干最小切比雪夫间距。参考风格是开阔草原上的疏林，
/// 间距比密林大得多，草地方能连片露出来。
pub const TREE_SPACING: i32 = 7;
/// 每千列草地中出现树苗候选的比例。
const TREE_PER_MILLE: u64 = 15;

/// 二维格点哈希，返回 `[0, 1)`。
fn lattice(x: i32, z: i32, seed: u64) -> f32 {
    let key = (x as i64 as u64)
        .wrapping_mul(0xD6E8_FEB8_6659_FD93)
        .rotate_left(31)
        ^ (z as i64 as u64).wrapping_mul(0xA24B_AED4_963E_E407)
        ^ seed;
    ((mix64(key) >> 40) as f32) / ((1u64 << 24) as f32)
}

/// 整数哈希：树木选址与矿脉流使用。
fn hash2(x: i32, z: i32, seed: u64) -> u64 {
    mix64(
        (x as i64 as u64)
            .wrapping_mul(0xD6E8_FEB8_6659_FD93)
            .rotate_left(31)
            ^ (z as i64 as u64).wrapping_mul(0xA24B_AED4_963E_E407)
            ^ seed,
    )
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

/// 三维格点哈希，返回 `[0, 1)`。
fn lattice3(x: i32, y: i32, z: i32, seed: u64) -> f32 {
    let key = (x as i64 as u64)
        .wrapping_mul(0xD6E8_FEB8_6659_FD93)
        .rotate_left(31)
        ^ (y as i64 as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .rotate_left(17)
        ^ (z as i64 as u64).wrapping_mul(0xA24B_AED4_963E_E407)
        ^ seed;
    ((mix64(key) >> 40) as f32) / ((1u64 << 24) as f32)
}

/// 三线性平滑的值噪声。
fn value_noise3(x: f32, y: f32, z: f32, seed: u64) -> f32 {
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let z0 = z.floor() as i32;
    let tx = smooth(x - x0 as f32);
    let ty = smooth(y - y0 as f32);
    let tz = smooth(z - z0 as f32);
    let mut corners = [0.0f32; 8];
    for (index, corner) in corners.iter_mut().enumerate() {
        let dx = (index & 1) as i32;
        let dy = ((index >> 1) & 1) as i32;
        let dz = ((index >> 2) & 1) as i32;
        *corner = lattice3(x0 + dx, y0 + dy, z0 + dz, seed);
    }
    let x00 = corners[0] + (corners[1] - corners[0]) * tx;
    let x01 = corners[2] + (corners[3] - corners[2]) * tx;
    let x10 = corners[4] + (corners[5] - corners[4]) * tx;
    let x11 = corners[6] + (corners[7] - corners[6]) * tx;
    let y0f = x00 + (x01 - x00) * ty;
    let y1f = x10 + (x11 - x10) * ty;
    y0f + (y1f - y0f) * tz
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
    let distance = (x - CENTER).abs().max((z - CENTER).abs());
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

/// 该格是否属于洞穴噪声的挖掘区域。此函数只看噪声，不检查地表保护与
/// 方块种类；生成时还要叠加地壳与石头两个条件。
pub fn cave_noise(x: i32, y: i32, z: i32, seed: u64) -> bool {
    let cavern = value_noise3(
        x as f32 / 16.0,
        y as f32 / 10.0,
        z as f32 / 16.0,
        seed ^ 0xC0FF_EE01_5A17_3D29,
    );
    if cavern > 0.78 {
        return true;
    }
    let tunnel_a = value_noise3(
        x as f32 / 12.0,
        y as f32 / 8.0,
        z as f32 / 12.0,
        seed ^ 0x1B2C_3D4E_5F60_7182,
    );
    let tunnel_b = value_noise3(
        x as f32 / 12.0,
        y as f32 / 8.0,
        z as f32 / 12.0,
        seed ^ 0x8A90_A1B2_C3D4_E5F6,
    );
    (tunnel_a - 0.5).abs() < 0.075 && (tunnel_b - 0.5).abs() < 0.075
}

/// 生成整个世界的方块数据。
pub(super) fn generate(seed: u64) -> VoxelWorld {
    let mut world = VoxelWorld::empty(seed);
    let sites = entrance_sites(seed);
    fill_terrain(&mut world, seed);
    carve_caves(&mut world, seed);
    for site in &sites {
        carve_entrance(&mut world, seed, *site);
    }
    place_ore_veins(&mut world, seed);
    plant_trees(&mut world, seed, &sites);
    place_cave_glowstone(&mut world, seed);
    world
}

/// 高度场：逐区块、逐列填基岩、石头、泥土与表层。
fn fill_terrain(world: &mut VoxelWorld, seed: u64) {
    for cz in 0..WORLD_CHUNKS {
        for cx in 0..WORLD_CHUNKS {
            let coord = IVec2::new(cx as i32, cz as i32);
            let chunk = world.chunk_mut(coord).expect("生成的区块坐标必在世界内");
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
}

/// 一列：基岩、石头、泥土与表层（草地或沙）。
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

/// 洞穴雕刻：只挖石头，只在地壳以下，保留基岩垫层。
fn carve_caves(world: &mut VoxelWorld, seed: u64) {
    for z in 0..WORLD_SIZE as i32 {
        for x in 0..WORLD_SIZE as i32 {
            let height = column_height(x, z, seed) as i32;
            let ceiling = height - CAVE_CRUST_BELOW_SURFACE - 1;
            for y in CAVE_MIN_Y..ceiling {
                if !cave_noise(x, y, z, seed) {
                    continue;
                }
                let pos = IVec3::new(x, y, z);
                if world.block(pos) == Block::Stone {
                    put(world, pos, Block::Air);
                }
            }
        }
    }
}

/// 洞口坡道：从地表沿切线方向下行，前段是露天切口，之后是四格高的
/// 有顶隧道；触底后继续向前挖，直到撞上既有洞穴空气。
fn carve_entrance(world: &mut VoxelWorld, seed: u64, site: EntranceSite) {
    let mouth_height = column_height(site.mouth_x, site.mouth_z, seed) as i32;
    let perpendicular = (-site.dir_z, site.dir_x);
    let mut floor_y = mouth_height;
    let mut step = 0;
    while step < ENTRANCE_MAX_STEPS {
        let center_x = site.mouth_x + site.dir_x * step;
        let center_z = site.mouth_z + site.dir_z * step;
        if !clear_of_world_edge(center_x, center_z) {
            break;
        }
        floor_y = (mouth_height - step / 2).max(ENTRANCE_FLOOR_Y);
        let top = if step < ENTRANCE_OPEN_STEPS {
            mouth_height + 2
        } else {
            floor_y + 4
        };
        for y in floor_y..=top {
            for offset in -1..=1 {
                let x = center_x + perpendicular.0 * offset;
                let z = center_z + perpendicular.1 * offset;
                put(world, IVec3::new(x, y, z), Block::Air);
            }
        }
        step += 1;
        if floor_y == ENTRANCE_FLOOR_Y {
            break;
        }
    }
    if floor_y > ENTRANCE_FLOOR_Y {
        return;
    }

    // 触底后向前探洞：每一步先看截面上是否已有洞穴空气，挖穿后立即停。
    let end_x = site.mouth_x + site.dir_x * (step - 1);
    let end_z = site.mouth_z + site.dir_z * (step - 1);
    for probe in 1..=ENTRANCE_PROBE_STEPS {
        let center_x = end_x + site.dir_x * probe;
        let center_z = end_z + site.dir_z * probe;
        if !clear_of_world_edge(center_x, center_z) {
            break;
        }
        let touched_cave = (-1..=1).any(|offset| {
            let x = center_x + perpendicular.0 * offset;
            let z = center_z + perpendicular.1 * offset;
            (0..3).any(|dy| world.block(IVec3::new(x, ENTRANCE_FLOOR_Y + dy, z)) == Block::Air)
        });
        for y in ENTRANCE_FLOOR_Y..(ENTRANCE_FLOOR_Y + 3) {
            for offset in -1..=1 {
                let x = center_x + perpendicular.0 * offset;
                let z = center_z + perpendicular.1 * offset;
                let pos = IVec3::new(x, y, z);
                if world.block(pos).is_solid() {
                    put(world, pos, Block::Air);
                }
            }
        }
        if touched_cave {
            break;
        }
    }
}

/// 三条坡道都要留出三格边缘，避免把世界边界墙挖穿。
fn clear_of_world_edge(x: i32, z: i32) -> bool {
    x >= 3 && z >= 3 && (x as usize) < WORLD_SIZE - 3 && (z as usize) < WORLD_SIZE - 3
}

/// 该列是否落在洞口坡道的走廊内（含 `margin` 格余量）。
///
/// 树木与地壳检查都要避开走廊：坡道是刻意贯穿地壳的通道，走廊内的
/// 地表本就应该被切开。
pub fn near_entrance_path(sites: &[EntranceSite], x: i32, z: i32, margin: i32) -> bool {
    sites.iter().any(|site| {
        (0..ENTRANCE_MAX_STEPS).any(|step| {
            let center_x = site.mouth_x + site.dir_x * step;
            let center_z = site.mouth_z + site.dir_z * step;
            (x - center_x).abs() <= margin && (z - center_z).abs() <= margin
        })
    })
}

/// 一个矿脉规则：方块种类、深度窗口、每区块尝试次数与脉长。
struct OreRule {
    block: Block,
    min_y: i32,
    max_y: i32,
    attempts: u32,
    length: u32,
    salt: u64,
}

const ORE_RULES: [OreRule; 3] = [
    OreRule {
        block: Block::CoalOre,
        min_y: 5,
        max_y: 20,
        attempts: 7,
        length: 5,
        salt: 0x00C0_4A11_9E37_0001,
    },
    OreRule {
        block: Block::IronOre,
        min_y: 3,
        max_y: 14,
        attempts: 5,
        length: 4,
        salt: 0x00C0_4A11_9E37_0002,
    },
    OreRule {
        block: Block::GoldOre,
        min_y: 2,
        max_y: 9,
        attempts: 2,
        length: 3,
        salt: 0x00C0_4A11_9E37_0003,
    },
];

/// 六个随机游走方向，顺序固定。
const WALK_DIRECTIONS: [(i32, i32, i32); 6] = [
    (1, 0, 0),
    (-1, 0, 0),
    (0, 1, 0),
    (0, -1, 0),
    (0, 0, 1),
    (0, 0, -1),
];

/// 按区块生成矿脉。短随机游走只替换石头，脉体不会伸进洞穴空气。
fn place_ore_veins(world: &mut VoxelWorld, seed: u64) {
    for cz in 0..WORLD_CHUNKS as i32 {
        for cx in 0..WORLD_CHUNKS as i32 {
            for rule in &ORE_RULES {
                for attempt in 0..rule.attempts {
                    place_vein(world, seed, cx, cz, rule, attempt as u64);
                }
            }
        }
    }
}

fn place_vein(world: &mut VoxelWorld, seed: u64, cx: i32, cz: i32, rule: &OreRule, attempt: u64) {
    let mut counter = 0u64;
    let mut next = || {
        let value = ore_stream(seed, cx, cz, rule.salt, attempt, counter);
        counter += 1;
        value
    };
    let margin = (CHUNK_SIZE - 1) as i32;
    let mut x = cx * CHUNK_SIZE as i32 + (next() % CHUNK_SIZE as u64) as i32;
    let mut z = cz * CHUNK_SIZE as i32 + (next() % CHUNK_SIZE as u64) as i32;
    let span = (rule.max_y - rule.min_y + 1) as u64;
    let mut y = rule.min_y + (next() % span) as i32;

    for _ in 0..rule.length {
        if world.block(IVec3::new(x, y, z)) == Block::Stone {
            put(world, IVec3::new(x, y, z), rule.block);
        }
        let (dx, dy, dz) = WALK_DIRECTIONS[(next() % 6) as usize];
        let candidate_x = (x + dx).clamp(cx * CHUNK_SIZE as i32, cx * CHUNK_SIZE as i32 + margin);
        let candidate_z = (z + dz).clamp(cz * CHUNK_SIZE as i32, cz * CHUNK_SIZE as i32 + margin);
        let candidate_y = (y + dy).clamp(rule.min_y, rule.max_y);
        x = candidate_x;
        y = candidate_y;
        z = candidate_z;
    }
}

/// 计数器式哈希流：调用序即随机流位置，不依赖任何全局状态。
fn ore_stream(seed: u64, cx: i32, cz: i32, salt: u64, attempt: u64, counter: u64) -> u64 {
    mix64(
        seed ^ (cx as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (cz as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
            ^ salt
            ^ attempt.wrapping_mul(0xD1B5_4A32_D192_ED03)
            ^ counter.wrapping_mul(0x1656_67B1_9E37_79F9),
    )
}

/// 在草地上种树。候选按固定扫描序产生，间距检查保证结果确定。
fn plant_trees(world: &mut VoxelWorld, seed: u64, sites: &[EntranceSite]) {
    let mut bases: Vec<IVec3> = Vec::new();
    for z in 4..(WORLD_SIZE as i32 - 4) {
        for x in 4..(WORLD_SIZE as i32 - 4) {
            let distance = (x - CENTER).abs().max((z - CENTER).abs());
            if distance <= TREE_CLEAR_RADIUS {
                continue;
            }
            // 树冠半径两格，树干离坡道中心三格以内就会把树叶伸进走廊。
            if near_entrance_path(sites, x, z, 3) {
                continue;
            }
            let key = hash2(x, z, seed ^ 0x7A11_57EE_0000_0001);
            if key % 1000 >= TREE_PER_MILLE {
                continue;
            }
            let height = column_height(x, z, seed) as i32;
            if !(8..=24).contains(&height) {
                continue;
            }
            let base = IVec3::new(x, height, z);
            if world.block(base) != Block::Grass {
                continue;
            }
            let trunk_height = 4 + ((key >> 32) % 3) as i32;
            let clear =
                (1..=(trunk_height + 2)).all(|dy| world.block(base + IVec3::Y * dy) == Block::Air);
            if !clear {
                continue;
            }
            if bases.iter().any(|other| {
                (other.x - x).abs() < TREE_SPACING && (other.z - z).abs() < TREE_SPACING
            }) {
                continue;
            }
            bases.push(base);
            place_tree(world, base, trunk_height);
        }
    }
}

/// 一棵橡树：原木树干、两层带缺角的五格冠、两层三格冠与顶端十字。
fn place_tree(world: &mut VoxelWorld, base: IVec3, trunk_height: i32) {
    for dy in 1..=trunk_height {
        let pos = base + IVec3::Y * dy;
        if world.block(pos) == Block::Air {
            put(world, pos, Block::Wood);
        }
    }
    for dy in (trunk_height - 3)..=(trunk_height + 1) {
        let radius: i32 = if dy <= trunk_height - 2 {
            2
        } else if dy <= trunk_height {
            1
        } else {
            0
        };
        for dx in -radius..=radius {
            for dz in -radius..=radius {
                if radius == 2 && dx.abs() == 2 && dz.abs() == 2 {
                    continue;
                }
                let pos = base + IVec3::new(dx, dy, dz);
                if world.block(pos) == Block::Air {
                    put(world, pos, Block::Leaves);
                }
            }
        }
    }
}

/// 洞穴荧光石：每个区块按哈希概率找一处洞穴地板，把地板方块换成
/// 荧光石。它是地下唯一的光源，也是光照系统的验收对象。
///
/// 单列可能整列都没有洞穴，因此每区块最多尝试八列；找到第一处合适的
/// 地板就停止。
fn place_cave_glowstone(world: &mut VoxelWorld, seed: u64) {
    const ATTEMPTS: u64 = 8;
    for cz in 0..WORLD_CHUNKS as i32 {
        for cx in 0..WORLD_CHUNKS as i32 {
            let gate = ore_stream(seed, cx, cz, 0x6100_5700_0000_0001, 0, 0);
            if gate % 100 >= 70 {
                continue;
            }
            for attempt in 0..ATTEMPTS {
                let key = ore_stream(seed, cx, cz, 0x6100_5700_0000_0002, attempt, 0);
                let x = cx * CHUNK_SIZE as i32 + (key % CHUNK_SIZE as u64) as i32;
                let z = cz * CHUNK_SIZE as i32 + ((key >> 8) % CHUNK_SIZE as u64) as i32;
                let height = column_height(x, z, seed) as i32;
                let ceiling = height - CAVE_CRUST_BELOW_SURFACE - 1;
                let start_y = (CAVE_MIN_Y + ((key >> 16) % 9) as i32).min(ceiling - 1);
                let mut placed = false;
                for y in (CAVE_MIN_Y..=start_y).rev() {
                    let pos = IVec3::new(x, y, z);
                    if world.block(pos) != Block::Air || world.block(pos + IVec3::Y) != Block::Air {
                        continue;
                    }
                    let floor = pos - IVec3::Y;
                    if matches!(world.block(floor), Block::Stone | Block::Dirt) {
                        put(world, floor, Block::Glowstone);
                        placed = true;
                    }
                    break;
                }
                if placed {
                    break;
                }
            }
        }
    }
}

/// 直接写区块数组；生成阶段不维护光照与脏列表，光照最后统一重算。
fn put(world: &mut VoxelWorld, pos: IVec3, block: Block) {
    if !VoxelWorld::in_bounds(pos) {
        return;
    }
    let coord = VoxelWorld::chunk_of(pos);
    if let Some(chunk) = world.chunk_mut(coord) {
        chunk.set(
            pos.x.rem_euclid(CHUNK_SIZE as i32) as usize,
            pos.y as usize,
            pos.z.rem_euclid(CHUNK_SIZE as i32) as usize,
            block,
        );
    }
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
        for dz in -PLATEAU_FLAT_RADIUS..=PLATEAU_FLAT_RADIUS {
            for dx in -PLATEAU_FLAT_RADIUS..=PLATEAU_FLAT_RADIUS {
                let height = column_height(CENTER + dx, CENTER + dz, 7);
                assert_eq!(height, PLATEAU_HEIGHT);
                assert_eq!(
                    world.surface_height(CENTER + dx, CENTER + dz),
                    Some(PLATEAU_HEIGHT)
                );
            }
        }
        let spawn = SPAWN_BLOCK;
        assert_eq!(
            world.block(IVec3::new(spawn[0], spawn[1], spawn[2])),
            Block::Grass
        );
        assert_eq!(
            world.block(IVec3::new(spawn[0], spawn[1] + 1, spawn[2])),
            Block::Air
        );
        assert!(spawn_position().y > PLATEAU_HEIGHT as f32 + 1.0);
    }

    /// 洞穴不得破坏地表：每列表面块与其下三格都是实心，基岩垫层完整。
    #[test]
    fn caves_keep_the_surface_crust() {
        let seed = 99;
        let world = VoxelWorld::generate(seed);
        let sites = entrance_sites(seed);
        for z in (0..WORLD_SIZE as i32).step_by(7) {
            for x in (0..WORLD_SIZE as i32).step_by(11) {
                // 洞口走廊是刻意切穿地壳的通道，不计入地壳完整性检查。
                if near_entrance_path(&sites, x, z, 2) {
                    continue;
                }
                let height = column_height(x, z, seed) as i32;
                for dy in 0..=CAVE_CRUST_BELOW_SURFACE {
                    assert!(
                        world.block(IVec3::new(x, height - dy, z)).is_solid(),
                        "列 ({x},{z}) 的地壳在 y={} 被挖穿",
                        height - dy
                    );
                }
                assert_eq!(world.block(IVec3::new(x, 0, z)), Block::Bedrock);
                assert!(world.block(IVec3::new(x, 1, z)).is_solid());
            }
        }
    }

    /// 洞穴在石头层内产生可见体积，且存在至少三格高的连续空腔。
    #[test]
    fn caves_carve_visible_volume() {
        let seed = 20_261_022;
        let world = VoxelWorld::generate(seed);
        let mut underground = 0u32;
        let mut carved = 0u32;
        let mut tall_cavity = false;
        for z in 0..WORLD_SIZE as i32 {
            for x in 0..WORLD_SIZE as i32 {
                let height = column_height(x, z, seed) as i32;
                for y in CAVE_MIN_Y..(height - CAVE_CRUST_BELOW_SURFACE - 1) {
                    underground += 1;
                    if world.block(IVec3::new(x, y, z)) == Block::Air {
                        carved += 1;
                        if y > CAVE_MIN_Y
                            && world.block(IVec3::new(x, y - 1, z)) == Block::Air
                            && world.block(IVec3::new(x, y - 2, z)) == Block::Air
                        {
                            tall_cavity = true;
                        }
                    }
                }
            }
        }
        let share = carved as f32 / underground as f32;
        assert!(
            (0.02..=0.25).contains(&share),
            "洞穴体积占比应可见但不过量，实际 {share}"
        );
        assert!(tall_cavity, "应存在三格高的洞厅");
    }

    /// 三条坡道从地表连续通到洞穴层，中心线上每一步都能站人。
    #[test]
    fn entrances_reach_the_cave_layer() {
        let seed = 20_261_022;
        let world = VoxelWorld::generate(seed);
        let sites = entrance_sites(seed);
        assert_eq!(sites.len(), ENTRANCE_COUNT, "应选出三个洞口");
        for site in sites {
            let mouth_height = column_height(site.mouth_x, site.mouth_z, seed) as i32;
            assert!(
                mouth_height >= ENTRANCE_FLOOR_Y + 4,
                "洞口应有足够下降空间，实际 {mouth_height}"
            );
            let mut reached_bottom = false;
            for step in 0..ENTRANCE_MAX_STEPS {
                let floor_y = (mouth_height - step / 2).max(ENTRANCE_FLOOR_Y);
                let x = site.mouth_x + site.dir_x * step;
                let z = site.mouth_z + site.dir_z * step;
                for dy in 1..=2 {
                    assert_eq!(
                        world.block(IVec3::new(x, floor_y + dy, z)),
                        Block::Air,
                        "坡道 ({x},{z}) 在 y={} 被堵住",
                        floor_y + dy
                    );
                }
                if floor_y == ENTRANCE_FLOOR_Y {
                    reached_bottom = true;
                    break;
                }
            }
            assert!(reached_bottom, "坡道应下降到 y={ENTRANCE_FLOOR_Y}");
        }
    }

    /// 矿脉只出现在各自深度窗口内，块数合理，且至少一侧仍是岩石。
    #[test]
    fn ore_veins_respect_depth_windows() {
        let world = VoxelWorld::generate(20_261_022);
        let mut coal = 0;
        let mut iron = 0;
        let mut gold = 0;
        let mut exposed = 0;
        for z in 0..WORLD_SIZE as i32 {
            for x in 0..WORLD_SIZE as i32 {
                for y in 0..CHUNK_HEIGHT as i32 {
                    let pos = IVec3::new(x, y, z);
                    let block = world.block(pos);
                    let (min, max) = match block {
                        Block::CoalOre => {
                            coal += 1;
                            (5, 20)
                        }
                        Block::IronOre => {
                            iron += 1;
                            (3, 14)
                        }
                        Block::GoldOre => {
                            gold += 1;
                            (2, 9)
                        }
                        _ => continue,
                    };
                    assert!((min..=max).contains(&y), "{block:?} 出现在窗口外 y={y}");
                    let embedded = crate::logic::block::Face::ALL.iter().any(|face| {
                        let n = face.normal();
                        matches!(
                            world.block(pos + IVec3::new(n[0], n[1], n[2])),
                            Block::Stone | Block::CoalOre | Block::IronOre | Block::GoldOre
                        )
                    });
                    assert!(embedded, "{block:?} 在 {pos:?} 悬空");
                    if crate::logic::block::Face::ALL.iter().any(|face| {
                        let n = face.normal();
                        world.block(pos + IVec3::new(n[0], n[1], n[2])) == Block::Air
                    }) {
                        exposed += 1;
                    }
                }
            }
        }
        assert!(coal > 200, "煤矿应成规模出现，实际 {coal}");
        assert!(iron > 100, "铁矿应成规模出现，实际 {iron}");
        assert!(gold > 20, "金矿应出现，实际 {gold}");
        assert!(exposed > 20, "洞穴壁上应露出矿石，实际 {exposed}");
    }

    /// 每个树干都立在草地上，冠层至少覆盖一层五格宽。
    #[test]
    fn trees_stand_on_grass_with_canopies() {
        let seed = 20_261_022;
        let world = VoxelWorld::generate(seed);
        let mut trunks = 0;
        for z in 0..WORLD_SIZE as i32 {
            for x in 0..WORLD_SIZE as i32 {
                for y in 1..CHUNK_HEIGHT as i32 {
                    let pos = IVec3::new(x, y, z);
                    if world.block(pos) != Block::Wood {
                        continue;
                    }
                    if world.block(pos - IVec3::Y) == Block::Wood {
                        continue;
                    }
                    trunks += 1;
                    assert_eq!(
                        world.block(pos - IVec3::Y),
                        Block::Grass,
                        "树干底部 {pos:?} 应立在草地上"
                    );
                    let mut canopy = 0;
                    for dx in -2..=2 {
                        for dz in -2..=2 {
                            for dy in 1..=6 {
                                if world.block(pos + IVec3::new(dx, dy, dz)) == Block::Leaves {
                                    canopy += 1;
                                }
                            }
                        }
                    }
                    assert!(canopy >= 12, "树干 {pos:?} 的冠层只有 {canopy} 片树叶");
                }
            }
        }
        assert!(trunks >= 20, "世界应有成规模的树林，实际 {trunks} 棵");
    }

    /// 树木避开出生点，彼此保持间距。
    #[test]
    fn trees_keep_clear_of_spawn_and_each_other() {
        let seed = 20_261_022;
        let world = VoxelWorld::generate(seed);
        let sites = entrance_sites(seed);
        let mut bases = Vec::new();
        for z in 0..WORLD_SIZE as i32 {
            for x in 0..WORLD_SIZE as i32 {
                for y in 1..CHUNK_HEIGHT as i32 {
                    let pos = IVec3::new(x, y, z);
                    if world.block(pos) == Block::Wood && world.block(pos - IVec3::Y) != Block::Wood
                    {
                        bases.push(pos);
                    }
                }
            }
        }
        assert!(bases.len() >= 20);
        for base in &bases {
            let distance = (base.x - CENTER).abs().max((base.z - CENTER).abs());
            assert!(
                distance > TREE_CLEAR_RADIUS,
                "树 {base:?} 落在出生点净空半径内"
            );
            assert!(
                !near_entrance_path(&sites, base.x, base.z, 3),
                "树 {base:?} 的树冠会伸进洞口走廊"
            );
        }
        for (index, a) in bases.iter().enumerate() {
            for b in &bases[index + 1..] {
                assert!(
                    (a.x - b.x).abs() >= TREE_SPACING || (a.z - b.z).abs() >= TREE_SPACING,
                    "树 {a:?} 与 {b:?} 间距不足"
                );
            }
        }
    }

    /// 洞穴荧光石存在，且确实照亮周围的洞穴空气。
    #[test]
    fn cave_glowstone_lights_the_underground() {
        let world = VoxelWorld::generate(20_261_022);
        let mut clusters = 0;
        for z in 0..WORLD_SIZE as i32 {
            for x in 0..WORLD_SIZE as i32 {
                for y in 1..CHUNK_HEIGHT as i32 {
                    let pos = IVec3::new(x, y, z);
                    if world.block(pos) != Block::Glowstone {
                        continue;
                    }
                    clusters += 1;
                    let mut lit = 0;
                    for offset in [IVec3::X, -IVec3::X, IVec3::Y, IVec3::Z, -IVec3::Z] {
                        let neighbour = pos + offset;
                        if world.block(neighbour) == Block::Air
                            && world.light().block_light(neighbour) >= 13
                        {
                            lit += 1;
                        }
                    }
                    assert!(lit >= 1, "荧光石 {pos:?} 没有照亮任何相邻空气格");
                }
            }
        }
        assert!(clusters >= 15, "洞穴荧光石太少：{clusters}");
    }

    /// 地下洞穴在生成后没有天空光。
    #[test]
    fn caves_are_dark_below_the_surface() {
        let seed = 20_261_022;
        let world = VoxelWorld::generate(seed);
        let mut dark_cells = 0;
        for z in (0..WORLD_SIZE as i32).step_by(3) {
            for x in (0..WORLD_SIZE as i32).step_by(3) {
                let height = column_height(x, z, seed) as i32;
                for y in CAVE_MIN_Y..(height - CAVE_CRUST_BELOW_SURFACE - 3) {
                    let pos = IVec3::new(x, y, z);
                    if world.block(pos) == Block::Air && world.light().sky(pos) == 0 {
                        dark_cells += 1;
                    }
                }
            }
        }
        assert!(dark_cells > 100, "地下应有大量零天空光的洞穴格");
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
        let outer = CENTER + PLATEAU_BLEND_RADIUS;
        assert_eq!(
            column_height(outer, CENTER, seed),
            natural_height(outer, CENTER, seed).round() as usize
        );
        let inner = column_height(CENTER + PLATEAU_FLAT_RADIUS + 1, CENTER, seed);
        assert!((inner as i32 - PLATEAU_HEIGHT as i32).abs() <= 6);
    }

    /// 二维与三维噪声都在 [0, 1) 内，且对相同坐标稳定。
    #[test]
    fn value_noise_is_bounded_and_stable() {
        for i in 0..64 {
            let x = i as f32 * 0.37 - 7.0;
            let z = i as f32 * -0.23 + 3.0;
            let value = value_noise(x, z, 42);
            assert!((0.0..1.0).contains(&value), "噪声越界：{value}");
            assert_eq!(value.to_bits(), value_noise(x, z, 42).to_bits());

            let y = i as f32 * 0.19 - 4.0;
            let value = value_noise3(x, y, z, 43);
            assert!((0.0..1.0).contains(&value), "三维噪声越界：{value}");
            assert_eq!(value.to_bits(), value_noise3(x, y, z, 43).to_bits());
        }
    }

    /// 洞穴噪声只在地壳以下生效的约束由生成流程保证，这里只验证
    /// 同坐标的噪声判断可复现。
    #[test]
    fn cave_noise_is_stable() {
        for i in 0..32 {
            let x = i * 3 - 40;
            let y = i % 20;
            let z = i * 5 - 60;
            assert_eq!(cave_noise(x, y, z, 7), cave_noise(x, y, z, 7));
        }
    }
}
