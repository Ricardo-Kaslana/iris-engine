//! 士兵模型：蒙皮人形 + 装备 + 武器（程序化 · 单网格单材质 · 零手绘资产）。
//!
//! 本模块把「一具能看的士兵」拆成**可复算的参数化部件**，按形态选基本体：
//!
//! - **硬表面装备**（护甲板 / 头盔 / 弹匣 / 枪身）用 [`crate::geom::plate`]（倒角板）、
//!   [`crate::geom::prism`]（凸棱柱）与 [`crate::geom::arc_shell`]（弧形壳）——
//!   顶点分裂 ⇒ **法线硬边**，棱上有高光带，读作机加工件；
//! - **肢体 / 躯干**用超椭圆截面放样（[`crate::geom::tube`] / [`crate::geom::loft`]），
//!   平滑法线的有机形态；
//! - **背带 / 鞋带 / 天线**用 [`crate::geom::sweep`]（平行传输框架，不扭转）。
//!
//! 全部顶点带蒙皮权重（沿骨链平滑混合，装备刚性绑定）与图集 UV，合并成**一张**
//! 带 TANGENT 的蒙皮网格 ⇒ 整具角色一次 draw。
//!
//! # 与骨架表的关系（单一事实来源）
//!
//! 所有关节位置取自 [`crate::skeleton::bind_pose_world`]——网格顶点写在绑定
//! 姿势的模型空间里，蒙皮逆绑定矩阵是同一张表的逆。改骨架 = 改
//! [`crate::skeleton::BONE_LOCAL`]，网格与蒙皮**同时**跟着走，不存在
//! 「模型与骨架错位」这一整类 bug。
//!
//! 权重链（[`Piece::blend_chain`]）按模型空间折线投影分配：
//! - **脊柱**：[`PELVIS`] → [`TORSO`] → [`CHEST`] → [`HEAD`]（band 0.13）；
//! - **腿**：骨盆 → 大腿 → 小腿 → 脚（band 0.10；靴筒到踝 band 0.06）；
//! - **臂**：胸 → 上臂 → 前臂 → 手（band 0.09）。
//!
//! 装备（护甲板 / 弹匣包 / 腰带 / 武器）刚性绑定到承载骨——硬表面本来
//! 就不该被蒙皮软化，且刚性件省掉关节处的挤压伪影。
//!
//! # 坐标约定
//!
//! 角色朝 **−Z**（枪口 / 面罩 / 脚尖都指向 −Z），+X 是角色**右手**方向，
//! +Y 向上。所有「绕 Y 轴的角度」按截面基（`right = X`、`up = Z`）计：
//! θ = 0 在 +X（右侧）、θ = π/2 在 +Z（**背后**）、θ = 3π/2 在 −Z（**面前**）。
//!
//! # 诚实限制
//! - 体型是**一份参数**（身高 ≈ 2.07 m 含头盔的成年男性士兵）：没有体型系统，
//!   换体型要改本模块参数；
//! - 手指是**四指三节 + 拇指两节**的固定微屈姿势（刚性绑手骨）——没有逐指
//!   骨骼，故不能抓握变形，握枪 / 握刀靠手骨 IK 摆位；
//! - 布料无蒙皮褶皱（没有姿态空间变形 / 布料模拟），关节弯曲处靠权重混合
//!   与装备遮挡；
//! - 左右对称部件由同一函数镜像生成（改一侧即改两侧），不做非对称装备；
//! - 头盔 / 护甲是**单一实体**，没有可拆件与挂点系统（挂点靠位置常数）。

use glam::{Mat4, Quat, Vec2, Vec3};
use iris_engine::prelude::MeshData;

use crate::geom::{self, ArcRow, Assembly, Axis, ChainBone, Piece, Section, Tile};
use crate::skeleton as sk;
use crate::soldier_tex::Surface;

/// 面前方向角（截面基 θ：0 = +X 右侧、π/2 = +Z 背后、3π/2 = −Z 面前）。
const FRONT: f32 = 3.0 * std::f32::consts::FRAC_PI_2;

/// 士兵网格 + 蒙皮逆绑定矩阵（材质图集由 [`crate::soldier_tex`] 另出）。
pub struct SoldierMesh {
    /// 蒙皮网格（模型空间顶点 + 每顶点 4 骨权重 + 切线）。
    pub mesh: MeshData,
    /// 逆绑定矩阵（下标 = [`crate::skeleton`] 关节下标；与网格权重同序）。
    pub inverse_bind: Vec<Mat4>,
}

/// 绑定姿势关节位置查询。
struct Rig {
    world: [Mat4; sk::NODE_COUNT],
}

impl Rig {
    fn new() -> Self {
        Self {
            world: sk::bind_pose_world(),
        }
    }

    /// 关节模型空间位置。
    fn p(&self, index: usize) -> Vec3 {
        self.world[index].w_axis.truncate()
    }

    /// 骨链一节（头 = 关节位置，尾由调用方给出）。
    fn bone(&self, index: usize, tail: Vec3) -> ChainBone {
        ChainBone::new(index as u32, self.p(index), tail)
    }
}

// ---------------------------------------------------------------------------
// 生成工具
// ---------------------------------------------------------------------------

/// 竖直肢体放样：轴向下 / 向上，剖面 `(t, 半宽 X, 半深 Z, 指数)`。
fn limb(a: Vec3, b: Vec3, profile: &[(f32, f32, f32, f32)], segments: u32) -> MeshData {
    geom::tube(a, b, profile, segments, Vec3::Z)
}

/// 沿 Z 轴放样（枪管 / 靴楦）：剖面 `(t, 半宽 X, 半高 Y, 指数)`。
fn bore(a: Vec3, b: Vec3, profile: &[(f32, f32, f32, f32)], segments: u32) -> MeshData {
    geom::tube(a, b, profile, segments, Vec3::Y)
}

/// 旋转 + 平移的摆放矩阵。
fn place(at: Vec3, rot: Quat) -> Mat4 {
    Mat4::from_rotation_translation(rot, at)
}

/// 由体表法向构造摆放矩阵：局部 **+Z → `outward`**、局部 +Y 尽量朝 `up`。
///
/// 硬表面小件（导轨 / 扣具 / MOLLE 织带 / 徽记）贴到曲面上的通用工具：
/// 位置与朝向一起给，省掉「先摆位置再试角度」的试错。
fn facing(at: Vec3, outward: Vec3, up: Vec3) -> Mat4 {
    let forward = outward.normalize();
    let up = (up - forward * up.dot(forward)).normalize_or_zero();
    let right = up.cross(forward);
    Mat4::from_cols(
        right.extend(0.0),
        up.extend(0.0),
        forward.extend(0.0),
        at.extend(1.0),
    )
}

/// 闭合环带壳（绕 Y 轴整圈）：腰带 / 衣摆 / 领圈。行 = `(y, 半宽 X, 半深 Z, 指数)`。
fn ring_shell(rows: &[(f32, f32, f32, f32)], segments: u32, thickness: f32) -> MeshData {
    let rows: Vec<ArcRow> = rows
        .iter()
        .map(|&(y, hx, hz, exponent)| {
            ArcRow::new(
                Section::new(Vec3::new(0.0, y, 0.0), Vec3::X, Vec3::Z, hx, hz, exponent),
                0.0,
                std::f32::consts::TAU,
            )
        })
        .collect();
    geom::arc_shell(&rows, segments, thickness)
}

/// 绕 Y 轴的开弧壳：胸甲 / 背板 / 面罩 / 头盔下半（`angle` 为中心角，`half_arc` 半张角）。
fn arc_plate(
    rows: &[(f32, f32, f32, f32)],
    angle: f32,
    half_arc: f32,
    segments: u32,
    thickness: f32,
) -> MeshData {
    let rows: Vec<ArcRow> = rows
        .iter()
        .map(|&(y, hx, hz, exponent)| {
            ArcRow::new(
                Section::new(Vec3::new(0.0, y, 0.0), Vec3::X, Vec3::Z, hx, hz, exponent),
                angle - half_arc,
                angle + half_arc,
            )
        })
        .collect();
    geom::arc_shell(&rows, segments, thickness)
}

/// 肢体外包壳：轴 `a → b` 的一段壳，弧以体表法向 `outward` 为中心。
///
/// 肩甲 / 护腕 / 护膝 / 靴筒 / 护胫的通用形态——曲面包住肢体、四条封边是硬刃。
fn limb_shell(
    a: Vec3,
    b: Vec3,
    profile: &[(f32, f32, f32, f32)],
    segments: u32,
    outward: Vec3,
    half_arc: f32,
    thickness: f32,
) -> MeshData {
    let axis = (b - a).normalize();
    let forward = (outward - axis * outward.dot(axis)).normalize_or_zero();
    let right = axis.cross(forward).normalize();
    let rows: Vec<ArcRow> = profile
        .iter()
        .map(|&(t, half_right, half_up, exponent)| {
            ArcRow::new(
                Section::new(a.lerp(b, t), right, forward, half_right, half_up, exponent),
                std::f32::consts::FRAC_PI_2 - half_arc,
                std::f32::consts::FRAC_PI_2 + half_arc,
            )
        })
        .collect();
    geom::arc_shell(&rows, segments, thickness)
}

/// 倒角板部件（硬表面）：`half` 半轴 + 倒角 + 摆放矩阵 + UV。
fn hard_plate(half: Vec3, bevel: f32, matrix: Mat4, tile: Tile, scale: f32, offset: Vec2) -> Piece {
    Piece::new(geom::plate(half, bevel))
        .uv_box(tile, scale, offset)
        .transform(matrix)
}

/// 构建士兵网格（纯函数：同参数逐位同输出）。
pub fn build_soldier_mesh() -> SoldierMesh {
    let rig = Rig::new();
    let mut assembly = Assembly::new();
    push_torso(&rig, &mut assembly);
    push_legs(&rig, &mut assembly);
    push_arms(&rig, &mut assembly);
    push_head(&rig, &mut assembly);
    push_fabric(&rig, &mut assembly);
    push_gear(&mut assembly);
    push_sword(&rig, &mut assembly);
    let inverse_bind = rig.world.iter().map(|m| m.inverse()).collect();
    SoldierMesh {
        mesh: assembly.finish(),
        inverse_bind,
    }
}

/// 构建步枪**刚性**网格（[`RIFLE`] 骨节点局部坐标系，无蒙皮属性）。
///
/// 单独成网格的理由：枪要能被 [`set_subtree_hidden`] + RIFLE 整支隐藏（敌人
/// 是持刀近战、不带枪）——蒙皮网格是一整块实体，逐骨隐藏无从下手。直接挂
/// 骨节点的普通网格由节点变换驱动，与蒙皮前的表现路径逐字相同。
pub fn build_rifle_mesh() -> MeshData {
    let mut assembly = Assembly::new();
    push_rifle(&mut assembly);
    assembly.finish()
}

// ---------------------------------------------------------------------------
// 权重链
// ---------------------------------------------------------------------------

/// 脊柱权重链（骨盆 → 躯干 → 胸 → 头）。
fn spine_chain(rig: &Rig) -> Vec<ChainBone> {
    vec![
        rig.bone(sk::PELVIS, rig.p(sk::TORSO)),
        rig.bone(sk::TORSO, rig.p(sk::CHEST)),
        rig.bone(sk::CHEST, rig.p(sk::HEAD)),
    ]
}

/// 腿权重链（骨盆 → 大腿 → 小腿 → 脚）。
fn leg_chain(rig: &Rig, thigh: usize, shin: usize, foot: usize) -> Vec<ChainBone> {
    vec![
        rig.bone(sk::PELVIS, rig.p(thigh)),
        rig.bone(thigh, rig.p(shin)),
        rig.bone(shin, rig.p(foot)),
    ]
}

/// 踝部权重链（小腿 → 脚，靴筒用）。
fn ankle_chain(rig: &Rig, shin: usize, foot: usize) -> Vec<ChainBone> {
    vec![
        rig.bone(shin, rig.p(foot)),
        rig.bone(foot, rig.p(foot) + Vec3::new(0.0, -0.02, -0.14)),
    ]
}

/// 臂权重链（胸 → 上臂 → 前臂 → 手）。
fn arm_chain(rig: &Rig, upper: usize, fore: usize, hand: usize) -> Vec<ChainBone> {
    vec![
        rig.bone(sk::CHEST, rig.p(upper)),
        rig.bone(upper, rig.p(fore)),
        rig.bone(fore, rig.p(hand)),
    ]
}

// ---------------------------------------------------------------------------
// 躯干
// ---------------------------------------------------------------------------

