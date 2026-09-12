//! 纯游戏逻辑核心：确定性波次生存状态机，**只依赖 glam、零引擎依赖**。
//!
//! 「Iris Arena」的全部规则（移动 / 冲刺 / 自动开火 / 弹道 / 追击 AI /
//! 接触伤害与无敌帧 / 连击计分 / 波次推进 / 胜负）都在这个模块里以纯函数
//! 推进：输入 [`ArenaInput`]、步长 `dt` 与静态体列表，输出状态变化——
//! 不读时钟、不做随机、不触碰引擎。因此：
//! - 全部规则可毫秒级单测（本模块底部 18 项）；
//! - 插件层（`crate::plugin`）只负责把 `Input` / `Time` / 场景 `Collider`
//!   翻译成本模块类型，再把结果写回实体组件与对象池；
//! - headless 测试与离屏快照共享同一条确定性推进路径。
//!
//! # 与引擎物理的关系（近似性说明）
//!
//! 竞技场是俯视玩法，全部碰撞发生在 XZ 平面：本模块自带一个**本地简化版
//! 静态推离**（`resolve`：圆-圆与圆-AABB、Gauss-Seidel 顺序累加）与
//! **扫掠命中判定**（`segment_blocked` / `segment_hits_circle`：弹道本帧
//! 的 prev → pos 线段对目标的扫掠测试）——推离语义与
//! `iris_core::physics::resolve_static_push` 在水平面上的投影近似（同为
//! 「沿接触法线推离穿透量、逐个用修正后的位置重测」），扫掠语义与
//! `iris_core::physics::segment_hits_shape` 一致（防高速穿模），但刻意
//! **解耦**：纯核心不依赖引擎类型，[`StaticShape`] 是本地的轻量形状
//! （位置里的 y 分量被忽略）。插件层负责把场景 `Collider` 转换过来；实体
//! 全部贴地平面运动，垂直方向无需引擎参与。
//!
//! # 坐标约定
//!
//! 世界 XZ 平面为地面（y 向上，本模块只用 x/z；[`Vec2`] 的 y 分量即世界
//! z）。玩家朝向 `facing` 为绕 Y 偏航角，`facing = 0` 时前方是 -Z（与引擎
//! `Transform::forward` 约定一致）；敌人模型头部朝本地 +X，由插件做 +90°
//! 模型偏航补偿。

use glam::{Vec2, Vec3};

// ---------------------------------------------------------------------------
// 全部可调常量（pub：单测 / 插件层 / 文档共用同一事实来源）
// ---------------------------------------------------------------------------

/// 玩家移动速度（米/秒）。
pub const PLAYER_SPEED: f32 = 6.0;
/// 冲刺速度（米/秒）：冲刺期间替代移动速度。
pub const DASH_SPEED: f32 = 20.0;
/// 冲刺持续时间（秒）：高速位移窗口。
pub const DASH_TIME: f32 = 0.15;
/// 冲刺冷却（秒）：从冲刺开始计时。
pub const DASH_COOLDOWN: f32 = 0.9;
/// 冲刺无敌（秒）：从冲刺开始计（覆盖 0.15 s 冲刺 + 0.1 s 余韵）。
pub const DASH_INVULN: f32 = 0.25;
/// 受伤无敌（秒）：接触受击后的无敌窗口（窗口内只掉一次血）。
pub const HURT_INVULN: f32 = 0.8;
/// 自动开火间隔（秒）。
pub const FIRE_INTERVAL: f32 = 0.25;
/// 自动开火射程（米，XZ 平面距离）：只朝射程内**最近**的敌人开火。
pub const FIRE_RANGE: f32 = 14.0;
/// 弹道速度（米/秒）。
pub const PROJECTILE_SPEED: f32 = 22.0;
/// 弹道寿命（秒）：约 28 m 后自灭（覆盖竞技场对角的追击射程）。
pub const PROJECTILE_LIFE: f32 = 1.3;
/// 弹道命中半径（米）：与敌人半径求和判交。
pub const PROJECTILE_RADIUS: f32 = 0.25;
/// 弹道容量上限（同时在场的弹道数；插件层弹道实体池同规格）。
pub const MAX_PROJECTILES: usize = 40;
/// 玩家碰撞半径（米）。
pub const PLAYER_RADIUS: f32 = 0.45;
/// 玩家最大生命（点）：归零即 GAME OVER。
pub const PLAYER_MAX_HP: i32 = 5;
/// 敌人碰撞半径（米）。
pub const ENEMY_RADIUS: f32 = 0.55;
/// 敌人最大生命（点）：每发弹道 -1。
pub const ENEMY_MAX_HP: i32 = 3;
/// 敌人基础追击速度（米/秒，第 1 波）。
pub const ENEMY_BASE_SPEED: f32 = 2.2;
/// 敌人追击速度的每波增量（米/秒）：第 n 波 = 基础 + 增量 × (n−1)。
pub const ENEMY_SPEED_PER_WAVE: f32 = 0.15;
/// 敌人受击硬直闪烁时长（秒，表现层缩放脉冲用）。
pub const ENEMY_HIT_FLASH: f32 = 0.12;
/// 接触伤害（点/次）：敌人碰到玩家。
pub const CONTACT_DAMAGE: i32 = 1;
/// 玩家被撞击退（米/秒冲量，指数衰减）。
pub const PLAYER_KNOCKBACK: f32 = 7.0;
/// 敌人被弹道命中击退（米/秒冲量，指数衰减）。
pub const ENEMY_KNOCKBACK: f32 = 4.5;
/// 击退速度的指数衰减速率（1/秒）：v ×= e^(−decay·dt)。
pub const KNOCKBACK_DECAY: f32 = 8.0;
/// 连击保持窗口（秒）：窗口内无击杀则连击清零。
pub const COMBO_WINDOW: f32 = 3.0;
/// 每次击杀的基础得分：实际得分 = 基础 × 当前连击数。
pub const SCORE_PER_KILL: u32 = 25;
/// 第 n 波敌人数 = 基数 + 步进 × n（n = 1 → 5 只）。
pub const WAVE_BASE: u32 = 3;
/// 每波敌人数步进。
pub const WAVE_SCALE: u32 = 2;
/// 波次间歇（秒）：清空一波后的喘息时间。
pub const BREATHER_TIME: f32 = 3.0;
/// 敌人出生环内缩（米）：出生在围栏内侧一圈（arena_half − 内缩）。
pub const SPAWN_RING_INSET: f32 = 2.0;
/// 敌人对象池容量：同时**在场**的敌人数上限（第 7 波起人数封顶于此）。
pub const MAX_ENEMIES: usize = 15;
/// 玩家转身锐度：朝向平滑的指数逼近速率（1/秒），iris-game 同款手感参数。
const TURN_SHARPNESS: f32 = 12.0;

// ---------------------------------------------------------------------------
// 本地轻量形状（与引擎 ColliderShape 解耦）
// ---------------------------------------------------------------------------

/// 静态体形状（俯视 2D）：纯核心的本地形状，插件层从场景 `ColliderShape`
/// 转换（Box 只取 XZ 半边长——俯视玩法垂直方向无碰撞）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StaticShape {
    /// 圆柱 / 球在 XZ 面的投影：圆。
    Sphere { radius: f32 },
    /// 轴对齐盒在 XZ 面的投影：矩形（XZ 半边长）。
    Box { half_x: f32, half_z: f32 },
}

// ---------------------------------------------------------------------------
// 输入
// ---------------------------------------------------------------------------

