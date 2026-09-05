//! 集成测试：后处理链 v1——曝光控制与暗角（pass 串联框架的示范 pass）。
//!
//! 曝光断言依赖 ACES（Narkowicz）在正半轴严格单调：线性 HDR × 曝光后
//! 色调映射，曝光 0.5 / 1.0 / 2.0 的同场景中心亮度必严格递增（发光体
//! 亮度取值保证三档都落在 ACES 非饱和段）。暗角断言依赖其链位与解析
//! 表达式：全分辨率 HDR → HDR 压暗四角（中心 falloff 恒 0，逐位无损），
//! 强度 0 / 资源缺席时整段跳过，与无 pass 帧逐位一致（零开销默认路径）。
//!
//! R43 追加：景深（DOF）与运动模糊（MB）两组——
//! 1. **基线锁定（硬约束）**：两资源缺席或 enabled = false 时整帧像素
//!    与 R41/R42 基线 FNV-1a 哈希逐位一致（与 `normal_render.rs` 同一
//!    场景同一哈希值，交叉验证）；
//! 2. **DOF 生效**：聚焦带逐位透传、远景区局部方差显著下降；
//! 3. **MB 生效**：启用 + 相机静止 = 逐位透传（速度 0）、平移后拖影
//!    使边缘梯度能量下降；
//! 4. **确定性**：同输入两次渲染逐位一致。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 曝光测试场景：中央高自发光球（HDR ≈ 2，0.5×/1×/2× 后仍落在 ACES
/// 非饱和段，单调可辨）+ 暗地面。泛光显式关闭以隔离曝光效果。
fn exposure_scene(exposure: Option<f32>) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight {
        intensity: 0.3,
        ambient: Vec3::splat(0.02),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    if let Some(exposure) = exposure {
        world.insert_resource(ExposureSettings { exposure });
    }

    // 高自发光球：画面中心的 HDR 亮源
    let sphere = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, 32, 16));
    let mat_glow = world.resource_mut::<MaterialStore>().add(MaterialDef {
        base_color: [0.02, 0.02, 0.02, 1.0],
        emissive: [2.0, 1.4, 0.8, 1.0],
        ..MaterialDef::solid(0.0, 0.0, 0.0, 1.0)
    });
    world.spawn((Transform::from_xyz(0.0, 1.2, 0.0), sphere, mat_glow));

    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let mat_ground = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.22, 0.22, 0.25, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat_ground));

    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 1.6, 6.0);
        camera.target = Vec3::new(0.0, 1.2, 0.0);
    }

    propagate_transforms(&mut world);
    world
}

/// 区域均值（r+g+b 之和按像素平均），区域为画面中央 ±1/16。
fn center_mean(px: &[u8], w: u32, h: u32) -> f64 {
    region_mean(px, w, w * 7 / 16, w * 9 / 16, h * 7 / 16, h * 9 / 16)
}

/// 区域均值（r+g+b 之和按像素平均），像素坐标区间 [x0,x1) × [y0,y1)。
fn region_mean(px: &[u8], w: u32, x0: u32, x1: u32, y0: u32, y1: u32) -> f64 {
    let mut sum = 0u64;
    let mut count = 0u64;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * w + x) * 4) as usize;
            sum += (px[i] as u64) + (px[i + 1] as u64) + (px[i + 2] as u64);
            count += 1;
        }
    }
    sum as f64 / count as f64
}

/// 曝光单调（0.5 < 1.0 < 2.0）：ACES 在正半轴严格单调，线性 HDR 乘以
/// 更大的曝光必给出不更暗、且在非饱和段严格更亮的色调映射结果。
#[test]
fn exposure_scales_center_brightness_monotonically() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let (w, h) = renderer.size();
    let mut means = [0.0f64; 3];
    for (i, exposure) in [0.5f32, 1.0, 2.0].into_iter().enumerate() {
        renderer.render(&exposure_scene(Some(exposure)));
        means[i] = center_mean(&renderer.read_pixels(), w, h);
    }
    assert!(
        means[0] + 10.0 < means[1],
        "曝光 0.5 的中心亮度应显著低于 1.0（ACES 单调），实际 {:.1} vs {:.1}",
        means[0],
        means[1]
    );
    assert!(
        means[1] + 10.0 < means[2],
        "曝光 1.0 的中心亮度应显著低于 2.0（ACES 单调），实际 {:.1} vs {:.1}",
        means[1],
        means[2]
    );
}

