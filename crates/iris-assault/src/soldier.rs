//! 士兵模型：蒙皮人形 + 装备 + 武器（程序化 · 单网格单材质 · 零手绘资产）。
//!
//! 本模块把「一具能看的士兵」拆成**可复算的参数化部件**：躯干 / 四肢用
//! 超椭圆截面放样（[`crate::geom::tube`]），护甲板 / 头盔 / 弹匣 / 道具用
//! 超椭球（[`crate::geom::superellipsoid`]），柱础 / 穹顶用旋转体
//! （[`crate::geom::lathe`]）。全部顶点带蒙皮权重（沿骨链平滑混合，装备
//! 刚性绑定）与图集 UV，合并成**一张**带 TANGENT 的蒙皮网格。
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
//! # 诚实限制
//! - 体型是**一份参数**（身高 ≈ 1.85 m 的成年男性士兵）：没有体型系统 /
//!   没有面部（头盔面罩遮住），换体型要改本模块参数；
//! - 手指是**整手一块**（加拇指块）——武器握持由手骨 IK 摆位，不做逐指弯曲；
//! - 布料无蒙皮褶皱（没有姿态空间变形 / 布料模拟），关节弯曲处靠权重混合
//!   与装备遮挡；
//! - 左右对称部件由同一函数镜像生成（改一侧即改两侧），不做非对称装备。

use glam::{Mat4, Quat, Vec2, Vec3};
use iris_engine::prelude::MeshData;

use crate::geom::{self, Assembly, Axis, ChainBone, Piece};
use crate::skeleton as sk;
use crate::soldier_tex::Surface;

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

/// 竖直肢体放样：轴向下 / 向上，剖面 `(t, 半宽 X, 半深 Z, 指数)`。
fn limb(a: Vec3, b: Vec3, profile: &[(f32, f32, f32, f32)], segments: u32) -> MeshData {
    geom::tube(a, b, profile, segments, Vec3::Z)
}

/// 沿 Z 轴放样（枪管）：剖面 `(t, 半宽 X, 半高 Y, 指数)`。
fn bore(a: Vec3, b: Vec3, profile: &[(f32, f32, f32, f32)], segments: u32) -> MeshData {
    geom::tube(a, b, profile, segments, Vec3::Y)
}

/// 旋转 + 平移的摆放矩阵。
fn place(at: Vec3, rot: Quat) -> Mat4 {
    Mat4::from_rotation_translation(rot, at)
}

/// 构建士兵网格（纯函数：同参数逐位同输出）。
pub fn build_soldier_mesh() -> SoldierMesh {
    let rig = Rig::new();
    let mut assembly = Assembly::new();
    push_torso(&rig, &mut assembly);
    push_legs(&rig, &mut assembly);
    push_arms(&rig, &mut assembly);
    push_head(&rig, &mut assembly);
    push_gear(&rig, &mut assembly);
    push_sword(&rig, &mut assembly);
    let inverse_bind = rig.world.iter().map(|m| m.inverse()).collect();
    SoldierMesh {
        mesh: assembly.finish(),
        inverse_bind,
    }
}

/// 构建步枪**刚性**网格（[RIFLE] 骨节点局部坐标系，无蒙皮属性）。
///
/// 单独成网格的理由：枪要能被 [set_subtree_hidden] + RIFLE 整支隐藏（敌人
/// 是持刀近战、不带枪）——蒙皮网格是一整块实体，逐骨隐藏无从下手。直接挂
/// 骨节点的普通网格由节点变换驱动，与蒙皮前的表现路径逐字相同。
pub fn build_rifle_mesh() -> MeshData {
    let mut assembly = Assembly::new();
    push_rifle(&mut assembly);
    assembly.finish()
}

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

/// 躯干（含髋部）：一根从会阴到肩线的放样管——胸宽腰窄的连续体型。
fn push_torso(rig: &Rig, assembly: &mut Assembly) {
    let tile = Surface::ClothTorso.tile();
    let sections: Vec<(f32, f32, f32, f32)> = vec![
        (0.870, 0.112, 0.094, 2.2),
        (0.920, 0.150, 0.112, 2.3),
        (0.985, 0.160, 0.118, 2.4),
        (1.055, 0.147, 0.107, 2.4),
        (1.125, 0.133, 0.097, 2.3),
        (1.215, 0.144, 0.104, 2.3),
        (1.310, 0.170, 0.118, 2.4),
        (1.410, 0.190, 0.128, 2.4),
        (1.505, 0.196, 0.128, 2.5),
        (1.590, 0.183, 0.117, 2.6),
        (1.660, 0.142, 0.100, 2.6),
        (1.700, 0.098, 0.086, 2.4),
    ];
    let sections: Vec<geom::Section> = sections
        .iter()
        .map(|&(y, hx, hz, e)| {
            geom::Section::new(Vec3::new(0.0, y, 0.0), Vec3::X, Vec3::Z, hx, hz, e)
        })
        .collect();
    let piece = Piece::new(geom::loft(&sections, 16, true))
        .uv_cylinder(tile, Axis::Y, 1.0, 2.0, Vec2::ZERO)
        .blend_chain(&spine_chain(rig), 0.13);
    assembly.add(piece);
}

