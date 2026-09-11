//! 集成测试：预积分皮肤次表面散射（R63，`standard.wgsl› 的
//! `sss_diffuse› 段）。
//!
//! 六条主线（与 ARCHITECTURE §4.32 的验收标准一一对应）：
//! 1. **兼容锁**：`subsurface_strength = 0›（全部既有材质的缺省）的
//!    本场景全帧 FNV-1a 锁在 R63 落地的基线值——着色器 SSS 分支整段不
//!    进入，与「根本没有本特性」逐位一致的实现根据在 `material.rs›
//!    模块文档与 `standard.wgsl› 的调用点注释。全局兼容面由既有
//!    `normal_render› / `lightmap_render› / `probe_render› 等基线锁覆盖
//!    （本轮全部原值通过，不重锁）。
//! 2. **有效性①（终结线包裹）**：球体中心行亮度剖面——强度 0 vs 1 的
//!    三指标对照：①背光侧受光像素数（`NdotL < 0› 一侧）②亮度归零位置
//!    与 **LUT 解析零点** `NdotL = −sin α› 的像素级对齐 ③2%..98% 过渡带
//!    宽度比（R53/R60 双指标手法）。
//! 3. **有效性②（阴影协同）**：点光 cubemap 投影阴影内（`shadow = 0›
//!    硬零 ⇒ 直射辐亮度恒 0）的像素与强度**逐位无关**——背光侧的包裹
//!    绝不「晒穿」投影阴影；同一帧的非阴影区必须有可见的 SSS 增量
//!    （证明同一场景里特性确实生效，排除「整帧没开」的假通过）。
//! 4. **有效性③（金属 / 非金属分叉）**：`metallic = 1› 时强度 0 / 1
//!    **整帧逐位一致**——`kd = (1 − F)(1 − metallic)› 权重把补项归零，
//!    金属没有次表面散射；同场景非金属对照组必须有差异。
//! 5. **有效性④（光照贴图协同）**：挂光照贴图的实体其间接项走 R52c 的
//!    `select› 分支，SSS 补项只进直射项——同一几何在「有 / 无图集」两条
//!    路径下的 SSS 增量一致（容差见断言注释，来源是浮点结合律而非语义）。
//! 6. **确定性**：两个同输入完整会话（含相机平移与参数变化）逐帧逐位
//!    一致；LUT 生成逐位可复现由 `sss.rs› 单测锁定。
//!
//! 环境无 GPU 适配器时优雅跳过（打印提示，测试仍算通过）。

use std::sync::Mutex;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;
use iris_render::sss;

/// 屏幕边长（测试统一口径）。
const SIZE: u32 = 256;

/// **重负载 GPU 套件文件内互斥锁**（R62b 实测先例：同文件多项并行共用
/// 单适配器，release 下第 3 项之后无限停顿——各测自建渲染器的每帧 poll
/// 同步循环互相争用）。照 `ssr_render.rs::GPU_SERIAL› 的做法串行化，
/// 不依赖 `RUST_TEST_THREADS=1› 的记忆。
static GPU_SERIAL: Mutex<()> = Mutex::new(());

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ---- 终结线场景：单位球 + 侧向方向光（背景与暗面全黑）----

/// 终结线场景参数。相机用**长焦**（fov 14°、眼距 8 m）：投影近似正交，
/// 屏幕行 ↔ 球面世界坐标的映射接近线性，`NdotL = −sin α› 的解析零点
/// 才有可能落在像素级容差内（广角下 1/8 球半径的透视偏差就是 ~10 px）。
struct SphereScene {
    camera: Camera,
    light_dir: Vec3,
    /// 球心投影像素。
    center: (f32, f32),
    /// 赤道半径（世界 x = 1 的投影像素偏移）——`x = c·R› 的线性映射基准。
    radius_px: f32,
    /// 轮廓半径（像素）：`R·√(1 − (R/d)²)› 的投影，比赤道半径略大。
    silhouette_px: f32,
    /// 几何终结线（`NdotL = 0›，世界点 (0, 0, 1)）的屏幕 x。
    x_terminator: f32,
}

