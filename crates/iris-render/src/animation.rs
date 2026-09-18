//! glTF 动画运行时：剪辑采样、crossfade 混合、轻量状态机、事件轨道
//! 与每帧推进系统。
//!
//! glTF 动画模型：每条通道绑定「某节点 + 某属性（平移/旋转/缩放）」，
//! 按关键帧时间轴驱动。采样时被动画的属性**覆写**目标实体 `Transform` 的
//! 对应字段，未动画的字段保持当前值（即加载时的静止姿势）。
//!
//! v2 扩展（新字段全部带默认值，不启用时行为与 v1 完全一致）：
//! - **crossfade**：[`AnimationPlayer::transition`] 向目标剪辑平滑过渡，
//!   源姿势在过渡开始时冻结快照（[`AnimationBlend`]）；混合期「目标剪辑
//!   没有」的通道从冻结源值平滑淡回**基姿势**（rest pose，需先
//!   [`AnimationPlayer::capture_rest`] 捕获；未捕获时保持 v2 的
//!   「定格在源值」语义）——过渡完成后该通道恰停在基姿势，与「未被
//!   动画的字段保持当前值」的覆写语义自洽，不会出现完成瞬间的跳变；
//! - **状态机**：[`AnimationPlayer::states`] + [`AnimationPlayer::edges`]，
//!   [`AnimationPlayer::request_state`] 按「当前态 → 目标态」的边启动
//!   crossfade；**条件判定留给游戏侧**（无参数黑板，调用方自行决定何时
//!   request）；
//! - **事件轨道**：[`AnimationClip::events`] 命中后进入
//!   [`AnimationPlayer::pending_events`]，消费方每帧
//!   [`AnimationPlayer::take_events`] drain；跨界检测在纯函数
//!   [`collect_events`] 中（可脱离 ECS 单测）；
//! - **帧数据窗口 + 参数曲线轨（R74）**：[`AnimationClip::windows`]
//!   命名区间标记（半开 `[start, end)`，战斗判定的「攻击判定开 /
//!   可取消 / 无敌」区间语义载体，判定纯函数 [`window_active`]、
//!   播放器便捷入口 [`AnimationPlayer::window_active`]）；
//!   [`AnimationClip::curves`] 命名标量关键帧轨（线性插值、端点钳制，
//!   采样纯函数 [`sample_track`]），`update_animations` 每帧推进后
//!   重采样整表写入 [`AnimationPlayer::parameter_values`]（查询
//!   [`AnimationPlayer::parameter`]）。两者与事件同帧衔接：事件在
//!   `start` 触发、窗口从 `start` 生效（事件帧 = 窗口首帧）。
//!   GLB 解析均不产（不在 glTF 标准内），由 API 侧构造或经
//!   `clips_mut()` 注入。诚实限制：负 `speed`（倒放）不触发事件。
//!
//! 事件联动示范（音频脚步声；iris-render 不依赖 audio，运行时联动由
//! 玩法侧完成）：
//!
//! ```ignore
//! // 每帧在 update_animations 之后：
//! let mut player = world.get_mut::<AnimationPlayer>(root).unwrap();
//! for name in player.take_events() {
//!     if name == "footstep" {
//!         audio.play("step.wav"); // 对接 R25 音频闭环
//!     }
//! }
//! ```
//!
//! v3 扩展（新字段全部带中性默认值，不启用时行为与 v2 完全一致）：
//! - **BlendSpace2D 混合空间**：[`BlendSpace`]（任意分布的参数点集 →
//!   剪辑）+ [`AnimationPlayer::blend_param`]，[`AnimationPlayer::play_blendspace`]
//!   激活后 `update_animations` 每帧按参数点采样出 ≤ 4 路
//!   「剪辑 + 权重」，经 [`blend_pose`] 做 N 路姿势混合——把 v2 的两路
//!   crossfade 泛化为任意路数；采样算法为**逐轴线性分解**（先按 x 找
//!   左右列、列内再按 y 找前后行，权重 = 各轴插值系数乘积），权重非负、
//!   和恒为 1（单测守护）；
//! - **N 路旋转加权**：混合期旋转用「首个四元数为基准、点积 < 0 翻转后
//!   加权求和再归一」的 **nlerp**（[`blend_fields_nway`]），不用 slerp
//!   链式两两插值——slerp 不具结合性，N 路结果依赖配对顺序且误差链式
//!   累积；混合空间相邻剪辑夹角小，nlerp 的非线性不可感知（取舍见其文档）；
//! - **根运动**：[`AnimationPlayer::root_motion_clip`] + [`AnimationPlayer::root_motion_node`]
//!   声明「哪个剪辑的哪个节点带根位移」——该节点的 Translation 通道不再
//!   覆写 `Transform`，改为把本帧采样位移差（含循环回绕的三段精确累积，
//!   见 [`root_motion_delta`]）累积进 [`AnimationPlayer::pending_root_motion`]，
//!   消费方每帧 [`AnimationPlayer::take_root_motion`] 取走、旋转到角色
//!   yaw 系后交给角色控制器（消费契约见 [`root_motion_delta`] 文档）。
//!
//! v4 扩展（新字段全部带中性默认值，不启用时行为与 v3 完全一致）：
//! - **附加动画层**：[`AnimationPlayer::additive`] +
//!   [`AnimationPlayer::play_additive`]——在主姿势（active / blendspace /
//!   crossfade 任一现行模式）算完后叠加「剪辑相对其参考帧（t=0 采样值）
//!   的增量」（受击/呼吸类叠加语义）：平移/缩放加法
//!   `main + (v(t) − v(0)) × weight`，旋转按相对旋转复合
//!   `main × slerp(I, q(t)·q(0)⁻¹, weight)`（数学推导见
//!   [`additive_delta`]）；整层权重 [`AdditiveLayer::weight`] 压幅；
//! - **骨骼遮罩**：[`AnimationMask`] + [`AnimationPlayer::set_mask`]——
//!   覆写过滤器：主姿势采样产出的「目标实体 → 覆写字段」任务中，目标
//!   节点在遮罩集合内才覆写，集合外节点保持当前值（上半身瞄准层的
//!   基础——下半身保持移动姿势）。与 BlendSpace / crossfade / 状态机 /
//!   附加层**正交**（mask 只是最终的覆写过滤器，不参与任何姿势计算）；
//! - **手部两骨 IK**：`crate::hand_ik` 模块（[`crate::hand_ik::HandIk`]），
//!   复用足部的 [`crate::foot_ik::solve_two_bone_ik`] 纯函数与两阶段
//!   回写模式，落地查询换成「[`crate::hand_ik::IkTarget`] 实体的全局
//!   平移」（武器握把位）——与足部「射线打地面」的目标差异见其模块文档。
//!
//! v5 扩展（C4b 闭合战役；新字段全中性默认，不启用 = v4 行为逐位不变）：
//! - **状态机条件转移**：[`AnimationPlayer::params`] 参数黑板 +
//!   [`AnimationPlayer::transitions`] 条件转移表——[`AnimGuard`] 封闭
//!   枚举（浮点阈值 / 布尔 / [`AnimGuard::ClipFinished`] 播放末端 /
//!   And·Or·Not 组合），[`AnimationPlayer::step_state_machine`] 由
//!   [`update_animations`] 写回阶段每帧步检（声明序首条命中、一帧至多
//!   一条、空表恒 no-op）——「何时切换由游戏侧判定」的 v1 边界就此
//!   翻转为「判定可入引擎」，v1 `edges` + `request_state` 原样并存；
//! - **运行时重定向**：[`AnimationPlayer::retarget_by_name`] 按归一
//!   骨名（[`normalize_bone_name`]）把 `node_entities` 重绑到另一具
//!   骨架的实体表——通道与下标不动（源骨架空间语义保留），未命中断绑、
//!   无名保留（引擎不猜语义改名，alias 表归调用方）。

use std::collections::{BTreeMap, BTreeSet};

use glam::{Quat, Vec2, Vec3};
use iris_core::prelude::{Entity, Time, Transform};

/// 动画通道作用的节点属性。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationPath {
    Translation,
    Rotation,
    Scale,
}

/// 关键帧插值方式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnimationInterpolation {
    /// 线性插值；旋转用最短路径 slerp。
    #[default]
    Linear,
    /// 阶跃：保持上一关键帧值直到下一关键帧时刻。
    Step,
}

/// 单条动画通道：某节点的某属性随时间的关键帧序列。
#[derive(Clone, Debug)]
pub struct AnimationChannel {
    /// 目标节点索引（对应 [`crate::asset::GltfInstance::node_entities`] 下标）。
    pub node: usize,
    pub path: AnimationPath,
    pub interpolation: AnimationInterpolation,
    /// 关键帧时间（秒，非递减）。
    pub times: Vec<f32>,
    /// 平铺关键帧值：平移/缩放每帧 3 分量，旋转每帧 4 分量 (x, y, z, w)。
    pub values: Vec<f32>,
}

impl AnimationChannel {
    fn components(&self) -> usize {
        match self.path {
            AnimationPath::Rotation => 4,
            _ => 3,
        }
    }

    /// 把 t 时刻的插值结果写入 out（长度须为分量数）。
    fn sample_into(&self, t: f32, out: &mut [f32]) {
        let comps = self.components();
        let keys = self.times.len();
        if keys == 0 {
            return;
        }
        let last = keys - 1;
        let copy_key = |k: usize, out: &mut [f32]| {
            out[..comps].copy_from_slice(&self.values[k * comps..k * comps + comps]);
        };
        if keys == 1 || t <= self.times[0] {
            copy_key(0, out);
            return;
        }
        if t >= self.times[last] {
            copy_key(last, out);
            return;
        }
        // 段查找：i 满足 times[i] <= t < times[i+1]
        let i = self
            .times
            .partition_point(|&tt| tt <= t)
            .saturating_sub(1)
            .min(last - 1);
        if self.interpolation == AnimationInterpolation::Step {
            copy_key(i, out);
            return;
        }
        let span = (self.times[i + 1] - self.times[i]).max(1e-9);
        let f = (t - self.times[i]) / span;
        // 旋转必须 slerp（分量线性插值会破坏归一化且路径错误）；
        // glTF 规范要求点积为负时翻转短路径
        if self.path == AnimationPath::Rotation {
            let qa = Quat::from_slice(&self.values[i * 4..i * 4 + 4]);
            let mut qb = Quat::from_slice(&self.values[(i + 1) * 4..(i + 1) * 4 + 4]);
            if qa.dot(qb) < 0.0 {
                qb = -qb;
            }
            let q = qa.slerp(qb, f);
            out[..4].copy_from_slice(&[q.x, q.y, q.z, q.w]);
            return;
        }
        // comps ≤ out.len()（各属性分量数 3 / 4 / 4）：只覆写前 comps 个槽
        for (c, slot) in out.iter_mut().enumerate().take(comps) {
            let a = self.values[i * comps + c];
            let b = self.values[(i + 1) * comps + c];
            *slot = a + (b - a) * f;
        }
    }

    /// t 时刻的属性值（覆写到目标实体 `Transform` 用）。
    pub fn sample(&self, t: f32) -> AnimatedField {
        let mut v = [0.0f32; 4];
        self.sample_into(t, &mut v);
        match self.path {
            AnimationPath::Translation => AnimatedField::Translation([v[0], v[1], v[2]]),
            AnimationPath::Scale => AnimatedField::Scale([v[0], v[1], v[2]]),
            AnimationPath::Rotation => {
                let a = Quat::from_xyzw(v[0], v[1], v[2], v[3]);
                AnimatedField::Rotation(a)
            }
        }
    }
}

/// 采样结果：要覆写到目标实体 `Transform` 的单个字段。
#[derive(Clone, Copy, Debug)]
pub enum AnimatedField {
    Translation([f32; 3]),
    Rotation(Quat),
    Scale([f32; 3]),
}

/// 动画事件：剪辑时间轴上的命名标记（如「footstep」「hit_frame」）。
///
/// 事件不在 glTF 标准内，**GLB 解析不产事件**——由 API 侧构造
/// （`clip.events.push(AnimationEvent { time, name })`），`time` 建议落在
/// `[0, duration]` 内。命中判定见 [`collect_events`]。
#[derive(Clone, Debug, PartialEq)]
pub struct AnimationEvent {
    /// 触发时刻（秒，相对剪辑起点）。
    pub time: f32,
    /// 事件名（消费方按名分发）。
    pub name: String,
}

/// 帧数据窗口（R74）：剪辑时间轴上的命名**区间**标记——战斗判定的
/// 「攻击判定开 / 可取消 / 无敌」等区间语义的载体（事件是离散点、
/// 窗口是区间；两者边界衔接：事件在 `start` 时刻触发、窗口从 `start`
/// 生效——同一帧既收到事件又处于窗口首帧）。GLB 解析不产窗口（同
/// 事件先例），由 API 侧构造或经 [`AnimationPlayer::clips_mut`] 注入。
#[derive(Clone, Debug, PartialEq)]
pub struct ClipWindow {
    /// 窗口名（消费方按名查询，如 "active_frames" / "cancel_window"）。
    pub name: String,
    /// 起点（秒，**含**）。
    pub start: f32,
    /// 终点（秒，**不含**——半开区间 `[start, end)`）。
    pub end: f32,
}

/// 参数曲线轨（R74）：剪辑时间轴上的命名**标量**轨道（关键帧线性
/// 插值、端点钳制）——伤害随命中帧衰减、镜头 FOV / 受击权重等游戏侧
/// 数值的时间演化载体。GLB 解析不产曲线（同事件先例）；采样见
/// [`sample_track`]，播放器每帧采样见
/// [`AnimationPlayer::parameter_values`]。
#[derive(Clone, Debug, PartialEq)]
pub struct ParameterTrack {
    /// 轨道名（消费方按名查询）。
    pub name: String,
    /// 关键帧表 `(时间, 值)`；[`ParameterTrack::new`] 构造时按时间升序
    /// 排序（稳定排序 ⇒ 并列时间保持输入先者，确定性）。
    pub keys: Vec<(f32, f32)>,
}

impl ParameterTrack {
    /// 构造：按时间升序排序（稳定排序 ⇒ 并列时间保持输入先者）。
    pub fn new(name: impl Into<String>, mut keys: Vec<(f32, f32)>) -> Self {
        keys.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        Self {
            name: name.into(),
            keys,
        }
    }
}

/// 参数曲线采样（纯函数）：线性插值、端点钳制。
///
/// 确定性纪律：区间括号取「t ≤ key 时间的最大者」（扫描首遇即返回），
/// 相邻两 key 时间相等时返回**前者**的值（构造排序保证并列相邻，防
/// 除零且并列行为确定）；非有限 `t` 返回 0.0（与 [`BlendSpace::sample`]
/// 的「无效参数 → 中性值」先例一致）。
pub fn sample_track(keys: &[(f32, f32)], t: f32) -> f32 {
    if !t.is_finite() {
        return 0.0;
    }
    match keys.len() {
        0 => 0.0,
        1 => keys[0].1,
        _ => {
            if t <= keys[0].0 {
                return keys[0].1;
            }
            if t >= keys[keys.len() - 1].0 {
                return keys[keys.len() - 1].1;
            }
            for w in keys.windows(2) {
                let (t0, v0) = w[0];
                let (t1, v1) = w[1];
                if t <= t1 {
                    if t1 > t0 {
                        let a = (t - t0) / (t1 - t0);
                        return v0 + (v1 - v0) * a;
                    }
                    return v0;
                }
            }
            unreachable!("t < 末 key 时间 ⇒ 扫描必命中括住区间")
        }
    }
}

/// 帧数据窗口判定（纯函数）：播放时间 `time` 是否落在任一同名窗口内。
///
/// 半开区间 `[start, end)`；`start >= end` 的畸形窗口恒不命中（区间为
/// 空）；无同名窗口 = `false`（**未声明即不 active**，与组件缺席语义
/// 一致）；非有限 `time` 恒 `false`。循环播放的 `time` 为取模后的
/// 播放时间（天然落在 `[0, duration)`）。
pub fn window_active(windows: &[ClipWindow], time: f32, name: &str) -> bool {
    if !time.is_finite() {
        return false;
    }
    windows
        .iter()
        .any(|w| w.name == name && w.start <= time && time < w.end)
}

/// 一段动画剪辑：若干通道的集合，时长取各通道最后关键帧时间的最大值。
#[derive(Clone, Debug, Default)]
pub struct AnimationClip {
    pub name: String,
    pub channels: Vec<AnimationChannel>,
    /// 剪辑时长（秒）；无通道时为 0。
    pub duration: f32,
    /// 事件轨道（时间任意序均可，输出恒按触发时刻排序）；GLB 解析恒为空。
    pub events: Vec<AnimationEvent>,
    /// 帧数据窗口（R74，区间语义见 [`ClipWindow`]）；GLB 解析恒为空。
    pub windows: Vec<ClipWindow>,
    /// 参数曲线轨（R74，见 [`ParameterTrack`]）；GLB 解析恒为空。
    pub curves: Vec<ParameterTrack>,
}

/// 2D 混合空间（v3）：任意分布的参数点集 → 剪辑索引的映射，第三人称
/// 移动循环（idle / walk / run / strafe …）按「速度 × 转向」连续混合的
/// 标准载体。
///
/// # 采样算法（[`BlendSpace::sample`]，确定性）
///
/// **逐轴线性分解**——先按 x 把样本点分列（x 精确相等的样本同列），找出
/// 括住参数点的左右两列；再在每列内按 y 找括住参数点的前后两行。参数点
/// 的 x 插值系数决定两列的列权重（单列 = 全权重落该列），列权重再按列内
/// y 插值系数分给前后两行——权重 = 各轴插值系数乘积，恰为轴对齐双线性
/// 插值，最多 4 个样本获得非零权重。点集任意分布时列退化（单样本列）自动
/// 退化为「沿该列钳制」，所有样本同 y 时即 1D 链式 lerp、所有样本同 x 时
/// 为 1D 纵向 lerp，无需特判。确定性保证：轴扫描方向固定（x 先、y 后；
/// 括号取「≤ 参数一侧的最大者 / ≥ 参数一侧的最小者」）、并列取下标小者、
/// 输出按「左列前行 → 左列后行 → 右列前行 → 右列后行」固定顺序、恰为零
/// 的权重项不输出。参数点恰好等于某样本点 → 该点权重 1（精确相等）。
///
/// 权重不变量：非空点集下权重全部非负、和恒为 1（单测随机参数遍历守护）；
/// 剪辑索引可重复（同一剪辑可出现在多个参数点），输出按样本逐项给出。
#[derive(Clone, Debug, PartialEq)]
pub struct BlendSpace {
    /// 样本点集：`(参数点, 剪辑索引)`；点集任意分布，空集采样返回空表。
    pub samples: Vec<(Vec2, usize)>,
}

/// 列内 y 括号：把列权重 `column_weight` 按 `param_y` 分给列内前后两行
/// 样本（[`BlendSpace::sample`] 的内层步骤，见其算法文档）。
fn column_y_weights(
    samples: &[(Vec2, usize)],
    column: &[usize],
    param_y: f32,
    column_weight: f32,
    out: &mut Vec<(usize, f32)>,
) {
    // 前行 = y ≤ param_y 的最大者；后行 = y ≥ param_y 的最小者；
    // 严格比较才替换 ⇒ 并列保留先扫描者（下标小者，确定性）。
    // 非有限 param_y 已在入口拒绝 ⇒ 每个 y 至少满足一侧比较，
    // down / up 不会同时为 None；参数落在列外时缺侧 ⇒ 钳到存在侧
    let mut down: Option<usize> = None;
    let mut up: Option<usize> = None;
    for &i in column {
        let y = samples[i].0.y;
        if y <= param_y && down.is_none_or(|d| y > samples[d].0.y) {
            down = Some(i);
        }
        if y >= param_y && up.is_none_or(|u| y < samples[u].0.y) {
            up = Some(i);
        }
    }
    match (down, up) {
        (Some(d), Some(u)) => {
            if d == u {
                // 单行命中（参数 y 恰在该行上 / 列内只有一行）：全权重
                out.push((samples[d].1, column_weight));
                return;
            }
            let ty =
                ((param_y - samples[d].0.y) / (samples[u].0.y - samples[d].0.y)).clamp(0.0, 1.0);
            if 1.0 - ty > 0.0 {
                out.push((samples[d].1, column_weight * (1.0 - ty)));
            }
            if ty > 0.0 {
                out.push((samples[u].1, column_weight * ty));
            }
        }
        // 参数 y 在列 y 范围外：钳到唯一存在的一侧（全权重）
        (Some(d), None) => out.push((samples[d].1, column_weight)),
        (None, Some(u)) => out.push((samples[u].1, column_weight)),
        (None, None) => unreachable!("非空列至少有一侧括住有限参数"),
    }
}

