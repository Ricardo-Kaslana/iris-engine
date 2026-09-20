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

**`cargo test` 不构建 examples/benches**——先 build 后 test 才不会把 release 编译两轮。当前基线 **debug 1792 / release 1789**（……前史：C4a~C7b 累计至 1685/1682，闭合战役 C0~C7 全部完成；R90 毛发 v1 +22：iris-render CPU 17（groom 确定性双跑逐位/数量公式/几何解析锚（y 单调·宽度线性·根部贴头皮）/关节共享/切线归一域；kajiya_kay 镜像 5（sin 垂直峰平行零/移位峰值角差 ≈4·shift/exponent 半宽单调/移位切线三锚/WGSL 源码看门）；fur 3（层比公式/展开保真/缺省量级）；material 2（Mask 缺省与 builder clamp/KK 缺省与 builder）；pipeline 看门 1（fur_aux 四方影子契约——中文注释的字节游标陷阱：+1 游标切进多字节字符，用全局 find 位置单调断言规避）+ GPU `fur_render.rs` 5（GPU_SERIAL 串行：缺席逐位+双会话/发带可见性（高密度帘覆盖中心）/壳层毛高单调+**毛高 0 零点锚**（与不挂壳层整帧逐位）/Mask 三档阈值（纯垫板参考渲染法——垫板 IBL 镜面残余 ≈37/通道是非零解析底值）/复合场景双会话逐位）；全部 opt-in「缺席 = 逐位不变」，shadow_v2/sss/normal/transparent 既有像素锁零改动原值通过（KK 走独立贡献函数 + uniform 门，`pbr_light`/`point_light_contrib` 本体零触碰——R63 FMA 漂移教训的兑现）；**排障教训：`MeshStore::add` 从 0 起（与 MaterialStore 的 0 号预留不同）——判别位不得借用句柄值**（首版 mesh==0 哨兵与合法网格句柄 0 相撞，改显式 FurKind）；R92 降水 +18：iris-render CPU 12（weather 8：湿/雪端点逐位（重心式——mix 非规范保证且线性插值式 a=1 实测 2 ULP 残差）/坡度门单调/格哈希散布/涟漪有界确定间歇/geometry 覆盖/源码看门；particles_v2 4：阻尼终端速度 g/k 解析收敛+零阻尼逐位恒等/体积门逐位还原原点+散布满幅/雨雪预设字段与稳态预算 ≤ 池容量/advance 携带三旋钮）+ GPU `weather_render.rs` 6（GPU_SERIAL：缺席与全零资源逐位同构/湿 0-0.5-1 单调暗化（俯视机位——掠射 Fresnel 天空镜面会抵消变暗）/积雪墙地增量比 ~7%/雨渲染+移动+双会话逐位/雪渲染+双会话逐位/涟漪 wave_speed=0 隔离判别）；**排障教训：降水可见性的三重光度学下限**（256px 视口 + alpha 软边 + 色调映射——诊断链：draw call 与三角数照常但零像素 ⇒ 逐旋钮二分（fountain+微体积 6373 px 定位到大体积覆盖率）⇒ 加法混合 1724 vs alpha 145 定位到暗剪影对比度 ⇒ 深色化预设 + 测试放大丝宽至 0.45）；R93 海洋 v2 + 浮力 +43：iris-core CPU 21（buoyancy 14：球冠三锚点 / 平放盒线性闭式 / **OBB 三次 box spline 闭式 vs 16³ 中点积分收敛** / OBB 恒等姿态 ≡ Box / 90° 三轴互异取轴硬锚（专治旋转矩阵列 vs 行的静默转置）/ 过中心平面任意姿态精确对半分 / 端点 0-1 逐位 / 中性浮力体积判据 / to_bits 双跑；physics::buoyancy_tests 7：**净加速度按密度比解析对照** / 中性体 2 s 不动 / 半密盒收敛到一半吃水 / **缺席与零浸没轨迹逐位 ≡ legacy**（含「水位落在轨迹上时必须改位型」的判别力旁证）/ 无碰撞体飞行体不参与 / **浮力不改旋转位型**（无扶正力矩））+ iris-render CPU 14（ocean：平面表确定 + Nyquist 界 / RMS 归一化与带限能量单调 / ω 量化到 2π/T 格点 + 误差有界 / 时间 Nyquist 余量**上下界** / 烘图逐位 + 末帧回绕 0 + 通道布局锚 / f16 同量 / 重心式端点逐位 / 帧对全循环 + 负时间 / 环步距与细节权重 + 亚步吸收 + 越步换格 / 淡出单调 / 峰高与均值界 / **谱斜率搬能量到短波且不改 RMS** / WGSL 影子契约看门）+ GPU `ocean_render.rs` 6（GPU_SERIAL：在场改哈希 + 覆盖面积 / 无海洋帧可复现 / **海+池塘 ≡ 只有海**（互斥不是画了再盖）/ detail_amplitude 活旋钮 + 近场条带差分 / **far_extent 收缩让 100 m 落点从海翻成天空 + 环带差分** / 双会话逐帧逐位 + 时间改哈希）+ iris-engine 2（三段序列化往返 + 退化拒绝 + 旧档零变化 / 同步系统六档 opt-in）。**排障教训入档：① 掠射海面不能用「比天空暗」做判据**（3 m 机位看 100 m 外俯角几度 ⇒ Fresnel 趋 1，海就是天空的镜子，实测海 [214,222,230] vs 天空 [215,220,225] 反而略亮——判别改走几何落点 + 环带差分）；**② 谱斜率是观感量不是物理量**（Phillips 档振幅 ∝ k⁻² 使**斜率谱 ∝ k⁻²**、法线扰动全被最长波吃光，验收图细节开/关亮度差 < 1/255 目检不过 ⇒ `spectral_slope` 默认 1.5）；**③ 同一 WGSL module 是一个全局命名空间**（两路 `@group(0)` 同 binding 号不同类型 = naga 直接拒绝，「同文件加 entry point」不成立 ⇒ 改 Rust 侧拼接 common 段，见 §4.52）；**④ 快照里 Time 必须插进世界再推进**（留在局部变量 ⇒ step_physics 的 dt = 0，浮体全冻在出生高度）。；R94 水下 + 焦散 +16：iris-render CPU 7（underwater：入水边界等号算水上 / 列长四档解析锚 + 朝上单调 / 吸收 1/e 锚与下限防御 / 焦散有界·非常数·逐位·有亮筋 / 焦散随列长衰减 / 资源默认与消毒 / **WGSL 影子契约看门**）+ post 链槽 2（水下关 ⇒ 与雾档逐字段等价 16 组合；32 组合拓扑不变式：读槽≠写槽 / MB 源=前驱 / 链头=最后入链 pass）+ GPU `underwater_render.rs` 6（GPU_SERIAL：缺席与在水上逐位不变 + 判别力旁证 / 入水改画面 + 纯吸收档压暗 + 覆盖面积 / absorb_depth 单调**隔离混淆源** + R−B 色相方向 / 焦散与抖动到达像素 + 焦散只抬亮 / 雾与水下同链三者互不相同 / 双会话逐帧逐位 + 时间改哈希）+ iris-engine 1（underwater 段往返 + 三档退化拒绝 + 旧档零变化）。**排障教训入档：① 散射项必须带水色着色**（首稿 `sun_color × strength × (1−T)` ⇒ 3.0×0.35≈0.84 的白覆盖，验收图整幅洗白；改 `water_color × sun_color × …` 后自然落到 0.1 量级）；**② 吸收单调性只在纯吸收档成立**（默认档含内散射 / Snell 窗 / 焦散三处加性项，`bg·T + L_ins·(1−T)` 第二项随消光抬亮 ⇒ 实测「越浑越亮」是物理正确，用它当吸收判据即错指标）；**③ 每测试一只设备**（单文件 30+ 只 OffscreenRenderer ⇒ 驱动侧在第五六个测试返回 None，单独跑每个都过、整文件跑挂；各臂世界同构造函数生成、句柄与内容一一对应 ⇒ 复用不触发「同句柄不同内容串味」）；**④ u8 亮度求和先升 u32**（`px[i]+px[i+1]+px[i+2]` 在 debug 下溢出 panic，测试自身也要过门禁）。每轮收官顺手更新本行数字）。

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
调用加总，禁用 grep -v "0 passed"，基线 debug 1792 / release 1789）、
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

