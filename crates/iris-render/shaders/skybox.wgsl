// 天空盒背景：全屏三角形（深度只读），按像素视线方向采样预滤波环境
// 立方体贴图的 0 级 mip——粗糙度 0 的预滤波结果与原始天空逐像素一致，
// 无需为背景单独绑定天空贴图。在主 pass 内最先绘制，几何体随后覆盖。
//
// 射线重建：像素 NDC（z = 远平面）经 inv_view_proj 变换回世界空间，
// 方向 = 远平面点 - 相机位置。
//
// R79 大气散射（天空 v2）：binding 7 = SkyParams uniform（64B）。mode = 0
// （资源缺席 / SkySettings.atmosphere = false——渲染器不写参数，缓冲保持
// 零初始化）走旧 env cubemap 采样路径，与引入前逐位一致（全场景同值的
// 动态分支，死码分支零开销）；mode = 1 走片元内**解析单次散射**（无 LUT
// ——确定性零基础设施的裁定）：平面指数大气 + Kasten 风格气团近似的
// Rayleigh + Mie（Cornette-Shanks 相位）+ HDR 太阳盘。诚实限制（艺术化
// 解析近似非物理量纲、行星曲率不可测、IBL env 不联动等）见
// ARCHITECTURE §4.36 与 §8.48。捕获相机（探针）走同一着色器同一 uniform
// ——探针里的天空与主视口同源，这是特性不是限制。

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

