//! 集成测试：纹理 mip 链 + 各向异性过滤（R66，`MipSettings` 资源）。
//!
//! 三条主线（与 ARCHITECTURE §4 渲染纹理小节的验收标准一一对应）：
//! 1. **兼容（硬约束）**：`MipSettings` 缺席 = 与 R66 之前逐位一致——
//!    无纹理场景（只碰内置白色兜底）与 1×1 纹理场景（链长恒 1）在
//!    「缺席 / 在场」两种资源态下全帧 FNV 逐位一致：链上传与采样器
//!    改动对这两类纹理在数学上不可见（1×1 下 trilinear = bilinear、
//!    各向异性采样恒命中同一纹素），这是「改动面未外溢」的帧级证据；
//!    mip 关闭的高频缩小帧另锁 R66 文件基线哈希。
//! 2. **有效性（双向可判别，R53/R60 指标学）**：512² 高频棋盘格地面
//!    （4 纹素格、半透明 alpha → 恒走 RGBA8 路径，规避 BC1 单 mip），
//!    相机直下俯视使中心 ROI 处于 ≈12 纹素/像素的重度缩小：**关 mip
//!    的 ROI 亮度标准差必须 > 阈值（噪点存在），开 mip 必须 < 阈值
//!    （box 链把棋盘格均成灰）**——单一方向的「开了更平」不算数，两个
//!    阈值各自独立断言；ROI 取画面中心 ±12 px，远离地面边缘（混淆源）。
//! 3. **确定性**：同输入双会话（各自独立建世界 + 独立渲染器——句柄值
//!    缓存跨世界串味的既有教训）关/开两态全帧逐位一致。
//!
//! 环境无 GPU 适配器时优雅跳过（打印提示，测试仍算通过）。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 屏幕尺寸（测试统一口径）。
const SIZE: u32 = 512;

/// R66 mip 关闭态文件基线（高频棋盘俯视场景全帧 FNV-1a；本测试文件
/// 新建——非改任何既有锁。该帧不涉 mip 路径，锁的是「缺席路径零漂移」）。
const R66_MIP_OFF_BASELINE: u64 = 0x02f1_dc54_deae_ecd6;

/// ROI 半宽（中心 ±12 px；地面在屏内 ≈44 px 宽，ROI 距边缘 ≥9 px，
/// 避开几何边缘 / 背景混入的混淆源）。
const ROI_HALF: u32 = 12;

/// 有效性阈值的实测依据（DX12 适配器实测：关 ≈ 99.8、开 = 0.0，
/// 双向各留裕度，相差一个数量级以上）。
const OFF_STD_MIN: f64 = 60.0;
const ON_STD_MAX: f64 = 20.0;

/// **本套件串行化**（AGENTS.md 铁律：新写 GPU 套件直接带上互斥）：多项
/// 测试并行时各建 OffscreenRenderer 共用单适配器会互相拖死（R62b 症状
/// 谱：自旋挂死 / 空转死等 / 驱动段错误），文件内互斥把每帧 poll 的
/// 并发度压到 1。
static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 全帧像素 FNV-1a 64（兼容锁哈希，与既有 GPU 测试同款）。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// 世界坐标 → 像素坐标（与渲染器同一条相机路径，R62a 同款推导）。
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

/// 俯视棋盘格场景：512² / 4 纹素格棋盘地面（alpha 恒 128 → 非全不透明
/// ⇒ 恒走 RGBA8 上传路径，mip 链生效；材质为不透明，shader 不消费
/// alpha）。相机几乎垂直向下俯视，原点附近 ≈12 纹素/像素（重度缩小）。
/// 光照 = 纯环境光（直射关）：画面亮度 = 反照率 × 1，棋盘对比度最大
/// 且不受阴影 / 高光干扰。
fn checker_world(mip: bool) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    let camera = Camera {
        eye: Vec3::new(0.0, 39.8, 3.0),
        target: Vec3::ZERO,
        fov_y_radians: std::f32::consts::FRAC_PI_4,
        z_near: 0.1,
        z_far: 200.0,
        ..Camera::default()
    };
    world.insert_resource(camera);
    world.insert_resource(DirectionalLight {
        intensity: 0.0,
        ambient: Vec3::splat(1.0),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    if mip {
        world.insert_resource(MipSettings::default());
    }
    // 512²、4 纹素格；alpha 128 排除 BC1 路径（诚实口径：R66 不做压缩
    // 纹理多级化，测试对象必须是 RGBA8 路径）
    let tex = world
        .resource_mut::<TextureStore>()
        .add(CpuTexture::checkerboard(
            128,
            4,
            [0, 0, 0, 128],
            [255, 255, 255, 128],
        ));
    let mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
        texture: tex,
        ..MaterialDef::default()
    });
    let ground = world.resource_mut::<MeshStore>().add(MeshData::plane(4.0));
    world.spawn((Transform::IDENTITY, ground, mat));
    propagate_transforms(&mut world);
    world
}

