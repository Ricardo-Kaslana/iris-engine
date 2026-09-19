# AGENTS.md — iris-engine 智能体工作指南

面向在本仓库工作的 AI 智能体（与人类贡献者）。逐条都是实战沉淀，违反过会付出真实代价的条目加粗。

## 项目是什么

Rust 游戏引擎（workspace 版本 0.1.1），核心卖点：**跨平台（native + wasm32）、内存安全（全库零 unsafe）、确定性（物理/模拟/渲染 bit 级可复现）、低占用、中文文档**。技术栈：wgpu 30 + winit 0.30 + 自研稀疏集 ECS + glam 0.33。

crate 布局（依赖自上而下）：

| crate | 职责 |
|---|---|
| `iris-macros` | Component/Resource derive |
| `iris-core` | ECS / 调度 / 层级 / 时间 / 物理（旋转胶囊/约束/接触事件）/ 角色 / NavGrid 导航 / ragdoll / 事件 / **行为树 + 黑板 + 感知（R65）**（零渲染依赖） |
| `iris-platform` | winit 窗口 + 输入 |
| `iris-audio` | 混音纯函数核心 + cpal 设备薄壳（wasm 无设备自动降级）+ OGG/Vorbis 流式（symphonia）+ 音乐总线 + 混响区 |
| `iris-render` | wgpu 渲染：PBR + 法线/ORM、CSM + 点光预算化阴影、TAA、烘焙 GI（光照贴图）、反射探针 + SSR、heightmap 地形、粒子、贴花、后处理链（DOF/MB/泛光/FXAA）、动画 v4、GLB、RTT、遮挡剔除 |
| `iris-engine` | 插件编排（DefaultPlugins）/ App / 场景与**关卡格式 v2** / GltfBuilder / pack 打包 / AssetMount / manifest 优先级异步加载 / 演出时间线 / UI 菜单 / Settings / GLB 动画注册表 |
| `iris-editor` | 关卡摆放编辑器（**编辑命令层 + 交互壳分离**——命令层全部可 headless 单测；undo / 光源编辑 / 烘焙入口 / 导航可视化） |
| `iris-game` `iris-arena` `iris-bastion` `iris-ascent` `iris-infiltration` `iris-assault` | 六个 dogfooding 游戏，位于根级 `games/`（引擎 crate 在 `crates/`，目录分离）（结构范式：`logic.rs` 纯核心 + `plugin.rs` 插件层 + headless 测试 + 快照 example） |

**架构与特性全景唯一权威是 `ARCHITECTURE.md`**（§1–§8），改任何子系统前先读对应章节；`§8 已知限制` 是诚实清单，别把已声明的限制当 bug 修。历史脉络（逐轮做了什么）在 `CHANGELOG.md` 与 git log，不依赖会话记忆。

## 环境硬约束（Windows / Git Bash）

- **所有 cargo 命令必须加前缀 `CARGO_HTTP_CHECK_REVOKE=false`**（本机镜像 SSL 吊销检查问题，不加必挂）。
- `.cargo/config.toml` 已配 `rustc-wrapper = "sccache"`：新机器先 `cargo install sccache`，否则一切 cargo 命令失败。
- ffmpeg 在 PATH（验收图 BMP/PPM→PNG 转换用）。

## 门禁命令序（每轮收官，顺序有讲究——详见 ARCHITECTURE §7.2）

```sh
cargo fmt --all --check
cargo build --release --workspace --all-targets   # 先行：一次编译全部产物（含 examples/benches）
cargo test  --workspace           --quiet          # debug 测试（编译全命中缓存）
cargo test  --workspace --release --quiet          # release 测试 + 预算断言
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --target wasm32-unknown-unknown -- -D warnings
```

