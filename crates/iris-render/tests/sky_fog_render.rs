//! 集成测试：天空 v2（大气散射）与体积雾（R79，`SkySettings` /
//! `FogSettings` + `shaders/skybox.wgsl` + `shaders/fog.wgsl`）。
//!
//! 五条主线（与 ARCHITECTURE §4.36 的验收标准一一对应）：
//! 1. **兼容锁（硬约束）**：`FogSettings` / `SkySettings` 均缺席帧与
//!    显式 `enabled = false` / `atmosphere = false` 帧两两组合全帧
//!    FNV-1a **逐位一致**——雾 pass 不编码（链拓扑走旧函数）、天空
//!    mode = 0 死码分支（哑 uniform 零写路径）；
//! 2. **雾距单调（有效性①）**：两堵同材质墙一近一远（直射光强度 0
//!    隔离——纯消光无内散射混淆），雾开启后远墙向雾色（黑）的偏移
//!    显著大于近墙（消光透过率 exp(−σd) 的距离单调性）；
//! 3. **高度衰减（有效性②）**：相机升高后同方向采样的雾浓度下降
//!    （指数高度衰减 exp(−h·k) 的解析预期）；
//! 4. **大气天空（有效性③）**：atmosphere on vs off 天顶/地平采样点
//!    色移显著 + 太阳盘中心像素显著亮于同仰角旁路（盘是 HDR 添项）；
//! 5. **确定性**：雾 + 大气同开的双会话逐帧逐位一致（含物体运动）；
//!    改 `phase_g` 全帧哈希改变（防恒等假通过）。
//!
//! 环境无 GPU 适配器时优雅跳过（打印提示，测试仍算通过）。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 屏幕尺寸（测试统一口径）。
const SIZE: u32 = 256;

/// 本套件串行化（ssr / ao 同款纪律）：同文件多测试并行共用单适配器
/// 会挂死——文件内互斥锁把并发度压到 1。
static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ---- 场景公共件 ----

/// 雾距单调场景：地面 + 两堵同材质墙（近 x=−4 z=−6 / 远 x=5 z=−42）。
/// 直射光强度 0（纯环境光隔离——雾只消光不内散射，距离效应不被
/// 太阳内散射的空间分布混淆）。
fn two_wall_world(camera: &Camera) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(camera.clone());
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.3, -1.0, -0.2).normalize(),
        intensity: 0.0,
        ambient: Vec3::new(0.5, 0.55, 0.6),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    let mut solid = MaterialDef::solid(0.6, 0.6, 0.62, 1.0);
    solid.roughness = 0.9;
    let mat = world.resource_mut::<MaterialStore>().add(solid);
    let floor_mesh = world
        .resource_mut::<MeshStore>()
        .add(MeshData::plane(300.0));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    world.spawn((Transform::IDENTITY, floor_mesh, mat));
    // 近墙（左）与远墙（右）：同材质同朝向（+z 面向相机），唯一差异
    // 是到相机的距离——雾的消光距离单调性因此可判别
    world.spawn((
        Transform {
            position: Vec3::new(-4.0, 1.5, -6.0),
            scale: Vec3::new(3.0, 3.0, 0.4),
            ..Transform::IDENTITY
        },
        cube,
        mat,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(5.0, 1.5, -42.0),
            scale: Vec3::new(3.0, 3.0, 0.4),
            ..Transform::IDENTITY
        },
        cube,
        mat,
    ));
    propagate_transforms(&mut world);
    world
}

/// 水平注视相机（fov 60°，两墙分居画面左右）。
fn wall_camera() -> Camera {
    Camera {
        eye: Vec3::new(0.0, 1.8, 4.0),
        target: Vec3::new(0.0, 1.8, -20.0),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    }
}

fn fog_on() -> FogSettings {
    FogSettings {
        enabled: true,
        ..FogSettings::default()
    }
}

fn atmosphere_on() -> SkySettings {
    SkySettings {
        atmosphere: true,
        ..SkySettings::default()
    }
}

