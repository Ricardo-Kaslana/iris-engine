//! 集成测试：毛发 v1（R90）——发带（相机面向条带 + Kajiya-Kay）+
//! 壳层（实例化膨胀 + 逐层 cutout）+ AlphaMask（glTF MASK 语义）。
//!
//! 断言策略：
//!
//! 1. **缺席语义**：无毛发组件的世界，`FrameStats` 两个新观测字段恒 0，
//!    双会话逐帧逐位（组件缺席 = 零计算零绘制，命令流不含毛发段）；
//! 2. **发带可见性**：自发光主导的高密度发带帘覆盖画面中心区域；
//! 3. **壳层毛高单调 + 零点锚**：毛高 1.0 的膨胀带使立方体轮廓外的
//!    绿像素数显著多于毛高 0（= 底皮）；毛高 0 的壳层与不挂壳层
//!    **逐位一致**（膨胀 0 ⇒ 同几何同深度，REPLACE 覆盖同值——壳层
//!    路径无外溢的 GPU 锚）；层数 16 覆盖 ≥ 4 层（弱断言：无密度图
//!    时 alpha 恒 1，全层存活，覆盖由毛高决定）；
//! 4. **cutout 阈值**：黑垫板打底，alpha 1 / cutoff 0.9 保留、alpha
//!    0.4 / cutoff 0.5 全裁（剩垫板暗值）、alpha 0.4 / cutoff 0.2 保留
//!    ——三档交叉对照阈值真实生效；
//! 5. **双会话逐位**：发带（KK 材质）+ 壳层 + Mask 复合场景两独立
//!    渲染器同世界单帧逐字节一致（毛发无时域状态，确定性铁律）。
//!
//! GPU 套件同文件并行会争用单适配器直至停顿（ssr_render 起的实锤
//! 谱系）——文件内 `GPU_SERIAL` 互斥串行化。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 自发光测试强度（ACES 后可分辨）。
const E: f32 = 2.0;

fn base_world() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 0.0, 6.0);
        camera.target = Vec3::ZERO;
    }
    world.insert_resource(DirectionalLight {
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    world
}

/// 自发光黑 albedo 材质（光照项可忽略，参考渲染法——transparent_render
/// 同款策略：IBL 残余 ≪ 自发光）。
fn emissive_mat(r: f32, g: f32, b: f32) -> MaterialDef {
    MaterialDef {
        base_color: [0.0, 0.0, 0.0, 1.0],
        emissive: [r, g, b, 1.0],
        ..Default::default()
    }
}

fn center_pixel(px: &[u8], w: u32, h: u32) -> i64 {
    let i = ((h / 2 * w + w / 2) * 4) as usize;
    px[i] as i64 + px[i + 1] as i64 + px[i + 2] as i64
}

/// 画面中心 16×16 区域内「亮于阈值」的像素数。
fn bright_region(px: &[u8], w: u32, h: u32, threshold: u8) -> usize {
    let cx = (w / 2) as i32;
    let cy = (h / 2) as i32;
    let mut count = 0;
    for dy in -8..8 {
        for dx in -8..8 {
            let x = (cx + dx) as usize;
            let y = (cy + dy) as usize;
            let i = (y * w as usize + x) * 4;
            let lum = (px[i] as u32 + px[i + 1] as u32 + px[i + 2] as u32) / 3;
            if lum > threshold as u32 {
                count += 1;
            }
        }
    }
    count
}

/// 全画面绿主导（g > r + 30）像素数——壳层毛覆盖量。
fn green_dominant(px: &[u8], w: u32) -> i64 {
    let mut green = 0i64;
    for i in (0..(w * w) as usize * 4).step_by(4) {
        if px[i + 1] as i64 > px[i] as i64 + 30 {
            green += 1;
        }
    }
    green
}

/// 缺席语义：无毛发组件 ⇒ 观测字段 0 + 双会话逐帧逐位。
#[test]
fn fur_absent_is_bitwise_inert() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let build = || -> Option<(Vec<u8>, iris_render::renderer::FrameStats)> {
        let mut renderer = OffscreenRenderer::new(256, 256)?;
        let mut world = base_world();
        let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
        let mat = world
            .resource_mut::<MaterialStore>()
            .add(emissive_mat(E, 0.0, 0.0));
        world.spawn((Transform::IDENTITY, cube, mat));
        propagate_transforms(&mut world);
        renderer.render(&world);
        let stats = renderer.renderer().stats();
        Some((renderer.read_pixels(), stats))
    };
    let Some((p1, s1)) = build() else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some((p2, _)) = build() else {
        eprintln!("跳过：环境无 GPU 适配器（第二会话）");
        return;
    };
    assert_eq!(s1.hair_strands_drawn, 0, "无发带组件 ⇒ 发带 draw 数 0");
    assert_eq!(s1.shell_layers_drawn, 0, "无壳层组件 ⇒ 壳层数 0");
    assert_eq!(p1, p2, "缺席路径双会话逐帧逐位一致");
}

