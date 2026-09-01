// 粒子 billboard 着色器：4 顶点 triangle-strip 由 vertex_index 展开
//（无顶点缓冲），实例缓冲携带「世界中心 + 尺寸」与线性 RGBA。
// billboard = center + (right·x + up·y)·size——相机 right / up 从
// ParticleCamera 小 uniform（视图矩阵第 0 / 1 行，CPU 每帧写入）取得，
// quad 恒面向相机。片元做径向软衰减：UV 距中心越远 alpha 越小、quad
// 角落衰减到 0，避免「硬方块」感。
//
// 混合模式由两条管线区分（additive：SrcAlpha + One；alpha：标准
// ALPHA_BLENDING），片元输出相同——衰减进 alpha，加法混合下 SrcAlpha
// 直接调制成叠加亮度，alpha 混合下即常规软边精灵。深度只测不写、写
// 线性 HDR 目标（粒子 pass 位于透明 pass 之后、泛光之前，见 renderer.rs）。

// 只用 view_proj——声明精简 Globals（绑定的是同一份全局 uniform 缓冲，
// 短于缓冲的结构体合法，skybox.wgsl 同款做法）
struct Globals {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

// xyz: 相机 right 单位向量（视图矩阵第 0 行）, w: 保留
// xyz: 相机 up 单位向量（视图矩阵第 1 行）, w: 保留
struct ParticleCamera {
    right: vec4<f32>,
    up: vec4<f32>,
};

@group(1) @binding(0) var<uniform> cam: ParticleCamera;

// 实例输入：location 0 = (中心 xyz, 尺寸 w)，location 1 = 线性 RGBA
struct ParticleInstance {
    @location(0) center_size: vec4<f32>,
    @location(1) color: vec4<f32>,
};

struct ParticleVertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    // quad 内坐标（[-1,1]²，供片元径向衰减）
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(
    @builtin(vertex_index) index: u32,
    in: ParticleInstance,
) -> ParticleVertexOutput {
    // 4 顶点展开为两个三角形（triangle-strip）：左下 → 右下 → 左上 → 右上
    var corners = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    let c = corners[index];
    let world = in.center_size.xyz
        + (cam.right.xyz * c.x + cam.up.xyz * c.y) * in.center_size.w;
    var out: ParticleVertexOutput;
    out.clip_position = globals.view_proj * vec4<f32>(world, 1.0);
    out.uv = c;
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: ParticleVertexOutput) -> @location(0) vec4<f32> {
    // 径向软衰减：r² = u² + v²，falloff = (1 − r²)² ——中心 1、单位圆
    // 边缘 0、角落钳到 0（曲线写死，无需贴图）。RGB 不衰减，衰减只乘
    // alpha：additive（SrcAlpha, One）下等于调制叠加亮度形成光点，
    // alpha 混合下即软边精灵。
    let r2 = dot(in.uv, in.uv);
    var falloff = clamp(1.0 - r2, 0.0, 1.0);
    falloff *= falloff;
    return vec4<f32>(in.color.rgb, in.color.a * falloff);
}
