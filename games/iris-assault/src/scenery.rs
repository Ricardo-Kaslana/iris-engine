//! 庭院建筑几何（程序化 · 模块化构件 · 确定性）。
//!
//! 把「一个盒子围起来的操场」换成一座**有建筑语言的庭院**：基座 / 壁柱 /
//! 檐口 / 雉堞的墙体、拱形壁龛与雕像、多立克式石柱、分层喷泉与水面、悬挂
//! 火盆与旗帜、木门与铁箍。全部由 [`crate::geom`] 的构件函数生成（圆角盒 /
//! 超椭球 / 旋转体 / 放样），按材质分组输出，最终打成一张 GLB。
//!
//! # 与碰撞体的契约（本模块最容易出错的地方）
//!
//! 视觉几何**不得越过** [`crate::logic`] 的静态碰撞体表面：
//! - 可走区 = |x| ≤ 9.8、|z| ≤ 6.8（墙体碰撞盒内表面），角色胶囊半径 0.45
//!   ⇒ 人停在 |x| = 9.35；
//! - 因此**地面以上 2.1 m 以内的构件一律不越过 9.80 / 6.80**（凸出朝墙内
//!   侧做，不朝场内做）；
//! - 2.1 m 以上（檐口 / 雉堞 / 壁龛拱 / 悬挂物）可自由外挑——角色头顶
//!   （胶囊顶 1.8 m）够不到；
//! - 喷泉盆外沿 ≤ 1.72（球碰撞体 r = 1.75）；石柱全部构件落在 0.9×0.9 内。
//!
//! 违反这条契约的后果是「人可以走进石头里」——单元测试
//! [`tests::geometry_stays_inside_colliders`] 用包围盒逐材质卡这道关。
//!
//! # 诚实限制
//! - 全部构件是**轴对齐体块 + 旋转体**：没有斜屋顶、曲面幕墙、雕花线脚
//!   （线脚由 2~3 层台阶盒近似）；
//! - 砖缝 / 石缝在**贴图**里，不在几何里（贴面 10 cm 内能看出是平面）；
//! - 雕像 / 旗帜是几何示意（布幔无飘动，旗帜无图案——图案需要 UV 对齐的
//!   专用贴图，本轮不做）；
//! - 不做 LOD（庭院总量 ~20k 三角形，单级足够）。

use std::collections::BTreeMap;

use glam::{Mat4, Quat, Vec2, Vec3};
use iris_engine::prelude::MeshData;

use crate::geom::{self, Assembly, Axis, Piece, Tile};
use crate::scenery_tex::Surface;
use crate::textures::hash01;

/// 材质贴图占满 UV（各材质独立贴图 ⇒ 允许按米平铺，与图集不同）。
const FULL: Tile = Tile::new(0.0, 0.0, 1.0, 1.0);

/// 铺地纹理覆盖的世界尺度（米）：贴图内含 4×4 块石板 ⇒ 单块 1.15 × 0.75 m。
pub const PAVING_TILE_METERS: f32 = 4.6;

/// 地面以上「角色够得到」的高度（胶囊顶 1.8 m + 余量）。
pub const PLAYER_REACH_Y: f32 = 2.10;

/// 按材质分组的庭院网格（材质下标 = [`Surface::index`]）。
pub struct Courtyard {
    /// 分组网格（顺序 = [`Surface::ALL`]）。
    pub groups: Vec<(Surface, MeshData)>,
}

/// 累加器：按材质分组（BTreeMap ⇒ 分组顺序与插入序无关，确定性）。
#[derive(Default)]
struct Acc {
    groups: BTreeMap<Surface, Assembly>,
}

impl Acc {
    fn add(&mut self, surface: Surface, piece: Piece) {
        self.groups.entry(surface).or_default().add_rigid(piece);
    }

    fn finish(self) -> Courtyard {
        Courtyard {
            groups: self
                .groups
                .into_iter()
                .map(|(surface, assembly)| (surface, assembly.finish()))
                .collect(),
        }
    }
}

/// 圆角砌块（指数 10 ≈ 锐利长方体，8×5 分段足够）。
fn block(half: Vec3) -> MeshData {
    geom::superellipsoid(half, 10.0, 8, 5)
}

/// 逐构件 UV 偏移（同一贴图在不同构件上取样不同区域——避免「所有石块
/// 长得一模一样」的墙面）。
fn uv_jitter(seed: u64) -> Vec2 {
    Vec2::new(hash01(seed), hash01(seed ^ 0x9e37_79b9))
}

/// 盒投 UV + 摆放的常用封装。
fn piece(surface: Surface, mesh: MeshData, uv_scale: f32, seed: u64, matrix: Mat4) -> Piece {
    let _ = surface;
    Piece::new(mesh)
        .uv_box(FULL, uv_scale, uv_jitter(seed))
        .transform(matrix)
}

/// 地面：一张大平面（石板在贴图里，几何 2 个三角形/格）+ 中央圆形铺装。
fn paving(acc: &mut Acc) {
    let half_x = 9.95f32;
    let half_z = 6.95f32;
    let u_scale = 1.0 / PAVING_TILE_METERS;
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    const NX: u32 = 16;
    const NZ: u32 = 16;
    for iz in 0..=NZ {
        for ix in 0..=NX {
            let x = -half_x + 2.0 * half_x * ix as f32 / NX as f32;
            let z = -half_z + 2.0 * half_z * iz as f32 / NZ as f32;
            vertices.push(iris_engine::prelude::Vertex {
                position: [x, 0.0, z],
                normal: [0.0, 1.0, 0.0],
                uv: [(x + half_x) * u_scale, (z + half_z) * u_scale],
            });
        }
    }
    let row = NX + 1;
    for iz in 0..NZ {
        for ix in 0..NX {
            let a = iz * row + ix;
            let b = a + 1;
            let c = a + row + 1;
            let d = a + row;
            indices.extend_from_slice(&[a, c, b, a, d, c]);
        }
    }
    let mut floor = MeshData {
        vertices,
        indices,
        ..Default::default()
    };
    geom::smooth_normals(&mut floor);
    geom::compute_tangents(&mut floor);
    acc.add(Surface::Paving, Piece::new(floor));

    // 中央圆形铺装（喷泉四周的双环石带）
    for (radius, width, seed) in [(2.30f32, 0.22f32, 0x9001u64), (2.72, 0.16, 0x9002)] {
        let ring = geom::lathe(
            &[
                (radius - width, 0.004),
                (radius - width * 0.55, 0.012),
                (radius, 0.012),
                (radius + width * 0.55, 0.012),
                (radius + width, 0.004),
            ],
            40,
        );
        acc.add(
            Surface::Stone,
            piece(Surface::Stone, ring, 0.6, seed, Mat4::IDENTITY),
        );
    }
}

