// 粒子 v2 拖尾渲染着色器（R82）：相机面向条带（ribbon）+ 头部 billboard。
//
// 每个存活粒子贡献 trail_length 个屏幕空间展宽四边形（固定顶点数 =
// 池 × trail_length × 6，顶点全由 vertex_index 派生、数据从 storage 读
// ——无顶点缓冲、无实例缓冲）：
// - 第 0 个四边形 = 头部 billboard（当前位置 + 寿命插值尺寸，v1
//   collect_render 同式插值），径向软衰减（particle.wgsl 同式）；
// - 第 1..trail_length−1 个 = 相邻两历史点连成的条带节：两端点投影到
//   NDC 后求屏幕空间法向、按 trail_width（世界单位，经 1/tan(fov/2)
//   与 1/w 换算成逐端点 NDC 偏移）双向展宽——恒定世界宽度；横向软边
//   （相邻节共享端点宽度位置，透明度在关节处连续），颜色取各端点
//   **记录时** age 的寿命插值色（沿历史按 age 比例渐变，头部关节与
//   头部四边形颜色连续），alpha 再乘沿历史线性淡出因子（尾部全透明
//   ——平滑收束，无硬截断）。
//
// 死粒子 / 历史不足 / 端点在相机身后 / 零长度节 → 全部顶点输出画面外
// 同点 = 零面积退化（不产生片元）——固定顶点数下按索引确定性消隐。
//
// 深度只测不写、HDR 目标（与 v1 粒子 pass 同位——透明之后、泛光之前，
// 不写 scene_depth）；两条管线仅混合不同（additive / alpha），自相交
// 不排序（加法交换律下无序；alpha 自遮挡可能有叠序痕迹——诚实限制）。
//
// 布局影子契约：DrawParams ↔ DrawParamsRaw（64 B）、SimParticle ↔
// particles_v2_sim.wgsl 同构（560 B）——改一侧必须同步 Rust 侧。

const TRAIL_MAX: u32 = 32u;

// 只用 view_proj——精简 Globals（particle.wgsl 同款做法，绑定同一份
// 全局 uniform 缓冲）
struct Globals {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

struct DrawParams {
    lifetime_size: vec4<f32>, // x = 单粒子寿命，y = 出生尺寸，z = 终点尺寸，w = 拖尾全宽（世界单位）
    color_start: vec4<f32>,   // 出生颜色（线性 RGBA，可 HDR）
    color_end: vec4<f32>,     // 寿命终点颜色（alpha 渐隐 = 拖尾渐隐主通道）
    trail_view: vec4<f32>,    // x = trail_length，y = 投影 y 缩放（1/tan(fov/2)），z = 视口宽高比，w 保留
};

@group(1) @binding(0) var<uniform> dparams: DrawParams;

struct SimParticle {
    pos: vec4<f32>,
    vel_age: vec4<f32>,
    flags: vec4<u32>,
    history: array<vec4<f32>, TRAIL_MAX>,
};

@group(1) @binding(1) var<storage, read> particles: array<SimParticle>;

struct VOut {
    @builtin(position) clip: vec4<f32>,
    // 四边形内坐标（[-1,1]²；片段按 head 分别做径向 / 横向衰减）
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    // 1.0 = 头部四边形（径向衰减），0.0 = 条带节（横向衰减）
    @location(2) head: f32,
};

// 6 顶点 = 两三角形（左下 → 右下 → 左上 / 左上 → 右下 → 右上）
const CORNERS = array<vec2<f32>, 6>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(1.0, -1.0),
    vec2<f32>(-1.0, 1.0),
    vec2<f32>(-1.0, 1.0),
    vec2<f32>(1.0, -1.0),
    vec2<f32>(1.0, 1.0),
);

// 第 k 新历史条目（k = 0 最新）：绝对写指针 head ⇒ 上一写槽 =
// (head − 1 − k) mod t——绝对计数使 trail_length 变更后读取自洽
//（调用方保证 head ≥ 1 + k，无下溢）
fn ring_read(p: SimParticle, k: u32, t: u32) -> vec4<f32> {
    return p.history[(p.flags.y + t - 1u - k) % t];
}

// 寿命插值颜色（v1 collect_render 同式：t = age/lifetime 钳 1，
// lerp(color_start, color_end, t)）
fn color_at(age: f32, lifetime: f32) -> vec4<f32> {
    let t = select(1.0, clamp(age / lifetime, 0.0, 1.0), lifetime > 0.0);
    return mix(dparams.color_start, dparams.color_end, vec4<f32>(t));
}

// 条带顶点颜色：RGB = 该历史点**记录时** age 的寿命插值色（沿历史
// 按 age 比例渐变——头部关节处与头部四边形颜色连续，只差一帧 age）；
// alpha 额外乘「沿历史线性淡出」因子 fade = 1 − k/(t−1)（最新端 1、
// 最旧端 0 ⇒ 尾部全透明）——尾部平滑收束，不出现硬截断（目检判据：
// 拖尾连续性 + 无断层）。
fn trail_color(p: SimParticle, k: u32, t: u32, lifetime: f32) -> vec4<f32> {
    let rec = ring_read(p, k, t);
    let c = color_at(rec.w, lifetime);
    let span = select(1.0, f32(t - 1u), t > 1u);
    let fade = 1.0 - clamp(f32(k) / span, 0.0, 1.0);
    return vec4<f32>(c.rgb, c.a * fade);
}