**`cargo test` 不构建 examples/benches**——先 build 后 test 才不会把 release 编译两轮。当前基线 **debug 1685 / release 1682**（C4a 形变目标 +12 / C4b 状态机 v2 与运行时重定向 +15 / C5a-1 BC7 Mode 6 编码器 +13 / C5a-2 KTX2 容器 +11（解析 6 含拒收五类与 mipPadding 剥离 / 目录侧表 2 / GPU 端到端 2 含硬件解码保真兜底闸 / 场景 `.ktx2` 装载 1）/ C5b 热重载局部 +5（端到端参数应用与纹理句柄不动 / 缺席零触碰 / 节流与坏内容消费两旋钮 / 未命中收集 / 去重共享旁证，全 CPU headless）/ C6 空间化 v3 +11（iris-audio 8：多普勒静止逐位锚 / 双向径向解析值 / 钳界病态 / muffle 系数解析 / 阶跃爬坡跨调用持久 / 旁路三型逐位 / 端到端过零率升频 / 句柄卫生与声速旋钮；iris-engine 3：帧差分速度解析 / 遮挡二值与 opt-in / 自碰撞体豁免，全 headless）；C7a DDGI 同源役 +14（iris-render ddgi 9：缺席档逐位锚 / WGSL↔Rust 常数双看门 / 程序化解析向 / HDR nearest 映射解析 / 点光窗口解析值 / 遮板硬影逐位零 / 锥门控与点光逐位等 / 指纹行为组 / 滚动脏-收敛-判别全链；iris-engine 4：收集口径 / 快照优先级链 / 火把端到端+空集锚 / 天空方向性双向证据；GPU 1：火把 ROI 抬亮+稳态逐位+天空可判别）；C7b OIT 役 +9（iris-render oit CPU 6：权重地板/饱和/窗口域退化/单层兼容锚/无覆盖逐位恒等/交换序逐位 + transparent_render GPU 3：顺序无关主测（交换 spawn 序 painter 变、OIT 整帧逐位）/缺席逐位+无覆盖护栏+单层锚/双会话逐位）；BC7 永不自动、KTX2 仅未压缩 + mip 级、HRTF 按三门裁出回池、C4c 动画压缩同前裁出；全部 opt-in「缺席 = 逐位不变」，既有物理/渲染/游戏/音频快照测试零改动原值通过；**C6 收官、C7 收官 ⇒ 闭合战役 C0~C7 全部完成**；新功能批次（W8/R90 等）待用户重启裁定；每轮收官顺手更新本行数字）。

### 门禁耗时陷阱（实测教训）

- **每跑一个门禁前，必须先停止所有的 cargo 进程**（`tasklist | grep -iE "cargo|rustc|sccache"`
  检查，有则 `taskkill //PID <pid> //F`）。残留进程（超时的子智能体、被中断的后台任务）
  持着 `target/` 文件锁，新命令会**静默排队等待**（`Blocking waiting for file lock`）——
  实测出现过「40 分钟无输出」实为排队、进程清理后同任务 50 秒完成的案例。这不是可选项，
  是每个门禁的前置步骤。
- **`RUSTC_WRAPPER=""` 只属于 wasm 那两条门禁**（绕 sccache 的 os error 206）。把它加到
  原生命令上 = sccache 全程失效 + 依赖图指纹改变 ⇒ 整条 release 图从源码重编
  （实测同一次测试：**30 分钟 vs 2 分钟**）。
- **门禁之间不得并行**：多个 cargo 任务共用一个 `target/`，会串行在文件锁上并把 CPU
  打满，耗时相乘。串行顺序执行六项即可。
- **怀疑「测试跑太久」先分清编译与执行**：`cargo test --workspace --release --timings`
  出 HTML 报告；跑完立刻 `sccache --show-stats` 看命中（服务器空闲约 600 s 退出、
  统计清零，隔天看不到）。参考量级：本仓全量 release **测试执行**合计约 50 s。

### 测试计数陷阱

**统计测试总数必须逐二进制明细加总**（基线见上节，过期时顺手更新）。已知陷阱：
- `grep -v "0 passed"` 会把 `10 passed` 也过滤掉（子串匹配）——禁用；
- 两次独立的 cargo 调用输出无法对齐——二进制名与计数必须来自**同一次调用**的输出；
- **管道截断假象**：复跑数字与汇报不符时，先单次完整调用重定向文件再逐二进制对账
  （R52a 曾出现 860 vs 868 假象），不要急着判虚报。

