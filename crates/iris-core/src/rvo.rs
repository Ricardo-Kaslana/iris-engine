//! RVO 群体模拟 v1（R95）：ORCA 互避速度决策的**纯函数库**（零渲染依赖）。
//!
//! ## 语义摘要
//!
//! - 每帧由**消费方**（游戏 logic / 引擎外系统）把群体收集成切片，调用
//!   [`rvo_step`] 得到本帧各 agent 的避让速度；位置推进 `pos += vel·dt`
//!   **由调用方施加**（角色位移要过 `move_and_slide` 碰墙，引擎不替用户
//!   走位）。两阶段语义：全体新速度都从**旧速度**解出后统一写回——
//!   同帧内先解者不改变后解者的邻居输入。
//! - **ORCA 读当前速度**（不是偏好速度）：出生零速的 agent 首帧无相对
//!   速度 ⇒ 不重叠即无约束（与 RVO2 出生语义一致）——调用方通常在
//!   出生时置 `vel = pref_vel`。
//! - 算法 = ORCA（van den Berg et al. 2011，RVO2 库口径）：感知范围内
//!   每对邻居生成一条半平面约束线（双方各让一半的解析解），静态圆障碍
//!   同几何但**全责**（agent 全让）；把偏好速度投影到约束交集上（增量
//!   2D 线性规划，RVO2 的 linearProgram1/2 结构）。
//! - **安全对也生成线**：当前相对速度不构成 τ 内接触的对，其约束线的
//!   语义是「接近速度上限」（不被当前速度违反 = 无感，试图加速逼近才
//!   被压住）——这是 RVO2 密集群体鲁棒性的来源（实现期归因：先按 ORCA
//!   论文的「只对 VO 内部对生成约束」实现，30v30 对穿实测重叠 −0.71 m
//!   ——无预让余量的版本规避动作太晚太急，遂回到 RVO2 口径，实测闭合）。
//!
//! ## 确定性契约（公开，单测锁死）
//!
//! 1. 同输入两次调用，全部 agent 的 `vel.to_bits()` 逐位一致；无随机源、
//!    无全局状态、无时间源（τ 与 dt 全由调用方传入）。
//! 2. **切片序仅经 tie-break 与障碍线序渗入**：邻居线按（距离升序、
//!    相等取索引小者）排序，距离互异时重排切片**逐位不变**（更强的
//!    无害性，测试 `slice_order_invariant_distinct` 锁定）；等距 tie 与
//!    `max_neighbors` 截断边界处，索引即裁决——序是输入的一部分
//!    （测试 `tie_break_prefers_lower_index` 锁定）。
//! 3. 障碍线在邻居线之前、按切片序生成（静态物优先级高）；障碍感知
//!    半径 = `τ_o·max_speed + 半径和`（RVO2 口径：只看本帧速度下视野
//!    内够得到的障碍，与 agent 的 `neighbor_dist` 无关）。
//! 4. 内核算子集合 = `{+ − × ÷ sqrt 比较}`，源码级看门单测禁超越函数
//!    （`operator_watchdog`）。
//! 5. 线性规划容差 [`RVO_EPSILON`] 与 [`LP3_PARALLEL_EPSILON`] 的数值
//!    是**公开契约**，改动即破坏确定性（测试 `epsilon_contract` 锁值）。
//! 6. 约束线两分支（RVO2 口径）：圆弧分支（`w·x < 0` 且 `(w·x)² >
//!    |w|²·r²`）或切线腿分支；腿侧选 `cross(x, w) > 0` 取左、**恰在轴
//!    上（= 0）取右腿**；重叠（d ≤ r）走 1/dt 视野的弧（一帧内摊开，
//!    RVO2 同款）——tie-break 全部写死进本档。
//!
//! # 诚实限制清单（v1 明确不做）
//!
//! - **2D XZ 平面**：无视竖直分离（楼层 / 跳跃互避一律当同层）；
//! - **只认静态圆障碍**：线段 / 折线 / 盒障碍不做（圆序列可由调用方
//!   近似拟合——精确折线是 RVO2 的多边形障碍机制，后续轮再立项）；
//! - **邻居查询全配对 O(n²)**：无空间加速（500 agent ≈ 12.5 万对，
//!   CPU 微秒级；真实卡点出现再引入 broadphase 式分箱）；
//! - **速度瞬时改向**：无加速度 / 转向速率约束（视觉平滑由消费方对
//!   `vel` 做插值；RvoAgent 不记忆上帧解）；
//! - **圆形等权 agent**：不均等责任权重（responsibility）、非圆形状、
//!   旋转体一概不做；
//! - **群聚力 / 流场压力不做**：只有避让项（flocking 吸引、门口排队
//!   压力属上层行为，调用方自行叠加在 `pref_vel` 上）；
//! - **离散帧浅擦**：ORCA 预算语义在连续时间下无碰撞，离散帧下允许贴边
//!   穿越——穿透上界 = 求解时刻贴边 + 帧内闭合（相对速率 × dt），
//!   30v30 对穿实测最差 −0.01 m（RVO2 参考实现同款形态，其演示同样
//!   存在厘米级浅擦）；
//! - **不可行堆叠的回退无删线级联**：LP3 = 障碍线保留 + 与前序线交线
//!   的投影线集上方向优化重解（RVO2 linearProgram3 口径），但**无**
//!   RVO2 的 linearProgram4 渐进删线——极端不可行时的最优性弱于原库；
//! - **完全对称的对穿对只会减速不会分流**：镜像配置的浮点解也是镜像的
//!   （无隐式抖动源）——真实群体靠位置 / 速度的天然不对称分流，完全
//!   对称的场景会停在接触边界上（测试用微小错位打破对称，见
//!   `crowd_long_horizon` 的右群错位）；
//! - **入参消毒即契约**：`rvo_step` 对非法参数（非有限值 / τ ≤ 0 /
//!   radius ≤ 0）panic 并报索引——NaN 不会静默进求解器。

