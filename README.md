# Iris Engine

一个用 Rust 编写的现代 3D 游戏引擎：**跨平台 · 内存安全 · 高性能 · 低占用 · 现代化**。

```rust
use iris_engine::prelude::*;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins::new("我的游戏"))
        .add_system(Stage::Startup, setup)
        .add_system(Stage::Update, gameplay)
        .run();
}

fn setup(world: &mut World) {
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world.resource_mut::<MaterialStore>().add(MaterialDef::solid(0.9, 0.4, 0.2, 1.0));
    world.spawn((Transform::from_xyz(0.0, 0.6, 0.0), cube, mat));
}

fn gameplay(world: &mut World) {
    let dt = world.resource::<Time>().delta_secs();
    world.query::<(&mut Transform, &Spin)>(|(t, s)| {
        t.rotation = Quat::from_axis_angle(Vec3::Y, s.0 * dt) * t.rotation;
    });
}
```

## 核心特性

| 特性 | 落实方式 |
|------|----------|
| **跨平台** | wgpu 统一封装 Vulkan / DirectX 12 / Metal（WebGPU 标准 API），winit 跨平台窗口；同一份代码编译 Windows / macOS / Linux，并可交叉编译至 ARM64 与 wasm32 |
| **内存安全** | Rust 所有权系统；实体「索引 + 代数」防悬垂引用；组件与资源运行期借用检查——错误访问立刻 panic 而非未定义行为；**引擎全部代码零 `unsafe`** |
| **高性能** | 稀疏集 ECS（遍历内存连续、缓存友好）；SIMD 数学库 glam；GPU 侧自动实例化批处理；**实例缓冲跨帧持久：静态场景零上传，动态实体仅传变化段**；**视锥剔除**（普通包围球 + **蒙皮关节变换保守并集 AABB**——凸组合保证只多画不漏画）；**FrameStats 帧统计**（draw calls / 可见实例 / 剔除数 / 三角形数 / 实例上传字节）；**并行系统调度**（声明组件读写集，无冲突系统多线程并发）；wgpu 流水线异步提交；**criterion 性能基准 + release 预算测试**（R33：CPU 热路径正式基准 `cargo bench` + CI 门禁，基线数字见 ARCHITECTURE §7.1） |
| **低占用** | 无 GC、无反射、无运行时；组件销毁即时确定性归还内存；发布构建 `lto="thin"` + `codegen-units=1`；**纹理 BC1 GPU 压缩**（确定性编码器 + 「特性 + 尺寸对齐 + 全不透明」决策树，显存 8 倍节省实测 64 MiB → 8 MiB，wasm / 无特性自动回退 RGBA8）+ **网格距离 LOD**（`LodMesh` 组件：近 high / 远 low，远景三角形 64x 削减实测，`triangles_drawn` 即验收口径）+ **内存占用统计**（`memory_bytes` 源数据 / `gpu_bytes_estimate` 显存估算，基准示例输出真实对比数字） |
| **音频** | 纯软件混音器（多声音线性叠加 + 线性插值重采样 + 软限幅，混音数学全纯函数、单测采样级断言）+ **3D 空间音频 v1**（距离软倒数衰减 + 恒功率立体声声像的纯函数增益模型，`play_spatial` / `set_listener` / `set_sound_position` 逐帧重算；`SpatialListener` / `SpatialSource` 组件 + PostUpdate 同步系统，组件移除帧间差分自动停止；身后不消音 v1 限制）+ **OGG/Vorbis 与流式音乐（R44）**（`AudioClip::from_ogg` 让短音效以百分之一体积走既有混音路径；`OggVorbisStream` + `play_music` 走**流式通道**——解码在专用后台线程分块产出、混音回调 `try_lock` 按需取块，**常驻内存与曲目长度无关**：60 s 立体声同内容 WAV 全量 22 MB vs 流式 0.41 MB（54×，`music_memory` 示例实测并断言）；块拼接采样级直通，逐 `to_bits()` 断言「输出序列 == 源序列」；解码器 symphonia 全家族 `#![forbid(unsafe_code)]`，不破「全库零 unsafe」）+ **音乐总线混响**（简化 Schroeder：4 comb + 2 allpass、整数环延迟线逐位确定、`wet = 0` 逐位透明旁路；v1 只挂音乐总线）+ cpal 设备层（实时回调 try_lock 不阻塞，无设备 / wasm32 优雅降级为纯混音器）+ WAV 加载（手写 RIFF 解析，16-bit PCM）+ 程序化音效（正弦 / 方波 / 噪声，确定性可复现） |
| **物理与碰撞（Sphere/AABB/竖直 Capsule 刚体，v1）** | 轻量刚体物理：重力（`gravity_scale`）/ 线性阻尼 / restitution 反弹（组合取双方 max）/ 近似库仑摩擦；`Collider`（静态/动态）+ `RigidBody` 组件模型（组件缺席语义：只有 Collider = 静态、只有 RigidBody = 仅受力积分）；求解管线「半隐式欧拉积分 → **均匀网格 broadphase 候选筛选**（BTreeMap 字典序 = 确定性；候选升序 + `j > i` 过滤使配对遍历序与 O(n²) 完全一致，bit 级确定性测试无需改动即守护等价性）→ 分离/反弹/摩擦响应 → 2 次迭代位置松弛」；检测与求解核心为**纯函数**（穿透/法线/反弹可脱离世界单测），`step_physics` 为 `&World` 并行系统（Update 内求解、PostUpdate 层级传播，天然正确编排），同输入两次模拟逐采样 bit 级一致；**静态推离查询**（`resolve_static_push` / `resolve_against_statics`，不写组件的纯查询）支撑运动学角色控制器——iris-game 只取水平分量即得「墙体阻挡 + 贴墙滑行」；**射线 / 线段查询**（`raycast` / `raycast_filtered` / `segment_hits_shape`——最近命中优先、`t` 并列取存储序先遍历者，线段扫掠判定防高速穿模）；**球扫掠 shapecast**（`sphere_sweep` / `shape_sweep` / `sphere_sweep_statics`——保守推进二分求首次碰撞分数与阻挡法线，角色控制器地基）；**旋转感知 Capsule**（R31 一等形状 + **R50 轴随实体旋转**：检测 / 射线 / 扫掠 / 推离 / broadphase 包络全链跟随，恒等旋转与 R31 逐位一致；**Box 仍世界轴对齐 AABB**——OBB/SAT 留后续轮，旋转体的碰撞用胶囊）+ **角速度积分（R50）**（`RigidBody.angular_velocity` 轴角式 + `angular_damping`，四元数微分式 `q ← normalize(q + ½(ω,0)q·dt)` 每步归一化防漂移，与平移不耦合、零角速度整段跳过 ⇒ 既有场景 rotation 逐位不变；**接触不产生角冲量**——角动量只由初值 + 阻尼 + 约束演化，诚实限制）+ **运动学角色控制器**（`iris-core::character` 的 `move_and_slide`：重力 → 水平滑行（最多 4 次扫掠迭代，切向速度保留）→ 台阶抬起重试（低台阶自动迈上、高墙照撞）→ 斜坡限（坡内 grounded、过陡下滑）→ 贴地吸附（下坡不腾空）、跳跃不吸附，纯函数确定性可单测）+ **接触事件与 Sensor 触发器**（R34：`step_physics` 帧末差分 `ContactTracker` 产出 `CollisionStarted` / `CollisionEnded`——实体序归一化（`a.index() ≤ b.index()`，法线方向随归一化翻转）、首求解轮采样、静-静跳过、销毁成对（接触中 despawn 下一帧必报携带已死 id 的 Ended）、先 Started 后 Ended 按对序输出且跨平台确定；`Collider::is_sensor` 触发区域照常报事件但跳过求解（不推离不反弹，与 `is_static` 正交），压力板 / 警报区 / 着地回调免逐帧轮询，`world.events_mut::<E>().drain()` 每帧消费一次，事件资源缺席静默跳过）；**约束与关节 v1**（R36：`DistanceJoint` 距离约束——`stiffness = 1` 硬约束刚杆 / `(0,1)` 弹簧 + 轴向相对速度 damping、`TargetJoint` 目标点约束——`rest_distance` 到达半径的球壳停驻；求解嵌入松弛循环**每轮接触之后**（约束优先级高于接触——结构完整性优先于穿透修正），速度响应只在首轮，收集序 = 组件存储序 ⇒ bit 级确定性不破坏；**SwingJoint 摆动关节（R50）**——本地锚点距离投影 + **摆动锥限位**（锥轴随父端旋转 ⇒ 解剖学口径）+ 子端肢体轴最小弧方向修正 + 两轴速度阻尼，修正全给子端（层级投影，根先叶后求解序下每轮末尾精确满足，实测残差 1e-7）；诚实声明：位置松弛投影无能量守恒（弹簧 = 视觉弹跳语义）、双向刚杆、锥限位不区分弯曲平面（hinge/twist 留后续轮）、target 无效 / 两端重合 / rest_offset 零长跳过不 panic）；**碰撞过滤掩码**（R37：`Collider.groups` 所属位组 / `mask` 碰撞白名单，默认全 1 与引入前行为逐位一致；配对判定对称 `(a.groups & b.mask) != 0 && (b.groups & a.mask) != 0`，被过滤对不检测不求解、不进接触事件集合（与静-静同待遇），broadphase 候选与遍历序不动 ⇒ bit 级确定性保持；**布娃娃与死亡混合（iris-core::ragdoll，R37 v1 / **R50 v2**）**：v1 球肢（六球 + `DistanceJoint`，逐位不变，iris-assault / infiltration 照旧）；**v2 胶囊肢人形（R50）**——十肢（躯干胶囊 + 头球 + 双臂双腿各两段），`RagdollLimbDef` 增 `half_length`（≤ radius ⇒ 球肢 = v1 分支）与 `axis`（近端方向 = 本地 +Y 的世界朝向）、`links` 升级 `RagdollLink { a, b, swing_limit }`（None ⇒ v1 距离关节；Some ⇒ 自动接线 SwingJoint：关节点 = 子端近端帽中心、rest_offset 在父端本地系给距离 rest 与锥轴）；释放时注入冲量相关角速度（转轴绕链根肩线前扑/后仰而非侧倒——侧倒会把靠地肩窝压进地面、手臂转不出来），`RAGDOLL_SPIN_FACTOR` 按「渐近总转角 = ω₀/λ ≈ 92°」取值让躯干恰好躺平；v1 语义回顾：「死亡混合」= 运动学窗口两阶段——`spawn_ragdoll_kinematic` 生成静态肢体（动画接管期：挡玩家/弹道，游戏侧照常驱动 Transform），`release_ragdoll` 翻转动体 + 继承速度 + 可选冲量踢飞（物理接管期：关节自动生效链不散架）；自碰撞免抖——肢体全部挂 `RAGDOLL_GROUP` 组位、mask 排除该位互不碰撞（生成时相邻球相切/微重叠，互相求解会被挤开炸裂），与场景照常碰；诚实限制：堆叠靠地面/场景（无肢体间挤压）、两具 ragdoll 互不碰撞、速度无钳制（无 CCD 冲量过大可穿墙）；240 步倒地链不散架、v2 摆角不超限位 + 全流程（位置 + 旋转）逐位确定性单测守护；视觉验收 `examples/ragdoll_snapshot.rs` → `docs/ragdoll.png`（v1）与 `examples/ragdoll_v2_snapshot.rs` → `docs/ragdoll_v2.png`（v2：台阶跌落腾空 + 摊倒两帧，headless 断言关节 / 穿地 / 悬空）） |
| **导航网格与寻路（R35 → R49 高度场语义）** | 静态场景的 3D 地形导航基础设施，终结「硬编码航点敌人」：`NavGrid::build`（入参 `&[(Vec3, ColliderShape)]` 与静态体收集语义同源）**表面解析 + 侧挡 + 可达顶面雕刻**——逐 cell 解析覆盖体顶面的**最高可站立面**（Box 顶 / Sphere / 竖直 Capsule 上端帽；**坡度 ≤ `max_slope_radians`（默认 45°）才可站**；重叠取最高；顶面在隐式地面 y=0 之下的埋没体不覆盖 ⇒ 大埋深球 = 露出地面的丘；无覆盖 → 隐式地面 0），`NavGridParams.step_height`（默认 0.3，与角色控制器同值）决定连通：相邻可站格 **|Δh| ≤ step** 才可走（台阶 / 自然下落），经 step 边链与地面连通的顶面格被**雕刻释放**、离地 > step 的孤岛高台保持被阻——**全平地场景位图与 R35 逐位一致（既有测试零改动全过）**；侧挡保留最近点判定 + 相切算阻 + 圆角膨胀 + `cell_size ≤ agent_radius` 经验规则的 R35 语义；`find_path` 五步语义——越界 None / 极近直达 / **端点被阻就近修正**（8 邻域 BFS 半径 ≤ 2 cell）/ **A\***（直线 1.0 对角 √2、**攀爬惩罚**上坡边 ×(1 + 2·Δh/step)、对角两正交邻必须 free 的**防穿角**、octile 启发对加权边可采纳且一致、`BinaryHeap<Reverse<((f, h), 节点序号)>>` + `total_cmp` + 严格更优写入的**逐位确定性 tie-break**）/ **string pulling 平滑**（cell 光栅 DDA + 高度场逐格校验的 **3D 视线**——贴地表差 ≤ step 的弦放行、穿台 / 悬空被挡下折点）；**航点自带表面高度 y**（末航点 = 目标 XZ + 目标 cell 表面高，游戏侧无需覆写）；`is_reachable` 同边集 BFS 轻量版；动态障碍 = 便宜整图重建（毫秒级纯函数，v1 无增量更新）；纯数据结构 + `Resource` 整图换入，同输入两次 build + find_path 位图 / 高度场 / 航点逐位一致（29 项单测，多层桥洞留白 v1 限制，AI 行为结构留游戏侧） |
| **Heightmap 地形（R51）** | 写实路线「复杂地形」正式解（`iris_render::terrain`）：`Heightmap` 规则网格高度场 + `generate_hills` 三频方向正弦叠加生成器（splitmix64 哈希派生方向 / 相位，**无 RNG**、同 seed 逐位一致，`to_bits` 测试锁定）；`height_at` 双线性（域外 `None`）/ `normal_at` 中心差分查询，`Terrain::height_at_world` 供游戏贴地逻辑；**分块网格 + 3 级距离 LOD**（`LodMesh3` 组件 = `LodMesh` 三级一般化，收集期按 chunk 原点到相机 eye 距离选级，`FrameStats::triangles_drawn` 即验收口径——实测每块 2048/576/224 三角形逐级递减）；**无 skirt 的接缝不变量**：每级 chunk 边界线恒全分辨率采样、内部抽行抽列（拉链条带三角化）⇒ 相邻块共享边折线顶点 y / 法线 / UV **逐位一致**（裂缝的垂直轴恒 0，9 种 LOD 组合单测锁定）；UV = 世界 XZ ÷ `uv_tile` ⇒ 跨 chunk 平铺无缝；**碰撞 = 自动粗 AABB 拼块静态体**（每 `collider_stride²` cell 一个 `Collider::static_box`、顶面贴 cell 最高角 ⇒ 台阶语义与 `step_height` 0.3 咬合；角色控制器 / 射线扫掠 / R49 导航图**零 API 扩展**直接可用，nav 边界须外扩地形域的衔接规则入文档）；`grass_textures` 程序化草地三件套（周期值噪声 → 基色 sRGB / 细节法线 / ORM，同输入逐位）；诚实限制：碰撞是台阶不是光滑坡面、无滞回 / 无流式装卸 / 无坡度材质混合、LOD 固定 3 级（§4.23 / §8.28）；验收 `examples/terrain_snapshot.rs` → `docs/terrain.png`（第三人称贴坡近景 + 高空 LOD2 全景，triangles 6112 → 1760） |
| **场景序列化（JSON 双向）** | 关卡数据与游戏代码分离：手写 JSON 场景文件（环境段：方向光 + HDR 天空源；实体段：Name / Visibility / Transform / mesh / material / point_light / collider / rigidbody，缺省值 = 引擎默认）↔ 世界实体（`load_scene` / `save_scene`）；mesh 三态（图元 cube/plane/sphere/cylinder、glTF 整实例挂载为子实体、纯逻辑实体）；`SceneAssetRegistry` 登记表驱动资源来源往返（图元参数 / glTF 与贴图路径在 load 时登记、save 时还原），非场景来源句柄优雅降级 + 告警；`load(save(load(f)))` 语义等价且二次保存逐字节一致（21 项单测） |
| **资产打包与挂载（R45）** | 中型游戏资产体量的载入形态（纯 std、零 unsafe、零第三方依赖）：**PACK1 单文件格式**（`PackWriter` / `PackReader`，`iris_engine::pack`——头 + 字典序条目表 + 连续数据区，同输入逐位同输出；CRC-32 校验防损坏；**大文件只进条目表不进数据区**，`seek + read_exact` 按需读、计数源测试锁定小读 IO < 1 KB）；**多源挂载**（`AssetMount`：文件系统目录 / pack / 编译期嵌入文本三源**先挂先命中**顺序查找、损坏立即报错不回退；可入 World 的可选资源——默认不注册，既有 `asset_path` 语义零变化；wasm 上 Fs 运行时降级、Embedded 是 web 资产通路）；**异步加载骨架**（`AssetLoader::spawn`：专用加载线程 + 命令通道 + 结果信箱，`request` 只入队、`poll` 非阻塞轮询、错误保真回传；wasm 自动退化为 inline 同步后端同 API 同语义）；场景挂载入口 `load_scene_from`（场景 + 子资产全走 mount，`load_scene` 文件语义逐位不变）；`pack_demo` 示例实测：打包开销 +0.4%、pack 随机读 ≈25 µs/次 vs 直读 ≈47 µs/次（设计见 ARCHITECTURE §7.3，诚实限制见 §8.24） |
| **演出时间线 Sequencer-lite（R47）** | 写实动作游戏剧情演出最小骨架（`iris_engine::timeline`，数据纯结构 + 采样纯函数）：**轨道时间线**（`Timeline`——相机轨道 `CameraTrack` 的 `(time, eye, target)` 关键帧，眼点与注视点各过一条 **Catmull-Rom spline**（张力 0.5、过全部关键帧、段间 C1 连续、端点重复夹持缓入缓出）或线性插值；事件轨道 `TimelineEvent` 跨过即发，跨界检测复用动画事件轨道的公共纯函数 `collect_crossings`（单一来源：回绕 / multi-fire / 末端不重复语义与动画逐位一致），命中经 `Events<TimelineEvent>` 产出、游戏侧 `drain` 消费（事件双代契约）；对话轨道 `[time, end_time)` 活动窗口文本，多条目声明序后者胜；黑边条 `letterbox` + 高度比可配）；**播放器 `TimelinePlayer` = Resource（每世界至多一个）**，`start_timeline` / `stop_timeline` / `is_playing` 托管入口 + `update_timeline` 每帧系统（Update 阶段独占，动画后物理前，早于层级传播与渲染收集）；**相机接管与恢复**：start 快照主相机原姿态（优先 `ActiveCamera` 实体局部 Transform、回落 `Camera` 资源——与渲染器解析一致），播放中逐帧覆写（实体模式写位置 + 注视旋转，-Z 前方与 `view_from_global` 口径一致），stop / 播完快照**逐位写回**无缝回游戏；**对话与黑边 UI** 复用既有矩形管线（固定 `Name` 的 `UiRect`/`UiText` 实体显隐、缺席惰性生成，文本水平居中于下黑边上方；位图字体仅 ASCII）；不自动拦输入、不暂停相机跟随（游戏侧自理）；同 timeline 两次播放逐位一致（14 项单测），视觉验收 `examples/timeline_snapshot.rs` → `docs/timeline.png`（12 s 演出三时刻拼图：spline 推镜、第 3 s 事件开门、第 5–9 s 对话、全程黑边），设计见 ARCHITECTURE §4.22，诚实限制见 §8.25 |
| **现代化** | WGSL 着色器；ECS 架构 + 插件系统；**变换层级**（父子级联 + PostUpdate 阶段传播：Update 写变换当帧即全局生效）；**实体可见性开关**（`Visibility` 组件，全部渲染 pass 与 UI 统一过滤，**层级级联**：任一祖先隐藏 ⇒ 子树全隐藏，组件缺席 = 可见）；**实体命名查找**（`Name` + 按名定位）；**实体挂载相机 + 屏幕射线反投影**（`screen_ray`：NDC 点 → 世界射线，拾取的基础操作；`Viewport` 视口尺寸资源随窗口缩放自动同步）；**glTF 2.0 资产管线**（.gltf/.glb + **节点动画 + 骨骼蒙皮** + **动画 v2**：剪辑间 **crossfade** 姿势混合（smoothstep 权重、平移/缩放 lerp、旋转 slerp、缺方通道不拖拽）、轻量**动画状态机**（states/edges + 过渡时长，条件判定留游戏侧）、**动画事件轨道**（跨界检测纯函数、循环 multi-fire 确定性、`take_events` 每帧消费）、**动画 v3**：**BlendSpace2D 混合空间**（任意分布参数点集、逐轴线性分解采样 ≤4 路非零权重和恒 1，与 crossfade/状态机互斥）+ **N 路姿势混合**（旋转 nlerp——首个四元数基准翻转加权求和归一）、**两骨足部 IK**（`foot_ik`：余弦定理膝角 + pole 弯曲方向、静态体射线落地、平地 bit 级不动、大球面斜坡脚踝贴面 ±2 cm 单测守护、注册在动画后/传播前）、**根运动**（根节点平移改累积帧增量、循环回绕三段精确累积、`take_root_motion` 消费交 move_and_slide）、**动画 v4**：**附加动画层**（AdditiveLayer——剪辑相对参考帧 t=0 的增量叠加：平移/缩放加法、旋转 slerp 压幅后右乘主姿势，整层 weight 压幅，只在主姿势已覆写通道路上生效，受击/呼吸不破坏移动循环）+ **骨骼遮罩**（AnimationMask——BTreeSet 节点集合的覆写过滤器，上半身瞄准层基础、与混合空间/crossfade/附加层正交，mask=None 全覆写照旧）+ **两骨手部 IK**（hand_ik——复用足部两骨解算纯函数，目标 = IkTarget 实体的武器握把全局位，不可达容差带内伸臂贴握把、超带放弃，双臂分实体配置互不干扰，注册序 动画→足部 IK→手部 IK→传播））；**PBR 材质**（Cook-Torrance 金属度/粗糙度工作流 + **法线 / ORM 贴图**（R42：切线空间法线扰动——顶点 TANGENT 或屏幕空间 dpdx/dpdy 推导 TBN 两路；ORM 打包图逐像素 AO / 粗糙度 / 金属度（glTF G/B 约定 × 标量因子），槽位缺省绑中性常驻纹理、渲染与无贴图逐位一致））；**IBL 环境光照**（程序化天空 / **外部 HDR 环境贴图** + 分割求和，Radiance .hdr equirectangular 手写解析）；**HDR 渲染 + 后处理链**（Rgba16Float 中间缓冲 + ACES 色调映射；pass 串联框架 v1：**景深** / **运动模糊** / 泛光 / **曝光** / **暗角** 有序链接，`DepthOfFieldSettings` / `MotionBlurSettings` / `ExposureSettings` / `VignetteSettings` 资源缺席 = 关闭 / 中性、默认画面零变化（FNV 基线逐位锁定），ping-pong 中间纹理按半 / 全分辨率分配复用）；**方向光级联阴影 CSM v2**（2048² × 3 层 2D 数组深度图 + PFS 分割 + 纹素对齐拟合 + 3×3 PCF 软阴影，覆盖距离 120 m 外保持受光）；**多光源**（1 方向光 + 8 点光物理衰减，首灯 cubemap 阴影）；**透明渲染**（alpha 混合材质 + 按视深排序的独立透明 pass：远→近 back-to-front、深度只测不写、不投影，无透明实例零开销）；**盒投影贴花**（`Decal` 组件：弹孔 / 血迹 / 地面标记，±Z 端盖代理几何 + 逐实例世界盒矩阵 / CPU 预计算逆矩阵 + 144B 实例，按纹理分批，蒙皮后透明前叠加，深度 LessEqual 只测不写，盒裁剪 + 1e-4 边界容差，无贴花零开销，v1 无光照）；**粒子系统 v1**（Emitter 组件 + CPU 确定性模拟：splitmix64 哈希流方向抖动、累计分数发射计数、逐位可复现；billboard 实例化渲染 + additive / alpha 双混合，alpha 粒子按视深排序，喷泉 / 爆炸预设，`reset()` / `trigger_burst()` 池槽位就地复用与命令式爆发）；**2D UI 叠加层**（HUD：屏幕空间矩形 + 内嵌位图字体文本，单管线单纹理单 draw，色调映射后叠加）；**UI 与菜单框架**（R41：九宫格**锚定布局** `UiAnchor`（纯函数 `anchor_to_position`，中心锚点改视口不漂移）+ **按钮焦点导航** `UiButton`/`Focusable`/`UiFocus`（Y 再 X 确定性排序，↑↓/←→/Tab 换焦、Enter/Space/鼠标点击发 `UiClickEvent`，焦点高亮原色保存恢复）+ **菜单栈** `UiMenu`/`UiMenuStack`（栈顶菜单可见、其余隐藏，**引擎不内置 Esc 语义**）；`UiPlugin` 随 `DefaultPlugins` 注册三个系统到 PostUpdate——上述 UI 与菜单全部**支持祖先 `Visibility` 级联**）；**设置持久化**（`Settings` Resource：主音量 / 全屏 / vsync / fov，`load` 缺失损坏字段回退默认、`save` 写 pretty JSON）；**多视口与渲染到纹理**（`RendersTo` 组件把相机输出到离屏 RT + `RenderTargetStore` 按需注册；RT pass：不透明+蒙皮+透明复用主收集与阴影图/CSM，无天空盒/贴花/粒子/后处理，ACES blit 到 Rgba8UnormSrgb 颜色纹理——小地图 / 监控屏玩法基础；`UiImage` / `UiImageRtt` 图片控件：`UiRect` 定位 + `UiColor` 乘色、Nearest 直角采样、图片之下文本之上；默认场景（无 RT）主渲染像素零变化）；derive 宏；4x MSAA；固定步长逻辑 |

