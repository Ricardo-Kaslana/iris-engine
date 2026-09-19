// 平面水体 pass（R81）：主 pass / 贴花 / 透明 / 粒子 / scene_depth 之后、
// TAA 与后处理链之前——把水面 alpha 合成（a = 1 等价替换）进 HDR MSAA。
// 每片元：反射（半分辨率镜像相机捕获）× Fresnel（Schlick，F0 = 0.02 水
// 的物理值）+ 折射（主相机半分辨率捕获）× 深度吸收（scene_depth 第六
// 消费者——**链上同帧**：水体 pass 编码在本帧 scene_depth pass 之后，
// 同一编码器按序执行 ⇒ 读到的是本帧深度；「上帧」语义只适用于主 pass
// 之前读深度的 SSR / GTAO）+ 太阳镜面高光。
//
// 独立组 0 布局（不扩 globals 组——R61/R77/R78 先例）：0 = WaterParams
// uniform（256B，R92 扩容 240 → 256 增加天气通道）、1 = 反射 RT
// （Rgba16Float 可过滤）、2 = 折射 RT、
// 3 = scene_depth（R32Float 非过滤 textureLoad）、4 = 过滤采样器
// （ClampToEdge——波纹扰动把投影 uv 推出界时钳边）。
//
// 波形纯函数镜像契约：water_single_wave / water_evaluate 与 `src/water.rs`
// 的 gerstner_single_wave / gerstner_evaluate **逐式镜像（同式同序，两侧
// 必须同步修改）**——4 波固定序求和、振幅 / 波长衰减数组逐值一致；CPU
// 单测即波形形状的解析锚点（CPU↔GPU 不做位级等价声明，sin/cos 库实现
// 不同；同侧同输入的位级一致由两侧各自的确定性测试守护）。
//
// 诚实限制（ARCHITECTURE §8.50）：捕获不含水体自身（无递归）、不含透明
// / 贴花 / 粒子；水像素不写 scene_depth（下一帧雾 / AO 对水面用其后地面
// 深度——近似）；相机入水无特殊处理；网格边缘波高不收缩（边缘露缝是
// 接受项）。

// 参数（240B，POD；与 renderer.rs 的 WaterParamsRaw 逐字段对齐）。
struct WaterParams {
    view_proj: mat4x4<f32>,     // 主相机 VP（含 TAA 抖动，与主 pass 同口径）
    refl_view_proj: mat4x4<f32>, // 镜像相机 VP（proj × view × 反射矩阵）
    eye: vec4<f32>,             // xyz 主相机眼点，w 保留
    light_dir: vec4<f32>,       // xyz 表面 → 光源单位方向，w 保留
    light_color: vec4<f32>,     // rgb 方向光色 × 强度，w 保留
    wave_a: vec4<f32>,          // x 主波振幅, y 主波波长, z 相速度, w 陡度 Q
    wave_b: vec4<f32>,          // x 半边长 X, y 半边长 Z, z 波形时间 t, w 吸收特征深度
    shallow: vec4<f32>,         // rgb 浅水色，w 保留
    deep: vec4<f32>,            // rgb 深水色，w 保留
    weather: vec4<f32>,         // x = 涟漪强度（R92，Weather.ripple——雨打
                                // 水面法线扰动；0 = 分支不执行，逐位不变），
                                // yzw 保留
};

@group(0) @binding(0) var<uniform> params: WaterParams;
@group(0) @binding(1) var refl_tex: texture_2d<f32>;
@group(0) @binding(2) var refr_tex: texture_2d<f32>;
// 本帧场景深度副本（R32Float 线性视距，textureLoad 整数纹素读取——与
// 雾 / DOF / MB 的深度消费同款；水下厚度 = 该值 − 水面片元视距 clip.w）。
@group(0) @binding(3) var scene_depth: texture_2d<f32>;
@group(0) @binding(4) var samp: sampler;

const WATER_WAVE_COUNT: u32 = 4u;
const WATER_F0: f32 = 0.02;
const WATER_SPEC_POWER: f32 = 240.0;
// 反射采样的波纹扰动幅度（uv 比例——法线水平分量推偏投影坐标，
// 平水（法线 +Y）时精确为 0，确定性不受影响）
const WATER_REFLECT_DISTORT: f32 = 0.04;

struct WaterVertexInput {
    @location(0) pos_xz: vec2<f32>,
};

struct WaterVertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

// 逐波常量（与 src/water.rs 的 WAVE_DIRECTIONS / WAVE_LENGTH_DECAY /
// WAVE_AMPLITUDE_DECAY 逐值同步——修改必须两侧同步）：45° 起步等角四向。
fn water_wave_constants(i: u32) -> vec3<f32> {
    // 返回 (dir.x, dir.y, 波长缩短比)；振幅衰减另由 water_wave_amp 取
    var dirs = array<vec2<f32>, 4>(
        vec2<f32>(0.70710677, 0.70710677),
        vec2<f32>(-0.70710677, 0.70710677),
        vec2<f32>(-0.70710677, -0.70710677),
        vec2<f32>(0.70710677, -0.70710677),
    );
    var lens = array<f32, 4>(1.0, 0.55, 0.35, 0.25);
    let d = dirs[i];
    return vec3<f32>(d.x, d.y, lens[i]);
}

