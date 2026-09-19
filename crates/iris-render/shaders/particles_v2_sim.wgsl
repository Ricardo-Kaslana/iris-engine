// 粒子 v2 模拟着色器（R82）——**全仓首个 compute 管线**。
//
// 会话内逐位确定性的根基（路线图裁定）：固定池（4096 粒）+ 恒定
// dispatch（64 工作组 × 64 线程 = 池容量，不随存活数变）+ 每线程只
// 读写自己索引的粒子（无原子、无竞争、按索引有序写入）。同适配器同
// 会话内双会话逐帧逐位一致；**跨适配器位级不可保**（GPU 编译器 fma /
// 重结合自由——诚实边界，ARCHITECTURE §8.51）。
//
// CPU 镜像契约（crates/iris-render/src/particles_v2.rs）：
// - 发射节律 CPU 判定（累计分数法 spawn_budget），本着色器只消费
//   uniform 的（发射数 n，起始游标 c0）——出生范围 [c0, c0+n)；
// - 出生哈希 = mix64(绝对池索引, seed)——u64 以 u32 肢对模拟（WGSL
//   无 u64 算术；整数精确无舍入，跨适配器恒等），与 Rust
//   mix64_limb / mix64_native / particles.rs::mix64 / wind.rs::mix64
//   同式同常数——**四处同步契约**，改一处必须全部同步；
// - 积分 = 半隐式欧拉 + 风（R80 wind_sample 的发射器原点单点采样，
//   CPU 预算进 uniform——单代表点定稿，逐粒子空间变化留后续）；
// - age 超寿命 → 存活 = false；死粒子不回收索引（固定池顺序发射，
//   每个索引至多出生一次——哈希输入恒定）；
// - 拖尾环：每存活粒子每帧写入当前位置到环形历史（绝对写指针 head，
//   读侧 (head − 1 − k) mod trail_length 取第 k 新条目——绝对计数使
//   trail_length 变更后读取自洽）。
//
// 布局影子契约：SimParams ↔ SimParamsRaw（128 B）、SimParticle ↔ 560 B
// 步长——改一侧必须同步 Rust 侧（const 断言看门）。

const POOL: u32 = 4096u;
const TRAIL_MAX: u32 = 32u;

struct SimParams {
    origin_dt: vec4<f32>,      // xyz = 发射原点（世界坐标），w = dt（秒）
    counts_cursor: vec4<u32>,  // x = 本帧发射数 n，y = 起始游标 c0，z = 池占用上限，w = trail_length（已钳 1..=32）
    lifetime_grav: vec4<f32>,  // x = 单粒子寿命（秒），yzw = 重力加速度（PHYSICS_GRAVITY × gravity_scale，CPU 预乘）
    wind_speed: vec4<f32>,     // xyz = wind_sample(发射原点, t)（Wind 缺席 = 零向量），w = 出生速率基线（米/秒）
    seed_jitter: vec4<f32>,    // x = 出生方向抖动幅度，yzw 保留
    extent_damp: vec4<f32>,    // xyz = 出生体积半边长（R92，全零 = 点发射 = 逐位不变），w = 线性阻尼 k（0 = 无 = 逐位不变）
    axis: vec4<f32>,           // xyz = 发射轴向（R92，默认 +Y = 逐位不变），w 保留
    seed: vec2<u32>,           // 发射器种子（实体 to_bits 低/高 32 位）
    _pad: vec2<u32>,
};

@group(0) @binding(0) var<uniform> params: SimParams;

/// 单粒子状态（560 B：3×vec4 元数据 + 32×vec4 环形历史）。
struct SimParticle {
    pos: vec4<f32>,     // xyz = 世界位置，w 保留
    vel_age: vec4<f32>, // xyz = 速度，w = age（秒）
    flags: vec4<u32>,    // x = alive（0/1），y = 拖尾写指针（绝对计数），z = 有效历史数，w 保留
    history: array<vec4<f32>, TRAIL_MAX>, // 环形历史：xyz = 记录时位置，w = 记录时 age
};

@group(0) @binding(1) var<storage, read_write> particles: array<SimParticle>;

