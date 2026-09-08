// 遮挡查询代理（R53）：把逐实例的世界 AABB 展开为 8 角点立方体。
// 无片段着色器（pass 无颜色附件，光栅化 + 深度测试即产生查询计数）；
// 管线侧深度只测不写、cull None——背面角同样计数，是保守方向
// （宁可把「擦边可见」误判为可见，绝不误剔）。论证见 occlusion.rs 模块文档。

struct OcclusionParams {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> params: OcclusionParams;

// location 0 = 单位立方体角点（±0.5）；1/2 = 实例世界 AABB 的 min/max。
@vertex
fn vs_main(
    @location(0) corner: vec3<f32>,
    @location(1) bmin: vec4<f32>,
    @location(2) bmax: vec4<f32>,
) -> @builtin(position) vec4<f32> {
    // 角点分量 +0.5 ∈ {0,1} 恰为 min..max 的插值权重
    let t = corner + vec3<f32>(0.5);
    let world = mix(bmin.xyz, bmax.xyz, t);
    return params.view_proj * vec4<f32>(world, 1.0);
}