安全的加总方式：`cargo test --workspace 2>&1 | grep -oE "[0-9]+ passed" | awk '{s+=$1} END {print s}'`。

## 代码规约

- **中文文档/注释/断言消息**——公开项全带文档注释，模块头写语义与**诚实限制清单**（v1 做不到的明说，不藏）。
- **零 unsafe**，无例外。
- **确定性是硬文化**：需要迭代序的数据结构用 `BTreeMap`/`BTreeSet`（选型理由写进文档，参照 `broadphase.rs`）；随机性用无状态哈希流（splitmix64 + 计数器，参照 `particles.rs` 的 `mix64`），**禁止 rand 依赖与全局状态**；浮点确定性细节如 `sqrt` 优于 `hypot`（libm 无位级保证）。确定性测试模式：同输入两次跑，`to_bits()` 逐位断言；**有状态渲染**（TAA 历史/SSR 上帧颜色）用双会话逐帧逐位一致断言（R60/R62b 范式）。
- **纯函数优先**：数值逻辑提纯为可脱离 World 单测的自由函数/方法（参照 `physics.rs` 的 `solve_contact`）；系统用 `&World` + 尝试型锁 + 两阶段（收集→写回）模式，冲突即 panic（消息含 "already borrowed"）。
- **组件缺席语义**要写明（如：只有 Collider 无 RigidBody = 静态体；Visibility 缺席 = 可见；`TaaSettings`/`SsrSettings`/`OcclusionCulling` 资源缺席 = 关）——缺席行为是 API 的一部分。
- **像素基线方法论（FNV 兼容锁）**：改渲染路径前先用「改码前捕获全帧哈希」锁基线（R42 开创）；「默认资源缺席 = 逐位不变」是新增渲染特性的标准兼容形态。**合法打破基线的轮**（如 R61 阴影 v2）必须走「**归因 + 重锁**」程序：①受影响场景的「特性关闭对照组」新旧代码逐位一致（证明改动面限制在目标路径）②对照模式逐位复现旧版（`set_legacy_*` 先例）③重锁声明进 CHANGELOG——不允许静默改值；未受影响的锁原值通过是「改动面没外溢」的旁证。

## 测试规约