## 当前状态与路线图（2026-09-20 更新）

- HEAD = R94 水下 + 焦散（第三梯队收官）。测试基线 debug 1792 / release 1789；全库零 unsafe；native+wasm32 双 target 零警告；ARCHITECTURE §8 诚实清单现 73 条；**无 git 远端、无 tag**（workspace 版本 0.1.1，R56 发布时需用户提供远端）。
- **第三梯队（路线图 v5 硬科技纵深）就此收官**（R84–R94 十一轮全部交付）。**下一步待用户裁定**：W8 完整 Assault R69–R72（未获「OK」不开工）/ W9 收官 R55 生产化 + R56 发布 0.1（需先给 git 远端）/ 闭合轮池按需穿插。
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
    漏清时域状态）；诚实限制 §8.58，闭合 frictions ⑬）、R90 毛发 v1 ✅
    （双路线：实例化壳层 shell fur（层比进实例数据 + FS 统一 cutout，
    base 贴图 alpha 即毛长图）× Kajiya-Kay 相机面向发带（groom 程序化
    生成 + hair_vs 屏幕展宽（投影常量从 view_proj 列长推导，零 Globals
    扩容）+ fs_shaded 材质 flag 分支（R63 FMA 教训兑现：`pbr_light`/
    `point_light_contrib` 本体零触碰，既有像素锁原值通过））+ AlphaMask
    （glTF MASK 闭合，阴影实心近似）；设计四点裁定全按推荐；缺席 =
    逐位不变；诚实限制 §8.69）、R91 毛发 v2 ✅（CPU Verlet 风摆
    （strand_step 纯函数+根部钉扎+单遍距离约束；wind_sample 根位置
    采样；世界空间模拟局部写回）× alpha 感知毛发阴影（光空间展宽/
    层比膨胀 + fs_fur 同一判定语义——**§8.69③ 闭合**）× 交叉卡片
    LOD（同一随机字契约+滞回 20%）；全部 opt-in 缺席 = R90 逐位
    不变；诚实限制 §8.70）、R92 降水 ✅（雨雪发射器（v2 扩展三旋钮：
    体积出生/任意轴向/线性阻尼，终端速度 = g/k 物理 量级；rain()/snow()
    预设挂相机实体即得跟随体积；sim 三 select 门 = R82 逐位不变）×
    湿表面（粗糙度 0.08 + 反照率 (1−metallic)×0.7，**重心形式**混合
    ——mix 实现形式非规范且线性插值式 a=1 实测 2 ULP 残差）× 程序化
    涟漪（XZ 哈希网格环形波无状态纯函数，湿地表 + 雨打水面两处同钟
    消费，WGSL 双份镜像）× 积雪坡度遮罩（几何法线 y smoothstep 门，
    墙地增量比 ~7% GPU 可判）；binding 21 uniform（不扩 Globals 先例，
    主相机/捕获/RT 共享一只缓冲——天气是世界状态）；设计四点裁定
    全按推荐；缺席/全零 = 逐位不变（七套像素锁原值通过）；诚实限制
    §8.71）、**R93 海洋 v2 + 浮力 ✅**（2026-09-20 收官，主会话直接实现；CPU
    Phillips 型格点谱合成 + ω 量化到 2π/T ⇒ 16 关键帧无缝回绕的
    `texture_2d_array` 位移图（通道含**解析梯度**）× 相机锚点量化双层网格
    （近 ±24 m 吃细节位移 / 远 ±320 m 只涌浪，淡出终点 = 远场起点 ⇒ 无竖直
    台阶，天然闭合 §8.50⑧）× iris-core `BuoyancyPlane` opt-in 浮力（球冠 /
    AABB / **OBB 三次 box spline 闭式**浸没体积，缺席与零浸没 = 轨迹逐位
    不变）× 水体/海面/浮力三段序列化补账（R81 起从未入档）；池塘
    `vs_main`/`water_pipeline` 一字未动，海洋走 `water_common.wgsl` **Rust 侧
    拼接**出的独立 module + 独立组 0（同 module 两路 group(0) 会撞 binding
    号——实测 naga 拒绝）；设计三点裁定全按推荐（拆两轮 / 预烘关键帧 /
    浮力落 core + engine 同步）；六条实现偏离声明见 CHANGELOG；R81 四套 +
    R92 六套像素锁与六游戏轨迹锁**原值通过**；诚实限制 §8.72 十四条）、
    **R94 水下 + 焦散 ✅**（2026-09-20 收官，第三梯队就此收官；`UnderwaterFog`
    资源 + 链上第四级全屏 pass——列长走**解析平面求交钳制**（零新资源、不改水
    pass 深度写入、Snell 窗可见）× 合成 = Beer-Lambert 吸收 + 带水色着色的内
    散射 / Snell 窗 + 屏幕空间焦散 `((a+b)/2)₊³` + uv 抖动；链槽位新函数在水下
    关闭时直接委托既有函数（逐字段等价由构造给出）；四点裁定全按推荐；缺席 /
    相机在水上 = 整段不入链，post 13 + sky_fog 5 + water 4 + ocean 6 + weather 6
    既有像素锁原值通过；诚实限制 §8.73 十条）——第三梯队按 2026-09-20 追加裁定
    由 1 轮拆为 2 轮，两轮均已交付
  - **闭合战役 C0~C7 ✅ 全部收官（2026-09-19，8/8；2026-09-18 用户裁定「暂停新功能先清账」，
    计划全文 `docs/archive/closure-plan-v1.md`（`d386b60` 起归档入库）**：C0 文档健康 → C1 原语小件 →
    C2 渲染伪影（贴花受光 v2 / 光照贴图 mip / 粒子 v2 序列化 / 反射探针编辑入口）→
    C3 物理深化（CCD / 铰链+速率马达 / 可断关节 / C3d OBB×OBB SAT 闭式）→
    C4 动画（形变目标 v1 / 状态机 v2+运行时按名重定向）→ C5 格式（BC7 Mode 6 / KTX2 /
    场景参数热重载）→ C6 音频空间化 v3（多普勒 / 二值遮挡 / 逐声音低通）→ C7 大裁决
    （DDGI v3 同源天空+动态光源进 GI / Weighted Blended OIT）。总账：21 项排期债清算完、
    **全程零基线打破**（清一色 opt-in「缺席/默认 = 逐位不变」）；按三门裁出回池 HRTF、
    动画压缩、凸包 GJK/EPA 三项；iris-exp ⑫「引擎光照口径（皮肤泛白）」维持挂账。
  - 闭合轮池（战役后仍在池，按需穿插）：LOD v2 / RVO 群体 / 凸包 GJK+EPA / 动画压缩 /
    HRTF 空间化 / 贴花 v2 余项（§8.19 直射项已随 C2 闭合）
  - W8 完整 Assault（游戏七，dogfooding 纪律）：R69–R72（未获用户「OK」不开工）
  - W9 收官：R55 生产化、R56 发布 0.1（语义版本/docs.rs/CHANGELOG 发布段）
- 实验项目：`games/iris-exp`（外部 GLB 角色 + Mixamo 重定向动作导入验证；
  引擎侧摩擦独立成档 `docs/private/iris-exp-frictions.md`；最新一笔 = `3debd8f`
  中距离朦胧修复：视觉地面 24 m→240 m、天空盒地平线以下渐变不再冒充雾）。
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
