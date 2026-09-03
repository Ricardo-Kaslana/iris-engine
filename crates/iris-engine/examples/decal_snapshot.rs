//! 工具：盒投影贴花（R39）视觉验收场景（离屏渲染输出 `docs/decal.png`）。
//!
//! 灰墙 + 地面场景：墙面 3 个弹孔贴花（程序化深色同心圆纹理）+ 地面一片
//! 喷溅血迹（程序化不规则红斑）+ 斜面（坡道）上一枚任意朝向贴花（旋转盒
//! 证明投影盒随实体旋转）。目检要点：贴花贴面不悬浮不穿透、边缘无溢出
//! 光晕（端盖代理几何 + 盒裁剪）、斜面朝向正确。
//!
//! ```sh
//! cargo run --example decal_snapshot --release
//! ```
//!
//! 产物：`docs/decal.png`（640×360，从仓库根目录运行）。

use glam::{Quat, Vec3};
use iris_core::hierarchy::propagate_transforms;
use iris_engine::prelude::*;

/// 程序化弹孔纹理：深色同心圆（核心近黑不透明 → 环带渐隐 → 透明角）。
/// alpha 通道控制覆盖，无硬边（配合 ClampToEdge 无边缘光晕）。
fn bullet_hole_texture() -> CpuTexture {
    let size = 64u32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let dx = (x as f32 + 0.5) / size as f32 - 0.5;
            let dy = (y as f32 + 0.5) / size as f32 - 0.5;
            let d = (dx * dx + dy * dy).sqrt() * 2.0; // 0 = 中心, 1 = 内切圆缘
            let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            if d < 0.28 {
                // 核心：近黑弹坑
                r = 0.04;
                g = 0.04;
                b = 0.05;
                a = 1.0;
            } else if d < 0.5 {
                // 环带：深灰碎屑，alpha 线性渐隐
                let t = (d - 0.28) / (0.5 - 0.28);
                r = 0.10;
                g = 0.10;
                b = 0.11;
                a = 1.0 - t;
            }
            rgba.extend_from_slice(&[
                (r * 255.0) as u8,
                (g * 255.0) as u8,
                (b * 255.0) as u8,
                (a * 255.0) as u8,
            ]);
        }
    }
    CpuTexture {
        width: size,
        height: size,
        rgba,
    }
}

/// 程序化血迹纹理：不规则红斑（中心圆 + 黄金角分布的卫星圆斑，
/// smoothstep 软边），深红色随覆盖深度略变亮。
fn blood_splat_texture() -> CpuTexture {
    let size = 64u32;
    // 卫星斑：(方向角, 距中心, 半径)——紧贴主斑，连成一片喷溅
    let blobs: [(f32, f32, f32); 6] = [
        (0.0, 0.16, 0.20),
        (2.4, 0.15, 0.18),
        (4.8, 0.17, 0.19),
        (1.2, 0.24, 0.14),
        (3.6, 0.23, 0.15),
        (5.9, 0.14, 0.16),
    ];
    let smooth = |t: f32| {
        let t = t.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let dx = (x as f32 + 0.5) / size as f32 - 0.5;
            let dy = (y as f32 + 0.5) / size as f32 - 0.5;
            // 覆盖掩码 = 各圆斑「1 − 归一化距离」的最大值（软边）
            let mut mask = 1.0 - (dx * dx + dy * dy).sqrt() * 2.0 / 0.52;
            for (angle, dist, radius) in blobs {
                let (bx, by) = (angle.cos() * dist, angle.sin() * dist);
                let d = ((dx - bx).powi(2) + (dy - by).powi(2)).sqrt();
                mask = mask.max(1.0 - d / radius);
            }
            let m = smooth(mask).clamp(0.0, 1.0);
            // 深红：核心更深、边缘略亮（伪体积感）
            let r = 0.28 + 0.14 * (1.0 - m);
            let g = 0.02 + 0.02 * (1.0 - m);
            let b = 0.03 + 0.03 * (1.0 - m);
            rgba.extend_from_slice(&[
                (r * 255.0) as u8,
                (g * 255.0) as u8,
                (b * 255.0) as u8,
                (m * 255.0) as u8,
            ]);
        }
    }
    CpuTexture {
        width: size,
        height: size,
        rgba,
    }
}

