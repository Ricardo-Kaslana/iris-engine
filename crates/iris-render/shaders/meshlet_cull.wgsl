// meshlet 剔除 compute 着色器（R85）——虚拟几何体 V2 的逐簇视锥 + 背面锥
// 判定与前缀和稳定紧凑化。两个 entry point / 两条管线：
//
//   main_cull    ：一线程一（实例, 簇）对——包围球视锥判定 + 背面锥判定，
//                  写存活标志与紧凑载荷（无簇间交互 ⇒ 逐簇布尔稳定）；
//   main_compact ：单工作组（256 线程 × 每线程顺序扫 64 个 flag）两遍
//                  扫描——段总和 Hillis-Steele 包含式扫描得段基址，第二遍
//                  按最终槽位写紧凑列表与存活总数。
//
// 会话内逐位确定性的根基（路线图裁定，ARCHITECTURE §4.41）：
// - **u32 整数加法结合精确** ⇒ 前缀和无论扫描树形状结果恒等（与浮点
//   扫描的本质区别）；紧凑序 = 全局簇号升序（stable compaction）⇒
//   绘制序确定——写槽位由前缀值决定、与线程完成顺序无关，无原子；
// - 剔除谓词逐簇独立（无簇间交互）⇒ 逐簇布尔稳定；
// - dispatch 形状只由（实例数, 最大簇数）决定（CPU 每帧常量算出）。
//
// CPU 镜像契约（crates/iris-render/src/meshlet.rs，两侧必须同步修改）：
// - 平面提取 = `frustum.rs::Frustum::from_view_proj` 的逐式镜像
//   （Gribb-Hartmann，z ∈ [0,1] 深度约定，平面按法线长度归一化）；
// - 背面锥 = `meshlet::cluster_is_backfacing` 的逐式镜像（阈值 =
//   −sin(acos(cutoff))，严格小于才剔除，cutoff ≤ 0 永不剔除）；
// - 紧凑化 = `meshlet::chunked_prefix_compact` 的逐式镜像。
//
// 簇元数据词布局影子契约：`MeshletClusterRaw`（48 B = 12×u32，见
// meshlet.rs 模块头「GPU storage 词布局」）——w0 vertex_offset、
// w1 index_offset、w2 vertex_count|triangle_count<<8|reserved<<16、
// w3..w5 cone_axis、w6 cone_cutoff_cos、w7..w10 bounds、w11 error。
// `cluster_raw_word_layout` 单测锁定词下标。

const PLANE_EPSILON: f32 = 1.1920929e-7; // f32::EPSILON（与 frustum.rs 同值）

// 剔除参数（96 B uniform，与 Rust 侧 MeshletCullParamsRaw 逐字段对齐）。
struct CullParams {
    view_proj: mat4x4<f32>, // 本帧主相机 VP（与 Globals.view_proj 同源）
    eye: vec4<f32>,         // xyz = 相机世界坐标（背面锥视线原点），w 保留
    counts: vec4<u32>,      // x = 本帧实例数，y = 本帧全局簇总数，zw 保留
};

@group(0) @binding(0) var<uniform> params: CullParams;
// 簇元数据 arena（12 词/簇，注册期写、只读）
@group(0) @binding(1) var<storage, read> cluster_words: array<u32>;
// 本帧实例表（双缓冲槽位之一；176 B/条，与 Rust MeshletInstanceEntryRaw 对齐）
@group(0) @binding(2) var<storage, read> instances: array<InstanceEntry>;
// 存活标志（0/1）、紧凑载荷（inst<<16|local）、紧凑列表、存活总数
@group(0) @binding(3) var<storage, read_write> flags: array<u32>;
@group(0) @binding(4) var<storage, read_write> payload: array<u32>;
@group(0) @binding(5) var<storage, read_write> compact: array<u32>;
@group(0) @binding(6) var<storage, read_write> survive_count: array<u32>;

