//! 纯游戏逻辑核心：确定性塔防状态机，**只依赖 glam、零引擎依赖**。
//!
//! 「Iris Bastion」的全部规则（放塔校验 / 经济 / 索敌开火 / 弹道与溅射 /
//! 航点推进 / 漏怪扣血 / 波次推进 / 胜负）都在这个模块里以纯函数推进：
//! 输入 [`BastionInput`]（放塔请求 + `dt`），输出状态变化与 [`BastionEvent`]
//! 列表——不读时钟、不做随机、不触碰引擎。因此：
//! - 全部规则可毫秒级单测（本模块底部 12 项）；
//! - 插件层（`crate::plugin`）只负责把鼠标 / 键盘输入翻译成本模块类型，
//!   再把结果写回实体组件与对象池；
//! - headless 测试与离屏快照共享同一条确定性推进路径。
//!
//! # 与引擎查询的关系（近似性说明）
//!
//! 哨兵的快速弹沿用 iris-arena 的**本地扫掠命中判定**
//! （`segment_hits_circle`：弹道本帧 prev → pos 线段对目标的扫掠测试，
//! 与引擎 `segment_hits_shape` 同款防穿模语义）——刻意解耦：纯核心不依赖
//! 引擎类型。引擎侧的 `raycast`（R21）在**插件层**承担另一件真实工作：
//! 鼠标屏幕射线打地面静态碰撞体完成网格拾取（见 `plugin` 模块文档）。
//! 臼炮的溅射是「落点半径内全体受创」的纯几何判定（圆-圆距离测试），
//! 无需物理参与。
//!
//! # 坐标约定
//!
//! 世界 XZ 平面为地面（y 向上，本模块只用 x/z；[`Vec2`] 的 y 分量即世界
//! z）。场地为 x ∈ [−10, 10]、z ∈ [−7, 7] 的矩形，网格 1×1 m
//! （[`GRID_W`] × [`GRID_H`] = 20 × 14 格），格 `(i, j)` 的世界中心由
//! [`cell_center`] 给出。敌人沿航点折线（[`WAYPOINTS`]，7 段 6 弯）匀速
//! 推进，核心在路径末端（[`CORE_POS`]）。
//!
//! # 索敌策略（选型说明）
//!
//! 塔在射程内选择**路径进度最大**的敌人开火（塔防的经典「打最靠前」
//! 策略：火力优先砸向最接近核心的威胁）。进度并列时取敌人列表序靠前者
//! （确定性）。
//!
//! # 帧内顺序（固定，是确定性的一部分）
//!
//! 阶段冻结 → 放塔 → 出怪 → 塔冷却与开火 → 哨兵弹推进与命中 → 臼炮弹
//! 推进与溅射 → 击杀结算 → 敌人推进与漏怪 → 波次推进。

use glam::{IVec2, Vec2};

// ---------------------------------------------------------------------------
// 全部可调常量（pub：单测 / 插件层 / 文档共用同一事实来源）
// ---------------------------------------------------------------------------

/// 场地格数（X 方向）：场地宽 20 m。
pub const GRID_W: i32 = 20;
/// 场地格数（Z 方向）：场地深 14 m。
pub const GRID_H: i32 = 14;
/// 场地最小角（世界坐标，XZ）：格 (0, 0) 的西北角。
pub const FIELD_MIN: Vec2 = Vec2::new(-10.0, -7.0);
/// 航点折线（XZ 平面，米）：7 段 6 弯，首点为出生口、末点为核心。
pub const WAYPOINTS: [Vec2; 8] = [
    Vec2::new(-10.0, -5.0), // 出生口（场地西缘）
    Vec2::new(-5.0, -5.0),  // 弯 1
    Vec2::new(-5.0, 2.0),   // 弯 2
    Vec2::new(0.0, 2.0),    // 弯 3
    Vec2::new(0.0, -4.0),   // 弯 4
    Vec2::new(5.0, -4.0),   // 弯 5
    Vec2::new(5.0, 3.0),    // 弯 6
    Vec2::new(8.0, 3.0),    // 核心
];
/// 核心位置（路径末端）。
pub const CORE_POS: Vec2 = Vec2::new(8.0, 3.0);
/// 核心最大生命（点）：每个敌人抵达核心 −1，归零即败。
pub const CORE_MAX_HP: i32 = 10;
/// 初始积分：恰好够两座哨兵（经济起手式）。
pub const START_CREDITS: u32 = 100;
/// 总波数：守到第 5 波全清获胜。
pub const MAX_WAVES: u32 = 5;
/// 波次间歇（秒）：上一波全灭后到下一波开怪的喘息时间。
pub const WAVE_BREATHER: f32 = 5.0;
/// 同波内相邻两怪的出怪间隔（秒）。
pub const SPAWN_INTERVAL: f32 = 0.8;
/// 在场敌人上限（逻辑封顶 + 插件层敌人池同规格）。
pub const MAX_ENEMIES: usize = 24;
/// 塔数上限（逻辑封顶 + 插件层塔池同规格）。
pub const MAX_TOWERS: usize = 40;
/// 哨兵弹在场上限（逻辑封顶 + 插件层弹池同规格）。
pub const MAX_BULLETS: usize = 32;
/// 臼炮弹在场上限（逻辑封顶 + 插件层弹池同规格）。
pub const MAX_SHELLS: usize = 8;

/// 疾行虫（快脆）：生命 / 速度（米/秒）/ 击杀赏金 / 碰撞半径（米）。
pub const RUNNER_HP: i32 = 2;
pub const RUNNER_SPEED: f32 = 2.4;
pub const RUNNER_BOUNTY: u32 = 12;
pub const RUNNER_RADIUS: f32 = 0.35;
/// 重甲兽（慢坦）：同上四项。
pub const BRUTE_HP: i32 = 9;
pub const BRUTE_SPEED: f32 = 1.3;
pub const BRUTE_BOUNTY: u32 = 30;
pub const BRUTE_RADIUS: f32 = 0.5;

/// 哨兵（速射单体）：造价 / 射程（米）/ 冷却（秒）/ 单发伤害。
pub const SENTRY_COST: u32 = 50;
pub const SENTRY_RANGE: f32 = 3.2;
pub const SENTRY_COOLDOWN: f32 = 0.5;
pub const SENTRY_DAMAGE: i32 = 1;
/// 哨兵弹速（米/秒）与寿命（秒）：扫掠判定防高速穿模（见模块文档）。
pub const BULLET_SPEED: f32 = 14.0;
pub const BULLET_LIFE: f32 = 1.0;
/// 哨兵弹命中半径（米）：与敌人半径求和判交。
pub const BULLET_RADIUS: f32 = 0.2;
/// 臼炮（溅射慢速）：造价 / 射程 / 冷却 / 单发伤害 / 溅射半径（米）。
pub const MORTAR_COST: u32 = 90;
pub const MORTAR_RANGE: f32 = 4.5;
pub const MORTAR_COOLDOWN: f32 = 1.8;
pub const MORTAR_DAMAGE: i32 = 3;
pub const AOE_RADIUS: f32 = 1.4;
/// 臼炮弹速（米/秒）：落点 = 开火时目标当前位置的快照（无预判，溅射
/// 半径兜住目标移动量）。
pub const SHELL_SPEED: f32 = 8.0;

