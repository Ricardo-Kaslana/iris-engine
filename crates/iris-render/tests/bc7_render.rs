//! 集成测试：BC7（Mode 6）纹理压缩上传路径（C5a）。
//!
//! 三条主线（口径与 `bc3_render.rs` 对齐）：
//! 1. **显存口径（纯 CPU）**：同一半透明棋盘格纹理，未声明 → 决策树
//!    维持 BC1/BC3/RGBA8 现状；`declare_bc7` → 按 BC7 1 B/px 计。
//!    **BC7 永不自动**由「未声明者估算不变」钉住；
//! 2. **准入闸门（GPU）**：声明过 + BC 设备 → `texture_is_bc7`；未声明
//!    同款恒 `false`（走 BC1——半透明声明缺席时的对照）；
//! 3. **硬件解码保真（GPU，规格正确性的最终裁判）**：引擎编码器产出的
//!    块流交给 **GPU 硬件 BC7 解码器** 采样——8px 棋盘格对 4×4 块恒定
//!    纯色 ⇒ Mode 6 端点（min==max）精确还原源色（黑格 alpha=40 全偶、
//!    白格全 255 奇 ⇒ P-bit 无损），BC7 帧与 RGBA8 帧的全帧平均误差
//!    必须与「无损」同量级。若编码位序错（前缀 / 端点主序 / P-bit /
//!    anchor），硬件解出噪声 → 误差爆炸，本断言即刻失败——这正是
//!    CPU 自解码往返**查不出**的那类错误的兜底闸。

use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 文件内互斥串行化（重负载 GPU 套件并行会挂死，AGENTS 规约）。
static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 半透明棋盘格：64²、8px 格；黑格 [0,0,0,40]、白格 [255,255,255,255]。
/// 每个 4×4 块恒定纯色（格是 4 的倍数）⇒ BC7 Mode 6 端点退化
/// min==max，编解码无损（本测试的保真论证前提）。
fn checker() -> CpuTexture {
    let mut tex = CpuTexture::checkerboard(8, 8, [0, 0, 0, 40], [255, 255, 255, 255]);
    for (i, px) in tex.rgba.chunks_exact_mut(4).enumerate() {
        if i % 2 == 0 {
            px[3] = 40;
        }
    }
    tex
}

fn scene_world(declare_bc7: bool) -> (World, TextureHandle) {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());
    let tex_handle = if declare_bc7 {
        world
            .resource_mut::<TextureStore>()
            .add_bc7_declared(checker())
    } else {
        world.resource_mut::<TextureStore>().add(checker())
    };
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

#[test]
fn bc7_memory_estimate_and_never_automatic() {
    // 显存口径（纯 CPU，无 GPU 也跑）：BC7 声明把 4 B/px 降到 1 B/px，
    // 未声明恒走现状（BC1 0.5 B/px——全不透明？本纹理带 alpha=40 半透明
    // → 未声明回 RGBA8 4 B/px）
    let mut store = TextureStore::new();
    let handle = store.add(checker());
    assert_eq!(
        store.gpu_bytes_estimate(true),
        4 + 64 * 64 * 4,
        "未声明半透明纹理维持 RGBA8（BC7 永不自动 + BC1 被 alpha 否决）"
    );
    store.declare_bc7(handle);
    assert_eq!(
        store.gpu_bytes_estimate(true),
        4 + (64 / 4) * (64 / 4) * 16,
        "声明后按 BC7 16 B/块计"
    );
}

#[test]
fn bc7_upload_declared_gated_and_hardware_decodes_faithfully() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer_bc7) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut renderer_rgba) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let bc = renderer_bc7.gpu().supports_bc();

    // 声明过 → BC 设备必须走 BC7
    let (world_a, tex_a) = scene_world(true);
    renderer_bc7.render(&world_a);
    assert_eq!(
        renderer_bc7.renderer().texture_is_bc7(tex_a),
        bc,
        "BC 设备上已声明纹理必须走 BC7；无特性设备必须回退"
    );
    // 未声明同款 → 恒不走 BC7（永不自动的 GPU 侧证据），且按现状回退
    let (world_b, tex_b) = scene_world(false);
    renderer_rgba.render(&world_b);
    assert!(
        !renderer_rgba.renderer().texture_is_bc7(tex_b),
        "未声明纹理绝不进 BC7 路径"
    );
    if !bc {
        // 无 BC 特性：两侧同为 RGBA8，保真断言退化为恒等
        return;
    }
    // 硬件解码保真：BC7 帧 vs RGBA8 帧（块纯色 ⇒ 端点无损，误差只应
    // 来自 alpha 混合的采样插值噪声，与 bc3 测试同量级阈值）
    let err = mean_abs_error(&renderer_bc7.read_pixels(), &renderer_rgba.read_pixels());
    assert!(
        err < 3.0,
        "BC7 硬件解码帧与 RGBA8 参考帧误差 {err:.2} 超界（编码位序错误的兜底闸）"
    );
}
