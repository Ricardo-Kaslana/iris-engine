//! 顶点色与受控材质扩展的 GPU 回归。
//!
//! 覆盖三条硬约束：
//! 1. 材质缺省 `Ignore` 时，网格携带顶点色与完全不携带颜色的画面**逐字节
//!    一致**——顶点色分支没有在默认路径上引入任何算术；
//! 2. 材质开启 `Multiply` 时，红色顶点色与红色基色的画面逐字节一致，
//!    静态与蒙皮两条顶点流都验证（蒙皮管线颜色在第 6 个顶点缓冲）；
//! 3. `Unlit` 完全忽略方向光、环境光与点光，两个光照配置逐字节一致，
//!    且与 `Lit` 画面可判别；同一渲染器上经 `MaterialStore::get_mut`
//!    切换开关后画面必须变化（材质修订号驱动绑定组重建）。
//!
//! 跨世界对照各用独立渲染器（句柄值缓存会串味，AGENTS 规约）。文件内
//! 互斥串行化重负载 GPU 测试；无适配器时打印跳过并返回。

use std::sync::Mutex;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Entity, Transform, World};
use iris_render::prelude::*;

static GPU_SERIAL: Mutex<()> = Mutex::new(());

const WIDTH: u32 = 160;
const HEIGHT: u32 = 120;

fn assert_pixels_equal(label: &str, got: &[u8], want: &[u8]) {
    assert_eq!(got.len(), want.len(), "{label}：像素长度不一致");
    if let Some(index) = got.iter().zip(want).position(|(a, b)| a != b) {
        panic!(
            "{label}：首差字节 {index}（got {}，want {}，共 {} 字节）",
            got[index],
            want[index],
            got.len()
        );
    }
}

fn assert_pixels_differ(label: &str, a: &[u8], b: &[u8]) {
    if a == b {
        panic!("{label}：画面应不同但逐字节一致");
    }
}

/// 固定相机、方向光、关闭泛光的基础世界。
fn base_world() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 1.6, 4.5),
        target: Vec3::new(0.0, 0.8, 0.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(0.4, -0.8, 0.6).normalize(),
        intensity: 1.5,
        ambient: Vec3::splat(0.2),
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    world
}

/// 在指定位置生成一个立方体；`colors` 为 Some 时给全部顶点写入该颜色。
fn spawn_cube(
    world: &mut World,
    position: Vec3,
    material: MaterialHandle,
    colors: Option<VertexColor>,
) -> Entity {
    let mut mesh = MeshData::cube();
    mesh.colors = colors.map(|color| vec![color; mesh.vertices.len()]);
    let mesh = world.resource_mut::<MeshStore>().add(mesh);
    world.spawn((
        Transform {
            position,
            ..Transform::IDENTITY
        },
        mesh,
        material,
    ))
}

/// 独立渲染器渲染一帧并读回像素；无 GPU 时返回 None。
fn render_pixels(world: &World) -> Option<Vec<u8>> {
    let mut renderer = OffscreenRenderer::new(WIDTH, HEIGHT)?;
    renderer.render(world);
    Some(renderer.read_pixels())
}

/// Ignore 是默认路径：带颜色与不带颜色必须逐字节一致；Multiply 必须
/// 把顶点色乘进反照率，且与等价的红色基色世界逐字节一致。
#[test]
fn vertex_color_ignore_is_bitwise_and_multiply_matches_tinted_base() {
    let _gpu = GPU_SERIAL.lock().unwrap();

    // 世界 A：红色顶点色 + 默认 Ignore。
    let mut ignore_world = base_world();
    let white = ignore_world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(1.0, 1.0, 1.0, 1.0));
    spawn_cube(
        &mut ignore_world,
        Vec3::new(0.0, 0.8, 0.0),
        white,
        Some(VertexColor::new(255, 0, 0, 255)),
    );
    propagate_transforms(&mut ignore_world);

    // 世界 B：完全相同但没有颜色。
    let mut plain_world = base_world();
    let plain_material = plain_world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(1.0, 1.0, 1.0, 1.0));
    spawn_cube(
        &mut plain_world,
        Vec3::new(0.0, 0.8, 0.0),
        plain_material,
        None,
    );
    propagate_transforms(&mut plain_world);

    // 世界 C：红色顶点色 + Multiply。
    let mut multiply_world = base_world();
    let multiply_material = multiply_world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(1.0, 1.0, 1.0, 1.0).with_vertex_color(VertexColorMode::Multiply));
    spawn_cube(
        &mut multiply_world,
        Vec3::new(0.0, 0.8, 0.0),
        multiply_material,
        Some(VertexColor::new(255, 0, 0, 255)),
    );
    propagate_transforms(&mut multiply_world);

    // 世界 D：无颜色 + 红色基色（Multiply 的等价参照）。
    let mut tinted_world = base_world();
    let tinted_material = tinted_world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(1.0, 0.0, 0.0, 1.0));
    spawn_cube(
        &mut tinted_world,
        Vec3::new(0.0, 0.8, 0.0),
        tinted_material,
        None,
    );
    propagate_transforms(&mut tinted_world);

    let Some(ignore_pixels) = render_pixels(&ignore_world) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(plain_pixels) = render_pixels(&plain_world) else {
        return;
    };
    let Some(multiply_pixels) = render_pixels(&multiply_world) else {
        return;
    };
    let Some(tinted_pixels) = render_pixels(&tinted_world) else {
        return;
    };

    assert_pixels_equal("Ignore 带色与无色", &ignore_pixels, &plain_pixels);
    assert_pixels_equal(
        "Multiply 红色顶点色与红色基色",
        &multiply_pixels,
        &tinted_pixels,
    );
    assert_pixels_differ("Multiply 必须改变画面", &multiply_pixels, &ignore_pixels);
}

