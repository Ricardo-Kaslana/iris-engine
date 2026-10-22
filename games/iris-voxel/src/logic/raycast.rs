//! 体素 DDA 拾取（Amanatides–Woo）。
//!
//! 世界是规则的方块网格，逐方块遍历比引擎的通用射线查询更合适：
//! 复杂度只与射线穿过的格子数相关，不扫描任何碰撞体列表。

use glam::{IVec3, Vec3};

use super::world::{CHUNK_HEIGHT, VoxelWorld, WORLD_SIZE};

/// 一次方块命中。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockHit {
    /// 被命中的方块坐标。
    pub block: IVec3,
    /// 射线进入该方块的面法线。
    pub normal: IVec3,
    /// 从射线原点到进入面的距离。
    pub distance: f32,
}

/// 沿 `direction` 找到首个实心方块，最远 `max_distance` 米。
/// 方向为零、起点在世界上方之外或未命中时返回 `None`。
pub fn pick_block(
    world: &VoxelWorld,
    origin: Vec3,
    direction: Vec3,
    max_distance: f32,
) -> Option<BlockHit> {
    let direction = direction.normalize_or_zero();
    if direction == Vec3::ZERO || max_distance <= 0.0 {
        return None;
    }

    let mut cell = IVec3::new(
        origin.x.floor() as i32,
        origin.y.floor() as i32,
        origin.z.floor() as i32,
    );
    let step = IVec3::new(
        direction.x.signum() as i32,
        direction.y.signum() as i32,
        direction.z.signum() as i32,
    );

    // 各轴到下一格边界的参数距离；零分量用无穷大参与比较。
    let mut t_max = Vec3::splat(f32::INFINITY);
    let mut t_delta = Vec3::splat(f32::INFINITY);
    for axis in 0..3 {
        let d = direction[axis];
        if d == 0.0 {
            continue;
        }
        t_delta[axis] = 1.0 / d.abs();
        let boundary = if d > 0.0 {
            (cell[axis] + 1) as f32
        } else {
            cell[axis] as f32
        };
        t_max[axis] = (boundary - origin[axis]) / d;
    }

    let mut entry_normal = IVec3::ZERO;
    let mut distance = 0.0_f32;
    // 起点所在格若已是实心，法线无从判定；按零法线命中返回。
    if world.block(cell).is_solid() {
        return Some(BlockHit {
            block: cell,
            normal: entry_normal,
            distance,
        });
    }

    loop {
        // 前进到最近的下一个格子边界。
        let axis = if t_max.x <= t_max.y && t_max.x <= t_max.z {
            0
        } else if t_max.y <= t_max.z {
            1
        } else {
            2
        };
        distance = t_max[axis];
        if !distance.is_finite() || distance > max_distance {
            return None;
        }
        cell[axis] += step[axis];
        t_max[axis] += t_delta[axis];
        entry_normal = IVec3::ZERO;
        entry_normal[axis] = -step[axis];

        if cell.x < 0
            || cell.z < 0
            || cell.y < 0
            || cell.x as usize >= WORLD_SIZE
            || cell.z as usize >= WORLD_SIZE
            || cell.y as usize >= CHUNK_HEIGHT
        {
            return None;
        }
        if world.block(cell).is_solid() {
            return Some(BlockHit {
                block: cell,
                normal: entry_normal,
                distance,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logic::block::Block;

    fn at(x: i32, y: i32, z: i32) -> IVec3 {
        IVec3::new(x, y, z)
    }

    /// 沿 +X 命中已知方块：进入面是 -X，距离是到 x=3.0 平面的距离。
    #[test]
    fn hits_block_along_x() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(at(3, 1, 0), Block::Stone);
        let hit = pick_block(&world, Vec3::new(0.5, 1.5, 0.5), Vec3::X, 5.0).unwrap();
        assert_eq!(hit.block, at(3, 1, 0));
        assert_eq!(hit.normal, at(-1, 0, 0));
        assert!((hit.distance - 2.5).abs() < 1e-5);
    }

    /// 斜向射线进入顶面时法线为 +Y。
    #[test]
    fn top_face_normal() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(at(0, 0, 0), Block::Stone);
        let origin = Vec3::new(0.5, 3.0, 0.5);
        let hit = pick_block(&world, origin, Vec3::new(0.0, -1.0, 0.0), 5.0).unwrap();
        assert_eq!(hit.block, at(0, 0, 0));
        assert_eq!(hit.normal, at(0, 1, 0));
        assert!((hit.distance - 2.0).abs() < 1e-6);
    }

    /// 超出距离、方向为零与世界外都返回 None。
    #[test]
    fn misses_are_none() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(at(5, 1, 0), Block::Stone);
        assert!(pick_block(&world, Vec3::new(0.5, 1.5, 0.5), Vec3::X, 4.0).is_none());
        assert!(pick_block(&world, Vec3::new(0.5, 1.5, 0.5), Vec3::ZERO, 5.0).is_none());
        assert!(
            pick_block(
                &world,
                Vec3::new(0.5, 1.5, 0.5),
                Vec3::new(0.0, 0.0, -1.0),
                0.2
            )
            .is_none()
        );
    }

    /// 零分量方向不产生 NaN，水平射线不会意外命中同层的竖直邻格。
    #[test]
    fn zero_components_are_stable() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(at(0, 1, 2), Block::Stone);
        let hit = pick_block(
            &world,
            Vec3::new(0.5, 1.5, 0.5),
            Vec3::new(0.0, 0.0, 1.0),
            4.0,
        )
        .unwrap();
        assert_eq!(hit.block, at(0, 1, 2));
        assert!(hit.distance.is_finite());
        assert!((hit.distance - 1.5).abs() < 1e-5);
    }

    /// 起点在实心块内：立即返回该方块，距离为零。
    #[test]
    fn starting_inside_solid_returns_self() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(at(1, 1, 1), Block::Stone);
        let hit = pick_block(&world, Vec3::new(1.5, 1.5, 1.5), Vec3::Y, 5.0).unwrap();
        assert_eq!(hit.block, at(1, 1, 1));
        assert_eq!(hit.distance, 0.0);
    }
}
