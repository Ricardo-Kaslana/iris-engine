# Changelog

本文件记录 Iris 引擎的所有重要变更。格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本遵守 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Added（R52b：烘焙光照 b 阶段——UV2 参数化与图集布局）

写实路线「间接光」三阶段（a 跟踪器/积分器 → b UV2 图集 → c 渲染集成）
的第二阶段（第 52 轮迭代 b；语义规范全文 = `iris-render::bake_uv` 模块
文档 + ARCHITECTURE §4.24 b 小节，诚实清单 §8.30）。纯 CPU、零渲染集成、
确定性；本阶段把 a 阶段的「世界空间采样点列表」接口接到**光照贴图
texel** 上，产出 CPU 侧完整烘焙管线：世界几何 → chart 参数化 → 图集
装箱 → texel 世界位回溯 → 逐 texel 辐照（复用 R52a 积分器）→ 图集预览。

- **chart 参数化（主轴投影）**：`classify_axis` **完备分类**——
  `|n.y| > 0.7`（`FLOOR_NORMAL_THRESHOLD`）→ 水平面投影 XZ；否则
  `|n.x| ≥ |n.z|` → `WallX` 投影 ZY（并列取低轴号，45° 墙角归 X）否则
  `WallZ` 投影 XY；墙面类被丢弃轴分量 ≥ sqrt((1−0.49)/2) ≈ 0.505 ⇒
  **投影面积 ≥ 0.5 × 世界面积**（回溯 2D 重心分母有下界）。chart 粒度 =
  **(实体, 主轴)**（规范草稿「每 (实体, mesh 句柄)」的细分：本引擎每实体
  恰一个 `MeshHandle`，但同实体不同朝向三角形投影平面不同）。chart 内
  UV = 投影坐标 × `texels_per_meter` 的**纯线性**映射（缩放 + 平移）。
- **投影重叠（串光）防护**：同 chart 内两三角投影**正面积重叠**判定
  （顶点严格内点 + 边对严格穿越，边界相触 / 共边判否——quad 两半恒共
  斜边，按 bbox 相交会把每个 quad 撕成两块）+ 按输入序**贪心 +x 偏移**
  到空位（跳到冲突者右缘 + `OVERLAP_GAP_TEXELS` = 2；每轮越过至少一个
  已放置三角形 ⇒ 至多 n 轮终止）；chart 矩形取偏移后 bbox（「放不下」
  不存在）。共边相邻的 texel 归属由「先者得」解决，不是串光。
- **shelf 图集装箱（`pack_atlas`）**：全序 = `(面积降序 u64 精确整数,
  实体位升序, 轴序号升序, 原 chart 序)`；层内左到右、放不下开新层、层高
  = 该层最高 chart；chart 超图集或 shelf 高度耗尽 ⇒ panic（**尺寸不足
  报错语义**）。输出 `LightmapAtlas { size, used_height, entries }`：
  `AtlasEntry`（实体 / mesh 句柄 / 轴 / chart uv_rect / texel 原点与尺寸）
  + **每三角形** `AtlasTriangle`（自身图集 uv_rect + chart 局部三顶点
  ——回溯直接查表）。利用率口径 `utilization()` = Σ chart 面积 ÷
  (图集宽 × **已用高度**)：规则网格（16 个 18×18 chart / 图集 128）解析
  值 **0.75**（单测精确钉住），验收场景 50.4%。
- **texel 世界位回溯（`bake_texels`）**：逐 entry 逐三角光栅化其图集
  bbox，texel 中心 → chart 局部 → 2D 重心（投影仿射 ⇒ 即 3D 重心）→
  世界位；法线 = **三角面法线**（v1 不做顶点法线插值，与分类口径自洽）。
  归属 = 三边函数同号且 ≥ −1e-4（**含边界**）+ **先者得**（entry 序 =
  装箱序、三角序 = 输入序）⇒ 8×8 格点斜边 texel 恒归三角 0（解析计数
  n(n+1)/2 = 36 / n(n−1)/2 = 28 单测钉住）。未覆盖 texel 辐照恒零；
  输出按 **texel 行主序**升序（行 0 = v 0）= R52a 预留的确定性并行切分点。
- **全链组装（`bake::bake_lightmap` → `BakedLightmap { atlas, radiance }`）**：
  收集 → BVH → chart → 装箱 → `AtlasEntry::mesh` 补齐（纯装箱不查世界
  ⇒ None，组装时填）→ 回溯 → 逐 texel `bake_points` → 行主序回填；空
  世界合法（空图集 + 全零辐照）。
- **图集预览导出（`write_lightmap_preview`）**：辐照 → `aces_tonemap`
  （Narkowicz 系数 2.51/0.03/2.43/0.59/0.14，与 `shaders/blit.wgsl`
  **同款公式**）→ sRGB 编码（blit 的 sRGB 编码由硬件目标格式完成，CPU
  预览显式同款）→ **PPM P6**；验收 `examples/bake_preview.rs`（室内盒：
  10×10 地板 + 两墙 + 0.6×2×0.6 遮挡柱 + 侧照光，8 texel/m、图集 256²）
  → `docs/bake_preview.png`：headless 断言柱影带 texel = 开阔地板的
  **23%**（< 70% 界）、墙根 AO texel = 地板中心的 **21%**；目检 = 影带
  清晰 + 贴墙根渐暗 + 两墙一亮一暗 + 柱成对 chart 并排偏移 + 间隙恒黑。
- **性能实测（release，i7-13620H，2026-09-08）**：验收场景 11890 覆盖
  texel × 64 半球样本 + 1 反弹 + 0.08 软影全链 **212 ms** ⇒ 仍单线程
  （切分点不变：`bake_points` 的 `points` 分段）。
- **测试**（bake_uv.rs 10 项 + bake.rs 1 项，全解析对照 / 逐位）：分类
  六轴向 + 45° + 阈值两侧 + 256 哈希方向被丢弃轴下界扫描；UV 线性解析值
  （Floor / WallZ 两轴）；双层重叠偏移解析位移（+3 texel）+ 共边 quad
  不偏移 + 偏移后两两无正面积重叠；装箱不相交 + 等面积实体位升序 + shelf
  结构（7/7/2 层）+ 利用率 0.75 精确 + mesh 恒 None；尺寸不足两 panic 支
  （chart 超图集 / shelf 耗尽）；回溯 8×8 格点世界位 / pad 与角落不覆盖 /
  斜边 36/28 计数 / 重心自洽复原；共享边 texel 先者得四格钉死；ACES
  f(0) = 0 / f(1) = 2.54/3.16 / f(100) clamp 1 / 逐通道 / 单调；PPM P6
  头 + 黑 / 白 / 232 / 206 字节；全链两次烘焙逐位（atlas 全字段 +
  radiance `to_bits`）+ mesh 补齐 + 覆盖/零值并存 + 阴影对比。测试总数
  debug 870 → **881**、release 868 → **879**（iris-render lib 259 → 270 /
  255 → 266）。

**与规范的偏离**：①chart 粒度 = (实体, 主轴)（规范草稿「每 (实体, mesh
句柄) 一个 chart」——同实体不同主轴三角形投影平面不同，必须拆）；
②重叠判定 = **正面积相交**而非规范字面「投影矩形重叠」（bbox 判定会把
每个 quad 撕成两块、面积翻倍；共边相邻的归属由先者得解决）；③利用率
口径 = Σ chart ÷ (图集宽 × 已用高度)（规范只说「> 50%」未定分母；末层
以下空白是调用方 `atlas_size` 的选择而非装箱浪费）；④预览在 ACES 之后
加 sRGB 编码（blit 输出线性色、sRGB 由硬件目标格式完成，CPU 预览不做则
图偏暗、目检失真）；⑤`BakeUvParams` 默认 `texels_per_meter = 4.0`（规范
只给 `atlas_size` 默认 512）。

### Added（R52a：烘焙光照 a 阶段——CPU 光线跟踪核心 + 辐照度烘焙器）

写实路线「间接光」三阶段（a 跟踪器/积分器 → b UV2 参数化与图集 →
c 渲染集成）的第一阶段（第 52 轮迭代 a；语义规范全文 =
`iris-render::bake` 模块文档 + ARCHITECTURE §4.24，诚实清单 §8.29）。
纯 CPU、零渲染集成、确定性；本阶段输出接口 = **世界空间采样点列表**
（`bake_points`），texel 网格化刻意留给 b 阶段——先把最大风险项
（跟踪器与积分器的正确性 / 性能 / 确定性）钉死。

- **BVH 跟踪器**：`BakeTriangle`（世界空间三角 + 单位法线 + **反照率** +
  来源实体）；`build_bvh` 中值分割（叶容量 4）——**构建确定性** = 分割轴
  取质心包围盒最长轴（并列取低轴号）+ 子区间按 `(质心沿轴分量 total_cmp,
  原三角序号)` **全序**排序 ⇒ 同输入两次构建的节点数组与三角重排逐位一致
  （**构建序即结果的一部分**：`BvhHit::triangle` 是重排后下标）；
  `raycast_bvh` 显式栈遍历（近子先出、命中后收紧上界）+ AABB slab 前进
  测试（方向零分量显式按平行板判定——`0 × inf = NaN` 会被 min/max 静默
  吞掉，症状是「恰落在盒面上的射线被误判不相交」）+ Möller–Trumbore
  双面求交（`ray_triangle_intersection` 公开纯函数，暴力对照测试用它）；
  **射线并列** = `t` 逐位相等取内部索引小者（严格 `<` 才替换 + 等值比
  索引），内部索引由构建序唯一决定 ⇒ 命中不依赖遍历次序；`dir` 须单位
  （入口守卫断言、**不静默归一化**——再归一化会插入 1 ULP 方向扰动，
  破坏与暴力扫描的逐位可比性）；自交保护 = 原点沿法线偏置 2 mm +
  `t > 1e-4` 下限。
- **无 RNG 低差异采样**：`radical_inverse_2`（van der Corput，纯位运算 +
  半桶偏移，与调用次序无关）配 Hammersley `(u1 = 根数反序,
  u2 = (i+0.5)/n)`；`cosine_hemisphere` = Malley 投影（u1 给方位角、u2 给
  圆盘半径 ⇒ cos²θ 严格分层，pdf = cosθ/π，构造保证恒在闭半球内）；
  `uniform_cone` = 球冠 cosθ 线性分层（软影锥），`cos_theta ≥ 1` 退化逐位
  返回轴本身；正交基辅助轴规则固定（`|n.x| > 0.9` 用 +Y 否则 +X）⇒
  同法线同基。
- **辐照度积分器**：`BakeParams`（光传播方向 / 光色 / 天空色 / 软影锥角 /
  半球样本数 / 反弹数 0|1）+ `bake_sample` / `bake_points` →
  `BakeSample { direct, sky, bounce, occlusion }`。①直接光 =
  `max(N·L,0) × light_color`，硬影单射线 / 软影 8 方向锥取**未遮挡比例**
  （锥只改可见性、余弦项仍取光心 ⇒ 全亮区与硬影**逐位同值**、半影 =
  k/8 档离散中间值）；②天空 / AO = 余弦加权半球，pdf = cosθ/π 的无偏
  估计量 `(π/n)ΣL` ⇒ **开阔均匀天空解析值恰为 π × sky_color 且与法线
  朝向无关**，AO = 被挡样本比例；③一次反弹 = 即时辐射度（命中点出射
  `albedo/π × (其直接光 + π × sky)`，π 约掉后每样本贡献
  `albedo × (E_direct + π·E_sky)/n`）⇒ **色溢解析值 =
  albedo ⊙ (E_direct + π·E_sky)**；反弹命中法线翻向射线来向（双面）、
  二次阴影走硬影单射线。
- **世界封装**：`collect_bake_triangles(&World)`——静态 only（无
  `RigidBody` = 组件缺席语义，或 `Collider::is_static` 且非 Kinematic）；
  蒙皮 / `Visibility::HIDDEN` / 悬垂网格句柄排除；只认 `MeshHandle`
  （`LodMesh`/`LodMesh3` 几何含 R51 地形 chunk 暂不参与，b 阶段接最高
  细节级）；顶点走 `GlobalTransform`、法线走法线矩阵 `(M⁻¹)ᵀ`（与
  `InstanceRaw` 给 GPU 的同式）、三角法线 = 三顶点平均且与几何法线反号
  时翻转（负缩放镜像实例的正面朝向由环绕方向决定）；反照率 = 材质
  `base_color`（无材质 / 悬垂句柄 / 无 `MaterialStore` = 白）。
- **性能实测（阶段验收数字，release / i7-13620H / 2026-09-08）**：
  1000 三角 BVH 构建 **217 µs**、1000 采样点 × 64 sky 样本 + 1 反弹 +
  0.2 软影 = **34.3 ms**（criterion `bake_bvh_build_1k_tris` /
  `bake_irradiance_1k_points`；`tests/budgets.rs` 烟雾报警 200 ms =
  均值 × 约 5）⇒ **单线程够用，a 阶段不引入线程池**；b/c 阶段 texel 量
  放大越界时唯一切分点 = `bake_points` 的 `points` 分段（逐点独立、
  按序拼接保序 ⇒ 确定性不破）。
- **测试**（bake.rs 22 项 + budgets 1 项，全解析对照）：MT 解析
  t/重心双向复原、剔除五支（出界 / 反向 / 超 max_t / 贴面自交 / 平行
  退化）、最近命中 + 共面并列取小索引 + 撤遮挡打远者、**512 三角散落
  场景 64 射线 BVH == 暴力扫描逐位**、2048 三角两次构建 + 重排序列 +
  64 射线逐位、中值分割节点数钉死（4/5/8/9 → 1/3/3/5）、退化三角 /
  非单位方向 / bounces>1 三支 should_panic；半球恒闭半球 + E[ω]=(2/3)n
  （Var=1/18 ⇒ 3σ 容差）、球冠 E[cosθ]=(1+cos_apex)/2 + 退化锥逐位返轴、
  采样逆序重采逐位 + 换 count 换点集；开放平面 direct 逐位 = 光色×NdotL
  （倾斜 60° 恰半）+ sky = π×sky（倾斜不变）+ 背光解析零、盒影内 direct
  解析零 + AO 开区间、**半空间 AO 恰 0/1**（面朝地 64/64 全挡 vs 面朝天
  逐位 0）、色溢 bounce == albedo⊙(1+π·0.5)（±1e-4，G/B 逐位相等）+
  bounces 0 逐位零且其余分量不变、软影本影 0 / 半影 k/8 / 全亮区与硬影
  逐位同、24 点两次烘焙（各自重建 BVH）全字段逐位；世界收集静态过滤
  七支（装饰 / 动态 / 运动学 / 静态碰撞体 / 隐藏 / 蒙皮 / 仅 LodMesh3）+
  旋转 90° 与非均匀缩放法线矩阵解析值 + 镜像翻转定则 + 反照率三缺省
  路径 + 无 MeshStore 资源空返回。
- **偏离声明**：①规范 `BakeTriangle` 未含反照率——**增 `albedo` 字段**，
  理由：规范自己点名的「红地板染白墙」色溢验收必须有表面反照率，否则
  反弹辐射恒为白、无法验证；只是数据字段，不动任何签名语义。②软影样本
  数规范未给字段——取模块常量 `SHADOW_SOFT_SAMPLES = 8` 而非扩
  `BakeParams`（软影宽度由 `shadow_softness` 决定，样本数只影响噪声）。
  ③`raycast_bvh` 的 `dir` 从「内部归一化」改为「单位守卫断言」——
  归一化会插入 1 ULP 方向扰动，使 BVH 与暴力对照失去逐位可比性
  （实测差 2 ULP），守卫口径与 `ray_triangle_intersection` 一致。
  ④测试计数：debug 848 → **870**（+22）、release 845 → **868**
  （+22 单测 +1 预算）。

### Added（R51：Heightmap 地形——分块 3 级 LOD 渲染 + 碰撞拼块 + 导航 / 角色零扩展集成）

写实路线「复杂地形」的正式解（第 51 轮迭代；语义规范全文 =
`iris-render/src/terrain.rs` 模块文档 + ARCHITECTURE §4.23 新节 /
§3.8、§3.9 集成说明，设计与偏离理由见提交 body）。范围按决策执行：
渲染走标准管线**零新 shader**；碰撞 = 自动生成粗 AABB 拼块静态体
（复用既有物理 / nav 管线**零 API 扩展**）；地形高度查询纯函数供游戏
调用。

- **数据与生成**：`Heightmap{width, depth, cell_size, heights}` 行主序
  顶点高度场；`generate_hills` 三组方向正弦叠加（波长 48/24/12 m、
  权重 1:0.5:0.25，方向 / 相位由 seed 经 splitmix64 派生——**无 RNG
  无依赖**，同 seed 逐位一致 `to_bits` 锁定；值域闭区间
  `[base−amplitude, base+amplitude]` 与「单 cell 抬升 ≤
  amplitude×0.112」的方向导数界写入文档并测试断言）。查询：
  `height_at` 双线性（域外 / NaN `None`，格点处逐位等于数组元素）、
  `normal_at` 中心差分（边界单边，平面场解析对照）；`Terrain` 组件
  （根实体）持高度场 + `origin` + chunk / 拼块清单，
  `height_at_world` 纯函数供游戏贴地（投射物 / 放置物 / 音效）。
- **分块 + 3 级距离 LOD**：`chunk_cells`（默认 32）切块，每块预烘焙
  三级网格（LOD0 全分辨率 / LOD1 抽 ½ / LOD2 抽 ¼，n=32 实测
  1089/353/177 顶点、2048/576/224 三角形，逐级递减断言 + n=8 手推
  计数精确锁定）。chunk 实体挂 **`LodMesh3`**（`LodMesh` 三级一般化：
  `levels[3] + switch_distances[2]`），渲染**收集期**按「chunk 原点到
  相机 eye 欧氏距离」选级（与 R30 同落点 / 同边界归近侧 / 同无滞回；
  `resolve_mesh_handle` 优先级 LodMesh3 > LodMesh > MeshHandle，蒙皮
  路径不参与 = 地形 chunk 恒非蒙皮）。选中句柄决定合批键 / 剔除包围
  体 / `FrameStats::triangles_drawn`——GPU 测试对混合级（LOD0+LOD1+
  LOD2 各一）精确断言三角形总和。
- **无 skirt 的接缝不变量（对规范的强化）**：每级 LOD 的 chunk 边界线
  恒全分辨率采样、内部抽行抽列 ⇒ 任意两级相邻块的共享边折线顶点集
  一致；顶点 y 直接烘焙世界高度（实体 y = 0 ⇒ 无平移舍入）、法线 /
  UV 由全局格点索引推导 ⇒ **逐位一致**（cell 为 2 的幂时 x/z 同样
  逐位，否则 ≤ 1 ULP 亚毫米且不涉裂缝垂直轴——单测对全部 9 种
  LOD 组合断言）。行带间「拉链」条带三角化统一处理全采样边界行与
  抽行内部行（朝向按几何法线 y 现场裁决，退化序保留 = 确定性规则）。
  该不变量吸收规范的「相邻 LOD 差 ≤ 1」条件（更强，见偏离 ①）。
- **碰撞 = 自动粗 AABB 拼块（零 API 扩展）**：每 `collider_stride²`
  cell 一个 `Collider::static_box`（顶面 = 块内最高角点、底 = 全场
  最低 − `COLLIDER_DEPTH_MARGIN` 0.25 ⇒ 平地不退化零厚度、立面有
  高度）；只有 Collider 无 RigidBody = 静态体，无网格 / 材质句柄 ⇒
  天然不进渲染收集（**偏离规范字面**：未挂 `Visibility`，见偏离 ②）。
  集成证据：角色 `move_and_slide` 迈 0.28 台阶 / 撞 0.9 陡差（既有
  五步管线零改动）；`terrain_statics` 喂 `NavGrid::build` 翻丘——
  航点贴盒顶 ±1e-3、相邻高差 ≤ step；**衔接域实测教训入文档**：
  nav 边界须外扩地形域（R49 雕刻 BFS 种子 = 未被盒覆盖的隐式地面格，
  全覆盖无种子 ⇒ 整图不可走）。规模口径：拼块数 = cell 数 / stride²，
  64×64 stride 1 = 4096 盒为线性扫掠典型上限。
- **材质**：`grass_textures` 程序化草地三件套（周期值噪声 → 基色
  sRGB / 细节法线 / ORM，同输入逐位、金属度恒 0、法线 z ≥ 0.5，
  口径照 `brick_normal_orm`）；UV = 世界 XZ ÷ `uv_tile`（Repeat 寻址
  跨 chunk 平铺无缝，接缝 uv 逐位一致随不变量测试锁定）。
- **兼容**：纯增量——新组件不注册时收集查询走 `Option` 缺席语义
  （未注册 = 空，零开销），既有渲染 / 物理 / nav 测试**零改动全过**
  （`world_without_terrain_renders_unchanged` 守护旧语义基线）。
- **测试与验收**：新增 16 项（terrain.rs 13 + terrain_render.rs 3）：
  生成器逐位确定性 / 值域 / 高差上界、双线性与差分解析对照、n=8
  精确拓扑、9 组合接缝逐位、build 铺世界逐盒、角色台阶双向、NavGrid
  翻丘、GPU 混合级 triangles 精确 + 贴坡 ±2 cm + 旧语义回归。总数
  debug **832 → 848**、release **829 → 845**。视觉验收
  `examples/terrain_snapshot.rs` → `docs/terrain.png`（左 = 第三人称
  贴坡近景（LOD0/1 混合）、右 = 高空拉远全 LOD2；triangles_drawn
  6112 → 1760 = LOD 生效的统计证据；headless 断言脚底贴合
  `height_at_world` ≤ 0.1 m 且终态 grounded）。
- **诚实限制（v1，§8.28）**：碰撞是台阶不是光滑坡面；无 skirt（防水密
  靠接缝不变量，掠射角 T 型接点有轻微法线插值差）；无滞回 / 无地形
  流式装卸 / 无坡度材质混合；LOD 级数固定 3；拼块线性扫掠无空间索引。

### Changed（R50：角速度物理 + 旋转感知胶囊 + SwingJoint + 真人形 ragdoll v2）

写实死亡表现的最后一块物理拼图（第 50 轮迭代；语义规范全文 =
`iris-core::physics` / `iris-core::ragdoll` 模块文档重写版 + ARCHITECTURE
§3.8 更新，设计与偏离理由见提交 body）。范围明确四块：①角速度积分
②旋转感知胶囊 ③SwingJoint 摆动限位 ④ragdoll v2 胶囊肢人形；**Box 保持
世界轴对齐 AABB**（OBB/SAT 留后续轮，诚实清单写明「旋转体用胶囊」）。

- **① 角速度积分**：`RigidBody` 增 `angular_velocity`（rad/s 轴角式）与
  `angular_damping`（默认 0 ⇒ 既有字面量机械补齐、行为零变化）。纯函数
  `integrate_rotation`：`ω ×= exp(−λ·dt)` 后四元数微分式
  `q ← normalize(q + ½·(ω,0)q·dt)`——推导与「每步归一化防漂移」理由写入
  函数文档（打磨 VI 的 IK 范数教训提前内化）。**与平移不耦合**（位置积分
  不含 ω 项）且 **`ω == 0` 时整段跳过** ⇒ 既有场景的 `Transform.rotation`
  逐位不被触碰（单测 `zero_angular_velocity_leaves_rotation_bit_identical`
  锁定）。测试：单步对照解析轴角（容差 1e-5，依据 O((ω·dt)³) 残差）、长时
  积分范数稳定 + 累计转角对照离散解析解 `N·2·atan(ω·dt/2)`、角阻尼经管线
  对照 `ω₀·e^(−λt)`（1e-4）、角/平移不耦合逐位、确定性逐位。
- **② 旋转感知胶囊**：`ColliderShape::Capsule` 的轴 = `rotation` 旋转后的
  本地 ±Y。新增旋转感知入口 `detect_pair_rotated` / `ray_capsule_rotated`
  （局部系变换 + 复用竖直解析解 + 恒等快路径）/ `ray_vs_shape_rotated` /
  `segment_hits_shape_rotated` / `shape_sweep_rotated` /
  `resolve_static_push_rotated`（静态障碍快照升级 `StaticObstacle` 三元组）；
  既有 4/5 参入口保留为恒等特化。**线段最近点算法零数学改动**：三个
  capsule 分支的入参从来只是「段的两个端点」，把竖直端点换成旋转后的端点
  即得旋转感知（交替投影对斜段的收敛性论证写入 `CAPSULE_PROJECTION_ROUNDS`）；
  broadphase AABB 改「|rotation·Y|·轴半长 + radius」精确包络。**兼容锁**：
  单测 `identity_rotation_matches_upright_api_bitwise` 对六类 API（检测 9 种
  配对 × 16 组位置、射线三入口、推离、扫掠、角色控制器）逐位断言恒等旋转
  = 既有 API ⇒ R31 胶囊测试零改动全过。世界封装（`resolve_against_statics` /
  `sphere_sweep_statics` / `move_and_slide` / `raycast` / foot_ik 落地射线）
  全部改走旋转感知路径（倒地的斜置胶囊尸体照实阻挡 / 承托）。
