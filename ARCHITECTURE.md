# Iris Engine 架构设计

本文档阐述 Iris 引擎的设计决策、内存模型、渲染管线与跨平台策略。

## 1. 设计目标

| 目标 | 含义 | 关键决策 |
|------|------|----------|
| 跨平台 | 一份代码，桌面全平台运行 | wgpu（WebGPU 标准 API）+ winit；后端运行时探测 |
| 内存安全 | 消除 UB / 数据竞争 / 悬垂引用 | 全程安全 Rust（零 unsafe）；运行期借用检查 + 实体代数 |
| 高性能 | 面向实时 3D 的吞吐与帧率 | 稀疏集 ECS、SIMD 数学、自动实例化合批、异步 GPU 提交 |
| 低占用 | 小二进制、小内存、无运行时 | 无 GC / 反射 / RTTI 依赖；确定性销毁；LTO |
| 现代化 | 2020s 的图形与工程标准 | WGSL、ECS、derive 宏、GAT 查询、MSAA4、固定步长 |

## 2. Crate 划分与依赖方向

```text
iris-macros (proc-macro)      iris-audio（纯音频库，零 iris 依赖）
    ↑                              ↑
iris-core ──── iris-platform       │
    ↑              ↑               │
iris-render ───────┘               │
    ↑                              │
iris-engine（门面，用户唯一入口）────┘
    ↑
iris-game（下游示范游戏 I）── iris-arena（下游示范游戏 II）
          └── iris-bastion（下游示范游戏 III）── iris-ascent（下游示范游戏 IV）
                              └── iris-infiltration（下游示范游戏 V）
                                  └── iris-assault（下游示范游戏 VI）
```

- **iris-macros**：`#[derive(Component / Resource / Event)]`。通过 `extern crate self as iris_core`
  技巧让派生宏在 crate 内外都能解析 `::iris_core::...` 路径（serde 同款做法）。
- **iris-core**：不依赖任何图形/窗口库，可独立用于无渲染的服务端模拟或工具链。
- **iris-render**：只依赖 winit 类型做窗口句柄，不反向依赖 engine 门面；离屏渲染完全不触碰窗口。
- **iris-platform**：窗口配置与输入状态，薄封装 winit。
- **iris-audio**：不依赖任何 iris 内部 crate 的纯音频库（软件混音核心 + WAV /
  OGG-Vorbis 加载 + 流式音乐总线 + 音乐混响 + 程序化音效），可脱离引擎独立使用；
  混音数学全部纯函数（§4.14），解码依赖 symphonia（纯 Rust、`forbid(unsafe_code)`），
  cpal 设备层为 native 专属依赖，wasm32 下优雅降级为纯混音器。
- **iris-engine**：App 框架、插件、winit `ApplicationHandler` 主循环，把三者粘合；
  另承载**场景序列化**（§4.15——场景横跨 mesh / material / 灯光 / 物理，
  属门面层的编排职责）。
- **iris-game**：引擎的最下游消费者（示范游戏 Iris Collect，第三人称限时收集）——
  只依赖 iris-engine 门面（+ glam），是「只用公开 API 能否拼出完整游戏」的
  dogfooding 验证（§8.12 起的引擎缺口清单即来自这次实践）。纯逻辑状态机
  （`logic.rs`）只依赖 glam、零引擎依赖、全部可单测；`GamePlugin` 把
  `Input`/`Time` 翻译进逻辑并写回实体 / HUD / 音频，系统编排经
  `GamePlugin::build_schedule` 暴露，headless 集成测试与离屏快照在不建
  `App` / 窗口 / GPU 的前提下复用同一条推进路径。
- **iris-arena**：与 iris-game 同层的第二作（示范游戏 Iris Arena，波次生存
  竞技场）——同一依赖纪律（iris-engine 门面 + glam），把 dogfooding 推进到
  高强度真实用法：**蒙皮动画 × 对象池**（15 蒙皮敌人实例 + 40 弹道实体
  启动时预热、`Visibility` 待命、按逻辑侧稳定 id 取用 / 归还，零运行期
  spawn）、**根实体移动 × 蒙皮子树 × 关节动画叠加**、弹道投影体 + 追击 AI、
  Title / Playing / GameOver 多游戏状态。纯逻辑核心同样只依赖 glam（含本地
  XZ 平面静态推离，与 §3.8 的静态推离查询水平投影语义近似、刻意解耦——
  俯视玩法垂直方向无需引擎参与），插件层 Startup 把场景 `Collider` 一次性
  转换为逻辑形状、Update 每帧零查询；音效由逻辑侧单调事件计数器的差分触发。
  第二轮撞到的 API 摩擦与绕行见 §8.13。
- **iris-bastion**：与 iris-game / iris-arena 同层的第三作（示范游戏
  Iris Bastion，俯视塔防）——同一依赖纪律（iris-engine 门面 + glam），
  把 R21–R25 的五个新能力当载荷压榨：`raycast` 屏幕射线拾取（像素 →
  NDC → 引擎 `screen_ray_from_global` 世界射线打地面静态 Collider，
  刻意走引擎查询而非解析平面；拾取与 `world_to_screen` 共用
  `view_proj_from_global`，视口宽高读引擎 `Viewport` 资源）、
  `AlphaMode::Blend` 幽灵预览（红蓝着色 + 射程盘）、粒子 FX 池（发射器
  挂槽位、参数全字段覆写 + `reset()` 就地复用触发）、`play_spatial`
  事件定位音效（逻辑侧事件 `BastionEvent` 自带世界坐标）+ 听者跟随
  相机。纯逻辑核心（只 glam）含放塔校验纯函数（界内 / 空格 / 积分三分
  支单独导出）、扫掠弹道命中、路径进度索敌（打最靠前）、航点匀速推进
  与逐位确定性；塔 / 敌 / 双弹 / FX 四池启动预热零运行期 spawn。
  **同构池模式警示（dogfooding III 实测踩坑）**：资源按 `TypeId`
  寻址——同型多实例资源（敌 / 弹 / 臼炮三个同构对象池）必须是三个
  **distinct newtype**，`type alias` 只是同一类型的别名、共享 TypeId，
  后插入的会把先插入的**整个覆盖**且编译期 / 运行期均不报错（见
  `World::insert_resource` 文档的模式警示）。headless 自动玩家经鼠标
  点击全链路放塔、真实模拟 5 波全清获胜（经济 / 索敌闭环量化验证）。
- **iris-ascent**：与前三作同层的第四作（示范游戏 Iris Ascent，第三人称
  3D 平台跳跃）——同一依赖纪律（iris-engine 门面 + glam），把 R28–R31
  的四个新能力当载荷压榨：**动画状态机 / crossfade / 事件轨道（R28）**
  （双剪辑角色 GLB 程序化生成、`request_state` 双向 0.15 s 过渡、walk
  剪辑注入 footstep 事件轨道驱动脚步声）、**CSM（R29）**（方向光斜照
  ~130 m 场景，快照 + 阴影开 / 关逐像素差分作证据）、**距离 LOD（R30）**
  （22 个远景装饰挂 `LodMesh`，相机拉远 `triangles_drawn` 9 440 →
  7 324）、**运动学角色控制器（R31）**（`move_and_slide_statics` 就是
  玩法物理——纯逻辑核心只依赖 glam + 引擎纯函数，headless 与窗口同一
  模拟；跳板是游戏逻辑的一次竖直冲量而非物理）。线性上升关卡刻意设计成
  「前进 + 两个定点跳」即可通关，headless 经真实键盘注入全关通关
  （19.5 s / 宝石 14/14 / 检查点 3/3 / 坠落 0）。第四轮撞到的摩擦
  （crossfade 缺通道定格 / GLB 手写 JSON / 子实体相机平滑范式 /
  move_and_slide 衔接参数域）见 §8.18。
- **iris-infiltration**：与前三作同层的第五作（示范游戏 Iris
  Infiltration，第三人称潜入）——同一依赖纪律（iris-engine 门面 +
  glam），把 R34–R41 的新能力当载荷压榨：**NavGrid 寻路（R35）**（守卫
  巡逻 / 追逐全走 `find_path`，压力门开 → 整图「便宜重建」）、**动画 v3
  （R38）**（`BlendSpace` 参数 = 移动速度 0..1 的连续混合 + 每角色双腿链
  两骨足部 IK，脚踝贴地 ±2 cm headless 断言）、**ragdoll（R37）**（背后
  击晕 → 六球人形生成 + 立即释放，v1 不醒来；插件层逐肢体挂球体网格与
  守卫配色材质——ragdoll 只给物理原语，渲染是游戏侧的选择）、**接触
  事件（R34）**（压力板 sensor，站上板面才触发：逻辑判定半径 =
  `PLATE_RADIUS` 0.45 = 板面半宽，sensor 盒与视觉板同源同值）、**RTT（R40）**（RT
  相机俯瞰核心房 + `UiImageRtt` 画中画锚定 HUD 右上）、**UI 菜单栈
  （R41）**（Esc push/pop + 焦点导航 + `UiClickEvent` 经 PostUpdate 信箱
  消费 + `Settings` QUIT 落盘）、**距离关节（R36）**（弹簧门板被物理代理
  真实推开、`DistanceJoint` 拉回关闭位）。设施 24×18 m 四室一走廊，走廊
  刻意无守卫（3.6 m 直通区无掩体，迎面视锥无法走位避开——设计上交给
  时机）。headless 经真实键盘注入全关通关（49.5 s / 核心 3/3 / 被发现
  0，设计余量 ≤ 1）。第五轮撞到的摩擦（UiClickEvent 产出阶段 vs 事件
  清空 / FootIk 与 AnimationPlayer 强耦合 / ragdoll 需要 &mut World /
  节点 scale 污染 IK 链长 / 无隐藏子树原语 / 运动学角色无物理碰撞体）
  见 §8.22。
- **iris-assault**：与五作同层的第六作（示范游戏 Iris Assault，写实
  战斗庭院，写实路线垂直切片）——依赖纪律与其他各作一致（iris-engine
  门面 + glam；R48 曾为 R45 的 GLB 内存加载入口直依 iris-render，
  **打磨 VI 摩擦② 已把该入口补进 prelude、偏离删除**，见 §8.26②），
  把 **R42–R47 六轮新能力当载荷全量实弹**：**写实材质（R42）**（地面
  石板 / 砖墙用引擎 `brick_normal_orm`，刀身拉丝划痕 / 布料褶皱为本
  crate 纯函数生成器——splitmix64 无状态哈希 + 确定性单测；经
  `GltfBuilder::material_textured` 把法线 / ORM 通道 PNG 内嵌进 GLB，
  摩擦③ 闭合）、**DOF / MB（R43）**（战斗对焦 `focus_target` 指向
  交战目标骨，引擎解析视距——焦外敌人虚化即距离感，摩擦⑤ 闭合；MB
  覆盖演出相机 spline 运动）、
  **流式音频（R44）**（pack 内 OGG 上音乐总线循环 + 混响；8 种程序化
  音效 `play_spatial`）、**pack（R45）**（GLB + OGG 打包
  `assets/assault.ipak` 单文件、`AssetMount(Pack)` 挂载、GLB 走内存
  入口——自包含约束实弹验证）、**动画 v4（R46）**（上下身**双
  `AnimationPlayer`** 组合姿势：下半身 `BlendSpace` 步态 + 上半身动作
  剪辑 + `AnimationMask`——两层写入集不相交 ⇒ 跑动中挥砍不打断腿；
  受击 `AdditiveLayer` 抖动；ADS 双臂 `HandIk` 吸附枪身挂点，headless
  断言右手吸附 ±12 cm）、**演出时间线（R47）**（开场绕庭 Catmull-Rom
  相机 + 字幕 + 黑边，`intro_end` 事件开战；胜利 / 败北推近结算）。
  玩法：刀剑三连段（前摇 / 扇形判定窗 / 后摇 + 第三段击退）、步枪
  （射速 / 弹匣 / 换弹 / `ray_vs_shape` 弹道遮挡）、ragdoll 死亡、
  贴花战痕、粒子 FX 池（32 槽预热零运行期 spawn）、NavGrid 绕喷泉
  追击。headless 经真实键盘注入全歼通关（6.4 s / 4 杀 / HP 100）。
  第六轮撞到的摩擦（**足部 IK 回写四元数不归一化逐帧平方复合爆炸**
  / 内存加载入口不在 prelude / GltfBuilder 无纹理 / 无 Vorbis 编码器 /
  DOF 动态对焦无辅助 / 骨架下标契约全手工 / example 跨 crate 同名
  冲突）见 §8.26——**七条已在「打磨 VI」全部闭合**（①②③⑤⑥ 引擎修复
  + 游戏侧删绕行，④⑦ 文档 / 约定闭合）。

## 3. ECS 设计（iris-core）

### 3.1 数据布局：稀疏集

每个组件类型一个 `SparseSet<T>`：

```text
sparse:   [u32 实体索引 -> dense 下标]   O(1) 随机访问
dense:    [组件数据，连续排列]           遍历 = 线性扫描，缓存友好
entities: [与 dense 平行的实体标识]
```

- 删除用 `swap_remove`：dense 永远紧凑，无空洞；被交换元素的 sparse 映射同步修正。
- 与 archetype ECS 的权衡： Iris 选择稀疏集换取 **结构简单 + 组件动态增删 O(1)**；
  查询仍以首个组件的 dense 数组为迭代基准，多组件过滤通过 sparse 反查，
  对游戏常用的「少量组件类型 × 大量同质实体」场景性能足够。

### 3.2 实体代数：悬垂访问安全化

```text
Entity { index: u32, generation: u32 }
```

- 索引回收复用时 `generation += 1`；旧 `Entity` 值因代数不匹配而 `contains() == false`。
- 一切访问（`get / get_mut / 组件查询`）对失效实体返回 `None`，而非读到被复用实体的数据。
- `to_bits()/from_bits()` 支持 64 位序列化（存档 / 网络同步）。

### 3.3 借用检查：panic 而非 UB（跨线程生效）

组件 / 资源 / 事件存储都在 `std::sync::RwLock` 之后，但获取**只走尝试路径**
（`try_read / try_write`，见 `sync.rs`）：

- 多个**只读借用**可并存——包括跨线程并发读者与同线程递归读
  （查询守卫存活期间再次 `world.get::<同类型>` 是渲染器收集闭包的既有模式）；
- 可变借用与一切冲突；任何冲突**立即 panic**（消息含 already borrowed），
  绝不阻塞——由此不存在死锁路径，错误语义与 `RefCell` 时代完全一致；
- poison 标志被有意忽略（安全模型靠尝试型获取保证，不靠 poison）；
- `World::insert / despawn / insert_resource` 要求 `&mut World`，
  从 API 层禁止「迭代中做结构修改」，也使结构性修改天然成为并行调度的屏障。

这套原语让 `World: Send + Sync` 成立（静态断言测试守护），
是并行系统调度的类型层地基。

### 3.4 查询：GAT 实现零 unsafe 的多组件访问

```rust
world.query::<(&Position, &mut Velocity)>(|(pos, vel)| { ... });
```

- `QueryPart` trait 用 GAT（`type Item<'a>` / `type Guard<'w>`）把
  `RefCell` 守卫的生命周期在类型系统内闭合，**无需 unsafe 指针**即可返回
  多个 `&mut` 组件引用（不同存储互不别名，由编译器逐条验证）。
- 元组实现由宏生成至 6 元；单态化后热路径零动态分发。
- 采用回调式（而非 `Iterator`）的原因：Rust 的 `Iterator::Item` 不支持
  按借用期参数化（lending iterator），回调式让引用生命周期闭合在函数内，
  是「零 unsafe」约束下的自然选择。

### 3.5 变换层级

- `Transform` 是**本地**变换（相对父级），`GlobalTransform` 是世界变换快照，每帧由
  `propagate_transforms` 从父链复合（`parent.matrix() * local.matrix()`），只读。
- 关系存储为双向组件：`ChildOf(parent)` + `Children(Vec<Entity>)`，由
  `world.add_child / remove_from_parent` 维护；重挂自动解除旧关系。
- 容错语义：父实体销毁后子实体自动晋升为根（`GlobalTransform` 按本地值重建）；
  需要级联销毁用 `despawn_recursive`。
- 查询迭代基准需要实体本身时，`Entity` 直接实现了 `QueryPart`
  （`query::<(Entity, &Transform)>(...)`），守卫为分配器的共享借用。
- 传播在 **PostUpdate 阶段**执行（`HierarchyPlugin`，包含于 `DefaultPlugins`；
  每帧一次、不在 FixedUpdate 循环内）——编排：Update 写局部变换 →
  PostUpdate 传播 → 渲染读取，用户系统当帧写入的变换当帧即完成全局传播。

### 3.6 时间管理

- 可变 `delta`（已钳制 0.25s，防后台切回大步长）供渲染 / 视觉系统。
- 固定步长（默认 60Hz）累积器供确定性逻辑；单帧最多 5 步并丢弃积压，
  防止「死亡螺旋」（逻辑变慢 → 累积更多 → 更慢）。

### 3.7 并行系统调度

- **两种系统形态**：独占系统 `fn(&mut World)`（串行，可做结构性修改）；
  并行系统 `fn(&World)`——只读世界签名，组件写访问走内部尝试型锁
  （`world.get_mut` 等本就是 `&self` 方法），结构性修改被类型系统排除。
- **访问集声明**：`Access::new().read::<T>().write::<U>()` 声明并行系统的
  组件读写集；调度器按「一方写 ∩ 另一方读写 ≠ ∅」判定冲突。
- **贪心分批**：阶段内按注册顺序扫描，与当前批次全部成员无冲突则并入，
  否则开新批；独占系统独占一批（屏障）。批次间严格保序，批内不保证顺序。
- **执行**：`std::thread::scope` + 原子计数器自取任务，线程数取
  `min(系统数, 可用核数)`；`&World` 跨线程共享依赖 `World: Sync`（§3.3）。
- **引擎自身的并行系统**：`update_animations`（写 AnimationPlayer /
  Transform，读 Time）——DefaultPlugins 每帧真实走并行路径；Update 后的
  PostUpdate 阶段执行独占的层级传播（跨阶段天然屏障）。
- **阶段编排**：每帧 `FixedUpdate × N → Update → PostUpdate`（PostUpdate
  每帧一次、不在固定步长循环内），由 `App` 主循环驱动；Startup 在首帧前
  执行一次。
- **实测**：criterion 基准 `parallel_run_8_systems`（8 个互不冲突分量系统
  × 1 万实体，`cargo bench -p iris-core`）守护并行路径的回归；历史实测
  16 核上加速 3.27×（旧 example 版基准，已并入 criterion benches）。
  未达线性是每系统内部仍为单线程遍历 + 线程启动开销 + 内存带宽上限。
- **正确性断言**：并发观测（原子计数器峰值 ≥ 2）、冲突系统分批保序、
  独占屏障可见性，均有确定性测试；诚实性由「冲突即 panic」兜底——
  声明不诚实的并行系统会在运行期被锁检查当场击穿。

### 3.8 物理子系统（iris-core/physics + iris-core/character）

轻量刚体物理 v1（Sphere / AABB / 竖直 Capsule）：与变换层级同款的
「组件 + 单一系统」模型，零新依赖（全部数学基于 glam 手写）。

- **组件模型**：`Collider`（形状 Sphere / AABB / **竖直 Capsule** +
  `is_static` + `restitution`）与 `RigidBody`（速度 / `gravity_scale` /
  摩擦 / 线性阻尼 / `body_type`）。**组件缺席语义**：只有 `Collider`
  （无 `RigidBody`）= 静态体（与 `is_static` 取或）——不积分不移动，只作
  碰撞对象；只有 `RigidBody`（无 `Collider`）= 仅受力积分，不参与碰撞；
  两者都有且非静态 = 完整刚体。**运动学刚体（`BodyType::Kinematic`，
  打磨 V）**：游戏驱动位移（跳过重力 / 阻尼积分与速度写回，
  `Transform` 归游戏写）但参与碰撞——动体撞它按「静态方」路径被全额
  推离（inv_mass = 0），它自己不被物理挪动；与静态体（含 sensor）的
  配对**照常进接触集合**产出事件（配对随游戏驱动的位移变化，begin/end
  语义成立——运动学角色踩 sensor 压力板不再需要「动态物理代理」绕行）；
  kinematic × kinematic 与静-静同待遇跳过；全部「静态障碍」查询口径
  （`resolve_against_statics` / `sphere_sweep_statics` / `move_and_slide`
  的世界收集，统一走 `effective_static`）把它按静态障碍处理。
  **口径例外**：foot_ik 的落地查询只收**真静态体**——运动学角色胶囊常
  与自身腿链重叠，命中自身会毁掉落地语义（foot_ik.rs 文档明文）。
  **竖直 Capsule**（R31）：轴段恒沿世界 ±Y（端点 = 中心 ±
  (half_height − radius)·Y，`half_height ≥ radius` 构造断言）——角色
  控制器的标准形状；因为放进 `ColliderShape` 成为一等形状，broadphase
  （网格 AABB：中心 ± (radius, half_height, radius)）、detect_pair 配对、
  射线（圆柱 slab + 端帽球解析）、扫掠**全部自动获得胶囊支持**。
- **求解管线**（`step_physics`，系统只做编排，数值逻辑全在纯函数中）：
  两阶段收集（读 `Time.delta`，缺席视为 0；照 `update_animations` 的
  「收集 → 写回」两段式，迭代期间不持组件借用）→ 半隐式欧拉积分
  （`v += g·scale·dt`；`v ×= exp(-damping·dt)`；`pos += v·dt`）→
  **均匀网格 broadphase 候选筛选**（`BroadphaseGrid`，iris-core/broadphase：
  按「积分后、求解前」的 AABB 快照一次建网格，cell 尺寸取全部**非静态**体
  的最大轴向跨度钳制 [1.0, 50.0]——静态大地面不参与定尺寸，防 cell 撑到
  场景尺度使网格退化；无动体回退全体最大。**配对遍历序等价性**：候选
  升序 + `j > i` 过滤 ⇒ 与朴素 O(n²) 双层循环完全一致，既有 bit 级确定性
  测试无需改动即守护等价性。**必须 BTreeMap 而非 HashMap**——键字典序
  迭代与哈希种子 / 插入序 / 平台无关，是 bit 级确定性的选型前提）→
  逐对 `detect_pair` 精检（跳过静-静对）→ 响应（穿透沿法线分离，静态体不动、
  动体按动体数均分——等质量近似；法向速度按 restitution 反弹
  `v_n' = -e·v_n`，仅当朝向穿透，e 取双方 max；切向乘 `max(0, 1-friction)`，
  friction 取参与响应的动体均值——近似库仑）→ 2 次迭代位置松弛
  （`SOLVER_ITERATIONS`，速度响应只在首轮施加——重复施加会把摩擦/反弹
  多次结算）→ 写回 `Transform` / `RigidBody`。
- **约束与关节 v1（R36，位置约束集）**：`DistanceJoint`（距离约束——
  `stiffness = 1` 硬约束刚杆 / `(0,1)` 弹簧 + 轴向相对速度 `damping`）
  与 `TargetJoint`（目标点约束——把实体拉向世界固定点，`rest_distance`
  到达半径）两个组件，机关门 / 吊灯 / 弹簧板的地基。求解嵌入 `step_physics`
  松弛大循环**每轮接触之后**（顺序理由：约束优先级高于接触——先把实体
  投影回关节语义位置保结构完整，再让接触分离穿透；反过来接触推离会
  持续对抗约束、结构撕裂），速度响应只在首轮（与接触同轮同理由）；
  位置修正照常走既有写回阶段（零额外写回代码）。收集序 = 组件存储序、
  求解顺序固定（距离先、目标点后）⇒ bit 级确定性不破坏（关节场景
  单测守护）。诚实边界：位置松弛投影**无能量守恒**（弹簧是视觉弹跳
  语义，非力积分，无势能 / 共振）、双向刚杆（非「只拉不推」的绳）、
  挂载体须为碰撞体动体、target 无效（despawn）或两端重合 → 约束跳过
  不 panic（与实体世代语义一致）；**旋转约束（铰链）需角速度支持，
  见路线图**——R37 的 ragdoll 以「球肢近似 + 距离关节」绕开旋转语义
  落地（见下），旋转肢体重建仍待角速度扩展。纯函数 `solve_distance_joint` /
  `solve_target_joint` 与 `solve_contact` 同款返回 `ContactResponse`
  四通道，可脱离世界单测。
- **碰撞过滤掩码（R37）**：`Collider` 增 `groups`（所属位组）/ `mask`
  （碰撞白名单）两个 `u32` 位字段，**默认全 1**（全部构造辅助零变化，
  引入前行为逐位一致）。配对判定**对称**：`(a.groups & b.mask) != 0 &&
  (b.groups & a.mask) != 0` 才检测求解——两个方向都认可才碰撞，「a 想
  碰 b」但「b 不想碰 a」同样不碰；被过滤对与静-静对同待遇：不检测、
  不求解、**不进接触事件集合**（掩码固定的「永不碰撞」与静-静的「永不
  变化」语义一致）。**broadphase 照旧**：过滤只作用于配对白名单（逐配
  对一次位与）、不做粗筛——网格候选构建与遍历序完全不变，bit 级确定性
  保持（既有确定性测试零改动守护）。与 `is_static` / `is_sensor` / 关节
  正交：关节不看掩码（约束是显式拓扑，ragdoll 肢体链靠它连通），sensor
  照常报事件的前提是配对未被过滤。
- **布娃娃与死亡混合（iris-core/ragdoll，R37）**：「死亡混合」v1 =
  **运动学窗口两阶段**：① `spawn_ragdoll_kinematic`（`&mut World`——
  实体分配是结构性修改，分配器非内部可变）按 `RagdollDesc`（肢体球
  `RagdollLimbDef { offset, radius, restitution }` + `links` 相邻对）
  生成**静态**肢体（动画接管期——静态体挡玩家 / 弹道，游戏侧照常驱动
  `Transform` 定格死亡姿势；关节此期被跳过——挂载体须为动体）；肢体
  全部挂 `RAGDOLL_GROUP`（bit 0）组位、`mask` 排除该位 → **自碰撞免抖**
  （生成时相邻球刻意相切 / 微重叠，互相求解会被挤开炸裂；与场景照常碰
  ——世界侧默认全 1 双向认可）。② `release_ragdoll`（只需 `&World`，
  组件写锁翻转、实体失效静默跳过）把 `is_static` 翻转为 `false` +
  写入继承速度 + 可选冲量 `(方向, 强度)` 踢飞（物理接管期——关节自动
  生效，链拓扑保持）。**球肢近似（诚实声明）**：v1 纯位置物理无角速度，
  旋转铰链无法诚实实现——肢体 = 半径各异的球 + 既有 `DistanceJoint`
  硬约束（rest = 生成时实际距离，`stiffness = 1`）；球各向同性，翻滚 /
  拖拽 / 堆叠语义正确且不需要旋转；**堆叠靠地面 / 场景**（无肢体间挤压
  ——全部 ragdoll 共用组位，两具 ragdoll 也互不碰撞）；速度无钳制
  （无 CCD，冲量过大可穿墙）。确定性：生成序 = 定义序、无随机源——
  同 desc 两次全流程逐位一致；240 步倒地链不散架（链距 rest ± 容差、
  质心收敛、速度有界，单测守护）；视觉验收 `examples/ragdoll_snapshot.rs`
  → `docs/ragdoll.png`（左 = kinematic 站立定格、右 = 侧向冲量踢倒
  180 步后塌地不散架）。
- **纯函数与确定性**：检测（`detect_pair`：球-球 / 球-AABB 含盒内最近面
  退出 / AABB-AABB 最小穿透轴 / **胶囊三对**——capsule-球用轴段最近点
  （解析投影 clamp）退化到球-球、capsule-AABB 用轴段-盒**交替投影 4 轮
  收敛**（迭代近似，竖直段 × 轴对齐盒 2 轮内收敛，盒内照球-盒最浅轴
  退出）、capsule-capsule 用段-段钳制交替解法 4 轮（竖直轴恒平行））
  与求解（`solve_contact`：分离 + 反弹 + 摩擦）均为不触碰世界的纯函数，
  「穿透量 / 法线 / 反弹量」可脱离 ECS 单测。系统内部严格单线程、固定
  遍历序（收集序 = 组件存储序），同一输入两次模拟逐采样 bit 级一致
  （单测守护）——配 `Time::advance` / 固定步长即可确定性回放。
- **编排**：`PhysicsPlugin` 把 `step_physics` 注册为 Update 阶段的**并行
  系统**（访问集写 Transform / Collider / RigidBody + 读 Time；Collider
  v1 只读，属保守声明），层级传播在 PostUpdate——「物理在 Update 内求解、
  传播在 PostUpdate 收尾」，物理结果当帧即完成全局变换传播；与
  `update_animations` 访问集重叠，调度器自动分批并保持注册顺序
  （动画先、物理后，§3.7）。
- **静态推离查询（运动学角色控制器模式，游戏 × 物理库的推荐组合）**：
  `resolve_static_push`（纯函数：对每个静态体调 `detect_pair` 求
  （法线, 穿透），**顺序累加**推离、逐个用推离后的位置重测——角点的
  重复推离可接受，v1 简化）与世界封装 `resolve_against_statics`（照
  `step_physics` 的两阶段模式收集全部静态体——`is_static` 或**仅
  Collider 无 RigidBody** 的缺席语义——后调纯函数，**不写任何组件**）。
  推荐姿势：运动学实体自管移动积分（纯逻辑、可单测），物理库只回答
  「这个位置被什么挡住了」——调用方自行消费修正向量，**只取水平分量**
  即得「贴墙滑行」的角色控制器（推离只沿接触法线 → 切向速度天然保留；
  地面 ±Y 法线的水平分量恒 0，天然无作用）。iris-game 的玩家控制器即此
  模式：`logic::step` 保留移动积分与 ±arena_half 钳制安全网，场景文件的
  墙体 / 障碍柱 Collider 负责实际阻挡（§8.12）。
- **射线 / 线段查询（视线 · 拾取 · 高速命中）**：`ray_sphere`（几何法解
  二次方程）/ `ray_box`（slab 法；平行轴「起点在界内则不裁剪」；盒内起点
  取最浅出面外法线、角点斜射法线取主轴——退化情形全部写明确定性选择）→
  `ray_vs_shape`（分发 + `t ≤ max_t` 裁剪）→ `segment_hits_shape`（线段
  便捷封装：`dir = to − from`、`max_t = 距离`——高速离散体每帧位移可大于
  目标直径，当帧点判定会穿模漏检，线段扫掠判定是修法；iris-arena 的弹道
  命中即此语义的 XZ 本地实现）。世界级 `raycast` / `raycast_filtered`：
  遍历全部 `(Transform, Collider)`（静态动态都命中）取最小 `t`，`t` 相等
  取先遍历者（组件存储序——确定性）；`filter` 闭包只收 `Entity`（迭代期间
  持有读借用，不得访问世界）。纯查询：不写任何组件。
