# R95 RVO 群体模拟（路线图 v5 · 闭合池抽项 · 设计定稿）

> 2026-09-20。六项决策经用户裁定全按推荐定稿（AskUserQuestion 三轮），
> 主会话直接实现；收官提交 3ffc16a。撰写基础：HEAD = efd72f7
> （0.1.2 发版）之上的干净工作树；读码结论逐条对码核实（`nav.rs` /
> `buoyancy.rs` / `wind.rs` / `character.rs` / `lib.rs` 模块表 /
> `iris-render/examples` 快照组织）。

## 〇、六项裁定（各带推荐）

| # | 议题 | 推荐 |
|---|---|---|
| D1 | 算法选型 | **ORCA**（van den Berg 2011，RVO2 库口径） |
| D2 | 落点与 API 形态 | **iris-core 纯函数库**（`rvo.rs`：POD + `rvo_step` 批处理自由函数），零接线、零 ECS 组件、零引擎系统 |
| D3 | v1 范围边界 | **XZ 平面 2D；agent-agent 互避 + 静态圆障碍**；邻居全配对 + 最近 K 截断 |
| D4 | 确定性契约 | 切片序处理 + 算子集 {+ − × ÷ sqrt 比较} + LP 容差常量文档化 + 双跑 to_bits |
| D5 | 验收口径 | CPU 单测解析锚 + 长时程不变式 + example 快照 `docs/rvo.png` 双会话逐字节 |
| D6 | 文档落点 | ARCHITECTURE 新 §4.x（模拟章）+ §8 新诚实条目；CHANGELOG Unreleased |

## 一、读码结论（为什么是这个形状）

1. **iris-core 的既有三种消费范式**，RVO 对号入座第三种：
   - `NavGrid`（`nav.rs`）：**查询式纯库**——游戏喂静态几何构建、按需
     `find_path`，引擎不注册任何系统；AI 巡逻 / 路径跟随全部游戏侧。
   - `BuoyancyPlane`（`buoyancy.rs`，R93 裁定 7）：**资源即开关 + 物理
     步内 opt-in 分支**——因为浮力要作用在 `step_physics` 积分的刚体上。
   - `Wind`（`wind.rs`，R80）：**接口轮资源 + 采样纯函数**——引擎内无
     消费者，下游系统两阶段（读资源 → 写表现）自行消费。
   RVO 的对象是**群体的速度决策**，不是刚体（角色走 `move_and_slide`，
   不进 `step_physics`）；且它需要**一帧内全体协商**（每个 agent 的解
   依赖邻居的当前速度），单实体查询式不成立。正确形状 = **批处理纯
   函数**：游戏侧系统收集（BTreeMap 确定序）→ `rvo_step` → 写回速度，
   与 NavGrid / Wind 同族「引擎给纯数学，编排归消费方」。
2. **角色控制器现状**（`character.rs`）：`move_and_slide*` 五个自由
   函数收参数返回 `CharacterOutput`，位移由调用方施加——RVO 输出
   「本帧速度」恰好是它的输入语义，衔接面已经存在，零改动。
3. **存量零风险**：新模块无人调用 ⇒ 不存在「缺席 = 逐位不变」论证
   义务（无接线即无默认行为变化）；六游戏 FNV 锁原值通过是构造性
   结论。这是比 BuoyancyPlane 更强的兼容起点。
4. **确定性先例**：`wind.rs` 的 splitmix64 无状态哈希、`nav.rs` 的
   tie-break 写进文档当规范、「sqrt 优于 hypot」算子纪律——ORCA 全程
   只需 {+ − × ÷ sqrt 比较}（无超越函数），落在既有安全集内。

## 二、D1 算法选型：ORCA，拒绝两条替代

- **ORCA**（Optimal Reciprocal Collision Avoidance）：每对邻居生成一条
  半平面约束（「双方各让一半」的解析解），把偏好速度投影到约束交集
  上（增量 2D 线性规划，RVO2 的 linearProgram1–3 结构）。对穿场景
  数学上无振荡；参数少（时间视野 τ、邻居距离 / 数量上限、最大速度）。
- **拒绝基础 RVO**：对穿时双方在速度障碍两侧对称振荡，需要全局
  偏向（左右手法则）补丁——引入未定义的「世界手性」耦合；
- **拒绝单边速度障碍（VO）**：不互惠，双方全让导致死锁抖动。

## 三、D2/D3 API 与范围

```text
iris-core/src/rvo.rs
  RvoAgent { pos: Vec2, vel: Vec2, pref_vel: Vec2, radius: f32,
             max_speed: f32, neighbor_dist: f32, max_neighbors: u32 }
  RvoObstacle { pos: Vec2, radius: f32 }          // 静态圆
  RvoParams  { neighbor_horizon: f32 (=τ), obstacle_horizon: f32, dt }
  rvo_step(agents: &mut [RvoAgent], obstacles: &[RvoObstacle], &RvoParams)
  // 内部：per-agent orca_lines() + solve_velocity()（可独立单测的自由函数）
```

- **处理序 = 切片序**（文档写明：调用方应给 BTreeMap 序）；邻居选择
  = 距离升序、**相等取索引小者**（写进文档当规范，nav.rs tie-break 同款）；