// 单个 meshlet 实例条目（11 × vec4 = 176 B；全 vec4 字段天然 16 B 对齐）。
struct InstanceEntry {
    model0: vec4<f32>,
    model1: vec4<f32>,
    model2: vec4<f32>,
    model3: vec4<f32>,
    // 模型矩阵逆转置的法线矩阵（每列补零——与标准实例 InstanceRaw 同构，
    // CPU 侧直接复用 InstanceRaw::from_matrix 产出的值 ⇒ 与常规路径逐位同源）
    normal0: vec4<f32>,
    normal1: vec4<f32>,
    normal2: vec4<f32>,
    // x = 顶点 arena 基址，y = 索引 arena 基址，z = 簇元数据 arena 基址，
    // w = 本实例簇数
    base_counts: vec4<u32>,
    // x = 本帧全局簇号基（实例前缀和——同一网格被多实例引用时簇身份按
    // （实例, 局部号）枚举），yzw 保留
    frame_base: vec4<u32>,
    // 网格包围盒（簇球心归一化基）：xyz = min / extent，w 保留
    bounds_min: vec4<f32>,
    bounds_extent: vec4<f32>,
};

fn cluster_word(ci: u32, k: u32) -> u32 {
    return cluster_words[ci * 12u + k];
}

// Gribb-Hartmann 行提取（列主序矩阵的第 r 行 = 各列向量的第 r 分量拼合）
fn vp_row(vp: mat4x4<f32>, r: i32) -> vec4<f32> {
    return vec4<f32>(vp[0][r], vp[1][r], vp[2][r], vp[3][r]);
}

@compute
@workgroup_size(64, 1, 1)
fn main_cull(@builtin(global_invocation_id) gid: vec3<u32>) {
    let inst_slot = gid.y;
    if (inst_slot >= params.counts.x) {
        return;
    }
    let local = gid.x;
    let e = instances[inst_slot];
    if (local >= e.base_counts.w) {
        return; // 越界线程早退（dispatch 按「最大簇数 / 64」恒量算出）
    }
    let ci = e.frame_base.x + local; // 本帧全局簇号
    let mc = e.base_counts.z + local; // 簇元数据 arena 号

    // ---- 包围球：归一化球心反归一化 → 模型变换 → 半径按最大列缩放 ----
    // （与 collect_instances 的实例包围球口径同式：中心 transform_point3、
    // 半径 × 最大轴缩放——非均匀缩放下保守放大）
    let bounds = vec4<f32>(
        bitcast<f32>(cluster_word(mc, 7u)),
        bitcast<f32>(cluster_word(mc, 8u)),
        bitcast<f32>(cluster_word(mc, 9u)),
        bitcast<f32>(cluster_word(mc, 10u)),
    );
    let center_local = e.bounds_min.xyz + bounds.xyz * e.bounds_extent.xyz;
    let model = mat4x4<f32>(e.model0, e.model1, e.model2, e.model3);
    let center_world = (model * vec4<f32>(center_local, 1.0)).xyz;
    let scale = max(length(e.model0.xyz), max(length(e.model1.xyz), length(e.model2.xyz)));
    let radius = bounds.w * scale;

    // ---- 视锥：6 平面（Gribb-Hartmann，frustum.rs 同式；任一平面
    //      dist < −radius ⇒ 球整体在外）----
    let r0 = vp_row(params.view_proj, 0);
    let r1 = vp_row(params.view_proj, 1);
    let r2 = vp_row(params.view_proj, 2);
    let r3 = vp_row(params.view_proj, 3);
    var planes = array<vec4<f32>, 6>(
        r3 + r0, // left
        r3 - r0, // right
        r3 + r1, // bottom
        r3 - r1, // top
        r3,      // near（z ∈ [0,1] 约定）
        r3 - r2, // far
    );
    var survive = true;
    for (var i = 0; i < 6; i = i + 1) {
        let p = planes[i];
        let len = length(p.xyz);
        if (len > PLANE_EPSILON) {
            // 提取式与 frustum.rs 同式：仅当法线非退化才归一化后判定
            if (dot(p.xyz, center_world) / len + p.w / len < -radius) {
                survive = false;
            }
        }
    }

    // ---- 背面锥：视线方向与轴夹角越过 90° + acos(cutoff) ⇒ 整簇背面
    //      （cluster_is_backfacing 逐式镜像；cutoff ≤ 0 含全向退化锥 =
    //      永不剔除；眼点在球内时保守可见）----
    if (survive) {
        let cutoff = bitcast<f32>(cluster_word(mc, 6u));
        if (cutoff > 0.0) {
            let axis_local = vec3<f32>(
                bitcast<f32>(cluster_word(mc, 3u)),
                bitcast<f32>(cluster_word(mc, 4u)),
                bitcast<f32>(cluster_word(mc, 5u)),
            );
            let to_model = mat3x3<f32>(e.model0.xyz, e.model1.xyz, e.model2.xyz);
            let axis_world = normalize(to_model * axis_local);
            let to_eye = params.eye.xyz - center_world;
            let dist = length(to_eye);
            let sin_tc = sqrt(max(0.0, 1.0 - cutoff * cutoff));
            // 相等点判存活（≥ 为保守侧）；dist = 0（眼点在球心）保守可见
            let front = select(true, dot(to_eye / max(dist, 1e-9), axis_world) >= -sin_tc, dist > 0.0);
            survive = survive && front;
        }
    }

    flags[ci] = select(0u, 1u, survive);
    // 载荷 = (实例槽 << 16) | 簇局部号（各 ≤ 65535 / 256，容量契约见
    // MAX_MESHLET_CLUSTERS 与实例上限）——紧凑化按全局簇号升序原样搬运，
    // 绘制端解包后经实例表取顶点 / 索引基址做 vertex pull。
    payload[ci] = (inst_slot << 16u) | (local & 0xFFFFu);
}

