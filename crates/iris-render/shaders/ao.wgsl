// GTAO 环境光遮蔽 pass（R77）：主 pass **之前**的全屏 pass，读**上一帧**
// 主相机深度副本（scene_depth，R32Float 线性视距——与 SSR 同款「上帧」
// 语义：本帧 scene_depth pass 编码在本 pass 之后，同一编码器按序执行，
// 本 pass 读到的恒为上一帧内容），按「切片水平角」法估算逐像素环境可见
// 度，写半分辨率 R16Float 可见度图（R 通道；缺席帧由渲染器清成中性 1.0，
// standard.wgsl 按 @builtin(position) 采样后乘在间接光上）。
//
// 算法（每像素）：
// 1. 由 (纹素中心 uv, d) 重建视空间位置（DirectX RH：clip.w = −view_z，
//    见 post::ao_view_pos——CPU 镜像经 glam 透视正反算锁定）；
// 2. 法线 = ±1 纹素深度差分重建位置的叉积（中心差分，无导数内建——
//    纯 textureLoad，确定性同源）；
// 3. AO_SLICES 个切片绕视轴均匀展开，方向按像素哈希 dither 旋转
//    （post::ao_dither 同一整数哈希——交错式抖动把切片量化噪声打碎成
//    高频颗粒）；每切片沿正负两方向各 AO_STEPS 步世界半径搜索，逐样本
//    记录水平角正弦的最大值（切平面下方/半径外/越界样本给负值，不参与
//    竞争）；
// 4. 遮蔽 = 切片平均 0.5·(sinθ↑ + sinθ↓)，可见度 = (1 − 遮蔽)^intensity。
//
// 深度 = 远平面清空值（天空）的像素与样本一律无遮蔽（天空不参与、也不
// 接收）；采样越出屏幕的样本直接视为无效（钳制读取会引入边界假水平角）。
// 诚实限制清单见 ARCHITECTURE §8（掠射角低估、深度断裂法线光晕、光照
// 贴图叠加过暗、无时域累积）。