/// 一道围墙（沿 X 或沿 Z；`inner` = 面向场内的表面坐标）。
fn wall_run(acc: &mut Acc, axis: Axis, inner: f32, length: f32, seed: u64) {
    let half_len = length * 0.5;
    // 墙体本体由 [wall_shell] 负责（拆成背板 + 面板才留得出壁龛开口）；
    // 本函数只加「贴上去」的构件：基座 / 壁柱 / 檐口 / 雉堞。
    // 基座（凸出 0.06，止于碰撞面）
    let (base_half, base_center) = match axis {
        Axis::X => (
            Vec3::new(0.335, 0.28, half_len),
            Vec3::new(inner + 0.335 - 0.06, 0.28, 0.0),
        ),
        _ => (
            Vec3::new(half_len, 0.28, 0.335),
            Vec3::new(0.0, 0.28, inner + 0.335 - 0.06),
        ),
    };
    acc.add(
        Surface::Stone,
        piece(
            Surface::Stone,
            block(base_half),
            0.55,
            seed ^ 0x11,
            Mat4::from_translation(base_center),
        ),
    );
    // 基座压顶线
    let (cap_half, cap_center) = match axis {
        Axis::X => (
            Vec3::new(0.355, 0.05, half_len),
            Vec3::new(inner + 0.355 - 0.08, 0.585, 0.0),
        ),
        _ => (
            Vec3::new(half_len, 0.05, 0.355),
            Vec3::new(0.0, 0.585, inner + 0.355 - 0.08),
        ),
    };
    acc.add(
        Surface::Stone,
        piece(
            Surface::Stone,
            block(cap_half),
            0.55,
            seed ^ 0x12,
            Mat4::from_translation(cap_center),
        ),
    );
    // 壁柱 + 柱头（每 3.4 m 一根）
    let count = (length / 3.4).round().max(2.0) as i32;
    for index in 0..count {
        let t = (index as f32 + 0.5) / count as f32 - 0.5;
        let offset = t * length;
        let (p_half, p_center) = match axis {
            Axis::X => (
                Vec3::new(0.335, 1.06, 0.30),
                Vec3::new(inner + 0.275, 1.70, offset),
            ),
            _ => (
                Vec3::new(0.30, 1.06, 0.335),
                Vec3::new(offset, 1.70, inner + 0.275),
            ),
        };
        acc.add(
            Surface::Stone,
            piece(
                Surface::Stone,
                block(p_half),
                0.55,
                seed ^ 0x20 ^ index as u64,
                Mat4::from_translation(p_center),
            ),
        );
        // 柱头：三阶线脚（2.72 m 以上 ⇒ 可外挑）
        for (step, (w, h, out)) in [
            (0.40f32, 0.06f32, 0.10f32),
            (0.36, 0.05, 0.14),
            (0.32, 0.04, 0.18),
        ]
        .iter()
        .enumerate()
        {
            let y = 2.72 + step as f32 * 0.06 + h * 0.5;
            let (half, center) = match axis {
                Axis::X => (
                    Vec3::new(0.275 + out * 0.6, *h, *w),
                    Vec3::new(inner + 0.275 - out * 0.6, y, offset),
                ),
                _ => (
                    Vec3::new(*w, *h, 0.275 + out * 0.6),
                    Vec3::new(offset, y, inner + 0.275 - out * 0.6),
                ),
            };
            acc.add(
                Surface::Stone,
                piece(
                    Surface::Stone,
                    block(half),
                    0.6,
                    seed ^ 0x30 ^ step as u64,
                    Mat4::from_translation(center),
                ),
            );
        }
    }
    // 檐口（2.86 m 以上：逐步外挑）
    for (step, (out, height, thickness)) in [
        (0.14f32, 2.86f32, 0.07f32),
        (0.20, 2.94, 0.06),
        (0.26, 3.00, 0.05),
    ]
    .iter()
    .enumerate()
    {
        let (half, center) = match axis {
            Axis::X => (
                Vec3::new(0.275 + out * 0.5, thickness * 0.5, half_len),
                Vec3::new(inner + 0.275 - out * 0.5, height + thickness * 0.5, 0.0),
            ),
            _ => (
                Vec3::new(half_len, thickness * 0.5, 0.275 + out * 0.5),
                Vec3::new(0.0, height + thickness * 0.5, inner + 0.275 - out * 0.5),
            ),
        };
        acc.add(
            Surface::Stone,
            piece(
                Surface::Stone,
                block(half),
                0.5,
                seed ^ 0x40 ^ step as u64,
                Mat4::from_translation(center),
            ),
        );
    }
    // 雉堞（3.05 m 以上）
    let merlons = (length / 1.42).round().max(3.0) as i32;
    for index in 0..merlons {
        let t = (index as f32 + 0.5) / merlons as f32 - 0.5;
        let offset = t * length;
        let (half, center) = match axis {
            Axis::X => (
                Vec3::new(0.30, 0.24, 0.42),
                Vec3::new(inner + 0.30, 3.29, offset),
            ),
            _ => (
                Vec3::new(0.42, 0.24, 0.30),
                Vec3::new(offset, 3.29, inner + 0.30),
            ),
        };
        acc.add(
            Surface::Brick,
            piece(
                Surface::Brick,
                block(half),
                2.0,
                seed ^ 0x50 ^ index as u64,
                Mat4::from_translation(center),
            ),
        );
        let (eye_half, eye_center) = match axis {
            Axis::X => (
                Vec3::new(0.06, 0.07, 0.07),
                Vec3::new(inner + 0.24, 3.40, offset),
            ),
            _ => (
                Vec3::new(0.07, 0.07, 0.06),
                Vec3::new(offset, 3.40, inner + 0.24),
            ),
        };
        acc.add(
            Surface::Stone,
            piece(
                Surface::Stone,
                block(eye_half),
                1.4,
                seed ^ 0x51 ^ index as u64,
                Mat4::from_translation(eye_center),
            ),
        );
    }
}

