# R93 海洋 v2 + 浮力（路线图 v5 · 第三梯队 收官役 · 设计定稿）

> 定稿日期 2026-09-20。主会话已完成读码与决策，本文是**执行规格权威**；
> 下游 = 外部 Agent 提示词（见文末「提示词」段，整段可原样转发）。
> 路线图裁定 2 的兑现：海洋波形走 **CPU 频谱合成 + 预烘关键帧位移图**，
> 不做 GPU FFT（跨适配器不可逐位）。

## 〇、用户裁定（2026-09-20，三项全按推荐）

| # | 议题 | 裁定 |
|---|---|---|
| 0-1 | 体量切分 | **拆两轮**：R93 = 海洋 v2（位移图 + 无限海面 + 浮力物理 + 序列化）；**R94 = 水下**（入水检测全屏 pass + 焦散 + 闭合 §8.50⑤）。R94 设计另档。 |
| 0-2 | 位移图时间演化 | **预烘关键帧 + GPU 重心式插值**（加载期纯函数烘 M 层，运行期零 CPU 成本） |
| 0-3 | 浮力落点 | **iris-core 资源 + 纯函数，iris-engine 出同步系统**（iris-core 零渲染依赖不破） |

编号影响：路线图 v5「第三梯队仅剩 R93 一轮」修订为「R93 海洋 v2 + 浮力 / R94 水下 + 焦散」，
`docs/private/roadmap-v5.md` 已在本文档同批改注。

---

## 一、读码结论（本轮的地基，全部实证过）

| 事实 | 锚点 |
|---|---|
| R81 水体是**固定矩形**、以世界原点为中心 ±`half_extent`、不跟随 `Transform` | `iris-render/src/water.rs` `WaterSurface` |
| 水面网格 = 归一化 [−1,1]² 静态网格，`WATER_GRID_SUBDIVISIONS = 96`，VS 里乘 `half_extent` 展开 | `water.rs::water_grid_mesh` |
| 顶点格式 `WaterVertex` 带 `pos_xz`（location 0）**和 `uv`（location 1）**，但 `water.wgsl` 的 `WaterVertexInput` 只声明了 `pos_xz`——`uv` 现成可用作图采样坐标，零顶点改动 | `water.rs::WaterVertex::LAYOUT` ↔ `water.wgsl` |
| 波形是 CPU↔WGSL **逐式镜像**：`gerstner_evaluate` ↔ `water_evaluate`（同一 WGSL 文件内） | `water.rs` / `water.wgsl` |
| 水体组 0 = 独立 5 槽位（0 params / 1 反射 / 2 折射 / 3 scene_depth / 4 过滤采样器），**不扩 globals**（R61/R77/R78 先例） | `pipeline.rs::water_layout` |
| `WaterParamsRaw` = 256 B（R92 由 240 扩容 +`weather` 通道），`const_assert!` 锁尺寸 | `renderer.rs::WaterParamsRaw` |
| 水体的世界量通道先例 = **binding 21 一只 uniform 全局共享**（主相机 / 捕获 / RT 同缓冲，天气是世界状态不是屏幕状态） | `renderer.rs::weather_buf`、`weather.rs::WeatherRaw` |
| 单水面选择键 = 实体位模式升序取首盏（min 交换律 ⇒ 与查询序无关） | `water.rs::select_water_surface` |
| 物理唯一入口 `step_physics(&World)`，**半隐式欧拉内联重力**（`v += g·dt; v *= exp(−damping·dt); p += v·dt`），无 `apply_force/impulse` API，`RigidBody` 无 density 字段 | `iris-core/src/physics.rs::step_physics` |
| 碰撞形状恰 4 变体：`Sphere{radius}` / `Box{half_extents}` / `OrientedBox{half_extents}` / `Capsule{radius,half_height}`；**全库无任何体积 / 浸没辅助**（`shape_inertia` 内部算了胶囊体积但不对外） | `physics.rs::ColliderShape` |
| opt-in 资源范式（R76）：`world.try_resource::<ContactSolverV2>().map(|r| *r)` ⇒ `Option`，`if let Some` 整段分叉，缺席 = 六游戏轨迹 FNV 逐位不变 | `physics.rs` + `solver_v2_tests::v1_path_untouched_without_resource` |
| 水体 ↔ 物理**当前零连接**；场景 / 关卡**完全没序列化水体**（`LevelSection{terrain,nav,bake,ddgi}`，`scene.rs` 无 water/ocean 段）——本轮补账 | `iris-engine/src/level.rs`、`scene.rs` |
| 后处理链槽位机器（R94 才用，本轮不动）：`post::full_chain_slots_with_fog`、`PostPassDesc`/`encode_post_pass`、`make_post_depth_pipeline`；链上同帧读 `scene_depth` | `post.rs` / `renderer.rs` |

