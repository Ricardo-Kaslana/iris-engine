// 海洋 v2（R93）：无限海面 = 相机跟随双层网格 + CPU 预烘关键帧频谱位移图。
//
// **本文件与 `water.wgsl` 各拼接一份 `water_common.wgsl` 成两个独立 shader
// module**（`pipeline.rs` 的 `water_shader` / `water_ocean_shader`），因此两路
// 各自声明自己的 `@group(0)`，互不共享全局命名；池塘侧的 entry point 与像素
// 锁一字不动。Gerstner 求和 / 归一化 / 波形常量来自 common 段（单源、按构造
// 与池塘同式）。
//
// 链位与池塘同构：scene_depth 之后、TAA / 后处理链之前，HDR MSAA Load +
// resolve，深度只测不写，cull None。
//
// 组 0 布局（独立组，不扩 globals——R61/R77/R78/R81 先例）：
//   0 = OceanParams uniform（272 B，vertex | fragment）
//   1 = 反射捕获图（与池塘同一张镜像相机产物，Rgba16Float 可过滤）
//   2 = 关键帧位移图（`texture_2d_array<Rgba16Float>`，层号 = 关键帧号）
//   3 = scene_depth（R32Float 非过滤，textureLoad）
//   4 = ClampToEdge 过滤采样器（反射扰动出界钳边）
//   5 = Repeat 过滤采样器（位移图周期平铺——图样每 16 m 自重复）
//
// 契约常量与 CPU 侧 `src/ocean.rs` 逐值同步，看门 = `ocean.rs` 的
// `wgsl_ocean_contract_matches_cpu`（R61/R90/R92 源码层纪律）；
// `ocean_detail_fade` 与时间重心式插值同样是 CPU 公式的镜像拷贝。
//
// 诚实限制（ARCHITECTURE §8.72）：细节层只进近场环（远场步距 4 m 承载不了
// 0.25 m 尺度）；可见范围 = far_extent（默认 192 m）；不做折射图（无限海无
// 池底可折射）；波峰不碎（无 whitecap / 泡沫贴图，只有一个按高度的一阶微亮）；
// 不接 R92 雨打水面涟漪（那是池塘 / 湿地表的通道，海面纹素尺度已被谱细节占满）。

// ---- 契约常量（与 src/ocean.rs 逐值同步）----
const OCEAN_TILE_PERIOD: f32 = 16.0;
const OCEAN_FRAMES: u32 = 16u;
const OCEAN_TEXEL_H: u32 = 0u;
const OCEAN_TEXEL_DX: u32 = 1u;
const OCEAN_TEXEL_DZ: u32 = 2u;

struct OceanParams {
    view_proj: mat4x4<f32>,      // 主相机 VP（含 TAA 抖动，与主 pass 同口径）
    refl_view_proj: mat4x4<f32>, // 镜像相机 VP（反射捕获投影）
    eye: vec4<f32>,              // xyz 主相机眼点，w 保留
    light_dir: vec4<f32>,        // xyz 表面 → 光源单位方向
    light_color: vec4<f32>,      // rgb 方向光色 × 强度
    wave_a: vec4<f32>,           // x 主波振幅, y 主波波长, z 相速度, w 陡度 Q
    wave_b: vec4<f32>,           // x 本环半边长, y 网格步距, z 波形时间 t, w 吸收特征深度
    ocean_a: vec4<f32>,          // x/z 本环锚点世界 XZ, y 保留, w 细节 RMS 波高（米）
    ocean_b: vec4<f32>,          // x 关键帧 a, y 帧间权重 u, z 几何细节权重, w 法线细节权重
    shallow: vec4<f32>,          // rgb 浅水色
    deep: vec4<f32>,             // rgb 深水色
};

@group(0) @binding(0) var<uniform> ocean: OceanParams;
@group(0) @binding(1) var refl_tex: texture_2d<f32>;
// 只有近场环采样它（远场两权重恒 0 ⇒ 分支整段不执行，零采样）
@group(0) @binding(2) var ocean_map: texture_2d_array<f32>;
@group(0) @binding(3) var scene_depth: texture_2d<f32>;
@group(0) @binding(4) var samp_clamp: sampler;
@group(0) @binding(5) var samp_wrap: sampler;