/// 一帧的游戏输入：移动方向（世界系，未归一化）+ 边沿触发的功能键。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArenaInput {
    /// 世界 X 方向移动期望（+1 = 屏幕右）。
    pub move_x: f32,
    /// 世界 Z 方向移动期望（+1 = 屏幕近处；-Z 是「屏幕里侧」）。
    pub move_z: f32,
    /// 本帧刚按下冲刺（边沿触发：按住不放只触发一次）。
    pub dash: bool,
    /// 本帧刚按下「任意键」（Title 开局）。
    pub confirm: bool,
    /// 本帧刚按下重开键 R（GameOver → 直接重开 Playing，见 [`ArenaState::restart`]）。
    pub restart: bool,
}

impl ArenaInput {
    /// 无输入（静止）。
    pub const IDLE: Self = Self {
        move_x: 0.0,
        move_z: 0.0,
        dash: false,
        confirm: false,
        restart: false,
    };

    /// 是否存在移动期望。
    pub fn is_moving(&self) -> bool {
        self.move_x != 0.0 || self.move_z != 0.0
    }
}

// ---------------------------------------------------------------------------
// 状态
// ---------------------------------------------------------------------------

/// 游戏阶段：标题 / 进行中 / 结束。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// 标题画面：任意键（[`ArenaInput::confirm`]）开局。
    Title,
    /// 对局进行中。
    Playing,
    /// 玩家生命归零：按 R（[`ArenaInput::restart`]）直接重开。
    GameOver,
}

/// 玩家状态：XZ 位置 + 生命 / 无敌 / 冲刺 / 开火计时 + 朝向 + 击退速度。
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerState {
    /// XZ 平面位置（贴地：俯视玩法无 y 轴；y 分量即世界 z）。
    pub pos: Vec2,
    pub hp: i32,
    /// 无敌剩余（秒）：> 0 时接触不掉血（受伤 / 冲刺来源共用一个字段）。
    pub invuln: f32,
    /// 冲刺剩余（秒）：> 0 时以 [`DASH_SPEED`] 沿 `dash_dir` 位移。
    pub dash_timer: f32,
    /// 冲刺冷却剩余（秒）。
    pub dash_cd: f32,
    /// 开火间隔剩余（秒）。
    pub fire_timer: f32,
    /// 冲刺方向（冲刺开始时的移动方向或朝向，冲刺期间恒定）。
    pub dash_dir: Vec2,
    /// 击退速度（米/秒，指数衰减）。
    pub kb: Vec2,
    /// 朝向（绕 Y 偏航角，弧度）：最后非零移动方向；前方 = (-sin, -cos)。
    pub facing: f32,
}

/// 敌人状态：稳定 id + XZ 位置 + 生命 + 受击闪烁 + 击退速度。
///
/// `id` 由 [`ArenaState`] 的 `next_id` 单调分配——敌人从 `enemies` Vec
/// 死亡移除后下标会变，插件层的敌人池以 id（而非下标）做槽位映射。
#[derive(Clone, Debug, PartialEq)]
pub struct EnemyState {
    /// 稳定 id：同一次对局内唯一、从不复用。
    pub id: u32,
    pub pos: Vec2,
    pub hp: i32,
    /// 受击闪烁剩余（秒）：插件层做缩放脉冲表现。
    pub hit_flash: f32,
    /// 击退速度（米/秒，指数衰减）。
    pub kb: Vec2,
}

/// 弹道状态：稳定 id + XZ 位置 / 速度 + 剩余寿命。
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectileState {
    /// 稳定 id：弹道消亡后不复用（插件层弹道池以 id 映射槽位）。
    pub id: u32,
    pub pos: Vec2,
    pub vel: Vec2,
    /// 剩余寿命（秒）：耗尽即消亡。
    pub life: f32,
}

/// 游戏全局状态：阶段 + 玩家 + 敌人 / 弹道列表 + 波次 / 连击 / 计分。
///
/// 字段全部公开：测试可以直接构造 / 修改中间状态（如「假定第 3 波」），
/// 插件层按只读约定消费（每帧由 [`ArenaState::step`] 独占推进）。
#[derive(Clone, Debug)]
pub struct ArenaState {
    pub phase: Phase,
    pub player: PlayerState,
    /// 在场敌人（死亡即移除；`retain` 保序——确定性遍历序的一部分）。
    pub enemies: Vec<EnemyState>,
    /// 在场弹道（撞墙 / 命中 / 寿命耗尽即移除）。
    pub projectiles: Vec<ProjectileState>,
    /// 当前波次（从 1 起；Title 阶段为 0）。
    pub wave: u32,
    pub score: u32,
    /// 当前连击数（击杀 +1；受伤或 [`COMBO_WINDOW`] 秒无击杀清零）。
    pub combo: u32,
    /// 连击剩余窗口（秒）。
    pub combo_timer: f32,
    /// 波次间歇剩余（秒）：> 0 表示上一波已清空、正在倒计时下一波。
    pub breather: f32,
    /// 当前波次的敌人追击速度（米/秒，由 [`ArenaState::begin_wave`] 按波次算出）。
    pub enemy_speed: f32,
    /// 下一个敌人 / 弹道 id（单调递增，从不复用）。
    pub next_id: u32,
    /// 累计事件计数（单调递增）：插件层做前后差分触发音效，逻辑层只加不减。
    pub total_shots: u32,
    pub total_hits: u32,
    pub total_kills: u32,
    pub total_damage: u32,
    /// 竞技场半边长（米）：所有实体被钳制在 ±arena_half 内（安全网；
    /// 正常对局由场景墙体 collider 先挡住）。
    pub arena_half: f32,
}

impl ArenaState {
    /// 新状态：标题阶段（空场，等任意键开局）。
    pub fn new(arena_half: f32) -> Self {
        Self {
            phase: Phase::Title,
            player: PlayerState {
                pos: Vec2::ZERO,
                hp: PLAYER_MAX_HP,
                invuln: 0.0,
                dash_timer: 0.0,
                dash_cd: 0.0,
                fire_timer: 0.0,
                dash_dir: Vec2::NEG_Y, // y 分量即世界 -Z（朝向 0 的前方）
                kb: Vec2::ZERO,
                facing: 0.0,
            },
            enemies: Vec::new(),
            projectiles: Vec::new(),
            wave: 0,
            score: 0,
            combo: 0,
            combo_timer: 0.0,
            breather: 0.0,
            enemy_speed: ENEMY_BASE_SPEED,
            next_id: 0,
            total_shots: 0,
            total_hits: 0,
            total_kills: 0,
            total_damage: 0,
            arena_half,
        }
    }

    /// 开局 / 重开：完整复位到「第 1 波进行中」。
    ///
    /// 设计取舍（Game Over 的重开语义）：R 键**直接重开 Playing**（回
    /// Title 再按任意键是纯多余的一步、无信息增量）——选实现更直白的
    /// 路径并在此注明。Title 的任意键开局同样走这里。
    pub fn restart(&mut self) {
        let mut fresh = Self::new(self.arena_half);
        fresh.phase = Phase::Playing;
        fresh.wave = 1;
        fresh.begin_wave(1);
        *self = fresh;
    }

