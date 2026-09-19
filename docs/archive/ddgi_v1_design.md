# R88 DDGI v1 设计定稿

**轮次**：R88 · 路线图 v5 第三梯队「全局光照」1/2（R88 DDGI v1 → R89 DDGI v2）
**定稿日期**：2026-09-17
**状态**：已落地（R88 收官轮）——语义权威见 ARCHITECTURE §4.44 / §8.57 与
`iris_render::ddgi` 模块文档，本文件按下方「权威关系」转为历史决策记录。
**权威关系**：本文件是 R88 的**开工前设计定稿**；实现落地后语义权威仍是
`ARCHITECTURE.md`（新增 §4.44 与 §8.57）与 `iris_render::ddgi` 模块文档，
本文件转为历史决策记录（偏离声明与裁定依据留档）。

---

## 1 范围与目标

**一句话范围**：DDGI v1 = **规则探针网格**（纯资源态，不落实体）+ **CPU 光线
追踪的时间片滚动重照明**（复用 R52a 的 BVH 追踪器）+ **深度感知混合**
（chebyshev 单侧上界权重），复用 R83 已就位的三阶 SH 探针表与 binding 18/19
消费路径，只新增 binding 20 的深度表与着色器 mode = 2 分支。

**闭合的既有缺口**：`ARCHITECTURE.md` §8.52① —— R83 的径向权重不感知遮挡，
「隔着薄墙的两个探针在墙两侧互相串味」，影响半径是唯一隔离手段。本轮以
逐探针方向深度 + chebyshev 权重消灭该串味。附带缓解 §8.52⑩（探针布局是
作者责任）——规则网格自动生成布局。

**选 DDGI 而非 SSGI 的理由**（路线图 v5 原定，复述留档）：无屏幕空间假影、
不依赖步进基础设施、网格代数更新天然 `to_bits` 可测。

**MVP 边界**：opaque 间接漫反射；无屏幕空间分量；无 GPU 光线查询；1 次反弹；
单色天空。R89 才做与光照贴图 / 反射探针 / SSR 的合成语义修订与编辑器放置入口。

---

## 2 地基与复用清单（一律复用不重写）

| 复用面 | 位置 | 用途 |
|---|---|---|
| `BakeParams` | `bake.rs:612`（校验 `:645`，`bounces ≤ 1` 断言 `:657`） | 重照明的光照输入口径 |
| `build_bvh` / `raycast_bvh` | `bake.rs:234` / `:450`（`BvhHit { t, normal, triangle, barycentric }` `:216`） | 探针射线追踪 |
| `Bvh` | `bake.rs:183`（`Clone + Debug + Default`） | 可存进 World 资源（`Resource: 'static + Send + Sync`，`resource.rs:14`） |
| `direct_irradiance` | `bake.rs:762`（`allow_soft = false` 走硬影单射线） | 反弹命中点的直接光 |
| `collect_bake_triangles` | `bake.rs:830`（纯 `&World`、无内部缓存） | 几何收集 + 默认 bounds 派生 |
| `BAKE_SHADOW_BIAS` | `bake.rs` | 射线原点偏移（深度须加回，见 §5.3） |
| `sh_basis` / `sh_eval` / `sh_dc` | `irradiance.rs:1994` / `:2016` / `:2030` | SH 基与求值（CPU 侧） |
| `cube_face_direction` / `cube_texel_solid_angle` | `irradiance.rs:2040` / `:2063` | 面序与精确固体角权 |
| `probe_sh_radiance` | `irradiance.rs:2082` | **辐射率口径的单一事实源**（本轮改薄壳，见 §5.3） |
| `project_probe_sh` / `sh_lambert_convolve` / `probe_sh_coefficients` | `irradiance.rs:2120` / `:2157` / `:2169` | 投影 + E/π 直读卷积（重照明器的同构范式与等价锚点） |
| `probe_influence_weight` / `derive_probe_radii` / `sh_blend_l0` | `irradiance.rs:2184` / `:2205` / `:2263` | 径向窗、半径派生、mode 0 载荷口径 |
| `ShProbeRaw`（160 B） | `irradiance.rs:2324`（`:2316` 的「WGSL 无动态 swizzle」偏离声明 = 本轮深度表选标量数组的依据） | GPU 上传布局，**零改动** |
| 常量 | `MAX_IRRADIANCE_PROBES = 256`:240、`PROBE_AXES`:244、`SH_CUBE_GRID = 32`:1930、`SH_BLEND_EPS`:1971、`PROBE_RADIUS_NEIGHBOR_FACTOR = 1.5`:1979 | 容量 / 面序 / 混合下限 / 半径因子 |
| `GiShParamsRaw`（16 B） | `renderer.rs:140` | x = mode、y = 探针数、**z/w 保留 ⇒ 本轮启用 z** |
| `GiShGpu` 真/哑双路 + `bind_real` | `renderer.rs:1023` | binding 20 照此扩 |
| `probe_globals_resources` / `upload_gi_probes` | `renderer.rs:3971` / `:4100` | 绑定裁决与上传（后者是「每帧重传」先例，本轮偏离，见 §7.3） |
| gi_on 裁决段 / 上传调用点 | `renderer.rs:6026-6032` / `:6180` | 扩为三态 |
| `FrameStats` | `renderer.rs:1672-1733`（新字段照 `ao_on:1726` / `meshlet_surviving:1732`） | `ddgi_updated` 观测 |
| 惰性 GPU 资源先例 | `AoGpu` `renderer.rs:962`、`OccGpu:1040` | 768 KiB 深度表惰性创建 |
| 组 0 layout 集中声明 | `pipeline.rs:232`（条目 `:250-487`；**binding 0..=19 已满，下一空闲 = 20**；片元只读 storage 已占 3/4 `:459-465`） | 新增 binding 20 条目 |
| WGSL GI 段 | `standard.wgsl:230-251`（声明）、`:919`/`:947`/`:959`/`:974`（basis/eval/weight/irradiance）、**`:1107-1121`（mode 均匀门，本轮加第三支）**、`:1175`（`ibl_env`，irradiance 被 `env_params.y` 缩放）、`:1224`（`globals.ambient` 常量补光项，**不动**） | 消费路径 |
| 插件范式 | `plugin.rs:427` `DayNightPlugin` / `:474` `DefaultPlugins`（`:485` 注册序）、`iris-engine/src/daynight.rs`（系统体模块）、`lib.rs:40-80`（两级 prelude） | `DdgiPlugin` 落点 |