impl BlendSpace {
    /// 采样：返回 ≤ 4 个 `(剪辑索引, 权重)`——权重非负、和恒为 1
    /// （空点集或非有限参数返回空表）；算法与确定性见 [`BlendSpace`]。
    pub fn sample(&self, param: Vec2) -> Vec<(usize, f32)> {
        if self.samples.is_empty() || !param.x.is_finite() || !param.y.is_finite() {
            return Vec::new();
        }
        // 精确命中：参数点等于某样本点 → 该点权重 1
        if let Some(&(_, clip)) = self.samples.iter().find(|(p, _)| *p == param) {
            return vec![(clip, 1.0)];
        }
        // 按 x 精确值分列（保持首遇顺序；列 x 互不相同）
        let mut columns: Vec<(f32, Vec<usize>)> = Vec::new();
        for (i, (point, _)) in self.samples.iter().enumerate() {
            match columns.iter_mut().find(|(x, _)| *x == point.x) {
                Some((_, list)) => list.push(i),
                None => columns.push((point.x, vec![i])),
            }
        }
        // x 括号：左列 = x ≤ param.x 的最大者；右列 = x ≥ param.x 的
        // 最小者；严格比较 ⇒ 并列（不可能，列 x 唯一）天然无歧义
        let mut left: Option<(f32, &Vec<usize>)> = None;
        let mut right: Option<(f32, &Vec<usize>)> = None;
        for (x, list) in &columns {
            if *x <= param.x && left.is_none_or(|(lx, _)| *x > lx) {
                left = Some((*x, list));
            }
            if *x >= param.x && right.is_none_or(|(rx, _)| *x < rx) {
                right = Some((*x, list));
            }
        }
        let mut out = Vec::with_capacity(4);
        match (left, right) {
            (Some((lx, l)), Some((rx, r))) if lx < rx => {
                // 两列括住：列权重 = 各轴插值系数乘积的 x 因子
                let tx = ((param.x - lx) / (rx - lx)).clamp(0.0, 1.0);
                if 1.0 - tx > 0.0 {
                    column_y_weights(&self.samples, l, param.y, 1.0 - tx, &mut out);
                }
                if tx > 0.0 {
                    column_y_weights(&self.samples, r, param.y, tx, &mut out);
                }
            }
            // 单列命中（参数 x 落在唯一列上 / 点集单列）：全权重落该列
            (Some((_, l)), _) => column_y_weights(&self.samples, l, param.y, 1.0, &mut out),
            (None, Some((_, r))) => column_y_weights(&self.samples, r, param.y, 1.0, &mut out),
            (None, None) => unreachable!("非空列集至少有一侧括住有限参数"),
        }
        out
    }
}

/// 一次 crossfade 过渡的状态：目标剪辑为 `player.active`（正常推进），
/// 源姿势在过渡开始时**冻结快照**为 `(from_clip, from_time)`。
///
/// 源不再推进的选择理由：混合结果只由 `elapsed` 决定——可预测、可测试
/// （同样输入恒得同样姿势），且避免源剪辑循环回绕或播完造成的姿势跳变；
/// 代价是长过渡里源姿态定格，对 0.1~0.5s 量级的过渡不可感知。
///
/// 「目标剪辑没有的通道」（仅源有）在混合期不是恒取源值：已捕获基姿势
/// （[`AnimationPlayer::rest`] 非空）时按混合权重 `w` 从源值平滑淡回
/// 基姿势——过渡完成时（`w` = 1）恰为 rest 值，`blend` 清除后该通道
/// 不再被任何剪辑覆写、Transform 停在 rest，与「未被动画的字段保持
/// 当前值」的覆写语义自洽；rest 未捕获时保持 v2 语义（恒取源值，过渡
/// 结束后定格在最后的源值——纯构造播放器的既有行为不变）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnimationBlend {
    /// 过渡开始时的源剪辑索引。
    pub from_clip: usize,
    /// 过渡开始时冻结的源姿势快照（源时间不再推进）。
    pub from_time: f32,
    /// 过渡总时长（秒）。
    pub duration: f32,
    /// 已流逝的过渡时长（秒）。
    pub elapsed: f32,
}

/// 状态机的一个状态：绑定剪辑与播放参数。
#[derive(Clone, Debug, PartialEq)]
pub struct AnimationState {
    /// 该状态播放的剪辑索引。
    pub clip: usize,
    /// 该状态是否循环（进入状态时同步到播放器）。
    pub looping: bool,
    /// 该状态的播放速度倍率（进入状态时同步到播放器）。
    pub speed: f32,
}

/// 状态机的一条边：`from → to` 的过渡时长。
///
/// v1 边界：边只带「过渡时长」，不带条件表达式——**何时切换由游戏侧
/// 判定**（调用方按玩法逻辑调用 [`AnimationPlayer::request_state`]，
/// 引擎不设参数黑板 / 条件求值）。
///
/// **v2（C4b）**：条件转移走并行的 [`AnimationPlayer::transitions`]
/// （[`AnimTransition`]），本类型原样保留——v1 手动 `request_state`
/// 路径与全部既有构造零改动。二者可共存（同一 `states` 表）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnimationEdge {
    pub from: usize,
    pub to: usize,
    /// 过渡时长（秒）；0 = 硬切换。
    pub duration: f32,
}

/// 参数黑板的取值（C4b · 状态机 v2）：浮点（速度 / 朝向角这类连续量）
/// 与布尔（着地 / 受击这类标志）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AnimValue {
    Float(f32),
    Bool(bool),
}

/// 条件转移的守卫（C4b）：对 [`AnimationPlayer::params`] 黑板的纯读求值。
///
/// 刻意做成**封闭枚举而非闭包**——可序列化、可 `PartialEq`、求值确定
/// （无捕获环境），契合本仓确定性文化。缺失参数按类型取默认
/// （浮点 0.0 / 布尔 false），与「组件缺席 = 中性值」一致。
#[derive(Clone, Debug, PartialEq)]
pub enum AnimGuard {
    /// 恒真（无条件转移；「播完即走」请配 [`AnimGuard::ClipFinished`]）。
    Always,
    /// 浮点参数 `key >= n`。
    FloatAtLeast(String, f32),
    /// 浮点参数 `key < n`。
    FloatBelow(String, f32),
    /// 布尔参数 `key == v`。
    BoolIs(String, bool),
    /// 浮点参数落在 `[lo, hi]` 闭区间（混合空间入口的粗粒度门）。
    FloatBetween(String, f32, f32),
    /// **播放器侧守卫**（不读黑板）：当前状态的剪辑为非循环且已播到
    /// 末端（`time ≥ duration`，`duration > 0`）——attack → idle 这类
    /// 回边的最常见形态；循环状态恒假。
    ClipFinished,
    /// 全部子守卫为真（空 `And` = 恒真，合逻辑单位元）。
    All(Vec<AnimGuard>),
    /// 任一子守卫为真（空 `Any` = 恒假，合逻辑单位元）。
    Any(Vec<AnimGuard>),
    /// 子守卫取反。
    Not(Box<AnimGuard>),
}

impl AnimGuard {
    /// 对参数黑板求值（纯函数，`params` 缺失键按类型默认）。
    pub fn eval(&self, params: &BTreeMap<String, AnimValue>) -> bool {
        match self {
            AnimGuard::Always => true,
            AnimGuard::FloatAtLeast(key, n) => float_of(params, key) >= *n,
            AnimGuard::FloatBelow(key, n) => float_of(params, key) < *n,
            AnimGuard::BoolIs(key, v) => bool_of(params, key) == *v,
            AnimGuard::FloatBetween(key, lo, hi) => {
                let x = float_of(params, key);
                x >= *lo && x <= *hi
            }
            // 本函数只有黑板上下文、看不到播放时间 ⇒ 播放器侧叶子恒假。
            // 正常步检走 `AnimationPlayer::guard_holds`（两层上下文合流
            // 的求值口），直接调本方法判含 ClipFinished 的守卫是误用。
            AnimGuard::ClipFinished => false,
            AnimGuard::All(inner) => inner.iter().all(|g| g.eval(params)),
            AnimGuard::Any(inner) => inner.iter().any(|g| g.eval(params)),
            AnimGuard::Not(inner) => !inner.eval(params),
        }
    }
}

/// 取浮点参数（缺失 = 0.0；类型非浮点亦按 0.0——布尔当 0 处理不报错，
/// 与「中性默认」口径一致）。
fn float_of(params: &BTreeMap<String, AnimValue>, key: &str) -> f32 {
    match params.get(key) {
        Some(AnimValue::Float(v)) => *v,
        _ => 0.0,
    }
}

/// 取布尔参数（缺失 / 非布尔 = false）。
fn bool_of(params: &BTreeMap<String, AnimValue>, key: &str) -> bool {
    matches!(params.get(key), Some(AnimValue::Bool(v)) if *v)
}

/// 一条条件转移（C4b · 状态机 v2）：`from → to` 且守卫成立即过渡。
///
/// [`AnimationPlayer::step_state_machine`] 每帧按 `transitions` 声明序
/// （`Vec` 序确定）取**首条** `from == current_state` 且守卫成立的转移
/// 触发，**一帧最多一条**（防乒乓 / 级联，多条件同帧满足靠声明序裁决）。
#[derive(Clone, Debug, PartialEq)]
pub struct AnimTransition {
    pub from: usize,
    pub to: usize,
    /// 过渡时长（秒）；0 = 硬切换（与 [`AnimationEdge::duration`] 同义）。
    pub duration: f32,
    /// 触发守卫。
    pub guard: AnimGuard,
}

/// [`AnimationPlayer::retarget_by_name`] 的回报（C4b · 运行时重定向）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetargetReport {
    /// 成功重绑到目标骨架实体表的节点数。
    pub bound: usize,
    /// 未命中（目标骨架无同名骨 / 源无名）而保留原实体（通常 None）的节点数。
    pub unmatched: usize,
}

/// 附加动画层（v4）：在主姿势之上叠加「剪辑相对其参考帧的增量」——
/// 受击抖动、呼吸起伏这类**叠加语义**的载体（增量叠在移动循环上，
/// 不破坏步态；与 crossfade 的「两路姿势插值」语义正交）。
///
/// 参考帧 = 剪辑 `t = 0` 的采样值（构造时由播放器声明、每帧重采样）：
/// 增量 `delta = sample(t) − sample(0)`，剪辑静止段（t 处值 = 参考值）
/// 的增量恰为零——附加层激活但不产生偏移，语义自洽。整层受
/// `weight`（[0, 1]）压幅。推进语义与主时间轴对称：`playing = false`
/// 整层冻结、循环按剪辑时长取模、非循环钳制在末端（停在末端后增量
/// 恒定，不重复推进）。
///
/// 组合语义（与各姿势来源正交）：主姿势（active / blendspace /
/// crossfade 混合结果）算完后叠加，**只在主姿势已覆写的通道上生效**
/// （主姿势未覆写的通道跳过——以当前 `Transform` 为基会让增量逐帧
/// 复合漂移，诚实边界见 `apply_additive`）；根运动启用时根节点的
/// 附加平移增量同样被剔除（根位移是玩法量，见 `update_animations`）；
/// 遮罩过滤发生在叠加之后——被遮罩节点的附加覆写同样被剔除。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdditiveLayer {
    /// 附加剪辑索引（播放器 `clips` 下标）。
    pub clip: usize,
    /// 附加层播放时间（秒，独立于主时间轴推进）。
    pub time: f32,
    /// 附加层播放速度倍率。
    pub speed: f32,
    /// 整层权重（[0, 1]）：0 = 恒等（无偏移），1 = 全幅增量。
    pub weight: f32,
    /// 到达剪辑末尾后是否循环。
    pub looping: bool,
}

/// 节点遮罩（v4）：被遮罩节点集合的姿势覆写只作用于集合内——
/// **覆写过滤器**，不参与姿势计算。上半身瞄准层的载体：瞄准剪辑
/// 只覆写脊柱/手臂节点（集合内），腿部节点保持移动循环的姿势
/// （集合外），两层各自动画互不打架。
///
/// 组合语义（正交性承诺）：mask 与 BlendSpace / crossfade / 状态机 /
/// 附加层全部正交——它只是 `update_animations` 产出「目标实体 →
/// 覆写字段」任务后的**最后一道过滤器**：目标节点在 [`AnimationMask::nodes`]
/// 内才允许覆写，集合外节点本帧不写任何变换（`Transform` 保持当前值，
/// 通常为加载时的 rest 或其它系统的写入）。`mask = None`（默认）=
/// 全覆写照旧，v1 行为零变化。
///
/// 确定性：`BTreeSet` 固定迭代序（确定性是硬文化——需要迭代序的
/// 数据结构不用哈希容器）；过滤本身按「实体 → 节点下标」反查
/// `node_entities`，与集合迭代序无关，选 BTreeSet 是让序列化 /
/// 调试输出同样可复现。
#[derive(Clone, Debug, PartialEq, Default)]
pub struct AnimationMask {
    /// 允许覆写的节点下标集合（`node_entities` 下标）。
    pub nodes: BTreeSet<usize>,
}

/// 动画播放器组件：挂在任意实体上（通常是 glTF 根实体），
/// 持有剪辑与「节点索引 → 实体」映射，由 [`update_animations`] 每帧推进。
///
/// `clips` 为 `Arc<Vec<AnimationClip>>`（R59）：同一 GLB 的多实例经
/// 动画注册表（`crate::asset::AnimationRegistry`）spawn 时共享同一份
/// 剪辑数据（引用计数 +1，零拷贝）——「每实例深拷贝一份关键帧」的
/// 内存重复归零。生命周期 = Arc 语义：注册表与全部播放器都丢弃后数据
/// 才释放，无悬垂风险；克隆播放器（`Clone`）同样只复制引用。直接构造
/// （不经注册表）的播放器持有独占的 `Arc`（强计数 1），对外行为与
/// 过去的按值 `Vec` 语义等价（读路径全部经 Deref 透传）。
#[derive(Clone, Debug, iris_core::Component)]
pub struct AnimationPlayer {
    pub clips: std::sync::Arc<Vec<AnimationClip>>,
    /// 节点索引 → 目标实体；未在 active scene 中的节点为 None。
    pub node_entities: Vec<Option<Entity>>,
    /// 节点索引 → glTF 节点名（与 [`Self::node_entities`] **同下标对齐**，
    /// 缺席 = 该节点无名）。打磨 VI 摩擦⑥：骨架装配（IK 链、挂点、材质
    /// 替换）此前只能按裸下标寻骨，「资产生成器 ↔ 骨架表 ↔ 插件层」三方
    /// 下标契约全靠守护测试兜底——本字段把「按名查实体」变成引擎原语
    /// （[`Self::node_entity_by_name`]）。
    ///
    /// 由 [`crate::asset::GltfInstance::player`] 从 GLB 节点名填入；
    /// 手写剪辑构造的播放器可留空表（按名查找恒 None）。**名称唯一性由
    /// 资产侧保证**（glTF 规范不强制节点名唯一）：重名时取先出现的节点
    /// （向量序确定 ⇒ 结果确定）。
    pub node_names: Vec<Option<String>>,
    /// 当前播放的剪辑索引。
    pub active: usize,
    /// 当前播放时间（秒）。
    pub time: f32,
    /// 播放速度倍率（1 = 原速）；states 非空时以当前状态的 speed 为准。
    pub speed: f32,
    pub playing: bool,
    /// 到达剪辑末尾后是否循环；states 非空时以当前状态的 looping 为准。
    pub looping: bool,
    /// 进行中的 crossfade（None = 无过渡，纯 active 剪辑姿势）。
    pub blend: Option<AnimationBlend>,
    /// 状态机状态表；**空 = 不用状态机**——直接操作 active/looping/speed
    /// 的旧路径完全保留。
    pub states: Vec<AnimationState>,
    /// 状态机边表（`from → to` + 过渡时长）。
    pub edges: Vec<AnimationEdge>,
    /// 当前状态下标（states 非空时有效）。
    pub current_state: usize,
    /// 本帧命中待消费的事件名（按触发时刻升序）；**消费方每帧
    /// [`AnimationPlayer::take_events`] drain**——不消费会跨帧累积。
    pub pending_events: Vec<String>,
    /// 基姿势（rest pose）快照：「目标实体 → 平移 / 旋转 / 缩放三通道的
    /// 静止值」——[`AnimationPlayer::capture_rest`] 捕获（实体生成之后、
    /// 首次动画推进之前调用一次）。**空表 = 未捕获**：crossfade 的
    /// 「仅源有」通道保持 v2 定格语义；捕获后混合期平滑淡回基姿势
    /// （语义见 [`AnimationBlend`]）。默认空，兼容性承诺与 v1/v2 一致。
    pub rest: Vec<(Entity, AnimatedField)>,
    /// 混合空间（v3）：与 [`AnimationPlayer::blend_param`] 配合，
    /// [`AnimationPlayer::play_blendspace`] 激活后 `update_animations`
    /// 每帧按参数点采样出 ≤ 4 路混合输入（所有混合剪辑**共享播放器
    /// 时间轴**——步态循环同长的常规用法；相位偏移需求直接给不同
    /// 剪辑做首帧偏移，v1 不做逐剪辑时间）。默认 None = 不使用。
    pub blend_space: Option<BlendSpace>,
    /// 混合空间当前参数点（游戏侧每帧写入，如「水平速度 × 转向」）；
    /// 激活模式下每帧采样一次。默认原点。
    pub blend_param: Vec2,
    /// 混合空间激活标志：[`AnimationPlayer::play_blendspace`] 置位，
    /// [`AnimationPlayer::transition`]（含状态机路径）清位——**与
    /// crossfade / 状态机互斥**：激活时 `active` 剪辑的通道采样停用
    /// （不再覆写任何 Transform，姿势完全来自混合空间采样）。标志为真
    /// 而 `blend_space` 为 None 的无效组合按「无姿势来源」处理（不写）。
    pub blend_space_active: bool,
    /// 根运动源剪辑（v3，Some = 声明该剪辑带根位移）：与 `active` 相同
    /// 且 [`AnimationPlayer::root_motion_node`] 有效时，该节点的
    /// Translation 通道不再覆写 Transform，改累积帧增量到
    /// [`AnimationPlayer::pending_root_motion`]。
    ///
    /// 与规范的偏离说明：规范原文把 root_motion 标志放在 `AnimationClip`
    /// 上，但新增剪辑字段会给既有测试的全部 `AnimationClip` 结构体字面量
    /// 引入编译破坏（硬约束「既有动画测试零改动」），故把激活开关移到
    /// 播放器侧按索引声明——语义等价（同一播放器的剪辑集合固定）。
    pub root_motion_clip: Option<usize>,
    /// 根运动作用节点（`node_entities` 下标）；None = 根运动完全关闭
    /// （任何剪辑的平移照常覆写）。默认 None。
    pub root_motion_node: Option<usize>,
    /// 本帧根运动位移增量（剪辑空间，米）；每帧由 `update_animations`
    /// 累积写入，**消费方 [`AnimationPlayer::take_root_motion`] take**
    /// ——不消费会跨帧累积（照 `pending_events` 的帧内消息模式）。
    pub pending_root_motion: Option<Vec3>,
    /// 附加动画层（v4）：主姿势算完后叠加增量（语义与组合规则见
    /// [`AdditiveLayer`]）。默认 None = 不使用。
    pub additive: Option<AdditiveLayer>,
    /// 节点遮罩（v4）：覆写过滤器，集合外节点本帧不覆写（语义与
    /// 正交性见 [`AnimationMask`]）。默认 None = 全覆写照旧。
    pub mask: Option<AnimationMask>,
    /// 当前帧的参数曲线值（R74）：active 剪辑的全部
    /// [`ParameterTrack`] 由 `update_animations` 每帧推进后**重采样
    /// 整表覆写**（非消息流，无 drain 语义——查询用
    /// [`Self::parameter`]）。未推进 / 无曲线 = 空。
    pub parameter_values: Vec<(String, f32)>,
    /// 状态机参数黑板（C4b · v2）：游戏侧经 [`Self::set_float`] /
    /// [`Self::set_bool`] 写入，`update_animations` 每帧经
    /// [`Self::step_state_machine`] 消费求值条件转移。
    /// **空表 + 空 `transitions` = v1 行为逐位不变**（步检恒无触发）。
    /// `BTreeMap` 固定序（序列化 / 调试输出可复现，同
    /// [`AnimationMask`] 的选型理由）。
    pub params: BTreeMap<String, AnimValue>,
    /// 条件转移表（C4b · v2）：与 v1 `edges` 并存（同一 `states` 表），
    /// 语义与触发纪律见 [`AnimTransition`] 与 [`Self::step_state_machine`]。
    /// **空表 = 自动转移关闭**（缺席 = v1 逐位不变）。
    pub transitions: Vec<AnimTransition>,
}

impl Default for AnimationPlayer {
    /// 全字段的默认值（兼容性承诺：新增字段一律给中性默认——
    /// blend/states/edges/pending_events/根运动/混合空间默认空/None/未激活，
    /// 行为与 v1 一致）。
    fn default() -> Self {
        Self {
            clips: std::sync::Arc::new(Vec::new()),
            node_entities: Vec::new(),
            node_names: Vec::new(),
            active: 0,
            time: 0.0,
            speed: 1.0,
            playing: true,
            looping: true,
            blend: None,
            states: Vec::new(),
            edges: Vec::new(),
            current_state: 0,
            pending_events: Vec::new(),
            rest: Vec::new(),
            blend_space: None,
            blend_param: Vec2::ZERO,
            blend_space_active: false,
            root_motion_clip: None,
            root_motion_node: None,
            pending_root_motion: None,
            additive: None,
            mask: None,
            parameter_values: Vec::new(),
            params: BTreeMap::new(),
            transitions: Vec::new(),
        }
    }
}

impl AnimationPlayer {
    /// 默认播放器：循环播放第一个剪辑，原速，自动开始。
    ///
    /// `clips` 经 `Arc` 包装持有（R59 共享语义，见结构体文档）；
    /// 已持有 `Arc` 的调用方（注册表 / 共享 spawn 路径）请用
    /// [`Self::new_shared`] 避免二次包装。
    pub fn new(clips: Vec<AnimationClip>, node_entities: Vec<Option<Entity>>) -> Self {
        Self::new_shared(std::sync::Arc::new(clips), node_entities)
    }