## 快速开始

### 环境要求

- Rust 1.85+（`rustup update stable`）
- 支持 Vulkan / DX12 / Metal 的 GPU（软件回退亦可）
- **ffmpeg（资产工具链，非运行时依赖）**：引擎侧只做 Vorbis **解码**
  （运行时职责），不做编码器——音频资产的转换走外部管线，标准命令：

  ```sh
  ffmpeg -i in.wav -c:a libvorbis -q 4 in.ogg   # 16-bit PCM WAV → OGG
  ```

  编好的 `.ogg` 直接进 `AudioClip::from_ogg` / `OggVorbisStream`（短音效 /
  流式音乐两条路径），或经 `PackWriter` 打进 pack 归档（§7.3）。

### 运行示例

```sh
# 旋转立方体：纹理 + 光照 + 飞行相机（WASD / Space·Shift / 滚轮 / Esc）
cargo run --example cube --release

# glTF 资产加载：先生成示例资产，再加载渲染（外部 .bin buffer + 材质）
cargo run --example gen_asset --release
cargo run --example model --release

# 行星系：变换层级（太阳→地球→月球级联旋转）+ 彩色点光
cargo run --example orbit --release

# 4096 个实例自动合批：8 种材质 → 8 个 draw call
cargo run --example instanced --release

# 无窗口离屏渲染（CI 友好），双点光照明，输出 iris_offscreen.bmp
cargo run --example offscreen --release

# glTF 资产离屏快照（验收用），输出 iris_gltf_preview.bmp
cargo run --example gltf_snapshot --release

# PBR / HDR / 阴影验收场景（离屏渲染），输出 iris_shadow_preview.bmp：
# 金属球粗糙度梯度 + 绝缘体球 + 地面软阴影 + ACES 色调映射
cargo run --example shadow_snapshot --release

# 方向光级联阴影（CSM）验收场景（离屏渲染），输出 docs/csm.png：
# 近 / 中 / 远三距物体排开俯斜视——近处影子锐利、远处影子仍在（120 m
# 覆盖带之外正确无影）、级联过渡带无跳变
cargo run --example csm_snapshot --release

# 点光 cubemap 阴影验收场景（离屏渲染），输出 iris_point_shadow.bmp：
# 暗色房间中央点光，环灯物体的阴影背离灯的方向径向拉伸（点光阴影特征）
cargo run --example point_shadow_snapshot --release

# 泛光验收场景（离屏渲染），输出 iris_bloom_preview.bmp：
# 三颗自发光彩球的 HDR 光晕（阈值提取 + 半分辨率高斯模糊）
cargo run --example bloom_snapshot --release

# 后处理链验收场景（离屏渲染），输出 iris_post_*.bmp 三帧（→ docs/post.png）：
# 同一 HDR 场景 × {欠曝 0.4，标准 1.0，过曝 2.5 + 暗角 0.9 + 强泛光}——
# 曝光单调压暗 / 提亮，过曝帧亮源泛光扩散、暗角压暗四角而中心无损
cargo run --example post_snapshot --release

# 透明渲染验收场景（离屏渲染），输出 iris_transparent.bmp（→ docs/transparent.png）：
# 玻璃罩住自发光内核 + 红绿蓝三块透明板纵深 back-to-front 混合（透明物不投影）
cargo run -p iris-engine --example transparent_snapshot --release

# 盒投影贴花验收场景（离屏渲染），输出 docs/decal.png（640×360）：
# 灰墙 3 个弹孔（程序化同心圆纹理）+ 地面喷溅血迹（程序化不规则红斑）
# + 斜面旋转盒贴花——贴面不悬浮、边缘无光晕、任意朝向投影（蒙皮后透明前叠加）
cargo run -p iris-engine --example decal_snapshot --release

# 粒子系统验收场景（离屏渲染），输出 iris_particles_fountain.bmp /
# iris_particles_explosion.bmp（→ docs/particles_fountain.png /
# docs/particles_explosion.png）：喷泉（持续发射 + 重力回落，alpha 混合）
# 与爆炸（burst 一次性全向爆发 + 泛光辉光，additive 混合）
cargo run -p iris-engine --example particle_snapshot --release

# 2D UI 叠加层（HUD）验收场景（离屏渲染），输出 iris_ui.bmp：
# 点光阴影 3D 场景 + 顶部位图标题 / 血条 / 右下文本（矩形 + 位图字体单批渲染）
cargo run --example ui_snapshot --release

# 音频子系统验收场景（离屏渲染混音波形），输出 iris_audio.bmp：
# 混音器编排 boom/blip/噪声三段程序化音效 → 240 根渐变波形条（481 矩形单批），
# 有真实音频设备时同步真声播放一遍
cargo run --example audio_snapshot --release

# GLB 动画验收场景（先 gen_asset 生成资产），输出两个时刻的位图：
# 蓝色立方体自转 + 浮动（GLB 容器 + AnimationPlayer 播放链路）
cargo run --example gen_asset --release
cargo run --example anim_snapshot --release

# 骨骼蒙皮验收场景（同用 gen_asset 资产），输出两个时刻的位图：
# 两段臂绑髋/肘关节，肘关节挥舞 90°（GPU LBS 蒙皮 + 蒙皮阴影）
cargo run --example skin_snapshot --release

# 收集小游戏双时刻验收图（左：开局 / 右：拾取 3 枚后），输出
# iris_game_start.bmp / iris_game_after.bmp（headless 同款驱动 + 离屏渲染）
cargo run --example game_snapshot --release

# 波次生存竞技场双时刻验收图（左：wave 2 混战 / 右：GAME OVER），输出
# docs/arena_preview.png 拼图源（headless 同款驱动 + 离屏渲染，蒙皮蠕虫
# 敌人两时刻扭动相位不同）
cargo run -p iris-engine --example arena_snapshot --release

# 物理与碰撞验收场景（离屏渲染 + headless 终态断言），输出 iris_physics_midair.bmp /
# iris_physics_rest.bmp：10 颗不同弹性的刚体球落到静态台面与地面——t=0.3s 半空 /
# t=3s 静止（球心 y>0、两两不穿透、总速度 < 2 m/s 三重断言）
cargo run -p iris-engine --example physics_snapshot --release

# 视锥剔除量化基准：5000 立方体双朝向各 300 帧，输出 FrameStats 全字段 +
# draw() 平均/最坏 CPU 耗时（面向全场 ~4730 可见 vs 背对望空 5000 全剔除）
cargo run -p iris-engine --example culling_bench --release

# 内存占用与 LOD 量化基准：4 × 2048² 程序化纹理 + 高/低模网格对，
# 输出 CPU 源字节 / GPU RGBA8 与 BC1 上传估算 / 压缩比（8.00x）/
# 近远两帧 triangles_drawn（49152 → 768，-98.4%）
cargo run -p iris-engine --example memory_bench --release

# 场景序列化验收（内容工作流示范）：load_scene 加载手写的 assets/arena.scene.json
# （地面 + 围墙竞技场 + 台面/圆柱 + 金属/漫反射静态球 + 悬空动态球 + 点光），
# 装 PhysicsPlugin 模拟 1.5s（动态球落到台面静置）后离屏渲染，输出 scene_snapshot.bmp
cargo run -p iris-engine --example scene_snapshot --release

# 外部 HDR 环境贴图验收场景（先 gen_asset 生成日落全景），输出 iris_hdr.bmp：
# Radiance .hdr equirectangular → IBL + 天空盒，铬球反射暖色天空与太阳
cargo run --example gen_asset --release
cargo run --example hdr_snapshot --release

# CPU 热路径 criterion 基准（ECS / 调度 / 物理 / 角色控制器 / broadphase /
# 确定性模拟 + 渲染 CPU 收集 / BC1 / 空间音频），全量运行：
cargo bench --workspace
```

