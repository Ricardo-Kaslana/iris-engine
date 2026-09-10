//! 集成测试：法线 / ORM 材质通道（R42）。
//!
//! 两条主线：
//! 1. **兼容锁定（硬约束）**：材质不带 normal / ORM 槽位时，渲染结果与
//!    R41（引入本通道前）**逐位一致**——对固定场景的全帧像素做 FNV-1a
//!    哈希，与 R41 代码（改动落地前）实跑捕获的基线值比对。哈希改变的
//!    任何一位都意味着中性纹理路径被破坏（论证链见 `material.rs` 模块
//!    文档：16-bit float 中性法线 = (0.5,0.5,1) 采样 *2−1 恰为 (0,0,1)、
//!    TBN 第三列取未归一化插值法线、中性 ORM 采样恒 1.0）。
//! 2. **通道生效**：同一砖墙材质挂上法线 + ORM 后渲染结果必须显著
//!    改变（侧光下砖缝凹凸 / AO / 粗糙度差异），且同输入两次渲染
//!    **确定性逐位一致**。
//!
//! 环境：无 GPU 适配器时优雅跳过（ OffscreenRenderer 返回 None）。

use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// R41 基线全帧像素 FNV-1a 64（改动落地前以同一场景实跑捕获；
/// 场景 = 地面 + 砖色立方体 + 金属圆柱 + 默认 IBL / 阴影 / 侧光，
/// 320×180，见 `r41_scene_world`）。
///
/// **R61 重锁**：0x609b_2457_f4d5_cd82 → 0xe51d_b127_6079_ce39。
/// 归因证据（阴影 v2 的坡度缩放 bias + 法线偏移改变了阴影开启帧的
/// 采样口径）：同场景 `DirectionalLight.shadows = false` 的对照组在
/// R61 前后**逐位一致**（0x6a56_af90_bf4c_6686，见
/// `tests/shadow_v2_render.rs::shadow_off_control_group_is_bit_identical_to_pre_r61`），
/// 且 `Renderer::set_legacy_shadow_bias(true)` 逐位复现 R60 输出
/// （`legacy_bias_mode_reproduces_r60_output_bitwise`）——本值的变化
/// **只**来自阴影偏移口径，中性纹理路径未动（§4.7.2 / §8.8 的论证链不变）。
const R41_BASELINE_HASH: u64 = 0xe51d_b127_6079_ce39;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 与 R41 基线捕获完全一致的固定场景（无 normal / ORM 槽位材质）。
fn r41_scene_world() -> World {
    use glam::{Quat, Vec3};
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 2.2, 6.0),
        target: Vec3::new(0.0, 1.0, 0.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        intensity: 1.2,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let cyl = world
        .resource_mut::<MeshStore>()
        .add(MeshData::cylinder(0.6, 2.0, 24));
    let mut mats = world.resource_mut::<MaterialStore>();
    let wall = mats.add(MaterialDef::solid(0.65, 0.55, 0.45, 1.0));
    let metal = mats.add(MaterialDef::metal(0.9, 0.85, 0.8, 0.35));
    drop(mats);
    world.spawn((Transform::IDENTITY, plane, wall));
    world.spawn((
        Transform {
            position: Vec3::new(-2.0, 1.0, -1.5),
            ..Transform::IDENTITY
        },
        cube,
        wall,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(1.5, 1.0, -1.0),
            rotation: Quat::IDENTITY,
            ..Transform::IDENTITY
        },
        cyl,
        metal,
    ));
    propagate_transforms(&mut world);
    world
}

/// 硬约束锁定：无 normal / ORM 的材质渲染结果与 R41 基线逐位一致
/// （全帧像素哈希相等——比逐字节比对更抗测试代码本身的漂移）。
#[test]
fn materials_without_normal_orm_match_r41_baseline_bitwise() {
    let Some(mut renderer) = OffscreenRenderer::new(320, 180) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let world = r41_scene_world();
    renderer.render(&world);
    let pixels = renderer.read_pixels();
    assert_eq!(pixels.len(), 320 * 180 * 4, "全帧像素尺寸必须一致");
    let hash = fnv1a(&pixels);
    assert_eq!(
        hash, R41_BASELINE_HASH,
        "无贴图材质的渲染结果必须与 R41 基线逐位一致（哈希 {hash:#018x} ≠ 基线 \
         {R41_BASELINE_HASH:#018x}）——中性纹理路径被破坏，见 material.rs 论证链"
    );
}