/// 躯干（含髋部与斜方肌）：一根从会阴到颈根的放样管——胸宽腰窄的连续体型。
///
/// 肩线（y = 1.60）半宽 0.197 是**斜方肌**的体积：肩关节在 x = ±0.34，
/// 与躯干之间 0.14 m 的空缺由三角肌质量（[`push_arms`]）补上——两处一起
/// 才没有「断肩」。
fn push_torso(rig: &Rig, assembly: &mut Assembly) {
    let cloth = Surface::ClothTorso.tile();
    let sections: Vec<(f32, f32, f32, f32)> = vec![
        (0.858, 0.102, 0.088, 2.2),
        (0.898, 0.146, 0.110, 2.3),
        (0.958, 0.162, 0.118, 2.4),
        (1.030, 0.150, 0.108, 2.4),
        (1.100, 0.134, 0.098, 2.3),
        (1.180, 0.141, 0.103, 2.3),
        (1.270, 0.158, 0.113, 2.4),
        (1.360, 0.176, 0.122, 2.4),
        (1.450, 0.190, 0.127, 2.5),
        (1.530, 0.196, 0.126, 2.5),
        (1.600, 0.197, 0.121, 2.6),
        (1.655, 0.181, 0.109, 2.6),
        (1.700, 0.126, 0.093, 2.5),
        (1.734, 0.084, 0.079, 2.3),
    ];
    let sections: Vec<Section> = sections
        .iter()
        .map(|&(y, hx, hz, e)| Section::new(Vec3::new(0.0, y, 0.0), Vec3::X, Vec3::Z, hx, hz, e))
        .collect();
    assembly.add(
        Piece::new(geom::loft(&sections, 18, true))
            .uv_cylinder(cloth, Axis::Y, 1.0, 2.0, Vec2::ZERO)
            .blend_chain(&spine_chain(rig), 0.13),
    );
    // 战斗服下摆：盖住腰带下缘、向下外张（布料体积感），绑骨盆
    assembly.add(
        Piece::new(ring_shell(
            &[(0.946, 0.178, 0.132, 2.6), (1.058, 0.156, 0.113, 2.6)],
            18,
            0.011,
        ))
        .uv_cylinder(cloth, Axis::Y, 1.0, 3.0, Vec2::new(0.5, 0.0))
        .rigid(sk::PELVIS as u32),
    );
    // 腰部束带（衣摆与腰带之间的织带，收住下摆上沿）
    assembly.add(
        Piece::new(ring_shell(
            &[(1.060, 0.158, 0.115, 2.6), (1.096, 0.156, 0.113, 2.6)],
            18,
            0.008,
        ))
        .uv_cylinder(Surface::Webbing.tile(), Axis::Y, 1.0, 10.0, Vec2::ZERO)
        .rigid(sk::PELVIS as u32),
    );
    // 立领：颈根一圈（压住内衬与颈套的接缝，破掉「无颈」的方块感）
    assembly.add(
        Piece::new(ring_shell(
            &[(1.696, 0.100, 0.088, 2.4), (1.742, 0.094, 0.083, 2.4)],
            18,
            0.008,
        ))
        .uv_cylinder(cloth, Axis::Y, 1.0, 8.0, Vec2::ZERO)
        .rigid(sk::CHEST as u32),
    );
}

// ---------------------------------------------------------------------------
// 腿与靴
// ---------------------------------------------------------------------------

/// 双腿：大腿 / 小腿 / 侧袋 / 膝甲 / 战靴。
fn push_legs(rig: &Rig, assembly: &mut Assembly) {
    let pants = Surface::ClothPants.tile();
    let armor = Surface::Armor.tile();
    let pouch = Surface::Pouch.tile();
    let webbing = Surface::Webbing.tile();
    let trim = Surface::Trim.tile();
    for (thigh, shin, foot, side) in [
        (sk::L_THIGH, sk::L_SHIN, sk::L_FOOT, -1.0f32),
        (sk::R_THIGH, sk::R_SHIN, sk::R_FOOT, 1.0),
    ] {
        let (hip, knee, ankle) = (rig.p(thigh), rig.p(shin), rig.p(foot));
        let chain = leg_chain(rig, thigh, shin, foot);
        // 大腿：髋臀饱满、股四头肌前凸、膝上收窄
        let thigh_profile: Vec<(f32, f32, f32, f32)> = vec![
            (0.00, 0.099, 0.106, 3.0),
            (0.16, 0.103, 0.112, 3.0),
            (0.44, 0.095, 0.103, 3.0),
            (0.72, 0.083, 0.089, 3.0),
            (0.92, 0.074, 0.079, 3.0),
            (1.00, 0.071, 0.076, 3.0),
        ];
        assembly.add(
            Piece::new(limb(hip, knee, &thigh_profile, 14))
                .uv_cylinder(pants, Axis::Y, 1.0, 2.2, Vec2::ZERO)
                .blend_chain(&chain, 0.10),
        );
        // 小腿：腓肠肌鼓起、踝上收成靴口
        let shin_profile: Vec<(f32, f32, f32, f32)> = vec![
            (0.00, 0.072, 0.078, 3.0),
            (0.20, 0.075, 0.089, 3.0),
            (0.50, 0.063, 0.071, 3.0),
            (0.78, 0.053, 0.058, 2.8),
            (1.00, 0.050, 0.055, 2.8),
        ];
        assembly.add(
            Piece::new(limb(knee, ankle, &shin_profile, 14))
                .uv_cylinder(pants, Axis::Y, 1.0, 2.4, Vec2::ZERO)
                .blend_chain(&chain, 0.10),
        );
        // 裤脚收口（膝下打褶）：环带，绑小腿
        let cuff: Vec<(f32, f32, f32, f32)> = vec![
            (0.00, 0.079, 0.094, 3.0),
            (0.55, 0.074, 0.086, 3.0),
            (1.00, 0.068, 0.077, 3.0),
        ];
        assembly.add(
            Piece::new(limb(knee, knee.lerp(ankle, 0.42), &cuff, 14))
                .uv_cylinder(pants, Axis::Y, 1.0, 5.0, Vec2::new(0.3, 0.0))
                .rigid(shin as u32),
        );
        // 大腿侧袋：硬质口袋 + 翻盖 + 织带扣（绑大腿）
        let pocket_at = hip.lerp(knee, 0.40) + Vec3::new(side * 0.086, 0.0, -0.004);
        assembly.add(
            hard_plate(
                Vec3::new(0.038, 0.060, 0.017),
                0.007,
                facing(pocket_at, Vec3::new(side, 0.0, 0.0), Vec3::Y),
                pouch,
                5.0,
                Vec2::ZERO,
            )
            .rigid(thigh as u32),
        );
        assembly.add(
            hard_plate(
                Vec3::new(0.038, 0.017, 0.020),
                0.005,
                facing(
                    pocket_at + Vec3::new(side * 0.004, 0.070, 0.0),
                    Vec3::new(side, 0.0, 0.0),
                    Vec3::Y,
                ),
                pouch,
                6.0,
                Vec2::new(0.4, 0.4),
            )
            .rigid(thigh as u32),
        );
        assembly.add(
            hard_plate(
                Vec3::new(0.011, 0.013, 0.008),
                0.003,
                facing(
                    pocket_at + Vec3::new(side * 0.012, 0.036, 0.0),
                    Vec3::new(side, 0.0, 0.0),
                    Vec3::Y,
                ),
                trim,
                9.0,
                Vec2::ZERO,
            )
            .rigid(thigh as u32),
        );
        // 膝甲：包住膝前 200°（绑小腿，随小腿摆动）
        let knee_axis_a = knee + (hip - knee).normalize() * 0.055;
        let knee_axis_b = knee + (ankle - knee).normalize() * 0.080;
        let knee_profile: Vec<(f32, f32, f32, f32)> = vec![
            (0.00, 0.079, 0.079, 3.2),
            (0.42, 0.083, 0.085, 3.2),
            (1.00, 0.074, 0.072, 3.2),
        ];
        assembly.add(
            Piece::new(limb_shell(
                knee_axis_a,
                knee_axis_b,
                &knee_profile,
                14,
                -Vec3::Z,
                1.75,
                0.011,
            ))
            .uv_box(armor, 3.2, Vec2::new(0.2, 0.5))
            .rigid(shin as u32),
        );
        // 膝甲上下绑带（织带，绕过膝后）
        for (t, offset) in [(0.06f32, 0.010f32), (0.94, -0.008)] {
            let at = knee_axis_a.lerp(knee_axis_b, t);
            assembly.add(
                Piece::new(ring_shell(
                    &[
                        (at.y - 0.010 + offset, 0.076, 0.078, 3.0),
                        (at.y + 0.010 + offset, 0.076, 0.078, 3.0),
                    ],
                    14,
                    0.006,
                ))
                .uv_cylinder(webbing, Axis::Y, 1.0, 12.0, Vec2::ZERO)
                .rigid(shin as u32),
            );
        }
        push_boot(rig, assembly, shin, foot, ankle);
    }
}