---

## 二、本轮范围（四个决策点已定稿）

### D1 频谱位移图（`iris-render/src/ocean.rs`，新模块，全 CPU 纯函数）

**为什么是「预烘关键帧」而不是每帧生成**：每帧 96²×16 平面 ≈ 15 万次 `cos` 常驻帧预算，
违背低占用卖点；预烘把成本挪到加载期一次，运行期只有一次 GPU 双层插值，且
「烘出来的字节」天然可比对（`to_bits()` 逐位单测 + 快照哈希）。

1. **格点谱**：波数 k = 2π·(i,j)/L，整数格点 (i,j) ∈ [−K,K]² 排除 (0,0)，定稿 `L = 64 m`
   （tile 世界周期）、`N = 128`（tile 边纹素数 ⇒ 0.5 m/texel）、`K = 8`
   （⇒ 平面数 = 17²−1 = 288，烘 8 关键帧 = 18.4 万点积，加载期 < 10 ms）。
2. **幅度谱**：Phillips 型 `P(k) ∝ |k|^(−4)`，`A(k) = √(C·P(k))`，乘高频截止
   `exp(−(|k|/k_cap)²)`（k_cap = Nyquist/2 = π/texel_size，防 0.5 m 以下走样）；
   低频截止 `exp(−(k_min/|k|)²)`（防超长波在 64 m tile 内出现不闭合的位移台阶）。
   谱归一化后总波高上界 = Σ A(k) 必须**解析可算**，用 `detail_amplitude` 缩放到
   目标米级（默认 0.05 m 细节层——量级：Gerstner 主层 0.12 m × Σ衰减 2.1）。
3. **相位与色散**：`φ(k)` = 无状态哈希（复用 `weather.rs::weather_cell_hash` 同款
   splitmix/整数哈希流，禁 rand、禁全局状态）；`ω(k) = √(g·|k|)`（深水重力波色散，
   表面张力项裁掉并声明）。**时间量化**：`ω_n(k) = 2π·round(ω(k)·T/2π)/T`，
   `T = 12 s`（循环周期）、关键帧数 `M = 8`（Δt = 1.5 s）。量化后每个平面在 T 上
   严格周期 ⇒ **关键帧 0 与「关键帧 M」逐位相同**（无缝回绕，测试硬锚）。
   量化误差 |ω_n−ω|/ω ≤ π/(ωT) 要有界并在文档给出最坏值（最高频平面误差最小、
   最低频最大——低频平面是主要轮廓贡献者，声明可接受）。
4. **烘图**：`ocean_bake_frames(...) -> Vec<f32>`，布局 **层主序 [frame][row][col]**，
   每层 128²；同时烘 **mip 链**（逐级 2×2 均值，`log2(128)+1 = 8` 级），
   GPU 数组索引 = `frame·MIP_LEVELS + level`（布局写进模块文档 + WGSL 常量同步 +
   源码看门）。上传格式 `R16Float`（位移 ±0.05 m 在半精度下纹素精度 ~1e-4 m，够用；
   8 帧 × 8 mip × 128² × 2 B = 2 MB 显存，一次性）。
   `texture_2d_array` + 新建 `Sampler`（**Repeat** 寻址——tile 无缝是定义的一部分；
   R94/本轮 water 侧现有 sampler 是 ClampToEdge，不复用）。
5. **采样与混合（WGSL）**：`h_detail = bilinear(tile_uv + anchor_phase)`；
   时间插值 **重心式** `h = h_a·(1−u) + h_b·u`（R92 明文教训：**不写 `mix`**，
   线性插值式在 a=1 处有 2 ULP 残差，重心式端点逐位精确 ⇒
   「u=0 ⇒ 与只烘一帧的结果逐位相同」是可判别的测试锚）。
   法线细节 = 同 mip 上 4 邻域纹素**中心差分梯度**（解析度固定于 texel_size），
   与 Gerstner 法线**重心式**混合后归一化。CPU 侧 `ocean_detail_normal_gradient`
   做同式镜像 + 单测。
