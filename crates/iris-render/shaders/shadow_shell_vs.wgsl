// 壳层阴影顶点着色器（R91）：层比膨胀 + 级联光空间投影。
//
// 与 shell_vs.wgsl 同一膨胀式（world_normal × height × t），矩阵换成
// CascadeParams。fur_aux 传层比——fs_fur 的逐层 cutout 让壳层阴影带
// 毛形孔洞（密度贴图语义与主 pass 一致：值 d 处毛长 = d × height）。
//
// 顶点 / 实例布局与壳层主管线一致（shell_vs.wgsl 的 VertexInput——
// 影子契约三方同步）。

struct CascadeParams {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> cascade: CascadeParams;

struct ShadowFurVertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) fur_aux: vec4<f32>,
};

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) model0: vec4<f32>,
    @location(4) model1: vec4<f32>,
    @location(5) model2: vec4<f32>,
    @location(6) model3: vec4<f32>,
    @location(7) normal0: vec4<f32>,
    @location(8) normal1: vec4<f32>,
    @location(9) normal2: vec4<f32>,
    @location(15) shell: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> ShadowFurVertexOutput {
    let model = mat4x4<f32>(in.model0, in.model1, in.model2, in.model3);
    let normal_mat = mat3x3<f32>(in.normal0.xyz, in.normal1.xyz, in.normal2.xyz);
    let world_normal = normalize(normal_mat * in.normal);
    let inflated = model * vec4<f32>(in.position, 1.0)
        + vec4<f32>(world_normal * (in.shell.y * in.shell.x), 0.0);
    var out: ShadowFurVertexOutput;
    out.clip_position = cascade.view_proj * inflated;
    out.uv = in.uv;
    out.fur_aux = vec4<f32>(in.shell.x, 0.0, 0.0, 0.0);
    return out;
}