- **③ SwingJoint 摆动限位**：`{ target, anchor_a, anchor_b, rest_offset,
  swing_limit_radians, stiffness, damping }`——本地锚随各自实体旋转；
  `rest_offset`（**父端本地系**的「b 锚 − a 锚」静止偏移）长度给距离 rest、
  方向给**锥轴**（`rot_b·rest_offset.normalize()`，随父端旋转 ⇒ 解剖学口径：
  躯干翻倒时限位跟着翻）。三段求解：锚点距离投影（`DistanceJoint` 同构算式）
  → 锥面拉回（`d̂·â < cos L` 判超限，目标点 = a 锚 + (â·cos L + p̂·sin L)·|d|，
  免 `acos`）→ 子端肢体轴最小弧方向修正（`from_rotation_arc`，无三角超越；
  没有这段倒地的肢体只会保持出生朝向）。**修正全给子端（层级投影）**：
  实测均分让链上约束互相扰动、稳态残差达 rest 的 35% / 限位外 22°（肢体视觉
  脱位），层级投影 + 根先叶后求解序下每轮末尾全部约束精确满足（实测 1e-7 =
  f32 舍入量级）。**两轴速度阻尼**（锚点连线 + 锥面修正方向）：锥面是切向
  位置修正，速度不跟着衰减会变成能量泵（实测被限位夹住的肢体 4 s 后仍以
  5 m/s 沿锥面滑行）。求解在约束轮内「距离 → 摆动 → 目标点」，速度响应只
  在首轮。测试：限位内逐位零修正（幂等）、锚距投影解析对照（静态端 / 双动 /
  弹簧）、锥面拉回解析对照（修正后夹角恰为限位、长度保持、肢体轴对准锥面）、
  锥轴随父端旋转（帧无关性 + 只翻父端即超限）、退化三支（锚点重合 / rest 零长
  pin / 反平行任意垂直向）、两轴阻尼解析对照、单摆管线（rest 保持 + 摆角不超
  限）、despawn 跳过与无关节对照逐位、两级链确定性（位置 + 旋转逐位）。
- **④ ragdoll v2 胶囊肢人形**：`RagdollLimbDef` 增 `half_length`
  （`≤ radius` ⇒ 球肢 = v1 分支，形状 / rest 算式 / 挂载方向与 R37 逐位一致）
  与 `axis`（近端方向 = 本地 +Y 的世界朝向；**不能从 offset 推**：手臂挂在
  躯干侧面时「中心 → 父中心」是水平的，与竖直臂轴正交）；`links` 升级
  `RagdollLink { a, b, swing_limit }`（`None` ⇒ v1 距离关节；`Some` ⇒ 自动接线
  SwingJoint：关节点 = 子端近端帽中心、`anchor_a = 0`、`anchor_b` = 关节点在
  父端本地系、`rest_offset` = 关节点 − 子端中心 ⇒ 直立人形每段都是
  `(0, 轴半长, 0)`；**links 须父先子后**）。新预设 `humanoid_v2`（十肢：躯干
  胶囊 + 头球 + 上臂 / 前臂 / 大腿 / 小腿各二，肩 109° / 肘 120° / 胯 80° /
  膝 126° 限位，脚底贴地、总高 1.64 m）。`release_ragdoll` 注入冲量相关角速度
  `ω = topple_axis × 强度 × RAGDOLL_SPIN_FACTOR`：转轴**优先绕链根肩线**
  （前扑 / 后仰）而非 `Y × 冲量`（侧倒）——实测侧倒时靠地一侧肩窝落到地面
  以下、被钉在肩窝上的手臂转不出来（接触不产生角冲量）、稳态埋进土里
  0.15–0.37 m，绕肩线翻倒同一场景埋深 0.028 m；`RAGDOLL_SPIN_FACTOR = 0.8`
  按「渐近总转角 = ω₀/λ ≈ 92°」取值让躯干恰好躺平。v1 六球预设与
  iris-assault / iris-infiltration 的消费**一字未改**（v2 不切换游戏：切换需
  换胶囊视觉网格 + 改六肢断言，超出本轮范围，汇报说明）。测试：v2 拓扑与
  自动接线逐位对照、骨架相连性（每对肢体真实重叠 + 脚底贴地 + 头顶高度）、
  非竖直 axis 接线（T 姿势 ⇒ 锥轴 = 肢体自身方向）、释放注入同式逐位 +
  `topple_axis` 纯函数四条规则、240 步翻滚塌倒（锚距 ≤ 1e-4、摆角超限
  ≤ 1e-4、不穿地 5e-2、质心塌落、速度收敛、9/9 肢体水平、躯干躺平）、
  v2 全流程确定性（位置 + 旋转逐位）。
- **验收图**：`examples/ragdoll_v2_snapshot.rs` → `docs/ragdoll_v2.png`
  （台阶跌落：左 = 腾空帧（前扑离台、四肢沿肢体方向）、右 = 摊倒帧（躯干
  躺平、四肢摊开、无竖直悬空 / 反折 / 穿地），headless 断言与渲染同一世界
  状态；胶囊视觉 = 圆柱 + 两端帽子实体，刻意不加渲染端新原语）。
- **测试总数**：debug **801 → 832**、release **798 → 829**（新增 physics 22
  项 + ragdoll 9 项 = 31；既有测试零改动全过——含 R31 胶囊全套与六游戏
  headless，v1 兼容的硬验收）。
- **偏离与理由**：① `detect_pair` 保留 4 参签名、旋转感知走新增
  `detect_pair_rotated`——规格自身把「既有物理测试零改动」列为硬验收，改
  签名会强制改 30+ 处调用；② `SwingJoint` 增 `damping` 字段且修正全给子端
  （规格字段表无 damping、未指定 share 规则）——两者都是实测出来的稳定性
  必需品（能量泵 / 35% 残差），论证见上；③ `RagdollLimbDef` 增 `axis`
  （规格字段表无）——肢体朝向无法从 offset 推导；④ 翻倒转轴绕肩线而非
  `Y × 冲量`（规格只说「impulse 方向相关 ω」）——侧倒埋臂实测 0.37 m。

### Changed（R49：NavGrid 高度场语义——台阶 / 坡面 / 丘的 3D 导航 v1）

R35 的 2.5D 导航图升级为**带高度语义的地形导航**（第 49 轮迭代；语义
规范全文 = `iris-core::nav` 模块文档重写版 + ARCHITECTURE §3.9 更新，
设计与偏离理由见提交 body）：

- **表面高度解析**：`NavGridParams` 新增 `step_height`（默认 0.3，与
  角色控制器 `CharacterParams` 同值对齐）与 `max_slope_radians`（默认
  45°）。build 逐 cell 中心解析覆盖体顶面：Box 顶恒可站；Sphere /
  竖直 Capsule 上端帽面**法线坡度 ≤ max_slope 才可站**（ρ ≤ r·sin 等价
  判定，先比平方无 NaN）；多体重叠取最高顶面（等值取遍历序先者）；
  顶面在隐式地面（y = 0）之下的埋没体不覆盖（**大埋深球 = 露出地面的
  丘**）；无覆盖 → 隐式地面 0；覆盖不回落地面（多层留白）。
- **blocked = 地面 / 顶面双层 + 可达顶面雕刻**：覆盖格 offered（可站
  顶面，|Δh| ≤ step 边链与地面连通即**雕刻释放**；孤岛高台保持被阻），
  地面格被阻 ⇔ 最近点距离 ≤ `agent_radius` 且侧障高度 > step（矮体 /
  缓丘可迈不设防；最近点判定 / 相切算阻 / 圆角膨胀 / `cell_size ≤
  agent_radius` 经验规则 = R35 原样）。**兼容性硬约束**：全平地场景
  （既有测试与两游戏的世界：立墙顶面离地均 > step）位图与 R35 逐位
  一致、可走格高度恒 0 → 路径与航点全不变——既有 nav 测试零改动全过
  （唯一编译触点：`find_path_is_bitwise_deterministic` 的参数字面量按
  R49 新字段补默认值，断言语义零变化）。
- **寻路**：8 邻域边加 |Δh| ≤ step 门（低→高迈台阶、高→低自然下落）；
  **攀爬惩罚**——上坡边代价 ×(1 + 2·Δh/step)（能不爬就不爬，代价公式
  入文档即规范）；启发仍 octile（惩罚 ⇒ 边代价 ≥ 纯步距 ⇒ 可采纳且
  一致，首弹 goal 即加权图最优——证明写入模块文档）；**航点带 y**（=
  所在 cell 表面高，末航点 = 目标 XZ + 目标 cell 表面高）。
- **平滑 = cell 光栅 DDA + 高度场逐格校验的 3D 视线**：逐格查 blocked，
  并在进入该格的插值 y 上校验 |y − 表面高| ≤ step（cell 内 |y − h| 的
  最大值必在进入 / 离开边界——按 DDA 步进点采样是完备采样，逐位确定、
  不会像均匀大步长那样漏窄高台；≤ step 的贴丘弦放行 = 角色可迈、穿台
  / 悬空被挡下折点保留）；DDA 与 blocked 判定同源的 R35 论证原样成立。
- **测试**：nav.rs 17 → **29 项**（台阶丘逐级攀升 + 顶面释放 + 平滑
  保留中段折点、超 step 单级高台 None 与绕行两支、大埋深球缓丘路径
  贴合解析球面、陡球顶帽孤岛 + 陡坡带坡度过滤 + 绕行不翻越、攀爬
  惩罚锁定两案（平路绕行 vs 直爬，对照组 step_height 10.0 翻转验证）、
  走廊强制爬平台、混合地形两次 build + find_path 位图 / 高度场 / 航点
  逐位一致、表面解析对照解析解（盒顶 / 球面 / 重叠取最高 / 坡度过滤 /
  胶囊上端帽）、`step_height` / `max_slope_radians` 参数断言 panic ×2），
  既有全部测试零改动全过；游戏侧（Infiltration / Assault）`nav_params`
  按新字段补默认值（0.3 / 45°），平地世界行为零变化即兼容证据。
  测试总数 **789 → 801**（debug）/ **786 → 798**（release）。

### Changed（引擎 API 打磨 VI：闭合 Iris Assault 的七条摩擦，§8.26 全清单）

「现象 → 绕行 → 建议修法」的第六轮闭合（模式照打磨 I–V：引擎侧修复 +
游戏侧绕行删除作回归验证；④⑦ 两条以文档 / 工程约定闭合。架构记录见
ARCHITECTURE §4.6 / §4.6.1 / §4.11 / §4.14 / §7.2 / §8.26）：

- **①IK 回写四元数不归一化 → 逐帧平方复合爆炸（引擎真 bug，本轮最重）**：
  `update_foot_ik` / `update_hand_ik` 的局部旋转在**产生处**
  `normalize()`——两骨解走 `Quat::from_rotation_arc` + 父级共轭反推，
  两处都带浮点范数漂移；写回非单位值后，若下帧没有剪辑通道覆写该骨，
  它作为复合链的输入再乘（共轭不再是逆）⇒ 范数逐帧平方复合（R48 实测
  idle 角色 1.9 s 内 386 → 5.8e7 → NaN → `solve_two_bone_ik` 断言
  panic）。归一化不放通用 `Transform` 写路径（热路径 + 剪辑采样值本由
  关键帧插值保证单位），**谁引入非单位四元数谁负责**——IK 是唯一覆写
  「不被剪辑重置的骨旋转」的系统。归一化责任与推导写进 `foot_ik` 模块
  文档（hand 侧引用同源）。测试：
  `foot_ik::tests::foot_ik_quaternion_norm_stable_without_clip_overwrite`
  与 `hand_ik::tests::hand_ik_quaternion_norm_stable_without_clip_overwrite`
  ——剪辑只写髋 / 肩的**平移**通道（腿臂旋转无覆写），600 帧（10 s = 5
  个整循环）逐帧断言分量有限、‖q‖−1 ≤ 1e-3、骨长保持 0.5，并比对同
  相位（相隔 2 个整循环）两帧的关节世界位 ≤ 1e-3（位形口径不比旋转：
  最小弧只约束骨方向，四元数沿骨轴的 twist 分量由写入历史携带、不参与
  任何关节位置，是本 IK 表述的自由度）。两测试在撤掉 `normalize()` 后
  分别于第 14 / 17 帧失败（实测），即永久回归锁。游戏侧：iris-assault
  的 idle 剪辑**删除四条恒值腿旋转通道**（资产重生成），实弹锁在
  `intro_cinematic_plays_and_hands_over_control`（站立即足部 IK 每帧写
  四条腿骨 → 断言单位范数；撤修复即复现 R48 的断言 panic）。
- **②GLB 内存加载入口进 prelude**：`iris-render` prelude 的 `asset` 导出
  补 `spawn_gltf_instance_from_bytes`（R45 pack / mount 路径的刚需入口）。
  游戏侧：`iris-assault` 的 `iris-render` 直依赖与 Cargo.toml 偏离注释
  删除，`plugin.rs` / `textures.rs` / `tests/headless.rs` /
  `examples/gen_assault_assets.rs` 全部回到 `iris_engine::prelude` 一条
  门路面（依赖纪律回归：游戏 crate 全局只剩 glam 非引擎依赖线）。
- **③`GltfBuilder::material_textured`（纹理通道）**：新
  `GltfMaterialTextures { base_color, normal, orm }` 三通道收 CPU 端
  `CpuTexture` 像素，登记时校验（尺寸非零 + `rgba.len() == w×h×4`，中文
  断言）并按**像素内容**去重（同图多材质只嵌一份），build 时 PNG →
  base64 写成 `data:` URI 内嵌 GLB（自包含 ⇒ 内存加载路径可用；加载器
  对 `bufferView` 引用图片仍显式报 Unsupported，未扩面）；ORM 图同下标
  写进 `metallicRoughnessTexture` 与 `occlusionTexture`，走加载器「同
  下标免重打包」快路；无纹理声明时 `images` / `textures` 两键不写 ⇒
  既有产物 JSON 逐字不变。测试：
  `gltf_builder::tests::built_textured_material_roundtrips_through_the_memory_loader`
  ——builder → `spawn_gltf_instance_from_bytes` 读回，三通道像素逐字节
  一致（PNG 无损）、色彩空间按通道标注（baseColor sRGB / 法线 ORM 线性）、
  两材质共享同一 TextureStore 条目。游戏侧：「加载后克隆材质 +
  `with_normal/with_orm`」绕行删除（法线 / ORM 随 GLB 自带），插件层只
  剩敌我 `base_color` 配色覆写；诚实代价——角色 GLB 55 KB → 241 KB。
- **④Vorbis 编码 = 外部管线（明文闭合，引擎不做编码器）**：运行时只需
  解码，编码属资产生产期（做编码器要给 `iris-audio` 加整个外部依赖，
  换的是没人要的「运行时吐出自制音效文件」能力）。标准命令
  `ffmpeg -i in.wav -c:a libvorbis -q 4 in.ogg` 收录 README「环境要求」
  与 ARCHITECTURE §4.14（本机实测 88,278 B WAV → 5,391 B OGG，`ffprobe`
  确认 `vorbis`）；转换结果经 `PackWriter` 进 pack 即闭环。游戏侧占位
  氛围不变（不把二进制音频塞仓库）。
- **⑤`DepthOfFieldSettings::focus_target`（DOF 动态对焦原语）**：资源
  新增 `focus_target: Option<Entity>`（**字段而非新组件**——对焦目标是
  一次性配置，不需要组件存储迭代语义）；新纯函数
  `post::resolve_focus_distance(world, &settings, &view)` 取目标
  `GlobalTransform` 世界位经主相机视图矩阵的 −z（**view-Z**，与
  `focus_distance` 同口径），调用点在 `Renderer::draw` 的 DOF uniform
  上传处（相机已解析、链头第一个全分辨率 pass 之前；DOF 关闭时不解析 =
  缺席零开销）；目标失效 / 无 `GlobalTransform` / `Visibility::HIDDEN`
  / 非有限 → 回退标量；解析结果不回写资源（资源是配置不是状态）。测试：
  `post::tests` 三条（None 回退 / 目标移动跟随 + 偏轴 view-Z 口径 /
  缺席与隐藏回退）+
  `iris-render/tests/post_render.rs::dof_focus_target_overrides_scalar_bitwise`
  （经目标帧 vs 同值标量帧**逐位一致** = 渲染器读取路径的锁，另断言
  隐藏目标回退）。游戏侧：逐帧算相机→敌距离写 `focus_distance` 的绕行
  删除，改 `focus_target` 指向最近活敌的**胸骨**（无活敌回玩家，演出期
  标量对焦喷泉）；headless 采样改走同一纯函数，断言「采样帧过半由焦点
  实体驱动」+ 解析值仍随交战几何变化。
- **⑥`AnimationPlayer::node_entity_by_name`（按名寻骨）**：加载器给
  `GltfNode` 补 `name` 反序列化 ⇒ `GltfInstance::node_names`（与
  `node_entities` 同下标）随 `player()` 进入
  `AnimationPlayer::node_names`，新查询 `node_entity_by_name(name)`
  **重名取先**（向量序确定 ⇒ 结果确定；glTF 不强制名字唯一 ⇒ 唯一性由
  资产侧保证）。下标仍是动画通道与 `AnimationMask` 的寻址键（引擎侧
  通道数据即「节点下标 + 路径」），分工改为**下标服务动画、名服务
  装配**。测试：`gltf::tests::node_names_drive_lookup_by_name`（名字表
  与实体表对齐、按名与按下标命中同一实体、未知名 None）、
  `animation::tests::node_entity_by_name_takes_first_and_tolerates_gaps`
  （重名取先 / 有名无实体 / 无名表恒 None）。游戏侧：`skeleton.rs` 新增
  `names` 模块与 `NAMES` 换算表（+ `bone_names_are_unique_and_complete`
  守护），装配层（腿 / 臂 IK 链、枪身挂点、敌我配色、枪身隐藏、ADS 目标）
  全部改按名寻骨，`bone_entities` / `grip_entity` 下标版辅助删除；资产
  生成器的 `bone()` 按下标声明并**当场断言**「声明顺序 = 骨架表下标」。
- **⑦example 跨 crate 同名冲突（工程约定闭合）**：R48 已改名
  `gen_assault_assets`；命名约定（example 名跨 crate 唯一、游戏 crate 带
  游戏前缀）写入 ARCHITECTURE §7.2——cargo 把所有 example 编到同一
  `target/.../examples/` 目录，同名既互相覆盖产物（cargo 报 future hard
  error）也互相抹掉对方写在仓库根的验收图；引擎侧无修法。

摩擦清单原文与逐条闭合标注见 **ARCHITECTURE §8.26**。


### Added（R48：iris-assault 写实战斗庭院——R42–R47 六轮能力全量实弹的垂直切片）

新 crate `crates/iris-assault`（下游示范游戏 VI，dogfooding VI）：20×14 m
战斗庭院中持刀 + 步枪的玩家与 4 名持刀敌人死斗，全歼胜利 / HP 归零败北。
与往作把单轮新能力压榨到极限不同，本作首要目的是**六轮新能力的接线验收
与摩擦清单**（游戏轮纪律：只绕行不改引擎，摩擦全文见 ARCHITECTURE
§8.26）：

- **纯逻辑核心**（`logic.rs`，只依 glam + iris-core 纯函数，逐位确定性）：
  刀剑三连段状态机（每段 = 前摇 / 55° 扇形判定窗 / 后摇；`sweep_id`
  代次防同次横扫重复命中；第三段击退——线性衰减冲量 + 更长硬直；横扫 /
  后摇期按键预约下一段）、步枪（0.16 s 射速 / 12 发弹匣 / 1.6 s 换弹 +
  手动换弹边沿；弹道 = 引擎 `ray_vs_shape` 静态遮挡 + XZ 线段-圆扫掠，
  被墙挡住的敌人不中弹）、敌人 AI（NavGrid `find_path` 追击 + 前摇转向
  + 窗中点一次性扇形判定 + **重置型**硬直——不叠加）、喷泉球体碰撞元 +
  四角石柱 AABB（NavGrid 绕行价值有折点断言）、移动 = 引擎
  `move_and_slide_statics`（动作减速倍率分层）。19 项单测含扇形越界
  不伤、弹道遮挡解析解（喷泉命中 x ≈ −1.587）、硬直重置、击退位移
  解析（1.2 m）与两遍模拟 `to_bits` 逐位一致。
- **写实材质（R42 载荷）**：`textures` 模块纯函数生成器——刀身拉丝
  划痕 `scratched_metal_normal_orm`（锻面 + 哈希划痕 → 法线 + ORM，
  金属度恒 1）、布料褶皱 `cloth_normal_orm`、血渍 / 弹孔贴花（RGBA
  sRGB）；庭院石板 / 砖墙复用引擎 `brick_normal_orm`；GLB 材质加载后
  克隆 + `with_normal/with_orm` 替换（敌人上身暗红重着色）。
- **DOF / MB（R43 载荷）**：战斗默认开，`DepthOfFieldSettings` 逐帧
  对焦在交战距离（相机→最近活敌，focus_range 4 / max_blur 5）；演出
  期间 `MotionBlurSettings` 覆盖 spline 相机运动。
- **流式音频（R44 载荷）**：pack 内 OGG 经 `OggVorbisStream::from_bytes`
  上音乐总线（0.4 音量 / 1.5 s 淡入 / 循环）+ `set_reverb` 庭院混响
  （v1 只作用于音乐总线——诚实限制）；8 种程序化音效 `play_spatial`
  （挥砍 / 刀命中 / 枪声 / 换弹 / 受击 / 死亡 / 胜负），听者跟随主相机。
- **pack 资产（R45 载荷）**：角色 GLB + 氛围 OGG 打包
  `assets/assault.ipak`（`PackWriter`，CRC 条目表），`AssetMount(Pack)`
  挂载，GLB 经 `spawn_gltf_instance_from_bytes` 内存入口加载（自包含
  约束实弹验证）；pack 缺失时诚实告警并降级文件路径。
- **动画 v4（R46 载荷）**：上下身**双 `AnimationPlayer`** 组合姿势——
  下半身（挂模型根）`BlendSpace` idle/walk/run（参数 = 速度 0..1）+
  上半身（挂空子实体）guard / sword1..3 / aim / reload / flinch +
  `AnimationMask`（节点集 = 上身 14 骨），两层写入集不相交 ⇒ 跑动中
  挥砍 / 瞄准不打断腿；受击 = `AdditiveLayer`（flinch 相对 t=0 的增量
  压幅叠加，不破坏步态）；**手 IK**：程序化 22 节点人形 GLB
  （`GltfBuilder`，骨架下标契约 = `skeleton` 模块单一事实来源），ADS
  时双臂 `HandIk` 吸附枪身握把 / 护木 `IkTarget`（挂点在胸节点子树、
  不在手骨链内——无反馈环），headless 断言右手吸附 ±12 cm；足部 IK
  双腿链 5 角色 × 2。
- **演出时间线（R47 载荷）**：开场 6.5 s——绕庭 Catmull-Rom 相机轨道
  （5 关键帧）+ 两段 ASCII 字幕 + 2.35:1 黑边，`intro_end` 事件触发
  `begin_battle` 交还控制权；胜利 / 败北推近主角 4.5 s 结束演出
  （`OutroLatch` 每局一次、命令队列延迟启动）。老能力：ragdoll 死亡
  （六球人形 + 受击方向轻踢 + 逐肢体挂网格材质）、贴花战痕（血渍 /
  弹孔盒投影）、粒子 FX 池（32 槽四组启动预热、运行期零 spawn、组内
  帧计数轮转）、NavGrid 敌人追击。
- **headless 集成测试**（8 项）：六能力接线静态断言、开场演出端到端
  （字幕 / 黑边 / 事件交权）、ADS 手 IK 吸附量化、自动玩家真实键盘
  注入全歼通关（**6.4 s / 4 杀 / HP 100 / 16 发**，含首杀 ragdoll 六
  肢体 + 敌模型隐藏 + 贴花 + FX 粒子 + DOF 对焦变化断言）、战斗音效
  入混音器、双 Harness 逐位一致、写实材质槽位断言、离屏 smoke。
- **离屏快照**：`examples/assault_snapshot.rs` 自动玩家推进到战斗中段
  定格两帧（挥砍判定窗 + 枪口焰当帧）→ `docs/assault.png`（640×360
  横拼，FrameStats 打印 + 视锥剔除断言）。
- **摩擦清单（一级交付物）**：七条（现象 → 绕行 → 建议修法）记入
  ARCHITECTURE §8.26，头条为**足部 IK 回写四元数不归一化的逐帧平方
  复合爆炸**（引擎真 bug 级，建议引擎轮闭合：两处 IK 回写前
  `normalize()`）。

### Added（R47：演出时间线 Sequencer-lite——相机 spline 轨道 + 事件轨道 + 对话 + 黑边条）

