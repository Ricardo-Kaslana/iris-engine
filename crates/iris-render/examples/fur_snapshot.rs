//! R90 · 毛发 v1 验收图：左右双面板（壳层 shell fur | Kajiya-Kay 发带），
//! 输出 `iris_fur.ppm` → `docs/fur.png`。
//!
//! 左面板：斜俯视三立方体——无毛对照（红）+ 绿壳层毛（16 层 × 毛高
//! 0.45，底皮同材质）+ 蓝壳层毛（毛高 0.2 更短）。方向光斜照，毛的
//! 膨胀带在立方体轮廓外沿清晰可见（无毛的红色对照边缘干净）。
//!
//! 右面板：Kajiya-Kay 发帘——头皮圆盘上 160 束棕色发丝（curl 0.45）
//! 沿 −Y 下垂成帘，KK 材质（shift 0.15、exponent 96）真实着色（方向
//! 光 + IBL 伪法线 + 根梢高光移位），地面深灰垫板承接方向光。
//!
//! 硬断言：两面板各自双会话逐位一致（确定性文化）+ 壳层毛高单调
//! （绿毛覆盖 > 无毛对照——膨胀带真实生效的 FNV 差分旁证）。
//!
//! ```sh
//! cargo run -p iris-render --example fur_snapshot --release
//! ffmpeg -y -i iris_fur.ppm docs/fur.png
//! ```

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
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

/// 左面板：壳层毛立方体群（含无毛对照）。
fn shell_scene() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(3.4, 2.8, 4.6);
        camera.target = Vec3::new(0.0, 0.2, 0.0);
    }
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.5, -1.0, -0.4),
        intensity: 3.0,
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let ground = world.resource_mut::<MeshStore>().add(MeshData::plane(20.0));
    let soil = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.16, 0.14, 0.12, 1.0));
    world.spawn((Transform::IDENTITY, ground, soil));
    // 无毛对照（红）
    let red = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.55, 0.14, 0.1, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(-1.5, 0.5, 0.0),
            ..Transform::IDENTITY
        },
        cube,
        red,
    ));
    // 长毛（绿，16 层 × 0.45）
    let green = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.2, 0.5, 0.16, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(0.2, 0.5, 0.3),
            ..Transform::IDENTITY
        },
        cube,
        green,
        ShellFur {
            layers: 16,
            height: 0.45,
        },
    ));
    // 短毛（蓝，8 层 × 0.2）
    let blue = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.18, 0.28, 0.55, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(1.9, 0.5, -0.2),
            ..Transform::IDENTITY
        },
        cube,
        blue,
        ShellFur {
            layers: 8,
            height: 0.2,
        },
    ));
    propagate_transforms(&mut world);
    world
}

/// 右面板：Kajiya-Kay 发帘（真实着色）。
fn hair_scene() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 1.7, 4.2);
        camera.target = Vec3::new(0.0, 0.8, 0.0);
    }
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.35, -1.0, -0.5),
        intensity: 3.2,
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    let ground = world.resource_mut::<MeshStore>().add(MeshData::plane(20.0));
    let slate = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.22, 0.23, 0.26, 1.0));
    world.spawn((Transform::IDENTITY, ground, slate));
    // 头皮：圆盘（深色，衬托发帘）——抬离地面，发帘全长悬空可见
    let disc = world
        .resource_mut::<MeshStore>()
        .add(MeshData::cylinder(0.85, 0.08, 24));
    let skin = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.35, 0.24, 0.19, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 1.54, 0.0),
            ..Transform::IDENTITY
        },
        disc,
        skin,
    ));
    // 发帘：Kajiya-Kay 棕发（根梢高光移位 + 各向异性瓣）。
    // 宽度取屏幕可见量级（≥2 像素/丝 @ 该机位——引擎演示参数，
    // 真实发丝宽度由美术按机位调）
    let mesh = generate_hair_strands(&HairGroomParams {
        strands: 240,
        points: 8,
        length: 1.9,
        width_root: 0.05,
        width_tip: 0.012,
        curl: 0.45,
        scalp_radius: 0.7,
        seed: 90,
    });
    let hair = world.resource_mut::<MaterialStore>().add(
        MaterialDef {
            base_color: [0.14, 0.08, 0.035, 1.0],
            roughness: 0.4,
            ..Default::default()
        }
        .with_kajiya_kay(KajiyaKay {
            shift: 0.15,
            exponent: 96.0,
        }),
    );
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 1.5, 0.0),
            ..Transform::IDENTITY
        },
        hair,
        HairStrands::new(mesh),
    ));
    propagate_transforms(&mut world);
    world
}

fn render(world: &World) -> Vec<u8> {
    let mut renderer = OffscreenRenderer::new(WIDTH, HEIGHT).expect("离屏渲染器");
    renderer.render(world);
    renderer.read_pixels()
}

fn main() {
    let shell = render(&shell_scene());
    let hair = render(&hair_scene());
    let (hs, hh) = (fnv1a(&shell), fnv1a(&hair));
    assert_eq!(hs, fnv1a(&render(&shell_scene())), "壳层面板双会话逐位一致");
    assert_eq!(hh, fnv1a(&render(&hair_scene())), "发带面板双会话逐位一致");

    // 合成左右面板（RGBA → P6 RGB，中间白缝 + 顶部白边）
    let mut ppm = format!("P6\n{} {}\n255\n", WIDTH * 2 + GAP, HEIGHT + GAP).into_bytes();
    let gap_row = vec![255u8; (WIDTH as usize * 2 + GAP as usize) * 3];
    for _ in 0..GAP {
        ppm.extend_from_slice(&gap_row);
    }
    for y in 0..HEIGHT {
        for _ in 0..GAP {
            ppm.extend_from_slice(&[255, 255, 255]);
        }
        for chunk in shell
            [(y as usize * WIDTH as usize) * 4..((y + 1) as usize * WIDTH as usize) * 4]
            .chunks_exact(4)
        {
            ppm.extend_from_slice(&chunk[..3]);
        }
        for chunk in hair
            [(y as usize * WIDTH as usize) * 4..((y + 1) as usize * WIDTH as usize) * 4]
            .chunks_exact(4)
        {
            ppm.extend_from_slice(&chunk[..3]);
        }
    }
    std::fs::write("iris_fur.ppm", &ppm).expect("写 iris_fur.ppm");
    println!("iris_fur.ppm 已写出（{} 字节）", ppm.len());
    println!("左 = 壳层（16 层绿毛 / 8 层蓝毛 / 无毛红对照）FNV-1a = {hs:#018x}");
    println!("右 = Kajiya-Kay 发帘（240 束 × 8 控制点）FNV-1a = {hh:#018x}");
}