/// 曝光资源缺席 = 显式 1.0：两者走完全相同的命令流（y = 1.0 的乘法逐位
/// 中性），整帧必须逐位一致——这是「默认设置零变化」的引擎级证明。
#[test]
fn exposure_absent_equals_explicit_one_bitwise() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    renderer.render(&exposure_scene(None));
    let absent = renderer.read_pixels();
    renderer.render(&exposure_scene(Some(1.0)));
    let explicit = renderer.read_pixels();
    assert_eq!(
        absent, explicit,
        "曝光资源缺席与显式 1.0 应逐位一致（缺席 = 中性语义）"
    );
}

/// 曝光归零 → 全黑：hdr × 0 = 0，ACES(0) = 0，sRGB(0) = 0，包括泛光
/// 叠加在内的全部链上内容都被清零（曝光是链的终点前的乘法）。
#[test]
fn exposure_zero_renders_black() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    renderer.render(&exposure_scene(Some(0.0)));
    let px = renderer.read_pixels();
    assert!(!px.is_empty(), "应读回整帧像素");
    for (i, chunk) in px.chunks_exact(4).enumerate() {
        assert!(
            chunk[0] == 0 && chunk[1] == 0 && chunk[2] == 0,
            "曝光 0.0 时每个像素都应为全黑（ACES(0)=0），像素 {i} 实际 {:?}",
            &chunk[..3]
        );
    }
}

/// 暗角测试场景：俯视一整块自发光平面，画面每一点都是同值 HDR 辐射
/// （无光照、无泛光）——中心与角点的差异只来自暗角衰减，不受场景
/// 亮度分布干扰。
fn vignette_scene(strength: Option<f32>) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight {
        intensity: 0.0,
        ambient: Vec3::splat(0.0),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    if let Some(strength) = strength {
        world.insert_resource(VignetteSettings { strength });
    }

    // 全屏自发光地面：40×40 平面在 45° 视场下完全覆盖画面
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let mat_glow = world.resource_mut::<MaterialStore>().add(MaterialDef {
        emissive: [1.6, 1.5, 1.4, 1.0],
        ..MaterialDef::solid(0.0, 0.0, 0.0, 1.0)
    });
    world.spawn((Transform::IDENTITY, plane, mat_glow));

    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 10.0, 2.0);
        camera.target = Vec3::ZERO;
    }

    propagate_transforms(&mut world);
    world
}

/// sRGB 8bit → 线性值（标准 EOTF 逆）。暗角压暗发生在**线性 HDR 域**，
/// 而 8bit 读回是 sRGB 编码——编码曲线会把暗角幅度压缩近半（线性 ×0.3
/// 在 sRGB 里只表现为 ×0.7 左右），断言必须先解码回线性域才能对得上
/// vignette 强度的物理含义。
fn srgb_to_linear(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// 区域线性均值（r+g+b 通道各自解码 sRGB 后平均），像素坐标区间
/// [x0,x1) × [y0,y1)。
fn region_linear_mean(px: &[u8], w: u32, x0: u32, x1: u32, y0: u32, y1: u32) -> f64 {
    let mut sum = 0f64;
    let mut count = 0u64;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * w + x) * 4) as usize;
            sum += srgb_to_linear(px[i]) + srgb_to_linear(px[i + 1]) + srgb_to_linear(px[i + 2]);
            count += 1;
        }
    }
    sum / count as f64
}

