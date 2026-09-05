// 主相机场景深度副本 pass（R43）：从相机视角把**线性视距**写入 R32Float
// 颜色附件的 R 通道，供景深（DOF）与运动模糊（MB）采样。
//
// 为什么需要这个 pass：主 pass 的深度附件是外部传入的 Depth32Float
// MSAA 纹理（仅 RENDER_ATTACHMENT，不可绑采样、不可解析），DX12 下
// depth-only 采样又有写入异常的既有教训（ARCHITECTURE §4.7）——因此
// 照搬阴影 pass 的「深度写颜色 R 通道 + Depth32Float 双附件」方案：
// R32Float 单采样颜色附件存线性视距（TEXTURE_BINDING 供 textureLoad），
// 自带 Depth32Float 单采样附件（Less 测试）保证每像素存最近表面。
//
// 线性视距 = 裁剪 w：DirectX 深度约定（z ∈ [0,1]，见 camera.rs）下
// 透视投影的 clip.w = −view.z = 沿视线方向的距离。varying 插值是
// 透视校正的（对 1/w 线性），而逐像素真实视距恰满足 1/d 在屏幕空间
// 线性——所以直接插值 clip.w 得到的正是逐像素精确视距，无需片元重算。
// 天空 / 无几何像素清空为远平面距离（Clear 值由渲染器按 z_far 写入）。
//
// 与阴影 pass 同构的取舍：不透明 + 蒙皮实例重画一遍（透明 / 贴花 /
// 粒子不进深度副本——半透明表面给 DOF / MB 提供的深度本就无定义）；
// 蒙皮用与 vs_skinned 相同的 LBS 混合，保证深度随骨骼变形与主 pass
// 几何逐顶点一致。仅 DOF / MB 启用时才编码（零开销默认路径）。

// 64B uniform（与 CascadeParams 同布局）：主相机视图投影矩阵。
struct CameraParams {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: CameraParams;
// 蒙皮关节矩阵（vs_skinned 专用；vs_main 静态不使用）
@group(1) @binding(0) var<storage, read> joint_mats: array<mat4x4<f32>>;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(3) model0: vec4<f32>,
    @location(4) model1: vec4<f32>,
    @location(5) model2: vec4<f32>,
    @location(6) model3: vec4<f32>,
};

struct SkinnedDepthVertexInput {
    @location(0) position: vec3<f32>,
    @location(3) model0: vec4<f32>,
    @location(4) model1: vec4<f32>,
    @location(5) model2: vec4<f32>,
    @location(6) model3: vec4<f32>,
    @location(10) joints: vec4<u32>,
    @location(11) weights: vec4<f32>,
};

struct DepthVertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    // 线性视距（clip.w，见模块头注释的插值推导）
    @location(0) view_depth: f32,
};

@vertex
fn vs_main(in: VertexInput) -> DepthVertexOutput {
    let model = mat4x4<f32>(in.model0, in.model1, in.model2, in.model3);
    let world_pos = model * vec4<f32>(in.position, 1.0);
    let clip = camera.view_proj * world_pos;
    var out: DepthVertexOutput;
    out.clip_position = clip;
    out.view_depth = clip.w;
    return out;
}

// 蒙皮变体：与 shadow.wgsl vs_skinned / standard.wgsl 相同的 LBS 混合
// （先 skin 后 model）。
@vertex
fn vs_skinned(in: SkinnedDepthVertexInput) -> DepthVertexOutput {
    let j = in.joints;
    let w = in.weights;
    let skin_mat = joint_mats[j.x] * w.x + joint_mats[j.y] * w.y
                 + joint_mats[j.z] * w.z + joint_mats[j.w] * w.w;
    let model = mat4x4<f32>(in.model0, in.model1, in.model2, in.model3);
    let world_pos = model * (skin_mat * vec4<f32>(in.position, 1.0));
    let clip = camera.view_proj * world_pos;
    var out: DepthVertexOutput;
    out.clip_position = clip;
    out.view_depth = clip.w;
    return out;
}

@fragment
fn fs_main(in: DepthVertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(in.view_depth, 0.0, 0.0, 1.0);
}
