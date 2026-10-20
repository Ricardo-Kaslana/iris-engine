//! 动态资源生命周期 GPU 回归：同句柄更新与删除后的像素与缓存行为。
//!
//! 覆盖 `MeshStore` 与 `TextureStore` 的 `update` 与 `remove` 端到端
//! 路径：渲染器在帧首按目录纪元与槽位修订号驱逐陈旧 GPU 缓存，纹理变化
//! 连带失效材质绑定组。对照口径与既有 GPU 套件一致：最终画面必须与“直接
//! 用新内容新建的世界”逐字节一致，且重复更新不得让缓存条目增长。
//!
//! 重负载 GPU 套件同文件多测试并行会争用单适配器，因此文件内互斥串行。
//! 无 GPU 适配器时打印跳过并返回。

use std::sync::Mutex;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Entity, Transform, World};
use iris_render::prelude::*;

/// 同文件 GPU 测试串行锁（参照 `ssr_render.rs::GPU_SERIAL`）。
static GPU_SERIAL: Mutex<()> = Mutex::new(());

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;

/// 逐字节相等断言（失败时只打印首个差异，避免整帧像素刷屏）。
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

/// 逐字节不等断言（画面必须可判别地不同）。
fn assert_pixels_differ(label: &str, a: &[u8], b: &[u8]) {
    assert!(a != b, "{label}：画面应不同但逐字节一致");
}

/// 固定相机、方向光与关闭泛光的基础世界。
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

fn spawn_cube(world: &mut World, mesh: MeshHandle, material: MaterialHandle) -> Entity {
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 0.8, 0.0),
            ..Transform::IDENTITY
        },
        mesh,
        material,
    ))
}

/// 纵向拉伸的立方体：同一句柄更新后画面必须可判别地变化。
fn stretched_cube() -> MeshData {
    let mut data = MeshData::cube();
    for vertex in &mut data.vertices {
        vertex.position[1] *= 2.5;
    }
    data
}

fn render_to_pixels(world: &World) -> Vec<u8> {
    let mut renderer =
        OffscreenRenderer::new(WIDTH, HEIGHT).expect("本测试需要 GPU 适配器，环境无 GPU 时应跳过");
    renderer.render(world);
    renderer.read_pixels()
}

#[test]
fn mesh_update_at_same_handle_matches_fresh_world() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    let mut world = base_world();
    let material = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.9, 0.8, 0.7, 1.0));
    let mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    spawn_cube(&mut world, mesh, material);
    propagate_transforms(&mut world);

    renderer.render(&world);
    let before = renderer.read_pixels();
    assert_eq!(
        renderer.renderer().resource_cache_stats().mesh_entries,
        1,
        "首帧应只有业务网格一个缓存条目"
    );

    // 同句柄更新三次：每次都应与“新内容新建世界”逐字节一致，缓存不增长。
    let updates = [stretched_cube(), MeshData::cube(), stretched_cube()];
    for updated in updates {
        assert!(
            world.resource_mut::<MeshStore>().update(mesh, updated),
            "存活句柄更新必须命中"
        );
        renderer.render(&world);
        let after_update = renderer.read_pixels();
        assert_eq!(
            renderer.renderer().resource_cache_stats().mesh_entries,
            1,
            "同句柄更新不得累积 GPU 网格缓存"
        );

        let mut fresh = base_world();
        let fresh_material = fresh
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.9, 0.8, 0.7, 1.0));
        let fresh_mesh = fresh
            .resource_mut::<MeshStore>()
            .add(world.resource::<MeshStore>().get(mesh).unwrap().clone());
        spawn_cube(&mut fresh, fresh_mesh, fresh_material);
        propagate_transforms(&mut fresh);
        let fresh_pixels = render_to_pixels(&fresh);
        assert_pixels_equal("同句柄网格更新", &after_update, &fresh_pixels);
    }

    renderer.render(&world);
    let final_pixels = renderer.read_pixels();
    assert_pixels_differ("拉伸网格与原始立方体", &before, &final_pixels);
}