- **球扫掠 shapecast（角色控制器地基）**：`sphere_sweep`（纯函数：保守
  推进二分 24 次收敛，复用 `detect_pair`；「终点重叠、起点自由」为检出
  括弧——返回首次碰撞的位移分数 t∈[0,1] 与阻挡法线【从静态体指向扫掠球】，
  「无重叠侧」边界减 epsilon 收缩留皮肤；穿透 ≤ `SWEEP_CONTACT_EPSILON`
  （1e-6，静置贴合面的 f32 舍入名义穿透 ~1e-8）不算重叠——否则贴地角色
  会被误判「起点已重叠」原地卡死）与 `sphere_sweep_statics`（世界静态体
  收集封装，缺席语义同 `resolve_against_statics`）。任意形状泛化
  `shape_sweep`（R31）同款算法供胶囊扫掠。检出范围的诚实边界（起点与
  终点都自由的完全贯穿返回 `None`，调用方按需分步）写在函数文档。
- **接触事件与触发器（R34）**：`step_physics` 帧末把本帧接触对与
  `ContactTracker` 资源的上一帧集合差分，产出 `CollisionStarted` /
  `CollisionEnded` 事件（压力板 / 警报区 / 着地回调免逐帧轮询）。语义
  要点：**实体序归一化**（事件 a/b 恒 `a.index() ≤ b.index()`，检测原始
  方向与索引序相反时 normal 随归一化翻转，保持「从 a 指向 b」的方向
  约定）；**首求解轮采样**（接触对与 normal/penetration 取自
  `SOLVER_ITERATIONS` 首轮检测、位置分离前——松弛轮的重复检出对是同一
  配对的求解残影，无 begin/end 语义）；**静-静跳过**（静-静配对永不
  变化，不进接触集合不产事件，与求解同分支）；**销毁成对性**（接触中
  实体 despawn / 移除组件 → 下一帧必收到携带已死实体 id 的 Ended——
  本帧集合只含存活实体的检出对，差分必然筛出消失配对，begin/end 严格
  成对、世代安全）；**sensor 正交性**（`Collider::is_sensor` 触发区域与
  `is_static` 正交：sensor 对照常进接触集合报事件、但跳过位置/速度求解
  ——不推离不反弹，两动体 sensor 相撞只报事件）；**确定性**（追踪集合
  用 BTreeSet 与 broadphase 同款选型理由，事件先 Started 后 Ended 按对序
  输出，同输入两次模拟事件序列逐项相等——单测守护）；**资源缺席语义**
  （`ContactTracker` / 事件资源未注册则静默跳过，照 `Time` 缺席风格，
  `PhysicsPlugin` 已全部注册游戏侧即得）。消费契约（打磨 V 起）：事件
  在 `step_physics` 帧末产出、当帧其后注册的系统即可读；每帧**恰好消费
  一次**且必须 `world.events_mut::<E>().drain()` 取走（事件可读窗口
  覆盖产出帧与下一帧——只 `iter` 不 `drain` 会在下一帧重复看到同一
  事件、begin 被重复计数；总纲见 §3.10）。
- **运动学角色控制器（iris-core/character，R31）**：`move_and_slide` /
  `move_and_slide_statics`（纯函数核心，静态体列表入参可脱离世界单测）——
  竖直胶囊 + 五步管线：① 重力（`v.y += g·gravity_scale·dt`）；② 水平
  **最多 4 次扫掠迭代**：形状扫掠求首次碰撞 → 推进到命中点 → 速度与剩余
  位移切掉法向分量（法线水平归一；**可站立坡面不切速度**，避免水平推力
  转成沿坡上滑分量）→ 贴墙滑行；③ 台阶「抬起重试」：`vel.y ≤ 0` 且脚下
  snap 内有地面且上方畅通才抬升 `step_height`，前向位移完整走完 + 向下
  扫掠落回 + 落点法线可站立才接受——不可站立阻挡（台阶迎风面）前向距离
  至少 `radius`（中心送进台面落到平顶，贴台缘角点法线倾斜永不成立），
  可站立坡面按剩余位移原样（缓坡上坡 = 逐帧低台阶）；④ 斜坡限：垂直向下
  扫掠（探测距离 `max(|vel.y·dt|, snap_distance)`，即**贴地吸附**并入同一
  次扫掠）命中：法线 `n.y ≥ cos(slope_limit)` → grounded、`vel.y = 0`；
  过陡 → 不 grounded、速度切掉入坡法向（沿坡下滑分量保留，重力逐帧沿坡
  加速自然下滑）；⑤ `vel.y > 0`（跳跃）不吸附，向上扫掠命中天花板贴住。
  输出 `CharacterOutput { position, velocity, grounded, ground_normal }`
  由调用方写回组件。诚实边界：起点嵌入不去穿（出生点须合法）、一步一台
  阶、全贯穿不检出。
- **v1 限制（诚实清单）**：
  1. **AABB 不随实体旋转**：`ColliderShape::Box` 是世界轴对齐包围盒，
     旋转实体的碰撞盒不跟着转（物理体应使用不旋转的实体，或把旋转只给
     表现子实体）；**胶囊同理恒竖直**：`ColliderShape::Capsule` 轴段固定
     沿世界 ±Y、不随实体倒下——斜置胶囊（横倒圆柱）请用球串或 AABB
     近似；
  2. **物理体须为根实体**：`step_physics` 把 `Transform.position` 当世界
     坐标读写，与层级并用时须自行保证物理体无父级（或父级变换恒等）；
  3. **非迭代完整求解器**：等质量冲量 + 2 次位置松弛，重堆叠会松弛不
     彻底（残余穿透 / 缓慢滑移），不是完整 LCP 求解器；
  4. **粗检测为 AABB 网格快照**：均匀网格按「积分后、求解前」的 AABB 一次
     构建，求解期位置修正（穿透量级）不重建——常规场景修正量远小于 cell
     尺寸、候选为保守超集（LCG 生成 ~200 AABB 的单测守护）；极端穿透
     （接近 cell 尺寸）理论上可能漏对；
  5. **胶囊最近点为迭代近似**：capsule-AABB / capsule-capsule 的最近点
     用交替投影 4 轮收敛（竖直轴配对下 2 轮内即精确），非解析解但同输入
     恒同结果；角色控制器的台阶前跳（不可站立阻挡时前向 ≥ radius）带来
     单帧至多一个 radius 的水平速度红利。

### 3.9 导航网格与寻路（iris-core/nav，R35 → R49 高度场语义）

静态场景的导航基础设施：**高度场语义的 3D 地形导航图** + A* + 路径
平滑，终结「硬编码航点敌人」。纯数据结构（无系统、不进 DefaultPlugins），
`NavGrid` 实现 `Resource` 可整图存入 World；AI 行为结构（巡逻 / 状态机）
留游戏侧。语义规范全文 = `iris-core::nav` 模块文档（含 R49 升级主线 /
v1 诚实清单），本节是架构级摘要。

- **高度场语义（R49 主线）**：每 cell 一个**可站立表面高度**
  （`surface_height`）。`NavGridParams` 新增 `step_height`（默认 0.3，
  与角色控制器 `CharacterParams::step_height` 同值对齐——导航允许的
  落差 = 角色一步能迈的坎）与 `max_slope_radians`（默认 45°）。逐 cell
  中心做**覆盖体顶面解析**：Box 顶面恒可站（表面高 = 顶 y）；Sphere /
  竖直 Capsule 取上端帽面（表面高 = 帽心 y + √(r²−ρ²)，**法线坡度 ≤
  max_slope 才可站**——等价 ρ ≤ r·sin(max_slope)）；重叠取最高顶面
  （等值取遍历序先者，只保确定性）；顶面在隐式地面（y = 0）之下的
  埋没体不覆盖（**大埋深球 = 露出地面的丘**）；无任何覆盖 → 隐式地面
  y = 0（导航域架设在 0 平面；场景地板 ≠ 0 时喂 Box 顶面或平移场景）。
  覆盖情形不回落地面（浮空体下方 / 陡面下方不可走——多层留白）。
- **光栅化判定（双层 + 可达顶面雕刻）**：① 覆盖格 = offered（可站立
  顶面）或被阻（顶面坡度超限）；地面格被阻 ⇔ 中心到某体 XZ 投影最近
  距离 ≤ `agent_radius` **且侧障高度 > `step_height`**（矮体 / 缓丘
  可迈不设防；最近点判定、相切算阻、Sphere/Capsule→圆、Box→矩形圆角
  膨胀、`cell_size` 应 ≤ `agent_radius` 经验规则——R35 语义原样）；
  ② **可达顶面雕刻**：|Δh| ≤ `step_height` 的 8 邻域边链与地面连通
  的顶面格从 blocked 释放（确定性 BFS；对角步进要求两正交邻潜在可走
  且高度互差 ≤ step——防穿角高度化）；孤岛高台（离地 > step）保持被阻。
- **兼容性**：既有测试与两个游戏的世界全是「底贴地、顶离地 >
  step_height」的立墙形态 → 双层判定与 R35 柱体位图**逐位一致**、雕刻
  为空、可走格高度恒 0 → 边权 / 路径 / 航点全不变（既有 nav 测试零
  改动全过 = 验收）。
- **寻路（`find_path` 五步语义）**：① 端点越界 → `None`；② 水平距离
  < 1e-6 → `Some(vec![to])`；③ 就近可达性修正（被阻端点 8 邻域 BFS
  半径 ≤ 2 cell，穿过被阻格在所不辞）；④ **A\***：直线 1.0 / 对角 √2、
  **攀爬惩罚**（上坡边 ×(1 + 2·Δh/step_height)——爬台阶贵于绕平路）、
  对角两正交邻必须 free（防穿角）、两端 |Δh| ≤ step_height；启发 =
  octile（惩罚使边代价 ≥ 纯步距 ⇒ **可采纳且一致**，首弹 goal 即加权
  图最优）；⑤ **平滑**。**航点带 y**：逐 cell 航点 y = 表面高度，末
  航点 = 目标 XZ + 目标 cell 表面高（游戏侧无需再覆写）。
- **确定性 tie-break**（bit 级确定性的核心论证）：open 集 =
  `BinaryHeap<Reverse<((f, h), 节点序号)>>`——f32 无 `Ord`，用
  `total_cmp` 全序（全程无 NaN：g ≥ 0、h ≥ 0 有限）；比较序 f 升 →
  h 升 → 节点序号（行主序 cell 索引 = 栅格扫描序）升；g/parent 只在
  **严格更优**时写入（并列保留先到者）。弹出序由全序唯一决定；堆内
  完全相等的重复条目携带的状态转移相同（closed + 严格更优检查使二次
  处理幂等），弹出先后不影响结果。高度解析 / 雕刻的门条件只读静态
  数据、等值顶面取遍历序先者——同输入两次 build + find_path 逐位一致
  （混合地形单测守护）。
- **平滑（string pulling + 3D 视线）**：从锚点尽量远找视线畅通的最后
  一个航点；视线测试 = **cell 光栅 DDA（R35 原样）+ 高度场逐格校验**
  ——逐格查 blocked，并在线段进入该格的插值 y 上做 `|y − 表面高| ≤
  step_height` 校验（cell 内 |y − h| 的最大值必在进入 / 离开边界 →
  按 DDA 步进点采样是数学等价的完备采样；容差 ≤ step = 角色一步可迈
  的起伏，更大穿台 / 悬空被挡下折点保留）。选光栅 + 边界点采样而非
  2D 投影 / 均匀大步长采样的理由：与 blocked 判定同源（无二次膨胀不
  一致）、O(穿越 cell 数)、逐位确定、不会漏窄高台。平滑后点数与总长
  均 ≤ 平滑前（单测锁定）。
- **可达性轻量版**：`is_reachable` 同端点语义 + 与 A* **同边集**的
  BFS——find_path 为 `Some` ⇔ is_reachable 为 `true`，不构造路径更便宜。
- **动态障碍策略（v1）**：纯静态 + **便宜重建**——`build` 毫秒级纯
  函数，场景变化时或按固定节拍（0.5~1 s）整图重 build 换入（Resource
  原子替换）；**不做增量更新 / 单元标记**（v1 限制，见 nav.rs 模块
  文档诚实清单：多层（桥洞 / 上下叠层）留白、|Δh| > step 的落差不可
  跨（无跳跃 / 无自由落体）、隐式地面 0、无增量、窄障碍可漏、修正
  半径 ≤ 2 cell、最优性仅 cell 图上）。

### 3.10 事件生命周期与命令队列（打磨 V）

**事件生命周期（双代轮换）**：v1 是「单 Vec + 帧尾清空」——PostUpdate
（焦点 / 点击系统所在）产出的事件在帧尾被无条件清空，下一帧 Update 永远
读不到（§8.22①，游戏被迫用 `MenuCommand` 信箱资源绕行；`AppExit` 更是
帧尾退出检查在清空**之后**读、恒读空——QUIT 根本退不出）。打磨 V 改为：

- **`Events<E>` 双代缓冲**（`previous` + `current`）：`send` 写入
  `current`；读取（`iter` / `len`）见两代并集（上一代在前，产出时间序）；
  `drain` 取走两代全部。**可读窗口 = 产出帧余下部分 + 下一帧全程**；
- **帧首轮换**：主循环把 `clear_all_events` 从帧尾移到帧首
  （`run_update_stages` 之前），语义改为轮换——「上一代丢弃、本帧缓冲
  降级为上一代」。于是 PostUpdate 产出的事件本帧 Update 全程可读
  （跨阶段投递不再需要信箱），帧尾退出检查读本帧产出（Update 与
  PostUpdate 发的 `AppExit` 都能退出）；被丢弃的只是连续两帧未被消费
  的残留，不无限累积；
- **消费契约**：`iter` 非破坏性（窗口内每次读取可见，不 drain 时上帧
  事件会再出现一次）；`drain` 即消费。每条事件流每帧**恰好一个 drain
  方**——接触事件等 begin/end 计数型消费方必须 drain（iris-infiltration
  已改）。`events_mut` 放宽为 `&World` 可用（照 `try_send_event` 的写锁
  先例）——并行系统可以 drain；
- 编排级测试：PostUpdate 发事件 → 下一帧 Update 恰好读到一次（app.rs）。

**命令队列（`iris-core/commands`，最小实现）**：并行系统（`fn(&World)`）
无法 spawn / despawn（结构性修改需要 `&mut World`）——v1 游戏被迫插
独占系统屏障做运行期 spawn（§8.22③）。`CommandQueue` 资源（内部
`Mutex<Vec<Command>>`，`Command = Exec(闭包) | Despawn(Entity)`）：

- **入队**：`spawn_deferred(f)` / `despawn_deferred(e)`，`&World` 即可
  （`Mutex` 内锁，照事件写锁先例）；独占上下文用 `World::command_queue()`
  （缺席自动注册），并行系统用 `try_resource_mut::<CommandQueue>()`
  （`CommandsPlugin` / `DefaultPlugins` 已注册）；
- **执行**：`apply_commands` 独占系统注册在 **PostUpdate 尾部**（渲染
  之前——本帧产出的命令本帧生效，渲染读到最终实体态）；实现为「短命
  资源锁取走命令 → 逐条按**入队序**执行」（执行期间新入队的命令留给
  下一帧——**单遍语义**，防闭包自激励无限循环）；
- **确定性警示（诚实清单）**：命令按入队序执行（实体分配序确定），但
  闭包的执行结果依赖执行时的 World 状态——确定性回放场景请把「状态差」
  编进命令数据，慎用闭包捕获可变外部状态（`iris-core/commands` 模块
  文档明文；iris-infiltration 的 ragdoll 命令只捕获确定性的值）。

## 4. 渲染管线（iris-render）

### 4.1 帧流程

```text
World 查询 (Transform, MaterialHandle, MeshHandle | LodMesh)
  → LOD 网格解析：挂 LodMesh 的实体按实体原点到相机 eye 的欧氏距离
    选 high / low（§4.19），未挂载用 MeshHandle 旧语义
  → 实例数据 InstanceRaw（模型矩阵 + 法线矩阵，112B/实例）
  → 按 alpha_mode 分区：不透明 → (网格, 材质) 稳定排序成批；
    透明 → 视深降序（远 → 近，见 §4.16）
  → 实例缓冲跨帧持久：与上帧镜像比对后全量 / 变化段增量 / 零上传（见 §4.2）
  → 阴影 pass（可选）：光源正交视角，深度写入 R32Float 颜色附件（2048²），
    只画不透明实例区间（透明不投影，见 §4.16）
  → 点光阴影（可选）：首盏 shadows 点光的 cubemap，6 面 × 512² 各一个
    render pass，R32Float 颜色附件存归一化线性距离（见 §4.7.1）
  → RT pass（可选，R40：有 ActiveCamera+RendersTo 时才执行，零开销默认路径）：
    每台 RT 相机渲一次到其离屏纹理（不透明+蒙皮+透明，复用主阴影图/CSM、
    无天空盒/贴花/粒子）→ ACES blit 到 RT 颜色纹理（无后处理链，见 §4.21）
  → 主 pass：天空盒背景（inv_view_proj 射线采样预滤波环境 0 级 mip，深度只读）
    → 不透明几何渲染到线性 HDR 中间缓冲（Rgba16Float + 4x MSAA → resolve），
    环境光采样初始化时烘焙好的 IBL 贴图（辐照度 / 预滤波 / BRDF LUT），
    深度附件 Store（供贴花 / 透明 pass 遮挡测试）
  → 贴花 pass（可选，无贴花实例零开销，R39）：盒投影端盖代理几何实例化，
    同一 HDR MSAA 目标 LoadOp::Load 继续 alpha 混合，深度只测不写（LessEqual），
    按纹理分批（见 §4.20）
  → 透明 pass（可选，无透明实例零开销）：同一 HDR MSAA 目标 LoadOp::Load
    继续 alpha 混合，深度只测不写（见 §4.16）
  → 粒子 pass（可选，无活粒子零开销）：billboard 实例化绘制（additive /
    alpha 双管线），叠加在透明结果上、位于泛光之前（见 §4.17）
  → 后处理链（见 §4.11）：
    泛光（可选）：亮度提取 → 高斯模糊（半分辨率 ping-pong 三轮分离迭代）
    → 暗角（可选，全分辨率 HDR → HDR 压暗四角，强度 0 整段跳过）
    → blit pass：叠加泛光 + 曝光 + ACES 色调映射 → sRGB 目标（链的固定终点）
  → UI 叠加层（可选）：UiRect / UiText 顶点合批单 draw，LoadOp::Load
    叠加于同一 sRGB 目标（不参与 HDR/后处理/色调映射，见 §4.13）
  → queue.submit（异步）→ present
```

### 4.2 自动实例化批处理

- 排序键 `(mesh_id << 32) | material_id`，排序后相同 key 天然连续，
  渲染器无需预知场景即可把 **同网格同材质的所有实体压成一个 draw call**。
- 实测：4096 实例 + 地面 = 2 draw call（见 `instanced` 示例与集成测试断言）。

**跨帧持久化与增量上传**：实例缓冲（`iris.instances`）不因帧切换失效——
上一帧上传的内容保留在 CPU 镜像 `last_instances` 中，本帧实例数组组装完成后
逐实例比对（112B/实例，含蒙皮与透明段），按结果三分决策：

- 实例缓冲刚重建（容量不足整体扩容）或实例数与上帧不同 → **全量上传**
  （重建后旧缓冲内容作废，镜像失效）；
- 逐项完全一致 → **整帧跳过**（静态场景零上传）；
- 存在变化 → 只对**连续变化段** `write_buffer`（offset = 段起始实例 × 112B；
  wgpu 要求 write_buffer offset 4 字节对齐，112 = 4×28 天然满足）；
- 变化实例数超过总数 50%（阈值常量 `INSTANCE_DIFF_FULL_FALLBACK_RATIO`）→
  **回退全量**：大量碎段的驱动提交 / 编码开销会反噬省下的拷贝带宽。

实现要点：

- 比对为纯函数 `diff_changed_ranges(prev, next) -> Vec<Range<usize>>`
  （renderer.rs，模块单测覆盖全等 / 单点 / 首尾分离 / 相邻合并 / 长度不等 /
  空到非空）。
- 镜像零拷贝维护：上传决策完成后 `mem::swap(last_instances, 组装 scratch)`
  ——镜像取得本帧数据，上帧镜像连同其堆分配交还 scratch，下帧
  clear + extend 直接复用容量，实例数据全程无额外拷贝。
- 实际上传字节数经 `FrameStats::instance_upload_bytes` 观测（跳过帧 = 0），
  集成测试 `instance_upload_render.rs` 断言静态场景连续帧零上传且像素一致、
  单实体移动仅传变化段（字节数 < 全量）。

### 4.3 着色与颜色管线（PBR + HDR）

- **Cook-Torrance PBR**（`standard.wgsl`，金属度-粗糙度工作流）：
  法线分布 GGX、几何遮蔽 Smith-Schlick（k = roughness²/2 直接光近似）、
  菲涅尔 Schlick（绝缘体 F0 = 0.04，金属 F0 = albedo）；
  能量守恒 `kd = (1 − F)(1 − metallic)`，金属无漫反射。
- **照明**：环境光（**IBL 分割求和**，见 §4.12；另有方向光资源上的常量补光项）+
  1 个方向光（受阴影调制）+ 最多 8 个点光（查询顺序中的首盏可投射
  cubemap 阴影，见 §4.7.1）。点光采用**物理平方衰减**
  `window² / d²`（`window = 1 − (d/range)²`），在有效半径处平滑归零。
- 法线矩阵在 CPU 端取模型矩阵的**逆转置 3×3**，保证非均匀缩放下光照方向正确。
- **HDR 管线**：主 pass 渲染到线性 `Rgba16Float` 中间缓冲（MSAA 4x 解析），
  后处理链（§4.11）的 blit pass 叠加泛光、乘曝光后做 ACES 色调映射
  （Narkowicz 解析近似）输出 sRGB——
  光照在线性 HDR 空间累加，高光滚降由 ACES 完成，不会出现死白截断。
- WGSL 结构体对齐陷阱：`vec3<f32>` 在 WGSL 中对齐 16 字节，会把只含一个 vec3 的
  uniform 结构撑到 16 字节倍数；Iris 用 3 个独立 `f32` 填充保持与 CPU 侧 48 字节一致
  （这是 GPU 编程常见坑，集成测试曾实际捕获该问题）。
- **法线 / ORM 材质通道（R42）**：`MaterialDef` 新增 `normal_texture`
  （切线空间法线）与 `orm_texture`（R=AO / G=粗糙度 / B=金属度打包）两个
  `Option` 槽位，均线性数据贴图（`CpuTexture.srgb = false` → 上传
  `Rgba8Unorm` / `Bc1RgbaUnorm`，颜色数据仍 sRGB——色彩空间随数据走）。
  **兼容硬约束**：槽位缺省 `None` 时渲染器绑常驻中性纹理（法线 =
  (0.5, 0.5, 1) 的 1×1 `Rgba16Float`——8-bit unorm 表示不了 0.5，16-bit
  float 精确；ORM = 白色兜底纹理采样恒 1.0），着色器零按材质分支，渲染
  结果与 R41 **逐位一致**：扰动式 `T·nx + B·ny + N·nz` 的 TBN 第三列取
  **未归一化**插值世界法线，中性采样 (0,0,1) 代入逐位还原该法线
  （`x·0 = ±0`、`±0 + x = x`），后续 `normalize` 输入串位不变；
  `x·1.0 = x` 保证 ORM 中性值下金属度 / 粗糙度 / AO 与标量路径逐位相同。
  该不变量由 `tests/normal_render.rs` 的 R41 基线全帧 FNV-1a 像素哈希锁定。
- **TBN 两路（R42）**：网格带 glTF TANGENT 属性（`MeshData.tangents`，
  独立顶点缓冲 loc12，w = 手性符号；`GltfBuilder::mesh` 同步导出）时走
  顶点切线路（世界空间 Gram-Schmidt 正交化，精度首选）；无切线网格由
  渲染器垫常驻零缓冲（xyz 全零 = 标记，1 MiB 共享垫，超限网格单独分配），
  片元用 `dpdx/dpdy` 屏幕空间导数解 2×2 方程组重建 TBN（手性由解的几何
  关系自然给出）——免 CPU 预计算与逐网格 16 B/顶点，代价是像素级导数噪声。
  AO 只乘环境项（IBL + 常量补光），直接光不乘；粗糙度 / 金属度 =
  标量因子 × 贴图通道（glTF factor × texel 语义），无 ORM 时退化为标量。
  glTF 解析：`normalTexture` 直挂线性槽；`metallicRoughnessTexture` 与
  `occlusionTexture` 合并进同一张 ORM 打包图（同下标直接引用、异下标且
  同尺寸 CPU 通道合成 R←AO、仅 occlusion 时 G/B 填 255 交标量因子；
  `strength` v1 忽略）。纹理去重缓存键含色彩空间（同一张图可同时被
  baseColor sRGB 槽与数据线性槽引用）。

### 4.4 视锥剔除

- Gribb-Hartmann 方法从 view_proj 提取 6 平面（适配 z ∈ [0,1] 的 DirectX/WebGPU 深度
  约定：near = row₄，far = row₄ − row₃）。
- **平面必须按法线长度归一化**后才能与世界单位的包围球半径比较——未归一化的
  far 平面法线长度只有 ~0.001，比较结果毫无几何意义（单测实测抓出过该问题）。
- 剔除对象为「变换后的包围球」：局部 AABB 的中心/半径经模型矩阵变换，
  半径按矩阵三列长度的最大值保守放大；不可见实体不进入批处理与实例缓冲。
- **Visibility 过滤与层级级联（打磨 III）**：渲染收集查询带
  `Option<&Visibility>`（**组件缺席 = 可见**），自身 `HIDDEN` 或
  **任一祖先 `HIDDEN`** 的实体在全部收集路径跳过——级联实现为**收集期
  沿 `ChildOf` 父链上行遍历**（每实体 O(父链深度)，典型 ≤ 4；不加新
  组件、不动 `hierarchy` 数据），普通 / 蒙皮 / 透明（含透明蒙皮）三条
  几何路径与 UI、粒子发射器同语义；可见性过滤不计入
  `culled_instances`（口径见 §4.4.1）。GPU 回归测试
  `culling_render.rs::hidden_ancestor_cascades_to_descendant_subtree`
  守护「根 rig → 无网格夹层 → 孙子网格」三层结构（iris-bastion
  draw calls 139 事故的引擎级回归锁）。
- 渲染器通过 `FrameStats` 暴露剔除后的提交数，集成测试据此断言
  「相机背后 27 实体零提交、视锥内实体不误伤」。
- **蒙皮保守剔除（R22）**：蒙皮实体此前「保守全收」（注释自认关节可把
  顶点移出绑定姿势包围盒）。现改用**关节矩阵变换后的角点并集 AABB**：
  绑定姿势 AABB 的 8 个角点经**每个**关节矩阵 Jᵢ 变换到当前网格局部、
  再经实例 model 矩阵到世界，全部（关节数 × 8）点的并集 AABB 与视锥做
  `intersects_aabb` 测试。**数学保证只多画不漏画（凸组合论证）**：
  LBS 顶点 v′ = model·(Σᵢ wᵢ·Jᵢ·v)，权重非负且和为 1（加载时归一化）
  ——蒙皮后顶点落在各刚体变换结果 {Jᵢ·v} 的**凸组合**内，凸组合必属于
  凸包，而凸包又含于「逐关节变换后 8 角点并集的 AABB」（Jᵢ 仿射，绑定
  AABB 的像必落在其变换后角点的包围盒内）。代价：每蒙皮实例
  关节数 × 8 次点变换 + 一次 AABB 相交测试（Arena 15 只蠕虫 × ~5 关节
  量级，CPU 可忽略）；网格无 CPU 数据 / 空包围盒时退化为不剔除（宁可
  多画）。反误杀回归（`culling_render.rs`）：关节把几何刚性移到 +X 10 m、
  绑定 AABB 仍在原点——相机只看向 (10, 0, 0) 时误用绑定包围盒必误杀，
  测试要求该实例可见且画面出现蒙皮几何像素。

### 4.4.1 FrameStats 字段口径与剔除基准

| 字段 | 口径 |
|---|---|
| `draw_calls` | 主几何（不透明主 pass + 贴花 pass §4.20 + 透明 pass，§4.16 + 粒子 pass，§4.17）：普通批每批 1 + 蒙皮每实体 1 + 贴花每纹理批 1 + 透明 run 批每批 1 + 透明蒙皮每实体 1 + 粒子（additive / alpha 段各至多 1）+ UI 顶点非空时 1；阴影 / 天空 / 后处理 pass 不计 |
| `instances` | 主几何提交的**可见**实例数（普通 + 蒙皮 + 贴花 + 透明普通 + 透明蒙皮 + 粒子合计） |
| `culled_instances` | 被视锥剔除的实例数（普通 + 蒙皮 + 透明 + 贴花合计；`Visibility` 过滤——自身 `HIDDEN` 与祖先级联——不算剔除）。**粒子不进本口径**——剔除按发射器包围球整批裁决，被剔的发射器既不进 `instances` 也不进本字段（§4.17） |
| `triangles_drawn` | 主几何提交三角形总数：可见项索引数 / 3 累加（普通批 × 批实例数、蒙皮逐项、贴花每实例 4 三角形 = ±Z 两端盖、透明 run × 其实例数、粒子每粒 2 三角形；网格目录缺席按 0 不 panic）。不含 UI 与阴影 pass——阴影 pass 只复用不透明实例区间，贴花 / 透明项与粒子不投影 |
| `instance_upload_bytes` | 几何实例缓冲实际 `write_buffer` 字节数（0 = 整帧跳过上传，见 §4.2）。粒子与贴花实例走独立缓冲、每帧全量上传，不计入本字段（§4.17 / §4.20） |
| `particles_drawn` | 粒子 pass 提交的粒子实例数（发射器包围球剔除后；已并入 `instances` / `triangles_drawn`，其 draw call 已并入 `draw_calls`）。与发射器池状态对账：= 各可见发射器活粒子数之和（§4.17） |

**剔除基准（`culling_bench` 示例，实测记录）**：5000 立方体（100×50 网格，
同网格同材质单批），双朝向各 300 帧（前 50 帧预热丢弃），1280×720 离屏。
以下数字来自**开发机**（Windows 10 / DX12 后端 / release 构建），受机器
与驱动影响，仅供量级参考：

| 朝向 | instances | culled | draw_calls | triangles_drawn | draw() CPU 平均 / 最坏 |
|---|---|---|---|---|---|
| A 面向全场（高可见） | 4730 | 270 | 1 | 56 760 | ~0.52 / ~0.99 ms |
| B 背对望空（高剔除） | 0 | 5000 | 0 | 0 | ~0.36 / ~0.80 ms |

读法：全部剔除后 `draw` 的 CPU 耗时退化为「收集 + 5000 次包围球 / 并集
AABB 剔除判定」本身（~0.36 ms）；静态场景两朝向 `instance_upload_bytes`
均为 0（跨帧增量上传，§4.2）。计时只包 `Renderer::draw`（离屏读回不计，
走 `OffscreenRenderer::draw_frame`）。

### 4.5 实体挂载相机

- 相机有两种存在形态：全局 `Camera` 资源（简单场景）与挂在实体上的
  `ActiveCamera` 组件（跟随层级运动，第三人称 / 载具视角）。
