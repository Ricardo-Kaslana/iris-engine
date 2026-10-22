//! 体素光照网格：天空光与方块光的离散传播。
//!
//! 天空光与方块光各存一份 0..=15 的整世界数组，下标与方块数组一致。
//! 天空光从所有直接露天的空气格以 15 起步，方块光从发光方块以自身
//! 发光等级起步，两者都沿六个轴向每步衰减 1。实心方块视为不透明，
//! 光照不会进入；发光方块自身格保留光源等级，它的光从邻格开始传播。
//!
//! **天空光不变量**：因为衰减每步至少为 1，空气格的天空光等于 15
//! 当且仅当它当前直接露天。增量更新依赖这条性质：在露天柱中放下一块
//! 不透明方块时，整根柱内原本为 15 的空气格必须显式失效，不能只从
//! 编辑格按每步减 1 传递；反之挖开遮挡后，重新露天的整段空气柱要
//! 直接置为 15。
//!
//! 已知限制：不做半透明衰减（树叶、玻璃、水都按实心不透明处理），
//! 不做彩色光源与方向性光照，也没有逐像素光照贴图；光照等级最终只能
//! 烘进网格顶点色，光场每变化一格就要重建受影响的区块网格。

use std::collections::{BTreeSet, VecDeque};

use glam::IVec3;

use super::block::Block;
use super::world::{CHUNK_HEIGHT, CHUNK_SIZE, Chunk, WORLD_CHUNKS, WORLD_SIZE};

pub const MAX_LIGHT: u8 = 15;

const CELL_COUNT: usize = WORLD_SIZE * WORLD_SIZE * CHUNK_HEIGHT;

/// 固定邻居顺序；最终光场与顺序无关，固定顺序保证过程可复现。
const NEIGHBOURS: [IVec3; 6] = [
    IVec3::new(1, 0, 0),
    IVec3::new(-1, 0, 0),
    IVec3::new(0, 1, 0),
    IVec3::new(0, -1, 0),
    IVec3::new(0, 0, 1),
    IVec3::new(0, 0, -1),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Channel {
    Sky,
    Block,
}

/// 整世界天空光与方块光。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LightGrid {
    sky: Vec<u8>,
    block: Vec<u8>,
}

impl LightGrid {
    /// 全空气世界的天空光：每一格都直接露天。
    pub fn fully_open() -> Self {
        Self {
            sky: vec![MAX_LIGHT; CELL_COUNT],
            block: vec![0; CELL_COUNT],
        }
    }

    /// 从方块数据全量重算光照。作为增量更新的参考实现与启动入口。
    pub fn compute(chunks: &[Chunk]) -> Self {
        let mut grid = Self {
            sky: vec![0; CELL_COUNT],
            block: vec![0; CELL_COUNT],
        };
        let mut touched = BTreeSet::new();
        let mut sky_queue = VecDeque::new();
        let mut block_queue = VecDeque::new();

        for z in 0..WORLD_SIZE {
            for x in 0..WORLD_SIZE {
                for y in (0..CHUNK_HEIGHT).rev() {
                    let pos = IVec3::new(x as i32, y as i32, z as i32);
                    if block_at(chunks, pos).is_solid() {
                        break;
                    }
                    grid.set(Channel::Sky, pos, MAX_LIGHT);
                    sky_queue.push_back((pos, MAX_LIGHT));
                }
            }
        }

        for z in 0..WORLD_SIZE {
            for x in 0..WORLD_SIZE {
                for y in 0..CHUNK_HEIGHT {
                    let pos = IVec3::new(x as i32, y as i32, z as i32);
                    let emission = block_at(chunks, pos).emission();
                    if emission > 0 {
                        grid.set(Channel::Block, pos, emission);
                        block_queue.push_back((pos, emission));
                    }
                }
            }
        }

        grid.add_light(chunks, Channel::Sky, &mut sky_queue, &mut touched);
        grid.add_light(chunks, Channel::Block, &mut block_queue, &mut touched);
        grid
    }

