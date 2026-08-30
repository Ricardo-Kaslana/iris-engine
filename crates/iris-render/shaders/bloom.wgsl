// 泛光后处理：亮度提取（软阈值）+ 可分离高斯模糊。
// 半分辨率运行（HDR 中间缓冲的 1/2），水平 / 垂直两个 pass 共享 uniform。
// 泛光在 HDR 线性空间提取与模糊，合成在 blit pass（ACES 色调映射之前）完成。

struct BlurParams {
    // 亮度提取：x = 阈值（HDR 亮度），y = 软膝宽度；
    // 模糊：z = texel 尺寸 x，w = texel 尺寸 y
    a: vec4<f32>,
    // 模糊方向步长（方向单位向量 × texel 尺寸），提取 pass 不用
    b: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: BlurParams;
@group(0) @binding(1) var src_texture: texture_2d<f32>;
@group(0) @binding(2) var src_sampler: sampler;

struct BloomVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// 三顶点全屏三角形（与 blit.wgsl 相同）
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> BloomVertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(3.0, 1.0),
        vec2<f32>(-1.0, 1.0),
    );
    var uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 2.0),
        vec2<f32>(2.0, 0.0),
        vec2<f32>(0.0, 0.0),
    );
    var out: BloomVertexOutput;
    out.position = vec4<f32>(positions[index], 0.0, 1.0);
    out.uv = uvs[index];
    return out;
}

// 亮度提取：smoothstep 软阈值，阈值以下的像素贡献为 0（泛光只增不减）
@fragment
fn fs_threshold(in: BloomVertexOutput) -> @location(0) vec4<f32> {
    let c = textureSample(src_texture, src_sampler, in.uv).rgb;
    let luma = max(c.r, max(c.g, c.b));
    let t = params.a.x;
    let knee = max(params.a.y, 1e-4);
    let w = smoothstep(t, t + knee, luma);
    return vec4<f32>(c * w, 1.0);
}

// 可分离高斯模糊：9 tap 线性采样（5 个采样点，方向由 params.b 决定）
@fragment
fn fs_blur(in: BloomVertexOutput) -> @location(0) vec4<f32> {
    let o = params.b.xy;
    var c = textureSample(src_texture, src_sampler, in.uv).rgb * 0.2270270270;
    c = c + (textureSample(src_texture, src_sampler, in.uv + o * 1.3846153846).rgb
           + textureSample(src_texture, src_sampler, in.uv - o * 1.3846153846).rgb) * 0.3162162162;
    c = c + (textureSample(src_texture, src_sampler, in.uv + o * 3.2307692308).rgb
           + textureSample(src_texture, src_sampler, in.uv - o * 3.2307692308).rgb) * 0.0702702703;
    return vec4<f32>(c, 1.0);
}
