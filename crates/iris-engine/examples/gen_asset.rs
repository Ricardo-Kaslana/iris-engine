//! 工具：程序化生成示例 glTF 资产（打磨 IV 摩擦②闭合后全部经
//! [`iris_engine::gltf_builder::GltfBuilder`] 构造——bufferView 偏移 /
//! accessor 下标 / sampler 编号的记账归 builder，本文件只声明几何 /
//! 层级 / 关键帧）。
//!
//! - `assets/cube.gltf` + `cube0.bin`：外部 buffer 的标准 glTF（`model` 示例加载）
//! - `assets/anim_cube.glb`：GLB 容器，带旋转 + 浮动动画（`anim_snapshot` 示例加载）
//! - `assets/skin_arm.glb`：GLB 容器，两段式蒙皮手臂 + 肘关节挥舞动画（`skin_snapshot` 示例加载）
//! - `assets/enemy_worm.glb`：GLB 容器，三段式蒙皮蠕虫（iris-arena 敌人）
//!   + 关节反相扭动循环动画
//! - `assets/ascent_bot.glb`：GLB 容器，小机器人「Ascent Bot」（iris-ascent
//!   玩家角色）+ idle / walk 双剪辑（**各只声明真正驱动的通道**——打磨 IV
//!   摩擦①闭合后 crossfade 会把「仅源有」通道淡回基姿势，无须恒值通道
//!   对齐）
//! - `assets/sunset.hdr`：equirectangular 日落全景 Radiance 文件（`hdr_snapshot` 示例加载）
//!
//! ```sh
//! cargo run --example gen_asset --release   # 在仓库根目录执行
//! ```

use glam::Quat;
use iris_engine::gltf_builder::{AnimChannel, GltfBuilder, GltfNodeDesc};
use iris_render::animation::{AnimationInterpolation, AnimationPath};
use iris_render::mesh::primitives;
use iris_render::mesh::{MeshData, SkinVertex, Vertex};

fn main() {
    let cube = primitives::cube();
    write_gltf_cube("assets/cube.gltf", "cube0.bin", &cube);
    write_animated_cube_glb("assets/anim_cube.glb", &cube);
    write_skinned_arm_glb("assets/skin_arm.glb");
    write_enemy_worm_glb("assets/enemy_worm.glb");
    write_ascent_bot_glb("assets/ascent_bot.glb");
    write_sunset_hdr("assets/sunset.hdr");
    println!(
        "已生成 assets/cube.gltf、assets/cube0.bin、assets/anim_cube.glb、assets/skin_arm.glb、assets/enemy_worm.glb、assets/ascent_bot.glb 与 assets/sunset.hdr"
    );
}

/// 把单位立方体数据写成 glTF（JSON + 外部二进制 buffer）。
fn write_gltf_cube(gltf_path: &str, bin_name: &str, cube: &MeshData) {
    let mut builder = GltfBuilder::new();
    let material = builder.material([0.92, 0.48, 0.18, 1.0], 1.0, 0.35, None);
    let mesh = builder.mesh("Cube", cube, material);
    // 朝向屏幕斜 45°（yaw 22.5° 四元数 = (0, sin, 0, cos)）
    let (s, c) = (45.0f32 * 0.5).to_radians().sin_cos();
    builder.node(
        GltfNodeDesc::new("CubeRoot")
            .with_mesh(mesh)
            .rotated([0.0, s, 0.0, c]),
    );
    let (json, bin) = builder.build_json_and_bin(Some(bin_name));
    write_pair(gltf_path, bin_name, &json, &bin);
}

/// 把立方体写成 GLB 容器，附带「自转 4s + 上下浮动 4s」循环动画。
fn write_animated_cube_glb(path: &str, cube: &MeshData) {
    let mut builder = GltfBuilder::new();
    let material = builder.material([0.25, 0.55, 0.95, 1.0], 0.1, 0.3, None);
    let mesh = builder.mesh("Cube", cube, material);
    builder.node(GltfNodeDesc::new("SpinCube").with_mesh(mesh));
    let spin = |deg: f32| {
        let (s, c) = (deg * 0.5).to_radians().sin_cos();
        [0.0f32, s, 0.0, c]
    };
    builder.animation(
        "spin_bob",
        &[
            AnimChannel {
                node: 0,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 1.0, 2.0, 3.0, 4.0],
                values: [0.0f32, 90.0, 180.0, 270.0, 360.0]
                    .iter()
                    .flat_map(|&deg| spin(deg))
                    .collect(),
            },
            AnimChannel {
                node: 0,
                path: AnimationPath::Translation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 2.0, 4.0],
                values: vec![0.0, 0.0, 0.0, 0.0, 0.6, 0.0, 0.0, 0.0, 0.0],
            },
        ],
    );
    std::fs::write(path, builder.build_glb()).expect("写入 GLB");
}

