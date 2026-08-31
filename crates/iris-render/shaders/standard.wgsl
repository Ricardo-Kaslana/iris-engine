// Iris 标准材质着色器：Cook-Torrance PBR（金属度/粗糙度工作流）。
// 照明 = 环境光（Lambert 近似）+ 1 个方向光 + 最多 8 个点光（物理平方衰减）。
// 方向光带阴影贴图（R 通道深度 + 3×3 手工比较 PCF）；渲染在线性 HDR 空间，
// 色调映射由 blit pass 完成。
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
    // rgb（线性，乘在 albedo 上的环境项）
    ambient: vec4<f32>,
    // x: 实际点光数量
    point_count: vec4<f32>,
    point_lights: array<PointLight, 8>,
    // 方向光阴影矩阵（世界 -> 光源裁剪空间）
    light_view_proj: mat4x4<f32>,
    // x: 阴影贴图 texel 尺寸, y: 深度偏移 bias, z: 阴影强度, w: 是否启用
    shadow_params: vec4<f32>,
    // x: 预滤波环境最大 mip 级, y: IBL 强度
    env_params: vec4<f32>,
    // 天空盒射线重建用（clip -> world）
    inv_view_proj: mat4x4<f32>,
};

struct MaterialParams {
    base_color: vec4<f32>,
    emissive: vec4<f32>,
    // x: metallic, y: roughness, z: 绝缘体反射率基准 F0，w: 保留
    surface: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var shadow_map: texture_2d<f32>;
@group(0) @binding(2) var shadow_sampler: sampler;
// IBL 分割求和：辐照度立方体贴图 / 预滤波环境立方体贴图 / BRDF LUT
@group(0) @binding(3) var irradiance_map: texture_cube<f32>;
@group(0) @binding(4) var env_map: texture_cube<f32>;
@group(0) @binding(5) var brdf_lut: texture_2d<f32>;
@group(0) @binding(6) var env_sampler: sampler;
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

// ---- Cook-Torrance 项 ----

// 法线分布：GGX / Trowbridge-Reitz
fn distribution_ggx(noh: f32, roughness: f32) -> f32 {
    let a = roughness * roughness;
    let a2 = a * a;
    let d = noh * noh * (a2 - 1.0) + 1.0;
    return a2 / (3.14159265 * d * d + 1e-7);
}

// 几何遮蔽：Schlick-GGX（k = roughness²/2，直接光照近似）
fn geometry_schlick_ggx(nov: f32, roughness: f32) -> f32 {
    let k = (roughness * roughness) * 0.5;
    return nov / (nov * (1.0 - k) + k);
}

fn geometry_smith(nov: f32, nol: f32, roughness: f32) -> f32 {
    return geometry_schlick_ggx(nov, roughness) * geometry_schlick_ggx(nol, roughness);
}

// 菲涅尔：Schlick 近似
fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (vec3<f32>(1.0) - f0) * pow(clamp(1.0 - cos_theta, 0.0, 1.0), 5.0);
}

// 环境光专用菲涅尔：边缘反射率随粗糙度抬高（避免粗糙金属掠射角过暗）
fn fresnel_schlick_roughness(cos_theta: f32, f0: vec3<f32>, roughness: f32) -> vec3<f32> {
    let fmax = max(vec3<f32>(1.0 - roughness), f0);
    return f0 + (fmax - f0) * pow(clamp(1.0 - cos_theta, 0.0, 1.0), 5.0);
}

// 单灯的 Cook-Torrance 贡献（radiance 已含强度与衰减）
fn pbr_light(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, radiance: vec3<f32>,
             albedo: vec3<f32>, f0: vec3<f32>, metallic: f32, roughness: f32) -> vec3<f32> {
    let h = normalize(v + l);
    let nol = max(dot(n, l), 0.0);
    let nov = max(dot(n, v), 0.0);
    let noh = max(dot(n, h), 0.0);
    let voh = max(dot(v, h), 0.0);

    let d = distribution_ggx(noh, roughness);
    let g = geometry_smith(nov, nol, roughness);
    let f = fresnel_schlick(voh, f0);
    let specular = (d * g * f) / max(4.0 * nov * nol, 1e-4);
    // 能量守恒：漫反射扣除菲涅尔反射占比，金属无漫反射
    let kd = (vec3<f32>(1.0) - f) * (1.0 - metallic);
    return (kd * albedo / 3.14159265 + specular) * radiance * nol;
}