写实动作游戏剧情演出的最小骨架（`iris_engine::timeline` 新模块，
iris-engine 门面层——演出驱动相机与 UI，跨渲染边界；数据纯结构 +
采样纯函数，零 unsafe、零新依赖）：

- **轨道时间线**（`Timeline`）：**相机轨道** `CameraTrack`——
  `(time, eye, target)` 关键帧（≥ 2），眼点与注视点**各过一条
  Catmull-Rom spline** 的锁定注视相机路径（`sample_catmull_rom` /
  `sample_camera` 纯函数：张力固定 0.5、过全部关键帧——u=0/1 处多项式
  f32 求值不精确对消故端点精确早退、段间 C1 连续性数值单测、首尾端点
  **重复端点夹持**（零切向缓入缓出不外推）、单关键帧/空轨道恒定不
  panic、t 越界钳制到 [0, 末关键帧 time]；ease=false 线性；时间→段按
  关键帧 time 非均匀分段、段内归一化参数）。**事件轨道**
  `TimelineEvent { time, name }`——跨过即发，跨界检测**复用从
  `iris-render::animation::collect_events` 中抽出的公共纯函数
  `collect_crossings`**（动画与时间线共享跨界语义单一来源：回绕 /
  multi-fire / 末端不重复与动画逐位一致，collect_events 退化为薄接线
  层、既有测试零改动全过）；命中经 `Events<TimelineEvent>` 产出（插件
  注册、游戏侧不注册即得），Update 产出 → 同帧更晚系统与下一帧全程可读
  → 消费方 `drain`（事件双代契约，§3.10）。**对话轨道**
  `DialogueEntry`——`[time, end_time)` 活动窗口（左闭右开，同刻结束与
  开始不闪帧），多条目活动时声明序后者胜（确定性）。
- **播放器 `TimelinePlayer` = Resource（每世界至多一个）**：并发多条
  时间线 v1 不支持（多个相机/黑边写入方语义互斥，诚实声明），选
  Resource 让「每世界一个」成文档约定 + 托管入口直接可用。字段含
  `looped`（循环回绕重播、事件 multi-fire）与 `previous_time`（跨界
  检测基）；`start_timeline` / `stop_timeline` / `is_playing` 托管
  入口 + `update_timeline` 每帧系统——**Update 阶段独占系统**（动画后、
  物理前；独占理由：对话/黑边实体可能惰性 spawn + 演出独占屏障语义；
  必须早于层级传播与渲染收集，实体相机 GlobalTransform 当帧新鲜）。
- **相机接管与恢复**：start 快照主相机原姿态——**优先 ActiveCamera
  实体局部 Transform（查询序第一个，与渲染器相机解析一致）、回落
  Camera 资源 eye/target**；播放中逐帧覆写（实体模式写位置 + 注视旋转
  `look_at_quat`——局部 -Z 指向 eye→target、与 view_from_global 口径
  一致，前向与 +Y 平行时改以 +Z 为上参考不产 NaN）；stop / 播完快照
  **逐位写回**（f32 拷贝无舍入，演出后无缝回游戏；快照实体 despawn
  静默跳过）。播放中再次 start：先恢复旧快照再快照恢复后姿态（演出
  不嵌套）。**不自动拦截游戏输入**、不暂停相机跟随（游戏侧自理，
  演出期间 Update 后段别改主相机——后写的赢）。
- **对话与黑边 UI**：复用既有 UI 矩形管线，不新增渲染路径——固定
  `Name` 的实体显隐（对话框 `iris/timeline_dialogue`：UiText 写文本 +
  水平居中于下黑边上方；黑边 `iris/timeline_letterbox_top/_bottom`：
  全宽黑色矩形、高度 = 视口高 × `letterbox_ratio`（0.12 ≈ 2.35:1）、
  随视口重排）；三类实体**缺席惰性生成**（按 entity_named 查找不缓存
  句柄）。位图字体仅 ASCII（中文对白 v1 不渲染，诚实限制）。
- **测试 14 项**（catmull 过点精确 / C1 数值连续 / 端点钳制 / 单点
  两点退化；事件接线与排序；对话窗口与取舍；实体 + 资源两种相机接管
  恢复逐位一致（播完 + 中途 stop）；播放中实体相机位置 == 采样值
  ±1e-5 且注视点投影回视图中心；无相机轨道逐位不动；黑边随开关与
  视口；事件可 drain；同 timeline 两次播放逐位一致）；视觉验收
  `examples/timeline_snapshot.rs` → `docs/timeline.png`（12 s 演出三
  时刻拼图：spline 广角→特写推镜、第 3 s `door_open` 事件消费后开门
  （Visibility 翻转示范）、第 5–9 s 对话、全程黑边）。设计见
  ARCHITECTURE §4.22，诚实限制见 §8.25。

### Added（R46：动画 v4——附加动画层 + 骨骼遮罩 + 两骨手部 IK）

第三人称写实角色的动画纵深三件套（边走边瞄准、受击不破坏移动循环、
持枪手位吸附）。既有动画测试零改动全过——`AnimationPlayer` 既有
路径行为不变（新字段全部中性默认：`additive = None`、`mask = None`
与 v3 行为逐位一致）：

- **附加动画层**（`iris-render::animation`）：`AnimationPlayer::additive`
  （`AdditiveLayer { clip, time, speed, weight, looping }`）+
  `play_additive(clip, weight)` / `stop_additive()`——主姿势（active /
  blendspace / crossfade 任一现行模式）算完后叠加「剪辑相对参考帧
  （t = 0 采样值）的增量」：平移 / 缩放 `(v(t) − v(0)) × weight` 加法，
  旋转相对旋转 `q(t) · q(0)⁻¹` 经 `slerp(I, d, weight)` 压幅后**右乘**
  主姿势（增量解释进节点局部系——左乘会随身体朝向漂移，数学推导进
  `additive_delta` 文档）。推进与主时间轴对称（暂停冻结 / 循环取模 /
  非循环钳末端），采样与落账同式（与分帧方式无关）。**诚实边界**：
  只在主姿势已覆写的通道上叠加——以当前 Transform 为基会让增量逐帧
  复合无界漂移（实测捕获：上一帧附加输出被当基值、旋转每帧多转
  45°），未对齐通道跳过、附加剪辑须与主姿势对齐设计。测试：呼吸
  剪辑 weight 0.5 半幅解析精确、旋转增量 yaw135° 解析对照（容差
  2e-3 rad，slerp 复合 f32 精度实测 ~0.00098 rad）、weight = 0 与
  无附加层逐位一致、与 crossfade 并存叠加在后。
- **骨骼遮罩**（`iris-render::animation`）：`AnimationMask { nodes:
  BTreeSet<usize> }` + `set_mask` / `clear_mask`——**覆写过滤器**，
  不参与姿势计算：目标节点不在集合内的覆写任务整条剔除（腿节点
  保持移动循环姿势）。与 BlendSpace / crossfade / 状态机 / 附加层
  全部正交（附加层覆写同样被过滤；`BTreeSet` = 确定性迭代序）；
  `mask = None` 全覆写照旧。测试：遮罩含上身节点 → 腿节点逐位
  不动、`clear_mask` 后恢复覆写。
- **两骨手部 IK**（`iris-render::hand_ik` 新模块）：`HandIk`（肩-肘-手
  链 + `pole` + `target` + `reach_threshold`）+ `IkTarget` 握把标记 +
  `update_hand_ik`。复用足部 `solve_two_bone_ik` 纯函数与两阶段回写
  （`chain_world` 提为 `pub(crate)` 共用）。**与足部 IK 的目标差异**：
  ① 目标 = `IkTarget` 实体 `GlobalTransform` 全局平移（武器握把，
  玩法量）而非射线打地面；目标失效 / 无全局位 / `Visibility::HIDDEN`
  → 本帧放弃；诚实边界：一帧滞后——挂点在手骨子树内会成反馈环
  （装配禁忌写进模块文档）。② 不可达门带**容差带**：超全臂长但在
  `reach_threshold` 内仍解算（拉伸贴握把 = 伸臂取枪的正常表现），
  超带 / 过近才放弃——与足部全域放弃的刻意差异（足部不可达 = 射线
  采到异常几何必错；手部接近全臂长是玩法 authored 正常输入）。
  ③ 无平地门（目标逐帧移动、回写幂等）。双臂装配：每臂一条配置
  实体挂 `(IkRig, HandIk)`（组件存储每实体单槽），两链互不干扰。
  测试：握把解析解精确（手落握把、肘按余弦定理落位、骨长严格
  保持）、超域 / 缺席 / 隐藏三类姿势逐位不动、容差带内拉伸贴握把、
  双臂同时吸附、逐位确定性、debug 链契约断言。
- **编排**：`AnimationPlugin` 注册序扩展为 `update_animations →
  update_foot_ik → update_hand_ik`（手 IK 在足 IK 后：持枪手位是
  比脚落地更高频的视觉焦点，两段 IK 依次作用于同一份本帧局部姿势，
  注册序确定 ⇒ 结果确定）→ PostUpdate 层级传播。

### Added（R45：资产打包与流式——PACK1 单文件格式 + mount 挂载 + 异步加载骨架）

「低占用」卖点的规模化兑现：中型游戏资产体量的载入形态。三个新模块
全部在 iris-engine、纯 std、零 unsafe、零第三方依赖（确定性优先于压缩率）：

- **PACK1 单文件格式**（`pack.rs`）：`PackWriter`（`add` 收集 →
  `write_to` 全量产出）+ `PackReader`（`open` / `read` / `exists` /
  `iter`）。格式＝`"IPAK"` 魔数 + 版本 u32 + 条目数 u32 → 条目表（路径
  长度 + 路径 + 绝对偏移 u64 + 长度 u64 + 校验和 u64）→ 数据区连续无
  填充；条目表按路径**字典序**（BTreeMap），同输入两次打包**逐位同
  输出**。校验和选型＝**CRC-32（IEEE 802.3）以 u64 字段存储**：定位是
  防损坏而非防篡改，纯 Rust 表驱动（const fn 编译期建表）可审计，
  XXH3 需引入实现/依赖不划算；u64 字段宽 + 版本号给升级强校验留位
  （升级只动算法不动布局，旧 reader 拒读高版本而非误读）。**大文件
  语义是结构性保证**：open 只读头 + 条目表（两次有界读取），`read` 对
  目标条目 `seek + read_exact` 后验 CRC；`PackReader` 泛型源（默认
  `File`，`from_source` 注入任意 `Read + Seek`），计数源测试锁定
  「open 读 < 1 KB、读小条目全程 < 1 KB」——12 MB 邻居条目全程不进
  内存。读取错误三态可辨：`NotFound` / `ChecksumMismatch`（含期望与
  实际值）/ 结构 `Corrupt`；路径非法 UTF-8 / 偏移越界 / 条目重叠 /
  重复条目全部 open 期拦截。
- **多源挂载**（`vfs.rs`）：`AssetMount`（可入 World 的 `Resource`，
  **默认不注册**——缺席 = 既有 `asset_path` + `std::fs` 语义零变化）+
  `AssetSource` 三态：`Fs`（目录根拼接，现有语义的目录化形态）、
  `Pack`（PACK1 归档）、`Embedded`（编译期文本表，wasm 友好；v1 仅
  文本类）。**顺序语义**：先挂先命中（同路径多源 = 用挂载顺序做补丁
  覆盖）；源内未命中走下一源；命中但读取失败（CRC 不符 / IO 错误）
  **立即报错不回退**——损坏数据被静默跳过是 bug 温床。wasm 姿态＝
  **运行时降级**而非 cfg 删变体：`Fs` 在 wasm 上读取恒失败（`std::fs`
  为桩），枚举形状跨平台一致、调用方零 cfg 分叉；wasm 布局只挂
  `Embedded`（fetch 网络源在路线图）。
- **异步加载骨架**（`async_loading.rs`）：`AssetLoader::spawn` 起专用
  线程 `iris-asset-loader`（命令通道入队 + 结果信箱出队，照 iris-audio
  流式解码线程模式），**主线程零阻塞契约**——`request` 只入队、
  `poll` 非阻塞 `try_recv`。与音频流「后台错误不可回传」相反，这里
  错误**保真回传**（加载无实时回调约束）；`AssetKind::Scene` 在加载
  线程上做 JSON 结构校验（坏文件在后台报错，主线程拿到的字节保证可
  解析），`RawBytes` 原样透传。wasm 无线程 → `spawn` 自动退化为
  `inline` 同步后端（同 API 同语义，结果按请求序 FIFO；唯一契约差异
  是读取耗时计入 request 调用）；native 线程创建失败同样回退不丢源。
- **场景挂载入口**：`load_scene_from(world, &mount, path)`——场景
  JSON 与子资产（glTF / 贴图 / HDR 天空）全走挂载顺序查找；`load_scene`
  文件路径语义保持逐位不变（两者共用同一加载主体）。配套 iris-render
  新增 `spawn_gltf_instance_from_bytes`（内存字节入口；`GltfLoader` 的
  base_dir 改 `Option`，内存来源对外部 uri 引用显式报错而非落回磁盘
  静默解析——pack 内的 glTF 须自包含 GLB / data URI）。OGG 的 mount
  通路复用既有 `OggVorbisStream::from_bytes`（mount 读字节 → 内存入口，
  iris-audio 零改动）。
- **验收示例**（`examples/pack_demo.rs`，实测数字进 ARCHITECTURE
  §7.3）：场景 JSON + GLB + OGG 三资产打包——磁盘开销 34 639 B →
  34 785 B（**+0.4%**，头 + 条目表 146 B）；随机读 100 次实测 pack
  **≈ 25 µs/次** vs `std::fs::read` 直读 **≈ 47 µs/次**——pack 快约
  2 倍（文件句柄常驻省掉每次 open 系统调用），「海量小资产」场景下
  单文件归档的结构性收益。

新增测试 19 项（`iris-engine` lib 54 → 72：pack 往返 / 字典序确定性 /
损坏三态 / 空 pack / 大文件 IO 记账 +7；mount 顺序查找 / 覆盖 / 损坏
不回退 / read_text / 降级源 +4；加载线程往返 / 非阻塞 / inline 等价 /
pack 接缝 +4；场景 pack 往返 / 默认语义零变化 / Asset 错误 +3；另
`iris-render` glTF 内存入口等价 +1），测试总数 **695 → 714**（debug）/
**694 → 713**（release）。诚实限制清单见 ARCHITECTURE **§8.24**
（PACK1 无压缩 / 无去重 / 无加密、条目整读不分段、挂载无写入与热重载、
Embedded 仅文本、pack 内 glTF 须自包含、异步只覆盖字节读取两类、无
进度 / 取消 / join）；设计与实测细节见 **§7.3**。既有 iris-engine
测试全部原样通过（`load_scene` 行为零变化）。

### Added（R44：OGG/Vorbis 流式解码 + 音乐总线 + 混响——音频 v2）

音频侧补上「体积」这块短板：此前只支持 WAV 全量内存（60 s 立体声 48 kHz
= 22 MB，30 分钟配乐几百 MB），「低占用」卖点在音频侧破功。本轮
`iris-audio` 新增三个模块 + 一个第三方解码依赖（保持**零引擎依赖**纪律）：

- **OGG/Vorbis 解码**（`ogg.rs`）：依赖 **symphonia 0.5**（feature 只开
  `ogg + vorbis`）。选型硬理由＝symphonia 全家族 crate 顶部
  **`#![forbid(unsafe_code)]`**（编译器强制），与本仓库「全库零 unsafe」
  一致（同类解码器 lewton 含大量 unsafe，故不选）；拉取式
  `next_packet` + `decode` 状态机天然匹配「后台线程分块喂混音」；子集依赖树
  最小（无 rustfft / regex）。两条消费路径：`AudioClip::from_ogg` /
  `load_ogg`（整条解成交剪辑，短音效走既有混音路径**零改动**）与
  `OggVorbisStream`（实现 `AudioStream`，分块解码喂流式通道）。Vorbis 的
  f32 平面按帧交错重排，值原样搬运不缩放。
- **流式音乐总线**（`stream.rs`）：`AudioMixer::play_music(stream, volume,
  fade_in, looping)` / `stop_music(fade_out)` / `set_music_volume` /
  `music_is_playing` / `music_handle` / `music_producer_finished` /
  `join_music_producer`。`AudioStream` 契约（布局恒定 + `next_block` 追加
  语义 + `rewind` 循环能力 + 进展保证）；`SampleStream` 让常驻剪辑也走流式
  路径（**流式机制与格式无关**，故确定性测试全部用它）。解码在专用后台线程
  `iris-audio-music` 分块产出，经**有界块队列**（8 × 4096 帧）交混音回调：
  生产者满队列才阻塞 `Condvar`，消费者只做一次 `try_lock`（三态
  `Block / End / Empty`）——与 cpal 回调同一条「实时线程绝不等待」原则。
  **下溢语义**：该缓冲剩余帧不写音乐、游标不推进，下次混音从原处续播
  （听感是卡顿不是爆音，队列深度即抖动预算）。**循环在生产者侧** `rewind`
  重建 demuxer（不走容器 seek，免掉 seek 后解码器状态一致性的易错空间）。
  淡入淡出是**按输出帧推进的线性 f32 斜坡**；单总线单轨（切轨硬拆上一轨，
  v1 无 crossfade）；音乐不空间化。线程创建失败（wasm32）退化为混音调用内
  同步解码。
- **块拼接的逐位一致性**（确定性硬账）：块 `extend_from_slice` 接待播缓冲
  尾部，每次混音后把已消费的**整数帧**从缓冲与游标里剪掉（f64 减整数在块长
  量级无舍入 ⇒ 游标相位跨块、跨调用连续），块边界不出现在采样序列里；音乐
  与音效共用同一个 `SourceFrames::add_frame`（声道映射 + 插值 + 增益叠加
  数学**收敛成一处实现**）。验证：`music_block_splices_are_sample_exact`
  以「等生产者退出 ⇒ 数据全就位」为前置条件，跨 5 块、跨 2 次混音逐
  `to_bits()` 比对「输出 == 源」；`streaming_blocks_match_full_decode`
  比对「OGG 流式分块拼接 == 一次性整条解码」。采样率不一致只主张数值一致
  （容差 1e-6，两路径 f64 游标累加区间不同）。
- **音乐总线混响**（`reverb.rs`）：简化 Schroeder / Freeverb 精简版——
  每声道 4 comb 并联 → 2 allpass 串联，左右用同一延迟整列表的不同置换 +
  不同 allpass 表展宽，表按 `round(base × rate / 44100)` 缩放。
  `ReverbSettings { wet, room_size }`（参数卫生化，非有限 → 0）；
  `out = dry + wet × 湿项` ⇒ **`wet = 0`（默认）为逐位透明旁路**，混音器据此
  跳过整条混响路径（不拷 scratch）。纯 f32 乘加 + 整数环下标 ⇒ 逐位确定，
  且**与混音分块大小无关**（两条断言）。comb / allpass 的冲激响应解析可算
  （`y[kM] = g^(k−1)`、`[−g, …, 1−g², …]`、Σy² = 1）并逐采样锁定。
  v1 只挂音乐总线（音效 / 空间音效不受 `set_reverb` 影响）。
- **内存实测**（一级交付物，`cargo run -p iris-audio --release --example
  music_memory`，60 s 立体声 48 kHz 同内容）：WAV 全量常驻 **22.0 MB**
  （磁盘 16-bit 11.0 MB）、OGG 压缩 **0.7 MB**（fixture 实测 94 kbps 外推）、
  **流式常驻 0.41 MB**（8 块队列 + 待播块 + 64 KB demuxer 读缓冲，
  `open(path)` 路径）/ 1.08 MB（`from_bytes` 路径）⇒ **常驻比 54× / 20×**，
  式子里没有「曲目时长」项（长度无关是结构性的）。示例自带 `ratio ≥ 20`
  回归断言。
- **测试资产**：`crates/iris-audio/tests/fixtures/tone_2s.ogg`（ffmpeg
  libvorbis 编码的 48 kHz 立体声 2 s 正弦 + 噪声，23 KB，实测解出**恰好
  96000 帧**；集成测试文件头写明重生成命令）。OGG 解码走真实资产的集成测试
  `tests/ogg_vorbis.rs`（7 项：布局 / 长度 / 逐位确定性 / 流式 vs 一次性一致 /
  经混音器采样级直通 / 截断容错 / 两入口同源）。
- **门面**：iris-engine `Audio` 转发 `play_music` / `stop_music` /
  `set_music_volume` / `music_is_playing` / `music_handle` / `set_reverb` /
  `reverb_settings`，prelude 追加 `AudioStream` / `SampleStream` /
  `MusicHandle` / `OggVorbisStream` / `ReverbSettings` / `load_ogg` /
  `parse_ogg`；headless 与有设备同语义。

新增测试 38 项（`iris-audio` lib 38 → 68：mixer 流式组 +11、reverb +9、
stream +9、ogg 单元 +1；OGG 集成测试 +7；引擎门面转发 +1），测试总数
**657 → 695**（debug）/ **656 → 694**（release）。
诚实限制清单见 ARCHITECTURE **§8.23**（只支持 OGG 里的 Vorbis、单轨、音乐
不空间化、混响只挂音乐且无阻尼低通 / 预延迟、下溢不补偿、`from_bytes` 每次
rewind 复制一次压缩字节、流式路径解码错误不可回传、未做显式 gapless 裁剪、
wasm32 退化内联解码）；设计与实测细节见 **§4.14**。既有 iris-audio 测试
零改动全过（混音核心逐位文化不动）。

### Added（R43：景深 DOF + 运动模糊 MB——后处理链写实扩展）

后处理链新增两个**相机级**效果 pass（`iris-render`）：景深
（`DepthOfFieldSettings { enabled, focus_distance, focus_range,
max_blur_radius }`，`shaders/dof.wgsl`）与运动模糊
（`MotionBlurSettings { enabled, strength, max_samples }`，
`shaders/motion_blur.wgsl`），链位在泛光**之前**（HDR 线性域——焦外
亮源与拖影参与后续亮度提取更物理）、暗角之前。**硬约束兑现：两资源
缺席或 `enabled = false` 时整帧像素与 R42 基线逐位一致**
（`tests/post_render.rs` 复用 `normal_render.rs` 的 R41 基线哈希
`0x609b2457f4d5cd82` 交叉锁定；DOF 清晰带覆盖全深度、MB 启用 + 相机
静止两条**启用态透传**路径亦逐位断言）。验收示例
`examples/dof_snapshot.rs`（四帧：DOF 关/对焦中排/对焦远景/运动
模糊，ffmpeg 2×2 拼图 `docs/dof_motion.png`）：

- **场景深度副本**（`shaders/scene_depth.wgsl`）：主 pass 深度附件是
  MSAA Depth32Float 且不可采样——照搬阴影的「深度写颜色 R 通道 +
  Depth32Float 双附件」既有方案，仅 DOF / MB 启用时把不透明 + 蒙皮
  实例从相机视角重画一遍，R32Float 附件存**线性视距**（clip.w，透视
  校正插值恰给出逐像素精确视距）；透明 / 贴花 / 粒子不进副本（§8.22）。
- **DOF**：薄透镜简化 CoC（清晰带半宽 `focus_range`，最大模糊半径按
  纹素）→ 12-tap 泊松盘 HDR 平均；深度用 `textureLoad`（R32Float 非
  过滤，不依赖 float32-filterable 扩展）；清晰带 CoC 恒 0 走 select
  原色透传（逐位无损）。
- **MB**：逐像素深度 → NDC z 解析重建（DirectX [0,1] 约定，
  z = f/(f−n) − (n·f/(f−n))/d，CPU 预计算系数）→ inv_view_proj 反
  投影世界位 → 上一帧相机矩阵重投影 → 屏幕 UV 速度 → 沿速度方向
  n-tap 采样（循环恒跑 32 槽按 tap 数掩码加权，规避 WGSL uniform 控制
  流限制）。渲染器保存上一帧主相机矩阵，首帧 / 相机静止 → 速度 0 →
  原色透传。**诚实边界：只覆盖相机运动**（物体级速度缓冲留待后续，
  §8.22）。
- **框架增量化**：`PostPassDesc` 增加可选附加纹理槽 `extra_texture`
  （组 0 binding 3，新 `post_depth_layout` = `post_layout` + 非过滤
  纹理）；全分辨率链槽位拓扑由纯函数 `full_chain_slots` 单点定义
  （DOF 写 A、MB 写 B、暗角写链头对面槽位防同 pass 读写互斥、blit
  源 = 暗角输出或链头），8 种开关组合的拓扑不变量有 CPU 单测；blit
  的全分辨率 A/B 接入绑定组随纹理对惰性构建（`ensure_full_targets`
  取代 `ensure_vignette_target`，暗角单开行为不变）。
- **测试**（`tests/post_render.rs` 5 → 12）：基线 FNV 锁定、显式
  关闭 ≡ 缺席、DOF 远景高频方差下降（高通残差方差——盒式模糊近似保
  全变差，整块方差对模糊不敏感）、聚焦带近无损、DOF 全清晰带逐位
  透传、MB 静止逐位透传、平移后水平 L2 梯度能量下降且垂直不变（方向
  选择性证据）、同输入两次渲染逐位一致。

