// 发带顶点着色器（R90）：相机面向屏幕空间展宽（camera-facing ribbon）。
//
// 输入 = groom 控制网格（`groom.rs::HAIR_VERTEX_LAYOUT`，loc 0–2）+
// 常规实例数据（`InstanceRaw`，loc 3–9 / 14）。片元侧复用
// standard.wgsl 的 `fs_main`（Kajiya-Kay 分支由材质 uniform 门控），
// 因此本文件的 VertexOutput 与 standard.wgsl 逐字段同构——**影子契约**
// （改一侧必须另一侧；看门 = pipeline.rs::
// vertex_output_fur_aux_matches_standard_layout）。
//
// 展宽式与 `particles_v2_draw.wgsl` 的拖尾条带同源（已被 R82 GPU 测试
// 验证的公式）：控制点投影 NDC → 屏幕空间法向（宽高比修正 + 单位化）
// → 按 1/w 展宽 ⇒ 恒定世界宽度。相邻段共享控制点顶点（位置 / 方向 /
// 侧别全同，`groom.rs` 单测锁定）⇒ 展宽结果逐位一致、关节无缝——
// 与粒子拖尾的「软边 alpha 掩盖关节」不同，发带是 cutout（不透明），
// 无缝必须由数据共享保证。
//
// 宽高比 / 1/tan(fov/2) 从 view_proj **列长推导**：
// `length(view_proj[1].xyz) = |proj_y|`、
// `length(view_proj[0].xyz) = |proj_y / aspect|`（view 旋转块列单位长
// ⇒ 复合后列长 = 投影对角元绝对值；TAA 抖动在平移列不污染）。不扩
// Globals（skybox 并行布局陷阱零触碰），不新增 uniform。
//
// 伪法线（Kajiya-Kay 的 IBL 间接项与高光移位方向用）：视线方向在
// 垂直于发丝切线平面上的投影，逐顶点计算、插值后片元归一——发带
// 窄条带上插值误差可忽略。发丝主方向走 tangent 通道（KK 直射光瓣）。

struct Globals {
    view_proj: mat4x4<f32>,
    eye_pos: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

// 与 standard.wgsl 的 VertexOutput 逐字段同构（影子契约，四方同步：
// standard / meshlet_draw / hair_vs / shell_vs——见 standard.wgsl 同名
// 结构的看门注释）。
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) tangent: vec4<f32>,
    @location(4) uv2: vec2<f32>,
    @location(5) probe: vec4<f32>,
    @location(6) fur_aux: vec4<f32>,
};

struct VertexInput {
    @location(0) position: vec3<f32>,
    // xyz = 控制点切线（归一），w = 该点发丝全宽（世界单位）
    @location(1) dir_width: vec4<f32>,
    // x = 沿丝参数 t ∈ [0,1]（根 0 梢 1），y = 侧别 ±1
    @location(2) t_side: vec4<f32>,
    @location(3) model0: vec4<f32>,
    @location(4) model1: vec4<f32>,
    @location(5) model2: vec4<f32>,
    @location(6) model3: vec4<f32>,
    @location(7) normal0: vec4<f32>,
    @location(8) normal1: vec4<f32>,
    @location(9) normal2: vec4<f32>,
    @location(14) probe: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    let model = mat4x4<f32>(in.model0, in.model1, in.model2, in.model3);
    let normal_mat = mat3x3<f32>(in.normal0.xyz, in.normal1.xyz, in.normal2.xyz);
    let world_root = model * vec4<f32>(in.position, 1.0);
    let t_world = normalize(normal_mat * in.dir_width.xyz);

    // 投影常量从 view_proj 列长推导（见文件头）：proj_y = 1/tan(fov/2)，
    // aspect = proj_y / proj_x
    let proj_y = length(globals.view_proj[1].xyz);
    let proj_x = length(globals.view_proj[0].xyz);
    let aspect = proj_y / max(proj_x, 1e-6);

    // 发丝切线方向在屏幕上的投影（root + dir 再投影，差即方向）
    let clip_root = globals.view_proj * world_root;
    let clip_tip = globals.view_proj
        * vec4<f32>(world_root.xyz + t_world, 1.0);
    // 屏幕空间法向（宽高比修正后单位化）——particles_v2_draw 同式
    var d2 = vec2<f32>(
        (clip_tip.x / clip_tip.w - clip_root.x / clip_root.w) * aspect,
        clip_tip.y / clip_tip.w - clip_root.y / clip_root.w,
    );
    let len2 = length(d2);
    d2 = d2 / max(len2, 1e-8);
    let perp = vec2<f32>(-d2.y, d2.x);
    let perp_ndc = vec2<f32>(perp.x / aspect, perp.y);
    // 恒定世界宽度：半宽 × proj_y / w（1/w 换算，端点即本控制点）
    let half_ndc = in.dir_width.w * 0.5 * proj_y / max(clip_root.w, 1e-6);
    let side = in.t_side.y;
    let ndc = clip_root.xy / max(clip_root.w, 1e-6) + perp_ndc * (side * half_ndc);

    // 伪法线：视线在垂直于发丝平面上的投影（片元归一）
    let view_dir = normalize(globals.eye_pos.xyz - world_root.xyz);
    let n_pseudo = normalize(view_dir - t_world * dot(view_dir, t_world));

    var out: VertexOutput;
    out.clip_position = vec4<f32>(ndc * clip_root.w, clip_root.z, clip_root.w);
    out.world_pos = world_root.xyz;
    out.world_normal = n_pseudo;
    out.uv = vec2<f32>(in.t_side.x, in.t_side.y);
    out.tangent = vec4<f32>(t_world, 1.0);
    out.uv2 = vec2<f32>(0.0, 0.0);
    out.probe = in.probe;
    out.fur_aux = vec4<f32>(0.0);
    return out;
}
