//! 工具：多视口与渲染到纹理（R40）视觉验收场景（离屏渲染输出
//! `docs/rendertarget.png`）。
//!
//! 第三人称主视图看场景（地面 + 三色立方体）；左上角 = 俯视小地图（RT
//! 相机 + UiImageRtt 200×200，内容 = 场景俯视投影）；右侧 = 监视屏（第二台
//! RT 相机盯一个色块 + UiImageRtt 320×180）。目检要点：小地图 / 监视屏内容
//! 与主视图场景对应、位置正确、无错位；小地图底图垫在 HUD 文字之下。
//!
//! ```sh
//! cargo run --example rendertarget_snapshot --release
//! ```
//!
//! 产物：`docs/rendertarget.png`（640×360，从仓库根目录运行）。

use glam::{Quat, Vec3};
use iris_core::hierarchy::propagate_transforms;
use iris_engine::prelude::*;

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
    // 第三人称主相机（资源相机）= 主视图；RT 相机以 (ActiveCamera + RendersTo)
    // 关联到各自渲染目标。
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 4.0, 9.0),
        target: Vec3::new(0.0, 0.4, 0.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        intensity: 1.4,
        shadows: false,
        ..DirectionalLight::default()
    });
    // 泛光关闭：验收关注几何 / 位置对应，避免高光光晕混淆目检。
    world.insert_resource(BloomSettings::disabled());

    // 渲染目标 store（游戏侧按需 insert，不进 DefaultPlugins）：
    // 0 号 = 小地图 200×200，1 号 = 监视屏 320×180。
    world.insert_resource(RenderTargetStore::new());
    let minimap = world.resource_mut::<RenderTargetStore>().add(200, 200);
    let monitor = world.resource_mut::<RenderTargetStore>().add(320, 180);

    // 地面 + 三色立方体（红 / 绿 / 蓝），小地图俯视可见其布局，监视屏盯红色。
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(60.0));
    let ground_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.45, 0.46, 0.48, 1.0));
    world.spawn((Transform::IDENTITY, plane, ground_mat));

    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let color_mat = |r: f32, g: f32, b: f32| {
        world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(r, g, b, 1.0))
    };
    let red_mat = color_mat(0.85, 0.12, 0.10);
    let green_mat = color_mat(0.10, 0.75, 0.20);
    let blue_mat = color_mat(0.12, 0.25, 0.85);
    world.spawn((Transform::from_xyz(0.0, 0.5, 0.0), cube, red_mat));
    world.spawn((Transform::from_xyz(2.6, 0.5, 1.0), cube, green_mat));
    world.spawn((Transform::from_xyz(-2.6, 0.5, 1.0), cube, blue_mat));

    // 小地图 RT 相机：正上方向下看（局部 +Z → 世界 +Y ⇒ 前方 -Z → -Y）。
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 22.0, 0.0),
            rotation: Quat::from_rotation_arc(Vec3::Z, Vec3::Y),
            scale: Vec3::ONE,
        },
        ActiveCamera::default(),
        RendersTo { target: minimap },
    ));
    // 监视屏 RT 相机：正上方向下看红色立方体（特写；小地图为全场景俯视，
    // 监视屏为色块特写——两者朝向/放大不同，验证多 RT 相机互不干扰）。
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 4.0, 0.0),
            rotation: Quat::from_rotation_arc(Vec3::Z, Vec3::Y),
            scale: Vec3::ONE,
        },
        ActiveCamera::default(),
        RendersTo { target: monitor },
    ));

    // UI：小地图（左上）+ 监视屏（右侧），文本标签垫在其上（图片之下、文本之上）。
    spawn_ui_rtt(&mut world, [8.0, 8.0], [200.0, 200.0], minimap, "MINIMAP");
    spawn_ui_rtt(
        &mut world,
        [320.0, 100.0],
        [314.0, 180.0],
        monitor,
        "MONITOR",
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
        .save("docs/rendertarget.png")
        .expect("写 docs/rendertarget.png");
    println!("已输出 docs/rendertarget.png（{w}x{h}）");
}

/// 挂一个 UiImageRtt（渲染目标贴屏）+ 底部标签文本。标签用独立实体（其
/// UiRect 作定位载体、不绘制矩形——见 ui.rs 文档），保证不覆盖底图。
fn spawn_ui_rtt(
    world: &mut World,
    pos: [f32; 2],
    size: [f32; 2],
    target: RenderTargetHandle,
    label: &str,
) {
    world.spawn((
        UiRect {
            position: pos,
            size,
        },
        UiImageRtt { target },
        UiColor::default(),
    ));
    world.spawn((
        UiRect {
            position: [pos[0] + 6.0, pos[1] + 6.0],
            size: [8.0, 8.0],
        },
        UiText {
            string: label.into(),
            scale: 2.0,
        },
        UiColor([1.0, 1.0, 1.0, 1.0]),
    ));
}