#[test]
fn texture_update_at_same_handle_matches_fresh_world() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    let mut world = base_world();
    let texture = world
        .resource_mut::<TextureStore>()
        .add(CpuTexture::solid([255, 0, 0, 255]));
    let material = world.resource_mut::<MaterialStore>().add(MaterialDef {
        texture,
        ..MaterialDef::solid(1.0, 1.0, 1.0, 1.0)
    });
    let mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    spawn_cube(&mut world, mesh, material);
    propagate_transforms(&mut world);

    renderer.render(&world);
    let red = renderer.read_pixels();
    assert_eq!(
        renderer.renderer().resource_cache_stats().texture_entries,
        2,
        "0 号白色兜底 + 业务纹理"
    );

    assert!(
        world
            .resource_mut::<TextureStore>()
            .update(texture, CpuTexture::solid([0, 0, 255, 255])),
        "存活纹理更新必须命中"
    );
    renderer.render(&world);
    let blue = renderer.read_pixels();
    assert_eq!(
        renderer.renderer().resource_cache_stats().texture_entries,
        2,
        "同句柄更新不得累积 GPU 纹理缓存"
    );

    let mut fresh = base_world();
    let fresh_texture = fresh
        .resource_mut::<TextureStore>()
        .add(CpuTexture::solid([0, 0, 255, 255]));
    let fresh_material = fresh.resource_mut::<MaterialStore>().add(MaterialDef {
        texture: fresh_texture,
        ..MaterialDef::solid(1.0, 1.0, 1.0, 1.0)
    });
    let fresh_mesh = fresh.resource_mut::<MeshStore>().add(MeshData::cube());
    spawn_cube(&mut fresh, fresh_mesh, fresh_material);
    propagate_transforms(&mut fresh);
    let fresh_pixels = render_to_pixels(&fresh);

    assert_pixels_equal("同句柄纹理更新", &blue, &fresh_pixels);
    assert_pixels_differ("红蓝纹理", &red, &blue);
}

#[test]
fn mesh_removal_evicts_cache_and_matches_empty_world() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    let mut world = base_world();
    let material = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.9, 0.8, 0.7, 1.0));
    let mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let entity = spawn_cube(&mut world, mesh, material);
    propagate_transforms(&mut world);
    renderer.render(&world);
    assert_eq!(renderer.renderer().resource_cache_stats().mesh_entries, 1);

    world.despawn(entity);
    assert!(
        world.resource_mut::<MeshStore>().remove(mesh).is_some(),
        "存活网格删除应返回内容"
    );
    renderer.render(&world);
    assert_eq!(
        renderer.renderer().resource_cache_stats().mesh_entries,
        0,
        "删除后的网格 GPU 缓存必须被驱逐"
    );

    let empty = base_world();
    let empty_pixels = render_to_pixels(&empty);
    assert_pixels_equal("删除网格后与空场景", &renderer.read_pixels(), &empty_pixels);
}

#[test]
fn texture_removal_evicts_cache_and_invalidates_material() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    let mut world = base_world();
    let texture = world
        .resource_mut::<TextureStore>()
        .add(CpuTexture::solid([255, 0, 0, 255]));
    let material = world.resource_mut::<MaterialStore>().add(MaterialDef {
        texture,
        ..MaterialDef::solid(1.0, 1.0, 1.0, 1.0)
    });
    let mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let entity = spawn_cube(&mut world, mesh, material);
    propagate_transforms(&mut world);
    renderer.render(&world);
    assert_eq!(
        renderer.renderer().resource_cache_stats().texture_entries,
        2
    );
    assert_eq!(
        renderer.renderer().resource_cache_stats().material_entries,
        1
    );

    world.despawn(entity);
    assert!(
        world
            .resource_mut::<TextureStore>()
            .remove(texture)
            .is_some(),
        "存活纹理删除应返回内容"
    );
    renderer.render(&world);
    let stats = renderer.renderer().resource_cache_stats();
    assert_eq!(stats.texture_entries, 1, "删除后只剩 0 号白色兜底");
    assert_eq!(
        stats.material_entries, 0,
        "引用被删纹理的材质绑定组必须失效"
    );

    let empty = base_world();
    let empty_pixels = render_to_pixels(&empty);
    assert_pixels_equal("删除纹理后与空场景", &renderer.read_pixels(), &empty_pixels);
}