- 断言用**精确值**（解析解对照、±1e-5 容差写明依据），不写「大于 0」式摆设断言；**有效性断言要可判别**（R53/R60 的指标学教训：指标要在分布高端区分、ROI 避开混淆源）。
- GPU 测试：`OffscreenRenderer::new` 返回 None 时 `eprintln!("跳过：环境无 GPU")` + return，不 panic。**重负载 GPU 套件并行会挂死**（R62b 实测：同文件 7 项并行共用单适配器，release 下 3 项后其余无限停顿；后续复测发现 debug 同样会挂——shadow_v2_render 两轮自旋 10 分钟零推进，transparent_render 空转 18 分钟、decal_render 直接段错误 0xc0000005——症状谱：**自旋挂死 / 空转死等 / 驱动侧段错误**，同一病灶）：此类套件在文件内加 `static` 互斥锁串行化（参照 `ssr_render.rs::GPU_SERIAL`；现有互斥：ssr / sss / irradiance / shadow_v2 / transparent / decal / post / probe / meshlet / meshlet_v2 / meshlet_lod / meshlet_stream——post_render 13 项曾漏配，iris-exp 控制轮 debug 全量门禁实锤挂死 10 分钟后补齐；**probe_render 7 项曾漏配，R82 收官轮 release 全量门禁实锤挂死**（`running 7 tests` 后 CPU 0% 长驻；单独复跑 10.65 s 正常）**后补齐**；**meshlet_stream_render 的遮挡整合项曾漏配（R87 首稿 4 项只 3 项带锁——未及在标准门禁下复现，作为纪律缺口在验收轮补齐）**），不要靠 `RUST_TEST_THREADS=1` 记忆——标准门禁命令必须默认可靠。新写 GPU 套件时直接带上互斥，别等挂了再补。cargo test 的二进制之间本就串行执行，争用只来自**同文件多测试并行**，所以文件内互斥即足。
- should_panic 测试若依赖 `debug_assert!`，必须 `#[cfg(debug_assertions)]` 门控（R33/R38 各踩过一次 release 假失败）。
- 测试间**共享文件名会竞态**（并行执行）——临时文件用唯一名或 Once 模式（gltf/hdr/asset_pipeline/edit_headless 四次：R62a 验收 `pid` 同名目录下并行测试线程互删文件；R63 验收 editor 测试只在 pid 后**无调用序计数**——Windows 复用历史运行 pid 时 `create_dir_all` 命中旧目录 ⇒ `loaded = true` / 陈旧产物失配，debug 时序不同侥幸通过、release 才炸；唯一名 = 进程 id + 调用序计数，缺一不可）。
- 改物理/模拟必须有确定性测试；改渲染必须有像素断言测试；渲染器按**句柄值**缓存 GPU 资源——同一渲染器上渲染「同句柄不同内容」的多个世界会串味（R53/R62 两次踩到），跨世界对照必须各用独立渲染器。
- **偶发失败 = 真 bug 信号**（本仓库沉淀的铁律）：Duration 舍入、临时文件竞态、浮点边界（R39 盒面 1e-4 容差）、uv_sphere 极点退化三角（R58b）、u8 关节蒙皮被拒（R59 规范笔误）都是这么抓到的。不重跑通过就放过；归因要查到机制，「Windows/环境问题」是排除项不是首选解释。
- **每个公开旋钮至少要有一条非默认值断言**（R89 沉淀，B1 的教训）：分量镜像测试会漏掉整条公式的缺项——`SsrSettings::intensity` 上传到着色器后 26 轮零读取，CPU 侧只有 roughness / edge 两个**分量**镜像、合并权重无镜像，任何「默认值路径」的测试都测不到它；非默认值断言（强度 0 ⇒ 与关逐位一致、0.5 ⇒ 线性、2 ⇒ 钳 1）+ 整条公式的合并镜像函数才把旋钮钉住。

## 已知平台陷阱（改对应子系统前先读）

- **DX12 下 depth-only 管线写入恒 0**——阴影用「深度写颜色 R 通道 + Depth32Float 双附件」方案；深度附件跨 pass 读取必须显式 `Store`（R23 教训）。
- `pollster` 在 wasm 忙自旋卡死主线程——GPU 初始化走异步核心 + 事件循环逐帧 poll。
- `World::spawn` 需要 `&mut World`；`&World` 的系统**无法注册新组件类型**（存储表不可变）——运行期动态组件要预挂池槽（参照 games 的对象池模式）。
- glam 0.33：`Mat4::from_translation` 收 `Vec3` 不收数组；矩阵列主序用 `to_cols_array_2d`；抖动投影只能作用于**纯投影矩阵再乘 view**（对合成矩阵做行运算不是纯 NDC 平移，R60 教训）。
- **WGSL 并行结构体布局是影子契约**：`skybox.wgsl` 自持 `Globals` 前缀声明，`standard.wgsl` 扩容（如 PointLight 2→3×vec4）必须同步，漏改症状是天空整体错色——`pipeline.rs::skybox_globals_prefix_matches_standard_layout` 源码层看门（R61 教训）；新增并行布局时照此加看门单测。
- 捕获/倒视矩阵的 NDC y 翻转会**反转三角形环绕方向**——独立捕获管线需同步翻 `front_face`（R62a 教训），或用 `cull None` 吸收（阴影 pass 现状）。
- `Duration` 纳秒舍入：固定步长向内截断（`time.rs::set_fixed_hz` 注释详述——偶发差一步的元凶）。

## 多智能体协作（当前生效模式）