6. **缺席语义**：世界无 `OceanSurface` = 不烘图、不建数组纹理、不建第二管线、
   零 `write_buffer`、零 draw ⇒ **R81 池塘路径逐位不变**，`water_render.rs`
   四套既有 GPU 测试原值通过。

### D2 无限海面 = 相机跟随双层网格

1. **新组件 `OceanSurface`**（`ocean.rs`）：`{ height, detail_amplitude, tile_period,
   near_extent, far_extent, wind_direction, spectral_slope, + 配色/吸收（与
   `WaterSurface` 同字段名）}`。**与 `WaterSurface` 互斥**：海洋在场 ⇒ 走海洋 pass，
   池塘 `log::warn` 一次并忽略；无海洋 ⇒ R81 语义逐位不变。（理由：把两者并进同一
   选择键会改 R81 的选择语义 = 无谓的基线风险；世界尺度海面与池塘本来两种构图。）
2. **锚点量化**：每层网格世界锚点 = `round(eye.xz / cell) · cell`，`cell` = 该层
   texel 步距（近层 0.5 m、远层 4 m）。相机平移不足一步时顶点位置**逐位不变**
   （测试锚：两个相邻但同格的 eye ⇒ 实例数据逐位相同）。
3. **两层**：`near` = 96² 网格覆盖 ±24 m（Gerstner 大波 + 位移图细节，mip 0）；
   `far` = 同一份静态网格乘 `far_extent`（±192 m，步距 4 m，细节只走法线、
   mip 由步距/texel 比推导 ≈ 3）。交界处理：细节权重按半径 smoothstep 淡出，
   **Gerstner 解析层两层都有且同式** ⇒ 交界只可能淡出细节（声明 §8 新条：
   两层网格交界无波高不连续但细节层有权重台阶式过渡带）。
4. **绘制接线**：`water.wgsl` **同一 shader module 新增 entry point**
   `vs_ocean` / `fs_ocean`（+ 独立 `water_ocean_layout` / `water_ocean_pipeline`），
   池塘的 `vs_main`/`fs_main` **一字不动** ⇒ 零 Gerstner 公式复制（WGSL 无 include
   的双份镜像风险只在跨文件时存在），且不改池塘的顶点布局/绑定。海洋侧的
   逐实例常量（anchor / extent / mip / detail_weight）走 `OceanParamsRaw`
   的数组（2 个实例，uniform 里放 `[[f32;4]; 2]` 再按 `instance_index` 取，
   或直接两次 `draw_indexed` + 两份 params——**定稿：两次 draw，代码最直白，
   每层一次 uniform 覆写**，与 R91 毛发「动态实体每帧 write_buffer」同规模成本）。
   海洋用**反射捕获 + 天空**（池塘同款镜像相机；折射图对无限海无意义——水下内容
   留给 R94，v1 海洋片元色 = Fresnel×反射 + 深水色底，明文声明与 §8.50⑨ 的关系）。
5. **世界量通道**：新增 `Ocean`? 不需要——海面参数是实体组件不是全局资源；
   但**时间源沿用 `Time.elapsed_secs`**（缺席 = t 固定 ⇒ 关键帧 0 + mip 静态，
   §8.50⑫ 同款）。

### D3 浮力（`iris-core/src/physics.rs`，纯函数 + opt-in 资源）

1. **资源 `BuoyancyPlane { height, fluid_density, linear_drag, follow_water }`**
   （`fluid_density` 默认 1000，`linear_drag` 默认 2.0，`follow_water` 见 D4）。
   物理只认世界水平面，不认渲染——**iris-core 零渲染依赖不破**。