    /// 同 [`Self::new`]，但直接接管剪辑数据的 `Arc`（共享构造口）：
    /// 注册表命中路径用它在多实例间共享同一份剪辑数据。
    pub fn new_shared(
        clips: std::sync::Arc<Vec<AnimationClip>>,
        node_entities: Vec<Option<Entity>>,
    ) -> Self {
        Self {
            clips,
            node_entities,
            ..Default::default()
        }
    }

    /// 剪辑数据的可写访问（R59，写时复制语义）：独占持有者就地修改
    /// 零开销；共享自注册表的播放器首次调用会经 `Arc::make_mut` 克隆
    /// 出**本实例私有副本**，修改不影响注册表与其他实例。GLB 解析不产
    /// 事件轨道，注入 `clip.events`（脚步声等 API 侧事件）应走此口。
    pub fn clips_mut(&mut self) -> &mut Vec<AnimationClip> {
        std::sync::Arc::make_mut(&mut self.clips)
    }

    /// 按 glTF **节点名**查映射实体（打磨 VI 摩擦⑥）：骨架装配（IK 链 /
    /// 武器挂点 / 材质替换寻骨）不再依赖裸下标契约。语义：
    /// - 命中首个同名节点（[`Self::node_names`] 向量序确定 ⇒ 重名时
    ///   结果确定；名称唯一性由资产侧保证）；
    /// - 该名存在但节点不在 active scene（实体为 None）、或本播放器是
    ///   手写构造（`node_names` 空表）→ [`None`]；
    /// - 与下标寻址等价可互换：命中项恒等于
    ///   `node_entities[首个匹配下标]`（单测锁定）。
    pub fn node_entity_by_name(&self, name: &str) -> Option<Entity> {
        self.node_names
            .iter()
            .position(|slot| slot.as_deref() == Some(name))
            .and_then(|index| self.node_entities.get(index).copied().flatten())
    }

    /// 生效的循环标记：挂了状态机（states 非空且下标合法）时以当前状态
    /// 为准，否则回落到组件字段（旧路径）。
    fn effective_looping(&self) -> bool {
        match self.states.get(self.current_state) {
            Some(state) => state.looping,
            None => self.looping,
        }
    }

    /// 生效的播放速度：规则同 [`AnimationPlayer::effective_looping`]。
    fn effective_speed(&self) -> f32 {
        match self.states.get(self.current_state) {
            Some(state) => state.speed,
            None => self.speed,
        }
    }

    /// 捕获基姿势（rest pose）：把全部 [`AnimationPlayer::node_entities`]
    /// 目标实体当前的 `Transform`（平移 / 旋转 / 缩放三条通道）记入
    /// [`AnimationPlayer::rest`]。
    ///
    /// 调用时机：**实体生成之后、任何动画推进之前**——`spawn_gltf_instance`
    /// 的调用序（先 spawn 全部节点实体、后构造播放器）满足该前提，此时
    /// 捕获到的就是「未播放动画时的初始值」。重复调用以调用时刻的姿势
    /// 覆盖旧快照（换装 / 重新锚定基姿势时可用）。没有 `Transform` 的
    /// 目标实体跳过。
    ///
    /// 捕获后 crossfade 的「仅源有」通道在混合期从冻结源值平滑淡回基
    /// 姿势（语义见 [`AnimationBlend`]）。两个诚实的边界：
    /// - **rest 是捕获时刻的快照**：捕获后若 Transform 被其它剪辑改过，
    ///   混合仍淡回捕获时记录的值（不是「当前未动画姿势」）；
    /// - 退化的零四元数（glTF 节点缺省 rotation 的加载产物）按恒等
    ///   处理——零四元数不可作为 slerp 端点。
    pub fn capture_rest(&mut self, world: &iris_core::world::World) {
        self.rest = self
            .node_entities
            .iter()
            .filter_map(|slot| slot.as_ref())
            .filter_map(|&entity| {
                let transform = world.get::<Transform>(entity)?;
                // 零四元数（长度² ≈ 0）不是合法旋转：按恒等捕获
                let rotation = if transform.rotation.length_squared() < 1e-10 {
                    Quat::IDENTITY
                } else {
                    transform.rotation
                };
                Some((
                    entity,
                    [
                        AnimatedField::Translation(transform.position.to_array()),
                        AnimatedField::Rotation(rotation),
                        AnimatedField::Scale(transform.scale.to_array()),
                    ],
                ))
            })
            .flat_map(|(entity, fields)| fields.map(move |field| (entity, field)))
            .collect();
    }

    /// 启动向目标剪辑 `clip` 的 crossfade。
    ///
    /// 语义：
    /// - `duration <= 0` = **硬切换**（立即改播目标剪辑，时间归零）；
    /// - 与当前活动剪辑相同 = 忽略（无过渡意义）；
    /// - 目标越界 = 忽略并 `debug_assert!`（release 下静默忽略——调用方
    ///   传错索引不应让播放器报废）；
    /// - 过渡中再次 transition：以当前活动剪辑为新源重新开始。
    ///
    /// 过渡开始时把源姿势冻结为快照（见 [`AnimationBlend`]），目标剪辑
    /// 从 0 开始正常推进并置 `playing = true`。
    pub fn transition(&mut self, clip: usize, duration: f32) {
        if clip >= self.clips.len() {
            debug_assert!(
                false,
                "transition 目标剪辑越界：{clip} / {}",
                self.clips.len()
            );
            return;
        }
        // 与混合空间互斥：任何 transition（含状态机经此的路径）都退出
        // 混合空间模式——同一时刻只有一种姿势来源
        self.blend_space_active = false;
        if clip == self.active {
            return;
        }
        if duration <= 0.0 {
            self.active = clip;
            self.time = 0.0;
            self.blend = None;
            return;
        }
        self.blend = Some(AnimationBlend {
            from_clip: self.active,
            from_time: self.time,
            duration,
            elapsed: 0.0,
        });
        self.active = clip;
        self.time = 0.0;
        self.playing = true;
    }

    /// 请求切换到状态机状态 `state`。
    ///
    /// 存在 `current → state` 边时按 `edge.duration` 启动 crossfade，
    /// 否则硬切换（**无边不是错误**——显式硬切换是合法用法，但会打一条
    /// `log::debug!` 留线索：含 from / to 状态与边表长度。调试期清空
    /// edges 会让全部过渡退化成跳变、画面闪变无线索，由此可定位，
    /// 打磨 IV 摩擦③）；两种情况下 `looping` / `speed` 都同步为目标
    /// 状态值，`current_state` 立即更新。与 [`AnimationPlayer::transition`]
    /// 同款边界语义：同状态请求忽略、越界立即 panic（API 误用——
    /// `request_state` 只在状态切换时调用、非每帧热点，无条件断言
    /// 让 release 构建同样暴露调用方错误，与世界的尝试型锁冲突
    /// panic 同一哲学）。
    /// **何时调用由游戏侧判定**——v2（C4b）新增条件自动转移见
    /// [`AnimationPlayer::transitions`] / [`Self::step_state_machine`]；
    /// 两条路径共存，时长口径不同：本方法取 v1 边表首条同对边的
    /// duration，自动步检取命中转移自身的 duration。
    pub fn request_state(&mut self, state: usize) {
        assert!(
            state < self.states.len(),
            "request_state 目标状态越界：{state} / {}",
            self.states.len()
        );
        if state == self.current_state {
            return;
        }
        let from = self.current_state;
        let edge = self
            .edges
            .iter()
            .find(|e| e.from == from && e.to == state)
            .map(|e| e.duration);
        let duration = match edge {
            Some(duration) => duration,
            None => {
                log::debug!(
                    "request_state：状态 {from} → {state} 无边（边表 {} 条）——硬切换到新状态",
                    self.edges.len()
                );
                0.0
            }
        };
        self.enter_state(state, duration);
    }

    /// 进入状态 `state` 并按 `duration` 过渡（v2 抽出的共用落账口：
    /// `request_state` 与 `step_state_machine` 共享同一套字段同步语义）。
    /// 越界断言与 [`Self::request_state`] 同款（配置错误必须暴露）。
    fn enter_state(&mut self, state: usize, duration: f32) {
        assert!(
            state < self.states.len(),
            "enter_state 目标状态越界：{state} / {}",
            self.states.len()
        );
        let target = self.states[state].clone();
        self.current_state = state;
        self.looping = target.looping;
        self.speed = target.speed;
        self.transition(target.clip, duration);
    }

    /// 写浮点参数到状态机黑板（C4b · v2）。同名覆盖。
    pub fn set_float(&mut self, key: impl Into<String>, value: f32) {
        self.params.insert(key.into(), AnimValue::Float(value));
    }

    /// 写布尔参数到状态机黑板（C4b · v2）。同名覆盖。
    pub fn set_bool(&mut self, key: impl Into<String>, value: bool) {
        self.params.insert(key.into(), AnimValue::Bool(value));
    }

    /// 读浮点参数（缺失 / 非浮点 = `None`，区别于守卫求值的中性默认）。
    pub fn param_float(&self, key: &str) -> Option<f32> {
        match self.params.get(key) {
            Some(AnimValue::Float(v)) => Some(*v),
            _ => None,
        }
    }

    /// 读布尔参数（缺失 / 非布尔 = `None`）。
    pub fn param_bool(&self, key: &str) -> Option<bool> {
        match self.params.get(key) {
            Some(AnimValue::Bool(v)) => Some(*v),
            _ => None,
        }
    }

    /// 状态机条件步检（C4b · v2，纯播放器侧，无 World 可单测）：
    /// 按 `transitions` **声明序**扫描，取首条 `from == current_state`
    /// 且守卫成立的转移，经 [`Self::enter_state`] 进入目标状态并以该
    /// 转移的 `duration` 过渡；**一帧至多触发一条**（防乒乓 / 级联——
    /// 目标状态的出边最早也要下一帧才被评估）。
    ///
    /// 返回触发的转移下标（`None` = 本帧无转移）。`transitions` 为空
    /// ⇒ 恒 `None` 零开销（v1 逐位兼容锚）；同状态自环（`from == to`
    /// 且守卫成立）不触发（与 [`Self::request_state`] 的同状态忽略
    /// 同款语义——避免每帧重启循环剪辑）。目标状态越界 ⇒ 断言 panic
    /// （转移表是作者声明的配置，错误必须暴露，与 `request_state` 同哲学）。
    pub fn step_state_machine(&mut self) -> Option<usize> {
        for index in 0..self.transitions.len() {
            if self.transitions[index].from != self.current_state
                || self.transitions[index].to == self.current_state
            {
                continue;
            }
            if !self.guard_holds(&self.transitions[index].guard) {
                continue;
            }
            let transition = &self.transitions[index];
            assert!(
                transition.to < self.states.len(),
                "转移表 #{index} 引用越界状态：{} → {} / {}",
                transition.from,
                transition.to,
                self.states.len()
            );
            let duration = transition.duration;
            let to = transition.to;
            self.enter_state(to, duration);
            return Some(index);
        }
        None
    }

    /// 守卫求值（C4b）：黑板类叶子走 [`AnimGuard::eval`]（纯读参数），
    /// [`AnimGuard::ClipFinished`] 走播放器状态（[`Self::current_clip_finished`]），
    /// 复合子守卫递归——两层上下文（黑板 / 播放时间）在此合流。
    fn guard_holds(&self, guard: &AnimGuard) -> bool {
        match guard {
            AnimGuard::ClipFinished => self.current_clip_finished(),
            AnimGuard::All(inner) => inner.iter().all(|g| self.guard_holds(g)),
            AnimGuard::Any(inner) => inner.iter().any(|g| self.guard_holds(g)),
            AnimGuard::Not(inner) => !self.guard_holds(inner),
            other => other.eval(&self.params),
        }
    }

    /// 当前状态的剪辑是否已播完（非循环、时长 > 0、time ≥ duration）。
    fn current_clip_finished(&self) -> bool {
        let Some(state) = self.states.get(self.current_state) else {
            return false;
        };
        if state.looping {
            return false;
        }
        let Some(clip) = self.clips.get(state.clip) else {
            return false;
        };
        clip.duration > 0.0 && self.time >= clip.duration
    }

    /// 运行时重定向（C4b，按骨名）：把本播放器的节点绑定表
    /// （`node_entities`）**整体重绑**到另一具骨架的实体——通道不改写
    /// （节点下标语义保持源骨架空间），只换「下标 → 实体」指向：
    /// 节点 i 的新实体 = `target_entities[j]`，j 为 `target_names` 中
    /// 首个与 `node_names[i]` **骨名匹配**（[`normalize_bone_name`]）
    /// 的下标。
    ///
    /// 用途：同一剪辑库驱动多具命名一致（或仅导入器前缀差异）的骨架
    /// ——GLB A 的剪辑 + 骨架 B 的实体表即可跑通，替代 iris-exp 的
    /// 离线 Blender 烘焙路线（§8.35⑨ 拒绝的是**导入期**重定向，本节
    /// 是运行期播放器侧重绑，不同层面）。
    ///
    /// 语义与限制：
    /// - 源无名节点（`node_names` 短表 / None 项）**不参与重定向**：
    ///   其绑定原样保留（重定向是显式操作，半途断绑比静默保留更糟）；
    /// - 有名字但目标骨架无同名骨 ⇒ 该节点断绑为 `None`（通道本帧起
    ///   不覆写，骨停在当前 `Transform`——与「节点不在 active scene」
    ///   既有 None 语义合流），计入 [`RetargetReport::unmatched`]；
    /// - 目标重名取**先出现者**（向量序确定 ⇒ 结果确定）；
    /// - `node_names` 为空表（手写构造的播放器）⇒ 恒 no-op——重定向
    ///   以源名字表为前提，GLB 实例的 [`GltfInstance::player`] 自带；
    /// - 两次 `target_names`/`target_entities` 长度不一致 ⇒ 以短表为准
    ///   （zip 截断，不 panic——名字表带缺项是资产侧常态）。
    ///
    /// [`GltfInstance::player`]: crate::asset::GltfInstance::player
    pub fn retarget_by_name(
        &mut self,
        target_names: &[Option<String>],
        target_entities: &[Option<Entity>],
    ) -> RetargetReport {
        let mut report = RetargetReport::default();
        // 目标骨架：骨名归一键 → 首个下标（BTreeMap 固定序构建，
        // 同键重复名取先出现者——与 node_entity_by_name 同口径）
        let mut first_by_key: BTreeMap<String, usize> = BTreeMap::new();
        for (index, name) in target_names.iter().enumerate() {
            if let Some(name) = name {
                first_by_key
                    .entry(normalize_bone_name(name))
                    .or_insert(index);
            }
        }
        for index in 0..self.node_names.len() {
            let Some(name) = self.node_names[index].clone() else {
                continue; // 无名节点不参与（保留原绑定）
            };
            let target = first_by_key
                .get(&normalize_bone_name(&name))
                .and_then(|&j| target_entities.get(j))
                .copied()
                .flatten();
            if index < self.node_entities.len() {
                self.node_entities[index] = target;
            }
            if target.is_some() {
                report.bound += 1;
            } else {
                report.unmatched += 1;
            }
        }
        report
    }

    /// 取走全部待消费事件名（drain 语义）。**每帧调用一次**——
    /// 事件是帧内消息，不调用会跨帧累积。
    pub fn take_events(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_events)
    }

    /// 帧数据窗口判定（R74）：active 剪辑在**当前播放时间**下，名为
    /// `name` 的窗口是否 active（纯函数语义见 [`window_active`]——
    /// 半开区间、未声明即 false）。查询发生在
    /// `update_animations` 推进之后（消费系统读到的 time 为本帧新值，
    /// 与事件「跨过即发」同帧衔接：事件帧即窗口首帧）。无剪辑恒 false。
    pub fn window_active(&self, name: &str) -> bool {
        self.clips
            .get(self.active)
            .map(|clip| window_active(&clip.windows, self.time, name))
            .unwrap_or(false)
    }

    /// 当前帧参数曲线值查询（R74）：同名多轨取先声明者（`clips` 内
    /// 向量序确定）。无该名轨 = `None`。
    pub fn parameter(&self, name: &str) -> Option<f32> {
        self.parameter_values
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| *v)
    }

    /// 激活混合空间模式（v3）：此后 `update_animations` 每帧按
    /// [`AnimationPlayer::blend_param`] 采样 [`AnimationPlayer::blend_space`]
    /// 做 N 路姿势混合，`active` 剪辑的通道采样停用（语义与互斥边界见
    /// [`AnimationPlayer::blend_space_active`]）。进行中的 crossfade 立即
    /// 丢弃（姿势直接切换到混合空间，无过渡——混合空间内部的权重连续性
    /// 由参数点连续变化保证，游戏侧移动 `blend_param` 即天然的淡入淡出）。
    ///
    /// # Panics / 边界
    /// 未设置 `blend_space` 字段时 `debug_assert!`（API 误用），release 下
    /// 静默忽略（标志不置位）。
    pub fn play_blendspace(&mut self) {
        if self.blend_space.is_none() {
            debug_assert!(false, "play_blendspace 需先设置 blend_space 字段");
            return;
        }
        self.blend_space_active = true;
        self.blend = None;
    }

    /// 取走本帧根运动位移增量（v3，drain 语义）。返回 `None` = 本帧（及
    /// 累积以来）无根运动——根运动未启用、或暂停 / 停在末端的零增量帧
    /// 也会产 `Some(零向量)`，消费方按向量用即可。典型消费（游戏侧每帧）：
    ///
    /// ```ignore
    /// // 剪辑空间增量 → 旋转到角色 yaw 系 → 交给角色控制器位移
    /// if let Some(delta) = world.get_mut::<AnimationPlayer>(root).unwrap().take_root_motion() {
    ///     let yaw = Quat::from_rotation_y(character_yaw);
    ///     let world_delta = yaw * delta;
    ///     let out = move_and_slide(&world, pos, world_delta / dt, dt, &params);
    ///     world.get_mut::<Transform>(root).unwrap().position = out.position;
    /// }
    /// ```
    pub fn take_root_motion(&mut self) -> Option<Vec3> {
        self.pending_root_motion.take()
    }

    /// 启动附加动画层（v4）：`clip` 从 0 时起以 `weight` 权重叠加增量
    /// （速度 1、循环；逐项调整直接改 [`AnimationPlayer::additive`] 的
    /// 字段）。重复调用以新参数整体重置（时间归零）——换受击剪辑时
    /// 旧层的相位不留残留。
    ///
    /// 边界语义与 `transition` 同款：剪辑越界 `debug_assert!` 后忽略
    /// （release 静默——调用方传错索引不应让播放器报废）。附加层与
    /// 所有姿势来源**正交**：激活 / 停用不触碰 active、blend、
    /// blend_space_active 中的任何一个。
    pub fn play_additive(&mut self, clip: usize, weight: f32) {
        if clip >= self.clips.len() {
            debug_assert!(
                false,
                "play_additive 剪辑越界：{clip} / {}",
                self.clips.len()
            );
            return;
        }
        self.additive = Some(AdditiveLayer {
            clip,
            time: 0.0,
            speed: 1.0,
            weight,
            looping: true,
        });
    }

    /// 停用附加动画层（v4）：下一帧起姿势为纯主姿势（无过渡淡出——
    /// 受击类增量的消失时机由玩法侧控制，需要淡出时逐步调低
    /// `additive.weight` 后再停用）。
    pub fn stop_additive(&mut self) {
        self.additive = None;
    }

    /// 设置节点遮罩（v4）：只覆写 `nodes` 集合内的节点（语义见
    /// [`AnimationMask`]）。重复调用整体替换。
    pub fn set_mask(&mut self, nodes: impl IntoIterator<Item = usize>) {
        self.mask = Some(AnimationMask {
            nodes: nodes.into_iter().collect(),
        });
    }

    /// 清除节点遮罩：恢复全覆写（v1 语义）。
    pub fn clear_mask(&mut self) {
        self.mask = None;
    }

    /// 推进后的时间与播放状态（不修改自身，供系统两阶段处理）。
    pub fn advanced(&self, dt: f32) -> (f32, bool) {
        if !self.playing {
            return (self.time, false);
        }
        let Some(clip) = self.clips.get(self.active) else {
            return (self.time, false);
        };
        if clip.duration <= 0.0 {
            return (self.time, false);
        }
        let t = self.time + dt * self.effective_speed();
        if self.effective_looping() {
            (t % clip.duration, true)
        } else if t >= clip.duration {
            (clip.duration, false)
        } else {
            (t, true)
        }
    }
}

impl AdditiveLayer {
    /// 附加层推进后的时间（纯函数，供系统两阶段处理）：`dt` 已含
    /// 暂停门（暂停传 0）；循环按 `duration` 取模、非循环钳制在末端。
    /// `duration <= 0` 的剪辑时间不动（单关键帧剪辑的增量恒为零，
    /// 推进无意义）。
    fn advanced_additive_time(&self, dt: f32, duration: f32) -> f32 {
        if duration <= 0.0 {
            return self.time;
        }
        let raw = self.time + dt * self.speed;
        if self.looping {
            raw % duration
        } else {
            raw.min(duration)
        }
    }
}