### Added（R42：法线 / AO / ORM 材质通道——写实渲染第一块地基）

`MaterialDef` 新增 `normal_texture`（切线空间法线）与 `orm_texture`
（R=AO / G=粗糙度 / B=金属度打包，glTF metallicRoughness 的 G/B 约定 +
occlusion 合并进 R）两个 `Option<TextureHandle>` 槽位，配套 builder
`with_normal` / `with_orm`。**硬约束兑现：材质不带这两张贴图时渲染结果
与 R41 逐位一致**（`tests/normal_render.rs` 以改动落地前实跑捕获的 R41
基线全帧像素 FNV-1a 哈希 `0x609b2457f4d5cd82` 锁定，全部既有测试零改动
通过；论证链见 `iris-render/src/material.rs` 模块文档）：

- **中性纹理常驻 + 零分支 shader**：槽位 `None` 时渲染器绑 1×1 常驻
  纹理（法线 = (0.5, 0.5, 1) 的 `Rgba16Float`——8-bit unorm 无法精确
  表示 0.5（128/255 ≈ 0.502，×2−1 残留 0.0039 扰动），binary16 精确；
  ORM 复用白色兜底纹理，采样恒 1.0）。采样 *2−1 恰得 (0,0,1)，扰动式
  `T·nx + B·ny + N·nz`（TBN 第三列取**未归一化**插值世界法线）逐位还原
  该法线，`x·1.0 = x` 保证 ORM 中性值下金属度 / 粗糙度 / AO 与标量路径
  逐位相同。
- **TBN 两路**：网格带 glTF TANGENT（`MeshData.tangents` 新字段，独立
  顶点缓冲 loc12，w = 手性符号；`GltfBuilder::mesh` 同步导出）走顶点
  切线路（Gram-Schmidt 正交化）；无切线网格渲染器垫常驻零缓冲
  （1 MiB 共享，超 65536 顶点单独分配），片元 `dpdx/dpdy` 屏幕空间
  推导 TBN（手性由解的几何关系给出）——零 CPU 预计算、零逐网格开销。
- **逐像素表面参数**：粗糙度 / 金属度 = 标量因子 × ORM 通道
  （glTF factor × texel 语义），AO 只乘环境项（IBL + 常量补光）、
  不乘直接光；无 ORM 时逐位退化为既有标量路径。
- **色彩空间标注**：`CpuTexture` 新增 `srgb` 字段——颜色数据
  （baseColor / 贴花 / 程序化棋盘格）恒 sRGB（与既往上传格式逐位一致），
  法线 / ORM 数据贴图线性（`Rgba8Unorm` / `Bc1RgbaUnorm`）；glTF 纹理
  去重缓存键改为 (下标, 色彩空间)，同一张图可同时服务两槽。
- **glTF 解析**：`TANGENT`（VEC4 手性）进 `MeshData`；`normalTexture` /
  `metallicRoughnessTexture` / `occlusionTexture` 三引用解析——MR 与
  occlusion 同下标直接引用，异下标同尺寸 CPU 通道合成（R←AO），
  仅 occlusion 时 G/B 填 255 交标量因子；`strength` v1 忽略（诚实清单）。
- **程序化资产**：`CpuTexture::brick_normal_orm`（砖缝高度场中心差分
  →法线 + AO/粗糙度打包，splitmix64 每砖变化，纯函数逐位可复现）；
  验收场景 `examples/normal_snapshot.rs` → `docs/normal.png`（左右砖墙
  有 / 无法线对比 + 金属圆柱划痕 ORM，侧光目检：砖缝凹凸、AO 暗缝、
  划痕高光条纹）。
- 测试 +8：`normal_render`（R41 基线逐位锁定 + 通道生效 + 确定性）
  2 GPU 集成；texture 2（色彩空间标注、砖纹确定性 / 语义）、material 1
  （槽位 builder）、mesh 1（切线字节入账）、gltf 2（TANGENT + 法线/ORM
  解析、occlusion 异下标合成）。基线 debug 640 / release 639 →
  **debug 648 / release 647**。

### Changed（引擎 API 打磨 V：闭合 Iris Infiltration 的六条摩擦，§8.22 全清单）

「现象 → 绕行 → 建议修法」的第五轮闭合（模式照打磨 I–IV：引擎侧修复 +
游戏侧绕行删除作回归验证；语义规范 = `iris-core/src/event.rs` /
`iris-core/src/commands.rs` / `iris-render/src/foot_ik.rs` /
`iris-render/src/visibility.rs` / `iris-core/src/physics.rs` 模块文档，
架构记录见 ARCHITECTURE §3.8 / §3.10 / §8.22）：

- **①事件生命周期（双代轮换）**：`Events<E>` 改双代缓冲（`previous` +
  `current`），`clear_all_events` 从帧尾移到**帧首**、语义改为轮换
  （上一代丢弃、本帧缓冲降级）——事件可读窗口 = 产出帧余下部分 +
  下一帧全程：PostUpdate 产出的事件下一帧 Update 全程可读（§8.22① 的
  信箱绕行不再需要），帧尾退出检查读本帧产出（**修复既有 bug：PostUpdate
  发出的 `AppExit` 现状被清空、QUIT 根本退不出**）。消费契约改严：
  `iter` 非破坏性（不 drain 的消费方下帧会再看到同一事件），每条事件流
  每帧恰好一个 `drain` 方；`events_mut` 放宽为 `&World` 可用（并行系统
  可 drain）。游戏侧：iris-infiltration 的 `MenuCommand` 信箱资源与
  `consume_ui_clicks` 独占系统删除，`UiClickEvent` 在 Update 直接 drain
  消费；接触事件消费同步改 drain（防 begin 跨帧重复计数）。测试：
  `post_update_events_are_readable_next_frame_exactly_once`（编排级）+
  `rotation_keeps_last_frame_readable_and_drained_events_stay_gone` /
  `unconsumed_events_are_dropped_after_second_rotation` /
  `events_readable_until_drained`（单元级）。
- **②FootIk 与 AnimationPlayer 解耦**：新组件 `IkRig { node_entities }`
  （节点映射自带），`update_foot_ik` 查询 `Option<&AnimationPlayer>` /
  `Option<&IkRig>` 二选一非空（IkRig 优先、播放器回退），构造辅助
  `FootIk::from_rig(hip, knee, foot)`——纯 IK 装配不再被迫挂零剪辑
  播放器。游戏侧：iris-infiltration 四条腿链改纯 IkRig（零剪辑
  `AnimationPlayer` 删除）。测试：`pure_ik_rig_without_animation_player_works` /
  `ik_rig_takes_precedence_over_animation_player`。
- **③命令队列（`iris-core/commands`，最小实现）**：`CommandQueue` 资源
  （`Mutex<Vec<Command>>`，`Command = Exec(闭包) | Despawn`）+
  `spawn_deferred` / `despawn_deferred`（`&World` 可入队，并行系统可用）
  + `apply_commands` 独占系统（`CommandsPlugin`，注册在 **PostUpdate
  尾部**渲染之前——本帧命令本帧生效；单遍语义：执行期间新入队命令留给
  下一帧，防闭包自激励）。确定性警示入文档：命令按入队序执行，但闭包
  结果依赖执行时 World 状态，确定性回放慎用（诚实清单）。游戏侧：
  iris-infiltration 击晕处理从独占系统屏障改**并行系统**——ragdoll
  生成经 `spawn_deferred` 入队、守卫隐藏用引擎原语（⑤），并行吞吐
  保留。测试：`commands_execute_in_enqueue_order` /
  `despawn_deferred_removes_components_after_apply` /
  `despawn_of_dead_entity_is_noop` /
  `commands_enqueued_during_apply_wait_for_next_pass` /
  `apply_commands_works_through_world_resource_path`。
- **④节点 scale 污染 IK 骨长（生产保护）**：`FootIk` 新字段
  `expected_upper` / `expected_lower`（构造期显式填写或首次解算捕获）
  + `scale_warned`（防刷屏，每实体至多一次）——解算骨长与期望偏差
  > 10% 时 `log::warn!` 一次（不 panic，IK 按当前骨长继续）；模块文档
  写明契约「IK 链节点 scale 恒 1（缩放用网格子实体或父级）」。测试：
  `scale_pollution_triggers_warn_once` /
  `healthy_chain_captures_expected_lengths_without_warn`。
- **⑤隐藏子树原语**：`iris-render/visibility::set_subtree_hidden(world,
  root, hidden)`——沿 `Children` 显式栈迭代（深度帽 64 防构造性环，
  照 `hidden_by_ancestor` 模式）翻转整棵子树；**无 `Visibility` 的子
  实体插入** HIDDEN/VISIBLE（存储未注册时跳过 + warn——`&World` 不能
  注册新组件类型的既有约束）；配套 `World::insert_registered`（存储
  已注册前提下的 `&World` 插入路径）。游戏侧：iris-infiltration 的
  `hide_subtree` 10 行递归删除。测试：
  `subtree_hidden_flips_three_levels_and_back` /
  `missing_visibility_components_get_inserted` /
  `cyclic_subtree_terminates_via_depth_cap`。
- **⑥运动学刚体（`BodyType::Kinematic`）**：`RigidBody` 新增
  `body_type`（`Dynamic` 默认 / `Kinematic`）——**游戏驱动位置**（跳过
  重力 / 阻尼积分与速度写回，`Transform` 归游戏写）但**参与碰撞**：
  动体撞它按「静态方」路径全额推离（inv_mass = 0），与静态体（含
  sensor）配对**照常进接触集合**报事件（运动学角色踩 sensor 压力板
  不再需要动态代理绕行），kinematic × kinematic 与静-静同待遇跳过；
  全部「静态障碍」查询口径（`resolve_against_statics` /
  `sphere_sweep_statics` / `move_and_slide` 世界收集）统一走新函数
  `effective_static` 把它按静态障碍处理。**口径例外**：foot_ik 落地
  查询只收真静态体（运动学角色胶囊与自身腿链重叠，命中自身会毁掉
  落地语义）。游戏侧：iris-infiltration 的物理代理（`PROXY_NAME`
  实体 + 逐帧同步）删除——玩家根实体直接
  `RigidBody { body_type: Kinematic, .. }` + 动态胶囊，压力板 sensor
  事件与弹簧门推离由真实碰撞产出。测试：
  `kinematic_body_skips_integration_and_writeback` /
  `dynamic_body_is_pushed_out_of_kinematic` /
  `kinematic_participates_in_contact_events` /
  `kinematic_touching_static_sensor_produces_events` /
  `kinematic_kinematic_pairs_are_skipped` /
  `kinematic_counts_as_static_in_push_queries_and_character`。
- **验证**：headless 全链路不改语义照跑（自动玩家全关通关 / 压力板
  开门 / 弹簧门推回 / ragdoll 六肢体 / 菜单 RESUME 全部仍过）；测试
  总数 614 → **636**（debug）/ 613 → **635**（release）。
- **偏离声明**：①的双代轮换实现与「单 Vec 移动调用点」的字面方案
  不同——单 Vec 下帧首清空必然在 Update 读取前删掉上帧 PostUpdate
  事件，「PostUpdate 产出 → 下一帧 Update 读到」的验收测试在单 Vec
  下不可满足；双代轮换是满足该验收语义的最小实现（每事件最多存活
  两帧，`drain` 语义不变）。⑥的 foot_ik 口径例外同上（自身碰撞体重叠
  的工程事实）。

### Fixed（Infiltration 实测反馈四连修：压力板误触 / 过门腿部变形 / 击晕凭空消失 / 弹簧门无辨识度）

- **压力板靠近即触发 → 站上板面才触发**：`PLATE_RADIUS` 0.9 → **0.45**（语义 =
  玩家中心进入板投影内），插件层 sensor 盒半宽与视觉板 `scale` 同步缩到板面
  大小（三方同源同值；sensor 在胶囊接触面即报 begin，比中心距离判定早
  ~0.4 m，两路径调用同一幂等入口、单向闩锁，语义无碍）。测试：既有
  `plate_opens_door_and_player_passes_through` / `pressure_plate_contact_events_open_the_door`
  语义不变照跑。
- **过门时足部网格拉成大平面**（引擎侧，`iris-render/src/foot_ik.rs`）：
  ①`update_foot_ik` 新增**不可达回退门**——踝目标距髋超出
  `(|upper − lower|, upper + lower)` 可达域时本帧放弃该腿修正（保持姿势不写
  变换）：不可达/过近唯一解释是射线采样到非地面几何（过门、贴墙瞬间的门框
  /墙顶命中），钳制解算（拉伸至全长）会把腿指向异常目标；②落地查询不再
  收 **sensor**（事件域不是地面）与**碰撞组/掩码清零**（标记失效，如压力门
  开门标记）的碰撞体——失效前穿门瞬间射线起点落入门盒、t=0 异常采样把踝
  逐帧上拉直至飞出盒顶，正是变形的实测机理。纯函数 `solve_two_bone_ik`
  钳制语义不变（系统门在其之前拦截）。测试：`unreachable_ground_leaves_pose_untouched`
  （原 `unreachable_ground_stretches_leg` 按新语义改写）+
  `too_close_target_leaves_pose_untouched` / `sensor_volume_is_not_ground` /
  `disabled_collider_is_not_ground`（iris-render）；`player_crosses_pressure_door_without_foot_deformation`
  （iris-infiltration headless，过门全程脚踝贴地 ±5 cm）。
- **守卫被击晕后凭空消失**（游戏侧）：`spawn_ragdoll_kinematic` 只提供物理
  原语（文档明示渲染是游戏的选择），插件层 ragdoll 生成后逐肢体挂球体网格
  （半径 = 肢体定义半径）+ 守卫配色材质 + `Visibility::VISIBLE`。测试：
  `stun_spawns_ragdoll_and_hides_guard_model` 增加六肢体 MeshHandle/材质/可见断言。
- **门洞里的可碰撞方块 = 弹簧门板（设计如此，非 bug）**：R36 的动态盒 +
  `DistanceJoint` 回拉门（可推开、松手回弹），原配色与墙体几乎无差、缺视觉
  提示——改为青色 + 微发光材质，注释与模块文档写明设计语义。

### Added（游戏五：Iris Infiltration 第三人称潜入——dogfooding V，R34–R41 全量压榨）

新 crate `iris-infiltration`（第 42 轮迭代，游戏轮：**引擎零改动**，八个
新能力当载荷逐一压榨；语义规范 = `crates/iris-infiltration/src/logic.rs` /
`plugin.rs` 模块文档，crate 图与游戏清单见 README「用 Iris 做的游戏 V」与
ARCHITECTURE §2，摩擦清单见 §8.22，视觉验收
`examples/infiltration_snapshot.rs` → `docs/infiltration.png`）：

- **玩法**：俯视偏斜 45° 第三人称的 24×18 m 单层设施潜入——绕过 2 名
  巡逻守卫取 3 个数据核心、抵达出口获胜；被发现 3 次告负。Shift 潜行
  （视锥发现半径减半）、E 背后击晕、Esc 暂停菜单。纯逻辑核心（`logic`）
  零渲染依赖、逐位确定性；结构照抄 iris-ascent（logic + plugin +
  headless 自动玩家 + 快照 example）。
- **八能力接入**：① R35 NavGrid——守卫巡逻 / 追逐全走 `find_path`，
  压力门开 → 整图「便宜重建」（守卫可穿门追逐）；② R38 动画 v3——
  双剪辑 `BlendSpace`（参数 = 移动速度 0..1）连续混合 + 每角色双腿链
  两骨足部 IK（脚踝贴地 ±2 cm headless 断言）；③ R37 ragdoll——击晕
  → `spawn_ragdoll_kinematic` + `release_ragdoll` 瘫倒（v1 不醒来）；
  ④ R34 接触事件——压力板 sensor + 物理代理胶囊，begin/end 成对计数、
  踩上开门（单向闩锁）；⑤ R40 RTT——RT 相机俯瞰核心房 + `UiImageRtt`
  画中画（`UiAnchor` 锚定右上，改视口不漂移）；⑥ R41 UI 菜单栈——
  Esc push/pop + 焦点导航 + `UiClickEvent` 经 PostUpdate 信箱消费 +
  `Settings` QUIT 落盘；⑦ R36 距离关节——弹簧门板被物理代理真实推开、
  `DistanceJoint` 拉回关闭位（纯展示语义）；⑧ R25 空间音频——事件
  定位声源（警报 / 拾取 / 击晕 / 门）、听者跟随主相机。
- **验证**：headless 真实键盘注入自动玩家全关通关（49.5 s / 核心 3/3 /
  被发现 0，设计余量 ≤ 1）；纯核心 12 项单测（视锥三要素 / 警觉累积
  衰减 / 巡逻绕墙不进墙 / 压力板双路径 / 击晕背面限定 / 逐位确定性等）+
  插件 / headless 集成 10 项（接触事件端到端 / 弹簧门推回 / ragdoll
  六肢体翻转 / 菜单栈焦点点击 / 混合参数 + 踝贴地 / 双 Harness 逐位）。
  测试总数 590 → **614**（debug）/ 589 → **613**（release）。
- **摩擦清单（一级交付物，§8.22）**：UiClickEvent 产出阶段 vs 帧尾事件
  清空（Update 永远读不到）／FootIk 与 AnimationPlayer 强耦合（纯 IK rig
  被迫挂零剪辑播放器）／ragdoll 生成需要 `&mut World`（缺命令队列）／
  节点 scale 污染 IK 链长（无告警）／无隐藏子树原语／运动学角色无物理
  碰撞体（代理同步绕行）——六条均「现象 → 绕行 → 建议修法」，本轮
  引擎零改动全部只绕行。（更新：六条已全部闭合于「引擎 API 打磨 V」，
  见上方 Changed 首节。）

### Added（R41：UI 与菜单框架——锚定布局 + 按钮焦点导航 + 菜单栈 + 设置持久化）

UI 与菜单框架落地（第 41 轮迭代，A 级清单收官；语义规范 =
`iris-render/src/ui.rs` / `iris-engine/src/settings.rs` / `iris-engine/src/ui.rs`
模块文档，架构记录见 ARCHITECTURE §4.13，限制见 §8.21，视觉验收
`examples/ui_menu_snapshot.rs` → `docs/ui_menu.png`）：

- **锚定布局**：`UiAnchor { point, offset }` 组件 + `UiAnchorPoint` 九宫格
  锚点；纯函数 `anchor_to_position(point, viewport, size, offset)` 把元素
  一角对齐到视口一角（**中心类锚点把元素中心钉在视口中心，改视口不漂移**，
  不拉伸不缩放 `size`）。系统 `update_ui_layout` 读 `Viewport` 资源（缺席
  = 1280×720 兜底），只处理挂 `UiAnchor` 的实体（未挂者位置零变化）。
- **按钮 + 焦点导航**：`UiButton { label }`（便利组装，配合
  `spawn_ui_button`）+ `Focusable` 标记 + `UiFocus` 资源（`{ current, order,
  highlight_color, 原色映射 }`）。系统 `update_ui_focus` 收集**可见**可聚焦
  实体按锚定后屏幕 Y 再 X 确定性排序（并列取实体 bits）；↑/← 后退、
  ↓/→/Tab 前进、Enter/Space 与鼠标左键按下沿 + `cursor_position` 命中 →
  发 `UiClickEvent { entity }`（事件而非回调，游戏侧 `events_mut().drain()`
  消费）。焦点高亮 = 改写当前实体 `UiColor`（原色保存恢复，无样式表）。
- **菜单栈**：`UiMenu(String)` 标记 + `UiMenuStack` 资源
  （`push_menu`/`pop_menu`/`top_menu`）。系统 `update_ui_menus` 只裁决
  可见性（**栈顶菜单实体 `VISIBLE`、其余 `HIDDEN`、空栈全隐、未挂
  `UiMenu` 的实体不参与**；同名菜单多实体全可见）；**引擎不内置 Esc 语义**
  （弹栈由游戏侧自行调栈）。
- **UI Visibility 级联**：把渲染器的 `hidden_by_ancestor` 收归
  `iris-render::visibility` 共享，UI 全部收集路径（矩形 / 文本 /
  `UiImage` / `UiImageRtt`）统一启用**祖先级联**（任一祖先 `HIDDEN` ⇒ 子树
  全隐藏）；自身 `HIDDEN` / 缺席 = 可见的既有语义零变化（行为变更：此前
  UI 只看自身 `Visibility`，不看祖先）。
- **设置持久化**：`Settings`（Resource）字段 `master_volume` /
  `fullscreen` / `vsync` / `fov_degrees`；`Settings::load(path)` 文件缺失 /
  损坏 / 字段缺失一律回退默认（`log::warn`，加载永不失败，字段带 serde
  default 向后兼容）、`Settings::save(path)` 写 pretty JSON 并建父目录。
- **系统注册序**：`UiPlugin` 加入 `DefaultPlugins`，把 `UiFocus` /
  `UiMenuStack` 资源与 `UiClickEvent` 事件、三个系统注册到
  `Stage::PostUpdate`（渲染收集之前，顺序
  `update_ui_layout → update_ui_menus → update_ui_focus`，理由见
  `crate::ui` 模块头）。**新增 `iris-platform` 依赖到 `iris-render`**（UI
  焦点导航 / 点击消费 `Input` 资源；platform 不依赖 render，无环）。

与规范的偏离：
- **`update_ui_layout` 实现在 `iris-engine` 而非 `iris-render`**（设计指定
  ui.rs，但 crate 边界不允许 `iris-render` 反向引用 `iris-engine` 的
  `Viewport` 资源）；组件 / 纯函数 `anchor_to_position` 与菜单 / 焦点系统
  仍在 `iris-render::ui`。`update_ui_focus` 需要 `Input`，故 `iris-render`
  新增对 `iris-platform` 的依赖。
- **`Settings` 未派生 `Default`**（设计 derive 列表含 `Default`，但派生给
  出全零 master_volume=0 / fov=0）：改用手写 `Default`（master_volume=1.0、
  vsync=true、fov_degrees=70.0），字段级 `#[serde(default=...)]` 兼作缺字段
  向后兼容。

### Added（R40：多视口与渲染到纹理——小地图 / 监控屏玩法基础 + UI 图片控件）

多视口与渲染到纹理落地（第 40 轮迭代；iris-render 新模块 `rendertarget` +
RT pass + 两条 UI 图片控件管线，语义规范 = `iris_render::rendertarget` /
`ui` 模块文档，架构记录见 ARCHITECTURE §4.21，限制见 §8.20，视觉验收
`examples/rendertarget_snapshot.rs` → `docs/rendertarget.png`）：

- **`RendersTo { target: RenderTargetHandle }` 组件**（挂在相机实体上，须
  同步挂 `ActiveCamera`）：渲染器收集全部 `(ActiveCamera + RendersTo)`
  实体，每台 RT 相机渲一次到对应离屏纹理；**主视图 = 无 `RendersTo` 的
  相机**（选择逻辑零变化——默认场景无 RT 实体时行为与 R39 逐位一致，集成
  测试 `main_view_pixels_unchanged_with_rt_camera` / `no_rt_store_keeps_baseline`
  逐像素锁定「主视图零变化」）；只有 RT 相机时主视图回退 `Camera` 资源。
- **`RenderTargetStore`（World 资源，CPU 侧尺寸描述 + 句柄，GPU 纹理由
  渲染器惰性创建并跨帧持有）**：**不进 `DefaultPlugins`**——游戏侧按需
  `insert_resource`，缺席 = 无 RT 相机 = 主路径零变化。无 `resize`（v1）。
- **RT pass**：不透明 + 蒙皮 + 透明（复用 `collect_instances` 视锥参数化、
  主 pass 同款排序——确定性），用 RT 相机自己的视锥剔除；**复用主阴影图 /
  CSM**（RT 相机朝向不同时拟合仍按主相机）；**天空盒 / 贴花 / 粒子 v1 不画**；
  输出 = 几何渲到 RT 线性 HDR 中间缓冲 → **ACES 色调映射**（泛光 0 / 曝光 1 /
  暗角 0——无后处理链）到 `Rgba8UnormSrgb` 颜色纹理；**无 UI**。
  FrameStats 口径 = 主几何（RT 辅助视口不计）。数量上限 2，超出 warn 跳过。
- **UI 图片控件**：`UiImage { texture }` / `UiImageRtt { target }` 把普通
  纹理或 RT 颜色纹理贴到屏幕（复用于 `UiRect` 定位 + `UiColor` 乘色），UV
  全屏 0..1 直角采样（Nearest）；**图片之下、文本之上**（图片先于 atlas
  矩形 / 文本绘制，小地图底图垫在 HUD 文字之下）。挂图片的实体其 `UiRect`
  仅作定位 / 尺寸载体、不绘制矩形（与 `UiText` 同语义——否则缺省白矩形
  盖住图片，集成测试实测抓出）。
