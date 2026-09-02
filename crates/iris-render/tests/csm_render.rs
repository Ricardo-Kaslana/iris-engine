//! 集成测试：方向光级联阴影 CSM v2（R29，3 级）。
//!
//! 大场景回归（~80 m 跨度）：近 / 远物体各一颗 + 一面地面阴影带横跨
//! 级联 0→1 边界的侧墙。断言：
//! 1. 近处（级联 0）与远处（级联 2）物体脚下都有阴影（像素差分证据）；
//! 2. 级联边界两侧的墙面阴影带都存在（无可见缝——若边界处丢影，靠边界
//!    一侧的窗口将几乎无变暗像素）。
//!
//! 场景几何（默认光方向 (−0.5,−1,−0.4)，高 h 的点影子向 (−0.5,−0.4)·h
//! 平移）与相机 eye (0,5,40) → target (0,0,−15)（fov 45°、正方形视口
//! aspect 1、z_far 1000 被钳制到覆盖距离 120 m）：分割点
//! splits ≈ [24.5, 52.5, 120]（PFS λ=0.6），沿视线深度的地面参照点——
//! 近立方体 ≈ 10 m（级联 0）、墙面阴影带 23→26 m（横跨边界 24.5 m，
//! 边界在地面 z ≈ 15.9）、远立方体 ≈ 81 m（级联 2）。
//! 侧墙放在 x ∈ [5,11]：其阴影带 x ∈ [3.5,11]，探头取 x = 4.2——
//! 从相机的视线 x < 5，不被墙体自身遮挡。
//!
//! 容差说明：窗口半径 8 px（≈0.3–2.1 m，按各处像素密度取值，覆盖 PCF
//! 软边与 MSAA 边缘抖动）；变暗阈值与既有阴影测试一致（逐像素 RGB 和
//! 差 > 12/765）；每窗口计数阈值留 3 倍以上余量。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 大场景（~80 m 跨度）：近 / 远排开 + 阴影带横跨级联边界的侧墙 + 大地面。
fn big_scene(shadows: bool) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 5.0, 40.0),
        target: Vec3::new(0.0, 0.0, -15.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        shadows,
        ..DirectionalLight::default()
    });

    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.85, 0.3, 0.2, 1.0));
    // 近处物体：视线深度 ~10 m → 级联 0；地面阴影 x ∈ [−3,0], z ∈ [27.3,30.3]
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 1.5, 30.0),
            scale: Vec3::splat(3.0),
            ..Transform::IDENTITY
        },
        cube,
        mat,
    ));
    // 侧墙（x ∈ [5,11], z ∈ [8,22]，高 3）：视线深度 18→32 m，横跨级联
    // 0/1 边界；地面阴影带 x ∈ [3.5,11], z ∈ [6.8,22]
    let mat_wall = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.4, 0.5, 0.7, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(8.0, 1.5, 15.0),
            scale: Vec3::new(6.0, 3.0, 14.0),
            ..Transform::IDENTITY
        },
        cube,
        mat_wall,
    ));
    // 远处物体：视线深度 ~81 m → 级联 2。放大到 6 m（该距离仅 ~3.8 px/m，
    // 保证地面阴影带在屏幕上有足够的像素证据）；阴影带 x ∈ [−6,−3] 露出
    // 于轮廓左侧（更靠内被立方体自身遮挡）
    let mat_far = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.2, 0.7, 0.4, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 3.0, -40.0),
            scale: Vec3::splat(6.0),
            ..Transform::IDENTITY
        },
        cube,
        mat_far,
    ));
    // 大地面（±120 m，覆盖全部级联足印）
    let plane = world
        .resource_mut::<MeshStore>()
        .add(MeshData::plane(240.0));
    let mat_ground = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.5, 0.5, 0.5, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_ground));

    propagate_transforms(&mut world);
    world
}

/// 世界点投影到像素坐标（与 Camera 的 DirectX 深度约定一致的 CPU 复算）。
fn project_to_pixel(point: Vec3, camera: &Camera, width: u32, height: u32) -> (f32, f32) {
    let vp = camera.view_proj(width as f32 / height as f32);
    let clip = vp * point.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    (
        (ndc.x * 0.5 + 0.5) * width as f32,
        (1.0 - (ndc.y * 0.5 + 0.5)) * height as f32,
    )
}

/// 差分统计：返回 (变暗数, 变亮数)。
fn diff_counts(on: &[u8], off: &[u8]) -> (u32, u32) {
    assert_eq!(on.len(), off.len());
    let mut darkened = 0u32;
    let mut brightened = 0u32;
    for i in (0..on.len()).step_by(4) {
        let sum_on = on[i] as i32 + on[i + 1] as i32 + on[i + 2] as i32;
        let sum_off = off[i] as i32 + off[i + 1] as i32 + off[i + 2] as i32;
        if sum_on < sum_off - 12 {
            darkened += 1;
        } else if sum_on > sum_off + 12 {
            brightened += 1;
        }
    }
    (darkened, brightened)
}