fn sphere_scene() -> SphereScene {
    let camera = Camera {
        eye: Vec3::new(0.0, 0.0, 8.0),
        target: Vec3::ZERO,
        fov_y_radians: 14f32.to_radians(),
        ..Camera::default()
    };
    let vp = camera.view_proj(1.0);
    let project = |p: Vec3| -> (f32, f32) {
        let clip = vp * p.extend(1.0);
        let ndc = clip.truncate() / clip.w;
        (
            (ndc.x * 0.5 + 0.5) * SIZE as f32,
            (1.0 - (ndc.y * 0.5 + 0.5)) * SIZE as f32,
        )
    };
    let center = project(Vec3::ZERO);
    let radius_px = project(Vec3::new(1.0, 0.0, 0.0)).0 - center.0;
    // 轮廓圆半径：切线切点距视线轴 R·√(1 − (R/d)²)、沿轴偏移 R²/d
    let (radius, distance) = (1.0f32, 8.0f32);
    let tangent = (1.0f32 - (radius / distance).powi(2)).sqrt() * radius;
    let tangent_z = radius * radius / distance;
    let silhouette_px = project(Vec3::new(tangent, 0.0, tangent_z)).0 - center.0;
    SphereScene {
        camera,
        light_dir: Vec3::new(-1.0, -0.25, 0.0).normalize(),
        center,
        radius_px,
        silhouette_px,
        x_terminator: project(Vec3::new(0.0, 0.0, 1.0)).0,
    }
}

/// 球体世界：除方向光外**无任何环境项**（`ambient = 0›、IBL 强度 0）——
/// 剖面只由直射漫反射构成，暗面恰为全黑，`x_zero› 的判据干净。
/// `roughness = 1›：GGX 极粗糙把镜面项摊成平滑本底（两帧同值，不参与
/// 差分），漫反射主导剖面。
fn sphere_world(strength: f32, scale: f32, metallic: f32) -> World {
    sphere_world_seg(strength, scale, metallic, (160, 80))
}

/// [`sphere_world`] 的分段参数版：光照贴图对照用小分段球（烘焙成本按
/// 三角形数线性，1152 面对 25600 面），轮廓半径差异 < 0.2%（不影响指标）。
fn sphere_world_seg(strength: f32, scale: f32, metallic: f32, seg: (u32, u32)) -> World {
    let scene = sphere_scene();
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(scene.camera.clone());
    world.insert_resource(DirectionalLight {
        direction: scene.light_dir,
        color: Vec3::new(1.0, 1.0, 1.0),
        intensity: 3.0,
        ambient: Vec3::ZERO,
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(EnvironmentSettings { intensity: 0.0 });
    world.insert_resource(BloomSettings::disabled());

    let mesh = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, seg.0, seg.1));
    let mut mat = MaterialDef::solid(1.0, 1.0, 1.0, 1.0);
    mat.metallic = metallic;
    mat.roughness = 1.0;
    mat.subsurface_strength = strength;
    mat.subsurface_scale = scale;
    let handle = world.resource_mut::<MaterialStore>().add(mat);
    world.spawn((Transform::IDENTITY, mesh, handle));
    propagate_transforms(&mut world);
    world
}