/// 挂一枚贴花：`rotation` 把局部 +Z（投影方向）指到表面法线，位置沿
/// 法线抬高一半盒厚 + 毫米级裕量（端盖与表面共面，见 decal.rs 文档）。
fn spawn_decal(
    world: &mut World,
    texture: TextureHandle,
    size: Vec3,
    tint: [f32; 4],
    center: Vec3,
    rotation: Quat,
) {
    let normal = rotation * Vec3::Z;
    world.spawn((
        Transform {
            position: center + normal * (size.z * 0.5 + 0.002),
            rotation,
            scale: Vec3::ONE,
        },
        Decal {
            texture,
            size,
            tint,
        },
    ));
}

fn main() {
    env_logger::init();
    let Some(mut renderer) = OffscreenRenderer::new(640, 360) else {
        eprintln!("无 GPU");
        std::process::exit(1);
    };

    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    // 中低机位平视墙面：弹孔、血迹与斜面贴花同框
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 2.0, 6.0),
        target: Vec3::new(0.0, 1.1, -2.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        intensity: 1.1,
        ..DirectionalLight::default()
    });
    // 贴花验收关注边缘干净，关闭泛光避免高光光晕混淆目检
    world.insert_resource(BloomSettings::disabled());

    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());

    // 灰墙（薄盒正面 z = -3.8）与地面
    let wall_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.55, 0.55, 0.58, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 2.0, -4.0),
            scale: Vec3::new(16.0, 4.0, 0.4),
            ..Transform::IDENTITY
        },
        cube,
        wall_mat,
    ));
    let floor_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.42, 0.44, 0.46, 1.0));
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(60.0));
    world.spawn((Transform::IDENTITY, plane, floor_mat));

    let hole_tex = world
        .resource_mut::<TextureStore>()
        .add(bullet_hole_texture());
    // 墙面 3 个弹孔：局部 +Z = 世界 +Z（墙面法线），间距错落
    for (x, y, s) in [(-1.6f32, 1.6f32, 1.0f32), (0.2, 1.1, 0.75), (1.8, 1.8, 0.9)] {
        spawn_decal(
            &mut world,
            hole_tex,
            Vec3::new(s, s, 0.08),
            [1.0; 4],
            Vec3::new(x, y, -3.8),
            Quat::IDENTITY,
        );
    }

    // 地面血迹：局部 +Z → 世界 +Y
    let blood_tex = world
        .resource_mut::<TextureStore>()
        .add(blood_splat_texture());
    spawn_decal(
        &mut world,
        blood_tex,
        Vec3::new(2.2, 1.8, 0.08),
        [1.0; 4],
        Vec3::new(0.7, 0.0, 1.4),
        Quat::from_rotation_arc(Vec3::Z, Vec3::Y),
    );

    // 斜面（坡道）+ 任意朝向贴花：坡道绕 X 轴倾斜 +28°（顶面朝向相机），
    // 顶面法线随之偏转；贴花盒 Z 轴对齐该法线（旋转盒证明投影随实体朝向）
    let tilt = 28.0f32.to_radians();
    let ramp_rotation = Quat::from_rotation_x(tilt);
    let ramp_center = Vec3::new(-3.6, 0.5, 0.2);
    world.spawn((
        Transform {
            position: ramp_center,
            rotation: ramp_rotation,
            scale: Vec3::new(2.4, 0.3, 2.2),
        },
        cube,
        wall_mat,
    ));
    let ramp_normal = ramp_rotation * Vec3::Y;
    let top_center = ramp_center + ramp_normal * 0.15; // 半厚 0.15 = 顶面中心
    spawn_decal(
        &mut world,
        hole_tex,
        Vec3::new(1.3, 1.1, 0.08),
        [0.85, 0.9, 1.0, 1.0],
        top_center,
        Quat::from_rotation_arc(Vec3::Z, ramp_normal),
    );

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
        .save("docs/decal.png")
        .expect("写 docs/decal.png");
    println!("已输出 docs/decal.png（{w}x{h}）");
}