---

## 3 设计决策定稿

### 3.1 用户裁定（2026-09-17，用户为项目唯一决策人）

| # | 议题 | 裁定 | 落地 |
|---|---|---|---|
| 1 | 深度感知混合的数据形态与每探针射线预算 | **cube 面两阶矩 + chebyshev，`rays_per_face = 16`**（质量档，非默认推荐的 8） | §5.3 / §6 / §12 |
| 2 | DDGI 表与 R83 烘焙探针集（`IrradianceProbeSet`）的共存 | **互斥，DDGI 优先**（在场 ⇒ 整表替换 + warn 一次；缺席 ⇒ R83 裁决原样） | §7.4 / §10.6 |
| 3 | CPU 重照明的天空色口径 | **默认取 `DirectionalLight.ambient`**（R80 昼夜免费联动）+ `DdgiSettings.sky_color` 可覆盖；ambient 双计作为已知代价入 §8.57 | §9 / §14⑬ |
| 4 | 规则网格点数超 256 容量 | **确定性抽稀**（最小整数 k 使 `⌈nx/k⌉⌈ny/k⌉⌈nz/k⌉ ≤ 256`，`spacing_eff = spacing × k`）+ warn 一次 | §4.3 |

### 3.2 主会话定稿（附理由）

| # | 决策 | 理由 |
|---|---|---|
| 5 | 深度存 **cube-face 参数化**（6×8²）而非八面体图集 | 与 SH 投影**同批射线** ⇒ 零额外光线成本；WGSL 查表 = 主轴 + uv，省掉 oct 编解码的 CPU/WGSL 双侧镜像与看门面（确定性面更小） |
| 6 | 深度表用 **`array<f32>` 标量数组**，不用 `array<vec4>` | WGSL 无动态 swizzle ⇒ vec4 打包要 3 次 select/取值，正是 `ShProbeRaw:2316` 偏离声明记录过的坑 |
| 7 | 存 **(μ, σ²) 且 σ² 在 CPU 用 f64 求差后钳下限**，着色器零减法 | `E[t²] − μ²` 在 f32 里会灾难性抵消；钳 `σ² ≥ (0.1μ)²` 同时消掉「单纹素零方差 ⇒ chebyshev 退化为硬阶跃」 |
| 8 | **脏表门控的滚动**（无脏探针 ⇒ tick 立即 return） | 静止场景 + 恒定光照的稳态 CPU 与上传成本 = 0，符合仓库「缺席 = 零开销」文化；滚动序仍是固定轮转 ⇒ 可断言（不违背「时间片滚动重照明」的规格） |
| 9 | 安装帧 **粗填**（`DDGI_PREFILL_GRID = 2`，24 方向/探针，深度置中性） | 避免前 `⌈n/k⌉` 帧出现「SH = 0 的黑波按网格索引序扫过场景」；3.3 ms 一次性成本换观感 |
| 10 | **增量上传**（只写本帧重照明的 k 个探针段） | 偏离 R83「每帧重传」：深度表 768 KiB 是 SH 表（40 KiB）的 19×，且 DDGI 有 dirty/cursor 变更记账 ⇒ 与 README 既有「实例缓冲跨帧持久、动态实体仅传变化段」同纪律 |
| 11 | mode 2 的 `Σw ≤ ε` 兜底 **回退 `env_irradiance`，不回退最近探针** | 回退最近探针会**绕过深度项** ⇒ 墙后片元吃到墙另一侧探针的辐照 = 正是本轮要消灭的漏光。这是对 R83 语义的显式偏离，须声明 |
| 12 | 几何变化靠 **`geometry_generation` 显式宣告**，不自动侦测 | 自动侦测要每帧全量收集三角形（ms 级）或遍历算指纹；照 `SkySource.generation` / `ensure_environment` 先例。DDGI 的卖点是「光照动态」而非「几何动态」 |
| 13 | 跨模块等价锚点：**`rays_per_face = 32` 时 DDGI 的 SH 表逐位 ≡ `probe_sh_coefficients`** | 把新重照明器与 R83 已验证的投影器钉成同一串位，是最强的正确性锚（也是「`probe_sh_radiance` 改薄壳未漂移」的硬证据） |
| 14 | `DDGI_DEPTH_GRID = 8` 是**编译期常量、不进 settings** | 缓冲定长可算（768 KiB）+ WGSL 侧可 const + 少一个旋钮；`rays_per_face` 才是可调项（只影响 CPU 成本与质量，不影响 GPU 布局），校验其能被 8 整除 |
| 15 | tick 系统落 `iris-engine`（`DdgiPlugin`），注册在 `DayNightPlugin` 之后 | 逻辑全在 CPU 纯函数 ⇒ headless 可单测（不需要 GPU）；昼夜当帧写的方向光当帧被 DDGI 消费 |
| 16 | 半径用解析式 `spacing_eff × 1.5`，不跑 `derive_probe_radii` | 规则网格的最近邻恒为 `spacing_eff` ⇒ O(n²) 无谓；但**必须加单测断言二者逐位一致**（防口径漂移） |
| 17 | hysteresis 做成旋钮但**默认 0（关）** | 默认关 ⇒ 「收敛帧数 = `⌈n/k⌉`」可解析断言；旋钮在（3 行代码）满足 DDGI 原味需求，α = 0 时逐位等于纯替换（单测钉住） |

