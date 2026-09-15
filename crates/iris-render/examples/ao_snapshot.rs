//! GTAO 环境光遮蔽验收图（R77）：三组面板对照，输出 `iris_ao.ppm`
//! → `docs/ao.png`。
//!
//! 面板布局（3 排 × 2 列，每排「关 | 开」左右并排，同一房间场景）：
//! 1. **日光房间 关|开**：直射光 + IBL + 环境常量的常规场景——开启后
//!    墙角、方柱根部、箱体接地处出现柔和接触阴影（屏幕空间遮蔽调制
//!    间接光；直接光不受影响的语义同时可见：向阳地面不变黑）；
//! 2. **强度对照 0.5 | 2.0**：同一视角两档 intensity 指数——低档轻微
//!    压暗、高档明显的角落加深（指数单调性）；
//! 3. **纯间接光 关|开**：直射光强度 0——全部照明都是间接光，AO 的
//!    调制作用最大化呈现（测试套件的有效性口径同源）。
//!
//! 每格带硬断言（角落采样点显著暗于开阔采样点 / 强度单调；不通过直接
//! panic、不写文件）；跨运行逐字节一致（同输入两次渲染的 FNV-1a 全帧
//! 哈希相同，本示例打印哈希供比对）。
//!
//! ```sh
//! cargo run -p iris-render --example ao_snapshot --release
//! ffmpeg -y -i iris_ao.ppm docs/ao.png
//! ```

use std::path::Path;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
const GAP: u32 = 6;
/// 面板行数（每组一行，行内两格）。
const ROWS: u32 = 3;

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

fn luminance(p: [i32; 3]) -> i32 {
    p[0] + p[1] + p[2]
}

/// 房间场景公共参数。
struct AoRoom {
    camera: Camera,
    /// 角落地面采样点（两墙交线附近）。
    corner: Vec3,
    /// 开阔地面采样点（2 m 内无遮蔽物）。
    open: Vec3,
}

fn ao_room() -> AoRoom {
    AoRoom {
        camera: Camera {
            eye: Vec3::new(2.4, 2.6, 2.8),
            target: Vec3::new(-1.2, 0.4, -1.2),
            fov_y_radians: std::f32::consts::FRAC_PI_3,
            ..Camera::default()
        },
        corner: Vec3::new(-2.3, 0.0, -2.3),
        open: Vec3::new(0.8, 0.0, 0.8),
    }
}

/// 房间世界：地面 + 两面墙 + 方柱 + 两只箱体。`sun` = 直射光强度
/// （面板 3 传 0 做纯间接光对照），`ao` = GTAO 开关与参数。
fn build_world(room: &AoRoom, sun: f32, ao: Option<AoSettings>, spin: f32) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(room.camera.clone());
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.45, -1.0, -0.35).normalize(),
        intensity: sun,
        ambient: Vec3::new(0.18, 0.2, 0.24),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    if let Some(ao) = ao {
        world.insert_resource(ao);
    }
    let mut floor_mat = MaterialDef::solid(0.62, 0.6, 0.58, 1.0);
    floor_mat.roughness = 0.85;
    let mut wall_mat = MaterialDef::solid(0.55, 0.58, 0.62, 1.0);
    wall_mat.roughness = 0.9;
    let mut prop_mat = MaterialDef::solid(0.7, 0.45, 0.3, 1.0);
    prop_mat.roughness = 0.8;
    let floor_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(12.0));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let floor = world.resource_mut::<MaterialStore>().add(floor_mat);
    let wall = world.resource_mut::<MaterialStore>().add(wall_mat);
    let prop = world.resource_mut::<MaterialStore>().add(prop_mat);
    world.spawn((Transform::IDENTITY, floor_mesh, floor));
    // 背墙 / 侧墙围出内角
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 1.5, -2.6),
            scale: Vec3::new(6.0, 3.0, 0.2),
            ..Transform::IDENTITY
        },
        cube,
        wall,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(-2.6, 1.5, 0.0),
            scale: Vec3::new(0.2, 3.0, 6.0),
            ..Transform::IDENTITY
        },
        cube,
        wall,
    ));
    // 方柱（贴角）与两只箱体：接触遮蔽的主要来源
    world.spawn((
        Transform {
            position: Vec3::new(-2.05 + spin, 1.0, -2.15),
            scale: Vec3::new(0.5, 2.0, 0.5),
            ..Transform::IDENTITY
        },
        cube,
        prop,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(-0.9, 0.3, -0.6),
            scale: Vec3::new(0.6, 0.6, 0.6),
            ..Transform::IDENTITY
        },
        cube,
        prop,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(-1.5 + spin * 0.5, 0.25, 0.4),
            scale: Vec3::new(0.5, 0.5, 0.5),
            ..Transform::IDENTITY
        },
        cube,
        prop,
    ));
    propagate_transforms(&mut world);
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