- **主会话**：设计决策 → 产出提示词 → 独立验收 → git 提交（**全仓唯一提交者**；外部 Agent 一律不做 git 写操作）。
- **外部 Agent 模式**：主会话把整轮提示词发给用户 → 用户转发外部 Agent 执行 → 汇报发回（贴文本或仓库根 `REPORT.md`，**不入库、验收时删除**）→ 主会话按下方 SOP 验收后提交。
- **验收 SOP**（汇报到达后顺序执行）：清残留 cargo 进程 → `git status --short` 看改动面 → 亲审关键 diff（挑 1–3 处：数学推导/设计决策/偏离声明对应的实现）→ 六项门禁串行复跑 + 计数对账 → 快照 example 复跑验逐字节一致 → 目检验收图（`docs/*.png`）→ 删 REPORT.md → 中文 conventional 提交（body 详写设计理由与偏离声明）。
- **长门禁一律显式后台启动，绝不让前台命令被自动转后台**（R58b/R61/R63 三次踩坑，每次烧掉 15–20 分钟验收时间）：前台超时被自动转后台的命令会**爬行**（输出管道背压阻塞子进程——症状 = 进度突发式推进、10 分钟阻塞轮询一次只推一波、看似卡死实则缓慢排空）；而一开始就 `run_in_background` 启动的同一命令约 5 分钟正常跑完。执行协议：①预计超过 5 分钟的门禁（目前是 debug 全量测试）启动即挂后台，靠**读日志文件**轮询进度，不做长阻塞等待；②任何命令若已被自动转后台且进度停滞（日志文件 2–3 分钟无增长），立即杀任务 + 清 cargo/rustc 残留进程，再显式后台重启——编译缓存是热的，重跑只花正常时间，干等只花 20 分钟；③自动转后台的任务被杀时症状 = 日志中途截断 + exit=1 且无任何失败记录，不要当成测试失败；④**长门禁命令不得以 `echo` 收尾**（任务退出码被 echo 恒置 0，杀进程无从察觉）——把退出码写进日志文件尾（`cargo test … > log 2>&1; echo "exit=$?" >> log`），完整性以「日志末行 exit=0 + 二进制计数」判定；⑤测试计数**一律文件重定向**（管道截断假象在 R65 验收再赔一整轮 release 重跑）；⑥**区分「编译中」与「停滞」**：改 `iris-core`（依赖链根）的轮次双 profile 全量重编是固有成本，门禁 3 编译期 8–12 分钟属正常——轮询报「编译中」不等于卡住，先看 `cargo` 是否在推进（`target/debug` 增量时间戳）再决定杀；⑦**日志/产物消失 = 外部干扰信号**（正常完成的 `>` 重定向文件不可能消失），停下向用户核实（用户可能刚中断/清理过——用户手上有会话外信息），不要静默绕过。
- 子智能体（Agent 工具）可用但超时频发：留半成品时先 `cargo check` 探测完成度，再派续做或主会话接手。

### 给外部 Agent 的提示词骨架（发新一轮时照此拼装）

```
# RXX：<轮名>（路线图 v5 · W?/梯队?）

你是 iris-engine 仓库（D:\Development\Complex\iris-engine）的外部工程 Agent。
开工前必读：AGENTS.md（工程规约权威——门禁顺序/耗时陷阱/统计陷阱/硬约束）、
ARCHITECTURE.md 对应章节、本轮涉及的既有模块。

## 任务（设计决策点逐一定稿：范围边界 / 关键算法 / 兼容硬约束 / 拒绝清单）

## 验收（硬标准：既有测试零改动全过 或 归因+重锁程序；确定性测试；
验收图 + 快照跨运行逐字节一致——主会话会复跑）

## 门禁与汇报
六项门禁全绿（AGENTS.md 顺序与铁律：每门前清残留进程；
CARGO_HTTP_CHECK_REVOKE=false 前缀；RUSTC_WRAPPER="" 只加 wasm clippy；
门禁串行）。REPORT.md 于仓库根：改动清单、逐二进制测试数（单次完整
调用加总，禁用 grep -v "0 passed"，基线 debug 1685 / release 1682）、
门禁表、设计决策、与规格偏离及理由。不做任何 git 写操作；REPORT.md
不入库。
```

关键习惯：每轮必带「既有测试零改动全过」或「默认像素零变化 + FNV 基线锁」类硬约束；偏离必须声明理由；渲染轮快照必须跨运行逐字节一致。

## 工作流（每轮的完成定义）