---

## 4 探针网格生成（纯 CPU，可 headless 单测）

### 4.1 `DdgiSettings`（World 资源；**缺席 = DDGI 关**）

| 字段 | 默认 | 语义与校验 |
|---|---|---|
| `spacing: f32` | `2.0` | 网格间距（米）；须正有限 |
| `bounds: Option<(Vec3, Vec3)>` | `None` | (min, max)；`None` ⇒ 取 `collect_bake_triangles` 的静态几何 AABB 向外扩 1×spacing，**且 Y 下限额外抬 0.5×spacing**（防探针埋地板下）；空几何 ⇒ 网格空 ⇒ 视为缺席 |
| `probes_per_frame: usize` | `2` | 每帧重照明探针数 k（滚动窗口宽度） |
| `rays_per_face: u32` | `16` | SH 投影的每面格数 G（**裁定 1 的质量档**）；允许 8/16/32/64，须能被 `DDGI_DEPTH_GRID` 整除 |
| `hysteresis: f32` | `0.0` | 时域滤波的**旧值权重** α；钳 `[0, 1)`；0 = 关 |
| `sky_color: Option<Vec3>` | `None` | `None` ⇒ 取 `DirectionalLight.ambient`（裁定 3） |
| `bounces: u32` | `1` | 间接反弹次数（0 = 只天空）；受 `BakeParams` 的 `≤ 1` 断言约束 |
| `geometry_generation: u64` | `0` | 宿主递增 = 宣告场景几何变化（⇒ 重建 BVH + 全表标脏） |

### 4.2 格点与索引序

- 点数 `nx = max(1, floor((max.x − min.x) / spacing))`，y/z 同。
- 格点 = `min + (i + 0.5) × spacing_eff`（**格心**，非格点角）。
- 索引序 **x 最快**：`index = ix + nx × (iy + ny × iz)` ⇒ 轮转序在空间上是
  x 行扫描（写进模块文档：滚动刷新的空间推进方向由此决定）。

### 4.3 容量抽稀（裁定 4）

`stride_for_capacity(nx, ny, nz, MAX_IRRADIANCE_PROBES)` = **最小整数 k ≥ 1** 使
`⌈nx/k⌉ · ⌈ny/k⌉ · ⌈nz/k⌉ ≤ 256`；抽点取 `i = 0, k, 2k, …`；
`spacing_eff = spacing × k`；`k > 1` ⇒ `log::warn!` 一次（报 k 与有效间距）。
**整数求解、无浮点搜索** ⇒ 逐位确定；网格仍规则无洞、覆盖范围不变
（对比按序截断会砍掉远端一整块空间 ⇒ 该块 GI 直接回退 env，出现可见空间断界）。

### 4.4 半径与互斥

- 半径 = `spacing_eff × PROBE_RADIUS_NEIGHBOR_FACTOR`（= 1.5）；单测断言与
  `derive_probe_radii` 在该规则点云上**逐位一致**（决策 16）。
- 探针**不落实体**（纯资源态；避免 256 实体的 ECS 开销与 `&mut World` spawn
  限制）。R89 的编辑器放置入口再议实体化。
- 与 `IrradianceProbe` 组件 / `IrradianceProbeSet` **互斥**：DDGI 在场 ⇒ 忽略
  烘焙集 + warn 一次。理由：两份间接漫反射叠加会**双重计入**天空与色溢（物理错误）。

---

## 5 时间片滚动重照明（§七 硬骨头 3 的答卷）

### 5.1 `DdgiGrid`（World 资源；`DdgiPlugin` 在 Startup 插入空网格）

```text
positions: Vec<Vec3>          radii: Vec<f32>
sh: Vec<ShCoeffs>             depth: Vec<[f32; 2]>   // 长 n × 6 × 8²，存 (μ, σ²)
dirty: Vec<bool>              cursor: usize
bvh: Bvh                      bvh_generation: u64
light_fp: u64                 stats { last_updated: u32, … }
```

### 5.2 标脏与轮转

- **光照指纹** `light_fp` = 对 (`light_dir`, `light_color`, `sky_color`, `bounces`)
  的 `to_bits` 做**固定序**位组合；变化 ⇒ 全表标脏（不清空旧值——旧值继续用、
  逐帧刷新 ⇒ 无闪烁，这正是 DDGI 的时域行为）。
- **几何指纹** = `settings.geometry_generation`；变化 ⇒ 重收集三角 +
  `build_bvh` + 全表标脏。
- 轮转纯函数 `advance(cursor, dirty, k, n) -> (updated: Vec<usize>, cursor')`：
  自 cursor 起**环形扫描至多 n 个位置**，取前 k 个 dirty 者；`cursor'` = 最后被
  更新者的下一位；无更新 ⇒ cursor 不动。O(n) 最坏（n ≤ 256，可忽略）。
- 收敛帧数 = `⌈n_dirty / k⌉`（解析可断言）。默认 256/2 = **128 帧 ≈ 2.1 s @60fps**。
- **稳态**：无脏探针 ⇒ tick 立即 return（零重照明、零上传）。

### 5.3 单探针重照明