/// 战靴：鞋底（跟 / 足弓 / 前掌一体放样）+ 鞋帮楦体 + 鞋头包 + 后跟杯 + 靴筒 +
/// 靴口滚边 + 踝束带 + 鞋舌 + 鞋带 ×4 + 鞋眼 ×8 + 后提环 + 外侧补强片。
///
/// **整片法线反向（本轮修的真 bug）**：鞋楦 / 鞋底 / 跟块三件用显式截面 loft，
/// 截面基 (X, Y) 与放样轴 −Z 不成右手系 ⇒ **整片法线朝内**（小圆柱探针实测有向
/// 体积 −5.81e-4，应为 +5.81e-4）。装配总体积为正 ⇒ 单个部件反向在整身上不显眼，
/// 只在靴子上表现为「打光发灰的圆疙瘩」。**根治在基元层**：loft / lathe / 超椭球
/// 现在都按几何自检环绕方向（geom::orient_outward），本函数只保留「趾 → 跟」
/// 这一顺手的截面序。
///
/// 靴型读法：后跟窄直、足弓抬起（底面中段离地 15 mm，侧面轮廓因此有跟有腰）、
/// 前掌最宽、趾尖上翘收圆、鞋底比鞋帮各宽出 5 mm（沿条）。靴筒比小腿略粗
/// （裤脚罩在靴口上），靴口一圈滚边 + 踝束带 + 后提环。
fn push_boot(rig: &Rig, assembly: &mut Assembly, shin: usize, foot: usize, ankle: Vec3) {
    let boot = Surface::Boot.tile();
    let rubber = Surface::Rubber.tile();
    let webbing = Surface::Webbing.tile();
    let armor = Surface::Armor.tile();
    let leather = Surface::Leather.tile();
    let trim = Surface::Trim.tile();
    let side = if ankle.x < 0.0 { -1.0f32 } else { 1.0 };
    // 鞋底：趾 → 跟（d = +Z 才与截面基 (X, Y) 成右手系）。(z, 底面 y, 顶面 y, 半宽, 指数)
    let outsole: [(f32, f32, f32, f32, f32); 10] = [
        (-0.222, 0.011, 0.019, 0.021, 3.0),
        (-0.198, 0.002, 0.024, 0.036, 3.6),
        (-0.152, 0.000, 0.028, 0.052, 4.0),
        (-0.102, 0.000, 0.030, 0.057, 4.0),
        (-0.052, 0.006, 0.031, 0.056, 4.0),
        (-0.004, 0.015, 0.032, 0.052, 4.0),
        (0.038, 0.003, 0.035, 0.049, 4.0),
        (0.072, 0.000, 0.037, 0.045, 3.6),
        (0.100, 0.005, 0.035, 0.036, 3.2),
        (0.112, 0.014, 0.029, 0.023, 3.0),
    ];
    let sections: Vec<Section> = outsole
        .iter()
        .map(|&(z, bottom, top, hx, e)| {
            Section::new(
                ankle + Vec3::new(0.0, (bottom + top) * 0.5, z),
                Vec3::X,
                Vec3::Y,
                hx,
                (top - bottom) * 0.5,
                e,
            )
        })
        .collect();
    assembly.add(
        Piece::new(geom::loft(&sections, 16, true))
            .uv_box(rubber, 3.0, Vec2::ZERO)
            .rigid(foot as u32),
    );
    // 鞋帮楦体：趾（低窄）→ 踝（高宽），底面埋在鞋底里（底面 y < 鞋底顶面 y）
    let upper: [(f32, f32, f32, f32, f32); 9] = [
        (-0.212, 0.010, 0.056, 0.026, 2.4),
        (-0.186, 0.008, 0.074, 0.040, 2.6),
        (-0.140, 0.008, 0.092, 0.048, 2.8),
        (-0.092, 0.009, 0.107, 0.052, 2.8),
        (-0.040, 0.011, 0.121, 0.053, 2.8),
        (0.008, 0.013, 0.131, 0.051, 2.8),
        (0.050, 0.015, 0.141, 0.047, 2.8),
        (0.086, 0.017, 0.147, 0.041, 3.0),
        (0.106, 0.024, 0.142, 0.030, 3.0),
    ];
    let sections: Vec<Section> = upper
        .iter()
        .map(|&(z, bottom, top, hx, e)| {
            Section::new(
                ankle + Vec3::new(0.0, (bottom + top) * 0.5, z),
                Vec3::X,
                Vec3::Y,
                hx,
                (top - bottom) * 0.5,
                e,
            )
        })
        .collect();
    assembly.add(
        Piece::new(geom::loft(&sections, 16, true))
            .uv_box(boot, 4.2, Vec2::ZERO)
            .rigid(foot as u32),
    );
    // 鞋头包（包住趾部 190°，硬刃）
    assembly.add(
        Piece::new(limb_shell(
            ankle + Vec3::new(0.0, 0.048, -0.152),
            ankle + Vec3::new(0.0, 0.036, -0.206),
            &[
                (0.00, 0.046, 0.040, 2.8),
                (0.55, 0.040, 0.033, 2.6),
                (1.00, 0.026, 0.019, 2.4),
            ],
            14,
            Vec3::Y,
            1.75,
            0.006,
        ))
        .uv_box(armor, 5.5, Vec2::new(0.3, 0.6))
        .rigid(foot as u32),
    );
    // 后跟杯（包住跟腱，绕 Y 轴 166°）
    assembly.add(
        Piece::new(limb_shell(
            ankle + Vec3::new(0.0, 0.018, 0.030),
            ankle + Vec3::new(0.0, 0.104, 0.030),
            &[
                (0.00, 0.048, 0.066, 3.0),
                (0.55, 0.046, 0.064, 3.0),
                (1.00, 0.042, 0.055, 3.0),
            ],
            14,
            Vec3::Z,
            1.45,
            0.007,
        ))
        .uv_box(boot, 4.5, Vec2::new(0.5, 0.5))
        .rigid(foot as u32),
    );
    // 靴筒：踝上到靴口（绑踝链，随踝弯曲）
    assembly.add(
        Piece::new(limb_shell(
            ankle + Vec3::new(0.0, 0.100, 0.006),
            ankle + Vec3::new(0.0, 0.222, 0.006),
            &[
                (0.00, 0.062, 0.070, 3.0),
                (0.45, 0.062, 0.070, 3.0),
                (1.00, 0.066, 0.074, 3.0),
            ],
            16,
            -Vec3::Z,
            std::f32::consts::PI,
            0.008,
        ))
        .uv_cylinder(boot, Axis::Y, 2.0, 3.2, Vec2::ZERO)
        .blend_chain(&ankle_chain(rig, shin, foot), 0.06),
    );
    // 靴口滚边（一圈加厚）
    assembly.add(
        Piece::new(limb_shell(
            ankle + Vec3::new(0.0, 0.214, 0.006),
            ankle + Vec3::new(0.0, 0.238, 0.006),
            &[(0.0, 0.069, 0.077, 2.8), (1.0, 0.065, 0.073, 2.8)],
            16,
            -Vec3::Z,
            std::f32::consts::PI,
            0.006,
        ))
        .uv_cylinder(leather, Axis::Y, 2.0, 12.0, Vec2::ZERO)
        .rigid(foot as u32),
    );
    // 踝束带（整圈织带 + 外侧扣片）
    assembly.add(
        Piece::new(ring_shell(
            &[(0.148, 0.066, 0.074, 3.0), (0.170, 0.066, 0.074, 3.0)],
            16,
            0.005,
        ))
        .uv_box(webbing, 9.0, Vec2::new(0.4, 0.2))
        .transform(Mat4::from_translation(Vec3::new(
            ankle.x,
            0.0,
            ankle.z + 0.006,
        )))
        .rigid(foot as u32),
    );
    assembly.add(
        hard_plate(
            Vec3::new(0.0055, 0.0110, 0.0028),
            0.0014,
            facing(
                ankle + Vec3::new(side * 0.064, 0.159, 0.006),
                Vec3::new(side, 0.0, 0.0),
                Vec3::Y,
            ),
            trim,
            12.0,
            Vec2::new(0.2, 0.5),
        )
        .rigid(foot as u32),
    );
    // 鞋舌（鞋带下的软垫，贴在足背）
    assembly.add(
        hard_plate(
            Vec3::new(0.021, 0.030, 0.0035),
            0.0022,
            facing(
                ankle + Vec3::new(0.0, 0.112, -0.050),
                Vec3::new(0.0, 0.62, -0.79),
                Vec3::Y,
            ),
            boot,
            6.0,
            Vec2::new(0.4, 0.1),
        )
        .rigid(foot as u32),
    );
    // 鞋带 ×4（平行传输扫掠）+ 鞋眼 ×8（黄铜衬圈）
    let laces: [(f32, f32, f32, f32); 4] = [
        (-0.030, 0.120, 0.042, 0.004),
        (-0.062, 0.113, 0.044, 0.005),
        (-0.094, 0.104, 0.046, 0.005),
        (-0.126, 0.093, 0.045, 0.004),
    ];
    for (z, top, half, dip) in laces {
        let path = [
            ankle + Vec3::new(-half, top - 0.019, z + 0.004),
            ankle + Vec3::new(0.0, top + dip, z),
            ankle + Vec3::new(half, top - 0.019, z - 0.004),
        ];
        assembly.add(
            Piece::new(geom::sweep(
                &path,
                &[(0.0, 0.0048, 0.0036, 2.0), (1.0, 0.0048, 0.0036, 2.0)],
                8,
                Vec3::Y,
            ))
            .uv_box(webbing, 11.0, Vec2::new(0.2, 0.7))
            .rigid(foot as u32),
        );
        for s in [-1.0f32, 1.0] {
            assembly.add(
                hard_plate(
                    Vec3::new(0.0060, 0.0042, 0.0026),
                    0.0012,
                    facing(
                        ankle + Vec3::new(s * (half + 0.005), top - 0.020, z),
                        Vec3::new(s, 0.0, 0.0),
                        Vec3::Y,
                    ),
                    trim,
                    14.0,
                    Vec2::new(0.5, 0.5),
                )
                .rigid(foot as u32),
            );
        }
    }
    // 后提环
    assembly.add(
        Piece::new(geom::sweep(
            &[
                ankle + Vec3::new(0.0, 0.170, 0.072),
                ankle + Vec3::new(0.0, 0.216, 0.082),
                ankle + Vec3::new(0.0, 0.246, 0.068),
            ],
            &[(0.0, 0.0085, 0.0040, 2.0), (1.0, 0.0085, 0.0040, 2.0)],
            8,
            Vec3::X,
        ))
        .uv_box(webbing, 8.0, Vec2::new(0.6, 0.3))
        .rigid(foot as u32),
    );
    // 外侧补强片（护踝硬片）
    assembly.add(
        hard_plate(
            Vec3::new(0.0028, 0.026, 0.016),
            0.0016,
            facing(
                ankle + Vec3::new(side * 0.062, 0.196, 0.010),
                Vec3::new(side, 0.0, 0.0),
                Vec3::Y,
            ),
            armor,
            8.0,
            Vec2::new(0.3, 0.7),
        )
        .rigid(foot as u32),
    );
}

// 臂与手
// ---------------------------------------------------------------------------

/// 双臂：三角肌 / 上臂 / 前臂 / 护腕 / 肘甲 / 手（掌 + 四指三节 + 拇指两节）。
fn push_arms(rig: &Rig, assembly: &mut Assembly) {
    let cloth = Surface::ClothTorso.tile();
    let armor = Surface::Armor.tile();
    let webbing = Surface::Webbing.tile();
    for (upper, fore, hand) in [
        (sk::L_UPPERARM, sk::L_FOREARM, sk::L_HAND),
        (sk::R_UPPERARM, sk::R_FOREARM, sk::R_HAND),
    ] {
        let (shoulder, elbow, wrist) = (rig.p(upper), rig.p(fore), rig.p(hand));
        let chain = arm_chain(rig, upper, fore, hand);
        let side = if shoulder.x < 0.0 { -1.0f32 } else { 1.0 };
        // 三角肌：沿 X 的**放样**把斜方肌（躯干半宽 0.197）与肩关节（x = ±0.34）
        // 连成一体——单颗超椭球会留下「断肩」缝（第一版实测）。
        let deltoid: Vec<(f32, f32, f32, f32)> = vec![
            (0.115, 0.096, 0.100, 2.6),
            (0.205, 0.108, 0.110, 2.6),
            (0.290, 0.108, 0.106, 2.6),
            (0.360, 0.096, 0.094, 2.6),
            (0.412, 0.076, 0.076, 2.4),
            (0.444, 0.048, 0.052, 2.2),
        ];
        // 左右臂的截面序互为反向（镜像），环绕方向交给 loft 的几何自检兜住
        let samples = deltoid;
        let sections: Vec<Section> = samples
            .iter()
            .map(|&(x, hz, hy, e)| {
                Section::new(
                    Vec3::new(side * x, shoulder.y - 0.012, 0.0),
                    Vec3::Z,
                    Vec3::Y,
                    hz,
                    hy,
                    e,
                )
            })
            .collect();
        let deltoid_chain = vec![rig.bone(sk::CHEST, shoulder), rig.bone(upper, elbow)];
        assembly.add(
            Piece::new(geom::loft(&sections, 14, true))
                .uv_box(cloth, 3.0, Vec2::new(0.2, 0.5))
                .blend_chain(&deltoid_chain, 0.10),
        );
        // 肩甲：三片叠层弧形壳（绑上臂，随臂摆动）
        for (t0, t1, radius, arc) in [
            (0.00f32, 0.15f32, 0.104f32, 2.0f32),
            (0.12, 0.28, 0.098, 2.1),
            (0.25, 0.42, 0.090, 2.2),
        ] {
            let a = shoulder.lerp(elbow, t0);
            let b = shoulder.lerp(elbow, t1);
            assembly.add(
                Piece::new(limb_shell(
                    a,
                    b,
                    &[
                        (0.0, radius, radius * 0.96, 3.0),
                        (0.55, radius * 0.98, radius * 0.94, 3.0),
                        (1.0, radius * 0.90, radius * 0.86, 3.0),
                    ],
                    16,
                    Vec3::new(side, 0.0, 0.0),
                    arc,
                    0.011,
                ))
                .uv_box(armor, 3.4, Vec2::new(0.2 * t0, 0.3))
                .rigid(upper as u32),
            );
        }
        // 上臂
        let upper_profile: Vec<(f32, f32, f32, f32)> = vec![
            (0.00, 0.078, 0.081, 2.6),
            (0.22, 0.076, 0.079, 2.6),
            (0.70, 0.062, 0.066, 2.6),
            (1.00, 0.057, 0.061, 2.6),
        ];
        assembly.add(
            Piece::new(limb(shoulder, elbow, &upper_profile, 12))
                .uv_cylinder(cloth, Axis::Y, 1.0, 2.6, Vec2::ZERO)
                .blend_chain(&chain, 0.09),
        );
        // 前臂
        let fore_profile: Vec<(f32, f32, f32, f32)> = vec![
            (0.00, 0.059, 0.063, 2.6),
            (0.22, 0.063, 0.067, 2.6),
            (0.75, 0.049, 0.053, 2.6),
            (1.00, 0.045, 0.049, 2.6),
        ];
        assembly.add(
            Piece::new(limb(elbow, wrist, &fore_profile, 12))
                .uv_cylinder(cloth, Axis::Y, 1.0, 2.6, Vec2::ZERO)
                .blend_chain(&chain, 0.09),
        );
        // 护腕：前臂 20%..96% 包一层装甲壳（外侧 240°，露出内侧布料）
        assembly.add(
            Piece::new(limb_shell(
                elbow.lerp(wrist, 0.20),
                elbow.lerp(wrist, 0.96),
                &[
                    (0.00, 0.066, 0.070, 3.4),
                    (0.55, 0.057, 0.061, 3.4),
                    (1.00, 0.052, 0.056, 3.4),
                ],
                14,
                Vec3::new(side, 0.0, -0.35),
                2.10,
                0.010,
            ))
            .uv_box(armor, 3.6, Vec2::new(0.35, 0.15))
            .rigid(fore as u32),
        );
        // 肘甲：包住肘后 200°
        assembly.add(
            Piece::new(limb_shell(
                elbow + (shoulder - elbow).normalize() * 0.052,
                elbow + (wrist - elbow).normalize() * 0.062,
                &[(0.0, 0.062, 0.062, 3.2), (1.0, 0.058, 0.058, 3.2)],
                12,
                Vec3::new(side * 0.35, 0.0, 1.0),
                1.75,
                0.009,
            ))
            .uv_box(armor, 4.0, Vec2::new(0.2, 0.4))
            .rigid(fore as u32),
        );
        // 腕环（护腕下沿与手套的接缝）
        assembly.add(
            Piece::new(limb_shell(
                wrist + Vec3::new(0.0, 0.026, 0.0),
                wrist + Vec3::new(0.0, -0.010, 0.0),
                &[(0.0, 0.054, 0.058, 3.4), (1.0, 0.052, 0.056, 3.4)],
                12,
                -Vec3::Z,
                std::f32::consts::PI,
                0.009,
            ))
            .uv_cylinder(webbing, Axis::Y, 1.0, 6.0, Vec2::new(0.6, 0.4))
            .rigid(hand as u32),
        );
        push_hand(assembly, wrist, side, hand);
    }
}

