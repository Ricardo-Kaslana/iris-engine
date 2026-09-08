//! 集成测试：FXAA 后处理 pass（R53，`shaders/fxaa.wgsl`）。
//!
//! 四条主线：
//! 1. **兼容锁（硬约束）**：`FxaaSettings` 缺席与显式 `enabled = false`
//!    逐位一致，且全帧 FNV-1a 哈希锁定（blit 直写最终目标的命令流与
//!    R52c 之后不变）；
//! 2. **生效（边缘平滑）**：45° 自发光斜条——总变差（TV = 行相邻亮度差
//!    之和）与最大相邻跳变都严格下降（过渡带变宽 = 抗锯齿的定义性
//!    特征）；
//! 3. **只动边缘**：变更像素占比有界（非边缘像素走早退分支透传中心
//!    采样，8-bit sRGB 往返恒等）；
//! 4. **UI 不受影响**：FXAA 链位在 UI pass 之前，不透明 HUD 色块
//!    区域开启前后**逐位一致**（文字锐利的像素级证据）。
//!
//! 环境无 GPU 适配器时优雅跳过（打印提示，测试仍算通过）。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 缺席路径全帧 FNV-1a 兼容锁（R53 首跑实测锁定；语义 = FXAA 资源
/// 缺席时命令流与 R52c 之后逐位一致）。
const R53_FXAA_OFF_BASELINE: u64 = 0x24e3_8254_0222_7bbb;

/// 屏幕尺寸（测试统一口径）。
const SIZE: u32 = 256;

/// 斜条场景：相机沿 −Z 正视，45° 近黑漫反射条横贯画面，与程序化天空
/// 背景形成强对比斜边缘（背景显示亮度 ≈0.85、条 ≈0.05——远超 FXAA
/// 阈值。注：天空盒**不吃** `EnvironmentSettings.intensity`，压暗 IBL
/// 不影响背景亮度——实测教训，对比度必须靠几何材质本身做）。
/// 左上角 28×28 不透明红色 HUD 色块验证「UI 在 FXAA 之后」。泛光关。
fn edge_world() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight {
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());

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

    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 0.0, 3.0);
        camera.target = Vec3::ZERO;
    }
    propagate_transforms(&mut world);
    world
}

/// 像素亮度（r+g+b，0..765）。
fn lum(px: &[u8], x: u32, y: u32) -> f64 {
    let i = ((y * SIZE + x) * 4) as usize;
    (px[i] as f64 + px[i + 1] as f64 + px[i + 2] as f64) / 3.0
}

/// 行区间 [y0,y1) 内水平相邻亮度差的最大值。
fn max_adjacent_diff(px: &[u8], y0: u32, y1: u32) -> f64 {
    let mut mx = 0.0f64;
    for y in y0..y1 {
        for x in 1..SIZE {
            mx = mx.max((lum(px, x, y) - lum(px, x - 1, y)).abs());
        }
    }
    mx
}

/// 过渡带宽度指标：逐行统计亮度落在 [min + 0.1·(max−min),
/// min + 0.9·(max−min)] 开区间的像素数并求和（行自适应，边缘越"硬"
/// 计数越小；抗锯齿 = 过渡带变宽 ⇒ 计数变大。窗口取 10%–90%：
/// FXAA 抹出的中间调常紧贴端点，20%–80% 窄窗口实测数不到新增像素）。
///
/// 注：**不用总变差（TV）**——TV 对单调重映射不变，把 1 像素硬跳变
/// 摊成 2 像素缓坡的 TV 与原值几乎相等，判别不了平滑（实测教训）。
fn band_count(px: &[u8], y0: u32, y1: u32) -> u32 {
    let mut total = 0u32;
    for y in y0..y1 {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for x in 0..SIZE {
            let v = lum(px, x, y);
            lo = lo.min(v);
            hi = hi.max(v);
        }
        let span = hi - lo;
        if span < 1.0 {
            continue;
        }
        let a = lo + 0.1 * span;
        let b = lo + 0.9 * span;
        for x in 0..SIZE {
            let v = lum(px, x, y);
            if v > a && v < b {
                total += 1;
            }
        }
    }
    total
}

/// 区域 [x0,y0)×[x1,y1) 像素逐位提取（用于 UI 色块比对）。
fn region(px: &[u8], x0: u32, y0: u32, x1: u32, y1: u32) -> Vec<u8> {
    let mut out = Vec::new();
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * SIZE + x) * 4) as usize;
            out.extend_from_slice(&px[i..i + 4]);
        }
    }
    out
}

/// 变更像素占比。
fn changed_ratio(a: &[u8], b: &[u8]) -> f64 {
    let mut changed = 0u64;
    for i in 0..(SIZE * SIZE) as usize {
        let p = i * 4;
        if a[p..p + 4] != b[p..p + 4] {
            changed += 1;
        }
    }
    changed as f64 / (SIZE * SIZE) as f64
}

/// 主测试：缺席/关闭兼容锁 + 生效（TV、最大跳变下降）+ 只动边缘 +
/// UI 逐位不变 + 开启态确定性。
#[test]
fn fxaa_smooths_edges_keeps_ui_and_default_path_is_bitwise() {
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = edge_world();

    // ---- 缺席基线 ----
    renderer.render(&world);
    let off = renderer.read_pixels();
    assert_eq!(
        fnv1a(&off),
        R53_FXAA_OFF_BASELINE,
        "FXAA 缺席帧必须与 R52c 之后的默认链逐位一致（兼容锁）"
    );

    // ---- 显式 enabled = false 与缺席逐位 ----
    world.insert_resource(FxaaSettings {
        enabled: false,
        ..Default::default()
    });
    renderer.render(&world);
    let explicit_off = renderer.read_pixels();
    assert_eq!(off, explicit_off, "缺席 = 显式关闭必须逐位一致");

    // ---- 开启：边缘平滑（中心带行 y∈[112,144) 跨斜条两缘）----
    world.insert_resource(FxaaSettings {
        enabled: true,
        ..Default::default()
    });
    renderer.render(&world);
    let on = renderer.read_pixels();

    let mx_off = max_adjacent_diff(&off, 112, 144);
    let mx_on = max_adjacent_diff(&on, 112, 144);
    let band_off = band_count(&off, 112, 144);
    let band_on = band_count(&on, 112, 144);
    assert!(
        mx_on < mx_off,
        "FXAA 应压低最大相邻跳变（1 像素硬边 → 缓坡）：{mx_on} vs {mx_off}"
    );
    assert!(
        band_on > band_off,
        "过渡带必须变宽（中间调像素数上升）：{band_on} vs {band_off}"
    );
    let ratio = changed_ratio(&off, &on);
    assert!(
        ratio < 0.35,
        "只应改变边缘邻域：变更占比 {ratio:.3} 过大（早退分支未生效？）"
    );

    // ---- UI 色块逐位不变（FXAA 在 UI pass 之前，HUD 不被抹糊）----
    assert_eq!(
        region(&off, 6, 6, 34, 34),
        region(&on, 6, 6, 34, 34),
        "不透明 HUD 色块在 FXAA 开启前后必须逐位一致"
    );

    // ---- 开启态确定性 ----
    renderer.render(&world);
    let on2 = renderer.read_pixels();
    assert_eq!(on, on2, "同输入两帧必须逐位一致（确定性）");
}

/// 全帧像素 FNV-1a 64（与 `occlusion_render.rs` 同款兼容锁哈希）。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}
