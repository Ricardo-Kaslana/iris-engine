// 阴影深度 pass：从光源视角渲染场景深度（仅顶点变换，无片段输出）。
// 与 standard.wgsl 共享同一 Globals uniform 布局（本 pass 只读 light_view_proj）。

struct Globals {
    view_proj: mat4x4<f32>,
    eye_pos: vec4<f32>,
    light_dir: vec4<f32>,
    light_color: vec4<f32>,
    ambient: vec4<f32>,
    point_count: vec4<f32>,
    point_lights: array<vec4<f32>, 16>,
    light_view_proj: mat4x4<f32>,
    shadow_params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(3) model0: vec4<f32>,
    @location(4) model1: vec4<f32>,
    @location(5) model2: vec4<f32>,
    @location(6) model3: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> @builtin(position) vec4<f32> {
    let model = mat4x4<f32>(in.model0, in.model1, in.model2, in.model3);
    let world_pos = model * vec4<f32>(in.position, 1.0);
    return globals.light_view_proj * world_pos;
}