/// 手：手套腕口 + 掌体（含鱼际）+ 四指（各三节，绕掌心圆弧屈曲）+ 拇指（两节对掌）+ 指背两节护片。
///
/// **比例优先**：手骨节点只到腕（l_hand 在 y = 1.13），掌长与指长全由本函数摆出。
/// 第一版的指长只有 0.030 m（真人中指 0.080）⇒ 指尖够不到掌底，整只手读作
/// 「连指手套」，是本轮返工的第一因。现在：掌长 0.104、指长 0.064~0.080，
/// 指间留缝、关节处剖面半径内收（起皱）、指背掌指关节与近节指间各一片护片。
///
/// 屈曲用**绕掌心的圆弧**而不是直线折角：每指给总屈曲角 θ，曲率半径
/// R = L/θ，路径点取在圆弧上（P(φ) = base + R·(sin φ·down + (1−cos φ)·inward)），
/// 指节因此自然连成一段弧，剖面再按弧长比例分布（sweep 的 t 是弧长比例）。
///
/// **坐标系**：局部基 outward = 手背侧（±X）、inward = 掌心侧、down = 指尖方向、
/// forward = −Z（食指侧）。掌体放样的截面基取 (X, Z)、行方向 −Y（腕 → 指）；两臂
/// 互为镜像 ⇒ 行方向相反，环绕方向交给基元的几何自检（geom::orient_outward）。
fn push_hand(assembly: &mut Assembly, wrist: Vec3, side: f32, hand: usize) {
    let glove = Surface::Glove.tile();
    let armor = Surface::Armor.tile();
    let webbing = Surface::Webbing.tile();
    let bone = hand as u32;
    let outward = Vec3::new(side, 0.0, 0.0);
    let inward = -outward;
    let down = Vec3::NEG_Y;
    let forward = Vec3::NEG_Z;

    // 掌体：腕窄 → 掌指关节宽厚（厚 0.052、宽 0.090）。(y, 半厚 X, 半宽 Z, 指数)
    let palm: [(f32, f32, f32, f32); 6] = [
        (0.016, 0.025, 0.030, 2.6),
        (-0.012, 0.028, 0.037, 2.8),
        (-0.042, 0.029, 0.043, 3.0),
        (-0.070, 0.028, 0.045, 3.0),
        (-0.090, 0.026, 0.044, 2.8),
        (-0.104, 0.021, 0.038, 2.4),
    ];
    let palm_sections: Vec<Section> = palm
        .iter()
        .map(|&(y, hx, hz, e)| {
            Section::new(wrist + Vec3::new(0.0, y, 0.0), Vec3::X, Vec3::Z, hx, hz, e)
        })
        .collect();
    assembly.add(
        Piece::new(geom::loft(&palm_sections, 16, true))
            .uv_box(glove, 6.0, Vec2::ZERO)
            .rigid(bone),
    );
    // 鱼际（拇指根隆起）：掌前内侧，让掌不是一块板
    assembly.add(
        Piece::new(geom::superellipsoid(
            Vec3::new(0.013, 0.029, 0.021),
            2.6,
            12,
            8,
        ))
        .uv_box(glove, 9.0, Vec2::new(0.3, 0.1))
        .transform(place(
            wrist + inward * 0.014 + down * 0.046 + forward * 0.022,
            Quat::IDENTITY,
        ))
        .rigid(bone),
    );
    // 腕口：手套护套（一圈，外侧略厚）+ 背面束带 + 扣片
    assembly.add(
        Piece::new(ring_shell(
            &[
                (0.038, 0.028, 0.033, 2.6),
                (0.014, 0.031, 0.038, 2.6),
                (-0.008, 0.030, 0.036, 2.6),
            ],
            14,
            0.006,
        ))
        .uv_cylinder(webbing, Axis::Y, 2.0, 8.0, Vec2::new(0.2, 0.5))
        .transform(Mat4::from_translation(wrist))
        .rigid(bone),
    );
    assembly.add(
        Piece::new(geom::sweep(
            &[
                wrist + Vec3::new(-side * 0.026, 0.008, -0.030),
                wrist + outward * 0.032 + Vec3::new(0.0, 0.004, -0.008),
                wrist + outward * 0.033 + Vec3::new(0.0, 0.002, 0.014),
                wrist + Vec3::new(-side * 0.028, 0.006, 0.030),
            ],
            &[(0.0, 0.0058, 0.0028, 2.0), (1.0, 0.0058, 0.0028, 2.0)],
            8,
            down,
        ))
        .uv_box(webbing, 9.0, Vec2::new(0.6, 0.4))
        .rigid(bone),
    );
    // 手背护片（掌骨段）
    assembly.add(
        hard_plate(
            Vec3::new(0.024, 0.020, 0.0028),
            0.0016,
            facing(wrist + outward * 0.030 + down * 0.056, outward, down),
            armor,
            9.0,
            Vec2::new(0.2, 0.8),
        )
        .rigid(bone),
    );
    // 四指：(z, 指根半径, 指长, 总屈曲角)——食指靠前（−Z）、小指靠后
    let fingers: [(f32, f32, f32, f32); 4] = [
        (-0.0315, 0.0108, 0.0745, 1.30),
        (-0.0105, 0.0112, 0.0800, 1.42),
        (0.0105, 0.0106, 0.0755, 1.40),
        (0.0300, 0.0094, 0.0640, 1.36),
    ];
    for (z, radius, length, curl) in fingers {
        let base = wrist + down * 0.098 + Vec3::new(0.0, 0.0, z);
        let bend_radius = length / curl;
        let mut path = Vec::new();
        for step in 0..5 {
            let t = step as f32 / 4.0;
            let phi = curl * t;
            let point = base
                + down * (bend_radius * phi.sin())
                + inward * (bend_radius * (1.0 - phi.cos()));
            // 指尖略向掌中线收（握持感）
            path.push(point + Vec3::new(0.0, 0.0, -z * 0.18 * t));
        }
        let profile: [(f32, f32, f32, f32); 8] = [
            (0.00, radius * 1.00, radius * 1.02, 2.4),
            (0.16, radius * 1.07, radius * 1.05, 2.4),
            (0.30, radius * 0.93, radius * 0.97, 2.4),
            (0.50, radius * 1.00, radius * 1.00, 2.4),
            (0.62, radius * 0.90, radius * 0.94, 2.4),
            (0.80, radius * 0.94, radius * 0.96, 2.4),
            (0.92, radius * 0.83, radius * 0.87, 2.2),
            (1.00, radius * 0.60, radius * 0.64, 2.0),
        ];
        assembly.add(
            Piece::new(geom::sweep(&path, &profile, 8, outward))
                .uv_box(glove, 12.0, Vec2::new(0.15, 0.6))
                .rigid(bone),
        );
        // 指背两节护片：掌指关节（t≈0.20）与近节指间（t≈0.62）各一片
        for (t, half, arc_offset) in [
            (0.20f32, Vec3::new(0.0074, 0.0106, 0.0026), 1.00f32),
            (0.62, Vec3::new(0.0062, 0.0084, 0.0022), 0.92),
        ] {
            let phi = curl * t;
            let center = base
                + down * (bend_radius * phi.sin())
                + inward * (bend_radius * (1.0 - phi.cos()));
            let tangent = (down * phi.cos() + inward * phi.sin()).normalize();
            // 指背法向 = 切线绕弧转过的 outward 分量（与 sweep 的截面基同构）
            let back = (outward - tangent * tangent.dot(outward)).normalize();
            assembly.add(
                hard_plate(
                    half,
                    0.0012,
                    facing(center + back * (radius * arc_offset), back, tangent),
                    armor,
                    12.0,
                    Vec2::new(0.3, 0.2),
                )
                .rigid(bone),
            );
        }
    }
    // 拇指：掌前内侧伸出、两节对掌（指尖转向掌心中线）
    let thumb_base = wrist + inward * 0.015 + forward * 0.026 + down * 0.034;
    let thumb_path = [
        thumb_base,
        thumb_base + forward * 0.026 + inward * 0.005 + down * 0.013,
        thumb_base + forward * 0.043 + inward * 0.018 + down * 0.030,
        thumb_base + forward * 0.049 + inward * 0.029 + down * 0.045,
    ];
    let thumb_profile: [(f32, f32, f32, f32); 5] = [
        (0.00, 0.0132, 0.0136, 2.4),
        (0.28, 0.0126, 0.0130, 2.4),
        (0.52, 0.0114, 0.0118, 2.4),
        (0.80, 0.0104, 0.0108, 2.4),
        (1.00, 0.0072, 0.0076, 2.0),
    ];
    assembly.add(
        Piece::new(geom::sweep(&thumb_path, &thumb_profile, 10, outward))
            .uv_box(glove, 11.0, Vec2::new(0.5, 0.3))
            .rigid(bone),
    );
    assembly.add(
        hard_plate(
            Vec3::new(0.0068, 0.0090, 0.0024),
            0.0012,
            facing(
                thumb_base + outward * 0.012 + forward * 0.008,
                outward,
                forward,
            ),
            armor,
            14.0,
            Vec2::new(0.4, 0.6),
        )
        .rigid(bone),
    );
}

// 头颈
// ---------------------------------------------------------------------------

/// 织物细节：肢体上的**堆褶与束带**——上臂 / 前臂 / 大腿 / 小腿各一两道。
///
/// 第一版的四肢是「光滑锥管 + 一块硬壳」，近景读作塑料管：真实的战斗服在关节
/// 附近必然堆褶（布料被压缩），战术裤在大腿中段有织带束带、膝下裤脚収口。
/// 这一层的每一件都是「比肢体半径大 3~6 mm 的短环」——limb 放样、两端収口
/// （剖面半径两端 ×0.92），14 段 ≈ 56 顶点；八个部件合计约 450 顶点，
/// 用最小代价换掉「锥管感」。
///
/// 半径来自相邻肢体的剖面值（腿 0.86 处 hx 0.0765 / 小腿 0.14 处 hx 0.0732 …），
/// 环一律比该处大 3~6 mm ⇒ 既不埋进肢体，也不鼓成救生圈。
fn push_fabric(rig: &Rig, assembly: &mut Assembly) {
    let cloth = Surface::ClothTorso.tile();
    let pants = Surface::ClothPants.tile();
    let webbing = Surface::Webbing.tile();
    // (起点, 终点, 半宽 X, 半深 Z, 骨, 材质, UV 平铺率)
    let mut bands: Vec<(Vec3, Vec3, f32, f32, u32, Tile, f32)> = Vec::new();
    for (upper, fore, hand) in [
        (sk::L_UPPERARM, sk::L_FOREARM, sk::L_HAND),
        (sk::R_UPPERARM, sk::R_FOREARM, sk::R_HAND),
    ] {
        let (shoulder, elbow, wrist) = (rig.p(upper), rig.p(fore), rig.p(hand));
        // 上臂中段袖褶
        bands.push((
            shoulder.lerp(elbow, 0.44),
            shoulder.lerp(elbow, 0.53),
            0.0700,
            0.0740,
            upper as u32,
            cloth,
            8.0,
        ));
        // 肘下堆褶
        bands.push((
            elbow.lerp(wrist, 0.06),
            elbow.lerp(wrist, 0.17),
            0.0650,
            0.0690,
            fore as u32,
            cloth,
            8.0,
        ));
    }
    for (thigh, shin, foot) in [
        (sk::L_THIGH, sk::L_SHIN, sk::L_FOOT),
        (sk::R_THIGH, sk::R_SHIN, sk::R_FOOT),
    ] {
        let (hip, knee, ankle) = (rig.p(thigh), rig.p(shin), rig.p(foot));
        // 大腿中段织带束带（唯一用织带材质的一条：读作装具而不是布褶）
        bands.push((
            hip.lerp(knee, 0.34),
            hip.lerp(knee, 0.41),
            0.1000,
            0.1070,
            thigh as u32,
            webbing,
            9.0,
        ));
        // 膝上堆褶
        bands.push((
            hip.lerp(knee, 0.82),
            hip.lerp(knee, 0.90),
            0.0780,
            0.0840,
            thigh as u32,
            pants,
            6.0,
        ));
        // 膝下堆褶（裤脚压在靴口上）
        bands.push((
            knee.lerp(ankle, 0.10),
            knee.lerp(ankle, 0.19),
            0.0760,
            0.0870,
            shin as u32,
            pants,
            6.0,
        ));
        // 小腿中段
        bands.push((
            knee.lerp(ankle, 0.52),
            knee.lerp(ankle, 0.60),
            0.0660,
            0.0740,
            shin as u32,
            pants,
            6.0,
        ));
    }
    for (start, end, hx, hz, bone, tile, scale) in bands {
        let profile: [(f32, f32, f32, f32); 4] = [
            (0.00, hx * 0.92, hz * 0.94, 3.0),
            (0.25, hx, hz, 3.0),
            (0.75, hx, hz, 3.0),
            (1.00, hx * 0.92, hz * 0.94, 3.0),
        ];
        assembly.add(
            Piece::new(limb(start, end, &profile, 14))
                .uv_cylinder(tile, Axis::Y, 1.0, scale, Vec2::ZERO)
                .rigid(bone),
        );
    }
}

