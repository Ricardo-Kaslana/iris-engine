# C7 大裁决役 · 设计稿 v1（OIT + DDGI 动态光源与天空同源）

日期：2026-09-19 ｜ 状态：**已拍板，执行中**
前置：闭合战役计划 #5（OIT，§8.3 半透明排序条）+ #21（DDGI 动态光源 + 天空同源，§8.57①②）。
#20 光照口径（iris-exp ⑫）维持挂账，不在本批。

---

## 0. 总则

1. **两案均全 opt-in，缺席 = 逐位不变**。推论（普查实锤，见 §5）：
   全仓硬编码 FNV 基线场景均不含 Blend 材质、DDGI 现口径不吃点光/方向天空
   ⇒ 本批**预期零基线打破**。验收口径从「归因 + 重锁」**升级**为
   「**全部既有基线零改动原值通过**」——除非实现中发现外溢，届时按
   AGENTS 归因+重锁程序走并在此文档追记。
2. 公式纪律（C5 教训延续）：WBOIT 权重式、大气散射 CPU 镜像一律
   **照抄论文/现有 WGSL 原文**进模块文档，禁止凭记忆重写；WGSL↔CPU
   双源处加源码层看门（`DDGI_COVER_FADE` / `skybox_globals_prefix` 先例）。
3. 分两增量提交：**C7a · DDGI 同源役**（纯 CPU 侧 + 引擎接线，无渲染管线改动）
   → **C7b · OIT 役**（渲染路径重改动）。DDGI 先行：改动面小、可判别测试多，
   先落袋；OIT 是全部透明像素语义，风险后置单独一笔便于归因。

---

## 1. 案 A · OIT（§8.3 逐像素路线图项）

### 1.1 现状（已对码）

- 收集/排序：`collect_instances` 按 `AlphaMode::Blend` 分流（renderer.rs:2548），
  视深 = 包围球中心 −z，`sort_unstable_by` 全序比较器（renderer.rs:6681-6697，
  RT 相机同款 :10194）；凹体/交叠自遮挡排序误差 = 本债本体。
- pass 本体：`iris.pass.transparent`（renderer.rs:7947-8067）画进
  `hdr.msaa_view`（4× MSAA，Rgba16Float）+ resolve 到 `hdr.resolved_view`，
  深度只测不写（Depth32Float，Less）；混合态 = 经典 SrcAlpha/1−SrcAlpha
  （pipeline.rs:744-763）；`standard.wgsl fs_main` 输出**非预乘** alpha（:1439）。
- 帧序：主 → 贴花 → **透明** → 粒子 → scene_depth 副本 → 水体 → TAA（吃透明
  混合结果）→ 后处理链。水/粒子/贴花各有独立管线，不在本 pass。
- 资源约束：片元只读 storage 已 4/4 触顶（pipeline.rs:490-493，§8.57⑮）
  ⇒ OIT 数据必须走纹理；全仓无现成 Rg16/Rgba16 累积纹理可白嫖
  （PostScratch 是惰性对，语义不同不挪用）。

### 1.2 裁决点

- **A1 算法**（提请）：
  - **甲（推荐）Weighted Blended OIT**（McGuire & Bavoil, HPG 2013）：
    两张累积纹理（accum RGBA16Float：`rgb·w·a, w·a`；reveal R16Float：`Π(1−a)`），
    任意序加法混合 + 单遍片元，无原子依赖、wasm 兼容。近似算法——
    多层叠加能量有偏，但凹体/交叠的本债场景恰是其判别区。
  - 乙 per-pixel linked list：精确但需 storage 原子 + 帧内两遍扫描；
    片元 storage 4/4 已满 ⇒ 必须先申请 device feature（双 target 兼容面扩大），
    且帧内存池管理引入新确定性面。**拒**。
  - 丙 深度剥离：多 pass 成本 ×剥离层数，层数旋钮无解析上界。拒。
