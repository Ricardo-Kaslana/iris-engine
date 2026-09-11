//! 工具：Iris Assault 资产生成器——程序化人形「Assault Soldier」GLB
//! （[`GltfBuilder`] 构造，骨架下标与节点名 = [`iris_assault::skeleton`]
//! 单一事实来源）+ **R45** pack 打包（GLB + 氛围 OGG → `assets/assault.ipak`）。
//!
//! # 骨架装配契约（视觉升级轮：单张蒙皮网格）
//!
//! **22 个骨节点 + 1 个网格节点**：网格挂 `root` 节点（`with_mesh` +
//! `attach_skin`），顶点权重指向关节表——节点总数仍是 22，动画通道下标 /
//! `AnimationMask` / 按名寻骨三条装配路径全部不变，而形变从「逐骨刚性挂
//! 块」升级为 **GPU 线性混合蒙皮**（关节处平滑过渡、装备随骨刚性跟随）。
//!
//! 节点位置取自 [`sk::BONE_LOCAL`](iris_assault::skeleton::BONE_LOCAL)、
//! 名取自 [`sk::NAMES`](iris_assault::skeleton::NAMES)——骨架表是唯一
//! 事实来源，生成器只做「按表声明 + 断言下标」，两侧不同步在生成当场就
//! 爆炸（断言消息含节点名），而不是留到下帧画面。
//!
//! # 材质（单材质 + 程序化图集）
//!
//! 整具角色（身体 / 护甲 / 皮革 / 钢刃 / 面罩玻璃 / 枪械）共用**一张**
//! 1024² 图集（[`soldier_tex`](iris_assault::soldier_tex)）：16 个 256²
//! 分区逐纹素表达金属度 / 粗糙度，法线与 ORM 通道随 GLB 内嵌
//! （`GltfBuilder::material_textured`）。单网格单材质 = **单次 draw**。
//!
//! # 音频条目（R44 / 打磨 VI 摩擦④）
//!
//! 引擎只有 Vorbis **解码**（运行时职责），编码走外部资产管线：
//! `ffmpeg -i in.wav -c:a libvorbis -q 4 in.ogg`（标准命令见 README /
//! ARCHITECTURE §4.14）。本作氛围暂用引擎测试 fixture 的 2 s 正弦 OGG。
//!
//! # 剪辑集（双播放器上下身分离的结构性前提）
//!
//! - **下半身**（通道只写 `LOWER_BODY` 节点）：`idle` / `walk` / `run`
//!   ——插件层挂 `BlendSpace`（参数 = 速度 0..1）；
//! - **上半身**（通道只写 `UPPER_BODY` 节点）：`guard`（待机持刀）/
//!   `sword1..3`（三连段，非循环，时长 = 逻辑前摇 + 判定窗 + 后摇）/
//!   `aim`（ADS） / `reload`（非循环） / `flinch`（受击附加层，首尾
//!   关键帧 = 中性姿势——附加增量在 t=0 恰为零）。
//!
//! 两层写入集不相交 ⇒ 跑动中挥砍 / 瞄准不打断腿（组合姿势由结构保证）。
//!
//! ```sh
//! cargo run -p iris-assault --example gen_assault_assets --release   # 在仓库根执行
//! ```

use glam::{EulerRot, Quat};
use iris_assault::scenery_tex::Surface;
use iris_assault::skeleton as sk;
use iris_assault::{scenery, scenery_tex, soldier, soldier_tex};
use iris_engine::gltf_builder::{AnimChannel, GltfBuilder, GltfMaterialTextures, GltfNodeDesc};
use iris_engine::pack::PackWriter;
// 渲染层类型全部经引擎门面取用（打磨 VI 摩擦② 闭合：本 crate 不再直依
// iris-render）
use iris_engine::prelude::{AnimationInterpolation, AnimationPath};