    /// 天空光等级。世界顶部以上视为全露天，其余界外为 0。
    pub fn sky(&self, pos: IVec3) -> u8 {
        if !in_bounds(pos) {
            return if pos.y >= CHUNK_HEIGHT as i32 {
                MAX_LIGHT
            } else {
                0
            };
        }
        self.sky[index(pos)]
    }

    /// 方块光等级；界外为 0。
    pub fn block_light(&self, pos: IVec3) -> u8 {
        if !in_bounds(pos) {
            return 0;
        }
        self.block[index(pos)]
    }

    /// 天空光原始数组（确定性测试用）。
    pub fn raw_sky(&self) -> &[u8] {
        &self.sky
    }

    /// 方块光原始数组（确定性测试用）。
    pub fn raw_block(&self) -> &[u8] {
        &self.block
    }

    /// 一次方块写入后的增量光照更新，返回光照变化波及的区块坐标。
    ///
    /// `old_sky` 与 `old_block_light` 是写入前编辑格的光值。调用时方块
    /// 数据必须已经写入 `chunks`，后续判断实心性读的是新方块。
    pub fn apply_edit(
        &mut self,
        chunks: &[Chunk],
        pos: IVec3,
        new_block: Block,
        old_sky: u8,
        old_block_light: u8,
    ) -> BTreeSet<(i32, i32)> {
        let mut touched = BTreeSet::new();
        let mut add_sky = VecDeque::new();
        let mut add_block = VecDeque::new();

        if old_sky > 0 {
            let mut remove = VecDeque::new();
            self.set(Channel::Sky, pos, 0);
            mark_chunk(&mut touched, pos);
            remove.push_back((pos, old_sky));

            // 露天柱被新方块截断：柱内所有 sky == 15 的格都失去了直接
            // 露天条件，必须一起失效；按每步减 1 传播会让它们残留为
            // 15、14、13……而不是正确的地下 0 或侧向回填值。
            if new_block.is_solid() {
                let mut below = pos - IVec3::Y;
                while in_bounds(below)
                    && !block_at(chunks, below).is_solid()
                    && self.sky(below) == MAX_LIGHT
                {
                    self.set(Channel::Sky, below, 0);
                    mark_chunk(&mut touched, below);
                    remove.push_back((below, MAX_LIGHT));
                    below -= IVec3::Y;
                }
            }
            self.remove_light(Channel::Sky, &mut remove, &mut add_sky, &mut touched);
        }

        if old_block_light > 0 {
            let mut remove = VecDeque::new();
            self.set(Channel::Block, pos, 0);
            mark_chunk(&mut touched, pos);
            remove.push_back((pos, old_block_light));
            self.remove_light(Channel::Block, &mut remove, &mut add_block, &mut touched);
        }

        if new_block.emission() > 0 {
            self.set(Channel::Block, pos, new_block.emission());
            mark_chunk(&mut touched, pos);
            add_block.push_back((pos, new_block.emission()));
        }

        if !new_block.is_solid() {
            if self.sky_exposed(chunks, pos) {
                // 重新露天的整根空气柱直接恢复满值，不能靠逐格减 1
                // 从编辑格往下传，否则会得到 14、13……的假渐变。
                let mut cell = pos;
                while in_bounds(cell) && !block_at(chunks, cell).is_solid() {
                    if self.sky(cell) != MAX_LIGHT {
                        self.set(Channel::Sky, cell, MAX_LIGHT);
                        mark_chunk(&mut touched, cell);
                        add_sky.push_back((cell, MAX_LIGHT));
                    }
                    cell -= IVec3::Y;
                }
            } else {
                for offset in NEIGHBOURS {
                    let neighbour = pos + offset;
                    if in_bounds(neighbour) {
                        let level = self.sky(neighbour);
                        if level > 1 {
                            add_sky.push_back((neighbour, level));
                        }
                    }
                }
            }
            for offset in NEIGHBOURS {
                let neighbour = pos + offset;
                if in_bounds(neighbour) {
                    let level = self.block_light(neighbour);
                    if level > 1 {
                        add_block.push_back((neighbour, level));
                    }
                }
            }
        }

        self.add_light(chunks, Channel::Sky, &mut add_sky, &mut touched);
        self.add_light(chunks, Channel::Block, &mut add_block, &mut touched);
        touched
    }