/// 头颈：颈套 + 立领 + 头盔（颅顶闭合壳 + 下半开弧壳）+ 帽檐 + 侧导轨 +
/// 夜视座 + 护目镜带 + 下颌护罩 + 耳罩 + 下巴带。
///
/// 头盔由**两片**壳体拼成：颅顶是闭合环壳（整圈），下半是开弧壳（前面留出
/// 面部开口）。`arc_shell` 要求各行闭合性一致，故不能一片到底——两片在
/// y ≈ 1.90 重叠 3 mm，接缝互相埋进对方体积里（不是共面 ⇒ 无 z-fighting）。
fn push_head(rig: &Rig, assembly: &mut Assembly) {
    let undersuit = Surface::Undersuit.tile();
    let helmet = Surface::Helmet.tile();
    let visor = Surface::Visor.tile();
    let gunmetal = Surface::Gunmetal.tile();
    let webbing = Surface::Webbing.tile();
    let rubber = Surface::Rubber.tile();
    let head = rig.p(sk::HEAD);
    // 颈（内衬紧身衣）：胸 → 头平滑混合
    let neck_chain = vec![
        rig.bone(sk::CHEST, head),
        rig.bone(sk::HEAD, head + Vec3::new(0.0, 0.18, 0.0)),
    ];
    assembly.add(
        Piece::new(limb(
            Vec3::new(0.0, 1.606, 0.008),
            Vec3::new(0.0, 1.804, 0.008),
            &[
                (0.00, 0.066, 0.068, 2.2),
                (0.45, 0.059, 0.061, 2.2),
                (1.00, 0.055, 0.057, 2.2),
            ],
            14,
        ))
        .uv_cylinder(undersuit, Axis::Y, 1.0, 3.2, Vec2::ZERO)
        .blend_chain(&neck_chain, 0.05),
    );
    // 立领：盖住颈与躯干的接缝（第一版「细杆脖子」的修法），绑胸
    assembly.add(
        Piece::new(ring_shell(
            &[
                (1.694, 0.116, 0.101, 2.4),
                (1.742, 0.106, 0.094, 2.3),
                (1.788, 0.096, 0.088, 2.3),
            ],
            16,
            0.013,
        ))
        .uv_cylinder(Surface::ClothTorso.tile(), Axis::Y, 1.0, 5.0, Vec2::ZERO)
        .rigid(sk::CHEST as u32),
    );
    // 头盔下半：开弧（前面留 105° 面部开口），后颈外张
    let lower_rows: Vec<(f32, f32, f32, f32, f32)> = vec![
        (1.812, 0.030, 0.106, 0.118, 2.4),
        (1.836, 0.016, 0.116, 0.128, 2.4),
        (1.868, 0.006, 0.121, 0.130, 2.3),
        (1.900, 0.002, 0.124, 0.130, 2.2),
    ];
    let rows: Vec<ArcRow> = lower_rows
        .iter()
        .map(|&(y, z, hx, hz, e)| {
            ArcRow::new(
                Section::new(Vec3::new(0.0, y, z), Vec3::X, Vec3::Z, hx, hz, e),
                std::f32::consts::FRAC_PI_2 - 2.20,
                std::f32::consts::FRAC_PI_2 + 2.20,
            )
        })
        .collect();
    assembly.add(
        Piece::new(geom::arc_shell(&rows, 26, 0.009))
            .uv_box(helmet, 3.6, Vec2::ZERO)
            .rigid(sk::HEAD as u32),
    );
    // 头盔颅顶：闭合环壳（上缘收成小环，顶面由封边盖住）
    let crown: Vec<(f32, f32, f32, f32, f32)> = vec![
        (1.897, 0.001, 0.124, 0.130, 2.2),
        (1.930, 0.000, 0.124, 0.129, 2.2),
        (1.966, 0.000, 0.119, 0.123, 2.2),
        (2.002, 0.000, 0.106, 0.110, 2.2),
        (2.034, 0.000, 0.086, 0.089, 2.1),
        (2.058, 0.000, 0.058, 0.060, 2.0),
        (2.070, 0.000, 0.026, 0.027, 2.0),
    ];
    let rows: Vec<ArcRow> = crown
        .iter()
        .map(|&(y, z, hx, hz, e)| {
            ArcRow::new(
                Section::new(Vec3::new(0.0, y, z), Vec3::X, Vec3::Z, hx, hz, e),
                0.0,
                std::f32::consts::TAU,
            )
        })
        .collect();
    assembly.add(
        Piece::new(geom::arc_shell(&rows, 26, 0.009))
            .uv_box(helmet, 3.6, Vec2::new(0.3, 0.3))
            .rigid(sk::HEAD as u32),
    );
    // 帽檐（眉眼上方外伸的一圈硬唇）
    let brim: Vec<(f32, f32, f32, f32, f32)> = vec![
        (1.894, 0.004, 0.126, 0.135, 2.2),
        (1.908, 0.004, 0.128, 0.143, 2.1),
    ];
    let rows: Vec<ArcRow> = brim
        .iter()
        .map(|&(y, z, hx, hz, e)| {
            ArcRow::new(
                Section::new(Vec3::new(0.0, y, z), Vec3::X, Vec3::Z, hx, hz, e),
                FRONT - 1.15,
                FRONT + 1.15,
            )
        })
        .collect();
    assembly.add(
        Piece::new(geom::arc_shell(&rows, 20, 0.010))
            .uv_box(helmet, 4.0, Vec2::new(0.6, 0.6))
            .rigid(sk::HEAD as u32),
    );
    // 侧导轨（两侧各一条，夜视仪 / 手电的挂轨）
    for side in [-1.0f32, 1.0] {
        assembly.add(
            hard_plate(
                Vec3::new(0.055, 0.012, 0.005),
                0.003,
                facing(
                    Vec3::new(side * 0.128, 1.938, -0.006),
                    Vec3::new(side, 0.0, 0.0),
                    Vec3::Y,
                ),
                gunmetal,
                6.0,
                Vec2::ZERO,
            )
            .rigid(sk::HEAD as u32),
        );
        // 侧面固定螺栓
        assembly.add(
            Piece::new(geom::lathe(
                &[(0.0, -0.005), (0.008, -0.005), (0.008, 0.004), (0.0, 0.004)],
                10,
            ))
            .uv_box(gunmetal, 8.0, Vec2::new(0.2, 0.2))
            .transform(place(
                Vec3::new(side * 0.130, 1.940, 0.048),
                Quat::from_rotation_z(side * std::f32::consts::FRAC_PI_2),
            ))
            .rigid(sk::HEAD as u32),
        );
    }
    // 夜视仪座 + 前挂臂（扫掠）
    assembly.add(
        hard_plate(
            Vec3::new(0.030, 0.026, 0.009),
            0.004,
            facing(
                Vec3::new(0.0, 2.006, -0.108),
                Vec3::new(0.0, 0.42, -1.0),
                Vec3::Y,
            ),
            gunmetal,
            6.0,
            Vec2::ZERO,
        )
        .rigid(sk::HEAD as u32),
    );
    assembly.add(
        Piece::new(geom::sweep(
            &[
                Vec3::new(0.0, 2.008, -0.120),
                Vec3::new(0.0, 2.036, -0.142),
                Vec3::new(0.0, 2.052, -0.168),
            ],
            &[(0.0, 0.010, 0.007, 3.0), (1.0, 0.009, 0.006, 3.0)],
            8,
            Vec3::X,
        ))
        .uv_box(gunmetal, 8.0, Vec2::new(0.5, 0.5))
        .rigid(sk::HEAD as u32),
    );
    // 后配重包（平衡夜视仪）
    assembly.add(
        hard_plate(
            Vec3::new(0.048, 0.032, 0.020),
            0.007,
            facing(
                Vec3::new(0.0, 1.958, 0.128),
                Vec3::new(0.0, 0.25, 1.0),
                Vec3::Y,
            ),
            helmet,
            5.0,
            Vec2::new(0.7, 0.2),
        )
        .rigid(sk::HEAD as u32),
    );
    // 护目镜带：环绕面部 140° 的弧壳（暗色镜面）
    let goggles: Vec<(f32, f32, f32, f32, f32)> = vec![
        (1.846, 0.006, 0.107, 0.103, 2.4),
        (1.892, 0.004, 0.112, 0.108, 2.3),
        (1.930, 0.004, 0.112, 0.107, 2.3),
    ];
    let rows: Vec<ArcRow> = goggles
        .iter()
        .map(|&(y, z, hx, hz, e)| {
            ArcRow::new(
                Section::new(Vec3::new(0.0, y, z), Vec3::X, Vec3::Z, hx, hz, e),
                FRONT - 1.20,
                FRONT + 1.20,
            )
        })
        .collect();
    assembly.add(
        Piece::new(geom::arc_shell(&rows, 22, 0.012))
            .uv_box(visor, 3.4, Vec2::ZERO)
            .rigid(sk::HEAD as u32),
    );
    // 下颌护罩（呼吸面罩）：下半张脸的包覆
    let mask: Vec<(f32, f32, f32, f32, f32)> = vec![
        (1.744, -0.012, 0.086, 0.084, 2.4),
        (1.782, -0.004, 0.098, 0.098, 2.4),
        (1.824, 0.000, 0.104, 0.104, 2.4),
        (1.858, 0.000, 0.106, 0.104, 2.4),
    ];
    let rows: Vec<ArcRow> = mask
        .iter()
        .map(|&(y, z, hx, hz, e)| {
            ArcRow::new(
                Section::new(Vec3::new(0.0, y, z), Vec3::X, Vec3::Z, hx, hz, e),
                FRONT - 1.35,
                FRONT + 1.35,
            )
        })
        .collect();
    assembly.add(
        Piece::new(geom::arc_shell(&rows, 22, 0.014))
            .uv_box(rubber, 3.2, Vec2::new(0.4, 0.1))
            .rigid(sk::HEAD as u32),
    );
    // 呼吸阀 + 滤芯（面罩正面两片圆盘）
    for (x, radius) in [(-0.040f32, 0.021f32), (0.040, 0.021)] {
        assembly.add(
            Piece::new(geom::lathe(
                &[
                    (0.0, 0.0),
                    (radius, 0.0),
                    (radius, 0.010),
                    (radius * 0.72, 0.014),
                    (0.0, 0.014),
                ],
                14,
            ))
            .uv_box(rubber, 7.0, Vec2::new(0.3, 0.3))
            .transform(place(
                Vec3::new(x, 1.792, -0.104),
                Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2),
            ))
            .rigid(sk::HEAD as u32),
        );
    }
    // 耳罩（头盔下沿两侧的护耳垫）+ 下巴带
    for side in [-1.0f32, 1.0] {
        assembly.add(
            Piece::new(geom::superellipsoid(
                Vec3::new(0.030, 0.044, 0.048),
                3.2,
                12,
                9,
            ))
            .uv_box(undersuit, 5.0, Vec2::new(0.4, 0.4))
            .transform(Mat4::from_translation(Vec3::new(
                side * 0.112,
                1.792,
                0.008,
            )))
            .rigid(sk::HEAD as u32),
        );
    }
    assembly.add(
        Piece::new(geom::sweep(
            &[
                Vec3::new(-0.108, 1.788, 0.008),
                Vec3::new(-0.078, 1.712, -0.032),
                Vec3::new(0.000, 1.690, -0.044),
                Vec3::new(0.078, 1.712, -0.032),
                Vec3::new(0.108, 1.788, 0.008),
            ],
            &[(0.0, 0.004, 0.009, 2.4), (1.0, 0.004, 0.009, 2.4)],
            8,
            Vec3::Y,
        ))
        .uv_box(webbing, 9.0, Vec2::new(0.2, 0.5))
        .rigid(sk::HEAD as u32),
    );
    // 下巴带扣（两侧）
    for side in [-1.0f32, 1.0] {
        assembly.add(
            hard_plate(
                Vec3::new(0.011, 0.013, 0.006),
                0.003,
                facing(
                    Vec3::new(side * 0.104, 1.766, -0.014),
                    Vec3::new(side, 0.0, -0.3),
                    Vec3::Y,
                ),
                gunmetal,
                9.0,
                Vec2::ZERO,
            )
            .rigid(sk::HEAD as u32),
        );
    }
}

