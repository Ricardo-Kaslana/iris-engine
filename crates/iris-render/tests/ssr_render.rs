//! 集成测试：SSR 屏幕空间反射（R62b，`standard.wgsl` 的 SSR 段）。
//!
//! 六条主线（与 ARCHITECTURE §4.31 的验收标准一一对应）：
//! 1. **兼容锁（硬约束）**：`SsrSettings` 缺席帧的全帧 FNV-1a 锁在 R53
//!    之后默认链（同 fxaa/taa 场景交叉验证）；显式 `enabled = false` 与
//!    缺席逐位一致；**开启首帧（上帧无效，整段跳过）与 SSR 关闭帧逐位
//!    一致**（重置语义 = 硬断言）；`FrameStats::ssr_on` 观测字段翻转。
//! 2. **有效性①（屏内标记物）**：金属地面反射屏内自发光标记——反射
//!    像素出现在**解析镜像位置**邻域（R62a 视差落点断言同款手法），
//!    SSR 关闭时该位置无标记色（env 只反射天空）。
//! 3. **有效性②（动态物体，本轮存在的理由）**：探针烘焙后标记从 A 移到
//!    B——SSR 反射跟随 B 的镜像位置；探针路径（SSR 关）只反映 A（烘焙
//!    时刻的内容），B 的镜像位置无标记色。探针做不到、SSR 做得到。
//! 4. **有效性③（粗糙度梯度）**：ORM 粗糙度沿 x 递增的金属地面 +
//!    全宽红色背景墙：SSR 开/关的逐列亮度差随粗糙度单调下降，早退阈值
//!    （0.6）之上的地面列**逐位等于关闭帧**（步进整段跳过的硬证据）。
//! 5. **有效性④（屏外回退）**：标记物不在上帧屏内 ⇒ 所有片元未命中 ⇒
//!    整帧与「探针路径（SSR 关）」**逐位一致**——回退 = 既有通路原值
//!    的最强形式。
//! 6. **确定性（上帧依赖 = 有状态）**：两个同输入完整会话（48 帧，含
//!    相机运动与物体自体运动）逐帧逐位一致；`reset_taa()`（切镜）后
//!    下一帧与 SSR 关闭同姿态帧逐位一致（SSR 跟随切镜重置，裁决 3）。
//!
//! 环境无 GPU 适配器时优雅跳过（打印提示，测试仍算通过）。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Entity, Transform, World};
use iris_render::prelude::*;

/// 屏幕尺寸（测试统一口径）。
const SIZE: u32 = 256;

/// R53 默认链兼容锁（fxaa_render / taa_render 同款：同场景缺席帧必逐位
/// 一致——SSR 缺席路径不得扰动整条链）。
const R53_FXAA_OFF_BASELINE: u64 = 0x24e3_8254_0222_7bbb;

/// SSR 场景的公共参数（相机 / 标记位置的解析几何都由这些常量导出）。
struct SsrScene {
    camera: Camera,
    /// 站立在地面上的自发光标记中心（A = 探针烘焙位置，B = 移动后位置）。
    marker_a: Vec3,
    marker_b: Vec3,
    marker_scale: Vec3,
}

fn ssr_scene() -> SsrScene {
    SsrScene {
        camera: Camera {
            eye: Vec3::new(0.0, 2.2, 3.0),
            target: Vec3::new(0.0, 0.2, 0.0),
            fov_y_radians: std::f32::consts::FRAC_PI_4,
            ..Camera::default()
        },
        // x = ±1.2：标记直接图像完整落在画面内（x ≈ 208±8，uv ≈ 0.81）
        // ——贴边（x=±1.8 时直接图像在 uv≈0.99）会让反射采样吃进边缘
        // AA 切边 + 屏幕边缘衰减带，反射色被冲淡且权重 ×0.1
        marker_a: Vec3::new(-1.2, 1.0, -1.5),
        marker_b: Vec3::new(1.2, 1.0, -1.5),
        marker_scale: Vec3::new(0.6, 2.0, 0.06),
    }
}

/// 开启态的 SSR 资源（Default 的 enabled = false——缺席 = 关语义，
/// 测试显式开启）。
fn ssr_enabled() -> SsrSettings {
    SsrSettings {
        enabled: true,
        ..SsrSettings::default()
    }
}

fn emissive(r: f32, g: f32, b: f32) -> MaterialDef {
    let mut mat = MaterialDef::solid(r, g, b, 1.0);
    // ×6 自发光 + roughness = 1（压掉粗糙镜面对天空的白色高光反射——
    // 否则标记本体读 [254,195,155]，红判定 r−g>80 抓不住）
    mat.emissive = [r * 6.0, g * 6.0, b * 6.0, 1.0];
    mat.roughness = 1.0;
    mat
}