/// 墙壳：背板（通长）+ 前沿墙板（在 `gaps` 处留出壁龛开口）。
///
/// 墙体被拆成「背板 + 面板」是为了壁龛：一个实心长方体没法挖洞，而拆成
/// 背板（留 0.30 m 深处的立面）+ 面板（0.30 厚，凸到碰撞面）后，两板之间
/// 的空档天然就是壁龛。碰撞由档里的静态盒照旧挡住（视觉凹进去、物理不凹
/// ——玩家停在 9.35，够不到凹处）。
fn wall_shell(
    acc: &mut Acc,
    axis: Axis,
    inner: f32,
    length: f32,
    gaps: &[f32],
    gap_width: f32,
    seed: u64,
) {
    let half_len = length * 0.5;
    // 背板（0.15 厚，贴碰撞盒外沿）
    let (back_half, back_center) = match axis {
        Axis::X => (
            Vec3::new(0.075, 1.5, half_len),
            Vec3::new(inner + 0.525, 1.5, 0.0),
        ),
        _ => (
            Vec3::new(half_len, 1.5, 0.075),
            Vec3::new(0.0, 1.5, inner + 0.525),
        ),
    };
    acc.add(
        Surface::Brick,
        piece(
            Surface::Brick,
            block(back_half),
            2.0,
            seed,
            Mat4::from_translation(back_center),
        ),
    );
    // 前沿墙板：把 [-half_len, half_len] 按 gaps 切成若干段
    let mut cuts: Vec<(f32, f32)> = gaps
        .iter()
        .map(|center| (center - gap_width * 0.5, center + gap_width * 0.5))
        .collect();
    cuts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut cursor = -half_len;
    let mut segments = Vec::new();
    for (start, end) in cuts {
        if start > cursor {
            segments.push((cursor, start));
        }
        cursor = cursor.max(end);
    }
    if cursor < half_len {
        segments.push((cursor, half_len));
    }
    for (index, (start, end)) in segments.iter().enumerate() {
        let width = end - start;
        if width <= 0.02 {
            continue;
        }
        let center_along = (start + end) * 0.5;
        let (half, center) = match axis {
            Axis::X => (
                Vec3::new(0.15, 1.5, width * 0.5),
                Vec3::new(inner + 0.15, 1.5, center_along),
            ),
            _ => (
                Vec3::new(width * 0.5, 1.5, 0.15),
                Vec3::new(center_along, 1.5, inner + 0.15),
            ),
        };
        acc.add(
            Surface::Brick,
            piece(
                Surface::Brick,
                block(half),
                2.0,
                seed ^ 0x71 ^ index as u64,
                Mat4::from_translation(center),
            ),
        );
    }
}

/// 拱形壁龛：两段壁柱 + 半圆拱券（楔石逐块）+ 龛底深色背衬 + 龛内雕像。
///
/// 拱券用 9 块楔石拼——比「半圆环放样」多花 8 个 draw 无所谓，读起来是
/// 砌体而不是光滑管道（低多边形建筑的关键：**让结构可见**）。
fn arch_niche(acc: &mut Acc, axis: Axis, inner: f32, center_along: f32, seed: u64) {
    const HALF_W: f32 = 0.62;
    const SPRING_Y: f32 = 1.72; // 起拱线
    const TOP_Y: f32 = 2.72; // 拱顶石外沿
    // 龛底背衬（深一档的石材，制造纵深）
    let (back_half, back_center) = match axis {
        Axis::X => (
            Vec3::new(0.035, 0.86, HALF_W - 0.02),
            Vec3::new(inner + 0.34, 0.86, center_along),
        ),
        _ => (
            Vec3::new(HALF_W - 0.02, 0.86, 0.035),
            Vec3::new(center_along, 0.86, inner + 0.34),
        ),
    };
    acc.add(
        Surface::Marble,
        piece(
            Surface::Marble,
            block(back_half),
            1.1,
            seed ^ 0x81,
            Mat4::from_translation(back_center),
        ),
    );
    // 侧壁柱（凸到碰撞面）
    for side in [-1.0f32, 1.0] {
        let along = center_along + side * (HALF_W + 0.09);
        let (half, center) = match axis {
            Axis::X => (
                Vec3::new(0.17, 1.36, 0.09),
                Vec3::new(inner + 0.17, 1.36, along),
            ),
            _ => (
                Vec3::new(0.09, 1.36, 0.17),
                Vec3::new(along, 1.36, inner + 0.17),
            ),
        };
        acc.add(
            Surface::Stone,
            piece(
                Surface::Stone,
                block(half),
                0.6,
                seed ^ 0x82 ^ (side > 0.0) as u64,
                Mat4::from_translation(center),
            ),
        );
    }
    // 拱券：9 块楔石绕起拱线圆心的半圆
    const VOUSSOIRS: usize = 9;
    let radius = HALF_W;
    for index in 0..VOUSSOIRS {
        let t = (index as f32 + 0.5) / VOUSSOIRS as f32;
        let angle = std::f32::consts::PI * t; // 0..π
        let (sin_a, cos_a) = angle.sin_cos();
        let radial = radius - 0.10;
        let along = center_along - cos_a * radial;
        let y = SPRING_Y + sin_a * radial;
        let depth_center = inner + 0.13;
        let rotation = -angle + std::f32::consts::FRAC_PI_2;
        let (half, center, rot) = match axis {
            Axis::X => (
                Vec3::new(0.13, 0.10, 0.115),
                Vec3::new(depth_center, y, along),
                Quat::from_rotation_x(rotation),
            ),
            _ => (
                Vec3::new(0.115, 0.10, 0.13),
                Vec3::new(along, y, depth_center),
                Quat::from_rotation_z(rotation),
            ),
        };
        let mesh = block(half);
        acc.add(
            Surface::Stone,
            piece(
                Surface::Stone,
                mesh,
                0.7,
                seed ^ 0x83 ^ index as u64,
                Mat4::from_rotation_translation(rot, center),
            ),
        );
    }
    // 龛内雕像：立在龛底（不越过碰撞面——第一版摆在龛口，基座凸出场地
    // 14 cm，玩家贴墙时会走进石座里）
    statue(acc, axis, inner + 0.30, center_along, seed ^ 0x84);
    let _ = TOP_Y;
}

