//! 平面水体验收图（R81）：1×2 面板对照，输出 `iris_water.ppm` →
//! `docs/water.png`。
//!
//! 面板布局（1 排 × 2 列，同一岸边场景：浅滩地面 + 深水空域 + 远岸 +
//! 岩石箱体 + 亮红倒影柱，大气天空）：
//! 1. **水体关**：水面缺席——深水区直见天空清屏色，无倒影；
//! 2. **水体开**：Gerstner 波面 + 镜面反射（红柱倒影）+ Fresnel 天空 +
//!    深度吸收（近岸浅青 → 远处深蓝）+ 太阳镜面高光。
//!
//! 每格带硬断言（开关必须改变全帧 / 倒影采样点红柱色可判别 / 深浅水
//! 色相差可判别；不通过直接 panic、不写文件）；跨运行逐字节一致（同
//! 输入两次渲染的 FNV-1a 全帧哈希相同，本示例打印哈希供比对）。
//!
//! ```sh
//! cargo run -p iris-render --example water_snapshot --release
//! ffmpeg -y -i iris_water.ppm docs/water.png
//! ```

use std::path::Path;
use std::time::Duration;

use glam::{Vec2, Vec3};
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Time, Transform, World};
use iris_render::prelude::*;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
const GAP: u32 = 6;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn pixel(px: &[u8], w: u32, x: u32, y: u32) -> [i32; 3] {
    let i = ((y * w + x) * 4) as usize;
    [px[i] as i32, px[i + 1] as i32, px[i + 2] as i32]
}

/// 岸边场景相机：轻俯视，水面占画面下半、远岸与天空占上半。
fn camera() -> Camera {
    Camera {
        eye: Vec3::new(0.0, 3.2, 17.0),
        target: Vec3::new(0.0, 0.6, -6.0),
        fov_y_radians: 55f32.to_radians(),
        ..Camera::default()
    }
}

/// 岸边世界（海岸斜切构图）：近岸沙滩只铺在画面右下（x ≥ −6、
/// z ∈ [6, 28]），水面（height 0.5，半边长 14×14）在其上形成**前景浅水**
/// ；左侧与中部水下无地面 = **深水空域**（scene_depth = 远平面清空值，
/// 深度吸收的判别处）；远岸台地（y = 1.2，z ≤ −14）上散布岩石与亮红
/// 倒影柱。`water` = 水体开关。
fn build_world(water: bool) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(camera());
    // 白日太阳 + 大气天空（反射捕获与主视口同源生效）
    let sun_dir = Vec3::new(0.45, 1.05, -1.0).normalize();
    world.insert_resource(DirectionalLight {
        direction: -sun_dir,
        intensity: 1.2,
        ambient: Vec3::new(0.16, 0.18, 0.22),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(SkySettings {
        atmosphere: true,
        ..SkySettings::default()
    });
    world.insert_resource(BloomSettings::disabled());
    let mut sand_mat = MaterialDef::solid(0.42, 0.36, 0.26, 1.0);
    sand_mat.roughness = 0.95;
    let mut rock_mat = MaterialDef::solid(0.36, 0.34, 0.32, 1.0);
    rock_mat.roughness = 0.9;
    let mut pillar_mat = MaterialDef::solid(0.85, 0.08, 0.06, 1.0);
    pillar_mat.roughness = 0.5;
    let ground_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(1.0));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let sand = world.resource_mut::<MaterialStore>().add(sand_mat);
    let rock = world.resource_mut::<MaterialStore>().add(rock_mat);
    let pillar = world.resource_mut::<MaterialStore>().add(pillar_mat);
    // 近岸沙滩（右下）：x ∈ [−6, 30]、z ∈ [6, 28]、顶 y = 0
    world.spawn((
        Transform {
            position: Vec3::new(12.0, 0.0, 17.0),
            scale: Vec3::new(36.0, 1.0, 22.0),
            ..Transform::IDENTITY
        },
        ground_mesh,
        sand,
    ));
    // 远岸台地：z ∈ [−38, −14]、顶 y = 1.2（水面之上——可见岸线）
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 1.2, -26.0),
            scale: Vec3::new(80.0, 1.0, 24.0),
            ..Transform::IDENTITY
        },
        ground_mesh,
        sand,
    ));
    // 远岸岩石（反射 / 折射捕获都有内容）
    for (x, z, s) in [
        (-6.0, -17.0, 1.4),
        (5.0, -19.0, 1.1),
        (-1.0, -22.0, 1.8),
        (10.0, -26.0, 2.2),
        (-12.0, -24.0, 1.6),
    ] {
        world.spawn((
            Transform {
                position: Vec3::new(x, 1.2 + s * 0.5, z),
                scale: Vec3::splat(s),
                ..Transform::IDENTITY
            },
            cube,
            rock,
        ));
    }
    // 倒影塔：远岸水边（亮红——面板 2 倒影的判别处；立于远岸台地上，
    // 跨 y 1.2..9.2。塔身 1.5 m 宽：半分辨率反射图上占 ~9 px——波纹
    // 扰动（±5 px）不把采样点推出塔身）
    world.spawn((
        Transform {
            position: Vec3::new(3.0, 5.2, -16.0),
            scale: Vec3::new(1.5, 8.0, 1.5),
            ..Transform::IDENTITY
        },
        cube,
        pillar,
    ));
    if water {
        world.spawn((WaterSurface {
            height: 0.5,
            // 半边长 Z = 20：近边缘（z = 20）在相机（z = 17）身后——波动的
            // 网格边缘不入画（边缘露缝是 v1 限制，见 §8.50；构图上让
            // 近边缘出画即可回避）
            half_extent: Vec2::new(14.0, 20.0),
            ..WaterSurface::default()
        },));
    }
    propagate_transforms(&mut world);
    // 波形时间：2 × 50 ms 推进（双会话确定可复现）
    let mut time = Time::default();
    for _ in 0..2 {
        time.advance(Duration::from_millis(50));
    }
    world.insert_resource(time);
    world
}

