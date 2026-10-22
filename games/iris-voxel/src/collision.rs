//! 体素碰撞数据：逐区块实心壳与玩家邻域收集。
//!
//! 引擎的角色控制器接收扁平静态体列表，没有逐区块查询入口。游戏侧
//! 为每个区块缓存「至少有一面暴露于空气」的实心方块整格 AABB；玩家
//! 每步只合并所在区块及 8 个邻区块，避免全世界上万方块线性扫描。
//! 世界边界另加 4 面不可见墙，防止走出固定世界。

use glam::{IVec3, Vec3};
use iris_engine::prelude::ColliderShape;

use crate::logic::player::{PLAYER_HALF_HEIGHT, PLAYER_RADIUS, PlayerState};
use crate::logic::world::{CHUNK_HEIGHT, CHUNK_SIZE, VoxelWorld, WORLD_CHUNKS, WORLD_SIZE};

/// 静态障碍：方块中心与整格 AABB。
pub type StaticBox = (Vec3, ColliderShape);

/// 全部区块的碰撞壳缓存。
pub struct ChunkCollision {
    lists: Vec<Vec<StaticBox>>,
}

impl ChunkCollision {
    pub fn build(world: &VoxelWorld) -> Self {
        let mut lists = Vec::with_capacity(WORLD_CHUNKS * WORLD_CHUNKS);
        for cz in 0..WORLD_CHUNKS {
            for cx in 0..WORLD_CHUNKS {
                lists.push(build_chunk_colliders(
                    world,
                    glam::IVec2::new(cx as i32, cz as i32),
                ));
            }
        }
        Self { lists }
    }

    fn index(coord: glam::IVec2) -> Option<usize> {
        VoxelWorld::chunk_index(coord)
    }

    /// 重建一个区块的壳。坐标越界时忽略。
    pub fn rebuild(&mut self, world: &VoxelWorld, coord: glam::IVec2) {
        if let Some(index) = Self::index(coord) {
            self.lists[index] = build_chunk_colliders(world, coord);
        }
    }

    /// 合并 `center` 所在区块及 8 邻区块的静态体，并追加世界边界墙。
    pub fn gather(&self, center: Vec3, out: &mut Vec<StaticBox>) {
        out.clear();
        let cx = (center.x.floor() as i32).div_euclid(CHUNK_SIZE as i32);
        let cz = (center.z.floor() as i32).div_euclid(CHUNK_SIZE as i32);
        for dz in -1..=1 {
            for dx in -1..=1 {
                let coord = glam::IVec2::new(cx + dx, cz + dz);
                if let Some(index) = Self::index(coord) {
                    out.extend_from_slice(&self.lists[index]);
                }
            }
        }
        append_world_borders(out);
    }

    /// 某区块的碰撞壳数量（测试与诊断）。
    pub fn len(&self, coord: glam::IVec2) -> Option<usize> {
        Self::index(coord).map(|index| self.lists[index].len())
    }
}

/// 区块内所有暴露于空气的实心方块，整格 AABB。
pub fn build_chunk_colliders(world: &VoxelWorld, coord: glam::IVec2) -> Vec<StaticBox> {
    let Some(chunk) = world.chunk(coord) else {
        return Vec::new();
    };
    let origin_x = coord.x * CHUNK_SIZE as i32;
    let origin_z = coord.y * CHUNK_SIZE as i32;
    let mut boxes = Vec::new();
    for y in 0..CHUNK_HEIGHT {
        for z in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                if !chunk.get(x, y, z).is_solid() {
                    continue;
                }
                let world_pos = IVec3::new(origin_x + x as i32, y as i32, origin_z + z as i32);
                let exposed = crate::logic::block::Face::ALL.iter().any(|face| {
                    let n = face.normal();
                    !world
                        .block(world_pos + IVec3::new(n[0], n[1], n[2]))
                        .is_solid()
                });
                if exposed {
                    boxes.push((
                        Vec3::new(
                            world_pos.x as f32 + 0.5,
                            world_pos.y as f32 + 0.5,
                            world_pos.z as f32 + 0.5,
                        ),
                        ColliderShape::Box {
                            half_extents: Vec3::splat(0.5),
                        },
                    ));
                }
            }
        }
    }
    boxes
}

fn append_world_borders(out: &mut Vec<StaticBox>) {
    let half_height = 64.0;
    let center_y = 24.0;
    let half_size = WORLD_SIZE as f32 * 0.5 + 2.0;
    out.push((
        Vec3::new(-1.0, center_y, WORLD_SIZE as f32 * 0.5),
        ColliderShape::Box {
            half_extents: Vec3::new(1.0, half_height, half_size),
        },
    ));
    out.push((
        Vec3::new(WORLD_SIZE as f32 + 1.0, center_y, WORLD_SIZE as f32 * 0.5),
        ColliderShape::Box {
            half_extents: Vec3::new(1.0, half_height, half_size),
        },
    ));
    out.push((
        Vec3::new(WORLD_SIZE as f32 * 0.5, center_y, -1.0),
        ColliderShape::Box {
            half_extents: Vec3::new(half_size, half_height, 1.0),
        },
    ));
    out.push((
        Vec3::new(WORLD_SIZE as f32 * 0.5, center_y, WORLD_SIZE as f32 + 1.0),
        ColliderShape::Box {
            half_extents: Vec3::new(half_size, half_height, 1.0),
        },
    ));
}

