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
iris-macros (proc-macro)
    ↑
iris-core ──── iris-platform
    ↑              ↑
iris-render ───────┘
    ↑
iris-engine（门面，用户唯一入口）
```

- **iris-macros**：`#[derive(Component / Resource / Event)]`。通过 `extern crate self as iris_core`
  技巧让派生宏在 crate 内外都能解析 `::iris_core::...` 路径（serde 同款做法）。
- **iris-core**：不依赖任何图形/窗口库，可独立用于无渲染的服务端模拟或工具链。
- **iris-render**：只依赖 winit 类型做窗口句柄，不反向依赖 engine 门面；离屏渲染完全不触碰窗口。
- **iris-platform**：窗口配置与输入状态，薄封装 winit。
- **iris-engine**：App 框架、插件、winit `ApplicationHandler` 主循环，把三者粘合。

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
- 传播在用户 Update 系统之前执行（`HierarchyPlugin`，包含于 `DefaultPlugins`）。

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
  Transform，读 Time）——DefaultPlugins 每帧真实走并行路径；
  独占的层级传播紧随其后形成屏障。
- **实测**：16 核上 8 个互不冲突分量系统 3.27× 加速
  （`cargo run -p iris-core --example bench_parallel --release`）。
  未达线性是每系统内部仍为单线程遍历 + 线程启动开销 + 内存带宽上限。
- **正确性断言**：并发观测（原子计数器峰值 ≥ 2）、冲突系统分批保序、
  独占屏障可见性，均有确定性测试；诚实性由「冲突即 panic」兜底——
  声明不诚实的并行系统会在运行期被锁检查当场击穿。

## 4. 渲染管线（iris-render）

### 4.1 帧流程

```text
World 查询 (Transform, MeshHandle, MaterialHandle)
  → 实例数据 InstanceRaw（模型矩阵 + 法线矩阵，112B/实例）
  → 按 (网格, 材质) 稳定排序 → 相同 key 连续成批
  → 一次 write_buffer 上传全部实例
  → 阴影 pass（可选）：光源正交视角，深度写入 R32Float 颜色附件（2048²）
  → 主 pass：天空盒背景（inv_view_proj 射线采样预滤波环境 0 级 mip，深度只读）
    → 几何体渲染到线性 HDR 中间缓冲（Rgba16Float + 4x MSAA → resolve），
    环境光采样初始化时烘焙好的 IBL 贴图（辐照度 / 预滤波 / BRDF LUT）
  → 泛光（可选）：亮度提取 → 高斯模糊（半分辨率三轮分离迭代）
  → blit pass：叠加泛光 + ACES 色调映射 → sRGB 目标
  → queue.submit（异步）→ present
```

### 4.2 自动实例化批处理

- 排序键 `(mesh_id << 32) | material_id`，排序后相同 key 天然连续，
  渲染器无需预知场景即可把 **同网格同材质的所有实体压成一个 draw call**。
- 实测：4096 实例 + 地面 = 2 draw call（见 `instanced` 示例与集成测试断言）。

### 4.3 着色与颜色管线（PBR + HDR）

- **Cook-Torrance PBR**（`standard.wgsl`，金属度-粗糙度工作流）：
  法线分布 GGX、几何遮蔽 Smith-Schlick（k = roughness²/2 直接光近似）、
  菲涅尔 Schlick（绝缘体 F0 = 0.04，金属 F0 = albedo）；
  能量守恒 `kd = (1 − F)(1 − metallic)`，金属无漫反射。
- **照明**：环境光（**IBL 分割求和**，见 §4.12；另有方向光资源上的常量补光项）+
  1 个方向光（受阴影调制）+ 最多 8 个点光。点光采用**物理平方衰减**
  `window² / d²`（`window = 1 − (d/range)²`），在有效半径处平滑归零。
- 法线矩阵在 CPU 端取模型矩阵的**逆转置 3×3**，保证非均匀缩放下光照方向正确。
- **HDR 管线**：主 pass 渲染到线性 `Rgba16Float` 中间缓冲（MSAA 4x 解析），
  全屏三角形 blit pass 做 ACES 色调映射（Narkowicz 解析近似）后输出 sRGB——
  光照在线性 HDR 空间累加，高光滚降由 ACES 完成，不会出现死白截断。