// ---- 稳定紧凑化：单工作组两遍扫描（chunked_prefix_compact 逐式镜像）----
//
// 256 线程 × 每线程 64 flag = 16384 容量（MAX_MESHLET_CLUSTERS，const
// 断言锁定）。第一遍顺序累加段内存活数 → 共享内存树形扫描段总和 →
// 第二遍按「段基址（排他前缀）+ 段内游标」把载荷写进最终槽位。全程
// 无原子：槽位由前缀值唯一决定，与线程完成顺序无关 ⇒ 紧凑序 = 全局
// 簇号升序（stable），u32 加法精确 ⇒ 扫描树形状不影响结果。

const THREADS: u32 = 256u;
const CHUNK: u32 = 64u;

var<workgroup> scan_scratch: array<u32, 256>;

@compute
@workgroup_size(256, 1, 1)
fn main_compact(@builtin(local_invocation_index) lid: u32) {
    let base = lid * CHUNK;
    var run = 0u;
    for (var k = 0u; k < CHUNK; k = k + 1u) {
        let g = base + k;
        if (g < params.counts.y) {
            run = run + (flags[g] & 1u);
        }
    }
    // Hillis-Steele 包含式扫描（双缓冲由 barrier + 单 scratch 轮转实现：
    // 每步先写本线程当前值、读左邻上一步值）
    var incl = run;
    for (var d = 1u; d < THREADS; d = d << 1u) {
        scan_scratch[lid] = incl;
        workgroupBarrier();
        if (lid >= d) {
            incl = incl + scan_scratch[lid - d];
        }
        workgroupBarrier();
    }
    // 第二遍：排他前缀 = 包含式 − 自身段和；存活者升序写紧凑列表
    var slot = incl - run;
    for (var k = 0u; k < CHUNK; k = k + 1u) {
        let g = base + k;
        if (g < params.counts.y && (flags[g] & 1u) == 1u) {
            compact[slot] = payload[g];
            slot = slot + 1u;
        }
    }
    // 末线程写全局存活总数（其包含式前缀 = 全部段和之总）
    if (lid == THREADS - 1u) {
        survive_count[0] = incl;
    }
}