fn pixel(px: &[u8], w: u32, x: u32, y: u32) -> [i32; 3] {
    let i = ((y * w + x) * 4) as usize;
    [px[i] as i32, px[i + 1] as i32, px[i + 2] as i32]
}

fn luminance(p: [i32; 3]) -> i32 {
    p[0] + p[1] + p[2]
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

/// 世界坐标 → 像素坐标（与渲染器同一条相机路径；NDC y = +1 对应第 0 行）。
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

fn sample(px: &[u8], camera: &Camera, at: Vec3) -> [i32; 3] {
    let (fx, fy) = project_to_pixel(camera, at, SIZE, SIZE);
    pixel(px, SIZE, fx as u32, fy as u32)
}

/// 渲染 `frames` 帧并读回最后一帧（固定帧数口径：跨会话比较时两侧
/// 帧序一致——渲染器有跨帧状态如实例增量上传镜像）。
fn render_frames(renderer: &mut OffscreenRenderer, world: &World, frames: u32) -> Vec<u8> {
    for _ in 0..frames {
        renderer.render(world);
    }
    renderer.read_pixels()
}

// ---- 1. 兼容锁：缺席 = 显式关（两两组合逐位） ----

#[test]
fn sky_fog_absence_and_disabled_are_bitwise_pristine() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let camera = wall_camera();
    let mut worlds: Vec<(String, World)> = Vec::new();
    worlds.push(("双缺席".into(), two_wall_world(&camera)));
    let mut w = two_wall_world(&camera);
    w.insert_resource(FogSettings::disabled());
    w.insert_resource(SkySettings {
        atmosphere: false,
        ..SkySettings::default()
    });
    worlds.push(("双显式关".into(), w));
    let mut w = two_wall_world(&camera);
    w.insert_resource(FogSettings::disabled());
    worlds.push(("仅雾显式关".into(), w));
    let mut w = two_wall_world(&camera);
    w.insert_resource(SkySettings {
        atmosphere: false,
        ..SkySettings::default()
    });
    worlds.push(("仅天空显式关".into(), w));

    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut hashes = Vec::new();
    for (name, world) in &worlds {
        renderer.render(world);
        let h = fnv1a(&renderer.read_pixels());
        hashes.push(h);
        println!("组合「{name}」FNV = {h:#018x}");
    }
    for (i, h) in hashes.iter().enumerate().skip(1) {
        assert_eq!(
            *h, hashes[0],
            "组合 {i} 必须与双缺席帧逐位一致（零开销默认路径）"
        );
    }
}

// ---- 2. 雾距单调：远墙向雾色偏移显著大于近墙 ----

#[test]
fn fog_far_wall_shifts_more_than_near_wall() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let camera = wall_camera();
    let near_point = Vec3::new(-4.0, 1.6, -5.8);
    let far_point = Vec3::new(5.0, 1.6, -41.8);

    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    // 雾关基线（独立会话，2 帧——与雾会话同帧数口径）
    let world_off = two_wall_world(&camera);
    let off = render_frames(&mut renderer, &world_off, 2);

    // 雾开会话：深度副本与雾 pass 同编码器按序执行（链上同帧——首帧
    // 即有效），测第 2 帧保持与基线同帧数口径
    let mut world_on = two_wall_world(&camera);
    world_on.insert_resource(fog_on());
    let on = render_frames(&mut renderer, &world_on, 2);

    let near_off = luminance(sample(&off, &camera, near_point));
    let near_on = luminance(sample(&on, &camera, near_point));
    let far_off = luminance(sample(&off, &camera, far_point));
    let far_on = luminance(sample(&on, &camera, far_point));
    let near_drop = near_off - near_on;
    let far_drop = far_off - far_on;
    println!(
        "雾距单调：近墙 off={near_off} on={near_on}（Δ={near_drop}），远墙 off={far_off} on={far_on}（Δ={far_drop}）"
    );
    // 消光解析预期：σ ≈ 0.0365（1.6 m 高度），近墙 10 m → T ≈ 0.70，
    // 远墙 46 m → T ≈ 0.19 ⇒ 远墙暗化 ≈ 2.7× 近墙
    assert!(
        near_drop >= 0,
        "近墙不应因雾变亮（无内散射隔离）：{near_drop}"
    );
    assert!(
        far_drop >= 25,
        "远墙必须显著向雾色偏移（off={far_off} on={far_on}）"
    );
    assert!(
        far_drop >= 2 * near_drop,
        "远墙暗化必须 ≥ 2× 近墙（距离单调可判别）：far={far_drop} near={near_drop}"
    );
}

