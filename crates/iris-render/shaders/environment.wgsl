// 环境光照烘焙：天空源立方体贴图 -> 漫反射辐照度卷积 / GGX 预滤波 / BRDF LUT。
// 片段入口共享全屏三角形顶点着色器与 uniform（a.x = 立方体面索引，
// a.y = 预滤波粗糙度）。天空捕获有两条路径：fs_sky（程序化天空）与
// fs_capture_equirect（外部 equirectangular 全景图，绑定 2D 纹理）；下游
// 辐照度 / 预滤波 / BRDF pass 与天空源无关，全部一次性烘焙，运行时零开销。
//
// IBL 采用分割求和近似（Karis 2010）：
//   环境光 ≈ irradiance(n) * kd * albedo + prefiltered_env(r) * (F * LUT.x + LUT.y)

struct EnvParams {
    // x: 立方体面索引（0..5），y: 预滤波粗糙度，z: 太阳方向预备位
    a: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: EnvParams;
// 源环境立方体贴图（辐照度 / 预滤波 pass 采样；天空 / BRDF pass 不用）
@group(0) @binding(1) var env_cube: texture_cube<f32>;
@group(0) @binding(2) var env_sampler: sampler;
// 外部 equirectangular 全景图（fs_capture_equirect 专用；绑定在独立的
// equirect 布局上，程序化路径的 plain 布局无需提供这两个资源）
@group(0) @binding(1) var equirect_tex: texture_2d<f32>;
@group(0) @binding(2) var equirect_sampler: sampler;

struct EnvVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// 三顶点全屏三角形（与 blit.wgsl 相同）
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> EnvVertexOutput {
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
    var out: EnvVertexOutput;
    out.position = vec4<f32>(positions[index], 0.0, 1.0);
    out.uv = uvs[index];
    return out;
}

// 面片 uv -> 立方体方向（采样器 dir->uv 的精确逆映射，WebGPU 沿用 OpenGL
// 立方体约定：+X: sc=-rz,tc=-ry | -X: sc=rz,tc=-ry | +Y: sc=rx,tc=rz |
// -Y: sc=rx,tc=-rz | +Z: sc=rx,tc=-ry | -Z: sc=-rx,tc=-ry。
// ±Z 面的 tc 符号写反会把天空垂直镜像烘焙（实测：仰视天顶拿到地平线浅色）
fn face_dir(face: u32, uv: vec2<f32>) -> vec3<f32> {
    let a = uv * 2.0 - 1.0;
    switch face {
        case 0u: { return vec3<f32>(1.0, -a.y, -a.x); }   // +X
        case 1u: { return vec3<f32>(-1.0, -a.y, a.x); }   // -X
        case 2u: { return vec3<f32>(a.x, 1.0, a.y); }     // +Y
        case 3u: { return vec3<f32>(a.x, -1.0, -a.y); }   // -Y
        case 4u: { return vec3<f32>(a.x, -a.y, 1.0); }    // +Z
        default: { return vec3<f32>(-a.x, -a.y, -1.0); }  // -Z
    }
}

const PI: f32 = 3.14159265;

// ---- 程序化天空：天顶蓝 -> 地平线白，地面暖棕回弹 + HDR 太阳盘 ----

const SUN_DIR = vec3<f32>(0.4, 0.8, 0.45);

fn sky_color(d: vec3<f32>) -> vec3<f32> {
    let dir = normalize(d);
    let t = dir.y;
    // 亮部必须保持在泛光阈值（1.0）以下，否则地平线会自发泛光、
    // 经三轮模糊铺满整个天空，把渐变洗成一片均匀的浅蓝；
    // 太阳盘（30.0）是天空里唯一的泛光源
    let zenith = vec3<f32>(0.10, 0.36, 0.94);
    let horizon = vec3<f32>(0.70, 0.80, 0.94);
    let ground = vec3<f32>(0.30, 0.24, 0.18) * 0.55;
    var c = horizon;
    if (t >= 0.0) {
        c = mix(horizon, zenith, pow(clamp(t, 0.0, 1.0), 0.45));
    } else {
        c = mix(horizon, ground, pow(clamp(-t, 0.0, 1.0), 0.5));
    }
    // 太阳盘 + 光晕（HDR 亮度，供镜面反射出现热点）
    let sd = max(dot(dir, normalize(SUN_DIR)), 0.0);
    let disc = smoothstep(0.9993, 0.9997, sd);
    let halo = pow(sd, 350.0) * 0.12;
    c = c + (vec3<f32>(30.0, 26.0, 18.0) * disc + vec3<f32>(1.0, 0.85, 0.6) * halo);
    return c;
}

@fragment
fn fs_sky(in: EnvVertexOutput) -> @location(0) vec4<f32> {
    let d = face_dir(u32(params.a.x), in.uv);
    return vec4<f32>(sky_color(d), 1.0);
}

// ---- 外部 equirectangular 捕获：方向 -> 全景 uv（图像第 0 行 = +Y 天顶） ----

// u: 方位角（+X 方向在图像水平正中），v: 0 = 天顶行、1 = 天底行
fn dir_to_equirect_uv(dir: vec3<f32>) -> vec2<f32> {
    let d = normalize(dir);
    let u = 0.5 + atan2(d.z, d.x) / (2.0 * PI);
    let v = 0.5 - asin(clamp(d.y, -1.0, 1.0)) / PI;
    return vec2<f32>(u, v);
}

@fragment
fn fs_capture_equirect(in: EnvVertexOutput) -> @location(0) vec4<f32> {
    let d = face_dir(u32(params.a.x), in.uv);
    let uv = dir_to_equirect_uv(d);
    return vec4<f32>(textureSampleLevel(equirect_tex, equirect_sampler, uv, 0.0).rgb, 1.0);
}

// ---- 漫反射辐照度：围绕法线的余弦加权半球积分 ----

@fragment
fn fs_irradiance(in: EnvVertexOutput) -> @location(0) vec4<f32> {
    let n = normalize(face_dir(u32(params.a.x), in.uv));
    let up = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(n.y) > 0.999);
    let right = normalize(cross(up, n));
    let fwd = cross(n, right);

