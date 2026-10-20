// meshlet vertex-pull 绘制着色器（R85）——无顶点缓冲 / 无索引缓冲的
// instanced draw：`instance_index → 紧凑簇列表 → 簇元数据（storage）→
// storage 拉顶点 / 索引`（WebGPU vertex-pulling 标准形态；只读 storage
// 在顶点阶段合法）。
//
// **片元阶段 = standard.wgsl 的 fs_main 本体**（wgpu 允许管线的顶点 /
// 片元来自不同模块，接口按 location 匹配）。这是「PBR 求值共享」的最强
// 形态：零复制、零漂移——与常规网格路径共用同一条片元管线与同一份材质
// 绑定组语义，像素等价断言（快照 FNV 相等 / GPU 全量 vs 剔除逐字节）
// 的根据。顶点侧输出与 standard.wgsl 的 VertexOutput 逐字段同构。
//
// 实例常量的中性缺席口径（与常规路径「网格缺席该属性时垫零」完全一致）：
// - tangent = 0（容器不载切线 ⇒ 屏幕空间 TBN 路径，fs 侧按 xyz 全零
//   判定——与渲染器零填充垫同值同语义）；
// - uv2 = 0 + 默认材质 lightmap_mask = 0 ⇒ 光照贴图采样被 select 丢弃；
// - probe = 0（标志位 0）⇒ 间接漫反射走 irradiance_map 全局 env 兜底
//   （vertex-pull 管线没有 loc14 探针实例属性路径——诚实限制 §8.54）。
//
// 越界防御：每个实例恒发 MESHLET_CLUSTER_MAX_TRIANGLES × 3 = 384 个顶点
// 槽位，`vertex_index / 3 ≥ triangle_count` 的槽位输出三点同位的零面积
// 退化三角（光栅化无贡献）——单 instanced draw 的固定顶点数契约。
//
// 布局影子契约（与 meshlet_cull.wgsl / meshlet.rs 模块头同步）：
// - 簇元数据 = 12×u32 词（MeshletClusterRaw 48 B 逐字排布）；
// - 顶点 = array<vec4<f32>>，每顶点 2 词 [px,py,pz,nx] / [ny,nz,u,v]
//   （vertex_storage_words 的重排下标）；
// - 实例表 = InstanceEntry（176 B，与 meshlet_cull.wgsl 同一 struct——
//   两侧同步修改；本模块绑定的是「产出当前所绘列表的那一帧」的槽位）。

const MESHLET_CLUSTER_MAX_TRIANGLES: u32 = 128u;

// Globals 前缀声明（skybox.wgsl 同款并行布局影子契约：与 standard.wgsl
// 的 Globals 逐字段同序同型，guard 单测 pipeline.rs 锁定）。本模块只用
// view_proj，但绑定组与常规管线共享同一布局，struct 按前缀全量自持。
struct PointLight {
    position: vec4<f32>,
    color: vec4<f32>,
    shadow: vec4<f32>,
};