/// 两段式蒙皮手臂 + 地面：下段 y∈[0,1] 绑髋关节，上段 y∈[1,2] 绑肘关节，
/// 肘关节 rotZ 0°→90°→0° / 3s 循环挥舞。`skin_snapshot` 示例加载。
///
/// 节点树：0=ArmRoot(mesh+skin) 1=hip 2=elbow(T(0,1,0)) 3=Ground；
/// skin.joints=[1,2]，ibm=[I, T(0,-1,0)]。
fn write_skinned_arm_glb(path: &str) {
    // 臂网格：两个细长立方体段
    let mut arm = MeshData {
        skin: Some(Vec::new()),
        ..MeshData::default()
    };
    let add_segment = |data: &mut MeshData, center: [f32; 3], joint: u32| {
        let base = data.vertices.len() as u32;
        let cube = primitives::cube();
        for v in &cube.vertices {
            data.vertices.push(Vertex {
                position: [
                    v.position[0] * 0.3 + center[0],
                    v.position[1] + center[1],
                    v.position[2] * 0.3 + center[2],
                ],
                normal: v.normal,
                uv: v.uv,
            });
        }
        data.indices.extend(cube.indices.iter().map(|i| i + base));
        data.skin.as_mut().unwrap().extend(std::iter::repeat_n(
            SkinVertex {
                joints: [joint, 0, 0, 0],
                weights: [1.0, 0.0, 0.0, 0.0],
            },
            cube.vertices.len(),
        ));
    };
    add_segment(&mut arm, [0.0, 0.5, 0.0], 0);
    add_segment(&mut arm, [0.0, 1.5, 0.0], 1);
    let ground = primitives::plane(30.0);

    let mut builder = GltfBuilder::new();
    let arm_material = builder.material([0.92, 0.48, 0.18, 1.0], 0.1, 0.4, None);
    let ground_material = builder.material([0.45, 0.47, 0.5, 1.0], 0.0, 0.9, None);
    let arm_mesh = builder.mesh("Arm", &arm, arm_material);
    let ground_mesh = builder.mesh("Ground", &ground, ground_material);
    // 节点顺序与旧手写文件一致（0=网格节点）：蒙皮经 attach_skin 后置
    // 补挂（关节先于 skin() 声明，网格节点保住下标 0）
    let arm_root = builder.node(GltfNodeDesc::new("ArmRoot").with_mesh(arm_mesh));
    let hip = builder.node(GltfNodeDesc::new("hip"));
    let elbow = builder.node(GltfNodeDesc::new("elbow").at([0.0, 1.0, 0.0]));
    builder.add_child(hip, elbow);
    let skin = builder.skin(
        "ArmSkin",
        &[hip, elbow],
        &[
            iris_core::prelude::Mat4::IDENTITY,
            iris_core::prelude::Mat4::from_translation([0.0, -1.0, 0.0].into()),
        ],
    );
    builder.attach_skin(arm_root, skin);
    builder.node(GltfNodeDesc::new("Ground").with_mesh(ground_mesh));
    // 肘挥舞：rotZ 0° → 90° → 0°，3s 循环
    let rot_z = |deg: f32| {
        let (s, c) = (deg * 0.5).to_radians().sin_cos();
        [0.0f32, 0.0, s, c]
    };
    builder.animation(
        "wave",
        &[AnimChannel {
            node: elbow,
            path: AnimationPath::Rotation,
            interpolation: AnimationInterpolation::Linear,
            times: vec![0.0, 1.5, 3.0],
            values: [0.0f32, 90.0, 0.0]
                .iter()
                .flat_map(|&deg| rot_z(deg))
                .collect(),
        }],
    );
    std::fs::write(path, builder.build_glb()).expect("写入 GLB");
}