    var sum = vec3<f32>(0.0);
    let delta = 0.025;
    var samples = 0.0;
    var phi = 0.0;
    loop {
        if (phi >= 2.0 * PI) { break; }
        var theta = 0.0;
        loop {
            if (theta >= 0.5 * PI) { break; }
            let dir = right * (cos(phi) * sin(theta))
                    + fwd * (sin(phi) * sin(theta))
                    + n * cos(theta);
            // 余弦权重 cos(theta) 与立体角因子 sin(theta)
            sum = sum + textureSampleLevel(env_cube, env_sampler, dir, 0.0).rgb
                        * (cos(theta) * sin(theta));
            samples = samples + 1.0;
            theta = theta + delta;
        }
        phi = phi + delta;
    }
    return vec4<f32>(PI * sum / samples, 1.0);
}

// ---- GGX 重要性采样（Hammersley 序列），预滤波 + BRDF LUT 共用 ----

// Van der Corput 根式逆（u32 位反转）：WGSL 无内建位反转，用移位交换展开
fn radical_inverse_vdc(i: u32) -> f32 {
    var bits = i;
    bits = (bits << 16u) | (bits >> 16u);
    bits = ((bits & 0x55555555u) << 8u) | ((bits & 0xAAAAAAAAu) >> 8u);
    bits = ((bits & 0x33333333u) << 4u) | ((bits & 0xCCCCCCCCu) >> 4u);
    bits = ((bits & 0x0F0F0F0Fu) << 2u) | ((bits & 0xF0F0F0F0u) >> 2u);
    bits = ((bits & 0x00FF00FFu) << 1u) | ((bits & 0xFF00FF00u) >> 1u);
    return f32(bits) * 2.3283064365386963e-10; // / 2^32
}