/// 3×3 PCF 百分比渐近软阴影，手工深度比较（R 通道存光源空间深度，
/// 清空值 1.0 代表无遮挡）。返回阴影后保留的光照比例。
///
/// 阴影相机覆盖范围外、光源背后或阴影关闭的片段直接视为受光。
/// textureSample 要求 uniform 控制流，因此不做越界提前返回，
/// 采样照常执行、最后用掩码把结果混向「受光」。
fn shadow_factor(shadow_pos: vec4<f32>) -> f32 {
    let w = max(shadow_pos.w, 1e-6);
    let ndc = shadow_pos.xy / w;
    let depth = shadow_pos.z / w;
    // NDC -> UV：NDC y 正方向 = 贴图上端（与 blit 的 V 翻转约定一致）
    let uv = ndc * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    let texel = globals.shadow_params.x;
    var lit = 0.0;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let d = textureSample(shadow_map, shadow_sampler,
                                  uv + vec2<f32>(f32(dx), f32(dy)) * texel).r;
            // 遮挡物深度 >= 片段参考深度 -> 未被遮挡
            lit = lit + select(0.0, 1.0, d >= depth - globals.shadow_params.y);
        }
    }
    lit = lit / 9.0;
    let shaded = mix(1.0 - globals.shadow_params.z, 1.0, lit);
    let outside = shadow_pos.w <= 0.0
        || ndc.x < -1.0 || ndc.x > 1.0
        || ndc.y < -1.0 || ndc.y > 1.0
        || depth < 0.0 || depth > 1.0
        || globals.shadow_params.w < 0.5;
    return mix(shaded, 1.0, select(0.0, 1.0, outside));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let base = material.base_color * textureSample(base_texture, base_sampler, in.uv);
    let albedo = base.rgb;
    let metallic = clamp(material.surface.x, 0.0, 1.0);
    let roughness = clamp(material.surface.y, 0.045, 1.0);
    let f0 = mix(vec3<f32>(material.surface.z), albedo, metallic);

    let n = normalize(in.world_normal);
    let v = normalize(globals.eye_pos.xyz - in.world_pos);

    // 方向光（受阴影遮挡）
    let l = normalize(globals.light_dir.xyz);
    let sun = shadow_factor(in.shadow_pos);
    let radiance = globals.light_color.rgb * sun;
    var color = pbr_light(n, v, l, radiance, albedo, f0, metallic, roughness);

    // 点光：物理平方衰减 + range 平滑窗口
    let count = u32(globals.point_count.x);
    for (var i = 0u; i < count; i = i + 1u) {
        let light = globals.point_lights[i];
        let to_light = light.position.xyz - in.world_pos;
        let dist = length(to_light);
        if (dist < light.color.w) {
            let lp = to_light / max(dist, 1e-4);
            let t = dist / light.color.w;
            let window = 1.0 - t * t;
            let atten = (window * window) / max(dist * dist, 0.01);
            let pl = light.color.rgb * atten;
            color = color + pbr_light(n, v, lp, pl, albedo, f0, metallic, roughness);
        }
    }

    // 环境光（IBL 分割求和）：漫反射采样辐照度贴图；镜面按粗糙度取预滤波
    // 环境 mip，乘 BRDF LUT 的 (缩放, 偏移)。intensity 可整帧关闭 IBL。
    let irradiance = textureSampleLevel(irradiance_map, env_sampler, n, 0.0).rgb;
    let nov = max(dot(n, v), 0.0);
    let f_amb = fresnel_schlick_roughness(nov, f0, roughness);
    let kd_amb = (vec3<f32>(1.0) - f_amb) * (1.0 - metallic);
    let env_spec = textureSampleLevel(env_map, env_sampler, reflect(-v, n),
                                      roughness * globals.env_params.x).rgb;
    let brdf = textureSample(brdf_lut, env_sampler, vec2<f32>(nov, roughness)).rg;
    let ibl = (kd_amb * albedo * irradiance + env_spec * (f_amb * brdf.x + brdf.y))
            * globals.env_params.y;
    // 美术补光：方向光资源上的常量环境项（与 IBL 叠加）
    let f_env = mix(vec3<f32>(material.surface.z), albedo, 0.5);
    color = color + ibl + mix(albedo, f_env, metallic) * globals.ambient.rgb + material.emissive.rgb;

    return vec4<f32>(color, base.a);
}