    /// 生成第 `n` 波：敌人数 = [`WAVE_BASE`] + [`WAVE_SCALE`] × n（
    /// [`MAX_ENEMIES`] 封顶），出生在场地边缘的确定性环上（角度 = 均分 +
    /// 波次相位，无随机源），追击速度按波次递增。
    pub fn begin_wave(&mut self, n: u32) {
        self.wave = n;
        self.enemy_speed = ENEMY_BASE_SPEED + ENEMY_SPEED_PER_WAVE * (n - 1) as f32;
        let count = (WAVE_BASE + WAVE_SCALE * n).min(MAX_ENEMIES as u32) as usize;
        let ring = self.arena_half - SPAWN_RING_INSET;
        let phase_offset = 0.6 * n as f32;
        for i in 0..count {
            let angle = (i as f32) * (std::f32::consts::TAU / count as f32) + phase_offset;
            let (s, c) = angle.sin_cos();
            let id = self.next_id;
            self.next_id += 1;
            self.enemies.push(EnemyState {
                id,
                pos: Vec2::new(c * ring, s * ring),
                hp: ENEMY_MAX_HP,
                hit_flash: 0.0,
                kb: Vec2::ZERO,
            });
        }
    }

    /// 推进一帧。非 Playing 阶段只处理阶段转换；`dt <= 0` 时冻结对局。
    ///
    /// 帧内顺序（固定，是确定性的一部分）：
    /// 阶段转换 → 计时器 → 玩家移动/冲刺 → 玩家推离 → 自动开火 →
    /// 弹道推进与命中 → 敌人追击与推离 → 接触伤害 → 连击衰减 → 波次推进。
    pub fn step(&mut self, input: &ArenaInput, dt: f32, statics: &[(Vec3, StaticShape)]) {
        // ---- 阶段转换（Title / GameOver 只消费确认 / 重开键）----
        match self.phase {
            Phase::Title => {
                if input.confirm || input.restart {
                    self.restart();
                }
                return;
            }
            Phase::GameOver => {
                if input.restart {
                    self.restart();
                }
                return;
            }
            Phase::Playing => {}
        }
        if dt <= 0.0 {
            return;
        }

        // ---- 计时器（无敌 / 冲刺 / 冷却 / 开火 / 受击闪烁）----
        self.player.invuln = (self.player.invuln - dt).max(0.0);
        self.player.dash_timer = (self.player.dash_timer - dt).max(0.0);
        self.player.dash_cd = (self.player.dash_cd - dt).max(0.0);
        self.player.fire_timer = (self.player.fire_timer - dt).max(0.0);
        for enemy in &mut self.enemies {
            enemy.hit_flash = (enemy.hit_flash - dt).max(0.0);
        }

        // ---- 玩家移动：冲刺期沿 dash_dir 高速位移，否则按输入积分 ----
        let move_dir = {
            let wish = Vec2::new(input.move_x, input.move_z);
            let len = wish.length();
            if len > 1e-6 { wish / len } else { Vec2::ZERO }
        };
        if input.dash && self.player.dash_cd <= 0.0 {
            // 冲刺：方向取当前移动方向（静止时取朝向前方），同时进入无敌
            self.player.dash_timer = DASH_TIME;
            self.player.dash_cd = DASH_COOLDOWN;
            self.player.invuln = self.player.invuln.max(DASH_INVULN);
            self.player.dash_dir = if move_dir != Vec2::ZERO {
                move_dir
            } else {
                self.player.forward()
            };
        }
        let velocity = if self.player.dash_timer > 0.0 {
            self.player.dash_dir * DASH_SPEED
        } else {
            move_dir * PLAYER_SPEED
        };
        // 朝向平滑转身（冲刺期方向已由 dash_dir 锁定，不转身）
        if move_dir != Vec2::ZERO {
            let target = (-move_dir.x).atan2(-move_dir.y);
            let mut delta = target - self.player.facing;
            delta = delta.rem_euclid(std::f32::consts::TAU);
            if delta > std::f32::consts::PI {
                delta -= std::f32::consts::TAU;
            }
            self.player.facing += delta * (TURN_SHARPNESS * dt).clamp(0.0, 1.0);
        }
        self.player.pos += (velocity + self.player.kb) * dt;
        self.player.kb *= (-KNOCKBACK_DECAY * dt).exp();
        self.clamp_player();

        // ---- 玩家对静态体推离（运动学控制器；只沿法线 → 贴墙滑行）----
        self.player.pos += resolve(self.player.pos, PLAYER_RADIUS, statics);
        self.clamp_player();

        // ---- 自动开火：射程内最近敌人（无敌人 / 弹道池满不开火）----
        let target_pos = if self.player.fire_timer <= 0.0 {
            self.nearest_enemy_in_range().map(|e| e.pos)
        } else {
            None
        };
        if let Some(target_pos) = target_pos {
            if self.projectiles.len() < MAX_PROJECTILES {
                let dir = (target_pos - self.player.pos).normalize_or_zero();
                let id = self.next_id;
                self.next_id += 1;
                self.projectiles.push(ProjectileState {
                    id,
                    pos: self.player.pos,
                    vel: dir * PROJECTILE_SPEED,
                    life: PROJECTILE_LIFE,
                });
                self.player.fire_timer = FIRE_INTERVAL;
                self.total_shots += 1;
            }
        }

        // ---- 弹道推进：寿命 → 静态体阻挡 → 命中敌人（一弹至多一敌）----
        // 命中 / 阻挡都用本帧扫掠线段（prev → pos）判定：弹速 × dt 的步长
        // 可大于敌人 / 障碍直径，当帧点判定会**穿模漏检**（引擎
        // `segment_hits_shape` 同款语义，XZ 平面本地实现保持纯核心零引擎依赖）
        let mut dead_projectiles: Vec<u32> = Vec::new();
        for projectile in &mut self.projectiles {
            let prev = projectile.pos;
            projectile.pos += projectile.vel * dt;
            projectile.life -= dt;
            if projectile.life <= 0.0
                || segment_blocked(prev, projectile.pos, PROJECTILE_RADIUS, statics)
            {
                dead_projectiles.push(projectile.id);
                continue;
            }
            // 命中判定按敌人列表序取首个（确定性）；扫掠半径 = 半径求和
            let hit_radius = ENEMY_RADIUS + PROJECTILE_RADIUS;
            let dir = projectile.vel.normalize_or_zero();
            for enemy in &mut self.enemies {
                if segment_hits_circle(prev, projectile.pos, enemy.pos, hit_radius) {
                    enemy.hp -= 1;
                    enemy.hit_flash = ENEMY_HIT_FLASH;
                    enemy.kb += dir * ENEMY_KNOCKBACK;
                    dead_projectiles.push(projectile.id);
                    self.total_hits += 1;
                    if enemy.hp <= 0 {
                        // 击杀：连击 +1、窗口重置、得分 = 基础 × 连击
                        self.combo += 1;
                        self.combo_timer = COMBO_WINDOW;
                        self.score += SCORE_PER_KILL * self.combo;
                        self.total_kills += 1;
                    }
                    break;
                }
            }
        }
        self.projectiles
            .retain(|p| !dead_projectiles.contains(&p.id));
        self.enemies.retain(|e| e.hp > 0);

        // ---- 敌人追击：朝玩家方向 + 击退速度积分，随后对静态体推离 ----
        for enemy in &mut self.enemies {
            let dir = (self.player.pos - enemy.pos).normalize_or_zero();
            enemy.pos += (dir * self.enemy_speed + enemy.kb) * dt;
            enemy.kb *= (-KNOCKBACK_DECAY * dt).exp();
            enemy.pos += resolve(enemy.pos, ENEMY_RADIUS, statics);
            let half = self.arena_half;
            enemy.pos = enemy.pos.clamp(Vec2::splat(-half), Vec2::splat(half));
        }

        // ---- 接触伤害：无敌窗口内不掉血（同一窗口只掉一次）----
        if self.player.invuln <= 0.0 {
            for enemy in &self.enemies {
                if enemy.pos.distance(self.player.pos) <= ENEMY_RADIUS + PLAYER_RADIUS {
                    self.player.hp -= CONTACT_DAMAGE;
                    self.total_damage += 1;
                    self.player.invuln = HURT_INVULN;
                    self.combo = 0;
                    self.combo_timer = 0.0;
                    // 击退方向：从敌人指向玩家
                    let away = (self.player.pos - enemy.pos).normalize_or_zero();
                    self.player.kb += away * PLAYER_KNOCKBACK;
                    if self.player.hp <= 0 {
                        self.player.hp = 0;
                        self.phase = Phase::GameOver;
                        return;
                    }
                }
            }
        }

        // ---- 连击衰减：窗口耗尽清零（受伤清零已在上面处理）----
        if self.combo > 0 {
            self.combo_timer -= dt;
            if self.combo_timer <= 0.0 {
                self.combo = 0;
                self.combo_timer = 0.0;
            }
        }

        // ---- 波次推进：清空 → 间歇倒计时 → 下一波（更多更快）----
        if self.enemies.is_empty() {
            if self.breather <= 0.0 {
                // 清空当帧：开始间歇（弹道余烬照常飞完，无碍）
                self.breather = BREATHER_TIME;
            } else {
                self.breather -= dt;
                if self.breather <= 0.0 {
                    self.breather = 0.0;
                    self.begin_wave(self.wave + 1);
                }
            }
        }
    }