/// 世界坐标 → 像素坐标（与渲染器同一条相机路径）。
fn project_to_pixel(camera: &Camera, p: Vec3, w: u32, h: u32) -> (f32, f32) {
    let aspect = w as f32 / h as f32;
    let view = camera.view_matrix();
    let proj = glam::camera::rh::proj::directx::perspective(
        camera.fov_y_radians,
        aspect,
        camera.z_near,
        camera.z_far,
    );
    let clip = proj * view * p.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    (
        (ndc.x * 0.5 + 0.5) * w as f32,
        (0.5 - ndc.y * 0.5) * h as f32,
    )
}

fn sample(px: &[u8], camera: &Camera, at: Vec3, w: u32, h: u32) -> [i32; 3] {
    let (fx, fy) = project_to_pixel(camera, at, w, h);
    pixel(px, w, fx as u32, fy as u32)
}

/// 把一格画面拷进面板缓冲。
fn blit(panel: &mut [u8], src: &[u8], x0: u32) {
    let stride = (WIDTH * 2 + GAP) as usize;
    for row in 0..HEIGHT as usize {
        let sy = row * WIDTH as usize * 4;
        let dy = (row * stride + x0 as usize) * 4;
        let n = WIDTH as usize * 4;
        panel[dy..dy + n].copy_from_slice(&src[sy..sy + n]);
    }
}

