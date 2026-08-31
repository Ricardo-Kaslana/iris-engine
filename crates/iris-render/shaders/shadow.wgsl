// 阴影深度 pass：从光源视角渲染场景深度到颜色附件（Rgba32Float 的 R 通道）。
// 与 standard.wgsl 共享同一 Globals uniform 布局（本 pass 只读 light_view_proj）。
// vs_point / vs_point_skinned / fs_point 为点光 cubemap 阴影入口：
// 只读每面 80B 小 uniform（PointFaceParams），与 Globals 同绑定点但无入口同时使用。
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
    inv_view_proj: mat4x4<f32>, // 与 standard.wgsl 保持同布局（本 pass 不读）
    point_shadow_params: vec4<f32>, // 与 standard.wgsl 保持同布局（本 pass 不读）
};

// 点光 cubemap 阴影的每面参数（80 字节，POD）：
// vs_point 只读 view_proj；fs_point 另需灯位置与 range 计算归一化距离。
// 与上面的 Globals 共用绑定点 (group 0, binding 0)，但没有任何入口
// 同时静态使用两者——WGSL 允许这种「同点不同变量」的声明。
struct PointFaceParams {
    view_proj: mat4x4<f32>,
    // xyz: 灯世界位置, w: range（归一化距离的分母）
    light_pos_range: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(0) var<uniform> point_face: PointFaceParams;
// 蒙皮关节矩阵（vs_skinned 专用；vs_main 静态不使用）
@group(1) @binding(0) var<storage, read> joint_mats: array<mat4x4<f32>>;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(3) model0: vec4<f32>,
    @location(4) model1: vec4<f32>,
    @location(5) model2: vec4<f32>,
    @location(6) model3: vec4<f32>,
};

struct SkinnedShadowVertexInput {
    @location(0) position: vec3<f32>,
    @location(3) model0: vec4<f32>,
    @location(4) model1: vec4<f32>,
    @location(5) model2: vec4<f32>,
    @location(6) model3: vec4<f32>,
    @location(10) joints: vec4<u32>,
    @location(11) weights: vec4<f32>,
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

// 蒙皮几何体的阴影：与 standard.wgsl vs_skinned 相同的 LBS 混合
// （先 skin 后 model），保证阴影与主 pass 几何逐顶点一致。
@vertex
fn vs_skinned(in: SkinnedShadowVertexInput) -> ShadowVertexOutput {
    let j = in.joints;
    let w = in.weights;
    let skin_mat = joint_mats[j.x] * w.x + joint_mats[j.y] * w.y
                 + joint_mats[j.z] * w.z + joint_mats[j.w] * w.w;
    let model = mat4x4<f32>(in.model0, in.model1, in.model2, in.model3);
    let world_pos = model * (skin_mat * vec4<f32>(in.position, 1.0));
    var out: ShadowVertexOutput;
    out.clip_position = globals.light_view_proj * world_pos;
    return out;
}

// 点光 cubemap 阴影：只读每面小 uniform（不触 Globals），裁剪位置来自
// 面视图投影矩阵；世界坐标交给 fs_point 计算归一化线性距离。
struct PointShadowVertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
};

@vertex
fn vs_point(in: VertexInput) -> PointShadowVertexOutput {
    let model = mat4x4<f32>(in.model0, in.model1, in.model2, in.model3);
    let world_pos = model * vec4<f32>(in.position, 1.0);
    var out: PointShadowVertexOutput;
    out.clip_position = point_face.view_proj * world_pos;
    out.world_pos = world_pos.xyz;
    return out;
}

// 点光阴影蒙皮变体：与 vs_skinned 相同的 LBS 混合，配合 vs_point 的输出。
@vertex
fn vs_point_skinned(in: SkinnedShadowVertexInput) -> PointShadowVertexOutput {
    let j = in.joints;
    let w = in.weights;
    let skin_mat = joint_mats[j.x] * w.x + joint_mats[j.y] * w.y
                 + joint_mats[j.z] * w.z + joint_mats[j.w] * w.w;
    let model = mat4x4<f32>(in.model0, in.model1, in.model2, in.model3);
    let world_pos = model * (skin_mat * vec4<f32>(in.position, 1.0));
    var out: PointShadowVertexOutput;
    out.clip_position = point_face.view_proj * world_pos;
    out.world_pos = world_pos.xyz;
    return out;
}

@fragment
fn fs_main(in: ShadowVertexOutput) -> @location(0) vec4<f32> {
    // 正交投影下 w = 1；透视投影时此除法同样正确。深度存 R 通道。
    let d = in.clip_position.z / in.clip_position.w;
    return vec4<f32>(d, 0.0, 0.0, 1.0);
}

// 点光面片段：存「归一化线性距离」length(world − light) / range 而非光空间
// z/w——采样侧只需 normalize(world_pos − light_pos) 方向查 Cube texel，
// 无需任何光空间矩阵；深度附件（Less）单独用裁剪 z 保证每 texel 存最近面。
// 清空值 1.0（= range/range）代表无遮挡。
@fragment
fn fs_point(in: PointShadowVertexOutput) -> @location(0) vec4<f32> {
    let d = length(in.world_pos - point_face.light_pos_range.xyz);
    return vec4<f32>(d / point_face.light_pos_range.w, 0.0, 0.0, 1.0);
}
