// 景深（Depth of Field，R43）：薄透镜的简化近似——逐像素按圆混淆圈
// （CoC）半径在 HDR 上做 12-tap 泊松盘平均，聚焦带（CoC = 0）原样透传。
//
// 链位：全分辨率 HDR → HDR，在泛光**之前**（HDR 线性域——焦外亮源应
// 参与后续亮度提取，先在物理域混合、后做镜头合成）；位于运动模糊之前
// （两者同处相机级效果段，链序见 ARCHITECTURE §4.11 与 post.rs 模块
// 文档）。关闭时整段跳过，链与无 DOF 帧逐位一致。
//
// CoC 推导（简化薄透镜）：真实透镜中 CoC ∝ |d − d_focus| / d，简化为
// 线性归一化 + 带内清零：
//   coc_px = clamp(|d − focus| / range − 1, 0, 1) × max_radius
// d = 线性视距（view-Z，米，来自 scene_depth 副本的 R 通道），focus /
// range / max_radius 来自 DepthOfFieldSettings。清晰带（|d − focus| ≤
// range）内 CoC 恰为 0，走 select 原色透传分支——textureSample 在纹素
// 中心采样返回精确原值，输出逐位等于输入。
//
// 深度读取用 textureLoad（整数纹素坐标）：R32Float 不可过滤，绑定侧
// 声明 filterable = false，不依赖 float32-filterable 扩展（阴影贴图的
// 既有取舍）。
//
// 诚实边界：CoC 是逐像素常量的盘形平均，不区分前景/背景倾角、不做
// 孔径形状与色散；模糊采样不检查邻居深度（前景边缘会把背景色带进
// 焦内像素的采样盘，反之亦然——无逐 tap CoC 门控）。

// 参数 uniform（32B）：a = (聚焦距离, 清晰带半宽, 最大模糊半径/纹素, 0)；
// b = (深度副本 1/宽, 1/高, 0, 0)——把纹素半径换算成 UV 偏移。
struct DofParams {
    a: vec4<f32>,
    b: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: DofParams;
@group(0) @binding(1) var src_texture: texture_2d<f32>;
@group(0) @binding(2) var src_sampler: sampler;
@group(0) @binding(3) var depth_texture: texture_2d<f32>;

struct DofVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// 三顶点全屏三角形（与 blit.wgsl 相同）
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> DofVertexOutput {
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
    var out: DofVertexOutput;
    out.position = vec4<f32>(positions[index], 0.0, 1.0);
    out.uv = uvs[index];
    return out;
}

// 12-tap 泊松盘（单位圆内近似均匀分布；经典的圆盘采样偏移组）
const POISSON: array<vec2<f32>, 12> = array<vec2<f32>, 12>(
    vec2<f32>(-0.326, -0.406),
    vec2<f32>(-0.840, -0.074),
    vec2<f32>(-0.696, 0.457),
    vec2<f32>(-0.203, 0.621),
    vec2<f32>(0.962, -0.195),
    vec2<f32>(0.473, -0.480),
    vec2<f32>(0.519, 0.767),
    vec2<f32>(0.185, -0.893),
    vec2<f32>(0.507, 0.064),
    vec2<f32>(0.896, 0.412),
    vec2<f32>(-0.322, -0.933),
    vec2<f32>(-0.792, -0.598),
);

@fragment
fn fs_main(in: DofVertexOutput) -> @location(0) vec4<f32> {
    let center = textureSample(src_texture, src_sampler, in.uv).rgb;
    let dims = vec2<f32>(textureDimensions(depth_texture));
    let px = clamp(
        vec2<i32>(in.uv * dims),
        vec2<i32>(0, 0),
        vec2<i32>(dims) - vec2<i32>(1, 1),
    );
    let d = textureLoad(depth_texture, px, 0).r;
    // CoC（纹素）：清晰带内恰为 0（分子 ≤ 0 被 clamp 截断）
    let coc = clamp(
        abs(d - params.a.x) / max(params.a.y, 1e-6) - 1.0,
        0.0,
        1.0,
    ) * params.a.z;
    // 泊松盘平均（半径 = CoC 纹素 → UV）。循环边界恒定（uniform 控制
    // 流），CoC = 0 时所有 tap 命中同一纹素、平均无意义——用 select 走
    // 原色透传，保证聚焦带逐位无损。
    var sum = vec3<f32>(0.0);
    for (var i = 0; i < 12; i++) {
        let offset = POISSON[i] * coc * vec2<f32>(params.b.x, params.b.y);
        sum += textureSample(src_texture, src_sampler, in.uv + offset).rgb;
    }
    let blurred = sum / 12.0;
    // 半纹素以下半径不产生可见模糊，同样透传（亚纹素盘只放大采样噪声）
    return vec4<f32>(select(blurred, center, coc < 0.5), 1.0);
}