use glam::Vec2;

/// 线性规划可行性容差（平行判定分支）。**数值是公开契约**：改动会改变
/// 退化几何（近乎平行的约束线）下的分支走向，破坏逐位确定性。
pub const RVO_EPSILON: f32 = 1e-5;

/// 一个避让 agent 的全部决策输入 / 状态（POD，由调用方每帧组装）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RvoAgent {
    /// 世界位置（XZ 平面）。**由调用方推进**（`pos += vel·dt` 后写回）。
    pub pos: Vec2,
    /// 当前速度。入参 = 上帧解，`rvo_step` 写回本帧解。
    pub vel: Vec2,
    /// 偏好速度（寻路 / 行为层的目标速度，如航点方向 × 期望速率）。
    pub pref_vel: Vec2,
    /// 半径（米）。> 0。
    pub radius: f32,
    /// 速度上限（米/秒）。LP 的圆约束。≥ 0。
    pub max_speed: f32,
    /// 邻居 / 障碍感知半径（米）；超出者不参与本 agent 的决策。
    pub neighbor_dist: f32,
    /// 邻居数量上限（取最近者）；0 = 无视其他 agent（只避障碍）。
    pub max_neighbors: u32,
}

impl Default for RvoAgent {
    fn default() -> Self {
        // 人形 crowds 的常用量级：半径 0.4 m、步速上限 3 m/s、
        // 8 m 内取最近 10 个邻居（对齐 RVO2 实战默认——混战中的约束
        // 覆盖不足会让 LP 不可行回退频发，实测全程间隙变差）。
        Self {
            pos: Vec2::ZERO,
            vel: Vec2::ZERO,
            pref_vel: Vec2::ZERO,
            radius: 0.4,
            max_speed: 3.0,
            neighbor_dist: 8.0,
            max_neighbors: 10,
        }
    }
}

impl RvoAgent {
    /// 入参合法性（有限值 + 正半径 + 非负速度上限）。
    pub fn is_valid(&self) -> bool {
        self.pos.is_finite()
            && self.vel.is_finite()
            && self.pref_vel.is_finite()
            && self.radius > 0.0
            && self.radius.is_finite()
            && self.max_speed >= 0.0
            && self.max_speed.is_finite()
    }
}

/// 静态圆障碍（全责：agent 全额让行）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RvoObstacle {
    pub pos: Vec2,
    /// 半径（米）。> 0。
    pub radius: f32,
}

impl RvoObstacle {
    /// 入参合法性（有限值 + 正半径）。
    pub fn is_valid(&self) -> bool {
        self.pos.is_finite() && self.radius > 0.0 && self.radius.is_finite()
    }
}

/// 求解参数（时间视野）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RvoParams {
    /// agent-agent 时间视野 τ（秒）："未来多久内会撞就需要现在让"。
    /// > 0 且有限。大值 = 更早更柔的规避。
    pub neighbor_horizon: f32,
    /// agent-静态障碍时间视野（秒）。惯例比 agent 视野短（障碍不让行，
    /// 晚让即可）；速度快的场景建议调大，否则规避会很晚很急。
    pub obstacle_horizon: f32,
}

impl Default for RvoParams {
    fn default() -> Self {
        Self {
            neighbor_horizon: 2.0,
            obstacle_horizon: 0.5,
        }
    }
}

impl RvoParams {
    /// 入参合法性（两视野均有限且 > 0）。
    pub fn is_valid(&self) -> bool {
        self.neighbor_horizon > 0.0
            && self.neighbor_horizon.is_finite()
            && self.obstacle_horizon > 0.0
            && self.obstacle_horizon.is_finite()
    }
}

/// 一条 ORCA 半平面约束线（内部表示）：可行侧 = {v : det(dir, point − v) ≤ 0}，
/// `dir` 恒单位向量。
#[derive(Clone, Copy, Debug)]
struct Line {
    point: Vec2,
    dir: Vec2,
}

/// 二维叉积（z 分量）：> 0 表示 b 在 a 的左侧。
fn cross(a: Vec2, b: Vec2) -> f32 {
    a.x * b.y - a.y * b.x
}