/// 三段式蒙皮蠕虫（iris-arena 敌人）：箱体段沿 -X 排成蠕虫、逐段缩小，
/// 关节链 hip(0,0,0) → j1(-0.8) → j2(j1 局部 -0.8，全局 -1.6)，全段蒙皮
/// （首段绑 hip、中段绑 j1、末段绑 j2），扭动动画 1.2 s 循环（j1 yaw
/// +25°→−25°→+25°，j2 反相），材质暗红 + 微光 emissive。iris-arena 的
/// 敌人池实例化。
///
/// 节点树：0=WormRoot(mesh+skin) 1=hip 2=j1(T(-0.8,0,0)) 3=j2(T(-0.8,0,0))；
/// skin.joints=[1,2,3]，ibm=[I, T(0.8,0,0), T(1.6,0,0)]（j2 的 ibm 用
/// 绑定姿势的**全局** T(1.6)——节点平移是父局部，ibm 是全局的逆）。
fn write_enemy_worm_glb(path: &str) {
    // 蠕虫网格：三段箱体（沿 -X 排布，段间略缩；底面落在 y = 0）
    let mut worm = MeshData {
        skin: Some(Vec::new()),
        ..MeshData::default()
    };
    let add_segment = |data: &mut MeshData, center: [f32; 3], half: [f32; 3], joint: u32| {
        let base = data.vertices.len() as u32;
        let cube = primitives::cube();
        for v in &cube.vertices {
            data.vertices.push(Vertex {
                position: [
                    v.position[0] * half[0] + center[0],
                    v.position[1] * half[1] + center[1],
                    v.position[2] * half[2] + center[2],
                ],
                normal: v.normal,
                uv: v.uv,
            });
        }
        data.indices.extend(cube.indices.iter().map(|i| i + base));
        data.skin.as_mut().unwrap().extend(std::iter::repeat_n(
            SkinVertex {
                joints: [joint, 0, 0, 0],
                weights: [1.0, 0.0, 0.0, 0.0],
            },
            cube.vertices.len(),
        ));
    };
    // （中心, 全边长, 关节）——头部在原点，躯干与尾巴依次向 -X 排开。
    // 图元立方体跨度为 ±0.5，缩放系数即全边长；段间恰好相接：头 [−0.45,
    // 0.45] × 躯干中心 −0.85（跨 [−1.25, −0.45]）× 尾中心 −1.6（即 j2，
    // 跨 [−1.95, −1.25]），端点两两重合。
    add_segment(&mut worm, [0.0, 0.35, 0.0], [0.9, 0.7, 0.7], 0);
    add_segment(&mut worm, [-0.85, 0.30, 0.0], [0.8, 0.6, 0.6], 1);
    add_segment(&mut worm, [-1.6, 0.25, 0.0], [0.7, 0.5, 0.5], 2);

    let mut builder = GltfBuilder::new();
    let material = builder.material([0.45, 0.08, 0.08, 1.0], 0.0, 0.6, Some([0.35, 0.05, 0.05]));
    let mesh = builder.mesh("Worm", &worm, material);
    let worm_root = builder.node(GltfNodeDesc::new("WormRoot").with_mesh(mesh));
    let hip = builder.node(GltfNodeDesc::new("hip"));
    let j1 = builder.node(GltfNodeDesc::new("j1").at([-0.8, 0.0, 0.0]));
    let j2 = builder.node(GltfNodeDesc::new("j2").at([-0.8, 0.0, 0.0]));
    builder.add_child(hip, j1);
    builder.add_child(j1, j2);
    let skin = builder.skin(
        "WormSkin",
        &[hip, j1, j2],
        &[
            iris_core::prelude::Mat4::IDENTITY,
            iris_core::prelude::Mat4::from_translation([0.8, 0.0, 0.0].into()),
            iris_core::prelude::Mat4::from_translation([1.6, 0.0, 0.0].into()),
        ],
    );
    builder.attach_skin(worm_root, skin);
    // 扭动动画：j1 yaw +25° → −25° → +25°（1.2 s 三关键帧循环），j2 反相
    // （yaw θ 的四元数 = (0, sin(θ/2), 0, cos(θ/2))，两通道共用一条时间轴
    // ——builder 对内容一致的时间轴自动共享 accessor）
    let yaw = |deg: f32| {
        let (s, c) = (deg * 0.5).to_radians().sin_cos();
        [0.0f32, s, 0.0, c]
    };
    builder.animation(
        "wiggle",
        &[
            AnimChannel {
                node: j1,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.6, 1.2],
                values: [25.0f32, -25.0, 25.0]
                    .iter()
                    .flat_map(|&deg| yaw(deg))
                    .collect(),
            },
            AnimChannel {
                node: j2,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.6, 1.2],
                values: [-25.0f32, 25.0, -25.0]
                    .iter()
                    .flat_map(|&deg| yaw(deg))
                    .collect(),
            },
        ],
    );
    std::fs::write(path, builder.build_glb()).expect("写入 GLB");
}