- **A2 覆盖范围**（提请）：
  - **甲（推荐）**：仅主透明 pass（`AlphaMode::Blend` 几何 + 蒙皮变体）。
    水（独立 pass，自身已有深度测试语义）、粒子（自排 + 加色为主）、
    贴花（不透明域前置）各维持原路，声明边界。
  - 乙：并入水体。水体读 scene_depth 做折射，OIT 化要连带重构其
    LessEqual+不写深语义与反射捕获，改动面 ×2。**不建议本轮**。
- **A3 RT 相机**：透明管线族有 RT 变体（renderer.rs:10391）。推荐本轮
  **主相机专属**，RT 相机维持 painter（声明边界——消费方出现再接）。

### 1.3 架构定稿（A1甲/A2甲/A3甲 前提下）

1. **开关资源** `OitSettings`（iris-render，`renderer.rs` 侧读）：
   `App` 插入 ⇒ 开；缺席 ⇒ 现有 pass 指令流**一帧不改**（所有 OIT 代码
   在 `if oit.is_some()` 之后，含纹理分配——惰性 `ensure_oit_targets`
   照 `ensure_scene_depth`（:5302）范式）。
2. **深度真值**：OIT 开启时新增独立「不透明深度快照」解析（透明 pass 前，
   MSAA 深度 → 单采样 depth 纹理；**不挪动既有 scene_depth 副本的位置与
   内容**，水体路径零感知）。gather 无深度附件，片元对快照手动测深
   （`frag_depth ≥ scene_depth ⇒ discard`）。
3. **gather pass**：新管线族（同 `standard.wgsl` 顶点 + 新片元输出：
   读 `fs_main` 同口径的 color+alpha，写 `w` 权重进双纹理，加法混合
   One/One；blend 态独立不动 `ALPHA_BLENDING`）；实例收集与合批口径不变，
   **不再需要视深排序**（排序分支在 OIT 开启时跳过——绘制序仍按
   collect 原序，确定性不依赖它）。
4. **resolve pass**：全屏三角形，读 `hdr.resolved_view`（背景，已带
   TEXTURE_BINDING/COPY_SRC）+ accum + reveal + 深度快照，
   `out = bg·reveal + accum.rgb/accum.w 型合成`（精确式照抄论文），
   结果写回 `hdr.msaa_view`（全 4 采样同值写）。此后粒子/TAA 照旧。
5. **诚实限制预写（§8.68 候选）**：①WBOIT 是近似——多层半透明能量有偏、
   极薄极浓边界不收敛到精确合成；②单层兼容锚：单张 Blend 面片时
   resolve 结果与经典 alpha blend 在 RGBA16Float 量化内一致（容差测试钉住，
   不是逐位）；③OIT 开启时透明覆盖区背景的 MSAA 子采样细节被解析中心值
   重写替代（边缘抗锯齿局部降级）；④RT 相机不吃 OIT；⑤水/粒子/贴花
   不在覆盖范围；⑥逐位确定性 = 同适配器双会话断言，跨适配器不保（§8.51
   同款）；⑦开关翻回缺席外的热切换语义（纹理保留 = 无泄漏，逐声音
   C6 同款句柄卫生测试）。

### 1.4 测试学（可判别性优先）

- **本债判别主测**：构造「painter 必错、逐像素必对」的解析场景——
  两块交叠凸面片（或一凹面片自交叠），视深中心序与逐像素正确序相反；
  断言 OIT 开中心像素 = 解析加权合成（±量化容差、写明依据），
  OIT 关（painter）同像素落在错误值——两态都要断言，证明测试可判别。
- 兼容锚：单层半透明 OIT ≈ SrcAlpha 经典混合（容差）。
- 缺席 = 逐位不变：既有 `transparent_render.rs` 5 测零改动原值通过
  （其解析值断言天然承担此责）+ 新增「OIT 资源插入前后无 Blend 场景
  全帧哈希一致」。
- 双会话逐帧逐位（有状态渲染文化，R60/R62b 断言式）。
- 快照 example：`games` 之外新建 `crates/iris-engine/examples/c7_oit_snapshot.rs`
  （交叠玻璃罩变体），BMP 目检图入 `docs/`。