/// 双腿（大腿 / 小腿 / 靴筒 / 靴体 / 鞋底 / 鞋头 / 后跟）。
fn push_legs(rig: &Rig, assembly: &mut Assembly) {
    let pants = Surface::ClothPants.tile();
    let boot = Surface::Boot.tile();
    let rubber = Surface::Rubber.tile();
    for (thigh, shin, foot) in [
        (sk::L_THIGH, sk::L_SHIN, sk::L_FOOT),
        (sk::R_THIGH, sk::R_SHIN, sk::R_FOOT),
    ] {
        let (hip, knee, ankle) = (rig.p(thigh), rig.p(shin), rig.p(foot));
        let chain = leg_chain(rig, thigh, shin, foot);
        // 大腿：髋部饱满、膝上收窄
        let thigh_profile: Vec<(f32, f32, f32, f32)> = vec![
            (0.00, 0.098, 0.104, 3.0),
            (0.18, 0.100, 0.108, 3.0),
            (0.55, 0.086, 0.094, 3.0),
            (0.85, 0.074, 0.080, 3.0),
            (1.00, 0.069, 0.075, 3.0),
        ];
        assembly.add(
            Piece::new(limb(hip, knee, &thigh_profile, 12))
                .uv_cylinder(pants, Axis::Y, 1.0, 2.2, Vec2::ZERO)
                .blend_chain(&chain, 0.10),
        );
        // 小腿：腓肠肌鼓起后收向踝
        let shin_profile: Vec<(f32, f32, f32, f32)> = vec![
            (0.00, 0.070, 0.076, 3.0),
            (0.22, 0.073, 0.086, 3.0),
            (0.55, 0.060, 0.068, 3.0),
            (0.85, 0.050, 0.055, 2.8),
            (1.00, 0.048, 0.053, 2.8),
        ];
        assembly.add(
            Piece::new(limb(knee, ankle, &shin_profile, 12))
                .uv_cylinder(pants, Axis::Y, 1.0, 2.4, Vec2::ZERO)
                .blend_chain(&chain, 0.10),
        );
        // 靴筒（踝上）：只混小腿 / 脚
        let shaft_profile: Vec<(f32, f32, f32, f32)> = vec![
            (0.00, 0.085, 0.089, 2.8),
            (0.55, 0.081, 0.086, 2.8),
            (1.00, 0.075, 0.079, 2.8),
        ];
        assembly.add(
            Piece::new(limb(
                ankle + Vec3::new(0.0, 0.215, 0.0),
                ankle,
                &shaft_profile,
                12,
            ))
            .uv_cylinder(boot, Axis::Y, 1.0, 3.2, Vec2::ZERO)
            .blend_chain(&ankle_chain(rig, shin, foot), 0.06),
        );
        // 靴体：踝关节为轴，鞋底贴地（y = 0）
        assembly.add(
            Piece::new(geom::superellipsoid(
                Vec3::new(0.078, 0.055, 0.132),
                4.0,
                14,
                9,
            ))
            .uv_box(boot, 3.4, Vec2::ZERO)
            .transform(Mat4::from_translation(
                ankle + Vec3::new(0.0, 0.055, -0.055),
            ))
            .rigid(foot as u32),
        );
        // 鞋头 / 后跟
        assembly.add(
            Piece::new(geom::superellipsoid(
                Vec3::new(0.072, 0.040, 0.042),
                4.5,
                12,
                8,
            ))
            .uv_box(boot, 4.0, Vec2::new(0.3, 0.2))
            .transform(Mat4::from_translation(
                ankle + Vec3::new(0.0, 0.044, -0.162),
            ))
            .rigid(foot as u32),
        );
        assembly.add(
            Piece::new(geom::superellipsoid(
                Vec3::new(0.070, 0.038, 0.040),
                5.0,
                12,
                8,
            ))
            .uv_box(boot, 4.0, Vec2::new(0.6, 0.1))
            .transform(Mat4::from_translation(ankle + Vec3::new(0.0, 0.034, 0.058)))
            .rigid(foot as u32),
        );
        // 鞋底：扁平圆角块（橡胶）
        assembly.add(
            Piece::new(geom::superellipsoid(
                Vec3::new(0.083, 0.021, 0.142),
                8.0,
                14,
                6,
            ))
            .uv_box(rubber, 3.0, Vec2::ZERO)
            .transform(Mat4::from_translation(
                ankle + Vec3::new(0.0, 0.021, -0.052),
            ))
            .rigid(foot as u32),
        );
    }
}

