//! 战斗相机反馈（R68）：屏幕震动 + FOV punch + 战斗取景呼吸——全部
//! 纯函数 + 无状态哈希流，可 headless 单测、逐位确定。
//!
//! # 屏幕震动（trauma 模型）
//!
//! 经典 trauma 包络：事件往 [`CameraFx::trauma`]（0..1）里**加**值，
//! 每帧线性衰减；实际震动幅度 ∝ **trauma²**——多个小震叠加时感知
//! 平缓、接近满格才剧烈（平方把 0..1 压进感知的下半段）。每帧的
//! 震动方向从**无状态哈希流**抽取（splitmix64 × 帧计数器，引擎
//! `particles.rs::mix64` 同款纪律）：不用 wall clock、不用 RNG、
//! 同输入两次模拟逐位一致。
//!
//! 震动 = 平移（XZ 圆内 + 微小 Y）+ 相机 roll（绕视线轴 ±2°）——
//! 固定俯角相机上平移震动的「画面抖」比旋转更自然，roll 只加微量
//! 不安感。衰减 [`TRAUMA_DECAY_PER_SEC`] = 1.8/s：0.45 的一次重击
//! 约 0.25 s 归零，不遮蔽后续输入。
//!
//! # FOV punch
//!
//! [`CameraFx::fov_kick`]（弧度正值 = 拉远）：三段重劈命中 +6°、
//! 玩家受击 +4°，每帧指数衰减——瞬时的视野扩张是近战打击感最廉价
//! 有效的放大器。写回 `ActiveCamera::fov_y_radians`（组件字段可变，
//! 渲染器每帧读——引擎无需改动）。
//!
//! # 战斗取景呼吸
//!
//! [`frame_scale`]：最近活敌距离映射机位偏移缩放——敌人在挥刀距离
//! 附近（< 4 m）时机位拉远至多 12%（留挥砍与击退空间），远距 / 无敌
//! 回到 1.0。帧间平滑由 trauma 同款指数趋近的调用方（平滑系数写在
//! 常量）完成……v1 取直接映射（偏移变化本身就连续），不另设记忆态。
//!
//! # 编排
//!
//! 触发（事件 → add_trauma / add_fov_kick）在 `consume_game_events`；
//! 消费（包络 → 相机 Transform / ActiveCamera）在
//! `write_character_visuals` 的相机块，非演出期生效——演出期时间线
//! 接管相机，trauma 只衰减不上屏（不打断运镜）。

use glam::Vec3;

use iris_core::Resource;

/// 震动强度每秒衰减量（1/秒）：0.45 的重击 trauma 约 0.25 s 归零。
pub const TRAUMA_DECAY_PER_SEC: f32 = 1.8;
/// 最大平移震动幅度（米，trauma = 1 时）。
pub const MAX_SHAKE_OFFSET: f32 = 0.22;
/// 最大 roll 震动（弧度，trauma = 1 时）。
pub const MAX_SHAKE_ROLL: f32 = 2.0_f32.to_radians();
/// FOV kick 每秒衰减量（弧度/秒）：+6° 约 0.4 s 回落。
pub const FOV_DECAY_PER_SEC: f32 = 0.26;
/// 战斗取景的最大机位缩放（最近敌在挥刀距离内时）。
pub const MAX_FRAME_SCALE: f32 = 1.12;
/// 取景缩放的作用半径（米）：最近敌在此距离内开始拉远（= 起手锁敌半径）。
pub const FRAME_RADIUS: f32 = 4.0;

/// 战斗相机反馈状态（Resource）：trauma / fov_kick 记忆态 + 噪声计数器。
#[derive(Debug, Clone, Copy, PartialEq, Resource)]
pub struct CameraFx {
    /// 震动强度（0..1，clamp 加法累积）。
    pub trauma: f32,
    /// FOV kick（弧度，≥ 0）。
    pub fov_kick: f32,
    /// 噪声计数器（每帧 +1；splitmix64 流的游标，无状态可复算）。
    pub noise_counter: u64,
}

impl Default for CameraFx {
    fn default() -> Self {
        Self {
            trauma: 0.0,
            fov_kick: 0.0,
            noise_counter: 0,
        }
    }
}

impl CameraFx {
    /// 加震动（clamp 到 0..1——超格不累积，防连续命中震到不可读）。
    pub fn add_trauma(&mut self, amount: f32) {
        self.trauma = (self.trauma + amount).clamp(0.0, 1.0);
    }

    /// 加 FOV kick（弧度，clamp 到 0..0.3 防叠爆）。
    pub fn add_fov_kick(&mut self, radians: f32) {
        self.fov_kick = (self.fov_kick + radians).clamp(0.0, 0.3);
    }