// ---- u64 无符号算术（u32 肢对 lo/hi）——mix64 的 WGSL 镜像基座 ----

fn limb_add(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let lo = a.x + b.x;
    let carry = select(0u, 1u, lo < a.x);
    return vec2<u32>(lo, a.y + b.y + carry);
}

fn limb_not(a: vec2<u32>) -> vec2<u32> {
    return a ^ vec2<u32>(0xFFFFFFFFu, 0xFFFFFFFFu);
}

fn limb_shr(a: vec2<u32>, s: u32) -> vec2<u32> {
    if (s == 0u) { return a; }
    if (s < 32u) { return vec2<u32>((a.x >> s) | (a.y << (32u - s)), a.y >> s); }
    if (s == 32u) { return vec2<u32>(a.y, 0u); }
    return vec2<u32>(a.y >> (s - 32u), 0u);
}

// 32×32 → 高 32 位（floor(a·b / 2³²)，16 位裂项四块 schoolbook——
// 全部中间量 < 2³²，u32 算术精确；Rust mulhi32 同式镜像）
fn mulhi32(a: u32, b: u32) -> u32 {
    let al = a & 0xFFFFu;
    let ah = a >> 16u;
    let bl = b & 0xFFFFu;
    let bh = b >> 16u;
    let ll = al * bl;
    let lh = al * bh;
    let hl = ah * bl;
    let hh = ah * bh;
    let mid = (ll >> 16u) + (lh & 0xFFFFu) + (hl & 0xFFFFu);
    return hh + (lh >> 16u) + (hl >> 16u) + (mid >> 16u);
}

// 64×64 → 低 64 位：低字 = 低位乘积，高字 = 交叉项 + 低位乘积的进位
//（位 ≥ 64 溢出丢弃——u32 乘法按 2³² 回绕，模运算天然正确；Rust
// limb_mul_lo 同式镜像）
fn limb_mul_lo(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let ll = a.x * b.x;
    let cross = a.y * b.x + a.x * b.y;
    return vec2<u32>(ll, cross + mulhi32(a.x, b.x));
}

// splitmix64 终混合（mix64 肢对版——与 particles.rs / wind.rs /
// particles_v2.rs::mix64_native / mix64_limb 同式同常数，同步契约）
fn mix64_limb(z_in: vec2<u32>) -> vec2<u32> {
    var z = z_in;
    z = limb_mul_lo(z ^ limb_shr(z, 30u), vec2<u32>(0x1CE4E5B9u, 0xBF58476Du));
    z = limb_mul_lo(z ^ limb_shr(z, 27u), vec2<u32>(0x133111EBu, 0x94D049BBu));
    return z ^ limb_shr(z, 31u);
}

// 高 24 位 → [0, 1)（particles_v2.rs::v2_fraction 同式——整数 ≤ 2²⁴
// 在 f32 内精确、乘 2⁻²⁴ 幂次精确，CPU/GPU 逐位一致）
fn v2_fraction(z: vec2<u32>) -> f32 {
    return f32(z.y >> 8u) * (1.0 / 16777216.0);
}

fn normalize_or_zero(v: vec3<f32>) -> vec3<f32> {
    let l = length(v);
    return select(vec3<f32>(0.0), v / l, l > 0.0);
}

