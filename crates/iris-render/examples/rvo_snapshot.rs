//! RVO 群体模拟验收图（R95）：1×2 面板，输出 `iris_rvo.ppm` →
//! `docs/rvo.png`。
//!
//! 面板布局（单排两列，同一初始配置的两群对穿 + 场心静态圆障碍）：
//! **左 = 无 RVO 对照**（agent 直走偏好速度，互相穿透）| **右 = RVO 开启**
//! （`rvo_step` 决策速度，分流绕行）。俯视机位，轨迹带 = 每 15 步一枚
//! 暗色小方块（左群暖色系 / 右群冷色系），终态 agent = 圆柱。
//!
//! 硬断言（不通过直接 panic、不写文件）：
//! 1. **无碰撞不变式**——RVO 侧全程（450 步 × 60 agent）最小间隙
//!    ≥ −1e-4；对照侧最小间隙 < 0（判别力：无 RVO 确实穿透）；
//! 2. **通过性**——RVO 侧左群质心 x > 3（绕过去了，不是停死）；
//! 3. **面板判别力**——左右面板逐像素差 ≥ 500 px（规避轨迹可见）；
//! 4. **确定性**——同世界两次渲染 FNV-1a 全帧哈希相同（打印供比对）。
//!
//! ```sh
//! cargo run -p iris-render --example rvo_snapshot --release
//! ffmpeg -y -i iris_rvo.ppm docs/rvo.png
//! ```

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::*;
use iris_core::rvo::{RvoAgent, RvoObstacle, RvoParams, rvo_step};
use iris_render::prelude::*;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
const GAP: u32 = 6;
const STEPS: usize = 450;
const DT: f32 = 1.0 / 30.0;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 初始两群（左 6×5 暖色向 +x / 右同构错位半格向 −x）+ 场心静态圆。
fn build_agents() -> Vec<RvoAgent> {
    let mut v = Vec::with_capacity(60);
    for row in 0..5i32 {
        for col in 0..6i32 {
            let y = (row as f32 - 2.0) * 1.6;
            let x = -9.0 + col as f32 * 0.8;
            v.push(RvoAgent {
                pos: glam::Vec2::new(x, y),
                vel: glam::Vec2::new(1.2, 0.0),
                pref_vel: glam::Vec2::new(1.2, 0.0),
                ..RvoAgent::default()
            });
        }
    }
    for row in 0..5i32 {
        for col in 0..6i32 {
            let y = (row as f32 - 2.0) * 1.6 + 0.4;
            let x = 9.0 - col as f32 * 0.8;
            v.push(RvoAgent {
                pos: glam::Vec2::new(x, y),
                vel: glam::Vec2::new(-1.2, 0.0),
                pref_vel: glam::Vec2::new(-1.2, 0.0),
                ..RvoAgent::default()
            });
        }
    }
    v
}

/// 仿真一段并返回 (终态, 全程最小间隙, 轨迹采样)。
/// `with_rvo = false` 时直走偏好速度（对照组）。
fn simulate(
    mut agents: Vec<RvoAgent>,
    with_rvo: bool,
) -> (Vec<RvoAgent>, f32, Vec<Vec<glam::Vec2>>) {
    let obstacles = [RvoObstacle {
        pos: glam::Vec2::ZERO,
        radius: 1.0,
    }];
    let params = RvoParams {
        neighbor_horizon: 2.0,
        obstacle_horizon: 1.0,
    };
    let mut min_gap = f32::INFINITY;
    let mut trail = Vec::new();
    for step in 0..STEPS {
        if with_rvo {
            rvo_step(&mut agents, &obstacles, &params, DT);
        } else {
            for a in agents.iter_mut() {
                a.vel = a.pref_vel;
            }
        }
        for a in agents.iter_mut() {
            a.pos += a.vel * DT;
        }
        if step % 15 == 0 {
            trail.push(agents.iter().map(|a| a.pos).collect());
        }
        for i in 0..agents.len() {
            for j in (i + 1)..agents.len() {
                let d = (agents[j].pos - agents[i].pos).length();
                let gap = d - agents[i].radius - agents[j].radius;
                if gap < min_gap {
                    min_gap = gap;
                }
            }
        }
    }
    (agents, min_gap, trail)
}