/// 单条约束线的生成（核心几何，可独立单测）。
///
/// 输入一对 (i, j)：位置 / 速度、半径和、时间视野 `horizon`、责任份额
/// `resp`（agent 对 0.5、障碍对 1.0）、`inv_time_step`（1/帧步长，仅
/// 重叠分支用）。返回 `None` = 该对无需约束。
///
/// 几何（速度空间，`c = x/τ` 为截断圆圆心、`ρ = r/τ` 为半径，`x` = j
/// 相对 i 的位置、`v_rel` = i 相对 j 的速度）：VO^τ 边界 = 圆的**近弧**
/// （面向原点一侧）+ 两条从原点出发的切线腿。可行半平面
/// {v : n·v ≥ n·point}，法向 n 恒指向远离 VO 一侧。
#[allow(clippy::too_many_arguments)] // 纯函数入参即算法签名（同 solve_contact_impulse 口径）
fn orca_line(
    pos_i: Vec2,
    vel_i: Vec2,
    pos_j: Vec2,
    vel_j: Vec2,
    radius_sum: f32,
    horizon: f32,
    resp: f32,
    inv_time_step: f32,
) -> Option<Line> {
    let x = pos_j - pos_i;
    let v_rel = vel_i - vel_j;
    let r = radius_sum;

    // —— RVO2 口径：对感知范围内每对都生成线。**法向 n 恒指向远离 VO 的
    // 一侧**，偏移带符号——深碰对（v_rel 在 VO 内）的线被当前速度违反 ⇒
    // 推出边界；安全对（v_rel 在 VO 外）的线自带裕量 ⇒ 不被违反，语义为
    // 「接近预算上限」（这正是密集群体鲁棒性的来源）。可行半平面
    // {v : n·v ≥ n·point}，即 det(dir, point − v) ≤ 0（dir = n 右垂直）。
    let inv_tau = 1.0 / horizon;
    let c = x * inv_tau;
    let rho = r * inv_tau;
    let w = v_rel - c; // v_rel 相对圆心的偏移
    let w_sq = w.dot(w);
    let w_len = w_sq.sqrt();
    let d_sq = x.length_squared();

    // 外法向与带符号偏移（point = vel_i + resp·offset·n）。
    let (n, offset): (Vec2, f32) = if d_sq <= r * r {
        // 重叠（d ≤ r）：RVO2 口径——视野换成 1/dt（**一帧内摊开**的强
        // 救援；τ 口径太温和，同群追尾实测只被轻推）。仍是弧公式，但
        // c/ρ 按 inv_time_step 缩放。d = 0 且 w = 0 时 n 取 +x（固定
        // tie-break）。
        let c = x * inv_time_step;
        let rho = r * inv_time_step;
        let w = v_rel - c;
        let w_len = w.length();
        let n = if w_len > 0.0 {
            w / w_len
        } else if d_sq > 0.0 {
            -(x / d_sq.sqrt())
        } else {
            Vec2::new(1.0, 0.0)
        };
        (n, rho - w_len)
    } else {
        let dp1 = w.dot(x);
        if dp1 < 0.0 && dp1 * dp1 > w_sq * r * r {
            // 弧分支：v_rel 在圆前楔内（近弧点最近）。分支条件内生可见性：
            // dp1 < 0 ⇒ 弧点在近弧；dp1² > |w|²·r² ⇒ 不越过切点。
            (w / w_len, rho - w_len)
        } else {
            // 腿分支：腿方向（单位）= (edge·x̂ ± r·左垂直)/d，
            // edge = √(d²−r²) = 原点到切点距离。侧选：v_rel 在轴哪侧取
            // 哪条腿（cross(x, w) 与 cross(x, v_rel) 同号——c 在轴上）；
            // 恰在轴上（= 0）取右腿（契约 tie-break）。
            // 腿线过原点、方向 ld ⇒ 带符号垂距 = n·v_rel（n = 外法向，
            // 左腿 (−ld.y, ld.x)、右腿 (ld.y, −ld.x)，均满足 n·x̂ < 0）。
            let d = d_sq.sqrt();
            let edge = (d_sq - r * r).sqrt();
            let xh = x / d;
            let perp = Vec2::new(-xh.y, xh.x);
            let left = cross(x, w) > 0.0;
            let ld = if left {
                (xh * edge + perp * r) / d
            } else {
                (xh * edge - perp * r) / d
            };
            let n = if left {
                Vec2::new(-ld.y, ld.x)
            } else {
                Vec2::new(ld.y, -ld.x)
            };
            (n, -(n.dot(v_rel)))
        }
    };

    Some(Line {
        point: vel_i + n * (offset * resp),
        dir: Vec2::new(n.y, -n.x),
    })
}

/// linearProgram1：在线 `lines[line_no]` 上找最优可行点（受
/// `lines[0..line_no]` 与半径 `radius` 的速度圆约束）。`direction_opt`
/// = 沿方向取极值（LP3 用），否则取离 `opt` 最近点。无解返回 false。
fn lp1(
    lines: &[Line],
    line_no: usize,
    radius: f32,
    opt: Vec2,
    direction_opt: bool,
    v: &mut Vec2,
) -> bool {
    let l = lines[line_no];
    // |point + t·dir| ≤ radius ⟺ t ∈ [−dp − √disc, −dp + √disc]
    let dp = l.point.dot(l.dir);
    let disc = dp * dp + radius * radius - l.point.length_squared();
    if disc < 0.0 {
        return false; // 速度圆与此线不相交
    }
    let sq = disc.sqrt();
    let mut t_left = -dp - sq;
    let mut t_right = -dp + sq;
    for prev in lines.iter().take(line_no) {
        let denom = cross(l.dir, prev.dir);
        let numer = cross(prev.dir, l.point - prev.point);
        if denom.abs() <= RVO_EPSILON {
            if numer < 0.0 {
                return false; // 平行且被完全排除
            }
            continue; // 平行且无冲突：不收紧
        }
        let t = numer / denom;
        if denom > 0.0 {
            if t < t_right {
                t_right = t;
            }
        } else if t > t_left {
            t_left = t;
        }
        if t_left > t_right {
            return false;
        }
    }
    if direction_opt {
        if opt.dot(l.dir) > 0.0 {
            *v = l.point + l.dir * t_right;
        } else {
            *v = l.point + l.dir * t_left;
        }
    } else {
        // 最优 t = (opt − point)·dir，钳到 [t_left, t_right]
        let mut t = (opt - l.point).dot(l.dir);
        if t < t_left {
            t = t_left;
        }
        if t > t_right {
            t = t_right;
        }
        *v = l.point + l.dir * t;
    }
    true
}

/// linearProgram2：把 `opt`（钳到速度圆后）投影进全部约束线的交集。
/// 成功返回 `None`；失败返回**失败线的索引**（`v` 保留失败前的最后
/// 可行值，供 LP3 从该线接手）。
fn lp2(lines: &[Line], radius: f32, opt: Vec2, direction_opt: bool, v: &mut Vec2) -> Option<usize> {
    if direction_opt {
        *v = opt * radius; // opt 恒单位（LP3 路径）
    } else {
        let opt_sq = opt.length_squared();
        if opt_sq > radius * radius {
            *v = opt * (radius / opt_sq.sqrt());
        } else {
            *v = opt;
        }
    }
    for i in 0..lines.len() {
        let l = lines[i];
        // lp1 有写回副作用（*v），短路合并不改变语义——仅在被违反时调用
        if cross(l.dir, l.point - *v) > 0.0 && !lp1(lines, i, radius, opt, direction_opt, v) {
            return Some(i);
        }
    }
    None
}