fn sample_lum(px: &[u8], room: &AoRoom, at: Vec3, w: u32, h: u32) -> i32 {
    let (fx, fy) = project_to_pixel(&room.camera, at, w, h);
    let (x, y) = (fx as u32, fy as u32);
    assert!(x < w && y < h, "采样点 ({fx},{fy}) 越界 {w}x{h}");
    luminance(pixel(px, w, x, y))
}

/// 把一格画面拷进面板缓冲。
fn blit(panel: &mut [u8], src: &[u8], x0: u32, y0: u32) {
    let stride = (WIDTH * 2 + GAP) as usize;
    for row in 0..HEIGHT as usize {
        let sy = row * WIDTH as usize * 4;
        let dy = ((y0 as usize + row) * stride + x0 as usize) * 4;
        let n = WIDTH as usize * 4;
        panel[dy..dy + n].copy_from_slice(&src[sy..sy + n]);
    }
}

fn main() {
    let Some(mut renderer) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let room = ao_room();
    let mut panel =
        vec![0u8; ((WIDTH * 2 + GAP) * (HEIGHT * ROWS + GAP * (ROWS - 1))) as usize * 4];

    // 面板尺寸 → 行 y 偏移
    let row_y = |r: u32| r * (HEIGHT + GAP);

    // ---- 面板 1：日光房间 关|开 ----
    let world_off = build_world(&room, 1.2, None, 0.0);
    let mut world_on = build_world(&room, 1.2, None, 0.0);
    world_on.insert_resource(AoSettings {
        enabled: true,
        intensity: 1.0,
        radius: 0.9,
    });
    renderer.render(&world_on); // 中性首帧
    renderer.render(&world_on); // 生效帧
    let on = renderer.read_pixels();
    let mut renderer_off = OffscreenRenderer::new(WIDTH, HEIGHT).expect("关面板渲染器");
    renderer_off.render(&world_off);
    let off = renderer_off.read_pixels();
    let on_hash = fnv1a(&on);
    let off_hash = fnv1a(&off);
    blit(&mut panel, &off, 0, row_y(0));
    blit(&mut panel, &on, WIDTH + GAP, row_y(0));

    // 有效性断言（日光房间）：角落显著变暗，开阔地几乎不变
    let corner_off = sample_lum(&off, &room, room.corner, WIDTH, HEIGHT);
    let corner_on = sample_lum(&on, &room, room.corner, WIDTH, HEIGHT);
    let open_off = sample_lum(&off, &room, room.open, WIDTH, HEIGHT);
    let open_on = sample_lum(&on, &room, room.open, WIDTH, HEIGHT);
    println!(
        "面板1 角落 off={corner_off} on={corner_on} (Δ={})",
        corner_on - corner_off
    );
    println!(
        "面板1 开阔 off={open_off} on={open_on} (Δ={})",
        open_on - open_off
    );
    assert!(
        corner_on < corner_off - 24,
        "日光房间角落必须显著变暗（off={corner_off} on={corner_on}）"
    );
    assert_ne!(on_hash, off_hash, "AO 开关必须改变全帧");

    // ---- 面板 2：强度对照 0.5 | 2.0 ----
    let mut sample_intensity = |intensity: f32| -> (Vec<u8>, i32) {
        let mut world = build_world(&room, 1.2, None, 0.0);
        world.insert_resource(AoSettings {
            enabled: true,
            intensity,
            radius: 0.9,
        });
        renderer.render(&world);
        renderer.render(&world);
        let px = renderer.read_pixels();
        let corner = sample_lum(&px, &room, room.corner, WIDTH, HEIGHT);
        (px, corner)
    };
    let (weak_px, corner_weak) = sample_intensity(0.5);
    let (strong_px, corner_strong) = sample_intensity(2.0);
    blit(&mut panel, &weak_px, 0, row_y(1));
    blit(&mut panel, &strong_px, WIDTH + GAP, row_y(1));
    println!("面板2 角落 i=0.5: {corner_weak}, i=2.0: {corner_strong}");
    assert!(
        corner_strong < corner_weak,
        "强度指数必须单调（i2={corner_strong} < i0.5={corner_weak}）"
    );

    // ---- 面板 3：纯间接光 关|开（sun = 0，AO 调制作用最大化） ----
    let world_indirect_off = build_world(&room, 0.0, None, 0.0);
    let mut world_indirect_on = build_world(&room, 0.0, None, 0.0);
    world_indirect_on.insert_resource(AoSettings {
        enabled: true,
        intensity: 1.0,
        radius: 0.9,
    });
    renderer_off.render(&world_indirect_off);
    let indirect_off = renderer_off.read_pixels();
    renderer.render(&world_indirect_on);
    renderer.render(&world_indirect_on);
    let indirect_on = renderer.read_pixels();
    blit(&mut panel, &indirect_off, 0, row_y(2));
    blit(&mut panel, &indirect_on, WIDTH + GAP, row_y(2));
    let i_corner_off = sample_lum(&indirect_off, &room, room.corner, WIDTH, HEIGHT);
    let i_corner_on = sample_lum(&indirect_on, &room, room.corner, WIDTH, HEIGHT);
    let i_open_off = sample_lum(&indirect_off, &room, room.open, WIDTH, HEIGHT);
    let i_open_on = sample_lum(&indirect_on, &room, room.open, WIDTH, HEIGHT);
    println!(
        "面板3 角落 off={i_corner_off} on={i_corner_on} (Δ={})",
        i_corner_on - i_corner_off
    );
    println!(
        "面板3 开阔 off={i_open_off} on={i_open_on} (Δ={})",
        i_open_on - i_open_off
    );
    assert!(
        i_corner_on < i_corner_off - 24,
        "纯间接光角落必须显著变暗（off={i_corner_off} on={i_corner_on}）"
    );

    // ---- 跨运行确定性：关键帧二次渲染逐字节一致 ----
    let mut renderer_a = OffscreenRenderer::new(WIDTH, HEIGHT).expect("确定性渲染器 A");
    let mut renderer_b = OffscreenRenderer::new(WIDTH, HEIGHT).expect("确定性渲染器 B");
    let mut wa = build_world(&room, 1.2, None, 0.0);
    wa.insert_resource(AoSettings {
        enabled: true,
        intensity: 1.0,
        radius: 0.9,
    });
    let mut wb = build_world(&room, 1.2, None, 0.0);
    wb.insert_resource(AoSettings {
        enabled: true,
        intensity: 1.0,
        radius: 0.9,
    });
    renderer_a.render(&wa);
    renderer_a.render(&wa);
    renderer_b.render(&wb);
    renderer_b.render(&wb);
    assert_eq!(
        fnv1a(&renderer_a.read_pixels()),
        fnv1a(&renderer_b.read_pixels()),
        "两条会话必须逐位一致"
    );

    // ---- 写 PPM（供 ffmpeg 转 PNG 入 docs/）----
    let header = format!(
        "P6\n{} {}\n255\n",
        panel.len() as u32 / 4 / (HEIGHT * ROWS + GAP * (ROWS - 1)),
        HEIGHT * ROWS + GAP * (ROWS - 1)
    );
    let mut ppm = header.into_bytes();
    for chunk in panel.chunks_exact(4) {
        ppm.extend_from_slice(&chunk[..3]);
    }
    std::fs::write(Path::new("iris_ao.ppm"), &ppm).expect("写 iris_ao.ppm");
    println!("iris_ao.ppm 已写出");
    println!("AO 开启帧 FNV-1a = {on_hash:#018x}");
    println!("AO 关闭帧 FNV-1a = {off_hash:#018x}");
}