// 零面积退化：全部顶点画面外同点，不产生片元
fn degenerate() -> VOut {
    var o: VOut;
    o.clip = vec4<f32>(1e9, 1e9, 1e9, 1.0);
    o.uv = vec2<f32>(0.0);
    o.color = vec4<f32>(0.0);
    o.head = 0.0;
    return o;
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VOut {
    let t = u32(dparams.trail_view.x);
    let per_particle = t * 6u;
    let p_idx = vi / per_particle;
    let local = vi % per_particle;
    let q = local / 6u; // 0 = 头部四边形，≥1 = 第 q−1 条带节
    let c = CORNERS[local % 6u];
    let p = particles[p_idx];
    if (p.flags.x == 0u) {
        return degenerate();
    }
    if (q == 0u) {
        // 头部 billboard：当前位置 + 寿命插值尺寸（世界单位 → NDC：
        // × proj_y / w；x 向再除宽高比）
        let clip = globals.view_proj * vec4<f32>(p.pos.xyz, 1.0);
        if (clip.w <= 0.0) {
            return degenerate();
        }
        let ndc = clip.xy / clip.w;
        let age = p.vel_age.w;
        let t_life = select(1.0, clamp(age / dparams.lifetime_size.x, 0.0, 1.0),
            dparams.lifetime_size.x > 0.0);
        let size = dparams.lifetime_size.y
            + (dparams.lifetime_size.z - dparams.lifetime_size.y) * t_life;
        // size 语义 = quad 半边长（v1 collect_render 同款），直接作
        // NDC 展宽半径
        let half_ndc = size * dparams.trail_view.y / clip.w;
        let off = vec2<f32>(c.x * half_ndc / dparams.trail_view.z, c.y * half_ndc);
        var o: VOut;
        o.clip = vec4<f32>((ndc + off) * clip.w, clip.z, clip.w);
        o.uv = c;
        o.color = color_at(age, dparams.lifetime_size.x);
        o.head = 1.0;
        return o;
    }
    let k = q - 1u;
    // 需要第 k 与第 k+1 新两个有效历史点（flags.z = 有效历史数，钳于 t）
    if (p.flags.z <= k + 1u) {
        return degenerate();
    }
    let h0 = ring_read(p, k, t);
    let h1 = ring_read(p, k + 1u, t);
    let ca = globals.view_proj * vec4<f32>(h0.xyz, 1.0);
    let cb = globals.view_proj * vec4<f32>(h1.xyz, 1.0);
    if (ca.w <= 0.0 || cb.w <= 0.0) {
        return degenerate(); // 端点在相机身后（或退化 w）
    }
    // 屏幕空间法向（宽高比修正后求向、单位化），两端按各自 1/w 展宽
    // ——恒定世界宽度
    let na = ca.xy / ca.w;
    let nb = cb.xy / cb.w;
    var d = vec2<f32>((nb.x - na.x) * dparams.trail_view.z, nb.y - na.y);
    let len = length(d);
    if (len < 1e-8) {
        return degenerate(); // 零长度节（静止 / 重叠）→ 零面积
    }
    d = d / len;
    let perp = vec2<f32>(-d.y, d.x);
    let side = c.y;
    let half_a = dparams.lifetime_size.w * 0.5 * dparams.trail_view.y / ca.w;
    let half_b = dparams.lifetime_size.w * 0.5 * dparams.trail_view.y / cb.w;
    let perp_ndc = vec2<f32>(perp.x / dparams.trail_view.z, perp.y);
    var o: VOut;
    if (c.x < 0.0) {
        o.clip = vec4<f32>((na + perp_ndc * (side * half_a)) * ca.w, ca.z, ca.w);
        o.color = trail_color(p, k, t, dparams.lifetime_size.x);
    } else {
        o.clip = vec4<f32>((nb + perp_ndc * (side * half_b)) * cb.w, cb.z, cb.w);
        o.color = trail_color(p, k + 1u, t, dparams.lifetime_size.x);
    }
    o.uv = c;
    o.head = 0.0;
    return o;
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    // 头部：径向软衰减（particle.wgsl 同式 (1−r²)²）；条带节：横向
    // 软边（(1−v²)²，v = 宽度方向）——RGB 不衰减，衰减只乘 alpha
    var f: f32;
    if (in.head > 0.5) {
        let r2 = dot(in.uv, in.uv);
        f = clamp(1.0 - r2, 0.0, 1.0);
    } else {
        f = clamp(1.0 - in.uv.y * in.uv.y, 0.0, 1.0);
    }
    f *= f;
    return vec4<f32>(in.color.rgb, in.color.a * f);
}