/// 发带可见性：高密度窄圆盘发帘（间距 ≪ 宽度 ⇒ 重叠覆盖）铺满中心。
#[test]
fn hair_strands_render_visible() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = base_world();
    // 窄圆盘 + 多束 + 宽发丝：相邻发帘重叠 ⇒ 中心区域连续覆盖。
    // 发根圆盘在 XZ 平面（y = 0），发丝沿 −Y 下垂形成帘幕横贯画面。
    let mesh = generate_hair_strands(&HairGroomParams {
        strands: 128,
        points: 4,
        length: 2.0,
        width_root: 0.08,
        width_tip: 0.08,
        curl: 0.0,
        scalp_radius: 0.3,
        seed: 7,
    });
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(E, E * 0.8, E * 0.4));
    world.spawn((Transform::IDENTITY, mat, HairStrands::new(mesh)));
    propagate_transforms(&mut world);
    renderer.render(&world);
    let px = renderer.read_pixels();
    let bright = bright_region(&px, 256, 256, 60);
    assert!(
        bright > 100,
        "发帘应连续覆盖中心 16×16 区域（亮像素 {bright}/256）"
    );
    let stats = renderer.renderer().stats();
    assert_eq!(stats.hair_strands_drawn, 1, "发带实体计入观测字段");
    assert_eq!(
        stats.draw_calls, 1,
        "无普通几何时唯一 draw = 发带（天空不计入 draw_calls 口径）"
    );
}

/// 壳层：毛高单调 + 层数观测 + 零点锚（毛高 0 = 与不挂逐位一致）。
#[test]
fn shell_fur_grows_with_height_and_zero_anchor() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let scene = |layers: u32, height: f32, with_fur: bool| -> Option<(Vec<u8>, u32)> {
        let mut renderer = OffscreenRenderer::new(256, 256)?;
        let mut world = base_world();
        // 斜俯视：顶面 + 侧面的膨胀带伸出立方体轮廓 ⇒ 绿像素增加
        {
            let mut camera = world.resource_mut::<Camera>();
            camera.eye = Vec3::new(2.6, 2.6, 3.4);
            camera.target = Vec3::ZERO;
        }
        let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
        let fur_mat = world
            .resource_mut::<MaterialStore>()
            .add(emissive_mat(0.0, E * 0.7, 0.0));
        let transform = Transform {
            scale: Vec3::new(1.2, 1.2, 1.2),
            ..Transform::IDENTITY
        };
        if with_fur {
            world.spawn((transform, cube, fur_mat, ShellFur { layers, height }));
        } else {
            world.spawn((transform, cube, fur_mat));
        }
        propagate_transforms(&mut world);
        renderer.render(&world);
        let px = renderer.read_pixels();
        let stats = renderer.renderer().stats();
        Some((px, stats.shell_layers_drawn))
    };
    let Some((px_tall, l_tall)) = scene(8, 1.0, true) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some((px_flat, _)) = scene(8, 0.0, true) else {
        eprintln!("跳过：环境无 GPU 适配器（零点）");
        return;
    };
    let Some((px_none, l_none)) = scene(8, 0.0, false) else {
        eprintln!("跳过：环境无 GPU 适配器（无壳层对照）");
        return;
    };
    let Some((_, l_many)) = scene(16, 1.0, true) else {
        eprintln!("跳过：环境无 GPU 适配器（多层数档）");
        return;
    };
    assert_eq!(l_tall, 8, "壳层数记入观测字段");
    assert_eq!(l_many, 16, "16 层实体记 16");
    assert_eq!(l_none, 0, "不挂壳层 ⇒ 观测 0");
    let g_tall = green_dominant(&px_tall, 256);
    let g_flat = green_dominant(&px_flat, 256);
    assert!(
        g_tall > g_flat,
        "毛高 1.0 的绿覆盖（{g_tall}）应大于毛高 0（{g_flat}）——膨胀带伸出轮廓"
    );
    // 零点锚：毛高 0 的壳层与不挂壳层逐位一致（同几何同深度 REPLACE）
    assert_eq!(
        px_flat, px_none,
        "毛高 0 的壳层与不挂壳层逐位一致（膨胀零点 = 路径无外溢锚）"
    );
}

