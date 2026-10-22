//! 固定体素世界：区块划分、方块读写与编辑影响范围。
//!
//! 世界是 8×8 个区块，每区块 16×16×48，全部常驻。区块数据按
//! `(y × 16 + z) × 16 + x` 行主序存放。世界坐标越界查询返回空气，
//! 写入越界被忽略。

use glam::{IVec2, IVec3};

use super::block::Block;

pub const CHUNK_SIZE: usize = 16;
pub const CHUNK_HEIGHT: usize = 48;
pub const WORLD_CHUNKS: usize = 8;
pub const WORLD_SIZE: usize = CHUNK_SIZE * WORLD_CHUNKS;
pub const CHUNK_COUNT: usize = WORLD_CHUNKS * WORLD_CHUNKS;
pub const CHUNK_VOLUME: usize = CHUNK_SIZE * CHUNK_SIZE * CHUNK_HEIGHT;

/// 单区块方块数组。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    blocks: Vec<u8>,
}

impl Chunk {
    pub fn empty() -> Self {
        Self {
            blocks: vec![Block::Air.as_u8(); CHUNK_VOLUME],
        }
    }

    #[inline]
    pub fn index(x: usize, y: usize, z: usize) -> usize {
        debug_assert!(x < CHUNK_SIZE && y < CHUNK_HEIGHT && z < CHUNK_SIZE);
        (y * CHUNK_SIZE + z) * CHUNK_SIZE + x
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize, z: usize) -> Block {
        Block::from_u8(self.blocks[Self::index(x, y, z)])
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, z: usize, block: Block) {
        self.blocks[Self::index(x, y, z)] = block.as_u8();
    }

    /// 原始字节（确定性测试用）。
    pub fn raw(&self) -> &[u8] {
        &self.blocks
    }
}

/// 方块写入的结果：是否真的改变，以及需要重建网格与碰撞的区块。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditOutcome {
    pub changed: bool,
    /// 自身区块在前，随后按西、东、北、南顺序加入跨边界的相邻区块。
    pub dirty: Vec<IVec2>,
}

impl EditOutcome {
    fn unchanged() -> Self {
        Self {
            changed: false,
            dirty: Vec::new(),
        }
    }
}

/// 固定体素世界。
#[derive(Clone, Debug)]
pub struct VoxelWorld {
    pub seed: u64,
    chunks: Vec<Chunk>,
}

impl VoxelWorld {
    /// 生成确定性地形（见 [`super::terrain`]）。
    pub fn generate(seed: u64) -> Self {
        super::terrain::generate(seed)
    }

    /// 全空气世界（测试与手工布置用）。
    pub fn empty(seed: u64) -> Self {
        Self {
            seed,
            chunks: (0..CHUNK_COUNT).map(|_| Chunk::empty()).collect(),
        }
    }

    /// 区块坐标是否在世界内。
    pub fn chunk_in_bounds(coord: IVec2) -> bool {
        coord.x >= 0
            && coord.y >= 0
            && (coord.x as usize) < WORLD_CHUNKS
            && (coord.y as usize) < WORLD_CHUNKS
    }

    /// 区块坐标到线性下标；越界返回 `None`。
    pub fn chunk_index(coord: IVec2) -> Option<usize> {
        Self::chunk_in_bounds(coord).then(|| coord.y as usize * WORLD_CHUNKS + coord.x as usize)
    }

    pub fn chunk(&self, coord: IVec2) -> Option<&Chunk> {
        Self::chunk_index(coord).map(|index| &self.chunks[index])
    }

    pub fn chunk_mut(&mut self, coord: IVec2) -> Option<&mut Chunk> {
        Self::chunk_index(coord).map(|index| &mut self.chunks[index])
    }

    pub fn chunks(&self) -> &[Chunk] {
        &self.chunks
    }

    /// 方块坐标是否在世界内。
    pub fn in_bounds(pos: IVec3) -> bool {
        pos.x >= 0
            && pos.y >= 0
            && pos.z >= 0
            && (pos.x as usize) < WORLD_SIZE
            && (pos.y as usize) < CHUNK_HEIGHT
            && (pos.z as usize) < WORLD_SIZE
    }

    /// 方块所属区块坐标（不检查边界）。
    pub fn chunk_of(pos: IVec3) -> IVec2 {
        IVec2::new(
            pos.x.div_euclid(CHUNK_SIZE as i32),
            pos.z.div_euclid(CHUNK_SIZE as i32),
        )
    }

    /// 读取方块；越界返回空气。
    pub fn block(&self, pos: IVec3) -> Block {
        if !Self::in_bounds(pos) {
            return Block::Air;
        }
        let coord = Self::chunk_of(pos);
        let local_x = pos.x.rem_euclid(CHUNK_SIZE as i32) as usize;
        let local_z = pos.z.rem_euclid(CHUNK_SIZE as i32) as usize;
        self.chunk(coord)
            .map(|chunk| chunk.get(local_x, pos.y as usize, local_z))
            .unwrap_or(Block::Air)
    }

