# HANDOFF.md — 主会话交接文档

> 面向上下文压缩后的主会话（我）。仓库工程规约的权威是 **AGENTS.md（先读它）**；本文档记录会话性信息：协作模式、验收 SOP、路线图位置、历史脉络。压缩后第一件事：读 AGENTS.md + 本文档。

## 1. 角色与协作模式（当前生效）

- **主会话（我）**：设计决策 → 产出提示词 → 独立验收 → git 提交。不写大块实现代码（R42 起实现全部由子智能体/外部 Agent 完成）。
- **用户**：极简中文指令驱动（"OK 下一轮"/"继续"）。**R44 起的模式**：我把提示词完整写给用户 → 用户发给外部 Agent 执行 → 外部 Agent 完成后用户把汇报发回（贴文本或仓库根的 `REPORT.md` 文件）→ 我验收后提交。继续沿用此模式：用户说"OK 继续"即要我发下一轮提示词。
- **外部 Agent 汇报文件**：`REPORT.md` / `report.md`（根目录，大小写不定）——**不入库，验收时删除**。
- 子智能体（Agent 工具）仍可用但近年少用（超时频发）；超时留半成品时：先 `cargo check` 探测完成度，再派续做智能体审计补齐或主会话亲自接手。

## 2. 验收 SOP（每轮外部 Agent 汇报到达后，顺序执行）

