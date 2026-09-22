# AGENTS.md — iris-engine 智能体工作指南

面向在本仓库工作的 AI 智能体（与人类贡献者）。逐条都是实战沉淀，违反过会付出真实代价的条目加粗。本文件只保留**每次开工都要用的高频规约**；规则背后的实测案例、长篇操作协议与测试基线前史收在同仓库根的 **`AGENTS-PLAYBOOK.md`**（按需查阅——本文每处「见 PLAYBOOK §N」都有落点）。

## 项目是什么

Rust 游戏引擎（workspace 版本 0.1.2），核心卖点：**跨平台（native + wasm32）、内存安全（全库零 unsafe）、确定性（物理/模拟/渲染 bit 级可复现）、低占用、中文文档**。技术栈：wgpu 30 + winit 0.30 + 自研稀疏集 ECS + glam 0.33。

crate 布局（依赖自上而下）：

| crate | 职责 |
|---|---|
| `iris-macros` | Component/Resource derive |
| `iris-core` | ECS / 调度 / 层级 / 时间 / 物理（旋转胶囊/约束/接触事件）/ 角色 / NavGrid 导航 / ragdoll / 事件 / **行为树 + 黑板 + 感知（R65）** / **RVO 群体互避（R95）**（零渲染依赖） |
| `iris-platform` | winit 窗口 + 输入 |
| `iris-audio` | 混音纯函数核心 + cpal 设备薄壳（wasm 无设备自动降级）+ OGG/Vorbis 流式（symphonia）+ 音乐总线 + 混响区 |
| `iris-render` | wgpu 渲染：PBR + 法线/ORM、CSM + 点光预算化阴影、TAA、烘焙 GI（光照贴图）、反射探针 + SSR、heightmap 地形、粒子、贴花、后处理链（DOF/MB/泛光/FXAA）、动画 v4、GLB、RTT、遮挡剔除 |
| `iris-engine` | 插件编排（DefaultPlugins）/ App / 场景与**关卡格式 v2** / GltfBuilder / pack 打包 / AssetMount / manifest 优先级异步加载 / 演出时间线 / UI 菜单 / Settings / GLB 动画注册表 |
| `iris-editor` | 关卡摆放编辑器（**编辑命令层 + 交互壳分离**——命令层全部可 headless 单测；undo / 光源编辑 / 烘焙入口 / 导航可视化） |
| `iris-game` `iris-arena` `iris-bastion` `iris-ascent` `iris-infiltration` `iris-assault` | 六个 dogfooding 游戏，位于根级 `games/`（引擎 crate 在 `crates/`，目录分离）（结构范式：`logic.rs` 纯核心 + `plugin.rs` 插件层 + headless 测试 + 快照 example） |

**架构与特性全景唯一权威是 `ARCHITECTURE.md`**（§1–§8），改任何子系统前先读对应章节；`§8 已知限制` 是诚实清单，别把已声明的限制当 bug 修。历史脉络（逐轮做了什么）在 `CHANGELOG.md` 与 git log，不依赖会话记忆。

`docs/` 目录分工：根目录的 PNG 是各特性的离屏验收图（入库）；`docs/archive/`（**入库**）是已收官设计的定稿归档，按主题一篇一档（如 `ddgi_v2_design.md`、`r95-rvo.md`），只读参考，不再随实现更新；`docs/private/`（**不入库**，已在 `.gitignore`）是私有工作档——路线图、游戏摩擦清单、动捕笔记、辅助脚本与本机验收证据，克隆仓库后缺席，脉络以 `CHANGELOG.md` 与 git log 补齐。

**当前进度**：项目**不再按照路线图 v5 / v6 开发**，两档路线图均为私有档、不入库，仅作历史参考，不再作为开发依据。历史脉络以 `CHANGELOG.md` 与 git log 为准——路线图未体现的久远历史 commit 同样记录在 `CHANGELOG.md` 中；克隆仓库后私有档缺席也不影响，以 `CHANGELOG.md` 最新段与 git log 为准。

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

**`cargo test` 不构建 examples/benches**——先 build 后 test 才不会把 release 编译两轮。当前基线 **debug 1823 / release 1820**（每轮收官顺手更新本行数字；逐轮测试明细与排障教训前史见 PLAYBOOK §3）。

### 门禁执行铁律（实测案例全文见 PLAYBOOK §1/§2）