```text
probe_relight(bvh, position, params, grid) -> (ShCoeffs, Vec<[f64; 2]>)
```

- 单遍循环：6 面（`PROBE_AXES` 序）× `grid²`（行列序固定），每方向调用
  `probe_ray_radiance_depth(bvh, position, dir, params) -> (Vec3, f32)`。
- **辐射率口径逐式复用 `probe_sh_radiance:2082`**：
  - 未命中 ⇒ 辐射率 `sky_color`，深度 = `DDGI_SKY_DEPTH = 1.0e4`（10 km 有限
    哨兵；不用 `f32::INFINITY` —— 会污染矩累加）；
  - 命中 ⇒ `albedo/π × (direct_irradiance(allow_soft = false) + π·sky_color)`，
    深度 = **`hit.t + BAKE_SHADOW_BIAS`**（把原点从偏移后的射线起点归一回探针
    位置，与着色器侧 `d = distance(x, p)` 同原点）；
  - `bounces = 0` 且命中 ⇒ 辐射率 0（与 `probe_sh_radiance` 一致）。
- **实现方式**：把 `probe_sh_radiance` 的函数体搬进新函数、旧函数改为返回 `.0`
  的薄壳。**R83 既有测试必须原值通过** + §11 的等价锚点②逐位相等，二者共同
  构成「逐式同构、未漂移」的硬证据。
- SH 累加：f64 × `cube_texel_solid_angle` 精确固体角权（**不是**等权 × 4π/N），
  出口 `sh_lambert_convolve` ⇒ 存 **E/π 直读**系数（与 binding 18 现布局同口径）。
- **深度矩**：`grid²` 方向按 `row / (grid / DDGI_DEPTH_GRID)` 映射到 `8²` 纹素
  （`grid` 能被 8 整除是参数校验项）；每纹素 f64 定序累加 `Σt`、`Σt²`、计数 ⇒
  出口 **在 CPU 用 f64 求 `σ² = E[t²] − μ²`** 并钳 `σ² ≥ (DDGI_SIGMA_REL × μ)²`
  （`DDGI_SIGMA_REL = 0.1`），存 `(μ, σ²)` 两个 f32。
- **粗填阶段**（`DDGI_PREFILL_GRID = 2`，安装帧同步一次）：只填 SH，**深度矩置
  中性**（`μ = DDGI_SKY_DEPTH`、`σ² = 0` ⇒ 深度项恒 1、不抑制），随后精修帧逐步
  写真深度。理由：粗填 grid 2 < depth grid 8 无法整除映射；且保守方向（粗填期
  可能短暂漏光，`⌈n/k⌉` 帧内被真深度取代）优于黑波。
- **hysteresis**：`sh = old × α + fresh × (1 − α)` 逐系数，深度矩同款；
  **α = 0 时必须逐位等于纯替换**（单测钉住）。

---

## 6 深度感知混合（关键算法）

### 6.1 权重

```text
dir_i = normalize(x − p_i)            d_i = distance(x, p_i)
w_i   = gi_probe_weight(d_i, r_i) × ddgi_depth_weight(d_i, μ_i, σ²_i)

ddgi_depth_weight(dist, mu, sigma2) =
    if (!(dist > mu))  1.0                                    // 无遮挡证据（NaN 落此支）
    else               sigma2 / (sigma2 + (dist − mu)²)        // chebyshev 单侧上界
```

- 径向窗 `gi_probe_weight` = R83 原式 `(1 − (d/r)²)²`（`irradiance.rs:2184` /
  `standard.wgsl:959`），**零改动**。
- chebyshev 是 DDGI 原式：`dist ≤ μ` 时着色点在探针可见范围内 ⇒ 权 1；
  `dist > μ` 时按「真实深度 ≥ dist」的概率上界衰减 ⇒ 墙后权重趋 0。
- NaN 语义：`dist` 为 NaN 时径向窗已给 0（R83 的 `!(d < r)` 约定）⇒ 乘积为 0，
  深度支的 NaN 处理不影响结果（文档写明即可）。

### 6.2 求值与兜底

```text
ddgi_irradiance(world_pos, n) -> vec4<f32>     // xyz = E/π，w = 有效标志
    for i in 0..count:
        w = 径向 × 深度
        if (w > 0) { acc += gi_sh_eval(p_i, n) × w; wsum += w }   // 剪枝同 R83
    if (wsum > GI_BLEND_EPS) return (acc / wsum, 1)
    else                     return (0, 0)                        // 无有效探针
```

调用点（mode 门第三支）：

```wgsl
let ddgi = ddgi_irradiance(in.world_pos, n);
irradiance = select(env_irradiance, ddgi.xyz, in.probe.w > 0.5 && ddgi.w > 0.5);
```

**兜底语义对 R83 的显式偏离（决策 11）**：mode 1 的 `Σw ≤ ε ⇒ 回退最近探针`
在 mode 2 **改为回退 `env_irradiance`**（天空兜底）。理由见决策 11。

### 6.3 深度纹素查表（CPU/WGSL 双侧镜像 + 看门）

```text
ddgi_depth_texel(dir, grid) -> u32
    face = |x|,|y|,|z| 最大者；并列取 PROBE_AXES 序低者   // 与 cube_face_direction 严格互逆
    (u, v) = 另两轴分量 / 主轴分量（符号按面约定）
    row = clamp(floor((v + 1) × 0.5 × grid), 0, grid − 1)；col 同
    texel = face × grid² + row × grid + col
索引 = (probe × 384 + texel) × 2 + moment                  // moment: 0 = μ, 1 = σ²
```

- **CPU 镜像函数 + 源码层看门单测**（照 `wgsl_sh_constants_match_cpu_table` /
  `skybox_globals_prefix_matches_standard_layout` 先例）。