fn base_world(camera: Camera) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(camera);
    // 直射光关闭：标记 = 纯自发光红、地面 = 纯环境反射——镜像位置出现
    // 红色只能来自 SSR 对标记的真实命中（排除地面太阳高光假命中的干扰，
    // 该伪影见 §8.38 诚实清单）
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.5, -1.0, 0.35).normalize(),
        intensity: 0.0,
        ambient: Vec3::new(0.02, 0.02, 0.03),
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    world
}

/// 镜像地面场景：金属地面 + 站立在 `marker_at` 的自发光标记 +
/// 可选探针实体。返回（world, 探针实体, 标记实体）。
fn mirror_world(
    scene: &SsrScene,
    marker_at: Option<Vec3>,
    with_probe: bool,
) -> (World, Option<Entity>, Option<Entity>) {
    let mut world = base_world(scene.camera.clone());
    let floor = world.resource_mut::<MeshStore>().add(MeshData::plane(12.0));
    let mirror = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::metal(0.85, 0.85, 0.88, 0.05));
    world.spawn((Transform::IDENTITY, floor, mirror));

    let slab = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let marker = marker_at.map(|at| {
        let red = world
            .resource_mut::<MaterialStore>()
            .add(emissive(1.0, 0.02, 0.0));
        world.spawn((
            Transform {
                position: at,
                scale: scene.marker_scale,
                ..Transform::IDENTITY
            },
            slab,
            red,
        ))
    });
    let probe = with_probe.then(|| {
        world.spawn((
            Transform::from_xyz(0.0, 1.475, 0.0),
            // falloff = 0：地面在盒内 0.05 m 处，二值权重下恒为 1（R62a 同款）
            ReflectionProbe::new(Vec3::new(3.0, 1.525, 3.0)).with_falloff(0.0),
        ))
    });
    propagate_transforms(&mut world);
    (world, probe, marker)
}

/// 全帧像素 FNV-1a 64（兼容锁哈希，与既有 GPU 测试同款）。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

fn pixel(px: &[u8], w: u32, x: u32, y: u32) -> [i32; 3] {
    let i = ((y * w + x) * 4) as usize;
    [px[i] as i32, px[i + 1] as i32, px[i + 2] as i32]
}

/// 世界坐标 → 像素坐标（与渲染器同一条相机路径：`view_matrix` +
/// DirectX 约定的透视投影；NDC y = +1 对应贴图第 0 行）。
fn project_to_pixel(camera: &Camera, p: Vec3, w: u32, h: u32) -> (f32, f32) {
    let aspect = w as f32 / h as f32;
    let view = camera.view_matrix();
    let proj = glam::camera::rh::proj::directx::perspective(
        camera.fov_y_radians,
        aspect,
        camera.z_near,
        camera.z_far,
    );
    let clip = proj * view * p.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    (
        (ndc.x * 0.5 + 0.5) * w as f32,
        (0.5 - ndc.y * 0.5) * h as f32,
    )
}

/// 解析镜像像素：标记中心关于地面（y = 0）的镜像点与相机连线同地面的
/// 交点——完美平面镜上标记的像就落在这里（R62a 视差测试同款推导）。
fn analytic_mirror_pixel(camera: &Camera, marker: Vec3, w: u32, h: u32) -> (f32, f32) {
    let mirrored = Vec3::new(marker.x, -marker.y, marker.z);
    let eye = camera.eye;
    let dir = mirrored - eye;
    let t = -eye.y / dir.y;
    let hit = eye + dir * t;
    assert!(
        hit.y.abs() < 1e-4,
        "镜像交点必须落在地面上（y = 0），实际 {hit:?}"
    );
    project_to_pixel(camera, hit, w, h)
}

/// 以 `center` 为圆心、半径 `radius` 内的「标记红」像素数
/// （判定 r ≫ g,b：ACES 后的自发光红，与 R62a 的 red_pixels_near 同款）。
fn red_pixels_near(px: &[u8], w: u32, h: u32, center: (f32, f32), radius: f32) -> usize {
    let mut count = 0;
    let x0 = (center.0 - radius).max(0.0) as u32;
    let x1 = ((center.0 + radius) as u32).min(w - 1);
    let y0 = (center.1 - radius).max(0.0) as u32;
    let y1 = ((center.1 + radius) as u32).min(h - 1);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let dx = x as f32 - center.0;
            let dy = y as f32 - center.1;
            if dx * dx + dy * dy > radius * radius {
                continue;
            }
            let p = pixel(px, w, x, y);
            if p[0] > 150 && p[0] - p[1] > 80 && p[0] - p[2] > 80 {
                count += 1;
            }
        }
    }
    count
}

