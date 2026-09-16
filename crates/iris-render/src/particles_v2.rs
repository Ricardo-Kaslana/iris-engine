//! 粒子系统 v2（R82）：[`ParticleEmitterV2`] 组件、**GPU compute 模拟**
//! 与相机面向拖尾条带（ribbon）渲染。模拟见
//! `shaders/particles_v2_sim.wgsl`（**全仓首个 compute 管线**——里程碑；
//! 管线创建见 `pipeline.rs`，接线见 `renderer.rs` 粒子 pass），渲染见
//! `shaders/particles_v2_draw.wgsl`。
//!
//! # 与 v1 的分工（路线图明文）
//!
//! v1（`particles.rs`）CPU 确定性模拟**一字不动**，继续作为对照轨与
//! wasm 回退；v2 全新增，二者并存互不感知。缺 [`ParticleEmitterV2`]
//! 组件 = 零计算零绘制（无 write_buffer、无 dispatch、无 draw——既有
//! 像素与命令流逐位不变，缺席语义同 R77/R78/R81 范式）。
//!
//! # 会话内逐位确定性的根基（路线图裁定）
//!
//! **固定池 + 按索引有序写入**：池容量恒为 [`PARTICLES_V2_MAX`]（4096），
//! dispatch 数由池容量 / 工作组大小常量算出（**恒定 dispatch**，不随
//! 存活数变）；每个 compute 线程只读写自己索引的粒子（无原子、无竞争），
//! 状态存 read-write storage。同适配器同会话内：固定 dispatch + 固定
//! 工作组 + 按索引写入 ⇒ 双会话逐帧**逐位一致**（GPU 测试 FNV 锁定）。
//! **跨适配器位级不可保**（GPU 编译器 fma / 重结合自由——诚实边界，
//! §8.51；CPU 对照轨 = 本模块镜像纯函数的解析对照单测）。
//!
//! # CPU/GPU 镜像契约（同步纪律）
//!
//! - **发射节律**：CPU 侧判定每帧发射数与种子偏移（[`spawn_budget`]
//!   累计分数法，v1 逐式独立实现 + 单测），每帧 write_buffer 一个小
//!   uniform（`SimParamsRaw`）；GPU 只消费，不自判发射。
//! - **出生哈希**：GPU 用 `hash(绝对池索引, seed)` 初始化新粒子——
//!   [`mix64_limb`]（u32 肢对模拟 u64，[`mix64_native`] 同式）是
//!   `particles.rs::mix64` / `wind.rs::mix64` 的逐式镜像，**三处 +
//!   WGSL 同步契约**：改常数 / 式子必须全部同步（单测逐位对照）。
//! - **积分**：半隐式欧拉 + 风加速度（[`euler_step`] 镜像式），age
//!   超寿命 → 存活 = false；**死粒子不回收索引**（固定池顺序发射——
//!   每个索引至多出生一次，哈希输入恒定，池满即永久停发）。
//! - **拖尾环**：[`ring_advance`] 镜像 GPU 环形写指针推进。
//! - **风（R80 `Wind` 接口的首个引擎内消费者）**：CPU 每帧在**发射器
//!   原点单点采样** `wind_sample`（定稿：单代表点——WGSL 无 u64 算术，
//!   阵风波在 GPU 侧逐粒子复刻需 u32 肢对复刻整条 gust 哈希链，收益
//!   不抵风险；逐粒子空间变化留后续轮）写入 uniform，GPU 侧把它当
//!   常值加速度与重力合并进欧拉步（[`euler_step`]）。Wind 资源缺席 =
//!   无风（零向量）。CPU 镜像值与 `wind_sample` **恒等**（直接调用，
//!   零漂移风险，单测锁定）。
//!
//! # 调用契约
//!
//! 与 v1 同款：`update_particles_v2` 必须在 `propagate_transforms` 之后
//! （发射原点取当帧全局位置）、`Renderer::draw` 之前执行——iris-engine
//! 的 `ParticlePlugin` 已保证（与 `update_particles` 同层注册）；离屏
//! 快照 / 测试须手动按此顺序调用。渲染器消费组件 `pending` 帧队列
//! （互斥锁——渲染只有 `&World`，组件约束 `Send + Sync` 排除
//! `RefCell` / `Cell`，经内部可变性排水；每次渲染排水一次，同一状态
//! 重复渲染不重复步进，帧哈希稳定）。
//!
//! # 诚实限制清单（v2 明确不做 / 注意）
//!
//! - **池满永久**：死粒子不回收索引，累计发射达 [`PARTICLES_V2_MAX`]
//!   后该发射器永久停发（持续特效场景请复用实体 + `reset`，见下）；
//! - **reset 的 GPU 侧代价**：[`ParticleEmitterV2::reset`] 清 CPU 状态，
//!   GPU 池由渲染器检测游标回退后全量清零（一次 ~2.3 MB 上传，仅
//!   检测到 reset 的那一帧）；
//! - **无视锥剔除**：粒子位置状态在 GPU，CPU 无逐粒位置——发射器
//!   包围球剔除不可做（v1 有）；`Visibility` 过滤照常（隐藏只过滤
//!   绘制，模拟照常推进，v1 同语义）；
//! - **帧队列上限**：`pending` 容量 [`PARTICLES_V2_PENDING_CAP`]，
//!   超限丢弃最旧帧（headless 长跑不渲染的防 OOM；丢弃 = 那些粒子
//!   从未出生，确定性不破坏，`alive` 估计随之失真——如实声明）；
//! - **拖尾为相机面向条带，无体积**：屏幕空间展宽，侧看不变细
//!   （ billboard 同族语义）；条带节自相交时 alpha 混合不排序
//!   （v1 additive 不排序同哲学，alpha 模式自遮挡可能出现叠序痕迹，
//!   高亮特效建议 additive）；
//! - **无场景序列化**：v1 的 `ParticleSection` 不覆盖 v2（后续轮按需）；
//! - **不写 scene_depth**（与 v1 / 贴花同纪律，§8 延续）；
//! - **GPU 池按实体绑定、销毁不回收**：每个曾出现的 v2 发射器实体占
//!   一个 ~2.3 MB 池（4096 × 560 B），实体销毁不释放（世代位防撞池）；
//! - **跨适配器位级不可保**（见上，确定性声明的边界）。

use std::sync::Mutex;

use glam::Vec3;
use iris_core::prelude::{Entity, GlobalTransform, Time};

/// 固定池容量（粒）：会话内逐位确定性的根基（恒定 dispatch 的被除数）。
pub const PARTICLES_V2_MAX: u32 = 4096;

/// 拖尾历史上限（点/粒）：`trail_length` 的钳制上界（storage 布局按此
/// 定长——`trail_length` 是运行期配置，缓冲必须初始化时定尺寸）。
pub const PARTICLES_V2_TRAIL_MAX: u32 = 32;

