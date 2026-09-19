// 水体波形的共享段（R93 拆出）：4 波 Gerstner 求和 + 非零安全归一化 +
// 波形 / 光学常量。**池塘（`water.wgsl`）与海洋（`water_ocean.wgsl`）共用
// 这一份**——由 `pipeline.rs` 在 Rust 侧拼接成两个独立 shader module
// （WGSL 无 include；拼接是「双份镜像 + 源码看门」范式的替代：单源、
// 按构造成立，无需看门。拼接产物各自只声明自己那一份 `@group(0)`，
// 因此两路可以各用一套绑定，而池塘侧的像素锁原值通过）。
//
// 与 CPU 侧 `src/water.rs::gerstner_single_wave` / `gerstner_evaluate`
// **逐式镜像**（同式同序，两侧必须同步修改）——CPU 单测即波形形状的解析
// 锚点；CPU↔GPU 不做位级等价声明（sin/cos 库实现不同）。

const WATER_WAVE_COUNT: u32 = 4u;
const WATER_F0: f32 = 0.02;
const WATER_SPEC_POWER: f32 = 240.0;
// 反射采样的波纹扰动幅度（uv 比例——法线水平分量推偏投影坐标，
// 平水（法线 +Y）时精确为 0，确定性不受影响）
const WATER_REFLECT_DISTORT: f32 = 0.04;

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

// 水面网格顶点输入（R81 定义、R93 拆进 common）：池塘与海洋两路共用同一份
// 归一化 [−1,1]² 静态网格（`uv` 属性目前闲置——图采样坐标由世界 XZ 推）。
struct WaterVertexInput {
    @location(0) pos_xz: vec2<f32>,
};