    /// 射程内最近的敌人（无则 `None`）；距离并列时取列表序靠前者（确定性）。
    fn nearest_enemy_in_range(&self) -> Option<&EnemyState> {
        let mut best: Option<(f32, &EnemyState)> = None;
        for enemy in &self.enemies {
            let d = enemy.pos.distance(self.player.pos);
            if d <= FIRE_RANGE && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, enemy));
            }
        }
        best.map(|(_, e)| e)
    }

    /// 玩家位置钳制（场景缺失 / 加载失败时的边界安全网）。
    fn clamp_player(&mut self) {
        let half = self.arena_half;
        self.player.pos = self.player.pos.clamp(Vec2::splat(-half), Vec2::splat(half));
    }
}

impl PlayerState {
    /// 前方单位向量（XZ 平面）。与引擎 `Transform::forward` 同约定：
    /// `facing = 0` 指向 -Z。
    pub fn forward(&self) -> Vec2 {
        Vec2::new(-self.facing.sin(), -self.facing.cos())
    }
}

// ---------------------------------------------------------------------------
// 本地静态碰撞（与引擎 resolve_static_push 的 XZ 投影近似，见模块文档）
// ---------------------------------------------------------------------------

/// 把 `pos` 处半径 `radius` 的圆从全部静态体重叠中沿法线推出（XZ 平面）。
/// Gauss-Seidel 风格：逐个静态体推离、后续用修正后的位置重测；推离只沿
/// 接触法线 → 调用方保留切向速度即得「贴墙滑行」。
fn resolve(pos: Vec2, radius: f32, statics: &[(Vec3, StaticShape)]) -> Vec2 {
    let mut pos = pos;
    let mut push = Vec2::ZERO;
    for (center, shape) in statics {
        let c = Vec2::new(center.x, center.z);
        let contact = match *shape {
            StaticShape::Sphere { radius: r } => circle_circle(pos, radius, c, r),
            StaticShape::Box { half_x, half_z } => circle_box(pos, radius, half_x, half_z, c),
        };
        if let Some((normal, penetration)) = contact {
            pos -= normal * penetration;
            push -= normal * penetration;
        }
    }
    push
}

/// 弹道本帧扫掠线段（`from → to`，半径 `radius`）是否撞上任意静态体
/// （命中即消亡，不做推离）。替代旧的当帧点判定 `overlaps_static`——
/// 弹速 × dt 步长可大于障碍直径，点判定会穿模漏检（见模块文档）。
fn segment_blocked(from: Vec2, to: Vec2, radius: f32, statics: &[(Vec3, StaticShape)]) -> bool {
    statics.iter().any(|(center, shape)| {
        let c = Vec2::new(center.x, center.z);
        match *shape {
            StaticShape::Sphere { radius: r } => segment_hits_circle(from, to, c, r + radius),
            StaticShape::Box { half_x, half_z } => {
                // 盒按半边长 + 弹半径扩张（方角近似 Minkowski 和的圆角，
                // 角点处至多多判 r·(√2−1) ≈ 0.1 m——弹道消亡语义下可接受）
                segment_hits_box(from, to, c, half_x + radius, half_z + radius)
            }
        }
    })
}

/// 线段-圆判交（XZ 平面）：线段 `from → to` 上到 `center` 的最近点距
/// ≤ `radius` 即相交（参数钳到 [0,1] 的最近点，纯几何无迭代）。
fn segment_hits_circle(from: Vec2, to: Vec2, center: Vec2, radius: f32) -> bool {
    let d = to - from;
    let len_sq = d.length_squared();
    if len_sq <= 1e-12 {
        // 零长度线段退化为点判定
        return from.distance_squared(center) <= radius * radius;
    }
    let t = ((center - from).dot(d) / len_sq).clamp(0.0, 1.0);
    let closest = from + d * t;
    closest.distance_squared(center) <= radius * radius
}

/// 线段-矩形判交（XZ 轴对齐，slab 法）：线段 `from → to` 是否进入以
/// `center` 为心、半边长 `(half_x, half_z)` 的矩形（t 归一到 [0,1]）。
/// 分母近零的轴按「起点在界内则不裁剪、界外则未命中」处理。
fn segment_hits_box(from: Vec2, to: Vec2, center: Vec2, half_x: f32, half_z: f32) -> bool {
    let d = to - from;
    let local = from - center;
    let mut t_min = 0.0f32;
    let mut t_max = 1.0f32;
    for (delta, offset, half) in [(d.x, local.x, half_x), (d.y, local.y, half_z)] {
        if delta.abs() <= 1e-12 {
            if offset.abs() > half {
                return false;
            }
            continue;
        }
        let t1 = (-half - offset) / delta;
        let t2 = (half - offset) / delta;
        let (near, far) = if t1 <= t2 { (t1, t2) } else { (t2, t1) };
        t_min = t_min.max(near);
        t_max = t_max.min(far);
        if t_min > t_max {
            return false;
        }
    }
    true
}

/// 圆-圆重叠：返回（从 a 指向 b 的法线，穿透量）。
fn circle_circle(a: Vec2, ra: f32, b: Vec2, rb: f32) -> Option<(Vec2, f32)> {
    let d = b - a;
    let r_sum = ra + rb;
    let dist_sq = d.length_squared();
    if dist_sq >= r_sum * r_sum {
        return None;
    }
    if dist_sq <= 1e-12 {
        // 圆心重合：方向退化，取确定性任意方向（-Z 侧）
        return Some((Vec2::NEG_Y, r_sum));
    }
    let dist = dist_sq.sqrt();
    Some((d / dist, r_sum - dist))
}