### 添加为依赖

```sh
cargo add iris-engine
```

## 架构总览

```mermaid
flowchart TB
    subgraph 用户代码
        S[系统 fn(&mut World)]
        P[插件 Plugin]
    end
    subgraph iris-engine 门面
        APP[App / 主循环 / 插件系统]
    end
    subgraph iris-render 渲染层
        R[Renderer 批处理] --- SK[SurfaceKit 窗口呈现]
        R --- OFF[OffscreenRenderer 离屏]
        R --- SH[standard.wgsl]
    end
    subgraph iris-platform 平台层
        W[窗口配置] --- IN[Input 键鼠状态]
    end
    subgraph iris-core 核心
        ECS[World / 稀疏集 ECS] --- SCH[Schedule 阶段调度]
        ECS --- RES[资源 / 事件] --- TIME[Time 固定步长]
    end
    APP --> S & P
    APP --> R
    APP --> W & IN
    APP --> ECS
```

详细设计见 [ARCHITECTURE.md](ARCHITECTURE.md)。

## 项目结构

```
iris-engine/
├── assets/              # 示例资产（cube.gltf + cube0.bin + sunset.hdr + arena.scene.json，gen_asset 生成；collect_arena.scene.json / arena_battle.scene.json 为手写关卡，enemy_worm.glb 由 gen_asset 生成）
├── crates/
│   ├── iris-macros/     # 过程宏：#[derive(Component / Resource / Event)]
│   ├── iris-core/       # ECS、调度、变换层级、时间、事件、物理与碰撞（benches/ criterion 基准 + tests/budgets.rs release 预算门禁）
│   ├── iris-render/     # wgpu 渲染器、glTF 管线、离屏渲染
│   ├── iris-platform/   # 窗口配置、键鼠输入
│   ├── iris-audio/      # 软件混音器、WAV / OGG 加载、流式音乐总线、混响、程序化音效、cpal 设备层
│   ├── iris-engine/     # App 门面、插件系统、主循环、场景序列化 + 示例
│   │   └── examples/    # cube / orbit / model / instanced / offscreen / gen_asset / gltf_snapshot / shadow_snapshot / csm_snapshot / point_shadow_snapshot / bloom_snapshot / hdr_snapshot / ui_snapshot / audio_snapshot / skin_snapshot / game_snapshot / arena_snapshot / physics_snapshot / culling_bench / memory_bench / scene_snapshot / particle_snapshot / web（wasm 专用入口，见「Web 构建」）
│   ├── iris-game/       # 引擎下游示范游戏 I：第三人称限时收集（见「用 Iris 做的游戏」）
│   ├── iris-arena/      # 引擎下游示范游戏 II：波次生存竞技场（同上）
│   ├── iris-bastion/    # 引擎下游示范游戏 III：塔防 Iris Bastion（同上）
│   ├── iris-ascent/     # 引擎下游示范游戏 IV：3D 平台跳跃 Iris Ascent（同上）
│   ├── iris-infiltration/ # 引擎下游示范游戏 V：第三人称潜入 Iris Infiltration（同上）
│   └── iris-assault/    # 引擎下游示范游戏 VI：写实战斗庭院 Iris Assault（同上）
└── ARCHITECTURE.md      # 架构设计文档
```

## 实测数据

本机实测（Windows 11 · x86_64 · DirectX 12 · release 构建），复现命令见下。

| 指标 | 数值 |
|------|------|
| ECS：10 万实体生成（2 组件/实体） | **6.0 ms** |
| ECS：双组件遍历（10 万实体/帧，含可变写回） | **251.6 µs/帧** |
| ECS 吞吐 | **≈ 4.0 亿实体组件更新/秒** |
| GPU 合批：25 实例 + 地面（全部在视锥内） | **2 个 draw call**（同网格同材质自动合并） |
| 视锥剔除：相机背后 27 实体 | **0 次提交**（包围球 × 6 平面，测试断言） |
| 示例二进制（含 wgpu 全套渲染栈） | cube.exe **7.8 MB** / offscreen.exe 7.4 MB |
| 离屏渲染一帧 1280×720（含 GPU 初始化 + BMP 落盘） | < 1 s |

