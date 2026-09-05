// 运动模糊（Motion Blur，R43）：相机运动的帧间拖影——逐像素用当前帧
// 线性视距重建世界位，经上一帧相机矩阵重投影，屏幕 UV 差即该像素速度，
// 沿速度方向 N-tap 采样 HDR 平均。
//
// 链位：全分辨率 HDR → HDR，在景深之后、泛光之前（HDR 线性域，理由同
// DOF——拖影先在物理域混合，再进镜头合成段）；关闭时整段跳过，链与
// 无 MB 帧逐位一致。相机静止（含首帧：渲染器用当前矩阵补上一帧）时
// 速度恒 0，走 select 原色透传——启用但无运动，输出仍逐位等于无 MB 帧。
//
// 深度 → NDC z 的重建（DirectX 约定 z ∈ [0,1]，camera.rs 的投影约定）：
// 透视投影下 z_ndc = f/(f−n) − (n·f/(f−n))/d，其中 d = 线性视距
// （clip.w）。CPU 侧预compute f/(f−n) 与 n·f/(f−n) 写进参数，着色器只
// 做两次乘加；d = near → 0、d = far → 1（解析可验证）。
// 世界位 = inv_view_proj · vec4(ndc, z_ndc, 1)（透视除法还原），再经
// prev_view_proj 得上一帧 NDC，换算 UV（blit 的 V 翻转约定：
// uv = ndc·vec2(0.5, −0.5) + 0.5）后与当前 UV 相减 = 该像素速度。
//
// 方向采样：tap 数 n = clamp(|速度| × strength, 1, max_samples)，tap 沿
// 速度方向均匀铺满 [−0.5, +0.5]×速度（覆盖整段拖影）。实际循环恒跑
// MAX_TAPS 次、按 n 掩码加权（WGSL uniform 控制流要求——textureSample
// 不得进入非一致分支），n ≤ 1 时 select 原色透传保证静止路径逐位无损。
//
// 诚实边界：只覆盖**相机运动**（物体级速度缓冲是大工程，v1 限制入
// ARCHITECTURE §8）；屏幕外重投影点（上一帧不可见 / 相机大幅切换）
// 不做特殊处理，速度大时 tap 被 max_samples 与 UV 钳制兜住。

// 参数 uniform（144B）：当前帧逆视图投影 + 上一帧视图投影 + 标量。
// inv_view_proj / prev_view_proj 每帧 write_buffer（同一 submit 内对同一
// 缓冲只写一次，规避 write_buffer 合并问题——泛光的教训）。
struct MbParams {
    inv_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    // x: strength（速度→tap 数缩放）, y: max_samples（f32）,
    // z: f/(f−n), w: n·f/(f−n)（深度→NDC z 重建的两个预compute标量）
    scalars: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: MbParams;
@group(0) @binding(1) var src_texture: texture_2d<f32>;
@group(0) @binding(2) var src_sampler: sampler;
@group(0) @binding(3) var depth_texture: texture_2d<f32>;

struct MbVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// 三顶点全屏三角形（与 blit.wgsl 相同）
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> MbVertexOutput {
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
    var out: MbVertexOutput;
    out.position = vec4<f32>(positions[index], 0.0, 1.0);
    out.uv = uvs[index];
    return out;
}

// 方向采样的循环槽数（编译期常量；max_samples 只作掩码上限）
const MAX_TAPS: u32 = 32u;

@fragment
fn fs_main(in: MbVertexOutput) -> @location(0) vec4<f32> {
    let center = textureSample(src_texture, src_sampler, in.uv).rgb;
    let dims = vec2<f32>(textureDimensions(depth_texture));
    let px = clamp(
        vec2<i32>(in.uv * dims),
        vec2<i32>(0, 0),
        vec2<i32>(dims) - vec2<i32>(1, 1),
    );
    let d = textureLoad(depth_texture, px, 0).r;
    // 线性视距 → NDC z（DirectX [0,1] 深度约定，推导见模块头）
    let z = clamp(
        params.scalars.z - params.scalars.w / max(d, 1e-6),
        0.0,
        1.0,
    );
    // UV → NDC（V 翻转的逆变换），反投影到世界空间
    let ndc = vec2<f32>(in.uv.x * 2.0 - 1.0, 1.0 - 2.0 * in.uv.y);
    let world_h = params.inv_view_proj * vec4<f32>(ndc, z, 1.0);
    let world = world_h.xyz / world_h.w;
    // 上一帧重投影 → 屏幕 UV 速度
    let prev_clip = params.prev_view_proj * vec4<f32>(world, 1.0);
    let prev_uv = vec2<f32>(prev_clip.x, -prev_clip.y) / prev_clip.w * 0.5 + 0.5;
    let vel = in.uv - prev_uv;
    // tap 数：速度长度 × 缩放，钳制在 [1, max_samples]；重投影到相机
    // 背后的点（w ≤ 0，上一帧不可见）按无运动处理
    let n = clamp(
        length(vel) * params.scalars.x,
        1.0,
        min(params.scalars.y, f32(MAX_TAPS)),
    );
    let valid = select(0.0, 1.0, prev_clip.w > 0.0);
    // 掩码加权方向采样：n ≤ 1（含静止）时唯一激活 tap 的 t = 0（居中）
    var sum = vec3<f32>(0.0);
    var weight = 0.0;
    for (var i = 0u; i < MAX_TAPS; i++) {
        let w = select(0.0, 1.0, f32(i) < n) * valid;
        let t = (f32(i) - 0.5 * (n - 1.0)) / max(n - 1.0, 1.0);
        sum += w * textureSample(src_texture, src_sampler, in.uv + vel * t).rgb;
        weight += w;
    }
    let blurred = sum / max(weight, 1.0);
    return vec4<f32>(select(blurred, center, n <= 1.0 || valid < 0.5), 1.0);
}
