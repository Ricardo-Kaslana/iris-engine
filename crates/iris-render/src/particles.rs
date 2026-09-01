//! 粒子系统 v1：[`ParticleEmitter`] 组件 + CPU 确定性模拟 + billboard
//! 实例化渲染（渲染侧见 `renderer.rs` 粒子 pass 与 `shaders/particle.wgsl`）。
//!
//! 模型：发射器组件挂载在任意实体上，发射原点 = 该实体 `GlobalTransform`
//! 的平移；粒子出生即存**世界坐标**——发射器移动时旧粒子留在原地，天然
//! 形成拖尾。模拟在 CPU 侧每帧推进（[`update_particles`]），渲染侧只读
//! 收集活粒子为 billboard 实例（`ParticleInstance`，32 字节/粒）。
//!
//! ```ignore
//! world.spawn((Transform::from_xyz(0.0, 0.0, 0.0), ParticleEmitter::fountain()));
//! // DefaultPlugins 的 ParticlePlugin 已注册 update_particles
//! //（PostUpdate、层级传播之后）；渲染器每帧自动绘制活粒子。
//! ```
//!
//! **确定性**：模拟不依赖任何全局状态与 rand 依赖——出生方向由
//! splitmix64 哈希流（发射器内全局递增计数器 ^ 实体 bits，见
//! `particle_randoms`）派生，同一输入序列两次模拟**逐位一致**（模块
//! 单测守护）。与物理子系统同款语义，可安全用于回放与网络同步。
//!
//! **单遍系统的理由**：[`update_particles`] 在一次 `world.query` 内以
//! `&mut ParticleEmitter` 直接推进——模拟只写发射器**自身内部**的粒子池
//! （私有 `Vec`），不跨组件借用、每实体恰被访问一次，无别名风险；不像
//! `update_animations` 需要写**其他实体**的 Transform 才被迫两阶段。
//!
//! **调用契约**：`update_particles` 必须在 `propagate_transforms` 之后
//! （读取当帧全局位置）、`Renderer::draw` 之前（渲染收集活粒子）执行，
//! 与层级传播同层——DefaultPlugins 的 ParticlePlugin 已保证；离屏快照 /
//! 测试须手动按此顺序调用。
//!
//! v1 限制：不写场景序列化（JSON 场景无粒子段）；渲染实例缓冲每帧全量
//! 上传（粒子逐帧全变，增量比对无价值）；无碰撞与光照交互。

use glam::Vec3;
use iris_core::prelude::{Entity, GlobalTransform, Time};

/// 粒子混合模式（`Default` 不 derive——默认值由构造函数决定：
/// [`ParticleEmitter::fountain`] 为 `Alpha`、[`ParticleEmitter::explosion`]
/// 为 `Additive`，[`ParticleEmitter::new`] 取 `Alpha` 兜底）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParticleBlend {
    /// 加法混合（`src·src_alpha + dst`）：火焰 / 火花类高亮粒子，
    /// 叠加越多层越亮，满足交换律——渲染不排序。
    Additive,
    /// 标准 alpha 混合（`src·src_alpha + dst·(1−src_alpha)`）：烟 / 水花
    /// 类常规粒子——不满足交换律，渲染按视深降序（远 → 近）排序。
    Alpha,
}

/// 单个活粒子（CPU 池内数据；世界坐标，见模块文档）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Particle {
    /// 世界坐标位置。
    pub pos: Vec3,
    /// 世界坐标速度。
    pub vel: Vec3,
    /// 已存活秒数（≥ [`ParticleEmitter::particle_lifetime`] 即被剔除）。
    pub age: f32,
}

