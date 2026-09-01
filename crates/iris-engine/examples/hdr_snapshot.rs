//! 工具：外部 HDR 环境贴图视觉验收（离屏渲染，输出 `iris_hdr.bmp`）。
//!
//! 加载 `assets/sunset.hdr`（gen_asset 生成：1024×512 equirectangular 日落
//! 全景，地平线暖橙 + 天顶深蓝 + 亮度 ~40 的太阳盘），经 `SkySource` 资源
//! 挂入世界——渲染器检测 generation 变化自动重烘 IBL 环境贴图并重建绑定组；
//! 场景为金属铬球 + 地面，铬球反射暖色天空与太阳（泛光提取源），
//! 地面承接 HDR 环境的漫反射辐射。
//!
//! ```sh
//! cargo run --example gen_asset --release
//! cargo run --example hdr_snapshot --release
//! ```

use iris_core::hierarchy::propagate_transforms;
use iris_engine::prelude::*;
use iris_render::asset::load_hdr;

fn main() {
    env_logger::init();
    let Some(mut renderer) = OffscreenRenderer::new(640, 360) else {
        eprintln!("无 GPU");
        std::process::exit(1);
    };
    let image = load_hdr("assets/sunset.hdr").expect("加载 sunset.hdr（先运行 gen_asset 示例）");

    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    // 弱方向光 + 低环境补光：画面辐射几乎全部来自 HDR 环境源
    world.insert_resource(DirectionalLight {
        intensity: 0.25,
        ambient: Vec3::splat(0.02),
        ..DirectionalLight::default()
    });
    // 外部 HDR 天空源：generation 失效机制让渲染器首帧即重烘 IBL
    world.insert_resource(SkySource::equirect_hdr(image));

    let sphere = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, 48, 24));
    let chrome = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::metal(0.95, 0.95, 0.98, 0.05));
    world.spawn((Transform::from_xyz(0.0, 1.0, 0.0), sphere, chrome));

    let ground = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let mat_ground = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.25, 0.25, 0.25, 1.0));
    world.spawn((Transform::IDENTITY, ground, mat_ground));

    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 1.4, 4.5);
        camera.target = Vec3::new(0.0, 1.0, 0.0);
    }

    propagate_transforms(&mut world);
    renderer.render(&world);
    renderer.save_bmp("iris_hdr.bmp").expect("写 BMP");
    println!("已输出 iris_hdr.bmp");
}