```sh
# 复现
cargo bench -p iris-core            # ECS 生成 / 遍历基准（criterion 正式版，
                                    # 完整基线表见 ARCHITECTURE §7.1）
cargo run --example offscreen --release   # 产出 iris_offscreen.bmp
ls -l target/release/examples/*.exe
```

离屏渲染实测输出（`docs/offscreen_preview.png`，缩放预览）：方向光下的旋转立方体，
左右两盏彩色点光在地面形成蓝 / 红光晕并平滑衰减——多光源、深度、MSAA 均工作。

![离屏渲染输出](docs/offscreen_preview.png)

glTF 资产管线实测（`docs/gltf_preview.png`）：从 `assets/cube.gltf`（外部 .bin
buffer + POSITION/NORMAL/TEXCOORD + 45° 旋转节点）加载渲染，材质因子与点光正确。

![glTF 资产渲染](docs/gltf_preview.png)

方向光级联阴影 CSM v2 实测（`docs/csm.png`）：2048² × 3 层 2D 数组深度图，
级联沿相机视锥深度按 PFS（λ=0.6 对数/线性混合）分割、逐级联拟合光空间
正交矩阵（XY 纹素对齐消除相机平移闪烁）——近 / 中 / 远三距物体排开俯斜视：
近处影子锐利（首级联 ~100 texel/m），~104 m 处影子仍在，136 m 处（超出
120 m 覆盖距离）正确无影，级联过渡带无可见跳变；片元按 view 深度选级联后
3×3 PCF 软阴影，`DirectionalLight.shadows = false` 可整帧关闭。

![方向光级联阴影](docs/csm.png)

小场景单张对照（`docs/shadow_preview.png`）：立方体与球体向地面投射软阴影，
PCF 边缘自然、无 acne 条纹（v1 单张路径的验收图，CSM 下同场景近景更清晰）。

![方向光阴影](docs/shadow_preview.png)

点光 cubemap 阴影实测（`docs/point_shadow_preview.png`）：暗色房间中央点光
（512² × 6 面 R32Float，存归一化线性距离），环灯方块/球的阴影从根部
**背离点光方向径向拉伸**——放射状阴影是点光区别于方向光平行阴影的视觉特征；
`PointLight.shadows = true` 时查询顺序中的第一盏点光投影（v1 限定）。

![点光 cubemap 阴影](docs/point_shadow_preview.png)

PBR + HDR + IBL 实测（`docs/pbr_preview.png`）：Cook-Torrance 金属度/粗糙度工作流——
金 / 铜 / 银 / 钢四种金属球（粗糙度 0.05 → 0.55 递增）+ 绝缘体红球，方向光软阴影，
程序化天空背景（inv_view_proj 射线采样），IBL 环境光照（天空立方体贴图：
漫反射辐照度卷积 + GGX 预滤波 + BRDF LUT 分割求和），金属球上半球反射蓝天、
下半球反射地面；线性 Rgba16Float HDR 渲染经 ACES 色调映射输出 sRGB。

![PBR + HDR 渲染](docs/pbr_preview.png)

法线 / ORM 材质通道实测（`docs/normal.png`，R42）：左右两堵同色砖墙对比——左墙
纯标量材质（中性纹理路径），右墙挂程序化砖纹法线 + ORM 贴图（砖缝高度场中心差分
生成法线；R=AO 缝暗 / G=粗糙度缝大磨面小 / B=金属度的打包图）。侧光下右墙呈现
砖块凹凸与缝内阴影，右侧金属圆柱的 ORM 划痕条纹逐像素调制粗糙度高光。

![法线 / ORM 材质通道](docs/normal.png)

泛光实测（`docs/bloom_preview.png`）：HDR 亮度提取（阈值 1.0，软膝）→ 半分辨率
高斯模糊（三轮分离迭代）→ ACES 前按强度叠加。发光体溢出柔和彩色光晕，
暗部与地面保持干净；`BloomSettings` 资源可调强度 / 阈值或整帧关闭。

![泛光后处理](docs/bloom_preview.png)

后处理链实测（`docs/post.png`，左→右：欠曝 0.4 / 标准 1.0 / 过曝 2.5 +
暗角 0.9 + 强泛光）：曝光在 ACES 之前线性缩放画面（欠曝压暗但 HDR 亮源
仍保细节，过曝提亮且亮源泛光扩散）；暗角压暗四角而中心无损（线性 HDR
域、曝光之前作用）。`ExposureSettings` / `VignetteSettings` 资源缺席 =
中性 / 关闭，默认画面与无曝光控制逐位一致。

![后处理链](docs/post.png)

景深与运动模糊（R43）：相机级后处理两 pass，位于泛光之前（HDR 线性域）。
**景深**（`DepthOfFieldSettings`）按逐像素线性视距算混淆圈，12-tap 泊松盘
HDR 平均，清晰带内原色透传；**运动模糊**（`MotionBlurSettings`）用上一帧
相机矩阵重投影得屏幕速度，沿速度方向 N-tap 采样（v1 只覆盖相机运动）。
两资源缺席 / 关闭时整帧与基线逐位一致（FNV 哈希锁定），静止相机下即使
启用也逐位透传。

glTF 动画实测（`docs/anim_preview.png`，左 t=0 / 右 t=1）：`anim_cube.glb`
（GLB 容器）中的蓝色立方体按嵌入的自转 + 浮动剪辑运动——
`AnimationPlayer` 组件驱动节点 `Transform`，层级传播当帧更新世界变换。

![glTF 动画](docs/anim_preview.png)

骨骼蒙皮实测（`docs/skin_preview.png`，左 t=0 / 右 t=1.5）：两段臂分别绑定
髋 / 肘关节，肘关节旋转 90° 时上段**绕肘点**扫成水平——关节矩阵
`J = inv(蒙皮全局) × global(joint) × ibm` 每帧算出后经 storage buffer
进顶点着色器做 4 关节线性混合（LBS）；蒙皮阴影管线复用同一份矩阵，
阴影随骨骼同步变形。

![骨骼蒙皮](docs/skin_preview.png)

外部 HDR 环境贴图实测（`docs/hdr_preview.png`）：程序化生成的日落全景
`assets/sunset.hdr`（1024×512 equirectangular Radiance 文件，手写 RGBE 解析器
加载）经 `SkySource` 资源挂入世界，渲染器按 generation 失效自动重烘 IBL——
暖色天空与亮度 40 的太阳盘成为天空盒背景与泛光源，金属铬球反射出天顶蓝、
暖色地平线与太阳高光，地面承接 HDR 环境的漫反射辐射。

![外部 HDR 环境贴图](docs/hdr_preview.png)

2D UI 叠加层实测（`docs/ui_preview.png`）：点光阴影 3D 场景之上叠加 HUD——
顶部 `IRIS ENGINE` 位图标题、左上血条（深底 + 红色前景两个矩形实体）、
右下青色占位文本。UI pass 位于色调映射 blit 之后：矩形与文本合批为
**一个 draw call**、直接写 sRGB 目标——文字与色块干净锐利，不受泛光 /
ACES 影响；屏幕像素坐标系（原点左上、y 向下），字体为内嵌公有领域
font8x8（ASCII 96 字形，等宽 8×scale 步进，逐亮位展开、形状由单测守护）。

![2D UI 叠加层](docs/ui_preview.png)

UI 与菜单框架实测（R41，`docs/ui_menu.png`）：3D 背景之上叠加**居中暂停菜单**
（半透明底板 + RESUME / SETTINGS / QUIT 三按钮）+ 右下角小地图（复用 R40
`RendersTo` 相机 + `UiImageRtt` 采样）。菜单系统经 `update_ui_layout` 把
`UiAnchor::Center` 锚点解析为绝对像素（纯函数 `anchor_to_position` 保证**改
视口不漂移**），`update_ui_focus` 把焦点落到 SETTINGS 并用高亮色改写其
`UiColor`（原色保存恢复，无样式表）；按钮为「底板 + 标签」两实体（v1
单实体因挂 `UiText` 只作定位载体、不绘制矩形）。设置持久化经
`Settings::load` / `save` 落盘 JSON（字段缺失向后兼容）。

![UI 与菜单框架](docs/ui_menu.png)

音频子系统实测（`docs/audio_preview.png`）：`AudioMixer` 离线编排三段程序化
音效——55Hz 低频 boom（t=0，起始强峰）、880Hz 短 blip（t=0.4，与 boom 尾部
线性叠加出更高峰）、噪声 burst（t=0.7，后段尖峰）——混音到 2 秒 48kHz 立体声
缓冲，逐 400 帧取峰值归一化，映射为 240 根青→橙渐变垂直条（暗色双层底条 +
波形条，复用 UI 矩形系统：481 个 UiRect 实体合批为**一个 draw call**）。
音量编排使全程峰值 0.95 不触软限幅——包络三段清晰、无爆音削顶的平顶；
混音数学（音量 / 叠加 / 循环 / 重采样 / 限幅）全部为确定性纯函数单测覆盖，
有真实音频设备时示例同步真声播放同一编排（无设备自动降级为纯混音器）。

![音频子系统](docs/audio_preview.png)

物理与碰撞实测（`docs/physics_preview.png`，左 t=0.3s / 右 t=3s，同一世界 headless
驱动到两个时刻各自离屏渲染）：10 颗彩色刚体球（restitution 0.2..0.9、半径
0.35..0.55）从 3.5~6.5 m 落向静态高台与地面——左图球群半空中（点光软阴影随球
移动），右图球群落定在台面顶与地面上、无穿插；同一运行内嵌 headless 终态断言
（全部球心 y > 0、球-球与球-静态体残余穿透 ≤ 2e-3、总速度 0.998 < 2.0 m/s 近似
静止）。求解器核心为纯函数（检测 = 穿透/法线、求解 = 分离/反弹/摩擦），17 项
单测覆盖球-球 / 球-AABB / AABB-AABB 几何、自由落体解析解、e² 回弹高度、静止
不抖、摩擦与阻尼衰减、静态体不动、逐采样确定性等。

![物理与碰撞](docs/physics_preview.png)

场景序列化实测（`docs/scene_preview.png`）：手写 JSON 关卡文件
`assets/arena.scene.json`（环境段方向光 + 12 个实体：地面、四围墙、中央台面、
台上圆柱、金属 / 漫反射两颗静态球、悬空动态球、点光、隐藏的逻辑标记实体）经
`load_scene` 整体载入世界——示例代码零场景搭建；装上 `PhysicsPlugin` 确定性
模拟 1.5 s 后离屏渲染：所有物体在位、材质区分清晰（金属球带高光反射、漫反射
球哑光）、方向光 + 点光阴影正常；动态球精确落到台面静息高度（球心 y = 1.500、
速度 0.033 m/s，headless 断言）。mesh 来源（图元参数）由 `SceneAssetRegistry`
登记表在 load 时登记、save 时还原，`load(save(load(f)))` 与 `load(f)` 实体
组件语义等价且二次保存逐字节一致（往返不变量单测守护）。

![场景序列化](docs/scene_preview.png)

## 用 Iris 做的游戏

引擎的「真实用户」验证——只用公开 API（`iris-engine` 门面 + glam）拼完整游戏，
**引擎侧零改动**。三作递进：Iris Collect 验证「能不能拼出来」，Iris Arena
压榨「蒙皮动画 × 对象池 × 弹道 × 多游戏状态」的高强度真实用法，Iris Bastion
把 R21–R25 的五个新能力（射线拾取 / 蒙皮剔除统计 / 透明材质 / 粒子 / 空间
音频）当载荷整体压榨一轮。

### I. Iris Collect（`crates/iris-game`）——第三人称限时收集

