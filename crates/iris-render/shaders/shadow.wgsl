// 阴影深度 pass：从光源视角渲染场景深度到颜色附件（Rgba32Float 的 R 通道）。
// 与 standard.wgsl 共享同一 Globals uniform 布局（本 pass 只读 light_view_proj）。
//
// 实现说明：不用 depth 附件 + 硬件比较采样，而是把深度写入颜色缓冲、
// 主 pass 手工比较——绕开 wgpu 30 / DX12 下 depth-only 管线深度写入
// 行为异常的问题（深度恒 0 的实证记录见 ARCHITECTURE.md §4.7）。

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
    env_params: vec4<f32>, // 与 standard.wgsl 保持同布局（本 pass 不读）
};

@group(0) @binding(0) var<uniform> globals: Globals;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(3) model0: vec4<f32>,
    @location(4) model1: vec4<f32>,
    @location(5) model2: vec4<f32>,
    @location(6) model3: vec4<f32>,
};

struct ShadowVertexOutput {
    @builtin(position) clip_position: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> ShadowVertexOutput {
    let model = mat4x4<f32>(in.model0, in.model1, in.model2, in.model3);
    let world_pos = model * vec4<f32>(in.position, 1.0);
    var out: ShadowVertexOutput;
    out.clip_position = globals.light_view_proj * world_pos;
    return out;
}

@fragment
fn fs_main(in: ShadowVertexOutput) -> @location(0) vec4<f32> {
    // 正交投影下 w = 1；透视投影时此除法同样正确。深度存 R 通道。
    let d = in.clip_position.z / in.clip_position.w;
    return vec4<f32>(d, 0.0, 0.0, 1.0);
}