fn main() {
    let Some(mut renderer) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let cam = camera();

    // ---- 面板 1（左）：水体关 ----
    let world_off = build_world(false);
    let off = {
        renderer.render(&world_off);
        renderer.read_pixels()
    };
    // ---- 面板 2（右）：水体开 ----
    let world_on = build_world(true);
    let on = {
        renderer.render(&world_on);
        renderer.read_pixels()
    };

    let mut panel = vec![0u8; ((WIDTH * 2 + GAP) * HEIGHT) as usize * 4];
    blit(&mut panel, &off, 0);
    blit(&mut panel, &on, WIDTH + GAP);

    let off_hash = fnv1a(&off);
    let on_hash = fnv1a(&on);
    println!("水体关 FNV-1a = {off_hash:#018x}");
    println!("水体开 FNV-1a = {on_hash:#018x}");
    assert_ne!(off_hash, on_hash, "水体开关必须改变全帧");

    // ---- 写 PPM（供 ffmpeg 转 PNG 入 docs/）——先落盘再做像素断言，
    // 验收中断时图像仍在（目检用）----
    let header = format!("P6\n{} {}\n255\n", WIDTH * 2 + GAP, HEIGHT);
    let mut ppm = header.into_bytes();
    for chunk in panel.chunks_exact(4) {
        ppm.extend_from_slice(&chunk[..3]);
    }
    std::fs::write(Path::new("iris_water.ppm"), &ppm).expect("写 iris_water.ppm");
    println!("iris_water.ppm 已写出");

    // ---- 倒影判别：塔身两点 P(y) = (3, y, −16)（y = 5.2 / 4）关于水面
    // y=0.5 的镜像 (3, 1−y, −16) 与眼点 (0, 3.2, 17) 的连线在 y=0.5 处的
    // 交点 W = (3t, 0.5, 17−33t)，t = 2.7/(2.2+y)——掠射角（Fresnel
    // ≈ 0.3）下这些水面像素应出现塔的红（镜像相机捕获的精确落点，构造
    // 保证反射线命中塔身；两点取最大值以容忍波形相位的逐点抖动）----
    let mut rg_on_max = i32::MIN;
    let mut rg_off_min = i32::MAX;
    for y in [5.2f32, 4.0] {
        let t = 2.7 / (2.2 + y);
        let refl_point = Vec3::new(3.0 * t, 0.5, 17.0 - 33.0 * t);
        let p_off = sample(&off, &cam, refl_point, WIDTH, HEIGHT);
        let p_on = sample(&on, &cam, refl_point, WIDTH, HEIGHT);
        rg_on_max = rg_on_max.max(p_on[0] - p_on[1]);
        rg_off_min = rg_off_min.min(p_off[0] - p_off[1]);
        println!(
            "倒影点 y={y}：W=({:.2},0.5,{:.2}) 关 {p_off:?} 开 {p_on:?}",
            refl_point.x, refl_point.z
        );
    }
    println!("倒影带：max(R−G)开 = {rg_on_max}，min(R−G)关 = {rg_off_min}");
    assert!(
        rg_on_max >= 25,
        "水面倒影必须出现塔色（红主导）：max(R−G) = {rg_on_max}"
    );
    assert!(
        rg_on_max - rg_off_min >= 25,
        "有水与无水的倒影差必须可判别：Δ(R−G) = {}",
        rg_on_max - rg_off_min
    );

    // ---- 深度渐变判别：深水点 (−9, 0.5, 8)（左带空域，无地面 →
    // 透过率 ≈ 0 → 深水色主导）vs 浅水点 (5, 0.5, 10)（沙滩正上方，
    // 透过率 ≈ 0.8 → 底色可见）——默认配色下深水色是深蓝：深水点的
    // B−R 显著大于浅水点（深度吸收方向可判别）----
    let deep_p = sample(&on, &cam, Vec3::new(-9.0, 0.5, 8.0), WIDTH, HEIGHT);
    let shallow_p = sample(&on, &cam, Vec3::new(5.0, 0.5, 10.0), WIDTH, HEIGHT);
    let br_deep = deep_p[2] - deep_p[0];
    let br_shallow = shallow_p[2] - shallow_p[0];
    println!("深浅水：深 {deep_p:?}（B−R={br_deep}），浅 {shallow_p:?}（B−R={br_shallow}）");
    assert!(
        br_deep >= 40,
        "深水必须趋 deep_color（深蓝）：B−R = {br_deep}"
    );
    assert!(
        br_deep - br_shallow >= 25,
        "深浅水的深度吸收差必须可判别：Δ(B−R) = {}",
        br_deep - br_shallow
    );

    // ---- 跨运行确定性：水体开世界双会话逐字节一致 ----
    let mut renderer_b = OffscreenRenderer::new(WIDTH, HEIGHT).expect("确定性渲染器 B");
    renderer_b.render(&world_on);
    renderer_b.render(&world_on);
    assert_eq!(
        fnv1a(&renderer_b.read_pixels()),
        on_hash,
        "同输入双会话必须逐位一致"
    );
}