**Dogfooding I**——10 枚金币、60 秒倒计时、围墙竞技场、跳跃拾取、HUD、
程序化音效。
纯逻辑状态机（`logic.rs`，只依赖 glam、零引擎依赖）与插件接线（`GamePlugin`）
分离：**关卡来自 JSON 场景文件**——Startup 经 `load_scene` 载入
`assets/collect_arena.scene.json`（微弱日光环境段 + 地面、四面围墙（静态
Collider，内缘 ±17.5）、三根障碍柱（压在金币环路线上制造绕行）、中央点光），
玩家 / 金币 / HUD 为代码生成，相机作为玩家子实体（层级跟随验证
`ActiveCamera`）；Update 把 `Input` + `Time` 翻译进逻辑（移动积分 / 拾取 /
胜负全部可单测的纯函数）后，用物理库的**静态推离查询**实现运动学角色控制
器——玩家球体对场景静态体求推离、只回馈水平分量：墙体把玩家挡在 17.05
（= 内缘 17.5 − 半径 0.45，远早于 ±18 的逻辑钳制安全网），推离只沿法线 →
切向速度天然保留 = **贴墙滑行**；随后把结果写回实体 / HUD / `Audio`——
金币自发光供泛光、点光 cubemap 阴影、位图字体 HUD 全部来自引擎既有能力。
同一套插件系统链经 `GamePlugin::build_schedule` 在无窗口无 GPU 的 headless
测试与离屏快照中复用（测试经 `GamePlugin::new` 注入临时目录的 fixture 关卡，
全部测试跑在真实场景加载路径上）。这次实践撞到的 7 条 API 缺口（可见性开关 /
阶段排序 / 并行发事件 / 按名查实体 / Input 注入 / Audio headless /
Time·UI·图元小项）已在后续「引擎 API 打磨」轮全部修复，游戏内的绕行代码
同步替换为新 API；角色控制器也已由物理查询实现（清单见 ARCHITECTURE.md §8.12）。

![收集小游戏双时刻验收](docs/game_preview.png)

实测（`docs/game_preview.png`，左：开局 / 右：拾取 3 枚后，同一世界用 headless
方式驱动到两个时刻各自离屏渲染）：左图玩家居中、围墙与障碍柱在位、金币环
带泛光、SCORE 0/10；右图 SCORE 3/10、TIME 开始倒数、金币减少、玩家与子实体
相机已随输入移动——场景文件关卡、层级跟随、点光阴影、泛光、HUD 在真实游戏
负载下完整工作，玩家与墙体 / 障碍柱无穿插。

```sh
cargo run -p iris-game --release   # 玩游戏（WASD 移动 / Space 跳跃）
cargo run --example game_snapshot --release   # 双时刻离屏验收图
```

### II. Iris Arena（`crates/iris-arena`）——波次生存竞技场

**Dogfooding II**——俯视 3/4 视角竞技场：WASD 移动（屏幕相对 + 贴墙滑行）、
**Shift 冲刺**（0.15 s 高速 + 0.25 s 无敌帧）、**自动开火**（每 0.25 s 朝
14 m 内最近敌人发射弹道，无敌人不开火）。蒙皮蠕虫敌人（`enemy_worm.glb`，
三段箱体关节链扭动循环）从场边刷新成波次（第 n 波 3 + 2n 只、追击速度逐波
+0.15）；接触掉 1 血 + 击退 + 连击清零；弹道命中 -1 HP + 小击退。清空一波
→ 3 s 间歇 → 下一波；玩家 5 HP 归零 GAME OVER，R 直接重开。连击：击杀 +1、
受伤或 3 s 无击杀清零，得分 = 25 × 连击数。

相比 Iris Collect 高强度压榨的引擎面：**蒙皮动画 × 对象池**——启动时一次
spawn 15 个蒙皮蠕虫实例（`spawn_gltf_instance` + `AnimationPlayer` 自动
循环扭动）与 40 个自发光弹道小球，全部 `Visibility::HIDDEN` 待命，波次间
按逻辑侧稳定 id 取用 / 归还槽位，**零运行期 spawn / despawn**（池不超发由
单测守护）；**根实体移动 × 蒙皮子树**——插件只写 glTF 根的
Transform，关节子树随层级传播跟随、动画逐帧覆写关节旋转，两者在同一实体
树上叠加；纯逻辑核心（`logic.rs`，只 glam、零引擎依赖、18 项单测含逐采样
确定性）内置本地 XZ 静态推离（与引擎 `resolve_static_push` 水平投影语义
近似、刻意解耦）与弹道扫掠命中判定（本帧 `prev → pos` 线段对敌人 / 静态体
测试，与引擎 `segment_hits_shape` 同款防穿模语义——弹速 × dt 步长可大于
敌人直径，当帧点判定会漏检；含「每帧位移 > 敌人直径 2 倍仍 1 s 内命中」的
穿模回归测试），插件层在 Startup 把场景 `Collider` 一次性转换为逻辑形状
（Update 每帧零查询）；多游戏状态 Title / Playing / GameOver + 波次间歇；
音效由逻辑侧单调事件计数器的前后差分触发（射击 / 命中 / 受伤 / 开波双音，
全部程序化生成）。headless 集成测试（9 项）复用与窗口程序同一编排的插件
系统链，覆盖池取用 / 归还 / 跨波复用、动画推进、HUD 刷新、弹道池封顶、
终局重开闭环、蒙皮实例根位移带动关节子树与混战 draw calls 记录性断言
（13 敌 + 20 弹 + 场景 + HUD 一帧 **25 次 draw call**）；本次实践撞到的
API 摩擦（AnimationPlugin 未入 prelude / 资产路径工作目录相对 /
蒙皮实例关节树不随根移动等）已在「引擎 API 打磨 II」轮全部修复，
游戏内绕行代码同步删除改用新 API（清单见 ARCHITECTURE.md §8.13）。

![波次生存竞技场双时刻验收](docs/arena_preview.png)

实测（`docs/arena_preview.png`，左：wave 2 混战 / 右：GAME OVER，同一世界
用 headless 方式驱动到两个时刻各自离屏渲染）：左图 5 只蒙皮蠕虫从场边
围拢玩家、自动开火弹道在飞、HUD（WAVE 2 / SCORE / HP 条）完整；右图蠕虫
围拢倒地的玩家、HP 条清空、GAME OVER 横幅——两时刻相隔真实推进的帧，
蒙皮扭动相位肉眼可见地不同（动画在跑）。玩家与障碍柱 / 墙体无穿插。

```sh
cargo run -p iris-arena --release   # 玩游戏（WASD 移动 / Shift 冲刺 / 自动开火）
cargo run -p iris-engine --example arena_snapshot --release   # 双时刻离屏验收图
```

### III. Iris Bastion（`crates/iris-bastion`）——俯视塔防

**Dogfooding III**——俯视固定视角塔防：敌人沿 7 段 6 弯的航点折线进攻路径
末端的核心，玩家用击杀所得积分在 20×14 的 1×1 m 网格上放塔——**哨兵**
（速射单体，快速弹扫掠命中）与**臼炮**（慢速弹抛射、落点 AOE 半径 1.4 m）
两种塔、疾行虫 / 重甲兽两种敌人、5 波递增，全清获胜、核心 10 HP 归零告负。
放塔校验（界内 / 空格 / 积分够）是单独导出的纯函数，幽灵预览据此实时
红蓝着色。

本作把 R21–R25 的五个新能力当载荷压榨：**鼠标拾取（R21）**——`cursor_
position()` 像素 → NDC → 引擎 `screen_ray_from_global` 世界射线 →
`raycast` 打地面静态 Collider → 网格（刻意走引擎查询而非解析平面）；
**透明材质（R23）**——幽灵塔身 `AlphaMode::Blend` 可放蓝 / 不可放红 +
半透明射程指示盘；**粒子（R24）**——枪口闪光 / 命中火花 / 死亡爆炸 /
漏怪告警四类 burst 挂 FX 池槽位，参数覆写 + `reset()` 就地复用触发；
**空间音频（R25）**——开火 / 命中 / 爆炸 / 死亡 / 漏怪五种程序化音效
`play_spatial` 定位到事件世界坐标，听者每帧跟随俯视相机。逻辑侧事件自带
世界坐标（`BastionEvent`），表现层零反查。塔 / 敌 / 双弹 / FX 四池启动
预热，运行期零 spawn / despawn（池守恒由测试守护；三个同构池各自
distinct newtype——资源按 TypeId 寻址，type alias 同型会互相覆盖）。
纯逻辑核心（`logic.rs`，只 glam、零引擎依赖）含扫掠弹道命中、路径进度
索敌（射程内**打最靠前**）、航点匀速推进、波次间歇与逐位确定性；
headless 集成测试里的**脚本化自动玩家**经鼠标点击全链路放塔，真实模拟
跑完 5 波**获胜且核心无损**（经济 / 索敌闭环可玩的量化证据）。本次实践
撞到的 5 条引擎摩擦（屏幕射线辅助 + 视口资源 / 粒子重置 API / 粒子
Visibility 过滤 / 可见性层级级联 / 同构池 type alias 模式警示）已在
「引擎 API 打磨 III」轮全部闭合：游戏内的 `BastionViewport` 资源、手工
vp 逆与「换挂整只新发射器组件」绕行删除改用新 API，塔 / 敌池还原
「根 → 形态 rig → 网格」三层结构由级联真实生效（清单见
ARCHITECTURE.md §8.15）。

![塔防中期战局验收](docs/bastion.png)

实测（`docs/bastion.png`，640×360 离屏单帧）：四塔就位（哨兵压西段 / 臼炮
守弯口）、第 1 波敌群在途（橙色疾行虫沿航点推进）、击杀爆炸粒子在扩散、
幽灵预览（臼炮形态 + 射程盘）悬停待放格、HUD（CREDITS / WAVE 1/5 /
CORE HP / 建造提示）完整；该帧 FrameStats：`draw_calls = 17` /
`instances = 366` / `culled_instances = 8`（界外岩柱全部被视锥剔除——
剔除在真实场景生效的量化证据）/ `particles_drawn = 338`。对比打磨 III
前的拍平池结构（`draw_calls = 18` / `instances = 367`）：旧逐实体可见性
语义下击杀归还的池槽位只隐藏根实体、网格子实体自带 `VISIBLE` 不级联——
被击杀敌人的机体**滞留在死亡点入画**（恰藏于 HUD 文字后，新旧帧逐像素
差分定位）；层级级联闭合后该伪影消失，还原「根 → 形态 rig → 网格」三层
池结构 draw_calls 18 → 17（与「旧结构 + 手工逐子隐藏」的帧逐字节一致）。

```sh
cargo run -p iris-bastion --release                  # 玩游戏（鼠标选格 / 左键放塔 / 1·2·右键换塔种）
cargo run -p iris-bastion --example bastion_snapshot --release   # 中期战局离屏验收帧
```

### IV. Iris Ascent（`crates/iris-ascent`）——3D 平台跳跃

**Dogfooding IV**——第三人称线性上升关卡：角色沿悬浮台地（0.28 m 台阶）
→ 大球坡面（R = 14，走落 + 球顶定点跳）→ **四段跳板弹射**（竖直冲量
18 m/s，弹高 ~16.5 m，承担主要爬升）一路向上攀登 61 m，沿途收集 14 颗
宝石、激活 3 个检查点，抵达山顶信标获胜；坠落到坠落线（y = −12）回最近
检查点复活。全部机制集中在纯逻辑核心 `logic.rs`。

本作把 R28–R31 的四个新能力当载荷压榨：**动画状态机 / crossfade /
事件轨道（R28）**——双剪辑角色 GLB（`assets/ascent_bot.glb` 由
`gen_asset` 示例程序化生成：`idle` 呼吸压缩 2 s / `walk` 前倾摇摆
0.6 s），运行时 `request_state` 双向 0.15 s crossfade；walk 剪辑注入
「footstep」事件轨道（GLB 解析不产事件、API 侧构造），`take_events`
驱动脚步声；**CSM 级联阴影（R29）**——方向光斜照 ~130 m 场景（无需新
代码，快照目检 + 阴影开 / 关逐像素差分作证据）；**距离 LOD（R30）**——
22 个远景装饰（山景球 / 巨柱 / 浮岛剪影）挂 `LodMesh`（高 / 低模 +
60 m 切换），相机拉远 160 m 后 `triangles_drawn` 9 440 → 7 324 的量化
下降；**运动学角色控制器（R31）**——`move_and_slide_statics` 就是本作
的物理核心：纯逻辑核心只依赖 glam + 引擎纯函数（刻意 dogfooding），
headless 与窗口走同一模拟，台阶迈上 / 贴墙滑行 / 陡坡下滑 / 贴地吸附
全部由控制器五步管线承担。跳板不是物理——AABB 判定 + 一次竖直冲量
（`vel.y ≤ 0` 门禁保证每段接触恰好触发一次）。土狼时间 0.1 s、落地
冲量阈值 3 m/s（防小台阶误触发尘土）等手感参数集中在 `logic.rs` 顶部。

**脚本化自动玩家**（headless 与纯核心单测共用同一 `AutoPilot` 脚本）：
关卡被刻意设计成「前进 + 两个定点跳」（出生台地一跳 + 球顶一跳，其余
全部由控制器自动迈上 / 走落衔接、跳板自动弹射）——headless 集成测试经
**真实键盘注入**（Input → 相机相对翻译 → 逻辑 → 表现）全关通关：
**用时 19.5 s、宝石 14/14、检查点 3/3、坠落 0**；纯核心单测另有逐位
确定性（两次模拟 to_bits 逐帧比对）与弹射顶点解析对照。