- 渲染器每帧优先查询 `(&GlobalTransform, &ActiveCamera)`：eye 取实体世界位置，
  视图朝向取实体局部 **-Z 前方、+Y 上方**（`view_from_global`）；
  无挂载实体时回退到 `Camera` 资源，两条路径的投影参数（fov / near / far）语义一致。
- 挂载相机会被视锥剔除与光照 uniform 复用（eye 位置同时供高光计算）。
- **屏幕射线反投影（打磨 III）**：`Camera::screen_ray(ndc, aspect)` /
  `screen_ray_from_global(gt, cam, ndc, aspect)` 把 NDC 点映射回世界
  射线 `(origin, dir)`——inv(view_proj) 对近 / 远平面（DirectX 深度
  z = 0 / 1）各取一点、两点连线即过眼点的针孔射线（透视除法奇异时
  方向归零，由调用方兜底）。正向投影 `view_proj_from_global` 公开同一
  份 vp 拼装（DirectX 深度约定与渲染器同款）——拾取、`world_to_screen`
  与渲染器共用同一几何事实，引擎外不再手工拼矩阵。视口宽高比由调用方
  给出：窗口程序读 iris-engine 的 **`Viewport` 资源**（物理像素，与
  `Input::cursor_position` 同口径；`WindowPlugin` 以窗口初始尺寸首次
  插入、主循环每次 `Resized` 自动同步，照 `Input` 资源模式；离屏 /
  headless 程序无窗口事件，手动 `insert_resource` 写入实际渲染目标
  尺寸，缺省 1280×720）。
- **平滑追随（打磨 IV）**：iris-engine `camera_rig` 模块的
  `SmoothFollow` 组件 + `update_smooth_follow` 系统把「挂载相机 + 第三
  人称平滑追随」固化为现成范式——平滑世界位以 `1 − exp(−lerp·dt)`
  帧率无关指数趋近「目标当帧世界位 + offset」，追随者局部平移写为
  `parent⁻¹ × pos`（父链旋转 / 缩放也精确还原）；`snap()` 吸附（复活 /
  传送）、首帧吸附。注册契约：Update 阶段、在追随目标的写入系统之后
  （目标本帧位移当帧即被追随）；目标世界位沿父链以局部 `Transform`
  现值合成，不读滞后一帧的 `GlobalTransform`（读上帧口径亦可接受，
  文档写明）。iris-ascent 第三人称相机即消费方。

### 4.6 glTF 2.0 资产管线（`asset` 模块）

**数据流**：

```text
.gltf (JSON) / .glb（容器：JSON chunk + BIN chunk）──┬─ buffers：外部 .bin / data-URI base64 / GLB 内嵌
               ├─ accessors：POSITION/NORMAL/TEXCOORD_0 (FLOAT)、索引 (u16/u32)、动画采样器数据
               ├─ materials：baseColorFactor / emissiveFactor / metallicFactor / roughnessFactor
               ├─ textures/images：PNG/JPEG（文件或 data-URI）→ CpuTexture
               ├─ animations：translation/rotation/scale 通道（LINEAR/STEP）→ AnimationClip
               └─ nodes TRS + children ──→ Iris 实体树（add_child 级联）
```

- **加载即场景**：`spawn_gltf_instance` 把节点层级映射为实体树——
  有 mesh 的节点生成父实体（携带节点 TRS），每个 primitive 挂一个子实体
  （独立材质批次，复用父变换）；返回 [`GltfInstance`]（顶层实体 +
  节点索引→实体映射 + 全部动画剪辑；蒙皮实例的兄弟顶层关节树在加载
  完成时自动收编为蒙皮节点的子树，§4.6.2）。`spawn_gltf` 为只取实体树
  的便捷包装。
- **GLB 容器**：按内容嗅探（magic "glTF"），校验版本（仅 2）、chunk 边界与
  顺序（JSON 首位、BIN 仅一个且在 JSON 后）；BIN chunk 作为无 uri 的
  buffer 0 数据源。`write_glb` 为对称的容器组装工具。
- **编程式构造（打磨 IV）**：iris-engine `gltf_builder` 模块的
  `GltfBuilder` 把「声明几何 / 层级 / 关键帧」与「bufferView 偏移 /
  accessor 下标 / sampler 编号 / 对齐填充」分离——`material` / `mesh`
  （单 primitive，POSITION 包围盒与索引宽度自动处理）/ `node` +
  `add_child`（顶层自动推断）/ `skin` + `attach_skin` / `animation`
  （`AnimChannel` 复用运行时枚举，时间轴内容一致自动共享 times
  accessor）声明完即 `build_glb()` / `build_json_and_bin(uri)`；与
  加载器互为逆操作（单测「构造 → 加载 → 剪辑 / 节点映射 / 蒙皮」守
  护），gen_asset 示例的全部资产生成已经迁移。
  **打磨 VI 摩擦③ 追加 `material_textured`**：`GltfMaterialTextures`
  三通道（baseColor / 法线 / ORM）收 CPU 端 `CpuTexture` 像素，登记时
  校验尺寸与非零缓冲，build 时 PNG → base64 写成 `data:` URI 内嵌
  （**不用 bufferView 引用图片**——加载器对该形态仍显式报 Unsupported，
  见上「v1 不支持」；data URI 也保证 GLB 自包含、内存加载路径可用）；
  ORM 图同下标写进 `metallicRoughnessTexture` 与 `occlusionTexture`
  两通道，走加载器「同下标免重打包」快路（§4.6 ORM 合并策略）。
  无纹理声明时 `images` / `textures` 两键不写 ⇒ 既有资产产物逐字不变。
- **去重**：同一 glTF 的 mesh / material / texture 按 (索引[, primitive]) 缓存句柄，
  多节点引用同一网格不重复上传 CPU 目录；builder 侧按**像素内容**去重
  （同图多通道 / 多材质只嵌一份 image）。
- **容错与降级**：所有错误收敛为 `GltfError`（Io / Json / Image / Unsupported /
  Missing / OutOfRange），越界引用不 panic；NORMAL 缺失时按三角形自动生成
  平面法线；TEXCOORD 缺失填充 (0,0)。
- **v1 不支持**（显式报错而非静默出错）：骨架 / 蒙皮 / 形变目标、
  CUBICSPLAT 插值、非三角形拓扑、非 FLOAT 顶点分量、bufferView 引用图片。
- base64 解码为手写实现（RFC 4648 严格校验：长度 4 对齐、padding 后禁止数据、
  残位必须为零），避免为此引入依赖。

### 4.6.1 glTF 动画（节点 TRS）

- **模型**：每条通道绑定「节点索引 + 属性（translation/rotation/scale）」，
  关键帧时间轴 + 平铺值数组；加载时全部解码为紧凑 `Vec<f32>`（`AnimationClip`）。
- **采样**（`AnimationChannel::sample`）：二分定位关键帧段；LINEAR 分量插值，
  **旋转用最短路径 slerp**（点积为负先翻转，glTF 规范要求）——分量线性插值
  会破坏四元数归一化且旋转路径错误（单测实测抓出）；STEP 保持上一关键帧；
  端点外钳制到首/末关键帧。
- **播放**：`AnimationPlayer` 组件持有剪辑、节点映射与播放状态
  （active / time / speed / playing / looping），挂在 glTF 根实体上；
  `instance.player()` 一行挂载。**打磨 VI 摩擦⑥**：播放器另有
  `node_names`（与 `node_entities` 同下标的 glTF 节点名表，加载器从
  `nodes[].name` 填入，随 `player()` 带入），配套
  `node_entity_by_name(name)` 供装配层（IK 链 / 武器挂点 / 材质覆写）
  按名寻骨——重名取先（向量序确定），无名表（手工构造的播放器）恒
  None。动画**通道**仍以节点下标为寻址键（`AnimationChannel.node` =
  glTF 节点索引，与 `AnimationMask` 的节点集同源），名不参与采样。
- **系统**（`update_animations`，DefaultPlugins 在层级传播前注册）：
  两阶段——先只读查询推进时间并采样出「目标实体 → 覆写字段」任务列表，
  再统一写入目标 `Transform`（被动画的属性覆写，其余字段保持静止姿势）。
  `Time` 资源缺席时 dt=0，可手动设置 `player.time` 定位到任意时刻（离屏快照用）。
- **单帧 delta 钳制**：`Time.max_delta`（默认 0.25s）会钳制大步长——
  测试或回放需要确定性大步长时分多帧推进。
- **crossfade 姿势混合（v2）**：`AnimationPlayer::transition(clip, duration)`
  启动向目标剪辑的平滑过渡（`AnimationBlend`）。目标剪辑正常推进、
  源姿势在过渡开始时**冻结快照**（`from_clip`/`from_time`，源不再推进）——
  混合结果只由 elapsed 决定：可预测、可测试，且避免源剪辑循环回绕 /
  播完造成的姿势跳变。权重 `w = smoothstep(elapsed/duration)`（首尾速度
  连续、视觉顺滑；中点与线性同为 0.5）；混合按「目标实体 + 路径」并集
  逐项进行——双方都有：平移/缩放分量 lerp、旋转 **slerp**（恒定角速度；
  nlerp 大角度下中段角速度不均，逐实体每帧个位数旋转通道不值得省）；
  仅目标有：取目标值；**仅源有**：`lerp/slerp(源值, rest 值, w)` 平滑淡回
  **基姿势**（`AnimationPlayer::capture_rest(&World)` 在实体生成后、动画
  推进前把全部目标实体的初始 TRS 记为 rest 快照；过渡完成时恰为 rest，
  blend 清除后该通道不再被覆写、Transform 停在 rest——完成瞬间无跳变，
  不再定格在源值；rest 未捕获时保持 v2「取源值、定格」语义——纯构造
  播放器的既有行为不变）。边界语义：`duration ≤ 0`
  = 硬切换；同剪辑请求忽略；目标越界忽略 + debug_assert（release 静默）；
  `elapsed ≥ duration` 后混合自动清除回纯目标姿势。混合采样用的 elapsed
  与 time 同步在阶段一推进（`elapsed + dt`，阶段二才落账）——姿势只由
  过渡总时长决定，与分帧方式无关。
- **轻量状态机（v2）**：`states`（`AnimationState { clip, looping, speed }`）+
  `edges`（`AnimationEdge { from, to, duration }`）+ `current_state`，
  全部默认空 = 不启用，直接操作 `active/looping/speed` 的旧路径完全保留。
  `request_state(state)`：查 `current → state` 边——有边按 `edge.duration`
  启动 crossfade、无边硬切换（**无边 = 合法的显式硬切换**，但会打
  `log::debug!` 留线索：含 from / to 状态与边表长度——调试期清空 edges
  导致全部过渡跳变时可由此定位）；两种情况 `looping/speed` 都同步为目标态，
  states 非空时 `advanced` 的循环/速度以当前状态为准。**条件判定留给
  游戏侧**：边只带过渡时长，无参数黑板 / 条件求值——何时 request 由
  调用方按玩法逻辑决定（v1 边界：引擎只做「边 + 过渡时长」，不做状态
  内行为逻辑）。
- **动画事件轨道（v2）**：`AnimationClip.events`
  （`AnimationEvent { time, name }`）——事件不在 glTF 标准内，**GLB 解析
  不产事件**，由 API 侧构造（`clip.events.push`）。`update_animations`
  每帧做跨界检测（纯函数 `collect_events`，可脱离 ECS 单测）：
  - 非循环命中 `(prev, new]`（prev 必须取推进前的 `player.time`——两阶段
    数据流先取再推进）；播放器停在末端后每帧 `prev == new`、区间为空，
    **末端事件不重复触发**；
  - 循环传入**取模前原始时间**：事件每被跨越一次触发一次，一帧跨 k 圈
    重复命中 k 次（**multi-fire**，确定性；跨越次数 =
    `⌊(new−t_e)/dur⌋ − ⌊(prev−t_e)/dur⌋`）；回绕 `(prev, dur] ∪ [0, new]`
    是跨圈不足一圈的特例；
  - 输出恒按触发时刻升序（事件表本身可乱序）。
  命中的事件名进入 `player.pending_events`，**消费方每帧
  `take_events()` drain**（帧内消息语义，同 iris-core `Events` 的 drain；
  不消费会跨帧累积）。crossfade 期间只有目标剪辑（active）产事件——
  源已冻结不再推进。典型联动（音频脚步声，玩法侧每帧）：
  `for name in player.take_events() { if name == "footstep" { audio.play(...) } }`。
- **BlendSpace2D 混合空间（v3）**：`BlendSpace { samples: Vec<(Vec2, usize)> }`
  ——任意分布的参数点集 → 剪辑索引（idle / walk / run / strafe 按
  「速度 × 转向」连续混合的标准载体）。**采样 = 逐轴线性分解**：先按
  x 把样本分列（x 精确相等同列）、找括住参数点的左右两列，列内再按 y
  找前后两行，权重 = 各轴插值系数乘积（轴对齐双线性）——点集任意分布
  时单样本列自动钳制、全同 y / 全同 x 自然退化为 1D 链式 lerp，无需特判；
  最多 4 路非零权重，**非负、和恒为 1**（确定性：轴扫描序固定、并列取
  下标小者、参数点精确命中某样本 → 权重 1；单测随机参数网格遍历守护）。
  `AnimationPlayer::play_blendspace()` 激活混合空间模式：**与 crossfade /
  状态机互斥**（`transition` 清位、`play_blendspace` 丢 `blend`），激活时
  `active` 剪辑的通道采样停用（事件轨道不停——共享时间轴仍按 active
  推进），`update_animations` 每帧按 `blend_param` 采样出 ≤ 4 路
  「剪辑 + 权重」走 **N 路姿势混合**（`blend_pose`）：把 v2 的两路混合
  泛化为任意路数，所有混合剪辑共享播放器时间轴；平移 / 缩放 = 加权
  线性和，旋转 = **N 路 nlerp**（首个四元数为基准、点积 < 0 翻转后加权
  求和再归一）——slerp 是两算子不具结合性，N 路结果依赖配对顺序且误差
  链式累积；混合空间相邻剪辑夹角小，nlerp 的非线性不可感知（两路
  crossfade 仍 slerp 语义不变）。
- **两骨足部 IK（v3，`foot_ik` 模块）**：动画之后的落地修正层。
  `update_foot_ik`（DefaultPlugins 注册在 `update_animations` **之后**
  ——修正对象是动画刚写出的本帧局部关节姿势；层级传播**之前**——写回
  的也是局部 `Transform`，当帧传播全局生效）按 `FootIk` 组件开关：
  链节点实体由 `IkRig`（打磨 V：纯 IK 装配自带节点映射，与播放器解耦
  ——§8.22②）或 `AnimationPlayer::node_entities`（回退）二选一提供。
  对「hip ← knee ← foot」直连链，从脚踝世界位上抬 `ray_up_offset` 竖直
  向下射线（**真静态体 only**——动态体会抖动、运动学体常与自身链的
  角色胶囊重叠，见 §3.8 口径例外；**sensor** 是事件域、**碰撞组/掩码
  清零**是标记失效（如压力门开门标记），两者同样不参与落地查询），
  命中点即踝目标；**回写四元数一律先 `normalize()`**（打磨 VI 摩擦①）
  ——IK 是唯一覆写「不被剪辑逐帧重置的骨旋转」的系统，非单位写回在
  无覆写的下一帧会作为复合链输入再乘（共轭不再是逆）⇒ 范数逐帧平方
  复合直至 NaN，归一化责任归 IK 本身（`foot_ik` 模块文档有推导；
  `update_hand_ik` 同款）；**不可达回退门**：目标距髋超出
  `(|upper − lower|, upper + lower)` 可达域时本帧放弃该腿修正（保持姿势
  不写任何变换）——不可达/过近唯一解释是射线采样到非地面几何（过门、
  贴墙瞬间的门框/墙顶命中或起点入体），钳制解算会把腿部网格拉成平面式
  变形，放弃一帧是视觉上最小的错误；**平地门**（高差
  ≤ 1 mm）内不写任何变换——平地姿势 bit 级不动。**链契约：节点
  scale 恒 1**（打磨 V，§8.22④）：节点 scale 会进链矩阵缩水骨长，
  系统按 `FootIk::expected_upper/lower`（构造填写或首解捕获）监测偏差
  > 10% 时每实体一次性 `warn`——生产保护不 panic。解算为
  **完整两骨旋转解**：骨长取当前世界姿势实际距离，纯函数
  `solve_two_bone_ik` 余弦定理解膝角（`a = (d² + u² − l²)/2d`）、pole
  向量投影定弯曲方向（不可达拉伸至全长、过近钳到 |u − l|，确定性退化
  兜底），旋转回写 = 骨方向旧 → 新的最小旋转弧
  （`Quat::from_rotation_arc`）除以父级旋转反推 hip / knee 局部旋转，
  foot 局部旋转不动、按膝新世界矩阵逆回写局部位置把踝精确放到目标；
  骨长严格保持（hip / knee 局部位置不动）。验收：大球面斜坡脚踝贴面
  ±2 cm、平地 / 不可达 / 过近 / sensor / 失效碰撞体五类场景姿势 bit 级
  不变（单测守护）。诚实边界：三节点须 `ChildOf`
  直连成链（debug_assert）、链上均匀缩放假设（非均匀缩放位置仍精确、
  旋转可能偏差）、hip 旋转带动其全部子级（腿部链挂独立节点）。
- **根运动（v3）**：`AnimationPlayer::root_motion_clip`（哪个剪辑带根
  位移）+ `root_motion_node`（哪个节点，None = 完全关闭）。启用中该
  节点的 Translation 通道**不再覆写 Transform**（从覆写列表剔除，含
  混合结果），改为按纯函数 `root_motion_delta` 累积**本帧位移增量**进
  `pending_root_motion`——循环回绕三段精确累积：「旧圈余段 v(dur) −
  v(prev)」+「(k−1) 个整圈位移 v(dur) − v(0)」+「新圈已走 v(raw mod
  dur) − v(0)」（一帧跨 k 圈 multi-lap，同 `collect_events` 的跨越计数
  语义）；非循环停在末端后 prev == new → 零增量不重复计步。
  **消费契约**：增量是剪辑空间（根节点局部平移）的帧位移，游戏侧每帧
  `take_root_motion()`（take 语义同 `take_events`）→ 旋转到角色 yaw 系
  → 交给 `move_and_slide` 位移——根运动只出「每帧走多远」，不直接写
  角色 Transform。crossfade 期间 delta 仅取目标剪辑（冻结源的根位移
  丢弃）。与原规范的偏离：root_motion 标志放播放器侧而非
  `AnimationClip`（新增剪辑字段会给既有测试的结构体字面量引入编译
  破坏，硬约束「既有动画测试零改动」优先）。
- **附加动画层（v4）**：`AnimationPlayer::additive`
  （`AdditiveLayer { clip, time, speed, weight, looping }`）+
  `play_additive(clip, weight)` / `stop_additive()`——在主姿势
  （active / blendspace / crossfade 任一现行模式的混合结果）算完后
  叠加「剪辑相对其参考帧（t = 0 采样值）的增量」：受击抖动 / 呼吸
  起伏类**叠加语义**的载体，不破坏主姿势的移动循环。增量数学：
  平移 / 缩放（加法群）`delta = (v(t) − v(0)) × weight` 分量加法；
  旋转相对旋转 `d = q(t) · q(0)⁻¹`（把参考帧旋回恒等的左乘增量），
  叠加为主姿势**右乘** `main · slerp(I, d, weight)`——附加关键帧表达
  节点局部系内的姿态，主姿势之后的偏移发生在同一局部系（左乘会被
  解释进父级系，受击叠加会随身体朝向漂移）；权重经 slerp 压幅
  （`weight = 0` 恒等、`1` 全幅，直接幂运算无解析式）。推进语义与
  主时间轴对称：暂停整层冻结、循环按剪辑时长取模、非循环钳制末端；
  采样与落账同式（姿势与分帧方式无关，同 crossfade 纪律）。
  **诚实边界：只在主姿势已覆写的通道上叠加**（按「目标实体 + 路径」
  配对）——若以当前 Transform 为基，而当前值含附加层上一帧自身的
  输出，增量逐帧复合无界漂移（旋转类每帧多转一份），与叠加语义相悖；
  附加剪辑的通道应与主姿势对齐设计，未对齐通道跳过（weight = 0 逐位
  恒等单测守护；与 crossfade 并存时叠加发生在混合之后；根运动启用时
  根节点的附加平移增量同样被剔除——叠加发生在根运动剔除之前）。
- **骨骼遮罩（v4）**：`AnimationPlayer::mask`（`AnimationMask { nodes:
  BTreeSet<usize> }`）+ `set_mask(nodes)` / `clear_mask()`——**覆写
  过滤器**，不参与姿势计算：`update_animations` 产出「目标实体 →
  覆写字段」任务后，目标节点不在 `nodes` 集合内的任务整条剔除
  （该节点保持当前值 / 父姿势）。上半身瞄准层的载体：瞄准剪辑只
  覆写脊柱 / 手臂节点（集合内），腿节点保持移动循环姿势（集合外），
  两层各自动画互不打架。**正交性承诺**：与 BlendSpace / crossfade /
  状态机 / 附加层全部正交——附加层的覆写同样被过滤（叠加在过滤
  之前）；`mask = None`（默认）= 全覆写照旧（v1 行为零变化，既有
  测试守护）。`BTreeSet` 选型：确定性文化——固定迭代序，序列化 /
  调试输出可复现。测试：遮罩含上身节点 → 腿节点 Transform 逐位
  不动、清除后恢复覆写。
- **两骨手部 IK（v4，`hand_ik` 模块）**：动画与足部 IK 之后的持枪
  吸附层。`update_hand_ik`（DefaultPlugins 注册序
  `update_animations → update_foot_ik → update_hand_ik`，层级传播
  之前）按 `HandIk` 组件开关，复用足部的 `solve_two_bone_ik` 纯函数
  与「世界系解 → 最小旋转弧回写局部变换」两阶段模式（骨长取当前
  世界姿势实际距离、`ChildOf` 直连链契约、scale 恒 1、均匀缩放假设
  全部同款）。**与足部的目标差异**：① 目标 = `IkTarget` 实体的
  `GlobalTransform` 全局平移（**武器握把位**，玩法量）而非射线打
  地面——目标实体失效 / 无 `GlobalTransform` / 自身
  `Visibility::HIDDEN` → 本帧放弃；诚实边界：全局位是上一帧传播
  产物（一帧滞后），**挂点若位于手骨链子树内会形成一帧滞后反馈环**
  （装配禁忌：挂点放武器根 / 躯干）。② 不可达门的**容差带**：目标
  距肩超出全臂长但在 `reach_threshold` 带内
  （`(upper + lower, upper + lower + reach_threshold]`）仍解算——
  拉伸至全长贴向握把（「伸臂取枪」的正常表现）；超出带或近于
  `|upper − lower|` 才视为异常目标本帧放弃——与足部全域放弃的刻意
  差异：足部不可达的唯一解释是射线采到非地面几何（钳制必错），手部
  接近全臂长的目标是玩法 authored 的正常输入。③ 无平地门（手部
  目标逐帧随武器移动，回写天然幂等）。**双臂装配模式**：组件存储
  每实体单槽 → 每臂一条配置实体挂 `(IkRig(该臂三节点), HandIk)`，
  两条链节点不重叠互不干扰（单测守护）；单臂可直接与播放器 /
  `IkRig` 同挂根实体（与足部同款二选一映射）。注册在足部 IK 之后的
  理由：持枪手位是比脚落地更高频的视觉焦点，两段 IK 作用于同一份
  本帧局部姿势依次生效，注册序确定 ⇒ 结果确定。

### 4.6.2 骨骼蒙皮（GPU LBS）

- **数据模型**：`Skin` 组件挂在蒙皮网格实体上，持有「关节实体表 +
  逆绑定矩阵表」（glTF skins/joints/inverseBindMatrices 直映射）；
  顶点属性 `JOINTS_0`（u8/u16）与 `WEIGHTS_0`（FLOAT）解码为独立的
  `SkinVertex` 缓冲（32 字节/顶点）——非蒙皮网格不为蒙皮属性付出代价。
- **关节矩阵**（`Skin::joint_matrices`，渲染时每帧计算）：
  `J_i = inv(蒙皮实体全局) × global(joint_i) × ibm_i`。乘蒙皮实体全局逆
  把蒙皮定义在**网格局部空间**——移动角色根实体，整具骨骼跟随
  （glTF 规范「忽略蒙皮节点变换」在蒙皮节点变换为恒等时与此等价）。
  解析解陷阱：肘关节旋转是**绕肘点**（bind 位置）的旋转而非绕原点——
  `J = T(elbow)·R·T(−elbow)`，顶点位移断言先写错过一次，被物理直觉纠正。
- **GPU 路径**：每具蒙皮一个只读 storage buffer（按关节数上限分配容量、
  动画驱动下每帧 `write_buffer` 覆写）；独立**蒙皮管线变体**（主管线 +
  阴影管线各一条），顶点着色器 `vs_skinned` 做 4 关节线性混合
  （LBS）：`skinned = model × (Σ w_i·J_i) × v`，法线按刚性变换处理
  （`Skin::joint_matrices` 的 CPU 参考实现与 shader 逐项对应，一致性由
  GPU 端到端测试守护）。
- **绘制路径**：蒙皮实体从常规批处理中排除、独立收集（每实体一次
  draw）；视锥剔除走 §4.4 的**关节变换保守并集界**（不再保守全收）；
  实例区间紧随普通实例之后；阴影 pass 使用蒙皮阴影管线复用同一份关节
  矩阵，保证阴影与主 pass 几何逐顶点一致。
- **加载器**：`skins`/`skin` 节点字段、MAT4 accessor、IBM 缺省时为
  恒等（规范允许）；权重和偏离 1 时归一化（防御非法资产）；
  JOINTS_0 与 WEIGHTS_0 必须成对出现，否则报 `Missing`。
- **蒙皮实例自动收编（iris-arena dogfooding II 发现，「引擎 API 打磨 II」
  轮修复）**：`J_i` 中的 `global(joint_i)` 来自关节实体的全局变换——多
  顶层节点的蒙皮 glTF（mesh 根与关节树根为兄弟，如 gen_asset 生成的
  `enemy_worm.glb`）若关节树留在外面，移动 mesh 根不改变渲染位置
  （关节全局不含蒙皮节点变换，静态展示不暴露，游戏内移动即散架）。
  `spawn_gltf_instance` 加载完成时自动把关节树根收编为蒙皮节点的子树
  （求 joints 集合中不被集合内任何其他节点的 child 边指向的根，
  `add_child` 挂回；关节树已是蒙皮节点后代的单顶层节点资产保持原层级，
  见 §8.13）——移动实例根即移动整只蒙皮角色，调用方无须再做层级操作。
- **限制**：关节非均匀缩放下法线变换不精确（按刚性处理）；剔除界为
  保守并集（含全部关节的角点，即便其顶点权重为零），极端骨骼展开时界
  偏大——只多画不漏画（§4.4）；多 primitive 共享蒙皮时关节矩阵每
  primitive 各上传一份。

### 4.7 方向光阴影贴图

- **深度 pass**：独立 shadow pipeline 从光源视角把裁剪空间深度 `z/w` 写入
  固定 2048² 的 **`R32Float` 颜色附件**（深度存 R 通道）。
- **深度测试必须保留**：颜色附件存深度的方案曾因「省掉深度附件」而翻车——
  没有深度测试时重叠表面按**绘制顺序**互相覆盖，球体背面深度污染贴图
  （实测症状：采样深度系统性大于片段深度、整帧无阴影）。
  最终形态为 **Depth32Float 深度附件（Less 测试，保证存最近面）+ R32Float 颜色附件并存**。
- **为什么不用 depth-only 管线**：PBR/HDR 集成期间实证发现 wgpu 30 / DX12 下
  shadow pass 的深度附件写入恒为 0（排除矩阵、buffer 回读、NaN、剔除、
  强制 `clip.z` 后依旧），且 `textureSampleCompare` 的语义反直觉
  （遮挡判定需 `GreaterEqual`）。改为「颜色附件存深度 + shader 手工比较」后
  路径完全可控、行为可解释，也不再依赖 `float32-filterable` 扩展
  （`R32Float` 采样用 `NonFiltering` 采样器，3×3 PCF 为 9 次最近邻采样）。
- **UV 映射**：`uv = ndc * (0.5, −0.5) + 0.5`——纹理 uv(0,0) 是渲染目标的
  左上角（NDC y 正方向 = 贴图上端），与 blit pass 的 V 翻转约定一致。
  历史版本曾把 `xy/w` 直接当 uv 用（缺 NDC→UV 变换），弱断言的差分测试
  没有抓住它，视觉放大后才定位到；修复已固化在 shader 注释里。
- **比较**：遮挡判定 `d >= depth − bias`（清空值 1.0 = 远平面 = 无遮挡）；
  bias 0.0015（正交投影 + 深度测试下的常数偏移，抑制 acne），
  阴影暗部保留 45% 光照（`shadow_params.z`）。
- **覆盖范围（v1 单张）**：以注视点为中心、`shadow_range`（默认 24）为半边长的正交盒；
  光源相机沿光传播方向反向拉远 `2×range`，保证中心两侧 `±range` 的深度都落在 [0,1]。
  R29 起生产路径为级联阴影（CSM，见 §4.7.2），本节所述贴图形态/双附件/
  UV 映射/bias/控制流模式均为 CSM 沿用的公共基础。
- **uniform 控制流合规**：`textureSample` 要求 uniform 控制流，
  越界 / 光源背后 / 阴影关闭都不做提前返回，采样照常执行、最后用掩码混向「受光」。
- **同 pass 读写冲突**：shadow pass 使用只含 uniform 的精简绑定组
  （`shadow_globals_bg`）——完整全局组引用了阴影贴图本身，
  在同一 render pass 内会触发资源互斥校验错误。
- 阴影只调制方向光的辐射度，不影响环境光 / 点光 / 自发光；
  `DirectionalLight.shadows` 可整帧关闭（贴图 pass 跳过 + shader 掩码兜底）。
- **透明几何不进阴影 pass**（R23）：收集期按 `alpha_mode` 分区，阴影
  只遍历不透明实例区间——半透明不应投实心阴影（语义与理由见 §4.16）。

### 4.7.1 点光阴影（cubemap）

- **范围（v1 限定）**：只有查询顺序中的**第一盏** `PointLight.shadows == true`
  的点光投射阴影（收集后交换到 uniform 槽位 0，着色器只对 `i == 0` 采样）；
  多盏开启时取首灯并 `log::warn`。cubemap 每灯 6 个 pass，多灯投影在路线图。
- **归一化距离存储**：cubemap 的 `R32Float` 存
  `length(world_frag − light_pos) / range` 而非光空间 `z/w`——
  采样侧因此**无需任何光空间矩阵**：`normalize(world_pos − light_pos)` 直接
  `textureSampleLevel(shadow_cube, ..., dir, 0.0)` 查 texel，比较
  `stored >= dist/range − bias/range`（bias 以米存 uniform，比较时归一化）。
  捕获侧由片段着色器从插值世界坐标现算距离（距离函数非线性，顶点插值会在
  大三角形上产生米级误差）；清空值 1.0（= range/range）代表无遮挡。