2. **浸没体积纯函数**（新增 `buoyancy.rs` 于 iris-core，或 physics.rs 内新段）：
   - `Sphere`：球冠解析 `V_cap = π h²(3r − h)/3`（h = 浸没深度，全浸 = 4/3πr³）。
   - `Box`（轴对齐）：精确 `V = 8·hx·hz·clamp浸没高`。
   - `OrientedBox`：**平面截平行六面体解析体积**。关键论证（必须写进文档）：
     盒内点的世界 y 是局部坐标 `[0,1]³` 的**仿射**函数（仿射∘仿射=仿射），
     带符号深度 `d(u,v,w) = A u + B v + C w + D` ⇒ 浸没比例
     `∫∫∫ 1[d<0]` 是关于 `(A,B,C,D)` 的分段三次闭式（经典 cuboid-plane cut，
     用 8 个角深度符号分类 + 多项式积分）。要求：实现 + 「平放盒退化 = Box 逐位一致」
     「任意角 45° = 与 4³/8³/16³ 中点数值积分在 1e-3 相对误差内」双锚测试。
     **若实现受阻**，允许降级为 16³ 中点采样并声明 O(1/16²)——但必须在 REPORT 里
     写明降级理由，不许静默。
   - `Capsule`：v1 **不实现**，走 AABB 包络近似并 `warn` 一次（胶囊轴任意旋转下的
     浸没体积闭式风险过高，且本仓无浮力消费者需要角色浮力）→ 诚实限制 §8。
3. **施加方式**：`step_physics` 积分段内、重力之后同处（`v += g·dt` 的紧邻）：
   `v += (ρ_f·V_sub/m)·g_up·dt − 附加拖拽`；拖拽按浸没比例走**与既有
   `linear_damping` 同式的指数衰减**（`v *= exp(−(damping + sub_ratio·linear_drag)·dt)`
   ——合并进同一指数项，不新增第二次乘法，避免 R63 类 FMA/舍入外溢）。
   无浸没（`V_sub = 0`）⇒ 该项 `exp(0) = 1`，但**仍会引入一次乘法改变位型** ⇒
   必须用 `if sub_ratio > 0` 分支门（分支内才进新算术），照 R92 sim「select 门」纪律。
4. **v1 拒绝清单**（明文写进模块头与 §8）：不做扶正力矩（浮力只作用在质心，
   姿态无关 ⇒ 船不会自己立起来）、不做附加质量、不做二次拖拽、不做逐顶点水动力、
   不做浪涌对姿态的耦合、不做 `Capsule` 精确浸没。
5. **兼容铁律**：资源缺席 = `step_physics` 全部既有路径逐位不变
   （六游戏轨迹 FNV 锁 + `ContactSolverV2` 范式测试 `v1_path_untouched_without_resource`
   的同款写法：新增 `buoyancy_absent_bitwise_identical_to_v1`）。

### D4 跨界同步（`iris-engine`）

`OceanSurface` / `WaterSurface` 在 iris-render、`BuoyancyPlane` 在 iris-core——
**iris-engine 同时依赖两者**，同步系统合法落这里：

- 新系统 `sync_buoyancy_water_surface(&World)`（注册在 `PhysicsPlugin::build` 内、
  `step_physics` **之前**的 Update 系统，或独立 `WaterPlugin`——定稿：**并入
  `PhysicsPlugin`**，不再新建插件，理由：它是物理的前置准备，注册序即依赖序）：
  仅当 `BuoyancyPlane` 资源在场且 `follow_water == true` 时，把世界内
  `OceanSurface`/`WaterSurface`（同 R81 位模式升序首盏）的 `height` 写入资源；
  `follow_water == false`（默认）⇒ 资源零触碰，游戏手设高度不被覆盖。
- 这闭合 R81 遗留的「水体不可序列化」小缺口，同轮做 **D5**。

### D5 序列化补账（`iris-engine/src/scene.rs` + `level.rs`）

- 场景实体段新增可选 `water`（`WaterSurface` 全字段）与 `ocean`（`OceanSurface`
  全字段）；`RigidBodySection` 邻位新增可选 `buoyancy`（**资源段**，非实体段：
  `height/fluid_density/linear_drag/follow_water`）。
- serde 缺省 = 字段缺席 ⇒ 不挂组件 ⇒ 与今日逐位一致（旧档零语义变化，R92
  `ParticleV2Section` 三新字段的同款口径）。
- 关卡 v2 往返：位级一致断言（`scene_roundtrip` 既有测试族扩展）。

---

## 三、验收硬标准