- **每跑一个门禁前，必须先停止所有 cargo/rustc/sccache 残留进程**（`tasklist | grep -iE "cargo|rustc|sccache"`，有则 `taskkill //PID <pid> //F`）——残留进程持 `target/` 文件锁，新命令会**静默排队等待**，看似卡死实为排队。
- **门禁之间不得并行**（共用一个 `target/`，串行在文件锁上并把 CPU 打满，耗时相乘），串行顺序执行六项即可。
- **`RUSTC_WRAPPER=""` 只属于 wasm 那两条 clippy 门禁**（绕 sccache 的 os error 206）——加到原生命令上 = sccache 全程失效 + 依赖图指纹改变 ⇒ 整条 release 图从源码重编。
- **测试计数逐二进制明细加总**：禁用 `grep -v "0 passed"`（子串匹配会误滤 `10 passed`）；二进制名与计数必须来自**同一次调用**的输出；复跑数字与汇报不符时，先单次完整调用重定向文件再逐二进制对账，不要急着判虚报。安全加总：`cargo test --workspace 2>&1 | grep -oE "[0-9]+ passed" | awk '{s+=$1} END {print s}'`。

## 代码规约

- **中文文档/注释/断言消息**——公开项全带文档注释，模块头写语义与**诚实限制清单**（v1 做不到的明说，不藏）。
- **文档文风（R95 起立规，用户裁定）**：技术文档（CHANGELOG / ARCHITECTURE / README / 模块头）以**人类程序员为第一读者**，用完整句子陈述，主谓宾齐全。**禁比喻与类比**——直接描述行为与数值，不写「像什么」；**禁箭头链串因果**（⇒ / →）与**等号当系动词**（「形态 = X」写成「形态是 X」）；**禁斜杠竖线压缩清单与成串名词短语**——多项内容各写成完整句子或用真正的列表；**禁电报体超短句**。不写智能体工作流视角的元叙述（「目检」「记档」等验收 SOP 词汇只出现在 AGENTS/PLAYBOOK 流程文档里）。写完自查一遍：人类维护者能否直接朗读理解。
- **零 unsafe**，无例外。
- **确定性是硬文化**：需要迭代序的数据结构用 `BTreeMap`/`BTreeSet`（选型理由写进文档，参照 `broadphase.rs`）；随机性用无状态哈希流（splitmix64 + 计数器，参照 `particles.rs` 的 `mix64`），**禁止 rand 依赖与全局状态**；浮点确定性细节如 `sqrt` 优于 `hypot`（libm 无位级保证）。确定性测试模式：同输入两次跑，`to_bits()` 逐位断言；**有状态渲染**（TAA 历史/SSR 上帧颜色）用双会话逐帧逐位一致断言（R60/R62b 范式）。
- **纯函数优先**：数值逻辑提纯为可脱离 World 单测的自由函数/方法（参照 `physics.rs` 的 `solve_contact`）；系统用 `&World` + 尝试型锁 + 两阶段（收集→写回）模式，冲突即 panic（消息含 "already borrowed"）。
- **组件缺席语义**要写明（如：只有 Collider 无 RigidBody = 静态体；Visibility 缺席 = 可见；`TaaSettings`/`SsrSettings`/`OcclusionCulling` 资源缺席 = 关）——缺席行为是 API 的一部分。
- **像素基线方法论（FNV 兼容锁）**：改渲染路径前先用「改码前捕获全帧哈希」锁基线（R42 开创）；「默认资源缺席 = 逐位不变」是新增渲染特性的标准兼容形态。**合法打破基线的轮**（如 R61 阴影 v2）必须走「**归因 + 重锁**」程序：①受影响场景的「特性关闭对照组」新旧代码逐位一致（证明改动面限制在目标路径）②对照模式逐位复现旧版（`set_legacy_*` 先例）③重锁声明进 CHANGELOG——不允许静默改值；未受影响的锁原值通过是「改动面没外溢」的旁证。

## 测试规约