- WGSL 结构体对齐陷阱：`vec3<f32>` 在 WGSL 中对齐 16 字节，会把只含一个 vec3 的
  uniform 结构撑到 16 字节倍数；Iris 用 3 个独立 `f32` 填充保持与 CPU 侧 48 字节一致
  （这是 GPU 编程常见坑，集成测试曾实际捕获该问题）。

### 4.4 视锥剔除

- Gribb-Hartmann 方法从 view_proj 提取 6 平面（适配 z ∈ [0,1] 的 DirectX/WebGPU 深度
  约定：near = row₄，far = row₄ − row₃）。
- **平面必须按法线长度归一化**后才能与世界单位的包围球半径比较——未归一化的
  far 平面法线长度只有 ~0.001，比较结果毫无几何意义（单测实测抓出过该问题）。
- 剔除对象为「变换后的包围球」：局部 AABB 的中心/半径经模型矩阵变换，
  半径按矩阵三列长度的最大值保守放大；不可见实体不进入批处理与实例缓冲。
- 渲染器通过 `FrameStats` 暴露剔除后的提交数，集成测试据此断言
  「相机背后 27 实体零提交、视锥内实体不误伤」。

### 4.5 实体挂载相机

- 相机有两种存在形态：全局 `Camera` 资源（简单场景）与挂在实体上的
  `ActiveCamera` 组件（跟随层级运动，第三人称 / 载具视角）。
- 渲染器每帧优先查询 `(&GlobalTransform, &ActiveCamera)`：eye 取实体世界位置，
  视图朝向取实体局部 **-Z 前方、+Y 上方**（`view_from_global`）；
  无挂载实体时回退到 `Camera` 资源，两条路径的投影参数（fov / near / far）语义一致。
- 挂载相机会被视锥剔除与光照 uniform 复用（eye 位置同时供高光计算）。

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
  节点索引→实体映射 + 全部动画剪辑）。`spawn_gltf` 为只取实体树的便捷包装。
- **GLB 容器**：按内容嗅探（magic "glTF"），校验版本（仅 2）、chunk 边界与
  顺序（JSON 首位、BIN 仅一个且在 JSON 后）；BIN chunk 作为无 uri 的
  buffer 0 数据源。`write_glb` 为对称的容器组装工具（gen_asset 示例使用）。
- **去重**：同一 glTF 的 mesh / material / texture 按 (索引[, primitive]) 缓存句柄，
  多节点引用同一网格不重复上传 CPU 目录。
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
  `instance.player()` 一行挂载。
- **系统**（`update_animations`，DefaultPlugins 在层级传播前注册）：
  两阶段——先只读查询推进时间并采样出「目标实体 → 覆写字段」任务列表，
  再统一写入目标 `Transform`（被动画的属性覆写，其余字段保持静止姿势）。
  `Time` 资源缺席时 dt=0，可手动设置 `player.time` 定位到任意时刻（离屏快照用）。
- **单帧 delta 钳制**：`Time.max_delta`（默认 0.25s）会钳制大步长——
  测试或回放需要确定性大步长时分多帧推进。

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
  draw，不参与视锥剔除——关节可把顶点移出绑定姿势包围盒）；实例区间
  紧随普通实例之后；阴影 pass 使用蒙皮阴影管线复用同一份关节矩阵，
  保证阴影与主 pass 几何逐顶点一致。
- **加载器**：`skins`/`skin` 节点字段、MAT4 accessor、IBM 缺省时为
  恒等（规范允许）；权重和偏离 1 时归一化（防御非法资产）；
  JOINTS_0 与 WEIGHTS_0 必须成对出现，否则报 `Missing`。
- **限制**：关节非均匀缩放下法线变换不精确（按刚性处理）；蒙皮网格
  无视锥剔除；多 primitive 共享蒙皮时关节矩阵每 primitive 各上传一份。

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
- **覆盖范围**：以注视点为中心、`shadow_range`（默认 24）为半边长的正交盒；
  光源相机沿光传播方向反向拉远 `2×range`，保证中心两侧 `±range` 的深度都落在 [0,1]。