1. **既有测试零改动全过**（本轮合法基线打破项 = 0；水像素锁四套
   `water_render.rs`、天气六套 `weather_render.rs`、OIT / shadow / normal /
   transparent / sss / post / probe 全原值通过）。若任何既有测试需要改动 = 偏离，
   必须归因 + 重锁程序（AGENTS.md 明文）。
2. **确定性**：`ocean_bake_frames` 两次调用逐位一致；关键帧 0 ≡ 关键帧 M（回绕锚）；
   重心式 u=0/1 端点逐位；浮力同输入双跑 `to_bits()` 逐位；缺席 = 逐位不变三处
   （无 `OceanSurface` / 无 `BuoyancyPlane` / `follow_water=false`）。
3. **GPU**：`tests/ocean_render.rs`（**文件内 `GPU_SERIAL` 互斥，新写套件直接带上**）
   —— 至少 5 项：海洋在场改变全帧 FNV；`detail_amplitude = 0` 与「不烘图」逐位
   （位移层退化为纯 Gerstner 的解析底）；近/远两层覆盖（远场非天空底板的可判别
   ROI）；浮力无关（纯渲染）；双会话逐帧逐位一致。
4. **浮力解析锚**（headless，无 GPU）：平放盒密度 = ρ_f/2 ⇒ 吃水 = 高/2 **精确**；
   球密度 = ρ_f ⇒ 中性浮力（速度零变化，逐位）；全浸 ⇒ V_sub = 解析体积；
   零密度比 ⇒ 自由落体逐位 = 无浮力路径。
5. **验收图** `docs/ocean.png`（`examples/ocean_snapshot.rs`，三面板 960×540，
   双会话逐字节一致 + FNV 差分旁证 + 目检结论写进 CHANGELOG）。
   面板：① 远海地平线 + 涌浪谱波（含 far 层）② 近景细节开/关对照
   ③ 浮力演示（三球/盒不同密度 → 不同吃水静态平衡，机位可判）。
6. **文档三件套**：README 路线图勾选 + `[x]` 行、ARCHITECTURE **§4.52**（新章）+
   **§8.72**（诚实限制，逐条编号）、CHANGELOG Unreleased 段（含基线增量行）。
   AGENTS.md 基线行与状态节顺手更新（当前 debug 1733 / release 1730）。

---

## 四、提示词（整段转发给外部 Agent）