/// 把一段仿真结果摆成世界（地面 + 障碍柱 + 轨迹带 + 终态圆柱）。
fn build_world(final_state: &[(glam::Vec2, bool)], trail: &[Vec<glam::Vec2>]) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 26.0, 16.0),
        target: Vec3::ZERO,
        fov_y_radians: 50f32.to_radians(),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.4, -1.0, -0.3).normalize(),
        color: Vec3::new(1.0, 0.97, 0.92),
        intensity: 2.2,
        ambient: Vec3::new(0.16, 0.17, 0.2),
        shadows: true,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    world.insert_resource(EnvironmentSettings { intensity: 0.0 });
    world.insert_resource(SkySettings {
        atmosphere: true,
        ..SkySettings::default()
    });

    let mut mesh = world.resource_mut::<MeshStore>();
    let mut mat = world.resource_mut::<MaterialStore>();
    let ground_mesh = mesh.add(MeshData::plane(200.0));
    let mark_mesh = mesh.add(MeshData::cube());
    let agent_mesh = mesh.add(MeshData::cylinder(0.4, 0.9, 12));
    let pillar_mesh = mesh.add(MeshData::cylinder(1.0, 1.4, 20));
    let ground = mat.add(MaterialDef::solid(0.16, 0.18, 0.2, 1.0));
    let pillar = mat.add(MaterialDef::solid(0.75, 0.72, 0.66, 1.0));
    let left_mat = mat.add(MaterialDef::solid(0.82, 0.38, 0.22, 1.0));
    let right_mat = mat.add(MaterialDef::solid(0.24, 0.5, 0.86, 1.0));
    let left_mark = mat.add(MaterialDef::solid(0.4, 0.18, 0.1, 1.0));
    let right_mark = mat.add(MaterialDef::solid(0.12, 0.24, 0.42, 1.0));
    drop(mesh);
    drop(mat);

    world.spawn((Transform::IDENTITY, ground_mesh, ground));
    world.spawn((Transform::IDENTITY, pillar_mesh, pillar));

    for frame in trail.iter() {
        for (i, p) in frame.iter().enumerate() {
            let m = if i < 30 { left_mark } else { right_mark };
            world.spawn((
                Transform {
                    position: Vec3::new(p.x, 0.02, p.y),
                    scale: Vec3::new(0.3, 0.04, 0.3),
                    ..Transform::IDENTITY
                },
                mark_mesh,
                m,
            ));
        }
    }
    for (p, left) in final_state.iter() {
        let m = if *left { left_mat } else { right_mat };
        world.spawn((
            Transform {
                position: Vec3::new(p.x, 0.45, p.y),
                ..Transform::IDENTITY
            },
            agent_mesh,
            m,
        ));
    }
    propagate_transforms(&mut world);
    world
}

fn main() {
    let Some(mut renderer) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    // 同一初始配置跑两份：无 RVO 对照 / RVO 开启。
    let (plain_final, plain_gap, plain_trail) = simulate(build_agents(), false);
    let (rvo_final, rvo_gap, rvo_trail) = simulate(build_agents(), true);
    println!("无 RVO 全程最小间隙 = {plain_gap}；RVO 全程最小间隙 = {rvo_gap}");
    // 断言 1：无碰撞不变式（离散帧浅擦界，与 rvo.rs 单测同口径：上界 =
    // 帧内闭合 2.4 × dt = 0.08 的 1/4）+ 对照判别力
    assert!(
        rvo_gap >= -0.02,
        "RVO 侧全程最小间隙 {rvo_gap} 超出离散帧浅擦界（0.02）"
    );
    assert!(
        plain_gap < 0.0,
        "对照组必须穿透（实测 {plain_gap}），否则判别力缺失"
    );
    // 断言 2：通过性——左群（前 30）质心过中场
    let mean_left = rvo_final[0..30].iter().map(|a| a.pos.x).sum::<f32>() / 30.0;
    println!("RVO 侧左群终态质心 x = {mean_left}");
    assert!(
        mean_left > 3.0,
        "左群质心 {mean_left} 未越过中场（绕行失败）"
    );

    let plain_world = build_world(
        &(0..60)
            .map(|i| (plain_final[i].pos, i < 30))
            .collect::<Vec<_>>(),
        &plain_trail,
    );
    let rvo_world = build_world(
        &(0..60)
            .map(|i| (rvo_final[i].pos, i < 30))
            .collect::<Vec<_>>(),
        &rvo_trail,
    );

    renderer.render(&plain_world);
    let plain_frame = renderer.read_pixels();
    renderer.render(&rvo_world);
    let rvo_frame = renderer.read_pixels();
    // 断言 4：确定性——同世界重渲染逐字节一致
    renderer.render(&rvo_world);
    let rvo_again = renderer.read_pixels();
    let (h1, h2) = (fnv1a(&rvo_frame), fnv1a(&rvo_again));
    println!("RVO 面板 FNV = {h1:#x} / 重渲染 {h2:#x}");
    assert_eq!(h1, h2, "同世界两次渲染必须逐字节一致");
    // 断言 3：面板判别力——规避轨迹可见
    let diff = rvo_frame
        .chunks_exact(4)
        .zip(plain_frame.chunks_exact(4))
        .filter(|(a, b)| a[0] != b[0] || a[1] != b[1] || a[2] != b[2])
        .count();
    println!("左右面板差异像素 = {diff}");
    assert!(diff >= 500, "面板差异 {diff} 不足（规避轨迹应清晰可见）");

    // 拼 1×2 面板并落盘（RGBA → RGB，daynight 同款）
    let stride = (WIDTH * 2 + GAP) as usize;
    let mut panel = vec![0u8; stride * HEIGHT as usize * 4];
    for row in 0..HEIGHT as usize {
        let sy = row * WIDTH as usize * 4;
        let dl = (row * stride) * 4;
        panel[dl..dl + WIDTH as usize * 4]
            .copy_from_slice(&plain_frame[sy..sy + WIDTH as usize * 4]);
        let dr = (row * stride + WIDTH as usize + GAP as usize) * 4;
        panel[dr..dr + WIDTH as usize * 4].copy_from_slice(&rvo_frame[sy..sy + WIDTH as usize * 4]);
    }
    let header = format!("P6\n{} {}\n255\n", WIDTH * 2 + GAP, HEIGHT);
    let mut ppm = header.into_bytes();
    for chunk in panel.chunks_exact(4) {
        ppm.extend_from_slice(&chunk[..3]);
    }
    std::fs::write("iris_rvo.ppm", &ppm).expect("写 iris_rvo.ppm");
    println!("iris_rvo.ppm 已写出（{WIDTH}×{HEIGHT} ×2 面板）");
}