/// 兼容锁：缺席帧 FNV = R53 之后默认链；显式关 = 缺席逐位；SSR 开启
/// 首帧（上帧无效 → 整段跳过）= 关闭帧逐位（重置语义的硬证据）；
/// `FrameStats::ssr_on` 观测字段按 0 → 1 翻转。
///
/// **本套件串行化**（R62b 验收实测）：7 项并行线程共用单适配器时，
/// release 下 3 项完成后其余 4 项无限停顿（各测自建 OffscreenRenderer、
/// 每帧 poll 的同步循环互相争用；debug 时序较慢反而通过）——文件内
/// 互斥锁把每帧 poll 的并发度压到 1，标准门禁命令（默认并行度）恢复
/// 可靠；单测串行总耗时 ≈ 7 s，可忽略。
static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn ssr_off_paths_and_first_frame_are_bitwise_pristine() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = base_world(Camera {
        eye: Vec3::new(0.0, 0.0, 3.0),
        target: Vec3::ZERO,
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        shadows: false,
        ..DirectionalLight::default()
    });
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat_bar = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.02, 0.02, 0.02, 1.0));
    world.spawn((
        Transform {
            rotation: glam::Quat::from_rotation_z(45f32.to_radians()),
            scale: Vec3::new(0.14, 4.0, 0.14),
            ..Transform::IDENTITY
        },
        cube,
        mat_bar,
    ));
    world.spawn((
        UiRect {
            position: [6.0, 6.0],
            size: [28.0, 28.0],
        },
        UiColor([1.0, 0.0, 0.0, 1.0]),
    ));
    propagate_transforms(&mut world);

    // ---- 缺席：全帧 FNV 锁在 R53 之后默认链 ----
    renderer.render(&world);
    let absent = renderer.read_pixels();
    assert_eq!(
        fnv1a(&absent),
        R53_FXAA_OFF_BASELINE,
        "SSR 缺席帧必须与 R53 之后默认链逐位一致（兼容锁，同 fxaa/taa 场景）"
    );
    assert_eq!(renderer.renderer().stats().ssr_on, 0, "缺席帧 ssr_on = 0");

    // ---- 显式 enabled = false 与缺席逐位 ----
    world.insert_resource(SsrSettings {
        enabled: false,
        ..SsrSettings::default()
    });
    renderer.render(&world);
    let explicit_off = renderer.read_pixels();
    assert_eq!(absent, explicit_off, "缺席 = 显式关闭必须逐位一致");

    // ---- 开启首帧（上帧无效）与关闭帧逐位一致 ----
    world.insert_resource(ssr_enabled());
    renderer.render(&world);
    let first_on = renderer.read_pixels();
    assert_eq!(
        absent, first_on,
        "SSR 开启首帧必须与关闭帧逐位一致（重置语义）"
    );
    assert_eq!(
        renderer.renderer().stats().ssr_on,
        0,
        "首帧上帧资源无效，ssr_on = 0"
    );
    // 第二帧起上帧资源就位，正常步进（观测字段翻转）
    renderer.render(&world);
    assert_eq!(
        renderer.renderer().stats().ssr_on,
        1,
        "第二帧起 ssr_on 应翻转为 1"
    );
}