![平台跳跃中段验收](docs/ascent.png)

实测（`docs/ascent.png`，640×360 离屏单帧）：自动玩家真实打通第一段
登顶后定格在第一块弹射落点台 P6——角色（teal 机器人，落地尘土粒子
扩散中）、检查点 1 旗已点亮（激活火花在飘）、前方 P7 跳板辉光盘、
HUD（GEMS 7/14 / CHECKPOINT 1/3 / FALLS 0）完整、两侧远景 LOD 装饰
（山景球 / 巨柱）；该帧 FrameStats：`draw_calls = 13` / `instances = 116`
/ `culled_instances = 41`（远景装饰视锥剔除）/ `particles_drawn = 94`
（落地尘土 + 检查点火花）。CSM 证据：方向光 `shadows = false` 与 `true`
两帧逐像素差分 4 622 px（远景山体的受光 / 阴影面 + 角色 / 旗杆接触
阴影——级联 0 近景锐利、级联 2 覆盖远景山体）。

本轮 R32 的**摩擦清单**（现象 → 绕行 → 建议修法，全文见
ARCHITECTURE.md §8.18；**打磨 IV 轮已全部闭合**——①②③④ 引擎侧
修复 + 本作绕行删除，⑤⑥ 文档闭合）：

1. **crossfade 只混「双方都有」的通道，缺通道保持源姿势且混完定格**
   → 角色两剪辑都被迫写满同路径通道（idle/walk 都带 scale + rotation），
   否则呼吸缩放会冻结在过渡时刻的值 → **已修复**：
   `AnimationPlayer::capture_rest` 基姿势快照——混合期「仅源有」通道
   平滑淡回基姿势，过渡完成恰停在 rest（完成瞬间无跳变）；本作两剪辑
   已删除恒值对齐通道（idle 只写 scale、walk 只写 rotation）；
2. **GLB 生成器是纯手写 JSON 字符串**（bufferView 偏移 / accessor 下标
   全手工记账，错一位就加载失败或花屏）→ 绕行：以既有 gen_asset 函数
   为模板复制粘贴改数 → **已修复**：引擎提供 `GltfBuilder` 编程式
   组装 API（iris-engine `gltf_builder` 模块），gen_asset 全部资产生成
   已迁移、重新生成资产与旧文件加载语义等价；
3. **动画状态机无边时 `request_state` 硬切换**——调试时临时清空 edges
   会让所有过渡变成跳变，画面「闪一下」无任何告警 → **已修复**：
   无边硬切换打 `log::debug!`（含 from / to 状态与边表长度）；
4. **第三人称「子实体相机 + 平滑追随」没有现成模式**：相机挂根实体后
   平滑必须写在**局部**位置（局部 = 平滑世界位 − 根位置），且
   GlobalTransform 滞后一帧、复活瞬移需手动吸附 → 本作把 rig 世界位放
   资源里自行积分（约 30 行）→ **已修复**：引擎提供 `SmoothFollow`
   组件 + `update_smooth_follow` 系统（iris-engine `camera_rig` 模块，
   帧率无关指数趋近 + `snap()` 吸附）；本作 `CameraRig` 绕行已删除，
   相机改挂组件；
5. **`Input::press` 电平与 `key_pressed` 边沿的双轨**：脚本玩家「按住
   跳跃键」要在每帧 release+press 注入边沿，电平语义对跳跃这类
   「落地就跳」的玩法反而更自然 → 本作逻辑层直接采用电平语义
   （`input.jump` 按住即跳），绕开了边沿翻译 → **已闭合（文档）**：
   input 模块文档新增「输入语义选择指南」（判定法则 + 门控范式）；
6. **`move_and_slide` 参数整定无指导**：`step_height` 0.3 / `snap_
   distance` 0.2 的组合对「0.28 m 台阶 + 0.5 m 桥接缝」的走落衔接恰好
   工作，但缝隙 > 2×snap_distance、落差 > step_height 的衔接会卡墙
   （「贴边落在台缘角点」的失败模式）→ 本作以关卡不变量单测固化
   「衔接高差 ≤ 0.3、缝宽 ≤ 0.5」→ **已闭合（文档）**：character
   模块文档新增「平台跳跃衔接参数表」（可靠域 / 失败模式症状与诊断）。

```sh
cargo run -p iris-ascent --release                  # 玩游戏（WASD 移动 / 空格跳 / R 回检查点）
cargo run -p iris-ascent --example ascent_snapshot --release   # 中段战局离屏验收帧
```

### V. Iris Infiltration（`crates/iris-infiltration`）——第三人称潜入

**Dogfooding V**——俯视偏斜 45° 第三人称的 24×18 m 单层设施潜入：玩家从
走廊西端潜入，绕过两名巡逻守卫取 3 个数据核心，抵达出口获胜；被发现 3 次
告负。Shift 潜行（守卫视锥发现半径减半）、E 从背后 2.2 m 内击晕守卫、
Esc 暂停菜单。全部机制集中在纯逻辑核心 `logic.rs`。

本作把 R34–R41 的新能力当载荷压榨：**NavGrid 寻路（R35）**——守卫巡逻 /
追逐全部走 `NavGrid::find_path` 航点（硬编码的只有「路线环目标点」，逐段
走位交给导航图），压力门被踩开后按引擎文档推荐的「便宜重建」姿势整图重建；
**动画 v3（R38）**——玩家与守卫共用双剪辑**混合空间**（`BlendSpace` 参数 =
移动速度 0..1，idle ↔ walk 连续混合，潜行半速时参数停在 ~0.48 的中间
姿势）+ **两骨足部 IK**（每角色双腿链 hip→knee→foot，脚踝贴地 ±2 cm，
headless 断言量化）；**ragdoll（R37）**——背后击晕 → `spawn_ragdoll_
kinematic` + `release_ragdoll` 六球人形瘫倒（v1 击晕不醒来，诚实限制）；
**接触事件（R34）**——压力板 sensor + 物理代理胶囊，`ContactStarted/Ended`
begin/end 成对计数（踩上开门为单向闩锁）；**RTT（R40）**——固定 RT 相机
俯瞰核心房，`UiImageRtt` 画中画贴 HUD 右上（R41 `UiAnchor` 锚定，改视口
不漂移）；**UI 菜单栈（R41）**——Esc push/pop `UiMenuStack`、按钮焦点
导航（↓ + Enter）、`UiClickEvent` 消费（RESUME/QUIT）、`Settings` 加载 /
QUIT 落盘；**距离关节（R36）**——NW 门洞的「弹簧门」动态面板被玩家物理
代理真实推开、`DistanceJoint` 拉回关闭位（纯展示语义，不在逻辑静态体）。

**脚本化自动玩家**（恒潜行 + 预设路线点 + 视线避让：被巡逻守卫看见就朝
反方向撤退、守卫背身窗口出现即恢复推进——「潜行跟随」行为涌现）：
headless 集成测试经**真实键盘注入**全关通关——**用时 49.5 s、核心 3/3、
被发现 0（设计余量 ≤ 1）、压力门已开**；纯核心单测另有逐位确定性（两次
模拟 to_bits 逐帧比对）、守卫巡逻绕墙断言（任意帧不进墙 0.3 m 域）、
视锥三要素（距离 / 夹角 / 遮挡）与警觉累积衰减的精确断言。

![潜入中段验收](docs/infiltration.png)

实测（`docs/infiltration.png`，640×360 离屏单帧）：自动玩家真实推进到
「核心 A 已到手、正穿走廊奔压力板」的中段定格——玩家（teal 机器人）
潜行中、守卫在身后 NW 室巡逻、右上角监控屏画中画（RT 相机俯瞰核心房：
金色核心 C 与红色压力门清晰可辨）、HUD（CORES 1/3 / SPOTTED 0/3 /
ALERT 0% / CAM-02 标签）完整；该帧 FrameStats：`draw_calls = 15` /
`instances = 29` / `culled_instances = 14`（外墙背面视锥剔除）/
`triangles = 348`。

本轮 R42（游戏五）的**摩擦清单**（现象 → 绕行 → 建议修法，全文见
ARCHITECTURE.md §8.22；本轮为游戏轮，全部只绕行不改引擎。
**更新：六条已全部闭合于「引擎 API 打磨 V」**——①事件双代轮换生命周期
（`UiClickEvent` 在 Update 直接消费，`MenuCommand` 信箱删除；QUIT 退出
修复）＋ 事件可读窗口跨两帧、消费必须 `drain`；②`IkRig` 组件解耦
FootIk（零剪辑播放器删除）；③`CommandQueue` 帧末命令队列（ragdoll 生成
改并行系统 + `spawn_deferred`）；④IK 链「节点 scale 恒 1」契约 + 骨长
偏差 > 10% 一次性 warn；⑤`set_subtree_hidden` 引擎原语；⑥
`RigidBody::body_type = Kinematic` 运动学碰撞体（物理代理删除，压力板 /
弹簧门由玩家胶囊真实碰撞驱动）。各条绕行实现详见下）：

1. **`UiClickEvent` 在 PostUpdate 产出、帧尾 `clear_all_events` 清空——
   Update 系统永远读不到**（焦点系统发事件在 PostUpdate，主循环事件清空
   在 PostUpdate 之后；连 `AppExit` 也只有 Update 阶段发出的能被帧尾退出
   检查读到）→ 绕行：游戏侧 PostUpdate 尾部独占系统（注册晚于 `UiPlugin`
   焦点系统）把事件转写成 `MenuCommand` 信箱资源，Update 消费 → **建议**：
   焦点系统改发「命令资源 / 回调」或事件清空移到窗口事件之后；
2. **`update_foot_ik` 与 `AnimationPlayer` 强耦合**——纯 IK 装配（无剪辑
   的腿链）也必须挂一个空 `AnimationPlayer` 才能被系统查询到 → 绕行：
   挂零剪辑播放器（语义无害但费解）→ **建议**：FootIk 独立查询或
   `IkRig` 组件；
3. **ragdoll 生成需要 `&mut World`**——实体分配是结构性修改，并行系统
   （`&World`）无法 spawn → 绕行：游戏 Update 里插独占系统做生成（屏障
   语义天然隔离）→ **建议**：命令队列（`World::spawn_batch` 类 API）；
4. **节点 scale 会污染 IK 链长**——用节点 `Transform.scale` 缩放关节网格
   时，scale 被 `chain_world` 的链矩阵继承，骨长随之缩水（腿直接缩短到
   一半），IK 落地查询跟着错 → 绕行：关节节点 scale 恒 1，网格尺寸放在
   独立子网格实体上 → **建议**：foot_ik 模块文档加「链上禁止非均匀
   scale」契约（或文档 + debug_assert 骨长突变告警）；
5. **「隐藏子树」无引擎原语**——击晕后要藏整个角色（模型 + 双腿链 +
   网格），只能游戏侧沿 `Children` 递归 `Visibility::HIDDEN` → 绕行：
   `hide_subtree` 10 行递归 → **建议**：`visibility` 模块提供
   `set_subtree_hidden`；
6. **运动学角色没有「物理碰撞体」概念**——逻辑角色（`move_and_slide`）
   要参与 R34 接触事件 / 推开 R36 动态门，必须额外 spawn 一个动态胶囊
   「物理代理」并逐帧同步位置 / 速度（漂移 ≤ 1 帧即被覆写）→ 绕行：
   代理同步（本作 `PROXY_NAME` 实体）→ **建议**：RigidBody 增加
   kinematic 模式（位置由调用方驱动、参与检测与推挤、不被动力学积分）。

```sh
cargo run -p iris-infiltration --release                  # 玩游戏（WASD 移动 / Shift 潜行 / E 击晕 / Esc 菜单）
cargo run -p iris-infiltration --example infiltration_snapshot --release   # 中段战局离屏验收帧
```

### VI. Iris Assault（`crates/iris-assault`）——写实战斗庭院

**Dogfooding VI（写实路线垂直切片）**——20×14 m 战斗庭院：黄昏斜阳下的
砖围墙、四角石柱与中央喷泉（圆障碍），持刀 + 步枪的玩家与 4 名持刀敌人
死斗。刀剑三连段（每段 = 前摇 / 55° 扇形判定窗 / 后摇，第三段击退 +
更长硬直；横扫 / 后摇期按攻击键预约下一段）、步枪（0.16 s 射速 / 12 发
弹匣 / 1.6 s 换弹 / `ray_vs_shape` 弹道遮挡——被墙挡住的敌人不中弹、
命中墙面贴弹孔）、全歼胜利、HP 归零败北；开场绕庭相机演出与终局推近
结算。与前五作把单轮新能力压榨到极限不同，本作首要目的是把
**R42–R47 六轮新能力当载荷全量实弹**（接线验收 + 摩擦清单）：