// 参数（32 B，POD；与 Rust 侧 AoParamsRaw 逐字段对齐）。
struct AoParams {
    // x: 搜索半径（米）, y: 强度指数, z: tan(fov_y/2), w: 宽高比
    scalars: vec4<f32>,
    // x: 远平面（scene_depth 的天空清空值）, y/z/w: 保留
    extra: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: AoParams;
// 上帧场景深度副本：R32Float 非过滤——textureLoad 整数纹素读取，
// 不依赖 float32-filterable 扩展（与 DOF / MB / SSR 的深度消费同款）。
@group(0) @binding(1) var scene_depth: texture_2d<f32>;

const AO_SLICES: u32 = 4u;
const AO_STEPS: u32 = 8u;

struct AoVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// 三顶点全屏三角形（与 vignette.wgsl 相同；uv 与纹理行序 / 帧缓冲坐标
// 同向，standard.wgsl 按 @builtin(position) 采样可见度图时口径一致）
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> AoVertexOutput {
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
    var out: AoVertexOutput;
    out.position = vec4<f32>(positions[index], 0.0, 1.0);
    out.uv = uvs[index];
    return out;
}

// (uv, 线性视距) → 视空间位置。CPU 镜像 post::ao_view_pos 经 glam 的
// directx 透视正反算互锁（重建与光栅化同口径的解析证明见该函数单测）。
fn ao_view_pos(uv: vec2<f32>, view_d: f32) -> vec3<f32> {
    let ndc = uv * 2.0 - vec2<f32>(1.0);
    return vec3<f32>(
        ndc.x * view_d * params.scalars.w * params.scalars.z,
        ndc.y * view_d * params.scalars.z,
        -view_d,
    );
}

// 像素坐标 → [0, 1) 确定性抖动（CPU 镜像 post::ao_dither——u32 运算
// 回绕语义两侧位级一致）。纯像素坐标函数：无状态、无时域依赖。
fn ao_dither(px: vec2<i32>) -> f32 {
    var h = u32(px.x) * 73856093u ^ u32(px.y) * 19349663u;
    h = (h ^ (h >> 13u)) * 1274126177u;
    h = h ^ (h >> 16u);
    return f32(h & 0x00FFFFFFu) / 16777216.0;
}

fn clamp_px(q: vec2<i32>, dims: vec2<i32>) -> vec2<i32> {
    return clamp(q, vec2<i32>(0, 0), dims - vec2<i32>(1, 1));
}

// 单样本对水平角的贡献：样本仰角正弦 × 距离衰减。切平面下方 / 半径外 /
// 天空 / 越界样本给负值——在调用方的 max() 里不可能抬高水平线（CPU 镜像
// post::ao_sample_sin 的着色器版，差异仅在深度读取与越界裁决）。
fn ao_sample(q_uv: vec2<f32>, dims: vec2<f32>, p: vec3<f32>, n: vec3<f32>) -> f32 {
    let dims_i = vec2<i32>(dims);
    let raw = vec2<i32>(floor(q_uv * dims));
    if (raw.x < 0 || raw.y < 0 || raw.x >= dims_i.x || raw.y >= dims_i.y) {
        // 越界样本：无效（不遮蔽）。钳制读取会用边界纹素 depth 配界外
        // 纹素中心位置，制造不存在的表面几何——直接弃权是标准 GTAO 卫生。
        return -1.0;
    }
    let d = textureLoad(scene_depth, raw, 0).r;
    if (d >= params.extra.x) {
        return -1.0; // 天空样本：不遮蔽
    }
    // 重建位置必须取**深度纹素中心**：floor(q_uv·dims) 落在的纹素，其
    // 中心与 q_uv 有最多半纹素的系统偏差——斜面上这会把样本抬离/压入
    // 表面（中心点同理），给平坦面注入 ~1e-2 量级的假仰角（实测半径
    // 0.2 m 时遮蔽 ~5%，表现为大面积假暗化）。纹素中心配纹素深度后，
    // 同一平面上的样本仰角精确为 0（f32 舍入量级）。
    let q_c = (vec2<f32>(raw) + vec2<f32>(0.5)) / dims;
    let vv = ao_view_pos(q_c, d) - p;
    let dist = length(vv);
    let r = params.scalars.x;
    let falloff = clamp(1.0 - (dist / r) * (dist / r), 0.0, 1.0);
    if (falloff <= 0.0) {
        return -1.0; // 半径外：不参与水平角竞争
    }
    return dot(vv, n) / max(dist, 1e-4) * falloff;
}

@fragment
fn fs_main(in: AoVertexOutput) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(scene_depth));
    let dims_i = vec2<i32>(dims);
    let px = clamp_px(vec2<i32>(floor(in.uv * dims)), dims_i);
    let d_c = textureLoad(scene_depth, px, 0).r;
    if (d_c >= params.extra.x) {
        // 天空像素：无遮蔽（可见度 1）
        return vec4<f32>(1.0, 0.0, 0.0, 1.0);
    }
    // 中心点与样本同口径：深度纹素中心（半纹素错配的假仰角见 ao_sample）
    let uv_c = (vec2<f32>(px) + vec2<f32>(0.5)) / dims;
    let p = ao_view_pos(uv_c, d_c);
    // 法线：±1 纹素深度差分重建位置的叉积。屏幕 +y（纹理行增）对应
    // 视空间 −y（uv 与行序同向），故「下减上 × 右减左」给出朝向视者的
    // 法线（面向相机的墙 ≈ +z_view）。长度退化（相邻像素全为天空/平坦）
    // 时回退视线方向——保守的「无遮蔽」法线。
    let p_l = ao_view_pos((vec2<f32>(clamp_px(px - vec2<i32>(1, 0), dims_i)) + vec2<f32>(0.5)) / dims, textureLoad(scene_depth, clamp_px(px - vec2<i32>(1, 0), dims_i), 0).r);
    let p_r = ao_view_pos((vec2<f32>(clamp_px(px + vec2<i32>(1, 0), dims_i)) + vec2<f32>(0.5)) / dims, textureLoad(scene_depth, clamp_px(px + vec2<i32>(1, 0), dims_i), 0).r);
    let p_u = ao_view_pos((vec2<f32>(clamp_px(px - vec2<i32>(0, 1), dims_i)) + vec2<f32>(0.5)) / dims, textureLoad(scene_depth, clamp_px(px - vec2<i32>(0, 1), dims_i), 0).r);
    let p_d = ao_view_pos((vec2<f32>(clamp_px(px + vec2<i32>(0, 1), dims_i)) + vec2<f32>(0.5)) / dims, textureLoad(scene_depth, clamp_px(px + vec2<i32>(0, 1), dims_i), 0).r);
    let raw_n = cross(p_d - p_u, p_r - p_l);
    var n = vec3<f32>(0.0, 0.0, 1.0);
    if (length(raw_n) > 1e-10) {
        n = raw_n / length(raw_n);
    }

    // 切片水平角累积（CPU 镜像 post::{ao_slice_occlusion, ao_visibility}）
    let dither = ao_dither(px);
    var occ: f32 = 0.0;
    for (var s = 0u; s < AO_SLICES; s = s + 1u) {
        let phi = (f32(s) + dither) * 3.14159265 / f32(AO_SLICES);
        let dir_m = vec2<f32>(cos(phi), sin(phi));
        var sin_up = -1.0;
        var sin_down = -1.0;
        for (var j = 1u; j <= AO_STEPS; j = j + 1u) {
            let t = f32(j) / f32(AO_STEPS);
            // 世界偏移 dir_m · (radius·t) 换算 uv 偏移（每 uv 的世界尺寸
            // 随中心深度线性放大；CPU 镜像 post::ao_uv_offset）
            let world_per_uv = vec2<f32>(
                2.0 * d_c * params.scalars.z * params.scalars.w,
                2.0 * d_c * params.scalars.z,
            );
            let off = dir_m * (params.scalars.x * t) / world_per_uv;
            let sp = ao_sample(uv_c + off, dims, p, n);
            let sm = ao_sample(uv_c - off, dims, p, n);
            if (sp > sin_up) {
                sin_up = sp;
            }
            if (sm > sin_down) {
                sin_down = sm;
            }
        }
        occ = occ + 0.5 * (max(sin_up, 0.0) + max(sin_down, 0.0));
    }
    occ = occ / f32(AO_SLICES);
    let vis = pow(clamp(1.0 - occ, 0.0, 1.0), params.scalars.y);
    return vec4<f32>(vis, 0.0, 0.0, 1.0);
}