/// 蒙皮标准管线的颜色缓冲是第 6 个顶点流（槽 5）；颜色与 UV2 绑错会
/// 同时破坏采样与像素。恒等单关节蒙皮下，Multiply 必须与红色基色一致。
#[test]
fn skinned_vertex_color_uses_sixth_vertex_stream() {
    let _gpu = GPU_SERIAL.lock().unwrap();

    let build = |multiply: bool| {
        let mut world = base_world();
        let joint = world.spawn((Transform::from_xyz(0.0, 0.8, 0.0),));
        let mut mesh = MeshData::cube();
        mesh.skin = Some(vec![
            SkinVertex {
                joints: [0, 0, 0, 0],
                weights: [1.0, 0.0, 0.0, 0.0],
            };
            mesh.vertices.len()
        ]);
        mesh.colors = Some(vec![VertexColor::new(255, 0, 0, 255); mesh.vertices.len()]);
        let mesh = world.resource_mut::<MeshStore>().add(mesh);
        let mut material = MaterialDef::solid(1.0, 1.0, 1.0, 1.0);
        if multiply {
            material = material.with_vertex_color(VertexColorMode::Multiply);
        } else {
            material = MaterialDef::solid(1.0, 0.0, 0.0, 1.0);
        }
        let material = world.resource_mut::<MaterialStore>().add(material);
        world.spawn((
            Transform::from_xyz(0.0, 0.8, 0.0),
            mesh,
            material,
            Skin::new(vec![joint], vec![glam::Mat4::IDENTITY]),
        ));
        propagate_transforms(&mut world);
        world
    };

    let multiply_world = build(true);
    let tinted_world = build(false);

    let Some(multiply_pixels) = render_pixels(&multiply_world) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(tinted_pixels) = render_pixels(&tinted_world) else {
        return;
    };
    assert_pixels_equal("蒙皮 Multiply 与红色基色", &multiply_pixels, &tinted_pixels);
}

/// 相机置于双面立方体内部：全部像素都是同一材质的内壁。
fn interior_world(shading: ShadingMode, lit_scale: f32, with_point_light: bool) -> World {
    let mut world = base_world();
    world.insert_resource(Camera {
        eye: Vec3::ZERO,
        target: Vec3::new(0.0, 0.0, -1.0),
        ..Camera::default()
    });
    let mut light = world.resource_mut::<DirectionalLight>();
    light.intensity = 1.5 * lit_scale;
    light.ambient = Vec3::splat(0.2 * lit_scale);
    drop(light);
    if with_point_light {
        world.spawn((
            Transform::from_xyz(0.0, 0.0, 0.0),
            PointLight::new(Vec3::ONE, 40.0, 50.0),
        ));
    }
    let mut material = MaterialDef::solid(0.25, 0.5, 0.75, 1.0).with_double_sided();
    material.extensions.shading = shading;
    let material = world.resource_mut::<MaterialStore>().add(material);
    let mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    world.spawn((
        Transform {
            position: Vec3::ZERO,
            scale: Vec3::splat(10.0),
            ..Transform::IDENTITY
        },
        mesh,
        material,
    ));
    propagate_transforms(&mut world);
    world
}

/// Unlit 在 cutout 之后早退，完全不吃方向光、环境光与点光；两个光照
/// 配置必须逐字节一致，且与同一光照下的 Lit 画面可判别。
#[test]
fn unlit_ignores_all_lighting_and_differs_from_lit() {
    let _gpu = GPU_SERIAL.lock().unwrap();

    let unlit_dim = interior_world(ShadingMode::Unlit, 1.0, false);
    let unlit_bright = interior_world(ShadingMode::Unlit, 4.0, true);
    let lit_bright = interior_world(ShadingMode::Lit, 4.0, true);

    let Some(unlit_dim_pixels) = render_pixels(&unlit_dim) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(unlit_bright_pixels) = render_pixels(&unlit_bright) else {
        return;
    };
    let Some(lit_bright_pixels) = render_pixels(&lit_bright) else {
        return;
    };

    assert_pixels_equal(
        "Unlit 两种光照配置",
        &unlit_dim_pixels,
        &unlit_bright_pixels,
    );
    assert_pixels_differ("Unlit 与 Lit", &unlit_bright_pixels, &lit_bright_pixels);
}

/// 同一渲染器上经 `MaterialStore::get_mut` 把材质从 Lit 切到 Unlit，
/// 下一帧必须重建绑定组并改变画面（材质修订号驱动失效）。
#[test]
fn material_revision_rebuilds_bind_group_at_runtime() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    let mut world = base_world();
    world.spawn((
        Transform::from_xyz(0.0, 0.8, 2.5),
        PointLight::new(Vec3::ONE, 60.0, 20.0),
    ));
    let material = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.3, 0.5, 0.8, 1.0));
    spawn_cube(&mut world, Vec3::new(0.0, 0.8, 0.0), material, None);
    propagate_transforms(&mut world);

    renderer.render(&world);
    let lit = renderer.read_pixels();

    world
        .resource_mut::<MaterialStore>()
        .get_mut(material)
        .expect("存活材质句柄")
        .extensions
        .shading = ShadingMode::Unlit;
    // 扩展切换必须让目录修订号推进；渲染器据此重建该材质的绑定组。
    assert!(
        world
            .resource::<MaterialStore>()
            .revision(material)
            .is_some()
    );
    renderer.render(&world);
    let unlit = renderer.read_pixels();

    assert_pixels_differ("运行期 Lit 切换为 Unlit", &lit, &unlit);
}
