//! 工具：R43 景深 + 运动模糊视觉验收（离屏渲染，四帧拼图）。
//!
//! 同一纵深场景（近/中/远三排彩色立方体 + 暗地面）渲染四帧：
//!
//! 1. **DOF 关**：全画面清晰的基准帧（资源缺席语义，与无设置逐位一致）；
//! 2. **DOF 对焦中排**：聚焦带（±focus_range）锐利、远景糊化（CoC 泊松
//!    盘平均）、近景轻糊——浅景深的纵深分离；
//! 3. **DOF 对焦远景**：聚焦带切到远排，中排反而糊——对焦距离语义；
//! 4. **运动模糊开**：相机沿 X 平移的帧间速度场驱动的方向性拖影
//!    （重投影上一帧 view_proj，静止场景 + 运动相机 = 全画面同向模糊）。
//!
//! 输出四张 640×360 BMP，ffmpeg 拼成 2×2 单张：
//!
//! ```sh
//! cargo run --example dof_snapshot --release
//! ffmpeg -y -i iris_dof_off.bmp -i iris_dof_mid.bmp -i iris_dof_far.bmp \
//!     -i iris_dof_motion.bmp -filter_complex \
//!     "[0][1]hstack[t];[2][3]hstack[b];[t][b]vstack" docs/dof_motion.png
//! ```

use iris_core::hierarchy::propagate_transforms;
use iris_engine::prelude::*;

fn build_world(dof: Option<DepthOfFieldSettings>, blur: Option<MotionBlurSettings>) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());
    if let Some(dof) = dof {
        world.insert_resource(dof);
    }
    if let Some(blur) = blur {
        world.insert_resource(blur);
    }

    // 近/中/远三排彩色立方体：纵深分离的判读基准（各排异色）
    let colors: [([f32; 4], f32); 3] = [
        ([0.85, 0.25, 0.2, 1.0], 2.0), // 近排红
        ([0.2, 0.7, 0.3, 1.0], 9.0),   // 中排绿（默认对焦）
        ([0.25, 0.4, 0.9, 1.0], 20.0), // 远排蓝
    ];
    for &(color, z) in &colors {
        let material = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(color[0], color[1], color[2], color[3]));
        let mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
        for i in 0..3i32 {
            world.spawn((
                Transform::from_xyz(i as f32 * 1.6 - 1.6, 0.5, -z),
                mesh,
                material,
            ));
        }
    }
    propagate_transforms(&mut world);
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 1.4, 5.0);
        camera.target = Vec3::new(0.0, 0.6, -9.0);
    }
    world
}

fn render(out: &str, dof: Option<DepthOfFieldSettings>, blur: Option<MotionBlurSettings>) {
    let Some(mut renderer) = OffscreenRenderer::new(640, 360) else {
        eprintln!("无 GPU");
        std::process::exit(1);
    };
    let world = build_world(dof, blur);
    renderer.render(&world);
    renderer.save_bmp(out).expect("写 BMP");
    println!("已输出 {out}");
}

fn main() {
    env_logger::init();
    render("iris_dof_off.bmp", None, None);
    render(
        "iris_dof_mid.bmp",
        Some(DepthOfFieldSettings {
            enabled: true,
            focus_distance: 14.0, // 相机到中排的视距（5 + 9）
            focus_range: 1.5,
            max_blur_radius: 10.0,
        }),
        None,
    );
    render(
        "iris_dof_far.bmp",
        Some(DepthOfFieldSettings {
            enabled: true,
            focus_distance: 25.0, // 远排
            focus_range: 1.5,
            max_blur_radius: 10.0,
        }),
        None,
    );
    render(
        "iris_dof_motion.bmp",
        None,
        Some(MotionBlurSettings {
            enabled: true,
            strength: 48.0,
            ..MotionBlurSettings::default()
        }),
    );
}