// ---- 3. 高度衰减：相机升高后同方向雾浓度下降 ----

#[test]
fn fog_thins_with_camera_height() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let low_cam = wall_camera();
    let high_cam = Camera {
        eye: Vec3::new(0.0, 18.0, 4.0),
        target: Vec3::new(0.0, 18.0, -20.0),
        ..low_cam.clone()
    };

    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    // 只有地面（画面中心为天空像素——两台相机同方向、同一素色天空）
    let floor_world = |cam: &Camera| {
        let mut w = World::new();
        w.insert_resource(MeshStore::new());
        w.insert_resource(MaterialStore::new());
        w.insert_resource(TextureStore::new());
        w.insert_resource(cam.clone());
        w.insert_resource(DirectionalLight {
            direction: Vec3::new(-0.3, -1.0, -0.2).normalize(),
            intensity: 0.0,
            ambient: Vec3::new(0.5, 0.55, 0.6),
            shadows: false,
            ..DirectionalLight::default()
        });
        w.insert_resource(BloomSettings::disabled());
        let mut solid = MaterialDef::solid(0.6, 0.6, 0.62, 1.0);
        solid.roughness = 0.9;
        let mat = w.resource_mut::<MaterialStore>().add(solid);
        let floor_mesh = w.resource_mut::<MeshStore>().add(MeshData::plane(300.0));
        w.spawn((Transform::IDENTITY, floor_mesh, mat));
        propagate_transforms(&mut w);
        w
    };
    let world_low = floor_world(&low_cam);
    let world_high = floor_world(&high_cam);
    let off_low = render_frames(&mut renderer, &world_low, 1);
    let off_high = render_frames(&mut renderer, &world_high, 1);

    let mut world_low_on = floor_world(&low_cam);
    world_low_on.insert_resource(fog_on());
    let mut world_high_on = floor_world(&high_cam);
    world_high_on.insert_resource(fog_on());
    let on_low = render_frames(&mut renderer, &world_low_on, 2);
    let on_high = render_frames(&mut renderer, &world_high_on, 2);

    // 屏幕中心 = 水平视线的天空像素（雾射线长 = max_distance = 96 m）
    let center = (SIZE / 2, SIZE / 2);
    let read = |px: &[u8]| {
        let (x, y) = center;
        luminance(pixel(px, SIZE, x, y))
    };
    let low_off = read(&off_low);
    let low_on = read(&on_low);
    let high_off = read(&off_high);
    let high_on = read(&on_high);
    let low_drop = low_off - low_on;
    let high_drop = high_off - high_on;
    println!(
        "高度衰减：低相机 off={low_off} on={low_on}（Δ={low_drop}），高相机 off={high_off} on={high_on}（Δ={high_drop}）"
    );
    // 指数衰减解析预期：1.8 m 处 σ ≈ 0.0363 → T(96) ≈ 0.03；18 m 处
    // σ ≈ 0.0053 → T(96) ≈ 0.60 ⇒ 低相机雾化显著更强
    assert!(
        low_drop >= 40,
        "低相机天空必须被雾显著压暗（off={low_off} on={low_on}）"
    );
    assert!(
        low_drop >= 2 * high_drop,
        "低相机雾化必须 ≥ 2× 高相机（高度衰减可判别）：low={low_drop} high={high_drop}"
    );
}

// ---- 4. 大气天空：色移显著 + 太阳盘可判别 ----

