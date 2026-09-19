//! R93 · 海洋 v2 验收图：三面板（谱细节海面 | 细节关闭对照 | 浮力平衡吃水），
//! 输出 `iris_ocean.ppm` → `docs/ocean.png`。
//!
//! 左面板：开阔海面（`OceanSurface` 默认档——涌浪 0.5 m + 谱细节 0.06 m），
//! 相机 6 m 高俯视，画面下半从近处细节过渡到掠射远海（far 环参与）；
//! 中面板：**同场景同钟，`detail_amplitude = 0`**——左右并置即细节层的
//! 肉眼差分（中面板 = 只有 Gerstner 涌浪的 R81 型波形，海洋档参数）；
//! 右面板：同海面 + 三只不同密度的箱子（0.4ρ / 0.9ρ / 1.6ρ）+ 一只中性
//! 浮力球，`BuoyancyPlane` 开启、`step_physics` 跑 240 步（4 s）后的**平衡
//! 吃水**——半密箱吃水约四成、近中性箱几乎齐面、重箱沉到底板。这是浮力
//! 端到端（物理 ↔ 水面高度）唯一肉眼可验的形态。
//!
//! 硬断言：三面板各自双会话逐位一致（确定性文化）+ 左 ≠ 中（细节层到达
//! 像素）+ 右 ≠ 左（浮力箱入画）+ 吃水顺序与密度顺序一致（浮力方向与
//! 幅值都反了都过不了这条）。
//!
//! ```sh
//! cargo run -p iris-render --example ocean_snapshot --release
//! ffmpeg -y -i iris_ocean.ppm docs/ocean.png
//! ```

use std::time::Duration;

use glam::Vec3;
use iris_core::buoyancy::{shape_volume, submerged_volume};
use iris_core::hierarchy::propagate_transforms;
use iris_core::physics::{Collider, ColliderShape, RigidBody, step_physics};
use iris_core::prelude::{BuoyancyPlane, Time, Transform, World};
use iris_render::prelude::*;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 320;
const GAP: u32 = 6;
const STEP: Duration = Duration::from_nanos(1_000_000_000 / 60);
/// 右面板的物理推进步数（8 s @ 60 Hz——足够浮体收敛到平衡吃水）。
const PHYS_STEPS: usize = 480;
const RHO_WATER: f32 = 1000.0;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 机位：6 m 高、俯角约 15°——近处看细节、远处看海平线。
fn camera() -> Camera {
    Camera {
        eye: Vec3::new(0.0, 6.0, 14.0),
        target: Vec3::new(0.0, -1.0, -60.0),
        fov_y_radians: 60f32.to_radians(),
        ..Camera::default()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    /// 谱细节海面。
    Detail,
    /// 细节关闭（对照）。
    Flat,
    /// 浮力平衡吃水。
    Float,
}

/// 海面 + 暗色海床（y = −4，给深度吸收一个近岸底；远处仍读远平面 = 深水色）。
fn sea_world(panel: Panel, ocean: OceanSurface) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(camera());
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.4, -1.0, -0.35).normalize(),
        color: Vec3::new(1.0, 0.97, 0.92),
        intensity: 3.2,
        ambient: Vec3::new(0.12, 0.14, 0.17),
        shadows: true,
        shadow_range: 120.0,
    });
    world.insert_resource(BloomSettings::disabled());
    // 海面薄雾（R79 通道）：把 far 环的边缘接缝融进大气——「构图上让边缘
    // 出画或被雾遮住」是 §8.50⑧ 的既有口径，海洋侧同样适用。
    world.insert_resource(FogSettings {
        enabled: true,
        density: 0.0022,
        height_falloff: 0.03,
        height_offset: 0.0,
        inscatter_strength: 0.9,
        phase_g: 0.3,
        max_distance: 900.0,
    });
    let seabed_mesh = world
        .resource_mut::<MeshStore>()
        .add(MeshData::plane(600.0));
    let seabed_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.055, 0.05, 0.045, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, -4.0, -40.0),
            ..Transform::IDENTITY
        },
        seabed_mesh,
        seabed_mat,
    ));

    let ocean = match panel {
        Panel::Flat => OceanSurface {
            detail_amplitude: 0.0,
            ..ocean
        },
        _ => ocean,
    };
    world.spawn((ocean,));

    if panel == Panel::Float {
        // 三只箱（边长 2 m，体积 8 m³）+ 一只球：密度比决定平衡吃水。
        let box_mat = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.62, 0.34, 0.12, 1.0));
        let white_mat = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.72, 0.72, 0.7, 1.0));
        let cube_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
        let ball_mesh = world
            .resource_mut::<MeshStore>()
            .add(MeshData::uv_sphere(0.9, 24, 16));
        let volume = 8.0f32;
        for (i, ratio) in [0.4f32, 0.9, 1.6].iter().enumerate() {
            world.spawn((
                Transform {
                    position: Vec3::new(-4.5 + i as f32 * 4.5, 3.0, -14.0),
                    ..Transform::IDENTITY
                },
                Collider {
                    shape: ColliderShape::Box {
                        half_extents: Vec3::splat(1.0),
                    },
                    is_static: false,
                    restitution: 0.0,
                    is_sensor: false,
                    groups: u32::MAX,
                    mask: u32::MAX,
                },
                RigidBody {
                    mass: ratio * RHO_WATER * volume,
                    linear_damping: 0.05,
                    ..RigidBody::default()
                },
                cube_mesh,
                box_mat,
            ));
        }
        // 中性浮力球（m = ρ_f·V）：全浸随遇平衡，停在入水深度附近。
        let ball_volume = 4.0 / 3.0 * std::f32::consts::PI * 0.9f32.powi(3);
        world.spawn((
            Transform {
                position: Vec3::new(7.0, -0.6, -14.0),
                ..Transform::IDENTITY
            },
            Collider::dynamic_sphere(0.9, 0.0),
            RigidBody {
                mass: RHO_WATER * ball_volume,
                linear_damping: 0.05,
                ..RigidBody::default()
            },
            ball_mesh,
            white_mat,
        ));
        // 浮力平面（与海面同高；follow_water 关——这里显式给值，
        // 证明两路不冲突）。
        world.insert_resource(BuoyancyPlane {
            height: ocean.height,
            fluid_density: RHO_WATER,
            linear_drag: 2.5,
            follow_water: false,
        });
        // Time 必须**在世界里**推进：step_physics 的 dt 取自该资源，
        // 缺席时按 0 处理 ⇒ 浮体一动不动（首稿把 time 留在局部变量里，
        // 面板 4 只箱全冻在出生高度，被「吃水 = 0」的断言当场抓住）。
        world.insert_resource(Time::default());
        for _ in 0..PHYS_STEPS {
            world.resource_mut::<Time>().advance(STEP);
            step_physics(&world);
        }
        propagate_transforms(&mut world);
    } else {
        let mut time = Time::default();
        for _ in 0..90 {
            time.advance(STEP);
        }
        world.insert_resource(time);
    }
    propagate_transforms(&mut world);
    world
}