- **面矩阵**：每面 `look_at(light_pos, light_pos + dir, up)` × 90° 透视
  （near 0.05、far = range，DirectX 深度 [0,1] 约定）；面序 = wgpu/OpenGL
  立方体层序 `[+X, −X, +Y, −Y, +Z, −Z]`，(dir, up) 取标准正交面集
  （±X 与 ±Z 面的 up = (0,−1,0)，+Y 面 up = (0,0,1)，−Y 面 up = (0,0,−1)）。
  投影只决定光栅化覆盖，**面方向必须与 GPU Cube 采样约定精确一致**。
- **NDC y 翻转（关键推导）**：OpenGL 的面矩阵标准集在 wgpu 下**直接用会垂直镜像**。
  原因：GL 的 FBO 渲染把 NDC y=+1 写进贴图**最后一行**（t = (y_ndc+1)/2），
  而 wgpu/Vulkan/D3D 的光栅化把 NDC y=+1 写进**第 0 行**（v = (1−y_ndc)/2）；
  Cube 采样表（s、t 由主轴分量给出）两者相同，于是捕获结果逐面 V 镜像。
  补救是在面矩阵左乘 `scale(1, −1, 1)`（只翻 y 行）：u 与深度 z 不受影响，
  三角形环绕方向反转由阴影管线的 cull None 吸收。横向 u 逐面核对过均与采样表
  一致，无需翻转。集成测试 `face_orientation_matches_cube_sampling` 用**偏置
  遮挡物**守护此约定——居中遮挡物的阴影对镜像对称，抓不住这个错（实测：去掉
  翻转后径向差分测试照常通过，偏置测试 0/148 全错位）。
- **双附件教训的复刻**：与方向光阴影同构——`R32Float` 颜色附件存值 +
  `Depth32Float` 深度附件（Less）并存，颜色附件的 texel 只在深度测试通过时
  更新，保证存最近遮挡物；没有深度测试时重叠表面按绘制顺序互相覆盖，
  背面值污染贴图（§4.7 的实证教训）。
- **6 个渲染视图与 6 份独立 uniform**：采样视图 dimension Cube 一次创建；
  渲染视图每面一个（D2 + `base_array_layer = i`）。每面参数（64B 面矩阵 +
  16B 灯位置/range = 80B）放**独立小 uniform 缓冲**而非一份缓冲的六个插槽——
  绑定偏移必须满足 min_uniform_buffer_offset_alignment（典型 256），且同一
  submit 内对同一缓冲的多次 write_buffer 会在执行前全部合并（泛光参数教训，
  §4.11）。全部 write_buffer 后随本帧统一 submit。
- **uniform 控制流掩码**：采样在 `dist < range` 受光分支之外无条件执行
  （`i == 0` 由均匀的 point_count 派生，分支均匀；`textureSampleLevel` 带
  显式 mip 不依赖导数），最后把阴影系数乘进该灯的 radiance——与
  `shadow_factor` 同款模式；超出 range 的片段不进受光分支，掩码自然为受光。
- **捕获时的粗剔除**：实例包围球到灯的距离减去半径仍超出 range 的，六个
  90° 面的并集（以灯为中心的完整球域）必不覆盖它，六面全部跳过；剔除只
  影响阴影 pass 的绘制区间（可见性变化处拆分 run），不影响主 pass 合批。
- 阴影只调制首盏点光的辐射度，不影响环境光 / 方向光 / 其余点光 / 自发光。

### 4.7.2 方向光级联阴影（CSM v2，R29）

v1 的单张阴影贴图以注视点为中心、`shadow_range`（24 m）为半径覆盖：
近处 2048² 摊在 48 m 上尚可，大场景「近处糊、远处无影」。R29 改为
**3 级级联阴影（CSM）**，级联沿相机视锥深度分割，各用一张全分辨率
切片——近处纹素密度提升一个量级，覆盖带推到 120 m。

- **资源形态**：阴影贴图从单张 2048² `R32Float` 改为 **2048² × 3 层的
  `texture_2d_array`**（Depth32Float 数组附件 + 「深度写颜色 R 通道」的
  双附件方案原样保留——§4.7 的 DX12 depth-only 教训对数组同样适用，
  逐级联一个渲染视图（D2 + `base_array_layer = i`）逐层渲染 pass，采样侧
  用全层数组视图按级联选层）。最小改动且与既有测试完全兼容，未改用
  depth-only + `CompareFunction` 采样。
- **分割（PFS）**：`cascade_splits(near, far)`（纯函数，可单测）——
  `split_i = (1−λ)·near·(far/near)^(i/N) + λ·(near + (far−near)·i/N)`，
  λ = 0.6 混合对数（近处密）与线性（远级联不失控）；`near` 取
  `min(z_near, 1.0)` 防止过小近平面把对数分割压向零点；`far` 钳制到
  **覆盖距离 `SHADOW_DISTANCE = 120 m`**——级联只铺到此处，覆盖带之外的
  片元（shader 侧 `view_z ≥ splits.z`）不投影、保持受光。
- **拟合与纹素对齐**：`cascade_fit`（纯函数）取相机该深度片 8 角（沿
  forward 按 fov/aspect 撑开），变换到光空间（朝向逻辑与 v1 一致，光源
  眼点固定世界原点——正交投影沿光轴移动眼点不改变 XY 映射，深度窗口
  宽度只由 AABB 尺寸决定，与相机离原点多远无关）求 AABB。**纹素对齐
  （消闪烁的关键）**：AABB 的 XY 边界 round 到 `texel = extent / 2048`
  网格（min 向下、max 向上取整，盒只扩不缩）——正交投影下片元阴影图
  UV = (dot(p,u) − min_xy)/extent，相机平移时 AABB 连续移动，不量化则
  UV 亚纹素漂移、阴影边缘爬行闪烁；量化后投影平移恒为纹素整数倍，
  采样相位只会整纹素跳变，视觉稳定。**Z（深度）不对齐**：深度只影响
  比较精度，无可见相位漂移。深度窗口向光/背光各外扩一个 AABB 包围球
  半径，保证切片外的遮挡物（高塔向切片投影）与足印内的地面都落在
  窗口内；SHADOW_BIAS（NDC 常数 0.0015）对应的世界偏移 = 0.0015 ×
  窗口宽，随级联变大（远级联接触阴影略有 peter-panning，v1 接受）。
- **级联参数独立缓冲**：主 pass 片元需要 3 × 矩阵 + 相机前向 + 分割点
  （224B `CsmParams`，globals 组 binding 8——排在现有最大 binding 7 之后，
  **不扩 Globals**，Globals 反而因单一 `light_view_proj` 移除而从 576B
  缩到 512B）；阴影 pass 每级联只读一个 64B 矩阵（一级联一缓冲，规避
  同 submit 多次 write_buffer 合并问题，同 §4.7.1 面参数教训）。
- **片元级联选择**：`view_z = dot(world_pos − eye, cam_forward)`（**view
  深度**而非欧氏距离——欧氏距离在视锥边缘把近处片元误分到远级联；也
  不用 clip z——透视非线性，分割点得先换算）。`view_z < splits[i]` 即第
  i 级；动态索引进 uniform 数组取该级联矩阵，`textureSample` 带
  `i32(cascade)` 数组层。选择/越界均为非均匀分支，采样照常执行、掩码
  事后混向「受光」（§4.7 的 uniform 控制流模式不变）。
- **实例区间沿用主 pass**：三个级联 pass 复用主 pass 视锥剔除后的不透明
  实例区间全部重画（透明项与粒子不投影，语义不变）——保守不漏影；
  按级联视锥再剔除是后续优化点。`FrameStats` 口径不变（阴影 pass 不计）。
- **测试**：`cascade_splits` 单调性/钳制/λ 混合生效、`snap_aabb_to_texels`
  对齐后跨度为 texel 整数倍/只扩不缩/幂等、`cascade_fit` 含住 8 角点/
  光空间深度单调/亚纹素平移 UV 漂移 ≤ 2 texel（相位稳定性的数学形态）；
  GPU 侧 `csm_render.rs` 大场景（~80 m）断言近/远级联物体脚下都有阴影、
  级联边界两侧阴影带连续（无接缝）。

### 4.8 MSAA 与深度

- 固定 4x 多重采样：主 pass 渲染到 **HDR（Rgba16Float）MSAA 附件** +
  `resolve_target` 解析到单采样 HDR；深度 `Depth32Float`，`Less` 比较 + 写入。
- HDR 中间缓冲按帧尺寸懒创建，窗口 resize 时同步重建。
- 交换链 `PresentMode::Fifo`：全平台可用、天然限流不空烧 GPU。

### 4.9 GPU 资源生命周期

- CPU 目录（`MeshStore / MaterialStore / TextureStore`，World 资源）与
  GPU 缓存（`Renderer` 内 HashMap）分离，**句柄即弱引用**。
- 惰性上传：渲染时首次遇到的句柄才上传 GPU；重复句柄零开销。
  纹理上传走 BC1 决策树（R30）：设备启用 `TEXTURE_COMPRESSION_BC` +
  尺寸 4 对齐 + 全不透明 → `Bc1RgbaUnormSrgb`（显存 1/8），否则回退
  `Rgba8UnormSrgb`（见 §4.18）。
- 帧统计（`FrameStats`）暴露 draw call / 实例数 / 剔除数 / 三角形数 /
  上传字节（字段口径见 §4.4.1），供性能面板与测试断言；目录侧另有
  `memory_bytes` / `gpu_bytes_estimate` 内存占用统计（§4.18）。
- `Gpu::features()` / `Gpu::supports_bc()` 暴露设备特性（wgpu 的压缩
  纹理格式要求特性显式请求，初始化时已按适配器能力请求 BCn 子集）。

### 4.10 离屏渲染

`OffscreenRenderer` 与窗口渲染共享同一 `Renderer` 核心（管线、批处理、上传逻辑完全一致），
仅目标是纹理 + 读回缓冲。用途：CI 集成测试、截图导出、无头环境。
`render` 渲染并拷贝到读回缓冲；`draw_frame` 只渲染不拷贝（性能基准把
`Renderer::draw` 的 CPU 耗时与读回路径解耦，见 §4.4.1）。
GPU 适配器缺失时构造返回 `None`，调用方优雅降级——测试可跳过而非失败。

### 4.11 后处理链 v1（pass 串联框架：景深 / 运动模糊 / 泛光 / 曝光 / 暗角）

**pass 链顺序**（编译期有序；tonemap/blit 是链的固定终点，全部效果都在
线性 HDR 域作用）：

```text
主 HDR 解析缓冲（不透明 → 蒙皮 → 透明 → 粒子的混合结果 + 天空背景）
  → 景深 DOF（可选，全分辨率）：逐像素 CoC → 12-tap 泊松盘 HDR 平均
      （采样主相机深度副本，见下「场景深度副本」）
  → 运动模糊 MB（可选，全分辨率）：深度重建世界位 → 上帧相机重投影
      → 屏幕速度方向 N-tap 采样（v1 只覆盖相机运动）
  → 泛光（可选）：亮度提取 → 高斯模糊（半分辨率 ping-pong A/B，三轮分离迭代）
  → 暗角（可选）：全分辨率 HDR → HDR，UV 距中心 smoothstep 压暗四角
  → blit：叠加泛光（x）+ 曝光（y）+ ACES 色调映射 → sRGB 目标
  → UI 叠加层（blit 之后，见 §4.13，不吃任何后处理）
```

DOF / MB 排在泛光**之前**：焦外亮源与运动拖影应参与后续亮度提取
（HDR 线性域先混合、后做镜头合成，与暗角先于曝光同理）。两者默认
关闭（资源缺席 = 关），关闭时整段跳过、链与 R42 基线逐位一致（
`tests/post_render.rs` FNV 哈希锁定）；DOF 的清晰带覆盖全深度 / MB
的相机静止路径在**启用时**也逐位透传（select 原色分支）。

- **pass 串联框架**（`post.rs`，防过度设计——编译期有序链，不做运行时
  注册 / trait 对象注册表 / 拓扑排序）：全屏后处理 pass =
  `PostPassDesc`（管线 + 组 0 布局 + 参数 uniform + 源 + 目标槽位 +
  可选附加纹理 `extra_texture`——DOF / MB 据此绑深度副本，组布局用
  `post_depth_layout`：binding 3 = R32Float 非过滤纹理，不依赖
  float32-filterable 扩展），编码统一走 `encode_post_pass` 单一函数
  ——链上全部 pass 的 begin_pass / set_bind_group / draw 样板只此一份。
  中间纹理由 `PostScratch` ping-pong 分配器按**分辨率等级**（泛光 =
  半分辨率、其余 = 全分辨率按需）各缓存一对 Rgba16Float 纹理，命中同
  尺寸跨帧复用、尺寸变化整体重建；槽位拓扑由两个纯函数单点定义并各
  有 CPU 单测守护：泛光的 `bloom_blur_slots`（任意轮数输出恒回 A）与
  全分辨率链的 `full_chain_slots`（DOF 写 A、MB 写 B、暗角写链头对面
  槽位防同 pass 读写互斥、blit 源 = 暗角输出或链头，8 种开关组合的
  拓扑不变量有测试）。
- **场景深度副本**（R43，`shaders/scene_depth.wgsl`）：主 pass 的深度
  附件是外部传入的 Depth32Float **MSAA** 纹理（仅 RENDER_ATTACHMENT，
  不可绑采样、无深度解析），DX12 下 depth-only 采样另有写入异常的教训
  （§4.7）——因此照搬阴影的「深度写颜色 R 通道 + Depth32Float 双附件」
  方案：仅 DOF / MB 启用时把不透明 + 蒙皮实例从相机视角重画一遍，
  R32Float 单采样附件存**线性视距**（clip.w；varying 透视校正插值对
  1/w 线性恰给出逐像素精确视距），天空清空为 z_far；自带 Depth32Float
  附件（Less）做最近面裁决。资源随尺寸惰性创建、跨帧复用。
- **景深**（`DepthOfFieldSettings { enabled, focus_distance,
  focus_range, max_blur_radius, focus_target }`，`shaders/dof.wgsl`）：
  薄透镜简化
  ——CoC = clamp(|d − focus| / range − 1) × max_radius（d = 深度副本
  的线性视距，textureLoad 读取），12-tap 泊松盘 HDR 平均；清晰带内
  CoC 恒 0 走 select 原色透传（半纹素以下同样透传）。focus_distance
  口径为沿视线方向的 view-Z（画面边缘的斜向距离略长，§8）。
  **打磨 VI 摩擦⑤**：`focus_target: Option<Entity>` 让对焦距离由引擎
  逐帧解析——`post::resolve_focus_distance`（纯函数）取目标实体
  `GlobalTransform` 世界位经主相机视图矩阵的 −z（同 view-Z 口径），
  解析点在 `Renderer::draw` 的 DOF uniform 上传处（DOF 关闭时不解析）；
  目标失效 / 无 `GlobalTransform` / `Visibility::HIDDEN` 或非有限值 →
  回退 `focus_distance` 标量（资源是配置不是状态，解析结果不回写）。
- **运动模糊**（`MotionBlurSettings { enabled, strength, max_samples }`，
  `shaders/motion_blur.wgsl`）：逐像素深度 → NDC z（DirectX [0,1]
  约定的解析重建 z = f/(f−n) − (n·f/(f−n))/d，CPU 预计算两系数）→
  inv_view_proj 反投影世界位 → prev_view_proj 重投影 → 屏幕 UV 速度
  → 沿速度方向 n = clamp(|v| × strength, 1, max_samples) tap 采样
  （循环恒跑 32 槽按 n 掩码加权，规避 WGSL uniform 控制流限制）。
  渲染器存上一帧主相机矩阵（首帧用当前帧补位）；**只覆盖相机运动**
  （§8）。相机静止（两帧矩阵逐位相同 → 速度 0）时原色透传。
- **泛光三段式**：亮度提取（smoothstep 软阈值，HDR 亮度超过
  `BloomSettings.threshold` 才参与）→ 可分离高斯模糊（9 tap 线性采样，
  半分辨率，水平/垂直管线共用、三轮迭代扩展半径）→ blit 合成（ACES
  色调映射**之前**按 `strength` 叠加，保证高光泛光同样被色调映射滚降，
  不会死白）。**纯加法不变量**：泛光只增不减（加法 + 单调速映射），
  集成测试据此断言「开/关差分：光晕成片变亮、零像素变暗、远场不变」。
- **曝光**（`ExposureSettings { exposure }`）：传感器增益，线性 HDR 在
  ACES **之前**乘 `exposure`（blit 的 `hdr * blit_params.y`）。ACES
  （Narkowicz）在正半轴严格单调 → 0.5 / 1.0 / 2.0 三帧中心亮度严格
  递增（GPU 测试守护）；`exposure = 0` 全黑（ACES(0)=0）；负值按 0
  钳制。y = 1.0 的乘法逐位中性，是「默认零变化」的数学保证。
- **暗角**（`VignetteSettings { strength }`，`shaders/vignette.wgsl`）：
  镜头物理遮挡的近似——全分辨率单输入 pass 按 UV 到中心距离
  `smoothstep(0.35, 0.75, d)` 压暗四角，中心 falloff 恒 0 逐位无损。
  **链位与顺序理由**：暗角在泛光之后、blit 之前（线性 HDR 域作用）；
  暗角（镜头遮挡）先于曝光（传感器增益）——光先被镜头边缘挡住、再
  落到传感器上放大，blit 中的乘曝光因此排在暗角之后。开启时 blit 的
  hdr 源切到暗角输出槽（DOF / MB 入链时写链头**对面**槽位，A/B 两个
  接入绑定组随纹理对惰性构建、跨帧复用）。
- **资源缺席语义（零开销默认路径）**：`BloomSettings` 缺席 = 默认启用；
  `ExposureSettings` 缺席 = 1.0 中性（画面与无曝光控制逐位一致）；
  `VignetteSettings` 缺席或 `strength = 0` = 整段跳过暗角 pass；
  `DepthOfFieldSettings` / `MotionBlurSettings` 缺席或 `enabled = false`
  = 整段跳过对应 pass（连同场景深度副本 pass）与额外按需资源。全部
  关闭时 blit 直读主解析缓冲，与 R42 基线帧逐位一致。以上资源都**不在
  DefaultPlugins 强制注册**，需要时手动 `insert_resource`。GPU 测试
  逐位断言：「曝光缺席 ≡ 显式 1.0」「暗角 0/缺席 ≡ 无 pass 帧」
  「DOF/MB 关闭 ≡ R42 基线 FNV 哈希」「DOF 清晰带覆盖全深度 ≡ 透传」
  「MB 启用 + 相机静止 ≡ 透传」。
- **uniform 对齐陷阱**：泛光三个 pass 的参数曾放在同一缓冲的 32B 插槽
  里，触发 `min_uniform_buffer_offset_alignment`（典型 256）校验错误——
  绑定偏移必须 256 对齐；改为三个独立小缓冲后消除。全链共享合成参数
  `PostParamsRaw`（16B：x 泛光 / y 曝光 / z 暗角 / w 保留）为独立缓冲，
  暗角 pass 复用同布局只消费 z。
- **新增一个后处理 pass 的步骤清单**（暗角即模板）：
  1. 写 WGSL：三顶点全屏三角形 + 片段入口，组 0 = uniform + 源纹理 +
     采样器（照抄 `vignette.wgsl` 骨架）；
  2. `pipeline.rs` 用 `make_post_pipeline` 建管线（目标 = HDR 格式，
     与链上其他中间 pass 同构，仅换着色器模块与片段入口）；
  3. `renderer.rs` 的 `draw` 后处理段按链序追加一个 `PostPassDesc` 并
     调用 `encode_post_pass`；需要每帧可调参数就在 `PostParamsRaw`
     占位分量或独立参数缓冲里携带；
  4. 需要新分辨率等级就在 `PostScale` 加变体（分配器自动管理纹理对），
     强度可关的 pass 记得「0 = 整段跳过」并补一条逐位等价测试。
- v1 不做降采样链（1/4、1/8 mip 链），三轮半分辨率迭代在质量与占用间
  取中；泛光源来自自发光（`MaterialDef.emissive`，HDR 值可 > 1）。

### 4.12 IBL 环境光照（分割求和）

- **零资产依赖**：环境源是**程序化天空立方体贴图**（128²，Rgba16Float）——
  天顶蓝 → 地平线白的渐变 + 地面暖棕回弹 + HDR 太阳盘，渲染器初始化时烘焙，
  无需外部 HDR 图片（契合低占用与开箱即用）。
- **一次性预计算**（`environment.rs`，43 个全屏 pass、动态偏移 uniform 传参）：
  1. 漫反射辐照度立方体贴图：围绕法线的余弦加权半球积分（128²）；
  2. 镜面预滤波立方体贴图：GGX 重要性采样（Hammersley 序列），5 级 mip
     对应粗糙度 0..1 均分；
  3. BRDF LUT（512²）：分割求和的环境 BRDF 积分，uv = (NdotV, roughness)。
- **运行时主 pass 只做三次采样**：`irradiance(n)` 漫反射 +
  `prefiltered(reflect(-v,n), lod = roughness×(mip−1)) × (F×LUT.x + LUT.y)` 镜面，
  金属的环境反射由 `fresnel_schlick_roughness` 抬高边缘反射率。
- **控制**：`EnvironmentSettings { intensity }` World 资源整帧缩放（0 = 关闭，
  集成测试用开/关差分验证：铬球应反射出成片蓝天像素）。
- **天空盒背景**：主 pass 内最先绘制的全屏三角形（深度只测不写），像素 NDC
  （z = 远平面）经 `inv_view_proj`（Globals 内，560B）变换回世界空间，
  方向 = 远平面点 − 相机位置；采样预滤波环境 0 级 mip——粗糙度 0 的预滤波
  结果与原始天空逐像素一致，无需为背景单独绑定贴图。
- **烘焙 pass 的读写边界**：预滤波读天空立方体、写预滤波立方体——不同纹理
  同 pass 无冲突；pass 参数（面索引/粗糙度）经 256B 步长动态偏移传入
  （单次 submit 内多次 write_buffer 同一缓冲会在命令缓冲执行前全部完成）。
- **烘焙参数步长陷阱**：CPU 写入与 GPU 动态偏移必须用同一步长（256B）。
  曾按 16B 紧凑写入、按 256B 偏移读取——除 0 号槽外全部参数错位，
  预滤波立方体六个面被烘成同一面的内容。实测症状：天空盒高仰角区域
  带纯红偏置（采样到 +X 面地平线色）；IBL 差分测试仍然通过（地平线色
  恰好也是蓝色，弱断言抓不住）——最终靠「解析天空 vs 贴图采样」二分
  + 逐像素正向 ACES 验证定位。
- **图元环绕方向陷阱**：`uv_sphere` 的索引曾按 [a,c,d/a,d,b] 生成——
  `(∂φ)×(∂θ)` 指向球内，CCW 正面判定下**所有外向面被剔除、渲染的是内壁**，
  法线背向相机导致直接光照恒 0（实测症状：球体在 2.5 强度强光下仍是纯黑
  剪影，仅剩微弱环境光；金属球「发暗发平」长期被误判为缺 IBL）。
  修复为 [a,d,c/a,b,d] 并在代码注释中固化推导。

### 4.12.1 外部 HDR 环境源（Radiance .hdr equirectangular）

- **手写解析器**（`asset/hdr.rs`，零新依赖）：`#?` 魔数 + 任意头部键值行 +
  空行；分辨率行仅支持 `-Y <h> +X <w>`（其余朝向 → Unsupported）；扫描线
  双格式——旧式逐像素交错 RGBE 与新式逐通道 RLE（`[2,2,hi,lo]` 头且
  `hi:lo == width`，宽度校验是新旧格式的关键判别，防止旧式像素恰好以
  [2,2,·,·] 开头时误判）。RGBE → 线性值 = m × 2^(e−136)，指数 0 表示编码黑。
- **约定**：图像第 0 行 = +Y 天顶，+X 方向在图像水平正中——捕获 shader 的
  `dir → uv`（`u = 0.5 + atan2(dir.z, dir.x)/2π`，`v = 0.5 − asin(dir.y)/π`）
  与生成/加载端共用该约定，半球颠倒会直接表现为上下反转。
- **捕获路径**：`SkySource::EquirectHdr` 时全景图上传为 Rgba16Float 2D 纹理
  （f32→f16 手写位模式转换，舍入到最近偶数；行距按 256B 对齐建暂存——
  `write_texture` 的 `bytes_per_row` 必须 256 对齐），捕获 pass 换用
  `fs_capture_equirect` 入口按方向采样全景图；下游辐照度 / 预滤波 / BRDF
  pass 与天空盒背景完全复用，无需感知天空源种类。
- **generation 失效重烘**：`SkySource` 资源携带全局递增的 generation，
  `Renderer` 缓存已烘焙值（0 = 程序化天空，资源缺席同值 → 零开销）；
  `draw` 开头发现不一致即重烘并重建全局绑定组（env 视图已更换），
  构建逻辑与 `Renderer::new` 共用同一函数，避免条目清单两处漂移。

### 4.13 2D UI 叠加层（HUD）

- **pass 顺序即语义**：UI pass 是一帧的最后一个 pass，位于色调映射
  blit 之后、同一 sRGB 目标上以 `LoadOp::Load` 叠加（不清屏、无深度
  附件）。因此 HUD **不参与 HDR / 后处理 / ACES 色调映射**——HUD 是最终
  界面而非被后期处理二次加工的场景辐射：白色文本就是纯白，不会被泛光
  染雾、不会被色调映射压灰（集成测试以「HUD 区域蓝主导不透 3D + HUD
  外区域与无 UI 渲染逐像素一致」守护该性质）。
- **像素坐标系**：`UiRect.position / size` 为屏幕像素，**原点左上、
  y 向下**（与窗口行序一致）。顶点着色器一步完成归一化与翻转：
  `ndc = pos / screen × (2,−2) + (−1,1)`；屏幕尺寸（vec2，uniform 补齐
  16B）每帧 `write_buffer`。整数像素坐标 + 整数 scale 下 quad 与像素
  网格精确对齐，无 MSAA（目标是 blit 后的单采样 sRGB）、cull None
  （UI 面向屏幕）、标准 alpha 混合（`BlendState::ALPHA_BLENDING`）。
- **单批单 draw 的合批策略**：每帧两阶段只读查询（先矩形后文本，
  alpha 混合下文本叠于矩形之上；查询带 `Option<&Visibility>`，挂
  `Visibility::HIDDEN` 的实体不产生顶点、组件缺席 = 可见，且**含层级
  级联**——任一祖先 `HIDDEN` ⇒ 子树全隐藏（R41：把渲染器的
  `hidden_by_ancestor` 收归 `visibility.rs` 共享，UI 全部收集路径——矩形 /
  文本 / 图片 / 图片 RT——统一启用，此前 UI 只看自身 `Visibility`、不看
  祖先）直接组装
  `Vec<UiVertex>`（像素位置 +
  图集 UV + 线性 RGBA 顶点色，32B POD），矩形与文本全部 UI 内容合并为
  **一次 draw call**（`stats.draw_calls` 计入）。矩形 = 6 顶点、UV 取
  图集 0 号格纯白块（带半纹素内缩）；文本逐字形扫描行位码，每个亮位
  展开为一个 scale×scale 像素的实心小 quad，四角 UV 恒定采样该位
  texel 中心（最强的内缩，恒不触邻格，逐位缩放逐像素精确）。顶点缓冲
  容量复用（不足时 2 倍扩容整体重建，与实例缓冲同款模式），顶点非空
  才开 pass——空 UI 场景零额外开销。
- **位图字体来源与图集布局**：内嵌公有领域 font8x8（Daniel Hepper
  整理，ASCII 32..=127 共 96 字形，每字形 8 字节行位码、LSB = 最左
  像素），按标准表誊录于 `ui.rs` 并由单测守护（参考表逐字节比对 +
  空格全零 + 'A' 亮像素数区间 + 墨迹包围盒列和镜像对称——直接比较
  第 0/7 列对左对齐的真实字表恒假，守护以墨迹边界为轴）。字形图集是
  初始化时一次性构建的单一 `Rgba8UnormSrgb` 纹理（128×56 = 16×7 格，
  每格 8²）：0 号格纯白不透明（矩形复用，单管线单纹理单 draw），96 个
  字形格为「白色 + alpha = 行位码」；文本颜色由顶点色调制
  （片段 = tex × color），采样器 Nearest（位图锐利，亦弱化图集渗色）。
  等宽步进与行高均为 8×scale，`\n` 换行；非 ASCII 字符跳过并告警一次。
- **文本定位**：`UiText` 的排版原点复用同实体 `UiRect.position`；该
  `UiRect` 此时**仅作定位载体、不绘制矩形**（缺省白矩形会盖住文字的实测
  教训），面板背景用独立矩形实体表达（验收示例的血条即「深底矩形 + 红色
  前景矩形」两实体）。
- **锚定布局（R41）**：`UiAnchor { point, offset }` 组件把挂有 `UiRect`
  的元素一角对齐到视口九宫格一角（`UiAnchorPoint` 九点），位置由**纯函数
  `anchor_to_position(point, viewport, size, offset)`** 解析为绝对像素
  （不拉伸不缩放 `size`）。解析系统 `update_ui_layout` 读 `Viewport`
  资源（缺席 = 1280×720 兜底）。**Center 类锚点把元素中心钉在视口中心**，
  改视口尺寸不漂移；文本实体复用本组件（其 `UiRect.size` 视作文本框参与
  居中）→ 标签与按钮面板共用同一锚点时恒居中。
- **按钮 + 焦点导航（R41）**：`UiButton { label }`（便利组装，配合
  `spawn_ui_button` 生成「底板 + 标签」两实体）+ `Focusable`（标记）+
  `UiFocus` 资源（`{ current, order, highlight_color }`）。`update_ui_focus`
  每帧收集全部**可见**可聚焦实体，按锚定后屏幕 Y 再 X **确定性排序**
  （三者全等并列取实体 bits，可网络/存档复用）；↑/← 后退、↓/→/Tab 前进、
  Enter/Space 与鼠标左键**按下沿**+`cursor_position` 命中 → 发
  `UiClickEvent { entity }`（事件而非回调——组件/资源友好，无闭包注册表，
  游戏侧 `events_mut().drain()` 消费）。焦点高亮 = 改写当前实体 `UiColor`
  为 `highlight_color`（**原色保存恢复**于 `UiFocus` 资源的映射，组件换色
  实现，无样式表）。
- **菜单栈（R41）**：`UiMenu(String)` 标记实体归属 + `UiMenuStack`
  资源（`push_menu`/`pop_menu`/`top_menu`）。`update_ui_menus` 只裁决
  可见性——**栈顶菜单的全部实体 `VISIBLE`、其余 `HIDDEN`，空栈全隐，
  未挂 `UiMenu` 的实体不参与**（HUD 常驻可见）；同名菜单多实体全可见。
  **引擎不内置 Esc 语义**——弹栈由游戏侧在有 `UiClickEvent` / 按键输入时
  自行调栈资源方法（`update_ui_menus` 不读输入）。
- **执行序**：三系统由引擎门面层 `UiPlugin` 注册到 `Stage::PostUpdate`
  （渲染发生在 PostUpdate 之后，故本帧即对收集生效），顺序
  `update_ui_layout`（写 `UiRect.position`）→ `update_ui_menus`（写
  `Visibility`）→ `update_ui_focus`（按最终位置与可见性排序/换焦/高亮）。