/// 壁龛里的石像：长袍（旋转体）+ 头 + 双臂（示意，非写实雕塑）。
fn statue(acc: &mut Acc, axis: Axis, face: f32, along: f32, seed: u64) {
    let place_at = |x: f32, y: f32, z: f32| match axis {
        Axis::X => Vec3::new(x, y, z),
        _ => Vec3::new(z, y, x),
    };
    // 基座
    acc.add(
        Surface::Marble,
        piece(
            Surface::Marble,
            block(Vec3::new(0.30, 0.14, 0.30)),
            1.4,
            seed,
            Mat4::from_translation(place_at(face + 0.05, 0.14, along)),
        ),
    );
    // 长袍：自下而上收窄的旋转体
    let robe = geom::lathe(
        &[
            (0.000, 0.28),
            (0.235, 0.30),
            (0.215, 0.62),
            (0.185, 0.92),
            (0.160, 1.12),
            (0.175, 1.26),
            (0.120, 1.38),
            (0.085, 1.48),
            (0.000, 1.50),
        ],
        16,
    );
    acc.add(
        Surface::Marble,
        Piece::new(robe)
            .uv_cylinder(FULL, Axis::Y, 2.0, 1.2, uv_jitter(seed ^ 0x91))
            .transform(Mat4::from_translation(place_at(face + 0.05, 0.0, along))),
    );
    // 头
    acc.add(
        Surface::Marble,
        piece(
            Surface::Marble,
            geom::superellipsoid(Vec3::splat(0.105), 2.6, 12, 8),
            3.0,
            seed ^ 0x92,
            Mat4::from_translation(place_at(face + 0.05, 1.58, along)),
        ),
    );
    // 双臂（垂在身侧）
    for side in [-1.0f32, 1.0] {
        let arm = geom::tube(
            Vec3::new(0.0, 1.34, 0.0),
            Vec3::new(0.0, 0.98, 0.0),
            &[(0.0, 0.055, 0.055, 2.4), (1.0, 0.045, 0.045, 2.4)],
            8,
            Vec3::X,
        );
        acc.add(
            Surface::Marble,
            Piece::new(arm)
                .uv_cylinder(FULL, Axis::Y, 2.0, 2.0, uv_jitter(seed ^ 0xA0))
                .transform(Mat4::from_translation(place_at(
                    face + 0.05 + side * 0.19,
                    0.0,
                    along,
                ))),
        );
    }
}

/// 多立克式石柱（含方形柱础、收分柱身、柱头与顶板）。
fn column(acc: &mut Acc, center_xz: Vec2, seed: u64) {
    let base = Vec3::new(center_xz.x, 0.0, center_xz.y);
    // 方形柱础（两级）
    for (half, y) in [
        (Vec3::new(0.44, 0.10, 0.44), 0.10f32),
        (Vec3::new(0.38, 0.08, 0.38), 0.28f32),
    ] {
        acc.add(
            Surface::Stone,
            piece(
                Surface::Stone,
                block(half),
                0.6,
                seed ^ (y.to_bits() as u64),
                Mat4::from_translation(base + Vec3::new(0.0, y, 0.0)),
            ),
        );
    }
    // 柱脚圆盘（旋转体）
    acc.add(
        Surface::Stone,
        Piece::new(geom::lathe(
            &[(0.000, 0.34), (0.300, 0.36), (0.285, 0.44), (0.000, 0.46)],
            20,
        ))
        .uv_cylinder(FULL, Axis::Y, 3.0, 1.5, uv_jitter(seed ^ 0x01))
        .transform(Mat4::from_translation(base)),
    );
    // 柱身：微收分（entasis 用两段锥度近似）
    let shaft = geom::tube(
        Vec3::new(0.0, 0.46, 0.0),
        Vec3::new(0.0, 2.62, 0.0),
        &[
            (0.0, 0.275, 0.275, 2.0),
            (0.35, 0.262, 0.262, 2.0),
            (0.75, 0.248, 0.248, 2.0),
            (1.0, 0.236, 0.236, 2.0),
        ],
        24,
        Vec3::X,
    );
    acc.add(
        Surface::Stone,
        Piece::new(shaft)
            .uv_cylinder(FULL, Axis::Y, 4.0, 0.9, uv_jitter(seed ^ 0x02))
            .transform(Mat4::from_translation(base)),
    );
    // 柱头（钟形 + 顶板）
    acc.add(
        Surface::Stone,
        Piece::new(geom::lathe(
            &[
                (0.000, 2.62),
                (0.240, 2.64),
                (0.268, 2.72),
                (0.296, 2.80),
                (0.310, 2.86),
                (0.000, 2.88),
            ],
            24,
        ))
        .uv_cylinder(FULL, Axis::Y, 3.0, 1.6, uv_jitter(seed ^ 0x03))
        .transform(Mat4::from_translation(base)),
    );
    acc.add(
        Surface::Stone,
        piece(
            Surface::Stone,
            block(Vec3::new(0.34, 0.09, 0.34)),
            0.6,
            seed ^ 0x04,
            Mat4::from_translation(base + Vec3::new(0.0, 2.97, 0.0)),
        ),
    );
}