    fn get(&self, channel: Channel, pos: IVec3) -> u8 {
        match channel {
            Channel::Sky => self.sky[index(pos)],
            Channel::Block => self.block[index(pos)],
        }
    }

    fn set(&mut self, channel: Channel, pos: IVec3, value: u8) {
        let slot = match channel {
            Channel::Sky => &mut self.sky[index(pos)],
            Channel::Block => &mut self.block[index(pos)],
        };
        *slot = value;
    }

    /// 移除 BFS：队列中的位置必须已经被清 0，值为移除前的等级。
    /// 光值不低于当前等级的邻格作为回填种子，交给添加阶段。
    fn remove_light(
        &mut self,
        channel: Channel,
        remove: &mut VecDeque<(IVec3, u8)>,
        re_add: &mut VecDeque<(IVec3, u8)>,
        touched: &mut BTreeSet<(i32, i32)>,
    ) {
        while let Some((pos, level)) = remove.pop_front() {
            for offset in NEIGHBOURS {
                let neighbour = pos + offset;
                if !in_bounds(neighbour) {
                    continue;
                }
                let neighbour_level = self.get(channel, neighbour);
                if neighbour_level == 0 {
                    continue;
                }
                if neighbour_level < level {
                    self.set(channel, neighbour, 0);
                    mark_chunk(touched, neighbour);
                    remove.push_back((neighbour, neighbour_level));
                } else {
                    re_add.push_back((neighbour, neighbour_level));
                }
            }
        }
    }

    /// 添加 BFS：只写入空气格，目标等级为源等级减 1，严格变大才写入。
    fn add_light(
        &mut self,
        chunks: &[Chunk],
        channel: Channel,
        add: &mut VecDeque<(IVec3, u8)>,
        touched: &mut BTreeSet<(i32, i32)>,
    ) {
        while let Some((pos, level)) = add.pop_front() {
            if level <= 1 {
                continue;
            }
            for offset in NEIGHBOURS {
                let neighbour = pos + offset;
                if !in_bounds(neighbour) {
                    continue;
                }
                if block_at(chunks, neighbour).is_solid() {
                    continue;
                }
                let target = level - 1;
                if self.get(channel, neighbour) < target {
                    self.set(channel, neighbour, target);
                    mark_chunk(touched, neighbour);
                    add.push_back((neighbour, target));
                }
            }
        }
    }

    /// 编辑格上方是否整列无实心方块。
    fn sky_exposed(&self, chunks: &[Chunk], pos: IVec3) -> bool {
        ((pos.y + 1)..CHUNK_HEIGHT as i32)
            .all(|y| !block_at(chunks, IVec3::new(pos.x, y, pos.z)).is_solid())
    }
}

fn in_bounds(pos: IVec3) -> bool {
    pos.x >= 0
        && pos.y >= 0
        && pos.z >= 0
        && (pos.x as usize) < WORLD_SIZE
        && (pos.y as usize) < CHUNK_HEIGHT
        && (pos.z as usize) < WORLD_SIZE
}

fn index(pos: IVec3) -> usize {
    ((pos.y as usize * WORLD_SIZE + pos.z as usize) * WORLD_SIZE) + pos.x as usize
}

fn block_at(chunks: &[Chunk], pos: IVec3) -> Block {
    if !in_bounds(pos) {
        return Block::Air;
    }
    let cx = pos.x as usize / CHUNK_SIZE;
    let cz = pos.z as usize / CHUNK_SIZE;
    chunks[cz * WORLD_CHUNKS + cx].get(
        pos.x as usize % CHUNK_SIZE,
        pos.y as usize,
        pos.z as usize % CHUNK_SIZE,
    )
}