#[test]
fn atmosphere_changes_sky_and_sun_disk_is_bright() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    // 相机对准太阳（仰角 ~55°，方位 −z）；高仰位形让太阳盘以深蓝天空
    // 为背景（低仰位形下盘周天空物理上就是近白——8bit 显示域无判别度）；
    // fov 30° 让太阳盘占 ~14 像素、盘心 ACES 饱和
    let sun_dir = Vec3::new(0.0, 1.0, -0.7).normalize();
    let camera = Camera {
        eye: Vec3::new(0.0, 2.0, 8.0),
        target: Vec3::new(0.0, 2.0, 8.0) + sun_dir * 30.0,
        fov_y_radians: 30f32.to_radians(),
        ..Camera::default()
    };
    let empty_world = |cam: &Camera, atmo: bool| {
        let mut w = World::new();
        w.insert_resource(MeshStore::new());
        w.insert_resource(MaterialStore::new());
        w.insert_resource(TextureStore::new());
        w.insert_resource(cam.clone());
        // 太阳方向与相机对准方向一致（−direction = sun_dir）
        w.insert_resource(DirectionalLight {
            direction: -sun_dir,
            intensity: 1.0,
            shadows: false,
            ..DirectionalLight::default()
        });
        w.insert_resource(BloomSettings::disabled());
        if atmo {
            w.insert_resource(atmosphere_on());
        }
        w
    };

    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let world_off = empty_world(&camera, false);
    let off = render_frames(&mut renderer, &world_off, 1);
    let mut world_on = empty_world(&camera, true);
    world_on.insert_resource(atmosphere_on());
    let on = render_frames(&mut renderer, &world_on, 1);

    assert_ne!(
        fnv1a(&off),
        fnv1a(&on),
        "大气开关必须改变全帧（天空覆盖整屏）"
    );
    // 采样点：高仰侧（画面顶部中心，仰角 ~70°）与低仰侧（底部中心，
    // 仰角 ~40°）——旧渐变与解析大气在两处都应有可判别色移
    let zenith_px = (SIZE / 2, 4u32);
    let horizon_px = (SIZE / 2, SIZE - 8u32);
    let read = |px: &[u8], p: (u32, u32)| pixel(px, SIZE, p.0, p.1);
    let z_off = read(&off, zenith_px);
    let z_on = read(&on, zenith_px);
    let h_off = read(&off, horizon_px);
    let h_on = read(&on, horizon_px);
    let z_delta: i32 = (0..3).map(|c| (z_off[c] - z_on[c]).abs()).sum();
    let h_delta: i32 = (0..3).map(|c| (h_off[c] - h_on[c]).abs()).sum();
    println!(
        "大气色移：高仰 off={z_off:?} on={z_on:?}（ΣΔ={z_delta}），低仰 off={h_off:?} on={h_on:?}（ΣΔ={h_delta}）"
    );
    assert!(z_delta >= 20, "高仰采样点必须有显著色移：ΣΔ = {z_delta}");
    assert!(h_delta >= 20, "低仰采样点必须有显著色移：ΣΔ = {h_delta}");

    // 太阳盘：盘心显著亮于同仰角旁路（方位偏 12°——画面内、盘外；
    // 55° 仰位的盘周天空是深蓝，盘心 ACES 饱和，判别度充足）
    let disk_center = project_to_pixel(&camera, camera.eye + sun_dir * 50.0, SIZE, SIZE);
    let off_disk_dir = Vec3::new(
        (12f32).to_radians().sin() * sun_dir.z.abs(),
        sun_dir.y,
        -(12f32).to_radians().cos() * sun_dir.z.abs(),
    )
    .normalize();
    let off_disk = project_to_pixel(&camera, camera.eye + off_disk_dir * 50.0, SIZE, SIZE);
    // 采样点必须严格在画面内（越界读取会拿到相邻行的像素——不可判别）
    assert!(
        disk_center.0 >= 0.0
            && disk_center.1 >= 0.0
            && disk_center.0 < SIZE as f32
            && disk_center.1 < SIZE as f32,
        "盘心采样点越界：{disk_center:?}"
    );
    assert!(
        off_disk.0 >= 0.0
            && off_disk.1 >= 0.0
            && off_disk.0 < SIZE as f32
            && off_disk.1 < SIZE as f32,
        "旁路采样点越界：{off_disk:?}"
    );
    let disk_lum = luminance(pixel(&on, SIZE, disk_center.0 as u32, disk_center.1 as u32));
    let side_lum = luminance(pixel(&on, SIZE, off_disk.0 as u32, off_disk.1 as u32));
    println!("太阳盘：盘心 {disk_lum} vs 同仰角旁路 {side_lum}");
    assert!(
        disk_lum - side_lum >= 60,
        "太阳盘中心必须显著亮于同仰角旁路：disk={disk_lum} side={side_lum}"
    );
}