/// compute 工作组大小（`@workgroup_size` 同值）。
pub const PARTICLES_V2_WORKGROUP: u32 = 64;

/// 恒定 dispatch 的工作组数（POOL / WORKGROUP——编译期整除，const 断言锁定）。
pub const PARTICLES_V2_DISPATCH: u32 = PARTICLES_V2_MAX / PARTICLES_V2_WORKGROUP;

/// 帧队列容量上限（headless 长跑防 OOM；超限丢最旧，见模块诚实限制）。
pub const PARTICLES_V2_PENDING_CAP: usize = 1024;

const _: () = assert!(
    PARTICLES_V2_MAX % PARTICLES_V2_WORKGROUP == 0,
    "池容量必须被工作组大小整除（恒定 dispatch 不留尾组）"
);

// ---------------------------------------------------------------------------
// 模拟 uniform 布局（与 shaders/particles_v2_sim.wgsl 的 SimParams 对齐；
// 渲染 uniform 与 particles_v2_draw.wgsl 的 DrawParams 对齐。影子契约：
// 改一侧必须同步另一侧，布局大小 const 断言看门）
// ---------------------------------------------------------------------------

/// 模拟 uniform（96 B，POD；每帧每 advance 一份，CPU 判定发射数 / 种子
/// 偏移 / dt / 风 / 位姿——GPU 只消费）。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct SimParamsRaw {
    /// xyz = 发射原点（世界坐标）；w = dt（秒）。
    pub origin_dt: [f32; 4],
    /// x = 本帧发射数 n；y = 本帧起始游标 c0（绝对池索引）；z = 池占用
    /// 上限；w = trail_length（已钳 1..=32）。
    pub counts_cursor: [u32; 4],
    /// x = 单粒子寿命（秒）；yzw = 重力加速度（[`PHYSICS_GRAVITY`] ×
    /// gravity_scale，CPU 预乘——GPU 零常量依赖）。
    pub lifetime_grav: [f32; 4],
    /// xyz = `wind_sample(发射原点, t)`（Wind 缺席 = 零向量）；w =
    /// 出生速率基线（米/秒）。
    pub wind_speed: [f32; 4],
    /// x = 出生方向抖动幅度；yzw 保留（0）。
    pub seed_jitter: [f32; 4],
    /// 发射器种子 = 实体 `to_bits` 的低 / 高 32 位。
    pub seed: [u32; 2],
    /// 对齐填充（uniform 尺寸补到 16 的倍数）。
    pub _pad: [u32; 2],
}

const _: () = assert!(
    std::mem::size_of::<SimParamsRaw>() == 96,
    "SimParamsRaw 必须为 96 字节（与 WGSL SimParams 布局一致）"
);

/// 拖尾渲染 uniform（64 B，POD；渲染器每帧每可见发射器写一份）。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct DrawParamsRaw {
    /// x = 单粒子寿命；y = 出生尺寸；z = 寿命终点尺寸；w = 拖尾全宽
    /// （世界单位）。
    pub lifetime_size: [f32; 4],
    /// 出生颜色（线性 RGBA，可 HDR）。
    pub color_start: [f32; 4],
    /// 寿命终点颜色（alpha 渐隐承担拖尾渐隐的主通道）。
    pub color_end: [f32; 4],
    /// x = trail_length；y = 投影 y 缩放（1/tan(fov/2)，世界长度 →
    /// NDC 的换算系数）；z = 视口宽高比；w 保留。
    pub trail_view: [f32; 4],
}

const _: () = assert!(
    std::mem::size_of::<DrawParamsRaw>() == 64,
    "DrawParamsRaw 必须为 64 字节（与 WGSL DrawParams 布局一致）"
);

// ---------------------------------------------------------------------------
// CPU 镜像纯函数（仅测试构建——见下方 `mirror_pures` 文档；WGSL 侧是
// 生产实现，本文件对应函数是其可单测的逐式镜像，防双侧漂移）
// ---------------------------------------------------------------------------

/// 持续发射的累计分数预算（v1 同式独立实现）：`rate·dt` 加进余量、取整
/// 出生、小数滚存——任意 dt 序列下出生总数收敛于 rate·总时长。返回
/// （本步发射数，新余量）。生产路径（[`ParticleEmitterV2::advance`]）与
/// 单测共用。
fn spawn_budget(rate: f32, accumulator: f32, dt: f32) -> (u32, f32) {
    let acc = accumulator + rate * dt;
    let n = acc as u32;
    (n, acc - n as f32)
}

/// u64 → u32 肢对（lo, hi）——种子写入 uniform（与 WGSL `vec2<u32>`
/// 种子通道同构）。
fn split_u64(v: u64) -> (u32, u32) {
    (v as u32, (v >> 32) as u32)
}

// ---------------------------------------------------------------------------
// 组件
// ---------------------------------------------------------------------------

/// 粒子发射器 v2（GPU compute 模拟 + 拖尾）：配置字段沿用 v1 形状 +
/// 拖尾参数；内部状态只做**发射记账**（发射数判定 / 游标 / 存活估计），
/// 粒子级模拟全在 GPU（见模块头）。位置语义与 v1 同：挂
/// `GlobalTransform` 实体，发射原点 = 全局平移。
///
/// 缺本组件 = 零计算零绘制（像素与命令流与引入前逐位不变）。
#[derive(Debug, iris_core::Component)]
pub struct ParticleEmitterV2 {
    /// 混合模式（拖尾高亮特效建议 `Additive`——条带自相交时 alpha
    /// 混合不排序，见模块诚实限制）。
    pub blend: crate::particles::ParticleBlend,
    /// 持续发射速率（粒/秒）；累计分数法采样（[`spawn_budget`]）。
    pub spawn_rate: f32,
    /// 一次性爆发粒数：首次 `advance` 时全部射出；0 = 无。
    pub burst_count: u32,
    /// 发射器寿命（秒；<= 0 = 永久）：耗尽停发。
    pub emitter_lifetime: f32,
    /// 单粒子寿命（秒）。
    pub particle_lifetime: f32,
    /// 出生速率基线（米/秒）。
    pub initial_speed: f32,
    /// 出生方向抖动幅度（v1 同式：方向 = normalize(+Y + jitter·u)）。
    pub velocity_jitter: f32,
    /// 重力缩放（1.0 = [`iris_core::physics::PHYSICS_GRAVITY`]）。
    pub gravity_scale: f32,
    /// 出生尺寸（头部 billboard 半边长，世界单位），随寿命线性插值到
    /// `size_end`。
    pub size_start: f32,
    /// 寿命终点尺寸。
    pub size_end: f32,
    /// 出生颜色（线性 RGBA，可 HDR）。
    pub color_start: [f32; 4],
    /// 寿命终点颜色。
    pub color_end: [f32; 4],
    /// 发射累计上限（与 [`PARTICLES_V2_MAX`] 取小；池满永久停发）。
    pub max_particles: usize,
    /// 拖尾历史点数（含头部当前位置；1 = 无拖尾，只有头部 billboard；
    /// 使用时钳 1..=[`PARTICLES_V2_TRAIL_MAX`]）。
    pub trail_length: u32,
    /// 拖尾全宽（世界单位；条带 = 相机面向屏幕空间展宽，无体积）。
    pub trail_width: f32,