```
# R93：海洋 v2 + 浮力（路线图 v5 · 第三梯队收官役 · 设计定稿见下）

你是 iris-engine 仓库（D:\Development\Complex\iris-engine）的外部工程 Agent。
开工前必读，按顺序：
1. AGENTS.md —— 工程规约权威（门禁顺序与六项、耗时陷阱、测试计数陷阱、
   代码/测试规约、已知平台陷阱）。它的每一句「铁律」都有踩坑成本。
2. ARCHITECTURE.md §4.37（平面水体 v1）、§4.51（天气表面 R92）、§4.45（DDGI v2
   合成语义，仅作 opt-in 范式参考）、§8.50（水体 v1 的十二条诚实限制——本轮
   闭合其中⑦「无频谱」，其余维持并在 §8.72 里说明关系）。
3. 代码：crates/iris-render/src/water.rs（全 515 行）、shaders/water.wgsl（全 268 行）、
   crates/iris-render/src/weather.rs（binding 21 全局共享 uniform 的先例 +
   重心式混合的教训 + `wgsl_mirrors_present` 源码看门写法）、
   crates/iris-render/src/pipeline.rs 的 water_layout / water_pipeline 构造段、
   crates/iris-render/src/renderer.rs 的 WaterParamsRaw / WaterGpu / 水体 pass 编码段 /
   select_water_surface 调用点、crates/iris-core/src/physics.rs 的 step_physics
   积分段与 ContactSolverV2 opt-in 范式（含 solver_v2_tests 的「缺席 = 逐位不变」
   测法）、crates/iris-engine/src/plugin.rs 的 PhysicsPlugin、
   crates/iris-engine/src/scene.rs 的 section 登记与往返测试族。

## 任务（设计已定稿，逐点实现；任何一点要偏离都必须先停下来在 REPORT 里声明理由）

D1 频谱位移图（新模块 crates/iris-render/src/ocean.rs）
  - L=64 m tile / N=128 纹素 / 格点 K=8（288 平面）；Phillips |k|^-4 幅度谱 +
    高频高斯截止（k_cap = π/texel_size）+ 低频截止；相位 = 无状态整数哈希流
    （照 weather.rs::weather_cell_hash / particles.rs::mix64 口径，禁 rand 禁全局态）
  - ω(k)=sqrt(g|k|) 量化到 2π/T 整数倍，T=12 s，M=8 关键帧 ⇒ 第 M 帧与第 0 帧
    逐位相同（硬锚测试）；量化相对误差的最坏值写进模块头
  - CPU 烘 texture_2d_array 数据：布局层主序 frame*MIP_LEVELS + mip，mip = 逐级
    2x2 均值；R16Float；总波高上界解析可算并缩放至 detail_amplitude（默认 0.05 m）
  - WGSL 侧双线性采样 + 时间重心式插值（**禁止 mix，一律 x*(1-a)+y*a**）+
    4 邻域中心差分法线梯度；CPU 镜像函数 + 源码看门单测（数组布局常量两侧同步）

D2 无限海面（相机跟随双层网格）
  - 新组件 OceanSurface（字段见定稿），与 WaterSurface 互斥（海洋在场则 warn 一次
    忽略池塘）；无 OceanSurface = R81 池塘路径逐位不变
  - 锚点量化 round(eye.xz/cell)*cell；near 96²/±24 m/mip0 全位移，
    far 同网格 ±192 m/步距 4 m/细节只进法线 + 高 mip；细节权重按半径 smoothstep
  - **在 shaders/water.wgsl 同一模块内新增 vs_ocean/fs_ocean**，池塘 vs_main/fs_main
    与 water_pipeline/water_layout 一字不动；新增 water_ocean_pipeline + 独立 layout；
    两次 draw_indexed、每层一次 OceanParamsRaw 覆写（不扩 globals 组，binding 21
    先例：不往 Globals 前缀塞东西）
  - 反射捕获复用池塘镜像相机机制；海洋 v1 不做折射图（深水色底 + Fresnel，
    §8.72 明文）；scene_depth 消费者链位不变

D3 浮力（crates/iris-core/src/physics.rs，纯函数 + opt-in 资源）
  - BuoyancyPlane{height, fluid_density=1000, linear_drag=2.0, follow_water=false}
  - 浸没体积：Sphere 球冠解析 / Box 精确 / OrientedBox 用「盒内世界 y 是局部坐标
    仿射函数 ⇒ d=A u+B v+C w+D ⇒ 浸没比例分段三次闭式」的解析实现，
    双锚测试（平放退化 = Box 逐位一致；任意姿态 = 与 16³ 中点积分 1e-3 相对误差内）；
    Capsule 走 AABB 包络 + warn 一次并诚实声明
  - 施力点：step_physics 半隐式积分段、重力行紧邻；拖拽合并进既有指数衰减
    （v *= exp(-(damping + sub_ratio*linear_drag)*dt)），且整个新分支用
    sub_ratio>0 的 if 门包住（**无浸没不得进入任何新算术**，R92 select 门纪律）
  - v1 拒绝清单必须原样写进模块头与 §8.72：无扶正力矩 / 无附加质量 / 无二次拖拽 /
    无 Capsule 精确浸没 / 无浪涌姿态耦合

D4 跨界同步（crates/iris-engine/src/plugin.rs，PhysicsPlugin 内、step_physics 之前）
  - sync_buoyancy_water_surface：仅当 BuoyancyPlane 在场且 follow_water=true 时把
    水面（Ocean/Water，位模式升序首盏）的 height 写入资源；否则资源零触碰

D5 序列化补账（crates/iris-engine/src/scene.rs + level.rs）
  - 实体段可选 water / ocean；资源段可选 buoyancy；serde 缺省 = 不挂 = 逐位同今日；
    关卡往返位级一致断言扩展（旧档零语义变化）

## 验收（硬标准）
1. 既有测试**零改动**全过；全部既有像素/轨迹 FNV 基线**原值通过**
   （本轮不允许任何基线打破；若出现，停下并在 REPORT 里归因，不要偷偷改锁值）。
2. 确定性：烘图两次调用 to_bits 逐位；关键帧 0 ≡ M；重心式端点逐位；
   浮力同输入双跑逐位；三处缺席逐位不变（无 OceanSurface / 无 BuoyancyPlane /
   follow_water=false）。
3. 新 GPU 套件 tests/ocean_render.rs ≥5 项，**文件内 static GPU_SERIAL 互斥**
   （新套件一律自带，别等挂死再补；无适配器时 eprintln!("跳过：环境无 GPU") + return，
   不 panic）。浮力测试全 headless。
4. 解析锚不许写「大于 0」式摆设断言：平放盒 ρ=ρ_f/2 ⇒ 吃水精确 = 高/2、
   ρ=ρ_f ⇒ 中性浮力逐位、全浸 ⇒ 解析体积、零密度比 ⇒ 与无浮力路径逐位。
5. 每个公开旋钮至少一条**非默认值**断言（R89 教训：detail_amplitude /
   tile_period / T / fluid_density / linear_drag / follow_water / spectral_slope
   全部覆盖）。
6. 快照 examples/ocean_snapshot.rs → docs/ocean.png 三面板（960×540），
   跨运行**逐字节一致** + 双会话断言 + 硬像素判据（写明阈值依据）；
   BMP/PPM→PNG 用 ffmpeg（PATH 里有）。
7. 文档三件套：README 路线图 + ARCHITECTURE §4.52 新章 + §8.72 诚实限制
   （含「本轮闭合 §8.50⑦、其余十一条维持」的关系说明）+ CHANGELOG Unreleased 段。

## 门禁与汇报
六项门禁全绿，严格照 AGENTS.md 顺序与铁律：
  cargo fmt --all --check
  cargo build --release --workspace --all-targets
  cargo test  --workspace           --quiet
  cargo test  --workspace --release --quiet
  cargo clippy --workspace --all-targets -- -D warnings
  cargo clippy --workspace --target wasm32-unknown-unknown -- -D warnings
每门前先 `tasklist | grep -iE "cargo|rustc|sccache"` 清残留进程；全部门禁串行不并行；
所有 cargo 命令前缀 `CARGO_HTTP_CHECK_REVOKE=false`；`RUSTC_WRAPPER=""` **只加**
最后那条 wasm clippy；先 build 后 test（否则 release 编译两轮）。
测试计数用单次完整调用重定向文件再逐二进制加总
（`cargo test --workspace 2>&1 | grep -oE "[0-9]+ passed" | awk '{s+=$1} END {print s}'`），
**禁用 grep -v "0 passed"**。当前基线 debug 1733 / release 1730，逐二进制明细也要给。

REPORT.md 写在仓库根（**不入库，不要 git add**）：改动清单（文件 + 行数）、
逐二进制测试数与总数、六项门禁表（含每门耗时与 sccache 命中率若可得）、
设计决策落地情况逐条对照（D1~D5）、**与本规格的偏离及理由**（没有偏离也要写「无」）、
诚实限制草案（准备进 §8.72 的逐条）、以及你踩到的引擎摩擦（若有）。
**不做任何 git 写操作**（不 commit / 不 branch / 不 tag / 不 push）。
```

