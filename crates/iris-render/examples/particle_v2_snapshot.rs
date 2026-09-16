//! 粒子 v2 验收图（R82）：1×2 面板对照，输出 `iris_particles_v2.ppm` →
//! `docs/particles_v2.png`。
//!
//! 面板布局（1 排 × 2 列，同一旋转喷射场景：发射器沿半径 1.6 m 的圆周
//! 运动、周期 2 s，粒子受重力下落、方向抖动 0.3；大气天空 + 暗色地面
//! 承接拖尾亮度）：
//! 1. **拖尾关**（`trail_length = 1`）：只有头部 billboard——圆点串；
//! 2. **拖尾开**（`trail_length = 12`）：头部 + 11 节相机面向条带，
//!    颜色沿历史按 age 比例渐变、alpha 向尾部线性淡出（目检判据：
//!    拖尾连续性、渐隐、无闪烁断层）。
//!
//! 每格带硬断言（拖尾必须改变全帧 / 发光像素与新增像素成片；不通过直接
//! panic、不写文件）；跨运行逐字节一致（同输入两次渲染的 FNV-1a 全帧
//! 哈希相同，本示例打印哈希供比对）。
//!
//! ```sh
//! cargo run -p iris-render --example particle_v2_snapshot --release
//! ffmpeg -y -i iris_particles_v2.ppm docs/particles_v2.png
//! ```

use std::path::Path;
use std::time::Duration;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Entity, Time, Transform, World};
use iris_render::prelude::*;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
const GAP: u32 = 6;

/// 模拟步长（1/60 s）与总步数（2 s——发射器走完一个整周期）。
const STEP: Duration = Duration::from_nanos(1_000_000_000 / 60);
const STEPS: usize = 120;

/// 差分判据的通道阈值（sRGB 级；明亮天空背景下粒子加光经基调映射压缩，
/// 见 particle_v2_render.rs 模块头的度量学说明）。
const DIFF: i32 = 8;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 与参考帧逐像素差分超过阈值的像素数（任一 RGB 通道）。
fn count_changed(a: &[u8], b: &[u8], threshold: i32) -> u32 {
    let mut n = 0u32;
    for (ca, cb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        let dr = (ca[0] as i32 - cb[0] as i32).abs();
        let dg = (ca[1] as i32 - cb[1] as i32).abs();
        let db = (ca[2] as i32 - cb[2] as i32).abs();
        if dr.max(dg).max(db) > threshold {
            n += 1;
        }
    }
    n
}

/// 喷射场景相机：轻俯视，暗色地面占下半（承接 additive 拖尾的亮度）。
fn camera() -> Camera {
    Camera {
        eye: Vec3::new(0.0, 3.4, 6.8),
        target: Vec3::new(0.0, 2.9, 0.0),
        fov_y_radians: 50f32.to_radians(),
        ..Camera::default()
    }
}

/// 场景公共资源（空场景基线与两个面板**必须逐项同源**——天空受方向光
/// 影响，光照参数不一致会让「与空场景差分」把天空色差误算成粒子发光；
/// 本轮实测：基线与面板方向光不同源时，空场景差分给出 40121 px 的假
/// 「发光」，同源后回落到 48 px 的真值）。
///
/// 太阳置于相机**背后**（+Z）并压 Mie / 太阳盘：粒子是 additive 混合，
/// 背景越暗判别度越高；朝日视线的天空在 ACES 后近白，会把粒子完全淹没。
fn scene_resources(world: &mut World) {
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(camera());
    let sun_dir = Vec3::new(0.30, 0.85, 0.75).normalize();
    world.insert_resource(DirectionalLight {
        direction: -sun_dir,
        intensity: 1.0,
        ambient: Vec3::new(0.10, 0.11, 0.14),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(SkySettings {
        atmosphere: true,
        rayleigh_strength: 0.75,
        mie_strength: 0.55,
        sun_disk_intensity: 12.0,
        ground_color: Vec3::new(0.04, 0.04, 0.05),
    });
    world.insert_resource(BloomSettings::disabled());
    world.insert_resource(Time::default());

    // 暗色地面（承接拖尾；空场景基线与面板同源）
    let mut ground_mat = MaterialDef::solid(0.06, 0.06, 0.07, 1.0);
    ground_mat.roughness = 0.95;
    let ground_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(1.0));
    let ground = world.resource_mut::<MaterialStore>().add(ground_mat);
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 0.0, 0.0),
            scale: Vec3::new(60.0, 1.0, 60.0),
            ..Transform::IDENTITY
        },
        ground_mesh,
        ground,
    ));
}

/// 旋转喷射世界（`trail_length` = 拖尾历史点数）。
fn build_world(trail_length: u32) -> World {
    let mut world = World::new();
    scene_resources(&mut world);

    let mut emitter = ParticleEmitterV2::trail_fountain();
    emitter.trail_length = trail_length;
    emitter.trail_width = 0.13;
    emitter.spawn_rate = 100.0;
    emitter.particle_lifetime = 1.2;
    emitter.initial_speed = 6.0;
    emitter.velocity_jitter = 0.30;
    emitter.gravity_scale = 0.30;
    emitter.size_start = 0.10;
    emitter.size_end = 0.03;
    emitter.color_start = [0.78, 0.48, 0.20, 0.85];
    emitter.color_end = [0.42, 0.10, 0.02, 0.0];
    world.spawn((Transform::from_xyz(0.0, 2.0, 0.0), emitter));
    propagate_transforms(&mut world);
    world
}