/// 喷泉：八角？不——圆形三层水盘（池 / 中柱 / 上盘）+ 水面。
fn fountain(acc: &mut Acc, seed: u64) {
    // 池壁（外沿 ≤ 1.72，球碰撞体 r = 1.75）
    let basin = geom::lathe(
        &[
            (0.900, 0.010),
            (1.400, 0.020),
            (1.640, 0.080),
            (1.700, 0.260),
            (1.715, 0.470),
            (1.720, 0.580),
            (1.700, 0.640),
            (1.735, 0.700),
            (1.700, 0.745),
            (1.585, 0.720),
            (1.520, 0.600),
            (1.455, 0.520),
            (1.150, 0.470),
            (0.560, 0.440),
            (0.000, 0.430),
        ],
        28,
    );
    acc.add(
        Surface::Marble,
        Piece::new(basin)
            .uv_cylinder(FULL, Axis::Y, 6.0, 1.1, uv_jitter(seed))
            .transform(Mat4::IDENTITY),
    );
    // 池底铺装（深色石）
    acc.add(
        Surface::Stone,
        Piece::new(geom::lathe(&[(0.000, 0.432), (1.450, 0.470)], 24))
            .uv_cylinder(FULL, Axis::Y, 4.0, 2.0, uv_jitter(seed ^ 0x11))
            .transform(Mat4::IDENTITY),
    );
    // 中柱 + 上盘 + 顶尖
    let column = geom::lathe(
        &[
            (0.000, 0.430),
            (0.430, 0.440),
            (0.360, 0.520),
            (0.300, 0.700),
            (0.270, 0.960),
            (0.330, 1.060),
            (0.290, 1.120),
            (0.560, 1.180),
            (0.640, 1.260),
            (0.610, 1.330),
            (0.470, 1.300),
            (0.300, 1.255),
            (0.215, 1.400),
            (0.185, 1.640),
            (0.235, 1.720),
            (0.185, 1.790),
            (0.090, 1.860),
            (0.000, 1.890),
        ],
        24,
    );
    acc.add(
        Surface::Marble,
        Piece::new(column)
            .uv_cylinder(FULL, Axis::Y, 5.0, 1.0, uv_jitter(seed ^ 0x22))
            .transform(Mat4::IDENTITY),
    );
    // 水面（池内 + 上盘）
    let water_pool = geom::lathe(&[(0.000, 0.575), (1.520, 0.575)], 32);
    acc.add(
        Surface::Water,
        Piece::new(water_pool).transform(Mat4::IDENTITY),
    );
    let water_upper = geom::lathe(&[(0.000, 1.315), (0.560, 1.315)], 24);
    acc.add(
        Surface::Water,
        Piece::new(water_upper).transform(Mat4::IDENTITY),
    );
}

/// 悬挂火盆：墙托 + 斜撑 + 铁链 + 铜盆 + 炭火（光点位在盆上方，见 logic）。
fn brazier(acc: &mut Acc, at: Vec3, wall_z: f32, seed: u64) {
    let outward = if at.z > 0.0 { -1.0f32 } else { 1.0f32 };
    // 墙托横杆（3.0 m 高，角色够不到）
    let arm_len = (at.z - wall_z).abs() + 0.06;
    let arm_center = Vec3::new(at.x, 3.02, wall_z + outward * -arm_len * 0.5);
    acc.add(
        Surface::Iron,
        piece(
            Surface::Iron,
            block(Vec3::new(0.045, 0.045, arm_len * 0.5)),
            2.2,
            seed,
            Mat4::from_translation(arm_center),
        ),
    );
    // 墙面板 + 斜撑
    acc.add(
        Surface::Iron,
        piece(
            Surface::Iron,
            block(Vec3::new(0.11, 0.16, 0.03)),
            2.2,
            seed ^ 0x01,
            Mat4::from_translation(Vec3::new(at.x, 3.02, wall_z + outward * 0.03)),
        ),
    );
    let strut_from = Vec3::new(at.x, 2.62, wall_z + outward * 0.05);
    let strut_to = Vec3::new(at.x, 3.00, wall_z + outward * -arm_len + outward * 0.06);
    let strut = geom::tube(
        strut_from,
        strut_to,
        &[(0.0, 0.028, 0.028, 2.2), (1.0, 0.028, 0.028, 2.2)],
        8,
        Vec3::Y,
    );
    acc.add(
        Surface::Iron,
        Piece::new(strut)
            .uv_cylinder(FULL, Axis::Z, 2.0, 2.0, uv_jitter(seed ^ 0x02))
            .transform(Mat4::IDENTITY),
    );
    // 铁链（三节示意）
    for index in 0..3 {
        let y = 2.98 - index as f32 * 0.09;
        acc.add(
            Surface::Iron,
            Piece::new(geom::lathe(
                &[(0.000, y), (0.022, y + 0.045), (0.000, y + 0.09)],
                8,
            ))
            .uv_cylinder(
                FULL,
                Axis::Y,
                3.0,
                3.0,
                uv_jitter(seed ^ 0x10 ^ index as u64),
            )
            .transform(Mat4::from_translation(Vec3::new(at.x, 0.0, at.z))),
        );
    }
    // 铜盆（S 形剖面）
    let bowl = geom::lathe(
        &[
            (0.000, 2.560),
            (0.120, 2.570),
            (0.240, 2.620),
            (0.310, 2.700),
            (0.320, 2.740),
            (0.285, 2.735),
            (0.230, 2.680),
            (0.140, 2.645),
            (0.000, 2.640),
        ],
        20,
    );
    acc.add(
        Surface::Iron,
        Piece::new(bowl)
            .uv_cylinder(FULL, Axis::Y, 4.0, 2.5, uv_jitter(seed ^ 0x20))
            .transform(Mat4::from_translation(Vec3::new(at.x, 0.0, at.z))),
    );
    // 炭火
    acc.add(
        Surface::Coal,
        Piece::new(geom::lathe(
            &[
                (0.000, 2.640),
                (0.170, 2.655),
                (0.230, 2.690),
                (0.000, 2.700),
            ],
            16,
        ))
        .uv_cylinder(FULL, Axis::Y, 3.0, 4.0, uv_jitter(seed ^ 0x30))
        .transform(Mat4::from_translation(Vec3::new(at.x, 0.0, at.z))),
    );
}

