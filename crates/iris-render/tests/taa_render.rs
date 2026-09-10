//! 集成测试：TAA 时域抗锯齿（R60，`shaders/taa.wgsl`）。
//!
//! 五条主线：
//! 1. **兼容锁（硬约束）**：`TaaSettings` 缺席帧的全帧 FNV-1a 逐位锁定
//!    在 R53 之后默认链（与 fxaa_render 同场景同哈希——交叉验证整条链
//!    未被 TAA 机制扰动）；显式 `enabled = false` 与缺席逐位一致；
//!    **重置帧输出与无 TAA 帧逐位一致**（重置语义 = 透传的硬证据）。
//! 2. **确定性（历史反馈下的硬要求）**：两个同输入完整会话
//!    （130 帧 = 100 帧亚像素平移 + 30 帧静止）逐帧逐位一致——TAA 使
//!    渲染有状态，抖动序列 / 混合算术 / 纹理反馈全部必须可复现；
//! 3. **有效性（R53「指标学」教训：断言必须可判别）**：亚像素缓动相机
//!    与 2px 细杆场景（指标在 8px 边距的内部 ROI 上度量——抖动使视锥
//!    边界几何随相位出画/入画，与边缘爬行是两类现象）。其一，运动段
//!    近满幅硬翻转（|Δ| > 100）归零、高跳变（|Δ| > 75）大于 4 倍下降；
//!    其二，静止收敛帧边缘过渡带（10%–90% 窗口）变宽（时域 SSAA 的
//!    定义性特征，实测 215 → 782）。
//! 4. **鬼影有界**：运动方块过屏——中段拖影像素（|Δ| > 25）< 4%；
//!    方块停止 80 帧后衰减到 < 1%（残余 = 收敛边缘软化）；
//! 5. **稳态 FNV 基线**：TAA 开启 + 静止收敛到 Halton 8 帧循环轨道后，
//!    固定帧号的全帧哈希锁入库。
//!
//! 环境无 GPU 适配器时优雅跳过（打印提示，测试仍算通过）。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// R53 默认链兼容锁（fxaa_render.rs 同款：同场景缺席帧必逐位一致）。
const R53_FXAA_OFF_BASELINE: u64 = 0x24e3_8254_0222_7bbb;

/// TAA 开启稳态静止帧基线（R60 首跑实测锁定 = 0x5d96_20c7_3382_cbf0；
/// 语义 = 细杆场景 100 帧亚像素平移 + 110 帧静止收敛后，Halton 8 帧
/// 循环轨道中该帧号的全帧 FNV——历史反馈完全收敛为周期轨道，哈希锁定
/// 其位模式；测试内另以「两个完整周期 8 帧逐位一致」守护轨道稳定性）。
const R60_TAA_STEADY_BASELINE: u64 = 0x5d96_20c7_3382_cbf0;

/// 屏幕尺寸（测试统一口径）。
const SIZE: u32 = 256;

/// 细杆场景：相机沿 −Z 正视，5 根 45° 近黑细杆（世界宽 0.03 ≈ 2px，
/// 高频几何）横贯画面，与程序化天空背景形成强对比斜边缘。泛光关。
fn bars_world() -> World {
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
    for i in 0..5i32 {
        world.spawn((
            Transform {
                position: Vec3::new(i as f32 * 0.6 - 1.2, 0.0, 0.0),
                rotation: glam::Quat::from_rotation_z(45f32.to_radians()),
                scale: Vec3::new(0.03, 4.0, 0.03),
            },
            cube,
            mat_bar,
        ));
    }

    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 0.0, 3.0);
        camera.target = Vec3::ZERO;
    }
    propagate_transforms(&mut world);
    world
}

/// 鬼影场景：红色方块（世界宽 0.5 ≈ 37px）居中前方，测试逐帧改写其
/// Transform 模拟物体自体运动（TAA v1 无逐物体运动向量——鬼影由钳制
/// 压制的被试对象）。
fn ghost_world() -> (World, iris_core::prelude::Entity) {
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
    let mat_cube = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.75, 0.06, 0.06, 1.0));
    let entity = world.spawn((
        Transform {
            position: Vec3::new(-1.3, 0.0, 0.0),
            ..Transform::IDENTITY
        },
        cube,
        mat_cube,
    ));
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 0.0, 3.0);
        camera.target = Vec3::ZERO;
    }
    propagate_transforms(&mut world);
    (world, entity)
}