/// 有效性①：金属地面反射**屏内**标记——SSR 开启（第二帧起）后，标记的
/// 反射出现在解析镜像位置邻域；SSR 关闭时该位置无标记色（env 只反射
/// 天空）。首帧（重置跳过）同样无反射——逐帧证据链完整。
#[test]
fn ssr_reflects_on_screen_marker_at_analytic_mirror_position() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let scene = ssr_scene();
    let (world, _, _) = mirror_world(&scene, Some(scene.marker_b), false);
    // 断言点 = 标记下部 1/4 高度点（y = 0.5）的镜像：镜像 streak 的
    // 「基底侧」是步进的可靠命中区；streak 的远端（更近的地面片元，
    // 反射标记上部）会被地面自身的屏幕深度先截获——掠射大平面上方的
    // 反射射线在屏幕空间分不清「离开地面」与「落到远处地面像素」，这是
    // 线性步进 SSR 的经典伪影（诚实清单 §8.38），颜色断言避开该段。
    let probe_point = Vec3::new(scene.marker_b.x, 0.5, scene.marker_b.z);
    let mirror_px = analytic_mirror_pixel(&scene.camera, probe_point, SIZE, SIZE);
    println!(
        "解析镜像落点（标记 y=0.5 点）({:.1}, {:.1})",
        mirror_px.0, mirror_px.1
    );
    assert!(
        mirror_px.0 > 20.0 && mirror_px.0 < 236.0 && mirror_px.1 > 90.0 && mirror_px.1 < 250.0,
        "镜像落点应在画面内的地面区域，否则断言无意义"
    );

    // ---- SSR 关（对照）：env 只反射天空，镜像位置无标记红 ----
    renderer.render(&world);
    let off = renderer.read_pixels();
    let off_at_mirror = red_pixels_near(&off, SIZE, SIZE, mirror_px, 8.0);
    assert_eq!(
        off_at_mirror, 0,
        "SSR 关闭时镜像位置不应有标记红（env 只反射天空）：{off_at_mirror}"
    );

    // ---- SSR 开：首帧重置跳过（仍无反射），第二帧步进生效 ----
    let (mut world_on, _, _) = mirror_world(&scene, Some(scene.marker_b), false);
    world_on.insert_resource(ssr_enabled());
    renderer.render(&world_on);
    let first = renderer.read_pixels();
    let first_at_mirror = red_pixels_near(&first, SIZE, SIZE, mirror_px, 8.0);
    assert_eq!(
        first_at_mirror, 0,
        "开启首帧（重置跳过）镜像位置应无标记红：{first_at_mirror}"
    );
    renderer.render(&world_on);
    let on = renderer.read_pixels();
    let mut diff_px = 0usize;
    for i in (0..off.len()).step_by(4) {
        if on[i..i + 3] != off[i..i + 3] {
            diff_px += 1;
        }
    }
    println!("[调试] 开/关逐帧差异像素数 = {diff_px}");
    let mpx = project_to_pixel(&scene.camera, scene.marker_b, SIZE, SIZE);
    {
        let mx = mpx.0 as u32;
        let my = mpx.1 as u32;
        let i = ((my * SIZE + mx) * 4) as usize;
        println!("[调试] 标记直接像素 ({mx},{my}) = {:?}", &on[i..i + 3]);
    }
    // ASCII 差异图（8×8 块）+ on 帧的“红主导”块分布
    for by in 0..32u32 {
        let mut row = String::new();
        for bx in 0..32u32 {
            let mut d = 0;
            let mut red = 0;
            for y in (by * 8)..(by * 8 + 8) {
                for x in (bx * 8)..(bx * 8 + 8) {
                    let i = ((y * SIZE + x) * 4) as usize;
                    if on[i..i + 3] != off[i..i + 3] {
                        d += 1;
                    }
                    let (r, g, b) = (on[i] as i32, on[i + 1] as i32, on[i + 2] as i32);
                    if r > 150 && r - g > 80 && r - b > 80 {
                        red += 1;
                    }
                }
            }
            let ch = if red > 8 {
                'R'
            } else if d > 32 {
                '#'
            } else if d > 8 {
                '+'
            } else if d > 0 {
                '.'
            } else {
                ' '
            };
            row.push(ch);
        }
        println!("  {row}");
    }
    let on_at_mirror = red_pixels_near(&on, SIZE, SIZE, mirror_px, 8.0);
    println!("SSR 开：镜像落点 8px 邻域红色 {on_at_mirror}（关 = {off_at_mirror}）");
    let mut diff_px = 0usize;
    for i in (0..off.len()).step_by(4) {
        if on[i..i + 3] != off[i..i + 3] {
            diff_px += 1;
        }
    }
    println!("[调试] 差异像素 = {diff_px}");
    for by in 0..32u32 {
        let mut row = String::new();
        for bx in 0..32u32 {
            let mut d = 0;
            let mut red = 0;
            for y in (by * 8)..(by * 8 + 8) {
                for x in (bx * 8)..(bx * 8 + 8) {
                    let i = ((y * SIZE + x) * 4) as usize;
                    if on[i..i + 3] != off[i..i + 3] {
                        d += 1;
                    }
                    let (r, g, b) = (on[i] as i32, on[i + 1] as i32, on[i + 2] as i32);
                    if r > 150 && r - g > 80 && r - b > 80 {
                        red += 1;
                    }
                }
            }
            let ch = if red > 8 {
                'R'
            } else if d > 32 {
                '#'
            } else if d > 8 {
                '+'
            } else if d > 0 {
                '.'
            } else {
                ' '
            };
            row.push(ch);
        }
        println!("  |{row}|");
    }
    let dpx = project_to_pixel(&scene.camera, scene.marker_b, SIZE, SIZE);
    println!("[调试] 标记中心直接像素位 ({:.0},{:.0})", dpx.0, dpx.1);
    {
        let (mx, my) = (dpx.0 as u32, dpx.1 as u32);
        let i = ((my * SIZE + mx) * 4) as usize;
        println!("[调试] 标记本体颜色 = {:?}", &on[i..i + 3]);
    }
    for (x, y) in [
        (198u32, 113u32),
        (195, 110),
        (200, 105),
        (205, 120),
        (210, 113),
        (190, 113),
    ] {
        let i = ((y * SIZE + x) * 4) as usize;
        println!(
            "[调试] on({x},{y})={:?} off={:?}",
            &on[i..i + 3],
            &off[i..i + 3]
        );
    }
    for (x, y) in [
        (228u32, 137u32),
        (235, 120),
        (240, 110),
        (230, 100),
        (245, 130),
        (220, 140),
    ] {
        let i = ((y * SIZE + x) * 4) as usize;
        println!(
            "[调试] on({x},{y}) = {:?}  off({x},{y}) = {:?}",
            &on[i..i + 3],
            &off[i..i + 3]
        );
    }
    assert!(
        on_at_mirror >= 15,
        "SSR 开启后标记的反射应落在解析镜像位置附近（8px 邻域 {on_at_mirror} 个标记红像素）"
    );
}