/// 渲染一个面板（两次独立会话 ⇒ 帧数一致，双跑逐位比对在 main 里做）。
fn render_panel(world: &World) -> Vec<u8> {
    let mut renderer = OffscreenRenderer::new(WIDTH, HEIGHT).expect("环境应有 GPU 适配器");
    for _ in 0..3 {
        renderer.render(world);
    }
    renderer.read_pixels()
}

/// 面板内的水平扫描线（画面下 1/4 = 近场海面主导区）。
fn row_luma(px: &[u8], y: u32) -> f64 {
    (0..WIDTH)
        .map(|x| {
            let i = ((y * WIDTH + x) * 4) as usize;
            (px[i] as f64 + px[i + 1] as f64 + px[i + 2] as f64) / 3.0
        })
        .sum::<f64>()
        / WIDTH as f64
}

/// 差分像素数（任一通道差 ≥ 4/255）。
fn count_changed(a: &[u8], b: &[u8]) -> usize {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .filter(|(x, y)| (0..3).any(|i| (x[i] as i32 - y[i] as i32).abs() >= 4))
        .count()
}

/// 浮体的物理量：`(期望密度比, 实测浸没比, 世界 y)`，按期望密度比升序。
///
/// 口径是**阿基米德判据本身**：平衡 ⇔ `V_sub / V = ρ_b / ρ_f`。不再按
/// 「第几只箱」定位（首稿按质量排序取前三只，把中性球当成了 0.4ρ 箱——
/// 球体积 3.05 m³、质量 3054 kg 恰好在最轻那只箱之下，排序即错位）。
fn floaters(world: &World) -> Vec<(f32, f32, f32)> {
    let mut v = Vec::new();
    world.query::<(&Transform, &Collider, &RigidBody)>(|(t, col, body)| {
        let volume = shape_volume(&col.shape);
        if volume <= 0.0 || body.mass <= 0.0 {
            return;
        }
        let expected = body.mass / (RHO_WATER * volume);
        let sub = submerged_volume(&col.shape, t.position, t.rotation, 0.0);
        v.push((expected, (sub / volume).clamp(0.0, 1.0), t.position.y));
    });
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    v
}