- 邻居查询 v1 **全配对 O(n²) + 最近 K 截断**（500 agent = 12.5 万对，
  CPU 微秒级；broadphase 式空间加速留待真实卡点再立项，诚实入 §8）；
- 位置推进 `pos += vel·dt` 由 **调用方**施加（RVO 只决策速度——角色
  位移要过 `move_and_slide` 碰墙，引擎不该替用户走位）；`vel` 写回
  `RvoAgent.vel` 作为下帧邻居输入。

**v1 明确不做**（诚实限制候选，收官时入 §8）：线段 / 折线障碍、3D /
竖直分离、加速度连续性约束（速度瞬时改向，视觉平滑靠消费方插值）、
旋转体 / 非圆 agent、responsibility 权重（不均等让行）、流场出口
（door funneling 压力）与群聚力（flocking 吸引项）。

## 四、D4 确定性契约（写进模块头文档）

1. 同输入（含切片序）两次调用，全部 agent 的 `vel.to_bits()` 逐位一致；
2. 内核算子集合 = {+ − × ÷ sqrt 比较}，源码级看门单测禁 `sin/cos/
   atan2/pow/exp/hypot`（照 `skybox_globals_prefix` 影子契约模式）；
3. LP 容差常量（可行性判定 ε、maxSpeed 圆钳制分支）**数值写进文档**，
   视为公开契约不静默改；
4. 「先乘满后累加」分步写法防 FMA 收缩（R63 口径，门禁全走基线
   x86-64 / wasm32 不变）；
5. 无 rand、无全局状态、无时间源（dt 由调用方传）。

## 五、D5 验收硬标准

- **CPU 单测（目标 ≥ 15 条）**：
  - 解析锚：无邻居 ⇒ vel = clamp(pref, max_speed) 逐位；两 agent 对穿
    （镜像对称配置）⇒ 解互为镜像且横向分量非零；静止邻居 ⇒ 本方全让
    （让行量 ≈ 全额，对照互让各半）；静态圆障碍 ⇒ 速度偏向切线侧；
  - 长时程不变式：两群 30v30 对穿 + 静态圆 60 s 模拟，逐帧断言
    min pairwise dist ≥ Σr − 1e-4（ORCA 的无碰撞承诺）；
  - 确定性：双跑 to_bits 逐位；处理序敏感性（打乱切片序结果不同，
    证明序是输入的一部分——防「隐式不依赖序」的假象）；
  - 邻居截断与 tie-break 边界档；看门单测两条（算子禁用 + 容差常量）。
- **快照**：`iris-render/examples/rvo_snapshot.rs`（daynight_snapshot
  同款组织）——俯视机位渲染两群对穿 + 圆障碍绕行的轨迹带，
  `docs/rvo.png` 双会话逐字节一致；目检判据：两群交汇处可见分流
  廊道、无重叠团块、障碍后方无直穿。
- **门禁**：六项全绿，基线增量 = 新测试数，AGENTS.md 数字顺手更新。

## 六、工程量级

单模块新轮（iris-core 一个文件 + 一个 example），一轮收官；无基线
打破风险面（零接线），主要工作量在单测锚的解析推导与 LP 容差界定。

---

## 七、实现回写（同日，主会话实现）

六项裁定全部按推荐落地，**三处实现期语义修正**（均为实测驱动，详见
CHANGELOG「实现期归因两则」+ 附加一条）：

1. **线生成回到 RVO2 口径（安全对也生成线）**：先按 ORCA 论文「只对
   VO 内部对生成约束」实现，30v30 对穿全程逐帧实测重叠 −0.71 m——无
   预让余量的规避太晚太急。改为 RVO2 口径后（配合下两条）−0.010。
2. **法向恒取 ŵ、偏移带符号**：首稿以 unit(u) 定线法向，u 系数为负时
   （对穿安全对）法向翻转、可行侧倒置——安全对被**强制加速冲向障碍**
   （调试轨迹实证）。修正后安全对的线自带裕量 = 「接近预算上限」。
3. **重叠对走 1/dt 视野**：τ 口径的分离约束太温和（同群追尾只被轻推，
   −0.34）；改 RVO2 的一帧内摊开口径后 −0.010。
4. 另：LP3 按 RVO2 原结构实现（投影线集 + 方向优化重解，非设计稿设想
   的单遍投影）；`max_neighbors` 默认 6 → 10（对齐 RVO2 实战值，6 时
   混战约束覆盖不足实测劣化）；`rvo_step` 增加 `dt` 参数（重叠口径
   需要，设计稿漏了）。

参考实现对账：RVO2 官方仓已下架，拉取社区 Python 移植
（chengji253/RVO2-python）逐式核对 LP1/LP2/LP3 与线生成分支后定稿。
无碰撞断言口径：**离散帧浅擦界 −0.02**（上界 = 帧内闭合 2.4·dt = 0.08
的 1/4，实测 −0.0101；对照组无 RVO −0.40）——连续时间无碰撞 ⟺ 离散
帧浅擦有界，非严格零穿透，测试注释写明依据。

验收：CPU 18 项 + 快照 docs/rvo.png（1×2 对照 + 轨迹带 + FNV 双渲染
一致 + 面板差 16622 px，目检通过）；debug 基线 1792 → 1810（+18）。