/// 有效性②（本轮存在的理由）：探针烘焙后标记从 A 移到 B——SSR 的反射
/// 跟随**新位置 B**（镜像位置出现标记红）；探针路径（SSR 关）只反映
/// 烘焙时刻的内容 A（A 的镜像位置有红、B 的没有）。探针做不到、SSR
/// 做得到，GPU 断言锁定。
#[test]
fn ssr_reflects_dynamic_objects_the_probe_cannot() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let scene = ssr_scene();
    let mirror_a = analytic_mirror_pixel(&scene.camera, scene.marker_a, SIZE, SIZE);
    let mirror_b = analytic_mirror_pixel(&scene.camera, scene.marker_b, SIZE, SIZE);
    let separation = ((mirror_a.0 - mirror_b.0).powi(2) + (mirror_a.1 - mirror_b.1).powi(2)).sqrt();
    println!(
        "镜像落点：A ({:.1}, {:.1})，B ({:.1}, {:.1})，相距 {separation:.1}px",
        mirror_a.0, mirror_a.1, mirror_b.0, mirror_b.1
    );
    assert!(
        separation > 100.0,
        "两个镜像落点必须显著分离（实际 {separation:.1}px），否则断言无鉴别力"
    );

    // ---- 探针路径（SSR 关）：烘焙于标记在 A 的时刻，随后移到 B ----
    let (mut world_off, probe_off, marker_off) = mirror_world(&scene, Some(scene.marker_a), true);
    assert!(
        renderer
            .renderer_mut()
            .bake_reflection_probe(&world_off, probe_off.expect("探针实体应存在")),
        "探针烘焙应成功（标记仍在 A）"
    );
    // 烘焙后把标记移到 B（探针内容冻结在 A）
    *world_off.get_mut::<Transform>(marker_off.unwrap()).unwrap() = Transform {
        position: scene.marker_b,
        scale: scene.marker_scale,
        ..Transform::IDENTITY
    };
    propagate_transforms(&mut world_off);
    renderer.render(&world_off);
    let off = renderer.read_pixels();
    // 探针视差对**盒内**标记是盒面近似重投影（R62a §8.37 诚实清单），
    // 反射落点相对解析镜像有偏移 ⇒ 探针侧断言用 24px 邻域；SSR 侧
    // （平面镜精确）仍用 8px。A/B 镜像相距 134px，24px 邻域不重叠。
    let off_at_a = red_pixels_near(&off, SIZE, SIZE, mirror_a, 24.0);
    let off_at_b = red_pixels_near(&off, SIZE, SIZE, mirror_b, 24.0);
    println!("探针路径：A 镜像 24px 邻域红色 {off_at_a}，B 邻域 {off_at_b}");
    assert!(
        off_at_a >= 8,
        "探针路径应反映烘焙时刻的内容 A（A 镜像邻域 {off_at_a} 个红像素）"
    );
    assert_eq!(
        off_at_b, 0,
        "探针路径不能反映移动后的 B（B 镜像邻域 {off_at_b} 个红像素）——这正是探针的边界"
    );

    // ---- SSR 开：同样的「烘焙后移动」，反射跟随 B ----
    let (mut world_on, probe_on, marker_on) = mirror_world(&scene, Some(scene.marker_a), true);
    assert!(
        renderer
            .renderer_mut()
            .bake_reflection_probe(&world_on, probe_on.expect("探针实体应存在"))
    );
    *world_on.get_mut::<Transform>(marker_on.unwrap()).unwrap() = Transform {
        position: scene.marker_b,
        scale: scene.marker_scale,
        ..Transform::IDENTITY
    };
    propagate_transforms(&mut world_on);
    world_on.insert_resource(ssr_enabled());
    renderer.render(&world_on); // 首帧重置，建立上帧资源
    renderer.render(&world_on); // 第二帧步进生效
    let on = renderer.read_pixels();
    let on_at_b = red_pixels_near(&on, SIZE, SIZE, mirror_b, 8.0);
    let on_at_a = red_pixels_near(&on, SIZE, SIZE, mirror_a, 24.0);
    println!("SSR 开：B 镜像 8px 邻域红色 {on_at_b}，A 邻域 24px 红色 {on_at_a}");
    assert!(
        on_at_b >= 15,
        "SSR 必须反射移动后的标记 B（B 镜像 {on_at_b} 个红像素）"
    );
    assert!(
        on_at_a >= 8,
        "A 镜像处的探针回退应保留（SSR 未在 A 处命中）：{on_at_a}"
    );
}