## 五、主会话验收清单（汇报到达后）

1. 清残留 cargo 进程 → `git status --short` 看改动面（`REPORT.md` 应为未跟踪）。
2. 亲审 1–3 处关键 diff：① OrientedBox 浸没体积解析式的推导是否真在代码里成立
   （仿射性论证 + 分段边界）；② ω 量化到 2π/T 整数倍的回绕是否真严格（关键帧 0 ≡ M
   的锚是真锚还是容差锚）；③ 浮力分支的 `sub_ratio > 0` 门是否真的让无浸没路径
   一行新算术都不进。
3. 六项门禁串行复跑 + 计数对账（与 REPORT 逐二进制对齐）。
4. `examples/ocean_snapshot.rs` 复跑两遍 → 逐字节一致 + 目检 `docs/ocean.png`
   （涌浪有没有「平移爬行」、far/near 交界、地平线有没有露缝或锯齿带）。
5. 确认既有像素锁**原值**通过（不是被重锁过）；`git diff` 里出现任何
   `tests/*.rs` 既有断言值的改动都要追问归因。
6. 删 `REPORT.md` → 中文 conventional 提交（body 写四点设计裁定 + 偏离声明 +
   §8.72 逐条）。
7. 文档一致性：AGENTS.md 基线行 + 状态节、README、ARCHITECTURE §4.52/§8.72、
   CHANGELOG、`docs/private/roadmap-v5.md`（第三梯队 → R93 ✅，R94 水下挂上）。