    /// 持续发射的累计分数余量。
    spawn_accumulator: f32,
    /// 发射器已存活秒数（`emitter_lifetime` 判据）。
    emitter_age: f32,
    /// burst 是否已触发。
    burst_fired: bool,
    /// 命令式 burst 进账（下一次 `advance` 一次射出）。
    pending_burst: u32,
    /// 已发射总数 = 固定池顺序游标（绝对池索引；死粒子不回收）。
    spawn_cursor: u32,
    /// 存活估计批表：(批年龄, 批粒数)——同批粒子同刻出生同速老化，
    /// 死亡判定（age 越寿命）与 GPU 逐帧同式同序 ⇒ 逐帧精确（前提 =
    /// 每个入队帧都被渲染消费；帧队列溢出丢弃时失真，见模块限制）。
    alive_batches: Vec<(f32, u32)>,
    /// 待消费模拟帧队列（互斥锁——`update_particles_v2` 入队、渲染器
    /// 排水；渲染只有 `&World`，组件约束 Send + Sync 排除 RefCell/Cell。
    /// 每帧一份 [`SimParamsRaw`]，渲染侧按序 dispatch，同状态重复渲染
    /// 不重复步进）。
    pending: Mutex<Vec<SimParamsRaw>>,
}

impl Clone for ParticleEmitterV2 {
    /// 克隆配置字段 + 发射记账状态；帧队列按当前内容克隆（std `Mutex`
    /// 无 `Clone`，故手写——克隆体与原体在其后各自独立步进）。
    fn clone(&self) -> Self {
        Self {
            blend: self.blend,
            spawn_rate: self.spawn_rate,
            burst_count: self.burst_count,
            emitter_lifetime: self.emitter_lifetime,
            particle_lifetime: self.particle_lifetime,
            initial_speed: self.initial_speed,
            velocity_jitter: self.velocity_jitter,
            gravity_scale: self.gravity_scale,
            size_start: self.size_start,
            size_end: self.size_end,
            color_start: self.color_start,
            color_end: self.color_end,
            max_particles: self.max_particles,
            trail_length: self.trail_length,
            trail_width: self.trail_width,
            spawn_accumulator: self.spawn_accumulator,
            emitter_age: self.emitter_age,
            burst_fired: self.burst_fired,
            pending_burst: self.pending_burst,
            spawn_cursor: self.spawn_cursor,
            alive_batches: self.alive_batches.clone(),
            pending: Mutex::new(self.take_frames()),
        }
    }
}

impl ParticleEmitterV2 {
    /// 全默认参数的发射器（混合模式必选）：1 秒寿命、60 粒/秒、竖直
    /// 上抛无抖动、标准重力、拖尾 8 点 / 全宽 0.06、池上限 = 固定池容量。
    pub fn new(blend: crate::particles::ParticleBlend) -> Self {
        Self {
            blend,
            spawn_rate: 60.0,
            burst_count: 0,
            emitter_lifetime: 0.0,
            particle_lifetime: 1.0,
            initial_speed: 4.0,
            velocity_jitter: 0.0,
            gravity_scale: 1.0,
            size_start: 0.2,
            size_end: 0.05,
            color_start: [1.0, 1.0, 1.0, 0.9],
            color_end: [0.4, 0.5, 0.8, 0.0],
            max_particles: PARTICLES_V2_MAX as usize,
            trail_length: 8,
            trail_width: 0.06,
            spawn_accumulator: 0.0,
            emitter_age: 0.0,
            burst_fired: false,
            pending_burst: 0,
            spawn_cursor: 0,
            alive_batches: Vec::new(),
            pending: Mutex::new(Vec::new()),
        }
    }

    /// 拖尾喷泉预设：持续上抛 + 小抖动、标准重力、additive 高亮拖尾
    ///（快照 / 测试 / 演示起点；参数语义见字段文档）。
    pub fn trail_fountain() -> Self {
        let mut e = Self::new(crate::particles::ParticleBlend::Additive);
        e.spawn_rate = 90.0;
        e.particle_lifetime = 1.6;
        e.initial_speed = 6.0;
        e.velocity_jitter = 0.35;
        e.gravity_scale = 1.0;
        e.size_start = 0.22;
        e.size_end = 0.05;
        e.color_start = [1.20, 0.90, 0.45, 0.85];
        e.color_end = [0.60, 0.20, 0.05, 0.0];
        e.trail_length = 8;
        e.trail_width = 0.06;
        e
    }

    /// 池占用上限（配置与固定池容量取小；游标达此值即永久停发）。
    fn capacity(&self) -> u32 {
        self.max_particles.min(PARTICLES_V2_MAX as usize) as u32
    }

    /// trail_length 的钳制值（1..=[`PARTICLES_V2_TRAIL_MAX`]；GPU storage
    /// 布局按上限定长，运行期配置越界必须钳制）。
    pub fn trail_length_clamped(&self) -> u32 {
        self.trail_length.clamp(1, PARTICLES_V2_TRAIL_MAX)
    }

    /// 已发射总数（固定池顺序游标；渲染器据此检测 `reset` 后重清 GPU 池）。
    pub fn spawn_cursor(&self) -> u32 {
        self.spawn_cursor
    }

    /// 存活估计（渲染统计口径；精确性前提见字段文档）。
    pub fn alive_estimate(&self) -> u32 {
        self.alive_batches.iter().map(|b| b.1).sum()
    }

