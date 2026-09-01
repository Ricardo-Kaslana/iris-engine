//! 示例：变换层级（行星系）+ 彩色点光。
//!
//! 层级链：太阳 → 地球 → 月球。旋转太阳实体时，挂在它下面的地球随之公转；
//! 旋转地球时，月球叠加两种运动 —— 这就是 `add_child` + `GlobalTransform` 传播。
//! 太阳中心放一个暖色点光，场景一角放一个紫色点光，展示多光源照明。
//!
//! ```sh
//! cargo run --example orbit --release
//! ```

use iris_engine::prelude::*;

fn main() {
    env_logger::init();
    App::new()
        .add_plugins(DefaultPlugins::new("Iris · 层级与点光（行星系）").with_size(1600, 900))
        .add_system(Stage::Startup, setup)
        .add_system(Stage::Update, spin)
        .run();
}

/// 匀速自转（弧度/秒）。父实体旋转会带动全部后代公转。
#[derive(Component)]
struct Angular(f32);

fn setup(world: &mut World) {
    let sphere = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, 32, 20));

    // 太阳：自发光球，旋转时带动行星公转
    let mat_sun = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [1.0, 0.85, 0.35, 1.0],
        emissive: [1.6, 1.2, 0.4, 1.0],
        metallic: 0.0,
        roughness: 0.5,
        texture: TextureHandle(0),
    });
    let mut sun_transform = Transform::IDENTITY;
    sun_transform.scale = Vec3::splat(2.0);
    let sun = world.spawn((sun_transform, sphere, mat_sun, Angular(0.15)));

    // 地球：挂在太阳下，本地 +X 偏移 —— 太阳旋转使它公转
    let mat_earth = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.2, 0.5, 0.95, 1.0));
    let mut earth_transform = Transform::from_xyz(7.0, 0.0, 0.0);
    earth_transform.scale = Vec3::splat(0.8);
    let earth = world.spawn((earth_transform, sphere, mat_earth, Angular(0.9)));
    world.add_child(sun, earth);

    // 月球：挂在地球下 —— 公转 + 地球自转的双重叠加
    let mat_moon = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.75, 0.75, 0.78, 1.0));
    let mut moon_transform = Transform::from_xyz(2.2, 0.0, 0.0);
    moon_transform.scale = Vec3::splat(0.28);
    let moon = world.spawn((moon_transform, sphere, mat_moon));
    world.add_child(earth, moon);

    // 太阳中心的暖色点光 + 场景一角的紫色点光
    world.spawn((
        Transform::IDENTITY,
        PointLight::new(Vec3::new(1.0, 0.85, 0.6), 30.0, 40.0),
    ));
    world.spawn((
        Transform::from_xyz(-9.0, 3.0, -6.0),
        PointLight::new(Vec3::new(0.7, 0.3, 1.0), 20.0, 20.0),
    ));

    // 地面
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(60.0));
    let mat_ground = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.22, 0.24, 0.3, 1.0));
    world.spawn((Transform::from_xyz(0.0, -2.0, 0.0), plane, mat_ground));

    // 相机拉远看全景
    let mut camera = world.resource_mut::<Camera>();
    camera.eye = Vec3::new(0.0, 7.0, 18.0);
    camera.target = Vec3::new(0.0, 0.0, 0.0);
}

fn spin(world: &mut World) {
    let dt = world.resource::<Time>().delta_secs();
    world.query::<(&mut Transform, &Angular)>(|(t, a)| {
        t.rotation = Quat::from_axis_angle(Vec3::Y, a.0 * dt) * t.rotation;
    });
}