1. `tasklist | grep -iE "cargo|rustc|sccache"` 清残留进程（AGENTS.md 铁律——曾实测 40 分钟排队假象）；
2. `git status --short` 看改动面；删除 REPORT.md；
3. **后台复跑双 profile**：`cargo test --workspace --quiet | grep -oE "[0-9]+ passed" | awk '{s+=$1} END {print s}'` + release 同款（release 不加 RUSTC_WRAPPER=""）。**警惕管道截断假象**：数字与汇报不符时用单次完整调用重定向文件再对账（R52a 曾出现 860 vs 868 假象）；
4. 前台跑：`cargo fmt --all --check` → clippy native（不带 RUSTC_WRAPPER=""）→ clippy wasm32（带 RUSTC_WRAPPER=""）→ `cargo build --release --workspace --all-targets`；
5. **亲审关键代码**（每轮挑 1-3 处：数学/设计决策/偏离声明对应的实现）；
6. **目检验收图**（docs/*.png，Read 工具看图）；
7. `git add -A && git commit`——中文 conventional commit，body 详写设计理由与偏离声明（照 git log 近期风格）。

## 3. 当前状态快照（2026-09-08，HEAD = e0cff67）

- **测试基线：debug 905 / release 903**（AGENTS.md 的基线条目已过期时顺手更新）。
- 路线图 v3（写实动作游戏导向）**W4 阶段收官**：R42–R53 全部完成。
- 六个 dogfooding 游戏 + 引擎 10 crate；全库零 unsafe；CI 文件就绪但**项目无 git 远端**（R56 发布时需用户提供仓库地址）。
- 写实路线上限锚定「上世代写实」（法线/烘焙 GI/DOF），非目标：材质图、照片级、TAA（留候选）。

## 4. 路线图 v3 剩余（下一步从这里继续）

- **R55 生产化**：崩溃上报钩子、分发打包布局（itch/Steam 资产组织）、对外用户视角文档（现文档是建设者视角）。
- **R56 发布 0.1**：语义版本、docs.rs、示例矩阵整理、CHANGELOG 发布段；**需要用户提供 git 远端激活 CI**。
- 休眠项（随时可插队）：手柄输入（用户明确暂缓）、Android、wasm 深化、TAA、GPU 粒子拖尾、水面、载具、本地化。
- 用户此前说过游戏六（完整箱庭游戏，W5）是可选——未确认是否做，别自作主张开。

## 5. R42–R53 历史脉络（一句话/轮）

| 轮 | 内容 | 提交 |
|---|---|---|
| R42 | 法线/AO/ORM 材质通道（中性 16F 纹理逐位兼容 + FNV 哈希实证先例开创） | 25d3f34 |
| R43 | 景深 + 运动模糊（深度副本 pass；用户报 40min 构建超时 → 定位为残留进程持锁，AGENTS.md 铁律来源） | 06efcf5 |
| R44 | OGG/Vorbis 流式音频（symphonia 选型核验 forbid(unsafe)；音乐总线 + 混响；**外部 Agent 模式起点**） | 6a92cc4 |
| R45 | pack 资产打包 + AssetMount + 异步加载（CRC32 确定性；读 24.6µs vs 直读 46.3µs） | 2a540f7 |
| R46 | 动画 v4：附加层（增量右乘）+ 骨骼遮罩 + 手部两骨 IK（容差带） | c14f5e8 |
| R47 | 演出时间线 Sequencer-lite（catmull-rom 端点精确早退；collect_crossings 抽公共） | 9d02d33 |
| R48 | 游戏六《Iris Assault》垂直切片（六能力实弹；摩擦清单 7 条——含足部 IK 四元数爆炸引擎级 bug） | 7a8e7e9 |
| 打磨 VI | 闭合 7 条（IK 归一化回归锁"撤修复 14 帧即败"是黄金标准；main.rs 遗漏拦截） | 3d19ac0 |
| R49 | 3D 导航（NavGrid 高度场：双层 + flood 雕刻；octile 可采纳证明） | a9d6315 |
| R50 | 角速度物理 + 旋转胶囊 + SwingJoint（锥面拉回）+ ragdoll v2（绕肩线翻倒） | 7933e75 |
| R51 | 地形系统（heightmap 分块 3 级 LOD；边界线全采样接缝不变量；碰撞拼块零扩展） | d3ebb2a |
| R52a | 烘焙 GI 一：BVH + 无 RNG 采样 + 辐照积分器（解析测试灵魂；34.3ms/1k 点） | b4fff2b |
| R52b | 烘焙 GI 二：chart 分类 + shelf 装箱 + texel 回溯（正面积相交防串光） | d246f99 |
| R52c | 烘焙 GI 三：GPU 集成（select 混合语义非算术；dilation 防黑边；FNV 双锁） | 51fb95d |
| R53 | 遮挡剔除（硬件查询两阶段；LessEqual 修正）+ FXAA（专属线性采样器教训） | e0cff67 |

更早历史（R1–R41：ECS/渲染/物理/音频/五游戏/五打磨）细节已在 ARCHITECTURE.md/CHANGELOG.md，不必依赖会话记忆。

## 6. 给外部 Agent 提示词的固定骨架（发新一轮时照此拼装）

```
你在 Rust 游戏引擎仓库 D:\Development\Complex\iris-engine 工作（Windows，Git Bash；**不要执行任何 git 写操作**）。
引擎约定：全库零 unsafe、中文文档/注释/断言消息、native+wasm32 双 target 零警告、
cargo fmt --all --check 必须过（六项门禁顺序、测试统计陷阱、**门禁耗时陷阱**见仓库根
**AGENTS.md——先读它**；RUSTC_WRAPPER="" 只用于 wasm 那两条；**每跑一个门禁前必须先停止
所有 cargo 进程**）。所有 cargo 命令加前缀 `CARGO_HTTP_CHECK_REVOKE=false`。

本轮（RXX）：【设计决策：范围边界/关键算法/兼容硬约束/测试要求/验收图/文档三件套】

收尾门禁六项（含当前测试基线数字）……
汇报格式：……
```

关键设计习惯：每轮必带「既有测试零改动全过」或「默认像素零变化 + FNV 基线锁」类硬约束；偏离声明必须汇报理由；验收图必目检。

## 7. 用户偏好与红线

- 中文交流；用户是项目唯一决策人（裁剪/加轮都听他的，如"不裁剪 做R52 可分多阶段"）。
- 不做 git 远端操作（无远端）；提交全部由我执行。
- 手柄输入暂缓（用户明确）；非目标重申：不做可视化编辑器/网络多人/照片级。
- 用户实测过游戏（Infiltration 四问题反馈是真实 bug 流）——用户会玩游戏，验收图/演示质量要真实。
