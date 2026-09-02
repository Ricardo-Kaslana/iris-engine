//! 工具：后处理链 v1 视觉验收（离屏渲染，曝光 × 暗角 × 泛光三帧）。
//!
//! 同一 HDR 场景（暗地面 + 三颗高自发光彩球）渲染三帧，验证后处理链
//! 三个控制量的视觉语义：
//!
//! 1. **欠曝 0.4**：画面整体压暗，暗部层次保留（线性 HDR × 曝光后
//!    ACES 滚降，不是简单乘黑）；
//! 2. **标准 1.0**：中性基准帧（曝光资源缺席语义 = 1.0，画面与无曝光
//!    控制逐位一致）；
//! 3. **过曝 2.5 + 暗角 0.9 + 强泛光**：亮源泛光扩散、四角压暗而中心
//!    无损——暗角在线性 HDR 域、曝光之前作用（镜头物理遮挡先于传感器
//!    增益），blit 色调映射是链的固定终点。
//!
//! 输出三张 640×360 BMP，可用 ffmpeg 横向拼成单张宽幅预览：
//!
//! ```sh
//! cargo run --example post_snapshot --release
//! ffmpeg -y -i iris_post_underexposed.bmp -i iris_post_standard.bmp \
//!     -i iris_post_overexposed.bmp -filter_complex hstack=inputs=3 \
//!     docs/post.png
//! ```

use iris_core::hierarchy::propagate_transforms;
use iris_engine::prelude::*;

fn main() {
    env_logger::init();
    let Some(mut renderer) = OffscreenRenderer::new(640, 360) else {
        eprintln!("无 GPU");
        std::process::exit(1);
    };

    // 三帧共用同一场景：暗地面 + 三颗高自发光彩球（HDR 亮源）
    let scene = |exposure: f32, vignette: f32, bloom_strength: f32| {
        let mut world = World::new();
        world.insert_resource(MeshStore::new());
        world.insert_resource(MaterialStore::new());
        world.insert_resource(TextureStore::new());
        world.insert_resource(Camera::default());
        // 弱照明 + 低环境光：发光体是画面里唯一的亮源
        world.insert_resource(DirectionalLight {
            intensity: 0.6,
            ambient: Vec3::splat(0.05),
            ..DirectionalLight::default()
        });
        world.insert_resource(ExposureSettings { exposure });
        if vignette > 0.0 {
            world.insert_resource(VignetteSettings { strength: vignette });
        }
        world.insert_resource(BloomSettings {
            strength: bloom_strength,
            ..BloomSettings::default()
        });

        let sphere = world
            .resource_mut::<MeshStore>()
            .add(MeshData::uv_sphere(0.9, 48, 24));
        let glow = |emissive: [f32; 3]| MaterialDef {
            emissive: [emissive[0], emissive[1], emissive[2], 1.0],
            ..MaterialDef::solid(0.0, 0.0, 0.0, 1.0)
        };
        let mats: [([f32; 3], [f32; 3]); 3] = [
            ([22.0, 9.0, 2.5], [0.0, 1.1, 0.0]),    // 橙
            ([1.5, 14.0, 18.0], [2.6, 1.0, -0.8]),  // 青
            ([20.0, 3.5, 13.0], [-2.6, 1.0, -0.8]), // 品红
        ];
        for (emissive, pos) in mats {
            let mat = world.resource_mut::<MaterialStore>().add(glow(emissive));
            world.spawn((Transform::from_xyz(pos[0], pos[1], pos[2]), sphere, mat));
        }

        let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
        let mat_g = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.12, 0.12, 0.14, 1.0));
        world.spawn((Transform::IDENTITY, plane, mat_g));

        {
            let mut camera = world.resource_mut::<Camera>();
            camera.eye = Vec3::new(0.0, 2.2, 7.5);
            camera.target = Vec3::new(0.0, 1.1, 0.0);
        }

        propagate_transforms(&mut world);
        world
    };

    let frames: [(&str, f32, f32, f32); 3] = [
        ("iris_post_underexposed.bmp", 0.4, 0.0, 0.15),
        ("iris_post_standard.bmp", 1.0, 0.0, 0.15),
        ("iris_post_overexposed.bmp", 2.5, 0.9, 0.45),
    ];
    for (path, exposure, vignette, bloom) in frames {
        renderer.render(&scene(exposure, vignette, bloom));
        renderer.save_bmp(path).expect("写 BMP");
        println!("已输出 {path}（曝光 {exposure}，暗角 {vignette}，泛光 {bloom}）");
    }
}