    /// 一帧推进：线性衰减 + 噪声游标前进。返回 `(平移偏移, roll, fov)`
    /// ——调用方直接加到机位 / 朝向 / 视场上。`trauma`/`fov_kick` 归零
    /// 后偏移恒 `(0,0,0), 0, 0`（无震动 = 逐位等于 R67 之前的相机行为）。
    pub fn advance(&mut self, dt: f32) -> (Vec3, f32, f32) {
        let t2 = self.trauma * self.trauma;
        let offset = if t2 > 0.0 {
            let (dx, dz, dy) = shake_noise(self.noise_counter);
            Vec3::new(dx, dy, dz) * (MAX_SHAKE_OFFSET * t2)
        } else {
            Vec3::ZERO
        };
        let roll = if t2 > 0.0 {
            roll_noise(self.noise_counter) * (MAX_SHAKE_ROLL * t2)
        } else {
            0.0
        };
        let fov = self.fov_kick;
        self.trauma = (self.trauma - TRAUMA_DECAY_PER_SEC * dt).max(0.0);
        self.fov_kick = (self.fov_kick - FOV_DECAY_PER_SEC * dt).max(0.0);
        self.noise_counter = self.noise_counter.wrapping_add(1);
        (offset, roll, fov)
    }
}

/// splitmix64 终结混洗（引擎 `particles.rs::mix64` 同款常量）。
fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// 平移噪声：一个 64 位字派生 XZ 单位圆内方向 + 1/4 幅度的 Y 抖动
/// （固定俯角相机的画面内抖动以水平为主，竖直分量克制）。
/// 输出无偏：`u64 → [0,1)` 用高 32 位（低位雪崩弱，弃用）。
fn shake_noise(counter: u64) -> (f32, f32, f32) {
    let h = mix64(counter.wrapping_add(0x0061_7070_6C65_7321)); // "apples!" 派生盐
    let angle = (h >> 32) as f32 / u32::MAX as f32 * std::f32::consts::TAU;
    let r = ((h & 0xFFFF_FFFF) as f32 / u32::MAX as f32).sqrt(); // 圆内均匀
    let y = (mix64(h) >> 32) as f32 / u32::MAX as f32 - 0.5;
    (angle.cos() * r, angle.sin() * r, y * 0.25)
}

/// roll 噪声：独立 64 位字 → [-0.5, 0.5)。
fn roll_noise(counter: u64) -> f32 {
    (mix64(counter.wrapping_add(0x726F_6C6C_2162_6164)) >> 32) as f32 / u32::MAX as f32 - 0.5
}