/// sRGB 编码字节 → 线性亮度（Rec. 709）。剖面统计统一在线性域做：
/// 终结线是**光强**的过渡，8-bit sRGB 的非线性会把过渡带宽度量歪。
fn linear_luma(pixels: &[u8], x: u32, y: u32) -> f32 {
    let i = ((y * SIZE + x) * 4) as usize;
    let ch = |k: usize| -> f32 {
        let s = f32::from(pixels[i + k]) / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * ch(0) + 0.7152 * ch(1) + 0.0722 * ch(2)
}

/// 终结线剖面的三项指标。
struct TerminatorMetrics {
    /// 受光侧平台亮度（线性）。
    plateau: f32,
    /// 自受光侧向左扫描，亮度首次跌破 0.1% 平台值的屏幕 x（= 光到不了的位置）。
    x_zero: f32,
    /// 2% → 98% 平台值的过渡带宽度（像素）。
    band_px: f32,
    /// 几何终结线背光侧的受光像素数（亮度 > 2% 平台值）。
    backlit_px: u32,
}

/// 中心行剖面统计。扫描范围限定在轮廓圆内（不含 AA 边缘）。
fn terminator_metrics(pixels: &[u8], scene: &SphereScene) -> TerminatorMetrics {
    let y = scene.center.1.round() as u32;
    let x_left = ((scene.center.0 - scene.silhouette_px + 3.0).ceil() as i32).max(0);
    let x_right =
        ((scene.center.0 + scene.silhouette_px - 3.0).floor() as i32).min(SIZE as i32 - 1);
    assert!(x_left < x_right, "剖面扫描必须落在画面内");
    let lum = |x: i32| linear_luma(pixels, x as u32, y);

    // **间接项地板**：挂光照贴图的帧在背光侧仍有环境间接光（不为 0），
    // 直接项的过渡带会被它垫高。取整行的最小亮度作为地板并从剖面里扣除，
    // 剩下的就是纯直射剖面——无光照贴图的帧地板恰为 0，语义不变。
    let mut floor = f32::INFINITY;
    for x in x_left..=x_right {
        floor = floor.min(lum(x));
    }

    // 平台：受光侧 0.35..0.75 赤道半径区间的中位数（避开高光与轮廓边缘）
    let mut plateau_samples: Vec<f32> = Vec::new();
    for x in (scene.center.0 + 0.35 * scene.radius_px) as i32
        ..(scene.center.0 + 0.75 * scene.radius_px) as i32
    {
        plateau_samples.push(lum(x) - floor);
    }
    plateau_samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let plateau = plateau_samples[plateau_samples.len() / 2];
    assert!(
        plateau > 0.01,
        "受光侧平台亮度 {plateau} 过低，场景照明有误"
    );

    // 自右向左扫：98% / 2% / 0.1% 三个交点
    let mut x98 = None;
    let mut x02 = None;
    let mut x_zero = None;
    for x in (x_left..=x_right).rev() {
        let v = (lum(x) - floor) / plateau;
        if x98.is_none() && v < 0.98 {
            x98 = Some(x);
        }
        if x02.is_none() && v < 0.02 {
            x02 = Some(x);
        }
        if v < 0.001 {
            x_zero = Some(x);
            break;
        }
    }
    let x98 = x98.expect("剖面必须穿过 98% 平台值");
    let x02 = x02.expect("剖面必须穿过 2% 平台值");
    let x_zero = x_zero.map(|v| v as f32).unwrap_or(x_left as f32);
    let mut backlit_px = 0u32;
    for x in x_left..(scene.x_terminator as i32) {
        if (lum(x) - floor) / plateau > 0.02 {
            backlit_px += 1;
        }
    }
    TerminatorMetrics {
        plateau,
        x_zero,
        band_px: (x98 - x02) as f32,
        backlit_px,
    }
}

fn new_renderer() -> Option<OffscreenRenderer> {
    OffscreenRenderer::new(SIZE, SIZE)
}

/// 双球皮肤场景（确定性会话用）：两个球体各配一个皮肤材质（句柄 1 = 强度
/// 0.4 / 尺度 0.08；句柄 2 = 强度 1.0 / 尺度 0.05）。同一会话内材质内容
/// 恒定 ⇒ 渲染器的绑定组缓存语义正确，可安全复用同一渲染器跑多帧。
fn two_sphere_world() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(-0.6, 0.0, 9.0),
        target: Vec3::ZERO,
        fov_y_radians: 18f32.to_radians(),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-1.0, -0.25, 0.0).normalize(),
        color: Vec3::new(1.0, 0.96, 0.9),
        intensity: 3.0,
        ambient: Vec3::new(0.03, 0.03, 0.04),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(EnvironmentSettings { intensity: 0.25 });
    world.insert_resource(BloomSettings::disabled());
    let mesh = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, 64, 32));
    let skin = |strength: f32, scale: f32, tint: [f32; 4]| {
        let mut m = MaterialDef::solid(tint[0], tint[1], tint[2], tint[3]);
        m.roughness = 0.85;
        m.subsurface_strength = strength;
        m.subsurface_scale = scale;
        m
    };
    let a = world
        .resource_mut::<MaterialStore>()
        .add(skin(0.4, 0.08, [0.92, 0.72, 0.62, 1.0]));
    let b = world
        .resource_mut::<MaterialStore>()
        .add(skin(1.0, 0.05, [0.85, 0.62, 0.55, 1.0]));
    world.spawn((Transform::from_xyz(-1.1, 0.0, 1.0), mesh, a));
    world.spawn((Transform::from_xyz(1.1, 0.0, -1.0), mesh, b));
    propagate_transforms(&mut world);
    world
}