/// 双臂（上臂 / 前臂 / 护腕 / 肘垫 / 手 + 拇指）。
fn push_arms(rig: &Rig, assembly: &mut Assembly) {
    let cloth = Surface::ClothTorso.tile();
    let armor = Surface::Armor.tile();
    let glove = Surface::Glove.tile();
    for (upper, fore, hand) in [
        (sk::L_UPPERARM, sk::L_FOREARM, sk::L_HAND),
        (sk::R_UPPERARM, sk::R_FOREARM, sk::R_HAND),
    ] {
        let (shoulder, elbow, wrist) = (rig.p(upper), rig.p(fore), rig.p(hand));
        let chain = arm_chain(rig, upper, fore, hand);
        let side = if shoulder.x < 0.0 { -1.0f32 } else { 1.0f32 };
        // 三角肌：肩关节在 x = ±0.34、躯干半宽 0.20 —— 不补这块体积，
        // 上臂与胸之间会露出一段空气（第一版预览实测的「断臂」）。
        let deltoid_chain = vec![rig.bone(sk::CHEST, shoulder), rig.bone(upper, elbow)];
        assembly.add(
            Piece::new(geom::superellipsoid(
                Vec3::new(0.110, 0.100, 0.114),
                2.8,
                14,
                10,
            ))
            .uv_box(cloth, 3.0, Vec2::new(0.2, 0.5))
            .transform(Mat4::from_translation(
                shoulder + Vec3::new(-side * 0.028, -0.014, 0.0),
            ))
            .blend_chain(&deltoid_chain, 0.10),
        );
        let upper_profile: Vec<(f32, f32, f32, f32)> = vec![
            (0.00, 0.077, 0.080, 2.6),
            (0.22, 0.075, 0.078, 2.6),
            (0.70, 0.061, 0.065, 2.6),
            (1.00, 0.056, 0.060, 2.6),
        ];
        assembly.add(
            Piece::new(limb(shoulder, elbow, &upper_profile, 12))
                .uv_cylinder(cloth, Axis::Y, 1.0, 2.6, Vec2::ZERO)
                .blend_chain(&chain, 0.09),
        );
        let fore_profile: Vec<(f32, f32, f32, f32)> = vec![
            (0.00, 0.058, 0.062, 2.6),
            (0.22, 0.062, 0.066, 2.6),
            (0.75, 0.048, 0.052, 2.6),
            (1.00, 0.044, 0.048, 2.6),
        ];
        assembly.add(
            Piece::new(limb(elbow, wrist, &fore_profile, 12))
                .uv_cylinder(cloth, Axis::Y, 1.0, 2.6, Vec2::ZERO)
                .blend_chain(&chain, 0.09),
        );
        // 护腕：前臂 22%..95% 外包一层装甲
        let bracer: Vec<(f32, f32, f32, f32)> = vec![
            (0.00, 0.063, 0.067, 3.6),
            (0.55, 0.054, 0.058, 3.6),
            (1.00, 0.050, 0.054, 3.6),
        ];
        assembly.add(
            Piece::new(limb(
                elbow.lerp(wrist, 0.22),
                elbow.lerp(wrist, 0.95),
                &bracer,
                12,
            ))
            .uv_cylinder(armor, Axis::Y, 1.0, 3.0, Vec2::new(0.35, 0.15))
            .rigid(fore as u32),
        );
        // 肘垫
        assembly.add(
            Piece::new(geom::superellipsoid(
                Vec3::new(0.058, 0.052, 0.058),
                3.5,
                12,
                8,
            ))
            .uv_box(armor, 4.0, Vec2::new(0.2, 0.4))
            .transform(Mat4::from_translation(
                elbow + Vec3::new(0.0, -0.004, 0.012),
            ))
            .rigid(fore as u32),
        );
        // 手（整手一块）
        assembly.add(
            Piece::new(geom::superellipsoid(
                Vec3::new(0.046, 0.079, 0.034),
                3.2,
                12,
                9,
            ))
            .uv_box(glove, 4.0, Vec2::ZERO)
            .transform(Mat4::from_translation(
                wrist + Vec3::new(0.0, -0.048, -0.006),
            ))
            .rigid(hand as u32),
        );
        // 腕环（护腕下沿与手套的接缝）
        let cuff: Vec<(f32, f32, f32, f32)> =
            vec![(0.0, 0.052, 0.056, 3.4), (1.0, 0.050, 0.054, 3.4)];
        assembly.add(
            Piece::new(limb(
                wrist + Vec3::new(0.0, 0.022, 0.0),
                wrist + Vec3::new(0.0, -0.012, 0.0),
                &cuff,
                12,
            ))
            .uv_cylinder(armor, Axis::Y, 1.0, 6.0, Vec2::new(0.6, 0.4))
            .rigid(hand as u32),
        );
        // 拇指（朝身体内侧）
        assembly.add(
            Piece::new(geom::superellipsoid(
                Vec3::new(0.021, 0.044, 0.023),
                3.5,
                10,
                7,
            ))
            .uv_box(glove, 6.0, Vec2::new(0.5, 0.3))
            .transform(place(
                wrist + Vec3::new(-side * 0.050, -0.048, -0.016),
                Quat::from_rotation_z(side * 0.55),
            ))
            .rigid(hand as u32),
        );
    }
}