- **图片控件（R40，见 §4.21）**：`UiImage { texture }` / `UiImageRtt
  { target }` 把普通纹理或 RT 颜色纹理贴到屏幕（`UiRect` 定位 + `UiColor`
  乘色），UV 全屏 0..1 直角采样（Nearest）；**图片之下、文本之上**（图
  片先于 atlas 矩形/文本绘制）。挂图片的实体其 `UiRect` **仅作定位/尺寸
  载体、不绘制矩形**（与 `UiText` 同语义——否则缺省白矩形会盖住图片）。

### 4.14 音频子系统（iris-audio）

**分层：混音核心与设备层彻底分离**。`AudioMixer` 是纯软件混音器，不接触任何
设备——采样推进由调用方每帧 `mix(&mut out, channels, sample_rate)` 填充输出
缓冲驱动，因此**全部混音数学都是可确定性断言的纯函数**（音量缩放、多声音线性
叠加、循环回绕、重采样过零点、限幅边界均有采样级单测）；设备层 `AudioOutput`
只是把混音器接到 cpal 输出流的薄壳。这条线画在「能测什么」上：混音的每个
可观察行为都可以离线复现，设备只是把它变成声音。

- **数据模型**：`AudioClip` = `Arc<Vec<f32>>` 交错 PCM（归一化 [-1,1]）+ 声道 +
  采样率；`Arc` 使多声音共享同一份大缓冲，`play` 只克隆指针。
- **混合模型**：活动声音采样 × 音量后逐帧线性叠加。叠加结果做 `[-1, 1]` **硬钳制
  软限幅**——选钳制而非 tanh：tanh 在未超限时也压缩动态（音色失真）且每采样
  多一次超越函数；钳制对 |x| ≤ 1 的信号完全透明，只在叠加爆表时兜底。
- **重采样**：声音采样率 ≠ 输出采样率时按 `src_rate / out_rate` 步进的线性插值
  （f64 游标累积，长缓冲无漂移）；采样率一致时步进恰为整数、插值系数为 0，
  **逐采样直通**（单测以精确相等断言）。v1 不做抗混叠滤波——主要场景是
  48k ↔ 44.1k 的轻微比率。通道映射：单声道源复制上混、立体声源平均下混。
- **声音生命周期**：槽位 + 空闲链（非紧凑 Vec）——stop / 自然播完只把槽位置空，
  复用时世代 +1。`SoundId` = 槽位下标 + 世代（与 ECS 实体「索引 + 代数」同款），
  stop 旧句柄必然失配返回 `false`，绝不误伤复用槽位的新声音。
- **实时回调 try_lock 原则**：cpal 回调跑在设备驱动的音频线程上，任何阻塞都是
  可闻爆音。回调内 `try_lock` 混音器——锁被主线程持有（play / stop / 手动 mix）
  时**立即放弃并填静音**，绝不等待；主线程侧的锁操作保持短临界区。
- **线程模型**：cpal 0.15 的 `Stream` 是 `!Send`（平台音频句柄不允许跨线程迁移），
  无法由调用方线程代为保活。设备枚举、流构建、`play` 与流保活全部发生在专属
  后台线程内，协商格式（声道 / 采样率）经 channel 传回；`AudioOutput` 被丢弃后
  关闭信号发送端析构 → 后台线程从 `recv` 返回 → `Stream` 析构停流。
- **格式协商**：设备默认输出配置、f32 采样（各平台共享模式的默认格式）；默认
  格式非 f32 时显式降级为纯混音器并告警，不为少见格式手写逐格式转换。
- **WAV 加载**（`wav.rs`）：手写 RIFF 解析器（零新依赖，风格同 `asset/hdr.rs`）——
  支持 PCM 16-bit、声道 1/2、任意采样率；其他编码 / 位深 / 声道数报
  `Unsupported`（诚实报错而非静默出错）；RIFF 块 2 字节对齐、未知块跳过、
  16-bit 小端 → f32 / 32768 全值域映射。
- **程序化音效**（`procedural.rs`）：正弦 / 方波 / 伪随机噪声（xorshift64\\*，固定
  seed 可复现），全部确定性（禁用随机源）；相位由解析式逐采样计算（非增量振荡
  器，无漂移），单测对照解析解逐采样验证（容差 1e-5）；首尾 5ms 线性淡入淡出
  防爆音（时长不足自动缩包络）。
- **wasm32 降级**：cpal 在 wasm 上不可编译（Web Audio 回调模型 + wasm-bindgen
  链路），cpal 依赖整体收敛在 `cfg(not(target_arch = "wasm32"))` 的 target 专属
  dependencies 里，cfg 分支集中在 `iris-audio/src/device.rs` 一处；wasm 分支保留
  同名占位类型，`AudioOutput::device()` 恒返回 `None`——引擎侧 `Audio` 资源
  （iris-engine，包装共享混音器 + Option\<设备\>）自动降级为纯混音器，
  play / stop / set_volume 语义不变，只是不发声。

**空间化 v1（R25，距离衰减 + 恒功率立体声声像）**：

- **纯函数增益模型**（`mixer.rs` 的 `spatial_gain(listener, source, ref)`，
  单独导出可单测）：距离衰减 `att = ref / (ref + d)`——软倒数（d = 0 → 1、
  远处平滑趋零、无硬截断；`ref` 默认 4 m、混音器级可配，`play_spatial`
  播放时刻固化进声音槽位、可按声音覆写）；声像 `pan =
  clamp(dot(normalize(源 − 听者), 听者右轴), −1, 1)` 后做**恒功率映射**
  `θ = (pan + 1) × π/4`、左 = `att·cos θ`、右 = `att·sin θ`（pan = −1 全左、
  0 居中 0.707/0.707——总功率不随声像变化、+1 全右）；源与听者重叠或
  位于正上 / 正下时 pan 无定义，取 0 居中。
- **槽位与听者**：`play_spatial(clip, position, volume, looping)` 在声音
  槽位上额外记 `spatial` 标记 + 世界坐标；`set_listener(ListenerParams)`
  写当帧听者位姿（`from_view(position, forward, up)` 辅助做正交化——俯仰
  俯视的相机得到水平右轴，声像只随水平方位摆动）。混音每帧按**当帧听者
  位姿**重算增益：挪听者 / 挪声源（`set_sound_position`）下一混音缓冲即
  生效；非空间 `play` 恒居中、无衰减、与听者无关。立体声剪辑 v1 只乘
  总增益、不做逐声道声像（源自带左右内容，二次定位反而失真）。
- **已知限制——身后不消音**：无 HRTF / 耳间延迟差，源在正后方与正前方
  完全对称（pan 同为 0、衰减只随距离）——前后不可区分，v1 的空间线索
  只有左右与远近。
- **ECS 接线**（iris-engine `spatial.rs`）：`SpatialListener` 标记组件
  （每世界取查询序第一个，位姿来自实体 `GlobalTransform`，前方 = 实体
  -Z 轴经世界 +Y 重正交化）；`SpatialSource` 持续声源组件（v1 定位为
  **循环环境声**——剪辑 / 音量 / 循环 / 距离参考 + 内部 `SoundId` 状态）。
  `update_spatial_audio` 系统三步：起播（无句柄时）→ 逐帧
  `set_sound_position` 同步位置 → **组件移除检测 = 帧间差分**：上一帧
  活跃的 `(实体 bits → SoundId)` 快照挂在 `Audio` 资源上，本帧查询检不到
  （组件被移除 / 实体 despawn）即 `stop`；实体 bits 作键防悬垂安全（实体
  「索引 + 代数」与 `SoundId` 同款，despawn 后复用下标的新实体代数必变，
  绝不误停）。`GlobalTransform` 缺席的源被跳过。`SpatialPlugin` 注册于
  **PostUpdate、层级传播之后**（与 `ParticlePlugin` 同层、同理由：听者 /
  声源读当帧全局位姿，访问集与粒子不相交可同批并行），已入
  `DefaultPlugins`。**一次性空间音效不走组件**：游戏代码直接
  `audio.play_spatial(clip, pos, vol, false)`——iris-arena 即此用法
  （开火定位枪口、命中定位受击敌人，听者每帧跟随俯视场景相机：
  位姿取相机 `GlobalTransform`、悬于场地上方，pan 由 XZ 平面方位决定；
  竞技场尺度把距离参考放大到 25 m——默认 4 m 会让全场音效衰减到不可闻）。

**音频 v2（R44：OGG/Vorbis 流式解码 + 音乐总线 + 混响）**：

**为什么做**：`AudioClip` 是全量常驻的 f32 PCM（60 s 立体声 48 kHz = 22 MB），
30 分钟配乐用 WAV 是几百 MB——「低占用」卖点在音频侧破功。本轮把「采样从哪来」
抽象成 trait，让 OGG 分块解码喂进一条新的**流式音乐总线**，常驻内存与曲目长度脱钩。

```text
  assets/*.ogg ──► OggVorbisStream（symphonia 分块解码）
                          │ 后台线程 iris-audio-music（只有满队列才阻塞）
                          ▼
  有界块队列 8 × 4096 帧         混音回调 try_lock 三态 poll      ┌─► out（音效之后、限幅之前）
  （生产端 Condvar / 消费端不等待）──────────────────────────► MusicBus ─► Reverb ─► out
                                                                 块拼接 + 线性包络      dry + wet
```

- **OGG/Vorbis 解码**（`ogg.rs`）：依赖 **symphonia 0.5**（feature 只开
  `ogg + vorbis`）。选型硬理由：symphonia 全家族 crate 顶部
  **`#![forbid(unsafe_code)]`**（编译器强制、不公开裸指针 C API）——与本仓库
  「全库零 unsafe」定位一致（同类解码器 `lewton` 含大量 unsafe，故不选）；
  拉取式 `FormatReader::next_packet` + `Decoder::decode` 状态机天然匹配
  「后台线程分块喂混音」；`ogg + vorbis` 子集依赖树最小（arrayvec / bitflags /
  bytemuck / lazy_static / encoding_rs，无 rustfft / regex）。两条消费路径：
  `AudioClip::from_ogg` / `load_ogg`（整条解成交剪辑——短音效，混音路径零改动）
  与 `OggVorbisStream`（实现 `AudioStream`，分块解码喂流式通道）。Vorbis 输出的
  f32 平面按帧交错重排（`L0 R0 L1 R1…`），**值原样搬运不缩放**。坏包跳过继续，
  其他解码错误：一次性路径回传 `AudioError`，流式路径 `log::warn` + 按结束处理
  （后台线程里没有 Result 可回传）。
- **流式通道**（`stream.rs`）：`AudioStream` 契约 = 布局恒定（`channels` /
  `sample_rate`）+ `next_block(&mut Vec<f32>) -> bool`（**追加**语义，块长上限
  针对本次追加量，返回 false 时缓冲里可能仍有最后一段）+ `rewind`（循环能力，
  默认不可循环）+ 「每次调用必须有进展」（连续 8 次空转被解码线程判为流结束）。
  `SampleStream` 把常驻剪辑按块喂出去（流式机制与格式无关 ⇒ 确定性测试全走它）。
  `MusicBus` 语义：新轨硬拆上一轨（v1 单总线不做 crossfade）、淡入淡出是
  **按输出帧推进的线性 f32 斜坡**（秒数在第一次混音拿到输出采样率时才落地）、
  总线音量与包络相乘、`stop_music(fade_out)` 淡出到位或流耗尽即拆轨（**混响尾巴
  继续**，因为混响状态挂在混音器上）。
- **块队列（实时原则的延伸）**：生产者（解码线程）满队列时阻塞在 `Condvar`；
  消费者（混音回调）`poll` 只做一次 `try_lock` ⇒ 三态 `Block / End / Empty`，
  **忙或空即判下溢**，绝不等待（与设备回调的 `try_lock` 填静音是同一条原则）。
  下溢时该缓冲剩余帧**不写音乐采样、游标不推进**，下次混音从原处续播（表现为
  卡顿而非爆音，是解码跟不上实时消费的信号）。生产者退出统一由 `ProducerGuard`
  （`Drop`）标记 `finished` + `producer_done`，panic 也不会让消费者无限期停在
  静音；`stop_music` / 切轨 / 混音器析构 → `close()` 唤醒阻塞中的生产者自行退出
  （`music_producer_finished` 观测、`join_music_producer` 供干净收尾）。线程创建
  失败（wasm32：std 的 wasm PAL 是「桩件直接返回错误」）时退化为**混音调用内
  同步解码**——wasm 本就没有 cpal 实时回调，混音由主线程推进。
- **块拼接的逐位一致性**（本轮的确定性硬账）：取出的块 `extend_from_slice` 接到
  待播缓冲尾部，每次混音后把已消费的**整数帧**从缓冲与游标里剪掉（f64 减整数在
  块长量级下无舍入 ⇒ 游标相位跨块、跨混音调用连续），块边界因此不出现在采样序列
  里；且音乐与音效共用同一个 `SourceFrames::add_frame`（声道映射 + 插值 + 增益
  叠加的数学**收敛成一处实现**，不复制第二份）。验证方式：
  `music_block_splices_are_sample_exact` 以「等待生产者退出 ⇒ 数据全部就位、
  不可能下溢」为前置条件，跨 5 个块、跨 2 次混音调用逐 `to_bits()` 比对
  「输出序列 == 源序列」；`streaming_blocks_match_full_decode` 逐位比对
  「OGG 流式分块拼接 == 一次性整条解码」。采样率不一致时只主张**数值**一致
  （`music_resamples_to_output_rate_like_voices`，容差 1e-6）——两条路径的 f64
  游标累加区间不同，逐位等价只在 ratio = 1（精确整数）时成立。
- **混响**（`reverb.rs`，简化 Schroeder / Freeverb 精简版）：每声道 4 条 comb
  并联求和 → 2 条 allpass 串联，左右用同一延迟整列表的**不同置换** + 不同
  allpass 表展宽；表以 44100 Hz 为基准按 `round(base × rate / 44100)` 缩放
  （换采样率保持时间尺度）。comb：`y[n] = b[n−M]`、`b[n] = x[n] + g·y[n]`，
  `g = COMB_FEEDBACK[k] × room_size` ⇒ 单位冲激下 `y[kM] = g^(k−1)` **严格成立**
  （单测逐采样锁定），`RT60 = ln(0.001) / ln(g) × M / rate`；allpass：
  `y[n] = b[n−M] − g·x[n]`、`b[n] = x[n] + g·y[n]` ⇒ 冲激响应
  `[−g, …, 1−g², …, g(1−g²), …]`、Σy² = 1。输出 `dst += dry + wet × 湿项`，
  `wet = 0`（默认）为**逐位透明的旁路**，混音器据此直接跳过整条混响路径
  （不拷 scratch、不做无谓浮点运算）。纯 f32 乘加 + 整数环下标 ⇒ 同输入逐位
  一致、且**与混音分块大小无关**（两条都有断言）。v1 只挂音乐总线，不做
  Freeverb 阻尼低通 / 预延迟 / 逐声音 send。
- **内存实测**（`cargo run -p iris-audio --release --example music_memory`，
  60 s 立体声 48 kHz 同内容对比；流式侧按「队列容量 × 实测块长 + demuxer 读缓冲」
  计账，**不是** RSS 直测）：

  | 形态 | 常驻字节 | 说明 |
  |---|---|---|
  | WAV 全量（f32 混音态） | 23,040,000（22.0 MB） | 采样数 × 4 |
  | WAV 磁盘（16-bit PCM） | 11,520,044（11.0 MB） | 头 44 + 采样数 × 2 |
  | OGG 压缩体积 | 704,070（0.7 MB） | fixture 实测 94 kbps 外推 |
  | **流式常驻（`open(path)`）** | **429,568（0.41 MB）** | 8 块队列 + 待播块 + 64 KB 读缓冲 |
  | 流式常驻（`from_bytes`） | 1,133,638（1.08 MB） | 上面 + 整条压缩码流常驻 |

  ⇒ **常驻比 54×**（open 路径）/ 20×（bytes 路径），且计账式子里没有「曲目时长」
  这一项（长度无关是结构性的：队列有界 + 块长固定）。示例自带 `ratio ≥ 20` 的
  回归断言。
- **门面与测试资产**：iris-engine `Audio` 转发 `play_music` / `stop_music` /
  `set_music_volume` / `music_is_playing` / `music_handle` / `set_reverb` /
  `reverb_settings`（headless 与有设备同语义：无设备时照常推进、可离线断言）。
  真实资产 `crates/iris-audio/tests/fixtures/tone_2s.ogg`（ffmpeg libvorbis 编码
  的 48 kHz 立体声 2 s 正弦 + 噪声，23 KB / 94 kbps，实测解出恰好 96000 帧；
  集成测试文件头写明重生成命令）。v1 限制清单见 **§8.23**。
- **编码管线（打磨 VI 摩擦④：明文接受外部工具链，引擎不做编码器）**：
  运行时只需要**解码**（`AudioClip::from_ogg` / `OggVorbisStream`）；
  Vorbis 编码器要给 `iris-audio` 加一整个外部依赖（symphonia 只有解码），
  换到的却是「游戏运行时能吐出自制音效文件」这种没人要的能力——编码是
  **资产生产期**的事，归工具链。标准命令（README「环境要求」同步收录）：

  ```sh
  ffmpeg -i in.wav -c:a libvorbis -q 4 in.ogg   # 16-bit PCM WAV → OGG
  ```

  本机实测（48 kHz 单声道 1 s 正弦）：88,278 B WAV → 5,391 B OGG（16.4×），
  `ffprobe` 确认容器编码 `vorbis`；引擎侧对该命令产物的解码路径由
  `tone_2s.ogg` fixture 的同源集成测试覆盖。转换结果经 `PackWriter`
  进 pack（§7.3）即完成「自制音效 → 运行时播放」的资产闭环。

### 4.15 场景序列化（iris-engine/scene）

**关卡数据与游戏代码分离**：JSON 场景描述文件 ↔ 世界实体（`load_scene` /
`save_scene`，iris-engine 门面层——场景横跨 mesh / material / 灯光 / 物理，
正好是门面的职责）。serde derive（零新依赖：serde / serde_json / image 均在
iris-render 的 glTF 管线依赖树内，Cargo.lock 无新版本号）。

**格式设计（全蛇形命名）**：两类内容——

- **环境段**（场景级资源形态，非实体）：`directional_light`（方向 / 颜色 /
  强度 / 环境光 / 阴影开关，各字段可选、缺省 = 引擎默认；`shadow_range` v1
  不序列化）与 `sky_source`（Radiance `.hdr` 相对路径 → `SkySource::equirect_hdr`，
  缺席 = 程序化天空）。缺席的环境段**不改动**既有资源（方向光常由代码按关卡
  氛围覆盖）。
- **实体段**（必需，逐实体可选字段，缺省值全部与引擎默认一致——`Transform`
  恒等、材质 `MaterialDef::default`、点光照 `PointLight::default`，手写关卡
  只写与默认不同的字段）：`name` → `Name`；`visible: false` → `Visibility::HIDDEN`
  （缺省可见，不插组件——与可见性的缺席语义一致）；`transform`（位置 /
  四元数 (x,y,z,w) / 缩放）；`material`（color / metallic / roughness /
  emissive（RGB，引擎侧 alpha 恒 1）/ texture 路径）；`point_light`；
  `collider`（`shape`: `{"sphere": {radius}}` / `{"box": {half_extents}}` +
  `static` + `restitution`）；`rigidbody`（gravity_scale / friction /
  linear_damping）。serde 默认忽略未知字段——新版本文件在旧引擎上可加载
  （前向兼容，单测守护）。

**mesh 三态**：`{"primitive": {kind: cube|plane(size)|sphere(radius, segments,
rings)|cylinder(radius, height, segments)}}`（图元直通 `primitives` 生成器，
分段数低于生成器断言下限提前报 `OutOfRange` 而非 panic）；`{"gltf": 路径}`
整实例挂载——`spawn_gltf_instance` 的顶层实体作为该实体**子实体**（本实体
transform 作用于整棵子树），实例生成的全部实体打 `GltfMounted` 标记；缺 mesh
= 纯逻辑 / 灯光实体。渲染收集查询要求 `(MeshHandle, MaterialHandle)` 成对，
无材质段的图元实体挂默认材质 `MaterialHandle(0)`（0 号槽位 = 纯白兜底）。

**登记表往返机制**：`MeshStore / MaterialStore / TextureStore` 只存数值与像素，
没有「这份数据来自哪个文件」的概念——`SceneAssetRegistry`（World 资源）在
load 时登记「场景创建的句柄 ↔ 场景文件内的来源」（图元参数 / glTF 路径 /
贴图路径 / 天空源路径），save 时据此还原资源段：

- mesh 句柄有登记 → 还原 `{"primitive": ...}` / `{"gltf": ...}`；无登记
  （用户代码 `MeshStore::add`）→ mesh 段省略 + 数量统计告警（降级而非失败，
  其余实体照写、产出文件可再加载）；
- material：句柄 0（默认材质，隐式）整段省略；有登记 → 数值字段照
  `MaterialDef` 现读（保存「当前值」）+ 登记的贴图路径；无登记 → 数值照写 +
  贴图路径丢失告警（纯数值材质无损）；
- 天空源为 `EquirectHdr` 且登记了路径 → 写 `sky_source`；无登记（手工挂入）
  → 省略 + 告警（HDR 图像内容不回写文件）。

**往返不变量**：实体顺序 = 组件存储序（spawn 序，稳定）；`save(load(f))`
再 save 一次**逐字节一致**（同输入确定性输出），`load(save(load(f)))` 与
`load(f)` 实体组件语义等价（数值 1e-6 容差、mesh ref 相等——单测以丰富场景
（全组件 + glTF + 贴图）守护）。glTF 内部实体带标记被 save 跳过——它们由
glTF 段在下次加载时整体重建，不属于场景文件本身。错误收敛为 `SceneError`
（Io / Json / Missing / Unsupported / OutOfRange，风格同 `GltfError`；
glTF / HDR 错误经 `From` 映射保上下文）。

**v1 不含（诚实清单）**：UI 组件（HUD 属游戏逻辑）与相机（资源级）不进
场景文件；克隆 / 预设 / 组件插件扩展点；`RigidBody.velocity` 与
`DirectionalLight.shadow_range` 不序列化（物体从静止开始）；同一 glTF 文件
被多次引用时每引用重新解析（各得独立实体树与网格句柄——句柄级去重需跨实例
复制异构组件树或共享实体，后者会被 `add_child` 重挂载破坏，v1 不做）；
无热重载（§8.4）；文件读写走 `std::fs`（wasm32 编译可用、web 运行时不可用，
与引擎资产管线平台约定一致）。

### 4.16 透明渲染（R23）

**材质分区**：`MaterialDef::alpha_mode`（`Opaque` 默认 / `Blend`）在
`collect_instances` 收集期把实例分进四个列表——不透明普通（键排序成批，
语义与透明支持之前**完全一致**）、不透明蒙皮、透明普通、透明蒙皮。
实例缓冲布局：`[不透明普通 | 不透明蒙皮 | 透明普通（视深序）| 透明蒙皮（视深序）]`，
跨帧增量上传比对覆盖全部四段。

- **管线变体**：透明普通 / 透明蒙皮与不透明变体共用同一份 WGSL 与
  顶点布局，仅两点不同——混合 `BlendState::ALPHA_BLENDING`
  （`src α, 1 − src α`，与 UI 管线一致；α 取 `base_color.a`，即着色器
  输出的 alpha 通道）与**深度只测不写**（`Less` 测试保留、写入关闭）。
  深度写入关闭的理由：透明 pass 内后画的片元可能比先画的更远（按包围
  球中心排序只是启发式），写入深度会把更远但尚未绘制的透明面错误裁掉
  （共面透明对必然丢失其一）；只测不写则透明仍被不透明几何正确遮挡。
- **视深排序**：透明项按**视深降序（远 → 近，back-to-front）**绘制——
  alpha 混合不满足交换律，必须先画远处，近面的 `α·src + (1−α)·dst`
  才能落在远面结果之上。视深 = 包围球中心（网格 bounds 中心，无 bounds
  回退模型平移列）经视图矩阵变换后的 `-z`；透明蒙皮取「关节矩阵变换后
  角点并集 AABB」的中心（跟随骨骼实际位移，复用 §4.4 的保守剔除盒）。
  深度并列按实体 bits 升序——比较器因此为全序，`sort_unstable` 结果
  不依赖查询顺序（确定性）；并列项视深相同，先后只影响并列项之间的
  叠放，视觉等价。
- **绘制与合批**：透明 pass 位于主 pass（天空 + 不透明 + 蒙皮）之后、
  泛光 / 色调映射 blit 之前，写同一 HDR MSAA 颜色目标
  （`LoadOp::Load` 继续混合，pass 结束的 resolve 与主 pass 同一目标——
  MSAA 内容完整保留，再解析的结果 = 主 pass 结果 + 透明混合），因此
  透明**参与 HDR / 泛光 / ACES**（与 HUD 的 blit 后叠加形成对照）。
  普通透明按「深度序中相邻同 (网格, 材质) 键」合并为 run 批（同一材质
  的多块板纵深排列仍是一个 draw call）；透明蒙皮每实体一次 draw
  （与不透明蒙皮同构，组 2 关节矩阵）。无透明实例时整个 pass 跳过。
- **深度附件 Store 的教训**：主 pass 的深度附件必须 `StoreOp::Store`——
  透明 pass 以 `LoadOp::Load` 读同一深度附件做遮挡测试，而 `Discard`
  在 pass 结束后把附件内容置为**未定义**（DX12 实测为 0，`Less` 比较
  全挂）——症状是透明整体消失、画面只剩不透明几何。此前没有任何
  pass 读主深度，`Discard` 是安全的写回优化；R23 起改为 `Store`。
- **透明不投影**：方向光与点光阴影 pass 只遍历不透明实例区间——收集期
  分区天然排除透明项。理由：半透明几何不该在地面上投出**实心**阴影
  （贴图深度不分 alpha；正确的透明阴影贡献需要分离投影或 alpha 感知的
  深度，v1 不做），「玻璃不挡光」也比「玻璃投实心黑影」更不易察觉。
- **接入**：glTF `alphaMode: "BLEND"` → `Blend`（`"MASK"` 需要 alpha
  cutoff 逐像素裁剪，v1 按不透明处理）；场景 JSON 材质段新增
  `alpha_mode` 字段，缺席反序列化为 `Opaque`（R23 之前的旧场景文件
  直接兼容），不透明时序列化省略（旧文件重存字节级不变）。
- **验收**：`transparent_render.rs` 五组用例——混合正确性（参考渲染法，
  自发光材质使 HDR 合成值可解析断言）、排序正确性（三层 α=0.5 通道
  比对 `(αE, α²E, α³E)`，顺序颠倒通道互换必挂）、深度只测不写
  （共面透明对两块都必须出现——写深度则后画板被裁）、透明不投影
  （开/关阴影差分 ≈ 0 + 不透明对照版排除假阴性）、同键 run 合批
  （FrameStats 断言）。CPU 单测覆盖分区与视深排序（renderer.rs）与
  旧场景兼容（scene.rs）。验收图 `docs/transparent.png`
  （`transparent_snapshot` 示例：玻璃罩 + 自发光内核 + 三色板纵深排列）。

### 4.17 粒子系统（R24）

**组件与模拟**（`iris-render/particles.rs`）：`ParticleEmitter` 组件 =
参数（混合模式 / 发射速率 / burst / 寿命 / 初速与方向抖动 / 重力缩放 /
尺寸与颜色起止插值 / 池上限）+ 私有活粒子池。发射原点 = 实体
`GlobalTransform` 平移，粒子出生即存**世界坐标**——发射器移动天然留拖尾。
模拟为**纯 CPU**（`advance` 不触世界，可脱离世界单测），每帧由
`update_particles` 系统推进（半隐式欧拉：先更新速度再积分位置 → 寿命
剔除 → burst 一次性爆发 → 持续发射）。

- **确定性**：出生方向由 splitmix64 哈希流派生（发射器内全局递增计数器
  ⊕ 实体 `Entity::to_bits` 种子；**无 rand 依赖、无全局状态**）——同一
  输入序列两次模拟**逐位一致**（单测按位型比对守护，与物理子系统同级
  语义，可回放）。方向 = normalize(+Y + jitter·u)（u 球面均匀，(z, θ)
  参数化）：jitter = 0 竖直上抛（喷泉主轴），≈1 张角覆盖上半球，≫1
  趋近全向球面（爆炸预设取 2.6）。持续发射用**累计分数法**
  （rate·dt 取整、小数余量滚存）——任意 dt 序列下出生总数收敛于
  rate·总时长，计数确定。
- **池满语义**：池达 `max_particles` 时**丢弃新粒子**（保老弃新——持续
  发射与 burst 同语义，不挤掉旧粒子；池规模即视觉效果上限，行为可预期）。
  被丢弃的发射尝试同样推进哈希流计数器（确定性与池满与否解耦）。
- **发射器寿命**：`emitter_lifetime <= 0` 永久；> 0 时耗尽后停止持续
  发射（已出生粒子存活到各自终点）。burst 在首次 update 一次性触发，
  不受寿命门控、dt = 0 亦触发（粒子定格供渲染）。
- **重置与命令式爆发（打磨 III）**：`reset()` 清空活粒子池、burst 重新
  上膛、发射余量 / 发射器年龄 / 哈希流计数器归零——参数字段不动的前提
  下与刚构造的同参数新实例逐位等价，`advance` 行为完全一致（对象池槽位
  复用的正道：FX 槽位触发 = 覆写参数字段 + reset，无需销毁重建组件）；
  `trigger_burst(count)` 把命令式爆发记账到下一次 `advance` 一次射出
  （多次累加、消耗后不补发，不改 `burst_count` 配置语义），用于运行期
  即时触发的特效。
- **单遍系统**：`update_particles` 在一次查询内以 `&mut ParticleEmitter`
  直接推进——模拟只写发射器**自身内部**的粒子池，无跨组件借用（对照
  `update_animations` 的两阶段：它需要写**其他实体**的 Transform）。
- **构造辅助**：`ParticleEmitter::fountain()`（持续水柱、alpha 混合）与
  `ParticleEmitter::explosion()`（一次性全向爆发、additive + HDR 暖色）。
- **集成**：`ParticlePlugin` 注册 `update_particles` 于 **PostUpdate、
  `HierarchyPlugin` 之后**（发射原点取当帧传播后的全局位置），渲染在其
  之后进行——「当帧模拟当帧成像」。场景序列化 v1 留白（JSON 场景无
  粒子段，代码生成）。

**渲染**（`renderer.rs` 粒子 pass + `shaders/particle.wgsl`）：

- **Billboard 实例化**：实例 = (世界中心 xyz, 尺寸 w) + 线性 RGBA
  （32 字节/粒，`ParticleInstance`）。顶点阶段由 `vertex_index` 把 4 顶点
  展开为 triangle-strip（**无顶点缓冲**），位置 = center +
  (right·x + up·y)·size——相机 right / up 从视图矩阵第 0 / 1 行提取，
  经 32 字节小 uniform（`ParticleCamera`，每帧 write_buffer）进组 1，
  quad 恒面向相机。片元做**径向软衰减** `falloff = (1 − r²)²`（quad 内
  UV 距中心，中心 1 → 单位圆边缘 0 → 角落钳 0；曲线写死免贴图），衰减
  只乘 alpha——加法混合下即调制叠加亮度成光点，alpha 混合下即软边
  精灵，无「硬方块」感。