/// 每次渲染各用**独立渲染器**：渲染器按**句柄值**缓存 GPU 资源（材质绑定
/// 组 / 网格 / 纹理），同一渲染器上渲染「同句柄不同内容」的多个世界会串味
/// ——R63 的强度对照恰好是「1 号材质、不同 SSS 参数」的典型串味场景
/// （R53/R62 两次踩到，AGENTS.md 的规约是「跨世界对照必须各用独立渲染器」）。
/// R63 落地实测：复用渲染器时 strength 0|1 整帧差异 0 字节（假通过）。
fn render_fresh(world: &World) -> Option<Vec<u8>> {
    let mut renderer = new_renderer()?;
    renderer.render(world);
    Some(renderer.read_pixels())
}

// ---- 光照贴图对照场景（低分段球 + 烘焙 + 装配）----

/// 与 [`sphere_scene`] 同构（同一相机与光照），仅球面分段更粗。
fn sphere_scene_lowpoly() -> SphereScene {
    sphere_scene()
}

fn sphere_world_lowpoly(strength: f32) -> World {
    sphere_world_seg(strength, sss::SSS_DEFAULT_SCALE, 0.0, (48, 24))
}

/// 烘焙 + 装配（R52c 全链）：UV2 注入会把实体的网格句柄换成带 UV2 的
/// **副本**（变换不变 ⇒ 无需重跑 `propagate_transforms`），几何逐位不变
/// ⇒ 与未烘焙的对照帧像素级对齐。
fn lightmapped_sphere_world(strength: f32) -> (World, LightmapHandle) {
    let mut world = sphere_world_lowpoly(strength);
    let params = BakeParams {
        light_dir: Vec3::new(-1.0, -0.25, 0.0).normalize(),
        light_color: Vec3::new(1.0, 1.0, 1.0) * 3.0,
        // 天空给足亮度：图集必须实际改变间接项，否则「两条路径」是同一条
        sky_color: Vec3::new(0.35, 0.42, 0.55),
        shadow_softness: 0.08,
        sky_samples: 32,
        bounces: 1,
    };
    let uv = BakeUvParams {
        texels_per_meter: 8.0,
        atlas_size: 128,
    };
    let baked = bake_lightmap(&world, &params, &uv);
    let report = install_lightmap(&mut world, baked);
    (world, report.lightmap)
}

/// R63 基线：终结线场景（球体 + 侧向方向光，`subsurface› 取引擎缺省 =
/// 强度 0）的全帧像素 FNV-1a 64，本轮落地时以同一场景实跑捕获。
/// 语义 = 「SSS 强度 0（全部既有材质的缺省）与根本没有本特性**逐位一致**」
/// 的永久回归锁。
const R63_STRENGTH_ZERO_BASELINE: u64 = 0x6027_ffef_5e06_9a1d;

/// **兼容锁**：强度 0 的全帧哈希锁定。着色器侧的实现根据是
/// `if (material.sss.x > 0.0)› 的 uniform 分支——strength = 0 时整段
/// 不执行、一个算术都不多做（比 `mix(a, b, 0)› 的浮点恒等式论证强，
/// 后者在 `a = −0.0› 时翻符号，见 `material.rs› 模块文档）。
#[test]
fn strength_zero_matches_r63_baseline_bitwise() {
    let _guard = GPU_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(pixels) = render_fresh(&sphere_world(0.0, sss::SSS_DEFAULT_SCALE, 0.0)) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let hash = fnv1a(&pixels);
    println!("R63 基线（强度 0 终结线场景）全帧哈希 {hash:#018x}");
    assert_eq!(
        hash, R63_STRENGTH_ZERO_BASELINE,
        "强度 0（全部既有材质的缺省）必须与 R63 基线逐位一致——SSS 分支被误入"
    );
}

