//! 感知：视野锥 / 视线遮挡 / 听觉半径（纯函数）+ 差分事件系统（R65）。
//!
//! 敌人 AI 的「看见 / 听见」在状态机时代是游戏侧手写的距离 + 点积 + 射线
//! 三件套（iris-infiltration 的 `guard_sees` 即此）。本模块把它提成引擎级
//! 原语，形状是**纯函数 + 组件 + 事件**：
//!
//! - **纯函数**：[`in_view_cone`]（距离 + 半角点积）、[`has_line_of_sight`]
//!   （线段 × 静态体，复用 `physics` 的射线纯函数）、[`can_see`]（两者合成）、
//!   [`within_hearing`]（纯距离，无视线）——全部可脱离 `World` 单测；
//! - **组件**：感知源挂 [`Perception`]（视距 / 半角余弦 / 听觉半径 / 眼位
//!   偏移，参数全部字段化），目标挂 [`PerceptionTarget`]（标记 + 受感点偏移）；
//! - **系统**：[`perception_system`]（并行系统体）扫描感知源 → 目标列表 →
//!   与上帧差分 → `Events<[PerceptionEvent]>`（Seen / Lost / Heard，双代缓冲
//!   先例见 `crate::event`），并把本帧结果写回 [`Perception`] 的 `seen` /
//!   `heard` 运行态字段。
//!
//! 守卫式的「警觉 / 丢失」由游戏侧消费事件写**黑板**（`crate::behavior`）——
//! 本模块不认识行为树，两者通过事件与组件正交组合。
//!
//! # 几何定义（确定性契约：运算序固定，逐位可复现）
//!
//! ```text
//! eye    = Transform.position + Transform.rotation * Perception.eye_offset
//! point  = Transform.position + Transform.rotation * PerceptionTarget.offset
//! to     = point - eye
//! dist   = |to|                       // 三维距离（y 计入视距）
//! forward = normalize_xz(Transform.rotation * -Z)   // 水平朝向（引擎前方约定）
//! 可见 ⇔ 1e-4 ≤ dist ≤ sight_distance 且 forward · (to_xz / dist) ≥ fov_cos
//!         且 eye → point 线段不被任何静态体命中
//! 可闻 ⇔ hearing_radius > 0 且 dist ≤ hearing_radius（无视线判定）
//! ```
//!
//! `forward · (to_xz / dist)` 正是**三维视锥**的夹角余弦（朝向水平、无 y 分量，
//! 故点积只看水平投影，分母用三维距离）——半角 55° 就填 `fov_cos = cos(55°)`。
//! 该式与 iris-infiltration 既有 `guard_sees` 逐位同源（迁移验收的硬约束）。
//!
//! # 事件语义（双代缓冲）
//!
//! 每帧与上帧结果差分：新入视锥 → `Seen`；离开视锥 → `Lost`；新入听觉半径 →
//! `Heard`（带声源点）。事件产出顺序 = 感知源实体 id 升序，逐源内先 Seen
//! （目标 id 升序）、再 Lost、再 Heard——顺序本身是确定性的一部分。
//! 事件类型须先 `world.register_event::<PerceptionEvent>()`（未注册时
//! `try_send_event` 丢弃并告警，见 `World::try_send_event`）。
//!
//! # 组件缺席 / 默认语义
//!
//! - 无 [`Perception`] 的实体**不是感知源**（不扫描、不产出事件）；
//! - 无 [`PerceptionTarget`] 的实体**不是目标**（哪怕几何上在视锥里）；
//! - 无 `Transform` 的实体两边都不参与（查询要求）；
//! - [`Perception::default`] = **全盲**（视距 0 / 听觉半径 0）——默认构造不会
//!   意外开眼；
//! - 感知源自身不感知自己（同一实体既挂 Perception 又挂 PerceptionTarget
//!   时跳过自指对）。
//!
//! # 诚实限制清单（v1）
//!
//! - **不做**：扇形记忆 / 目标预测 / 声音优先级 / 视线焦散、多眼位、
//!   视锥的竖直扇形（朝向恒水平，俯仰靠 `eye_offset` 手工给）；
//! - **遮挡只看静态体**：判定口径 = `physics::effective_static`（`is_static` /
//!   无 `RigidBody` / `Kinematic`）。**动态刚体不挡视线**（可推动的箱子不会
//!   挡住守卫）——v1 取舍：动态遮挡需要逐帧全量射线且语义上「暴露/掩体」
//!   应由游戏规则决定（潜行游戏的掩体是设计物而不是物理副产物）；
//! - 组（group / mask）不参与感知过滤（v1：谁挂目标组件谁可被感知）；
//! - 余量判定不含「渐进发现」：本模块只给二值可见性，警觉计量是游戏侧
//!   的规则（行为树 + 黑板承载）；
//! - `heard` 没有对应的「离开听觉」事件（对称的 `Silence` 在后续轮需要时
//!   再加）——游戏侧要清听觉状态请自行比较组件字段。