fn main() {
    let Some(probe) = OffscreenRenderer::new(8, 8) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    drop(probe);

    let ocean = OceanSurface::default();
    let detail_world = sea_world(Panel::Detail, ocean);
    let flat_world = sea_world(Panel::Flat, ocean);
    let float_world = sea_world(Panel::Float, ocean);

    // 浮力平衡吃水的**数值**断言（阿基米德判据：V_sub/V == ρ_b/ρ_f）：
    // 三只箱密度比 0.4 / 0.9 / 1.6，中性球恰为 1.0。前三者应停在自己的
    // 平衡吃水上（收敛容差 0.08），超重的那只沉到海床（y < −2.5）。
    let bodies = floaters(&float_world);
    assert_eq!(bodies.len(), 4, "三只箱 + 一只球（海床无刚体）");
    for (i, &(expected, measured, y)) in bodies.iter().enumerate() {
        if expected > 1.0 {
            assert!(
                y < -2.5,
                "第 {i} 只（ρ_b/ρ_f = {expected:.2}）超重应沉底，实得 y = {y}"
            );
            assert!(measured > 0.99, "沉底箱应全浸，实测浸没比 {measured}");
            continue;
        }
        assert!(
            (expected - measured).abs() < 0.08,
            "第 {i} 只浮体平衡吃水应满足 V_sub/V = ρ_b/ρ_f = {expected:.3}，             实得 {measured:.3}（y = {y}）"
        );
    }
    // 单调性旁证：吃水随密度比加深（中心 y 严格下降）。
    for w in bodies.windows(2) {
        assert!(
            w[0].2 > w[1].2 - 1e-3,
            "密度比升序 ⇒ 中心 y 应单调不升：{} vs {}",
            w[0].2,
            w[1].2
        );
    }

    let detail = render_panel(&detail_world);
    let flat = render_panel(&flat_world);
    let float = render_panel(&float_world);
    let (hd, hf, ho) = (fnv1a(&detail), fnv1a(&flat), fnv1a(&float));
    assert_eq!(
        hd,
        fnv1a(&render_panel(&detail_world)),
        "细节面板双会话逐位一致"
    );
    assert_eq!(
        hf,
        fnv1a(&render_panel(&flat_world)),
        "对照面板双会话逐位一致"
    );
    assert_eq!(
        ho,
        fnv1a(&render_panel(&float_world)),
        "浮力面板双会话逐位一致"
    );
    assert_ne!(hd, hf, "细节层必须到达像素（detail_amplitude 是活旋钮）");
    assert_ne!(ho, hd, "浮力箱必须入画");
    // 细节的空间判别力：近处扫描线在开 / 关两面板间的差分计数。
    let near_changed = count_changed(
        &detail[(HEIGHT as usize * 3 / 4 * WIDTH as usize * 4)..],
        &flat[(HEIGHT as usize * 3 / 4 * WIDTH as usize * 4)..],
    );
    assert!(
        near_changed > 2000,
        "画面下 1/4（近场环）应有大量像素因细节层而不同，实得 {near_changed}"
    );
    let luma_d = row_luma(&detail, HEIGHT * 7 / 8);
    let luma_f = row_luma(&flat, HEIGHT * 7 / 8);
    println!("近处扫描线平均亮度：细节开 {luma_d:.1} vs 关 {luma_f:.1}（镜面碎点抬亮）");

    // 三面板合成（P6 RGB，白缝 + 顶边，与 precipitation 同款口径）
    let panels = [&detail, &flat, &float];
    let mut ppm = format!("P6\n{} {}\n255\n", WIDTH * 3 + GAP * 2, HEIGHT + GAP).into_bytes();
    let gap_row = vec![255u8; (WIDTH as usize * 3 + GAP as usize * 2) * 3];
    for _ in 0..GAP {
        ppm.extend_from_slice(&gap_row);
    }
    for y in 0..HEIGHT {
        for (pi, panel) in panels.iter().enumerate() {
            if pi > 0 {
                for _ in 0..GAP {
                    ppm.extend_from_slice(&[255, 255, 255]);
                }
            }
            for chunk in panel
                [(y as usize * WIDTH as usize) * 4..((y + 1) as usize * WIDTH as usize) * 4]
                .chunks_exact(4)
            {
                ppm.extend_from_slice(&chunk[..3]);
            }
        }
    }
    std::fs::write("iris_ocean.ppm", &ppm).expect("写 iris_ocean.ppm");
    println!("iris_ocean.ppm 已写出（{} 字节）", ppm.len());
    println!("左 = 谱细节海面      FNV-1a = {hd:#018x}");
    println!("中 = 细节关闭对照    FNV-1a = {hf:#018x}");
    println!("右 = 浮力平衡吃水    FNV-1a = {ho:#018x}");
}