/// 小机器人「Ascent Bot」（iris-ascent 玩家角色）：盒身 + 眼，双剪辑——
/// `idle`（原地压缩伸展呼吸，2 s 循环）与 `walk`（身体前倾 10° + 左右
/// 摇摆 ±6°，0.6 s 循环）。iris-ascent 插件运行时用动画状态机在两剪辑
/// 间做 0.15 s crossfade。
///
/// 节点树：0=BotRoot 1=Body(T(0,0.9,0) 网格以身体中心为原点，旋转/缩放
/// 围绕身体中心) 2=Eye(T(0,0.15,-0.27) 挂在 Body 下随动画联动)。**两剪辑
/// 各只声明真正驱动的通道**：idle 只写 Body 的 scale（呼吸）、walk 只写
/// Body 的 rotation（前倾 + 摇摆）——打磨 IV 摩擦①闭合后，crossfade
/// 混合期「仅源有」的通道会平滑淡回基姿势（rest =
/// `AnimationPlayer::capture_rest` 捕获的初始 TRS），过渡完成后恰停在
/// 基姿势，不再需要「两剪辑都写满 scale + rotation」的恒值通道对齐。
fn write_ascent_bot_glb(path: &str) {
    // 网格：以自身中心为原点的缩放立方体（节点平移负责摆放）
    let scaled_cube = |sx: f32, sy: f32, sz: f32| {
        let cube = primitives::cube();
        let mut data = MeshData::default();
        for v in &cube.vertices {
            data.vertices.push(Vertex {
                position: [v.position[0] * sx, v.position[1] * sy, v.position[2] * sz],
                normal: v.normal,
                uv: v.uv,
            });
        }
        data.indices = cube.indices.clone();
        data
    };
    let body = scaled_cube(0.7, 0.8, 0.5);
    let eye = scaled_cube(0.24, 0.1, 0.06);

    let mut builder = GltfBuilder::new();
    let body_material = builder.material([0.2, 0.65, 0.6, 1.0], 0.1, 0.5, None);
    let eye_material = builder.material([0.1, 0.3, 0.32, 1.0], 1.0, 1.0, Some([0.3, 1.8, 1.8]));
    let body_mesh = builder.mesh("Body", &body, body_material);
    let eye_mesh = builder.mesh("Eye", &eye, eye_material);
    let bot_root = builder.node(GltfNodeDesc::new("BotRoot"));
    let body_node = builder.node(
        GltfNodeDesc::new("Body")
            .with_mesh(body_mesh)
            .at([0.0, 0.9, 0.0]),
    );
    let eye_node = builder.node(
        GltfNodeDesc::new("Eye")
            .with_mesh(eye_mesh)
            .at([0.0, 0.15, -0.27]),
    );
    builder.add_child(bot_root, body_node);
    builder.add_child(body_node, eye_node);

    // idle 呼吸（缩放 1 → 0.93 → 1）；walk 前倾 10° + 左右摇摆 ±6°
    // （Qz·Qx）——各只一条通道（打磨 IV 摩擦①闭合）
    let lean_sway = |sway_deg: f32| {
        let q = Quat::from_rotation_z(sway_deg.to_radians())
            * Quat::from_rotation_x(10.0_f32.to_radians());
        [q.x, q.y, q.z, q.w]
    };
    builder.animation(
        "idle",
        &[AnimChannel {
            node: body_node,
            path: AnimationPath::Scale,
            interpolation: AnimationInterpolation::Linear,
            times: vec![0.0, 1.0, 2.0],
            values: vec![1.0, 1.0, 1.0, 1.0, 0.93, 1.0, 1.0, 1.0, 1.0],
        }],
    );
    builder.animation(
        "walk",
        &[AnimChannel {
            node: body_node,
            path: AnimationPath::Rotation,
            interpolation: AnimationInterpolation::Linear,
            times: vec![0.0, 0.3, 0.6],
            values: [6.0f32, -6.0, 6.0]
                .iter()
                .flat_map(|&deg| lean_sway(deg))
                .collect(),
        }],
    );
    std::fs::write(path, builder.build_glb()).expect("写入 GLB");
}