- 断言用**精确值**（解析解对照、±1e-5 容差写明依据），不写「大于 0」式摆设断言；**有效性断言要可判别**（R53/R60 的指标学教训：指标要在分布高端区分、ROI 避开混淆源）。
- GPU 测试：`OffscreenRenderer::new` 返回 None 时 `eprintln!("跳过：环境无 GPU")` + return，不 panic。**重负载 GPU 套件（同文件多测试并行共用单适配器）会挂死——必须在文件内加 `static` 互斥锁串行化**（参照 `ssr_render.rs::GPU_SERIAL`；现有互斥：ssr / sss / irradiance / shadow_v2 / transparent / decal / post / probe / meshlet / meshlet_v2 / meshlet_lod / meshlet_stream）。不要靠 `RUST_TEST_THREADS=1` 记忆——标准门禁命令必须默认可靠；新写 GPU 套件时直接带上互斥，别等挂了再补；cargo test 的二进制之间本就串行执行，争用只来自**同文件多测试并行**，所以文件内互斥即足。症状谱与历次漏配挂死案例见 PLAYBOOK §4。
- should_panic 测试若依赖 `debug_assert!`，必须 `#[cfg(debug_assertions)]` 门控（R33/R38 各踩过一次 release 假失败）。
- 测试间**共享文件名会竞态**（并行执行）——临时文件唯一名 = **进程 id + 调用序计数**，缺一不可（四起事故案例见 PLAYBOOK §4）。
- 改物理/模拟必须有确定性测试；改渲染必须有像素断言测试；渲染器按**句柄值**缓存 GPU 资源——同一渲染器上渲染「同句柄不同内容」的多个世界会串味（R53/R62 两次踩到），跨世界对照必须各用独立渲染器。
- **偶发失败 = 真 bug 信号**（本仓库沉淀的铁律）：Duration 舍入、临时文件竞态、浮点边界（R39 盒面 1e-4 容差）、uv_sphere 极点退化三角（R58b）、u8 关节蒙皮被拒（R59 规范笔误）都是这么抓到的。不重跑通过就放过；归因要查到机制，「Windows/环境问题」是排除项不是首选解释。
- **每个公开旋钮至少要有一条非默认值断言**（分量镜像测试会漏掉整条公式的缺项）：非默认三档（如强度 0 ⇒ 与关逐位一致 / 0.5 ⇒ 线性 / 2 ⇒ 钳 1）+ 整条公式的合并镜像函数才把旋钮钉住（B1 案例全文见 PLAYBOOK §4）。

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
- **外部 Agent 模式**：主会话把整轮提示词发给用户 → 用户转发外部 Agent 执行 → 汇报发回（贴文本或仓库根 `REPORT.md`，**不入库、验收时删除**）→ 主会话按验收 SOP 验收后提交。
- **主会话专用协议在 PLAYBOOK**：验收 SOP 全文（§6）、长门禁后台执行协议（§5）、外部 Agent 提示词骨架（§7，发轮前照此拼装）。验收一句话版：清残留 cargo 进程 → 看改动面 → 亲审关键 diff → 六项门禁串行复跑 + 计数对账 → 快照复跑逐字节一致 → 目检验收图 → 删 REPORT.md → 中文 conventional 提交。
- 子智能体（Agent 工具）可用但超时频发：留半成品时先 `cargo check` 探测完成度，再派续做或主会话接手。

## 工作流（每轮的完成定义）

1. 设计定稿（读码 → 决策写清）→ 2. 实现 + 测试 → 3. 六项门禁全绿 → 4. 涉渲染出离屏验收图并**目检** → 5. 文档三件套（README / ARCHITECTURE 对应章节 / CHANGELOG Unreleased；AGENTS.md 基线数字过期顺手修）→ 6. 提交（中文 conventional commit，body 写设计理由与偏离声明；**提交标题与正文禁止出现 Rxx 轮次信息**——路线图已弃，轮次编号不再进入 git 历史，逐项交付内容只记 `CHANGELOG.md`）。

- **不做任何 git 写操作除非主会话明确要求**（多智能体协作时外部 Agent 只改代码）。
- dogfooding 游戏轮产出的引擎摩擦清单是**一级交付物**；游戏轮**不改引擎**（纪律），摩擦留给下一轮打磨闭合——每条按「现象 → 绕行 → 建议修法」记录进 ARCHITECTURE §8。
- 与规格偏离必须汇报并给理由（历史偏离多有充分依据，如 `&mut World` spawn、rest 姿势显式捕获、上帧深度复用单份 scene_depth）。
- 文档滞后是 bug：改行为必须同步 ARCHITECTURE 对应章节；测试数字散落在 README「测试」节与本文门禁命令序节两处，过期要顺手修（`_FEATURES` 文件已不在仓内，勿再引用）。