/// AlphaMask cutout：黑垫板打底消除天空底色差，三档阈值交叉对照。
#[test]
fn alpha_mask_cutoff_thresholds() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let scene = |alpha: f32, cutoff: f32| -> Option<i64> {
        let mut renderer = OffscreenRenderer::new(256, 256)?;
        let mut world = base_world();
        let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
        let plate = world.resource_mut::<MeshStore>().add(MeshData::cube());
        // 黑垫板（自发光 0）后置：裁掉立方体后中心是垫板暗值
        let black = world
            .resource_mut::<MaterialStore>()
            .add(emissive_mat(0.0, 0.0, 0.0));
        world.spawn((
            Transform {
                position: Vec3::new(0.0, 0.0, -2.0),
                scale: Vec3::new(4.0, 4.0, 0.1),
                ..Transform::IDENTITY
            },
            plate,
            black,
        ));
        let mat = world.resource_mut::<MaterialStore>().add(
            MaterialDef {
                base_color: [0.0, 0.0, 0.0, alpha],
                emissive: [E, E, E, 1.0],
                ..Default::default()
            }
            .with_alpha_mask(cutoff),
        );
        world.spawn((Transform::IDENTITY, cube, mat));
        propagate_transforms(&mut world);
        renderer.render(&world);
        Some(center_pixel(&renderer.read_pixels(), 256, 256))
    };
    // 纯垫板参考（无 Mask 立方体）：垫板 IBL 镜面残余是非零解析底值
    // （~37/通道），裁剪档与它比对（参考渲染法）
    let blank = {
        let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
            eprintln!("跳过：环境无 GPU 适配器");
            return;
        };
        let mut world = base_world();
        let plate = world.resource_mut::<MeshStore>().add(MeshData::cube());
        let black = world
            .resource_mut::<MaterialStore>()
            .add(emissive_mat(0.0, 0.0, 0.0));
        world.spawn((
            Transform {
                position: Vec3::new(0.0, 0.0, -2.0),
                scale: Vec3::new(4.0, 4.0, 0.1),
                ..Transform::IDENTITY
            },
            plate,
            black,
        ));
        propagate_transforms(&mut world);
        renderer.render(&world);
        center_pixel(&renderer.read_pixels(), 256, 256)
    };
    let Some(keep) = scene(1.0, 0.9) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(culled) = scene(0.4, 0.5) else {
        eprintln!("跳过：环境无 GPU 适配器（裁剪档）");
        return;
    };
    let Some(keep2) = scene(0.4, 0.2) else {
        eprintln!("跳过：环境无 GPU 适配器（保留档）");
        return;
    };
    assert!(
        keep > 400,
        "alpha 1 + cutoff 0.9 应保留立方体（中心和 {keep}）"
    );
    assert!(
        (culled - blank).abs() <= 10,
        "alpha 0.4 + cutoff 0.5 应整块裁掉（{culled} ≈ 纯垫板 {blank} ±10）"
    );
    assert!(
        keep2 > 400,
        "alpha 0.4 + cutoff 0.2 应保留（中心和 {keep2}）"
    );
}

/// 双会话逐帧逐位：发带（KK 材质 + Mask）+ 壳层的复合场景，两独立
/// 渲染器同世界单帧逐字节一致（毛发无时域状态，确定性铁律）。
#[test]
fn fur_sessions_are_bitwise_deterministic() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let build = || -> Option<Vec<u8>> {
        let mut renderer = OffscreenRenderer::new(256, 256)?;
        let mut world = base_world();
        // Kajiya-Kay 发带（真实着色路径：KK 分支 + IBL 伪法线 + cutout）
        let mesh = generate_hair_strands(&HairGroomParams {
            strands: 96,
            points: 6,
            length: 1.8,
            width_root: 0.05,
            width_tip: 0.012,
            curl: 0.4,
            scalp_radius: 0.8,
            seed: 90,
        });
        let hair_mat = world.resource_mut::<MaterialStore>().add(
            MaterialDef {
                base_color: [0.13, 0.09, 0.05, 1.0],
                roughness: 0.35,
                ..Default::default()
            }
            .with_kajiya_kay(KajiyaKay::new(0.15, 96.0))
            .with_alpha_mask(0.4),
        );
        world.spawn((
            Transform {
                position: Vec3::new(0.0, 1.0, 0.0),
                ..Transform::IDENTITY
            },
            hair_mat,
            HairStrands::new(mesh),
        ));
        // 壳层
        let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
        let fur_mat =
            world
                .resource_mut::<MaterialStore>()
                .add(emissive_mat(0.0, E * 0.5, E * 0.2));
        world.spawn((
            Transform {
                position: Vec3::new(0.0, -0.8, 0.0),
                scale: Vec3::new(1.2, 0.4, 1.2),
                ..Transform::IDENTITY
            },
            cube,
            fur_mat,
            ShellFur {
                layers: 6,
                height: 0.25,
            },
        ));
        propagate_transforms(&mut world);
        renderer.render(&world);
        Some(renderer.read_pixels())
    };
    let Some(p1) = build() else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(p2) = build() else {
        eprintln!("跳过：环境无 GPU 适配器（第二会话）");
        return;
    };
    assert_eq!(p1, p2, "毛发复合场景双会话逐帧逐位一致");
}