- **双混合管线**：additive（`src·SrcAlpha + dst·One`——加法交换律，
  实例顺序无关、不排序）与 alpha（标准 `ALPHA_BLENDING`，实例按**视深
  降序**（远 → 近）排序，比较器与 R23 透明同款 `total_cmp` + 收集序号
  并列断言——全序、结果与收集顺序无关）。都深度**测试开（Less）/
  写入关**（与透明管线同参：不遮挡后续几何、互相重叠不被裁）。
- **pass 顺序**：透明 pass 之后、泛光之前——粒子叠加在玻璃等透明几何的
  **混合结果**之上（「后画者在上」由 pass 顺序统一裁决），同时高亮
  additive 粒子（HDR 亮度 > 1）参与泛光亮度提取，与自发光几何同权。
  写同一 HDR MSAA 目标（Load 混合）；深度附件 Load（被不透明几何挡住
  的粒子照常被遮挡）。additive 段先画、alpha 段后画，各一次 draw；
  无活粒子时整个 pass 跳过（零开销）。
- **缓冲与上传**：粒子实例走**独立缓冲**，容量不足按 2 倍扩容重建
  （照实例缓冲模式）；**每帧全量上传**——粒子位置 / 尺寸 / 颜色逐帧
  全变，帧级比对无增量价值（对照 §4.2 几何实例的 diff 增量上传；
  增量上传留待后续亦无收益）。
- **剔除与统计**：剔除按**发射器包围球**（活粒子 AABB 包围球 + 最大
  尺寸边距，R22 同款保守界——只多画不漏画），被剔发射器整批不画且
  不计入 `culled_instances`（该字段口径为几何实例，粒子剔除口径独立，
  见 §4.4.1）。**收集查询带 `Option<&Visibility>`（打磨 III）**：挂
  `HIDDEN` 的发射器（或任一祖先隐藏，层级级联 §4.4）整池跳过——组件
  缺席 = 可见，与几何收集同语义；隐藏只是渲染过滤，`update_particles`
  照常推进、粒子继续老化。可见粒子计入 `instances` / `draw_calls` / 
  `triangles_drawn`（每粒 2 三角形），并单列 `particles_drawn`
  （= 各可见发射器活粒子数之和，可与池状态对账）。**粒子不进阴影
  pass**（与透明同理由）。
- **验收**：CPU 单测 9 项（逐位确定性、发射计数精确性 / 余量滚存、
  半隐式欧拉解析对照、寿命剔除 / 寿命耗尽 / 池满、burst 一次性、哈希流
  健全性、渲染插值、reset 逐位还原 + burst 重新上膛、trigger_burst
  确定性与累加）；GPU 集成 `particles_render.rs` 4 项（喷泉可见性 +
  `particles_drawn` 与池状态对账、additive 亮度显著高于 alpha 混合的
  可区分断言、爆炸 one-shot 全灭后不补发、隐藏发射器整池跳过 + 级联
  隐藏 + 恢复重现）。验收图
  `docs/particles_fountain.png` / `docs/particles_explosion.png`
  （`particle_snapshot` 示例）。

### 4.18 纹理 BC1 压缩与内存占用统计（R30）

**上传决策树**（`texture_bcn::bc1_eligible`，渲染器 `ensure_texture` 与
`TextureStore::gpu_bytes_estimate` 共用同一纯函数，单一事实来源）：

```text
设备启用 Features::TEXTURE_COMPRESSION_BC（Gpu::supports_bc，初始化时
    按适配器能力显式请求——wgpu 压缩格式要求设备特性）
  且 尺寸本身 4 对齐（wgpu 压缩纹理块对齐；编码器的 pad 能力不在上传
    路径启用——pad 区是复制的边缘像素，贴边有渗色风险，v1 保守）
  且 全不透明（alpha 恒 255——BC1 无 alpha 通道，半透明纹理压缩即丢语义）
→ 上传 Bc1RgbaUnormSrgb（4×4 块 8 B = 0.5 B/px，对 RGBA8 的 8 倍节省）
否则 → Rgba8UnormSrgb 照旧（4 B/px）
```

- **无分支回退**：wasm 的 WebGPU 实现普遍无 BC 特性、native 老 GPU 同样
  可能没有——不写任何 cfg，运行时特性检测天然覆盖（web 自动 RGBA8），
  与「wasm 只需编译可用 + 优雅降级」的平台策略一致。
- **编码器**（`texture_bcn::encode_bc1`，纯函数、全整数运算、零 unsafe）：
  逐 4×4 块取 RGB 距离最远的两像素为端点（O(16²) 确定性暴力，与
  PCA / 范围法的取舍见模块文档——质量略差但逐位可复现），按亮度排序
  c0 > c1，RGB565 量化（四舍五入 + 位复制展开），像素索引取四色调色板
  （两 565 端点 + 两个 1/3 插值）中最近项、并列取最小索引。**确定性
  契约**：同输入两次编码逐位一致（单测守护）——回放 / 差分测试的硬
  要求。纯色块往返零误差（端点同色 → 调色板退化、索引无歧义）；位打包
  布局有手工构造块的逐字节断言。尺寸非 4 倍数统一**预 pad**（边缘像素
  复制）写死——「向下取整块数」丢像素、「末块按实际像素」产出非法
  BC1 流，pad 是唯一能产出完整合法块阵列的方案。
- **质量护栏**（单测）：纯色 565 精确色黑/白/三原色/三补色往返误差 = 0；
  温和灰度渐变块平均误差 < 4/255；GPU 集成测试 `bc1_render.rs` 用
  「不透明副本 → BC1 路径、alpha=100 副本 → RGBA 回退」的同画面 A/B
  （不透明材质不消费纹理 alpha，两副本画面等价，像素对比即纯格式对比）
  断言全帧平均误差 < 2/255（块内纯色棋盘格解码精确，实测 0）。
- **mipmap v1 不做**：引擎无 mipmap 机制（`mip_level_count` 恒 1），
  BC1 上传为单 mip；引入 mipmap 时编码器补 mip 链生成（接口另议）。

**内存占用统计**（「低占用」卖点的观测口径）：

- `TextureStore::memory_bytes()`：Σ 源 RGBA 字节；`MeshStore::memory_bytes()`：
  Σ（顶点 32 B + 索引 4 B + 蒙皮属性 32 B）——亦即 GPU 网格缓冲的准确
  上界估算（顶点/索引/蒙皮缓冲与 CPU 源同尺寸）。
- `TextureStore::gpu_bytes_estimate(feature_bc)`：GPU 侧显存占位**估算**
  （驱动可能对齐分配，非精确值；mipmap 无，单 mip 计），逐纹理按上传
  决策树计（BC1 0.5 B/px 或 RGBA 4 B/px）；`false`/`true` 两次调用之差
  即压缩节省。`Renderer::texture_is_bc1(handle)` 观测逐纹理实际上传
  格式（测试与内存对账用）。

**实测数据**（开发机：RTX 4060 Laptop GPU / Vulkan / Windows，release
构建，`memory_bench` 示例，量级参考非承诺值）：

| 口径 | 字节 | 说明 |
|------|------|------|
| CPU 源纹理（4 × 2048² RGBA8 + 1×1 兜底） | 67,108,868 B（64.00 MiB） | `TextureStore::memory_bytes` |
| GPU 上传估算（无 BC 特性，全 RGBA8） | 67,108,868 B（64.00 MiB） | `gpu_bytes_estimate(false)` |
| GPU 上传（BC1，本机实际路径） | 8,388,612 B（8.00 MiB） | `gpu_bytes_estimate(true)`，实测 `texture_is_bc1` 全真 |
| **压缩比** | **8.00x（显存 -87.5%）** | 理论值 8x，程序化棋盘格无 pad 损耗 |
| 高模 uv_sphere(64, 48) | 6,144 三角形 / 175,648 B | 网格对之一 |
| 低模 uv_sphere(8, 6) | 96 三角形（1.6%） | 64x 削减 |
| LOD 近景帧 `triangles_drawn`（8 实体） | 49,152（8 × 6144） | eye 距离 15–50 m ≤ 60 m |
| LOD 远景帧 `triangles_drawn` | 768（8 × 96），**-98.4%** | eye 距离 205–240 m > 60 m；instances / draw_calls 不变 |

### 4.19 网格距离 LOD（R30）

- **组件**：`LodMesh { high, low, switch_distance }`（iris-render，
  Component 派生）。挂载实体**替换** `MeshHandle` 参与渲染收集（组件
  自带两个句柄，不与 `MeshHandle` 并存）；收集查询为
  `Option<&MeshHandle>` + `Option<&LodMesh>` 双可选——有 `LodMesh` 按距离
  选一个句柄参与合批排序键 / 视锥剔除 / 透明排序 / 三角形统计，无则
  `MeshHandle` 旧语义，两者皆无的实体静默跳过（不可渲染）。蒙皮路径
  同口径（高低模各自带蒙皮属性时 LOD 随切换生效）。
- **距离**：实体 `GlobalTransform` 平移到相机 eye 的欧氏距离（v1 用实体
  原点而非包围球中心——省每帧包围盒展开，对称网格原点即中心，偏心
  网格边界略保守）；`dist <= switch_distance` 用 high（边界归近侧），
  否则 low。选择为纯函数 `LodMesh::select`（单测覆盖边界与方向无关性）。
- **验收口径**：`FrameStats::triangles_drawn` 天然成为 LOD 指标——GPU
  集成测试 `lod_render.rs` 断言同一实体近/远两帧三角形精确等于选中
  网格的三角形数、两帧均可见且近帧像素更多（低模像素宽松证据）。
- **无滞回**（v1 限制，见 §8）：实体距离在 `switch_distance` 边界抖动时
  两网格可能逐帧来回切换；需要时游戏侧可给切换距离做时间平滑。

### 4.20 盒投影贴花（R39）

**组件与数据**（iris-render `decal.rs`）：`Decal { texture, size, tint }`
组件——实体变换定义**投影盒**（局部 Z 轴 = 投影方向，XY 为贴花平面，
`size` = 盒三轴尺寸，Z 常较薄），纹理 RGBA 的 A 通道控制覆盖。弹孔 /
血迹 / 地面标记类表面装饰的正向前向标准方案（decal cube projection）。

- **代理几何**：单位盒的 **±Z 两块端盖**（8 顶点 / 12 索引，仅位置属性，
  初始化时一次性上传的静态小缓冲）——**不画整盒侧面**：侧面的局部坐标
  恒在盒面上，`UV = local.xy + 0.5` 会把纹理边缘 texel 拉伸成一圈
  「光晕」；端盖即投影平面，CCW 正面朝盒外（Cull Back 下相机在哪侧只见
  哪侧端盖）。
- **盒投影着色**（`shaders/decal.wgsl`）：顶点经逐实例世界盒矩阵变换；
  片元把插值世界位经**逆世界盒矩阵**（CPU 预计算，着色器零矩阵求逆）
  变回盒局部，`|local| > 0.5 + 1e-4` discard（盒裁剪），`UV = local.xy +
  0.5` 采样贴花纹理 × tint，标准 alpha 混合输出。**容差 1e-4 是实测教
  训**：端盖的 `local.z` 恒 = ±0.5（恰在盒面上），inv·model 往返的末位
  浮点误差会让无容差的边界比较随矩阵舍入方向**整片 / 条带状误 discard**
  （同一场景仅改盒尺寸即从正常变整枚消失）；0.1 mm 级容差远大于 f32
  噪声、远小于视觉相关。**推荐摆放**：盒中心沿 +Z 抬 `size.z/2` 使端盖
  与表面重合，再向相机侧加毫米级裕量规避共面深度抖动（深度测试为
  **LessEqual**——等深必须通过，`Less` 会闪烁；深度写入关闭，不污染后
  续 pass 读取的不透明深度）。
- **实例数据**：`DecalInstanceRaw` = model（64B）+ inv_model（64B）+
  tint（16B）= **144B/实例**（9 × `Float32x4`，location 1..=9；布局断言
  锁定）。inv_model 在 CPU 求逆传入——片元需把插值世界位变回盒局部，
  顶点级求逆不可行、逐片元求逆更不可行；tint 逐实例携带，同纹理不同乘
  色的贴花不拆批（与规范的偏离说明见 decal.rs：规范给定的 128B 未含
  tint 通道，进实例数据是唯一不破坏「按纹理分批」的承载方式）。
- **绘制序（蒙皮之后、透明之前）**：贴花是**不透明表面的装饰**，属于场
  景「实体」的一部分；玻璃等透明物应盖在贴花的**混合结果**之上（透过
  玻璃看到的墙面弹孔带玻璃色）——「后画者在上」由 pass 顺序统一裁决，
  与 §4.16 透明、§4.17 粒子同构（链路：主 pass → 贴花 → 透明 → 粒子）。
  写同一 HDR MSAA 目标（LoadOp::Load 继续混合 + 再 resolve，贴花参与
  HDR / 泛光 / ACES）；**深度附件 Load + Store**（非透明 / 粒子 pass 的
  Discard）：贴花不写深度，但其后的透明 pass 仍要 Load 同一深度附件——
  Discard 会把附件内容置为未定义（§4.16 的 DX12 教训），Store 未变更
  内容是严格定义的零成本透传。
- **收集与剔除**：查询 `(Entity, &GlobalTransform, &Decal, Option<&Visibility>)`
  ——Visibility 过滤与层级级联同全部收集路径（§4.4，组件缺席 = 可见，
  任一祖先 `HIDDEN` ⇒ 跳过）；视锥剔除按**变换后 AABB**（单位盒 8 角点
  经世界盒矩阵变换取包围盒，§4.4 同款保守界——薄盒旋转后包围球半径远
  大于实际覆盖，AABB 明显更贴合），被剔计入 `culled_instances`；size 三
  轴必须为正（debug 断言——零/负分量使盒矩阵不可逆）。
- **分批（按纹理）**：收集后按纹理句柄排序分组，同纹理连续实例区间
  合并为 **1 draw**（批内仅切换组 1 纹理绑定组）；批内实体 bits 并列序
  （确定性，与收集顺序无关），同批重叠贴花的叠放先后未做视深排序（贴
  花常为不透明深色标记，v1 接受并列序；与透明几何的先后由 pass 顺序保
  证）。贴花实例走**独立缓冲**（容量不足 2 倍扩容，照粒子模式）、每帧
  全量上传——实例少且常静止，帧级比对无增量价值。无贴花实例时整个
  pass 跳过（零开销默认路径）。
- **v1 限制（诚实清单，见 §8）**：无光照（输出 = 纹理 × tint，不参与
  PBR / 阴影 / IBL——弹孔血迹类深色标记语义足够，半亮环境下贴花比表面
  「亮一档」，深色标记上不可察觉）；平面假设（端盖是投影平面，深盒贴
  斜面时贴合偏差上界 `size.z/2`，基于深度缓冲重投影的屏幕空间 decals
  留待后续）；不进阴影 pass（表面装饰而非几何，同透明 / 粒子理由）。
- **验收**：`decal_render.rs` 七组 GPU 用例——投影正确（棋盘格贴花，
  盒内红白格与 UV 方向精确对应、盒外纯绿）、盒裁剪（投影足迹外 2px 环
  带零采样，区域差分）、alpha 混合精确（50% alpha 纯色叠已知色，参考渲
  染法 ±2/255）、tint 乘色与整体 alpha（±2/255）、绘制序（玻璃合成在贴
  花之上，参考 = 解析合成 glass ∘ decal ∘ wall，序错必挂）、Visibility
  自身隐藏与祖先级联（统计 + 像素双断言）、无贴花场景 FrameStats 与
  R38 基线一致（零开销路径）。CPU 单测覆盖 144B 布局 / 代理几何环绕方
  向 / CPU 求逆往返。验收图 `docs/decal.png`（`decal_snapshot` 示例：灰
  墙 3 弹孔 + 地面血迹 + 斜面旋转盒贴花）。

### 4.21 多视口与渲染到纹理（R40）

**组件与存储**（iris-render `rendertarget.rs` + `ui.rs`）：`RendersTo`
组件挂在相机实体上（须同时挂 `ActiveCamera`），把该相机输出到一张离屏
RT；`RenderTargetStore`（World 资源，**不进 DefaultPlugins**——游戏侧按需
`insert_resource`，缺席 = 无 RT 相机 = 主路径零变化）保存 CPU 侧尺寸描述
+ `RenderTargetHandle`，GPU 纹理由 [`crate::renderer::Renderer`] 按句柄
惰性创建并跨帧持有（`GpuRenderTarget`：颜色 + 深度 + 线性 HDR 中间缓冲 +
ACES blit 绑定组 + RT 相机专用全局 / 实例缓冲）。

**RT pass 语义（v1，小地图 / 监控屏）**：
- **相机收集**：渲染器收集全部 `(ActiveCamera + RendersTo)` 实体，每台
  RT 相机渲一次到对应纹理；**主视图 = 无 `RendersTo` 的相机**（选择逻辑
  在既有基础上加 `Option<&RendersTo>` 过滤——无 RT 实体时行为与 R39
  逐位一致，见下）；只有 RT 相机时主视图回退 `Camera` 资源（原语义）。
  数量上限 [`RENDER_TARGET_MAX`] = 2，超出 `log::warn` 跳过。
- **几何**：不透明 + 蒙皮 + 透明（实例区间与主 pass 同布局），用 RT 相机
  **自己的视锥做剔除**（复用 `collect_instances` 的视锥参数化，收集 /
  排序与主 pass 同款 `total_cmp` 全序 + 并列断言——确定性）。**天空盒 /
  贴花 / 粒子 v1 不画**（诚实限制，见 §8.20）。
- **阴影**：复用主阴影图 / CSM 与点光 cubemap（`csm_params` 仍按主相机
  拟合——RT 相机朝向不同时是 v1 限制，见 §8.20）。
- **输出**：几何渲到 RT 专用**线性 HDR 中间缓冲**（`Rgba16Float`、单采样
  无 MSAA——像素化小地图天然契合），再经 `rt_blit_pipeline` **ACES 色调
  映射**（泛光强度恒 0 / 曝光 1 / 暗角 0——无后处理链）到 `Rgba8UnormSrgb`
  颜色纹理。**无 UI**（UI 只叠在主视图 blit 之后）。
- **FrameStats 口径 = 主几何**：RT 辅助视口不计入 `draw_calls` /
  `instances` / `triangles_drawn` / `culled_instances`（见 §4.4.1）。
- **主视图零变化保证**：默认场景（无 RT store = 无 RT 相机）时 RT 相机
  收集为空、整段 RT 逻辑零执行——主 pass 像素与 R39 逐位一致；有 RT 相机
  但无 UI 时主 pass 像素亦不受影响（集成测试
  `main_view_pixels_unchanged_with_rt_camera` / `no_rt_store_keeps_baseline`
  逐像素锁定）。

**UI 图片控件**（ui.rs + ui.wgsl）：`UiImage { texture }`（普通纹理）与
`UiImageRtt { target }`（RT 颜色纹理）组件，定位复用同实体 `UiRect`
（position / size 像素，`UiColor` 乘色、缺省白）。UV 恒为全屏 0..1 直角
采样（Nearest，无抗锯齿 v1——像素化小地图天然契合）。**绘制顺序 = 图片
之下、文本之上**：图片先于 atlas（矩形 + 文本）绘制于同一 `LoadOp::Load`
pass，「后画者在上」由 pass 内顺序统一裁决（小地图底图应垫在 HUD 文字
下方）。图片按源各持独立组 0 绑定组（复用 `ui_layout`：纹理 + `ui_sampler`
+ 屏幕 uniform），逐图 draw；atlas 内容仍合为单 draw。**`UiRect` 仅作定位
/ 尺寸载体**：挂 `UiImage` / `UiImageRtt` 的实体与 `UiText` 同语义，不再
绘制缺省白矩形（否则盖住图片，集成测试 `ui_image_draws_texture_pixels`
实测抓出）。

### 4.22 演出时间线 Sequencer-lite（iris-engine/timeline，R47）

写实动作游戏剧情演出的最小骨架：轨道时间线 + 播放器，模块在 iris-engine
门面层（演出驱动相机与 UI，跨渲染边界；数据纯结构、采样纯函数，零 unsafe）。

**轨道模型**（`Timeline` 纯数据）：
- **相机轨道** `CameraTrack`：`(time, eye, target)` 关键帧（≥ 2），
  眼点与注视点**各过一条 Catmull-Rom spline** 的「锁定注视」相机路径。
  `ease = true` 走标准 Catmull-Rom（张力固定 0.5，过全部关键帧、段间
  C1 连续，首尾端点重复端点夹持 = 零切向缓入缓出不外推；`t = i` 恰过
  第 i 个控制点——u=0/1 处多项式 f32 求值不精确对消，端点做精确早退）；
  `ease = false` 线性。时间→段映射按关键帧 time 非均匀分段、段内归一化
  参数 u 求值（C1 在 spline 参数空间成立——不按弦长/向心重参数化是 v1
  简化，关键帧间距由人工调校）；`sample_camera` 的 t 越界钳制到
  `[0, 末关键帧 time]`；单关键帧/空轨道采样恒定不 panic（坏数据不报废演出）。
- **事件轨道** `TimelineEvent { time, name }`：跨过即发，跨界检测复用
  动画事件轨道的公共数学——`iris-render::animation` 的
  **`collect_crossings`（R47 从 `collect_events` 中抽出为独立纯函数，
  动画与时间线共享的单一来源**：回绕 / multi-fire / 末端不重复语义必须
  逐位一致，分叉必然漂移；`collect_events` 退化为薄接线层，既有测试
  零改动全过）。命中事件经 `Events<TimelineEvent>` 产出：Update 产出 →
  同帧更晚系统与下一帧全程可读 → 消费方 `drain`（事件双代契约，
  §3.10）。恰在 t=0 的事件永不触发（命中区间 `(prev, new]` 左开右闭）。
- **对话轨道** `DialogueEntry { time, end_time, text }`：`[time, end_time)`
  活动窗口（左闭右开——同刻「结束」与「下一条开始」不闪帧）；多条目
  活动时**声明序后者胜**（确定性取舍）。
- **黑边条**（letterbox）：`TimelinePlayer::letterbox` + `letterbox_ratio`
  （单条高度占视口高比例，0.12 ≈ 2.35:1）。

**播放器形态 = Resource（每世界至多一个）**：v1 定位「剧情演出独占主
相机与对话 UI」，并发多条时间线需要多个相机/黑边写入方、语义必然互斥，
Component 形态要引入「谁说了算」仲裁——v1 明确不支持并发演出，选
Resource 让约定成为文档 + `start_timeline` 直接可用。字段：`timeline /
time / playing / looped / previous_time`（跨界检测基）/ `letterbox /
letterbox_ratio` + 私有快照（快照/恢复只经托管入口成对出现）。

**相机接管与恢复**：`start_timeline` 快照主相机原姿态——**优先
`ActiveCamera` 实体的局部 `Transform`（查询序第一个，与渲染器相机解析
一致）、回落 `Camera` 资源的 `eye/target`**，都没有则相机不动（事件/
对话照常播）；播放期间每帧以轨道采样值覆写（实体模式写
`Transform.position` + 注视旋转 `look_at_quat`——局部 -Z 指向 eye→target、
与 `view_from_global` 口径一致，前向与 +Y 平行时改以 +Z 为上参考正交化
不产 NaN；资源模式写 `eye/target`）；`stop_timeline` 或播完时快照
**逐位写回**（f32 值拷贝无舍入；快照实体已 despawn 静默跳过）。已有
演出在播时再次 start：先恢复旧快照、隐藏演出 UI，再快照**恢复后**姿态
（先还旧债再借新债）。播完 `playing = false`、timeline 保留可读/重播。

**对话与黑边的 UI 手段**（复用既有 UI 矩形管线，不新增渲染路径）：三类
**固定 `Name` 的实体显隐**——对话框 `iris/timeline_dialogue`（`UiRect +
UiText + UiColor + Visibility`，窗口内有条目则写文本 + 水平居中显示在
下黑边上方，否则隐藏）、黑边 `iris/timeline_letterbox_top` / `_bottom`
（全宽黑色矩形，播放中随视口重排）。缺席时**惰性生成**（独占系统可
spawn；按 `entity_named` 查找不缓存句柄，despawn 后自动重建）。
**位图字体仅 ASCII**（既有 `UiText` 语义），中文对白 v1 不渲染。

**编排**：`TimelinePlugin` 注册 `TimelinePlayer` 资源 + `TimelineEvent`
事件（游戏侧不注册即得）+ **Update 阶段独占系统 `update_timeline`**
（动画之后、物理之前）。独占（`&mut World`）而非并行的理由：① 对话/
黑边实体可能需要惰性 spawn（结构性修改）；② 演出独占语义 + 屏障让
本帧写入对其后系统立即可见。必须早于 PostUpdate 层级传播与渲染收集
（实体相机 `GlobalTransform` 当帧新鲜）；用户系统在 DefaultPlugins 之后
注册、因此在其后执行——**演出期间游戏侧不要在 Update 后段改主相机**
（相机跟随应暂停，后写的赢），**不自动拦截游戏输入**（是否忽略由游戏
侧决定）。

**测试**（timeline.rs 14 项）：catmull-rom 过点精确 / C1 数值连续 /
端点钳制 / 单点与两点退化；事件接线与排序（回绕 / multi-fire 全覆盖在
动画侧既有测试）；对话窗口左闭右开 / 声明序取舍；实体与资源两种相机的
接管恢复逐位一致（播完 + 中途 stop）；播放中实体相机位置 == 采样值
（±1e-5）且注视点投影回视图中心；无相机轨道两种相机逐位不动；黑边
随开关与视口尺寸；事件产出可 drain；同 timeline 两次播放逐位一致。
视觉验收 `examples/timeline_snapshot.rs` → `docs/timeline.png`（12 s
演出：广角→特写 spline、第 3 s `door_open` 事件消费后开门、第 5–9 s
对话、全程黑边，三时刻拼图）。

## 5. 内存安全策略总表

| 风险 | 传统 C++ 引擎 | Iris |
|------|---------------|------|
| 悬垂实体引用 | 读到复用后实体的脏数据 | 代数校验，返回 `None` |
| 组件重复可变借用 | 数据竞争 / UB | 尝试型 RwLock 运行期 panic（跨线程生效） |
| 迭代中修改场景 | 迭代器失效 / UB | API 层禁止（`&mut World` 互斥） |
| 并行系统读写竞争 | 数据竞争 / UB | 类型级访问集分批 + 冲突即 panic 兜底 |
| GPU 缓冲悬垂 | 手写生命周期，易错 | wgpu 引用计数 + Usage 校验 |
| 类型混淆的句柄 | 强转 UB | `TypeId` + `downcast` + 泛型封装 |
| 第三方音频解码 | FFI 绑定 libvorbis / stb_vorbis | 选 symphonia（crate 顶部 `#![forbid(unsafe_code)]`，编译器强制），iris-audio 自身零 unsafe（§4.14，R44） |
| 全库 | 手工内存管理 | **零 unsafe，全程借用检查** |

## 6. 跨平台策略

- **图形**：wgpu 运行时枚举适配器（Vulkan → DX12 → Metal → GL），按平台选最优后端；
  WebGPU 语义保证 API 层可移植，WGSL 免除着色器方言问题。
- **窗口 / 输入**：winit 抽象事件源；输入层只暴露「物理键码 + 状态机」，
  布局无关（WASD 在 AZERTY 上依然是相同物理键）。
- **线程模型**：主线程驱动事件循环与渲染（winit 平台约束）；ECS 全部状态
  `Send + Sync`，无冲突系统经 `std::thread::scope` 并行执行（§3.7），
  工作线程按帧创建销毁（批次粒度，无驻留线程池）。
- **wasm32（Web）**：全 workspace 以 `wasm32-unknown-unknown` 编译零警告，
  平台差异收敛为五类，cfg 分支集中在 `iris-core/src/instant.rs`、
  `iris-core/src/schedule.rs`、`iris-render/src/context.rs`、
  `iris-render/src/surface.rs`、`iris-audio/src/device.rs`
  （另平台层 `iris-platform/src/window.rs` 与
  门面 `iris-engine/src/app.rs` 各一处，均有平台原因注释）：
  - **时间源**：std 的 `Instant::now()` 在 wasm 运行时 panic（std 未实现
    时间源，实测探针确认）。`instant.rs` 导出平台封装——native 直接
    `pub use std::time::Instant`；wasm 用 `performance.now()`（毫秒 f64）
    手写最小实现（构造 / `elapsed` / `duration_since` / 与 `Duration`
    加减），web-sys 依赖经 target 专属 dependencies 引入（已在 wgpu/winit
    依赖树内，无新增编译单元）。
  - **线程**：wasm 无 std 线程（`thread::spawn` 必 panic；实测
    `available_parallelism` 返回 `Err`）。`run_parallel` 在可用线程数 ≤ 1
    时内联顺序执行、不进 scope 不 spawn——native 单核同样受益，语义等价。
  - **GPU 初始化**：WebGPU 后端的适配器 / 设备请求是 JS Promise，无法阻塞
    等待（pollster 在 wasm 是忙自旋：`thread::park` 立即返回，std 无线程可
    阻塞）。`Gpu::with_instance_async` / `SurfaceKit::with_window_async`
    为全平台统一异步核心；native 保留同名同步包装（pollster），wasm 由
    App 的事件循环逐帧手动 poll 初始化未来（noop Waker，Promise 在帧间由
    微任务队列 resolve）。离屏渲染器在 wasm 恒返回 `None`（`device.poll`
    阻塞语义与 `std::fs` 读回均不可用），快照示例 web 上优雅降级。
    浏览器 `requestAdapter()` 不依赖 compatible_surface（wgpu WebGPU 后端
    不转发该参数），但呈现必须有画布：winit 窗口经 raw-window-handle 的
    `WebCanvas` 变体直达 `HtmlCanvasElement`，同一 `create_surface(window)`
    调用两端通用；winit web 创建的 canvas 默认不进 DOM，`WindowConfig`
    已显式 `with_append(true)`。
  - **日志**：`env_logger` 在 wasm 编译通过但写往不可见的 stderr（无输出），
    v1 不接 console log。
  - **音频**：cpal 的 Web 后端在 wasm32 上不可编译，cpal 依赖为 native 专属
    （target 专属 dependencies），设备层 cfg 收敛在 `iris-audio/src/device.rs`；
    wasm 下 `AudioOutput::device()` 恒返回 `None`，`Audio` 资源自动降级为
    纯混音器（play / stop / set_volume 照常，只是不发声，见 §4.14）。
- **交叉编译**：安装对应 std target 即可（如 `aarch64-pc-windows-msvc`、
  `wasm32-unknown-unknown`）；平台条件编译分支集中且均有平台原因注释（见上）。

## 7. 性能策略