---

## 六、实现回写（同日，主会话直接实现，未走外部 Agent）

用户裁定「直接开工」⇒ 本轮由主会话实现（跳过提示词转发环节）。定稿的 D1~D5
全部落地，六处偏离设计规格（每条都在实现期由证据驱动，非偷懒）：

1. **位移图 mip 链 → 远场完全不吃谱细节**。`texture_2d_array` 各层尺寸必须
   一致，关键帧与 mip 无法共用一个数组；带限双图（近 16 m/0.25 m + 远
   64 m/2 m）要两套格点与两套周期常量，影子契约面翻倍，而远场 6.7 m 网格
   采 0.25 m 图本来就必然混叠 ⇒ 定稿「远场 vertex/normal 两权重恒 0，只剩
   Gerstner 涌浪」。§8.72 头条限制。
2. **WGSL 由「同文件加 entry point」改为 Rust 侧拼接**。实测：同一 shader
   module 是一个全局命名空间，池塘的 `@group(0)@binding(0) WaterParams` 与
   海洋的 `OceanParams` 按 binding 号直接冲突（naga 报 redefinition）；
   备选是第三份 Gerstner 拷贝或共享 group(0) + dummy 资源。改为
   `water_common.wgsl`（Gerstner 求和 / normalize_safe / 波形常量 /
   `WaterVertexInput`）分别拼进两个 module ⇒ 重复按构造不存在，连看门都不
   需要（保留一条「海洋文件不得自带 water_evaluate」的判别断言）。
   代价：水体 WGSL 不能脱离 Rust 侧单独用 naga CLI 校验。
3. **谱斜率默认 1.5 而非 Phillips 2.0**。首版验收图目检失败：细节开 / 关两
   面板近处扫描线亮度差 < 1/255，谱细节完全不可辨。归因：Phillips 档振幅
   `∝ k^{-2}` ⇒ **斜率谱 `∝ k^{-2}`**，法线扰动被最长波吃光（物理对，观感
   错）。改 `spectral_slope` 为公开旋钮、默认 1.5（斜率谱 `∝ k^{-1}` 对数
   铺开），`detail_amplitude` 0.06 → 0.12 ⇒ 第二版图上 2–16 m 短波与暖色
   碎闪清晰可见。新增测试锁定「斜率谱随 slope 单调 + RMS 恒 1」。
4. **海洋不接 R92 雨涟漪**，且 `OceanParams` 不留 `weather` 空槽（避免第三
   份 `weather_ripple_gradient` 镜像 + R89「死旋钮」教训）。
5. **胶囊浸没**从「AABB 包络 + warn」改为**竖直包络线性比例**（同精度阶、
   无日志噪声，且横躺退化档有专门断言）。
6. **水阻**从定稿的「合并进同一指数因子」改为**分支内第二段指数**——合并式
   会给零浸没路径也添一次加法，违背「无浸没 = 一行新算术都不进」。

实现期抓到的两个自身错误（都进了断言，不是靠肉眼）：

- 浮力集成测试的「判别力旁证」首稿把水面设在落体轨迹之外（4 m vs 落点
  4.84 m），`assert_ne!` 以左右相等自曝 → 水位必须落在轨迹上才算判别。
- 快照的浮体断言首稿按质量排序取前三只，把中性球（3054 kg）当成 0.4ρ 箱
  （3200 kg）→ 排序错位；改用**阿基米德判据** `V_sub/V == ρ_b/ρ_f` 逐体
  断言，与身份无关。
- 快照的物理推进首稿把 `Time` 留在局部变量 ⇒ `step_physics` 的 dt = 0，
  四只浮体全冻在出生高度，被吃水断言当场抓住（Time 必须在世界里推进）。
- 掠射海面的颜色启发式不成立：3 m 机位看 100 m 外俯角只有几度，Fresnel 趋
  1 ⇒ 海把天空镜像回来，「海洋应比天空暗」实测反向。判别改走几何落点
  （投影 100 m 处的点必须从海翻成天空）+ 环带差分计数。

未做（明确留给 R94 / 池）：水下全屏 pass、焦散、水像素写 scene_depth 的链位
重裁、多层 clipmap、泡沫 / whitecap、扶正力矩。