/// 事件时刻跨界检测公共数学（纯函数）：时刻表 `times` 中落在跨界区间内
/// 的下标（按触发时刻升序；一时刻多次跨越则下标重复出现多次）。
///
/// **动画事件轨道（[`collect_events`]）与演出时间线事件轨道
/// （`iris_engine::timeline::collect_timeline_events`）共享的单一来源**——
/// 两类轨道的「跨过即发」语义必须逐位一致（同一套回绕 / multi-fire /
/// 末端不重复语义），故把数学从 `collect_events` 中抽出为独立纯函数，
/// 两者各自接线。
///
/// 时间语义（调用方务必区分）：
/// - **非循环**：`new_time` 为钳制后的播放时间，命中区间 `(prev_time, new_time]`。
///   播放器停在末端后每帧 `prev == new`，区间为空 → **末端事件不重复触发**；
/// - **循环**：`new_time` 为**取模前的原始推进时间**（`prev + dt·speed`，
///   一帧可跨多圈）。事件每被跨越一次触发一次——一帧跨 k 圈即重复命中
///   k 次（**multi-fire**，确定性：命中次数 = 跨越该事件时刻的完整次数）；
///   单圈内的回绕 `(prev, dur] ∪ [0, new]` 是跨圈不足一圈时的特例。
///   `duration <= 0` 恒返回空。
///
/// 两阶段数据流注意：`prev_time` 必须取推进前的播放时间
/// （先取再推进，见 [`update_animations`]）。
pub fn collect_crossings(
    times: &[f32],
    prev_time: f32,
    new_time: f32,
    duration: f32,
    looping: bool,
) -> Vec<usize> {
    if duration <= 0.0 || times.is_empty() {
        return Vec::new();
    }
    // (触发时刻, 事件下标)：触发时刻用于输出排序（时刻表本身可乱序）
    let mut hits: Vec<(f32, usize)> = Vec::new();
    if looping {
        for (i, &time) in times.iter().enumerate() {
            // 跨越次数 = 满足 prev < e.time + m·dur <= new 的整数 m 的个数
            //            = floor((new - e.time)/dur) - floor((prev - e.time)/dur)
            // 下界取严格大于：恰在 prev 处的跨越上一帧已触发，不重复
            let lower = ((prev_time - time) / duration).floor() as i64;
            let upper = ((new_time - time) / duration).floor() as i64;
            for m in lower + 1..=upper {
                hits.push((time + m as f32 * duration, i));
            }
        }
    } else {
        let new_time = new_time.min(duration);
        for (i, &time) in times.iter().enumerate() {
            if time > prev_time && time <= new_time {
                hits.push((time, i));
            }
        }
    }
    // 按触发时刻升序；同刻按下标（确定性）
    hits.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    hits.into_iter().map(|(_, i)| i).collect()
}

/// 事件跨界检测（纯函数）：播放时间从 `prev_time` 推进到 `new_time`，
/// 返回本帧命中的事件下标（按触发时刻升序；一事件多次触发则下标重复
/// 出现多次）。跨界数学见 [`collect_crossings`]（本函数是其薄接线层）。
///
/// 两阶段数据流注意：`prev_time` 必须取推进前的 `player.time`
/// （阶段一先取再推进）。
pub fn collect_events(
    events: &[AnimationEvent],
    prev_time: f32,
    new_time: f32,
    duration: f32,
    looping: bool,
) -> Vec<usize> {
    let times: Vec<f32> = events.iter().map(|event| event.time).collect();
    collect_crossings(&times, prev_time, new_time, duration, looping)
}

/// crossfade 权重（目标剪辑占比）：`smoothstep(elapsed / duration)`。
///
/// 选 smoothstep 而非线性：线性在过渡首尾速度突变（起停生硬），
/// smoothstep 两端导数为 0、视觉更顺滑；中点处两者同为 0.5，可预测性
/// 不打折。`duration <= 0` 恒为 1.0（纯目标）。
fn blend_weight(elapsed: f32, duration: f32) -> f32 {
    if duration <= 0.0 {
        return 1.0;
    }
    let t = (elapsed / duration).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 同路径的两份采样值按目标权重 `w` 混合。
///
/// 平移/缩放分量线性 lerp；旋转用 **slerp 而非 nlerp**——nlerp 在角度
/// 较大时角速度先快后慢（非线性），crossfade 中段旋转会肉眼发冲，
/// slerp 恒定角速度，代价只是少量三角函数（每实体每帧个位数旋转通道，
/// 不值得省）。点积为负先翻转短路径（与通道采样同款）。
fn blend_fields(src: &AnimatedField, dst: &AnimatedField, w: f32) -> AnimatedField {
    match (src, dst) {
        (AnimatedField::Translation(a), AnimatedField::Translation(b)) => {
            AnimatedField::Translation(lerp3(a, b, w))
        }
        (AnimatedField::Scale(a), AnimatedField::Scale(b)) => AnimatedField::Scale(lerp3(a, b, w)),
        (AnimatedField::Rotation(a), AnimatedField::Rotation(b)) => {
            let mut b = *b;
            if a.dot(b) < 0.0 {
                b = -b;
            }
            AnimatedField::Rotation(a.slerp(b, w))
        }
        // 两侧路径必然一致（按「实体 + 路径」配对后才会调用），兜底取目标
        _ => *dst,
    }
}

fn lerp3(a: &[f32; 3], b: &[f32; 3], w: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * w,
        a[1] + (b[1] - a[1]) * w,
        a[2] + (b[2] - a[2]) * w,
    ]
}

fn animated_path(field: &AnimatedField) -> AnimationPath {
    match field {
        AnimatedField::Translation(_) => AnimationPath::Translation,
        AnimatedField::Rotation(_) => AnimationPath::Rotation,
        AnimatedField::Scale(_) => AnimationPath::Scale,
    }
}

/// N 路旋转加权（v3）：**首个四元数为基准，点积 < 0 翻转后加权求和再
/// 归一（nlerp）**。
///
/// 为什么 N 路不用 slerp：slerp 是两算子、不具结合性——`slerp(slerp(a,b,
/// w₁), c, w₂)` 的结果依赖配对顺序，三路以上混合会出现「权重对称但结果
/// 不对称」且误差随链式插值累积；nlerp 退化为一次加权求和 + 单次归一，
/// 对权重完全对称、与输入顺序无关（求和顺序固定即可复现）。代价是角度
/// 大时角速度非线性——混合空间的相邻剪辑夹角小（步态族），非线性不可
/// 感知；两路 crossfade 可能拉到 180°，仍走 [`blend_fields`] 的 slerp
/// （两路语义不变）。求和后长度趋零（对跖四元数互抵）按基准四元数兜底
/// （确定性，不产 NaN）。
fn blend_rotation_nway(inputs: &[(Quat, f32)]) -> Quat {
    let base = inputs[0].0;
    let mut sum = base * inputs[0].1;
    for &(q, w) in &inputs[1..] {
        let mut q = q;
        if base.dot(q) < 0.0 {
            q = -q;
        }
        sum += q * w;
    }
    if sum.length_squared() < 1e-10 {
        base
    } else {
        sum.normalize()
    }
}

/// 「目标实体 + 路径」分组（[`blend_pose`] 内部）：该组的全部采样值与
/// 各自权重（保持首遇顺序，确定性）。
type PoseGroup = (Entity, AnimationPath, Vec<(AnimatedField, f32)>);

/// N 路姿势混合（v3，纯函数）：对每个 `(剪辑索引, 权重, 时间)` 三元组
/// 采样剪辑，按「目标实体 + 路径」并集逐通道加权——平移 / 缩放为加权
/// 线性和（权重和 1 时即逐分量 lerp 的 N 路推广），旋转走
/// [`blend_rotation_nway`]。权重 ≤ 0 的输入跳过（不采样）；剪辑越界
/// `debug_assert!` 后跳过（release 静默，坏索引不应报废整帧姿势）。
///
/// 空输入 / 全零权重返回空表（调用方 = 无姿势来源，不写任何 Transform）。
pub fn blend_pose(
    clips: &[AnimationClip],
    node_entities: &[Option<Entity>],
    inputs: &[(usize, f32, f32)],
) -> Vec<(Entity, AnimatedField)> {
    // 「目标实体 + 路径」并集分组（保持首遇顺序，确定性）
    let mut groups: Vec<PoseGroup> = Vec::new();
    for &(clip_index, weight, time) in inputs {
        if weight <= 0.0 {
            continue;
        }
        let Some(clip) = clips.get(clip_index) else {
            debug_assert!(false, "blend_pose 剪辑越界：{clip_index} / {}", clips.len());
            continue;
        };
        for channel in &clip.channels {
            let Some(target) = node_entities.get(channel.node).copied().flatten() else {
                continue;
            };
            match groups
                .iter_mut()
                .find(|(e, p, _)| *e == target && *p == channel.path)
            {
                Some((_, _, list)) => list.push((channel.sample(time), weight)),
                None => groups.push((target, channel.path, vec![(channel.sample(time), weight)])),
            }
        }
    }
    groups
        .into_iter()
        .map(|(target, _, list)| {
            let first = list[0].0;
            let field = match first {
                AnimatedField::Rotation(_) => {
                    let quats: Vec<(Quat, f32)> = list
                        .iter()
                        .filter_map(|(field, w)| match field {
                            AnimatedField::Rotation(q) => Some((*q, *w)),
                            _ => None,
                        })
                        .collect();
                    AnimatedField::Rotation(blend_rotation_nway(&quats))
                }
                AnimatedField::Translation(v) => {
                    AnimatedField::Translation(weighted_sum3(&v, &list))
                }
                AnimatedField::Scale(v) => AnimatedField::Scale(weighted_sum3(&v, &list)),
            };
            (target, field)
        })
        .collect()
}

/// `[f32; 3]` 的 N 路加权和：首项权重 × 首值 + 其余各项加权和。
fn weighted_sum3(first: &[f32; 3], rest: &[(AnimatedField, f32)]) -> [f32; 3] {
    let mut acc = [
        first[0] * rest[0].1,
        first[1] * rest[0].1,
        first[2] * rest[0].1,
    ];
    for (field, w) in rest.iter().skip(1) {
        let v = match field {
            AnimatedField::Translation(v) | AnimatedField::Scale(v) => v,
            _ => continue,
        };
        acc[0] += v[0] * w;
        acc[1] += v[1] * w;
        acc[2] += v[2] * w;
    }
    acc
}

/// 附加层的加权增量（v4，纯函数）：剪辑通道在 `time` 处相对参考帧
/// （`t = 0` 采样值）的增量，乘整层权重 `weight`。
///
/// # 增量数学
///
/// - **平移 / 缩放（加法群）**：`delta = (v(time) − v(0)) × weight`——
///   叠加语义是「从参考姿势偏移多少」，加法群下直接做差；
/// - **旋转（非交换）**：相对旋转 `d = q(time) · q(0)⁻¹`（把参考帧
///   旋回恒等的左乘增量：`q(0) · d = q(time)`），叠加为主姿势右乘
///   `main · d`——附加剪辑的关键帧表达的是**节点局部系**内的姿态，
///   主姿势已把节点带到 `main`，附加偏移发生在主姿势之后的同一局部系，
///   故右乘（若左乘 `d · main`，增量会被解释进父级系，受击叠加在
///   移动循环上会随身体朝向漂移）。权重经
///   `slerp(Identity, d, weight)` 压幅：`weight = 0` 恰为恒等、
///   `weight = 1` 恰为全幅增量，且 `slerp` 保持单位长度（直接
///   `d^weight` 幂运算无解析式）。`d` 归一化后与恒等的点积为负时
///   取反（四元数双覆盖，保证走最短弧）。
///
/// 剪辑静止段（`sample(time) == sample(0)`）的增量恰为零——附加层
/// 激活但不产生偏移。
pub fn additive_delta(channel: &AnimationChannel, time: f32, weight: f32) -> AnimatedField {
    match channel.sample(time) {
        AnimatedField::Translation(v) => {
            let AnimatedField::Translation(v0) = channel.sample(0.0) else {
                unreachable!("同一通道路径恒定");
            };
            AnimatedField::Translation([
                (v[0] - v0[0]) * weight,
                (v[1] - v0[1]) * weight,
                (v[2] - v0[2]) * weight,
            ])
        }
        AnimatedField::Scale(v) => {
            let AnimatedField::Scale(v0) = channel.sample(0.0) else {
                unreachable!("同一通道路径恒定");
            };
            AnimatedField::Scale([
                (v[0] - v0[0]) * weight,
                (v[1] - v0[1]) * weight,
                (v[2] - v0[2]) * weight,
            ])
        }
        AnimatedField::Rotation(q) => {
            let AnimatedField::Rotation(q0) = channel.sample(0.0) else {
                unreachable!("同一通道路径恒定");
            };
            let mut d = (q * q0.conjugate()).normalize();
            if d.dot(Quat::IDENTITY) < 0.0 {
                d = -d;
            }
            AnimatedField::Rotation(Quat::IDENTITY.slerp(d, weight))
        }
    }
}

/// 附加增量合成（v4）：主姿势（或当前 Transform 基值）+ 附加增量。
/// 平移 / 缩放分量加法；旋转**右乘**（增量解释进节点局部系，推导见
/// [`additive_delta`]）。路径不匹配时兜底取基值（调用方按同路径配对，
/// 与 [`blend_fields`] 同款防御）。
fn compose_additive(base: &AnimatedField, delta: &AnimatedField) -> AnimatedField {
    match (base, delta) {
        (AnimatedField::Translation(a), AnimatedField::Translation(d)) => {
            AnimatedField::Translation([a[0] + d[0], a[1] + d[1], a[2] + d[2]])
        }
        (AnimatedField::Scale(a), AnimatedField::Scale(d)) => {
            AnimatedField::Scale([a[0] + d[0], a[1] + d[1], a[2] + d[2]])
        }
        (AnimatedField::Rotation(q), AnimatedField::Rotation(d)) => {
            AnimatedField::Rotation(*q * *d)
        }
        _ => *base,
    }
}

/// 附加层叠加（v4，`update_animations` 阶段一的内联步骤）：把附加剪辑
/// 全部通道的加权增量合成进主姿势任务表。**只在主姿势已覆写的通道上
/// 叠加**（按「目标实体 + 路径」配对；crossfade 的「仅源有」淡回通道
/// 同样在列）——主姿势未覆写的通道跳过。诚实边界：若以目标实体当前
/// `Transform` 为基，而当前值含附加层上一帧自身的输出，增量会逐帧
/// 复合、无界漂移（旋转类每帧多转一份），与「受击/呼吸叠加不破坏
/// 主姿势」的语义相悖——附加剪辑的通道应与主姿势对齐设计，未对齐
/// 通道的覆写完全交给主姿势（缺席 = 保持当前值，与覆写语义自洽）。
fn apply_additive(
    fields: &mut [(Entity, AnimatedField)],
    clips: &[AnimationClip],
    node_entities: &[Option<Entity>],
    layer: &AdditiveLayer,
    time: f32,
) {
    let Some(clip) = clips.get(layer.clip) else {
        return;
    };
    for channel in &clip.channels {
        let Some(target) = node_entities.get(channel.node).copied().flatten() else {
            continue;
        };
        let delta = additive_delta(channel, time, layer.weight);
        if let Some((_, field)) = fields
            .iter_mut()
            .find(|(entity, field)| *entity == target && animated_path(field) == channel.path)
        {
            let combined = compose_additive(field, &delta);
            *field = combined;
        }
    }
}

/// 遮罩过滤判定（v4）：目标实体对应的节点下标**任一**在遮罩集合内
/// 即允许覆写（同一实体被多个节点映射的非常规装配按宽松口径放行）。
fn node_allows_overwrite(
    node_entities: &[Option<Entity>],
    nodes: &BTreeSet<usize>,
    target: Entity,
) -> bool {
    node_entities
        .iter()
        .enumerate()
        .any(|(index, slot)| *slot == Some(target) && nodes.contains(&index))
}

/// 根运动帧增量（v3，纯函数）：平移通道在 `prev_time → raw_time` 间的
/// 采样位移差。**循环回绕的三段精确累积**——一帧跨 k 次回绕时，
/// 增量 = 「旧圈余段：v(dur) − v(prev)」+「(k−1) 个整圈位移：v(dur) − v(0)」
/// +「新圈已走：v(raw mod dur) − v(0)」，其中 `k = ⌊raw/dur⌋ − ⌊prev/dur⌋`；
/// 方向语义：增量是**剪辑空间**（根节点局部平移）的前向位移，单圈不回绕
/// 时退化为 `v(raw) − v(prev)`。非循环按钳制后的时间差（停在末端后
/// prev == new → 零增量，不重复计步）。`duration <= 0` 恒为零。
///
/// # 消费契约
///
/// 增量累积在 [`AnimationPlayer::pending_root_motion`]（剪辑空间），
/// 消费方每帧 [`AnimationPlayer::take_root_motion`] 取走后**旋转到角色
/// yaw 系**（剪辑以 +Z 或引擎约定前方为前进向，角色朝向由玩法控制）
/// 再交给 `move_and_slide` 做位移——根运动只出「每帧走多远」，不直接
/// 写角色 Transform（见 [`AnimationPlayer::take_root_motion`] 用例）。
pub fn root_motion_delta(
    channel: &AnimationChannel,
    prev_time: f32,
    raw_time: f32,
    duration: f32,
    looping: bool,
) -> Vec3 {
    if duration <= 0.0 {
        return Vec3::ZERO;
    }
    let value_at = |t: f32| match channel.sample(t) {
        AnimatedField::Translation(v) => Vec3::new(v[0], v[1], v[2]),
        _ => Vec3::ZERO,
    };
    if !looping {
        // 非循环：两端钳制（末端 prev == new → 零增量）
        return value_at(raw_time.min(duration)) - value_at(prev_time.clamp(0.0, duration));
    }
    let laps = (raw_time / duration).floor() as i64 - (prev_time / duration).floor() as i64;
    if laps <= 0 {
        // 单圈内（未回绕）
        return value_at(raw_time) - value_at(prev_time);
    }
    // 回绕：旧圈余段 + (laps − 1) 个整圈 + 新圈已走（multi-lap 语义与
    // collect_events 的跨越计数同源）
    let full_lap = value_at(duration) - value_at(0.0);
    let wrapped = value_at(raw_time % duration) - value_at(0.0);
    (value_at(duration) - value_at(prev_time)) + full_lap * (laps - 1) as f32 + wrapped
}

/// 单个动画根实体的采样任务包（`update_animations` 阶段一产出、阶段二
/// 消费）：根实体 + 推进后的时间 + 循环标记 + 「目标实体 → 覆写字段」
/// 列表（crossfade / 混合空间时已混合完毕）+ 本帧命中的事件名 +
/// 本帧根运动增量（未启用 / 无效时为 None）+ active 剪辑参数曲线
/// 采样值（R74，推进后时间采样，阶段二整表覆写）。
type AnimationJob = (
    Entity,
    f32,
    bool,
    Vec<(Entity, AnimatedField)>,
    Vec<String>,
    Option<Vec3>,
    Vec<(String, f32)>,
);

/// 骨名归一键（C4b 运行时重定向的匹配口径）：trim → 小写 → 剥常见
/// 导入器前缀（`mixamorig` / `bip01` / `mocap` / `ue4-skel`，可叠加，
/// 无分隔符形态如 `mixamorigHips` 同样剥）→ 剥后残留的头部
/// 分隔符（`:` `_` `-` 空格）吃掉 → 空格压成 `_`。
///
/// 刻意保守：只处理**前缀差异**这一种真实的跨骨架形态（Mixamo 系
/// `mixamorig:Hips` vs 自产骨架 `Hips`）；不做编辑距离 / 内置别名表——
/// 语义性改名（`thigh_r` vs `rightThigh`）留给调用方自备映射，
/// 引擎不猜（猜错的绑定比断绑更难排查）。误伤面（恰以这些前缀
/// 开头的真实骨名，如假想骨 `mocapTarget`）归一后两边仍一致地指向
/// 同一目标，匹配语义自反 ⇒ 可接受。纯函数、确定性。
pub fn normalize_bone_name(name: &str) -> String {
    const PREFIXES: [&str; 4] = ["mixamorig", "bip01", "mocap", "ue4-skel"];
    let mut s = name.trim().to_lowercase();
    // 每轮必剥掉 ≥4 字符前缀 ⇒ 严格递减，循环天然终止
    while let Some(prefix) = PREFIXES.iter().copied().find(|p| s.starts_with(p)) {
        s = s[prefix.len()..]
            .trim_start_matches([':', '_', '-', ' '])
            .to_string();
    }
    s.replace(' ', "_")
}