---

## 2. 案 B · DDGI 动态光源与天空同源（§8.57①②）

### 2.1 根因位置（已对码）

- 天空单色：重照明未命中支直接回 `params.sky_color` 常数
  （irradiance.rs:2121），命中点环境项 = `sky_color·π`（:2135）；
  引擎侧 `ddgi_bake_params` 的 `sky_color = settings.sky_color.unwrap_or(ambient)`
  （iris-engine/ddgi.rs:50）。**不读 SkySource / SkySettings / 程序化渐变**。
- 点光缺席：`BakeParams`（bake.rs:632-647）只有方向光三字段；
  `tick_ddgi`（iris-engine/ddgi.rs:59-93）无灯查询；
  `relight_fingerprint`（ddgi.rs:619-638）无灯/天空指纹位。

### 2.2 裁决点

- **B1 天空同源深度**（提请，三档）：
  - **甲（推荐）**：逐射线天空辐亮度 CPU 求值器 `sky_radiance(dir)`，覆盖两源：
    ①程序化渐变 = `environment.wgsl::sky_color(d)` 的 CPU 镜像（源码看门钉双源同值）；
    ②SkySource HDR 全景 = 方向→equirect **nearest-texel** 采样（与 §8.57⑥
    nearest 立场一致，免插值序问题）。`SkySettings.atmosphere = true` 时
    该帧回退用参数化天顶/地平双色近似 **不做**——大气镜像未实现时
    逐射线求值器拒绝冒充，明写「atmosphere 场景的天空同源维持 §8.57①」，
    把三档的丙作为挂账。命中点环境项维持 `sky_color` 均值底（= 现值），
    **只有未命中射线看方向**——声明：反弹光的天空底色仍单色，远景入射才逐向。
  - 乙：甲 + R79 大气散射 CPU 镜像（airmass/transmittance/积分全式移植 +
    看门）。真实但重：float 超越函数 CPU/GPU 不逐位（声明内即可），
    移植 + 双源看门单测 ≈ 半个 R79。**除非用户点名，默认不做**。
  - 丙：仅「底色同源」——把逐向求值换成一次性面积加权平均常数。
    便宜但 §8.57①「地平线暖橙不进间接光」只修一半（无方向性）。**不够格**。
- **B2 点光/聚光进 GI**（提请）：
  - 灯集合：`tick_ddgi` 查询 `Entity + GlobalTransform + PointLight/SpotLight`，
    **entity bits 升序 + 上限 8**（与主灯表 `MAX_POINT_LIGHTS=8` 同容量；
    主渲染的分箱/预算选择发生在 draw（renderer.rs:6272+），重照明在 Update
    ——>8 盏时两侧选取可不同，声明边界）。
  - **贡献口径**（推荐甲）：命中点入射加
    `Σ_l color_l × intensity_l × ndotl × window(d, range)`——
    与 `standard.wgsl::point_light_contrib` 的 `window=(1−(d/r)²)²`
    同式（CPU 镜像先例 irradiance.rs:2216-2223）；聚光追加锥测试
    （inner/outer smoothstep，与 R78 同式）。
  - **阴影处理 = 核心裁决**：
    - **甲（推荐）**：逐灯逐命中点**单条硬影射线**（与方向光同口径，
      `allow_soft=false` 分支先例）。成本模型：§8.57⑪ 基线 0.83 ms/探针
      @rays16/1000 三角，命中射线 × 灯数最坏 +8× 影线 ⇒ 昼夜 + 满灯场景
      常态开销上界进声明；`settings` 加 `point_light_shadows: bool`（默认 true）
      作逃生门。
    - 乙：无阴影（光穿墙进 GI）。便宜一档，但与 DDGI「深度感知遮挡」的
      卖点正面冲突（墙后火把照亮墙前）——判 **拒**，除非用户要「先通后对」。
  - 空集旁路：`lights.is_empty()` ⇒ 一条额外算术都不做（**逐位锚**：
    现口径函数体在 lights 空时与原式完全同 op 序列，测试 `to_bits` 钉死）
    ⇒ 既有 DDGI 全测（含 R89_COV1_ON 定值哈希）原值通过。
  - 指纹：`relight_fingerprint` 追加消费——天空位（`sky_from_source` 标志 +
    `SkySource.generation`/kind）与灯集位（逐灯 pos/color/intensity/range/
    锥角 bits + 计数）。空集/关 ⇒ 追加位取固定 0 域，「不变 ⇒ 不标脏」
    语义不破坏（指纹值本身无逐位锁，只有「变化⇒脏」行为锁）。