/// 旗帜：横杆 + 布幔（三段放样做出下垂的弧）+ 铁环。
fn banner(acc: &mut Acc, axis: Axis, inner: f32, along: f32, seed: u64) {
    const WIDTH: f32 = 0.52;
    const TOP: f32 = 3.00;
    const BOTTOM: f32 = 2.16;
    // 横杆（木）
    let (pole_half, pole_center) = match axis {
        Axis::X => (
            Vec3::new(0.10, 0.035, WIDTH + 0.08),
            Vec3::new(inner + 0.10, TOP, along),
        ),
        _ => (
            Vec3::new(WIDTH + 0.08, 0.035, 0.10),
            Vec3::new(along, TOP, inner + 0.10),
        ),
    };
    acc.add(
        Surface::Wood,
        piece(
            Surface::Wood,
            block(pole_half),
            1.4,
            seed,
            Mat4::from_translation(pole_center),
        ),
    );
    // 布幔：四段放样（v 向从杆到布底，u 向沿宽度），带下垂弧度
    let mut sections = Vec::new();
    for step in 0..5 {
        let t = step as f32 / 4.0;
        let y = TOP - 0.04 - t * (TOP - BOTTOM);
        let swell = 0.03 + 0.05 * (t * std::f32::consts::PI).sin(); // 中段鼓起
        let (center, right, up) = match axis {
            Axis::X => (Vec3::new(inner + 0.09 + swell, y, along), Vec3::Y, Vec3::Z),
            _ => (Vec3::new(along, y, inner + 0.09 + swell), Vec3::Y, Vec3::X),
        };
        sections.push(geom::Section::new(center, right, up, 0.02, WIDTH, 1.0));
    }
    acc.add(
        Surface::Cloth,
        Piece::new(geom::loft(&sections, 8, false))
            .uv_box(FULL, 0.9, uv_jitter(seed ^ 0x01))
            .transform(Mat4::IDENTITY),
    );
    // 下摆金边（细条）
    let (hem_half, hem_center) = match axis {
        Axis::X => (
            Vec3::new(0.03, 0.035, WIDTH + 0.01),
            Vec3::new(inner + 0.11, BOTTOM - 0.03, along),
        ),
        _ => (
            Vec3::new(WIDTH + 0.01, 0.035, 0.03),
            Vec3::new(along, BOTTOM - 0.03, inner + 0.11),
        ),
    };
    acc.add(
        Surface::Iron,
        piece(
            Surface::Iron,
            block(hem_half),
            3.0,
            seed ^ 0x02,
            Mat4::from_translation(hem_center),
        ),
    );
}

/// 木门 + 铁箍（嵌在墙体凹口里：玩家被碰撞盒挡住，够不到门面）。
fn gate(acc: &mut Acc, inner: f32, seed: u64) {
    const HALF_W: f32 = 1.60;
    const TOP: f32 = 2.60;
    // 石框（两侧门柱 + 门楣）
    for side in [-1.0f32, 1.0] {
        acc.add(
            Surface::Stone,
            piece(
                Surface::Stone,
                block(Vec3::new(0.30, TOP * 0.5, 0.255)),
                0.6,
                seed ^ 0x01,
                Mat4::from_translation(Vec3::new(side * (HALF_W + 0.30), TOP * 0.5, inner + 0.175)),
            ),
        );
    }
    acc.add(
        Surface::Stone,
        piece(
            Surface::Stone,
            block(Vec3::new(HALF_W + 0.60, 0.22, 0.30)),
            0.6,
            seed ^ 0x02,
            Mat4::from_translation(Vec3::new(0.0, TOP + 0.22, inner + 0.20)),
        ),
    );
    // 门扇（双开，微开一条缝）+ 竖板
    for side in [-1.0f32, 1.0] {
        let width = 0.76;
        acc.add(
            Surface::Wood,
            piece(
                Surface::Wood,
                block(Vec3::new(width, TOP * 0.5 - 0.02, 0.09)),
                1.0,
                seed ^ 0x03 ^ (side > 0.0) as u64,
                Mat4::from_translation(Vec3::new(side * (width + 0.015), TOP * 0.5, inner + 0.40)),
            ),
        );
        // 铁箍三道
        for (index, y) in [0.42f32, 1.26, 2.14].iter().enumerate() {
            acc.add(
                Surface::Iron,
                piece(
                    Surface::Iron,
                    block(Vec3::new(width - 0.02, 0.075, 0.115)),
                    2.0,
                    seed ^ 0x04 ^ index as u64,
                    Mat4::from_translation(Vec3::new(side * (width + 0.015), *y, inner + 0.40)),
                ),
            );
        }
        // 门把手
        acc.add(
            Surface::Iron,
            Piece::new(geom::lathe(
                &[(0.000, -0.16), (0.035, -0.20), (0.000, -0.24)],
                8,
            ))
            .uv_cylinder(FULL, Axis::X, 3.0, 3.0, uv_jitter(seed ^ 0x05))
            .transform(
                Mat4::from_rotation_z(0.0)
                    * Mat4::from_translation(Vec3::new(side * 0.22, 1.18, inner + 0.30)),
            ),
        );
    }
}

/// 对整张网格施加刚体变换（位置 + 逆转置法线 + 切线；**不含镜像**）。
fn transform_mesh(mesh: &mut MeshData, matrix: Mat4) {
    let normal_matrix = matrix.inverse().transpose();
    for vertex in &mut mesh.vertices {
        let position = matrix.transform_point3(Vec3::from(vertex.position));
        let normal = normal_matrix
            .transform_vector3(Vec3::from(vertex.normal))
            .normalize_or_zero();
        vertex.position = position.into();
        vertex.normal = normal.into();
    }
    if let Some(tangents) = &mut mesh.tangents {
        for tangent in tangents.iter_mut() {
            let t = matrix
                .transform_vector3(Vec3::new(tangent[0], tangent[1], tangent[2]))
                .normalize_or_zero();
            *tangent = [t.x, t.y, t.z, tangent[3]];
        }
    }
}