/// 与 fxaa_render 同款单杆场景（+ HUD 色块）：兼容锁哈希的载体。
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

/// 相机亚像素平移（缓动脚本：世界单位，256px / 60° fov / 3m 距离下
/// 0.008 世界单位 ≈ 0.65 px/帧）。
fn pan_camera(world: &mut World, x: f32) {
    world.resource_mut::<Camera>().eye = Vec3::new(x, 0.0, 3.0);
}

/// 像素亮度（r+g+b 均值，0..255）。
fn lum(px: &[u8], x: u32, y: u32) -> f64 {
    let i = ((y * SIZE + x) * 4) as usize;
    (px[i] as f64 + px[i + 1] as f64 + px[i + 2] as f64) / 3.0
}

/// 画框边距（纹素）：抖动使视锥边界几何随相位出画/入画（±0.5 纹素的
/// 真实内容交替，ARCHITECTURE §8.36⑥），与「几何边缘爬行」是两类现象
/// ——crawling / 跳变指标在内部 ROI 上度量，排除边框环带。
const MARGIN: u32 = 8;

/// 帧间「crawling 像素」计数（内部 ROI）：|Δ亮度| > thresh 的像素数
/// （硬翻转边缘的判别指标之一——TAA 把 0/满幅跳变摊成亚阈值缓坡后
/// 计数骤降）。
fn crawl_count(a: &[u8], b: &[u8], thresh: f64) -> u32 {
    let mut n = 0u32;
    for y in MARGIN..SIZE - MARGIN {
        for x in MARGIN..SIZE - MARGIN {
            let d = (lum(a, x, y) - lum(b, x, y)).abs();
            if d > thresh {
                n += 1;
            }
        }
    }
    n
}

/// 帧间最大像素跳变（内部 ROI；fxaa「最大相邻跳变」的跨帧版，指标学
/// 教训的同款回应：总数/总变差判别不了平滑，极值判别得了）。
fn max_frame_jump(a: &[u8], b: &[u8]) -> f64 {
    let mut mx = 0.0f64;
    for y in MARGIN..SIZE - MARGIN {
        for x in MARGIN..SIZE - MARGIN {
            mx = mx.max((lum(a, x, y) - lum(b, x, y)).abs());
        }
    }
    mx
}

/// 过渡带宽度指标（fxaa_render 同款）：行区间 [y0,y1) 内亮度落在
/// [min + 10%·span, min + 90%·span] 开区间的像素数之和。
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

/// 全帧像素 FNV-1a 64（兼容锁哈希，与既有 GPU 测试同款）。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// 兼容锁：缺席帧 FNV = R53 默认链（同场景交叉验证）；显式关 = 缺席
/// 逐位；TAA 开启重置帧（首帧）= 无 TAA 帧逐位（重置 = 透传的硬证据，
/// 同时覆盖 jitter 关闭路径的矩阵逐位恒等）。
#[test]
fn taa_off_paths_and_reset_frame_are_bitwise_pristine() {
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = edge_world();

    // ---- 缺席：全帧 FNV 锁在 R53 之后默认链 ----
    renderer.render(&world);
    let absent = renderer.read_pixels();
    assert_eq!(
        fnv1a(&absent),
        R53_FXAA_OFF_BASELINE,
        "TAA 缺席帧必须与 R53 之后默认链逐位一致（兼容锁，同 fxaa 场景）"
    );

    // ---- 显式 enabled = false 与缺席逐位 ----
    world.insert_resource(TaaSettings { enabled: false });
    renderer.render(&world);
    let explicit_off = renderer.read_pixels();
    assert_eq!(absent, explicit_off, "缺席 = 显式关闭必须逐位一致");

    // ---- 开启首帧（重置帧）与无 TAA 帧逐位一致 ----
    world.insert_resource(TaaSettings { enabled: true });
    renderer.render(&world);
    let first_on = renderer.read_pixels();
    assert_eq!(
        absent, first_on,
        "TAA 开启首帧（重置透传）必须与无 TAA 帧逐位一致"
    );
    assert_eq!(
        renderer.renderer().stats().taa_reset,
        1,
        "首帧应处于重置透传（FrameStats 观测字段）"
    );
    // 第二帧起恢复正常累积（画面开始含历史贡献，允许变化）
    renderer.render(&world);
    assert_eq!(
        renderer.renderer().stats().taa_reset,
        0,
        "非重置帧的观测字段应归零"
    );
}