struct OceanVertexOutput {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) height: f32,
};

// 世界 XZ → 图 uv（除以周期即 Repeat 域坐标；负坐标由平铺语义处理，与 CPU
// 侧 `ocean_texel_center` 的「纹素中心 = (col+0.5)·cell」同址）。
fn ocean_texel_uv(p: vec2<f32>) -> vec2<f32> {
    return p / OCEAN_TILE_PERIOD;
}

// 关键帧间的重心式取样（CPU 镜像 `ocean.rs::ocean_blend`——**不得写 mix**：
// 线性插值式 `a + (b−a)·u` 在 u = 1 处实测有 2 ULP 残差，而重心式在 u = 0/1
// 两端点逐位复现单帧值，「停在关键帧上」才是可逐位断言的锚。层号回绕 ⇒ 末帧
// 的下一帧是第 0 帧，循环无缝是索引性质）。
fn ocean_detail_at(p: vec2<f32>) -> vec4<f32> {
    let uv = ocean_texel_uv(p);
    let fa = i32(ocean.ocean_b.x);
    let fb = (fa + 1) % i32(OCEAN_FRAMES);
    let u = ocean.ocean_b.y;
    // 数组纹理：坐标维 = 非数组维（vec2 的 uv）+ 显式 array_index + 显式
    // level（VS 无导数，level 必须给——这里恒 0，图未烘 mip，见 §8.72）。
    let h_a = textureSampleLevel(ocean_map, samp_wrap, uv, fa, 0.0);
    let h_b = textureSampleLevel(ocean_map, samp_wrap, uv, fb, 0.0);
    return h_a * (1.0 - u) + h_b * u;
}

// 近场细节位移的半径淡出（CPU 镜像 `ocean.rs::ocean_detail_fade`，同式同序：
// 0.6R 内全额、R 外归零、中间 smoothstep 下降）。淡出终点 = 远场起点，而远场
// 几何位移恒 0 ⇒ 两环交界无竖直台阶。
fn ocean_detail_fade(radius: f32, half_extent: f32) -> f32 {
    let edge = max(half_extent, 1e-3);
    let x = clamp((radius - 0.6 * edge) / (0.4 * edge), 0.0, 1.0);
    return 1.0 - x * x * (3.0 - 2.0 * x);
}

// 细节法线：纹素梯度（解析求和，非差分）× 波高缩放 = 无量纲斜率，与 Gerstner
// 法线的水平分量**累加后**统一归一化（一阶叠加，不做旋转复合）。
fn ocean_detail_normal(n: vec3<f32>, grad: vec2<f32>, scale: f32) -> vec3<f32> {
    return water_normalize_safe(vec3<f32>(
        n.x - grad.x * scale,
        n.y,
        n.z - grad.y * scale,
    ));
}

@vertex
fn vs_ocean(in: WaterVertexInput) -> OceanVertexOutput {
    // 归一化格点 [−1,1]² → 世界 XZ：按本环半边长展开、再平移到量化锚点
    // （锚点在 CPU 侧量化到本环步距的整数格 ⇒ 相机亚步移动不改顶点位型）。
    let ext = ocean.wave_b.x;
    let planar = in.pos_xz * vec2<f32>(ext, ext) + ocean.ocean_a.xz;
    var normal = vec3<f32>(0.0, 1.0, 0.0);
    // Gerstner 涌浪层（与池塘同式，同文件 common 段）
    var p = water_evaluate(
        planar,
        ocean.wave_b.z,
        ocean.wave_a.x,
        ocean.wave_a.y,
        ocean.wave_a.z,
        ocean.wave_a.w,
        &normal,
    );
    // 频谱细节层：只在近场环（远场 vertex_detail = 0 ⇒ 整段不执行）
    if (ocean.ocean_b.z > 0.0) {
        let d = ocean_detail_at(p.xz);
        let amp = ocean.ocean_a.w;
        let w = ocean_detail_fade(length(p.xz - ocean.ocean_a.xz), ext) * ocean.ocean_b.z;
        p = vec3<f32>(p.x, p.y + d.x * amp * w, p.z);
        normal = ocean_detail_normal(normal, vec2<f32>(d[OCEAN_TEXEL_DX], d[OCEAN_TEXEL_DZ]), amp * w);
    }
    var out: OceanVertexOutput;
    out.clip = ocean.view_proj * vec4<f32>(p, 1.0);
    out.world_pos = p;
    out.normal = normal;
    out.height = p.y;
    return out;
}