/// 每帧动画系统：推进所有 [`AnimationPlayer`]，把采样结果覆写到目标实体的
/// `Transform`。必须在 `propagate_transforms` 之前执行（DefaultPlugins 已保证）；
/// `Time` 资源缺席时 dt 视为 0（可手动设置 `player.time` 后调用以定位到指定时刻）。
///
/// 状态机 v2（C4b）：写回阶段每帧对每个播放器调用一次
/// [`AnimationPlayer::step_state_machine`]——`transitions` 空表 ⇒ 恒
/// `None` 零开销，v1 行为逐位不变（转移生效帧的姿势仍来自旧状态采样，
/// 过渡自下一帧推进，与游戏侧手调 `request_state` 同款一帧软延迟）。
///
/// 姿势来源三选一（优先级即列出顺序，见各字段文档）：
/// 1. **混合空间模式**（`blend_space_active` 置位）：按 `blend_param` 采样
///    `blend_space` 做 N 路混合（[`blend_pose`]）——`active` 剪辑采样停用；
/// 2. **crossfade**（`blend` 非空）：目标剪辑 + 冻结源快照两路混合（v2 语义）；
/// 3. **纯 active 剪辑**（v1 语义）。
///
/// 根运动（`root_motion_clip == Some(active)` 且 `root_motion_node` 有效）
/// 独立于姿势来源生效：根节点的 Translation 通道从覆写列表中剔除（含
/// crossfade / 混合空间产出的混合结果），改为按 [`root_motion_delta`]
/// 累积帧增量进 `pending_root_motion`——**crossfade 期间 delta 仅取目标
/// 剪辑**（冻结源快照的根位移被丢弃，v1 简化：根位移是游戏玩法量、
/// 不应随表现层过渡回放）。事件轨道不随混合空间停用：共享时间轴仍按
/// active 剪辑推进，事件照常检测（脚步声等玩法事件与姿势来源正交）。
///
/// 只读世界签名（`&World`）：组件写访问走世界内部的尝试型锁，
/// 因此可注册为**并行系统**（访问集：写 AnimationPlayer / Transform，读 Time）。
pub fn update_animations(world: &iris_core::world::World) {
    let dt = world
        .try_resource::<Time>()
        .map(|t| t.delta_secs())
        .unwrap_or(0.0);

    // 阶段一（只读采样）：推进时间、做事件跨界检测、按需混合，
    // 收集为覆写任务
    let mut jobs: Vec<AnimationJob> = Vec::new();
    world.query::<(Entity, &AnimationPlayer)>(|(entity, player)| {
        // 事件检测要用推进前的时间（先取再推进）
        let prev_time = player.time;
        // 混合推进与目标剪辑播放同步：暂停时整段冻结（含 crossfade）
        let blend_dt = if player.playing { dt } else { 0.0 };
        let (time, playing) = player.advanced(dt);
        let looping = player.effective_looping();
        // 循环时 collect_events 需要取模前的原始时间（multi-fire 检测）；
        // 非循环 advanced 已钳制，直接用
        let raw_time = if playing && looping {
            prev_time + dt * player.effective_speed()
        } else {
            time
        };
        let fired = player
            .clips
            .get(player.active)
            .map(|clip| {
                collect_events(&clip.events, prev_time, raw_time, clip.duration, looping)
                    .into_iter()
                    .map(|i| clip.events[i].name.clone())
                    .collect()
            })
            .unwrap_or_default();
        // 参数曲线采样（R74）：用推进后的播放时间（窗口/曲线的「当前
        // 值」语义 = 本帧新值；循环时 time 已取模）
        let parameter_values = player
            .clips
            .get(player.active)
            .map(|clip| {
                clip.curves
                    .iter()
                    .map(|track| (track.name.clone(), sample_track(&track.keys, time)))
                    .collect()
            })
            .unwrap_or_default();
        let Some(clip) = player.clips.get(player.active) else {
            jobs.push((
                entity,
                time,
                playing,
                Vec::new(),
                fired,
                None,
                parameter_values,
            ));
            return;
        };
        // 姿势来源：混合空间激活时按参数点 N 路采样，否则纯 active 剪辑
        let mut fields = if player.blend_space_active {
            let inputs: Vec<(usize, f32, f32)> = player
                .blend_space
                .as_ref()
                .map(|space| {
                    space
                        .sample(player.blend_param)
                        .into_iter()
                        .map(|(index, weight)| (index, weight, time))
                        .collect()
                })
                .unwrap_or_default();
            blend_pose(&player.clips, &player.node_entities, &inputs)
        } else {
            let mut fields = Vec::with_capacity(clip.channels.len());
            for channel in &clip.channels {
                let target = player.node_entities.get(channel.node).copied().flatten();
                let Some(target) = target else {
                    continue;
                };
                fields.push((target, channel.sample(time)));
            }
            fields
        };
        // crossfade 混合：对「目标实体 + 路径」并集逐项混入冻结的源快照。
        // 混合用的 elapsed 加上 dt（与 time 的推进对称，两阶段只读），
        // 保证姿势只由过渡总时长决定、与分帧方式无关
        if let Some(blend) = player.blend {
            if let Some(src_clip) = player.clips.get(blend.from_clip) {
                let w = blend_weight(blend.elapsed + blend_dt, blend.duration);
                // 源快照采样（源时间冻结在 transition 时刻）
                let mut src: Vec<(Entity, AnimationPath, AnimatedField)> = src_clip
                    .channels
                    .iter()
                    .filter_map(|c| {
                        let target = player.node_entities.get(c.node).copied().flatten()?;
                        Some((target, c.path, c.sample(blend.from_time)))
                    })
                    .collect();
                // 双方都有：混合
                for (target, field) in fields.iter_mut() {
                    let path = animated_path(field);
                    if let Some(pos) = src.iter().position(|(e, p, _)| *e == *target && *p == path)
                    {
                        let src_field = src.remove(pos).2;
                        *field = blend_fields(&src_field, field, w);
                    }
                }
                // 仅源有：目标剪辑没有的通道不拖拽——rest 已捕获时从源值
                // 平滑淡回基姿势（w = 1 时恰为 rest；过渡完成 blend 清除后
                // 该通道不再覆写、Transform 停在 rest，与「未被动画的字段
                // 保持当前值」自洽，完成瞬间无跳变）；rest 未捕获保持 v2
                // 语义（保持源姿势，过渡后定格在最后的源值）
                for (target, path, field) in src {
                    let value = match player.rest.iter().find(|(entity, rest_field)| {
                        *entity == target && animated_path(rest_field) == path
                    }) {
                        Some((_, rest_field)) => blend_fields(&field, rest_field, w),
                        None => field,
                    };
                    fields.push((target, value));
                }
            }
        }
        // 附加层（v4）：主姿势（现行模式的混合结果）算完后叠加增量。
        // 叠加放在根运动剔除**之前**——根运动启用时根节点的附加平移
        // 增量同样从覆写列表剔除（根位移是玩法量，附加层不得越权直写）
        if let Some(add) = player.additive.as_ref() {
            if player.clips.get(add.clip).is_none() {
                debug_assert!(
                    false,
                    "附加层剪辑越界：{} / {}",
                    add.clip,
                    player.clips.len()
                );
            } else if add.weight > 0.0 {
                let add_clip = &player.clips[add.clip];
                if add_clip.duration > 0.0 {
                    // 采样用的推进时间与阶段二落账（advanced_additive_time）
                    // 同式：暂停冻结、循环取模、非循环钳制——与 crossfade
                    // 的「姿势只由时间决定、与分帧方式无关」同一纪律
                    let add_dt = if player.playing { dt * add.speed } else { 0.0 };
                    let t = if add.looping {
                        (add.time + add_dt) % add_clip.duration
                    } else {
                        (add.time + add_dt).min(add_clip.duration)
                    };
                    apply_additive(&mut fields, &player.clips, &player.node_entities, add, t);
                }
            }
        }
        // 遮罩过滤（v4）：覆写过滤器，与姿势来源正交——目标节点不在
        // 遮罩集合内的任务整条剔除（该节点保持当前值 / 父姿势）。附加层
        // 产出的覆写同样被过滤（叠加发生在过滤之前，语义见 AnimationMask）
        if let Some(mask) = &player.mask {
            fields.retain(|(target, _)| {
                node_allows_overwrite(&player.node_entities, &mask.nodes, *target)
            });
        }
        // 根运动（v3）：启用中的根节点平移从覆写列表剔除（含混合结果），
        // 改为累积本帧位移增量（剪辑空间；crossfade 期间仅取目标剪辑）
        let mut root_delta = None;
        if player.root_motion_clip == Some(player.active) {
            if let Some(node) = player.root_motion_node {
                if let Some(root_target) = player.node_entities.get(node).copied().flatten() {
                    fields.retain(|(target, field)| {
                        !(*target == root_target && matches!(field, AnimatedField::Translation(_)))
                    });
                    let delta: Vec3 = clip
                        .channels
                        .iter()
                        .filter(|c| c.node == node && c.path == AnimationPath::Translation)
                        .map(|c| root_motion_delta(c, prev_time, raw_time, clip.duration, looping))
                        .fold(Vec3::ZERO, |acc, d| acc + d);
                    root_delta = Some(delta);
                }
            }
        }
        jobs.push((
            entity,
            time,
            playing,
            fields,
            fired,
            root_delta,
            parameter_values,
        ));
    });

    // 阶段二（写入）：更新播放状态，覆写目标实体的局部变换
    for (player_entity, time, playing, fields, events, root_delta, parameter_values) in jobs {
        if let Some(mut player) = world.get_mut::<AnimationPlayer>(player_entity) {
            // 读到的是推进前的 playing：暂停时混合计时同步冻结
            let was_playing = player.playing;
            player.time = time;
            player.playing = playing;
            player.pending_events.extend(events);
            player.parameter_values = parameter_values;
            // 根运动增量累积（消费方 take_root_motion 取走；一帧一增量，
            // 已有累积时叠加——多系统同帧写同一播放器的防御性合并）
            if let Some(delta) = root_delta {
                player.pending_root_motion =
                    Some(player.pending_root_motion.unwrap_or_default() + delta);
            }
            // 推进 crossfade 计时；完成后清除，回到纯目标姿势
            if was_playing {
                if let Some(blend) = player.blend.as_mut() {
                    blend.elapsed += dt;
                    if blend.elapsed >= blend.duration {
                        player.blend = None;
                    }
                }
                // 附加层时间推进（与阶段一采样同式，见 update_animations
                // 阶段一注释）；剪辑越界的层时长按 0 处理 = 时间不动
                let additive_duration = player
                    .additive
                    .as_ref()
                    .and_then(|add| player.clips.get(add.clip))
                    .map(|clip| clip.duration)
                    .unwrap_or(0.0);
                if let Some(add) = player.additive.as_mut() {
                    add.time = add.advanced_additive_time(dt, additive_duration);
                }
            }
            // 状态机 v2 条件步检（C4b）：本帧已落账的 time / blend 之后
            // 执行——ClipFinished 守卫看到新时间；触发的 enter_state
            // 会把 time 归零并开新 blend，下一帧起姿势走新状态过渡。
            player.step_state_machine();
        }
        for (target, field) in fields {
            let Some(mut transform) = world.get_mut::<Transform>(target) else {
                continue;
            };
            match field {
                AnimatedField::Translation(v) => transform.position = v.into(),
                AnimatedField::Rotation(q) => transform.rotation = q,
                AnimatedField::Scale(v) => transform.scale = v.into(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;
    use iris_core::world::World;
    use std::time::Duration;

    fn translation_channel(interpolation: AnimationInterpolation) -> AnimationChannel {
        AnimationChannel {
            node: 0,
            path: AnimationPath::Translation,
            interpolation,
            times: vec![0.0, 1.0, 2.0],
            values: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 2.0, 0.0, 0.0],
        }
    }

    /// 恒值平移通道（单关键帧）。
    fn constant_translation(node: usize, v: [f32; 3]) -> AnimationChannel {
        AnimationChannel {
            node,
            path: AnimationPath::Translation,
            interpolation: AnimationInterpolation::Linear,
            times: vec![0.0],
            values: v.to_vec(),
        }
    }

    /// 恒值旋转通道（单关键帧）。
    fn constant_rotation(node: usize, q: Quat) -> AnimationChannel {
        AnimationChannel {
            node,
            path: AnimationPath::Rotation,
            interpolation: AnimationInterpolation::Linear,
            times: vec![0.0],
            values: vec![q.x, q.y, q.z, q.w],
        }
    }

    fn clip(name: &str, channels: Vec<AnimationChannel>, duration: f32) -> AnimationClip {
        AnimationClip {
            name: name.into(),
            channels,
            duration,
            events: Vec::new(),

            windows: Vec::new(),
            curves: Vec::new(),
        }
    }

    /// 建「双剪辑 + 播放器」世界：root 实体挂播放器，node 0 指向 root。
    fn blend_world(clip_a: AnimationClip, clip_b: AnimationClip) -> (World, Entity) {
        let mut world = World::new();
        let root = world.spawn((Transform::default(),));
        let player = AnimationPlayer::new(vec![clip_a, clip_b], vec![Some(root)]);
        world.insert(root, player);
        (world, root)
    }

    /// 确定性推进 Time 并跑一帧动画系统。
    fn step(world: &World, dt: f64) {
        world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(dt));
        update_animations(world);
    }

    fn sample_translation(channel: &AnimationChannel, t: f32) -> [f32; 3] {
        match channel.sample(t) {
            AnimatedField::Translation(v) => v,
            _ => panic!("应为平移通道"),
        }
    }

    #[test]
    fn linear_interpolates_between_keys() {
        let ch = translation_channel(AnimationInterpolation::Linear);
        assert_eq!(sample_translation(&ch, 0.5), [0.5, 0.0, 0.0]);
        assert_eq!(sample_translation(&ch, 1.5), [1.5, 0.0, 0.0]);
    }

    #[test]
    fn sample_clamps_outside_range() {
        let ch = translation_channel(AnimationInterpolation::Linear);
        assert_eq!(sample_translation(&ch, -1.0), [0.0, 0.0, 0.0]);
        assert_eq!(sample_translation(&ch, 9.0), [2.0, 0.0, 0.0]);
    }

    #[test]
    fn step_holds_previous_key() {
        let ch = translation_channel(AnimationInterpolation::Step);
        assert_eq!(sample_translation(&ch, 0.99), [0.0, 0.0, 0.0]);
        assert_eq!(sample_translation(&ch, 1.0), [1.0, 0.0, 0.0]);
    }

    #[test]
    fn single_key_channel_is_constant() {
        let ch = AnimationChannel {
            node: 0,
            path: AnimationPath::Translation,
            interpolation: AnimationInterpolation::Linear,
            times: vec![0.5],
            values: vec![1.0, 2.0, 3.0],
        };
        assert_eq!(sample_translation(&ch, 0.0), [1.0, 2.0, 3.0]);
        assert_eq!(sample_translation(&ch, 10.0), [1.0, 2.0, 3.0]);
    }

    #[test]
    fn rotation_slerp_takes_shortest_path() {
        let ch = AnimationChannel {
            node: 0,
            path: AnimationPath::Rotation,
            interpolation: AnimationInterpolation::Linear,
            times: vec![0.0, 1.0],
            values: vec![
                0.0,
                0.0,
                0.0,
                1.0, // 单位四元数（yaw 0°）
                0.0,
                std::f32::consts::FRAC_1_SQRT_2,
                0.0,
                std::f32::consts::FRAC_1_SQRT_2, // yaw 90°
            ],
        };
        match ch.sample(0.5) {
            AnimatedField::Rotation(q) => {
                let expected = glam::Quat::from_rotation_y(45f32.to_radians());
                // 四元数双覆盖：取与期望夹角的一半判断
                let dot = (q.dot(expected)).abs();
                assert!(
                    (dot - 1.0).abs() < 1e-4,
                    "0.5s 应为 yaw 45°，实际 {q:?} vs {expected:?}"
                );
            }
            _ => panic!("应为旋转通道"),
        }
    }

    #[test]
    fn player_loops_and_stops() {
        let clip = AnimationClip {
            name: "c".into(),
            channels: vec![],
            duration: 1.0,
            events: Vec::new(),

            windows: Vec::new(),
            curves: Vec::new(),
        };
        let mut player = AnimationPlayer::new(vec![clip], vec![]);
        assert_eq!(player.advanced(0.3), (0.3, true));
        player.time = 0.9;
        let (t, playing) = player.advanced(0.3);
        assert!((t - 0.2).abs() < 1e-5 && playing, "循环应回绕，实际 {t}");
        player.time = t;
        player.looping = false;
        let (t, playing) = player.advanced(0.9);
        assert!(
            (t - 1.0).abs() < 1e-5 && !playing,
            "非循环应停在末尾，实际 {t}"
        );
        player.time = t;
        player.playing = playing;
        assert_eq!(player.advanced(0.3), (1.0, false), "暂停不应推进");
    }

    #[test]
    fn blend_weight_is_smoothstep() {
        assert_eq!(blend_weight(0.0, 1.0), 0.0);
        assert_eq!(blend_weight(1.0, 1.0), 1.0);
        // smoothstep(0.5) = 0.25 · (3 − 1) = 0.5：中点与线性一致
        assert_eq!(blend_weight(0.5, 1.0), 0.5);
        // smoothstep(0.25) = 0.0625 · 2.5
        assert!((blend_weight(0.25, 1.0) - 0.15625).abs() < 1e-6);
        // 越界钳制
        assert_eq!(blend_weight(2.0, 1.0), 1.0);
        assert_eq!(blend_weight(-1.0, 1.0), 0.0);
    }

    #[test]
    fn crossfade_translation_midpoint() {
        let a = clip("a", vec![constant_translation(0, [0.0, 0.0, 0.0])], 1.0);
        let b = clip("b", vec![constant_translation(0, [2.0, 0.0, 0.0])], 1.0);
        let (mut world, root) = blend_world(a, b);
        world.insert_resource(Time::default());
        world
            .get_mut::<AnimationPlayer>(root)
            .unwrap()
            .transition(1, 1.0);
        assert!(world.get::<AnimationPlayer>(root).unwrap().blend.is_some());

        // max_delta = 0.25s 钳制，分两帧共推进 0.5s；混合权重应与
        // 分帧方式无关：w = smoothstep(0.5) = 0.5
        step(&world, 0.25);
        step(&world, 0.25);
        let transform = world.get::<Transform>(root).unwrap();
        // 精确式：x = 0 + (2 − 0)·smoothstep(0.5) = 2·blend_weight(0.5, 1)
        let expected_x = 2.0 * blend_weight(0.5, 1.0);
        assert!(
            (transform.position.x - expected_x).abs() < 1e-6,
            "中点 x 应为 {expected_x}，实际 {}",
            transform.position.x
        );
        let player = world.get::<AnimationPlayer>(root).unwrap();
        assert!(
            (player.time - 0.5).abs() < 1e-6,
            "目标剪辑时间应推进到 0.5，实际 {}",
            player.time
        );
    }

    #[test]
    fn crossfade_rotation_slerp_midpoint() {
        let a = clip("face", vec![constant_rotation(0, Quat::IDENTITY)], 1.0);
        let b = clip(
            "side",
            vec![constant_rotation(
                0,
                Quat::from_rotation_y(90f32.to_radians()),
            )],
            1.0,
        );
        let (mut world, root) = blend_world(a, b);
        world.insert_resource(Time::default());
        world
            .get_mut::<AnimationPlayer>(root)
            .unwrap()
            .transition(1, 1.0);
        step(&world, 0.25);
        step(&world, 0.25);
        let transform = world.get::<Transform>(root).unwrap();
        let expected = Quat::from_rotation_y(45f32.to_radians());
        // slerp 中点 = 45°（角距容差 1e-5 rad）
        let angle = 2.0
            * transform
                .rotation
                .dot(expected)
                .abs()
                .clamp(0.0, 1.0)
                .acos();
        assert!(
            angle < 1e-5,
            "中点应为 yaw 45°，角距 {angle} rad，实际 {:?}",
            transform.rotation
        );
    }

    #[test]
    fn crossfade_missing_channels_do_not_drag() {
        // A 只有平移、B 只有旋转：混合期平移取 A、旋转取 B
        let a = clip(
            "trans_only",
            vec![constant_translation(0, [2.0, 0.0, 0.0])],
            1.0,
        );
        let b = clip(
            "rot_only",
            vec![constant_rotation(
                0,
                Quat::from_rotation_y(90f32.to_radians()),
            )],
            1.0,
        );
        let (mut world, root) = blend_world(a, b);
        world.insert_resource(Time::default());
        world
            .get_mut::<AnimationPlayer>(root)
            .unwrap()
            .transition(1, 1.0);
        step(&world, 0.25);
        step(&world, 0.25);
        let transform = world.get::<Transform>(root).unwrap();
        assert_eq!(
            [
                transform.position.x,
                transform.position.y,
                transform.position.z
            ],
            [2.0, 0.0, 0.0],
            "平移只有 A 有：应取 A 的值"
        );
        let expected = Quat::from_rotation_y(90f32.to_radians());
        let dot = transform.rotation.dot(expected).abs();
        assert!(
            (dot - 1.0).abs() < 1e-5,
            "旋转只有 B 有：应取 B 的值，实际 {:?}",
            transform.rotation
        );
    }

    /// 摩擦①闭合（rest 已捕获）：「仅源有」的通道在混合期从源值平滑淡回
    /// 基姿势，过渡完成帧恰为 rest 值——blend 清除后该通道不再被任何
    /// 剪辑覆写，Transform 停在 rest（与「未被动画的字段保持当前值」
    /// 自洽），不再定格在源姿势。
    #[test]
    fn crossfade_source_only_channel_settles_to_rest() {
        // A 只有平移 [2,0,0]（源）、B 只有旋转（目标）；rest 平移 = 原点
        let a = clip(
            "trans_only",
            vec![constant_translation(0, [2.0, 0.0, 0.0])],
            1.0,
        );
        let b = clip(
            "rot_only",
            vec![constant_rotation(
                0,
                Quat::from_rotation_y(90f32.to_radians()),
            )],
            1.0,
        );
        let mut world = World::new();
        let root = world.spawn((Transform::default(),));
        let mut player = AnimationPlayer::new(vec![a, b], vec![Some(root)]);
        player.capture_rest(&world);
        world.insert(root, player);
        world.insert_resource(Time::default());
        world
            .get_mut::<AnimationPlayer>(root)
            .unwrap()
            .transition(1, 1.0);
        // 混合中点：平移 = lerp(源 2, rest 0, smoothstep(0.5)=0.5) = 1
        // ——不再恒为源值（旧语义的断言值 2 会在混合期被带走向 rest）
        step(&world, 0.25);
        step(&world, 0.25);
        let x_mid = world.get::<Transform>(root).unwrap().position.x;
        assert!(
            (x_mid - 1.0).abs() < 1e-6,
            "中点应 = lerp(2, rest 0, 0.5) = 1，实际 {x_mid}"
        );
        // 过渡完成：平移 = rest（原点），旋转 = 目标剪辑的值
        for _ in 0..2 {
            step(&world, 0.25);
        }
        {
            let player = world.get::<AnimationPlayer>(root).unwrap();
            assert!(player.blend.is_none(), "过渡完成后 blend 应清除");
            let transform = world.get::<Transform>(root).unwrap();
            assert_eq!(
                transform.position.x, 0.0,
                "完成帧「仅源有」通道应恰为 rest 平移"
            );
            let expected = Quat::from_rotation_y(90f32.to_radians());
            let dot = transform.rotation.dot(expected).abs();
            assert!((dot - 1.0).abs() < 1e-5, "目标侧通道应为目标剪辑的值");
        }
        // blend 清除后：该通道不再被覆写，Transform 停在 rest
        step(&world, 0.25);
        assert_eq!(
            world.get::<Transform>(root).unwrap().position.x,
            0.0,
            "完成后 Transform 应停在 rest"
        );
    }

    /// 摩擦①闭合（连续性）：过渡完成瞬间「仅源有」的通道值连续——
    /// 帧序列按 smoothstep 收敛逼近 rest、完成帧恰为 rest（无跳变）。
    #[test]
    fn crossfade_completion_frame_is_rest_without_jump() {
        let a = clip(
            "trans_only",
            vec![constant_translation(0, [2.0, 0.0, 0.0])],
            1.0,
        );
        let b = clip("rot_only", vec![], 1.0);
        let mut world = World::new();
        let root = world.spawn((Transform::default(),));
        let mut player = AnimationPlayer::new(vec![a, b], vec![Some(root)]);
        player.capture_rest(&world);
        world.insert(root, player);
        world.insert_resource(Time::default());
        world
            .get_mut::<AnimationPlayer>(root)
            .unwrap()
            .transition(1, 1.0);
        // 0.25 s × 4 = 1.0 s：第 4 帧为完成帧
        let mut xs = Vec::new();
        for _ in 0..4 {
            step(&world, 0.25);
            xs.push(world.get::<Transform>(root).unwrap().position.x);
        }
        // x = 2·(1 − smoothstep(k/4))：单调收敛，完成帧 = rest = 0
        assert_eq!(xs[3], 0.0, "完成帧应恰为 rest 平移 0");
        assert!(xs[2] > 0.0, "完成前一帧应仍未到 rest");
        assert!(
            (xs[2] - xs[3]) < (xs[1] - xs[2]),
            "帧间步幅应按 smoothstep 收敛（无跳变）：{xs:?}"
        );
    }

    /// 兼容性承诺：rest 未捕获（纯构造播放器）时保持 v2 语义——「仅源
    /// 有」通道过渡期恒取源值、完成后定格在最后的源值。
    #[test]
    fn crossfade_without_rest_keeps_v2_freeze_semantics() {
        let a = clip(
            "trans_only",
            vec![constant_translation(0, [2.0, 0.0, 0.0])],
            1.0,
        );
        let b = clip("rot_only", vec![], 1.0);
        let (mut world, root) = blend_world(a, b);
        world.insert_resource(Time::default());
        world
            .get_mut::<AnimationPlayer>(root)
            .unwrap()
            .transition(1, 1.0);
        for _ in 0..4 {
            step(&world, 0.25);
        }
        assert!(world.get::<AnimationPlayer>(root).unwrap().blend.is_none());
        assert_eq!(
            world.get::<Transform>(root).unwrap().position.x,
            2.0,
            "rest 未捕获：完成后应定格在源值（v2 语义不变）"
        );
    }

    /// rest 是捕获时刻的快照：捕获后 Transform 被其它剪辑改过，混合仍
    /// 淡回捕获时记录的值（模块文档写明的诚实边界）。
    #[test]
    fn rest_pose_is_capture_time_snapshot() {
        let a = clip(
            "trans_only",
            vec![constant_translation(0, [2.0, 0.0, 0.0])],
            1.0,
        );
        let b = clip("rot_only", vec![], 1.0);
        let mut world = World::new();
        let root = world.spawn((Transform::from_xyz(5.0, 0.0, 0.0),));
        let mut player = AnimationPlayer::new(vec![a, b], vec![Some(root)]);
        player.capture_rest(&world); // rest 平移 = (5, 0, 0)
        world.insert(root, player);
        // 捕获后 Transform 被改（模拟其它系统动过）
        world.get_mut::<Transform>(root).unwrap().position = Vec3::ZERO;
        world.insert_resource(Time::default());
        world
            .get_mut::<AnimationPlayer>(root)
            .unwrap()
            .transition(1, 1.0);
        for _ in 0..4 {
            step(&world, 0.25);
        }
        assert_eq!(
            world.get::<Transform>(root).unwrap().position.x,
            5.0,
            "混合应淡回捕获时刻的 rest（5），而非当前值（0）"
        );
    }

    /// 零四元数（glTF 节点缺省 rotation 的加载产物）不可作为 slerp
    /// 端点：capture_rest 按恒等捕获。
    #[test]
    fn capture_rest_sanitizes_degenerate_rotation() {
        // 源只有旋转（会被淡回 rest 旋转）、目标空剪辑
        let a = clip(
            "rot_only",
            vec![constant_rotation(
                0,
                Quat::from_rotation_y(90f32.to_radians()),
            )],
            1.0,
        );
        let b = clip("none", vec![], 1.0);
        let mut world = World::new();
        // 模拟 GLB 加载产物：rotation 缺省 → 零四元数
        let root = world.spawn((Transform {
            position: Vec3::ONE,
            rotation: Quat::from_xyzw(0.0, 0.0, 0.0, 0.0),
            scale: Vec3::ONE,
        },));
        let mut player = AnimationPlayer::new(vec![a, b], vec![Some(root)]);
        player.capture_rest(&world);
        let rest_rotation = player
            .rest
            .iter()
            .find_map(|(_, field)| match field {
                AnimatedField::Rotation(q) => Some(*q),
                _ => None,
            })
            .expect("rest 应含旋转通道");
        assert!(
            (rest_rotation.dot(Quat::IDENTITY) - 1.0).abs() < 1e-6,
            "零四元数应按恒等捕获，实际 {rest_rotation:?}"
        );
        // 淡回 rest 的 slerp 端点合法：完整过渡不产生 NaN
        world.insert(root, player);
        world.insert_resource(Time::default());
        world
            .get_mut::<AnimationPlayer>(root)
            .unwrap()
            .transition(1, 1.0);
        for _ in 0..4 {
            step(&world, 0.25);
        }
        let rotation = world.get::<Transform>(root).unwrap().rotation;
        assert!(
            rotation.length_squared() > 0.5,
            "完成后旋转应停在合法的 rest 恒等（无 NaN），实际 {rotation:?}"
        );
    }

    #[test]
    fn crossfade_completes_to_pure_target() {
        let a = clip("a", vec![constant_translation(0, [0.0, 0.0, 0.0])], 1.0);
        let b = clip("b", vec![constant_translation(0, [2.0, 0.0, 0.0])], 1.0);
        let (mut world, root) = blend_world(a, b);
        world.insert_resource(Time::default());
        world
            .get_mut::<AnimationPlayer>(root)
            .unwrap()
            .transition(1, 1.0);
        // 共推进 1.0s ≥ duration：blend 清除，姿势为纯目标
        for _ in 0..4 {
            step(&world, 0.25);
        }
        assert!(
            world.get::<AnimationPlayer>(root).unwrap().blend.is_none(),
            "过渡完成后 blend 应清除"
        );
        let x = world.get::<Transform>(root).unwrap().position.x;
        assert_eq!(x, 2.0, "应为纯目标姿势");
        // 继续推进保持纯目标
        step(&world, 0.25);
        assert!(world.get::<AnimationPlayer>(root).unwrap().blend.is_none());
        assert_eq!(world.get::<Transform>(root).unwrap().position.x, 2.0);
    }

    #[test]
    fn pause_freezes_blend() {
        let a = clip("a", vec![constant_translation(0, [0.0, 0.0, 0.0])], 1.0);
        let b = clip("b", vec![constant_translation(0, [2.0, 0.0, 0.0])], 1.0);
        let (mut world, root) = blend_world(a, b);
        world.insert_resource(Time::default());
        world
            .get_mut::<AnimationPlayer>(root)
            .unwrap()
            .transition(1, 1.0);
        step(&world, 0.25); // w = smoothstep(0.25)
        let x0 = world.get::<Transform>(root).unwrap().position.x;
        // 暂停：混合计时应与播放时间一起冻结，姿势不再变化
        world.get_mut::<AnimationPlayer>(root).unwrap().playing = false;
        step(&world, 0.25);
        step(&world, 0.25);
        let elapsed = world
            .get::<AnimationPlayer>(root)
            .unwrap()
            .blend
            .unwrap()
            .elapsed;
        assert_eq!(elapsed, 0.25, "暂停时 blend 计时应冻结");
        let x1 = world.get::<Transform>(root).unwrap().position.x;
        assert_eq!(x0.to_bits(), x1.to_bits(), "暂停时姿势应逐位不动");
    }

    #[test]
    fn transition_hard_switch_semantics() {
        let a = clip("a", vec![constant_translation(0, [0.0, 0.0, 0.0])], 1.0);
        let b = clip("b", vec![constant_translation(0, [2.0, 0.0, 0.0])], 1.0);
        let (mut world, root) = blend_world(a, b);
        world.insert_resource(Time::default());
        step(&world, 0.25);
        // duration = 0：硬切换
        world
            .get_mut::<AnimationPlayer>(root)
            .unwrap()
            .transition(1, 0.0);
        {
            let player = world.get::<AnimationPlayer>(root).unwrap();
            assert!(player.blend.is_none(), "硬切换不应有 blend");
            assert_eq!(player.active, 1);
            assert_eq!(player.time, 0.0, "硬切换后时间归零");
        }
        // 同剪辑请求忽略
        world
            .get_mut::<AnimationPlayer>(root)
            .unwrap()
            .transition(1, 1.0);
        assert!(world.get::<AnimationPlayer>(root).unwrap().blend.is_none());
    }

    #[test]
    fn state_machine_edge_starts_crossfade() {
        let idle = clip("idle", vec![constant_translation(0, [0.0, 0.0, 0.0])], 1.0);
        let walk = clip("walk", vec![constant_translation(0, [2.0, 0.0, 0.0])], 1.0);
        let mut player = AnimationPlayer::new(vec![idle, walk], vec![]);
        player.states = vec![
            AnimationState {
                clip: 0,
                looping: true,
                speed: 1.0,
            },
            AnimationState {
                clip: 1,
                looping: false,
                speed: 2.0,
            },
        ];
        player.edges = vec![AnimationEdge {
            from: 0,
            to: 1,
            duration: 0.3,
        }];
        player.request_state(1);
        assert!(player.blend.is_some(), "有边应启动 crossfade");
        assert_eq!(player.blend.unwrap().duration, 0.3, "过渡时长取边值");
        assert_eq!(player.current_state, 1);
        assert_eq!(player.active, 1, "应切到目标态的剪辑");
        assert!(!player.looping, "looping 应切到目标态");
        assert!((player.speed - 2.0).abs() < 1e-6, "speed 应切到目标态");
        // states 非空：advanced 以当前状态的 looping/speed 为准
        // （walk 态 speed=2.0、非循环：0 + 0.4·2 = 0.8）
        assert_eq!(player.advanced(0.4), (0.8, true));
        // 无边：硬切换（清掉边表后切回 idle）
        player.edges.clear();
        player.request_state(0);
        assert!(player.blend.is_none(), "无边应硬切换");
        assert_eq!(player.active, 0);
        assert_eq!(player.time, 0.0);
        assert!(player.looping, "应切回 idle 的循环参数");
        assert!((player.speed - 1.0).abs() < 1e-6);
    }

    #[test]
    #[should_panic(expected = "目标状态越界")]
    fn request_state_out_of_range_panics() {
        let mut player = AnimationPlayer::new(vec![], vec![]);
        player.states = vec![AnimationState {
            clip: 0,
            looping: true,
            speed: 1.0,
        }];
        player.request_state(5);
    }

    #[test]
    fn events_fire_once_within_interval() {
        let events = vec![
            AnimationEvent {
                time: 0.5,
                name: "a".into(),
            },
            AnimationEvent {
                time: 0.95,
                name: "b".into(),
            },
        ];
        // 精确命中：0.4 → 0.6 命中 t=0.5
        assert_eq!(collect_events(&events, 0.4, 0.6, 1.0, false), vec![0]);
        // 未达不触发
        assert!(collect_events(&events, 0.0, 0.49, 1.0, false).is_empty());
        // 越过后不重复
        assert!(collect_events(&events, 0.6, 0.9, 1.0, false).is_empty());
        // 边界：命中区间 (prev, new] 恰好含 new
        assert_eq!(collect_events(&events, 0.4, 0.5, 1.0, false), vec![0]);
        // 非循环停在末端：prev == new == duration，区间为空，不重复触发
        assert!(collect_events(&events, 1.0, 1.0, 1.0, false).is_empty());
        // 末端事件：推进到末端那一帧触发一次
        let end = vec![AnimationEvent {
            time: 1.0,
            name: "end".into(),
        }];
        assert_eq!(collect_events(&end, 0.9, 1.0, 1.0, false), vec![0]);
        assert!(collect_events(&end, 1.0, 1.0, 1.0, false).is_empty());
    }

    #[test]
    fn looping_events_wrap_and_multifire() {
        // 事件表故意乱序：输出必须按触发时刻排序
        let events = vec![
            AnimationEvent {
                time: 0.95,
                name: "late".into(),
            },
            AnimationEvent {
                time: 0.05,
                name: "early".into(),
            },
        ];
        // 回绕：prev=0.9 → 原始时间 1.1（显示 0.1），
        // 命中 (0.9, 1] 的 0.95 与 [0, 0.1] 的 0.05，按触发时刻排序
        assert_eq!(collect_events(&events, 0.9, 1.1, 1.0, true), vec![0, 1]);
        // 一帧两圈 multi-fire：prev=0.1 → 原始时间 2.15，t=0.5 被跨越两次
        let mid = vec![AnimationEvent {
            time: 0.5,
            name: "m".into(),
        }];
        assert_eq!(
            collect_events(&mid, 0.1, 2.15, 1.0, true),
            vec![0, 0],
            "一帧跨两圈应重复命中两次"
        );
        // 恰在 prev 处的事件不重复触发（上一帧已触发）
        assert!(collect_events(&mid, 0.5, 0.9, 1.0, true).is_empty());
    }

    #[test]
    fn events_flow_through_update_and_drain() {
        let mut clip_a = clip("walk", vec![], 1.0);
        clip_a.events = vec![
            AnimationEvent {
                time: 0.25,
                name: "footstep".into(),
            },
            AnimationEvent {
                time: 0.75,
                name: "footstep2".into(),
            },
        ];
        let (mut world, root) = blend_world(clip_a, clip("idle", vec![], 1.0));
        world.insert_resource(Time::default());
        step(&world, 0.25);
        assert_eq!(
            world.get::<AnimationPlayer>(root).unwrap().pending_events,
            vec!["footstep"],
            "0.25s 事件应在第一帧命中"
        );
        // drain 消费
        let taken = world
            .get_mut::<AnimationPlayer>(root)
            .unwrap()
            .take_events();
        assert_eq!(taken, vec!["footstep"]);
        assert!(
            world
                .get::<AnimationPlayer>(root)
                .unwrap()
                .pending_events
                .is_empty()
        );
        // 第二帧（0.25 → 0.30）：无跨越，不重复触发
        step(&world, 0.05);
        assert!(
            world
                .get::<AnimationPlayer>(root)
                .unwrap()
                .pending_events
                .is_empty()
        );
        // 循环回绕经系统：0.9 → 原始 1.1（显示 0.1）。
        // 命中区间 (0.9, 1] ∪ [0, 0.1] 不含 0.25 / 0.75 → 无误报
        world.get_mut::<AnimationPlayer>(root).unwrap().time = 0.9;
        step(&world, 0.2);
        assert!(
            world
                .get::<AnimationPlayer>(root)
                .unwrap()
                .pending_events
                .is_empty(),
            "回绕帧不应误报"
        );
        // 跨圈命中：0.75 → 原始 1.25（显示 0.25），0.25 事件在新一圈再触发
        world.get_mut::<AnimationPlayer>(root).unwrap().time = 0.75;
        step(&world, 0.25); // 原始 1.0：区间 (0.75, 1]，无命中
        assert!(
            world
                .get::<AnimationPlayer>(root)
                .unwrap()
                .pending_events
                .is_empty()
        );
        step(&world, 0.25); // 原始 1.25：跨过 1.25 处的 0.25 → 命中
        assert_eq!(
            world.get::<AnimationPlayer>(root).unwrap().pending_events,
            vec!["footstep"],
            "回绕后事件应在新的圈里再次触发"
        );
    }

    #[test]
    fn blend_is_bitwise_deterministic() {
        let build = || {
            let (a, d) = (constant_translation(0, [0.0, 1.0, 0.0]), 1.0);
            let b = constant_translation(0, [2.0, 3.0, 0.0]);
            let (mut world, root) = blend_world(clip("a", vec![a], d), clip("b", vec![b], 1.0));
            world.insert_resource(Time::default());
            world
                .get_mut::<AnimationPlayer>(root)
                .unwrap()
                .transition(1, 1.0);
            (world, root)
        };
        let (world_a, root_a) = build();
        let (world_b, root_b) = build();
        for _ in 0..3 {
            step(&world_a, 0.25);
            step(&world_b, 0.25);
            let ta = world_a.get::<Transform>(root_a).unwrap();
            let tb = world_b.get::<Transform>(root_b).unwrap();
            assert_eq!(
                [
                    ta.position.x.to_bits(),
                    ta.position.y.to_bits(),
                    ta.position.z.to_bits()
                ],
                [
                    tb.position.x.to_bits(),
                    tb.position.y.to_bits(),
                    tb.position.z.to_bits()
                ],
                "同配置两个播放器应逐位一致"
            );
        }
    }

    // ---- BlendSpace2D（v3）----

    /// 循环直线走位剪辑：node 0 平移 0 → 1 → 2 米 over 2 秒（复用既有
    /// 测试构造器，速度 1 m/s）。
    fn walk_clip() -> AnimationClip {
        clip(
            "walk",
            vec![translation_channel(AnimationInterpolation::Linear)],
            2.0,
        )
    }

    #[test]
    fn blendspace_exact_hit_gives_full_weight() {
        let space = BlendSpace {
            samples: vec![(Vec2::new(0.0, 0.0), 0), (Vec2::new(1.0, 0.0), 1)],
        };
        assert_eq!(
            space.sample(Vec2::new(1.0, 0.0)),
            vec![(1, 1.0)],
            "参数点恰等于样本点 → 该点权重 1"
        );
        assert_eq!(space.sample(Vec2::new(0.0, 0.0)), vec![(0, 1.0)]);
        // 空点集：空表
        let empty = BlendSpace { samples: vec![] };
        assert!(empty.sample(Vec2::ZERO).is_empty());
    }

    #[test]
    fn blendspace_axis_midpoint_halves() {
        let space = BlendSpace {
            samples: vec![(Vec2::ZERO, 0), (Vec2::new(1.0, 0.0), 1)],
        };
        assert_eq!(
            space.sample(Vec2::new(0.5, 0.0)),
            vec![(0, 0.5), (1, 0.5)],
            "轴中点两邻各 0.5"
        );
    }

    #[test]
    fn blendspace_1d_chain_lerp_and_clamp() {
        // 所有样本同 y → 1D 链式 lerp（无需特判的退化路径）
        let space = BlendSpace {
            samples: vec![
                (Vec2::new(0.0, 0.0), 0),
                (Vec2::new(1.0, 0.0), 1),
                (Vec2::new(2.0, 0.0), 2),
            ],
        };
        assert_eq!(
            space.sample(Vec2::new(0.25, 0.0)),
            vec![(0, 0.75), (1, 0.25)],
            "链式 lerp：两侧邻按 x 距离分权"
        );
        assert_eq!(
            space.sample(Vec2::new(1.5, 0.0)),
            vec![(1, 0.5), (2, 0.5)],
            "中点两邻各 0.5"
        );
        // 范围外钳到最近端（权重 1）
        assert_eq!(space.sample(Vec2::new(-1.0, 0.0)), vec![(0, 1.0)]);
        assert_eq!(space.sample(Vec2::new(5.0, 0.0)), vec![(2, 1.0)]);
    }

    #[test]
    fn blendspace_grid_bilinear_exact() {
        let space = BlendSpace {
            samples: vec![
                (Vec2::new(0.0, 0.0), 0),
                (Vec2::new(1.0, 0.0), 1),
                (Vec2::new(0.0, 1.0), 2),
                (Vec2::new(1.0, 1.0), 3),
            ],
        };
        // 参数 (0.25, 0.75)：x 列权重 (0.75, 0.25)、y 行权重 (0.25, 0.75)
        // → 权重 = 各轴插值系数乘积（轴对齐双线性），输出按「左列前行 →
        // 左列后行 → 右列前行 → 右列后行」固定顺序
        assert_eq!(
            space.sample(Vec2::new(0.25, 0.75)),
            vec![
                (0, 0.75 * 0.25),
                (2, 0.75 * 0.75),
                (1, 0.25 * 0.25),
                (3, 0.25 * 0.75),
            ],
            "满网格应为标准双线性"
        );
    }

    /// 权重不变量：非负、和恒为 1、至多 4 路——确定性「随机」参数网格
    /// 遍历（坐标写死，无随机源）。
    #[test]
    fn blendspace_weights_nonnegative_and_sum_to_one() {
        let space = BlendSpace {
            samples: vec![
                (Vec2::new(0.0, 0.0), 0),
                (Vec2::new(1.0, 0.0), 1),
                (Vec2::new(0.0, 1.0), 2),
                (Vec2::new(1.0, 1.0), 1),
                (Vec2::new(2.0, 0.5), 0),
            ],
        };
        for i in 0..64u32 {
            let param = Vec2::new((i % 8) as f32 * 0.313 - 1.0, (i / 8) as f32 * 0.271 - 1.0);
            let out = space.sample(param);
            assert!(
                out.len() <= 4,
                "至多 4 路非零权重：参数 {param:?} 得 {out:?}"
            );
            let mut sum = 0.0f32;
            for &(clip_index, weight) in &out {
                assert!(
                    (0.0..=1.0).contains(&weight),
                    "权重应在 [0,1]：参数 {param:?} 的 ({clip_index}, {weight})"
                );
                sum += weight;
            }
            assert!(
                (sum - 1.0).abs() < 1e-4,
                "权重和应恒为 1：参数 {param:?} 和 {sum}"
            );
        }
    }

    #[test]
    fn nway_rotation_blend_yaw_midpoint() {
        // yaw 0/45/90 等权 → nlerp 中间值 45°
        let inputs = [
            (Quat::from_rotation_y(0.0), 1.0 / 3.0),
            (Quat::from_rotation_y(45f32.to_radians()), 1.0 / 3.0),
            (Quat::from_rotation_y(90f32.to_radians()), 1.0 / 3.0),
        ];
        let q = blend_rotation_nway(&inputs);
        let expected = Quat::from_rotation_y(45f32.to_radians());
        let angle = 2.0 * q.dot(expected).abs().clamp(0.0, 1.0).acos();
        assert!(angle < 1e-4, "三路等权应得中间值 45°，角距 {angle} rad");
    }

    #[test]
    fn play_blendspace_excludes_crossfade_and_vice_versa() {
        let mut player =
            AnimationPlayer::new(vec![clip("a", vec![], 1.0), clip("b", vec![], 1.0)], vec![]);
        player.blend_space = Some(BlendSpace {
            samples: vec![(Vec2::ZERO, 0)],
        });
        player.transition(1, 0.5);
        assert!(player.blend.is_some(), "前置：crossfade 进行中");
        player.play_blendspace();
        assert!(player.blend_space_active, "应激活混合空间模式");
        assert!(player.blend.is_none(), "激活时进行中的 crossfade 应丢弃");
        // 反向：transition（含状态机路径）退出混合空间模式
        player.transition(0, 0.5);
        assert!(
            !player.blend_space_active,
            "transition 应退出混合空间模式（互斥）"
        );
        assert!(player.blend.is_some(), "transition 正常启动过渡");
    }

    /// 仅 debug 构建：断言走 `debug_assert!`（release 下静默忽略是
    /// `play_blendspace` 的文档语义），release 编译掉断言后不应再测 panic。
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "blend_space")]
    fn play_blendspace_without_space_panics() {
        let mut bare = AnimationPlayer::new(vec![], vec![]);
        bare.play_blendspace();
    }

    /// 混合空间模式经 update_animations：active 剪辑采样停用，姿势来自
    /// N 路加权（参数 0.25 → x = 0·0.75 + 2·0.25 = 0.5，而非 active 剪辑
    /// 的原值 2）。
    #[test]
    fn blendspace_mode_blends_in_update_animations() {
        let a = clip("a", vec![constant_translation(0, [0.0, 0.0, 0.0])], 1.0);
        let b = clip("b", vec![constant_translation(0, [2.0, 0.0, 0.0])], 1.0);
        let mut world = World::new();
        let root = world.spawn((Transform::default(),));
        let mut player = AnimationPlayer::new(vec![a, b], vec![Some(root)]);
        player.blend_space = Some(BlendSpace {
            samples: vec![(Vec2::new(0.0, 0.0), 0), (Vec2::new(1.0, 0.0), 1)],
        });
        player.blend_param = Vec2::new(0.25, 0.0);
        player.play_blendspace();
        world.insert(root, player);
        world.insert_resource(Time::default());
        step(&world, 0.25);
        assert_eq!(
            world.get::<Transform>(root).unwrap().position.x,
            0.5,
            "混合空间参数 0.25 应得加权 x = 0.5（active 剪辑的 x=2 不覆写）"
        );
    }

    /// 三路旋转 N 路 nlerp 经系统：2 列混合空间在参数 (0.25, 0.25) 给出
    /// 权重 0.5625 / 0.25 / 0.1875（yaw 0/45/90），结果应等于显式加权和
    /// 的归一化。
    #[test]
    fn blendspace_three_way_rotation_nlerp() {
        let yaw = |deg: f32| constant_rotation(0, Quat::from_rotation_y(deg.to_radians()));
        let clips = vec![
            clip("yaw0", vec![yaw(0.0)], 1.0),
            clip("yaw45", vec![yaw(45.0)], 1.0),
            clip("yaw90", vec![yaw(90.0)], 1.0),
        ];
        let mut world = World::new();
        let root = world.spawn((Transform::default(),));
        let mut player = AnimationPlayer::new(clips, vec![Some(root)]);
        player.blend_space = Some(BlendSpace {
            samples: vec![
                (Vec2::new(0.0, 0.0), 0),
                (Vec2::new(1.0, 0.0), 1),
                (Vec2::new(0.0, 1.0), 2),
            ],
        });
        player.blend_param = Vec2::new(0.25, 0.25);
        player.play_blendspace();
        world.insert(root, player);
        world.insert_resource(Time::default());
        step(&world, 0.25);
        let expected = (Quat::from_rotation_y(0.0) * 0.5625
            + Quat::from_rotation_y(45f32.to_radians()) * 0.25
            + Quat::from_rotation_y(90f32.to_radians()) * 0.1875)
            .normalize();
        let actual = world.get::<Transform>(root).unwrap().rotation;
        let angle = 2.0 * actual.dot(expected).abs().clamp(0.0, 1.0).acos();
        assert!(
            angle < 1e-5,
            "三路旋转应等于显式 nlerp 加权和，角距 {angle} rad，实际 {actual:?}"
        );
    }

    // ---- 根运动（v3）----

    /// 直线走位剪辑全程：增量累计 = 总位移 2 m，根节点平移不被覆写。
    #[test]
    fn root_motion_accumulates_full_clip() {
        let mut world = World::new();
        let root = world.spawn((Transform::default(),));
        let mut player = AnimationPlayer::new(vec![walk_clip()], vec![Some(root)]);
        player.looping = false;
        player.root_motion_clip = Some(0);
        player.root_motion_node = Some(0);
        world.insert(root, player);
        world.insert_resource(Time::default());
        let mut total = Vec3::ZERO;
        for _ in 0..9 {
            step(&world, 0.25);
            let delta = world
                .get_mut::<AnimationPlayer>(root)
                .unwrap()
                .take_root_motion();
            if let Some(delta) = delta {
                total += delta;
            }
        }
        assert!(
            (total.x - 2.0).abs() < 1e-5,
            "8 帧增量应累计总位移 2 m，实际 {total:?}"
        );
        assert_eq!(
            world.get::<Transform>(root).unwrap().position,
            Vec3::ZERO,
            "根运动开启时根节点平移不应被覆写"
        );
        assert!(
            world
                .get::<AnimationPlayer>(root)
                .unwrap()
                .pending_root_motion
                .is_none(),
            "take 后应无剩余增量"
        );
    }

    /// 循环回绕帧：0.9 → 原始 1.1 的增量 = (1 − 0.9) + (0.1 − 0) = 0.2，
    /// 无跳变；整圈累计恰为一个周期位移。
    #[test]
    fn root_motion_loop_wrap_has_no_jump() {
        let wrap_clip = clip(
            "walk",
            vec![AnimationChannel {
                node: 0,
                path: AnimationPath::Translation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 1.0],
                values: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            }],
            1.0,
        );
        let mut world = World::new();
        let root = world.spawn((Transform::default(),));
        let mut player = AnimationPlayer::new(vec![wrap_clip], vec![Some(root)]);
        player.root_motion_clip = Some(0);
        player.root_motion_node = Some(0);
        world.insert(root, player);
        world.insert_resource(Time::default());
        // 回绕帧：0.9 + 0.2 = 1.1（显示 0.1）
        world.get_mut::<AnimationPlayer>(root).unwrap().time = 0.9;
        step(&world, 0.2);
        let delta = world
            .get_mut::<AnimationPlayer>(root)
            .unwrap()
            .take_root_motion()
            .expect("回绕帧应产增量");
        assert!(
            (delta.x - 0.2).abs() < 1e-5,
            "回绕增量应 = 旧圈余段 0.1 + 新圈 0.1 = 0.2（无跳变），实际 {delta:?}"
        );
        // 整圈累计：从 0 起 4 × 0.25 s 恰走完 1 m
        let mut total = 0.0f32;
        for _ in 0..4 {
            step(&world, 0.25);
            total += world
                .get_mut::<AnimationPlayer>(root)
                .unwrap()
                .take_root_motion()
                .map(|d| d.x)
                .unwrap_or(0.0);
        }
        assert!(
            (total - 1.0).abs() < 1e-5,
            "整圈增量累计应为一个周期位移 1 m，实际 {total}"
        );
    }

    /// 纯函数多圈语义：一帧跨两圈 = 旧圈余段 + 1 个整圈 + 新圈已走。
    #[test]
    fn root_motion_delta_multilap_and_nonlooping_end() {
        let channel = AnimationChannel {
            node: 0,
            path: AnimationPath::Translation,
            interpolation: AnimationInterpolation::Linear,
            times: vec![0.0, 1.0],
            values: vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        };
        // prev 0.1 → raw 2.15：跨两圈，0.9 + 1.0 + 0.15 = 2.05
        let delta = root_motion_delta(&channel, 0.1, 2.15, 1.0, true);
        assert!(
            (delta.x - 2.05).abs() < 1e-5,
            "一帧跨两圈应三段精确累积 2.05 m，实际 {delta:?}"
        );
        // 非循环停在末端：prev == new → 零增量（不重复计步）
        assert_eq!(
            root_motion_delta(&channel, 1.0, 1.0, 1.0, false),
            Vec3::ZERO,
            "非循环末端应为零增量"
        );
    }

    /// 关闭根运动（默认字段）：平移照常覆写 Transform、不产增量——
    /// 既有行为零影响。
    #[test]
    fn root_motion_disabled_writes_transform_normally() {
        let mut world = World::new();
        let root = world.spawn((Transform::default(),));
        let player = AnimationPlayer::new(vec![walk_clip()], vec![Some(root)]);
        world.insert(root, player);
        world.insert_resource(Time::default());
        step(&world, 0.25);
        assert_eq!(
            world.get::<Transform>(root).unwrap().position.x,
            0.25,
            "关闭根运动：平移照常覆写"
        );
        assert!(
            world
                .get::<AnimationPlayer>(root)
                .unwrap()
                .pending_root_motion
                .is_none(),
            "关闭根运动：不应产增量"
        );
    }

    // ---- 附加动画层（v4）----

    /// 呼吸剪辑（y 平移 0 → 0.1 → 0 三角关键帧）叠加在静止主姿势上，
    /// weight 0.5 → 半幅：t = 0.25 处采样 y = 0.05、delta = 0.025，
    /// 主姿势 y = 1 → 1.025（解析精确）。
    fn breathing_clip() -> AnimationClip {
        clip(
            "breathe",
            vec![AnimationChannel {
                node: 0,
                path: AnimationPath::Translation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 0.5, 1.0],
                values: vec![0.0, 0.0, 0.0, 0.0, 0.1, 0.0, 0.0, 0.0, 0.0],
            }],
            1.0,
        )
    }

    #[test]
    fn additive_breathing_half_weight_exact() {
        let main = clip("idle", vec![constant_translation(0, [0.0, 1.0, 0.0])], 1.0);
        let mut world = World::new();
        let root = world.spawn((Transform::default(),));
        let mut player = AnimationPlayer::new(vec![main, breathing_clip()], vec![Some(root)]);
        player.play_additive(1, 0.5);
        world.insert(root, player);
        world.insert_resource(Time::default());
        step(&world, 0.25);
        let transform = world.get::<Transform>(root).unwrap();
        assert!(
            (transform.position.y - 1.025).abs() < 1e-6,
            "weight 0.5 应得半幅增量：y = 1 + (0.05 − 0) × 0.5 = 1.025，实际 {}",
            transform.position.y
        );
        assert_eq!(
            [transform.position.x, transform.position.z],
            [0.0, 0.0],
            "主姿势的 x / z 无增量，应保持主姿势值"
        );
        assert!(
            (world
                .get::<AnimationPlayer>(root)
                .unwrap()
                .additive
                .unwrap()
                .time
                - 0.25)
                .abs()
                < 1e-6,
            "附加层时间应独立推进到 0.25"
        );
    }

    /// 旋转增量解析对照：附加剪辑 yaw 0° → 90°（参考帧 = 恒等），
    /// t = 0.5 处相对旋转 d = yaw45°，主姿势 yaw90° 右乘 → yaw135°。
    #[test]
    fn additive_rotation_delta_analytic() {
        let main = clip(
            "aim",
            vec![constant_rotation(
                0,
                Quat::from_rotation_y(90f32.to_radians()),
            )],
            1.0,
        );
        let additive = clip(
            "recoil",
            vec![AnimationChannel {
                node: 0,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 1.0],
                values: vec![
                    0.0,
                    0.0,
                    0.0,
                    1.0,
                    0.0,
                    std::f32::consts::FRAC_1_SQRT_2,
                    0.0,
                    std::f32::consts::FRAC_1_SQRT_2,
                ],
            }],
            1.0,
        );
        let mut world = World::new();
        let root = world.spawn((Transform::default(),));
        let mut player = AnimationPlayer::new(vec![main, additive], vec![Some(root)]);
        player.play_additive(1, 1.0);
        world.insert(root, player);
        world.insert_resource(Time::default());
        // max_delta = 0.25s 钳制：分两帧推进 0.5s（附加层 t = 0.5）
        step(&world, 0.25);
        step(&world, 0.25);
        let actual = world.get::<Transform>(root).unwrap().rotation;
        let expected = Quat::from_rotation_y(135f32.to_radians());
        let angle = 2.0 * actual.dot(expected).abs().clamp(0.0, 1.0).acos();
        // 容差 2e-3 rad（≈ 0.11°）：slerp(Identity, d, 1) 与主姿势复合
        // 经两次三角函数路径，f32 实测角距 ~0.00098 rad，1e-5 是单段
        // slerp 的精度（crossfade 测试域），叠加复合放宽两个量级
        assert!(
            angle < 2e-3,
            "相对旋转应右乘主姿势：yaw90 × yaw45 = yaw135，角距 {angle} rad，实际 {actual:?}"
        );
    }

    /// weight = 0 恒等：附加层存在但零权重 → 与完全无附加层的同配置
    /// 世界逐位一致（覆写管线零影响）。
    #[test]
    fn additive_zero_weight_is_bitwise_identity() {
        let build = |additive: bool| {
            let main = clip(
                "idle",
                vec![
                    constant_translation(0, [0.0, 1.0, 0.0]),
                    constant_rotation(0, Quat::from_rotation_y(30f32.to_radians())),
                ],
                1.0,
            );
            let mut world = World::new();
            let root = world.spawn((Transform::default(),));
            let mut player = AnimationPlayer::new(vec![main, breathing_clip()], vec![Some(root)]);
            if additive {
                player.play_additive(1, 0.0);
            }
            world.insert(root, player);
            world.insert_resource(Time::default());
            (world, root)
        };
        let (world_a, root_a) = build(true);
        let (world_b, root_b) = build(false);
        for _ in 0..3 {
            step(&world_a, 0.25);
            step(&world_b, 0.25);
            let ta = world_a.get::<Transform>(root_a).unwrap();
            let tb = world_b.get::<Transform>(root_b).unwrap();
            assert_eq!(
                [
                    ta.position.x.to_bits(),
                    ta.position.y.to_bits(),
                    ta.position.z.to_bits()
                ],
                [
                    tb.position.x.to_bits(),
                    tb.position.y.to_bits(),
                    tb.position.z.to_bits()
                ],
                "weight 0：位置应与无附加层逐位一致"
            );
            assert_eq!(
                [
                    ta.rotation.x.to_bits(),
                    ta.rotation.y.to_bits(),
                    ta.rotation.z.to_bits(),
                    ta.rotation.w.to_bits()
                ],
                [
                    tb.rotation.x.to_bits(),
                    tb.rotation.y.to_bits(),
                    tb.rotation.z.to_bits(),
                    tb.rotation.w.to_bits()
                ],
                "weight 0：旋转应与无附加层逐位一致"
            );
        }
    }

    /// 与 crossfade 并存：叠加发生在 crossfade 混合**之后**——
    /// x = 混合结果 2·smoothstep(0.5) = 1，y = 附加增量 0.1。
    #[test]
    fn additive_applies_after_crossfade() {
        let a = clip("a", vec![constant_translation(0, [0.0, 0.0, 0.0])], 1.0);
        let b = clip("b", vec![constant_translation(0, [2.0, 0.0, 0.0])], 1.0);
        let mut world = World::new();
        let root = world.spawn((Transform::default(),));
        let mut player = AnimationPlayer::new(vec![a, b, breathing_clip()], vec![Some(root)]);
        player.transition(1, 1.0);
        player.play_additive(2, 1.0);
        world.insert(root, player);
        world.insert_resource(Time::default());
        step(&world, 0.25);
        step(&world, 0.25);
        let transform = world.get::<Transform>(root).unwrap();
        let expected_x = 2.0 * blend_weight(0.5, 1.0);
        assert!(
            (transform.position.x - expected_x).abs() < 1e-6,
            "x 应为 crossfade 混合值 {expected_x}，实际 {}",
            transform.position.x
        );
        assert!(
            (transform.position.y - 0.1).abs() < 1e-6,
            "y 应为附加层全幅增量 0.1（叠加在混合结果之后），实际 {}",
            transform.position.y
        );
    }

    /// 诚实边界：主姿势未覆写的通道被附加层**跳过**（以当前 Transform
    /// 为基会让增量逐帧复合漂移——见 `apply_additive` 文档）：主剪辑
    /// 只有平移、附加剪辑只有旋转 → 平移照常覆写，旋转逐位不动
    /// （附加剪辑须与主姿势对齐设计）。
    #[test]
    fn additive_skips_channels_absent_in_main_pose() {
        let main = clip(
            "trans_only",
            vec![constant_translation(0, [3.0, 0.0, 0.0])],
            1.0,
        );
        let additive = clip(
            "rot_only",
            vec![AnimationChannel {
                node: 0,
                path: AnimationPath::Rotation,
                interpolation: AnimationInterpolation::Linear,
                times: vec![0.0, 1.0],
                values: vec![
                    0.0,
                    0.0,
                    0.0,
                    1.0,
                    0.0,
                    std::f32::consts::FRAC_1_SQRT_2,
                    0.0,
                    std::f32::consts::FRAC_1_SQRT_2,
                ],
            }],
            1.0,
        );
        let mut world = World::new();
        let root = world.spawn((Transform::default(),));
        let mut player = AnimationPlayer::new(vec![main, additive], vec![Some(root)]);
        player.play_additive(1, 1.0);
        world.insert(root, player);
        world.insert_resource(Time::default());
        let rotation_before = world.get::<Transform>(root).unwrap().rotation;
        step(&world, 0.25);
        step(&world, 0.25);
        let transform = world.get::<Transform>(root).unwrap();
        assert_eq!(
            transform.position.x, 3.0,
            "主姿势通道照常覆写（不受附加层影响）"
        );
        assert_eq!(
            [
                transform.rotation.x.to_bits(),
                transform.rotation.y.to_bits(),
                transform.rotation.z.to_bits(),
                transform.rotation.w.to_bits()
            ],
            [
                rotation_before.x.to_bits(),
                rotation_before.y.to_bits(),
                rotation_before.z.to_bits(),
                rotation_before.w.to_bits()
            ],
            "主姿势未覆写的旋转通道应被附加层跳过（逐帧复合漂移的病理防线）"
        );
    }

    /// 附加层剪辑越界（debug）：`play_additive` 断言，release 静默忽略
    /// （与 `transition` 同款边界语义）。
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "play_additive")]
    fn play_additive_out_of_range_panics() {
        let mut player = AnimationPlayer::new(vec![], vec![]);
        player.play_additive(3, 1.0);
    }

    // ---- 骨骼遮罩（v4）----

    /// 遮罩含上身节点（0）→ 走位剪辑只覆写上身，腿节点（1）Transform
    /// 逐位不动；`clear_mask` 后恢复全覆写照旧（v1 语义）。
    #[test]
    fn mask_limits_overwrite_to_masked_nodes() {
        let walk = clip(
            "walk",
            vec![
                AnimationChannel {
                    node: 0,
                    path: AnimationPath::Translation,
                    interpolation: AnimationInterpolation::Linear,
                    times: vec![0.0, 1.0],
                    values: vec![0.0, 1.0, 0.0, 1.0, 1.0, 0.0],
                },
                AnimationChannel {
                    node: 1,
                    path: AnimationPath::Translation,
                    interpolation: AnimationInterpolation::Linear,
                    times: vec![0.0, 1.0],
                    values: vec![0.0, 0.0, 0.0, 0.0, 0.0, -1.0],
                },
            ],
            1.0,
        );
        let mut world = World::new();
        let torso = world.spawn((Transform::default(),));
        let leg = world.spawn((Transform::default(),));
        let player = AnimationPlayer::new(vec![walk], vec![Some(torso), Some(leg)]);
        world.insert(torso, player);
        world.insert_resource(Time::default());
        world
            .get_mut::<AnimationPlayer>(torso)
            .unwrap()
            .set_mask(vec![0]);
        let leg_before = *world.get::<Transform>(leg).unwrap();
        step(&world, 0.25);
        assert!(
            (world.get::<Transform>(torso).unwrap().position.x - 0.25).abs() < 1e-6,
            "遮罩内节点（上身）照常覆写"
        );
        let leg_after = *world.get::<Transform>(leg).unwrap();
        assert_eq!(
            [
                leg_after.position.x.to_bits(),
                leg_after.position.y.to_bits(),
                leg_after.position.z.to_bits()
            ],
            [
                leg_before.position.x.to_bits(),
                leg_before.position.y.to_bits(),
                leg_before.position.z.to_bits()
            ],
            "遮罩外节点（腿）应逐位不动"
        );
        // 清除遮罩：恢复全覆写
        world
            .get_mut::<AnimationPlayer>(torso)
            .unwrap()
            .clear_mask();
        step(&world, 0.25);
        let leg_cleared = world.get::<Transform>(leg).unwrap();
        assert!(
            (leg_cleared.position.z - (-0.5)).abs() < 1e-6,
            "清除遮罩后腿节点恢复覆写：z = −0.5，实际 {}",
            leg_cleared.position.z
        );
    }

    /// 按名查骨（打磨 VI 摩擦⑥）的三条边界：重名**取先**（向量序确定 ⇒
    /// 结果确定）、名在但实体缺席（节点不在 active scene）为 None、
    /// 手写构造的播放器无名字表 ⇒ 任何查询都是 None。
    #[test]
    fn node_entity_by_name_takes_first_and_tolerates_gaps() {
        let mut world = World::new();
        let first = world.spawn((Transform::IDENTITY,));
        let second = world.spawn((Transform::IDENTITY,));
        let player = AnimationPlayer {
            node_names: vec![Some("hip".into()), Some("hip".into()), Some("gone".into())],
            node_entities: vec![Some(first), Some(second), None],
            ..Default::default()
        };
        assert_eq!(
            player.node_entity_by_name("hip"),
            Some(first),
            "重名节点应取先出现者"
        );
        assert_eq!(
            player.node_entity_by_name("gone"),
            None,
            "有名但实体缺席应视为查不到"
        );
        assert_eq!(player.node_entity_by_name("knee"), None);
        let handwritten = AnimationPlayer::new(vec![], vec![Some(first)]);
        assert_eq!(
            handwritten.node_entity_by_name("hip"),
            None,
            "无名字表的手写播放器按名查找恒 None"
        );
    }

    /// 参数曲线采样（R74）：排序构造、端点钳制、区间线性——全部精确值
    /// 断言（关键帧取 2 的幂分数，插值无舍入）。
    #[test]
    fn sample_track_is_exact_linear_with_clamping() {
        // 乱序输入 → 构造时升序排序
        let track = ParameterTrack::new("dmg", vec![(1.0, 10.0), (0.0, 0.0), (2.0, 30.0)]);
        assert_eq!(track.keys, vec![(0.0, 0.0), (1.0, 10.0), (2.0, 30.0)]);
        assert_eq!(track.name, "dmg");
        // 端点钳制
        assert_eq!(sample_track(&track.keys, -1.0), 0.0);
        assert_eq!(sample_track(&track.keys, 9.0), 30.0);
        // 区间线性（0.5 / 1.5 均为精确可表示值）
        assert_eq!(sample_track(&track.keys, 0.5), 5.0);
        assert_eq!(sample_track(&track.keys, 1.5), 20.0);
        // 单 key 恒值；空 keys → 0；非有限 t → 0（中性值先例同 BlendSpace）
        let single = ParameterTrack::new("s", vec![(0.3, 7.0)]);
        assert_eq!(sample_track(&single.keys, 0.0), 7.0);
        assert_eq!(sample_track(&single.keys, 5.0), 7.0);
        assert_eq!(sample_track(&[], 1.0), 0.0);
        assert_eq!(sample_track(&track.keys, f32::NAN), 0.0);
    }

    /// 并列时间 key：构造排序保持输入先者；采样在并列处取前者、
    /// 并列之后的区间以**后者**为左端（扫描首遇即返回的确定性行为）。
    #[test]
    fn duplicate_time_keys_are_deterministic() {
        let track = ParameterTrack::new("k", vec![(1.0, 5.0), (1.0, 9.0), (2.0, 3.0)]);
        // 排序稳定：同刻两 key 保持 (5.0, 9.0) 输入序
        assert_eq!(track.keys[0], (1.0, 5.0));
        assert_eq!(track.keys[1], (1.0, 9.0));
        // t 恰在并列处：区间 (0..1] 的右端 → 前者 5.0
        let track = ParameterTrack::new("k", vec![(0.0, 1.0), (1.0, 5.0), (1.0, 9.0), (2.0, 3.0)]);
        assert_eq!(sample_track(&track.keys, 1.0), 5.0);
        // t=1.5 落在 (1.0, 2.0] 区间：左端取并列后者 9.0 → 9 + (3−9)·0.5 = 6.0
        assert_eq!(sample_track(&track.keys, 1.5), 6.0);
    }

    /// 帧数据窗口判定（R74）：半开区间 [start, end)、start 含 / end 不含、
    /// 畸形窗口恒 false、未声明即 false、非有限 time 恒 false。
    #[test]
    fn window_semantics_are_half_open() {
        let windows = vec![
            ClipWindow {
                name: "active".into(),
                start: 0.5,
                end: 1.0,
            },
            ClipWindow {
                name: "active".into(),
                start: 1.5,
                end: 2.0,
            },
            ClipWindow {
                name: "cancel".into(),
                start: 1.2,
                end: 3.0,
            },
        ];
        assert!(!window_active(&windows, 0.499, "active"));
        assert!(window_active(&windows, 0.5, "active"), "start 含");
        assert!(window_active(&windows, 0.999, "active"));
        assert!(!window_active(&windows, 1.0, "active"), "end 不含");
        assert!(window_active(&windows, 1.5, "active"), "第二个同名窗口");
        assert!(window_active(&windows, 1.8, "cancel"));
        assert!(!window_active(&windows, 0.7, "cancel"), "未进区间");
        assert!(!window_active(&windows, 0.7, "nope"), "未声明 = false");
        assert!(!window_active(&windows, f32::NAN, "active"));
        let malformed = vec![ClipWindow {
            name: "m".into(),
            start: 2.0,
            end: 1.0,
        }];
        assert!(!window_active(&malformed, 1.5, "m"), "畸形窗口恒 false");
    }

    /// 窗口 / 曲线 / 事件三件套经 update_animations：事件「跨过即发」与
    /// 窗口半开区间**同帧衔接**（事件帧 = 窗口首帧）；曲线值 = 推进后
    /// 时间的采样。关键帧与步长全取 2 的幂分数 ⇒ 期望值精确无舍入。
    #[test]
    fn windows_curves_and_events_flow_through_update() {
        let mut combat = clip("combat", vec![], 2.0);
        combat.events.push(AnimationEvent {
            time: 0.5,
            name: "hit".into(),
        });
        combat.windows.push(ClipWindow {
            name: "active".into(),
            start: 0.5,
            end: 1.0,
        });
        combat
            .curves
            .push(ParameterTrack::new("dmg", vec![(0.0, 0.0), (1.0, 10.0)]));
        let (mut world, root) = blend_world(combat, clip("idle", vec![], 1.0));
        world.insert_resource(Time::default());
        world.get_mut::<AnimationPlayer>(root).unwrap().looping = false;

        step(&world, 0.25); // 0.0 → 0.25
        {
            let mut player = world.get_mut::<AnimationPlayer>(root).unwrap();
            assert!(player.take_events().is_empty(), "未跨过 0.5 不触发");
            assert!(!player.window_active("active"));
            assert_eq!(player.parameter("dmg"), Some(2.5));
        }
        step(&world, 0.25); // 0.25 → 0.5：跨过事件时刻
        {
            let mut player = world.get_mut::<AnimationPlayer>(root).unwrap();
            assert_eq!(player.take_events(), vec!["hit".to_string()]);
            assert!(
                player.window_active("active"),
                "事件帧 = 窗口首帧（0.5 ∈ [0.5, 1.0)）"
            );
            assert_eq!(player.parameter("dmg"), Some(5.0));
        }
        step(&world, 0.25); // 0.5 → 0.75
        {
            let mut player = world.get_mut::<AnimationPlayer>(root).unwrap();
            assert!(player.take_events().is_empty());
            assert!(player.window_active("active"));
            assert_eq!(player.parameter("dmg"), Some(7.5));
        }
        step(&world, 0.25); // 0.75 → 1.0
        {
            let mut player = world.get_mut::<AnimationPlayer>(root).unwrap();
            assert!(player.take_events().is_empty());
            assert!(!player.window_active("active"), "end=1.0 不含");
            assert_eq!(player.parameter("dmg"), Some(10.0), "越末端钳制");
        }
    }

    // ---- C4b · 状态机 v2（参数黑板 + 条件自动转移）× 运行时重定向 ----

    /// 双态播放器（idle=clip0 循环 / run=clip1 非循环 speed2），带名字表。
    fn two_state_player_named() -> AnimationPlayer {
        let idle = clip("idle", vec![constant_translation(0, [0.0, 0.0, 0.0])], 1.0);
        let run = clip("run", vec![constant_translation(0, [3.0, 0.0, 0.0])], 1.0);
        let mut player = AnimationPlayer::new(vec![idle, run], vec![Some(Entity::from_bits(1))]);
        player.node_names = vec![Some("Hips".into())];
        player.states = vec![
            AnimationState {
                clip: 0,
                looping: true,
                speed: 1.0,
            },
            AnimationState {
                clip: 1,
                looping: false,
                speed: 2.0,
            },
        ];
        player
    }

    /// 条件转移：浮点守卫 `>= 1.0` 恰在阈值触发（边界归触发侧），
    /// 阈值下方不触发；触发后 `current_state` / `active` / 目标态
    /// looping+speed 一并落账，非硬切换（duration 0.3 开 crossfade）。
    #[test]
    fn guarded_transition_fires_on_float_threshold_inclusive() {
        let mut player = two_state_player_named();
        player.transitions.push(AnimTransition {
            from: 0,
            to: 1,
            duration: 0.3,
            guard: AnimGuard::FloatAtLeast("speed".into(), 1.0),
        });
        player.set_float("speed", 0.999);
        assert_eq!(player.step_state_machine(), None, "阈值下方不触发");
        assert_eq!(player.current_state, 0);
        player.set_float("speed", 1.0);
        assert_eq!(
            player.step_state_machine(),
            Some(0),
            "恰等于阈值应触发（>= 边界归触发侧）"
        );
        assert_eq!(player.current_state, 1);
        assert_eq!(player.active, 1);
        assert!(!player.looping, "run 态非循环");
        assert!(player.blend.is_some(), "duration 0.3 开 crossfade");
        assert!((player.speed - 2.0).abs() < 1e-6, "speed 落账目标态");
    }

    /// 空转移表 = 恒 no-op（v1 逐位兼容锚：不启用条件转移时步检零影响）。
    #[test]
    fn empty_transitions_step_is_noop() {
        let mut player = two_state_player_named();
        player.set_float("speed", 100.0);
        assert_eq!(player.step_state_machine(), None);
        assert_eq!(player.current_state, 0);
    }

    /// 声明序裁决 + 一帧至多一条：两条 from=0 都成立时取首条，
    /// 触发后不再评估后续（防级联）。
    #[test]
    fn first_declared_transition_wins_and_only_one_per_step() {
        let mut player = two_state_player_named();
        // 第一条（会命中）+ 第二条（from 已被首条改走，本步不再触发）
        player.transitions.push(AnimTransition {
            from: 0,
            to: 1,
            duration: 0.0,
            guard: AnimGuard::Always,
        });
        player.transitions.push(AnimTransition {
            from: 1,
            to: 0,
            duration: 0.0,
            guard: AnimGuard::Always,
        });
        assert_eq!(player.step_state_machine(), Some(0), "取首条命中");
        assert_eq!(player.current_state, 1);
        // 再步一次才会走 1→0（下一帧语义）
        assert_eq!(player.step_state_machine(), Some(1));
        assert_eq!(player.current_state, 0);
    }

    /// 自环守卫（from==to）不触发——避免循环剪辑每帧重启。
    #[test]
    fn self_transition_is_skipped() {
        let mut player = two_state_player_named();
        player.transitions.push(AnimTransition {
            from: 0,
            to: 0,
            duration: 0.0,
            guard: AnimGuard::Always,
        });
        assert_eq!(player.step_state_machine(), None);
    }

    /// ClipFinished 守卫：非循环态剪辑未播完不触发，`time ≥ duration` 触发。
    #[test]
    fn clip_finished_guard_tracks_playback_end() {
        let mut player = two_state_player_named();
        player.current_state = 1; // run 非循环、duration 1.0
        player.transitions.push(AnimTransition {
            from: 1,
            to: 0,
            duration: 0.0,
            guard: AnimGuard::ClipFinished,
        });
        player.time = 0.5;
        assert_eq!(player.step_state_machine(), None, "未播完不触发");
        player.time = 1.0;
        assert_eq!(player.step_state_machine(), Some(0), "播完触发回 idle");
        assert_eq!(player.current_state, 0);
    }

    /// 复合守卫 Not / Any / All + 布尔：`(grounded==true) && !(airborne)`。
    #[test]
    fn compound_bool_guard_evaluates() {
        let mut player = two_state_player_named();
        player.transitions.push(AnimTransition {
            from: 0,
            to: 1,
            duration: 0.0,
            guard: AnimGuard::All(vec![
                AnimGuard::BoolIs("grounded".into(), true),
                AnimGuard::Not(Box::new(AnimGuard::BoolIs("airborne".into(), true))),
            ]),
        });
        player.set_bool("grounded", true);
        player.set_bool("airborne", true);
        assert_eq!(
            player.step_state_machine(),
            None,
            "airborne 为真被 Not 拦下"
        );
        player.set_bool("airborne", false);
        assert_eq!(player.step_state_machine(), Some(0));
    }

    /// 缺失参数取类型默认（浮点 0.0 / 布尔 false）——不 panic。
    #[test]
    fn missing_params_use_neutral_defaults() {
        let empty = BTreeMap::new();
        assert!(
            AnimGuard::FloatBelow("x".into(), 1.0).eval(&empty),
            "缺浮点默认 0.0"
        );
        assert!(
            AnimGuard::BoolIs("b".into(), false).eval(&empty),
            "缺布尔默认 false"
        );
        assert!(!AnimGuard::BoolIs("b".into(), true).eval(&empty));
    }

    /// update_animations 端到端：黑板满足后由系统写回阶段步检触发
    /// （同帧其余播放器无转移 ⇒ 零影响）。
    #[test]
    fn update_animations_steps_guarded_machine() {
        let idle = clip("idle", vec![constant_translation(0, [0.0, 0.0, 0.0])], 1.0);
        let run = clip("run", vec![constant_translation(0, [3.0, 0.0, 0.0])], 1.0);
        let (mut world, root) = blend_world(idle, run);
        world.insert_resource(Time::default());
        {
            let mut player = world.get_mut::<AnimationPlayer>(root).unwrap();
            player.states = vec![
                AnimationState {
                    clip: 0,
                    looping: true,
                    speed: 1.0,
                },
                AnimationState {
                    clip: 1,
                    looping: true,
                    speed: 1.0,
                },
            ];
            player.transitions.push(AnimTransition {
                from: 0,
                to: 1,
                duration: 0.0,
                guard: AnimGuard::FloatAtLeast("speed".into(), 1.0),
            });
        }
        step(&world, 1.0 / 60.0);
        {
            let player = world.get::<AnimationPlayer>(root).unwrap();
            assert_eq!(player.current_state, 0, "speed 未设（默认 0）⇒ 不转移");
        }
        world
            .get_mut::<AnimationPlayer>(root)
            .unwrap()
            .set_float("speed", 2.0);
        step(&world, 1.0 / 60.0);
        let player = world.get::<AnimationPlayer>(root).unwrap();
        assert_eq!(player.current_state, 1, "系统步检自动进入 run");
        assert_eq!(player.active, 1);
        assert!(
            (world.get::<Transform>(root).unwrap().position.x - 0.0).abs() < 1e-6,
            "转移生效帧的姿势仍来自旧状态采样（idle 恒值 0）——一帧软延迟口径"
        );
    }

    /// 越界目标状态 = 无条件断言 panic（配置错误必须暴露，release 同防）。
    #[test]
    #[should_panic(expected = "越界状态")]
    fn transition_to_out_of_range_state_panics() {
        let mut player = two_state_player_named();
        player.transitions.push(AnimTransition {
            from: 0,
            to: 99,
            duration: 0.0,
            guard: AnimGuard::Always,
        });
        player.step_state_machine();
    }

    /// 确定性：同参数黑板两跑逐步转移，落账序一致（BTreeMap 固定序 + 声明序）。
    #[test]
    fn guarded_stepping_is_deterministic() {
        let build = || {
            let mut p = two_state_player_named();
            p.transitions.push(AnimTransition {
                from: 0,
                to: 1,
                duration: 0.0,
                guard: AnimGuard::FloatAtLeast("speed".into(), 1.0),
            });
            p.set_float("speed", 2.0);
            p
        };
        let mut a = build();
        let mut b = build();
        let ra = a.step_state_machine();
        let rb = b.step_state_machine();
        assert_eq!(ra, rb);
        assert_eq!(a.current_state, b.current_state);
        assert_eq!(a.active, b.active);
        assert_eq!(a.time.to_bits(), b.time.to_bits());
    }

    /// 运行时重定向：源骨架（Hips / Spine / RightArm）按名重绑到目标骨架
    /// 实体（前缀差异 Mixamo 归一命中），未命中骨断绑为 None 并计数。
    #[test]
    fn retarget_by_name_rebinds_and_severs() {
        let idle = clip("idle", vec![constant_translation(0, [0.0, 0.0, 0.0])], 1.0);
        let mut player = AnimationPlayer::new(vec![idle], vec![None, None, None]);
        player.node_names = vec![
            Some("mixamorig:Hips".into()),
            Some("Spine".into()),
            Some("RightArm".into()),
        ];
        // 目标骨架：Hips（无前缀，经归一与 mixamorig:Hips 命中）、Spine；无 RightArm
        let hips = Entity::from_bits(0x1000);
        let spine = Entity::from_bits(0x2000);
        let report = player.retarget_by_name(
            &[
                Some("Hips".into()),
                Some("Spine".into()),
                Some("Head".into()),
            ],
            &[Some(hips), Some(spine), None],
        );
        assert_eq!(player.node_entities, vec![Some(hips), Some(spine), None]);
        assert_eq!(report.bound, 2, "Hips + Spine 命中");
        assert_eq!(report.unmatched, 1, "RightArm 目标骨架无同名骨");
    }

    /// 重定向不猜语义改名：`thigh_r` 在只有 `rightThigh` 的目标骨架上断绑
    /// （引擎不做编辑距离 / 别名表，猜错比断绑难排查）。
    #[test]
    fn retarget_does_not_guess_semantic_renames() {
        let idle = clip("idle", vec![constant_translation(0, [0.0, 0.0, 0.0])], 1.0);
        let mut player = AnimationPlayer::new(vec![idle], vec![None]);
        player.node_names = vec![Some("thigh_r".into())];
        let report =
            player.retarget_by_name(&[Some("rightThigh".into())], &[Some(Entity::from_bits(7))]);
        assert_eq!(player.node_entities, vec![None], "语义改名不匹配 ⇒ 断绑");
        assert_eq!(report.unmatched, 1);
    }

    /// 空名字表（手写构造播放器）⇒ 重定向恒 no-op（无源名可比对）。
    #[test]
    fn retarget_with_empty_names_is_noop() {
        let idle = clip("idle", vec![constant_translation(0, [0.0, 0.0, 0.0])], 1.0);
        let e = Entity::from_bits(42);
        let mut player = AnimationPlayer::new(vec![idle], vec![Some(e)]);
        // node_names 默认空
        let report = player.retarget_by_name(&[Some("Hips".into())], &[Some(e)]);
        assert_eq!(report, RetargetReport::default(), "无源名 ⇒ 无重绑");
        assert_eq!(player.node_entities, vec![Some(e)], "原绑定保留");
    }

    /// 无名节点不参与重定向（保留原绑定，半途断绑比保留更糟）。
    #[test]
    fn retarget_preserves_unnamed_nodes() {
        let idle = clip("idle", vec![constant_translation(0, [0.0, 0.0, 0.0])], 1.0);
        let keep = Entity::from_bits(9);
        let mut player = AnimationPlayer::new(vec![idle], vec![Some(keep), Some(keep)]);
        player.node_names = vec![None, Some("Ghost".into())];
        player.retarget_by_name(&[Some("Hips".into())], &[Some(Entity::from_bits(1))]);
        assert_eq!(player.node_entities[0], Some(keep), "无名节点保留原绑定");
        assert_eq!(player.node_entities[1], None, "有名未命中断绑");
    }

    /// 骨名归一纯函数：前缀 / 大小写 / 空格形态。
    #[test]
    fn bone_name_normalization() {
        assert_eq!(normalize_bone_name("mixamorig:Hips"), "hips");
        assert_eq!(normalize_bone_name("mixamorigLeftArm"), "leftarm");
        assert_eq!(normalize_bone_name("Bip01_Head"), "head");
        assert_eq!(normalize_bone_name("Hips"), "hips");
        assert_eq!(normalize_bone_name("  Left Arm  "), "left_arm");
    }
}
