//! 集成测试：点光 cubemap 阴影（6 面捕获 + Cube 采样）。
//!
//! 三个层次：
//! 1. 面矩阵数学：六个轴向点落入对应面 NDC 盒、面间对角落入某面
//!    （证明 6 个 90° 锥并集覆盖全方向）；
//! 2. 径向阴影差分：开/关 `PointLight.shadows` 整帧差分应表现为
//!    「成片像素变暗、几乎没有像素变亮、大部分不变」（照 shadow_render 模式）；
//! 3. 面方向一致性：偏置遮挡物的阴影必须落在解析预测的位置——
//!    面矩阵若与 GPU Cube 采样约定不一致（镜像/旋转），阴影会错位或消失；
//! 4. 空跑不污染：无遮挡物时开关阴影，整帧几乎不变。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 径向阴影差分场景：灯 (0,3,0)、遮挡方块 (0,2,0)（灯与地面之间）、
/// 地面 plane(30)、相机 (4,2.5,4) 看 (0,0.5,0)。
fn radial_scene(shadows: bool) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(4.0, 2.5, 4.0),
        target: Vec3::new(0.0, 0.5, 0.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight::default());

    // 遮挡方块（亮灰，材质仅用于区分几何与阴影区）
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.75, 0.75, 0.75, 1.0));
    world.spawn((Transform::from_xyz(0.0, 2.0, 0.0), cube, mat));

    // 地面
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(30.0));
    let mat_ground = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.5, 0.5, 0.5, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_ground));

    // 投影点光：悬在方块正上方
    world.spawn((
        Transform::from_xyz(0.0, 3.0, 0.0),
        PointLight {
            shadows,
            ..PointLight::default()
        },
    ));

    propagate_transforms(&mut world);
    world
}

/// 面方向一致性场景：遮挡方块偏置到灯的 +Z 一侧（0,2.2,1.5），
/// 阴影必然落在地面 z≈5.6 处而非灯正下方——错位的面约定无处遁形。
fn orientation_scene(shadows: bool) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 1.0, 8.5),
        target: Vec3::new(0.0, 0.9, 0.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight::default());

    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.75, 0.75, 0.75, 1.0));
    world.spawn((Transform::from_xyz(0.0, 2.2, 1.5), cube, mat));

    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(30.0));
    let mat_ground = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.5, 0.5, 0.5, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_ground));

    world.spawn((
        Transform::from_xyz(0.0, 3.0, 0.0),
        PointLight {
            shadows,
            ..PointLight::default()
        },
    ));

    propagate_transforms(&mut world);
    world
}

/// 无遮挡物场景（灯 + 地面）：验证阴影 pass 空跑不污染。
fn empty_scene(shadows: bool) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(4.0, 2.5, 4.0),
        target: Vec3::new(0.0, 0.5, 0.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight::default());

    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(30.0));
    let mat_ground = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.5, 0.5, 0.5, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_ground));

    world.spawn((
        Transform::from_xyz(0.0, 3.0, 0.0),
        PointLight {
            shadows,
            ..PointLight::default()
        },
    ));

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

/// 差分统计：返回 (变暗数, 变亮数, 不变数, 变暗掩码)。
fn diff_pixels(on: &[u8], off: &[u8], width: u32, height: u32) -> (u32, u32, u32, Vec<bool>) {
    assert_eq!(on.len(), off.len());
    let mut darkened = 0u32;
    let mut brightened = 0u32;
    let mut unchanged = 0u32;
    let mut mask = Vec::with_capacity((width * height) as usize);
    for y in 0..height as usize {
        for x in 0..width as usize {
            let i = (y * width as usize + x) * 4;
            let sum_on = on[i] as i32 + on[i + 1] as i32 + on[i + 2] as i32;
            let sum_off = off[i] as i32 + off[i + 1] as i32 + off[i + 2] as i32;
            if sum_on < sum_off - 12 {
                darkened += 1;
                mask.push(true);
            } else if sum_on > sum_off + 12 {
                brightened += 1;
                mask.push(false);
            } else {
                unchanged += 1;
                mask.push(false);
            }
        }
    }
    (darkened, brightened, unchanged, mask)
}