use glam::{Quat, Vec2, Vec3};

use crate::entity::Entity;
use crate::physics::{
    Collider, ColliderShape, RigidBody, StaticObstacle, effective_static,
    segment_hits_shape_rotated,
};
use crate::transform::Transform;
use crate::world::World;
use crate::{Component, Event};

// ---------------------------------------------------------------------------
// 组件
// ---------------------------------------------------------------------------

/// 感知源组件：视野锥与听觉半径参数（全部字段化，便于按角色配不同数值）。
///
/// 运行态字段 `seen` / `heard` 由 [`perception_system`] 每帧覆写（实体 id
/// 升序）；手工构造中间态时留空即可。`eye_offset` 是**本地偏移**（随
/// `Transform.rotation` 旋转）——站立人形的眼位就是 `(0, 1.4, 0)` 之类。
#[derive(Clone, Debug, PartialEq, Component)]
pub struct Perception {
    /// 眼位本地偏移（世界眼位 = 位置 + 旋转 × 此偏移）。
    pub eye_offset: Vec3,
    /// 视距（米；≤ 0 = 无视锥）。
    pub sight_distance: f32,
    /// 视锥半角余弦（1 = 正前方零角，-1 = 全向；固定运算序见模块文档）。
    pub fov_cos: f32,
    /// 听觉半径（米；≤ 0 = 无听觉）。
    pub hearing_radius: f32,
    /// 上帧可见目标（实体 id 升序；系统写）。
    pub seen: Vec<Entity>,
    /// 上帧可闻目标（实体 id 升序；系统写）。
    pub heard: Vec<Entity>,
}

impl Default for Perception {
    /// **全盲**（视距 0 / 听觉 0）——默认构造不会意外开眼。
    fn default() -> Self {
        Self {
            eye_offset: Vec3::ZERO,
            sight_distance: 0.0,
            fov_cos: 1.0,
            hearing_radius: 0.0,
            seen: Vec::new(),
            heard: Vec::new(),
        }
    }
}

impl Perception {
    /// 按「视距 / 半角余弦 / 听觉半径」构造（眼位偏移为 0）。
    pub fn new(sight_distance: f32, fov_cos: f32, hearing_radius: f32) -> Self {
        Self {
            sight_distance,
            fov_cos,
            hearing_radius,
            ..Self::default()
        }
    }

    /// 眼位本地偏移（链式配置）。
    pub fn with_eye_offset(mut self, offset: Vec3) -> Self {
        self.eye_offset = offset;
        self
    }
}

/// 可被感知的目标组件（标记 + 受感点本地偏移）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Component)]
pub struct PerceptionTarget {
    /// 受感点本地偏移（世界受感点 = 位置 + 旋转 × 此偏移）——人形给胸口高度。
    pub offset: Vec3,
}
// ---------------------------------------------------------------------------
// 纯函数（感知几何）
// ---------------------------------------------------------------------------

/// 视野锥判定（纯函数）：**三维距离 + 水平朝向的半角点积**。
///
/// 精确定义见模块文档「几何定义」：`dist = |target - eye|`（y 计入视距），
/// 方向取水平投影、按三维距离归一——即三维视锥的夹角余弦（朝向无 y 分量）。
/// `forward_xz` 须是**单位**水平朝向（`Transform.rotation * -Vec3::Z` 的 XZ
/// 投影再归一；引擎前方约定 -Z）；内部不重归一，以保证与既有游戏侧实现
/// 逐位同源。
///
/// 边界：`dist < 1e-4`（退化重合）或 `dist > sight_distance` 视作不可见；
/// 夹角余弦**等于** `fov_cos` 视为可见（闭边界）。
pub fn in_view_cone(
    eye: Vec3,
    target: Vec3,
    forward_xz: Vec2,
    sight_distance: f32,
    fov_cos: f32,
) -> bool {
    let to = target - eye;
    let dist = to.length();
    if !(1e-4..=sight_distance).contains(&dist) {
        return false;
    }
    let dir_xz = Vec2::new(to.x, to.z) / dist;
    forward_xz.dot(dir_xz) >= fov_cos
}

