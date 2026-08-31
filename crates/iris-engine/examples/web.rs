//! 示例：Web（wasm32-unknown-unknown）冒烟入口——点光场景 + HUD 叠加层。
//!
//! 场景参照 `ui_snapshot.rs`：暗色房间中央点光 + 一圈彩色方块 + 地面，
//! 叠加 HUD（顶部标题 + 左上血条）。wasm 下由 winit 的 web 后端驱动 App，
//! 立方体自转可确认 Time（`performance.now()` 封装）与逐帧渲染在跑。
//!
//! 本地浏览器冒烟（需要 Rust 1.85+ 与 wasm32 target）：
//! ```sh
//! rustup target add wasm32-unknown-unknown
//! cargo build --example web --release --target wasm32-unknown-unknown
//! wasm-bindgen --out-dir web-demo --target web \
//!     target/wasm32-unknown-unknown/release/examples/web.wasm
//! # web-demo/index.html 已提供，起任意静态服务器即可：
//! # npx serve web-demo   （或 python -m http.server -d web-demo）
//! ```
//!
//! 平台说明（web）：
//! - 需要支持 WebGPU 的浏览器（Chrome / Edge 113+）；wgpu 30 在 wasm 下
//!   只编译 WebGPU 后端（WebGL2 需显式启用非默认 `webgl` feature）；
//! - winit 默认把创建的 canvas 附加进 `document.body`（iris 已在
//!   `WindowConfig` 显式开启），容器页只需提供空 `<body>`；
//! - GPU 适配器 / 设备请求是 JS Promise，引擎在事件循环内逐帧驱动初始化，
//!   就绪前不渲染（1-2 帧内完成）；
//! - `env_logger` 在 wasm 编译通过但无输出（写往不可见的 stderr），v1 不接
//!   console log，故本示例仅 native 开启日志；
//! - Esc 触发 `AppExit` 后 winit web 以 JS 异常 unwind 退出循环，属正常路径。

use iris_engine::prelude::*;

fn main() {
    // 平台原因（wasm32）：env_logger 写 stderr，浏览器中不可见（v1 未接
    // console log），native 照常启用
    #[cfg(not(target_arch = "wasm32"))]
    env_logger::init();

    App::new()
        .add_plugins(DefaultPlugins::new("iris web"))
        .add_system(Stage::Startup, setup)
        .add_system(Stage::Update, spin)
        .run();
}

#[derive(Component)]
struct Spin(f32);

fn setup(world: &mut World) {
    // 灯光基调与 ui_snapshot 一致：暗色房间，中央点光是唯一亮源
    world.insert_resource(DirectionalLight {
        intensity: 0.15,
        ambient: Vec3::splat(0.03),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(EnvironmentSettings { intensity: 0.15 });

    // 中央点光：暖色高强度
    world.spawn((
        Transform::from_xyz(0.0, 2.8, 0.0),
        PointLight {
            color: Vec3::new(1.0, 0.72, 0.42),
            intensity: 42.0,
            range: 18.0,
            shadows: false, // web 冒烟从简：不开 cubemap 阴影
        },
    ));

    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());

    // 一圈自转方块（与 ui_snapshot 的道具环同布局，取方块位）
    let props: [(f32, f32, f32, f32, [f32; 3]); 4] = [
        (0.0, 3.0, 1.6, 0.5, [0.85, 0.35, 0.2]),
        (3.0, 0.0, 1.2, 0.5, [0.3, 0.55, 0.85]),
        (2.2, -2.2, 2.2, 0.45, [0.45, 0.8, 0.5]),
        (-3.0, 0.6, 1.0, 0.55, [0.9, 0.75, 0.3]),
    ];
    for (x, z, height, half, color) in props {
        let mat = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(color[0], color[1], color[2], 1.0));
        world.spawn((
            Transform {
                position: Vec3::new(x, height * 0.5, z),
                scale: Vec3::new(half * 2.0, height, half * 2.0),
                ..Transform::IDENTITY
            },
            cube,
            mat,
            Spin(0.8),
        ));
    }

    // 深灰地面
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(20.0));
    let mat_g = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.16, 0.16, 0.18, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_g));

    // 固定机位（web 无飞行相机，画面稳定便于冒烟确认）
    // 注意：resource_mut 的守卫带 Drop，必须用块作用域先行释放
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(5.5, 3.2, 5.5);
        camera.target = Vec3::new(0.0, 0.8, 0.0);
    }

    // ---- HUD（与 ui_snapshot 同款：标题 + 血条）----
    // 屏幕像素坐标按 1280×720 窗口布设（web 画布初始尺寸同 DefaultPlugins）
    world.spawn((
        UiRect { position: [24.0, 20.0], size: [0.0, 0.0] },
        UiText { string: "IRIS WEB".into(), scale: 4.0 },
        UiColor::default(),
    ));
    world.spawn((
        UiRect { position: [24.0, 76.0], size: [250.0, 26.0] },
        UiColor([0.04, 0.04, 0.06, 0.85]),
    ));
    world.spawn((
        UiRect { position: [27.0, 79.0], size: [180.0, 20.0] },
        UiColor([0.90, 0.15, 0.15, 1.0]),
    ));
}

fn spin(world: &mut World) {
    let dt = world.resource::<Time>().delta_secs();
    world.query::<(&mut Transform, &Spin)>(|(t, s)| {
        t.rotation = Quat::from_axis_angle(Vec3::Y, s.0 * dt) * t.rotation;
    });
}