    /// 写入方块。越界、空气写空气或同值写入都不改变世界。
    pub fn set_block(&mut self, pos: IVec3, block: Block) -> EditOutcome {
        if !Self::in_bounds(pos) || self.block(pos) == block {
            return EditOutcome::unchanged();
        }
        let coord = Self::chunk_of(pos);
        let local_x = pos.x.rem_euclid(CHUNK_SIZE as i32);
        let local_z = pos.z.rem_euclid(CHUNK_SIZE as i32);
        if let Some(chunk) = self.chunk_mut(coord) {
            chunk.set(local_x as usize, pos.y as usize, local_z as usize, block);
        } else {
            return EditOutcome::unchanged();
        }

        let mut dirty = vec![coord];
        // 面剔除依赖相邻区块的数据：边界方块的编辑要让邻区块重建。
        if local_x == 0 {
            Self::push_dirty(&mut dirty, coord + IVec2::new(-1, 0));
        } else if local_x as usize == CHUNK_SIZE - 1 {
            Self::push_dirty(&mut dirty, coord + IVec2::new(1, 0));
        }
        if local_z == 0 {
            Self::push_dirty(&mut dirty, coord + IVec2::new(0, -1));
        } else if local_z as usize == CHUNK_SIZE - 1 {
            Self::push_dirty(&mut dirty, coord + IVec2::new(0, 1));
        }
        EditOutcome {
            changed: true,
            dirty,
        }
    }

    fn push_dirty(dirty: &mut Vec<IVec2>, coord: IVec2) {
        if Self::chunk_in_bounds(coord) {
            dirty.push(coord);
        }
    }

    /// 该列最高的实心方块 y；整列空气返回 `None`。
    pub fn surface_height(&self, x: i32, z: i32) -> Option<usize> {
        if x < 0 || z < 0 || x as usize >= WORLD_SIZE || z as usize >= WORLD_SIZE {
            return None;
        }
        (0..CHUNK_HEIGHT)
            .rev()
            .find(|&y| self.block(IVec3::new(x, y as i32, z)).is_solid())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: i32, y: i32, z: i32) -> IVec3 {
        IVec3::new(x, y, z)
    }

    /// 区块下标与方块坐标的换算覆盖 0 号与最后一个区块。
    #[test]
    fn chunk_index_roundtrip() {
        for (cx, cz) in [(0, 0), (3, 5), (7, 7)] {
            let coord = IVec2::new(cx, cz);
            let index = VoxelWorld::chunk_index(coord).unwrap();
            assert_eq!(index, cz as usize * WORLD_CHUNKS + cx as usize);
            let pos = at(cx * 16 + 7, 10, cz * 16 + 9);
            assert_eq!(VoxelWorld::chunk_of(pos), coord);
        }
        assert!(VoxelWorld::chunk_index(IVec2::new(-1, 0)).is_none());
        assert!(VoxelWorld::chunk_index(IVec2::new(8, 0)).is_none());
    }

    /// 越界读写：读空气、写忽略。
    #[test]
    fn out_of_bounds_is_inert() {
        let mut world = VoxelWorld::empty(1);
        for pos in [
            at(-1, 0, 0),
            at(0, -1, 0),
            at(0, 0, WORLD_SIZE as i32),
            at(WORLD_SIZE as i32, 0, 0),
        ] {
            assert_eq!(world.block(pos), Block::Air);
            let outcome = world.set_block(pos, Block::Stone);
            assert!(!outcome.changed);
            assert!(outcome.dirty.is_empty());
        }
    }

    /// 非边界编辑只脏自身区块；边界编辑按东西南北顺序补邻区块。
    #[test]
    fn edit_dirty_set_covers_borders() {
        let mut world = VoxelWorld::empty(1);
        let own = world.set_block(at(5, 5, 5), Block::Stone);
        assert!(own.changed);
        assert_eq!(own.dirty, vec![IVec2::new(0, 0)]);

        let west = world.set_block(at(0, 5, 5), Block::Stone);
        assert_eq!(west.dirty, vec![IVec2::new(0, 0)]);

        let east = world.set_block(at(31, 5, 5), Block::Stone);
        assert_eq!(east.dirty, vec![IVec2::new(1, 0), IVec2::new(2, 0)]);

        let corner = world.set_block(at(31, 5, 31), Block::Stone);
        assert_eq!(
            corner.dirty,
            vec![IVec2::new(1, 1), IVec2::new(2, 1), IVec2::new(1, 2),]
        );
    }

    /// 世界边缘的编辑不会加入不存在的邻区块。
    #[test]
    fn world_edge_edit_has_no_phantom_neighbour() {
        let mut world = VoxelWorld::empty(1);
        let outcome = world.set_block(at(0, 5, 0), Block::Stone);
        assert!(outcome.changed);
        assert_eq!(outcome.dirty, vec![IVec2::new(0, 0)]);
    }

    /// 重复写入同值不产生脏区块。
    #[test]
    fn writing_same_block_is_noop() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(at(3, 3, 3), Block::Dirt);
        let outcome = world.set_block(at(3, 3, 3), Block::Dirt);
        assert!(!outcome.changed);
        assert!(outcome.dirty.is_empty());
    }

    /// 表面高度扫描找到最高实心块。
    #[test]
    fn surface_height_finds_top_solid() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(at(2, 7, 3), Block::Stone);
        world.set_block(at(2, 11, 3), Block::Dirt);
        assert_eq!(world.surface_height(2, 3), Some(11));
        assert_eq!(world.surface_height(4, 4), None);
        assert_eq!(world.surface_height(-1, 0), None);
    }
}