/// 战斗取景缩放：最近活敌距离 `dist`（米）→ 机位偏移缩放因子。
/// `dist >= FRAME_RADIUS`（含无敌 → 调用方传 `f32::INFINITY`）恒 1.0；
/// 越近越拉远，线性到 [`MAX_FRAME_SCALE`]。非有限 `dist` 恒 1.0
/// （无效输入不缩放）。
pub fn frame_scale(dist: f32) -> f32 {
    if !dist.is_finite() || dist >= FRAME_RADIUS {
        1.0
    } else {
        1.0 + (1.0 - dist / FRAME_RADIUS) * (MAX_FRAME_SCALE - 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// trauma 加法累积 + clamp 0..1；超格不爆表。
    #[test]
    fn trauma_clamps_at_one() {
        let mut fx = CameraFx::default();
        fx.add_trauma(0.4);
        assert!((fx.trauma - 0.4).abs() < 1e-6);
        fx.add_trauma(0.4);
        fx.add_trauma(0.4);
        assert_eq!(fx.trauma, 1.0, "clamp 到满格");
        fx.add_fov_kick(0.2);
        fx.add_fov_kick(0.2);
        assert_eq!(fx.fov_kick, 0.3, "fov kick clamp 0.3");
    }

    /// 衰减：线性精确值；归零后 advance 产出零偏移 / 零 roll / 零 fov
    ///（= 逐位回到无反馈行为）。
    #[test]
    fn decay_is_linear_and_idle_is_zero() {
        let mut fx = CameraFx::default();
        fx.add_trauma(0.4);
        fx.add_fov_kick(6.0f32.to_radians());
        fx.advance(0.1);
        assert!(
            (fx.trauma - (0.4 - 0.18)).abs() < 1e-6,
            "trauma 衰减 1.8 × 0.1"
        );
        let expected_fov = 6.0f32.to_radians() - 0.26 * 0.1;
        assert!(
            (fx.fov_kick - expected_fov).abs() < 1e-6,
            "fov 线性衰减 0.26 rad/s"
        );
        // 归零：连续推进到耗尽
        for _ in 0..120 {
            fx.advance(1.0 / 60.0);
        }
        assert_eq!(fx.trauma, 0.0);
        assert_eq!(fx.fov_kick, 0.0);
        let (offset, roll, fov) = fx.advance(1.0 / 60.0);
        assert_eq!(offset, Vec3::ZERO, "静默帧零偏移");
        assert_eq!(roll, 0.0);
        assert_eq!(fov, 0.0);
    }

    /// 震动包络上界：任意噪声帧的偏移模长 ≤ MAX_SHAKE_OFFSET × trauma²，
    /// |roll| ≤ MAX_SHAKE_ROLL × trauma²；全 4096 计数器值扫描不越界。
    #[test]
    fn shake_envelope_stays_bounded() {
        let max = MAX_SHAKE_OFFSET * 0.36; // trauma 0.6 → t² = 0.36
        for counter in 0..4096u64 {
            let (dx, dz, dy) = shake_noise(counter);
            let horizontal = (dx * dx + dz * dz).sqrt();
            assert!(
                horizontal <= 1.0 + 1e-4,
                "水平分量应在单位圆内（counter {counter}，模长 {horizontal}）"
            );
            assert!(dy.abs() <= 0.25 + 1e-4, "竖直分量 ≤ 1/4 幅度");
            let roll = roll_noise(counter);
            assert!(roll.abs() <= 0.5 + 1e-6, "roll 噪声 ∈ [-0.5, 0.5)");
            let _ = max;
        }
    }

    /// 噪声确定性：同 counter 逐位一致、跨 counter 至少平移不同
    ///（哈希流不是常数源）。
    #[test]
    fn noise_is_deterministic_bitwise() {
        let (a_x, a_z, a_y) = shake_noise(7);
        let (b_x, b_z, b_y) = shake_noise(7);
        assert_eq!(a_x.to_bits(), b_x.to_bits());
        assert_eq!(a_z.to_bits(), b_z.to_bits());
        assert_eq!(a_y.to_bits(), b_y.to_bits());
        let (c_x, _, _) = shake_noise(8);
        assert_ne!(a_x.to_bits(), c_x.to_bits(), "相邻计数器应产出不同流");
    }

    /// advance 推进噪声游标：同 trauma 下相邻帧偏移方向不同（震动才
    /// 抖得起来），且两次「同状态双跑」逐位一致（确定性回归口）。
    #[test]
    fn advance_shakes_across_frames_and_is_deterministic() {
        let mut a = CameraFx {
            trauma: 0.5,
            ..CameraFx::default()
        };
        let mut b = a;
        let (o1, ra1, fa1) = a.advance(1.0 / 60.0);
        let (o2, ra2, fa2) = a.advance(1.0 / 60.0);
        assert_ne!(o1.x.to_bits(), o2.x.to_bits(), "相邻帧抖动方向应不同");
        // b 双跑：与 a 的同序输出逐位一致（同状态 ⇒ 同轨迹）
        let (p1, rb1, fb1) = b.advance(1.0 / 60.0);
        let (p2, rb2, fb2) = b.advance(1.0 / 60.0);
        assert_eq!(o1.x.to_bits(), p1.x.to_bits());
        assert_eq!(o1.y.to_bits(), p1.y.to_bits());
        assert_eq!(o1.z.to_bits(), p1.z.to_bits());
        assert_eq!(ra1.to_bits(), rb1.to_bits());
        assert_eq!(fa1.to_bits(), fb1.to_bits());
        assert_eq!(o2.x.to_bits(), p2.x.to_bits());
        assert_eq!(o2.y.to_bits(), p2.y.to_bits());
        assert_eq!(o2.z.to_bits(), p2.z.to_bits());
        assert_eq!(ra2.to_bits(), rb2.to_bits());
        assert_eq!(fa2.to_bits(), fb2.to_bits());
    }

    /// 取景缩放：端点与线性中点精确值；无敌 / 远距 / 非有限恒 1.0。
    #[test]
    fn frame_scale_endpoints_exact() {
        assert_eq!(frame_scale(f32::INFINITY), 1.0);
        assert_eq!(frame_scale(100.0), 1.0);
        assert_eq!(frame_scale(4.0), 1.0, "恰在半径上不缩放");
        assert_eq!(frame_scale(2.0), 1.06, "中点线性");
        assert!(
            (frame_scale(0.0) - MAX_FRAME_SCALE).abs() < 1e-6,
            "贴脸最大"
        );
        assert_eq!(frame_scale(f32::NAN), 1.0);
    }
}