| 层 | 手段 |
|----|------|
| 数据 | 稀疏集 dense 数组连续遍历；组件按类型分储避免缓存行浪费 |
| 调度 | 声明式读写集冲突分批，无冲突系统多线程并行（§3.7） |
| 数学 | glam：SSE/NEON 自动向量化（release 构建生效） |
| GPU | 自动实例化合批；单实例 112B 紧凑布局；实例缓冲跨帧持久——与上帧镜像比对后仅传变化段，静态场景零上传（变化 >50% 或缓冲重建 / 实例数变化时回退全量，§4.2）；纹理 BC1 GPU 压缩（8 倍显存节省，§4.18）；网格距离 LOD（远景 64x 三角形削减实测，§4.19） |
| 提交 | wgpu 命令编码 → 队列异步提交，CPU/GPU 并行 |
| 内存 | 无 GC；实体索引复用；scratch 缓冲跨帧复用（批处理排序数组）；纹理/网格内存占用统计口径（`memory_bytes` / `gpu_bytes_estimate`，§4.18） |
| 构建 | `lto = "thin"`、`codegen-units = 1`、`strip = "debuginfo"` |

### 7.1 性能基线与预算门禁（R33）

**方法论**：CPU 纯函数热路径以 criterion 正式基准守护（`cargo bench
--workspace`；`iris-core/benches/core.rs`、`iris-render/benches/render_cpu.rs`、
`iris-audio/benches/audio.rs`），全部 GPU 无关（不构造设备即可测）。
基准只测量、不断言——回归检测靠 criterion 的逐次基线对比报告。

**基线数字表**（测量机器：Windows 11 笔记本，i7-13620H + RTX 4060，
release 构建，2026-09-03；**仅供量级参考**——CI 虚拟机数字不可与本地
横向比较，只作 CI 自身纵向参考）：

| 基准 | mean | 含义 |
|------|------|------|
| `ecs_spawn_10k` | 2.31 ms | 1 万实体生成（2 组件/实体） |
| `ecs_query_iterate_10k` | 60.1 µs | 1 万实体双组件遍历 + 写回（帧循环最热路径） |
| `schedule_build_batches` | 843 ns | 30 系统读写集冲突分批的纯构建（每帧调度前固定成本） |
| `parallel_run_8_systems` | 3.47 ms | 8 个无冲突并行系统 × 1 万实体（含每批线程创建） |
| `physics_step_100` | 4.56 ms | 100 体（60 动 + 40 静）持续接触一帧物理 |
| `move_and_slide` | 1.59 µs | 角色控制器一帧（平地 + 撞墙滑行 + 贴地） |
| `broadphase_200_aabb` | 168 µs | 200 AABB 建网格 + 全量候选查询 |
| `determinism/trace_240` | 2.15 s | 确定性模拟 240 步 × 2 世界 + 逐采样记录（确定性测试开销上界；样本数 10，见 CHANGELOG R33） |
| `frustum_extract_and_test` | 5.53 µs | 视锥平面提取 + 1000 次球 / AABB 混合测试 |
| `bc1_encode_256` | 866 µs | 256² 纹理 BC1 编码（上传期一次性成本） |
| `instance_diff_10k` | 99.6 µs | 1 万实例 10% 变化的增量上传分段 |
| `collect_instances_5k` | 303 µs | 真实 World + MeshStore 下 5k 立方体收集 + 剔除（最接近帧预算） |
| `mix_32_voices_1s` | 92.6 µs | 32 声道（8 空间）44100 Hz 单次 1024 帧块（回调粒度） |
| `spatial_gain_10k` | 190 µs | 空间增益纯函数 1 万次 |

**预算测试（`crates/*/tests/budgets.rs`，CI `test-release` job 生效）**
是「防突然 10 倍劣化」的**烟雾报警**，不是精确基准：预算 = 上表 mean
× 约 5（写死数值 + 测量机器与日期注释），断言 N 次实测的中位数不超
预算。5 倍裕度吸收 CI 与本地机器差距与噪声——目标是误报率接近零，
代价是只能抓住数量级劣化；精确回归检测靠 criterion 基线对比，两者
互补。debug 构建自动跳过（数值无参照意义）。四个代表：ECS 遍历
（300 µs）、物理步进（25 ms）、帧收集（1.5 ms）、混音块（500 µs）。

### 7.2 构建与门禁约定（2026-09 工程化）

**门禁命令序**（子智能体收尾与独立验收共用；顺序有讲究）：

```sh
cargo fmt --all --check
cargo build --release --workspace --all-targets   # 一次编译全部产物（含 examples/benches/tests）
cargo test  --workspace           --quiet          # debug 测试（编译全命中缓存，纯运行）
cargo test  --workspace --release --quiet          # release 测试 + 预算断言（同上）
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --target wasm32-unknown-unknown -- -D warnings
```

要点：`build --all-targets` 必须在两个 `test` **之前**——`cargo test`
不构建 examples/benches（20+ 个），分开跑会让 release 编译两轮；先
build 后 test 则 test 的全部编译单元命中缓存，只剩纯运行时间。clippy
放最后（自有 check-profile 缓存，与前后互不干扰）。

**example 命名约定（打磨 VI 摩擦⑦）**：example 名必须**跨 crate 唯一**，
游戏 crate 一律带游戏前缀（`gen_assault_assets` / `assault_snapshot`，
不是 `gen_asset` / `snapshot`）。cargo 把全部 example 编到同一个
`target/.../examples/` 目录，同名即互相覆盖产物、并在构建期报
"will become a hard error" 警告；许多 example 还会往仓库根写 BMP/PNG
验收图，同名时更是互相抹掉对方的验收结果。引擎侧无修法（改 cargo 的
产物布局不归我们管），故记为约定。

**sccache 编译缓存**（`.cargo/config.toml` 的 `rustc-wrapper`，仓库级
自动生效）：新机器先 `cargo install sccache`（否则一切 cargo 命令失败，
见 config 注释）。诚实预期：**workspace 内部源码变更的重编无法避免**
（源码变 → 哈希变 → 必重编），sccache 的收益在依赖树——`cargo clean`、
target 目录丢失、profile 切换时依赖工件直接命中缓存；构建脚本与
proc-macro 本身不可缓存。观察统计：`sccache --show-stats`。

### 7.3 资产管线：pack 打包 / mount 挂载 / 异步加载（R45）

中型游戏资产体量的载入形态（「低占用」卖点的规模化兑现）：**PACK1 单文件
格式**（文件数爆炸 → 一个归档 + 随机读）、**AssetMount 多源挂载**（文件
系统 / pack / 嵌入统一读取）、**异步加载骨架**（主线程零阻塞轮询）。
实现全部在 iris-engine（`pack.rs` / `vfs.rs` / `async_loading.rs`），零渲染
依赖、零 unsafe、纯 std（无第三方依赖——确定性优先于压缩率）。

**PACK1 格式**（全部小端；条目表按路径字典序，数据区同序连续——同输入
两次打包逐位同输出）：

```text
┌────────┬──────────┬───────────┬──────────────────────┬─────────────┐
│ "IPAK" │ 版本 u32 │ 条目数 u32 │ 条目表 × N           │ 数据区       │
│ 4 B    │ = 1      │            │ 路径长u32+路径+偏移u64│ 连续无填充   │
└────────┴──────────┴───────────┴─+长度u64+校验和u64─────┴─────────────┘
```

校验和选型＝**CRC-32（IEEE 802.3）以 u64 字段存储**：定位是防损坏而非
防篡改，纯 Rust 表驱动十余行可审计（const fn 编译期建表，运行期零初始化
成本），XXH3 需引入实现/依赖，不划算；u64 字段宽度 + 版本号是格式承诺，
将来升级强校验只动算法不动布局。**大文件语义是结构性保证**：`PackReader`
是泛型源（默认 `File`），open 只做头 + 条目表两次有界读取，`read` 对目标
条目 `seek + read_exact` 后验 CRC——计数源测试锁定「open 读 < 1 KB、
读 1 KB 条目全程 < 1 KB」（12 MB 邻居条目全程不进内存）。

**mount 顺序语义**（`vfs.rs`）：`AssetMount::read` 按挂载顺序逐源尝试——
源内未命中 → 下一源；命中 → 立即返回（先挂先命中，同路径多源 = 用挂载
顺序做补丁覆盖）；命中但读取失败（CRC 不符 / IO 错误）→ **立即报错不
回退**（损坏数据被静默跳过是 bug 温床）。`AssetMount` 是可选资源：默认
不注册，缺席 = 既有 `asset_path` + `std::fs` 语义零变化；消费方是
`load_scene_from`（场景 JSON 与 glTF / 贴图 / HDR 子资产全走挂载，
`load_scene` 文件路径语义保持逐位不变）。wasm 姿态＝**运行时降级**：
`Fs` 源在 wasm 上读取恒失败（`std::fs` 为桩），枚举形状跨平台一致、
调用方零 cfg 分叉，wasm 布局只挂 `Embedded`（编译期文本表，场景 JSON
刚需；二进制嵌入与 fetch 源在路线图）。

**异步加载骨架**（`async_loading.rs`）：`AssetLoader::spawn` 起专用线程
`iris-asset-loader`（命令通道入队 + 结果信箱出队，照 iris-audio 流式
解码线程模式），`request` 只入队、`poll` 非阻塞 `try_recv`——主循环每帧
轮询是既定用法。与音频流「后台错误不可回传」相反，这里错误**保真回传**
（加载无实时回调约束）；`AssetKind::Scene` 在加载线程上做 JSON 结构校验
（坏文件在后台报错，主线程拿到的字节保证可解析）。wasm 无线程 →
`spawn` 自动退化为 `inline` 同步后端（同 API 同语义，唯一契约差异是
读取耗时计入 request 调用）。

**实测数字**（Windows 11 笔记本 i7-13620H，release，2026-09-06，
`cargo run -p iris-engine --release --example pack_demo`；量级参考）：

| 项 | 数字 | 含义 |
|----|------|------|
| 打包开销 | 34 639 B → 34 785 B（+0.4%） | 三资产（场景 JSON + GLB + OGG），头 + 条目表共 146 B，无填充无压缩 |
| pack 随机读 | ≈ 25 µs / 次 | `seek + read_exact` + CRC 校验（文件句柄常驻） |
| 直读对比 | ≈ 47 µs / 次 | `std::fs::read`（每次重开文件——open 系统调用占大头） |

pack 读反而快约 2 倍：单文件归档天然免 open/Close 每次开销；这正是
「海量小资产」场景下 pack 的结构性收益（大资产收益主要在文件数与
分布 locality，速度差随资产变小而放大）。

## 8. 已知限制（诚实清单）

1. **并行调度为批次粒度**：无驻留线程池，每帧按批创建线程（批内系统少时
   开销可忽略；高频小批次场景可引入线程池）；分批为类型级保守分析，
   无法表达「分片读同一组件」的数据并行（如按实体区间拆分遍历）。
2. **层级传播为递归实现**：层级深度受栈深限制（游戏场景足够）；每次传播为
   全量重建，海量静态实体场景可考虑脏标记增量传播。
3. **批处理粒度**：不透明按 (网格, 材质) 合批，无深度排序（不透明物体绘制
   顺序无关紧要）；半透明已按视深排序独立 pass 处理（§4.16），但排序粒度
   是包围球中心而非逐像素——凹体 / 交叠体积的自遮挡排序误差在路线图
   （逐像素需 OIT 方案）；glTF `"MASK"` alpha 裁剪按不透明处理（§4.6）。
4. **无热重载**：CPU 目录句柄不可变，资产修改需重建；场景文件为手工编写，
   热重载未做（`load_scene` 加载完成后与文件的关联即断开，修改后需重新加载）。
5. **Web 目标（wasm32）**：全 workspace 编译零警告，Instant / 线程 / 异步
   GPU 初始化平台封装就位；**端到端验证通过**——无头 Chrome（152）冒烟
   「winit 窗口创建 + canvas 附加 DOM + 适配器请求发出 + 无适配器时优雅
   降级不卡死」，随后在带 WebGPU 的真实浏览器中完整渲染确认
   （点光场景 + HUD，`docs/web_preview.png`）。WebGL2 回退未启用
   （需非默认 `webgl` feature）；离屏渲染器在 web 不可用（恒 `None`）；
   `env_logger` 无输出。
6. **点光阴影为 v1 限定**：只有查询顺序中的首盏 `shadows` 点光投影
   （cubemap 每灯 6 个 pass，多灯投影在路线图）；单采样无 PCF，阴影边缘
   为硬边；面矩阵约定（OpenGL 层序 + NDC y 翻转）依赖 GPU Cube 采样的
   方向语义——更换图形栈或 wgpu 大版本时需重新验证
   （集成测试 `face_orientation_matches_cube_sampling` 守护）。
7. **外部 HDR 环境源仅覆盖 Radiance .hdr 的 `-Y +X` equirectangular 形式**：
   其他分辨率朝向（±X ±Z 组合、垂直镜像）解析为 Unsupported；不读 `.exr` /
   `.ktx` 等其他 HDR 容器；超大图受设备纹理尺寸上限约束（Rgba16Float 中间
   纹理按原图尺寸上传）。程序化天空仍是缺席 `SkySource` 时的默认（零资产
   依赖）；烘焙在主线程同步执行，换入大贴图的当帧有一次性停顿。
8. **阴影偏移为常数 bias**：无坡度缩放 / 法线偏移（normal offset），大入射角
   掠射表面可能有轻微 acne；PCF 9 采样在实际场景中已足够干净。
9. **动画**：骨骼蒙皮已覆盖刚性关节 LBS（GPU 蒙皮 + 蒙皮阴影）；形变目标、
   CUBICSPLAT 插值与关节非均匀缩放的精确法线变换在路线图中。
10. **2D UI 为 v1**：无层级、无锚点、无剪裁（元素完全靠屏幕像素坐标手工
    排布，高 DPI / 窗口缩放不做自动适配）；字体为等宽位图且仅 ASCII
    （非 ASCII 字符跳过并告警一次），`\n` 之外无自动换行 / 自动布局；
    `UiText` 复用同实体 `UiRect.position` 定位且该矩形不绘制；UI 顶点
    每帧 CPU 重组上传（无 GPU 持久化 / 增量更新）；无输入命中测试。
11. **音频为 v1**：空间音频为 v1 距离衰减 + 恒功率立体声声像（§4.14）——
    无 HRTF / 耳间延迟差（**源在正后方与正前方对称、前后不可区分**）、
    无多普勒 / 遮挡 / 混响；立体声剪辑不做逐声道声像（只乘总增益）；
    无流式播放（音频全量解码为 PCM 常驻内存，长音频 / 音乐播放成本高）；
    每 App 一个混合器（无总线 / 子混音分组 / 每声音滤波）；解码仅支持
    16-bit PCM WAV（无 mp3 / ogg / flac）；重采样为线性插值（无抗混叠
    滤波，比率悬殊时会引入混叠）；软限幅为硬钳制（叠加爆表段有可闻削
    底）；cpal `Stream` 非 `Send`，设备会话托管在专属后台线程（§4.14）。
12. **dogfooding 发现的引擎缺口（iris-game 实践反馈）**——以下均为用引擎拼
    一个完整游戏时实际撞到的摩擦与绕行方式。（更新：**七项已全部修复于
    「引擎 API 打磨」轮**，iris-game 的绕行代码已同步删除并改用新 API 作
    闭合证明；条目按原文保留作为 dogfooding 记录。）
    - **无实体可见性开关**：隐藏实体的唯一办法是移出视野（游戏把已收集
      金币每帧钉在 y = -100）或销毁重建（丢失句柄语义）。建议 `Visibility`
      组件在批处理阶段过滤。
      → **已修复**：`iris_render::Visibility` 组件（`VISIBLE` / `HIDDEN`
      常量）+ 渲染收集查询带 `Option<&Visibility>`，主 pass / 方向光阴影 /
      点光阴影 / UI 四条收集路径统一过滤；组件缺席 = 可见（默认行为零变化）。
      游戏侧：已收集金币改切 `Visibility::HIDDEN`（钉 y = -100 的绕行已删）。
    - **无系统排序 API**：同阶段系统按注册顺序分批，DefaultPlugins 之后的
      插件无法把自己的系统排到层级传播**之前**（也无 before/after 约束）——
      iris-game 在系统链末尾补一次幂等的 `propagate_transforms` 绕行（同帧
      双次传播，约 20 实体代价可忽略）。且并行系统的先后顺序完全由声明的
      读写集冲突「涌现」（逻辑系统写 `GameState` / HUD 系统读之，恰好分批
      保序）：声明漂移即静默破坏顺序（运行期借用检查兜底 panic，不会数据
      竞争）。建议显式排序约束或增加 PostUpdate 阶段。
      → **已修复**：`Stage::PostUpdate` 阶段（每帧一次、不在 FixedUpdate
      循环内），编排 Update 写局部变换 → PostUpdate 传播 → 渲染读取；
      `HierarchyPlugin` 的 `propagate_transforms` 迁址 PostUpdate，用户系统
      当帧写入当帧传播，插件不再关心与传播的注册顺序（游戏侧幂等补传播
      的绕行已删；显式 before/after 排序约束仍不在路线图内）。
    - **并行系统无法发送事件**：`send_event` 需要 `&mut World`（独占系统
      专属），并行系统（`&World`）只能旁路——拾取音效改为在系统内直接调用
      `Audio::play` 而非「拾取事件」。建议资源形态的事件写入器。
      → **已修复**：`World::try_send_event<E>(&self, event) -> bool`——写
      路径本就是 `&self` + 写锁，已注册类型即可从并行系统发送；未注册返回
      `false` 并告警（不自动注册：注册是结构性修改需 `&mut`，`send_event`
      保留自动注册语义）。游戏侧保留直接调用 `Audio::play`（更简单直白），
      注释改指新 API 作为解耦选项。
    - **无按名查实体**：跨系统定位实体（HUD 刷新、金币动画）只能自建
      「实体句柄索引」资源（`GameEntities`）。建议 `Name` 组件 + 查询辅助。
      → **已修复**：`iris_core::Name` 组件 + `World::entity_named(&str)
      -> Option<Entity>`（线性扫描，O(n) 适用低频查找，同名取首个）。
      游戏侧：HUD 文本实体改挂 `Name` 并按名定位，`GameEntities` 精简为
      玩家 / 相机 / 金币三个每帧热路径句柄。
    - **Input 注入侧类型未重导出**：查询侧 `KeyCode` 在 prelude，脚本化
      注入所需的 `PhysicalKey` / `ElementState` 不在——下游 headless 测试
      被迫直接依赖 winit。且「按住」状态跨帧保持、换向必须显式 `Released`
      （快照脚本忘发释放事件导致两键叠加相消，实测踩坑）。建议补重导出与
      `press / release` 便捷方法。
      → **已修复**：prelude 重导出 `PhysicalKey` / `ElementState`（查询侧
      `KeyCode` / `MouseButton` 原有），`Input::press(impl Into<PhysicalKey>)`
      / `Input::release(...)` 包装 `on_key`（`KeyCode` 经 winit 的
      `From` 直通，无须手构事件类型）。游戏侧：headless 测试与
      `game_snapshot` 示例已删除 winit 直接依赖（dev-dependency 移除）。
    - **Audio 无法强制纯混音器模式**：`Audio::new()` 优先建设备且字段私有，
      有声卡的开发机上 `cargo test` 会短暂打开真实设备（无声卡 CI 自动
      降级）。建议 `Audio::headless()` 构造器。
      → **已修复**：`Audio::headless()`——绝不打开设备（纯混音器），
      `play / stop / set_volume / mixer` 语义与有设备时一致；资源兜底逻辑
      遇已存在的 `Audio` 不覆盖，headless 测试预插入即全程零设备访问。
    - **小项**：`Time` 的确定性推进需手工构造单调 `Instant`（`advance`
      便捷方法会更直白，首帧 delta = 0 也需测试知晓）；`UiText` 无文本
      测量（右对齐 HUD 需手算 8×scale×字符数）；图元库无圆柱 / 胶囊
      （金币以压扁球体替代）。
      → **已修复**：`Time::advance(Duration)`（delta = min(dt, max_delta)，
      elapsed / ticks / accumulator 同步，首帧即有效 delta，时钟基准归位
      不与 `begin_frame` 二次差分）；`iris_render::ui::text_width(&UiText)
      -> f32`（8 × scale × 最长行字符数，与渲染步进同源，游戏侧 TIME
      右对齐与横幅居中改用）；`primitives::cylinder(radius, height,
      segments)`（侧面 quad 环 + 上下三角扇圆帽，外侧 CCW 环绕与既有
      图元一致）。胶囊仍不在（金币仍为压扁球体，视觉等价）。
    - **无墙体碰撞的角色控制器**（场景 + 物理接入轮）：竞技场边界最初
      只有逻辑层的 ±18 坐标钳制，玩家与围墙 / 障碍柱可视觉穿插——坐标
      钳制表达不了「被什么挡住」的语义。建议提供不写组件的碰撞查询。
      → **已实现**：物理库的静态推离查询（`resolve_static_push` /
      `resolve_against_statics`，§3.8）——游戏侧以运动学角色控制器模式
      消费：关卡迁入 JSON 场景文件（墙体 / 障碍柱带静态 Collider，经
      `load_scene` 载入），`logic::step` 保留移动积分与钳制安全网，控制器
      对场景静态体求推离、只回馈水平分量（贴墙滑行）；玩家被墙挡在
      17.05 = 内缘 17.5 − 半径 0.45（headless 测试断言 x 稳定在 17.05 且
      小于钳制上限 18——「是 Collider 而非 clamp 在挡」的直接证据）。
13. **dogfooding II 发现的 API 摩擦（iris-arena 实践反馈）**——第二轮以
    「波次生存竞技场」压榨蒙皮动画 × 对象池 × 弹道 × 多游戏状态时撞到的
    摩擦。本轮原则：**引擎侧零改动**，全部记录 + 游戏内绕行。
    （更新：**七条已全部修复于「引擎 API 打磨 II」轮**，iris-arena 的
    绕行代码已同步删除并改用新 API 作闭合证明——headless 测试对 Default
    资产路径零注入直接通过；条目按原文保留作为 dogfooding 记录。）
    - **`AnimationPlugin` 未列入 `iris-engine` prelude**：`HierarchyPlugin`
      等同批插件都在 prelude，唯独动画插件缺席——headless 测试 / 离屏
      快照无法 `AnimationPlugin.build(&mut app)`，只能绕行：按其实现原样
      `schedule.add_parallel_system(..., update_animations)`（同一系统同一
      访问集）。建议补一行 prelude 重导出。
      → 游戏侧绕行：iris-arena 的 headless 测试与 `arena_snapshot` 内联
      注册；文档注明「本插件刻意不自带动画注册，避免窗口路径重复注册
      双倍速」。
      → **已修复**：`iris-engine` prelude 导出 `AnimationPlugin`（与
      `HierarchyPlugin` 等同批插件一致）。游戏侧：headless 测试与
      `arena_snapshot` 删除内联注册，改为 `AnimationPlugin.build(&mut
      app)`——headless 路径与窗口路径的编排来源完全同一。
    - **资产路径是工作目录相对，`cargo test` 下失效**：`cargo run` 的 cwd
      是 workspace 根（窗口程序开箱即用），但 `cargo test` 的 cwd 是
      **crate 目录**——插件内写死的 `assets/enemy_worm.glb` 在测试里加载
      失败且错误被 `log::error` 吞掉（测试默认无 logger），池静默缩容为
      0，只有断言「池应有 15 槽位」才暴露。建议：引擎提供「资产根」约定
      （如 `CARGO_MANIFEST_DIR` 兜底）或 `Result` 外露加载错误。
      → 游戏侧绕行：敌人模型路径提升为 `ArenaPlugin.enemy_model_path`
      字段（可注入），测试用 `env!("CARGO_MANIFEST_DIR")` 拼绝对路径。
      → **已修复**：`iris_engine::asset_path(relative) -> PathBuf`——
      按序尝试 CWD 相对（存在）→ 工作区根相对（编译期 `CARGO_MANIFEST_DIR`
      上溯两级，存在）→ 都不存在返回 CWD 相对（让后续 load 报出调用方
      熟悉的相对路径错误，解析层不吞错）。`GamePlugin` / `ArenaPlugin`
      的 Default 场景与敌模路径经它解析，字段注入能力保留（测试 fixture
      仍走显式路径）；游戏侧 `CARGO_MANIFEST_DIR` 注入删除。
    - **`spawn_gltf_instance` 的蒙皮实例关节树不随 mesh 根移动**：多顶层
      节点的蒙皮 glTF（mesh 根与关节树根是**兄弟**）生成的实体树无法整体
      移动——蒙皮矩阵 `J = inv(mesh 全局) × global(joint) × ibm`，关节树
      留在世界原点时移动 mesh 根，蠕虫永远渲染在原点绑定姿势（静态展示
      的 skin_arm 不会暴露，一动就散架）。建议：单根蒙皮场景约定或加载时
      自动收编兄弟顶层节点。
      → 游戏侧绕行：spawn 后 `world.add_child(root, 关节树根)` 把兄弟
      顶层节点收编为 mesh 根的子树（公开 API 层级操作即可表达）。
      → **已修复**：`spawn_gltf_instance` 加载完成时自动收编——对每个
      skin 求关节树根（joints 集合中不被集合内任何其他节点的 child 边
      指向者），`add_child(蒙皮节点, 关节树根)`；关节树已是蒙皮节点
      后代的（单顶层节点资产）保持原层级，不拆动中间非关节节点的变换链。
      正确性论证：蒙皮公式 `verts = mesh全局 × (inv(mesh全局) ×
      joint全局 × ibm) × v = joint全局 × ibm × v`——mesh全局相消，几何体
      **跟随关节**；收编后 joint全局含蒙皮节点变换 ⇒ 移动实例根即移动
      整只蒙皮角色（收编时 glTF 蒙皮资产的 mesh 节点变换为恒等，不改变
      初始全局变换）。游戏侧绕行删除；守护：glTF 单测（链式 / 多根
      关节树根识别、收编后父子层级、收编后 propagate 关节全局含实例
      位移）+ GPU 回归测试 `skinned_instance_follows_root_translation`
      （实例定位 (1.5, 0, 0) 渲染：断言全部角色像素整体右移越过位移前
      最右端、位移前的屏幕位置零红像素、像素总量同数量级——收编失效时
      关节矩阵会把几何体拉向反方向）。headless 侧补「逻辑移动敌人 →
      槽位根移动 → 关节子树全局跟随」端到端断言。
    - **`Input` 无按键枚举 / any-key 查询**：标题画面的「任意键开始」只能
      白名单探测常用键的 `key_pressed`（漏掉按键 = 玩家卡在标题屏）。
      建议提供 `Input::any_pressed()` 或按帧记录的按键迭代器。
      → 游戏侧绕行：8 键白名单（WASD / Space / Enter / Shift / R）。
      → **已修复**：`Input::any_key_pressed() -> bool`——本帧任意键刚
      按下（与 `key_pressed` 同款边沿语义，系统自动重复不重触发）。
      游戏侧：8 键白名单删除，标题画面改用 any-key 查询。
    - **程序化音效无法拼接 / 变调**：`sine / square / noise` 只生成单段
      常频剪辑，`AudioClip` 采样不公开读取——「开波双音」只能绕开生成器，
      用公开的 `AudioClip::from_interleaved` 手写采样拼接（含首尾 5 ms
      包络防爆音）。建议公开采样读取或提供 `concat / mix` 组合子。
      → **已修复**：`AudioClip::concat(&[&AudioClip]) -> Result<
      AudioClip, AudioError>`——声道数 / 采样率一致才可拼（不一致报
      `Unsupported`，静默重采样会隐藏调用方参数错误），顺序串联；
      采样只读视图 `samples()` 原有。游戏侧：开波双音改为
      `sine(660) + 静音段 + sine(990)` 三段 `concat`（各段包络由生成器
      自带），手写采样缓冲删除。
    - **差分事件在状态复位时会下溢**（游戏侧自伤，非引擎缺口，记录为
      陷阱）：逻辑侧单调计数器（total_shots 等）在 restart 时归零，插件
      的 `after − before` 差分在 debug 构建 overflow panic。修法用
      `saturating_sub`——「资源承载可复位状态 + 差分触发表现」的组合
      应默认防下溢。
      → **改写为模式警示**（无引擎代码改动）：这是游戏代码模式问题而非
      引擎缺陷——引擎资源可以承载**可复位**的游戏状态，引擎无从（也
      不应）阻止 restart 把计数器归零。凡「资源承载可复位状态 + 帧间
      差分触发表现」的组合，差分处必须用 `saturating_sub`（或改用有符号
      差值），把「计数器被复位」当作正常输入而非算术异常；iris-arena
      已按此实现（重开帧静音，不补播旧对局的事件）。
    - **蒙皮资产制作陷阱**（gen_asset 侧，记录给后续资产管线）：图元立方体
      跨度是 ±0.5（缩放系数 = 全边长）；关节节点平移是**父局部**偏移而
      ibm 是**绑定姿势全局**的逆——两者混用会让末端关节错位一倍。蒙皮
      数值正确性目前只能靠离屏渲染目检守护，CPU 侧 `skin_vertex` 参考实现
      可单测但未覆盖「GLB 文件 → 关节矩阵」全链路。
      → **陷阱原文保留**（资产管线守则）；「GLB 文件 → 关节矩阵」全链路
      的自动化守护已由 GPU 回归测试补上（`skinned_instance_follows_root_
      translation` 的像素级断言，见上上条）。
14. **粒子系统为 v1**（§4.17）：场景序列化留白（JSON 场景无粒子段，
    发射器只能代码生成）；渲染实例每帧全量上传（粒子逐帧全变，无增量
    价值）；无碰撞 / 光照交互 / 软粒子深度淡出（贴地粒子可能穿插硬边）；
    剔除按发射器包围球保守整批裁决（单个巨池发射器跨视锥边界时会整批
    保留）；alpha 粒子排序粒度为逐粒子中心视深（与透明几何之间的先后
    由 pass 顺序裁决，粒子恒在透明几何结果之上）。（「burst 一次性、
    无重置 API」的限制已由打磨 III 闭合：`reset()` / `trigger_burst()`
    就地复用发射器，重复爆炸无需重建组件。）