// ---------------------------------------------------------------------------
// 敌人 / 塔种类
// ---------------------------------------------------------------------------

/// 敌人种类：疾行虫（快脆）/ 重甲兽（慢坦）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnemyKind {
    /// 疾行虫：速度快、血薄、赏金低。
    Runner,
    /// 重甲兽：速度慢、血厚、赏金高。
    Brute,
}

impl EnemyKind {
    /// 最大生命（点）。
    pub fn max_hp(self) -> i32 {
        match self {
            EnemyKind::Runner => RUNNER_HP,
            EnemyKind::Brute => BRUTE_HP,
        }
    }

    /// 移动速度（米/秒，沿航点匀速）。
    pub fn speed(self) -> f32 {
        match self {
            EnemyKind::Runner => RUNNER_SPEED,
            EnemyKind::Brute => BRUTE_SPEED,
        }
    }

    /// 击杀赏金（积分）。
    pub fn bounty(self) -> u32 {
        match self {
            EnemyKind::Runner => RUNNER_BOUNTY,
            EnemyKind::Brute => BRUTE_BOUNTY,
        }
    }

    /// 碰撞半径（米，弹道命中判定用）。
    pub fn radius(self) -> f32 {
        match self {
            EnemyKind::Runner => RUNNER_RADIUS,
            EnemyKind::Brute => BRUTE_RADIUS,
        }
    }
}

/// 塔种类：哨兵（速射单体）/ 臼炮（溅射慢速）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TowerKind {
    /// 哨兵：快速弹单体射击，射程短、冷却短。
    Sentry,
    /// 臼炮：慢速弹抛射到目标当前位置，落点 AOE。
    Mortar,
}

impl TowerKind {
    /// 造价（积分）。
    pub fn cost(self) -> u32 {
        match self {
            TowerKind::Sentry => SENTRY_COST,
            TowerKind::Mortar => MORTAR_COST,
        }
    }

    /// 射程（米，塔心到敌人的 XZ 平面距离）。
    pub fn range(self) -> f32 {
        match self {
            TowerKind::Sentry => SENTRY_RANGE,
            TowerKind::Mortar => MORTAR_RANGE,
        }
    }

    /// 开火冷却（秒）。
    pub fn cooldown(self) -> f32 {
        match self {
            TowerKind::Sentry => SENTRY_COOLDOWN,
            TowerKind::Mortar => MORTAR_COOLDOWN,
        }
    }
}

// ---------------------------------------------------------------------------
// 网格坐标换算
// ---------------------------------------------------------------------------

/// 格 (i, j) 的世界中心（XZ）。
pub fn cell_center(cell: IVec2) -> Vec2 {
    Vec2::new(
        FIELD_MIN.x + cell.x as f32 + 0.5,
        FIELD_MIN.y + cell.y as f32 + 0.5,
    )
}

/// 世界坐标（XZ）→ 所在格（越界返回界外格坐标，合法性由
/// [`BastionState::check_placement`] 裁决）。
pub fn world_to_cell(pos: Vec2) -> IVec2 {
    IVec2::new(
        (pos.x - FIELD_MIN.x).floor() as i32,
        (pos.y - FIELD_MIN.y).floor() as i32,
    )
}

/// 格是否在场地内。
pub fn cell_in_field(cell: IVec2) -> bool {
    cell.x >= 0 && cell.x < GRID_W && cell.y >= 0 && cell.y < GRID_H
}

// ---------------------------------------------------------------------------
// 航点几何
// ---------------------------------------------------------------------------

/// 航点路径总长（米）：7 段折线的长度和。逐段累加，确定性。
pub fn path_length() -> f32 {
    let mut total = 0.0;
    for pair in WAYPOINTS.windows(2) {
        total += pair[1].distance(pair[0]);
    }
    total
}

/// 路径进度（沿路径行走米数）→ 世界坐标（XZ）。进度钳制在 [0, 总长]；
/// 逐段线性插值（匀速推进的几何含义）。
pub fn path_point(progress: f32) -> Vec2 {
    let mut remaining = progress.clamp(0.0, path_length());
    for pair in WAYPOINTS.windows(2) {
        let seg_len = pair[1].distance(pair[0]);
        if remaining <= seg_len {
            let t = if seg_len <= 1e-9 {
                0.0
            } else {
                remaining / seg_len
            };
            return pair[0].lerp(pair[1], t);
        }
        remaining -= seg_len;
    }
    *WAYPOINTS.last().expect("航点表非空")
}

/// 路径进度处的行进方向（单位向量，XZ）：取所在线段的方向（敌人朝向
/// 表现用；进度越界时取最后一段方向，端点行为可预期）。
pub fn path_dir(progress: f32) -> Vec2 {
    let mut remaining = progress.clamp(0.0, path_length());
    for pair in WAYPOINTS.windows(2) {
        let seg_len = pair[1].distance(pair[0]);
        let dir = (pair[1] - pair[0]).normalize_or_zero();
        if remaining <= seg_len {
            return dir;
        }
        remaining -= seg_len;
    }
    let last = WAYPOINTS.len();
    (WAYPOINTS[last - 1] - WAYPOINTS[last - 2]).normalize_or_zero()
}

// ---------------------------------------------------------------------------
// 输入 / 事件
// ---------------------------------------------------------------------------

/// 一帧的游戏输入：放塔请求 + 步长。纯核心不碰鼠标——插件层把「光标
/// 所在格 + 左键边沿」翻译成请求（同帧多请求按序结算，都过校验都生效）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BastionInput {
    /// 本帧的放塔请求（格坐标 + 塔种）。
    pub place_requests: Vec<(IVec2, TowerKind)>,
    /// 步长（秒）：纯核心不读时钟，dt 由调用方随输入给出。
    pub dt: f32,
}

/// 一帧内产生的游戏事件（含世界坐标）：插件层据此触发空间音频与粒子。
/// 每帧 `step` 开头清空——`step` 返回后 [`BastionState::frame_events`]
/// 恰好持有本帧全部事件（顺序 = 结算顺序，确定性）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BastionEvent {
    /// 哨兵开火（位置 = 塔心）。
    SentryShot { pos: Vec2 },
    /// 臼炮开火（位置 = 塔心）。
    MortarShot { pos: Vec2 },
    /// 哨兵弹命中（位置 = 受击敌人）。
    Hit { pos: Vec2 },
    /// 臼炮弹落点爆炸（位置 = 落点）。
    Boom { pos: Vec2 },
    /// 敌人被击杀（位置 = 死亡点）。
    Death { pos: Vec2 },
    /// 敌人抵达核心（位置 = 核心位置）。
    Leak { pos: Vec2 },
}