- **写实材质（R42）**——地面石板 / 砖墙用引擎 `brick_normal_orm`
  程序化法线 + ORM 对；刀身拉丝划痕（`scratched_metal_normal_orm`）与
  布料褶皱（`cloth_normal_orm`）为本 crate 纯函数生成器（splitmix64
  哈希、确定性单测锁定），经 `GltfBuilder::material_textured` 把法线 /
  ORM 通道 PNG 内嵌进角色 GLB（打磨 VI 摩擦③ 闭合，运行期只剩敌我配色
  覆写）；
- **DOF / MB（R43）**——战斗默认开：`DepthOfFieldSettings::focus_target`
  指向最近活敌的胸骨，引擎每帧解析视距（焦外敌人虚化即距离感，摩擦⑤
  闭合）；`MotionBlur` 覆盖演出相机的 spline 运动；
- **流式音频（R44）**——pack 内 OGG 氛围循环上音乐总线（`play_music`
  后台线程分块解码 + `set_reverb` 庭院混响，v1 只作用于音乐总线）；
  挥砍 / 枪声 / 命中 / 换弹 / 胜负共 8 种程序化音效走 `play_spatial`
  （听者跟随主相机）；
- **pack 资产（R45）**——角色 GLB + 氛围 OGG 打包 `assets/assault.ipak`
  单文件，`AssetMount(Pack)` 挂载；GLB 走 `spawn_gltf_instance_from_bytes`
  内存入口（自包含约束实弹验证）；pack 缺失时诚实降级文件路径；
- **动画 v4（R46）**——上下身**双 `AnimationPlayer`** 组合姿势：下半身
  `BlendSpace`（idle/walk/run，参数 = 速度）+ 上半身动作剪辑
  （guard / sword1..3 / aim / reload）+ `AnimationMask`（节点集 = 上身
  12+2 骨）——两层写入集不相交 ⇒ 跑动中挥砍 / 瞄准不打断腿；受击 =
  `AdditiveLayer` flinch 增量叠加（不破坏步态）；**手 IK**：ADS 时双臂
  `HandIk` 吸附枪身握把 / 护木挂点（`IkTarget` 挂胸节点子树——不在手骨
  链内，无反馈环；headless 断言右手吸附握把 ±12 cm）；
- **演出时间线（R47）**——开场 6.5 s 绕庭 Catmull-Rom 相机 + 两段字幕 +
  2.35:1 黑边，`intro_end` 事件触发 `begin_battle` 交还控制权；胜利 /
  败北推近主角结算（`OutroLatch` 每局一次）。
  老能力照用：ragdoll 死亡（六球人形 + 受击方向轻踢）、贴花战痕
  （血渍 / 弹孔盒投影贴花）、粒子 FX 池（32 槽启动预热零运行期 spawn：
  血雾 / 枪口焰 / 刀光 / 火花四组）、NavGrid 绕喷泉追击（敌人 AI）。

**脚本化自动玩家**（接近最近敌人 → 扇区内三连段 → 中距步枪补射 → 弹尽
换弹 → HP < 30 拉距射击）：headless 经**真实键盘注入**通关——
**用时 6.4 s（382 帧）全歼 4 敌、玩家 HP 100、开枪 16 发**；纯核心
19 项单测覆盖扇形越界不伤、同代次防重复命中、换弹时序、弹道遮挡解析解
（喷泉球体命中点 x ≈ −1.587）、硬直重置语义、击退位移解析（1.2 m）与
逐位确定性。

![战斗中段验收](docs/assault.png)

实测（`docs/assault.png`，640×360 两帧横拼）：左帧刀剑横扫判定窗中
（上身动作剪辑 + DOF 对焦交战距离、远处敌人焦外虚化）；右帧枪口焰当帧
（FX 池 additive 粒子 + 喷泉水花 + ragdoll 尸体可见）。帧 A
FrameStats：`draw_calls = 72` / `instances = 230` /
`culled_instances = 19` / `triangles = 3150`。

本轮 R48（游戏六）的**摩擦清单**（现象 → 绕行 → 建议修法，全文见
ARCHITECTURE.md §8.26；本轮为游戏轮，全部只绕行不改引擎）：

1. **足部 IK 回写四元数不归一化 → 逐帧平方复合爆炸**（本轮最重）——
   `update_foot_ik` 旋转回写（旋转弧复合 + 共轭反推）产出**未归一化**
   四元数；当下帧无剪辑通道覆写该骨（idle 只有骨盆 bob、无腿通道）时
   幅度逐帧平方复合，实测 1.9 s 内 386 → 5.8e7 → NaN（骨长 NaN →
   `solve_two_bone_ik` 断言 panic；infiltration 未炸纯属其 walk 剪辑
   恰好每帧覆写双腿）→ 绕行：idle 剪辑补恒值腿通道（每帧重置骨旋转）
   → **建议**：两处 IK 回写前 `normalize()` + 单测锁定 ‖q‖−1 ≤ ε；
2. **GLB 内存加载入口不在 prelude**——`spawn_gltf_instance_from_bytes`
   （R45 pack 路径的刚需）未进 `iris-render` prelude，门面下游拿不到
   → 绕行：iris-assault 显式直依 iris-render（依赖纪律偏离，Cargo.toml
   注明）→ **建议**：补进 prelude；
3. **GltfBuilder 不支持纹理引用**——R42 法线 / ORM 无法在资产生成端
   表达 → 绕行：加载后克隆材质 + `with_normal/with_orm` 替换 →
   **建议**：builder 增加 `material_textured`（GLB 内嵌图片）；
4. **无 Vorbis 编码器**（R44 只有解码）——氛围 OGG 无法程序化生成
   → 绕行：复用引擎音频测试 fixture（2 s 正弦循环）→ **建议**：
   `procedural` 增加直写 OGG 编码或接受外部资产管线；
5. **DOF 动态对焦无引擎辅助**——对焦距离是交战几何的函数，静态值在
   快速接近时把敌人糊成一团；游戏侧每帧写 `DepthOfFieldSettings` 可用
   但无「对焦目标实体」原语 → 绕行：插件层每帧算相机→最近敌距离写资源
   → **建议**：`FocusTarget(Entity)` 组件 + 渲染器自动对焦；
6. **上下身分离剪辑的骨架契约全手工**——加载器不带节点名，22 节点下标
   契约（skeleton.rs）与资产生成器 / 插件层三方对齐只能靠 headless
   守护测试（第一版把网格做成独立子节点 → 42 节点错位立刻爆出）
   → 绕行：`skeleton` 模块单一事实来源 + 节点数守护断言 → **建议**：
   `GltfInstance` 携带节点名（`node_named` API）；
7. **example 跨 crate 同名冲突**——iris-assault 的 `gen_asset` 与
   iris-engine 的同名 example 输出文件互相覆盖（cargo 警告 future hard
   error）→ 绕行：改名 `gen_assault_assets` → **建议**：无引擎修法，
   记录命名约定（游戏 crate example 加游戏前缀）。

**打磨 VI 已把七条全部闭合**（引擎侧修复 + 本 crate 侧绕行删除，
ARCHITECTURE §8.26 逐条标注）：

1. **①** `update_foot_ik` / `update_hand_ik` 写回前 `normalize()`——
   归一化是 IK 的责任（模块文档有推导）；600 帧回归锁
   `foot_ik::tests::foot_ik_quaternion_norm_stable_without_clip_overwrite`
   与 hand 侧同名测试（逐帧断言 ‖q‖−1 ≤ 1e-3 + 同相位位形一致）；
   idle 的恒值腿通道**删除**（资产重生成）。删改前该场景在
   `intro_cinematic_plays_and_hands_over_control` 里 1.9 s 必炸；
2. **②** `spawn_gltf_instance_from_bytes` 进 `iris-render` prelude ⇒
   iris-assault 的 iris-render 直依删除（依赖纪律回归）；
3. **③** `GltfBuilder::material_textured`：baseColor / 法线 / ORM 三通道
   像素 PNG → base64 data URI 内嵌 GLB（自包含，内存加载路径可用），
   内容相同去重；插件层「加载后克隆材质挂纹理」的绕行删除，运行期只剩
   敌我 `base_color` 配色覆写；
4. **④** 引擎不做编码器（运行时职责，明文接受外部资产管线）——
   WAV → OGG 标准命令进 README「环境要求」与 ARCHITECTURE §4.14；
5. **⑤** `DepthOfFieldSettings::focus_target`（引擎每帧解析目标
   `GlobalTransform` → 视距，缺席 / 隐藏回退标量）——插件层逐帧写
   `focus_distance` 的绕行删除，战斗中对焦最近活敌的胸骨；
6. **⑥** `AnimationPlayer::node_names`（加载器携带 GLB 节点名）+
   `node_entity_by_name`——装配层（IK 链 / 挂点 / 配色）改按名寻骨，
   下标只服务动画通道与遮罩；
7. **⑦** example 命名约定（跨 crate 唯一前缀）记入 ARCHITECTURE §7.2。

```sh
cargo run -p iris-assault --release                  # 玩游戏（WASD 移动 / 空格·左键挥刀 / F 开火 / Q·右键瞄准 / R 换弹）
cargo run -p iris-assault --example assault_snapshot --release   # 战斗中段两帧离屏验收
cargo run -p iris-assault --example gen_assault_assets --release # 重新生成 GLB + pack
```

## 测试

```sh
cargo test --workspace   # 801 个测试：单元 + headless 集成 + GPU 端到端离屏渲染
```

集成测试在真实 GPU 上离屏渲染并校验像素（无 GPU 环境自动跳过），覆盖：
着色器编译、深度测试、MSAA、实例化合批、视锥剔除、多光源照明、
**粒子系统（喷泉可见性 + particles_drawn 与池状态对账 + additive 亮度
显著高于 alpha 混合 + 爆炸 one-shot 全灭不补发 + 模拟逐位确定性）**、
**方向光阴影（开/关整帧差分：成片变暗、无变亮、大部分不变）**、
**点光 cubemap 阴影（面矩阵全方向覆盖 + 偏置遮挡物阴影落在解析预测位置 +
开/关整帧差分 + 空跑不污染）**、
**泛光（开/关整帧差分：光晕变亮、无变暗、远场不变）**、
**IBL（铬球反射蓝天：蓝主导像素对比 + 整帧变亮）**、
**glTF 动画（旋转方块离屏渲染：正对 → 侧对红色像素锐减）**、
**骨骼蒙皮（肘关节动画驱动顶点位移：最左像素左移 ≥30px + 蒙皮阴影成片变暗）**、
**外部 HDR 环境贴图（红/蓝合成全景：画面上下半球主导通道 + 铬球反射上半球）**、
**2D UI 叠加层（矩形像素精确落地无溢出 + 文本亮像素命中逐位解析预测 ±25% +
HUD 区域不透 3D 且 HUD 外区域与无 UI 渲染逐像素一致）**、
**并行调度（原子计数观测真并发 / 读写冲突分批保序 / 独占系统屏障）**、
**实例缓冲增量上传（静态场景连续帧零上传且像素一致 + 单实体移动仅传变化段）**、
glTF 资产管线（内嵌 data-URI 与外部 .bin buffer、metallic/roughness 直通、双相机路径）、
Radiance .hdr 解析器（平面 / RLE 扫描线、RGBE 指数编解码、f16 位模式转换）、
**音频混音核心（音量缩放与即时生效、多声音线性叠加可分解、播完自动移除、
循环回绕、stop 静音与 SoundId 世代防悬垂、通道上/下混、重采样过零点保持、
软限幅边界）**、WAV 解析器（16-bit PCM 值映射、帧交错保留、奇数长度未知块
跳过、坏魔数 / 非 16-bit / 非 PCM / 声道数报错）与程序化音效（正弦 / 方波
对照解析解逐采样 1e-5 容差、噪声固定 seed 可复现、首尾防爆音包络）、
**headless 游戏集成（iris-game：不建窗口不建 GPU，`GamePlugin` 完整系统链 +
`Time::advance` 确定性推进 + `Input::press / release` 脚本化按键注入 +
临时 fixture 场景文件注入驱动真实游戏世界——静止 / 移动积分与朝向 / 单次
跳跃弧线 / 相机子实体层级跟随 / 脚本路线拾取与计分 / 金币可见性隐藏 /
HUD 文本刷新 / 全收集胜利与终局冻结 / 超时落败 /
**墙体 Collider 阻挡（玩家稳定贴墙 17.05，小于钳制上限 18——是物理而非
clamp 在挡）/ 贴墙滑行（法向被挡、切向持续滑行）**）、
**headless 竞技场集成（iris-arena：同款驱动 + 双池预热断言 + 蒙皮动画推进 +
任意键开局取池 + 波次间池槽位复用（第 2 波复用第 1 波全部 5 槽）+ HUD
四要素刷新 + 弹道池封顶与槽位回收 + GAME OVER → R 重开闭环（旧 id 全释放、
事件计数复位）+ 混战 draw calls < 300 记录性断言（实测 25））**、
**物理与碰撞（球-球 / 球-AABB（顶面 / 侧向 / 角点 / 盒内退出）/ AABB-AABB
接触几何与反弹 / 摩擦均值的纯函数断言；自由落体对照 y(t)=h-½gt² 的 60Hz 离散
容差、gravity_scale 缩放、e=0.8 回弹顶点 ≈ 初高×0.64 且逐次衰减、e=0 落地静止
穿透 <1e-3 不抖、摩擦切向衰减对比、线性阻尼对照 v₀e^(-λt)、静态体 bit 级不动、
双世界逐采样 bit 级确定性、7 球堆叠两两不穿透、100 球密集阵求解后两两不重叠
（broadphase 候选保守性回归）、Time 缺席零步长；
静态推离查询：单墙推离量精确且只沿法线（切向保留）/ 正交双墙顺序累加 /
空静态表恒等 / 世界封装跳过动态体并遵循静态缺席语义；
射线与线段查询：球面命中 t/法线精确值 / 未命中与背面 / 球内起点 t=0 /
盒面命中 / 盒内起点最浅出面 / 斜射角点法线取主轴 / 线段步长大于目标直径
仍命中（穿模防护）/ 世界级最近命中优先 + filter 排除发射者 + max_t 裁剪 +
空世界 None；球扫掠：畅通 None / 正碰 t 分数与法线精确 / 斜向滑过角点
不误报 / 起点重叠 t=0 / 世界封装跳过动态体；均匀网格：插入查询往返 /
负坐标 floor / 同输入两次 candidates 相等（BTreeMap 确定性）/ 确定性 LCG
生成 ~200 AABB 的保守超集断言（凡真重叠对都出现在双方候选里））**、
**场景序列化（最小场景与字段缺省值 = 引擎默认、未知字段容错、坏 JSON /
缺 entities / 缺文件 → 错误枚举各归其位；实体段逐字段进组件（Name / Visibility /
Transform 四元数 / 材质数值 / 点光 / 碰撞体 / 刚体）、静态体缺席语义、
glTF 引用挂载子实体树 + GltfMounted 标记、GLB 容器引用、方向光环境资源
（缺席不改）、sky_source 合成 HDR → EquirectHdr + 路径登记、材质贴图文件
载入 + 登记；往返：丰富场景（全组件 + glTF + 贴图）save → load → save
二次保存逐字节一致且逐实体语义等价、glTF 内部实体不泄漏进场景文件、
非场景来源 mesh / 材质优雅降级告警、默认材质段省略）**。