/// 暗角 0.8：角区**线性**均值显著低于中心区（角点 falloff ≥ 0.81 →
/// 线性压到 ≤ 0.35 倍，ACES 滚降后约 0.7 倍），且中心区与无暗角帧几乎
/// 一致（中心 falloff 恒 0，压暗不触碰画面核心）。
#[test]
fn vignette_darkens_corners_more_than_center() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let (w, h) = renderer.size();
    renderer.render(&vignette_scene(None));
    let without = renderer.read_pixels();
    renderer.render(&vignette_scene(Some(0.8)));
    let with = renderer.read_pixels();

    // 中心无损（falloff 恒 0）：8bit 读回均值不因暗角 pass 变化
    let center_with = center_mean(&with, w, h);
    let center_without = center_mean(&without, w, h);
    assert!(
        (center_with - center_without).abs() <= 2.0,
        "暗角中心应无损（falloff 恒 0）：无暗角 {:.1} vs 有暗角 {:.1}",
        center_without,
        center_with
    );

    // 四角 20×20 区块：UV 距中心 ≥ 0.599，falloff ≥ 0.68 → 线性辐射
    // 至少压暗 45%；断言在线性域进行（见 srgb_to_linear 的理由）
    let edge = 20;
    let corners = [(0, 0), (w - edge, 0), (0, h - edge), (w - edge, h - edge)];
    for (x0, y0) in corners {
        let corner_with = region_linear_mean(&with, w, x0, x0 + edge, y0, y0 + edge);
        let corner_without = region_linear_mean(&without, w, x0, x0 + edge, y0, y0 + edge);
        assert!(
            corner_with < corner_without * 0.8,
            "角区 ({x0},{y0}) 应被显著压暗（线性域）：无暗角 {:.4} vs 有暗角 {:.4}",
            corner_without,
            corner_with
        );
        let center_linear =
            region_linear_mean(&with, w, w * 7 / 16, w * 9 / 16, h * 7 / 16, h * 9 / 16);
        assert!(
            corner_with < center_linear * 0.8,
            "角区 ({x0},{y0}) 应显著暗于同帧中心区（线性域）：{:.4} vs {:.4}",
            corner_with,
            center_linear
        );
    }
}