- **往返单测**：逐 `(face, row, col)` 取纹素中心方向 `cube_face_direction(face, u, v)`
  ⇒ `ddgi_depth_texel` 必须回到同一 `(face, row, col)`。这是「CPU 累积格 ↔
  GPU 查表格同格」的硬证据。
- 角分辨率：每面 90° / 8 ⇒ **≈11.25°/纹素**（面心；面角因切向拉伸更宽）。
  nearest-texel 查表，无双线性插值（诚实限制 §14⑥）。

### 6.4 mode 门三支重构（兼容关键）

```wgsl
if (gi_params.params.x < 0.5)       { /* R64 loc14 逐实体载荷 —— 语句逐条不动 */ }
else if (gi_params.params.x < 1.5)  { /* R83 gi_sh_irradiance —— 语句逐条不动 */ }
else                                { /* R88 ddgi_irradiance —— 新增 */ }
```

mode 0 与 mode 1 的语句**逐条不动** ⇒ R83 / R64 既有 GPU 断言原值通过（照 R78
「双路径位等价 + 均匀门」纪律）。

---

## 7 GPU 接线

### 7.1 binding 预算

组 0 的 binding 0..=19 已占满（`pipeline.rs:250-487`），**binding 20 = 深度表**：

```wgsl
@group(0) @binding(20) var<storage, read> gi_depth: array<f32>;
```

binding 19 参数扩位：`x = mode（0/1/2）`、`y = 探针数`、**`z = DDGI_DEPTH_GRID`
（= 8，f32）**、`w = 保留 0`（σ² 已在 CPU 钳过 ⇒ 着色器不需要 `DDGI_SIGMA_REL`，
少一处契约面）。

**不扩 `Globals`** ⇒ `skybox.wgsl` 并行布局陷阱零触碰，看门单测不动。

### 7.2 缓冲

- 定长 `256 × 6 × 8² × 2 × 4 B = 786432 B = 768 KiB`，**惰性创建**（首次 DDGI
  帧；照 `AoGpu` / `OccGpu`）；关闭帧绑 **4 B 全零哑缓冲**。
- SH 表沿用 binding 18 的 `256 × 160 B = 40 KiB` 现缓冲（`ShProbeRaw` 零改动）。

### 7.3 上传策略（偏离声明）

**增量上传**：只写本帧重照明的 k 个探针段——SH 的 `k × 160 B` + 深度的
`k × 3072 B`（按探针索引偏移 `write_buffer`）；网格结构变化 / 首次启用 /
绑定组重建 ⇒ 全量写。

偏离 R83「每帧重传」（`renderer.rs:4100` 文档：探针集是烘焙产物、无 generation
字段可比）的理由：①数据量 19×（40 KiB → 768 KiB）；②DDGI **有** dirty/cursor
变更记账，不需要靠重传规避比对成本；③与 README 既有「实例缓冲跨帧持久：静态
场景零上传，动态实体仅传变化段」同纪律。默认参数下增量 ≈ **6.3 KiB/脏帧**、
稳态 0 B。

### 7.4 三态裁决与观测

- `renderer.rs:6026-6032` 段扩为三态：DDGI 网格非空 ⇒ `mode = 2` + `bind_real` +
  增量上传；否则 R83 现规则（`IrradianceProbeSet.has_sh()` ⇒ 1，否则 0）。
- `probe_globals_resources:3971` 加 binding 20 的真/哑裁决。
- **捕获 / RT 相机恒绑哑（mode 0）**，沿用 R83（§8.52⑦ 同款立场）。
- `FrameStats` 新字段 `ddgi_updated: u32`（本帧重照明探针数，取自
  `DdgiGrid.stats.last_updated`；注释明标「不进任何排序键」）。
- 对照模式 `Renderer::set_ddgi_depth_weight_enabled(bool)`（照
  `set_legacy_probe_parallax` 先例，仅供验收图与归因测试；默认 `true`）——
  漏光抑制有效性断言的必要开关。

---

## 8 插件层与 prelude

- `crates/iris-engine/src/ddgi.rs`：系统体 `tick_ddgi`（Stage::Update）。两阶段：
  `&World` 读 settings / `DirectionalLight` /（generation 变化时）三角 →
  `resource_mut::<DdgiGrid>` 写回；尝试型锁，冲突即 panic（消息含
  "already borrowed"）。
- `DdgiPlugin`（`plugin.rs`）注册在 **`DayNightPlugin`（`:485`）之后** ⇒ 昼夜
  当帧写的方向光当帧被 DDGI 消费；含于 `DefaultPlugins`；`DdgiSettings` 缺席 ⇒
  立即 return（零分配零成本）。
- prelude：`iris_render::prelude`（`lib.rs:129` 起）导出 `DdgiSettings` /
  `DdgiGrid` / 相关常量；`iris_engine::prelude`（`lib.rs:40-80`）导出
  `DdgiPlugin`。**R74 的 prelude 缺口摩擦（§8.26⑧）不要重演。**

---

## 9 运行期光照口径映射（写进模块文档）

| `BakeParams` 字段 | 来源 | 依据 |
|---|---|---|
| `light_dir` | `DirectionalLight.direction` | 同口径（传播方向，`bake.rs:612` 文档已声明对齐） |
| `light_color` | `DirectionalLight.color × intensity` | `light_color` 约定「已含强度」 |
| `sky_color` | `settings.sky_color.unwrap_or(DirectionalLight.ambient)` | 裁定 3；**不乘 `EnvironmentSettings.intensity`** —— `irradiance` 在 `ibl_env`（`standard.wgsl:1175`）已被 `env_params.y` 缩放，预乘会双计 |
| `shadow_softness` | `0`（硬影单射线） | 重照明走 `allow_soft = false` 分支 |
| `sky_samples` | `1`（本路径不消费） | `probe_ray_radiance_depth` 不做半球采样 |
| `bounces` | `settings.bounces`（≤ 1） | `BakeParams` 现断言 |