1. 设计定稿（读码 → 决策写清）→ 2. 实现 + 测试 → 3. 六项门禁全绿 → 4. 涉渲染出离屏验收图并**目检** → 5. 文档三件套（README / ARCHITECTURE 对应章节 / CHANGELOG Unreleased；AGENTS.md 基线数字过期顺手修）→ 6. 提交（中文 conventional commit，body 写设计理由与偏离声明）。

- **不做任何 git 写操作除非主会话明确要求**（多智能体协作时外部 Agent 只改代码）。
- dogfooding 游戏轮产出的引擎摩擦清单是**一级交付物**；游戏轮**不改引擎**（纪律），摩擦留给下一轮打磨闭合——每条按「现象 → 绕行 → 建议修法」记录进 ARCHITECTURE §8。
- 与规格偏离必须汇报并给理由（历史偏离多有充分依据，如 `&mut World` spawn、rest 姿势显式捕获、上帧深度复用单份 scene_depth）。
- 文档滞后是 bug：改行为必须同步 ARCHITECTURE 对应章节，_FEATURES 数字（测试数等）过期也要顺手修。

## 当前状态与路线图（2026-09-14 更新）

- 测试基线 debug 1685 / release 1682；全库零 unsafe；native+wasm32 双 target 零警告；**无 git 远端**（R56 发布时需用户提供）。
- **路线图 v5**（2026-09-14 用户裁定定稿；全文与裁定记录见 `docs/private/roadmap-v5.md`。
  编号与执行序解耦：W8 保留 R69–R72，新轮自 R73 起编，第一梯队先于 W8 执行）：
  - 已收官：W5 内容产能 ✅（R57–R59）、W6 视觉上限 ✅（R60–R66b）、W7 首轮 R65
    行为树 ✅（infiltration 守卫迁移 FNV 逐位等价）
  - 第一梯队（战斗底座，先于 W8）：**R73 任务调度器 v2 ✅（数据并行分片；
    常驻线程池归因 `'static` 边界后 v1 不做，见 §8.1 / CHANGELOG 偏离声明）**、
    R74 动画
    事件+曲线轨道 ✅（事件轨 v1 已有——grep 笔误偏离声明；实交付帧数据
    窗口 + 参数曲线轨）、R67 帧数据战斗 ✅（assault 帧数据表单一事实源 +
    取消窗口 + 事件驱动表现层，快照重生成；摩擦：R74 类型 prelude 缺口
    同轮闭合 §8.26⑧）、R68 战斗相机 ✅（trauma 屏震
    + FOV punch + 取景呼吸，引擎零改动）、R75 IK v2 ✅（FABRIK 任意长链
    + look-at 单遍分布式瞄准，与两骨 IK 并存；末端位置不写防吸收态
    漂移）、R76 接触求解 v2 ✅（ContactSolverV2 资源 opt-in：角冲量 +
    摩擦锥 + 顺序冲量；缺席 = v1 逐位不变，闭合 §8.27①）、R77 GTAO
    ✅（AoSettings 缺席 = binding 14 哑纹素 1.0 逐位不变；4 切片×8 步
    世界半径 + dither，半分辨率 R16Float，@builtin(position) 采样乘
    间接光；三个实现级归因：半纹素错配 / 越界弃权 / screen uv 投影量
    弃用 varying）、R78 SpotLight+光源扩容 ✅（第一梯队就此收官：
    SpotLight 组件 + light_bins 分箱纯函数 + binding 15/16/17 灯表，
    fs_main 点光循环逐式提取共享函数 + 均匀门 = 双路径位等价，
    全部既有 FNV 基线零改动原值通过；诚实限制 §8.47）
  - 第二梯队（环境底座）**全部收官**：R79 天空 v2+大气+体积雾 ✅、R80 昼夜+风场 ✅、
    R81 平面水体 v1 ✅、R82 粒子 v2（GPU 化+拖尾）✅、R83 GI 逐片元化（探针三阶
    SH + 片端径向权重，§8.40①② 移入实现、③ 维持拒绝）✅
  - 第三梯队（硬科技）：R84 虚拟几何体 V1 ✅（确定性簇构建 + IRML 容器 +
    展开等价锚点，渲染路径零改动）、R85 虚拟几何体 V2 ✅（compute 簇剔除 +
    u32 前缀和稳定紧凑化 + vertex-pull 绘制 + 片元阶段 = standard fs_main
    本体 ⇒ expand 与 compute 路径跨管线 FNV 相等；诚实限制 §8.54）、
    R86 虚拟几何体 V3 ✅（层级 LOD：簇 DAG + 屏幕空间误差选择；IRML v2
    与 v1 双向兼容，选择只换输入集合 ⇒ GPU 管线零改动；诚实限制 §8.55）、
    R87 虚拟几何体 V4 ✅（分页流送 + 遮挡整合，**连役 4/4 收官**：IRML v3
    页区段 + 16 簇/页 + GPU 显存驻留状态机（优先级全序、无抖动、乱序请求
    同队列）+ 驻留感知 LOD 选择（页缺沿 DAG 上取父组、无缝洞）+ R53 实体级
    遮挡整合（收集期整帧跳过）；页槽 = arena 规范区间 ⇒ 两个 WGSL 零改动；
    兑现 R84/R85 推迟的 pack 最小入口与路线图 §一红线修订声明；诚实限制
    §8.56）、
    R88 DDGI 动态 GI v1 ✅（规则探针网格 × 时间片滚动重照明 × chebyshev
    深度感知混合；**闭合 §8.52①**——径向权重不感知遮挡的墙两侧串味；诚实限制
    §8.57）、R89 DDGI v2 ✅（**第三梯队 GI 2/2 收官**：合成语义五方裁决
    （贴图互斥/覆盖度混合/ambient 分摊/捕获零阶/SSR 一致性）× 覆盖度连续化
    `cov` 修 §8.57⑬⑱ × 编辑器入口（BakeExcluded + 参数步进 + 可视化 +
    level.ddgi 入档）× Fixed 两条既有 bug（SSR 强度死旋钮 26 轮 / reset_taa
    漏清时域状态）；诚实限制 §8.58，闭合 frictions ⑬）、R90/R91 毛发、
    R92 降水、R93 海洋 v2+水下（CPU 位移图
    折衷，裁定接受）
  - 闭合轮池（按需穿插）：约束 v2 / OBB+凸包 / 形变目标 / 状态机 v2+运行时重定向 /
    OIT / 贴花 v2 / LOD v2 / 音频 v3 空间化 / 动画压缩 / BC7+KTX2 / 热重载局部 /
    CCD / RVO 群体
  - W8 完整 Assault（游戏七，dogfooding 纪律）：R69–R72（未获用户「OK」不开工）
  - W9 收官：R55 生产化、R56 发布 0.1（语义版本/docs.rs/CHANGELOG 发布段）