/// 粒子发射器组件：参数 + 活粒子池 + 模拟内部状态，由 [`update_particles`]
/// 每帧推进、渲染器只读收集。粒子池满时**丢弃新粒子**（持续发射与 burst
/// 同语义：不挤掉旧粒子——保老弃新，池规模即视觉效果上限，行为可预期）。
#[derive(Clone, Debug, iris_core::Component)]
pub struct ParticleEmitter {
    pub blend: ParticleBlend,
    /// 持续发射速率（粒/秒）；累计分数法采样（`rate·dt` 取整、余量滚存），
    /// 任意 dt 序列下的出生总数都收敛于 rate·总时长（见模块单测）。
    pub spawn_rate: f32,
    /// 一次性爆发粒数：首次 `advance` 时全部射出（爆炸用；0 = 无）。
    /// 只触发一次，不受 `emitter_lifetime` 门控（寿命只约束持续发射），
    /// dt = 0 的首次推进同样触发（粒子定格在出生状态）。
    pub burst_count: u32,
    /// 发射器寿命（秒；<= 0 = 永久）：耗尽后停止**持续发射**，
    /// 已出生粒子继续存活到各自寿命终点。
    pub emitter_lifetime: f32,
    /// 单粒子寿命（秒）。
    pub particle_lifetime: f32,
    /// 出生速率基线（沿出生方向，米/秒）。
    pub initial_speed: f32,
    /// 出生方向抖动幅度：方向 = normalize(+Y + jitter·u)（u 球面均匀）。
    /// 0 = 竖直上抛（喷泉主轴）；≈1 张角覆盖上半球；≫1 趋近全向球面（爆炸）。
    pub velocity_jitter: f32,
    /// 重力缩放（1.0 = 标准重力 [`iris_core::physics::PHYSICS_GRAVITY`]，
    /// 与物理子系统同一常量）。
    pub gravity_scale: f32,
    /// 出生尺寸（billboard 半边长，世界单位），随寿命线性插值到 `size_end`。
    pub size_start: f32,
    /// 寿命终点尺寸。
    pub size_end: f32,
    /// 出生颜色（线性 RGBA），随寿命线性插值到 `color_end`；
    /// RGB 分量可 > 1（HDR，供 additive 粒子触发泛光）。
    pub color_start: [f32; 4],
    /// 寿命终点颜色。
    pub color_end: [f32; 4],
    /// 活粒子池上限（池满丢弃新粒子，见结构体文档）。
    pub max_particles: usize,
    /// 活粒子池（私有：渲染收集走 [`ParticleEmitter::collect_render`]，
    /// 统计走 [`ParticleEmitter::particles`] 只读切片）。
    particles: Vec<Particle>,
    /// 持续发射的累计分数余量（0 ≤ 余量 < 1，逐帧滚存保证确定性计数）。
    spawn_accumulator: f32,
    /// 发射器已存活秒数（`emitter_lifetime` 判据）。
    emitter_age: f32,
    /// burst 是否已触发（一次性标记，v1 无重置 API）。
    burst_fired: bool,
    /// 伪随机流全局递增计数器（每尝试发射一颗 +1；哈希流的状态本体）。
    rng_counter: u64,
}

/// 单步推进报告（`advance` 返回值；测试与调试用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdvanceStats {
    /// 本步实际发射（进入池）的粒子数——burst + 持续发射合计，
    /// 池满丢弃不计入。
    pub spawned: u32,
    /// 步进后的活粒子数（≡ [`ParticleEmitter::live_count`]）。
    pub alive: usize,
}