/// BC1 压缩路径对照场景（R66d）：与 [`checker_world`] 同几何同相机，仅
/// 棋盘 alpha 恒 255（全不透明 ⇒ 命中 BC1 压缩上传）——BC 路径 mip 链的
/// 测试对象。R66 时压缩路径恒单 mip，本场景两态同噪；R66d 起在场态走
/// 块流链平滑（frictions ⑧ 皮肤点状噪点的病灶形态）。
fn checker_world_bc(mip: bool) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    let camera = Camera {
        eye: Vec3::new(0.0, 39.8, 3.0),
        target: Vec3::ZERO,
        fov_y_radians: std::f32::consts::FRAC_PI_4,
        z_near: 0.1,
        z_far: 200.0,
        ..Camera::default()
    };
    world.insert_resource(camera);
    world.insert_resource(DirectionalLight {
        intensity: 0.0,
        ambient: Vec3::splat(1.0),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    if mip {
        world.insert_resource(MipSettings::default());
    }
    let tex = world
        .resource_mut::<TextureStore>()
        .add(CpuTexture::checkerboard(
            128,
            4,
            [0, 0, 0, 255],
            [255, 255, 255, 255],
        ));
    let mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
        texture: tex,
        ..MaterialDef::default()
    });
    let ground = world.resource_mut::<MeshStore>().add(MeshData::plane(4.0));
    world.spawn((Transform::IDENTITY, ground, mat));
    propagate_transforms(&mut world);
    world
}

/// 纯色 1×1 纹理立方体场景（兼容对照用：链长恒 1，mip 路径数学上
/// 不可见）。`textured = false` 时材质用纯色（无任何纹理上传）。
fn simple_world(mip: bool, textured: bool) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 0.0, 4.0),
        target: Vec3::ZERO,
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight::default());
    world.insert_resource(BloomSettings::disabled());
    if mip {
        world.insert_resource(MipSettings::default());
    }
    let mat = if textured {
        let tex = world
            .resource_mut::<TextureStore>()
            .add(CpuTexture::solid([200, 60, 40, 255]));
        world.resource_mut::<MaterialStore>().add(MaterialDef {
            texture: tex,
            ..MaterialDef::default()
        })
    } else {
        world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.8, 0.3, 0.2, 1.0))
    };
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    world.spawn((Transform::IDENTITY, cube, mat));
    propagate_transforms(&mut world);
    world
}

/// ROI（中心 ±[`ROI_HALF`]）内的亮度标准差（亮度 = RGB 均值，0–255 刻度）。
fn roi_lum_std(px: &[u8], w: u32, cx: f32, cy: f32) -> f64 {
    let x0 = (cx as i32 - ROI_HALF as i32).max(0) as u32;
    let x1 = (cx as u32 + ROI_HALF).min(w - 1);
    let y0 = (cy as i32 - ROI_HALF as i32).max(0) as u32;
    let y1 = (cy as u32 + ROI_HALF).min(SIZE - 1);
    let mut lums = Vec::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            let i = ((y * w + x) * 4) as usize;
            lums.push((px[i] as u32 + px[i + 1] as u32 + px[i + 2] as u32) as f64 / 3.0);
        }
    }
    let mean = lums.iter().sum::<f64>() / lums.len() as f64;
    let var = lums.iter().map(|l| (l - mean) * (l - mean)).sum::<f64>() / lums.len() as f64;
    var.sqrt()
}

/// 兼容硬约束：MipSettings 缺席与在场，对「无纹理场景」（只碰内置白色
/// 兜底，根本不进 ensure_texture）与「1×1 纹理场景」（链长恒 1）的
/// 全帧像素逐位一致——链上传与采样器改动对这两类纹理不可见。
#[test]
fn mip_settings_absent_and_present_match_on_mip_invisible_scenes() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer_off) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut renderer_on) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    // 无纹理场景：MipSettings 只影响 ensure_texture，兜底路径不碰
    let world_off = simple_world(false, false);
    let world_on = simple_world(true, false);
    renderer_off.render(&world_off);
    renderer_on.render(&world_on);
    let off = renderer_off.read_pixels();
    let on = renderer_on.read_pixels();
    assert_eq!(
        fnv1a(&off),
        fnv1a(&on),
        "无纹理场景：MipSettings 在场必须逐位一致"
    );

    // 1×1 纹理场景：链长 1 + 各向异性对常色纹理采样恒等
    let w2_off = simple_world(false, true);
    let w2_on = simple_world(true, true);
    renderer_off.render(&w2_off);
    renderer_on.render(&w2_on);
    let off = renderer_off.read_pixels();
    let on = renderer_on.read_pixels();
    assert_eq!(
        fnv1a(&off),
        fnv1a(&on),
        "1×1 纹理场景：MipSettings 在场必须逐位一致"
    );
}