// ---------------------------------------------------------------------------
// 装具
// ---------------------------------------------------------------------------

/// 装具：腰带 / 带扣 / 战术背心（正背板 + MOLLE + 弹匣包 + 肩带）/ 电台 /
/// 手雷 / 水壶 / 匕首鞘。
///
/// 背心是**弧形壳**（贴合躯干曲面、封边是硬刃），弹匣包是**凸棱柱**
/// （梯形侧影 + 硬边），小件用倒角板——三种硬表面基本体各司其职，
/// 不再是一堆圆角疙瘩。
fn push_gear(assembly: &mut Assembly) {
    let leather = Surface::Leather.tile();
    let armor = Surface::Armor.tile();
    let pouch = Surface::Pouch.tile();
    let webbing = Surface::Webbing.tile();
    let trim = Surface::Trim.tile();
    let rubber = Surface::Rubber.tile();
    let gunmetal = Surface::Gunmetal.tile();
    // 腰带（闭合环带，端盖埋在躯干里）
    assembly.add(
        Piece::new(ring_shell(
            &[
                (0.996, 0.171, 0.122, 2.8),
                (1.040, 0.170, 0.121, 2.8),
                (1.078, 0.167, 0.118, 2.8),
            ],
            20,
            0.012,
        ))
        .uv_cylinder(leather, Axis::Y, 1.0, 7.0, Vec2::ZERO)
        .rigid(sk::PELVIS as u32),
    );
    // 带扣（正面黄铜方扣）
    assembly.add(
        hard_plate(
            Vec3::new(0.048, 0.034, 0.010),
            0.005,
            facing(Vec3::new(0.0, 1.038, -0.130), -Vec3::Z, Vec3::Y),
            trim,
            6.0,
            Vec2::ZERO,
        )
        .rigid(sk::PELVIS as u32),
    );
    // 腰带环 ×4（织带，压住腰带）
    for theta in [
        FRONT - 0.72,
        FRONT + 0.72,
        std::f32::consts::FRAC_PI_2 - 0.7,
        std::f32::consts::FRAC_PI_2 + 0.7,
    ] {
        let section = Section::new(
            Vec3::new(0.0, 1.038, 0.0),
            Vec3::X,
            Vec3::Z,
            0.170,
            0.121,
            2.8,
        );
        let p = section.point_at(theta, -0.002);
        let n = (p - Vec3::new(0.0, 1.038, 0.0)).normalize();
        assembly.add(
            hard_plate(
                Vec3::new(0.014, 0.052, 0.005),
                0.002,
                facing(p + n * 0.003, n, Vec3::Y),
                webbing,
                10.0,
                Vec2::new(0.3, 0.3),
            )
            .rigid(sk::PELVIS as u32),
        );
    }
    // 战术背心正面（弧壳，包住前身 224°）
    let carrier: Vec<(f32, f32, f32, f32)> = vec![
        (1.238, 0.172, 0.125, 2.6),
        (1.320, 0.184, 0.132, 2.6),
        (1.400, 0.198, 0.138, 2.6),
        (1.472, 0.206, 0.142, 2.6),
        (1.544, 0.211, 0.142, 2.6),
        (1.588, 0.208, 0.137, 2.6),
    ];
    assembly.add(
        Piece::new(arc_plate(&carrier, FRONT, 1.95, 24, 0.016))
            .uv_box(webbing, 2.6, Vec2::ZERO)
            .rigid(sk::CHEST as u32),
    );
    // 战术背心背面（弧壳，包住后背 200°）
    let back: Vec<(f32, f32, f32, f32)> = vec![
        (1.268, 0.176, 0.128, 2.6),
        (1.344, 0.190, 0.135, 2.6),
        (1.420, 0.202, 0.140, 2.6),
        (1.496, 0.207, 0.140, 2.6),
        (1.560, 0.205, 0.135, 2.6),
    ];
    assembly.add(
        Piece::new(arc_plate(
            &back,
            std::f32::consts::FRAC_PI_2,
            1.75,
            24,
            0.014,
        ))
        .uv_box(armor, 2.6, Vec2::new(0.2, 0.2))
        .rigid(sk::CHEST as u32),
    );
    // 背板硬插板（外凸一片，读作防弹插板）
    assembly.add(
        Piece::new(arc_plate(
            &[
                (1.320, 0.186, 0.150, 4.0),
                (1.400, 0.199, 0.156, 4.0),
                (1.500, 0.205, 0.156, 4.0),
            ],
            std::f32::consts::FRAC_PI_2,
            1.15,
            18,
            0.014,
        ))
        .uv_box(armor, 2.8, Vec2::new(0.6, 0.6))
        .rigid(sk::CHEST as u32),
    );
    // MOLLE 织带：3 行 × 5 列（贴在正板外表面，间距按角度均分）
    let molle_rows = [
        (1.318f32, 0.184f32, 0.132f32),
        (1.400, 0.198, 0.138),
        (1.482, 0.206, 0.142),
    ];
    for (y, hx, hz) in molle_rows {
        let center = Vec3::new(0.0, y, 0.0);
        let section = Section::new(center, Vec3::X, Vec3::Z, hx, hz, 2.6);
        for k in -2..=2 {
            let theta = FRONT + k as f32 * 0.62;
            let p = section.point_at(theta, -0.004);
            let n = (p - center).normalize();
            assembly.add(
                hard_plate(
                    Vec3::new(0.030, 0.009, 0.004),
                    0.002,
                    facing(p + n * 0.002, n, Vec3::Y),
                    webbing,
                    9.0,
                    Vec2::new(0.05 * (k + 2) as f32, 0.02 * y),
                )
                .rigid(sk::CHEST as u32),
            );
        }
    }
    // 正面弹匣包 ×3（凸棱柱 + 翻盖 + 带扣）
    for k in -1..=1 {
        let theta = FRONT + k as f32 * 0.60;
        let section = Section::new(
            Vec3::new(0.0, 1.330, 0.0),
            Vec3::X,
            Vec3::Z,
            0.184,
            0.132,
            2.6,
        );
        let p = section.point_at(theta, -0.012);
        let n = (p - Vec3::new(0.0, 1.330, 0.0)).normalize();
        let mut mesh = geom::prism(
            &[
                Vec2::new(-0.038, -0.062),
                Vec2::new(0.038, -0.062),
                Vec2::new(0.034, 0.046),
                Vec2::new(-0.034, 0.046),
            ],
            0.022,
        );
        mesh = Piece::new(mesh)
            .uv_box(pouch, 3.0, Vec2::new(0.05 * (k + 1) as f32, 0.0))
            .transform(facing(p, n, Vec3::Y))
            .mesh;
        assembly.add(Piece::new(mesh).rigid(sk::CHEST as u32));
        // 翻盖
        assembly.add(
            hard_plate(
                Vec3::new(0.039, 0.020, 0.025),
                0.006,
                facing(p + Vec3::new(0.0, 0.062, 0.0) + n * 0.004, n, Vec3::Y),
                pouch,
                4.0,
                Vec2::new(0.4, 0.2),
            )
            .rigid(sk::CHEST as u32),
        );
        // 翻盖织带 + 卡扣
        assembly.add(
            hard_plate(
                Vec3::new(0.009, 0.030, 0.006),
                0.002,
                facing(p + Vec3::new(0.0, 0.024, 0.0) + n * 0.026, n, Vec3::Y),
                webbing,
                11.0,
                Vec2::ZERO,
            )
            .rigid(sk::CHEST as u32),
        );
    }
    // 肩带 ×2：从正板上沿越过肩到背板上沿（平行传输扫掠，不扭转）
    for side in [-1.0f32, 1.0] {
        let sign = if side < 0.0 { -1.0f32 } else { 1.0 };
        let front_section = Section::new(
            Vec3::new(0.0, 1.586, 0.0),
            Vec3::X,
            Vec3::Z,
            0.208,
            0.137,
            2.6,
        );
        let back_section = Section::new(
            Vec3::new(0.0, 1.556, 0.0),
            Vec3::X,
            Vec3::Z,
            0.205,
            0.135,
            2.6,
        );
        let start = front_section.point_at(FRONT + sign * 0.78, -0.006);
        let end = back_section.point_at(std::f32::consts::FRAC_PI_2 - sign * 0.80, -0.006);
        let path = [
            start,
            Vec3::new(sign * 0.132, 1.700, -0.062),
            Vec3::new(sign * 0.118, 1.792, 0.010),
            Vec3::new(sign * 0.128, 1.716, 0.088),
            end,
        ];
        assembly.add(
            Piece::new(geom::sweep(
                &path,
                &[
                    (0.00, 0.030, 0.008, 3.0),
                    (0.35, 0.034, 0.009, 3.0),
                    (0.50, 0.034, 0.009, 3.0),
                    (0.70, 0.030, 0.008, 3.0),
                    (1.00, 0.028, 0.008, 3.0),
                ],
                12,
                Vec3::Y,
            ))
            .uv_cylinder(webbing, Axis::Y, 1.0, 6.0, Vec2::new(0.2, 0.5))
            .rigid(sk::CHEST as u32),
        );
        // 肩带上的调节扣
        assembly.add(
            hard_plate(
                Vec3::new(0.020, 0.014, 0.010),
                0.003,
                facing(
                    Vec3::new(sign * 0.126, 1.760, -0.040),
                    Vec3::new(sign * 0.5, 0.3, -0.8),
                    Vec3::Y,
                ),
                gunmetal,
                9.0,
                Vec2::ZERO,
            )
            .rigid(sk::CHEST as u32),
        );
    }
    // 左胸电台 + 天线（天线穿过肩上方，是轮廓上最醒目的识别特征）
    assembly.add(
        hard_plate(
            Vec3::new(0.030, 0.044, 0.018),
            0.006,
            facing(
                Vec3::new(-0.132, 1.470, -0.148),
                Vec3::new(-0.35, 0.0, -1.0),
                Vec3::Y,
            ),
            gunmetal,
            5.0,
            Vec2::new(0.2, 0.6),
        )
        .rigid(sk::CHEST as u32),
    );
    assembly.add(
        Piece::new(geom::sweep(
            &[
                Vec3::new(-0.134, 1.510, -0.146),
                Vec3::new(-0.146, 1.640, -0.150),
                Vec3::new(-0.152, 1.760, -0.152),
            ],
            &[
                (0.0, 0.005, 0.005, 2.0),
                (0.55, 0.004, 0.004, 2.0),
                (1.0, 0.003, 0.003, 2.0),
            ],
            8,
            Vec3::Z,
        ))
        .uv_box(gunmetal, 12.0, Vec2::new(0.3, 0.3))
        .rigid(sk::CHEST as u32),
    );
    // 右胸医疗包（IFAK：小硬包 + 红十字扣）
    assembly.add(
        hard_plate(
            Vec3::new(0.036, 0.040, 0.020),
            0.006,
            facing(
                Vec3::new(0.140, 1.440, -0.140),
                Vec3::new(0.35, 0.0, -1.0),
                Vec3::Y,
            ),
            pouch,
            5.0,
            Vec2::new(0.7, 0.3),
        )
        .rigid(sk::CHEST as u32),
    );
    assembly.add(
        hard_plate(
            Vec3::new(0.014, 0.006, 0.005),
            0.002,
            facing(
                Vec3::new(0.146, 1.462, -0.158),
                Vec3::new(0.35, 0.0, -1.0),
                Vec3::Y,
            ),
            trim,
            12.0,
            Vec2::ZERO,
        )
        .rigid(sk::CHEST as u32),
    );
    assembly.add(
        hard_plate(
            Vec3::new(0.006, 0.014, 0.005),
            0.002,
            facing(
                Vec3::new(0.146, 1.462, -0.158),
                Vec3::new(0.35, 0.0, -1.0),
                Vec3::Y,
            ),
            trim,
            12.0,
            Vec2::new(0.5, 0.5),
        )
        .rigid(sk::CHEST as u32),
    );
    // 手雷 ×2（左腰：旋转体 + 保险片）
    for (index, z) in [-0.030f32, 0.020].iter().enumerate() {
        assembly.add(
            Piece::new(geom::lathe(
                &[
                    (0.0, 0.0),
                    (0.022, 0.004),
                    (0.029, 0.016),
                    (0.030, 0.044),
                    (0.026, 0.056),
                    (0.014, 0.060),
                    (0.0, 0.062),
                ],
                12,
            ))
            .uv_box(armor, 5.0, Vec2::new(0.1 * index as f32, 0.4))
            .transform(place(
                Vec3::new(-0.196, 1.104, *z),
                Quat::from_rotation_z(0.16),
            ))
            .rigid(sk::PELVIS as u32),
        );
        assembly.add(
            hard_plate(
                Vec3::new(0.006, 0.016, 0.004),
                0.002,
                facing(
                    Vec3::new(-0.222, 1.146, *z),
                    Vec3::new(-1.0, 0.25, 0.0),
                    Vec3::Y,
                ),
                gunmetal,
                10.0,
                Vec2::ZERO,
            )
            .rigid(sk::PELVIS as u32),
        );
    }
    // 水壶（左髋后）+ 壶盖
    assembly.add(
        Piece::new(geom::lathe(
            &[
                (0.0, 0.0),
                (0.040, 0.006),
                (0.048, 0.020),
                (0.048, 0.100),
                (0.038, 0.114),
                (0.024, 0.120),
                (0.024, 0.130),
                (0.0, 0.130),
            ],
            14,
        ))
        .uv_box(rubber, 4.0, Vec2::new(0.2, 0.7))
        .transform(place(Vec3::new(-0.176, 0.986, 0.096), Quat::IDENTITY))
        .rigid(sk::PELVIS as u32),
    );
    // 匕首鞘（右髋：硬鞘 + 两条绑带）
    assembly.add(
        hard_plate(
            Vec3::new(0.020, 0.086, 0.030),
            0.008,
            facing(
                Vec3::new(0.190, 1.010, -0.048),
                Vec3::new(1.0, 0.0, -0.15),
                Vec3::Y,
            ),
            leather,
            5.0,
            Vec2::new(0.6, 0.6),
        )
        .rigid(sk::PELVIS as u32),
    );
    for y in [0.960f32, 1.056] {
        assembly.add(
            hard_plate(
                Vec3::new(0.008, 0.010, 0.032),
                0.002,
                facing(
                    Vec3::new(0.200, y, -0.048),
                    Vec3::new(1.0, 0.0, 0.0),
                    Vec3::Y,
                ),
                webbing,
                11.0,
                Vec2::new(0.4, 0.4),
            )
            .rigid(sk::PELVIS as u32),
        );
    }
    // 匕首柄（露出鞘口）
    assembly.add(
        Piece::new(geom::lathe(
            &[
                (0.0, 0.0),
                (0.013, 0.0),
                (0.015, 0.012),
                (0.011, 0.072),
                (0.013, 0.080),
                (0.0, 0.086),
            ],
            10,
        ))
        .uv_box(Surface::Grip.tile(), 7.0, Vec2::ZERO)
        .transform(place(
            Vec3::new(0.190, 1.096, -0.048),
            Quat::from_rotation_z(0.10),
        ))
        .rigid(sk::PELVIS as u32),
    );
}

