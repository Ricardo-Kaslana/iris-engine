// TAA 时域抗锯齿（R60）：链首 HDR 域时域累加——当前帧抖动色 + 主相机
// 深度副本 + 上帧历史 → 相机重投影 → 3×3 邻域钳制 → 混合写入本帧历史槽
// （下游链以 PostInput::TaaHistory 直读本帧输出，ARCHITECTURE §4.29）。
//
// 重投影与运动模糊（motion_blur.wgsl）复用同一份数学：线性视距 →
// NDC z 解析重建（DirectX [0,1] 约定，z = f/(f−n) − (n·f/(f−n))/d，CPU
// 预计算两系数）→ 本帧 inv_view_proj 反投影世界位 → 上帧 view_proj 重
// 投影 → 历史 UV。**只覆盖相机运动**（无逐物体运动向量，§8.36）——动态
// 物体的历史采样位置错误由邻域钳制压制：历史颜色超出当前帧 3×3 邻域
// 的 RGB 包络时被拉回边界（遮挡揭示 / 物体移走的过期历史按约 10 帧
// 量级指数衰减，1/0.1 的混合系数）。
//
// 历史采样用线性采样器（重投影落点在亚纹素位置；Nearest 会把亚纹素
// 偏移量化回同纹素——FXAA 的实测教训）。重投影到相机背后（w ≤ 0）或
// 历史画面之外 → 判无效，原色透传。
//
// 重置帧（首帧启用 / 关→开 / 分辨率变化 / 显式 reset_taa）标量 y = 1：
// 输出逐位等于当前帧（ early return，与无 TAA 帧的链输出逐位一致）。

// 参数 uniform（144B，与 CPU 侧 TaaParamsRaw 对齐）：
struct TaaParams {
    inv_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    // x: 历史权重（0.9）, y: 重置标志（1 = 输出当前帧）,
    // z: f/(f−n), w: n·f/(f−n)（深度 → NDC z 重建的两个预计算标量）
    scalars: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: TaaParams;
@group(0) @binding(1) var src_texture: texture_2d<f32>; // 本帧抖动色（主解析缓冲）
@group(0) @binding(2) var src_sampler: sampler; // 线性
@group(0) @binding(3) var depth_texture: texture_2d<f32>; // 深度副本（非过滤，textureLoad）
@group(0) @binding(4) var history_texture: texture_2d<f32>; // 上帧历史

struct TaaVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// 三顶点全屏三角形（与 blit.wgsl 相同）
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> TaaVertexOutput {
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
    var out: TaaVertexOutput;
    out.position = vec4<f32>(positions[index], 0.0, 1.0);
    out.uv = uvs[index];
    return out;
}

@fragment
fn fs_main(in: TaaVertexOutput) -> @location(0) vec4<f32> {
    let current = textureSample(src_texture, src_sampler, in.uv);
    // 重置帧：逐位透传当前帧（混合系数不生效——首帧无历史可言）
    if (params.scalars.y > 0.5) {
        return vec4<f32>(current.rgb, current.a);
    }
    // ---- 相机重投影（与 motion_blur.wgsl 同款数学）----
    let dims = vec2<f32>(textureDimensions(depth_texture));
    let px = clamp(
        vec2<i32>(in.uv * dims),
        vec2<i32>(0, 0),
        vec2<i32>(dims) - vec2<i32>(1, 1),
    );
    let d = textureLoad(depth_texture, px, 0).r;
    let z = clamp(
        params.scalars.z - params.scalars.w / max(d, 1e-6),
        0.0,
        1.0,
    );
    // UV → NDC（V 翻转的逆变换）→ 世界位 → 上帧 NDC → 历史 UV
    let ndc = vec2<f32>(in.uv.x * 2.0 - 1.0, 1.0 - 2.0 * in.uv.y);
    let world_h = params.inv_view_proj * vec4<f32>(ndc, z, 1.0);
    let world = world_h.xyz / world_h.w;
    let prev_clip = params.prev_view_proj * vec4<f32>(world, 1.0);
    let prev_uv = vec2<f32>(prev_clip.x, -prev_clip.y) / prev_clip.w * 0.5 + 0.5;
    // 重投影到相机背后（上一帧不可见）或画面外 → 历史不可信
    let valid = prev_clip.w > 0.0
        && all(prev_uv >= vec2<f32>(0.0, 0.0))
        && all(prev_uv <= vec2<f32>(1.0, 1.0));
    // ---- 3×3 邻域 RGB AABB min/max（9 样本，恒定界循环 = 一致控制流）----
    let texel = 1.0 / dims;
    var mn = current.rgb;
    var mx = current.rgb;
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let c = textureSample(
                src_texture,
                src_sampler,
                in.uv + vec2<f32>(f32(dx), f32(dy)) * texel,
            ).rgb;
            mn = min(mn, c);
            mx = max(mx, c);
        }
    }
    // ---- 钳制混合：邻域内历史可信（时域累加），邻域外拉回包络边界 ----
    let history = textureSample(history_texture, src_sampler, prev_uv).rgb;
    let clamped = clamp(history, mn, mx);
    let blended = mix(current.rgb, clamped, params.scalars.x);
    return vec4<f32>(select(current.rgb, blended, valid), current.a);
}