- **uniform 控制流合规**：`textureSample` 要求 uniform 控制流，
  越界 / 光源背后 / 阴影关闭都不做提前返回，采样照常执行、最后用掩码混向「受光」。
- **同 pass 读写冲突**：shadow pass 使用只含 uniform 的精简绑定组
  （`shadow_globals_bg`）——完整全局组引用了阴影贴图本身，
  在同一 render pass 内会触发资源互斥校验错误。
- 阴影只调制方向光的辐射度，不影响环境光 / 点光 / 自发光；
  `DirectionalLight.shadows` 可整帧关闭（贴图 pass 跳过 + shader 掩码兜底）。

### 4.8 MSAA 与深度

- 固定 4x 多重采样：主 pass 渲染到 **HDR（Rgba16Float）MSAA 附件** +
  `resolve_target` 解析到单采样 HDR；深度 `Depth32Float`，`Less` 比较 + 写入。
- HDR 中间缓冲按帧尺寸懒创建，窗口 resize 时同步重建。
- 交换链 `PresentMode::Fifo`：全平台可用、天然限流不空烧 GPU。

### 4.9 GPU 资源生命周期

- CPU 目录（`MeshStore / MaterialStore / TextureStore`，World 资源）与
  GPU 缓存（`Renderer` 内 HashMap）分离，**句柄即弱引用**。
- 惰性上传：渲染时首次遇到的句柄才上传 GPU；重复句柄零开销。
- 帧统计（`FrameStats`）暴露 draw call / 实例数，供性能面板与测试断言。

### 4.10 离屏渲染

`OffscreenRenderer` 与窗口渲染共享同一 `Renderer` 核心（管线、批处理、上传逻辑完全一致），
仅目标是纹理 + 读回缓冲。用途：CI 集成测试、截图导出、无头环境。
GPU 适配器缺失时构造返回 `None`，调用方优雅降级——测试可跳过而非失败。

### 4.11 泛光（Bloom）后处理

- **三段式**：亮度提取（smoothstep 软阈值，HDR 亮度超过 `BloomSettings.threshold`
  才参与）→ 可分离高斯模糊（9 tap 线性采样，半分辨率，水平/垂直管线共用、
  三轮迭代扩展半径）→ blit 合成（ACES 色调映射**之前**按 `strength` 叠加，
  保证高光泛光同样被色调映射滚降，不会死白）。
- **纯加法不变量**：泛光只增不减（加法 + 单调速映射），集成测试据此断言
  「开/关差分：光晕成片变亮、零像素变暗、远场不变」。
- **uniform 对齐陷阱**：三个 pass 的参数曾放在同一缓冲的 32B 插槽里，
  触发 `min_uniform_buffer_offset_alignment`（典型 256）校验错误——
  绑定偏移必须 256 对齐；改为三个独立小缓冲后消除。
- **关闭路径**：`BloomSettings` 未注册 = 默认启用；`enabled = false` 时跳过
  三个 pass、blit 强度 uniform 写 0（绑定组无需重建，纹理照常绑定）。
- v1 不做降采样链（1/4、1/8 mip 链），三轮半分辨率迭代在质量与占用间取中；
  泛光源来自自发光（`MaterialDef.emissive`，HDR 值可 > 1）。

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

## 5. 内存安全策略总表

| 风险 | 传统 C++ 引擎 | Iris |
|------|---------------|------|
| 悬垂实体引用 | 读到复用后实体的脏数据 | 代数校验，返回 `None` |
| 组件重复可变借用 | 数据竞争 / UB | 尝试型 RwLock 运行期 panic（跨线程生效） |
| 迭代中修改场景 | 迭代器失效 / UB | API 层禁止（`&mut World` 互斥） |
| 并行系统读写竞争 | 数据竞争 / UB | 类型级访问集分批 + 冲突即 panic 兜底 |
| GPU 缓冲悬垂 | 手写生命周期，易错 | wgpu 引用计数 + Usage 校验 |
| 类型混淆的句柄 | 强转 UB | `TypeId` + `downcast` + 泛型封装 |
| 全库 | 手工内存管理 | **零 unsafe，全程借用检查** |

## 6. 跨平台策略

- **图形**：wgpu 运行时枚举适配器（Vulkan → DX12 → Metal → GL），按平台选最优后端；
  WebGPU 语义保证 API 层可移植，WGSL 免除着色器方言问题。
