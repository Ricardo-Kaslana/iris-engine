//! 工具：Iris Assault 资产生成器——程序化人形「Assault Soldier」GLB
//! （[`GltfBuilder`] 构造，骨架下标与节点名 = [`iris_assault::skeleton`]
//! 单一事实来源）+ **R45** pack 打包（GLB + 氛围 OGG → `assets/assault.ipak`）。
//!
//! # 骨架装配契约
//!
//! **网格直接挂骨节点**（`node.with_mesh`）——加载器会为每个 primitive
//! 生成独立的子实体（持 MeshHandle / MaterialHandle），骨节点本身保持
//! 下标与 `skeleton.rs` 一致（22 节点）；网格若做成独立子节点会引入 20
//! 个额外节点、把骨架下标全部错位（开发实录）。节点**名**取自骨架表
//! [`sk::NAMES`](iris_assault::skeleton::NAMES)，运行期装配（IK 链 /
//! 挂点 / 配色）按名寻骨（打磨 VI 摩擦⑥）；[`bone`] 内断言「声明顺序 =
//! 骨架表下标」，两侧不同步在生成当场就爆炸，而不是留到下帧画面。
//!
//! # 写实材质（R42，打磨 VI 摩擦③ 闭合）
//!
//! 布料褶皱与刀身划痕的**法线 / ORM 通道直接写进 GLB**
//! （`GltfBuilder::material_textured`，像素 PNG 内嵌 data URI）——R48 的
//! 「加载后克隆 `MaterialDef` + `with_normal/with_orm`」绕行删除，插件层
//! 只剩敌我 `base_color` 配色覆写。
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
use iris_assault::skeleton as sk;
use iris_assault::textures;
use iris_engine::gltf_builder::{AnimChannel, GltfBuilder, GltfMaterialTextures, GltfNodeDesc};
use iris_engine::pack::PackWriter;
// 渲染层类型全部经引擎门面取用（打磨 VI 摩擦② 闭合：本 crate 不再直依
// iris-render）
use iris_engine::prelude::{AnimationInterpolation, AnimationPath, MeshData, Vertex, primitives};

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
    let mut writer = PackWriter::new();
    writer
        .add("models/assault_bot.glb", glb.clone())
        .expect("GLB 条目");
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

/// 以自身中心为原点的缩放立方体（可再整体平移——脚掌等偏心部件）。
fn scaled_cube(sx: f32, sy: f32, sz: f32, offset: [f32; 3]) -> MeshData {
    let cube = primitives::cube();
    let mut data = MeshData::default();
    for v in &cube.vertices {
        data.vertices.push(Vertex {
            position: [
                v.position[0] * sx + offset[0],
                v.position[1] * sy + offset[1],
                v.position[2] * sz + offset[2],
            ],
            normal: v.normal,
            uv: v.uv,
        });
    }
    data.indices = cube.indices.clone();
    data
}

/// 竖直圆柱段（沿 -Y 下垂的肢体段：中心下移 `length`/2）。
fn limb_cylinder(radius: f32, length: f32) -> MeshData {
    let mut cyl = primitives::cylinder(radius, length, 10);
    for v in cyl.vertices.iter_mut() {
        v.position[1] -= length * 0.5;
    }
    cyl
}

/// 声明骨节点并把网格**直接挂上**（装配契约见模块文档）——加载器拆出的
/// primitive 子实体持材质，骨节点保持骨架表下标。
///
/// 节点名取骨架表 [`sk::NAMES`]（打磨 VI 摩擦⑥：生成器与插件层共用一张
/// 名字表，运行期按名寻骨的键与资产侧不可能分叉）。**声明顺序即下标**：
/// 与本表给定的下标不一致时当场 panic（错位过去只能靠 headless 守护测试
/// 事后爆炸，见 §8.26⑥）。
fn bone(
    builder: &mut GltfBuilder,
    parent: usize,
    index: usize,
    at: [f32; 3],
    data: &MeshData,
    material: usize,
) -> usize {
    let name = sk::NAMES[index];
    let mesh = builder.mesh(name, data, material);
    let node = builder.node(GltfNodeDesc::new(name).at(at).with_mesh(mesh));
    builder.add_child(parent, node);
    assert_eq!(
        node, index,
        "节点声明顺序（{node}）与骨架表下标（{index} = {name}）错位——生成器与 `skeleton.rs` 不同步"
    );
    node
}