/// 暗角强度 0 与资源缺席逐位一致：两者都整段跳过暗角 pass、blit 直读
/// 主解析缓冲（默认绑定组与引入暗角前逐项一致）——零开销默认路径的
/// 等价性证明。
#[test]
fn vignette_zero_and_absent_equal_nopass_bitwise() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    renderer.render(&vignette_scene(None));
    let absent = renderer.read_pixels();
    renderer.render(&vignette_scene(Some(0.0)));
    let zero = renderer.read_pixels();
    assert_eq!(
        absent, zero,
        "暗角资源缺席与强度 0 应逐位一致（pass 完全跳过）"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// R43：景深（DOF）与运动模糊（MB）——基线锁定与效果断言
// ═══════════════════════════════════════════════════════════════════════

/// R41/R42 基线全帧像素 FNV-1a 64（与 `normal_render.rs` 同一常量、同一
/// 场景：地面 + 砖色立方体 + 金属圆柱 + 默认 IBL / 阴影 / 侧光，320×180，
/// 泛光关闭）。DOF / MB 资源缺席或 enabled = false 时本帧哈希必须逐位
/// 等于该值——两个新 pass「零开销默认路径」的硬约束。
const R42_BASELINE_HASH: u64 = 0x609b_2457_f4d5_cd82;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 与 normal_render.rs 的 R41 基线捕获完全一致的固定场景（DOF / MB /
/// 暗角资源缺席、泛光关闭）。
fn baseline_scene() -> World {
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

/// 硬约束锁定（FNV）：DOF / MB 资源缺席时整帧像素哈希与 R41/R42 基线
/// 逐位一致。哈希改变的任何一位都意味着默认路径被破坏。
#[test]
fn dof_mb_absent_match_r42_baseline_bitwise() {
    let Some(mut renderer) = OffscreenRenderer::new(320, 180) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    renderer.render(&baseline_scene());
    let pixels = renderer.read_pixels();
    assert_eq!(pixels.len(), 320 * 180 * 4, "全帧像素尺寸必须一致");
    let hash = fnv1a(&pixels);
    assert_eq!(
        hash, R42_BASELINE_HASH,
        "DOF/MB 缺席帧的哈希 {hash:#018x} ≠ 基线 {R42_BASELINE_HASH:#018x}\
         ——默认渲染路径被破坏（应与 normal_render.rs 交叉一致）"
    );
}

/// 硬约束锁定（逐字节）：DOF / MB 显式 enabled = false 与资源缺席逐位
/// 一致（两者都整段跳过新 pass、blit 直读主解析缓冲）。
#[test]
fn dof_mb_disabled_equals_absent_bitwise() {
    let Some(mut renderer) = OffscreenRenderer::new(320, 180) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    renderer.render(&baseline_scene());
    let absent = renderer.read_pixels();
    let mut enabled_off = baseline_scene();
    enabled_off.insert_resource(DepthOfFieldSettings {
        enabled: false,
        ..Default::default()
    });
    enabled_off.insert_resource(MotionBlurSettings {
        enabled: false,
        ..Default::default()
    });
    renderer.render(&enabled_off);
    let disabled = renderer.read_pixels();
    assert_eq!(
        absent, disabled,
        "DOF/MB 显式关闭应与资源缺席逐位一致（pass 完全跳过）"
    );
}

/// 景深测试场景：近处聚焦目标（立方体，视距 ≈ 6）+ 远处一排异色立方体
/// （视距 ≈ 22）+ 灰色地面。DOF 断言的方差来源 = 远处异色立方体与背景
/// 的亮度差异，模糊即把成片异色混在一起。
fn dof_scene(dof: Option<DepthOfFieldSettings>) -> World {
    use glam::Vec3;
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(0.0, 2.0, 6.0),
        // 直接注视近处立方体（视距恰为 6.0，与 focus_distance 一致，
        // 保证中心区域完全落在清晰带内）
        target: Vec3::new(0.0, 1.0, 0.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        intensity: 0.8,
        ambient: Vec3::splat(0.08),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    if let Some(dof) = dof {
        world.insert_resource(dof);
    }
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(80.0));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mut mats = world.resource_mut::<MaterialStore>();
    let ground = mats.add(MaterialDef::solid(0.45, 0.45, 0.48, 1.0));
    let near = mats.add(MaterialDef::solid(0.9, 0.3, 0.2, 1.0));
    let far_a = mats.add(MaterialDef::solid(0.95, 0.9, 0.15, 1.0));
    let far_b = mats.add(MaterialDef::solid(0.15, 0.85, 0.3, 1.0));
    let far_c = mats.add(MaterialDef::solid(0.2, 0.35, 0.95, 1.0));
    drop(mats);
    world.spawn((Transform::IDENTITY, plane, ground));
    // 近处聚焦目标（相机注视点方向，视距 ≈ 6）
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 1.0, 0.0),
            ..Transform::IDENTITY
        },
        cube,
        near,
    ));
    // 远处异色立方体（视距 ≈ 22，CoC 打满 → 强模糊）：密集交替的小立方
    // 群（间距 ≈ 13 px < 2×模糊半径）提供小尺度颜色交替——方差由这部分
    // 高频主导，12 纹素泊松盘可将其基本抹平
    for i in 0..10 {
        let mat = match i % 3 {
            0 => far_a,
            1 => far_b,
            _ => far_c,
        };
        world.spawn((
            Transform {
                position: Vec3::new(-4.5 + i as f32 * 1.0, 1.5, -16.0),
                scale: Vec3::splat(0.6),
                ..Transform::IDENTITY
            },
            cube,
            mat,
        ));
    }
    propagate_transforms(&mut world);
    world
}

