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

```
├── crates/              # 引擎 crate
│   ├── iris-macros/     # 过程宏：#[derive(Component / Resource / Event)]
│   ├── iris-core/       # ECS、调度、变换层级、时间、事件、物理与碰撞
│   ├── iris-render/     # wgpu 渲染器、glTF 管线、离屏渲染
│   ├── iris-platform/   # 窗口配置、键鼠输入
│   ├── iris-audio/      # 软件混音器、WAV / OGG 加载、流式音乐总线、混响、程序化音效、cpal 设备层
│   ├── iris-engine/     # App 门面、插件系统、主循环、场景序列化 + 关卡格式 v2（level）+ 示例
│   │   └── examples/    # 示例
│   └── iris-editor/     # 关卡摆放编辑器
└── games/               # 引擎下游示范游戏
    ├── iris-game/       # I：第三人称限时收集
    ├── iris-arena/      # II：波次生存竞技场
    ├── iris-bastion/    # III：塔防 Iris Bastion
    ├── iris-ascent/     # IV：3D 平台跳跃 Iris Ascent
    ├── iris-infiltration/ # V：第三人称潜入 Iris Infiltration
    └── iris-assault/    # VI：写实战斗庭院 Iris Assault
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
  属门面层的编排职责）与其 v2 超集**关卡格式**（§4.26，`level` 模块——
  地形 / 导航 / 烘焙参数段与光照贴图产物文件，编辑器轮的数据地基）。
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
  **视觉上限轮（内容侧，引擎零改动）**：角色与庭院从「图元拼装」重建为
  **程序化蒙皮人形**（新模块 `geom` / `soldier` / `soldier_tex`：22 骨 + 1 蒙皮
  网格 + 单材质 512² 图集 = 单 draw，31065 顶点 / 28974 三角形，装备与武器
  刚性挂骨）与**程序化建筑庭院**（新模块 `scenery` / `scenery_tex`：铺地 /
  拱形壁龛与雕像 / 多立克石柱 / 三层喷泉 / 悬挂火盆 / 旗帜 / 木门，9 材质
  42980 三角形）。关卡档保留**碰撞 + 灯光 + 粒子**权威
  （`level_roundtrip.rs` 零改动全过），视觉由 `assets/assault_courtyard.glb`
  承担、插件在装配成功后按名隐藏档里的可见图元（失败则回退为「能玩的旧
  庭院」）；碰撞契约（视觉不得越过碰撞面 8 cm 以上、2.10 m 以上自由）由
  逐顶点单测守护。验收图 `docs/soldier.png` / `docs/courtyard.png` /
  **角色建模迭代轮**：手足按真人比例重建（指长 0.030 → 0.064~0.080 并绕掌心
  圆弧屈曲；战靴的趾→跟一体鞋底 / 足弓抬起 / 鞋帮楦体 / 鞋头包 / 后跟杯 / 鞋舌 /
  鞋带与鞋眼），四肢加布褶与束带、躯干加立领；并修掉一个**影响整身光照的真
  bug**——放样 / 旋转体 / 超椭球的环绕方向取决于调用方给的截面基与行方向是否
  成右手系（right × up = 行方向），写反的壳法线朝内、整片发灰：逐件有向体积
  诊断在整身抓到 20 个（鞋楦三件、双肩三角肌、双掌鱼际、头盔耳罩、脸侧滤罐、
  腰侧挂具、腕环、刀镡……），且镜像部件未必同病。现在基元层按首个有效面几何
  统一纠向（geom::orient_outward，端面按与径向夹角跳过），Assembly::add 保留
  「闭合壳有向体积必须为正」的不变量兜底；材质 bump 按现行量纲全部减半、枪械
  金属改磷化黑。
  **角色建模迭代轮二（四肢 → 躯体 → 关节 → 衣物 → 细节 → 头面）**：新原语
  `limb_shaped` 让每个截面的中心可以离开轴线（股四头肌前凸、腓肠肌后鼓、踝上
  收成跟腱、肱二头肌与肱三头肌分列前后），躯干 14 个截面同样带前后偏移
  （臀后凸 / 腰内收 / 胸骨前凸）；肘 / 膝改成多片铰接护具（肘三件 + 铆钉、
  膝五件 + 铆钉，全部走 blend_chain，关节内侧加压缩褶）；补臀部后袋、下摆抽绳、
  背部育克缝、肩部徽记；头盔改成 FAST 型盔形并加颅顶中脊、通风窗、扫掠侧导轨、
  盔带与猫眼片，面部做成「镜框 + 左右镜片 + 鼻梁桥 + 侧扣 + 连接带」的护目镜
  与带鼻梁脊 / 颊部滤罐 / 呼气阀 / 口栅的面罩。同时修掉两个「护膝其实不存在」
  的真 bug（膝下裤脚比护膝还粗、护膝绑带漏平移 ⇒ 悬在两腿之间且左右逐位重合）
  与一处材质问题（面罩玻璃金属度 1 + 近黑基色 = 无环境反射时的纯黑洞）。
  `docs/assault.png`；摩擦见 §8.42。
- **iris-editor**：**关卡摆放编辑器**（R58a 最小 MVP，W5 内容产能工具的
  第一块）——与六个示范游戏同层的**工具** crate（bin，原生为主；依赖
  纪律同款：iris-engine 门面 + iris-core 纯函数 + glam，零新增第三方
  crate）。**架构硬要求 = 命令层与交互壳分离**：`edit.rs` 命令层（编辑
  器状态 `EditorState` + 摆放 / 拾取 / 拖拽 / 删除 / 复制 / 微移 / 缩放
  全是 `World + EditorState + 事件参数 → 确定性结果` 的自由函数，
  headless 可单测）与 `shell.rs` 交互壳（winit 窗口 + 飞行相机 + 输入
  翻译，刻意薄；`EditorPlugin::build_schedule` 暴露编排供 headless /
  离屏复用，六个游戏同款范式）。数据层 = R57 关卡档 v2（不发明新格式）：
  `open_level` / `save_level` 走 `load_scene` / `save_scene`，`level` 段
  经 `LevelRegistry` 快照语义只读保留。两条机制不变量（§4.27）：编辑器
  spawn 的网格 / 材质**同步登记进 `SceneAssetRegistry`**（否则 save 还原
  不出 mesh 段）；**句柄编号对齐**（同源去重 + 瞬态视觉资产先建 ⇒ 保存
  与重载两侧 `MeshHandle` / `MaterialHandle` 逐位相同）。验收：headless
  19 项（9 实体往返逐位 + assault 庭院档经编辑器往返）+ 离屏验收图
  `docs/editor_mvp.png` / `docs/editor_viewport.png`；诚实清单 §8.34 与
  edit.rs 模块文档。

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

- **三种系统形态**：独占系统 `fn(&mut World)`（串行，可做结构性修改）；
  并行系统 `fn(&World)`——只读世界签名，组件写访问走内部尝试型锁
  （`world.get_mut` 等本就是 `&self` 方法），结构性修改被类型系统排除；
  分片并行系统（R73）`Fn(&World, &[Entity]) -> Vec<(Entity, T)>`——
  数据并行（见下）。
- **访问集声明**：`Access::new().read::<T>().write::<U>()` 声明并行系统的
  组件读写集；调度器按「一方写 ∩ 另一方读写 ≠ ∅」判定冲突。分片系统的
  写组件 `T` 由签名给定并自动并入写集。
- **贪心分批**：阶段内按注册顺序扫描，与当前批次全部成员无冲突则并入，
  否则开新批；独占系统独占一批（屏障）。批次间严格保序，批内不保证顺序。
- **执行（任务列表统一）**：批内成员展开为任务——普通并行系统 1 任务、
  分片系统 W 任务；`std::thread::scope` + 原子计数器自取任务，线程数取
  `min(任务数, 可用核数)`；单任务或单核（wasm / 1 核）内联顺序执行；
  `&World` 跨线程共享依赖 `World: Sync`（§3.3）。
- **数据并行分片（R73，两段式）**：调度器把实体列表（稀疏集插入序）按
  `shard_count_for(items, cores)` 切成固定连续区间（`shard_range` 尾片吸收
  余数），每片一次调用。**分片闭包只读世界、返回 `(Entity, T)` 写缓冲**，
  join 后按「成员序 × 分片序 × 缓冲序」（= 实体列表序的稳定全序）串行
  写回——设计由存储模型倒逼：组件列写锁是**列级** RwLock（`sync.rs`），
  分片线程并发写同一列必冲突，故写全部延迟到写回段。稳定全序 ⇒ 结果与
  分片数 / 线程数 / 机器核数无关（native 多线程与 wasm 单线程逐位一致）。
  诚实边界：写回段串行，写密集、计算轻的变换分片收益有限。分片数公式
  `items ≤ 1024` 不分片、否则 `min(cores, items/1024)`（`MIN_SHARD_ITEMS`），
  每片 ≥1024 实体摊薄线程创建开销；输出只依赖 (实体数, 核数) 两入参。
- **引擎自身的并行系统**：`update_animations`（写 AnimationPlayer /
  Transform，读 Time）——DefaultPlugins 每帧真实走并行路径；Update 后的
  PostUpdate 阶段执行独占的层级传播（跨阶段天然屏障）。
- **阶段编排**：每帧 `FixedUpdate × N → Update → PostUpdate`（PostUpdate
  每帧一次、不在固定步长循环内），由 `App` 主循环驱动；Startup 在首帧前
  执行一次。
- **实测**：criterion 基准 `parallel_run_8_systems`（8 个互不冲突分量系统
  × 1 万实体）、`whole_transform_100k` / `sharded_transform_100k`（10 万
  实体、64 次 sqrt 重型工作体：55.5ms → 39.4ms = 1.41×；纯拷贝级轻变换
  同机实测净亏 35.3ms vs 24.0ms——串行写回 + 线程创建占大头，两数字
  连同边界入基准注释），`cargo bench -p iris-core` 守护并行路径回归；
  历史实测 16 核上加速 3.27×（旧 example 版基准，已并入 criterion
  benches）。未达线性是每系统内部仍为单线程遍历 + 线程启动开销 + 内存
  带宽上限。
- **正确性断言**：并发观测（原子计数器峰值 ≥ 2）、冲突系统分批保序、
  独占屏障可见性、分片结果与分片数无关（3 万实体真多线程逐实体
  `to_bits` 对账参考实现），均有确定性测试；诚实性由「冲突即 panic」
  兜底——声明不诚实的并行/分片系统会在运行期被锁检查当场击穿。

### 3.8 物理子系统（iris-core/physics + iris-core/character）

轻量刚体物理 v1（Sphere / AABB / **旋转感知** Capsule + **角速度**，R50）：
与变换层级同款的「组件 + 单一系统」模型，零新依赖（全部数学基于 glam
手写）。

- **组件模型**：`Collider`（形状 Sphere / AABB / **旋转感知 Capsule** +
  `is_static` + `restitution`）与 `RigidBody`（速度 / **角速度** / 重力
  缩放 / 摩擦 / 线性 + **角**阻尼 / `body_type`）。**组件缺席语义**：只有
  `Collider`（无 `RigidBody`）= 静态体（与 `is_static` 取或）——不积分不
  移动，只作碰撞对象；只有 `RigidBody`（无 `Collider`）= 仅受力积分，不
  参与碰撞；两者都有且非静态 = 完整刚体。**运动学刚体（`BodyType::Kinematic`，
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
  **旋转感知 Capsule（R50）**：轴 = `Transform.rotation` 旋转后的本地
  ±Y（端点 = 中心 ± (half_height − radius)·(rotation·Y)，`half_height ≥
  radius` 构造断言）——检测（`detect_pair_rotated`）、射线
  （`ray_capsule_rotated`：局部系变换 + 复用竖直解析解）、扫掠
  （`shape_sweep_rotated`）、推离（`resolve_static_push_rotated`）、
  broadphase AABB（`|rotation·Y|·轴半长 + radius` 的精确包络）**全链
  跟随**；恒等旋转与 R31 逐位一致（`rotation·Y` 在 glam 展开下逐位等于
  `Y`，单测 `identity_rotation_matches_upright_api_bitwise` 锁定六类 API）。
  **Box 仍是世界轴对齐 AABB**：`rotation` 对 Box 无语义（OBB/SAT 留后续
  轮）——旋转体的碰撞请用胶囊。
- **求解管线**（`step_physics`，系统只做编排，数值逻辑全在纯函数中）：
  两阶段收集（读 `Time.delta`，缺席视为 0；照 `update_animations` 的
  「收集 → 写回」两段式，迭代期间不持组件借用；收集**位置 + 旋转**两份
  快照）→ 半隐式欧拉积分（`v += g·scale·dt`；`v ×= exp(-damping·dt)`；
  `pos += v·dt`；**角通道（R50）**：`ω ×= exp(-angular_damping·dt)`、
  `q ← normalize(q + ½·(ω,0)·q·dt)`——四元数微分式，归一化每步防漂移
  （打磨 VI 的 IK 范数教训），**与平移不耦合**且 `ω == 0` 时整段跳过 ⇒
  既有场景的 `rotation` 逐位不被触碰）→
  **均匀网格 broadphase 候选筛选**（`BroadphaseGrid`，iris-core/broadphase：
  按「积分后、求解前」的 AABB 快照一次建网格（旋转胶囊用精确包络），cell
  尺寸取全部**非静态**体的最大轴向跨度钳制 [1.0, 50.0]——静态大地面不参与
  定尺寸，防 cell 撑到场景尺度使网格退化；无动体回退全体最大。**配对遍历序
  等价性**：候选升序 + `j > i` 过滤 ⇒ 与朴素 O(n²) 双层循环完全一致，既有
  bit 级确定性测试无需改动即守护等价性。**必须 BTreeMap 而非 HashMap**
  ——键字典序迭代与哈希种子 / 插入序 / 平台无关，是 bit 级确定性的选型
  前提）→ 逐对 `detect_pair_rotated` 精检（跳过静-静对）→ 响应（穿透沿
  法线分离，静态体不动、动体按动体数均分——等质量近似；法向速度按
  restitution 反弹 `v_n' = -e·v_n`，仅当朝向穿透，e 取双方 max；切向乘
  `max(0, 1-friction)`，friction 取参与响应的动体均值——近似库仑；**接触
  不产生角冲量**，见诚实清单——**接触求解 v2（R76）以 `ContactSolverV2`
  资源 opt-in 补齐角冲量与库仑摩擦锥**，见下条）→ 2 次迭代位置松弛
  （`SOLVER_ITERATIONS`，
  速度响应只在首轮施加——重复施加会把摩擦/反弹多次结算；每轮接触后追加
  约束轮：**距离 → 摆动 → 目标点**）→ 写回 `Transform.position` /
  **`Transform.rotation`** / `RigidBody.velocity` /
  **`RigidBody.angular_velocity`**（静态体与运动学体不写）。
- **接触求解 v2（R76，`ContactSolverV2` 资源启用——缺席 = v1 逐位不变）**：
  注册资源后 `step_physics` 的**接触求解段**换成顺序冲量求解器（检测 /
  过滤 / sensor / 事件采样 / 关节轮 / 写回共用 v1 管线；六个游戏不注册
  ⇒ 轨迹 FNV 零变化）。**角冲量**（§8.27① 闭合）：接触点臂
  `r = point − center` 进冲量 `ω += I⁻¹(r × P)`，惯性张量解析计算
  （`shape_inertia`：球 / 盒精确，胶囊 = 圆柱 + 双半球精确组合——
  `hh = r` 退化同归球，单测守护；`inv_inertia_world`：胶囊随体旋转
  `R·diag⁻¹·Rᵀ`，盒按世界轴对齐语义同盒碰撞体恒 AABB 口径）；
  **库仑摩擦锥**：切向冲量向量按 `|λ_t| ≤ μ·λ_n` 钳制（μ 取参与响应的
  动体摩擦的**几何均值**——v1 参与规则的 v2 版，纯 Collider 静态地板
  无摩擦数据、球自身 `RigidBody::friction` 表达）；**迭代**：首轮收集
  全部接触（遍历序 = v1 候选序、事件采样点相同——首帧事件逐位一致
  单测守护），`velocity_iterations` 轮 Gauss-Seidel 顺序冲量
  （`solve_contact_impulse` 纯函数：法向 λ ≥ 0 累积钳制 + 摩擦锥，
  弹性并入首轮捕获的 `target_vn = −e·max(0, v_n0 − threshold)`，低速
  阈值防堆叠微弹），位置投影按**逆质量加权**（替代动体数均分——
  `RigidBody::mass` 字段，默认 1.0，v1 路径不读）+ `slop` 容差 +
  `beta` 比例。**接触点** `contact_point_rotated`：九分支界面点近似
  （两侧表面最近点中点——球-球恒在连心线上 ⇒ 法向冲量零力臂，与
  真实物理一致）。诚实边界：**无 warm starting**（跨帧冲量缓存不做
  ——有状态与「资源缺席 = 无状态」冲突，收敛速度低于生产引擎，堆叠
  需更多轮次）、各向同性 μ、关节轮仍 v1（无角冲量耦合）、mass 不进
  关卡序列化。解析验收：双球弹性对撞质量加权对照
  （`v' = (m₁−m₂)/(m₁+m₂)·u`）、偏心冲量手算 ω、锥边界 `|λ_t| = μλ_n`、
  滑动球起转方向、v2 全轨迹 bit 级确定性、v1 零变化锁。
- **约束与关节（R36 距离 / 目标点 + **R50 摆动**，位置约束集）**：
  `DistanceJoint`（距离约束——`stiffness = 1` 硬约束刚杆 / `(0,1)` 弹簧 +
  轴向相对速度 `damping`）、`TargetJoint`（目标点约束——把实体拉向世界
  固定点，`rest_distance` 到达半径）与 **`SwingJoint`（摆动关节，R50）**
  ——机关门 / 吊灯 / 弹簧板与**真人形 ragdoll** 的地基。R50 之前 v1 是纯
  位置物理，旋转铰链无法诚实实现；角速度 + 旋转感知胶囊落地后旋转约束
  随之解锁（仍**没有** hinge / twist 两自由度铰链，「只能单向弯」留后续轮，
  v1 用锥近似）。`SwingJoint` 三段求解：① 本地锚点（随各自实体旋转）的
  距离投影（双向刚杆，同 `DistanceJoint` 算式）；② **摆动锥限位**——
  `∠(a锚→b锚, 锥轴) > swing_limit` 时把 b 锚拉回锥面（目标点 = a 锚 + 锥面
  最近方向 × 当前长度；**锥轴 = `rot_b · rest_offset.normalize()`**，随父端
  旋转 ⇒ 解剖学口径：躯干翻倒时限位跟着翻，肢体不被钉回出生朝向）；
  ③ **肢体轴方向修正**——子端本地 +Y 以最小弧（`from_rotation_arc`，无
  三角超越）对齐到限位内的 a锚→b锚 方向（胶囊肢的朝向由此跟随链的姿态；
  没有这段，倒地的肢体只会保持出生朝向——接触不产生角冲量）。三段的位置
  修正**全给子端**（层级投影：父端不被子端拽动，躯干靠自身接触承托）——
  实测均分会让链上约束互相扰动、稳态残差达 rest 的 35% / 限位外 22°，
  层级投影在**根先叶后**的求解序下每轮末尾全部约束精确满足（残差 1e-7 =
  f32 舍入量级）；速度响应 = 两条轴（锚点连线 + 锥面修正方向）的相对速度
  阻尼（锥面是切向位置修正，速度不跟着衰减会变成能量泵：实测肢体 4 s 后
  仍以 5 m/s 沿锥面滑行）。求解嵌入 `step_physics` 松弛大循环**每轮接触
  之后**（顺序理由：约束优先级高于接触——先把实体投影回关节语义位置保
  结构完整，再让接触分离穿透；反过来接触推离会持续对抗约束、结构撕裂），
  轮内顺序固定**距离 → 摆动 → 目标点**，速度响应只在首轮（与接触同轮同
  理由）；位置 / 旋转修正照常走既有写回阶段（零额外写回代码）。收集序 =
  组件存储序 ⇒ bit 级确定性不破坏（关节场景单测守护）。诚实边界：位置
  松弛投影**无能量守恒**（弹簧是视觉弹跳语义，非力积分，无势能 / 共振）、
  距离关节双向刚杆（非「只拉不推」的绳）、锥限位不区分弯曲平面、链须根
  先叶后（乱序退回 Gauss-Seidel 残差，不 panic）、挂载体须为碰撞体动体、
  target 无效（despawn）/ 两端重合 / `rest_offset` 零长 → 约束跳过不 panic
  （与实体世代语义一致）。纯函数 `solve_distance_joint` / `solve_swing_joint`
  / `solve_target_joint` 与 `solve_contact` 同款返回四通道响应（摆动另加
  旋转通道），可脱离世界单测。
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
- **布娃娃与死亡混合（iris-core/ragdoll，R37 v1 / **R50 v2**）**：
  「死亡混合」= **运动学窗口两阶段**：① `spawn_ragdoll_kinematic`
  （`&mut World`——实体分配是结构性修改，分配器非内部可变）按
  `RagdollDesc` 生成**静态**肢体（动画接管期——静态体挡玩家 / 弹道，
  游戏侧照常驱动 `Transform` 定格死亡姿势；关节此期被跳过——挂载体须为
  动体）；肢体全部挂 `RAGDOLL_GROUP`（bit 0）组位、`mask` 排除该位 →
  **自碰撞免抖**（生成时相邻肢体刻意相切 / 微重叠，互相求解会被挤开炸裂；
  与场景照常碰——世界侧默认全 1 双向认可）。② `release_ragdoll`（只需
  `&World`，组件写锁翻转、实体失效静默跳过）把 `is_static` 翻转为 `false`
  + 写入继承速度 + 可选冲量 `(方向, 强度)` 踢飞（物理接管期——关节自动
  生效，链拓扑保持）。两条路线：
  - **v1 球肢（R37，逐位不变）**：`RagdollDesc::humanoid`（六球）+
    `links` 相邻对 → `DistanceJoint` 硬约束（rest = 生成时两端中心实际
    距离）。球各向同性：翻滚 / 拖拽 / 堆叠语义正确且不需要旋转；iris-assault
    / iris-infiltration 照旧消费；
  - **v2 胶囊肢（R50）**：`RagdollDesc::humanoid_v2`（十肢：躯干胶囊 +
    头球 + 上臂 / 前臂 / 大腿 / 小腿各二）。`RagdollLimbDef` 增
    `half_length`（`≤ radius` ⇒ 球肢 = v1 分支；`> radius` ⇒ 胶囊肢）与
    `axis`（近端方向 = 本地 +Y 的世界朝向，生成时写进 `rotation`——胶囊
    轴不能从 offset 推：手臂挂在躯干侧面时「中心 → 父中心」是水平的，与
    竖直的臂轴正交）；`links` 升级为 `RagdollLink { a, b, swing_limit }`
    （`None` ⇒ v1 距离关节；`Some(limit)` ⇒ 自动接线 `SwingJoint`：关节点 =
    子端近端帽中心、`anchor_a = 0`、`anchor_b` = 关节点在父端本地系、
    `rest_offset` = 关节点 − 子端中心（父端本地系）⇒ 长度给距离 rest、方向
    给锥轴；**links 须父先子后**）。释放时另注入角速度 `ω = topple_axis ×
    强度 × RAGDOLL_SPIN_FACTOR`：转轴**优先绕链根肩线**（前扑 / 后仰）
    而非 `Y × 冲量`（侧倒）——实测侧倒时靠地一侧肩窝落到地面以下、被钉在
    肩窝上的手臂转不出来（接触不产生角冲量）、稳态埋进土里 0.15–0.37 m，
    绕肩线翻倒同一场景埋深 0.028 m；`RAGDOLL_SPIN_FACTOR = 0.8` 按
    「渐近总转角 = ω₀/λ ≈ 92°」取值让躯干恰好躺平。
  共同诚实限制：**堆叠靠地面 / 场景**（无肢体间挤压）、速度无钳制（无
  CCD）、**接触不产生角冲量**（落地不自转，翻滚全靠注入的 ω + 关节方向
  修正）、侧躺仍可能埋臂（上限 ≈ 肩偏 − 躯干半径 + 臂半径；根治需多胶囊
  躯干 / OBB 或角冲量）。确定性：生成序 = 定义序、无随机源——同 desc 两次
  全流程（位置 + 旋转）逐位一致；240 步倒地链不散架、摆角不超限位（v2
  实测残差 1e-7）、质心收敛、速度有界（单测守护）。视觉验收：v1
  `examples/ragdoll_snapshot.rs` → `docs/ragdoll.png`；v2
  `examples/ragdoll_v2_snapshot.rs` → `docs/ragdoll_v2.png`（左 = 台阶跌落
  腾空帧、右 = 摊倒帧，headless 断言关节 / 穿地 / 悬空）。
- **纯函数与确定性**：检测（`detect_pair_rotated`：球-球 / 球-AABB 含盒内
  最近面退出 / AABB-AABB 最小穿透轴 / **胶囊三对**——capsule-球用轴段最近点
  （解析投影 clamp）退化到球-球、capsule-AABB 用轴段-盒**交替投影 4 轮
  收敛**（迭代近似，竖直段 × 轴对齐盒 2 轮内收敛，盒内照球-盒最浅轴
  退出）、capsule-capsule 用段-段钳制交替解法 4 轮）**旋转感知零数学
  改动**：三个 capsule 分支的算法入参从来只是「段的两个端点」，把竖直
  端点换成 `rotation·Y` 旋转后的端点即得旋转感知（斜段 × 盒 / 斜段 ×
  斜段不可分解，4 轮是确定性裕度，残差单测锁定 < 1e-3）
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
  命中即此语义的 XZ 本地实现）。**R50 旋转感知变体**
  `ray_capsule_rotated`（局部系变换 + 复用竖直解析解）/
  `ray_vs_shape_rotated` / `segment_hits_shape_rotated`：胶囊轴随实体旋转
  （恒等旋转走快路径，逐位不变）；世界级 `raycast` / `raycast_filtered`
  已带 `Transform.rotation`。世界级 `raycast` / `raycast_filtered`：
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
  `shape_sweep`（R31）同款算法供胶囊扫掠；**R50 旋转感知变体**
  `shape_sweep_rotated` / `resolve_static_push_rotated` 的静态障碍快照
  升级为 [`StaticObstacle`] 三元组（位置 + 旋转 + 形状）——世界封装
  （`resolve_against_statics` / `sphere_sweep_statics` / `move_and_slide` /
  foot_ik 落地射线）已全部走旋转感知路径（倒地的斜置胶囊尸体照实阻挡 /
  承托）。检出范围的诚实边界（起点与终点都自由的完全贯穿返回 `None`，
  调用方按需分步）写在函数文档。
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
  阶、全贯穿不检出（**C3 起有 `move_and_slide_ccd` 子步包装闭合**——
  原入口语义不变）。
- **v1 限制（诚实清单）**：
  1. **AABB 语义固化 + OBB 独立变体（C3d 专章 P1 过审交付）**：
     `ColliderShape::Box` 是世界轴对齐包围盒，`rotation` 对它无语义
     （v1 固化，既有场景逐位不变）；**旋转的盒用
     `ColliderShape::OrientedBox`**——obb×obb **SAT 15 轴闭式**（无迭代、
     无超越函数，OBB 内核算子看门单测锁死；相切不算接触、并列分离轴
     取固定枚举序首位、准平行叉积轴跳过分立），obb×{Sphere, Capsule,
     Box} 经局部系刚体变换复用既有闭式；射线局部系 slab、broadphase
     包围 AABB、v2 惯性随体旋转；恒等旋转与 Box 数值等价（±1e-5）但
     非逐位路径（opt-in 变体）。任意凸包（GJK/EPA，专章 P2）**维持
     缓行**待真实消费方；`Capsule`（R50）仍是第二旋转感知形状；
  2. **物理体须为根实体**：`step_physics` 把 `Transform.position` /
     `rotation` 当世界坐标读写，与层级并用时须自行保证物理体无父级
     （或父级变换恒等）；
  3. **非迭代完整求解器**：等质量冲量 + 2 次位置松弛，重堆叠会松弛不
     彻底（残余穿透 / 缓慢滑移），不是完整 LCP 求解器；
  4. **粗检测为 AABB 网格快照**：均匀网格按「积分后、求解前」的 AABB 一次
     构建，求解期位置 / 旋转修正（穿透量级）不重建——常规场景修正量远小于
     cell 尺寸、候选为保守超集（LCG 生成 ~200 AABB 的单测守护）；极端穿透
     （接近 cell 尺寸）理论上可能漏对；
  5. **胶囊最近点为迭代近似**：capsule-AABB / capsule-capsule 的最近点
     用交替投影 4 轮收敛（竖直 / 平行轴配对下 2 轮内即精确；**斜段 × 盒 /
     斜段 × 斜段不可分解**，4 轮是确定性裕度，残差 < 1e-3 由单测锁定），
     非解析解但同输入恒同结果；角色控制器的台阶前跳（不可站立阻挡时前向
     ≥ radius）带来单帧至多一个 radius 的水平速度红利；
  6. **接触不产生角冲量（R50）**：碰撞只改位置与线速度——落地不会自动
     「滚起来」、偏心撞击不会让物体打转；角动量只由 ω 初值 + 角阻尼 +
     约束（`SwingJoint` 方向修正）演化。ragdoll 的翻滚因此是「注入的 ω +
     关节方向修正 + 接触推离」的组合，不是真实转动动力学（需要惯性张量
     与接触点臂，留后续轮）；
  7. ~~**无 hinge / twist 两自由度铰链（R50）**~~ **C3 闭合**：
     `HingeJoint`（双轴最小弧对齐把摆动自由度全闭合 + 绕铰链线的扭转角
     限位与速率限制马达）提供「只能单向弯」的合法表达；`SwingJoint`
     维持锥近似原语义（限位是**锥**、层级投影、链须根先叶后——ragdoll
     既有接线零改动，两关节并存、各自 opt-in）；
  8. **ragdoll 侧躺仍可能埋臂（R50）**：单胶囊躯干的肩窝在轴外 0.25 m
     （> 躯干半径 0.18），躯干**侧躺**时靠地一侧肩窝低于地面 ⇒ 被钉在肩窝
     上的手臂转不出来（见第 6 条）、埋进土里 ≤ 肩偏 − 躯干半径 + 臂半径。
     释放时的翻倒转轴已绕链根肩线（前扑 / 后仰）规避主路径；根治需多胶囊
     / OBB 躯干（角冲量已由 R76 v2 侧向缓解；OBB 躯干随第 1 条专章门禁）。

**C3 补记（闭合战役·物理深化役，2026-09-18）**：三件全 opt-in、缺席 =
逐位不变——① **CCD**：刚体 `Ccd` 组件（积分位移按「本体最小投影宽度 /
显式段长」拆等分子步，首个嵌入终点停步交还本帧接触管线；纯函数
`ccd_substepped_displacement` 解析可测；只对照静态 / 运动学障碍、子步数
封顶 64）+ 角色侧 `move_and_slide_ccd` 子步包装（闭合其「全贯穿不检出」
诚实边界；v1 对照测试留在场证明原位）；② **铰链关节** `HingeJoint`
（三段：锚点距离 → 双轴对齐 → 绕铰链线的扭转限位 + 速率限制马达刚体
旋转；位置级伺服、无扭矩 / 惯量语义，旋转修正绝对覆盖不读 ω）；
③ **可断关节** `JointBreak` + `BrokenJoints` 队列 + `prune_broken_joints`
系统（`PhysicsPlugin` 注册）：判据 = 每帧位置修正累计量（米），step 只
入队、prune 移除组件并发 `JointBroken` 事件——断裂作用于实体（该实体
全部关节一起清）。诚实清单增补进 §8.59。
**C3d 追补（同役续，专章 P1 过审拆段）**：④ **OBB**
`ColliderShape::OrientedBox`——obb×obb SAT 15 轴闭式 + 其余组合局部系
变换复用（全闭式无迭代、算子看门 / 固定种子 200 池 / 边界归属 / step
集成四件套验收，见 `docs/private/gjk-determinism.md` 与 §8.60；凸包
P2 维持缓行）。

**地形集成（R51，零扩展）**：heightmap 地形的碰撞拼块（见 §4.23）就是
一批普通 `Collider::static_box` 静态体——接触检测、射线 / 扫掠查询、
角色控制器与导航全部按既有静态体口径直接消费，物理层**未为此新增任何
API**；拼块顶面高差 = 台阶语义（≤ `step_height` 可迈，超出即墙），
默认丘陵生成参数（`amplitude × 0.112` 的单 cell 抬升上界）保证可行走。

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
- **地形衔接（R51）**：`terrain_statics` 把地形碰撞拼块喂成
  `NavGrid::build` 的 `(位置, 形状)` 静态体列表——盒顶 = cell 最高角，
  表面解析直接吃拼块顶面，翻丘航点贴盒顶（单测 ±1e-3 锁定）。两条
  衔接域规则：nav `cell_size` 取地形 `cell_size × collider_stride` 的
  整数倍（每导航格完整落在一块拼块内，无亚格歧义）；nav 边界须
  **外扩于地形域**——雕刻 BFS 的种子是「未被任何盒覆盖的隐式地面格」，
  被拼块全覆盖的域没有种子会整图判不可走（实测教训，模块文档同步）。
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

### 3.11 行为树 / 黑板 / 感知（iris-core/behavior + iris-core/perception，R65）

敌人 AI 的游戏侧手写状态机在能力变多后必然爆炸（状态 × 转移的笛卡尔增长，
最初评估的第二个瓶颈）。R65 在 iris-core 落一套最小骨架——**结构是纯数据、
决策带记忆、通信只有黑板、感知是纯函数 + 事件**。语义规范全文 =
`iris-core::behavior` / `iris-core::perception` 模块文档（含诚实限制清单），
本节是架构级摘要。

- **树的表示（数据结构而非 trait 对象）**：游戏侧用 `NodeSpec`（嵌套、可读）
  写树，`BehaviorTree::new` **前序展开**成扁平 `Node` 数组（子节点只存
  `NodeId` 下标）——运行期零动态分发、遍历期零分配、节点 id 构造期定死
  （可序列化 = 结构即数据）；节点族八种：`Sequence` / `Selector` / `Parallel`
  （成功数 × 失败数双阈值，**失败优先**）/ `Inverter` / `Repeat` / `Cooldown`
  / `Condition` / `Action`。**装饰器只做三个**（拒绝清单挡掉全集）。叶子的
  实现由游戏侧按**具名动作 id** 分发（`ActionId` = `&«q»static str` 新类型 +
  `BehaviorContext` trait）——引擎不认识任何游戏动作，树里没有函数指针。
- **children 是 Vec，遍历序 = 声明序**（下标升序）；需要按键序迭代的黑板用
  `BTreeMap`（选型理由照 `broadphase.rs`）⇒「同输入两次 tick 逐位一致」由
  构造保证（单测：逐节点状态表 + 黑板摘要 dump 对照）。
- **tick 模型 = 带记忆**：`BehaviorState` 逐节点保存上帧状态；上帧 `Running`
  的节点下帧**从记忆处恢复**（复合节点记「当前子游标」、`Repeat` 记已完成
  次数、`Cooldown` 记剩余冷却、`Parallel` 记「本轮已终结子位图 + 本轮计数」），
  不从头重新评估。每帧每代理 tick 一次，`dt` 由调用方给（通常来自 `Time`）——
  引擎不为行为树提供内置系统，而是给系统体 `tick_behavior_agents`（Update
  阶段并行系统，迭代序 = 实体 id 升序）：动作分发的上下文由游戏侧按实体构造。
- **黑板 = 逐代理组件**：`Blackboard`（`BTreeMap<String, BlackValue>`，值五型
  Bool / Float / Entity / Vec3 / Text）。**感知结果 → 黑板 → 动作是唯一通道**
  （动作之间不走结构字段私运状态）；**不做跨代理共享黑板**——跨代理通信走
  事件（共享状态会让确定性论证与生命周期双双失控）。
- **感知 = 纯函数 + 组件 + 系统 + 事件**：`in_view_cone`（三维距离 + 水平
  朝向的半角点积——`forward · (to_xz / dist) ≥ fov_cos`）、`has_line_of_sight`
  （线段 × `StaticObstacle`，复用 physics 射线，旋转感知）、`can_see`（合成）、
  `within_hearing`（纯距离、**无视线**——隔墙听得见是有意取舍）；`Perception`
  组件把视距 / 半角余弦 / 听觉半径 / 眼位偏移**字段化**，`PerceptionTarget`
  标记可感知目标；`perception_system` 扫描感知源 → 目标列表 → 与上帧差分 →
  `Events<PerceptionEvent>`（Seen / Lost / Heard，双代缓冲先例见 §3.10）+ 运行态
  写回。三份收集列表（静态体 / 感知源 / 目标）都按**实体 id 升序**处理 ⇒
  输出与 spawn 顺序无关（单测：乱序 spawn 的两个世界归一后逐位一致）。
- **生命周期与清理**：树运行态与黑板都在逐代理 `BehaviorAgent` 组件里——实体
  despawn 时三者一起消亡（组件生命周期天然如此），无手工清理、无全局注册表。
- **确定性论证**：决策路径上无哈希容器、无随机源、无时钟读取（时间只从 `dt`
  进）；动作按声明序串行 tick（`Parallel` 是**语义**并行而非线程并行）；
  浮点只在 `Cooldown` 里做「减法 + `max(0.0)`」。
- **实战验收（iris-infiltration 守卫迁移）**：守卫驱动改写为行为树——顶层
  Selector「击晕冻结 / 感知 + 决策」，决策 Selector「逃跑 → 追逐 → 支援 →
  巡逻」（前两个演示能力默认关）。旧状态机四条分支**逐位等价**：迁移前后两个
  场景（自动玩家全流程 2993 帧、发现 → 追逐 → 放弃 → 击晕 900 帧）的全状态
  FNV 轨迹哈希逐位相同。两个演示能力（①逃跑：低血量 / 被逼近 → 远离玩家的
  路点；②呼叫增援：发现玩家 → `GuardAlert` → 半径内守卫支援）是「树相对
  状态机的扩展力」载荷：支援态**不进** `GuardState` 枚举，由黑板承载
  （`guard.support_timer` / `guard.alert_pos`），轨迹与状态在
  `games/iris-infiltration/tests/guard_behavior.rs` 里以精确断言钉住。

### 3.12 昼夜循环与全局风场（iris-core/daynight + iris-core/wind，R80）

环境模拟的两个**纯函数资源**——零渲染依赖、确定性（无 rand / 无全局
状态，同输入 to_bits 逐位一致），语义规范全文 = 模块文档（含诚实限制
清单），本节是架构级摘要。

- **昼夜（`DayNightCycle`）**：唯一状态是归一化相位 `phase ∈ [0, 1)`
  （0 = 日出、0.25 = 正午、0.5 = 日落、0.75 = 午夜），`advance(dt)` 按
  `cycles_per_second` 推进 + `rem_euclid` 回绕（f64，**纯相位函数无
  累积漂移**——固定步长推荐）。太阳方向 / 色温 / 强度 / 环境光全是
  相位的解析纯函数：太阳弧 = 倾斜平面上的圆周（`sun_direction`，默认
  倾角 35°——正午仰角 55° 不在头顶，日出日落点钉在地平线）；光照包络
  = 正弦仰角驱动的分段线性曲线（`sun_envelope` 地平暖橙 → 正午白、
  夜间强度归零；`ambient_envelope` 昼间微冷白 ↔ 夜间下限底光
  `(0.035, 0.045, 0.07)`——**午夜不黑屏的保底**）；`is_night` = 太阳
  严格低于地平线。
- **接线（iris-engine `DayNightPlugin`，已含于 `DefaultPlugins`）**：
  Update 阶段并行系统把四个纯函数值写进 `DirectionalLight` 的
  direction（= −sun，光传播方向换算）/ color / intensity / ambient；
  `shadows` / `shadow_range` 不动；`SkySettings` / `FogSettings` **不
  自动联动**（§8.49）。**资源缺席语义**：`DayNightCycle` 缺席 = 零
  操作（方向光逐位不被触碰）；`DirectionalLight` 缺席 = 相位照常推进
  （模拟事实与渲染资产解耦）；`Time` 缺席 = dt 0（与粒子系统同款兜底）。
- **风场（`Wind`）——接口轮**：R82 粒子 v2 / R90–R91 毛发 / R92 降水
  / 闭合池布料与植被四条下游线的**共用采样接口**；v1 引擎内无消费者
  （诚实声明在案，非摆设 API——路线图立项的接口形状定稿）。
  `wind_sample(wind, pos, t)` = 恒风（水平单位风向 × strength）+
  阵风调制（`1 + gustiness × g`，g 为 (x, z, t) 格点三线性值噪声
  ∈ [−1, 1]，格点值来自 splitmix64 哈希流——`particles.rs::mix64`
  范式；量化步长 2 m / 0.25 s，五次平滑插值 C2 连续）。
  **v1 边界**：阵风只调幅值不旋风向、高度无关、单八度无湍流细节。
- **快照**：`examples/daynight_snapshot.rs` 四相位 2×2（日出 0.04 /
  正午 0.25 / 黄昏 0.46 / 午夜 0.75）→ `docs/daynight.png`——正午
  地面亮度 ≫ 午夜（阈值 40）、黄昏太阳侧 R>B 可判别、午夜不黑屏、
  FNV 双会话逐字节一致。

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
- **预积分皮肤 SSS（R63）**：`MaterialDef` 新增 `subsurface_strength`
  （0..1 混合，缺省 0 = 关闭）与 `subsurface_scale`（曲率尺度半径，米，
  缺省 0.1）——肤质材质的**直射漫反射**按 (NdotL × 曲率) 查程序化预积分
  LUT 的包裹值，光软化终结线并渗入背光侧；**镜面 / IBL / 光照贴图间接项 /
  自发光一概不动**，金属经 `kd = (1 − F)(1 − metallic)` 权重自动归零，
  SSS 补项与主体项消费同一个已乘阴影的辐亮度（不晒穿投影阴影）。
  **缺省 0 = 着色器整段分支不进入 = 与 R62b 逐位一致**。完整设计（积分式 /
  归一化选型 / LUT 精度依据 / 调参表 / 拒绝清单）见 §4.32。
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
| `culled_instances` | 被视锥剔除的实例数（普通 + 蒙皮 + 透明 + 贴花合计；`Visibility` 过滤——自身 `HIDDEN` 与祖先级联——不算剔除）；**开启遮挡剔除（R53）时另加本帧被遮挡剔除数**（单列口径见下行）。**粒子不进本口径**——剔除按发射器包围球整批裁决，被剔的发射器既不进 `instances` 也不进本字段（§4.17） |
| `occlusion_culled` | 本帧被遮挡剔除的实例数（R53，`OcclusionCulling` 开启且有上一帧新鲜查询结果时的主 pass 跳过数，同时计入 `culled_instances`；关闭恒 0）。被剔实例只退出主 pass——阴影 / 深度副本保留（保守性，§4.25） |
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
  （data URI 保证 GLB 自包含、内存加载路径可用；bufferView 引用图片
  形态亦受支持——补全于 R59 后的**外部模型导入支线**，见 §8.24 修订）；
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
  CUBICSPLAT 插值、非三角形拓扑、非 FLOAT 顶点分量、bufferView 引用
  图片。——其中前三条与「非 FLOAT 顶点分量」在后续演进中已部分放开
  （蒙皮 R38 起、u8 关节索引与 bufferView 图片补全于 R59 后外部模型
  导入支线，见 §8.24 修订与 REPORT 补遗）。
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
  states 非空时 `advanced` 的循环/速度以当前状态为准。
- **状态机条件转移 v2（C4b）**：~~「条件判定留给游戏侧」~~ 参数黑板
  `params`（`set_float` / `set_bool`，`BTreeMap` 固定序）+ 条件转移表
  `transitions`（`AnimTransition { from, to, duration, guard }`）+
  `step_state_machine()`（`update_animations` 写回阶段每帧自动步检）。
  守卫 `AnimGuard` 是**封闭枚举**（浮点阈值 `FloatAtLeast`/`FloatBelow`/
  `FloatBetween` 闭区间、`BoolIs`、播放器侧 `ClipFinished`、组合 `All`/
  `Any`/`Not`）——可序列化、可 PartialEq、求值确定（不做闭包，确定性
  文化）。触发纪律：按**声明序**取首条 `from == current_state` 且守卫
  成立的转移、**一帧至多一条**（防乒乓/级联）、自环（from==to）不触发、
  越界目标 = 无条件断言 panic。**`transitions` 空表 = 恒 no-op = v1
  行为逐位不变**（与 v1 `edges` 并存、共享 `enter_state` 落账口；
  六游戏自建状态判定自此有引擎承载）。
- **运行时重定向（C4b）**：`retarget_by_name(target_names,
  target_entities) -> RetargetReport`——通道不改写（节点下标保持源骨架
  空间），只把 `node_entities` 整体重绑到目标骨架实体：骨名经
  `normalize_bone_name`（小写 + 剥 `mixamorig`/`bip01`/`mocap`/
  `ue4-skel` 前缀 + 空格压 `_`）匹配，目标重名取先出现者。**不猜语义
  改名**（无编辑距离/内置别名表——猜错的绑定比断绑难排查，alias 表
  归调用方）：有名未命中断绑为 `None`（通道停写，合流既有 None 语义）、
  无名节点保留原绑定、空名字表恒 no-op。兑现 §8.35⑨ 拒绝「导入期 GLB
  重定向」时留的口子——这是**运行期播放器侧**重绑，替代 iris-exp 的
  离线 Blender 烘焙路线（同骨架命名族的跨角色复用剪辑库）。
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
  **诚实限制：负 `speed`（倒放）不触发事件**——`collect_crossings` 的
  数学以「时间正向推进」为前提（非循环分支区间恒空、循环分支 floor 差
  同样无命中），战斗玩法无倒放需求，v1 不支持。
- **帧数据窗口 + 参数曲线轨（R74）**：事件是离散点，战斗判定还需要
  **区间**与**连续值**两类时间语义，本轮补齐：
  - **窗口** `AnimationClip.windows: Vec<ClipWindow { name, start, end }>`
    ——「攻击判定开 / 可取消 / 无敌」等区间载体，判定纯函数
    `window_active(windows, time, name)`：半开区间 `[start, end)`（start
    含、end 不含——**事件帧 = 窗口首帧**，与「事件在 start 触发」同帧
    衔接）、`start >= end` 畸形窗口恒 false、无同名窗口 = false（未声明
    即不 active，组件缺席语义）。播放器便捷入口
    `AnimationPlayer::window_active(name)`（active 剪辑 × 当前时间）。
  - **参数曲线** `AnimationClip.curves: Vec<ParameterTrack { name, keys }>`
    ——命名标量关键帧轨（伤害随命中帧衰减、镜头 FOV 等游戏侧数值的
    时间演化），采样纯函数 `sample_track`（线性插值、端点钳制、并列
    时间取输入先者——构造 `ParameterTrack::new` 稳定排序、扫描首遇即
    返回 ⇒ 并列行为确定；非有限 t 返回 0.0）。`update_animations` 每帧
    推进后对 active 剪辑全部曲线重采样，**整表覆写**
    `player.parameter_values`（区间状态语义，非消息流、无 drain），查询
    `player.parameter(name)`（同名多轨取先声明者）。
  - 资产侧与事件同款：GLB 解析不产窗口 / 曲线（不在 glTF 标准内），
    由 API 侧构造或经 `clips_mut()`（`Arc::make_mut`，共享时复制——
    setup 期一次性注入）写入。R67 帧数据战斗的直接前置。
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
- **链式 IK v2（R75，`chain_ik` 模块）**：与两骨解析解**并存不替换**
  ——`ChainIk` 补三骨以上软链（触手 / 尾巴 / 脊柱），`LookIk` 补瞄准
  （头 / 炮塔 / 脊柱跟随视线）。**FABRIK**（纯函数 `solve_fabrik`）：
  前后向交替迭代，链首节点是**锚**（位置永不动，与足部 IK 的髋同款）
  ；确定性契约 = 固定迭代上限 + 数据驱动早退（末端距目标 ≤ 容差即停
  ）；不可达 = 沿锚→目标按累计骨长**摆直**（软链期望行为，刻意不做
  手部式放弃门）；退化方向取 ±Y 兜底。回写同款最小旋转弧 + 局部旋转
  `normalize()`；**末端节点局部旋转与位置都不写**——FABRIK 骨长取
  当前世界姿势，若照 foot_ik 模式回写末端局部位置，写坏的偏移长会
  喂进下一帧骨长测量形成**无回归力的吸收态漂移**（实现期探针实测
  600 帧内 0.25 → 0.251 破容差）；不写则局部偏移恒定、骨长结构性
  严格保持，末端落点与解算位差在浮点噪声级（≪ 容差 1e-4，与两骨解
  「精确到位」的刻意差异）。**look-at**（`update_look_ik`）：末端节点
  瞄准轴（局部系，默认 −Z）指向目标，**单遍闭式分布**——总摆动 S =
  瞄准方向 → 目标方向最小旋转弧，每节点施加份额 `S^(weight/n)`
  （轴角幂 `swing_fraction`；同 q 世界系施加于各节点子树，末端复合
  n 份恰得 S^w，权重 1 时瞄准精确）；诚实边界：S 按修正前端末位置
  计算、施加中末端移动 ⇒ **近目标残差** ∝（末端位移 × sin 摆角）/
  目标距离（米级目标 < 0.5°，不累积——每帧从当前方向全量重摆）。
  公共约定：`node_entities` 连续段 + `ChildOf` 直连链契约、`IkRig` /
  播放器二选一映射、`IkTarget` 目标语义（缺席 / 失效 / 隐藏放弃、
  一帧滞后、挂点勿在链子树内）、链上 scale 恒 1、幂等门（已在容差 /
  ~0.08° 内不写——静止目标 bit 级稳定）、**无关节角限制（v1）**。
  注册序 `update_animations → foot → hand → chain → look`（look 收尾
  ：瞄准是姿态最后一笔，读到含全部前序修正的位形）；均并行系统。
  不挂组件的世界零行为变化。

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

### 4.6.3 形变目标 v1（CPU 混合 × 逐实体动态 VBO，iris-render/morph，C4a）

闭合战役 C4 第一项（§8.9 / §8.24 的「形变目标在路线图」条翻转，v1 口径
见 §8.61）：

- **数据模型**：`MeshData.morph: Option<MorphData>` 是网格侧表——每个
  `MorphTarget` 持绑定姿势空间的每顶点位置增量（必选）+ 法线增量
  （可选）+ 可选名字；`vertices` 恒为绑定姿势，混合结果是渲染期临时
  产物不回写。`MorphWeights` 组件逐实体挂权重（与目标表按 `min(len)`
  配对）。缺席语义：组件缺席 = 绑定姿势 = 现路径**逐位不变**。
- **混合纯函数** `blend_morph_vertices`：`p + Σ wᵢ·Δpᵢ`（目标表固定序
  累加）；法线有增量参与时相加后**重归一化**（glTF 标准口径），除零
  守卫逐顶点。`w == 0` 的目标整体跳过——IEEE 中 `-0.0 + 0.0 = +0.0`
  会翻转符号位，跳过才是逐位透传（与 `skin_vertex` 零权重跳过同法，
  单测钉住）。
- **执行路径**：CPU 混合 + 逐实体动态顶点缓冲（`COPY_DST | VERTEX`，
  按蒙皮实体位标识缓存，与 `GpuSkin` 同构），蒙皮绘制分支（主 / 透明 /
  方向与点光阴影 / 场景深度 / RT / 探针捕获 / 水面共 11 个绑定点）的
  slot 0 经 `SkinnedItem::vertex_buf` 换绑。**零着色器改动、零管线布局
  改动**。脏检：权重位图案与上次上传一致 ⇒ 不重混合不重上传，
  `morph_uploads` 观测计数钉住该口径。
- **加载器**：`primitives[].targets`（属性名→增量 accessor 映射，走
  `read_floats` 同一扇门：稠密 FLOAT VEC3 之外沿用全池 `Unsupported`）
  + `mesh.extras.targetNames` 按序入名；规范允许的属性子集合法（缺
  POSITION 补零表、纯法线形变可表达），两通道皆缺 ⇒ `Unsupported`。
- **逐位锚**：像素级「零权重 = 组件缺席逐字节一致」+「同权重连续帧
  零重上传」+ 位移/阴影一致性，见 `tests/morph_render.rs`。

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
  bias 自 R61 起为**坡度缩放 + 常数下限**（见 §4.7.2「阴影偏移 v2」，
  R29–R60 为常数 0.0015），阴影暗部保留 45% 光照（`shadow_params.z`）。
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

### 4.7.1 点光阴影（cubemap 数组，预算化多盏，R61）

- **范围（R61 起为预算化多盏）**：最多 `POINT_SHADOW_BUDGET = 4` 盏
  `PointLight.shadows == true` 的点光**同时**投射 cubemap 阴影，各自
  6 个捕获 pass + 一张 512² 面；超出预算的灯按确定性选择键裁掉并
  `log::warn`（不是静默丢弃）。R61 之前（v1）只允许查询顺序中的**首盏**
  生效，其余开启者被 warn 忽略。
- **选择键（确定性）**：`(相机距离升序, 实体位模式升序)`
  （`light::select_point_shadow_casters`，纯函数）——距离近的灯其阴影更
  可能落在画面近处（cubemap 的角度分辨率与距离无关，屏幕空间重要性却
  随距离迅速下降），并列时以 `Entity::to_bits()` 收尾使比较器成为**全序**：
  结果只取决于灯集合本身，**与查询 / 插入顺序无关**（乱序插入的两次
  世界选中同一批灯，单测与 GPU 测试双重守护）。选中者按选择键序占据
  cubemap 数组槽位 0..n。只有一盏开启时选中结果与 v1 的「查询序首盏」
  一致（只有一盏，任一确定性键都选中它）。
- **uniform 布局（R61 调整，渲染内部契约）**：`PointLight` 从 2×vec4
  扩到 3×vec4（新增 `shadow = (是否投影, cubemap 槽位, −, −)`），
  `Globals` 512B → 640B，逐灯自带阴影开关与槽位——着色器不再假设
  「i == 0 才有阴影」，也不再需要收集期把投影灯交换到槽位 0。
  `point_shadow_params` 语义同步调整为（是否有投影灯, bias 常数下限,
  坡度系数, 法线偏移系数），range 从「投影灯 range」改为**逐灯**
  `color.w`（多盏各自的 range 不同）。**并行布局陷阱**：`skybox.wgsl`
  为了让天空盒只读 `inv_view_proj` 而自持一份 `Globals` 前缀声明，
  `point_lights` 的长度必须同步（8 × 3 = 24 个 vec4）——R61 实测漏改
  一次，症状是天空整体错色（读到的 `inv_view_proj` 落在别的字段上），
  现由 `pipeline.rs::skybox_globals_prefix_matches_standard_layout` 看门。
- **归一化距离存储**：cubemap 的 `R32Float` 存
  `length(world_frag − light_pos) / range` 而非光空间 `z/w`——
  采样侧因此**无需任何光空间矩阵**：`normalize(world_pos − light_pos)` 直接
  `textureSampleLevel(shadow_cube, ..., dir, layer, 0.0)` 查 texel，比较
  `stored >= dist/range − bias/range`（bias 以米存 uniform，比较时归一化）。
  捕获侧由片段着色器从插值世界坐标现算距离（距离函数非线性，顶点插值会在
  大三角形上产生米级误差）；清空值 1.0（= range/range）代表无遮挡。
- **偏移 v2 的点光适配（R61）**：与方向光同两技术、同口径——
  `bias_m = clamp(BIAS_MIN + SLOPE · texel_world(d) · tanθ, BIAS_MIN, MAX)`
  其中 `texel_world(d) = d · 2/512`（90° 面摊在 512 纹素上，`d` = 片元到
  灯的距离），`tanθ = tan(acos(NdotL))`（clamp 到 64）；采样点另沿**几何
  法线**推移 `NORMAL · texel_world(d) · (1 − NdotL)`（上限 0.15 m）。
  常数下限从 v1 的 0.03 m 降到 0.005 m（掠射角由坡度项承担）。点光是
  **单采样**（无 PCF），纹素跨度内的深度变化上限就是 1 纹素 · tanθ，
  故坡度系数取 1.0（方向光因 3×3 PCF 取 1.5）。**对照模式**
  （`Renderer::set_legacy_shadow_bias`）把两个系数置零并写回 0.03 m
  ⇒ 采样表达式与 v1 逐位等价（实测三个场景哈希相同）。
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
- **资源形态（R61）**：颜色与深度纹理都是 `512² × （预算 × 6）` 层的
  `texture_2d_array`，采样侧用 `CubeArray` 视图（层 = 槽位 × 6 + 面序），
  渲染侧每（槽位, 面）一个 D2 视图。**显存**：单盏 6 MiB 颜色 +
  6 MiB 深度 = 12 MiB；预算 4 盏 = 48 MiB（净增 +36 MiB，对照方向光 CSM
  的 96 MiB 固定开销，点光预算控制在 CSM 的一半）。预算做成**常量而非
  资源**：纹理数组层数在 `Renderer::new` 固定，运行时调整需重建纹理与
  24 个面视图 / 绑定组，收益远小于复杂度。
- **每（槽位, 面）独立 uniform**：每面参数（64B 面矩阵 + 16B 灯位置/range
  = 80B）放**独立小 uniform 缓冲**而非一份缓冲的多个插槽——绑定偏移必须
  满足 min_uniform_buffer_offset_alignment（典型 256），且同一 submit 内
  对同一缓冲的多次 write_buffer 会在执行前全部合并（泛光参数教训，
  §4.11）。全部 write_buffer 后随本帧统一 submit。
- **uniform 控制流掩码**：采样在 `dist < range` 受光分支之外无条件执行
  （逐灯开关与槽位都由 uniform 派生，分支均匀；`textureSampleLevel` 带
  显式 mip 不依赖导数），最后把阴影系数乘进该灯的 radiance——与
  `shadow_factor` 同款模式；超出 range 的片段不进受光分支，掩码自然为受光。
- **捕获时的粗剔除**：实例包围球到灯的距离减去半径仍超出 range 的，六个
  90° 面的并集（以灯为中心的完整球域）必不覆盖它，六面全部跳过；R61 起
  **逐槽位**各自计算（各灯的 range / 位置不同），布局 = 槽位主序的一维
  bool 表。剔除只影响阴影 pass 的绘制区间（可见性变化处拆分 run），
  不影响主 pass 合批。
- 每盏投影灯只调制**自己**的辐射度；不影响环境光 / 方向光 / 其余点光 /
  自发光。无阴影点光走与 v1 完全相同的路径（逐位一致，见
  `tests/shadow_v2_render.rs` 的「无阴影点光零变化」对照组）。
- **v1 不做（→ §8.6）**：点光阴影仍为单采样硬边（无 PCF / PCSS）；灯数量
  上限仍是 8（`MAX_POINT_LIGHTS`），预算选择只在进入 uniform 的前 8 盏内
  进行；逐灯阴影图集化 / 软阴影在路线图。

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
  窗口内；偏移口径见下「阴影偏移 v2」。
- **阴影偏移 v2（坡度缩放 bias + 法线偏移，R61；闭合 §8.8）**：
  常数 bias（v1 = 0.0015 NDC）必须按最坏情况（掠射角）取值，于是平地上
  也吃下同样大的世界偏移——peter-panning 与「接触阴影浮起」的根源，且
  掠射角仍会漏 acne（实测：无遮挡物的掠射角平面在 0.0015 下 12883 个
  acne 像素）。v2 把偏移拆成两项，**逐级联按世界尺度缩放**：
  ```text
  texel_world   = max(对齐后 AABB 的 XY 跨度) / 2048      （米/纹素）
  depth_window  = far − near                              （米）
  k             = texel_world / depth_window              （每单位 tanθ 的 NDC 深度误差）
  bias          = clamp(BIAS_MIN + SLOPE_SCALE · k · tanθ, BIAS_MIN, BIAS_MAX)
  法线偏移       = min(NORMAL_SCALE · texel_world · (1 − NdotL), NORMAL_MAX)
  采样点         = world_pos + 几何法线 · 法线偏移
  ```
  **推导**：接收面相对光源平面倾斜 θ 时，一个阴影图纹素跨度内的深度变化
  = `texel_world · tanθ`（`tanθ = tan(acos(NdotL))`，clamp 到 64 防发散）；
  最近邻比较下片段与该纹素的深度差就是这个量，3×3 PCF 的对角样本再乘
  √2 ⇒ 系数取下界 √2 与实测值之间的 `SLOPE_SCALE = 1.5`。`BIAS_MIN`
  保留常数项（**完全去掉常数会复发平地 acne**）但降到 0.0004（v1 的
  1/3.75）；`BIAS_MAX = 0.02` 限制掠射极端退化为无影。
  **法线偏移用几何法线**（不用法线贴图扰动后的着色法线：深度误差由光栅化
  几何决定，高频抖动的法线只会给偏移量注入噪声、在凹凸面漏光）。
  **逐级联缩放**：`texel_world` 与 `depth_window` 随级联增大（近 ≈ 16 mm /
  101 m，远级联更大），CPU 由 `cascade_fit_params` 逐级联算出并放进
  `CsmParams.cascade_texel / cascade_slope_k`（256B，+32B），着色器按片元
  选中的级联动态索引。
  **效果（同场景对照，逐位可复现）**：掠射角平面 acne 12883 px → **0**；
  0.07 m 薄板的接触阴影在常数 bias 下**整条消失**（开/关阴影两帧逐位
  相同），v2 保留 2591 px 且根部亮带 0 px（不浮起）。
  **对照模式**：`Renderer::set_legacy_shadow_bias(true)` 把两个系数置零、
  写回 v1 常数——采样路径与 R60 **逐位等价**（三个场景哈希实测相同），
  供验收图左右面板与归因测试同一二进制内对照。
- **级联参数独立缓冲**：主 pass 片元需要 3 × 矩阵 + 相机前向 + 分割点 +
  逐级联的两个世界尺度（256B `CsmParams`，globals 组 binding 8——排在
  现有最大 binding 7 之后，**不扩 Globals**）；阴影 pass 每级联只读一个
  64B 矩阵（一级联一缓冲，规避同 submit 多次 write_buffer 合并问题，
  同 §4.7.1 面参数教训）。
- **片元级联选择**：`view_z = dot(world_pos − eye, cam_forward)`（**view
  深度**而非欧氏距离——欧氏距离在视锥边缘把近处片元误分到远级联；也
  不用 clip z——透视非线性，分割点得先换算）。`view_z < splits[i]` 即第
  i 级；动态索引进 uniform 数组取该级联矩阵，`textureSample` 带
  `i32(cascade)` 数组层。选择/越界均为非均匀分支，采样照常执行、掩码
  事后混向「受光」（§4.7 的 uniform 控制流模式不变）。
- **实例区间沿用主 pass**：三个级联 pass 复用主 pass 视锥剔除后的不透明
  实例区间全部重画（透明项与粒子不投影，语义不变）——保守不漏影；
  按级联视锥再剔除是后续优化点。`FrameStats` 口径不变（阴影 pass 不计，
  投影灯数单列在 `point_shadow_casters`）。
- **测试**：`cascade_splits` 单调性/钳制/λ 混合生效、`snap_aabb_to_texels`
  对齐后跨度为 texel 整数倍/只扩不缩/幂等、`cascade_fit` 含住 8 角点/
  光空间深度单调/亚纹素平移 UV 漂移 ≤ 2 texel（相位稳定性的数学形态）、
  `cascade_fit_params` 与 `cascade_fit` 矩阵逐位相同 + 逐级联世界尺度
  单调（R61）；GPU 侧 `csm_render.rs` 大场景（~80 m）断言近/远级联物体
  脚下都有阴影、级联边界两侧阴影带连续（无接缝），
  `shadow_v2_snapshot` 另以「跨接缝剖线最小暗化 > 5/255、相邻采样跳变
  < 1/255」量化验收图 ④。

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
  → TAA（可选，R60，链首，全分辨率）：投影抖动色 + 深度副本 + 历史
      → 3×3 邻域钳制混合 → 新 HDR（详见 §4.29）
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
iris-render 的 glTF 管线依赖树内，Cargo.lock 无新版本号）。**R57 起本格式
升为 v2 = 关卡档**（顶层 `version` + `level` 段，v1 档无损直读）——v2 新增
面（关卡段 / 材质三纹理槽 / 粒子段 / 同源去重 / 版本迁移）全文见 §4.26，
本节保留 v1 基底语义的权威描述。

**格式设计（全蛇形命名）**：两类内容——

- **环境段**（场景级资源形态，非实体）：`directional_light`（方向 / 颜色 /
  强度 / 环境光 / 阴影开关，各字段可选、缺省 = 引擎默认；`shadow_range` v1
  不序列化，v2 补齐为缺省省略的可选字段——§4.26）与 `sky_source`（Radiance
  `.hdr` 相对路径 → `SkySource::equirect_hdr`，
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
场景文件；克隆 / 预设 / 组件插件扩展点；`RigidBody.velocity` 不序列化
（物体从静止开始）；`DirectionalLight.shadow_range` v1 不序列化（v2 补齐，
§4.26）；同一 glTF 文件
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

- **无压缩声明门（R66e，`TextureStore::declare_uncompressed`）**：决策树
  的**前置否决**——声明句柄跳过 BC1/BC3 直走 RGBA8 + mip 链。动机：BC1
  的通道耦合端点插值在**法线图**上产生近距块状伪梯度（iris-exp
  frictions ⑨：皮肤近距块斑；同图 Blender 无压缩渲染干净、摘法线离屏对照
  干净 ⇒ 压缩为病灶）。**glTF 加载器为法线 / ORM 自动声明**（外部资产
  质量优先）；引擎原生资产不声明 = 决策树逐位不变（内存优先，既有 FNV
  基线零漂移）。显存估算两口径同步行（`texture_gpu_bytes` 前置分支 +
  `gpu_bytes_estimate_mipped` 谓词同步，声明者按 RGBA8 链增量计）。

- **无分支回退**：wasm 的 WebGPU 实现普遍无 BC 特性、native 老 GPU 同样
  可能没有——不写任何 cfg，运行时特性检测天然覆盖（web 自动 RGBA8），
  与「wasm 只需编译可用 + 优雅降级」的平台策略一致。
- **编码器**（`texture_bcn::encode_bc1`，纯函数、全整数运算、零 unsafe）：
  逐 4×4 块取 RGB 距离最远的两像素为端点（O(16²) 确定性暴力，与
  PCA / 范围法的取舍见模块文档——质量略差但逐位可复现），**按 RGB565
  打包整数值排序 c0 > c1**（R66f：BC1 解码器以打包值比较选四色/三色
  模式，旧版按亮度排序在高饱和暖色上出反序块 ⇒ 误判三色模式、index 3
  解出纯黑，iris-exp 脸部黑斑病灶），RGB565 量化（四舍五入 + 位复制
  展开），像素索引取四色调色板（两 565 端点 + 两个 1/3 插值，插值
  **向下取整**与 BC1 规格整数除法口径一致——R66f 同轮修正旧版 +1
  四舍五入的索引错档）中最近项、并列取最小索引。**确定性
  契约**：同输入两次编码逐位一致（单测守护）——回放 / 差分测试的硬
  要求。纯色块往返零误差（端点同色 → 调色板退化、索引无歧义）；位打包
  布局有手工构造块的逐字节断言；高饱和暖色块「恒四色模式、解不出纯黑」
  有回归锁（`bc1_saturated_warm_block_never_decodes_black`）。尺寸非 4
  倍数统一**预 pad**（边缘像素复制）写死——「向下取整块数」丢像素、
  「末块按实际像素」产出非法 BC1 流，pad 是唯一能产出完整合法块阵列
  的方案。
- **质量护栏**（单测）：纯色 565 精确色黑/白/三原色/三补色往返误差 = 0；
  温和灰度渐变块平均误差 < 4/255；GPU 集成测试 `bc1_render.rs` 用
  「不透明副本 → BC1 路径、alpha=100 副本 → RGBA 回退」的同画面 A/B
  （不透明材质不消费纹理 alpha，两副本画面等价，像素对比即纯格式对比）
  断言全帧平均误差 < 2/255（块内纯色棋盘格解码精确，实测 0）。
- **mip 链 + 各向异性过滤（R66，`MipSettings` 资源，默认关）**：
  `texture.rs` 的 `MipSettings { anisotropy: u8 }`（Default = 8）是全库
  材质纹理采样策略总开关——**资源缺席 = 与 R66 之前逐位一致**（单 mip
  上传、`mip_level_count` 恒 1、采样器 Nearest mip 过滤 + clamp 1），
  与 TaaSettings / SsrSettings 的「缺席 = 关」范式同款。在场时：
  - **CPU 纯函数链**（`mip_chain_rgba8` / `downsample_rgba8` /
    `mip_level_count`，全整数运算跨平台位级确定）：2×2 box 降采样、
    逐通道 floor 均值、维数 `max(1, dim/2)` 终止于 1×1；**奇数维末行/
    列源像素不参与下一级**（floor 链语义），一维为 1 时边缘 clamp
    （边缘纹素权重加倍）。RGBA8 回退路径直接生效；**BC1 / BC3 压缩路径
    自 R66d 起在场时也生成链**（`mip_chain_bc`：逐级 RGBA8 降采样后编码
    成块流——R66 只做 RGBA8 链时全不透明数据贴图命中 BC1 恒单 mip，远距
    无法线平滑出点状噪点，iris-exp frictions ⑧），**缺席仍单 mip 逐位
    不变**（§8.43 已闭合为历史形态）；
  - **采样器**：`mipmap_filter: Linear`（三线性）+ `anisotropy_clamp`
    夹到 wgpu hal 边界 [1, 16]（适配器不支持各向异性时 wgpu-core 内部
    收敛为 1）；1×1 纹理链长恒 1，trilinear / 各向异性对常色纹理
    数学上不可见（`mip_render.rs` 帧级逐位断言）；
  - **生效时机 = 上传时**：GPU 纹理与采样器按句柄值缓存，运行中修改
    资源不追溯已上传纹理（`draw` 开头读取快照，本帧全部懒上传同口径）；
  - **动机**（games/iris-exp frictions ①）：外部 GLB 发片（黑底透明 +
    彩色发丝）远距重度缩小采样时高频 alpha 逐像素翻转成噪点；混合
    公式（ALPHA_BLENDING 直通 alpha）与 glTF 非预乘语义一致，不动。
  GPU 验收（`mip_render.rs`，文件内互斥）：512² / 4 纹素格棋盘俯视
  重度缩小（≈12 纹素/像素）ROI 亮度标准差**双向可判别**——关 mip
  实测 99.77（摩尔纹噪点）vs 开 0.00（box 链均灰），阈值 60 / 20
  独立断言；1×1 / 无纹理场景「缺席 = 在场」全帧 FNV 逐位一致（改动面
  未外溢旁证）；关帧另锁 R66 文件基线哈希；同输入双会话（独立世界 +
  独立渲染器）两态逐位一致。验收图 `docs/mip.png`（关/开对照）。

**内存占用统计**（「低占用」卖点的观测口径）：

- `TextureStore::memory_bytes()`：Σ 源 RGBA 字节；`MeshStore::memory_bytes()`：
  Σ（顶点 32 B + 索引 4 B + 蒙皮属性 32 B）——亦即 GPU 网格缓冲的准确
  上界估算（顶点/索引/蒙皮缓冲与 CPU 源同尺寸）。
- `TextureStore::gpu_bytes_estimate(feature_bc)`：GPU 侧显存占位**估算**
  （驱动可能对齐分配，非精确值；mipmap 无，单 mip 计），逐纹理按上传
  决策树计（BC1 0.5 B/px 或 RGBA 4 B/px）；`false`/`true` 两次调用之差
  即压缩节省。`Renderer::texture_is_bc1(handle)` 观测逐纹理实际上传
  格式（测试与内存对账用）。
- `TextureStore::gpu_bytes_estimate_mipped(feature_bc)`（R66）：mip 感知
  口径——RGBA8 路径纹理补算完整链增量（第 1 级起 Σ w_k·h_k·4，几何
  级数 ≈ 基级 +1/3），BC1 / BC3 路径与旧口径相同（单 mip）；假设
  `MipSettings` 在场。**旧方法签名与数值口径不动**（既有测试零改动）。

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
  **C1 v2 修订**：世界带 `LodSettings` 资源时主相机收集改「双阈值滞回 +
  可选包围球中心参照」（下述两条限制均闭合），资源**缺席 = v1 口径逐位
  不变**；`LodMesh3`（地形三级）不吃 v2 设置。
- **验收口径**：`FrameStats::triangles_drawn` 天然成为 LOD 指标——GPU
  集成测试 `lod_render.rs` 断言同一实体近/远两帧三角形精确等于选中
  网格的三角形数、两帧均可见且近帧像素更多（低模像素宽松证据）；
  v2 滞回带振荡五帧序列 + 偏心网格参照系判别用例同文件。
- **滞回（C1 闭合）**：`LodSettings::hysteresis = h` ⇒ 高→低阈值
  `s·(1+h)`、低→高阈值 `s·(1−h)`，带内维持原档（防边界抖动逐帧来回
  切换）；跨帧记忆（实体位 → 档位，`BTreeMap` 确定序）只由主相机收集
  更新——RT / 捕获上下文无记忆、按当帧单阈值判定；`h = 0` 时全部 prev
  形态逐位退化为 v1 `select`（兼容锚单测 + GPU 锚）。
- **R51 推广**：`LodMesh3`（三级 + 两级阈值，同收集期口径、同无滞回）
  服务 heightmap 地形的分块 LOD，见 §4.23。

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

**C2 受光增量（贴花 v2 直射项）**：世界带 `DecalLighting` 资源时逐实例
location 10 开关置 1，片元把输出乘上「方向光 Lambert + ambient」因子——
法线由逆世界盒矩阵重建（`(M⁻¹)ᵀ e_z` = 逆矩阵 z 行，端盖平面法线精确，
非均匀缩放仍正确），朝相机一侧翻正；CPU 镜像 `decal_lit_factor` 解析三档
单测，`decal.wgsl` 的 Globals 前缀（到 ambient 五字段）+ location 10 契约
由 pipeline.rs 源码层看门钉住。**缺席 = v1 逐位不变**（既有 7 项像素锁
原值通过实证布局 144→160B 无漂移）；中性光照（ambient≡1、intensity 0）
时开与关逐像素一致（乘 1.0 恒等锚）。阴影采样 / 点光 / IBL 维持声明边界。

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
  **R81 修订**：本上限只约束 `RenderTargetStore` 的**用户** RT 相机；
  引擎内部的捕获视口（反射探针六面、水体的反射 / 折射两张半分辨率图，
  §4.37）走独立资源路径、不占用户预算——用户可见能力（多视口编排）
  与引擎内部实现细节（捕获）是正交维度，见 §8.20。
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

### 4.23 Heightmap 地形（iris-render/terrain，R51）

写实路线「复杂地形」的正式解：**heightmap 分块渲染（3 级距离 LOD）+
碰撞自动拼块 + 导航 / 角色零扩展集成**。语义规范全文 =
`iris_render::terrain` 模块文档，本节只记架构位与关键口径。

**数据与生成**：`Heightmap{width, depth, cell_size, heights}` 行主序
顶点高度场；`generate_hills` 三组方向正弦叠加（波长 48/24/12 m、权重
1:0.5:0.25，方向 / 相位由 seed 经 splitmix64 派生——**无 RNG 无依赖**，
同 seed 逐位一致，`to_bits` 测试锁定）。查询：`height_at` 双线性
（域外 `None`）、`normal_at` 中心差分（边界单边）；`Terrain` 组件
（根实体）持高度场 + `origin` 平移 + chunk / 碰撞体清单，
`height_at_world` 供游戏贴地逻辑（投射物 / 放置物 / 音效触发）。

**分块与 LOD**：地形按 `chunk_cells`（默认 32 cell）切块，每块预烘焙
3 级网格入 `MeshStore`（LOD0 全分辨率 / LOD1 抽 ½ / LOD2 抽 ¼，顶点
与索引逐级递减——n=32 实测 1089/353/177 顶点、2048/576/224 三角形，
与解析推导一致）。chunk 实体挂 **`LodMesh3`**（`LodMesh` 的 3 级一般化，
`levels + switch_distances[2]`），渲染**收集期**按「chunk 原点到相机
eye 欧氏距离」选级（与 R30 同落点、同边界归近侧、同无滞回），选中句柄
决定合批键 / 剔除包围体 / `FrameStats::triangles_drawn`——LOD 生效的
统计证据即 triangles 近远对比。

**接缝不变量（无 skirt）**：每级 LOD 的 chunk 边界线恒全分辨率采样、
内部按步长抽行抽列 ⇒ 相邻 chunk 共享边折线顶点集一致；顶点 y 直接
烘焙世界高度（实体 y = 0，无平移舍入）、法线 / UV 由全局格点索引推导
⇒ **逐位一致**（cell 为 2 的幂时 x/z 同样逐位，否则 ≤ 1 ULP 亚毫米且
不涉裂缝的垂直轴——单测对 9 种 LOD 组合断言）。行带间用「拉链」
（zipper）条带三角化统一处理全采样边界行与抽行内部行；代价 = 粗级
chunk 边界内侧长条三角形 + T 型接点（折线共点 ⇒ 水密，仅掠射角轻微
法线插值差）。

**碰撞 = 粗 AABB 拼块（零 API 扩展）**：每 `collider_stride²` cell 一
个 `Collider::static_box`（顶面 = 块内最高角点、底 = 全场最低 − 0.25
余量），只有 Collider 无 RigidBody = 静态体（组件缺席语义）；无网格 /
材质句柄 ⇒ 天然不进渲染收集（无需 Visibility）。集成口径：角色控制器
（`move_and_slide` 世界静态体收集）、射线 / 扫掠查询、R49 导航图
（Box 表面解析吃盒顶）全部零改动可用。台阶语义 = 相邻块顶高差 ≤
`step_height`（0.3）可迈、超出即墙；单 cell 坡面抬升 ≤
`amplitude × 0.112`（生成器方向导数界）⇒ 默认丘陵参数开箱可行走。
**导航衔接域**：nav `cell_size` 取地形 `cell_size × stride` 整数倍，
且 nav 边界须外扩于地形域（R49 雕刻 BFS 种子 = 未被盒覆盖的隐式地面
格，全域覆盖则无种子——实测教训，单测锁定）。规模：拼块数 = cell 数 /
stride²，64×64 stride 1 = 4096 盒为典型上限（线性扫掠口径），更大场景
调 stride。

**材质**：`grass_textures` 程序化草地三件套（周期值噪声 → 基色 sRGB /
细节法线 / ORM，同输入逐位一致，口径照 `brick_normal_orm`）；UV =
世界 XZ ÷ `uv_tile` ⇒ 跨 chunk 平铺无缝（Repeat 寻址）。

**诚实限制（v1）**：碰撞是台阶不是光滑坡面（视觉连续、盒顶按 cell
阶梯）；无 skirt / 无滞回 / 无地形流式装卸 / 无坡度材质混合（单纹理）；
LOD 级数固定 3；`generate_hills` 域外查询 `None` 边界处理游戏侧自理。

**测试**（terrain.rs 13 项 + terrain_render.rs 3 项）：生成器逐位确定
性 / 值域 / 高差上界；双线性与差分法线解析对照（角 / 边 / 心 + 域外）；
n=8 精确拓扑计数（81/128、41/48、33/32 手推对照）与朝向 / 单位法线；
9 种 LOD 组合接缝逐位一致；build 铺世界（chunk / 拼块 / 句柄 / 顶面
逐盒）；角色迈 0.28 台阶 / 撞 0.9 陡差；NavGrid 翻丘航点贴盒顶
（±1e-3）+ 高差 ≤ step；GPU 混合级 triangles 精确对账 + 贴坡 ±2 cm +
无地形世界旧语义回归。视觉验收 `examples/terrain_snapshot.rs` →
`docs/terrain.png`（左 = 第三人称贴坡近景、右 = 高空拉远全 LOD2，
triangles 6112 → 1760）。

### 4.24 烘焙光照：CPU 光线跟踪 → UV2 图集 → GPU 集成（iris-render/bake、bake_uv、lightmap，R52a/b/c）

写实路线「间接光」三阶段，**已全部落地**：a = 纯 CPU 跟踪器 + 积分器
（R52a，零渲染集成）；b = UV2 参数化与图集布局（R52b，把「世界空间采样
点列表」换成 texel 网格）；c = 烘焙纹理上传 + 着色器消费（R52c，间接项
`select` 替换 IBL 环境项——**不是**接管美术补光 `ambient` 项，见下）。
先落 a 的理由：正确性（解析对照）、性能（要不要并行）、
确定性（逐位复现）三个风险全在跟踪器与积分器上，而这两块与 GPU / 资产
管线零耦合。

**场景三角收集（`collect_bake_triangles`）**：静态几何 only——无
`RigidBody`（组件缺席 = 静态）或 `Collider::is_static` 且非 Kinematic；
蒙皮实体排除（姿态每帧由骨骼驱动）；`Visibility::HIDDEN` 排除（隐藏几何
不遮光）；只认 `MeshHandle`（`LodMesh`/`LodMesh3` 几何——含 R51 地形
chunk——暂不参与，b 阶段按最高细节级接入）。顶点走 `GlobalTransform`，
法线走法线矩阵 `(M⁻¹)ᵀ`（与 `InstanceRaw` 给 GPU 的同式）；三角法线 =
三顶点法线平均，与几何法线（边叉积）反号时翻转——负缩放镜像实例的正面
朝向由环绕方向决定。反照率取材质 `base_color`（无材质 / 悬垂句柄 / 无
`MaterialStore` = 白，与渲染器兜底同口径）；**反照率是色溢的前提**，没有
它反弹辐射恒为白。

**BVH（`build_bvh` / `raycast_bvh`）**：中值分割，叶容量 4。**构建确定
性**：分割轴 = 质心包围盒最长轴（并列取低轴号），子区间按
`(质心分量 total_cmp, 原三角序号)` **全序**排序 ⇒ 同输入两次构建的节点
数组与三角重排逐位一致；构建序即结果的一部分（`BvhHit::triangle` 是重排
后下标）。**射线并列**：`t` 逐位相等取内部索引小者（严格 `<` 才替换 +
等值比索引），内部索引由构建序唯一决定 ⇒ 命中不依赖遍历次序。遍历 =
显式栈（近子先出）+ AABB slab 前进测试（方向分量为 0 时显式按平行板判定，
不走 `0 × inf = NaN` 被 min/max 吞掉的坑）；命中后用 `t` 收紧上界。
求交 = Möller–Trumbore（`ray_triangle_intersection`，公开纯函数，暴力
对照测试用它），**双面**（烘焙要回答薄板背面看到什么）；`dir` 须单位
（入口守卫断言、不静默归一化——归一化会插入 1 ULP 方向扰动，破坏与暴力
扫描的逐位可比性）。自交保护 = 原点沿法线偏置 2 mm（`BAKE_SHADOW_BIAS`）
+ `t > 1e-4`（`BAKE_RAY_MIN_T`）。

**采样（无 RNG）**：`radical_inverse_2`（van der Corput，纯位运算 +
半桶偏移）配 Hammersley `(u1 = 根数反序, u2 = (i+0.5)/n)`。
`cosine_hemisphere` = Malley 投影（u1 给方位、u2 给圆盘半径 ⇒ cos²θ 严格
分层，pdf = cosθ/π，恒在闭半球内）；`uniform_cone` = 球冠 cosθ 线性分层
（软影锥）。正交基的辅助轴规则固定（`|n.x| > 0.9` 用 +Y 否则 +X）⇒
同法线同基、逐位可复现。

**积分器（`bake_sample` / `bake_points`）**：辐照度 = 直接 + 天空/AO +
一次反弹。①直接 = `max(N·L,0) × light_color`，遮挡 = 1 条（硬影）或 8 条
软影锥方向的**未遮挡比例**（锥只改可见性、余弦项仍取光心 ⇒ 全亮区与硬影
逐位同值，半影 = k/8 档）；②天空 = 余弦加权半球，pdf = cosθ/π 的无偏
估计量 `(π/n)ΣL` ⇒ **开阔均匀天空解析值恰为 π × sky_color 且与朝向无关**，
AO = 被挡样本比例；③反弹 = 命中点出射辐射 `albedo/π × (其直接光 +
π × sky)`（即时辐射度一次近似），π 约掉后每样本贡献
`albedo × (E_direct + π·E_sky)/n` ⇒ 色溢解析值 = `albedo ⊙ (E_direct +
π·E_sky)`。反弹命中法线翻向射线来向（双面）；反弹的二次阴影走硬影单射线。

**性能实测（release，i7-13620H，2026-09-08）**：1000 三角 BVH 构建
217 µs；1000 采样点 × 64 sky 样本 + 1 反弹 + 0.2 软影 = **34.3 ms**
（criterion `bake_bvh_build_1k_tris` / `bake_irradiance_1k_points`，
烟雾报警 200 ms 见 `tests/budgets.rs`）。⇒ a 阶段**不引入线程池**；
b/c 阶段 texel 量放大越界时，唯一切分点是 `bake_points` 的 `points`
分段（逐点独立、按序拼接保序 ⇒ 确定性不破）。

**诚实限制（v1）**：只 0/1 次反弹（>1 断言）；反弹二次阴影硬影、命中点
天空开阔近似（深凹角反弹偏亮）；无面光 / 点光 / emissive / HDR 天空卷积
（天空单色常量）；半透明按不透明遮光；蒙皮与 LOD 句柄几何不参与；单线程。

**测试**（bake.rs 22 项 + budgets 1 项）：MT 解析 t / 重心双向复原、
剔除四支（出界 / 反向 / 超 max_t / 贴面自交 / 平行退化）、最近命中 +
共面并列取小索引、**512 三角散落场景 64 射线 BVH == 暴力扫描逐位**、
2048 三角两次构建 + 64 射线逐位、中值分割节点数钉死（4/5/8/9 → 1/3/3/5）、
退化三角拒绝；半球恒在闭半球 + E[ω] = (2/3)n（3σ 容差，Var = 1/18）、
球冠 E[cosθ] = (1+cos_apex)/2 + 退化锥逐位返轴、采样逆序重采逐位；
开放平面 direct 逐位 = 光色 × NdotL + sky = π×sky（倾斜 60° 天空不变）、
盒影内 direct 解析零 + AO 开区间、半空间 AO 恰 0/1、色溢 bounce ==
albedo⊙(1+π·0.5)（G/B 逐位相等）+ bounces 0 逐位零且其余分量不变、
软影本影 0 / 半影 k/8 / 全亮区与硬影逐位同、24 点两次烘焙全字段逐位；
世界收集静态过滤七支 + 旋转/非均匀缩放法线矩阵/镜像翻转解析值 + 反照率
三缺省路径 + 无 MeshStore 资源空返回。

**b 阶段（R52b）：UV2 参数化与图集布局（`iris-render/bake_uv`）**——把
a 阶段的「世界空间采样点列表」接到光照贴图 texel 上：世界几何 → chart
参数化 → 图集装箱 → texel 世界位回溯 → 逐 texel 辐照（复用
`bake_points`/BVH）→ 图集预览。组装入口 `bake::bake_lightmap`（收集 →
BVH → chart → 装箱 → 回溯 → 积分 → 行主序回填 `BakedLightmap { atlas,
radiance }`）。

- **chart 粒度 = (实体, 主轴)**：规范草稿「每 (实体, mesh 句柄) 一个
  chart」在本引擎等价于每实体一个 `MeshHandle`，但同实体不同朝向三角形
  投影平面不同（盒顶与侧面），故按 [`classify_axis`] 至多拆 3 chart。
  **分类完备**：`|n.y| > 0.7` → 水平（投影 XZ）；否则 `|n.x| ≥ |n.z|` →
  WallX（投影 ZY，并列取低轴号）否则 WallZ（投影 XY）。墙面类被丢弃轴
  分量 ≥ sqrt((1−0.49)/2) ≈ 0.505 ⇒ **投影面积 ≥ 0.5 × 世界面积**，回溯
  的 2D 重心分母有下界（不退化）。chart 内 UV = 投影 × `texels_per_meter`
  的**纯线性**映射（缩放 + 平移，单测钉解析值）。
- **投影重叠（串光）= 正面积相交判定 + 贪心 +x 偏移**：同 chart 内两三角
  投影正面积重叠（双层地板 / 盒子 ±X 面）时按输入序逐个放置、冲突者跳到
  其右缘 + 2 texel 间隙，chart 矩形取偏移后 bbox（「放不下」不存在）；
  每轮偏移越过至少一个已放置三角形 ⇒ 至多 n 轮终止。**不用 bbox 相交**：
  quad 两半恒共斜边、bbox 大面积重叠，按 bbox 会把每个 quad 撕成两块；
  共边相邻的 texel 归属由「先者得」解决，不是串光。
- **shelf 装箱**：全序 = `(面积降序 u64 精确整数, 实体位升序, 轴序号升序,
  原 chart 序)`；层内左到右、放不下开新层；chart 超图集或 shelf 高度耗尽
  ⇒ panic（尺寸不足报错语义）。利用率口径 = Σ chart 面积 ÷ (图集宽 ×
  **已用高度**)——末层以下空白是调用方 `atlas_size` 的选择而非算法浪费；
  规则网格（16 个 18×18 chart / 图集 128）解析值 **0.75**，验收场景
  50.4%。
- **texel 回溯**：逐 entry 逐三角光栅化其图集 bbox，texel 中心 → chart
  局部 → 2D 重心（投影仿射 ⇒ 即 3D 重心）→ 世界位；法线 = **三角面法线**
  （v1 不做顶点法线插值，与分类口径自洽）。点在三角形内 = 三边函数同号
  且 ≥ −1e-4（**含边界**）+ **先者得**（entry 序 = 装箱序、三角序 = 输入
  序）⇒ 8×8 格点斜边 texel 恒归三角 0（解析计数 36/28 单测钉住）。未覆盖
  texel 辐照恒零；输出按 **texel 行主序**升序 = R52a 预留的并行切分点。
- **预览导出**：辐照 → `aces_tonemap`（Narkowicz 系数与 `blit.wgsl`
  **同款公式**）→ sRGB 编码（blit 的 sRGB 编码由硬件目标格式完成，CPU
  预览显式同款）→ PPM P6；验收 `examples/bake_preview.rs`（室内盒：地板 +
  两墙 + 遮挡柱 + 侧照光）→ `docs/bake_preview.png`。
- **实测（release，i7-13620H，2026-09-08）**：验收场景（11890 覆盖 texel、
  8 texel/m、图集 256²、64 半球样本 + 1 反弹 + 0.08 软影）全链
  **212 ms**；柱影带 texel = 开阔地板的 23%、墙根 AO texel = 地板中心的
  21%（headless 断言 < 70% / 严格小于）。⇒ 仍无需线程池；c 阶段 texel 量
  再放大时切分点不变（`bake_points` 的 `points` 分段）。
- **测试**（bake_uv.rs 10 项 + bake.rs 1 项）：分类六轴向 / 45° / 阈值两侧
  / 256 哈希方向被丢弃轴下界扫描；UV 线性解析值（Floor/WallZ）；双层重叠
  偏移解析位移 + 共边 quad 不偏移 + 偏移后两两无正面积重叠；装箱不相交 +
  等面积实体序 + 利用率 0.75 精确 + mesh 恒 None；尺寸不足两 panic 支；
  回溯 8×8 格点世界位 / pad 与角落不覆盖 / 斜边 36/28 计数 / 重心自洽；
  共享边 texel 先者得四格钉死；ACES f(0)/f(1)/f(100)/逐通道/单调解析；
  PPM P6 头 + 黑/白/232/206 字节；全链两次烘焙逐位（atlas 全字段 +
  radiance to_bits）+ mesh 补齐 + 覆盖/零值并存 + 阴影对比。

**c 阶段（R52c）：GPU 集成与混合语义（`iris-render/lightmap`）**——把
a/b 的 CPU 产物接到渲染器：图集纹理上传（Rgba16Float）+ UV2 顶点属性 +
shader 间接项替换 + 动态光叠加语义。三阶段蓝图至此闭合（a 跟踪器 /
b 图集 / c 渲染）。

```text
bake_lightmap(world)                       ← R52b 全链组装
  └─ BakedLightmap { atlas, radiance, indirect, covered }
       └─ install_lightmap(world, baked)   ← 本阶段装配入口
            ├─ LightmapStore::insert → LightmapHandle（CPU 目录）
            ├─ build_uv2_mesh 逐实体 → MeshStore 新句柄（UV2 + 顶点拆分）
            └─ 逐实体换 MeshHandle + 挂 LightmapBinding
       └─ 渲染器懒上传 ensure_lightmap（Rgba16Float + 共用线性采样器）
            └─ standard.wgsl：间接项 = select(IBL 环境, 光照贴图辐照, mask)
```

- **混合语义 = 「静态 GI 补动态直射」**：光照贴图存的是
  [`BakeSample::indirect`] = **天空（含 AO）+ 一次反弹（含色溢）**，**不含
  烘焙直接光**——直接光由运行时方向光（CSM）与点光提供，把烘焙直射一起
  上传会与实时直射**双计**（受光面过曝、烘焙阴影与 CSM 阴影叠两层）。
  片元叠加次序：`直接项（方向光 + Σ 点光，光照贴图不参与）+ 间接项 +
  美术补光 ambient × ao + emissive`，其中
  `间接项 = select(ibl_env, kd_amb × albedo × E/π, mask > 0.5)`。
- **逐位不变的实现根据 = `select` 而非算术**：`mask = 0` 时
  `select(ibl_env, …, false)` 返回的就是 `ibl_env` **同一串位**；写成
  `ibl × (1−mask) + lm × mask` 虽然数学恒等，但把「未挂光照贴图的场景渲染
  逐位不变」这条硬约束交给浮点恒等式（`x×1.0 + y×0.0`）不如交给语言语义。
  掩码是**材质常量**（`MaterialParams.surface.w`，随 (材质, 图集) 绑定组
  缓存）：挂 `LightmapBinding` 且网格有 UV2 且句柄在目录内 = 1，否则 0。
  两道兜底：网格无 UV2 / 句柄悬垂 ⇒ `collect_instances` 解析时退回
  `NO_LIGHTMAP` 并告警（集成测试钉住「绑定但无 UV2 ⇒ 与基线逐位一致」）。
- **口径对齐**：IBL 的 `irradiance_map` 存的是 E/π（见 `environment.wgsl`
  的 `fs_irradiance`），光照贴图存 E ⇒ shader 乘 `INV_PI` 后两项可直接互换。
  光照贴图项**不再乘 `ao`**（orm.r）：遮蔽已烘进天空项，再乘是双重 AO。
- **与反射探针的协同（R62a）**：`select` 是**整项替换** ⇒ 挂图集的实体
  原本失去环境镜面（§8.31④）。R62a 之后，若片元还有生效的反射探针
  （§4.30），着色器在 `select` 之后**追加**探针镜面项：
  `ibl_final = ibl + probe_spec × (F·LUT.x + LUT.y) × env_intensity × ao`，
  且该项只在「有图集 且 有生效探针」时执行。语义即「间接**漫反射**走
  光照贴图 / **镜面**走探针」；无探针时该项不执行，R52c 的语义与逐位兼容
  完全不变（`tests/probe_render.rs` 的 `probe_restores_specular_on_lightmapped_entities`
  两侧对照：无探针时挂图集的金属球没有板的反射，有探针时出现）。
- **UV2 = 逐顶点图集归一化坐标**（`UV2_LAYOUT`，loc13，独立顶点缓冲，缺席
  绑常驻零垫）⇒ shader 采样**零常量变换**，同图集全部实体共用一份绑定组。
  注入 = [`build_uv2_mesh`]：逐三角形角点写 UV2；同一顶点被分进不同 chart
  （球面 / 圆柱的相邻面）或同 chart 内被重叠偏移移到不同位置时**按需拆分
  顶点**（复制全部属性后追加、索引改指）——拓扑不变（`triangles_drawn`
  不变）、逐三角形 UV2 恒正确。UV2 是**逐实体**数据而网格是共享资源 ⇒
  每实体一份网格副本（共享网格的两面墙各自分裂；副本内存计入
  `MeshStore::memory_bytes`）。局部三角形序号 = 全局下标在该实体区间内的
  **秩**（`collect_bake_triangles` 逐实体连续 + `build_charts` 不丢三角 ⇒
  秩即序号，函数内断言连续性）。
- **分批与兼容**：材质绑定组缓存键 = `(材质 << 32) | 图集槽位`（同图集共
  组）；绘制段按 `(键, 图集)` 切（`run_length`）。**排序键不含图集** ⇒ 无
  光照贴图场景的排序结果与批划分与 R52b **逐位一致**——这是
  `tests/lightmap_render.rs` 的 FNV-1a 全帧基线锁（R52b 捕获）与
  `tests/normal_render.rs` 的 R41 基线锁共同看门的硬约束，两条锁在本轮
  改动后**原值通过**。
- **dilation 填充（chart 边缘黑线的对策）**：chart 角点 UV 恰落在 texel
  边界 ⇒ 双线性对边界两侧各取一半，外侧是 pad / 间隙的黑值 ⇒ 不处理则
  每条 chart 边一道半 texel 暗线（R52b 留给本轮目检的决定）。`dilate_atlas`
  = 迭代 4 邻域均值扩散（双缓冲、至多 8 轮、只写未覆盖 texel、已覆盖逐位
  不变）；覆盖判据用 `BakedLightmap::covered` 位图而**不是** `!= ZERO`
  （封闭暗角的真黑值必须保留，按零判会漏光）。验收图目检无黑线。
- **GPU 资源**：图集 = `Rgba16Float` 单 mip（线性 HDR 辐照度，半精度对
  量级 10⁻²..10¹ 足够）；`f32_to_f16` 为本模块自带的 IEEE 754 binary16
  RNE 转换纯函数（免引 `half` 依赖，解析值单测钉住）；采样器全图集共用
  一份（双线性 + ClampToEdge）；未挂图集的材质绑常驻 1×1 黑纹理（创建即
  零初始化，无需写入）。懒上传 + 句柄缓存（照 `ensure_texture` 模式），
  `lightmap_is_uploaded` 供观测。
- **实测（release，i7-13620H，2026-09-08）**：验收场景烘焙 213 ms + 装配
  1.3 ms（4 实体、11890 覆盖 texel、8 texel/m、256² 图集 = 512 KiB 显存）；
  烘焙前后对照（`docs/bake_lightmap.png`，左原状 / 右挂载）：受影响像素
  99.3%、暗部四分位亮度 179.5 → 145.9（**81%**，墙角 AO）、亮部四分位
  226.7 → 215.5（**95%**，直射保持）；目检墙角 / 柱根接触暗化连续无条带、
  柱旁地板带暖红色溢、无 chart 边缘黑线。
- **测试**（lightmap.rs 9 项 + mesh.rs 3 项 + tests/lightmap_render.rs 3 项）：f16 全值域
  解析（±0 / 次正规中点取偶 / 正规 / 尾数进位推指数 / 溢出阈值 65520 /
  Inf / NaN quiet 位 / π）+ 可表示值往返；dilation 十字扩散两轮 / 两邻均值 /
  无邻域不动 / 确定性；UV2 注入逐角点逐位 + 冲突拆分（位置/法线/UV/切线
  全复制）+ 拓扑不变 + 数量不符 panic；端到端装配（目录建项 / 共享网格
  分裂 / 球面拆分 > 0 / 索引不越界 / 报告与增量一致）；UV2 角点 ↔ 图集
  坐标逐位 + 已覆盖 texel dilation 前后逐位 + 角点 texel 填充后非黑 + 墙角
  < 0.9 × 地板中心（AO 解析对照）；悬垂 mesh 句柄 panic。**GPU**：R52b
  全帧 FNV 基线锁原值通过；烘焙后暗部四分位降 ≥ 15% / 亮部保持 ≥ 85% /
  受影响像素 ≥ 10% / 懒上传已发生 / 两次渲染逐位；绑定但无 UV2 逐位退回
  IBL。

**C2 mip 链**：光照贴图上传路径随 `MipSettings` 在场生成线性域 2×2 box
链（`LightmapItem::mip_chain_half_bytes`——线性 HDR 存储 ⇒ 线性平均即
正确定义，无 RGBA8 路径的编码空间均值问题），逐级 `write_texture` +
三线性 / 各向异性采样器（采样器随图集上传期口径创建、绑进材质组，
与 `GpuTexture { view, sampler }` 同构）；**缺席 = 单 mip + 采样器逐字段
与 R52c 一致**（既有像素锁原值通过）；`gpu_bytes_estimate_mipped` 对账
口径；GPU 判别锚 `lightmap_mip_upload_and_minified_field_are_discriminable`。

### 4.25 遮挡剔除与 FXAA（iris-render/occlusion、shaders/fxaa.wgsl，R53）

写实路线 W4 收官轮：硬件遮挡查询剔除 + LDR 域 FXAA。两者的默认路径
都是**资源缺席 = 关 = 命令流逐位不变**（全部既有 GPU 测试的 FNV 基线
原值通过，兼容硬约束）。

**遮挡剔除（`occlusion` 模块 + renderer 查询段）**——wgpu 硬件遮挡
查询，两阶段、延迟一帧、保守剔除：

- **能力前提（对规范的偏离①）**：wgpu 30 中遮挡查询已是**核心能力**
  （WebGPU 规范把它从特性位升格为必选实现，`Features` 里不存在
  `OCCLUSION_QUERY` 位；Vulkan/DX12/Metal/GL/WebGPU 后端全部实现）。
  因此 `Gpu::supports_occlusion_query()` 恒返回 `true`，仅作为能力探测
  点保留；「无能力平台 = 全部可见」的回退由执行路径天然承担（读回
  迟到一帧即丢弃、本帧不剔除）。
- **开关**：`OcclusionCulling { enabled }` 资源，缺席 = 关（默认路径
  连查询 GPU 资源都不分配）。
- **两阶段数据通路**：帧 N 收集后对**视锥内全部普通不透明实例**画
  世界 AABB 代理盒（查询 pass 紧随主 pass，复用本帧不透明深度：
  Load 不写、Store 原样——贴花 / 透明 pass 仍要 Load 同一附件），
  每实例一个查询槽（4096 上限，超出恒可见）；resolve → copy → 帧 N
  提交后 `map_async`；帧 N+1 的 `draw` 开头 `poll(Poll)` + `try_recv`
  消费，样本数 = 0 者记入隐藏表，本帧主 pass 跳过绘制
  （`occlusion_culled` 计数并计入 `culled_instances`）。双帧读回环
  （resolve/readback 各两份按帧奇偶交替），同一时刻至多一条在途。
- **为什么查询在「主 pass 之后」而非规范草稿的「主 pass 之前」**
  （偏离②）：遮挡测试需要一份已填充的场景深度。草稿方案要求跨帧持有
  上一帧深度，但主深度附件是 MSAA（WebGPU 不支持 MSAA 深度采样解析）
  且每帧开头被清；复用**本帧**主 pass 深度是零拷贝、零额外 pass 的
  唯一位置，剔除决策仍延迟一帧（读回异步的固有代价），语义不变。
- **为什么每帧查询全部视锥实例**（偏离③，草稿只查「可见或未判定」）：
  被剔对象不再写深度，若停止查询它就没有恢复可见的路径（遮挡者移动
  ⇒ 永久空洞）。每帧重查让恢复延迟封顶一帧；查询盒本身是廉价光栅化
  （被挡者全部深度失败）。
- **保守性论证**（像素级，见 `occlusion.rs` 模块文档）：查询盒 =
  局部包围盒 8 角点过模型变换的轴对齐并集 ⊇ 几何；遮挡查询语义是
  保守下界（硬件可多报、禁止少报）。计 0 ⇒ 全部采样**严格**远于已
  写入深度 ⇒ 该实例对本帧颜色与深度零贡献 ⇒ 剔除不改变任何像素。
  关键实证修正：**深度比较必须 LessEqual**——可见实例的 AABB 前面与
  它自己在主 pass 写入的深度共面等深，严格 Less 会让每个轴对齐几何
  「自遮挡误剔」（贴花 R39 等深必须通过的同一课）。计 0 的语义在
  LessEqual 下更强（严格更远），保守性不损失。
- **只作用于主 pass**：阴影 / 点光阴影 / 场景深度副本仍按完整实例
  列表绘制——被遮挡者的投影与深度贡献保留，「开启遮挡与关闭逐位
  一致」不被阴影侧打破。主 pass 批内按隐藏表切连续子段（实例区间
  天然连续，同 `point_shadow_visible` 先例）。
- **读回节拍**：`map_async` 回调经 `device.poll` 派发且需要 GPU 真正
  退休提交（Vulkan 实测毫秒级真实时间）。60fps 事件循环天然满足；
  headless 背靠背连跑 `draw` 会让读回迟到 ⇒ 功能自动退化为全部可见
  （保守）。测试 / 示例在帧间做阻塞 `read_pixels()` 模拟帧节拍。
- **实测（release，i7-13620H，480×270，13 实例）**：遮挡开启剔除 3 /
  主 pass 三角形 116 → 80、`Renderer::draw` CPU 墙钟 0.125 → 0.085 ms
  每帧（−32%）；开启前后两帧**差异字节 = 0**（保守性像素级验证）。

**FXAA（`shaders/fxaa.wgsl` + post.rs 设置）**——后处理链尾部 LDR 域
抗锯齿：

- **链位**：blit（ACES 色调映射 → sRGB）之后、UI 之前。开启时 blit 改
  写中间 LDR 纹理（与最终目标同格式、同管线、同绑定组——输出字节与
  直写一致），FXAA pass 再写最终目标；关闭时 blit 直写（默认命令流
  逐位不变）。UI 在 FXAA 之后绘制 ⇒ HUD 文字不被抹糊（验收图两侧
  同样锐利 + 色块逐位一致测试锁定）。RT 视口无后处理链 ⇒ 不受影响。
- **算法**：FXAA 3.11 console 质量档紧凑实现——8 邻域显示域亮度
  （线性样本 `pow(1/2.2)` 近似回显示空间，原文边缘检测假设显示域）、
  亮度自适应阈值早退（**逐位透传中心采样**：非边缘像素不改写，
  8-bit sRGB 编解码往返恒等）、梯度定向 + 沿边缘 ±1/6 纹素两点采样、
  亮度带裁决取点或取中。未做：方向搜索细化与二阶亚像素项（诚实清单）。
- **采样器教训**（偏离④，实现细节）：链共享 `post_sampler` 是 Nearest
  （泛光靠显式纹素偏移工作），FXAA 的 A/B 采样点在纹素中心 ±0.5 内，
  Nearest 把亚纹素偏移量化回同纹素 ⇒ 整帧无差别透传（首版实测
  changed=0）。FXAA 用专属线性采样器（中心取样权重恰 (1,0,0,0)，
  早退透传性质不破坏）。
- **TAA 已做**（R60，§4.29）：HDR 链首时域抗锯齿——历史缓冲 + 投影
  抖动 + 相机重投影三件套落地，与本链单帧架构的冲突以「状态收敛在
  链首单一 pass」解开；与 FXAA 链位正交（HDR 链首 / LDR 链尾）、可
  同开为推荐组合。
- **测试**（occlusion.rs 4 项 + occlusion_render.rs 4 项 +
  fxaa_render.rs 1 项）：`world_aabb` 恒等 / 90° 精确 / 任意角含面中心
  + 代理表一致性（±0.5 角点、索引界内）；**GPU**：遮挡——全遮挡盒
  稳定帧 `occlusion_culled == 1`、instances 恰少 1、开启前后全帧像素
  **逐位一致**、首帧无结果恒可见、稳态逐帧逐位（确定性）、显式关 =
  缺席 = 兼容锁（FNV 0x0369_3e06_193a_9c14）、射线隐藏盒回归；
  FXAA——缺席 = 显式关 = 兼容锁（FNV 0x24e3_8254_0222_7bbb）、最大
  相邻跳变下降 + 过渡带（10%–90% 窗口）变宽 + 变更占比 < 35% +
  不透明 HUD 色块逐位不变 + 开启态逐位确定。**指标学**：总变差（TV）
  对单调重映射不变，判别不了平滑——首版用 TV 假失败，改最大跳变 +
  过渡带计数（实测教训）。

### 4.26 关卡格式 v2（iris-engine/level + scene，R57）

**定位**：给「关卡」一份持久化格式，让箱庭关卡的**静态世界**完全脱离
游戏代码存在——后续编辑器（R58）的数据地基，本轮只做格式与数据模型
（**无编辑器 GUI**）。边界：**关卡档管静态世界**（地形、静态碰撞体、
装饰网格含 glTF 实例、光照、天空源、NavGrid 构建参数、烘焙参数、命名
锚点）；**游戏代码管动态实体**（玩家 / 敌人 / HUD / 粒子池仍由 plugin
spawn）。命名锚点 = 实体段的 `name` + `transform`——格式**游戏语义中立**
（无 "player_spawn" 类专用字段，锚点含义由游戏侧约定）。

**关卡档 = 场景档（§4.15）的超集**：顶层 `version`（缺席 = v1 无损直读）
+ `level` 段。单通路——`load_scene` / `save_scene` 同时服务两代格式，
无第二套 IO。版本迁移 = 钩子函数表（`MIGRATIONS`，索引 = 源版本 − 1，
顺序施加）：v1 → v2 = 加默认空 `level` 段；高于当前版本 → `Unsupported`
拒读（与 pack「版本号 +1 旧 reader 拒读而非误读」同一策略）；save 恒写
当前版本，空 `level` 段省略 ⇒ v1 档重存只多 `version` 字段（无损迁移）。

**`level` 段三子段**（全可选；应用顺序 terrain → 实体段 → nav → bake——
地基先于摆设、nav / bake 消费静态几何全集）：

- **terrain**：heightmap 两态——`hills`（`generate_hills` 生成器全参数，
  无 RNG 同 seed 逐位一致）或 `inline`（行主序高度数组）。取舍：**不引用
  外置高度场文件**——v1 引擎没有高度场装载器，生成器参数与内联数组是与
  现状最贴合的表达；大地形内联 JSON 体积大（129² ≈ 16k 浮点），外置
  二进制高度场留待编辑器轮。材质段可选（缺席 = 句柄 0 白兜底），建造
  参数缺省 = `TerrainOptions::default`。`build_terrain` 派生实体（根 /
  chunk / 碰撞拼块）打 `LevelGenerated` 标记，save 跳过（terrain 段下次
  加载整体重建——`GltfMounted` 同款模式）。`Heightmap::new` /
  `build_terrain` 的全部断言前提在加载路径前置校验为 `OutOfRange`；
- **nav**：**只存 `NavGridParams` 全字段，不存烘焙格子**——加载后由
  世界静态碰撞体（无 `RigidBody` 或 `is_static`，排除传感器；收集序 =
  组件存储序）经 `NavGrid::build` 重建入 World 资源。位图是静态几何的
  纯函数（§3.9「便宜重建」），存格子 = 存冗余 + 「格子与几何失配」整类
  陈旧态。已知边界：被静态盒**全覆盖**的域无顶面雕刻种子（§3.9 实测
  教训）——整块地面盒的关卡应以 terrain 或隐式地面表达地面（assault
  关卡档因此不带 nav 段，其导航权威在 `logic.rs`，见 §8.33①）；
- **bake**：烘焙**参数**（`BakeParams` + `BakeUvParams` 全字段，对齐
  `bake_lightmap` 实际入参）+ 记录性 `dilate_iterations`（dilation 轮数
  是引擎常量 `LIGHTMAP_DILATE_ITERATIONS`，非运行期入参——不符告警）+
  **产物文件名引用**。产物存独立外置文件（IRISLMAP 二进制，见下）；
  加载时**存在则挂、缺失则诚实降级为无光照贴图并告警**（参数仍可驱动
  重烘）；**损坏 / 与关卡几何失配 = 报错**（静默挂错位阴影比不挂更糟）。

**光照贴图产物文件（IRISLMAP v1，全小端）**：魔数 + 版本 + 图集布局表
（chart 表项按 `Entity::to_bits` / `MeshHandle` 引用 + 逐三角图集矩形与
局部坐标）+ radiance / indirect 双辐照数组（f32 原样字节）+ covered
位图——`BakedLightmap` 全字段无损（radiance 保住 preview 工具通路）。
选型：自定义二进制而非 JSON（512² 图集 ≈ 6 MB 二进制 vs 13 MB 文本，
位保真免十进制往返论证）；编码是纯函数（同输入逐位同输出，测试锁定），
风格照 pack.rs 格式文档规约。**一致性校验**：产物必须产自「同一关卡档
加载出的世界」（确定性加载 ⇒ 实体位模式与句柄可复现）；挂载前逐项
校验（实体在世 / `MeshHandle` 相符 / 三角数相符 / 全局下标逐实体连续），
穷尽 `install_lightmap` 的 panic 前提后才安装；安装后把 UV2 副本句柄的
来源**回补进登记表**（照抄原句柄的图元 / glTF 来源）⇒ save 的 mesh 段
仍还原为原始来源，重载后产物重挂——往返闭环（测试钉住副本句柄可复现）。

**实体段扩展（v2）**：①材质段升为**三纹理槽**（`texture` 基色 /
`normal_texture` / `orm_texture`），每槽**纹理源两态**——字符串 = 文件
路径（v1 语义原样，serde untagged 直读）或对象 = **程序化源**
（`grass` = `grass_textures` 三件套 / `brick_normal_orm` = 法线 + ORM
对，引擎确定性生成器全参数）。sRGB 按槽位（基色 sRGB / 法线 ORM 线性）；
brick 源无基色产物，用于基色槽报 `Unsupported`。程序化源让写实材质
**零二进制资产**入档（关卡档保持纯文本自包含——assault 石板 / 砖墙即
此路径）；②`particles` 段 = `ParticleEmitter` 全部 13 个配置字段（缺省
= `new(Alpha)`；运行期状态不入档，重载新鲜）——水柱 / 装饰粒子自此可
入档（particles.rs 的「不写场景序列化」v1 限制作废）；③方向光段补齐
`shadow_range`（等于引擎默认时省略——v1 档重存字节级不变）。**点光
核实结论**：v1 实体段 `point_light` 已全覆盖 `PointLight` 四字段
（color / intensity / range / shadows），无缺口；方向光走 `environment`
资源段先例（`DirectionalLight` 不是实体）。

**同源去重（v2 加载语义）**：一次加载内，参数相同的图元 mesh、同
(路径, srgb) 的文件纹理、同参数的程序化纹理、解析结果位级相同的材质
**共享同一句柄**（首见注册、后见复用；`LoadCache` Vec 线性扫描——键含
f32 无 Hash，位模式元组对照，首见序确定）。「文件内同源 ⇒ 世界内同
句柄」是 assault 迁移逐位复现代码侧共享布局（一个 cube 九处引用、
盆材质双引用）的机制前提，也是编辑器（R58）的资产心智。glTF 例外
（整实例每次独立解析，v1 语义原样）。

**往返与规范化**：save 输出字段序 = 结构体声明序、`version` 恒写、
`level` 段按 `LevelRegistry`（加载时快照）原样回写。**浮点往返实测
结论**：serde_json 的 f32 序列化走 ryu 最短往返表示、反序列化正确舍入，
**逐位保真**（4112 个确定性伪随机位模式 + 边界值全集 `to_bits` 对照
测试钉住；NaN / Inf 是 JSON 非法值、被加载校验排除）——无需自定义
浮点输出策略，「经 f64 中转 1 ULP 漂移」的担忧实测不成立（最短表示
的十进制恰为 f32 值的精确 double，回转 cast 无损）。

**assault 庭院迁移实证（验收口径）**：`build_courtyard` 的约 170 行
代码生成（程序化纹理 / 五材质 / 四网格 / 15 实体 / 灯光）整体替换为
`load_scene(assets/assault_courtyard.level.json)`。迁移前后：同帧离屏
渲染两帧 **FNV-1a 逐位相等**（帧 A 0x1070dd6e79e0523c / 帧 B
0x31ca9c4842076a08，改码前捕获基线法）；FrameStats 全等（72/230/19/3150
与 83/275/19/2500，定格帧号 473 同）；headless parity dump（288 实体 +
三大 Store 内容 FNV + 登记表 + 资源位模式）**逐行一致**；既有 29 项
单测 + 8 项 headless 零改动全过。粒子 RNG 以 `Entity::to_bits` 为种子
⇒ 水柱实体 id 必须原位复现——档内实体序 = 原 spawn 序是硬约束
（`level_roundtrip.rs` 的实体位模式断言钉住）。

**测试**：level.rs 14 项（terrain hills / inline / 校验 / 草地三源、
nav 重建 / 校验、bake 产物闭环 / 缺失降级 / 陈旧报错 / 损坏报错 /
参数校验 / preview 通路、版本迁移、f32 位保真、端到端浮点往返）+
scene.rs 新增 2 项（粒子段往返、同源去重）共 28 项 + assault
`level_roundtrip.rs` 3 项（档往返逐位 / 档与 logic 常量双源守护 /
材质三槽接线）。诚实清单见 §8.33 与 level.rs 模块文档。

### 4.27 关卡摆放编辑器（iris-editor，R58a a 阶段 → R58b b 阶段）

**定位**：W5 内容产能工具的第一块——一个**能用的**关卡摆放编辑器。
它不发明数据格式：读写的全是 §4.26 的关卡档 v2，「编辑器 = 那份格式的
一个写者」。范围边界即验收标准。a 阶段（R58a）：视口 / 摆放 / 拾取 /
拖拽 / 删除复制 / 只读面板 / 存取。b 阶段（R58b）：**undo / redo**（快
照栈）、**光源参数编辑**（点光四键 / 方向光五键）、**烘焙入口**（`B`）、
**NavGrid 重建与可视化**（`N`）、**相机视角旋转**（中键轨道 + `F` 聚焦）。
拒绝清单剩余项（多选 / GLB 摆放与资产浏览器 / 地形雕刻 / 材质编辑器 /
文本输入框 / 快捷键自定义 / 主题 / **bake-nav 参数数值编辑面板**（需
`LevelRegistry` 快照语义升级为现读 + 回写，§8.33② 语义变更，单独评审）/
精确三角形拾取 / 动画预览）一条未越。

**分层（架构硬要求）**：

- `edit.rs` **命令层**：`EditorState`（World 资源：选择集 / 摆放类型 /
  材质预设 / 碰撞体开关 / 吸附开关 / 拖拽平面与偏移 / 状态行 + 私有句柄
  缓存与瞬态实体清单）+ 全部编辑操作为自由函数——`open_level` /
  `save_level` / `place_at` / `place_light_at` / `place_from_ray` /
  `pick` / `pick_with_hit` / `placement_point` / `select` / `begin_drag` /
  `drag_to` / `end_drag` / `delete_selected` / `duplicate_selected` /
  `nudge_selected` / `scale_selected` / `cycle_kind` / `cycle_preset` /
  `toggle_collider` / `toggle_snap` + 只读查询 `editable_entities` /
  `describe` / `panel_text` / `world_aabb_of` / `screen_ray_at`。签名一律
  `World(+ 事件参数) → 确定性结果`，**不含窗口 / 输入 / 渲染代码**，
  headless 直接单测（验收①的载体 `tests/edit_headless.rs` 19 项全走
  这一层）。
- `shell.rs` **交互壳**：`EditorPlugin`（Startup 开关卡 + Update 三系统：
  飞行相机 → 输入翻译 → Esc 退出）。输入翻译先把一帧设备状态读成
  `FrameInput` 只读快照再调命令层（避免 `Input` 守卫与 `&mut World`
  借用打架）。键位表在模块文档与运行期面板；`Ctrl` 按住时相机不移动
  （`Ctrl+S` / `Ctrl+D` 期间镜头不漂）。`build_schedule` 暴露编排，
  headless 测试与离屏快照不建窗口复用同一条推进路径（六游戏同款范式）。

**机制不变量①：登记表同步登记**。`MeshStore` / `MaterialStore` 只存顶点
与数值，`save_scene` 靠 `SceneAssetRegistry` 还原 mesh / material 段
（§4.15）。因此编辑器每创建一个**关卡内容**网格就登记
`MeshRef::Primitive(段)`（段 = 摆放参数，与存档写出的 mesh 段同源）、每
创建一个内容材质就登记 `MaterialRef::default()`（= 无外部纹理来源；预设
恒无纹理，与加载侧对无纹理材质段的登记逐字段相同）。不登记 ⇒ save 认不
出来源 ⇒ mesh 段整段省略 + 告警（重载后实体没有网格）——这是「编辑器
产物能被存档完整还原」的机制前提。

**机制不变量②：句柄编号对齐**。往返对照测试要能直接比较
`MeshHandle` / `MaterialHandle` **数值**（而不只比内容），需要保存侧与
重载侧的句柄分配序一致。两条手段：(a) 编辑器侧**同源去重**（同
`PrimitiveSection` 只入 `MeshStore` 一次、同预设只入 `MaterialStore`
一次——缓存用 `Vec` 线性扫描：键含 `f32` 不可 `Hash`，且插入序即句柄
序），与 `load_scene` 的 `LoadCache` 去重语义对齐；(b) `open_level` 把
**瞬态视觉资产**（网格线立方体 + 4 个材质）创建在关卡内容**之前**，两侧
都如此。于是「摆放 → 保存」与「重载」两侧句柄逐位相同（测试 dump 里
`mesh 1..5` / `material 5..10` 两侧全等）。注意**实体编号不在此保证内**：
编辑侧的选中框（12 条边）与关卡内容交错 spawn（摆放后自动选中），原始
`Entity` 编号必然与重载侧不同——而关卡档里根本没有实体 id，往返可对照
的是「顺序 + 内容」，故 dump 用清单序号（`slot`）而非实体 bits。

**瞬态视觉不入档**：地面参考网格（±12 m、1 m 间距、50 条线，X / Z 轴
红 / 蓝）与选中框（世界 AABB 的 12 条边，`gizmo_edge_transforms` 纯函数）
挂 `EditorTransient` 标记；`save_level` 在调 `save_scene` **之前全部
despawn、之后按缓存句柄重建**（重建不新占编号）。选 despawn 而非引擎侧
过滤标记的理由：`save_scene` 的跳过名单是 `GltfMounted` /
`LevelGenerated`，两者语义都是「该实体由档内某段整体重建」，编辑器视觉
不属于任何档内段；为它加第三个引擎级标记要改引擎存档过滤规则（a 阶段
不必）。属性面板 HUD 实体无 `Transform`，`save_scene` 的查询天然不含它。
拖拽期选中框走 `update_gizmo` **原位更新**（不 despawn 重建，实体编号
稳定、每帧调用无 churn）。

**拾取选型**：刻意**不复用** `iris_core::physics::raycast`——它只查
`(Transform, Collider)`，而编辑器摆放的实体可以没有碰撞体（可选挂载），
会拾取不到自己刚摆的东西。改用「网格实际包围盒」路线：`MeshData::bounds()`
局部 AABB 经实体模型矩阵变换成世界 AABB（`local_aabb_to_world`，
iris-render 的 `occlusion::world_aabb` 同语义但 `pub(crate)`，此处按同
算法重写）再交引擎纯函数 `ray_box`（slab 法，与物理同一份求交实现——
dogfooding）。命中裁决：`t` 最小者胜，`t` 位相等取**存储序在前**者
（遍历用严格小于比较即天然满足，无排序、无 HashMap）。摆放落点 =
实体表面与 y = 0 数学地面的**最近**交点，实体沿 +Y 抬升「网格局部包围盒
底面到枢轴的距离」⇒ 底面贴合命中面（叠放自然成立）。代价：旋转按轴对齐
语义放大包围盒（与引擎 `ColliderShape::Box` 忽略旋转的既有简化一致）。

**确定性选型**：句柄缓存 `Vec` 线性扫描（插入序确定）；实体遍历一律
`world.query`（组件存储序 = spawn 序；b 阶段注：遍历基准 = 查询首项
`Entity` 的**槽位升序**，分配器空闲列表 LIFO 复用使「删除后重建」的
槽位不可复现——undo 的 dump 对照因此按**名字序**转储，槽位是分配器
副产物的立场不变）；命名计数 `next_name` 扫描世界取
「同前缀最大下标 + 1」（不维护跨调用计数器 ⇒ 加载既有档后接着摆放不撞
名）；吸附 `snap_value` 只用除法 / `round` / 乘法（IEEE 确定运算）；面板
文本非 ASCII 折 `?`（8×8 位图字体只有 ASCII 字形，不伪装能显示中文）。

**undo / redo（R58b）**：命令层每个写操作（摆放 / 删除 / 复制 / 拖拽按下
/ 微移 / 缩放 / 光源编辑）执行前拍 `WorldSnapshot` 入栈（深度 32 环形丢
弃）。快照 = 可编辑实体全组件（存储序）+ 两 Store **整库克隆** + 登记表
全文 + 方向光资源 + `level` 段快照 + 编辑器句柄缓存；undo 后世界 dump
（名字序转储）与快照时点逐字符一致（测试钉住）。三条硬决策：①**恢复 =
全删全建**——组件存储 swap_remove 打乱存储序，部分重建无法还原清单序；
实体编号因此改变（选中清空、以实体位为种子的粒子运行期表现不保证复原，
配置字段逐位一致）；②Store 记**整库克隆**而非截断长度——undo 截掉的数
据 redo 无法凭空恢复（长度不可逆、数据可逆；快照世界无 glTF / 无烘焙副
本，体量有界）；③**挂载根边界**——存在带子实体的可编辑实体（glTF 挂载
根）时不拍快照、清空双栈（恢复子树需重解析文件），含根世界 undo 恒为空。
烘焙与 NavGrid 不入栈（幂等可重跑；烘焙且**清栈**——跨烘焙 undo 的整库
恢复会把在用的 UV2 副本从库里换掉）。

**光源参数编辑（R58b）**：模式由选中态裁决（`light_edit_mode`）——选中
点光实体 = 点光四键（`[` `]` 强度 ×/÷1.25、`-` `=` 范围 ±0.5、`H` 色相
+30°（`rgb_to_hsv` / `hsv_to_rgb` / `rotate_hue` 纯函数，饱和度 / 明度
保持）、`K` 阴影开关）；未选中实体 = 方向光五键（`[` `]` 强度、`;` `'`
环境光 ×/÷1.1（三通道同乘保色比）、`J`/`L` 偏航、`I`/`K` 俯仰——0.05
rad 步进、限位 ±89°、归一化回写，`rotate_light_direction` 纯函数）。
全部入 undo 栈、位模式精确测试钉住。键位冲突裁决：`L` 双语义（未选中 =
偏航 +；有选中 = 摆放点光——R58a 语义保留，摆放后自动选中使「连按连摆」
不受影响）。

**烘焙入口（R58b，`B` = 保存 + 烘焙）**：产物按实体位引用实体（R57 契约
「同一关卡档加载出的世界」），而编辑器活动世界因瞬态家具与分配器代数，
位布局与任何重载路径**不同构**——直接烘活动世界必然产出失配产物（实测
踩坑）。设计：先 `save_level`（文件 = 内容的规范事实）→ 在**规范影子世
界**（fresh World + 与 `open_level` 同款初始化序（含瞬态资产句柄前缀）+
`load_scene`）上跑 `bake_lightmap` → 产物 IRISLMAP 落盘（位引用 = 规范
布局，任何 `open_level` 重载都校验通过）→ 产物表项按**清单序**（两边的
可编辑实体序同一时刻同源）重映射回活动世界（实体位与 mesh 句柄都改写为
活动世界现值）→ `install_lightmap` + 登记表回补（UV2 副本继承原句柄来
源 ⇒ 烘焙后 `Ctrl+S` 仍写原始 mesh 段）→ 清 undo 栈。烘焙后继续摆放的
新实体无图集绑定走 IBL（引擎既有混合语义）。参数 = 档内 `level.bake`
（缺席 = 引擎默认 + 档同目录 `*.ilm` 默认名，档内无引用、重载不自动挂
载——`level` 段只读语义不变）。附带发现并修正引擎 bug：`uv_sphere` 极
点带的 quad 剖分有一半是零面积退化三角（极点环顶点全重合），与
`build_bvh`「primitives 不产出退化三角」的前提矛盾，任何含球场景烘焙即
panic——极点带改三角扇修正（视觉零差异），`mesh.rs::sphere_topology`
与 `lod_render::HIGH_TRIANGLES` 两个金标准随之重推导（索引数 =
(rings−2)·segments·6 + 2·segments·3）。

**open_level 的内容实体先行（R58b）**：实体生成序改为「状态 → Store →
基线 → 瞬态**资产句柄**（非实体）→ 关卡内容实体 → 网格线 / HUD 家具」
——内容实体从 0 号槽连续、代数 0，与 `load_scene` 的规范位布局一致
（烘焙产物按实体位引用的前提）；a 阶段的「家具先行」布局使创建会话与
重载的实体位从第二个内容实体起必然分叉（选中框交错 spawn）。家具实体
只在自身槽位间复用，内容位从此跨存 / 载稳定。a 阶段测试中「创建会话 ==
重载」的编号断言随之改钉「重载侧内容实体 0 号槽起步」（创建会话的内容
在家具之后，两侧本就不同构——dump 的清单序号对照不受影响）。

**NavGrid 重建与可视化（R58b，`N`）**：开 = 重建 + 生成可视化（每次开都
重建，关再开 = 手动重建），关 = 撤可视化、`NavGrid` 资源保留。参数优先
档内 `level.nav`；缺席时 `nav_bounds_from_statics`（纯函数）从世界静态
碰撞体（与 R57 `apply_nav` 同判据）的轴对齐 AABB 并集推 bounds + 默认格
子参数。可视化 = blocked 格半透明红薄片（复用瞬态线网格，浮在可站立
表面上，挂 `EditorTransient`——不入档、不可拾取）+ 域边框橙线 ×4；
面板显示 格子总数 / blocked 数（`is_blocked` 逐格统计）/ 域尺寸。可视化
是重建时点的快照，后续编辑不自动刷新。

**相机轨道（R58b）**：`orbit_eye` 球坐标解析式纯函数（yaw = `atan2(z,x)`、
pitch = `asin(y/r)`，俯仰钳 ±89° 防 gimbal），中键拖拽按像素增量驱动
（灵敏度 0.005 rad/px，拖右 = 偏航 +、拖上 = 俯仰 +）；`F` =
`focus_selected`（target 直设选中实体世界 AABB 中心，eye 不动）。相机仍
不入档（§8.33⑥ 不变）。

**undo / 烘焙 / 光源的验收补充（R58b）**：headless 测试 19 → **31 项**
（undo 多步序列 dump 对照含「undo 到头再 redo 到尾」全恢复 + 深度 32 环
形丢弃 + 拖拽按下拍一次 + 无状态 inert；点光 / 方向光编辑位模式精确 +
模式裁决；烘焙闭环（带 bake 段：产物落盘 → UV2 副本 → 登记表回补 →
save → 重载重挂句柄可复现 → 世界 dump 逐字符一致；无 bake 段默认名）；
nav 自动 bounds 与纯函数同源 + blocked 逐格统计 + 可视化不入档不可拾取
+ 段参数优先；orbit 半径保持 / 俯仰钳制 / F 聚焦）；离屏验收图
`docs/editor_b.png`（2×2：点光编辑前后 / 烘焙前后 + 导航叠加 + 面板
UNDO/BAKED/NAV 行，`examples/editor_b_snapshot.rs` 产出，两帧跨运行逐字
节一致）。

**验收（a 阶段口径，b 阶段在上）**：`tests/edit_headless.rs` 19 项——9 实体（四图元 + 五预设中的
五种 + 静态碰撞体 + 点光 + 复制体）摆放 → `save_level` → 新世界
`open_level` → 世界 dump（实体数 / 清单序 / 逐实体 Transform 位模式 /
句柄 / 组件位 / 登记表 BTree 序 / 两 Store 逐句柄 FNV 与位模式 / 方向光
位模式 / level 段快照）**逐字符相等**；同档二次保存逐字节相同；拾取
最近 / 同距存储序 / 瞬态不可拾取；拖拽吸附与高度保持；删除级联 / 复制
后缀；屏幕像素 → 射线 → 拾取闭环；面板只读纯 ASCII；`level` 段快照
原样保留；**assault 庭院档（R57 产物）经编辑器开 → 编辑 → 存 → 重载
往返一致**（编辑器不改坏既有档）。离屏验收 `examples/editor_snapshot.rs`
→ `docs/editor_mvp.png`（整体：24 实体测试关卡 + 网格 + 选中框 + 面板）
与 `docs/editor_viewport.png`（编辑态近景：换视角换选中）。窗口交互
`cargo run -p iris-editor --release`（CLI 第一参数 = 关卡档路径，缺省
`assets/editor_mvp.level.json`；档存在但解析失败 **panic** 而非降级为空
世界——避免随后一次保存覆盖原档）。诚实清单 §8.34 与 edit.rs 模块文档
（9 条）。

**C2 探针编辑入口**（§8.37⑧ 闭合）：`M` 摆放探针实体（单位立方体视觉
标记 + `ReflectionProbe`，**双写契约**：组件 `half_extents` 权威、标记
`Transform.scale = 2×half` 随命令同步）；选中探针时 `[ ]` = 过渡带
falloff ∓/+ 0.5 m（钳 [0,10]）、`- =` = 半边长 ÷/× 1.25（钳 [0.05,100]）；
探针进 undo 快照字段集（修复声明的「undo/redo 后丢失」）；烘焙入口回报
`probe_count` 提醒捕获重烘属游戏侧（§8.37③ 立场）。序列化走 `probe` 段。

### 4.28 资产管线 v2：动画注册表 / BC3 压缩通路 / manifest + 优先级（R59）

外部资产「从导入到运行」一条龙的三块独立能力（管线骨架层的优先级 /
续载语义修订见 §7.3 R59 增量）。

**GLB 动画注册表**（`iris_render::asset::registry`）：`AnimationRegistry`
（World 资源）维护 资产路径 → `GltfAnimationLibrary`（`BTreeMap`，迭代
恒路径字典序——与 broadphase / pack 条目表同一确定性选型）。库 = 共享
剪辑数据 `Arc<Vec<AnimationClip>>` + 骨架节点名表（glTF 节点声明序）+
逐剪辑元数据 `ClipMeta`（名字 / 时长 / 轨道数 / 事件数）。三个动作：
`import`（headless 批量导入——不 spawn 实例也能解析元数据，先看有什么
动画再决定加载；解析与 spawn 路径共用 `build_clip` 同一实现，错误在
导入期即报）；`spawn_instance`（命中则共享剪辑数据不重新解析、未命中
自动导入；实体树 / 网格 / 材质 / 纹理工作与常规 spawn 完全一致）；
`clip_index_by_name`（按名取用，重名**取先**——与骨架名表同一约定）。

**共享生命周期取舍**：`AnimationPlayer::clips` 与 `GltfInstance::clips`
升级为 `Arc<Vec<AnimationClip>>`——注册表命中 spawn 的 N 个实例 + 注册
表共享**同一份**关键帧数据（引用计数），「每 spawn 深拷贝一份」的内存
重复归零。选**引用计数而非克隆**：克隆语义只省解析 CPU、内存重复原样
保留，与本轮动机相悖；Arc 生命周期 = 最后一个持有者（注册表或任一播放
器），无悬垂风险。读路径经 Deref 透传全兼容；**写时复制口**
`AnimationPlayer::clips_mut()`（`Arc::make_mut`）给「注入事件轨道」这类
私有修改——共享播放器首次调用克隆出本实例私有副本，不影响注册表与
其他实例（iris-ascent 脚步事件注入即此用法）。诚实边界：`Arc` 强计数
> 1 时直接 `get_mut` 返回 None（共享数据不可变借用规则）——修改剪辑
应走 `clips_mut` 或独占构造。常规（非注册表）spawn 行为不变：每实例
独占一份（强计数 1，语义与按值 Vec 等价）。

**BC3 压缩通路**（`texture_bcn.rs` + `texture.rs` + 渲染器决策树）：
`encode_bc3` = BC4 alpha 块（8 B：a0/a1 端点 + 16 像素 × 3-bit 索引，
LSB 优先）+ BC1 色块（8 B，**复用 `encode_bc1` 输出**）= 16 B/块 =
1 B/px（RGBA8 的 1/4）。**分层规则**（`bc1_eligible` / `bc3_eligible`
判定口径，两决策对同一输入恒互斥）：全不透明走 BC1（0.5 B/px，无 alpha
语义可丢且压缩率翻倍）；带 alpha 走 BC3（BC1 的 1-bit punch-through
表达不了连续半透明）。**兼容硬约束的兑现方式**：BC3 只对「经理入期
manifest 声明」的纹理生效——`TextureStore` 维护声明侧表
（`declare_bc3` / `add_bc3_declared` / `is_bc3_declared`，BTreeSet），
渲染器 `ensure_texture` 决策树只对声明过的纹理启用 BC3 分支，未声明
纹理的决策路径与 R59 之前**逐位一致**（全部既有 GPU FNV 基线零变化，
`bc1_render` / `post_render` 等兼容锁原值看门）。**入库形态选「加载期
编码」**（渲染器懒上传时编码，与 BC1 同模式）：上传决策保持单一事实
来源（避免「盘上格式 × 运行时格式」两套真相）；GLB 内嵌纹理是 PNG
data URI，盘上预编码需发明新容器（拒绝清单禁 KTX2/DDS）；CpuTexture
保持 RGBA8 使默认路径不变最易论证。代价是上传时一次 CPU 编码（与既有
BC1 路径同量级）。**误差断言口径（诚实）**：BC3 有损——alpha 端点取
块内极值像素（8-bit 原值零量化），8 值调色板等分端点区间，单像素误差
上界 = (a0−a1)/14 ≤ 255/14 ≈ 18.2；纯色 alpha 块索引全 0 恒等还原
（8 值模式端点相等时硬件解为含 0/255 穿透值的退化调色板，索引全 0
绕开）。GPU 侧等价渲染测试（`bc3_render.rs`）：块内纯色棋盘格 alpha
与 RGB 解码精确，BC3 路径与 RGBA8 回退全帧平均误差 < 2/255；无 BC
特性环境两侧同走 RGBA（无分支回退，与 BC1 同款）。

**资产 manifest**（`iris_engine::manifest`）：JSON 清单（路径 / kind
`"scene"|"raw"` / 优先级整数 / `bc3` 标志），serde derive 零新依赖；
校验中文报错（空路径 / 重复路径 / 未知 kind / 结构损坏，定位到条目）；
未知字段按 serde 默认忽略（前向兼容，与场景 JSON 同约定）。条目表
`BTreeMap` 按路径字典序——`request_all` 按该序逐条以自身优先级入队，
**请求序与清单内容一一对应** ⇒ 加载线程出队序列完全由清单内容决定
（分帧 poll 的完成序 = 出队序，串行加载线程）。`bc3: true` 条目的
消费通路：异步加载拿到 GLB 字节 → 注册表 spawn → `GltfInstance::textures`
（R59 新字段：本次 spawn 落入 `TextureStore` 的全部句柄，含 ORM 合成
路径）→ 按 manifest 标志逐句柄 `declare_bc3`（只需发生在首次 GPU 上传
前——懒上传）。

**全流程验收**：`gen_r59_assets`（程序化生成含内嵌 alpha 贴图 + 法线
贴图 + idle/swing/hit 三命名动画的 GLB 与 manifest）→ `r59_snapshot`
（manifest → 优先级异步分帧加载 → 注册表导入 → 双实例 spawn（共享
`ptr_eq` 断言）→ 按名切动画 → 离屏渲染 `docs/asset_pipeline.png` 六
面板：三动画姿势帧 / BC3 vs RGBA8 对照 / 加载时序与显存数字
（RGBA8+BC1 292 B → BC3+BC1 100 B）；快照跨运行逐字节一致）。
headless 端到端测试 `iris-engine/tests/asset_pipeline.rs`（清单 → 异步
优先级出队 → 注册表元数据 → 双实例共享 → bc3 声明落表显存口径 → 按名
播放姿势精确断言 + 管线级中断续载）。

**v1 不含（诚实清单 → §8.35）**：网格 / 材质 / 纹理的跨实例去重（注册
表只共享动画数据，spawn 仍逐实例解析实体树）；纹理 GPU 上传异步化；
BC3 盘上预编码容器；剪辑数据惰性解析（导入即全量读关键帧浮点）；清单
无目录递归 / 通配符；同一 GLB 多实例的纹理重复登记（每次 spawn 独立
注册句柄——`GltfInstance::textures` 按次返回，去重是独立工程）。

### 4.29 TAA 时域抗锯齿（历史缓冲 + 投影抖动 + 相机重投影，R60）

R53 遗留的「三件套与单帧链架构冲突」（§8.32⑩）在本节解开：历史与
抖动把渲染变成**有状态**过程，但状态被严格收敛在「TAA pass 一处 +
一个帧计数器 + 一个重投影矩阵快照」，链的其余部分仍是无信号单帧
pass。`TaaSettings` 资源**缺席 = 关**（零开销默认路径：不抖动任何
矩阵、不分配历史纹理、链命令流与 R59 逐位一致——既有全部 GPU FNV
基线的兼容铁律不变）。

```text
主 HDR 解析缓冲（MSAA 4x 解析结果 + 天空 + 透明 + 粒子）
  → TAA（可选，链首，全分辨率）：抖动色 + 深度副本 + 历史 → 3×3 邻域
      钳制混合 → 新 HDR（写入本帧历史槽，下游链直接读它）
  → 景深 DOF → 运动模糊 MB → 泛光 → 暗角 → blit（原有链序不变）
```

**六项设计裁决**：

1. **链位 = 链首**（主解析缓冲之后、DOF/MB 之前）。时域累加的输出是
   本帧最干净的 HDR 图像，应给所有后续 pass 当输入；排在链尾的错误：
   DOF 焦外光斑与 MB 拖影已被混进颜色，TAA 的邻域钳制会把「历史里的
   清晰边缘 vs 当前帧的模糊值」之差误判为离群鬼影整段钳掉（抗锯齿
   pass 上下游内容必须同构——邻域统计只对同类内容成立），且 LDR 域
   累加会放大 8-bit 量化噪声。FXAA 已占链尾 LDR 域，TAA 在链首 HDR
   域，两者链位正交、可同开（推荐组合：TAA 消时域走样 + FXAA 补
   残余 LDR 边缘；TAA 收敛后 FXAA 早退分支大多透传）。
2. **抖动 = Halton(2,3) 8 点序列**，帧号模 8 索引（`taa_frame` 计数器，
   纯整数 radical inverse，确定性零依赖）。偏移 =（Halton − 0.5）纹素
   经 `jitter_projection` 加进**纯投影矩阵再乘 view**（row0 +=
   jx·row3，单元素实现依赖 w 行 = (0,0,±1,0) 的投影结构；对合成矩阵
   行运算不是纯 NDC 平移——NDC 位移有解析单测钉住符号与幅值）。选 Halton(2,3) 8 点而非
   4 点旋转网格：单位像素内低差异、8 个亚像素位在 MSAA 4x 之上使
   静止收敛帧 ≈ 32 等效样本，成本只是 8 组预计算偏移查表；4 点旋转
   网格在时域相关的相机运动下更易出条纹。**作用范围**：抖动只作用于
   主相机投影，且**本帧矩阵处处一致**——`Globals.view_proj /
   inv_view_proj`（主 pass 几何 + 天空盒反投影）、视锥剔除（与渲染
   同矩阵，剔除边界随抖动 ±0.5 纹素移动，边界处提前/推迟半纹素被剔，
   与既有剔除近似同类）、scene_depth 深度副本、**遮挡查询代理 pass**
   （与主深度共用矩阵故一致抖动——保守性论证原样成立：AABB ⊇ 几何
   是世界空间性质与矩阵无关，深度与代理在同一 clip 空间比较，抖动是
   作用于两者的全局刚性偏移，相对深度序不变，±0.5 纹素不破坏「计 0
   ⇒ 零像素贡献」）、MB（当前帧反投影用本帧矩阵、上帧重投影用上帧
   实际渲染矩阵）。**CSM 阴影不抖动**（光空间矩阵由 `cascade_fit`
   从 view 拟合，独立于投影）；**RT 相机不抖动**（无后处理链）。
3. **运动向量口径 = 相机重投影**，与 MB 复用同一份数学与同一张深度
   副本（线性视距 → NDC z 解析重建 → 本帧 `inv_view_proj` 反投影
   世界位 → `prev_view_proj` 重投影 → 历史 UV）。**无逐物体运动向量
   缓冲**（GBuffer 级工程，拒绝清单）——动态 / 蒙皮物体的历史采样位
   置错误，鬼影由颜色钳制压制（拉回当前邻域色域边界）不消除，进
   §8.36 诚实清单。
4. **颜色钳制 = 3×3 邻域 RGB AABB min/max**（9 样本 `textureLoad`），
   不用 YCoCg 变体：YCoCg 的增益在色度过冲型鬼影，而 v1 的鬼影源是
   重投影错位（结构 / 亮度型），RGB AABB 已把历史拉回当前邻域包络，
   实现零调参常数（YCoCg 变体通常还要配收缩系数）。混合系数固定
   0.9 历史 / 0.1 当前（收敛速度与残影长度的经验折中，不做配置面板）。
   钳制 + 混合的意义：邻域内（静止边缘）历史可信 → 时域累加降噪；
   邻域外（遮挡揭示 / 物体移走）历史被判为过期 → 拉回边界，拖影
   衰减有界（约 1/0.1 = 10 帧量级衰减到 1/e）。
5. **历史与重置**：历史 = **Rgba16Float 双缓冲 ping-pong**，独立字段
   `TaaTargets` 挂在 `HdrTargets` 下（不用 `PostScratch`：其 Full 纹
   理对每帧被链上 pass 覆写，历史必须跨帧存续；挂 HdrTargets 使
   分辨率变化时随集合重建 = 免费获得尺寸变化重置）。TAA pass 读
   hist[上帧] 写 hist[本帧]，下游链经 `PostInput::TaaHistory` 读本帧
   输出。跨帧状态全集 = 双历史纹理 + `prev_view_proj` 快照（既有 MB
   字段复用；TAA 开帧存**抖动后的**矩阵——下帧重投影需要的就是上帧
   实际渲染矩阵）+ `taa_frame` 计数。**重置 = 显式 API
   （`Renderer::reset_taa`）+ 自动重置（首帧启用 / 关→开切换 / 分辨
   率变化），不做相机切镜阈值检测**——切镜检测是内容相关启发式
   （阈值无普适口径，快速平移的正常镜头会误触发），而宿主天然知道
   切镜时刻，显式 API 把决策交给有信息的一方。重置帧混合系数置 0 =
   输出逐位等于当前帧（`TaaSettings` 开启首帧与无 TAA 帧链输出逐位
   一致，测试锁定）。
6. **共存语义**：**MSAA 4x**——抖动叠加在 MSAA 之上（主 pass 仍在，
   只是几何整体亚像素位移后做 4x 解析；TAA 再时域累加，静止收敛
   = 4×8 = 32 等效样本 SSAA；MSAA 管线与目标格式零改动）。
   **FXAA**——链位不冲突（HDR 链首 vs LDR 链尾），可同开，推荐组合。
   **RT 视口**——无后处理链，不受影响（与既有语义一致）。

**确定性**：历史反馈使 TAA 开启的渲染有状态，确定性文化的硬要求因此
升级为「**两个同输入完整会话（≥120 帧，含相机运动与静止段）逐帧逐位
一致**」——Halton 索引、混合算术、纹理反馈全部是同输入同输出的纯
函数式演化，GPU 测试锁定。静止稳态下输出收敛为 Halton 周期的 8 帧
循环轨道（当前帧贡献随抖动循环变化），稳态 FNV 基线锁定在固定帧号。

**实现布局**：`TaaSettings { enabled }`（post.rs，缺席 = 关；
`Default = 关`）；`taa.wgsl`（组 0 五 binding：参数 144B + 抖动色 +
线性采样器 + 深度副本（非过滤，与 MB 同款系数重建 NDC z）+ 历史）；
`taa_layout` / `taa_pipeline`（pipeline.rs）；编码走 post.rs 的
`encode_taa_pass`（**独立于** `encode_post_pass` 的第二份编码函数——
TAA 是链上唯一有状态 pass：写历史目标 + 三路输入纹理，组布局 5
binding 超出框架的 4 binding 上限；编码样板仍收拢在 post.rs 单点，
不散落 renderer）。链源切换由纯函数 `chain_source` 单点定义
（DOF / 泛光 / 暗角 / blit 的源在 TAA 开启时从主解析缓冲切到本帧
历史输出，全分辨率 ping-pong 槽位拓扑 `full_chain_slots` 不变）。

**v1 不做（诚实清单 → §8.36）**：逐物体 / 蒙皮运动向量缓冲（鬼影
压制不消除）；相机切镜自动检测；历史有效性掩码（stencil / depth
reject 精细化）；抖动模式 / 混合系数运行时切换；SMAA；TAA 收敛前的
单帧画质（重置帧后约 10 帧内边缘仍偏糊——收敛过程的固有代价）。

### 4.30 烘焙反射探针（局部长方体影响域 + 预滤波 cubemap + 视差修正，R62a）

全局 env 贴图（§4.12）只能反射天空：房间里的金属球与地面反射不到周围的
墙与物。R62a 引入**烘焙反射探针**——场景中一点向 6 面捕获 cubemap，按
粗糙度预滤波（与 env 同一条 split-sum 链），片元落在影响域内时以探针取代
全局 env 做**镜面**采样。

- **影响域是轴对齐长方体**（`ReflectionProbe { half_extents, falloff }`，
  位置取实体 `GlobalTransform`）。选盒不选球的理由：①室内房间是盒，
  长方体能贴着墙 / 地板 / 天花板铺满空间，球形域要么包住墙外、要么缩进
  房间中央留一圈无探针的边角；②视差修正的投影几何**就是**盒（射线与
  轴对齐长方体求交有解析解），球面投影没有与之对应的解析解，只能退化成
  「按距离缩放方向」的近似；③影响域权重可逐轴分离定义（乘积形式），
  球只能给径向衰减——贴墙的探针会在墙侧留下一条权重不满的缝。
- **权重**（`probe::influence_weight`，GPU 侧逐行镜像）：逐轴
  `f_i = clamp((E_i − |p_i|) / falloff, 0, 1)`，三轴**乘积**。域内
  （`|p_i| ≤ E_i − falloff`）恒 1；过渡带内随 `|p_i|` 线性降到 0；任一轴
  越界 ⇒ 因子 0 ⇒ 乘积 0（域外必不生效）。`falloff = 0` 退化为二值判定
  （不做除法）。乘积形式在角区给出二次衰减（等高面是圆角盒），比三轴取
  min 更贴合「两面墙都会削弱探针可信度」的直觉。
- **视差修正**（经典 box projection，`probe::parallax_direction`）。盒是
  轴对齐的，取片元相对探针中心的局部坐标 `p`（盒 = `[−E, E]`），反射射线
  `p + t·d`（`t ≥ 0`）与盒出射面求交：

  ```text
  d_i ≠ 0 :  t_i = (sign(d_i)·E_i − p_i) / d_i
  d_i = 0 :  |p_i| ≤ E_i ⇒ 该轴不约束（t_i = +∞）；否则射线恒在板外（无交点）
  t = min_i t_i,   q = p + t·d,   采样方向 d' = normalize(q)
  ```

  性质：**中心恒等**（`p = 0` ⇒ `q = t·d ∝ d`，任意方向都不被改写）；
  域内任意点的出射点必在盒面上；`d_i` 精确为 0 时不做除法（无 NaN）；
  盒外且射线背离盒 / `t ≤ 0` / 零方向 ⇒ **原样返回**（该片元权重必为 0，
  回落全局 env）。**盒投影对「落在盒面上的几何」是精确的**——射线从片元
  出发在盒面出射，采样方向 `normalize(q)` 恰指向该盒面点，而探针记录的
  正是「中心看向该盒面点」的辐射度；不在盒面上的物体会按盒面近似重投影
  （诚实限制见 §8.37）。解析单测锁定五组特例（中心恒等 / 偏心解析解 /
  逐轴并行 / 盒外平行回退 / 盒外指向盒），GPU 测试用**解析镜像位置**
  做落点断言（见下）。
- **捕获内容含天空（决策点）**：6 面各自先画一遍天空盒（复用
  `skybox.wgsl` 与 `inv_view_proj` 射线重建，与主视口逐像素同源），再画
  不透明几何。另一条路（天空清黑 + 盒外方向回落全局 env）需要**逐 texel
  的有效性掩码与混合**（哪些 texel 打到几何、哪些打到空），且盒内本来能
  看到的天空（窗口 / 天窗）会丢失。含天空的产物是**完整辐射度函数**：
  域内任意方向都有值，采样侧只需一次 `textureSampleLevel`、零回退分支。
  代价写进诚实限制（§8.37）：探针内容与烘焙时刻的 `SkySource` 绑定，
  换天空源后需重新烘焙。
- **预滤波链复用**：`environment::encode_prefilter_chain` 把源立方体的
  0 级 mip 按 GGX 预滤波进目标立方体的整条 mip 链（同一 `fs_prefilter`、
  同一 256B 步长动态偏移、同一「mip ⟷ 粗糙度 = mip/(mips−1)」映射）。
  着色器侧探针与 env 共用**同一表达式** `roughness * globals.env_params.x`
  ⇒ 同一档粗糙度取同一档 mip。探针 mip 级数 = `ENV_MIP_COUNT`（5），编译期
  `const _: () = assert!` 锁死一致性。**源与目标必须是两张纹理**（预滤波
  pass 同时把源绑成采样纹理、把目标绑成颜色附件），故探针保留一张所有
  探针共享的「原始捕获」擦写立方体（Rgba16Float 128²×6，768 KiB）。
- **捕获 pass 的面矩阵与剔除（实测教训）**：面矩阵用
  `light::cube_face_view_proj`（90° 透视 + **NDC y 翻转**，与点光阴影
  同源，推导见 §4.7.1）。翻转会**反转三角形在帧缓冲空间的环绕方向** ⇒
  捕获管线必须把正面约定同步翻成 `front_face: Cw`（`probe_capture_pipeline`
  / `probe_capture_skinned_pipeline`）。沿用 `Ccw` 的实测症状：探针被金属
  球壳包住时整张 cubemap 变成球内壁的暗灰——**剔掉的是房间朝内的墙面、
  渲染的是外壳**，墙上图案与自发光板全部消失（`tests/probe_render.rs` 的
  朝向测试与验收图面板 1 都抓这个）。
- **捕获的光照**：方向光阴影以**探针盒**做单级联拟合（`probe::fit_probe_shadow`：
  光视图 = 世界原点出发的纯旋转，盒 8 角求光空间 AABB，XY 走纹素对齐，
  深度窗口以包围球半径向光 / 背光外扩），渲染进**主帧的级联槽 0**（`splits`
  全为 +∞、`cam_forward` 置零 ⇒ 所有片元恒选级联 0）。室内场景的太阳光
  因此被墙 / 天花板正确遮挡——否则封闭房间的地板会在探针里「晒到太阳」。
  槽 0 是**临时借用**：主帧每帧重画级联，下一次 `draw` 即覆盖。点光按
  世界位置进 uniform 但**阴影关闭**；透明 / 粒子 / 贴花不进捕获；探针自身
  不参与捕获（捕获绑定组绑**零参数 + 1×1 哑 cubemap**，避免「反射里看到
  反射」的自反馈）。
- **逐片元选择裁决**：着色器逐探针算权重、取**严格更大**者 ⇒ 权重并列时
  先出现者胜出；uniform 数组由 CPU 按**实体位模式升序**填充，故并列裁决
  = 位模式小者（全序，与查询 / 插入顺序无关，R61 选择键同款纪律）。
  **不做多探针加权混合**（拒绝清单）。
- **预算与上传**：`MAX_REFLECTION_PROBES = 4`（cubemap 数组 24 层 × 5 级
  mip）。CPU 侧按（相机到影响域距离升序, 实体位模式升序）取预算内一批
  （`probe::select_active_probes`），超预算 `log::warn`。**未烘焙的探针不
  进 uniform** ⇒ 与没有探针逐位一致。
- **uniform 不扩 Globals**：探针参数走**独立缓冲 + 绑定 9**（144B：
  `count` + 4 × 32B），cubemap 数组走**绑定 10**（CubeArray，采样复用
  binding 6 的 Filtering 采样器）。R61 的并行布局陷阱（`skybox.wgsl` 自持
  `Globals` 前缀声明）因此完全不被触碰——`csm`（binding 8）之后新增绑定
  不需要同步天空盒。`ProbeParams.count.w` = 视差修正开关（1 = 生产路径，
  0 = `Renderer::set_legacy_probe_parallax` 的对照模式，验收图与归因专用）。
- **存储形态（ProbeStore）**：GPU 侧 = 一张所有探针共享的 cubemap 数组
  （槽位按首次烘焙调用序分配，`BTreeMap` 登记实体位 → 槽位，重复烘焙幂等、
  预算耗尽返回 `None` 并 warn）；**没有 CPU 侧像素数据**（捕获 / 预滤波
  全程在 GPU 上完成，回读只会引入昂贵同步与无损往返问题），故不做
  `LightmapStore` 那样的 World 资源——登记表与 GPU 资源同生命周期放在
  渲染器内部，避免「World 说已烘焙、Renderer 没有纹理」的撕裂。
- **光照贴图协同（闭合 §8.31④）**：挂光照贴图的实体其间接**漫反射**走
  贴图、**镜面**走探针——着色器在 `select(ibl_env, ibl_lightmap, mask)`
  之后追加一项 `spec × (F·LUT.x + LUT.y) × env_intensity × ao`，且**只在
  「有图集 且 有生效探针」时执行**。无探针时该项恒不执行 ⇒ 光照贴图实体
  仍然没有环境镜面（R52c 语义与逐位兼容不变）；无图集时镜面已含在
  `ibl_env` 里，不重复叠加。
- **缺省路径逐位一致（兼容铁律）**：无 `ReflectionProbe` / 探针未烘焙 /
  片元在域外 ⇒ `spec` 保持 `env_spec` 的**同一串位**（`select` 是纯值选择，
  分支不改变 env_spec 的计算），且无探针时一次 `write_buffer` 都不发
  （`probe_params_active` 记账，探针退场那一帧才回写零值）。全部既有 GPU
  FNV 基线原值通过（R62a 不重锁）。
- **验收图**：`docs/reflection_probe.png`（4 排 × 2 列：室内盒场景探针
  关|开 / 视差修正 关|开 / 粗糙度 0.1|0.8 / 光照贴图场景探针 关|开），
  由 `examples/reflection_probe_snapshot.rs` 生成（内含硬断言）。

**v1 不做（诚实清单 → §8.37）**：多探针加权混合；探针磁盘持久化
（重捕获确定性 + 启动耗时可控优先）；动态探针（每帧重捕获）；平面镜
planar reflection；探针编辑器集成（R58b 烘焙入口的扩展留游戏工具轮）。
SSR 原列在本清单，R62b 已落地——见 §4.31（探针是其未命中回退通路）。

**C2 编辑器入口与序列化**：探针布局参数经关卡档 `probe` 段入档
（`half_extents` / `falloff`，退化半边长加载期拒绝）；捕获内容维持
「每次启动重烘」不持久化。摆放 / 步进 / undo 语义见 §4.27 探针入口段。

### 4.31 SSR 屏幕空间反射（前向片元内步进 × 上帧颜色/深度，R62b）

探针（§4.30）是静态烘焙的：后摆进场景的物体、移动的物体、屏幕里才有的
内容都进不了 cubemap。SSR 在**前向片元着色内**对**上一帧**的颜色与深度
做屏幕空间射线步进——命中 ⇒ 反射屏幕内内容（含一切动态物体）；未命中
⇒ 回退探针 / env（R62a 通路原值）。

```text
主 pass 片元（standard.wgsl fs_main）：
  反射方向 = reflect(−v, n)（与探针/env 镜面同一 spec_dir）
  → 世界空间线性步进（32 步 × max_distance）+ 命中后二分细化（6 轮）
  → 逐样本投影到上帧屏幕（上帧实际渲染矩阵）：UV + clip.w（线性视距）
  → 与上帧场景深度副本比较（穿越 + 厚度容差 ⇒ 命中）
  → 采样上帧主解析缓冲帧尾副本 ⇒ 按「粗糙度衰减 × 边缘衰减」混入镜面项
```

**六项设计裁决**：

1. **采样位置 = 前向片元内步进**（对上帧深度 + 颜色）。本引擎无
   GBuffer——后处理式 SSR 拿不到逐像素粗糙度 / 法线 / 反射方向，前向
   片元内步进是架构正解：`fs_main` 手握全部输入，步进只是多一段纯函数
   计算；代价是每个参与片元付 32 次 mat4 乘 + textureLoad（性能段见
   下），且步进发生在**全分辨率逐片元**（无半分辨率降采样，拒绝清单）。
2. **上帧资源**：上帧颜色 = 主 HDR 解析缓冲的**帧尾副本**（帧尾一次
   `copy_texture_to_texture`，resolved 纹理加 `COPY_SRC` usage）；
   上帧深度 = **复用现有单份 scene_depth 纹理，不做 ping-pong**——
   **对规格的偏离声明**：规格预设「单份变双份」，但本引擎的
   scene_depth pass 编码在主 pass **之后**（阴影 → 主 pass →
   scene_depth → DOF/MB/TAA），同一 CommandEncoder 内按序执行 ⇒ 主
   pass 的 SSR 读到的**必然**是上一帧写入的内容（读后写由 wgpu 的
   hazard 跟踪在 pass 间插屏障保证），单份纹理天然就是「上帧深度」，
   变双份是 1080p 下 ~16 MiB 的纯浪费。语义与 ping-pong 逐位等价
   （每帧 Clear + 全量覆写）。**不与 TAA 历史共用**：TAA 历史是「抖动
   色 × 邻域钳制 × 0.9 历史」的时域混合产物，不是任何一帧的真实辐亮度
   ——拿它当反射源会给反射注入阻尼拖尾与抖动残留，且 TAA 关闭时无
   历史可用；主解析缓冲才是「帧」的定义。RT 相机与探针捕获的绑定组
   绑**零参数 + 1×1 哑纹理**（`SsrGpu` 常驻哑资源，探针的哑 cubemap
   同款）——「上帧」语义只对主相机成立。
3. **重置语义**：首帧启用 / 关→开切换 / 分辨率变化（任一资源重建）/
   切镜（`reset_taa()` 跟随）/ 上帧或本帧无不透明几何（深度副本没跑）
   ⇒ `ssr_prev_valid = false` ⇒ 该帧 uniform `flags.x = 0` ⇒ 着色器
   整段跳过 ⇒ **输出与 SSR 关闭帧逐位一致**（GPU 硬断言；重置帧帧尾
   照常副本，下一帧恢复）。**与 reset_taa 的关系 = 跟随**：reset_taa
   的语义是「宿主宣告切镜」，切镜让一切跨帧时域状态过期——TAA 历史
   与 SSR 的上帧颜色 / 深度同理（上帧是另一个场景）；跳过一帧的成本
   是切镜帧的反射回退探针 / env，收益是杜绝「新场景地板反射旧场景」
   的一帧闪现。`FrameStats::ssr_on` 观测字段（1 = 本帧实际步进）。
4. **分叉语义（单一来源公式，`standard.wgsl` 与 `post.rs` 各持一份
   同式实现，CPU 侧单测锁曲线）**：
   ```text
   spec_refl_w = spec × (f_amb·brdf.x + brdf.y)      // 既有探针/env 镜面项（R62a 通路）
   w_ssr       = roughness_fade(r) × edge_fade(uv) × 强度 × [命中]
   roughness_fade(r) = clamp((CUT − r)/(CUT − FLOOR), 0, 1)，FLOOR = 0.25，CUT = 0.6
   edge_fade(uv)     = clamp(min(u.x,1−u.x)/M) × clamp(min(u.y,1−u.y)/M)，M = 0.1
   ssr_spec    = prev_color(uv_hit) × f_amb           // BRDF 权 = 纯 Fresnel
   spec_refl_w ← mix(spec_refl_w, ssr_spec, w_ssr)    // 命中才执行
   ```
   合成次序：SSR 在「env vs 探针」选择（R62a 的 `spec`）**之后**、乘
   BRDF 权重处替换——一条 `mix`，探针 / env 是未命中与部分权重的底；
   env 权（split-sum `(f_amb·brdf.x + brdf.y)`）给既有通路，SSR 权
   （纯 `f_amb`）给单根镜像射线（无 lobe 可积分）。**粗糙度分层**：
   `r ≥ CUT (0.6)` 着色器早退、整段不步进——依据：GGX lobe 半角在
   r=0.6 已达 ~40°，单根镜像射线无法积分该宽度的 lobe，而上帧颜色
   无预滤波 mip（直接采样是噪声）；该段探针 / env 预滤波（同一
   `roughness × max_mip` 链）已是正确估计，步进纯浪费（粗糙面常是
   屏幕大多数像素）。`r ≤ FLOOR (0.25)` 全量权重，中间线性下降。
5. **步进算法**：世界空间反射射线 `p(t) = world_pos + spec_dir·t`，
   `SSR_STEPS = 32` 等分线性步进（首样本 t = 半步长防自命中），
   `SSR_REFINE = 6` 轮二分细化。常量依据：默认 `max_distance = 12 m`
   ⇒ 步长 0.375 m；相邻样本的视距变化 ≤ 步长（方向单位向量）⇒ 任何
   真实穿越在首个越界样本的穿透 ≤ 0.375 m < 默认 `thickness = 0.5 m`
   ⇒ 不漏判；6 轮二分把命中精度收到 0.375/2⁶ ≈ 6 mm（256 px 宽画面
   3 m 距离下 ≈ 半像素——解析镜像落点断言的像素级对齐由此保证）。
   出屏（含 `w ≤ 0` 上帧相机背后）即终止为未命中。深度解析与 MB 同源
   ：比较在**线性视距**域进行（scene_depth 存 clip.w，步进样本取
   `prev_view_proj` 的 clip.w——同一条 DirectX 透视约定的量，无需
   NDC z 重建）。**全程确定性**：无 RNG、无导数依赖（textureLoad +
   命中后一次 `textureSampleLevel` 显式 mip），步进算术逐位可复现。
6. **与 MSAA**：深度副本是单采样解析语义（既有），SSR 按逐像素单深度
   步进——MSAA 在几何边缘的亚像素遮挡结构在步进深度里不存在，边缘
   处可能命中 / 漏判偏差（诚实清单 §8.38⑤）。上帧颜色同理取自解析后
   的单采样缓冲（反射内容天然是解析后的）。

**兼容铁律（缺席 / 关 = 零开销默认路径）**：`SsrSettings` 资源缺席或
`enabled = false` ⇒ 不分配上帧颜色副本、条件链不扩展（scene_depth 的
编码条件照旧）、不发 SSR 参数 write_buffer（退场那一帧才回写零值，
探针 `params_active` 同款记账）、着色器 `flags.x = 0` 早退 ⇒ 与 R62a
逐位一致。binding 11/12/13 走 R61/R62a 的「不扩 Globals 前缀」扩容
先例（追加在最大 binding 之后，skybox.wgsl 的并行布局不受触碰），哑
资源常驻保证绑定组合法。全部既有 GPU FNV 基线原值通过（本轮不重锁）。

**性能（480×270 离屏实测，release）**：帧时 0.122 → 0.347 ms（步进
+ 帧尾副本 ≈ +0.23 ms；室内小场景、约半屏片元参与步进）；帧尾副本
带宽 = 2 × W × H × 8 B（1080p ≈ 33 MiB/帧读写）；显存增量 = 上帧颜色
副本 W×H×8 B（1080p ≈ 16 MiB）+ 1×1 哑纹理与双 96B 参数缓冲。验收图
`docs/ssr.png`（4 排 × 2 列：金属地面 关|开 / 动态物体 关|开 /
粗糙度梯度 关|开 / 屏外回退 关|开），由 `examples/ssr_snapshot.rs`
生成（内含硬断言，跨运行 FNV 逐字节一致）。

**v1 不做（诚实清单 → §8.38）**：半分辨率 SSR；hi-z 加速结构；
stochastic SSR / 蓝噪抖动采样；SSR 专用时域滤波（TAA 已在链首）；
逐物体反射捕获；湿面 / 水坑专用模型。

### 4.32 预积分皮肤次表面散射（R63）

皮肤在终结线（terminator）处是硬边漫反射——光在皮肉里的穿透被完全忽略，
背光侧一刀切黑。R63 给写实角色的肤质材质一条**预积分次表面散射**通路
（Penner 2011「Pre-Integrated Skin Shading」近似的引擎实现）：**程序化
预计算一张 (NdotL × 曲率尺度) 的漫反射 LUT**，肤质材质的**直射漫反射**
用它取代几何 `NdotL` 因子——光「包裹」进终结线背光侧。
**无屏空间模糊、无多 pass、无厚度图、无透射**（拒绝清单见本节末）。

**积分式（单一来源 = `iris_render::sss`）**：把像素覆盖的表面块建模为
**球冠法线分布**（法线均匀落在半顶角 α 的锥内，按表面积测度
`sin a · da · dφ` 加权），该像素的 Lambert 响应 = 球冠上投影面积加权的平均：

```text
N(c, α) = ∫₀^α ∫₀^{2π} max(c·cos a + √(1−c²)·sin a·cos φ, 0) · sin a dφ da
D(α)    = ∫₀^α ∫₀^{2π} cos a · sin a dφ da = π·sin²α
W(c, t) = N(c, α(t)) / D(α)      ← LUT 存的就是 W
α(t)    = t · π/2                 ← t = 归一化曲率包裹参数（0 = 平面）
```

方位角积分有**闭式解**（分段：`|a| ≥ b ≥ 0` ⇒ `2π·max(a,0)`；`|a| < b` ⇒
`2(a·φ₀ + b·sin φ₀)`，`φ₀ = acos(−a/b)`），只剩极角一维数值积分（复合
Simpson 128 步；与 20000 步参考的最大偏差 9.0e-7，比 LUT 插值误差小三个
数量级 ⇒ 步数不是精度瓶颈）。**积分在 f64 里累加、末端落 f32**：f32 累加
129 个量级 1 的项会攒出 ~1e-7 的求和噪声，足以在「对 c / 对 t 单调」这条
解析性质上制造亚 ulp 反向抖动（实测 1.19e-7）。**无 RNG、无全局状态**——
采样确定性照 `bake.rs` 的无 RNG 文化，同输入两次生成逐位同一输出。

**归一化选型（按 `D(α)` 归一 = 以 c = 1 的响应为基准）**：不归一的话
全照面会随曲率变暗（球冠平均 `⟨cos a⟩ < 1`），「开 SSS = 整张脸变暗」是
调参灾难。归一后 `W(1, t) ≡ 1`：**strength 只在终结线及其背光侧加光**，
受光面观感与关闭时一致，A/B 对照干净。代价是终结线附近**增能**（非严格
能量守恒）——这是预积分 SSS 近似的既有取舍（Penner 2011 同样如此），
进诚实清单。

**LUT 布局与精度**：`Rgba16Float` 64×64 常驻纹理（渲染器初始化时一次
生成上传），行 = 曲率 t、列 = NdotL c，RGB 同值（LUT 本体可当图看）、A = 1。
**纹素中心对齐**（`c_i = −1 + 2i/(N−1)`、`t_j = j/(N−1)`，采样坐标
`(idx + 0.5)/N`）⇒ 端点 `W(−1) = 0 / W(1) = 1` **逐位精确**，不会出现
「全照面被 LUT 抹暗 1.6%」的端点截断伪影。64 的精度依据（双线性重建 vs
解析式实测）：`t ≥ 0.05` 时 ≤ 1.4e-3（低于 8-bit 输出量化步长 3.9e-3）；
`t → 0` 的 Lambert 折点处 ≤ 7.9e-3（折点不可导，线性插值 O(h) 是下界）。
选 `Rgba16Float` 而非 `Rgba8`：binary16 相对精度 4.9e-4，比 8-bit unorm
细 8 倍——平缓的终结线斜坡用 8-bit 会出现 1/255 量化台阶（banding）；
且中性法线纹理（R42）与光照贴图（R52c）已确立「线性数据 / 低频信号走
Rgba16Float」的先例（核心可过滤格式，无扩展依赖）。

**作用面 = 只改直射漫反射**（六条边界逐一裁决）：

1. **镜面项留在几何 NdotL 上**——皮下散射是漫反射现象，高光仍遵守几何
   反射定律。实现形态是**相加的补项**而非把 `nol` 替换进 `pbr_light`：
   `sss_add = (1 − metallic) · albedo/π · radiance · (nol_sss − nol)`，
   `nol_sss = mix(nol, W, strength)`（strength ∈ [0,1] 线性混合）。
2. **金属无 SSS = 权重而不是分支**：能量权重 `(1 − metallic)`，metallic = 1
   ⇒ 权重 = 0 ⇒ 补项恒 ±0.0。选权重不选分支的理由：不多一条可能与
   pbr_light 失同步的金属判定，且 GPU 测试能直接断言「metallic = 1 时
   strength 0/1 **整帧逐位一致**」。
   **为什么不用 `kd = (1 − F)(1 − metallic)`（R63 实测教训，写进 §8.39）**：
   初版按 `kd` 写，需要在补项里重算 `h = normalize(v + l)` 与
   `fresnel_schlick(voh, f0)`——与 `pbr_light` 完全重合的子表达式**改变了
   编译器在 `pbr_light` 内的公共子表达式消除 / 内联决策，进而改变其浮点
   收缩（FMA）选择**：实测 `tests/shadow_v2_render.rs` 的接触阴影对照场景
   有 **3 个字节 ±1 LSB** 漂移（320×180×4 中的 3 个通道，全在接触阴影边缘
   ——PCF 比较的临界像素）。该漂移与 SSS 逻辑无关（strength = 0 时补项不
   执行），是着色器代码增量的编译期副作用；去掉子表达式复用后该基线
   **逐位复原**（实测）。代价：绝缘体漫反射权重比 `kd` 高约 4%（F0 = 0.04
   起），对近似补项在噪声量级。**新增直射项时不要在辅助函数里重算
   `pbr_light` 的子表达式。**
3. **点光同样适用**：终结线软化是方向无关的漫反射现象；`pl` 已含距离窗口
   衰减与 cubemap 阴影，补项自动继承两者（阴影内 / 有效半径外为 0）。
   成本 = 每盏生效光源多一次 64×64 LUT 采样（显式 mip、常驻缓存）。
4. **IBL / 光照贴图间接项、自发光、AO 一概不参与**——它们不是「直射
   漫反射」。SSS 不引入任何新的间接光通路。
5. **透射（耳朵透光）/ 厚度图明确不做**（拒绝清单）——本模型建模的是
   **法线分布**导致的终结线软化，`NdotL = −1`（完全背光）响应恒为 0。
   **LUT 生成成本**：单次 64×64 生成 18.8 ms（debug 实测），渲染器初始化
   的一次性开销；刻意不做记忆化缓存（纯函数 + 本仓对全局状态的审慎态度）。
6. **不做屏空间 SSS**（separable SSS 模糊多 pass）：与前向管线的链结构
   冲突，且需要额外的模糊 pass 与深度支撑。

**阴影协同（乘位论证，必须正确）**：补项与主体项**消费同一个已乘完阴影的
辐亮度**——方向光是 `light_color × shadow_factor`（CSM + 3×3 PCF），点光
是 `light_color × atten × shadow`（cubemap 单采样）——不存在第二条绕过
阴影的通路。投影阴影内 radiance → 0 ⇒ 补项 → 0：**背光侧的包裹绝不
「晒穿」投影阴影**。点光 cubemap 阴影是**硬 0**（`select(0,1,·)`）⇒ 阴影区
像素与 strength **逐位无关**，GPU 硬断言；CSM 的阴影暗部保留系数
（`SHADOW_STRENGTH` = 0.45 ⇒ 全影 = 45% 光照）使直射不为 0，「贡献 = 0」
的逐位断言落在点光路径上，CSM 侧以「增量被同一阴影因子缩放」的断言覆盖
（见测试文件注释与 REPORT 的偏离声明）。

**光照贴图协同**：挂光照贴图的实体其间接项走 R52c 的 `select` 分支，SSS
补项只进直射项 ⇒ 两条路径（有 / 无图集）下 SSS 的**几何行为一致**——
GPU 测试用终结线指标（背光侧受光像素数、亮度零点位置）对照，实测零点
位置差 1 px（127 → 34 vs 127 → 33）。

**参数语义与调参指引**（`MaterialDef::subsurface_strength` /
`subsurface_scale`，缺省 0 / 0.1 m）：

| 部位 | `scale` (m) | t | α | 观感 |
|---|---|---|---|---|
| 手指 / 鼻尖 / 耳缘 | 0.02 | 1.00（饱和） | 90° | 强烈包裹，接近半透明 |
| 头 / 前臂（缺省） | 0.10 | 0.50 | 45° | 终结线明显软化 |
| 躯干 / 大腿 | 0.30 | 0.17 | 15° | 轻微软化 |
| 大面积平坦皮肤 | ≥ 0.50 | ≤ 0.10 | ≤ 9° | 近乎经典 Lambert |

`strength` 是 0..1 的线性混合（0 = 关闭、1 = 漫反射完全取 LUT 值）；
`scale` 是曲率尺度半径（世界单位 / 米，**越小越弯曲、包裹越强**），
CPU 侧换算成 LUT 的 v 坐标（`sss::wrap_param` ⇒ `t = clamp(0.05/scale, 0, 1)`），
片元侧不做除法。非法尺度（非有限 / ≤ 0）保守退化为 t = 0（不做包裹）。

**兼容铁律（缺省 = 关闭 = 逐位一致）**：`subsurface_strength = 0` 是全部
既有材质的缺省，着色器的 SSS 分支 `if (material.sss.x > 0.0)`（uniform
条件 ⇒ 无发散）**整段不进入——一个算术都不多做**。这比 `mix(a, b, 0)` 的
浮点恒等式论证更强（后者在 `a = −0.0` 时翻符号），是 R52c `select` 先例
的强化形态：「把逐位不变交给语言语义，而不是浮点恒等式」。全部既有 GPU
FNV 基线原值通过（本轮不重锁）。

**实现位置与绑定**：LUT 生成 / 采样镜像在 `iris_render::sss`（纯函数 +
单测）；渲染器初始化时生成并上传常驻纹理，绑定在**材质组 binding 7**
（追加在现有最大 binding 之后，R61/R62a/R62b 的「不扩 Globals」先例），
采样复用 binding 6 的 ClampToEdge 双线性采样器（LUT 坐标恒在 [0,1]，
Repeat 会在端点混入对侧纹素）。`MaterialParams` 扩一个 `vec4 sss`
（x = 强度、y = 曲率包裹参数），Rust 侧 `MaterialRaw` 同步（64 B，
`size_of` 编译期断言）。关卡档：`MaterialSection` 加两个**可选**字段
（缺省值省略 ⇒ v2 旧档往返逐字节不变，`SCENE_FORMAT_VERSION` 保持 2）。

**测试与验收**：9 项 LUT 纯函数单测（逐位可复现 / 端点解析对照 / 零点
`−sin α` 解析对照 / 双轴单调 / 方位角镜像对称 / 二维中点法独立参考对照 /
重建误差上界 / 调参表数字）+ 6 项 GPU 集成（R63 基线 FNV 锁 / 终结线三指标 /
金属逐位零贡献 / 点光阴影逐位无关 / 光照贴图双路径一致 / 双会话逐帧逐位）。
验收图 `docs/sss.png`（5 排 × 2 列：终结线 关|开、尺度 0.05|0.10、
尺度 0.30|LUT 本体、阴影区 关|开、人形角色 关|开），由
`examples/sss_snapshot.rs` 生成（内含硬断言，跨运行逐字节一致）。

**v1 不做（诚实清单 → §8.39）**：屏空间 SSS（separable SSS 模糊 pass）；
透射 / 厚度图 / 背光穿透光晕；各向异性高光；头发 / 布料专用模型；
逐顶点曲率烘焙（`scale` 是逐材质常量）；LUT 运行时热替换。

### 4.33 光照探针 GI（烘焙 irradiance 探针云 + 四面体重心插值，R64）

**互补于 §4.32 的漫反射通路**。光照贴图（§4.24）只服务静态几何、反射探针
（§4.30）/ SSR（§4.31）只服务镜面——动态物体（角色 / 掉落物）的**间接漫反射**
没有任何来源，只能吃全局 IBL 的天空辐照：室内阴影里的角色要么被天空色污染成
冷蓝，要么在封闭房间里亮得不合理。R64 补上这一格，合成心智模型至此闭合：

`text»
静态漫反射 = 光照贴图（逐 texel 烘焙辐照，§4.24c）
动态漫反射 = 光照探针（逐探针烘焙辐照 + 四面体重心插值）   ← §4.33
镜面       = 反射探针（域内）→ SSR（屏幕内）→ 全局 env（回退）  ← §4.30 / §4.31
`text»

- **模块**：`iris-render/irradiance»（纯 CPU：烘焙、四面体化、插值，零 GPU 依赖）。
  公开面 = `IrradianceProbe» 组件 + `bake_irradiance_probes» /
  `install_irradiance_probes» + `IrradianceProbeSet» 资源 + `Tetrahedralization»
  纯函数（构建 / 定位 / 插值 / 结构自检）。
- **四面体化 = 增量 Bowyer-Watson 空腔重三角化**（自研、零依赖）。选它而不是
  「提升 4D 凸包」的理由：两者数学等价（空腔 = 4D 凸包的可见面集合），但空腔
  形态下判定全是 3×3 / 4×4 行列式，4D 凸包要维护 4 维单纯形表、出错面大得多；
  探针预算 256（`MAX_IRRADIANCE_PROBES»）下 O(n²) 暴力定位在烘焙期毫秒量级。
  关键工程要点（都是实测踩出来的）：
  1. **空腔必须取「与 p 所在胞连通」的冲突分量（BFS）**：不加约束时，远处胞也
     可能把 p 包进自己的外接球，把它们算进空腔会让「腔对 p 星形」失效 ⇒ 新胞
     朝向相反、结构空洞（症状：面被三个以上胞共享）；
  2. **凸包外的点按「幽灵胞元」接入**（凸包面 ⟷ 无穷远胞元，与 CGAL 同构）：
     可见凸包面（`orient3d(面, p) < 0»）进空腔，反向面序与 p 连成新胞；
  3. **初始单纯形取「最胖四点」贪心**（v0 = 距中心最近、v1 = 最远点、v2/v3 =
     距直线 / 平面最远）：字典序最早四点在做网格布局下近乎共线，初始胞是「针」
     ⇒ 第 24 次插入就把复合体弄坏（实测）；
  4. **确定性抖动**（`jitter»，±包围盒 1e-6 的三线性哈希抖动）：精确共面 /
     共球是真实退化（共面边界小面与 p 连成的「新胞」体积恰 0 ⇒ 朝向判定恒不过），
     抖动把退化推开；三条性质是构造保证——整数哈希 ⇒ 确定性、三线性 ⇒ 连续性、
     同坐标同抖动 ⇒ 探针点处重心权重 1/0/0/0 **逐位**；代价是几何为「真值 ±1 µm」，
     规则的网格 / 立方体布局的凸包组合结构可能与理想构型不同（任何扰动法都有）；
  5. **抖动幅度阶梯重试**：极扁平布局（探针全部共面且点数很多）在 1e-6 档仍可能
     落进退化区，插入期的拓扑自检（面共享计数 / 冲突集连通性 / 局部撞面）当场
     发现 ⇒ 换 1e-4 / 1e-2 档重建；全部失败 ⇒ 空四面体化 + `degenerate» 标记，
     查询整体回退全局 env（确定性：阶梯固定 ⇒ 同输入同结果）。
- **点定位与插值**：世界坐标 → 包围盒规范化 + 同幅度抖动 → 逐胞「四面的分子
  全非负」+ AABB 早退 → 重心权重 `num/denom»（每个面的分母与分子**同一次
  计算** ⇒ 顶点处权重恰为 (0,0,0,1)）。贴边点先按相对松弛 1e-9 兜底（否则会
  穿透所有胞掉进包外投影、取值跳变——实测凸包角落 0.07 m 步长跳 0.077）。
  **法线无关性**：权重表与取值表都按**顶点序**索引（`tet_faces_vertex_order»；
  用面序索引会把权重接到错误的顶点上——线性场复原测试当场抓住）。
- **包外语义 = 最近凸包面投影（重心钳制，无分支）**：选它而不是「回落全局 env」
  的理由是**连续性**（凸包面上内外同值 ⇒ 角色走出覆盖区不跳变）；实现刻意用
  「平面投影 + 负重心置零归一化」而不是 Ericson 的分区判定——后者在「点几乎
  落在三角形平面上」时会因舍入在分支间抖动（实测把凸包角落的查询判成「最近点
  是三米外的顶点」）。限制：远离探针云的实体一直吃边界辐照（无距离衰减）。
- **烘焙源 = 复用 `bake_sample»**（与光照贴图同一 BVH / 同一低差异采样 / 同一
  三项口径），存 `BakeSample::indirect»（天空含 AO + 一次反弹含色溢）——
  与光照贴图**完全同口径**，直接光仍由运行时 CSM / 点光提供（否则双计）。
  **探针朝向 = 六轴平均**（±X/±Y/±Z 各烘一次取算术平均）：v1 探针是无方向
  RGB 标量，物理上自洽的取法是辐照度函数的方向零阶平均，六轴平均对均匀场
  精确（= π·L）；代价 = 6× 烘焙成本；局限 = 强方向性场下亮面能量被摊到暗面。
- **求值 = 逐实体 CPU 侧**（实体根 `GlobalTransform» 位置 → 一次插值 → 实例载荷
  `InstanceRaw::probe» = xyz 存 E/π、w = 标志位）。**拒绝逐片元 GPU 插值**：
  逐片元要上传整张四面体表并逐像素搜索（10⁵ 量级像素 × 搜索 vs 每实体一次），
  成本差 4–5 个数量级，而箱庭尺度的角色 / 掉落物本来就是逐实体常数。代价：
  角色自身没有空间梯度（头脚同一份辐照）、大体积动态物体需要逐片元（§8.40）。
- **语义边界（六条）**：①挂光照贴图的实体不动（CPU 侧直接不发标志位 + 着色器
  `select» 掩码优先，两道保险）；②无贴图实体由探针插值**取代全局 env 的漫反射
  辐照**；③镜面一概不碰（`spec_refl_w» 的计算与探针无关）；④SSS（§4.32）是直射
  项，与间接项正交；⑤包外 = 最近凸包面投影；⑥实体增删不影响探针数据（探针集是
  只读烘焙产物，角色出生 / 死亡不改动它）。
- **注入载体 = `InstanceRaw» 扩一个 `vec4 probe»**（128 B/实例，顶点属性 loc14）：
  载荷存 **E/π**（与 `irradiance_map» 采样值同口径）⇒ 着色器零换算，
  `let irradiance = select(env_irradiance, in.probe.rgb, in.probe.w > 0.5);»。
  **兼容铁律**：探针资源缺席 / 空集 / 实体挂图集 ⇒ 载荷恒 `[0, 0, 0, 0]»，
  `select» 返回纹理采样值的**同一串位**（纯值选择、不做算术——R52c 先例）。
  显存增量 = 16 B/实例（112 → 128 B）。
- **存储 = World 资源**（`IrradianceProbeSet»）而不是渲染器内部：R62a 的反射探针
  把登记表放渲染器里是因为产物**是 GPU 资源**；本轮全程 CPU、产物是纯数据，
  放 World 没有第二份副本、没有撕裂问题，渲染器每帧从 `&World» 只读它。
- **验收图**：`docs/light_probes.png»（4 排 × 2 列：①室内阴影区角色 关|开
  ②亮区|阴影区 ③与静态光照贴图的融合（近景）关|开 ④探针布置可视化），由
  `examples/light_probe_snapshot.rs» 生成（内含硬断言 + 跨运行逐字节一致，
  实测面板 1：角色亮度 200.5 → 143.8（**72%**）、蓝红偏置 22.5 → 19.0 并向
  房间参考 11.2 收敛、角色外像素变化 **0**）。

**实测（release，i7-13620H，2026-09-11）**：18 探针 × 6 轴 × 96 半球样本 +
1 反弹的烘焙 + 四面体化合计 **11.6 ms**（单探针 6 轴烘焙 ≈ 0.6 ms、18 点
四面体化 53 胞 ≈ 0.2 ms）；每帧逐实体插值 < 5 µs/实体（AABB 早退后平均只
过 1–2 个胞）；CPU 内存 ≈ 7.6 KiB（18 探针）/ ≈ 150 KiB（256 探针满载）；
显存 +16 B/实例。

**测试**（irradiance.rs 23 项 + tests/irradiance_render.rs 6 项）：谓词符号约定
与四面体面序 / 抛物面行列式的球内外与共球零点（含立方体八点全组合扫描）；
立方体共球构型两跑逐位 + 欧拉特征 1 + 体积和；3×3×3 网格结构自检；随机云
（5/17/64/150 点）局部 Delaunay + 流形 + 欧拉；**输入顺序置换不改四面体表**；
重合点合并与非有限点记账；退化输入（< 4 点 / 全同点）空结构；共面点云薄板
可用性；**探针点处取值逐位还原**；权重非负且和为 1；线性场解析复原；共享面
两侧一致（1e-4 相对容差，实测 1.6e-6）；包外投影（面心解析 / 距离无关 /
跨边界连续）；凸包面对所有探针内向；**探针值与直接调用积分器逐位一致**；
烘焙两跑逐位；开阔天空 π·sky_color 解析；GlobalTransform 与退化探针记账；
预算截断；实例载荷标志位；装配报告与资源替换；内存口径。**GPU**：R63 全帧
FNV 基线两条（无贴图 / 有贴图）原值通过；只挂光照贴图的场景装探针后逐位不变；
阴影区角色变暗 + 色相向房间收敛 + 多数像素变暗；亮区 / 阴影区插值跟随位置；
双会话 5 帧逐帧逐位（角色移动穿过探针网格）+ CPU 侧路径连续性。

### 4.34 GTAO 环境光遮蔽（切片水平角 × 上帧深度副本，R77）

**SSAO 是屏幕空间时代的接触阴影**：ORM 的 `ao`（R41）只描述材质级小尺度
遮蔽，光照贴图的天空 AO（§4.24）只覆盖静态烘焙几何——动态物体（角色 /
箱体）落在墙角、柱脚、箱堆时的**环境光互遮蔽**没有任何来源。R77 以
GTAO（Ground-Truth Ambient Occlusion，Activision 2016 的简化变体）补上：
主 pass **之前**的独立全屏 pass 读**上一帧**主相机深度副本（§4.43 的
scene_depth，与 SSR 同款「上帧」语义——同编码器内按序执行，本 pass 读到
的恒为上一帧内容），估算逐像素环境可见度写入半分辨率 R16Float 图；
`standard.wgsl` 在主 pass 按屏幕坐标采样后乘在**间接光**上。

**算法（`shaders/ao.wgsl`，每像素）**：① 由（纹素中心 uv, 线性视距）重建
视空间位置（`post::ao_view_pos`，CPU 镜像经 glam 透视正反算互锁）；② 法线
= ±1 纹素深度差分重建位置的叉积；③ 4 切片绕视轴均匀展开、方向按像素整数
哈希 dither 旋转（`post::ao_dither` 逐行镜像），每切片正负两方向各 8 步
世界半径搜索记水平角正弦最大值；④ 遮蔽 = 切片平均 0.5·(sinθ↑ + sinθ↓)，
可见度 = (1 − 遮蔽)^intensity。sin 和简式在法线 ≈ 视线方向时是精确的
余弦加权积分；掠射角低估遮蔽（§8.46）。

**乘点与 ORM ao 同靶（只间接光）**：`ibl_final`（IBL / 光照贴图 / 探针
镜面）与常量环境项乘 `ao_ss`；直接光（方向光 / 点光）不乘——「AO 描述
小尺度互遮蔽的环境光损失」是 R41 既有语义的屏幕空间扩展。

**兼容形态（缺席 = 逐位不变，R61/R62a/R62b 扩容先例）**：globals 组
binding 14 = 可见度图（排在 SSR binding 13 之后，**不扩 Globals 前缀**，
skybox.wgsl 并行布局零触碰）；`AoSettings` 缺席/关闭时不分配可见度图、
不编码 pass、binding 14 回落常驻 1×1 R16Float 哑纹理——**纹素恰为
1.0（f16 0x3C00）**，着色器无条件采样后 `× 1.0` 由 IEEE 754 逐位精确，
零分支实现零开销默认路径（全部既有 GPU FNV 基线原值通过）。捕获面 /
RT 相机同样哑纹素（屏幕空间语义只对主相机成立，§4.31 裁决 2 同款）。

**重置语义**：可见度图依赖上一帧深度。`ao_depth_frame` 记账深度副本最近
运行帧号——本帧编号恰好 +1 才编码 GTAO，否则只清中性 1.0（首帧启用 /
上帧无几何 / 副本重建帧输出与关闭帧逐位一致，`FrameStats::ao_on` 观测）。

**两个实现级归因（R77 实测教训，测试锁定）**：
1. **半纹素错配**：样本位置若用连续 uv 配 floor 后的纹素深度，斜面上
   样本被抬离/压入表面 ~1e-2 量级（半径 0.2 m 时假遮蔽 5%）——重建
   一律取深度纹素中心，平面样本仰角精确归零；
2. **screen uv 不得走顶点 varying**：`clip.x/w` 是投影量，地面/墙等大
   三角形常有顶点在相机背后，近平面裁剪后新顶点的 x/w 爆炸，插值出
   越界坐标把可见度图采样扭曲成大片假遮蔽（实测梯度注入定位）——改用
   `@builtin(position)`（片元输入语义 = 帧缓冲像素坐标，纯仿射）除以
   `textureDimensions(ao_tex)×2`；哑纹理路径值恒 1.0 与坐标无关，
   兼容性不受口径影响。奇数分辨率下右/下边缘 ≤1 像素口径差（§8.46）。

**测试**（`tests/ao_render.rs`，GPU_SERIAL 串行化）：缺席 = 显式关逐位；
启用首帧（上帧无副本）= 关闭帧逐位 + `ao_on` 0→1；角落严格变暗 /
开阔地容差内不变且暗化差可判别；强度指数单调；双会话含物体运动逐帧
逐位。CPU 镜像纯函数（`post::{ao_dither, ao_view_pos, ao_uv_offset,
ao_sample_sin, ao_falloff, ao_slice_occlusion, ao_visibility}`）含重建
公式与 glam 透视的正反算互锁。验收图 `docs/ao.png`（三排 关|开 对照：
日光 / 强度两档 / 纯间接光）。

### 4.35 前向光源分箱与 SpotLight（CPU 屏幕空间 tile 分箱 × storage 灯表 × 双路径位等价，R78）

**问题**：Globals 均摊路径把点光硬顶在 `MAX_POINT_LIGHTS = 8`（uniform
数组随 Globals 一起 640 B 定长），且没有锥光——手电筒 / 聚光灯只能用
「点光 + 人为调暗锥外」approximate，第 9 盏点光直接被丢弃。R78 给
`iris-render` 补上**前向光源分箱**（clustered forward 的 2D tile 子集）：
CPU 侧把全部点光 + 锥光按屏幕 tile 分箱进 storage 灯表，片元按
`@builtin(position)` 查本 tile 的灯表下标求和；`SpotLight` 组件随之入库。

**数据路径（globals 组 binding 15/16/17，R61/R62a/R62b/R77「不扩
Globals 前缀」扩容先例的延续——skybox.wgsl 并行布局零触碰）**：
15 = 参数 uniform（32 B：屏幕尺寸 / tile 数 / mode 0|1 / 灯数与 index
总数）；16 = 灯表 read-only storage（`MAX_BINNED_LIGHTS = 512` 条 × 80 B，
前 3×vec4 与 `Globals.PointLight` 同布局——前 8 盏点光的阴影开关与
cubemap 槽位逐字段镜像，锥光条目追加 `cone` / `outer` 两个 vec4 携带
单位轴向量与 CPU 预计算的 cos(inner)/cos(outer)）；17 = tile 索引
read-only storage（固定 1 MiB：前 `2 × tile 数` 个 u32 是每 tile 的
`[start, count]`，其后是灯下标展平区）。`LIGHT_TILE_SIZE = 32` 像素。

**CPU 分箱（`light_bins::bin_lights` 纯函数）**：逐灯①近平面门——视
空间球心深度满足 `z_view < −0.05` 才可能可见（DirectX/RH 透视下
`clip.w = −z_view`，门即 `clip.w ≥ near`），球心在近平面之后的灯整盏
跳过；②投影包围盒——球心 ± range 沿相机 right/up/back 三轴的 8 角逐个
投影取 NDC 包围矩形（轴取自 `view_proj⁻¹` 的方向列；角深度各自钳到
近平面再除——球可跨越近平面，越过相机的角直接除会 w 翻转出伪矩形，
钳制后是屏幕上的保守外扩）；③NDC 矩形 clamp 到屏幕后 `floor(px/32)`
展开成 tile 集合。**覆盖保守性**（双路径位等价的根据）：包围盒 3D 包含
range 球 ⇒ 投影矩形的凸包包含球的剪影 ⇒ 任何 `dist < range` 有贡献的
片元所在 tile 必在覆盖集合内——分箱不漏灯；多覆盖的灯由 `dist ≥ range`
早退兜底，不影响数值。**确定性**：tile → 灯下标映射用
`BTreeMap`（展平顺序只取决于 tile 下标，与构建次序无关——`HashMap`
迭代序逐进程漂移会烧穿逐位复现），tile 内下标按灯数组序升序（灯数组序
即求和序，与 Globals 旧路径同序累加）。

**SpotLight 组件**（`light.rs`）：`color / intensity / range`（衰减与
`PointLight` 同模型）/ `direction`（**显式世界空间**锥轴）/ `inner_cutoff
/ outer_cutoff`（弧度，inner 内全强、inner→outer 线性衰减到 0）。锥光
因子 `spot_cone_factor(cos_theta, cos_inner, cos_outer)` CPU 与 WGSL
逐行镜像（同式同序）：`denom = cos_inner − cos_outer ≤ 1e-6` 走硬边
（正是「inner > outer 钳到硬边」的实现机制），否则线性插值 clamp；NaN
输入按 0 贡献处理不外溢。零长度方向 = 不发光（上传时颜色置零）。

**双路径位等价（本设计的中枢）**：逐灯贡献从 fs_main 的点光循环体
**逐式提取**为 `point_light_contrib` 共享函数（经由 `ptr<function>`
出参累计而非返回增量——保持「color + pbr 再 color + sss」的原加法
结合序，返回值方案会把它重结合成 `color + (pbr + sss)`，SSS 场景
±1 LSB）。mode = 1 时旧点光循环由**均匀门**（`bins_params.counts.x <
0.5`）整体跳过——灯表前段就是 Globals 的那 8 盏，两路同开会把它们计
两次；mode = 0 时该门恒真、分箱分支死码。于是：同一灯集合两条路径
逐像素加出同一串位（同函数 × 同序 × 保守覆盖），且**灯表资源缺席/
关闭 = 全零哑缓冲 + 零 write_buffer**（命令流与 R77 逐位一致）——
兼容硬约束「既有渲染快照逐字节复现」由 10 个既有 FNV 基线套件与
force-对-缺席的逐字节 GPU 测试双重实证。锥光因子乘在灯色（radiance）
上——贡献各项对 radiance 线性，与「乘在最终贡献」数学一致且贡献函数
零改动。分箱矩阵用主相机最终 `view_proj`（含 TAA 抖动）；真缓冲仅
mode = 1 帧每帧重传（params + 灯表 + tile 索引三 `write_buffer`）。

**兼容形态**：`LightBinningSettings` 资源缺席 = 自动阈值
（`light_bins_enabled`：点光 > 8 或有锥光即启用）；`force` 字段为调试
/验收强制入口。探针捕获面与 RT 相机的绑定组**恒绑哑资源**（mode = 0
——捕获不渲染锥光与第 9+ 盏点光，捕获视口的 8 盏点光语义与 R77
一致）。超容量口径与点光超限一致：灯表 > 512 按（点光查询序 → 锥光
查询序）截断 + `log::warn`；tile 索引超 1 MiB 按序截断（保留数组序
靠前的，受影响 tile 及其后清零计数）。

**测试**（`tests/light_bins_render.rs`，GPU_SERIAL 串行化）：①双路径
位等价——5 盏点光（含 1 盏投影阴影灯）force 对缺席逐字节相等（阴影
字段镜像的实证）；②第 9 盏点光——8 盏聚簇基线的暗角在加第 9 盏后
显著变亮、range 外远点不变（自动阈值）；③锥光形状——斜轴下照的锥内
点显著亮于锥外**同灯距**点（等距消衰减、同平面消 NdotL 混淆），
force 复跑；④双会话逐字节一致 + 移动锥光改变全帧哈希（防恒黑假
通过）。CPU 纯函数（`bin_lights` 覆盖/近平面/clamp/升序/排列无关/
确定性、`spot_cone_factor` 解析解与 NaN、阈值边界）单测全带。验收图
`docs/spotlight.png`（2×2：8 灯基线 / 12 灯分箱 / spot 关 / spot 开，
跨运行逐字节一致）。

### 4.36 天空 v2 与体积雾（大气解析单次散射 × 链头光线步进，R79）

**天气渲染底座**：旧天空是烘焙进 env cubemap 的风格化渐变（§4.10，
天顶蓝 → 地平线白 + 固定方向 HDR 太阳盘），与光照无联动、无大气感；
R79 补上**程序化大气散射天空**（`SkySettings`）与**逐像素体积雾**
（`FogSettings`），全部走「资源缺席 = 像素与命令流逐位不变」兼容形态
（`DefaultPlugins` 不注册，全部既有 FNV 基线原值通过）。

**大气散射（`shaders/skybox.wgsl`，片元内解析单次散射——无 LUT）**：
裁定为确定性零基础设施路线（LUT 预计算留闭合池）：视线方向一次解析
求值，无预计算纹理、无逐帧 bake。`SkyParams` uniform（64B，skybox 组
**binding 7**）携带 mode / 三强度 / 太阳方向与色 / 地面淡出色 / 强度
校准常量；**Globals 并行前缀结构一字未动**（看门单测保持绿的前提）。
binding 7 无法扩进 globals 组（7 号已是点光 cubemap 阴影贴图），故新
建**专用 `sky_layout`**（bindings 0/4/6 与 globals 组同资源 + 7 =
SkyParams），sky / probe_sky 两条管线换用该布局；天空绑定组由
`build_sky_bind_group` 在全部三个创建点（主相机初建 / env 重烘焙重建 /
六捕获面）生成，mode = 0 帧渲染器**不发 write_buffer**（缓冲保持零
初始化 = 死码分支，命令流与 R78 逐位一致）。

**算法（每片元）**：视线柱与太阳路径柱各算光学深度（平面指数大气 +
Kasten 风格气团近似 `1/(cosθ + 0.045)`——本仓场景为米级尺度，行星
曲率不可测，不建球体）；Rayleigh 相位 `3/16π·(1+cos²θ)`（波长相关
β_R，蓝端经艺术化增强 33.1→52e-6 补饱和）+ Mie Cornette-Shanks 相位
（g = 0.76，β_M 取晴空下限 4e-6——霾值会洗掉蓝）；单次散射解析积分
`∫₀^D e^(−σt)dt = (1−T)/σ` 逐通道闭合；HDR 太阳盘（smoothstep 双阈
cos 0.99985/0.99995，约 0.99° 视直径）乘太阳路径透射（日落红化）；
地平线以下按仰角淡出到 `ground_color`。**太阳方向 = −light_dir**
（Globals.light_dir 是光源 → 场景的传播方向）、太阳色 = light.color ×
intensity，与方向光资源同源。散射亮度为物理量级（~1e-5 × 太阳辐照），
由 `SKY_INTENSITY = 48` 常量折算到本仓 HDR 域（方向光强度 1 ≈ 画面
白）。**捕获相机同样生效**（同一着色器同一 uniform——探针里的天空
与主视口逐像素同源，特性不是限制；但 IBL env cubemap 仍是旧渐变内容，
不联动，见 §8.48）。

**体积雾（`shaders/fog.wgsl` + `src/fog.rs`，链头独立全屏 pass）**：
插在主 pass / TAA 之后、DOF / MB / 泛光**之前**（雾是场景内容的大气
调制，理应参与焦外 / 拖影 / 亮度提取——HDR 线性域链序同理）。输入 =
链头颜色 + scene_depth（R32Float 线性视距——**链上同帧消费**，与
DOF / MB 同款：同一编码器内本帧场景深度副本在雾 pass 之前编码，读到
的是本帧深度）+ 参数 uniform（160B：inv_view_proj + 相机/太阳 + 密度
场/相位/距离标量）。每像素 16 步（`FOG_STEPS` const）发射-吸收积分：
起点按像素哈希抖动半步以内（`fog_dither`，与 `ao_dither` 同套回绕哈希
——去条带，无状态跨运行一致），逐步 `L += T·σ·ds·phase·sun_color`、
`T ×= e^(−σ·ds)`，终点 `out = color·T + L`；深度 = 远平面清空值（天空
像素）的射线长取 `max_distance`。密度场 = `density·exp(−(h−offset)·
falloff)`（下限钳 0）；相位 = Henyey-Greenstein（g 钳 ±0.999）。CPU
纯函数核心（`fog::{fog_density_at, fog_phase_hg, fog_dither,
fog_transmittance}`）与 WGSL 逐行镜像（同式同序，两侧同步修改）。

**槽位拓扑（`post.rs::full_chain_slots_with_fog` 新纯函数）**：旧
`full_chain_slots` **零改动**（既有单测零改动的红线）。雾开启时相机
级链 A→B→A 轮转：雾占全分辨率 **A**（场景级位置），DOF 顺移 **B**，
MB 写源的对面槽，链头 = 最后入链 pass 输出，暗角 / blit 规则不变；
雾关闭时返回值与旧函数**逐字段相同**（渲染器走旧路径，8 种开关组合
的拓扑不变量由新增单测守护）。

**兼容形态**：`FogSettings` 缺席/关闭 = 不分配全分辨率槽、不编码
pass、不写参数（零开销默认路径）；`SkySettings` 缺席/关闭 = 零写 +
mode 0 死码分支。捕获 / RT 相机不吃雾（后处理链只在主相机路径——
天然成立）。

**测试**（`tests/sky_fog_render.rs`，GPU_SERIAL 串行化）：缺席与显式
关两两组合逐位；雾距单调（两堵同材质墙一近一远、直射光强度 0 隔离
纯消光——远墙暗化 ≥ 2× 近墙）；高度衰减（相机升到 18 m 同方向天空
雾化显著减弱）；大气（天顶/地平色移可判别 + 太阳盘中心显著亮于同仰
角旁路）；双会话逐帧逐位（含物体运动）+ 改 `phase_g` 全帧哈希改变
（防恒等假通过）。CPU 纯函数解析解 / 端点 / 单调 / 抖动分散性单测
全带。快照 `examples/sky_fog_snapshot.rs` → `docs/sky_fog.png`
（2×2：旧渐变 | 大气抬头构图 / 平视雾关 | 雾开，跨运行逐字节一致）。

### 4.37 平面水体 v1（Gerstner 波 × 平面反射/折射捕获，R81）

**水体渲染底座**：实体挂 `WaterSurface`（iris-render `water.rs`）声明
一块**世界空间水平矩形面**（`height` 为平面世界 y，`half_extent` 为
XZ 半边长——v1 不随 `Transform` 平移 / 旋转、不可倾斜）。渲染管线 =
主 pass 后的水面合成网格 draw + 主 pass 前的反射 / 折射两张半分辨率
捕获。**缺席语义**：无 `WaterSurface` 实体 = 无捕获 pass、无额外
write_buffer、无水体 draw——像素与命令流与引入前逐位一致（零开销
默认路径；组件本身即开关，`DefaultPlugins` 无需注册任何东西）。

**多水面选择**：多个 `WaterSurface` 实体按确定性选择键
`select_water_surface`（**实体位模式升序取首盏**——min 的交换律使
结果只取决于实体集合、与查询 / 插入顺序无关，与
`select_point_shadow_casters` 同哲学）取首个，其余 `log::warn` 忽略；
反射 / 折射捕获只为首盏做（单水面是 v1 边界，§8.50）。

**Gerstner 波（`water.rs` 纯函数 + `shaders/water.wgsl` 逐式镜像）**：
4 波固定方向（45° 起步等角对角分布）、固定波序求和——振幅按
`[1, 0.5, 0.35, 0.25]` 衰减（主波承载轮廓、高阶波添细节；和 2.1 =
波形有界上界的解析依据，const 断言锁定）、波长按 `[1, 0.55, 0.35,
0.25]` 缩短。相位 φ = k·(d·p) − ω·t，ω = k·speed·√(λ_i/λ_0)（深水
色散 c ∝ √λ 锚定主波相速度）；x/z 位移（choppy，陡度按波数均分
Q_i = Q/(k·A·4)——Q ≤ 1 保证法线 y 分量恒正不卷绕）+ y = ΣA·sin φ；
法线用**解析偏导**（非差分）。同输入 to_bits 逐位一致（确定性单测
锁定）；**波形时间 = 世界 `Time` 资源的 `elapsed_secs`**（资源缺席 =
t = 0 波形静止——不引入渲染器内部时钟，测试可精确驱动）。CPU 与
WGSL 同式同序（镜像契约，与 fog.rs 同款纪律；CPU↔GPU 不做位级等价
声明）。网格 = 归一化 [−1,1]² 96² quad 静态网格 CPU 一次生成
（`water_grid_mesh` 纯函数 + 单测），顶点只带 XZ + uv，顶点着色器乘
half_extent 展开后位移——同一份 GPU 网格服务任意尺寸水面。

**反射 / 折射捕获（主 pass 前，每帧）**：反射 = **镜像相机**（视图
= view × 反射矩阵 M，M 为关于 y = height 平面的反射；投影与主相机
同 fov / 深度范围并同受 TAA 抖动）渲到半分辨率 **RGBA16Float**
（半分辨率理由：反射 / 折射都是低频近似内容——水面本身还做逐片元
Fresnel 与波纹扰动，全分辨率带宽翻倍无观感收益）；折射 = 主相机
视角渲到第二张同格式图。捕获内容 = 不透明几何（普通 + 蒙皮）+ 天空
（探针捕获同款最低配组装：SSR / AO / 分箱 / 探针走哑资源——无屏幕
语义、无探针自反馈）；**不含水体自身**（无递归）、不含透明 / 贴花 /
粒子。**镜像环绕反转的处理**：M 行列式为负 ⇒ 三角形环绕方向反转
（R62a 教训），捕获管线复用 `probe_capture_double_sided` 族
（cull None）吸收——单面 / 双面 / 镜像 / 主相机四种场景共用两条
管线。反射捕获用镜像视锥独立收集（镜像视角能看到主视锥外的物体），
折射捕获直接复用主 pass 实例区间（同相机同 VP，零额外上传）。CSM
复用主相机拟合（捕获朝向不同时阴影近似，与 RT 相机 §8.20 同款）。
**§4.21 / §8.20 修订**：水体内部捕获不占用户 RT 相机预算 2——预算
只约束 `RenderTargetStore` 的用户 RT 相机，引擎内部捕获视口（探针
六面、水体两张）走独立资源路径，修订理由 = 用户可见能力（多视口
编排）与引擎内部实现细节（捕获）是两个正交维度，混在一个预算里
会迫使用户在「监控屏」与「水面」之间做无意义取舍。

**水体 pass（scene_depth 之后、TAA / 后处理链之前）**：独立管线 +
独立组 0 布局（**不扩 globals 组**——R61/R77/R78 先例：0 =
WaterParams uniform（240B：主 VP + 镜像 VP + 眼点 / 光照 + 波形 /
配色）、1 = 反射图、2 = 折射图、3 = scene_depth（R32Float 非过滤
textureLoad）、4 = 过滤采样器 ClampToEdge）。逐片元：
**Fresnel**（Schlick，F0 = 0.02 水的物理值——垂直入射 ≈ 2% 反射）；
**反射** = 镜像 VP 把片元投影回反射图（W 在水面上 ⇒ 镜像投影与主
投影同 uv）+ 法线水平分量小幅波纹扰动（0.04 uv，ClampToEdge 吸收
出界）；**折射 + 深度吸收** = 水下厚度 = scene_depth（线性视距）−
水面片元视距（clip.w 同口径），透过率 T = exp(−厚度/absorb_depth)
（Beer-Lambert 标量式；谱选择性 v1 用浅 / 深双色渐变承担——厚度 0
岸边直接见底，≥ 数倍特征深度趋 deep_color；天空像素深度 = 远平面
清空值 ⇒ 深水色）；**太阳镜面** = Blinn-Phong（power 240）× Fresnel
折扣。输出 a = 1（折射图已含水下内容，alpha 混合退化为替换——
完整合成色）。HDR MSAA 目标 Load + resolve，深度只测不写（LessEqual
与贴花同款等深不丢失）。**scene_depth 第六消费者**：水体在场时
`ensure_scene_depth`（fog_on 同款门），水体 pass 编码在 scene_depth
pass 之后 ⇒ 链上同帧读深度（与雾 / DOF / MB 同纪律）；**水像素不写
scene_depth**（下一帧雾 / AO 对水面用其后地面深度——近似，§8.50）。
FrameStats 口径：水体 draw 计入 draw_calls 与 triangles。

**测试**（`tests/water_render.rs`，GPU_SERIAL 串行化）：生效反证
（有 / 无水体全帧 FNV 不同）；镜面反射（亮红竖柱 + 低机位掠射角，
倒影像素 R−G 有水 / 无水差可判别，采样点 = 镜像几何解析落点）；
深度吸收（深水点无地面支撑 → scene_depth = 远平面 → 透过率 ≈ 0 →
deep_color 主导 vs 浅水点透过率 ≈ 0.8——R−G 色相差 ≥ 90 可判别）；
确定性（水体 + Time 推进双会话逐位 + 改 `wave_speed` 全帧哈希变化）。
CPU 单测 9 项：默认值 / t=0 原点解析解 / 单波波长平移对称 / 单波
振幅上界 / 四波有界（Σ衰减 × 主振幅）/ 法线单位化与不卷绕 / 逐位
确定性 / 网格良构与环绕 / 选择键乱序不变。快照
`examples/water_snapshot.rs` → `docs/water.png`（1×2：水体关 | 开，
海岸斜切构图——近岸浅滩 + 深水空域 + 远岸倒影塔，倒影 / 深浅吸收 /
双会话逐字节断言全带）。

### 4.38 粒子 v2（GPU compute 模拟 × 相机面向拖尾条带，R82）

**v1 / v2 分工（路线图明文）**：v1（`particles.rs`，§4.17）CPU 确定性
模拟**一字不动**，继续作对照轨与 wasm 回退；v2 全新增、二者并存互不
感知。实体挂 `ParticleEmitterV2`（iris-render `particles_v2.rs`）即
启用，配置字段沿用 v1 形状 + 拖尾参数（`trail_length` 历史点数、
`trail_width` 全宽），位置语义与 v1 同（挂 `GlobalTransform` 实体，
发射原点 = 全局平移）。**缺席语义**：无本组件 = 零计算零绘制（无
`write_buffer`、无 compute dispatch、无 draw、不分配 GPU 池——像素与
命令流与引入前逐位一致，R77/R78/R81 同款范式）。**调用契约**与 v1
同：`update_particles_v2` 必须在 `propagate_transforms` 之后、
`Renderer::draw` 之前（`DefaultPlugins` 的 `ParticlePlugin` 与
`update_particles` 同层注册）；离屏快照 / 测试须手动按此顺序调用。

**全仓首个 compute 管线**（`shaders/particles_v2_sim.wgsl`；管线创建
见 `pipeline.rs`，接线见 `renderer.rs` 粒子 pass）。**固定池 + 按索引
有序写入 = 会话内逐位确定性的根基**（路线图裁定）：池容量恒为
`PARTICLES_V2_MAX` = 4096，dispatch 数由池容量 / 工作组大小常量算出
（`PARTICLES_V2_DISPATCH` = 64；整除由 const 断言锁定，不留尾组）——
**恒定 dispatch，不随存活数变**；每个 compute 线程只读写自己索引的
粒子（无原子、无竞争），状态（位置 / 速度 / 年龄 / 存活 + 拖尾环形
历史 `PARTICLES_V2_TRAIL_MAX` = 32 × vec4）存 read-write storage，
步长 560 B（与 WGSL `SimParticle` 影子契约，const 断言看门）。同适配器
同会话内：固定 dispatch + 固定工作组 + 按索引写入 ⇒ 双会话逐帧
**逐位一致**（GPU 测试 FNV 锁定）。**跨适配器位级不可保**（GPU 编译器
fma / 重结合自由——诚实边界，§8.51；CPU 对照轨 = 镜像纯函数的解析
对照单测，非位级等价声明）。

**CPU/GPU 镜像契约（同步纪律：改一处必须两侧同步）**：

① **发射节律 CPU 判定**——`spawn_budget` 累计分数法（v1 逐式独立
实现 + 单测），每步 `write_buffer` 一份 `SimParamsRaw`（96 B：发射数 /
起始游标 / dt / 风 / 位姿 / 种子）；GPU 只消费、不自判发射。
② **出生哈希 = mix64(绝对池索引, seed)**——WGSL 无 u64 算术，故以
u32 肢对（`mulhi32` 16 位裂项 schoolbook + `limb_mul_lo`）逐式复刻
`particles.rs::mix64` / `wind.rs::mix64`（**三处 + WGSL 共四处同步
契约**；整数精确 ⇒ 跨适配器恒等，单测逐位对照原生 u64）。
③ **积分 = 半隐式欧拉 + 风加速度**（`euler_step` 镜像式），age ≥
寿命 → 存活 = false；**死粒子不回收索引**（固定池顺序发射——每个索引
至多出生一次，哈希输入恒定，池满即永久停发）。

**Wind 消费（R80 `Wind` 接口的首个引擎内消费者）**：CPU 每帧在
**发射器原点单点采样** `wind_sample` 写入 sim uniform，GPU 侧把风当
常值加速度与重力合并进欧拉步（**逐式镜像**——WGSL 无 u64 算术，逐
粒子复刻阵风波需肢对复刻整条 gust 哈希链，收益不抵风险；逐粒子空间
变化留后续）。`Wind` 资源缺席 = 无风（零向量）。CPU 镜像值与
`wind_sample` **恒等**（直接调用，零漂移风险，单测锁定）。

**帧队列与池生命周期**：`update_particles_v2` 每步入队一份
`SimParamsRaw`（零发射帧同样入队——积分与拖尾照常推进），渲染器每帧
排水一次并按序 dispatch（同一状态重复渲染 = 空队列 = 不重复步进，帧
哈希稳定；多次 update 而未渲染则按序补跑）；队列上限
`PARTICLES_V2_PENDING_CAP` = 1024，超限丢最旧（headless 长跑防 OOM
——被丢的粒子从未出生，确定性不破坏）。GPU 池按发射器实体绑定
（世代位含于 `to_bits`，销毁重建不撞旧池）、**销毁不回收**（每个曾
出现的 v2 发射器实体占一个 ~2.3 MB 池）；`reset` 清 CPU 状态，渲染器
检测游标回退后全量清零 storage（一次 ~2.3 MB 上传，仅检测到 reset 的
那一帧）。

**拖尾渲染**（`shaders/particles_v2_draw.wgsl`）：相机面向条带
（ribbon）——每存活粒子的历史环相邻两点生成一节屏幕空间展宽四边形，
**固定顶点数 = 池 × trail_length × 6**（顶点全由 `vertex_index` 派生、
数据从 storage 读——无顶点缓冲、无实例缓冲，逐发射器一次 draw）。
第 0 个四边形 = **头部 billboard**（当前位置 + 寿命插值尺寸，v1
`collect_render` 同式，径向软衰减 (1−r²)² 同 `particle.wgsl`）；第
1.. 个 = **条带节**：两端点投影到 NDC 后求屏幕空间法向（宽高比修正后
单位化），两端按各自 1/w 展宽 ⇒ **恒定世界宽度**（`trail_width` 为
全宽）；横向软边 (1−v²)²，相邻节共享端点宽度位置 ⇒ 关节处透明连续。
颜色：头部 = `color_at(当前 age)`（v1 同式），条带顶点 = `color_at(
记录时 age)`（**沿历史按 age 比例渐变**——头部关节与头部四边形颜色
连续，只差一帧 age）× alpha 沿历史线性淡出（尾部全透明，平滑收束、
无硬截断）。死粒子 / 历史不足（`flags.z ≤ k+1`）/ 端点在相机身后 /
零长度节 → 全部顶点输出画面外同点 = **零面积退化**（不产生片元，固定
顶点数下按索引确定性消隐）。渲染进 v1 粒子 pass 位置（透明之后、泛光
之前；R81 水体更后，不受影响），HDR 目标 alpha 混合（additive / alpha
两条管线仅混合不同），深度只测不写（Less）、cull None（屏幕空间展宽
的绕向随相机变化）——**不写 scene_depth**（与 v1 / 贴花同纪律）。**无
视锥剔除**（粒子位置在 GPU，CPU 无逐粒位置——v1 有）；`Visibility`
过滤照常（隐藏只过滤绘制，模拟照常推进排水，v1 同语义）。FrameStats
口径：`particles_drawn` 累计各可见发射器的 `alive` 估计，`triangles_drawn`
加 alive × trail_length × 2，其 draw call 并入 `draw_calls`。

**测试**（`tests/particle_v2_render.rs`，文件内 `GPU_SERIAL` 串行化）：
① v1 零改动 + 缺席语义（v2 组件在场但从未推进 ⇒ `draw_calls` / 三角数
与仅有 v1 时同构，重复渲染逐位一致）；② v2 生效（未推进 0 粒子 →
推进后成片像素，再推进一步分布改变）；③ 拖尾（`trail_length` 1 vs 8：
发光像素显著更多、三角数恰 8 倍）；④ 确定性（双会话逐位 + 换种子帧
哈希变化）。**度量学**：本场景天空是程序化大气**恒亮**（sRGB R ≈ 208，
全帧 R 阈值计数恒为 ≈ 30400 px），且 additive 进 HDR 后经 ACES 压缩，
条带峰值贡献在 sRGB 域仅 12–30 级——判据一律用**与空场景 / 仅头部帧的
逐像素差分**（阈值依据 = 条带解析面积，见测试模块头）。CPU 单测 13 项：
肢对 mix64 与原生 u64 逐位一致 / fraction 值域与精度 / 出生方向语义 /
发射节律 / 欧拉步解析对照（匀速直线 ±1e-5）/ 拖尾环回绕 / 记账与帧队列
/ 队列上限丢最旧 / 风镜像与 `wind_sample` 恒等 / 帧队列逐位确定性 /
reset / 命令式 burst / 钳制。快照 `examples/particle_v2_snapshot.rs` →
`docs/particles_v2.png`（1×2：拖尾关 | 开，旋转喷射 + 暗色地面 + 背光
太阳——背景越暗 additive 判别度越高；发光像素 / 新增像素 / 几何增量 /
双会话逐字节断言全带）。

**C2 序列化闭合（§8.51⑩）**：`ParticleEmitterV2` 全配置字段（含
trail_length / trail_width）进关卡档 `particles_v2` 段——缺省 = `new(Alpha)`、
运行态（帧队列 / 池游标 / 存活批表）不入档，往返位级一致
（`particles_v2_section_loads_and_roundtrips`）。

### 4.39 GI 探针逐片元化（三阶 SH 方向性探针 × 片端径向权重求值，R83）

**把 §8.40 的①（逐片元 GPU 插值）与②（SH 方向性探针）从拒绝清单移进
实现**，③（可见性加权）维持拒绝。v1（§4.33）的探针是「逐探针一个无方向
RGB 标量 + 四面体重心插值 + 逐实体 CPU 求值」：角色头脚吃同一份辐照、
强方向性场被六轴平均摊平。R83 不换掉 v1，而是**给探针追加一层方向性数据
并把求值点从 CPU 逐实体搬到 GPU 逐片元**——v1 的数据与路径一字不动，
继续作 mode = 0 的兼容轨与对照轨。

**为什么逐片元在这里便宜**：v1 拒绝逐片元的理由是「四面体表上 GPU = 逐
像素点定位搜索，成本差 4–5 个数量级」。R83 绕开搜索：探针的辐照场改存
**三阶球谐**（L0–L2，每通道 9 系数），逐片元求值 = 「固定序循环探针表 →
径向距离权重 → 9 项点积」，无空间结构、无搜索，成本与反射探针（§4.30 的
`probe_weight` 循环）同量级。数据结构形状同时是 R88/R89 DDGI 的前置
（探针 + 方向性系数 + 影响半径）。

- **三阶 SH 烘焙（`irradiance.rs` 纯函数段）**：系数来自**与 v1 同一份
  烘焙口径**（同一 BVH、同一 `BakeParams`、同一「天空 or 一次反弹」辐射率
  公式 `probe_sh_radiance`——不新增捕获 pass，只是把「六轴各积一次半球」
  换成「六面 × 32² 方向各取一次辐射率再投影」）。投影 = 固定面序
  （`PROBE_AXES`：+X,−X,+Y,−Y,+Z,−Z）× 固定行列序 × **精确固体角权重**
  （`cube_texel_solid_angle` 的闭式 `F(x,y) = atan2(xy, √(x²+y²+1))` 四角
  inclusion–exclusion；六面合计 4π 钉到 f64 的 1e-12，面心/面角权重比 ≈ 4.9
  ⇒ 等权 4π/N 会把 L2 系统性压偏）。累加 f64、基函数 f32（**单一事实源**，
  不为投影另写 f64 基）。
- **Lambert 带通卷积 + 除 π（存「E/π 直读」系数）**：clamped-cosine 的 SH
  系数只有三个非零频带 `A = (π, 2π/3, π/4)`（Ramamoorthi & Hanrahan），
  投影后逐带乘 `A_l/π = (1, 2/3, 1/4)` ⇒ 着色器对任意法线 n 的求值退化为
  **9 项纯点积每通道**（`Σ_k d_k·Y_k(n)`），无逐片元卷积、无 π 换算，与
  `irradiance_map` 采样值 / loc14 载荷**同口径**。解析锚点：常量辐射率 C ⇒
  任意方向求值恒 = C（精确，残差只有 f32 舍入）；纯 L1 场 `L = C·ω_y` ⇒
  `E(+Y)/π = 2C/3`（把 `A_1/π = 2/3` 钉死，无求积误差）。
- **CPU/WGSL 同步契约**：实球谐基常数表（`SH_Y0/Y1/Y2A/Y2B/Y2C`）与
  `sh_basis` / `probe_influence_weight` / `sh_blend` 的表达式**逐式同序**镜像
  到 `standard.wgsl::gi_sh_basis / gi_probe_weight / gi_sh_irradiance`；常数表
  由源码层看门单测 `wgsl_sh_constants_match_cpu_table` 逐位比对（照
  `pipeline.rs::skybox_globals_prefix_matches_standard_layout` 的先例）。
- **GPU 接线（R78 模式复刻）**：binding 18 = 探针 read-only storage
  （`MAX_IRRADIANCE_PROBES` = 256 × `ShProbeRaw` 160 B = `vec4 位置+半径` +
  9 × `vec4 (RGB, 保留)`）；binding 19 = 16 B 小参数 uniform
  （`params.x = mode`、`params.y = 探针数`）。**Globals 一字节不动、顶点
  布局不动（loc14 属性保留）**；skybox.wgsl 的并行布局不受影响（片元阶段
  只读 storage 计数 = 3，在 WebGPU 默认上限 4 之内）。哑资源（160 B / 16 B
  全零）**恒在**：缺席 / 无 SH / 捕获 / RT 帧绑哑缓冲 ⇒ mode = 0 分支死码，
  零 write_buffer，命令流与 R82 逐位一致。
- **mode 均匀门（R78 同款形态）**：mode = 0 ⇒ 执行 R64 的 loc14 逐实体支
  （**逐位不变**）；mode = 1 ⇒ GI 探针项由逐片元 SH 求值替代。两支都只替换
  `in.probe.w > 0.5` 的实体 ⇒ **天空 `irradiance_map` 兜底与 R52c 光照贴图
  `select` 混合语义零触碰**（正交）。裁决 = 探针集是否带 SH（`has_sh()`）：
  SH 是烘焙期的显式选择（`bake_irradiance_probes_sh`），「烘了就用」，没有
  独立开关资源（与 R64「装了探针集就用」同一心智模型）。
- **逐片元混合**：径向权重 `w = (1 − (d/r)²)²`（**与点光衰减窗同式同源**），
  `E/π = Σ(w·SH(n)) / max(Σw, ε)`；`Σw ≤ ε`（片元在全部影响球之外）⇒ 回退
  **最近探针**的 SH（严格 `<` 裁决，等距取下标小者）——与加权支在球边界处
  连续（边界上 w = 0、最近探针即该探针），且延续 v1「包外无距离衰减」的
  语义。固定循环序 = 固定累加序 ⇒ 会话内逐位确定；出口钳 ≥ 0（L2 截断的
  振铃在暗区可给出小负值，负间接光是非物理方向；**无去振铃窗**，v1 接受）。
- **影响半径**：自动派生 = **最近邻距离 × 1.5**（平方距离 + `total_cmp` 全序
  裁决 ⇒ 输入任意重排给出同一组半径；单探针 = +∞ 覆盖全场、重合点 = 0 不
  参与加权）；[`IrradianceProbeRadius`] 组件可覆盖（0 / 非有限 = 自动）。
  **偏离声明**：规格原文是「`IrradianceProbe` 加可选覆盖字段」——它是单元
  结构体，加字段会把全仓 `IrradianceProbe` 值表达式（含既有测试）变成编译
  错误，撞「既有测试零改动」硬约束 ⇒ 改为独立可选组件（缺席语义等价）。
- **带 SH 集的 loc14 载荷**：xyz 改由 SH 的**零阶项**按同一套径向权重给出
  （`sh_blend_l0`），标志位对任何有限位置恒 1——逐片元路径不依赖四面体化，
  1–3 个探针（四面体化为空）也生效。这份 xyz 只被 mode = 0 的路径（探针
  捕获 / RT 相机）消费；主视图在 mode = 1 时只读标志位。
- **烘焙 API 签名保持兼容**：`bake_irradiance_probes` 一字不动（无 SH ⇒
  mode = 0），新增 `bake_irradiance_probes_sh`；`install_irradiance_probes`
  不变（报告多一个 `sh` 字段）。编辑器零改动（编辑器本就不调探针烘焙）。

**测试**：CPU 单测 16 项（常数表 = 解析式的正确舍入 f32 + 正交归一自校验 /
基函数在坐标轴的解析值 / WGSL 常数看门 / 固体角合计 4π 与面心面角比 /
cube 面覆盖 / 常量场求值恒 C / L1 带通因子精确 / 异色半球上下分离 + 六轴
平均摊平对照 / 投影两跑逐位 / 辐射率口径与 `bake_sample` 同源 / 半径派生
全序与置换不变 / 权重窗与点光窗逐位同值 / 加权混合与最近探针回退与边界
连续 / 出口钳负 / SH 烘焙 v1 数据逐位不变 + 与六轴平均互证 + GPU 形态往返
/ 单探针无四面体也发标志位）；GPU 5 项（`GPU_SERIAL` 串行化：缺席 = 空集 =
退化集三者逐字节 / **空间梯度**（4 m 高柱横跨红区绿区：SH 柱脚偏红柱头偏绿
中段单调、v1 上下同色、跨模式作差 ±50–70 级）/ **方向性**（同一探针内球顶
朝亮面 vs 球底朝暗面 ±77/−173 级、常量场对照逐位同源、v1 差 1 级）/ 真实
烘焙贯通（天窗房间，GPU 与 CPU `sh_irradiance_at` 同侧互证、v1 分离 0）/
确定性双会话逐字节 + 改单探针 SH / 位置帧哈希变化）。快照
`examples/gi_sh_snapshot.rs` → `docs/gi_sh.png`（2×2：空间梯度 v1 | SH、
方向性 v1 | SH；**每世界独立渲染器**——句柄缓存按值复用，跨世界复用会串味，
AGENTS.md 已知陷阱）。

**实测（release，i7-13620H，2026-09-16）**：单探针 SH 烘焙 = 6144 主射线 +
命中方向一次反弹射线 ≈ v1 六轴烘焙的 `6144 / (6·sky_samples)` 倍（默认
`sky_samples = 64` 下约 16×），一次性支出；运行时零增量（系数是常量表，
逐片元只做 9 项点积 + 一次距离权重）。满载 256 探针的 storage = 40 KiB，
mode = 1 帧每帧重传（与 R78 分箱三缓冲同纪律）。

### 4.40 虚拟几何体 V1：簇构建 + 资产格式（iris-render/meshlet + iris-engine/meshlet_builder，R84）

第三梯队 R84–R87 连役 1/4。**全案 MVP 边界（红线修订声明的落地面，
越界即 bug）**：本仓体量 Nanite 式子集——**无 visbuffer、opaque-only、
无软件光栅回退**；V1 只做簇构建 + 资产格式 + 等价性锚点，渲染器绘制
路径**零改动**（meshlet 资产经既有常规网格路径渲染验证等价；compute
剔除是 R85 的事）。**流式红线收窄声明**（路线图 v5 §一裁定，R87 兑现）
的全文见 §4.43 末节——简式：**地块流式仍不做；资产级 meshlet 分页
流送（GPU 显存分页）允许**。

- **运行期表示（`iris_render::meshlet`）**：`MeshletMesh` = 全局顶点
  （与既有网格同 32 B `Vertex` 格式，材质 / 光照零改动复用）+ 全局索引
  （**簇内局部号**）+ 逐簇元数据 `MeshletClusterRaw`（48 B POD，
  `reserved: [u8; 2]` 显式占位消除隐式填充满足 bytemuck::Pod——派生宏
  对有填充的类型编译期 panic，副产品是给 R85 留了语义位）。字段：
  `vertex_offset` / `index_offset`（指向全局缓冲，簇按构建序连续衔接，
  读侧校验）/ `vertex_count: u8` / `triangle_count: u8` / 边界锥
  （axis = 面法线均值归一，`cutoff_cos = min(axis·n_i)`；退化防护：
  全退化三角或法线完全对消 ⇒ 全向锥 [0,0,1] / −1 **永不背面剔除**）/
  包围球（xyz = 球心相对网格包围盒**归一**，w = 半径**世界单位**；均值
  球心 + 最大半径——选型：单遍、必然包含、逐位确定，Ritter 双遍近似
  无紧度优势；退化轴 extent = 0 除数取 1.0）/ `error`（**V1 恒 0.0**，
  量化编码就位，R86 简化器启用）。**诚实口径**：预算常量
  `MESHLET_CLUSTER_MAX_VERTICES = 256` 被 u8 字段物理容量收窄到 255，
  构建器按 255 卡（`as u8` 回绕防线）。
- **确定性贪心聚类（`iris_engine::meshlet_builder`）**：顶点 → 三角形
  邻接表（构建序即确定序）上的种子生长——种子 = 未分配三角**索引序
  最小**者；候选集 `BTreeSet`（升序迭代 = 确定性硬文化选型）；评分 =
  固定字典序全序（与簇已用顶点的共享数**降序**、三角索引**升序**收尾
  ——`BTreeSet` 升序扫描 + 严格大于 = 同分取索引最小，无并列歧义）；
  簇满 128 三角或无可生长候选（含顶点容量装不下的候选跳过）即封簇。
  簇内顶点**局部重映射**（发现序编号），簇间边界顶点不去重（各簇各存
  一份，meshlet 标准形态：总量换逐簇自包含）。**输入即索引序**：乱序
  输入不适用种子序保证，但同输入两跑输出逐位一致（`to_bits` 断言）。
  元数据全部走 `meshlet` 模块的无状态纯函数（`cluster_bounding_sphere` /
  `cluster_cone` / `cluster_bounds_normalized`——R85/R87 将共用）。
- **资产容器 IRML（v1）**：`"IRML"` + version u32 + 顶点/索引/簇数
  u32×3 + CRC-32（u32，与 `pack::crc32` 同一定义——跨 crate 复制而非
  共享依赖，反向依赖会成环）+ 数据区（顶点 32 B | 索引 4 B | 簇 48 B
  顺序排布，全 LE 逐字段编解码）。reader 校验链：魔数 → **精确版本**
  （`!= 1` 即拒——v1 无历史版本可向后兼容，升版时加逐版本分支）→
  长度精确匹配 → CRC → 簇偏移连续性 / 区间越界 / **簇内局部索引越界**
  / 保留位非零。API 与 Pack 同构：`write_to` / `write_file` / `from_bytes`
  / `open`。
- **展开 API（`MeshletMesh::expand`）与渲染等价性锚点**：逐簇
  「局部号 + vertex_offset」还原全局索引的纯函数。**V1 的正确性论证**：
  展开 = 「顶点重排 + 索引重排」，三角形集合（含环绕方向）与逐角属性
  值与构建输入完全一致 ⇒ 不透明几何覆盖集不变、逐角插值位级不变 ⇒
  **渲染逐字节一致**（前提：场景无共面重叠面——深度并列时绘制序才
  影响像素；断言失败即真 bug，无阈值豁免）。GPU 佐证
  `tests/meshlet_render.rs` 4 项（`GPU_SERIAL`；两台独立渲染器分别
  服务常规 / meshlet 世界——句柄值缓存跨世界串味，R53/R62 教训）：
  两机位逐字节相等 / 反空洞（两角度 FNV 必不同）/ 同世界重渲逐字节。
- **集成形态偏离声明**：规格给「GltfBuilder 侧导出入口 or 独立 example
  二选一」，定稿**独立 example** `meshlet_build_example`（GltfBuilder
  现场生成测试 GLB → `spawn_gltf_instance` 真实解析路径 → 逐网格簇化 →
  `.imesh` 写出 → 位级往返 + 三角守恒 + 簇上限断言）；pack 集成留
  R87 分页流送时做（避免本轮摊大饼）。仓内既有内容资产全部带蒙皮或
  TANGENT（assault_courtyard 为法线贴图写入切线），V1 按边界拒收——
  example 自产测试 GLB 的原因。快照 `examples/meshlet_snapshot.rs` →
  `docs/meshlet.png`（2×2：常规 | meshlet × 两机位；同机位全帧 FNV
  相等、跨机位不等、跨运行逐位）。

**测试**：CPU 26 项（`meshlet` 13：48 B 布局契约与 Pod 位级往返 / 包围球
解析解（单三角 2/√3、正方形半对角线）/ 锥解析解（平面簇 cutoff ≈ 1、
min 点积关系、对消退化兜底）/ 归一化映射 / 容器往返位级 / 魔数版本拒绝
/ CRC 与截断 / 语义校验（越界 / 超上限 / 不连续 / 保留位）/ 展开三角
集合守恒；`meshlet_builder` 13：两跑逐位 / 覆盖恰一次 / 簇上限与连续
偏移 / 连通性 BFS / 球面锥朝外 / 立方逐面解析解（轴与 cutoff 精确）/
对消退化兜底 / 平面 cutoff / 单三角 / 空输入 / 资格判定 / should_panic×2）；
GPU 4 项。诚实限制 §8.53（R85 起第③条由 §4.41 闭合）。

### 4.41 虚拟几何体 V2：compute 簇剔除 + 前缀和压缩绘制（iris-render/meshlet + renderer，R85）

R84–R87 连役 2/4。路线图裁定：wgpu 无 mesh shader、WebGPU 无
multi_draw_indirect ⇒ **前缀和压缩绘制**（wasm 兼容）。MVP 边界延续
§4.40（无 visbuffer / opaque-only / 单 LOD / 遮挡留 R87）。

- **数据流（每帧，仅 meshlet 实体在场时）**：CPU 收集可见
  `MeshletInstance`（实例表条目 = 模型 / 法线矩阵 + arena 基址 + 帧内
  全局簇号前缀，**双缓冲**——compute 写本帧槽、draw 读「产出当前所绘
  列表的那一帧」的槽）→ compute pass ① `main_cull`（一线程一
  （实例, 簇）对：包围球视锥判定 + 背面锥判定 → 存活标志 + 紧凑载荷）
  ② `main_compact`（单工作组 256 线程 × 每线程顺序扫 64 flag：段总和
  Hillis-Steele 包含式扫描 → 段基址 + 段内游标写紧凑列表与存活总数，
  **无原子**）→ CPU 回读（staging + map_async，遮挡 R53 同款双槽协议，
  **下一帧使用**）→ 一个 instanced draw（实例数 = 回读计数）。
- **前缀和确定性零负担**：u32 整数加法**结合精确** ⇒ 无论扫描树形状
  结果恒等（与浮点扫描的本质区别）；紧凑槽位由前缀值唯一决定、与线程
  完成顺序无关；**紧凑序 = 全局簇号升序**（stable compaction）⇒ 绘制
  序确定 ⇒ 会话内逐位（`chunked_prefix_compact` 是 WGSL `main_compact`
  的逐式镜像，单测对朴素过滤对照）。
- **剔除谓词**（`meshlet.rs` 纯函数为锚点，WGSL 逐式镜像）：视锥 =
  Gribb-Hartmann 6 平面提取（`frustum.rs::Frustum::from_view_proj`
  同式，含 EPSILON 归一化护栏）× 变换后包围球（归一化球心反归一化 →
  模型变换 → 半径按最大列缩放，与 `collect_instances` 同口径）；背面锥
  = `cluster_is_backfacing`——**R85 把 R84 头注释的松散表述定稿为精确
  谓词**：`cutoff = min_i(axis·n_i)` ⇒ 全部法线在半角 θc = acos(cutoff)
  的球冠内 ⇒ 全背 ⇔ 视线角 ≥ 90° + θc ⇔ `dot ≤ −sin θc`（严格小于才
  剔、cutoff ≤ 0 含全向退化锥永不剔——R84 的「点积 < cutoff 即背面」
  对近平坦簇会误剔正面，V1 无消费者未暴露）。遮挡不在本轮（R87）。
- **绘制管线（vertex pull）**：`meshlet_draw.wgsl` vs_main——
  `instance_index → 紧凑簇列表 → 簇元数据（12×u32 词 storage，48 B
  `MeshletClusterRaw` 逐字排布）→ storage 拉顶点 / 索引`（无顶点 / 索引
  缓冲；只读 storage 顶点阶段合法）。**片元阶段 = standard.wgsl 的
  fs_main 本体**（wgpu 允许管线两阶段跨模块、接口按 location 匹配）——
  「PBR 求值共享 vs 逐式复制」的两难以此消解：零复制零漂移，与常规
  路径同一材质绑定组语义；间接光只吃 irradiance_map（probe / 光照贴图
  实例属性路径不在 vertex-pull 管线，诚实限制 §8.54；GTAO / 雾等全屏
  链自动生效）。**跨管线像素等价因此可达**：同一 fs 模块 + 顶点侧与
  `vs_main` 逐式同构（同 `InstanceRaw::from_matrix` 值源）⇒ 快照
  FNV 相等断言实锤（`meshlet_v2_snapshot.rs`）。
- **回读与兜底**：消费端要求「紧凑列表的产出帧 == 恰好上一帧」（帧号
  标记配对）——首帧 / 读回迟到 / 映射失败 / 跳帧一律降级 **恒等列表
  全量绘制**（CPU write_buffer `(实例槽<<16)|簇局部号` 升序表）。剔除
  只删不可见几何 ⇒ 全量帧与剔除帧像素恒等，兜底无视觉跳变（定稿理由：
  不走 R84 expand 常规管线兜底——那会引入第二条着色路径，破坏像素
  连续性与单管线断言；expand 保留为几何对照锚点不删）。
- **容量与缺席**：`MAX_MESHLET_CLUSTERS = 16384`（IRML 读取构建期拒绝
  + 渲染器注册处 warn 跳过 + GPU 缓冲定长三处同源）；实例表 256/帧；
  绘制按簇上限展开固定顶点数（128 三角 × 3，越界槽位零面积退化）。
  缺 `MeshletInstance` = 零注册上传、零 dispatch、零回读、零绘制（既有
  像素与命令流逐位不变；GPU 佐证 `absence_semantics_bit_stable`）。
  arena 追加式注册（句柄只增，已发布帧的紧凑列表引用恒有效）。
- **排序位置**：compute pass 编码在主 pass **之后**（本帧 draw 消费的
  是上帧列表，本帧 compute 的覆写必须在 draw 之后——同编码器按序）；
  meshlet draw 在主 pass 内、常规不透明段之后（不透明 z-buffer 裁决与
  顺序无关）。`FrameStats.meshlet_surviving` = 本帧提交的紧凑实例数
  （剔除有效性可观测；兜底帧 = 全量簇数）。

**测试**：CPU +7（`meshlet` 20：+48 B 词布局锚点（WGSL 12×u32 词下标
契约）/ 视锥平面提取与球判定解析解（正交盒平面常数 + 内 / 切 / 外三态，
`Frustum::planes` 新只读视图）/ 背面锥符号约定（正对存活 / 背对剔除 /
99°-101° 阈值带 / 退化全向 / 相等点存活）/ 紧凑化扫描镜像（朴素过滤
对照 + 段基址收敛自检 + splitmix64 伪随机批量）/ 顶点词重排位级往返 /
mesh_bounds 解析解 / IRML 簇上限拒绝）；pipeline +1（meshlet 三重接口
看门：Globals 前缀 / VertexOutput 逐字段 / InstanceEntry 双侧同序）；
GPU 6 项（`meshlet_v2_render.rs`，`GPU_SERIAL`）：三机位全量帧 vs 剔除
帧逐字节相等（本轮最强断言）/ 紧凑列表严格升序 / `meshlet_surviving`
剔除有效性口径 / 缺席语义（有 vs 无 FNV 不同 + 空场景双帧双渲染器逐位
+ 仅挂 store 零改动）/ 双会话逐字节 / 旋转一帧哈希变化。快照
`meshlet_v2_snapshot.rs` → `docs/meshlet_v2.png`（1×2：expand | compute
剔除；全帧 FNV 相等 + 反空洞 + 跨运行逐位 + 目检）。诚实限制 §8.54。

### 4.42 虚拟几何体 V3：层级 LOD——簇 DAG + 屏幕空间误差选择（iris-render/meshlet + iris-engine/meshlet_builder + renderer，R86）

R84–R87 连役 3/4。MVP 边界延续 §4.40（无 visbuffer / opaque-only）；
**「单 LOD」边界由本轮解除**（层级落进容器与选择，GPU 管线仍零改动）。

- **简化器 = 体素网格顶点聚类（Rossignac）**（`iris_engine::meshlet_builder::simplify_by_vertex_clustering`，
  纯函数）：选型裁定——QEM 边坍缩质量更高，但堆序 / 半边结构的确定性
  成本过高；顶点聚类对三角汤天然友好、简化 = 空间网格函数 ⇒ **逐位
  确定免费**（质量折衷诚实入档 §8.55）。口径：同一**可合并格**内的
  顶点收敛为首见代表，代表位置吸附格心 `(floor(p/cell)+0.5)×cell`；
  单顶点格**原位保留**（无合并不位移）；`boundary` 集合（组边界）内
  的顶点**永不合并**（同格也各占独立桶、位置位级原样）——压住组间
  接缝裂缝；合并后塌缩 / 零面积三角剔除、三顶点号升序规范键去重；
  误差 = 实际 cell × √3/2（格对角半长 = 合并位移上界），未发生合并
  诚实归零。f32 量化限制（极小 cell 的 p/cell 舍入可致意外合并）入
  简化器文档。
- **层级构建（自底向上 DAG）**（`build_meshlet_hierarchy[_with_config]`）：
  Level 0 = R84 叶簇（每簇一组）；每轮把当前层组按**连续贪心**分块
  （≤ 8 组一块，余 1 时本块多吞 1——组沿叶簇构建序天然空间连续，
  「固定字典序评分」的落地面 = 组序邻接），块内几何按位去重合并 →
  简化（cell = 叶簇包围球半径均值 × 系数 × 2^level；三角数未达「组内
  一半」时有限次倍增 ≤ 4）→ **复用 R84 聚类**重聚类为父组自有簇 →
  产出父组（children / 簇范围 / own_error / group_error =
  max(own, children)）。停止条件：单组 / 当层三角合计 < 128 / 层数上限
  8。**块间共享顶点 = 边界集**（首见块 owner + contested 判定，构建期
  固定扫描），块内组间缝合顶点不锁（那是合并的自由度）。簇层布局：
  叶簇在前、父簇按层追加 ⇒ 组范围连续 +「子组号 < 父组号」（容器读侧
  校验，遍历深度天然有界）；收尾对**全部簇 / 组**按整网格包围盒统一
  归一 bounds（与渲染器注册口径对齐）。
- **IRML v2**：头 36 B（v1 字段 + 叶簇数 / 组数 / 根数）+ 数据区追加
  组区段（children 数 + children + 簇范围 + 误差 ×2 + bounds + cone，
  全 LE）与根区段；CRC 覆盖整个数据区（错误优先级：v1 长度精确先于
  CRC = R84 原序；v2 尾部闭合在组 / 根解析后校验）。版本矩阵 = {1,
  2}：v1 文件照常读取（层级空 = 全叶，R84 单层布局零漂移——单层
  `write_to` 仍写 v1 布局，`--no-lod` 语义天然落回）；其余版本拒读。
  v2 读侧校验：叶数 ∈ [1, 簇数]、子组号 < 父组号、簇范围闭合、根
  越界、组 / 根区段长度闭合。
- **逐帧选择（CPU 纯函数）**（`select_lod_cluster_ranges_in` /
  `select_lod_clusters`）：从根按固定序遍历（显式栈 DFS，根升序 /
  子组升序），组屏幕误差 = `group_error × error_scale × (screen_h ×
  0.5) / (dist × tan_half_fov)` ≤ `threshold_px` ⇒ 选中本组簇范围、
  停止下钻；否则下钻 children；**叶组恒选自身** ⇒ threshold 0（或负）
  恒细化到叶。选择是**逐组布尔** ⇒ 输出集合与遍历实现无关；输出 =
  选中簇区间升序 + 相邻合并（`sort_unstable_by_key` 全序 + 合并），
  与绘制紧凑序对齐。选择在网格局部空间做（眼点经模型逆变换、组球心
  反归一化与 `meshlet_cull.wgsl` 同式；误差放大 = 最大轴缩放——与剔
  除端半径口径一致）。**选择在 CPU，GPU 选择留后续**（诚实限制
  §8.55）。
- **渲染器接线（只换输入集合）**：`MeshletLodSettings { threshold_px,
  enabled }` 资源**缺席 / enabled=false = 全叶表 = R85 行为逐位不变**
  （参数不组装即零额外路径）。启用时逐实例选择产出的连续区间 = 帧
  实例表的 **run 形条目**（`base_counts = [顶点基, 索引基, 簇基 +
  run 起点, run 长]`）——run 区间在 arena 中连续 ⇒ R85 的
  cull / compact / draw 管线、双缓冲、读回协议**零改动**照常消费；
  紧凑序 = 本帧枚举序（选中集升序）⇒ 绘制序确定。容量退化（确定性，
  按查询序）：run 数 / 簇数超单帧预算 ⇒ 该实例退化全叶 run（像素正
  确，放弃简化收益）；全叶也超 ⇒ 跳过实例 + warn（R85 口径）。三角
  形统计经 run 起点表还原网格局部簇号（回读消费与兜底帧同式精确累
  加）。GPU 测试（`meshlet_lod_render.rs`，`GPU_SERIAL`）：**τ→全叶
  等价**（threshold 0 + 层级 mesh vs R85 全叶路径逐字节——本轮最强
  断言）/ 距离单调（54 → 18 → 8 簇 + 物体不消失）/ 双会话逐字节 +
  阈值变化换像素流。快照 `meshlet_lod_snapshot.rs` → `docs/meshlet_lod.png`
  （1×3：近叶簇细 | 中 L1 过渡 | 远根簇粗；选中数 54/18/8 标注于
  example 输出；目检：远处无裂缝碎面、近处细节不丢、无消失闪面）。

### 4.43 虚拟几何体 V4：分页流送 + 遮挡整合（iris-render/meshlet_stream + meshlet + renderer + iris-engine/pack，R87）

R84–R87 连役 4/4（**收官**）。MVP 边界延续 §4.40（无 visbuffer /
opaque-only / 无软件光栅回退）；**本轮兑现两笔欠账**——R84/R85 两次
推迟的 **pack 集成**（最小读取入口）与路线图 §一 的**红线修订声明**
（流式红线收窄，全文见本节末）。

- **页划分（`iris_render::meshlet_stream::MeshletPageTable`，纯派生）**：
  固定簇数页，**定稿 16 簇/页**（规格草案「如 256」的偏离声明）——256
  簇/页下典型中等资产（数百至一两千簇）整网格只落 1–8 页，叶层与粗层
  几乎必然同页，粗化回退与字节预算双双失去页粒度；16 簇/页 ≈ 2k 三角
  /页 ≈ 百 KB 量级，页表开销（每页 4×u32 区间 + 逐簇组号）可忽略。
  页 p 的簇区间恒为 `[p×cpp, min((p+1)×cpp, 簇总数))`；顶点 / 索引字节
  区间由簇元数据的**连续衔接性**导出（= 容器读侧校验的不变量）；
  逐簇所属层级组号（`cluster_group`）是优先级评定的输入，取「升序扫描
  组表时**首个**覆盖该簇的组」（容器按不变量组范围互斥，重叠时最低组号
  赢——防御式定序，畸形层级不 panic）。`derive` 与构建期写入逐位等价
  （单测锁定）。
- **IRML v3**：头 40 B（v2 头 + 每页簇数，CRC 后移）+ v2 全部区段 +
  **页区段**（页数 + 逐页顶点 / 索引区间 + 逐簇组号）。版本矩阵 = {1,
  2, 3}：**v1 / v2 文件照常读取**（页表缺席 ⇒ 流送激活时按
  `MESHLET_PAGE_CLUSTERS` 现算，纯派生逐位等价）；其余拒读。v3 读侧
  校验：页数 = `ceil(簇总数 / 每页簇数)`、逐页区间与簇元数据**导出值**
  逐字段核对、逐簇组号 ∈ 哨兵 ∪ 所指组的自有簇范围、数据区在页区段后
  精确闭合。**偏离声明**：R86 时代「版本 3 必拒」断言随 bump 调整
  （3 进兼容名单，[0, 4, 999] 仍拒）——「bump v3」与「v1/v2 照常读取」
  两规格共同要求下的必然后果（同 R86 先例）。**页槽 = arena 规范区间**：
  页 p 的 GPU 落点恒为它在簇 / 顶点 / 索引三个 arena 里的固有字节区间
  （页内簇数固定 ⇒ 区间由页号唯一决定），**不引入 packed 页池 + 页间
  重定向**——`meshlet_cull.wgsl` / `meshlet_draw.wgsl` 按（实例基址 +
  局部号）直读 arena，**零改动**是 R84–R86 兼容硬约束的直接推论。
  arena 缓冲按全量容量预留 = v1 的 VRAM 诚实成本（裁决只决定「哪些
  字节有效可读」，见 §8.56①）。
- **驻留状态机（`ResidencyManager`，CPU 纯逻辑，确定性铁律）**：每帧
  一次 `tick(budget, pages_per_frame, requests, page_bytes)` →
  `ResidencyOps { evict, upload }`。步骤即实现序：①**键合并**（同帧同页
  多请求取最小误差——`min` 与请求顺序无关；保护集 = 请求页并集）②
  **预算收缩强制执行**（超预算先逐未请求页）③**按优先级准入**
  （`pages_per_frame` 封顶；放不下则逐未请求页腾位，腾不出则本页延后
  ——下帧请求重建时自然重试）。**优先级 = 固定字典序全序**：页键 =
  （页**紧迫度**的**倒序位映射**，页号升序收尾）——
  `BTreeSet` 升序迭代即「高优先级先驻留」，**请求顺序与队列无关**
  （乱序请求同队列）。浮点紧迫度经 `order_key_bits` 位映射成全序 u64
  （非负浮点位型单调；NaN 归 +∞），倒序键 = 位取反 ⇒ 全部迭代序来自
  `BTreeMap` / `BTreeSet` 键序，同输入两跑裁决逐位一致。**页紧迫度 =
  该页内「被请求区间覆盖的簇」的簇紧迫度最小值**（不是页内全部簇）；
  **簇紧迫度**：有所属层级组 ⇒ 该组屏幕误差，**无所属组 ⇒ `+∞`**
  （最紧急——无组簇只在单层网格与「层级未覆盖」的防御态出现，二者都
  没有父组可回退，缺页即整块消失）。**驱逐**：预算
  放不下新页时未请求的**最低优先级**驻留页让位（同全序裁决）；本帧请求
  页受保护（**无抖动**——满足当前帧请求的驻留页不动）。**偏离声明**：
  规格写「最小屏幕误差**升序**」（越需要越先驻留），实现取**倒序**
  （大紧迫度优先）——按字面升序时叶页（组误差恒 0）恒最先驻留、粗页恒被
  逐出，推演结果是「预算紧张 ⇒ 叶页挤占粗页 ⇒ 选中的叶组沿 DAG 找
  不到任何驻留祖先 ⇒ 实例整帧消失」，与本轮「小预算 ⇒ 粗层渲染、无洞」
  的验收硬标准直接矛盾；倒序（粗层 base-mip 先行）让「页缺 ⇒ 父组
  回退」的无缝洞机制在任意预算下都有落点——「越需要越先驻留」按
  「缺页即物体消失」的口径理解，粗页是物体存在的下界，最需要。
  **紧迫度口径的两处细化**（用户 2026-09-17 裁定后修订，原稿为「页内
  全部簇的最小误差、无组簇按叶口径计 0」）：①**只统计请求方覆盖的
  簇**——容器布局叶簇在前、父簇按层追加 ⇒ 叶 / 父边界那一页通常同时含
  两类簇，远景粗化时请求方只要父簇，若仍按页内全部簇取 min 会被同页
  叶簇拖到最低优先级、粗化回退在该页失去落点；②**无组簇取 `+∞`**——
  原口径把「无回退」误当「叶簇（有回退）」，使单层网格资产在预算紧张
  时最先被饿死（缺页即整块消失，本应最紧急）。两条改动均不改变任何
  像素（只改准入 / 驱逐顺序），快照 FNV 逐位不变。
- **驻留感知 LOD 选择**（`select_lod_cluster_ranges_resident`）：R86
  选择函数的扩展——遍历与阈值判定**逐式同构**（全驻留时输出与
  `select_lod_cluster_ranges_in` 逐位一致），选中组若其簇区间覆盖的页
  **未全部驻留** ⇒ **沿 DAG 上取第一个区间全驻留的祖先**（父页驻留则
  用父，逐级上溯）；链上无任何驻留祖先（含根）⇒ `skipped = true`，
  实例整帧跳过。双子组回退到同一父组只产出一条区间（合并段对相同区间
  去重）。同时输出**请求页集** =（理想集 ∪ 实际可绘区间 ∪ **回退链沿途
  拜访的祖先区间**）覆盖的页——最后一项是必须的：
  理想集只含叶页、预算只容粗页时，粗页从未被请求 ⇒ 回退永不成功 ⇒
  实例永不出现（**自锁**，实现期推演发现）。逐页紧迫度见上条（只计
  请求方覆盖的簇；无组簇 `+∞`）。渲染器将其喂给下
  一帧的 `tick`，理想页由此逐步流入（本帧粗化、数帧后细化收敛）。
- **渲染器接线**（`renderer.rs` 的 R87 接线段）：`MeshletStreamSettings
  { gpu_byte_budget: Option<u64>, pages_per_frame }` 资源——**缺席 /
  预算 `None` = 无预算 = 全驻留 = R86 行为逐位不变**（默认兼容形态：
  页表仍随注册构建，但不做门控、不做逐页上传，arena 全量上传照旧）；
  有预算 ⇒ 页按优先级分帧上传（tick 消费**上一帧**收集的请求——请求
  一帧延迟 = R85 紧凑回读的同款节拍）、驻留感知选择门控绘制。注册期
  `stream_active` 时**跳过整库数据上传**（页数据按驻留裁决逐页
  `write_buffer`；缓冲容量照常预留）；**驱逐 = 页数据清零**（非驻留页
  的 arena 区间不保留有效数据，预算的字节语义落在真实拷贝上）。**迁移
  重传**：流送 → 非流送的运行期迁移（预算被移除）时驻留页可能已被逐出
  arena，回非流送路径前必须整库重传（`meshlet_stream_recover_upload`
  标记，先于「无新句柄」早退消费）。**重建补传**：运行期新增资产把顶点 /
  索引容量推过当前容量时 arena 缓冲被重建，新缓冲零初始化而驻留状态机
  仍标记旧页驻留——`tick` 只对**未驻留**页组队（`resident.contains` 短路），
  「后续请求自然补传」不成立 ⇒ 重建后必须按驻留集全量补传，否则是
  **静默消失**（簇元数据在定长缓冲里完好：cull 照报存活、顶点 / 索引
  全零 ⇒ 零面积三角、画面空而统计不空）。GPU 测试
  `stream_survives_arena_rebuild_on_late_registration` 锁定（去掉补传即
  失败——该测试是在验收轮亲审 diff 时发现的缺陷，见 CHANGELOG）。
  **请求一帧延迟 + 本帧驻留状态**的
  组合 ⇒ 首帧无驻留页 ⇒ 实例整帧跳过、第 2 帧起粗层可见、数帧后收敛。
- **遮挡整合（R53 两阶段，实体级 v1）**：`MeshletInstance` 的**实例
  AABB**（全部簇包围球的联合盒，注册期一次预计算，过模型矩阵取保守
  世界盒——与 R53 的 `world_aabb` 同式）作为查询条目追加在普通实例
  条目之后（收集序 = 查询序，查询槽 ↔ 实体映射稳定；4096 上限对合并
  列表统一封顶，尾部不查询恒可见 = 保守方向）。被遮挡实体在**收集期**
  整帧跳过（不进实例表 = 不进 cull dispatch 输入）。**与流送的次序**：
  遮挡测试用实例 AABB，与页驻留**正交**——未驻留实例在选择期已整帧
  跳过，遮挡路径不双重处理；遮挡隐藏的实例**不产生流送请求**（不为
  看不见的物体预热页）。AABB 条目**先于任何跳过路径**产生——被遮挡
  隐藏者也持续重查，那是恢复可见的唯一路径。**逐簇 GPU 遮挡（Hi-Z
  类）不做**（诚实限制 §8.56③）。
- **pack 集成兑现（R84/R85 两次推迟）**：`iris_engine::pack::read_meshlet`
  ——一个函数 + 往返测试的最小入口：`PackReader::read`（seek +
  read_exact + CRC 校验，数据区不整体进内存）+ `MeshletMesh::from_bytes`
  （版本矩阵 {1, 2, 3} / 结构校验链）。错误分档
  `MeshletPackError { Pack / NotIrml / Irml }`——调用方可判别「挂载层
  继续找下一来源」（`Pack(NotFound)`）与「资产真的坏了」（其余）；魔数
  为准，扩展名只是约定。**编辑器 / AssetMount 全接线不做**（诚实限制
  §8.56④）。
- **测试**：CPU +20（`meshlet_stream` 17：页划分守恒（簇 / 顶点 / 索引
  恰覆盖一次 + 字节守恒 + 组号归属）/ 末页短页 / `derive` ≡ `build` /
  全驻留选择 ≡ R86 逐位（三档阈值）/ 父组回退精确性（解析解 `[(9, 2)]`）
  / 双子组回退去重 / 根未驻留 ⇒ 整帧跳过（理想请求仍完整）/ 单层网格
  整块门控 / 页请求键 = 页内最小误差（解析解 2.0）/ 乱序请求同队列 +
  上传序 = 优先级序 / 字节与页数双重预算 / 驱逐选最低优先级 + 请求页
  保护（无抖动）/ 收敛零操作 / 同页多请求取最小误差 / 预算收缩先逐未
  请求页 + 全保护接受超预算 / 位键全序（NaN 归 +∞、负值钳 0）/ 越界页
  防御；`meshlet` +2：IRML v3 往返位级（含总长逐字节 + 单层叶数归一 +
  二次写出幂等）+ v3 页区段四连语义校验（页数 / 区间 / 组号越界 / 组号
  与组范围不符）；`iris-engine::pack` +1：`.imesh` 往返 + 错误分档）；
  GPU +4（`tests/meshlet_stream_render.rs`，`GPU_SERIAL`）：**全驻留
  等价**（流送缺席 / 预算 `None` vs R85 全叶路径逐字节 + `surviving`
  = 叶簇总数——零成本回归锚点）/ **预算流送**（粗层预算 ⇒ 像素哈希变
  化、选中簇数下降、画面非空（回退链请求保证粗页流入、无死锁）；预算
  逐步放大 ⇒ 选中簇数单调趋近全驻留；预算 = 全量 ⇒ 收敛帧与 R85 全叶
  路径逐字节）/ **遮挡整合**（柱后 meshlet 实体收敛帧整帧跳过——像素与
  实体缺席帧逐字节相等；前提校验：无遮挡配置的在缺席帧亦逐字节相等
  （球零像素贡献）；帧 1 保守全可见；稳态逐位一致；柱侧可见实体恒不被
  剔、像素与无遮挡帧逐字节相等）/ **确定性**（同预算双会话逐字节）。
  快照 `examples/meshlet_stream_snapshot.rs` → `docs/meshlet_stream.png`
  （1×3：①全驻留 | ②低预算粗层 | ③遮挡整合——①断言与 R85 全叶路径
  逐字节相等；②选中簇数 15 < ① 的 47 且物体不消失；③立柱 + 柱侧可见
  球（照常渲染）+ 柱后球（`culled == 1` 整帧跳过），格内两球构成可判别
  的目检对照。目检：粗层轮廓完整无洞（形态与 R86 中景粗代理一致）、
  遮挡正确无残留、无闪烁断层）。**诚实限制**：§8.56。

### 流式红线收窄声明（用户 2026-09-14 裁定，R87 兑现）

路线图 v5 §一「红线修订」的原条款：「原『不做大开放世界流式』收窄为：
**开放世界地块流式不做；资产级 meshlet 分页流送**随虚拟几何体 V4
（R87）允许。」

本轮兑现为可执行边界，本声明即全仓权威口径：

- **仍不做**：开放世界**地块流式**（terrain chunk 的按需装卸 / 驻留
  管理 / 流式 LOD 拼接——§4.23 与 §8.31 的限制条目**原样保留**，本轮
  不触碰地形子系统）；
- **本轮允许且已落地**：**资产级 meshlet 分页流送**——页划分 + GPU
  显存驻留状态机 + 驻留感知 LOD 选择（§4.43 全节）。粒度是**资产内的
  固定簇数页**，不是世界地块；
- **仍不做**（红线未动）：visbuffer 全量管线、软件光栅回退、Lumen 类
  无限定距离全动态 GI、网络多人、通用 DCC 级编辑器；
- **本轮新增的流式边界**：分页是 **GPU 显存分页**——页数据源是进程内
  的 `MeshletMesh`，**磁盘级异步 IO 不做**（IRML 整文件仍先读进内存）；
  arena 缓冲按全量容量预留（VRAM 诚实成本，§8.56①）。

### 4.44 DDGI 动态全局光照 v1（规则探针网格 × 时间片滚动重照明 × 深度感知混合，R88）

**闭合 §8.52①**（R83 的径向权重不感知遮挡 ⇒ 隔着薄墙的两个探针在墙两侧
互相串味，影响半径是唯一隔离手段），附带缓解 §8.52⑩（探针布局是作者责任
⇒ 规则网格自动生成布局）。R83 的数据与路径**一字不动**，继续作 mode = 0/1
的兼容轨与对照轨；R88 追加 mode = 2。开工前设计定稿（决策点裁定与理由）留档
在 `docs/ddgi_v1_design.md`。

**选 DDGI 而非 SSGI**（路线图 v5 原定）：无屏幕空间假影、不依赖步进基础
设施、网格代数更新天然 `to_bits` 可测。**MVP 边界**：opaque 间接漫反射；
无屏幕空间分量；无 GPU 光线查询（WebGPU 无 RT 扩展）；1 次反弹；单色天空。

- **规则探针网格（纯资源态，不落实体）**：`DdgiSettings`（World 资源，
  **缺席 = DDGI 关**）声明 spacing / bounds / k / 射线档 / hysteresis /
  sky_color / bounces / `geometry_generation`。格点数 `n = max(1, floor(span/spacing))`
  逐轴、格心 `min + (i + 0.5)·spacing_eff`、索引序 **x 最快**
  （`index = ix + nx·(iy + ny·iz)` ⇒ 滚动刷新的空间推进方向是 x 行扫描）。
  `bounds = None` ⇒ 取静态几何 AABB 外扩 1×spacing、**Y 下限额外抬
  0.5×spacing**（探针埋地板下会把地板上的着色点判成「墙后」⇒ 整片 GI 被抑制）。
  不落实体的理由：避免 256 实体的 ECS 开销与 `&mut World` spawn 限制
  （`&World` 的系统无法注册新组件类型）；R89 的编辑器放置入口再议实体化。
- **容量抽稀（确定性，不是按序截断）**：超 `MAX_IRRADIANCE_PROBES`（256）⇒
  `stride_for_capacity` 求**最小整数 k** 使 `⌈nx/k⌉·⌈ny/k⌉·⌈nz/k⌉ ≤ 256`
  （整数二分 + `checked_mul`，无浮点搜索 ⇒ 逐位确定），`spacing_eff = spacing×k`
  + `log::warn!` 一次。网格仍规则无洞、**覆盖范围不变**——按序截断会砍掉
  远端一整块空间 ⇒ 该块 GI 直接回退 env，出现可见的空间断界。半径用解析式
  `spacing_eff × 1.5`（规则网格的最近邻恒为 `spacing_eff` ⇒ 不跑 O(n²) 的
  `derive_probe_radii`；单测断言二者逐位一致防口径漂移）。
- **时间片滚动重照明（复用 R52a 的 BVH 追踪器）**：`DdgiGrid::tick` 每帧只
  重照明 k = `probes_per_frame` 个脏探针——`advance` 纯函数自 cursor 起环形
  扫描至多 n 个位置取前 k 个 dirty 者，`cursor'` = 最后被更新者的下一位。
  收敛帧数 = `⌈n_dirty/k⌉`（解析可断言；默认 256/2 = 128 帧 ≈ 2.1 s @60fps）。
  **光照指纹**（`light_dir`/`light_color`/`sky_color`/`bounces`/`rays_per_face`
  的 `to_bits` 固定序位组合）或**几何 generation** 变化 ⇒ 全表标脏但
  **不清空旧值**（旧值继续用、逐帧刷新 ⇒ 无闪烁，这正是 DDGI 的时域行为）。
  **稳态（无脏探针）⇒ tick 立即 return**：静止场景 + 恒定光照的 CPU 与上传
  成本双零（「缺席 = 零开销」文化的延伸）。
- **安装帧粗填**（`DDGI_PREFILL_GRID = 2`，6×2² = 24 方向/探针，满载一次性
  ≈ 3.3 ms）：避免前 `⌈n/k⌉` 帧出现「SH = 0 的黑波按网格索引序扫过场景」。
  粗填**只填 SH、深度矩置中性**（μ = 天空哨兵、σ² = 0 ⇒ 深度项恒 1）：
  粗填档 2 < 深度格 8 无法整除映射，且保守方向正确（粗填期可能短暂漏光，
  `⌈n/k⌉` 帧内被真深度取代，优于黑波）。
- **单探针重照明 `probe_relight`：SH 投影与深度两阶矩同一批射线**（⇒ 深度项
  **零额外光线成本**）。SH 累加与 R83 的 `project_probe_sh` **逐式同构**
  （固定面序 × 固定行列序 × f64 定序累加 × `cube_texel_solid_angle` 精确
  固体角权 × 出口 `sh_lambert_convolve` ⇒ E/π 直读）。辐射率口径的**单一
  事实源**是 `probe_ray_radiance_depth`（返回 `(L, t)`）——`probe_sh_radiance`
  **改薄壳**返回 `.0`（偏离声明见下）。
- **深度表的数据形态**：每探针 **cube 面参数化**的两阶矩 6 面 × 8² 纹素 ×
  `(μ, σ²)` = 384 × 2 个 f32 = **3072 B/探针**，满载 768 KiB。
  ①**cube-face 而非八面体图集**（DDGI 论文原味是 oct atlas）：与 SH 投影
  同批射线 ⇒ 零额外光线成本，且省掉 oct 编解码的 CPU/WGSL 双侧镜像与看门面；
  ②**`array<f32>` 标量数组而非 `array<vec4>`**：WGSL 无动态 swizzle ⇒ vec4
  打包要 3 次 select（`ShProbeRaw` 偏离声明记录过的同一个坑）；
  ③**σ² 在 CPU 用 f64 求差后钳下限** `σ² ≥ (0.1μ)²`：`E[t²] − μ²` 在 f32 里
  会灾难性抵消（单测量化：μ = 1e4、σ = 1e-3 时 f32 求差归零、f64 保住信号），
  钳位同时消掉「单纹素零方差 ⇒ chebyshev 退化为硬阶跃」⇒ **着色器零减法、
  零钳位**。深度 = `hit.t + BAKE_SHADOW_BIAS`（`t` 从偏移后的射线原点起算，
  加回偏移才与着色器侧 `distance(x, p)` 同原点；单测直证垂直向下的深度恰为
  探针高度）；未命中 ⇒ **有限**哨兵 `DDGI_SKY_DEPTH = 1e4`（不用 `inf`：
  会污染矩累加 ⇒ σ² 变 NaN）。
- **深度感知混合（关键算法）**：`w_i = gi_probe_weight(d_i, r_i) ×
  ddgi_depth_weight(d_i, μ_i, σ²_i)`，径向窗是 R83 原式 `(1−(d/r)²)²`
  **零改动**；深度项是 DDGI 原式的 **chebyshev 单侧上界**——`dist ≤ μ ⇒ 1`
  （着色点在探针可见范围内），`dist > μ ⇒ σ²/(σ² + (dist−μ)²)`（按「真实
  深度 ≥ dist」的概率上界衰减 ⇒ 墙后权重趋 0）。双剪枝（`radial > 0` 且
  `w > 0`）后才做 9 项 SH 点积。深度纹素查表 = 主轴判定 + uv（CPU 镜像
  `ddgi_depth_texel` ↔ WGSL 同式同序），往返单测（`cube_face_direction` 的
  纹素中心必须回到同一格 ⇒ 双射）+ 逐格穷举「CPU 累积格 ↔ WGSL 查表格同格」
  （四个合法档位全测）+ 源码层看门单测三重钉住。
- **兜底语义对 R83 的显式偏离**：mode 1 的 `Σw ≤ ε ⇒ 回退最近探针` 在
  mode 2 **改为回退 `env_irradiance`**（天空兜底）。理由：回退最近探针会
  **绕过深度项** ⇒ 墙后片元吃到墙另一侧探针的辐照 = 正是本轮要消灭的漏光。
- **hysteresis 旋钮默认 0（关）**：`sh = old×α + fresh×(1−α)` 逐系数（深度矩
  同款）。默认关 ⇒「收敛帧数 = `⌈n/k⌉`」可解析断言；α = 0 逐位等于纯替换、
  α = 1 冻结、α = 0.5 解析中点三条单测钉住。
- **GPU 接线**：binding **20** = 深度表 read-only storage（组 0 的 0..=19 已满，
  片元阶段只读 storage 计数 = 4（16/17/18/20），**恰在** WebGPU 默认的
  `maxStorageBuffersPerShaderStage = 4` 之内）；binding 19 参数扩位
  `x = mode(0/1/2)`、`y = 探针数`、`z = DDGI_DEPTH_GRID`、`w = 深度项开关`。
  **不扩 `Globals`** ⇒ `skybox.wgsl` 的并行布局陷阱零触碰、看门单测不动。
  768 KiB 真表**惰性创建**（首个 mode = 2 帧，照 `AoGpu`/`OccGpu`），关闭帧
  绑 4 B 全零哑缓冲。SH 表沿用 binding 18 的 40 KiB 现缓冲（`ShProbeRaw`
  **零改动**）。mode 门三支重构：mode 0 与 mode 1 的**语句逐条不动** ⇒
  R83 / R64 既有 GPU 断言原值通过（R78「双路径位等价 + 均匀门」纪律）。
- **增量上传（偏离 R83 的「每帧重传」）**：只写本帧重照明的 k 个探针段
  （SH 的 `k × 160 B` + 深度的 `k × 3072 B`，按探针索引偏移 `write_buffer`）；
  网格结构变化 / 首次启用 ⇒ 全量写 + 重同步记账。比对键 = `DdgiGrid::probe_stamp`
  （逐探针「最后一次被重照明的 tick 号」）+ `structure_generation`。默认参数下
  增量 ≈ 6.3 KiB/脏帧、**稳态 0 B**。
- **三态裁决与互斥**：`DdgiGrid` 非空 ⇒ mode = 2 + **整表替换** binding 18/19
  + **忽略** `IrradianceProbeSet`（裁定 2，`log::warn!` 一次）。理由：两份
  间接漫反射叠加会**双重计入**天空与色溢（物理错误）。DDGI 缺席 ⇒ R83 裁决
  原样（`has_sh()` ⇒ 1，否则 0）。loc14 逐实体载荷改由 `DdgiGrid::instance_payload`
  给出（`sh_blend_l0` 的零阶径向混合 + 标志位，**与 R83 带 SH 的集同口径**）
  ——这份 xyz 只被 mode = 0 的路径（探针捕获 / RT 相机**恒 mode 0**，
  §8.52⑦ 同款立场）消费，主视图只读标志位。
- **运行期光照口径**（`iris_engine::ddgi::ddgi_bake_params`，纯函数可单测）：
  `light_dir` = `DirectionalLight.direction`（传播方向同口径）、`light_color`
  = `color × intensity`（约定「已含强度」）、`sky_color` =
  `settings.sky_color.unwrap_or(ambient)`（裁定 3：**R80 昼夜免费联动**，
  不乘 `EnvironmentSettings.intensity`——`ibl_env` 已被 `env_params.y` 缩放，
  预乘会双计）、`shadow_softness` = 0（硬影单射线）、`sky_samples` = 1
  （本路径不做半球采样）、`bounces` ≤ 1。`globals.ambient` 的常量补光项
  **不动**（独立的美术补光预算）⇒ 同源导致的天空底色双计入 §8.57⑬。
- **编排**：`DdgiPlugin`（`DefaultPlugins` 已含）在 Startup 插入空 `DdgiGrid`
  （`&World` 的系统无法注册新资源类型）、Update 注册 `tick_ddgi` 并行系统，
  **注册在 `DayNightPlugin` 之后** ⇒ 昼夜当帧写的方向光当帧被 DDGI 消费。
  两阶段模式（读 settings / light / 需要时才 `collect_bake_triangles` →
  `resource_mut::<DdgiGrid>` 写回）；**稳态帧一次三角形都不收集**（几何变化
  靠宿主 `geometry_generation` 显式宣告，照 `SkySource.generation` 先例）。
- **确定性**：轮转序 = 纯索引环形扫描；重照明 = 固定面序 × 固定行列序 ×
  f64 定序累加；深度矩 = 固定子格序 f64 累加 + CPU f64 域求差钳位；指纹 =
  `to_bits` 固定序位组合；hysteresis = 固定 α 的一阶确定性递推；跨帧累积 ⇒
  **双会话 48 帧逐帧逐位**（像素 FNV 序列 + CPU 表 `to_bits` 序列双断言，
  R60/R62b 范式）。**两条等价锚点**：①α = 0 且 k ≥ n ⇒ 表逐位等于「全量
  重照明一遍」；②`rays_per_face = SH_CUBE_GRID`(32) ⇒ SH 表**逐位等于**
  `probe_sh_coefficients`（跨模块锚点，同时证明 `probe_sh_radiance` 改薄壳
  未漂移）。跨适配器位级不可保（§8.51 / §8.52⑤ 同款边界）。
- **成本账（`rays_per_face = 16` 的质量档，裁定 1）**：每探针 6×16² = 1536
  方向 = 3072 条射线（primary + 反弹点硬影）≈ **0.83 ms**（1000 三角 BVH，
  基线取 `bake.rs` 模块文档实测的 0.27 µs/ray）；默认 k = 2 的脏帧
  ≈ **1.7 ms/帧**；显存 768 KiB（惰性）+ 40 KiB（既有）；上传增量
  ≈ 6.3 KiB/脏帧、稳态 0 B。**昼夜循环场景是持续脏表**（光照每帧变）⇒
  1.7 ms/帧是常态开销而非峰值；可调档位 `rays_per_face = 8` ⇒ 0.42 ms/帧、
  `probes_per_frame = 1` ⇒ 0.83 ms/帧（收敛 256 帧）。着色器侧全表线性扫描
  ≤ 256 探针，双剪枝使增量约 10 op/探针（主轴判定 + 2 次 floor + 2 次
  storage load + 1 次除法）。
- **对照模式**：`Renderer::set_ddgi_depth_weight_enabled(false)`（照
  `set_legacy_probe_parallax` / `set_legacy_shadow_bias` 先例，默认 `true`）
  ——只改片元的权重口径、不改重照明与深度表的**内容** ⇒ 两侧像素差只能来自
  chebyshev 项，是「漏光抑制有效性」断言的必要开关。**不是**运行时诊断可视化。
- **观测**：`FrameStats::ddgi_updated`（本帧重照明探针数，取自
  `DdgiGrid::stats().last_updated`，不进任何排序键）；`DdgiStats` 另有
  probe_count / dirty_count / ticks / stride / spacing_eff。
- **验收**（`tests/ddgi_render.rs` 6 项，文件内 `GPU_SERIAL` 互斥；跨世界
  对照各用独立渲染器）：①缺席逐位不变（无网格 / 空网格 / 有 settings 但
  网格空 三者逐字节相等，且**空 DDGI 网格不得扰动 R83 的 mode = 1 裁决**）；
  ②开 / 关可判别 + 门口梯度（受试点取**墙面而非地板**——墙面法线 −X 与指向
  太阳的 +X 侧夹角 > 90° ⇒ 直射项结构性为零、像素里只剩常量 ambient + 间接
  漫反射，地板做不到（天花板出视锥 ⇒ 不进阴影贴图，实测直射占地板亮度 92%）；
  CPU 对照轨沿墙面扫 z 剖面（z = 0.9…6.0），GI 必须随离门口的距离**严格单调
  衰减**且首末落差 ≥ 3×（实测 4.04×）——暗室全封闭、门口是唯一连通 ⇒ 梯度
  只能来自门口进来的光）；③**漏光抑制有效性**（同一世界同一渲染器只翻深度项
  开关：暗室近墙墙面 ROI 亮度比 ≥ 1.8 且绝对差 ≥ 60（实测 2.88 / 265），
  **ROI 避开门口这个混淆源**；三条 CPU 精确归因：穿墙探针 chebyshev 权重
  < 0.25（实测 0.059）、同侧近墙探针 ≥ 0.9（实测 1.0，射线恰命中 ROI 所在
  墙面 ⇒ μ ≥ dist）、**开阔无遮挡处关 / 开比 < 1.5**（实测 1.011 ⇒ 抑制是
  遮挡感知而非一律调暗）；CPU 线性辐照比 ≥ 3（实测 7.9×）作像素阈值定标）；
  ④滚动收敛（`ddgi_updated` 计数
  序列 = k,k,…,尾帧,0,0 + `⌈n/k⌉` 帧后画面与「k = n 一帧全量重照明」FNV
  相等 + CPU 表逐位相等）；⑤双会话 48 帧逐帧逐位；⑥昼夜联动（光照阶跃 ⇒
  L0 单调跟随、`⌈n/k⌉` 帧后逐位到位）。CPU 侧 34 项解析 / 逐位单测
  （`iris-render/src/ddgi.rs`）+ 7 项编排单测（`iris-engine/src/ddgi.rs`）。
  快照 `examples/ddgi_snapshot.rs` → `docs/ddgi.png`（2×2：关 | 开 |
  深度项关（漏光对照）| 夜间相位；warmup = `⌈n/k⌉ + 1` 帧并在 example 内
  断言 `dirty_count == 0`，不许拿未收敛的表出验收图；跨运行逐字节一致 +
  双会话 FNV 断言）。
- **偏离声明**（4 条，全部有依据）：①mode 2 的 `Σw ≤ ε` 兜底改为回退 env
  （回退最近探针会把漏光从后门放回来）；②增量上传偏离 R83 的「每帧重传」
  （数据量 19× + DDGI 有变更记账 + 与实例缓冲同纪律）；③`probe_sh_radiance`
  改薄壳（辐射率口径单一事实源，避免逐式复制漂移；R83 既有测试原值通过 +
  等价锚点② 逐位相等双重看门）；④深度存 cube-face 参数化而非八面体图集
  （同批射线 ⇒ 零额外光线成本 + 省掉双侧镜像与看门面）。另有两处小澄清：
  binding 19 的 `w` 分量承载深度项对照开关（设计稿写「保留 0」，但对照模式
  必须有落脚处，照 `probe_params.count.w` 先例）；重照明指纹在设计稿的四项
  之外追加 `rays_per_face`（它决定射线预算，变了旧表就不是同一口径的产物）。

**诚实限制**：§8.57。

### 4.45 DDGI v2：合成语义定稿 + 覆盖度连续化 + 编辑器入口（iris-render/ddgi·bake·post·renderer + iris-editor + iris-engine/level，R89）

路线图 v5 第三梯队「全局光照」的收官轮（2/2）：把 R88 留下的「与烘焙光照
贴图 / 反射探针 / SSR 的合成语义未修订」升级为**五方显式裁决**，用一条
覆盖度连续化一次性修掉 §8.57⑬（ambient 双计）与 §8.57⑱（扇贝带），补上
编辑器的 DDGI 入口（参数步进 + 可视化 + 入档），顺带闭合两个既有 bug
（SSR 强度死旋钮 26 轮未发现、`reset_taa` 漏清 GTAO 跨帧状态）。开工前
设计定稿（决策点裁定与理由）留档在 `docs/ddgi_v2_design.md`。

- **合成语义五方裁决**（§8.57⑫ 移入实现，语义权威 = 本节）：
  1. **× 烘焙光照贴图 = 互斥，贴图赢**：挂图集的实体不发探针标志位
     （`renderer.rs` 的 `probe_payload` 裁决点，R88 前已是行为）⇒ 完全
     吃不到 DDGI。物理依据：图集已含 `sky + bounce`（bounce 带方向光
     弹射），与 DDGI 承载同一份间接漫反射，相加即双计。**差值法（贴图
     静态 + DDGI 动态增量）裁定拒绝**（设计决策 9）：需要①持久化基线
     探针表、②每探针二次重照明对照、③负增量钳位、④「贴图烘焙参数与
     网格参数严格同源」的脆弱隐性契约，而「同空间两套 GI 混用」可由
     美术流程自行规避（一个关卡一套 GI）。编辑器 `bake_and_install` 在
     `DdgiGrid` 非空时发一条状态提示（只提示，不自动裁剪，
     `BakeReport::ddgi_grid_present`）。
  2. **× env 天空兜底 = 覆盖度连续混合**（下条详述）：R88 的
     `Σw ≤ ε ⇒ env` 硬兜底升级为羽化。
  3. **× ambient 补光 = 按覆盖度分摊**（下条详述，对 R52c/R63「ambient
     是独立美术补光预算、探针不接管」的部分推翻——裁定 3 让 `sky_color`
     与 ambient 同源后「不接管」不再自洽，会双计）。
  4. **× 反射探针捕获 = 捕获恒 mode 0，吃 `DdgiGrid::instance_payload`
     的零阶载荷**（R88 事实的显式裁决 + 锚）：捕获画面的间接漫反射是
     **零阶径向混合、不含深度项、无方向性**（低频量，可接受）；DDGI 表
     变 ⇒ 捕获变；烘焙集变 ⇒ 捕获不变（互斥）。让捕获走 mode 2 会引入
     「捕获相机看墙后」的新语义与探针自反馈风险。
  5. **× SSR = 零代码改动的一致性**：SSR 读的上帧颜色 = 主解析缓冲帧尾
     副本（含全部 GI）⇒ SSR 命中色天然跟随 DDGI 表（锚
     `ssr_reflects_ddgi_and_intensity_knob_is_live` 的粗填/收敛对照）。
- **覆盖度连续化（修 ⑬ + ⑱ 的核心算法）**：逐片元几何覆盖度
  `cov = max_i clamp((r_i − d_i) / (r_i × DDGI_COVER_FADE), 0, 1)`
  （`DDGI_COVER_FADE = 0.25` ⇒ 半径最外 25% 是羽化带、`d ≤ 0.75·r` 内区
  恒 1），与 `Σw` 同一遍固定序循环取 `max`。两个消费点：
  ①`irradiance = select(env, mix(env, gi, cov), 实体标志位)`——env ↔ GI
  从 R88 的硬 `select`（半径球面并集处整片切换 = 扇贝形亮带）变成 25%
  半径宽的连续过渡；②ambient 补光行追加 `× (1 − cov)`——`cov > 0` 的
  像素按覆盖度分摊、不再双计同一份 ambient 源天空底色。
  **为什么不是 `Σw` 归一化**（决策 5）：`Σw` 在网格内部就给不出 1，会把
  R88 已经正确的像素削弱；「带内深度」把改动面**结构性地限制在外壳**。
  **逐位口径**：`cov = 1` 时 `mix(x, y, 1.0) = y` 精确恒等 ⇒ 内区逐位等
  于 R88（GPU 锚实测：全画面 cov=1、ambient=0 的场景与 R88 代码的差异
  = 全帧恰 **1 字节 ±1 LSB**——R63 同款编译期副作用，非语义差异）；
  `cov = 0` 时 `× (1 − 0) = × 1.0` 逐位恒等（R77 `ao_ss` 哑纹素 1.0 同款
  形态）⇒ mode ≤ 1 与 DDGI-off 路径零变化（DDGI-off 对照组与 R88 逐位
  一致，GPU 锚锁定 R88 原值）。
- **编辑器入口（桶 C，四步顺序不可倒）**：
  - **C0 前置**：新 marker 组件 **`BakeExcluded`**（`iris_render::bake`，
    缺席 = 收录）——`collect_bake_triangles` 是 lightmap / 辐照探针 /
    DDGI 三个消费者共用的三角收集，编辑器叠加层（网格线 / 选中框 /
    Nav 红片 / 探针球 / 灯泡视觉）此前会进 DDGI 的 BVH 挡光并撑大
    `default_bounds`。查询元组已满 7 项 ⇒ 在既有的两阶段收集**阶段二**
    用 `world.get` 逐实体判定（零重构，输出序 = 收录实体的查询序）。
  - **C1 命令层**：直抄光源编辑模板的四步形态（`push_undo` → 改资源 →
    `set_status` → `refresh_hud`）：`DdgiEditMode{Inert, Grid, Budget,
    Debug}`（`X` 循环，默认 Inert = 既有键位零变化）+ `ddgi_spacing_step`
    / `ddgi_bounds_translate`（`bounds = None` 时先按引擎同式物化）/
    `ddgi_probes_per_frame_step` / `ddgi_rays_face_step`（四档循环）/
    `ddgi_hysteresis_step` / `ddgi_overlay_toggle`。**`WorldSnapshot` 增
    `ddgi: Option<DdgiSettings>`**：`DdgiSettings` 缺席时第一条步进命令
    = 插入默认资源（「启用 DDGI」本身是可撤销的一步）。**明确不做数值
    输入面板**（`edit.rs` 既有拒绝：牵连 §8.33② 的 `LevelRegistry`
    语义变更评审）。
  - **C2 可视化**：探针标记球（`uv_sphere`，R58b 极点坑已修）走主 pass
    （零新 pass / 零新 binding），`EditorTransient` + `BakeExcluded` 双
    标记；三档着色 = 脏红 / 粗填琥珀 / 稳态绿（数据源 `dirty()` +
    `probe_stamp()` 只读零成本）——**粗填琥珀档在当前状态机下为空集**
    （安装帧全表标脏、精修即绿，「非脏且 stamp == 安装帧」恒不成立；
    判定保留 = 数据面完备），验收图实际呈现红 / 绿收敛前沿。维持
    「手动重建、不自动刷新」的既有立场（与 Nav 同源）；参数步进与
    toggle 时重建。
  - **C3 入档**：`LevelSection.ddgi: Option<DdgiSection>`（逐字段
    `#[serde(default = "…")]` + 整段 `skip_serializing_if`，
    **`SCENE_FORMAT_VERSION` 不 bump**——nav/bake 子段 + `MaterialSection`
    字段级三处先例）；`geometry_generation` 不入档（运行期记账）；加载
    段 → `DdgiSettings` 资源（校验前置拦截为 `SceneError`），保存从
    **活动资源**回写（照 `DirectionalLight` 先例；`LevelRegistry` 的加载
    时快照语义对 ddgi 段例外——编辑器步进改的是活动资源，快照回写会让
    Ctrl+S 静默丢参数）。**探针表持久化维持拒绝**（§8.57⑭）。
- **桶 B（Fixed）**：
  - **B1 SSR 强度死旋钮（R62b 引入，26 轮未发现）**：`SsrSettings::
    intensity` 上传到 `ssr.flags.y` 后 WGSL **零读取点**——模块文档里的
    合成公式多了「强度」项。修法：命中权重
    `clamp(roughness_fade × edge_fade × intensity, 0, 1)`（强度 0 ⇒ 与
    SSR 关逐位一致；> 1 钳到 1——命中权重、不是亮度倍率）。配套 CPU
    镜像 `post::ssr_blend_weight`（此前只有两个分量镜像、合并权重无
    镜像 = bug 藏住的结构性原因）+ 非默认强度断言。**零基线风险的依
    据**：全仓每一处 `SsrSettings` 构造都走 `..Default::default()` ⇒
    `intensity` 恒 1.0 ⇒ 乘进去逐位恒等。沉淀 AGENTS.md 测试规约一条：
    每个公开旋钮至少一条非默认值断言。
  - **B2 `reset_taa` 漏清跨帧时域状态（iris-exp frictions ⑬）**：语义
    定稿为「清一切跨帧时域状态，除了探针域」——补清
    `ao_depth_frame`（GTAO 切镜残影：切镜后首帧仍按旧场景深度算可见度，
    旧物体形状投出假遮蔽并被 TAA 吸收拖尾）与 `prev_view_proj`（消费点
    `unwrap_or(vp_raw)` 的首帧语义 = 零速度透传，等价「无历史」）；
    DDGI 表 / `probe_stamp` / `structure_generation` / `ddgi_upload` 是
    **故意不清**的那一类（探针域状态与镜头无关，锚
    `reset_taa_does_not_disturb_ddgi` 钉住）。不调 `reset_taa` 的宿主
    逐位不变。
- **确定性**：`cov` 求值 = 固定序循环 + `f32::max`（全序、无迭代序依
  赖）；CPU 镜像 `ddgi_coverage` / `ddgi_blend_with_cov` 与 WGSL 同式同
  序（源码层看门扩展到 `DDGI_COVER_FADE` 常数与 mix / ambient 行的形态）；
  编辑器步进 = 对 `DdgiSettings` 字段的确定性算术；网格重规划复用 R88
  的整数二分；双会话 48 帧锚自动覆盖新混合式。`DDGI_COVER_FADE → 0`
  的极限自检锚 = CPU 纯函数参数化入口
  （`ddgi_coverage_with_fade`：fade→0 ⇒ cov 退化为球内指示函数 =
  R88 硬判据形态；WGSL 侧常数是编译期固化的，GPU 级退化锚不可构造）。
- **验收**（`tests/ddgi_render.rs` 增至 11 项 + CPU 单测 / headless /
  level 子段测试）：R88 六项原值通过（既有测试零改动）；R89 五项 =
  ①cov=1 内区新基线 + DDGI-off 对照 R88 原值（1 字节 LSB 归因见上）；
  ②图集互斥（全场景挂图集 + ambient=0 ⇒ 开/关逐位一致 + 撤绑定可判
  别）；③SSR 强度旋钮（0 ≡ 关、0.5 可判别、粗填/收敛 ⇒ 反射色随表变）
  ；④`reset_taa` 不扰 DDGI；⑤`reset_taa` 清 GTAO（首帧 `ao_on == 0` 且
  像素 ≡ AO 关帧）。验收图：`docs/ddgi.png`（R88 四面板重生成——对照
  度因去双计反而增强：漏光差 117→210）+ `docs/ddgi_v2.png`（新 2×2：
  关 | 羽化 | 漏光 | ambient 去双计——内区 ROI 在 ambient 拉高 40× 下
  **逐位不动**、球外可判别，A3 的锐利像素证据）+ `docs/ddgi_editor.png`
  （编辑器叠加层 + HUD 状态行）。

**诚实限制**：§8.58。

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
`spawn` 自动退化为 `inline` 同步后端（同 API 同语义；R59 起两后端共用
同一优先级队列，唯一契约差异是读取发生在出队 `poll` 调用——耗时计入
poll 而非 request，见下方 R59 增量）。

**R59 增量：manifest + 优先级出队 + 续载语义**（§4.28 是本轮资产管线的
完整设计记录，此处补管线骨架层的语义修订）：

- **优先级出队**：`request_with_priority(path, kind, priority)`，
  `priority: i32` **越小越先加载**（缺省 0 = `request` 的既有行为）；
  出队键 = `(priority, seq)` 升序，`seq` 是入队调用序的单调计数
  （**在调用线程分配**——排序键在入队时即确定，与到达加载线程的先后
  无关，`AssetLoader::next_seq` 原子计数）。加载线程主循环 =
  **排空命令通道 → 弹最小键作业执行**：尽最大可能把「已入队」请求收进
  同一批排序；串行执行 ⇒ **在途作业不抢占**（后到的高优先级请求等当前
  作业完成）。`inline` 后端 `request` 入队、`poll` 出队并即时执行——
  与线程后端共用同一 [`PendingQueue`]（`BTreeMap<(i32, u64), _>`，
  键即出队序，与 broadphase 同一确定性选型）；**契约修订**：R45 的
  「inline 耗时计入 request」改为「耗时计入 poll」（同 API 同语义的
  代价；同优先级下结果仍按请求序 FIFO，既有调用方无感）。线程后端的
  确定性出队序测试靠**测试闸门**（加载线程每次出队前等待，生产恒
  `None`）把「请求全部入队」与「线程开始消费」定序。
- **中断 / 续载**：loader **无记忆**（无缓存 / 无去重 / 无完成登记）。
  丢弃 loader（native 通道关闭线程自然退出；inline 未出队请求随结构体
  丢弃）即在途请求作废——v1 无 join 语义不变。**续载 = 消费方持已得
  结果、对未完成路径重新 request**：同路径重请求即幂等重读（字节一致），
  已取回结果归消费方、不重复投递。两后端各有「部分完成 → 中断 → 重建
  → 补齐」全流程测试钉住。

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

> **阅读约定（2026-09-18 闭合审计补）**：本清单三类条目各有状态——
> ①「已修复/已闭合」标注的 dogfooding 摩擦条目（12/13/15/18/22-V/26、43）是
> **档案记录**，保留原文作历史，不是现存缺口；②「拒绝清单 / 裁定 / 定义性边界」
> 是主动设计决策，**「闭合」它们反而破坏意图**；③标注「留后续轮 / 候选」的才是
> 排期债（对应路线图闭合轮池）。另：条目编号「22」历史上重复出现两次
> （dogfooding V 与景深/MB），既有引用消歧口径 = **带 ①~⑥ 子目标号的 §8.22x
> 一律指 dogfooding V 条；指明景深 / 运动模糊语义的指后一条**——不做全文重编号
> （CHANGELOG 与模块文档的引用都锚定现编号，批量漂移风险大于消歧注记）。

1. **并行调度为批次粒度 + 数据并行分片（R73 起），无跨帧驻留线程池**：
   每帧按批创建线程（批内系统少时开销可忽略）；数据并行已由分片并行
   系统闭合（§3.7，两段式写回契约），但**跨帧常驻线程池 v1 不做**——
   归因是类型系统层面：常驻 worker 只能收 `'static` 任务，而并行/分片
   闭包需借用 `&World`（非 `'static`），安全 Rust 中非 `'static` 值无法
   放入任何 `'static` 容器与长命线程通信（channel / Mutex / Arc 全被
   借用检查阻断）。可行解锁路径：`Arc<World>` 所有权重构（App 持
   `Arc<World>`、独占系统走 `Arc::get_mut`、并行任务捕获克隆）——涉及
   全工作区 World 所有权 API 改动，收益（每批省约几十微秒 spawn）不抵
   风险，留作后续评估。分批为类型级保守分析。
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
6. **点光阴影为 v2 预算化多盏**（R61 起）：最多 `POINT_SHADOW_BUDGET = 4`
   盏 `shadows` 点光同时投影，超出预算按（相机距离升序, 实体位模式升序）
   确定性裁掉并 warn；预算选择只在进入 uniform 的前
   `MAX_POINT_LIGHTS = 8` 盏内进行。仍为**单采样无 PCF**（阴影边缘硬边，
   无 PCSS / 接触硬化）；投影灯与普通灯共享同一 8 盏上限；面矩阵约定
   （OpenGL 层序 + NDC y 翻转）依赖 GPU Cube 采样的方向语义——更换图形栈
   或 wgpu 大版本时需重新验证（集成测试
   `face_orientation_matches_cube_sampling` 守护）。逐灯阴影图集化、
   软阴影、按级联视锥二次剔除均在路线图。
7. **外部 HDR 环境源仅覆盖 Radiance .hdr 的 `-Y +X` equirectangular 形式**：
   其他分辨率朝向（±X ±Z 组合、垂直镜像）解析为 Unsupported；不读 `.exr` /
   `.ktx` 等其他 HDR 容器；超大图受设备纹理尺寸上限约束（Rgba16Float 中间
   纹理按原图尺寸上传）。程序化天空仍是缺席 `SkySource` 时的默认（零资产
   依赖）；烘焙在主线程同步执行，换入大贴图的当帧有一次性停顿。
8. **阴影偏移 v2 的残留限制**（R61 闭合了「常数 bias」条目本身，留下
   的是新口径的边界）：偏移量仍是**逐片元启发式**而非解析精确的
   depth-range 重建——`BIAS_MAX = 0.02` / 法线偏移上限 0.15 m 在极端
   掠射（NdotL < 0.05）与极远级联下会退化为「阴影变浅」而非「无影」，
   属保守方向；点光阴影仍是单采样（无 PCF），其 bias 口径与方向光同构
   但只有 1 纹素的容差余量。两者都不做 PCSS / 接触硬化软阴影、不做
   阴影去噪、不做逐灯图集化（拒绝清单）。
9. **动画**：骨骼蒙皮已覆盖刚性关节 LBS（GPU 蒙皮 + 蒙皮阴影）；
   ~~形变目标~~ **C4a 已交付 CPU 混合形态**（glTF morphTargets 加载 +
   逐实体权重 + 蒙皮路径动态 VBO，§4.6.3，v1 口径与边界见 §8.61）；
   CUBICSPLAT 插值与关节非均匀缩放的精确法线变换在路线图。
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
14. **粒子系统为 v1**（§4.17）：~~场景序列化留白（JSON 场景无粒子段，
    发射器只能代码生成）~~ **R57 起 v1 有 `particles` 段、C2 补
    `particles_v2` 段**（两轨配置字段全序列化，运行态不入档）；渲染实例每帧全量上传（粒子逐帧全变，无增量
    价值）；无碰撞 / 光照交互 / 软粒子深度淡出（贴地粒子可能穿插硬边）；
    剔除按发射器包围球保守整批裁决（单个巨池发射器跨视锥边界时会整批
    保留）；alpha 粒子排序粒度为逐粒子中心视深（与透明几何之间的先后
    由 pass 顺序裁决，粒子恒在透明几何结果之上）。（「burst 一次性、
    无重置 API」的限制已由打磨 III 闭合：`reset()` / `trigger_burst()`
    就地复用发射器，重复爆炸无需重建组件。**R82 修订**：本条描述的是
    **v1（`particles.rs`）**；v2（`ParticleEmitterV2`，GPU compute 模拟
    + 拖尾）为独立新增轨，二者并存互不感知——v2 的场景序列化同样留白，
    其自身限制见 §8.51 与 §4.38。）
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

16. **距离 LOD 为 v1**（§4.19）：~~距离用实体原点（非包围球中心，偏心
    网格的切换边界略保守）；**无滞回**~~ **C1 已闭合这两个子项**
    （§4.19）：
    `LodSettings` 资源在场 ⇒ 双阈值滞回（`hysteresis`）+ 包围球中心参照
    （`bounds_center`，high 网格 CPU bounds，缺席回退原点），**资源缺席 =
    v1 口径逐位不变**（滞回 `h = 0` 全 prev 形态退化锚）；滞回记忆只由
    主相机收集更新（RT / 捕获按当帧单阈值）。仍存：只有两级（无连续
    LOD 链，连续化走 R86 meshlet 层级）；切换即硬切（无 geometry
    morph / 渐隐过渡）；`LodMesh3` 地形三级不吃本设置。
17. **BC1 压缩为 v1**（§4.18）：仅 BC1（~~无 BC3~~ **R59 已补 BC3**——
    `declare_bc3` 声明门 + 分层互斥规则见 §8.35③/④；仍无 BC7——带 alpha 且未声明
    BC3 的纹理一律回退 RGBA8，显存优势丢失）；上传路径只在源尺寸 4 对齐时启用
    （非对齐纹理回退 RGBA8，编码器 pad 能力未接入上传）；~~无 mipmap
    （远距离细节闪烁没有 mip 链缓解）~~ **R66d 已闭合**：`MipSettings` 在场时
    `mip_chain_bc` 逐级降采样重编码成块流链（见 §8.43；缺席 = 单 mip 逐位不变）；
    编码器为确定性暴力端点选择
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
19. **盒投影贴花为 v1**（§4.20）：~~无光照~~ **C2 闭合直射项**：
    `DecalLighting` 资源在场 ⇒ 逐片元乘「方向光 Lambert + ambient」因子
    （法线由逆盒矩阵重建、朝相机翻正；§4.20）——**缺席 = 纹理 × tint
    逐位不变**；残留边界（声明维持）：不采样阴影贴图（遮蔽区内贴花仍按
    直射亮）、不吃点光 / 聚光 / IBL / 探针 / DDGI / GTAO（完整合成 =
    延迟 decals，维持拒绝）；平面假设（端盖是投影平面，深盒贴斜面贴合偏差上界
    `size.z/2`，基于深度缓冲重投影的屏幕空间 decals 在路线图）；不进
    阴影 pass；同批重叠贴花无视深排序（实体 bits 并列序）；场景序列化
    留白（JSON 场景无贴花段）；实例每帧全量上传（无帧级增量比对）。
20. **多视口与渲染到纹理为 v1**（§4.21）：RT pass 数量上限 2（小地图 +
    监控屏两个典型用例，超出 warn 跳过；**R81 修订**：上限只约束
    `RenderTargetStore` 的用户 RT 相机——引擎内部捕获视口（探针六面
    cubemap、水体反射 / 折射半分辨率图）走独立资源路径不占用户预算，
    修订理由 = 多视口编排（用户能力）与捕获（引擎内部实现）是正交
    维度，见 §4.37）；~~**无 `RenderTargetStore::resize`**
    （RT 尺寸创建后固定，尺寸变化需重建 store 条目——监控屏分辨率一般
    固定，读回 / 重分配留待后续）~~ **C1 已闭合**：`resize` 改 CPU 描述，
    渲染器按句柄比对尺寸、失配下一帧整体重建（重建产物 ≡ 新尺寸直建，
    GPU 逐像素锚 `rt_resize_rebuild_matches_fresh_target_bitwise`；读回仍
    不做）；**RT 相机朝向不同时 CSM 仍按主相机
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
22. **景深 / 运动模糊为 v1**（§4.11，R43；**编号 22 与上方 dogfooding V 条
    重复——引用消歧见 §8 头部阅读约定，本条 = 无子目标号、指景深 / 运动模糊语义的
    「§8.22」**）：**MB 只覆盖相机运动**——
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
24. **资产打包 / 挂载 / 异步加载为 v1**（§7.3，R45；**R59 修订**——优先
    级与中断续载语义已闭合（§4.28 / §7.3 R59 增量），取消仍未做）：
    **PACK1 无压缩、
    无去重、无加密**（CRC-32 防损坏不防篡改；同内容两份存两份，压缩率
    收益留给版本号演进）；**大资产整条读回**——`PackReader::read` 返回
    完整 `Vec<u8>`，条目内部分段流式读（边读边解码）未做；**挂载无
    写入 / 热重载 / 监听**；**Embedded 源仅文本**（路径 → 字符串表，
    二进制嵌入宏与 fetch 网络源在路线图）；**pack 内的 glTF 须自包含**
    （GLB 容器或 data URI——内存来源没有 base_dir，外部 uri 引用显式
    报错，见 `spawn_gltf_instance_from_bytes`）；**异步骨架只覆盖字节
    读取**——`AssetKind` 仅 Scene（后台 JSON 校验）与 RawBytes 两类，
    渲染纹理 / 网格的 GPU 上传异步化、进度回调与取消均未做（优先级出队
    已于 R59 闭合，§7.3）；
    **结果无完成顺序保证**（native 并行乱序到达），且丢弃 `AssetLoader`
    不等待在途请求（无 join 语义）；**glTF 加载器只有内存入口不支持
    外部 uri**，路径入口行为不变。**R59 后外部模型导入支线的加载器
    补全**（REPORT 补遗）：u8 关节索引（UNSIGNED_BYTE，常量笔误
    5120 → 规范值 5121）与 GLB 内嵌图片的 bufferView 形态（规范推荐
    形态，主流导出器默认输出；mimeType 前置校验 + 越界校验）均已支持，
    各配回归测试；诚实限制仍在：外部 uri、CUBICSPLAT、稀疏 accessor
    （~~形变目标~~ C4a 已加载 `targets`/`targetNames`，渲染口径 §8.61）。
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
    - **⑧（R67 补记）R74 帧数据类型不在 iris-render prelude**：
      `ClipWindow` / `ParameterTrack` / `window_active` / `sample_track`
      落库时只导出在 `iris_render::animation` 路径下，而 assault 自
      R48 打磨后刻意**不直依 iris-render**（渲染类型一律经
      `iris_engine::prelude` 直达）——帧数据注入在编译期即撞墙。
      → **闭合（R67 同轮，引擎侧最小修复）**：四项补入
      `iris_render::prelude`（纯再导出，零行为）；教训：新公开 API
      落库时同步评估 prelude 归位（R74 漏评估 = 埋给下一个实弹游戏
      的摩擦）。

27. **角速度物理 / 旋转感知胶囊 / SwingJoint 的诚实限制（R50）**——完整
    清单在 §3.8「v1 限制（诚实清单）」第 6–8 条，此处索引：①**接触不产生
    角冲量**（碰撞只改位置与线速度；落地不自转、偏心撞击不打转；角动量只
    由 ω 初值 + 角阻尼 + 约束方向修正演化——ragdoll 的翻滚是注入 ω 的表现
    而非转动动力学。**R76 修订：默认 v1 路径仍是本条原状；注册
    `ContactSolverV2` 资源的接触求解 v2 已闭合角冲量**（惯性张量 +
    接触点臂 + 摩擦锥，见 §3.8 v2 条目）——**opt-in 而非默认替换**，六个
    游戏不注册即保持 v1 轨迹逐位不变）；②~~**无 hinge / twist 两自由度
    铰链**~~（**C3 闭合**：`HingeJoint` 单轴铰链 + 扭转限位 + 速率限制
    马达，见 §3.8 C3 补记与 §8.59；`SwingJoint` 的
    锥限位不区分弯曲平面；摆动关节是层级投影，链须根先叶后）；③**ragdoll
    侧躺仍可能埋臂**（单胶囊躯干的肩窝在轴外 > 躯干半径；释放转轴已绕肩线
    规避主路径，实测埋深 0.028 m；根治需多胶囊 / OBB 躯干——v2 角冲量
    侧向缓解、结构性根治仍需 OBB 躯干）。
    另：**Box 仍世界轴对齐 AABB**（`rotation` 无语义，v1 固化）；旋转盒
    自 C3d 起用独立变体 `ColliderShape::OrientedBox`（SAT 闭式，见
    §8.60；ragdoll 的 OBB 躯干**能力已就位、接线未迁移**）；
    旋转体的碰撞用胶囊或 OBB；v2 惯性张量——盒类中 OBB 随体旋转、
    Box 仍世界轴对齐）。
28. **heightmap 地形 v1 的诚实限制（R51）**——完整清单在 terrain.rs
    模块文档与 §4.23，此处索引：①**碰撞是台阶不是光滑坡面**（盒顶按
    cell 阶梯，视觉网格连续；单 cell 高差超 step_height 的陡面照挡）；
    ②**无 skirt**（接缝防水密靠「边界线全采样 + y/法线/UV 逐位一致」
    不变量，粗级 chunk 边界内侧长条三角形与 T 型接点在掠射角有轻微
    法线插值差）；③**无滞回 / 无流式装卸**（LOD 阈值抖动逐帧来回切、
    全部 chunk 常驻，域外查询 `None`）；④**无坡度材质混合**（单草地
    纹理平铺，斜率染色留路线图）；⑤LOD 级数固定 3；⑥碰撞拼块线性
    扫掠（无空间索引），64×64 cell stride 1 = 4096 盒为典型上限，
    更大场景调 `collider_stride`。
29. **烘焙光照 a 阶段的诚实限制（R52a）**——完整清单在 bake.rs 模块文档
    与 §4.24，此处索引：①**只 0/1 次反弹**（`bounces > 1` 断言，无多重
    漫反射级联）；②**反弹二次阴影走硬影单射线**、命中点天空取开阔近似
    （深凹角反弹偏亮）；③**无面光 / 点光 / emissive / HDR 天空卷积**
    （天空 = 单色常量，`PointLight` 不参与烘焙）；④**半透明按不透明遮光**
    （玻璃照样挡光）；⑤**蒙皮实体与 `LodMesh`/`LodMesh3` 几何不参与**
    （只认 `MeshHandle` ⇒ R51 地形 chunk 暂不被烘焙，「取最高细节级」接入
    推迟——b 阶段规范未含，见 §4.24）；
    ⑥**无 UV2 / 无图集 / 无渲染集成**（输出是世界空间采样点列表，b/c
    阶段闭合）；⑦**单线程**（实测 1k 点 34 ms，越界才切 `points` 分段）。
30. **烘焙光照 b 阶段的诚实限制（R52b）**——完整清单在 bake_uv.rs 模块
    文档与 §4.24，此处索引：①**主轴投影不是共形展开**（曲面按面法线逐
    三角分类后投影有拉伸与撕裂；LSCM / xatlas 类参数化在路线图）；
    ②**重叠偏移是贪心 +x**（双层地板等重叠几何的 chart 面积可膨胀 2×
    以上，非最优装箱）；③**无 chart 旋转**（主轴投影已轴对齐）；
    ④**texel 法线 = 面法线**（无顶点法线插值，光滑曲面 AO 有 faceting）；
    ⑤**`texels_per_meter` 全图集单一值**（无按实体 / 重要度分配分辨率）；
    ⑥**pad = 1 texel 无 dilation**（黑 pad 在 c 阶段双线性采样下会轻微
    拉暗 chart 边缘——**已在 c 阶段闭合**：`dilate_atlas` 迭代扩散，见
    §4.24 c 与 §8.31⑥）；**LOD 句柄几何仍不
    参与**（沿用 a 阶段收集口径，见 §8.29⑤）。
31. **烘焙光照 c 阶段的诚实限制（R52c）**——完整清单在 lightmap.rs 模块
    文档与 §4.24，此处索引：①**动态物体不进烘焙**（收集口径只收静态几何，
    会动的实体挂上也是错位阴影 ⇒ 移动物体吃 IBL 环境间接光，与静态物体
    并存时有光照风格差；路线图：光照探针 / SH 体积）；②**蒙皮实体不烘焙**
    但蒙皮通路**同样绑 UV2 槽位**（手工注入 UV2 的静态蒙皮网格能采样，
    绑定姿势之外的姿态会错位）；③**透明普通路径不采样光照贴图**（透明项
    按视深排序合批，与图集分批冲突；玻璃的间接光仍走 IBL；透明**蒙皮**
    沿用蒙皮路径解析出的图集——每实体一次 draw，无分批冲突）；④**挂光照
    贴图的实体失去 IBL 镜面反射项**（光照贴图只存漫反射辐照度，`select`
    是整项替换 ⇒ 金属表面在烘焙场景里没有环境反射；**已在探针覆盖范围内
    闭合**：R62a 反射探针在场时镜面由探针提供、间接漫反射仍走贴图，无探针
    时维持本限制不变——见 §4.30「光照贴图协同」；屏幕内容的反射另由
    SSR 补充（R62b，§4.31——其命中与探针 / env 在同一镜面项上合成，
    挂图集实体的镜面同样可被 SSR 替换））；
    ⑤~~**无 mipmap**（单 mip 上传 ⇒ 掠射角 / 远处 texel 走样）~~
    **C2 闭合**：`MipSettings` 在场 ⇒ 线性域 2×2 box 链多级上传 + 三线性
    采样器（`mip_chain_half_bytes`；缺席 = 单 mip + 采样器逐字段一致，
    既有像素锁原值通过）；
    ⑥**dilation 是各向同性 4 邻域均值扩散、至多 8 轮**（距已覆盖 texel
    超过 8 的空洞中心仍为黑；chart pad 1 texel + shelf 无间隙 ⇒ 实际 1 轮
    闭合）；⑦**多图集场景按句柄切段绘制**（段数 = draw call 数，无跨图集
    合并）；⑧**无卸载路径**（恢复原网格需调用方自己留档旧 `MeshHandle`；
    UV2 副本常驻 `MeshStore`）；⑨**每实体一份网格副本**（UV2 是逐实体
    数据而网格是共享资源 ⇒ 共享网格必须分裂，副本内存计入
    `MeshStore::memory_bytes`）；⑩**LOD 句柄几何不参与**（沿用 a 阶段
    收集口径，见 §8.29⑤；挂了 `LodMesh` 的实体换 `MeshHandle` 副本也不
    影响渲染——LOD 优先，光照贴图静默不生效）。

32. **遮挡剔除与 FXAA v1 的诚实限制（R53）**——完整清单在 occlusion.rs
    模块文档与 §4.25，此处索引：①**剔除决策延迟一帧**（硬件读回异步
    的固有代价）：相机 / 场景快速运动时可能有一帧的漏画或多画，静止
    场景逐位一致；②**只剔普通不透明实例**：蒙皮 / 透明 / 贴花 / 粒子
    不参与查询、永不被遮挡剔除；③**每实例一次查询 draw**（4096 槽
    封顶，超出恒可见），数千实例时查询 pass 的 CPU 提交开销可能吃掉
    收益——v1 面向「大量静态遮挡物」场景；④**AABB 不是逐像素代理**：
    细长 / 凹形几何的包围盒空隙让它长期不被剔（只漏剔不误剔）；
    ⑤**被剔实体不再遮挡更远的对象**（主 pass 深度缺席）：链式遮挡
    场景可能多留 / 少画一帧（①的同类滞后）；⑥**RT 相机不参与**
    （多视口共享一份隐藏表会互相误剔，v1 只对主相机生效）；⑦**读回
    节拍依赖帧间 `device.poll` 推进**：headless 背靠背连跑 draw 时
    功能自动退化为全部可见（保守）；⑧**FXAA 是 console 质量档**：
    无方向搜索细化与二阶亚像素项，极细结构（1px 栏杆）仍可能残留
    走样 / 轻微软化；⑨**FXAA 工作在色调映射后的 LDR 域**：HDR 域
    抗锯齿已由 TAA 补齐（R60，§4.29）；⑩~~**TAA 未做**~~ **TAA 已做**
    （R60，§4.29）：历史缓冲 + 投影抖动 + 相机重投影三件套落地，
    单帧链架构的冲突以「状态收敛在链首单一 pass」解开；逐物体运动
    向量仍不做（鬼影钳制压制不消除，§8.36）。

33. **关卡格式 v2 的诚实限制（R57）**——完整清单在 level.rs / scene.rs
    模块文档与 §4.26，此处索引：①**关卡档与游戏侧常量可能双源**
    （assault 的墙 / 柱同时存在于关卡档与 `logic.rs` 常量——纯逻辑核心
    不读档是分层纪律；`tests/level_roundtrip.rs` 位级对照钉住一致性，
    单一事实来源留待编辑器轮 R58）；②**`LevelRegistry` 是加载时快照**
    （代码侧改 `Terrain` / `NavGrid` / 烘焙参数不回写——与材质「现读
    数值」语义不同，level 段的来源数据没有可现读的世界侧完整形态）；
    ③**无外置高度场文件**（.r16 / PNG 高度图无装载器，`inline` 数组
    大地形体积大；烘焙产物的 pack 打包工具链未接，产物经 `AssetMount`
    可读）；④**产物实体引用对加载序敏感**（IRISLMAP 按 `Entity::to_bits`
    引用——档面实体序变化即失配报错，重烘是唯一出路；这是「陈旧产物
    是错误不是降级」的自觉代价）；⑤**assault 关卡档不带 nav 段**
    （整块地面盒全覆盖导航域 ⇒ 顶面雕刻无种子整图不可走，§3.9 实测
    教训；其导航权威在 `logic.rs`）；⑥**EnvironmentSettings / 相机 /
    UI 不入档**（渲染调节旋钮与游戏表现留代码侧，v1 场景语义原样）；
    ⑦**粒子段只写配置**（运行期状态不入档）；⑧**无编辑器 GUI /
    无 schema 文件**（格式权威 = Rust 类型 + 模块文档，R58 补）。

34. **关卡摆放编辑器的诚实限制（R58a a 阶段 → R58b b 阶段）**——完整
    清单在 edit.rs 模块文档（12 条）与 §4.27，此处索引（标 ◆ 者为 b 阶段
    新增 / 收窄）：①**单选**（一次一个选中实体；多选 / 框选不在本轮）；
    ②◆**undo 覆盖扁平可编辑世界**（已实现：快照栈深度 32、dump 逐字符
    一致测试钉住；边界：存在 glTF 挂载根时快照栈清空——含根世界 undo
    恒为空、挂载根删除不可逆；undo 全删全建 ⇒ 实体编号改变——选中清空、
    以实体位为种子的粒子运行期表现不保证复原）；③**拾取与选中框基于
    世界 AABB**（旋转后的实体包围盒被放大，拾取判定比可见网格宽松；逐三
    角形精确拾取的引擎原语在位，留待需要时接）；④**无网格的实体拾取不
    到**（编辑器自己摆的点光带灯泡网格故可拾取）；⑤◆**`level` 段仍只
    读**（b 阶段新增烘焙 / 导航**入口**但参数只读使用——数值编辑面板需
    `LevelRegistry` 快照语义升级为现读 + 回写（§8.33② 语义变更，单独评
    审）；无 bake 段的档烘焙后产物落盘但档内无引用，重载不自动挂载）；
    ⑥**glTF 挂载根不可复制**（子树重挂载需重解析文件；可拾取 / 拖拽 /
    级联删除——但见 ②：含挂载根的世界无 undo）；⑦**属性面板只读**
    （8×8 ASCII 位图字体无 IME；b 阶段新增光源四 / 五键与 undo 深度、
    NAV 摘要行；非等比缩放仍无入口）；⑧◆**相机不入档；视角旋转已实现**
    （中键轨道 + `F` 聚焦；轨道俯仰钳 ±89°，f32 三角重建下半径逐位只在
    轴向特例成立、一般情形 1e-5 相对保持——测试同式解析对照）；⑨**摆
    放面窄**（四图元 + 五种无纹理材质预设 + 一种点光；GLB 摆放 / 资产
    浏览器 / 材质编辑器 / 地形雕刻在拒绝清单内）；⑩◆**烘焙产物绑定
    `open_level` 加载路径**（规范影子世界按编辑器同款初始化序（含瞬态
    资产句柄前缀）构建 ⇒ 编辑器存 / 载闭环成立；游戏侧裸 `load_scene`
    的句柄布局不同——产物会报失配，游戏侧消费需复刻同款前缀初始化或用
    档内参数重烘；格式以 (实体位, 句柄) 二元组为引用键是 R57 语义，路径
    无关的引用键属格式演进）；⑪◆**烘焙 / 导航不入 undo 栈**（幂等可重
    跑；烘焙清栈——跨烘焙 undo 会把在用的 UV2 副本从库里换掉；烘焙后
    编辑使产物陈旧 = 重载报错（R57「陈旧是错误不是降级」语义），重烘是
    唯一出路）；⑫◆**Nav 可视化是重建时点的快照**（后续编辑不自动刷新；
    自动 bounds 按轴对齐语义忽略旋转）。

35. **资产管线 v2 的诚实限制（R59）**——完整清单在 registry.rs /
    manifest.rs / async_loading.rs 模块文档与 §4.28，此处索引：
    ①**注册表只共享动画数据**（网格 / 材质 / 纹理仍逐 spawn 重新解析
    与登记——同一 GLB 多实例的纹理在 `TextureStore` 中是独立句柄重复
    像素，`GltfInstance::textures` 按次返回本轮新增的句柄列表；跨实例
    网格 / 纹理去重需共享句柄的实体树语义，独立工程）；②**共享剪辑
    不可就地改**（`Arc` 强计数 > 1 时 `get_mut` 为 None——修改剪辑走
    `clips_mut` 写时复制或 `AnimationPlayer::new` 独占构造；注册表本身
    的库内容只读）；③**BC3 只对声明纹理生效且加载期编码**（盘上无
    压缩容器——GLB 内嵌图是 PNG data URI，预编码需发明新容器，拒绝
    清单禁 KTX2/DDS；上传时一次 CPU 编码，与 BC1 路径同量级）；全不
    透明纹理即使声明 BC3 也归 BC1（分层互斥）；非 4 对齐 / 无 BC 特性
    自动回退 RGBA8（无分支回退）；④**BC3 是有损压缩**（alpha 单像素
    误差上界 255/14 ≈ 18.2，端点即块内极值像素的 8 值调色板口径；纯色
    alpha 块恒等还原；像素断言写上界不写「完全相等」）；⑤**优先级出
    队的并发边界**（在途作业不抢占——串行加载线程，后到高优先级请求
    等当前作业完成；「已入队请求按（优先级，请求序）确定性出队」由
    测试闸门定序钉住）；inline 后端读取耗时在 poll 而非 request
    （R45 契约修订，同 API 同语义的代价）；⑥**续载 = 消费方重发**
    （loader 无记忆：中断即在途请求作废，无 join / 无进度回调 / 无
    取消；同路径重请求幂等重读）；⑦**清单无递归 / 通配 / 依赖图**
    （平铺条目表，`"scene"|"raw"` 两类 kind；加载顺序由优先级整数表达，
    无显式依赖拓扑）；⑧**剪辑导入即全量解析**（无「只读名字」的惰性
    形态；GLB 动画数据量级下一次解析成本可接受）；⑨**GLB 重定向 /
    热重载 / 网络加载不在本轮**（拒绝清单）。

36. **TAA v1 的诚实限制（R60）**——完整清单在 post.rs 模块文档与
    §4.29，此处索引：①**无逐物体 / 蒙皮运动向量**（GBuffer 级工程，
    拒绝清单）——相机重投影对动态 / 蒙皮物体的历史采样位置错误，
    鬼影由 3×3 RGB 邻域钳制**压制不消除**（拖影约 10 帧量级指数
    衰减到 1/e，快移物体身后仍有可见残迹）；②**相机切镜不自动检测**
    （阈值检测是内容启发式，重置走显式 `Renderer::reset_taa`——宿主
    忘了调，切镜后一帧内全屏鬼影直至钳制收敛）；③**历史采样位置对
    透明 / 粒子无深度支撑**（深度副本不含透明 / 贴花 / 粒子，§4.11）
    ——它们按远平面重投影，运动透明物的残迹同样靠钳制兜底；④**静止
    稳态是 8 帧循环轨道**（当前帧贡献随 Halton 抖动循环变化，逐帧
    输出并非冻结——收敛后残差为 f16 精度量）；⑤**收敛前偏糊**（重置
    帧后约 10 帧内边缘过渡带还在收窄中，与 FXAA 的即开即用不同）；
    ⑥**视锥剔除边界随抖动 ±0.5 纹素**（与渲染同矩阵的代价，屏幕边缘
    物体提前 / 推迟半纹素被剔，与既有剔除近似同类）；⑦**SMAA / 抖动
    模式切换 / 采样数面板 / 历史有效性掩码**（拒绝清单，未做）。

37. **反射探针 v1 的诚实限制（R62a）**——完整清单在 probe.rs 模块文档与
    §4.30，此处索引：①**单探针硬切换**（重叠区按影响权重取一个探针，权重
    过渡带只淡化「探针 vs 全局 env」的切换，不淡化「探针 vs 探针」——两个
    权重都接近 1 的探针在交界处会出现反射内容的硬切；多探针加权混合在拒绝
    清单）；②**盒面近似**（视差修正对「落在影响域盒面上的几何」是精确的，
    盒内 / 盒外的物体按盒面重投影 ⇒ 距盒面越远、反射错位越大；典型用法是
    让盒贴着房间内壁）；③**捕获不含阴影之外的时间性 / 几何变化**（静态
    烘焙：场景或光照变化后需调用方重新 `bake_reflection_probe`，无脏标记
    自动重捕）；④**捕获含天空 ⇒ 与 `SkySource` generation 绑定**（换天空源
    后探针内容不再匹配，需重烘；探针捕获不自动跟随 `ensure_environment` 的
    重烘）；⑤**捕获不含透明 / 粒子 / 贴花**，与点光阴影捕获同一取舍；
    ⑥**点光阴影不进捕获**（探针域没有点光 cubemap 的槽位归属，逐灯重捕获
    代价高；方向光阴影**有**，按探针盒单级联拟合）；⑦**无磁盘持久化 / 无
    pack 集成 / 无关卡档集成**（每次启动重新烘焙；重捕获确定性由测试守护，
    持久化与 `level.bake` 集成留后续轮）；⑧~~**无探针编辑器入口**（R58b 烘焙
    入口的扩展留游戏工具轮；编辑器 undo 快照的组件集不含 `ReflectionProbe`
    ⇒ 探针组件在 undo/redo 后丢失，见 §8.34）~~ **C2 闭合编辑侧**：
    `M` 摆放（标记立方双写 scale）、`[ ]` 过渡带 / `- =` 半边长步进、
    探针进 undo 快照字段集（丢失 bug 一并修复）、关卡档 `probe` 段序列化
    （§4.27 / §4.30）；**捕获重烘仍属游戏 / 工具侧**（§8.37③ 立场维持，
    烘焙入口只报数提醒）；⑨**探针槽位不回收**（移除
    实体或改预算后槽位仍占用，预算 4 用尽后再烘焙会 warn 拒绝）；⑩**SSR
    / 平面镜 / 动态探针 / 探针烘焙图集化**（SSR 已于 R62b 落地——见
    §4.31，探针是其未命中回退通路；平面镜 / 动态探针 / 烘焙图集化仍在
    拒绝清单，未做）。

38. **SSR v1 的诚实限制（R62b）**——完整清单在 post.rs `SsrSettings`
    文档与 §4.31，此处索引：①**只反射上帧屏幕内容**（屏幕外 / 上帧被
    遮挡的内容不可反射——屏外标记的反射走探针 / env 回退，这是屏幕
    空间方法的定义性边界而非缺陷）；②**掠射大平面上的假命中**（反射
    射线近乎平行于地面时，其屏幕投影滑向远处的地面像素——射线点视距
    慢慢追上远处像素的存储视距即被判穿越，命中到地面自身的影像而非
    镜像内容；线性步进的经典伪影，法线锥 / hi-z 均在拒绝清单）；
    ③**厚度容差与斜穿漏判**（穿透 > thickness 的斜穿被判 miss——
    线性步进在「一步跨过薄结构」时不检测中间样本；thickness 调大可
    换取命中却引入前后混淆）；④**单采样无预滤波**（上帧颜色只有
    mip0，反射在粗糙端本应模糊、实际按权重衰减到探针 / env——衰减带
    内（0.25..0.6）的反射偏锐利）；⑤**MSAA 细节不在步进深度里**
    （深度副本是解析后的单采样语义，几何边缘的亚像素遮挡结构丢失，
    边缘处命中 / 漏判偏差）；⑥**反射内容是上一帧**（快速运动的反射
    体 / 相机有一帧滞后；TAA 历史不共用——它是混合产物非帧影像）；
    ⑦**透明 / 粒子 / 贴花不进深度副本**（同 §4.11 边界——它们的
    反射不可被命中，但它们**自身表面**的片元照常步进）；⑧**无逐物体
    运动向量**（动态物体的反射采样位置按相机重投影——物体自体运动
    的历史错位同 TAA §8.36①，由权重淡出兜底不做钳制）；⑨**hi-z /
    半分辨率 / stochastic / 时域滤波 / 湿面模型**（拒绝清单，未做）。

39. **预积分皮肤 SSS v1 的诚实限制（R63）**——完整清单在
    `iris_render::sss` 模块文档与 §4.32，此处索引：①**完全背光
    （NdotL = −1）响应恒为 0**（本模型建模的是**法线分布**导致的终结线
    软化，不含光在皮下横向扩散到背光面的穿透光晕——那需要厚度图 / 透射
    积分，在拒绝清单里；半背光侧 c ≳ −sin α 有可见包裹）；②**终结线附近
    增能**（按 c = 1 归一的代价，非严格能量守恒——Penner 2011 的既有
    取舍）；③**曲率尺度是逐材质常量**（不是逐顶点 / 逐像素曲率；逐顶点
    曲率烘焙在拒绝清单里——同一角色开 SSS 时头与躯干应给不同 `scale`）；
④**LUT 的 Lambert 折点重建误差 7.9e-3**（`t → 0` 时折点不可导，
    线性插值 O(h) 是下界；`t ≥ 0.05` 时 ≤ 1.4e-3）；⑤**跨平台位级复现
    只保证同一次构建内**（f64 libm 生成，不同 libm 实现间可能有 1 ulp
    差异——LUT 是引擎常量，没有跨机位级契约）；⑥**屏空间 SSS / 透射 /
    厚度图 / 各向异性高光 / 头发布料专用模型 / LUT 热替换 / 逐顶点曲率
    烘焙**（拒绝清单，未做）；⑦**「新增着色器代码会扰动既有 FMA 收缩」是
    本仓的新增工程约束**（R63 实测）：初版 SSS 补项在辅助函数里重算了
    `pbr_light` 的 `normalize(v + l)` / `fresnel_schlick` 子表达式，导致
    `shadow_v2_render.rs` 接触阴影对照场景 3 个字节 ±1 LSB 漂移（与 SSS
    逻辑无关，strength = 0 时补项不执行）；去掉复用后逐位复原。**改
    `standard.wgsl` 后必须跑全部 FNV 基线锁**，出现 1 LSB 级漂移时先查
    辅助函数与主路径的子表达式重合，而不是急着重锁。

40. **光照探针 GI v1 的诚实限制（R64；R83 修订①②）**——完整清单在
    `iris_render::irradiance` 模块文档与 §4.33，此处索引：
    ①**逐片元 GPU 插值**：~~（拒绝清单）~~ **R83 已实现**（§4.39）——但实现
    形态不是「四面体表上 GPU + 逐像素点定位」，而是给探针追加三阶 SH 后在
    片端做径向权重混合（无搜索）；v1 的逐实体路径保留作 mode = 0 兼容轨；
    ②**球谐 SH / 方向性探针（L1 及以上）**：~~（拒绝清单）~~ **R83 已实现**
    （三阶 = L0–L2，每通道 9 系数，E/π 直读）；③**可见性（occlusion）加权
    插值**（**维持拒绝**）：两个隔着墙的探针会通过共享四面体互相串味——
    探针布局必须自己避开薄墙两侧共胞的构型；R83 的径向权重同样不感知遮挡
    （影响半径是唯一的隔离手段，见 §8.52）；④**探针磁盘持久化 / pack 集成 /
    编辑器集成 / 动态重烘焙 / 脏标记**（拒绝清单，与 R62a 同立场：运行时
    烘焙即可）；⑤**几何是「真值 ± 1 µm」**（抖动法的固有代价）：规则网格 /
    立方体角点布局的凸包组合结构可能与理想构型不同（任何扰动法含 SoS 都
    如此），结果始终是**抖动点集的正确 Delaunay**；⑥**极扁平点云（全部共面
    且点数多）可能整体退化为空结构**（抖动阶梯三档都过不了插入期自检 ⇒
    整体回退全局 env）——实战建议给两层高度（本仓验收场景用 3×3×2）；
    ⑦**包外无距离衰减**：远离探针云的实体一直吃边界辐照（连续性的代价；
    R83 的逐片元支同语义——Σw ≈ 0 回退最近探针）；⑧**单线程烘焙**（与
    §4.24 同口径；切分点仍是逐探针独立；R83 的 SH 投影把单探针烘焙成本
    抬高约 16×，仍是单线程一次性支出）；⑨**探针自遮挡**：探针落在几何
    内部时烘出的是几何内部的辐照，需作者把探针放在空腔里；⑩**超预算按
    实体位升序截断**（不做重要性排序）。

41. **行为树 / 黑板 / 感知 v1 的诚实限制（R65）**——完整清单在
    `iris-core/behavior.rs` 与 `iris-core/perception.rs` 模块文档，此处索引：
    ①**不做** GOAP / 效用 AI / 层级 FSM、跨代理共享黑板、子树引用与热重载、
    可视化调试器 / 编辑器集成、树序列化入关卡档（结构是纯数据，序列化留后续
    轮）、装饰器全集（只 Inverter / Cooldown / Repeat 三个）、黑板键的编译期
    校验（键是运行时字符串）；②`Condition` **只读**黑板（有状态的判断请写成
    `Action`）；③无节点级「超时 / 重试预算」装饰器，需要时用 `Cooldown` +
    黑板计时；④树不可变——运行期换树 = 换组件，且运行态与树**必须同源**
    （不同源即 panic）；⑤`Parallel` 是**语义**并行，帧内仍按声明序串行 tick；
    ⑥感知遮挡**只看静态体**（`effective_static` 口径：`is_static` / 无
    `RigidBody` / `Kinematic`）——**动态刚体不挡视线**（v1 取舍：潜行玩法的掩体
    应是设计物而非物理副产物）；⑦感知无扇形记忆 / 目标预测 / 声音优先级 /
    多眼位 / 竖直扇形（朝向恒水平，俯仰靠 `eye_offset`），且 `heard` 没有对称
    的「离开听觉」事件；⑧感知**不看碰撞组**（谁挂 `PerceptionTarget` 谁可被
    感知）；⑨行为树的 `ActionId` 是运行时字符串——未知 id 在 debug 断言
    （`debug_assert!`），release 下动作判失败 / 条件判假，无编译期校验；
    ⑩`GuardCapabilities` 的两个演示能力默认关（缺席 = 旧行为），正式玩法启用
    前需要重调自动玩家脚本与难度预算（本轮只做框架与演示验收）。

42. **`set_subtree_hidden` 的组件注册前提（视觉上限轮摩擦，游戏侧绕行）**：
    该原语靠 `insert_registered` 给「无 `Visibility` 组件」的实体补写隐藏
    标记，而**组件存储未注册时插入静默失败（只 warn）**——注册需要
    `&mut World`，`&World` 做不到（§8.22⑤ 的设计边界，当时无碍：调用点
    总在世界已有角色 `Visibility` 之后）。本仓「先隐藏关卡图元、后创建角色」
    的顺序恰好踩中：旧图元照旧渲染并与新建筑 z-fighting（实景：4 m 宽的
    旧砖块墙盖住整面新墙，两轮排查才定位——第一轮误判为「新墙贴图错」）。
    **游戏侧修法**：让先创建的实体（庭院视觉根）显式挂一个 `Visibility`，
    一并完成组件注册。**建议**：若后续出现更多「早期隐藏」场景，可在
    `iris-core` 补 `World::register_component::<T>()`（`&mut World` 侧的一次性
    注册 API）——本轮未做（游戏轮不改引擎）。
    **✅ C1 已闭合**：`World::register_component`（幂等预注册）+
    `is_component_registered`（前提自检）落地 `iris-core`；并修正一处
    文档 / 代码落差——`insert_registered` 的**未注册分支**此前实为静默
    `None`（本条原文写「只 warn」，但代码该分支不 warn），C1 起真发
    warn 且消息直指预注册 API。游戏侧「显式挂 Visibility 先行」的绕行
    语义仍合法，各游戏下次触及时可替换为 Startup 一次性预注册。
43. **BC1 / BC3 压缩纹理单 mip（R66 诚实边界，✅ R66d 闭合）**：R66 的
    CPU box 链只对 RGBA8 回退路径生成——压缩块流没有逐像素语义，降采样
    需先解码再重编码。后果实测显形（iris-exp frictions ⑧）：4 对齐全不
    透明纹理（法线 / ORM 数据贴图）命中 BC1 恒单 mip，远距缩小采样无法线
    平滑 ⇒ 皮肤点状噪点（归因实验：关法线噪点消失、关 mip 不变、关阴影
    不变）。**R66d 修法**：`mip_chain_bc` 逐级 RGBA8 降采样后编码成块流
    链（止于 4×4——wgpu 压缩纹理 copy extent 须块宽整数倍），`MipSettings` 在
    场时三条上传路径同口径多级化；**缺席 = 单 mip 逐位不变**（兼容形态与
    R66 同款）。`gpu_bytes_estimate_mipped` 口径同步（BC 路径补块流链增量）；
    `mip_render.rs::bc_path_mip_chain_suppresses_minify_aliasing` 双向阈值锁。
44. **动画窗口 / 参数曲线轨 v1 边界（R74）**：①GLB 解析不产窗口与曲线
    （不在 glTF 标准内；glTF 侧自动化的 KHR_animation_pointer 类扩展
    不做），由 API 侧构造或 `clips_mut()` 注入（`Arc::make_mut` 共享时
    复制整表，只宜 setup 期调用）；②负 `speed`（倒放）不触发事件轨道
    （`collect_crossings` 以正向推进为前提）；③`parameter_values` 每帧
    整表覆写是**区间状态语义**（查询型），与事件的消息流（drain 型）
    刻意分离——把曲线当事件流消费是误用；④窗口 / 曲线采样只对 active
    剪辑生效（crossfade 源剪辑冻结、附加层剪辑不产——与事件轨道同
    口径）；⑤同刻并列 key 的区间左端取后者、并列点取前者（确定性
    规则，语义上是「重复 key 后者只作用于开区间」，见
    `sample_track` 文档）。
45. **引擎原生数据贴图仍走 BC1（R66e 声明门为 opt-in）**：无压缩声明
    （`declare_uncompressed`）当前只由 glTF 加载器为法线 / ORM 自动打上；
    引擎原生资产（程序化纹理 / 内置场景）的法线 / ORM 仍命中 BC1 决策树
    （内存优先，基线零漂移的兼容形态）——近距观察原生资产法线仍可能见
    块状伪梯度（frictions ⑨ 机理同款）。后续若原生资产也需近距质量，按
    句柄补声明即可（决策树前置否决，无管线改动）；BC5（法线专用双通道
    压缩）不做（体量取舍）。
    **C1 补充**：一步式登记口 `TextureStore::add_uncompressed_declared`
    就位（与 `add_bc3_declared` 同族）——原生程序化数据贴图
    （`CpuTexture::brick_normal_orm` 一类）登记即带声明，不必两步；
    **默认口径不变**（常规 `add` 仍走 BC1 决策树，既有基线零漂移），
    闭合面 = 声明能力可达性，不改任何既有资产上传路径。
46. **GTAO v1 的简化与伪影边界（R77）**：① **掠射角低估遮蔽**——sin 和
    简式水平角积分（非论文的 cos² 加权精确积分），法线接近视线方向时
    精确、掠射面（如远看地面）遮蔽被低估；② **深度断裂光晕**——法线由
    ±1 纹素深度差分重建，深度不连续处（剪影边缘）法线失真，遮蔽出现
    1–2 像素级光晕；网格对角线两侧 f32 深度插值舍入差产生 ~1% 微痕；
    ③ **光照贴图叠加过暗**——烘焙天空 AO 与 GTAO 在挂图集实体上叠加，
    静态角落双重变暗（强度由用户调低缓解）；④ **无空间/时域滤波**——
    dither 抖动残留细颗粒（半分辨率 + 双线性上采样天然柔化，无独立
    模糊 pass）；⑤ **奇数分辨率口径差**——可见度图半分辨率下取整，
    右/下边缘 ≤1 像素的采样坐标偏差（ClampToEdge 下不可见）；⑥ **透明/
    贴花/粒子不写深度副本**——其脚下的 GTAO 遮蔽由其后的不透明几何
    间接提供，自身不接收也不投射遮蔽。

47. **前向光源分箱与 SpotLight v1 的诚实限制（R78）**——完整清单在
    `iris_render::light_bins` 模块文档与 §4.35，此处索引：
    ①**SpotLight 不投影阴影**（点光 cubemap 阴影不覆盖锥光，shadow
    字段恒 0——手电筒场景的遮挡硬边照不亮影）；②**锥轴是显式世界
    空间向量、不随 `Transform` 旋转**（跟随旋转需逐帧读层级，v1 让
    作者直接改字段）；③**探针捕获与 RT 相机不渲染锥光与第 9+ 盏点光**
    （捕获绑定组恒绑哑灯表 mode = 0——捕获视口的 8 盏点光语义与 R77
    一致，屏幕 tile 只对主相机成立）；④**2D tile 对深度不敏感**（同一
    屏幕列的墙前墙后共享灯表——墙后的灯仍进 tile 列表，逐灯
    `dist < range` 早退保证正确性但白付求和成本，不做 3D 分箱）；
    ⑤**超容量按序截断**（灯表 > 512 条、tile 索引 > 1 MiB：按点光
    查询序 → 锥光查询序保留靠前的并 `log::warn`，不做重要性排序）；
    ⑥**分箱路径每帧重传三缓冲**（params + 灯表 + tile 索引约
    几十 KB @1080p，mode = 0 帧零 write_buffer——常态 ≤ 8 盏点光
    零锥光的场景零成本）；⑦**投影包围盒保守**（8 角 NDC 矩形是球
    剪影的超集，多覆盖的 tile 白付无效求和——正确性由
    `dist < range` 早退兜底，不追求 tight 覆盖）。

48. **天空 v2 与体积雾的诚实限制（R79）**——完整清单在
    `iris_render::fog` 模块文档与 §4.36，此处索引：①**雾深度与
    DOF / MB 同帧、与 SSR/AO 不同纪律**（雾在后处理链读 scene_depth
    ——同一编码器内本帧场景深度副本已先编码，读到**本帧**深度无滞后；
    原规格按 SSR / GTAO 的「主 pass 前读上帧」直觉写为上帧语义，实现
    时核实编码器顺序后按实际情况记录，**对规格的偏离声明**见
    CHANGELOG R79；链上无几何帧副本不跑，吃到陈旧深度，与 DOF / MB
    同款）；②**捕获 / RT 相机不吃雾**（后处理链只存在于主相机路径，
    探针捕获与 RT 视口无雾——RT 本就不画天空，语义自洽）；③**IBL
    env cubemap 不与大气联动**（反射探针的镜面与环境光仍是旧渐变内容
    ——同一 `SkySettings` 下主视口天空与探针内天空盒背景不一致；联动
    属探针重烘焙，归 R83 / GI 线）；④**大气为艺术化解析近似、非物理
    量纲**（平面指数大气 + Kasten 气团 clamp，无多散射 / 无行星曲率
    / β_R 蓝端经 33.1→52e-6 艺术化增强、亮度由 SKY_INTENSITY = 48
    常量折算——调参以观感为准，能量不守恒）；⑤**雾不参与 TAA 抖动
    前处理**（雾 pass 在链上 TAA 之后读链头色与同帧深度，抖动矩阵
    口径与几何一致，低频雾无需时域累积）；⑥**max_distance 截断**
    （超出上限的雾贡献不计，16 步预算下的艺术化折衷——超远几何不
    再向雾色收敛）；⑦**density/falloff 负值钳 0**（负密度按「无雾」
    处理是安全方向，不回退为增光）；⑧**HG 相位 g 钳 ±0.999**（|g| → 1
    分母趋 0，不暴露奇点给作者）。

49. **昼夜循环与风场 v1 的边界（R80）**——完整清单在
    `iris_core::daynight` / `iris_core::wind` 模块文档与 §3.12，此处
    索引：①**星空 / 月亮不做**（夜间只有 `ambient_envelope` 的下限
    底光，太阳项强度归零不发光——无月光光源、无星空天空盒）；②
    **SkySettings / FogSettings 不随相位自动联动**（大气强度、雾密度、
    泛光等渲染参数是用户手动资源，昼夜只驱动方向光
    direction / color / intensity / ambient 四字段）；③**Wind v1 无
    引擎内消费者**（R82 粒子 v2 / R90–R91 毛发 / R92 降水 / 闭合池
    布料与植被四线共用接口的**接口轮**——`DefaultPlugins` 不注册
    `Wind`，消费形态 = 下游轮读资源 + 调 `wind_sample`）；④**相位
    推进是 dt 积分**（f64 + 取模回绕无累积漂移，但可变步长的 dt 序列
    本身不可复现——确定性回放用固定步长）；⑤**太阳弧是单平面圆弧**
    （固定倾角 35° 的倾斜平面，无季节倾角 / 纬度 / 日期模型，方位年
    不变）；⑥**阵风只调幅值不旋风向**（风向恒为资源给定的水平方向；
    高度无关、单八度值噪声——无风切变 / 湍流细节）；⑦**包络为艺术化
    分段线性曲线**（正弦仰角驱动的色温 / 强度 / 环境光，非物理辐射
    量纲，调参以观感为准）。

50. **平面水体 v1 的诚实限制（R81）**——完整清单在
    `iris_render::water` 模块文档与 §4.37，此处索引：①**单水面**
    （多 `WaterSurface` 实体按实体位模式升序取首盏、其余 warn 忽略；
    多水面各自的捕获与合成留后续）；②**水下几何不裁剪进反射 / 折射**
    （捕获画全部不透明实例，无逐水面遮挡裁剪；水面以下的几何照常
    入图——深度吸收在合成时近似处理）；③**捕获半分辨率**
    （RGBA16Float、无 MSAA——低频近似内容的裁定，波纹扰动掩盖分辨
    率差）；④**无递归**（捕获不含水体自身——水面不会映出水面；透明
    / 贴花 / 粒子同样不进捕获，透过水面不可见）；⑤**相机入水无处理**
    （水面 cull None 双面渲染，从下往上看能见到波面，但无水下雾 /
    色调 / 全屏折射偏移——入水状态检测留后续）；⑥**水像素不写
    scene_depth**（下一帧雾 / AO 对水面像素用其后地面深度——近似；
    水体 pass 之后的消费者读的是「水面后的世界」深度）；⑦**波形 4 波
    等角无频谱**（固定方向固定衰减，非 FFT / 频谱合成——大浪 /
   涌浪形态不在参数表达范围内）；⑧**网格边缘露缝**（波高在边界处
    不收缩，波动的网格边缘会周期性露出下方几何——构图上让边缘出画
    或远离即可回避；岸边自适应网格留后续）；⑨**水面后的透明 / 粒子
    被水面覆盖**（水体 pass 在透明 / 粒子之后合成、a = 1 替换——
    水面前的玻璃 / 粒子会被水面像素覆盖；水体在 scene_depth 之后
    编码的链位是深度吸收同帧语义的前提，链位调整需整体重裁）；⑩
    **`height` 不随 `Transform` / 不可倾斜**（世界空间水平面；倾斜
    水面的镜像矩阵不再是对角反射，留后续）；⑪**CSM 按主相机拟合**
    （捕获朝向不同时阴影近似，与 RT 相机 §8.20 同款）；⑫**Time 缺席 =
    波形静止在 t = 0**（渲染器不持有内部时钟——测试 / headless 场景
    需自行插 `Time` 并推进）。

51. **粒子 v2 的诚实边界（R82）**——完整清单在
    `iris_render::particles_v2` 模块文档与 §4.38，此处索引：①**跨适配器
    位级不可保**（同适配器同会话内：固定 dispatch + 固定工作组 + 按索引
    写入 ⇒ 双会话逐帧逐位一致；跨适配器受 GPU 编译器 fma / 重结合自由
    影响——CPU 对照轨 = 镜像纯函数的解析对照单测，**不是**位级等价
    声明）；②**固定池恒定 dispatch**（4096 池满即永久停发，死粒子不
    回收索引——持续特效请复用实体 + `reset`；恒定 dispatch 意味着死
    粒子照常占顶点处理，换来的是会话内确定性；被钳掉的发射不占游标）；
    ③**拖尾为相机面向条带、无体积**（屏幕空间展宽，侧看不变细，
    billboard 同族语义；条带节自相交时 alpha 混合不排序——高亮特效
    建议 additive，alpha 模式自遮挡可能出现叠序痕迹）；④**GPU-CPU 镜像
    同步契约**（发射节律 / 出生哈希 / 欧拉步 / 拖尾环 / 风公式两侧必须
    同步改——镜像纯函数的存在意义就是测试侧对照 WGSL，漂移由单测
    拦截）；⑤**不写 scene_depth**（与 v1 / 贴花同纪律——下一帧雾 / AO
    对粒子像素用其后的世界深度）；⑥**风采样为发射器原点单点**（GPU
    侧当常值加速度；逐粒子空间变化留后续）；⑦**无视锥剔除**（粒子
    位置在 GPU，CPU 无逐粒位置——v1 有；`Visibility` 过滤照常）；⑧**帧
    队列上限 1024**（超限丢最旧——`alive` 估计随之失真，如实声明）；
    ⑨**GPU 池按实体绑定、销毁不回收**（每个曾出现的 v2 发射器实体占
    一个 ~2.3 MB 池；`reset` 触发一次全量零上传）；⑩~~**无场景序列化**
    （v1 的 `ParticleSection` 不覆盖 v2，后续轮按需）~~ **C2 闭合**：
    `particles_v2` 段（全配置字段 + 拖尾两参数，运行态不入档，
    §4.38）。

52. **GI 探针三阶 SH 逐片元化 v1 的诚实边界（R83）**——完整清单在
    `iris_render::irradiance` 模块文档与 §4.39，此处索引：①**无可见性
    （occlusion）加权**（§8.40③ 维持拒绝）：径向权重不感知遮挡，隔着薄墙
    的两个探针在墙两侧互相串味——影响半径（自动 = 最近邻 × 1.5，或
    `IrradianceProbeRadius` 覆盖）是唯一的隔离手段（**R88 已在 DDGI 路径
    闭合**：mode = 2 的 chebyshev 深度项消灭该串味，见 §4.44 / §8.57；
    R83 的 mode = 1 烘焙探针路径**原样保留**本限制——烘焙产物不带深度表）；②**无去振铃窗**：L2
    截断在强对比角落可振铃出小负值（求值出口钳 ≥ 0 是安全侧兜底，不是去
    振铃）；③**三阶截断**：L3 及以上的方向细节（窄窗、强点光源的高频）
    表达不了，E/π 的方向分辨率上限就是 L2；④**求积是 6 面 × 32² 中点规则**：
    固体角权重精确但方向采样有限，亚纹素级的辐射率尖峰（如细灯管）会被
    平均掉；⑤**跨适配器位级不可保**（同适配器同会话内：固定循环序 + 固定
    累加序 ⇒ 双会话逐字节一致；跨适配器受 GPU 编译器 fma / 重结合自由影响
    ——CPU 对照轨 `sh_blend` 是镜像纯函数的解析对照，**不是**位级等价声明，
    §8.51 同款边界）；⑥**mode = 1 帧每帧重传探针表**（满载 256 探针 =
    40 KiB/帧，与 R78 分箱三缓冲同纪律；探针集是烘焙产物、无 generation
    字段可比，逐帧比对哈希不会比直接写更快）；⑦**捕获 / RT 相机恒 mode = 0**
    （吃 loc14 的 SH 零阶载荷，与「捕获不渲染锥光与第 9+ 盏点光」同一立场，
    §8.47③ 同款）；⑧**SH 存储偏离规格的 128 B/探针**：规格写 7×vec4 线性
    打包，WGSL 无动态 swizzle ⇒ 每系数一个 vec4（10×vec4 = 160 B/探针，
    满载 40 KiB 而非 32 KiB）——求值从 81 次 select/探针/片元降到 9 次
    vec3 乘加；⑨**带 SH 集的逐实体载荷口径换了**（loc14 xyz = SH 零阶混合
    而非四面体插值）：只影响 mode = 0 的路径（捕获 / RT），主视图 mode = 1
    只读标志位；⑩**探针布局仍是作者责任**：半径自动派生假设探针云大致
    均匀，疏密悬殊的云里密区的探针半径会小到大片片元落进「最近探针回退」
    支（连续但失去混合的平滑）（**R88 缓解**：DDGI 路径下探针由规则网格
    自动生成、半径解析式派生，布局不再是作者责任，见 §4.44；R83 的手工
    放置路径原样保留本限制）。

53. **虚拟几何体 V1 的诚实边界（R84）**——完整清单在
    `iris_render::meshlet` 模块文档与 §4.40，此处索引（R84–R87 全案
    MVP 边界随轮写）：①**单 LOD 无简化器**：簇 `error` 恒 0.0（量化
    编码就位，R86 启用），无簇 DAG / 无层级、无简化 / 合并；②**无
    visbuffer、opaque-only、无软件光栅回退**（全案红线修订的落地面）：
    V1 经既有常规网格路径渲染，蒙皮 / 透明 / 带切线 / 带光照贴图 UV 的
    网格不在簇化范围（容器只载标准 32 B 顶点——仓内全部既有内容资产
    均被此边界拒收，见 §4.40 偏离声明）；③**compute 剔除未做（R85 已
    闭合，见 §4.41）**：V1 当轮边界锥 / 包围球元数据无消费者；④**非
    流式加载 → R87 收窄为「GPU 显存分页」（见 §4.43 / §8.56①）**：
    IRML 整文件仍先读进内存（open 一次读全量，磁盘级异步 IO 不做），
    但页划分（IRML v3）+ 驻留状态机 + 驻留感知选择已落地，pack 源
    的最小读取入口（`iris_engine::pack::read_meshlet`）随 R87 兑现
    （全接线仍不做，§8.56④）；⑤**无压缩 → R87 评估结论 = 维持不压缩
    （见 §8.56⑨）**：顶点 / 索引 / 簇元数据原样存储——页粒度流送依赖
    「页 = 固定字节区间」的直接寻址，压缩留格式 v4 评估；⑥**簇间顶点不去重**：边界顶点各簇各存一份
    （展开顶点数可比原网格多约 +70%，球体 48×32 实测；总量换逐簇
    自包含）；⑦**预算常量 256 被 u8 字段收窄到 255**：`vertex_count`
    是 u8（R85 storage 布局承诺），构建器按 255 卡——常量与字段的
    口径差已写入两边文档；⑧**跨簇绘制序不保证全局最优**：簇按构建序
    （种子序）排布，无按锥朝向 / 距离的簇排序（R85 起紧凑化后绘制序 =
    全局簇号升序，确定性成立、缓存局部性最优性仍不追求）；⑨**CRC-32
    跨 crate 复制**：与 `pack::crc32` 同一定义但代码独立（反向依赖会成
    环），漂移由往返 + 损坏单测兜住。

54. **虚拟几何体 V2 的诚实边界（R85）**——完整设计见 §4.41 与
    `iris_render::meshlet` 模块文档：①**回读一帧延迟**：紧凑列表本帧
    compute 产出、下一帧才绘制——相机快速移动时剔除决策滞后一帧，可能
    出现单帧缺簇（保守性不成立的方向）；跨世界切换共享渲染器时同理，
    切换首帧画的是上一世界的 meshlet 内容（一帧幽灵，随后的帧号配对
    检查不拦连续帧切换）；②**首帧 / 兜底形态**：无有效紧凑回读（首帧 /
    迟到 / 映射失败 / 跳帧）⇒ CPU 写恒等列表**全量绘制**（剔除只删不
    可见几何 ⇒ 兜底帧与剔除帧像素恒等，无视觉跳变；不走 expand 常规
    管线兜底——避免第二着色路径）；③**meshlet 路径间接光只吃
    irradiance_map**（全局 env 兜底口径）：vertex-pull 管线没有 loc14
    探针 / UV2 光照贴图的实例属性路径——反射探针镜面 / SSR / GTAO /
    雾等全屏或不依赖实例属性的效果照常生效，逐实体探针插值与光照贴图
    混合不适用于 meshlet 几何；④**遮挡剔除不在本轮（R87 已闭合，
    见 §4.43 与 §8.56③）**：视锥 + 背面锥已就位；R87 起 meshlet
    实例参与 R53 两阶段（实体级 AABB，逐簇 Hi-Z 仍不做）；⑤**跨适配器剔除边界位翻转可能**：剔除谓词在阈值
    边界（平面距离恰 = −r / dot 恰 = −sin θc）的浮点比较跨适配器无位级
    保证——边界簇可能一边剔除一边保留；同会话内恒定（会话内逐位不受
    影响），像素等价断言只对同适配器声明；⑥**16384 簇上限**：单帧枚举
    容量（IRML 构建期拒绝 + 注册处 warn 跳过 + 超限实例跳过三道闸），
    更大资产等 R87 分页（**R87 分页已落地，但单帧枚举容量本身未放宽**：
    16384 是「一帧能枚举多少簇」，分页解决的是「显存里常驻多少簇」——
    超大资产仍须靠 LOD 选择把单帧选中集压进上限内，见 §8.56②）；⑦**
    单材质**：meshlet 路径 v1 恒用默认材质（句柄 0）——单 instanced
    draw 单绑定组，逐实例材质需按材质切段（多 draw 或 storage 材质
    表），**R87 未纳入**（本轮范围 = 流送 + 遮挡整合），留后续；⑧**不投影阴影 / 不进
    scene_depth 副本**：阴影 pass 复用不透明实例区间，meshlet 实体不在
    其中——meshlet 几何不投影、也不吃 DOF/MB/TAA/SSR/AO 的深度副本
    输入（AO / SSR 对 meshlet 片元的屏幕空间输入仍生效）；⑨**固定顶点
    数展开**：每簇恒发 128 三角 × 3 顶点槽位，小簇（如 2 三角的地面
    quad）顶点着色器空转率高——MVP 换单 draw 简单性，按簇变长绘制留
    后续；⑩**非均匀缩放下的锥保守性弱化**：圆形锥经非均匀缩放后不再是
    圆锥，归一化 + 原 cutoff 可能漏剔或（理论上）误剔——均匀 / 近均匀
    缩放假设内使用（像素等价测试用均匀缩放场景）。

55. **虚拟几何体 V3 的诚实边界（R86）**——完整设计见 §4.42 与
    `iris_render::meshlet` / `iris_engine::meshlet_builder` 模块文档：
    ①**简化质量 v1 = 体素聚类非 QEM**：顶点聚类只保证位移上界
    （cell×√3/2），轮廓可能出现块状 dent（验收图中景球面可见）——QEM
    的堆序 / 半边结构确定性成本过高，本轮裁定接受；②**组间接缝残余
    T 型连接**：边界顶点锁定保证共享顶点不裂，但 coarse 侧可能用直线
    连接两个锁定顶点、fine 侧中间还有顶点 ⇒ T 型接缝微裂缝可能（像素
    级，目检未见明显伪影）；③**父组几何由子组自有几何继续简化**——
    误差沿 DAG 复合，group_error 的 max 链忠实记录上界，但相对「始终
    从原始几何简化」的方案质量略降（Nanite 式全量 DAG 留后续）；④**
    选择在 CPU**：逐实例逐帧 DFS，实例数 × 组数线性——GPU 侧选择
    （簇 error 进 cull 谓词）留后续，簇级 `error` 字段仍恒 0；⑤**
    非均匀缩放的误差口径**：error_scale 取最大轴缩放（与剔除半径同款
    保守放大），压缩轴上的有效误差被高估 ⇒ 可能偏细（保守方向）；⑥**
    层数上限 8 / 停止三角 128**：极端资产到顶后不再合并（远处保持中
    层精度）——**「非流式分页留 R87」已由 §4.43 闭合**（GPU 显存分页
    落地，磁盘级异步 IO 仍不做，§8.56①）；⑦**v1 / v2 容器兼容矩阵**：v1 文件
    照常读（全叶），v2 写出仅当层级非空（单层 `write_to` 保持 v1 字节
    布局 = R84 零漂移）；reader 接受 {1, 2}、其余拒读——R84 时代
    「版本 2 必拒」的断言随 bump 调整（版本 2 进入兼容名单，[0, 3,
    999] 仍拒）；⑧**LOD 选中集超单帧预算** ⇒ 实例级退化全叶（确定性，
    像素正确仅无收益）——不会绘制缺失，但 LOD 收益在超大场景受限
    （16384 簇 / 256 实例表条目上限延续 §8.54⑥）。

56. **虚拟几何体 V4 的诚实边界（R87）**——完整设计见 §4.43 与
    `iris_render::meshlet_stream` / `iris_render::meshlet` /
    `iris_engine::pack` 模块文档：①**分页是「GPU 显存分页」，磁盘级
    异步 IO 不做**——页数据源是进程内的 `MeshletMesh`，IRML 整文件仍
    先读进内存（`open` 一次读全量）；**arena 缓冲按全量容量预留**：
    页槽 = arena 规范区间（页号唯一决定落点），驻留裁决只决定「哪些
    字节有效可读」，**VRAM 占用不随预算缩小**——预算约束的是「分帧
    上传量 + 门控绘制」，不是显存峰值（packed 页池 + 页间重定向留
    格式 v4，代价是 cull / draw 两个 WGSL 的间接寻址改造）；②**页
    粒度固定 16 簇/页、不自适应**：极小资产（< 16 簇）整网格单页 ⇒
    流送退化为「全有或全无」（无粗化回退空间）；极大资产页数线性增长，
    页表与逐页请求的内存开销随页数线性（每页 4×u32 + 逐簇 4 B 组号）；
    ③**遮挡整合是实体级（v1）**：查询几何 = 实例 AABB（簇 bounds 联合），
    实例内逐簇遮挡**不做**——**逐簇 GPU 遮挡（Hi-Z / 深度金字塔类）
    缺席**，凹形 / 大 AABB 实例的漏剔率高于逐簇方案（只漏剔不误剔，
    保守方向）；④**pack 集成为最小入口**：`read_meshlet` 一个函数 +
    往返测试，**编辑器与 AssetMount 全接线不做**（`.imesh` 不进
    `AssetMount` 的扩展名分派，也不进关卡格式 / GltfBuilder 的自动
    簇化路径——挂载层要读 meshlet 需调用方显式走本入口）；⑤**请求
    一帧延迟**：驻留 tick 消费上一帧收集的请求（同 R85 紧凑回读节拍）
    ⇒ **首帧无驻留页 = 实例整帧跳过**（画面比 R86 少一帧内容，非
    闪面）；预算紧张时细化收敛需数帧（每帧 `pages_per_frame` 封顶），
    相机快速移动时粗化 / 细化切换滞后于视口；⑥**单层网格（空层级）
    无粗化回退**：页缺 ⇒ 整块跳过（无父组可上溯）——流送资产**须带
    层级**才有无缝洞语义，`--no-lod` 单层产物在预算下是「全有或全无」
    （其页的簇紧迫度取 `+∞` ⇒ 优先级最高、不会被饿死，但**要么全在
    要么全无**，无中间态）；
    ⑦**预算收缩时全保护页接受暂时超预算**：驻留页全被本帧请求保护时
    不驱逐（抖动比缺页贵）——`resident_bytes` 可短时高于
    `gpu_byte_budget`，预算不是硬上限；⑧**每帧上传字节无独立上限**：
    只有 `pages_per_frame`（页数）封顶，单页字节由资产页大小决定 ⇒
    超大页（顶点密集簇）资产的单帧上传量不可控（诚实口径：页大小 =
    16 簇 × 各自顶点 / 索引，实测中等资产页 ≈ 10–30 KB）；⑨**容器
    维持不压缩**：R86 的「压缩随 R87 流送评估」结论 = **不压缩**——
    页粒度流送依赖「页 = 固定字节区间」的直接寻址，压缩会破坏它，
    留格式 v4 评估；⑩**查询槽 4096 对合并列表统一封顶**：普通实例 +
    meshlet 实体共用 R53 的 4096 槽（§4.25 限制③），超出部分恒可见
    （保守方向）——meshlet 实体多的大场景下遮挡收益被稀释。

57. **DDGI 动态全局光照 v1 的诚实边界（R88）**——完整清单在
    `iris_render::ddgi` 模块文档与 §4.44，此处索引：①**单色天空**：
    `sky_color` 是逐通道常量，不吃 `SkySource`（程序化渐变 / HDR 全景被
    平均成一个常量——R79 的大气天空与 DDGI 的天空底色**不同源**，日落时
    地平线的暖橙不会进间接光）；②**无点光 / 聚光 / emissive 参与重照明**
    （`BakeParams` 无此输入 ⇒ 夜里点亮的火把不会在 GI 里留下痕迹，R89+
    候选）；③**1 次反弹上限**（`BakeParams.bounces ≤ 1` 现断言 ⇒ 门口那束
    光在暗室里只弹一次，深处的衰减比真实更快）；④**几何变化靠宿主
    `geometry_generation` 显式宣告，不自动侦测**（自动侦测要每帧全量收集
    三角形（ms 级）或遍历算指纹；照 `SkySource.generation` / `ensure_environment`
    先例。**移动的墙 / 开合的门在宣告前会投影出陈旧的遮挡**——DDGI 的卖点
    是「光照动态」而非「几何动态」）；⑤**光照突变后 `⌈n/k⌉` 帧才收敛**
    （默认 256/2 = 128 帧 ≈ 2.1 s @60fps；期间表是「新旧混合」——旧值继续
    用不清空 ⇒ 无闪烁，但日落瞬间的 GI 会滞后于直射光）；⑥**深度角分辨率
    6×8²**（面心 ≈11.25°/纹素，面角因切向拉伸更宽）+ **nearest-texel 查表
    无双线性插值** ⇒ 薄遮挡物在纹素边界上会跳变；⑦**σ² 每纹素只有
    `(rays_per_face/8)²` 条射线**（默认档 4 条）+ 相对 10% 下限钳 ⇒
    **厚度 < σ 的薄墙仍可能漏**（chebyshev 是概率上界，不是精确可见性）；
    ⑧**嵌入几何的探针不做 relocation**（贡献被深度项抑制 ⇒ 不会把墙内
    的黑色泄出来，但白占 1/256 的预算与一份重照明成本）；⑨**hysteresis
    默认关（α = 0）** ⇒ 滚动窗口推进时相邻探针的新旧差异可能有轻微空间
    撕裂（旋钮在，开它可换平滑但拖慢收敛）；⑩**跨适配器位级不可保**
    （GPU 编译器 fma / 重结合自由；同适配器双会话逐帧逐位由测试钉住，
    §8.51 / §8.52⑤ 同款边界）；⑪**CPU 单线程内联重照明**（未用 R73 分片
    并行——并行改累加序 ⇒ 需另做固定分片归约序论证；成本随场景三角数
    线性上浮，`rays_per_face = 16` ⇒ ≈0.83 ms/探针 @1000 三角基线，
    **昼夜循环场景是持续脏表 ⇒ 1.7 ms/帧是常态开销而非峰值**）；
    ⑫~~合成语义未修订~~ **R89 已升级为五方显式裁决并落地**（贴图互斥 /
    覆盖度混合 / ambient 分摊 / 捕获零阶 / SSR 一致性），见 §4.45；
    ⑬~~ambient 双计~~ **R89 已按覆盖度分摊**（ambient 行 `× (1 − cov)`，
    §4.45）——残留：`cov` 不判断可见性 ⇒ 墙外区域 `cov` 仍可能为 1
    （靠深度项在混合权重侧抑制，见 §8.58①）；⑭**无探针持久化**（每次启用重新粗填 + 滚动精修；
    与反射探针「产物只存 GPU」同款心智——R89 起参数经 `level.ddgi` 段
    入档、**表内容仍不持久化**，§8.58⑦）；⑮**片元阶段
    只读 storage 缓冲占满 4/4**（binding 16/17/18/20 = WebGPU 默认的
    `maxStorageBuffersPerShaderStage` 上限）⇒ **下一轮再加片元 storage 缓冲
    必须先申请特性或改用纹理**；⑯**着色器全表线性扫描**（≤ 256 探针、无
    逐探针空间加速结构，同 R83）：径向 × 深度双剪枝使 `w > 0` 才做 9 项
    SH 点积，但扫描本身与探针数线性；⑰**探针不落实体**（纯资源态）——R89 的编辑器可视化让网格**看得见**
    （标记球 + 状态着色，§4.45 C2），但**仍不可逐个调**：逐探针放置 /
    删除 / 排除格笔刷 = 「探针实体化」，在拒绝清单（§8.58⑥）；⑱~~扇贝形
    亮带~~ **R89 已用覆盖度羽化替代硬断界**（§4.45）：越过 `0.75·r` 等值
    线从「整片切到 env」变成 25% 半径宽的连续过渡。残留：羽化带宽是固定
    常量 25%、不随探针密度自适应；且 `bounds` 覆盖不到的表面（探针层高
    1.3、半径 2.4 只够到 y = 3.7，墙顶 y = 4 以上）仍然只有 env 天空兜底
    ——**羽化让过渡平滑，不凭空补光**（§8.58②），`bounds = None` 自动
    派生不保证盖住顶面仍是作者责任。

58. **DDGI v2 合成语义 + 编辑器入口的诚实边界（R89）**——完整清单在
    `iris_render::ddgi` 模块文档与 §4.45，此处索引：①**`cov` 是几何并集
    量，不判断可见性**（墙外区域 `cov` 仍可能为 1，靠深度项在混合权重
    侧抑制，而深度项是概率上界——§8.57⑬ 的残留面）；②**羽化带宽固定
    25%、不随探针密度自适应**，且 `bounds` 覆盖不到的表面仍只有 env
    兜底（羽化让过渡平滑、不凭空补光——§8.57⑱ 的残留面）；③**ambient
    衰减与 `cov` 共用一个量** ⇒ 无法单独调「去双计强度」（独立旋钮需要
    把 ambient 的 sky 源与补光预算拆成两个参数，留后续评估）；④**图集
    实体在 DDGI 在场时完全不吃动态 GI**（无局部混用，裁定 2 的代价，
    差值法拒绝见 §4.45）；⑤**捕获 / RT 相机的间接漫反射是零阶、无遮挡
    感知、无方向性**（mode 0 吃 `instance_payload`，低频量的裁定）；
    ⑥**编辑器叠加层「手动重建、不自动刷新」**（沿用 Nav 同款立场；
    参数步进 / toggle / 存档时重建）+ 与 Nav 叠加同开时可读性下降；
    三档着色的**粗填琥珀档在当前状态机下为空集**（安装帧全表标脏、精修
    即绿，「非脏且 stamp == 安装帧」恒不成立），判定保留 = 数据面完备；
    ⑦**DDGI 参数入档但探针表不入档** ⇒ 换机 / 换几何后首次启用要重走
    粗填 + 滚动收敛（§8.57⑭ 维持）；⑧**SSR 强度是命中权重钳位而非亮度
    倍率**（> 1 被钳到 1、0 ≡ 关闭——口径 R89 定稿，写进
    `SsrSettings::intensity` 文档与 `post::ssr_blend_weight` 镜像）；
    ⑨**`BakeExcluded` 是显式声明式排除**，不做几何类型自动判定（透明 /
    粒子 / 贴花仍进收集，与 R52a 同立场）；编辑器灯泡等「内容 + 视觉」
    实体的 mesh 视觉也靠它出收集（undo 快照已含该标记的复原）；⑩
    **`reset_taa` 语义 = 「清一切跨帧时域状态，除了探针域」**——DDGI 表 /
    stamp / generation 故意不清（与镜头无关，锚测试钉住）；`occ_frame` /
    `meshlet_frame` 等帧号记账是自愈型新鲜度机制、不属于「历史」。
59. **C3 物理深化役（CCD / 铰链 / 可断关节）的诚实边界（闭合战役）**——
    完整语义在 `physics.rs` / `character.rs` 模块文档与 §3.8 C3 补记，
    此处索引：①**CCD 只对照静态 / 运动学障碍**（动-动高速贯穿不检出；
    子步数封顶 `MAX_CCD_SEGMENTS = 64` ⇒ 保护距离 = 段长 × 64 有上界；
    起点已嵌入时每帧至多走一段；sensor 不阻挡也不为它停步）；
    ②**铰链马达是位置级伺服**——无扭矩 / 惯量语义（推不动的门照样被
    目标角拖走），旋转修正绝对覆盖、不读也不写 `angular_velocity`
    （绕轴转动的动力学惯性不在 v1，门板自身 ω 归游戏 / 接触）；
    ③**断裂判据 = 每帧位置修正累计量（米）**而非力——v1 位置投影制
    没有可读的关节力；旋转修正不带米量纲、不计入应力；断裂作用于
    **实体**（该实体全部关节一起清除，无按关节记账）；兑现走
    step 入队 + `prune_broken_joints` 移除的两拍（`&World` 系统改不了
    组件表的结构约束，非设计偏好）；④**角色 CCD 包装的重力按子步
    分摊**——与单次调用有浮点结合序差（新入口无 v1 逐位义务，仅保证
    自身确定；亚阈值位移逐位 = 原入口的兼容锚在单测锁定）。
60. **OBB（OrientedBox，C3d 专章 P1）的诚实边界**——完整语义在
    `physics.rs` OBB 段与形状文档，此处索引：①**obb×obb 接触点是
    中心连线中点**（`contact_point_rotated` 入口口径）——单点力臂对
    面接触偏粗，v2 角冲量在 obb×obb 上的扭矩分布是近似（obb×{球,
    胶囊, 盒} 走局部系复用各自的最近点近似，无此退化）；②**nav 不
    支持 OBB 顶面**（nav 静态体快照无旋转入参 ⇒ `covering_surface`
    保守 `None`，斜置平台不可作导航顶面；侧障高度 / 投影间隙取
    包围球 / 外接圆保守近似——只多阻不漏放）；③**恒等旋转非逐位
    路径**（obb(ID) 与 Box 数值等价 ±1e-5、命中性一致，但变换链乘序
    不同——opt-in 变体无逐位义务，既有 `Box` 场景零义务不变）；
    ④**准平行叉积轴跳过**（两轴夹角正弦² ≤ 1e-6 时该轴判分由面法线
    接管——游戏尺度角度误差 < 0.06°、穿透误差 < 1e-3·尺寸，标准 SAT
    取舍）；⑤**接触语义分两级**：obb×obb 相切 = 不接触（`overlap ≤ 0`
    分离，`box_box` 同式），而 obb×sphere 继承 `sphere_box` 的闭区间
    （恰相切报穿透 0）——下游 `SWEEP_CONTACT_EPSILON` 过滤本就抹平
    该差异，行为一致、声明留档；⑥**编辑器 nav 取景按 AABB 忽略旋转**
    （覆盖域仅视图框定用途）；⑦ 任意**凸包 GJK/EPA 维持缓行**
    （专章 P2，待消费方）。
61. **形变目标 v1（C4a，§4.6.3）的诚实边界**——①**只对蒙皮路径生效**：
    非蒙皮网格的 `targets` 照常解析进 `MeshData.morph`（CPU 侧可离线
    混合），渲染期权重组件在场也无效——静态路径按 (网格, 材质) 共享
    合批，逐实体形变要先分裂批次；表情动机场景（蒙皮角色脸）已覆盖；
    ②**CPU 混合 O(顶点数 × 目标数)/帧 + 整份 VBO 重上传**（脏检下
    静态权重零重传）：适合面部级小网格；大网格全量形变（肌肉 / 软体）
    的 GPU 存储缓冲形态缓行；③增量通道只取 POSITION / NORMAL——
    TEXCOORD 与切线增量加载期丢弃（切线按基座值蒙皮，形变脸的切线
    误差在法线贴图扰动尺度以下）；④权重不归一不钳制（glTF「作者
    声明的直接线性系数」语义，超界 / NaN 直通 = 资产错误，加载与
    渲染都不兜底）；⑤**视锥剔除与蒙皮包围界按基座姿势算**（morph
    增量不参与包围展开）——大幅位移形变可能被误剔，v1 假设形变量为
    面部级小位移；⑥meshlet / 地形路径不经手形变（IRML 展开几何无
    morph 表）；⑦`morph_gpu` 逐实体缓冲缓存只增不回收（与 `skin_gpu`
    同寿命口径）；⑧权重是运行期组件、不进场景 / 关卡序列化。
62. **状态机 v2 + 运行时重定向（C4b）的诚实边界**——①**一帧至多一条**
    条件转移 + 写回阶段步检 ⇒ 转移生效帧姿势仍来自旧状态采样（一帧
    软延迟，与游戏侧手调 `request_state` 同款）；多状态级联要逐帧排；
    ②守卫是封闭枚举（确定性 / 可序列化换来的裁剪）——需要历史 /
    随机 / 复杂表达式的判定仍在游戏侧（黑板照写，`request_state`
    手动入口常在）；③`ClipFinished` 粒度 = 「非循环且 `time ≥
    duration`」，无「末端前 N 秒预回边」概念（要预判用参数守卫自管）；
    ④`params` / `transitions` 是运行期声明，不进关卡 / 场景序列化；
    ⑤重定向**只重绑不改剪辑**（节点下标恒为源骨架空间）——源播放器
    `node_names` 是前提（GLB 实例播放器自带，手写空表恒 no-op）；
    归一化只处理声明前缀表（`mixamorig`/`bip01`/`mocap`/`ue4-skel`），
    语义改名不吃 alias 表就断绑，引擎不猜。

## 9. 关键设计取舍记录

- **回调式查询 vs 迭代器**：见 §3.4。牺牲一点语法糖换取零 unsafe 与确定性 panic。
- **稀疏集 vs Archetype**：见 §3.1。Iris 优先结构透明与动态组件能力；
  archetype 的成批迭代优势在实体百万级时才显著。
- **运行期借用检查 vs 编译期**：编译期（如 hecs 的 unsafe 实现或 slotmap 双缓冲）
  换不来「错误 = panic」的明确语义；运行期检查的每次查询只有常数次原子操作，
  开销在纳秒级，换取的是整库零 unsafe。
- **MSAA 固定 4x**：管线单份、代码简单；前向渲染下 MSAA 仍是性价比最高的抗锯齿。
