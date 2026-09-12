//! 工具：士兵模型多机位预览（离屏渲染，目检验收用）。
//!
//! 把 `assets/assault_bot.glb`（资产生成器产物）载入最小世界——三点光照 +
//! 地面 + **真实动画姿势**（上半身 guard 剪辑 + 下半身 idle，即游戏里常驻的
//! 待机架势；绑定姿势的「T 字站桩」不是玩家看到的样子）——按十个机位（四个全身、
//! 头 / 躯干 / 双手 / 双靴特写）各渲一帧落盘 BMP，并打印节点 / 剪辑 / 三角形 /
//! draw call 统计。
//!
//! ```sh
//! cargo run -p iris-assault --example soldier_preview --release
//! ffmpeg -y -i soldier_01_front.bmp -i soldier_02_three_quarter.bmp -i soldier_03_side.bmp \
//!   -i soldier_04_back.bmp -i soldier_05_head.bmp -i soldier_06_torso.bmp \
//!   -i soldier_07_hand_right.bmp -i soldier_08_hand_left.bmp -i soldier_09_boots.bmp \
//!   -i soldier_10_boot_side.bmp \
//!   -filter_complex "[0][1][2][3][4]hstack=5[a];[5][6][7][8][9]hstack=5[b];[a][b]vstack=2" \
//!   -update 1 docs/soldier.png
//! ```

use std::time::Duration;

use iris_engine::prelude::*;

/// 机位表：`(文件名, 眼位, 注视点, 垂直视场角°)`。
///
/// 全身四机位 + 六个特写（头 / 躯干装具 / 右手握把 / 左臂手 / 双靴 / 靴侧面）。
/// 全身四机位统一**距离 4.30 m / 30° 视场**：竖直覆盖 2 × 4.30 × tan15° ≈ 2.30 m，
/// 臂展 2.07 m 的角色全身落在画面 90% 内——第一版 2.95 m / 38° 只覆盖 2.03 m，
/// 头顶与靴底同时被裁掉（目检时误判成「模型缺件」）。
///
/// **手 / 足各给两个特写**：它们是角色建模返工的两个重点，而全身机位上根本看不清
/// （4.30 m 处一只手只占画面高 4%）。特写机位离被摄件 0.9~1.4 m、竖直覆盖
/// 0.5~0.75 m —— 一只手 0.19 m、一只靴 0.30 m 都占画面 1/3 以上，指节护片、
/// 鞋眼、足弓抬起这类「几毫米级」的形态只有在特写里才谈得上目检。
fn views() -> [(&'static str, Vec3, Vec3, f32); 6] {
    [
        (
            "soldier_01_front.bmp",
            Vec3::new(0.0, 1.08, -4.30),
            Vec3::new(0.0, 1.03, 0.0),
            30.0,
        ),
        (
            "soldier_02_three_quarter.bmp",
            Vec3::new(2.85, 1.32, 3.22),
            Vec3::new(0.0, 1.03, 0.0),
            30.0,
        ),
        (
            "soldier_03_side.bmp",
            Vec3::new(4.30, 1.06, 0.05),
            Vec3::new(0.0, 1.03, 0.0),
            30.0,
        ),
        (
            "soldier_04_back.bmp",
            Vec3::new(0.0, 1.12, 4.30),
            Vec3::new(0.0, 1.05, 0.0),
            30.0,
        ),
        (
            "soldier_05_head.bmp",
            Vec3::new(0.80, 2.02, 0.98),
            Vec3::new(0.0, 1.90, 0.0),
            30.0,
        ),
        (
            "soldier_06_torso.bmp",
            Vec3::new(1.35, 1.62, 1.55),
            Vec3::new(0.0, 1.44, -0.02),
            32.0,
        ),
    ]
}

/// 特写机位：眼位 = 注视点 + 单位方向 × 距离。
///
/// **手 / 靴这种「小、且被动画摆到任意位置」的部位，写死机位必然拍空**——
/// 第一版 hand_right 机位就只拍到了弹匣与空景。改为由骨节点（R_HAND / L_HAND /
/// 双踝）的**实测世界位置**取景：剪辑改了姿势，特写跟着走，不会失焦。
fn closeup(
    name: &'static str,
    target: Vec3,
    direction: Vec3,
    distance: f32,
    fov: f32,
) -> (&'static str, Vec3, Vec3, f32) {
    (name, target + direction.normalize() * distance, target, fov)
}

/// 骨节点世界位置（动画推进后才是玩家看到的姿势）。
fn node_at(world: &World, nodes: &[Option<Entity>], index: usize) -> Vec3 {
    let entity = nodes[index].unwrap_or_else(|| panic!("节点 {index} 不在场景内"));
    world
        .get::<GlobalTransform>(entity)
        .unwrap_or_else(|| panic!("节点 {index} 缺少 GlobalTransform"))
        .position()
}

/// 预览台：最小世界 + 层级 / 动画系统（与游戏同一条动画推进路径）。
struct Harness {
    app: App,
    /// 骨节点实体（下标 = skeleton 关节序，特写取景用）。
    nodes: Vec<Option<Entity>>,
}

impl Harness {
    fn new() -> Self {
        let mut app = App::new();
        app.world.insert_resource(Time::default());
        app.world.insert_resource(MeshStore::new());
        app.world.insert_resource(MaterialStore::new());
        app.world.insert_resource(TextureStore::new());
        app.world.insert_resource(Camera::default());
        app.world.insert_resource(Input::new());
        app.world.insert_resource(Viewport::new(600, 800));
        // 主光从角色**面前**（−Z）的右上打来：机位表里的「front」在 −Z，
        // 主光若仍从 +Z 来则正面机位全是背光（第一版把 front/back 机位写反，
        // 主光跟着反，正面细节全糊在阴影里）。
        app.world.insert_resource(DirectionalLight {
            direction: Vec3::new(-0.42, -0.80, 0.44),
            color: Vec3::new(1.0, 0.96, 0.90),
            intensity: 1.05,
            ambient: Vec3::splat(0.10),
            shadows: true,
            shadow_range: 12.0,
        });
        app.world
            .insert_resource(EnvironmentSettings { intensity: 0.30 });
        HierarchyPlugin.build(&mut app);
        AnimationPlugin.build(&mut app);
        // 补光 / 轮廓光（把暗部材质也照出来）
        for (position, color, intensity) in [
            (Vec3::new(-2.0, 2.2, 1.7), Vec3::new(1.00, 0.93, 0.84), 6.5),
            (Vec3::new(-1.7, 1.6, -2.0), Vec3::new(0.72, 0.82, 1.00), 5.0),
        ] {
            app.world.spawn((
                Transform::from_translation(position),
                PointLight {
                    color,
                    intensity,
                    range: 14.0,
                    shadows: false,
                },
            ));
        }
        // 地面
        let ground = app
            .world
            .resource_mut::<MeshStore>()
            .add(MeshData::plane(24.0));
        let ground_mat = app
            .world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.22, 0.22, 0.24, 1.0));
        app.world.spawn((Transform::IDENTITY, ground, ground_mat));

        // 角色（生成器产物：22 骨 + 1 蒙皮网格 + 单材质图集）
        let path = asset_path("assets/assault_bot.glb");
        let instance = spawn_gltf_instance(&mut app.world, &path)
            .unwrap_or_else(|err| panic!("加载 {} 失败：{err}", path.display()));
        println!(
            "节点 {} / 剪辑 {} / 顶层根 {} / 纹理 {}",
            instance.node_entities.len(),
            instance.clips.len(),
            instance.roots.len(),
            instance.textures.len()
        );
        let model = instance.roots[0];
        let mut player = instance.player();
        // 上半身 guard（持刀架势）+ 下半身 idle：与游戏默认姿势一致
        if let Some(index) = instance.clips.iter().position(|clip| clip.name == "guard") {
            player.active = index;
            player.looping = true;
        }
        app.world.insert(model, player);
        let nodes = instance.node_entities.clone();
        let mut harness = Self { app, nodes };
        // 推进 0.5 s：姿势收敛（动画系统 + 层级传播）
        for _ in 0..30 {
            harness
                .app
                .world
                .resource_mut::<Time>()
                .advance(Duration::from_secs_f64(1.0 / 60.0));
            harness
                .app
                .schedule
                .run(Stage::Update, &mut harness.app.world);
            harness
                .app
                .schedule
                .run(Stage::PostUpdate, &mut harness.app.world);
        }
        harness
    }
}