#[test]
fn face_matrices_cover_all_directions() {
    let light = PointLight {
        range: 12.0,
        ..PointLight::default()
    };
    // 灯位置任意化：矩阵必须以灯为原点工作，而非世界原点
    let light_pos = Vec3::new(3.0, -2.0, 4.0);

    // 六个轴向方向的点（5×单位轴）必须落入对应面的 NDC 盒
    for (face, dir) in POINT_SHADOW_FACES.iter().enumerate() {
        let point = light_pos + dir.0 * 5.0;
        let vp = PointLight::face_view_proj(light_pos, face, light.range);
        let clip = vp * point.extend(1.0);
        let ndc = clip.truncate() / clip.w;
        assert!(
            ndc.x.abs() <= 1.0 + 1e-4
                && ndc.y.abs() <= 1.0 + 1e-4
                && (-1e-4..=1.0 + 1e-4).contains(&ndc.z),
            "轴向点 {point:?} 应落在面 {face} 的 NDC 盒内，实际 {ndc:?}"
        );
    }

    // 面间对角方向（如 (1,1,1) 归一化）必须落在某个面的 NDC 盒内：
    // 证明 6 个 90° 锥的并集覆盖全方向（对角方向恰在面盒的角点）
    let diagonals = [
        Vec3::new(1.0, 1.0, 1.0),
        Vec3::new(1.0, 1.0, -1.0),
        Vec3::new(1.0, -1.0, 1.0),
        Vec3::new(-1.0, 1.0, 1.0),
        Vec3::new(1.0, -1.0, -1.0),
        Vec3::new(-1.0, -1.0, 1.0),
        Vec3::new(-1.0, 1.0, -1.0),
        Vec3::new(-1.0, -1.0, -1.0),
    ];
    for d in diagonals {
        let point = light_pos + d.normalize() * 5.0;
        let covered = (0..6).any(|face| {
            let vp = PointLight::face_view_proj(light_pos, face, light.range);
            let clip = vp * point.extend(1.0);
            let ndc = clip.truncate() / clip.w;
            ndc.x.abs() <= 1.0 + 1e-4
                && ndc.y.abs() <= 1.0 + 1e-4
                && (-1e-4..=1.0 + 1e-4).contains(&ndc.z)
        });
        assert!(covered, "对角方向 {d:?} 未被任何面覆盖");
    }
}

#[test]
fn point_shadow_darkens_ground_without_brightening_anything() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    // 开阴影
    let world = radial_scene(true);
    renderer.render(&world);
    let with_shadows = renderer.read_pixels();

    // 关阴影
    let world = radial_scene(false);
    renderer.render(&world);
    let without_shadows = renderer.read_pixels();

    let (w, h) = renderer.size();
    let (darkened, brightened, unchanged, _) = diff_pixels(&with_shadows, &without_shadows, w, h);

    // 方块悬在灯与地面之间（-Y 面覆盖正下方），地面阴影必然成片变暗
    assert!(
        darkened > 30,
        "开点光阴影后应有成片像素变暗（地面阴影），实际 {darkened}"
    );
    assert!(
        brightened <= 4,
        "关->开点光阴影不应让任何像素显著变亮（MSAA 边缘容差 4 像素），实际 {brightened}"
    );
    let total_pixels = ((w * h) as f32) * 0.9;
    assert!(
        unchanged as f32 > total_pixels * 0.5,
        "大部分画面（远处地面/背景）不应受点光阴影影响，实际 {unchanged}"
    );
}

#[test]
fn face_orientation_matches_cube_sampling() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let (w, h) = renderer.size();
    let camera = Camera {
        eye: Vec3::new(0.0, 1.0, 8.5),
        target: Vec3::new(0.0, 0.9, 0.0),
        ..Camera::default()
    };

    // 阴影解析中心：灯 (0,3,0) 穿过遮挡方块中心 (0,2.2,1.5) 的射线
    // 与地面 y=0 的交点 = (0, 0, 5.625)
    let expected = Vec3::new(0.0, 0.0, 5.625);
    let (px, py) = project_to_pixel(expected, &camera, w, h);
    let radius = (h as f32 * 0.22).min(w as f32 * 0.3);

    let world = orientation_scene(true);
    renderer.render(&world);
    let with_shadows = renderer.read_pixels();
    let world = orientation_scene(false);
    renderer.render(&world);
    let without_shadows = renderer.read_pixels();

    let (darkened, brightened, _, mask) = diff_pixels(&with_shadows, &without_shadows, w, h);
    assert!(darkened > 30, "偏置遮挡物的阴影应成片变暗，实际 {darkened}");
    assert!(brightened <= 4, "不应有像素显著变亮，实际 {brightened}");

    // 变暗像素应聚集在预测位置附近：面矩阵若与 GPU Cube 采样约定不一致
    //（镜像/旋转），阴影会落到别处，此窗口内几乎无变暗像素
    let mut in_window = 0u32;
    for yy in 0..h as usize {
        for xx in 0..w as usize {
            if !mask[yy * w as usize + xx] {
                continue;
            }
            let dx = xx as f32 + 0.5 - px;
            let dy = yy as f32 + 0.5 - py;
            if dx * dx + dy * dy <= radius * radius {
                in_window += 1;
            }
        }
    }
    assert!(
        in_window > 30,
        "阴影应落在解析预测位置 ({px:.0},{py:.0}) 附近（半径 {radius:.0}px），\
         实际窗口内变暗 {in_window}/{darkened}——面方向与 Cube 采样约定可能不一致"
    );
}

#[test]
fn disabled_shadow_light_pollutes_nothing() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    // 无遮挡物：开阴影（6 个捕获 pass 空跑，贴图全为清空值 1.0 = 无遮挡）
    let world = empty_scene(true);
    renderer.render(&world);
    let with_pass = renderer.read_pixels();

    // 关阴影（捕获 pass 完全跳过）
    let world = empty_scene(false);
    renderer.render(&world);
    let without_pass = renderer.read_pixels();

    let (w, h) = renderer.size();
    let (darkened, brightened, _, _) = diff_pixels(&with_pass, &without_pass, w, h);
    let changed = darkened + brightened;
    assert!(
        changed < 5,
        "无遮挡物时开关点光阴影应几乎无差异（防止阴影 pass 空跑污染），实际 {changed} 像素变化"
    );
}
