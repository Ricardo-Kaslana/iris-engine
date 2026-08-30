// HDR → sRGB 色调映射 blit：全屏三角形，采样线性 HDR 中间缓冲，
// 叠加泛光后做 ACES 近似色调映射，输出到 sRGB 目标（硬件完成最终 gamma 编码）。

@group(0) @binding(0) var hdr_texture: texture_2d<f32>;
@group(0) @binding(1) var hdr_sampler: sampler;
// 泛光（半分辨率模糊结果，HDR 线性空间）；关闭时 strength uniform 为 0
@group(0) @binding(2) var bloom_texture: texture_2d<f32>;
// x: 泛光叠加强度
@group(0) @binding(3) var<uniform> blit_params: vec4<f32>;

struct BlitVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// 三顶点全屏三角形：越出屏幕的部分被裁剪，无需索引缓冲
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> BlitVertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(3.0, 1.0),
        vec2<f32>(-1.0, 1.0),
    );
    // 屏幕位置 -> UV（翻转 V：NDC 上正 = 纹理上端）
    var uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 2.0),
        vec2<f32>(2.0, 0.0),
        vec2<f32>(0.0, 0.0),
    );
    var out: BlitVertexOutput;
    out.position = vec4<f32>(positions[index], 0.0, 1.0);
    out.uv = uvs[index];
    return out;
}

// ACES 色调映射（Narkowicz 解析近似）
fn aces_tonemap(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs_main(in: BlitVertexOutput) -> @location(0) vec4<f32> {
    var hdr = textureSample(hdr_texture, hdr_sampler, in.uv).rgb;
    // 泛光叠加（HDR 线性空间）后做 ACES 色调映射（输出线性色，sRGB 编码由目标格式完成）
    hdr = hdr + textureSample(bloom_texture, hdr_sampler, in.uv).rgb * blit_params.x;
    let exposed = hdr * 1.0;
    return vec4<f32>(aces_tonemap(exposed), 1.0);
}