/// 方块整格 AABB 是否与玩家胶囊的包围盒重叠。
pub fn block_overlaps_player(block: IVec3, player: &PlayerState) -> bool {
    let block_min = Vec3::new(block.x as f32, block.y as f32, block.z as f32);
    let block_max = block_min + Vec3::ONE;
    let player_min = player.position - Vec3::new(PLAYER_RADIUS, PLAYER_HALF_HEIGHT, PLAYER_RADIUS);
    let player_max = player.position + Vec3::new(PLAYER_RADIUS, PLAYER_HALF_HEIGHT, PLAYER_RADIUS);
    block_min.x < player_max.x
        && block_max.x > player_min.x
        && block_min.y < player_max.y
        && block_max.y > player_min.y
        && block_min.z < player_max.z
        && block_max.z > player_min.z
}

/// 放置合法性：界内、目标为空气、不与玩家重叠。
pub fn can_place(world: &VoxelWorld, player: &PlayerState, pos: IVec3) -> bool {
    VoxelWorld::in_bounds(pos)
        && world.block(pos) == crate::logic::Block::Air
        && !block_overlaps_player(pos, player)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logic::Block;

    /// 3×3×3 实心体的壳恰好是 26 块（去掉完全被包裹的中心）。
    #[test]
    fn solid_cube_shell_has_26_boxes() {
        let mut world = VoxelWorld::empty(1);
        for y in 5..8 {
            for z in 5..8 {
                for x in 5..8 {
                    world.set_block(IVec3::new(x, y, z), Block::Stone);
                }
            }
        }
        let boxes = build_chunk_colliders(&world, glam::IVec2::ZERO);
        assert_eq!(boxes.len(), 26);
    }

    /// 空世界没有方块壳，但 gather 仍返回 4 面世界边界墙。
    #[test]
    fn empty_world_has_only_border_walls() {
        let world = VoxelWorld::empty(1);
        let collision = ChunkCollision::build(&world);
        let mut out = Vec::new();
        collision.gather(Vec3::new(64.0, 16.0, 64.0), &mut out);
        assert_eq!(out.len(), 4);
        assert_eq!(collision.len(glam::IVec2::ZERO), Some(0));
    }

    /// 邻域收集包含 3×3 区块内的方块，忽略远处区块。
    #[test]
    fn gather_includes_neighbour_chunks_only() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(IVec3::new(8, 5, 8), Block::Stone);
        world.set_block(IVec3::new(80, 5, 80), Block::Stone);
        let collision = ChunkCollision::build(&world);
        let mut out = Vec::new();
        collision.gather(Vec3::new(8.0, 6.0, 8.0), &mut out);
        assert_eq!(out.len(), 4 + 1);
        collision.gather(Vec3::new(120.0, 6.0, 120.0), &mut out);
        assert_eq!(out.len(), 4);
    }

    /// 编辑后重建只影响目标区块。
    #[test]
    fn rebuild_updates_single_chunk() {
        let mut world = VoxelWorld::empty(1);
        let coord = glam::IVec2::new(1, 1);
        let mut collision = ChunkCollision::build(&world);
        assert_eq!(collision.len(coord), Some(0));
        world.set_block(IVec3::new(20, 5, 20), Block::Dirt);
        collision.rebuild(&world, coord);
        assert_eq!(collision.len(coord), Some(1));
        assert_eq!(collision.len(glam::IVec2::ZERO), Some(0));
    }

    /// 玩家脚下的方块和与包围盒相交的方块都拒绝放置。
    #[test]
    fn placement_rejects_player_overlap_and_bounds() {
        let world = VoxelWorld::empty(1);
        let player = PlayerState {
            position: Vec3::new(8.5, 5.9, 8.5),
            velocity: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            grounded: true,
        };
        assert!(
            !can_place(&world, &player, IVec3::new(8, 5, 8)),
            "脚下格与胶囊相交"
        );
        assert!(!can_place(&world, &player, IVec3::new(-1, 5, 8)), "界外");
        let mut blocked = world;
        blocked.set_block(IVec3::new(4, 5, 8), Block::Stone);
        assert!(
            !can_place(&blocked, &player, IVec3::new(4, 5, 8)),
            "目标非空气"
        );
        assert!(
            can_place(&blocked, &player, IVec3::new(4, 6, 8)),
            "相邻空格可放"
        );
    }
}
