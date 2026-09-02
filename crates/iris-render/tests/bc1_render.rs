//! 集成测试：BC1 纹理压缩上传路径（R30）。
//!
//! 两条主线：
//! 1. **路径决策**：同一棋盘格纹理，全不透明（alpha=255）→ BC 设备上必须
//!    走 BC1 上传（`Renderer::texture_is_bc1`）；同色但 alpha=100 的副本
//!    不满足不透明检查 → 回退 RGBA8。不透明材质不消费纹理 alpha
//!    （standard.wgsl 只取 RGB），两份副本画面内容完全等价——像素对比
//!    因此是**纯纹理格式**的对比。
//! 2. **等价渲染**：两条路径各用独立渲染器（GPU 缓存按句柄号复用，
//!    同一渲染器会命中缓存不走第二份上传）渲染同一场景，全帧平均绝对
//!    误差应近零——棋盘格对 BC1 友好：4×4 块内纯色 → 565 端点精确还原、
//!    索引无歧义，解码与 RGBA 源逐位相同。环境无 BC 特性时两侧都走
//!    RGBA 路径，退化为 RGBA vs RGBA 等价（断言依然有效）；无 GPU 环境
//!    优雅跳过。

use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 棋盘格纹理：64²、8px 格（= 2 个 4×4 块，块内纯色，BC1 精确解码）。
/// 颜色取黑 / 白（565 可精确表示，端点量化零误差）。
fn checker_texture(alpha: u8) -> CpuTexture {
    CpuTexture::checkerboard(8, 8, [0, 0, 0, alpha], [255, 255, 255, alpha])
}

/// 构建场景：目录资源 + 注册贴图的棋盘格立方体（相机默认注视原点方向）。
fn scene_world(tex: CpuTexture) -> (World, TextureHandle) {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight::default());
    let tex_handle = world.resource_mut::<TextureStore>().add(tex);
    let mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
        texture: tex_handle,
        ..MaterialDef::default()
    });
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    world.spawn((Transform::from_xyz(0.0, 0.0, -4.0), cube, mat));
    propagate_transforms(&mut world);
    (world, tex_handle)
}

/// 全帧平均绝对误差（RGB 三通道，0-255 刻度；alpha 不比——两路径纹理
/// alpha 语义不同正是设计的一部分）。
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
fn bc1_upload_matches_rgba_fallback_rendering() {
    let Some(mut renderer_bc) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let Some(mut renderer_rgba) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let bc = renderer_bc.gpu().supports_bc();

    // ---- 路径 A：全不透明（alpha=255）→ BC 设备必须走 BC1 ----
    let (world_a, tex_a) = scene_world(checker_texture(255));
    renderer_bc.render(&world_a);
    assert_eq!(
        renderer_bc.renderer().texture_is_bc1(tex_a),
        bc,
        "BC 设备上不透明纹理必须走 BC1 上传；无特性设备必须回退（决策与特性一致）"
    );
    let (w, h) = renderer_bc.size();
    let px_bc = renderer_bc.read_pixels();
    assert!(
        count_scene_pixels(&px_bc, w, h) > 200,
        "棋盘格立方体必须出现在画面中"
    );

    // ---- 路径 B：alpha=100 副本 → 不透明检查失败，恒走 RGBA8 ----
    let (world_b, tex_b) = scene_world(checker_texture(100));
    renderer_rgba.render(&world_b);
    assert!(
        !renderer_rgba.renderer().texture_is_bc1(tex_b),
        "含非 255 alpha 像素的纹理必须回退 RGBA8（BC1 无 alpha 通道）"
    );
    let px_rgba = renderer_rgba.read_pixels();
    assert!(count_scene_pixels(&px_rgba, w, h) > 200);

    // ---- 等价性：全帧平均误差近零（块纯色棋盘格 BC1 精确解码）----
    let err = mean_abs_error(&px_bc, &px_rgba);
    assert!(
        err < 2.0,
        "BC1 路径与 RGBA 回退路径的全帧平均误差 {err:.3} 应 < 2/255 \
         （块内纯色棋盘格解码精确；无 BC 特性环境两侧同为 RGBA，本断言退化为恒等）"
    );
}
