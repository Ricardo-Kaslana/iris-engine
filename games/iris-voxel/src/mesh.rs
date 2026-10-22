//! 区块网格生成与目标框网格。
//!
//! 逐方块检查六个面，只有相邻格非实心时才发射该面。相邻查询走
//! [`VoxelWorld::block`]，因此区块边界处会读取邻区块数据，接缝不会
//! 多出一层面。顶点使用区块局部坐标，实体的 `Transform` 放在区块
//! 原点。

use glam::IVec2;
use iris_engine::prelude::{MeshData, Vertex, VertexColor};

use crate::atlas::tile_uv;
use crate::logic::block::{Block, Face, Tile};
use crate::logic::world::{CHUNK_HEIGHT, CHUNK_SIZE, VoxelWorld};

struct FaceDef {
    face: Face,
    normal: [f32; 3],
    /// 四个角相对方块最小角的偏移，顺序为 CCW，且 `u × v == normal`。
    corners: [[f32; 3]; 4],
    /// 经典体素逐面明暗（无光照材质下由顶点色乘入）。顶面最亮，
    /// 底面最暗，四个侧面各不相同以拉开体积感。
    shade: u8,
}

const UVS: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

const FACES: [FaceDef; 6] = [
    // +Y：u = +X，v = -Z
    FaceDef {
        face: Face::Top,
        normal: [0.0, 1.0, 0.0],
        corners: [
            [0.0, 1.0, 1.0],
            [1.0, 1.0, 1.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ],
        shade: 255,
    },
    // -Y：u = +X，v = +Z
    FaceDef {
        face: Face::Bottom,
        normal: [0.0, -1.0, 0.0],
        corners: [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 1.0],
            [0.0, 0.0, 1.0],
        ],
        shade: 128,
    },
    // -X：中心在 x=0，u = +Z，v = +Y
    FaceDef {
        face: Face::West,
        normal: [-1.0, 0.0, 0.0],
        corners: [
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 1.0, 1.0],
            [0.0, 1.0, 0.0],
        ],
        shade: 168,
    },
    // +X：中心在 x=1，u = -Z，v = +Y
    FaceDef {
        face: Face::East,
        normal: [1.0, 0.0, 0.0],
        corners: [
            [1.0, 0.0, 1.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [1.0, 1.0, 1.0],
        ],
        shade: 184,
    },
    // -Z：中心在 z=0，u = -X，v = +Y
    FaceDef {
        face: Face::North,
        normal: [0.0, 0.0, -1.0],
        corners: [
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
        ],
        shade: 208,
    },
    // +Z：中心在 z=1，u = +X，v = +Y
    FaceDef {
        face: Face::South,
        normal: [0.0, 0.0, 1.0],
        corners: [
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ],
        shade: 224,
    },
];

/// 生成一个区块的可见面网格（局部坐标）。区块坐标越界时返回空网格。
pub fn build_chunk_mesh(world: &VoxelWorld, coord: IVec2) -> MeshData {
    let mut mesh = MeshData::default();
    let Some(chunk) = world.chunk(coord) else {
        return mesh;
    };
    let origin_x = coord.x * CHUNK_SIZE as i32;
    let origin_z = coord.y * CHUNK_SIZE as i32;

    for y in 0..CHUNK_HEIGHT {
        for z in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                let block = chunk.get(x, y, z);
                if !block.is_solid() {
                    continue;
                }
                let world_x = origin_x + x as i32;
                let world_z = origin_z + z as i32;
                for face in &FACES {
                    let n = face.normal;
                    let neighbour = glam::IVec3::new(
                        world_x + n[0] as i32,
                        y as i32 + n[1] as i32,
                        world_z + n[2] as i32,
                    );
                    if world.block(neighbour).is_solid() {
                        continue;
                    }
                    push_face(&mut mesh, [x as f32, y as f32, z as f32], block, face);
                }
            }
        }
    }
    mesh
}