/// 视线遮挡判定（纯函数）：`eye → target` 线段不被任何静态体命中即通视。
///
/// 复用 `physics` 的 [`segment_hits_shape_rotated`]（旋转感知——倒地尸体的
/// 斜置胶囊照实挡视线）。零长度线段（`eye == target`）判**通视**
/// （`segment_hits_shape` 对退化线段返回 `None`）——与「距离下界」无关，
/// 调用方一般先过 [`in_view_cone`]。
pub fn has_line_of_sight(eye: Vec3, target: Vec3, statics: &[StaticObstacle]) -> bool {
    statics.iter().all(|&(position, rotation, ref shape)| {
        segment_hits_shape_rotated(eye, target, position, rotation, shape).is_none()
    })
}

/// 完整可见性（纯函数）：视锥内 + 通视。
pub fn can_see(
    eye: Vec3,
    target: Vec3,
    forward_xz: Vec2,
    sight_distance: f32,
    fov_cos: f32,
    statics: &[StaticObstacle],
) -> bool {
    in_view_cone(eye, target, forward_xz, sight_distance, fov_cos)
        && has_line_of_sight(eye, target, statics)
}

/// 听觉判定（纯函数，**纯距离、无视线**）：`hearing_radius > 0` 且
/// `|target - source| ≤ hearing_radius`。
///
/// 语义说明：隔墙听得见（声音绕过障碍是 v1 的刻意取舍——门后的脚步正是
/// 潜行玩法的信息源）；半径 ≤ 0 = 无听觉。
pub fn within_hearing(source: Vec3, target: Vec3, hearing_radius: f32) -> bool {
    hearing_radius > 0.0 && (target - source).length() <= hearing_radius
}

/// 把关卡形态的「位置 + 形状」静态体列表转成 [`StaticObstacle`]（恒等旋转）。
///
/// 俯视 / 关卡数据（如 `NavGrid::build` 的输入）不带旋转；视线判定吃带旋转
/// 的形态。**调用方缓存结果**即可逐帧零分配（iris-infiltration 即如此）。
pub fn flat_statics(statics: &[(Vec3, ColliderShape)]) -> Vec<StaticObstacle> {
    statics
        .iter()
        .map(|&(position, ref shape)| (position, Quat::IDENTITY, *shape))
        .collect()
}

// ---------------------------------------------------------------------------
// 事件
// ---------------------------------------------------------------------------

/// 感知差分事件（双代缓冲；见模块文档「事件语义」）。
#[derive(Clone, Copy, Debug, PartialEq, Event)]
pub enum PerceptionEvent {
    /// 目标进入视野（本帧可见、上帧不可见）。
    Seen {
        /// 感知源实体。
        observer: Entity,
        /// 目标实体。
        target: Entity,
    },
    /// 目标离开视野（上帧可见、本帧不可见）。
    Lost {
        /// 感知源实体。
        observer: Entity,
        /// 目标实体。
        target: Entity,
    },
    /// 目标进入听觉半径（本帧可闻、上帧不可闻）——`at` = 声源点。
    Heard {
        /// 感知源实体。
        observer: Entity,
        /// 目标实体。
        target: Entity,
        /// 声源点（受感点世界坐标）。
        at: Vec3,
    },
}
// ---------------------------------------------------------------------------
// 系统（差分 + 事件）
// ---------------------------------------------------------------------------

/// 单个感知源的只读快照（阶段一收集 → 阶段二差分写回，两阶段模式）。
struct ObserverSnapshot {
    entity: Entity,
    eye: Vec3,
    forward_xz: Vec2,
    sight_distance: f32,
    fov_cos: f32,
    hearing_radius: f32,
    seen: Vec<Entity>,
    heard: Vec<Entity>,
}