// ---------------------------------------------------------------------------
// 状态
// ---------------------------------------------------------------------------

/// 放塔校验结果：三分支 + 成功（校验纯函数单独导出，插件层与单测共用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlacementError {
    /// 目标格在场地外。
    OutOfBounds,
    /// 目标格已有塔。
    Occupied,
    /// 积分不足。
    InsufficientCredits,
}

/// 塔状态：所在格 + 种类 + 冷却剩余（秒）。位置由格坐标派生
/// （[`cell_center`]）——放塔后不可移动，格坐标即稳定身份（插件层塔池
/// 以列表下标映射槽位：塔从不移除，下标天然稳定）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TowerState {
    pub cell: IVec2,
    pub kind: TowerKind,
    /// 开火冷却剩余（秒）。
    pub cooldown: f32,
}

/// 敌人状态：稳定 id + 种类 + 路径进度 + 生命。
///
/// 位置不直接存储——由进度经 [`path_point`] 派生（保证严格贴线、进度
/// 即索敌权重的天然来源）。`id` 由 [`BastionState`] 的 `next_id` 单调
/// 分配，敌人死亡移除后下标会变，插件层的敌人池以 id（而非下标）做
/// 槽位映射。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnemyState {
    /// 稳定 id：同一次对局内唯一、从不复用。
    pub id: u32,
    pub kind: EnemyKind,
    /// 路径进度（沿路径行走米数）。
    pub progress: f32,
    pub hp: i32,
}

impl EnemyState {
    /// 当前世界坐标（XZ，由进度派生）。
    pub fn pos(&self) -> Vec2 {
        path_point(self.progress)
    }
}

/// 哨兵弹：稳定 id + XZ 位置 / 速度 + 剩余寿命（秒）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BulletState {
    pub id: u32,
    pub pos: Vec2,
    pub vel: Vec2,
    pub life: f32,
}

/// 臼炮弹：稳定 id + 当前 XZ 位置 + 落点（开火时目标位置的快照）+
/// 剩余引信 / 总飞行时间（表现层按比例算抛物线高度用）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShellState {
    pub id: u32,
    pub pos: Vec2,
    /// 落点（世界 XZ）：引信归零时以此为中心溅射。
    pub target: Vec2,
    /// 剩余引信（秒）：归零即在 `target` 爆炸。
    pub fuse: f32,
    /// 总飞行时间（秒，`fuse` 的初值）。
    pub flight_total: f32,
}

/// 游戏阶段：对局进行中 / 胜利 / 失败（后两者为终局冻结，塔防无重开——
/// 重开 = 重建 [`BastionState`]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// 对局进行中（塔防开局即进行，无标题画面）。
    Playing,
    /// 第 5 波全清：胜利。
    Victory,
    /// 核心生命归零：失败。
    Defeat,
}

/// 游戏全局状态：阶段 + 经济 + 塔 / 敌 / 弹列表 + 波次 + 事件列表。
///
/// 字段全部公开：测试可以直接构造 / 修改中间状态，插件层按只读约定
/// 消费（每帧由 [`BastionState::step`] 独占推进）。
#[derive(Clone, Debug)]
pub struct BastionState {
    pub phase: Phase,
    /// 核心剩余生命（点）。
    pub core_hp: i32,
    /// 当前积分（放塔扣款、击杀入账）。
    pub credits: u32,
    /// 当前波次（从 1 起）。
    pub wave: u32,
    /// 已放下的塔（按放置序；下标即塔池槽位）。
    pub towers: Vec<TowerState>,
    /// 在场敌人（死亡 / 漏怪即移除；`retain` 保序——确定性遍历序的一部分）。
    pub enemies: Vec<EnemyState>,
    /// 在场哨兵弹。
    pub bullets: Vec<BulletState>,
    /// 在场臼炮弹。
    pub shells: Vec<ShellState>,
    /// 本波尚未出场的敌人队列（按出怪顺序）。
    pub spawn_queue: Vec<EnemyKind>,
    /// 距下次出怪的秒数（≤ 0 = 立即出下一个）。
    pub spawn_timer: f32,
    /// 波次间歇剩余（秒）：> 0 表示上一波已清空、正在倒计时下一波。
    pub breather: f32,
    /// 下一个敌人 / 弹道 id（单调递增，从不复用）。
    pub next_id: u32,
    /// 累计统计（HUD / 调试用；表现层的事件触发走 [`Self::frame_events`]，
    /// 自带世界坐标）。
    pub total_kills: u32,
    pub total_leaks: u32,
    pub total_shots: u32,
    /// 本帧事件列表（每帧 `step` 开头清空，见 [`BastionEvent`]）。
    pub frame_events: Vec<BastionEvent>,
}

impl Default for BastionState {
    fn default() -> Self {
        Self::new()
    }
}

impl BastionState {
    /// 新状态：第 1 波即将开怪（出怪计时为 0 → 首步即出第一只）。
    pub fn new() -> Self {
        Self {
            phase: Phase::Playing,
            core_hp: CORE_MAX_HP,
            credits: START_CREDITS,
            wave: 1,
            towers: Vec::new(),
            enemies: Vec::new(),
            bullets: Vec::new(),
            shells: Vec::new(),
            spawn_queue: wave_composition(1),
            spawn_timer: 0.0,
            breather: 0.0,
            next_id: 0,
            total_kills: 0,
            total_leaks: 0,
            total_shots: 0,
            frame_events: Vec::new(),
        }
    }

    /// 放塔校验（三分支 + 成功）：目标格在场地内、无塔、积分足够——
    /// 三者全满足才成功。纯函数单独导出：插件层（幽灵预览红蓝着色）与
    /// 单测共用同一事实来源。
    pub fn check_placement(&self, cell: IVec2, kind: TowerKind) -> Result<(), PlacementError> {
        if !cell_in_field(cell) {
            return Err(PlacementError::OutOfBounds);
        }
        if self.towers.iter().any(|tower| tower.cell == cell) {
            return Err(PlacementError::Occupied);
        }
        if self.credits < kind.cost() {
            return Err(PlacementError::InsufficientCredits);
        }
        Ok(())
    }