/// 光照变化影响到的区块。格在区块边界层时，平滑光照与 AO 会采样到
/// 相邻区块，因此同时标记该方向的邻区块（角点最多 2×2 个区块）。
fn mark_chunk(touched: &mut BTreeSet<(i32, i32)>, pos: IVec3) {
    let cx = pos.x.div_euclid(CHUNK_SIZE as i32);
    let cz = pos.z.div_euclid(CHUNK_SIZE as i32);
    let local_x = pos.x.rem_euclid(CHUNK_SIZE as i32);
    let local_z = pos.z.rem_euclid(CHUNK_SIZE as i32);
    let x_lo = if local_x == 0 { cx - 1 } else { cx };
    let x_hi = if local_x as usize == CHUNK_SIZE - 1 {
        cx + 1
    } else {
        cx
    };
    let z_lo = if local_z == 0 { cz - 1 } else { cz };
    let z_hi = if local_z as usize == CHUNK_SIZE - 1 {
        cz + 1
    } else {
        cz
    };
    for x in x_lo..=x_hi {
        for z in z_lo..=z_hi {
            if x >= 0 && z >= 0 && (x as usize) < WORLD_CHUNKS && (z as usize) < WORLD_CHUNKS {
                touched.insert((x, z));
            }
        }
    }
}

/// 天空光曲线：低等级更暗，接近平方衰减。查表避免浮点差异。
const SKY_CURVE: [u8; 16] = [
    0, 11, 23, 36, 50, 65, 81, 99, 118, 138, 160, 183, 208, 230, 245, 255,
];
/// 方块光曲线，比天空光略饱满，让火把周边更暖更亮。
const BLOCK_CURVE: [u8; 16] = [
    0, 13, 27, 42, 58, 75, 93, 112, 132, 153, 175, 198, 220, 238, 249, 255,
];
const AMBIENT: [u16; 3] = [24, 26, 31];
const SKY_TINT: [u16; 3] = [225, 238, 255];
const BLOCK_TINT: [u16; 3] = [255, 196, 130];
/// AO 等级 0..3 对应的亮度系数（0 最暗）。
const AO_SCALE: [u16; 4] = [140, 180, 218, 255];