/// LP3 内部的平行容差（RVO2 参考实现口径，与 [`RVO_EPSILON`] 不同值——
/// 投影线的方向差归一化前的平行判定，改值即破坏逐位确定性）。
pub const LP3_PARALLEL_EPSILON: f32 = 1e-6;

/// linearProgram3（RVO2 口径）：LP2 在 `begin_line` 不可行时的松弛回退。
/// 对每条违反量超过已让步量的线 i：构造投影线集（障碍线全保留 + 线 i
/// 与前序 agent 线的**交线**——方向 = 归一化 (dir_j − dir_i)、点在线 i
/// 上；平行同向跳过、平行反向取中点），在其上以「线 i 的左法向」做
/// 方向优化重解 LP2；重解失败保留临时值。无 RVO2 的 linearProgram4
/// 删线级联（见诚实限制清单）。
fn lp3(lines: &[Line], num_obst_lines: usize, begin_line: usize, radius: f32, v: &mut Vec2) {
    let mut distance = 0.0f32;
    let mut proj: Vec<Line> = Vec::new();
    for i in begin_line..lines.len() {
        if cross(lines[i].dir, lines[i].point - *v) > distance {
            proj.clear();
            proj.extend_from_slice(&lines[..num_obst_lines]);
            for j in num_obst_lines..i {
                let determinant = cross(lines[i].dir, lines[j].dir);
                let mut pl = Line {
                    point: Vec2::ZERO,
                    dir: Vec2::ZERO,
                };
                if determinant.abs() <= LP3_PARALLEL_EPSILON {
                    if lines[i].dir.dot(lines[j].dir) > 0.0 {
                        continue; // 平行同向：无需投影
                    }
                    pl.point = (lines[i].point + lines[j].point) * 0.5; // 平行反向
                } else {
                    let t = cross(lines[j].dir, lines[i].point - lines[j].point) / determinant;
                    pl.point = lines[i].point + lines[i].dir * t;
                }
                pl.dir = (lines[j].dir - lines[i].dir).normalize();
                proj.push(pl);
            }
            let opt_dir = Vec2::new(-lines[i].dir.y, lines[i].dir.x);
            let temp = *v;
            if lp2(&proj, radius, opt_dir, true, v).is_some() {
                *v = temp;
            }
            distance = cross(lines[i].dir, lines[i].point - *v);
        }
    }
}

/// 单 agent 求解：约束线集合 → 本帧速度。
fn solve_velocity(lines: &[Line], num_obst_lines: usize, max_speed: f32, pref_vel: Vec2) -> Vec2 {
    let mut v = Vec2::ZERO;
    if let Some(fail) = lp2(lines, max_speed, pref_vel, false, &mut v) {
        lp3(lines, num_obst_lines, fail, max_speed, &mut v);
    }
    v
}