    /// 推进一帧。终局阶段冻结（什么都不做）；`dt <= 0` 时冻结对局。
    /// 帧内顺序见模块文档（是确定性的一部分）。
    pub fn step(&mut self, input: &BastionInput) {
        self.frame_events.clear();
        match self.phase {
            Phase::Victory | Phase::Defeat => return,
            Phase::Playing => {}
        }
        let dt = input.dt;
        if dt <= 0.0 {
            return;
        }

        // ---- 放塔：逐请求过校验，通过才扣款落塔（同帧多请求按序结算，
        // 先落的塔会被后续请求的同格校验看到）----
        for &(cell, kind) in &input.place_requests {
            if self.check_placement(cell, kind).is_ok() {
                self.credits -= kind.cost();
                self.towers.push(TowerState {
                    cell,
                    kind,
                    cooldown: 0.0,
                });
            }
        }

        // ---- 出怪：计时归零即从队首放出一只（间隔见 SPAWN_INTERVAL；
        // while 循环兜底极大 dt 的极端情况）----
        while !self.spawn_queue.is_empty() && self.spawn_timer <= 0.0 {
            let kind = self.spawn_queue.remove(0);
            let id = self.next_id;
            self.next_id += 1;
            self.enemies.push(EnemyState {
                id,
                kind,
                progress: 0.0,
                hp: kind.max_hp(),
            });
            self.spawn_timer += SPAWN_INTERVAL;
        }
        self.spawn_timer -= dt;

        // ---- 塔开火（两段式避免借用交叠）：先收集开火意图（冷却推进 +
        // 索敌），再统一出弹——弹池满时该塔本帧不开火、冷却不重置 ----
        let mut intents: Vec<(usize, Vec2, TowerKind)> = Vec::new();
        for (index, tower) in self.towers.iter_mut().enumerate() {
            tower.cooldown -= dt;
            if tower.cooldown > 0.0 {
                continue;
            }
            let origin = cell_center(tower.cell);
            if furthest_enemy_in_range(&self.enemies, origin, tower.kind.range()).is_some() {
                intents.push((index, origin, tower.kind));
            }
        }
        for (index, origin, kind) in intents {
            let mut fired = false;
            match kind {
                TowerKind::Sentry => {
                    if self.bullets.len() < MAX_BULLETS {
                        if let Some(target) =
                            furthest_enemy_in_range(&self.enemies, origin, kind.range())
                        {
                            let dir = (target.pos() - origin).normalize_or_zero();
                            let id = self.next_id;
                            self.next_id += 1;
                            self.bullets.push(BulletState {
                                id,
                                pos: origin,
                                vel: dir * BULLET_SPEED,
                                life: BULLET_LIFE,
                            });
                            self.frame_events
                                .push(BastionEvent::SentryShot { pos: origin });
                            fired = true;
                        }
                    }
                }
                TowerKind::Mortar => {
                    if self.shells.len() < MAX_SHELLS {
                        if let Some(target) =
                            furthest_enemy_in_range(&self.enemies, origin, kind.range())
                        {
                            let target_pos = target.pos();
                            let flight = origin.distance(target_pos) / SHELL_SPEED;
                            let id = self.next_id;
                            self.next_id += 1;
                            self.shells.push(ShellState {
                                id,
                                pos: origin,
                                target: target_pos,
                                fuse: flight,
                                flight_total: flight,
                            });
                            self.frame_events
                                .push(BastionEvent::MortarShot { pos: origin });
                            fired = true;
                        }
                    }
                }
            }
            if fired {
                self.towers[index].cooldown = kind.cooldown();
                self.total_shots += 1;
            }
        }

        // ---- 哨兵弹推进：寿命 → 扫掠命中（一弹至多一敌，列表序取首个）----
        // 弹速 × dt 的步长可大于敌人直径，当帧点判定会穿模漏检——本帧
        // prev → pos 线段对敌人做扫掠测试（与引擎 segment_hits_shape
        // 同款语义，本地实现保持纯核心零引擎依赖）
        let mut dead_bullets: Vec<u32> = Vec::new();
        for bullet in &mut self.bullets {
            let prev = bullet.pos;
            bullet.pos += bullet.vel * dt;
            bullet.life -= dt;
            if bullet.life <= 0.0 {
                dead_bullets.push(bullet.id);
                continue;
            }
            for enemy in &mut self.enemies {
                let hit_radius = enemy.kind.radius() + BULLET_RADIUS;
                if segment_hits_circle(prev, bullet.pos, enemy.pos(), hit_radius) {
                    enemy.hp -= SENTRY_DAMAGE;
                    self.frame_events
                        .push(BastionEvent::Hit { pos: enemy.pos() });
                    dead_bullets.push(bullet.id);
                    break;
                }
            }
        }
        self.bullets
            .retain(|bullet| !dead_bullets.contains(&bullet.id));

        // ---- 臼炮弹推进：引信归零 → 落点 AOE（半径内全体受创）----
        let mut booms: Vec<Vec2> = Vec::new();
        self.shells.retain_mut(|shell| {
            shell.pos += (shell.target - shell.pos).normalize_or_zero() * SHELL_SPEED * dt;
            shell.fuse -= dt;
            if shell.fuse > 0.0 {
                return true;
            }
            booms.push(shell.target);
            false
        });
        for &boom in &booms {
            self.frame_events.push(BastionEvent::Boom { pos: boom });
            for enemy in &mut self.enemies {
                if enemy.pos().distance(boom) <= AOE_RADIUS {
                    enemy.hp -= MORTAR_DAMAGE;
                }
            }
        }

        // ---- 击杀结算：hp 归零 → 赏金入账 + 事件 + 移除（保序）----
        let mut survivors: Vec<EnemyState> = Vec::with_capacity(self.enemies.len());
        for enemy in self.enemies.drain(..) {
            if enemy.hp > 0 {
                survivors.push(enemy);
            } else {
                self.credits += enemy.kind.bounty();
                self.total_kills += 1;
                self.frame_events
                    .push(BastionEvent::Death { pos: enemy.pos() });
            }
        }
        self.enemies = survivors;

        // ---- 敌人推进：沿路径匀速；走完全程 → 漏怪（核心 −1、移除）----
        let total = path_length();
        self.enemies.retain_mut(|enemy| {
            enemy.progress += enemy.kind.speed() * dt;
            if enemy.progress < total {
                return true;
            }
            self.core_hp -= 1;
            self.total_leaks += 1;
            self.frame_events.push(BastionEvent::Leak { pos: CORE_POS });
            false
        });
        if self.core_hp <= 0 {
            self.core_hp = 0;
            self.phase = Phase::Defeat;
            return;
        }

        // ---- 波次推进：清空（队列空 + 场上空）→ 间歇倒计时 → 下一波；
        // 末波清完 → 胜利 ----
        if self.spawn_queue.is_empty() && self.enemies.is_empty() {
            if self.wave >= MAX_WAVES {
                self.phase = Phase::Victory;
                return;
            }
            if self.breather > 0.0 {
                self.breather -= dt;
                if self.breather <= 0.0 {
                    self.breather = 0.0;
                    self.wave += 1;
                    self.spawn_queue = wave_composition(self.wave);
                    self.spawn_timer = 0.0;
                }
            } else {
                self.breather = WAVE_BREATHER;
            }
        }
    }
}