fn push_face(mesh: &mut MeshData, min: [f32; 3], block: Block, face: &FaceDef) {
    let tile = block.atlas_tile(face.face);
    let base = mesh.vertices.len() as u32;
    for (corner, uv) in face.corners.into_iter().zip(UVS) {
        mesh.vertices.push(Vertex {
            position: [min[0] + corner[0], min[1] + corner[1], min[2] + corner[2]],
            normal: face.normal,
            uv: tile_uv(tile, uv[0], uv[1]),
        });
        mesh.colors
            .get_or_insert_with(Vec::new)
            .push(VertexColor::new(face.shade, face.shade, face.shade, 255));
    }
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// 目标方块框：12 条细长盒子合并成一个网格，中心在世界原点，
/// 使用时把实体平移到方块中心。
pub fn wireframe_cube_mesh() -> MeshData {
    let mut mesh = MeshData::default();
    let half = 0.5;
    let t = 0.018;
    for a in [-half, half] {
        for b in [-half, half] {
            // 沿 X 的四条边
            push_box(&mut mesh, [-half, a - t, b - t], [half, a + t, b + t]);
            // 沿 Y 的四条边
            push_box(&mut mesh, [a - t, -half, b - t], [a + t, half, b + t]);
            // 沿 Z 的四条边
            push_box(&mut mesh, [a - t, b - t, -half], [a + t, b + t, half]);
        }
    }
    mesh
}

fn push_box(mesh: &mut MeshData, min: [f32; 3], max: [f32; 3]) {
    let tile = Tile::Stone;
    for face in &FACES {
        let base = mesh.vertices.len() as u32;
        for (corner, uv) in face.corners.into_iter().zip(UVS) {
            let position = [
                if corner[0] == 0.0 { min[0] } else { max[0] },
                if corner[1] == 0.0 { min[1] } else { max[1] },
                if corner[2] == 0.0 { min[2] } else { max[2] },
            ];
            mesh.vertices.push(Vertex {
                position,
                normal: face.normal,
                uv: tile_uv(tile, uv[0], uv[1]),
            });
        }
        mesh.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logic::block::Block;
    use glam::IVec3;

    fn count_faces(mesh: &MeshData) -> usize {
        mesh.indices.len() / 6
    }

    /// 空世界所有区块都是空网格。
    #[test]
    fn empty_world_has_no_faces() {
        let world = VoxelWorld::empty(1);
        for cz in 0..4 {
            for cx in 0..4 {
                let mesh = build_chunk_mesh(&world, IVec2::new(cx, cz));
                assert!(mesh.vertices.is_empty());
                assert!(mesh.indices.is_empty());
            }
        }
    }

    /// 单方块六面：24 顶点、36 索引。
    #[test]
    fn single_block_has_six_faces() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(IVec3::new(5, 5, 5), Block::Stone);
        let mesh = build_chunk_mesh(&world, IVec2::ZERO);
        assert_eq!(mesh.vertices.len(), 24);
        assert_eq!(mesh.indices.len(), 36);
        assert_eq!(count_faces(&mesh), 6);
    }

    /// 两个相邻方块只发射 10 个面（共享面被剔除）。
    #[test]
    fn adjacent_blocks_cull_shared_face() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(IVec3::new(5, 5, 5), Block::Stone);
        world.set_block(IVec3::new(6, 5, 5), Block::Stone);
        let mesh = build_chunk_mesh(&world, IVec2::ZERO);
        assert_eq!(count_faces(&mesh), 10);
    }

    /// 跨区块相邻：西侧区块的 +X 面被东侧区块的方块剔除。
    #[test]
    fn cross_chunk_neighbour_culls_face() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(IVec3::new(15, 5, 5), Block::Stone);
        world.set_block(IVec3::new(16, 5, 5), Block::Stone);
        let west = build_chunk_mesh(&world, IVec2::ZERO);
        let east = build_chunk_mesh(&world, IVec2::new(1, 0));
        assert_eq!(count_faces(&west), 5);
        assert_eq!(count_faces(&east), 5);
    }

    /// 逐面明暗：颜色侧表与顶点数一致，顶面 255、底面 128，侧面介于
    /// 两者之间。
    #[test]
    fn face_shading_colors_are_complete() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(IVec3::new(1, 1, 1), Block::Stone);
        let mesh = build_chunk_mesh(&world, IVec2::ZERO);
        let colors = mesh.colors.as_ref().expect("区块网格应带顶点色");
        assert_eq!(colors.len(), mesh.vertices.len());
        for (vertex, color) in mesh.vertices.iter().zip(colors) {
            let expected = match vertex.normal {
                [0.0, 1.0, 0.0] => 255,
                [0.0, -1.0, 0.0] => 128,
                [-1.0, 0.0, 0.0] => 168,
                [1.0, 0.0, 0.0] => 184,
                [0.0, 0.0, -1.0] => 208,
                [0.0, 0.0, 1.0] => 224,
                other => panic!("意外的面法线：{other:?}"),
            };
            assert_eq!(color.r, expected);
            assert_eq!(color.g, color.r);
            assert_eq!(color.b, color.r);
            assert_eq!(color.a, 255);
        }
    }

    /// 每个三角形的几何法线与顶点法线同向（背面剔除不会把正面翻掉）。
    #[test]
    fn triangle_winding_matches_normals() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(IVec3::new(2, 2, 2), Block::Grass);
        let mesh = build_chunk_mesh(&world, IVec2::ZERO);
        for triangle in mesh.indices.chunks_exact(3) {
            let a = glam::Vec3::from_array(mesh.vertices[triangle[0] as usize].position);
            let b = glam::Vec3::from_array(mesh.vertices[triangle[1] as usize].position);
            let c = glam::Vec3::from_array(mesh.vertices[triangle[2] as usize].position);
            let geometric = (b - a).cross(c - a);
            let declared = glam::Vec3::from_array(mesh.vertices[triangle[0] as usize].normal);
            assert!(
                geometric.dot(declared) > 0.0,
                "三角形环绕与法线相反：{geometric} vs {declared}"
            );
        }
    }

    /// UV 落在图集范围内，且不落在图集外的负值区。
    #[test]
    fn uvs_stay_in_atlas() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(IVec3::new(1, 1, 1), Block::Grass);
        let mesh = build_chunk_mesh(&world, IVec2::ZERO);
        for vertex in &mesh.vertices {
            assert!((0.0..=1.0).contains(&vertex.uv[0]));
            assert!((0.0..=1.0).contains(&vertex.uv[1]));
        }
    }

    /// 目标框是 12 条盒子，共 288 个顶点。
    #[test]
    fn wireframe_has_twelve_boxes() {
        let mesh = wireframe_cube_mesh();
        assert_eq!(mesh.vertices.len(), 12 * 24);
        assert_eq!(mesh.indices.len(), 12 * 36);
    }

    /// 目标框的三角形环绕同样与法线一致。
    #[test]
    fn wireframe_winding_matches_normals() {
        let mesh = wireframe_cube_mesh();
        for triangle in mesh.indices.chunks_exact(3) {
            let a = glam::Vec3::from_array(mesh.vertices[triangle[0] as usize].position);
            let b = glam::Vec3::from_array(mesh.vertices[triangle[1] as usize].position);
            let c = glam::Vec3::from_array(mesh.vertices[triangle[2] as usize].position);
            let geometric = (b - a).cross(c - a);
            let declared = glam::Vec3::from_array(mesh.vertices[triangle[0] as usize].normal);
            assert!(geometric.dot(declared) > 0.0);
        }
    }
}