- 实验项目：`games/iris-exp`（外部 GLB 角色 + Mixamo 重定向动作导入验证；
  引擎侧摩擦独立成档 `docs/private/iris-exp-frictions.md`）。
- 休眠项（可插队）：手柄输入（**用户明确暂缓**）、Android、wasm 深化、载具
  （2026-09-14 裁定维持休眠）、本地化。（GPU 粒子拖尾 / 水面已随路线图 v5 立项为
  R82 / R81，脱离休眠。）
- **红线（用户裁定，2026-09-14 修订）**：中文交流；用户是项目唯一决策人（裁剪/加轮
  都听他的）；不做网络多人、通用 DCC 级编辑器（自有 MVP 编辑器除外）。原「照片级
  （Nanite/Lumen 类）」红线解禁收窄：虚拟几何体（本仓体量 Nanite 式子集、opaque-only
  MVP，R84–R87）与 DDGI 动态 GI（R88–R89）立项；visbuffer 全量管线与 Lumen 类全动态
  GI 仍不做。原「大开放世界流式」收窄：开放世界地块流式不做，资产级 meshlet 分页
  流送随 R87 允许（**已兑现**：GPU 显存分页，磁盘级异步 IO 仍不做——§8.56①）。
- 用户会实际运行游戏与编辑器并报 bug（Infiltration 四问题、编辑器试玩落盘）——演示与验收图质量必须真实。