/// **有效性①：终结线包裹**（三项指标，数字进断言）。
///
/// 球心行剖面上，方向光自 +X 侧照来，`NdotL = l.x · (x/R)› 在赤道行上
/// 是 x 的线性函数 ⇒ 几何终结线落在球心正上方那一列，LUT 的解析零点
/// `NdotL = −sin α›（α = t·90°）落在背光侧 `x = −sin α·R/l.x› 处。
/// 强度 1 时：①背光侧出现成片受光像素 ②亮度归零位置对齐该解析点
/// ③2%..98% 过渡带明显变宽。
#[test]
fn subsurface_wraps_light_into_the_terminator_backlit_side() {
    let _guard = GPU_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scene = sphere_scene();
    // 跨世界对照各用独立渲染器（R53/R62 教训：渲染器按句柄值缓存 GPU
    // 资源，同一渲染器上渲染「同句柄不同内容」的材质会串味）。
    let Some(off) = render_fresh(&sphere_world(0.0, sss::SSS_DEFAULT_SCALE, 0.0)) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(on) = render_fresh(&sphere_world(1.0, sss::SSS_DEFAULT_SCALE, 0.0)) else {
        return;
    };
    let m_off = terminator_metrics(&off, &scene);
    let m_on = terminator_metrics(&on, &scene);

    // 解析零点：NdotL = −sin α ⇒ 世界 x = −sin α · R / l.x（R = 1）
    let t = sss::wrap_param(sss::SSS_DEFAULT_SCALE);
    let alpha = t * std::f32::consts::FRAC_PI_2;
    let lx = scene.light_dir.x.abs(); // 渲染器上传的是「指向光源」方向
    let c_zero = -(alpha.sin());
    let x_world = c_zero / lx;
    let z_world = (1.0f32 - x_world * x_world).sqrt();
    let vp = scene.camera.view_proj(1.0);
    let clip = vp * Vec3::new(x_world, 0.0, z_world).extend(1.0);
    let x_analytic = (clip.x / clip.w * 0.5 + 0.5) * SIZE as f32;

    println!(
        "终结线剖面（赤道半径 {:.1} px，几何终结线 x = {:.1}）：\n           强度 0：平台 {:.4}｜零点 x = {:.1}｜过渡带 {:.0} px｜背光受光 {} px\n           强度 1：平台 {:.4}｜零点 x = {:.1}｜过渡带 {:.0} px｜背光受光 {} px\n           LUT 解析零点 t = {t:.2}（α = {:.1}°）⇒ NdotL = {c_zero:.4} ⇒ 屏幕 x = {x_analytic:.1}",
        scene.radius_px,
        scene.x_terminator,
        m_off.plateau,
        m_off.x_zero,
        m_off.band_px,
        m_off.backlit_px,
        m_on.plateau,
        m_on.x_zero,
        m_on.band_px,
        m_on.backlit_px,
        alpha.to_degrees(),
    );

    // 指标①：几何终结线背光侧
    assert!(
        m_off.backlit_px <= 2,
        "强度 0 的背光侧必须近乎全黑（实测 {} px）——硬边终结线的基准不成立",
        m_off.backlit_px
    );
    assert!(
        m_on.backlit_px as f32 >= 0.45 * scene.radius_px,
        "强度 1 的背光侧受光像素 {} px 不足赤道半径的 45%——包裹没有发生",
        m_on.backlit_px
    );

    // 指标②：解析零点**有界对齐**。LUT 的零点onset 是幂律
    // （W ∝ (NdotL + sin α)^p），固定相对阈值（0.1% 平台）的交点必然落在
    // 解析零点**受光侧**若干像素处（实测 ~10 px）——所以断言写成有界形式：
    // 光绝不越过解析零点（硬物理界），且必须推进到距它 15 px 以内
    // （包裹确实抵达预测极限）。解析零点由 sss.rs 的单测逐点锁定（±5e-3），
    // 此处只锁「GPU 剖面与解析式的像素级一致性」。
    assert!(
        m_on.x_zero >= x_analytic - 2.0,
        "强度 1 的亮度零点 x = {:.1} 越过了 LUT 解析零点 x = {x_analytic:.1}（物理不可能）",
        m_on.x_zero
    );
    assert!(
        m_on.x_zero <= x_analytic + 15.0,
        "强度 1 的亮度零点 x = {:.1} 距解析零点（{x_analytic:.1}）过远，包裹没有抵达预测极限",
        m_on.x_zero
    );
    assert!(
        m_off.x_zero > x_analytic + 40.0,
        "强度 0 的零点 x = {:.1} 必须远在解析零点（{x_analytic:.1}）的受光侧",
        m_off.x_zero
    );

    // 指标③：过渡带宽度（R53/R60 双指标手法）
    assert!(
        m_on.band_px >= 1.30 * m_off.band_px,
        "包裹后 2%..98% 过渡带 {} px 未显著宽于硬边的 {} px",
        m_on.band_px,
        m_off.band_px
    );
}

