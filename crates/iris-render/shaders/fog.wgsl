// 体积雾 pass（R79）：后处理**链头**的全屏 pass（主 pass / TAA 之后、
// DOF / MB / 泛光之前）——逐像素沿视线固定 FOG_STEPS 步光线步进，累加
// 消光透过率与太阳内散射（HG 相位 × 太阳色 × 密度 × 步长），最终
// out = color × T + inscatter（发射-吸收积分的标准形式）。
//
// 输入 = 链头 HDR 颜色（binding 1，链源 = 主解析缓冲 / TAA 本帧输出）
// + scene_depth（binding 3，R32Float **线性视距**——链上同帧消费，与
// DOF / MB 同款：同一编码器内本帧场景深度副本已先编码，读到的是本帧
// 深度；「上帧」只适用于主 pass 之前读深度的消费者如 SSR / GTAO）
// + 参数 uniform + 相机重建量。组布局复用 `post_depth_layout`
// （与 DOF / MB 同构：参数 + 源色 + 采样器 + 非过滤深度）。
//
// 深度口径（R77 半纹素教训）：深度读取与位置重建一律取**同一纹素中
// 心**——`floor(uv·dims)` 落在的纹素，其配深必须用该纹素中心坐标。
// 天空像素（深度 = 远平面清空值 z_far）的雾射线长取 max_distance。
//
// 纯函数镜像契约：fog_density_at / fog_phase_hg / fog_dither 与
// `src/fog.rs` 的同名 CPU 函数**逐行镜像（两侧同步修改）**——CPU 单测
// 即着色器行为的解析锚点；fog_dither 与 post::ao_dither 同一套回绕整数
// 哈希（u32 运算 WGSL 天然回绕，两侧位级一致）。
//
// 步进起点按像素哈希抖动一个步长以内（去条带的交错式 dither）——纯
// 像素坐标函数，无状态、无时域依赖，跨运行逐位一致。

// 参数（160 B，POD；与 renderer.rs 的 FogParamsRaw 逐字段对齐）。
struct FogParams {
    inv_view_proj: mat4x4<f32>, // 主相机（proj × view）⁻¹——NDC → 世界
    eye: vec4<f32>,             // xyz 相机世界位，w 保留
    sun_dir: vec4<f32>,         // xyz 单位太阳方向（= -light_dir），w 保留
    sun_color: vec4<f32>,       // rgb 太阳辐照色（线性 HDR），w 保留
    a: vec4<f32>,               // x density, y height_falloff, z height_offset, w inscatter_strength
    b: vec4<f32>,               // x phase_g, y max_distance, z z_far（scene_depth 天空清空值）, w a_coef
    c: vec4<f32>,               // x b_coef, y/z/w 保留
};

@group(0) @binding(0) var<uniform> params: FogParams;
@group(0) @binding(1) var src_color: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
// 上帧场景深度副本：R32Float 非过滤——textureLoad 整数纹素读取
//（与 AO / DOF / MB 的深度消费同款）。
@group(0) @binding(3) var scene_depth: texture_2d<f32>;

const FOG_STEPS: u32 = 16u;
const FOG_PI: f32 = 3.14159265;

struct FogVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// 三顶点全屏三角形（与 ao.wgsl 相同；uv 与纹理行序 / 帧缓冲坐标同向）
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> FogVertexOutput {
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
    var out: FogVertexOutput;
    out.position = vec4<f32>(positions[index], 0.0, 1.0);
    out.uv = uvs[index];
    return out;
}

// 高度 h 处的雾消光密度（CPU 镜像 fog::fog_density_at——两侧同步修改）：
// 指数高度衰减，下限钳 0（负密度 / 极端参数一律按「无雾」处理）。
fn fog_density_at(height: f32, density: f32, falloff: f32, offset: f32) -> f32 {
    return max(density * exp(-(height - offset) * falloff), 0.0);
}

// Henyey-Greenstein 相位（CPU 镜像 fog::fog_phase_hg——两侧同步修改）。
// g 由渲染器钳制到 ±0.999（|g| → 1 时分母趋 0）。
fn fog_phase_hg(cos_theta: f32, g: f32) -> f32 {
    let g2 = g * g;
    return (1.0 - g2) / (4.0 * FOG_PI * pow(1.0 + g2 - 2.0 * g * cos_theta, 1.5));
}

// 像素坐标 → [0, 1) 确定性抖动（CPU 镜像 fog::fog_dither——u32 运算
// 回绕语义两侧位级一致）。纯像素坐标函数：无状态、无时域依赖。
fn fog_dither(px: vec2<i32>) -> f32 {
    var h = u32(px.x) * 73856093u ^ u32(px.y) * 19349663u;
    h = (h ^ (h >> 13u)) * 1274126177u;
    h = h ^ (h >> 16u);
    return f32(h & 0x00FFFFFFu) / 16777216.0;
}

@fragment
fn fs_main(in: FogVertexOutput) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(scene_depth));
    let dims_i = vec2<i32>(dims);
    let px = clamp(vec2<i32>(floor(in.uv * dims)), vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
    // 深度纹素中心口径：位置重建用的 uv 与配深取**同一纹素**的中心
    //（半纹素错配会在斜面上注入假距离，R77 实测教训）
    let uv_c = (vec2<f32>(px) + vec2<f32>(0.5)) / dims;
    let ndc = uv_c * 2.0 - vec2<f32>(1.0);
    let d = textureLoad(scene_depth, px, 0).r;
    // 射线方向：远平面点重建（与 skybox.wgsl 同式，inv_view_proj 复用）
    let far4 = params.inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
    let dir = normalize(far4.xyz / far4.w - params.eye.xyz);
    // 射线长度：天空像素（深度 = 远平面清空值）步进到 max_distance；
    // 几何像素由线性视距反算 NDC z（DirectX [0,1] 约定，与 MB 同款系数
    // z_ndc = a_coef − b_coef/d）重建世界命中点
    var ray_len = params.b.y;
    if (d < params.b.z) {
        let z_ndc = params.b.w - params.c.x / max(d, 1e-6);
        let hit4 = params.inv_view_proj * vec4<f32>(ndc, z_ndc, 1.0);
        ray_len = min(length(hit4.xyz / hit4.w - params.eye.xyz), params.b.y);
    }
    let ds = ray_len / f32(FOG_STEPS);
    // 起点抖动：确定性像素哈希，把步进量化噪声打碎成高频颗粒（去条带）
    let start = params.eye.xyz + dir * (fog_dither(px) * ds);
    let phase = fog_phase_hg(dot(dir, params.sun_dir.xyz), clamp(params.b.x, -0.999, 0.999));
    // 发射-吸收积分：先以步前透过率累计内散射，再衰减透过率
    var transmittance = 1.0;
    var inscatter = vec3<f32>(0.0);
    for (var i = 0u; i < FOG_STEPS; i = i + 1u) {
        let p = start + dir * (f32(i) * ds);
        let sigma = fog_density_at(p.y, params.a.x, params.a.y, params.a.z);
        inscatter += transmittance * sigma * ds * phase * params.sun_color.rgb * params.a.w;
        transmittance *= exp(-sigma * ds);
    }
    let color = textureSample(src_color, samp, in.uv).rgb;
    return vec4<f32>(color * transmittance + inscatter, 1.0);
}