/// 头颈：颈套 + 头盔穹顶（旋转体）+ 面罩 + 下颌护板 + 耳罩 + 导轨 / 夜视座。
fn push_head(rig: &Rig, assembly: &mut Assembly) {
    let undersuit = Surface::Undersuit.tile();
    let helmet = Surface::Helmet.tile();
    let visor = Surface::Visor.tile();
    let gunmetal = Surface::Gunmetal.tile();
    let head = rig.p(sk::HEAD);
    // 颈
    let neck_chain = vec![
        rig.bone(sk::CHEST, head),
        rig.bone(sk::HEAD, head + Vec3::new(0.0, 0.18, 0.0)),
    ];
    let neck_profile: Vec<(f32, f32, f32, f32)> = vec![
        (0.00, 0.064, 0.066, 2.2),
        (0.45, 0.058, 0.060, 2.2),
        (1.00, 0.054, 0.056, 2.2),
    ];
    assembly.add(
        Piece::new(limb(
            Vec3::new(0.0, 1.615, 0.008),
            Vec3::new(0.0, 1.832, 0.008),
            &neck_profile,
            12,
        ))
        .uv_cylinder(undersuit, Axis::Y, 1.0, 3.2, Vec2::ZERO)
        .blend_chain(&neck_chain, 0.05),
    );
    // 领圈：盖住颈与躯干的接缝（同样是第一版「细杆脖子」的修法）
    let collar_sections = vec![
        geom::Section::new(
            Vec3::new(0.0, 1.608, 0.006),
            Vec3::X,
            Vec3::Z,
            0.140,
            0.104,
            2.5,
        ),
        geom::Section::new(
            Vec3::new(0.0, 1.658, 0.006),
            Vec3::X,
            Vec3::Z,
            0.118,
            0.094,
            2.3,
        ),
    ];
    assembly.add(
        Piece::new(geom::loft(&collar_sections, 16, false))
            .uv_cylinder(Surface::ClothTorso.tile(), Axis::Y, 1.0, 4.0, Vec2::ZERO)
            .rigid(sk::CHEST as u32),
    );
    // 头盔穹顶：旋转体轮廓（含下沿外翻收口）+ 前后拉长 10%
    let dome_profile: Vec<(f32, f32)> = vec![
        (0.000, 2.072),
        (0.046, 2.068),
        (0.082, 2.048),
        (0.108, 2.016),
        (0.124, 1.974),
        (0.132, 1.930),
        (0.134, 1.888),
        (0.131, 1.858),
        (0.139, 1.842),
        (0.133, 1.826),
        (0.110, 1.820),
        (0.000, 1.818),
    ];
    assembly.add(
        Piece::new(geom::lathe(&dome_profile, 24))
            .uv_cylinder(helmet, Axis::Y, 1.0, 1.8, Vec2::ZERO)
            .transform(
                Mat4::from_translation(Vec3::new(0.0, 0.0, 0.010))
                    * Mat4::from_scale(Vec3::new(1.0, 1.0, 1.06)),
            )
            .rigid(sk::HEAD as u32),
    );
    // 面罩（倾斜的暗色镜面）
    assembly.add(
        Piece::new(geom::superellipsoid(
            Vec3::new(0.098, 0.056, 0.050),
            4.0,
            16,
            9,
        ))
        .uv_box(visor, 3.0, Vec2::ZERO)
        .transform(place(
            Vec3::new(0.0, 1.888, -0.106),
            Quat::from_rotation_x(-0.12),
        ))
        .rigid(sk::HEAD as u32),
    );
    // 下颌护板
    assembly.add(
        Piece::new(geom::superellipsoid(
            Vec3::new(0.104, 0.052, 0.086),
            3.5,
            14,
            9,
        ))
        .uv_box(helmet, 3.2, Vec2::new(0.4, 0.1))
        .transform(Mat4::from_translation(Vec3::new(0.0, 1.838, -0.046)))
        .rigid(sk::HEAD as u32),
    );
    // 耳罩（两侧）
    for side in [-1.0f32, 1.0] {
        assembly.add(
            Piece::new(geom::superellipsoid(
                Vec3::new(0.033, 0.060, 0.058),
                3.5,
                12,
                8,
            ))
            .uv_box(helmet, 4.0, Vec2::new(0.6, 0.5))
            .transform(Mat4::from_translation(Vec3::new(
                side * 0.128,
                1.896,
                0.006,
            )))
            .rigid(sk::HEAD as u32),
        );
    }
    // 顶部导轨
    assembly.add(
        Piece::new(geom::superellipsoid(
            Vec3::new(0.023, 0.017, 0.098),
            5.0,
            10,
            8,
        ))
        .uv_box(gunmetal, 5.0, Vec2::ZERO)
        .transform(Mat4::from_translation(Vec3::new(0.0, 2.050, 0.0)))
        .rigid(sk::HEAD as u32),
    );
    // 夜视仪座 + 后配重
    assembly.add(
        Piece::new(geom::superellipsoid(
            Vec3::new(0.033, 0.035, 0.031),
            4.0,
            12,
            8,
        ))
        .uv_box(gunmetal, 4.0, Vec2::new(0.2, 0.6))
        .transform(Mat4::from_translation(Vec3::new(0.0, 1.988, -0.120)))
        .rigid(sk::HEAD as u32),
    );
    assembly.add(
        Piece::new(geom::superellipsoid(
            Vec3::new(0.054, 0.041, 0.029),
            4.0,
            12,
            8,
        ))
        .uv_box(gunmetal, 4.0, Vec2::new(0.7, 0.2))
        .transform(Mat4::from_translation(Vec3::new(0.0, 1.902, 0.142)))
        .rigid(sk::HEAD as u32),
    );
}