/// 远景**高频局部方差**：逐像素减去 (2r+1)² 盒式均值后的残差平方均值，
/// 区域 [x0,x1) × [y0,y1)。
///
/// 为什么不直接用整块方差：区域里天空 / 地平线 / 地面的大尺度亮度分层
/// 模糊本就抹不掉（低频），直接方差会被这一层主导、稀释模糊效果。高频
/// 残差只度量模糊能消除的部分（异色立方体交替纹理），是 CoC 盘式平均
/// 作用面的正确度量——它仍是严格定义的方差（高通信号的二阶矩）。
fn region_highfreq_variance(
    px: &[u8],
    w: u32,
    h: u32,
    xs: std::ops::Range<u32>,
    ys: std::ops::Range<u32>,
    radius: u32,
) -> f64 {
    let luma = |px: &[u8], x: u32, y: u32| -> f64 {
        let i = ((y * w + x) * 4) as usize;
        0.299 * px[i] as f64 + 0.587 * px[i + 1] as f64 + 0.114 * px[i + 2] as f64
    };
    let r = radius as i64;
    let mut sum = 0.0;
    let mut count = 0u64;
    for y in ys.clone() {
        for x in xs.clone() {
            let mut acc = 0.0;
            let mut n = 0u64;
            for dy in -r..=r {
                for dx in -r..=r {
                    let sx = (x as i64 + dx).clamp(0, w as i64 - 1) as u32;
                    let sy = (y as i64 + dy).clamp(0, h as i64 - 1) as u32;
                    acc += luma(px, sx, sy);
                    n += 1;
                }
            }
            let hp = luma(px, x, y) - acc / n as f64;
            sum += hp * hp;
            count += 1;
        }
    }
    sum / count as f64
}

/// DOF 生效：远景（画面上半中带，覆盖远处异色立方体行）高频局部方差
/// 显著下降——12 纹素半径的泊松盘把间距 ≈ 13 px 的异色交替纹理基本
/// 抹平；容差依据：CoC 打满（半径 12 纹素 ≥ 立方体间距）时高频残差应
/// 被压到量化噪声量级（±0.5/255 → 方差 < 1）附近的数十倍余量内，取
/// 0.5 倍为留有余量的上界。
#[test]
fn dof_blurs_far_background_reducing_variance() {
    let Some(mut renderer) = OffscreenRenderer::new(320, 240) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let (w, h) = renderer.size();
    renderer.render(&dof_scene(None));
    let without = renderer.read_pixels();
    renderer.render(&dof_scene(Some(DepthOfFieldSettings {
        enabled: true,
        // 聚焦在近处立方体（视距 ≈ 6），清晰带半宽 0.5 → 远景全模糊
        focus_distance: 6.0,
        focus_range: 0.5,
        max_blur_radius: 12.0,
    })));
    let with = renderer.read_pixels();

    // 远景区域：上半中带（远处异色立方体行所在）
    let xs = w / 4..w * 3 / 4;
    let ys = h / 8..h / 2;
    let var_without = region_highfreq_variance(&without, w, h, xs.clone(), ys.clone(), 6);
    let var_with = region_highfreq_variance(&with, w, h, xs, ys, 6);
    assert!(
        var_with < var_without * 0.5,
        "DOF 开启后远景高频方差应显著下降（模糊抹平异色交替）：\
         无 DOF {var_without:.1} vs 有 DOF {var_with:.1}"
    );

    // 聚焦带近无损：近处立方体在清晰带内（CoC = 0 原样透传），中心区域
    // 均值不应因 DOF 明显变化（立方体边缘混入远景深度属采样噪声级）
    let center_mean = |px: &[u8]| -> f64 {
        let mut sum = 0u64;
        let mut count = 0u64;
        for y in h * 7 / 16..h * 9 / 16 {
            for x in w * 7 / 16..w * 9 / 16 {
                let i = ((y * w + x) * 4) as usize;
                sum += (px[i] as u64) + (px[i + 1] as u64) + (px[i + 2] as u64);
                count += 1;
            }
        }
        sum as f64 / count as f64
    };
    let near_without = center_mean(&without);
    let near_with = center_mean(&with);
    assert!(
        (near_without - near_with).abs() <= 4.0,
        "聚焦带内像素应基本不变（CoC = 0 透传）：无 DOF {near_without:.1} vs \
         有 DOF {near_with:.1}"
    );
}

/// DOF 透传逐位锁定：清晰带半宽覆盖全场景深度范围（1000 m ≥ z_far）时
/// 每像素 CoC 恒 0、select 走原色透传分支——即使 pass 入链，整帧也应与
/// 无 DOF 帧逐位一致（纹理采样在纹素中心返回精确原值）。
#[test]
fn dof_focus_range_covering_scene_is_bitwise_transparent() {
    let Some(mut renderer) = OffscreenRenderer::new(320, 240) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    renderer.render(&dof_scene(None));
    let without = renderer.read_pixels();
    renderer.render(&dof_scene(Some(DepthOfFieldSettings {
        enabled: true,
        focus_distance: 6.0,
        focus_range: 1000.0,
        max_blur_radius: 12.0,
    })));
    let with = renderer.read_pixels();
    assert_eq!(
        without, with,
        "清晰带覆盖全场景深度时 DOF 应逐位透传（CoC 恒 0）"
    );
}