/// 构造士兵 GLB：22 节点人形（骨架表见 `iris_assault::skeleton`）+
/// 10 剪辑（下半身 3 + 上半身 7）。
fn build_soldier_glb() -> Vec<u8> {
    let mut builder = GltfBuilder::new();

    // ---- 材质（R42 写实通道**随 GLB 自带**：打磨 VI 摩擦③ 闭合，
    // 插件层的「加载后克隆材质挂纹理」绕行删除）----
    // 布料对（身体 / 裤子共用）与划痕金属对（刀身）都是本 crate 的纯函数
    // 生成器产物；builder 按像素内容去重，两材质共用同一张图只嵌一份。
    // 因子与 ORM 像素是**相乘**关系（glTF factor × texel），故粗糙度 /
    // 金属度因子保留 R48 的标量值即得同一种材质观感。
    let (cloth_normal, cloth_orm) = textures::cloth_normal_orm(128, 0x00C1_0711);
    let cloth = || GltfMaterialTextures {
        normal: Some(cloth_normal.clone()),
        orm: Some(cloth_orm.clone()),
        ..Default::default()
    };
    let (scratch_normal, scratch_orm) = textures::scratched_metal_normal_orm(128, 0xD00D_FEED);
    let body_mat = builder.material_textured([0.30, 0.33, 0.38, 1.0], 0.0, 0.8, None, cloth());
    let pants_mat = builder.material_textured([0.20, 0.20, 0.23, 1.0], 0.0, 0.85, None, cloth());
    let helmet_mat = builder.material([0.42, 0.40, 0.37, 1.0], 0.4, 0.5, None);
    let blade_mat = builder.material_textured(
        [0.82, 0.84, 0.88, 1.0],
        1.0,
        0.22,
        None,
        GltfMaterialTextures {
            normal: Some(scratch_normal),
            orm: Some(scratch_orm),
            ..Default::default()
        },
    );
    let guard_mat = builder.material([0.15, 0.13, 0.12, 1.0], 0.3, 0.6, None);
    let rifle_mat = builder.material([0.12, 0.13, 0.14, 1.0], 0.7, 0.4, None);

    // ---- 骨架（节点声明顺序 = skeleton.rs 下标契约，`bone` 内断言；
    // 节点名取骨架表 NAMES，网格即骨节点）----
    let root = builder.node(GltfNodeDesc::new(sk::names::ROOT));
    let pelvis = bone(
        &mut builder,
        root,
        sk::PELVIS,
        [0.0, 0.95, 0.0],
        &scaled_cube(0.36, 0.24, 0.24, [0.0, 0.0, 0.0]),
        pants_mat,
    );
    let torso = bone(
        &mut builder,
        pelvis,
        sk::TORSO,
        [0.0, 0.28, 0.0],
        &scaled_cube(0.40, 0.30, 0.24, [0.0, 0.0, 0.0]),
        body_mat,
    );
    let chest = bone(
        &mut builder,
        torso,
        sk::CHEST,
        [0.0, 0.30, 0.0],
        &scaled_cube(0.44, 0.32, 0.26, [0.0, 0.0, 0.0]),
        body_mat,
    );
    let _head = bone(
        &mut builder,
        chest,
        sk::HEAD,
        [0.0, 0.32, 0.0],
        &scaled_cube(0.22, 0.26, 0.24, [0.0, 0.12, 0.0]),
        helmet_mat,
    );

    // 腿（下垂 -Y；脚掌朝 -Z、踝下偏心）
    let l_thigh = bone(
        &mut builder,
        pelvis,
        sk::L_THIGH,
        [-0.17, -0.05, 0.0],
        &limb_cylinder(0.09, 0.45),
        pants_mat,
    );
    let l_shin = bone(
        &mut builder,
        l_thigh,
        sk::L_SHIN,
        [0.0, -0.45, 0.0],
        &limb_cylinder(0.07, 0.45),
        pants_mat,
    );
    let _l_foot = bone(
        &mut builder,
        l_shin,
        sk::L_FOOT,
        [0.0, -0.45, 0.0],
        &scaled_cube(0.14, 0.10, 0.30, [0.0, -0.05, -0.06]),
        guard_mat,
    );
    let r_thigh = bone(
        &mut builder,
        pelvis,
        sk::R_THIGH,
        [0.17, -0.05, 0.0],
        &limb_cylinder(0.09, 0.45),
        pants_mat,
    );
    let r_shin = bone(
        &mut builder,
        r_thigh,
        sk::R_SHIN,
        [0.0, -0.45, 0.0],
        &limb_cylinder(0.07, 0.45),
        pants_mat,
    );
    let _r_foot = bone(
        &mut builder,
        r_shin,
        sk::R_FOOT,
        [0.0, -0.45, 0.0],
        &scaled_cube(0.14, 0.10, 0.30, [0.0, -0.05, -0.06]),
        guard_mat,
    );

    // 双臂（下垂 -Y；肩挂胸两侧）
    let l_upperarm = bone(
        &mut builder,
        chest,
        sk::L_UPPERARM,
        [-0.34, 0.22, 0.0],
        &limb_cylinder(0.075, 0.32),
        body_mat,
    );
    let l_forearm = bone(
        &mut builder,
        l_upperarm,
        sk::L_FOREARM,
        [0.0, -0.32, 0.0],
        &limb_cylinder(0.06, 0.30),
        body_mat,
    );
    let _l_hand = bone(
        &mut builder,
        l_forearm,
        sk::L_HAND,
        [0.0, -0.30, 0.0],
        &scaled_cube(0.09, 0.20, 0.09, [0.0, -0.08, 0.0]),
        guard_mat,
    );
    let r_upperarm = bone(
        &mut builder,
        chest,
        sk::R_UPPERARM,
        [0.34, 0.22, 0.0],
        &limb_cylinder(0.075, 0.32),
        body_mat,
    );
    let r_forearm = bone(
        &mut builder,
        r_upperarm,
        sk::R_FOREARM,
        [0.0, -0.32, 0.0],
        &limb_cylinder(0.06, 0.30),
        body_mat,
    );
    let r_hand = bone(
        &mut builder,
        r_forearm,
        sk::R_HAND,
        [0.0, -0.30, 0.0],
        &scaled_cube(0.09, 0.20, 0.09, [0.0, -0.08, 0.0]),
        guard_mat,
    );

    // 刀：格挡段挂手，刃段挂格挡（跟随手骨——非 IK 对象）
    let sword = bone(
        &mut builder,
        r_hand,
        sk::SWORD,
        [0.0, -0.02, 0.0],
        &scaled_cube(0.17, 0.05, 0.06, [0.0, 0.0, 0.0]),
        guard_mat,
    );
    let _sword_blade = bone(
        &mut builder,
        sword,
        sk::SWORD_BLADE,
        [0.0, -0.42, 0.0],
        &scaled_cube(0.045, 0.78, 0.11, [0.0, 0.0, 0.0]),
        blade_mat,
    );

    // 步枪：枪身挂胸（手 IK 追握把 / 护木——挂点不在手骨链子树内）
    let rifle = bone(
        &mut builder,
        chest,
        sk::RIFLE,
        [0.15, 0.10, -0.28],
        &scaled_cube(0.06, 0.11, 0.58, [0.0, 0.0, -0.05]),
        rifle_mat,
    );
    let _rifle_grip = bone(
        &mut builder,
        rifle,
        sk::RIFLE_GRIP,
        [0.0, -0.10, 0.12],
        &scaled_cube(0.05, 0.15, 0.07, [0.0, -0.06, 0.0]),
        guard_mat,
    );
    // 护木：纯标记节点（无网格）
    let _foregrip = {
        let node = builder.node(GltfNodeDesc::new(sk::names::FOREGRIP).at([0.0, -0.03, -0.18]));
        builder.add_child(rifle, node);
        assert_eq!(node, sk::FOREGRIP, "护木声明顺序与骨架表错位");
        node
    };

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