// 逐波振幅衰减（与 src/water.rs 的 WAVE_AMPLITUDE_DECAY 逐值同步）。
fn water_wave_amp(i: u32) -> f32 {
    var amps = array<f32, 4>(1.0, 0.5, 0.35, 0.25);
    return amps[i];
}

// 非零安全归一化（CPU 镜像 water.rs::normalize_safe——零向量返回零，
// 退化参数下不产生 NaN，两侧行为一致）。
fn water_normalize_safe(v: vec3<f32>) -> vec3<f32> {
    let l = length(v);
    return select(vec3<f32>(0.0), v / l, l > 0.0);
}

// 四波 Gerstner 求和（CPU 镜像 water.rs::gerstner_evaluate +
// gerstner_single_wave——同式同序，固定波序 0..4、固定求和序；单波主体
// 内联在循环里：k = 2π/λ、ω = k·speed·√(λ/λ₀)（深水色散锚定主波速度）、
// φ = k·(d·p) − ω·t、位移 = (Q_i·A·d⊥·cosφ, A·sinφ)、法线累加 =
// (−d⊥·kA·cosφ, −Q_i·kA·sinφ)，陡度均分 Q_i = Q/(k·A·4)——Q ≤ 1 时
// 每波单项 ≤ 1/4，法线 y 分量恒正（不卷绕界，CPU 单测锁定））。
// 法线经 out_norm 出参（WGSL 无元组返回的惯例替代）。
fn water_evaluate(
    p: vec2<f32>,
    t: f32,
    amplitude: f32,
    wave_length: f32,
    speed: f32,
    choppiness: f32,
    out_norm: ptr<function, vec3<f32>>,
) -> vec3<f32> {
    var disp = vec3<f32>(0.0);
    var acc = vec3<f32>(0.0, 1.0, 0.0);
    for (var i = 0u; i < WATER_WAVE_COUNT; i = i + 1u) {
        let cst = water_wave_constants(i);
        let dir = vec2<f32>(cst.x, cst.y);
        let len = wave_length * cst.z;
        let amp = amplitude * water_wave_amp(i);
        let k = 6.2831855 / len;
        let omega = k * speed * sqrt(len / wave_length);
        let phase = k * dot(dir, p) - omega * t;
        let s = sin(phase);
        let c = cos(phase);
        let steep = choppiness / (k * amp * 4.0);
        disp = disp + vec3<f32>(
            steep * amp * dir.x * c,
            amp * s,
            steep * amp * dir.y * c,
        );
        let wa = k * amp;
        acc = acc + vec3<f32>(-dir.x * wa * c, -steep * wa * s, -dir.y * wa * c);
    }
    (*out_norm) = water_normalize_safe(acc);
    return vec3<f32>(p.x + disp.x, disp.y, p.y + disp.z);
}

// ---- 天气涟漪（R92）：weather.rs::ripple_gradient 的 WGSL 镜像 ----
// （standard.wgsl 持有同式另一份拷贝——WGSL 无 include 的双份镜像纪律；
// 水面侧的时间源 = wave_b.z，与波形同一时钟——雨打水面与湿地表涟漪
// 同相。整数格哈希跨平台精确；sin/cos 两侧库实现不同，CPU↔GPU 不做
// 位级等价声明，公式锚点在 weather.rs 单测。）

fn weather_hash(a: u32, b: u32) -> vec2<u32> {
    var h = a * 0x9E3779B1u + b * 0x85EBCA77u;
    h = h ^ (h >> 13u);
    h = h * 0xC2B2AE3Du;
    h = h ^ (h >> 16u);
    let g = h * 0x27D4EB2Fu + 0x5BD1E995u;
    return vec2<u32>(h, g);
}

fn weather_fraction(z: u32) -> f32 {
    return f32(z >> 8u) * (1.0 / 16777216.0);
}

fn weather_ripple_gradient(p: vec2<f32>, t: f32) -> vec2<f32> {
    let cell = floor(p / 0.4);
    var grad = vec2<f32>(0.0);
    for (var dz = -1; dz <= 1; dz = dz + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            // 格坐标取整走 i32（f32→i32 范围内截断良定义，i32→u32 位
            // 重解释）——负格坐标与 Rust 侧 as i32 + as u32 逐位一致。
            let cx = i32(cell.x) + dx;
            let cz = i32(cell.y) + dz;
            let h = weather_hash(u32(cx), u32(cz));
            let fx = weather_fraction(h.x);
            let fy = weather_fraction(h.x ^ h.y);
            let fp = weather_fraction(h.y);
            let center = (cell + vec2<f32>(f32(dx), f32(dz))
                + vec2<f32>(0.15 + 0.7 * fx, 0.15 + 0.7 * fy)) * 0.4;
            let t0 = fp * 1.3;
            var age = t - t0;
            age = age - 1.3 * floor(age / 1.3);
            if (age < 0.6) {
                let d = p - center;
                let r = length(d);
                if (r > 1e-5) {
                    let u = (r - 0.85 * age) / 0.09 + 1.0;
                    if (u > 0.0 && u < 2.0) {
                        let decay = 1.0 - age / 0.6;
                        let dhdu = 0.01 * decay * 3.14159265 * cos(3.14159265 * u);
                        grad = grad + (dhdu / 0.09) * (d / r);
                    }
                }
            }
        }
    }
    return grad;
}

