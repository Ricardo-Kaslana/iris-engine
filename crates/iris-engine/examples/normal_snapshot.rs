//! 工具：法线 / ORM 材质通道（R42）视觉验收场景（离屏渲染输出
//! `docs/normal.png`）。
//!
//! 左右两堵砖墙（同一砖纹法线 + ORM 纹理对，程序化生成）对比：
//! **左墙不挂贴图**（中性纹理路径 = 既有标量材质），**右墙挂法线 +
//! ORM**——侧光角度下目检要点：右墙砖缝的凹凸感（法线扰动随光向
//! 变化的明暗）、缝内 AO 变暗、缝面粗糙度大于磨面（高光形态差异）；
//! 右侧金属圆柱挂划痕 ORM（金属度 1、粗糙度沿环向条纹变化）——目检
//! 环向粗糙度条纹的高光差异。
//!
//! ```sh
//! cargo run --example normal_snapshot --release
//! ```
//!
//! 产物：`docs/normal.png`（960×540，从仓库根目录运行）。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_engine::prelude::*;

/// 金属划痕 ORM 纹理（程序化，纯函数）：R = AO（无遮蔽，恒 1）、
/// G = 粗糙度（环向条纹——圆柱侧面 UV 的 v 轴 = 高度方向，条纹按
/// v 行经 splitmix64 哈希取值，模拟圆周研磨划痕）、B = 金属度 1。
fn scratched_metal_orm(height_px: u32) -> CpuTexture {
    let size = 256;
    let mix64 = |mut z: u64| {
        z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        // 每 `height_px` 像素一条划痕带，带内粗糙度由行哈希决定
        let band = y / height_px;
        let noise = (mix64(band as u64) & 0xff) as f32 / 255.0;
        let rough = 0.05 + 0.85 * noise;
        for _x in 0..size {
            rgba.extend_from_slice(&[
                255,
                (rough * 255.0) as u8,
                255, // 金属度 1
                255,
            ]);
        }
    }
    CpuTexture {
        width: size,
        height: size,
        rgba,
        srgb: false,
    }
}

fn main() {
    env_logger::init();
    let Some(mut renderer) = OffscreenRenderer::new(960, 540) else {
        eprintln!("无 GPU");
        std::process::exit(1);
    };

    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 1.4, 4.2),
        target: Vec3::new(0.0, 1.1, -2.0),
        ..Camera::default()
    });
    // 侧光：从左前方斜照——法线扰动在砖缝处的受光差异最大化
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(0.55, -0.45, 0.7).normalize(),
        intensity: 1.6,
        ambient: Vec3::new(0.18, 0.19, 0.22),
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());

    // 地面
    let floor_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.30, 0.31, 0.33, 1.0));
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(60.0));
    world.spawn((Transform::IDENTITY, plane, floor_mat));

    // 砖纹法线 + ORM 纹理对（程序化，线性数据贴图）
    let (brick_normal, brick_orm) = CpuTexture::brick_normal_orm(6, 3, 512, 0.09, 1.8);
    let mut textures = world.resource_mut::<TextureStore>();
    let normal_tex = textures.add(brick_normal);
    let orm_tex = textures.add(brick_orm);
    drop(textures);

    // 两堵砖墙：同色同几何，只有法线 / ORM 槽位有无之差
    let wall_color = [0.72, 0.52, 0.40, 1.0];
    let mut mats = world.resource_mut::<MaterialStore>();
    let wall_plain = mats.add(MaterialDef {
        base_color: wall_color,
        roughness: 0.7,
        ..Default::default()
    });
    let wall_pbr = mats.add(
        MaterialDef {
            base_color: wall_color,
            roughness: 0.7,
            ..MaterialDef::solid(0.0, 0.0, 0.0, 1.0)
        }
        .with_normal(normal_tex)
        .with_orm(orm_tex),
    );
    // 金属圆柱：划痕 ORM（金属度 1 由 ORM 的 B 通道逐像素给出）
    let scratch_orm = world
        .resource_mut::<TextureStore>()
        .add(scratched_metal_orm(5));
    let cylinder_pbr = mats.add(MaterialDef::metal(0.62, 0.60, 0.58, 1.0).with_orm(scratch_orm));
    drop(mats);

    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    // 左墙（无法线）与右墙（法线 + ORM），几何与颜色完全一致
    world.spawn((
        Transform {
            position: Vec3::new(-1.7, 1.4, -2.0),
            scale: Vec3::new(3.0, 2.8, 0.2),
            ..Transform::IDENTITY
        },
        cube,
        wall_plain,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(1.7, 1.4, -2.0),
            scale: Vec3::new(3.0, 2.8, 0.2),
            ..Transform::IDENTITY
        },
        cube,
        wall_pbr,
    ));
    // 金属圆柱立在右墙前（划痕粗糙度条纹朝向相机）
    let cylinder = world
        .resource_mut::<MeshStore>()
        .add(MeshData::cylinder(0.45, 2.2, 48));
    world.spawn((
        Transform {
            position: Vec3::new(0.6, 1.1, -0.6),
            ..Transform::IDENTITY
        },
        cylinder,
        cylinder_pbr,
    ));

    propagate_transforms(&mut world);
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    println!(
        "FrameStats: draw_calls={}, instances={}, triangles={}, culled={}",
        stats.draw_calls, stats.instances, stats.triangles_drawn, stats.culled_instances
    );
    let pixels = renderer.read_pixels();
    let (w, h) = renderer.size();
    image::RgbaImage::from_raw(w, h, pixels)
        .expect("像素缓冲尺寸匹配")
        .save("docs/normal.png")
        .expect("写 docs/normal.png");
    println!("已输出 docs/normal.png（{w}x{h}）");
}
