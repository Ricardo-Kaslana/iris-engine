//! 工具：方向光级联阴影（CSM v2）视觉验收场景（离屏渲染输出 `docs/csm.png`）。
//!
//! 大场景（近 / 中 / 远三距物体排开，~90 m 跨度）俯斜视：级联 0 覆盖
//! 近处（高纹素密度、影子锐利），级联 1 / 2 依次铺到 120 m 覆盖距离——
//! 远处物体脚下应有影子（v1 单张贴图在同等近景精度下远处无影），
//! 级联过渡带无可见跳变。
//!
//! ```sh
//! cargo run --example csm_snapshot --release
//! ```
//!
//! 产物：`docs/csm.png`（从仓库根目录运行）。

use iris_core::hierarchy::propagate_transforms;
use iris_engine::prelude::*;

fn main() {
    env_logger::init();
    let Some(mut renderer) = OffscreenRenderer::new(1280, 720) else {
        eprintln!("无 GPU");
        std::process::exit(1);
    };

    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    // 低机位缓俯角：可见地面从级联 0 一直铺到级联 2（~120 m 覆盖带）
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 12.0, 60.0),
        target: Vec3::new(0.0, 0.0, -40.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        intensity: 2.2,
        ..DirectionalLight::default()
    });

    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let sphere = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, 48, 24));

    // 近 / 中 / 远三距物体左右错开（默认光影子向 (−0.5,−0.4)·h 平移）：
    // 近处影子应锐利，远处影子应存在且无级联跳变。中景物体放右侧，
    // 避免遮挡相机看向远处物体影子的视线。
    let props: [(f32, f32, [f32; 3], f32, f32, bool); 6] = [
        // x, z, 颜色, 半宽, 高度, 是否方块
        (-7.0, 30.0, [0.9, 0.3, 0.2], 2.0, 4.0, true), // 近（级联 0）
        (7.0, 18.0, [0.95, 0.75, 0.3], 1.8, 3.6, false), // 近球（级联 0/1 间）
        (9.0, 2.0, [0.3, 0.55, 0.85], 2.5, 6.0, true), // 中（级联 1）
        (-8.0, -16.0, [0.45, 0.8, 0.5], 2.2, 4.4, false), // 中远球（级联 1）
        (-5.0, -44.0, [0.2, 0.7, 0.4], 5.0, 12.0, true), // 远（级联 2）
        (8.0, -76.0, [0.75, 0.4, 0.8], 5.5, 13.0, true), // 远（级联 2 深处）
    ];
    for (x, z, color, half, height, is_cube) in props {
        let mat = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(color[0], color[1], color[2], 1.0));
        let mesh = if is_cube { cube } else { sphere };
        let transform = if is_cube {
            Transform {
                position: Vec3::new(x, height * 0.5, z),
                scale: Vec3::new(half * 2.0, height, half * 2.0),
                ..Transform::IDENTITY
            }
        } else {
            Transform::from_xyz(x, height * 0.6, z)
        };
        world.spawn((transform, mesh, mat));
    }

    // 大地面（±120 m，覆盖全部级联足印）
    let plane = world
        .resource_mut::<MeshStore>()
        .add(MeshData::plane(240.0));
    let mat_g = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.5, 0.55, 0.5, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_g));

    propagate_transforms(&mut world);
    renderer.render(&world);
    let pixels = renderer.read_pixels();
    let (w, h) = renderer.size();
    image::RgbaImage::from_raw(w, h, pixels)
        .expect("像素缓冲尺寸匹配")
        .save("docs/csm.png")
        .expect("写 docs/csm.png");
    println!("已输出 docs/csm.png（{w}x{h}）");
}