/// 有效性③：SSR 强度随粗糙度单调下降。几何完全固定的金属地面 + 全宽
/// 红色背景墙，地面粗糙度取 4 个离散值 {0.1, 0.3, 0.5, 0.7} 各渲染
/// 「关 / 开」一对——**唯粗糙度是变量**：SSR 开 / 关的差分像素数在
/// 衰减带内单调不增（0.7 处权重 0 ⇒ 差分恒 0），且 0.7 的帧**逐位**
/// 等于关闭帧（粗糙面整段跳过步进的硬证据）。早退阈值 0.6 依据见
/// §4.31 裁决 4（GGX lobe 半角 ~40° 的单射线采样无意义）。
#[test]
fn ssr_strength_falls_monotonically_with_roughness() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let camera = Camera {
        eye: Vec3::new(0.0, 1.5, 3.5),
        target: Vec3::new(0.0, 0.8, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_4,
        ..Camera::default()
    };
    let build = |roughness: f32, with_ssr: bool| {
        let mut world = base_world(camera.clone());
        let floor = world.resource_mut::<MeshStore>().add(MeshData::plane(12.0));
        let mirror = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::metal(0.85, 0.85, 0.88, roughness));
        world.spawn((Transform::IDENTITY, floor, mirror));
        // 全宽红色背景墙（自发光，roughness = 1 不参与步进）：上半屏直接
        // 可见，其在金属地面里的反射铺满地面反射带
        let wall = world.resource_mut::<MeshStore>().add(MeshData::plane(12.0));
        let red_wall = world
            .resource_mut::<MaterialStore>()
            .add(emissive(1.0, 0.02, 0.0));
        world.spawn((
            Transform {
                position: Vec3::new(0.0, 3.0, -5.5),
                rotation: glam::Quat::from_axis_angle(Vec3::X, std::f32::consts::FRAC_PI_2),
                ..Transform::IDENTITY
            },
            wall,
            red_wall,
        ));
        if with_ssr {
            world.insert_resource(ssr_enabled());
        }
        propagate_transforms(&mut world);
        world
    };

    // 逐粗糙度：「关」帧与「开」（第 2 帧，上帧资源就位）帧比对。
    // **每档粗糙度用全新渲染器**：材质 uniform 按句柄缓存（同渲染器内
    // 同句柄只上传一次），跨迭代改同句柄材质不会生效。
    let mut stats = Vec::new();
    // 取样点拉开衰减带间距：fade(0.1)=1、fade(0.45)=0.43、
    // fade(0.55)=0.14、fade(0.7)=0（早退）——相邻档 3 倍权重差压过
    // env mip 变化的噪声级扰动（~0.4% 像素数）
    for r in [0.1f32, 0.45, 0.55, 0.7] {
        let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
            eprintln!("跳过：环境无 GPU 适配器");
            return;
        };
        let world_plain = build(r, false);
        let world_ssr = build(r, true);
        renderer.render(&world_plain);
        let off = renderer.read_pixels();
        renderer.render(&world_ssr); // 首帧重置，建立上帧资源
        renderer.render(&world_ssr); // 第二帧步进生效
        let on = renderer.read_pixels();
        // 指标 = 「SSR 造成的红像素」数：on 帧红主导（r>150 且 r−g>80 且
        // r−b>80）且 off 帧不红——地面反射带里 env 给蓝天、SSR 给红墙，
        // 「on 变红」只能来自 SSR，随合成权重 w 单调（redness ≈
        // w·(S_r − S_g) + (1−w)·(E_r − E_g)，S 红主导、E 蓝偏）。
        // 墙本体的直射红像素 on/off 相同，被「off 不红」条件剔除。
        let is_red = |px: &[u8], i: usize| {
            let (r, g, b) = (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32);
            r > 150 && r - g > 80 && r - b > 80
        };
        let mut red = 0usize;
        for i in (0..off.len()).step_by(4) {
            if !is_red(&off, i) && is_red(&on, i) {
                red += 1;
            }
        }
        stats.push((r, red));
        println!("[指标] 粗糙度 {r}: 红像素 = {red}");
        if r == 0.7 {
            // 早退阈值之上：整帧逐位等于关闭帧
            assert_eq!(off, on, "粗糙度 0.7 ≥ 早退阈值 0.6：整帧必须逐位等于关闭帧");
        }
    }
    // 差分像素数单调不增（同一几何 ⇒ 命中集相同，权重随粗糙度衰减；
    // 8-bit 量化下弱权重像素可能回落为「无差」，只会更少不会更多）
    // 严格单调下降（衰减带内相邻档权重差 3 倍，远大于噪声）
    for i in 0..stats.len() - 2 {
        assert!(
            stats[i].1 > stats[i + 1].1,
            "SSR 反射强度必须随粗糙度严格单调下降：r={:.1} → r={:.1}，{} → {}",
            stats[i].0,
            stats[i + 1].0,
            stats[i].1,
            stats[i + 1].1
        );
    }
    assert!(
        stats[0].1 > 500,
        "最光滑地面的 SSR 反射应显著（{} 个红像素）——场景自检",
        stats[0].1
    );
}