- **RT 渲染实现形态**：RT 几何走单采样几何管线（`rt_*`，无 MSAA），渲到
  每 RT 专用线性 HDR 缓冲，`rt_blit_pipeline`（Rgba8UnormSrgb 输出）ACES
  色调映射到 RT 颜色纹理；每 RT 独立全局 uniform / 实例缓冲 / 深度缓冲。

与规范的偏离：无（按设计决策实现；tonemap 选 ACES-through-blit 而非直接
clamp——复用既有 blit 着色器，几何管线同为 HDR 线性输出、alpha 混合在线
性域正确；RT 无 MSAA 为「像素化小地图天然契合」）。

新增 5 个 CPU 单测（rendertarget 3 + ui 2 图片收集/定位）与 6 个 GPU 集成
测试（RT 内容正确 + UiImageRtt 呈现、主视图零变化回归锁、RT 相机不改
FrameStats、UiImage 贴纹理像素、UiImageRtt 区域差分、无 RT / 空 store
主路径零变化），既有 IRIS 测试零改动全过（默认场景主渲染像素零变化）。

### Added（R39：盒投影贴花——弹孔 / 血迹 / 地面标记，正向前向管线的 decal cube projection）

贴花渲染落地（第 39 轮迭代；iris-render 新模块 `decal` + 贴花管线 +
独立 pass，语义规范 = `iris_render::decal` 模块文档，架构记录见
ARCHITECTURE §4.20，视觉验收 `examples/decal_snapshot.rs` →
`docs/decal.png`）。`Decal { texture, size, tint }` 组件：实体变换定义
**投影盒**（局部 Z 轴 = 投影方向，XY 为贴花平面，size 三轴尺寸、Z 常较
薄），纹理 RGBA 的 A 通道控制覆盖，tint 乘色（含整体 alpha 缩放）：

- **端盖代理几何 + 盒投影着色**：代理几何 = 单位盒 **±Z 两块端盖**
  （8 顶点 / 12 索引、仅位置属性，静态小缓冲全实例共享）——刻意不画整
  盒侧面（侧面 UV 退化为纹理边缘 texel 拉伸的「光晕」伪影），端盖即投
  影平面。片元把插值世界位经**逆世界盒矩阵**（CPU 预计算，着色器零矩
  阵求逆）变回盒局部 → `|local| > 0.5 + 1e-4` discard（盒裁剪）→
  `UV = local.xy + 0.5` 采样纹理 × tint → alpha 混合。**1e-4 容差是实
  测教训**：端盖 `local.z` 恒 = ±0.5（恰在盒面上），inv·model 往返的末
  位浮点误差使无容差的边界比较随矩阵舍入方向**整片 / 条带状误
  discard**（同一场景仅改盒尺寸即从正常变整枚消失；0.1 mm 级容差远大
  于 f32 噪声、远小于视觉相关）。
- **实例数据与深度语义**：`DecalInstanceRaw` = model 64B + inv_model
  64B + tint 16B = **144B/实例**（9 × Float32x4 location 1..=9，布局断
  言锁定；tint 逐实例携带，同纹理不同乘色不拆批）。深度 **LessEqual 只
  测不写**——端盖与被贴表面共面（推荐摆放：盒中心沿 +Z 抬 `size.z/2`
  使端盖贴面，再向相机侧加毫米级裕量规避共面深度抖动；`Less` 浮点抖动
  会闪烁），不写则透明 / 粒子照常按不透明深度裁决。**不进阴影 pass**
  （表面装饰而非几何，同透明 / 粒子理由）。
- **绘制序：蒙皮之后、透明之前**：贴花是**不透明表面的装饰**，玻璃等
  透明物应盖在贴花的**混合结果**之上——「后画者在上」由 pass 顺序统一
  裁决（主 pass → 贴花 → 透明 → 粒子），同一 HDR MSAA 目标 Load 混合
  （贴花参与 HDR / 泛光 / ACES）；**深度附件 Load + Store**（非透明 /
  粒子 pass 的 Discard）——贴花不写深度但其后的透明 pass 仍要 Load 同
  一附件，Discard 会把内容置为未定义（R23 DX12 教训的严格化）。
- **按纹理分批**：收集后按纹理句柄排序分组，同纹理连续实例区间合并为
  **1 draw**（批内仅切纹理绑定组）；批内实体 bits 并列序（确定性）。
  视锥剔除按**变换后 AABB**（单位盒 8 角点变换取包围盒——薄盒旋转下
  比包围球明显更贴合），被剔计入 `culled_instances`；`Visibility` 过滤
  与层级级联照常（组件缺席 = 可见，任一祖先 `HIDDEN` ⇒ 跳过）。贴花实
  例走独立缓冲（2 倍扩容复用）、每帧全量上传；**无贴花实例整个 pass 跳
  过（零开销默认路径）**。FrameStats 口径同步：`draw_calls` 含贴花批、
  `instances` / `triangles_drawn`（每实例 4 三角形 = ±Z 两端盖）、
  `culled_instances` 含贴花剔除（ARCHITECTURE §4.4.1）。
- **v1 限制（诚实清单，ARCHITECTURE §8.19）**：无光照（输出 = 纹理 ×
  tint，不参与 PBR / 阴影 / IBL——弹孔血迹类深色标记语义足够）；平面
  假设（深盒贴斜面贴合偏差上界 `size.z/2`，深度缓冲重投影的屏幕空间
  decals 在路线图）；同批重叠贴花无视深排序；场景序列化留白。

与规范的偏离：实例布局规范给定 model + inv_model = 128B，实际 144B——
tint 是 `Decal` 的**逐实例**语义（同纹理不同乘色并存），进实例数据是唯
一不破坏「按纹理分批」的承载方式（按 (纹理, tint) 分批则同纹理异色贴
花无法合批且需每批 uniform 池）。

新增 10 个测试（decal 模块单测 3 + GPU 集成 7），既有测试零改动全过；
GPU 集成覆盖：投影正确（棋盘格 UV 精确对应 + 盒外纯绿）、盒裁剪（足迹
外 2px 环带零采样区域差分）、alpha 混合精确（参考渲染法 ±2/255）、tint
乘色与整体 alpha（±2/255）、绘制序（玻璃合成在贴花之上，序错必挂）、
Visibility 自身隐藏与祖先级联、无贴花场景 FrameStats 与 R38 基线一致。

### Added（R38：动画 v3——BlendSpace2D 混合空间 + 两骨足部 IK + 根运动，第三人称动作动画门面）

动画运行时第三轮扩展（第 38 轮迭代；`iris-render` 新模块 `foot_ik` +
`animation` 扩展，语义规范 = `iris_render::animation` /
`iris_render::foot_ik` 模块文档，架构记录见 ARCHITECTURE §4.6.1）。v3
新字段全部带中性默认值——`blend_space` / 根运动默认关闭，不启用时行为
与 v2 完全一致（既有动画测试零改动全过）：

- **BlendSpace2D 混合空间**：`BlendSpace { samples: Vec<(Vec2, usize)> }`
  任意分布参数点集 → 剪辑索引；采样 = **逐轴线性分解**（先按 x 分列找
  左右列、列内按 y 找前后行，权重 = 各轴插值系数乘积的轴对齐双线性；
  单样本列自动钳制、全同 y / 全同 x 退化为 1D 链式 lerp 无需特判），
  ≤ 4 路非零权重、**非负和恒 1**（轴扫描序固定、并列取下标小者、参数
  精确命中样本 → 权重 1；测试覆盖精确命中 / 轴中点 0.5·0.5 / 1D 链式
  与范围外钳制 / 满网格双线性精确值 / 确定性参数网格遍历和恒 1）。
  `AnimationPlayer::play_blendspace()` 激活：**与 crossfade / 状态机互斥**
  （`transition` 清位、激活时进行中过渡丢弃），激活时 active 剪辑采样
  停用（事件轨道照常——共享时间轴仍按 active 推进）。
- **N 路姿势混合（`blend_pose`）**：把两路 crossfade 泛化为任意路数
  （每帧按 `blend_param` 采样出 ≤ 4 路「剪辑 + 权重」加权）；平移 /
  缩放 = 加权线性和，旋转 = **N 路 nlerp**（首个四元数为基准、点积 < 0
  翻转后加权求和再归一）——slerp 不具结合性、N 路链式配对误差累积，
  混合空间相邻夹角小 nlerp 非线性不可感知（两路 crossfade 仍 slerp）；
  三点等权 yaw 0/45/90 → 中间值 45°（纯函数 + 世界级双测）。
- **两骨足部 IK（`foot_ik` 模块）**：`update_foot_ik` 注册在
  `update_animations` 之后、层级传播之前（修正对象 = 动画刚写出的本帧
  局部姿势，写回局部 Transform 当帧传播生效；按 `FootIk` 组件开关）。
  落地查询 = 脚踝世界位上抬后竖直向下射线、**静态体 only**（收集语义
  同 `resolve_against_statics`，动态体不参与 IK 落地）；**平地门**
  （高差 ≤ 1 mm）不写任何变换。解算 = **完整两骨旋转解**：纯函数
  `solve_two_bone_ik` 余弦定理解膝角 + pole 向量定弯曲方向（不可达
  拉伸全长 / 过近钳 |u−l| / pole 并列确定性兜底），旋转回写用骨方向
  最小旋转弧反推 hip / knee 局部旋转、foot 按膝新世界矩阵逆回写位置，
  骨长严格保持。测试：纯函数精确解 / 不可达拉伸 / 最小距离钳制 /
  pole 镜像不翻转 / 零长骨断言；世界级**大球面斜坡脚踝贴面 ±2 cm**、
  平地 bit 级不变、无地面 / 动态体不写、**逐位确定性**、断链 debug
  断言。
- **根运动**：`root_motion_clip` + `root_motion_node`（None = 关闭）
  声明带根位移的剪辑节点——该节点 Translation 通道不再覆写 Transform，
  帧位移增量经纯函数 `root_motion_delta` 累积进 `pending_root_motion`，
  消费方每帧 `take_root_motion()` 取走（旋转到角色 yaw 系后交
  `move_and_slide`，用例入文档）。**循环回绕三段精确累积**（旧圈余段 +
  (k−1) 整圈 + 新圈已走，一帧跨多圈不跳变不漏记）、非循环末端零增量
  不重复计步；crossfade 期间仅取目标剪辑。与规范的偏离：root_motion
  标志放播放器侧而非 `AnimationClip`——新增剪辑字段会给既有测试的
  结构体字面量引入编译破坏（「既有动画测试零改动」硬约束优先，语义
  等价）。测试：全程增量累计 = 总位移、回绕帧无跳变 + 整圈累计恰一
  周期、一帧跨两圈 2.05 m 纯函数精确值、关闭时零影响。

新增 26 个测试（animation 14 + foot_ik 12），既有动画 / 物理测试零改动
全过。

### Added（R37：布娃娃与死亡混合 v1——球肢链 + 运动学窗口，附碰撞过滤掩码）

死亡系统地基落地（第 37 轮迭代；iris-core 新模块 `ragdoll` + `physics`
扩展，语义规范 = `iris-core::ragdoll` / `iris-core::physics` 模块文档，
架构记录见 ARCHITECTURE §3.8，视觉验收 `examples/ragdoll_snapshot.rs` →
`docs/ragdoll.png`）。v1 物理是纯位置物理（`RigidBody` 只有线速度、
无角速度 / 旋转积分），语义完整的旋转铰链无法诚实实现——ragdoll 刻意
用**球肢近似**绕开旋转语义（肢体 = 半径各异的球 + 既有 `DistanceJoint`
硬约束；球各向同性，翻滚 / 拖拽 / 堆叠语义正确且不需要旋转；旋转肢体
重建仍待角速度扩展，见路线图）：

- **碰撞过滤掩码（`Collider.groups` / `mask`）**：所属位组 + 碰撞白名单
  两个 `u32` 位字段，**默认全 1**——全部构造辅助零变化，引入前行为
  逐位一致。配对判定**对称**：`(a.groups & b.mask) != 0 &&
  (b.groups & a.mask) != 0` 才检测求解（单向认可同样过滤）；被过滤对
  与静-静对同待遇——不检测、不求解、**不进接触事件集合**；broadphase
  照旧（过滤只在配对白名单层做一次位与，候选构建与遍历序完全不变）
  ⇒ 既有 bit 级确定性测试零改动全过。测试：互不匹配组穿透（位置 /
  速度 / 事件三重断言 + 默认掩码对照组）、单向 mask（含镜像配置）、
  默认全 1 字段级断言。
- **`spawn_ragdoll_kinematic`（动画接管期）**：按 `RagdollDesc`
  （`limbs: Vec<RagdollLimbDef { offset, radius, restitution }>` +
  `links: Vec<(usize, usize)>`）生成**静态**肢体——`is_static = true`
  挡玩家 / 弹道，游戏侧照常驱动 `Transform` 定格死亡姿势；相邻对挂
  `DistanceJoint`（`stiffness = 1` 硬约束，rest = 生成时两端实际距离）；
  肢体全部挂 `RAGDOLL_GROUP`（bit 0）组位、`mask` 排除该位 →
  **自碰撞免抖**（生成时相邻球相切 / 微重叠，互相求解会被挤开炸裂；
  与场景照常碰）。确定性：生成序 = 定义序、同 pos 同 desc 逐位一致；
  断言半径 > 0、链接在界内且非自链接（中文断言消息）。
- **`release_ragdoll`（物理接管期）**：把全部肢体翻转动体
  （`is_static = false`）+ 写入继承速度 + 可选冲量 `(方向, 强度)`
  踢飞（`normalize_or_zero`，零方向确定性跳过）；只需 `&World`
  （组件写锁翻转，生成时已带齐组件；实体失效静默跳过不 panic）；
  释放后关节自动生效（挂载体已为动体），重力 / 碰撞 / 接触事件照常。
- **诚实限制**（模块文档「前提与边界」一一对应）：肢体间无挤压——
  **堆叠靠地面 / 场景**；全部 ragdoll 共用组位，两具 ragdoll 的肢体
  也互不碰撞（位组按语义层细分留游戏侧）；速度无钳制（v1 无 CCD，
  冲量过大可穿墙，调用方自控强度）；生成需 `&mut World`（实体分配是
  结构性修改），释放只需 `&World`。
- **测试**：拓扑（limb / link 数量、rest = 生成距离 bit 级、组位 /
  mask / 静态初值）、断言守护（越界 / 自链接 / 非正半径 panic）、
  kinematic 期纹丝不动 + 静态肢体挡弹道（对照球直达地面）、release
  翻转 / 速度继承 / 冲量增量 / 下坠 / despawn 静默跳过、**倒地链
  不散架**（240 步相邻肢体距离保持 rest ± 3e-2、终态 ± 1e-2、质心
  塌地收敛、全程速度有界无爆炸）、确定性（同 desc 两次全流程 240 步
  逐采样 bit 级一致）。

新增 11 个测试（physics 过滤 3 + ragdoll 8），既有物理 / 场景测试
零改动全过。

### Added（R36：物理约束与关节 v1——距离/弹簧/目标点约束进 step_physics 管线）

位置物理约束集落地，机关门 / 吊灯 / 弹簧板的地基（第 36 轮迭代；iris-core
`physics` 扩展，语义规范 = `iris-core::physics` 模块文档「约束与关节」
章节，架构记录见 ARCHITECTURE §3.8）。v1 物理是纯位置物理（`RigidBody`
只有线速度），语义完整的旋转铰链在此模型下无法诚实实现——**旋转约束
需角速度支持，见路线图**（R37 的 ragdoll 关节同属此类，本轮不做）；
v1 只做位置物理下语义清晰的两种关节组件、三种约束行为：

- **`DistanceJoint`（距离约束）**：把挂载实体与 `target` 实体的间距约束
  到 `rest_length`。`stiffness = 1` 为硬约束（每轮松弛全额投影回
  `rest_length`，双向「刚杆」语义）；`(0, 1)` 为弹簧（按比例部分修正
  ——位置物理的弹簧是**松弛投影**而非力积分：无能量守恒、无共振频率，
  「视觉弹跳」语义的诚实近似，每轮部分修正 × 2 轮松弛 × 多帧 ≈ 指数
  收敛）；`damping ∈ [0, 1)` 为轴向**相对**速度的每帧衰减（均分写回的
  阻尼器语义）。**静态端语义**：`target` 为静态体 / 锚点时 share = 0
  不动、动端全吃修正——静态锚点（is_static 或无碰撞体的纯 `Transform`
  实体均可，关节不要求碰撞）+ 动态球 + 硬距离关节 = 吊灯悬挂（悬挂
  验证场景单测锚定：重力下稳定悬在 rest 处、速度被阻尼压到近零、锚点
  bit 级不动）。
- **`TargetJoint`（目标点约束）**：把挂载实体拉向世界固定点（机关门
  开合 / 移动平台的物理形态）。`rest_distance` 给出到达半径（进入即
  不修——停在球壳上；0 = 精确到达）；速度响应把「沿目标轴」分量按
  `1 − stiffness` 缩放、切向分量按 `1 − damping` 缩放——到达时速度
  归零的临界阻尼**工程近似**（精确临界解需要力模型与质量，文档写明）。
- **管线嵌入（每轮接触之后）**：约束求解轮追加在 `step_physics` 松弛
  大循环内、每轮接触求解之后——**约束优先级高于接触**（先把实体投影
  回关节语义位置保结构完整，再让接触分离穿透；反过来接触推离会持续
  对抗约束、结构撕裂）；速度响应只在首轮（与接触速度响应同轮同理由）；
  位置修正照常走既有写回阶段（零额外写回代码）。**确定性**：约束收集
  序 = 组件存储序、求解顺序固定（距离先、目标点后）、无随机源——既有
  逐位确定性测试零改动全过，关节场景跑两遍逐帧 bit 级一致（新单测
  守护）。
- **纯函数求解器**：`solve_distance_joint` / `solve_target_joint` 与
  `solve_contact` 同款返回 `ContactResponse` 四通道，可脱离世界单测
  （均分 / 静态端 share / 重合退化 / 轴向阻尼只动轴向分量 / 球壳到达
  语义均有对照断言）。
- **诚实边界**（组件与模块文档一一对应）：无能量守恒；双向刚杆（非
  「只拉不推」的绳）；挂载体须为碰撞体动体（无 `Collider` 或静态 →
  跳过）；`target` 实体 despawn / 从未存在 / 两端重合 → 约束跳过不
  panic（与实体世代语义一致，不做悬垂 panic）；锚点为仅 `RigidBody`
  的飞行体时按帧初快照视为固定点。

新增 9 个测试（纯函数语义 / 硬距离 / 静态端 bit 级不动 / 弹簧单调收敛
无过冲 / damping 逐帧减半 / 目标点球壳与精确到达 / despawn 跳过对照 /
吊灯悬挂稳定 / 多约束场景确定性），既有物理测试零改动全过。

### Added（R35：导航网格与寻路 AI——静态场景 NavGrid + A* + 路径平滑）

导航基础设施落地，「硬编码航点敌人」时代终结（第 35 轮迭代；iris-core
新模块 `nav`，语义规范 = `iris-core::nav` 模块文档，架构记录见
ARCHITECTURE §3.9；AI 行为结构——巡逻 / 状态机——留游戏五）：

- **2.5D 导航图**：`NavGrid::build(params, &[(Vec3, ColliderShape)])`
  （入参与 `resolve_against_statics` 的静态体收集语义同源）逐 cell 判
  「中心点到静态体 **XZ 投影**的**最近点距离** ≤ `agent_radius`」→
  blocked——**最近点判定**（投影与 cell 面积相交即阻，对 agent 保守：
  多阻只绕行、漏阻即穿墙）、**相切算阻**（`≤`）；投影形状 Sphere /
  Capsule → 圆（竖直胶囊俯视即圆，`half_height` 不参与）、Box → 矩形
  （角点按真实最近点距离 = 圆角膨胀）。**2.5D 语义**：只在地平面导航，
  完全忽略 y 与高度（静态体按 XZ 投影作无限高柱阻挡——俯视语义，无
  跳跃 / 桥上桥下），路径航点 y 恒 0 由游戏侧覆写。诚实边界：cell 中
  心采样是离散判定，**`cell_size` 应 ≤ `agent_radius`**（经验规则非
  数学保证），窄于 cell 的障碍可能整条漏阻（单测锁定该走样语义）。
  `NavGrid` 纯数据结构 + `Resource`（可整图存入 World），不进
  DefaultPlugins；`world_to_cell` / `cell_center` / `is_blocked`（越界
  视为阻）确定性纯函数，参数非法中文断言 panic。
- **寻路 `find_path`（五步语义，文档即规范）**：① 端点越界 → `None`
  （覆盖域即导航域）；② 水平距离 < 1e-6 → `Some(vec![to])`；③ **就
  近可达性修正**——端点 cell 被阻（出生点贴墙）时 8 邻域 BFS 半径
  ≤ 2 cell 按 BFS 发现序取最近 free cell（**穿过被阻格也在所不辞**：
  找几何最近、可达性交 A* 裁决），找不到 → `None`——游戏侧免自兜底；
  ④ **A\***：8 邻域、直线代价 1.0 / 对角 √2 且**两正交邻必须 free**
  （防穿角——贴阻档格角点斜穿即擦墙；单测以「绕行代价恰 6.0 vs 穿角
  2.83」实证）、octile 启发（对 8 邻域代价可采纳且一致，首弹 goal 即
  cell 图最优）；⑤ **string pulling 路径平滑**（见下）。返回航点首点
  = 起点 cell 中心、末点 = 目标精确坐标。
- **逐位确定性 tie-break（核心论证）**：open 集 =
  `BinaryHeap<Reverse<((f, h), 节点序号)>>`——f32 无 `Ord`，用
  `f32::total_cmp` 全序（全程无 NaN：g ≥ 0、h ≥ 0 有限）；比较序 = f
  升 → h 升 → 节点序号（行主序 cell 索引 = 栅格扫描序）升；g / parent
  只在**严格更优**时写入（并列保留先到者）。弹出序由该全序唯一决定；
  堆内完全相等的重复条目（同节点同 f 同 h）携带的状态转移相同（closed
  + 严格更优检查使二次处理幂等），弹出先后不影响结果——同输入两次
  build + find_path 航点序列逐位一致（复杂多障碍场景单测守护）。
- **路径平滑（string pulling + cell 光栅 DDA 视线）**：贪心视线——从
  锚点尽量远找视线畅通的最后一个航点；视线测试 = **cell 光栅 DDA**
  （Amanatides–Woo 网格遍历，线段经过的每个 cell 含两端逐一查
  `blocked`；t 并列——斜线恰穿格角——两轴同步步进、角点两格都查，
  宁保守不放行）。**选光栅而非几何线段-膨胀体求交的理由**：与 blocked
  判定**同源**（同一位图，杜绝「光栅说能走、几何说撞墙」的二次膨胀
  不一致）、O(穿越 cell 数) 廉价、天然逐位确定。平滑后点数与总长均
  ≤ 平滑前（欧氏下界，单测锁定）并逐段 DDA 复验畅通。
- **`is_reachable`**：find_path 的轻量版——同端点语义 + 与 A* **同
  边集**的 BFS，find_path 为 `Some` ⇔ 本函数为 `true`，不构造路径
  更便宜（封闭房间不可达 / 修正后可达均有单测）。
- **动态障碍策略（v1）**：纯静态 + **便宜重建**——`build` 毫秒级纯
  函数，场景变化或固定节拍（0.5~1 s）整图重 build 换入（Resource 原子
  替换）；**不做增量更新 / 单元标记**，v1 限制清单（无高度语义 / 无
  增量 / 窄障碍可漏 / 修正半径 ≤ 2 cell / 最优性仅 cell 图上）写入
  模块文档。
- **文档**：nav.rs 模块文档（2.5D 语义 / 光栅化判定 / 寻路五步 /
  确定性论证 / DDA 选型理由 / 重建策略 / v1 诚实清单）；ARCHITECTURE
  §3.9 新小节；README 特性行 + 路线图。**测试**：nav.rs 新增 17 项
  （盒 / 球 / 胶囊光栅化精确 cell 断言、窄障碍走样语义、world_to_cell
  往返与越界、横贯墙留门必经门、L 形墙绕外角、对角缝防穿角、平滑
  点数 / 长度 / DDA 复验、封闭房间不可达、贴墙端点就近修正、逐位
  确定性、from==to 与越界边界、三项参数断言 panic），既有全部测试
  零改动全过。

### Added（R34：接触事件与触发器——物理产出 begin/end 事件 + sensor 触发区域）

物理系统从此**主动广播**接触生命周期，压力板 / 警报区 / 着地回调免逐帧
轮询（第 34 轮迭代；文档用例见 `iris-core::physics` 模块文档「接触事件
与触发器」章节）：

