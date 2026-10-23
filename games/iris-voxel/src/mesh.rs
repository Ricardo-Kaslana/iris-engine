//! 区块网格生成与目标框网格。
//!
//! 逐方块检查六个面，只有相邻格不遮蔽时才发射该面。遮蔽的判据是
//! [`Block::is_opaque`] 而不是 `is_solid`：树叶是裁切方块，树叶与树叶
//! 之间不互相剔除，树叶旁边的石头也照常发射面——否则透过叶隙会看到
//! 背面剔除留下的空洞。相邻查询走 [`VoxelWorld::block`]，因此区块边界
//! 处会读取邻区块数据，接缝不会多出一层面。顶点使用区块局部坐标，
//! 实体的 `Transform` 放在区块原点。
//!
//! 方块分两套网格：不透明方块进 [`ChunkMeshes::opaque`]，树叶进
//! [`ChunkMeshes::cutout`]，后者用带 alpha 测试的材质绘制。
//!
//! 每个面的四个顶点独立采样平滑光照与 AO：在面外一格取前向格、两个
//! 切向边格与对角格，环境光遮蔽由后三格的不透明性决定，光照只平均
//! 透光格（空气与树叶；不透明格不参与，避免与 AO 双重变暗），再与
//! 逐面明暗一起经整数查表合成顶点色。

use glam::{IVec2, IVec3};
use iris_engine::prelude::{MeshData, Vertex, VertexColor};

use crate::atlas::tile_uv;
use crate::logic::block::{Block, Face, Tile};
use crate::logic::light::{block_tint, tint_rgb, vertex_rgb};
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

/// 一个区块的两套网格：不透明方块与裁切方块（树叶）。
///
/// 空分区是常态：多数区块没有树，`cutout` 会是空网格，接线层据此把
/// 对应的实体隐藏掉，不产生空绘制。
#[derive(Default)]
pub struct ChunkMeshes {
    pub opaque: MeshData,
    pub cutout: MeshData,
}

