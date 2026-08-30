// Iris 标准材质着色器：Blinn-Phong 光照。
// 照明 = 环境光 + 1 个方向光 + 最多 8 个点光（平滑距离衰减）。
// 实例化绘制：每个实例携带模型矩阵（4 个 vec4 列）与法线矩阵（3 个 vec4 列）。

struct PointLight {
    // xyz: 世界位置
    position: vec4<f32>,
    // rgb: 颜色 × 强度, w: 有效半径
    color: vec4<f32>,
};

struct Globals {
    view_proj: mat4x4<f32>,
    // xyz: 相机世界坐标
    eye_pos: vec4<f32>,
    // xyz: 指向光源的单位方向（表面 -> 光源）
    light_dir: vec4<f32>,
    // rgb * intensity
    light_color: vec4<f32>,
    // rgb
    ambient: vec4<f32>,
    // x: 实际点光数量
    point_count: vec4<f32>,
    point_lights: array<PointLight, 8>,
    // 方向光阴影矩阵（世界 -> 光源裁剪空间）
    light_view_proj: mat4x4<f32>,
    // x: 阴影贴图 texel 尺寸 (1/分辨率), y: 深度偏移 bias, z: 阴影强度, w: 是否启用
    shadow_params: vec4<f32>,
};

struct MaterialParams {
    base_color: vec4<f32>,
    emissive: vec4<f32>,
    // 注意：vec3<f32> 在 WGSL 中 16 字节对齐会把结构撑到 64 字节，
    // 这里用三个独立 f32 保持与 CPU 侧 48 字节布局一致
    shininess: f32,
    pad0: f32,
    pad1: f32,
    pad2: f32,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var shadow_map: texture_depth_2d;
@group(0) @binding(2) var shadow_sampler: sampler_comparison;
@group(1) @binding(0) var<uniform> material: MaterialParams;
@group(1) @binding(1) var base_texture: texture_2d<f32>;
@group(1) @binding(2) var base_sampler: sampler;

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
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) shadow_pos: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    let model = mat4x4<f32>(in.model0, in.model1, in.model2, in.model3);
    let normal_mat = mat3x3<f32>(in.normal0.xyz, in.normal1.xyz, in.normal2.xyz);
    let world_pos = model * vec4<f32>(in.position, 1.0);
    var out: VertexOutput;
    out.clip_position = globals.view_proj * world_pos;
    out.world_pos = world_pos.xyz;
    out.world_normal = normal_mat * in.normal;
    out.uv = in.uv;
    out.shadow_pos = globals.light_view_proj * world_pos;
    return out;
}

/// 3×3 PCF 百分比渐近软阴影。返回 0（全阴影）..1（全照明）。
///
/// 注意：textureSampleCompare 要求 uniform 控制流，因此不做
/// 「越界提前返回」，而是先 clamp 采样、再用掩码丢弃越界贡献。
fn shadow_factor(shadow_pos: vec4<f32>) -> f32 {
    if (globals.shadow_params.w < 0.5) {
        return 1.0;
    }
    let proj = shadow_pos.xy / shadow_pos.w;
    let depth = shadow_pos.z / shadow_pos.w;
    let inside = select(0.0, 1.0,
        proj.x >= 0.0 && proj.x <= 1.0 && proj.y >= 0.0 && proj.y <= 1.0 && depth <= 1.0);
    let uv = clamp(proj, vec2<f32>(0.001), vec2<f32>(0.999));
    let texel = globals.shadow_params.x;
    var sum = 0.0;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let offset = vec2<f32>(f32(dx), f32(dy)) * texel;
            // 硬件深度比较（LessEqual）：被遮挡为 0，可见为 1
            sum += textureSampleCompare(shadow_map, shadow_sampler, uv + offset, depth - globals.shadow_params.y);
        }
    }
    let lit = sum / 9.0;
    let shaded = mix(1.0 - globals.shadow_params.z, 1.0, lit);
    return mix(1.0, shaded, inside);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let base = material.base_color * textureSample(base_texture, base_sampler, in.uv);
    let n = normalize(in.world_normal);
    let l = normalize(globals.light_dir.xyz);
    let view_dir = normalize(globals.eye_pos.xyz - in.world_pos);
    let half_dir = normalize(l + view_dir);
    let diffuse = max(dot(n, l), 0.0);
    // 背光面不产生高光；方向光漫反射与高光受阴影遮挡
    let facing = step(0.0, dot(n, l));
    let specular = pow(max(dot(n, half_dir), 0.0), material.shininess) * facing;
    let sun = shadow_factor(in.shadow_pos);


    var lit = base.rgb * globals.ambient.rgb
            + base.rgb * globals.light_color.rgb * diffuse * sun
            + globals.light_color.rgb * specular * sun
            + material.emissive.rgb;

    // 点光：平滑二次衰减，range 处归零
    let count = u32(globals.point_count.x);
    for (var i = 0u; i < count; i = i + 1u) {
        let light = globals.point_lights[i];
        let to_light = light.position.xyz - in.world_pos;
        let dist = length(to_light);
        if (dist < light.color.w) {
            let lp = to_light / max(dist, 1e-4);
            let ndotl = max(dot(n, lp), 0.0);
            if (ndotl > 0.0) {
                let falloff = 1.0 - dist / light.color.w;
                let atten = falloff * falloff;
                let radiance = light.color.rgb * atten;
                let half_l = normalize(lp + view_dir);
                let spec = pow(max(dot(n, half_l), 0.0), material.shininess);
                lit = lit + base.rgb * radiance * ndotl + radiance * spec;
            }
        }
    }
    return vec4<f32>(lit, base.a);
}