    /// 模拟步进（CPU 侧只做发射记账，粒子级模拟在 GPU）：死亡批老化 →
    /// 发射器寿命计时 → burst（配置式一次性 + 命令式进账合并，v1 同序）
    /// → 持续发射（累计分数法）→ 入队一份模拟帧。池满（游标达上限）后
    /// 不再发射（固定池死粒子不回收——与 v1「保老弃新」的差别：v1 池满
    /// 丢新保老，v2 全池永久封顶，见模块限制）。
    ///
    /// `wind` 为发射器原点处的 `wind_sample` 采样向量（系统侧采样；
    /// Wind 资源缺席 = 零向量）。
    pub fn advance(&mut self, dt: f32, origin: Vec3, seed: u64, wind: Vec3) -> u32 {
        assert!(
            dt.is_finite() && dt >= 0.0,
            "粒子步长必须为非负有限值，实际 {dt}"
        );
        // 1. 存活批老化 + 寿命判死（与 GPU `if age >= lifetime → dead`
        //    逐帧同式同序——alive 估计精确性的根据）
        if dt > 0.0 {
            for batch in &mut self.alive_batches {
                batch.0 += dt;
            }
        }
        self.alive_batches
            .retain(|batch| batch.0 < self.particle_lifetime);
        // 2. 发射器寿命（v1 同式：<= 0 永久；计时只在有限寿命下推进）
        if self.emitter_lifetime > 0.0 {
            self.emitter_age += dt;
        }
        let emitting = self.emitter_lifetime <= 0.0 || self.emitter_age < self.emitter_lifetime;
        // 3. burst（配置式一次性 + 命令式进账合并为一次批量——v1 同序）
        let mut burst_total = 0u32;
        if self.burst_count > 0 && !self.burst_fired {
            self.burst_fired = true;
            burst_total += self.burst_count;
        }
        if self.pending_burst > 0 {
            burst_total += std::mem::take(&mut self.pending_burst);
        }
        // 4. 发射（池满钳制——游标不越上限；被钳掉的发射不占游标）
        let cap = self.capacity();
        let start = self.spawn_cursor;
        let burst_n = burst_total.min(cap - start);
        self.spawn_cursor += burst_n;
        let mut cont_n: u32 = 0;
        if emitting && self.spawn_rate > 0.0 && dt > 0.0 {
            let (n, acc) = spawn_budget(self.spawn_rate, self.spawn_accumulator, dt);
            self.spawn_accumulator = acc;
            cont_n = n.min(cap - self.spawn_cursor);
            self.spawn_cursor += cont_n;
        }
        let total = burst_n + cont_n;
        // 5. 存活批入账
        if total > 0 {
            self.alive_batches.push((0.0, total));
        }
        // 6. 入队模拟帧（每 advance 恰一份——零发射帧同样推进积分与拖尾）；
        //    队列上限防 headless 长跑 OOM，超限丢最旧（诚实限制见模块头）
        {
            let mut q = self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if q.len() >= PARTICLES_V2_PENDING_CAP {
                q.remove(0);
            }
            let gravity_accel = iris_core::physics::PHYSICS_GRAVITY * self.gravity_scale;
            q.push(SimParamsRaw {
                origin_dt: [origin.x, origin.y, origin.z, dt],
                counts_cursor: [total, start, cap, self.trail_length_clamped()],
                lifetime_grav: [
                    self.particle_lifetime,
                    gravity_accel.x,
                    gravity_accel.y,
                    gravity_accel.z,
                ],
                wind_speed: [wind.x, wind.y, wind.z, self.initial_speed],
                seed_jitter: [self.velocity_jitter, 0.0, 0.0, 0.0],
                seed: split_u64(seed).into(),
                _pad: [0; 2],
            });
        }
        total
    }

    /// 命令式额外爆发 `count` 颗：进账在下一次 `advance` 一次射出
    ///（v1 `trigger_burst` 同语义；不改 `burst_count` 配置语义）。
    pub fn trigger_burst(&mut self, count: u32) {
        self.pending_burst = self.pending_burst.saturating_add(count);
    }

