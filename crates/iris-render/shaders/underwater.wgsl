// 水下 pass（R94）：后处理链上的一级全屏 pass，插在**体积雾之后、DOF 之前**
// （空气雾是「场景内的气溶胶」、水是「相机所在介质」，两者同开时先雾后水在
// 物理层次上正确；且下游 DOF / MB / 泛光应作用于已被水吸收后的颜色）。
//
// 编码条件 = `UnderwaterFog` 资源在场**且**相机眼点低于选中水面。资源缺席
// 或相机在水上 ⇒ 本 pass 整段不入链（零 write_buffer、零 draw、链槽位拓扑
// 与 R93 逐字段相同 ⇒ 像素与命令流逐位不变）。
//
// 组布局复用 `post_depth_layout`（与雾 / DOF / MB 同构，零新布局）：
//   0 = UnderwaterParams uniform（176 B）、1 = 链色（src）、2 = 过滤采样器、
//   3 = scene_depth（R32Float 非过滤，textureLoad——链上同帧消费）。
//
// **水下列长是解析算的**（设计裁定 1）：由视线方向 `dir` 与水面高度差直接
// 求交，`column = min(head / dir.y, d)`（视线向上时），否则 `= d`。这样水面
// 像素（其 scene_depth 是水面**之后**的远平面）不会被误算成整段深水，从水下
// 仰望的 Snell 窗因此可见——而水 / 海洋 pass 依旧「深度只测不写、水像素不写
// scene_depth」（§8.50⑥ 原样维持，既有像素锁一串不动）。代价：波面被当成
// 常量平面 `height`（浪高不参与列长，§8.73 声明）。
//
// 深度口径沿用 R77 教训：位置重建与配深取**同一纹素中心**。
//
// 纯函数镜像契约：`uw_caustic` / `uw_column` 与 `src/underwater.rs` 的
// `caustic_intensity` / `water_column_distance` **逐式镜像**（同式同序，两侧
// 同步修改），由该模块测试 `wgsl_underwater_mirrors_cpu_formulas` 源码层看门。
// CPU↔GPU 不做位级等价声明（sin/cos 库实现不同）。
//
// 诚实限制（§8.73）：焦散是屏幕空间加性项（无朝向判定、不参与光照阴影、无
// 能量守恒）；抖动是 uv 偏移不是折射光追；无体积光柱；跨水面硬切换。

struct UnderwaterParams {
    inv_view_proj: mat4x4<f32>, // 主相机（proj × view）⁻¹——NDC → 世界（射线方向）
    eye: vec4<f32>,             // xyz 相机世界位，w 保留
    sun_dir: vec4<f32>,         // xyz 单位太阳方向（表面 → 光源），w 保留
    sun_color: vec4<f32>,       // rgb 太阳辐照色（线性 HDR），w 保留
    a: vec4<f32>,               // rgb 水体吸收色, w 吸收特征深度（米）
    b: vec4<f32>,               // x 太阳内散射, y Snell 窗补光, z 焦散强度, w 抖动幅度
    c: vec4<f32>,               // x 波形时间 t, y z_far（scene_depth 天空清空值）, zw 保留
    d: vec4<f32>,               // x 水面高度 plane_y, yzw 保留
};

@group(0) @binding(0) var<uniform> params: UnderwaterParams;
@group(0) @binding(1) var src_color: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var scene_depth: texture_2d<f32>;

struct UwVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// 三顶点全屏三角形（与 fog.wgsl / ao.wgsl 相同；uv 与纹理行序同向）
@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> UwVertexOutput {
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
    var out: UwVertexOutput;
    out.position = vec4<f32>(positions[index], 0.0, 1.0);
    out.uv = uvs[index];
    return out;
}

// 焦散强度 ∈ [0, 1]（CPU 镜像 `underwater.rs::caustic_intensity`——同式同序：
// 两个方向相位场互扭 + 正部三次幂 ⇒ 窄亮筋 / 宽暗区，有界由构造保证）。
fn uw_caustic(p: vec2<f32>, t: f32) -> f32 {
    let q = p + vec2(sin(t) * 0.35, cos(t) * 0.28);
    let a = cos(q.x * 2.1 + sin(q.y * 1.3 + t));
    let b = cos(q.y * 1.9 - sin(q.x * 1.7 - t * 1.3));
    let c = max((a + b) * 0.5, 0.0);
    return c * c * c;
}