struct Globals {
    view_proj: mat4x4<f32>,
    eye_pos: vec4<f32>,
    light_dir: vec4<f32>,
    light_color: vec4<f32>,
    ambient: vec4<f32>,
    point_count: vec4<f32>,
    point_lights: array<PointLight, 8>,
    shadow_params: vec4<f32>,
    env_params: vec4<f32>,
    inv_view_proj: mat4x4<f32>,
    point_shadow_params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

@group(2) @binding(0) var<storage, read> cluster_words: array<u32>;
@group(2) @binding(1) var<storage, read> meshlet_vertices: array<vec4<f32>>;
@group(2) @binding(2) var<storage, read> meshlet_indices: array<u32>;
@group(2) @binding(3) var<storage, read> instances: array<InstanceEntry>;
@group(2) @binding(4) var<storage, read> compact: array<u32>;

// 与 meshlet_cull.wgsl 的 InstanceEntry 同一 struct（176 B，两侧同步修改）。
struct InstanceEntry {
    model0: vec4<f32>,
    model1: vec4<f32>,
    model2: vec4<f32>,
    model3: vec4<f32>,
    normal0: vec4<f32>,
    normal1: vec4<f32>,
    normal2: vec4<f32>,
    // x = 顶点 arena 基址，y = 索引 arena 基址，z = 簇元数据 arena 基址，
    // w = 本实例簇数
    base_counts: vec4<u32>,
    // x = 本帧全局簇号基（本模块不消费——紧凑载荷已含实例槽），yzw 保留
    frame_base: vec4<u32>,
    bounds_min: vec4<f32>,
    bounds_extent: vec4<f32>,
};

// 与 standard.wgsl 的 VertexOutput 逐字段同构（片元接口按 location 匹配）。
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) tangent: vec4<f32>,
    @location(4) uv2: vec2<f32>,
    @location(5) probe: vec4<f32>,
    // 毛发辅助通道（R90，影子契约——四方同步见 standard.wgsl 同名结构）：
    // meshlet 路径恒写 0（不进壳层裁剪语义）
    @location(6) fur_aux: vec4<f32>,
    // 顶点色（影子契约）：meshlet 容器不载颜色，恒写白色（乘性恒等）。
    @location(7) color: vec4<f32>,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
) -> VertexOutput {
    // 紧凑簇列表（ Culled 帧 = 上帧 compute 产物；全量兜底帧 = CPU 写入的
    // 恒等列表——与实例表槽位配对，见 renderer.rs meshlet 段）
    let entry = compact[instance_index];
    let inst_slot = entry >> 16u;
    let local = entry & 0xFFFFu;
    let e = instances[inst_slot];
    let ci = e.base_counts.z + local;

    let w0 = cluster_words[ci * 12u + 0u]; // vertex_offset
    let w1 = cluster_words[ci * 12u + 1u]; // index_offset
    let w2 = cluster_words[ci * 12u + 2u]; // vertex_count | triangle_count << 8
    let triangle_count = (w2 >> 8u) & 0xFFu;

    // 固定顶点数契约：超出本簇三角形数的槽位 = 三点同位零面积退化
    let tri = vertex_index / 3u;
    let corner = vertex_index % 3u;
    if (tri >= triangle_count) {
        var degenerate: VertexOutput;
        degenerate.clip_position = vec4<f32>(0.0, 0.0, 0.0, 1.0);
        degenerate.world_pos = vec3<f32>(0.0);
        degenerate.world_normal = vec3<f32>(0.0);
        degenerate.uv = vec2<f32>(0.0);
        degenerate.tangent = vec4<f32>(0.0);
        degenerate.uv2 = vec2<f32>(0.0);
        degenerate.probe = vec4<f32>(0.0);
        degenerate.fur_aux = vec4<f32>(0.0);
        degenerate.color = vec4<f32>(1.0);
        return degenerate;
    }

    // vertex pull：全局索引 = 索引 arena 基址 + 簇索引偏移 + 簇内三角角点；
    // 全局顶点 = 顶点 arena 基址 + 簇顶点偏移 + 簇内局部号
    let local_index = meshlet_indices[e.base_counts.y + w1 + tri * 3u + corner];
    let slot = (e.base_counts.x + w0 + local_index) * 2u;
    let pa = meshlet_vertices[slot];
    let pb = meshlet_vertices[slot + 1u];
    let position = pa.xyz;
    let normal = vec3<f32>(pa.w, pb.x, pb.y);
    let uv = pb.zw;

    // 变换：与 standard.wgsl vs_main 同式同序（model 列构造 → world →
    // clip / world_pos / world_normal）——像素等价断言的顶点侧根据。
    let model = mat4x4<f32>(e.model0, e.model1, e.model2, e.model3);
    let normal_mat = mat3x3<f32>(e.normal0.xyz, e.normal1.xyz, e.normal2.xyz);
    let world_pos = model * vec4<f32>(position, 1.0);
    var out: VertexOutput;
    out.clip_position = globals.view_proj * world_pos;
    out.world_pos = world_pos.xyz;
    out.world_normal = normal_mat * normal;
    out.uv = uv;
    // 中性缺席口径（见文件头）：无切线 / 无 UV2 / 无探针载荷
    out.tangent = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    out.uv2 = vec2<f32>(0.0, 0.0);
    out.probe = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    out.fur_aux = vec4<f32>(0.0);
    out.color = vec4<f32>(1.0);
    return out;
}