/// 有效性④：标记物不在上帧屏内 ⇒ 反射射线无处可命中 ⇒ 全部片元未命中
/// ⇒ 整帧与「探针路径（SSR 关）」逐位一致——回退 = 既有通路原值的最强
/// 形式（含探针影响的数值恒等）。
#[test]
fn ssr_miss_falls_back_to_probe_path_bitwise() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    // 相机陡峭下视：天花板高度的标记在正前上方——实际视锥之外，其镜像
    // 却落在画面中央的地面里（解析验证）。
    let camera = Camera {
        eye: Vec3::new(0.0, 2.0, 2.0),
        target: Vec3::new(0.0, 0.0, -0.5),
        fov_y_radians: std::f32::consts::FRAC_PI_4,
        ..Camera::default()
    };
    let marker = Vec3::new(0.0, 3.2, -3.0);
    let marker_px = project_to_pixel(&camera, marker, SIZE, SIZE);
    let mirror_px = analytic_mirror_pixel(&camera, marker, SIZE, SIZE);
    println!(
        "屏外标记投影像素 ({:.1}, {:.1})，镜像落点 ({:.1}, {:.1})",
        marker_px.0, marker_px.1, mirror_px.0, mirror_px.1
    );
    assert!(
        marker_px.1 < -10.0 || marker_px.1 > SIZE as f32 + 10.0,
        "标记必须在画面之外（实际 {marker_px:?}），否则「屏外」前提不成立"
    );
    assert!(
        mirror_px.0 > 40.0 && mirror_px.0 < 216.0 && mirror_px.1 > 40.0 && mirror_px.1 < 216.0,
        "镜像落点应在画面中央（实际 {mirror_px:?}），否则反射路径无从谈起"
    );

    let build = |with_ssr: bool| {
        let mut world = base_world(camera.clone());
        let floor = world.resource_mut::<MeshStore>().add(MeshData::plane(12.0));
        let mirror = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::metal(0.85, 0.85, 0.88, 0.05));
        world.spawn((Transform::IDENTITY, floor, mirror));
        let slab = world.resource_mut::<MeshStore>().add(MeshData::cube());
        let red = world
            .resource_mut::<MaterialStore>()
            .add(emissive(1.0, 0.02, 0.0));
        world.spawn((
            Transform {
                position: marker,
                scale: Vec3::new(1.2, 0.12, 1.2),
                ..Transform::IDENTITY
            },
            slab,
            red,
        ));
        world.spawn((
            Transform::from_xyz(0.0, 1.475, 0.0),
            ReflectionProbe::new(Vec3::new(4.0, 3.0, 4.0)).with_falloff(0.0),
        ));
        if with_ssr {
            world.insert_resource(ssr_enabled());
        }
        propagate_transforms(&mut world);
        world
    };
    let probe_entity = |world: &World| {
        let mut found: Option<Entity> = None;
        world.query::<(Entity, &ReflectionProbe)>(|(e, _)| {
            if found.is_none() {
                found = Some(e);
            }
        });
        found.expect("场景应有探针实体")
    };

    // 探针烘焙（两侧各自烘焙同场景——探针内容含屏外标记）
    let world_off = build(false);
    assert!(
        renderer
            .renderer_mut()
            .bake_reflection_probe(&world_off, probe_entity(&world_off))
    );
    renderer.render(&world_off);
    let off = renderer.read_pixels();

    let world_on = build(true);
    assert!(
        renderer
            .renderer_mut()
            .bake_reflection_probe(&world_on, probe_entity(&world_on))
    );
    renderer.render(&world_on); // 首帧重置
    renderer.render(&world_on); // 第二帧步进（全部未命中）
    let on = renderer.read_pixels();
    assert_eq!(
        off, on,
        "屏外标记 ⇒ 全部片元未命中 ⇒ 整帧必须与探针路径（SSR 关）逐位一致"
    );
}