/// 装具：腰带 / 带扣 / 胸挂 / 弹匣包 / 背板 / 肩甲 / 大腿护甲 / 膝垫 / 水壶 / 匕首。
fn push_gear(rig: &Rig, assembly: &mut Assembly) {
    let leather = Surface::Leather.tile();
    let armor = Surface::Armor.tile();
    let pouch = Surface::Pouch.tile();
    let webbing = Surface::Webbing.tile();
    let trim = Surface::Trim.tile();
    let rubber = Surface::Rubber.tile();
    // 腰带（环带，无端盖：端盖埋在躯干里）
    let belt_sections = vec![
        geom::Section::new(
            Vec3::new(0.0, 0.998, 0.0),
            Vec3::X,
            Vec3::Z,
            0.167,
            0.119,
            3.0,
        ),
        geom::Section::new(
            Vec3::new(0.0, 1.078, 0.0),
            Vec3::X,
            Vec3::Z,
            0.164,
            0.116,
            3.0,
        ),
    ];
    assembly.add(
        Piece::new(geom::loft(&belt_sections, 16, false))
            .uv_cylinder(leather, Axis::Y, 1.0, 7.0, Vec2::ZERO)
            .rigid(sk::PELVIS as u32),
    );
    // 带扣
    assembly.add(
        Piece::new(geom::superellipsoid(
            Vec3::new(0.049, 0.037, 0.023),
            4.0,
            12,
            8,
        ))
        .uv_box(trim, 5.0, Vec2::ZERO)
        .transform(Mat4::from_translation(Vec3::new(0.0, 1.038, -0.128)))
        .rigid(sk::PELVIS as u32),
    );
    // 胸挂（plate carrier）：比躯干外扩一圈的环带
    let rig_sections = vec![
        geom::Section::new(
            Vec3::new(0.0, 1.250, 0.0),
            Vec3::X,
            Vec3::Z,
            0.170,
            0.124,
            3.0,
        ),
        geom::Section::new(
            Vec3::new(0.0, 1.360, 0.0),
            Vec3::X,
            Vec3::Z,
            0.186,
            0.130,
            3.0,
        ),
        geom::Section::new(
            Vec3::new(0.0, 1.470, 0.0),
            Vec3::X,
            Vec3::Z,
            0.204,
            0.134,
            3.0,
        ),
        geom::Section::new(
            Vec3::new(0.0, 1.560, 0.0),
            Vec3::X,
            Vec3::Z,
            0.208,
            0.132,
            3.2,
        ),
    ];
    assembly.add(
        Piece::new(geom::loft(&rig_sections, 16, false))
            .uv_box(webbing, 2.4, Vec2::ZERO)
            .rigid(sk::CHEST as u32),
    );
    // 正面弹匣包 ×3 + 侧包 ×2
    for (index, x) in [-0.092f32, 0.0, 0.092].iter().enumerate() {
        let y = if index == 1 { 1.298 } else { 1.318 };
        assembly.add(
            Piece::new(geom::superellipsoid(
                Vec3::new(0.047, 0.057, 0.032),
                5.0,
                12,
                9,
            ))
            .uv_box(pouch, 4.0, Vec2::new(0.1 * index as f32, 0.0))
            .transform(Mat4::from_translation(Vec3::new(*x, y, -0.140)))
            .rigid(sk::CHEST as u32),
        );
    }
    for side in [-1.0f32, 1.0] {
        assembly.add(
            Piece::new(geom::superellipsoid(
                Vec3::new(0.030, 0.052, 0.058),
                5.0,
                12,
                9,
            ))
            .uv_box(pouch, 4.0, Vec2::new(0.4, 0.3))
            .transform(Mat4::from_translation(Vec3::new(
                side * 0.196,
                1.318,
                0.010,
            )))
            .rigid(sk::CHEST as u32),
        );
    }
    // 背板（硬质）
    assembly.add(
        Piece::new(geom::superellipsoid(
            Vec3::new(0.152, 0.160, 0.048),
            5.0,
            16,
            10,
        ))
        .uv_box(armor, 2.6, Vec2::new(0.2, 0.2))
        .transform(Mat4::from_translation(Vec3::new(0.0, 1.420, 0.128)))
        .rigid(sk::CHEST as u32),
    );
    // 胸前徽记（黄铜）
    assembly.add(
        Piece::new(geom::superellipsoid(
            Vec3::new(0.031, 0.011, 0.007),
            5.0,
            10,
            6,
        ))
        .uv_box(trim, 8.0, Vec2::new(0.3, 0.3))
        .transform(Mat4::from_translation(Vec3::new(-0.078, 1.500, -0.146)))
        .rigid(sk::CHEST as u32),
    );
    // 肩甲（上臂刚性：随臂摆动）+ 下层甲片
    for upper in [sk::L_UPPERARM, sk::R_UPPERARM] {
        let shoulder = rig.p(upper);
        let side = if shoulder.x < 0.0 { -1.0f32 } else { 1.0f32 };
        assembly.add(
            Piece::new(geom::superellipsoid(
                Vec3::new(0.098, 0.074, 0.101),
                3.0,
                14,
                9,
            ))
            .uv_box(armor, 3.0, Vec2::ZERO)
            .transform(Mat4::from_translation(
                shoulder + Vec3::new(side * 0.004, 0.014, 0.0),
            ))
            .rigid(upper as u32),
        );
        assembly.add(
            Piece::new(geom::superellipsoid(
                Vec3::new(0.092, 0.040, 0.096),
                4.0,
                14,
                8,
            ))
            .uv_box(armor, 3.0, Vec2::new(0.5, 0.1))
            .transform(Mat4::from_translation(
                shoulder + Vec3::new(side * 0.012, -0.074, 0.0),
            ))
            .rigid(upper as u32),
        );
    }
    // 大腿护甲 + 膝垫
    for (thigh, shin) in [(sk::L_THIGH, sk::L_SHIN), (sk::R_THIGH, sk::R_SHIN)] {
        let (hip, knee) = (rig.p(thigh), rig.p(shin));
        let side = if hip.x < 0.0 { -1.0f32 } else { 1.0f32 };
        assembly.add(
            Piece::new(geom::superellipsoid(
                Vec3::new(0.080, 0.118, 0.102),
                4.0,
                14,
                9,
            ))
            .uv_box(armor, 3.0, Vec2::new(0.2, 0.5))
            .transform(Mat4::from_translation(
                hip + Vec3::new(side * 0.002, -0.148, -0.022),
            ))
            .rigid(thigh as u32),
        );
        assembly.add(
            Piece::new(geom::superellipsoid(
                Vec3::new(0.074, 0.068, 0.072),
                3.5,
                12,
                8,
            ))
            .uv_box(armor, 3.4, Vec2::new(0.6, 0.2))
            .transform(Mat4::from_translation(
                knee + Vec3::new(0.0, -0.016, -0.024),
            ))
            .rigid(shin as u32),
        );
        // 小腿绑带（织带）
        let strap: Vec<(f32, f32, f32, f32)> =
            vec![(0.0, 0.070, 0.076, 3.2), (1.0, 0.068, 0.074, 3.2)];
        assembly.add(
            Piece::new(limb(
                knee.lerp(
                    rig.p(if thigh == sk::L_THIGH {
                        sk::L_FOOT
                    } else {
                        sk::R_FOOT
                    }),
                    0.30,
                ),
                knee.lerp(
                    rig.p(if thigh == sk::L_THIGH {
                        sk::L_FOOT
                    } else {
                        sk::R_FOOT
                    }),
                    0.42,
                ),
                &strap,
                12,
            ))
            .uv_cylinder(webbing, Axis::Y, 1.0, 8.0, Vec2::ZERO)
            .rigid(shin as u32),
        );
    }
    // 水壶（左髋）+ 匕首鞘（右髋）
    assembly.add(
        Piece::new(geom::superellipsoid(
            Vec3::new(0.046, 0.062, 0.032),
            5.0,
            12,
            9,
        ))
        .uv_box(rubber, 4.0, Vec2::new(0.2, 0.7))
        .transform(Mat4::from_translation(Vec3::new(-0.178, 1.052, 0.086)))
        .rigid(sk::PELVIS as u32),
    );
    assembly.add(
        Piece::new(geom::superellipsoid(
            Vec3::new(0.021, 0.098, 0.036),
            5.0,
            12,
            9,
        ))
        .uv_box(leather, 4.0, Vec2::new(0.6, 0.6))
        .transform(Mat4::from_translation(Vec3::new(0.176, 1.010, -0.052)))
        .rigid(sk::PELVIS as u32),
    );
}

