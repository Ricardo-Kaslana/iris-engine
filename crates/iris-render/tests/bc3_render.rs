//! 集成测试：BC3 纹理压缩上传路径（R59）。
//!
//! 三条主线（口径与 `bc1_render.rs` 对齐）：
//! 1. **准入闸门**：同一半透明棋盘格纹理，**经 `declare_bc3` 声明**且
//!    BC 设备可用 → 必须走 BC3 上传（`Renderer::texture_is_bc3`）；
//!    **未声明的同款纹理** → 维持既有决策树回退 RGBA8（默认路径逐位
//!    不变的 GPU 侧证据）。alpha=255 全不透明副本即使声明 BC3 也归
//!    BC1（分层互斥：BC1 无 alpha 不丢语义且压缩率翻倍）。
//! 2. **等价渲染**：BC3 路径与 RGBA8 回退路径各用独立渲染器渲染同一
//!    alpha 混合场景，全帧平均绝对误差应有界——棋盘格 alpha 为块内
//!    纯色（40 / 255）→ BC4 端点即块内常数、索引全 0，alpha 解码精确；
//!    RGB 为黑 / 白纯色块 → BC1 端点精确还原。无 BC 特性环境两侧都走
//!    RGBA，断言退化为恒等；无 GPU 环境优雅跳过。
//! 3. **显存口径**：声明前后 `TextureStore::gpu_bytes_estimate` 之差
//!    即压缩节省（4 B/px → 1 B/px），在 CPU 侧即可断言（无 GPU 也跑）。

use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 半透明棋盘格纹理：64²、8px 格。颜色黑 / 白（565 端点精确），alpha
/// 黑格 40、白格 255（块内纯色 → BC4 端点精确、索引无歧义）。
fn translucent_checker() -> CpuTexture {
    let mut tex = CpuTexture::checkerboard(8, 8, [0, 0, 0, 40], [255, 255, 255, 255]);
    for (i, px) in tex.rgba.chunks_exact_mut(4).enumerate() {
        if i % 2 == 0 {
            px[3] = 40;
        }
    }
    tex
}

/// 构建场景：alpha 混合材质挂贴图棋盘格立方体（Blend 模式消费纹理
/// alpha——BC3 的 alpha 语义因此真正进入画面）。
fn scene_world(tex: CpuTexture, declare_bc3: bool) -> (World, TextureHandle) {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());
    let tex_handle = if declare_bc3 {
        world.resource_mut::<TextureStore>().add_bc3_declared(tex)
    } else {
        world.resource_mut::<TextureStore>().add(tex)
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

/// 与画面角落（天空背景）显著不同的像素数（场景可见性 sanity）。
fn count_scene_pixels(px: &[u8], w: u32, h: u32) -> u32 {
    let sky = [px[0], px[1], px[2]];
    let mut total = 0u32;
    for i in 0..(w * h) as usize {
        let d = (0..3).any(|c| (px[i * 4 + c] as i32 - sky[c] as i32).abs() > 12);
        if d {
            total += 1;
        }
    }
    total
}

#[test]
fn bc3_upload_declared_gated_and_rendering_close_to_rgba() {
    // ---- 显存口径（纯 CPU，无 GPU 也跑）----
    let mut store = TextureStore::new();
    let handle = store.add(translucent_checker());
    let rgba_estimate = store.gpu_bytes_estimate(true);
    store.declare_bc3(handle);
    let bc3_estimate = store.gpu_bytes_estimate(true);
    assert_eq!(
        rgba_estimate,
        4 + 64 * 64 * 4,
        "未声明半透明纹理按 RGBA8 4 B/px 计"
    );
    assert_eq!(
        bc3_estimate,
        4 + (64 / 4) * (64 / 4) * 16,
        "声明后半透明纹理按 BC3 1 B/px 计"
    );

    // ---- GPU 路径（无 GPU 优雅跳过）----
    let Some(mut renderer_bc3) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut renderer_rgba) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let bc = renderer_bc3.gpu().supports_bc();

    // ---- 路径 A：声明过 → BC 设备必须走 BC3 ----
    let (world_a, tex_a) = scene_world(translucent_checker(), true);
    renderer_bc3.render(&world_a);
    assert_eq!(
        renderer_bc3.renderer().texture_is_bc3(tex_a),
        bc,
        "BC 设备上已声明的半透明纹理必须走 BC3；无特性设备必须回退"
    );
    let (w, h) = renderer_bc3.size();
    let px_bc3 = renderer_bc3.read_pixels();
    assert!(
        count_scene_pixels(&px_bc3, w, h) > 200,
        "半透明棋盘格立方体必须出现在画面中"
    );

    // ---- 路径 B：同款纹理未声明 → 维持既有决策树回退 RGBA8 ----
    let (world_b, tex_b) = scene_world(translucent_checker(), false);
    renderer_rgba.render(&world_b);
    assert!(
        !renderer_rgba.renderer().texture_is_bc3(tex_b),
        "未声明纹理绝不走 BC3（准入闸门：默认路径逐位不变）"
    );
    assert!(
        !renderer_rgba.renderer().texture_is_bc1(tex_b),
        "半透明纹理本就不满足 BC1 不透明条件"
    );
    let px_rgba = renderer_rgba.read_pixels();
    assert!(count_scene_pixels(&px_rgba, w, h) > 200);

    // ---- 等价性：全帧平均误差有界（块内纯色棋盘格 BC3 解码精确）----
    let err = mean_abs_error(&px_bc3, &px_rgba);
    assert!(
        err < 2.0,
        "BC3 路径与 RGBA 回退路径的全帧平均误差 {err:.3} 应 < 2/255 \
         （块内纯色棋盘格 alpha 与 RGB 解码均精确；无 BC 特性环境两侧同为 \
         RGBA，本断言退化为恒等）"
    );
}

#[test]
fn bc3_declared_opaque_falls_back_to_bc1_layering() {
    let Some(mut renderer) = OffscreenRenderer::new(128, 128) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let bc = renderer.gpu().supports_bc();
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());
    // 全不透明棋盘格：声明 BC3 也必须归 BC1（分层互斥——全不透明走 BC1
    // 是 bc1_eligible / bc3_eligible 的判定口径，渲染器决策树与其一致）
    let tex = CpuTexture::checkerboard(8, 8, [0, 0, 0, 255], [255, 255, 255, 255]);
    let handle = world.resource_mut::<TextureStore>().add_bc3_declared(tex);
    let mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
        texture: handle,
        ..MaterialDef::default()
    });
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    world.spawn((Transform::from_xyz(0.0, 0.0, -4.0), cube, mat));
    propagate_transforms(&mut world);
    renderer.render(&world);
    assert_eq!(
        renderer.renderer().texture_is_bc1(handle),
        bc,
        "全不透明已声明纹理必须走 BC1（分层规则），无特性设备回退 RGBA8"
    );
    assert!(
        !renderer.renderer().texture_is_bc3(handle),
        "全不透明纹理绝不走 BC3（与 BC1 恒互斥）"
    );
}