// R79 大气散射参数（64B，与 renderer.rs 的 SkyParamsRaw 逐字段对齐，
// 静态断言在那边）：
// - mode_params: x = mode（0 旧渐变 / env 采样，1 大气解析散射），
//   y = Rayleigh 强度，z = Mie 强度，w = 太阳盘 HDR 强度；
// - sun_dir: xyz = 单位太阳方向（世界空间，= -light_dir），w = 太阳盘
//   smoothstep 内阈 cos（0.99985）；
// - sun_color: rgb = 太阳辐照色（线性 HDR = light.color × intensity），
//   w = 太阳盘 smoothstep 外阈 cos（0.99995）；
// - ground: rgb = 地平线以下淡出色，w = 天空整体强度校准（SKY_INTENSITY
//   常量 22.0，把物理量级的散射亮度折算到本仓 HDR 域）。
struct SkyParams {
    mode_params: vec4<f32>,
    sun_dir: vec4<f32>,
    sun_color: vec4<f32>,
    ground: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(4) var env_map: texture_cube<f32>;
@group(0) @binding(6) var env_sampler: sampler;
@group(0) @binding(7) var<uniform> sky_params: SkyParams;

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

// ---- R79 解析单次散射（平面指数大气，本仓场景尺度折衷）----

const SKY_PI: f32 = 3.14159265;
const SKY_H_R: f32 = 8000.0; // Rayleigh 标高（m）
const SKY_H_M: f32 = 1200.0; // Mie 标高（m）
// Rayleigh 散射系数（/m，波长相关）。蓝端在物理值 33.1e-6 上做了艺术化
// 增强（→52e-6）：物理量级的蓝端在天顶只给低饱和灰蓝，增强后天顶
// sRGB ≈ (0.42, 0.54, 0.79) 的常规晴空观感——非物理量纲的诚实声明见
// ARCHITECTURE §4.36 / §8.48
const SKY_BETA_R: vec3<f32> = vec3<f32>(5.8e-6, 13.5e-6, 52e-6);
// Mie 散射系数（/m）：晴空气溶胶取量级下限（21e-6 为霾天值——雾蒙天空
// 会把 Rayleigh 的蓝洗掉；艺术化折衷见 ARCHITECTURE §4.36）
const SKY_BETA_M: f32 = 4e-6;
const SKY_MIE_G: f32 = 0.76; // Cornette-Shanks 各向异性（大气气溶胶典型值）

fn sky_airmass(mu: f32) -> f32 {
    // Kasten 风格气团近似（艺术化）：天顶 1.0、地平线 ~22——除法的分母
    // 下限 0.045 保证任何方向的视线柱有界（f32 无奇点）
    return 1.0 / (max(mu, 0.0) + 0.045);
}

// 太阳路径透射（光源侧气柱的消光——日落时太阳/盘面红化变暗的来源）。
fn sky_sun_transmittance() -> vec3<f32> {
    let sun = sky_params.sun_dir.xyz;
    let obs_h = max(globals.eye_pos.y, 0.0);
    let airmass_s = 1.0 / max(sun.y, 0.045); // 地平线下按地平线气团计
    let od_sun = SKY_BETA_R * (exp(-obs_h / SKY_H_R) * SKY_H_R * airmass_s)
        + vec3<f32>(SKY_BETA_M * exp(-obs_h / SKY_H_M) * SKY_H_M * airmass_s);
    return exp(-od_sun);
}

// dir：单位视线（世界空间，天空盒射线重建产物）。返回线性 HDR 天空色
//（未含地平线以下淡出与强度校准——调用方处理）。
fn sky_atmosphere(dir: vec3<f32>) -> vec3<f32> {
    let sun = sky_params.sun_dir.xyz;
    // 平面指数大气（行星曲率在米级场景不可测——本仓场景尺度折衷）：
    // 观察者高度只影响起点密度，气团用 Kasten 近似
    let obs_h = max(globals.eye_pos.y, 0.0);
    let dens_r = exp(-obs_h / SKY_H_R);
    let dens_m = exp(-obs_h / SKY_H_M);
    let airmass_v = sky_airmass(dir.y);
    // 视线柱（等效米数 = 起点密度 × 标高 × 气团）
    let col_r = dens_r * SKY_H_R * airmass_v;
    let col_m = dens_m * SKY_H_M * airmass_v;
    let cos_theta = clamp(dot(dir, sun), -1.0, 1.0);
    // Rayleigh 相位（分子散射精确式）
    let phase_r = 3.0 / (16.0 * SKY_PI) * (1.0 + cos_theta * cos_theta)
        * sky_params.mode_params.y;
    // Cornette-Shanks 相位（HG 的 δ-Eddington 修正，前向峰更接近 Mie 解）
    let g2 = SKY_MIE_G * SKY_MIE_G;
    let phase_m = 3.0 / (8.0 * SKY_PI) * ((1.0 - g2) * (1.0 + cos_theta * cos_theta))
        / ((2.0 + g2) * pow(1.0 + g2 - 2.0 * SKY_MIE_G * cos_theta, 1.5))
        * sky_params.mode_params.z;
    // 光学深度与逐通道透射（视线段）
    let od_view = SKY_BETA_R * col_r + vec3<f32>(SKY_BETA_M * col_m);
    let view_t = exp(-od_view);
    // 单次散射解析积分：∫₀^D e^(−σt) dt = (1 − T)/σ（逐通道）。散射源
    // 强度 = β × 柱密度 × 相位（柱密度是积分核的权重——漏乘它天空会
    // 整体暗三个数量级）
    let scatter = SKY_BETA_R * (col_r * phase_r) + vec3<f32>(SKY_BETA_M * (col_m * phase_m));
    let integral = (vec3<f32>(1.0) - view_t) / max(od_view, vec3<f32>(1e-6));
    return sky_params.sun_color.rgb * sky_sun_transmittance() * scatter * integral;
}

@fragment
fn fs_main(in: SkyVertexOutput) -> @location(0) vec4<f32> {
    let far_point = globals.inv_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let dir = normalize(far_point.xyz / far_point.w - globals.eye_pos.xyz);
    // mode = 0：旧渐变 / env cubemap 路径逐式保留（缺席 / 关闭帧与引入
    // 前逐位一致——动态 uniform 分支，全部片元同值）
    if (sky_params.mode_params.x < 0.5) {
        return vec4<f32>(textureSampleLevel(env_map, env_sampler, dir, 0.0).rgb, 1.0);
    }
    // mode = 1：解析大气
    let cos_theta = clamp(dot(dir, sky_params.sun_dir.xyz), -1.0, 1.0);
    var sky = sky_atmosphere(dir);
    // HDR 太阳盘（太阳路径透射调制——日落时盘面随大气红化变暗）
    let disk = smoothstep(sky_params.sun_dir.w, sky_params.sun_color.w, cos_theta);
    sky += sky_params.sun_color.rgb * (sky_params.mode_params.w * disk * sky_sun_transmittance());
    // 地平线以下按仰角淡出（−0.06 全落地面色；smoothstep 要求边沿递增，
    // 故用 1 − smoothstep(−0.06, 0, y)）
    let below = 1.0 - smoothstep(-0.06, 0.0, dir.y);
    sky = mix(sky, sky_params.ground.rgb, below);
    return vec4<f32>(sky * sky_params.ground.w, 1.0);
}
