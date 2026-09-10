//! 工具：R59 资产管线验收资产生成器（`r59_snapshot` 示例的输入）。
//!
//! - `assets/r59_actor.glb`：路牌式「立柱 + 摇摆板」双节点资产——
//!   材质经 [`GltfBuilder::material_textured`] 内嵌**带 alpha 通道的
//!   baseColor 贴图**（8×8 半透明棋盘 + 全不透明白格）与法线贴图
//!   （`set_alpha_blend` 标 BLEND，透明 pass 消费纹理 alpha——BC3
//!   压缩通路的验收载体）；三条**命名动画** idle / swing / hit；
//! - `assets/r59_manifest.json`：资产清单——actor（priority 5 +
//!   `bc3: true`）与 support.bin（priority 0，用于演示优先级出队）。
//!
//! ```sh
//! cargo run --example gen_r59_assets --release   # 在仓库根目录执行
//! ```

use iris_engine::gltf_builder::{AnimChannel, GltfBuilder, GltfMaterialTextures, GltfNodeDesc};
use iris_render::animation::{AnimationInterpolation, AnimationPath};
use iris_render::mesh::MeshData;
use iris_render::texture::CpuTexture;

fn main() {
    std::fs::write("assets/r59_actor.glb", actor_glb()).expect("写 assets/r59_actor.glb");
    std::fs::write("assets/r59_support.bin", SUPPORT).expect("写 assets/r59_support.bin");
    std::fs::write("assets/r59_manifest.json", MANIFEST).expect("写 assets/r59_manifest.json");
    println!("已生成 assets/r59_actor.glb、assets/r59_support.bin 与 assets/r59_manifest.json");
}

/// 支撑资产（演示优先级出队用：priority 0 先于 actor 的 5）。
const SUPPORT: &[u8] = b"r59 support blob";

/// 清单：support 先于 actor 加载（priority 0 < 5）；actor 带 bc3 标志
/// （其内嵌纹理入库时声明 BC3 压缩上传）。
const MANIFEST: &str = r#"{
    "assets": [
        { "path": "r59_support.bin", "kind": "raw", "priority": 0 },
        { "path": "r59_actor.glb", "kind": "raw", "priority": 5, "bc3": true }
    ]
}"#;

/// 带 alpha 的 baseColor 贴图：8×8 棋盘，半透明深蓝格（alpha 190）与
/// 不透明白格交错——块内 alpha 纯色（BC4 端点精确），BC3 压缩后视觉
/// 无可见劣化的同时显存 4 B/px → 1 B/px。
fn alpha_base_texture() -> CpuTexture {
    let mut tex = CpuTexture::checkerboard(2, 4, [5, 50, 190, 190], [245, 245, 245, 255]);
    for (i, px) in tex.rgba.chunks_exact_mut(4).enumerate() {
        if i % 2 == 1 {
            px[3] = 190;
        }
    }
    tex
}

/// 构造 r59_actor.glb（节点树 + 三条命名动画，见文件头注释）。
fn actor_glb() -> Vec<u8> {
    let base = alpha_base_texture();
    // 法线贴图：程序化砖纹对取其法线半张（线性数据贴图；尺寸 8×8 与
    // base 一致，无 alpha——声明 BC3 也按分层规则归 BC1）
    let (normal, _) = CpuTexture::brick_normal_orm(2, 2, 8, 0.2, 1.0);

    let mut builder = GltfBuilder::new();
    let material = builder.material_textured(
        [1.0, 1.0, 1.0, 1.0],
        0.0,
        0.7,
        None,
        GltfMaterialTextures {
            base_color: Some(base),
            normal: Some(normal),
            orm: None,
        },
    );
    builder.set_alpha_blend(material);

    let post_mesh = builder.mesh("Post", &MeshData::cube(), material);
    let board_mesh = builder.mesh("Board", &MeshData::cube(), material);
    let root = builder.node(GltfNodeDesc::new("R59Root"));
    let post = builder.node(
        GltfNodeDesc::new("Post")
            .with_mesh(post_mesh)
            .at([0.0, 0.5, 0.0])
            .scaled([0.15, 1.0, 0.15]),
    );
    let board = builder.node(
        GltfNodeDesc::new("Board")
            .with_mesh(board_mesh)
            .at([0.0, 1.2, 0.0])
            .scaled([0.9, 0.5, 0.08]),
    );
    builder.add_child(root, post);
    builder.add_child(root, board);

    let yaw = |deg: f32| {
        let (s, c) = (deg * 0.5).to_radians().sin_cos();
        [0.0f32, s, 0.0, c]
    };
    // idle：板上呼吸 bob（1 s 循环，板在 y 1.2 → 1.5 → 1.2）
    builder.animation(
        "idle",
        &[AnimChannel {
            node: board,
            path: AnimationPath::Translation,
            interpolation: AnimationInterpolation::Linear,
            times: vec![0.0, 0.5, 1.0],
            values: vec![0.0, 1.2, 0.0, 0.0, 1.5, 0.0, 0.0, 1.2, 0.0],
        }],
    );
    // swing：yaw ±40° 摆动（1.2 s 循环）
    builder.animation(
        "swing",
        &[AnimChannel {
            node: board,
            path: AnimationPath::Rotation,
            interpolation: AnimationInterpolation::Linear,
            times: vec![0.0, 0.6, 1.2],
            values: [40.0f32, -40.0, 40.0]
                .iter()
                .flat_map(|&deg| yaw(deg))
                .collect(),
        }],
    );
    // hit：快速 jab（yaw 0 → 60° → 0，0.5 s 非循环观感）
    builder.animation(
        "hit",
        &[AnimChannel {
            node: board,
            path: AnimationPath::Rotation,
            interpolation: AnimationInterpolation::Linear,
            times: vec![0.0, 0.25, 0.5],
            values: [0.0f32, 60.0, 0.0]
                .iter()
                .flat_map(|&deg| yaw(deg))
                .collect(),
        }],
    );
    builder.build_glb()
}