impl ParticleEmitter {
    /// 全默认参数的发射器（仅混合模式必选）：1 秒寿命、60 粒/秒、
    /// 竖直上抛无抖动、标准重力，其余见字段文档。
    pub fn new(blend: ParticleBlend) -> Self {
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
            max_particles: 1024,
            particles: Vec::new(),
            spawn_accumulator: 0.0,
            emitter_age: 0.0,
            burst_fired: false,
            rng_counter: 0,
        }
    }

    /// 喷泉预设：持续发射的水柱——竖直上抛 + 小抖动、标准重力回落、
    /// alpha 混合（水花半透明）、尺寸与不透明度随寿命衰减。
    pub fn fountain() -> Self {
        let mut e = Self::new(ParticleBlend::Alpha);
        e.spawn_rate = 90.0;
        e.particle_lifetime = 1.6;
        e.initial_speed = 6.0;
        e.velocity_jitter = 0.35;
        e.gravity_scale = 1.0;
        e.size_start = 0.22;
        e.size_end = 0.05;
        e.color_start = [0.60, 0.80, 1.00, 0.85];
        e.color_end = [0.15, 0.30, 0.80, 0.0];
        e.max_particles = 1024;
        e
    }

    /// 爆炸预设：一次性全向爆发——`burst_count` 颗径向扩散（jitter ≫ 1
    /// 近似球面均匀）、低重力拖尾、additive 混合 + HDR 暖色（亮度 > 1，
    /// 可触发泛光），尺寸与颜色随寿命衰减到熄灭。
    pub fn explosion() -> Self {
        let mut e = Self::new(ParticleBlend::Additive);
        e.spawn_rate = 0.0;
        e.burst_count = 320;
        e.particle_lifetime = 1.4;
        e.initial_speed = 7.0;
        e.velocity_jitter = 2.6;
        e.gravity_scale = 0.55;
        e.size_start = 0.30;
        e.size_end = 0.02;
        e.color_start = [3.2, 1.6, 0.5, 1.0];
        e.color_end = [1.5, 0.12, 0.04, 0.0];
        e.max_particles = 1024;
        e
    }

    /// 活粒子数（池状态；渲染统计与 `FrameStats.particles_drawn` 对账用）。
    pub fn live_count(&self) -> usize {
        self.particles.len()
    }

    /// 池是否为空。
    pub fn is_empty(&self) -> bool {
        self.particles.is_empty()
    }

    /// 活粒子只读切片（包围球剔除 / 统计用；修改只能经由 `advance`）。
    pub fn particles(&self) -> &[Particle] {
        &self.particles
    }

    /// 收集当前活粒子的 GPU 渲染实例（中心 + 插值尺寸 + 插值颜色），
    /// 追加到 `out`——只读方法，供渲染器在 `update_particles` 之后调用。
    pub fn collect_render(&self, out: &mut Vec<ParticleInstance>) {
        for p in &self.particles {
            let t = if self.particle_lifetime > 0.0 {
                (p.age / self.particle_lifetime).clamp(0.0, 1.0)
            } else {
                1.0
            };
            let lerp = |a: f32, b: f32| a + (b - a) * t;
            let (cs, ce) = (self.color_start, self.color_end);
            out.push(ParticleInstance {
                center_size: [
                    p.pos.x,
                    p.pos.y,
                    p.pos.z,
                    lerp(self.size_start, self.size_end),
                ],
                color: [
                    lerp(cs[0], ce[0]),
                    lerp(cs[1], ce[1]),
                    lerp(cs[2], ce[2]),
                    lerp(cs[3], ce[3]),
                ],
            });
        }
    }

    /// 模拟核心：推进一步（纯函数，不触世界——可脱离世界单测）。
    ///
    /// 步序（半隐式欧拉，与物理子系统同款）：
    /// 1. 推进现有粒子：`age += dt` → `vel += g·gravity_scale·dt` →
    ///    `pos += vel·dt`（先更新速度再积分位置）；
    /// 2. 剔除到寿粒子（`age >= particle_lifetime`）；
    /// 3. 发射器寿命计时（`emitter_lifetime > 0` 时才累计），耗尽停发；
    /// 4. burst：首次调用一次性射出 `burst_count` 颗；
    /// 5. 持续发射：累计分数法 `spawn_rate·dt` 取整、余量滚存。
    ///
    /// `origin` 为本帧发射原点（发射器 `GlobalTransform` 平移，世界坐标）；
    /// `seed` 为伪随机流种子基（系统传实体 `Entity::to_bits`，测试传任意
    /// 常量）——同一 `(dt 序列, origin, seed)` 输入两次调用，粒子池逐位一致。
    pub fn advance(&mut self, dt: f32, origin: Vec3, seed: u64) -> AdvanceStats {
        assert!(
            dt.is_finite() && dt >= 0.0,
            "粒子步长必须为非负有限值，实际 {dt}"
        );
        // 1. 推进（速度先行：半隐式欧拉，能量行为稳定）
        for p in &mut self.particles {
            p.age += dt;
            p.vel += iris_core::physics::PHYSICS_GRAVITY * (self.gravity_scale * dt);
            p.pos += p.vel * dt;
        }
        // 2. 寿命剔除（retain 保持相对次序——池的确定性）
        self.particles.retain(|p| p.age < self.particle_lifetime);
        // 3. 发射器寿命：<= 0 视为永久；> 0 时计时、耗尽停发（已出生粒子
        //    不受影响，继续存活到各自终点）
        if self.emitter_lifetime > 0.0 {
            self.emitter_age += dt;
        }
        let emitting = self.emitter_lifetime <= 0.0 || self.emitter_age < self.emitter_lifetime;
        let mut spawned = 0;
        // 4. burst：只触发一次（与 dt 无关，dt = 0 亦触发——定格供渲染）
        if self.burst_count > 0 && !self.burst_fired {
            self.burst_fired = true;
            spawned += self.spawn_batch(self.burst_count, origin, seed);
        }
        // 5. 持续发射：累计分数法——rate·dt 取整出生、小数余量滚存，
        //    任意 dt 序列下出生总数都收敛于 rate·总时长（确定性计数）
        if emitting && self.spawn_rate > 0.0 && dt > 0.0 {
            self.spawn_accumulator += self.spawn_rate * dt;
            let n = self.spawn_accumulator as u32;
            self.spawn_accumulator -= n as f32;
            spawned += self.spawn_batch(n, origin, seed);
        }
        AdvanceStats {
            spawned,
            alive: self.particles.len(),
        }
    }

    /// 批量发射 `count` 颗：逐颗尝试入池，池满即**丢弃剩余新粒子**
    ///（保老弃新语义，见结构体文档）。随机数按「尝试发射」逐颗消耗
    /// 计数器——被丢弃的尝试同样推进哈希流（确定性与「池满与否」解耦）。
    fn spawn_batch(&mut self, count: u32, origin: Vec3, seed: u64) -> u32 {
        let mut spawned = 0;
        for _ in 0..count {
            let counter = self.rng_counter;
            self.rng_counter = self.rng_counter.wrapping_add(1);
            if self.particles.len() >= self.max_particles {
                continue;
            }
            // 出生方向 = normalize(+Y + jitter·u)：0 抖动 = 竖直上抛；
            // jitter ≫ 1 时 +Y 项可忽略，u 的球面均匀性主导 → 近似全向
            //（归零保护：极端输入退化为静止粒子而非 NaN）
            let u = unit_sphere(particle_randoms(counter, seed));
            let dir = (Vec3::Y + self.velocity_jitter * u).normalize_or_zero();
            self.particles.push(Particle {
                pos: origin,
                vel: dir * self.initial_speed,
                age: 0.0,
            });
            spawned += 1;
        }
        spawned
    }
}