@vertex
fn vs_main(in: WaterVertexInput) -> WaterVertexOutput {    // 归一化格点 [−1,1]² → 世界 XZ（half_extent 展开——同一份静态网格
    // 服务任意尺寸的水面）
    let planar = in.pos_xz * vec2<f32>(params.wave_b.x, params.wave_b.y);
    var normal = vec3<f32>(0.0, 1.0, 0.0);
    let p = water_evaluate(
        planar,
        params.wave_b.z,
        params.wave_a.x,
        params.wave_a.y,
        params.wave_a.z,
        params.wave_a.w,
        &normal,
    );
    var out: WaterVertexOutput;
    out.clip = params.view_proj * vec4<f32>(p, 1.0);
    out.world_pos = p;
    out.normal = normal;
    return out;
}

@fragment
fn fs_main(in: WaterVertexOutput) -> @location(0) vec4<f32> {
    // 屏幕坐标（本帧 HDR 与折绘制同口径；v = 0.5 − 0.5·ndc.y 与纹理
    // 行序一致——NDC y=+1 写进第 0 行，与雾 / 天空的采样约定相同）
    let ndc = in.clip.xy / in.clip.w;
    let screen_uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    var n = normalize(in.normal);
    // 雨打水面涟漪（R92）：强度 > 0 时梯度扰动法线（Fresnel / 反射
    // 扰动 / 太阳镜面全吃到——spec 闪点是主要观感）；时间源与波形同钟
    //（wave_b.z）。0 = 分支不执行，逐位不变。
    if (params.weather.x > 0.0) {
        let g = weather_ripple_gradient(in.world_pos.xz, params.wave_b.z) * params.weather.x;
        n = normalize(n + vec3<f32>(g.x, 0.0, g.y));
    }
    let v = normalize(params.eye.xyz - in.world_pos);

    // Fresnel（Schlick，F0 = 0.02 水的物理值——垂直入射 ≈ 2% 反射，
    // 掠射趋 1）
    let f = WATER_F0 + (1.0 - WATER_F0) * pow(1.0 - max(dot(n, v), 0.0), 5.0);

    // 反射：镜像相机把水面片元投影回反射捕获图（镜像图 = 世界从镜像
    // 相机看到的样子，片元处的镜面反射色就是该投影点的捕获内容），
    // 法线水平分量做小幅波纹扰动（ClampToEdge 吸收出界）
    let rc = params.refl_view_proj * vec4<f32>(in.world_pos, 1.0);
    let r_ndc = rc.xy / rc.w;
    let r_uv = vec2<f32>(r_ndc.x * 0.5 + 0.5, 0.5 - r_ndc.y * 0.5)
        + vec2<f32>(n.x, n.z) * WATER_REFLECT_DISTORT;
    let refl = textureSample(refl_tex, samp, r_uv).rgb;

    // 折射 + 深度吸收：水下厚度 = 本帧 scene_depth（线性视距）− 水面
    // 片元视距（clip.w，与 scene_depth 的 clip.w 同口径）。透过率
    // T = exp(−厚度/特征深度)（Beer-Lambert 标量式；谱选择性 v1 用
    // 浅 / 深双色渐变承担——厚度 0 = 岸边直接见底，≥ 数倍特征深度
    // 趋深水色。天空像素深度 = 远平面清空值 ⇒ T ≈ 0 = 深水色）。
    let dims = vec2<f32>(textureDimensions(scene_depth));
    let dims_i = vec2<i32>(dims);
    let px = clamp(
        vec2<i32>(floor(screen_uv * dims)),
        vec2<i32>(0, 0),
        dims_i - vec2<i32>(1, 1),
    );
    let ground = textureLoad(scene_depth, px, 0).r;
    let thick = max(ground - in.clip.w, 0.0);
    let t = exp(-thick / max(params.wave_b.w, 1e-3));
    let body = mix(params.deep.rgb, params.shallow.rgb, t);
    let refr = textureSample(refr_tex, samp, screen_uv).rgb;
    let refracted = mix(body, refr, t);

    // 太阳镜面高光（Blinn-Phong，× Fresnel 折扣——掠射更亮）
    let h = normalize(params.light_dir.xyz + v);
    let spec = pow(max(dot(n, h), 0.0), WATER_SPEC_POWER) * params.light_color.rgb * f;

    let color = mix(refracted, refl, f) + spec;
    // a = 1：水面片元是完整合成结果（折射 RT 已含水下内容），alpha
    // 混合在此退化为替换（与「水面后的透明 / 粒子被覆盖」的 v1 语义
    // 一致，见 §8.50）
    return vec4<f32>(color, 1.0);
}