15. **dogfooding III 发现的引擎摩擦（iris-bastion 实践反馈）**——第三轮
    以塔防压榨 raycast × 透明 × 粒子 × 空间音频时撞到的摩擦（原则同前
    两轮：引擎侧零改动、记录 + 游戏内绕行；摩擦原文见 CHANGELOG R26
    条目）。（更新：**五条已全部修复于「引擎 API 打磨 III」轮**，
    iris-bastion 的绕行代码已同步删除并改用新 API 作闭合证明。）
    - 无「屏幕 → 射线」辅助且实际窗口尺寸不作为资源暴露（插件手工拼
      vp 逆 + `BastionViewport` 缺省 1280×720 绕行）
      → **已修复**：`Camera::screen_ray` / `screen_ray_from_global` /
      `view_proj_from_global`（§4.5）+ `Viewport` 资源（`WindowPlugin`
      首插、`Resized` 自动同步）。游戏侧：`BastionViewport` /
      `active_camera_vp` / `BastionPickError` 删除，拾取改走引擎 API。
    - `ParticleEmitter` burst 一次性无重置 API（FX 池「覆写整只新组件」
      绕行）
      → **已修复**：`reset()` / `trigger_burst()`；游戏侧改「参数全字段
      覆写 + reset 就地复用」。
    - 粒子收集不查实体 `Visibility`（隐藏发射器的活粒子照画）
      → **已修复**：收集查询带 `Option<&Visibility>`，隐藏发射器整池
      跳过（GPU 测试守护，§4.17）。
    - `Visibility` 不向子树级联且组件缺席 = 可见——池槽位「无网格 rig
      夹层 + 孙子网格」结构下孙子恒渲染（待命池实体堆在原点入画，
      draw calls 139 事故），游戏被迫拍平层级 + 名字识别形态绕行
      → **已修复**：渲染收集期沿 `ChildOf` 父链上行的层级级联（§4.4），
      GPU 回归测试复刻三层结构；游戏侧还原「根 → 形态 rig → 网格」池
      结构，形态切换回到「翻两个 rig」。级联缺失期间「过滤逐实体」的
      旧语义只存在于文档，代码语义由本条回归测试锁定。**快照实证**：
      拍平结构下击杀归还的槽位只隐藏根、网格自带 `VISIBLE` 滞留死亡点
      入画（bastion_snapshot 定格帧 `draw_calls = 18`——残块恰藏于 HUD
      文字后，新旧帧逐像素差分定位）；级联闭合后 `draw_calls = 17`，
      与「旧结构 + 手工逐子隐藏」帧逐字节一致。
    - 同构池 type alias 共享 TypeId 互相覆盖（游戏侧设计陷阱，引擎无从
      阻止）
      → **改写为模式警示**（无引擎行为改动）：`World::insert_resource`
      文档 + §2 iris-bastion 段记录——同型多实例资源必须 distinct
      newtype。

16. **距离 LOD 为 v1**（§4.19）：距离用实体原点（非包围球中心，偏心
    网格的切换边界略保守）；**无滞回**——实体在 `switch_distance` 边界
    抖动时两网格可能逐帧来回切换（需要时游戏侧做时间平滑，引擎内双
    阈值滞回在路线图）；只有两级（无连续 LOD 链）；切换即硬切（无
    geometry morph / 渐隐过渡）。
17. **BC1 压缩为 v1**（§4.18）：仅 BC1（无 BC3/BC7——带 alpha 的纹理
    一律回退 RGBA8，显存优势丢失）；上传路径只在源尺寸 4 对齐时启用
    （非对齐纹理回退 RGBA8，编码器 pad 能力未接入上传）；无 mipmap
    （远距离细节闪烁没有 mip 链缓解）；编码器为确定性暴力端点选择
    （质量敏感资产可用离线工具预压缩，运行时编码走同一管线）。
18. **dogfooding IV 发现的引擎摩擦（iris-ascent 实践反馈）**——第四轮
    （3D 平台跳跃）把 R28–R31 压榨一遍的「现象 → 绕行 → 建议修法」
    清单；均未改引擎（绕行在游戏侧，作为下一轮打磨的输入）。
    （更新：**六条已全部闭合于「引擎 API 打磨 IV」轮**，iris-ascent 的
    绕行代码已同步删除并改用新 API 作闭合证明——①②③④ 引擎侧修复，
    ⑤⑥ 按原建议以文档闭合。）
    ① **crossfade 的缺通道混合语义**：混合只作用于「双方都有」的
    「目标实体 + 路径」对，「仅源有」的通道在过渡期保持源姿势、过渡
    结束后**定格在最后的源值**（不再被任何剪辑驱动）——idle 只有
    scale、walk 只有 rotation 的自然写法会让呼吸缩放在起跑后冻住。
    **绕行**：状态机用的剪辑强制路径对齐（两剪辑都写 scale + rotation
    两通道，恒值通道也要写）。**建议**：blend 结束时把「仅源有」路径
    淡回目标实体的静止姿势，或文档显式警告「crossfade 剪辑必须路径
    对齐」→ **已修复**：`AnimationPlayer::rest` 基姿势快照 +
    `capture_rest(&World)`（实体生成后、动画推进前捕获一次），混合期
    「目标无此通道」的通道值 = `lerp/slerp(源值, rest 值, w)`——过渡
    完成时恰为 rest、blend 清除后该通道停在 rest（与「未被动画的字段
    保持当前值」自洽，完成瞬间无跳变）；rest 未捕获保持 v2 语义（既有
    测试零改动全过）。游戏侧：双剪辑恒值对齐通道删除，模型装配处
    `capture_rest`，回归测试断言 idle↔walk 过渡后 scale / rotation 回
    基姿势；GPU 端到端断言完成帧与播放动画前逐像素一致（§4.6.1）；② **程序化 GLB 生成是手写 JSON 字符串**：bufferView 偏移 /
    accessor 下标 / sampler 编号全靠手工记账（gen_asset 生成函数
    均如此），错一位即加载失败或姿势错乱且无定位信息。**绕行**：复制
    既有函数为模板改数。**建议**：引擎提供编程式组装 API
    （`GltfBuilder`：append_mesh / add_animation_channel / write_glb）→
    **已修复**：iris-engine 新模块 `gltf_builder`（material / mesh /
    node+add_child / skin+attach_skin / animation 声明式 API +
    build_glb / build_json_and_bin）——二进制记账、4 字节对齐、POSITION
    包围盒、索引宽度选型、顶层推断、times accessor 共享全部收进
    builder；gen_asset 五个生成函数全部迁移，重新生成的资产与旧文件
    加载语义等价（节点 TRS / 剪辑关键帧 / 蒙皮 ibm 逐位一致；唯一差异
    为节点旋转从缺省零四元数变为显式恒等，行为等价），加载侧 GPU 测试
    零改动全过；③ **状态机无边时 `request_state` 静默硬切换**：调试期清空 edges
    会让全部过渡变成跳变，画面闪变无任何日志线索。**建议**：硬切换
    路径打 `log::debug` → **已修复**：无边分支打 `log::debug!`（含
    from / to 状态与边表长度），文档写明「无边 = 合法的显式硬切换」；④ **「子实体相机 + 平滑追随」无现成范式**：
    平滑量必须写在子实体的**局部**位置（局部 = 平滑世界位 − 根位置，
    层级传播还原世界位）；GlobalTransform 滞后一帧；瞬移（复活）需要
    手动吸附，否则相机长距离甩镜。**绕行**：游戏侧约 30 行的
    `CameraRig` 资源（iris-ascent plugin）。**建议**：引擎提供
    `SmoothFollow` 组件或在下游游戏模板文档中固化该范式 →
    **已修复**：iris-engine 新模块 `camera_rig`——`SmoothFollow` 组件 +
    `update_smooth_follow` 并行系统（帧率无关 `1 − exp(−lerp·dt)` 指数
    趋近、局部 = `parent⁻¹ × 平滑世界位`、`snap()` 吸附、首帧吸附）；
    编排契约（目标写入系统之后注册、目标世界位沿父链局部 Transform
    现值合成而非读滞后一帧的 GlobalTransform、与层级传播的先后）写入
    模块文档。游戏侧：`CameraRig` 资源与积分代码删除，相机改挂
    `SmoothFollow`，复活帧 `snap()`；⑤ **`move_and_slide` 的衔接参数域无指导**：`step_height 0.3` +
    `snap_distance 0.2` 对「落差 ≤ 0.3、缝宽 ≤ 0.5（< 胶囊直径）」的
    台间衔接恰好可靠；缝宽 > 2×snap_distance 且落差 > step_height 的
    衔接会以「贴边落在台缘角点」的方式卡墙，且失败模式不易诊断。
    **绕行**：iris-ascent 以关卡不变量单测固化衔接域（落差 ≤ 0.3、
    缝宽 ≤ 0.5）。**建议**：character 模块文档附「平台跳跃衔接参数
    表」→ **已闭合（文档）**：`iris-core::character` 模块文档新增
    「平台跳跃衔接参数表」——低台阶 / 走落 / 水平缝 / 落点窗口 / 平台
    厚度的可靠域与推导，超域失败模式（台缘角点 45° 法线卡墙、双超域、
    薄板穿通）的症状 / 复现特征 / 诊断建议；⑥ **跳跃输入的电平 vs 边沿**：`key_pressed` 边沿对「落地就
    跳」（跳跃蓄压）不友好，脚本注入也要 release+press 两步。**绕行**：
    逻辑层输入直接用电平语义（`AscentInput::jump` 按住即跳，落地即
    触发）。**建议**：无（记录语义选择，下游游戏按玩法挑一边）→
    **已闭合（文档）**：`iris-platform` input 模块文档新增「输入语义
    选择指南」——电平 / 边沿的判定法则（动作是否应按住持续生效）、
    电平 + 玩法门控的组合范式、脚本注入差异。
19. **盒投影贴花为 v1**（§4.20）：无光照（输出 = 纹理 × tint，不参与
    PBR / 阴影 / IBL——半亮环境下贴花比表面「亮一档」，深色标记上不可
    察觉）；平面假设（端盖是投影平面，深盒贴斜面贴合偏差上界
    `size.z/2`，基于深度缓冲重投影的屏幕空间 decals 在路线图）；不进
    阴影 pass；同批重叠贴花无视深排序（实体 bits 并列序）；场景序列化
    留白（JSON 场景无贴花段）；实例每帧全量上传（无帧级增量比对）。
20. **多视口与渲染到纹理为 v1**（§4.21）：RT pass 数量上限 2（小地图 +
    监控屏两个典型用例，超出 warn 跳过）；**无 `RenderTargetStore::resize`**
    （RT 尺寸创建后固定，尺寸变化需重建 store 条目——监控屏分辨率一般
    固定，读回 / 重分配留待后续）；**RT 相机朝向不同时 CSM 仍按主相机
    拟合**（级联覆盖范围可能不匹配 RT 视锥，俯视 / 平视角下阴影精度
    退化）；**天空盒 / 贴花 / 粒子 v1 不画**（RT 视锥内这些元素缺席）；
    **无后处理链**（RT 直渲后仅 ACES（泛光 0 / 曝光 1 / 暗角 0）——
    无 bloom / 曝光 / 暗角）；RT 无读写回（v1 不做 RT 像素 map，UI 直接
    采样 GPU 纹理）；UI 图片无抗锯齿（Nearest 直角采样，像素化小地图
    天然契合）且绘制顺序固定（图片之下、文本之上）。
21. **UI 编排（锚定 / 焦点 / 菜单）为 v1**（§4.13）：**无父容器嵌套**——
    锚点只相对视口，`UiAnchor` 不溯源到祖先实体布局（面板 + 按钮的居中需
    各自挂同一锚点，无「子元素相对父面板」）；**不拉伸不缩放 `UiRect.size`**
    （锚点只平移元素，不做比例填充，DPI / 分辨率自适应留待后续）；焦点
    导航用**屏幕 Y 再 X 的序列化顺序**而非方向自由度导航（横向 / 纵向菜单
    混合时 ↑↓ 语义按全序走，方向键无法直接「上下左右」四向跳位）；焦点
    高亮是**改 UiColor 的单色标记**（无样式表 / 渐变 / 动效）；`UiClickEvent`
    只带 `entity`（无命中坐标 / 无 modifier）；菜单栈是**纯字符串栈**（无泛型
    菜单 ID、无级联子菜单树）；`update_ui_layout` **依赖 `Viewport` 资源**
    （自建 App 未插 `Viewport` 时按 1280×720 兜底，离屏程序需手动
    `insert_resource(Viewport::new(w, h))`）；按钮「底板 + 标签」是**两实体
    组装**（v1 单实体的 `UiRect` 因挂 `UiText` 只作定位载体、不绘制矩形，
    故按钮背景需独立实体，见模块头既有规则）。
22. **dogfooding V 发现的引擎摩擦（iris-infiltration 实践反馈）**——第五轮
    以第三人称潜入（24×18 m 设施、NavGrid 守卫、混合空间 + 足部 IK、
    ragdoll、接触事件、RTT 监控屏、UI 菜单栈、距离关节弹簧门）整体压榨
    R34–R41。本轮为**游戏轮，引擎零改动**，全部只绕行（纪律：摩擦留给
    下一轮打磨闭合）；条目按「现象 → 绕行 → 建议修法」记录。
    （更新：**六条已全部闭合于「引擎 API 打磨 V」轮**，iris-infiltration
    的绕行代码已同步删除并改用新 API 作回归验证——①双代事件生命周期
    （§3.10，MenuCommand 信箱删除）、②`IkRig` 节点映射（FootIk 与
    AnimationPlayer 解耦，零剪辑播放器删除）、③`CommandQueue` +
    `apply_commands`（§3.10，ragdoll 独占屏障改并行系统 +
    `spawn_deferred`）、④IK 链 scale 契约 + 骨长期望偏差 > 10% 一次性
    `warn`（`FootIk::expected_upper/lower` + 首解捕获）、⑤
    `set_subtree_hidden`（游戏侧递归隐藏删除）、⑥`BodyType::Kinematic`
    （§3.8，物理代理删除——压力板与弹簧门由玩家运动学胶囊真实碰撞
    驱动）：
    ① **`UiClickEvent` 产出阶段与事件清空冲突**：`update_ui_focus` 在
    **PostUpdate** 发事件，主循环在 PostUpdate 之后统一
    `clear_all_events`——Update 系统永远读不到按钮点击（连 `AppExit` 也
    只有 Update 阶段发出的能被帧尾退出检查读到，PostUpdate 发的会被
    清空）。**绕行**：游戏侧在 PostUpdate 尾部加独占系统（注册晚于
    `UiPlugin` 焦点系统，同帧即消费）把事件转写成 `MenuCommand` 信箱
    资源，Update 下一帧消费。**建议**：焦点系统改写命令资源 / 回调，
    或让 PostUpdate 产出的事件存活到下一帧 Update；② **FootIk 与
    `AnimationPlayer` 强耦合**：`update_foot_ik` 查询
    `(&AnimationPlayer, &FootIk)`——纯 IK 装配（无剪辑的腿链 rig）也
    必须挂一个**零剪辑** `AnimationPlayer` 才能被系统命中（语义无害但
    费解，`update_animations` 对空剪辑世界空转一次查询）。**绕行**：
    每条腿挂零剪辑播放器。**建议**：FootIk 改查自己的 rig 组件（节点
    映射自带），或文档明示「空播放器 = 纯 IK rig」惯例；③ **ragdoll
    生成需要 `&mut World`**：实体分配是结构性修改，游戏并行系统
    （`&World`）无法在事件帧 spawn——v1 缺命令队列。**绕行**：游戏
    Update 插独占系统（屏障语义天然隔离批次）读逻辑事件后
    `spawn_ragdoll_kinematic` + `release_ragdoll`。**建议**：引擎提供
    帧末 flush 的命令队列（`Commands::spawn` 类 API），保留并行系统的
    吞吐；④ **节点 `Transform.scale` 污染 IK 链长**：`chain_world` 沿
    父链累乘局部矩阵——若用节点 scale 缩放关节网格（常见捷径），scale
    进入链矩阵、骨长按比例缩水（本作实测腿长缩到一半、IK 落地查询
    全错），且无任何告警。**绕行**：关节节点 scale 恒 1，网格尺寸放在
    独立子网格实体。**建议**：foot_ik 模块文档写明「链上节点禁止
    非均匀 scale」契约，`debug_assert` 骨长帧间突变告警；⑤ **无
    「隐藏子树」原语**：击晕后要藏整个角色（GLB 模型 + 双腿链 +
    网格子孙），`Visibility` 是逐实体的——游戏侧只能沿 `Children`
    递归 set `HIDDEN`。**绕行**：`hide_subtree` 10 行递归。**建议**：
    `visibility` 模块提供 `set_subtree_hidden(world, root)`；⑥ **运动学
    角色无物理碰撞体**：`move_and_slide` 角色只认调用方静态体列表，
    不参与 `step_physics` 的接触事件与推挤——要踩 sensor 压力板
    （R34）、要推开 `DistanceJoint` 弹簧门（R36），必须额外 spawn 一个
    动态胶囊「物理代理」逐帧同步逻辑位置 / 速度（漂移 ≤ 1 帧位移即被
    覆写，对逻辑零影响，但概念上是重复体）。**绕行**：代理同步实体
    （`PROXY_NAME`）。**建议**：`RigidBody` 增加 kinematic 体类型
    （位置由调用方驱动、参与检测与推挤、不参与动力学积分）。
22. **景深 / 运动模糊为 v1**（§4.11，R43）：**MB 只覆盖相机运动**——
    物体级速度缓冲（逐实体运动向量 + 物体级重投影 + tile 分类）是大
    工程，运动物体在相机静止帧中无拖影；**DOF 的 focus_distance 口径
    为 view-Z**（沿视线方向的线性视距）而非径向距离——画面边缘的
    斜向物体实际距离更远，聚焦判定偏近（真实薄透镜按光程，需逐像素
    径向深度）；**场景深度副本不含透明 / 贴花 / 粒子**（半透明表面
    深度无定义，被透明物遮住的不透明面在副本中可见 → 透过玻璃看远处
    时 DOF / MB 按不透明面深度计算）；**副本为单采样**而主色为 4x
    MSAA（边缘像素的深度与颜色来自不同采样分布，焦外边缘可能有 1 px
    级噪声）；DOF 为**逐像素常量盘形平均**（不区分前景 / 背景倾角、
    无孔径形状 / 色散、采样不检查邻居 CoC——焦外亮源会渗入焦内像素
    边缘，无逐 tap 门控）；MB 屏幕外重投影点不做剔除（上一帧不可见的
    像素按无运动透传兜底）；MB 只服务主相机（RT 相机无后处理链，
    §8.20）。
23. **音频 v2 为 v1 限定**（§4.14，R44）：**只支持 OGG 容器里的 Vorbis**
    （feature 只开 `ogg + vorbis`，MP3 / FLAC / Opus 报 `Unsupported`，不做
    静默降级），声道数上限 8；**音乐总线单轨**——新轨硬拆上一轨，无
    crossfade / 无多轨混音，且不参与空间化（无距离衰减与声像）；**混响只挂
    音乐总线**，音效与空间音效不受 `set_reverb` 影响，且简化 Schroeder 无
    阻尼低通（高低频同速衰减，尾巴偏亮）、无预延迟、无逐声音 send 量；
    **下溢填静音不做任何补偿**（不拉伸、不补帧——解码跟不上实时消费时听感
    是卡顿，队列深度即抖动预算，8 × 4096 帧 @ 48 kHz ≈ 0.68 s）；
    `OggVorbisStream::from_bytes` 在构造与每次 `rewind` 时**复制一次压缩字节**
    （symphonia 的 `MediaSourceStream` 接管所有权；`open(path)` 路径改为重开
    文件，不常驻）；**循环用「重建 demuxer / decoder」实现而不是容器 seek**
    （省掉 seek 后解码器状态一致性这一类易错空间，代价是一次 re-probe）；
    **解码错误在流式路径不可回传**（跑在后台线程）：坏包跳过、其余错误
    `log::warn` 后按「流结束」处理——需要明确错误的场合走 `parse_ogg` 一次性
    路径；未做显式 gapless 裁剪（实测本仓库 fixture 帧数与时长精确相符，
    但缺粒位置提示的码流可能保留约 2 k 采样头尾冗余）；**wasm32 无线程**
    ⇒ 退化为混音调用内同步解码（该平台上解码耗时计入当次混音，因其本无
    cpal 实时回调）。
24. **资产打包 / 挂载 / 异步加载为 v1**（§7.3，R45）：**PACK1 无压缩、
    无去重、无加密**（CRC-32 防损坏不防篡改；同内容两份存两份，压缩率
    收益留给版本号演进）；**大资产整条读回**——`PackReader::read` 返回
    完整 `Vec<u8>`，条目内部分段流式读（边读边解码）未做；**挂载无
    写入 / 热重载 / 监听**；**Embedded 源仅文本**（路径 → 字符串表，
    二进制嵌入宏与 fetch 网络源在路线图）；**pack 内的 glTF 须自包含**
    （GLB 容器或 data URI——内存来源没有 base_dir，外部 uri 引用显式
    报错，见 `spawn_gltf_instance_from_bytes`）；**异步骨架只覆盖字节
    读取**——`AssetKind` 仅 Scene（后台 JSON 校验）与 RawBytes 两类，
    渲染纹理 / 网格的 GPU 上传异步化、进度回调、优先级与取消均未做；
    **结果无完成顺序保证**（native 并行乱序到达），且丢弃 `AssetLoader`
    不等待在途请求（无 join 语义）；**glTF 加载器只有内存入口不支持
    外部 uri**，路径入口行为不变。
25. **演出时间线为 Sequencer-lite**（§4.22，R47）：**单播放器**
    （`TimelinePlayer` = Resource，并发多条时间线不支持）；相机轨道
    **段内均匀参数化**（不按弦长 / 向心重参数化——非均匀关键帧间距的
    速度感靠人工调校）；**对话无打字机 / 无发言人头像**，位图字体仅
    ASCII（中文对白不渲染）；黑边无圆角 / 无进出动画；**不自动拦截
    游戏输入**、不暂停相机跟随（游戏侧自理）；**不嵌套**（播放中再次
    start 直接替换并恢复旧快照）；look_at 前向与 +Y 平行的竖直机位以
    +Z 为上参考（滚转自由度不由脚本控制）；演出实体（对话框 / 黑边）
    按 `entity_named` 每帧线性查找（O(n)，每帧三条实体可接受，高频
    场景应缓存句柄）。

26. **dogfooding VI 发现的引擎摩擦（iris-assault 实践反馈）**——第六轮
    （写实垂直切片，六轮新能力全量实弹）。游戏轮纪律：只绕行不改引擎，
    各条绕行实现写在 iris-assault 内。**→ 打磨 VI 已把七条全部闭合**
    （①②③⑤⑥ 引擎侧修复 + 游戏侧删除绕行验证；④⑦ 文档 / 工程约定
    闭合），逐条「闭合」段标注如下，R48 记录原文保留作历史。
    - **①足部 IK 回写四元数不归一化 → 逐帧平方复合爆炸（本轮最重，
      引擎真 bug 级）**：`update_foot_ik` 的旋转回写（世界系旋转弧
      `from_rotation_arc` 复合 + 父共轭反推局部）**不做归一化**，四元数
      范数逐帧漂移；当下帧没有动画通道覆写被 IK 写过的骨（如 idle 剪辑
      只有骨盆 bob、无腿通道），回写值被下一帧 IK 读回再复合——范数
      **平方级爆炸**（实测 idle 角色 ~1.9 s 内 thigh 旋转范数
      386 → 5.8e7 → NaN，随后骨长 NaN 撞 `solve_two_bone_ik` 的正值
      断言 panic）。iris-infiltration 未触发纯属侥幸（其 walk 剪辑每帧
      覆写双腿旋转，覆写即重置）。→ 绕行：资产侧给 idle 剪辑补恒值腿
      通道（每帧以单位四元数重置被 IK 写的骨）→ **闭合（打磨 VI）**：
      两处 IK（`update_foot_ik` / `update_hand_ik`）在局部旋转产生处
      `normalize()`——归一化是 IK 的责任（它是唯一覆写「不被剪辑重置的
      骨旋转」的系统，`foot_ik` 模块文档有推导）；回归锁
      `foot_ik::tests::foot_ik_quaternion_norm_stable_without_clip_overwrite`
      与 hand 侧同名测试（剪辑只写髋 / 肩平移、无腿臂旋转通道，600 帧
      逐帧断言分量有限 + ‖q‖−1 ≤ 1e-3 + 骨长保持，并比对同相位两帧的
      关节世界位 ≤ 1e-3）；iris-assault 的 idle 恒值腿通道**删除**
      （资产重生成），实弹锁在
      `iris-assault/tests/headless.rs::intro_cinematic_plays_and_hands_over_control`
      （撤掉修复即复现 R48 的 `solve_two_bone_ik` panic）；
    - **②GLB 内存加载入口不在 prelude**：R45 的
      `spawn_gltf_instance_from_bytes`（pack / mount 路径的刚需入口）
      未列入 `iris-render` prelude——依赖纪律「游戏 crate 只依门面」
      的下游拿不到它 → 绕行：iris-assault 显式直依 iris-render
      （Cargo.toml 注明偏离）→ **闭合（打磨 VI）**：
      `spawn_gltf_instance_from_bytes` 已进 `iris-render` prelude，
      iris-assault 的 iris-render 直依与其 Cargo.toml 偏离注释一并删除
      （游戏 crate 依赖纪律回归：全局只剩 glam 一条非引擎依赖线）；
    - **③GltfBuilder 不支持纹理引用**：R42 材质通道（法线 / ORM）无法
      在资产生成端表达，程序化写实资产断链 → 绕行：加载后克隆
      `MaterialDef` + `with_normal/with_orm` 替换（游戏侧二次处理）
      → **闭合（打磨 VI）**：`GltfBuilder::material_textured` +
      `GltfMaterialTextures`（baseColor / 法线 / ORM 三通道，ORM 图同
      下标写进 `metallicRoughnessTexture` 与 `occlusionTexture`，走
      加载器的「同下标免重打包」快路）——像素 build 时 PNG → base64
      `data:` URI **内嵌 GLB**（自包含，`spawn_gltf_instance_from_bytes`
      可用；登记时校验宽高与非零像素缓冲，内容相同的图只嵌一份）。
      往返测试 `iris-engine/gltf_builder.rs::tests::built_textured_material_roundtrips_through_the_memory_loader`
      断言三通道像素逐字节一致（PNG 无损）+ 色彩空间按通道标注 + 两材质
      共享同一 store 条目。iris-assault 侧克隆材质绕行删除，插件层只剩
      敌我 `base_color` 配色覆写。诚实代价：角色 GLB 55 KB → 241 KB
      （4 张 128² 法线 / ORM 图 base64 后占绝对大头），换来资产端一次
      生成即可渲染写实材质；
    - **④无 Vorbis 编码器**（R44 只有解码）：程序化生成的音效 /
      氛围无法编码进 OGG，pack 的音频条目只能取外部资产 → 绕行：复用
      引擎音频测试 fixture（2 s 正弦）作占位氛围 → **闭合（打磨 VI，
      工程 / 文档侧）**：明文接受**外部资产管线**——引擎不做编码器
      （运行时只需解码；编码属资产生产期，见 §4.14「编码管线」），
      标准命令 `ffmpeg -i in.wav -c:a libvorbis -q 4 in.ogg` 收录进
      README「环境要求」与 §4.14（本机实测 16.4× 压缩、`ffprobe` 确认
      vorbis 编码）。游戏侧占位氛围保持不变（不把二进制音频塞进仓库）；
    - **⑤DOF 动态对焦无引擎辅助**：`DepthOfFieldSettings` 是静态资源，
      而「战斗中对焦交战目标」要求逐帧更新对焦距离（静态值在快速接近
      时把敌人糊成一团——本轮实测确认需逐帧更新，资源形态下游戏侧每帧
      写可用但无原语）→ 绕行：插件层每帧算相机→最近敌距离写资源
      → **闭合（打磨 VI）**：`DepthOfFieldSettings::focus_target:
      Option<Entity>`（**资源字段而非新组件**——对焦目标是「谁来配这台
      镜头」的一次性配置，不需要组件存储的迭代语义）；解析走纯函数
      `post::resolve_focus_distance(world, &settings, &view)`，调用点在
      `Renderer::draw` 的 DOF uniform 上传处（相机已解析、链头第一个
      全分辨率 pass 之前，DOF 关闭时不解析——缺席零开销）；口径与
      `focus_distance` 一致为 **view-Z**（目标世界位经视图矩阵取 −z）；
      实体缺席 / 无 `GlobalTransform` / 自身 `Visibility::HIDDEN` →
      回退标量，解析值非有限同样回退。测试：`post::tests` 三条纯函数
      单测（None 回退、目标移动跟随、偏轴 view-Z、缺席 / 隐藏回退）+
      `iris-render/tests/post_render.rs::dof_focus_target_overrides_scalar_bitwise`
      （经目标帧与同值标量帧逐位一致 = 渲染器读取路径的锁）。
      iris-assault 逐帧写 `focus_distance` 的绕行删除，改为对焦最近活敌
      的胸骨（无活敌回玩家，演出期标量喷泉）；
    - **⑥骨架下标契约全手工**：glTF 加载器不带节点名，
      「资产生成器 ↔ 骨架表 ↔ 插件层」三方的 22 节点下标契约只能靠
      headless 守护测试（节点数 + 腿链父子形状）兜底；且**网格必须
      直挂骨节点**——若做成骨的独立子节点，加载器拆 primitive 子实体
      会引入额外节点使 `node_entities` 错位（本轮第一版 42 ≠ 22 立刻
      爆出）→ 绕行：`skeleton` 模块单一事实来源 + `NODE_COUNT` 守护
      断言 → **闭合（打磨 VI）**：`AnimationPlayer::node_names`（与
      `node_entities` **同下标**的 glTF 节点名表，由
      `GltfInstance::node_names` 随 `player()` 带入——加载器给
      `GltfNode` 补 `name` 反序列化）+ 查询 API
      `AnimationPlayer::node_entity_by_name(name)`；**重名取先**
      （向量序确定 ⇒ 结果确定；glTF 不强制名字唯一，唯一性由资产侧
      保证，`iris-assault/src/skeleton.rs` 的 `names` / `NAMES` 表 +
      `bone_names_are_unique_and_complete` 测试即此保证）。测试
      `gltf::tests::node_names_drive_lookup_by_name`（名字表与实体表
      对齐、按名与按下标命中同一实体）与
      `animation::tests::node_entity_by_name_takes_first_and_tolerates_gaps`
      （重名取先 / 有名无实体 / 无名表恒 None）。**下标仍是动画通道与
      `AnimationMask` 的寻址键**（引擎侧通道数据即「节点下标 + 路径」，
      不改），所以骨架表的分工变成：下标服务动画、名服务装配；
      iris-assault 迁移范围＝装配层全部（IK 链、枪身挂点、敌我配色、
      枪身隐藏），动画通道与遮罩保留下标契约并在生成器内加
      「声明顺序 = 骨架表下标」断言（错位当场爆炸）；
    - **⑦example 跨 crate 同名冲突**：iris-assault 的 `gen_asset`
      example 与 iris-engine 的同名 example 输出文件互相覆盖
      （cargo 警告 "may become a hard error"）→ 绕行：改名
      `gen_assault_assets` → **闭合（打磨 VI，工程约定）**：命名约定
      写入 §7.2（example 名跨 crate 唯一，游戏 crate 带游戏前缀）。

## 9. 关键设计取舍记录

- **回调式查询 vs 迭代器**：见 §3.4。牺牲一点语法糖换取零 unsafe 与确定性 panic。
- **稀疏集 vs Archetype**：见 §3.1。Iris 优先结构透明与动态组件能力；
  archetype 的成批迭代优势在实体百万级时才显著。
- **运行期借用检查 vs 编译期**：编译期（如 hecs 的 unsafe 实现或 slotmap 双缓冲）
  换不来「错误 = panic」的明确语义；运行期检查的每次查询只有常数次原子操作，
  开销在纳秒级，换取的是整库零 unsafe。
- **MSAA 固定 4x**：管线单份、代码简单；前向渲染下 MSAA 仍是性价比最高的抗锯齿。