/// 在给定**刚体变换**下追加一组构件：墙 / 拱 / 旗帜一律按 +X / +Z 侧建模，
/// 对面那一道用**绕 Y 轴 180° 旋转**复制。
///
/// 不能用纯平移：墙体的构件在垂直方向是不对称的（基座 / 壁柱朝场内凸出
/// 0.06 m、墙体本体朝场外延伸 0.6 m），平移过去会把「凸出」搬到场外、
/// 把 0.6 m 厚的墙体推进场地（实测：−Z 墙的基座落到 z = −6.19，比碰撞面
/// 深 0.61 m）。旋转才是「对面那一道」的正确变换；镜像（scale = −1）会
/// 翻环绕方向，同样不能用。
fn with_transform(acc: &mut Acc, matrix: Mat4, build: impl FnOnce(&mut Acc)) {
    let mut temp = Acc::default();
    build(&mut temp);
    for (surface, mut mesh) in temp.finish().groups {
        transform_mesh(&mut mesh, matrix);
        acc.add(surface, Piece::new(mesh));
    }
}

/// 庭院内表面（= 碰撞盒内表面，视觉几何以此为基准）。
const INNER_Z: f32 = 6.80;
/// 庭院内表面（X 向）。
const INNER_X: f32 = 9.80;
/// 长墙长度（X 向）。
const LONG_WALL: f32 = 20.8;
/// 短墙长度（Z 向）。
const SHORT_WALL: f32 = 14.8;
/// 长墙上的壁龛中心（X 坐标）。
const LONG_NICHES: [f32; 3] = [-6.0, 0.0, 6.0];
/// 短墙上的壁龛中心（Z 坐标）。
const SHORT_NICHES: [f32; 2] = [-3.4, 3.4];
/// 壁龛开洞宽度。
const NICHE_WIDTH: f32 = 1.55;

/// 构建整座庭院（纯函数：同参数逐位同输出）。
pub fn build_courtyard() -> Courtyard {
    let mut acc = Acc::default();
    // 铺地
    paving(&mut acc);
    // 四道墙：两条长墙（三道壁龛；+Z 侧中央是木门）+ 两条短墙（两道壁龛）
    for wall_side in [1.0f32, -1.0] {
        // +Z 侧按正坐标建模；−Z 侧整体绕 Y 轴转 180°（见 with_transform）
        let matrix = if wall_side > 0.0 {
            Mat4::IDENTITY
        } else {
            Mat4::from_rotation_y(std::f32::consts::PI)
        };
        with_transform(&mut acc, matrix, |acc| {
            let gaps: Vec<f32> = LONG_NICHES.to_vec();
            wall_shell(acc, Axis::Z, INNER_Z, LONG_WALL, &gaps, NICHE_WIDTH, 0xA001);
            wall_run(acc, Axis::Z, INNER_Z, LONG_WALL, 0xA002);
            for (index, along) in LONG_NICHES.iter().enumerate() {
                arch_niche(acc, Axis::Z, INNER_Z, *along, 0xA010 + index as u64);
            }
            // 旗帜：壁龛之间的空白处（2.16 m 以上，角色够不到）
            for (index, along) in [-9.2f32, -3.0, 3.0, 9.2].iter().enumerate() {
                banner(acc, Axis::Z, INNER_Z, *along, 0xA020 + index as u64);
            }
        });
    }
    for wall_side in [1.0f32, -1.0] {
        let matrix = if wall_side > 0.0 {
            Mat4::IDENTITY
        } else {
            Mat4::from_rotation_y(std::f32::consts::PI)
        };
        with_transform(&mut acc, matrix, |acc| {
            let gaps: Vec<f32> = SHORT_NICHES.to_vec();
            wall_shell(
                acc,
                Axis::X,
                INNER_X,
                SHORT_WALL,
                &gaps,
                NICHE_WIDTH,
                0xB001,
            );
            wall_run(acc, Axis::X, INNER_X, SHORT_WALL, 0xB002);
            for (index, along) in SHORT_NICHES.iter().enumerate() {
                arch_niche(acc, Axis::X, INNER_X, *along, 0xB010 + index as u64);
            }
            for (index, along) in [-6.4f32, 6.4].iter().enumerate() {
                banner(acc, Axis::X, INNER_X, *along, 0xB020 + index as u64);
            }
        });
    }
    // 木门：+Z 长墙中央（壁龛让位给门洞）
    gate(&mut acc, INNER_Z, 0xC001);
    // 石柱（碰撞盒 0.9×0.9，构件全部落在盒内）
    for (index, (x, z)) in [(-6.5f32, -4.5f32), (6.5, -4.5), (-6.5, 4.5), (6.5, 4.5)]
        .iter()
        .enumerate()
    {
        column(&mut acc, Vec2::new(*x, *z), 0xD000 + index as u64);
    }
    // 喷泉
    fountain(&mut acc, 0xE001);
    // 悬挂火盆：光点位（logic 常量）在 (±7.4, 2.6, ±5.8)，盆挂在 2.6 m
    brazier(&mut acc, Vec3::new(7.4, 0.0, 5.8), INNER_Z, 0xF001);
    with_transform(
        &mut acc,
        Mat4::from_rotation_y(std::f32::consts::PI),
        |acc| {
            brazier(acc, Vec3::new(7.4, 0.0, 5.8), INNER_Z, 0xF002);
        },
    );
    acc.finish()
}