// ---- 5. 确定性：双会话逐位 + phase_g 改变哈希 ----

#[test]
fn fog_atmosphere_double_session_bitwise_and_phase_g_changes_hash() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let camera = wall_camera();
    let build = |box_x: f32, phase_g: f32| {
        let mut w = two_wall_world(&camera);
        // 覆盖为有强度直射光：内散射（相位 × 太阳色）真实参与，
        // phase_g 的变化才有可观测效应（纯消光场景相位不进画面）
        w.insert_resource(DirectionalLight {
            direction: Vec3::new(-0.3, -1.0, -0.2).normalize(),
            intensity: 1.0,
            ambient: Vec3::new(0.5, 0.55, 0.6),
            shadows: false,
            ..DirectionalLight::default()
        });
        let cube = w.resource_mut::<MeshStore>().add(MeshData::cube());
        let mut solid = MaterialDef::solid(0.7, 0.45, 0.3, 1.0);
        solid.roughness = 0.8;
        let mat = w.resource_mut::<MaterialStore>().add(solid);
        w.spawn((
            Transform {
                position: Vec3::new(box_x, 0.4, -14.0),
                scale: Vec3::splat(0.8),
                ..Transform::IDENTITY
            },
            cube,
            mat,
        ));
        w.insert_resource(FogSettings {
            phase_g,
            ..fog_on()
        });
        w.insert_resource(atmosphere_on());
        propagate_transforms(&mut w);
        w
    };

    let Some(mut renderer_a) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut renderer_b) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    // 会话序列：W1 → W2 → W2（含物体运动，跨帧确定性全覆盖）
    let wa1 = build(-1.2, 0.6);
    let wa2 = build(1.2, 0.6);
    let h_a: Vec<u64> = [(&wa1, 1usize), (&wa2, 1), (&wa2, 2)]
        .into_iter()
        .map(|(w, frames)| {
            for _ in 0..frames {
                renderer_a.render(w);
            }
            fnv1a(&renderer_a.read_pixels())
        })
        .collect();
    let wb1 = build(-1.2, 0.6);
    let wb2 = build(1.2, 0.6);
    let h_b: Vec<u64> = [(&wb1, 1usize), (&wb2, 1), (&wb2, 2)]
        .into_iter()
        .map(|(w, frames)| {
            for _ in 0..frames {
                renderer_b.render(w);
            }
            fnv1a(&renderer_b.read_pixels())
        })
        .collect();
    for i in 0..3 {
        assert_eq!(
            h_a[i],
            h_b[i],
            "双会话第 {} 帧必须逐位一致（确定性）",
            i + 1
        );
    }
    assert_ne!(h_a[0], h_a[1], "物体运动必须改变全帧（防恒等假通过）");

    // 改 phase_g（0.6 → 0.2）必须改变全帧哈希（内散射相位真实参与）
    let wc = build(1.2, 0.2);
    for _ in 0..2 {
        renderer_a.render(&wc);
    }
    let h_c = fnv1a(&renderer_a.read_pixels());
    assert_ne!(h_c, h_a[2], "phase_g 变化必须改变全帧哈希（防恒等假通过）");
}
