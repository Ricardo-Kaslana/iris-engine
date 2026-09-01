//! 工具：2D UI 叠加层（HUD）视觉验收场景（离屏渲染输出 `iris_ui.bmp`）。
//!
//! 复用点光阴影场景（暗色房间 + 中央点光 + 环灯方块/球 + 地面）作为 3D
//! 背景，叠加 HUD：顶部 "IRIS ENGINE" 白色标题、左上血条（深底 UiRect +
//! 红色前景 UiRect 两个独立实体）、右下青色 "HUD OVERLAY" 占位文本。
//!
//! HUD pass 在色调映射 blit 之后：文字与色块应干净锐利、不受泛光/ACES
//! 影响，3D 场景（含点光阴影与泛光）照常渲染。
//!
//! ```sh
//! cargo run --example ui_snapshot --release
//! ```

use iris_core::hierarchy::propagate_transforms;
use iris_engine::prelude::*;

fn main() {
    env_logger::init();
    let Some(mut renderer) = OffscreenRenderer::new(1280, 720) else {
        eprintln!("无 GPU");
        std::process::exit(1);
    };

    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    // 暗色房间：方向光与环境光几乎不参与（关阴影），点光是唯一亮源
    world.insert_resource(DirectionalLight {
        intensity: 0.15,
        ambient: Vec3::splat(0.03),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(EnvironmentSettings { intensity: 0.15 });
    world.insert_resource(BloomSettings::default());

    // 中央点光：暖色高强度，投射 cubemap 阴影（环灯道具的径向阴影是场景签名）
    world.spawn((
        Transform::from_xyz(0.0, 2.8, 0.0),
        PointLight {
            color: Vec3::new(1.0, 0.72, 0.42),
            intensity: 42.0,
            range: 18.0,
            shadows: true,
        },
    ));

    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let sphere = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(0.55, 48, 24));

    // 环灯一圈的方块/球（与 point_shadow_snapshot 同款布局）
    let props: [(f32, f32, f32, f32, bool, [f32; 3]); 6] = [
        (0.0, 3.0, 1.6, 0.5, true, [0.85, 0.35, 0.2]),
        (3.0, 0.0, 1.2, 0.5, true, [0.3, 0.55, 0.85]),
        (2.2, -2.2, 2.2, 0.45, true, [0.45, 0.8, 0.5]),
        (-3.0, 0.6, 1.0, 0.55, false, [0.9, 0.75, 0.3]),
        (0.4, -3.2, 1.8, 0.5, true, [0.75, 0.4, 0.8]),
        (-2.4, 2.6, 0.9, 0.5, false, [0.55, 0.85, 0.9]),
    ];
    for (x, z, height, half, is_cube, color) in props {
        let mat = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(color[0], color[1], color[2], 1.0));
        let mesh = if is_cube { cube } else { sphere };
        let transform = if is_cube {
            Transform {
                position: Vec3::new(x, height * 0.5, z),
                scale: Vec3::new(half * 2.0, height, half * 2.0),
                ..Transform::IDENTITY
            }
        } else {
            Transform::from_xyz(x, height, z)
        };
        world.spawn((transform, mesh, mat));
    }

    // 深灰地面
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(20.0));
    let mat_g = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.16, 0.16, 0.18, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_g));

    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(5.5, 3.2, 5.5);
        camera.target = Vec3::new(0.0, 0.8, 0.0);
    }

    // ---- HUD 叠加层（屏幕像素坐标，原点左上、y 向下）----
    // 文本实体的 UiRect 仅作定位载体（不绘制矩形）
    world.spawn((
        UiRect {
            position: [24.0, 20.0],
            size: [0.0, 0.0],
        },
        UiText {
            string: "IRIS ENGINE".into(),
            scale: 4.0,
        },
        UiColor::default(),
    ));

    // 血条：深底 + 红色前景（两个独立矩形实体，前景短一截模拟「未满血」）
    world.spawn((
        UiRect {
            position: [24.0, 76.0],
            size: [250.0, 26.0],
        },
        UiColor([0.04, 0.04, 0.06, 0.85]),
    ));
    world.spawn((
        UiRect {
            position: [27.0, 79.0],
            size: [180.0, 20.0],
        },
        UiColor([0.90, 0.15, 0.15, 1.0]),
    ));

    // 右下占位文本：青色，定位载体算好等宽宽度（11 字符 × 8 × 2 = 176px）
    world.spawn((
        UiRect {
            position: [1280.0 - 176.0 - 24.0, 720.0 - 16.0 - 24.0],
            size: [0.0, 0.0],
        },
        UiText {
            string: "HUD OVERLAY".into(),
            scale: 2.0,
        },
        UiColor([0.2, 0.9, 0.9, 1.0]),
    ));

    propagate_transforms(&mut world);
    renderer.render(&world);
    renderer.save_bmp("iris_ui.bmp").expect("写 BMP");
    println!("已输出 iris_ui.bmp");
}