- **窗口 / 输入**：winit 抽象事件源；输入层只暴露「物理键码 + 状态机」，
  布局无关（WASD 在 AZERTY 上依然是相同物理键）。
- **线程模型**：主线程驱动事件循环与渲染（winit 平台约束）；ECS 全部状态
  `Send + Sync`，无冲突系统经 `std::thread::scope` 并行执行（§3.7），
  工作线程按帧创建销毁（批次粒度，无驻留线程池）。
- **交叉编译**：安装对应 std target 即可（如 `aarch64-pc-windows-msvc`、
  `wasm32-unknown-unknown`），工程无平台条件编译分支。

## 7. 性能策略

| 层 | 手段 |
|----|------|
| 数据 | 稀疏集 dense 数组连续遍历；组件按类型分储避免缓存行浪费 |
| 调度 | 声明式读写集冲突分批，无冲突系统多线程并行（§3.7） |
| 数学 | glam：SSE/NEON 自动向量化（release 构建生效） |
| GPU | 自动实例化合批；单实例 112B 紧凑布局；一次 write_buffer 上传全部实例 |
| 提交 | wgpu 命令编码 → 队列异步提交，CPU/GPU 并行 |
| 内存 | 无 GC；实体索引复用；scratch 缓冲跨帧复用（批处理排序数组） |
| 构建 | `lto = "thin"`、`codegen-units = 1`、`strip = "debuginfo"` |

## 8. 已知限制（诚实清单）

1. **并行调度为批次粒度**：无驻留线程池，每帧按批创建线程（批内系统少时
   开销可忽略；高频小批次场景可引入线程池）；分批为类型级保守分析，
   无法表达「分片读同一组件」的数据并行（如按实体区间拆分遍历）。
2. **层级传播为递归实现**：层级深度受栈深限制（游戏场景足够）；每次传播为
   全量重建，海量静态实体场景可考虑脏标记增量传播。
3. **批处理粒度**：按 (网格, 材质) 合批，无深度排序（不透明物体绘制顺序无关紧要，
   半透明需额外排序，暂未处理）。
4. **无热重载**：CPU 目录句柄不可变，资产修改需重建。
5. **Web 目标**：代码路径兼容（无平台分支），但 winit/wasm 窗口层未做端到端验证。
6. **点光无阴影**：方向光已有阴影贴图；点光阴影（cubemap）在路线图中。
7. **外部 HDR 环境源仅覆盖 Radiance .hdr 的 `-Y +X` equirectangular 形式**：
   其他分辨率朝向（±X ±Z 组合、垂直镜像）解析为 Unsupported；不读 `.exr` /
   `.ktx` 等其他 HDR 容器；超大图受设备纹理尺寸上限约束（Rgba16Float 中间
   纹理按原图尺寸上传）。程序化天空仍是缺席 `SkySource` 时的默认（零资产
   依赖）；烘焙在主线程同步执行，换入大贴图的当帧有一次性停顿。
8. **阴影偏移为常数 bias**：无坡度缩放 / 法线偏移（normal offset），大入射角
   掠射表面可能有轻微 acne；PCF 9 采样在实际场景中已足够干净。
9. **动画**：骨骼蒙皮已覆盖刚性关节 LBS（GPU 蒙皮 + 蒙皮阴影）；形变目标、
   CUBICSPLAT 插值与关节非均匀缩放的精确法线变换在路线图中。

## 9. 关键设计取舍记录

- **回调式查询 vs 迭代器**：见 §3.4。牺牲一点语法糖换取零 unsafe 与确定性 panic。
- **稀疏集 vs Archetype**：见 §3.1。Iris 优先结构透明与动态组件能力；
  archetype 的成批迭代优势在实体百万级时才显著。
- **运行期借用检查 vs 编译期**：编译期（如 hecs 的 unsafe 实现或 slotmap 双缓冲）
  换不来「错误 = panic」的明确语义；运行期检查的每次查询只有常数次原子操作，
  开销在纳秒级，换取的是整库零 unsafe。
- **MSAA 固定 4x**：管线单份、代码简单；前向渲染下 MSAA 仍是性价比最高的抗锯齿。