- **B3 序列化**：`DdgiSection` 追加 `sky_from_source: Option<bool>` +
  `point_light_shadows: Option<bool>`（Default 缺省 = false/true 与运行时
  口径一致），`to_settings`/`from_settings` 往返对称，旧档零感知。
  昼夜联动免费：DayNight 每帧写 `DirectionalLight`（daynight.rs:67-70）⇒
  指纹里 light_dir/light_color 已是输入 ⇒ 日落渐进标脏（收敛滞后 = §8.57⑤
  现声明，不改）。

### 2.3 诚实限制预写（§8.67 候选）

①atmosphere 场景天空同源维持拒绝（B1 甲前提）；②命中点反弹天空底仍均值
常数；③>8 盏灯时 GI 灯集与主渲染选取可不同（entity 序截断）；④点光进 GI
仍是 1 反弹、nearest-texel 角分辨率不变（§8.57⑥⑦ 继承）；⑤硬影单射线
逐灯 = 火把 GI 阴影边缘全硬；⑥灯多时常态开销上界（成本进 §8.57⑪ 同款
量化句）；⑦聚光锥测试与主渲染共享公式但 CPU 侧无分箱（8 盏线性扫）。

### 2.4 测试学

- CPU headless（全解析可判别）：
  - 空集/关 ⇒ `probe_ray_radiance_depth` 逐位锚（to_bits 对照现口径手算值）；
  - 单点光无遮挡：命中点解析 `ndotl×window` 值镜像断言（±1e-6，依据写明）；
  - 墙后火把：二值遮挡 ⇒ SH 系数与无灯锚定差 < ε（「不进 GI」判别）；
  - 程序化天空逐向：天顶冷/地平暖双色渐变 ⇒ 朝地平探针分量偏暖的
    可判别断言（对照单色常数表）；
  - 指纹：加灯/换天空 generation ⇒ 全表标脏；灯集不变 ⇒ 不标脏（行为对）；
  - 滚动收敛 ≡ 全量重照明 FNV（照 ddgi_render:700 先例）+ 双会话逐位。
- GPU（ddgi_render.rs 追加，带 GPU_SERIAL）：点光场景下 DDGI 开/关
  全帧哈希可判别；`DdgiSettings` 新旋钮缺席 ⇒ 三态逐字节等（既有 :365 范式）。
- §8.57①② 处置：①改写为「程序化/HDR 同源已闭合，atmosphere 维持」；
  ②翻转为已闭合 + 边界索引 §8.67。

---

## 3. 门禁与收官

- 六项门禁串行、长门禁后台+退出码进日志、计数逐二进制对账（AGENTS 全程照旧）。
- 文档三件套 + AGENTS 基线行 + 计划文档 C7 收官条目 + 记忆更新；
  CHANGELOG 两役各一段；README 批次行 C7 → [x]。
- 预期测试增量：C7a ≈ +10（CPU 8 / GPU 2）、C7b ≈ +8（CPU 3 / GPU 5）。
- 收官后 = **闭合战役 8/8 全部完成**，出总收官声明（含 C0-C7 债务清算表
  与 §8 现存活声明盘点）。

## 4. 决策点汇总（等用户拍板）