/// 刀（蒙皮）：握把 / 护手 / 配重挂 [`SWORD`]，刃挂 [`SWORD_BLADE`]（模型空间）。
fn push_sword(rig: &Rig, assembly: &mut Assembly) {
    let armor = Surface::Armor.tile();
    let trim = Surface::Trim.tile();
    let grip = Surface::Grip.tile();
    let gunmetal = Surface::Gunmetal.tile();
    let blade_tex = Surface::Blade.tile();
    let hilt = Mat4::from_translation(rig.p(sk::SWORD));
    // 配重球
    assembly.add(
        Piece::new(geom::superellipsoid(
            Vec3::new(0.032, 0.030, 0.032),
            3.6,
            12,
            8,
        ))
        .uv_box(trim, 6.0, Vec2::ZERO)
        .transform(hilt * Mat4::from_translation(Vec3::new(0.0, 0.096, 0.0)))
        .rigid(sk::SWORD as u32),
    );
    // 缠柄
    let grip_profile: Vec<(f32, f32, f32, f32)> = vec![
        (0.00, 0.026, 0.028, 2.4),
        (0.50, 0.025, 0.027, 2.4),
        (1.00, 0.024, 0.026, 2.4),
    ];
    assembly.add(
        Piece::new(limb(
            Vec3::new(0.0, 0.082, 0.0),
            Vec3::new(0.0, -0.074, 0.0),
            &grip_profile,
            12,
        ))
        .uv_cylinder(grip, Axis::Y, 1.0, 5.0, Vec2::ZERO)
        .transform(hilt)
        .rigid(sk::SWORD as u32),
    );
    // 护手（横向护格 + 两端球头）
    assembly.add(
        Piece::new(geom::superellipsoid(
            Vec3::new(0.086, 0.019, 0.030),
            5.0,
            14,
            8,
        ))
        .uv_box(armor, 4.0, Vec2::new(0.3, 0.3))
        .transform(hilt * Mat4::from_translation(Vec3::new(0.0, -0.096, 0.0)))
        .rigid(sk::SWORD as u32),
    );
    for side in [-1.0f32, 1.0] {
        assembly.add(
            Piece::new(geom::superellipsoid(
                Vec3::new(0.020, 0.023, 0.024),
                3.4,
                10,
                7,
            ))
            .uv_box(trim, 7.0, Vec2::new(0.4, 0.4))
            .transform(hilt * Mat4::from_translation(Vec3::new(side * 0.082, -0.100, 0.0)))
            .rigid(sk::SWORD as u32),
        );
    }
    // 根部厚段
    assembly.add(
        Piece::new(geom::superellipsoid(
            Vec3::new(0.026, 0.040, 0.015),
            5.0,
            10,
            7,
        ))
        .uv_box(gunmetal, 6.0, Vec2::new(0.5, 0.5))
        .transform(hilt * Mat4::from_translation(Vec3::new(0.0, -0.140, 0.0)))
        .rigid(sk::SWORD as u32),
    );
    // 刃：**宽度沿 X**（第三人称相机在角色背后，宽度朝屏幕才看得见刀面而
    // 不是一条刀脊线——第一版沿 Z 的教训），厚度沿 Z；双刃菱形截面。
    let blade_matrix = Mat4::from_translation(rig.p(sk::SWORD_BLADE));
    let blade_profile: Vec<(f32, f32, f32, f32)> = vec![
        (0.00, 0.0375, 0.0118, 1.0),
        (0.10, 0.0385, 0.0122, 1.0),
        (0.45, 0.0352, 0.0110, 1.0),
        (0.75, 0.0284, 0.0086, 1.0),
        (0.92, 0.0145, 0.0050, 1.0),
        (1.00, 0.0055, 0.0022, 1.0),
    ];
    assembly.add(
        Piece::new(limb(
            Vec3::new(0.0, 0.310, 0.0),
            Vec3::new(0.0, -0.455, 0.0),
            &blade_profile,
            10,
        ))
        .uv_box(blade_tex, 4.0, Vec2::ZERO)
        .transform(blade_matrix)
        .rigid(sk::SWORD_BLADE as u32),
    );
}

