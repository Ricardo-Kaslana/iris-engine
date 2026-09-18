//! 集成测试：KTX2 预压缩纹理直传路径（C5a-2）。
//!
//! 与 `bc7_render.rs`（引擎运行时编码 → GPU）互补，本轮考察的是
//! **预压缩块数据免编码直传**这条独立代码路径：
//! 1. **容器 ↔ 上传接线（GPU）**：用引擎 `encode_bc7` 产出真实 BC7 块，
//!    按规范布局组装成未压缩 + mip 双级的 KTX2 容器，`add_ktx2` 入库后
//!    `ensure_texture` 短路决策树走 `upload_compressed`——BC 设备上
//!    `texture_is_bc7` 必须为真（观测口径按容器族置位），且无 BC 特性
//!    设备绑白兜底、绝不进上传（`supports_bc()` 为假时该句柄不入缓存）；
//! 2. **硬件解码保真（GPU，位序正确性最终裁判）**：容器里的块交给 GPU
//!    硬件 BC7 解码器采样，与「同款棋盘格走 RGBA8 直传」的参考帧逐像素
//!    比对——块恒定纯色 ⇒ Mode 6 端点无损，误差只应来自 alpha 混合采样
//!    噪声。若解析器级偏移/`bytes_per_row`/级维数推算任一环节错位，
//!    硬件解出噪声 → 误差爆炸，本断言即刻失败。
//!
//! 诚实限制：容器不带 RGBA8 回退数据（裁定「仅未压缩 + mip 级」），
//! 故无 BC 特性设备只能白图兜底——本测试在无 BC 环境下跳过保真段。

use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 文件内互斥串行化（重负载 GPU 套件并行会挂死，AGENTS 规约）。
static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 半透明棋盘格（与 `bc7_render::checker` 同参）：64² 基级、8px 格、
/// 黑格 alpha=40 / 白格 alpha=255；每 4×4 块恒定纯色 ⇒ Mode 6 无损。
fn base_checker() -> CpuTexture {
    let mut tex = CpuTexture::checkerboard(8, 8, [0, 0, 0, 40], [255, 255, 255, 255]);
    for (i, px) in tex.rgba.chunks_exact_mut(4).enumerate() {
        if i % 2 == 0 {
            px[3] = 40;
        }
    }
    tex
}

/// 32² 单色级（充当 mip 1）：仅验证多级上传接线（`bytes_per_row` /
/// `level_extent` / 逐级 `write_texture`），不参与保真比对。
fn mip_solid() -> Vec<u8> {
    encode_bc7(&vec![200u8; 32 * 32 * 4], 32, 32)
}

/// KTX2 世界：`add_ktx2` 入库预压缩句柄 + 贴同尺寸材质的立方体。
fn ktx2_world(bytes: &[u8]) -> (World, TextureHandle) {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());
    let tex_handle = world
        .resource_mut::<TextureStore>()
        .add_ktx2(bytes)
        .expect("合法 KTX2 应入库");
    let mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
        texture: tex_handle,
        alpha_mode: AlphaMode::Blend,
        ..MaterialDef::default()
    });
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    world.spawn((Transform::from_xyz(0.0, 0.0, -4.0), cube, mat));
    propagate_transforms(&mut world);
    (world, tex_handle)
}

/// RGBA8 参考世界：同款棋盘格未声明（半透明 ⇒ BC1 否决回 RGBA8 直传）。
fn rgba_world(tex: CpuTexture) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());
    let tex_handle = world.resource_mut::<TextureStore>().add(tex);
    let mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
        texture: tex_handle,
        alpha_mode: AlphaMode::Blend,
        ..MaterialDef::default()
    });
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    world.spawn((Transform::from_xyz(0.0, 0.0, -4.0), cube, mat));
    propagate_transforms(&mut world);
    world
}

/// 全帧平均绝对误差（RGB 三通道，0-255 刻度）。
fn mean_abs_error(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let mut total = 0f64;
    for i in 0..(a.len() / 4) {
        for c in 0..3 {
            total += (a[i * 4 + c] as i32 - b[i * 4 + c] as i32).abs() as f64;
        }
    }
    total / (a.len() / 4) as f64 / 3.0
}

/// 纯 CPU 侧：解析器 + 目录侧表登记（无 GPU 也跑）——容器块数据被
/// `add_ktx2` 原样读进侧表，元数据占位宽高/srgb 真值、rgba 空表。
#[test]
fn ktx2_container_parses_and_registers() {
    let base = encode_bc7(&base_checker().rgba, 64, 64);
    let mip = mip_solid();
    let bytes = build_ktx2(146, 64, 64, &[base.clone(), mip.clone()]);
    // 解析器口径（公开口）
    let img = parse_ktx2(&bytes).expect("合法容器应可解析");
    assert_eq!(img.format, Ktx2BcFormat::Bc7Srgb);
    assert_eq!((img.width, img.height), (64, 64));
    assert_eq!(img.levels.len(), 2);
    assert_eq!(img.levels[0], base, "基级块数据原样");
    assert_eq!(img.levels[1], mip, "mip 级块数据原样");
    // 目录侧表口径
    let mut store = TextureStore::new();
    let h = store.add_ktx2(&bytes).expect("入库");
    let c = store.compressed_levels(h).unwrap();
    assert_eq!(c.total_bytes(), (base.len() + mip.len()) as u64);
    assert_eq!(c.level_extent(0), (64, 64));
    assert_eq!(c.level_extent(1), (32, 32));
    assert_eq!(c.level_extent(6), (1, 1), "级维数钳到 ≥1");
}

#[test]
fn ktx2_direct_upload_gated_and_hardware_decodes_faithfully() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let base = encode_bc7(&base_checker().rgba, 64, 64);
    let mip = mip_solid();
    let bytes = build_ktx2(146, 64, 64, &[base, mip]);

    let Some(mut renderer_ktx2) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let bc = renderer_ktx2.gpu().supports_bc();
    let (world, tex) = ktx2_world(&bytes);
    renderer_ktx2.render(&world);
    assert_eq!(
        renderer_ktx2.renderer().texture_is_bc7(tex),
        bc,
        "BC 设备上预压缩句柄必须走 BC7 直传；无特性设备绑白兜底（is_bc7 恒 false）"
    );
    if !bc {
        // 无 BC 特性：容器无 RGBA8 回退 ⇒ 只能白图，保真段无从比对
        return;
    }
    // 硬件解码保真：KTX2 直传帧 vs RGBA8 参考帧（同款棋盘格）
    let Some(mut renderer_rgba) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let ref_world = rgba_world(base_checker());
    renderer_rgba.render(&ref_world);
    let err = mean_abs_error(&renderer_ktx2.read_pixels(), &renderer_rgba.read_pixels());
    assert!(
        err < 3.0,
        "KTX2 直传帧与 RGBA8 参考帧误差 {err:.2} 超界（解析器级偏移/bytes_per_row/级维数错位的兜底闸）"
    );
}