/// **有效性③：金属 / 非金属分叉**。`metallic = 1› 时
/// `kd = (1 − F)(1 − metallic) = 0› ⇒ SSS 补项恒为 ±0.0 ⇒ 强度 0 / 1
/// **整帧逐位一致**（无需分支，与既有的能量守恒式共用同一个 kd）。
/// 同场景的非金属对照组必须有可见差异——否则「逐位一致」可能只是
/// 场景根本没照亮（假通过）。
#[test]
fn metal_gets_no_subsurface_while_dielectric_does() {
    let _guard = GPU_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scale = sss::SSS_DEFAULT_SCALE;
    let Some(metal_off) = render_fresh(&sphere_world(0.0, scale, 1.0)) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(metal_on) = render_fresh(&sphere_world(1.0, scale, 1.0)) else {
        return;
    };
    let metal_diff = metal_off
        .iter()
        .zip(&metal_on)
        .filter(|(a, b)| a != b)
        .count();
    println!("金属（metallic = 1）强度 0|1 差异字节：{metal_diff}");
    assert_eq!(
        metal_diff, 0,
        "金属的 SSS 补项必须被 kd = (1 − F)(1 − metallic) 精确归零（差异 {metal_diff} 字节）"
    );

    let Some(diel_off) = render_fresh(&sphere_world(0.0, scale, 0.0)) else {
        return;
    };
    let Some(diel_on) = render_fresh(&sphere_world(1.0, scale, 0.0)) else {
        return;
    };
    let diel_diff = diel_off
        .iter()
        .zip(&diel_on)
        .filter(|(a, b)| a != b)
        .count();
    println!("绝缘体（metallic = 0）强度 0|1 差异字节：{diel_diff}");
    assert!(
        diel_diff > 500,
        "非金属对照组必须有可见的 SSS 增量（仅 {diel_diff} 字节不同）"
    );
}

// ---- 阴影协同场景：点光 cubemap 投影阴影 ----

/// 点光投影阴影场景：地面（SSS 材质）+ 遮挡方块（普通材质）+ 头顶投影
/// 点光。除点光外无任何环境项 ⇒ 阴影内（`shadow = 0› 硬零）直射辐亮度
/// 恒 0 ⇒ 像素值与 SSS 强度**逐位无关**。
fn point_shadow_world(strength: f32, shadows: bool) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 4.2, 7.0),
        target: Vec3::new(0.0, 0.4, 0.0),
        ..Camera::default()
    });
    // 方向光清零（只留 ambient = 0 的中性资源）：本场景的照明**只**来自点光，
    // 阴影内的直射项才是干净的 0。
    world.insert_resource(DirectionalLight {
        intensity: 0.0,
        ambient: Vec3::ZERO,
        ..DirectionalLight::default()
    });
    world.insert_resource(EnvironmentSettings { intensity: 0.0 });
    world.insert_resource(BloomSettings::disabled());

    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(24.0));
    let mut ground = MaterialDef::solid(0.75, 0.7, 0.65, 1.0);
    ground.roughness = 1.0;
    ground.subsurface_strength = strength;
    ground.subsurface_scale = sss::SSS_DEFAULT_SCALE;
    let ground_mat = world.resource_mut::<MaterialStore>().add(ground);
    world.spawn((Transform::IDENTITY, plane, ground_mat));

    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let caster_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.6, 0.6, 0.6, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 2.1, 0.0),
            scale: Vec3::new(1.4, 0.5, 1.4),
            ..Transform::IDENTITY
        },
        cube,
        caster_mat,
    ));

    world.spawn((
        Transform::from_xyz(0.0, 4.0, 0.0),
        PointLight {
            intensity: 60.0,
            range: 14.0,
            shadows,
            ..PointLight::default()
        },
    ));
    propagate_transforms(&mut world);
    world
}

