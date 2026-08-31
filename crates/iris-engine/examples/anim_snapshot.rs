//! 工具：GLB 动画视觉验收（离屏渲染，输出两个时刻的 `iris_anim_*.bmp`）。
//!
//! 加载 `assets/anim_cube.glb`（gen_asset 生成：自转 4s + 浮动 4s 循环），
//! 分别渲染 t=0 与 t=1（yaw 90°、浮到 +0.3）两个时刻。
//!
//! ```sh
//! cargo run --example gen_asset --release
//! cargo run --example anim_snapshot --release
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
    world.insert_resource(DirectionalLight::default());

    let instance =
        spawn_gltf_instance(&mut world, "assets/anim_cube.glb").expect("加载 anim_cube.glb");
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
        camera.eye = Vec3::new(2.2, 1.6, 3.4);
        camera.target = Vec3::new(0.0, 0.5, 0.0);
    }

    propagate_transforms(&mut world);
    renderer.render(&world);
    renderer.save_bmp(out).expect("写 BMP");
    println!("已输出 {out}");
}

fn main() {
    env_logger::init();
    render_at(0.0, "iris_anim_t0.bmp");
    render_at(1.0, "iris_anim_t1.bmp");
}