fn hammersley(i: u32, n: u32) -> vec2<f32> {
    return vec2<f32>(f32(i) / f32(n), radical_inverse_vdc(i));
}

fn importance_sample_ggx(xi: vec2<f32>, n: vec3<f32>, roughness: f32) -> vec3<f32> {
    let a = roughness * roughness;
    let phi = 2.0 * PI * xi.x;
    let cos_t = sqrt((1.0 - xi.y) / (1.0 + (a * a - 1.0) * xi.y));
    let sin_t = sqrt(max(1.0 - cos_t * cos_t, 0.0));
    let h = vec3<f32>(cos(phi) * sin_t, sin(phi) * sin_t, cos_t);
    let up = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(n.y) > 0.999);
    let right = normalize(cross(up, n));
    let fwd = cross(n, right);
    return normalize(right * h.x + fwd * h.y + n * h.z);
}

@fragment
fn fs_prefilter(in: EnvVertexOutput) -> @location(0) vec4<f32> {
    let n = normalize(face_dir(u32(params.a.x), in.uv));
    let roughness = clamp(params.a.y, 0.0, 1.0);
    var prefiltered = vec3<f32>(0.0);
    var total_weight = 0.0;
    let samples = 128u;
    for (var i = 0u; i < samples; i = i + 1u) {
        let xi = hammersley(i, samples);
        let h = importance_sample_ggx(xi, n, roughness);
        // 视线沿法线：l = reflect(-n, h)
        let l = normalize(2.0 * dot(n, h) * h - n);
        let nol = max(dot(n, l), 0.0);
        prefiltered = prefiltered + textureSampleLevel(env_cube, env_sampler, l, 0.0).rgb * nol;
        total_weight = total_weight + nol;
    }
    return vec4<f32>(prefiltered / max(total_weight, 1e-4), 1.0);
}

// ---- BRDF 分割求和 LUT：uv = (NdotV, roughness) -> (F 的缩放, 偏移) ----

fn distribution_ggx(noh: f32, roughness: f32) -> f32 {
    let a = roughness * roughness;
    let a2 = a * a;
    let d = noh * noh * (a2 - 1.0) + 1.0;
    return a2 / (PI * d * d + 1e-7);
}

// IBL 变体：k = roughness²/2 的 Smith-Schlick 几何项
fn geometry_ibl(nov: f32, nol: f32, roughness: f32) -> f32 {
    let k = (roughness * roughness) * 0.5;
    let gv = nov / (nov * (1.0 - k) + k);
    let gl = nol / (nol * (1.0 - k) + k);
    return gv * gl;
}

@fragment
fn fs_brdf(in: EnvVertexOutput) -> @location(0) vec4<f32> {
    let nov = clamp(in.uv.x, 1e-3, 1.0);
    let roughness = clamp(in.uv.y, 0.0, 1.0);
    let n = vec3<f32>(0.0, 0.0, 1.0);
    let v = vec3<f32>(sqrt(1.0 - nov * nov), 0.0, nov);

    var scale = 0.0;
    var bias = 0.0;
    let samples = 1024u;
    for (var i = 0u; i < samples; i = i + 1u) {
        let xi = hammersley(i, samples);
        let h = importance_sample_ggx(xi, n, roughness);
        let l = normalize(2.0 * dot(v, h) * h - v);
        let nol = max(l.z, 0.0);
        let noh = max(h.z, 0.0);
        let voh = max(dot(v, h), 0.0);
        if (nol > 0.0) {
            let d = distribution_ggx(noh, roughness);
            let g = geometry_ibl(nov, nol, roughness);
            let fc = pow(1.0 - voh, 5.0);
            scale = scale + (1.0 - fc) * g * voh;
            bias = bias + fc * g * voh;
        }
    }
    return vec4<f32>(scale / f32(samples), bias / f32(samples), 0.0, 1.0);
}
