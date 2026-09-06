# AGENTS.md — iris-engine 智能体工作指南

面向在本仓库工作的 AI 智能体（与人类贡献者）。逐条都是实战沉淀，违反过会付出真实代价的条目加粗。

## 项目是什么

Rust 游戏引擎（workspace 版本 0.1.0），核心卖点：**跨平台（native + wasm32）、内存安全（全库零 unsafe）、确定性（物理/模拟 bit 级可复现）、低占用、中文文档**。技术栈：wgpu 30 + winit 0.30 + 自研稀疏集 ECS + glam 0.33。

crate 布局（依赖自上而下）：

| crate | 职责 |
|---|---|
| `iris-macros` | Component/Resource derive |
| `iris-core` | ECS / 调度 / 层级 / 时间 / 物理 / 角色 / 导航 / ragdoll / 事件（零渲染依赖） |
| `iris-platform` | winit 窗口 + 输入 |
| `iris-audio` | 混音纯函数核心 + cpal 设备薄壳（wasm 无设备自动降级） |
| `iris-render` | wgpu 渲染（PBR/阴影/粒子/贴花/后处理链/动画/GLB） |
| `iris-engine` | 插件编排（DefaultPlugins）/ App / 场景序列化 / GltfBuilder / SmoothFollow |
| `iris-game` `iris-arena` `iris-bastion` `iris-ascent` | 四个 dogfooding 游戏（结构范式：`logic.rs` 纯核心 + `plugin.rs` 插件层 + headless 测试 + 快照 example） |

**架构与特性全景唯一权威是 `ARCHITECTURE.md`**（约 1700 行，§1–§8），改任何子系统前先读对应章节；`§8 已知限制` 是诚实清单，别把已声明的限制当 bug 修。

## 环境硬约束（Windows / Git Bash）

- **所有 cargo 命令必须加前缀 `CARGO_HTTP_CHECK_REVOKE=false`**（本机镜像 SSL 吊销检查问题，不加必挂）。
- `.cargo/config.toml` 已配 `rustc-wrapper = "sccache"`：新机器先 `cargo install sccache`，否则一切 cargo 命令失败。
- ffmpeg 在 PATH（验收图 BMP→PNG 转换用）。

## 门禁命令序（每轮收尾，顺序有讲究——详见 ARCHITECTURE §7.2）

```sh
cargo fmt --all --check
cargo build --release --workspace --all-targets   # 先行：一次编译全部产物（含 examples/benches）
cargo test  --workspace           --quiet          # debug 测试（编译全命中缓存）
cargo test  --workspace --release --quiet          # release 测试 + 预算断言
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --target wasm32-unknown-unknown -- -D warnings
```

**`cargo test` 不构建 examples/benches**——先 build 后 test 才不会把 release 编译两轮。

### 门禁耗时陷阱（R44 实测教训）

- **`RUSTC_WRAPPER=""` 只属于 wasm 那一条门禁**（绕 sccache 的 os error 206）。把它加到
  原生命令上 = sccache 全程失效 + 依赖图指纹改变 ⇒ 整条 release 图从源码重编
  （实测同一次测试：**30 分钟 vs 2 分钟**）。
- **门禁之间不得并行**：多个 cargo 任务共用一个 `target/`，会串行在文件锁上
  （`Blocking waiting for file lock`）并把 CPU 打满，耗时相乘。想省事请改用
  `--timings`（见下），不要并发跑。
- **怀疑「测试跑太久」先分清编译与执行**：`cargo test --workspace --release --timings`
  出 HTML 报告（编译并发与锁等待一目了然）；跑完立刻 `sccache --show-stats` 看命中
  （服务器空闲约 600 s 退出、统计清零，隔天看不到）。参考量级：本仓全量 release
  **测试执行**合计约 32 s，最慢单个二进制是 `iris_core`（≈ 6 s）。

### 测试计数陷阱

**统计测试总数必须逐二进制明细加总**（当前基线见最近 CHANGELOG 条目；撰写时为 debug 695 / release 694）。两个已知陷阱：
- `grep -v "0 passed"` 会把 `10 passed` 也过滤掉（子串匹配）——禁用；
- 两次独立的 cargo 调用输出无法对齐——二进制名与计数必须来自**同一次调用**的输出。

