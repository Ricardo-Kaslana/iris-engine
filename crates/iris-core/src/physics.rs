//! 物理与碰撞 v1：轻量刚体物理（Sphere / AABB / **旋转感知** Capsule），
//! 积分（线 + **角**，R50）→ 检测 → 响应 → 迭代松弛（接触 + 约束）。
//!
//! 与 [`crate::transform`] / [`crate::hierarchy`] 同款的「组件 + 单一系统」模型：
//! - [`Collider`]：碰撞体（形状 + 是否静态 + 弹性 + 触发区域 sensor）；
//! - [`RigidBody`]：刚体（速度 / **角速度**（R50）/ 重力缩放 / 摩擦 /
//!   线性 + **角**阻尼）；
//! - [`step_physics`]：每帧推进一步的物理系统（`&World` + 尝试型锁，可注册并行系统），
//!   帧末差分 [`ContactTracker`] 产出接触事件（见「接触事件与触发器」章节）。
//!
//! **R50 的两块新能力**：①**角速度积分**（[`integrate_rotation`]：四元数
//! 微分式 + 每步归一化）与 ②**旋转感知胶囊**（[`ColliderShape::Capsule`]
//! 的轴随实体 [`Transform::rotation`] 转，检测 / 射线 / 扫掠 / 推离全链
//! 跟随）——两者合起来支撑 ③[`SwingJoint`] 摆动限位关节与
//! [`crate::ragdoll`] v2 的胶囊肢人形（写实死亡表现）。**Box 仍是世界轴
//! 对齐 AABB**（OBB/SAT 留后续轮）：旋转体的碰撞请用胶囊。
//!
//! # 组件缺席语义
//!
//! - 只有 `Collider`（无 `RigidBody`）→ **静态体**（与 `is_static = true` 取或）：
//!   不积分、不移动，只作为碰撞对象（地面 / 墙 / 台面）；
//! - 只有 `RigidBody`（无 `Collider`）→ **仅受力积分**（重力 / 阻尼照常，
//!   不参与任何碰撞）；
//! - 两者都有且 `Collider::is_static == false` → 完整刚体（积分 + 碰撞）；
//!   `RigidBody::body_type` 选 [`BodyType::Kinematic`](BodyType) 时是
//!   **运动学碰撞体**：跳过积分与速度写回（位移由游戏每帧写
//!   `Transform`）、动体撞它按静态方推离、参与接触事件——语义全表见
//!   [`BodyType`] 文档。
//!
//! # 求解管线
//!
//! [`step_physics`] 只做编排，全部数值逻辑在纯函数 [`detect_pair`] /
//! [`solve_contact`] 中（可脱离世界单测）。每帧六步：
//!
//! 1. **两阶段收集**（照 `update_animations` 模式）：读 `Time.delta`
//!    （资源缺席视为 0），收集全部 `(Transform 位置 + 旋转, Collider,
//!    Option<RigidBody>, Entity)`，另收集只有 `RigidBody`（无 Collider）的
//!    「仅受力积分体」；
//! 2. **积分**（半隐式欧拉，仅动体）：`v += 重力 × gravity_scale × dt`；
//!    `v ×= exp(-linear_damping × dt)`；`pos += v × dt`；**角通道（R50）**
//!    `ω ×= exp(-angular_damping × dt)`、`q ← normalize(q + ½·(ω,0)·q·dt)`
//!    （四元数微分式，推导见 [`integrate_rotation`]）——**与平移不耦合**
//!    （位置积分不含 ω 项），且 `ω == 0` 时整段跳过（既有场景的
//!    `Transform.rotation` 逐位不被触碰）；
//! 3. **检测**：均匀网格 broadphase（[`crate::broadphase::BroadphaseGrid`]）
//!    先按 AABB 快照（**旋转胶囊的精确包络**）筛出候选对，再逐对
//!    [`detect_pair_rotated`] 精检（跳过静-静对与被 `groups`/`mask` 过滤的
//!    配对，见「碰撞过滤掩码」章节）。
//!    候选升序 + `j > i` 过滤 ⇒ 配对遍历序与朴素 O(n²) 双层循环**完全一致**
//!    （既有 bit 级确定性测试无需改动即守护等价性，见 [`step_physics`]）；
//! 4. **响应**：穿透深度沿法线分离（静态体不动，动体按动体数均分——
//!    等质量近似）；速度法向分量按 restitution 反弹（`v_n' = -e × v_n`，
//!    仅当朝向穿透，e 取双方最大）；切向分量乘 `max(0, 1 - friction)`
//!    （friction 取参与响应的动体的均值）；**接触不产生角冲量**（诚实
//!    限制，见「v1 限制」）；
//! 5. **松弛**：位置分离重复求解 [`SOLVER_ITERATIONS`] 次（速度响应只在
//!    首轮施加），减弱堆叠抖动；每轮接触之后追加**约束求解轮**（距离 →
//!    **摆动（R50）** → 目标点关节，见「约束与关节」章节）；
//! 6. **写回**：动体的 `Transform.position` / **`Transform.rotation`（R50）**
//!    / `RigidBody.velocity` / **`RigidBody.angular_velocity`（R50）**
//!    （静态体与运动学体不写——它们的 Transform 归游戏所有；两阶段处理
//!    保证迭代期间不持任何组件借用）。
//!
//! # 确定性
//!
//! 系统内部严格单线程、固定遍历序（收集序 = 组件存储序），同一输入两次
//! 模拟逐采样 bit 级一致（单测守护）。引擎侧配 `Time::advance` /
//! 固定步长驱动即可做确定性回放 / 联机同步。
//!
//! # 静态推离查询（运动学角色控制器）
//!
//! [`resolve_static_push`] / [`resolve_against_statics`]：**查询**而非系统
//! ——不积分、不写任何组件，把给定位置上的形状从全部静态体的重叠中沿
//! 法线推出，返回修正向量。消费方式完全由调用方决定（iris-game 的玩家
//! 控制器只取水平分量）：推离只沿接触法线 → 切向速度天然保留——这正是
//! 「贴墙滑行」手感的数学基础，也是游戏逻辑与物理库组合的推荐姿势
//! （kinematic 实体自管积分，物理库只回答「这里被什么挡住了」）。
//!
//! # 射线 / 线段查询（视线 · 拾取 · 高速命中）
//!
//! [`ray_sphere`] / [`ray_box`]（几何基元）→ [`ray_vs_shape`]（分发 +
//! `t ≤ max_t` 裁剪）→ [`segment_hits_shape`]（线段便捷封装，dir = 终点
//! − 起点、max_t = 长度）——高速离散体每帧位移可大于目标直径，当帧点
//! 判定会**穿模漏检**，线段扫掠判定是修法（iris-arena 的弹道命中即此）。
//! 世界级封装 [`raycast`] / [`raycast_filtered`]：遍历全部
//! `(Transform, Collider)`（静态动态都命中）取最小 t，纯查询不写组件。
//!
//! # 球扫掠查询（角色控制器地基）
//!
//! [`sphere_sweep`] / [`sphere_sweep_statics`]：半径 `r` 的球从 `origin`
//! 位移 `displacement`，问「首次碰到静态体的位移分数 t∈[0,1] 与阻挡
//! 法线」。实现为保守推进二分（复用 [`detect_pair_rotated`]，24 次收敛）
//! ——非解析解但确定性：同输入恒同输出。任意形状的泛化版本
//! [`shape_sweep`](crate::physics::shape_sweep) 同款算法——胶囊
//! （[`ColliderShape::Capsule`]）扫掠即由此获得；**R50 旋转感知变体**
//! [`shape_sweep_rotated`] 让查询形状与静态障碍（[`StaticObstacle`]
//! 三元组）都带旋转：倒地的斜置胶囊尸体照实阻挡 / 承托。角色控制器的
//! 正式消费方见 [`crate::character`]（`move_and_slide` 运动学控制器，
//! 世界封装已走旋转感知路径）。
//!
//! # 接触事件与触发器（begin/end + sensor）
//!
//! [`step_physics`] 帧末把本帧接触对与上一帧（[`ContactTracker`] 资源）
//! 差分，产出 [`CollisionStarted`] / [`CollisionEnded`] 事件——压力板 /
//! 警报区 / 着地回调从此**免逐帧轮询**。事件在帧首轮换前可读窗口覆盖
//! 「产出帧 + 下一帧」（见 `crate::event` 生命周期），每帧消费一次用
//! `events_mut::<E>().drain()`（&World 并行系统可用）。语义要点（实现
//! 与文档一一对应）：
//!
//! - **实体序归一化**：事件 a/b 恒满足 `a.index() ≤ b.index()`；检测
//!   原始方向与索引序相反时 `normal` 随归一化翻转（保持「从 a 指向 b」
//!   的 [`Contact`] 方向约定）；
//! - **首求解轮采样**：接触对与 normal / penetration 取自
//!   [`SOLVER_ITERATIONS`] 的**首轮**检测（位置分离前）——松弛轮的重复
//!   检出对是同一配对的求解残影，无 begin/end 语义；
//! - **静-静跳过**：静-静对互不动、配对永不变化，不进接触集合、不产
//!   事件（与求解的静-静跳过同一条分支）；
//! - **过滤配对同待遇**：被 `groups`/`mask` 过滤的配对同样不进接触
//!   集合、不产事件（掩码固定的「永不碰撞」与静-静的「永不变化」
//!   语义一致，见「碰撞过滤掩码」章节）；
//! - **销毁成对性**：接触中的实体 despawn / 移除组件后，下一帧必然收
//!   到携带已死实体 id 的 [`CollisionEnded`]（本帧集合只含存活实体的检
//!   出对，差分必然筛出消失配对）——begin/end 严格成对，消费方的计数
//!   / 状态不泄漏、世代安全；
//! - **sensor 正交性**：[`Collider::is_sensor`]（触发区域）与 `is_static`
//!   正交；sensor 对**照常进接触集合报事件**、但**跳过位置/速度求解**
//!   （不推离不反弹，彼此穿过）——两动体 sensor 相撞只报事件；
//! - **运动学体参与接触集合（打磨 V）**：`body_type == Kinematic` 的
//!   实体与动体、静态体（含 sensor）的配对照常检出、照常报事件——
//!   配对随游戏驱动的位移变化，begin/end 语义成立（运动学角色踩
//!   sensor 压力板不再需要动态代理绕行）；运动学 × 静态实体配对求解为
//!   no-op（双方 inv_mass = 0），运动学 × 运动学与静-静同待遇跳过；
//!   全语义见 [`BodyType`] 文档；
//! - **资源缺席语义**（照 `Time` 缺席静默跳过的风格）：`ContactTracker`
//!   或事件资源未注册则静默跳过对应产出，不 panic、不强制注册
//!   （`PhysicsPlugin` 已全部注册，游戏侧即得）；
//! - **事件顺序**：先 Started 后 Ended，各自按对序（BTree 字典序）输出
//!   ——同输入两次模拟的事件序列逐项一致（确定性守护）；
//! - **消费契约**：事件在 `step_physics`（Update 阶段）帧末产出，当帧
//!   其后注册的系统即可读；每帧**恰好消费一次**，且必须用
//!   `world.events_mut::<E>().drain()` 取走——事件可读窗口覆盖产出帧
//!   与下一帧（只 `iter` 不 `drain` 的消费方会在下一帧重复看到同一
//!   事件，begin 被重复计数）；drain 即消费，读到的是本帧物理结果
//!   （消费契约总纲见 `crate::event`）。
//!
//! 压力板用例（sensor 区域 + drain 事件 + 门开逻辑）：
//!
//! ```ignore
//! use iris_core::prelude::*;
//!
//! struct Door { plate: Entity, open: bool, occupants: u32 }
//!
//! // 事件驱动：物理帧末差分产出，本系统每帧恰好消费一次
//! fn update_pressure_plate(world: &World, door: &mut Door) {
//!     for event in world.events_mut::<CollisionStarted>().drain() {
//!         // 板是 sensor 区域：踏上即触发（实体序已归一化，双方都判）
//!         if event.a == door.plate || event.b == door.plate {
//!             door.occupants += 1;
//!             door.open = true;
//!         }
//!     }
//!     for event in world.events_mut::<CollisionEnded>().drain() {
//!         if event.a == door.plate || event.b == door.plate {
//!             door.occupants = door.occupants.saturating_sub(1);
//!             door.open = door.occupants > 0; // 板上仍有实体不关门
//!         }
//!     }
//! }
//!
//! // 板：sensor 盒（不阻挡不反弹，只报事件）；门逻辑自持计数
//! let plate = world.spawn((
//!     Transform::from_xyz(2.0, 0.05, 0.0),
//!     Collider::sensor_box(Vec3::new(0.8, 0.05, 0.8)),
//! ));
//! ```
//!
//! # 碰撞过滤掩码（groups / mask，R37）
//!
//! [`Collider::groups`]（所属位组）与 [`Collider::mask`]（碰撞白名单）
//! 是按语义层开关碰撞的通用能力（ragdoll 自碰撞免抖的前置：全部肢体同
//! 组位、mask 排除该位，肢体互不碰撞，见 [`crate::ragdoll`]）。语义要点
//! （实现与文档一一对应）：
//!
//! - **配对判定对称**：`(a.groups & b.mask) != 0 && (b.groups & a.mask)
//!   != 0` 才进入检测与求解——两个方向都认可才碰撞，任一方向为 0 即
//!   过滤（「a 想碰 b」但「b 不想碰 a」同样不碰：碰撞是配对的对称属性
//!   而非单方意愿）；
//! - **被过滤对与静-静对同待遇**：不检测、不求解、**不进接触事件集合**
//!   ——掩码固定的「永不碰撞」配对与静-静配对一样永不变化，begin/end
//!   无语义（sensor 正交性不受影响：过滤在 sensor 判定之前）；
//! - **broadphase 照旧**：过滤只作用于配对白名单、不做粗筛——网格候选
//!   的构建与遍历序完全不变（候选序 = 既有确定性遍历序，掩码判定是逐
//!   配对的一次位与，成本可忽略）；被过滤对的 `j ≤ i` / 静-静等既有
//!   跳过分支照常先行，**没有任何遍历顺序被改变** ⇒ bit 级确定性保持
//!   （既有确定性测试零改动守护）；
//! - **默认全 1**：全部构造辅助（[`Collider::static_box`] 等）产出的
//!   碰撞体 `groups` / `mask` 均为全 1——与引入过滤前的行为逐位一致，
//!   既有场景零迁移成本；需要过滤时字段直改（无辅助构造，示例见下）；
//! - **正交性**：过滤与 `is_static` / `is_sensor` / 关节均正交——关节
//!   不看掩码（约束是显式拓扑，[crate::ragdoll] 的肢体链靠它连通），
//!   sensor 照常报事件的前提是配对未被过滤。
//!
//! 分层碰撞用例（敌人子弹打玩家、敌我子弹互穿、装饰物不挡弹道）：
//!
//! ```ignore
//! use iris_core::prelude::*;
//!
//! const PLAYER: u32 = 1 << 1; // 位 0 已被 RAGDOLL_GROUP 占用，层位从 1 起
//! const ENEMY: u32 = 1 << 2;
//!
//! // 玩家：被一切打（mask 全开）；敌人子弹：只打玩家（mask 仅 PLAYER）
//! world.spawn((
//!     Transform::from_xyz(0.0, 0.9, 0.0),
//!     Collider::static_capsule(0.4, 0.9),
//! ));
//! let mut bullet = Collider::dynamic_sphere(0.08, 0.0);
//! bullet.groups = ENEMY;
//! bullet.mask = PLAYER; // 敌方子弹不与场景 / 敌人自身碰撞，只命中玩家
//! world.spawn((
//!     Transform::from_xyz(3.0, 1.0, 0.0),
//!     bullet,
//!     RigidBody { velocity: Vec3::new(-6.0, 0.0, 0.0), ..RigidBody::default() },
//! ));
//! ```
//!
//! # 约束与关节（位置约束集：距离 / 摆动 / 目标点）
//!
//! 机关门 / 吊灯 / 弹簧板（R36）与**真人形 ragdoll**（R50）的地基。
//! R50 之前 v1 是纯位置物理（只有线速度），语义完整的旋转铰链无法诚实
//! 实现——**角速度积分 + 旋转感知胶囊落地后，旋转约束随之解锁**：
//! [`SwingJoint`]（摆动锥限位 + 肢体轴方向修正）是第一个真正消费旋转的
//! 约束，[`crate::ragdoll`] v2 的胶囊肢人形靠它接线肩 / 胯 / 肘 / 膝。
//! 仍**没有** hinge / twist 两自由度铰链（限制「只能单向弯」需要它），
//! v1 用锥近似（见诚实边界）。三个关节组件：
//!
//! - [`DistanceJoint`]：距离约束——`stiffness = 1` 硬约束（双向刚杆）、
//!   `(0, 1)` 弹簧（部分修正投影）+ 轴向相对速度 `damping`；
//! - [`SwingJoint`]（R50）：**摆动关节**——本地锚点（随各自实体旋转）的
//!   距离投影 + 摆动锥限位（锥轴随父端旋转）+ 子端肢体轴的最小弧方向
//!   修正 + 两轴速度阻尼；修正**全给子端**（层级投影，见类型文档的实测
//!   论证）；
//! - [`TargetJoint`]：目标点约束——把实体拉向世界固定点，`rest_distance`
//!   给出「停在距目标多近」的到达半径（0 = 精确到达）。
//!
//! ## 求解在管线中的位置
//!
//! [`step_physics`] 的松弛大循环（[`SOLVER_ITERATIONS`] 轮）内，每轮
//! **先接触后约束**：全部接触对求解完毕后追加一轮约束求解。顺序理由：
//! **约束优先级高于接触**——机关门被挤进墙体时，先把门投影回关节语义
//! 位置（结构完整性），再让下一轮接触分离穿透（外观修正）；反过来会
//! 让接触推离持续对抗约束、结构撕裂。位置修正即时生效（Gauss-Seidel
//! 风格，与接触分离同款）；**速度响应只在首轮**施加（与接触速度响应
//! 同轮同理由——重复结算会放大阻尼）。修正后的 `positions` / `rotations`
//! 快照照常走既有写回阶段——约束**零额外写回代码**（摆动关节的旋转
//! 修正是绝对覆盖，同轮后解的关节能看到前面改过的朝向 ⇒ 肢体链自根
//! 向叶逐级对齐）。
//!
//! 约束轮内的求解顺序固定：**距离关节 → 摆动关节 → 目标点关节**，各自
//! 按收集序（= 组件存储序）。
//!
//! - **距离约束**：当前距离 d 与 `rest_length` 偏差 `err = d − rest`，
//!   沿连线按动体数均分修正（静态端 share = 0 不动）；弹簧
//!   `stiffness < 1` 即部分修正——**迭代性质**：每轮部分修正 ×
//!   [`SOLVER_ITERATIONS`] 轮 × 多帧 ≈ 收敛到平衡，非一步到位；
//! - **摆动约束（R50）**：三段（锚点距离投影 → 锥面拉回 → 肢体轴方向
//!   修正），全部**只作用于子端**（层级投影）⇒ 在「根先叶后」的求解序下
//!   每轮末尾全部摆动约束精确满足（实测残差 1e-7 = f32 舍入量级）；
//!   数学与退化规则见 [`solve_swing_joint`]；
//! - **目标点约束**：单端向 target 修正，进入 `rest_distance` 半径即
//!   不修（到达语义）；速度响应把「沿目标轴」分量按 `1 − stiffness`
//!   缩放、切向分量按 `1 − damping` 缩放——到达时速度归零的临界阻尼
//!   **近似**（精确临界解需要力模型与质量，v1 用速度缩放代替，诚实
//!   声明，见 [`solve_target_joint`]）；
//! - **确定性**：约束收集序 = 组件存储序（query 遍历序）、两端位置 /
//!   旋转取自同一快照、求解顺序固定（距离 → 摆动 → 目标点）、无随机源
//!   ——既有逐位确定性测试不受影响，关节场景同样 bit 级可复现（单测守护）。
//!
//! ## 诚实边界（v1 约束）
//!
//! - **无能量守恒**：位置松弛投影不是力积分——弹簧不存储势能、无共振
//!   频率，「弹性」是每轮部分修正的视觉弹跳语义；需要精确能量行为的
//!   场景请由游戏逻辑自管；
//! - **双向刚杆**：距离约束靠近与远离都修正（非「只拉不推」的绳）；
//! - **锥限位不区分弯曲平面**：肘 / 膝的「只能单向弯」需要 hinge + twist
//!   两自由度约束，[`SwingJoint`] 用锥近似（超限方向一律拉回锥面）；
//! - **摆动关节是层级投影**：子端被投影到父端，父端不被子端拽动 ⇒ 躯干
//!   靠自身接触承托、不靠腿的约束提着（尸体要的语义）；代价是**链必须
//!   根先叶后**（[`crate::ragdoll::RagdollDesc::links`] 的定义序），乱序
//!   会退回 Gauss-Seidel 残差（不 panic、不断言）；
//! - **挂载体须为碰撞体动体**：关节挂在无 [`Collider`] 或静态的实体上
//!   被跳过（静态体不写回，约束无从生效）；`target` 端可以是静态体、
//!   动态体或**无碰撞体的锚点实体**（只有 [`Transform`](crate::transform::Transform)
//!   也行——关节不要求碰撞）；此类锚点按本帧 Transform 快照视为固定
//!   点，若锚点本身是仅 [`RigidBody`] 的飞行体，关节视角下等效固定于
//!   帧初位置；
//! - **退化 / 无效跳过**：两端重合（约束轴不定）、`rest_offset` 零长
//!   （无锥轴）、`target` 实体 despawn / 从未存在 → 约束跳过不 panic
//!   ——与实体世代语义一致（不做悬垂 panic）。
//!
//! 吊灯用例（静态锚点 + 动态球 + 硬距离关节，悬挂验证场景）：
//!
//! ```ignore
//! use iris_core::prelude::*;
//!
//! // 锚点：只有 Transform 也行（关节不要求碰撞）
//! let anchor = world.spawn((Transform::from_xyz(0.0, 6.0, 0.0),));
//! // 吊灯球：被 rest = 2 的硬距离关节拉住，悬在锚点下方 2 m 处稳定
//! world.spawn((
//!     Transform::from_xyz(0.0, 5.0, 0.0),
//!     Collider::dynamic_sphere(0.3, 0.0),
//!     RigidBody { linear_damping: 0.2, ..RigidBody::default() },
//!     DistanceJoint {
//!         target: anchor,
//!         rest_length: 2.0,
//!         stiffness: 1.0, // 硬约束；改 0.1 即弹簧（吊灯摇晃缓冲）
//!         damping: 0.5,   // 轴向阻尼：抑制持续弹跳
//!     },
//! ));
//! ```
//!
//! # v1 限制（诚实清单）
//!
//! - **AABB 不随实体旋转（OBB 留后续轮）**：[`ColliderShape::Box`] 是世界
//!   轴对齐包围盒，`Transform.rotation` 对它**无语义**（检测忽略）——旋转
//!   实体的碰撞盒不跟着转；**旋转体请用胶囊**（R50 起胶囊轴随实体旋转，
//!   是唯一旋转感知的形状）。OBB / SAT 升级是后续轮的事，本轮范围明确
//!   不含（Box 的 AABB 语义一字未动 ⇒ 既有场景逐位不变）；
//! - **接触不产生角冲量（R50 诚实限制）**：碰撞只改**位置与线速度**——
//!   落地不会自动「滚起来」、偏心撞击不会让物体打转。角动量只由
//!   「初值（[`RigidBody::angular_velocity`]）+ 角阻尼 + 约束（
//!   [`SwingJoint`] 的方向修正）」演化。ragdoll 的翻滚表现因此来自
//!   死亡时注入的 ω 初值（[`crate::ragdoll::release_ragdoll`]）+ 关节
//!   方向修正 + 接触推离的组合，而不是真实的转动动力学；需要「撞哪儿转
//!   哪儿」请等角冲量轮（需要惯性张量与接触点臂）；
//! - **物理体须为根实体**：`step_physics` 把 `Transform.position` /
//!   `rotation` 当世界坐标读写——与变换层级并用时请自行保证物理体没有
//!   父级（或父级变换恒等）；
//! - **非迭代完整求解器**：等质量冲量 + 2 次位置松弛，重堆叠场景松弛不
//!   彻底（残余穿透 / 缓慢滑移），不是完整 LCP 求解器；
//! - **约束 v1 是位置松弛投影**：无能量守恒（弹簧无势能 / 共振）、距离
//!   关节双向刚杆、摆动关节层级投影 + 锥形限位（不区分弯曲平面）——完整
//!   清单见「约束与关节」章节的诚实边界；
//! - **粗检测为 AABB 网格快照**：均匀网格按「积分后、求解前」的 AABB
//!   一次构建，求解期位置 / 旋转修正（穿透量级）不重建——常规场景修正量
//!   远小于 cell 尺寸、候选为保守超集；极端穿透（接近 cell 尺寸）理论上
//!   可能漏对。
//!
//! # 用法
//!
//! ```ignore
//! use iris_core::prelude::*;
//!
//! // 地面：静态碰撞体（无 RigidBody = 静态语义）
//! world.spawn((
//!     Transform::from_xyz(0.0, -1.0, 0.0),
//!     Collider::static_box(Vec3::new(30.0, 1.0, 30.0)), // 顶面 y = 0
//! ));
//! // 球：完整刚体，从 5 m 高度落下
//! world.spawn((
//!     Transform::from_xyz(0.0, 5.0, 0.0),
//!     Collider::dynamic_sphere(0.5, 0.8),
//!     RigidBody { friction: 0.4, linear_damping: 0.1, ..RigidBody::default() },
//! ));
//! // 竖直胶囊（角色碰撞体：半高 0.9 ≥ 半径 0.4，脚底 = 中心 y − 0.9）——
//! // 配 iris-core::character 的 move_and_slide 做运动学角色控制器
//! world.spawn((
//!     Transform::from_xyz(0.0, 0.9, 0.0),
//!     Collider::static_capsule(0.4, 0.9),
//! ));
//! ```

use glam::{Quat, Vec3};

use crate::Component;
use crate::Event;
use crate::Resource;
use crate::broadphase::BroadphaseGrid;
use crate::entity::Entity;
use crate::time::Time;
use crate::transform::Transform;
use crate::world::World;

/// 标准重力（米/秒²，世界 -Y 向下）。可用 [`RigidBody::gravity_scale`]
/// 缩放（0 = 失重、负值反向）。
pub const PHYSICS_GRAVITY: Vec3 = Vec3::new(0.0, -9.81, 0.0);

/// 位置松弛迭代次数：每帧对全部接触重复「检测 → 位置分离」这么多次，
/// 减弱堆叠场景的单帧残余穿透（速度响应只在首轮施加，见 [`solve_contact`]）。
pub const SOLVER_ITERATIONS: usize = 2;

/// 碰撞形状。Sphere 与世界轴对齐 Box（AABB **不随实体旋转**——OBB/SAT
/// 留后续轮）+ **旋转感知** Capsule（R50：轴 = 实体
/// [`Transform::rotation`](crate::transform::Transform) 旋转后的本地 ±Y，
/// 检测 / 射线 / 扫掠全链跟随；`Quat::IDENTITY` 即恒竖直，与 R31 逐位一致）。
#[derive(Clone, Copy, Debug, PartialEq, Component)]
pub enum ColliderShape {
    /// 球体（各向同性：`rotation` 无语义，检测忽略之）。
    Sphere {
        /// 半径（米）。
        radius: f32,
    },
    /// 轴对齐包围盒（AABB，**不随实体旋转**：`rotation` 被忽略——旋转体的
    /// 碰撞请用胶囊，见模块文档 v1 限制）。
    Box {
        /// 半边长（米）。
        half_extents: Vec3,
    },
    /// 胶囊：圆柱段 + 两端半球端帽（角色控制器的标准形状，也是 R50 起
    /// 唯一**随实体旋转**的形状——ragdoll 肢体用它重建四肢朝向）。
    /// 轴段端点 = 中心 ± (half_height − radius)·**旋转后的本地 +Y**
    /// （`rotation == Quat::IDENTITY` 时即恒竖直，与既有行为逐位一致）；
    /// `half_height ≥ radius`（构造辅助有断言；取等号时退化为球——零长
    /// 轴段，旋转同样无语义）。
    Capsule {
        /// 端帽半球半径（米），也是垂直轴向的最大半宽。
        radius: f32,
        /// 胶囊半高（米）：整体包围盒沿轴向的半长（须 ≥ radius）。
        half_height: f32,
    },
}

/// 碰撞体组件：形状 + 是否静态 + 弹性 + 是否触发区域（sensor）+
/// 碰撞过滤掩码（`groups` / `mask`，见模块文档「碰撞过滤掩码」章节）。
#[derive(Clone, Copy, Debug, PartialEq, Component)]
pub struct Collider {
    pub shape: ColliderShape,
    /// 静态体（地面 / 墙 / 台面）不移动也不积分；缺席 [`RigidBody`] 时
    /// 同样按静态处理（与该标记取或）。
    pub is_static: bool,
    /// 弹性系数（0..1）：碰撞后法向速度的保留比例；接触组合取双方最大。
    pub restitution: f32,
    /// **触发区域**（sensor / trigger）：接触照常检出、照常产出
    /// [`CollisionStarted`] / [`CollisionEnded`] 事件（压力板 / 警报区 /
    /// 着地回调），但**跳过位置与速度求解**——不推离、不反弹、不摩擦，
    /// 实体彼此穿过。与 `is_static` **正交**：sensor 也可以动（两动体
    /// sensor 相撞只报事件、互不阻挡）。
    pub is_sensor: bool,
    /// **所属位组**（R37）：本实体归属哪些碰撞组（按位多选）。默认全 1
    /// ——一切位组都在场，与引入过滤前的行为逐位一致。典型用法：一个
    /// 语义层一个位（层「玩家」「敌人」「布娃娃」），见 [`crate::ragdoll`]
    /// 的 `RAGDOLL_GROUP`。
    pub groups: u32,
    /// **碰撞白名单**（R37）：本实体愿意与哪些位组碰撞（按位多选）。
    /// 配对判定**对称**：`(a.groups & b.mask) != 0 && (b.groups & a.mask)
    /// != 0` 才检测求解；被过滤的配对**不进接触事件集合**（与静-静对同
    /// 待遇）。默认全 1 = 与一切碰撞（既有行为零变化）；过滤用字段直改
    /// （构造辅助保持全开，见模块文档「碰撞过滤掩码」）。
    pub mask: u32,
}

impl Collider {
    /// 静态 AABB（地面 / 墙 / 台面），弹性 0。
    pub fn static_box(half_extents: Vec3) -> Self {
        Self {
            shape: ColliderShape::Box { half_extents },
            is_static: true,
            restitution: 0.0,
            is_sensor: false,
            groups: u32::MAX,
            mask: u32::MAX,
        }
    }

    /// 静态球，弹性 0。
    pub fn static_sphere(radius: f32) -> Self {
        Self {
            shape: ColliderShape::Sphere { radius },
            is_static: true,
            restitution: 0.0,
            is_sensor: false,
            groups: u32::MAX,
            mask: u32::MAX,
        }
    }

    /// 动态球（刚体参数在 [`RigidBody`] 上）。
    pub fn dynamic_sphere(radius: f32, restitution: f32) -> Self {
        Self {
            shape: ColliderShape::Sphere { radius },
            is_static: false,
            restitution,
            is_sensor: false,
            groups: u32::MAX,
            mask: u32::MAX,
        }
    }

    /// 动态 AABB。
    pub fn dynamic_box(half_extents: Vec3, restitution: f32) -> Self {
        Self {
            shape: ColliderShape::Box { half_extents },
            is_static: false,
            restitution,
            is_sensor: false,
            groups: u32::MAX,
            mask: u32::MAX,
        }
    }

    /// 静态胶囊（角色 / 胶囊柱 / 横倒圆柱），弹性 0。`half_height ≥ radius`
    /// 断言。轴随实体 [`Transform::rotation`](crate::transform::Transform)
    /// （R50）——恒等旋转即竖直。
    pub fn static_capsule(radius: f32, half_height: f32) -> Self {
        Self {
            shape: ColliderShape::capsule(radius, half_height),
            is_static: true,
            restitution: 0.0,
            is_sensor: false,
            groups: u32::MAX,
            mask: u32::MAX,
        }
    }

    /// 动态胶囊（轴随实体旋转，R50——ragdoll 肢体即此）。
    pub fn dynamic_capsule(radius: f32, half_height: f32, restitution: f32) -> Self {
        Self {
            shape: ColliderShape::capsule(radius, half_height),
            is_static: false,
            restitution,
            is_sensor: false,
            groups: u32::MAX,
            mask: u32::MAX,
        }
    }

    /// **触发球**（sensor）：不阻挡不反弹，只产出接触事件（拾取圈 /
    /// 爆炸伤害范围）。静态语义（无 `RigidBody` 时本就静态；要动态
    /// sensor 请把结构体的 `is_static` 置 `false` 并挂 `RigidBody`）。
    pub fn sensor_sphere(radius: f32) -> Self {
        Self {
            shape: ColliderShape::Sphere { radius },
            is_static: true,
            restitution: 0.0,
            is_sensor: true,
            groups: u32::MAX,
            mask: u32::MAX,
        }
    }

    /// **触发 AABB**（sensor）：不阻挡不反弹，只产出接触事件（压力板 /
    /// 警报区 / 检查点）。静态语义（同 [`Collider::sensor_sphere`]）。
    pub fn sensor_box(half_extents: Vec3) -> Self {
        Self {
            shape: ColliderShape::Box { half_extents },
            is_static: true,
            restitution: 0.0,
            is_sensor: true,
            groups: u32::MAX,
            mask: u32::MAX,
        }
    }
}

impl ColliderShape {
    /// 构造胶囊形状：`half_height ≥ radius` 断言（中文断言消息，半高小于
    /// 半径的「胶囊」不再是凸的胶囊语义）。轴 = 实体
    /// [`Transform::rotation`](crate::transform::Transform) 旋转后的本地
    /// ±Y（恒等旋转即竖直，见 [`ColliderShape::Capsule`]）。
    pub fn capsule(radius: f32, half_height: f32) -> Self {
        assert!(
            half_height >= radius,
            "胶囊 half_height {half_height} 须 ≥ radius {radius}（半高小于半径退化为非胶囊形状）"
        );
        Self::Capsule {
            radius,
            half_height,
        }
    }
}

/// 刚体组件：动体才有（静态体只有 [`Collider`]）。
#[derive(Clone, Copy, Debug, PartialEq, Component)]
pub struct RigidBody {
    /// 线速度（米/秒）。`body_type = Kinematic` 时本字段不参与积分也不被
    /// 写回（位移由游戏每帧写 `Transform`，语义见 [`BodyType::Kinematic`]）。
    pub velocity: Vec3,
    /// 重力缩放：1.0 = 标准重力（[`PHYSICS_GRAVITY`]），0 = 失重，负值反向。
    pub gravity_scale: f32,
    /// 摩擦：接触时切向速度的每帧衰减系数（把切向相对速度乘
    /// `max(0, 1 - friction)`——近似库仑摩擦的廉价替代，非严格冲量模型）。
    pub friction: f32,
    /// 线性阻尼：每秒指数速度衰减（`v ×= exp(-damping × dt)`，空气阻力近似）。
    pub linear_damping: f32,
    /// **角速度**（R50，rad/s，轴角式）：方向 = 瞬时转轴（右手系）、模长 =
    /// 角速率。每帧按四元数微分式积分进 [`Transform::rotation`]（见
    /// [`integrate_rotation`]）。`body_type = Kinematic` 时与 `velocity`
    /// 同样不参与积分也不被写回（旋转由游戏每帧写 `Transform`）。
    ///
    /// **角动量只由「初值 + 阻尼 + 约束」演化**：接触**不产生角冲量**
    /// （v1 诚实限制，见模块文档）——碰撞只改位置与线速度，落地不会
    /// 自动「滚起来」；翻滚表现由生成 / 释放时写入的角速度初值提供
    /// （[`crate::ragdoll::release_ragdoll`] 的冲量相关注入即此）。
    pub angular_velocity: Vec3,
    /// **角阻尼**（R50）：每秒指数角速度衰减（`ω ×= exp(-damping × dt)`，
    /// 与 [`RigidBody::linear_damping`] 同式同语义）。默认 0 = 无阻尼
    /// （角速度恒定，旋转匀速持续）；ragdoll 用它把死亡翻滚在数秒内停下。
    pub angular_damping: f32,
    /// 刚体类型（打磨 V）：[`BodyType::Dynamic`]（默认，完整动力学）或
    /// [`BodyType::Kinematic`]（游戏驱动位移、参与碰撞，见类型文档）。
    pub body_type: BodyType,
}

impl Default for RigidBody {
    fn default() -> Self {
        Self {
            velocity: Vec3::ZERO,
            gravity_scale: 1.0,
            friction: 0.0,
            linear_damping: 0.0,
            angular_velocity: Vec3::ZERO,
            angular_damping: 0.0,
            body_type: BodyType::Dynamic,
        }
    }
}

/// 角速度积分（R50 纯函数）：一步 `dt` 的**角阻尼 + 四元数微分式**积分，
/// 返回 `(新旋转, 新角速度)`——脱离世界可单测，与 `solve_contact` 同款
/// 「数值逻辑提纯」口径。
///
/// # 数学（推导）
///
/// 1. **阻尼**（与线速度同式，解析解 `ω(t) = ω₀·e^(−λt)`）：
///    `ω ← ω·exp(−angular_damping·dt)`；半隐式口径——先衰减再积分，
///    与 `step_physics` 的线速度「先阻尼后位移」一致；
/// 2. **旋转**：单位四元数的运动学微分方程为
///    `dq/dt = ½·(ω, 0) ⊗ q`（`(ω, 0)` = 纯虚四元数，`⊗` = 四元数乘；
///    该式等价于「以角速度 ω 刚体旋转」的旋量表述，推导：速度场
///    `v = ω × r` 对应旋转矩阵微分 `Ṙ = [ω]× R`，映射到四元数即得）。
///    一步显式（半隐式欧拉的旋转版）：`q ← q + ½·(ω, 0) ⊗ q·dt`；
/// 3. **归一化**：上式只在 `dt → 0` 时保范（一阶截断引入 `O(dt²)` 的
///    范数漂移，长时积分会平方复合放大——打磨 VI 的 IK 四元数教训），
///    故每步 `normalize()`：`|q| ≡ 1` 是旋转语义的前提，也是确定性
///    逐位复现的前提（漂移会让同输入的两次长跑分岔）。
///
/// # 确定性与兼容
///
/// 无随机源、无迭代分支——同输入逐位同输出（单测 `to_bits` 守护）。
/// `angular_velocity == Vec3::ZERO` 时 [`step_physics`] **跳过本函数与
/// 旋转写回**（不是本函数的分支）：既有场景（无角速度）的
/// [`Transform::rotation`] 逐位不被触碰，`normalize` 的末位舍入也不会
/// 逐帧改写既有姿态。
pub fn integrate_rotation(
    rotation: Quat,
    angular_velocity: Vec3,
    angular_damping: f32,
    dt: f32,
) -> (Quat, Vec3) {
    let damped = angular_velocity * (-angular_damping * dt).exp();
    // 纯虚四元数 (ω, 0)：四元数微分式 dq/dt = ½·(ω,0) ⊗ q
    let spin = Quat::from_xyzw(damped.x, damped.y, damped.z, 0.0);
    let next = (rotation + spin * rotation * (0.5 * dt)).normalize();
    (next, damped)
}

/// 刚体类型（打磨 V）。
///
/// - [`BodyType::Dynamic`]（默认）：完整动力学——重力 / 阻尼积分、接触
///   推离与反弹、速度写回，v1 行为逐位不变；
/// - [`BodyType::Kinematic`]：**游戏驱动位置**——跳过重力 / 阻尼积分与
///   速度写回（`RigidBody.velocity` 对它无语义），位移由游戏每帧直接写
///   `Transform`；但**参与碰撞与接触事件**：
///   - 动体撞它按「静态方」路径被推离（inv_mass = 0，全部修正给动体），
///     它自己不被物理挪动——弹簧门板被角色推开、布娃娃被角色撞飞即此；
///   - 与静态体（含 sensor）的配对**照常进接触集合**产出
///     [`CollisionStarted`] / [`CollisionEnded`]——kinematic 角色踩
///     sensor 压力板不再需要「动态物理代理」绕行（§8.22⑥ 的闭合）；
///     配对的变化由游戏驱动的位移产生，begin/end 语义成立；
///   - kinematic × kinematic 配对与静-静同待遇：跳过检测与事件（两者
///     位置都由游戏驱动，物理求解对它们无话可说）；
///   - 在全部「静态体」查询口径（`resolve_against_statics` /
///     `sphere_sweep_statics` / `move_and_slide` 的世界收集）中按**静态
///     障碍**处理——角色控制器撞上 kinematic 体被挡住是合理语义。
///
/// # 组件组合语义（更新缺席语义表）
///
/// - 只有 `Collider`（无 `RigidBody`）→ 静态体（不变）；
/// - `Collider` + `RigidBody { body_type: Dynamic, .. }` → 完整刚体；
/// - `Collider` + `RigidBody { body_type: Kinematic, .. }` → 运动学碰撞体
///   （游戏写 `Transform`，物理只回答「谁撞上了谁、把动体推去哪」）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BodyType {
    /// 完整动力学（默认）。
    #[default]
    Dynamic,
    /// 运动学：游戏驱动位移、参与碰撞与接触事件、不参与动力学积分。
    Kinematic,
}

/// 「求解视角的静态」判定：[`Collider::is_static`]、缺席 [`RigidBody`]
/// （组件缺席语义）或 `body_type == Kinematic`（游戏驱动位移、碰撞按
/// 静态方处理）——积分 / 写回 / 推离查询 / 角色控制器的静态体收集统一
/// 走本函数（各处口径一致，见 [`BodyType::Kinematic`] 文档）。
pub fn effective_static(collider: &Collider, rigid: Option<&RigidBody>) -> bool {
    collider.is_static
        || rigid.is_none()
        || rigid.is_some_and(|body| body.body_type == BodyType::Kinematic)
}

/// 距离关节：把挂载实体与 `target` 实体的**间距**约束到 `rest_length`
/// （v1 位置约束集，语义规范 = 模块文档「约束与关节」章节）。
///
/// - `stiffness ∈ (0, 1]`：`1` 为**硬约束**（每轮松弛把间距全额投影回
///   `rest_length`——双向「刚杆」语义，靠近与远离都修正，非只拉不推的
///   绳）；`(0, 1)` 为**弹簧**（按比例部分修正——位置物理的弹簧是**松弛
///   投影**而非力积分：无能量守恒、无共振频率，是「视觉弹跳」语义的
///   诚实近似；每轮部分修正 × [`SOLVER_ITERATIONS`] 轮 × 多帧 ≈ 指数
///   收敛到 `rest_length`）。越界值在求解时钳制到 [0, 1]；
/// - `damping ∈ [0, 1)`：轴向**相对**速度的每帧衰减（`v_rel_axis ×=
///   (1 − damping)`，按动体数均分写回——阻尼器语义，抑制弹簧的持续
///   弹跳；只在求解首轮施加，与接触速度响应同轮同理由）。
///
/// **无效目标**：`target` 实体已 despawn / 从未存在 / 无
/// [`Transform`](crate::transform::Transform) → 本帧约束整体跳过（不
/// panic、不悬垂——与实体世代语义一致）。**挂载体**须为参与物理的动体
/// （有 [`Collider`] 且非静态），否则组件同样被跳过。
#[derive(Clone, Copy, Debug, PartialEq, Component)]
pub struct DistanceJoint {
    /// 另一端实体；无效实体 = 约束跳过（见类型文档）。
    pub target: Entity,
    /// 两端保持的距离（米）。
    pub rest_length: f32,
    /// 松弛强度 `(0, 1]`：1 = 硬约束（全额投影），`(0, 1)` = 弹簧。
    pub stiffness: f32,
    /// 轴向相对速度每帧衰减 `[0, 1)`。
    pub damping: f32,
}

/// 目标点关节：把挂载实体拉向**世界固定点**（机关门开合 / 移动平台的
/// 物理形态；v1 位置约束集，语义规范 = 模块文档「约束与关节」章节）。
///
/// - `rest_distance`：停在距目标多近（**到达语义**——实体进入该半径后
///   不再修正，停在球壳上；`0` = 精确到达目标点）；
/// - `stiffness ∈ (0, 1]`：每轮松弛向目标的部分修正比例（1 = 全额
///   投影到球壳），越界值钳制到 [0, 1]；
/// - `damping ∈ [0, 1)`：**切向**速度的每帧衰减（与 [`DistanceJoint`]
///   的「轴向」阻尼正交互补——目标点约束的速度响应把「沿目标轴」分量
///   按 `1 − stiffness` 缩放（临界阻尼的工程近似，见纯函数
///   [`solve_target_joint`] 文档），切向分量按 `1 − damping` 衰减，
///   两者合力把实体阻尼到静止在球壳上）。
///
/// 目标是世界坐标（非实体），无无效目标语义；挂载体限制同
/// [`DistanceJoint`]（须为碰撞体动体，否则跳过）。
#[derive(Clone, Copy, Debug, PartialEq, Component)]
pub struct TargetJoint {
    /// 世界固定点（米）。
    pub target: Vec3,
    /// 停在距目标多近（米）；`0` = 精确到达。
    pub rest_distance: f32,
    /// 松弛强度 `(0, 1]`。
    pub stiffness: f32,
    /// 切向速度每帧衰减 `[0, 1)`。
    pub damping: f32,
}

/// 摆动关节（R50）：把挂载实体（**肢体侧 a**）与 `target`（**父端 /
/// 躯干侧 b**）用「锚点距离 + 摆动锥限位 + 肢体轴方向修正」三段位置约束
/// 连起来——真人形 ragdoll 的肩 / 胯 / 肘 / 膝语义（见 [`crate::ragdoll`]
/// 的 v2 自动接线）。语义规范 = 模块文档「摆动关节」章节。
///
/// # 锚点与 rest
///
/// - **世界锚点**：`pa = pos_a + rot_a·anchor_a`、`pb = pos_b + rot_b·anchor_b`
///   ——本地锚随各自实体旋转，关节窝「长在骨头上」：躯干翻滚时窝跟着走，
///   肢体不会被钉在世界坐标上；
/// - **`rest_offset`**：静止时「b 锚 − a 锚」的偏移，**表达在父端 b 的
///   本地坐标系**（生成时 = `rot_b⁻¹·(pb − pa)`）。它同时给出两件事：
///   - **长度** = 锚点距离约束的 rest（`|pb − pa| → |rest_offset|`；
///     典型接线是「a 锚 = 肢体中心、b 锚 = 父端关节窝」⇒ rest = 半肢长，
///     即肢体的近端帽被钉在关节窝上）；
///   - **方向** = 摆动锥的**锥轴**，世界向 = `rot_b·rest_offset.normalize()`
///     ——**随父端旋转**：躯干倒下时肘部的「不许反折」方向跟着倒下
///     （解剖学口径）；若锥轴取世界固定方向，翻倒的尸体会被限位强行把
///     肢体钉回出生时的世界朝向（错误表现）。
///
/// # 三段求解（每轮松弛、距离关节之后）
///
/// **修正的归属 = 层级投影（root → leaf）**：三段的位置修正**全给子端 a**，
/// 父端 b 不被子端的约束挪动（b 只由它自己的父约束、接触与积分驱动）。
/// 不用「动体数均分」（[`solve_distance_joint`] 的规则）的理由是实测的：
/// 均分让链上约束互相扰动（解肘挪上臂、解肩又挪躯干），2 轮松弛不收敛，
/// v2 人形侧向踢倒 240 步的稳态残差达 **rest 的 35% / 限位外 22°**（肢体
/// 视觉脱位、关节看着反折）；层级投影 + **根先叶后的求解序**（= 组件存储
/// 序 = [`crate::ragdoll::RagdollDesc::links`] 定义序）下每轮末尾全部约束
/// 精确满足（同一场景实测残差 1e-7 = f32 舍入量级，单测锁定 1e-4）。
/// 代价：父端不会被肢体「拖拽」（躯干靠自身接触承托，不靠腿的约束提着）
/// ——对尸体是想要的语义（根驱动、四肢跟随）。
///
/// 1. **锚点距离投影**：与 [`solve_distance_joint`] 同构的算式
///    （`err = d − rest` 沿连线修正、`stiffness` 部分修正），修正全给 a；
/// 2. **摆动锥限位**：基于①修正后的锚点，若 `∠(pb − pa, 锥轴) >
///    swing_limit_radians` → 把 b 锚**拉回锥面**（目标点 = a 锚 + 锥面上
///    最近方向 × 当前长度；锥面上最近方向 = 把锥轴朝当前方向转
///    `swing_limit_radians`），等效地把 a 拉到「b 锚恰在锥面上」的位置。
///    **限位内零修正**（幂等——关节不抖的关键，单测逐位锁定）；
/// 3. **肢体轴方向修正**：把 a 的本地 +Y 以**最小弧**对齐到「限位内的
///    a锚 → b锚 方向」——胶囊肢体的朝向由此跟随链的姿态。没有这一段，
///    倒地的肢体只会保持出生朝向（接触不产生角冲量，见模块文档诚实
///    清单），四肢会直挺挺地插在空中。**只作用于 a**：父端朝向由它自己
///    的父关节决定，链根（躯干）由角速度演化。
///
/// # 诚实边界
///
/// - **速度响应只有轴向阻尼**（同 [`DistanceJoint::damping`]）：无接触式
///   冲量、无能量守恒。**必须有阻尼**——纯位置约束下重力会对被钉住的
///   肢体持续加速（位置每轮被拉回、速度却只增不减：实测 240 步冲破
///   20 m/s，表现为高频抖动 = 「爆炸」），`damping` 每轮吃掉轴向相对
///   速度即断开这个能量注入通路（只在求解首轮施加，与接触速度响应同轮
///   同理由）；
/// - **锥形限位不区分弯曲平面**：肘 / 膝的「只能单向弯」需要 hinge +
///   twist 限位（两自由度约束），v1 用锥近似——超限方向一律拉回锥面，
///   不阻止「侧向弯」；
/// - **挂载体 / 目标端限制**同 [`DistanceJoint`]（挂载体须为碰撞体动体；
///   `target` 无效 / despawn → 本帧跳过不 panic）；
/// - **退化跳过**：两锚点重合（距离与锥面方向都不定）或 `rest_offset`
///   零长（无锥轴）→ 对应段跳过，不凭空选轴（确定性）。
#[derive(Clone, Copy, Debug, PartialEq, Component)]
pub struct SwingJoint {
    /// 父端 / 躯干侧实体；无效实体 = 约束跳过（同 [`DistanceJoint`]）。
    pub target: Entity,
    /// a（挂载体，肢体侧）的**本地**锚点（随 a 的旋转带到世界）。
    pub anchor_a: Vec3,
    /// b（`target`，父端 / 躯干侧）的**本地**锚点（关节窝）。
    pub anchor_b: Vec3,
    /// 静止时「b 锚 − a 锚」的偏移，**表达在 b 的本地系**：长度 = 锚点
    /// 距离约束的 rest，方向 = 摆动锥的锥轴（见类型文档）。
    pub rest_offset: Vec3,
    /// 最大摆角（弧度，锥半角）：`∠(a锚 → b锚, 锥轴)` 超过即拉回锥面。
    /// `≥ π` = 实际无限位（永不超限）。
    pub swing_limit_radians: f32,
    /// 松弛强度 `(0, 1]`：1 = 每轮全额投影（硬约束），`(0, 1)` = 部分
    /// 修正（迭代收敛，同 [`DistanceJoint::stiffness`]）。越界钳制到 [0, 1]。
    pub stiffness: f32,
    /// 轴向**相对**速度每帧衰减 `[0, 1)`（同 [`DistanceJoint::damping`]，
    /// 只在求解首轮施加）：抑制悬挂肢体在重力下的持续加速与链的震荡
    /// ——**必要性见类型文档的诚实边界**。
    pub damping: f32,
}

/// 一次接触（纯数据）：法线 + 穿透深度。由 [`detect_pair`] 产出。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Contact {
    /// 接触法线（单位向量）：**从 a 指向 b**——分离时 b 沿 +法线移动、
    /// a 沿 -法线移动。几何退化（球心重合等）时取确定性的任意方向。
    pub normal: Vec3,
    /// 穿透深度（正数）：两形状沿法线的重叠量。
    pub penetration: f32,
}

// ---------------------------------------------------------------------------
// 接触事件与触发器（sensor，见模块文档「接触事件」章节）
// ---------------------------------------------------------------------------

/// 首次接触事件（begin）：上一帧不接触、本帧接触的一对碰撞体。由
/// [`step_physics`] 帧末差分 [`ContactTracker`] 产出。
///
/// **实体序归一化**：`a.index() ≤ b.index()` 恒成立——配对以
/// `Entity::to_bits` 按**索引序**存入追踪集合（检测顺序取决于组件存储
/// 序，与索引序无关，删除可打乱两者相对次序）；若检测原始方向与索引序
/// 相反，[`CollisionStarted::normal`] 随归一化**翻转**（保持「从 a 指向
/// b」的 [`Contact`] 方向约定）。消费方不能假设 a 是「先 spawn 的一方」，
/// 只能依赖索引序与法线方向的自洽。
#[derive(Clone, Copy, Debug, PartialEq, Event)]
pub struct CollisionStarted {
    /// 归一化后的第一方（索引较小者）。
    pub a: Entity,
    /// 归一化后的第二方（索引较大者）。
    pub b: Entity,
    /// 接触法线（单位向量）：**从 a 指向 b**（归一化后方向，见类型文档）。
    /// 采样自**首求解轮**（位置分离前）的 [`Contact::normal`]。
    pub normal: Vec3,
    /// 穿透深度（首求解轮采样，位置分离前的原始重叠量）。
    pub penetration: f32,
}

/// 接触结束事件（end）：上一帧接触、本帧不再接触的一对碰撞体。由
/// [`step_physics`] 帧末差分 [`ContactTracker`] 产出。
///
/// 与 [`CollisionStarted`] 严格成对：**销毁也成对**——接触中的实体被
/// despawn（或组件移除）后，下一帧必然收到携带已死实体 id 的
/// [`CollisionEnded`]（本帧接触集合只含存活实体的检出对，差分必然把它
/// 从上一帧集合中筛出），消费方持有的计数 / 状态因此不会泄漏。实体序
/// 归一化规则同 [`CollisionStarted`]。
#[derive(Clone, Copy, Debug, PartialEq, Event)]
pub struct CollisionEnded {
    /// 归一化后的第一方（索引较小者；可能已销毁，见类型文档）。
    pub a: Entity,
    /// 归一化后的第二方（索引较大者；可能已销毁，见类型文档）。
    pub b: Entity,
}

/// 接触对追踪资源：维护上一帧的接触对集合，供 [`step_physics`] 帧末
/// 差分产出 [`CollisionStarted`] / [`CollisionEnded`]（begin/end 语义的
/// 状态基线）。由 `PhysicsPlugin` 自动注册；自建 App（不用
/// DefaultPlugins）时手动 `insert_resource(ContactTracker::default())`
/// 即可开启事件产出，缺席则 [`step_physics`] 静默跳过（照 `Time` 缺席
/// 语义，不强制注册）。
///
/// 集合选型 [`std::collections::BTreeSet`]（与 broadphase 的 BTreeMap
/// 同款理由）：迭代序 = 字典序 = 跨平台确定——事件产出顺序由对序唯一
/// 决定，同输入两次模拟的事件序列逐项一致（确定性测试守护）。
#[derive(Default, Resource)]
pub struct ContactTracker {
    /// 上一帧的全部接触对（`to_bits` 按 `a.index() ≤ b.index()` 归一化，
    /// 与 sensor 与否无关——sensor 对照常入集合报事件）。
    prev: std::collections::BTreeSet<(u64, u64)>,
}

/// 参与接触求解的单方参数（从组件解包的纯数值，不依赖 ECS，便于单测）。
#[derive(Clone, Copy, Debug)]
pub struct BodyParams {
    /// 线速度；静态体传 `None`（不参与速度求解，位置与速度修正恒为零）。
    pub velocity: Option<Vec3>,
    pub is_static: bool,
    pub restitution: f32,
    pub friction: f32,
}

impl BodyParams {
    /// 静态体参数（速度缺席、弹性摩擦为零）。
    pub const STATIC: Self = Self {
        velocity: None,
        is_static: true,
        restitution: 0.0,
        friction: 0.0,
    };

    /// 动态体参数。
    pub fn dynamic(velocity: Vec3, restitution: f32, friction: f32) -> Self {
        Self {
            velocity: Some(velocity),
            is_static: false,
            restitution,
            friction,
        }
    }
}

/// 单个接触 / 关节的求解输出：双方的位置修正与速度修正（叠加到各自
/// 当前值上；静态方恒为零）。接触求解（[`solve_contact`]）与关节求解
/// （[`solve_distance_joint`] / [`solve_target_joint`]）共用此四通道
/// 形状——关节语境下 a = 挂载体、b = 目标端（[`TargetJoint`] 的 b 通道
/// 恒为零；目标端为静态体 / 锚点时 b 通道亦为零）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContactResponse {
    pub delta_pos_a: Vec3,
    pub delta_pos_b: Vec3,
    pub delta_vel_a: Vec3,
    pub delta_vel_b: Vec3,
}

/// [`SwingJoint`] 的求解输出（R50）：与 [`ContactResponse`] 同构的**四通道**
/// （双方位置修正 + 双方速度修正）+ a 端（肢体侧）的**目标旋转**。
///
/// 速度通道只有「轴向相对速度阻尼」一项（见 [`SwingJoint`] 的诚实边界）；
/// 旋转通道是接触求解没有的——肢体的朝向由关节语义决定，这是 v2 ragdoll
/// 「四肢方向跟随姿态」的来源。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SwingResponse {
    /// a（挂载体，肢体侧）的位置修正（叠加到当前位置）。
    pub delta_pos_a: Vec3,
    /// b（`target`，父端 / 躯干侧）的位置修正：**层级投影下恒零**（父端
    /// 不被子端的约束挪动，见 [`SwingJoint`] 的三段求解）——保留通道是为
    /// 与 [`ContactResponse`] 四通道同构、管线写回代码可对称处理。
    pub delta_pos_b: Vec3,
    /// a 的速度修正（两条轴的相对速度阻尼，仅 `apply_velocity` 的首轮非零）。
    pub delta_vel_a: Vec3,
    /// b 的速度修正：**恒零**（同 [`SwingResponse::delta_pos_b`]）。
    pub delta_vel_b: Vec3,
    /// a 的**目标旋转**（绝对值，非增量）：`None` = 本轮不修正（a 为静态、
    /// 方向退化、或已对齐——最小弧为恒等旋转）。写回时直接覆盖
    /// [`Transform::rotation`](crate::transform::Transform)。
    pub rotation_a: Option<Quat>,
}

/// 形状对碰撞检测（纯函数，**胶囊恒竖直**特化）：穿透时返回接触，否则
/// `None`。等价于 [`detect_pair_rotated`] 传两侧 `Quat::IDENTITY`——
/// Sphere / Box 本就不看旋转，Capsule 在恒等旋转下轴段逐位等于 R31 的
/// 竖直轴（见 [`capsule_axis`]），故既有调用方与既有测试**逐位不变**。
///
/// 需要「胶囊随实体倒下」时用 [`detect_pair_rotated`]（旋转入参版）。
pub fn detect_pair(
    a_pos: Vec3,
    a_shape: &ColliderShape,
    b_pos: Vec3,
    b_shape: &ColliderShape,
) -> Option<Contact> {
    detect_pair_rotated(
        a_pos,
        Quat::IDENTITY,
        a_shape,
        b_pos,
        Quat::IDENTITY,
        b_shape,
    )
}

/// 形状对碰撞检测（纯函数，**旋转感知**，R50）：穿透时返回接触，否则
/// `None`。只依赖几何入参、不触碰世界——「穿透量 / 法线」可直接单测。
///
/// 三种形状两两配对共 9 分支（3 对称 + 6 有序）：胶囊相关的新对为
/// capsule-sphere（轴段最近点 → 球-球）、capsule-AABB（轴段-盒交替投影 →
/// 复用球-盒）、capsule-capsule（段-段最近距离）；法线方向约定与既有
/// [`Contact`] 一致（a → b），反向对翻转入参后取反法线。
///
/// # 旋转的语义边界
///
/// - **Capsule**：轴段 = 中心 ± (half_height − radius)·(`rotation` 旋转后
///   的本地 +Y)，见 [`capsule_axis`]。三个 capsule 分支的几何算法是
///   「**线段最近点**」族（解析投影 clamp / 交替投影 4 轮收敛 / 段-段钳制
///   交替），它们的入参从来只是「段的两个端点」——把竖直端点换成旋转后
///   的端点即得旋转感知，**零数学改动**（收敛性论证见各函数文档：交替
///   投影对任意方向的段同样单调收缩，轮数上界不变）；
/// - **Sphere**：各向同性，`rotation` 无语义（忽略）；
/// - **Box**：仍是**世界轴对齐** AABB，`rotation` 被忽略——OBB/SAT 升级
///   留后续轮（诚实清单见模块文档）；**旋转体的碰撞请用胶囊**。
pub fn detect_pair_rotated(
    a_pos: Vec3,
    a_rotation: Quat,
    a_shape: &ColliderShape,
    b_pos: Vec3,
    b_rotation: Quat,
    b_shape: &ColliderShape,
) -> Option<Contact> {
    match (a_shape, b_shape) {
        (ColliderShape::Sphere { radius: ra }, ColliderShape::Sphere { radius: rb }) => {
            sphere_sphere(a_pos, *ra, b_pos, *rb)
        }
        // 球在前：sphere_box 返回「从球指向盒」的法线，恰为 a → b
        (ColliderShape::Sphere { radius: r }, ColliderShape::Box { half_extents }) => {
            sphere_box(a_pos, *r, b_pos, half_extents).map(|(normal, penetration)| Contact {
                normal,
                penetration,
            })
        }
        // 盒在前：交换到「球在前」求解，再把法线翻回 a → b
        (ColliderShape::Box { half_extents }, ColliderShape::Sphere { radius: r }) => {
            sphere_box(b_pos, *r, a_pos, half_extents).map(|(normal, penetration)| Contact {
                normal: -normal,
                penetration,
            })
        }
        (ColliderShape::Box { half_extents: a_he }, ColliderShape::Box { half_extents: b_he }) => {
            box_box(a_pos, a_he, b_pos, b_he)
        }
        // 胶囊在前：capsule_sphere / capsule_box 返回「从胶囊指向对方」的法线，恰为 a → b
        (
            ColliderShape::Capsule {
                radius: r,
                half_height: hh,
            },
            ColliderShape::Sphere { radius: rb },
        ) => {
            let (a0, a1) = capsule_axis(a_pos, a_rotation, *hh, *r);
            capsule_sphere(a0, a1, *r, b_pos, *rb).map(|(normal, penetration)| Contact {
                normal,
                penetration,
            })
        }
        (
            ColliderShape::Capsule {
                radius: r,
                half_height: hh,
            },
            ColliderShape::Box { half_extents },
        ) => {
            let (a0, a1) = capsule_axis(a_pos, a_rotation, *hh, *r);
            capsule_box(a0, a1, *r, b_pos, half_extents).map(|(normal, penetration)| Contact {
                normal,
                penetration,
            })
        }
        (
            ColliderShape::Capsule {
                radius: ra,
                half_height: ahh,
            },
            ColliderShape::Capsule {
                radius: rb,
                half_height: bhh,
            },
        ) => {
            let (a0, a1) = capsule_axis(a_pos, a_rotation, *ahh, *ra);
            let (b0, b1) = capsule_axis(b_pos, b_rotation, *bhh, *rb);
            capsule_capsule(a0, a1, *ra, b0, b1, *rb)
        }
        // 胶囊在后：交换到「胶囊在前」求解，再把法线翻回 a → b
        (
            ColliderShape::Sphere { radius: r },
            ColliderShape::Capsule {
                radius: rc,
                half_height: hh,
            },
        ) => {
            let (b0, b1) = capsule_axis(b_pos, b_rotation, *hh, *rc);
            capsule_sphere(b0, b1, *rc, a_pos, *r).map(|(normal, penetration)| Contact {
                normal: -normal,
                penetration,
            })
        }
        (
            ColliderShape::Box { half_extents },
            ColliderShape::Capsule {
                radius: rc,
                half_height: hh,
            },
        ) => {
            let (b0, b1) = capsule_axis(b_pos, b_rotation, *hh, *rc);
            capsule_box(b0, b1, *rc, a_pos, half_extents).map(|(normal, penetration)| Contact {
                normal: -normal,
                penetration,
            })
        }
    }
}

/// 接触求解（纯函数）：位置分离 + 法向反弹 + 切向摩擦。
///
/// - **分离**：静态体不动，动体按动体数均分穿透深度（1 动体全吃、
///   2 动体各半——等质量近似）；
/// - **反弹**：相对速度（a 相对 b）的法向分量在「朝向穿透」（`v_n > 0`）
///   时反转为 `v_n' = -e × v_n`，e 取双方 restitution 最大；分离中
///   （`v_n ≤ 0`）不额外注入速度（这是静止接触不抖的关键）；
/// - **摩擦**：切向相对速度乘 `max(0, 1 - friction)`，friction 取
///   **参与响应的动体**的均值（静体速度不可写，其摩擦不参与）。
///
/// 「反弹量 / 摩擦衰减」据此可直接单测。
pub fn solve_contact(contact: Contact, a: BodyParams, b: BodyParams) -> ContactResponse {
    let zero = ContactResponse {
        delta_pos_a: Vec3::ZERO,
        delta_pos_b: Vec3::ZERO,
        delta_vel_a: Vec3::ZERO,
        delta_vel_b: Vec3::ZERO,
    };
    let inv_a = if a.is_static { 0.0 } else { 1.0 };
    let inv_b = if b.is_static { 0.0 } else { 1.0 };
    let inv_sum = inv_a + inv_b;
    if contact.penetration <= 0.0 || inv_sum == 0.0 {
        // 无穿透 / 静-静对（收集阶段已跳过，防御兜底）：无响应
        return zero;
    }
    let n = contact.normal;
    let share_a = inv_a / inv_sum;
    let share_b = inv_b / inv_sum;

    // 位置分离：动体按动体数均分穿透深度（法线从 a 指向 b：a 沿 -n、b 沿 +n）
    let delta_pos_a = -n * (contact.penetration * share_a);
    let delta_pos_b = n * (contact.penetration * share_b);

    // 速度响应：相对速度 = a 相对 b
    let rel = a.velocity.unwrap_or(Vec3::ZERO) - b.velocity.unwrap_or(Vec3::ZERO);
    let v_n = rel.dot(n);
    let tangent = rel - n * v_n;
    let e = a.restitution.max(b.restitution);
    let new_n = if v_n > 0.0 { -e * v_n } else { v_n };
    let friction = match (a.is_static, b.is_static) {
        (false, false) => (a.friction + b.friction) * 0.5,
        (false, true) => a.friction,
        (true, false) => b.friction,
        (true, true) => 0.0, // 不可达（静-静对已跳过），确定性兜底
    };
    let tangent_scale = (1.0 - friction).max(0.0);
    let new_rel = n * new_n + tangent * tangent_scale;
    let delta_rel = new_rel - rel;
    let delta_vel_a = delta_rel * share_a;
    let delta_vel_b = -delta_rel * share_b;

    ContactResponse {
        delta_pos_a,
        delta_pos_b,
        delta_vel_a,
        delta_vel_b,
    }
}

/// 关节求解的目标端（`step_physics` 收集阶段的内部纯数据，脱离 ECS）：
/// 碰撞体快照索引（静态或动态，位置随求解演进、速度参与阻尼）或无碰撞
/// 体锚点（本帧 [`Transform`](crate::transform::Transform) 快照视为固定
/// 点——关节不要求碰撞，吊灯锚点只有 Transform 也行；若锚点本身是仅
/// [`RigidBody`] 的飞行体，关节视角下等效固定于帧初位置，见模块文档
/// 诚实边界）。
#[derive(Clone, Copy, Debug)]
enum JointEnd {
    /// 碰撞体快照索引（位置与旋转随求解演进）。
    Body(usize),
    /// 无碰撞体锚点：`(位置, 旋转)` 的本帧 [`Transform`](crate::transform::Transform)
    /// 快照（视为固定点；旋转供 [`SwingJoint`] 的本地锚与锥轴变换）。
    Anchor(Vec3, Quat),
}

/// 距离关节求解（纯函数）：位置投影 + 轴向相对速度阻尼，可脱离世界单测。
///
/// - **位置投影**：当前距离 `d` 与 `rest_length` 偏差 `err = d − rest`，
///   沿连线（a → b 单位轴）修正 `axis·err·stiffness`，按动体数均分
///   （双动各半、静态端 / 锚点 share = 0 不动、动端全吃）；stiffness
///   钳制到 [0, 1]（弹簧 = 每轮部分修正，× 多轮 × 多帧 ≈ 收敛，见
///   [`DistanceJoint`] 的迭代性质声明）；
/// - **速度响应**（`apply_velocity` = 首轮才传 true，与接触速度响应
///   同轮）：轴向**相对**速度（a 相对 b 沿轴分量）×`(1 − damping)`
///   衰减到零（切向分量保留），修正量按动体数均分写回——阻尼器语义，
///   双向耗散（靠近与远离的相对速度都衰减）；
/// - **退化**：两端重合（约束轴不定）→ 全零响应（确定性跳过，无凭空
///   选轴）；挂载体速度缺席（不应出现，收集阶段已过滤）按零处理。
pub fn solve_distance_joint(
    pos_a: Vec3,
    vel_a: Option<Vec3>,
    pos_b: Vec3,
    vel_b: Option<Vec3>,
    joint: DistanceJoint,
    apply_velocity: bool,
) -> ContactResponse {
    let zero = ContactResponse {
        delta_pos_a: Vec3::ZERO,
        delta_pos_b: Vec3::ZERO,
        delta_vel_a: Vec3::ZERO,
        delta_vel_b: Vec3::ZERO,
    };
    let delta = pos_b - pos_a;
    let dist_sq = delta.length_squared();
    if dist_sq <= SEGMENT_EPSILON_SQ {
        // 两端重合：约束轴退化，任意方向都需凭空选轴——跳过本轮（确定性）
        return zero;
    }
    let dist = dist_sq.sqrt();
    let axis = delta / dist;
    let stiffness = joint.stiffness.clamp(0.0, 1.0);
    let correction = axis * ((dist - joint.rest_length) * stiffness);
    // 动体数均分（等质量近似，与 solve_contact 的分离同款规则）
    let (share_a, share_b) = dynamic_shares(vel_a.is_some(), vel_b.is_some());
    let delta_pos_a = correction * share_a;
    let delta_pos_b = -correction * share_b;

    let (delta_vel_a, delta_vel_b) = if apply_velocity {
        let rel = vel_a.unwrap_or(Vec3::ZERO) - vel_b.unwrap_or(Vec3::ZERO);
        let damp = (1.0 - joint.damping).clamp(0.0, 1.0);
        let delta_rel = damp_along(rel, axis, damp) - rel;
        (delta_rel * share_a, -delta_rel * share_b)
    } else {
        (Vec3::ZERO, Vec3::ZERO)
    };

    ContactResponse {
        delta_pos_a,
        delta_pos_b,
        delta_vel_a,
        delta_vel_b,
    }
}

/// 目标点关节求解（纯函数）：位置投影 + 朝目标的速度阻尼，可脱离世界单测。
///
/// - **位置投影**：进入 `rest_distance` 半径即不修（**到达语义**——停在
///   球壳上），否则沿「实体 → 目标」轴修正 `err·stiffness`（单端修正，
///   目标是世界固定点）；stiffness 钳制到 [0, 1]；
/// - **速度响应**（`apply_velocity` = 首轮才传 true）：**近似**临界阻尼
///   ——把实体速度分解为「沿目标轴」与「切向」两个分量：轴向分量 ×
///   `(1 − stiffness)`（stiffness = 1 即每帧清零轴向速度，到达时速度
///   归零的临界行为），切向分量 ×`(1 − damping)`（消解侧向漂移，把
///   实体阻尼到静止在球壳上）。这是速度缩放的工程近似，**非**精确
///   临界阻尼解（精确解需要力模型与质量，位置物理 v1 不具备）——诚实
///   声明见模块文档；
/// - **退化**：已在目标点上（轴不定）→ 全零响应（确定性跳过）。
pub fn solve_target_joint(
    pos: Vec3,
    vel: Option<Vec3>,
    joint: TargetJoint,
    apply_velocity: bool,
) -> ContactResponse {
    let zero = ContactResponse {
        delta_pos_a: Vec3::ZERO,
        delta_pos_b: Vec3::ZERO,
        delta_vel_a: Vec3::ZERO,
        delta_vel_b: Vec3::ZERO,
    };
    let delta = joint.target - pos;
    let dist_sq = delta.length_squared();
    if dist_sq <= SEGMENT_EPSILON_SQ {
        return zero; // 已在目标点上：轴退化，确定性跳过
    }
    let dist = dist_sq.sqrt();
    let axis = delta / dist;
    let stiffness = joint.stiffness.clamp(0.0, 1.0);
    let err = (dist - joint.rest_distance).max(0.0);
    let delta_pos_a = axis * (err * stiffness);

    let delta_vel_a = if apply_velocity {
        let v = vel.unwrap_or(Vec3::ZERO);
        let axial = v.dot(axis);
        let tangent = v - axis * axial;
        let axial_scale = (1.0 - joint.stiffness).clamp(0.0, 1.0);
        let tangent_scale = (1.0 - joint.damping).clamp(0.0, 1.0);
        axis * (axial * axial_scale) + tangent * tangent_scale - v
    } else {
        Vec3::ZERO
    };

    ContactResponse {
        delta_pos_a,
        delta_pos_b: Vec3::ZERO,
        delta_vel_a,
        delta_vel_b: Vec3::ZERO,
    }
}

/// 动体数均分（等质量近似，[`solve_contact`] 的分离同款规则）：双动各半、
/// 一端静态则动端全吃。`(false, _)`（a 静态）是「不应出现」的组合——关节
/// 收集阶段已过滤静态挂载体，此处确定性兜底把修正全给 b 端。
fn dynamic_shares(dynamic_a: bool, dynamic_b: bool) -> (f32, f32) {
    match (dynamic_a, dynamic_b) {
        (true, true) => (0.5, 0.5),
        (true, false) => (1.0, 0.0),
        (false, _) => (0.0, 1.0),
    }
}

/// 阻尼器算式（[`solve_distance_joint`] / [`solve_swing_joint`] 共用）：把
/// 相对速度 `rel` 沿单位轴 `axis` 的分量按 `damp` 缩放、**切向分量原样保留**
/// （阻尼器只吃掉约束方向上的相对运动，摆动 / 滑移本身不被阻止）。
fn damp_along(rel: Vec3, axis: Vec3, damp: f32) -> Vec3 {
    let v_axial = rel.dot(axis);
    let tangent = rel - axis * v_axial;
    axis * (v_axial * damp) + tangent
}

/// 四元数部分插值（nlerp = 线性插值后归一化）：slerp 的**无三角**替代
/// ——只用乘加与 `sqrt`，不碰 `acos` / `sin`（确定性口径同 AGENTS 的
/// 「`sqrt` 优于 `hypot`」：libm 超越函数无位级跨实现保证）。小角度下与
/// slerp 的差别不可见（关节的部分修正本就是松弛近似）。
///
/// 点积为负时先翻到同半球（否则插值走「远路」= 转超过 180°）。
fn nlerp(from: Quat, to: Quat, t: f32) -> Quat {
    let to = if from.dot(to) < 0.0 { -to } else { to };
    (from * (1.0 - t) + to * t).normalize()
}

/// 摆动关节求解（R50 纯函数）：锚点距离投影 + 摆动锥限位 + a 端肢体轴
/// 方向修正，可脱离世界单测。三段语义与退化规则的规范见 [`SwingJoint`]
/// 类型文档；`vel_a` / `vel_b` 的 `Option` 兼作**动体标记**（`None` = 静态
/// 体 / 无碰撞体锚点），父端速度只参与「相对速度」的计算——**位置与速度
/// 修正全部落在 a 上**（层级投影，`delta_*_b` 恒零，理由见类型文档）。
///
/// # 锥面拉回的数学
///
/// 设 `d̂` = 当前 a锚 → b锚 的单位向、`â` = 锥轴（`rot_b·rest_offset` 归一）、
/// `L` = `swing_limit_radians`。超限判据用余弦单调性（免 `acos`）：
/// `d̂·â < cos L` ⇒ 夹角 > L。锥面上**离 `d̂` 最近**的方向 = 把 `â` 朝 `d̂`
/// 转 `L`：取 `d̂` 垂直于 `â` 的分量 `p = d̂ − â(d̂·â)`，归一得 `p̂`，则
///
/// ```text
/// ĉ = â·cos L + p̂·sin L        （|ĉ| = 1，∠(ĉ, â) = L，且 ĉ 落在 â 与 d̂ 张成的平面内）
/// 目标点 = a锚 + ĉ·|d|         （保持当前锚点长度——只改方向不改距离，
///                                距离由第①段负责，两段正交不打架）
/// ```
///
/// 修正量 `(目标点 − b锚)·stiffness` 全部记在 a 上（a 沿 −、b 恒零）。
/// `p ≈ 0`（`d̂` 与 `â` 反平行，即肢体正好折到锥轴背面）时锥面方向不定：
/// 取 `â.any_orthonormal_vector()` 作确定性任意垂直向（与 [`sphere_sphere`]
/// 重合退化取 +Y 同款「确定性任意方向」口径）。
///
/// # 方向修正（第③段）
///
/// `Quat::from_rotation_arc(rot_a·Y, d̂_限位内)` 的最小弧左乘 `rot_a`——
/// 无三角超越（内部只有叉积 / 点积 / `sqrt`），确定性口径同 foot_ik 的
/// 两骨 IK；`stiffness < 1` 时用 [`nlerp`] 部分对齐（松弛语义）。最小弧
/// 只约束肢体轴方向，**绕自身轴的 roll 由写入历史携带**（与 IK 的同款
/// 自由度声明）——正是这一点让「死亡翻滚的 ω 初值」在肢体上表现为绕肢
/// 轴的自然扭转，而不是被约束抹平。
///
/// # 速度响应
///
/// 阻尼**两条轴**上的相对速度（各 ×`(1 − damping)`、切向保留，算式见
/// [`damp_along`]）：① 锚点连线（与 [`solve_distance_joint`] 同式）；
/// ② 锥面修正方向（锥面激活时）——锥面拉回是切向位置修正，速度不跟着
/// 衰减就成了能量泵（实测被限位夹住的肢体 4 s 后仍以 5 m/s 沿锥面滑行）。
/// 只在 `apply_velocity`（首轮）施加，修正全记在 a 上。
#[allow(clippy::too_many_arguments)]
pub fn solve_swing_joint(
    pos_a: Vec3,
    rot_a: Quat,
    vel_a: Option<Vec3>,
    pos_b: Vec3,
    rot_b: Quat,
    vel_b: Option<Vec3>,
    joint: SwingJoint,
    apply_velocity: bool,
) -> SwingResponse {
    let anchor_a = pos_a + rot_a * joint.anchor_a;
    let anchor_b = pos_b + rot_b * joint.anchor_b;
    let delta = anchor_b - anchor_a;
    let dist_sq = delta.length_squared();
    if dist_sq <= SEGMENT_EPSILON_SQ {
        // 两锚点重合：距离与锥面方向都不定——整段跳过（确定性，不凭空选轴）
        return SwingResponse {
            delta_pos_a: Vec3::ZERO,
            delta_pos_b: Vec3::ZERO,
            delta_vel_a: Vec3::ZERO,
            delta_vel_b: Vec3::ZERO,
            rotation_a: None,
        };
    }
    let dist = dist_sq.sqrt();
    let dir = delta / dist;
    let rest_length = joint.rest_offset.length();
    let stiffness = joint.stiffness.clamp(0.0, 1.0);
    // **层级投影（root → leaf）**：位置修正**全给子端 a**、父端 share 恒 0
    // ——不用 `dynamic_shares` 的动体数均分。理由（实测对照，v2 人形 240 步
    // 侧向踢倒）：均分让链上约束互相扰动（解肘会挪上臂、解肩又挪躯干），
    // 2 轮松弛不收敛，稳态残差达 **rest 的 35% / 限位外 22°**（肢体视觉
    // 脱位、关节反折）；层级投影下子端被投影到父端的关节窝与锥面上、父端
    // 只由它自己的父约束与接触驱动，**根先叶后的求解序**（= 组件存储序 =
    // `RagdollDesc::links` 定义序）保证每轮末尾全部约束精确满足（实测残差
    // 1e-7 = f32 舍入量级）。前提写在 [`crate::ragdoll::RagdollLink`]：
    // links 须父先子后（乱序会退回 Gauss-Seidel 残差，不 panic）。
    let share_a = if vel_a.is_some() { 1.0 } else { 0.0 };
    let share_b = 0.0;

    // ---- ① 锚点距离投影（**双向刚杆**，与 solve_distance_joint 同构：
    // err = d − rest，靠近与远离都修正）----
    // 试过「单向绳」语义（只在 d > rest 时拉近，让地面接触赢）：埋臂确实
    // 缓解（0.37 → 0.15 m），但代价是肢体被接触压到关节窝上、锚距偏差达
    // rest 的 100%（腿塌进髋部，链的拓扑名义上还在、几何上已散）。埋臂的
    // 正解不在约束侧而在**翻倒转轴**：绕链根肩线前扑 / 后仰（双肩等高）
    // 而非侧倒，见 `crate::ragdoll::topple_axis` 的实测论证——同一场景埋深
    // 从 0.37 m 降到 0.028 m，且锚距 / 摆角残差回到 f32 舍入量级。
    let err = dist - rest_length;
    let correction = dir * (err * stiffness);
    let mut delta_pos_a = correction * share_a;
    let mut delta_pos_b = -correction * share_b;

    // ---- ② 摆动锥限位（基于①修正后的锚点）----
    let mut desired_dir = dir; // ③ 的对齐目标：限位内的 a锚 → b锚 方向
    let mut cone_axis: Option<Vec3> = None; // 速度响应要阻尼的第二条轴（见下）
    let rest_dir = joint.rest_offset.normalize_or_zero();
    if rest_dir != Vec3::ZERO {
        let moved_a = anchor_a + delta_pos_a;
        let moved_b = anchor_b + delta_pos_b;
        let swing = moved_b - moved_a;
        let swing_sq = swing.length_squared();
        if swing_sq > SEGMENT_EPSILON_SQ {
            let swing_length = swing_sq.sqrt();
            let swing_dir = swing / swing_length;
            // 锥轴随父端旋转（解剖学口径，见 SwingJoint 文档）
            let axis = rot_b * rest_dir;
            let cos_angle = swing_dir.dot(axis);
            let cos_limit = joint.swing_limit_radians.cos();
            if cos_angle < cos_limit {
                let sin_limit = joint.swing_limit_radians.sin();
                let perp = swing_dir - axis * cos_angle;
                let perp_sq = perp.length_squared();
                let cone_dir = if perp_sq <= SEGMENT_EPSILON_SQ {
                    // 反平行退化：锥面方向不定，取确定性任意垂直向
                    axis * cos_limit + axis.any_orthonormal_vector() * sin_limit
                } else {
                    axis * cos_limit + (perp / perp_sq.sqrt()) * sin_limit
                };
                let target_point = moved_a + cone_dir * swing_length;
                let cone_correction = (target_point - moved_b) * stiffness;
                delta_pos_a -= cone_correction * share_a;
                delta_pos_b += cone_correction * share_b;
                desired_dir = cone_dir;
                cone_axis = Some(cone_correction.normalize_or_zero());
            }
        }
    }

    // ---- 速度响应（仅首轮）：阻尼**两条轴**上的相对速度 ----
    // ① 锚点连线（轴向）：与 solve_distance_joint 同式；
    // ② 锥面修正方向（锥面激活时）：锥面拉回是**切向**位置修正，若速度不
    //    跟着衰减，被限位夹住的肢体就变成能量泵——每帧重力把它推出锥面、
    //    约束把它推回，位置稳定而切向速度持续累积（实测 4 s 后肢体仍以
    //    5 m/s 沿锥面「滑行」，尸体散着滑走）。两条轴各按 (1 − damping)
    //    衰减后，平衡速度 ≈ 单帧重力增量 / damping（0.3 m/s 量级）。
    let (delta_vel_a, delta_vel_b) = if apply_velocity {
        let damp = (1.0 - joint.damping).clamp(0.0, 1.0);
        let rel = vel_a.unwrap_or(Vec3::ZERO) - vel_b.unwrap_or(Vec3::ZERO);
        let mut damped = damp_along(rel, dir, damp);
        if let Some(axis) = cone_axis {
            if axis != Vec3::ZERO {
                damped = damp_along(damped, axis, damp);
            }
        }
        let delta_rel = damped - rel;
        (delta_rel * share_a, -delta_rel * share_b)
    } else {
        (Vec3::ZERO, Vec3::ZERO)
    };

    // ---- ③ 肢体轴方向修正（只作用于 a；静态端不写旋转）----
    let rotation_a = if vel_a.is_some() {
        let current = (rot_a * Vec3::Y).normalize_or_zero();
        let desired = desired_dir.normalize_or_zero();
        if current == Vec3::ZERO || desired == Vec3::ZERO {
            None
        } else {
            let arc = Quat::from_rotation_arc(current, desired);
            if arc == Quat::IDENTITY {
                None // 已对齐：不写回（避免 normalize 的末位改写）
            } else {
                let aligned = arc * rot_a;
                Some(if stiffness >= 1.0 {
                    aligned
                } else {
                    nlerp(rot_a, aligned, stiffness)
                })
            }
        }
    } else {
        None
    };

    SwingResponse {
        delta_pos_a,
        delta_pos_b,
        delta_vel_a,
        delta_vel_b,
        rotation_a,
    }
}

/// **静态障碍快照**（R50）：位置 + 旋转 + 形状。旋转感知查询
/// （[`resolve_static_push_rotated`] / [`shape_sweep_rotated`] /
/// [`crate::character::move_and_slide_statics_rotated`]）的入参元素——
/// 旋转只对 [`ColliderShape::Capsule`] 有语义（Sphere / Box 忽略，见
/// [`detect_pair_rotated`]）。世界封装（[`resolve_against_statics`] /
/// [`sphere_sweep_statics`] / [`crate::character::move_and_slide`]）直接从
/// `Transform` 收集，倒地尸体的斜置胶囊因此能被正确当作障碍。
pub type StaticObstacle = (Vec3, Quat, ColliderShape);

/// 静态推离查询（纯函数，**胶囊恒竖直**特化）：把 `pos` 处的 `shape` 从
/// 全部静态体的重叠中推出，返回累计修正向量。等价于
/// [`resolve_static_push_rotated`] 的旋转全取 `Quat::IDENTITY`。
///
/// 对每个静态体调 [`detect_pair`] 求（法线, 穿透），**顺序累加**推离
/// ——逐个用推离后的位置重测（Gauss-Seidel 风格）；角点处同一静态体
/// 的重复推离可接受（v1 简化：顺序单遍而非联合求解，分离性优先于精确性）。
/// 无穿透（含 `statics` 为空）返回零向量。
///
/// 推离只沿接触法线：调用方保留自己的切向速度即可实现「贴墙滑行」。
/// 世界封装见 [`resolve_against_statics`]。
pub fn resolve_static_push(
    pos: Vec3,
    shape: &ColliderShape,
    statics: &[(Vec3, ColliderShape)],
) -> Vec3 {
    push_out_of_statics(
        pos,
        Quat::IDENTITY,
        shape,
        statics.iter().map(|&(p, ref s)| (p, Quat::IDENTITY, *s)),
    )
}

/// 静态推离查询（纯函数，**旋转感知**，R50）：查询形状与全部静态障碍都带
/// 旋转（[`StaticObstacle`]）——斜置胶囊（倒地肢体 / 横倒圆柱）照实推离。
/// 其余语义（顺序累加、Gauss-Seidel 重测、只沿法线）同
/// [`resolve_static_push`]。
pub fn resolve_static_push_rotated(
    pos: Vec3,
    rotation: Quat,
    shape: &ColliderShape,
    statics: &[StaticObstacle],
) -> Vec3 {
    push_out_of_statics(pos, rotation, shape, statics.iter().copied())
}

/// 推离核心（[`resolve_static_push`] / [`resolve_static_push_rotated`] 共用；
/// 泛型迭代器入参 ⇒ 两种静态体列表形态都不额外分配）。
fn push_out_of_statics<I>(mut pos: Vec3, rotation: Quat, shape: &ColliderShape, statics: I) -> Vec3
where
    I: IntoIterator<Item = StaticObstacle>,
{
    let mut push = Vec3::ZERO;
    for (static_pos, static_rotation, static_shape) in statics {
        // 法线从查询方指向静态体：静态体不动，查询方沿 -法线退出穿透量
        let Some(contact) = detect_pair_rotated(
            pos,
            rotation,
            shape,
            static_pos,
            static_rotation,
            &static_shape,
        ) else {
            continue;
        };
        let correction = -contact.normal * contact.penetration;
        pos += correction; // 后续静态体用修正后的位置重测
        push += correction;
    }
    push
}

/// 球-球检测：法线从 a 指向 b。
fn sphere_sphere(a_pos: Vec3, a_radius: f32, b_pos: Vec3, b_radius: f32) -> Option<Contact> {
    let d = b_pos - a_pos;
    let r_sum = a_radius + b_radius;
    let dist_sq = d.length_squared();
    if dist_sq >= r_sum * r_sum {
        return None;
    }
    if dist_sq <= 1e-12 {
        // 球心重合：方向退化，取 +Y（确定性任意方向）
        return Some(Contact {
            normal: Vec3::Y,
            penetration: r_sum,
        });
    }
    let dist = dist_sq.sqrt();
    Some(Contact {
        normal: d / dist,
        penetration: r_sum - dist,
    })
}

/// 球-AABB 检测：返回（从球心指向盒的法线，穿透深度）。
fn sphere_box(
    sphere_pos: Vec3,
    radius: f32,
    box_pos: Vec3,
    half_extents: &Vec3,
) -> Option<(Vec3, f32)> {
    let local = sphere_pos - box_pos; // 球心在盒局部坐标（盒轴对齐，无需旋转变换）
    let closest = local.clamp(-*half_extents, *half_extents); // 盒面（或盒内）最近点
    let delta = closest - local; // 球心 → 最近点
    let dist_sq = delta.length_squared();
    if dist_sq > radius * radius {
        return None;
    }
    if dist_sq > 1e-12 {
        // 球心在盒外：法线 = 球心 → 最近点（即从球指向盒）
        let dist = dist_sq.sqrt();
        Some((delta / dist, radius - dist))
    } else {
        // 球心在盒内：沿「最浅穿透轴」从最近的面推出。法线仍从球指向盒
        // （分离时球沿 -法线移动 = 朝最近的面退出）。并列时按 x < y < z
        // 优先，保证确定性。
        let dx = half_extents.x - local.x.abs();
        let dy = half_extents.y - local.y.abs();
        let dz = half_extents.z - local.z.abs();
        let (depth, axis, positive) = if dx <= dy && dx <= dz {
            (dx, 0, local.x >= 0.0)
        } else if dy <= dz {
            (dy, 1, local.y >= 0.0)
        } else {
            (dz, 2, local.z >= 0.0)
        };
        let away = if positive { -1.0 } else { 1.0 };
        let normal = match axis {
            0 => Vec3::new(away, 0.0, 0.0),
            1 => Vec3::new(0.0, away, 0.0),
            _ => Vec3::new(0.0, 0.0, away),
        };
        Some((normal, depth + radius))
    }
}

/// AABB-AABB 检测：法线取最小穿透轴、从 a 指向 b。
fn box_box(a_pos: Vec3, a_half: &Vec3, b_pos: Vec3, b_half: &Vec3) -> Option<Contact> {
    let d = b_pos - a_pos;
    let overlap = Vec3::new(
        a_half.x + b_half.x - d.x.abs(),
        a_half.y + b_half.y - d.y.abs(),
        a_half.z + b_half.z - d.z.abs(),
    );
    if overlap.x <= 0.0 || overlap.y <= 0.0 || overlap.z <= 0.0 {
        return None;
    }
    // 最小穿透轴（并列时 x < y < z 优先，保证确定性）
    let (normal, penetration) = if overlap.x <= overlap.y && overlap.x <= overlap.z {
        (Vec3::new(signum(d.x), 0.0, 0.0), overlap.x)
    } else if overlap.y <= overlap.z {
        (Vec3::new(0.0, signum(d.y), 0.0), overlap.y)
    } else {
        (Vec3::new(0.0, 0.0, signum(d.z)), overlap.z)
    };
    Some(Contact {
        normal,
        penetration,
    })
}

/// 零取 +1 的符号函数（中心重合时方向退化，确定性取正）。
fn signum(x: f32) -> f32 {
    if x >= 0.0 { 1.0 } else { -1.0 }
}

// ---------------------------------------------------------------------------
// 胶囊几何（detect_pair 的 capsule 系列辅助，全部纯函数、确定性）
// R50：轴段朝向由实体旋转给出（capsule_axis），几何算法只吃「段的两个
// 端点」——旋转感知是**入参替换**而非数学改动。
// ---------------------------------------------------------------------------

/// 交替投影收敛轮数：胶囊轴段与盒 / 段-段最近点的迭代近似轮数。
///
/// **收敛性（R50 旋转感知后的论证）**：交替投影 = 在两个凸集之间往返取
/// 最近点，每轮的距离序列单调不增且有下界（两集合间的最小距离）⇒ 必收敛。
/// 竖直段 × 轴对齐盒、平行竖直轴对这类**可分解**问题（XZ 与 Y 独立）2 轮
/// 内即达不动点；斜段（旋转胶囊）× 盒 / 斜段 × 斜段不可分解，收敛是几何
/// 速率（每轮消去一阶误差）而非 2 轮精确——4 轮是「确定性优先于解析精确」
/// 的既有取舍（同输入恒同轮数同结果），斜置配对的残差量级由单测锁定
/// （`rotated_capsule_box_matches_analytic_segment_distance`，< 1e-3）。
const CAPSULE_PROJECTION_ROUNDS: usize = 4;

/// 段-点最近点判定容差：段方向 `length²` 低于此值按「零长段」处理
/// （half_height = radius 的退化胶囊，任意参数等价）。
const SEGMENT_EPSILON_SQ: f32 = 1e-12;

/// 胶囊的轴段端点（R50 旋转感知）：中心 ± (half_height − radius)·**旋转后
/// 的本地 +Y**（见 [`ColliderShape::Capsule`]）。
///
/// **恒等旋转逐位等价**（既有场景零变化的根据）：glam 的 `q * v` 展开为
/// `v + 2·(w·(q_xyz × v) + q_xyz × (q_xyz × v))`，`q == IDENTITY` 时
/// `q_xyz = 0` ⇒ 两个叉积恒为零向量 ⇒ 结果逐分量 `v_i + 0.0`，与 `v` 逐位
/// 相同；再乘 `(half_height − radius)` 即 R31 的 `Vec3::Y * half_axis`
/// 同式同值。故 `rotation == IDENTITY` 时本函数与旧实现**逐位一致**，无需
/// 恒等快路径分支。
fn capsule_axis(center: Vec3, rotation: Quat, half_height: f32, radius: f32) -> (Vec3, Vec3) {
    let axis = (rotation * Vec3::Y) * (half_height - radius);
    (center - axis, center + axis)
}

/// 段 `a → a + ab` 上到点 `p` 最近的参数（解析投影 clamp 到 [0,1]）。
/// 零长段（ab 退化）取 0.5——端点重合时任意参数等价，取中值定死确定性。
fn closest_on_segment(a: Vec3, ab: Vec3, p: Vec3) -> f32 {
    let denom = ab.length_squared();
    if denom <= SEGMENT_EPSILON_SQ {
        return 0.5;
    }
    ((p - a).dot(ab) / denom).clamp(0.0, 1.0)
}

/// 胶囊-球检测：轴段（`axis_start → axis_end`，**任意朝向**）上到球心的
/// 最近点（解析投影 clamp）→ 球-球。最近点落在轴段内时等价于「半径
/// radius 的球在最近点处」的球-球；落在端点时即端帽半球判定。返回（从
/// 胶囊指向球的法线，穿透深度）。
///
/// 算法只吃段端点、与朝向无关 ⇒ R50 旋转感知**零数学改动**。
fn capsule_sphere(
    axis_start: Vec3,
    axis_end: Vec3,
    radius: f32,
    sphere_pos: Vec3,
    sphere_radius: f32,
) -> Option<(Vec3, f32)> {
    let ab = axis_end - axis_start;
    let t = closest_on_segment(axis_start, ab, sphere_pos);
    sphere_sphere(axis_start + ab * t, radius, sphere_pos, sphere_radius)
        .map(|contact| (contact.normal, contact.penetration))
}

/// 胶囊-AABB 检测：轴段（**任意朝向**）与盒的最近点——**交替投影**（段上
/// 点投影到盒范围（clamp），再把盒上点投影回段参数（clamp），来回
/// [`CAPSULE_PROJECTION_ROUNDS`] 轮收敛）。这是迭代近似而非解析解：竖直段
/// × 轴对齐盒按 XZ / Y 分解后 2 轮内收敛；斜段（R50 旋转胶囊）不可分解，
/// 4 轮取确定性裕度（残差量级见 [`CAPSULE_PROJECTION_ROUNDS`] 文档）。
/// 盒内情形（段上最近点进入盒内）照 [`sphere_box`] 的盒内逻辑——沿最浅
/// 穿透轴退出。返回（从胶囊指向盒的法线，穿透深度）。
fn capsule_box(
    axis_start: Vec3,
    axis_end: Vec3,
    radius: f32,
    box_pos: Vec3,
    half_extents: &Vec3,
) -> Option<(Vec3, f32)> {
    let ab = axis_end - axis_start;
    let min = box_pos - *half_extents;
    let max = box_pos + *half_extents;
    let mut t = 0.5; // 起始参数取段中点（确定性；首轮投影即修正到真实邻域）
    for _ in 0..CAPSULE_PROJECTION_ROUNDS {
        let on_segment = axis_start + ab * t;
        let on_box = on_segment.clamp(min, max);
        t = closest_on_segment(axis_start, ab, on_box);
    }
    let closest = axis_start + ab * t;
    sphere_box(closest, radius, box_pos, half_extents)
}

/// 胶囊-胶囊检测：段-段最近距离（标准钳制解法——固定对方参数解析投影
/// 己方、clamp 到 [0,1]，交替 [`CAPSULE_PROJECTION_ROUNDS`] 轮收敛），
/// 距离对比半径和。两段**任意朝向**（R50 旋转感知）：平行 / 竖直轴配对
/// 2 轮内收敛，斜交轴 4 轮取确定性裕度（见
/// [`CAPSULE_PROJECTION_ROUNDS`]）。法线从 a 指向 b；两轴段最近点重合
/// （穿透中心重合等退化）时取 +Y（与 [`sphere_sphere`] 同约定）。
fn capsule_capsule(
    a_start: Vec3,
    a_end: Vec3,
    a_radius: f32,
    b_start: Vec3,
    b_end: Vec3,
    b_radius: f32,
) -> Option<Contact> {
    let d1 = a_end - a_start;
    let d2 = b_end - b_start;
    let mut s = 0.5;
    let mut t = 0.5;
    for _ in 0..CAPSULE_PROJECTION_ROUNDS {
        let on_b = b_start + d2 * t;
        s = closest_on_segment(a_start, d1, on_b);
        let on_a = a_start + d1 * s;
        t = closest_on_segment(b_start, d2, on_a);
    }
    let closest_a = a_start + d1 * s;
    let closest_b = b_start + d2 * t;
    let delta = closest_b - closest_a;
    let r_sum = a_radius + b_radius;
    let dist_sq = delta.length_squared();
    if dist_sq >= r_sum * r_sum {
        return None;
    }
    if dist_sq <= SEGMENT_EPSILON_SQ {
        // 最近点重合：方向退化，取 +Y（确定性任意方向）
        return Some(Contact {
            normal: Vec3::Y,
            penetration: r_sum,
        });
    }
    let dist = dist_sq.sqrt();
    Some(Contact {
        normal: delta / dist,
        penetration: r_sum - dist,
    })
}

/// 碰撞体的世界 AABB（球 → 中心 ± 半径；盒 → 中心 ± 半边长——盒本就
/// 轴对齐，`rotation` 被忽略；胶囊 → **旋转轴的精确包络**：半 extent =
/// |旋转后轴向量| + radius 逐分量，即「斜段的 AABB ⊕ 半径球」的解析解
/// ——旋转胶囊倒下后包围盒自动变扁变宽）。供 broadphase 网格的插入与
/// 查询使用。
///
/// 恒等旋转下胶囊的 y 半 extent 是 `(half_height − radius) + radius`，与
/// R31 直接写的 `half_height` 相差至多 1 ULP（~1e-8 量级）：只影响网格
/// 粒度、不影响 [`detect_pair_rotated`] 的精检结果，「候选为保守超集」的
/// 性质不变（漏对只可能发生在穿透 ≤ 1 ULP 的配对上，而该量级远低于
/// [`SWEEP_CONTACT_EPSILON`] 的相切判定阈）。
fn collider_aabb(pos: Vec3, rotation: Quat, shape: &ColliderShape) -> (Vec3, Vec3) {
    match *shape {
        ColliderShape::Sphere { radius } => {
            let r = Vec3::splat(radius);
            (pos - r, pos + r)
        }
        ColliderShape::Box { half_extents } => (pos - half_extents, pos + half_extents),
        ColliderShape::Capsule {
            radius,
            half_height,
        } => {
            let axis = (rotation * Vec3::Y) * (half_height - radius);
            let half = axis.abs() + Vec3::splat(radius);
            (pos - half, pos + half)
        }
    }
}

/// AABB 的最大轴向跨度（供 cell 尺寸估计：贴着最大动体的包围盒粒度）。
fn aabb_span(aabb: &(Vec3, Vec3)) -> f32 {
    (aabb.1 - aabb.0).max_element()
}

/// 仅受力积分体（只有 [`RigidBody`] 无 [`Collider`]）的本帧积分结果：
/// 位置 / 速度 / 可选角积分（零角速度 = `None` ⇒ 写回阶段不碰旋转）。
struct FlyerState {
    entity: Entity,
    position: Vec3,
    velocity: Vec3,
    spin: Option<(Quat, Vec3)>,
}

/// 物理主系统：每帧推进一次（积分 → 检测 → 响应 → 迭代松弛 → 写回，
/// 见模块文档的管线六步）。
///
/// 只读世界签名（`&World`）+ 两阶段处理（先全量收集再统一写回，
/// `update_animations` 同款模式），组件写访问走世界内部的尝试型锁，
/// 因此可注册为**并行系统**（访问集：写 Transform / Collider / RigidBody，
/// 读 Time）。`Time` 资源缺席时 dt = 0：不积分，但仍会分离初始摆放重叠。
pub fn step_physics(world: &World) {
    let dt = world
        .try_resource::<Time>()
        .map(|t| t.delta_secs())
        .unwrap_or(0.0);

    // ---- 阶段一（只读收集）：全部碰撞体实体 + 可选刚体 ----
    // rotations（R50）：与 positions 平行的旋转快照——旋转感知胶囊的轴朝向、
    // 角速度积分的写入目标、摆动关节的本地锚变换都用它（求解期可变，
    // 写回阶段与 positions 同批落盘）。
    let mut collected: Vec<(Entity, Collider, Option<RigidBody>)> = Vec::new();
    let mut positions: Vec<Vec3> = Vec::new();
    let mut rotations: Vec<Quat> = Vec::new();
    world.query::<(Entity, &Transform, &Collider, Option<&RigidBody>)>(
        |(entity, transform, collider, rigid)| {
            collected.push((entity, *collider, rigid.copied()));
            positions.push(transform.position);
            rotations.push(transform.rotation);
        },
    );
    let n = collected.len();

    // 仅受力积分体：只有 RigidBody（无 Collider）的实体——不参与碰撞，
    // 只随积分与写回阶段一起推进（模块文档的「缺席语义」第二条）。
    let mut flyers: Vec<(Entity, RigidBody)> = Vec::new();
    world.query::<(Entity, &RigidBody)>(|(entity, rigid)| {
        // 递归读另一组件存储是合法的（共享读可并存，渲染收集闭包同款模式）
        if world.get::<Collider>(entity).is_none() {
            flyers.push((entity, *rigid));
        }
    });
    if n == 0 && flyers.is_empty() {
        // 全部碰撞体都没了：接触集合为空，上一帧的残留配对全部转 ended
        //（销毁成对性：接触中的实体被销毁后，ended 不能因早退而丢失）
        emit_contact_events(world, &std::collections::BTreeMap::new());
        return;
    }

    // 有效静态：Collider::is_static 或缺席 RigidBody（取或，见模块文档）；
    // 运动学标记：body_type == Kinematic（跳过积分与速度写回，但参与
    // 检测与接触事件，见 BodyType::Kinematic 文档）
    let is_static: Vec<bool> = collected
        .iter()
        .map(|(_, collider, rigid)| collider.is_static || rigid.is_none())
        .collect();
    let is_kinematic: Vec<bool> = collected
        .iter()
        .map(|(_, _, rigid)| rigid.is_some_and(|body| body.body_type == BodyType::Kinematic))
        .collect();

    // 触发区域标记（与 is_static 正交：sensor 也可以动，见 Collider 文档）
    let is_sensor: Vec<bool> = collected
        .iter()
        .map(|(_, collider, _)| collider.is_sensor)
        .collect();

    // ---- 约束收集（两阶段模式追加，见模块文档「约束与关节」章节）----
    // 挂载体实体 → 碰撞体快照索引。HashMap 只做点查、从不迭代（求解序 =
    // 下面 query 的组件存储收集序），确定性不受哈希序影响。
    let mut body_index: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for (index, (entity, _, _)) in collected.iter().enumerate() {
        body_index.insert(entity.to_bits(), index);
    }

    // 距离关节：挂载体须为碰撞体动体（无 Collider 或 is_static → 跳过，
    // 静态体本就不写回）；目标端是碰撞体（静态 / 动态皆可，位置随求解
    // 演进）或无碰撞体锚点（Transform 快照视为固定点）；目标无效
    //（despawn / 从未存在 / 无 Transform）→ 约束跳过（世代语义一致，
    // 不 panic）。收集序 = 组件存储序 = 求解序（确定性）。
    let mut distance_joints: Vec<(usize, DistanceJoint, JointEnd)> = Vec::new();
    world.query::<(Entity, &DistanceJoint)>(|(entity, joint)| {
        let Some(&owner) = body_index.get(&entity.to_bits()) else {
            return; // 挂载体不是碰撞体：v1 关节跳过（模块文档诚实边界）
        };
        if is_static[owner] {
            return;
        }
        let end = match body_index.get(&joint.target.to_bits()) {
            Some(&index) => JointEnd::Body(index),
            // 目标不是碰撞体：仍存活且有 Transform → 固定锚点快照；
            // 否则无效，整体跳过
            None => match world.get::<Transform>(joint.target) {
                Some(transform) => JointEnd::Anchor(transform.position, transform.rotation),
                None => return,
            },
        };
        distance_joints.push((owner, *joint, end));
    });

    // 摆动关节（R50）：挂载体 / 目标端 / 无效跳过的规则与距离关节逐条相同
    //（复用同一份 body_index 与 is_static 口径）；收集序 = 组件存储序 =
    // 求解序（确定性）。求解在约束轮内排在距离关节之后（见下方注释）。
    let mut swing_joints: Vec<(usize, SwingJoint, JointEnd)> = Vec::new();
    world.query::<(Entity, &SwingJoint)>(|(entity, joint)| {
        let Some(&owner) = body_index.get(&entity.to_bits()) else {
            return; // 挂载体不是碰撞体：跳过（同距离关节）
        };
        if is_static[owner] {
            return;
        }
        let end = match body_index.get(&joint.target.to_bits()) {
            Some(&index) => JointEnd::Body(index),
            None => match world.get::<Transform>(joint.target) {
                Some(transform) => JointEnd::Anchor(transform.position, transform.rotation),
                None => return,
            },
        };
        swing_joints.push((owner, *joint, end));
    });

    // 目标点关节：目标是世界坐标，无实体存在性语义；挂载体限制同上。
    let mut target_joints: Vec<(usize, TargetJoint)> = Vec::new();
    world.query::<(Entity, &TargetJoint)>(|(entity, joint)| {
        if let Some(&owner) = body_index.get(&entity.to_bits()) {
            if !is_static[owner] {
                target_joints.push((owner, *joint));
            }
        }
    });

    // 本帧接触对（首求解轮采样，差分产出事件用）：位序归一化对 →
    // (归一化方向法线, 穿透)。BTreeMap 字典序 = 事件产出顺序确定。
    let mut current_contacts: std::collections::BTreeMap<(u64, u64), (Vec3, f32)> =
        std::collections::BTreeMap::new();

    // ---- 积分（半隐式欧拉，仅动体；静态与运动学体均跳过）----
    let mut velocities: Vec<Option<Vec3>> = collected
        .iter()
        .map(|(_, _, rigid)| rigid.map(|b| b.velocity))
        .collect();
    // 角速度快照（R50）：与 velocities 平行；静态 / 运动学体不积分也不写回
    let mut angular_velocities: Vec<Vec3> = collected
        .iter()
        .map(|(_, _, rigid)| rigid.map(|b| b.angular_velocity).unwrap_or(Vec3::ZERO))
        .collect();
    for index in 0..n {
        if is_static[index] || is_kinematic[index] {
            continue;
        }
        let rigid = collected[index].2.expect("非静态实体必有 RigidBody");
        let mut velocity = rigid.velocity;
        velocity += PHYSICS_GRAVITY * rigid.gravity_scale * dt;
        velocity *= (-rigid.linear_damping * dt).exp();
        positions[index] += velocity * dt;
        velocities[index] = Some(velocity);
        // 角积分（R50）：与线性积分**不耦合**（位置积分不含 ω 项——既有
        // free_fall 等测试零改动守护）。零角速度整段跳过：既有场景的
        // Transform.rotation 逐位不被触碰（normalize 的末位舍入也不改写
        // 既有姿态），见 integrate_rotation 文档。
        if rigid.angular_velocity != Vec3::ZERO {
            let (rotation, angular) = integrate_rotation(
                rotations[index],
                rigid.angular_velocity,
                rigid.angular_damping,
                dt,
            );
            rotations[index] = rotation;
            angular_velocities[index] = angular;
        }
    }
    // 仅受力积分体：同样的积分式（线性 + 角），无接触可解（运动学飞行体
    // 同样跳过——游戏驱动位移与旋转，velocity / angular_velocity 无语义）
    let mut flyer_states: Vec<FlyerState> = Vec::new();
    for &(entity, rigid) in &flyers {
        if rigid.body_type == BodyType::Kinematic {
            continue;
        }
        let Some(transform) = world.get::<Transform>(entity) else {
            continue;
        };
        let mut velocity = rigid.velocity;
        velocity += PHYSICS_GRAVITY * rigid.gravity_scale * dt;
        velocity *= (-rigid.linear_damping * dt).exp();
        let spin = if rigid.angular_velocity != Vec3::ZERO {
            let (rotation, angular) = integrate_rotation(
                transform.rotation,
                rigid.angular_velocity,
                rigid.angular_damping,
                dt,
            );
            Some((rotation, angular))
        } else {
            None
        };
        flyer_states.push(FlyerState {
            entity,
            position: transform.position + velocity * dt,
            velocity,
            spin,
        });
    }

    // 求解参数快照（速度随响应即时同步，见松弛循环内的回写）。静态与
    // 运动学体都按「静态方」参与（velocity = None → inv_mass = 0：动体
    // 撞运动学体被全额推离，运动学体本身不被挪动）
    let mut params: Vec<BodyParams> = Vec::with_capacity(n);
    for index in 0..n {
        let (_, collider, rigid) = &collected[index];
        params.push(BodyParams {
            velocity: if is_static[index] || is_kinematic[index] {
                None
            } else {
                velocities[index]
            },
            is_static: is_static[index] || is_kinematic[index],
            restitution: collider.restitution,
            friction: rigid.map(|b| b.friction).unwrap_or(0.0),
        });
    }

    // ---- 检测 + 响应 + 松弛（均匀网格 broadphase 筛候选对）----
    // 粗筛：按「积分后、求解前」的位置算每个碰撞体的世界 AABB（球 →
    // 中心 ± 半径；盒 → 中心 ± 半边长），建一次性网格。cell 尺寸取全部
    // **非静态**体的最大轴向跨度，钳制 [1.0, 50.0]——静态大地面（跨度
    // 100 m）会把 cell 撑到场景尺度、网格退化为全配对，故不参与定尺寸
    //（地面照样插入网格、照样被查询，只是不决定粒度）；无动体时回退
    // 全体最大跨度（此时只剩静-静对，配对本就全跳过，尺寸不影响结果）。
    let aabbs: Vec<(Vec3, Vec3)> = (0..n)
        .map(|index| {
            collider_aabb(
                positions[index],
                rotations[index],
                &collected[index].1.shape,
            )
        })
        .collect();
    let mut max_span = 0.0f32;
    for index in 0..n {
        if !is_static[index] && !is_kinematic[index] {
            max_span = max_span.max(aabb_span(&aabbs[index]));
        }
    }
    if max_span <= 0.0 {
        for aabb in &aabbs {
            max_span = max_span.max(aabb_span(aabb));
        }
    }
    let cell_size = max_span.clamp(1.0, 50.0);
    let mut grid = BroadphaseGrid::new(cell_size);
    for (index, (min, max)) in aabbs.iter().copied().enumerate() {
        grid.insert_aabb(index, min, max);
    }
    // 每 i 的候选一次算好（AABB 快照固定，候选与松弛轮次无关）。候选
    // 升序 + `j > i` 过滤 ⇒ 配对遍历序与原 O(n²) 双层循环完全一致——
    // 既有 bit 级确定性测试（simulation_is_deterministic_across_runs）
    // 无需改动即守护这一等价性。快照后的位置修正为穿透量级（远小于
    // cell 尺寸），候选保持保守超集（诚实边界见模块文档「v1 限制」）。
    let candidates: Vec<Vec<usize>> = (0..n)
        .map(|index| grid.candidates(aabbs[index].0, aabbs[index].1))
        .collect();

    // 静-静对跳过（互不动，永不穿透；也从不产出事件——静-静配对永不
    // 变化，begin/end 无语义）。运动学 × 运动学对与静-静同待遇（位置都
    // 由游戏驱动，物理求解对它们无话可说，见 BodyType::Kinematic）；
    // 运动学 × 静态体（含 sensor）**不跳过**——配对随游戏驱动的位移变化，
    // begin/end 语义成立（kinematic 角色踩 sensor 压力板即此）。位置分离
    // 即时生效（Gauss-Seidel 风格），共 SOLVER_ITERATIONS 轮；速度响应
    // 只在首轮施加（重复施加会把摩擦/反弹多次结算，语义错误）。接触
    // 事件只在**首轮**采样：松弛轮的重复检出对是同一配对的求解残影，
    // 无 begin/end 语义。
    for iteration in 0..SOLVER_ITERATIONS {
        for i in 0..n {
            for &j in &candidates[i] {
                if j <= i {
                    continue; // 只保留下三角（j > i），与原 (i+1)..n 内层一致
                }
                if (is_static[i] && is_static[j]) || (is_kinematic[i] && is_kinematic[j]) {
                    continue;
                }
                // 碰撞过滤（groups/mask 位掩码，见模块文档「碰撞过滤掩码」）：
                // 双向白名单都认可的配对才检测求解；被过滤对与静-静对同
                // 待遇——不检测、不求解、不进接触集合（下方分支共享）。
                // 放在 broadphase 之后：过滤只影响配对白名单、不粗筛——
                // 网格候选构建与遍历序完全不变（确定性保持），逐配对一次
                // 位与的成本可忽略。
                if collected[i].1.groups & collected[j].1.mask == 0
                    || collected[j].1.groups & collected[i].1.mask == 0
                {
                    continue;
                }
                let Some(contact) = detect_pair_rotated(
                    positions[i],
                    rotations[i],
                    &collected[i].1.shape,
                    positions[j],
                    rotations[j],
                    &collected[j].1.shape,
                ) else {
                    continue;
                };
                if iteration == 0 {
                    // 实体序归一化（a.index() ≤ b.index()）：配对以索引序
                    // 入集合；检测原始方向与索引序相反时法线随归一化翻转
                    //（保持「从 a 指向 b」的方向约定，见 CollisionStarted）
                    let (entity_a, entity_b) = (collected[i].0, collected[j].0);
                    let (pair, normal) = if entity_a.index() <= entity_b.index() {
                        ((entity_a.to_bits(), entity_b.to_bits()), contact.normal)
                    } else {
                        ((entity_b.to_bits(), entity_a.to_bits()), -contact.normal)
                    };
                    current_contacts.insert(pair, (normal, contact.penetration));
                }
                // sensor 对照常入接触集合（报事件），但跳过位置/速度求解
                // ——不推离不反弹，实体彼此穿过（见 Collider::is_sensor）
                if is_sensor[i] || is_sensor[j] {
                    continue;
                }
                let response = solve_contact(contact, params[i], params[j]);
                positions[i] += response.delta_pos_a;
                positions[j] += response.delta_pos_b;
                if iteration == 0 {
                    if let Some(v) = velocities[i].as_mut() {
                        *v += response.delta_vel_a;
                        params[i].velocity = Some(*v);
                    }
                    if let Some(v) = velocities[j].as_mut() {
                        *v += response.delta_vel_b;
                        params[j].velocity = Some(*v);
                    }
                }
            }
        }

        // ---- 约束轮（每轮接触求解之后追加，见模块文档「约束与关节」）----
        // 顺序理由：约束优先级高于接触——先把实体投影回关节语义位置
        //（结构完整性），再让下一轮接触分离穿透（外观修正）；反过来
        // 会让接触推离持续对抗约束、结构撕裂。位置修正即时生效
        //（Gauss-Seidel）；速度响应只在首轮（与接触速度响应同轮同
        // 理由——重复结算会放大阻尼/反弹）。求解序 = 收集序（距离
        // 关节先、目标点关节后），固定无随机。
        for &(owner, joint, end) in &distance_joints {
            let (pos_b, vel_b) = match end {
                JointEnd::Body(index) => (positions[index], velocities[index]),
                JointEnd::Anchor(pos, _) => (pos, None),
            };
            let response = solve_distance_joint(
                positions[owner],
                velocities[owner],
                pos_b,
                vel_b,
                joint,
                iteration == 0,
            );
            positions[owner] += response.delta_pos_a;
            if let JointEnd::Body(index) = end {
                positions[index] += response.delta_pos_b;
            }
            if iteration == 0 {
                // 速度写回；params 快照无需同步——其速度只在首轮的接触
                // 速度响应中被消费，且接触先于约束轮
                if let Some(v) = velocities[owner].as_mut() {
                    *v += response.delta_vel_a;
                }
                if let JointEnd::Body(index) = end {
                    if let Some(v) = velocities[index].as_mut() {
                        *v += response.delta_vel_b;
                    }
                }
            }
        }
        // 摆动关节（R50）：排在**距离关节之后、目标点关节之前**——同属
        // 「结构完整性」约束，先解既有拓扑（距离）再解方向限位（摆动），
        // 顺序固定无随机（确定性；收集序 = 组件存储序 = 求解序）。位置修正
        // 即时生效（Gauss-Seidel，与接触 / 距离同款）；**旋转修正是绝对
        // 覆盖**（不是增量），同轮内后解的关节因此能看到前一个关节改过的
        // 朝向——肢体链自根（躯干）向叶（手 / 脚）逐级对齐，一轮即成形。
        // 速度响应（轴向阻尼）与接触 / 距离关节同轮同理由：只在首轮施加。
        for &(owner, joint, end) in &swing_joints {
            let (pos_b, rot_b, vel_b) = match end {
                JointEnd::Body(index) => (positions[index], rotations[index], velocities[index]),
                JointEnd::Anchor(pos, rot) => (pos, rot, None),
            };
            let response = solve_swing_joint(
                positions[owner],
                rotations[owner],
                velocities[owner],
                pos_b,
                rot_b,
                vel_b,
                joint,
                iteration == 0,
            );
            positions[owner] += response.delta_pos_a;
            if let JointEnd::Body(index) = end {
                positions[index] += response.delta_pos_b;
            }
            if let Some(rotation) = response.rotation_a {
                rotations[owner] = rotation;
            }
            if iteration == 0 {
                if let Some(v) = velocities[owner].as_mut() {
                    *v += response.delta_vel_a;
                }
                if let JointEnd::Body(index) = end {
                    if let Some(v) = velocities[index].as_mut() {
                        *v += response.delta_vel_b;
                    }
                }
            }
        }
        for &(owner, joint) in &target_joints {
            let response =
                solve_target_joint(positions[owner], velocities[owner], joint, iteration == 0);
            positions[owner] += response.delta_pos_a;
            if iteration == 0 {
                if let Some(v) = velocities[owner].as_mut() {
                    *v += response.delta_vel_a;
                }
            }
        }
    }

    // ---- 阶段二（写回）：仅动体写 Transform / RigidBody，静态体与
    // 运动学体不触碰（运动学体的 Transform 位置与旋转都归游戏所有）----
    for index in 0..n {
        if is_static[index] || is_kinematic[index] {
            continue;
        }
        let (entity, _, _) = collected[index];
        if let Some(mut transform) = world.get_mut::<Transform>(entity) {
            transform.position = positions[index];
            // 旋转（R50）：角积分与摆动关节方向修正的结果；两者都没碰过时
            // 写回的就是收集值本身 ⇒ 既有场景逐位不变
            transform.rotation = rotations[index];
        }
        if let (Some(velocity), Some(mut rigid)) =
            (velocities[index], world.get_mut::<RigidBody>(entity))
        {
            rigid.velocity = velocity;
            rigid.angular_velocity = angular_velocities[index];
        }
    }

    // 仅受力积分体写回（不参与碰撞，只有位置 / 速度 / 旋转）
    for state in flyer_states {
        if let Some(mut transform) = world.get_mut::<Transform>(state.entity) {
            transform.position = state.position;
            if let Some((rotation, _)) = state.spin {
                transform.rotation = rotation;
            }
        }
        if let Some(mut rigid) = world.get_mut::<RigidBody>(state.entity) {
            rigid.velocity = state.velocity;
            if let Some((_, angular)) = state.spin {
                rigid.angular_velocity = angular;
            }
        }
    }

    // ---- 接触事件（帧末差分，见 emit_contact_events / 模块文档）----
    emit_contact_events(world, &current_contacts);
}

/// 帧末差分产出接触事件：`started = 当前 − prev`、`ended = prev − 当前`，
/// 按对序（BTree 字典序）先 Started 后 Ended 写入事件资源。
///
/// 资源缺席语义（照 `Time` 缺席静默跳过的风格，不强制注册）：
/// - [`ContactTracker`] 缺席 → 完全跳过（无差分状态基线）；
/// - [`Events<CollisionStarted>`](crate::event::Events) /
///   [`Events<CollisionEnded>`](crate::event::Events) 单独缺席 → 跳过
///   对应事件产出，但**照常推进 prev 基线**（否则后注册的一方会收到
///   积压多帧的陈旧差分）。
///
/// **销毁成对性**：`current` 只含本帧实际检出的存活实体对——上一帧
/// 配对若因 despawn / 组件移除消失，必然落入 `prev − current` 成为
/// ended（携带已死实体 id），begin/end 严格成对、世代安全。
fn emit_contact_events(
    world: &World,
    current: &std::collections::BTreeMap<(u64, u64), (Vec3, f32)>,
) {
    let Some(mut tracker) = world.try_resource_mut::<ContactTracker>() else {
        return;
    };
    let started: Vec<CollisionStarted> = if world.try_events::<CollisionStarted>().is_some() {
        current
            .iter()
            .filter(|(pair, _)| !tracker.prev.contains(pair))
            .map(|(&(lo, hi), &(normal, penetration))| CollisionStarted {
                a: Entity::from_bits(lo),
                b: Entity::from_bits(hi),
                normal,
                penetration,
            })
            .collect()
    } else {
        Vec::new()
    };
    let ended: Vec<CollisionEnded> = if world.try_events::<CollisionEnded>().is_some() {
        tracker
            .prev
            .iter()
            .filter(|pair| !current.contains_key(*pair))
            .map(|&(lo, hi)| CollisionEnded {
                a: Entity::from_bits(lo),
                b: Entity::from_bits(hi),
            })
            .collect()
    } else {
        Vec::new()
    };
    // 基线先推进再发事件（发事件不读 tracker，顺序无语义；按对序输出）
    tracker.prev = current.keys().copied().collect();
    drop(tracker);
    for event in started {
        world.try_send_event(event);
    }
    for event in ended {
        world.try_send_event(event);
    }
}

/// 世界封装的静态推离查询：把 `pos` 处的 `shape` 从世界中全部静态体的
/// 重叠中推出，返回累计修正向量（[`resolve_static_push`] 的收集 + 调用）。
///
/// 「静态体」判定与 [`step_physics`] 的推离口径一致：[`effective_static`]
/// ——有 [`Collider`] 且 `is_static`、**或无** [`RigidBody`]（组件缺席
/// 语义：只有 Collider = 静态）、**或** `body_type == Kinematic`（运动学
/// 体按静态障碍处理，见 [`BodyType::Kinematic`]）。两阶段中的收集阶段照
/// `step_physics` 同款模式（只读查询一次收齐），随后不写任何组件——纯
/// 查询，可与任何系统并存。
///
/// **旋转感知（R50）**：收集时带上 `Transform.rotation`
/// （[`StaticObstacle`]）——动画接管期的 ragdoll 肢体是「静态 + 斜置
/// 胶囊」，照实按其倒下的朝向阻挡；查询形状自身按恒竖直处理（调用方
/// 只给位置，角色控制器口径），需要旋转的查询形状用
/// [`resolve_static_push_rotated`]。
pub fn resolve_against_statics(world: &World, pos: Vec3, shape: &ColliderShape) -> Vec3 {
    let mut statics: Vec<StaticObstacle> = Vec::new();
    world.query::<(&Transform, &Collider, Option<&RigidBody>)>(|(transform, collider, rigid)| {
        // 静态体收集口径含运动学体（游戏驱动位移、按静态障碍处理，
        // 见 BodyType::Kinematic / effective_static）
        if effective_static(collider, rigid) {
            statics.push((transform.position, transform.rotation, collider.shape));
        }
    });
    resolve_static_push_rotated(pos, Quat::IDENTITY, shape, &statics)
}

// ---------------------------------------------------------------------------
// 射线 / 线段查询（视线 · 拾取 · 高速命中，见模块文档）
// ---------------------------------------------------------------------------

/// 射线命中结果：命中参数 + 命中点 + 表面法线（[`raycast`] 系列的产出）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    /// 沿射线参数：命中点 = `origin + dir × t`（`0 ≤ t ≤ max_t`）。
    pub t: f32,
    /// 命中点世界坐标。
    pub point: Vec3,
    /// 单位表面**外**法线，指向射线来侧（`dot(normal, dir) ≤ 0`）。
    pub normal: Vec3,
}

/// 零长度方向的判定容差：`length²` 低于此值按「无方向」处理（返回 `None`）。
const DIR_EPSILON_SQ: f32 = 1e-12;

/// 线段退化为点的判定容差：`from` 与 `to` 距离低于此值时
/// [`segment_hits_shape`] 返回 `None`。
const SEGMENT_EPSILON: f32 = 1e-9;

/// 射线-球求交（纯函数，几何法解二次方程取最小非负根）。
///
/// `dir` 须为**单位方向**（调用方约定；零长度方向返回 `None`）。返回
/// `(t, 表面外法线)`；`t = 0` 表示起点已在球内 / 球面上。
///
/// 退化情形的确定性选择：
/// - 起点在球内 → `t = 0`、法线 = `(origin − center).normalize_or_zero()`
///   （指向来侧即离开球心的方向）；起点与球心**重合**时该向量为零，
///   取 `-dir`（单位入参保证非零）。
/// - 球整条在射线背后（两根皆负）→ `None`。
pub fn ray_sphere(origin: Vec3, dir: Vec3, center: Vec3, radius: f32) -> Option<(f32, Vec3)> {
    if dir.length_squared() <= DIR_EPSILON_SQ {
        return None;
    }
    let oc = origin - center;
    // |origin + dir·t − center|² = r²  →  t² + 2b·t + c = 0（dir 单位长）
    let b = oc.dot(dir); // 半系数：两根 = −b ± √(b²−c)
    let c = oc.length_squared() - radius * radius;
    if c < 0.0 {
        // 起点在球内：t = 0；法线指向来侧（离开球心）。重合退化为 −dir
        // （确定性任意方向，见文档）。
        let normal = oc.normalize_or_zero();
        return Some((0.0, if normal == Vec3::ZERO { -dir } else { normal }));
    }
    let discriminant = b * b - c;
    if discriminant < 0.0 {
        return None; // 射线与球不相交
    }
    let sqrt_d = discriminant.sqrt();
    let t = -b - sqrt_d; // 最小根（外表面入射点）；负 = 起点在球内或球在背后
    if t < 0.0 {
        return None; // 球内已在上面处理，这里必为「球在射线背后」
    }
    let normal = (origin + dir * t - center).normalize_or_zero();
    Some((t, normal))
}

/// 射线-AABB 求交（纯函数，slab 法）。
///
/// `dir` 须为**单位方向**（零长度方向返回 `None`）。分母为 0 的轴按
/// 「起点在界内则该轴不裁剪、在界外则直接未命中」处理。返回
/// `(t, 入射面外法线)`；`t = 0` 表示起点已在盒内 / 盒面上。
///
/// 退化情形的确定性选择（与 [`sphere_box`] 的盒内逻辑同约定）：
/// - 起点在盒内 → `t = 0`、法线取「最浅出面」（到各面距离最小者）的
///   **外**法线，并列时按 x < y < z 优先；
/// - 入射面轴并列（斜射角点）→ 取先设置 `tmin` 的轴，即 x < y < z 优先。
pub fn ray_box(origin: Vec3, dir: Vec3, center: Vec3, half_extents: &Vec3) -> Option<(f32, Vec3)> {
    if dir.length_squared() <= DIR_EPSILON_SQ {
        return None;
    }
    let local = origin - center;
    let mut t_min = f32::MIN;
    let mut t_max = f32::MAX;
    let mut entry_axis = 2; // 产生 t_min 的轴（入射面法线的轴向）
    for axis in 0..3 {
        let (d, half, offset) = match axis {
            0 => (dir.x, half_extents.x, local.x),
            1 => (dir.y, half_extents.y, local.y),
            _ => (dir.z, half_extents.z, local.z),
        };
        if d.abs() <= DIR_EPSILON_SQ {
            // 该轴与 slab 平行：起点在界内 → 不裁剪；界外 → 未命中
            if offset.abs() > half {
                return None;
            }
            continue;
        }
        let t1 = (-half - offset) / d;
        let t2 = (half - offset) / d;
        let (near, far) = if t1 <= t2 { (t1, t2) } else { (t2, t1) };
        // 严格大于才更新入射轴：并列（角点斜射）保留先者（x < y < z 优先）
        if near > t_min {
            t_min = near;
            entry_axis = axis;
        }
        t_max = t_max.min(far);
        if t_min > t_max {
            return None;
        }
    }
    if t_max < 0.0 {
        return None; // 盒整条在射线背后
    }
    if t_min < 0.0 {
        // 起点在盒内：t = 0，法线取最浅出面的外法线（x < y < z 并列优先，
        // 与 sphere_box 盒内分支同约定；外法线指向偏移量所在侧）。
        let dx = half_extents.x - local.x.abs();
        let dy = half_extents.y - local.y.abs();
        let dz = half_extents.z - local.z.abs();
        let normal = if dx <= dy && dx <= dz {
            Vec3::new(signum(local.x), 0.0, 0.0)
        } else if dy <= dz {
            Vec3::new(0.0, signum(local.y), 0.0)
        } else {
            Vec3::new(0.0, 0.0, signum(local.z))
        };
        return Some((0.0, normal));
    }
    // 入射面外法线：与该轴方向分量相反（dot(normal, dir) ≤ 0 指向来侧）
    let sign = -signum(match entry_axis {
        0 => dir.x,
        1 => dir.y,
        _ => dir.z,
    });
    let normal = match entry_axis {
        0 => Vec3::new(sign, 0.0, 0.0),
        1 => Vec3::new(0.0, sign, 0.0),
        _ => Vec3::new(0.0, 0.0, sign),
    };
    Some((t_min, normal))
}

/// 射线-竖直胶囊求交（纯函数，解析解）：**轴恒竖直**的特化（R31 原样，
/// 浮点路径逐位不变）；轴随实体旋转用 [`ray_capsule_rotated`]。
///
/// `dir` 须为**单位方向**（零长度方向返回 `None`）。胶囊 = 无限圆柱侧壁
/// （轴段范围裁剪）+ 上下两个端帽球，取三者中最小正 `t`。推导：
///
/// - **侧壁（无限圆柱 slab）**：射线 `p(t) = origin + dir·t`，轴为过
///   `center` 的竖直线。水平距离约束 `(p.x − c.x)² + (p.z − c.z)² = R²`
///   代入射线得 `a·t² + 2b·t + c₀ = 0`，其中 `(ox, oz) = (origin − c).xz`、
///   `a = dx² + dz²`、`b = ox·dx + oz·dz`、`c₀ = ox² + oz² − R²`（dir 单位
///   长故一次项系数为 2b）。判别式 `Δ = b² − a·c₀ ≥ 0` 时近根
///   `t = (−b − √Δ) / a`；命中点 y 须落在轴段范围
///   `[c.y − (half_height − R), c.y + (half_height − R)]` 内，否则该命中
///   实属端帽区（交给端帽球判定）。`a ≈ 0`（射线竖直、与侧壁平行）时
///   无侧壁命中，只试端帽。侧壁外法线 = 命中点的水平径向
///   `((p − c).xz / R, 0)`。
/// - **端帽**：上下两球（`center ± (half_height − R)·Y`，半径 R）各走
///   [`ray_sphere`]——外法线（指向射线来侧）天然正确。
/// - **内部起点**：起点到轴段的最近距离 < R → `t = 0`，法线 = 离轴方向
///   `(origin − 轴段最近点)` 归一（照 [`ray_sphere`] 的球内语义）；最近点
///   与起点重合的退化取 `-dir`。并列 `t`（擦着端帽缘）按
///   「侧壁 → 下帽 → 上帽」先检查先保留（确定性）。
pub fn ray_capsule(
    origin: Vec3,
    dir: Vec3,
    center: Vec3,
    radius: f32,
    half_height: f32,
) -> Option<(f32, Vec3)> {
    if dir.length_squared() <= DIR_EPSILON_SQ {
        return None;
    }
    let half_axis = half_height - radius;
    // 内部起点：到轴段（竖直）最近点的距离 < 半径 → t = 0、离轴法线
    let closest_axis = Vec3::new(
        center.x,
        origin.y.clamp(center.y - half_axis, center.y + half_axis),
        center.z,
    );
    let to_axis = origin - closest_axis;
    if to_axis.length_squared() < radius * radius {
        let normal = to_axis.normalize_or_zero();
        return Some((0.0, if normal == Vec3::ZERO { -dir } else { normal }));
    }
    // 侧壁：无限圆柱 slab 解 + 轴段 y 范围裁剪
    let mut best: Option<(f32, Vec3)> = None;
    let ox = origin.x - center.x;
    let oz = origin.z - center.z;
    let a = dir.x * dir.x + dir.z * dir.z;
    if a > DIR_EPSILON_SQ {
        let b = ox * dir.x + oz * dir.z;
        let c = ox * ox + oz * oz - radius * radius;
        let discriminant = b * b - a * c;
        if discriminant >= 0.0 {
            let t = (-b - discriminant.sqrt()) / a; // 近根（外表面入射）
            if t >= 0.0 {
                let hit_y = origin.y + dir.y * t;
                if (hit_y - center.y).abs() <= half_axis {
                    // 侧壁外法线 = 水平径向（命中点绕轴指出）
                    let normal =
                        Vec3::new((ox + dir.x * t) / radius, 0.0, (oz + dir.z * t) / radius);
                    best = Some((t, normal));
                }
            }
        }
    }
    // 端帽两球：取最小正 t（严格小于才替换——并列保留先检查者，确定性）
    let mut consider = |candidate: Option<(f32, Vec3)>| {
        if let Some((t, n)) = candidate {
            if best.is_none_or(|(current, _)| t < current) {
                best = Some((t, n));
            }
        }
    };
    consider(ray_sphere(
        origin,
        dir,
        center - Vec3::Y * half_axis,
        radius,
    ));
    consider(ray_sphere(
        origin,
        dir,
        center + Vec3::Y * half_axis,
        radius,
    ));
    best
}

/// 射线-胶囊求交（纯函数，**旋转感知**，R50）：胶囊轴 = `rotation` 旋转后
/// 的本地 ±Y（与 [`detect_pair_rotated`] / [`capsule_axis`] 同口径）。
///
/// **实现 = 局部系变换 + 复用竖直解析解**（零新数学）：旋转是正交变换，
/// 保长度、保角度、保直线 ⇒ 把射线整体变换到胶囊局部系
/// （`origin' = rotation⁻¹·(origin − center)`、`dir' = rotation⁻¹·dir`，
/// 胶囊在局部系里心在原点、轴沿 ±Y），竖直解出的 `t` 就是世界系的 `t`
/// （`dir` 单位 ⇒ `dir'` 单位，仅末位舍入），命中法线再 `rotation·n`
/// 变回世界系。
///
/// `rotation == Quat::IDENTITY` 时**直接走** [`ray_capsule`]：局部系变换会
/// 重排浮点运算（`(origin.y − center.y) + dir.y·t` vs
/// `(origin.y + dir.y·t) − center.y`），末位可差 1 ULP——恒等快路径保证
/// 既有场景（胶囊恒竖直）的射线结果**逐位不变**。
pub fn ray_capsule_rotated(
    origin: Vec3,
    dir: Vec3,
    center: Vec3,
    rotation: Quat,
    radius: f32,
    half_height: f32,
) -> Option<(f32, Vec3)> {
    if rotation == Quat::IDENTITY {
        return ray_capsule(origin, dir, center, radius, half_height);
    }
    let inverse = rotation.inverse();
    let (t, normal) = ray_capsule(
        inverse * (origin - center),
        inverse * dir,
        Vec3::ZERO,
        radius,
        half_height,
    )?;
    Some((t, rotation * normal))
}

/// 射线-形状求交（[`ColliderShape`] 分发，**胶囊恒竖直**特化）＋
/// `t ≤ max_t` 裁剪。等价于 [`ray_vs_shape_rotated`] 传 `Quat::IDENTITY`。
///
/// `dir` 须为**单位方向**；`t > max_t` 视为未命中（命中点超出查询范围）。
/// 命中参数 / 命中点 / 法线打包为 [`RayHit`]。
pub fn ray_vs_shape(
    origin: Vec3,
    dir: Vec3,
    max_t: f32,
    pos: Vec3,
    shape: &ColliderShape,
) -> Option<RayHit> {
    ray_vs_shape_rotated(origin, dir, max_t, pos, Quat::IDENTITY, shape)
}

/// 射线-形状求交（[`ColliderShape`] 分发，**旋转感知**，R50）＋
/// `t ≤ max_t` 裁剪。旋转只对 Capsule 有语义（Sphere 各向同性、Box 仍是
/// 世界轴对齐 AABB——见 [`detect_pair_rotated`] 的语义边界）。
///
/// `dir` 须为**单位方向**；`t > max_t` 视为未命中（命中点超出查询范围）。
/// 命中参数 / 命中点 / 法线打包为 [`RayHit`]。
pub fn ray_vs_shape_rotated(
    origin: Vec3,
    dir: Vec3,
    max_t: f32,
    pos: Vec3,
    rotation: Quat,
    shape: &ColliderShape,
) -> Option<RayHit> {
    let (t, normal) = match shape {
        ColliderShape::Sphere { radius } => ray_sphere(origin, dir, pos, *radius)?,
        ColliderShape::Box { half_extents } => ray_box(origin, dir, pos, half_extents)?,
        ColliderShape::Capsule {
            radius,
            half_height,
        } => ray_capsule_rotated(origin, dir, pos, rotation, *radius, *half_height)?,
    };
    if t > max_t {
        return None;
    }
    Some(RayHit {
        t,
        point: origin + dir * t,
        normal,
    })
}

/// 线段-形状求交：`from → to` 的扫掠判定（高速离散体防穿模），**胶囊恒
/// 竖直**特化（等价于 [`segment_hits_shape_rotated`] 传 `Quat::IDENTITY`）。
///
/// 等价于 `ray_vs_shape(from, dir = (to − from).normalize(), max_t = 距离)`
/// ——每帧位移可大于目标直径的弹道 / 高速刚体用它替代当帧点判定。
/// `from` 与 `to` 容差内重合（零长度线段）返回 `None`。
pub fn segment_hits_shape(
    from: Vec3,
    to: Vec3,
    pos: Vec3,
    shape: &ColliderShape,
) -> Option<RayHit> {
    segment_hits_shape_rotated(from, to, pos, Quat::IDENTITY, shape)
}

/// 线段-形状求交（**旋转感知**，R50）：`from → to` 的扫掠判定，目标形状
/// 的胶囊轴随 `rotation`（倒地尸体的肢体 / 斜置圆柱柱体即此）。其余语义
/// 同 [`segment_hits_shape`]。
pub fn segment_hits_shape_rotated(
    from: Vec3,
    to: Vec3,
    pos: Vec3,
    rotation: Quat,
    shape: &ColliderShape,
) -> Option<RayHit> {
    let delta = to - from;
    let length = delta.length();
    if length <= SEGMENT_EPSILON {
        return None;
    }
    ray_vs_shape_rotated(from, delta / length, length, pos, rotation, shape)
}

/// 世界级射线查询：沿 `origin → origin + dir × max_t` 找**最近**命中的
/// 碰撞体（静态与动态都命中），返回 `(实体, RayHit)`；未命中 / 零长度
/// 方向返回 `None`。
///
/// [`raycast_filtered`] 的「无过滤」便捷封装。纯查询：不写任何组件。
pub fn raycast(world: &World, origin: Vec3, dir: Vec3, max_t: f32) -> Option<(Entity, RayHit)> {
    raycast_filtered(world, origin, dir, max_t, |_| true)
}

/// 世界级射线查询（带实体过滤）：`filter` 返回 `false` 的实体不参与命中。
///
/// `dir` 须为**单位方向**；遍历全部 `(Transform, Collider)` 取最小 `t`，
/// `t` 相等取**先遍历者**（组件存储序——确定性，与 [`step_physics`] 的
/// 收集序同源）。纯查询：不写任何组件，可与任何系统并存。
///
/// **旋转感知（R50）**：命中判定走 [`ray_vs_shape_rotated`]，胶囊轴随实体
/// `Transform.rotation`（倒地尸体 / 斜置圆柱可被正确命中）；恒等旋转经
/// 其快路径与既有行为逐位一致。
///
/// **闭包不得访问世界**（如 `world.get`）：迭代期间查询持有组件存储的
/// 读借用，`filter` 只应基于 `Entity` 做纯判定（如排除发射者、按组件
/// 标签预存的位集筛选）。
pub fn raycast_filtered(
    world: &World,
    origin: Vec3,
    dir: Vec3,
    max_t: f32,
    filter: impl Fn(Entity) -> bool,
) -> Option<(Entity, RayHit)> {
    if dir.length_squared() <= DIR_EPSILON_SQ {
        return None;
    }
    let mut best: Option<(Entity, RayHit)> = None;
    world.query::<(Entity, &Transform, &Collider)>(|(entity, transform, collider)| {
        if !filter(entity) {
            return;
        }
        let Some(hit) = ray_vs_shape_rotated(
            origin,
            dir,
            max_t,
            transform.position,
            transform.rotation,
            &collider.shape,
        ) else {
            return;
        };
        // 严格小于才替换：t 并列保留先遍历者（存储序，确定性）
        if best.is_none_or(|(_, current)| hit.t < current.t) {
            best = Some((entity, hit));
        }
    });
    best
}

// ---------------------------------------------------------------------------
// 球扫掠查询（角色控制器地基，见模块文档）
// ---------------------------------------------------------------------------

/// 球扫掠二分迭代次数：区间宽度收敛到 2⁻²⁴ ≈ 6e-8（对位移分数而言
/// 已远低于任何游戏可感知精度）。
const SWEEP_BISECTION_STEPS: usize = 24;

/// 球扫掠收缩量：命中分数从「无重叠侧」边界向回收这么多，给控制器
/// 留一层防抖皮肤。
const SWEEP_SKIN: f32 = 1e-4;

/// 扫掠重叠判定阈值：穿透深度低于此值按「恰好相切 / f32 舍入噪声」
/// 处理、不算重叠——静置在面上的形状经舍入可产生 ~1e-8 量级的名义
/// 穿透（如 0.9 − 0.5 + 1.4 的浮点路径），若算重叠会把「起点已重叠」
/// 误判、角色原地卡死。
const SWEEP_CONTACT_EPSILON: f32 = 1e-6;

/// 球扫掠（纯函数）：半径 `radius` 的球从 `origin` 位移 `displacement`，
/// 对一组静态体问「首次碰撞」。返回 `(位移分数 t ∈ [0,1], 阻挡法线)`——
/// 法线为**从静态体指向扫掠球**的单位向量（消费方沿法线切掉位移的法向
/// 分量即得滑动）。未命中返回 `None`。
///
/// [`shape_sweep`] 的球便捷封装（算法、退化与诚实边界见其文档）。
pub fn sphere_sweep(
    origin: Vec3,
    displacement: Vec3,
    radius: f32,
    statics: &[(Vec3, ColliderShape)],
) -> Option<(f32, Vec3)> {
    shape_sweep(
        origin,
        displacement,
        &ColliderShape::Sphere { radius },
        statics,
    )
}

/// 形状扫掠（纯函数，[`sphere_sweep`] 的任意形状泛化）：`shape`（如竖直
/// 胶囊 [`ColliderShape::Capsule`]）从 `origin` 位移 `displacement`，对一组
/// 静态体问「首次碰撞」。返回 `(位移分数 t ∈ [0,1], 阻挡法线)`——法线为
/// **从静态体指向扫掠形状**的单位向量（消费方沿法线切掉位移的法向分量即得
/// 滑动）。未命中返回 `None`。
///
/// 实现为**保守推进二分**（复用 [`detect_pair`]，非解析解但确定性）：
/// - 终点（t = 1）无任何重叠 → `None`；
/// - 起点（t = 0）已重叠 → `(0.0, 阻挡法线)`——取 `statics` 迭代序中
///   **首个**检出接触的静态体（确定性）；
/// - 否则以「lo 无重叠、hi 重叠」为不变式二分 24 次收敛到首次接触
///   边界，命中分数取**无重叠侧**边界 `lo` 再减 [`SWEEP_SKIN`] 收缩；
///   法线 = `−detect_pair(形状@t, 静态).normal`（接触法线从扫掠方指向
///   静态，翻转即阻挡方向），静态体取迭代序首个；浮点噪声致收缩点测不出
///   接触时回退重叠侧边界 `hi`（两者相差 ≤ 2⁻²⁴）。
///
/// **检出范围（诚实边界）**：只检出「终点重叠」括起的首次接触——
/// 起点与终点都自由的**完全贯穿**（薄障碍被整段跳过）返回 `None`。
/// 典型用法（角色控制器单帧位移被墙挡住，终点即嵌入处）恰为括弧
/// 情形；位移可能贯穿薄障碍的调用方请分步扫掠。
pub fn shape_sweep(
    origin: Vec3,
    displacement: Vec3,
    shape: &ColliderShape,
    statics: &[(Vec3, ColliderShape)],
) -> Option<(f32, Vec3)> {
    sweep_bisect(origin, displacement, |pos| {
        first_blocker(
            pos,
            Quat::IDENTITY,
            shape,
            statics.iter().map(|&(p, ref s)| (p, Quat::IDENTITY, *s)),
        )
    })
}

/// 形状扫掠（纯函数，**旋转感知**，R50）：[`shape_sweep`] 的旋转入参版
/// ——查询形状自身的朝向（`rotation`）与全部静态障碍的朝向
/// （[`StaticObstacle`]）都参与检测：斜置胶囊（倒地肢体作障碍、横倒圆柱
/// 被扫掠）照实判定。算法、退化与诚实边界全同 [`shape_sweep`]（同一个
/// 私有二分核心）；恒等旋转下与 [`shape_sweep`] 逐位一致。
pub fn shape_sweep_rotated(
    origin: Vec3,
    displacement: Vec3,
    rotation: Quat,
    shape: &ColliderShape,
    statics: &[StaticObstacle],
) -> Option<(f32, Vec3)> {
    sweep_bisect(origin, displacement, |pos| {
        first_blocker(pos, rotation, shape, statics.iter().copied())
    })
}

/// 指定位置上首个检出**真实重叠**的静态障碍（迭代序，确定性）→ 阻挡法线
/// （从静态体指向扫掠形状）。穿透 ≤ [`SWEEP_CONTACT_EPSILON`]（恰好相切 /
/// 舍入噪声）不算重叠——「起点已重叠」语义要求实际嵌入。
fn first_blocker<I>(pos: Vec3, rotation: Quat, shape: &ColliderShape, statics: I) -> Option<Vec3>
where
    I: IntoIterator<Item = StaticObstacle>,
{
    statics
        .into_iter()
        .find_map(|(static_pos, static_rotation, static_shape)| {
            detect_pair_rotated(
                pos,
                rotation,
                shape,
                static_pos,
                static_rotation,
                &static_shape,
            )
            .filter(|contact| contact.penetration > SWEEP_CONTACT_EPSILON)
            .map(|contact| -contact.normal)
        })
}

/// 保守推进二分核心（[`shape_sweep`] / [`shape_sweep_rotated`] 共用）：
/// `blocker_at(pos)` 回答「该位置上有无真实重叠、阻挡法线为何」。
/// 终点必须重叠（完全贯穿的诚实边界见 [`shape_sweep`] 文档）——`?` 在此
/// 只做存在性检查，终点阻挡法线不消费（起点 / 二分边界处各自重测）。
fn sweep_bisect(
    origin: Vec3,
    displacement: Vec3,
    blocker_at: impl Fn(Vec3) -> Option<Vec3>,
) -> Option<(f32, Vec3)> {
    blocker_at(origin + displacement)?;
    if let Some(normal) = blocker_at(origin) {
        return Some((0.0, normal)); // 起点已重叠
    }
    // 二分不变式：lo 无重叠、hi 重叠，首次接触边界夹在中间
    let mut lo = 0.0f32;
    let mut hi = 1.0f32;
    for _ in 0..SWEEP_BISECTION_STEPS {
        let mid = 0.5 * (lo + hi);
        if blocker_at(origin + displacement * mid).is_some() {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let t = (lo - SWEEP_SKIN).max(0.0);
    let normal =
        blocker_at(origin + displacement * t).or_else(|| blocker_at(origin + displacement * hi))?;
    Some((t, normal))
}

/// 世界封装的球扫掠查询：从 `from` 出发位移 `displacement` 的半径
/// `radius` 球，问世界中全部静态体的首次碰撞（收集语义与
/// [`resolve_against_statics`] 一致：[`effective_static`]——`is_static`、
/// 仅 Collider 无 RigidBody 的缺席语义，或运动学体）。纯查询：不写任何
/// 组件。
pub fn sphere_sweep_statics(
    world: &World,
    from: Vec3,
    displacement: Vec3,
    radius: f32,
) -> Option<(f32, Vec3)> {
    let mut statics: Vec<StaticObstacle> = Vec::new();
    world.query::<(&Transform, &Collider, Option<&RigidBody>)>(|(transform, collider, rigid)| {
        // 静态体收集口径含运动学体（游戏驱动位移、按静态障碍处理，
        // 见 BodyType::Kinematic / effective_static）
        if effective_static(collider, rigid) {
            statics.push((transform.position, transform.rotation, collider.shape));
        }
    });
    // 球各向同性：查询侧旋转无语义（传恒等），障碍侧旋转照实参与检测
    shape_sweep_rotated(
        from,
        displacement,
        Quat::IDENTITY,
        &ColliderShape::Sphere { radius },
        &statics,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 常规测试步长 60Hz（解析解对照测试专用，其余测试按需用更细步长）。
    const DT: f32 = 1.0 / 60.0;

    // ---- 纯函数测试：几何 / 求解不依赖世界 ----

    /// 球-球：穿透法线与分离量精确（沿法线精确恢复到恰好相切）。
    #[test]
    fn sphere_sphere_penetration_normal_and_separation() {
        // a 在原点 r=1，b 在 (1.5, 0, 0) r=1：中心距 1.5 < 半径和 2
        let a = ColliderShape::Sphere { radius: 1.0 };
        let b = ColliderShape::Sphere { radius: 1.0 };
        let contact =
            detect_pair(Vec3::ZERO, &a, Vec3::new(1.5, 0.0, 0.0), &b).expect("两球重叠应检出接触");
        assert_eq!(contact.normal, Vec3::X, "法线应从 a 指向 b（+X）");
        assert_eq!(contact.penetration, 0.5, "穿透 = 半径和 - 中心距");

        // 静态 b + 朝向 b 运动（v=+2, e=0.5）的动体 a：a 全额分离并反弹
        let response = solve_contact(
            contact,
            BodyParams::dynamic(Vec3::new(2.0, 0.0, 0.0), 0.5, 0.0),
            BodyParams::STATIC,
        );
        assert_eq!(
            response.delta_pos_a,
            Vec3::new(-0.5, 0.0, 0.0),
            "动体沿 -法线全额退出"
        );
        assert_eq!(response.delta_pos_b, Vec3::ZERO, "静态体不移动");
        assert_eq!(
            response.delta_vel_a,
            Vec3::new(-3.0, 0.0, 0.0),
            "v_n' = -e·v_n → Δv = -(1+e)·2"
        );
        assert_eq!(response.delta_vel_b, Vec3::ZERO);

        // 分离状态的球对：无接触
        assert!(detect_pair(Vec3::ZERO, &a, Vec3::new(2.1, 0.0, 0.0), &b).is_none());
        // 恰好相切：不算穿透
        assert!(detect_pair(Vec3::ZERO, &a, Vec3::new(2.0, 0.0, 0.0), &b).is_none());

        // 重合退化：法线取确定性 +Y，穿透 = 半径和
        let coincident = detect_pair(Vec3::ZERO, &a, Vec3::ZERO, &b).expect("重合应检出接触");
        assert_eq!(coincident.normal, Vec3::Y);
        assert_eq!(coincident.penetration, 2.0);
    }

    /// 球-AABB：球心正对盒顶面（法线 -Y、穿透精确）。
    #[test]
    fn sphere_box_top_face_contact() {
        // 盒半边长 (1, 0.5, 1)，顶面 y = 0.5；球 r=0.5 悬在 (0, 0.8, 0)
        let sphere = ColliderShape::Sphere { radius: 0.5 };
        let box_ = ColliderShape::Box {
            half_extents: Vec3::new(1.0, 0.5, 1.0),
        };
        let contact = detect_pair(Vec3::new(0.0, 0.8, 0.0), &sphere, Vec3::ZERO, &box_)
            .expect("球底 0.3 < 盒顶 0.5，应接触");
        assert_eq!(
            contact.normal,
            Vec3::new(0.0, -1.0, 0.0),
            "法线从球指向盒（-Y）"
        );
        assert!((contact.penetration - 0.2).abs() < 1e-6, "穿透 = 0.5 - 0.3");

        // 反转参数序（盒在前）：法线应翻回「从 a 指向 b」（+Y），穿透不变
        let flipped = detect_pair(Vec3::ZERO, &box_, Vec3::new(0.0, 0.8, 0.0), &sphere)
            .expect("参数序无关，接触仍存在");
        assert_eq!(flipped.normal, Vec3::new(0.0, 1.0, 0.0));
        assert_eq!(flipped.penetration, contact.penetration);

        // 分离：球底在盒顶之上
        assert!(
            detect_pair(Vec3::new(0.0, 1.01, 0.0), &sphere, Vec3::ZERO, &box_).is_none(),
            "球底 0.51 > 盒顶 0.5 不应接触"
        );
    }

    /// 球-AABB：侧向接触与角点接触（45° 法线）。
    #[test]
    fn sphere_box_side_and_corner_contact() {
        let sphere = ColliderShape::Sphere { radius: 0.5 };
        let box_ = ColliderShape::Box {
            half_extents: Vec3::new(1.0, 0.5, 1.0),
        };

        // 侧向：球心 (1.35, 0, 0)，最近点 (1, 0, 0)，间距 0.35
        let side =
            detect_pair(Vec3::new(1.35, 0.0, 0.0), &sphere, Vec3::ZERO, &box_).expect("侧向应接触");
        assert_eq!(side.normal, Vec3::new(-1.0, 0.0, 0.0));
        assert!((side.penetration - 0.15).abs() < 1e-6);

        // 角点：球心 (1.2, 0.7, 0)，最近点 (1, 0.5, 0)，间距 √(0.2² + 0.2²)
        let corner =
            detect_pair(Vec3::new(1.2, 0.7, 0.0), &sphere, Vec3::ZERO, &box_).expect("角点应接触");
        let want = Vec3::new(
            -std::f32::consts::FRAC_1_SQRT_2,
            -std::f32::consts::FRAC_1_SQRT_2,
            0.0,
        );
        assert!(
            (corner.normal - want).length() < 1e-5,
            "角点法线应为 45°（-X-Y），实际 {:?}",
            corner.normal
        );
        let dist = 0.2f32.hypot(0.2);
        assert!((corner.penetration - (0.5 - dist)).abs() < 1e-5);
    }

    /// 球心在盒内：沿最浅穿透轴从最近的面退出。
    #[test]
    fn sphere_center_inside_box_exits_nearest_face() {
        let sphere = ColliderShape::Sphere { radius: 0.5 };
        let box_ = ColliderShape::Box {
            half_extents: Vec3::new(1.0, 0.5, 1.0),
        };
        // 球心 (0, 0.3, 0)：到顶面 0.2、底面 0.8、侧面 1.0 → 顶面最浅
        let contact =
            detect_pair(Vec3::new(0.0, 0.3, 0.0), &sphere, Vec3::ZERO, &box_).expect("盒内应接触");
        assert_eq!(
            contact.normal,
            Vec3::new(0.0, -1.0, 0.0),
            "法线从球指向盒（顶面退出）"
        );
        assert!((contact.penetration - 0.7).abs() < 1e-6, "穿透 = 0.2 + 0.5");

        // 偏向 -X：最近面变为 -X 侧壁
        let side =
            detect_pair(Vec3::new(-0.9, 0.0, 0.0), &sphere, Vec3::ZERO, &box_).expect("盒内应接触");
        assert_eq!(
            side.normal,
            Vec3::new(1.0, 0.0, 0.0),
            "法线从球指向盒（-X 面退出）"
        );
        assert!((side.penetration - 0.6).abs() < 1e-6, "穿透 = 0.1 + 0.5");
    }

    /// AABB-AABB：最小穿透轴法线与分离；完全分离时无接触。
    #[test]
    fn box_box_min_axis_normal_and_separation() {
        let unit = ColliderShape::Box {
            half_extents: Vec3::splat(1.0),
        };
        // x 重叠 0.3，y/z 各重叠 2.0 → 法线沿 x
        let contact = detect_pair(Vec3::ZERO, &unit, Vec3::new(1.7, 0.0, 0.0), &unit)
            .expect("重叠应检出接触");
        assert_eq!(contact.normal, Vec3::X, "法线从 a 指向 b");
        assert!((contact.penetration - 0.3).abs() < 1e-6);

        // b 在 a 下方：y 最浅 → 法线 -Y
        let vertical = detect_pair(Vec3::ZERO, &unit, Vec3::new(0.0, -1.8, 0.0), &unit)
            .expect("重叠应检出接触");
        assert_eq!(vertical.normal, Vec3::new(0.0, -1.0, 0.0));
        assert!((vertical.penetration - 0.2).abs() < 1e-6);

        // 完全分离：无接触
        assert!(detect_pair(Vec3::ZERO, &unit, Vec3::new(2.01, 0.0, 0.0), &unit).is_none());
    }

    /// restitution 组合取双方最大。
    #[test]
    fn restitution_combines_by_max() {
        let contact = Contact {
            normal: Vec3::X,
            penetration: 0.5,
        };
        let response = solve_contact(
            contact,
            BodyParams::dynamic(Vec3::new(2.0, 0.0, 0.0), 0.2, 0.0),
            BodyParams::dynamic(Vec3::ZERO, 0.7, 0.0),
        );
        // e = max(0.2, 0.7) = 0.7：Δv = -(1 + 0.7) × 2 = -3.4（双方各半）
        assert!((response.delta_vel_a.x - -3.4 * 0.5).abs() < 1e-6);
        assert!((response.delta_vel_b.x - 3.4 * 0.5).abs() < 1e-6);
        assert_eq!(response.delta_vel_a.y, 0.0);
        // 分离中（v_n ≤ 0）不反弹：速度不变
        let separating = solve_contact(
            contact,
            BodyParams::dynamic(Vec3::new(-2.0, 0.0, 0.0), 0.7, 0.0),
            BodyParams::STATIC,
        );
        assert_eq!(separating.delta_vel_a, Vec3::ZERO, "背向接触面运动不应反弹");
    }

    /// 摩擦取参与响应的动体均值；切向分量按 max(0, 1-f) 衰减。
    #[test]
    fn friction_uses_dynamic_mean_and_damps_tangent() {
        let contact = Contact {
            normal: Vec3::Y,
            penetration: 0.1,
        };
        // 两动体：f = (0.5 + 0.0)/2 = 0.25 → 切向 ×0.75
        let both = solve_contact(
            contact,
            BodyParams::dynamic(Vec3::new(3.0, -4.0, 0.0), 0.0, 0.5),
            BodyParams::dynamic(Vec3::new(1.0, -4.0, 0.0), 0.0, 0.0),
        );
        let rel_after = Vec3::new(3.0, -4.0, 0.0) + both.delta_vel_a
            - (Vec3::new(1.0, -4.0, 0.0) + both.delta_vel_b);
        assert!(
            (rel_after - Vec3::new(1.5, 0.0, 0.0)).length() < 1e-5,
            "切向相对速度应从 2 衰减到 1.5、法向归零，实际 {rel_after:?}"
        );
        // 单动体：摩擦取动体自身（静体不参与）；v_n = +1 朝向穿透、e = 0 → 法向归零
        let sliding = solve_contact(
            contact,
            BodyParams::dynamic(Vec3::new(2.0, 1.0, 0.0), 0.0, 0.8),
            BodyParams::STATIC,
        );
        let v_after = Vec3::new(2.0, 1.0, 0.0) + sliding.delta_vel_a;
        assert!(
            (v_after - Vec3::new(0.4, 0.0, 0.0)).length() < 1e-5,
            "切向应 ×0.2、法向归零，实际 {v_after:?}"
        );
    }

    // ---- 世界级测试：step_physics 管线 ----

    /// 测试台：Time 资源 + 推进一帧（Time::advance 确定性驱动 + step_physics）。
    fn make_world() -> World {
        let mut world = World::new();
        world.insert_resource(Time::default());
        world
    }

    fn step(world: &World, dt: f32) {
        world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(dt as f64));
        step_physics(world);
    }

    /// 顶面 y = 0 的大静态平台（地面）。
    fn spawn_floor(world: &mut World) -> Entity {
        world.spawn((
            Transform::from_xyz(0.0, -1.0, 0.0),
            Collider::static_box(Vec3::new(50.0, 1.0, 50.0)),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_ball(
        world: &mut World,
        pos: Vec3,
        vel: Vec3,
        radius: f32,
        restitution: f32,
        friction: f32,
    ) -> Entity {
        world.spawn((
            Transform::from_translation(pos),
            Collider::dynamic_sphere(radius, restitution),
            RigidBody {
                velocity: vel,
                friction,
                ..RigidBody::default()
            },
        ))
    }

    fn ball_position(world: &World, entity: Entity) -> Vec3 {
        world
            .get::<Transform>(entity)
            .expect("球实体应有 Transform")
            .position
    }

    fn ball_velocity(world: &World, entity: Entity) -> Vec3 {
        world
            .get::<RigidBody>(entity)
            .expect("球实体应有 RigidBody")
            .velocity
    }

    /// 自由落体对照解析解 y(t) = h - ½gt²（60Hz 半隐式欧拉的离散容差：
    /// y_n = h - ½g·t·(t+dt)，比连续解析解多落 ½g·t·dt ≈ 0.082 m @ 1s）。
    #[test]
    fn free_fall_matches_analytic_solution() {
        let mut world = make_world();
        let h = 10.0;
        let ball = spawn_ball(
            &mut world,
            Vec3::new(0.0, h, 0.0),
            Vec3::ZERO,
            0.5,
            0.0,
            0.0,
        );
        let steps = 60; // 1 秒 @ 60Hz
        for _ in 0..steps {
            step(&world, DT);
        }
        let t = steps as f32 * DT;
        let y = ball_position(&world, ball).y;
        let g = 9.81f32;
        let analytic = h - 0.5 * g * t * t;
        let discrete = h - 0.5 * g * t * (t + DT); // 半隐式欧拉的精确离散解
        assert!((y - discrete).abs() < 1e-2, "y {y} 应匹配离散解 {discrete}");
        assert!(
            (y - analytic).abs() < 0.5 * g * t * DT + 1e-2,
            "y {y} 应落在解析解 {analytic} 的 60Hz 离散容差内"
        );
        assert!(
            y < analytic,
            "半隐式欧拉应比连续解析解略快落地（{y} < {analytic}）"
        );
        // 速度对照 v = g·t
        let vy = ball_velocity(&world, ball).y;
        assert!((vy - -g * t).abs() < 1e-2, "垂直速度应为 -g·t，实际 {vy}");
    }

    /// gravity_scale 线性缩放重力加速度。
    #[test]
    fn gravity_scale_scales_acceleration() {
        let mut world = make_world();
        let normal = spawn_ball(
            &mut world,
            Vec3::new(0.0, 50.0, 0.0),
            Vec3::ZERO,
            0.5,
            0.0,
            0.0,
        );
        let doubled = world.spawn((
            Transform::from_xyz(10.0, 50.0, 0.0),
            Collider::dynamic_sphere(0.5, 0.0),
            RigidBody {
                gravity_scale: 2.0,
                ..RigidBody::default()
            },
        ));
        step(&world, DT);
        let dt = Duration::from_secs_f64(DT as f64).as_secs_f32();
        assert!(
            (ball_velocity(&world, normal).y - PHYSICS_GRAVITY.y * dt).abs() < 1e-6,
            "一帧后 v = g·dt"
        );
        assert!(
            (ball_velocity(&world, doubled).y - PHYSICS_GRAVITY.y * 2.0 * dt).abs() < 1e-6,
            "scale=2 应双倍重力"
        );
        // 位置同步积分：pos += v·dt
        assert!(
            (ball_position(&world, normal).y - (50.0 + PHYSICS_GRAVITY.y * dt * dt)).abs() < 1e-6
        );
    }

    /// restitution = 0.8 的落地回弹顶点 ≈ 下落高度 × 0.64（e²），且逐次衰减。
    #[test]
    fn restitution_bounce_height_scales_with_e_squared() {
        const FINE: f32 = 1.0 / 240.0; // 细步长降低离散误差
        let mut world = make_world();
        spawn_floor(&mut world);
        // 静息球心高度 0.5，从 2.5 落下 → 下落高度 2.0；回弹顶点应 ≈ 2.0 × 0.8² = 1.28
        let ball = spawn_ball(
            &mut world,
            Vec3::new(0.0, 2.5, 0.0),
            Vec3::ZERO,
            0.5,
            0.8,
            0.0,
        );

        let mut samples: Vec<f32> = Vec::new();
        for _ in 0..900 {
            step(&world, FINE);
            samples.push(ball_position(&world, ball).y);
        }
        let rest = 0.5f32;
        let touching = |y: f32| y <= rest + 1e-3;
        let first = samples
            .iter()
            .position(|&y| touching(y))
            .expect("应发生第一次触地");
        let second = samples[first + 1..]
            .iter()
            .position(|&y| touching(y))
            .map(|i| i + first + 1)
            .expect("应发生第二次触地");
        let apex1 = samples[first + 1..second]
            .iter()
            .cloned()
            .fold(f32::MIN, f32::max);
        let apex2 = samples[second..].iter().cloned().fold(f32::MIN, f32::max);

        let expected = 2.0 * 0.8 * 0.8;
        assert!(
            ((apex1 - rest) - expected).abs() < 0.09,
            "首次回弹顶点（高于静息面）应 ≈ {expected}，实际 {}",
            apex1 - rest
        );
        assert!(
            apex2 < apex1 && apex2 > rest,
            "第二次回弹应低于首次（能量耗散）且高于静息面：{apex2} vs {apex1}"
        );
    }

    /// restitution = 0 落地静止：穿透 < 1e-3、3 秒后速度 ≈ 0、不抖。
    #[test]
    fn zero_restitution_settles_without_jitter() {
        const FINE: f32 = 1.0 / 120.0;
        let mut world = make_world();
        spawn_floor(&mut world);
        let ball = spawn_ball(
            &mut world,
            Vec3::new(0.0, 3.0, 0.0),
            Vec3::ZERO,
            0.5,
            0.0,
            0.0,
        );
        let steps = (3.0 / FINE) as usize; // 3 秒
        let mut tail: Vec<f32> = Vec::new();
        for i in 0..steps {
            step(&world, FINE);
            if i >= steps - 10 {
                tail.push(ball_position(&world, ball).y);
            }
        }
        let y = ball_position(&world, ball).y;
        let penetration = 0.5 - y; // 球心应回到静息高度（半径 0.5）
        assert!(
            penetration.abs() < 1e-3,
            "静止穿透应 < 1e-3，实际 {penetration}"
        );
        assert!(
            ball_velocity(&world, ball).length() < 1e-5,
            "静止后速度应 ≈ 0，实际 {:?}",
            ball_velocity(&world, ball)
        );
        let spread = tail.iter().cloned().fold(f32::MIN, f32::max)
            - tail.iter().cloned().fold(f32::MAX, f32::min);
        assert!(
            spread < 1e-5,
            "静止后最后 10 帧位置波动应 < 1e-5（不抖），实际 {spread}"
        );
    }

    /// 摩擦：斜落碰地后切向速度显著衰减（对比 friction = 0）。
    #[test]
    fn friction_damps_tangential_slide_on_landing() {
        const FINE: f32 = 1.0 / 120.0;
        let mut world = make_world();
        spawn_floor(&mut world);
        let sticky = spawn_ball(
            &mut world,
            Vec3::new(-10.0, 1.5, 0.0),
            Vec3::new(3.0, -6.0, 0.0),
            0.5,
            0.0,
            0.9,
        );
        let slippery = spawn_ball(
            &mut world,
            Vec3::new(10.0, 1.5, 0.0),
            Vec3::new(3.0, -6.0, 0.0),
            0.5,
            0.0,
            0.0,
        );
        for _ in 0..60 {
            step(&world, FINE); // 0.5 s：两球都已落地滑行
        }
        let sticky_vx = ball_velocity(&world, sticky).x.abs();
        let slippery_vx = ball_velocity(&world, slippery).x.abs();
        assert!(
            sticky_vx < 0.1,
            "friction=0.9 的球切向速度应几乎归零，实际 {sticky_vx}"
        );
        assert!(
            slippery_vx > 2.5,
            "friction=0 的球应保留切向速度，实际 {slippery_vx}"
        );
        assert!(
            sticky_vx < slippery_vx * 0.1,
            "摩擦球的切向应显著低于无摩擦球"
        );
    }

    /// 线性阻尼：无碰撞自由飞行的速度指数衰减对照解析式 v = v₀·e^(-λt)。
    /// 同时覆盖「只有 RigidBody 无 Collider = 仅受力积分」的缺席语义。
    #[test]
    fn linear_damping_decays_velocity_exponentially() {
        let mut world = make_world();
        let damping = 2.0f32;
        let flyer = world.spawn((
            Transform::from_xyz(0.0, 100.0, 0.0),
            RigidBody {
                velocity: Vec3::new(4.0, 0.0, 0.0),
                gravity_scale: 0.0, // 隔离重力，只看阻尼
                friction: 0.0,
                linear_damping: damping,
                angular_velocity: Vec3::ZERO,
                angular_damping: 0.0,
                body_type: BodyType::Dynamic,
            },
        ));
        let seconds = 1.0;
        // round 而非截断：1.0/(1/60 f32) = 59.999…，直接 as usize 会丢一步
        for _ in 0..(seconds / DT).round() as usize {
            step(&world, DT);
        }
        let want = 4.0 * (-damping * seconds).exp();
        let vx = ball_velocity(&world, flyer).x;
        assert!(
            (vx - want).abs() < 1e-4,
            "v 应为 v₀·e^(-λt) = {want}，实际 {vx}"
        );
        assert!(
            ball_position(&world, flyer).x > 0.0,
            "无 Collider 也应照常积分位移"
        );
    }

    /// 静态体不积分不移动：只有 Collider 的台子（被高速球撞击）、以及
    /// is_static = true 但带 RigidBody 的实体，位置都 bit 级不变。
    #[test]
    fn static_bodies_never_integrate_or_move() {
        const FINE: f32 = 1.0 / 120.0;
        let mut world = make_world();
        // 静态台子：只有 Collider（无 RigidBody 的静态缺席语义），顶面 y = 0.8
        let table_pos = Vec3::new(0.0, 0.4, 0.0);
        let table = world.spawn((
            Transform::from_translation(table_pos),
            Collider::static_box(Vec3::new(1.0, 0.4, 1.0)),
        ));
        // is_static = true 但带 RigidBody：同样按静态处理（取或语义）
        let anchored_pos = Vec3::new(5.0, 5.0, 5.0);
        let anchored = world.spawn((
            Transform::from_translation(anchored_pos),
            Collider {
                shape: ColliderShape::Sphere { radius: 0.5 },
                is_static: true,
                restitution: 0.0,
                is_sensor: false,
                groups: u32::MAX,
                mask: u32::MAX,
            },
            RigidBody {
                velocity: Vec3::new(0.0, -3.0, 0.0),
                ..RigidBody::default()
            },
        ));
        // 高速球横撞台子
        spawn_ball(
            &mut world,
            Vec3::new(-8.0, 1.3, 0.0),
            Vec3::new(9.0, 0.0, 0.0),
            0.5,
            0.3,
            0.0,
        );

        for _ in 0..120 {
            step(&world, FINE);
        }
        assert_eq!(
            ball_position(&world, table),
            table_pos,
            "只有 Collider 的静态体位置应 bit 级不变"
        );
        assert_eq!(
            ball_position(&world, anchored),
            anchored_pos,
            "is_static = true 的实体不应被积分（重力也不生效）"
        );
    }

    /// 确定性：同一场景跑两遍，逐采样位置 bit 级一致。
    #[test]
    fn simulation_is_deterministic_across_runs() {
        fn build() -> (World, Vec<Entity>) {
            let mut world = make_world();
            spawn_floor(&mut world);
            // 五颗异质球：不同高度 / 初速 / 弹性 / 摩擦 / 阻尼（坐标写死，无随机源）
            let balls = [
                (
                    Vec3::new(-2.0, 4.0, 0.0),
                    Vec3::new(1.0, 0.0, 0.5),
                    0.8,
                    0.1,
                    0.0,
                ),
                (Vec3::new(0.0, 6.5, 1.0), Vec3::ZERO, 0.5, 0.4, 0.3),
                (
                    Vec3::new(2.0, 3.2, -1.0),
                    Vec3::new(-1.5, 0.0, 0.0),
                    0.2,
                    0.8,
                    0.0,
                ),
                (
                    Vec3::new(0.5, 8.0, 0.0),
                    Vec3::new(0.5, 1.0, -0.5),
                    0.9,
                    0.2,
                    0.5,
                ),
                (Vec3::new(-1.0, 5.0, 2.0), Vec3::ZERO, 0.35, 0.6, 0.1),
            ]
            .map(|(pos, vel, radius, restitution, friction)| {
                spawn_ball(&mut world, pos, vel, radius, restitution, friction)
            })
            .to_vec();
            (world, balls)
        }

        let (world_a, balls_a) = build();
        let (world_b, balls_b) = build();
        let mut trace_a: Vec<Vec3> = Vec::new();
        let mut trace_b: Vec<Vec3> = Vec::new();
        for _ in 0..240 {
            step(&world_a, DT);
            step(&world_b, DT);
            for (&a, &b) in balls_a.iter().zip(&balls_b) {
                trace_a.push(ball_position(&world_a, a));
                trace_b.push(ball_position(&world_b, b));
            }
        }
        assert_eq!(trace_a, trace_b, "同输入两次模拟应逐采样 bit 级一致");
    }

    /// 多球落地互不重叠：终态两两距离 ≥ 半径和 - ε（堆叠松弛的残余穿透
    /// 容差），且没有球掉出地面。
    #[test]
    fn ball_pile_rests_without_overlap() {
        const FINE: f32 = 1.0 / 120.0;
        let mut world = make_world();
        spawn_floor(&mut world);
        let radius = 0.5;
        // 4 + 3 双层堆：底层 4 颗贴地排开，上层 3 颗落在缝隙上方（坐标写死）
        let starts = [
            Vec3::new(-1.65, 0.6, 0.0),
            Vec3::new(-0.55, 0.6, 0.0),
            Vec3::new(0.55, 0.6, 0.0),
            Vec3::new(1.65, 0.6, 0.0),
            Vec3::new(-1.1, 1.9, 0.0),
            Vec3::new(0.0, 2.0, 0.0),
            Vec3::new(1.1, 1.9, 0.0),
        ];
        let balls: Vec<Entity> = starts
            .iter()
            .map(|&pos| spawn_ball(&mut world, pos, Vec3::ZERO, radius, 0.05, 0.6))
            .collect();
        for _ in 0..720 {
            step(&world, FINE); // 6 秒：充分沉降
        }
        let min_gap = radius * 2.0;
        for (i, &a) in balls.iter().enumerate() {
            let pa = ball_position(&world, a);
            assert!(
                pa.y > 0.0 && pa.y < 2.0,
                "球 {i} 应停在地面附近，实际 y = {}",
                pa.y
            );
            for &b in balls.iter().skip(i + 1) {
                let dist = pa.distance(ball_position(&world, b));
                assert!(
                    dist >= min_gap - 5e-3,
                    "球 {i} 与后继球的最小间距应 ≥ {min_gap}，实际 {dist}"
                );
            }
        }
    }

    /// Time 资源缺席 → dt = 0：不积分、速度不变（空中球纹丝不动）。
    #[test]
    fn missing_time_resource_means_zero_delta() {
        let mut world = World::new(); // 故意不插入 Time
        let pos = Vec3::new(1.0, 20.0, 2.0);
        let ball = spawn_ball(&mut world, pos, Vec3::new(1.0, 0.0, 0.0), 0.5, 0.0, 0.0);
        step_physics(&world);
        assert_eq!(ball_position(&world, ball), pos, "dt = 0 不应积分位移");
        assert_eq!(
            ball_velocity(&world, ball),
            Vec3::new(1.0, 0.0, 0.0),
            "dt = 0 不应改速度"
        );
    }

    // ---- 静态推离查询：resolve_static_push / resolve_against_statics ----

    /// 单墙阻挡量精确：推离后球心到墙面恰为半径，且修正只沿法线
    /// （y / z 分量为零——切向分量保留是「贴墙滑行」的数学基础）。
    #[test]
    fn static_push_single_wall_exact_and_normal_only() {
        let sphere = ColliderShape::Sphere { radius: 0.45 };
        // +X 侧墙：内缘 x = 17.5（盒心 18.0、半边 0.5），z 向长条；
        // 玩家从场地内侧（x < 17.5）逼近
        let wall = ColliderShape::Box {
            half_extents: Vec3::new(0.5, 0.5, 19.0),
        };
        let statics = [(Vec3::new(18.0, 0.5, 0.0), wall)];
        // 球心 17.4 距内缘 0.1（球面已侵入 0.35）：推离应恰好把球心送回
        // 「内缘 − 半径」= 17.05，且不带任何切向修正
        let pos = Vec3::new(17.4, 0.45, 5.0);
        let push = resolve_static_push(pos, &sphere, &statics);
        assert!(
            (push.x + 0.35).abs() < 1e-5,
            "推离量应 = 穿透 0.35（−X 向），实际 {push:?}"
        );
        assert_eq!(push.y, 0.0, "修正只沿法线（−X），不应有 y 分量");
        assert_eq!(push.z, 0.0, "切向 z 分量保留（不把玩家往墙中段推）");
        assert!(
            (pos.x + push.x - 17.05).abs() < 1e-5,
            "推离后球心到墙面应为半径 0.45"
        );
    }

    /// 两正交墙夹角：顺序累加、逐个用推离后的位置重测——两轴修正互不
    /// 干扰，合计把球从角点推离；无重叠时恒零。
    #[test]
    fn static_push_two_orthogonal_walls_accumulate() {
        let sphere = ColliderShape::Sphere { radius: 0.45 };
        let statics = [
            (
                Vec3::new(18.0, 0.5, 0.0),
                ColliderShape::Box {
                    half_extents: Vec3::new(0.5, 0.5, 19.0),
                },
            ),
            (
                Vec3::new(0.0, 0.5, 18.0),
                ColliderShape::Box {
                    half_extents: Vec3::new(19.0, 0.5, 0.5),
                },
            ),
        ];
        // 角点内侧球面各侵入两墙 0.1 m：先对 +X 墙推离 0.35（原位置重测），
        // 再对 +Z 墙用推离后的位置重测再推 0.35
        let push = resolve_static_push(Vec3::new(17.4, 0.45, 17.4), &sphere, &statics);
        assert!(
            (push.x + 0.35).abs() < 1e-5 && (push.z + 0.35).abs() < 1e-5,
            "两墙应各贡献 0.35 的正交推离，实际 {push:?}"
        );
        // 场地中央无重叠：零向量
        let clear = resolve_static_push(Vec3::new(0.0, 0.45, 0.0), &sphere, &statics);
        assert_eq!(clear, Vec3::ZERO, "无穿透应返回零向量");
    }

    /// 空静态表恒等：没有任何静态体时返回零向量（位置原样可用）。
    #[test]
    fn static_push_empty_statics_is_identity() {
        let sphere = ColliderShape::Sphere { radius: 0.45 };
        let push = resolve_static_push(Vec3::new(3.0, 0.45, -4.0), &sphere, &[]);
        assert_eq!(push, Vec3::ZERO);
    }

    /// 世界封装：收集遵循「仅 Collider 无 RigidBody = 静态」缺席语义——
    /// 挡路的是静态墙；路径上的动态球（完整刚体）不参与推离，
    /// `is_static = true` 的实体（即使带 RigidBody）同样静态。
    #[test]
    fn resolve_against_statics_skips_dynamic_bodies() {
        let mut world = World::new();
        // 静态墙：只有 Collider（组件缺席 = 静态）
        world.spawn((
            Transform::from_xyz(18.0, 0.5, 0.0),
            Collider::static_box(Vec3::new(0.5, 0.5, 19.0)),
        ));
        // 动态球停在推离路径上（与查询球明显重叠）：完整刚体——
        // 若被错误计入，下方断言的推离量将不止墙的 0.35
        world.spawn((
            Transform::from_xyz(17.3, 0.45, 0.0),
            Collider::dynamic_sphere(0.45, 0.0),
            RigidBody::default(),
        ));
        let sphere = ColliderShape::Sphere { radius: 0.45 };
        // 球心 17.4（球面侵入内缘 0.35）：只应被静态墙推回 0.35——
        // 若把路径上的动态球错误计入，推离量将明显更多
        let push = resolve_against_statics(&world, Vec3::new(17.4, 0.45, 0.0), &sphere);
        assert!(
            (push.x + 0.35).abs() < 1e-5,
            "只应被静态墙推离，实际 {push:?}"
        );

        // is_static = true + RigidBody：取或语义，同样静态、同样推离。
        // （摆在第一面墙的 z 跨度之外——同轴重叠的两个静态体会被先后
        // 各推一次，即文档所述 v1 的角点重复推离简化，测试不依赖它。）
        world.spawn((
            Transform::from_xyz(28.0, 0.5, 4.0),
            Collider {
                shape: ColliderShape::Box {
                    half_extents: Vec3::new(0.5, 0.5, 19.0),
                },
                is_static: true,
                restitution: 0.0,
                is_sensor: false,
                groups: u32::MAX,
                mask: u32::MAX,
            },
            RigidBody {
                velocity: Vec3::new(1.0, 0.0, 0.0),
                ..RigidBody::default()
            },
        ));
        let push2 = resolve_against_statics(&world, Vec3::new(27.4, 0.45, 4.0), &sphere);
        assert!(
            (push2.x + 0.35).abs() < 1e-5,
            "is_static = true 应同样参与推离，实际 {push2:?}"
        );
    }

    // ---- 射线 / 线段查询：ray_sphere / ray_box / ray_vs_shape / segment ----

    /// 射线-球：正对命中 t / 法线精确；未命中；球在射线背后；起点在球内；
    /// 零长度方向。
    #[test]
    fn ray_sphere_hit_miss_inside_and_degenerates() {
        let dir = Vec3::new(0.0, 0.0, -1.0); // 单位方向：-Z
        // 球心原点 r=1，射线起点 (0,0,5)：t = 5 − 1 = 4，命中点 (0,0,1)
        let (t, normal) =
            ray_sphere(Vec3::new(0.0, 0.0, 5.0), dir, Vec3::ZERO, 1.0).expect("正对球心应命中");
        assert_eq!(t, 4.0, "t = 起距 − 半径");
        assert_eq!(normal, Vec3::Z, "外法线指向射线来侧（+Z）");

        // 未命中：射线从球侧旁掠过（最近距 2 > 半径 1）
        assert!(ray_sphere(Vec3::new(0.0, 2.0, 5.0), dir, Vec3::ZERO, 1.0).is_none());

        // 背面：球整条在射线背后（两根皆负）
        assert!(
            ray_sphere(
                Vec3::new(0.0, 0.0, 5.0),
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::ZERO,
                1.0
            )
            .is_none(),
            "背向球的射线不应命中"
        );

        // 起点在球内：t = 0、法线 = 离开球心的单位向量（指向来侧）
        let (t_in, n_in) = ray_sphere(Vec3::new(0.0, 0.0, 0.5), dir, Vec3::ZERO, 1.0)
            .expect("球内起点应命中 t = 0");
        assert_eq!(t_in, 0.0);
        assert_eq!(n_in, Vec3::Z);

        // 起点与球心重合：退化为 −dir（确定性任意方向，见文档）
        let (t_zero, n_zero) =
            ray_sphere(Vec3::new(0.2, 0.0, 0.0), dir, Vec3::new(0.2, 0.0, 0.0), 1.0)
                .expect("重合起点应命中 t = 0");
        assert_eq!(t_zero, 0.0);
        assert_eq!(n_zero, -dir, "重合退化取 −dir");

        // 零长度方向：None
        assert!(ray_sphere(Vec3::new(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::ZERO, 1.0).is_none());
    }

    /// 射线-盒：正对面命中 t / 法线精确；盒内起点最浅出面；斜射角点法线取
    /// 主轴（x 优先）；平行轴起点在界外未命中。
    #[test]
    fn ray_box_face_inside_corner_and_parallel_axis() {
        let half = Vec3::new(1.0, 0.5, 1.0);
        // 正对盒面：起点 (0,0,5) 沿 -Z，命中 z = 1 面：t = 4、法线 +Z
        let (t, normal) = ray_box(
            Vec3::new(0.0, 0.0, 5.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::ZERO,
            &half,
        )
        .expect("正对盒面应命中");
        assert_eq!(t, 4.0);
        assert_eq!(normal, Vec3::Z, "入射面外法线指向射线来侧");

        // 盒内起点：t = 0、法线取最浅出面的外法线（x<y<z 并列优先）
        // 局部 (0, 0.3, 0)：到顶面 0.2 < 侧面 1.0 < 底面 0.8 → +Y 面
        let (t_in, n_in) = ray_box(
            Vec3::new(0.0, 0.3, 0.0),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::ZERO,
            &half,
        )
        .expect("盒内起点应命中 t = 0");
        assert_eq!(t_in, 0.0);
        assert_eq!(n_in, Vec3::Y, "最浅出面为顶面（+Y 外法线）");

        // 斜射角点：单位立方盒，从 (5,5,0) 沿 (−1,−1,0)/√2 入射，x / y
        // 两轴同时给出 tmin（命中点在角点 (1,1,0)）→ 入射轴并列取 x
        // （确定性），法线 = −sign(dir.x)·X = +X（主轴，非角点对角向量）
        let diagonal = Vec3::new(-1.0, -1.0, 0.0).normalize();
        let (t_c, n_c) = ray_box(Vec3::new(5.0, 5.0, 0.0), diagonal, Vec3::ZERO, &Vec3::ONE)
            .expect("斜射角点应命中");
        assert!(
            (t_c - 4.0 * std::f32::consts::SQRT_2).abs() < 1e-5,
            "t = 4√2，实际 {t_c}"
        );
        assert_eq!(n_c, Vec3::X, "角点斜射法线取主轴 x（并列优先）");

        // 平行轴且起点在界外：x 向射线从 y=5（盒半边 0.5）掠过 → 未命中
        assert!(ray_box(Vec3::new(0.0, 5.0, 0.0), Vec3::X, Vec3::ZERO, &half).is_none());

        // 平行轴且起点在界内：该轴不裁剪，沿 x 贯穿命中侧面 x = 1
        let (t_p, n_p) =
            ray_box(Vec3::new(-5.0, 0.0, 0.0), Vec3::X, Vec3::ZERO, &half).expect("贯穿应命中");
        assert_eq!(t_p, 4.0);
        assert_eq!(n_p, Vec3::new(-1.0, 0.0, 0.0));

        // 背面：盒在射线背后
        assert!(
            ray_box(
                Vec3::new(0.0, 0.0, 5.0),
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::ZERO,
                &half
            )
            .is_none()
        );
    }

    /// 线段扫掠防穿模：步长（10 m）远大于目标直径（1.6 m），两端点都在
    /// 目标外——当帧点判定必漏检，线段判定命中；from == to 容差内为 None。
    #[test]
    fn segment_hits_shape_catches_high_speed_tunnelling() {
        let shape = ColliderShape::Sphere { radius: 0.8 };
        let from = Vec3::new(0.0, 0.0, 0.0);
        let to = Vec3::new(10.0, 0.0, 0.0);
        // 佐证「点判定必漏」：两端点到球心距都是 5 > 0.8
        let center = Vec3::new(5.0, 0.0, 0.0);
        assert!(from.distance(center) > 0.8 && to.distance(center) > 0.8);
        let hit = segment_hits_shape(from, to, center, &shape).expect("线段应命中扫掠球");
        assert!(
            (hit.t - 4.2).abs() < 1e-5,
            "命中参数 = 5 − 0.8 = 4.2，实际 {}",
            hit.t
        );
        assert!(
            (hit.point - Vec3::new(4.2, 0.0, 0.0)).length() < 1e-5,
            "命中点 = (4.2, 0, 0)，实际 {:?}",
            hit.point
        );
        assert_eq!(
            hit.normal,
            Vec3::new(-1.0, 0.0, 0.0),
            "法线指向线段来侧（−X）"
        );

        // from == to 容差内：None（零长度线段无扫掠语义）
        assert!(segment_hits_shape(from, from, center, &shape).is_none());
        // 起点已在目标内：t = 0（近距离判定仍成立）
        let inside = segment_hits_shape(
            Vec3::new(5.0, 0.0, 0.3),
            Vec3::new(6.0, 0.0, 0.3),
            center,
            &shape,
        )
        .expect("扫掠段穿过球体应命中");
        assert_eq!(inside.t, 0.0);
    }

    /// 世界级 raycast：静态动态都命中、最近者胜；filter 排除发射者（最近
    /// 命中）后取次近；max_t 裁剪；空世界 / 零方向 None。
    #[test]
    fn raycast_picks_nearest_and_filter_excludes_shooter() {
        let mut world = World::new();
        // 近处动态球（t=4）与远处静态球（t=11）沿 +Z 排开
        let near = world.spawn((
            Transform::from_xyz(0.0, 0.0, 5.0),
            Collider::dynamic_sphere(1.0, 0.0),
            RigidBody::default(),
        ));
        let far = world.spawn((
            Transform::from_xyz(0.0, 0.0, 12.0),
            Collider::static_sphere(1.0),
        ));
        let dir = Vec3::Z;

        let (entity, hit) = raycast(&world, Vec3::ZERO, dir, 50.0).expect("应命中最近的近处球");
        assert_eq!(entity, near, "最近命中（t=4）应优先于远处（t=11）");
        assert!((hit.t - 4.0).abs() < 1e-6);
        assert_eq!(
            hit.normal,
            Vec3::new(0.0, 0.0, -1.0),
            "外法线指向射线来侧（−Z）"
        );

        // filter 排除发射者（近处球）→ 命中远处静态球（静态动态语义一致）
        let (entity_f, hit_f) = raycast_filtered(&world, Vec3::ZERO, dir, 50.0, |e| e != near)
            .expect("排除近处后应命中远处球");
        assert_eq!(entity_f, far);
        assert!((hit_f.t - 11.0).abs() < 1e-6);

        // max_t 裁剪：4 < t ≤ 11 的查询范围内只有近处；全排除时 None
        assert_eq!(raycast(&world, Vec3::ZERO, dir, 10.0).unwrap().0, near);
        assert!(raycast_filtered(&world, Vec3::ZERO, dir, 10.0, |e| e != near).is_none());
        // 零长度方向 / 空世界：None
        assert!(raycast(&world, Vec3::ZERO, Vec3::ZERO, 10.0).is_none());
        let empty = World::new();
        assert!(raycast(&empty, Vec3::ZERO, dir, 10.0).is_none());
    }

    // ---- 球扫掠查询：sphere_sweep / sphere_sweep_statics ----

    /// 扫掠畅通返回 None；斜向滑过角点（最近距 0.566 > 半径 0.5）不误报。
    #[test]
    fn sphere_sweep_clear_path_and_corner_graze_miss() {
        let wall = ColliderShape::Box {
            half_extents: Vec3::new(0.5, 5.0, 5.0),
        };
        let statics = [(Vec3::new(18.0, 0.0, 0.0), wall)];
        // 背向墙位移：整段畅通
        assert!(
            sphere_sweep(Vec3::ZERO, Vec3::new(-5.0, 0.0, 0.0), 0.5, &statics).is_none(),
            "远离墙的扫掠应畅通"
        );

        // 斜向滑过单位盒角点：路径沿 y = −x + 3 从 (1,2) 位移 (0.7,−0.7)，
        // 到盒（角点 (1,1)）的最近距 ≈ 0.762 > 半径 0.5 → 不误报
        let unit_box = ColliderShape::Box {
            half_extents: Vec3::ONE,
        };
        let statics = [(Vec3::ZERO, unit_box)];
        assert!(
            sphere_sweep(
                Vec3::new(1.0, 2.0, 0.0),
                Vec3::new(0.7, -0.7, 0.0),
                0.5,
                &statics
            )
            .is_none(),
            "最近距 0.762 > 0.5 的斜掠不应误报命中"
        );
        // 非空转佐证：同一位置与路径、半径放大到 0.8（> 终点距 0.762，
        // < 起点距 1.0——终点重叠、起点自由的括弧情形）应命中
        assert!(
            sphere_sweep(
                Vec3::new(1.0, 2.0, 0.0),
                Vec3::new(0.7, -0.7, 0.0),
                0.8,
                &statics
            )
            .is_some(),
            "半径 0.8 覆盖角点附近路径的斜掠应命中（测试非空转的佐证）"
        );
    }

    /// 扫掠正碰：半径 0.5 的球向墙位移 2.0、球心到墙面 1.2 →
    /// t ≈ (1.2 − 0.5) / 2 = 0.35、阻挡法线 −X；起点已重叠时 t = 0。
    #[test]
    fn sphere_sweep_head_on_fraction_and_normal() {
        // 墙内缘面 x = 1.2（盒心 1.7、半边 0.5）
        let wall = ColliderShape::Box {
            half_extents: Vec3::new(0.5, 5.0, 5.0),
        };
        let statics = [(Vec3::new(1.7, 0.0, 0.0), wall)];
        let (t, normal) = sphere_sweep(Vec3::ZERO, Vec3::new(2.0, 0.0, 0.0), 0.5, &statics)
            .expect("朝墙位移 2 m 应碰撞");
        assert!(
            (t - 0.35).abs() < 1e-3,
            "首次碰撞分数 ≈ 0.35（皮肤收缩 1e-4），实际 {t}"
        );
        assert!(
            (normal - Vec3::new(-1.0, 0.0, 0.0)).length() < 1e-5,
            "阻挡法线应从墙指向球（−X），实际 {normal:?}"
        );

        // 起点已重叠（球心 0.8、球面越过墙面 1.2 的 0.1）：t = 0、法线同向
        let (t_in, n_in) = sphere_sweep(
            Vec3::new(0.8, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            0.5,
            &statics,
        )
        .expect("起点重叠应报告 t = 0");
        assert_eq!(t_in, 0.0);
        assert!((n_in - Vec3::new(-1.0, 0.0, 0.0)).length() < 1e-5);
    }

    /// 世界封装：只对静态体扫掠（动态刚体不阻挡），缺席语义与
    /// resolve_against_statics 一致。
    #[test]
    fn sphere_sweep_statics_skips_dynamic_bodies() {
        let mut world = World::new();
        // 动态球横在路径上：不参与阻挡
        world.spawn((
            Transform::from_xyz(2.0, 0.0, 0.0),
            Collider::dynamic_sphere(1.0, 0.0),
            RigidBody::default(),
        ));
        // 静态墙：内缘 x = 5.5（盒心 6.0、半边 0.5）
        world.spawn((
            Transform::from_xyz(6.0, 0.0, 0.0),
            Collider::static_box(Vec3::new(0.5, 5.0, 5.0)),
        ));
        // 位移 5.6：终点 (5.6,0,0) 嵌入墙内（终点重叠的括弧情形）；
        // 路径上的动态球（半径 1，距路径起点仅 2）若被错误计入会在
        // t = 1/5.6 ≈ 0.18 处阻挡——远早于墙的 0.89
        let (t, normal) = sphere_sweep_statics(&world, Vec3::ZERO, Vec3::new(5.6, 0.0, 0.0), 0.5)
            .expect("应被静态墙阻挡");
        // 接触点：球心到 5.5 − 0.5 = 5.0 → t = 5/5.6 ≈ 0.8929
        assert!(
            (t - 5.0 / 5.6).abs() < 1e-3,
            "只应被静态墙挡在 t ≈ 0.893，实际 {t}"
        );
        assert!((normal - Vec3::new(-1.0, 0.0, 0.0)).length() < 1e-5);
        // 反向（远离墙）：畅通
        assert!(sphere_sweep_statics(&world, Vec3::ZERO, Vec3::new(-5.6, 0.0, 0.0), 0.5).is_none());
    }

    // ---- broadphase 接入：密集场景求解后无重叠（网格候选保守性回归）----

    /// 100 球密集阵（初始间距 0.62 < 直径 0.7，即摆放即穿透）：交给
    /// step_physics（均匀网格 broadphase 配对）挤开，6 s 沉降后两两不
    /// 重叠、无球掉出地面——网格候选漏对时密集阵是首先暴露的场景。
    #[test]
    fn hundred_ball_dense_grid_separates_without_overlap() {
        const FINE: f32 = 1.0 / 120.0;
        let mut world = make_world();
        spawn_floor(&mut world);
        let radius = 0.35;
        // 10×10 方阵：中心对称排布，全部同层 y = 半径
        let mut balls: Vec<Entity> = Vec::with_capacity(100);
        for row in 0..10 {
            for col in 0..10 {
                let pos = Vec3::new((col as f32 - 4.5) * 0.62, radius, (row as f32 - 4.5) * 0.62);
                balls.push(spawn_ball(&mut world, pos, Vec3::ZERO, radius, 0.05, 0.6));
            }
        }
        for _ in 0..720 {
            step(&world, FINE); // 6 s：充分挤开 + 沉降
        }
        let positions: Vec<Vec3> = balls.iter().map(|&b| ball_position(&world, b)).collect();
        let min_gap = radius * 2.0;
        for (i, &pa) in positions.iter().enumerate() {
            assert!(
                pa.y > 0.0 && pa.y < 2.0,
                "球 {i} 应停在地面附近，实际 y = {pa}"
            );
            for (j, &pb) in positions.iter().enumerate().skip(i + 1) {
                let dist = pa.distance(pb);
                assert!(
                    dist >= min_gap - 5e-3,
                    "球 ({i},{j}) 求解后最小间距应 ≥ {min_gap}，实际 {dist}"
                );
            }
        }
    }

    // ---- 竖直胶囊：capsule-sphere / capsule-AABB / capsule-capsule ----

    /// 半径 0.5、半高 1.5 的竖直胶囊（轴段 y ∈ [−1, 1]）。
    fn capsule() -> ColliderShape {
        ColliderShape::Capsule {
            radius: 0.5,
            half_height: 1.5,
        }
    }

    /// capsule-球：侧向（最近点在轴段内，等价最近点处的球-球）/ 端帽
    ///（最近点 = 轴段端点，法线斜向）/ 反向参数序法线翻转 / 分离。
    #[test]
    fn capsule_sphere_lateral_end_cap_and_reverse() {
        // 侧向：球心 (0.9, 0.3, 0)，轴段最近点 (0, 0.3, 0)（参数 0.65，
        // 段内）→ 等价球-球：中心距 0.9 < 半径和 1.0
        let sphere = ColliderShape::Sphere { radius: 0.5 };
        let side = detect_pair(Vec3::new(0.9, 0.3, 0.0), &sphere, Vec3::ZERO, &capsule())
            .expect("侧向应接触");
        assert!(
            (side.normal - Vec3::new(-1.0, 0.0, 0.0)).length() < 1e-5,
            "球在前：法线应从球指向胶囊（−X），实际 {:?}",
            side.normal
        );
        assert!((side.penetration - 0.1).abs() < 1e-5, "穿透 = 1.0 − 0.9");

        // 胶囊在前：法线从胶囊指向球（+X），穿透不变
        let forward = detect_pair(Vec3::ZERO, &capsule(), Vec3::new(0.9, 0.3, 0.0), &sphere)
            .expect("侧向应接触");
        assert!(
            (forward.normal - Vec3::X).length() < 1e-5,
            "胶囊在前：法线应从胶囊指向球（+X），实际 {:?}",
            forward.normal
        );
        assert_eq!(forward.penetration, side.penetration);

        // 端帽：球心 (0.9, 1.4, 0)，最近点 = 轴段上端点 (0, 1, 0)，
        // 距离 √(0.81 + 0.16) = √0.97 → 穿透 1 − √0.97、法线斜向
        let cap = detect_pair(Vec3::ZERO, &capsule(), Vec3::new(0.9, 1.4, 0.0), &sphere)
            .expect("端帽应接触");
        let dist = 0.97f32.sqrt();
        let want_normal = Vec3::new(0.9 / dist, 0.4 / dist, 0.0);
        assert!(
            (cap.normal - want_normal).length() < 1e-5,
            "端帽法线应从端点指向球心，实际 {:?}",
            cap.normal
        );
        assert!((cap.penetration - (1.0 - dist)).abs() < 1e-5);

        // 分离：球心距上端点 √(1.44 + 0.16) = 1.265 > 半径和 1.0
        assert!(
            detect_pair(Vec3::ZERO, &capsule(), Vec3::new(1.2, 1.4, 0.0), &sphere).is_none(),
            "距端点 1.265 > 1.0 不应接触"
        );
    }

    /// capsule-AABB：侧面（法线 −X）/ 端帽悬于顶面上方（法线 −Y、最近点
    /// = 轴段下端点）/ 反向参数序 / 完全分离。
    #[test]
    fn capsule_box_side_end_cap_and_reverse() {
        let box_ = ColliderShape::Box {
            half_extents: Vec3::new(1.0, 0.5, 1.0),
        };

        // 侧面：胶囊轴 x = 1.35（盒右缘 1.0），轴段 y ∈ [−0.8, 1.2] 与盒
        // y ∈ [−0.5, 0.5] 重叠 → 交替投影收敛到最近点 (1.35, 0.2, 0)，
        // 间距 0.35 < 半径 0.5 → 穿透 0.15、法线从胶囊指向盒（−X）
        let side = detect_pair(Vec3::new(1.35, 0.2, 0.0), &capsule(), Vec3::ZERO, &box_)
            .expect("侧面应接触");
        assert_eq!(side.normal, Vec3::new(-1.0, 0.0, 0.0));
        assert!((side.penetration - 0.15).abs() < 1e-6);

        // 端帽：胶囊悬于盒顶上方（中心 y = 1.75，轴段下端点 (0, 0.75, 0)），
        // 最近点 = 下端点、到顶面 (0, 0.5, 0) 间距 0.25 → 穿透 0.25、法线 −Y
        let cap = detect_pair(Vec3::new(0.0, 1.75, 0.0), &capsule(), Vec3::ZERO, &box_)
            .expect("顶面端帽应接触");
        assert_eq!(
            cap.normal,
            Vec3::new(0.0, -1.0, 0.0),
            "法线从胶囊指向盒（−Y）"
        );
        assert!((cap.penetration - 0.25).abs() < 1e-6);

        // 反向参数序（盒在前）：法线翻回 a → b（+Y），穿透不变
        let flipped = detect_pair(Vec3::ZERO, &box_, Vec3::new(0.0, 1.75, 0.0), &capsule())
            .expect("参数序无关，接触仍存在");
        assert_eq!(flipped.normal, Vec3::new(0.0, 1.0, 0.0));
        assert_eq!(flipped.penetration, cap.penetration);

        // 完全分离：轴段下端点 (0, 1.2, 0) 距顶面 0.7 > 半径 0.5
        assert!(
            detect_pair(Vec3::new(0.0, 2.2, 0.0), &capsule(), Vec3::ZERO, &box_).is_none(),
            "距顶面 0.7 > 0.5 不应接触"
        );
    }

    /// capsule-capsule：平行轴（都竖直）距离解析对照——y 区间重叠时距离
    /// = 轴线水平距；区间错开时端点距离；反向参数序法线翻转。
    #[test]
    fn capsule_capsule_parallel_axis_analytic() {
        // 同尺寸胶囊 B 挪到 (0.8, 1.6, 0)：轴段 y ∈ [0.6, 2.6] 与 A 的
        // [−1, 1] 重叠 → 最近距离 = 轴线水平距 0.8 < 半径和 1.0
        let b_pos = Vec3::new(0.8, 1.6, 0.0);
        let pair = detect_pair(Vec3::ZERO, &capsule(), b_pos, &capsule()).expect("平行轴应接触");
        assert_eq!(pair.normal, Vec3::X, "法线从 a 指向 b（+X）");
        assert!((pair.penetration - 0.2).abs() < 1e-6, "穿透 = 1.0 − 0.8");

        // 反向参数序：法线翻转（−X），穿透不变
        let flipped = detect_pair(b_pos, &capsule(), Vec3::ZERO, &capsule()).expect("应接触");
        assert_eq!(flipped.normal, Vec3::new(-1.0, 0.0, 0.0));
        assert_eq!(flipped.penetration, pair.penetration);

        // y 区间错开 + 水平错开：B 轴段 y ∈ [2.6, 4.6]，最近点对为
        // A 上端点 (0, 1, 0) 与 B 下端点 (1.2, 2.6, 0)，距离
        // √(1.44 + 2.56) = 2.0 > 半径和 → 不接触
        let apart = Vec3::new(1.2, 3.6, 0.0);
        assert!(
            detect_pair(Vec3::ZERO, &capsule(), apart, &capsule()).is_none(),
            "端点距 2.0 = 半径和恰相切，不应接触"
        );
    }

    /// half_height = radius 的退化胶囊按球语义求解（零长轴段）；
    /// half_height < radius 的构造断言失败（中文断言消息）。
    #[test]
    fn degenerate_capsule_behaves_as_sphere() {
        let degenerate = ColliderShape::Capsule {
            radius: 0.5,
            half_height: 0.5,
        };
        let sphere = ColliderShape::Sphere { radius: 0.5 };
        let from_capsule = detect_pair(Vec3::ZERO, &degenerate, Vec3::new(0.9, 0.0, 0.0), &sphere)
            .expect("退化胶囊应接触");
        let from_sphere = detect_pair(Vec3::ZERO, &sphere, Vec3::new(0.9, 0.0, 0.0), &sphere)
            .expect("对照球应接触");
        assert_eq!(from_capsule, from_sphere, "退化胶囊应与同半径球完全一致");
    }

    #[test]
    #[should_panic(expected = "须 ≥ radius")]
    fn capsule_constructor_rejects_half_height_below_radius() {
        let _ = ColliderShape::capsule(0.5, 0.4);
    }

    // ---- 射线-胶囊：ray_capsule（圆柱 slab + 端帽球，取最小正 t）----

    /// 正交命中圆柱段 t 解析精确 / 端帽命中（斜法线）/ 未命中 / 内部起点
    /// t = 0 / 背后射线 / ray_vs_shape 的 max_t 裁剪。
    #[test]
    fn ray_capsule_cylinder_cap_miss_inside_and_clipping() {
        let shape = capsule();

        // 正交命中圆柱段：起点 (5, 0.3, 0) 沿 −X → t = 5 − 0.5 = 4.5、
        // 命中点 (0.5, 0.3, 0)、外法线 +X（全部二进制可精确保留）
        let (t, normal) = ray_capsule(
            Vec3::new(5.0, 0.3, 0.0),
            Vec3::new(-1.0, 0.0, 0.0),
            Vec3::ZERO,
            0.5,
            1.5,
        )
        .expect("正交命中圆柱段");
        assert_eq!(t, 4.5, "t = 起距 − 半径");
        assert_eq!(normal, Vec3::X, "侧壁外法线 = 水平径向");

        // 端帽命中：起点 (5, 1.3, 0) 高于圆柱段（y > 1）→ 上端帽球
        //（心 (0, 1, 0)）解：(5−t)² + 0.09 = 0.25 → t = 4.6、
        // 法线 = (0.4, 0.3, 0)/0.5 = (0.8, 0.6, 0)
        let (t_cap, n_cap) = ray_capsule(
            Vec3::new(5.0, 1.3, 0.0),
            Vec3::new(-1.0, 0.0, 0.0),
            Vec3::ZERO,
            0.5,
            1.5,
        )
        .expect("端帽应命中");
        assert!((t_cap - 4.6).abs() < 1e-5, "t = 4.6，实际 {t_cap}");
        assert!(
            (n_cap - Vec3::new(0.8, 0.6, 0.0)).length() < 1e-5,
            "端帽法线应斜向，实际 {n_cap:?}"
        );

        // 未命中：起点 (5, 1.6, 0) 高于端帽顶（1.5）且判别式 < 0
        assert!(
            ray_capsule(
                Vec3::new(5.0, 1.6, 0.0),
                Vec3::new(-1.0, 0.0, 0.0),
                Vec3::ZERO,
                0.5,
                1.5
            )
            .is_none(),
            "从胶囊上方掠过不应命中"
        );

        // 内部起点：(0.2, 0, 0) 距轴 0.2 < 0.5 → t = 0、法线 = 离轴方向
        let (t_in, n_in) = ray_capsule(
            Vec3::new(0.2, 0.0, 0.0),
            Vec3::new(-1.0, 0.0, 0.0),
            Vec3::ZERO,
            0.5,
            1.5,
        )
        .expect("内部起点应命中 t = 0");
        assert_eq!(t_in, 0.0);
        assert_eq!(n_in, Vec3::X, "法线 = 离轴方向");
        // 起点在轴上：离轴向量退化 → 取 −dir（确定性任意方向）
        let (t_axis, n_axis) =
            ray_capsule(Vec3::ZERO, Vec3::new(-1.0, 0.0, 0.0), Vec3::ZERO, 0.5, 1.5)
                .expect("轴上起点应命中 t = 0");
        assert_eq!(t_axis, 0.0);
        assert_eq!(n_axis, Vec3::X, "重合退化取 −dir");

        // 背后：胶囊在射线背后（近根为负）
        assert!(
            ray_capsule(
                Vec3::new(5.0, 0.3, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::ZERO,
                0.5,
                1.5
            )
            .is_none(),
            "背向胶囊的射线不应命中"
        );

        // ray_vs_shape 分发 + max_t 裁剪：4.5 > 4.0 视为未命中；放宽到 5 命中
        assert!(
            ray_vs_shape(
                Vec3::new(5.0, 0.3, 0.0),
                Vec3::new(-1.0, 0.0, 0.0),
                4.0,
                Vec3::ZERO,
                &shape
            )
            .is_none(),
            "命中点超出 max_t 应视为未命中"
        );
        let hit = ray_vs_shape(
            Vec3::new(5.0, 0.3, 0.0),
            Vec3::new(-1.0, 0.0, 0.0),
            5.0,
            Vec3::ZERO,
            &shape,
        )
        .expect("max_t 覆盖命中点应命中");
        assert_eq!(hit.t, 4.5);
        assert_eq!(hit.point, Vec3::new(0.5, 0.3, 0.0));
    }

    /// 胶囊经完整 step_physics 管线（网格 broadphase 的胶囊 AABB + capsule
    /// 检测 + 求解）落地静止：中心稳定在 half_height（穿透 < 1e-3）、不抖。
    #[test]
    fn capsule_body_settles_on_floor_through_pipeline() {
        const FINE: f32 = 1.0 / 120.0;
        let mut world = make_world();
        spawn_floor(&mut world);
        let capsule_entity = world.spawn((
            Transform::from_xyz(0.0, 5.0, 0.0),
            Collider::dynamic_capsule(0.5, 1.5, 0.0),
            RigidBody::default(),
        ));
        let steps = (3.0 / FINE) as usize;
        for i in 0..steps {
            step(&world, FINE);
            if i >= steps - 10 {
                let y = ball_position(&world, capsule_entity).y;
                assert!(
                    (y - 1.5).abs() < 1e-3,
                    "静止穿透应 < 1e-3（中心 y = half_height 1.5），实际 {y}"
                );
            }
        }
        assert!(
            ball_velocity(&world, capsule_entity).length() < 1e-4,
            "静止后速度应 ≈ 0"
        );
    }

    // ---- 接触事件与触发器：CollisionStarted / CollisionEnded / sensor ----

    /// 事件测试台：Time + 接触事件资源 + ContactTracker 全注册
    /// （PhysicsPlugin 的注册等价物）。
    fn make_event_world() -> World {
        let mut world = make_world();
        world.register_event::<CollisionStarted>();
        world.register_event::<CollisionEnded>();
        world.insert_resource(ContactTracker::default());
        world
    }

    /// 推进一帧并取走本帧事件（drain 消费契约：每帧恰好一次）。
    fn step_collect(world: &mut World, dt: f32) -> (Vec<CollisionStarted>, Vec<CollisionEnded>) {
        world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(dt as f64));
        step_physics(world);
        let started = world.events_mut::<CollisionStarted>().drain().collect();
        let ended = world.events_mut::<CollisionEnded>().drain().collect();
        (started, ended)
    }

    /// begin/end 严格成对（弹跳三段时序）：首次触地恰 1 个 Started →
    /// 弹起离地恰 1 个 Ended → 再次落地又恰 1 个 Started；接触持续期
    /// 与滞空期不重复报事件。地面用物理静态盒（sensor 不反弹，弹起
    /// 离地需要真实求解；sensor 配对的同款成对性由 sensor 穿越测试覆盖）。
    #[test]
    fn collision_events_pair_across_bounce_phases() {
        const FINE: f32 = 1.0 / 240.0;
        let mut world = make_event_world();
        let floor = spawn_floor(&mut world);
        let ball = spawn_ball(
            &mut world,
            Vec3::new(0.0, 2.5, 0.0),
            Vec3::ZERO,
            0.5,
            0.8,
            0.0,
        );

        let mut first_started: Option<usize> = None;
        let mut first_ended: Option<usize> = None;
        let mut second_started: Option<usize> = None;
        // 预算：2.5 m 落地 ~155 帧、回弹滞空 ~140 帧（240Hz）——600 帧
        // 覆盖三段时序绰绰有余（断言 expect 兜底，不达标即失败）
        for frame in 0..600 {
            let (started, ended) = step_collect(&mut world, FINE);
            if second_started.is_some() {
                continue; // 后续弹跳的同款循环不再关心
            }
            if first_started.is_none() {
                assert!(ended.is_empty(), "首次接触前不应有 ended");
                if !started.is_empty() {
                    assert_eq!(started.len(), 1, "首次接触帧应恰有 1 个 Started");
                    let event = started[0];
                    assert_eq!(event.a, floor, "a = 索引较小的地面（先 spawn）");
                    assert_eq!(event.b, ball, "b = 索引较大的球");
                    assert!(
                        (event.normal - Vec3::Y).length() < 1e-5,
                        "法线应从地面指向球（+Y），实际 {:?}",
                        event.normal
                    );
                    assert!(event.penetration > 0.0, "穿透深度应为正");
                    first_started = Some(frame);
                }
            } else if first_ended.is_none() {
                assert!(started.is_empty(), "接触持续期不应重复 Started");
                if !ended.is_empty() {
                    assert_eq!(ended.len(), 1, "离地帧应恰有 1 个 Ended");
                    assert_eq!(ended[0].a, floor);
                    assert_eq!(ended[0].b, ball);
                    first_ended = Some(frame);
                }
            } else {
                assert!(ended.is_empty(), "再次落地前不应有 ended");
                if !started.is_empty() {
                    assert_eq!(started.len(), 1, "再次落地应恰有 1 个 Started");
                    second_started = Some(frame);
                }
            }
        }
        let (s1, e1, s2) = (
            first_started.expect("应发生首次触地"),
            first_ended.expect("应弹起离地"),
            second_started.expect("应再次落地"),
        );
        assert!(s1 < e1 && e1 < s2, "时序应为 Started → Ended → Started");
    }

    /// 销毁成对性：接触中的球 despawn → 下一帧恰 1 个 Ended（携带已死
    /// 实体 id），再下一帧无新事件——begin/end 严格成对、不重复。
    #[test]
    fn despawn_mid_contact_emits_ended_with_dead_entity() {
        const FINE: f32 = 1.0 / 120.0;
        let mut world = make_event_world();
        let floor = spawn_floor(&mut world);
        let ball = spawn_ball(
            &mut world,
            Vec3::new(0.0, 3.0, 0.0),
            Vec3::ZERO,
            0.5,
            0.0,
            0.0,
        );
        // 2 秒充分沉降：落地帧的 Started 已逐帧取走
        for _ in 0..240 {
            step_collect(&mut world, FINE);
        }
        let (started, ended) = step_collect(&mut world, FINE);
        assert!(
            started.is_empty() && ended.is_empty(),
            "沉降后的持续接触不应有事件"
        );

        // 接触中销毁球：消失配对转 ended（id 已死但保留，世代安全）
        assert!(world.despawn(ball), "球应仍在场");
        let (started, ended) = step_collect(&mut world, FINE);
        assert!(started.is_empty());
        assert_eq!(ended.len(), 1, "销毁应把消失配对转 ended");
        assert_eq!(ended[0].a, floor);
        assert_eq!(ended[0].b, ball, "ended 应携带已销毁实体的 id");
        assert!(
            !world.contains(ball),
            "球确已销毁（ended 中的 id 不再匹配）"
        );

        // 差分只报一次：下一帧无新事件
        let (started, ended) = step_collect(&mut world, FINE);
        assert!(started.is_empty() && ended.is_empty(), "不应重复报 ended");
    }

    /// sensor 语义：事件报了、球穿过去了——Started → 逐帧单调下降
    /// （无位置推离）→ 速度持续向下（无反弹）→ 从下方穿出报 Ended。
    #[test]
    fn sensor_region_reports_events_without_solving() {
        const FINE: f32 = 1.0 / 240.0;
        let mut world = make_event_world();
        // sensor 盒：中心 y = 2、半高 0.5（占据 y ∈ [1.5, 2.5]）；球 r = 0.5
        let sensor = world.spawn((
            Transform::from_xyz(0.0, 2.0, 0.0),
            Collider::sensor_box(Vec3::new(1.0, 0.5, 1.0)),
        ));
        let ball = spawn_ball(
            &mut world,
            Vec3::new(0.0, 6.0, 0.0),
            Vec3::ZERO,
            0.5,
            0.9,
            0.0,
        );

        let mut started_frame: Option<usize> = None;
        let mut ended_frame: Option<usize> = None;
        let mut y_at_started = 0.0f32;
        let mut vy_at_started = 0.0f32;
        let mut prev_y = 6.0f32;
        for frame in 0..1200 {
            let (started, ended) = step_collect(&mut world, FINE);
            let y = ball_position(&world, ball).y;
            if !started.is_empty() {
                assert_eq!(started.len(), 1, "穿越 sensor 应恰报 1 次 Started");
                assert!(started_frame.is_none(), "全程应只报一次 Started");
                let event = started[0];
                assert_eq!(event.a, sensor, "a = 索引较小的 sensor（先 spawn）");
                assert_eq!(event.b, ball);
                assert!(
                    (event.normal - Vec3::Y).length() < 1e-5,
                    "法线应从 sensor（下方）指向球（上方），实际 {:?}",
                    event.normal
                );
                started_frame = Some(frame);
                y_at_started = y;
                vy_at_started = ball_velocity(&world, ball).y;
            }
            if !ended.is_empty() {
                assert_eq!(ended.len(), 1, "穿出 sensor 应恰报 1 次 Ended");
                assert!(ended_frame.is_none(), "全程应只报一次 Ended");
                assert_eq!(ended[0].a, sensor);
                assert_eq!(ended[0].b, ball);
                ended_frame = Some(frame);
            }
            if started_frame.is_some() && ended_frame.is_none() {
                // 穿越期：位置逐帧下降（无推离）、速度向下（无反弹支撑）
                assert!(
                    y <= prev_y + 1e-6,
                    "sensor 不应把球推离，y 应持续下降：{y} vs {prev_y}"
                );
                assert!(ball_velocity(&world, ball).y < 0.0, "穿越期不应反弹");
            }
            prev_y = y;
        }
        assert!(started_frame.is_some(), "球应进入 sensor 区域");
        assert!(ended_frame.is_some(), "球应从下方穿出 sensor 区域");
        assert!(
            (2.5..3.0).contains(&y_at_started),
            "首次接触帧球底应刚越过盒顶（y ≈ 3.0 减当帧位移），实际 {y_at_started}"
        );
        assert!(
            vy_at_started < -1.0,
            "接触帧球仍在自由下落（sensor 不提供支撑），实际 {vy_at_started}"
        );
        assert!(
            ball_position(&world, ball).y < 0.0,
            "球应完全穿过 sensor 区域（最终远在盒下方），实际 {}",
            ball_position(&world, ball).y
        );
    }

    /// 归一化与方向：检测原始方向与实体索引序相反（销毁 swap_remove 打乱
    /// 存储序）时，事件 a/b 按索引序归一化、法线随归一化翻转（a → b）。
    #[test]
    fn events_normalize_entity_order_and_flip_normal() {
        const FINE: f32 = 1.0 / 120.0;
        let mut world = make_event_world();
        // 失重漂移球（隔离重力，水平相向运动）
        let spawn_drifter = |world: &mut World, pos: Vec3, vel: Vec3| {
            world.spawn((
                Transform::from_translation(pos),
                Collider::dynamic_sphere(0.5, 0.0),
                RigidBody {
                    velocity: vel,
                    gravity_scale: 0.0,
                    ..RigidBody::default()
                },
            ))
        };
        // 占位者（index 0，spawn 后立即销毁）
        let filler = spawn_drifter(&mut world, Vec3::new(0.0, 100.0, 0.0), Vec3::ZERO);
        let left = spawn_drifter(&mut world, Vec3::new(-1.0, 100.0, 0.0), Vec3::X);
        let right = spawn_drifter(&mut world, Vec3::new(1.0, 100.0, 0.0), -Vec3::X);
        assert!(left.index() < right.index(), "spawn 序即索引序的前提");
        // 销毁占位者：swap_remove 把最后 spawn 的 right 挪到存储首位 →
        // 检测原始方向为 right → left，与索引序（left < right）相反
        assert!(world.despawn(filler));

        let mut event = None;
        for _ in 0..240 {
            let (started, _ended) = step_collect(&mut world, FINE);
            if let Some(first) = started.first().copied() {
                event = Some(first);
                break;
            }
        }
        let event = event.expect("相向运动的两球应接触");
        assert_eq!(event.a, left, "事件 a 应为索引较小者（归一化）");
        assert_eq!(event.b, right, "事件 b 应为索引较大者（归一化）");
        assert!(
            (event.normal - Vec3::X).length() < 1e-5,
            "归一化后法线应从 a（左）指向 b（右）——检测原始方向（右 → 左，-X）已被翻转，实际 {:?}",
            event.normal
        );
    }

    /// 确定性：同场景跑两遍（物理地面 + sensor 盒 + 两颗异质弹球），
    /// 逐帧事件序列（内容与顺序）完全相等。
    #[test]
    fn contact_events_are_deterministic_across_runs() {
        fn build() -> World {
            let mut world = make_event_world();
            spawn_floor(&mut world);
            world.spawn((
                Transform::from_xyz(4.0, 1.0, 0.0),
                Collider::sensor_box(Vec3::new(1.0, 1.0, 1.0)),
            ));
            spawn_ball(
                &mut world,
                Vec3::new(0.0, 5.0, 0.0),
                Vec3::new(0.5, 0.0, 0.0),
                0.5,
                0.7,
                0.0,
            );
            spawn_ball(
                &mut world,
                Vec3::new(4.0, 8.0, 0.0),
                Vec3::ZERO,
                0.4,
                0.2,
                0.1,
            );
            world
        }
        let mut world_a = build();
        let mut world_b = build();
        let mut log_a: Vec<(Vec<CollisionStarted>, Vec<CollisionEnded>)> = Vec::new();
        let mut log_b: Vec<(Vec<CollisionStarted>, Vec<CollisionEnded>)> = Vec::new();
        for _ in 0..240 {
            let (sa, ea) = step_collect(&mut world_a, DT);
            let (sb, eb) = step_collect(&mut world_b, DT);
            log_a.push((sa, ea));
            log_b.push((sb, eb));
        }
        assert!(
            log_a.iter().any(|(s, e)| !s.is_empty() || !e.is_empty()),
            "场景应产出事件（测试非空转的佐证）"
        );
        assert_eq!(log_a, log_b, "同输入两次模拟的事件序列应逐项相等（含顺序）");
    }

    /// 静-静对不报事件：两静态盒刻意重叠（求解与事件同分支跳过——
    /// 静-静配对永不变化，begin/end 无语义）。
    #[test]
    fn static_static_pairs_never_report_events() {
        let mut world = make_event_world();
        world.spawn((
            Transform::from_xyz(0.0, 0.0, 0.0),
            Collider::static_box(Vec3::ONE),
        ));
        world.spawn((
            Transform::from_xyz(0.5, 0.5, 0.0),
            Collider::static_box(Vec3::ONE),
        ));
        for _ in 0..10 {
            let (started, ended) = step_collect(&mut world, DT);
            assert!(
                started.is_empty() && ended.is_empty(),
                "静-静对永不变化，不应有事件"
            );
        }
    }

    // ---- 碰撞过滤掩码：groups / mask（见模块文档「碰撞过滤掩码」章节）----

    /// 全部构造辅助产出全 1 的 groups / mask（默认 = 与一切碰撞，引入
    /// 过滤前的行为逐位不变——既有测试全量守护该等价性，这里是字段级
    /// 直接断言）。
    #[test]
    fn collider_helpers_default_to_all_one_groups_and_mask() {
        let helpers = [
            Collider::static_box(Vec3::ONE),
            Collider::static_sphere(0.5),
            Collider::dynamic_sphere(0.5, 0.3),
            Collider::dynamic_box(Vec3::ONE, 0.3),
            Collider::static_capsule(0.4, 0.9),
            Collider::dynamic_capsule(0.4, 0.9, 0.3),
            Collider::sensor_sphere(0.5),
            Collider::sensor_box(Vec3::ONE),
        ];
        for collider in &helpers {
            assert_eq!(collider.groups, u32::MAX, "groups 默认应全 1");
            assert_eq!(collider.mask, u32::MAX, "mask 默认应全 1");
        }
    }

    /// 失重对撞体的过滤辅助（restitution 0.9：若未被过滤必然剧烈反弹）。
    fn spawn_grouped_drifter(
        world: &mut World,
        pos: Vec3,
        vel: Vec3,
        groups: u32,
        mask: u32,
    ) -> Entity {
        world.spawn((
            Transform::from_translation(pos),
            Collider {
                shape: ColliderShape::Sphere { radius: 0.5 },
                is_static: false,
                restitution: 0.9,
                is_sensor: false,
                groups,
                mask,
            },
            RigidBody {
                velocity: vel,
                gravity_scale: 0.0,
                ..RigidBody::default()
            },
        ))
    }

    /// 组 A vs 组 B 互不匹配（各自 mask 只含自己的组）：相互穿过——
    /// 位置 = 自由飞航迹（bit 级，无任何推离）、速度 bit 级不变（无反弹
    /// 无摩擦）、全程零事件；同几何的默认掩码对照组则反弹 + 报事件
    /// （证明行为差异来自过滤本身，测试非空转）。
    #[test]
    fn group_filter_mismatched_pair_passes_through_without_events() {
        const GROUP_A: u32 = 0b01;
        const GROUP_B: u32 = 0b10;
        let start_a = Vec3::new(-2.0, 100.0, 0.0);
        let start_b = Vec3::new(2.0, 100.0, 0.0);
        let vel_a = Vec3::X;
        let vel_b = -Vec3::X;

        // ---- 被过滤场景 ----
        let mut world = make_event_world();
        let a = spawn_grouped_drifter(&mut world, start_a, vel_a, GROUP_A, GROUP_A);
        let b = spawn_grouped_drifter(&mut world, start_b, vel_b, GROUP_B, GROUP_B);

        let mut events_seen = 0usize;
        for _ in 0..120 {
            let (started, ended) = step_collect(&mut world, DT);
            events_seen += started.len() + ended.len();
        }
        assert_eq!(events_seen, 0, "被过滤配对不应产出任何接触事件");

        // 速度 bit 级不变（求解从未触碰）
        assert_eq!(ball_velocity(&world, a), vel_a, "过滤对的 a 速度不应被改写");
        assert_eq!(ball_velocity(&world, b), vel_b, "过滤对的 b 速度不应被改写");
        // 位置 = 自由飞航迹：同款逐帧累加复算，bit 级一致（无推离）
        let mut expect_a = start_a;
        let mut expect_b = start_b;
        for _ in 0..120 {
            expect_a += vel_a * DT;
            expect_b += vel_b * DT;
        }
        assert_eq!(ball_position(&world, a), expect_a, "a 应严格沿自由飞航迹");
        assert_eq!(ball_position(&world, b), expect_b, "b 应严格沿自由飞航迹");
        // 非空转佐证：两球末态深度重叠（对心穿越已发生）却无任何求解
        let gap = ball_position(&world, a).distance(ball_position(&world, b));
        assert!(gap < 1.0, "相向 2 s 应已对心穿越重叠，实际间距 {gap}");

        // ---- 对照组：同几何 + 默认全 1 掩码 → 反弹 + 报事件 ----
        let mut control = make_event_world();
        let ca = spawn_grouped_drifter(&mut control, start_a, vel_a, u32::MAX, u32::MAX);
        let cb = spawn_grouped_drifter(&mut control, start_b, vel_b, u32::MAX, u32::MAX);
        let mut control_events = 0usize;
        for _ in 0..120 {
            let (started, ended) = step_collect(&mut control, DT);
            control_events += started.len() + ended.len();
        }
        assert!(control_events > 0, "默认掩码的对照组应报接触事件");
        assert!(
            ball_velocity(&control, ca).x < 0.9,
            "对照组 a 应发生反弹（速度被改写），实际 {:?}",
            ball_velocity(&control, ca)
        );
        assert!(ball_velocity(&control, cb).x > -0.9, "对照组 b 应发生反弹");
        assert!(
            ball_position(&control, ca).distance(ball_position(&control, cb)) >= 1.0,
            "对照组两球应被弹开而非重叠"
        );
    }

    /// 单向 mask（a 的白名单含 b 的组、b 的不含 a 的组）同样不碰——
    /// 配对判定对称：两个方向都认可才碰撞；镜像配置（b 认 a、a 不认 b）
    /// 结果一致。
    #[test]
    fn group_filter_one_sided_mask_is_still_filtered() {
        const GROUP_A: u32 = 0b01;
        const GROUP_B: u32 = 0b10;
        let start_a = Vec3::new(-2.0, 100.0, 0.0);
        let start_b = Vec3::new(2.0, 100.0, 0.0);
        let vel_a = Vec3::X;
        let vel_b = -Vec3::X;

        // 正向单向：a（组 A）的白名单含组 B，b（组 B）只认组 B
        let mut world = make_event_world();
        let a = spawn_grouped_drifter(&mut world, start_a, vel_a, GROUP_A, GROUP_A | GROUP_B);
        let b = spawn_grouped_drifter(&mut world, start_b, vel_b, GROUP_B, GROUP_B);
        let mut events_seen = 0usize;
        for _ in 0..120 {
            let (started, ended) = step_collect(&mut world, DT);
            events_seen += started.len() + ended.len();
        }
        assert_eq!(events_seen, 0, "单向认可的配对同样应被过滤");
        assert_eq!(ball_velocity(&world, a), vel_a, "a 速度不应被改写");
        assert_eq!(ball_velocity(&world, b), vel_b, "b 速度不应被改写");
        let gap = ball_position(&world, a).distance(ball_position(&world, b));
        assert!(gap < 1.0, "两球应已穿越重叠（无求解），实际间距 {gap}");

        // 镜像单向：b 认 a、a 不认 b → 同样过滤（对称语义）
        let mut mirrored = make_event_world();
        let ma = spawn_grouped_drifter(&mut mirrored, start_a, vel_a, GROUP_A, GROUP_A);
        let mb = spawn_grouped_drifter(&mut mirrored, start_b, vel_b, GROUP_B, GROUP_A | GROUP_B);
        let mut mirrored_events = 0usize;
        for _ in 0..120 {
            let (started, ended) = step_collect(&mut mirrored, DT);
            mirrored_events += started.len() + ended.len();
        }
        assert_eq!(mirrored_events, 0, "镜像单向配置同样应被过滤");
        assert_eq!(ball_velocity(&mirrored, ma), vel_a);
        assert_eq!(ball_velocity(&mirrored, mb), vel_b);
        let mirrored_gap = ball_position(&mirrored, ma).distance(ball_position(&mirrored, mb));
        assert!(
            (mirrored_gap - gap).abs() < 1e-6,
            "镜像配置的穿越轨迹应与正向逐位一致：{mirrored_gap} vs {gap}"
        );
    }

    /// 资源缺席不 panic：完全不注册（照 Time 缺席语义静默跳过）；以及只
    /// 有 ContactTracker 无事件资源——基线照常逐帧推进，后注册事件资源
    /// 不收到积压差分，物理行为不受影响。
    #[test]
    fn missing_event_resources_does_not_panic() {
        // 情形一：什么都不注册
        let mut world = make_world();
        spawn_floor(&mut world);
        spawn_ball(
            &mut world,
            Vec3::new(0.0, 2.0, 0.0),
            Vec3::ZERO,
            0.5,
            0.0,
            0.0,
        );
        step(&world, DT); // 不 panic 即通过

        // 情形二：只有 ContactTracker、没有事件资源
        let mut world = make_world();
        world.insert_resource(ContactTracker::default());
        spawn_floor(&mut world);
        let ball = spawn_ball(
            &mut world,
            Vec3::new(0.0, 2.0, 0.0),
            Vec3::ZERO,
            0.5,
            0.0,
            0.0,
        );
        for _ in 0..60 {
            step(&world, DT); // 落地 + 持续接触，基线静默推进
        }
        assert!(
            ball_position(&world, ball).y < 0.6,
            "球应已落地（物理行为不受事件资源缺席影响）"
        );
        // 后补注册：不收到积压的陈旧差分
        world.register_event::<CollisionStarted>();
        world.register_event::<CollisionEnded>();
        let (started, ended) = step_collect(&mut world, DT);
        assert!(
            started.is_empty() && ended.is_empty(),
            "补注册不应收到积压差分（持续接触配对无变化）"
        );
    }

    // ---- 约束与关节 v1：DistanceJoint / TargetJoint（见模块文档章节）----

    /// 失重动体球（隔离重力，纯看约束语义；半径 0.5、无弹性无摩擦）。
    fn spawn_weightless(world: &mut World, pos: Vec3, vel: Vec3) -> Entity {
        world.spawn((
            Transform::from_translation(pos),
            Collider::dynamic_sphere(0.5, 0.0),
            RigidBody {
                velocity: vel,
                gravity_scale: 0.0,
                ..RigidBody::default()
            },
        ))
    }

    /// 纯函数语义：距离关节的均分 / 静态端全吃 / 重合退化 / 轴向阻尼只动
    /// 轴向分量；目标点关节的球壳投影 / 到达不修 / 轴向按 (1−stiffness)
    /// 缩放、切向按 (1−damping) 缩放。
    #[test]
    fn joint_solver_pure_functions_semantics() {
        // 双动体：err = 1 沿轴各修一半
        let both = solve_distance_joint(
            Vec3::ZERO,
            Some(Vec3::ZERO),
            Vec3::new(3.0, 0.0, 0.0),
            Some(Vec3::ZERO),
            DistanceJoint {
                target: Entity::from_bits(u64::MAX),
                rest_length: 2.0,
                stiffness: 1.0,
                damping: 0.0,
            },
            false,
        );
        assert_eq!(both.delta_pos_a, Vec3::new(0.5, 0.0, 0.0), "a 朝 b 修一半");
        assert_eq!(both.delta_pos_b, Vec3::new(-0.5, 0.0, 0.0), "b 朝 a 修一半");

        // 静态端（速度缺席）：share 0 不动，动端全吃（拉回 rest_length）
        let anchored = solve_distance_joint(
            Vec3::ZERO,
            Some(Vec3::ZERO),
            Vec3::new(3.0, 0.0, 0.0),
            None,
            DistanceJoint {
                target: Entity::from_bits(u64::MAX),
                rest_length: 2.0,
                stiffness: 1.0,
                damping: 0.0,
            },
            false,
        );
        assert_eq!(anchored.delta_pos_a, Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(anchored.delta_pos_b, Vec3::ZERO, "静态端不移动");

        // 速度阻尼：只衰减轴向相对速度（均分写回），切向分量保留
        let damped = solve_distance_joint(
            Vec3::ZERO,
            Some(Vec3::new(3.0, 4.0, 0.0)),
            Vec3::new(3.0, 0.0, 0.0),
            Some(Vec3::new(1.0, 4.0, 0.0)),
            DistanceJoint {
                target: Entity::from_bits(u64::MAX),
                rest_length: 2.0,
                stiffness: 1.0,
                damping: 0.5,
            },
            true,
        );
        // rel = (2, 0, 0) 沿 +X 轴 → ×0.5 → Δv_a = (−0.5, 0, 0)，切向 4 已在
        // rel 中抵消（两端同速），不应产生任何修正
        assert_eq!(damped.delta_vel_a, Vec3::new(-0.5, 0.0, 0.0));
        assert_eq!(damped.delta_vel_b, Vec3::new(0.5, 0.0, 0.0));

        // apply_velocity = false：位置照常、速度零修正（松弛第 2 轮语义）
        let pos_only = solve_distance_joint(
            Vec3::ZERO,
            Some(Vec3::new(3.0, 4.0, 0.0)),
            Vec3::new(3.0, 0.0, 0.0),
            None,
            DistanceJoint {
                target: Entity::from_bits(u64::MAX),
                rest_length: 2.0,
                stiffness: 1.0,
                damping: 0.5,
            },
            false,
        );
        assert_eq!(pos_only.delta_vel_a, Vec3::ZERO);
        assert_eq!(pos_only.delta_pos_a, Vec3::new(1.0, 0.0, 0.0));

        // 两端重合：约束轴退化，确定性全零跳过
        let degenerate = solve_distance_joint(
            Vec3::ONE,
            Some(Vec3::X),
            Vec3::ONE,
            Some(Vec3::ZERO),
            DistanceJoint {
                target: Entity::from_bits(u64::MAX),
                rest_length: 2.0,
                stiffness: 1.0,
                damping: 0.5,
            },
            true,
        );
        assert_eq!(degenerate, zero_response());

        // 目标点关节：投影到球壳（rest 0.5），b 通道恒零
        let pulled = solve_target_joint(
            Vec3::new(3.0, 0.0, 0.0),
            Some(Vec3::new(-4.0, 2.0, 0.0)),
            TargetJoint {
                target: Vec3::ZERO,
                rest_distance: 0.5,
                stiffness: 1.0,
                damping: 0.0,
            },
            true,
        );
        assert_eq!(pulled.delta_pos_a, Vec3::new(-2.5, 0.0, 0.0), "投影到球壳");
        assert_eq!(pulled.delta_pos_b, Vec3::ZERO);
        // 轴向（朝目标）分量 ×(1 − 1) = 0 清零，切向 (0, 2, 0) 保留
        assert_eq!(pulled.delta_vel_a, Vec3::new(4.0, 0.0, 0.0));

        // 已在球壳内：到达语义，不再修正
        let arrived = solve_target_joint(
            Vec3::new(0.3, 0.0, 0.0),
            Some(Vec3::ZERO),
            TargetJoint {
                target: Vec3::ZERO,
                rest_distance: 0.5,
                stiffness: 1.0,
                damping: 0.0,
            },
            true,
        );
        assert_eq!(arrived.delta_pos_a, Vec3::ZERO);

        // 恰在目标点上：轴退化，全零
        let at_target = solve_target_joint(
            Vec3::ZERO,
            Some(Vec3::X),
            TargetJoint {
                target: Vec3::ZERO,
                rest_distance: 0.0,
                stiffness: 1.0,
                damping: 0.5,
            },
            true,
        );
        assert_eq!(at_target, zero_response());
    }

    /// 零响应便捷构造（纯函数测试用）。
    fn zero_response() -> ContactResponse {
        ContactResponse {
            delta_pos_a: Vec3::ZERO,
            delta_pos_b: Vec3::ZERO,
            delta_vel_a: Vec3::ZERO,
            delta_vel_b: Vec3::ZERO,
        }
    }

    /// 硬距离约束（stiffness = 1）：拉开 3 m 的双动球对一帧内被拉回
    /// rest = 2，两端各修正一半（中点保持不动），此后稳定。
    #[test]
    fn hard_distance_joint_pulls_pair_to_rest_length() {
        let mut world = make_world();
        let b = spawn_weightless(&mut world, Vec3::new(1.5, 20.0, 0.0), Vec3::ZERO);
        let a = world.spawn((
            Transform::from_translation(Vec3::new(-1.5, 20.0, 0.0)),
            Collider::dynamic_sphere(0.5, 0.0),
            RigidBody {
                gravity_scale: 0.0,
                ..RigidBody::default()
            },
            DistanceJoint {
                target: b,
                rest_length: 2.0,
                stiffness: 1.0,
                damping: 0.0,
            },
        ));

        step(&world, DT);
        let pa = ball_position(&world, a);
        let pb = ball_position(&world, b);
        // 全程无速度（重力隔离、初速为零）：f32 运算 bit 级精确
        assert_eq!(pa, Vec3::new(-1.0, 20.0, 0.0), "a 朝 b 全额修正一半");
        assert_eq!(pb, Vec3::new(1.0, 20.0, 0.0), "b 朝 a 全额修正一半");
        assert_eq!(
            (pa + pb) * 0.5,
            Vec3::new(0.0, 20.0, 0.0),
            "等质量均分 ⇒ 质心中点不动"
        );

        for _ in 0..10 {
            step(&world, DT);
        }
        assert!(
            (pa.distance(ball_position(&world, a)) < 1e-6)
                && (pb.distance(ball_position(&world, b)) < 1e-6),
            "到达 rest 后应稳定不动"
        );
        assert!(
            (ball_position(&world, a).distance(ball_position(&world, b)) - 2.0).abs() < 1e-3,
            "数步后距离应收敛到 rest = 2"
        );
    }

    /// 静态端距离约束：锚点（is_static 静态体）不动（bit 级），动球全额
    /// 修正、悬在 rest 处。
    #[test]
    fn distance_joint_static_end_anchors_and_stays_bit_identical() {
        let mut world = make_world();
        let anchor_pos = Vec3::new(5.0, 5.0, 0.0);
        let anchor = world.spawn((
            Transform::from_translation(anchor_pos),
            Collider::static_sphere(0.5),
        ));
        let ball = spawn_weightless(&mut world, Vec3::new(2.0, 5.0, 0.0), Vec3::ZERO);
        world.insert(
            ball,
            DistanceJoint {
                target: anchor,
                rest_length: 2.0,
                stiffness: 1.0,
                damping: 0.0,
            },
        );

        for _ in 0..5 {
            step(&world, DT);
        }
        assert_eq!(
            ball_position(&world, ball),
            Vec3::new(3.0, 5.0, 0.0),
            "静态端 share = 0，动球沿轴全额修正 1 m 到 rest 处"
        );
        assert_eq!(
            ball_position(&world, anchor),
            anchor_pos,
            "静态锚点位置应 bit 级不变"
        );
        assert!(
            (ball_position(&world, ball).distance(anchor_pos) - 2.0).abs() < 1e-6,
            "球应悬在锚点 rest = 2 处"
        );
    }

    /// 弹簧（stiffness = 0.1）：多帧渐进收敛到 rest（对比硬约束一帧到位）、
    /// 过程单调无过冲（每轮部分修正 × 多轮 ≈ 收敛的迭代性质）。
    #[test]
    fn spring_stiffness_converges_monotonically_without_overshoot() {
        let mut world = make_world();
        let b = spawn_weightless(&mut world, Vec3::new(1.5, 20.0, 0.0), Vec3::ZERO);
        let a = world.spawn((
            Transform::from_translation(Vec3::new(-1.5, 20.0, 0.0)),
            Collider::dynamic_sphere(0.5, 0.0),
            RigidBody {
                gravity_scale: 0.0,
                ..RigidBody::default()
            },
            DistanceJoint {
                target: b,
                rest_length: 2.0,
                stiffness: 0.1,
                damping: 0.0,
            },
        ));

        let mut previous = 3.0f32; // 初始间距
        for frame in 0..90 {
            step(&world, DT);
            let dist = ball_position(&world, a).distance(ball_position(&world, b));
            assert!(
                dist <= previous + 1e-6,
                "弹簧修正应朝 rest 单调（帧 {frame}：{dist} > {previous}）"
            );
            assert!(
                dist >= 2.0 - 1e-5,
                "无过冲：间距不应越过 rest 往回弹（帧 {frame}：{dist}）"
            );
            if frame == 0 {
                assert!(
                    (2.5..2.9).contains(&dist),
                    "弹簧第一帧只部分修正（≈ 0.81 收敛因子），应仍明显大于 rest，实际 {dist}"
                );
            }
            previous = dist;
        }
        assert!(
            (previous - 2.0).abs() < 1e-2,
            "90 帧后应收敛到 rest 附近，实际间距 {previous}"
        );
    }

    /// 距离关节 damping：拉开方向运动的球对的轴向相对速度逐帧精确减半
    ///（×(1 − damping)，均分写回），单调衰减。
    #[test]
    fn joint_damping_decays_axial_relative_velocity() {
        let mut world = make_world();
        let b = spawn_weightless(
            &mut world,
            Vec3::new(1.5, 20.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
        );
        let a = world.spawn((
            Transform::from_translation(Vec3::new(-1.5, 20.0, 0.0)),
            Collider::dynamic_sphere(0.5, 0.0),
            RigidBody {
                velocity: Vec3::new(-1.0, 0.0, 0.0),
                gravity_scale: 0.0,
                ..RigidBody::default()
            },
            DistanceJoint {
                target: b,
                rest_length: 2.0,
                stiffness: 1.0,
                damping: 0.5,
            },
        ));

        let mut previous = 2.0f32; // 初始轴向相对速度 |v_a − v_b| = 2
        for frame in 0..20 {
            step(&world, DT);
            let rel = (ball_velocity(&world, a) - ball_velocity(&world, b)).x;
            assert!(
                rel < 0.0 && -rel <= previous + 1e-6,
                "轴向相对速度应单调衰减（帧 {frame}）"
            );
            assert!(
                (-rel - previous * 0.5).abs() < 1e-5,
                "damping = 0.5 ⇒ 轴向相对速度每帧精确减半（帧 {frame}：{} vs {}）",
                -rel,
                previous * 0.5
            );
            previous = -rel;
        }
    }

    /// 目标点关节：偏移球被拉到 rest_distance 球壳上、速度归零；球壳内
    /// 到达语义不修正（位置 bit 级不动）；rest_distance = 0 精确到达。
    #[test]
    fn target_joint_pulls_to_shell_and_reaches_rest_distance() {
        let mut world = make_world();
        // 三个子情形各自独立的目标点（垂直间隔 60 m），避免球体互撞干扰
        let target_a = Vec3::new(0.0, 20.0, 0.0);
        // 偏移 3 m 且带背离初速：一帧内被拉回球壳、轴向速度清零
        let seeker = spawn_weightless(
            &mut world,
            Vec3::new(3.0, 20.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
        );
        world.insert(
            seeker,
            TargetJoint {
                target: target_a,
                rest_distance: 0.5,
                stiffness: 1.0,
                damping: 0.0,
            },
        );
        // 球壳内（0.3 < rest 0.5）：到达语义，应纹丝不动
        let target_b = Vec3::new(0.0, 80.0, 0.0);
        let insider = spawn_weightless(&mut world, Vec3::new(0.3, 80.0, 0.0), Vec3::ZERO);
        world.insert(
            insider,
            TargetJoint {
                target: target_b,
                rest_distance: 0.5,
                stiffness: 1.0,
                damping: 0.0,
            },
        );
        // rest_distance = 0：精确到达目标点
        let target_c = Vec3::new(0.0, 140.0, 0.0);
        let exact = spawn_weightless(&mut world, Vec3::new(3.0, 140.0, 0.0), Vec3::ZERO);
        world.insert(
            exact,
            TargetJoint {
                target: target_c,
                rest_distance: 0.0,
                stiffness: 1.0,
                damping: 0.0,
            },
        );

        for _ in 0..60 {
            step(&world, DT);
        }
        let d = ball_position(&world, seeker).distance(target_a);
        assert!(
            (d - 0.5).abs() < 1e-3,
            "应停在 rest_distance = 0.5 球壳上，实际距目标 {d}"
        );
        assert!(
            ball_velocity(&world, seeker).length() < 1e-6,
            "到达后速度应归零（轴向 ×(1 − stiffness) 清零）"
        );
        assert_eq!(
            ball_position(&world, insider),
            Vec3::new(0.3, 80.0, 0.0),
            "球壳内到达语义：位置应 bit 级不动"
        );
        assert!(
            ball_position(&world, exact).distance(target_c) < 1e-3,
            "rest_distance = 0 应精确到达目标点"
        );
    }

    /// 无效目标：despawn 的 target 实体 → 约束整体跳过（不 panic），挂载
    /// 体自由落体与无关节对照球 bit 级一致。
    #[test]
    fn joint_with_despawned_target_is_skipped() {
        let mut world = make_world();
        let dead = world.spawn((Transform::from_xyz(100.0, 100.0, 0.0),));
        assert!(world.despawn(dead), "占位实体应成功销毁");

        let linked = spawn_ball(
            &mut world,
            Vec3::new(-5.0, 20.0, 0.0),
            Vec3::ZERO,
            0.5,
            0.0,
            0.0,
        );
        world.insert(
            linked,
            DistanceJoint {
                target: dead,
                rest_length: 2.0,
                stiffness: 1.0,
                damping: 0.0,
            },
        );
        let linked_control = spawn_ball(
            &mut world,
            Vec3::new(5.0, 20.0, 0.0),
            Vec3::ZERO,
            0.5,
            0.0,
            0.0,
        );

        for _ in 0..60 {
            step(&world, DT);
        }
        assert!(
            ball_position(&world, linked).y < 20.0,
            "无效目标的挂载体应照常自由落体"
        );
        assert_eq!(
            ball_position(&world, linked).y,
            ball_position(&world, linked_control).y,
            "距离关节跳过 ⇒ 与无关节对照球 bit 级一致"
        );
    }

    /// 悬挂验证场景（语义锚）：静态锚点实体（无 Collider——关节不要求
    /// 碰撞）+ 动态吊灯球 + 硬距离关节（rest = 2）：重力下被拉住悬在锚点
    /// 下方 2 m 处稳定，速度被阻尼压到近零、不发散，锚点 bit 级不动。
    #[test]
    fn chandelier_hangs_stable_under_gravity() {
        let mut world = make_world();
        let anchor_pos = Vec3::new(0.0, 6.0, 0.0);
        let anchor = world.spawn((Transform::from_translation(anchor_pos),));
        let lamp = world.spawn((
            Transform::from_xyz(0.0, 4.5, 0.0),
            Collider::dynamic_sphere(0.3, 0.0),
            RigidBody {
                linear_damping: 0.2,
                ..RigidBody::default()
            },
            DistanceJoint {
                target: anchor,
                rest_length: 2.0,
                stiffness: 1.0,
                damping: 0.9,
            },
        ));

        let mut tail_velocity: Vec<f32> = Vec::new();
        for frame in 0..300 {
            step(&world, DT);
            let dist = ball_position(&world, lamp).distance(anchor_pos);
            assert!(
                (dist - 2.0).abs() < 1e-3,
                "吊灯应被拉住悬在 rest = 2 处（帧 {frame}：距锚点 {dist}）"
            );
            if frame >= 240 {
                tail_velocity.push(ball_velocity(&world, lamp).length());
            }
        }
        let max_tail = tail_velocity.iter().cloned().fold(f32::MIN, f32::max);
        assert!(
            max_tail < 0.05,
            "末段速度应被关节阻尼压到近零（平衡 ≈ g·dt·(1−d)/d ≈ 0.018），实际峰值 {max_tail}"
        );
        assert!(
            ball_position(&world, lamp).y < anchor_pos.y,
            "吊灯应悬在锚点下方"
        );
        assert_eq!(
            world
                .get::<Transform>(anchor)
                .expect("锚点应有 Transform")
                .position,
            anchor_pos,
            "无 Collider 的锚点实体位置应 bit 级不变"
        );
    }

    /// 确定性：吊灯 + 地面 + 多约束场景（距离硬约束 / 目标点约束 / 双动
    /// 刚杆对 / 接触与约束交互）跑两遍，逐帧位置 bit 级一致。
    #[test]
    fn joint_simulation_is_deterministic_across_runs() {
        fn build() -> (World, Vec<Entity>) {
            let mut world = make_world();
            spawn_floor(&mut world);
            // 吊灯：无 Collider 锚点 + 硬距离关节（重力下悬挂）
            let anchor = world.spawn((Transform::from_xyz(0.0, 6.0, 0.0),));
            let lamp = world.spawn((
                Transform::from_xyz(0.5, 4.5, 0.0),
                Collider::dynamic_sphere(0.3, 0.2),
                RigidBody {
                    linear_damping: 0.1,
                    ..RigidBody::default()
                },
                DistanceJoint {
                    target: anchor,
                    rest_length: 2.0,
                    stiffness: 1.0,
                    damping: 0.9,
                },
            ));
            // 目标点约束球：拉向 (3, 0.8, 0) 的球壳——球壳底恰触地面，
            // 接触推离与约束投影长期交互（管线顺序的确定性压力场景）
            let seeker = world.spawn((
                Transform::from_xyz(3.0, 5.0, 0.0),
                Collider::dynamic_sphere(0.3, 0.0),
                RigidBody::default(),
                TargetJoint {
                    target: Vec3::new(3.0, 0.8, 0.0),
                    rest_distance: 0.5,
                    stiffness: 1.0,
                    damping: 0.5,
                },
            ));
            // 双动刚杆对：先后落到地面，落地震荡由关节与接触共同求解
            let lower = spawn_ball(
                &mut world,
                Vec3::new(-3.0, 3.8, 0.0),
                Vec3::ZERO,
                0.5,
                0.05,
                0.4,
            );
            let upper = world.spawn((
                Transform::from_xyz(-3.0, 5.0, 0.0),
                Collider::dynamic_sphere(0.5, 0.05),
                RigidBody {
                    friction: 0.4,
                    ..RigidBody::default()
                },
                DistanceJoint {
                    target: lower,
                    rest_length: 1.2,
                    stiffness: 1.0,
                    damping: 0.3,
                },
            ));
            (world, vec![lamp, seeker, lower, upper])
        }

        let (world_a, entities_a) = build();
        let (world_b, entities_b) = build();
        let mut trace_a: Vec<Vec3> = Vec::new();
        let mut trace_b: Vec<Vec3> = Vec::new();
        for _ in 0..240 {
            step(&world_a, DT);
            step(&world_b, DT);
            for (a, b) in entities_a.iter().zip(&entities_b) {
                trace_a.push(ball_position(&world_a, *a));
                trace_b.push(ball_position(&world_b, *b));
            }
        }
        assert_eq!(trace_a, trace_b, "同输入两次模拟应逐采样 bit 级一致");

        // 场景非空转佐证：约束语义各自兑现（悬挂 / 球壳 / 刚杆间距）
        let (lamp, seeker, lower, upper) =
            (entities_a[0], entities_a[1], entities_a[2], entities_a[3]);
        assert!(
            (ball_position(&world_a, lamp).distance(Vec3::new(0.0, 6.0, 0.0)) - 2.0).abs() < 1e-2,
            "吊灯应悬在锚点 rest 处"
        );
        assert!(
            ball_position(&world_a, seeker).distance(Vec3::new(3.0, 0.8, 0.0)) <= 0.5 + 1e-2,
            "目标点约束球应停在球壳上"
        );
        assert!(
            (ball_position(&world_a, lower).distance(ball_position(&world_a, upper)) - 1.2).abs()
                < 1e-2,
            "刚杆对应保持 rest 间距"
        );
        assert!(
            ball_position(&world_a, lower).y > 0.0 && ball_position(&world_a, lower).y < 2.0,
            "刚杆对应落在地面上而非穿出"
        );
    }

    // ---- 运动学刚体（打磨 V：BodyType::Kinematic，全语义见 BodyType 文档）----

    /// kinematic 体跳过积分与写回：velocity / gravity_scale 对它无语义，
    /// 位移归游戏所有（每帧写 Transform 的游戏值不被物理覆写）。
    #[test]
    fn kinematic_body_skips_integration_and_writeback() {
        let mut world = make_world();
        let kinematic = world.spawn((
            Transform::from_xyz(3.0, 2.0, 1.0),
            Collider::dynamic_sphere(0.5, 0.0),
            RigidBody {
                velocity: Vec3::new(10.0, 0.0, 0.0),
                gravity_scale: 1.0,
                body_type: BodyType::Kinematic,
                ..RigidBody::default()
            },
        ));
        let bits = |t: &Transform| {
            (
                t.position.x.to_bits(),
                t.position.y.to_bits(),
                t.position.z.to_bits(),
            )
        };
        let before = bits(&world.get::<Transform>(kinematic).unwrap());
        for _ in 0..10 {
            step(&world, DT);
        }
        let after = bits(&world.get::<Transform>(kinematic).unwrap());
        assert_eq!(
            after, before,
            "运动学体的 Transform 应逐位不动（不积分、不被写回）"
        );
        assert_eq!(
            world.get::<RigidBody>(kinematic).unwrap().velocity,
            Vec3::new(10.0, 0.0, 0.0),
            "运动学体的 velocity 字段不应被写回"
        );
    }

    /// 动体撞运动学体按「静态方」路径被全额推离：kinematic 逐位不动、
    /// 动体沿法线恢复到恰好相切。
    #[test]
    fn dynamic_body_is_pushed_out_of_kinematic() {
        let mut world = make_world();
        let kinematic = world.spawn((
            Transform::from_xyz(1.0, 0.5, 0.0),
            Collider::dynamic_sphere(0.5, 0.0),
            RigidBody {
                body_type: BodyType::Kinematic,
                ..RigidBody::default()
            },
        ));
        // 动球叠进 kinematic 球（中心距 0.6 < 半径和 0.7，穿透 0.1；
        // 重力 0 隔离几何——解析解对照）
        let ball = world.spawn((
            Transform::from_xyz(1.6, 0.5, 0.0),
            Collider::dynamic_sphere(0.2, 0.0),
            RigidBody {
                gravity_scale: 0.0,
                ..RigidBody::default()
            },
        ));
        step(&world, DT);
        // kinematic 不被挪动
        assert_eq!(
            world.get::<Transform>(kinematic).unwrap().position,
            Vec3::new(1.0, 0.5, 0.0),
            "运动学体不被物理挪动"
        );
        // 动体沿 +X 全额分离到恰好相切（中心距 = 0.7）
        let ball_pos = world.get::<Transform>(ball).unwrap().position;
        let separation = (ball_pos - Vec3::new(1.0, 0.5, 0.0)).length();
        assert!(
            (separation - 0.7).abs() < 1e-4,
            "动体应被全额推离到相切（0.7），实际 {separation}"
        );
    }

    /// kinematic × 动体配对进接触集合：重叠帧报 Started，游戏把
    /// kinematic 挪开后报 Ended（begin/end 成对，随游戏驱动的位移变化）。
    #[test]
    fn kinematic_participates_in_contact_events() {
        let mut world = make_event_world();
        let kinematic = world.spawn((
            Transform::from_xyz(3.0, 0.5, 0.0),
            Collider::dynamic_sphere(0.5, 0.0),
            RigidBody {
                body_type: BodyType::Kinematic,
                ..RigidBody::default()
            },
        ));
        spawn_ball(
            &mut world,
            Vec3::new(2.9, 0.5, 0.0),
            Vec3::ZERO,
            0.5,
            0.0,
            0.0,
        );
        let (started, ended) = step_collect(&mut world, DT);
        assert_eq!(started.len(), 1, "重叠帧应报 CollisionStarted");
        assert_eq!(ended.len(), 0);
        // 游戏驱动：把 kinematic 挪远（写 Transform 即 kinematic 的位移方式）
        if let Some(mut transform) = world.get_mut::<Transform>(kinematic) {
            transform.position = Vec3::new(30.0, 0.5, 0.0);
        }
        let (started, ended) = step_collect(&mut world, DT);
        assert_eq!(started.len(), 0);
        assert_eq!(
            ended.len(),
            1,
            "挪开后应报 CollisionEnded（配对销毁成对性）"
        );
    }

    /// kinematic × 静态 sensor 配对进接触集合（压力板语义）：kinematic
    /// 角色踩 sensor 板即得事件，不需要动态代理。
    #[test]
    fn kinematic_touching_static_sensor_produces_events() {
        let mut world = make_event_world();
        world.spawn((
            Transform::from_xyz(0.0, 0.25, 0.0),
            Collider::sensor_box(Vec3::new(0.9, 0.25, 0.9)),
        ));
        world.spawn((
            Transform::from_xyz(0.0, 0.25, 0.0),
            Collider::dynamic_capsule(0.4, 0.9, 0.0),
            RigidBody {
                body_type: BodyType::Kinematic,
                ..RigidBody::default()
            },
        ));
        let (started, ended) = step_collect(&mut world, DT);
        assert_eq!(
            started.len(),
            1,
            "运动学体踩静态 sensor 应报 CollisionStarted（压力板语义）"
        );
        assert_eq!(ended.len(), 0);
    }

    /// kinematic × kinematic 配对与静-静同待遇：不进接触集合、不报事件。
    #[test]
    fn kinematic_kinematic_pairs_are_skipped() {
        let mut world = make_event_world();
        for x in [0.0f32, 0.5] {
            world.spawn((
                Transform::from_xyz(x, 0.5, 0.0),
                Collider::dynamic_sphere(0.5, 0.0),
                RigidBody {
                    body_type: BodyType::Kinematic,
                    ..RigidBody::default()
                },
            ));
        }
        let (started, ended) = step_collect(&mut world, DT);
        assert!(
            started.is_empty() && ended.is_empty(),
            "运动学 × 运动学配对应被跳过（与静-静同待遇）"
        );
    }

    /// kinematic 体进入全部「静态障碍」查询口径：推离查询与角色控制器
    /// 都把它当墙（BodyType::Kinematic 文档的语义承诺）。
    #[test]
    fn kinematic_counts_as_static_in_push_queries_and_character() {
        use crate::character::{CharacterParams, move_and_slide};
        let mut world = make_world();
        // kinematic 墙（竖直薄板，西侧面 x = 0.5）
        world.spawn((
            Transform::from_xyz(1.0, 1.0, 0.0),
            Collider::static_box(Vec3::new(0.5, 1.0, 2.0)),
            RigidBody {
                body_type: BodyType::Kinematic,
                ..RigidBody::default()
            },
        ));
        // 推离查询：起点嵌在墙内的球被沿 -X 推出
        let fix = resolve_against_statics(
            &world,
            Vec3::new(0.8, 1.0, 0.0),
            &ColliderShape::Sphere { radius: 0.2 },
        );
        assert!(
            fix.x < -0.05,
            "运动学墙应参与推离查询（沿 -X 推出），实际 {fix:?}"
        );
        // 角色控制器：+X 行走被 kinematic 墙挡住——墙面 x = 0.5，胶囊
        // 半径 0.2 → 中心停在 x ≈ 0.3。（位移取 0.6 m：扫掠终点须仍与
        // 墙体重叠——shape_sweep 的「完全贯穿不检测」诚实边界，见其文档。）
        let params = CharacterParams {
            radius: 0.2,
            half_height: 0.4,
            ..CharacterParams::default()
        };
        let output = move_and_slide(
            &world,
            Vec3::new(0.0, 0.4, 0.0),
            Vec3::new(36.0, 0.0, 0.0),
            DT,
            &params,
        );
        assert!(
            (output.position.x - 0.3).abs() < 5e-2,
            "角色应被 kinematic 墙挡在 x ≈ 0.3，实际 {}",
            output.position.x
        );
    }

    // -----------------------------------------------------------------------
    // R50：角速度积分（integrate_rotation 纯函数 + step_physics 角通道）
    // -----------------------------------------------------------------------

    /// 四元数逐位比较口径（AGENTS 的 `to_bits` 确定性模式）。
    fn quat_bits(q: Quat) -> [u32; 4] {
        [q.x.to_bits(), q.y.to_bits(), q.z.to_bits(), q.w.to_bits()]
    }

    /// 单步角积分对照解析轴角：ω = 2 rad/s 绕 +Z、dt = 1/60、无阻尼 ⇒
    /// 作用在 +X 上与 `from_axis_angle(Z, ω·dt)` 一致；零阻尼下 ω 逐位不变。
    ///
    /// 容差 1e-5 依据：`normalize(q + ½(ω,0)·q·dt)` 与 `exp(½(ω,0)·dt)·q`
    /// 的差是 **O((ω·dt)³)**（一阶截断 + 归一化恰好补回二阶项，见
    /// [`integrate_rotation`] 推导）；ω·dt = 0.0333 ⇒ 残差 ≈ 3.7e-6 < 1e-5。
    #[test]
    fn angular_step_matches_analytic_axis_angle() {
        let omega = Vec3::new(0.0, 0.0, 2.0);
        let (rotation, damped) = integrate_rotation(Quat::IDENTITY, omega, 0.0, DT);
        assert_eq!(damped, omega, "零阻尼下角速度逐位不变");
        let want = Quat::from_axis_angle(Vec3::Z, 2.0 * DT);
        assert!(
            (rotation * Vec3::X - want * Vec3::X).length() < 1e-5,
            "一步角积分应等于解析轴角旋转（残差 O(dt³)）：实际 {:?}、解析 {:?}",
            rotation * Vec3::X,
            want * Vec3::X
        );
        assert!(
            (rotation.length() - 1.0).abs() < 1e-6,
            "积分结果须为单位四元数，实际 |q| = {}",
            rotation.length()
        );
    }

    /// 长时积分：1000 步（16.7 s）大角速度后 ①范数仍为 1（每步 normalize
    /// 防漂移——打磨 VI 的 IK 四元数教训）②累计转角对照**离散积分的解析
    /// 解** `N·2·atan(ω·dt/2)`：一阶式的每步转角是 `2·atan(θ/2)` 而非 `θ`
    /// （归一化把 `1 + u` 映到 `atan(u)` 的半角），相对偏差 `(ω·dt)²/12`
    /// ——写进断言而不是拿连续解析解 `ω·t` 糊过去。
    #[test]
    fn long_angular_integration_keeps_unit_norm_and_exact_discrete_angle() {
        let omega = 6.0f32;
        let steps = 1000usize;
        let mut q = Quat::IDENTITY;
        let mut w = Vec3::new(0.0, 0.0, omega);
        for _ in 0..steps {
            let (next, damped) = integrate_rotation(q, w, 0.0, DT);
            q = next;
            w = damped;
        }
        assert!(
            (q.length() - 1.0).abs() < 1e-6,
            "{steps} 步后仍须单位范数（每步 normalize），实际 |q| = {}",
            q.length()
        );
        assert_eq!(w, Vec3::new(0.0, 0.0, omega), "零阻尼下角速度逐位不变");
        // 累计转角对照：总角 ≈ 100 rad 已绕圈十几轮，直接比四元数会被
        // 双覆盖 / 取模糊住——改比「实测与解析的**相对旋转角**」（数值稳健）
        let want = steps as f32 * 2.0 * (omega * DT * 0.5).atan();
        let delta = q * Quat::from_axis_angle(Vec3::Z, want).inverse();
        let angle_error = 2.0 * delta.xyz().length().atan2(delta.w.abs());
        assert!(
            angle_error < 1e-3,
            "累计转角应为离散解析解 N·2·atan(ω·dt/2) = {want} rad，实测偏差 {angle_error} rad"
        );
    }

    /// 角阻尼经完整 `step_physics` 管线：ω 按解析式 `ω₀·e^(−λt)` 衰减
    /// （与 `linear_damping_decays_velocity_exponentially` 同构同容差）；
    /// 失重 + 零线速度 ⇒ 位置逐位不动（角通道不掺进平移）。
    #[test]
    fn angular_damping_decays_angular_velocity_exponentially() {
        let mut world = make_world();
        let damping = 2.0f32;
        let spinner = world.spawn((
            Transform::from_xyz(0.0, 100.0, 0.0),
            RigidBody {
                gravity_scale: 0.0, // 隔离重力，只看角阻尼
                angular_velocity: Vec3::new(0.0, 3.0, 0.0),
                angular_damping: damping,
                ..RigidBody::default()
            },
        ));
        let seconds = 1.0;
        // round 而非截断：1.0/(1/60 f32) = 59.999…，直接 as usize 会丢一步
        for _ in 0..(seconds / DT).round() as usize {
            step(&world, DT);
        }
        let want = 3.0 * (-damping * seconds).exp();
        let body = world
            .get::<RigidBody>(spinner)
            .expect("飞行体应有 RigidBody");
        assert!(
            (body.angular_velocity.y - want).abs() < 1e-4,
            "ω 应为 ω₀·e^(-λt) = {want}，实际 {}",
            body.angular_velocity.y
        );
        assert_eq!(
            (body.angular_velocity.x, body.angular_velocity.z),
            (0.0, 0.0),
            "绕 Y 的角速度不应渗到其它轴"
        );
        let transform = world
            .get::<Transform>(spinner)
            .expect("飞行体应有 Transform");
        assert!(
            (transform.rotation * Vec3::X - Vec3::X).length() > 0.5,
            "角速度应真实旋转实体（绕 Y ⇒ +X 被转走），实际 {:?}",
            transform.rotation
        );
        assert_eq!(
            transform.position,
            Vec3::new(0.0, 100.0, 0.0),
            "失重 + 零线速度 ⇒ 位置逐位不动（角 / 平移不耦合）"
        );
    }

    /// 角通道与平移**不耦合**：同一落体场景，带 ω 与不带 ω 的两 world
    /// 逐帧位置 **bit 级一致**（既有 free_fall / restitution 等测试零改动
    /// 守护的正是这条；此处显式锁定）。
    #[test]
    fn angular_channel_does_not_coupling_with_translation() {
        fn build(spin: bool) -> (World, Entity) {
            let mut world = make_world();
            spawn_floor(&mut world);
            let body = world.spawn((
                Transform::from_xyz(0.0, 5.0, 0.0),
                Collider::dynamic_sphere(0.5, 0.3),
                RigidBody {
                    velocity: Vec3::new(1.0, 0.0, 0.0),
                    angular_velocity: if spin {
                        Vec3::new(1.0, 2.0, 3.0)
                    } else {
                        Vec3::ZERO
                    },
                    ..RigidBody::default()
                },
            ));
            (world, body)
        }
        let (world_spin, spun) = build(true);
        let (world_plain, plain) = build(false);
        for _ in 0..120 {
            step(&world_spin, DT);
            step(&world_plain, DT);
            assert_eq!(
                ball_position(&world_spin, spun),
                ball_position(&world_plain, plain),
                "位置轨迹应与角速度无关（逐位：旋转不参与平移积分，球接触各向同性）"
            );
            assert_eq!(
                ball_velocity(&world_spin, spun),
                ball_velocity(&world_plain, plain),
                "线速度同样与角速度无关（接触不产生角冲量、角也不反馈线）"
            );
        }
        assert_ne!(
            quat_bits(
                world_spin
                    .get::<Transform>(spun)
                    .expect("应有 Transform")
                    .rotation
            ),
            quat_bits(Quat::IDENTITY),
            "带 ω 的一方确实转了（测试非空转）"
        );
    }

    /// **零角速度 ⇒ 旋转写回整段跳过**：非恒等的既有姿态经 120 步物理
    /// （含落地接触）后 `Transform.rotation` **逐位不变**——若把 normalize
    /// 无条件塞进写回路径，末位舍入会逐帧改写既有场景的姿态（既有资产 /
    /// 快照的兼容性根据）。
    #[test]
    fn zero_angular_velocity_leaves_rotation_bit_identical() {
        let mut world = make_world();
        spawn_floor(&mut world);
        let rotation = Quat::from_rotation_y(0.7) * Quat::from_rotation_x(0.3);
        let body = world.spawn((
            Transform {
                position: Vec3::new(0.0, 3.0, 0.0),
                rotation,
                ..Transform::IDENTITY
            },
            Collider::dynamic_capsule(0.4, 0.9, 0.0),
            RigidBody::default(),
        ));
        for _ in 0..120 {
            step(&world, DT);
        }
        assert_eq!(
            quat_bits(
                world
                    .get::<Transform>(body)
                    .expect("应有 Transform")
                    .rotation
            ),
            quat_bits(rotation),
            "零角速度 ⇒ 既有旋转逐位不变（写回被跳过）"
        );
        // 倾斜胶囊的静止高度 = |旋转后轴的 y 分量|·轴半长 + 半径（旋转感知
        // 的落地口径；竖直时退化为 half_height）
        let axis_y = (rotation * Vec3::Y).y.abs();
        let want_y = axis_y * (0.9 - 0.4) + 0.4;
        assert!(
            (ball_position(&world, body).y - want_y).abs() < 1e-3,
            "倾斜胶囊应静止在 {want_y}（轴 y 分量 {axis_y}），实际 {}",
            ball_position(&world, body).y
        );
    }

    /// 角积分确定性：同输入两 world 逐采样位置 + 旋转 `to_bits` 一致。
    #[test]
    fn angular_integration_is_deterministic_across_runs() {
        fn build() -> (World, Entity) {
            let mut world = make_world();
            spawn_floor(&mut world);
            let body = world.spawn((
                Transform {
                    position: Vec3::new(0.0, 4.0, 0.0),
                    rotation: Quat::from_rotation_z(0.4),
                    ..Transform::IDENTITY
                },
                Collider::dynamic_capsule(0.3, 0.8, 0.1),
                RigidBody {
                    velocity: Vec3::new(1.5, 0.0, -0.5),
                    angular_velocity: Vec3::new(0.7, -1.3, 2.1),
                    angular_damping: 0.4,
                    friction: 0.5,
                    ..RigidBody::default()
                },
            ));
            (world, body)
        }
        let (world_a, body_a) = build();
        let (world_b, body_b) = build();
        for _ in 0..180 {
            step(&world_a, DT);
            step(&world_b, DT);
            let transform_a = world_a.get::<Transform>(body_a).expect("应有 Transform");
            let transform_b = world_b.get::<Transform>(body_b).expect("应有 Transform");
            assert_eq!(
                transform_a.position, transform_b.position,
                "位置应逐位一致（确定性）"
            );
            assert_eq!(
                quat_bits(transform_a.rotation),
                quat_bits(transform_b.rotation),
                "旋转应逐位一致（确定性：无随机源、固定遍历序）"
            );
            assert!(
                (transform_a.rotation.length() - 1.0).abs() < 1e-5,
                "长跑后仍须单位范数，实际 |q| = {}",
                transform_a.rotation.length()
            );
        }
    }

    // -----------------------------------------------------------------------
    // R50：旋转感知胶囊（检测 / 射线 / 扫掠 / 推离 / 管线）
    // -----------------------------------------------------------------------

    /// 躺平（轴沿世界 X）：绕 Z 转 90°，本地 +Y → 世界 ∓X。
    /// 注：`from_rotation_z(π/2)` 的 `cos` 残差 ~4.4e-8 ⇒ 涉及旋转的断言
    /// 一律写 1e-6 容差（依据即此残差量级），不做无依据的 `assert_eq!`。
    fn lying_x() -> Quat {
        Quat::from_rotation_z(std::f32::consts::FRAC_PI_2)
    }

    /// 躺平（轴沿世界 Z）：绕 X 转 90°，本地 +Y → 世界 +Z。
    fn lying_z() -> Quat {
        Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)
    }

    /// 旋转胶囊-球：轴随实体旋转 ⇒ 同一球在「躺平后接触、竖直时不接触」
    /// 与「竖直时接触、躺平后不接触」两个方向上都翻转（旋转是差异的唯一
    /// 来源——位置 / 形状 / 半径全同）。穿透与法线对照解析值。
    #[test]
    fn rotated_capsule_axis_follows_entity_rotation() {
        let capsule = ColliderShape::Capsule {
            radius: 0.5,
            half_height: 1.5,
        };
        let sphere = ColliderShape::Sphere { radius: 0.5 };

        // 轴向（世界 X）上的球：躺平后轴端点在 (±1, 0, 0)，球心 1.4 ⇒
        // 最近点距 0.4 < 半径和 1.0 ⇒ 穿透 0.6、法线 +X（a 胶囊 → b 球）
        let along = detect_pair_rotated(
            Vec3::ZERO,
            lying_x(),
            &capsule,
            Vec3::new(1.4, 0.0, 0.0),
            Quat::IDENTITY,
            &sphere,
        )
        .expect("躺平胶囊的轴向上应接触");
        assert!(
            (along.penetration - 0.6).abs() < 1e-6,
            "穿透 = 半径和 1.0 − 轴端点距 0.4，实际 {}",
            along.penetration
        );
        assert!(
            (along.normal - Vec3::X).length() < 1e-6,
            "法线应从胶囊指向球（+X），实际 {:?}",
            along.normal
        );
        assert!(
            detect_pair(Vec3::ZERO, &capsule, Vec3::new(1.4, 0.0, 0.0), &sphere).is_none(),
            "竖直口径下轴段最近点是中心（距 1.4 > 1.0）⇒ 不接触"
        );

        // 原竖直轴向（世界 Y）上的球：躺平后落在轴外，竖直时压在端帽内
        assert!(
            detect_pair_rotated(
                Vec3::ZERO,
                lying_x(),
                &capsule,
                Vec3::new(0.0, 1.4, 0.0),
                Quat::IDENTITY,
                &sphere
            )
            .is_none(),
            "躺平后 y = 1.4 的球距轴 1.4 > 1.0 ⇒ 不接触"
        );
        assert!(
            detect_pair(Vec3::ZERO, &capsule, Vec3::new(0.0, 1.4, 0.0), &sphere).is_some(),
            "竖直口径下同一球压在上端帽内 ⇒ 接触（对照）"
        );
    }

    /// 旋转胶囊-AABB：躺平胶囊（轴沿 X）压在地面盒上 ⇒ 穿透 = 半径 − 轴高
    /// （0.5 − 0.4 = 0.1）、法线 −Y；同一位置竖直口径的轴段深入盒内 ⇒
    /// 走盒内最浅轴退出、穿透 = 半径 0.5（两者的差异即旋转感知的证据）。
    #[test]
    fn rotated_capsule_box_penetration_uses_rotated_axis() {
        let capsule = ColliderShape::Capsule {
            radius: 0.5,
            half_height: 1.5,
        };
        let floor = ColliderShape::Box {
            half_extents: Vec3::new(50.0, 1.0, 50.0),
        };
        let floor_pos = Vec3::new(0.0, -1.0, 0.0);

        let lying = detect_pair_rotated(
            Vec3::new(0.0, 0.4, 0.0),
            lying_x(),
            &capsule,
            floor_pos,
            Quat::IDENTITY,
            &floor,
        )
        .expect("躺平胶囊应压在地面上");
        assert!(
            (lying.penetration - 0.1).abs() < 1e-6,
            "穿透 = 半径 0.5 − 轴高 0.4，实际 {}",
            lying.penetration
        );
        assert!(
            (lying.normal - Vec3::new(0.0, -1.0, 0.0)).length() < 1e-6,
            "法线从胶囊指向盒（−Y），实际 {:?}",
            lying.normal
        );

        let upright = detect_pair(Vec3::new(0.0, 0.4, 0.0), &capsule, floor_pos, &floor)
            .expect("竖直胶囊同样接触");
        assert!(
            (upright.penetration - 0.5).abs() < 1e-6,
            "竖直轴段深入盒内 ⇒ 盒内退出、穿透 = 半径 0.5，实际 {}",
            upright.penetration
        );
    }

    /// 旋转胶囊-旋转胶囊（十字交叉）：段-段最近点解析对照——A 轴沿 X
    /// （原点）、B 轴沿 Z（抬高 0.3）⇒ 最近点对 (0,0,0) 与 (0,0.3,0)、
    /// 穿透 = 1.0 − 0.3 = 0.7、法线 +Y；抬到 1.2（> 半径和）即不接触，
    /// 而竖直口径下同位置两轴重叠仍接触。
    #[test]
    fn rotated_capsule_capsule_crossed_axes_analytic() {
        let capsule = ColliderShape::Capsule {
            radius: 0.5,
            half_height: 1.5,
        };
        let contact = detect_pair_rotated(
            Vec3::ZERO,
            lying_x(),
            &capsule,
            Vec3::new(0.0, 0.3, 0.0),
            lying_z(),
            &capsule,
        )
        .expect("十字交叉的胶囊应接触");
        assert!(
            (contact.penetration - 0.7).abs() < 1e-6,
            "穿透 = 半径和 1.0 − 轴间距 0.3，实际 {}",
            contact.penetration
        );
        assert!(
            (contact.normal - Vec3::Y).length() < 1e-6,
            "法线从 a 指向 b（+Y），实际 {:?}",
            contact.normal
        );
        assert!(
            detect_pair_rotated(
                Vec3::ZERO,
                lying_x(),
                &capsule,
                Vec3::new(0.0, 1.2, 0.0),
                lying_z(),
                &capsule
            )
            .is_none(),
            "轴间距 1.2 > 半径和 1.0 ⇒ 不接触"
        );
        assert!(
            detect_pair(Vec3::ZERO, &capsule, Vec3::new(0.0, 1.2, 0.0), &capsule).is_some(),
            "竖直口径下两轴段重叠 ⇒ 接触（对照）"
        );
    }

    /// 旋转胶囊的射线：轴沿 Z 的躺平胶囊（r 0.5、轴半长 1.0），射线从
    /// (5, 0.3, 0) 沿 −X ⇒ 圆柱侧壁命中 x = √(0.25 − 0.09) = 0.4、
    /// t = 4.6、外法线 = 离轴径向 (0.8, 0.6, 0)；同一射线的竖直口径是
    /// t = 4.5、法线 +X（旋转改变命中点与法线，两条都是解析精确值）。
    #[test]
    fn rotated_capsule_ray_hits_lying_cylinder() {
        let (t, normal) = ray_capsule_rotated(
            Vec3::new(5.0, 0.3, 0.0),
            Vec3::new(-1.0, 0.0, 0.0),
            Vec3::ZERO,
            lying_z(),
            0.5,
            1.5,
        )
        .expect("躺平胶囊的圆柱段应命中");
        assert!((t - 4.6).abs() < 1e-5, "t = 5 − 0.4 = 4.6，实际 {t}");
        assert!(
            (normal - Vec3::new(0.8, 0.6, 0.0)).length() < 1e-5,
            "外法线 = 离轴径向 (0.4, 0.3, 0)/0.5，实际 {normal:?}"
        );
        // 分发层同样旋转感知（ray_vs_shape_rotated）
        let hit = ray_vs_shape_rotated(
            Vec3::new(5.0, 0.3, 0.0),
            Vec3::new(-1.0, 0.0, 0.0),
            10.0,
            Vec3::ZERO,
            lying_z(),
            &ColliderShape::Capsule {
                radius: 0.5,
                half_height: 1.5,
            },
        )
        .expect("分发层应命中");
        assert!((hit.t - 4.6).abs() < 1e-5, "分发层 t 同源，实际 {}", hit.t);
        // 竖直口径对照：t = 4.5、法线 +X（既有解析精确值）
        let (t_upright, n_upright) = ray_capsule(
            Vec3::new(5.0, 0.3, 0.0),
            Vec3::new(-1.0, 0.0, 0.0),
            Vec3::ZERO,
            0.5,
            1.5,
        )
        .expect("竖直胶囊应命中");
        assert_eq!(t_upright, 4.5, "竖直圆柱的 t 解析精确");
        assert_eq!(n_upright, Vec3::X, "竖直圆柱的外法线 = 水平径向");
    }

    /// 旋转感知的推离与扫掠查询：躺平胶囊（轴沿 X、轴高 0.5）作静态障碍，
    /// 半径 0.3 的球在 (0, 1.2, 0) ⇒ 距轴 0.7 < 0.8 ⇒ 沿 +Y 推出 0.1；
    /// 竖直口径下球心正落在轴段上（退化）⇒ 法线取确定性 +Y、穿透 = 半径和
    /// 0.8（两者差异悬殊，正是「尸体倒下后不该被当成直立柱」的根据）。
    /// 扫掠：球从 (0, 2, 0) 下落 2 m ⇒ 首次接触在球心 y = 0.5 + 0.8 = 1.3
    /// ⇒ t = 0.35（含扫掠皮肤收缩）。
    #[test]
    fn rotated_capsule_push_and_sweep_queries() {
        let capsule = ColliderShape::Capsule {
            radius: 0.5,
            half_height: 1.5,
        };
        let query = ColliderShape::Sphere { radius: 0.3 };
        let at = Vec3::new(0.0, 0.5, 0.0);
        let rotated: Vec<StaticObstacle> = vec![(at, lying_x(), capsule)];
        let upright = vec![(at, capsule)];

        let push =
            resolve_static_push_rotated(Vec3::new(0.0, 1.2, 0.0), Quat::IDENTITY, &query, &rotated);
        assert!(
            (push - Vec3::new(0.0, 0.1, 0.0)).length() < 1e-6,
            "应沿 +Y 推出 0.1（半径和 0.8 − 轴距 0.7），实际 {push:?}"
        );
        let push_upright = resolve_static_push(Vec3::new(0.0, 1.2, 0.0), &query, &upright);
        assert!(
            (push_upright - Vec3::new(0.0, 0.8, 0.0)).length() < 1e-6,
            "竖直口径：球心落在轴段上 ⇒ 退化法线 +Y、穿透 = 半径和 0.8，实际 {push_upright:?}"
        );

        let (t, normal) = shape_sweep_rotated(
            Vec3::new(0.0, 2.0, 0.0),
            Vec3::new(0.0, -2.0, 0.0),
            Quat::IDENTITY,
            &query,
            &rotated,
        )
        .expect("下落到躺平胶囊应命中");
        assert!(
            (t - 0.35).abs() < 1e-3,
            "首次接触分数 = (2 − 1.3)/2 = 0.35（含 1e-4 皮肤收缩与二分收敛），实际 {t}"
        );
        assert!(
            (normal - Vec3::Y).length() < 1e-3,
            "阻挡法线从静态体指向扫掠球（+Y），实际 {normal:?}"
        );
    }

    /// 躺平胶囊经**完整管线**（旋转 AABB → 网格 broadphase → 旋转感知
    /// detect → 求解 → 写回）落地：静止高度 = 半径 0.5（竖直口径是
    /// half_height 1.5），且零角速度下旋转逐位不变。
    #[test]
    fn lying_capsule_settles_on_floor_through_pipeline() {
        const FINE: f32 = 1.0 / 120.0;
        let mut world = make_world();
        spawn_floor(&mut world);
        let rotation = lying_x();
        let body = world.spawn((
            Transform {
                position: Vec3::new(0.0, 3.0, 0.0),
                rotation,
                ..Transform::IDENTITY
            },
            Collider::dynamic_capsule(0.5, 1.5, 0.0),
            RigidBody::default(),
        ));
        let steps = (2.0 / FINE) as usize;
        for _ in 0..steps {
            step(&world, FINE);
        }
        let transform = world.get::<Transform>(body).expect("应有 Transform");
        assert!(
            (transform.position.y - 0.5).abs() < 1e-3,
            "躺平胶囊应静止在半径高度 0.5（轴水平 ⇒ 最低点 = 中心 − r），实际 {}",
            transform.position.y
        );
        assert!(
            transform.position.x.abs() < 0.2,
            "水平轴胶囊落地不应横向滑走，实际 x = {}",
            transform.position.x
        );
        assert_eq!(
            quat_bits(transform.rotation),
            quat_bits(rotation),
            "零角速度 ⇒ 躺平朝向逐位保持（不会自己立起来）"
        );
        assert!(
            ball_velocity(&world, body).length() < 1e-3,
            "静止后速度应 ≈ 0，实际 {:?}",
            ball_velocity(&world, body)
        );
    }

    /// **兼容锁（R50 的硬验收）**：恒等旋转下全部旋转感知 API 与既有 API
    /// **逐位一致**——检测（9 种形状配对 × 16 组位置）、射线 / 线段（胶囊
    /// 三入口）、静态推离、形状扫掠、角色控制器。这条测试是「既有场景
    /// 胶囊恒竖直 + rotation 恒等 ⇒ 结果逐位不变」的直接证据，与既有
    /// R31 胶囊测试零改动全过互为表里。
    #[test]
    fn identity_rotation_matches_upright_api_bitwise() {
        use crate::character::{
            CharacterParams, move_and_slide_statics, move_and_slide_statics_rotated,
        };

        let capsule = ColliderShape::Capsule {
            radius: 0.4,
            half_height: 0.9,
        };
        let sphere = ColliderShape::Sphere { radius: 0.3 };
        let box_ = ColliderShape::Box {
            half_extents: Vec3::new(1.0, 0.5, 1.0),
        };
        let positions = [
            Vec3::ZERO,
            Vec3::new(0.7, 0.2, -0.3),
            Vec3::new(0.0, 1.1, 0.0),
            Vec3::new(-0.5, 0.0, 0.9),
        ];

        // ---- 检测：全部 9 种配对 × 16 组位置 ----
        for &pa in &positions {
            for &pb in &positions {
                for (sa, sb) in &[
                    (capsule, sphere),
                    (sphere, capsule),
                    (capsule, box_),
                    (box_, capsule),
                    (capsule, capsule),
                    (sphere, sphere),
                    (sphere, box_),
                    (box_, sphere),
                    (box_, box_),
                ] {
                    assert_eq!(
                        detect_pair_rotated(pa, Quat::IDENTITY, sa, pb, Quat::IDENTITY, sb),
                        detect_pair(pa, sa, pb, sb),
                        "恒等旋转下 detect 应逐位一致（{pa:?} × {pb:?}）"
                    );
                }
            }
        }

        // ---- 射线 / 线段（胶囊三入口）----
        let origins = [
            Vec3::new(4.0, 0.3, 0.0),
            Vec3::new(0.2, 0.0, 0.0),
            Vec3::new(-3.0, 1.2, 2.0),
            Vec3::new(0.0, 2.5, -1.0),
        ];
        let dirs = [
            Vec3::new(-1.0, 0.0, 0.0),
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
        ];
        let centers = [Vec3::ZERO, Vec3::new(1.0, 0.5, -1.0)];
        for &origin in &origins {
            for &dir in &dirs {
                for &center in &centers {
                    assert_eq!(
                        ray_capsule_rotated(origin, dir, center, Quat::IDENTITY, 0.4, 0.9),
                        ray_capsule(origin, dir, center, 0.4, 0.9),
                        "恒等旋转下 ray_capsule 应逐位一致（{origin:?} {dir:?} {center:?}）"
                    );
                    assert_eq!(
                        ray_vs_shape_rotated(origin, dir, 10.0, center, Quat::IDENTITY, &capsule),
                        ray_vs_shape(origin, dir, 10.0, center, &capsule),
                        "恒等旋转下 ray_vs_shape 应逐位一致"
                    );
                    assert_eq!(
                        segment_hits_shape_rotated(
                            origin,
                            origin + dir * 5.0,
                            center,
                            Quat::IDENTITY,
                            &capsule
                        ),
                        segment_hits_shape(origin, origin + dir * 5.0, center, &capsule),
                        "恒等旋转下 segment_hits_shape 应逐位一致"
                    );
                }
            }
        }

        // ---- 静态推离 / 形状扫掠 / 角色控制器 ----
        let statics = vec![
            (Vec3::new(0.0, -1.0, 0.0), box_),
            (Vec3::new(1.2, 0.0, 0.0), capsule),
            (Vec3::new(-0.8, 0.4, 0.5), sphere),
        ];
        let obstacles: Vec<StaticObstacle> = statics
            .iter()
            .map(|&(p, shape)| (p, Quat::IDENTITY, shape))
            .collect();
        for &p in &positions {
            assert_eq!(
                resolve_static_push_rotated(p, Quat::IDENTITY, &capsule, &obstacles),
                resolve_static_push(p, &capsule, &statics),
                "恒等旋转下静态推离应逐位一致（{p:?}）"
            );
            for &dir in &dirs {
                assert_eq!(
                    shape_sweep_rotated(p, dir * 2.0, Quat::IDENTITY, &capsule, &obstacles),
                    shape_sweep(p, dir * 2.0, &capsule, &statics),
                    "恒等旋转下形状扫掠应逐位一致（{p:?} {dir:?}）"
                );
            }
        }
        let params = CharacterParams {
            radius: 0.4,
            half_height: 0.9,
            ..CharacterParams::default()
        };
        for &p in &positions {
            assert_eq!(
                move_and_slide_statics_rotated(
                    p,
                    Vec3::new(2.0, -1.0, 0.5),
                    DT,
                    &params,
                    &obstacles
                ),
                move_and_slide_statics(p, Vec3::new(2.0, -1.0, 0.5), DT, &params, &statics),
                "恒等旋转下角色控制器应逐位一致（{p:?}）"
            );
        }
    }

    // -----------------------------------------------------------------------
    // R50：SwingJoint（锚点距离 + 摆动锥限位 + 肢体轴方向修正）
    // -----------------------------------------------------------------------

    /// 构造摆动关节（纯函数测试用）：`target` 只是载荷（纯求解不看它），
    /// 用 `Entity::from_bits(0)` 占位；`damping` 固定 0.5（与 ragdoll 预设
    /// 同值）——下面各测试都以**零速度**调用求解 ⇒ 阻尼通道恒零，不干扰
    /// 位置 / 旋转断言（阻尼本身由 `swing_joint_damping_decays_axial_*`
    /// 单独锁定）。
    fn swing(
        anchor_a: Vec3,
        anchor_b: Vec3,
        rest_offset: Vec3,
        limit: f32,
        stiffness: f32,
    ) -> SwingJoint {
        SwingJoint {
            target: Entity::from_bits(0),
            anchor_a,
            anchor_b,
            rest_offset,
            swing_limit_radians: limit,
            stiffness,
            damping: 0.5,
        }
    }

    /// 求解便捷封装（纯函数测试专用）：把「哪端是动体」的 bool 口径翻成
    /// 新签名的 `Option<Vec3>` 速度（动体 = `Some(零速度)`）+ 首轮施加。
    /// 速度取零 ⇒ 轴向阻尼通道恒零，位置 / 旋转断言不被干扰；阻尼本身由
    /// `swing_joint_damping_decays_axial_relative_velocity` 直接调
    /// [`solve_swing_joint`] 锁定。
    #[allow(clippy::too_many_arguments)]
    fn solve(
        pos_a: Vec3,
        rot_a: Quat,
        dynamic_a: bool,
        pos_b: Vec3,
        rot_b: Quat,
        dynamic_b: bool,
        joint: SwingJoint,
    ) -> SwingResponse {
        solve_swing_joint(
            pos_a,
            rot_a,
            dynamic_a.then_some(Vec3::ZERO),
            pos_b,
            rot_b,
            dynamic_b.then_some(Vec3::ZERO),
            joint,
            true,
        )
    }

    /// 轴向阻尼（速度通道）：子端以 3 m/s 沿锚点连线远离父端（静态）、
    /// 另有 2 m/s 切向分量 ⇒ 首轮求解后轴向分量 ×(1 − damping) = 1.5、
    /// 切向分量原样保留（摆动本身不被阻尼）；非首轮（`apply_velocity =
    /// false`）速度通道恒零。与 `joint_damping_decays_axial_relative_velocity`
    /// （距离关节）同口径同解析值。
    #[test]
    fn swing_joint_damping_decays_axial_relative_velocity() {
        // 父端窝在原点，子端在 (0, −1, 0)：连线轴 = 子端 → 窝 = +Y
        let joint = swing(
            Vec3::ZERO,
            Vec3::ZERO,
            Vec3::new(0.0, 1.0, 0.0),
            std::f32::consts::PI, // 隔离锥段
            1.0,
        );
        let pos_a = Vec3::new(0.0, -1.0, 0.0);
        // 相对速度 = 轴向 +3（远离窝，沿 −Y 为正的「拉开」方向取 −Y）+ 切向 2（+X）
        let velocity = Vec3::new(2.0, -3.0, 0.0);
        let response = solve_swing_joint(
            pos_a,
            Quat::IDENTITY,
            Some(velocity),
            Vec3::ZERO,
            Quat::IDENTITY,
            None,
            joint,
            true,
        );
        // 轴向分量 −3 → −1.5（damping 0.5），Δv = +1.5·(轴向单位向)；
        // 轴向单位向 = 子端 → 窝 = +Y ⇒ Δv = (0, +1.5, 0)
        assert!(
            (response.delta_vel_a - Vec3::new(0.0, 1.5, 0.0)).length() < 1e-6,
            "轴向分量应衰减一半、切向保留 ⇒ Δv = (0, 1.5, 0)，实际 {:?}",
            response.delta_vel_a
        );
        assert_eq!(response.delta_vel_b, Vec3::ZERO, "静态父端不吃速度修正");
        let damped = velocity + response.delta_vel_a;
        assert!(
            (damped - Vec3::new(2.0, -1.5, 0.0)).length() < 1e-6,
            "阻尼后速度 = 切向 2 + 轴向 −1.5，实际 {damped:?}"
        );

        // 非首轮：速度通道恒零（重复结算会放大阻尼，与接触同轮同理由）
        let later = solve_swing_joint(
            pos_a,
            Quat::IDENTITY,
            Some(velocity),
            Vec3::ZERO,
            Quat::IDENTITY,
            None,
            joint,
            false,
        );
        assert_eq!(later.delta_vel_a, Vec3::ZERO, "非首轮不施加速度响应");
        assert_eq!(later.delta_vel_b, Vec3::ZERO);
    }

    /// **限位内零修正（幂等）**：rest 姿态（锚距 = rest、方向 = 锥轴、肢体
    /// 轴已对准关节窝）⇒ 位置修正逐位为零、旋转不修正。这是关节不抖的
    /// 关键——任何「静止时也在微调」的实现都会把 ragdoll 抖散。
    #[test]
    fn swing_joint_inside_limit_is_bit_exact_no_op() {
        // 父端 b 在原点、关节窝 anchor_b = (0, 1, 0)；子端 a 中心 (0, 0.5, 0)
        // ⇒ 锚距 0.5 = |rest_offset|、方向 +Y = 锥轴
        let joint = swing(
            Vec3::ZERO,
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 0.5, 0.0),
            1.0,
            1.0,
        );
        let response = solve(
            Vec3::new(0.0, 0.5, 0.0),
            Quat::IDENTITY,
            true,
            Vec3::ZERO,
            Quat::IDENTITY,
            false,
            joint,
        );
        assert_eq!(
            response.delta_pos_a,
            Vec3::ZERO,
            "rest 姿态 ⇒ 子端零位置修正（逐位）"
        );
        assert_eq!(
            response.delta_pos_b,
            Vec3::ZERO,
            "rest 姿态 ⇒ 父端零位置修正（逐位）"
        );
        assert_eq!(response.rotation_a, None, "肢体 +Y 已对准关节窝 ⇒ 不修旋转");
        // 幂等 + 纯函数确定性：同输入两次求解逐位一致
        let again = solve(
            Vec3::new(0.0, 0.5, 0.0),
            Quat::IDENTITY,
            true,
            Vec3::ZERO,
            Quat::IDENTITY,
            false,
            joint,
        );
        assert_eq!(again, response, "同输入两次求解应逐位一致（纯函数）");
    }

    /// 锚点距离投影（第①段，限位取 π 隔离锥段）：锚距 0.8 > rest 0.5 ⇒
    /// err 0.3 沿连线修正——父端静态时子端全吃 0.3（修正后锚距 = rest，
    /// 解析精确）、双动时各半 0.15、弹簧刚度 0.5 时半额 0.15。
    #[test]
    fn swing_joint_anchor_distance_projects_to_rest() {
        let limit_free = std::f32::consts::PI; // cos π = −1 ⇒ 锥段永不触发
        let joint = swing(
            Vec3::ZERO,
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 0.5, 0.0),
            limit_free,
            1.0,
        );
        let pos_a = Vec3::new(0.0, 0.2, 0.0); // 锚距 0.8
        let socket = Vec3::new(0.0, 1.0, 0.0);

        // 父端静态（share_a = 1）：子端全额拉回
        let r = solve(
            pos_a,
            Quat::IDENTITY,
            true,
            Vec3::ZERO,
            Quat::IDENTITY,
            false,
            joint,
        );
        assert!(
            (r.delta_pos_a - Vec3::new(0.0, 0.3, 0.0)).length() < 1e-6,
            "静态父端 ⇒ 子端全吃 err 0.3（+Y），实际 {:?}",
            r.delta_pos_a
        );
        assert_eq!(r.delta_pos_b, Vec3::ZERO, "静态端不动");
        assert!(
            ((pos_a + r.delta_pos_a).distance(socket) - 0.5).abs() < 1e-6,
            "修正后锚距应恰为 rest 0.5，实际 {}",
            (pos_a + r.delta_pos_a).distance(socket)
        );

        // 双动：**层级投影** ⇒ 父端仍不被挪动、子端全额拉回（与静态父端
        // 同值；这是链上约束不互相扰动的根据，见 SwingJoint 类型文档）
        let both = solve(
            pos_a,
            Quat::IDENTITY,
            true,
            Vec3::ZERO,
            Quat::IDENTITY,
            true,
            joint,
        );
        assert!(
            (both.delta_pos_a - Vec3::new(0.0, 0.3, 0.0)).length() < 1e-6,
            "层级投影：双动时子端也全额吃 err 0.3，实际 {:?}",
            both.delta_pos_a
        );
        assert_eq!(
            both.delta_pos_b,
            Vec3::ZERO,
            "层级投影：父端不被子端约束挪动（恒零）"
        );

        // 弹簧刚度 0.5：半额修正（迭代收敛语义，同 DistanceJoint）
        let spring = swing(
            Vec3::ZERO,
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 0.5, 0.0),
            limit_free,
            0.5,
        );
        let r_spring = solve(
            pos_a,
            Quat::IDENTITY,
            true,
            Vec3::ZERO,
            Quat::IDENTITY,
            false,
            spring,
        );
        assert!(
            (r_spring.delta_pos_a - Vec3::new(0.0, 0.15, 0.0)).length() < 1e-6,
            "stiffness 0.5 ⇒ 半额修正 0.15，实际 {:?}",
            r_spring.delta_pos_a
        );
    }

    /// 摆动锥限位（第②段）解析对照：关节窝在 (1, 0, 0)、rest = 1、锥轴
    /// +X、限位 45°；子端在 (1, −1, 0) ⇒ 锚距已是 rest（①零修正）但方向
    /// 与锥轴成 90° ⇒ 超限。拉回量 = (−cos45, 1−cos45, 0)；**修正后夹角
    /// 恰为 45°、锚距仍为 1**（锥面 = 只改方向不改长度，与①正交）；肢体轴
    /// （第③段）对准锥面方向。
    #[test]
    fn swing_joint_pulls_back_to_cone_surface() {
        let limit = std::f32::consts::FRAC_PI_4;
        let joint = swing(
            Vec3::ZERO,
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            limit,
            1.0,
        );
        let pos_a = Vec3::new(1.0, -1.0, 0.0);
        let socket = Vec3::new(1.0, 0.0, 0.0);
        let response = solve(
            pos_a,
            Quat::IDENTITY,
            true,
            Vec3::ZERO,
            Quat::IDENTITY,
            false,
            joint,
        );
        let cos45 = std::f32::consts::FRAC_1_SQRT_2;
        assert!(
            (response.delta_pos_a - Vec3::new(-cos45, 1.0 - cos45, 0.0)).length() < 1e-6,
            "锥面拉回量应等于解析值 (−cos45, 1−cos45, 0)，实际 {:?}",
            response.delta_pos_a
        );
        assert_eq!(response.delta_pos_b, Vec3::ZERO, "静态父端不动");

        // 修正后：夹角 = 限位、锚距 = rest（解析对照）
        let moved = pos_a + response.delta_pos_a;
        let delta = socket - moved;
        assert!(
            (delta.length() - 1.0).abs() < 1e-6,
            "锥面拉回只改方向不改长度，锚距应仍为 1，实际 {}",
            delta.length()
        );
        let angle = (delta.normalize().dot(Vec3::X)).clamp(-1.0, 1.0).acos();
        assert!(
            (angle - limit).abs() < 1e-6,
            "修正后夹角应恰为限位 {limit} rad，实际 {angle} rad"
        );
        // 肢体轴对准锥面方向（第③段：最小弧把本地 +Y 转到限位内的方向）
        let rotation = response.rotation_a.expect("超限时应给出肢体目标旋转");
        let cone_dir = delta.normalize();
        assert!(
            (rotation * Vec3::Y - cone_dir).length() < 1e-5,
            "肢体 +Y 应对准锥面方向 {cone_dir:?}，实际 {:?}",
            rotation * Vec3::Y
        );
    }

    /// **锥轴随父端旋转**（解剖学口径的直接证据）：关节窝在父端中心
    /// （anchor_b = 0）、rest_offset = (0, 1, 0) ⇒ 锥轴 = `rot_b·Y`。
    /// - 父端恒等：子端挂在正下方 ⇒ rest 姿态、零修正；
    /// - 父端与子端一起翻滚 90°：窝与锥轴一起转 ⇒ **仍零修正**（帧无关性
    ///   ——世界固定锥轴的实现会在这里产生修正，把尸体肢体钉回出生朝向）；
    /// - 只翻父端（子端留在原处）：锥轴转走 ⇒ 同一几何现在超限、有修正，
    ///   且拉回后对**新**锥轴成限位角。
    #[test]
    fn swing_joint_cone_axis_follows_parent_rotation() {
        let joint = swing(
            Vec3::ZERO,
            Vec3::ZERO,
            Vec3::new(0.0, 1.0, 0.0),
            std::f32::consts::FRAC_PI_4,
            1.0,
        );
        let pos_a = Vec3::new(0.0, -1.0, 0.0);

        let rest = solve(
            pos_a,
            Quat::IDENTITY,
            true,
            Vec3::ZERO,
            Quat::IDENTITY,
            false,
            joint,
        );
        assert_eq!(
            rest.delta_pos_a,
            Vec3::ZERO,
            "rest 姿态（子端在锥轴上）⇒ 零修正"
        );

        let tumbling = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        // 「一起翻滚」= 整个装配绕父端刚体旋转：子端位置也按同一旋转带走
        let together = solve(
            tumbling * pos_a,
            tumbling,
            true,
            Vec3::ZERO,
            tumbling,
            false,
            joint,
        );
        assert!(
            together.delta_pos_a.length() < 1e-6,
            "父端与子端一起翻滚 ⇒ rest 姿态仍零修正（锥轴随父端、帧无关），实际 {:?}",
            together.delta_pos_a
        );

        let parent_only = solve(
            pos_a,
            Quat::IDENTITY,
            true,
            Vec3::ZERO,
            tumbling,
            false,
            joint,
        );
        assert!(
            parent_only.delta_pos_a.length() > 1e-3,
            "只翻父端 ⇒ 锥轴转走、同一子端位置超限应被拉回，实际 {:?}",
            parent_only.delta_pos_a
        );
        let moved = pos_a + parent_only.delta_pos_a;
        let axis = tumbling * Vec3::Y;
        let angle = ((Vec3::ZERO - moved).normalize().dot(axis))
            .clamp(-1.0, 1.0)
            .acos();
        assert!(
            (angle - std::f32::consts::FRAC_PI_4).abs() < 1e-5,
            "拉回后应对**新**锥轴成 45°，实际 {angle} rad"
        );
    }

    /// 退化三支（全部确定性跳过 / 兜底，不 panic、不 NaN、不凭空选轴）：
    /// ① 两锚点重合 ⇒ 全零响应；② `rest_offset` 零长（无锥轴）⇒ 跳过锥段、
    /// 距离段退化成 **pin**（rest = 0 把子端拉到窝上）；③ 子端正好折到锥轴
    /// 背面（反平行、垂直分量 ≈ 0）⇒ 取确定性任意垂直向，拉回后夹角仍是限位。
    #[test]
    fn swing_joint_degenerate_cases_skip_deterministically() {
        // ① 锚点重合
        let coincident = swing(Vec3::ZERO, Vec3::ZERO, Vec3::new(0.0, 0.5, 0.0), 1.0, 1.0);
        let r = solve(
            Vec3::ZERO,
            Quat::IDENTITY,
            true,
            Vec3::ZERO,
            Quat::IDENTITY,
            false,
            coincident,
        );
        assert_eq!(r.delta_pos_a, Vec3::ZERO, "锚点重合 ⇒ 零修正（轴不定）");
        assert_eq!(r.delta_pos_b, Vec3::ZERO);
        assert_eq!(r.rotation_a, None, "锚点重合 ⇒ 不凭空选轴修旋转");

        // ② rest_offset 零长 ⇒ pin（距离段仍生效、锥段跳过）
        let pin = swing(Vec3::ZERO, Vec3::ZERO, Vec3::ZERO, 1.0, 1.0);
        let r = solve(
            Vec3::new(0.3, 0.0, 0.0),
            Quat::IDENTITY,
            true,
            Vec3::ZERO,
            Quat::IDENTITY,
            false,
            pin,
        );
        assert!(
            (r.delta_pos_a - Vec3::new(-0.3, 0.0, 0.0)).length() < 1e-6,
            "rest = 0 ⇒ 子端被拉到父端锚点（pin），实际 {:?}",
            r.delta_pos_a
        );

        // ③ 反平行（子端在锥轴正背面）：垂直分量退化 ⇒ 任意垂直向兜底
        let anti = swing(
            Vec3::ZERO,
            Vec3::ZERO,
            Vec3::new(0.0, 1.0, 0.0),
            std::f32::consts::FRAC_PI_4,
            1.0,
        );
        let pos_a = Vec3::new(0.0, 1.0, 0.0); // d = 窝 − 子端 = (0,−1,0) = −锥轴
        let r = solve(
            pos_a,
            Quat::IDENTITY,
            true,
            Vec3::ZERO,
            Quat::IDENTITY,
            false,
            anti,
        );
        assert!(
            r.delta_pos_a.is_finite() && r.delta_pos_b.is_finite(),
            "反平行退化不得产出 NaN / Inf，实际 {:?}",
            r.delta_pos_a
        );
        let moved = pos_a + r.delta_pos_a;
        assert!(
            (moved.length() - 1.0).abs() < 1e-6,
            "锥面拉回保长度：锚距应仍为 1，实际 {}",
            moved.length()
        );
        let angle = ((Vec3::ZERO - moved).normalize().dot(Vec3::Y))
            .clamp(-1.0, 1.0)
            .acos();
        assert!(
            (angle - std::f32::consts::FRAC_PI_4).abs() < 1e-5,
            "反平行退化后仍应拉回锥面（夹角 = 限位），实际 {angle} rad"
        );
        // 退化分支同样逐位确定（无随机源）
        let again = solve(
            pos_a,
            Quat::IDENTITY,
            true,
            Vec3::ZERO,
            Quat::IDENTITY,
            false,
            anti,
        );
        assert_eq!(again, r, "退化分支同输入两次求解应逐位一致");
    }

    /// 摆动关节管线（单摆 + 限位）：父端是无碰撞体锚点（只有 Transform），
    /// 子端胶囊肢给 4 m/s 横向初速 ⇒ 甩起来；240 步全程
    /// ① 锚距保持 rest 0.5 ± 1e-2（距离段生效）② 摆角 ≤ 限位 0.6 + 1e-4
    /// （锥段夹住，且**确实甩到过限位附近** ⇒ 测试非空转）③ 速度有界
    /// （无爆炸）④ 终态肢体 +Y 对准关节窝方向（方向修正在管线里生效）。
    #[test]
    fn swing_joint_pendulum_holds_rest_length_and_clamps_angle() {
        const LIMIT: f32 = 0.6;
        const REST: f32 = 0.5;
        let mut world = make_world();
        let parent = world.spawn((Transform::from_xyz(0.0, 2.0, 0.0),));
        let child = world.spawn((
            Transform::from_xyz(0.0, 1.5, 0.0),
            Collider::dynamic_capsule(0.15, 0.6, 0.0),
            RigidBody {
                velocity: Vec3::new(4.0, 0.0, 0.0),
                ..RigidBody::default()
            },
            SwingJoint {
                target: parent,
                anchor_a: Vec3::ZERO,
                anchor_b: Vec3::ZERO,
                rest_offset: Vec3::new(0.0, REST, 0.0),
                swing_limit_radians: LIMIT,
                stiffness: 1.0,
                damping: 0.5,
            },
        ));

        let socket = Vec3::new(0.0, 2.0, 0.0);
        let mut max_angle = 0.0f32;
        let mut max_speed = 0.0f32;
        for _ in 0..240 {
            step(&world, DT);
            let position = ball_position(&world, child);
            let delta = socket - position;
            assert!(
                (delta.length() - REST).abs() < 1e-2,
                "锚距应保持 rest {REST} ± 1e-2，实际 {}",
                delta.length()
            );
            let angle = (delta.normalize().dot(Vec3::Y)).clamp(-1.0, 1.0).acos();
            assert!(
                angle <= LIMIT + 1e-4,
                "摆角 {angle} rad 不得超限位 {LIMIT} rad（锥面拉回生效）"
            );
            max_angle = max_angle.max(angle);
            let speed = ball_velocity(&world, child).length();
            assert!(speed < 20.0, "速度 {speed} 应有界（无求解爆炸）");
            max_speed = max_speed.max(speed);
        }
        assert!(
            max_angle > 0.3,
            "单摆应真实甩到过限位附近（最大摆角 {max_angle}，测试非空转）"
        );
        assert!(
            max_speed > 1.0,
            "初速应真实驱动过肢体（最大速度 {max_speed}）"
        );
        // 终态：肢体 +Y 对准「子端 → 关节窝」方向（第③段在管线里的效果）
        let transform = world.get::<Transform>(child).expect("应有 Transform");
        let want_axis = (socket - transform.position).normalize();
        assert!(
            (transform.rotation * Vec3::Y - want_axis).length() < 1e-2,
            "肢体 +Y 应对准关节窝方向 {want_axis:?}，实际 {:?}",
            transform.rotation * Vec3::Y
        );
        assert!(
            (transform.rotation.length() - 1.0).abs() < 1e-5,
            "关节修正后的旋转仍须单位范数，实际 |q| = {}",
            transform.rotation.length()
        );
    }

    /// 无效目标（despawn）⇒ 摆动约束整体跳过、不 panic：挂载体照常自由
    /// 落体，与无关节对照球 **bit 级一致**（与距离关节同款语义）。
    #[test]
    fn swing_joint_with_despawned_target_is_skipped() {
        let mut world = make_world();
        let dead = world.spawn((Transform::from_xyz(100.0, 100.0, 0.0),));
        assert!(world.despawn(dead), "占位实体应成功销毁");

        let linked = spawn_ball(
            &mut world,
            Vec3::new(-5.0, 20.0, 0.0),
            Vec3::ZERO,
            0.5,
            0.0,
            0.0,
        );
        world.insert(
            linked,
            SwingJoint {
                target: dead,
                anchor_a: Vec3::ZERO,
                anchor_b: Vec3::ZERO,
                rest_offset: Vec3::new(0.0, 1.0, 0.0),
                swing_limit_radians: 0.5,
                stiffness: 1.0,
                damping: 0.5,
            },
        );
        let control = spawn_ball(
            &mut world,
            Vec3::new(5.0, 20.0, 0.0),
            Vec3::ZERO,
            0.5,
            0.0,
            0.0,
        );

        for _ in 0..60 {
            step(&world, DT);
            // 两球 x 本就不同（±5）：逐位对照只比 y（距离关节同款口径）
            assert_eq!(
                ball_position(&world, linked).y,
                ball_position(&world, control).y,
                "无效目标的摆动关节应整体跳过 ⇒ 与对照球 y 逐位一致"
            );
        }
        assert!(
            ball_position(&world, linked).y < 20.0,
            "挂载体应照常自由落体"
        );
    }

    /// 两级肢体链（父锚 → 大腿 → 小腿，各带摆动限位 + 角速度翻滚）经
    /// 240 步：两 world 的位置与旋转逐采样 `to_bits` 一致（约束轮求解序 =
    /// 收集序、旋转修正是确定性最小弧 ⇒ bit 级可复现）。
    #[test]
    fn swing_joint_simulation_is_deterministic_across_runs() {
        fn build() -> (World, Vec<Entity>) {
            let mut world = make_world();
            spawn_floor(&mut world);
            let hip = world.spawn((Transform::from_xyz(0.0, 2.0, 0.0),));
            let thigh = world.spawn((
                Transform::from_xyz(0.0, 1.5, 0.0),
                Collider::dynamic_capsule(0.12, 0.55, 0.0),
                RigidBody {
                    velocity: Vec3::new(2.0, 0.0, 0.5),
                    angular_velocity: Vec3::new(0.0, 0.0, 3.0),
                    angular_damping: 0.8,
                    friction: 0.6,
                    linear_damping: 0.5,
                    ..RigidBody::default()
                },
                SwingJoint {
                    target: hip,
                    anchor_a: Vec3::ZERO,
                    anchor_b: Vec3::ZERO,
                    rest_offset: Vec3::new(0.0, 0.5, 0.0),
                    swing_limit_radians: 1.2,
                    stiffness: 1.0,
                    damping: 0.5,
                },
            ));
            let shin = world.spawn((
                Transform::from_xyz(0.0, 0.6, 0.0),
                Collider::dynamic_capsule(0.1, 0.45, 0.0),
                RigidBody {
                    velocity: Vec3::new(2.0, 0.0, 0.5),
                    angular_velocity: Vec3::new(1.0, 0.0, -2.0),
                    angular_damping: 0.8,
                    friction: 0.6,
                    linear_damping: 0.5,
                    ..RigidBody::default()
                },
                SwingJoint {
                    target: thigh,
                    anchor_a: Vec3::ZERO,
                    anchor_b: Vec3::new(0.0, -0.43, 0.0),
                    rest_offset: Vec3::new(0.0, 0.4, 0.0),
                    swing_limit_radians: 2.0,
                    stiffness: 1.0,
                    damping: 0.5,
                },
            ));
            (world, vec![hip, thigh, shin])
        }

        let (world_a, limbs_a) = build();
        let (world_b, limbs_b) = build();
        for _ in 0..240 {
            step(&world_a, DT);
            step(&world_b, DT);
            // 跳过 hip（无碰撞体锚点，不参与物理求解）
            for (index, (&a, &b)) in limbs_a.iter().zip(&limbs_b).enumerate().skip(1) {
                let transform_a = world_a.get::<Transform>(a).expect("肢体应有 Transform");
                let transform_b = world_b.get::<Transform>(b).expect("肢体应有 Transform");
                assert_eq!(
                    transform_a.position, transform_b.position,
                    "肢体 {index} 位置应逐位一致（确定性）"
                );
                assert_eq!(
                    quat_bits(transform_a.rotation),
                    quat_bits(transform_b.rotation),
                    "肢体 {index} 旋转应逐位一致（确定性）"
                );
            }
        }
        // 非空转：链确实被驱动过（小腿离开了出生点）
        let final_position = ball_position(&world_a, limbs_a[2]);
        assert!(
            final_position.distance(Vec3::new(0.0, 0.6, 0.0)) > 0.2,
            "小腿应被初速 / 重力 / 关节真实驱动过，实际 {final_position:?}"
        );
    }
}