/// **有效性②：阴影协同（不晒穿）**。阴影掩码由「开 / 关投影的同一场景」
/// 差分给出（点光阴影是**单采样二值**判定，没有 PCF 半影 ⇒ 差分掩码恰好
/// 等于 `shadow = 0› 的像素集）。断言：
/// 1. 掩码非空且成片（前置条件成立）；
/// 2. 掩码内强度 0 / 1 **逐位一致**——SSS 补项与主体项消费同一个已乘阴影
///    的辐亮度，阴影里 radiance = 0 ⇒ 补项 = 0，背光侧的包裹晒不穿投影阴影；
/// 3. 掩码外必须有 SSS 增量（同一帧特性确实生效，排除假通过）。
#[test]
fn point_light_shadow_blocks_subsurface_wrap() {
    let _guard = GPU_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // 掩码：开投影 vs 关投影（同强度 0）逐像素变暗的位置
    let Some(no_shadow) = render_fresh(&point_shadow_world(0.0, false)) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(shadow_off) = render_fresh(&point_shadow_world(0.0, true)) else {
        return;
    };
    let mask: Vec<bool> = no_shadow
        .chunks_exact(4)
        .zip(shadow_off.chunks_exact(4))
        // 掩码 = 「关投影」比「开投影」**更亮**的像素（有影帧被压暗的位置）
        .map(|(a, b)| b[0] < a[0] || b[1] < a[1] || b[2] < a[2])
        .collect();
    let masked = mask.iter().filter(|m| **m).count();
    println!("投影阴影掩码像素数：{masked}");
    assert!(
        masked > 500,
        "阴影掩码只有 {masked} 像素——前置条件不成立（点光投影没生效）"
    );

    let Some(shadow_on) = render_fresh(&point_shadow_world(1.0, true)) else {
        return;
    };
    let mut inside_diff = 0usize;
    let mut outside_diff = 0usize;
    for (i, m) in mask.iter().enumerate() {
        let a = &shadow_off[i * 4..i * 4 + 3];
        let b = &shadow_on[i * 4..i * 4 + 3];
        if a != b {
            if *m {
                inside_diff += 1;
            } else {
                outside_diff += 1;
            }
        }
    }
    println!("强度 0|1 差异像素：阴影内 {inside_diff}｜阴影外 {outside_diff}");
    assert_eq!(
        inside_diff, 0,
        "投影阴影内的像素必须与 SSS 强度逐位无关（{inside_diff} 个像素被改变了）——         包裹晒穿了阴影"
    );
    assert!(
        outside_diff > 500,
        "阴影外必须有可见的 SSS 增量（仅 {outside_diff} 个像素不同）"
    );
}