// ---------------------------------------------------------------------------
// 刀
// ---------------------------------------------------------------------------

/// 刀（蒙皮）：配重球 / 缠柄（含金属丝缠绕）/ 护手 / 护格 / 根部厚段 / 刃。
/// 握把段挂 [`SWORD`]，刃挂 [`SWORD_BLADE`]（模型空间）。
///
/// 刃的截面是**双刃菱形**（宽度沿 X、厚度沿 Z）——第三人称相机在角色背后，
/// 宽度朝屏幕才看得见刀面而不是一条刀脊线（第一版沿 Z 的教训）。
fn push_sword(rig: &Rig, assembly: &mut Assembly) {
    let armor = Surface::Armor.tile();
    let trim = Surface::Trim.tile();
    let grip = Surface::Grip.tile();
    let gunmetal = Surface::Gunmetal.tile();
    let blade_tex = Surface::Blade.tile();
    let leather = Surface::Leather.tile();
    let hilt = Mat4::from_translation(rig.p(sk::SWORD));
    // 配重球 + 尾环（穿绳用）
    assembly.add(
        Piece::new(geom::lathe(
            &[
                (0.000, 0.000),
                (0.024, 0.004),
                (0.030, 0.016),
                (0.030, 0.034),
                (0.022, 0.046),
                (0.011, 0.050),
                (0.000, 0.052),
            ],
            14,
        ))
        .uv_box(trim, 6.0, Vec2::ZERO)
        .transform(hilt * Mat4::from_translation(Vec3::new(0.0, 0.070, 0.0)))
        .rigid(sk::SWORD as u32),
    );
    // 缠柄（皮革芯）
    let grip_profile: Vec<(f32, f32, f32, f32)> = vec![
        (0.00, 0.027, 0.029, 2.4),
        (0.50, 0.026, 0.028, 2.4),
        (1.00, 0.025, 0.027, 2.4),
    ];
    assembly.add(
        Piece::new(limb(
            Vec3::new(0.0, 0.076, 0.0),
            Vec3::new(0.0, -0.070, 0.0),
            &grip_profile,
            12,
        ))
        .uv_cylinder(grip, Axis::Y, 1.0, 5.0, Vec2::ZERO)
        .transform(hilt)
        .rigid(sk::SWORD as u32),
    );
    // 金属丝缠绕（螺旋扫掠，8 圈）
    let mut coil: Vec<Vec3> = Vec::new();
    for index in 0..=64 {
        let t = index as f32 / 64.0;
        let theta = t * 8.0 * std::f32::consts::TAU;
        coil.push(Vec3::new(
            0.0285 * theta.cos(),
            0.072 - t * 0.140,
            0.0305 * theta.sin(),
        ));
    }
    assembly.add(
        Piece::new(geom::sweep(
            &coil,
            &[(0.0, 0.0028, 0.0028, 2.0), (1.0, 0.0028, 0.0028, 2.0)],
            6,
            Vec3::Y,
        ))
        .uv_box(gunmetal, 30.0, Vec2::new(0.5, 0.5))
        .transform(hilt)
        .rigid(sk::SWORD as u32),
    );
    // 护手横格（硬板 + 两端球头）+ 皮革垫圈
    assembly.add(
        hard_plate(
            Vec3::new(0.084, 0.014, 0.026),
            0.005,
            hilt * Mat4::from_translation(Vec3::new(0.0, -0.084, 0.0)),
            armor,
            4.0,
            Vec2::new(0.3, 0.3),
        )
        .rigid(sk::SWORD as u32),
    );
    for side in [-1.0f32, 1.0] {
        assembly.add(
            Piece::new(geom::superellipsoid(
                Vec3::new(0.019, 0.021, 0.022),
                3.4,
                10,
                7,
            ))
            .uv_box(trim, 7.0, Vec2::new(0.4, 0.4))
            .transform(hilt * Mat4::from_translation(Vec3::new(side * 0.080, -0.088, 0.0)))
            .rigid(sk::SWORD as u32),
        );
    }
    assembly.add(
        Piece::new(geom::lathe(
            &[(0.030, 0.0), (0.032, 0.004), (0.030, 0.008), (0.0, 0.009)],
            14,
        ))
        .uv_box(leather, 8.0, Vec2::ZERO)
        .transform(hilt * Mat4::from_translation(Vec3::new(0.0, -0.100, 0.0)))
        .rigid(sk::SWORD as u32),
    );
    // 根部厚段（ricasso）
    assembly.add(
        Piece::new(geom::superellipsoid(
            Vec3::new(0.027, 0.040, 0.014),
            5.0,
            10,
            7,
        ))
        .uv_box(gunmetal, 6.0, Vec2::new(0.5, 0.5))
        .transform(hilt * Mat4::from_translation(Vec3::new(0.0, -0.142, 0.0)))
        .rigid(sk::SWORD as u32),
    );
    // 刃（双刃菱形截面）
    let blade_matrix = Mat4::from_translation(rig.p(sk::SWORD_BLADE));
    let blade_profile: Vec<(f32, f32, f32, f32)> = vec![
        (0.00, 0.0362, 0.0112, 1.0),
        (0.08, 0.0382, 0.0120, 1.0),
        (0.45, 0.0350, 0.0106, 1.0),
        (0.75, 0.0282, 0.0082, 1.0),
        (0.92, 0.0142, 0.0046, 1.0),
        (1.00, 0.0050, 0.0018, 1.0),
    ];
    assembly.add(
        Piece::new(limb(
            Vec3::new(0.0, 0.300, 0.0),
            Vec3::new(0.0, -0.455, 0.0),
            &blade_profile,
            10,
        ))
        .uv_box(blade_tex, 4.0, Vec2::ZERO)
        .transform(blade_matrix)
        .rigid(sk::SWORD_BLADE as u32),
    );
    // 两侧刃口斜面（略凸出的一线亮钢：读作开刃的斜面）
    for side in [-1.0f32, 1.0] {
        let bevel: Vec<Section> = [
            (0.300f32, 0.0330f32, 0.0088f32),
            (0.180, 0.0352, 0.0098),
            (-0.040, 0.0340, 0.0088),
            (-0.240, 0.0290, 0.0070),
            (-0.372, 0.0160, 0.0040),
        ]
        .iter()
        .map(|&(y, w, t)| {
            Section::new(
                Vec3::new(side * (w - 0.006), y, 0.0),
                Vec3::X,
                Vec3::Z,
                0.0060,
                t + 0.0006,
                1.2,
            )
        })
        .collect();
        let mut bevel = bevel;
        if side < 0.0 {
            bevel.reverse();
        }
        assembly.add(
            Piece::new(geom::loft(&bevel, 8, true))
                .uv_box(blade_tex, 8.0, Vec2::new(0.2, 0.8))
                .transform(blade_matrix)
                .rigid(sk::SWORD_BLADE as u32),
        );
    }
}

// ---------------------------------------------------------------------------
// 步枪
// ---------------------------------------------------------------------------