/// 窗口内变暗像素计数（差分按坐标统计）。
fn darkened_in_window(on: &[u8], off: &[u8], w: u32, h: u32, px: f32, py: f32, radius: f32) -> u32 {
    let mut count = 0u32;
    for y in 0..h as usize {
        for x in 0..w as usize {
            let i = (y * w as usize + x) * 4;
            let sum_on = on[i] as i32 + on[i + 1] as i32 + on[i + 2] as i32;
            let sum_off = off[i] as i32 + off[i + 1] as i32 + off[i + 2] as i32;
            if sum_on >= sum_off - 12 {
                continue;
            }
            let dx = x as f32 + 0.5 - px;
            let dy = y as f32 + 0.5 - py;
            if dx * dx + dy * dy <= radius * radius {
                count += 1;
            }
        }
    }
    count
}

/// 开 / 关阴影各渲染一帧，返回 (开帧像素, 关帧像素, 整帧差分)。
fn render_pair() -> (Vec<u8>, Vec<u8>, u32, u32) {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return (Vec::new(), Vec::new(), 0, 0);
    };
    let world = big_scene(true);
    renderer.render(&world);
    let with_shadows = renderer.read_pixels();
    let world = big_scene(false);
    renderer.render(&world);
    let without_shadows = renderer.read_pixels();
    let (darkened, brightened) = diff_counts(&with_shadows, &without_shadows);
    (with_shadows, without_shadows, darkened, brightened)
}

/// 近处（级联 0）与远处（级联 2）物体脚下都应有阴影：远处物体脚下带
/// 像素证据是「远处无影」被修好的直接判据。
#[test]
fn csm_shadows_reach_near_and_far_cascades() {
    let (with_shadows, without_shadows, darkened, brightened) = render_pair();
    if with_shadows.is_empty() {
        return; // 无 GPU 环境已打印跳过说明
    }
    let (w, h) = (256u32, 256u32);
    let camera = Camera {
        eye: Vec3::new(0.0, 5.0, 40.0),
        target: Vec3::new(0.0, 0.0, -15.0),
        ..Camera::default()
    };
    assert!(darkened > 100, "整帧应有成片阴影像素，实际 {darkened}");
    assert!(
        brightened <= 4,
        "关->开阴影不应让任何像素显著变亮（MSAA 边缘容差 4 像素），实际 {brightened}"
    );

    // 近处物体的地面阴影（级联 0，深度 ~12 m）。取立方体轮廓左侧的露出
    // 部分（x ≈ −2.6）：更靠内的地面影子被立方体自身挡住（相机视线不过顶）。
    let (px, py) = project_to_pixel(Vec3::new(-2.6, 0.0, 28.5), &camera, w, h);
    let near_count = darkened_in_window(&with_shadows, &without_shadows, w, h, px, py, 8.0);
    assert!(
        near_count > 30,
        "近处物体（级联 0）脚下应有阴影：窗口 ({px:.0},{py:.0}) 内变暗 {near_count}"
    );

    // 远处物体的地面阴影（级联 2，深度 ~81 m；取轮廓左侧露出部分）
    let (px, py) = project_to_pixel(Vec3::new(-4.5, 0.0, -41.0), &camera, w, h);
    let far_count = darkened_in_window(&with_shadows, &without_shadows, w, h, px, py, 8.0);
    assert!(
        far_count > 10,
        "远处物体（级联 2，~81 m）脚下应有阴影：窗口 ({px:.0},{py:.0}) 内变暗 {far_count}"
    );
}

/// 级联边界（地面 z ≈ 15.9 / 视线深度 ≈ 24.5 m）两侧的墙面阴影带都存在
/// ——若级联切换处丢影（接缝），靠边界一侧的窗口将几乎无变暗像素。
/// 两窗口各距边界 ~1.6–1.9 m（≈20 px，远大于 PCF/MSAA 的像素级抖动）。
#[test]
fn csm_boundary_shadow_is_continuous_across_cascades() {
    let (with_shadows, without_shadows, _, brightened) = render_pair();
    if with_shadows.is_empty() {
        return; // 无 GPU 环境已打印跳过说明
    }
    assert!(
        brightened <= 4,
        "关->开阴影不应让任何像素显著变亮，实际 {brightened}"
    );
    let (w, h) = (256u32, 256u32);
    let camera = Camera {
        eye: Vec3::new(0.0, 5.0, 40.0),
        target: Vec3::new(0.0, 0.0, -15.0),
        ..Camera::default()
    };
    // 边界两侧对称取样（都在侧墙的地面阴影带 x ∈ [3.5,11], z ∈ [6.8,22] 内，
    // 且视线 x < 5 不被墙体遮挡）：
    // z = 17.5 → 深度 ≈ 22.9 m（级联 0 侧）；z = 14 → 深度 ≈ 26.3 m（级联 1 侧）
    let (px, py) = project_to_pixel(Vec3::new(4.2, 0.0, 17.5), &camera, w, h);
    let c0_count = darkened_in_window(&with_shadows, &without_shadows, w, h, px, py, 8.0);
    assert!(
        c0_count > 30,
        "级联边界近侧（级联 0，z=17.5）墙面阴影应存在：窗口 ({px:.0},{py:.0}) 内变暗 {c0_count}"
    );
    let (px, py) = project_to_pixel(Vec3::new(4.2, 0.0, 14.0), &camera, w, h);
    let c1_count = darkened_in_window(&with_shadows, &without_shadows, w, h, px, py, 8.0);
    assert!(
        c1_count > 30,
        "级联边界远侧（级联 1，z=14）墙面阴影应存在（边界无缝）：窗口 ({px:.0},{py:.0}) 内变暗 {c1_count}"
    );
}
