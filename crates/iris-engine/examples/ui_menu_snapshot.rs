//! R41 视觉验收：锚定菜单 + 焦点高亮 + 右下角小地图（离屏渲染输出
//! `iris_ui_menu.bmp`）。
//!
//! 场景：几个立方体 + 地面为 3D 背景（透视主视图）；叠加居中的暂停菜单
//! （半透明底板 + RESUME / SETTINGS / QUIT 三按钮，SETTINGS 处于焦点高亮
//! 态）与右下角小地图（复用 R40 `RenderTargetStore` + `RendersTo` 相机，
//! `UiImageRtt` 采样呈现）。
//!
//! 验收点（目检文档 `docs/ui_menu.png`）：
//! - 锚定正确：菜单系统经 `update_ui_layout` 把 Center 锚点解析为绝对像素，
//!   底板与按钮保持居中（纯函数 `anchor_to_position` 的「改视口不漂移」由
//!   单测覆盖）；
//! - 焦点高亮可见：SETTINGS 背景改写为高亮色（`update_ui_focus`）；
//! - 按钮文字清晰：位图字体等宽步进，字色与底板对比。
//!
//! ```sh
//! cargo run --example ui_menu_snapshot --release
//! # 随后 ffmpeg 转 PNG：ffmpeg -y -i iris_ui_menu.bmp docs/ui_menu.png
//! ```

use glam::Mat3;
use iris_core::hierarchy::propagate_transforms;
use iris_engine::prelude::*;

/// 组装顶部透视（默认资源相机）所需的背景场景：立方体 + 地面 + 点光。
fn build_scene(world: &mut World) {
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.9, 0.45, 0.2, 1.0));
    let mut transform = Transform::from_xyz(0.0, 0.6, 0.0);
    transform.rotation = Quat::from_rotation_y(0.6);
    world.spawn((transform, cube, mat));

    let cube2 = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat2 = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.3, 0.55, 0.85, 1.0));
    world.spawn((Transform::from_xyz(1.8, 0.8, -1.0), cube2, mat2));

    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(20.0));
    let mat_g = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.3, 0.33, 0.4, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_g));

    world.spawn((
        Transform::from_xyz(-1.8, 1.2, 1.2),
        PointLight::new(Vec3::new(0.3, 0.6, 1.0), 14.0, 9.0),
    ));
    world.spawn((
        Transform::from_xyz(1.8, 1.0, -1.0),
        PointLight::new(Vec3::new(1.0, 0.25, 0.2), 12.0, 8.0),
    ));

    let mut camera = world.resource_mut::<Camera>();
    camera.eye = Vec3::new(4.5, 3.0, 4.5);
    camera.target = Vec3::new(0.0, 0.8, 0.0);
}

/// 右下角小地图：R40 `RendersTo` 相机（俯视 3D 场景）+ `UiImageRtt` 采样。
fn spawn_minimap(world: &mut World) {
    world.insert_resource(RenderTargetStore::new());
    let minimap = world.resource_mut::<RenderTargetStore>().add(200, 200);
    // 俯视相机：位于场景上空斜向 45°，避免正下方 +Y 上向量退化的平行问题
    let eye = Vec3::new(0.0, 20.0, 14.0);
    let back = eye.normalize(); // eye 指向原点 → 相机 +Z 轴（back）
    let right = Vec3::Y.cross(back).normalize_or_zero();
    let up = back.cross(right).normalize_or_zero();
    let rot = Quat::from_mat3(&Mat3::from_cols(right, up, back));
    world.spawn((
        Transform {
            position: eye,
            rotation: rot,
            ..Transform::IDENTITY
        },
        ActiveCamera::default(),
        RendersTo { target: minimap },
    ));
    // 把该 RT 采样到屏幕右下角（红渲染为小地图）
    world.spawn((
        UiRect {
            position: [0.0, 0.0],
            size: [200.0, 200.0],
        },
        UiImageRtt { target: minimap },
        UiColor([1.0, 1.0, 1.0, 1.0]),
        UiAnchor {
            point: UiAnchorPoint::BottomRight,
            offset: [-16.0, -16.0],
        },
    ));
}

fn main() {
    env_logger::init();
    let Some(mut renderer) = OffscreenRenderer::new(1280, 720) else {
        eprintln!("没有可用的 GPU 适配器（离屏渲染需要 Vulkan / DirectX 12 / Metal）");
        std::process::exit(1);
    };

    let mut world = World::new();
    // 渲染 / 资源目录
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());
    // 离屏程序无窗口事件：手动写入实际渲染目标尺寸（锚点解析用）
    world.insert_resource(Viewport::new(1280, 720));
    // UI 焦点 / 菜单栈资源（`UiPlugin` 在离屏场景手动插资源）
    world.insert_resource(Input::new());
    world.insert_resource(UiFocus::default());
    world.insert_resource(UiMenuStack::default());

    build_scene(&mut world);
    spawn_minimap(&mut world);

    // ---- 居中暂停菜单：半透明底板 + 三按钮（同一 Center 锚点）----
    let menu = "pause";
    world.resource_mut::<UiMenuStack>().push_menu(menu);
    // 底板（无 UiText → 矩形被绘制）
    world.spawn((
        UiRect {
            position: [0.0, 0.0],
            size: [220.0, 220.0],
        },
        UiColor([0.06, 0.07, 0.10, 0.88]),
        UiMenu(menu.into()),
        UiAnchor {
            point: UiAnchorPoint::Center,
            offset: [0.0, 0.0],
        },
    ));
    // 三按钮：RESUME / SETTINGS / QUIT，纵向排列（offset -48 / 0 / +48）
    let anchor = |dy: f32| UiAnchor {
        point: UiAnchorPoint::Center,
        offset: [0.0, dy],
    };
    let bg = [0.16, 0.18, 0.24, 0.95];
    let fg = [0.90, 0.90, 0.92, 1.0];
    let _resume = spawn_ui_button(
        &mut world,
        "RESUME",
        [180.0, 40.0],
        bg,
        fg,
        2.0,
        anchor(-48.0),
        menu,
    );
    let settings = spawn_ui_button(
        &mut world,
        "SETTINGS",
        [180.0, 40.0],
        bg,
        fg,
        2.0,
        anchor(0.0),
        menu,
    );
    let _quit = spawn_ui_button(
        &mut world,
        "QUIT",
        [180.0, 40.0],
        bg,
        fg,
        2.0,
        anchor(48.0),
        menu,
    );

    // 初始焦点落在 SETTINGS（演示焦点高亮态）
    world.resource_mut::<UiFocus>().current = Some(settings);

    // 离屏主循环合一帧：层级传播 → 锚定 → 菜单可见性 → 焦点高亮 → 渲染
    propagate_transforms(&mut world);
    update_ui_layout(&mut world);
    update_ui_menus(&mut world);
    update_ui_focus(&mut world);

    renderer.render(&world);
    renderer.save_bmp("iris_ui_menu.bmp").expect("写 BMP");
    println!("已输出 iris_ui_menu.bmp（1280×720，锚定菜单 + 焦点高亮 + 小地图）");
}