/// 显式 reset_taa()：切镜模拟（相机瞬移）后调用，下一帧输出与无 TAA
/// 会话同姿态帧逐位一致——历史被完整旁路。
#[test]
fn taa_explicit_reset_bypasses_history_bitwise() {
    let Some(mut taa_renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut plain_renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    // 两个独立世界：TAA 会话开 TAA，对照会话不插 TaaSettings（缺席 = 关）
    let mut world_taa = bars_world();
    let mut world_plain = bars_world();
    world_taa.insert_resource(TaaSettings { enabled: true });

    // 两会话同步跑 10 帧平移，建立各自历史状态
    for i in 0..10u32 {
        let x = i as f32 * 0.008;
        pan_camera(&mut world_taa, x);
        pan_camera(&mut world_plain, x);
        taa_renderer.render(&world_taa);
        plain_renderer.render(&world_plain);
    }
    // 切镜：相机瞬移 + TAA 会话显式重置
    pan_camera(&mut world_taa, 0.6);
    pan_camera(&mut world_plain, 0.6);
    taa_renderer.renderer_mut().reset_taa();
    taa_renderer.render(&world_taa);
    plain_renderer.render(&world_plain);
    let after_reset = taa_renderer.read_pixels();
    let plain = plain_renderer.read_pixels();
    assert_eq!(
        after_reset, plain,
        "reset_taa() 后的下一帧必须与无 TAA 同姿态帧逐位一致（历史旁路）"
    );
}

/// 确定性：两个同输入完整会话（100 帧亚像素平移 + 30 帧静止 = 130 帧）
/// 逐帧逐位一致；静止收敛 110 帧后稳态 FNV 锁库。
#[test]
fn taa_two_sessions_are_frame_by_frame_bitwise_and_steady_state_locks() {
    let Some(mut renderer_a) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut renderer_b) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world_a = bars_world();
    let mut world_b = bars_world();
    world_a.insert_resource(TaaSettings { enabled: true });
    world_b.insert_resource(TaaSettings { enabled: true });

    const MOTION: u32 = 100;
    const STILL: u32 = 30;
    let mut frames_a: Vec<Vec<u8>> = Vec::new();
    let mut frames_b: Vec<Vec<u8>> = Vec::new();
    for i in 0..(MOTION + STILL) {
        let x = i.min(MOTION - 1) as f32 * 0.008;
        pan_camera(&mut world_a, x);
        pan_camera(&mut world_b, x);
        renderer_a.render(&world_a);
        renderer_b.render(&world_b);
        frames_a.push(renderer_a.read_pixels());
        frames_b.push(renderer_b.read_pixels());
    }
    assert_eq!(
        frames_a.len() as u32,
        MOTION + STILL,
        "会话帧数 ≥ 120（含运动与静止段）"
    );
    for (i, (a, b)) in frames_a.iter().zip(&frames_b).enumerate() {
        assert_eq!(a, b, "第 {i} 帧两会话必须逐位一致（确定性硬要求）");
    }

    // 静止收敛：继续 80 帧静止（累计 110），稳态 FNV 锁库
    for _ in 0..80 {
        renderer_a.render(&world_a);
    }
    let steady = renderer_a.read_pixels();
    assert_eq!(
        fnv1a(&steady),
        R60_TAA_STEADY_BASELINE,
        "静止收敛稳态帧必须锁定 R60 基线（Halton 周期轨道的固定位模式）"
    );

    // 周期轨道稳定性：再跑两个完整 Halton 周期，对应相位帧的残差必须
    // 收敛到量化精度内（f16 历史混合的不动点附近，8-bit 输出量化残差
    // ≤ 2 LSB——R60 首跑实测残差 1 LSB；逐位不动点不成立：8-bit 舍入
    // 边界上的 f16 摆动，§8.36④ 的量化残差口径）
    let mut orbit1 = Vec::new();
    let mut orbit2 = Vec::new();
    for orbit in 0..2 {
        for _ in 0..8 {
            renderer_a.render(&world_a);
            let px = renderer_a.read_pixels();
            if orbit == 0 {
                orbit1.push(px);
            } else {
                orbit2.push(px);
            }
        }
    }
    let mut orbit_residual = 0.0f64;
    for i in 0..8 {
        orbit_residual = orbit_residual.max(max_frame_jump(&orbit1[i], &orbit2[i]));
    }
    eprintln!("[指标] 稳态轨道残差（两周期对应相位帧最大 |Δ|）= {orbit_residual:.1} LSB");
    assert!(
        orbit_residual <= 2.0,
        "稳态轨道残差必须收敛到量化精度内（≤ 2 LSB）：{orbit_residual}"
    );
}