/// 构造庭院 GLB：程序化建筑几何（铺地 / 墙 / 壁龛 / 石柱 / 喷泉 / 火盆 /
/// 旗帜 / 木门）按材质分组，每材质一张独立贴图（可平铺）。
fn build_courtyard_glb() -> Vec<u8> {
    let mut builder = GltfBuilder::new();
    // ---- 材质：每材质一组（贴图材质走 material_textured，纯参数材质走 material）----
    let mut material_index = Vec::new();
    for surface in Surface::ALL {
        let params = scenery_tex::surface_params(surface);
        let index = if surface.size() == 0 {
            builder.material(
                params.base_color,
                params.metallic,
                params.roughness,
                params.emissive,
            )
        } else {
            let tex = scenery_tex::surface_texture(surface);
            builder.material_textured(
                params.base_color,
                params.metallic,
                params.roughness,
                params.emissive,
                GltfMaterialTextures {
                    base_color: Some(tex.base),
                    normal: tex.normal,
                    orm: tex.orm,
                },
            )
        };
        material_index.push((surface, index));
    }
    // ---- 网格 + 节点（每个材质一个顶层节点）----
    let courtyard = scenery::build_courtyard();
    println!(
        "庭院几何：{} 三角形 / {} 材质分组",
        scenery::triangle_count(&courtyard),
        courtyard.groups.len()
    );
    for (order, (surface, mesh)) in courtyard.groups.iter().enumerate() {
        let material = material_index
            .iter()
            .find(|(s, _)| s == surface)
            .unwrap_or_else(|| panic!("材质 {surface:?} 未登记"))
            .1;
        let name = format!("courtyard/{surface:?}");
        let mesh_index = builder.mesh(&name, mesh, material);
        let node = builder.node(GltfNodeDesc::new(name).with_mesh(mesh_index));
        assert_eq!(node, order, "节点声明顺序应与材质分组序一致");
    }
    builder.build_glb()
}

fn main() {
    let glb = build_soldier_glb();
    std::fs::create_dir_all("assets").expect("创建 assets 目录");
    std::fs::write("assets/assault_bot.glb", &glb).expect("写入 assets/assault_bot.glb");
    println!(
        "已生成 assets/assault_bot.glb（{} 字节，{} 节点 / 10 剪辑）",
        glb.len(),
        sk::NODE_COUNT
    );

    // ---- R45：pack 打包（GLB + 氛围 OGG → 单文件归档）----
    let ambience = find_ambience_ogg().expect("未找到 OGG 氛围源（引擎音频 fixture）");
    let ambience_bytes = std::fs::read(&ambience).expect("读 OGG 氛围源");
    // ---- 庭院视觉 GLB（程序化建筑）----
    let courtyard_glb = build_courtyard_glb();
    std::fs::write("assets/assault_courtyard.glb", &courtyard_glb).expect("写入庭院 GLB");
    println!(
        "已生成 assets/assault_courtyard.glb（{} 字节）",
        courtyard_glb.len()
    );

    let mut writer = PackWriter::new();
    writer
        .add("models/assault_bot.glb", glb.clone())
        .expect("GLB 条目");
    writer
        .add("models/assault_courtyard.glb", courtyard_glb.clone())
        .expect("庭院 GLB 条目");
    writer
        .add("audio/ambience.ogg", ambience_bytes.clone())
        .expect("OGG 条目");
    let packed = writer
        .write_file("assets/assault.ipak")
        .expect("写 assets/assault.ipak");
    println!(
        "已生成 assets/assault.ipak（{packed} 字节：GLB {} + OGG {}）",
        glb.len(),
        ambience_bytes.len()
    );
}

