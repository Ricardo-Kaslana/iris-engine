//! 集成测试：外部 HDR 环境贴图（Radiance .hdr equirectangular → IBL + 天空盒）。
//!
//! fixture 是一张 8×4 的合成全景图：上半 2 行纯红 (4, 0, 0)、下半 2 行纯蓝
//! (0, 0, 4)——>1.0 的值顺带验证 HDR 通路，红/蓝对撞让「半球颠倒 / 垂直镜像 /
//! 捕获失效」都直接表现为主导通道反转。经 `encode_hdr → parse_hdr` 往返构造
//! （同时覆盖解析器），落盘临时文件再走 `load_hdr` 完整加载链。
//!
//! 断言用主导通道比较：渲染输出穿透 sRGB + ACES，精确值断言不可靠。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::asset::{encode_hdr, load_hdr, parse_hdr, HdrImage};
use iris_render::prelude::*;

/// 合成红/蓝 fixture 并走完整资产链（往返 → 临时文件 → load_hdr）。
fn red_blue_fixture() -> HdrImage {
    let mut image = HdrImage::new(8, 4);
    for y in 0..4u32 {
        for x in 0..8u32 {
            image.set_pixel(x, y, if y < 2 { [4.0, 0.0, 0.0] } else { [0.0, 0.0, 4.0] });
        }
    }
    let bytes = encode_hdr(&image);
    let parsed = parse_hdr(&bytes).expect("fixture 往返解析应成功");
    for y in 0..4u32 {
        for x in 0..8u32 {
            let a = image.pixel(x, y);
            let b = parsed.pixel(x, y);
            assert!(
                a.iter().zip(b.iter()).all(|(p, q)| (p - q).abs() < 1e-5),
                "fixture 往返失真：({x},{y}) {a:?} vs {b:?}"
            );
        }
    }
    let dir = std::env::temp_dir().join("iris_hdr_render_tests");
    std::fs::create_dir_all(&dir).expect("创建临时目录");
    let path = dir.join("red_blue_equirect.hdr");
    // 内容恒定，Once 防并行测试重复写（两用例共用同一 fixture 路径，
    // 并发覆写会让对方读到被截断的半截文件——偶发竞态）
    static WRITE_ONCE: std::sync::Once = std::sync::Once::new();
    WRITE_ONCE.call_once(|| {
        std::fs::write(&path, &bytes).expect("写入临时 .hdr");
    });
    load_hdr(&path).expect("fixture 文件加载应成功")
}

/// 基础世界：空目录 + 默认相机 + 弱方向光（让天空 / IBL 主导画面）+ 外部 HDR 天空源。
fn world_with_hdr_sky(sky: HdrImage) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight {
        intensity: 0.3,
        ambient: Vec3::splat(0.02),
        ..DirectionalLight::default()
    });
    world.insert_resource(SkySource::equirect_hdr(sky));
    world
}

/// 用例一「背景捕获」：空场景渲染，画面上半区应红主导、下半区应蓝主导——
/// equirect 上传 → 方向采样捕获 → 立方体贴图 → skybox 全链路的视觉签名。
#[test]
fn hdr_skybox_captures_equirect_halves() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let world = world_with_hdr_sky(red_blue_fixture());
    // 水平视线（target 与 eye 同高）：上下半球的分界（地平线）落在画面正中
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.target = Vec3::new(0.0, 1.8, 0.0);
    }
    renderer.render(&world);
    let (w, h) = renderer.size();
    let px = renderer.read_pixels();

    // 取画面上 1/4 与下 1/4 区域统计主导通道；贴近地平线的一小条混色带
    // （fixture 只有 4 行，equirect 垂直量化宽）不计入——阈值远低于该区域
    // 内理论上限，混色带有多余余量
    let mut red_dominant = 0u32;
    let mut blue_dominant = 0u32;
    for y in 0..h / 4 {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            if px[i] as i32 > px[i + 2] as i32 + 40 {
                red_dominant += 1;
            }
        }
    }
    for y in h * 3 / 4..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            if px[i + 2] as i32 > px[i] as i32 + 40 {
                blue_dominant += 1;
            }
        }
    }
    let region = w * (h / 4);
    assert!(
        red_dominant > region / 4,
        "画面上 1/4 应红主导（equirect 上半球 → 天空盒），实际 {red_dominant}/{region}"
    );
    assert!(
        blue_dominant > region / 4,
        "画面下 1/4 应蓝主导（equirect 下半球 → 天空盒），实际 {blue_dominant}/{region}"
    );
}

/// 用例二「IBL 反射」：金属铬球（metallic 1.0, roughness 0.05）反射上半球
/// 红色辐射——验证 IBL 分割求和（辐照度 + 预滤波 + BRDF LUT）吃到 HDR 源。
#[test]
fn hdr_ibl_reflects_red_upper_half_on_chrome() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = world_with_hdr_sky(red_blue_fixture());
    let sphere = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, 32, 24));
    let chrome = world.resource_mut::<MaterialStore>().add(MaterialDef {
        metallic: 1.0,
        roughness: 0.05,
        ..MaterialDef::default()
    });
    world.spawn((Transform::from_xyz(0.0, 0.0, 0.0), sphere, chrome));
    // 相机对准球心且偏高：球面正对区域反射的是「指向相机」的方向
    // （俯角约 32°，远高于 fixture 的红/蓝分界 0°），中央区域必为纯红
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 2.5, 4.0);
        camera.target = Vec3::ZERO;
    }
    propagate_transforms(&mut world);
    renderer.render(&world);
    let (w, h) = renderer.size();
    let px = renderer.read_pixels();

    // 球心附近小区域（±1/16 画面）：全部落在球面上（球角半径约 12°），
    // 且反射俯角都高于红/蓝分界——统计红主导像素占比
    let mut red = 0u32;
    let mut total = 0u32;
    for y in h * 7 / 16..h * 9 / 16 {
        for x in w * 7 / 16..w * 9 / 16 {
            total += 1;
            let i = ((y * w + x) * 4) as usize;
            if px[i] as i32 > px[i + 2] as i32 + 40 {
                red += 1;
            }
        }
    }
    assert!(
        red > total / 2,
        "铬球球心区域应反射红色上半球（IBL 分割求和吃到 HDR 源），实际 {red}/{total}"
    );
}