/// 有效性（可判别双指标）：运动段 crawling 像素计数大幅下降 + 静止
/// 收敛帧边缘过渡带变宽。
#[test]
fn taa_reduces_crawling_and_widens_transition_band() {
    let Some(mut plain) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut taa) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world_plain = bars_world();
    let mut world_taa = bars_world();
    world_taa.insert_resource(TaaSettings { enabled: true });

    const MOTION: u32 = 24;
    const STILL: u32 = 96;
    let mut prev_plain: Option<Vec<u8>> = None;
    let mut prev_taa: Option<Vec<u8>> = None;
    let mut flip100_plain = 0u64;
    let mut flip100_taa = 0u64;
    let mut flip75_plain = 0u64;
    let mut flip75_taa = 0u64;
    let mut max_jump_plain = 0.0f64;
    let mut max_jump_taa = 0.0f64;
    let mut last_plain = Vec::new();
    let mut last_taa = Vec::new();
    for i in 0..(MOTION + STILL) {
        let x = i.min(MOTION - 1) as f32 * 0.008;
        pan_camera(&mut world_plain, x);
        pan_camera(&mut world_taa, x);
        plain.render(&world_plain);
        taa.render(&world_taa);
        let p = plain.read_pixels();
        let t = taa.read_pixels();
        if i < MOTION {
            if let (Some(pp), Some(pt)) = (&prev_plain, &prev_taa) {
                max_jump_plain = max_jump_plain.max(max_frame_jump(pp, &p));
                max_jump_taa = max_jump_taa.max(max_frame_jump(pt, &t));
                // R60 首跑实测分布（内部 ROI，23 帧累计）：
                //   plain  >25/50/75/100 = 2579/1136/1064/296，max 117.7
                //   TAA    >25/50/75/100 = 32270/3788/121/0，   max 91.7
                // 低阈值下 TAA 变化更多（32k vs 2.6k）= 缓坡整体平滑移动
                // 的签名；高阈值下硬翻转被钳制混合消除——判别选在高端。
                flip100_plain += crawl_count(pp, &p, 100.0) as u64;
                flip100_taa += crawl_count(pt, &t, 100.0) as u64;
                flip75_plain += crawl_count(pp, &p, 75.0) as u64;
                flip75_taa += crawl_count(pt, &t, 75.0) as u64;
            }
        }
        prev_plain = Some(p);
        prev_taa = Some(t);
        last_plain = prev_plain.clone().unwrap();
        last_taa = prev_taa.clone().unwrap();
    }
    eprintln!(
        "[指标] 运动段 crawling：近满幅硬翻转（|Δ|>100）关 = {flip100_plain} / 开 = {flip100_taa}；         高跳变（|Δ|>75）关 = {flip75_plain} / 开 = {flip75_taa}；         最大帧间跳变 关 = {max_jump_plain:.1} / 开 = {max_jump_taa:.1}"
    );
    assert!(
        flip100_plain >= 100,
        "亚像素运动下无 TAA 必须出现大量近满幅硬翻转（场景自检）：{flip100_plain}"
    );
    assert_eq!(
        flip100_taa, 0,
        "TAA 开启后近满幅硬翻转必须归零（内部 ROI 内 |Δ| 全部 ≤ 100）"
    );
    assert!(
        flip75_taa * 4 < flip75_plain,
        "TAA 开启后高跳变像素必须大幅下降（>4 倍）：开 = {flip75_taa} vs 关 = {flip75_plain}"
    );
    assert!(
        max_jump_taa < max_jump_plain,
        "最大帧间跳变必须下降：开 = {max_jump_taa} vs 关 = {max_jump_plain}"
    );

    // 静止收敛帧：边缘过渡带变宽（时域 SSAA 的定义性特征）
    let band_plain = band_count(&last_plain, 112, 144);
    let band_taa = band_count(&last_taa, 112, 144);
    eprintln!("[指标] 收敛帧过渡带（10%–90%）：TAA 关 = {band_plain}，TAA 开 = {band_taa}");
    assert!(
        band_taa > band_plain,
        "TAA 收敛帧边缘过渡带必须变宽：开 = {band_taa} vs 关 = {band_plain}"
    );
}

