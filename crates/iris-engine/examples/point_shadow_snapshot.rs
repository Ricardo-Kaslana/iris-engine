//! 工具：点光 cubemap 阴影视觉验收场景（离屏渲染输出 `iris_point_shadow.bmp`）。
//!
//! 场景：暗色房间 + 中央点光（暖色、高强度、shadows = true），
//! 环灯一圈摆放不同高度的方块/球。点光阴影的特征是阴影从物体根部
//! **背离点光方向径向拉伸**（方向光阴影是平行投影，无此放射状）。
//!
//! ```sh
//! cargo run --example point_shadow_snapshot --release
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

    // 中央点光：暖色高强度，投射 cubemap 阴影
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

    // 环灯一圈的方块/球（不同高度、朝向相机可见的半圈为主）：
    // 每个物体的阴影都应从根部背离点光方向径向拉伸，合起来呈放射状
    let props: [(f32, f32, f32, f32, bool, [f32; 3]); 6] = [
        // x, z, 高度, 半宽/半径, 是否方块, 颜色
        (0.0, 3.0, 1.6, 0.5, true, [0.85, 0.35, 0.2]), // 近相机：阴影朝 +Z 拉向画面下缘
        (3.0, 0.0, 1.2, 0.5, true, [0.3, 0.55, 0.85]), // 右侧：阴影朝 +X
        (2.2, -2.2, 2.2, 0.45, true, [0.45, 0.8, 0.5]), // 右后：阴影朝 −X−Z（被自身遮挡一半）
        (-3.0, 0.6, 1.0, 0.55, false, [0.9, 0.75, 0.3]), // 左侧球：阴影朝 −X
        (0.4, -3.2, 1.8, 0.5, true, [0.75, 0.4, 0.8]), // 正后：阴影朝 −Z
        (-2.4, 2.6, 0.9, 0.5, false, [0.55, 0.85, 0.9]), // 左前球：阴影朝 −X+Z
    ];
    for (x, z, height, half, is_cube, color) in props {
        let mat = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(color[0], color[1], color[2], 1.0));
        let mesh = if is_cube { cube } else { sphere };
        // 方块用非均匀缩放改变高度；球按半径落地
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

    propagate_transforms(&mut world);
    renderer.render(&world);
    renderer.save_bmp("iris_point_shadow.bmp").expect("写 BMP");
    println!("已输出 iris_point_shadow.bmp");
}
