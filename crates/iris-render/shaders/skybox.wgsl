// 天空盒背景：全屏三角形（深度只读），按像素视线方向采样预滤波环境
// 立方体贴图的 0 级 mip——粗糙度 0 的预滤波结果与原始天空逐像素一致，
// 无需为背景单独绑定天空贴图。在主 pass 内最先绘制，几何体随后覆盖。
//
// 射线重建：像素 NDC（z = 远平面）经 inv_view_proj 变换回世界空间，
// 方向 = 远平面点 - 相机位置。

// **并行 Globals 布局（易错点）**：本结构体是 standard.wgsl 声明的前缀，
// 逐字段偏移必须与那边保持一致——天空盒只读 inv_view_proj，但**数组型
// 成员的长度会平移后面所有字段**：PointLight 自 R61 起为 3×vec4（多带
// 逐灯阴影参数），故这里是 8 × 3 = 24 个 vec4 平铺。漏改的症状是天空
// 整体错色（读到的 inv_view_proj 落在别的字段上），R61 实测踩过一次，
// `pipeline.rs::skybox_globals_prefix_matches_standard_layout` 看门。
struct Globals {
    view_proj: mat4x4<f32>,
    eye_pos: vec4<f32>,
    light_dir: vec4<f32>,
    light_color: vec4<f32>,
    ambient: vec4<f32>,
    point_count: vec4<f32>,
    point_lights: array<vec4<f32>, 24>, // 8 个 PointLight（3×vec4）平铺，保持与 standard.wgsl 同布局
    // R29 起方向光阴影为 CSM（每级联矩阵在独立 uniform，binding 8），
    // 单一 light_view_proj 字段已从全局缓冲移除，布局与 Rust GlobalsRaw 对齐。
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