// 出生状态（v1 spawn_batch 逐式镜像）：方向 = normalize(axis + jitter·u)，
// u 球面均匀（z 均匀 [-1,1)、θ 均匀 [0, 2π)——TAU 取 f32 最近值
// 6.2831855，与 std::f32::consts::TAU 一致）；速度 = dir × initial_speed；
// age = 0。哈希输入 = 绝对池索引（死粒子不回收 ⇒ 每索引至多出生一次，
// 输入恒定——会话内确定性与「池满与否」解耦）。
// R92 体积出生：r2/r3/r4 = mix64(counter + (seed + k))，k = 1..=3——
// 三分量均匀 [-1, 1) 偏移 × extent；**extent 全零时 select 门逐位还原
// 原点**（`0·x = ±0` 的符号歧义被 select 吸收——缺席 = 逐位不变）；
// 轴向 uniform 化（axis = +Y 时与常量路径同值同算术）。
fn init_particle(i: u32) -> SimParticle {
    let counter = vec2<u32>(i, 0u);
    let r0 = mix64_limb(limb_add(counter, params.seed));
    let r1 = mix64_limb(limb_add(limb_not(counter), params.seed)
        ^ vec2<u32>(0x7F4A7C15u, 0x9E3779B9u));
    let z = 1.0 - 2.0 * v2_fraction(r0);
    let theta = 6.2831855 * v2_fraction(r1);
    let s = sqrt(max(0.0, 1.0 - z * z));
    let u = vec3<f32>(s * cos(theta), z, s * sin(theta));
    // 体积出生随机字（R92）：三分量偏移——与首两路输入互异
    let r2 = mix64_limb(limb_add(counter, limb_add(params.seed, vec2<u32>(1u, 0u))));
    let r3 = mix64_limb(limb_add(counter, limb_add(params.seed, vec2<u32>(2u, 0u))));
    let r4 = mix64_limb(limb_add(counter, limb_add(params.seed, vec2<u32>(3u, 0u))));
    let off = vec3<f32>(v2_fraction(r2) * 2.0 - 1.0,
                        v2_fraction(r3) * 2.0 - 1.0,
                        v2_fraction(r4) * 2.0 - 1.0);
    let vol = params.extent_damp.xyz * off;
    let pos = select(params.origin_dt.xyz, params.origin_dt.xyz + vol, vol != vec3<f32>(0.0));
    var p: SimParticle;
    p.pos = vec4<f32>(pos, 0.0);
    p.vel_age = vec4<f32>(normalize_or_zero(params.axis.xyz + params.seed_jitter.x * u)
        * params.wind_speed.w, 0.0);
    p.flags = vec4<u32>(1u, 0u, 0u, 0u);
    p.history = array<vec4<f32>, TRAIL_MAX>();
    return p;
}

@compute
@workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i >= POOL) {
        return;
    }
    var p = particles[i];
    let dt = params.origin_dt.w;
    let c0 = params.counts_cursor.y;
    let n = params.counts_cursor.x;
    if (i >= c0 && i < c0 + n) {
        // 出生（固定池顺序游标；与积分互斥——本帧不老化，v1 同语义）
        p = init_particle(i);
    } else if (p.flags.x == 1u && dt > 0.0) {
        // 半隐式欧拉（v1 / CPU 镜像 euler_step 同式：先速度后位置）
        // + 风加速度（wind_sample 单点采样，与重力合并）。
        // WGSL 不支持 swizzle 赋值——整向量重组。
        let accel = params.lifetime_grav.yzw + params.wind_speed.xyz;
        let vel_raw = p.vel_age.xyz + accel * dt;
        // 线性阻尼（R92）：k > 0 时速度 /(1 + k·dt)（指数衰减的精确
        // 离散，无爆炸风险）；k = 0 走 select 另一侧 = 原式**逐位**不变
        //（阻尼除法不进入既有算术的收缩 / 重结合决策——R63 教训的
        // select 门形态）。
        let vel = select(vel_raw, vel_raw / (1.0 + params.extent_damp.w * dt),
            params.extent_damp.w > 0.0);
        let pos = p.pos.xyz + vel * dt;
        let age = p.vel_age.w + dt;
        p.vel_age = vec4<f32>(vel, age);
        p.pos = vec4<f32>(pos, p.pos.w);
        // 寿命判死（判据 age >= lifetime，与 CPU 存活估计同式同序）
        if (age >= params.lifetime_grav.x) {
            p.flags.x = 0u;
        }
    }
    if (p.flags.x == 1u) {
        // 拖尾环写入（出生帧同样执行——新粒子即有第一个历史点）
        let t = params.counts_cursor.w;
        p.history[p.flags.y % t] = vec4<f32>(p.pos.xyz, p.vel_age.w);
        p.flags.y = p.flags.y + 1u;
        p.flags.z = min(p.flags.z + 1u, t);
    }
    particles[i] = p;
}