// 水下列长（CPU 镜像 `underwater.rs::water_column_distance`——同式同序）。
// rise = 视线竖直抬升量（= dir.y · d），head = 水面在眼点上方的量。
fn uw_column(d: f32, rise: f32, head: f32) -> f32 {
    if (rise > 1e-6 && head > 0.0) {
        return min(d * head / rise, d);
    }
    return d;
}

// Beer-Lambert 标量透过率（CPU 镜像 `absorption_through`，下限同 1e-3）。
fn uw_absorb(column: f32, absorb_depth: f32) -> f32 {
    return exp(-column / max(absorb_depth, 1e-3));
}

@fragment
fn fs_main(in: UwVertexOutput) -> @location(0) vec4<f32> {
    let dims = vec2<f32>(textureDimensions(scene_depth));
    let dims_i = vec2<i32>(dims);
    let px = clamp(
        vec2<i32>(floor(in.uv * dims)),
        vec2<i32>(0, 0),
        dims_i - vec2<i32>(1, 1),
    );
    let uv_c = (vec2<f32>(px) + vec2<f32>(0.5)) / dims;
    let ndc = uv_c * 2.0 - vec2<f32>(1.0);
    // 射线方向 = 远平面点重建（与 fog.wgsl 同式；scene_depth 存的是**线性
    // 视距**，故命中点 = eye + dir·d，无需再解 NDC z）
    let far4 = params.inv_view_proj * vec4<f32>(ndc, 1.0, 1.0);
    let dir = normalize(far4.xyz / far4.w - params.eye.xyz);
    let d = textureLoad(scene_depth, px, 0).r;
    let head = params.d.x - params.eye.y;
    let column = uw_column(d, dir.y * d, head);
    let tr = uw_absorb(column, params.a.w);

    // 屏幕空间折射抖动：wobble = 0 ⇒ 偏移恒 0 ⇒ 采样点与不抖逐位同值
    var src_uv = in.uv;
    if (params.b.w > 0.0) {
        let t = params.c.x;
        let w = params.b.w;
        src_uv = in.uv + vec2<f32>(
            sin(in.uv.y * 42.0 + t * 1.7) * w,
            cos(in.uv.x * 37.0 - t * 1.3) * w,
        );
    }
    let src = textureSample(src_color, samp, src_uv).rgb;

    // 吸收 + 水色（全链**重心式**，不写 mix——端点逐位可断言，R92 纪律）
    var out = src * tr + params.a.rgb * (1.0 - tr);

    // 太阳内散射：路径辐射的标准形式 `L_ins · (1 − T)`，其中 L_ins 取
    // **水体色 × 太阳色**——水分子散射后光带着水的颜色（首稿漏了这层
    // 着色，直接把太阳辐照乘进去 ⇒ 3.0 × 0.35 ≈ 0.84 的白覆盖整幅，
    // 验收图被洗白；水体的散射亮度本就应当是「暗蓝绿的一层辉光」）。
    // 朝太阳方向略强：一阶朝向项（不做 HG——水下前向峰被多次散射抹平）。
    if (params.b.x > 0.0) {
        let toward = 0.75 + 0.25 * max(dot(dir, params.sun_dir.xyz), 0.0);
        out += params.a.rgb * params.sun_color.rgb * (params.b.x * toward * (1.0 - tr));
    }

    // Snell 窗：视线越朝上、离水面越近越亮（column 大 ⇒ tr 小 ⇒ 暗）。
    // 同样带水色着色，但比内散射亮一档（那是太阳盘方向的直接透射）。
    if (params.b.y > 0.0) {
        let up = max(dir.y, 0.0);
        out += params.a.rgb * params.sun_color.rgb * (params.b.y * up * up * tr);
    }

    // 焦散：只落在**几何命中像素**（天空像素 d = z_far 不参与），随列长衰减。
    // 这条**不带水色**——它是透过波面聚焦的日光，落在水底上是太阳色。
    if (params.b.z > 0.0 && d < params.c.y) {
        let hit_xz = params.eye.xz + dir.xz * d;
        let c = uw_caustic(hit_xz, params.c.x);
        out += params.sun_color.rgb * (params.b.z * c * tr);
    }

    return vec4<f32>(out, 1.0);
}
