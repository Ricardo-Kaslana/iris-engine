//! 区块网格生成与目标框网格。
//!
//! 逐方块检查六个面，只有相邻格非实心时才发射该面。相邻查询走
//! [`VoxelWorld::block`]，因此区块边界处会读取邻区块数据，接缝不会
//! 多出一层面。顶点使用区块局部坐标，实体的 `Transform` 放在区块
//! 原点。
//!
//! 每个面的四个顶点独立采样平滑光照与 AO：在面外一格取前向格、两个
//! 切向边格与对角格，环境光遮蔽由后三格的实心性决定，光照只平均空气
//! 格（不透明格不参与，避免与 AO 双重变暗），再与逐面明暗一起经整数
//! 查表合成顶点色。

use glam::{IVec2, IVec3};
use iris_engine::prelude::{MeshData, Vertex, VertexColor};

use crate::atlas::tile_uv;
use crate::logic::block::{Block, Face, Tile};
use crate::logic::light::vertex_rgb;
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
                    push_face(
                        &mut mesh,
                        IVec3::new(world_x, y as i32, world_z),
                        [x as f32, y as f32, z as f32],
                        block,
                        face,
                        world,
                    );
                }
            }
        }
    }
    mesh
}

fn push_face(
    mesh: &mut MeshData,
    block_pos: IVec3,
    min: [f32; 3],
    block: Block,
    face: &FaceDef,
    world: &VoxelWorld,
) {
    let tile = block.atlas_tile(face.face);
    let base = mesh.vertices.len() as u32;
    let normal = face.normal;
    let axis = if normal[0] != 0.0 {
        0
    } else if normal[1] != 0.0 {
        1
    } else {
        2
    };
    let tangent_a = (axis + 1) % 3;
    let tangent_b = (axis + 2) % 3;
    let front = block_pos + IVec3::new(normal[0] as i32, normal[1] as i32, normal[2] as i32);
    let light = world.light();

    for (corner, uv) in face.corners.into_iter().zip(UVS) {
        let mut offset_a = [0i32; 3];
        let mut offset_b = [0i32; 3];
        offset_a[tangent_a] = if corner[tangent_a] > 0.5 { 1 } else { -1 };
        offset_b[tangent_b] = if corner[tangent_b] > 0.5 { 1 } else { -1 };
        let side_a = front + IVec3::from_array(offset_a);
        let side_b = front + IVec3::from_array(offset_b);
        let corner_cell = front
            + IVec3::from_array([
                offset_a[0] + offset_b[0],
                offset_a[1] + offset_b[1],
                offset_a[2] + offset_b[2],
            ]);

        let blocked_a = world.block(side_a).is_solid();
        let blocked_b = world.block(side_b).is_solid();
        let blocked_corner = world.block(corner_cell).is_solid();
        let ao = if blocked_a && blocked_b {
            0
        } else {
            3 - (blocked_a as u8 + blocked_b as u8 + blocked_corner as u8)
        };

        // 前向格由面剔除保证是空气，至少有一个采样；不透明格不参与
        // 平均，AO 已经单独表达了它们的遮蔽。
        let mut sky_sum = 0u16;
        let mut block_sum = 0u16;
        let mut samples = 0u16;
        for sample in [front, side_a, side_b, corner_cell] {
            if !world.block(sample).is_solid() {
                sky_sum += light.sky(sample) as u16;
                block_sum += light.block_light(sample) as u16;
                samples += 1;
            }
        }
        debug_assert!(samples > 0, "面的前向格必须是空气");
        let sky = ((sky_sum + samples / 2) / samples) as u8;
        let mut block_light = ((block_sum + samples / 2) / samples) as u8;
        if block.emission() > 0 {
            // 光源自身面的亮度不应比邻格低一档。
            block_light = block_light.max(block.emission());
        }
        let [r, g, b] = vertex_rgb(sky, block_light, face.shade, ao);

        mesh.vertices.push(Vertex {
            position: [min[0] + corner[0], min[1] + corner[1], min[2] + corner[2]],
            normal: face.normal,
            uv: tile_uv(tile, uv[0], uv[1]),
        });
        mesh.colors
            .get_or_insert_with(Vec::new)
            .push(VertexColor::new(r, g, b, 255));
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

    fn samples(mesh: &MeshData, normal: [f32; 3]) -> Vec<([f32; 3], [u8; 4])> {
        let colors = mesh.colors.as_ref().expect("区块网格应带顶点色");
        mesh.vertices
            .iter()
            .zip(colors)
            .filter(|(vertex, _)| vertex.normal == normal)
            .map(|(vertex, color)| (vertex.position, [color.r, color.g, color.b, color.a]))
            .collect()
    }

    fn top_face(mesh: &MeshData, x: i32, y: i32, z: i32) -> Vec<([f32; 3], [u8; 4])> {
        samples(mesh, [0.0, 1.0, 0.0])
            .into_iter()
            .filter(|(position, _)| {
                position[1] == (y + 1) as f32
                    && position[0] >= x as f32
                    && position[0] <= (x + 1) as f32
                    && position[2] >= z as f32
                    && position[2] <= (z + 1) as f32
            })
            .collect()
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

    /// 全露天单方块：六面颜色只由逐面明暗决定，天空光均为 15。
    #[test]
    fn open_sky_faces_follow_face_shades() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(IVec3::new(5, 5, 5), Block::Stone);
        let mesh = build_chunk_mesh(&world, IVec2::ZERO);
        assert_eq!(
            mesh.colors.as_ref().map(Vec::len),
            Some(mesh.vertices.len())
        );
        let expected = [
            ([0.0, 1.0, 0.0], [249, 255, 255, 255]),
            ([0.0, -1.0, 0.0], [124, 128, 128, 255]),
            ([-1.0, 0.0, 0.0], [164, 168, 168, 255]),
            ([1.0, 0.0, 0.0], [179, 184, 184, 255]),
            ([0.0, 0.0, -1.0], [203, 208, 208, 255]),
            ([0.0, 0.0, 1.0], [218, 224, 224, 255]),
        ];
        for (normal, color) in expected {
            let faces = samples(&mesh, normal);
            assert_eq!(faces.len(), 4, "单方块每面应四个顶点：{normal:?}");
            for (position, actual) in faces {
                assert_eq!(actual, color, "法线 {normal:?} 顶点 {position:?}");
            }
        }
    }

    /// 高一格的相邻墙把靠墙顶面顶点压到 AO 2，远离墙的顶点保持 AO 3。
    #[test]
    fn ao_darkens_contact_vertices() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(IVec3::new(5, 5, 5), Block::Stone);
        world.set_block(IVec3::new(6, 6, 5), Block::Stone);
        let mesh = build_chunk_mesh(&world, IVec2::ZERO);
        let top = top_face(&mesh, 5, 5, 5);
        assert_eq!(top.len(), 4);
        for (position, color) in top {
            if position[0] == 6.0 {
                assert_eq!(color, [212, 218, 218, 255], "靠墙顶点 {position:?}");
            } else {
                assert_eq!(color, [249, 255, 255, 255], "远离墙顶点 {position:?}");
            }
        }
    }

    /// 屋盖下的顶面天空光降到 13，颜色与查表结果一致。
    #[test]
    fn roof_shadow_darkens_top_face() {
        let mut open = VoxelWorld::empty(1);
        open.set_block(IVec3::new(5, 5, 5), Block::Stone);
        let open_mesh = build_chunk_mesh(&open, IVec2::ZERO);
        let open_color = top_face(&open_mesh, 5, 5, 5)
            .into_iter()
            .find(|(position, _)| position[0] == 5.0 && position[2] == 5.0)
            .expect("顶面等角顶点")
            .1;
        assert_eq!(open_color, [249, 255, 255, 255]);

        let mut roofed = VoxelWorld::empty(1);
        roofed.set_block(IVec3::new(5, 5, 5), Block::Stone);
        for z in 3..=7 {
            for x in 3..=7 {
                roofed.set_block(IVec3::new(x, 7, z), Block::Stone);
            }
        }
        let roofed_mesh = build_chunk_mesh(&roofed, IVec2::ZERO);
        let roofed_color = top_face(&roofed_mesh, 5, 5, 5)
            .into_iter()
            .find(|(position, _)| position[0] == 5.0 && position[2] == 5.0)
            .expect("顶面等角顶点")
            .1;
        assert_eq!(
            roofed_color,
            {
                let [r, g, b] = crate::logic::light::vertex_rgb(13, 0, 255, 3);
                [r, g, b, 255]
            },
            "屋盖下顶面应只剩天空光 13"
        );
        assert!(roofed_color[0] < open_color[0]);
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