/// 有效性（双向可判别）+ 确定性：关 mip 的重度缩小 ROI 必须高方差
/// （噪点存在）、开 mip 必须低方差（box 链均灰），两阈值独立断言；
/// 同输入双会话（独立世界 + 独立渲染器）两态全帧逐位一致；关闭态另锁
/// R66 文件基线哈希。
#[test]
fn mip_off_aliases_and_mip_on_resolves_with_determinism() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer_off) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut renderer_on) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let camera = Camera {
        eye: Vec3::new(0.0, 39.8, 3.0),
        target: Vec3::ZERO,
        fov_y_radians: std::f32::consts::FRAC_PI_4,
        z_near: 0.1,
        z_far: 200.0,
        ..Camera::default()
    };
    let (cx, cy) = project_to_pixel(&camera, Vec3::ZERO, SIZE, SIZE);
    let world_off = checker_world(false);
    let world_on = checker_world(true);
    renderer_off.render(&world_off);
    renderer_on.render(&world_on);
    let off = renderer_off.read_pixels();
    let on = renderer_on.read_pixels();

    let off_std = roi_lum_std(&off, SIZE, cx, cy);
    let on_std = roi_lum_std(&on, SIZE, cx, cy);
    println!("mip 关 ROI 标准差 = {off_std:.2}，开 = {on_std:.2}（ROI 中心 {cx:.0},{cy:.0}）");
    println!(
        "mip 关帧 FNV = {:016x}，开帧 FNV = {:016x}",
        fnv1a(&off),
        fnv1a(&on)
    );
    assert!(
        off_std > OFF_STD_MIN,
        "关 mip 的重度缩小 ROI 必须高方差（噪点存在）：{off_std:.2} ≤ {OFF_STD_MIN}"
    );
    assert!(
        on_std < ON_STD_MAX,
        "开 mip 的重度缩小 ROI 必须低方差（链均灰）：{on_std:.2} ≥ {ON_STD_MAX}"
    );
    assert!(
        off_std > on_std * 3.0,
        "两态指标必须拉开数量级差距：{off_std:.2} vs {on_std:.2}"
    );
    assert_eq!(
        fnv1a(&off),
        R66_MIP_OFF_BASELINE,
        "mip 关闭态帧哈希 = R66 文件基线（缺席路径零漂移锁）"
    );

    // 确定性：独立世界 + 独立渲染器双跑，两态各自逐位一致
    let Some(mut renderer_off2) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut renderer_on2) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let world_off2 = checker_world(false);
    let world_on2 = checker_world(true);
    renderer_off2.render(&world_off2);
    renderer_on2.render(&world_on2);
    assert_eq!(
        fnv1a(&off),
        fnv1a(&renderer_off2.read_pixels()),
        "mip 关：同输入双会话必须逐位一致"
    );
    assert_eq!(
        fnv1a(&on),
        fnv1a(&renderer_on2.read_pixels()),
        "mip 开：同输入双会话必须逐位一致（各向异性采样确定性）"
    );
}

/// BC 压缩路径 mip 链（R66d）：全不透明棋盘命中 BC1 上传，重度缩小下
/// 关 mip（`MipSettings` 缺席 = 单 mip，与 R66 之前逐位一致）必须高方差
/// （块别名噪点存在）、开 mip 必须低方差（块流链平滑）——R66 时压缩路径
/// 恒单 mip、两态同噪（iris-exp frictions ⑧ 皮肤点状噪点的病灶形态），
/// 本测试双向阈值锁 R66d 修复后的行为。
#[test]
fn bc_path_mip_chain_suppresses_minify_aliasing() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer_off) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut renderer_on) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let camera = Camera {
        eye: Vec3::new(0.0, 39.8, 3.0),
        target: Vec3::ZERO,
        fov_y_radians: std::f32::consts::FRAC_PI_4,
        z_near: 0.1,
        z_far: 200.0,
        ..Camera::default()
    };
    let (cx, cy) = project_to_pixel(&camera, Vec3::ZERO, SIZE, SIZE);
    let world_off = checker_world_bc(false);
    let world_on = checker_world_bc(true);
    renderer_off.render(&world_off);
    renderer_on.render(&world_on);
    let off = renderer_off.read_pixels();
    let on = renderer_on.read_pixels();
    let off_std = roi_lum_std(&off, SIZE, cx, cy);
    let on_std = roi_lum_std(&on, SIZE, cx, cy);
    println!("BC 路径 mip 关 ROI 标准差 = {off_std:.2}，开 = {on_std:.2}");
    assert!(
        off_std > OFF_STD_MIN,
        "BC 路径关 mip 的重度缩小 ROI 必须高方差（单 mip 块别名）：{off_std:.2} ≤ {OFF_STD_MIN}"
    );
    assert!(
        on_std < ON_STD_MAX,
        "BC 路径开 mip 的重度缩小 ROI 必须低方差（块流链平滑）：{on_std:.2} ≥ {ON_STD_MAX}"
    );
    assert!(
        off_std > on_std * 3.0,
        "BC 路径两态指标必须拉开数量级差距：{off_std:.2} vs {on_std:.2}"
    );
}
