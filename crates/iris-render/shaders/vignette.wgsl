// 暗角（vignette）后处理：镜头物理遮挡的近似——画面四角按 UV 到中心
// 的距离 smoothstep 压暗，中心区域不受影响。
//
// 全分辨率线性 HDR → HDR 的单输入 pass，链位在泛光之后、blit 色调映射
// 之前：tonemap/blit 是链的固定终点，暗角在线性 HDR 域作用（压暗的是
// 辐射本身，随后才被曝光放大、被 ACES 滚降）。强度 0 时渲染器整段跳过
// 本 pass（零开销默认路径）。

// 参数复用 PostParamsRaw 布局（16B）：z = 暗角强度；
// x/y/w 归 blit 合成语义（泛光 / 曝光 / 保留），本 pass 不消费
@group(0) @binding(0) var<uniform> params: vec4<f32>;
@group(0) @binding(1) var src_texture: texture_2d<f32>;
@group(0) @binding(2) var src_sampler: sampler;

struct VignetteVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// 三顶点全屏三角形（与 blit.wgsl 相同）
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VignetteVertexOutput {
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
    var out: VignetteVertexOutput;
    out.position = vec4<f32>(positions[index], 0.0, 1.0);
    out.uv = uvs[index];
    return out;
}

@fragment
fn fs_main(in: VignetteVertexOutput) -> @location(0) vec4<f32> {
    let hdr = textureSample(src_texture, src_sampler, in.uv).rgb;
    // UV 空间到中心的距离（0..0.707，中心 0 / 边中点 0.5 / 角点 0.707）。
    // PostParamsRaw 布局无宽高比字段，各向异性让暗角沿屏幕自适应成
    // 椭圆——廉价暗角的通行做法。
    let d = distance(in.uv, vec2<f32>(0.5, 0.5));
    // smoothstep 在 d ≤ 0.35 时恒 0（中心完全无损）、d ≥ 0.75 时恒 1
    // （角点压到 (1 − strength) 倍），之间平滑过渡
    let falloff = smoothstep(0.35, 0.75, d);
    let darken = 1.0 - params.z * falloff;
    return vec4<f32>(hdr * darken, 1.0);
}