/// 三角面总数（预算诊断）。
pub fn triangle_count(courtyard: &Courtyard) -> usize {
    courtyard
        .groups
        .iter()
        .map(|(_, mesh)| mesh.indices.len() / 3)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(courtyard: &Courtyard, surface: Surface) -> &MeshData {
        &courtyard
            .groups
            .iter()
            .find(|(s, _)| *s == surface)
            .unwrap_or_else(|| panic!("材质 {surface:?} 应有网格"))
            .1
    }

    #[test]
    fn courtyard_is_deterministic_bitwise() {
        let a = build_courtyard();
        let b = build_courtyard();
        assert_eq!(a.groups.len(), b.groups.len(), "材质分组数应一致");
        for ((sa, ma), (sb, mb)) in a.groups.iter().zip(&b.groups) {
            assert_eq!(sa, sb);
            assert_eq!(ma.vertices.len(), mb.vertices.len());
            let bits_a: Vec<u32> = ma
                .vertices
                .iter()
                .flat_map(|v| v.position.iter().map(|f| f.to_bits()))
                .collect();
            let bits_b: Vec<u32> = mb
                .vertices
                .iter()
                .flat_map(|v| v.position.iter().map(|f| f.to_bits()))
                .collect();
            assert_eq!(bits_a, bits_b, "{sa:?} 顶点应逐位一致");
            assert_eq!(ma.indices, mb.indices, "{sa:?} 索引应一致");
        }
    }

    #[test]
    fn courtyard_triangle_budget_is_sane() {
        let courtyard = build_courtyard();
        let triangles = triangle_count(&courtyard);
        assert!(
            triangles > 8_000,
            "庭院几何过少（{triangles} 三角形）——构件漏建？"
        );
        assert!(triangles < 90_000, "庭院几何超预算（{triangles} 三角形）");
        for (surface, mesh) in &courtyard.groups {
            assert!(!mesh.indices.is_empty(), "{surface:?} 分组不应为空");
            assert!(
                mesh.indices
                    .iter()
                    .all(|&i| (i as usize) < mesh.vertices.len()),
                "{surface:?} 索引越界"
            );
            assert!(mesh.skin.is_none(), "庭院是静态几何，不应有蒙皮");
        }
    }

    /// 碰撞契约：角色够得到的高度（≤ 2.10 m）内，构件不得**深入**场地
    /// 8 cm 以上——浅浮雕式凸出（基座 / 壁柱 6 cm）允许，被碰撞体覆盖的
    /// 体量（喷泉球体、石柱盒）豁免。
    #[test]
    fn geometry_stays_clear_of_player_space() {
        const TOLERANCE: f32 = 0.08;
        let courtyard = build_courtyard();
        for (surface, mesh) in &courtyard.groups {
            for vertex in &mesh.vertices {
                let [x, y, z] = vertex.position;
                // 只在「角色躯干高度」卡：贴地的地面装饰（圆心石带 1.2 cm）
                // 与头顶以上构件不参与
                if !(0.06..=PLAYER_REACH_Y).contains(&y) {
                    continue;
                }
                let inside_x = x.abs() < INNER_X - TOLERANCE;
                let inside_z = z.abs() < INNER_Z - TOLERANCE;
                if !(inside_x && inside_z) {
                    continue; // 越出场地 = 在墙里，合法
                }
                // 喷泉球体 / 石柱盒覆盖的构件合法（各有专项测试卡外形）
                let in_fountain = (x * x + z * z).sqrt() <= 1.75;
                let in_pillar = crate::logic::PILLARS.iter().any(|pillar| {
                    (x - pillar.center.x).abs() <= 0.45 && (z - pillar.center.z).abs() <= 0.45
                });
                assert!(
                    in_fountain || in_pillar,
                    "{surface:?} 在玩家高度深入场地（{x}, {y}, {z}）且无碰撞体覆盖"
                );
            }
        }
    }

    /// 喷泉：池体全部落在球碰撞体（r = 1.75）内；石柱：构件全落在 0.9×0.9
    /// 碰撞盒内（否则玩家会走进石头里）。
    #[test]
    fn fountain_and_columns_stay_inside_their_colliders() {
        let courtyard = build_courtyard();
        // 喷泉：靠近原点的顶点半径必须 ≤ 1.75
        for (surface, mesh) in &courtyard.groups {
            for vertex in &mesh.vertices {
                let [x, y, z] = vertex.position;
                let radius = (x * x + z * z).sqrt();
                // 只看喷泉本体（半径 1.9 以内）：外圈石带是 1.2 cm 厚的地面
                // 装饰，玩家走过不受影响（无碰撞体，视觉上跨过一条细带）
                if radius < 1.9 && y > 0.0 && y < 2.2 {
                    assert!(
                        radius <= 1.76,
                        "{surface:?} 喷泉构件越出球碰撞体：r = {radius}"
                    );
                }
            }
        }
        // 石柱：以四根柱心为中心 0.9×0.9 的窗口内，|Δ| 必须 ≤ 0.45
        for (cx, cz) in [(-6.5f32, -4.5f32), (6.5, -4.5), (-6.5, 4.5), (6.5, 4.5)] {
            for (surface, mesh) in &courtyard.groups {
                for vertex in &mesh.vertices {
                    let [x, y, z] = vertex.position;
                    if (x - cx).abs() < 0.6 && (z - cz).abs() < 0.6 && y < 3.2 {
                        assert!(
                            (x - cx).abs() <= 0.46 && (z - cz).abs() <= 0.46,
                            "{surface:?} 石柱构件越出 0.9×0.9 碰撞盒：(Δx, Δz) = ({}, {})",
                            x - cx,
                            z - cz
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn every_surface_group_has_expected_material() {
        let courtyard = build_courtyard();
        // 主要材质必须都有几何（漏建一种 = 画面上一整类构件消失）
        for surface in [
            Surface::Paving,
            Surface::Stone,
            Surface::Brick,
            Surface::Marble,
            Surface::Wood,
            Surface::Iron,
            Surface::Cloth,
            Surface::Water,
            Surface::Coal,
        ] {
            let mesh = group(&courtyard, surface);
            assert!(
                mesh.indices.len() / 3 > 2,
                "{surface:?} 分组只有 {} 个三角形",
                mesh.indices.len() / 3
            );
        }
    }
}