| # | 决策 | 推荐 |
|---|------|------|
| A1 | OIT 算法 | Weighted Blended（McGuire HPG 2013，照抄原文） |
| A2 | OIT 覆盖范围 | 仅主透明 pass（水/粒子/贴花/RT 相机不并入） |
| A3 | RT 相机透明 | 本轮不做，维持 painter |
| B1 | 天空同源深度 | 甲：程序化渐变 + HDR 逐向镜像；atmosphere 维持挂账 |
| B2 | 点光进 GI 阴影 | 甲：逐灯硬影单射线 + `point_light_shadows` 逃生门；聚光同框架 |
| — | 验收口径 | 升级为「全部既有基线零改动原值通过」（普查支持） |

**2026-09-19 用户裁定（全部按推荐定稿）**：A1 = Weighted Blended（公式照抄原文）；
A2/A3 = 仅主透明 pass（水/粒子/贴花/RT 相机维持 painter，写边界）；
B1 = 甲（程序化+HDR 逐向镜像，atmosphere 维持 §8.57① 挂账）；
B2 = 甲（逐灯硬影单射线 + `point_light_shadows` 逃生门，聚光含锥测试）。
验收口径升级随批次生效。→ 排码序：C7a DDGI → C7b OIT。

## 5. C7b 公式取证（2026-09-19，禁凭记忆纪律的兑现记录）

来源：LearnOpenGL Guest Article "Weighted Blended"（McGuire & Bavoil HPG 2013
的转写，WebFetch 全文摘录）。**照抄件**：

- 纹理：accum = RGBA16F 清 (0,0,0,0)；revealage = R8（我们取 **R16Float**
  高精度档，原文允许）清 (1,0,0,0)。
- 混合：accum `(ONE, ONE)`；revealage `(ZERO, ONE_MINUS_SRC_COLOR)` ⇒
  dst ×= (1 − src.a 写入的红通道)——**逐像素连乘用固定混合态表达**。
- 片元权重（自适应档）：
  `weight = max(min(1.0, max(max(c.r,c.g),c.b)·a) · clamp(0.03/(1e-5+
  pow(z/200,4)), 1e-2, 3e3), a)`（z = 窗口深度 ∈ [0,1]；外层 `max(…, a)`
  的尾部在转写摘录中疑似折行截断，实现时按 canonical 形态落码 +
  CPU 镜像单测钉解析值）。
  `accum_out = vec4(c.rgb·a, a) · weight`；`reveal_out = a`。
- 合成：`avg = accum.rgb / max(accum.a, 1e-5)`；
  `out = avg·(1 − reveal) + bg·reveal`。
  ⚠️ 转写页的 composite 输出 `vec4(avg, reveal)` 依赖其自带混合态，
  直接照抄会得 `avg·reveal + bg·(1−reveal)` 的**倒置覆盖**——单层
  兼容锚（单片时 ≡ 经典 `c·a + bg·(1−a)`）代数上判死该歧义，取上式。
  该锚同时是 GPU 测试的第 2 条主测。

管线落点（读码定稿）：
- gather 前**提前编码 scene_depth 副本**（OIT 开启帧专属支路；副本内容
  = 不透明重画，透明/贴花/粒子本就不进副本 ⇒ 水体折射零感知；SSR 的
  「读上帧」语义靠编码位置保持在主 pass 之后，不受影响）；
  gather 无深度附件，片元 `textureLoad(sd.depth, coords)` 手动测深 discard。
- resolve 读 `hdr.resolved_view`（贴花后背景）+ accum + reveal，全屏三角
  **不透明写回 `hdr.msaa_view`**（4 采样同值；透明覆盖区背景 MSAA 子采样
  细节退化为解析中心值——边界③）。
- 管线族 = 现有 4 条透明管线（普通/双面/蒙皮/双面蒙皮）各 +gather 变体
  + 1 条 resolve；`standard.wgsl` 的 `fs_main` 抽共享体（R78 双路径位等价
  先例），算术逐式不动。
- 开关资源缺席 ⇒ 以上全部不编码不分配（`OIT` 无旋钮 ⇒ 无「非默认值
  断言」义务；`ensure_oit_targets` 惰性整尺寸重建照 `ensure_scene_depth` 范式）。
