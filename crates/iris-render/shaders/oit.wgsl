// C7b · Weighted Blended OIT 合成 pass（McGuire & Bavoil, HPG 2013）。
// 全屏三角形：读累积缓冲 A（RGBA16F，Σ color·α·w 与 Σ α·w）、遮蔽度纹理 V
// （R16F，Π(1−α)，经 (ZERO, ONE_MINUS_SRC_COLOR) 混合连乘）、已解析背景
// （贴花后的 HDR 中心解析值），输出逐像素合成结果写回 HDR MSAA 附件。
//
// 合成式（单层兼容锚：一张 α 面片时逐位退化到经典 src-alpha 混合，
// 见 renderer/tests 的解析测试）：
//   avg  = A.rgb / max(A.a, 1e-5)
//   out  = avg · (1 − V) + bg · V
// 无透明的像素 V = 1、A = 0 ⇒ out = bg 精确恒等（改动面护栏的代数根基）。
//
// 取证声明：权重与混合态照抄设计稿第 5 节存档的原文（禁凭记忆纪律）；
// WGSL 侧权重式的 CPU 镜像在 `iris_render::oit`（`oit_weight` /
// `oit_resolve_color`，看门 = 双层代数解析单测 + 单层逐位锚）。

@group(0) @binding(0) var accum_tex: texture_2d<f32>;
@group(0) @binding(1) var reveal_tex: texture_2d<f32>;
// 背景 = HDR 解析视图的**帧内拷贝**（resolve pass 同时刻把 msaa 解析写回
// 该视图，同 pass 读写同纹理会撞 wgpu hazard 检查 ⇒ 拷贝进专属 bg 缓冲，
// 1:1 对齐 ⇒ 三路都 textureLoad，零采样器、零过滤语义）
@group(0) @binding(2) var bg_tex: texture_2d<f32>;

struct OitVertexOutput {
    @builtin(position) position: vec4<f32>,
};

// 三顶点全屏三角形（与 blit.wgsl 相同；MSAA 目标下每个覆盖样元同值）。
// 输出只带像素坐标：三路输入都是 1:1 对齐的单采样纹理，textureLoad
// 精确取纹素，无 uv、无采样器。
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> OitVertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(3.0, 1.0),
        vec2<f32>(-1.0, 1.0),
    );
    var out: OitVertexOutput;
    out.position = vec4<f32>(positions[index], 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: OitVertexOutput) -> @location(0) vec4<f32> {
    // A / V 是单采样纹理，按像素坐标精确读取（MSAA 光栅化下各样元同值）
    let px = vec2<i32>(in.position.xy);
    let accum = textureLoad(accum_tex, px, 0);
    let reveal = textureLoad(reveal_tex, px, 0).r;
    // 背景 = 贴花后的 HDR 解析中心值的帧内拷贝（透明覆盖区背景的子采样
    // 细节退化 = 已声明边界 §8.68③）
    let bg = textureLoad(bg_tex, px, 0).rgb;
    let avg = accum.rgb / max(accum.a, 1e-5);
    return vec4<f32>(avg * (1.0 - reveal) + bg * reveal, 1.0);
}
