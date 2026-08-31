// 天空盒背景：全屏三角形（深度只读），按像素视线方向采样预滤波环境
// 立方体贴图的 0 级 mip——粗糙度 0 的预滤波结果与原始天空逐像素一致，
// 无需为背景单独绑定天空贴图。在主 pass 内最先绘制，几何体随后覆盖。
//
// 射线重建：像素 NDC（z = 远平面）经 inv_view_proj 变换回世界空间，
// 方向 = 远平面点 - 相机位置。

struct Globals {
    view_proj: mat4x4<f32>,
    eye_pos: vec4<f32>,
    light_dir: vec4<f32>,
    light_color: vec4<f32>,
    ambient: vec4<f32>,
    point_count: vec4<f32>,
    point_lights: array<vec4<f32>, 16>, // 8 个 PointLight（2×vec4）平铺，保持与 standard.wgsl 同布局
    light_view_proj: mat4x4<f32>,
    shadow_params: vec4<f32>,
    env_params: vec4<f32>,
    inv_view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(4) var env_map: texture_cube<f32>;
@group(0) @binding(6) var env_sampler: sampler;

struct SkyVertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

// 三顶点全屏三角形；clip z = 1（远平面）
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> SkyVertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(3.0, 1.0),
        vec2<f32>(-1.0, 1.0),
    );
    var out: SkyVertexOutput;
    let p = positions[index];
    out.clip_position = vec4<f32>(p, 1.0, 1.0);
    out.ndc = p;
    return out;
}

@fragment
fn fs_main(in: SkyVertexOutput) -> @location(0) vec4<f32> {
    let far_point = globals.inv_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let dir = normalize(far_point.xyz / far_point.w - globals.eye_pos.xyz);
    return vec4<f32>(textureSampleLevel(env_map, env_sampler, dir, 0.0).rgb, 1.0);
}