/// 步枪（**刚性网格**：几何写在 [`RIFLE`] 骨节点的局部坐标系，由节点变换
/// 驱动——枪不参与蒙皮，于是「隐藏枪械」仍是 [`set_subtree_hidden`] + RIFLE
/// 的一句（敌人是持刀近战，不带枪），蒙皮升级没有破坏这条表现路径）。
fn push_rifle(assembly: &mut Assembly) {
    let gunmetal = Surface::Gunmetal.tile();
    let grip = Surface::Grip.tile();
    let visor = Surface::Visor.tile();
    let rubber = Surface::Rubber.tile();
    let part =
        |mesh: MeshData, tile: crate::geom::Tile, scale: f32, offset: Vec2, at: Vec3, rot: Quat| {
            Piece::new(mesh)
                .uv_box(tile, scale, offset)
                .transform(place(at, rot))
        };
    // 机匣 + 顶部导轨
    assembly.add_rigid(part(
        geom::superellipsoid(Vec3::new(0.023, 0.044, 0.136), 6.0, 14, 9),
        gunmetal,
        4.0,
        Vec2::ZERO,
        Vec3::new(0.0, 0.0, -0.020),
        Quat::IDENTITY,
    ));
    assembly.add_rigid(part(
        geom::superellipsoid(Vec3::new(0.014, 0.010, 0.126), 6.0, 10, 6),
        gunmetal,
        5.0,
        Vec2::new(0.4, 0.4),
        Vec3::new(0.0, 0.052, -0.100),
        Quat::IDENTITY,
    ));
    // 枪管 + 枪口制退器
    let barrel_profile: Vec<(f32, f32, f32, f32)> =
        vec![(0.0, 0.0125, 0.0125, 2.0), (1.0, 0.0106, 0.0106, 2.0)];
    assembly.add_rigid(
        Piece::new(bore(
            Vec3::new(0.0, 0.008, -0.150),
            Vec3::new(0.0, 0.008, -0.462),
            &barrel_profile,
            10,
        ))
        .uv_box(gunmetal, 8.0, Vec2::new(0.2, 0.2)),
    );
    assembly.add_rigid(part(
        geom::superellipsoid(Vec3::new(0.018, 0.018, 0.031), 4.0, 10, 7),
        gunmetal,
        6.0,
        Vec2::new(0.5, 0.2),
        Vec3::new(0.0, 0.008, -0.488),
        Quat::IDENTITY,
    ));
    // 护木（左手 IK 挂点在 z = -0.18 上）
    assembly.add_rigid(part(
        geom::superellipsoid(Vec3::new(0.027, 0.035, 0.112), 5.0, 12, 9),
        gunmetal,
        4.5,
        Vec2::new(0.1, 0.6),
        Vec3::new(0.0, -0.002, -0.256),
        Quat::IDENTITY,
    ));
    // 弹匣（前倾）+ 握把（后倾，右手 IK 挂点在 (0,-0.05,0.12)）
    assembly.add_rigid(part(
        geom::superellipsoid(Vec3::new(0.017, 0.056, 0.039), 5.0, 12, 9),
        gunmetal,
        5.0,
        Vec2::new(0.7, 0.1),
        Vec3::new(0.0, -0.082, -0.048),
        Quat::from_rotation_x(0.22),
    ));
    assembly.add_rigid(part(
        geom::superellipsoid(Vec3::new(0.018, 0.055, 0.032), 5.0, 12, 9),
        grip,
        5.0,
        Vec2::ZERO,
        Vec3::new(0.0, -0.068, 0.108),
        Quat::from_rotation_x(-0.30),
    ));
    // 枪托 + 托底垫
    assembly.add_rigid(part(
        geom::superellipsoid(Vec3::new(0.019, 0.041, 0.102), 5.0, 12, 9),
        gunmetal,
        4.5,
        Vec2::new(0.3, 0.8),
        Vec3::new(0.0, -0.004, 0.192),
        Quat::from_rotation_x(0.06),
    ));
    assembly.add_rigid(part(
        geom::superellipsoid(Vec3::new(0.018, 0.037, 0.015), 6.0, 10, 7),
        rubber,
        6.0,
        Vec2::new(0.5, 0.5),
        Vec3::new(0.0, -0.006, 0.296),
        Quat::IDENTITY,
    ));
    // 光学瞄具：镜筒 + 座 + 前后镜片
    let scope_profile: Vec<(f32, f32, f32, f32)> =
        vec![(0.0, 0.027, 0.027, 2.2), (1.0, 0.027, 0.027, 2.2)];
    assembly.add_rigid(
        Piece::new(bore(
            Vec3::new(0.0, 0.078, -0.028),
            Vec3::new(0.0, 0.078, -0.132),
            &scope_profile,
            12,
        ))
        .uv_box(gunmetal, 5.0, Vec2::new(0.8, 0.3)),
    );
    assembly.add_rigid(part(
        geom::superellipsoid(Vec3::new(0.017, 0.031, 0.029), 5.0, 10, 7),
        gunmetal,
        6.0,
        Vec2::new(0.2, 0.9),
        Vec3::new(0.0, 0.058, -0.080),
        Quat::IDENTITY,
    ));
    for z in [-0.134f32, -0.026] {
        assembly.add_rigid(part(
            geom::superellipsoid(Vec3::new(0.026, 0.026, 0.008), 4.0, 12, 8),
            visor,
            5.0,
            Vec2::ZERO,
            Vec3::new(0.0, 0.078, z),
            Quat::IDENTITY,
        ));
    }
}
