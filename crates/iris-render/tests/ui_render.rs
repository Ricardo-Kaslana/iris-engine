//! 集成测试：2D UI 叠加层（HUD）——屏幕空间矩形 + 位图字体文本，
//! 渲染在 3D 场景色调映射 blit 之上。
//!
//! 断言用主导通道/区间比较：3D 部分输出穿透 sRGB + ACES，UI 直接写
//! sRGB 目标，精确值断言不可靠；「亮红 / 蓝主导」等签名比较对混合
//! 与编码路径的偏差远比精确值稳健。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;
use iris_render::ui::glyph_bits;

/// 基础世界：空目录 + 默认相机 + 方向光（关阴影，简化确定性）+ 泛光关闭。
fn base_world() -> World {
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
    world
}

/// 「亮红」像素签名：UI 红 (1.0, 0.1, 0.1) → sRGB ≈ (255, 89, 89)。
/// 程序化天空（天顶蓝 / 地平线白 / 暖棕地面，经 ACES）远达不到该签名，
/// 不会误报。
fn is_bright_red(px: &[u8], i: usize) -> bool {
    let (r, g, b) = (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32);
    r > 180 && r > g + 60 && r > b + 60
}

/// 用例一「矩形」：空场景（只有相机/灯）上的红色 UiRect。
/// 区域内部全部亮红；紧贴矩形的外框带零亮红溢出——位置与尺寸按
/// 像素精确落地（左上原点、y 向下坐标系）。
#[test]
fn ui_rect_draws_at_pixel_coordinates() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = base_world();
    world.spawn((
        UiRect {
            position: [64.0, 64.0],
            size: [64.0, 64.0],
        },
        UiColor([1.0, 0.1, 0.1, 1.0]),
    ));
    renderer.render(&world);
    let (w, h) = renderer.size();
    assert_eq!((w, h), (256, 256));
    let px = renderer.read_pixels();

    // 区域内部（68..124，避开对齐边缘）应全部亮红
    let mut red = 0u32;
    let total = (124 - 68) * (124 - 68);
    for y in 68..124 {
        for x in 68..124 {
            let i = ((y * w + x) * 4) as usize;
            if is_bright_red(&px, i) {
                red += 1;
            }
        }
    }
    assert_eq!(red, total, "矩形内部应全部亮红，实际 {red}/{total}");

    // 紧贴矩形的外框带（12px）零亮红溢出
    let mut stray = 0u32;
    for y in 52..140 {
        for x in 52..140 {
            if (64..128).contains(&x) && (64..128).contains(&y) {
                continue;
            }
            let i = ((y * w + x) * 4) as usize;
            if is_bright_red(&px, i) {
                stray += 1;
            }
        }
    }
    assert_eq!(stray, 0, "矩形外框带不应出现亮红溢出，实际 {stray}");
}

/// 用例二「文本」：UiText "AAAA" scale 4（等宽 32px/字符）白字于暗场景。
/// 逐亮位 quad 精确覆盖 scale×scale 像素块，解析预测 =
/// 4 字符 × popcount('A') × 16 px²；容忍 ±25% 证明逐字形位展开正确。
#[test]
fn ui_text_matches_analytic_bit_prediction() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = base_world();
    // 暗色地面铺满画面（相机俯视，最上沿视线仍落在地面内），弱光保持背景远暗于白字
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.05, 0.05, 0.05, 1.0));
    world.spawn((Transform::IDENTITY, plane, mat));
    {
        let mut light = world.resource_mut::<DirectionalLight>();
        light.intensity = 0.3;
        light.ambient = Vec3::splat(0.02);
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 4.0, 2.0);
        camera.target = Vec3::ZERO;
    }
    world.spawn((
        UiRect {
            position: [32.0, 100.0],
            size: [128.0, 32.0],
        },
        UiText {
            string: "AAAA".into(),
            scale: 4.0,
        },
        UiColor::default(),
    ));
    propagate_transforms(&mut world);
    renderer.render(&world);
    let (w, _) = renderer.size();
    let px = renderer.read_pixels();

    // 文本区域（32..160 × 100..132）：白字像素和 ≈ 765，暗背景 ≪ 400
    let mut lit = 0u32;
    for y in 100..132 {
        for x in 32..160 {
            let i = ((y * w + x) * 4) as usize;
            let sum = px[i] as u32 + px[i + 1] as u32 + px[i + 2] as u32;
            if sum > 400 {
                lit += 1;
            }
        }
    }
    let predicted = 4
        * glyph_bits(b'A')
            .unwrap()
            .iter()
            .map(|b| b.count_ones())
            .sum::<u32>()
        * 16;
    assert_eq!(predicted, 1792, "解析预测基准：4 字符 × 28 亮位 × 16 px²");
    assert!(
        (lit as i64 - predicted as i64).abs() <= predicted as i64 / 4,
        "'AAAA' 亮像素数应在解析预测 {predicted} ±25% 内，实际 {lit}"
    );
}

/// 用例三「叠加于 3D 之上」：地面 + 立方体常规场景 + 底部 HUD 条。
/// HUD 区域蓝主导且不透 3D（alpha=1 全覆盖）；HUD 外区域与无 UI
/// 渲染逐像素一致——HUD 不影响 3D 场景。
#[test]
fn ui_hud_overlays_3d_without_touching_scene() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let scene = |with_hud: bool| {
        let mut world = base_world();
        let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
        let mat = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.9, 0.3, 0.1, 1.0));
        world.spawn((Transform::from_xyz(0.0, 1.5, 0.0), cube, mat));
        let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
        let mat_ground = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.5, 0.5, 0.5, 1.0));
        world.spawn((Transform::IDENTITY, plane, mat_ground));
        if with_hud {
            world.spawn((
                UiRect {
                    position: [0.0, 224.0],
                    size: [256.0, 32.0],
                },
                UiColor([0.2, 0.5, 1.0, 1.0]),
            ));
        }
        propagate_transforms(&mut world);
        world
    };
    renderer.render(&scene(true));
    let with_hud = renderer.read_pixels();
    renderer.render(&scene(false));
    let without_hud = renderer.read_pixels();
    let (w, _) = renderer.size();

    // HUD 内部：全部像素蓝主导（sRGB 蓝 (0.2,0.5,1.0) ≈ (123,188,255)）
    let mut blue = 0u32;
    let total = (252 - 228) * (248 - 8);
    for y in 228..252 {
        for x in 8..248 {
            let i = ((y * w + x) * 4) as usize;
            let (r, g, b) = (
                with_hud[i] as i32,
                with_hud[i + 1] as i32,
                with_hud[i + 2] as i32,
            );
            if b > r + 40 && b > g + 20 {
                blue += 1;
            }
        }
    }
    assert_eq!(
        blue, total,
        "HUD 条内部应全部蓝主导（不透 3D），实际 {blue}/{total}"
    );

    // HUD 外部：与无 UI 渲染一致（±12 和差容差，与既有差分测试同款）
    let mut changed = 0u32;
    for y in 0..200 {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let d = (with_hud[i] as i32 + with_hud[i + 1] as i32 + with_hud[i + 2] as i32)
                - (without_hud[i] as i32 + without_hud[i + 1] as i32 + without_hud[i + 2] as i32);
            if d.abs() > 12 {
                changed += 1;
            }
        }
    }
    assert!(
        changed <= 4,
        "HUD 外区域不应受 UI 影响（照常渲染 3D），实际 {changed} 像素变化"
    );
}