    /// 重置模拟状态（对象池槽位复用）：清存活批 / 帧队列 / 游标 / burst
    /// 上膛——参数字段不变；游标回退会被渲染器检测并重清 GPU 池（一次
    /// 全量零上传，见模块限制）。
    pub fn reset(&mut self) {
        self.spawn_accumulator = 0.0;
        self.emitter_age = 0.0;
        self.burst_fired = false;
        self.pending_burst = 0;
        self.spawn_cursor = 0;
        self.alive_batches.clear();
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    // ---- 渲染器接口（同 crate；渲染只有 &World，经互斥锁内部可变性）----

    /// 排水待消费模拟帧（渲染侧每帧一次；同状态重复渲染 = 空队列 =
    /// 不重复步进，帧哈希稳定）。
    pub(crate) fn take_frames(&self) -> Vec<SimParamsRaw> {
        std::mem::take(
            self.pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_mut(),
        )
    }
}

/// 每帧粒子 v2 推进系统：推进所有 [`ParticleEmitterV2`]（发射记账见
/// [`ParticleEmitterV2::advance`]；GPU 模拟由渲染器在 `draw` 时消费
/// 帧队列执行）。`Time` 资源缺席 = dt 0、t 0（v1 同款兜底）；
/// `Wind` 资源缺席 = 无风（零向量）——**R80 风场接口的首个引擎内消费
/// 点**：在发射器原点单点采样 `wind_sample`（定稿理由见模块头）。
///
/// 调用契约：在 `propagate_transforms` 之后、`Renderer::draw` 之前
///（DefaultPlugins 的 ParticlePlugin 与 `update_particles` 同层注册）。
pub fn update_particles_v2(world: &iris_core::world::World) {
    let (dt, t) = world
        .try_resource::<Time>()
        .map(|tm| (tm.delta_secs(), tm.elapsed_secs()))
        .unwrap_or((0.0, 0.0));
    let wind = world.try_resource::<iris_core::wind::Wind>();
    world.query::<(Entity, &GlobalTransform, &mut ParticleEmitterV2)>(|(e, gt, emitter)| {
        let wind_vec = wind
            .as_deref()
            .map(|w| iris_core::wind::wind_sample(w, gt.position(), t))
            .unwrap_or(Vec3::ZERO);
        emitter.advance(dt, gt.position(), e.to_bits(), wind_vec);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core::prelude::World;

    // ---- WGSL 逐式镜像纯函数（仅测试构建——生产实现是
    // particles_v2_sim.wgsl / particles_v2_draw.wgsl，此处是可单测的
    // CPU 对照镜像，防双侧漂移；同步契约见各函数文档与模块头）----

    /// splitmix64 终混合的原生 u64 实现（v2 自持一份——`particles.rs::mix64`
    /// 私有不可复用；同式同常数，测试与 WGSL 肢对版互为对照）。
    fn mix64_native(mut z: u64) -> u64 {
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// u32 肢对（lo, hi）加法（模 2⁶⁴）——WGSL `limb_add` 的逐式镜像。
    fn limb_add(a: (u32, u32), b: (u32, u32)) -> (u32, u32) {
        let (lo, carry) = a.0.overflowing_add(b.0);
        (lo, a.1.wrapping_add(b.1).wrapping_add(carry as u32))
    }

    /// u32 肢对逻辑右移（0..=63）——WGSL `limb_shr` 的逐式镜像。
    fn limb_shr(a: (u32, u32), s: u32) -> (u32, u32) {
        if s == 0 {
            return a;
        }
        if s < 32 {
            return ((a.0 >> s) | (a.1 << (32 - s)), a.1 >> s);
        }
        if s == 32 {
            return (a.1, 0);
        }
        (a.1 >> (s - 32), 0)
    }

    /// 32×32 → 高 32 位（floor(a·b / 2³²)，16 位裂项四块 schoolbook——
    /// 全部中间量 < 2³²，u32 算术精确；WGSL 同式镜像）。
    fn mulhi32(a: u32, b: u32) -> u32 {
        let (al, ah) = (a & 0xFFFF, a >> 16);
        let (bl, bh) = (b & 0xFFFF, b >> 16);
        let (ll, lh, hl, hh) = (
            al.wrapping_mul(bl),
            al.wrapping_mul(bh),
            ah.wrapping_mul(bl),
            ah.wrapping_mul(bh),
        );
        let mid = (ll >> 16)
            .wrapping_add(lh & 0xFFFF)
            .wrapping_add(hl & 0xFFFF);
        hh.wrapping_add(lh >> 16)
            .wrapping_add(hl >> 16)
            .wrapping_add(mid >> 16)
    }

    /// 64×64 → 低 64 位乘法——WGSL `limb_mul_lo` 的逐式镜像：低字 =
    /// 低位乘积，高字 = 交叉项 + 低位乘积的进位（位 ≥ 64 溢出丢弃）。
    fn limb_mul_lo(a: (u32, u32), b: (u32, u32)) -> (u32, u32) {
        let ll = a.0.wrapping_mul(b.0);
        let cross = a.1.wrapping_mul(b.0).wrapping_add(a.0.wrapping_mul(b.1));
        (ll, cross.wrapping_add(mulhi32(a.0, b.0)))
    }

    /// splitmix64 终混合的 u32 肢对版：[`mix64_native`] 的逐式镜像
    ///（WGSL 无 u64 整数算术，GPU 侧以此形态实现；整数运算精确无舍入，
    /// 跨适配器恒等——与浮点不同）。`mix64_native` / `particles.rs::mix64`
    /// / `wind.rs::mix64` / WGSL `mix64_limb` 四处同步契约。
    fn mix64_limb(z: (u32, u32)) -> (u32, u32) {
        let s30 = limb_shr(z, 30);
        let z = limb_mul_lo((z.0 ^ s30.0, z.1 ^ s30.1), (0x1CE4_E5B9, 0xBF58_476D));
        let s27 = limb_shr(z, 27);
        let z = limb_mul_lo((z.0 ^ s27.0, z.1 ^ s27.1), (0x1331_11EB, 0x94D0_49BB));
        let s31 = limb_shr(z, 31);
        (z.0 ^ s31.0, z.1 ^ s31.1)
    }

    /// 单粒子伪随机字对（v1 `particle_randoms` 的逐式独立实现）：
    /// counter = 绝对池索引（死粒子不回收 ⇒ 每索引至多出生一次，输入
    /// 恒定），seed 区分发射器（实体 bits）。第二路以位反计数器 +
    /// 黄金比率常数偏移（v1 同式）。
    fn v2_randoms(counter: u64, seed: u64) -> (u64, u64) {
        (
            mix64_native(counter.wrapping_add(seed)),
            mix64_native((!counter).wrapping_add(seed) ^ 0x9E37_79B9_7F4A_7C15),
        )
    }

    /// 随机字 → [0, 1)：取高 24 位（f32 尾数 24 位内**精确**表示，乘
    /// 2⁻²⁴ 幂次精确——CPU 与 GPU 逐位一致；v1 取高 53 位再经 f64→f32
    /// 转换，实际只有高 24 位幸存，本式无损等价且免除 u64→f32 转换的
    /// 镜像歧义）。
    fn v2_fraction(r: (u32, u32)) -> f32 {
        (r.1 >> 8) as f32 * (1.0 / (1u32 << 24) as f32)
    }

    /// 球面均匀单位向量（v1 `unit_sphere` 同式：z 均匀 [-1, 1)、θ 均匀
    /// [0, 2π)，面积元守恒；z 用极轴 = y 配合「基准方向 +Y」）。
    fn v2_unit_sphere(r: (u64, u64)) -> Vec3 {
        let z = 1.0 - 2.0 * v2_fraction(split_u64(r.0));
        let theta = std::f32::consts::TAU * v2_fraction(split_u64(r.1));
        let s = (1.0 - z * z).max(0.0).sqrt();
        Vec3::new(s * theta.cos(), z, s * theta.sin())
    }

    /// 出生方向（v1 `spawn_batch` 同式）：`normalize(+Y + jitter·u)`——
    /// 0 抖动 = 竖直上抛；归零保护 = 退化静止粒子而非 NaN。
    fn v2_spawn_direction(jitter: f32, r: (u64, u64)) -> Vec3 {
        (Vec3::Y + jitter * v2_unit_sphere(r)).normalize_or_zero()
    }

    /// 半隐式欧拉一步（v1 `advance` 步骤 1 同式 + 风加速度）：先更新
    /// 速度再积分位置；`gravity_accel` = `PHYSICS_GRAVITY` ×
    /// gravity_scale（CPU 预乘，与 uniform `lifetime_grav.yzw` 同源），
    /// `wind` = `wind_sample` 采样向量。**WGSL `main` 积分段的逐式
    /// 镜像**——同步契约。
    fn euler_step(
        pos: Vec3,
        vel: Vec3,
        age: f32,
        dt: f32,
        gravity_accel: Vec3,
        wind: Vec3,
    ) -> (Vec3, Vec3, f32) {
        let vel = vel + (gravity_accel + wind) * dt;
        let pos = pos + vel * dt;
        (pos, vel, age + dt)
    }

    /// 拖尾环写指针推进（WGSL `main` 拖尾段的逐式镜像）：`head` 为
    /// 绝对写计数（读侧 `(head − 1 − k) mod t` 取第 k 新条目——绝对
    /// 计数使 trail_length 变更后读侧仍自洽）；`count` 钳到 `t`。
    fn ring_advance(head: u32, count: u32, trail_length: u32) -> (u32, u32) {
        (head.wrapping_add(1), (count + 1).min(trail_length))
    }

    const DT: f32 = 1.0 / 60.0;
    const SEED: u64 = 0xDEAD_BEEF;

    /// 肢对运算与原生 u64 逐位对照（mix64 镜像契约的锚点测试）：
    /// 覆盖 0 / 1 / 最大值 / 大素数 / 黄金比率 / 伪随机批量。
    #[test]
    fn limb_ops_match_native_u64() {
        let mut samples: Vec<u64> = vec![
            0,
            1,
            2,
            u64::MAX,
            u64::MAX - 1,
            0x9E37_79B9_7F4A_7C15,
            0xBF58_476D_1CE4_E5B9,
            0x94D0_49BB_1331_11EB,
            0xFFFF_FFFF_0000_0001,
            0x0000_0001_FFFF_FFFF,
        ];
        // 伪随机批量（无状态哈希流生成——自举但与 native 对照仍有效：
        // 两套实现独立，任一式错必炸）
        let mut x = 0x1234_5678_9ABC_DEF0u64;
        for _ in 0..512 {
            x = x.wrapping_mul(0x2545_F491_4F6C_DD1D).wrapping_add(1);
            samples.push(x);
        }
        for v in &samples {
            let native = mix64_native(*v);
            let limb = mix64_limb(split_u64(*v));
            assert_eq!(
                limb,
                split_u64(native),
                "mix64 肢对版与原生 u64 不一致 @ {v:#x}"
            );
        }
        // 肢对加法：与原生 u64 wrapping_add 对照（WGSL 出生哈希的
        // counter + seed 通道）
        for v in &samples {
            for w in [1u64, 0xDEAD_BEEF, u64::MAX, *v] {
                let native = v.wrapping_add(w);
                let limb = limb_add(split_u64(*v), split_u64(w));
                assert_eq!(limb, split_u64(native), "limb_add 与原生不一致 @ {v:#x}");
            }
        }
        // 退化锚点：mix64(0) = 0（全零输入零输出）
        assert_eq!(mix64_native(0), 0);
        assert_eq!(mix64_limb((0, 0)), (0, 0));
    }

    /// v2_fraction：值域 [0, 1)、端点行为、与「高 53 位再转 f32」的
    /// v1 式在 24 位粒度上无损等价（镜像契约的数值根据）。
    #[test]
    fn fraction_range_and_precision() {
        // 全零 → 0；全一 → 1 − 2⁻²⁴（高 24 位全 1——两侧均精确表示）
        assert_eq!(v2_fraction((0, 0)), 0.0);
        assert_eq!(
            v2_fraction((u32::MAX, u32::MAX)),
            1.0 - 1.0 / (1u32 << 24) as f32
        );
        // 单调：高 24 位递增 → 值严格递增
        let mut prev = -1.0f32;
        for k in 0..64u32 {
            let v = v2_fraction((0, k << 24));
            assert!(v > prev, "k={k} 应严格递增");
            assert!((0.0..1.0).contains(&v));
            prev = v;
        }
        // 值恰为 k·2¹⁶ / 2²⁴ = k / 2⁸（整数 ≤ 2²⁴ 在 f32 内精确、乘 2⁻²⁴ 幂次精确）
        for k in [1u32, 2, 3, 129, 255] {
            let expect = (k << 16) as f32 / (1u32 << 24) as f32;
            assert_eq!(v2_fraction((0, k << 24)), expect);
        }
    }

    /// 出生方向：jitter = 0 恒 +Y；jitter 大时单位化且各异（哈希流
    /// 非退化）；速率 = initial_speed（镜像 v1 hash_stream 测试形态）。
    #[test]
    fn spawn_direction_matches_v1_semantics() {
        assert_eq!(v2_spawn_direction(0.0, v2_randoms(7, SEED)), Vec3::Y);
        let mut distinct = std::collections::BTreeSet::new();
        for i in 0..64u64 {
            let d = v2_spawn_direction(2.6, v2_randoms(i, SEED));
            assert!((d.length() - 1.0).abs() < 1e-5, "方向应单位化");
            distinct.insert((d.x.to_bits(), d.y.to_bits(), d.z.to_bits()));
        }
        assert!(
            distinct.len() > 60,
            "方向应几乎各异，实际 {}",
            distinct.len()
        );
    }

    /// 发射节律：spawn_budget 累计分数法——rate=60、dt=1/60 每步 1 粒；
    /// rate=30 每两步 1 粒；dt=1 单步 60 粒；同 dt 序列余量位型一致。
    #[test]
    fn spawn_budget_accumulates_fractionally() {
        let mut acc = 0.0f32;
        let mut total = 0u64;
        for _ in 0..240 {
            let (n, a) = spawn_budget(60.0, acc, DT);
            acc = a;
            total += n as u64;
        }
        assert_eq!(total, 240, "60 粒/秒 × 4 秒应精确 240 粒");
        // 半速率：每两步 1 粒
        let mut acc = 0.0f32;
        let mut total = 0u64;
        for _ in 0..240 {
            let (n, a) = spawn_budget(30.0, acc, DT);
            acc = a;
            total += n as u64;
        }
        assert_eq!(total, 120, "30 粒/秒 × 4 秒应精确 120 粒（余量滚存）");
        // 大步长
        let (n, _) = spawn_budget(60.0, 0.0, 1.0);
        assert_eq!(n, 60);
        // 确定性：同序列余量位型一致
        let mut a1 = 0.0f32;
        let mut a2 = 0.0f32;
        for i in 0..97 {
            let dt = DT * (i % 5 + 1) as f32;
            a1 = spawn_budget(37.0, a1, dt).1;
            a2 = spawn_budget(37.0, a2, dt).1;
            assert_eq!(a1.to_bits(), a2.to_bits(), "步 {i} 余量应逐位一致");
        }
    }

    /// 欧拉步解析对照：匀速直线（零加速度）位移 = v·dt（±1e-5）；
    /// 匀加速（重力 + 风）半隐式欧拉 n 步解析解 y_n = v₀·n·dt +
    /// a·dt²·n(n+1)/2；风与重力合并进同一加速度向量。
    #[test]
    fn euler_step_matches_analytic_solution() {
        // 匀速直线
        let (p, v, a) = euler_step(
            Vec3::new(1.0, 2.0, 3.0),
            Vec3::new(4.0, 0.0, -2.0),
            0.5,
            DT,
            Vec3::ZERO,
            Vec3::ZERO,
        );
        assert!((p.x - (1.0 + 4.0 * DT)).abs() < 1e-5);
        assert!((p.z - (3.0 - 2.0 * DT)).abs() < 1e-5);
        assert_eq!(v, Vec3::new(4.0, 0.0, -2.0), "零加速度速度不变");
        assert!((a - (0.5 + DT)).abs() < 1e-6);

        // 半隐式欧拉 n 步：重力 2g + 恒风 (2, 0, 0)——y 按重力解析、
        // x 按风力解析（加速度合并后各分量独立成立）
        let (v0, g_scale, steps) = (6.0f32, 2.0f32, 120u32);
        let gravity_accel = iris_core::physics::PHYSICS_GRAVITY * g_scale;
        let wind = Vec3::new(2.0, 0.0, 0.0);
        let (mut pos, mut vel, mut age) = (Vec3::ZERO, Vec3::new(0.0, v0, 0.0), 0.0f32);
        for _ in 0..steps {
            let np = euler_step(pos, vel, age, DT, gravity_accel, wind);
            pos = np.0;
            vel = np.1;
            age = np.2;
        }
        let g = 9.81 * g_scale;
        let n = steps as f32;
        let expected_y = v0 * n * DT - 0.5 * g * DT * DT * n * (n + 1.0);
        let expected_x = 0.5 * 2.0 * DT * DT * n * (n + 1.0);
        assert!(
            (pos.y - expected_y).abs() < 1e-3,
            "y = {} 应 ≈ {expected_y}",
            pos.y
        );
        assert!(
            (pos.x - expected_x).abs() < 1e-4,
            "x = {} 应 ≈ {expected_x}",
            pos.x
        );
        assert_eq!(pos.z, 0.0, "z 方向无加速度应严格为零");
        assert!((vel.y - (v0 - g * n * DT)).abs() < 1e-3);
        assert!((age - n * DT).abs() < 1e-3);
    }

    /// 拖尾环：写槽公式（head % t）与读槽公式（(head − 1 − k) mod t）
    /// 对照朴素双端历史 + 槽表模拟——语义 = 第 k 新条目值一致；
    /// trail_length 变更（t → t'）后读取仍不越界。
    #[test]
    fn trail_ring_write_and_read_slots() {
        let mut naive: Vec<u32> = Vec::new(); // 完整历史（第 k 新 = len−1−k）
        let (mut head, mut count) = (0u32, 0u32);
        let t = 4u32;
        let mut slots = [0xFFFFu32; 4]; // 环形槽表（存完整历史下标作指纹）
        for step in 0..12u32 {
            naive.push(step);
            slots[(head % t) as usize] = step;
            let adv = ring_advance(head, count, t);
            head = adv.0;
            count = adv.1;
            assert_eq!(
                count as usize,
                naive.len().min(t as usize),
                "步 {step} 有效数应一致"
            );
            for k in 0..count {
                let slot = (head.wrapping_sub(1).wrapping_sub(k)) % t;
                assert_eq!(
                    slots[slot as usize],
                    naive[naive.len() - 1 - k as usize],
                    "步 {step} 第 {k} 新条目应从槽 {slot} 读出"
                );
            }
        }
        // trail_length 变更（4 → 2）：绝对 head 语义下读槽仍不越界
        let t2 = 2u32;
        for k in 0..count.min(t2) {
            let slot = (head.wrapping_sub(1).wrapping_sub(k)) % t2;
            assert!(slot < t2);
        }
    }

    /// advance 全链：burst + 持续发射 + 池满永久停发 + 存活估计与 GPU
    /// 死亡判定同步（批老化同式）；pending 帧逐份入队且 n / 游标一致。
    #[test]
    fn advance_accounting_and_pending_frames() {
        let mut e = ParticleEmitterV2::new(crate::particles::ParticleBlend::Additive);
        e.burst_count = 10;
        e.spawn_rate = 60.0;
        e.particle_lifetime = 0.5;
        // 首步：burst 10 + 持续 1 = 11
        let n0 = e.advance(DT, Vec3::ZERO, SEED, Vec3::ZERO);
        assert_eq!(n0, 11);
        assert_eq!(e.spawn_cursor(), 11);
        assert_eq!(e.alive_estimate(), 11);
        // 共 30 步（0.5 s 内）：首批 11 + 29 个单粒批全部批龄 < 0.5 s
        for _ in 0..29 {
            e.advance(DT, Vec3::ZERO, SEED, Vec3::ZERO);
        }
        assert_eq!(e.alive_estimate(), 40, "0.5 s 内应全活（11 + 29）");
        // 第 31 步：首批批龄 30·dt = 0.5 越寿命（判据 age >= lifetime，
        // 与 GPU 同式）→ 丢 11 粒、进 1 粒；寿命 30 步后进入稳态（逐批
        // 一进一出）
        e.advance(DT, Vec3::ZERO, SEED, Vec3::ZERO);
        assert_eq!(e.alive_estimate(), 30, "首批越寿命应剔除（40 − 11 + 1）");
        // 停发后再走 30 步：逐批老化出局 → 清零
        e.spawn_rate = 0.0;
        for _ in 0..30 {
            e.advance(DT, Vec3::ZERO, SEED, Vec3::ZERO);
        }
        assert_eq!(e.alive_estimate(), 0, "停发 + 30 步老化应全灭");
        // 池满永久：max_particles 压到 12 → cursor 12 后停发
        let mut e = ParticleEmitterV2::new(crate::particles::ParticleBlend::Additive);
        e.burst_count = 10;
        e.spawn_rate = 60.0;
        e.particle_lifetime = 1000.0;
        e.max_particles = 12;
        e.advance(DT, Vec3::ZERO, SEED, Vec3::ZERO); // 10 + 1 = 11
        let _ = e.advance(DT, Vec3::ZERO, SEED, Vec3::ZERO); // +1 = 12（封顶）
        assert_eq!(e.spawn_cursor(), 12);
        let n2 = e.advance(DT, Vec3::ZERO, SEED, Vec3::ZERO);
        assert_eq!(n2, 0, "池满后永久停发（死粒子不回收）");
        assert_eq!(e.spawn_cursor(), 12);
        // pending 帧：每 advance 恰一份，n / 起始游标一致
        let frames = e.take_frames();
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].counts_cursor, [11, 0, 12, 8]);
        assert_eq!(frames[1].counts_cursor, [1, 11, 12, 8]);
        assert_eq!(frames[2].counts_cursor, [0, 12, 12, 8]);
        assert_eq!(
            e.take_frames().len(),
            0,
            "排水后队列为空（重复渲染不重复步进）"
        );
    }

    /// 帧队列上限：超限丢最旧（headless 防 OOM 的诚实语义）。
    #[test]
    fn pending_queue_cap_drops_oldest() {
        let mut e = ParticleEmitterV2::new(crate::particles::ParticleBlend::Alpha);
        e.particle_lifetime = 1000.0;
        for i in 0..(PARTICLES_V2_PENDING_CAP + 8) {
            e.advance(DT, Vec3::new(i as f32, 0.0, 0.0), SEED, Vec3::ZERO);
        }
        let frames = e.take_frames();
        assert_eq!(frames.len(), PARTICLES_V2_PENDING_CAP);
        // 最旧的 8 帧被丢：队首游标 = 8
        assert_eq!(frames[0].counts_cursor[1], 8);
        // 游标语义自洽：起始 + n 连续
        for w in frames.windows(2) {
            assert_eq!(
                w[0].counts_cursor[1] + w[0].counts_cursor[0],
                w[1].counts_cursor[1]
            );
        }
    }

    /// 风镜像：advance 写进 uniform 的风向向量 = 调用方传入值逐位；
    /// update_particles_v2 系统对 Wind 资源在场 / 缺席的行为 =
    /// wind_sample(原点, t) / 零向量（R80 接口首个消费者的契约测试）。
    #[test]
    fn wind_mirror_matches_wind_sample() {
        // 系统级：Wind 在场 → uniform wind_speed.xyz 与 wind_sample 逐位一致
        let mut world = World::new();
        world.insert_resource(Time::default());
        let wind = iris_core::wind::Wind {
            direction: glam::Vec3::new(0.3, 4.0, -0.8),
            strength: 3.5,
            gustiness: 0.6,
            seed: 42,
        };
        world.insert_resource(wind);
        world.spawn((
            iris_core::prelude::Transform::from_xyz(1.0, 2.0, 3.0),
            ParticleEmitterV2::new(crate::particles::ParticleBlend::Alpha),
        ));
        iris_core::hierarchy::propagate_transforms(&mut world);
        let mut got: Vec<[f32; 4]> = Vec::new();
        let mut expects: Vec<Vec3> = Vec::new();
        let origin = Vec3::new(1.0, 2.0, 3.0);
        let wind_res = world.try_resource::<iris_core::wind::Wind>();
        for _i in 0..3 {
            world
                .resource_mut::<Time>()
                .advance(std::time::Duration::from_nanos(1_000_000_000 / 60));
            // 每帧期望 = wind_sample(原点, 当帧 elapsed)——与系统采样时刻
            // 逐帧对齐（阵风波随时间变化，帧间本就不同）
            expects.push(iris_core::wind::wind_sample(
                wind_res.as_deref().unwrap(),
                origin,
                world.resource::<Time>().elapsed_secs(),
            ));
            update_particles_v2(&world);
        }
        world.query::<&ParticleEmitterV2>(|e| {
            for f in e.take_frames() {
                got.push(f.wind_speed);
            }
        });
        assert_eq!(got.len(), 3);
        for (g, expect) in got.iter().zip(&expects) {
            assert_eq!(
                g[0].to_bits(),
                expect.x.to_bits(),
                "风 x 应与 wind_sample 逐位一致"
            );
            assert_eq!(g[1].to_bits(), expect.y.to_bits());
            assert_eq!(g[2].to_bits(), expect.z.to_bits());
        }
        // 系统级：Wind 缺席 → 零向量
        let mut world = World::new();
        world.insert_resource(Time::default());
        world.spawn((
            iris_core::prelude::Transform::IDENTITY,
            ParticleEmitterV2::new(crate::particles::ParticleBlend::Alpha),
        ));
        iris_core::hierarchy::propagate_transforms(&mut world);
        update_particles_v2(&world);
        world.query::<&ParticleEmitterV2>(|e| {
            for f in e.take_frames() {
                assert_eq!(f.wind_speed[0], 0.0);
                assert_eq!(f.wind_speed[1], 0.0);
                assert_eq!(f.wind_speed[2], 0.0);
            }
        });
    }

    /// 确定性：同输入序列的两个发射器，全部 pending 帧逐位一致
    ///（Pod 结构直接 PartialEq——位型比较）；换 seed → 帧内容不同
    ///（种子确实参与）。
    #[test]
    fn pending_frames_are_bit_deterministic() {
        let run = |seed: u64| {
            let mut e = ParticleEmitterV2::trail_fountain();
            let mut frames: Vec<Vec<SimParamsRaw>> = Vec::new();
            for i in 0..60 {
                let origin = Vec3::new(i as f32 * 0.1, 1.0, 0.0);
                e.advance(
                    DT * (i % 3 + 1) as f32,
                    origin,
                    seed,
                    Vec3::new(1.0, 0.0, 2.0),
                );
                frames.push(e.take_frames());
            }
            frames
        };
        let a = run(SEED);
        let b = run(SEED);
        assert_eq!(a, b, "同输入两次模拟必须逐位一致");
        let c = run(SEED + 1);
        assert_ne!(a, c, "不同 seed 应产生不同帧序列");
        // 风向量确实进入 uniform（非零风场景帧内 wind_speed 非零）
        assert!(
            a.iter()
                .flatten()
                .any(|f| f.wind_speed[0] != 0.0 || f.wind_speed[2] != 0.0)
        );
    }

    /// reset：推进一段后重置，游标 / 存活 / 队列清零、burst 重新上膛
    ///（渲染器检测游标回退重清 GPU 池的 CPU 侧前提）。
    #[test]
    fn reset_clears_state_and_rearms_burst() {
        let mut e = ParticleEmitterV2::trail_fountain();
        e.burst_count = 5;
        e.advance(DT, Vec3::ZERO, SEED, Vec3::ZERO);
        assert!(e.alive_estimate() > 0);
        assert!(!e.take_frames().is_empty());
        e.reset();
        assert_eq!(e.spawn_cursor(), 0);
        assert_eq!(e.alive_estimate(), 0);
        assert_eq!(e.take_frames().len(), 0);
        assert_eq!(
            e.advance(0.0, Vec3::ZERO, SEED, Vec3::ZERO),
            5,
            "burst 应重新上膛"
        );
    }

    /// trigger_burst：命令式进账在下一次 advance 一次射出（dt = 0 亦
    /// 触发），不改 burst_count 配置语义（v1 同款）。
    #[test]
    fn trigger_burst_fires_on_next_advance() {
        let mut e = ParticleEmitterV2::new(crate::particles::ParticleBlend::Alpha);
        e.burst_count = 5;
        e.spawn_rate = 0.0;
        e.particle_lifetime = 1000.0;
        assert_eq!(e.advance(DT, Vec3::ZERO, SEED, Vec3::ZERO), 5);
        e.trigger_burst(7);
        assert_eq!(e.advance(0.0, Vec3::ONE, SEED, Vec3::ZERO), 7);
        assert_eq!(e.advance(DT, Vec3::ZERO, SEED, Vec3::ZERO), 0, "不得补发");
    }

    /// trail_length 钳制与容量钳制：越界配置使用时不越 storage 上限；
    /// max_particles 与固定池容量取小。
    #[test]
    fn clamping_holds() {
        let mut e = ParticleEmitterV2::new(crate::particles::ParticleBlend::Alpha);
        e.trail_length = 999;
        assert_eq!(e.trail_length_clamped(), PARTICLES_V2_TRAIL_MAX);
        e.trail_length = 0;
        assert_eq!(e.trail_length_clamped(), 1);
        e.max_particles = usize::MAX;
        assert_eq!(e.capacity(), PARTICLES_V2_MAX);
        e.max_particles = 7;
        assert_eq!(e.capacity(), 7);
    }
}