/// 落盘 .gltf JSON 与外部 .bin（目录不存在时创建）。
fn write_pair(gltf_path: &str, bin_name: &str, json: &str, bin: &[u8]) {
    let gltf_file = std::path::Path::new(gltf_path);
    if let Some(parent) = gltf_file.parent() {
        std::fs::create_dir_all(parent).expect("创建 assets 目录");
    }
    std::fs::write(gltf_path, json).expect("写入 glTF");
    std::fs::write(gltf_file.parent().unwrap().join(bin_name), bin).expect("写入 bin");
}

// ---- Radiance .hdr 全景图生成（外部 HDR 环境贴图示例用） ----

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = dot3(v, v).sqrt();
    [v[0] / len, v[1] / len, v[2] / len]
}

fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// 程序化生成 1024×512 equirectangular 日落全景（Radiance `.hdr`，平面编码）：
/// 地平线暖橙 (3.0, 1.2, 0.4) → 天顶深蓝 (0.05, 0.1, 0.3)，地平线下深灰渐变，
/// 外加一个角半径约 0.04 rad、亮度 ~40 的软边太阳盘（HDR 亮部，泛光提取源）。
/// `hdr_snapshot` 示例加载。生成端的 dir→(u, v) 映射与捕获 shader 精确互逆：
/// 图像第 0 行 = +Y 天顶，+X 方向在图像水平正中。
fn write_sunset_hdr(path: &str) {
    use iris_render::asset::hdr::{HdrImage, encode_hdr};

    const W: u32 = 1024;
    const H: u32 = 512;
    const PI: f32 = std::f32::consts::PI;
    let sun = normalize3([0.3, 0.25, -0.9]);
    let sun_color = [1.0, 0.72, 0.45];
    let sun_radius: f32 = 0.04; // 角半径（弧度）
    let sun_softness: f32 = 0.3; // 边缘在半径最后 30% 内软化

    let mut image = HdrImage::new(W, H);
    for y in 0..H {
        // v = 0 为天顶行：行中心的仰角 elev = (0.5 - v)·π
        let elev = (0.5 - (y as f32 + 0.5) / H as f32) * PI;
        let (sin_elev, cos_elev) = elev.sin_cos();
        for x in 0..W {
            let az = ((x as f32 + 0.5) / W as f32 - 0.5) * 2.0 * PI;
            let dir = [cos_elev * az.cos(), sin_elev, cos_elev * az.sin()];
            // 天空底色：上半球暖橙→深蓝，下半球地平线灰→暗部渐变
            let mut rgb = if sin_elev >= 0.0 {
                mix3([3.0, 1.2, 0.4], [0.05, 0.1, 0.3], sin_elev.sqrt())
            } else {
                mix3([0.30, 0.27, 0.25], [0.02, 0.02, 0.03], (-sin_elev).sqrt())
            };
            // 太阳盘：角距落入半径内点亮，边缘 smoothstep 软化
            let angle = dot3(dir, sun).clamp(-1.0, 1.0).acos();
            let edge = ((sun_radius - angle) / (sun_softness * sun_radius)).clamp(0.0, 1.0);
            let disc = edge * edge * (3.0 - 2.0 * edge);
            for c in 0..3 {
                rgb[c] += sun_color[c] * 40.0 * disc;
            }
            image.set_pixel(x, y, rgb);
        }
    }
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent).expect("创建 assets 目录");
    }
    std::fs::write(path, encode_hdr(&image)).expect("写入 sunset.hdr");
}