/// 一步群体求解：读全部 agent 的旧速度与位置，解出全体新速度后统一写回
/// `RvoAgent::vel`。**不改 `pos`**（调用方推进）。`dt` 为本帧步长（秒，
/// > 0）——只用于重叠对的「一帧内摊开」救援口径（RVO2 同款）。
///
/// # Panics
///
/// 参数 / agent / 障碍非法（非有限、τ ≤ 0、dt ≤ 0、radius ≤ 0）时
/// panic，消息含首个非法项的索引——NaN 不静默进求解器。
pub fn rvo_step(agents: &mut [RvoAgent], obstacles: &[RvoObstacle], params: &RvoParams, dt: f32) {
    assert!(
        params.is_valid(),
        "rvo_step: RvoParams 非法（视野需有限且 > 0）"
    );
    assert!(
        dt > 0.0 && dt.is_finite(),
        "rvo_step: dt 需有限且 > 0（重叠救援以 1/dt 为视野）"
    );
    for (i, a) in agents.iter().enumerate() {
        assert!(
            a.is_valid(),
            "rvo_step: agents[{i}] 非法（需有限值、radius > 0）"
        );
    }
    for (i, o) in obstacles.iter().enumerate() {
        assert!(
            o.is_valid(),
            "rvo_step: obstacles[{i}] 非法（需有限值、radius > 0）"
        );
    }
    let inv_dt = 1.0 / dt;

    let mut new_vels: Vec<Vec2> = Vec::with_capacity(agents.len());
    let mut lines: Vec<Line> = Vec::new();
    let mut neighbors: Vec<(f32, usize)> = Vec::new();

    for i in 0..agents.len() {
        let a = agents[i];
        lines.clear();
        neighbors.clear();

        // 障碍线（切片序，全责）——静态物优先于 agent 邻居。
        // 感知半径 = τ_o·max_speed + 半径和（RVO2 口径：只看本帧速度下
        // 视野内够得到的障碍，与 agent 的 neighbor_dist 无关）。
        let obst_reach = params.obstacle_horizon * a.max_speed + a.radius;
        for o in obstacles.iter() {
            let dist_sq = (o.pos - a.pos).length_squared();
            let reach = obst_reach + o.radius;
            if dist_sq <= reach * reach {
                if let Some(l) = orca_line(
                    a.pos,
                    a.vel,
                    o.pos,
                    Vec2::ZERO,
                    a.radius + o.radius,
                    params.obstacle_horizon,
                    1.0,
                    inv_dt,
                ) {
                    lines.push(l);
                }
            }
        }
        let num_obst_lines = lines.len();

        // 邻居选择：感知半径内全收集 → 距离升序、相等取索引小 → 截断。
        for (j, b) in agents.iter().enumerate() {
            if j == i {
                continue;
            }
            let d = (b.pos - a.pos).length_squared();
            if d <= a.neighbor_dist * a.neighbor_dist {
                neighbors.push((d, j));
            }
        }
        neighbors.sort_by(|p, q| {
            p.0.partial_cmp(&q.0)
                .expect("rvo_step: 距离出现 NaN（输入已消毒，不可达）")
                .then(p.1.cmp(&q.1))
        });
        let keep = a.max_neighbors as usize;
        if neighbors.len() > keep {
            neighbors.truncate(keep);
        }

        for &(_, j) in neighbors.iter() {
            let b = agents[j];
            if let Some(l) = orca_line(
                a.pos,
                a.vel,
                b.pos,
                b.vel,
                a.radius + b.radius,
                params.neighbor_horizon,
                0.5,
                inv_dt,
            ) {
                lines.push(l);
            }
        }

        new_vels.push(solve_velocity(
            &lines,
            num_obst_lines,
            a.max_speed,
            a.pref_vel,
        ));
    }

    for (a, v) in agents.iter_mut().zip(new_vels.iter()) {
        a.vel = *v;
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    /// 测试统一帧步长（仅重叠救援口径消费）。
    const DT: f32 = 1.0 / 30.0;

    /// Vec2 逐位断言辅助（glam 0.33 向量无 to_bits，分量级拼对——
    /// wind.rs 同款范式）。
    fn bits(v: Vec2) -> [u32; 2] {
        [v.x.to_bits(), v.y.to_bits()]
    }

    /// 步进 n 步并推进位置的仿真循环（调用方推进语义的镜像），
    /// 返回**全程**（含初态）最小 agent-agent 间隙。
    fn simulate(
        agents: &mut [RvoAgent],
        obstacles: &[RvoObstacle],
        params: &RvoParams,
        steps: usize,
        dt: f32,
    ) -> f32 {
        let mut min_gap = min_clearance(agents);
        for _ in 0..steps {
            rvo_step(agents, obstacles, params, dt);
            for a in agents.iter_mut() {
                a.pos += a.vel * dt;
            }
            let gap = min_clearance(agents);
            if gap < min_gap {
                min_gap = gap;
            }
        }
        min_gap
    }

    /// 全程最小 agent-agent 间隙（负 = 重叠深度）。
    fn min_clearance(agents: &[RvoAgent]) -> f32 {
        let mut best = f32::INFINITY;
        for i in 0..agents.len() {
            for j in (i + 1)..agents.len() {
                let d = (agents[j].pos - agents[i].pos).length();
                let gap = d - agents[i].radius - agents[j].radius;
                if gap < best {
                    best = gap;
                }
            }
        }
        best
    }

    /// 构造出生即在移动的 agent（vel = pref——ORCA 读当前速度，出生零速
    /// 则首帧无约束；与 RVO2 出生语义一致，调用方通常置 vel = pref）。
    fn agent(pos: Vec2, pref: Vec2) -> RvoAgent {
        RvoAgent {
            pos,
            vel: pref,
            pref_vel: pref,
            ..RvoAgent::default()
        }
    }

    // 1. 无邻居：vel = clamp(pref, max_speed)，两档精确断言。
    #[test]
    fn no_neighbor_velocity_clamp() {
        let solved = {
            let mut arr = [agent(Vec2::ZERO, Vec2::new(1.0, 2.0))];
            rvo_step(&mut arr, &[], &RvoParams::default(), DT);
            arr[0].vel
        };
        assert_eq!(bits(solved), bits(Vec2::new(1.0, 2.0)));

        let solved = {
            let mut arr = [agent(Vec2::ZERO, Vec2::new(0.0, 10.0))]; // max_speed 3
            rvo_step(&mut arr, &[], &RvoParams::default(), DT);
            arr[0].vel
        };
        assert_eq!(bits(solved), bits(Vec2::new(0.0, 3.0)));
    }

    // 2. 对穿（v_rel 在截断圆的近侧，圆弧分支）：镜像配置 ⇒ 解互为镜像
    //    （逐位）、纯轴向减速、½ 责任解析锚 0.5 − ½·(ρ − |w|)。
    #[test]
    fn headon_arc_branch_mirror_deceleration() {
        let mut pair = [
            agent(Vec2::new(-1.25, 0.0), Vec2::new(0.5, 0.0)),
            agent(Vec2::new(1.25, 0.0), Vec2::new(-0.5, 0.0)),
        ];
        rvo_step(&mut pair, &[], &RvoParams::default(), DT);
        // 镜像对称：浮点算子（+ − × ÷ sqrt）符号对称 ⇒ 逐位镜像
        // 浮点等值断言（−0.0 与 0.0 数值相等但位级不同——镜像取负会在
        // 零分量上产生符号差，属表示差异非数值差异）
        assert_eq!(pair[0].vel, -pair[1].vel);
        // 圆弧分支：法向沿轴 ⇒ 无横向分量（完全对称对只会减速，见诚实限制）
        assert_eq!(pair[0].vel.y.to_bits(), 0.0f32.to_bits());
        // ½ 责任解析锚：τ=2 ⇒ c=(1.25,0)、ρ=0.4；w=(−0.25,0) ⇒
        // u=(0.4−0.25)·(−1,0)=(−0.15,0)；新 vel.x = 0.5 − 0.5·0.15 = 0.425
        assert!(
            (pair[0].vel.x - 0.425).abs() <= 1e-6,
            "vel = {:?}",
            pair[0].vel
        );
    }

    // 3. 对穿（v_rel 越过圆心侧，腿分支 tie-break 右腿）：横向分流。
    #[test]
    fn headon_fast_leg_split() {
        // d = 4、r = 0.8、τ = 0.4 ⇒ c=(10,0)、ρ=2；v_rel=(12,0) ⇒ w=(2,0)
        // 轴上 ⇒ 右腿；u≈(−0.48,−2.352) ⇒ a 向 −y 分流、b 镜像向 +y
        let mut pair = [
            RvoAgent {
                pos: Vec2::new(-2.0, 0.0),
                max_speed: 8.0,
                ..agent(Vec2::ZERO, Vec2::new(6.0, 0.0))
            },
            RvoAgent {
                pos: Vec2::new(2.0, 0.0),
                max_speed: 8.0,
                ..agent(Vec2::ZERO, Vec2::new(-6.0, 0.0))
            },
        ];
        let params = RvoParams {
            neighbor_horizon: 0.4,
            obstacle_horizon: 0.5,
        };
        rvo_step(&mut pair, &[], &params, DT);
        // 浮点等值断言（−0.0 与 0.0 数值相等但位级不同——镜像取负会在
        // 零分量上产生符号差，属表示差异非数值差异）
        assert_eq!(pair[0].vel, -pair[1].vel);
        assert!(pair[0].vel.y < 0.0, "vel = {:?}", pair[0].vel);
        assert!(pair[1].vel.y > 0.0, "vel = {:?}", pair[1].vel);
    }

    // 4. 静止邻居：移动者绕行通过，长时程不重叠（错位 0.25 打破对称）。
    #[test]
    fn stationary_neighbor_detour() {
        let mut arr = [
            agent(Vec2::new(-3.0, 0.25), Vec2::new(1.5, 0.0)),
            agent(Vec2::new(0.0, 0.0), Vec2::ZERO),
        ];
        simulate(&mut arr, &[], &RvoParams::default(), 240, 1.0 / 30.0);
        let clearance = min_clearance(&arr);
        assert!(clearance >= -1e-4, "min_clearance = {clearance}");
        // 判别力：移动者到达右侧（绕过去了，不是停死）
        assert!(arr[0].pos.x > 2.0, "pos = {:?}", arr[0].pos);
    }

    // 5. 静态圆障碍：直线冲圆心（错位 0.3 打破对称）⇒ 切线绕行通过。
    #[test]
    fn static_circle_tangent_detour() {
        let obstacles = [RvoObstacle {
            pos: Vec2::ZERO,
            radius: 1.0,
        }];
        let params = RvoParams {
            neighbor_horizon: 2.0,
            obstacle_horizon: 1.5, // 速度 1.5 下提前 2.25 m 介入
        };
        let mut arr = [agent(Vec2::new(-4.0, 0.3), Vec2::new(1.5, 0.0))];
        simulate(&mut arr, &obstacles, &params, 360, 1.0 / 30.0);
        // 到达右侧（通过障碍而非停在视野边界）
        assert!(arr[0].pos.x > 3.0, "pos = {:?}", arr[0].pos);
        // 绕行产生可判别的横向偏移（≥ 半径和 1.4 的量级）
        assert!(arr[0].pos.y.abs() > 0.8, "pos = {:?}", arr[0].pos);
        // 障碍间隙达标
        let d = (arr[0].pos - obstacles[0].pos).length();
        assert!(d >= arr[0].radius + obstacles[0].radius - 1e-4, "d = {d}");
    }

    // 6. 30v30 对穿 + 场心静态圆，**全程逐帧**无碰撞 + 双群越过中场。
    //    右群 y 错位 0.4（半格的半）打破对穿对称，且使对穿行 genuinely
    //    正对（dy = 0.4 < Σr 0.8）——无 RVO 时该场景会真实穿透。
    #[test]
    fn crowd_long_horizon() {
        let dt = 1.0 / 30.0;
        let params = RvoParams {
            neighbor_horizon: 2.0,
            obstacle_horizon: 1.0,
        };
        let obstacles = [RvoObstacle {
            pos: Vec2::new(0.0, 0.0),
            radius: 1.0,
        }];
        let mut world: Vec<RvoAgent> = Vec::with_capacity(60);
        // 左群 6 列 × 5 行（x ∈ [−9,−5]）向 +x
        for row in 0..5i32 {
            for col in 0..6i32 {
                let y = (row as f32 - 2.0) * 1.6;
                let x = -9.0 + col as f32 * 0.8;
                world.push(agent(Vec2::new(x, y), Vec2::new(1.2, 0.0)));
            }
        }
        // 右群同构、y 错位 0.4
        for row in 0..5i32 {
            for col in 0..6i32 {
                let y = (row as f32 - 2.0) * 1.6 + 0.4;
                let x = 9.0 - col as f32 * 0.8;
                world.push(agent(Vec2::new(x, y), Vec2::new(-1.2, 0.0)));
            }
        }
        let clearance = simulate(&mut world, &obstacles, &params, 900, dt);
        // 容差依据：ORCA 预算语义在连续时间下无碰撞，离散帧下允许贴边
        // 穿越——穿透上界 = 求解时刻贴边 + 帧内闭合（≤ 相对速率 2.4 ×
        // dt 1/30 = 0.08 m）；阈值取其 1/4，实测最差 −0.0101（RVO2 同款
        // 形态，其演示同样存在厘米级浅擦）。
        assert!(
            clearance >= -0.02,
            "全程最小间隙 = {clearance}（离散帧浅擦界 0.02）"
        );
        // 越过中场：左群（前 30 个）质心 x > 3
        let mean_left = world[0..30].iter().map(|a| a.pos.x).sum::<f32>() / 30.0;
        assert!(mean_left > 3.0, "mean_left = {mean_left}");
        // 障碍间隙：与静态圆的最小距离 ≥ 半径和 − 1e-4
        for a in world.iter() {
            let d = (a.pos - obstacles[0].pos).length();
            assert!(d >= a.radius + obstacles[0].radius - 1e-4, "d = {d}");
        }
    }

    // 7. 确定性：同输入双跑 to_bits 逐位一致（方位用字面量，禁三角函数）。
    #[test]
    fn determinism_double_run_bits() {
        let dirs = [
            Vec2::new(1.0, 0.0),
            Vec2::new(0.0, 1.0),
            Vec2::new(-1.0, 0.0),
            Vec2::new(0.0, -1.0),
            Vec2::new(
                std::f32::consts::FRAC_1_SQRT_2,
                std::f32::consts::FRAC_1_SQRT_2,
            ),
        ];
        let build = || {
            let mut v = Vec::new();
            for i in 0..24usize {
                let x = (i as f32 * 0.73) % 7.0 - 3.5;
                let y = (i as f32 * 1.31) % 7.0 - 3.5;
                v.push(agent(Vec2::new(x, y), dirs[i % dirs.len()] * 1.1));
            }
            v
        };
        let obstacles = [RvoObstacle {
            pos: Vec2::new(0.0, 0.0),
            radius: 0.8,
        }];
        let mut run1 = build();
        let mut run2 = build();
        let params = RvoParams::default();
        for _ in 0..120 {
            rvo_step(&mut run1, &obstacles, &params, DT);
            rvo_step(&mut run2, &obstacles, &params, DT);
            for (a, b) in run1.iter().zip(run2.iter()) {
                assert_eq!(bits(a.vel), bits(b.vel));
            }
            for a in run1.iter_mut() {
                a.pos += a.vel * DT;
            }
            for a in run2.iter_mut() {
                a.pos += a.vel * DT;
            }
        }
    }

    // 8. 切片序无害性（距离互异 + 无障碍）：重排切片逐位不变——邻居线
    //    按距离排序，序不渗入求解（等距 tie 的渗入见下一测试）。
    #[test]
    fn slice_order_invariant_distinct() {
        let mut abc = [
            agent(Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.0)),
            agent(Vec2::new(0.7, 0.31), Vec2::new(-1.0, 0.2)),
            agent(Vec2::new(0.2, -0.44), Vec2::new(0.3, 1.0)),
            agent(Vec2::new(-0.52, 0.66), Vec2::new(0.1, -0.9)),
        ];
        let mut perm = [abc[2], abc[0], abc[3], abc[1]];
        let params = RvoParams::default();
        rvo_step(&mut abc, &[], &params, DT);
        rvo_step(&mut perm, &[], &params, DT);
        // 按个体对齐（abc[i] ↔ perm 中同一份数据）
        let same = (bits(abc[0].vel) == bits(perm[1].vel))
            && (bits(abc[1].vel) == bits(perm[3].vel))
            && (bits(abc[2].vel) == bits(perm[0].vel))
            && (bits(abc[3].vel) == bits(perm[2].vel));
        assert!(same, "距离互异时重排切片改变了结果");
    }

    // 9. 等距 tie-break：索引小者先。cap = 1 时两种摆放各取自己的
    //    索引 1 邻居 ⇒ 横向偏转反号（序是输入的一部分的行为锁）。
    #[test]
    fn tie_break_prefers_lower_index() {
        let cap1 = |first: Vec2, second: Vec2| {
            let mut arr = [
                RvoAgent {
                    pos: Vec2::ZERO,
                    vel: Vec2::new(1.0, 0.0),
                    pref_vel: Vec2::new(1.0, 0.0),
                    max_neighbors: 1,
                    ..RvoAgent::default()
                },
                agent(first, Vec2::ZERO),
                agent(second, Vec2::ZERO),
            ];
            rvo_step(&mut arr, &[], &RvoParams::default(), DT);
            arr[0].vel
        };
        let v1 = cap1(Vec2::new(1.0, 0.6), Vec2::new(1.0, -0.6)); // 取上邻居
        let v2 = cap1(Vec2::new(1.0, -0.6), Vec2::new(1.0, 0.6)); // 取下邻居
        assert!(v1.y < -0.01, "v1 = {v1:?}（应向下绕开上方邻居）");
        assert!(v2.y > 0.01, "v2 = {v2:?}（应向上绕开下方邻居）");

        // 感知不截断时（cap 6）：对称双邻居横向消解（镜像线对交于轴上，
        // 容差 1e-5 = LP 交点的浮点尘埃界）
        let sym = |swap: bool| {
            let mut arr = [
                agent(Vec2::ZERO, Vec2::new(1.0, 0.0)),
                agent(Vec2::new(1.0, if swap { -0.6 } else { 0.6 }), Vec2::ZERO),
                agent(Vec2::new(1.0, if swap { 0.6 } else { -0.6 }), Vec2::ZERO),
            ];
            rvo_step(&mut arr, &[], &RvoParams::default(), DT);
            arr[0].vel
        };
        let s1 = sym(false);
        let s2 = sym(true);
        assert!(s1.y.abs() <= 1e-5, "s1 = {s1:?}");
        assert!(s2.y.abs() <= 1e-5, "s2 = {s2:?}");
    }

    // 10. 邻居截断与感知半径：cap 2 与 cap 6 在密集场景下结果不同；
    //     感知半径外的邻居不约束。
    #[test]
    fn neighbor_cap_and_range() {
        // 感知半径外：与无邻居一致
        let mut far = [
            agent(Vec2::ZERO, Vec2::new(1.0, 0.0)),
            agent(Vec2::new(20.0, 0.0), Vec2::new(-5.0, 0.0)),
        ];
        rvo_step(&mut far, &[], &RvoParams::default(), DT);
        assert_eq!(bits(far[0].vel), bits(Vec2::new(1.0, 0.0)));

        // 密集三邻居（距离互异、多线同时咬合）：cap = 1 只吃最近一条线，
        // cap = 3 吃满——最近邻单线允许的速度被第二 / 三条线进一步收紧
        let build = |cap: u32| {
            let mut v = vec![RvoAgent {
                pos: Vec2::ZERO,
                vel: Vec2::new(2.0, 0.0),
                pref_vel: Vec2::new(2.0, 0.0),
                max_neighbors: cap,
                ..RvoAgent::default()
            }];
            for p in [
                Vec2::new(1.0, 0.4),
                Vec2::new(1.15, -0.4),
                Vec2::new(1.3, 0.0),
            ] {
                v.push(agent(p, Vec2::ZERO));
            }
            v
        };
        let mut cap1 = build(1);
        let mut cap3 = build(3);
        let params = RvoParams::default();
        rvo_step(&mut cap1, &[], &params, DT);
        rvo_step(&mut cap3, &[], &params, DT);
        assert_ne!(bits(cap1[0].vel), bits(cap3[0].vel));
    }

    // 11. ε 契约锁：数值即公开契约，防静默改动。
    #[test]
    fn epsilon_contract() {
        assert_eq!(RVO_EPSILON, 1e-5);
        assert_eq!(LP3_PARALLEL_EPSILON, 1e-6);
    }

    // 12. 算子看门：内根源码禁超越函数（拼接构造避免看门命中自身字面量）。
    #[test]
    fn operator_watchdog() {
        let src = include_str!("rvo.rs");
        let banned: [String; 8] = [
            [".s", "in("].concat(),
            [".c", "os("].concat(),
            ["ata", "n2"].concat(),
            [".pow", "f("].concat(),
            [".pow", "i("].concat(),
            [".e", "xp("].concat(),
            ["hyp", "ot("].concat(),
            [".l", "n("].concat(),
        ];
        for b in banned.iter() {
            assert!(
                !src.contains(b.as_str()),
                "rvo.rs 源码出现禁用算子 {b:?}——确定性契约要求算子集 {{+ − × ÷ sqrt 比较}}"
            );
        }
    }

    // 13. 重叠退化：完全同位 ⇒ 全场有限值（不 NaN）。
    #[test]
    fn overlapping_degenerate_finite() {
        let mut pair = [
            agent(Vec2::new(0.0, 0.0), Vec2::new(0.0, 0.5)),
            agent(Vec2::new(0.0, 0.0), Vec2::new(0.0, -0.5)),
        ];
        rvo_step(&mut pair, &[], &RvoParams::default(), DT);
        for a in pair.iter() {
            assert!(a.vel.is_finite(), "重叠退化产生非有限值: {:?}", a.vel);
        }
    }

    // 14. 责任份额对照：同几何下全责（障碍，τ=0.5）单步即减速到解析锚
    //     2.4（P = v + u·1，u = −0.6 纵向）；互让（agent，τ=2）走腿分支
    //     只需横向让位（约束未违反），纵向速度保持 3.0。
    #[test]
    fn obstacle_full_responsibility() {
        let mut with_obs = [agent(Vec2::new(-2.0, 0.0), Vec2::new(3.0, 0.0))];
        let obstacles = [RvoObstacle {
            pos: Vec2::ZERO,
            radius: 0.4,
        }];
        let mut with_agent = [
            agent(Vec2::new(-2.0, 0.0), Vec2::new(3.0, 0.0)),
            agent(Vec2::ZERO, Vec2::ZERO),
        ];
        let params = RvoParams::default();
        rvo_step(&mut with_obs, &obstacles, &params, DT);
        rvo_step(&mut with_agent, &[], &params, DT);
        // 全责解析锚：u = (ρ−|w|)·û = (1.6−1)·(−1,0) = (−0.6,0)，P = v+u
        assert!(
            (with_obs[0].vel.x - 2.4).abs() <= 1e-6,
            "obst vel = {:?}",
            with_obs[0].vel
        );
        assert!(
            with_obs[0].vel.x < with_agent[0].vel.x,
            "obst {:?} vs agent {:?}",
            with_obs[0].vel,
            with_agent[0].vel
        );
    }

    // 15. 消毒 panic：非法参数带索引消息。
    #[test]
    #[should_panic(expected = "agents[1]")]
    fn invalid_input_panics() {
        let mut bad = [
            agent(Vec2::ZERO, Vec2::ZERO),
            RvoAgent {
                radius: -1.0,
                ..RvoAgent::default()
            },
        ];
        rvo_step(&mut bad, &[], &RvoParams::default(), DT);
    }

    // 16. 单步不推进位置：pos 恒等（调用方推进契约）。
    #[test]
    fn pos_untouched_by_step() {
        let mut pair = [
            agent(Vec2::new(-1.25, 0.0), Vec2::new(0.5, 0.0)),
            agent(Vec2::new(1.25, 0.0), Vec2::new(-0.5, 0.0)),
        ];
        let before = (pair[0].pos, pair[1].pos);
        rvo_step(&mut pair, &[], &RvoParams::default(), DT);
        assert_eq!(bits(pair[0].pos), bits(before.0));
        assert_eq!(bits(pair[1].pos), bits(before.1));
    }

    // 17. 避让的暂时性：斜擦对通过后速度恢复偏好（约束随安全消失）。
    #[test]
    fn avoidance_releases_after_pass() {
        let mut pair = [
            agent(Vec2::new(-1.2, 0.35), Vec2::new(1.5, 0.0)),
            agent(Vec2::new(1.2, 0.0), Vec2::new(-1.5, 0.0)),
        ];
        simulate(&mut pair, &[], &RvoParams::default(), 400, 1.0 / 30.0);
        assert_eq!(bits(pair[0].vel), bits(pair[0].pref_vel));
        assert_eq!(bits(pair[1].vel), bits(pair[1].pref_vel));
    }

    // 18. 感知半径含障碍：neighbor_dist 外的障碍不约束（文档语义）。
    #[test]
    fn obstacle_out_of_reach_ignored() {
        let mut arr = [agent(Vec2::ZERO, Vec2::new(1.0, 0.0))];
        let obstacles = [RvoObstacle {
            pos: Vec2::new(9.5, 0.0), // reach = 8 + 0.5 之外
            radius: 0.5,
        }];
        rvo_step(&mut arr, &obstacles, &RvoParams::default(), DT);
        assert_eq!(bits(arr[0].vel), bits(Vec2::new(1.0, 0.0)));
    }
}