/// 确定性：两个同输入完整会话（48 帧 ≥ 40：前 24 帧相机平移 + 后 24 帧
/// 标记自体移动）逐帧逐位一致——上帧颜色 / 深度反馈是有状态演化，与
/// TAA 同一确定性纪律。
#[test]
fn ssr_two_sessions_are_frame_by_frame_bitwise() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer_a) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut renderer_b) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let scene = ssr_scene();
    let (mut world_a, _, marker_a_e) = mirror_world(&scene, Some(scene.marker_b), false);
    let (mut world_b, _, marker_b_e) = mirror_world(&scene, Some(scene.marker_b), false);
    world_a.insert_resource(ssr_enabled());
    world_b.insert_resource(ssr_enabled());

    const CAM_FRAMES: u32 = 24;
    const OBJ_FRAMES: u32 = 24;
    let mut frames_a: Vec<Vec<u8>> = Vec::new();
    let mut frames_b: Vec<Vec<u8>> = Vec::new();
    for i in 0..(CAM_FRAMES + OBJ_FRAMES) {
        if i < CAM_FRAMES {
            // 相机亚像素平移（0.008 世界单位 ≈ 0.65 px/帧）
            let x = i as f32 * 0.008;
            for world in [&mut world_a, &mut world_b] {
                world.resource_mut::<Camera>().eye = Vec3::new(x, 2.2, 3.0);
            }
        } else {
            // 标记自体移动（横穿画面滑动——上帧内容逐帧变化）
            let t = (i - CAM_FRAMES) as f32 / (OBJ_FRAMES - 1) as f32;
            let x = scene.marker_b.x - 2.6 * t;
            *world_a.get_mut::<Transform>(marker_a_e.unwrap()).unwrap() = Transform {
                position: Vec3::new(x, scene.marker_b.y, scene.marker_b.z),
                scale: scene.marker_scale,
                ..Transform::IDENTITY
            };
            *world_b.get_mut::<Transform>(marker_b_e.unwrap()).unwrap() = Transform {
                position: Vec3::new(x, scene.marker_b.y, scene.marker_b.z),
                scale: scene.marker_scale,
                ..Transform::IDENTITY
            };
        }
        propagate_transforms(&mut world_a);
        propagate_transforms(&mut world_b);
        renderer_a.render(&world_a);
        renderer_b.render(&world_b);
        frames_a.push(renderer_a.read_pixels());
        frames_b.push(renderer_b.read_pixels());
    }
    assert_eq!(
        frames_a.len() as u32,
        CAM_FRAMES + OBJ_FRAMES,
        "会话帧数 ≥ 40（含相机运动与物体运动）"
    );
    for (i, (a, b)) in frames_a.iter().zip(&frames_b).enumerate() {
        assert_eq!(a, b, "第 {i} 帧两会话必须逐位一致（确定性硬要求）");
    }
    // SSR 确实活跃（否则双会话一致是平凡的）
    assert_eq!(
        renderer_a.renderer().stats().ssr_on,
        1,
        "运动段结束后 SSR 应处于活跃状态"
    );
}

/// reset_taa() 联动（裁决 3）：切镜 ⇒ SSR 的上帧语义一并过期 ⇒ 下一帧
/// 与 SSR 关闭同姿态帧逐位一致；再下一帧随帧尾副本恢复活跃。
#[test]
fn ssr_follows_reset_taa_skip_frame_is_bitwise_plain() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut plain) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let scene = ssr_scene();
    let (mut world_on, _, _) = mirror_world(&scene, Some(scene.marker_b), false);
    let (world_off, _, _) = mirror_world(&scene, Some(scene.marker_b), false);
    world_on.insert_resource(ssr_enabled());

    // 建立 SSR 活跃状态（2 帧后活跃）
    renderer.render(&world_on);
    renderer.render(&world_on);
    assert_eq!(renderer.renderer().stats().ssr_on, 1, "活跃状态自检");

    // 切镜：两会话同姿态瞬移相机；SSR 会话显式 reset_taa()
    let teleport = Vec3::new(1.4, 2.6, 3.4);
    world_on.resource_mut::<Camera>().eye = teleport;
    world_off.resource_mut::<Camera>().eye = teleport;
    renderer.renderer_mut().reset_taa();
    renderer.render(&world_on);
    plain.render(&world_off);
    let on_after_reset = renderer.read_pixels();
    let off_same_pose = plain.read_pixels();
    assert_eq!(
        on_after_reset, off_same_pose,
        "切镜帧（reset_taa 后）SSR 必须整段跳过：与 SSR 关闭同姿态帧逐位一致"
    );
    assert_eq!(
        renderer.renderer().stats().ssr_on,
        0,
        "切镜帧的 ssr_on 观测字段应为 0"
    );
    // 切镜帧完成了新的帧尾副本 ⇒ 再下一帧恢复活跃
    renderer.render(&world_on);
    assert_eq!(
        renderer.renderer().stats().ssr_on,
        1,
        "切镜后一帧应恢复活跃"
    );
}
