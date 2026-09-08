// FXAA（快速近似抗锯齿，R53）：后处理链尾部的 LDR 域单 pass 边缘平滑。
// 输入 = 色调映射 blit 写入的中间 LDR 纹理（sRGB 目标，硬件采样时解码为
// 线性浮点），输出 = 最终 sRGB 目标（硬件重新编码）。链位在 blit 之后、
// UI 之前——HUD 文字不经本 pass，保持锐利（见 ARCHITECTURE §4.25）。
//
// 算法 = FXAA 3.11 console 质量档的紧凑实现（Lottes 原版简化）：
// ①8 邻域亮度求局部对比度，低于阈值（随亮度自适应：
//   max(edge_threshold_min, edge_threshold × lumaMax)）→ 早退**逐位透传**
//   中心采样（非边缘像素不改写，是「开 FXAA 的画面与基线仅边缘有别」的
//   依据）；②梯度判定边缘方向，沿边缘一步 + 反向半步两点采样，按亮度
//   带宽裁决取点或取中混合。显示域亮度：样本经 pow(1/2.2) 近似回显示
//   值再算（原文的边缘检测假设显示空间；混合本身在线性域做）。
// 未做（诚实清单 §8.32）：方向搜索（span search）细化与二阶亚像素项。

@group(0) @binding(0) var<uniform> fxaa_params: vec4<f32>;
// x: edge_threshold（相对）, y: edge_threshold_min（绝对）,
// zw: 1/宽、1/高（纹素尺寸）
@group(0) @binding(1) var ldr_texture: texture_2d<f32>;
@group(0) @binding(2) var ldr_sampler: sampler;

struct FxaaVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// 三顶点全屏三角形（与 blit.wgsl 同款，越出屏幕部分被裁剪）
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> FxaaVertexOutput {
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
    var out: FxaaVertexOutput;
    out.position = vec4<f32>(positions[index], 0.0, 1.0);
    out.uv = uvs[index];
    return out;
}

// 显示域亮度：线性样本先 pow(1/2.2) 近似回显示值，再按 Rec.709 加权。
fn display_luma(c: vec3<f32>) -> f32 {
    let s = pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2));
    return dot(s, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@fragment
fn fs_main(in: FxaaVertexOutput) -> @location(0) vec4<f32> {
    let rcp = fxaa_params.zw;
    let uv = in.uv;
    let center = textureSample(ldr_texture, ldr_sampler, uv).rgb;
    let l_center = display_luma(center);
    let l_nw = display_luma(textureSample(ldr_texture, ldr_sampler, uv + vec2<f32>(-rcp.x, -rcp.y)).rgb);
    let l_ne = display_luma(textureSample(ldr_texture, ldr_sampler, uv + vec2<f32>(rcp.x, -rcp.y)).rgb);
    let l_sw = display_luma(textureSample(ldr_texture, ldr_sampler, uv + vec2<f32>(-rcp.x, rcp.y)).rgb);
    let l_se = display_luma(textureSample(ldr_texture, ldr_sampler, uv + vec2<f32>(rcp.x, rcp.y)).rgb);
    let l_n = display_luma(textureSample(ldr_texture, ldr_sampler, uv + vec2<f32>(0.0, -rcp.y)).rgb);
    let l_s = display_luma(textureSample(ldr_texture, ldr_sampler, uv + vec2<f32>(0.0, rcp.y)).rgb);
    let l_w = display_luma(textureSample(ldr_texture, ldr_sampler, uv + vec2<f32>(-rcp.x, 0.0)).rgb);
    let l_e = display_luma(textureSample(ldr_texture, ldr_sampler, uv + vec2<f32>(rcp.x, 0.0)).rgb);

    // ---- ①边缘判定与早退（低对比度 → 逐位透传中心样本）----
    var l_min = min(l_nw, min(l_ne, min(l_sw, l_se)));
    l_min = min(l_min, min(min(l_n, l_s), min(min(l_w, l_e), l_center)));
    var l_max = max(l_nw, max(l_ne, max(l_sw, l_se)));
    l_max = max(l_max, max(max(l_n, l_s), max(max(l_w, l_e), l_center)));
    // 阈值随亮度自适应：暗部用绝对下限，亮部要求相对对比度
    let threshold = max(fxaa_params.y, fxaa_params.x * l_max);
    if (l_max - l_min < threshold) {
        return vec4<f32>(center, 1.0);
    }

    // ---- ②边缘方向（亮度梯度，垂直于边缘走向）----
    let dir_x = 2.0 * (l_w + l_nw + l_sw - l_e - l_ne - l_se);
    let dir_y = 2.0 * (l_n + l_nw + l_ne - l_s - l_sw - l_se);
    // 缩减系数抑制平坦高噪区（FXAA 原版 dirReduce）；限幅 ±8 纹素防跑飞
    let dir_reduce = max((l_nw + l_ne + l_sw + l_se) * 0.25 * 0.125, 1.0 / 128.0);
    let rcp_dir_min = 1.0 / (min(abs(dir_x), abs(dir_y)) + dir_reduce);
    var dir = vec2<f32>(dir_x, dir_y) * rcp_dir_min;
    dir = clamp(dir, vec2<f32>(-8.0), vec2<f32>(8.0)) * rcp;

    // ---- ③沿边缘一步 + 反向半步两点采样，带宽裁决 ----
    let uv_a = uv + dir * (1.0 / 3.0 - 0.5);
    let uv_b = uv + dir * (2.0 / 3.0 - 0.5);
    let rgb_a = textureSample(ldr_texture, ldr_sampler, uv_a).rgb;
    let rgb_b = textureSample(ldr_texture, ldr_sampler, uv_b).rgb;
    let luma_a = display_luma(rgb_a);
    let luma_b = display_luma(rgb_b);
    if (luma_b < l_min || luma_b > l_max) {
        // B 点越出边缘亮度带 → 取更靠边缘中点的 A 点
        return vec4<f32>(rgb_a, 1.0);
    }
    // 两点都在带内 → 取中（半像素混合）
    return vec4<f32>((rgb_a + rgb_b) * 0.5, 1.0);
}
