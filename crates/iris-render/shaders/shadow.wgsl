// 阴影深度 pass：从各级联光视角渲染场景深度到颜色附件（R32Float 的 R 通道）。
// 方向光 CSM（R29）：每级联一次渲染 pass，顶点读绑定的 64B 级联矩阵
// （CascadeParams，CPU 侧 cascade_fit 拟合后逐帧写入）计算裁剪位置；
// vs_point / vs_point_skinned / fs_point 为点光 cubemap 阴影入口：
// 只读每面 80B 小 uniform（PointFaceParams），与级联矩阵同绑定点但无
// 入口同时使用——WGSL 允许这种「同点不同变量」的声明。
// R29 起本 pass 不再绑定 576B 全局 Globals（唯一被读的 light_view_proj
// 已被每级联矩阵取代），Globals 布局见 standard.wgsl。
//
// 实现说明：不用 depth 附件 + 硬件比较采样，而是把深度写入颜色缓冲、
// 主 pass 手工比较——绕开 wgpu 30 / DX12 下 depth-only 管线深度写入
// 行为异常的问题（深度恒 0 的实证记录见 ARCHITECTURE.md §4.7）。

// 级联阴影的每级联参数（64 字节，POD）：本级联的光空间视图投影矩阵。
struct CascadeParams {
    view_proj: mat4x4<f32>,
};

// 点光 cubemap 阴影的每面参数（80 字节，POD）：
// vs_point 只读 view_proj；fs_point 另需灯位置与 range 计算归一化距离。
struct PointFaceParams {
    view_proj: mat4x4<f32>,
    // xyz: 灯世界位置, w: range（归一化距离的分母）
    light_pos_range: vec4<f32>,
};

@group(0) @binding(0) var<uniform> cascade: CascadeParams;
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

// 方向光级联阴影：裁剪位置 = 本级联光空间矩阵（组 0 binding 0 的
// CascadeParams，每个级联 pass 绑定各自的 64B 缓冲）。
@vertex
fn vs_main(in: VertexInput) -> ShadowVertexOutput {
    let model = mat4x4<f32>(in.model0, in.model1, in.model2, in.model3);
    let world_pos = model * vec4<f32>(in.position, 1.0);
    var out: ShadowVertexOutput;
    out.clip_position = cascade.view_proj * world_pos;
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
    out.clip_position = cascade.view_proj * world_pos;
    return out;
}

// 点光 cubemap 阴影：只读每面小 uniform（不触级联矩阵），裁剪位置来自
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

// ---- 毛发阴影（R91）：alpha 感知 cutout 的毛发深度投影 ----
//
// 毛发 VS 在独立 module（shadow_hair_vs.wgsl / shadow_shell_vs.wgsl——
// 光空间屏幕展宽 / 法线膨胀），本入口只做与主 pass fs_shaded **同一
// 判定语义**的 cutout：`base.a < max(材质 cutoff, 层比)` 丢弃——
// 毛发阴影带毛形孔洞、Mask 材质的实心影近似（§8.69③）一并闭合。
//
// 材质组声明在 **group(1)**（shadow_hair/shadow_pipeline 的布局 =
// [cascade, material]）——与蒙皮入口的 group(1) joint_mats 同点不同
// 变量（本文件 vs_point 头注释的同款合法形态：无入口同时使用两者）。
// MaterialParams 是 standard.wgsl 的**前缀影子契约**：standard 的完整
// 结构在本轮扩到 112 B（末尾追加 extensions），阴影只声明并使用前 96 B
// 的 fur 与 fur_tint 字段。材质绑定组布局的 min_binding_size 为 None，
// 112 B 缓冲绑定到 96 B 前缀声明合法；改前缀字段必须两侧同步（看门：
// pipeline.rs 的字段存在性检查与 renderer.rs 的布局 const 断言）。

struct MaterialParams {
    base_color: vec4<f32>,
    emissive: vec4<f32>,
    surface: vec4<f32>,
    sss: vec4<f32>,
    fur: vec4<f32>,
    fur_tint: vec4<f32>,
};

@group(1) @binding(0) var<uniform> shadow_material: MaterialParams;
@group(1) @binding(1) var shadow_base_texture: texture_2d<f32>;
@group(1) @binding(2) var shadow_base_sampler: sampler;

// 毛发阴影 VS 的输出（shadow_hair_vs / shadow_shell_vs 的接口契约，
// 二方同步）
struct ShadowFurVertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) fur_aux: vec4<f32>,
};

@fragment
fn fs_fur(in: ShadowFurVertexOutput) -> @location(0) vec4<f32> {
    let base = shadow_material.base_color
        * textureSample(shadow_base_texture, shadow_base_sampler, in.uv);
    if (base.a < max(shadow_material.fur.y, in.fur_aux.x)) {
        discard;
    }
    let d = in.clip_position.z / in.clip_position.w;
    return vec4<f32>(d, 0.0, 0.0, 1.0);
}
