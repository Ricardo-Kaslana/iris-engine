//! 工具：顶点色与受控材质扩展的视觉验收（离屏渲染）。
//!
//! 同一排三个球体携带相同的逐顶点 RGB 渐变，从左到右依次是：
//! 1. 默认 `Ignore`：顶点色不参与着色，球体呈现基色并保留完整的明暗；
//! 2. `Multiply`：顶点色乘进反照率，球体呈现逐顶点渐变并保留明暗；
//! 3. `Multiply + Unlit`：渐变直接出图，没有受光面与暗面的区分。
//!
//! 输出 `vertex_color.bmp`。转 PNG：
//! ```sh
//! ffmpeg -y -i vertex_color.bmp docs/vertex_color.png
//! ```
//!
//! ```sh
//! cargo run -p iris-render --example vertex_color_snapshot --release
//! ```

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

const SIZE: u32 = 640;

/// 顶点色按本地位置铺满 RGB 渐变：x 控制红、y 控制绿、z 控制蓝。
fn gradient_sphere() -> MeshData {
    const RADIUS: f32 = 0.7;
    let mut mesh = MeshData::uv_sphere(RADIUS, 32, 20);
    mesh.colors = Some(
        mesh.vertices
            .iter()
            .map(|v| {
                let channel = |value: f32| {
                    ((value / (2.0 * RADIUS) + 0.5).clamp(0.0, 1.0) * 255.0).round() as u8
                };
                VertexColor::new(
                    channel(v.position[0]),
                    channel(v.position[1]),
                    channel(v.position[2]),
                    255,
                )
            })
            .collect(),
    );
    mesh
}

fn world_with_cubes() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 2.4, 8.0),
        target: Vec3::new(0.0, 0.8, 0.0),
        ..Camera::default()
    });
    // 强侧光 + 低环境光：前两个立方体的明暗面清晰可见，右侧 Unlit
    // 立方体保持无光照的平坦渐变，三者的差异一眼可辨。
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-1.0, -0.35, 0.3).normalize(),
        intensity: 2.2,
        ambient: Vec3::splat(0.08),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());

    let ground = world.resource_mut::<MeshStore>().add(MeshData::plane(30.0));
    let ground_material = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.35, 0.35, 0.38, 1.0));
    world.spawn((Transform::IDENTITY, ground, ground_material));

    let mut place = |x: f32, material: MaterialDef| {
        let mesh = world.resource_mut::<MeshStore>().add(gradient_sphere());
        let material = world.resource_mut::<MaterialStore>().add(material);
        world.spawn((
            Transform {
                position: Vec3::new(x, 0.72, 0.0),
                ..Transform::IDENTITY
            },
            mesh,
            material,
        ));
    };
    place(-2.2, MaterialDef::solid(0.9, 0.9, 0.9, 1.0));
    place(
        0.0,
        MaterialDef::solid(1.0, 1.0, 1.0, 1.0).with_vertex_color(VertexColorMode::Multiply),
    );
    place(
        2.2,
        MaterialDef::solid(1.0, 1.0, 1.0, 1.0)
            .with_vertex_color(VertexColorMode::Multiply)
            .with_unlit(),
    );
    propagate_transforms(&mut world);
    world
}

fn main() {
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("无 GPU，无法离屏渲染");
        std::process::exit(1);
    };
    let world = world_with_cubes();
    renderer.render(&world);
    renderer
        .save_bmp("vertex_color.bmp")
        .expect("写顶点色验收帧");
    println!("已写出 vertex_color.bmp（左到右：Ignore / Multiply / Multiply+Unlit）");
}