`globals.ambient` 的常量补光项（`standard.wgsl:1224`）**不动**——它是独立的美术
补光预算；与 `sky_color` 同源导致的天空底色双计入 §14⑬（配平手段：显式覆盖
`sky_color` 或调低 ambient）。

---

## 10 兼容硬约束（红线，逐条硬性）

1. `DdgiSettings` **缺席** ⇒ 渲染器**一个 `write_buffer` 都不发**、binding 20
   绑哑、params 维持 R83 裁决（0/1）⇒ 既有**全部** FNV 基线（R83 mode 0/1、
   R77 GTAO、R78 分箱、R82 粒子 v2 等）**原值通过**、**既有测试零改动全过**。
2. WGSL mode 0/1 分支语句**逐条不动**；R83 的 CPU 镜像纯函数（`sh_blend` /
   `probe_influence_weight` / `sh_eval` / `project_probe_sh`）**零改动**，新增
   函数并列。
3. **不扩 `Globals`**（binding 20 独立）⇒ `skybox.wgsl` 并行布局看门单测不动。
4. 捕获 / RT 相机路径恒 mode 0（沿用 R83）。
5. **零 unsafe**；无 rand、无全局可变状态（不用 `AtomicU64` 全局计数器；
   `DdgiGrid` 是 World 资源）。
6. DDGI 在场 ⇒ 忽略 `IrradianceProbeSet`（互斥 + warn 一次，裁定 2）。
7. **wasm32 兼容**：全 CPU + storage 缓冲，无 compute、无 ray query ⇒
   `cargo clippy --workspace --target wasm32-unknown-unknown -- -D warnings`
   零警告（`RUSTC_WRAPPER=""` 只加 wasm 两条门禁）。
8. 新 GPU 测试文件 `crates/iris-render/tests/ddgi_render.rs` **文件内 `static`
   互斥锁串行化**（照 `ssr_render.rs::GPU_SERIAL`）——R87 首稿漏配遮挡项的纪律
   缺口不要重演；跨世界对照各用**独立渲染器**（句柄值缓存串味，R53/R62 教训）。

---

## 11 确定性论证

| 环节 | 论据 |
|---|---|
| 轮转序 | 纯索引环形扫描（`advance` 纯函数），无哈希迭代序、无 BTree 依赖 |
| 单探针重照明 | 固定面序（`PROBE_AXES`）× 固定行列序 × f64 定序累加（R83 `project_probe_sh` 同构） |
| 深度矩 | 固定子格序 f64 累加；σ² 的求差与钳位在 CPU f64 域完成 |
| BVH | `build_bvh` 的 tie-break 已就位（`bake.rs:359-363`、命中并列 `:490`），同输入两次构建逐位一致 |
| 标脏判定 | 光照 / 几何指纹 = `to_bits` 固定序位组合 ⇒ 「是否变化」逐位可判 |
| hysteresis | 固定 α 的一阶确定性递推 |
| 跨帧累积 | 探针状态跨帧 ⇒ **双会话逐帧逐位断言**（R60/R62b 范式）：48 帧的 CPU 表（SH + 深度矩）`to_bits` 序列 + 像素 FNV 序列双断言 |
| 跨适配器 | 位级不可保（GPU 编译器 fma / 重结合自由），同 §8.51 / §8.52⑤ 边界；CPU 对照轨是镜像纯函数的解析对照，**不是**位级等价声明 |

**两条等价锚点（必做）**：
① `hysteresis = 0` 且单帧 `k = n` ⇒ 表逐位等于「全量重照明一遍」（解析对照）；
② `rays_per_face = 32`（= `SH_CUBE_GRID`）且几何/光照与 R83 烘焙同源 ⇒ DDGI 的
SH 表**逐位等于** `probe_sh_coefficients` 的输出（跨模块锚点，同时证明
`probe_sh_radiance` 改薄壳未漂移）。

---

## 12 成本账（裁定 1 选 G = 16 的代价，诚实记录）

基线：`bake.rs` 模块文档实测（release i7-13620H，1000 三角 BVH）
「1000 点 × 64 样本 + 1 反弹 = 34.3 ms」⇒ ≈ **0.27 µs/ray**；DDGI 每方向
2 条射线（primary + 反弹点的硬影射线）。

| 项 | 数值 |
|---|---|
| 每探针方向数 | `6 × 16² = 1536`（= 3072 rays） |
| 每探针重照明 | **≈ 0.83 ms**（1000 三角 BVH；三角数上升同比上浮） |
| 默认 k = 2 的脏帧成本 | **≈ 1.7 ms/帧** |
| 收敛帧数 | `⌈256/2⌉ = 128 帧 ≈ 2.1 s @60fps` |
| 安装帧粗填 | 24 方向/探针 ⇒ ≈ 0.013 ms/探针，满载 **≈ 3.3 ms 一次性** |
| 显存 | 深度表 **768 KiB**（惰性）+ SH 表 40 KiB（既有） |
| 上传 | 增量 ≈ **6.3 KiB/脏帧**；稳态 **0 B** |
| 稳态（无脏探针） | **CPU 0、上传 0**，只剩着色器侧成本 |

