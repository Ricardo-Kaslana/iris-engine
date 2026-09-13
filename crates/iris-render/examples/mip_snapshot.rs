//! 工具：mip 链 + 各向异性过滤视觉验收（R66，离屏渲染）。与
//! `tests/mip_render.rs` 同一俯视棋盘场景（512² / 4 纹素格、重度缩小
//! 缩小采样），左半 = `MipSettings` 缺席（单 mip + Nearest mip 过滤，
//! 高频噪点），右半 = `MipSettings` 在场（CPU box 链 + 三线性 + 各向
//! 异性 8，噪点均灰）。
//!
//! 输出 `mip_off.bmp` / `mip_on.bmp`。转 PNG（两帧横向拼接）：
//! ```sh
//! ffmpeg -y -i mip_off.bmp -i mip_on.bmp -filter_complex hstack docs/mip.png
//! ```
//!
//! ```sh
//! cargo run -p iris-render --example mip_snapshot --release
//! ```

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

const SIZE: u32 = 512;

fn checker_world(mip: bool) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 39.8, 3.0),
        target: Vec3::ZERO,
        fov_y_radians: std::f32::consts::FRAC_PI_4,
        z_near: 0.1,
        z_far: 200.0,
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        intensity: 0.0,
        ambient: Vec3::splat(1.0),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    if mip {
        world.insert_resource(MipSettings::default());
    }
    let tex = world
        .resource_mut::<TextureStore>()
        .add(CpuTexture::checkerboard(
            128,
            4,
            [0, 0, 0, 128],
            [255, 255, 255, 128],
        ));
    let mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
        texture: tex,
        ..MaterialDef::default()
    });
    let ground = world.resource_mut::<MeshStore>().add(MeshData::plane(4.0));
    world.spawn((Transform::IDENTITY, ground, mat));
    propagate_transforms(&mut world);
    world
}

fn main() {
    // 关 / 开各用独立渲染器：GPU 纹理按句柄值缓存，同一渲染器先渲染
    // 缺席世界会把无 mip 纹理留在缓存里，「开」帧复用旧上传（跨世界
    // 对照必须各用独立渲染器——既有教训）
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("无 GPU，无法离屏渲染");
        std::process::exit(1);
    };
    let off = checker_world(false);
    renderer.render(&off);
    renderer.save_bmp("mip_off.bmp").expect("写 mip 关帧");
    let Some(mut renderer_on) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("无 GPU，无法离屏渲染");
        std::process::exit(1);
    };
    let on = checker_world(true);
    renderer_on.render(&on);
    renderer_on.save_bmp("mip_on.bmp").expect("写 mip 开帧");
    println!("已输出 mip_off.bmp / mip_on.bmp");
    println!("转 PNG：ffmpeg -y -i mip_off.bmp -i mip_on.bmp -filter_complex hstack docs/mip.png");
}