fn main() {
    let Some(mut renderer) = OffscreenRenderer::new(600, 800) else {
        eprintln!("跳过：环境无 GPU");
        std::process::exit(1);
    };
    let mut harness = Harness::new();
    let world = &mut harness.app.world;

    // 蒙皮网格统计（网格实体 / 蒙皮 / 三角形）
    let mut meshes = 0usize;
    let mut triangles = 0usize;
    let mut skinned = 0usize;
    world.query::<(&MeshHandle, Option<&Skin>)>(|(handle, skin)| {
        meshes += 1;
        if let Some(data) = world.resource::<MeshStore>().get(*handle) {
            triangles += data.indices.len() / 3;
        }
        if skin.is_some() {
            skinned += 1;
        }
    });
    println!("网格实体 {meshes} / 蒙皮 {skinned} / 三角形 {triangles}");

    let nodes = std::mem::take(&mut harness.nodes);
    let hand_r = node_at(world, &nodes, 16);
    let hand_l = node_at(world, &nodes, 13);
    let foot_l = node_at(world, &nodes, 7);
    let foot_r = node_at(world, &nodes, 10);
    // 特写距离取「被摄件 0.33~0.62 m 竖直覆盖」：一只手 0.19 m 占画面 55% 以上
    let closeups = [
        // 从**掌心侧 + 面前**看：手指朝掌心内卷，从手背侧看只能看到一团
        // （第一版机位在手臂轴线后方，拍到的是护腕）
        closeup(
            "soldier_07_hand_right.bmp",
            hand_r,
            Vec3::new(-0.58, 0.36, -0.73),
            0.68,
            30.0,
        ),
        closeup(
            "soldier_08_hand_left.bmp",
            hand_l,
            Vec3::new(0.58, 0.36, -0.73),
            0.68,
            30.0,
        ),
        closeup(
            "soldier_09_boots.bmp",
            (foot_l + foot_r) * 0.5 + Vec3::new(0.0, 0.10, 0.0),
            Vec3::new(0.58, 0.46, 0.67),
            1.15,
            30.0,
        ),
        closeup(
            "soldier_10_boot_side.bmp",
            foot_l + Vec3::new(0.02, 0.10, -0.03),
            Vec3::new(-0.96, 0.24, 0.14),
            0.86,
            30.0,
        ),
    ];
    for (name, eye, target, fov) in views().into_iter().chain(closeups) {
        {
            let mut camera = world.resource_mut::<Camera>();
            camera.eye = eye;
            camera.target = target;
            camera.fov_y_radians = fov.to_radians();
            camera.z_near = 0.05;
            camera.z_far = 60.0;
        }
        renderer.render(world);
        renderer.save_bmp(name).expect("写 BMP");
        let stats = renderer.renderer().stats();
        println!(
            "{name}: draw_calls={} instances={} triangles={}",
            stats.draw_calls, stats.instances, stats.triangles_drawn
        );
    }
    println!("完成：六机位 BMP 已落盘（仓库根）");
}