/// 圆-矩形（XZ 轴对齐）重叠：法线从圆指向矩形，穿透量 = 半径 − 最近点距。
fn circle_box(a: Vec2, radius: f32, half_x: f32, half_z: f32, center: Vec2) -> Option<(Vec2, f32)> {
    let local = a - center;
    let closest = Vec2::new(
        local.x.clamp(-half_x, half_x),
        local.y.clamp(-half_z, half_z),
    );
    let delta = closest - local;
    let dist_sq = delta.length_squared();
    if dist_sq > radius * radius {
        return None;
    }
    if dist_sq > 1e-12 {
        let dist = dist_sq.sqrt();
        Some((delta / dist, radius - dist))
    } else {
        // 圆心在矩形内：沿最浅穿透轴退出（并列按 x 优先，确定性）
        let dx = half_x - local.x.abs();
        let dz = half_z - local.y.abs();
        if dx <= dz {
            let away = if local.x >= 0.0 { 1.0 } else { -1.0 };
            Some((Vec2::new(away, 0.0), dx + radius))
        } else {
            let away = if local.y >= 0.0 { 1.0 } else { -1.0 };
            Some((Vec2::new(0.0, away), dz + radius))
        }
    }
}

// ---------------------------------------------------------------------------
// 单测（纯核心全规则；坐标针对 ARENA_HALF = 24 的正式关卡设计）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const ARENA: f32 = 24.0;
    const DT: f32 = 1.0 / 60.0;
    /// 常用容差：60Hz 步长下的浮点积累误差量级。
    const EPS: f32 = 1e-3;

    /// 以固定步长推进 `seconds` 秒，期间持续给定输入。
    fn run(
        state: &mut ArenaState,
        input: ArenaInput,
        seconds: f32,
        statics: &[(Vec3, StaticShape)],
    ) {
        let steps = (seconds / DT).round() as u32;
        for _ in 0..steps {
            state.step(&input, DT, statics);
        }
    }

    /// 开局到第 1 波（restart 复位 + 生成 5 只敌人）。
    fn started() -> ArenaState {
        let mut state = ArenaState::new(ARENA);
        state.step(
            &ArenaInput {
                confirm: true,
                ..ArenaInput::IDLE
            },
            DT,
            &[],
        );
        assert_eq!(state.phase, Phase::Playing);
        state
    }

    /// 布置一只测试敌人（字段公开，构造中间态用）。
    fn enemy(id: u32, pos: Vec2, hp: i32) -> EnemyState {
        EnemyState {
            id,
            pos,
            hp,
            hit_flash: 0.0,
            kb: Vec2::ZERO,
        }
    }

    #[test]
    fn title_starts_on_any_key_and_wave_one_spawns() {
        let mut state = ArenaState::new(ARENA);
        assert_eq!(state.phase, Phase::Title);
        assert_eq!(state.wave, 0);
        // 无确认键：保持标题、什么都不动（移动输入也被忽略）
        run(
            &mut state,
            ArenaInput {
                move_x: 1.0,
                ..ArenaInput::IDLE
            },
            0.5,
            &[],
        );
        assert_eq!(state.phase, Phase::Title);
        assert!(state.enemies.is_empty());
        assert_eq!(state.player.pos, Vec2::ZERO, "标题阶段玩家不动");
        // 任意确认键：开局 + 第 1 波 = 3 + 2×1 = 5 只
        state.step(
            &ArenaInput {
                confirm: true,
                ..ArenaInput::IDLE
            },
            DT,
            &[],
        );
        assert_eq!(state.phase, Phase::Playing);
        assert_eq!(state.wave, 1);
        assert_eq!(state.enemies.len(), 5, "第 1 波应为 3 + 2×1 = 5 只");
        assert_eq!(
            state.enemy_speed, ENEMY_BASE_SPEED,
            "第 1 波速度 = 基础速度"
        );
    }

    #[test]
    fn auto_fire_targets_nearest_enemy_and_skips_when_empty_or_out_of_range() {
        // 两名敌人：近敌 6 m、远敌 12 m（同方向，都在射程内）→ 只射最近
        let mut state = started();
        state.enemies.clear();
        state
            .enemies
            .push(enemy(100, Vec2::new(6.0, 0.0), ENEMY_MAX_HP));
        state
            .enemies
            .push(enemy(101, Vec2::new(12.0, 0.0), ENEMY_MAX_HP));
        state.player.fire_timer = 0.0;
        state.step(&ArenaInput::IDLE, DT, &[]);
        assert_eq!(state.total_shots, 1, "射程内有敌人应开火");
        assert_eq!(state.projectiles.len(), 1);
        let p = &state.projectiles[0];
        assert!(
            (p.vel.x - PROJECTILE_SPEED).abs() < EPS && p.vel.y.abs() < EPS,
            "应朝最近敌人（+X）开火，实际 vel {:?}",
            p.vel
        );
        assert!(
            p.pos.distance(state.player.pos) <= PROJECTILE_SPEED * DT + EPS,
            "弹道出生点 = 玩家位置（弹道当帧即飞行一步），实际 {:?} vs {:?}",
            p.pos,
            state.player.pos
        );
        assert_eq!(state.player.fire_timer, FIRE_INTERVAL, "开火间隔重置");

        // 全部敌人移出射程：不开火
        let mut state = started();
        state.enemies.clear();
        state
            .enemies
            .push(enemy(200, Vec2::new(FIRE_RANGE + 1.0, 0.0), ENEMY_MAX_HP));
        state.player.fire_timer = 0.0;
        state.step(&ArenaInput::IDLE, DT, &[]);
        assert_eq!(state.total_shots, 0, "射程外不应开火");
        assert!(state.projectiles.is_empty());

        // 无敌人：不开火
        let mut state = started();
        state.enemies.clear();
        state.player.fire_timer = 0.0;
        state.step(&ArenaInput::IDLE, DT, &[]);
        assert_eq!(state.total_shots, 0, "无敌人不应开火");
    }

    #[test]
    fn projectile_hits_enemy_once_applies_hp_knockback_then_despawns() {
        let mut state = started();
        state.enemies.clear();
        state
            .enemies
            .push(enemy(300, Vec2::new(6.0, 0.0), ENEMY_MAX_HP));
        state.player.fire_timer = 0.0;
        state.step(&ArenaInput::IDLE, DT, &[]);
        let projectile_id = state.projectiles[0].id;

        // 推进至命中（敌我相向：6 m @ 相对 24 m/s ≈ 15 帧）
        let mut hit_frame = None;
        for i in 0..60 {
            state.step(&ArenaInput::IDLE, DT, &[]);
            if state.total_hits == 1 {
                hit_frame = Some(i);
                break;
            }
        }
        let frame = hit_frame.expect("弹道应在 1 s 内命中");
        assert!(frame < 25, "6 m 相向应 ~15 帧命中，实际 {frame} 帧");
        assert!(
            !state.projectiles.iter().any(|p| p.id == projectile_id),
            "命中后弹道应消失"
        );
        let enemy_state = &state.enemies[0];
        assert_eq!(enemy_state.hp, ENEMY_MAX_HP - 1, "命中应 -1 HP");
        assert_eq!(enemy_state.hit_flash, ENEMY_HIT_FLASH, "受击闪烁应重置");
        assert!(enemy_state.kb.x > 0.0, "应沿弹道方向（+X）获得击退速度");
    }

    /// 穿模回归：弹速调到「每帧位移 > 敌人直径的 2 倍」（160 m/s × dt ≈
    /// 2.67 m > 2 × 1.1 m），且弹道采样点恰好跨过敌人（两端点距敌人都
    /// > 判定半径 0.8）——旧的当帧点判定必漏检，扫掠线段判定应命中。
    #[test]
    fn high_speed_projectile_cannot_tunnel_through_enemies() {
        let mut state = started();
        state.enemies.clear();
        state
            .enemies
            .push(enemy(1100, Vec2::new(6.0, 0.0), ENEMY_MAX_HP));
        // 关闭自动开火（长冷却）：只观察注入的高速弹道
        state.player.fire_timer = 100.0;
        state.projectiles.push(ProjectileState {
            id: 6000,
            pos: Vec2::new(-6.0, 0.0),
            vel: Vec2::new(160.0, 0.0),
            life: 1.0,
        });

        // 场景自检：步长 > 敌人直径 2 倍、> 判定直径（2 × 判定半径）
        let step_len = 160.0 * DT;
        let hit_radius = ENEMY_RADIUS + PROJECTILE_RADIUS;
        assert!(
            step_len > 2.0 * 2.0 * ENEMY_RADIUS,
            "步长 {step_len} 应 > 敌人直径的 2 倍 {}（穿模条件成立）",
            2.0 * 2.0 * ENEMY_RADIUS
        );
        assert!(
            step_len > 2.0 * hit_radius,
            "步长应 > 判定直径（存在跨过窗口）"
        );
        // 逐帧采样点自检（确定性复算）：命中前每帧的弹道位置与敌人位置
        // （敌人以波次速度 2.2 m/s 相向追击）的距离都 > 判定半径——
        // 即旧点判定在整个飞行过程中**一次都不会触发**
        for k in 0..5 {
            let p = -6.0 + 160.0 * DT * k as f32;
            let e = 6.0 - ENEMY_BASE_SPEED * DT * k as f32;
            assert!(
                (p - e).abs() > hit_radius,
                "第 {k} 帧采样点距敌人 {p} vs {e} 应 > {hit_radius}（点判定必漏的前提）"
            );
        }

        // 12 m 行程 @ 相向 ~162 m/s ≈ 4~5 帧命中
        let mut hit_frame = None;
        for i in 0..60 {
            state.step(&ArenaInput::IDLE, DT, &[]);
            if state.total_hits == 1 {
                hit_frame = Some(i);
                break;
            }
        }
        let frame = hit_frame.expect("高速弹道应在 1 s 内命中（扫掠判定防穿模）");
        assert!(frame < 10, "12 m @ 160 m/s 应 ~4 帧命中，实际 {frame} 帧");
        assert!(state.projectiles.is_empty(), "命中后弹道应消失");
        assert_eq!(state.enemies[0].hp, ENEMY_MAX_HP - 1, "敌人应被命中扣血");
        assert!(state.enemies[0].kb.x > 0.0, "击退方向沿弹道（+X）");
    }

    #[test]
    fn kills_build_combo_and_score_is_combo_multiplied() {
        let mut state = started();
        // 两只 1 HP 敌人（分处 ±X 对称位：首杀取列表序靠前的 +X 那只）
        state.enemies.clear();
        state.enemies.push(enemy(400, Vec2::new(6.0, 0.0), 1));
        state.enemies.push(enemy(401, Vec2::new(-6.0, 0.0), 1));
        state.player.fire_timer = 0.0;
        for _ in 0..60 {
            state.step(&ArenaInput::IDLE, DT, &[]);
            if state.total_kills == 1 {
                break;
            }
        }
        assert_eq!(state.total_kills, 1);
        assert_eq!(state.combo, 1, "首杀连击 = 1");
        assert_eq!(state.score, SCORE_PER_KILL, "得分 = 25 × 连击 1");
        // 击杀帧内窗口先重置、帧尾照常衰减一个 dt
        assert!(
            (state.combo_timer - (COMBO_WINDOW - DT)).abs() < EPS,
            "连击窗口应重置为 {} − 本帧 dt，实际 {}",
            COMBO_WINDOW,
            state.combo_timer
        );
        // 第二杀（-X 那只仍在追近）→ 连击 2、得分 25 + 50
        state.player.fire_timer = 0.0;
        for _ in 0..60 {
            state.step(&ArenaInput::IDLE, DT, &[]);
            if state.total_kills == 2 {
                break;
            }
        }
        assert_eq!(state.total_kills, 2);
        assert_eq!(state.combo, 2);
        assert_eq!(
            state.score,
            SCORE_PER_KILL + SCORE_PER_KILL * 2,
            "第二杀 25 × 2"
        );
    }

    #[test]
    fn combo_decays_to_zero_after_window_without_kills() {
        let mut state = started();
        state.enemies.clear();
        state.combo = 3;
        state.combo_timer = COMBO_WINDOW;
        // 推进 COMBO_WINDOW + 余量：连击应清零
        run(&mut state, ArenaInput::IDLE, COMBO_WINDOW + 0.2, &[]);
        assert_eq!(state.combo, 0, "窗口内无击杀连击应清零");
        assert_eq!(state.combo_timer, 0.0);
    }

    #[test]
    fn contact_damage_is_once_per_invulnerability_window() {
        let mut state = started();
        state.enemies.clear();
        // 高 HP 敌人钉在玩家身上（持续把玩家按回它位置，模拟持续贴脸）
        state.enemies.push(enemy(500, Vec2::new(0.5, 0.0), 9999));
        let steps = 60; // 1 s：HURT_INVULN = 0.8 s 窗口内至多 2 次（第 0 与 ~0.8 s）
        for _ in 0..steps {
            state.player.pos = Vec2::new(0.3, 0.0);
            state.step(&ArenaInput::IDLE, DT, &[]);
        }
        assert!(
            state.total_damage >= 1 && state.total_damage <= 2,
            "1 s 贴脸应掉血 1~2 次（无敌窗口生效），实际 {}",
            state.total_damage
        );
        assert_eq!(state.combo, 0, "受伤应清零连击");
        assert_eq!(state.player.hp, PLAYER_MAX_HP - state.total_damage as i32);
    }

    #[test]
    fn dash_grants_invulnerability_through_contact() {
        let mut state = started();
        state.enemies.clear();
        // 敌人挡在 +X 方向 2 m 处；玩家向 +X 冲刺（方向锁进 dash_dir）穿过它
        state.enemies.push(enemy(600, Vec2::new(2.0, 0.0), 9999));
        let hp_before = state.player.hp;
        state.step(
            &ArenaInput {
                dash: true,
                move_x: 1.0,
                ..ArenaInput::IDLE
            },
            DT,
            &[],
        );
        assert!(state.player.invuln > 0.0, "冲刺应立即进入无敌");
        assert_eq!(state.player.dash_timer, DASH_TIME, "冲刺计时开始走表");
        assert!(
            (state.player.dash_dir.x - 1.0).abs() < EPS,
            "移动中冲刺应锁定移动方向（+X）"
        );
        // 继续推进：松开输入，冲刺沿锁定的 +X 穿过敌人并滑行
        run(&mut state, ArenaInput::IDLE, 0.12, &[]);
        assert!(
            state.player.pos.x > 2.0,
            "冲刺应穿过敌人位置（x ≥ 2 m），实际 x = {}",
            state.player.pos.x
        );
        assert_eq!(state.player.hp, hp_before, "冲刺无敌穿过敌人不应掉血");
        assert_eq!(state.total_damage, 0, "无敌窗口内接触不掉血");
    }

    #[test]
    fn dash_has_cooldown_and_static_or_forward_direction() {
        let mut state = started();
        // 移动中冲刺：沿移动方向（+X）
        state.step(
            &ArenaInput {
                dash: true,
                move_x: 1.0,
                ..ArenaInput::IDLE
            },
            DT,
            &[],
        );
        assert!(state.player.dash_timer > 0.0);
        assert!(
            (state.player.dash_dir.x - 1.0).abs() < EPS,
            "移动中冲刺应沿移动方向"
        );
        // 冷却期内再按冲刺：不触发（无新无敌）
        state.player.dash_timer = 0.0;
        state.player.invuln = 0.0;
        state.step(
            &ArenaInput {
                dash: true,
                ..ArenaInput::IDLE
            },
            DT,
            &[],
        );
        assert_eq!(state.player.invuln, 0.0, "冷却期内冲刺不应触发");
        // 冷却结束后（无输入）：沿朝向前方冲刺
        state.player.dash_cd = 0.0;
        state.step(
            &ArenaInput {
                dash: true,
                ..ArenaInput::IDLE
            },
            DT,
            &[],
        );
        assert!(state.player.invuln > 0.0, "冷却结束应可再次冲刺");
        assert!(
            (state.player.dash_dir.length() - 1.0).abs() < EPS,
            "静止冲刺应沿单位朝向前方"
        );
    }

    #[test]
    fn enemy_chase_reduces_distance_monotonically() {
        let mut state = started();
        state.enemies.clear();
        state.enemies.push(enemy(700, Vec2::new(3.0, 4.0), 9999));
        // 玩家长期无敌：排除接触伤害击退对「纯追击」观察的干扰
        state.player.invuln = 1.0e6;
        // 关掉自动开火（弹道击退同样会打断「单调递减」）
        let mut previous = state.enemies[0].pos.distance(state.player.pos);
        for _ in 0..120 {
            state.player.fire_timer = 1.0;
            state.step(&ArenaInput::IDLE, DT, &[]);
            let d = state.enemies[0].pos.distance(state.player.pos);
            assert!(d < previous + 1e-6, "追击距离应单调递减：{previous} → {d}");
            previous = d;
        }
        assert!(
            previous < 1.0,
            "2 s 追击（4.4 m）后应贴近玩家，实际 {previous}"
        );
    }

    #[test]
    fn wave_clear_leads_to_breather_then_bigger_faster_wave() {
        let mut state = started();
        assert_eq!(state.enemies.len(), 5);
        // 直接清空第 1 波（测试允许跳过战斗）
        state.enemies.clear();
        state.step(&ArenaInput::IDLE, DT, &[]);
        assert_eq!(state.breather, BREATHER_TIME, "清空当帧应开始间歇");
        // 推进间歇全程：下一波自动开始
        run(&mut state, ArenaInput::IDLE, BREATHER_TIME + 0.1, &[]);
        assert_eq!(state.wave, 2, "间歇结束应进入第 2 波");
        assert_eq!(
            state.enemies.len(),
            (WAVE_BASE + WAVE_SCALE * 2) as usize,
            "第 2 波应为 7 只"
        );
        assert_eq!(state.breather, 0.0);
        let want_speed = ENEMY_BASE_SPEED + ENEMY_SPEED_PER_WAVE;
        assert!(
            (state.enemy_speed - want_speed).abs() < 1e-5,
            "第 2 波速度应 +0.15，实际 {}",
            state.enemy_speed
        );
        // 波数超过对象池容量：封顶 MAX_ENEMIES
        state.enemies.clear();
        state.wave = 20;
        state.breather = 0.01;
        state.step(&ArenaInput::IDLE, DT, &[]);
        assert_eq!(
            state.enemies.len(),
            MAX_ENEMIES,
            "波次人数应封顶敌人对象池容量"
        );
    }

    #[test]
    fn player_death_transitions_to_game_over_and_stays_frozen() {
        let mut state = started();
        state.enemies.clear();
        state.player.hp = 1;
        state.player.invuln = 0.0;
        state.player.pos = Vec2::new(10.0, 0.0);
        state.enemies.push(enemy(800, Vec2::new(10.3, 0.0), 9999));
        state.step(&ArenaInput::IDLE, DT, &[]);
        assert_eq!(state.phase, Phase::GameOver);
        assert_eq!(state.player.hp, 0);
        assert_eq!(state.total_damage, 1);
        // 终局冻结：继续输入不再推进对局
        let snapshot = (state.player.pos, state.score, state.wave);
        run(
            &mut state,
            ArenaInput {
                move_x: 1.0,
                dash: true,
                ..ArenaInput::IDLE
            },
            1.0,
            &[],
        );
        assert_eq!(
            (state.player.pos, state.score, state.wave),
            snapshot,
            "终局应冻结"
        );
        assert_eq!(state.phase, Phase::GameOver);
    }

    #[test]
    fn restart_from_game_over_resets_everything() {
        let mut state = started();
        // 制造烂摊子：掉血、得分、连击、弹道、终局
        state.enemies.clear();
        state.player.hp = 1;
        state.score = 300;
        state.combo = 4;
        state.projectiles.push(ProjectileState {
            id: 999,
            pos: Vec2::new(1.0, 1.0),
            vel: Vec2::ONE,
            life: 1.0,
        });
        state.phase = Phase::GameOver;
        state.step(
            &ArenaInput {
                restart: true,
                ..ArenaInput::IDLE
            },
            DT,
            &[],
        );
        // 与「标题任意键开局」逐字段一致：完整复位（两条路径同一 restart）
        let fresh = started();
        assert_eq!(state.phase, fresh.phase);
        assert_eq!(state.player, fresh.player);
        assert_eq!(state.enemies, fresh.enemies);
        assert!(state.projectiles.is_empty(), "弹道应清空");
        assert_eq!((state.wave, state.score, state.combo), (fresh.wave, 0, 0));
        assert_eq!(
            (
                state.total_shots,
                state.total_hits,
                state.total_kills,
                state.total_damage
            ),
            (0, 0, 0, 0),
            "事件计数应归零"
        );
        assert_eq!(state.next_id, fresh.next_id, "id 分配器应复位");
    }

    #[test]
    fn projectiles_die_on_statics_and_cannot_reach_behind_them() {
        let mut state = started();
        state.enemies.clear();
        // 一堵墙（XZ 矩形）+ 墙后的敌人
        let statics = [(
            Vec3::new(5.0, 0.0, 0.0),
            StaticShape::Box {
                half_x: 0.5,
                half_z: 3.0,
            },
        )];
        state
            .enemies
            .push(enemy(900, Vec2::new(8.0, 0.0), ENEMY_MAX_HP));
        state.player.fire_timer = 0.0;
        state.step(&ArenaInput::IDLE, DT, &statics);
        assert_eq!(state.projectiles.len(), 1);
        let mut died = false;
        for _ in 0..40 {
            state.step(&ArenaInput::IDLE, DT, &statics);
            if state.projectiles.is_empty() {
                died = true;
                break;
            }
        }
        assert!(died, "弹道撞墙应消亡");
        assert_eq!(state.total_hits, 0, "墙后的敌人不应被命中");
        assert_eq!(state.enemies[0].hp, ENEMY_MAX_HP, "墙后敌人应无伤");
        // 敌人追击被墙挡住：继续推进 1.5 s 让它顶到墙，停在墙外侧
        run(&mut state, ArenaInput::IDLE, 1.5, &statics);
        let stopped = state.enemies[0].pos.x;
        assert!(
            (stopped - 6.05).abs() < 0.05,
            "敌人应被墙挡在 6.05 = 内缘 5.5 + 半径 0.55，实际 {stopped}"
        );
    }

    #[test]
    fn wall_blocks_player_with_tangential_slide() {
        let mut state = started();
        // +X 侧墙：内缘 x = 23.5（墙心 24、半边 0.5）
        let statics = [(
            Vec3::new(24.0, 0.0, 0.0),
            StaticShape::Box {
                half_x: 0.5,
                half_z: ARENA,
            },
        )];
        state.player.pos = Vec2::new(23.0, 0.0);
        // 斜向推 1 s：法向被挡在 23.05 = 内缘 23.5 − 半径 0.45，
        // 切向持续滑行（推离只沿法线 = 贴墙滑行）
        run(
            &mut state,
            ArenaInput {
                move_x: 1.0,
                move_z: 1.0,
                ..ArenaInput::IDLE
            },
            1.0,
            &statics,
        );
        let want_x = 23.5 - PLAYER_RADIUS;
        assert!(
            (state.player.pos.x - want_x).abs() <= 0.02,
            "法向应被墙挡在 {want_x}，实际 {}",
            state.player.pos.x
        );
        assert!(
            state.player.pos.y > 1.5,
            "切向应持续滑行（1 s 约 4.2 m），实际 z = {}",
            state.player.pos.y
        );
    }

    #[test]
    fn projectile_pool_is_capped_at_max() {
        // 池满即不开火：塞满 40 条弹道 + 射程内敌人 → 不新增
        let mut state = started();
        state.enemies.clear();
        state.enemies.push(enemy(1000, Vec2::new(5.0, 0.0), 9999));
        for i in 0..MAX_PROJECTILES {
            state.projectiles.push(ProjectileState {
                id: 5000 + i as u32,
                pos: Vec2::new(i as f32 * 0.1, 20.0), // 摆在场内远处，不干扰
                vel: Vec2::NEG_Y * 0.1,
                life: 30.0,
            });
        }
        state.player.fire_timer = 0.0;
        state.step(&ArenaInput::IDLE, DT, &[]);
        assert_eq!(state.total_shots, 0, "弹道池满时不应开火");
        assert_eq!(state.projectiles.len(), MAX_PROJECTILES, "池不应超发");

        // 混战不变量：站桩让敌人快速进入射程 → 持续开火 + 击杀 + 换波，
        // 8 s 内弹道数每帧 ≤ 上限
        let mut state = started();
        for _ in 0..480 {
            state.step(&ArenaInput::IDLE, DT, &[]);
            assert!(
                state.projectiles.len() <= MAX_PROJECTILES,
                "弹道数应封顶 {MAX_PROJECTILES}，实际 {}",
                state.projectiles.len()
            );
            assert!(state.enemies.len() <= MAX_ENEMIES, "敌人数不应超过池容量");
        }
        assert!(state.total_shots > 10, "混战脚本应持续开火");
    }

    #[test]
    fn enemy_ids_are_monotonic_and_never_reused_across_waves() {
        let mut state = started();
        let wave1_ids: Vec<u32> = state.enemies.iter().map(|e| e.id).collect();
        assert_eq!(wave1_ids.len(), 5);
        for pair in wave1_ids.windows(2) {
            assert!(pair[1] > pair[0], "id 应单调分配且不重复");
        }
        // 清空 → 第 2 波：id 继续单调，不复用
        state.enemies.clear();
        state.breather = 0.01;
        state.step(&ArenaInput::IDLE, DT, &[]);
        let wave1_max = *wave1_ids.iter().max().unwrap();
        for e in &state.enemies {
            assert!(
                e.id > wave1_max,
                "第 2 波 id 应大于第 1 波全部 id（不复用）"
            );
        }
    }

    #[test]
    fn same_input_two_runs_are_sample_identical() {
        // 混合场景：两面墙 + 圆柱 + 方柱 + 完整脚本（移动 / 冲刺 / 挨打 /
        // 击杀 / 过波）两遍推进，逐帧采样全部关键字段比对。
        let statics = [
            (
                Vec3::new(24.0, 0.0, 0.0),
                StaticShape::Box {
                    half_x: 0.5,
                    half_z: ARENA,
                },
            ),
            (
                Vec3::new(0.0, 0.0, -24.0),
                StaticShape::Box {
                    half_x: ARENA,
                    half_z: 0.5,
                },
            ),
            (
                Vec3::new(6.0, 0.0, 6.0),
                StaticShape::Sphere { radius: 0.7 },
            ),
            (
                Vec3::new(-8.0, 0.0, 4.0),
                StaticShape::Box {
                    half_x: 0.7,
                    half_z: 0.7,
                },
            ),
        ];
        fn trace(statics: &[(Vec3, StaticShape)]) -> Vec<(Vec2, f32, u32, u32, u32, usize, usize)> {
            let mut state = ArenaState::new(ARENA);
            state.step(
                &ArenaInput {
                    confirm: true,
                    ..ArenaInput::IDLE
                },
                DT,
                statics,
            );
            // 玩家挪到出生环附近：几秒内必然接敌（脚本不是空转）
            state.player.pos = Vec2::new(16.0, 6.0);
            let script: [ArenaInput; 8] = [
                ArenaInput {
                    move_x: 1.0,
                    move_z: 0.0,
                    ..ArenaInput::IDLE
                },
                ArenaInput {
                    move_x: 0.4,
                    move_z: -1.0,
                    dash: true,
                    ..ArenaInput::IDLE
                },
                ArenaInput::IDLE,
                ArenaInput {
                    move_x: -1.0,
                    move_z: 0.3,
                    ..ArenaInput::IDLE
                },
                ArenaInput {
                    move_x: 0.0,
                    move_z: 1.0,
                    dash: true,
                    ..ArenaInput::IDLE
                },
                ArenaInput::IDLE,
                ArenaInput {
                    move_x: 0.7,
                    move_z: -0.7,
                    ..ArenaInput::IDLE
                },
                ArenaInput::IDLE,
            ];
            let mut samples = Vec::new();
            for i in 0..600 {
                state.step(&script[i % 8], DT, statics);
                samples.push((
                    state.player.pos,
                    state.player.hp as f32,
                    state.score,
                    state.combo,
                    state.wave,
                    state.enemies.len(),
                    state.projectiles.len(),
                ));
                for e in state.enemies.iter().take(3) {
                    samples.push((e.pos, e.hp as f32, e.id, 0, 0, 0, 0));
                }
            }
            samples
        }
        let trace_a = trace(&statics);
        let trace_b = trace(&statics);
        assert_eq!(trace_a.len(), trace_b.len());
        for (i, (sa, sb)) in trace_a.iter().zip(&trace_b).enumerate() {
            assert_eq!(sa, sb, "同输入两遍应逐采样一致（bit 级），样本 #{i}");
        }
        // 采样非空转的佐证：期间存在在飞弹道（敌人进入了射程并持续开火）、
        // 玩家离开了出发点（接触 / 移动确实发生了）；接触与击杀的语义已由
        // 上方专项测试覆盖
        assert!(trace_a.iter().any(|s| s.6 > 0), "脚本期间应有弹道在场");
        let moved = trace_a
            .iter()
            .map(|s| s.0.distance(Vec2::new(16.0, 6.0)))
            .fold(0.0f32, f32::max);
        assert!(
            moved > 0.5,
            "10 s 脚本后玩家应曾离开出发点，最大位移 {moved}"
        );
    }
}