## 跨平台矩阵

| 平台 | 图形后端 | 状态 |
|------|----------|------|
| Windows x64 / ARM64 | DirectX 12 / Vulkan | ✅ 已验证（本机 DX12） |
| Linux x64 / ARM64 | Vulkan | ✅ wgpu 官方支持 |
| macOS / iOS | Metal | ✅ wgpu 官方支持 |
| Web (wasm32) | WebGPU | ✅ **端到端验证通过**：全 workspace 编译零警告 + 真实浏览器渲染确认（点光场景 + HUD 完整成像，见 `docs/web_preview.png` 与下方「Web 构建」） |

### Web 构建（wasm32-unknown-unknown）

需要 WebGPU 浏览器（Chrome / Edge 113+）；wgpu 30 在 wasm 下只编译 WebGPU 后端。

```sh
rustup target add wasm32-unknown-unknown
# wasm-bindgen-cli 版本必须与 Cargo.lock 中 wasm-bindgen 一致（先 grep 查询）
cargo install wasm-bindgen-cli --version 0.2.127 --locked
cargo build --example web --release --target wasm32-unknown-unknown
wasm-bindgen --out-dir web-demo --target web     target/wasm32-unknown-unknown/release/examples/web.wasm
# web-demo/index.html 已随构建脚本就位；起静态服务器访问：
npx serve web-demo    # 或 python -m http.server -d web-demo
```

平台要点（详见 ARCHITECTURE.md §6）：wasm 上 `Instant` 由 `performance.now()` 封装
（std 版本运行时 panic）；无 std 线程，并行系统自动内联顺序执行；GPU 适配器请求
是 JS Promise，引擎在事件循环内逐帧异步驱动（同步阻塞会卡死页面）；`env_logger`
编译通过但无输出（v1 未接 console log）；canvas 由 winit 自动附加到 `<body>`；
音频设备层不可用（cpal 不支持 wasm32，`AudioOutput::device()` 恒 `None`），
混音核心照常编译、`Audio` 资源自动降级为纯混音器。

## 路线图

- [x] 变换层级（父子级联）与全局变换传播
- [x] 多光源（点光数组）与视锥剔除
- [x] glTF 2.0 资产管线（网格 / 材质 / 纹理 / 节点层级）
- [x] 实体挂载相机（层级 × 相机）
- [x] 方向光阴影贴图（正交深度 pass + PCF）
- [x] PBR 材质模型（Cook-Torrance）与 HDR / ACES 色调映射
- [x] 法线 / ORM 材质贴图通道（切线空间 TBN 两路 + 逐像素表面参数，R42）
- [x] 泛光（Bloom）后处理
- [x] IBL 环境光照（程序化天空 + 分割求和）
- [x] 天空盒背景 pass（射线采样，与 IBL 同源）
- [x] GLB 二进制容器与 glTF 节点动画（AnimationPlayer + crossfade 混合 / 状态机 / 事件轨道）
- [x] 骨骼蒙皮动画（GPU LBS + 蒙皮阴影）
- [x] 外部 HDR 资产加载（equirectangular）
- [x] 点光阴影（cubemap，首灯 6 面归一化距离）
- [x] 2D UI 叠加层（HUD：屏幕空间矩形 + 位图字体文本，单批单 draw）
- [x] 多线程系统调度（声明式读写集 + 冲突分批并行）
- [x] wasm32 Web 目标端到端验证（全 workspace 编译 + Instant/线程/异步 GPU 初始化平台封装 + 真实浏览器渲染确认）
- [x] 音频子系统（纯软件混音器 + cpal 设备层 + WAV 加载 + 程序化音效生成，wasm32 降级为纯混音器）
- [x] 3D 空间音频 v1（R25：距离衰减 + 恒功率立体声声像纯函数模型 + 听者/声源组件同步系统；Iris Arena 开火/命中音 dogfooding）
- [x] 音频 v2（R44：OGG/Vorbis 解码 + 流式音乐总线（后台线程分块解码、回调 try_lock 取块、60 s 立体声常驻 0.41 MB vs WAV 22 MB）+ 音乐总线 Schroeder 混响；解码器 symphonia 零 unsafe）
- [x] 实例缓冲 GPU 持久化与增量更新（帧级比对：静态场景零上传，动态实体仅传变化段）
- [x] 物理与碰撞 v1（Sphere/AABB 刚体：积分 / 检测 / 响应 / 迭代松弛，纯函数确定性求解器）
- [x] 场景序列化（JSON 双向：环境段 + 实体段 ↔ 世界实体，登记表驱动的资源来源往返）
- [x] 物理查询 API（射线 / 线段 raycast + 球扫掠 shapecast）与均匀网格 broadphase（候选升序 + `j > i` ⇒ 配对遍历序与 O(n²) 完全一致，bit 级确定性不破坏）
- [x] 竖直 Capsule 碰撞体 + 运动学角色控制器（R31：`ColliderShape::Capsule` 一等形状贯通 broadphase / 检测 / 射线 / 扫掠；`move_and_slide` 五步管线——水平滑行 / 台阶 / 斜坡限 / 贴地吸附 / 跳跃，纯函数确定性）
- [x] 导航网格与寻路（R35：2.5D `NavGrid` 光栅化（最近点 ≤ `agent_radius` 保守判定）+ A*（8 邻域防穿角、octile 启发、逐位确定性 tie-break）+ string pulling 平滑（cell 光栅 DDA 视线）+ 端点被阻就近修正；纯数据结构 + Resource，AI 行为结构留游戏侧）→ **R49 高度场语义升级**（`step_height` / `max_slope_radians` 参数、覆盖体顶面解析 + 隐式地面 0、可达顶面雕刻、A* 攀爬惩罚、3D 视线平滑、航点带表面高度 y；多层桥洞留白 v1 限制）
- [x] 物理约束与关节 v1（R36：`DistanceJoint` 距离/弹簧 + `TargetJoint` 目标点约束进 `step_physics` 松弛循环（每轮接触之后，约束优先级高于接触），纯函数求解器 + bit 级确定性；旋转约束留待角速度扩展）
- [x] 布娃娃与死亡混合 v1 + 碰撞过滤掩码（R37：`Collider.groups`/`mask` 默认全 1 的对称配对过滤（被过滤对不进接触事件，broadphase 不动、确定性不破坏）；ragdoll 球肢近似——`spawn_ragdoll_kinematic` 静态肢体（动画接管期，自碰撞免抖）→ `release_ragdoll` 翻转动体 + 继承速度 + 冲量（物理接管期），`DistanceJoint` 硬约束链不散架；旋转铰链仍待角速度扩展，见 `iris-core::ragdoll` 文档）
- [x] 角速度物理 + 旋转感知胶囊 + SwingJoint + 真人形 ragdoll v2（R50：`RigidBody.angular_velocity`/`angular_damping` + 四元数微分式积分（每步归一化、与平移不耦合、零角速度跳过 ⇒ 既有场景 rotation 逐位不变）；`ColliderShape::Capsule` 轴随实体旋转贯通检测 / 射线 / 扫掠 / 推离 / broadphase 包络（线段最近点算法零数学改动，恒等旋转逐位兼容）；`SwingJoint` = 锚点距离投影 + 摆动锥限位（锥轴随父端）+ 子端肢体轴最小弧定向 + 两轴速度阻尼（层级投影，根先叶后每轮精确满足）；ragdoll v2 十肢胶囊人形（`humanoid_v2`：肩/肘/胯/膝限位自动接线）+ 释放时冲量相关角速度注入（绕链根肩线前扑）；**接触不产生角冲量**、Box 仍 AABB（OBB 留后续轮）、无 hinge/twist 铰链——诚实清单见 ARCHITECTURE §3.8/§8；v1 球肢与六游戏逐位不变；验收 `examples/ragdoll_v2_snapshot.rs` → `docs/ragdoll_v2.png`）
- [x] Heightmap 地形：分块 3 级 LOD 渲染 + 碰撞拼块 + 导航 / 角色集成（R51：`iris_render::terrain`——`Heightmap` 高度场 + `generate_hills` 三频方向正弦叠加（splitmix64 派生、无 RNG、同 seed 逐位一致）；`height_at` 双线性 / `normal_at` 中心差分 + `Terrain::height_at_world` 贴地查询；**分块 + 3 级距离 LOD**（`LodMesh3` = `LodMesh` 三级一般化，收集期按 chunk 到相机距离选级，`triangles_drawn` 即验收口径）；**无 skirt 接缝不变量**（每级边界线全分辨率采样 + 拉链条带三角化 ⇒ 共享边顶点 y / 法线 / UV 逐位一致，9 种 LOD 组合单测锁定，UV 世界平铺跨块无缝）；**碰撞 = 自动粗 AABB 拼块静态体**（顶面贴 cell 最高角 ⇒ 台阶语义与 `step_height` 咬合，角色控制器 / 射线 / R49 导航图零 API 扩展可用；nav 边界须外扩地形域的衔接规则入文档）；`grass_textures` 程序化草地三件套；诚实限制：碰撞是台阶非光滑坡面、无滞回 / 流式 / 坡度染色、LOD 固定 3 级（§4.23 / §8.28）；验收 `examples/terrain_snapshot.rs` → `docs/terrain.png`）
- [x] 透明渲染（R23：alpha 混合材质 + 视深排序透明 pass）
- [x] 粒子系统 v1（R24：Emitter 组件 + CPU 确定性模拟 + billboard 实例化渲染，additive / alpha 双混合）

### Web 端到端实测

`docs/web_preview.png`：真实浏览器（WebGPU）中加载 wasm 版引擎的截图——
位图字体标题 "IRIS WEB"、血条 HUD、四个彩色方块 + 中央点光径向照明 +
天空盒背景完整成像。整条链路（wasm 模块实例化 → winit canvas →
WebGPU 适配器/设备 → 全渲染栈 → HUD 叠加）在浏览器主线程上逐帧运行。

## License

MIT OR Apache-2.0