/// 把天空光、方块光、逐面明暗与 AO 组合为 8 位顶点色。
///
/// 全部使用整数运算：先查表得到冷色天空光与暖色方块光，加固定环境
/// 底光后按通道钳制到 255，再乘逐面明暗与 AO 系数。输出是线性空间的
/// 颜色分量，交给 `VertexColorMode::Multiply` 与 sRGB 纹理相乘。
pub fn vertex_rgb(sky: u8, block: u8, shade: u8, ao: u8) -> [u8; 3] {
    let sky_level = SKY_CURVE[sky.min(MAX_LIGHT) as usize] as u16;
    let block_level = BLOCK_CURVE[block.min(MAX_LIGHT) as usize] as u16;
    let ao_scale = AO_SCALE[ao.min(3) as usize];
    let mut rgb = [0u8; 3];
    for channel in 0..3 {
        let sky_part = sky_level * SKY_TINT[channel] / 255;
        let block_part = block_level * BLOCK_TINT[channel] / 255;
        let base = (AMBIENT[channel] + sky_part + block_part).min(255) as u32;
        rgb[channel] = (base * shade as u32 * ao_scale as u32 / (255 * 255)) as u8;
    }
    rgb
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logic::world::VoxelWorld;

    fn at(x: i32, y: i32, z: i32) -> IVec3 {
        IVec3::new(x, y, z)
    }

    /// 空世界全部直接露天：天空光 15、方块光 0。
    #[test]
    fn empty_world_is_fully_sky_lit() {
        let world = VoxelWorld::empty(1);
        for pos in [
            at(0, 0, 0),
            at(10, 20, 30),
            at(WORLD_SIZE as i32 - 1, CHUNK_HEIGHT as i32 - 1, 7),
        ] {
            assert_eq!(world.light().sky(pos), MAX_LIGHT);
            assert_eq!(world.light().block_light(pos), 0);
        }
    }

    /// 悬空方块让下方整列失去直接露天；挖掉后整列恢复 15。
    #[test]
    fn placed_block_shadows_its_column() {
        let mut world = VoxelWorld::empty(1);
        let block = at(10, 10, 10);
        assert!(world.set_block(block, Block::Stone).changed);
        for y in (0..10).rev() {
            assert_eq!(
                world.light().sky(at(10, y, 10)),
                14,
                "方块下方 y={y} 应由侧向回填到 14"
            );
        }
        world.set_block(block, Block::Air);
        for y in 0..10 {
            assert_eq!(world.light().sky(at(10, y, 10)), MAX_LIGHT);
        }
    }

    /// 1×1 封闭竖井：封顶后全黑，掀掉顶块后整柱恢复 15。
    #[test]
    fn capped_shaft_goes_dark_and_reopens() {
        let mut world = VoxelWorld::empty(2);
        let (cx, cz) = (10, 10);
        for y in 4..=10 {
            for z in (cz - 1)..=(cz + 1) {
                for x in (cx - 1)..=(cx + 1) {
                    let floor = y == 4;
                    let shaft = x == cx && z == cz;
                    if floor || !shaft {
                        world.set_block(at(x, y, z), Block::Stone);
                    }
                }
            }
        }
        for y in 5..=10 {
            assert_eq!(world.light().sky(at(cx, y, cz)), MAX_LIGHT);
        }
        world.set_block(at(cx, 10, cz), Block::Stone);
        for y in 5..=9 {
            assert_eq!(world.light().sky(at(cx, y, cz)), 0, "封闭竖井 y={y} 应为 0");
        }
        world.set_block(at(cx, 10, cz), Block::Air);
        for y in 5..=10 {
            assert_eq!(world.light().sky(at(cx, y, cz)), MAX_LIGHT);
        }
    }

    /// 荧光石的光按每格 1 级衰减，14 格后消失；拆除后全部归零。
    #[test]
    fn glowstone_falloff_and_removal() {
        let mut world = VoxelWorld::empty(3);
        let source = at(20, 20, 20);
        world.set_block(source, Block::Glowstone);
        assert_eq!(world.light().block_light(source), MAX_LIGHT);
        for distance in 1..=14 {
            assert_eq!(
                world.light().block_light(source + IVec3::X * distance),
                15 - distance as u8,
                "距离 {distance} 的方块光"
            );
        }
        assert_eq!(
            world.light().block_light(source + IVec3::X * 15),
            0,
            "15 格外应无光"
        );
        world.set_block(source, Block::Air);
        for distance in 0..=15 {
            assert_eq!(world.light().block_light(source + IVec3::X * distance), 0);
        }
    }

    /// 封闭石室里的光源不会照到室外。
    #[test]
    fn glowstone_inside_sealed_room_stays_inside() {
        let mut world = VoxelWorld::empty(4);
        for y in 10..15 {
            for z in 10..15 {
                for x in 10..15 {
                    let shell = x == 10 || x == 14 || y == 10 || y == 14 || z == 10 || z == 14;
                    if shell {
                        world.set_block(at(x, y, z), Block::Stone);
                    }
                }
            }
        }
        world.set_block(at(12, 12, 12), Block::Glowstone);
        assert!(world.light().block_light(at(11, 12, 12)) > 0);
        assert_eq!(world.light().block_light(at(9, 12, 12)), 0);
        assert_eq!(world.light().block_light(at(15, 12, 12)), 0);
    }

    /// 固定编辑序列后，增量光场必须与全量重算逐元素一致。
    #[test]
    fn incremental_light_matches_full_recompute() {
        let mut world = VoxelWorld::empty(77);
        let edits = [
            (at(20, 12, 20), Block::Stone),
            (at(21, 12, 20), Block::Stone),
            (at(22, 12, 20), Block::Stone),
            (at(21, 11, 20), Block::Glowstone),
            (at(20, 12, 20), Block::Air),
            (at(23, 12, 20), Block::Stone),
            (at(21, 11, 20), Block::Air),
            (at(21, 13, 20), Block::Stone),
            (at(24, 12, 20), Block::Planks),
            (at(24, 12, 20), Block::Air),
            (at(21, 12, 21), Block::Dirt),
            (at(21, 12, 21), Block::Air),
        ];
        for (pos, block) in edits {
            world.set_block(pos, block);
        }
        let fresh = LightGrid::compute(world.chunks());
        assert_eq!(world.light().raw_sky(), fresh.raw_sky(), "天空光不一致");
        assert_eq!(world.light().raw_block(), fresh.raw_block(), "方块光不一致");
    }

    /// 荧光石跨区块传播时，光照脏区块覆盖传播范围与边界邻区块。
    ///
    /// 光源在区块 (4,4) 的局部 (1,1)，14 格光程恰好在局部 15 处
    /// 停止，所以东侧与南侧邻区块 (5,4)、(4,5) 的接缝顶点会被采样到；
    /// 对角 (5,5) 的距离是 28 格，不在光照范围内。
    #[test]
    fn light_dirty_chunks_cover_cross_chunk_glow() {
        let mut world = VoxelWorld::empty(5);
        let outcome = world.set_block(at(65, 40, 65), Block::Glowstone);
        assert_eq!(
            outcome.light_dirty,
            vec![
                glam::IVec2::new(3, 3),
                glam::IVec2::new(3, 4),
                glam::IVec2::new(4, 3),
                glam::IVec2::new(4, 4),
                glam::IVec2::new(4, 5),
                glam::IVec2::new(5, 4),
            ]
        );
        assert!(!outcome.light_dirty.contains(&glam::IVec2::new(5, 5)));
        assert!(!outcome.light_dirty.contains(&glam::IVec2::new(6, 4)));
    }

    /// 顶点色查表：环境底光、满天空光、满方块光与 AO 档位的精确值。
    #[test]
    fn vertex_colors_are_exact_and_monotonic() {
        assert_eq!(vertex_rgb(0, 0, 255, 3), [24, 26, 31]);
        assert_eq!(vertex_rgb(15, 0, 255, 3), [249, 255, 255]);
        assert_eq!(vertex_rgb(0, 15, 255, 3), [255, 222, 161]);
        assert_eq!(vertex_rgb(15, 0, 255, 0), [136, 140, 140]);
        assert!(
            vertex_rgb(15, 0, 255, 2)[0] < vertex_rgb(15, 0, 255, 3)[0],
            "AO 等级越低越暗"
        );
        assert!(
            vertex_rgb(15, 0, 168, 3)[0] < vertex_rgb(15, 0, 255, 3)[0],
            "侧面明暗应低于顶面"
        );
        for level in 1..=15 {
            assert!(
                vertex_rgb(level, 0, 255, 3)[0] >= vertex_rgb(level - 1, 0, 255, 3)[0],
                "天空光等级递增时不应变暗：{level}"
            );
            assert!(
                vertex_rgb(0, level, 255, 3)[1] >= vertex_rgb(0, level - 1, 255, 3)[1],
                "方块光等级递增时不应变暗：{level}"
            );
        }
    }

    /// 生成的同 seed 世界方块与两份光场都逐字节一致。
    #[test]
    fn generated_world_light_is_deterministic() {
        let a = VoxelWorld::generate(20_261_022);
        let b = VoxelWorld::generate(20_261_022);
        assert_eq!(a.light().raw_sky(), b.light().raw_sky());
        assert_eq!(a.light().raw_block(), b.light().raw_block());
    }
}