**注意**：昼夜循环（R80）场景是**持续脏表**（光照每帧变）⇒ 1.7 ms/帧是常态
开销，不是峰值。可调档位：`rays_per_face = 8` ⇒ ≈0.21 ms/探针（0.42 ms/帧）；
`probes_per_frame = 1` ⇒ 0.83 ms/帧、收敛 256 帧 ≈ 4.3 s。
着色器侧：全表线性扫描 ≤ 256 探针，但径向 × 深度双剪枝使 `w > 0` 才做 9 项
SH 点积（与 R83 同剪枝面，增量约 10 op/探针：主轴判定 + 2 次 floor + 2 次
storage load + 1 次除法）。

---

## 13 拒绝清单（v1 明确不做）

- 屏幕空间 GI / SSGI、重投影、屏幕域历史累积（hysteresis 是**探针域**一阶递推）
- GPU ray query / compute 重照明（WebGPU 无 RT 扩展；CPU 单线程即够 + 逐位简单）
- 多级反弹（`BakeParams.bounces ≤ 1` 现断言）
- 点光 / 聚光 / emissive 参与重照明（`BakeParams` 无此输入；R89+ 候选）
- `SkySource` HDR 或程序化渐变天空卷积（单色 `sky_color`，与 `bake.rs` 现限制同源）
- 八面体图集（用 cube-face 参数化，与 SH 投影同批射线）
- 探针自适应放置 / relocation / 剔除（深度项自动抑制嵌入探针的贡献，但白占预算）
- R73 数据并行分片重照明（并行改累加序 ⇒ 需另做固定分片归约序论证）
- 与光照贴图 / 反射探针 / SSR 的合成语义修订（**R89**）
- 编辑器放置入口（**R89**）
- 探针持久化 / 序列化（运行期重算，与反射探针「产物只存 GPU」同款心智）
- 逐探针空间加速结构（着色器全表线性扫描 + 双剪枝，同 R83）

---

## 14 诚实限制（§8.57 草案，收官时落 ARCHITECTURE + 模块头）

1. 单色天空，不吃 `SkySource`（渐变 / HDR 全景被平均成常量）。
2. 无点光 / 聚光 / emissive 参与重照明。
3. 1 次反弹上限。
4. 几何变化靠宿主 `geometry_generation` 显式宣告（不自动侦测）。
5. 光照突变后 `⌈n/k⌉` 帧收敛（默认 256/2 = 128 帧 ≈ 2.1 s）。
6. 深度角分辨率 6×8²（≈11.25°/纹素，面角更宽）+ nearest-texel 查表无双线性。
7. σ² 来自每纹素 4 射线（G = 16 时）+ 相对 10% 下限钳 ⇒ 厚度 < σ 的薄墙仍可能漏。
8. 嵌入几何的探针不做 relocation（贡献被深度项抑制，但占预算）。
9. hysteresis 默认关 ⇒ 滚动窗口推进时相邻探针新旧差异可能有轻微空间撕裂。
10. 跨适配器位级不可保（§8.51 / §8.52⑤ 同款边界）。
11. CPU 单线程内联重照明（未用 R73 分片并行）。
12. 与光照贴图 / 反射探针 / SSR 的合成语义未修订（R89）。
13. `sky_color` 默认取 `DirectionalLight.ambient`，而 ambient 同时是
    `standard.wgsl:1224` 常量补光项的来源 ⇒ 同一旋钮两处消费、天空底色会双计
    （裁定 3 的已知代价；配平手段：显式覆盖 `sky_color` 或调低 ambient）。
14. 无探针持久化（每次启用重新粗填 + 滚动精修）。
15. 成本随场景三角数线性上浮（G = 16 ⇒ ≈0.83 ms/探针 @1000 三角基线）。

---

## 15 验收标准

### 15.1 CPU 单测 ≥ 18 项（全解析解 / 逐位；禁「大于 0」式摆设断言）

格点数与格心解析解 · 抽稀 k 的最小性（穷举小例） · 有效间距与半径 ≡
`derive_probe_radii` 逐位 · `advance` 轮转序解析对照（k=1 / k=n / k>n / 无脏 /
全脏） · 收敛帧数 = `⌈n/k⌉` · 粗填非零 + 深度中性 · **跨模块等价锚点②**
（grid=32 ⇒ ≡ `probe_sh_coefficients` 逐位） · **等价锚点①**（α=0 且 k=n ⇒ ≡
全量重照明） · hysteresis α=0 纯替换 / α=1 冻结 / α=0.5 解析中点 · chebyshev
权重解析解（`dist ≤ μ` ⇒ 1；σ²=0 退化被钳；`dist ≫ μ` 单调趋 0） · σ² 下限钳 ·
纹素往返（CPU 累积格 ↔ WGSL 查表格逐一对上） · WGSL 源码层看门 · 光照指纹位组合 ·
generation 失效 ⇒ 全表标脏 + BVH 重建 · 稳态零操作（无脏 ⇒ tick 不改任何字节） ·
双跑 `to_bits`。

### 15.2 GPU 集成 5–6 项（`tests/ddgi_render.rs`，`GPU_SERIAL` 互斥）

1. 缺席逐位不变（既有 FNV 基线原值）。
2. 开 / 关像素差可判别（封闭双室房间：暗室地板 GI > 阈值）。
3. **漏光抑制有效性**（暗室地板 ROI：深度项开 vs 关（`set_ddgi_depth_weight_
   enabled(false)`）的 GI 值比 ≥ 给定倍数；ROI 避开门口混淆源、指标要在分布
   高端可判别——R53/R60 度量学教训）。
