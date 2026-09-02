//! 集成测试：后处理链 v1——曝光控制与暗角（pass 串联框架的示范 pass）。
//!
//! 曝光断言依赖 ACES（Narkowicz）在正半轴严格单调：线性 HDR × 曝光后
//! 色调映射，曝光 0.5 / 1.0 / 2.0 的同场景中心亮度必严格递增（发光体
//! 亮度取值保证三档都落在 ACES 非饱和段）。暗角断言依赖其链位与解析
//! 表达式：全分辨率 HDR → HDR 压暗四角（中心 falloff 恒 0，逐位无损），
//! 强度 0 / 资源缺席时整段跳过，与无 pass 帧逐位一致（零开销默认路径）。

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
