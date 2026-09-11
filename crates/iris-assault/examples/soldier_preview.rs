//! 工具：士兵模型多机位预览（离屏渲染，目检验收用）。
//!
//! 把 `assets/assault_bot.glb`（资产生成器产物）载入最小世界——三点光照 +
//! 地面 + **真实动画姿势**（上半身 guard 剪辑 + 下半身 idle，即游戏里常驻的
//! 待机架势；绑定姿势的「T 字站桩」不是玩家看到的样子）——按六个机位各渲
//! 一帧落盘 BMP，并打印节点 / 剪辑 / 三角形 / draw call 统计。
//!
//! ```sh
//! cargo run -p iris-assault --example soldier_preview --release
//! ffmpeg -y -i soldier_01_front.bmp -i soldier_02_three_quarter.bmp -i soldier_03_side.bmp \
//!   -i soldier_04_back.bmp -i soldier_05_head.bmp -i soldier_06_weapons.bmp \
//!   -filter_complex "[0][1][2]hstack=3[a];[3][4][5]hstack=3[b];[a][b]vstack=2" -update 1 docs/soldier.png
//! ```

use std::time::Duration;

use iris_engine::prelude::*;

/// 机位表：`(文件名, 眼位, 注视点, 垂直视场角°)`。
fn views() -> [(&'static str, Vec3, Vec3, f32); 6] {
    [
        (
            "soldier_01_front.bmp",
            Vec3::new(0.0, 1.05, 2.95),
            Vec3::new(0.0, 1.00, 0.0),
            38.0,
        ),
        (
            "soldier_02_three_quarter.bmp",
            Vec3::new(1.95, 1.30, 2.15),
            Vec3::new(0.0, 1.00, 0.0),
            38.0,
        ),
        (
            "soldier_03_side.bmp",
            Vec3::new(2.95, 1.02, 0.05),
            Vec3::new(0.0, 1.00, 0.0),
            38.0,
        ),
        (
            "soldier_04_back.bmp",
            Vec3::new(0.0, 1.10, -2.95),
            Vec3::new(0.0, 1.02, 0.0),
            38.0,
        ),
        (
            "soldier_05_head.bmp",
            Vec3::new(0.72, 2.02, 1.10),
            Vec3::new(0.0, 1.86, 0.0),
            34.0,
        ),
        (
            "soldier_06_weapons.bmp",
            Vec3::new(1.25, 1.62, 1.30),
            Vec3::new(0.30, 1.28, -0.15),
            36.0,
        ),
    ]
}

/// 预览台：最小世界 + 层级 / 动画系统（与游戏同一条动画推进路径）。
struct Harness {
    app: App,
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
        app.world.insert_resource(Viewport::new(640, 880));
        app.world.insert_resource(DirectionalLight {
            direction: Vec3::new(-0.42, -0.80, -0.44),
            color: Vec3::new(1.0, 0.96, 0.90),
            intensity: 2.4,
            ambient: Vec3::splat(0.10),
            shadows: true,
            shadow_range: 12.0,
        });
        app.world
            .insert_resource(EnvironmentSettings { intensity: 0.40 });
        HierarchyPlugin.build(&mut app);
        AnimationPlugin.build(&mut app);
        // 补光 / 轮廓光（把暗部材质也照出来）
        for (position, color, intensity) in [
            (Vec3::new(-2.0, 2.2, 1.7), Vec3::new(1.00, 0.93, 0.84), 16.0),
            (Vec3::new(1.9, 2.1, -1.8), Vec3::new(0.72, 0.82, 1.00), 18.0),
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
        let mut harness = Self { app };
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
    let Some(mut renderer) = OffscreenRenderer::new(640, 880) else {
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

    for (name, eye, target, fov) in views() {
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