/// 氛围 OGG 源：引擎只带 Vorbis 解码（运行时职责），编码走外部管线
/// （打磨 VI 摩擦④ 的明文结论）——自有音效用
/// `ffmpeg -i in.wav -c:a libvorbis -q 4 in.ogg` 转好再进 pack。
/// 此处暂复用引擎音频测试 fixture（2 s 正弦——循环播放即「庭院嗡鸣」
/// 的占位氛围），避免把二进制音频资产塞进仓库。
fn find_ambience_ogg() -> Option<std::path::PathBuf> {
    let candidates = [
        std::path::PathBuf::from("crates/iris-audio/tests/fixtures/tone_2s.ogg"),
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/iris-audio/tests/fixtures/tone_2s.ogg"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

/// 欧拉角（XYZ 弧度）→ glTF 四元数 (x, y, z, w)。
fn quat(rx: f32, ry: f32, rz: f32) -> [f32; 4] {
    let q = Quat::from_euler(EulerRot::XYZ, rx, ry, rz);
    [q.x, q.y, q.z, q.w]
}

/// 构造士兵 GLB：22 骨节点 + 1 张蒙皮网格 + 单材质图集，10 剪辑（下半身 3 + 上半身 7）。
///
/// 网格几何与蒙皮权重来自 `iris_assault::soldier`（程序化人形 + 装备 + 武器），材质图集来自
/// `iris_assault::soldier_tex`；本函数只负责按骨架表声明节点、装配蒙皮与剪辑数据。
fn build_soldier_glb() -> Vec<u8> {
    let mut builder = GltfBuilder::new();

    // ---- 材质：单材质 + 士兵图集（base / normal / ORM 三通道）----
    // 整具角色（身体 + 装备 + 刀 + 枪）共用一张 1024² 图集：16 个分区逐
    // 纹素表达金属度 / 粗糙度（材质多样性），单网格单材质 = 单次 draw。
    let atlas = soldier_tex::soldier_atlas(soldier_tex::ATLAS_SEED);
    let material = builder.material_textured(
        [1.0, 1.0, 1.0, 1.0],
        1.0,
        1.0,
        None,
        GltfMaterialTextures {
            base_color: Some(atlas.base),
            normal: Some(atlas.normal),
            orm: Some(atlas.orm),
        },
    );

    // ---- 蒙皮网格：一张（22 骨 + 1 个网格节点 = 骨架下标契约不变）----
    let soldier = soldier::build_soldier_mesh();
    println!(
        "士兵网格：{} 顶点 / {} 三角形 / {} 关节",
        soldier.mesh.vertices.len(),
        soldier.mesh.indices.len() / 3,
        soldier.inverse_bind.len()
    );
    let mesh = builder.mesh("soldier", &soldier.mesh, material);

    // ---- 骨架节点：位置取 skeleton::BONE_LOCAL（单一事实来源）----
    // 网格挂 ROOT 节点（**不是**逐骨挂网格）：节点总数仍是 22，动画通道
    // 下标、动画遮罩、按名寻骨三条装配路径全部不变；形变交给 GPU 蒙皮。
    // 步枪：**独立刚性网格**挂 RIFLE 节点（节点局部坐标系，无蒙皮）——
    // 「敌人不带枪」仍是 set_subtree_hidden(RIFLE) 一句，蒙皮升级没破坏它
    let rifle_mesh = builder.mesh("rifle", &soldier::build_rifle_mesh(), material);
    let mut joints = Vec::with_capacity(sk::NODE_COUNT);
    for index in 0..sk::NODE_COUNT {
        let mut desc = GltfNodeDesc::new(sk::NAMES[index]).at(sk::BONE_LOCAL[index]);
        if index == sk::ROOT {
            desc = desc.with_mesh(mesh);
        } else if index == sk::RIFLE {
            desc = desc.with_mesh(rifle_mesh);
        }
        let node = builder.node(desc);
        assert_eq!(
            node,
            index,
            "节点声明顺序（{node}）与骨架表下标（{index} = {}）错位——生成器与 skeleton.rs 不同步",
            sk::NAMES[index]
        );
        if index != sk::ROOT {
            builder.add_child(sk::BONE_PARENT[index], node);
        }
        joints.push(node);
    }
    // 蒙皮：关节表 = 22 骨，逆绑定矩阵 = 绑定姿势逐项求逆（skeleton 表推出）
    let skin = builder.skin("soldier_skin", &joints, &soldier.inverse_bind);
    builder.attach_skin(sk::ROOT, skin);

    // ---- 下半身剪辑（通道只写 LOWER_BODY 节点）----

    // idle：骨盆低幅呼吸浮动（2 s 循环）。**不含恒值腿旋转通道**——那是
    // R48 为绕开「足部 IK 回写四元数不归一化 → 范数逐帧平方复合爆炸」补的
    // 资产侧兜底；引擎侧已在 `update_foot_ik` / `update_hand_ik` 写回前
    // 归一化（600 帧回归锁：`iris-render` foot_ik / hand_ik 的
    // `*_quaternion_norm_stable_without_clip_overwrite`），摩擦① 闭合。
    // 附带收益：idle 通道数 5 → 1，剪辑体积与每帧采样量都降。
    builder.animation(
        "idle",
        &[AnimChannel {
            node: sk::PELVIS,
            path: AnimationPath::Translation,
            interpolation: AnimationInterpolation::Linear,
            times: vec![0.0, 1.0, 2.0],
            values: vec![0.0, 0.95, 0.0, 0.0, 0.938, 0.0, 0.0, 0.95, 0.0],
        }],
    );

    // walk：腿部交替摆动（rotX 正 = 前摆）+ 膝弯 + 骨盆两步一浮（0.8 s 循环）
    builder.animation(
        "walk",
        &[
            AnimChannel {
                node: sk::PELVIS,
                path: AnimationPath::Translation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.2, 0.4, 0.6, 0.8],
                values: [0.95, 0.962, 0.95, 0.962, 0.95]
                    .iter()
                    .flat_map(|&y| [0.0, y, 0.0])
                    .collect(),
            },
            AnimChannel {
                node: sk::L_THIGH,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.4, 0.8],
                values: [0.45f32, -0.45, 0.45]
                    .iter()
                    .flat_map(|&deg| quat(deg, 0.0, 0.0))
                    .collect(),
            },
            AnimChannel {
                node: sk::R_THIGH,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.4, 0.8],
                values: [-0.45f32, 0.45, -0.45]
                    .iter()
                    .flat_map(|&deg| quat(deg, 0.0, 0.0))
                    .collect(),
            },
            AnimChannel {
                node: sk::L_SHIN,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.4, 0.8],
                values: [0.0f32, -0.55, 0.0]
                    .iter()
                    .flat_map(|&deg| quat(deg, 0.0, 0.0))
                    .collect(),
            },
            AnimChannel {
                node: sk::R_SHIN,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.4, 0.8],
                values: [-0.55f32, 0.0, -0.55]
                    .iter()
                    .flat_map(|&deg| quat(deg, 0.0, 0.0))
                    .collect(),
            },
        ],
    );

    // run：更大幅度的步态 + 更快的骨盆浮动（0.55 s 循环）
    builder.animation(
        "run",
        &[
            AnimChannel {
                node: sk::PELVIS,
                path: AnimationPath::Translation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.1375, 0.275, 0.4125, 0.55],
                values: [0.95, 0.97, 0.95, 0.97, 0.95]
                    .iter()
                    .flat_map(|&y| [0.0, y, 0.0])
                    .collect(),
            },
            AnimChannel {
                node: sk::L_THIGH,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.275, 0.55],
                values: [0.85f32, -0.85, 0.85]
                    .iter()
                    .flat_map(|&deg| quat(deg, 0.0, 0.0))
                    .collect(),
            },
            AnimChannel {
                node: sk::R_THIGH,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.275, 0.55],
                values: [-0.85f32, 0.85, -0.85]
                    .iter()
                    .flat_map(|&deg| quat(deg, 0.0, 0.0))
                    .collect(),
            },
            AnimChannel {
                node: sk::L_SHIN,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.275, 0.55],
                values: [0.0f32, -1.0, 0.0]
                    .iter()
                    .flat_map(|&deg| quat(deg, 0.0, 0.0))
                    .collect(),
            },
            AnimChannel {
                node: sk::R_SHIN,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.275, 0.55],
                values: [-1.0f32, 0.0, -1.0]
                    .iter()
                    .flat_map(|&deg| quat(deg, 0.0, 0.0))
                    .collect(),
            },
        ],
    );

    // ---- 上半身剪辑（通道只写 UPPER_BODY 节点）----

    // 三连段共用的首尾姿势（= guard 中位姿势，与过渡混合平滑衔接；
    // 横扫窗与逻辑判定窗 [windup, windup+sweep] 对齐）
    let guard_chest = quat(0.0, 0.05, 0.0);
    let guard_r_upper = quat(0.95, 0.0, 0.0);
    let guard_r_fore = quat(0.85, 0.0, 0.0);
    let guard_l_upper = quat(0.45, 0.0, 0.0);
    let guard_l_fore = quat(0.7, 0.0, 0.0);

    // guard：持刀待机——右臂前举持刀、左臂平衡、胸部呼吸微摆（1.6 s 循环）
    builder.animation(
        "guard",
        &[
            AnimChannel {
                node: sk::CHEST,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.8, 1.6],
                values: [0.05f32, -0.05, 0.05]
                    .iter()
                    .flat_map(|&deg| quat(0.0, deg, 0.0))
                    .collect(),
            },
            AnimChannel {
                node: sk::R_UPPERARM,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.8, 1.6],
                values: [0.95f32, 1.0, 0.95]
                    .iter()
                    .flat_map(|&deg| quat(deg, 0.0, 0.0))
                    .collect(),
            },
            AnimChannel {
                node: sk::R_FOREARM,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.8, 1.6],
                values: [0.85f32, 0.9, 0.85]
                    .iter()
                    .flat_map(|&deg| quat(deg, 0.0, 0.0))
                    .collect(),
            },
            AnimChannel {
                node: sk::L_UPPERARM,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.8, 1.6],
                values: [0.45f32, 0.5, 0.45]
                    .iter()
                    .flat_map(|&deg| quat(deg, 0.0, 0.0))
                    .collect(),
            },
            AnimChannel {
                node: sk::L_FOREARM,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.8, 1.6],
                values: [0.7f32, 0.75, 0.7]
                    .iter()
                    .flat_map(|&deg| quat(deg, 0.0, 0.0))
                    .collect(),
            },
        ],
    );

    // sword1（0.54 s = 0.18 前摇 + 0.14 横扫 + 0.22 后摇）：右→左横扫
    builder.animation(
        "sword1",
        &[
            AnimChannel {
                node: sk::CHEST,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.18, 0.25, 0.32, 0.54],
                values: [
                    guard_chest,
                    quat(0.0, 0.55, 0.0),
                    quat(0.0, 0.0, 0.0),
                    quat(0.0, -0.45, 0.0),
                    guard_chest,
                ]
                .concat(),
            },
            AnimChannel {
                node: sk::R_UPPERARM,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.18, 0.25, 0.32, 0.54],
                values: [
                    guard_r_upper,
                    quat(0.55, 0.0, -1.1),
                    quat(0.75, 0.0, 0.0),
                    quat(0.7, 0.0, 0.95),
                    guard_r_upper,
                ]
                .concat(),
            },
            AnimChannel {
                node: sk::R_FOREARM,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.18, 0.25, 0.32, 0.54],
                values: [
                    guard_r_fore,
                    quat(0.6, 0.0, 0.0),
                    quat(0.7, 0.0, 0.0),
                    quat(0.7, 0.0, 0.0),
                    guard_r_fore,
                ]
                .concat(),
            },
        ],
    );

    // sword2（0.54 s）：左→右回扫（镜像）
    builder.animation(
        "sword2",
        &[
            AnimChannel {
                node: sk::CHEST,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.16, 0.23, 0.30, 0.54],
                values: [
                    guard_chest,
                    quat(0.0, -0.5, 0.0),
                    quat(0.0, 0.0, 0.0),
                    quat(0.0, 0.5, 0.0),
                    guard_chest,
                ]
                .concat(),
            },
            AnimChannel {
                node: sk::R_UPPERARM,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.16, 0.23, 0.30, 0.54],
                values: [
                    guard_r_upper,
                    quat(0.6, 0.0, 0.95),
                    quat(0.8, 0.0, 0.0),
                    quat(0.6, 0.0, -1.05),
                    guard_r_upper,
                ]
                .concat(),
            },
            AnimChannel {
                node: sk::R_FOREARM,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.16, 0.23, 0.30, 0.54],
                values: [
                    guard_r_fore,
                    quat(0.65, 0.0, 0.0),
                    quat(0.75, 0.0, 0.0),
                    quat(0.65, 0.0, 0.0),
                    guard_r_fore,
                ]
                .concat(),
            },
        ],
    );

    // sword3（0.80 s = 0.22 前摇 + 0.16 横扫 + 0.42 后摇）：过顶劈砍
    builder.animation(
        "sword3",
        &[
            AnimChannel {
                node: sk::CHEST,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.22, 0.30, 0.38, 0.80],
                values: [
                    guard_chest,
                    quat(-0.15, 0.0, 0.0),
                    quat(0.28, 0.0, 0.0),
                    quat(0.25, 0.0, 0.0),
                    guard_chest,
                ]
                .concat(),
            },
            AnimChannel {
                node: sk::R_UPPERARM,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.22, 0.30, 0.38, 0.80],
                values: [
                    guard_r_upper,
                    quat(2.5, 0.0, 0.0),
                    quat(0.35, 0.0, 0.0),
                    quat(0.4, 0.0, 0.0),
                    guard_r_upper,
                ]
                .concat(),
            },
            AnimChannel {
                node: sk::R_FOREARM,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.22, 0.30, 0.38, 0.80],
                values: [
                    guard_r_fore,
                    quat(0.5, 0.0, 0.0),
                    quat(0.3, 0.0, 0.0),
                    quat(0.4, 0.0, 0.0),
                    guard_r_fore,
                ]
                .concat(),
            },
            AnimChannel {
                node: sk::L_UPPERARM,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.22, 0.38, 0.80],
                values: [
                    guard_l_upper,
                    quat(1.2, 0.0, 0.0),
                    quat(0.9, 0.0, 0.0),
                    guard_l_upper,
                ]
                .concat(),
            },
        ],
    );

    // aim：ADS 持枪——双臂前伸（手位最终由手 IK 吸附到枪身挂点）
    builder.animation(
        "aim",
        &[
            AnimChannel {
                node: sk::CHEST,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.5, 1.0],
                values: [
                    quat(-0.04, 0.0, 0.0),
                    quat(-0.06, 0.0, 0.0),
                    quat(-0.04, 0.0, 0.0),
                ]
                .concat(),
            },
            AnimChannel {
                node: sk::R_UPPERARM,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.5, 1.0],
                values: [
                    quat(1.45, 0.0, -0.15),
                    quat(1.47, 0.0, -0.15),
                    quat(1.45, 0.0, -0.15),
                ]
                .concat(),
            },
            AnimChannel {
                node: sk::R_FOREARM,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.5, 1.0],
                values: [
                    quat(-0.15, 0.0, 0.0),
                    quat(-0.15, 0.0, 0.0),
                    quat(-0.15, 0.0, 0.0),
                ]
                .concat(),
            },
            AnimChannel {
                node: sk::L_UPPERARM,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.5, 1.0],
                values: [
                    quat(1.35, 0.0, 0.25),
                    quat(1.35, 0.0, 0.25),
                    quat(1.35, 0.0, 0.25),
                ]
                .concat(),
            },
            AnimChannel {
                node: sk::L_FOREARM,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.5, 1.0],
                values: [
                    quat(0.55, 0.0, 0.0),
                    quat(0.55, 0.0, 0.0),
                    quat(0.55, 0.0, 0.0),
                ]
                .concat(),
            },
        ],
    );

    // reload：换弹（非循环 1.6 s）——左手下沉到弹匣位再回位
    builder.animation(
        "reload",
        &[
            AnimChannel {
                node: sk::L_UPPERARM,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.4, 1.1, 1.6],
                values: [
                    guard_l_upper,
                    quat(0.35, 0.0, 0.0),
                    quat(0.35, 0.0, 0.0),
                    guard_l_upper,
                ]
                .concat(),
            },
            AnimChannel {
                node: sk::L_FOREARM,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.4, 1.1, 1.6],
                values: [
                    guard_l_fore,
                    quat(1.5, 0.0, 0.0),
                    quat(1.5, 0.0, 0.0),
                    guard_l_fore,
                ]
                .concat(),
            },
            AnimChannel {
                node: sk::CHEST,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.4, 1.1, 1.6],
                values: [
                    guard_chest,
                    quat(0.0, 0.18, 0.0),
                    quat(0.0, 0.18, 0.0),
                    guard_chest,
                ]
                .concat(),
            },
        ],
    );

    // flinch：受击附加层（0.3 s）——首尾关键帧 = 中性（identity），
    // 附加增量在 t=0 恰为零（叠加语义，见 AdditiveLayer 文档）
    builder.animation(
        "flinch",
        &[
            AnimChannel {
                node: sk::CHEST,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.08, 0.3],
                values: [
                    quat(0.0, 0.0, 0.0),
                    quat(0.3, 0.0, 0.0),
                    quat(0.0, 0.0, 0.0),
                ]
                .concat(),
            },
            AnimChannel {
                node: sk::HEAD,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.08, 0.3],
                values: [
                    quat(0.0, 0.0, 0.0),
                    quat(-0.25, 0.0, 0.0),
                    quat(0.0, 0.0, 0.0),
                ]
                .concat(),
            },
            AnimChannel {
                node: sk::R_UPPERARM,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.08, 0.3],
                values: [
                    quat(0.0, 0.0, 0.0),
                    quat(0.0, 0.0, -0.2),
                    quat(0.0, 0.0, 0.0),
                ]
                .concat(),
            },
            AnimChannel {
                node: sk::L_UPPERARM,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.08, 0.3],
                values: [
                    quat(0.0, 0.0, 0.0),
                    quat(0.0, 0.0, 0.2),
                    quat(0.0, 0.0, 0.0),
                ]
                .concat(),
            },
        ],
    );

    builder.build_glb()
}
