// 壳层顶点着色器（R90）：实例化 shell fur——单网格 N 实例一次 draw，
// 层比 t = k/N 由实例数据携带（`fur.rs::ShellInstanceRaw`，loc 15）。
//
// 膨胀：`world_pos += normalize(world_normal) × height × t`——法线取
// **模型法线矩阵变换后归一**（非均匀缩放由法线矩阵正确处理）。层比
// 写入 `fur_aux.x` 传给片元（standard.wgsl fs_shaded 的 cutout 判定：
// `base.a < max(材质 cutoff, fur_aux.x)` 丢弃——密度贴图值 d 的毛在
// 第 ⌈d·N⌉ 层以上消失 = 毛尖渐隐）。
//
// 片元侧复用 standard.wgsl 的 `fs_main`——VertexOutput 与 standard.wgsl
// 逐字段同构（**影子契约**，四方同步见 standard.wgsl 同名结构）。
// Globals 用前缀声明（skybox / hair_vs 同款先例：只声明用到的字段，
// 字节布局与 standard.wgsl 前缀一致）。
//
// 蒙皮：v1 壳层不跟随骨骼（joint_mats 不绑定，蒙皮实体挂壳层的膨胀
// 按 TCP 侧模型矩阵算——诚实限制 §8，与 R91 毛发阴影一起处理）。

struct Globals {
    view_proj: mat4x4<f32>,
    eye_pos: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

// 与 standard.wgsl 的 VertexOutput 逐字段同构（影子契约）。
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) tangent: vec4<f32>,
    @location(4) uv2: vec2<f32>,
    @location(5) probe: vec4<f32>,
    @location(6) fur_aux: vec4<f32>,
    // 顶点色（影子契约）：壳层管线没有颜色顶点流，恒写白色（乘性恒等）。
    @location(7) color: vec4<f32>,
};

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) model0: vec4<f32>,
    @location(4) model1: vec4<f32>,
    @location(5) model2: vec4<f32>,
    @location(6) model3: vec4<f32>,
    @location(7) normal0: vec4<f32>,
    @location(8) normal1: vec4<f32>,
    @location(9) normal2: vec4<f32>,
    // 壳层参数（`fur.rs::ShellInstanceRaw`）：x = 层比 t ∈ (0,1]，
    // y = 毛高（世界单位），z/w 保留
    @location(15) shell: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    let model = mat4x4<f32>(in.model0, in.model1, in.model2, in.model3);
    let normal_mat = mat3x3<f32>(in.normal0.xyz, in.normal1.xyz, in.normal2.xyz);
    let world_normal = normalize(normal_mat * in.normal);
    // 沿法线膨胀：层比 × 毛高（k/N × height——顶皮满膨胀）
    let inflated = model * vec4<f32>(in.position, 1.0)
        + vec4<f32>(world_normal * (in.shell.y * in.shell.x), 0.0);
    var out: VertexOutput;
    out.clip_position = globals.view_proj * inflated;
    out.world_pos = inflated.xyz;
    out.world_normal = world_normal;
    out.uv = in.uv;
    out.tangent = vec4<f32>(0.0);
    out.uv2 = vec2<f32>(0.0);
    out.probe = vec4<f32>(0.0);
    // 层比传片元：cutout 阈值 = max(材质 cutoff, t)
    out.fur_aux = vec4<f32>(in.shell.x, 0.0, 0.0, 0.0);
    out.color = vec4<f32>(1.0);
    return out;
}