安全的加总方式：`cargo test --workspace 2>&1 | grep -oE "[0-9]+ passed" | awk '{s+=$1} END {print s}'`。

## 代码规约

- **中文文档/注释/断言消息**——公开项全带文档注释，模块头写语义与**诚实限制清单**（v1 做不到的明说，不藏）。
- **零 unsafe**，无例外。
- **确定性是硬文化**：需要迭代序的数据结构用 `BTreeMap`/`BTreeSet`（选型理由写进文档，参照 `broadphase.rs`）；随机性用无状态哈希流（splitmix64 + 计数器，参照 `particles.rs` 的 `mix64`），**禁止 rand 依赖与全局状态**；浮点确定性细节如 `sqrt` 优于 `hypot`（libm 无位级保证）。确定性测试模式：同输入两次跑，`to_bits()` 逐位断言。
- **纯函数优先**：数值逻辑提纯为可脱离 World 单测的自由函数/方法（参照 `physics.rs` 的 `solve_contact`）；系统用 `&World` + 尝试型锁 + 两阶段（收集→写回）模式，冲突即 panic（消息含 "already borrowed"）。
- **组件缺席语义**要写明（如：只有 Collider 无 RigidBody = 静态体；Visibility 缺席 = 可见）——缺席行为是 API 的一部分。

## 测试规约

- 断言用**精确值**（解析解对照、±1e-5 容差写明依据），不写「大于 0」式摆设断言。
- GPU 测试：`OffscreenRenderer::new` 返回 None 时 `eprintln!("跳过：环境无 GPU")` + return，不 panic。
- should_panic 测试若依赖 `debug_assert!`，必须 `#[cfg(debug_assertions)]` 门控（R33/R38 各踩过一次 release 假失败）。
- 测试间**共享文件名会竞态**（并行执行）——临时文件用唯一名或 Once 模式（gltf/hdr 各踩过一次）。
- 改物理/模拟必须有确定性测试；改渲染必须有像素断言测试。

## 已知平台陷阱（改对应子系统前先读）

- **DX12 下 depth-only 管线写入恒 0**——阴影用「深度写颜色 R 通道 + Depth32Float 双附件」方案；深度附件跨 pass 读取必须显式 `Store`（R23 教训）。
- `pollster` 在 wasm 忙自旋卡死主线程——GPU 初始化走异步核心 + 事件循环逐帧 poll。
- `World::spawn` 需要 `&mut World`；`&World` 的系统**无法注册新组件类型**（存储表不可变）——运行期动态组件要预挂池槽（参照 games 的对象池模式）。
- glam 0.33：`Mat4::from_translation` 收 `Vec3` 不收数组；矩阵列主序用 `to_cols_array_2d`。
- `Duration` 纳秒舍入：固定步长向内截断（`time.rs::set_fixed_hz` 注释详述——偶发差一步的元凶）。
- **偶发失败 = 真 bug 信号**（本仓库沉淀的铁律）：Duration 舍入、临时文件竞态、浮点边界（R39 盒面 1e-4 容差）都是这么抓到的。不重跑通过就放过。

## 工作流（每轮的完成定义）

1. 设计定稿（读码 → 决策写清）→ 2. 实现 + 测试 → 3. 六项门禁全绿 → 4. 涉渲染出离屏验收图并**目检** → 5. 文档三件套（README / ARCHITECTURE 对应章节 / CHANGELOG Unreleased）→ 6. 提交（中文 conventional commit，body 写设计理由与偏离声明）。

- **不做任何 git 写操作除非主会话明确要求**（多智能体协作时子智能体只改代码）。
- dogfooding 游戏轮产出的引擎摩擦清单是**一级交付物**；游戏轮**不改引擎**（纪律），摩擦留给下一轮打磨闭合——每条按「现象 → 绕行 → 建议修法」记录进 ARCHITECTURE §8。
- 与规格偏离必须汇报并给理由（历史偏离多有充分依据，如 `&mut World` spawn、rest 姿势显式捕获）。
- 文档滞后是 bug：改行为必须同步 ARCHITECTURE 对应章节，_FEATURES 数字（测试数等）过期也要顺手修。