/// 两面同色砖墙。断言：
/// 1. 场景可见（画面显著异于天空背景）；
/// 2. 通道生效——同一面墙挂上法线 + ORM 前后（两次渲染）的墙面核心区
///    像素显著不同（侧光下法线凹凸 + AO + 粗糙度共同作用；若中性纹理
///    被误绑或采样恒等，两次渲染该区域逐像素相同）；
/// 3. 确定性——同输入两次渲染逐位一致（确定性文化：同输入两次跑）。
#[test]
fn normal_orm_channels_change_rendering_deterministically() {
    use glam::Vec3;
    let Some(mut renderer) = OffscreenRenderer::new(480, 270) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    // `with_channels`：右墙是否挂法线 + ORM（左墙恒标量，作对照）
    let build_world = |with_channels: bool| {
        let mut world = World::new();
        world.insert_resource(MeshStore::new());
        world.insert_resource(MaterialStore::new());
        world.insert_resource(TextureStore::new());
        world.insert_resource(Camera {
            eye: Vec3::new(0.0, 1.3, 4.0),
            target: Vec3::new(0.0, 1.0, -2.0),
            ..Camera::default()
        });
        // 侧光：砖缝法线扰动差异最大化
        world.insert_resource(DirectionalLight {
            direction: Vec3::new(0.55, -0.45, 0.7).normalize(),
            intensity: 1.6,
            ambient: Vec3::new(0.18, 0.19, 0.22),
            ..DirectionalLight::default()
        });
        world.insert_resource(BloomSettings::disabled());
        let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
        let mut textures = world.resource_mut::<TextureStore>();
        let (normal_tex, orm_tex) = if with_channels {
            let (n, o) = CpuTexture::brick_normal_orm(6, 3, 512, 0.09, 2.2);
            (textures.add(n), textures.add(o))
        } else {
            (TextureHandle(0), TextureHandle(0))
        };
        drop(textures);
        let mut mats = world.resource_mut::<MaterialStore>();
        let wall_color = [0.72f32, 0.52, 0.40, 1.0];
        let plain = mats.add(MaterialDef {
            base_color: wall_color,
            roughness: 0.7,
            ..Default::default()
        });
        let pbr = mats.add(
            MaterialDef {
                base_color: wall_color,
                roughness: 0.7,
                ..Default::default()
            }
            .with_normal(normal_tex)
            .with_orm(orm_tex),
        );
        drop(mats);
        let right_mat = if with_channels { pbr } else { plain };
        world.spawn((
            Transform {
                position: Vec3::new(-1.5, 1.0, -2.0),
                scale: Vec3::new(2.8, 2.0, 0.2),
                ..Transform::IDENTITY
            },
            cube,
            plain,
        ));
        world.spawn((
            Transform {
                position: Vec3::new(1.5, 1.0, -2.0),
                scale: Vec3::new(2.8, 2.0, 0.2),
                ..Transform::IDENTITY
            },
            cube,
            right_mat,
        ));
        propagate_transforms(&mut world);
        world
    };

    let world_plain = build_world(false);
    renderer.render(&world_plain);
    let (w, h) = renderer.size();
    let px_plain = renderer.read_pixels();

    // 1. 场景可见：天空角落之外的像素存在
    let sky = [px_plain[0], px_plain[1], px_plain[2]];
    let scene_px = (0..(w * h) as usize)
        .filter(|&i| (0..3).any(|c| (px_plain[i * 4 + c] as i32 - sky[c] as i32).abs() > 12))
        .count();
    assert!(
        scene_px > 1000,
        "砖墙必须出现在画面中（场景像素 {scene_px}）"
    );

    // 2. 通道生效：右墙核心区（[0.58w, 0.75w] × 垂直中段）在挂通道前后
    //    平均绝对差显著非零
    let wall_diff = |px2: &[u8]| -> f64 {
        let mut total = 0f64;
        let mut n = 0u64;
        for y in (h / 4)..(3 * h / 4) {
            for x in (w * 58 / 100)..(w * 75 / 100) {
                let i = (y * w + x) as usize;
                for c in 0..3 {
                    total += (px_plain[i * 4 + c] as i32 - px2[i * 4 + c] as i32).abs() as f64;
                }
                n += 1;
            }
        }
        total / (n * 3) as f64
    };
    let world_pbr = build_world(true);
    renderer.render(&world_pbr);
    let px_pbr = renderer.read_pixels();
    let diff = wall_diff(&px_pbr);
    assert!(
        diff > 6.0,
        "挂法线 + ORM 前后的墙面平均差 {diff:.2} 应显著非零（/255）——\
         两通道未生效或中性纹理被误绑"
    );

    // 3. 确定性：重建同一世界再渲一次，逐位一致
    let world_pbr2 = build_world(true);
    renderer.render(&world_pbr2);
    let px_pbr2 = renderer.read_pixels();
    assert_eq!(px_pbr, px_pbr2, "同输入两次渲染必须逐位一致（确定性）");
}