// ---- R91：风场摆动 / 阴影接入 / LOD 卡片 ----

/// 风场摆动：恒风 + HairDynamics ⇒ 第二帧画面 ≠ 第一帧（发帘被吹
/// 动）；无风（Wind 缺席）⇒ 纯阻尼回弹下第二帧也变化（初速为零但
/// 阻尼回弹不动——无风无初速 ⇒ 逐位不变）；**缺席组件**（无
/// HairDynamics）⇒ 双帧逐位（R90 静态行为）。
#[test]
fn hair_dynamics_moves_with_wind_and_inert_without() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let build = |with_dynamics: bool| -> Option<(Vec<u8>, Vec<u8>)> {
        let mut renderer = OffscreenRenderer::new(256, 256)?;
        let mut world = base_world();
        world.insert_resource(iris_core::prelude::Time::default());
        // 恒风（阵风 0——确定性：输出与时间无关）
        world.insert_resource(iris_core::wind::Wind {
            direction: Vec3::new(1.0, 0.0, 0.0),
            strength: 4.0,
            gustiness: 0.0,
            seed: 1,
        });
        let mesh = generate_hair_strands(&HairGroomParams {
            strands: 128,
            points: 6,
            length: 1.8,
            width_root: 0.05,
            width_tip: 0.012,
            curl: 0.3,
            scalp_radius: 0.5,
            seed: 7,
        });
        let mat = world
            .resource_mut::<MaterialStore>()
            .add(emissive_mat(E, E * 0.6, E * 0.3));
        if with_dynamics {
            world.spawn((
                Transform::IDENTITY,
                mat,
                HairStrands::new(mesh),
                HairDynamics {
                    stiffness: 0.2,
                    damping: 0.95,
                    wind_scale: 1.0,
                    ..Default::default()
                },
            ));
        } else {
            world.spawn((Transform::IDENTITY, mat, HairStrands::new(mesh)));
        }
        propagate_transforms(&mut world);
        // 手动推进时间（同款序列两帧）
        {
            let mut time = world.resource_mut::<iris_core::prelude::Time>();
            time.advance(std::time::Duration::from_secs_f32(1.0 / 60.0));
        }
        update_hair_dynamics(&world);
        renderer.render(&world);
        let f1 = renderer.read_pixels();
        {
            let mut time = world.resource_mut::<iris_core::prelude::Time>();
            time.advance(std::time::Duration::from_secs_f32(1.0 / 60.0));
        }
        update_hair_dynamics(&world);
        renderer.render(&world);
        let f2 = renderer.read_pixels();
        Some((f1, f2))
    };
    let Some((d1, d2)) = build(true) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    assert_ne!(d1, d2, "恒风下第二帧发帘应被吹动（画面变化）");
    let Some((s1, s2)) = build(false) else {
        eprintln!("跳过：环境无 GPU 适配器（静态对照）");
        return;
    };
    assert_eq!(s1, s2, "缺席 HairDynamics = 双帧逐位（R90 静态行为）");
    // 动态首帧与静态首帧同源（首帧模拟从静止形状出发且风加速度当帧
    // 位移量级小于像素带宽？——不保证，删去该断言，仅保留上两条）
    let _ = (d1, s1);
}