- **接触事件**：`step_physics` 帧末把本帧接触对与上一帧
  （`ContactTracker` 资源）差分，产出 `CollisionStarted`（a / b /
  normal / penetration）与 `CollisionEnded`（a / b）事件。语义决策：
  **实体序归一化**（事件 a/b 恒 `a.index() ≤ b.index()`；检测原始方向与
  索引序相反——despawn 的 swap_remove 会打乱存储序——时 normal 随归一化
  翻转，保持「从 a 指向 b」的方向约定）；**首求解轮采样**（接触对与
  normal/penetration 取自 `SOLVER_ITERATIONS` 首轮、位置分离前——松弛轮
  的重复检出对是同一配对的求解残影，无 begin/end 语义）；**静-静跳过**
  （静-静配对永不变化，与求解同分支跳过，不产事件）；**销毁成对性**
  （接触中的实体 despawn / 移除组件 → 下一帧必收到携带已死实体 id 的
  Ended——本帧集合只含存活实体的检出对，差分必然筛出消失配对，
  begin/end 严格成对、世代安全；含「全部碰撞体销毁后的早退帧」）；**确
  定性**（追踪集合 BTreeSet 与 broadphase 同款选型理由，事件先 Started
  后 Ended 按对序输出，同输入两次模拟事件序列逐项相等——单测守护）；
  **资源缺席语义**（`ContactTracker` / 事件资源未注册则静默跳过，照
  `Time` 缺席风格不强制注册；基线照常推进，后注册不收积压差分）。
  `PhysicsPlugin` 注册全部三类资源（`Events<CollisionStarted>` /
  `Events<CollisionEnded>` / `ContactTracker`），游戏侧不注册即得。
- **Sensor 触发区域**：`Collider` 新增 `is_sensor`（默认 false，既有
  构造函数与结构字面量零破坏）——sensor 对**照常进接触集合报事件**、但
  **跳过位置/速度求解**（不推离不反弹不摩擦，实体彼此穿过）；与
  `is_static` **正交**（sensor 也可以动，两动体 sensor 相撞只报事件）。
  构造辅助 `Collider::sensor_sphere` / `sensor_box`（静态触发区域）。
- **事件消费入口**：`World::events_mut::<E>()`（`EventRefMut` 可变守卫，
  与既有只读 `events` / `EventRef` 对称）——`drain()` 每帧恰好消费一次；
  事件在 `step_physics` 帧末产出、当帧其后注册的系统即可读，帧尾
  `clear_all_events` 兜底清空（消费契约写入 physics.rs 模块文档）。
  prelude 导出 `CollisionStarted` / `CollisionEnded` / `ContactTracker`。
- **测试**：physics.rs 新增 7 项（弹跳三段时序 begin/end 成对、接触中
  despawn 的销毁成对、sensor 穿越报事件不推离不反弹、归一化与法线翻转、
  事件序列跨运行确定性、静-静不报 + 资源缺席不 panic），既有全部物理 /
  游戏测试零改动全过。

### Added（打磨 IV：引擎 API 打磨 IV——闭合 Iris Ascent dogfooding 的六条引擎摩擦）

延续「打磨 I / II / III」模式：**引擎侧修复 + iris-ascent 侧绕行删除
验证**——每条修完 iris-ascent 测试仍绿才算闭合（headless 7 项 + 纯
核心 14 项 + 全部单测通过；摩擦原文见 ARCHITECTURE §8.18）。

- **① crossfade 缺通道平滑淡回基姿势（rest pose，正确性核心）**：
  `AnimationPlayer` 新增 `rest: Vec<(Entity, AnimatedField)>` 字段与
  `capture_rest(&World)` 方法——把全部 `node_entities` 目标实体的
  当前 `Transform`（平移 / 旋转 / 缩放三通道）记为基姿势快照（调用
  时机 = 实体生成后、动画推进前；`spawn_gltf_instance` 的先 spawn 后
  new 调用序满足）。混合采样语义修正：crossfade 期「目标剪辑没有」
  的通道值 = `lerp/slerp(源值, rest 值, w)`——过渡完成时恰为 rest、
  `blend` 清除后该通道不再被任何剪辑覆写、Transform 停在 rest（与
  「未被动画的字段保持当前值」的覆写语义自洽），**完成瞬间无跳变、
  不再定格在源值**（旧「idle 只有 scale / walk 只有 rotation 会让呼吸
  缩放起跑后冻住」的正确性缺陷闭合）。诚实边界写入文档：rest 是
  捕获时刻快照（捕获后 Transform 被改过仍淡回捕获值）；退化零四元数
  （glTF 节点缺省 rotation 的加载产物）按恒等捕获（不可作 slerp
  端点）。**兼容性承诺**：rest 缺省空表 = 未捕获，保持 v2「仅源有
  恒取源值、完成后定格」语义——既有动画测试零改动全过（逐个确认：
  全部既有混合用例的通道双方齐备或走未捕获路径）。CPU 测试：
  `crossfade_source_only_channel_settles_to_rest`（中点混合值 + 完成帧
  = rest + 完成后停留）、`crossfade_completion_frame_is_rest_without_
  jump`（帧序列 smoothstep 收敛、完成帧恰为 rest）、
  `crossfade_without_rest_keeps_v2_freeze_semantics`（兼容路径锁定）、
  `rest_pose_is_capture_time_snapshot`（快照边界）、
  `capture_rest_sanitizes_degenerate_rotation`（零四元数）；GPU 端到端
  `crossfade_source_only_channel_settles_to_rest_on_screen`（完成后画面
  与播放动画前**逐像素一致**）。iris-ascent 侧绕行删除：GLB 与降级
  代码剪辑**不再写恒值对齐通道**（idle 只写 scale、walk 只写 rotation），
  模型装配处 `capture_rest`；回归测试
  `crossfade_missing_channels_settle_to_rest_pose`（idle↔walk 过渡后
  scale / rotation 回基姿势）。
- **② GltfBuilder 编程式 GLB 构造（投入最大项）**：iris-engine 新模块
  `gltf_builder`——「**记账归 builder，人只声明几何 / 层级 / 关键帧**」：
  `material(base_color, metallic, roughness, emissive)` /
  `mesh(name, &MeshData, material)`（单 primitive；二进制段自动追加、
  4 字节对齐自动填充、POSITION min/max 包围盒自动计算、索引按顶点数
  自动选 u16/u32、蒙皮自动写 JOINTS_0/WEIGHTS_0）/
  `node(GltfNodeDesc)` + `add_child`（场景顶层自动推断）/
  `skin(name, joints, ibm)` + `attach_skin`（后置补挂）/ `animation(name,
  channels)`（`AnimChannel` 复用运行时 `AnimationPath` /
  `AnimationInterpolation`；时间轴内容一致的通道自动共享 times
  accessor；关键帧数量不齐立即中文断言）/ `build_glb()`（GLB 容器）+
  `build_json_and_bin(uri)`（外部 buffer 的 .gltf）。旧手写 JSON 的
  隐式对齐假设（anim_render 测试注释的「times 声明在偏移 112」类）由
  builder 对齐逻辑统一消化。**gen_asset 五个生成函数全部迁移**（cube
  外部 buffer + anim_cube / skin_arm / enemy_worm / ascent_bot 四个
  GLB），重新生成的资产与旧文件**加载语义等价**：节点 TRS / 剪辑
  channels（times + values 逐位）/ 蒙皮 ibm 矩阵逐位一致，节点旋转从
  「缺省零四元数（按恒等处理）」变为显式恒等（唯一已知差异，行为等
  价且更正确）；gltf_render（cube.gltf）等加载侧 GPU 测试零改动全过
  = 等价性证据。单测 4 项：`built_triangle_glb_loads_and_animates`
  （builder 构造 → write_glb → 引擎加载 → 剪辑时长 / 节点映射 / 采样
  推进正确）、`built_skinned_glb_loads_skin_and_ibm`（Skin 组件挂载 +
  关节映射 + ibm 逐值）、`built_json_and_bin_roundtrips`（外部 buffer
  往返）、`animation_rejects_mismatched_keyframes`（断言消息）。
- **③ 状态机无边硬切换留调试线索**：`request_state` 无边分支新增
  `log::debug!`（含 from / to 状态与边表长度——调试期清空 edges 导致
  全部过渡变成跳变时可由此定位）；文档写明「无边不是错误、显式硬
  切换是合法用法」。
- **④ SmoothFollow 平滑追随范式**：iris-engine 新模块 `camera_rig`——
  `SmoothFollow` 组件（target / offset / lerp_per_sec / snap_on_first_
  frame + 记忆态 `pos`）+ `update_smooth_follow` 并行系统：平滑世界位
  朝「目标当帧世界位 + offset」以 `1 − exp(−lerp·dt)` **帧率无关**
  指数趋近，追随者局部平移写成 `parent⁻¹ × pos`（父链存在旋转 /
  缩放也精确）；`snap()` 标记下一次更新直接对齐（复活 / 传送）；
  首帧吸附。编排契约写入模块文档：在目标写入系统**之后**注册（目标
  本帧位移当帧即被追随）；目标世界位沿父链用局部 `Transform` 现值
  合成——**不读滞后一帧的 GlobalTransform**（读上帧的口径亦可接受，
  代价是目标本帧位移迟到一帧，文档写明）；Update 写局部、PostUpdate
  传播，传播后的消费方当帧读到新全局位；只管理位置（机位俯仰不动）。
  单测 5 项：首帧吸附 / 收敛的帧率无关（1/60×60 vs 1/20×20 终点
  一致）/ snap 逐位吸附 / 追随者旋转不动 / 使用目标当帧位置。
  iris-ascent 侧绕行删除：约 30 行的 `CameraRig` 资源及其积分代码
  删除，相机子实体改挂 `SmoothFollow::new(player_root, CAMERA_OFFSET,
  CAMERA_STIFFNESS)`，`update_smooth_follow` 注册在 `run_ascent` 之后，
  复活帧 `follow.snap()`；headless 吸附断言改读组件记忆态（逐位一致
  语义不变）。
- **⑤ 输入语义选择指南（文档闭合，无引擎改动）**：`iris-platform`
  input 模块文档新增「电平（`key`）vs 边沿（`key_pressed`）选择指南」
  ——判定法则「动作是否应该按住就持续生效」、电平 + 玩法门控（如
  grounded / 土狼时间）对跳跃类玩法的优势、脚本注入差异（电平一帧
  置位、边沿需 release+press 两步）；判定为合理设计语义（iris-ascent
  的 `AscentInput::jump` 即电平 + grounded 门控 = 自动 bunny-hop 友好）。
- **⑥ move_and_slide 衔接参数表（文档闭合，无引擎改动）**：
  `iris-core::character` 模块文档新增「平台跳跃衔接参数表」——低台阶
  （落差 ≤ `step_height`）/ 走落（落差 ≤ `snap_distance`）/ 水平缝
  （< `2 × radius`）/ 落点窗口（≥ `radius` 且落点法线可站立）/ 平台
  厚度（≥ `radius`）的可靠域与推导，超域失败模式（贴边落在台缘角点
  的 45° 法线卡墙、缝宽 + 落差双超域、薄板穿通）的症状 / 复现特征 /
  诊断建议（打印 `ground_normal`、以关卡不变量单测固化衔接域——
  iris-ascent 先例）。

### Added（R33：基准与性能门禁——criterion 基准 + release 预算测试 + CI 扩展）

32 轮成果的「性能防回归」保护层：CPU 热路径从 example 版基准升级为
criterion 正式基准，配 release 预算测试与两条新 CI 门禁。

- **criterion 基准 ×3 crate**（dev-dependencies `criterion 0.7` +
  `[[bench]] harness = false`，全 GPU 无关纯 CPU 路径）：
  `iris-core/benches/core.rs`（ecs_spawn_10k / ecs_query_iterate_10k /
  schedule_build_batches / parallel_run_8_systems / physics_step_100 /
  move_and_slide / broadphase_200_aabb / determinism trace_240）、
  `iris-render/benches/render_cpu.rs`（frustum_extract_and_test /
  bc1_encode_256 / instance_diff_10k / collect_instances_5k）、
  `iris-audio/benches/audio.rs`（mix_32_voices_1s /
  spatial_gain_10k）。全部均值基线整理进 ARCHITECTURE §7.1「性能基线」
  （标注测量机器与日期，仅供量级参考）。
- **release 预算测试**（`iris-core` / `iris-render` / `iris-audio` 的
  `tests/budgets.rs`）：4 个代表路径（ECS 遍历 300 µs / 物理步进
  25 ms / 帧收集 1.5 ms / 混音块 500 µs）各实测 N 次取中位数，断言
  不超「基准 mean × ~5」的写死预算（注释记录测量机器与日期）。
  **定位 = 防突然 10 倍劣化的烟雾报警**（5 倍裕度保证 CI 零误报），
  精确回归检测归 criterion 基线对比；debug 构建自动跳过并打印原因。
- **CI 扩展**（既有 test / wasm / lint 三 job 不动）：`test-release`
  （`cargo test --workspace --release`——预算测试在此生效）与 `bench`
  （`cargo bench --workspace`——记录性冒烟：全量可跑不 panic；CI 数字
  不可与本地基线横向比较）。
- **新公开入口（基准 / 诊断用）**：`iris-render::renderer` 的
  `CpuCollector`（帧收集 CPU 路径的 GPU 无关入口：World + MeshStore +
  Frustum 构造一次反复收集，语义与渲染器帧内 `collect_instances` 完全
  一致、scratch 跨帧复用）与 `diff_changed_ranges` 公开导出；
  `iris-core::Schedule::batch_sizes`（批次划分只读视图，不执行系统）。
- **清理**：删除 example 版基准 `bench_ecs.rs` / `bench_parallel.rs`
  （职责并入 criterion benches；README / ARCHITECTURE 引用同步更新）；
  `determinism` 基准组样本数降至 10（单次迭代 ~2.2 s，默认 100 样本
  单组耗时 220 s+，拖垮全量 bench 反馈周期）。

### Added（R32：示范游戏 IV——iris-ascent《Iris Ascent》3D 平台跳跃，dogfooding R28–R31）

- **新 crate `iris-ascent`**（下游示范游戏 IV）：第三人称线性上升平台
  跳跃——悬浮台地（0.28 m 台阶）→ 大球坡面（R=14，走落 + 球顶定点跳）
  → 四段跳板弹射（竖直冲量 18 m/s，弹高 ~16.5 m）向上攀登 61 m，14 颗
  宝石 + 3 检查点 + 山顶信标胜利 + 坠落线复活。纯逻辑核心 `logic.rs`
  只依赖 glam + 引擎纯函数：**`move_and_slide_statics` 就是玩法物理**
  （刻意 dogfooding R31——headless 与窗口同一模拟，台阶迈上 / 贴墙滑行 /
  坡限 / 贴地吸附全部由控制器五步管线承担）；跳板为游戏逻辑（AABB 判定
  + `vel.y ≤ 0` 门禁的一次竖直冲量）；土狼时间 0.1 s；落地冲量阈值
  3 m/s（防小台阶误触发落地尘土）；`AscentEvent` 七类事件自带世界坐标；
  逐位确定性（两次模拟 to_bits 逐帧比对单测）。关卡刻意设计成「前进 +
  两个定点跳」可通关——**headless 脚本化自动玩家经真实键盘注入
  （Input → 相机相对翻译 → 逻辑 → 表现全链路）通关：19.5 s / 宝石
  14/14 / 检查点 3/3 / 坠落 0**；纯核心单测 14 项（台阶行走 / 球面贴地 /
  弹射顶点解析对照 / 土狼时间窗口 / 落地冲量阈值 / 检查点复活逐位精确 /
  坠落复活 / 胜利冻结 / 全关通关 / 确定性）+ 插件层 headless 集成测试
  6 项（启动装配 / 全关通关 / 状态机切换 / 事件→粒子音频 / 复活相机吸附
  与 HUD / LOD 三角形对比，GPU 缺席自动跳过）。
- **双剪辑角色 GLB（R28 载荷）**：`gen_asset` 示例新增
  `write_ascent_bot_glb`——程序化生成 `assets/ascent_bot.glb`（盒身 +
  眼小机器人）：`idle`（呼吸压缩 1→0.93→1，2 s 循环）与 `walk`（前倾
  10° + 左右摇摆 ±6°，0.6 s 循环）两剪辑；运行时**状态机**
  （states = [idle, walk]、双向边 duration 0.15 s）由移动意图
  `request_state` 切换，crossfade 实测（headless 断言 blend 起止）。
  **事件轨道**：GLB 解析不产事件，API 侧向 walk 剪辑注入「footstep」
  标记（0.15 / 0.45 s），`take_events` 驱动脚步声（仅地面有效）。
  模型加载失败自动降级为程序化几何 + 代码构建的同构剪辑（行为完全
  一致，wasm / 资产缺失可玩）。**摩擦记录**：crossfade「仅源有」通道
  在过渡后定格——两剪辑被迫路径对齐（scale + rotation 双写），见
  ARCHITECTURE §8.18 ①。
- **LOD（R30 载荷）**：22 个远景装饰（山景球 ×8 / 巨柱 ×8 / 浮岛 ×6）
  挂 `LodMesh`（高 / 低模 + 60 m 切换）；量化证据：相机拉远 160 m 后
  `triangles_drawn` 9 440 → 7 324（headless 断言 `far < near`）。
- **CSM（R29 载荷）**：方向光斜照 ~130 m 场景（关卡高差 61 m）——无需
  新代码；快照目检 + `shadows` 开 / 关逐像素差分 4 622 px（远景山体
  受光 / 阴影面 + 角色 / 旗杆接触阴影）作证据。
- **三层玩家结构 + 相机平滑追随**：根实体（逻辑位置，rotation 恒等）→
  模型子实体（AnimationPlayer + 朝向 yaw）→ 相机子实体（固定偏移 +
  指数趋近平滑，平滑量写局部位置 = rig 世界位 − 根位置；复活瞬移整体
  吸附——范式与摩擦记录见 ARCHITECTURE §8.18 ④）。
- **FX 池 + 空间音频**：七类事件（跳 / 落 / 弹 / 宝石 / 检查点 / 复活 /
  胜利）→ FX 池 12 槽位 burst（参数全字段覆写 + `reset` 就地复用）+
  `play_spatial` 事件定位音效（八种程序化剪辑，听者跟随第三人称相机，
  复活横幅 2 s 计时）。
- **快照**：`examples/ascent_snapshot.rs`——自动玩家真实打通第一段
  登顶后定格中段战局（640×360 → `docs/ascent.png`，已目检）；打印
  FrameStats 并断言 `culled_instances > 0` 与 LOD 近 / 远三角形对比。
- 文档：README 新增「用 Iris 做的游戏 IV」+ 摩擦清单摘要；
  ARCHITECTURE §2 补 iris-ascent 段、§8.18 补第四轮摩擦六条。

### Added（R31：物理角色控制器——竖直 Capsule 碰撞体 + move_and_slide 运动学控制器）

- **竖直 Capsule 碰撞体**（iris-core `physics`）：`ColliderShape::Capsule
  { radius, half_height }`——轴段恒沿世界 ±Y（端点 = 中心 ±
  (half_height − radius)·Y，`half_height ≥ radius` 构造断言，取等号退化
  为球），角色直立人体的标准形状；构造辅助 `Collider::static_capsule` /
  `dynamic_capsule`。作为一等形状放进 `ColliderShape` 的意义：broadphase
  （collider_aabb → 中心 ± (radius, half_height, radius)，**胶囊轴恒竖直
  故 AABB 简单**）、`detect_pair` 配对、射线、扫掠**全部自动获得胶囊
  支持**。`detect_pair` 新增三对配对（+反向共 6 分支，形状对穷举 9 分支）：
  capsule-球 = 轴段最近点（解析投影 clamp [0,1]，段-点最近点）退化到
  球-球（段内最近点等价球-球、端点即端帽判定）；capsule-AABB = 轴段-盒
  **交替投影 4 轮收敛**（来回 clamp 段参数与盒范围——迭代近似非解析，
  竖直段 × 轴对齐盒按 XZ/Y 分解 2 轮内即收敛，4 轮为确定性裕度；盒内
  照 sphere_box 最浅穿透轴退出的既有语义）；capsule-capsule = 段-段最近
  距离（标准钳制交替解法 4 轮；竖直轴恒平行，距离与解析对照一致）。
  法线方向约定与既有 Contact 一致（a → b）。`ray_vs_shape` 增胶囊分支：
  无限圆柱 slab 解（推导入文档：水平距离约束代入射线得 a·t² + 2b·t + c
  = 0，近根 + 轴段 y 范围裁剪）+ 上下端帽两球解析解取最小正 t，起点在
  内部 → t = 0 照既有 ray_sphere 语义（法线 = 离轴方向、重合退化 −dir）。
  精确数值单测：侧向 / 端帽 / 反向法线翻转 / 平行轴解析对照 / 退化胶囊
  ≡ 球 / 射线正交命中 t 精确 / 端帽斜法线 / 未命中 / 内部起点 / max_t
  裁剪 / 胶囊经完整 step_physics 管线（网格 broadphase + 检测 + 求解）
  落地静止。
- **扫掠重叠判定阈值**：`sphere_sweep` 泛化为 `shape_sweep`（任意
  `ColliderShape`，同款保守推进二分 24 次；`sphere_sweep` 变为其球便捷
  封装，行为不变），并引入 `SWEEP_CONTACT_EPSILON = 1e-6`——穿透低于
  阈值按「恰好相切 / f32 舍入噪声」处理不算重叠（静置贴合面经浮点路径
  可产生 ~1e-8 名义穿透，若算重叠会把「起点已重叠」误判、贴地角色原地
  卡死）。
- **运动学角色控制器**（iris-core 新模块 `character`）：`move_and_slide`
  （世界封装，收集语义与 `resolve_against_statics` 一致）/
  `move_and_slide_statics`（纯函数核心，静态体列表入参可脱离世界单测，
  R21 球扫掠的正式消费）——竖直胶囊 + 五步管线（顺序即文档）：① 重力
  （`v.y += g·gravity_scale·dt`）；② 水平**最多 4 次扫掠迭代**：形状
  扫掠求首次碰撞 → 推进到命中点 → 速度与剩余位移切掉法向分量（法线
  水平归一，陡坡斜法线不把水平推力转成沿坡上滑分量；可站立坡面不切
  速度）→ 贴墙滑行；③ 台阶「抬起重试」：`vel.y ≤ 0` 且脚下 snap 距离
  内有地面（防空中跨步）且正上方畅通才抬升 `step_height`，前向位移完整
  走完 + 向下扫掠落回 + 落点法线可站立才接受——不可站立阻挡（台阶迎风
  面）前向距离至少 `radius`（中心须送进台面落到平顶，贴台缘角点法线
  倾斜永不成立；半径前跳的速度红利仅限「撞墙迈台阶」一种情形），可站立
  坡面按剩余位移原样（缓坡上坡 = 逐帧低台阶）；④ 斜坡限 + **贴地吸附**
  （同一 次向下扫掠，探测距离 = `max(|vel.y·dt|, snap_distance)`）：法线
  `n.y ≥ cos(slope_limit)` → grounded、`vel.y = 0`；过陡 → 不 grounded、
  速度切掉入坡法向（下滑分量保留，重力逐帧沿坡加速自然下滑）；⑤ 跳跃
  （`vel.y > 0`）不吸附，向上扫掠命中天花板贴住。输出
  `CharacterOutput { position, velocity, grounded, ground_normal }` 由
  调用方写回组件。全程纯函数、固定迭代轮数、无随机源——同输入两次调用
  逐位一致（单测守护）。场景测试全坐标写死：平地行走 / 墙前 radius 停止
  与切向滑行 / 低台阶一步迈上 / 高墙照撞 / 缓坡（30°，大山球解析斜面）
  上坡贴地 / 陡坡（60°）不 grounded 下滑加速 / 下小坡 snap 不腾空 /
  跳跃抛物线回落 / 确定性 / 空静态体自由运动。诚实边界（模块文档）：
  起点嵌入不去穿、一步一台阶、全贯穿不检出。
- **三处 `ColliderShape` 穷举匹配同步**：iris-arena（俯视投影：胶囊
  `Capsule { radius, .. }` → 同半径圆——轴竖直、俯视阻挡由半径决定）；
  iris-engine 场景序列化（`ColliderShapeSection::Capsule`，外部标签
  `{"capsule": {"radius", "half_height"}}`，加载鲁棒 clamp 半高 ≥ 半径、
  旧文件不受影响，往返单测守护）；iris-render 无 `ColliderShape` 消费
  点（grep 确认）。iris-game 未迁移 `move_and_slide`：玩家移动积分在纯
  逻辑 `logic::step`（跳跃 / y 积分 / 钳制安全网，既有测试锁定行为），
  迁移必改测试语义，违背「既有测试零改动」约束——`character` 模块文档
  附用法示例作为消费范式。

### Added（R30：内存与规模——BC1 纹理压缩 + 距离 LOD + 内存占用统计）