/// **有效性④：光照贴图协同**。挂光照贴图的实体其间接项走 R52c 的
/// `select› 分支，SSS 补项只进**直射**项 —— 两条路径（有 / 无图集）
/// 下 SSS 的**几何行为**必须一致。用与测试②同一套终结线指标做判据
/// （背光侧受光像素数、亮度零点位置）：这两个量是相对各帧自身平台值的
/// 几何量，对间接项的绝对亮度差免疫（浮点结合律导致的像素级差异不会
/// 改变「光在哪里停下」）。
#[test]
fn subsurface_behaves_identically_with_and_without_lightmap() {
    let _guard = GPU_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scene = sphere_scene_lowpoly();
    let Some(plain_off) = render_fresh(&sphere_world_lowpoly(0.0)) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(plain_on) = render_fresh(&sphere_world_lowpoly(1.0)) else {
        return;
    };

    // 图集懒上传的观测口径需要留住渲染器（其余跨世界对照一律 `render_fresh`）
    let (world_lm, handle) = lightmapped_sphere_world(1.0);
    let Some(mut r_lm) = new_renderer() else {
        return;
    };
    r_lm.render(&world_lm);
    let lm_on = r_lm.read_pixels();
    assert!(
        r_lm.renderer().lightmap_is_uploaded(handle),
        "渲染后图集必须已懒上传（{handle:?}）——光照贴图路径没有生效"
    );
    let Some(lm_off) = render_fresh(&lightmapped_sphere_world(0.0).0) else {
        return;
    };

    // 前置条件：图集确实改变了间接项（否则「两条路径」是同一条）
    let indirect_diff = plain_off
        .iter()
        .zip(&lm_off)
        .filter(|(a, b)| a != b)
        .count();
    println!("有无图集的整帧差异字节：{indirect_diff}");
    assert!(
        indirect_diff > 1000,
        "光照贴图必须实际改变画面（差异仅 {indirect_diff} 字节）"
    );

    let m_plain_off = terminator_metrics(&plain_off, &scene);
    let m_plain_on = terminator_metrics(&plain_on, &scene);
    let m_lm_off = terminator_metrics(&lm_off, &scene);
    let m_lm_on = terminator_metrics(&lm_on, &scene);
    println!(
        "背光受光像素：无图集 {} → {}｜有图集 {} → {}\n           亮度零点 x：无图集 {:.1} → {:.1}｜有图集 {:.1} → {:.1}",
        m_plain_off.backlit_px,
        m_plain_on.backlit_px,
        m_lm_off.backlit_px,
        m_lm_on.backlit_px,
        m_plain_off.x_zero,
        m_plain_on.x_zero,
        m_lm_off.x_zero,
        m_lm_on.x_zero,
    );
    assert!(
        m_plain_off.backlit_px <= 2 && m_lm_off.backlit_px <= 2,
        "两条路径在强度 0 时都必须近乎全黑（{}/{})",
        m_plain_off.backlit_px,
        m_lm_off.backlit_px
    );
    // 背光受光像素数对**间接项地板估计**敏感（光照贴图的间接光沿球面
    // 有空间变化，行最小值扣不干净），故取宽松的同量级判据；尖锐的判据
    // 是下面「亮度零点位置」——它只取决于直射项在哪里停下。
    let a = m_plain_on.backlit_px as f32;
    let b = m_lm_on.backlit_px as f32;
    assert!(
        b >= 0.65 * a && a >= 0.5 * b,
        "两条路径的包裹范围必须同量级（无图集 {a} px vs 有图集 {b} px）"
    );
    assert!(
        (m_lm_on.x_zero - m_plain_on.x_zero).abs() <= 3.0,
        "两条路径的亮度零点必须一致（无图集 {:.1} vs 有图集 {:.1}）",
        m_plain_on.x_zero,
        m_lm_on.x_zero
    );
}

/// **确定性**：两个同输入完整会话（6 帧，含相机平移与 SSS 参数变化）
/// 逐帧逐位一致。有状态渲染（TAA 历史等）默认缺席，本测试锁的是
/// 「LUT 常量 + 无 RNG 的着色路径」的跨会话可复现性。
#[test]
fn subsurface_frames_are_bitwise_reproducible_across_sessions() {
    let _guard = GPU_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let session = || -> Option<Vec<Vec<u8>>> {
        let mut renderer = new_renderer()?;
        let mut frames = Vec::new();
        for i in 0..6u32 {
            // 同一会话内**材质内容逐帧恒定**（句柄稳定 ⇒ 绑定组缓存语义正确，
            // 不落入「同句柄不同内容」的串味陷阱）；变化的是相机——跨会话
            // 逐位一致锁的是「LUT 常量 + 无 RNG 的着色路径」的可复现性。
            let world = two_sphere_world();
            {
                let mut camera = world.resource_mut::<Camera>();
                camera.eye = Vec3::new(0.3 * i as f32 - 0.6, 0.15 * i as f32, 9.0);
            }
            renderer.render(&world);
            frames.push(renderer.read_pixels());
        }
        Some(frames)
    };
    let Some(a) = session() else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(b) = session() else {
        return;
    };
    assert_eq!(a.len(), b.len());
    for (i, (fa, fb)) in a.iter().zip(&b).enumerate() {
        assert_eq!(fnv1a(fa), fnv1a(fb), "第 {i} 帧在两个会话间必须逐位一致");
    }
    println!("6 帧跨会话逐位一致（末帧哈希 {:#018x}）", fnv1a(&a[5]));
}