/// splitmix64 终混合函数：确定性伪随机原语（计数器型——无全局状态、
/// 无 rand 依赖，同一输入在任何时候重跑逐位一致）。
fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// 单粒子伪随机字对：`counter` 为发射器内全局递增计数（每尝试发射 +1），
/// `seed` 区分发射器（实体 bits）。同一计数器派生两个独立 64 位字
///（第二路以位反计数器 + 黄金比率常数偏移为输入，splitmix64 雪崩保证
/// 两字不相关）。
fn particle_randoms(counter: u64, seed: u64) -> (u64, u64) {
    (
        mix64(counter.wrapping_add(seed)),
        mix64((!counter).wrapping_add(seed) ^ 0x9E37_79B9_7F4A_7C15),
    )
}

/// 64 位随机字 → [0, 1) 均匀小数（取高 53 位，f64 尾数精度内无网格偏差）。
fn fraction(u: u64) -> f32 {
    (u >> 11) as f32 * (1.0 / (1u64 << 53) as f32)
}

/// 球面均匀单位向量：经典 (z, θ) 参数化——z 均匀 [-1, 1)、θ 均匀 [0, 2π)，
/// 面积元守恒（z 用极轴 = y，与「基准方向 +Y」的出生方向公式配合）。
fn unit_sphere(r: (u64, u64)) -> Vec3 {
    let z = 1.0 - 2.0 * fraction(r.0);
    let theta = std::f32::consts::TAU * fraction(r.1);
    let s = (1.0 - z * z).max(0.0).sqrt();
    Vec3::new(s * theta.cos(), z, s * theta.sin())
}

/// 每帧粒子推进系统：推进所有 [`ParticleEmitter`]（模拟核心见
/// [`ParticleEmitter::advance`]）。`Time` 资源缺席时 dt 视为 0（burst 仍会
/// 首次触发——可先手动推池再定位渲染）。**单遍实现**的理由见模块文档
///（模拟只写发射器自身内部池，无跨组件借用，无需两阶段）。
///
/// 调用契约：在 `propagate_transforms` 之后（发射原点取当帧全局位置）、
/// `Renderer::draw` 之前——DefaultPlugins 的 ParticlePlugin 已保证。
pub fn update_particles(world: &iris_core::world::World) {
    let dt = world
        .try_resource::<Time>()
        .map(|t| t.delta_secs())
        .unwrap_or(0.0);
    world.query::<(Entity, &GlobalTransform, &mut ParticleEmitter)>(|(e, gt, emitter)| {
        emitter.advance(dt, gt.position(), e.to_bits());
    });
}