/// 步枪（**刚性网格**：几何写在 [`RIFLE`] 骨节点的局部坐标系，由节点变换
/// 驱动——枪不参与蒙皮，于是「隐藏枪械」仍是 [`set_subtree_hidden`] + RIFLE
/// 的一句（敌人是持刀近战，不带枪），蒙皮升级没有破坏这条表现路径）。
///
/// 局部坐标：**−Z 是枪口**、+Y 向上、+X 是角色右手方向；右手 IK 挂点在
/// (0, −0.05, 0.12)、左手（护木）挂点在 (0, −0.03, −0.18)（见骨架表）。
fn push_rifle(assembly: &mut Assembly) {
    let gunmetal = Surface::Gunmetal.tile();
    let grip = Surface::Grip.tile();
    let visor = Surface::Visor.tile();
    let rubber = Surface::Rubber.tile();
    let armor = Surface::Armor.tile();
    let webbing = Surface::Webbing.tile();
    let part = |mesh: MeshData, tile: Tile, scale: f32, offset: Vec2, at: Vec3, rot: Quat| {
        Piece::new(mesh)
            .uv_box(tile, scale, offset)
            .transform(place(at, rot))
    };
    // 机匣（下机匣 + 上机匣 + 弹匣井）
    assembly.add_rigid(part(
        geom::plate(Vec3::new(0.023, 0.030, 0.128), 0.005),
        gunmetal,
        4.0,
        Vec2::ZERO,
        Vec3::new(0.0, -0.006, -0.020),
        Quat::IDENTITY,
    ));
    assembly.add_rigid(part(
        geom::plate(Vec3::new(0.021, 0.014, 0.118), 0.005),
        gunmetal,
        4.5,
        Vec2::new(0.4, 0.4),
        Vec3::new(0.0, 0.034, -0.030),
        Quat::IDENTITY,
    ));
    // 顶部皮卡汀尼导轨：基座 + 6 段齿
    assembly.add_rigid(part(
        geom::plate(Vec3::new(0.011, 0.006, 0.128), 0.002),
        gunmetal,
        6.0,
        Vec2::new(0.2, 0.6),
        Vec3::new(0.0, 0.052, -0.030),
        Quat::IDENTITY,
    ));
    for index in 0..6 {
        let z = 0.078 - index as f32 * 0.022;
        assembly.add_rigid(part(
            geom::plate(Vec3::new(0.013, 0.005, 0.007), 0.002),
            gunmetal,
            8.0,
            Vec2::new(0.1 * index as f32, 0.1),
            Vec3::new(0.0, 0.061, z),
            Quat::IDENTITY,
        ));
    }
    // 抛壳口 + 偏转板 + 拉机柄（右侧）
    assembly.add_rigid(part(
        geom::plate(Vec3::new(0.004, 0.013, 0.030), 0.002),
        armor,
        9.0,
        Vec2::new(0.3, 0.3),
        Vec3::new(0.024, 0.010, -0.042),
        Quat::IDENTITY,
    ));
    assembly.add_rigid(part(
        geom::plate(Vec3::new(0.008, 0.010, 0.012), 0.003),
        gunmetal,
        8.0,
        Vec2::new(0.6, 0.2),
        Vec3::new(0.026, 0.020, -0.078),
        Quat::IDENTITY,
    ));
    assembly.add_rigid(part(
        geom::plate(Vec3::new(0.016, 0.007, 0.026), 0.003),
        gunmetal,
        7.0,
        Vec2::new(0.2, 0.8),
        Vec3::new(0.026, 0.036, -0.012),
        Quat::IDENTITY,
    ));
    // 枪管 + 导气箍 + 枪口制退器（含泄气槽）
    let barrel_profile: Vec<(f32, f32, f32, f32)> =
        vec![(0.0, 0.0126, 0.0126, 2.0), (1.0, 0.0104, 0.0104, 2.0)];
    assembly.add_rigid(
        Piece::new(bore(
            Vec3::new(0.0, 0.008, -0.150),
            Vec3::new(0.0, 0.008, -0.462),
            &barrel_profile,
            12,
        ))
        .uv_box(gunmetal, 8.0, Vec2::new(0.2, 0.2)),
    );
    assembly.add_rigid(part(
        geom::plate(Vec3::new(0.018, 0.018, 0.016), 0.004),
        gunmetal,
        7.0,
        Vec2::new(0.5, 0.2),
        Vec3::new(0.0, 0.008, -0.212),
        Quat::IDENTITY,
    ));
    let brake_profile: Vec<(f32, f32, f32, f32)> = vec![
        (0.0, 0.0174, 0.0174, 3.0),
        (0.35, 0.0174, 0.0174, 3.0),
        (1.0, 0.0160, 0.0160, 3.0),
    ];
    assembly.add_rigid(
        Piece::new(bore(
            Vec3::new(0.0, 0.008, -0.464),
            Vec3::new(0.0, 0.008, -0.512),
            &brake_profile,
            12,
        ))
        .uv_box(gunmetal, 9.0, Vec2::new(0.4, 0.4)),
    );
    for z in [-0.478f32, -0.498] {
        assembly.add_rigid(part(
            geom::plate(Vec3::new(0.019, 0.005, 0.004), 0.001),
            armor,
            12.0,
            Vec2::new(0.2 * z, 0.2),
            Vec3::new(0.0, 0.021, z),
            Quat::IDENTITY,
        ));
    }
    // 护木（八棱柱：凸棱柱的一次实战）+ M-LOK 槽
    let octagon: Vec<Vec2> = (0..8)
        .map(|index| {
            let theta = std::f32::consts::TAU * index as f32 / 8.0 + std::f32::consts::FRAC_PI_8;
            Vec2::new(0.030 * theta.cos(), 0.034 * theta.sin())
        })
        .collect();
    assembly.add_rigid(part(
        geom::prism(&octagon, 0.104),
        gunmetal,
        4.5,
        Vec2::new(0.1, 0.6),
        Vec3::new(0.0, 0.002, -0.258),
        Quat::IDENTITY,
    ));
    for index in 0..4 {
        let z = -0.190 - index as f32 * 0.044;
        for side in [-1.0f32, 1.0] {
            assembly.add_rigid(part(
                geom::plate(Vec3::new(0.004, 0.008, 0.012), 0.002),
                armor,
                10.0,
                Vec2::new(0.1 * index as f32, 0.7),
                Vec3::new(side * 0.030, -0.004, z),
                Quat::IDENTITY,
            ));
        }
    }
    // 前握把（护木下方：左手 IK 挂点 (0, −0.03, −0.18) 正落在它上面）
    assembly.add_rigid(part(
        geom::plate(Vec3::new(0.016, 0.034, 0.017), 0.005),
        grip,
        5.0,
        Vec2::new(0.6, 0.3),
        Vec3::new(0.0, -0.040, -0.180),
        Quat::from_rotation_x(-0.10),
    ));
    // 弹匣（前倾棱柱 + 底板）
    let magazine: Vec<Vec2> = vec![
        Vec2::new(-0.019, -0.062),
        Vec2::new(0.019, -0.062),
        Vec2::new(0.021, 0.052),
        Vec2::new(-0.021, 0.052),
    ];
    assembly.add_rigid(part(
        geom::prism(&magazine, 0.034),
        gunmetal,
        5.0,
        Vec2::new(0.7, 0.1),
        Vec3::new(0.0, -0.093, -0.052),
        Quat::from_rotation_x(0.22),
    ));
    assembly.add_rigid(part(
        geom::plate(Vec3::new(0.023, 0.008, 0.036), 0.003),
        rubber,
        7.0,
        Vec2::new(0.5, 0.5),
        Vec3::new(0.0, -0.148, -0.064),
        Quat::from_rotation_x(0.22),
    ));
    // 握把（后倾 + 三道指槽）+ 扳机护圈 + 扳机
    let grip_sections: Vec<(f32, f32, f32, f32)> = vec![
        (0.00, 0.019, 0.030, 3.0),
        (0.45, 0.020, 0.032, 3.0),
        (1.00, 0.021, 0.034, 3.0),
    ];
    let grip_mesh = Piece::new(bore(
        Vec3::new(0.0, 0.014, 0.0),
        Vec3::new(0.0, -0.088, 0.026),
        &grip_sections,
        12,
    ))
    .uv_box(grip, 6.0, Vec2::ZERO)
    .transform(place(
        Vec3::new(0.0, -0.048, 0.106),
        Quat::from_rotation_x(-0.22),
    ));
    assembly.add_rigid(grip_mesh);
    for index in 0..3 {
        let y = -0.014 - index as f32 * 0.026;
        assembly.add_rigid(part(
            geom::plate(Vec3::new(0.021, 0.004, 0.033), 0.002),
            grip,
            9.0,
            Vec2::new(0.2 * index as f32, 0.4),
            Vec3::new(0.0, y, 0.112),
            Quat::from_rotation_x(-0.22),
        ));
    }
    assembly.add_rigid(
        Piece::new(geom::sweep(
            &[
                Vec3::new(0.0, -0.019, 0.052),
                Vec3::new(0.0, -0.032, 0.058),
                Vec3::new(0.0, -0.032, 0.086),
                Vec3::new(0.0, -0.010, 0.096),
            ],
            &[(0.0, 0.004, 0.008, 3.0), (1.0, 0.004, 0.008, 3.0)],
            8,
            Vec3::X,
        ))
        .uv_box(gunmetal, 10.0, Vec2::new(0.3, 0.3)),
    );
    assembly.add_rigid(part(
        geom::plate(Vec3::new(0.005, 0.014, 0.005), 0.002),
        gunmetal,
        12.0,
        Vec2::ZERO,
        Vec3::new(0.0, -0.011, 0.070),
        Quat::from_rotation_x(0.30),
    ));
    // 弹匣释放钮 + 弹匣卡榫 + 保险
    assembly.add_rigid(part(
        geom::plate(Vec3::new(0.008, 0.008, 0.008), 0.003),
        gunmetal,
        11.0,
        Vec2::new(0.5, 0.5),
        Vec3::new(0.024, -0.020, 0.020),
        Quat::IDENTITY,
    ));
    assembly.add_rigid(part(
        geom::plate(Vec3::new(0.004, 0.007, 0.012), 0.002),
        gunmetal,
        12.0,
        Vec2::new(0.2, 0.6),
        Vec3::new(-0.024, 0.004, 0.030),
        Quat::IDENTITY,
    ));
    // 枪托：托身 + 贴腮板 + 托底垫 + 调节杆
    assembly.add_rigid(part(
        geom::plate(Vec3::new(0.019, 0.032, 0.096), 0.006),
        gunmetal,
        4.5,
        Vec2::new(0.3, 0.8),
        Vec3::new(0.0, -0.004, 0.194),
        Quat::from_rotation_x(0.06),
    ));
    assembly.add_rigid(part(
        geom::plate(Vec3::new(0.017, 0.010, 0.052), 0.004),
        gunmetal,
        6.0,
        Vec2::new(0.7, 0.3),
        Vec3::new(0.0, 0.036, 0.206),
        Quat::from_rotation_x(0.06),
    ));
    assembly.add_rigid(part(
        geom::plate(Vec3::new(0.019, 0.038, 0.014), 0.005),
        rubber,
        6.0,
        Vec2::new(0.5, 0.5),
        Vec3::new(0.0, -0.006, 0.296),
        Quat::IDENTITY,
    ));
    assembly.add_rigid(part(
        geom::plate(Vec3::new(0.004, 0.012, 0.030), 0.002),
        armor,
        11.0,
        Vec2::new(0.2, 0.4),
        Vec3::new(0.021, -0.020, 0.180),
        Quat::IDENTITY,
    ));
    // 光学瞄具：镜筒 + 前后座 + 两片镜片 + 调节旋钮
    let scope_profile: Vec<(f32, f32, f32, f32)> = vec![
        (0.0, 0.0265, 0.0265, 2.2),
        (0.5, 0.0255, 0.0255, 2.2),
        (1.0, 0.0275, 0.0275, 2.2),
    ];
    assembly.add_rigid(
        Piece::new(bore(
            Vec3::new(0.0, 0.082, -0.024),
            Vec3::new(0.0, 0.082, -0.146),
            &scope_profile,
            14,
        ))
        .uv_box(gunmetal, 5.0, Vec2::new(0.8, 0.3)),
    );
    for z in [-0.046f32, -0.126] {
        assembly.add_rigid(part(
            geom::plate(Vec3::new(0.016, 0.017, 0.011), 0.004),
            gunmetal,
            8.0,
            Vec2::new(0.2, 0.9),
            Vec3::new(0.0, 0.062, z),
            Quat::IDENTITY,
        ));
    }
    for (z, scale) in [(-0.148f32, 0.0255f32), (-0.022, 0.0245)] {
        assembly.add_rigid(part(
            geom::lathe(
                &[(0.0, 0.0), (scale, 0.001), (scale, 0.005), (0.0, 0.006)],
                14,
            ),
            visor,
            5.0,
            Vec2::ZERO,
            Vec3::new(0.0, 0.082, z),
            Quat::from_rotation_x(if z < -0.1 {
                -std::f32::consts::FRAC_PI_2
            } else {
                std::f32::consts::FRAC_PI_2
            }),
        ));
    }
    assembly.add_rigid(part(
        geom::plate(Vec3::new(0.010, 0.012, 0.010), 0.003),
        gunmetal,
        12.0,
        Vec2::new(0.6, 0.6),
        Vec3::new(0.0, 0.108, -0.086),
        Quat::IDENTITY,
    ));
    // 枪背带（从托底绕到护木下方：平行传输扫掠）
    assembly.add_rigid(
        Piece::new(geom::sweep(
            &[
                Vec3::new(-0.020, -0.030, 0.286),
                Vec3::new(-0.036, -0.086, 0.196),
                Vec3::new(-0.038, -0.098, -0.010),
                Vec3::new(-0.034, -0.070, -0.180),
            ],
            &[(0.0, 0.010, 0.003, 3.0), (1.0, 0.010, 0.003, 3.0)],
            8,
            Vec3::Y,
        ))
        .uv_box(webbing, 6.0, Vec2::new(0.2, 0.4)),
    );
}