/// 生成一个区块的两套可见面网格（局部坐标）。区块坐标越界时返回空网格。
pub fn build_chunk_meshes(world: &VoxelWorld, coord: IVec2) -> ChunkMeshes {
    let mut meshes = ChunkMeshes::default();
    let Some(chunk) = world.chunk(coord) else {
        return meshes;
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
                let target = if block.is_cutout() {
                    &mut meshes.cutout
                } else {
                    &mut meshes.opaque
                };
                for face in &FACES {
                    let n = face.normal;
                    let neighbour = glam::IVec3::new(
                        world_x + n[0] as i32,
                        y as i32 + n[1] as i32,
                        world_z + n[2] as i32,
                    );
                    if world.block(neighbour).is_opaque() {
                        continue;
                    }
                    push_face(
                        target,
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
    meshes
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
    let tint = block_tint(block, block_pos);

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

        let blocked_a = world.block(side_a).is_opaque();
        let blocked_b = world.block(side_b).is_opaque();
        let blocked_corner = world.block(corner_cell).is_opaque();
        let ao = if blocked_a && blocked_b {
            0
        } else {
            3 - (blocked_a as u8 + blocked_b as u8 + blocked_corner as u8)
        };

        // 前向格由面剔除保证是透光格，至少有一个采样；不透明格不参与
        // 平均，AO 已经单独表达了它们的遮蔽。树叶算透光格——它自己有
        // 天空光等级，树冠内部的面才不会塌成全黑。
        let mut sky_sum = 0u16;
        let mut block_sum = 0u16;
        let mut samples = 0u16;
        for sample in [front, side_a, side_b, corner_cell] {
            if world.block(sample).light_cost().is_some() {
                sky_sum += light.sky(sample) as u16;
                block_sum += light.block_light(sample) as u16;
                samples += 1;
            }
        }
        debug_assert!(samples > 0, "面的前向格必须透光");
        let sky = ((sky_sum + samples / 2) / samples) as u8;
        let mut block_light = ((block_sum + samples / 2) / samples) as u8;
        if block.emission() > 0 {
            // 光源自身面的亮度不应比邻格低一档。
            block_light = block_light.max(block.emission());
        }
        let [r, g, b] = tint_rgb(vertex_rgb(sky, block_light, face.shade, ao), tint);

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

    /// 测试默认只看不透明分区；树叶分区由专门的用例读取。
    fn opaque_mesh(world: &VoxelWorld, coord: IVec2) -> MeshData {
        build_chunk_meshes(world, coord).opaque
    }

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
                let mesh = opaque_mesh(&world, IVec2::new(cx, cz));
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
        let mesh = opaque_mesh(&world, IVec2::ZERO);
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
        let mesh = opaque_mesh(&world, IVec2::ZERO);
        assert_eq!(count_faces(&mesh), 10);
    }

    /// 树叶之间**不**互相剔除：树冠内部要留一层内表面，透过叶隙才看得
    /// 到层叠的叶片而不是直穿过去的天空。矿石之间照旧剔除。
    #[test]
    fn leaves_do_not_cull_each_other() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(IVec3::new(5, 5, 5), Block::Leaves);
        world.set_block(IVec3::new(6, 5, 5), Block::Leaves);
        let meshes = build_chunk_meshes(&world, IVec2::ZERO);
        assert!(meshes.opaque.vertices.is_empty(), "树叶不进入不透明分区");
        assert_eq!(
            count_faces(&meshes.cutout),
            12,
            "相邻树叶的两个共享面都应发射"
        );

        let mut ores = VoxelWorld::empty(1);
        ores.set_block(IVec3::new(5, 5, 5), Block::CoalOre);
        ores.set_block(IVec3::new(6, 5, 5), Block::IronOre);
        let mesh = opaque_mesh(&ores, IVec2::ZERO);
        assert_eq!(count_faces(&mesh), 10, "相邻矿石的共享面应被剔除");
    }

    /// 不透明方块紧贴树叶时照常发射面：叶隙里能看到后面的石头。
    #[test]
    fn opaque_faces_are_not_culled_by_leaves() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(IVec3::new(5, 5, 5), Block::Stone);
        world.set_block(IVec3::new(6, 5, 5), Block::Leaves);
        let meshes = build_chunk_meshes(&world, IVec2::ZERO);
        assert_eq!(count_faces(&meshes.opaque), 6, "石头六面都要发射");
        assert_eq!(
            count_faces(&meshes.cutout),
            5,
            "树叶贴石头的那一面仍被石头遮蔽"
        );
    }

    /// 独立树叶方块进裁切分区，且六面都在。
    #[test]
    fn leaves_go_to_the_cutout_partition() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(IVec3::new(5, 5, 5), Block::Leaves);
        let meshes = build_chunk_meshes(&world, IVec2::ZERO);
        assert!(meshes.opaque.vertices.is_empty());
        assert_eq!(count_faces(&meshes.cutout), 6);
        assert_eq!(
            meshes.cutout.colors.as_ref().map(Vec::len),
            Some(meshes.cutout.vertices.len()),
            "裁切网格同样带顶点色"
        );
    }

    /// 树冠内部的面不能塌成纯黑：叶格自己带天空光，采样把它算进来。
    #[test]
    fn interior_leaf_faces_keep_their_light() {
        let mut world = VoxelWorld::empty(1);
        for x in 4..=8 {
            for z in 4..=8 {
                for y in 6..=8 {
                    world.set_block(IVec3::new(x, y, z), Block::Leaves);
                }
            }
        }
        let meshes = build_chunk_meshes(&world, IVec2::ZERO);
        let colors = meshes.cutout.colors.as_ref().expect("裁切网格应带顶点色");
        let darkest = colors
            .iter()
            .map(|color| color.r)
            .min()
            .expect("应有裁切面");
        // 树冠内部只有环境底光时红通道是 24 上下；叶格透光后应明显更亮。
        assert!(darkest > 40, "树冠内部的面过暗：{darkest}");
    }

    /// 新方块的图集槽位确实进入各自分区的网格 UV。
    #[test]
    fn new_block_tiles_reach_mesh_uvs() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(IVec3::new(2, 2, 2), Block::Leaves);
        world.set_block(IVec3::new(4, 2, 2), Block::GoldOre);
        let meshes = build_chunk_meshes(&world, IVec2::ZERO);
        let expected = [
            (&meshes.cutout, Tile::Leaves),
            (&meshes.opaque, Tile::GoldOre),
        ];
        for (mesh, tile) in expected {
            let uv = tile_uv(tile, 0.0, 0.0);
            let found = mesh.vertices.iter().any(|vertex| {
                (vertex.uv[0] - uv[0]).abs() < 1e-6 && (vertex.uv[1] - uv[1]).abs() < 1e-6
            });
            assert!(found, "{tile:?} 的 UV 没有进入对应分区：{uv:?}");
        }
    }

    /// 跨区块相邻：西侧区块的 +X 面被东侧区块的方块剔除。
    #[test]
    fn cross_chunk_neighbour_culls_face() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(IVec3::new(15, 5, 5), Block::Stone);
        world.set_block(IVec3::new(16, 5, 5), Block::Stone);
        let west = opaque_mesh(&world, IVec2::ZERO);
        let east = opaque_mesh(&world, IVec2::new(1, 0));
        assert_eq!(count_faces(&west), 5);
        assert_eq!(count_faces(&east), 5);
    }

    /// 全露天单方块：六面颜色只由逐面明暗决定，天空光均为 15。
    #[test]
    fn open_sky_faces_follow_face_shades() {
        let mut world = VoxelWorld::empty(1);
        world.set_block(IVec3::new(5, 5, 5), Block::Stone);
        let mesh = opaque_mesh(&world, IVec2::ZERO);
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
        let mesh = opaque_mesh(&world, IVec2::ZERO);
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
        let open_mesh = opaque_mesh(&open, IVec2::ZERO);
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
        let roofed_mesh = opaque_mesh(&roofed, IVec2::ZERO);
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
        let mesh = opaque_mesh(&world, IVec2::ZERO);
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
        let mesh = opaque_mesh(&world, IVec2::ZERO);
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