/// 鬼影有界：运动方块过屏中段拖影像素有上界；停止后 80 帧衰减归零。
#[test]
fn taa_ghosting_is_bounded_and_decays() {
    let Some(mut plain) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut taa) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let (mut world_plain, cube_plain) = ghost_world();
    let (mut world_taa, cube_taa) = ghost_world();
    world_taa.insert_resource(TaaSettings { enabled: true });

    const MOVE: u32 = 60;
    const STILL: u32 = 80;
    let mut ghost_mid = 0u64;
    let mut ghost_end = 0u64;
    let mut ghost_after = 0u64;
    for i in 0..(MOVE + STILL) {
        // 方块自体运动（无逐物体运动向量——TAA v1 的被试边界）
        let x = (i.min(MOVE - 1) as f32 * 0.0433) - 1.3;
        *world_plain.get_mut::<Transform>(cube_plain).unwrap() = Transform::from_xyz(x, 0.0, 0.0);
        *world_taa.get_mut::<Transform>(cube_taa).unwrap() = Transform::from_xyz(x, 0.0, 0.0);
        propagate_transforms(&mut world_plain);
        propagate_transforms(&mut world_taa);
        plain.render(&world_plain);
        taa.render(&world_taa);
        if i == MOVE / 2 {
            let p = plain.read_pixels();
            let t = taa.read_pixels();
            ghost_mid = crawl_count(&p, &t, 25.0) as u64;
        }
        if i == MOVE - 1 {
            let p = plain.read_pixels();
            let t = taa.read_pixels();
            ghost_end = crawl_count(&p, &t, 25.0) as u64;
        }
        if i == MOVE + STILL - 1 {
            let p = plain.read_pixels();
            let t = taa.read_pixels();
            ghost_after = crawl_count(&p, &t, 25.0) as u64;
        }
    }
    let total = (SIZE * SIZE) as u64;
    eprintln!(
        "[指标] 拖影像素（|Δ|>25）：运动中段 = {ghost_mid}，运动结束 = {ghost_end}，\
         停止 80 帧后 = {ghost_after}（全帧 {total} px）"
    );
    assert!(
        ghost_mid * 25 < total,
        "运动中段拖影必须有界（< 4% 像素，钳制的定义性边界）：实际 {ghost_mid}"
    );
    assert!(
        ghost_end < ghost_mid.max(1) * 3 && ghost_end * 50 < total,
        "运动结束时拖影仍须有界（< 2%）：实际 {ghost_end}"
    );
    assert!(
        ghost_after * 100 < total,
        "停止 80 帧后拖影必须衰减到残余水平（< 1%，残余 = 收敛边缘软化）：实际 {ghost_after}"
    );
}
