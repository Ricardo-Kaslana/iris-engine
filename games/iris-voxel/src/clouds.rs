//! 方块云层（BLOCKWILD 风格）。
//!
//! 参考项目用一批扁平的长方体当云：两到三块拼接为一组，平顶、无光照、
//! 近白色，在高空缓慢横移。这里沿用同样的形态，位置由种子确定性派生，
//! 合并成一张网格一次提交——几十个盒子合成一个 draw call。
//!
//! 云不参与阴影、不写入体素世界，也不受天空光/方块光影响；材质走
//! unlit，颜色只随曝光变化。

use glam::Vec3;
use iris_engine::prelude::{MeshData, Vertex};

/// 云层基准高度（米）。世界地形最高约 40，云层留在其上方。
pub const CLOUD_BASE_Y: f32 = 72.0;
/// 云组分布半径（米），以世界中心为原点。
const CLOUD_RADIUS: f32 = 150.0;
/// 云组数量。
const CLOUD_GROUPS: u32 = 38;

/// 一组云由几个盒子拼成，盒子的中心与半尺寸都按种子抖动。
pub struct CloudSpec {
    pub center: Vec3,
    pub half: Vec3,
}

fn mix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn unit(seed: u64, salt: u64) -> f32 {
    (mix64(seed ^ salt.wrapping_mul(0xA24B_AED4_963E_E407)) >> 40) as f32 / 16_777_216.0
}

/// 生成云组列表：每组一宽一窄两块，宽块压在上方形成平顶。
pub fn cloud_specs(seed: u64) -> Vec<CloudSpec> {
    let center = 64.0f32;
    let mut specs = Vec::with_capacity(CLOUD_GROUPS as usize * 2);
    for index in 0..CLOUD_GROUPS {
        let key = index as u64;
        let angle = unit(seed, key * 3 + 1) * std::f32::consts::TAU;
        let radius = CLOUD_RADIUS * (0.50 + unit(seed, key * 3 + 2) * 0.50);
        let x = center + angle.cos() * radius;
        let z = center + angle.sin() * radius;
        let y = CLOUD_BASE_Y + unit(seed, key * 3 + 4) * 9.0;
        let width = 7.0 + unit(seed, key * 5 + 6) * 9.0;
        let depth = 4.0 + unit(seed, key * 5 + 7) * 4.0;
        let thickness = 2.0 + unit(seed, key * 5 + 8) * 1.2;
        specs.push(CloudSpec {
            center: Vec3::new(x, y, z),
            half: Vec3::new(width, thickness, depth),
        });
        // 副块：沿主块长边偏出一段，略薄略矮，做出云团的层次。
        let offset = Vec3::new(
            (unit(seed, key * 7 + 9) - 0.5) * width * 1.6,
            0.6 + unit(seed, key * 7 + 10) * 0.8,
            (unit(seed, key * 7 + 11) - 0.5) * depth * 1.2,
        );
        specs.push(CloudSpec {
            center: Vec3::new(x, y, z) + offset,
            half: Vec3::new(
                width * (0.45 + unit(seed, key * 7 + 12) * 0.3),
                thickness * 0.75,
                depth * (0.6 + unit(seed, key * 7 + 13) * 0.4),
            ),
        });
    }
    specs
}

/// 把所有云组合并成一张网格。顶点法线只用于着色管线占位，云材质 unlit。
pub fn cloud_mesh(seed: u64) -> MeshData {
    let mut mesh = MeshData::default();
    for spec in cloud_specs(seed) {
        push_box(&mut mesh, spec.center - spec.half, spec.center + spec.half);
    }
    mesh
}

/// 六个面的角点顺序与法线（与区块网格保持同一套绕序与坐标约定）。
const FACES: [([f32; 3], [[f32; 3]; 4]); 6] = [
    (
        [0.0, 1.0, 0.0],
        [
            [0.0, 1.0, 1.0],
            [1.0, 1.0, 1.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ],
    ),
    (
        [0.0, -1.0, 0.0],
        [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 1.0],
            [0.0, 0.0, 1.0],
        ],
    ),
    (
        [1.0, 0.0, 0.0],
        [
            [1.0, 0.0, 1.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [1.0, 1.0, 1.0],
        ],
    ),
    (
        [-1.0, 0.0, 0.0],
        [
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 1.0, 1.0],
            [0.0, 1.0, 0.0],
        ],
    ),
    (
        [0.0, 0.0, 1.0],
        [
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ],
    ),
    (
        [0.0, 0.0, -1.0],
        [
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
        ],
    ),
];

fn push_box(mesh: &mut MeshData, min: Vec3, max: Vec3) {
    for (normal, corners) in FACES {
        let base = mesh.vertices.len() as u32;
        for corner in corners {
            mesh.vertices.push(Vertex {
                position: [
                    if corner[0] == 0.0 { min.x } else { max.x },
                    if corner[1] == 0.0 { min.y } else { max.y },
                    if corner[2] == 0.0 { min.z } else { max.z },
                ],
                normal,
                uv: [0.0, 0.0],
            });
        }
        mesh.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// 云层不进入体素世界，但验收脚本要判断镜头是否朝向云。
pub fn cloud_bounds(seed: u64) -> (Vec3, Vec3) {
    let specs = cloud_specs(seed);
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    for spec in &specs {
        min = min.min(spec.center - spec.half);
        max = max.max(spec.center + spec.half);
    }
    (min, max)
}

/// 诊断用：云网格占用的方块数（每个盒子 12 个三角形）。
pub fn cloud_triangle_count(seed: u64) -> usize {
    cloud_specs(seed).len() * 12
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clouds_sit_above_the_terrain() {
        const MAX_TERRAIN: f32 = 48.0;
        for spec in cloud_specs(20_261_022) {
            assert!(
                spec.center.y - spec.half.y > MAX_TERRAIN,
                "云块压到了地形上：{:?}",
                spec.center
            );
        }
    }

    #[test]
    fn cloud_mesh_is_two_boxes_per_group_and_deterministic() {
        let a = cloud_mesh(7);
        let b = cloud_mesh(7);
        let positions = |mesh: &MeshData| {
            mesh.vertices
                .iter()
                .map(|vertex| vertex.position)
                .collect::<Vec<_>>()
        };
        assert_eq!(positions(&a), positions(&b));
        assert_eq!(a.indices, b.indices);
        assert_eq!(a.indices.len(), cloud_triangle_count(7) * 3);
        assert_eq!(a.vertices.len(), cloud_specs(7).len() * 24);
    }

    #[test]
    fn cloud_boxes_have_positive_extent() {
        for spec in cloud_specs(99) {
            assert!(spec.half.min_element() > 0.0);
        }
    }

    #[test]
    fn cloud_positions_avoid_the_spawn_column() {
        // 出生点正上方留空，抬头不会正对一块云。
        let spawn = Vec3::new(64.0, 0.0, 64.0);
        for spec in cloud_specs(20_261_022) {
            let horizontal = Vec3::new(spec.center.x - spawn.x, 0.0, spec.center.z - spawn.z);
            assert!(
                horizontal.length() > 6.0,
                "云块悬在出生点正上方：{:?}",
                spec.center
            );
        }
    }
}