/// 波次构成（第 n 波的出怪序列，写死、确定性）：数量与构成逐波递增，
/// 疾行虫铺量、重甲兽压轴。
pub fn wave_composition(wave: u32) -> Vec<EnemyKind> {
    fn fill(queue: &mut Vec<EnemyKind>, kind: EnemyKind, count: u32) {
        for _ in 0..count {
            queue.push(kind);
        }
    }
    let mut queue = Vec::new();
    match wave {
        1 => fill(&mut queue, EnemyKind::Runner, 6),
        2 => fill(&mut queue, EnemyKind::Runner, 8),
        3 => {
            fill(&mut queue, EnemyKind::Runner, 3);
            fill(&mut queue, EnemyKind::Brute, 1);
            fill(&mut queue, EnemyKind::Runner, 4);
            fill(&mut queue, EnemyKind::Brute, 1);
        }
        4 => {
            fill(&mut queue, EnemyKind::Runner, 4);
            fill(&mut queue, EnemyKind::Brute, 2);
            fill(&mut queue, EnemyKind::Runner, 4);
            fill(&mut queue, EnemyKind::Brute, 2);
        }
        _ => {
            fill(&mut queue, EnemyKind::Runner, 6);
            fill(&mut queue, EnemyKind::Brute, 3);
            fill(&mut queue, EnemyKind::Runner, 6);
            fill(&mut queue, EnemyKind::Brute, 3);
        }
    }
    queue
}

/// 索敌：射程内**路径进度最大**的敌人（并列取列表序靠前者，确定性；
/// 见模块文档「索敌策略」）。
fn furthest_enemy_in_range(
    enemies: &[EnemyState],
    origin: Vec2,
    range: f32,
) -> Option<&EnemyState> {
    let mut best: Option<(f32, &EnemyState)> = None;
    for enemy in enemies {
        let d = enemy.pos().distance(origin);
        if d <= range {
            let better = match best {
                None => true,
                Some((best_progress, _)) => enemy.progress > best_progress,
            };
            if better {
                best = Some((enemy.progress, enemy));
            }
        }
    }
    best.map(|(_, enemy)| enemy)
}

/// 线段-圆判交（XZ 平面）：线段 `from → to` 上到 `center` 的最近点距
/// ≤ `radius` 即相交（参数钳到 [0,1] 的最近点，纯几何无迭代）。
/// 与 iris-arena / 引擎 `segment_hits_shape` 同款扫掠语义的本地实现。
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