4. 滚动收敛（`ddgi_updated` 计数序列 + `⌈n/k⌉` 帧后表 ≡ 全量重照明）。
5. 双会话 48 帧逐帧逐位（FNV 序列 + CPU 表 `to_bits` 序列）。
6. 昼夜联动（`DirectionalLight` 变化 ⇒ SH L0 在 `⌈n/k⌉` 帧内单调跟随）。

**落地偏离（验收轮修正，2026-09-17）——条目 2 / 3 的受试点从「地板」改为
「共享墙的西侧面」**（x = −0.15、法线 −X、z = 4.5）：地板 ROI 量不到漏光。
暗室天花板一旦落在视锥外就不进阴影贴图（阴影 pass 复用主相机的剔除结果）
⇒ 地板被当作开阔地直射照亮，实测直射项占其亮度 **92%**（596/646），把 CPU
侧两位数的线性辐照比压成 **1.012** 的像素比（完全不可判别）。墙面法线与
「指向太阳」的 +X 侧夹角 > 90° ⇒ `NdotL ≤ 0` ⇒ 直射项**结构性为零**（不依赖
阴影贴图的覆盖范围），像素里除常量 ambient 外只剩间接漫反射。配套两处：
①门口梯度（条目 2 的空间结构部分）移到 **CPU 对照轨**沿墙面扫 z 剖面并断言
严格单调衰减——GPU 侧做不了，因为门口正对的墙面点在门洞里（墙在 |z| ≥ 0.75），
像素量到的是透过门洞、被直射饱和的东侧场地；②相机抬高眼点 + 仰视，让天花板
落进视锥 ⇒ 级联拟合覆盖它 ⇒ 暗室真的暗。

### 15.3 快照

`crates/iris-render/examples/ddgi_snapshot.rs` → `docs/ddgi.png`
（2×2：关 | 开 | 深度项关（漏光对照）| 昼夜另一相位；跨运行逐字节一致 + FNV
断言；warmup ≥ `⌈n/k⌉ + 1` 帧并在 example 内断言已收敛）。场景要有可判读的
漏光对照：一墙分隔亮 / 暗两室、门口是唯一连通。**主会话会复跑验逐字节一致并目检。**

### 15.4 release 预算测试

固定小场景（≤ 2000 三角、n ≤ 64 探针）下 `tick` 每帧耗时上限断言（照 criterion /
预算断言先例，REPORT.md 给实测数字与依据）。

### 15.5 文档三件套

`README.md`（特性勾选条目）· `ARCHITECTURE.md`（新增 §4.44 + §8.57，§8.52①
标注闭合）· `CHANGELOG.md` `[Unreleased]`（含全部偏离声明）· `AGENTS.md`
基线数字顺手更新 · 本文件 §状态 改为「已落地，语义权威见 ARCHITECTURE §4.44」。

---

## 16 落点文件清单

| 文件 | 动作 |
|---|---|
| `crates/iris-render/src/ddgi.rs` | **新增**（网格生成 / 时间片状态机 / 重照明 / 深度矩 / 混合 CPU 镜像 / 常量 / 单测） |
| `crates/iris-render/src/lib.rs` | 挂模块 + prelude 导出 |
| `crates/iris-render/src/irradiance.rs` | `probe_sh_radiance:2082` 改薄壳（函数体搬进 `probe_ray_radiance_depth`）；其余**零改动** |
| `crates/iris-render/src/renderer.rs` | `DdgiGpu` 惰性资源、三态裁决（`:6026`）、增量上传、`probe_globals_resources:3971` 扩 binding 20、`FrameStats` 加 `ddgi_updated`、`set_ddgi_depth_weight_enabled` |
| `crates/iris-render/src/pipeline.rs` | 组 0 layout 加 binding 20 条目（`:250-487` 段）+ 哑缓冲 |
| `crates/iris-render/shaders/standard.wgsl` | binding 20 声明、`ddgi_depth_texel` / `ddgi_depth_weight` / `ddgi_irradiance`、mode 门第三支（`:1107-1121`） |
| `crates/iris-engine/src/ddgi.rs` | **新增**（`tick_ddgi` 系统体） |
| `crates/iris-engine/src/plugin.rs` | `DdgiPlugin` + `DefaultPlugins` 注册（`DayNightPlugin` 之后） |
| `crates/iris-engine/src/lib.rs` | 挂模块 + prelude 导出 `DdgiPlugin` |
| `crates/iris-render/tests/ddgi_render.rs` | **新增**（GPU 集成，`GPU_SERIAL`） |
| `crates/iris-render/examples/ddgi_snapshot.rs` | **新增**（快照 → `docs/ddgi.png`） |

---

## 附：需在 REPORT.md / CHANGELOG 声明的偏离

1. **mode 2 的 `Σw ≤ ε` 兜底改为回退 env**（R83 是回退最近探针）——理由：回退
   最近探针绕过深度项 = 把要消灭的漏光从后门放回来（决策 11）。
2. **增量上传偏离 R83「每帧重传」**——理由：数据量 19× + DDGI 有变更记账（§7.3）。
3. **`probe_sh_radiance` 改薄壳**（函数体搬进 `probe_ray_radiance_depth`）——
   理由：辐射率口径单一事实源，避免逐式复制漂移；R83 既有测试原值通过 + 等价
   锚点② 逐位相等双重看门（§5.3）。
4. **深度存 cube-face 参数化而非八面体图集**（DDGI 论文原味是 oct atlas）——
   理由：与 SH 投影同批射线 ⇒ 零额外光线成本，且省掉 oct 编解码的双侧镜像与
   看门面（决策 5）。
5. **`rays_per_face` 默认 16 而非 R83 的 `SH_CUBE_GRID = 32`**——理由：滚动重照明
   的每帧成本约束（§12）；32 仍是允许档位且是等价锚点②的取值。
