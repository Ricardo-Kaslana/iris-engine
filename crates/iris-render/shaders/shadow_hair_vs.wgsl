// 发带阴影顶点着色器（R91）：光空间（级联矩阵）相机面向屏幕展宽。
//
// 与 hair_vs.wgsl 同一展宽公式（已被 R90 GPU 测试验证），矩阵换成
// 阴影 pass 的 CascadeParams（组 0 binding 0 的 64 B uniform——与
// shadow.wgsl vs_main 同源）——发带在阴影贴图里以**光视角**的屏幕
// 法向展宽（否则阴影里的发带是零宽线）。伪法线 / Kajiya-Kay 不进
// 阴影（深度投影不需要着色）。
//
// 投影常量从 cascade 矩阵列长推导（同 hair_vs 的 view_proj 推导：
// 光空间正交投影的对角元 = 列长）。顶点布局 / 实例布局与发带主管线
// 完全一致（hair_vs.wgsl 的 VertexInput——影子契约三方同步）。
//
// 输出 ShadowFurVertexOutput（shadow.wgsl fs_fur 的接口契约）：
// uv = (沿丝 t, side) 供 base 采样、fur_aux 恒 0（发带无层比）。

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
    @location(1) dir_width: vec4<f32>,
    @location(2) t_side: vec4<f32>,
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
    let world_root = model * vec4<f32>(in.position, 1.0);
    let t_world = normalize(normal_mat * in.dir_width.xyz);

    let proj_y = length(cascade.view_proj[1].xyz);
    let proj_x = length(cascade.view_proj[0].xyz);
    let aspect = proj_y / max(proj_x, 1e-6);

    let clip_root = cascade.view_proj * world_root;
    let clip_tip = cascade.view_proj
        * vec4<f32>(world_root.xyz + t_world, 1.0);
    var d2 = vec2<f32>(
        (clip_tip.x / clip_tip.w - clip_root.x / clip_root.w) * aspect,
        clip_tip.y / clip_tip.w - clip_root.y / clip_root.w,
    );
    let len2 = length(d2);
    d2 = d2 / max(len2, 1e-8);
    let perp = vec2<f32>(-d2.y, d2.x);
    let perp_ndc = vec2<f32>(perp.x / aspect, perp.y);
    let half_ndc = in.dir_width.w * 0.5 * proj_y / max(clip_root.w, 1e-6);
    let side = in.t_side.y;
    let ndc = clip_root.xy / max(clip_root.w, 1e-6) + perp_ndc * (side * half_ndc);

    var out: ShadowFurVertexOutput;
    out.clip_position = vec4<f32>(ndc * clip_root.w, clip_root.z, clip_root.w);
    out.uv = vec2<f32>(in.t_side.x, in.t_side.y);
    out.fur_aux = vec4<f32>(0.0);
    return out;
}