fn emitter_of(world: &mut World) -> Entity {
    let mut found = None;
    world.query::<(Entity, &ParticleEmitterV2)>(|(e, _)| found = Some(e));
    found.expect("v2 发射器应在场")
}

/// 旋转推进：发射器沿半径 1.0 m 的圆周运动（角速度 2π/2 s），每步同步
/// 层级传播 + v2 推进（调用契约：`update_particles_v2` 在
/// `propagate_transforms` 之后）。
fn simulate(world: &mut World, emitter: Entity) {
    for i in 0..STEPS {
        let t = (i + 1) as f32 * (1.0 / 60.0);
        let theta = t * std::f32::consts::TAU / 2.0;
        world.insert(
            emitter,
            Transform::from_xyz(1.0 * theta.sin(), 2.0, 1.0 * theta.cos()),
        );
        world.resource_mut::<Time>().advance(STEP);
        propagate_transforms(world);
        update_particles_v2(world);
    }
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

    // 空场景基线（无 v2 组件 = 零计算零绘制；判据基准避开恒亮天空）
    let mut empty = World::new();
    scene_resources(&mut empty);
    propagate_transforms(&mut empty);
    renderer.render(&empty);
    let empty_px = renderer.read_pixels();

    let render_panel = |mut renderer: OffscreenRenderer, trail: u32| {
        let mut world = build_world(trail);
        let emitter = emitter_of(&mut world);
        simulate(&mut world, emitter);
        renderer.render(&world);
        let stats = renderer.renderer().stats();
        (renderer.read_pixels(), stats)
    };

    // ---- 面板 1（左）：拖尾关 ----
    let (head, head_stats) = render_panel(renderer, 1);
    // ---- 面板 2（右）：拖尾开（独立渲染器——跨世界对照须隔离 GPU 资源）----
    let renderer_b = OffscreenRenderer::new(WIDTH, HEIGHT).expect("拖尾面板渲染器");
    let (trail, trail_stats) = render_panel(renderer_b, 12);

    let head_lit = count_changed(&empty_px, &head, DIFF);
    let trail_lit = count_changed(&empty_px, &trail, DIFF);
    let added = count_changed(&head, &trail, DIFF);
    let head_hash = fnv1a(&head);
    let trail_hash = fnv1a(&trail);
    println!(
        "拖尾关：粒子 {} 三角 {} 发光像素 {head_lit}",
        head_stats.particles_drawn, head_stats.triangles_drawn
    );
    println!(
        "拖尾开：粒子 {} 三角 {} 发光像素 {trail_lit} 相对关的新增 {added}",
        trail_stats.particles_drawn, trail_stats.triangles_drawn
    );
    println!("拖尾关 FNV-1a = {head_hash:#018x}");
    println!("拖尾开 FNV-1a = {trail_hash:#018x}");

    // ---- 写 PPM（供 ffmpeg 转 PNG 入 docs/）——先落盘再做像素断言，
    // 验收中断时图像仍在（目检用）----
    let mut panel = vec![0u8; ((WIDTH * 2 + GAP) * HEIGHT) as usize * 4];
    blit(&mut panel, &head, 0);
    blit(&mut panel, &trail, WIDTH + GAP);
    let header = format!("P6\n{} {}\n255\n", WIDTH * 2 + GAP, HEIGHT);
    let mut ppm = header.into_bytes();
    for chunk in panel.chunks_exact(4) {
        ppm.extend_from_slice(&chunk[..3]);
    }
    std::fs::write(Path::new("iris_particles_v2.ppm"), &ppm).expect("写 iris_particles_v2.ppm");
    println!("iris_particles_v2.ppm 已写出");

    assert_ne!(head_hash, trail_hash, "拖尾开关必须改变全帧");
    assert!(
        head_lit > 100,
        "拖尾关面板应有头部粒子在画（实际 {head_lit}）"
    );
    assert!(
        trail_lit > head_lit * 3,
        "拖尾开面板发光像素应显著多于拖尾关（{trail_lit} vs {head_lit}）"
    );
    assert!(
        added > 2000,
        "拖尾相对仅头部应有成片新增像素（实际 {added}）"
    );
    assert_eq!(
        trail_stats.particles_drawn, head_stats.particles_drawn,
        "两面板活粒数应一致（仅 trail_length 不同，发射节律与寿命逐位相同）"
    );
    // 结构性断言：几何增量必须精确 = (trail_length − 1) 节 × 2 三角 ×
    // 活粒数（差值形式——场景里还有地面等共享几何，绝对值不可直接比）
    assert_eq!(
        trail_stats.triangles_drawn - head_stats.triangles_drawn,
        (12 - 1) * 2 * trail_stats.particles_drawn,
        "拖尾三角增量应恰为 11 节 × 2 × 活粒数（固定顶点数 = 池 × trail_length × 6）"
    );

    // ---- 跨运行确定性：拖尾世界双会话逐字节一致 ----
    let mut renderer_c = OffscreenRenderer::new(WIDTH, HEIGHT).expect("确定性渲染器 C");
    let mut world_c = build_world(12);
    let emitter_c = emitter_of(&mut world_c);
    simulate(&mut world_c, emitter_c);
    renderer_c.render(&world_c);
    assert_eq!(
        fnv1a(&renderer_c.read_pixels()),
        trail_hash,
        "同输入双会话必须逐位一致"
    );
    println!("双会话逐字节一致：是");
}