- **纹理 BC1 GPU 压缩**（iris-render `texture_bcn`）：上传决策树「设备
  启用 `TEXTURE_COMPRESSION_BC` + 尺寸 4 对齐 + 全不透明（alpha 恒 255）
  → `Bc1RgbaUnormSrgb`（0.5 B/px，对 RGBA8 的 8 倍节省）；任一不满足
  回退 RGBA8」。wasm 的 WebGPU 实现普遍无 BC 特性——**无分支回退**：
  不写 cfg，运行时特性检测天然覆盖 web 与 native 老 GPU；wgpu 压缩格式
  要求特性显式请求，`Gpu` 初始化按适配器能力请求 BCn 子集并新增
  `features()` / `supports_bc()` 检测入口。编码器 `encode_bc1` 纯函数、
  全整数运算、零 unsafe：逐 4×4 块取 RGB 距离最远端点（O(16²) 确定性
  暴力，与 PCA 法的取舍写入文档）、亮度排序 c0 > c1、RGB565 四舍五入
  量化、四色调色板最近项索引（并列取先）；**确定性契约**——同输入两次
  编码逐位一致（单测）；非 4 倍数尺寸统一预 pad（边缘像素复制）写死。
  质量护栏：纯色 565 精确色往返零误差、温和渐变平均误差 < 4/255、位
  打包逐字节断言；GPU 测试 `bc1_render.rs` 以「alpha=255 副本 → BC1 /
  alpha=100 副本 → RGBA 回退」的同画面 A/B 断言两路径全帧平均误差近零。
  mipmap v1 不做（引擎无 mip 机制，文档写明）。
- **网格距离 LOD**：`LodMesh { high, low, switch_distance }` 组件（挂载
  实体**替换** `MeshHandle` 参与收集）；收集查询改 `Option<&MeshHandle>`
  + `Option<&LodMesh>` 协同——有 LodMesh 按「实体原点到相机 eye 欧氏
  距离」选网格（`dist <= switch` 用 high、边界归近侧；纯函数 `select`
  可单测），选中的句柄参与合批 / 剔除 / 透明排序 / 三角形统计，无
  LodMesh 走旧语义、两者皆无跳过；蒙皮路径同口径。`FrameStats::
  triangles_drawn` 天然成为验收指标。v1 限制写入文档：距离用实体原点
  （非包围球中心）、**无滞回**（边界抖动可能逐帧切换）、仅两级。
- **内存占用统计**：「低占用」卖点观测口径——`TextureStore::
  memory_bytes()`（Σ 源 RGBA 字节）/ `MeshStore::memory_bytes()`（顶点 +
  索引 + 蒙皮属性字节）；`TextureStore::gpu_bytes_estimate(feature_bc)`
  按上传决策树逐纹理估算显存（BC1 0.5 / RGBA 4 B/px，与渲染器共用
  同一决策纯函数），`Renderer::texture_is_bc1(handle)` 观测实际上传
  格式。
- **验收基准**：新示例 `memory_bench`（iris-engine）——4 × 2048² 程序化
  纹理 + 高/低模 UV 球对 + 8 个 LodMesh 实体，输出真实数字（开发机
  RTX 4060 Laptop / Vulkan / release）：CPU 源纹理 64.00 MiB、GPU RGBA8
  回退 64.00 MiB vs **BC1 实际上传 8.00 MiB（压缩比 8.00x，显存
  -87.5%）**；LOD 近景帧 `triangles_drawn` 49,152（8 × 6144 高模）→
  远景帧 768（8 × 96 低模，**-98.4%**，instances / draw_calls 不变）。
  新 GPU 测试 `bc1_render.rs` / `lod_render.rs`；既有纹理相关测试零
  改动全过；文档：ARCHITECTURE §4.18 / §4.19（含实测数据段）、§4.1 /
  §4.9 / §7 / §8 同步，README 特性行与示例列表。

### Added（R29：阴影 v2——方向光级联阴影 CSM，3 级）

- **级联阴影贴图**（iris-render）：方向光阴影贴图从单张 2048² 升级为
  **2048² × 3 层 `texture_2d_array`**——逐级联一个渲染 pass（每层独立的
  D2 颜色/深度视图），采样侧全层数组视图按级联选层；「R32Float 颜色附件
  存深度 + Depth32Float 深度附件 Less 测试」的双附件方案原样保留（§4.7
  的 DX12 depth-only 教训对数组同样适用），点光 cubemap 阴影不动。
  近处纹素密度提升约一个量级（默认相机下首级联 ~100 texel/m，v1 单张
  ~42），解决大场景「近处阴影糊、远处无影」。
- **实用分割（PFS）**：`cascade_splits(near, far)` 纯函数——对数/线性
  分割按 λ = 0.6 混合，`near` 取 `min(z_near, 1.0)`，`far` 钳制到覆盖
  距离常量 **`SHADOW_DISTANCE = 120 m`**：级联只铺到此处，覆盖带之外的
  片元不投影、保持受光（远级联接触阴影 peter-panning 随窗口增大，v1 接受）。
- **拟合与纹素对齐**：`cascade_fit` 纯函数——相机深度片 8 角变换到光空间
  （朝向逻辑与 v1 一致、光源眼点固定世界原点，深度窗口宽度与相机离原点
  距离解耦）求 AABB，深度窗口向光/背光各外扩一个包围球半径（切片外的
  高遮挡物与足印内的地面都在窗口内）；**XY 纹素对齐**（`snap_aabb_to_texels`，
  min 向下/max 向上取整只扩不缩）——正交投影平移恒为纹素整数倍，相机
  平移时阴影图采样相位只整纹素跳变，消除阴影边缘闪烁；Z 不对齐（深度
  无可见相位漂移）。关键单测断言：对齐后跨度为 texel 整数倍、幂等、
  拟合含住 8 角点、亚纹素平移 UV 漂移 ≤ 2 texel。
- **级联选择（片元）**：`view_z = dot(world_pos − eye, cam_forward)`
  （view 深度而非欧氏距离/clip z），`view_z < splits[i]` 即第 i 级；
  `CsmParams`（224B：3 × 光空间矩阵 + 相机前向 + 分割点）独立 uniform
  缓冲走 globals 组 **binding 8**（排现有最大 binding 7 之后，不扩
  Globals——单一 `light_view_proj` 字段移除，Globals 576B → 512B，
  skybox 同步）；阴影 pass 每级联一个 64B 矩阵小缓冲（一级联一缓冲，
  规避 write_buffer 合并问题）。`textureSample` 的 uniform 控制流掩码
  模式不变：选择/越界非均匀分支照常采样、事后混向受光。
- **实例区间沿用主 pass**：三个级联 pass 复用主 pass 视锥剔除后的不透明
  区间全部重画（保守不漏影；按级联视锥细化列为后续优化），透明项与粒子
  不投影语义不变，`FrameStats` 口径不变。既有 shadow/point shadow/transparent
  等全部 GPU 测试零改动通过；新 `csm_render.rs`：~80 m 大场景断言近/远
  级联物体脚下都有阴影（远处脚下带像素证据）、级联边界两侧阴影带连续
  （无接缝，窗口容差见测试注释）。
- **验收图**：新示例 `csm_snapshot`（iris-engine，输出 `docs/csm.png`）：
  近/中/远三距物体 + 大地面俯斜视——近处影子锐利、~104 m 处影子仍在、
  136 m 处（覆盖带外）正确无影、级联过渡带无跳变。
- **文档**：ARCHITECTURE §4.7.2（分割公式/纹素对齐原理/级联选择/覆盖
  距离外无影）、README 阴影条目同步。

### Added（R28：动画 v2——crossfade 混合 + 轻量状态机 + 动画事件轨道）

- **crossfade 姿势混合**（iris-render `animation`）：`AnimationPlayer`
  新增 `blend: Option<AnimationBlend>`（默认 None）与
  `transition(clip, duration)`——目标剪辑正常推进、源姿势在过渡开始时
  **冻结快照**（可预测、可测试：混合结果只由 elapsed 决定，且避免源
  循环回绕的姿势跳变）；权重 `smoothstep(elapsed/duration)`（首尾速度
  连续，中点与线性同为 0.5）；对「目标实体 + 路径」并集逐项混合：
  平移/缩放 lerp、旋转 **slerp**（nlerp 大角度中段角速度不均，取舍
  写入注释），仅一方有的通道取该方值（**缺方通道不拖拽**）。
  `duration ≤ 0` = 硬切换、同剪辑请求忽略、目标越界忽略 + debug_assert；
  elapsed ≥ duration 自动清除回纯目标。混合与 time 同步在系统阶段一
  推进——姿势与分帧方式无关（GPU 测试守护「中点帧严格介于两极值之间」，
  程序化构造 yaw 0° / 80° 双剪辑直接 API）。
- **轻量状态机**：`states`（clip + looping + speed）/ `edges`
  （from → to + 过渡时长）/ `current_state`，全部默认空 = 不启用，
  直接操作 `active/looping/speed` 的旧路径完全保留。
  `request_state(state)`：有 `current → state` 边按 `edge.duration` 启动
  crossfade、无边硬切换，`looping/speed` 同步为目标态（states 非空时
  `advanced` 以当前状态参数为准）；**条件判定留给游戏侧**（无参数黑板，
  v1 边界写入文档）。
- **动画事件轨道**：`AnimationClip.events`
  （`AnimationEvent { time, name }`；事件不在 glTF 标准内，GLB 解析
  不产事件，API 侧 `clip.events.push` 构造）+ `player.pending_events`
  + `take_events()`（**消费方每帧 drain** 的帧内消息契约）。跨界检测
  纯函数 `collect_events` 单独导出可单测：非循环 `(prev, new]`——停在
  末端后区间为空、**末端事件不重复触发**；循环传**取模前原始时间**，
  事件每被跨越一次触发一次、一帧跨 k 圈 **multi-fire k 次**（确定性：
  `⌊(new−t_e)/dur⌋ − ⌊(prev−t_e)/dur⌋`），输出恒按触发时刻升序。
  CPU 单测：slerp 中点 45°（1e-5）、smoothstep 精确式、回绕 / multi-fire /
  时序、同配置双 player 逐位一致。iris-render 不依赖 audio——「脚步声」
  联动以模块文档 ignore 示例交付，运行时联动留待游戏轮。
- **兼容性**：全部既有动画测试（anim_render / skin_render / gltf_render
  及 lib 内动画单测）零逻辑改动通过；`AnimationPlayer` 新字段全部经
  `Default` 给中性默认，`AnimationClip` 仅 glTF 解析与既有单测的构造
  字面量机械补 `events: Vec::new()`，断言未动。

### Added（R27：后处理链 v1——曝光控制 + pass 串联框架 + 暗角示范 pass）

- **曝光控制**：iris-render 新资源 `ExposureSettings { exposure }`
  （Default 1.0；渲染器 `try_resource` 直读，与 `BloomSettings` 同款
  模式）。线性 HDR 在 ACES 色调映射之前乘曝光（`blit.wgsl` 的
  `hdr * blit_params.y`，替换原 `* 1.0` 占位）；**资源缺席 = 1.0 中性**，
  `DefaultPlugins` 不强制注册，缺席画面与无曝光控制逐位一致
  （1.0 乘法逐位中性的数学保证，GPU 测试逐位断言）。负值按 0 钳制；
  曝光 0 全黑（ACES(0)=0）。GPU 测试：同场景（HDR 亮源）曝光
  0.5 / 1.0 / 2.0 三帧中心亮度严格递增（ACES 正半轴严格单调）。
- **pass 串联框架 v1**（`post.rs`，防过度设计——编译期有序链，不做
  运行时注册 / trait 对象注册表 / 拓扑排序）：全屏后处理 pass =
  `PostPassDesc`（管线 + 组 0 布局 + 参数 uniform + 源 + 目标槽位），
  编码统一走 `encode_post_pass` 单一函数——泛光三段循环与暗角的
  begin_pass / set_bind_group / draw 样板只此一份。中间纹理由
  `PostScratch` ping-pong 分配器按**分辨率等级**各缓存一对 Rgba16Float
  纹理（原泛光半分辨率 a/b 缓冲泛化；命中同尺寸跨帧复用、尺寸变化
  整体重建，全分辨率对由暗角按需惰性分配），泛光槽位交替由纯函数
  `bloom_blur_slots` 单点定义（任意轮数输出恒回 A = blit 固定采样槽）；
  CPU 单测守护「半/全分辨率换算含 1×1 钳制」「缓存命中须尺寸精确
  一致」「槽位不读写同槽、链连续、落点恒 A」。blit 全链共享参数
  `PostParamsRaw`（16B：x 泛光 / y 曝光 / z 暗角 / w 保留，POD +
  WGSL 对齐断言）。
- **暗角示范 pass**（`shaders/vignette.wgsl` + 新资源
  `VignetteSettings { strength }`，证明框架可扩展）：全分辨率
  HDR → HDR 单输入 pass，UV 距中心 `smoothstep(0.35, 0.75, d)` 压暗
  四角、中心逐位无损。链位 = 泛光之后、blit 之前（tonemap/blit 是链
  的固定终点，暗角在线性 HDR 域作用）；**暗角先于曝光**——镜头物理
  遮挡先于传感器增益（顺序理由写入 shader 与 ARCHITECTURE §4.11）。
  强度 0 / 资源缺席时整段跳过、blit 直读主解析缓冲（绑定组与引入前
  逐项一致），GPU 测试逐位断言「0/缺席 ≡ 无 pass 帧」与「角区线性
  均值显著低于中心区」。
- **默认像素零变化（提交门槛）**：offscreen / hdr / bloom / transparent /
  particles / culling / skin / shadow / ui 全部既有 GPU 测试**未改动
  一行**、直接全过。
- **验收图**：新示例 `post_snapshot`（iris-engine，风格同
  bloom_snapshot）——同一 HDR 场景 × {欠曝 0.4、标准 1.0、过曝 2.5 +
  暗角 0.9 + 强泛光} 三帧，ffmpeg hstack 合成 `docs/post.png`。
- 文档：ARCHITECTURE §4.11 改写为「后处理链 v1」（pass 链顺序图、
  资源缺席语义、新增 pass 步骤清单以 vignette 为模板）；README 渲染
  特性行与验收图补充。

### Added（打磨 III：引擎 API 打磨 III——闭合 Iris Bastion dogfooding 的五条引擎摩擦）

延续「打磨 I / II」模式：**引擎侧修复 + Bastion 侧绕行删除验证**——每条
修完 iris-bastion 测试仍绿才算闭合（headless 7 项 + 全部单测通过）。

- **① 屏幕射线辅助 + 视口尺寸可读**：iris-render camera 模块新增
  `Camera::screen_ray(ndc, aspect) -> (origin, dir)`（NDC 点 → 世界射线：
  vp 逆对近 / 远平面各取一点、两点连线即针孔射线，数学推导写入注释；
  透视除法奇异时方向归零、由调用方按「射线无效」兜底）与实体相机版
  `screen_ray_from_global(gt, cam, ndc, aspect)` + `view_proj_from_global`
  （视图复用 `view_from_global`，投影 DirectX 深度约定与渲染器同款——
  世界 → 屏幕的正向投影与屏幕 → 世界的反投影共用同一矩阵，引擎外不再
  手工拼 vp）；单测覆盖「NDC 中心射线 ≈ eye→target」「左右缘射线发散
  且与 NDC x 符号一致」「实体版原点 = 全局位置、与正向投影互逆」。
  iris-engine 新资源 `Viewport { width, height }`（缺省 1280×720，与
  缺省窗口一致）入 `DefaultPlugins`：`WindowPlugin` 以窗口初始尺寸首次
  插入，此后主循环每次 `WindowEvent::Resized` 自动同步（照 `Input` 资源
  的模式）；离屏 / headless 程序没有窗口事件，手动
  `insert_resource(Viewport::new(w, h))`（资源文档写明）。
  Bastion 侧绕行删除：`BastionViewport` 资源、`DEFAULT_VIEWPORT` 常量、
  手工拼 vp 的 `active_camera_vp` 与错误分类 `BastionPickError` 全部
  删除，拾取 / `world_to_screen` 改走引擎 `screen_ray_from_global` /
  `view_proj_from_global` + `Viewport`（headless 路径由
  `setup_resources` 兜底插入缺省视口，窗口路径以框架维护的为准）。
- **② 粒子重置 API**：`ParticleEmitter::reset()`——清空活粒子池、
  `burst_fired` 归 false（burst 重新上膛）、发射余量 / 发射器年龄 /
  伪随机计数器与命令式进账一并归零，**参数字段不动**的前提下与刚构造
  的同参数新实例逐位等价；`trigger_burst(count)`——命令式进账在下一次
  `advance` 一次射出（dt = 0 亦触发、多次调用累加、消耗后不补发，不改
  `burst_count` 配置语义）。单测：reset 后内部状态与活粒子行为和新实例
  逐位一致 + 爆炸 burst 重新上膛；trigger_burst 的位置 / 确定性 / 累加 /
  不补发。Bastion FX 池触发处改「参数全字段覆写 + `reset()` 就地复用」
  （「换挂整只新发射器组件」的绕行删除；六个 `*_emitter()` 构造函数
  改写为 `configure_*(&mut)` 全字段写入函数，池结构不动）。
- **③ 粒子收集查 Visibility**：渲染器 `collect_particles` 查询带
  `Option<&Visibility>`（缺席 = 可见，与网格 / 透明收集同语义），挂
  `HIDDEN` 的发射器整池跳过；隐藏只是渲染过滤——`update_particles`
  照常推进，粒子继续老化。GPU 测试：HIDDEN 后 `particles_drawn = 0`、
  `draw_calls = 0` 且画面失去全部粒子像素（帧差分断言），恢复
  `VISIBLE` 原样重现 320 颗。
- **④ Visibility 层级级联（正确性核心）**：渲染收集期沿 `ChildOf`
  父链上行遍历（`hidden_by_ancestor`，每实体 O(父链深度) 典型 ≤ 4、
  防御性深度上限 64——不加新组件、不动 `hierarchy` 数据），任一祖先
  `HIDDEN` ⇒ 子树全隐藏；普通 / 蒙皮 / 透明（含透明蒙皮）三条几何
  收集路径与粒子发射器全覆盖；`visibility.rs` 模块头文档置顶改写
  （旧「过滤逐实体、不级联」说明替换为级联语义）；可见性过滤不计入
  `FrameStats.culled_instances`（口径不变）。GPU 回归测试复刻 Bastion
  之坑——根 rig（HIDDEN）→ 子夹层（无网格无 Visibility）→ 孙子网格：
  隐藏时 `instances = 0` 且零像素，根恢复可见则入画，夹层隐藏同样整树
  隐藏（级联对每一层祖先生效）。Bastion 侧：塔池 / 敌池**还原**「根 →
  形态 rig → 网格」三层结构（打磨 III 前级联缺失、孙子网格恒渲染，
  被迫拍平成「网格直接挂根 + 名字含段识别形态」——即 draw calls 139
  事故的绕行），形态切换回到「翻两个 rig 的可见性」；幽灵射程盘删除
  逐实体 `Visibility`，改由层级级联随根一并隐藏（顺带修复幽灵待命时
  射程盘滞留画面的缺陷）。快照 draw_calls 新旧对比见 README 的
  bastion.png 目检记录。
- **⑤ 同构池 type alias 警示**：`World::insert_resource` 文档新增模式
  警示（资源按 `TypeId` 寻址——同型多实例资源互相整个覆盖且编译期 /
  运行期均不报错，症状是「池凭空消失 / 数据串味」；同构池必须是
  distinct newtype，type alias 只是同一类型的别名）；ARCHITECTURE 的
  iris-bastion 段同步记录该模式。Bastion 代码不动（敌 / 弹 / 臼炮三池
  本就是三个独立 newtype，此次只是把踩坑经验沉淀进引擎文档）。

### Added（R26：游戏三 dogfooding——塔防《Iris Bastion》，R21–R25 五能力整体压榨）

- **新下游示范游戏 III：`crates/iris-bastion`**（塔防 Iris Bastion，依赖
  纪律同前两作：iris-engine 门面 + glam，引擎侧零改动）。俯视固定视角：
  敌人沿 7 段 6 弯航点折线进攻核心，玩家用击杀积分在 20×14 网格放塔
  （哨兵速射单体 / 臼炮落点 AOE），5 波全清获胜、核心 10 HP 归零告负；
  疾行虫 / 重甲兽双敌种、5 波写死构成、波间歇 5 s。
- **纯逻辑核心**（`logic.rs`，只 glam、零引擎依赖）：放塔校验三分支
  （界内 / 空格 / 积分）单独导出纯函数；索敌策略选型「射程内路径进度
  最大」（并列取列表序，确定性）；哨兵弹扫掠命中（与引擎
  `segment_hits_shape` 同款防穿模语义的本地 XZ 实现，含高速穿模回归
  单测）；臼炮弹 = 目标位置快照 + 引信 + 落点半径 AOE；航点匀速推进
  （进度即位置，`path_point` / `path_dir` 派生）；经济闭环（初始 100 =
  两哨兵、击杀赏金入账、放塔扣款）；`BastionInput { place_requests, dt }`
  纯核心不碰鼠标；同输入两遍逐采样 bit 级确定性单测。
- **逻辑事件自带世界坐标**（`BastionEvent`：开火 / 命中 / 爆炸 / 死亡 /
  漏怪，每帧 `step` 开头清空）：多塔同帧开火的定位需求下，arena 的
  「单调计数器差分」不含位置信息，改用事件列表（顺序 = 结算顺序，
  确定性），表现层零反查、无下溢陷阱。
- **鼠标拾取全链路（R21 压榨）**：`Input::cursor_position()` 像素 →
  NDC → 相机 `GlobalTransform` + `ActiveCamera` 手工拼 vp 逆 → 世界射线
  → `iris_core::physics::raycast` 打地面静态 Collider（顶面对齐 y = 0 的
  独立纯碰撞体实体，渲染网格与拾取几何分离）→ 网格坐标；`world_to_screen`
  逆变换公开（headless 测试以「格心 → 像素 → 射线 → 同格」往返闭环
  自检）。
- **幽灵预览（R23 压榨）**：待放塔身 `AlphaMode::Blend`（`MaterialDef::
  glass`）可放蓝 / 不可放红按帧换挂 + 半透明射程指示盘（单位圆柱按塔种
  射程缩放）；数字键 1 / 2 / 右键切换塔种。
- **粒子 FX 池（R24 压榨）**：12 个发射器槽位环形复用（预挂禁用发射器，
  触发 = 槽位移到事件坐标 + 整体覆写新发射器组件——`burst_count` 一次性
  无重置 API 的官方文档复用方式）；枪口闪光 / 命中火花 / 臼炮烟尘 / 落点
  爆炸（explosion 预设收紧版）/ 死亡爆炸（explosion 预设）/ 漏怪核心
  告警六类 burst 全部确定性。
- **空间音频（R25 压榨）**：六种程序化音效（正弦 / 噪声 / 双音 concat）
  经 `play_spatial` 定位到事件世界坐标（同类型同帧去重取末件位置），听者
  每帧跟随俯视相机（`set_listener` + `from_view` 正交化，headless /
  离屏路径不注册 SpatialPlugin 同样正确）；距离参考按场域放大到 25 m。
- **对象池 IV 件套**：塔池（40，逻辑列表下标直映射——塔从不移除）、
  敌池（24）/ 哨兵弹池（32）/ 臼炮弹池（8）（稳定 id 映射，先归还后
  取用）、FX 池（12，环形）；全部启动预热、运行期零 spawn / despawn，
  池守恒（活跃 + 闲置 = 容量）由 headless 测试守护。**三池必须为三个
  独立 newtype**——资源按 TypeId 寻址，type alias 同型会互相覆盖
  （dogfooding 实测踩坑，见下）。
- **headless 集成测试 7 项**：四池预热与 HUD、幽灵预览红蓝 / 塔种切换 /
  离场隐藏、鼠标点击放塔全链路（含同格重复与积分不足拒绝）、空间音频
  听者跟随 + 声源入表、击杀爆炸粒子 FX 槽位触发与自然熄灭、混战帧
  draw calls < 300 且 `culled_instances > 0`（界外岩柱 = 剔除量化载荷）、
  **脚本化自动玩家**：预设放塔序列经鼠标点击全链路放置，真实模拟跑完
  5 波 **Victory、核心 10/10 无损、53 杀 0 漏怪、8 座塔、83 s 游戏时间**
  ——经济 / 索敌闭环可玩的量化证据（波次曲线逐波打印）。
- **离屏快照**：`examples/bastion_snapshot.rs`——中期战局（4 塔 + 第 1 波
  敌群在途 + 击杀爆炸 + 臼炮幽灵预览）640×360 单帧，`FrameStats` 打印并
  断言 `culled_instances > 0`；`ffmpeg` 转 `docs/bastion.png`。
- **文档**：README 游戏 III 章节 + 项目树 + 快照目检记录；ARCHITECTURE
  crate 图与下游消费者清单补 iris-bastion 段。