@fragment
fn fs_ocean(in: OceanVertexOutput) -> @location(0) vec4<f32> {
    let ndc = in.clip.xy / in.clip.w;
    let screen_uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    var n = normalize(in.normal);
    // 近场逐片元再吃一次同一张图：顶点级细节法线被 0.5 m 网格插值摊平，
    // 片端补回亚格尺度的镜面闪点。远场 normal_detail = 0 ⇒ 分支不执行。
    if (ocean.ocean_b.w > 0.0) {
        let d = ocean_detail_at(in.world_pos.xz);
        n = ocean_detail_normal(
            n,
            vec2<f32>(d[OCEAN_TEXEL_DX], d[OCEAN_TEXEL_DZ]),
            ocean.ocean_a.w * ocean.ocean_b.w,
        );
    }
    let v = normalize(ocean.eye.xyz - in.world_pos);
    // Fresnel（Schlick，F0 = 0.02 水的物理值——与池塘同常量）
    let f = WATER_F0 + (1.0 - WATER_F0) * pow(1.0 - max(dot(n, v), 0.0), 5.0);

    // 反射（镜像相机投影 + 法线水平分量扰动，ClampToEdge 吸收出界）
    let rc = ocean.refl_view_proj * vec4<f32>(in.world_pos, 1.0);
    let r_ndc = rc.xy / rc.w;
    let r_uv = vec2<f32>(r_ndc.x * 0.5 + 0.5, 0.5 - r_ndc.y * 0.5)
        + vec2<f32>(n.x, n.z) * WATER_REFLECT_DISTORT;
    let refl = textureSample(refl_tex, samp_clamp, r_uv).rgb;

    // 水体本色：scene_depth 给「水面之后」的线性视距（本帧同链，与池塘同款
    // 深度吸收）。远海读到远平面清空值 ⇒ 厚度大 → 深水色；近岸读到海床 ⇒
    // 浅水色。**不做折射图**（无限海无池底可折射，R94 水下补）。
    let dims = vec2<f32>(textureDimensions(scene_depth));
    let px = clamp(
        vec2<i32>(floor(screen_uv * dims)),
        vec2<i32>(0, 0),
        vec2<i32>(dims) - vec2<i32>(1, 1),
    );
    let ground = textureLoad(scene_depth, px, 0).r;
    let thick = max(ground - in.clip.w, 0.0);
    let tr = exp(-thick / max(ocean.wave_b.w, 1e-3));
    let body = ocean.deep.rgb * (1.0 - tr) + ocean.shallow.rgb * tr;

    // 太阳镜面（Blinn-Phong × Fresnel 折扣——掠射更亮）
    let h = normalize(ocean.light_dir.xyz + v);
    let spec = pow(max(dot(n, h), 0.0), WATER_SPEC_POWER) * ocean.light_color.rgb * f;
    // 波峰微亮：按水面高度的一阶渐变（v1 唯一的「碎波」替身，无泡沫贴图）。
    // 用主波振幅归一 ⇒ 与涌浪同尺度，细节层不会单独把画面刷白。
    let crest = smoothstep(0.25, 0.9, in.height / max(ocean.wave_a.x, 1e-3)) * 0.06;

    // 全部合成走**重心式**（不写 mix——与 CPU 镜像同纪律，端点逐位可断言）
    let color = body * (1.0 - f) + refl * f + spec + vec3<f32>(crest);
    return vec4<f32>(color, 1.0);
}