/// 运动模糊测试场景：中央立方体群（异色）+ 灰色地面，相机平移时边缘
/// 密集，拖影平滑可量化。
fn mb_scene(mb: Option<MotionBlurSettings>, camera_x: f32) -> World {
    use glam::Vec3;
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(camera_x, 2.0, 6.0),
        target: Vec3::new(camera_x, 1.0, 0.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        intensity: 0.8,
        ambient: Vec3::splat(0.08),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    if let Some(mb) = mb {
        world.insert_resource(mb);
    }
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(80.0));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mut mats = world.resource_mut::<MaterialStore>();
    let ground = mats.add(MaterialDef::solid(0.45, 0.45, 0.48, 1.0));
    let red = mats.add(MaterialDef::solid(0.9, 0.3, 0.2, 1.0));
    let green = mats.add(MaterialDef::solid(0.15, 0.85, 0.3, 1.0));
    let blue = mats.add(MaterialDef::solid(0.2, 0.35, 0.95, 1.0));
    drop(mats);
    world.spawn((Transform::IDENTITY, plane, ground));
    for (x, mat) in [(-1.5f32, red), (0.0, green), (1.5, blue)] {
        world.spawn((
            Transform {
                position: Vec3::new(x, 1.0, 0.0),
                ..Transform::IDENTITY
            },
            cube,
            mat,
        ));
    }
    propagate_transforms(&mut world);
    world
}

/// 区域内梯度能量（相邻像素亮度差的**平方**均值），区域 [x0,x1) ×
/// [y0,y1)。direction = true 测水平差、false 测垂直差。
///
/// 选 L2 而非 L1 的依据：盒式模糊近似**保全变差**（L1 总量），阶梯边
/// 缘摊宽后 |Δ| 总和不变——L1 对模糊不敏感；L2 对高频敏感（边缘摊宽
/// W 倍 → 平方梯度降 W 倍），是拖影强度的正确度量。
fn region_gradient_energy(
    px: &[u8],
    w: u32,
    x0: u32,
    x1: u32,
    y0: u32,
    y1: u32,
    horizontal: bool,
) -> f64 {
    let luma = |px: &[u8], x: u32, y: u32| -> f64 {
        let i = ((y * w + x) * 4) as usize;
        0.299 * px[i] as f64 + 0.587 * px[i + 1] as f64 + 0.114 * px[i + 2] as f64
    };
    let mut sum = 0.0;
    let mut count = 0u64;
    for y in y0..y1 {
        for x in x0..x1 {
            let l = luma(px, x, y);
            let l_next = if horizontal {
                if x + 1 >= x1 {
                    continue;
                }
                luma(px, x + 1, y)
            } else {
                if y + 1 >= y1 {
                    continue;
                }
                luma(px, x, y + 1)
            };
            sum += (l_next - l) * (l_next - l);
            count += 1;
        }
    }
    sum / count as f64
}

/// MB 透传逐位锁定：启用 MB 但相机静止（两帧矩阵逐位相同 → 速度恒 0 →
/// n = 1 → select 原色透传），整帧与无 MB 帧逐位一致。这是「启用≠改变
/// 画面」的强断言：静止场景下的运动模糊必须是纯空转。
#[test]
fn motion_blur_static_camera_is_bitwise_transparent() {
    let Some(mut renderer) = OffscreenRenderer::new(320, 240) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    renderer.render(&mb_scene(None, 0.0));
    let without = renderer.read_pixels();
    renderer.render(&mb_scene(
        Some(MotionBlurSettings {
            enabled: true,
            ..Default::default()
        }),
        0.0,
    ));
    let first_frame = renderer.read_pixels();
    assert_eq!(
        without, first_frame,
        "MB 启用 + 首帧（无上帧矩阵）应逐位透传"
    );
    renderer.render(&mb_scene(
        Some(MotionBlurSettings {
            enabled: true,
            ..Default::default()
        }),
        0.0,
    ));
    let second_frame = renderer.read_pixels();
    assert_eq!(
        without, second_frame,
        "MB 启用 + 相机静止（两帧矩阵相同 → 速度 0）应逐位透传"
    );
}

