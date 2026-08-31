//! 工具：骨骼蒙皮视觉验收（离屏渲染，输出两个时刻的 `iris_skin_*.bmp`）。
//!
//! 加载 `assets/skin_arm.glb`（gen_asset 生成：两段臂绑髋/肘关节，
//! 肘关节 rotZ 0°→90°→0° 挥舞），分别渲染 t=0（竖直）与 t=1.5（弯成
//! 直角，上段水平指向 -X）两个时刻。
//!
//! ```sh
//! cargo run --example gen_asset --release
//! cargo run --example skin_snapshot --release
//! ```

use iris_core::hierarchy::propagate_transforms;
use iris_engine::prelude::*;

fn render_at(time: f32, out: &str) {
    let Some(mut renderer) = OffscreenRenderer::new(640, 360) else {
        eprintln!("无 GPU");
        std::process::exit(1);
    };
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.4, -1.0, -0.35),
        ..DirectionalLight::default()
    });

    let instance =
        spawn_gltf_instance(&mut world, "assets/skin_arm.glb").expect("加载 skin_arm.glb");
    let root = instance.roots[0];
    world.insert(root, instance.player());
    {
        let mut player = world.get_mut::<AnimationPlayer>(root).unwrap();
        player.looping = false;
        player.time = time;
    }
    update_animations(&mut world);

    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(2.6, 2.0, 4.2);
        camera.target = Vec3::new(-0.2, 0.9, 0.0);
    }

    propagate_transforms(&mut world);
    renderer.render(&world);
    renderer.save_bmp(out).expect("写 BMP");
    println!("已输出 {out}");
}

fn main() {
    env_logger::init();
    render_at(0.0, "iris_skin_t0.bmp");
    render_at(1.5, "iris_skin_t1.bmp");
}