/// GPU 粒子实例（32 字节，POD；与 `shaders/particle.wgsl` 顶点布局对齐）。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ParticleInstance {
    /// xyz：世界坐标 quad 中心；w：billboard 半边长（世界单位）。
    pub center_size: [f32; 4],
    /// 线性 RGBA（可 HDR）。
    pub color: [f32; 4],
}

impl ParticleInstance {
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<ParticleInstance>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &[
            wgpu::VertexAttribute {
                offset: 0,
                shader_location: 0,
                format: wgpu::VertexFormat::Float32x4,
            },
            wgpu::VertexAttribute {
                offset: 16,
                shader_location: 1,
                format: wgpu::VertexFormat::Float32x4,
            },
        ],
    };

    /// 世界坐标 quad 中心。
    pub fn center(&self) -> Vec3 {
        Vec3::new(
            self.center_size[0],
            self.center_size[1],
            self.center_size[2],
        )
    }
}

const _: () = assert!(
    std::mem::size_of::<ParticleInstance>() == 32,
    "ParticleInstance 必须为 32 字节（与 WGSL 顶点布局一致）"
);

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;
    const SEED: u64 = 0xDEAD_BEEF;

    /// 逐位快照：粒子全字段 + 内部状态的 f32/u64 位型（确定性断言用，
    /// NaN 也不会漏检）。
    fn bits(e: &ParticleEmitter) -> Vec<u64> {
        let mut v: Vec<u64> = e
            .particles()
            .iter()
            .flat_map(|p| {
                [
                    p.pos.x.to_bits() as u64,
                    p.pos.y.to_bits() as u64,
                    p.pos.z.to_bits() as u64,
                    p.vel.x.to_bits() as u64,
                    p.vel.y.to_bits() as u64,
                    p.vel.z.to_bits() as u64,
                    p.age.to_bits() as u64,
                ]
            })
            .collect();
        v.push(e.rng_counter);
        v.push(e.spawn_accumulator.to_bits() as u64);
        v.push(e.emitter_age.to_bits() as u64);
        v.push(e.burst_fired as u64);
        v
    }

    /// 用例 1（核心）：同一配置的两个池按相同序列步进 240 步，粒子池与
    /// 全部内部状态**逐位一致**（含哈希流方向序列一致、余量滚存一致）。
    #[test]
    fn simulation_is_bit_deterministic() {
        let mut a = ParticleEmitter::explosion();
        let mut b = ParticleEmitter::explosion();
        for i in 0..240 {
            // 爆炸预设 1.4 s 寿命 < 240/60 = 4 s：覆盖「发射 → 存活 →
            // 全灭」完整周期与哈希流的全程消耗
            let origin = Vec3::new(1.0, 2.0, 3.0);
            let sa = a.advance(DT, origin, SEED);
            let sb = b.advance(DT, origin, SEED);
            assert_eq!(sa, sb, "第 {i} 步报告应一致");
        }
        assert_eq!(bits(&a), bits(&b), "两次模拟必须逐位一致");
        assert!(a.is_empty(), "4 s ≫ 1.4 s 寿命，池应全灭");
        // 换种子：不同发射器（实体 bits 不同）方向序列不同——种子确实参与。
        // 只跑 30 步（池非空）比较粒子快照
        let mut c = ParticleEmitter::explosion();
        let mut d = ParticleEmitter::explosion();
        for _ in 0..30 {
            c.advance(DT, Vec3::ZERO, SEED + 1);
            d.advance(DT, Vec3::ZERO, SEED + 2);
        }
        assert_ne!(bits(&c), bits(&d), "不同 seed 应产生不同模拟序列");
    }

    /// 哈希流健全性：burst 出生的粒子方向确实各不相同（非退化常量），
    /// 且模长 = initial_speed（jitter 只改方向不改速率）。
    #[test]
    fn hash_stream_produces_varied_directions() {
        let mut e = ParticleEmitter::explosion();
        e.advance(DT, Vec3::ZERO, SEED);
        let particles = e.particles();
        assert_eq!(particles.len(), 320);
        let distinct: usize = {
            let mut dirs: Vec<[u32; 3]> = particles
                .iter()
                .map(|p| {
                    let n = p.vel.normalize();
                    [n.x.to_bits(), n.y.to_bits(), n.z.to_bits()]
                })
                .collect();
            dirs.sort_unstable();
            dirs.dedup();
            dirs.len()
        };
        assert!(
            distinct > 300,
            "320 颗粒子的方向应几乎各不相同，实际 {distinct}"
        );
        for p in particles {
            assert!((p.vel.length() - 7.0).abs() < 1e-4, "速率应等于基线 7.0");
        }
    }

    /// 用例 2：发射计数——rate=60、dt=1/60 每步恰出生 1 粒（余量恒近零）；
    /// rate=30 每两步 1 粒（累计分数余量行为）；rate=60、dt=1 大步长单步
    /// 出生 60 粒。
    #[test]
    fn spawn_counting_is_exact_with_fractional_accumulation() {
        let mut e = ParticleEmitter::new(ParticleBlend::Alpha);
        e.spawn_rate = 60.0;
        e.particle_lifetime = 1000.0;
        e.max_particles = 10_000;
        let mut total = 0;
        for _ in 0..240 {
            total += e.advance(DT, Vec3::ZERO, SEED).spawned;
        }
        assert_eq!(total, 240, "60 粒/秒 × 4 秒应精确出生 240 粒");
        assert_eq!(e.live_count(), 240);

        // 半速率：余量 0.5 逐帧滚存 → 每两步 1 粒，240 步恰 120 粒
        let mut e = ParticleEmitter::new(ParticleBlend::Alpha);
        e.spawn_rate = 30.0;
        e.particle_lifetime = 1000.0;
        let mut total = 0;
        for _ in 0..240 {
            total += e.advance(DT, Vec3::ZERO, SEED).spawned;
        }
        assert_eq!(total, 120, "30 粒/秒 × 4 秒应精确出生 120 粒（余量滚存）");

        // 大步长：单步 1 秒 → 60 粒一步出齐
        let mut e = ParticleEmitter::new(ParticleBlend::Alpha);
        e.spawn_rate = 60.0;
        e.particle_lifetime = 1000.0;
        let report = e.advance(1.0, Vec3::ZERO, SEED);
        assert_eq!(report.spawned, 60);
    }

    /// 用例 3：积分解析对照——jitter=0（方向恰为 +Y）、gravity_scale=2
    /// 时，半隐式欧拉 n 步的解析解 y_n = v₀·n·dt − ½·g·dt²·n(n+1)。
    #[test]
    fn integration_matches_semi_implicit_analytic_solution() {
        let (v0, g_scale, steps) = (6.0f32, 2.0f32, 120u32);
        let mut e = ParticleEmitter::new(ParticleBlend::Alpha);
        e.burst_count = 1; // 恰一颗
        e.spawn_rate = 0.0;
        e.initial_speed = v0;
        e.velocity_jitter = 0.0; // 无抖动 → 方向 = +Y，x/z 恒 0
        e.gravity_scale = g_scale;
        e.particle_lifetime = 1000.0;
        e.advance(DT, Vec3::ZERO, SEED); // burst 出生命题粒子
        assert_eq!(e.live_count(), 1);
        for _ in 0..steps {
            e.advance(DT, Vec3::ZERO, SEED);
        }
        let p = &e.particles()[0];
        let g = 9.81 * g_scale;
        let n = steps as f32;
        let expected_y = v0 * n * DT - 0.5 * g * DT * DT * n * (n + 1.0);
        assert!(
            (p.pos.y - expected_y).abs() < 1e-3,
            "半隐式欧拉解析对照：y = {} 应 ≈ {expected_y}",
            p.pos.y
        );
        assert_eq!(p.pos.x, 0.0);
        assert_eq!(p.pos.z, 0.0, "jitter=0 时水平方向应严格为零");
        assert!((p.vel.y - (v0 - g * n * DT)).abs() < 1e-3);
    }

    /// 用例 4：寿命剔除、发射器寿命耗尽停发、池满丢弃语义。
    #[test]
    fn lifetime_culling_emitter_exhaustion_and_pool_full() {
        // 粒子寿命：0.5 s 后全部出清（先关停持续发射，单独考验剔除）
        let mut e = ParticleEmitter::new(ParticleBlend::Alpha);
        e.spawn_rate = 60.0;
        e.particle_lifetime = 0.5;
        for _ in 0..30 {
            e.advance(DT, Vec3::ZERO, SEED);
        }
        assert_eq!(e.live_count(), 30, "0.5 s 内出生的 30 粒应全部存活");
        e.spawn_rate = 0.0;
        for _ in 0..90 {
            e.advance(DT, Vec3::ZERO, SEED);
        }
        assert!(e.is_empty(), "0.5 s 寿命，停发后再过 1.5 s 应全部剔除");

        // 发射器寿命：0.5 s 耗尽后停发（已出生粒子继续存活）。边界语义：
        // 判据为 emitter_age < emitter_lifetime，30 步后 f32 累计的
        // age = 0.50000001 已越过 0.5——第 30 步起不再发射，故恰 29 粒
        let mut e = ParticleEmitter::new(ParticleBlend::Alpha);
        e.spawn_rate = 60.0;
        e.particle_lifetime = 1000.0;
        e.emitter_lifetime = 0.5;
        let mut total = 0;
        for _ in 0..120 {
            total += e.advance(DT, Vec3::ZERO, SEED).spawned;
        }
        assert_eq!(total, 29, "寿命 0.5 s 内应出生 29 粒，之后停发");
        assert_eq!(e.live_count(), 29);

        // 池满：max=5 < burst=10 → 只入池 5 颗，其余丢弃（不 panic）
        let mut e = ParticleEmitter::new(ParticleBlend::Alpha);
        e.burst_count = 10;
        e.max_particles = 5;
        e.particle_lifetime = 1000.0;
        let report = e.advance(DT, Vec3::ZERO, SEED);
        assert_eq!(report.spawned, 5, "池满应丢弃新粒子");
        assert_eq!(report.alive, 5);
        // 后续持续发射同样被丢弃
        let report = e.advance(DT, Vec3::ZERO, SEED);
        assert_eq!(report.spawned, 0, "池满期间持续发射同样丢弃");
    }

    /// 用例 5：burst 只触发一次——首步爆发 + 之后只有持续发射的零星
    /// 补充，不再重复爆发。
    #[test]
    fn burst_fires_exactly_once() {
        let mut e = ParticleEmitter::new(ParticleBlend::Alpha);
        e.burst_count = 10;
        e.spawn_rate = 5.0; // 1/60 s × 5 = 0.083 粒/步 → 余量滚存
        e.particle_lifetime = 1000.0;
        let first = e.advance(DT, Vec3::ZERO, SEED);
        assert_eq!(first.spawned, 10, "首步应一次性爆发 10 粒");
        let mut rest = 0;
        for _ in 0..24 {
            rest += e.advance(DT, Vec3::ZERO, SEED).spawned;
        }
        assert_eq!(
            rest, 2,
            "24 步只应有持续发射的 2 粒（0.083×24≈2）——burst 不得重复"
        );
    }

    /// 渲染收集：尺寸 / 颜色按 age/particle_lifetime 线性插值（出生 = start，
    /// 寿命终点前一刻趋近 end）。
    #[test]
    fn collect_render_interpolates_size_and_color() {
        let mut e = ParticleEmitter::new(ParticleBlend::Additive);
        e.particle_lifetime = 2.0;
        e.size_start = 1.0;
        e.size_end = 0.0;
        e.color_start = [1.0, 0.0, 0.0, 1.0];
        e.color_end = [0.0, 1.0, 0.0, 0.0];
        e.burst_count = 1;
        e.spawn_rate = 0.0;
        e.initial_speed = 0.0;
        e.gravity_scale = 0.0;
        let mut out = Vec::new();
        e.advance(0.0, Vec3::ZERO, SEED); // dt=0：burst 触发、粒子定格 age=0
        e.collect_render(&mut out);
        assert_eq!(out.len(), 1);
        assert!(
            (out[0].center_size[3] - 1.0).abs() < 1e-6,
            "出生尺寸 = start"
        );
        assert_eq!(out[0].color, [1.0, 0.0, 0.0, 1.0]);

        // 推进 1 s（半程）：尺寸 / 颜色 / alpha 均到中点
        for _ in 0..60 {
            e.advance(DT, Vec3::ZERO, SEED);
        }
        out.clear();
        e.collect_render(&mut out);
        assert!((out[0].center_size[3] - 0.5).abs() < 1e-4);
        assert!((out[0].color[0] - 0.5).abs() < 1e-4);
        assert!((out[0].color[1] - 0.5).abs() < 1e-4);
        assert!((out[0].color[3] - 0.5).abs() < 1e-4);
    }
}