// ---------------------------------------------------------------------------
// 单测（纯核心全规则；坐标针对 20×14 正式关卡设计）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 60.0;
    /// 常用容差：60Hz 步长下的浮点积累误差量级。
    const EPS: f32 = 1e-3;

    /// 以固定步长推进 `seconds` 秒。
    fn run(state: &mut BastionState, seconds: f32) {
        let steps = (seconds / DT).round() as u32;
        for _ in 0..steps {
            state.step(&BastionInput {
                dt: DT,
                ..BastionInput::default()
            });
        }
    }

    /// 推进若干步，返回期间累计的事件（逐帧从 frame_events 抽取）。
    fn events_while(state: &mut BastionState, seconds: f32) -> Vec<BastionEvent> {
        let steps = (seconds / DT).round() as u32;
        let mut events = Vec::new();
        for _ in 0..steps {
            state.step(&BastionInput {
                dt: DT,
                ..BastionInput::default()
            });
            events.extend_from_slice(&state.frame_events);
        }
        events
    }

    /// 直接布置一只测试敌人（字段公开，构造中间态用）。
    fn enemy(id: u32, kind: EnemyKind, progress: f32) -> EnemyState {
        EnemyState {
            id,
            kind,
            progress,
            hp: kind.max_hp(),
        }
    }

    /// 把状态拨到「第 n 波已开怪」（跳过前面的波次；测试允许构造中间态）。
    fn at_wave(wave: u32) -> BastionState {
        let mut state = BastionState::new();
        state.wave = wave;
        state.spawn_queue = wave_composition(wave);
        state.spawn_timer = 0.0;
        state
    }

    #[test]
    fn path_geometry_waypoints_and_progress_mapping() {
        // 总长 = 5+7+5+6+5+7+3 = 38
        assert!((path_length() - 38.0).abs() < 1e-4, "路径总长应为 38 m");
        // 起点 / 终点精确
        assert_eq!(path_point(0.0), WAYPOINTS[0]);
        assert_eq!(path_point(38.0), *WAYPOINTS.last().expect("非空"));
        assert_eq!(
            path_point(100.0),
            *WAYPOINTS.last().expect("非空"),
            "进度钳制"
        );
        // 中段：进度 5 后进入第二段（(-5,-5) → (-5,2)，z 随进度线性）
        let p = path_point(8.5);
        assert!(
            (p.x - (-5.0)).abs() < EPS && (p.y - (-1.5)).abs() < EPS,
            "第二段插值：{p:?}"
        );
        // 方向：第二段纯 +Z
        let d = path_dir(6.0);
        assert!(
            d.x.abs() < EPS && (d.y - 1.0).abs() < EPS,
            "第二段方向应为 +Z：{d:?}"
        );
    }

    #[test]
    fn placement_check_rejects_each_branch_and_accepts_valid() {
        let mut state = BastionState::new();
        // 分支一：出界（负格 / 超宽 / 超深）
        assert_eq!(
            state.check_placement(IVec2::new(-1, 0), TowerKind::Sentry),
            Err(PlacementError::OutOfBounds)
        );
        assert_eq!(
            state.check_placement(IVec2::new(GRID_W, 0), TowerKind::Sentry),
            Err(PlacementError::OutOfBounds)
        );
        assert_eq!(
            state.check_placement(IVec2::new(0, GRID_H), TowerKind::Sentry),
            Err(PlacementError::OutOfBounds)
        );
        // 分支三：积分不足
        state.credits = MORTAR_COST - 1;
        assert_eq!(
            state.check_placement(IVec2::new(2, 3), TowerKind::Mortar),
            Err(PlacementError::InsufficientCredits)
        );
        // 成功：积分回到足够 → 落塔
        state.credits = MORTAR_COST;
        assert_eq!(
            state.check_placement(IVec2::new(2, 3), TowerKind::Mortar),
            Ok(())
        );
        state.step(&BastionInput {
            place_requests: vec![(IVec2::new(2, 3), TowerKind::Mortar)],
            dt: DT,
        });
        assert_eq!(state.towers.len(), 1);
        // 分支二：同格再放 → 已占用（且被拒的请求不落塔、不扣款）
        assert_eq!(
            state.check_placement(IVec2::new(2, 3), TowerKind::Sentry),
            Err(PlacementError::Occupied)
        );
        assert_eq!(state.towers.len(), 1, "被拒绝的请求不应落塔");
        assert_eq!(
            state.credits,
            MORTAR_COST - MORTAR_COST,
            "成功请求应恰好扣款"
        );
    }

    #[test]
    fn economy_start_kill_bounty_and_placement_cost_close_the_loop() {
        let mut state = at_wave(1);
        assert_eq!(state.credits, START_CREDITS, "初始积分应恰为 100");
        // 直接布置哨兵（测试绕过扣款）+ 射程内的疾行虫
        state.enemies.clear();
        state.spawn_queue.clear();
        state.enemies.push(enemy(100, EnemyKind::Runner, 6.0));
        state.towers.push(TowerState {
            cell: world_to_cell(path_point(6.0) + Vec2::new(1.5, 0.5)),
            kind: TowerKind::Sentry,
            cooldown: 0.0,
        });
        let credits_before = state.credits;
        let mut killed = false;
        for _ in 0..120 {
            state.step(&BastionInput {
                dt: DT,
                ..BastionInput::default()
            });
            if state.total_kills > 0 {
                killed = true;
                break;
            }
        }
        assert!(killed, "射程内的满血疾行虫应在 2 s 内被击杀");
        assert_eq!(
            state.credits - credits_before,
            RUNNER_BOUNTY,
            "击杀赏金应全额入账"
        );
        // 击杀必产生 Death 事件
        assert!(
            state
                .frame_events
                .iter()
                .any(|e| matches!(e, BastionEvent::Death { .. })),
            "击杀帧应产生 Death 事件"
        );
        // 塔心即格心（位置由格坐标派生）
        let tower = &state.towers[0];
        assert_eq!(tower.cell, world_to_cell(cell_center(tower.cell)));
    }

    #[test]
    fn sentry_targets_furthest_progress_and_respects_cooldown() {
        let mut state = at_wave(1);
        state.enemies.clear();
        state.spawn_queue.clear();
        // 两名敌人同在射程内：进度小者（近）与进度大者——应选进度最大者
        state.enemies.push(enemy(200, EnemyKind::Runner, 6.0));
        state.enemies.push(enemy(201, EnemyKind::Runner, 9.0));
        let tower_cell = world_to_cell(path_point(7.5) + Vec2::new(1.5, 0.0));
        state.towers.push(TowerState {
            cell: tower_cell,
            kind: TowerKind::Sentry,
            cooldown: 0.0,
        });
        state.step(&BastionInput {
            dt: DT,
            ..BastionInput::default()
        });
        assert_eq!(state.bullets.len(), 1, "射程内有敌人应开火");
        let bullet = &state.bullets[0];
        // 弹道应朝进度最大者（进度 9）：与「朝进度 6」的方向点积可区分
        let origin = cell_center(tower_cell);
        let to_far = (path_point(9.0) - origin).normalize_or_zero();
        let to_near = (path_point(6.0) - origin).normalize_or_zero();
        let actual = bullet.vel.normalize_or_zero();
        assert!(
            actual.dot(to_far) > 0.999,
            "应朝进度最大的敌人开火：actual {actual:?} vs to_far {to_far:?}"
        );
        assert!(actual.dot(to_near) < 0.999, "不应朝进度较小的敌人开火");
        assert_eq!(state.total_shots, 1);
        // 冷却期内不开火
        state.step(&BastionInput {
            dt: DT,
            ..BastionInput::default()
        });
        assert_eq!(state.bullets.len(), 1, "冷却期内不应开火");
        assert_eq!(state.total_shots, 1);
        // 射程外不开火
        let mut state = at_wave(1);
        state.enemies.clear();
        state.spawn_queue.clear();
        state.enemies.push(enemy(300, EnemyKind::Runner, 20.0));
        state.towers.push(TowerState {
            cell: world_to_cell(path_point(6.0) + Vec2::new(1.5, 0.0)),
            kind: TowerKind::Sentry,
            cooldown: 0.0,
        });
        state.step(&BastionInput {
            dt: DT,
            ..BastionInput::default()
        });
        assert_eq!(state.bullets.len(), 0, "射程外不应开火");
    }

    #[test]
    fn bullet_sweep_hits_and_high_speed_cannot_tunnel() {
        // 常规命中：相向接敌，2 s 内命中且弹消失、扣血
        let mut state = at_wave(1);
        state.enemies.clear();
        state.spawn_queue.clear();
        state.enemies.push(enemy(400, EnemyKind::Runner, 10.0));
        state.towers.push(TowerState {
            cell: world_to_cell(path_point(10.0) + Vec2::new(1.5, -0.5)),
            kind: TowerKind::Sentry,
            cooldown: 0.0,
        });
        let mut hit = false;
        for _ in 0..120 {
            state.step(&BastionInput {
                dt: DT,
                ..BastionInput::default()
            });
            if state
                .frame_events
                .iter()
                .any(|e| matches!(e, BastionEvent::Hit { .. }))
            {
                hit = true;
                break;
            }
        }
        assert!(hit, "哨兵弹应在 2 s 内命中");
        assert_eq!(state.enemies[0].hp, RUNNER_HP - SENTRY_DAMAGE, "命中应扣血");

        // 穿模回归：注入一条 160 m/s 的弹，本帧扫掠段从目标一侧跨到另一
        // 侧、两端采样点距敌人都 > 判定半径——点判定必漏，扫掠判定应命中
        let mut state = at_wave(1);
        state.enemies.clear();
        state.spawn_queue.clear();
        state
            .enemies
            .push(enemy(500, EnemyKind::Runner, path_length() / 2.0));
        let prey = state.enemies[0].pos();
        // 起点在目标 -X 侧 1.6 m：本帧 +2.67 m 的步长跨过目标、终点在
        // +X 侧 1.07 m——整段横穿判定圈
        state.bullets.push(BulletState {
            id: 9000,
            pos: prey - Vec2::new(1.6, 0.0),
            vel: Vec2::new(160.0, 0.0),
            life: 1.0,
        });
        let step_len = 160.0 * DT;
        let hit_radius = RUNNER_RADIUS + BULLET_RADIUS;
        assert!(
            step_len > 2.0 * hit_radius,
            "步长 {step_len} 应大于判定直径 {hit_radius}（穿模条件成立）"
        );
        let end_offset = 1.6 - step_len;
        assert!(
            1.6 > hit_radius && end_offset.abs() > hit_radius,
            "弹道两端采样点都应在判定圈外（点判定必漏的前提）"
        );
        state.step(&BastionInput {
            dt: DT,
            ..BastionInput::default()
        });
        assert!(
            state
                .frame_events
                .iter()
                .any(|e| matches!(e, BastionEvent::Hit { .. })),
            "扫掠判定应命中（防高速穿模）"
        );
        assert!(state.bullets.is_empty(), "命中后弹应消失");
    }

    #[test]
    fn mortar_shell_fires_travels_and_splashes_everyone_in_radius() {
        let mut state = at_wave(1);
        state.enemies.clear();
        state.spawn_queue.clear();
        // 落点取进度 12 处；三名敌人：两名同在落点（溅射应一石二鸟）、
        // 一名在路径远处（塔射程与溅射半径之外，对照组）。敌人到爆炸
        // 时刻还会前进 ~0.5 m（飞行 ~0.2 s），圆内余量按此留足
        let impact = path_point(12.0);
        state.enemies.push(enemy(600, EnemyKind::Runner, 12.0));
        state.enemies.push(enemy(601, EnemyKind::Runner, 12.0));
        state.enemies.push(enemy(602, EnemyKind::Runner, 20.0));
        state.towers.push(TowerState {
            cell: world_to_cell(impact + Vec2::new(0.0, 1.0)),
            kind: TowerKind::Mortar,
            cooldown: 0.0,
        });
        state.step(&BastionInput {
            dt: DT,
            ..BastionInput::default()
        });
        assert_eq!(state.shells.len(), 1, "臼炮应出膛一发");
        assert!(
            state
                .frame_events
                .iter()
                .any(|e| matches!(e, BastionEvent::MortarShot { .. })),
            "应产生臼炮开火事件"
        );
        // 落点 = 开火时目标位置的快照
        assert!(
            (state.shells[0].target - impact).length() < EPS,
            "落点应为目标当前位置快照"
        );
        // 推进到爆炸（射程内飞行 ≤ 0.6 s）
        let mut boomed = false;
        for _ in 0..90 {
            state.step(&BastionInput {
                dt: DT,
                ..BastionInput::default()
            });
            if state
                .frame_events
                .iter()
                .any(|e| matches!(e, BastionEvent::Boom { .. }))
            {
                boomed = true;
                break;
            }
        }
        assert!(boomed, "臼炮弹应在 1.5 s 内落地爆炸");
        assert!(state.shells.is_empty(), "爆炸后弹应消失");
        // 半径内的两名敌人被击杀（2 HP < 溅射 3 伤害），半径外的对照组
        // 毫发无伤地留在场上
        assert_eq!(state.total_kills, 2, "落点与溅射半径内的敌人应双双被击杀");
        assert_eq!(state.enemies.len(), 1, "半径外的对照组应存活");
        assert_eq!(state.enemies[0].hp, RUNNER_HP, "对照组不应被溅射误伤");
    }

    #[test]
    fn leak_costs_core_hp_and_zero_hp_is_defeat() {
        let mut state = at_wave(1);
        state.enemies.clear();
        state.spawn_queue.clear();
        state
            .enemies
            .push(enemy(700, EnemyKind::Runner, path_length() - 0.1));
        let leaks_before = state.total_leaks;
        let events = events_while(&mut state, 0.3);
        assert_eq!(state.total_leaks, leaks_before + 1, "走完全程应计一次漏怪");
        assert_eq!(state.core_hp, CORE_MAX_HP - 1, "漏怪应扣核心 1 点");
        assert_eq!(state.phase, Phase::Playing);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, BastionEvent::Leak { .. })),
            "漏怪应产生 Leak 事件"
        );
        // 连续漏到 0：败北 + 终局冻结
        for hp in (1..CORE_MAX_HP).rev() {
            state.core_hp = hp;
            state.enemies.push(enemy(
                800 + hp as u32,
                EnemyKind::Runner,
                path_length() - 0.1,
            ));
            run(&mut state, 0.2);
        }
        assert_eq!(state.core_hp, 0);
        assert_eq!(state.phase, Phase::Defeat, "核心归零应败北");
        let snapshot = (
            state.core_hp,
            state.wave,
            state.credits,
            state.enemies.len(),
        );
        run(&mut state, 1.0);
        assert_eq!(
            (
                state.core_hp,
                state.wave,
                state.credits,
                state.enemies.len()
            ),
            snapshot,
            "终局应冻结"
        );
    }

    #[test]
    fn spawn_interval_releases_and_wave5_clears_to_victory() {
        let mut state = BastionState::new();
        assert_eq!(state.wave, 1);
        assert_eq!(state.spawn_queue.len(), 6, "第 1 波应为 6 只");
        // 首步立即出第一只，此后按 SPAWN_INTERVAL 逐只放出
        state.step(&BastionInput {
            dt: DT,
            ..BastionInput::default()
        });
        assert_eq!(state.enemies.len(), 1, "开局首步应立即出第一只");
        assert!((state.spawn_timer - (SPAWN_INTERVAL - DT)).abs() < EPS);
        run(&mut state, 1.7);
        assert_eq!(state.enemies.len(), 3, "1.7 s 后应共出场 3 只");
        // 直接全灭第 1 波（清场 + 清队列）→ 间歇 → 第 2 波（数量守恒：
        // 出场 + 队列 = 8）
        state.enemies.clear();
        state.spawn_queue.clear();
        state.step(&BastionInput {
            dt: DT,
            ..BastionInput::default()
        });
        assert_eq!(state.breather, WAVE_BREATHER, "清空当帧应开始间歇");
        run(&mut state, WAVE_BREATHER + 0.5);
        assert_eq!(state.wave, 2);
        assert_eq!(
            state.enemies.len() + state.spawn_queue.len(),
            8,
            "第 2 波总量应为 8 只（出场 + 待出）"
        );
        // 末波清完 → 胜利 + 终局冻结（跳过战斗直接构造：13.8 s 出完全部
        // 18 只，首只疾行虫 15.8 s 才走完全程——清场窗口内不会漏怪）
        let mut state = at_wave(MAX_WAVES);
        run(&mut state, SPAWN_INTERVAL * 17.0 + 0.2);
        assert!(state.spawn_queue.is_empty(), "18 只应全部出完");
        state.enemies.clear();
        state.step(&BastionInput {
            dt: DT,
            ..BastionInput::default()
        });
        assert_eq!(state.phase, Phase::Victory, "末波清完应胜利");
        run(&mut state, 1.0);
        assert_eq!(state.phase, Phase::Victory, "终局应冻结");
        // 波次构成：第 5 波 = 12 疾行虫 + 6 重甲兽
        let wave5 = wave_composition(5);
        assert_eq!(wave5.len(), 18);
        assert_eq!(
            wave5.iter().filter(|k| **k == EnemyKind::Runner).count(),
            12
        );
        assert_eq!(wave5.iter().filter(|k| **k == EnemyKind::Brute).count(), 6);
    }

    // 敌 / 塔种类的相对关系是编译期不变量（clippy::assertions_on_constants：
    // 纯常量断言移入 const 块）
    const _: () = {
        assert!(BRUTE_HP > RUNNER_HP, "重甲兽应更肉");
        assert!(BRUTE_SPEED < RUNNER_SPEED, "重甲兽应更慢");
        assert!(BRUTE_BOUNTY > RUNNER_BOUNTY, "重甲兽赏金应更高");
        assert!(MORTAR_COOLDOWN > SENTRY_COOLDOWN, "臼炮应显著更慢");
        assert!(MORTAR_RANGE > SENTRY_RANGE, "臼炮射程应更远");
    };

    #[test]
    fn brute_is_slow_tanky_and_pricier_than_runner() {
        // 访问器与常量的一致性（运行期方法路径）
        assert_eq!(EnemyKind::Runner.max_hp(), RUNNER_HP);
        assert_eq!(EnemyKind::Brute.speed(), BRUTE_SPEED);
        assert_eq!(EnemyKind::Brute.bounty(), BRUTE_BOUNTY);
        assert_eq!(EnemyKind::Runner.radius(), RUNNER_RADIUS);
        assert_eq!(TowerKind::Sentry.cost(), SENTRY_COST);
        assert_eq!(TowerKind::Mortar.range(), MORTAR_RANGE);
        assert_eq!(TowerKind::Mortar.cooldown(), MORTAR_COOLDOWN);
    }

    #[test]
    fn enemy_movement_is_uniform_along_path() {
        let mut state = at_wave(1);
        state.enemies.clear();
        state.spawn_queue.clear();
        state.enemies.push(enemy(900, EnemyKind::Runner, 3.0));
        run(&mut state, 0.5);
        // 匀速：0.5 s 后进度 = 3 + 速度 × 0.5（仍在第一段直线内，无转折）
        let want = 3.0 + RUNNER_SPEED * 0.5;
        assert!(
            (state.enemies[0].progress - want).abs() < 1e-4,
            "匀速推进：应为 {want}，实际 {}",
            state.enemies[0].progress
        );
        // 位置严格贴线（第一段 z = -5）
        let pos = state.enemies[0].pos();
        assert!((pos.y - (-5.0)).abs() < EPS, "应贴航点线：{pos:?}");
    }

    #[test]
    fn same_input_two_runs_are_sample_identical() {
        // 完整脚本：预设放塔（含每帧混入一次必然被拒的出界请求）+ 混战 +
        // 换波直到终局，两遍推进逐帧采样全部关键字段比对（bit 级确定性）。
        /// 逐帧采样行：(核心HP, 积分, 波, 击杀, 漏怪, 敌数, 塔数, 弹数, 炮数)；
        /// 敌人行：(id, x·1024, z·1024, hp, 进度 bits, 种类, 0, 0, 0)。
        type Sample = (i32, i32, i32, i32, i32, usize, usize, usize, usize);

        fn trace() -> Vec<Sample> {
            let build: [(IVec2, TowerKind); 4] = [
                (IVec2::new(2, 3), TowerKind::Sentry),
                (IVec2::new(6, 5), TowerKind::Sentry),
                (IVec2::new(11, 4), TowerKind::Mortar),
                (IVec2::new(16, 7), TowerKind::Mortar),
            ];
            let mut state = BastionState::new();
            let mut samples = Vec::new();
            let mut frame = 0usize;
            while state.phase == Phase::Playing && frame < 60 * 300 {
                let mut input = BastionInput {
                    dt: DT,
                    ..BastionInput::default()
                };
                // 预设放塔序列：积分够即放（脚本化自动玩家的极简版）
                for &(cell, kind) in &build {
                    if state.check_placement(cell, kind).is_ok() {
                        input.place_requests.push((cell, kind));
                        break; // 每帧至多一个合法请求
                    }
                }
                // 混入一次必然被拒的出界请求（校验分支在真实推进中出现）
                input
                    .place_requests
                    .push((IVec2::new(-5, 3), TowerKind::Sentry));
                state.step(&input);
                samples.push((
                    state.core_hp,
                    state.credits as i32,
                    state.wave as i32,
                    state.total_kills as i32,
                    state.total_leaks as i32,
                    state.enemies.len(),
                    state.towers.len(),
                    state.bullets.len(),
                    state.shells.len(),
                ));
                for enemy in &state.enemies {
                    let p = enemy.pos();
                    samples.push((
                        enemy.id as i32,
                        (p.x * 1024.0) as i32,
                        (p.y * 1024.0) as i32,
                        enemy.hp,
                        enemy.progress.to_bits() as i32,
                        enemy.kind as usize,
                        0,
                        0,
                        0,
                    ));
                }
                frame += 1;
            }
            samples
        }
        let trace_a = trace();
        let trace_b = trace();
        assert_eq!(trace_a.len(), trace_b.len());
        for (i, (sa, sb)) in trace_a.iter().zip(&trace_b).enumerate() {
            assert_eq!(sa, sb, "同输入两遍应逐采样一致（bit 级），样本 #{i}");
        }
        // 采样非空转的佐证：脚本应放满 4 座塔、发生击杀并打到终局
        let towers = trace_a.iter().map(|s| s.6).fold(0usize, usize::max);
        assert_eq!(towers, 4, "脚本应放满 4 座塔");
        assert!(trace_a.iter().any(|s| s.3 > 0), "脚本期间应发生击杀");
        let terminal = trace_a
            .iter()
            .any(|s| s.0 == 0 || (s.2 == MAX_WAVES as i32 && s.5 == 0));
        assert!(terminal, "脚本应打到终局（败北或胜利）");
    }

    #[test]
    fn full_runthrough_scripted_build_reaches_terminal_state() {
        // 无胜负断言的全流程冒烟：6 塔脚本推进到终局（胜利或败北均可），
        // 波次必须推进到 5、经济与事件计数自洽——胜负平衡性由 headless
        // 自动玩家（tests/headless.rs）以「必须胜利 + 核心 ≥ 3」压榨。
        let mut state = BastionState::new();
        let build: [(IVec2, TowerKind); 6] = [
            (IVec2::new(2, 3), TowerKind::Sentry),
            (IVec2::new(6, 5), TowerKind::Sentry),
            (IVec2::new(11, 4), TowerKind::Mortar),
            (IVec2::new(16, 7), TowerKind::Mortar),
            (IVec2::new(11, 7), TowerKind::Sentry),
            (IVec2::new(6, 8), TowerKind::Mortar),
        ];
        let mut frame = 0usize;
        while state.phase == Phase::Playing && frame < 60 * 600 {
            let mut input = BastionInput {
                dt: DT,
                ..BastionInput::default()
            };
            for &(cell, kind) in &build {
                if state.check_placement(cell, kind).is_ok() {
                    input.place_requests.push((cell, kind));
                    break;
                }
            }
            state.step(&input);
            frame += 1;
        }
        assert_ne!(state.phase, Phase::Playing, "脚本应推进到终局");
        assert_eq!(state.wave, MAX_WAVES, "应打到最后一波");
        assert!(state.total_shots > 0, "对局应有开火");
        // 经济自洽：积分 + 已花造价 ≥ 初始积分 + 按最低赏金估计的击杀收入
        let spent: u32 = state.towers.iter().map(|tower| tower.kind.cost()).sum();
        let earned_floor = state.total_kills * RUNNER_BOUNTY.min(BRUTE_BOUNTY);
        assert!(
            state.credits + spent >= START_CREDITS + earned_floor - RUNNER_BOUNTY,
            "经济应守恒：credits {} + spent {} vs 初始 {} + 击杀下界 {}",
            state.credits,
            spent,
            START_CREDITS,
            earned_floor
        );
    }
}
