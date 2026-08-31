// 2D UI 叠加层：屏幕空间彩色矩形与位图字体文本。
//
// 顶点为像素坐标（原点左上、y 向下），顶点着色器统一映射到 NDC；
// 片段 = 图集采样 × 顶点色（矩形采样纯白块、文本逐亮位采样字形 texel），
// alpha 混合由管线状态（ALPHA_BLENDING）完成。UI pass 写在色调映射
// blit 之后的 sRGB 目标上——颜色**不经** HDR / 泛光 / ACES，
// 保证 HUD 是干净的界面叠加。

@group(0) @binding(0) var ui_atlas: texture_2d<f32>;
@group(0) @binding(1) var ui_sampler: sampler;
// 屏幕尺寸（像素）：xy = (宽, 高)；zw 恒 0（uniform 结构体需 16 字节对齐的填充）
@group(0) @binding(2) var<uniform> ui_globals: vec4<f32>;

struct UiVertexInput {
    @location(0) pos: vec2<f32>,   // 像素坐标
    @location(1) uv: vec2<f32>,    // 图集 UV
    @location(2) color: vec4<f32>, // 线性 RGBA
}

struct UiVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
}

@vertex
fn vs_main(in: UiVertexInput) -> UiVertexOutput {
    // 像素 → NDC：pos / screen 归一到 0..1 后映射到 -1..1；
    // y 乘 -2 并加 1 完成「左上原点、y 向下」到「NDC y 向上」的翻转
    let ndc = in.pos / ui_globals.xy * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);
    var out: UiVertexOutput;
    out.position = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: UiVertexOutput) -> @location(0) vec4<f32> {
    // tex.rgb × 顶点色、tex.a × 顶点 a：字形白色 + alpha=位码与顶点色相乘，
    // 矩形（白块 alpha=1）则原样输出顶点色；混合由管线 ALPHA_BLENDING 完成
    return textureSample(ui_atlas, ui_sampler, in.uv) * in.color;
}