- **dogfooding 撞到的引擎摩擦（本轮原则：引擎侧零改动，全部记录 +
  游戏内绕行，留待下一轮打磨）**：① 无「屏幕 → 射线」辅助且实际窗口
  尺寸不作为资源暴露（`WindowConfig` 只存初始配置）——插件手工拼 vp 逆
  + 视口尺寸资源缺省 1280×720 绕行，建议 iris-render 补 `screen_ray` /
  视口资源；② `ParticleEmitter` burst 一次性无重置 API（文档已注明 v1
  限制）——FX 池以「覆写整只新组件」绕行，建议补 `reset()` /
  `trigger_burst()`；③ 粒子收集不查实体 `Visibility`（隐藏发射器的活
  粒子照画）——本作无用例受害，记录为 v1 语义缺口；④ `Visibility` 不向
  子树级联且**组件缺席 = 可见**——池槽位「无网格 rig 夹层 + 孙子网格」
  结构下孙子恒渲染（待命池实体堆在原点入画，draw calls 139 → 拍平层级
  后 18），游戏侧改为「网格直接挂根 + 名字含段识别形态」，建议文档在
  visibility 模块置顶强调「过滤逐实体、不级联」；⑤ 同构池 type alias
  共享 TypeId 互相覆盖（上条）——引擎无从阻止，属游戏侧设计陷阱，记录
  为模式警示。

### Added（R25：3D 空间音频 v1——距离衰减 + 恒功率立体声声像 + ECS 组件同步系统）

- **混音核心空间化**（`iris-audio/mixer.rs`，新增 `glam` 依赖）：`play_spatial`
  在声音槽位上记 `spatial` 标记 + 声源世界坐标；`set_listener(ListenerParams)`
  写听者位姿（`position / forward / right`，`from_view(position, forward, up)`
  辅助自动正交化——俯仰俯视的相机得到水平右轴，声像只随水平方位摆动；
  默认原点 / -Z 前 / +X 右，与引擎右手系一致）；`set_sound_position` 播放中
  移动声源（世代校验防悬垂）。增益模型为**纯函数 `spatial_gain`**（单独导出
  可单测）：距离衰减 `att = ref / (ref + d)` 软倒数（d=0 → 1、远处平滑趋零、
  无硬截断；参考距离默认 4 m，混音器级可配、按声音可覆写）；声像
  `pan = clamp(dot(单位源方向, 听者右轴), −1, 1)` → 恒功率映射
  `θ = (pan+1)·π/4`、左 = `att·cos θ`、右 = `att·sin θ`（−1 全左 / 0 居中
  0.707 / +1 全右，总功率守恒）；重叠 / 正上取 pan = 0 居中。`mix` 每帧按
  **当帧听者位姿**对空间槽位重算增益（单声道源立体声输出按恒功率拆左右；
  **立体声剪辑 v1 只乘总增益**、不做逐声道声像；非空间 `play` 恒居中无衰减、
  行为逐位不变）。**已知限制（文档写明）**：无 HRTF / 耳间延迟差，源在正
  后方与正前方对称——身后不消音、前后不可区分。
- **Audio 门面转发**（iris-engine）：`play_spatial` / `set_listener` /
  `set_sound_position` / `set_reference_distance` /
  `set_sound_reference_distance` 与混音器直操同语义，headless 与设备模式
  一致（回调 try_lock 机制不变）。
- **ECS 组件与系统**（iris-engine 新模块 `spatial.rs`）：`SpatialListener`
  标记组件（每世界取查询序第一个，位姿来自实体 `GlobalTransform`，前方 =
  实体 -Z 轴经世界 +Y 重正交化）；`SpatialSource` 持续声源组件（剪辑 /
  音量 / 循环 / 距离参考 + 内部 `SoundId` 状态，v1 定位为循环环境声）。
  `update_spatial_audio` 系统三步：起播 → 逐帧同步位置 → **组件移除 =
  帧间差分**（上一帧活跃的「实体 bits → SoundId」快照挂在 `Audio` 资源上，
  本帧查询检不到即 `stop`；实体 bits 作键防悬垂安全——despawn 后复用下标
  的新实体代数必变，绝不误停）。`SpatialPlugin` 注册于 **PostUpdate、层级
  传播之后**（与 `ParticlePlugin` 同层同理由：听者 / 声源读当帧全局位姿；
  访问集与粒子不相交可同批并行），已入 `DefaultPlugins`。**一次性空间
  音效不走组件**：游戏代码直接 `audio.play_spatial(clip, pos, vol, false)`。
- **Iris Arena dogfooding**（iris-arena）：开火音定位**枪口**（弹道出生点）
  、命中音定位**受击敌人**（受击闪烁最强的在场敌人——本帧命中者闪烁恰为
  满值、上一帧已衰减，max 自然优先本帧；击杀的敌人当帧即被移除，全无
  痕迹时回退玩家位置，音效照发仅定位退化）；听者每帧跟随**俯视场景相机**
  （位姿取相机 `GlobalTransform`、悬于场地上方，pan 由 XZ 平面方位决定；
  直接 `set_listener` 而非挂组件——headless / 离屏路径未注册
  `SpatialPlugin` 同样得到正确听者）；竞技场尺度把距离参考放大到 25 m
  （`ARENA_REFERENCE_DISTANCE`——俯视相机听者悬空 ~23 m，默认 4 m 会让
  全场音效衰减到不可闻）。受伤 / 开波音保持居中（自身反馈 / 全局播报，
  定位无信息量）。headless 集成测试断言听者位姿跟随相机 + 空间声入表
  （槽位状态而非实听）。
- **测试**：iris-audio 混音器单测 10 项新增（衰减解析 ref=4、d=0/4/12 →
  1/0.5/0.25 精确断言；恒功率声像正右 / 正左 / 正前 / 重叠 / 正上；
  `from_view` 正交化与退化输入；混音增益合成 volume × att × 恒功率逐项
  对照；同配置混音 bit 级确定；`set_sound_position` 世代失效返回 false；
  左右声道能量比与源方位一致；立体声剪辑仅总增益语义锁定；非空间播放
  不受听者影响；距离参考默认 / 覆写 / 病态兜底）；iris-engine 5 项
  （源添加起播且不重复、位置逐帧同步镜像能量、despawn / 摘组件下一帧
  停止 + 快照归零、听者取查询序第一并逐帧跟随 + 无 Audio 安全、
  GlobalTransform 缺席跳过 + clone/detach 句柄语义）+ Audio 门面 headless
  空间转发 1 项；iris-arena headless 1 项（听者跟随相机 + 交火声入表）。
- **文档**：ARCHITECTURE §4.14「空间化 v1」小节（公式 / 已知限制 / 组件
  差分语义）、§8 已知限制第 11 条同步；README 音频特性行与路线图。

### Added（R24：粒子系统 v1——Emitter 组件 + CPU 确定性模拟 + billboard 实例化渲染）

- **组件与模拟**（`iris-render/particles.rs`）：`ParticleEmitter` 组件
  （混合模式 / `spawn_rate` 持续发射 / `burst_count` 一次性爆发 /
  `emitter_lifetime` 发射器寿命 / 粒子寿命 / 初速与方向抖动 / 重力缩放 /
  尺寸与颜色起止线性插值 / `max_particles` 池上限）+ 私有活粒子池。
  发射原点 = 实体 `GlobalTransform` 平移，粒子出生即存世界坐标（发射器
  移动天然留拖尾）。模拟核心 `advance` 为纯函数（不触世界，可脱离世界
  单测）：半隐式欧拉积分 → 寿命剔除 → burst → 持续发射。**确定性**：
  出生方向由 splitmix64 哈希流派生（发射器内全局递增计数器 ⊕ 实体
  `Entity::to_bits`，无 rand 依赖、无全局状态），同一输入两次模拟逐位
  一致（单测按位型比对守护）；持续发射用累计分数法（rate·dt 取整、
  余量滚存），任意 dt 序列出生总数收敛于 rate·总时长。**池满语义**：
  丢弃新粒子（保老弃新，不挤掉旧粒子）；发射器寿命耗尽停发（已出生
  粒子存活到终点）；burst 首次 update 一次性触发（不受寿命门控、dt=0
  亦触发）。预设构造 `ParticleEmitter::fountain()` / `explosion()`。
- **系统与编排**：`update_particles(world)` 单遍系统（模拟只写发射器
  自身内部粒子池、无跨组件借用，无需 `update_animations` 式两阶段）；
  `Time` 缺席 dt = 0。`ParticlePlugin` 注册于 **PostUpdate、
  `HierarchyPlugin` 之后**（发射原点取当帧传播后的全局位置），已入
  `DefaultPlugins`；prelude 导出 `ParticleEmitter` / `ParticleBlend` /
  `Particle` / `ParticleInstance` / `AdvanceStats` / `update_particles`。
  场景序列化 v1 留白（JSON 场景无粒子段）。
- **渲染**（`renderer.rs` 粒子 pass + `shaders/particle.wgsl`）：实例 =
  (世界中心, 尺寸) + 线性 RGBA（32 字节/粒）；顶点阶段 `vertex_index`
  展开 4 顶点 triangle-strip（无顶点缓冲），billboard = center +
  (right·x + up·y)·size——相机 right / up 从视图矩阵行提取进 32 字节
  小 uniform（每帧 write_buffer）；片元径向软衰减 `(1 − r²)²`（无硬
  方块）。**双混合管线**：additive（`SrcAlpha + One`，加法交换律不
  排序）与 alpha（`ALPHA_BLENDING`，粒子按视深降序、`total_cmp` +
  收集序号并列断言）；都深度测试开 / 写入关（照透明管线参数）。**pass
  顺序**：透明 pass 之后、泛光之前——粒子叠加在玻璃等透明结果上，高亮
  additive 粒子参与泛光；无活粒子零开销。实例缓冲独立、容量不足 2 倍
  扩容重建，**每帧全量上传**（粒子逐帧全变，增量比对无价值）。
- **剔除与统计**：剔除按发射器包围球（活粒子 AABB 包围球 + 最大尺寸
  边距，R22 同款保守界），被剔发射器整批不画且不计入 `culled_instances`
  （口径为几何实例）；可见粒子计入 `instances` / `draw_calls` /
  `triangles_drawn`（每粒 2 三角形），`FrameStats` 新增
  `particles_drawn`（= 各可见发射器活粒子数之和，可与池状态对账）。
  粒子不进阴影 pass（与透明同理由）。
- **测试**：CPU 单测 7 项（240 步逐位确定性、发射计数精确 / 余量滚存 /
  大步长、半隐式欧拉解析对照、寿命剔除 / 发射器耗尽 / 池满、burst
  一次性、哈希流方向健全性、渲染插值）；GPU 集成 `particles_render.rs`
  3 项（喷泉可见性 + `particles_drawn` 与池状态对账、40 颗重叠粒子
  additive 亮度显著高于 alpha 混合的可区分断言、爆炸 one-shot 全灭后
  不补发）。无 GPU 环境优雅跳过。验收图 `docs/particles_fountain.png`
  / `docs/particles_explosion.png`（`particle_snapshot` 示例：喷泉持续
  60 步成像 / 爆炸 burst + 0.3 s 扩散成像）。
- **文档**：ARCHITECTURE §4.17 粒子系统小节（确定性 / 双混合 / billboard /
  pass 顺序 / 池满语义），§4.1 帧流程、§4.4.1 FrameStats 口径表、§8 已知
  限制同步；README 特性行 / 示例列表 / 路线图。

### Added（R23：透明渲染——alpha 混合材质 + 视深排序透明 pass）

- **材质透明模式**（`iris-render/material.rs`）：`AlphaMode { Opaque, Blend }`
  （`Default = Opaque`）；`MaterialDef` 新增 `alpha_mode` 字段（旧构造路径
  与既有示例不受影响）；玻璃类构造辅助 `MaterialDef::glass(r, g, b, a)`
  （alpha 混合 + 低粗糙度绝缘体，混合系数取 `base_color.a`）。
- **管线变体**（`iris-render/pipeline.rs`）：透明普通 + 透明蒙皮两条管线，
  与不透明变体共用同一份 WGSL / 顶点布局 / 背面剔除 / 深度格式，仅两点
  不同——`BlendState::ALPHA_BLENDING`（`src α, 1 − src α`，α = 着色器输出
  alpha）+ 深度**测试开（Less）、写入关**：写入会把深度序更远但尚未绘制
  的透明面错误裁掉（共面透明对必然丢失其一），只测不写则透明仍被不透明
  几何正确遮挡。
- **收集与排序**（`iris-render/renderer.rs`）：`collect_instances` 按材质
  `alpha_mode` 分区——不透明路径语义完全不变；透明实例区间追加在全部
  不透明之后（实例缓冲 `[不透明普通 | 不透明蒙皮 | 透明普通 | 透明蒙皮]`）。
  透明项按**视深降序（远 → 近，back-to-front）**排序：深度 = 包围球中心
  经视图矩阵后的 `-z`（无 bounds 回退模型平移列；透明蒙皮取关节变换并集
  AABB 中心），深度并列按实体 bits 升序（比较器全序，结果与查询顺序
  无关）；深度序中相邻同 (网格, 材质) 键合并为 run 批。无透明实例时
  透明 pass 整体跳过（零开销）。
- **透明 pass**：位于不透明主 pass 之后、泛光 / 色调映射之前，写同一
  HDR MSAA 目标（`LoadOp::Load` 继续混合）——透明参与 HDR / 泛光 /
  ACES；深度附件同样 Load，保留不透明遮挡（主 pass 深度附件因此从
  `Discard` 改 `Store`——`Discard` 后附件内容未定义，DX12 实测透明整体
  消失，教训见 ARCHITECTURE §4.16）。**透明不进方向光 / 点光阴影 pass**
  （收集期分区排除）；视锥剔除与 FrameStats 对透明同样生效
  （instances / culled / triangles 口径含透明）。
- **接入**：glTF 材质 `alphaMode: "BLEND"` → `Blend`（`"MASK"` 需 alpha
  cutoff，v1 按不透明处理）；场景 JSON 材质段新增 `alpha_mode` 字段，
  旧场景缺席反序列化为 `Opaque`（向后兼容），不透明序列化省略（旧文件
  重存字节级不变）。
- **测试**（GPU 集成 `transparent_render.rs` + CPU 单测）：混合正确性
  （自发光材质使 HDR 合成值可解析断言，参考渲染法）、排序正确性（远蓝 /
  中绿 / 近红三层 α=0.5 合成 = `(αE, α²E, α³E)`，顺序颠倒 r/b 通道互换
  必挂）、深度写关（透过近板见后方不透明物与更远透明板；共面透明对
  两块都必须参与混合）、不投影（透明版开 / 关阴影差分 ≈ 0 + 不透明对照
  版差分成片排除假阴性）、run 合批（FrameStats 断言）、分区与视深排序
  CPU 单测（renderer.rs）、旧场景兼容与 Blend 往返（scene.rs）。无 GPU
  环境优雅跳过。验收图 `docs/transparent.png`（`transparent_snapshot`
  示例：玻璃罩内核 + 三色板纵深混合）。
- **文档**：ARCHITECTURE §4.16 透明渲染小节（分区 / 排序 / 深度写关 /
  不投影语义），§4.1 帧流程、§4.4.1 FrameStats 口径、§4.7 阴影、§8
  已知限制同步；README 渲染特性行与示例列表补透明渲染。

### Added（R22：蒙皮保守视锥剔除 + FrameStats 统计扩展 + culling_bench 基准）

- **蒙皮实例保守视锥剔除**（`iris-render/renderer.rs` `collect_instances`）：
  蒙皮实体不再「保守全收」——绑定姿势 AABB 的 8 角点经**每个**关节矩阵
  变换到当前网格局部、再经实例 model 矩阵到世界，全部（关节数 × 8）点的
  并集 AABB 与视锥做 `intersects_aabb`。数学保证只多画不漏画：LBS 顶点
  = Σ wᵢ·(Jᵢ·v)、权重非负和为 1，蒙皮后顶点落在各刚体变换结果的
  **凸组合**内，凸包必含于逐关节变换后角点并集的 AABB。代价为每实例
  关节数 × 8 次点变换（Arena 15 蠕虫 × ~5 关节量级可忽略）；网格无 CPU
  数据 / 空包围盒时退化为不剔除。实现细节：被剔除的蒙皮实体同样进入
  跳过集合——否则普通查询路径会把它再处理一次（双计 / 误画），CPU 单测
  抓出后修正。
- **FrameStats 扩展**：新增 `culled_instances`（本帧被视锥剔除的实例数，
  普通 + 蒙皮合计，`Visibility::HIDDEN` 过滤不算剔除）与
  `triangles_drawn`（主 pass 提交三角形总数 = 可见项索引数 / 3 累加：
  普通批 × 批实例数、蒙皮逐项；网格目录缺席按 0 处理不 panic）。口径
  文档化：主 pass 口径，不含 UI 与阴影 pass（阴影复用主 pass 实例区间）。
  `collect_instances` 返回值由 `u32` 改为小结构 `CollectCounts`
  （visible / culled），trace 日志同步。
- **测试**（GPU 集成 `culling_render.rs` + CPU 单测）：统计一致性——
  10 立方体两排、相机 180° 对调：`visible + culled == N`、
  `triangles_drawn == visible × 12`（精确断言）、`draw_calls` 与批数
  一致、红像素侧证且对调后（6 可见 > 4 可见）像素增多；蒙皮反误杀回归
  ——关节全局平移 +X 10 m、ibm 恒等 → J = T(10,0,0)，绑定 AABB 在原点
  视锥外而蒙皮几何在 (10,0,0) 视锥内：看向时实例必须可见且画面出现
  红像素、背对时计入 `culled_instances` 且画面全无——误用绑定包围盒
  剔除时这两组断言必挂。
- **culling_bench 示例**（量化验收）：5000 立方体（100×50 网格、同网格
  同材质单批）双朝向各 300 帧（前 50 预热丢弃），输出 FrameStats 全字段
  + `Renderer::draw` 平均 / 最坏 CPU 耗时——`Instant` 只包 draw 调用，
  离屏读回不计（`OffscreenRenderer` 新增不拷贝读回缓冲的 `draw_frame`）。
  开发机实测：面向全场 4730 可见 / 270 剔除 / 1 draw call / 56 760 三角
  形、draw 平均 ~0.52 ms 最坏 ~0.99 ms；背对望空 0 可见 / 5000 全剔除 /
  0 draw call、draw 平均 ~0.36 ms。
- **文档**：ARCHITECTURE §4.4 补蒙皮保守剔除（凸组合论证）与 §4.4.1
  FrameStats 字段口径表 + 基准实测记录（标注开发机、仅供量级参考），
  §4.6.2 绘制路径与限制条目同步；README 渲染特性行补蒙皮剔除与帧统计。

### Added（R21：物理查询 + 均匀网格 broadphase + Arena 扫掠命中 + CI）

- **物理查询 API（iris-core/physics）**：射线 / 线段查询一族纯函数——
  `ray_sphere`（几何法解二次方程）、`ray_box`（slab 法，平行轴按「起点在
  界内则不裁剪」处理，盒内起点取最浅出面外法线、角点斜射法线取主轴，
  退化情形全部写明确定性选择）、`ray_vs_shape`（形状分发 + `t ≤ max_t`
  裁剪）、`segment_hits_shape`（线段便捷封装：`dir = to − from`、
  `max_t = 距离`）与 `RayHit { t, point, normal }`；世界级
  `raycast` / `raycast_filtered`（最近命中优先、t 并列取存储序先遍历者、
  filter 闭包只收 `Entity`——迭代期间持有读借用，文档写明不得访问世界）。
- **球扫掠查询（角色控制器地基）**：`sphere_sweep`（保守推进二分 24 次
  收敛，复用 `detect_pair`；返回首次碰撞位移分数 t∈[0,1] 与阻挡法线，
  「无重叠侧」边界减 epsilon 收缩留皮肤）与 `sphere_sweep_statics`
  （世界静态体收集封装）；检出范围（终点重叠括弧）的诚实边界写进文档。
- **均匀网格 broadphase（iris-core/broadphase）**：`BroadphaseGrid`
  （`insert_aabb` / `candidates` 升序去重）——**BTreeMap 而非 HashMap**
  （键字典序迭代 = 确定性，引擎物理为 bit 级确定性，模块文档写明选型
  原因）；`step_physics` 检测阶段由 O(n²) 全配对改为网格筛候选：cell
  尺寸取全部非静态体最大轴向跨度钳制 [1.0, 50.0]（静态大地面不参与
  定尺寸——防 cell 撑到场景尺度；无动体回退全体最大），候选升序 +
  `j > i` 过滤使配对遍历序与原 O(n²) 双层循环**完全一致**，既有 bit 级
  确定性测试无需改动即守护等价性；单测含确定性 LCG 生成的 ~200 AABB
  保守超集断言（不引入 rand 依赖）。
- **iris-arena 弹道改扫掠判定**：命中敌人与静态阻挡都改用本帧
  `prev → pos` 线段扫掠（本地 XZ 平面实现，纯核心保持零引擎依赖）——
  修复高速穿模漏检（弹速 × dt 步长可大于敌人直径）；当帧点判定的
  `overlaps_static` 删除；新增「每帧位移 > 敌人直径 2 倍仍 1 s 内命中」
  穿模回归测试（含逐帧采样点距敌均 > 判定半径的场景自检）。
- **测试**：iris-core 新增射线（球面精确 t/法线 / 背面 / 球内起点、盒面 /
  盒内 / 角点主轴法线、线段穿模）、世界级 raycast（最近优先 + filter
  排除发射者 + max_t 裁剪 + 空世界）、球扫掠（畅通 / 正碰 t 分数与
  法线 / 斜掠角点不误报 / 起点重叠 / 世界封装跳过动态体）、broadphase
  （往返 / 负坐标 / 确定性 / LCG 保守超集）、100 球密集阵 step 后两两
  不重叠。
- **CI**：`.github/workflows/ci.yml`——三平台（windows / ubuntu / macos）
  测试矩阵 + wasm32 编译检查 + fmt / clippy 双目标（native + wasm32）
  `-D warnings` 门禁；GPU 相关测试在无 GPU 环境自动跳过（文件头注明）。

### Changed

- `step_physics` 检测阶段：配对来源从 O(n²) 双层循环改为网格候选
  （遍历序等价，见上）；模块与架构文档的「O(n²) 检测」限制条目改写为
  「AABB 网格快照」的诚实边界（求解期位置修正不重建网格——常规场景
  修正量远小于 cell 尺寸，候选保持保守超集）。

## 历史里程碑（R1–R20，按 git log 归纳）

- **R1** 引擎奠基：稀疏集 ECS（实体代数防悬垂 + 运行期借用检查 panic
  而非 UB）+ 阶段调度 + 变换层级 + wgpu 渲染器与 winit 窗口。
- **R2–R4** 渲染核心三连：实体挂载相机 + glTF 2.0 资产管线；方向光阴影
  贴图（正交深度 pass + 3×3 PCF）；PBR（Cook-Torrance）+ HDR/ACES 色调
  映射。
- **R5–R8** 光照体系：泛光（Bloom）后处理；IBL 环境光照（程序化天空 +
  分割求和）；天空盒背景 pass；外部 HDR 环境源（Radiance .hdr
  equirectangular 手写解析）。
- **R9–R10** 动画链路：GLB 二进制容器 + glTF 节点动画（AnimationPlayer）；
  骨骼蒙皮（GPU LBS + 蒙皮阴影）。
- **R11** 多线程系统调度：声明式读写集冲突分批并行（`std::thread::scope`，
  16 核实测 3.27× 加速）。
- **R12–R13** 点光阴影（cubemap，首灯 6 面归一化距离）；2D UI 叠加层
  （HUD：屏幕空间矩形 + 内嵌位图字体，单批单 draw）。
- **R14** wasm32 Web 目标端到端验证：全 workspace 编译零警告 + 真实
  浏览器渲染确认（Instant / 线程 / 异步 GPU 初始化平台封装）。
- **R15** 音频子系统：纯软件混音器 + cpal 设备层 + WAV 加载 + 程序化
  音效（wasm32 降级为纯混音器）。
- **R16** 实例缓冲 GPU 持久化与增量上传：静态场景零上传、动态实体仅传
  变化段。
- **R17–R18** dogfooding I + 引擎 API 打磨：Iris Collect 收集小游戏
  （第三人称限时收集）验证「只用公开 API 拼出完整游戏」，七条引擎缺口
  （可见性开关 / 阶段排序 / 并行发事件 / 按名查实体 / Input 注入 /
  Audio headless / 小项）全部修复闭合。
- **R19** 物理与碰撞 v1（Sphere/AABB 刚体：半隐式欧拉积分 / 检测 /
  分离反弹摩擦响应 / 迭代松弛，纯函数确定性求解器）+ 场景序列化
  （JSON 双向，登记表驱动的资源来源往返）。
- **R20** dogfooding II + 引擎 API 打磨 II：Iris Arena 波次生存竞技场
  （蒙皮动画 × 对象池 × 弹道 × 多游戏状态的高强度真实用法），七条
  API 摩擦（AnimationPlugin prelude / 资产路径 / 蒙皮实例关节树收编 /
  any-key / 音效拼接等）全部修复闭合。

## [0.1.0] - 初始版本

- Iris 3D 游戏引擎 v0.1.0：跨平台（wgpu + winit）、零 unsafe、稀疏集
  ECS、PBR + HDR 渲染管线。详见 ARCHITECTURE.md。