/// 感知系统：扫描感知源 → 目标列表 → 与上帧差分 → 事件 + 运行态写回。
///
/// 只读世界签名（`&World`），可注册为**并行系统**；写路径只有
/// `Perception` 组件与事件缓冲（尝试型锁，见 `crate::sync`）。
///
/// 确定性：三份收集列表（静态体 / 感知源 / 目标）都按**实体 id 升序**排序
/// 后处理（显式 `sort_unstable`，与 dense 数组顺序、spawn 顺序无关）；逐源
/// 内的事件产出序固定为 Seen → Lost → Heard。同世界同帧两次调用逐位一致。
///
/// 使用前须 `world.register_event::<PerceptionEvent>()`（未注册时事件被
/// `try_send_event` 丢弃并告警——注册是结构性修改，需要 `&mut World`）。
pub fn perception_system(world: &World) {
    // ---- 阶段一：只读收集（静态体 / 感知源 / 目标）----
    let mut statics: Vec<(Entity, StaticObstacle)> = Vec::new();
    world.query::<(Entity, &Transform, &Collider, Option<&RigidBody>)>(
        |(entity, transform, collider, rigid)| {
            if effective_static(collider, rigid) {
                statics.push((
                    entity,
                    (transform.position, transform.rotation, collider.shape),
                ));
            }
        },
    );
    statics.sort_unstable_by_key(|(entity, _)| *entity);
    let obstacles: Vec<StaticObstacle> = statics.into_iter().map(|(_, item)| item).collect();

    let mut observers: Vec<ObserverSnapshot> = Vec::new();
    world.query::<(Entity, &Perception, &Transform)>(|(entity, perception, transform)| {
        let forward = transform.rotation * -Vec3::Z;
        observers.push(ObserverSnapshot {
            entity,
            eye: transform.position + transform.rotation * perception.eye_offset,
            forward_xz: Vec2::new(forward.x, forward.z).normalize_or_zero(),
            sight_distance: perception.sight_distance,
            fov_cos: perception.fov_cos,
            hearing_radius: perception.hearing_radius,
            seen: perception.seen.clone(),
            heard: perception.heard.clone(),
        });
    });
    observers.sort_unstable_by_key(|snapshot| snapshot.entity);

    let mut targets: Vec<(Entity, Vec3)> = Vec::new();
    world.query::<(Entity, &PerceptionTarget, &Transform)>(|(entity, target, transform)| {
        targets.push((
            entity,
            transform.position + transform.rotation * target.offset,
        ));
    });
    targets.sort_unstable_by_key(|(entity, _)| *entity);

    // ---- 阶段二：逐源差分 → 事件 + 写回 ----
    for snapshot in &observers {
        let mut seen_now: Vec<Entity> = Vec::new();
        let mut heard_now: Vec<Entity> = Vec::new();
        let mut heard_at: Vec<Vec3> = Vec::new();
        for &(target_entity, point) in &targets {
            if target_entity == snapshot.entity {
                continue; // 自身不感知自身（同一实体可同时是源与目标）
            }
            if can_see(
                snapshot.eye,
                point,
                snapshot.forward_xz,
                snapshot.sight_distance,
                snapshot.fov_cos,
                &obstacles,
            ) {
                seen_now.push(target_entity);
            }
            if within_hearing(snapshot.eye, point, snapshot.hearing_radius) {
                heard_now.push(target_entity);
                heard_at.push(point);
            }
        }
        for &target in &seen_now {
            if !snapshot.seen.contains(&target) {
                world.try_send_event(PerceptionEvent::Seen {
                    observer: snapshot.entity,
                    target,
                });
            }
        }
        for &target in &snapshot.seen {
            if !seen_now.contains(&target) {
                world.try_send_event(PerceptionEvent::Lost {
                    observer: snapshot.entity,
                    target,
                });
            }
        }
        for (index, &target) in heard_now.iter().enumerate() {
            if !snapshot.heard.contains(&target) {
                world.try_send_event(PerceptionEvent::Heard {
                    observer: snapshot.entity,
                    target,
                    at: heard_at[index],
                });
            }
        }
        if let Some(mut perception) = world.get_mut::<Perception>(snapshot.entity) {
            perception.seen = seen_now;
            perception.heard = heard_now;
        }
    }
}
// ---------------------------------------------------------------------------
// 单测（纯函数解析对照 + 系统差分 / 双代缓冲 / 插入序无关）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Quat;

    /// 测试标签（跨世界比较的归一化键）。
    #[derive(Component)]
    struct Tag(&'static str);

    fn box_wall(center: Vec3) -> StaticObstacle {
        (
            center,
            Quat::IDENTITY,
            ColliderShape::Box {
                half_extents: Vec3::new(1.0, 1.25, 0.2),
            },
        )
    }

    fn tag_of(world: &World, entity: Entity) -> String {
        world
            .get::<Tag>(entity)
            .map(|tag| tag.0.to_string())
            .unwrap_or_else(|| format!("?{}", entity.index()))
    }

    /// 单条事件 → 标签化的可读串。
    fn format_event(world: &World, event: &PerceptionEvent) -> String {
        match *event {
            PerceptionEvent::Seen { observer, target } => format!(
                "Seen({},{})",
                tag_of(world, observer),
                tag_of(world, target)
            ),
            PerceptionEvent::Lost { observer, target } => format!(
                "Lost({},{})",
                tag_of(world, observer),
                tag_of(world, target)
            ),
            PerceptionEvent::Heard {
                observer, target, ..
            } => format!(
                "Heard({},{})",
                tag_of(world, observer),
                tag_of(world, target)
            ),
        }
    }

    /// 事件窗口内的全部事件（不消费；双代语义断言用）。
    fn event_trace(world: &World) -> Vec<String> {
        world
            .events::<PerceptionEvent>()
            .iter()
            .map(|event| format_event(world, event))
            .collect()
    }

    /// 消费式读取（每帧一个 drain 方的既有消费契约；帧序断言用）。
    fn drain_trace(world: &World) -> Vec<String> {
        world
            .events_mut::<PerceptionEvent>()
            .drain()
            .map(|event| format_event(world, &event))
            .collect()
    }

    /// 组件运行态 → 标签化摘要（按标签排序，跨世界可比）。
    fn state_trace(world: &World) -> Vec<String> {
        let mut out = Vec::new();
        world.query::<(&Tag, &Perception)>(|(tag, perception)| {
            let seen: Vec<String> = perception
                .seen
                .iter()
                .map(|entity| tag_of(world, *entity))
                .collect();
            let heard: Vec<String> = perception
                .heard
                .iter()
                .map(|entity| tag_of(world, *entity))
                .collect();
            out.push(format!(
                "{}(seen=[{}],heard=[{}])",
                tag.0,
                seen.join(","),
                heard.join(",")
            ));
        });
        out.sort();
        out
    }

    // ---- 纯函数：视野锥 ----

    #[test]
    fn view_cone_distance_angle_and_closed_boundary() {
        let eye = Vec3::new(0.0, 1.4, 0.0);
        let forward = Vec2::new(0.0, -1.0); // 面朝 -Z
        let sight = 10.0;
        // 半角 60°（fov_cos = 0.5）：正前方可见
        let fov = 0.5;
        assert!(in_view_cone(
            eye,
            Vec3::new(0.0, 1.4, -5.0),
            forward,
            sight,
            fov
        ));
        // 正后方：点积 -1 < 0.5 → 不可见
        assert!(!in_view_cone(
            eye,
            Vec3::new(0.0, 1.4, 5.0),
            forward,
            sight,
            fov
        ));
        // 正侧向：点积恰 0 < 0.5 → 不可见（90° 在 60° 半角之外）
        assert!(!in_view_cone(
            eye,
            Vec3::new(5.0, 1.4, 0.0),
            forward,
            sight,
            fov
        ));
        // 恰好视距：可见（闭边界；dist = 10.0 用 3D 距离精确表示）
        assert!(in_view_cone(
            eye,
            Vec3::new(0.0, 1.4, -10.0),
            forward,
            sight,
            fov
        ));
        // 超出视距 1 cm：不可见
        assert!(!in_view_cone(
            eye,
            Vec3::new(0.0, 1.4, -10.01),
            forward,
            sight,
            fov
        ));
        // 零角闭边界：fov_cos = 1.0 时正前方仍可见（== 判定为可见）
        assert!(in_view_cone(
            eye,
            Vec3::new(0.0, 1.4, -5.0),
            forward,
            sight,
            1.0
        ));
        // 偏 5° 即不可见（cos 5° < 1）
        let offset = Vec3::new(5.0 * 0.087_155_66, 1.4, -5.0);
        assert!(!in_view_cone(eye, offset, forward, sight, 1.0));
        // y 计入视距：水平 9.9 m + 眼胸口高差 0.5 → 3D 9.9126 ≤ 10 可见
        assert!(in_view_cone(
            eye,
            Vec3::new(0.0, 0.9, -9.9),
            forward,
            sight,
            fov
        ));
        // 同一水平距离 10.0：3D 10.0125 > 10 → 不可见（y 真的算进距离）
        assert!(!in_view_cone(
            eye,
            Vec3::new(0.0, 0.9, -10.0),
            forward,
            sight,
            fov
        ));
        // 退化重合（dist < 1e-4）→ 不可见
        assert!(!in_view_cone(eye, eye, forward, sight, fov));
        // 视距 ≤ 0 = 无视锥
        assert!(!in_view_cone(
            eye,
            Vec3::new(0.0, 1.4, -1.0),
            forward,
            0.0,
            fov
        ));
    }

    #[test]
    fn line_of_sight_is_blocked_by_static_shapes() {
        let eye = Vec3::new(0.0, 1.4, 0.0);
        let chest = Vec3::new(0.0, 0.9, -4.0);
        // 墙横在中间（z = -2）：挡
        let blocked = [box_wall(Vec3::new(0.0, 1.25, -2.0))];
        assert!(!has_line_of_sight(eye, chest, &blocked));
        // 墙挪到侧面（x = 4）：不挡
        let clear = [box_wall(Vec3::new(4.0, 1.25, -2.0))];
        assert!(has_line_of_sight(eye, chest, &clear));
        // 空列表：通视
        assert!(has_line_of_sight(eye, chest, &[]));
        // 合成：锥内但被墙挡 → 不可见（分辨「距离/夹角过了但遮挡没过」）
        assert!(in_view_cone(eye, chest, Vec2::new(0.0, -1.0), 10.0, 0.5));
        assert!(!can_see(
            eye,
            chest,
            Vec2::new(0.0, -1.0),
            10.0,
            0.5,
            &blocked
        ));
        // 旋转感知：同一根细胶囊（半径 0.2 / 半高 2），竖置挡线、绕 Y 转 90° 后不挡
        let shape = ColliderShape::Capsule {
            radius: 0.2,
            half_height: 2.0,
        };
        let vertical: StaticObstacle = (Vec3::new(0.0, 3.0, -2.0), Quat::IDENTITY, shape);
        assert!(
            !has_line_of_sight(eye, chest, &[vertical]),
            "竖置细杆（轴 y∈[1,5] @ z=-2）穿过 y≈1.15 的视线，应挡住"
        );
        let rotated: StaticObstacle = (
            Vec3::new(0.0, 3.0, -2.0),
            // 绕 Z 转 90°：本地 Y 轴（胶囊轴向）倒向世界 X——绕 Y 转是转不动轴向的
            Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
            shape,
        );
        assert!(
            has_line_of_sight(eye, chest, &[rotated]),
            "同一胶囊转 90°（轴改沿 x，离视线 1.85 m）不应再挡住"
        );
    }

    #[test]
    fn hearing_is_pure_distance_without_line_of_sight() {
        let source = Vec3::ZERO;
        let wall = [box_wall(Vec3::new(0.0, 1.25, -2.0))];
        let behind_wall = Vec3::new(0.0, 0.0, -4.0);
        assert!(within_hearing(source, Vec3::new(3.0, 0.0, 0.0), 4.0));
        assert!(
            within_hearing(source, Vec3::new(4.0, 0.0, 0.0), 4.0),
            "恰在半径上（闭边界）"
        );
        assert!(!within_hearing(source, Vec3::new(4.01, 0.0, 0.0), 4.0));
        assert!(!within_hearing(source, Vec3::ZERO, 0.0), "半径 0 = 无听觉");
        assert!(!within_hearing(source, Vec3::ZERO, -1.0), "负半径 = 无听觉");
        assert!(
            within_hearing(source, behind_wall, 5.0),
            "听觉无视线判定：隔墙照样听得见（声音绕障是 v1 的有意取舍）"
        );
        assert!(!has_line_of_sight(source, behind_wall, &wall));
    }

    #[test]
    fn flat_statics_marks_identity_rotation_and_keeps_geometry() {
        let flat = vec![(
            Vec3::new(1.0, 2.0, 3.0),
            ColliderShape::Sphere { radius: 0.5 },
        )];
        let obstacles = flat_statics(&flat);
        assert_eq!(obstacles.len(), 1);
        assert_eq!(obstacles[0].0, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(obstacles[0].1, Quat::IDENTITY, "关卡静态体不带旋转");
        assert_eq!(obstacles[0].2, ColliderShape::Sphere { radius: 0.5 });
        // 恒等旋转下与「不带旋转」的旧入口逐位等价（迁移兼容的根据）
        let from = Vec3::new(0.0, 0.0, 0.0);
        let to = Vec3::new(0.0, 0.0, 4.0);
        let manual = segment_hits_shape_rotated(
            from,
            to,
            Vec3::new(0.0, 0.0, 2.0),
            Quat::IDENTITY,
            &ColliderShape::Box {
                half_extents: Vec3::new(1.0, 1.0, 0.2),
            },
        );
        assert!(
            manual.is_some(),
            "线段应命中盒（与 physics::segment_hits_shape 同代码路径）"
        );
    }
    // ---- 系统：差分事件 / 双代缓冲 / 插入序无关 ----

    /// 感知源在原点（朝向恒等 → 前方 -Z），目标在前方 5 m；半角 60°、无听觉。
    fn scene() -> (World, Entity, Entity) {
        let mut world = World::new();
        world.register_event::<PerceptionEvent>();
        let observer = world.spawn((
            Tag("observer"),
            Transform::from_xyz(0.0, 0.0, 0.0),
            Perception::new(10.0, 0.5, 0.0),
        ));
        let target = world.spawn((
            Tag("target"),
            Transform::from_xyz(0.0, 0.0, -5.0),
            PerceptionTarget::default(),
        ));
        (world, observer, target)
    }

    fn place(world: &World, entity: Entity, position: Vec3) {
        world
            .get_mut::<Transform>(entity)
            .expect("应有 Transform")
            .position = position;
    }

    #[test]
    fn system_emits_seen_then_lost_then_seen_again() {
        let (world, observer, target) = scene();
        perception_system(&world);
        assert_eq!(drain_trace(&world), ["Seen(observer,target)"]);
        assert_eq!(
            world.get::<Perception>(observer).expect("应有组件").seen,
            vec![target],
            "运行态应记录本帧可见目标"
        );
        // 目标转到身后 → Lost（并清空运行态）
        place(&world, target, Vec3::new(0.0, 0.0, 5.0));
        perception_system(&world);
        assert_eq!(drain_trace(&world), ["Lost(observer,target)"]);
        assert!(
            world
                .get::<Perception>(observer)
                .expect("应有组件")
                .seen
                .is_empty()
        );
        // 回到前方 → Seen（再一次）
        place(&world, target, Vec3::new(0.0, 0.0, -5.0));
        perception_system(&world);
        assert_eq!(drain_trace(&world), ["Seen(observer,target)"]);
        // 状态未变：差分为空（不重复产事件）
        perception_system(&world);
        assert!(drain_trace(&world).is_empty(), "可见性未变不应重复产事件");
    }

    #[test]
    fn system_emits_heard_outside_the_cone() {
        let mut world = World::new();
        world.register_event::<PerceptionEvent>();
        // 零角视锥（fov_cos = 1：只有正前方）+ 听觉半径 4
        let observer = world.spawn((
            Tag("observer"),
            Transform::from_xyz(0.0, 0.0, 0.0),
            Perception::new(10.0, 1.0, 4.0),
        ));
        let target = world.spawn((
            Tag("target"),
            Transform::from_xyz(0.0, 0.0, 5.0),
            PerceptionTarget::default(),
        ));
        // 正后方 5 m：锥外、听觉半径外 → 无事件
        perception_system(&world);
        assert!(event_trace(&world).is_empty());
        // 拉近到 3 m：锥外仍可闻 → Heard
        place(&world, target, Vec3::new(0.0, 0.0, 3.0));
        perception_system(&world);
        assert_eq!(drain_trace(&world), ["Heard(observer,target)"]);
        {
            let perception = world.get::<Perception>(observer).expect("应有组件");
            assert!(perception.seen.is_empty(), "锥外的目标不得算「看见」");
            assert_eq!(perception.heard, vec![target]);
        }
        // 再拉近（仍在半径内）不重复产 Heard；走出半径也不产 Seen
        place(&world, target, Vec3::new(0.0, 0.0, 2.0));
        perception_system(&world);
        assert!(
            drain_trace(&world).is_empty(),
            "仍在听觉半径内不应重复产事件"
        );
    }

    #[test]
    fn double_generation_window_keeps_events_readable_until_drained() {
        let (mut world, _observer, _target) = scene();
        perception_system(&world);
        assert_eq!(event_trace(&world), ["Seen(observer,target)"]);
        // 帧首轮换（主循环每帧调用）：产出帧的事件降为「上一代」，下一帧仍可读
        world.clear_all_events();
        perception_system(&world);
        assert_eq!(
            event_trace(&world),
            ["Seen(observer,target)"],
            "上帧事件在下一帧仍可读（双代缓冲语义）"
        );
        // 再来一次轮换：连续两帧未被消费的残留被丢弃
        world.clear_all_events();
        assert!(event_trace(&world).is_empty());
    }

    #[test]
    fn system_is_insertion_order_independent() {
        /// 按给定插入序建世界 → 跑 2 帧 → 收集归一化摘要。
        fn run(order: [&'static str; 3]) -> (Vec<String>, Vec<String>) {
            let mut world = World::new();
            world.register_event::<PerceptionEvent>();
            for tag in order {
                match tag {
                    "observer" => {
                        world.spawn((
                            Tag("observer"),
                            Transform::from_xyz(0.0, 0.0, 0.0),
                            Perception::new(10.0, 0.5, 6.0),
                        ));
                    }
                    "near" => {
                        world.spawn((
                            Tag("near"),
                            Transform::from_xyz(0.0, 0.0, -4.0),
                            PerceptionTarget::default(),
                        ));
                    }
                    _ => {
                        world.spawn((
                            Tag("far"),
                            Transform::from_xyz(0.0, 0.0, 20.0),
                            PerceptionTarget::default(),
                        ));
                    }
                }
            }
            perception_system(&world);
            (event_trace(&world), state_trace(&world))
        }
        let forward = run(["observer", "near", "far"]);
        let reversed = run(["far", "near", "observer"]);
        assert_eq!(forward, reversed, "乱序 spawn 的两个世界应归一后逐位一致");
        assert_eq!(forward.0, ["Seen(observer,near)", "Heard(observer,near)"]);
        assert_eq!(forward.1, ["observer(seen=[near],heard=[near])"]);
    }

    #[test]
    fn system_runs_bit_identically_across_runs() {
        /// 同构造双跑：事件 dump 含**原始实体 id 的 bits**（不做标签归一）。
        fn trace() -> Vec<String> {
            let mut world = World::new();
            world.register_event::<PerceptionEvent>();
            let observer = world.spawn((
                Transform::from_xyz(0.0, 0.0, 0.0),
                Perception::new(12.0, 0.5, 8.0),
            ));
            let target = world.spawn((
                Transform::from_xyz(0.0, 0.0, -6.0),
                PerceptionTarget::default(),
            ));
            let mut dump = Vec::new();
            for frame in 0..8 {
                // 确定性轨迹：沿 x 往复（离散位置，无浮点累积歧义）
                let x = if frame % 2 == 0 { -4.0 } else { 4.0 };
                place(&world, target, Vec3::new(x, 0.0, -3.0));
                world.clear_all_events();
                perception_system(&world);
                for event in world.events::<PerceptionEvent>().iter() {
                    dump.push(format!("{event:?}@{frame}"));
                }
                dump.push(format!(
                    "state:{}",
                    world
                        .get::<Perception>(observer)
                        .expect("应有组件")
                        .seen
                        .iter()
                        .map(|entity| entity.to_bits())
                        .collect::<Vec<_>>()
                        .len()
                ));
            }
            dump
        }
        let a = trace();
        let b = trace();
        assert_eq!(a, b, "同构造两遍应逐位一致（含实体 id bits）");
        assert!(a.len() > 8, "样本应覆盖一段真实推进");
    }

    #[test]
    fn static_geometry_blocks_sight_through_the_system() {
        let mut world = World::new();
        world.register_event::<PerceptionEvent>();
        world.spawn((
            Tag("observer"),
            Transform::from_xyz(0.0, 0.0, 0.0),
            Perception::new(10.0, 0.5, 0.0),
        ));
        world.spawn((
            Tag("target"),
            Transform::from_xyz(0.0, 0.0, -5.0),
            PerceptionTarget::default(),
        ));
        // 静态墙横在中间（z = -2，y 抬高到视线高度）
        let wall = world.spawn((
            Transform::from_xyz(0.0, 1.25, -2.0),
            Collider::static_box(Vec3::new(1.0, 1.25, 0.2)),
        ));
        perception_system(&world);
        assert!(event_trace(&world).is_empty(), "墙后目标不可见（无 Seen）");
        // 挪开墙 → 下一帧 Seen（差分把「刚看见」记成 Seen）
        place(&world, wall, Vec3::new(6.0, 1.25, -2.0));
        perception_system(&world);
        assert_eq!(drain_trace(&world), ["Seen(observer,target)"]);
        // 再挡回去 → Lost
        place(&world, wall, Vec3::new(0.0, 1.25, -2.0));
        perception_system(&world);
        assert_eq!(drain_trace(&world), ["Lost(observer,target)"]);
    }
}
