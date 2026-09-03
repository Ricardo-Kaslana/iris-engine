// 贴花着色器（R39）：盒投影贴花——正向前向管线的弹孔 / 血迹 / 地面标记。
//
// 代理几何 = 单位盒的 ±Z 两块端盖（见 decal.rs proxy_geometry）：顶点经
// 逐实例世界盒矩阵变换到世界；片元把**插值世界位**经逆世界盒矩阵（CPU
// 预计算——着色器不做矩阵求逆，逐片元求逆更不可行）变回盒局部 local，
// |local| > 0.5 即盒外 discard（盒裁剪，防御端盖边缘的浮点误差），
// UV = local.xy + 0.5 采样贴花纹理 × tint，标准 alpha 混合输出。
// 逆矩阵经顶点输出 location 1..4 传入片元（实例输入只在顶点阶段可见；
// 端盖是平面，插值世界位与逐点变换在数学上都精确，取规范口径——片元
// 侧变换）。
//
// 深度只测不写（LessEqual）：贴花端盖与被贴表面共面（推荐摆放见
// decal.rs 模块文档），等深必须通过（Less 会因浮点抖动闪烁）；不写深度
// 则后续透明 pass / 粒子 pass 照常按不透明深度裁决遮挡。
//
// v1 无光照：输出 = 纹理 × tint，不参与 PBR / 阴影 / IBL（限制与理由见
// decal.rs 模块文档）。不进阴影 pass（贴花是表面装饰而非几何）。

// 只用 view_proj——声明精简 Globals（绑定的是同一份全局 uniform 缓冲，
// 短于缓冲的结构体合法，skybox.wgsl / particle.wgsl 同款做法）
struct Globals {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

// 贴花纹理 + 采样器（组 1 按纹理分批绑定：同纹理的贴花实例合并为一个
// draw，批内仅切换本绑定组）
@group(1) @binding(0) var decal_texture: texture_2d<f32>;
@group(1) @binding(1) var decal_sampler: sampler;

// 实例输入：location 1..4 = 世界盒矩阵（列主序 4 列），5..8 = 逆矩阵
//（CPU 预计算），9 = tint 乘色（含整体 alpha 缩放）。
struct DecalInstance {
    @location(1) model0: vec4<f32>,
    @location(2) model1: vec4<f32>,
    @location(3) model2: vec4<f32>,
    @location(4) model3: vec4<f32>,
    @location(5) inv_model0: vec4<f32>,
    @location(6) inv_model1: vec4<f32>,
    @location(7) inv_model2: vec4<f32>,
    @location(8) inv_model3: vec4<f32>,
    @location(9) tint: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    // 插值世界位（片元据此变回盒局部做盒裁剪与 UV 推导）
    @location(0) world_pos: vec3<f32>,
    // 逆世界盒矩阵 4 列（透传给片元；端盖平面下插值与逐点变换皆精确）
    @location(1) inv_model0: vec4<f32>,
    @location(2) inv_model1: vec4<f32>,
    @location(3) inv_model2: vec4<f32>,
    @location(4) inv_model3: vec4<f32>,
    // tint 乘色（含整体 alpha 缩放）
    @location(5) tint: vec4<f32>,
};

@vertex
fn vs_main(@location(0) position: vec3<f32>, inst: DecalInstance) -> VertexOutput {
    let model = mat4x4<f32>(inst.model0, inst.model1, inst.model2, inst.model3);
    let world_pos = model * vec4<f32>(position, 1.0);
    var out: VertexOutput;
    out.clip_position = globals.view_proj * world_pos;
    out.world_pos = world_pos.xyz;
    out.inv_model0 = inst.inv_model0;
    out.inv_model1 = inst.inv_model1;
    out.inv_model2 = inst.inv_model2;
    out.inv_model3 = inst.inv_model3;
    out.tint = inst.tint;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let inv_model = mat4x4<f32>(
        in.inv_model0, in.inv_model1, in.inv_model2, in.inv_model3,
    );
    let local = (inv_model * vec4<f32>(in.world_pos, 1.0)).xyz;
    // 盒裁剪：端盖几何的 local.z 恒 = ±0.5（恰在盒面上），inv·model 往返
    // 的浮点误差会让个别采样点落到 0.5000…1 一侧——无容差的边界比较会
    // 随矩阵舍入方向整片 / 条带状误 discard（实测：同一场景仅改变盒尺寸
    // 即从「正常」变「整枚消失」）。取 1e-4 盒局部容差（1 m 盒对应
    // 0.1 mm，远大于 f32 噪声、远小于视觉相关），盒外仍然严格丢弃。
    if (any(abs(local) > vec3<f32>(0.5 + 1e-4))) {
        discard;
    }
    // 盒局部 XY ∈ [-0.5, 0.5]² → UV ∈ [0, 1]²（UV 即贴花平面坐标，
    // 投影方向 Z 不参与）
    let uv = local.xy + vec2<f32>(0.5, 0.5);
    let tex = textureSample(decal_texture, decal_sampler, uv);
    // 乘色含整体 alpha 缩放：输出 alpha = tex.a × tint.a
    return tex * in.tint;
}