/// MB 生效：相机纯水平平移（无旋转，速度向量无垂直分量）后，拖影使
/// **水平**梯度能量（L2）显著下降、垂直梯度能量近似不变——方向选择性
/// 是「沿速度方向采样」的直接证据。容差依据：帧间位移 ≈ 0.3 UV →
/// 默认强度 32 下 tap 数 ~10，盒式滤波把边缘摊宽 ~10 倍 → 平方梯度降
/// 一个量级，取 0.7 留余量；垂直方向速度恒 0（纯平移无垂直视差分量），
/// 理论上逐位不变，1.2 倍余量只覆盖 8-bit 量化扰动。
#[test]
fn motion_blur_panning_reduces_gradient_energy() {
    let Some(mut renderer) = OffscreenRenderer::new(320, 240) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let (w, h) = renderer.size();
    // 平移后的相机位姿（两测试共用），平移量 1.5 m @ 距离 6 m ≈ 0.3 UV
    let panned = 1.5f32;
    // 参照：同位姿、无 MB
    renderer.render(&mb_scene(None, panned));
    let without = renderer.read_pixels();
    // MB 开：首帧静止位（建立 prev 矩阵），次帧平移位 → 帧间拖影
    renderer.render(&mb_scene(
        Some(MotionBlurSettings {
            enabled: true,
            ..Default::default()
        }),
        0.0,
    ));
    renderer.render(&mb_scene(
        Some(MotionBlurSettings {
            enabled: true,
            ..Default::default()
        }),
        panned,
    ));
    let with = renderer.read_pixels();

    assert_ne!(
        without, with,
        "相机平移时 MB 必须改变画面（速度非零 → 拖影）"
    );
    let (x0, x1, y0, y1) = (w / 4, w * 3 / 4, h / 4, h * 3 / 4);
    let h_without = region_gradient_energy(&without, w, x0, x1, y0, y1, true);
    let h_with = region_gradient_energy(&with, w, x0, x1, y0, y1, true);
    let v_without = region_gradient_energy(&without, w, x0, x1, y0, y1, false);
    let v_with = region_gradient_energy(&with, w, x0, x1, y0, y1, false);
    assert!(
        h_with < h_without * 0.7,
        "平移帧的拖影应显著降低水平梯度能量（运动方向）：\
         无 MB {h_without:.1} vs 有 MB {h_with:.1}"
    );
    assert!(
        v_with < v_without * 1.2,
        "纯水平平移不应显著抬高垂直梯度能量（速度无垂直分量）：\
         无 MB {v_without:.1} vs 有 MB {v_with:.1}"
    );
    assert!(
        h_with / h_without < v_with / v_without,
        "水平方向衰减必须强于垂直方向（方向选择性）：\
         水平 {:.2} vs 垂直 {:.2}",
        h_with / h_without,
        v_with / v_without
    );
}

/// 确定性（同输入两次跑逐位一致）：DOF 开启场景渲染两遍全帧相等。
/// 深度副本 pass 与 DOF/MB pass 均无随机性来源（泊松盘偏移是编译期
/// 常量表，无状态哈希流不涉及）。
#[test]
fn dof_render_is_deterministic() {
    let Some(mut renderer) = OffscreenRenderer::new(320, 240) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let world = dof_scene(Some(DepthOfFieldSettings {
        enabled: true,
        focus_distance: 6.0,
        focus_range: 0.5,
        max_blur_radius: 12.0,
    }));
    renderer.render(&world);
    let first = renderer.read_pixels();
    renderer.render(&world);
    let second = renderer.read_pixels();
    assert_eq!(first, second, "同输入两次 DOF 渲染必须逐位一致");
}