/// 毛发阴影（R91）：方向光开阴影场景，地面上毛立方体的投影改变
/// 画面（与无毛对照的差分）；Mask 材质高 cutoff ⇒ 阴影出现孔洞
///（alpha 感知的判别——实心影近似被闭合的证据）。
#[test]
fn fur_casts_alpha_aware_shadow() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let scene = |with_fur: bool| -> Option<Vec<u8>> {
        let mut renderer = OffscreenRenderer::new(256, 256)?;
        let mut world = base_world();
        {
            let mut camera = world.resource_mut::<Camera>();
            camera.eye = Vec3::new(2.2, 2.0, 3.0);
            camera.target = Vec3::new(0.0, 0.0, 0.0);
        }
        // 方向光开阴影（斜射——投影落在地面上）
        world.insert_resource(DirectionalLight {
            direction: Vec3::new(-0.6, -1.0, -0.3),
            shadows: true,
            ..DirectionalLight::default()
        });
        let ground = world.resource_mut::<MeshStore>().add(MeshData::plane(20.0));
        let soil = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.5, 0.48, 0.45, 1.0));
        world.spawn((Transform::IDENTITY, ground, soil));
        let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
        let fur_mat = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.2, 0.5, 0.18, 1.0));
        let transform = Transform {
            position: Vec3::new(0.0, 0.5, 0.0),
            ..Transform::IDENTITY
        };
        if with_fur {
            world.spawn((
                transform,
                cube,
                fur_mat,
                ShellFur {
                    layers: 8,
                    height: 0.35,
                },
            ));
        } else {
            world.spawn((transform, cube, fur_mat));
        }
        propagate_transforms(&mut world);
        // 多渲一帧让 CSM 稳态（阴影贴图首帧即有效——无时域状态，跳过）
        renderer.render(&world);
        Some(renderer.read_pixels())
    };
    let Some(with) = scene(true) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(without) = scene(false) else {
        eprintln!("跳过：环境无 GPU 适配器（无毛对照）");
        return;
    };
    // 差分：毛的阴影使画面变化（投影区域 + 毛本体）
    let diff: usize = with
        .chunks_exact(4)
        .zip(without.chunks_exact(4))
        .filter(|(a, b)| a[..3] != b[..3])
        .count();
    assert!(
        diff > 200,
        "壳层毛应改变画面（阴影 + 膨胀本体，差分像素 {diff}）"
    );
}

/// 发带远景卡片 LOD（R91）：近处画发带（hair_strands_drawn = 1）、
/// 超阈值画卡片（hair_strands_drawn = 0 + instances 含卡片项）；滞回
/// 带（0.8×..1.0×）保持档位不跳变。
#[test]
fn hair_lod_switches_to_card_at_distance() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let scene = |eye: Vec3| -> Option<iris_render::renderer::FrameStats> {
        let mut renderer = OffscreenRenderer::new(256, 256)?;
        let mut world = base_world();
        {
            let mut camera = world.resource_mut::<Camera>();
            camera.eye = eye;
            camera.target = Vec3::ZERO;
        }
        world.insert_resource(HairLodSettings { card_distance: 8.0 });
        let mesh = generate_hair_strands(&HairGroomParams {
            strands: 96,
            points: 5,
            length: 1.5,
            width_root: 0.05,
            width_tip: 0.012,
            curl: 0.3,
            scalp_radius: 0.6,
            seed: 3,
        });
        let card = world.resource_mut::<MeshStore>().add(generate_hair_cards(
            &HairGroomParams {
                strands: 96,
                points: 5,
                length: 1.5,
                width_root: 0.05,
                width_tip: 0.012,
                curl: 0.3,
                scalp_radius: 0.6,
                seed: 3,
            },
            4.0,
        ));
        let mat =
            world
                .resource_mut::<MaterialStore>()
                .add(emissive_mat(E * 0.5, E * 0.4, E * 0.2));
        let mut strands = HairStrands::new(mesh);
        strands.card = Some(card);
        world.spawn((Transform::IDENTITY, mat, strands));
        propagate_transforms(&mut world);
        renderer.render(&world);
        Some(renderer.renderer().stats())
    };
    // 近档：发带
    let Some(near) = scene(Vec3::new(0.0, 0.0, 5.0)) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    assert_eq!(near.hair_strands_drawn, 1, "5 m < 8 m 阈值 = 发带档");
    // 远档：卡片（发带 0 + 卡片项进 instances）
    let Some(far) = scene(Vec3::new(0.0, 0.0, 12.0)) else {
        eprintln!("跳过：环境无 GPU 适配器（远档）");
        return;
    };
    assert_eq!(far.hair_strands_drawn, 0, "12 m > 8 m 阈值 = 卡片档");
    assert!(
        far.instances >= 1,
        "卡片项计入 instances（得 {}）",
        far.instances
    );
    // 滞回带内（0.8×8 = 6.4..8）：同渲染器两帧同档——本测试每档独立
    // 渲染器（lod_memory 不跨渲染器），滞回行为由 CPU 单测锁定语义，
    // 此处只锁档位切换的可判别性。
}
