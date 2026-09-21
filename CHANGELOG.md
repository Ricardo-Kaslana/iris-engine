# Changelog

本文件记录 Iris 引擎的所有重要变更。格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本遵守 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Added（iris-exp 角色材质升级与三点布光）

iris-exp 在 GLB 导入后按节点名覆写四类材质，并补齐近景角色观感所需的
布光。皮肤节点（身体与脸部两个 primitive）开启预积分次表面散射（强度
0.55，曲率尺度 0.22 米——引擎调参表按真人尺度给值，本角色身高约为真人
的两倍，按等比放大）；眼球材质的粗糙度从全镜面 0 抬到 0.12，角膜高光
恢复自然；头发与睫毛抬到哑光粗糙度 0.85。布光在方向光之外新增一盏冷色
轮廓锥光（角色背后上方，负责肩部与发缘的轮廓分离）与一盏暖色补点光
（相机侧填充暗部），两者均不投影。

头发贴图本身接近纯黑，任何可见的定向高光在实机分辨率下都读作灰白，
因此哑光黑是该资产的终点；Kajiya-Kay 的适配边界与完整证据链记录在
iris-exp frictions ⑳。快照工具新增三个诊断机位（`EXP_CAM_HEAD` 正面
特写、`EXP_CAM_CROWN` 头顶俯视、`EXP_CAM_SIDE` 侧斜俯，均按 gMan Head
骨骼实时定位）与 `EXP_RES=宽x高` 分辨率覆盖，材质验收从此覆盖三视角且
不低于 720p。验收图 `docs/exp.png`（四联）与 `docs/exp_closeup.png`、
`docs/exp_crown.png`、`docs/exp_side.png`（720p 三视角）随本轮更新。

### Changed（引擎：Kajiya-Kay 高光染色乘子 highlight_tint）

`KajiyaKay` 新增 `highlight_tint` 字段（线性 RGB，逐分量乘在高光色
mix(白, albedo, 0.5) 之上）。R90 的固定高光色按中等深度发色设计，近黑
毛发会把瓣色钳到灰白量级，实机观感读作漂白；染色乘子让深色毛发把高光
压回发色域。serde 缺省值 [1, 1, 1] 与 R90 固定式逐位一致（IEEE 乘 1.0
精确，无分支无重排），旧场景文件反序列化按缺省补齐。材质 uniform 从
80 字节扩到 96 字节，`standard.wgsl` 与 `shadow.wgsl` 的影子结构同步，
布局由 `MaterialRaw` 编译期断言与 pipeline 源码看门钉住。CPU 侧新增镜
像函数 `kajiya_kay::highlight_color`，配 R89 非默认断言、缺省逐位锚与
serde 兼容测试；`fur_render.rs` 八项 GPU 测试原值全过，既有像素基线不
变。

### Added（iris-exp 动作自然过渡：三状态机 crossfade + 停步运动感）

iris-exp 的动作切换此前是硬切——直接改写 `player.active` 并把时间归零，
Idle、Walk、Jump 之间的姿势瞬间跳变，松键即急停。本轮把切换层整体迁到
引擎已有的动画状态机上，并补齐停步与起步的运动感。

三状态机（Idle 与 Walk 循环、Jump 空格单次）在装配期写入播放器：手动
边表给起走 0.15 s、起跳 0.08 s 的 crossfade 时长；跳跃播完不再由游戏侧
逐帧轮询，改由引擎条件转移自动回落——`ClipFinished` 守卫读每帧写入的
`moving` 参数分流，静止回 Idle、移动中直落 Walk（落地即走），回落带
0.10 s 淡出。

停步不立即切换，先等步行剪辑走到「姿势距 Idle 最近」的过步窗口。窗口
不是估出来的：对 Mixamo_Walk 全部 261 个通道逐分量计算对 Idle 首帧的
L1 距离曲线（96 相位采样），循环末尾（相位 0.93 到 1.0，两脚并拢触地）
距离最深，为 10.1 到 10.6；相位 0 到 0.10 与 0.57 到 0.70 两段次窗口的
距离在 12.9 到 13.7，与循环首帧同量级。三段窗口把最大等待压到约半步
（0.45 s），等待期速度保持全速走完最后一步，脚步与位移匹配。曲线峰值
在相位 0.26 与 0.52（满幅分腿，17 到 18），正是窗口要避开的相位。窗口
数字配了内容契约锁测试，GLB 换版导致步行循环相位挪位时会报警。停步
方向的过渡时长定 0.10 s，比起走方向短——同样的姿势差用更短的窗口
覆盖，读作「收住脚步」而非「形体渐变」。

速度平滑消灭急停打滑与瞬发窜出：起步从零按 6/s 指数加速到 1.8 m/s
（约 0.5 s 到 95% 全速），停步在相位门放行后按 20/s 指数减速，残差低于
0.05 m/s 时精确归零。此前位移是瞬发的：松键下一帧位置冻结而姿势还要
0.15 s 淡出，读作急停打滑；起步则是站立帧全速窜出。「无输入帧恒等」
语义相应精确化——速度非零的帧照常积分位置、相机照常跟随最后几厘米；
完全静止后的无输入帧才恢复不写相机与角色 Transform。

过渡中间姿势的视觉验收有一条采点经验：要采「源姿势与目标姿势差异大」
的方向。Walk 剪辑开头 0.1 s 的姿势本身接近站立，起走方向的过渡中段与
纯待机肉眼难分辨；停步方向以过步姿势为源、淡出进行到约一半时最可
判别。离屏验收图 `docs/exp.png` 重生成为四联（待机、满幅迈步、停步
淡出中段、跳跃腾空），双跑逐位一致。

测试：iris-exp lib 单测 5 到 7（`exp_approach_vec3` 的解析解与切分
无关性、`stop_phase_ready` 的窗口隶属与相位回绕）；headless 集成 6 到
11（crossfade 生命周期含硬切对照——过渡中段姿势可判别地不同、完成后
与硬切逐位一致；跳跃落地自动分流双变体；停步相位门的窗外保持与过窗
切换；起步加速与停步减速的位移等于速度平滑纯函数的镜像积分、归零后
无输入帧恢复逐位恒等；停步窗口的内容契约锁；插件装配单次推进回归
锚）。

### Fixed（iris-exp 窗口路径动画双倍速）

`ExpPlugin::build` 此前在注册自身系统之后又 build 了一次
`AnimationPlugin` 与 `HierarchyPlugin`，而 `main.rs` 的 `DefaultPlugins`
已经包含这两者。调度器不去重，`update_animations` 每帧执行两遍，窗口
二进制里动画自始至终以双倍速播放；headless 测试台只注册一次，所以
既有测试从未暴露它。实证过程：临时恢复双重注册后，合成播放器单帧
推进两个 dt（0.033333 s），修复后恰好一个 dt。修复遵循 iris-arena 的
既有约定（其插件文档明示重复注册会让动画双倍速），`ExpPlugin::build`
只注册自身系统；窗口路径动画速度回归一倍，与 headless 及离屏路径、
以及 MOVE_SPEED 的步幅匹配推导（0.967 s 每循环、1.8 m/s）对齐。回归
锚测试不依赖资产，任何环境常驻生效。

### Added（R95 RVO 群体模拟：ORCA 互避速度决策纯函数库）

这是 0.1.2 发版后从闭合池抽出的第一项。六项设计决策由用户裁定全部按
推荐方案定稿，设计定稿存档于 docs/archive/r95-rvo.md。新增 iris-core
的 rvo.rs 模块，内容是一个纯函数库：调用方每帧把群体组装成切片并调用
`rvo_step(agents, obstacles, params, dt)`，得到每个 agent 本帧的避让
速度；位置推进 `pos += vel·dt` 由调用方自己做，因为角色位移需要经过
`move_and_slide` 与墙体碰撞，引擎不应该替使用者走位。同帧的全部新速度
都从旧速度解出后才统一写回，先解的 agent 不会改变后解者的邻居输入。
这个模块不进入 step_physics，不定义任何 ECS 组件，也不注册任何引擎
系统，因此对存量行为没有影响；六个游戏的轨迹快照测试原值通过，是这个
事实的直接推论。使用时还有一个要点：ORCA 求解读的是当前速度而不是偏好
速度，出生时速度为零的 agent 首帧不产生任何约束，调用方通常在出生时把
vel 置为 pref_vel。

算法采用 ORCA（van den Berg 等人 2011 年论文），实现口径跟随 RVO2
参考库：

- 感知范围内每一对 agent 生成一条半平面速度约束，双方各承担一半让行
  责任；静态圆障碍使用同样的几何，但让行责任全在 agent 一侧。求解过程
  是把偏好速度投影到全部约束的交集上，用增量式二维线性规划完成，对应
  RVO2 的 linearProgram1/2/3 结构。
- 当前相对速度不会在时间视野内造成接触的一对，同样会生成约束线。这条
  线的作用是给双方的接近速度设上限：当前速度没有越过它时不起任何作用，
  某一方试图加速接近时才把它压回来。最初按 ORCA 论文的字面口径（只对
  速度障碍内部的组合生成约束）实现，30 对 30 的对穿场景实测最深重叠
  −0.71 米，让行动作又晚又急；改回 RVO2 口径（安全对也生成约束线）后
  提前让行，配合下文两处修正实测收敛到 −0.01 米量级。
- 约束线分圆弧与切线腿两个分支；切线腿的左右选择按叉积符号决定，恰好
  落在轴上时取右腿，tie-break 全部写死并文档化。已经重叠的一对（中心
  距不超过半径和）用 1/dt 的时间视野生成弧约束，即要求在一帧之内摊开
  重叠，这是 RVO2 的同款处理。
- 线性规划不可行时的回退（LP3）：保留全部障碍线，在与前序约束线交点
  构成的投影线集上按运动方向重新求解。没有实现 RVO2 的 linearProgram4
  渐进删线，极端不可行配置下的解劣于原库，已列入诚实限制清单。

确定性契约（模块头文档即规范，逐条有测试锁定）：

- 同样的输入（包括切片顺序）调用两次，全部 agent 速度的 `to_bits()`
  逐位一致；没有随机源、全局状态或时间源，τ 与 dt 都由调用方传入。
- 切片顺序只通过等距 tie-break 和障碍线生成顺序影响结果：邻居距离互异
  时打乱切片顺序，结果逐位不变；等距并列与邻居数量截断的边界处以索引
  定胜负，此时顺序是输入的一部分。
- 邻居按距离升序选取，相等时取索引较小者，超过 max_neighbors 截断。
  障碍线在邻居线之前生成；障碍感知半径为 `τ_o·max_speed + 半径和`，
  与 agent 的 neighbor_dist 无关（RVO2 口径）。
- 内核算子限制在加减乘除、开方与比较，源码级看门测试禁止超越函数。
  RVO_EPSILON（1e-5）与 LP3_PARALLEL_EPSILON（1e-6）的数值视为公开
  契约，有锁值测试。
- 入参消毒：出现非有限值、非正半径或非正时间视野时直接 panic 并报告
  索引，NaN 不会静默进入求解器。

测试（CPU 共 18 项）：

- 解析锚点五组：对穿镜像对的解互为镜像，弧分支下半责任让行把双方前进
  速度从 0.5 降到 0.425；快速接近触发切线腿分支的右侧让行；静态圆障碍
  的切线绕行，360 步后从障碍另一侧通过，与障碍中心的距离不低于半径和
  1.4（容差 1e-4）；障碍全责时 agent 独自承担全部修正量，前进速度从
  3.0 降到 2.4；无邻居时速度钳制到 max_speed 逐位一致。
- 长时程不变式：30 对 30 对穿，场地中心放一个静态圆障碍，共 900 步，
  全程逐帧断言最小成对间隙不低于 −0.02 米。该阈值是离散帧浅擦界：
  连续时间下 ORCA 承诺无碰撞，离散帧下贴边穿越的深度上界是求解时刻的
  贴边量加一帧内的接近量（测试推导取相对速率上界 2.4 m/s，一帧内最多
  再接近 2.4·dt = 0.08 米），阈值取上界的四分之一。实测最差 −0.0101
  米；关闭 RVO 的对照组是真穿透 −0.40 米。
- 其余各项：确定性双跑逐位一致；切片顺序不变性；等距并列的行为锁
  （邻居上限取 1 时，两种摆放让解偏向不同侧，证明并列由索引裁决）；
  截断行为；算子看门与容差锁值两条；完全重叠的退化输入产生有限结果；
  消毒 panic；rvo_step 不改动位置；避让的暂时性（离开后恢复偏好速度）；
  障碍移出感知半径后不再参与决策。

验收图 docs/rvo.png 来自 rvo_snapshot example，左右两栏对照，每 15 步
画一条轨迹带，场地中心放障碍柱。左栏关闭 RVO，两群直线穿插、互相
重叠；右栏开启 RVO，两群在交汇处分开绕行，并绕开中心障碍柱。example
内嵌四组断言：全程间隙的对照组比较、左群质心越过中场（绕行而非停在
原地）、同一世界两次渲染的 FNV-1a 全帧哈希相等（验收时实测值为
0xfdddfef0ec62c704）、左右两栏像素差不低于 500（实测 16622，证明两栏
确实画出了不同的轨迹）。

实现期有两处修正改变过语义，记录在案：

- 弧分支的线法向必须恒取相对速度方向 ŵ，偏移带符号。第一稿用向量
  `u = (ρ−|w|)·ŵ` 自身的方向定法向，系数 (ρ−|w|) 在对穿安全对上为负
  值，法向随之翻转，可行半平面倒置，结果是把安全对强制加速推向对方；
  调试轨迹显示某个 agent 第 0 步速度从 1.5 被推到 1.75，方向朝向障碍。
  修正后安全对的约束线自带接近余量。
- 重叠对最初用 τ 口径的分离约束，力道不足：同群追尾只被轻微推动，全程
  间隙 −0.34 米；改为 1/dt 口径后 −0.010 米。

参考实现对账：RVO2 官方仓库已从 GitHub 下架，最终按社区 Python 移植
chengji253/RVO2-python 逐式核对 linearProgram1/2/3 与线生成分支后
定稿。默认参数对齐 RVO2 实战值，max_neighbors 取 10：取 6 时混战中的
约束覆盖不足，线性规划回退频发，实测全程间隙变差。可调字段为 radius、
max_speed、neighbor_dist、max_neighbors 与两个时间视野，公开字段即
文档。

基线：debug 1792 → 1810，release 1789 → 1807（本轮 +18），AGENTS.md
门禁节数字同步更新。

## [0.1.2] - 2026-09-20

### Added（R94 水下 + 焦散：解析列长钳制 × 屏幕空间焦散 × 链上第四级全屏 pass）

路线图 v5 第三梯队收官役下半（裁定 5 拆轮）。设计四点裁定（2026-09-20，
主会话按推荐定稿）：**列长判据 = 解析平面求交钳制**（不改水体 pass 的深度
写入、不新增掩膜纹理）；**链位 = 雾之后、DOF 之前**（空气雾是场景内的
气溶胶、水是相机所在介质；下游 DOF / MB / 泛光应作用于已被水吸收的颜色）；
**焦散 = 水下 pass 内的屏幕空间加性项**（不进 PBR、不动 fs_shaded）；
**资源形态 = `UnderwaterFog` 即开关**（引擎不猜「用户想不想要水下」）。
兼容铁律：资源缺席 **或** 相机在水面之上 ⇒ 整段不入链（零 write_buffer、
零 pass、链拓扑逐字段等价 R93）——`post_render` 13 项 / `sky_fog_render`
5 项 / `water_render` 4 项 / `ocean_render` 6 项 / `weather_render` 6 项
既有像素锁全部原值通过，零基线打破。

- **解析列长（裁定 1 的兑现）**：水下列长 = `min(到几何的线性视距, 视线穿出
  水面的距离)`，后者由 `d·head/rise`（`head` = 水面在眼点上方的量、
  `rise = dir.y·d`）解析给出——视线水平或向下 ⇒ 永不出射 ⇒ 列长 = `d`。
  这样水面像素（其 scene_depth 是水面**之后**的远平面）不会被误算成整段
  深水，从水下仰望的 Snell 窗因此可见。被否决的两条替代路线：让水 pass
  写 scene_depth（同 pass 读写 hazard，且翻掉 §8.50⑥「水像素不写
  scene_depth」⇒ 一串既有锁要重裁）；新增一张全分辨率 `water_depth` 掩膜
  + 给水体 / 海洋 pass 各加一个颜色附件（资源与改动面双涨，收益相同）。
  代价明文声明：波面被当成常量平面 `height`，浪高不参与列长。
- **水下 pass（`underwater.rs` + `underwater.wgsl`）**：复用
  `post_depth_layout`（params / 链色 / 采样器 / scene_depth 四槽，**零新
  布局**）+ `make_post_depth_pipeline` 同族构造；合成 = Beer-Lambert 吸收
  与水体色（全链**重心式**，不写 `mix`）+ 太阳内散射 + Snell 窗补光 +
  焦散 + 屏幕空间抖动。深度与位置重建取**同一纹素中心**（R77 半纹素教训）。
- **链槽位（`post::full_chain_slots_with_water_column`）**：既有两个槽位
  函数（`full_chain_slots` / `full_chain_slots_with_fog`）**一字未动**，
  新函数在水下关闭时**直接委托**既有函数（逐字段相同由构造给出，不靠测试
  兜底）；开启时链头前插两级 ⇒ 相机级链再多轮转一格（雾 A → 水下 B →
  DOF 对面 → …）。新增两条拓扑测试：水下关 ⇒ 与雾档逐字段等价（16 组合）
  + 全 32 组合的「读槽 ≠ 写槽 / MB 源 = 前驱输出 / 链头 = 最后入链 pass」
  不变式。
- **焦散（裁定 3）**：`caustic_intensity(p, t)` = 两个方向相位场互扭后的
  余弦和正部三次幂——`a, b ∈ [−1,1] ⇒ ((a+b)/2)₊³ ∈ [0,1]` **有界由构造
  保证**（单测锁）；三次幂把能量压成窄亮筋 + 宽暗区。只落在几何命中像素
  （天空像素 `d = z_far` 不参与）、随列长按 `exp(−column/absorb_depth)`
  衰减。CPU↔WGSL 逐式镜像 + 源码看门（WGSL 无方法调用语法 ⇒ 镜像是
  「同式不同形」：`sin(q.y·1.3 + t)` vs `(q.y*1.3+t).sin()`，看门按各自
  形态锁死）。
- **六个公开旋钮**（每个都有非默认值断言，R89 铁律）：`color` /
  `absorb_depth` / `sun_inscatter` / `sky_window` / `caustic_strength` /
  `wobble`；`is_valid` + `clamped` 消毒口（与 `Weather` 同族）。
- **序列化**：场景环境段新增可选 `underwater`（缺席 = 无水下资源），
  越界参数（负特征深度 / 负强度 / 非有限值）在加载期 `OutOfRange` 拒绝，
  旧档重存不凭空多出本段。
- **两条实现期归因（都不是 bug，但都会骗人）**：① **散射项必须带水色
  着色**——首稿 `L_ins = sun_color × strength × (1−T)`，实测 3.0 × 0.35 ≈
  0.84 的白覆盖整幅，验收图被洗白；改成 `water_color × sun_color × …`
  后散射亮度自然落在 0.1 量级（水分子的散射光带着水的颜色），旋钮量级
  随之重标。② **吸收单调性只在纯吸收档成立**——GPU 测试首稿拿默认档
  （含内散射 / Snell 窗 / 焦散三处加性项）断言「越浑越暗」，实测反向
  （`bg·T + L_ins·(1−T)` 的第二项随消光抬亮，物理正确）；改为隔离混淆源
  后断言成立，并补一条「吸收色改变必须体现在 R−B 符号翻转」的色相判据
  （单看亮度会漏掉「色相没生效」这一类错）。
- **测试**（CPU 7 + 链槽 2 + GPU 6 + 序列化 1）：`underwater.rs` 单测 7
  （入水边界等号算水上 / 列长四档解析锚 + 朝上单调 / 吸收 1/e 锚与下限
  防御 / 焦散有界·非常数·逐位·有亮筋 / 焦散随列长衰减 / 资源默认与消毒 /
  WGSL 影子契约）；`post.rs` 新增 2 条拓扑测试；`tests/underwater_render.rs`
  6（GPU_SERIAL：缺席与在水上逐位不变 + 判别力旁证 / 入水改画面 + 纯吸收
  档压暗 + 覆盖面积 / absorb_depth 单调与色相方向 / 焦散与抖动到达像素 +
  焦散只抬亮 / 雾与水下同链三者互不相同 / 双会话逐帧逐位 + 时间改哈希）；
  `scene.rs` 1 条三段往返 + 退化拒绝 + 旧档零变化。
  **套件级排障**：本文件每测试一只设备（`Gpu` 结构）——单文件 30+ 只
  OffscreenRenderer 会让驱动侧在第五六个测试返回 `None`（单独跑每个测试
  都过、整文件跑挂），而各对照臂的世界由同一构造函数生成、句柄与内容
  逐一对应 ⇒ 复用渲染器不触发 AGENTS.md「同句柄不同内容串味」的前提。
- **验收图** `docs/underwater.png` 三面板（水下海床 + 焦散 | 同机位资源
  关闭对照 | 水下仰望 Snell 窗），双会话逐位一致 + 焦散开 / 关海床条带
  差分 2498 px + 朝上视线同点像素必须被改写。目检结论：青绿水体、网状
  焦散亮筋落在海床上并随距离衰减、从下方可见波面与透光的物体轮廓。
- 基线 debug 1776 → 1792 / release 1773 → 1789（本轮 +16，AGENTS.md 门禁节同步）。


### Added（R93 海洋 v2 + 浮力：CPU 频谱位移图 × 相机跟随无限海面 × BuoyancyPlane）

路线图 v5 第三梯队收官役上半（R94 水下 + 焦散另立）。设计三点裁定
（2026-09-20，用户全按推荐）：**拆两轮**（R93 海面 + 浮力 / R94 水下，
一轮六件事会让水面·水下·浮力三类 GPU 套件的风险相乘，而每轮全量 release
编译是固定成本）/ **预烘关键帧 + GPU 重心式插值**（裁定 2「放弃 GPU FFT」
的具体形态）/ **浮力落 iris-core 资源 + iris-engine 同步系统**（iris-core
零渲染依赖读不到 `WaterSurface`，跨界搬运的唯一合法落点在同时依赖两者的
编排层）。兼容铁律：`OceanSurface` / `BuoyancyPlane` 缺席 = 逐位不变
（R81 池塘四套像素锁、R92 天气六套、六游戏轨迹 FNV 全部原值通过）。

- **CPU 频谱位移图**（新模块 `iris-render/src/ocean.rs`）：整数波数
  **圆形格点**（`|k| = 2π(i,j)/L`，`i²+j² ≤ K²`，K=8 ⇒ 197 平面）上的有限
  余弦和，振幅谱 `(k₀/k)^slope`、相位 = splitmix64 式无状态整数哈希（禁
  rand、禁全局态）；角频率 `ω = √(g|k|)` **量化到 2π/T 的整数倍**（T=6 s）
  ⇒ 波形场以 T 严格周期回绕，16 张关键帧的「第 M 帧」在层号取模下**就是
  第 0 帧**（无缝是索引性质，不依赖浮点周期性）。图 = 16 层 × 64² 纹素 ×
  `Rgba16Float`，通道 `(高度, ∂h/∂x, ∂h/∂z, 0)`——**梯度解析求和**而非有限
  差分（R81 法线用解析偏导的同一哲学，且省掉每片元 4 次邻域采样）。
  烘图是纯函数（同输入逐位一致 + 峰高 ≤ Σ振幅 + 整周期均值近零三条锚），
  加载期一次完成、换窗口尺寸不重烘、运行期零 CPU 成本。
- **双层相机跟随网格**（`ocean_rings`）：近场 ±24 m（步距 0.5 m，吃细节
  几何位移 + 法线，按半径 smoothstep 淡出到 0）与远场 ±320 m（步距 6.7 m，
  只有 Gerstner 涌浪）。锚点量化 `round(eye/step)·step` ⇒ 相机亚步移动
  **不改任何顶点位型**（海面不随相机抖）。淡出终点恰为远场起点且远场几何
  位移恒 0 ⇒ **两环交界无竖直台阶**；网格跟随相机天然闭合 §8.50⑧
  「边缘露缝」（边缘恒在视野外，构图上再让海雾遮住接缝）。
- **海洋渲染路径**（`water.wgsl` 拆出 `water_common.wgsl` + 新
  `water_ocean.wgsl`）：**池塘的 `vs_main`/`fs_main` 与
  `water_pipeline`/`water_layout` 一字未动**，海洋走同 shader 文件族的新
  entry point（`vs_ocean`/`fs_ocean`）+ 独立 `water_ocean_layout`（组 0
  七槽位：params / 反射图 / 位移图数组 / scene_depth / Clamp 采样器 /
  Repeat 采样器）+ 独立管线，链位与池塘同构（scene_depth 之后、TAA 之前，
  HDR MSAA Load + resolve、深度只测不写）。两环 = 两次 `draw_indexed`
  （共用同一份归一化网格缓冲与同一张反射捕获，**折射图整趟跳过**——无限海
  无池底可折射，省一次全场景绘制）。与 `WaterSurface` 互斥：海洋在场则
  池塘 `warn` 一次并整段不选 ⇒ R81 路径零执行、零改动。
- **浮力**（`iris-core/src/buoyancy.rs`）：`BuoyancyPlane` 资源（opt-in，
  同 R76 `ContactSolverV2` 的 `try_resource().map(*r)` + `if let Some` 整段
  分叉范式）+ 逐形状**浸没体积解析**——球走球冠闭式 `πh²(3r−h)/3`、
  AABB 走一次线性档、**OrientedBox 走三次 box spline 闭式**（论据：盒内点的
  世界 y 是局部坐标的仿射函数 ⇒ 带符号深度 `d(v) = α + Σβᵢv`，浸没比例 =
  截断幂 `(x)_+³` 的错差；负系数轴用 `vᵢ ← 1−vᵢ` 翻正、水平轴按维数递降
  走二次 / 一次同族式，避免 1/0）、胶囊取竖直包络线性比例（近似，声明在案）。
  施力在 `step_physics` 半隐式积分段、重力行紧邻：`v.y += ρ_f·V_sub·g/m·dt`
  + 按浸没比例的**第二段**指数水阻（不合并进既有 damping 乘子——无浸没
  路径必须一行新算术都不进，R92 select 门 × R63 FMA 两条教训的联合兑现）。
  v1 拒绝清单：无扶正力矩（船不回正）、无附加质量、无二次拖拽、无浪高耦合。
- **跨界同步与序列化补账**（`iris-engine`）：`sync_buoyancy_water_surface`
  独占系统（注册在 `step_physics` 之前）仅当资源在场且
  `follow_water == true` 时把水面高度灌进资源（海洋优先于池塘，与渲染器
  互斥同序；默认 `false` ⇒ 手设高度永不被改写）；`step_physics` 访问集补
  `.read::<BuoyancyPlane>()`。场景 JSON 实体段新增可选 `water` / `ocean`、
  环境段新增可选 `buoyancy`（**R81 起水体从未入档**的补账）；serde 缺省 =
  不挂 = 逐位同今日，旧档重存不凭空多出三段。
- **与规格的偏离（六条，均有理由）**：① 位移图 **mip 链改为「远场完全不吃
  谱细节」**——`texture_2d_array` 各层尺寸必须一致，关键帧与 mip 无法共用
  一个数组；带限双图需两套格点 / 周期常量（影子契约面翻倍），而 4 m 网格
  采 0.25 m 图本就必然混叠 ⇒ 远场只留涌浪（§8.72 头条限制）；② WGSL 由
  「同一 module 加 entry point」改为 **Rust 侧拼接 `water_common.wgsl`**
  （同一 module 内两路 `@group(0)` 声明按 binding 号冲突，池塘的
  `WaterParams` 与海洋的 `OceanParams` 无法共存）——拼接让 Gerstner 求和
  单源、按构造不漂移，替代「双份镜像 + 源码看门」范式，代价是水体 WGSL
  文件不再能脱离 Rust 侧单独用 naga CLI 校验；③ **谱斜率默认 1.5 而非
  Phillips 的 2.0**（首版验收图目检：细节开 / 关两面板近处亮度差 < 1/255，
  判「细节层不可辨」——Phillips 档的斜率谱 `∝ k^{-2}` 把法线扰动全交给
  最长波；1.5 档 `∝ k^{-1}` 对数铺开，2–16 m 短波终于可见，`spectral_slope`
  成公开旋钮并带非默认值断言）；④ **海洋不接 R92 雨打水面涟漪**（第三份
  `weather_ripple_gradient` 镜像不值，且 `OceanParams` 不留无读取的
  `weather` 通道——R89「SSR 强度死旋钮 26 轮」教训）；⑤ 胶囊浸没从定稿的
  「AABB 包络 + warn 一次」改为**竖直包络线性比例**（同精度阶、无日志噪声）；
  ⑥ 水阻从定稿的「合并进同一指数因子」改为**分支内第二段指数**（理由见上）。
- **测试**（CPU 21 + GPU 6 + 引擎侧 2）：iris-core `buoyancy` 14（球冠三
  锚点 / 平放盒线性闭式 / **OBB 闭式 vs 16³ 中点积分收敛** / OBB 恒等姿态
  ≡ Box / **90° 三轴互不相同的取轴硬锚**（专治旋转矩阵列 vs 行的静默转置）/
  过中心平面任意姿态精确对半分 / 端点 0 与 1 逐位 / 退化形状零体积 /
  中性浮力体积判据 / `to_bits` 双跑）+ `physics::buoyancy_tests` 7（净
  加速度按密度比解析对照 / 中性体 2 秒不动 / 半密盒收敛到一半吃水 /
  **资源缺席与零浸没轨迹逐位 ≡ legacy** + 水位落在轨迹上时必须改变位型的
  判别力旁证 / 无碰撞体飞行体不参与 / 双跑逐位 / **浮力不改旋转**（无扶正
  力矩的位型断言））；iris-render `ocean` 14（平面表确定性 + Nyquist 界 /
  RMS 归一化与带限能量单调 / ω 量化到 2π/T 格点 + 误差有界 / 时间 Nyquist
  余量上下界 / 烘图逐位 + 末帧回绕 0 + 通道布局锚 / f16 打包同量 /
  重心式端点逐位 + 中点无过差 / 帧对全循环遍历 + 负时间 / 环步距与细节
  权重分配 + 亚步吸收 + 越步换格 / 淡出单调 / 峰高与均值界 / 组件缺省 /
  **谱斜率搬能量到短波且不改 RMS** / WGSL 影子契约看门）；
  `tests/ocean_render.rs` 6（GPU_SERIAL 串行：在场改全帧哈希 + 覆盖面积 /
  无海洋帧可复现 / **海与池塘同挂 ≡ 只有海洋**（互斥不是「画了再盖」）/
  detail_amplitude 活旋钮 + 近场条带差分 / **far_extent 192→40 让 100 m
  采样点从海翻成天空 + 环带差分**（低机位掠射下「比天空暗」的颜色启发式
  被物理否定——Fresnel 趋 1 时海就是天空的镜子，改用几何落点判别）/
  双会话逐帧逐位 + 时间推进改哈希）；iris-engine 2（三段序列化往返 +
  退化参数加载期拒绝 + 旧档零变化 / 同步系统六档 opt-in）。
- **验收图** `docs/ocean.png` 三面板（谱细节海面 | 细节关闭对照 | 浮力平衡
  吃水），480×320 三格 + 薄海雾，480 步物理推进后按**阿基米德判据**
  `V_sub/V == ρ_b/ρ_f` 逐体断言吃水（0.4ρ / 0.9ρ / 中性球三条收敛到
  ±0.08，1.6ρ 箱沉底），三面板各自双会话逐位一致。目检结论：近处可见
  2–16 m 谱波纹与暖色太阳碎闪、远场涌浪带 + 雾遮接缝、浮体吃水可辨。

### Added（R92 降水：雨雪发射器 × 湿表面 × 程序化涟漪 × 积雪坡度遮罩）

路线图 v5 第三梯队天气 1/2。设计四点裁定（2026-09-19，主会话按推荐
定稿）：**扩展 v2 发射器**（体积出生 / 任意发射轴向 / 线性阻尼三旋钮
——不建独立降水系统，复用 R82 全部 GPU 池机械）/ **全局资源 + 程序化
涟漪**（`Weather` 资源 + fs_shaded uniform 门——涟漪 = 哈希网格环形波，
水面与湿地表两处生效）/ **全局量 + 坡度遮罩**（积雪无时间累积——量
由游戏侧设定，Weather 是状态不是系统）/ **v1 无溅射**（雨滴穿地、寿命
按坠落时间裁断——溅射需场景高度查询，诚实限制入 §8.71）。兼容铁律：
`Weather` 资源缺席 / 全零 = 天气分支整段死码（GPU 测试 FNV 锁定）；
v2 三旋钮缺席（零体积 / +Y / 无阻尼）= select 门保证 R82 行为逐位不变
（particle_v2 / water / shadow_v2 / normal / transparent / sss 全部
像素锁原值通过）。

- **雨雪发射器**（`particles_v2.rs`）：`ParticleEmitterV2` 新增
  `spawn_extent`（盒形体积半边长——全零 = 点发射逐位不变）/ `spawn_axis`
  （默认 +Y = 原行为；雨雪 = −Y）/ `linear_damping`（速度每步
  /(1 + k·dt)——指数衰减的精确离散；终端速度 = g/k：雨 k=1 ≈ 9.8 m/s、
  雪 k=6.5 ≈ 1.5 m/s 物理量级）。`rain()` / `snow()` 预设（体积
  20×12×20 m、出生速率按「稳态存活 ≤ 4096 池容量」留余量；雨 = 拖尾
  3 点运动方向雨丝 + alpha 暗剪影、雪 = trail 1 软边 billboard）。
  **挂相机实体即得跟随相机的降水体积**（发射原点 = 实体全局位置，
  游戏侧零引擎代码）。`SimParamsRaw` 96 → 128 B（新二字段，
  WGSL SimParams 同步 + const 断言看门）。模拟着色器三门全部 select
  形态：体积偏移 `select(origin, origin+vol, vol≠0)`（`0·x = ±0` 的
  符号歧义被 select 吸收）、轴向 uniform 化（axis = +Y 时同值同算术）、
  阻尼除法不进入既有算术的收缩决策（R63 教训的公式层变体）。出生
  随机字 r2/r3/r4 = mix64(counter + seed + k)——与首两路输入互异，
  CPU 镜像 `v2_spawn_position` 逐位锁（extent 全零 → 逐位还原原点）。
- **湿表面**（`weather.rs::wet_surface` → standard.wgsl 天气分支）：
  粗糙度向 0.08 压低（镜面聚拢）+ 反照率按 (1−metallic) 乘 0.7（金属
  不因水变暗；f0 用干 albedo——金属湿感 v1 不建模）。混合全部**重心
  形式展开** `x·(1−a) + y·a`（不写 WGSL `mix`——其实现形式非规范保证，
  且线性插值 `x + (y−x)·a` 在 a=1 处实测 2 ULP 舍入残差；重心式两端点
  逐位精确，CPU↔WGSL 同式镜像）。
- **积雪坡度遮罩**（`snow_mask` / `snow_surface`）：世界**几何**法线
  y 的 smoothstep(0.45, 0.8) 门（用 n_geo——着色法线含贴图高频，做
  遮罩会出噪点，与阴影偏移同哲学）× 全局量；遮罩内反照率向雪白
  (0.87, 0.89, 0.94)、粗糙度向 0.6 混合。GPU 测试：朝上地面显著变亮、
  垂直墙面增量 < 地面增量的 25%（实测 ~7%）。
- **程序化涟漪**（`ripple_gradient`）：世界 XZ 哈希网格（格宽 0.4 m），
  每格一个周期性落点（中心 = 格内 [0.15, 0.85] 带哈希偏移、相位 = 哈希
  × 1.3 s 周期），环形波 `A·decay·sin(π·u)` 以 0.85 m/s 扩展、0.6 s
  衰减殆尽（A = 0.01 m——梯度上界 ~0.35/单环，远中景可判别的视觉
  标定）；3×3 邻域固定序求和（dz 外 dx 内——确定性求和序）。**无状态
  纯函数、同输入逐位确定**；整数格哈希跨平台精确（f32→i32→u32 的
  位重解释路径——直接 f32→u32 对负格坐标不是位重解释，不可用）。
  消费点两处：standard.wgsl（湿的朝上面 n_geo.y > 0.35，强度 × 湿度
  扰动着色法线——涟漪只该出现在湿表面上）与 water.wgsl（雨打水面，
  Fresnel / 反射扰动 / 太阳镜面全吃到；时间源 = wave_b.z 与波形同钟
  ——雨打水面与湿地表涟漪同相）。WGSL 双份镜像（无 include，同
  water_single_wave 纪律）+ 源码看门。
- **接线**：binding 21（16 B uniform，「不扩 Globals 前缀」扩容先例
  延续；**uniform 而非 storage**——片元只读 storage 计数维持 4 上限）。
  主相机 / 探针捕获 / 水面反射捕获 / RT **共享同一只缓冲**（天气是
  世界状态不是屏幕空间状态——反射捕获里的湿地也该湿），每帧 draw
  首部覆写一次。`WaterParamsRaw` 240 → 256 B（天气通道；water.wgsl
  同步 + const 断言）。meshlet / 蒙皮 / 发带路径共享 standard fs_main
  本体 ⇒ 天气自动覆盖，零额外改动。
- **测试**（CPU weather 8 + particles_v2 新 5；GPU weather_render 6）：
  缺席 / 全零逐位同构；湿 0/0.5/1 单调暗化（非默认中点断言——R89
  教训）+ 俯视机位（掠射角 Fresnel 天空镜面会抵消变暗【暗漫反射 +
  亮镜面的物理平衡】，俯视让主导项可判）；积雪坡度遮罩墙 / 地增量比；
  雨雪渲染 + 双会话逐位；水面涟漪 wave_speed=0 隔离判别（唯一时变源
  + 门关逐位不变）。CPU 锚点：湿 / 雪端点逐位精确（重心式）、阻尼
  解析收敛终端速度 g/k、体积门逐位还原、涟漪有界 / 确定 / 间歇覆盖、
  格哈希散布；WGSL 三处源码看门（天气绑定与公式 / 水面涟漪拷贝 /
  sim 三 select 门）。
- **场景序列化**：`ParticleV2Section` 补三新字段（`spawn_extent` /
  `spawn_axis` / `linear_damping`，serde 缺省 = 点发射 / +Y / 无阻尼
  ——旧档零语义变化）；雨雪发射器完整入档往返（位级一致断言扩展）。
- **验收图** `docs/precipitation.png` 三面板（雨+湿地+水面 | 积雪+
  降雪 | 干燥对照），960×540 双会话逐位一致 + FNV 差分旁证。目检
  结论：湿暗 / 雪白 / 雨丝 / 雪花 / 坡度遮罩（柱侧保持原色）全部
  可判，无伪影。
- 基线 debug 1715 → 1733 / release 1712 → 1730（本轮 +18，AGENTS.md 门禁节同步）。

### Added（R91 毛发 v2：风场 Verlet 摆动 × 远景卡片 LOD × alpha 感知毛发阴影）

路线图 v5 第三梯队尾巴 2/2（毛发 2/2 收官）。设计四点裁定（2026-09-19，
主会话按推荐定稿）：**CPU Verlet**（确定性——`strand_step` 纯函数 +
同输入序列双跑逐位单测；`wind_sample` 逐束根位置单点采样，粒子 v2
同款口径；GPU compute 与 gust 哈希链 WGSL 复刻重申拒绝）/ **阴影全接
+ alpha 感知**（壳层 + 发带进方向光 CSM，FS 与主 pass 同一判定语义
——§8.69③ 实心影近似闭合）/ **交叉卡片 LOD**（`generate_hair_cards`
与发带同一随机字契约——LOD 切换发束位置不跳变；滞回 20%）/
**蒙皮继续声明不做**（§8.69① 维持——无 in-repo 消费者）。兼容铁律：
全部 opt-in，`HairDynamics` / `HairLodSettings` / `card` 字段缺席 =
R90 行为逐位不变（shadow_v2 / normal 像素锁原值通过）。

- **风场摆动**（`fur.rs::update_hair_dynamics`，`ParticlePlugin` 注册
  同层）：半隐式 Verlet 积分 → 根部钉扎（rest[0] 随实体变换——发根
  跟随）→ 刚度回弹（`stiffness × dt` 钳 1）→ 从根到梢单遍距离约束。
  世界空间模拟、局部空间写回（`inverse(model)`）——实体移动时发梢
  甩动惯性自然；逐束去相关（mix64 种子，±33% 调制——束间不齐步）。
  `HairStrandMesh::points_per_strand` 显式声明控制点布局（**顶点数
  对「束 × 段 × 4」的分解有歧义，不猜**——自定义网格未声明 ⇒ warn
  跳过）；写回重算中心差分切线（KK 主方向与展宽同步）。
- **渲染接线**：动态实体每帧 `write_buffer` 重传顶点（morph 范式，
  建缓冲统一带 COPY_DST）；静态实体 VBO 缓存零触碰（R90 行为）。
- **alpha 感知毛发阴影**（`shadow_hair_vs` / `shadow_shell_vs` +
  shadow.wgsl `fs_fur`）：发带 VS 光空间屏幕展宽（主 pass 同式换
  CascadeParams——正交光空间 `clip.w ≈ 1`，投影常量列长推导同法）；
  壳层 VS 层比膨胀同式；FS = `base.a < max(材质 cutoff, 层比)`
  discard（与 fs_shaded 同一判定）——毛形阴影 + Mask 材质阴影孔洞
  一并闭合。布局 = [cascade, material]（组 1 复用主 pass 材质 bind
  group）；点光 cubemap 阴影不接（v1 边界 §8.70）。影子契约看门：
  `ShadowFurVertexOutput` 三方（shadow.wgsl + 两 VS）源码层拦截。
- **远景卡片 LOD**（`groom.rs::generate_hair_cards` +
  `HairLodSettings` 资源）：每束两条 60° 交叉卡片（Rodrigues 绕发丝
  旋转），标准顶点格式走标准不透明管线（KK 材质切线通道照常）；
  收集期距离裁决（实体原点到 eye，LOD v2 同口径）+ 滞回（远→近
  0.8× 回切，`hair_lod_memory` 跨帧记忆同款范式）——远档项 merge
  进普通不透明实例参与排序合批。卡片与发带**同一随机字消费序** ⇒
  切换时发束位置逐位对应。
- **测试**（CPU +8 / GPU +3，全部新增）：Verlet 稳态锚（恒风偏移
  收敛 + 根部钉扎）/ 段长守恒（任意风向 50 步 1e-5）/ 确定性双跑 +
  满刚度回弹 / 写回往返 + 切线归一 + 邻束隔离；GPU：恒风两帧画面
  差分 + 无组件双帧逐位（缺席静态锚）/ 毛发阴影差分（壳层毛投影
  改变画面 > 200 像素）/ LOD 档位切换可判别（近档发带 1 / 远档卡片
  0 + instances 计入）。
- **排障记录**：验收图首版「全画布雪崩斜纹」归因为 **PPM 合成行
  字节数错位**（三面板 + 每面板后缀 GAP ⇒ 每行多 18 字节，下一行
  数据错位延续到帧尾）——非 GPU 病灶（毛发阴影开关两轮 FNV 完全
  一致为旁证）；修复合成循环（panel GAP panel GAP panel）。
- 基线 debug 1707 → 1715 / release 1704 → 1712。六项门禁全绿。
  验收图 `docs/fur.png` 升级三面板（左：壳层毛 + 地面毛影；中：
  240 束静止发帘；右：恒风吹 1.5 s——发梢 +X 偏摆、根部钉扎）。
  诚实限制 §8.70。

### Added（R90 毛发 v1：双路线——壳层 shell fur × Kajiya-Kay 发带 + AlphaMask cutout）

路线图 v5 第三梯队尾巴首役（毛发 1/2）。设计四点裁定（2026-09-19，
主会话按推荐定稿）：**相机面向展宽带**（路线图原文「相机面向条带」
的忠实落地，粒子 v2 ribbon 同范式）/ **fs_shaded 材质 flag 分支**
（单一事实源，光源循环零复制）/ **实例化壳层**（instance_index 布局
同族、层比进实例数据）/ **本轮补 AlphaMask**（glTF MASK 规范缺口
顺手闭合）。兼容硬约束：毛发组件与 KK/cutout 材质参数全部缺席 =
逐位不变——既有全部 FNV 基线零改动原值通过（normal / transparent /
shadow_v2 / sss 等像素锁实测，R63 式 ±1 LSB 编译器 FMA 漂移未出现
——KK 走独立贡献函数 `kk_light` + uniform 门，`pbr_light` /
`point_light_contrib` 本体零触碰的兑现）。

- **程序化 groom 生成器**（`groom.rs`）：`generate_hair_strands`
  纯函数——头皮圆盘面积均匀采样 + −Y 下垂 + 逐束随机方位正弦弯曲
  （Scheuermann 简化），随机性全部来自 mix64 无状态哈希流
  （`particles.rs` 同款），**逐束随机字消费序即契约**（同参数同种子
  ⇒ 同网格逐字节，单测锁定）。发带控制网格 = 逐控制点「位置 + 中心
  差分切线 + 线性宽度 + 沿丝参数」，相邻段共享控制点顶点 ⇒ hair_vs
  展宽后关节无缝（发带是 cutout 不是软边 alpha——无缝必须由数据共享
  保证，单测钉死）。
- **发带管线**（`hair_vs.wgsl` + standard.wgsl `fs_main` 复用）：
  相机面向屏幕空间展宽——投影常量从 **view_proj 列长推导**
  （`length(view_proj[1].xyz)` = 1/tan(fov/2)，宽高比同法——零
  Globals 扩容、零新 uniform，skybox 并行布局陷阱零触碰）；伪法线
  （视线在垂直发丝平面上的投影）逐顶点算好走 world_normal 通道 ⇒
  fs_shaded 的 n 零改动；cull None（屏幕展宽绕向随相机变化）+
  REPLACE + 深度写（cutout 语义）。
- **Kajiya-Kay 着色**（`kajiya_kay.rs` CPU 镜像 + WGSL `kk_light` /
  `kk_point_contrib`）：漫反射 = max(0, sin(T·L)) KK 原式；高光 =
  sin(T'·H)^exponent，T' = normalize(T + N·shift·(2t−1)) 根梢移位；
  高光颜色 = mix(白, albedo, 0.5) 固定式（无旋钮不设——R89 铁律）；
  sin 一律 sqrt(1−cos²)（libm asin/acos 无位级保证）。间接项（IBL /
  探针 / 光照贴图）维持各向同性伪法线路径（v1 边界）。CPU 镜像
  `kk_lobe` 解析锚单测（垂直峰 / 平行零 / 移位峰值角差 ≈ 4·shift /
  exponent 半宽单调）+ WGSL 源码层看门（改公式漏同步即挂）。
- **壳层管线**（`shell_vs.wgsl` + fs_main 复用）：单网格 N 实例一次
  draw，层比 t = k/N 进实例数据（`ShellInstanceRaw` = InstanceRaw +
  loc15，144 B），VS 沿法线膨胀 t × height、fur_aux varying 传层比；
  FS 统一 cutout 判定 `base.a < max(材质 cutoff, fur_aux.x)`——
  密度贴图即毛长图（值 d 处毛长 = d × height，毛尖渐隐）。底皮照常
  走普通不透明路径（壳层实体 = Mesh+Material 实体挂 `ShellFur` 组件，
  缺席 = 只画底皮 = 现状）。
- **AlphaMask**（glTF MASK 语义闭合）：`AlphaMode::Mask` +
  `MaterialDef::alpha_cutoff`（缺省 0.5 = 规范默认）——不透明分区
  （深度写 / 批处理 / 剔除语义与 Opaque 全同），fs_shaded 开头逐像素
  discard；glTF `alphaMode:"MASK"` + `alphaCutoff` 接线；场景 JSON
  `alpha_mode:"Mask"` + `alpha_cutoff`（缺省省略 ⇒ 旧档往返字节级
  不变）；热重载参数补丁纳入两字段。**阴影为实心近似**（深度 pass
  不感知 alpha），alpha 感知阴影与毛发阴影一起留 R91。
- **VertexOutput 影子契约扩展**：`fur_aux`（@location(6)）在四个 VS
  module 并行声明（standard / meshlet_draw / hair_vs / shell_vs），
  看门测试 `vertex_output_fur_aux_matches_standard_layout` 源码层
  拦截（skybox Globals 前缀先例；既有路径恒写 0 ⇒ fs_shaded 只在
  > 0 时进壳层裁剪语义）。
- **测试**（CPU 17 + GPU 5）：groom 确定性双跑逐位 / 数量公式 / 几何
  解析锚（y 单调、宽度线性、根部贴头皮）/ 关节共享 / 切线归一；KK
  镜像五件套；壳层层比公式 / 展开保真；材质 Mask/KK 缺省与 builder
  clamp；GPU（`fur_render.rs`，GPU_SERIAL 串行）：缺席逐位 + 双会话 /
  发带可见性（高密度帘覆盖中心）/ 壳层毛高单调 + **毛高 0 零点锚**
  （与不挂壳层整帧逐位）/ Mask 三档阈值（纯垫板参考渲染法）/ 复合
  场景双会话逐位。验收图 `docs/fur.png`（左壳层 16 层绿毛 + 8 层蓝
  毛 + 无毛红对照 / 右 KK 发帘 160 束真实着色）；快照
  `examples/fur_snapshot.rs` 双面板双会话逐位断言。
- **排障记录（本仓铁律兑现）**：`MeshStore::add` 从 0 起（与
  MaterialStore 的 0 号预留不同）——首版用 mesh 句柄 0 当发带哨兵，
  壳层拿到句柄 0 的网格被误判成发带项（GPU panic + 逐项打印定位）；
  改显式 `FurKind` 判别。教训入档：**跨 store 的句柄值语义不一致，
  判别位不得借用句柄值**。

### Fixed（iris-exp：中距离朦胧——视觉地面 24 m → 240 m，天空盒地平线以下渐变不再冒充雾）

用户报 iris-exp 中距离画面「略微朦胧」并附截图。逐像素归因（左缘条带
垂直扫描）：**不是雾**——`FogSettings` 未注册（缺席 = 关），地面远缘
在截图 y≈400–418 处是 20 px 内 (207,211,217)→(157,177,201) 的**硬边**；
朦胧感来自天空盒：程序化渐变天空在地平线处最亮（(217,223,229)，设计
上刻意压在泛光阈值下），且地平线**以下**还有一段缓变白蓝渐变（像素
实测占画高 20%）。病灶是**视觉地面只有 ±12 m**：远缘到真实地平线
（12–19 m，恰为「中距离」）露出的是这段天空渐变，读作雾。

- 修复（纯游戏层，引擎零改动）：`plugin.rs` 新常量
  `GROUND_VISUAL_HALF = 120.0`，视觉地面网格 24 m → 240 m；玩法活动
  范围 `GROUND_HALF = 11.0` 钳制不变，两者解耦。默认机位下地-天边缘
  升到距眼角 ≈1.3°（DIST_MAX 拉远也在画外），白纱压缩成一条细线。
- 验证：640×360 离屏快照同款扫描——修复前地平线以下 20% 画高的白纱
  带；修复后天空→地面在 y≈115 处硬边直切（(211,217,224)→
  (141,159,183)），地面到画面底部均匀无距离渐变。验收图
  `docs/exp.png` 重生成（FrameStats 断言原值通过）。

### Added（C7b 闭合战役 · 大裁决役下半——Weighted Blended OIT：逐像素顺序无关透明）

路线图 v5 清账战役批次八下半（池 #5）：§8.3 半透明排序条目按逐像素粒度
闭合。设计稿四点裁定（2026-09-19）：**A1 = WBOIT（McGuire & Bavoil
HPG 2013，公式取证照抄、禁凭记忆）**；**A2/A3 = 仅主透明 pass**（水体 /
粒子 / 贴花 / RT 相机维持 painter，声明边界）。开关资源
`OrderedIndependentTransparency` 缺席 = painter 原路逐位不变；与 C7a 同
口径「全部既有基线零改动原值通过」（普查实锤硬 FNV 场景均无 Blend 材质）。

- **两遍 A-buffer**：gather 任意序累进 accum RGBA16F（`(ONE,ONE)`）+
  reveal R16F（`(ZERO,ONE_MINUS_SRC_COLOR)` 固定功能连乘 Π(1−α)）；
  resolve 全屏三角 `out = avg·(1−reveal) + bg·reveal` 写回 HDR MSAA 并
  解析回 resolved_view（下游不变式原样）。自适应权重在窗口深度域恒贴
  scale 上限（前优先保留、深度带不作用——如实入档 + CPU 镜像钉住）。
- **单一事实源**：`fs_main` 整段提取 `fs_shaded` 共享体，painter 与
  gather 入口皆薄壳（R78 先例）；权重/合成 CPU 镜像 `oit_weight` /
  `oit_resolve_color`（单层 ≡ 经典混合兼容锚 + 无覆盖 = bg 逐位恒等
  护栏锚）。
- **深度真值**：gather 无深度附件，片元对 scene_depth 副本手动线性测深
  （ε=2mm）；副本 pass 提前编码到透明 pass 之前（内容逐位不变——SSR
  读上帧语义保持，DOF/MB/TAA/雾/水体既有 GPU 锁原值 = 移动零外溢的
  整链背书）；`oit_on` 时无条件跑（空几何帧 Clear 远平面哨兵）。
- **hazard 绕行**：resolve 读写 resolved_view 冲突 ⇒ 帧内一次
  `copy_texture_to_texture` 到 bg_copy，三路 1:1 全 `textureLoad`。
- 测试学修正记录：首版主测用「对称场景左右镜像相等」——**理论错误**
  （painter 全局单一先后本就对称）；改「同几何交换 spawn 序：painter
  两帧必不同、OIT 整帧逐位一致」——顺序无关性的精确判别，不拿近似
  值冒充解析参考。
- 诚实边界 §8.68 八条（近似算法 / 覆盖范围 / MSAA 子采样退化 / bg 拷贝
  成本 / 手动测深口径 / 副本前移 / 无旋钮无序列化 / 跨适配器不保）。
- 测试 +9（`oit.rs` CPU 6 + `transparent_render.rs` GPU 3：顺序无关主测 /
  缺席逐位 + 无覆盖护栏 + 单层锚 / 双会话逐位）；验收图
  `docs/oit.png`（左 painter 实体序主导、右 OIT 加权平均）；
  快照 `examples/oit_snapshot.rs` 双断言（画面不同 + 各自逐位稳）。
  基线 debug 1676 → 1685 / release 1673 → 1682（门禁对账后确认）。
  **C7 就此收官 = 闭合战役 8/8 全部完成。**

### Added（C7a 闭合战役 · 大裁决役上半——DDGI v3：同源天空 × 动态光源进 GI）

路线图 v5 清账战役批次八上半（池 #21）：§8.57①② 两条自认候选翻转。
用户四点裁定（2026-09-19，设计稿 `docs/private/c7-design-v1.md`）全部
按推荐定稿：**B1 甲**（程序化渐变 + HDR 全景逐向镜像，R79 大气维持
挂账）；**B2 甲**（点/聚光进 GI、逐灯硬影单射线 + `point_light_shadows`
逃生门）；验收口径升级为「**全部既有基线零改动原值通过**」（普查实锤
DDGI 现口径不吃灯/方向天空 ⇒ 零重锁，`R89_COV1_ON` 定值锁原值通过）。

- **重照明输入扩展**（单一事实源纪律）：`RelightInputs { lights, sky }`
  平行加入，`probe_ray_radiance_depth` / `install` / `tick` /
  `probe_relight` 全部保留为**缺席档薄壳**（`_with` 档的空调用），
  旁路分支逐位 = R89 口径（`to_bits` 锚测试钉住）；烘焙 / R83 探针
  路径零感知。`relight_fingerprint` 扩为含灯集 bits + 天空快照
  （generation 入指纹）的合并指纹，任一变动 ⇒ 全表标脏不清值。
- **天空同源**（`sky_from_source`，默认 false）：未命中射线按方向求值——
  程序化渐变 CPU 镜像（`procedural_sky_radiance`，WGSL↔Rust 常数表
  **源码双看门**）+ HDR 全景 nearest-texel（`SkySourceKind::EquirectHdr`
  随 Arc 化 ⇒ 逐帧快照零拷贝）。拒绝冒充三态：atmosphere 场景不吃
  （warn 一次）、作者 `sky_color = Some` 覆盖恒胜、命中点反弹底仍均值
  常数（§8.67①）。
- **灯进 GI**（`lights_in_gi` 默认 false + `point_light_shadows` 默认 true）：
  `ddgi_gi_lights` 逐字段镜像主渲染灯表组装（`color×intensity`、
  `inner.min(outer)` 钳、零方向不发光、点先聚光后、容量 8 盏）；命中点
  入射追加逐灯 `radiance×atten×ndotl`（衰减窗/锥因子逐式镜像
  `point_light_contrib`，`spot_cone_factor` 共享 CPU 函数）+ 硬影单射线
  （墙后火把严格零进表；1e-3 m 病态贴灯档豁免，§8.67④）。
- **序列化**：`level.ddgi` 三 `Option` 新字段（回写只落非默认值 ⇒
  旧档形态不变）；往返对称由既有保存测试顺带钉住。
- 诚实边界 §8.67 八条（逐向化只及未命中射线 / nearest vs 线性 /
  8 盏与主渲染选取可不同 / 病态贴灯豁免 / 全硬边 / 成本上界随灯数 /
  锥测无分箱 / 覆盖优先级）。
- 测试 +14（iris-render ddgi 9：逐位锚/双看门/解析向/映射解析/窗口
  解析值/遮板逐位零/锥门控逐位等/指纹行为/滚动全链；引擎 4：收集
  口径/快照优先级链/火把端到端+空集锚/天空方向性双向证据；GPU 1：
  火把 ROI 抬亮+稳态逐位+天空可判别）；基线 debug 1662 → 1676。

### Added（C6 闭合战役 · 音频役——空间化 v3：多普勒 × 遮挡 × 逐声音低通）

路线图 v5 清账战役批次七（池 #13）：§8.11 的空间化半边清账。用户三点
裁定（2026-09-19）：**HRTF 本轮裁出回池**（「正前正后不可区分」声明维持
原样）；遮挡 = **二值**（射线命中即满强度，无衍射/厚度）；多普勒 =
**双向径向 + 钳 [⅓,3] + 常速 343 可配**（静止因子精确 1.0 = 逐位锚）。
三项全 opt-in「缺席 = 与 v1/v2 逐位一致」，混音核心纯函数分界不变：

- **`doppler_factor`（新纯函数）**：`f = (c+v_o)/(c−v_s)` 双向径向合成
  （声学时不对称如实保留），钳界与病态守卫（音速/NaN/零距 ⇒ 有界或
  恒等），声速全局 `set_speed_of_sound`；因子乘进重采样播放步长（趋近
  = 步长快 = 音高升）。`Voice.velocity` + `ListenerParams.velocity` +
  `set_sound_velocity` / `set_listener_velocity` 直通口。
- **遮挡闷声**：`set_sound_muffle(id, 0..=1)` + 一阶低通
  （`muffle_coef`，m=1 ⇒ −3dB ≈ 810 Hz）；滤波状态逐声道持久在槽位
  （实时路径零分配）；`muffle=0` 整体旁路（不读不写状态，「显式 0 /
  从不设 / 闷后放开」三型逐位锁）。`SourceFrames::add_frame` 以
  `Option` 参数接入——音乐与音效共用单一声道映射实现，音乐恒 `None`
  ⇒ R44 块拼接逐位锁原值通过。
- **引擎接线**（spatial.rs）：`SpatialSource::with_doppler()` /
  `with_occlusion(强度)` 两开关（默认关 = 系统根本不触碰混音器对应槽位，
  零额外锁）；速度 = **帧间位置差分**自动推估（含听者，游戏零改动；
  快照升格 `(bits, SoundId, position)`）；遮挡判定 = 「听者→源」线段 ×
  `effective_static` 静态碰撞体（守卫视线同一事实源，源自身实体豁免），
  惰性收集（全关零收集）。
- 顺手校准 **§8.11 三行过期错误陈述**（R44 文档债）：「无流式播放 /
  解码仅 WAV / 无总线无混响」早已不真——OGG/Vorbis 流式、音乐总线、
  音乐混响都在；按现状重写并划线标注（真限制改列为「混响只挂音乐总线 /
  解码仅 WAV+OGG / 无子混音分组树」）。
- 诚实边界 §8.66 六条（二值单射线无衍射 / 差分近似瞬移尖峰 / 开关只管
  推送不管回收 / 旁路不跟踪状态再闷一过路瞬态 / 只改播放步长非采样 /
  HRTF 裁出 + 一次性音效走直通口自推）。
- 测试 +11（iris-audio 8：静止逐位锚 / 双向解析值 / 钳界病态 / 系数
  解析 / 阶跃爬坡跨调用持久 / 旁路三型逐位 / 端到端过零率 / 句柄卫生
  旋钮；iris-engine 3：差分解析速度 / 遮挡二值 opt-in / 自碰撞体豁免），
  基线 debug 1651→1662 / release 1648→1659（门禁后对账）。
- 下一批 C7 大裁决役（OIT + DDGI 动态光源与天空同源，设计稿先行）。

### Added（C5b 闭合战役 · 格式役——场景参数热重载：材质/点光/方向光子集）

路线图 v5 清账战役批次六收官增量（池 #12）：§8.4「无热重载」的场景半边
翻转——开发期手改场景 JSON 的材质数值 / 点光 / 方向光段，半秒内就地生效，
不 reload、不重建实体、游戏状态原样保留。std-only（不引 notify 依赖）。

- **`hot_reload`（新模块）三层入口**：`build_scene_param_patch`（纯解析，
  与加载同路 serde + 版本迁移，高版本档同样拒读）→ `apply_scene_param_patch`
  （方向光资源 / 材质目录 / 点光组件就地写回 + 落账回报）→
  `SceneHotReload` 资源 + Update 系统（`DefaultPlugins` 恒注册，
  **资源缺席 = 零文件读取**，与 `TaaSettings`「缺席 = 关」范式同款）。
- **内容指纹而非 mtime**：FNV-1a 64 位哈希比对，滤掉 mtime 粒度与
  「复制未改」两类噪声——检测不依赖时钟精度，测试无同-mtime 竞态。
  坏 JSON 一次消费不重试（修好 = 新内容 = 自动再来）。
- **语义定案**：实体按 `Name` 匹配（无名段静默不入表，宁漏不错配）；
  基线轮不应用（文件只在变化时赢，游戏侧运行期改动不被覆写）；材质
  去重共享句柄 ⇒ 热改一处同步变所有引用（特性入册）；昼夜循环在场时
  为方向光真值源；结构级编辑（换图 / 换网格 / 增删实体）明确不吃——
  句柄内容不可变是 §8.4 既有边界，正是「子集」的机理原因。
- `MaterialStore::get_mut`（iris-render）：数值参数就地写入口（渲染器
  每帧从目录现读，写入下一帧即生效）；内容级句柄替换仍不提供。
- 诚实边界 §8.65 六条；§8.4 划线翻转（句柄不可换 / 结构编辑不吃 /
  wasm 无 fs 三样仍存）；新立 §4.46 全景段。
- 测试 +5（端到端参数应用与纹理句柄不动 / 缺席零触碰 / 节流与坏内容
  消费两旋钮 / 未命中收集与无名段忽略 / 去重共享旁证），全 CPU headless。
- **C5 格式役就此收官**（#11 BC7+KTX2 ✅ + #12 热重载局部 ✅）。

### Added（C5a-2 闭合战役 · 格式役——KTX2 预压缩容器 × 直传上传 × 场景接线）

路线图 v5 清账战役批次六第二增量（池 #11 三件套收口：容器 + 消费口）。
用户裁决定稿：**仅未压缩载荷 + mip 级**（supercompressionScheme = 0，
BasisLZ/Zstd/ZLIB 拒收——零新依赖取向）；三层接线 + 一处场景入口：

- **`texture_ktx2`（新模块）**：`parse_ktx2` 定长头 80 B + 级表解析
  （2D 单面、BC1/BC3/BC7 白名单 vkFormat 以 Khronos `vk.xml` 为准
  ——BC7 = 145/146，非连续算术枚举）、mipPadding 剥离、五类拒收
  （标识符/截断/白名单外/矛盾级表/不支持旋钮）；`build_ktx2` 产侧
  对偶（本引擎可读的最小容器，28 B 占位 DFD——非通用导出器，诚实
  声明进 §8.64②）。
- **目录侧表**：`TextureStore::add_ktx2`——块数据进 `compressed`
  BTreeMap（确定性遍历），`textures` 槽位只留元数据占位（宽高/srgb
  真值、rgba 空表）；`memory_bytes` / `gpu_bytes_estimate` /
  `gpu_bytes_estimate_mipped` 三口径短路决策树按容器字节照计（格式
  由容器声明即事实源，声明门不再参与）。
- **渲染器直传**：`ensure_texture` 首查侧表 ⇒ `upload_compressed` 免
  运行时编码逐级 `write_texture`（`bytes_per_row` 按级算、链止于维数
  <4 与 `mip_chain_bc` 同口径）；`texture_is_bc1/3/7` 观测口径按容器
  族置位；兜底两处：基级非 4 对齐 / 无 BC 特性设备 ⇒ 白图 + 告警
  （容器无 RGBA8 回退数据，区别于编码路径的自动回退）。
- **场景接线**：`load_texture` 按扩展名 `.ktx2` 短路入库（sRGB 语义
  随容器 vkFormat，材质槽位形参不参与）；§8.63⑥ 同步闭合为「消费方
  接线范式」（r59_snapshot 改走 `effective_compression()` 单口分发
  BC3/BC7——引擎 loader 交字节不注册是设计，非缺口）。
- 旧「拒绝清单禁 KTX2/DDS」两处声明修正（§8.35③ 与加载期编码论证段）：
  解禁的是 `.ktx2` 独立资产文件形态，GLB 内嵌 PNG data URI 不变；
  §8.17「仍无 BC7」过期修正（C5a 已补，指向 §8.63）；新立 §8.64 五条。
- 测试 +11（ktx2 解析单元 6 · 目录侧表 2 · GPU 端到端 2（含硬件解码
  保真兜底闸）· 场景 `.ktx2` 装载 1）。下一增量 C5b 热重载局部（#12）。

### Added（C5a-1 闭合战役 · 格式役——BC7 Mode 6 编码器 × manifest 声明门 × 上传决策树）

路线图 v5 清账战役批次六第一增量（池 #11 三件套之「编码决策树 + 声明门」；
KTX2 容器随后脚）。BC7 **永不自动**（用户裁决定稿：只做 Mode 6、
manifest 新增显式 `compression` 字段、旧 `bc3` 读兼容）：

- **`texture_bc7`（新模块）**：`encode_bc7` 全整数确定性编码器（逐块
  端点 = 逐通道 min/max、逐像素 16 档权重全扫并列取先、P-bit 多数票、
  anchor 端点交换归一）+ `decode_bc7_mode6`（DirectXTex `D3DX_BC7::
  Decode` 逐式移植的 CPU 镜像）+ 128-bit LSB-first 位流 reader/writer
  互逆对。**双闸验证**：CPU 编→解往返解析锁（纯色逐位、±步距界、
  共享 P-bit 的 ±1 量化面、确定性两跑）+ **GPU 硬件解码保真**（引擎
  块流经真实硬件 BC7 采样器与 RGBA8 参考帧全帧误差 <3——「自洽地错」
  位序的唯一兜底闸）。
- **声明门三口径**：`TextureStore::declare_bc7`/`is_bc7_declared`/
  `add_bc7_declared` 侧表 + 决策树**最高优先 BC 分支**（无压缩否决 >
  BC7 > BC3 > BC1 自动 > RGBA8）——`texture_gpu_bytes` /
  `gpu_bytes_estimate_mipped` / `ensure_texture` 三处同步（R66e 两口径
  纪律推广到三口径）；`Bc7RgbaUnorm[(Srgb)]` 格式随 `cpu.srgb` 分流、
  mip 链走 `mip_chain_bc7`（RGBA8 域降采样后逐级编码）；
  `Renderer::texture_is_bc7` 观测口。
- **manifest `compression` 字段**：`Compression { None, Bc1, Bc3, Bc7 }`
  解析（非法值中文拒绝）+ legacy `bc3: true` 读兼容映射 + 双声明择新
  （`effective_compression()` 单一事实源）。
- 诚实边界 §8.63 六条（仅 Mode 6 / 端点策略非联合优化 / P-bit 共享
  量化面 / 永不自动的内存取舍 / mip 在 RGBA8 域 / 清单自动接线随
  KTX2 批）。
- 测试 +13（bc7 单元 9 · GPU 端到端 2 · 三口径估算 1 · manifest 解析 1）。

### Added（C3 闭合战役 · 物理深化役——CCD × 铰链关节 × 可断关节，全部「缺席 = 逐位不变」）

路线图 v5 清账战役第四批（C0 文档健康 → C1 原语小件 → C2 渲染伪影 →
本批）。池项 #3 CCD / #1 关节约束 v2 交付；#2 OBB+凸包按裁定走
**确定性设计专章先行**（`docs/private/gjk-determinism.md`：算子集合
限定、FMA 口径、迭代界与边界归属、退化病理与回退、OBB-SAT 先行 /
凸包 GJK 缓行的拆段建议——待用户过审，未过审则整项缓行）。三件全
opt-in，全部既有物理/游戏快照测试零改动原值通过：

- **CCD 连续碰撞检测（§8.28 角色「全贯穿不检出」+ 刚体穿墙闭合）**：
  刚体侧 `Ccd` 组件——积分位移按段长（自动 = `ColliderShape::min_width`
  最小投影宽度，或显式 `max_step`）拆等分子步，在首个「终点嵌入静态 /
  运动学障碍」的子步停住，嵌入终点交还**本帧**接触管线（推离 / 反弹 /
  `CollisionStarted` 全照旧）；捕获保证有区间论证（段长 ≤ 本体宽度 ⇒
  重叠窗口必落入子步终点，任意厚度静态障碍可检出）。核心
  `ccd_substepped_displacement` 纯函数解析可单测（薄墙对照 x=5.0 精确
  停步）；过滤位组对称适用、sensor 双向豁免、子步数封顶 64、**亚阈值
  位移原样返回（v1 逐位兼容锚）**。角色侧 `move_and_slide_ccd`——五步
  管线子步包装（自动段长 = radius；v1 对照测试「单帧穿 0.2 m 薄墙」
  留在场证明原位，包装后挡停；steps=1 逐位 = 原入口锚）。
- **铰链关节 `HingeJoint`（§8.27② hinge/twist 闭合）**：三段位置投影
  （锚点距离 → 双轴最小弧对齐（摆动自由度全闭合）→ 绕铰链线的刚体
  旋转），扭转角带符号测量（Gram-Schmidt 正交参考向 + `atan2`）、限位
  自动排序 + 非有限消毒、**速率限制马达**（`|Δθ| ≤ rate×dt` 伺服、
  马达不能越限位）；层级投影全给子端（与 SwingJoint 同款实测理由）、
  限位内零修正幂等锚（不写旋转、逐位不动）、旋转收敛后失活（末位舍入
  不改写姿态）。`SwingJoint` 原语义一字未动（两关节并存、各自 opt-in）。
- **可断关节（§8.27② 可断闭合）**：`JointBreak` 标记 + `BrokenJoints`
  队列资源 + `prune_broken_joints` 系统（`PhysicsPlugin` 注册，Update
  阶段 step 之后）。判据 = 每帧**位置修正累计量**（米；四种关节、跨
  松弛轮、旋转修正不计）；step（`&World`）只入队、prune（`&mut World`）
  移除该实体全部关节组件并发 `JointBroken` 事件——两拍结构是 `&World`
  系统无法改组件表的结构约束，非设计偏好。双开关：无标记 / 无队列
  资源 = 整体关闭（应力采集都不发生）。
- 诚实边界入 §8.59（CCD 静态障碍专属 + 距离上界 / 马达无扭矩 /
  断裂实体粒度 / 角色包装重力分步序差）。

**C3d 追补（同役续批：专章过审拆段，OBB 排码）**：用户裁定「过审拆段」
后排入——`ColliderShape::OrientedBox` 新变体（**opt-in，`Box` 的 AABB
语义 v1 固化不动**，既有全部形状对逐位不变）：obb×obb 走 **SAT 15 轴
闭式**（6 面法线 + 9 叉积轴；准平行叉积轴跳过分立、并列分离轴取固定
枚举序首位、相切不算接触、中心重合 `signum` 零取正）；obb×{Sphere,
Capsule, Box} 经**局部系刚体变换复用既有闭式内核**（零新数学，法线
旋回世界）。射线局部系 slab（yaw90 ≡ AABB 对照）、broadphase 包围
AABB（|R|·h，8 角点全含测试）、v2 惯量随体旋转、接触点 obb×obb 取
中心中点（§8.60① 声明）、nav 保守不支持顶面（②）、关卡格式
`{"oriented_box": {...}}` 段往返位级一致。专章验收四件套落地：
**算子看门单测**（OBB 内核源码 regex 禁超越函数，R61 影子契约模式）、
**固定种子 200 构型池**（splitmix64 两跑 to_bits 全等 + 命中性配额）、
**边界归属断言**（相切 / 并列 / 符号逐条写死）、**恒等旋转兼容对照**
（obb ≡ Box ±1e-5）。凸包 GJK/EPA（专章 P2）维持缓行。诚实边界入
§8.60（七条）。

测试 +28（CCD 纯函数 5 + 弹丸集成 1 + 角色包装 3 + 铰链解析 6 + 铰链
step 集成 1 + 断裂闭环 1 + OBB 纯函数 9 + OBB step 集成 1 + OBB 序列化
1，其中含 v1 bug 在场对照与逐位兼容锚各一组）。§8 翻转：27②、28
（全贯穿条目）；OBB 条目落定为「AABB 固化 + OrientedBox 独立变体」。

### Added（C4a 闭合战役 · 动画役——形变目标 v1：glTF morphTargets 加载 × CPU 混合 × 逐实体动态 VBO）

路线图 v5 清账战役第五批（C0 → C1 → C2 → C3 → 本批 · 动画役第一项，
池 #8）。§8.9 / §8.24 的「形变目标在路线图」条翻转，v1 诚实边界新立
§8.61（八条：仅蒙皮路径 / CPU 混合成本 / 通道裁剪 / 权重直通 /
包围界按基座 / meshlet 不经手 / 缓存只增 / 权重不入序列化）：

- **数据模型**：`MeshData.morph` 侧表（`MorphTarget` = 绑定姿势空间的
  每顶点位置增量 + 可选法线增量 + 可选名）+ `MorphWeights` 逐实体
  组件（`min(len)` 配对）。**组件缺席 = 绑定姿势 = 现路径逐位不变**。
- **混合纯函数** `blend_morph_vertices`：表序固定累加（确定性）；
  法线有增量参与时重归一化（glTF 标准口径，除零守卫逐顶点）；
  **`w == 0` 目标整体跳过**——`-0.0 + 0.0 = +0.0` 的 IEEE 符号位
  翻转挡在逐位锚外（`skin_vertex` 零权重跳过同法）。
- **执行路径（零着色器、零管线布局改动）**：CPU 混合结果上传逐实体
  `COPY_DST | VERTEX` 缓冲（按蒙皮实体位标识缓存，与 `GpuSkin` 同构），
  蒙皮绘制分支 11 个 slot 0 绑定点（主 / 透明 / 方向与点光阴影 /
  场景深度 / RT / 探针捕获 / 水面）经 `SkinnedItem::vertex_buf` 换绑。
  脏检 = 权重位图案对账（静态权重不重混合不重上传），`morph_uploads`
  观测计数钉住。查询元组扩至 8 项 ⇒ `iris-core` 元组查询实例化补一档
  （序稳定论证不变：len/entity_at 恒委托首元素）。
- **加载器**：`primitives[].targets` + `mesh.extras.targetNames`（手写
  GLB 解析器新增 serde 字段；增量走 `read_floats` 同扇门，稀疏 /
  非 FLOAT 沿用全池 `Unsupported`）；缺 POSITION 的纯法线目标补零表，
  两通道皆缺 ⇒ 拒收。
- 测试 +12（CPU 混合解析与逐位锚 8 · gltf 加载与拒收面 2 · GPU 端到端
  2——**像素级「零权重 = 组件缺席逐字节一致」** + 位移右越界 + 阴影
  一致性带差分；新 GPU 文件自带串行互斥）。
- 附带闭合：`iris-assault` `geom.rs` 与四处测试 fixture 的 `MeshData`
  字面量补 `morph: None`（新增枚举侧表的编译面涟漪，零语义）。

### Added（C4b 闭合战役 · 动画役——状态机 v2 条件转移 × 运行时按名重定向）

C4 批次第二项（池 #9）。§8.18 时代的 v1 边界「边只带过渡时长，何时
切换由游戏侧判定」翻转（四家游戏自建状态管理自此有引擎承载），
诚实边界新立 §8.62（六条）：

- **参数黑板**：`AnimationPlayer::params`（`BTreeMap<String, AnimValue>`
  浮点/布尔，`set_float`/`set_bool`/`param_float`/`param_bool`）——
  BTreeMap 固定序（序列化 / 调试可复现，同 `AnimationMask` 选型理由）。
- **条件转移**：`transitions: Vec<AnimTransition { from, to, duration,
  guard }>` 与 v1 `edges` **并存**（AnimationEdge 一字未动，全部既有
  构造零改动）；守卫 `AnimGuard` 封闭枚举（`FloatAtLeast`/`FloatBelow`/
  `FloatBetween` 闭区间/`BoolIs`/`ClipFinished`/组合 `All`/`Any`/`Not`）
  ——不做闭包（确定性 + 可序列化 + PartialEq），缺失参数取类型中性默认。
- **步检**：`step_state_machine()`（`update_animations` 写回阶段每帧
  一次）——声明序首条命中、**一帧至多一条**（防乒乓/级联）、自环跳过、
  越界目标无条件断言；与 `request_state` 共享抽出
  的 `enter_state` 落账口（字段同步语义单一事实源）。**`transitions`
  空表 = 恒 no-op = v1 逐位不变**（默认兼容锚）。
- **运行时重定向**：`retarget_by_name(target_names, target_entities) ->
  RetargetReport`——通道不改写、只重绑 `node_entities`（节点下标恒
  源骨架空间）；骨名归一 `normalize_bone_name`（小写 + 剥
  mixamorig/bip01/mocap/ue4-skel 前缀 + 空格压 `_`，纯函数解析测试
  钉住）；**不猜语义改名**（有名未命中断绑 None 合流既有语义、无名
  节点保留、空名表 no-op——alias 表归调用方，猜错的绑定比断绑更难
  排查）。兑现 §8.35⑨「拒绝导入期重定向」的运行期对偶：iris-exp 的
  离线 Blender 烘焙路线自此有引擎内替代。
- 测试 +15（守卫边界 / 声明序裁决 / 一帧一条 / 自环 / ClipFinished /
  复合守卫 / 中性默认 / 越界 panic / 两跑确定性 / 系统端到端步检 /
  重定向四形态 / 归一化解析表）。

### Added（C2 闭合战役 · 渲染伪影役——四项「缺席/默认 = 逐位不变」，两条既有 bug 随修）

路线图 v5 清账战役第三批（C0 文档健康 → C1 原语小件 → 本批）。四条均为
「在场才生效」的 opt-in 形态，全部既有像素锁原值通过（无基线修订）：

- **贴花受光 v2（§8.19 直射项闭合）**：新资源 `DecalLighting`（缺席 =
  输出 = 纹理 × tint 逐位不变）。在场时逐实例 location 10 开关置 1，片元
  法线由逆世界盒矩阵重建（`(M⁻¹)ᵀ e_z` = 逆矩阵 z 行——端盖平面法线对
  非均匀缩放仍正确，inverse-transpose 性质），朝相机一侧翻正，输出乘
  「方向光 Lambert + ambient」因子（standard 漫反射口径、albedo ≡ 1）。
  实例布局 144B → 160B（10 × Float32x4，`lit` 向量的 yzw 保留恒 0——
  同批次恒等值，插值无中间态）。CPU 镜像 `decal_lit_factor` 解析三档；
  `decal.wgsl` Globals 前缀（到 ambient 五字段）+ location 10 契约 =
  **影子契约**，新增 pipeline.rs 源码层看门
  `decal_globals_prefix_and_lit_attribute_match_standard`（R61 教训的
  制度化）。GPU 双锚：中性光照（ambient≡1、intensity 0）开/关逐位一致
  （乘 1.0 IEEE 恒等退化锚）+ 顶光 ×2 判别（哈希差 + ROI 亮度升序——
  地面自发光不受光 ⇒ 差异只能来自贴花）。诚实边界：不采样阴影贴图、
  不吃点光/聚光/IBL/探针/DDGI/GTAO（延迟 decals 完整合成维持拒绝）。
- **光照贴图 mip 链（§8.31⑤ 闭合）**：`MipSettings` 在场 ⇒
  `LightmapItem::mip_chain_half_bytes` 线性域 2×2 box 链（光照贴图存
  线性 HDR ⇒ 线性平均即正确定义，无 RGBA8 路径自认的编码空间均值失真；
  截断维数序列 + clamp 边缘窗与 `downsample_rgba8` 同口径）逐级
  `write_texture` + 三线性/各向异性采样器随上传期创建、绑进材质组
  （与 `GpuTexture { view, sampler }` 同构；SSS LUT 经同 binding 显式
  level 采样不受影响）。缺席 = 单 mip + 采样器逐字段 = R52c（既有基线
  零漂移实证）；`gpu_bytes_estimate_mipped` 对账口径 + 解析链字节锚 +
  GPU 判别三件套（观测 API `lightmap_has_mips` / 64×48 缩比哈希差 /
  同配置逐位确定）。
- **粒子 v2 序列化（§8.14 修正 + §8.51⑩ 闭合）**：关卡档/场景 JSON 新增
  `particles_v2` 段（v1 全字段 + `trail_length`/`trail_width`，缺省 =
  `ParticleEmitterV2::new(Alpha)`；帧队列/池游标/存活批表等运行态不入档
  ——v1 同哲学）。审计顺带实锤：**§8.14「JSON 场景无粒子段」是 R57 后
  的过期陈述**（v1 `particles` 段早已在位），本轮一并修正。往返位级
  一致 + 新鲜态（池游标/存活估计 = 0）断言。
- **反射探针编辑入口（§8.37⑧ 闭合，含两条既有 bug 修复）**：
  ① 序列化：`probe` 段（`half_extents`/`falloff`，缺省 = 组件 Default，
  退化半边长**加载期 OutOfRange 拒绝**——不静默进收集期告警路径）；
  ② 编辑器：`M` 摆放（单位立方体视觉标记 + 组件，**双写契约**：组件
  `half_extents` 权威、标记 `Transform.scale = 2×half` 随命令同步，探针
  采样不读 scale）、选中时 `[ ]` = 过渡带 ∓/+0.5 m（钳 [0,10]）、
  `- =` = 半边长 ÷/×1.25（钳 [0.05,100]，下限防零体积盒）；
  ③ **修复 undo 丢失 bug**（§8.37⑧ 声明的「快照组件集不含
  `ReflectionProbe` ⇒ undo/redo 后探针丢失」——探针进 `EntitySnapshot`
  字段集，headless 逐步 undo 对账）；④ 烘焙入口回报 `probe_count`
  warn 提醒重烘（捕获仍属游戏/工具侧 GPU——§8.37③ 立场维持，编辑器
  命令层保持 headless 可测）。测试：摆放双写 + 步进 + 逐步 undo 全删
  全建（句柄改变走查询断言）+ 入档往返逐字节稳定 + 钳域封顶托底 +
  无选中 no-op 守卫，共 3 项 headless。

§8 状态翻转五条：14（含过期修正）/ 19 / 31⑤ / 37⑧（undo 丢失 bug 修复
计入此条）/ 51⑩。本批无独立 Fixed 条目——唯一的既有缺陷修复（探针
undo 丢失）属 §8.37⑧ 声明内容的闭合。

### Added（C1 闭合战役 · 原语小件轮——四件独立小项，全部「缺席 = 逐位不变」形态）

路线图 v5 暂停新功能后的清账战役第二批（计划 = `docs/private/closure-plan-v1.md`，
用户裁定 2026-09-18：全范围 / 顺序 C0→C1→… / 光照口径⑫ 挂账）。

- **`World::register_component` + `is_component_registered`（iris-core，闭合
  §8.42）**：`&mut World` 侧一次性幂等预注册，根治「早期 `insert_registered`
  补写在未注册类型上静默落空」的装配顺序坑（iris-infiltration 实景：旧图元
  滞留与新房 z-fighting、两轮排查）。顺手修一处**文档 / 代码落差**：
  `insert_registered` 未注册分支此前实际不 warn（§8.42 原文写「只 warn」），
  C1 起真发 warn 且消息直指预注册 API。两态返回口径写明（None = 失败或
  无旧值，成败经回读 / 自检判别），不 breaking 改签名。
- **`RenderTargetStore::resize`（iris-render，闭合 §8.20 一条）**：CPU 描述
  即时改尺寸（无效句柄 false、零尺寸钳 1 与 `add` 同口径）；渲染器
  `GpuRenderTarget` 增尺寸记账，`ensure` 失配臂**整体重建**（RT 实例缓冲
  本就每帧全量上传，无增量损失）；GPU 锚 = 「先 64 占缓存 → resize 96
  重建」与「直建 96」逐像素一致 + 同尺寸空操作快路锚；`rendertarget_render.rs`
  全文件补 `GPU_SERIAL` 互斥（AGENTS 纪律，六项曾漏配）。
- **LOD v2：双阈值滞回 + 包围球中心参照（iris-render，闭合 §8.16 两子项）**：
  新资源 `LodSettings { hysteresis, bounds_center }`——**缺席 = v1 逐位不变**
  （照 TaaSettings / SsrSettings 缺席范式）；高→低阈值 `s·(1+h)`、低→高
  `s·(1−h)`、带内维持原档；`h = 0` 三档 prev 全部逐位退化 v1 `select`
  （兼容锚：CPU 参数扫描 + GPU「记忆残留仍照单阈值」帧）。跨帧记忆
  （实体位 → 档位，`BTreeMap` 确定序）**只由主相机收集更新**，RT / 探针 /
  水体捕获按当帧单阈值（多上下文分表语义可疑，明说为诚实边界）；
  `bounds_center` 参照系只对 **high 网格** 取界（防「选中档位反馈进距离」
  自激振荡，无 CPU bounds 回退原点 = 保守方向）；`LodMesh3` 不吃本设置。
  消毒：h 非有限回 0、负钳 0、1.0 钳 0.999。GPU 判别用例：滞回带五帧
  振荡序列（两个判别点 = v1 会切、v2 维持）+ 偏心网格三帧参照切换。
- **`TextureStore::add_uncompressed_declared`（iris-render，闭合 §8.45 可达性）**：
  一步式「登记 + 无压缩声明」（与 `add_bc3_declared` 同族）——引擎原生
  程序化数据贴图此前拿不到 glTF 加载器的自动声明，近距法线可见 BC1 块状
  伪梯度（frictions ⑨ 同款机理）。默认口径不变（常规 `add` 仍走 BC1 树），
  等价锚 = 一步式 ≡ `add + declare`（目录 / 声明位 / 显存估算三口径）。

零渲染路径漂移：四件全为「默认 / 缺席 = 既有基线零改动」形态（全部 FNV
锁预期原值通过；lod_render / rendertarget_render 为新增用例）。测试基线
+9（debug/release 同步，收官数字见 AGENTS.md）。

### Docs（C0 闭合战役 · 文档健康轮——零代码改动）

路线图 v5 暂停新功能开发、转入诚实声明清账战役的第一批（计划全文
`docs/private/closure-plan-v1.md`：§8 全量 58 条三分类底账 + 21 项闭合候选
逐条代码级存在性核实 + C0~C7 批次编排与用户裁定）。本批修复审计实锤的
**声明过期**：

- **§8.17 双处过期修正**：①「无 BC3」——R59 已落地 BC3 声明门
  （`bc3_eligible` 分层互斥，§8.35③/④），原文未同步；②「无 mipmap」——
  R66d `mip_chain_bc` 已闭合 BC 路径 mip 链（§8.43 自标闭合），同文档两条款
  互斥。修正采用删除线 + 修订注记形态（与本仓「合法打破基线走归因+重锁、
  不静默改值」同纪律）。
- **§8 编号「22」重复消歧**：dogfooding V 与景深/MB 两条同为 22，既有引用
  （§8.22①~⑤ 指前者、无子目标号的景深语义指后者）在 §8 头部加「阅读约定」
  注记消歧并锚定引用口径；**不做全文重编号**（CHANGELOG / 模块文档引用
  锚定现编号，批量漂移风险大于收益）。景深条标题内嵌消歧标记。
- **§8 头部阅读约定**：明写三类条目状态（档案记录 / 主动裁定维持 / 排期债），
  防后续轮把设计边界误当 bug「闭合」。
- **README 测试数修正**：1081 → 1546（release 1543，与 AGENTS.md 基线对齐）。

### Added（R89：DDGI v2——合成语义五方定稿 × 覆盖度连续化 × 编辑器入口）

路线图 v5 第三梯队「全局光照」2/2（R88 DDGI v1 的收官轮）。开工前设计
定稿（4 项用户裁定 + 15 项决策及理由）留档在 `docs/ddgi_v2_design.md`；
语义权威 = ARCHITECTURE §4.45 与 §8.58。

- **合成语义五方裁决（§8.57⑫ 移入实现）**：①× 烘焙光照贴图 = **互斥，
  贴图赢**（挂图集实体不发探针标志位 ⇒ 完全吃不到 DDGI；贴图已含
  `sky + bounce`、相加即双计；**差值法裁定拒绝**——需要持久化基线表 +
  二次重照明对照 + 负增量钳位 + 「贴图与网格参数严格同源」的脆弱契约，
  收益可由美术流程自行规避）；编辑器烘焙在 `DdgiGrid` 非空时发状态提示
  （`BakeReport::ddgi_grid_present`，只提示不裁剪）。②× env 兜底 =
  覆盖度连续混合（见下）。③× ambient 补光 = 按覆盖度分摊（见下）。
  ④× 反射探针捕获 = 恒 mode 0、吃 `instance_payload` 的零阶载荷（R88
  事实的显式裁决 + 锚；反射里的间接漫反射无方向性、无遮挡感知——低频
  量）。⑤× SSR = 零代码改动的一致性（上帧颜色副本含全部 GI ⇒ 命中色
  天然跟随 DDGI 表；锚钉住）。
- **覆盖度连续化（一次修掉 §8.57⑬ ambient 双计 + §8.57⑱ 扇贝带）**：
  逐片元几何覆盖度 `cov = max_i clamp((r_i − d_i) / (r_i × 0.25), 0, 1)`
  （内区 `d ≤ 0.75·r` 恒 1、半径最外 25% 羽化带），与 `Σw` 同遍固定序取
  `max`；①env ↔ GI 从硬 `select` 换 `mix(env, gi, cov)`（半径球面并集的
  断界 → 连续过渡）；②ambient 行追加 `× (1 − cov)`（同源天空底色不再
  双计）。**逐位口径**：`cov = 1` ⇒ `mix(x, y, 1) = y` 精确恒等（内区 =
  R88）；`cov = 0` ⇒ `× 1.0` 恒等（mode ≤ 1 与 DDGI-off 零变化）。CPU
  镜像 `ddgi_coverage` / `ddgi_coverage_with_fade`（fade→0 退化锚）/
  `ddgi_blend_with_cov`；`ddgi_blend` 原签名薄壳保留（既有调用零改动）；
  WGSL 源码层看门扩展到 `DDGI_COVER_FADE` 与两处消费点形态。
  **像素修订声明**：R89 修订了 DDGI 在场场景的像素（外壳羽化 + ambient
  分摊两处，台账见设计稿 §8.2）——R88 的 `docs/ddgi.png` 四面板断言
  原样复跑全过（对比度因去双计反而增强），cov=1 全等场景与 R88 代码的
  实测差异 = 全帧恰 **1 字节 ±1 LSB**（R63 同款编译期副作用，归因报告
  见下偏离声明）。
- **编辑器 DDGI 入口（四步顺序不可倒）**：**C0** 新 marker 组件
  **`BakeExcluded`**（`iris_render::bake`，缺席 = 收录 ⇒ 既有场景逐位
  不变）——编辑器叠加层此前会进 GI 共用三角收集（挡光 + 撑大
  `default_bounds`）；`collect_bake_triangles` 查询元组已满 7 项 ⇒ 在
  既有两阶段收集的阶段二逐实体判定（零重构）。**C1** 命令层直抄光源
  编辑四步模板：`DdgiEditMode{Inert, Grid, Budget, Debug}`（`X` 循环，
  默认 Inert = 既有键位零变化）+ 间距 / bounds 平移（`None` 时按引擎
  同式物化）/ 每帧探针数 / 射线四档 / hysteresis 五条步进命令 +
  `V` 叠加开关；`WorldSnapshot` 增 `ddgi` 字段（**「启用 DDGI」本身是
  可撤销的一步**）；资源缺席时第一条命令插入默认资源。**明确不做数值
  输入面板**（既有拒绝：牵连 §8.33② 评审）。**C2** 可视化 = 探针标记球
  走主 pass（零新 pass / 零新 binding），脏红 / 稳态绿状态着色
  （`dirty()` + `probe_stamp()` 只读零成本），`EditorTransient` +
  `BakeExcluded` 双标记；维持「手动重建、不自动刷新」立场。
  **C3** 入档 = `LevelSection.ddgi: Option<DdgiSection>`（逐字段
  serde 默认 + 整段缺席省略，**`SCENE_FORMAT_VERSION` 不 bump**——
  nav/bake/字段级三处先例）；`geometry_generation` 不入档；加载段 →
  资源（校验前置拦截为 `SceneError`），保存从活动资源回写（照
  `DirectionalLight` 先例）；**探针表持久化维持拒绝**（§8.57⑭）。
- **GPU/CPU 测试**：`tests/ddgi_render.rs` 6 → 11 项（R88 六项原值零
  改动通过 + R89 五项：cov=1 内区新基线 + DDGI-off 对照 R88 原值、图集
  互斥、SSR 强度旋钮 + 反射随表变、reset_taa 不扰 DDGI、reset_taa 清
  GTAO）；CPU 单测覆盖 `ddgi_coverage` 边界解析解（球心 1 / `d = r` 恰
  0 / `d = 0.75r` 恰 1 / `r = 0` 无 NaN / max 序无关）/ mix 端点恒等 /
  fade→0 退化 / 兜底支 env 语义 / 编辑器步进 undo 逐字符复原 /
  `DdgiSection` 逐字段往返逐字节 + 无段档逐字节不变 + 参数步进 ⇒
  `structure_generation` 前进。验收图：`docs/ddgi.png` 重生成（R88 四
  面板断言原样全过）+ **新** `docs/ddgi_v2.png`（关 | 羽化 | 漏光 |
  ambient 去双计：GI 表经 `sky_color` 锁同后，ambient 拉高 40× 而内区
  ROI **逐位不动**、球外可判别——A3 的锐利像素证据）+ **新**
  `docs/ddgi_editor.png`（编辑器叠加层收敛前沿 + HUD 状态行）。

### Fixed（R89）

- **SSR 强度旋钮从未生效（R62b 引入，26 轮未发现）**：
  `SsrSettings::intensity` 被上传到 `ssr.flags.y` 后着色器**零读取点**
  （模块文档的合成公式多了「强度」项）。修复：SSR 命中权重
  `clamp(roughness_fade × edge_fade × intensity, 0, 1)`——强度 0 ⇒ 与
  SSR 关闭逐位一致（零反射），> 1 被钳到 1（**本项是命中权重、不是亮度
  倍率**，口径写进字段文档）。配套新增合并权重镜像
  `post::ssr_blend_weight` + 非默认强度断言（此前只有 roughness / edge
  两个分量镜像、整条公式无镜像 = bug 藏住的结构性原因；AGENTS.md 测试
  规约随之新增「每个公开旋钮至少一条非默认值断言」）。**既有像素零
  变化的依据**：全仓每一处 `SsrSettings` 构造都走 `..Default::default()`
  ⇒ `intensity` 恒 1.0 ⇒ 乘法与钳制均逐位恒等。
- **`reset_taa` 漏清跨帧时域状态（iris-exp frictions ⑬）**：切镜后首帧
  GTAO 仍按**旧场景深度**算可见度（旧物体形状投出假遮蔽并被 TAA 吸收
  拖尾多帧）。修复：`reset_taa` 语义定稿为「清一切跨帧时域状态，除了
  探针域」——补清 `ao_depth_frame`（GTAO 上帧深度记账 ⇒ 切镜首帧可见
  度图中性 1.0、`FrameStats::ao_on == 0`、第二帧起正确）与
  `prev_view_proj`（消费点 `unwrap_or(vp_raw)` 的首帧语义 = 零速度透传，
  等价「无历史」）。**DDGI 表 / `probe_stamp` / `structure_generation`
  是故意不清的那一类**（探针域状态与镜头无关，锚
  `reset_taa_does_not_disturb_ddgi` 钉住）。不调 `reset_taa` 的宿主逐位
  不变；iris-exp 的「连渲三帧」绕行已撤除（`exp_snapshot` 回到
  `reset_taa()` + 单帧定格）。

### 偏离声明（R89，全部有依据）

1. **mode 2 的 env 兜底从「调用方 select」改为 `ddgi_irradiance` 内部
   完成**（env 值作参数传入、`Σw ≤ ε` 返回 `(env_fallback, cov)`）——
   覆盖度混合要求兜底支仍返回真实 `cov`（ambient 分摊语义在兜底支同样
   成立），且 cov=1 的兜底像素与 R88 的 env 回退逐位同值。
2. **ambient 补光行在 DDGI 在场时被 `cov` 衰减**（含挂图集实体）——对
   R52c/R63「ambient 是独立美术补光预算、探针不接管」的**部分推翻**：
   裁定 3 让 `sky_color` 与 ambient 同源后「不接管」不再自洽（双计）。
   新口径 = 「ambient 与 DDGI 天光项同源 ⇒ 按覆盖度分摊」。影响与理由
   显式记录于本条与 §4.45，未并入 Added 含糊带过。
3. **`collect_bake_triangles` 未扩查询元组**（设计稿预案为扩项或两阶段
   重构）——既有的两阶段收集已天然提供落点：`BakeExcluded` 判定放在
   阶段二循环首行（`world.get` 逐实体点查），查询元组 7 项不动、输出
   序 = 收录实体的查询序 ⇒ **R52a/R64 既有逐位口径零影响**（无标记
   世界输出逐位不变，单测钉住带/不带标记的差异可判别）。
4. **SSR 权重公式补强度项**是修复而非改义，但它动的是既有已发布特性的
   像素路径——按 §8.2 台账程序声明：全仓 `intensity` 恒 1.0 ⇒ 既有
   `docs/ssr.png` 与 SSR 基线逐位不变（乘法与钳制对 1.0 与 [0,1] 值均
   精确恒等）；非默认值的有效性由新镜像单测与 GPU 锚承担。
5. **`reset_taa` 语义从「清 TAA + SSR」扩到「清一切跨帧时域状态」**，
   且 DDGI 是**故意不清**的那一类（探针域与镜头无关）——`prev_view_proj`
   的清理在实现期核实为等价（唯一消费点 `unwrap_or(vp_raw)` = 首帧零
   速度语义）后才落。`occ_frame` / `meshlet_frame` 等帧号记账不属
   「历史」（自愈型新鲜度机制），不在清理面。
6. **编辑器 DDGI 入口不含数值输入面板、不含探针实体化**——尊重
   `edit.rs` 既有拒绝（`LevelRegistry` 语义变更需单独评审）；路线图
   字面「探针放置入口」实交付为「参数步进 + 可视化 + 入档」三件套
   （不能逐个放 / 删探针），「排除格」笔刷列为后续候选。
7. **cov=1 全等锚的口径**：GPU 锚锁定的是 R89 修订后的新基线
   （cov=1 场景、ambient=0），与 R88 代码的实测差异 = 全帧 196,608 字节
   中恰 1 字节 ±1 LSB——mode 2 分支的增量代码改变 GPU 编译器 FMA /
   公共子表达式决策所致（R63 教训同款：**语义等价不保证编译期逐位**）。
   语义等价性由「CPU 镜像 mix 端点恒等单测 + DDGI-off 对照组与 R88
   逐位一致 + 差异占比 0.0005%」三重背书；R88 原值（0x7353e0ce27f6a7c6）
   与 R89 新基线一并记录在测试注释与归因日志。
8. **编辑器可视化三档着色的「粗填琥珀档」在当前状态机下为空集**（安装
   帧全表标脏、精修即绿，「非脏且 stamp == 安装帧」恒不成立）——判定
   保留（数据面完备、未来状态机演进即用），验收图实际呈现红 / 绿收敛
   前沿。

### Added（R88：DDGI 动态全局光照 v1——规则探针网格 × 时间片滚动重照明 × 深度感知混合）

路线图 v5 第三梯队「全局光照」1/2（R88 DDGI v1 → R89 DDGI v2）。**闭合
§8.52①**（R83 的径向权重不感知遮挡，「隔着薄墙的两个探针在墙两侧互相
串味」，影响半径是唯一隔离手段），附带缓解 §8.52⑩（探针布局是作者责任）。
R83 / R64 的数据与路径**一字不动**，继续作 mode = 0/1 的兼容轨与对照轨。
开工前设计定稿（决策点裁定与理由）留档在 `docs/ddgi_v1_design.md`。

- **规则探针网格（`iris_render::ddgi`，纯资源态、不落实体）**：
  `DdgiSettings` 资源**缺席 = DDGI 关**（渲染器一个 `write_buffer` 都不发、
  binding 20 绑 4 B 哑缓冲、mode 维持 R83 裁决）。格心点阵
  `min + (i + 0.5)·spacing_eff`、索引序 **x 最快**（⇒ 滚动刷新是 x 行扫描）；
  `bounds = None` ⇒ 静态几何 AABB 外扩 1×spacing **且 Y 下限额外抬
  0.5×spacing**（探针埋地板下会把地板上的着色点判成「墙后」⇒ 整片 GI 被
  抑制）。不落实体的理由：避免 256 实体的 ECS 开销与 `&mut World` spawn
  限制（`&World` 的系统无法注册新资源 / 组件类型）。
- **确定性抽稀（不是按序截断）**：超 `MAX_IRRADIANCE_PROBES`（256）⇒
  `stride_for_capacity` 求**最小整数 k** 使 `⌈nx/k⌉·⌈ny/k⌉·⌈nz/k⌉ ≤ 256`
  （整数二分 + `checked_mul`，无浮点搜索 ⇒ 逐位确定；`nx` 饱和到
  `usize::MAX` 也不 panic），`spacing_eff = spacing × k` + `log::warn!` 一次。
  网格仍规则无洞、**覆盖范围不变**——按序截断会砍掉远端一整块空间 ⇒ 该块
  GI 直接回退 env，出现可见的空间断界。半径用解析式 `spacing_eff × 1.5`
  （规则网格的最近邻恒定 ⇒ 不跑 O(n²) 的 `derive_probe_radii`；单测断言
  二者在规则点云上**逐位一致**，防口径漂移）。
- **时间片滚动重照明（复用 R52a 的 BVH 追踪器）**：`advance` 纯函数自
  cursor 起环形扫描至多 n 个位置取前 k 个 dirty 者，`cursor'` = 最后被更新者
  的下一位；收敛帧数 = `⌈n_dirty / k⌉`（解析可断言，默认 256/2 = 128 帧
  ≈ 2.1 s @60fps）。**光照指纹**（`light_dir`/`light_color`/`sky_color`/
  `bounces`/`rays_per_face` 的 `to_bits` 固定序位组合）或**几何 generation**
  变化 ⇒ 全表标脏但**不清空旧值**（旧值继续用、逐帧刷新 ⇒ 无闪烁，这正是
  DDGI 的时域行为）。**稳态（无脏探针）⇒ tick 立即 return**：静止场景 +
  恒定光照的 CPU 与上传成本双零（「缺席 = 零开销」文化的延伸）。
- **安装帧粗填**（`DDGI_PREFILL_GRID = 2`，6×2² = 24 方向/探针，满载一次性
  ≈ 3.3 ms）：避免前 `⌈n/k⌉` 帧出现「SH = 0 的黑波按网格索引序扫过场景」。
  粗填**只填 SH、深度矩置中性**（μ = 天空哨兵、σ² = 0 ⇒ 深度项恒 1）：
  粗填档 2 < 深度格 8 无法整除映射，且保守方向正确（粗填期可能短暂漏光，
  `⌈n/k⌉` 帧内被真深度取代，优于黑波）。
- **单探针重照明 `probe_relight`：SH 投影与深度两阶矩同一批射线**（⇒ 深度项
  **零额外光线成本**）。SH 累加与 R83 的 `project_probe_sh` **逐式同构**
  （固定面序 × 固定行列序 × f64 定序累加 × `cube_texel_solid_angle` 精确
  固体角权 × 出口 `sh_lambert_convolve` ⇒ E/π 直读）。
- **深度表的数据形态**：每探针 **cube-face 参数化**两阶矩，6 面 × 8² 纹素 ×
  `(μ, σ²)` = 384 × 2 个 f32 = **3072 B/探针**，满载 **768 KiB**（binding 20
  的 `array<f32>` **标量数组**）。索引式
  `(probe × 384 + texel) × 2 + moment`、`texel = face × 64 + row × 8 + col`
  （CPU/WGSL 同式，源码层看门 + 往返双射 + 逐格穷举三重钉住）。
  **σ² 在 CPU 用 f64 求差后钳 `≥ (0.1μ)²`**：`E[t²] − μ²` 在 f32 里会灾难性
  抵消（单测量化：μ = 1e4、σ = 1e-3 时 f32 求差归零、f64 保住信号），钳位
  同时消掉「单纹素零方差 ⇒ chebyshev 退化为硬阶跃」⇒ **着色器零减法、
  零钳位**。未命中 ⇒ **有限**哨兵 `DDGI_SKY_DEPTH = 1e4`（不用 `inf`：会
  污染矩累加 ⇒ σ² 变 NaN）；深度 = `hit.t + BAKE_SHADOW_BIAS`（`t` 从偏移后
  的射线原点起算，加回偏移才与片元侧 `distance(x, p)` 同原点；单测直证
  「垂直向下到地板的深度恰为探针高度」= 偏移往返精确抵消）。
- **深度感知混合（关键算法）**：`w_i = gi_probe_weight(d_i, r_i) ×
  ddgi_depth_weight(d_i, μ_i, σ²_i)`——径向窗是 R83 原式 `(1−(d/r)²)²`
  **零改动**；深度项是 DDGI 原式的 **chebyshev 单侧上界**：`dist ≤ μ ⇒ 1`
  （着色点在探针可见范围内），`dist > μ ⇒ σ²/(σ² + (dist−μ)²)`（按「真实
  深度 ≥ dist」的概率上界衰减 ⇒ 墙后权重趋 0）。**双剪枝**（`radial > 0`
  且 `w > 0`）后才做 9 项 SH 点积。角分辨率 ≈11.25°/纹素（面心），
  nearest-texel 无双线性插值。
- **hysteresis 旋钮默认 0（关）**：`sh = old × α + fresh × (1 − α)` 逐系数
  （深度矩同款）。默认关 ⇒「收敛帧数 = `⌈n/k⌉`」可解析断言；α = 0 逐位等于
  纯替换、α = 1 冻结、α = 0.5 解析中点三条单测钉住。
- **GPU 接线**：binding **20** = 深度表 read-only storage（组 0 的 0..=19
  已满；片元阶段只读 storage 计数 = 4（16/17/18/20），**恰在** WebGPU 默认
  `maxStorageBuffersPerShaderStage = 4` 之内 ⇒ 下一轮再加片元 storage 必须
  先申请特性或改用纹理）；binding 19 参数扩位 `x = mode(0/1/2)`、
  `y = 探针数`、`z = DDGI_DEPTH_GRID`、`w = 深度项开关`。**不扩 `Globals`**
  ⇒ `skybox.wgsl` 的并行布局陷阱零触碰、看门单测不动。768 KiB 真表
  **惰性创建**（首个 mode = 2 帧，照 `AoGpu` / `OccGpu`），关闭帧绑 4 B 全零
  哑缓冲；SH 表沿用 binding 18 的 40 KiB 现缓冲（`ShProbeRaw` **零改动**）。
  **mode 门三支**：mode 0 与 mode 1 的**语句逐条不动** ⇒ R83 / R64 既有 GPU
  断言原值通过（R78「双路径位等价 + 均匀门」纪律）。
- **三态裁决与互斥（裁定 2）**：`DdgiGrid` 非空 ⇒ mode = 2 + **整表替换**
  binding 18/19 + **忽略** `IrradianceProbeSet`（`log::warn!` 一次；两份间接
  漫反射叠加会**双重计入**天空与色溢，物理错误）；DDGI 缺席 ⇒ R83 裁决原样
  （`has_sh()` ⇒ 1，否则 0）。loc14 逐实体载荷改由
  `DdgiGrid::instance_payload` 给出（`sh_blend_l0` 的零阶径向混合 + 标志位，
  **与 R83 带 SH 的集同口径**）——这份 xyz 只被 mode = 0 的路径（探针捕获 /
  RT 相机**恒 mode 0**，§8.52⑦ 同款立场）消费，主视图只读标志位。
- **运行期光照口径**（`iris_engine::ddgi::ddgi_bake_params`，纯函数可单测）：
  `light_dir` = `DirectionalLight.direction`、`light_color` = `color × intensity`、
  **`sky_color` = `settings.sky_color.unwrap_or(ambient)`**（裁定 3：R80 昼夜
  **免费联动**；**不乘 `EnvironmentSettings.intensity`**——`ibl_env` 已被
  `env_params.y` 缩放，预乘会双计）、`shadow_softness` = 0（硬影单射线）、
  `sky_samples` = 1（本路径不做半球采样）、`bounces` ≤ 1。
  `globals.ambient` 的常量补光项**不动**（独立的美术补光预算）。
- **编排（`DdgiPlugin`，`DefaultPlugins` 已含）**：Startup 插入空 `DdgiGrid`
  （`&World` 的系统无法注册新资源类型）、Update 注册 `tick_ddgi` 并行系统，
  **注册在 `DayNightPlugin` 之后** ⇒ 昼夜当帧写的方向光当帧被 DDGI 消费。
  两阶段模式（读 settings / light、需要重建时才 `collect_bake_triangles` →
  `resource_mut::<DdgiGrid>` 写回）；**稳态帧一次三角形都不收集**（几何变化
  靠宿主 `geometry_generation` 显式宣告，照 `SkySource.generation` /
  `ensure_environment` 先例）。prelude 两级都补齐（R74 的 §8.26⑧ 摩擦不重演）。
- **确定性**：轮转序 = 纯索引环形扫描（无哈希迭代序）；重照明 = 固定面序 ×
  固定行列序 × f64 定序累加；深度矩 = 固定子格序 f64 累加 + CPU f64 域求差
  钳位；指纹 = `to_bits` 固定序位组合；hysteresis = 固定 α 的一阶确定性递推；
  跨帧累积 ⇒ **双会话 48 帧逐帧逐位**（像素 FNV 序列 + CPU 表哈希序列双断言，
  R60/R62b 范式）。**两条等价锚点**：①`hysteresis = 0` 且单帧 `k ≥ n` ⇒ 表
  逐位等于「全量重照明一遍」；②`rays_per_face = SH_CUBE_GRID`(32) ⇒ SH 表
  **逐位等于** `probe_sh_coefficients`（跨模块锚点，同时证明
  `probe_sh_radiance` 改薄壳未漂移）。跨适配器位级不可保（§8.51 / §8.52⑤
  同款边界）。
- **成本账（`rays_per_face = 16` 的质量档，裁定 1）**：每探针 6×16² = 1536
  方向 = 3072 条射线（primary + 反弹点硬影）≈ **0.83 ms**（1000 三角 BVH，
  基线取 `bake.rs` 模块文档实测的 0.27 µs/ray）；默认 k = 2 的脏帧
  ≈ **1.7 ms/帧**；显存 768 KiB（惰性）+ 40 KiB（既有）；上传增量
  ≈ **6.3 KiB/脏帧、稳态 0 B**。**昼夜循环场景是持续脏表**（光照每帧变）⇒
  1.7 ms/帧是常态开销而非峰值；可调档位 `rays_per_face = 8` ⇒ 0.42 ms/帧、
  `probes_per_frame = 1` ⇒ 0.83 ms/帧（收敛 256 帧）。着色器侧全表线性扫描
  ≤ 256 探针，双剪枝使增量约 10 op/探针。
- **对照模式 `Renderer::set_ddgi_depth_weight_enabled(bool)`**（默认 `true`，
  照 `set_legacy_probe_parallax` / `set_legacy_shadow_bias` 先例）：只改片元的
  权重口径、**不改重照明与深度表的内容** ⇒ 两侧像素差只能来自 chebyshev 项，
  是「漏光抑制有效性」断言的必要开关。不是运行时诊断可视化。
- **观测**：`FrameStats::ddgi_updated`（本帧重照明探针数，取自
  `DdgiGrid::stats().last_updated`，**不进任何排序键**）+ `DdgiStats`
  （probe_count / dirty_count / ticks / stride / spacing_eff）。
- **测试**：CPU 34 项解析 / 逐位单测（`iris-render/src/ddgi.rs`）+ 7 项编排
  单测（`iris-engine/src/ddgi.rs`）+ GPU 6 项（`tests/ddgi_render.rs`，文件内
  `GPU_SERIAL` 互斥、跨世界对照各用独立渲染器）。GPU 六条：①**缺席逐位不变**
  （无网格 / 空网格 / 有 settings 但网格空 三者逐字节相等，且**空 DDGI 网格
  不得扰动 R83 的 mode = 1 裁决**）②开关可判别 + **门口梯度**（受试点 =
  **共享墙的西侧面**而非地板：墙面法线 −X ⇒ `NdotL ≤ 0` ⇒ 直射项结构性为零
  ⇒ 像素里只剩常量 ambient + 间接漫反射；跨模式作差 ⇒ 镜面 / 天空 / 色调
  映射项抵消；梯度本身走 CPU 对照轨——沿墙面扫 z 剖面断言 GI 随离门口的距离
  **严格单调衰减**且首末落差 ≥ 3×（实测 4.04×），暗室全封闭 ⇒ 梯度只能来自
  门口）③**漏光抑制有效性**（同一世界同一渲染器只翻深度项开关；墙面 ROI
  亮度比 ≥ 1.8 且绝对差 ≥ 60，实测 2.88 / 265；CPU 三条精确归因：穿墙权重
  < 0.25（实测 0.059）、同侧探针 ≥ 0.9（实测 1.0，射线恰命中 ROI 所在墙面
  ⇒ μ ≥ dist）、**开阔无遮挡处关 / 开比 ≈ 1**（实测 1.011 ⇒ 遮挡感知而非
  一律调暗）；ROI 避开门口这个混淆源）④**滚动收敛**
  （`ddgi_updated` 序列 = k,k,…,尾帧,0,0 + `⌈n/k⌉` 帧后画面 FNV ≡「k = n
  一帧全量重照明」+ CPU 表逐位）⑤双会话 48 帧逐帧逐位 ⑥昼夜联动（光照
  阶跃 ⇒ L0 单调跟随、`⌈n/k⌉` 帧后逐位到位）。
- **验收图**：`examples/ddgi_snapshot.rs` → `docs/ddgi.png`（2×2：DDGI 关 |
  DDGI 开 | 深度项关（漏光对照）| 夜间相位；场景 = 一墙分隔亮 / 暗两室、
  **门口是唯一连通**，相机在暗室内；受试点同两条 GPU 测试取**墙面**，梯度上端
  取 `z = 0.9`（门口边缘 0.75 之外、仍在墙上）——首稿取 `z = 0` 落在门洞里，
  量到的是透过门洞的东侧场地而非墙面，验收轮已修正；面板 1–3 走 `sun_envelope`
  / `ambient_envelope` 的真实昼间档（面板 4 的昼夜联动要求如此）⇒ 常量 ambient
  + env 镜面项稀释像素比（实测漏光比 1.253 / 差 117，未被稀释的量级见
  `tests/ddgi_render.rs` 的 2.88）；warmup = `⌈n/k⌉ + 1` 帧并在 example 内
  断言 `dirty_count == 0`——不许拿未收敛的表出验收图；跨运行逐字节一致 +
  双会话 FNV 断言）。

#### 与规格的偏离（4 条主偏离 + 2 条小澄清，全部有依据）

1. **mode 2 的 `Σw ≤ ε` 兜底改为回退 `env_irradiance`**（R83 是回退最近
   探针）——回退最近探针会**绕过深度项** ⇒ 墙后片元吃到墙另一侧探针的辐照
   = 正是本轮要消灭的漏光，等于把它从后门放回来。
2. **增量上传偏离 R83 的「每帧重传」**——①数据量 19×（40 KiB → 768 KiB）；
   ②DDGI **有** dirty / cursor / `probe_stamp` 变更记账，不需要靠重传规避
   比对成本；③与 README 既有「实例缓冲跨帧持久：静态场景零上传，动态实体
   仅传变化段」同纪律。R83 的每帧重传路径**一字不动**（mode = 1 帧照旧）。
3. **`probe_sh_radiance` 改薄壳**（函数体搬进新的 `probe_ray_radiance_depth`，
   返回 `(L, t)`）——辐射率口径必须有单一事实源，逐式复制会在后续修订中悄悄
   漂移；看门两道：R83 既有测试**原值通过** + 等价锚点② 逐位相等。
4. **深度存 cube-face 参数化而非八面体图集**（DDGI 论文原味是 oct atlas）——
   与 SH 投影**同批射线** ⇒ 零额外光线成本，且省掉 oct 编解码的 CPU/WGSL
   双侧镜像与看门面（确定性面更小）。
5. （小澄清）binding 19 的 `w` 分量承载**深度项对照开关**（设计稿 §7.1 写
   「w 保留 0」——那条理由是「σ² 已在 CPU 钳过、着色器不需要
   `DDGI_SIGMA_REL`」，成立；但 §7.4 要求的对照模式必须有落脚处，`w` 是唯一
   空位，照 `probe_params.count.w` 承载视差开关的先例）。
6. （小澄清）重照明指纹在设计稿的四项之外**追加 `rays_per_face`**（它决定
   每探针的射线预算，变了旧表就不再是同一口径的产物 ⇒ 必须全表标脏）；
   `shadow_softness` / `sky_samples` **不入**指纹（本路径不消费它们，入指纹
   会让无关旋钮误判全表脏——单测双向钉住）。

### 已知限制（§8.57，18 条）

单色天空（不吃 `SkySource`）/ 无点光·聚光·emissive 参与重照明 / 1 次反弹
上限 / 几何变化靠 `geometry_generation` 显式宣告（**移动的墙与开合的门在宣告
前会投影出陈旧遮挡**）/ 光照突变后 `⌈n/k⌉` 帧收敛（期间表是新旧混合，无
闪烁但滞后）/ 深度角分辨率 6×8² + nearest-texel 无双线性 / σ² 每纹素只有
`(G/8)²` 条射线 + 10% 下限钳 ⇒ **厚度 < σ 的薄墙仍可能漏**（chebyshev 是
概率上界不是精确可见性）/ 嵌入几何的探针不做 relocation（贡献被抑制但白占
预算）/ hysteresis 默认关 ⇒ 滚动推进时可能有轻微空间撕裂 / 跨适配器位级
不可保 / CPU 单线程内联重照明（未用 R73 分片并行）/ 与光照贴图·反射探针·SSR
的合成语义未修订（**R89**）/ `sky_color` 默认取 ambient 而 ambient 同时是
常量补光项的来源 ⇒ **天空底色双计**（配平：显式覆盖 `sky_color` 或调低
ambient）/ 无探针持久化 / **片元阶段只读 storage 缓冲占满 4/4** / 着色器全表
线性扫描（无逐探针空间加速结构）/ 探针不落实体（编辑器里看不见、不能逐个调）/
**网格覆盖范围外回退 env ⇒ 可见等值线**（`Σw ≤ ε` 兜底是 env 天空常量而半径
是有限球 ⇒ 验收图里天花板与墙交接处的扇贝形亮带就是这条等值线：探针层高 1.3
+ 半径 2.4 ⇒ 只够到 y = 3.7；修法是抬 `bounds` 盖住所有会被着色的表面）。

### Added（R87：虚拟几何体 V4——分页流送 + 遮挡整合，**连役 4/4 收官**）

路线图 v5 第三梯队 R84–R87 连役 4/4 收官。MVP 边界延续（无 visbuffer /
opaque-only / 无软件光栅回退）。本轮兑现两笔欠账：R84/R85 两次推迟的
**pack 集成**（最小读取入口）与路线图 §一裁定的**红线修订声明**（流式
红线收窄，全文见 ARCHITECTURE §4.43 末节）。

- **页划分（`iris_render::meshlet_stream::MeshletPageTable`，纯派生）**：
  固定簇数页，**定稿 16 簇/页**。页 p 的簇区间 = `[p×cpp,
  min((p+1)×cpp, 簇总数))`；顶点 / 索引字节区间由簇元数据的连续衔接性
  导出；逐簇所属层级组号 = 升序扫描组表时首个覆盖者（组范围互斥时唯一，
  重叠时最低组号赢——防御式定序）。`derive` ≡ 构建期写入（单测锁定）。
  **偏离声明**：规格草案「如 256 簇/页」→ 16——256 下典型中等资产只落
  1–8 页，叶层与粗层几乎必然同页，粗化回退与字节预算双双失去页粒度。
- **IRML v3**：头 40 B（+ 每页簇数）+ v2 全部区段 + 页区段（页数 +
  逐页顶点 / 索引区间 + 逐簇组号）。版本矩阵 = {1, 2, 3}：**v1 / v2
  文件照常读取**（页表缺席 ⇒ 流送激活时按同参数现算，逐位等价）；其余
  拒读。v3 读侧校验：页数 = `ceil(簇总数 / 每页簇数)`、逐页区间与簇
  元数据导出值逐字段核对、逐簇组号 ∈ 哨兵 ∪ 所指组自有簇范围、数据区
  在页区段后精确闭合。**偏离声明**：R86 时代「版本 3 必拒」断言随 bump
  调整（3 进兼容名单，[0, 4, 999] 仍拒）——同 R86 先例。
- **驻留状态机（`ResidencyManager`，CPU 纯逻辑）**：`tick(budget,
  pages_per_frame, requests, page_bytes)` → `ResidencyOps { evict,
  upload }`。①键合并（同帧同页多请求取最小误差，`min` 与顺序无关）
  ②预算收缩强制执行 ③按优先级准入（页数封顶 + 逐未请求页腾位，腾不出
  则延后）。**优先级 = 固定字典序全序**（页**紧迫度**的倒序位映射，
  页号升序收尾）⇒ 乱序请求同队列。**页紧迫度 = 页内「被请求区间覆盖的
  簇」的簇紧迫度最小值**；簇紧迫度 = 所属组的屏幕误差，**无所属组 ⇒
  `+∞`**（最紧急）。**驱逐**选未请求的最低优先级
  页；本帧请求页受保护（**无抖动**）。浮点键经位映射成全序 u64（NaN
  归 +∞、负值钳 0），全部迭代序来自 `BTreeMap` / `BTreeSet` 键序 ⇒
  同输入两跑裁决逐位一致。**偏离声明**：规格「最小屏幕误差升序」→
  实现取**倒序**（粗页优先）——按字面升序时叶页（误差恒 0）恒最先驻留、
  粗页恒被逐出，推演结果是「预算紧张 ⇒ 叶页挤占粗页 ⇒ 选中的叶组沿
  DAG 找不到驻留祖先 ⇒ 实例整帧消失」，与「小预算 ⇒ 粗层渲染、无洞」
  的验收硬标准矛盾；倒序让「页缺 ⇒ 父组回退」在任意预算下都有落点。
  **紧迫度口径细化（验收轮裁定修订，原稿「页内全部簇最小误差、无组簇
  按叶口径计 0」）**：①只统计**请求方覆盖的簇**——容器布局叶簇在前、
  父簇按层追加 ⇒ 叶 / 父边界那一页通常同时含两类簇，远景粗化时请求方
  只要父簇，按页内全部簇取 min 会被同页叶簇拖到最低优先级、粗化回退
  在该页失去落点；②无组簇取 `+∞`——原口径把「无回退」误当「叶簇
  （有回退）」，使单层网格资产在预算紧张时最先饿死（缺页即整块消失，
  本应最紧急）。两条**只改准入 / 驱逐顺序、不改任何像素**（快照 FNV
  逐位不变，全部既有 GPU 断言原值通过）。
- **驻留感知 LOD 选择**（`select_lod_cluster_ranges_resident`）：遍历与
  阈值判定与 R86 逐式同构（**全驻留时输出逐位一致**），选中组页未全
  驻留 ⇒ **沿 DAG 上取第一个区间全驻留的祖先**；链上无驻留祖先（含根）
  ⇒ `skipped`、实例整帧跳过。双子组回退同一父组只产出一条区间。输出
  **理想集请求页** = 理想集 ∪ 可绘区间 ∪ **回退链沿途祖先页**——末项
  是防自锁的必需品（理想集只含叶页、预算只容粗页时粗页从未被请求 ⇒
  回退永不成功 ⇒ 实例永不出现）。
- **渲染器接线**：`MeshletStreamSettings { gpu_byte_budget, pages_per_frame }`
  资源——**缺席 / 预算 `None` = 无预算 = 全驻留 = R86 行为逐位不变**
  （页表仍随注册构建，不门控、不逐页上传，arena 全量上传照旧）；有预算
  ⇒ 页按优先级分帧上传（tick 消费上一帧请求，一帧延迟同 R85 紧凑回读
  节拍）+ 驻留感知选择门控绘制。注册期流送激活时跳过整库上传；**驱逐
  = 页数据清零**；**迁移重传**（流送 → 非流送时驻留页可能已被逐出，
  回非流送路径前整库重传）；**重建补传**（运行期新增资产把顶点 / 索引
  容量推过当前容量时 arena 缓冲被重建，新缓冲零初始化而驻留状态机仍
  标记旧页驻留——`tick` 只对未驻留页组队，「后续请求自然补传」不成立
  ⇒ 必须按驻留集全量补传，否则是「静默消失」：簇元数据在定长缓冲里
  完好 ⇒ cull 照报存活、顶点 / 索引全零 ⇒ 零面积三角、画面空而统计
  不空；该缺陷在**验收轮亲审 diff 时发现**，GPU 测试
  `stream_survives_arena_rebuild_on_late_registration` 锁定——去掉补传
  该测试即失败）。**页槽 = arena 规范区间**（不引入 packed
  页池 + 页间重定向）⇒ `meshlet_cull.wgsl` / `meshlet_draw.wgsl`
  **零改动**（R84–R86 兼容硬约束的直接推论）；arena 按全量容量预留 =
  v1 的 VRAM 诚实成本（§8.56①）。
- **遮挡整合（R53 两阶段，实体级 v1）**：`MeshletInstance` 的实例 AABB
  （簇包围球联合盒，注册期预计算，过模型矩阵取保守世界盒——与 R53
  `world_aabb` 同式）追加在普通实例查询条目之后（收集序 = 查询序），
  被遮挡实体在**收集期**整帧跳过（不进实例表 = 不进 cull dispatch
  输入）。AABB 条目**先于任何跳过路径**产生（持续重查是恢复可见的唯一
  路径）；**与流送正交**——未驻留实例在选择期已跳过，遮挡路径不双重
  处理；遮挡隐藏者不产生流送请求（不为看不见的物体预热页）。**逐簇
  GPU 遮挡（Hi-Z 类）不做**（§8.56③）。
- **pack 集成兑现**：`iris_engine::pack::read_meshlet`（一个函数 +
  往返测试）——`PackReader::read`（seek + read_exact + CRC）+ `MeshletMesh::from_bytes`
  （版本矩阵 {1, 2, 3}）；错误分档 `MeshletPackError { Pack / NotIrml
  / Irml }`，魔数为准（扩展名只是约定）。**编辑器 / AssetMount 全接线
  不做**（§8.56④）。
- **流式红线收窄声明**（路线图 v5 §一裁定，本轮兑现）：原「不做大开放
  世界流式」→ **地块流式仍不做**（§4.23 / §8.31 的地形限制原样保留）；
  **资产级 meshlet 分页流送（GPU 显存分页）允许且已落地**。仍不做：
  visbuffer 全量管线、软件光栅回退、Lumen 类全动态 GI、网络多人、
  通用 DCC 级编辑器。全文见 ARCHITECTURE §4.43 末节。
- **测试**：CPU +21（`meshlet_stream` 18：页划分守恒（簇 / 顶点 / 索引
  恰一次 + 字节守恒 + 组号归属）/ 末页短页 / `derive` ≡ `build` / 全
  驻留 ≡ R86 三档阈值 / 父组回退精确性（解析解）/ 双子组回退去重 / 根
  未驻留 ⇒ 整帧跳过（理想请求仍完整）/ 单层网格整块门控 / 页请求键只计
  请求方覆盖的簇（解析解 = 根误差 3.999，断言严格大于同页未请求的 B 的
  2.0）/ 无组簇紧迫度 `+∞` ⇒ 倒序键最小（严格优先于有限误差页）/
  乱序请求同队列 + 上传序 = 优先级序 / 字节
  + 页数双重预算 / 驱逐最低优先级 + 请求页保护 / 收敛零操作 / 同页多
  请求取最小误差 / 预算收缩先逐未请求 + 全保护接受超预算 / 位键全序 /
  越界页防御；`meshlet` +2：IRML v3 往返位级（总长逐字节 + 单层叶数
  归一 + 二次写出幂等）+ v3 页区段四连语义校验；`iris-engine::pack`
  +1：`.imesh` 往返 + 错误分档）；GPU +5（`tests/meshlet_stream_render.rs`，
  `GPU_SERIAL`）：**全驻留等价**（流送缺席 / 预算 `None` vs R85 全叶
  路径逐字节 + `surviving` = 叶簇总数）/ **预算流送**（粗层 ⇒ 像素哈希
  变化 + 选中簇数下降 + 画面非空（回退链请求保证粗页流入）；预算放大
  ⇒ 选中簇数单调；预算 = 全量 ⇒ 收敛帧与 R85 全叶路径逐字节）/ **遮挡
  整合**（柱后实体收敛帧整帧跳过、像素与实体缺席帧逐字节相等；前提
  校验 = 无遮挡配置的在缺席帧亦逐字节相等；帧 1 保守全可见；稳态逐位
  一致；柱侧可见实体恒不被剔且像素与无遮挡帧逐字节相等）/ **重建补传**
  （运行期新增资产触发 arena 重建后像素与重建前逐字节相等）/
  **确定性**（同预算双会话逐字节）。既有 R84/R85/R86 测试（含 GPU 14 项）零改动
  全过。快照 `examples/meshlet_stream_snapshot.rs` → `docs/meshlet_stream.png`
  （1×3：①全驻留 | ②低预算粗层 | ③遮挡整合；①断言与 R85 全叶路径逐
  字节相等，②选中 15 簇 < ① 的 47 且物体不消失，③柱侧可见球 + 柱后
  被剔球构成可判别对照；目检：粗层轮廓完整无洞、遮挡正确无残留、无
  闪烁断层）。诚实限制 §8.56。

### Added（R86：虚拟几何体 V3——层级 LOD：簇 DAG + 屏幕空间误差选择）

路线图 v5 第三梯队 R84–R87 连役 3/4。「单 LOD」边界解除：层级进容器
与 CPU 选择，R85 的 GPU cull / compact / draw 管线**零改动**（只换输
入集合）。MVP 边界延续（无 visbuffer / opaque-only；遮挡留 R87）。

- **简化器 = 体素网格顶点聚类（Rossignac）**（`iris_engine::meshlet_builder`::
  `simplify_by_vertex_clustering`，纯函数）：选型裁定——QEM 边坍缩质
  量更高但堆序 / 半边结构的确定性成本过高；顶点聚类对三角汤天然友好、
  简化 = 空间网格函数 ⇒ **逐位确定免费**（质量折衷诚实入档 §8.55①）。
  可合并格收敛首见代表并吸附格心、单顶点格原位保留、**边界顶点永不
  合并**（组间接缝防线的根据）、塌缩 / 零面积剔除 + 规范序去重；误差
  = cell × √3/2（格对角半长 = 位移上界），未合并诚实归零。
- **层级构建（自底向上簇 DAG）**（`build_meshlet_hierarchy[_with_config]`
  / `build_meshlet_hierarchy_from_mesh_data`）：Level 0 = R84 叶簇（每
  簇一组）；每轮连续贪心分块（≤ 8 组一块、尾块借用、块恒 ≥ 2）→ 块内
  几何按位去重合并 → 简化（cell = 叶簇包围球半径均值 × 2^level，三角
  未达一半时 ≤ 4 次倍增）→ **复用 R84 聚类**产出父组自有簇（叶簇在
  前、父簇按层追加 ⇒ 组范围连续 + 子组号 < 父组号）→ group_error =
  max(own, children) 沿 DAG 单调。停止：单组 / 三角 < 128 / 层数上限
  8。**块间共享顶点 = 边界集**（contested 判定）锁定。收尾按整网格包
  围盒统一归一全部簇 / 组 bounds（渲染器注册口径）。
- **IRML v2**：头 36 B（+ 叶簇数 / 组数 / 根数）+ 组区段（children /
  簇范围 / 误差 / bounds / cone）+ 根区段；CRC 覆盖整个数据区。版本
  矩阵 {1, 2}：**v1 文件照常读取**（层级空 = 全叶；单层 `write_to`
  保持 v1 字节布局 = R84 零漂移——`--no-lod` 单层写出语义天然落回），
  其余版本拒读。v2 读侧校验：叶数 / 子组号 < 父号 / 簇范围 / 根越界 /
  区段长度闭合。**偏离声明**：R84 时代「版本 2 必拒」断言随 bump 调整
  （2 进兼容名单，[0, 3, 999] 仍拒）——「bump v2」与「v1 照常读取」
  两规格共同要求下的必然后果。
- **逐帧选择（CPU 纯函数）**（`select_lod_cluster_ranges_in` /
  `select_lod_clusters` + `MeshletLodSettings { threshold_px, enabled }`
  资源）：组屏幕误差 = `group_error × error_scale × (screen_h × 0.5) /
  (dist × tan_half_fov)` ≤ 阈值 ⇒ 选中本组簇范围、停止下钻；叶组恒选
  自身 ⇒ threshold 0 恒全叶。选择是逐组布尔 ⇒ 输出集合与遍历无关；
  输出 = 选中簇区间升序 + 相邻合并（= 绘制紧凑序）。**资源缺席 /
  enabled=false = 全叶表 = R85 行为逐位不变**。
- **渲染器接线（只换输入集合）**：选中区间 = 帧实例表 **run 形条目**
  （簇范围 = run 起点 + 长度；区间在 arena 连续 ⇒ cull / compact /
  draw / 双缓冲 / 读回协议零改动）；紧凑序 = 选中集升序（绘制序确定）。
  容量退化确定性：run / 簇超预算 ⇒ 实例级退化全叶（像素正确）→ 全叶
  也超 ⇒ 跳过实例 + warn。三角形统计经 run 起点表还原网格局部簇号。
- **测试**：CPU +21（`meshlet` +9：选择解析解三档阈值 + 恰等边界（≤
  语义）/ 距离单调 / 非对称区间升序 / threshold 0 全叶 / 极大阈值选中
  根 / 空层级恒等表 / IRML v2 往返位级 + v1 兼容读取 + v2 语义校验四
  连；`meshlet_builder` +12：简化器两跑逐位 / 减面 + 误差公式 + 无合
  并归零 / 边界锁定位级原样 + 格心吸附 / 规范序去重 / should_panic ×2
  / 层级两跑逐位 / DAG 良构 + 误差单调 + 叶覆盖 / 层数上限两档 / 构建
  × 选择联动（threshold 0 全叶 + 大阈值根）/ 空输入 / 分块形状）；
  GPU +3（`tests/meshlet_lod_render.rs`，
  `GPU_SERIAL`）：**τ→全叶等价**（threshold 0 层级 mesh vs R85 全叶路
  径逐字节——本轮最强断言）/ 距离单调（近 54 > 中 18 > 远 8 簇 + 物
  体不消失 + 像素流变化）/ 双会话逐字节 + 阈值变化换哈希。既有
  R84/R85 测试（含 GPU 10 项）零改动全过。快照
  `examples/meshlet_lod_snapshot.rs` → `docs/meshlet_lod.png`（1×3：近
  叶簇细 | 中 L1 过渡 | 远根簇粗；选中数标注于 example 输出；跨运行
  逐位 + 目检：远处无裂缝碎面、近处细节不丢、无消失闪面）。诚实限制
  §8.55。

### Added（R85：虚拟几何体 V2——compute 簇剔除 + 前缀和压缩绘制）

路线图 v5 第三梯队 R84–R87 连役 2/4。路线图裁定：wgpu 无 mesh shader、
WebGPU 无 multi_draw_indirect ⇒ **前缀和压缩绘制**（wasm 兼容）。MVP
边界延续 R84（无 visbuffer / opaque-only / 单 LOD / 遮挡留 R87）。

- **compute 剔除（`shaders/meshlet_cull.wgsl`，两 entry / 两管线）**：
  ① `main_cull` 一线程一（实例, 簇）对——包围球视锥判定（Gribb-Hartmann
  6 平面提取，`frustum.rs` 同式；归一化球心反归一化 → 模型变换 → 半径
  最大列缩放）+ 背面锥判定；② `main_compact` 单工作组（256 线程 × 每
  线程顺序扫 64 flag）两遍扫描——u32 前缀和（**整数加法结合精确 ⇒ 扫描
  树形状无关的确定性**，与浮点扫描的本质区别）+ 按前缀槽位写紧凑列表，
  **无原子**，紧凑序 = 全局簇号升序（stable compaction ⇒ 绘制序确定）。
  谓词锚点 = `meshlet.rs` 纯函数（`cluster_is_backfacing` /
  `chunked_prefix_compact` 逐式镜像单测）。**背面锥谓词定稿修正**：R84
  头注释「点积 < cutoff 即背面」是松散表述（近平坦簇会误剔正面），R85
  以精确推导为准——全背 ⇔ 视线角 ≥ 90° + acos(cutoff) ⇔
  `dot ≤ −sin(cutoff)`（严格小于才剔、退化全向锥永不剔）。
- **vertex-pull 绘制（`shaders/meshlet_draw.wgsl`）**：无顶点 / 索引
  缓冲的 instanced draw——`instance_index → 紧凑簇列表 → 簇元数据
  （12×u32 词 storage，48 B `MeshletClusterRaw` 逐字排布）→ storage 拉
  顶点 / 索引`。**片元阶段 = standard.wgsl 的 fs_main 本体**（wgpu 跨
  模块管线阶段）——「PBR 提取共享 vs 逐式复制」以零复制消解，与常规
  路径同一材质绑定组语义；间接光只吃 irradiance_map（探针 / 光照贴图
  实例属性路径不在 vertex-pull 管线，§8.54③）。顶点侧与 vs_main 逐式
  同构（同 `InstanceRaw::from_matrix` 值源）⇒ **跨管线像素等价可达**。
- **组件与接线**：`MeshletInstance` 组件 + `MeshletMeshStore` 目录 +
  `MeshletMeshHandle` 句柄（MeshStore 同构注册表）；渲染器侧追加式
  arena（簇 / 顶点 / 索引，变更时上传、超容量 warn 跳过）、实例表双
  缓冲、剔除参数 uniform、紧凑列表 / 载荷 / 标志 storage。
  `MAX_MESHLET_CLUSTERS = 16384` 三处同源（IRML 读取构建期拒绝 + 注册
  处校验 warn + GPU 缓冲定长）。缺席语义：无 `MeshletInstance` = 零
  dispatch、零回读、零绘制（既有像素与命令流逐位不变）。绘制在主 pass
  内不透明段之后；compute pass 编码在主 pass 之后（本帧 draw 消费上帧
  列表、本帧 compute 覆写在后——同编码器按序）。
- **回读机制**：staging 双缓冲 + map_async（遮挡 R53 同款协议），本帧
  读上帧结果（**一帧延迟，§8.54①**）；消费端要求「产出帧 == 恰好上一
  帧」——首帧 / 迟到 / 映射失败 / 跳帧一律降级**恒等列表全量绘制**
  （剔除只删不可见几何 ⇒ 兜底帧与剔除帧像素恒等，无视觉跳变；定稿不
  走 expand 常规管线兜底——避免第二着色路径；expand 保留为对照锚点）。
  `FrameStats.meshlet_surviving` = 本帧提交的紧凑实例数（剔除有效性
  可观测）。
- **测试**：CPU +7（`meshlet` 20 项：+48 B 词布局锚点 / 视锥平面提取与
  球判定解析解（内 / 切 / 外三态）/ 背面锥符号约定（正对存活 / 背对
  剔除 / 阈值带 / 退化全向 / 相等点存活）/ 紧凑化扫描镜像（朴素过滤
  对照 + splitmix64 伪随机批量）/ 顶点词重排位级往返 / mesh_bounds /
  IRML 簇上限拒绝）；pipeline +1（meshlet 三重接口看门：Globals 前缀 /
  VertexOutput 逐字段 / InstanceEntry 双侧同序）；GPU 6 项
  (`tests/meshlet_v2_render.rs`，`GPU_SERIAL`)：三机位（正面 / 侧后 /
  远距）全量帧 vs 剔除帧逐字节相等（本轮最强断言）/ 紧凑列表严格升序 /
  surviving 口径 / 缺席语义（有 vs 无 FNV 不同 + 空场景双帧双渲染器
  逐位）/ 双会话逐字节 / 旋转一帧哈希变化。快照
  `examples/meshlet_v2_snapshot.rs` → `docs/meshlet_v2.png`（1×2：expand
  常规路径 | compute 剔除路径；**全帧 FNV 相等**（跨管线像素等价实锤）
  + 反空洞 + 跨运行逐位 + 目检）。
- **诚实限制（§8.54）**：回读一帧延迟（快速移动单帧缺簇 / 跨世界切换
  一帧幽灵）/ meshlet 路径间接光只吃 irradiance_map / 单材质（默认
  句柄 0）/ 不投影阴影不进 scene_depth / 固定顶点数展开（小簇空转）/
  非均匀缩放锥保守性弱化 / 16384 簇上限 / 跨适配器剔除边界位翻转可能。

### Added（R84：虚拟几何体 V1——簇构建 + 资产格式）

路线图 v5 第三梯队 R84–R87 连役 1/4。全案 MVP 边界：**无 visbuffer、
opaque-only、无软件光栅回退**；V1 = 运行期表示 + 确定性聚类构建器 +
IRML 资产容器 + 渲染等价性锚点，**渲染器绘制路径零改动**（compute 剔除
是 R85 的事，本轮不产生任何剔除收益——元数据就位待消费）。

- **运行期表示（`iris_render::meshlet`）**：`MeshletMesh`（全局顶点 +
  全局索引 + 逐簇元数据）与 `MeshletClusterRaw`（48 B POD，布局为 R85
  GPU storage 前置；`reserved: [u8; 2]` 显式占位消除隐式填充满足
  bytemuck::Pod）。逐簇元数据：`vertex_offset` / `index_offset`（全局
  缓冲偏移，簇按构建序连续衔接，读侧校验）、边界锥（面法线均值归一 +
  `cutoff = min(axis·n_i)`；退化 ⇒ 全向锥 [0,0,1] / −1 永不剔除，保守
  方向）、包围球（xyz 球心相对网格包围盒归一、w 半径世界单位；均值 +
  最大半径——单遍 / 必然包含 / 逐位确定）、`error` 恒 0.0（R86 简化器
  启用）。诚实口径：预算常量 256 顶点被 u8 字段物理容量收窄到 255，
  构建器按 255 卡。
- **确定性贪心聚类（`iris_engine::meshlet_builder`）**：顶点 → 三角形
  邻接表上的种子生长——种子 = 未分配三角索引序最小者；候选集
  `BTreeSet`（升序迭代，确定性硬文化选型）；评分 = 固定字典序全序
  （共享已用顶点数降序、三角索引升序收尾）；满 128 三角或无可生长候选
  即封簇。簇内顶点局部重映射（发现序编号），簇间边界顶点不去重（总量
  换逐簇自包含）。**输入即索引序**（乱序输入不适用种子序保证），同输入
  两跑输出逐位一致（to_bits 断言）。元数据走 `meshlet` 模块无状态纯
  函数（`cluster_bounding_sphere` / `cluster_cone` /
  `cluster_bounds_normalized`，R85/R87 共用）。`is_meshlet_eligible`
  判定 V1 网格资格（无蒙皮 / 切线 / UV2）。
- **IRML 资产容器（v1）**：`"IRML"` + version + 三区段计数 + CRC-32
  （与 `pack::crc32` 同一定义；跨 crate 复制而非共享依赖——反向依赖会
  成环）+ 数据区（顶点 | 索引 | 簇，全 LE 逐字段编解码）。校验链：魔数
  → 精确版本 → 长度精确匹配 → CRC → 簇偏移连续性 / 区间 / 簇内局部
  索引 / 保留位。API 与 Pack 同构（`write_to` / `write_file` /
  `from_bytes` / `open`）。
- **展开 API 与渲染等价性锚点**：`MeshletMesh::expand` 纯函数（逐簇
  局部号 + vertex_offset 还原全局索引）。正确性论证：展开 = 顶点重排 +
  索引重排，三角形集合（含环绕）与逐角属性值与输入完全一致 ⇒ 不透明
  几何渲染**逐字节一致**（场景无共面重叠面为前提；失败即真 bug，无
  阈值豁免）。GPU 佐证 `tests/meshlet_render.rs` 4 项（`GPU_SERIAL`；
  两台独立渲染器分服务常规 / meshlet 世界——句柄值缓存跨世界串味）：
  两机位逐字节 / 反空洞（两角度 FNV 必不同）/ 重渲逐字节。
- **偏离声明**：GltfBuilder 侧导出入口 or 独立 example 二选一，定稿
  **独立 example** `meshlet_build_example`（GltfBuilder 现场生成测试
  GLB → `spawn_gltf_instance` 真实解析 → 逐网格簇化 → `.imesh` 写出 →
  位级往返 + 三角守恒 + 簇上限断言；球 2976 三角 → 24 簇均 124 三角）。
  pack 集成留 R87 分页流送。仓内既有内容资产全部带蒙皮或 TANGENT
  （assault_courtyard 为法线贴图写切线），V1 按边界拒收——example
  自产测试 GLB 的原因。
- **测试**：CPU 26 项（`meshlet` 13 + `meshlet_builder` 13：48 B 布局
  契约 / 包围球与锥解析解 / 归一化映射 / 容器往返位级 / 魔数版本 CRC
  截断与语义损坏拒绝 / 展开守恒 / 两跑逐位 / 覆盖恰一次 / 连通性 BFS /
  锥朝外 / 立方逐面解析解 / 资格判定 / should_panic×2）；GPU 4 项；
  快照 `examples/meshlet_snapshot.rs` → `docs/meshlet.png`（2×2：常规 |
  meshlet × 两机位，同机位 FNV 相等、跨机位不等、跨运行逐位）。
  诚实限制 §8.53。

### Added（R83：GI 探针逐片元化——三阶 SH 方向性探针 × 片端径向权重求值）

路线图 v5 第二梯队收官轮（5/5）。`iris-render/irradiance.rs` 新增三阶球谐
纯函数段，`standard.wgsl` 新增逐片元求值支：把 ARCHITECTURE §8.40 的
**①（逐片元 GPU 插值）与②（SH 方向性探针）从拒绝清单移进实现**，③（可见性
加权）维持拒绝。v1（§4.33）的数据与路径**一字不动**，继续作 mode = 0 的
兼容轨与对照轨；SH 是**追加**的一层方向性数据。

- **三阶 SH 烘焙（`probe_sh_coefficients`）**：与 v1 **同一份烘焙口径**
  （同一 BVH / 同一 `BakeParams` / 同一「天空 or 一次反弹」辐射率公式
  `probe_sh_radiance`——不新增捕获 pass），把「六轴各积一次半球」换成
  「六面 × 32² 方向各取一次辐射率再投影」；投影 = 固定面序（`PROBE_AXES`）
  × 固定行列序 × **精确固体角权重**（`cube_texel_solid_angle` 闭式，六面
  合计 4π 钉到 f64 的 1e-12）；累加 f64、基函数 f32（单一事实源）。
- **Lambert 带通卷积 + 除 π（`sh_lambert_convolve`）**：clamped-cosine 的
  SH 只有三个非零频带 `A = (π, 2π/3, π/4)`，逐带乘 `A/π = (1, 2/3, 1/4)`
  ⇒ 存「**E/π 直读**」系数，着色器求值退化为 **9 项纯点积每通道**，与
  `irradiance_map` / loc14 同口径。解析锚点：常量辐射率 C ⇒ 任意方向求值
  恒 = C；纯 L1 场 ⇒ `E(+Y)/π = 2C/3`（把 `A_1/π = 2/3` 钉死）。
- **GPU 接线（R78 模式复刻）**：binding 18 = 探针 read-only storage
  （256 × `ShProbeRaw` 160 B），binding 19 = 16 B 参数 uniform
  （`x = mode`、`y = 探针数`）；**Globals 一字节不动、顶点布局不动
  （loc14 保留）**、skybox.wgsl 并行布局不受影响。哑资源（160 B / 16 B
  全零）恒在 ⇒ 缺席 / 无 SH / 捕获 / RT 帧 mode = 0 分支死码、**零
  write_buffer**，命令流与 R82 逐位一致。
- **mode 均匀门**：mode = 0 ⇒ R64 的 loc14 逐实体支**逐位不变**；mode = 1
  ⇒ GI 探针项由逐片元 SH 求值替代（`gi_sh_irradiance`）。两支都只替换
  `in.probe.w > 0.5` 的实体 ⇒ **天空兜底与 R52c 光照贴图 `select` 混合语义
  零触碰**（正交）。裁决 = 探针集是否带 SH（`has_sh()`），「烘了就用」，
  无独立开关资源。
- **逐片元混合**：径向权重 `w = (1 − (d/r)²)²`（与点光衰减窗同式同源），
  `E/π = Σ(w·SH(n)) / max(Σw, ε)`；`Σw ≤ ε` ⇒ 回退**最近探针**（球边界
  处连续，延续 v1「包外无衰减」语义）；固定循环序 = 固定累加序 ⇒ 会话内
  逐位确定；出口钳 ≥ 0（L2 振铃兜底，**无去振铃窗**）。
- **影响半径**：自动 = **最近邻 × 1.5**（`total_cmp` 全序 ⇒ 输入重排不变；
  单探针 = +∞、重合 = 0）；`IrradianceProbeRadius` 组件覆盖（0 = 自动）。
  **偏离声明**：规格写「`IrradianceProbe` 加可选覆盖字段」——单元结构体加
  字段会撞「既有测试零改动」硬约束 ⇒ 改为独立可选组件（缺席语义等价）。
- **CPU/WGSL 同步契约**：`SH_Y0/Y1/Y2A/Y2B/Y2C` 常数表与 `sh_basis` /
  `probe_influence_weight` / `sh_blend` 逐式镜像到 WGSL；常数表由源码层
  看门单测逐位比对（`skybox_globals_prefix_matches_standard_layout` 先例）。
- **兼容硬约束**：`bake_irradiance_probes` 签名与行为一字不动（无 SH ⇒
  mode = 0）；新增 `bake_irradiance_probes_sh`；`install_irradiance_probes`
  不变（报告多 `sh` 字段）；编辑器零改动。既有 FNV 基线（含
  `irradiance_render` 6 项）全部原值通过。
- **测试**：CPU 单测 16 项（解析锚点 / 投影两跑逐位 / 半径全序与置换不变 /
  权重窗与点光窗逐位同值 / 加权混合与回退与边界连续 / v1 数据逐位不变 +
  与六轴平均互证 / WGSL 常数看门）；GPU 5 项（`GPU_SERIAL`：缺席 = 空集 =
  退化集逐字节 / **空间梯度**（4 m 高柱横跨红区绿区，SH 柱脚偏红柱头偏绿
  中段单调、v1 上下同色、跨模式作差 ±50–70 级）/ **方向性**（同一探针内
  球顶朝亮面 vs 球底朝暗面 +77/−173 级、常量场对照逐位同源、v1 差 1 级）/
  真实烘焙贯通（天窗房间，GPU 与 CPU 同侧互证）/ 双会话逐字节 + 改单探针
  变哈希）；快照 `examples/gi_sh_snapshot.rs` → `docs/gi_sh.png`（2×2：
  空间梯度 v1 | SH、方向性 v1 | SH；**每世界独立渲染器**——句柄缓存按值
  复用，跨世界复用会串味）。

### Added（R82：粒子 v2——GPU compute 模拟 × 相机面向拖尾条带）

路线图 v5 第二梯队 4/5。`iris-render` 新增 `particles_v2` 模块与
`shaders/particles_v2_sim.wgsl`（**全仓首个 compute 管线**）+
`shaders/particles_v2_draw.wgsl`：实体挂 **`ParticleEmitterV2` 组件**
即得 GPU 模拟的粒子 + 拖尾条带，**无本组件 = 零计算零绘制**（无
`write_buffer`、无 dispatch、无 draw、不分配 GPU 池——像素与命令流与
引入前逐位一致）。**v1（`particles.rs`）一字不动**，继续作对照轨与
wasm 回退；v2 全新增，二者并存互不感知。

- **固定池 + 恒定 dispatch（会话内逐位确定性的根基）**：池容量恒为
  `PARTICLES_V2_MAX` = 4096，dispatch = 池 / 工作组常量算出
  （`PARTICLES_V2_DISPATCH` = 64，const 断言锁定整除，不留尾组），
  **不随存活数变**；每线程只读写自己索引的粒子（无原子无竞争），
  状态（位置 / 速度 / 年龄 / 存活 + 32 点环形历史）存 read-write
  storage，步长 560 B（与 WGSL `SimParticle` 影子契约，const 断言
  看门）。同适配器同会话内双会话逐帧**逐位一致**；**跨适配器位级
  不可保**（GPU 编译器 fma / 重结合自由——§8.51）。
- **CPU/GPU 镜像契约（四处同步：`particles.rs` / `wind.rs` /
  `particles_v2.rs` / WGSL）**：①发射节律 CPU 判定（`spawn_budget`
  累计分数法，v1 逐式独立实现 + 单测），每步上传 `SimParamsRaw`
  （96 B）——GPU 只消费；②出生哈希 = mix64(绝对池索引, seed)，WGSL
  无 u64 算术故以 u32 肢对（`mulhi32` 16 位裂项 schoolbook）逐式
  复刻（整数精确 ⇒ 跨适配器恒等，单测与原生 u64 逐位对照）；
  ③半隐式欧拉 + 风加速度（`euler_step` 镜像式），age ≥ 寿命 → 存活
  = false，**死粒子不回收索引**（固定池顺序发射，哈希输入恒定，池满
  永久停发）；④拖尾环写指针推进（`ring_advance` 镜像）。
- **Wind 消费（R80 `Wind` 接口的首个引擎内消费者）**：CPU 每帧在
  发射器原点单点采样 `wind_sample` 写入 uniform，GPU 侧当常值加速度
  与重力合并进欧拉步（逐式镜像；逐粒子空间变化留后续）。`Wind` 资源
  缺席 = 无风（零向量）；CPU 镜像值与 `wind_sample` **恒等**（直接
  调用，零漂移，单测锁定）。
- **拖尾渲染（相机面向条带 + 头部 billboard，固定顶点数）**：每存活
  粒子的历史环相邻两点生成一节屏幕空间展宽四边形，顶点数恒为
  池 × trail_length × 6（顶点全由 `vertex_index` 派生、数据从 storage
  读——无顶点缓冲、无实例缓冲）。头部 = 位置 + 寿命插值尺寸 + 径向
  软衰减（v1 `collect_render` / `particle.wgsl` 同式）；条带节 =
  两端点投影求屏幕空间法向、按各自 1/w 展宽 ⇒ **恒定世界宽度**，
  横向软边 (1−v²)² 且相邻节共享端点 ⇒ 关节透明连续。颜色：头部 =
  `color_at(当前 age)`，条带顶点 = `color_at(记录时 age)`（沿历史按
  age 比例渐变，与头部关节连续）× alpha 沿历史线性淡出（尾部全透明，
  平滑收束）。死粒子 / 历史不足 / 端点在身后 / 零长度节 → 全顶点
  画面外同点 = **零面积退化**（不产生片元，按索引确定性消隐）。渲染
  进 v1 粒子 pass 位置（透明之后、泛光之前），HDR 目标 alpha 混合，
  深度只测不写、cull None；**不写 scene_depth**（v1 / 贴花同纪律）。
- **帧队列与池生命周期**：每步入队一份 `SimParamsRaw`（零发射帧同样
  入队——积分与拖尾照常推进），渲染器每帧排水一次按序 dispatch（同
  状态重复渲染 = 空队列 = 不重复步进）；队列上限 1024，超限丢最旧
  （headless 长跑防 OOM）。GPU 池按实体绑定（世代位含于 `to_bits`）、
  销毁不回收；`reset` 清 CPU 状态 + 渲染器检测游标回退后全量零上传。
- **测试**：CPU 单测 13 项（肢对 mix64 与原生 u64 逐位一致 / fraction
  值域精度 / 出生方向语义 / 发射节律 / 欧拉步匀速直线解析对照 ±1e-5 /
  拖尾环回绕 / 记账与帧队列 / 队列上限丢最旧 / 风镜像恒等 / 帧队列
  逐位确定性 / reset / 命令式 burst / 钳制）+ GPU 集成
  `tests/particle_v2_render.rs` 4 项（`GPU_SERIAL` 串行化）：v1 零改动
  与缺席语义、v2 生效与推进、拖尾（发光像素显著更多 + 三角数恰
  trail_length 倍）、双会话逐位 + 换种子帧哈希变化。**度量学**：
  本场景天空恒亮（sRGB R ≈ 208）且 additive 经 ACES 压缩（条带峰值
  贡献仅 12–30 级，阈值 30 时新增像素为 0）——判据一律用与空场景 /
  仅头部帧的**逐像素差分**，阈值按条带解析面积给依据。快照
  `examples/particle_v2_snapshot.rs` → `docs/particles_v2.png`（1×2：
  拖尾关 | 开，旋转喷射 + 暗色地面 + 背光太阳；发光像素 / 新增像素 /
  几何增量 / 双会话逐字节断言全带，目检拖尾连续性 / 渐隐 / 无断层）。
- **本轮实测记录的两个陷阱**（写进测试模块头免得重犯）：①换种子的
  用例必须让**占位实体先于发射器**生成（先前的用例把占位 spawn 在
  发射器之后 ⇒ 实体 bits 不变 ⇒ 帧哈希不变）；②`velocity_jitter = 0`
  时整条出生哈希流被乘零（方向恒 +Y）⇒ 换种子对画面零影响，判别
  种子的场景必须带抖动。

### Added（R81：平面水体 v1——Gerstner 波 × 平面反射/折射捕获 × 深度吸收合成）

路线图 v5 第二梯队 3/5（环境底座）。`iris-render` 新增 `water` 模块
与 `shaders/water.wgsl`：实体挂 **`WaterSurface` 组件**即得一块世界
空间水平水体（Gerstner 波面 + 镜面反射 + 折射深度吸收 + 太阳镜面），
**无水体实体 = 无捕获 pass、无额外 write_buffer、像素与命令流与
引入前逐位一致**（零开销默认路径，全部既有 FNV 基线原值通过）。

- **组件与选择**（`water.rs`）：`WaterSurface { height, half_extent,
  wave_amplitude, wave_length, wave_speed, choppiness, shallow_color,
  deep_color, absorb_depth }`（Default 0 / 16² / 0.12 / 6.0 / 1.2 /
  0.6 / 浅青 / 深蓝 / 2.5）；多实体按**实体位模式升序**选择键
  `select_water_surface` 取首盏（min 的交换律 ⇒ 结果与查询 / 插入
  顺序无关，与 `select_point_shadow_casters` 同哲学），其余
  `log::warn` 忽略。
- **Gerstner 波纯函数 + WGSL 镜像契约**：4 波固定方向（45° 起步
  等角对角分布）、固定波序求和——振幅 `[1, 0.5, 0.35, 0.25]` 衰减
  （Σ = 2.1 = 波形有界上界，const 断言锁定）、波长 `[1, 0.55, 0.35,
  0.25]` 缩短；ω = k·speed·√(λ_i/λ_0)（深水色散锚定主波相速度）；
  x/z choppy 位移（陡度均分 Q_i = Q/(k·A·4)，Q ≤ 1 ⇒ 法线 y 分量
  恒正不卷绕）+ 解析偏导法线（非差分）；同输入 to_bits 逐位一致。
  **波形时间 = 世界 `Time` 资源 `elapsed_secs`**（缺席 = t = 0 静止
  ——渲染器无内部时钟，测试可精确驱动）。CPU 函数与
  `water.wgsl` 同式同序（fog.rs 同款镜像契约；CPU↔GPU 不做位级等价
  声明）。水面网格 = 归一化 [−1,1]² 96² quad 静态生成（纯函数
  `water_grid_mesh` + 良构 / 环绕 / 确定性单测），顶点着色器乘
  half_extent 展开。
- **反射 / 折射捕获**（主 pass 前，每帧）：反射 = **镜像相机**
  （view × 关于 y = height 的反射矩阵；抖动与主相机同款作用于纯投
  影）渲半分辨率 **RGBA16Float**（低频近似内容的裁定），折射 = 主
  相机视角同格式。捕获内容 = 不透明几何 + 天空（探针捕获同款最低
  配组装，SSR / AO / 分箱 / 探针走哑资源），**不含水体自身（无递
  归）**、不含透明 / 贴花 / 粒子。镜像矩阵行列式为负 ⇒ 环绕反转由
  **cull None 捕获管线吸收**（R62a 教训的「cull None 吸收」分支，
  单面 / 双面 / 镜像 / 主相机共用两条管线）；反射捕获按镜像视锥
  独立收集，折射捕获复用主 pass 实例区间（零额外上传）。**内部
  捕获不占用户 RT 相机预算 2**（§4.21 / §8.20 修订：多视口编排
  是用户能力、捕获是引擎内部实现，两个正交维度）。
- **水体 pass**（scene_depth 之后、TAA / 后处理链之前；HDR MSAA
  alpha 混合、深度只测不写、cull None；独立 `water_layout` 组 0
  五槽位——不扩 globals 组，R61/R77/R78 先例）：**Fresnel**
  （Schlick，F0 = 0.02 水的物理值）× 反射（镜像 VP 投影采样 +
  法线波纹扰动 0.04 uv，ClampToEdge 吸收出界）+ 折射 × **深度
  吸收**（水下厚度 = scene_depth − 水面片元视距 clip.w；
  T = exp(−厚度/absorb_depth) Beer-Lambert 标量式，谱选择性由浅 /
  深双色渐变承担——岸边直接见底、深水趋 deep_color）+ 太阳镜面
  （Blinn-Phong × Fresnel）。**scene_depth 第六消费者**：水体在场
  时 `ensure_scene_depth`（fog_on 同款门），水体 pass 编码在
  scene_depth pass 之后 ⇒ 链上同帧读深度；**水像素不写
  scene_depth**（下一帧雾 / AO 对水面用其后地面深度——近似，§8.50）。
  顶部 `WaterParamsRaw` 尺寸 const 断言（240B）与 WGSL 布局互锁。
- **测试**：CPU 单测 9 项（默认值 / t=0 原点解析解 / 单波波长平移
  对称 / 单波与四波振幅上界 / 法线单位化与不卷绕 / 逐位确定性 /
  网格良构环绕确定性 / 选择键乱序不变）+ GPU 集成
  `tests/water_render.rs` 4 项（GPU_SERIAL 串行化）：生效反证（有 /
  无水体全帧 FNV 不同）、镜面反射（亮红塔 + 掠射角，镜像几何解析
  落点采样 R−G 有水 / 无水差 ≥ 30 可判别）、深度吸收（深水点无
  地面 → scene_depth = 远平面 → deep_color 主导，R−G 差 ≥ 90）、
  确定性（双会话逐位 + `wave_speed` 变哈希）。快照
  `examples/water_snapshot.rs` → `docs/water.png`（1×2：水体关 |
  开，海岸斜切构图——近岸浅滩 / 深水空域 / 远岸倒影塔；倒影带双
  点采样、深浅 B−R 差、双会话逐字节断言全带，目检倒影位置 / 深度
  渐变 / 波形起伏 / 无递归伪影）。
- **文档**：ARCHITECTURE §4.37 新增；§4.21 / §8.20 修订（内部捕获
  不占用户 RT 预算）；§8.50 诚实限制索引（单水面 / 无递归 / 相机
  入水无处理 / 水像素不写 scene_depth / 4 波等角无频谱 / 网格边缘
  露缝 / 水面后合成覆盖其前透明粒子 / height 不随 Transform / CSM
  主相机拟合 / Time 缺席静止）。

### Added（R80：昼夜循环 + 全局风场——纯相位解析解 × mix64 阵风采样，四条下游线共用接口轮）

路线图 v5 第二梯队 2/5。`iris-core` 新增**昼夜循环**（`daynight`）与
**全局风场**（`wind`）两个纯函数模块——零渲染依赖、无 rand、无全局
状态，同输入 to_bits 逐位一致；`iris-render` 渲染路径**零改动**
（唯一触碰点是新增快照 example），全部既有 GPU FNV 基线原值通过。

- **昼夜循环**（`iris-core::daynight`）：`DayNightCycle` 资源 = 归一化
  相位（0 日出 / 0.25 正午 / 0.5 日落 / 0.75 午夜）+ 速率（Default
  1/600，10 分钟一天）+ 暂停；`advance` f64 推进 + `rem_euclid` 回绕
  ——**纯相位函数无累积漂移**（固定步长推荐）。太阳方向 = 倾斜平面
  圆弧解析解（`sun_direction`，默认倾角 35°：正午仰角 55° 不在头顶、
  日出日落点钉在地平线；`normalize_or_zero` 兜底）；光照包络 = 正弦
  仰角驱动的分段线性曲线（`sun_envelope` 地平暖橙 → 正午白、夜间强度
  归零；`ambient_envelope` 昼间微冷白 ↔ **夜间下限底光
  (0.035, 0.045, 0.07) 不黑屏**；`is_night` = 严格低于地平线）。
  单测 12 项：四象限解析解对照 / 全周期无 NaN 扫描 / 包络端点精确值
  （sin(π) 舍入伪影 ±1e-5 容差写明）/ 单调分段 / 逐位确定性 / 回绕。
- **插件接线**（`iris-engine` `DayNightPlugin`，已含于
  `DefaultPlugins`，RenderPlugin 之后）：Update 阶段并行系统把
  sun_direction / sun_envelope / ambient_envelope 写进
  `DirectionalLight` 的 direction（**= −sun，光传播方向换算**）/
  color / intensity / ambient 四字段；`shadows` / `shadow_range`
  不动；SkySettings / FogSettings 不自动联动（§8.49）。**缺席语义**：
  `DayNightCycle` 缺席 = 零操作（方向光逐位零触碰——headless 测试
  实证）；方向光缺席 = 相位照常推进；Time 缺席 = dt 0。
- **全局风场**（`iris-core::wind`，**接口轮——诚实声明在案**）：R82
  粒子 v2 / R90–R91 毛发 / R92 降水 / 闭合池布料与植被四线的共用采样
  接口，v1 引擎内无消费者（`DefaultPlugins` 不注册 `Wind`，消费姿势
  写进模块头文档）。`wind_sample(wind, pos, t)` = 恒风（水平单位风向
  × strength）+ 阵风调制（`1 + gustiness × g`，g ∈ [−1, 1] 为 (x, z,
  t) 格点三线性值噪声，格点值 = splitmix64 哈希流——`particles.rs`
  mix64 范式移植，量化步长 2 m / 0.25 s 定稿写明，五次平滑插值 C2
  连续）。单测 10 项：逐位确定性 / strength 0 → 精确零向量 /
  gustiness 0 → 恒风逐位 / 异 seed 异场 / 构造上界
  `strength × (1 + |gustiness|)` / 零方向安全退化 / 时间空间连续性 +
  格点角点锚定 / smootherstep 端点单调。
- **快照**：`examples/daynight_snapshot.rs`（2×2：日出 0.04 | 正午
  0.25 / 黄昏 0.46 | 午夜 0.75，环形箱体山野 + 大气天空 + 适度雾 +
  `EnvironmentSettings intensity 0` 关 IBL——环境下限包络才是夜景
  唯一底光）→ `docs/daynight.png`：正午地面 427 vs 午夜 74
  （Δ353 ≫ 阈值 40）、黄昏太阳侧 R−B = 90 可判别、午夜地面 74 不
  黑屏、四相位 FNV 双会话逐字节一致。
- **诚实限制**（ARCHITECTURE §8.49 / §3.12）：星空 / 月亮不做；
  SkySettings / FogSettings 不随相位自动联动；Wind v1 无引擎内消费者
  （接口轮）；相位推进 dt 积分（可变步长 dt 序列不可复现——回放走
  固定步长）；太阳弧单平面无季节倾角模型；阵风只调幅值不旋风向、
  高度无关、单八度；包络艺术化分段线性非物理量纲。

### Added（R79：天空 v2 + 大气散射 + 体积雾——解析单次散射 × 链头光线步进）

路线图 v5 第二梯队首轮（环境底座）。`iris-render` 新增**程序化大气
散射天空**（`SkySettings`）与**逐像素体积雾**（`FogSettings`），全部
走「资源缺席 = 像素与命令流逐位不变」兼容形态（`DefaultPlugins` 不
注册；全部既有 GPU FNV 基线零改动原值通过）。

- **大气散射天空**（`shaders/skybox.wgsl`，片元内解析单次散射——无
  LUT，确定性零基础设施裁定，LUT 路线留闭合池）：平面指数大气 +
  Kasten 风格气团近似的 Rayleigh + Mie（Cornette-Shanks，g = 0.76）
  + 解析积分 `∫₀^D e^(−σt)dt = (1−T)/σ` 逐通道闭合 + HDR 太阳盘
  （smoothstep 双阈，乘太阳路径透射——日落红化）+ 地平线以下淡出。
  β_R 蓝端 33.1→52e-6 艺术化增强补饱和、亮度以 `SKY_INTENSITY = 48`
  常量折算到本仓 HDR 域（非物理量纲，诚实声明见 §8.48④）。
- **sky 组 binding 7 与专用布局**：`SkyParams` uniform（64B，含
  Rust 侧 static assert）挂在**新建专用 `sky_layout`**（bindings
  0/4/6 与 globals 组同资源）——globals 组 7 号已是点光 cubemap 阴影
  贴图不能扩号，`sky` / `probe_sky` 两条管线换用该布局，Globals 并行
  前缀结构**一字未动**（看门单测保持绿）。天空绑定组在全部三个创建
  点生成（主相机初建 / env 重烘焙重建 / 六捕获面）；mode = 0 帧渲染器
  **不发 write_buffer**（缓冲零初始化 = 死码分支）。捕获相机同样生效
  （探针内天空与主视口同源——特性）；IBL env 不联动（§8.48③）。
- **体积雾**（新 `shaders/fog.wgsl` + 新模块 `fog.rs`）：链头全屏
  pass（主 pass / TAA 之后、DOF / MB / 泛光之前），逐像素 16 步
  发射-吸收积分（起点像素哈希抖动去条带），密度场 =
  `density·exp(−(h−offset)·falloff)`、HG 相位（g 钳 ±0.999）、
  天空像素射线长取 `max_distance`；组布局复用 `post_depth_layout`，
  深度 = scene_depth **链上同帧**消费（与 DOF / MB 同款；**对规格
  偏离声明**：原规格按 SSR/AO 直觉写「上帧语义」，核实编码器顺序后
  确认链上消费者读到本帧深度，按实际情况记录不虚构滞后限制——
  §8.48①）。CPU 纯函数（`fog_density_at` / `fog_phase_hg` /
  `fog_dither` / `fog_transmittance`）与 WGSL 逐行镜像、单测全带
  （解析解 / 端点 / 单调 / 抖动分散性）；`FOG_STEPS` 常量导出。
- **槽位拓扑**：新纯函数 `full_chain_slots_with_fog` 单点定义雾开
  拓扑（雾占全分辨率 A、相机级链 A→B→A 轮转）；旧 `full_chain_slots`
  及其既有单测**零改动**，雾关时新函数返回值与旧函数逐字段相同。
- **测试**：CPU 纯函数 + Default 语义 9 项（fog.rs）+ 链拓扑 2 项
  （post.rs）；GPU 5 项（`tests/sky_fog_render.rs`，GPU_SERIAL 串行：缺席与显式
  关两两逐位 / 雾距单调（纯消光隔离，远墙 ≥ 2× 近墙）/ 相机升高雾
  变薄 / 大气色移 + 太阳盘可判别 / 双会话逐位含物体运动 + `phase_g`
  变哈希防恒等）。快照 example `sky_fog_snapshot.rs`（2×2：旧渐变 |
  大气抬头构图 / 平视雾关 | 雾开，跨运行逐字节一致）→
  `docs/sky_fog.png`。
- **诚实限制**（ARCHITECTURE §8.48）：雾深度与 DOF/MB 同帧（偏离
  声明）；捕获 / RT 相机不吃雾；IBL env cubemap 不与大气联动（反射
  与环境光仍是旧渐变内容——联动属探针重烘焙，R83/GI 线）；大气为
  艺术化解析近似非物理量纲；雾不参与 TAA 抖动前处理；max_distance
  截断。

### Added（R78：SpotLight + 前向光源分箱——CPU 屏幕空间 tile 分箱 × storage 灯表 × 双路径位等价）

路线图 v5 第一梯队收官轮（战斗底座）。`iris-render` 新增 **SpotLight
锥光组件** 与 **前向光源分箱**：突破 `MAX_POINT_LIGHTS = 8` 的全局均摊
（第 9+ 盏点光从「静默丢弃」变为「灯表生效」），并补上锥光——手电筒 /
聚光灯类玩法光源此前只能用点光近似。

- **SpotLight 组件**（`light.rs`）：`color / intensity / range`（衰减与
  点光同模型）+ `direction`（显式世界空间锥轴）+ `inner_cutoff /
  outer_cutoff`（软边锥形，CPU/WGSL 同式的 `spot_cone_factor`；inner >
  outer 实现层钳到硬边、零长度方向 = 不发光）。prelude 导出。
- **分箱纯函数**（新模块 `light_bins.rs`）：`bin_lights` 把灯按屏幕
  32² tile 分箱——近平面门（球心 `clip.w ≥ 0.05` 才可见，整盏跳过）+
  相机轴 8 角投影 NDC 矩形（角深度钳近平面，保守外扩）+ clamp 展开；
  `BTreeMap` 展平保证与构建次序无关的确定性，tile 内下标升序（灯数组
  序即求和序）。`light_bins_enabled` 自动阈值（点光 > 8 或有锥光）。
- **GPU 资源**（globals 组 binding 15/16/17，「不扩 Globals 前缀」
  扩容先例延续，skybox 并行布局零触碰）：32 B 参数 uniform + 512×80 B
  灯表 storage（前 8 盏点光的阴影开关 / cubemap 槽位与 `Globals`
  逐字段镜像）+ 1 MiB tile 索引 storage（ranges 区 + indices 区，超容量
  按序截断 + warn）。`LightBinningSettings` 资源缺席 = 自动阈值，
  `force` 为验收强制入口；探针捕获 / RT 相机恒绑哑资源（mode = 0）。
- **双路径位等价**：fs_main 点光循环体**逐式提取**为
  `point_light_contrib` 共享函数（`ptr<function>` 出参累计，保加法
  结合序）；mode = 1 时旧循环由均匀门跳过（防前 8 盏计双）、分箱分支
  只在 mode = 1 执行。同一灯集合两路逐像素同串位；灯表缺席/关闭 =
  全零哑缓冲 + 零 `write_buffer`（命令流与 R77 一致）。**全部既有
  FNV 基线零改动原值通过**（10 个 GPU 基线套件亲验）。
- **测试**：CPU 纯函数 9 项（分箱覆盖/近平面/越界 clamp/升序/排列
  无关/确定性、锥光因子解析解与 NaN 不外溢、阈值边界、SpotLight
  构造）+ GPU 4 项（`tests/light_bins_render.rs`，GPU_SERIAL 串行：
  force 对缺席逐字节相等含阴影灯 / 第 9 盏照亮暗角且远点不变 /
  锥内显著亮于锥外同灯距点×force 复跑 / 双会话逐位 + 移动锥光变帧）。
  快照 example `lights_snapshot.rs`（2×2 验收图，跨运行逐字节一致）
  → `docs/spotlight.png`。
- **诚实限制**（ARCHITECTURE §8.47）：锥光不投影阴影；锥轴不随
  `Transform` 旋转；探针/RT 捕获不渲染锥光与第 9+ 盏点光；2D tile
  对深度不敏感（墙后灯白付求和成本）；灯表与 tile 索引超容量按序截断。

### Fixed（启动性能：GLB `buffer()` 缓存命中整段克隆——iris-exp 启动 16s → 2s）

用户报 iris-exp 启动 ~5s+ 才出画面。分相计时实测（临时探针，测毕删除）：
GPU 初始化 0.6s / **动画剪辑解析 14.6s** / PNG 解码 0.6s / 首帧上传
0.55s——病灶是 `asset/gltf.rs::buffer()`：BIN 虽缓存进 HashMap，但返回
owned `Vec<u8>`，**每次命中整段克隆**；`build_clip` 按通道两次调用
`read_floats`，32MB GLB × 783 通道 ≈ 50GB memcpy 全花在启动。

- 修复：缓存与返回改 `Arc<[u8]>`（命中 = O(1) 引用计数，miss 才装载），
  四个调用点（read_floats / read_indices / JOINTS / 内嵌图片）零逻辑
  改动——纯数据搬运路径，像素逐位不变（全量既有 FNV 基线零改动通过）。
- 看门单测 `buffer_cache_hit_shares_allocation`（`Arc::ptr_eq` 断言，
  退化回整段克隆即红）。
- 实测：iris-exp Startup 15.97s → **0.52s（31×）**，总启动 ~1.6s
  （GPU 设备 0.6s + 首帧贴图上传 0.45s 为正常成本）。

### Added（iris-exp：Idle 常态状态机 + 移速步幅匹配 + 渲染能力装配——演示轮，引擎零改动）

实验项目演示升级（用户三诉求：Idle 支持 / 走路过快排查 / 用上引擎渲染能力）。

- **常态 Idle 状态机**（`games/iris-exp/src/plugin.rs`）：新剪辑契约常量
  `IDLE_CLIP = "Mixamo_Idle"`（GLB 三剪辑齐备，Idle 6.0 s 呼吸循环）；
  `ExpEntities` 加 `idle` 下标（缺 Idle 回退指 walk，退化不 panic）。切换
  语义：移动即 Walk、停止即回落 Idle 从头循环、Jump 单次播完 → Idle
  （若仍按住移动键同帧随即切 Walk——「动作停止且无立即下一步动作 = Idle」
  精确成立）；旧「停下定格」语义与 `1` 手动切换键一并退役（走停全自动）。
  跳跃索引与 walk/idle 重合的退化态视为「无跳跃」，状态机不锁死。
- **走路过快排查：非帧率问题**——引擎动画推进 dt-based
  （`update_animations` 读 `Time::delta_secs()`，帧率无关），GLB 剪辑时长
  口径实测正确（Walk 58 键 @60fps = 0.967 s/循环）。真因 = `MOVE_SPEED`
  3.0 m/s 是慢跑速度、与步行动画步幅（≈1.8 m/循环）不匹配 → 位移快于
  步幅、脚底打滑。修正 3.0 → **1.8 m/s**（步幅匹配）。
- **渲染能力装配**（`setup_resources`，窗口/headless/离屏三路共享；全部
  「缺席 = 关」opt-in 资源的游戏层显式启用）：`AoSettings`（R77 GTAO，
  radius 0.5 世界半径）+ `TaaSettings` + `VignetteSettings` 0.35；
  `MotionBlurSettings` 装配就位但暂关（腾空段肢体拖影在定格图读作残影，
  窗口游玩可开）；SSR / 遮挡剔除按场景特性不开（无反射面 / 24 m 小场景），
  理由写进装配注释。Bloom 默认档不动。
- **快照定格协议升级**（`examples/exp_snapshot.rs`）：两张定格间只推进
  模拟不渲染 ⇒ 跨帧时域状态全是上一张——**`reset_taa` 清 TAA/SSR 历史但
  不清 GTAO 上帧深度副本**（引擎缺口，frictions ⑬），故定格 =
  `reset_taa()` + 三连渲让 AO 深度与 TAA 历史都收敛到本姿势；实测跳跃帧
  地面腿形残影闭合。`docs/exp.png` 重生成。
- **测试**：headless 新增 `idle_default_state`（初始 Idle / 无输入推进 /
  走停切换 / 跳跃回落四段精确断言）；`clip_contract` 扩 Idle 契约 + 初始
  常态断言；`character_move_camera_follow` 的「停下定格」旧断言更新为
  「停止回落 Idle 播放」。既有确定性/轨道相机测试零改动通过。

### Added（R77：GTAO 环境光遮蔽——切片水平角 × 上帧深度副本）

路线图 v5 第一梯队第七轮（战斗底座）。`iris-render` 新增**屏幕空间环境
光遮蔽**：动态物体落在墙角、柱脚、箱堆时的环境光互遮蔽此前没有来源
（ORM ao 只描述材质级遮蔽、光照贴图天空 AO 只覆盖静态烘焙几何）。

- **兼容形态（硬约束）**：`AoSettings` 资源**缺席/关闭 = R76 逐位不变**
  ——不分配可见度图、不编码 pass；globals 组新增 binding 14（R61/R62a/
  R62b「不扩 Globals 前缀」扩容先例，skybox 并行布局零触碰）回落常驻
  1×1 R16Float 哑纹理，**纹素恰为 1.0（f16 0x3C00）**——`standard.wgsl`
  无条件采样后 `× 1.0` 由 IEEE 754 逐位精确，零分支实现零开销默认路径。
  全部既有 GPU FNV 基线（post / ssr / taa / probe / lightmap / fxaa /
  bloom / env / shadow_v2 / sss / decal / transparent / irradiance 等
  37 个测试二进制 541 项）零改动通过。探针捕获面 / RT 相机同样哑纹素
  （屏幕空间语义只对主相机成立，§4.31 裁决 2 同款）。
- **GTAO pass**（`shaders/ao.wgsl`，主 pass 之前编码，读上一帧深度副本
  ——与 SSR 同款「上帧」语义）：①深度纹素中心重建视空间位置（CPU 镜像
  `post::ao_view_pos` 经 glam 透视正反算互锁）；②法线 = ±1 纹素深度
  差分叉积（纯 textureLoad，无导数内建）；③4 切片 × 正负 8 步世界半径
  搜索，切片方向按像素整数哈希 dither 旋转（`post::ao_dither` 逐行
  镜像）；④可见度 = (1 − 切片平均 0.5·(sinθ↑+sinθ↓))^intensity，写
  半分辨率 R16Float。
- **乘点与 ORM ao 同靶**：`ibl_final`（IBL / 光照贴图 / 探针镜面）与
  常量环境项乘可见度；直接光不乘——R41「AO 描述环境光损失」语义的
  屏幕空间扩展。
- **重置语义**：`ao_depth_frame` 记账深度副本最近运行帧号，本帧恰为
  +1 才编码 GTAO，否则清中性 1.0（首帧启用 / 上帧无几何 / 副本重建帧
  与关闭帧逐位一致）；`FrameStats::ao_on` 观测字段。
- **三个实现级归因（实测定位，测试锁定）**：①**半纹素错配**——样本
  连续 uv 配 floor 后纹素深度会在斜面上注入 ~1e-2 假仰角（半径 0.2 m
  时假遮蔽 5%），重建一律取深度纹素中心后平面样本仰角精确归零；
  ②**越界样本弃权**——钳制读取会配出不存在的表面几何；③**screen uv
  弃用顶点 varying**——`clip.x/w` 是投影量，地面/墙等大三角形常有顶点
  在相机背后，近平面裁剪后 x/w 爆炸，插值出越界坐标把可见度图采样扭曲
  成大片假遮蔽（梯度注入实测定位）；改用 `@builtin(position)`（片元
  输入 = 帧缓冲像素坐标，纯仿射）除以 `textureDimensions(ao_tex)×2`，
  哑纹理路径值恒 1.0 与坐标无关，兼容性不受影响。
- **CPU 镜像纯函数**（`post::{ao_dither, ao_view_pos, ao_uv_offset,
  ao_sample_sin, ao_falloff, ao_slice_occlusion, ao_visibility}` +
  `AO_SLICES/AO_STEPS`）：含重建公式与 glam DirectX RH 透视的正反算
  互锁、步进换算保世界半径解析证明、可见度曲线单调与锚点。
- **测试**：`tests/ao_render.rs`（GPU_SERIAL 串行化，缺席=显式关逐位、
  启用首帧中性 + `ao_on` 翻转、角落严格变暗 / 开阔地容差内不变且暗化
  差可判别、强度指数单调、双会话含物体运动逐帧逐位）+ post.rs CPU
  单测 6 项。基线 **debug 1276 / release 1273**（+10）。
- **验收图**：`docs/ao.png`（三排 关|开：日光房间 / 强度两档 / 纯间接
  光）——接触阴影贴合几何、开阔地逐位不变；快照 example 跨运行逐字节
  一致（FNV 打印比对）。诚实限制 → ARCHITECTURE §8.46（掠射角低估 /
  深度断裂光晕 / 光照贴图叠加过暗 / 无时域滤波 / 奇数分辨率口径差 /
  透明贴花不参与）。

### Added（R76：接触求解 v2——角冲量 + 库仑摩擦锥 + 顺序冲量迭代）

路线图 v5 第一梯队第六轮（战斗底座），闭合 §8.27①「接触不产生角
冲量」。`iris-core/physics` 新增 **opt-in** 顺序冲量求解器：

- **兼容形态（硬约束）**：`ContactSolverV2` 资源**缺席 = v1 路径逐位
  不变**——六个 dogfooding 游戏不注册，全量 headless 测试（FNV 轨迹
  锁）零改动通过；`step_physics` 内 v1 接触循环原文包进 `else` 分支，
  检测 / broadphase / 过滤 / sensor / 事件采样 / 关节轮 / 写回全部
  共用。首帧事件（法线 + 穿透）v1/v2 逐位一致（采样先于求解）单测
  守护。
- **角冲量**：接触点臂 `r = point − center` 进冲量 `ω += I⁻¹(r × P)`。
  惯性张量解析计算（`shape_inertia` 纯函数）：球 / 盒精确实体公式；
  胶囊 = 圆柱 + 双半球精确组合（对称轴 `I = ½m_cyl·r² + ⅘m_cap·r²`、
  垂直轴含平行轴项；`hh = r` 退化同归球——结构性校验单测）；
  `inv_inertia_world`：胶囊随体旋转 `R·diag⁻¹·Rᵀ`（碰撞是旋转感知的，
  R50 口径），球各向同性、盒世界轴对齐（同盒碰撞体恒 AABB 语义）。
- **接触点**（`contact_point_rotated` 纯函数）：九分支界面点近似 =
  两侧表面最近点中点。实现期自查修正：初版的「方向退化取中心连线
  中点」兜底会误伤**恰好触碰**（p == q 是正常态）——改为恒取中点。
- **库仑摩擦锥**：切向冲量向量按 `|λ_t| ≤ μ·λ_n` 钳制（各向同性
  近似）；μ 取参与响应的**动体**摩擦的几何均值（v1 参与规则的 v2
  版；纯 Collider 静态地板无摩擦数据——粗糙地面用球自身的
  `RigidBody::friction` 表达）。符号约定自查修正：摩擦冲量 λ_t 是
  **施给 b** 的向量（a 侧获得 −λ_t）——实现期发现的符号错误（初版
  `λ_t − t·(t_speed/k_t)`）在锥边界测试暴露后闭合。
- **迭代**：首轮收集全部接触（遍历序 = v1 候选序），`velocity_iterations`
  （默认 8）轮 Gauss-Seidel 顺序冲量——`solve_contact_impulse` 纯函数
  （法向 λ ≥ 0 累积钳制不吸、锥钳制不爆；弹性并入首轮捕获
  `target_vn = −e·max(0, v_n0 − threshold)`，`restitution_threshold`
  默认 0.5 m/s 低速非弹性防堆叠微弹）；位置投影**逆质量加权** +
  `slop`（默认 5 mm）+ `beta`（默认 0.8），外层 `SOLVER_ITERATIONS`
  结构不变。
- **`RigidBody::mass` 字段**（默认 1.0，须为正）：v1 路径不读（等质量
  近似不变）；不进关卡序列化（代码侧装配，v2 诚实边界）。**偏离
  声明**：字段新增机械更新三处穷尽结构体字面量（scene.rs 反序列化、
  infiltration 吊灯、physics.rs 既有测试一处——均补 `mass: 1.0`，
  语义零变化）；其余既有测试零改动。
- **测试 +19**（工作区 debug 1266 = 1247 + 19）：惯性解析对照（球
  8.0 / 盒 13/3 族 / 胶囊 0.46·0.91 组合 / 退化同球）、逆惯性旋转
  共轭对照、接触点三分支、正碰弹性精确（e=1/0.5/分离钳零）、偏心
  冲量**独立手算** ω（k_n = 1.625、ω_z = 1.25λ）、锥边界
  `|λ_t| = μλ_n` 精确、双球弹性对撞质量加权解析（m1=1 撞 m2=3 →
  −u/2 与 +u/2）、滑动球起转方向（+X 滑 → −Z 转起；v1 对照 ω 恒零）、
  沉降无抖、首帧事件 v1/v2 逐位一致、v2 全轨迹 bit 级确定性（含
  旋转胶囊）、v1 零变化锁（mass 不参与）。
- **文档**：ARCHITECTURE §3.8 v2 条目 + §8.27① 修订；physics.rs
  模块文档新章节「接触求解 v2」；prelude 导出七个新公开项；
  PhysicsPlugin 访问集 + `read::<ContactSolverV2>()`。

### Added（R75：IK v2——FABRIK 任意长链 + look-at 瞄准链）

路线图 v5 第一梯队第五轮（战斗底座）。`iris-render` 新模块
`chain_ik.rs`，与既有两骨 IK（`foot_ik` / `hand_ik`）**并存不替换**：
两骨解析解（余弦定理一次到位）继续服务腿 / 臂，本模块补「三骨以上的
软链」（触手 / 尾巴 / 脊柱链）与「瞄准」（头 / 炮塔 / 脊柱跟随视线）。

- **FABRIK 纯函数 `solve_fabrik`**：标准前向后向交替迭代——后向遍
  末端钉目标自末向根重排、前向遍根钉回原位（**链首节点是锚**，与
  足部 IK 的髋同款：链首位置永不动）自根向末重排。**确定性契约：
  固定迭代上限 + 数据驱动早退**（末端距目标 ≤ tolerance 即停——同
  输入同停点）；不可达目标 = 沿锚→目标按累计骨长**摆直**（软链的
  期望视觉，**刻意不做**手部 IK 式放弃门——手部防的是异常握把位把
  臂拉成直线指天，软链的伸长是期望行为）。退化方向（两端重合）取
  +Y / −Y 兜底（与 `solve_two_bone_ik` 同款约定）。
- **`ChainIk` 组件 + `update_chain_ik` 系统**：`node_entities` 连续段
  `[start_node, start_node + node_count)` 声明 `ChildOf` 直连链，末端追
  `IkTarget` 目标（复用 hand_ik 语义：缺席 / 失效 / 无全局位 / 隐藏 →
  本帧放弃；一帧滞后与挂点禁忌同款）。回写与足 / 手部同构：骨方向
  旧 → 新最小旋转弧左乘旧世界旋转、链内父级取新值反推局部旋转并
  `normalize()`（打磨 VI 摩擦①纪律）。
- **实现期真 bug（探针归因，已闭合）——末端位置回写的吸收态漂移**：
  初版照 foot_ik 模式把末端局部位置按解算位回写；但 FABRIK 骨长取
  **当前世界姿势**，写坏的末端偏移长会喂进下一帧骨长测量，误差成为
  无回归力的吸收态逐帧游走（探针实测 600 帧内末端骨 0.25 → 0.251 破
  容差）。修复：**末端节点局部旋转与位置都不写**——全部局部偏移恒定
  ⇒ 骨长**结构性**严格保持，末端世界位 = 末骨新方向 × 原偏移长，与
  解算位差在浮点噪声级（≪ 解算容差 1e-4；与两骨解「精确到位」语义
  的刻意差异已写进模块文档）。600 帧移动目标回归锁守护。
- **look-at 瞄准 `LookIk` + `update_look_ik`**：末端节点瞄准轴
  （局部系，默认 −Z）指向目标。**单遍闭式分布（非迭代）**：总摆动
  S = 当前瞄准 → 目标方向的最小旋转弧，链上每节点施加份额
  `q = S^(weight/node_count)`（轴角幂 `swing_fraction`；世界系同 q 施
  加于各节点子树、末端复合 n 份 ⇒ 恰得 S^w——权重 1 时瞄准方向精确
  指向目标方向）。局部旋转 = 旧父世界旋转⁻¹ · q · 旧世界旋转（q 共轭
  进局部系）。**诚实边界（近目标残差）**：S 按修正前端末位置计算、
  份额施加中关节旋转移动末端 ⇒ 目标方向微变，单遍不回算——残差 ∝
  （末端位移 × sin 摆角）/ 目标距离（米级目标 < 0.5°，可感知为零；
  目标贴近末端时增大；不累积——每帧从当前方向全量重摆）。
- **公共约定**：节点映射 `IkRig` / 播放器二选一（同款）；链上节点
  scale 恒 1 契约；幂等门（链末端已在容差内 / look-at 已在 ~0.08°
  内 → 不写任何变换，静止目标 bit 级稳定）；**无关节角限制（v1）**
  ——两套解算都不做角度 / 锥形限制，需要时游戏侧剪辑通道逐帧重置 +
  本层重解吸收。注册序 `update_animations → foot → hand → chain →
  look`（同段 IK 依次作用于同一份本帧姿势，注册序确定 ⇒ 结果确定；
  look-at 收尾——瞄准是姿态最后一笔）。均并行系统（`&World` 只读 +
  尝试型锁写）。
- **测试 +23**：FABRIK 纯函数（不可达摆直解析预言 / 收敛 + 骨长 /
  逐位确定 / 三类 panic 断言）、`swing_fraction`（轴角幂解析对照 /
  恒等）、链系统（到达 + 骨长 / 不可达拉直 / 缺席隐藏目标 / 幂等门 /
  逐位确定 / 断链 debug panic / 600 帧移动目标范数 + 骨长回归锁）、
  look-at（单节点 Ry(−90°) 解析 / 三节点均分 Ry(−30°) 与末端 +X 精确 /
  权重减半 / 三类跳过门逐位 / 600 帧范数锁 / 与链 IK 并存互不干扰 /
  无静态体依赖——FABRIK 目标是玩法量不做射线）。
- **影响面**：纯 CPU 变换层新增，无渲染路径改动、无既有组件字段
  变化——不挂 `ChainIk` / `LookIk` 的世界零行为变化，无像素基线问题。
  引擎文档：ARCHITECTURE §4.6.1 新条目；prelude 导出六个新公开项。

### Fixed（R66f：BC1 端点排序病灶——iris-exp 脸部黑斑闭合）

iris-exp 外部角色脸部近距散落**纯黑小条斑**（脸颊/下巴/嘴角，身体
皮肤干净）。归因链：资产侧排除（脸部法线无黑 texel、exp_face_d 无
孤立暗点、图集背景非黑）→ 离屏开关对照（隐藏头发/眉/眼/耳/服装、
摘法线全部零变化；**脸部 baseColor 声明 RGBA8 → 黑斑归零**）→ 病灶
锁定 BC1 压缩路径。CPU 解码探针钉死机制：**BC1 解码器以 565 打包
整数值比较选四色/三色模式，编码器却按亮度排序端点**——高饱和暖色
（粉红皮肤：R 主导打包值、亮度由 G 主导）亮度序与打包序相反 ⇒ 块
被误判三色模式、index 3 解出纯黑。测试解码器与编码器同假设四色
模式，往返自测双双「正确」把 bug 遮住（共谋盲区，非 GPU 特有）。

- **修复**（`texture_bcn.rs`）：①端点排序改按 **565 打包整数值**
  （恒四色模式；打包值相等的纯色块索引恒 0 亦安全）；②同轮修正
  `mix_third` 舍入方向（BC1 规格整数除法为向下取整，旧版 +1 四舍五入
  使编码器调色板与解码器差 ±1、最近邻索引可错档——整块偏一个色阶的
  质量 bug，非黑斑来源）；③测试解码器补齐硬件模式规则（四色/三色 +
  index 3 纯黑），消除共谋盲区。BC3 alpha 块核查无恙（端点取块内
  极值，`a0 > a1` 天然成立）；BC3 色块复用 `encode_bc1` 自动受益。
- **测试 +1**：`bc1_saturated_warm_block_never_decodes_black`——构造
  亮度序与打包序确实相反的双色块（橄榄 (200,200,120) / 红 (240,60,60)），
  断言硬件规则解码不出纯黑 + MAE < 30；bcn 单测 15 项全绿（含新锁；
  位打包/纯色往返断言不受影响：灰阶与 565 精确色的亮度序 = 打包序）。
- **影响面与基线**：BC1 / BC3 色块位流改变（凡命中 BC 上传路径的
  纹理 GPU 字节变化）。实测**像素 FNV 锁零漂移**（debug / release 全量
  门禁全绿）：既有测试纹理内容（灰阶 / 砖纹 / 游戏快照）亮度序与
  打包序一致，改动面未外溢到任何锁——无需重锁程序。高饱和暖色资产
  （粉红皮肤类）才会显形差异。
- **文档**：ARCHITECTURE §4.18 编码器条目改写；iris-exp frictions ⑩
  闭合。

### Added（R68：战斗相机——trauma 屏震 + FOV punch + 取景呼吸）

W7 战斗手感轮（游戏轮纪律：全部改动在 `games/iris-assault`，**引擎零
改动**）。战斗相机的「反馈感」三件套，新模块 `camera_fx.rs`（纯函数
+ 无状态哈希流，headless 可测、逐位确定）：

- **屏幕震动（trauma 模型）**：事件往 `CameraFx.trauma`（0..1，clamp
  加法）里加值、每帧线性衰减，实际幅度 ∝ **trauma²**（平方把 0..1
  压进感知下半段——小震叠加平缓、满格才剧烈）。每帧方向从 splitmix64
  × 帧计数器哈希流抽取（引擎 `particles.rs::mix64` 同款纪律：无
  wall clock、无 RNG）。震动 = XZ 单位圆内平移 + 1/4 幅度 Y 抖 +
  绕视线轴 ±2° roll。分级：受击 0.45 > 三段重劈命中 0.35 > 击杀
  0.25 > 普通刀 0.18 > 枪 0.06；
- **FOV punch**：`fov_kick` 线性衰减（0.26 rad/s），三段命中 +6°、
  受击 +4°，写回 `ActiveCamera::fov_y_radians`（组件字段可变，渲染
  器每帧读——无需引擎配合）。**粘滞 bug（实现期自查）**：kick 衰减
  到 0 的那帧若跳过写回，残留 kick 会永久粘在视场上——非演出期改为
  无条件写回（`BASE_FOV + 0` = 基准值）；
- **战斗取景呼吸**：最近活敌距离映射机位偏移缩放（`frame_scale`
  纯函数）——敌进 4 m 锁敌半径线性拉远至多 12%（留挥砍与击退空间），
  远距 / 无敌 / 非有限输入恒 1.0；
- **接线与编排**：触发在 `consume_game_events`（独立事件扫描循环，
  与各事件的表现处理解耦）；消费在 `write_character_visuals` 相机块
  ——**演出期时间线接管相机，反馈状态照常衰减但不上屏**（trauma
  不打断运镜）；静默（trauma = 0）时偏移 / roll / fov 恒零，相机
  行为逐位等于 R67 之前；
- **测试 +6**：trauma clamp 与线性衰减精确值、静默帧零输出（逐位）、
  震动包络上界（4096 计数器全扫描：水平 ≤ 1、竖直 ≤ 1/4、roll
  ≤ 1/2）、噪声逐位确定性（同状态双跑同轨迹 + 相邻计数器不同流）、
  advance 推进游标抖动、取景缩放端点精确值。headless 8 + roundtrip
  3 在新相机下全绿；
- **快照**：`docs/assault.png` 重生成（战斗中机位轻微拉远），两次
  运行 BMP md5 逐字节一致。

### Added（R67：帧数据战斗——assault 三连段时序权威迁移 + 取消窗口）

W7 战斗手感轮（路线图 v5 第一梯队；R74 帧数据基建的实弹验收，游戏轮
纪律：改动面在 `games/iris-assault`，引擎侧仅一处 prelude 再导出）。
**核心设计：帧数据表 = 单一事实源**。R67 之前同一份战斗时序存在三处
手工复刻——logic.rs 相位常量（模拟）、资产生成器剪辑关键帧（表现，
sword1 关键帧 `[0, 0.18, 0.25, 0.32, 0.54]` 恰为前摇+横扫+后摇）、
插件层「刚起手」启发式（`t <= 1.5/60`）——漂移隐患随任何一处改动成立。
新模块 `frame_data.rs`（纯 const 表 + const fn 查询）成为唯一手写处：

- **模拟侧派生**：`SWORD_WINDUP/SWEEP/RECOVER`、`ENEMY_WINDUP/…` 常量
  改为从表 const 派生（零运行期成本）——相位推进、取消判定、敌人硬直
  全部读表；**既有测试零改动全过**（常量名与值不变，既有 logic 测试
  原样通过）；
- **表现侧生成**：装配期把表生成为 R74 帧数据（`windup`/`strike`/
  `recover`/`cancel` 四窗口 + `swing_start`/`cancel_ready` 两事件 +
  `dmg_scale` 甜区曲线）注入玩家上身播放器的 sword1..3 剪辑
  （`clips_mut()`，setup 一次性）——剪辑窗口与模拟相位**构造性一致**；
- **取消窗口（新玩法）**：后摇期、动作时刻落在取消窗（= 判定窗关闭
  到动作末尾，第 1/2 段；第 3 段终结无取消）内的攻击键边沿**当帧
  接续**下一段——横扫判定窗内的按键仍是旧「预约」语义（后摇结束
  接续），预约在先、取消在后并存。三连段手感显著收紧；
- **事件驱动表现层**：新增 `AssaultEvent::SwordStageStart` /
  `EnemyWindupStart` 逻辑事件，插件层据此切换/重触发上身剪辑
  （一帧不差）——删除两处 `t <= 1.5/60` 启发式轮询（重复时序知识）；
- **R74 API 运行期实弹**：动画事件消费（`cancel_ready` → 刀尖提示
  火花）、窗口门控（`window_active("strike")` → 判定窗内逐帧刀光
  burst）、曲线缩放（`parameter("dmg_scale")` 1.0→1.5→1.0 → 粒子数
  3..5，窗中点最密）；
- **引擎侧（唯一改动，摩擦驱动）**：R74 帧数据类型补入 iris-render
  prelude（`ClipWindow` / `ParameterTrack` / `window_active` /
  `sample_track`）——assault 刻意不直依 iris-render（R48 打磨后的
  crate 结构约束），prelude 缺口是本轮 dogfooding 摩擦，按打磨轮
  先例引擎侧最小修复；
- **f32 位级一致性教训**：取消窗口写成 const 算术（`0.18 + 0.14`）
  而非预约合字面量（`0.32`）——`0.18+0.14+0.22` 的 f32 和 ≠ 手写
  `0.54`，表内值与派生式（`recover_start()` / `total()`）必须位级
  一致，frame_data 单测双向锁定；
- **快照**：`docs/assault.png` 重生成（取消窗口收紧连段节奏，自动
  玩家战局提前；两次运行 BMP md5 逐字节一致）。快照捕捉条件补
  「玩家在北半场」几何前提——相机朝北俯视，北半场时南外墙必在视锥
  背后，剔除断言（`culled_instances > 0`）才有保证；取消窗口曾让
  首次 stage≥2 横扫提前到南半场（南墙进视锥 → culled=0 断言炸）。
  收窄捕捉条件而非放宽断言（指标学：断言前提要自持）；
- **测试**：+7（frame_data 3：表↔常量派生双向锁定 / 取消窗半开边界 /
  总长与剪辑时长同源；logic 4：取消窗当帧接续+事件 / 判定窗内预约
  语义不变 / 第 3 段不可取消 / 敌人前摇事件）。headless 8 项 + 快照
  roundtrip 3 项在新玩法下全绿（预约接续测试按住键断言三段齐发与
  击杀，不钉接续时刻——取消收紧不破坏）。

### Added（R74：帧数据窗口 + 参数曲线轨——R67 帧数据战斗的动画侧前置）

**设计定稿修正**：路线图原文「动画事件 + 曲线轨道；全库无事件轨道
（grep 实证）」前半句有误——事件轨道 v1 早已完备（`AnimationEvent` /
`collect_events` 纯函数：非循环 `(prev, new]`、循环 multi-fire、触发
时刻升序；`take_events` drain），当轮 grep 用了小写模式漏检，本条即
偏离声明。事件是离散点，战斗判定还需要**区间**与**连续值**两类时间
语义，本轮把这两块补齐：

- **帧数据窗口**：`AnimationClip.windows: Vec<ClipWindow { name, start,
  end }>`——「攻击判定开 / 可取消 / 无敌」的区间载体。判定纯函数
  `window_active`（可脱离 ECS 单测）：半开区间 `[start, end)`、畸形
  窗口（`start >= end`）恒 false、无同名窗口 = false（未声明即不
  active，组件缺席语义）、非有限 time 恒 false。播放器便捷入口
  `AnimationPlayer::window_active(name)`（active 剪辑 × 当前时间）。
  **与事件同帧衔接**：事件在 `start` 触发、窗口从 `start` 生效——
  同一帧既收到事件又处于窗口首帧（集成测试逐帧锁定）。
- **参数曲线轨**：`AnimationClip.curves: Vec<ParameterTrack { name,
  keys }>`——命名标量关键帧轨（伤害随命中帧衰减、镜头 FOV 等数值的
  时间演化）。采样纯函数 `sample_track`：线性插值、端点钳制、并列
  时间取输入先者（`ParameterTrack::new` 稳定排序 + 扫描首遇即返回
  ⇒ 并列行为确定）、非有限 t 返回 0.0（与 BlendSpace「无效参数 →
  中性值」先例一致）。`update_animations` 每帧推进后对 active 剪辑
  全部曲线重采样**整表覆写** `player.parameter_values`（区间状态
  语义，非消息流、无 drain），查询 `player.parameter(name)`（同名
  多轨取先声明者，向量序确定）。
- **实现形态**：`AnimationJob` 扩展第 7 元（曲线采样表），两阶段
  数据流不变——阶段一只读采样（推进后的播放时间）、阶段二落账。
  GLB 解析不产窗口 / 曲线（不在 glTF 标准内，同事件先例），经
  API 侧构造或 `clips_mut()`（`Arc::make_mut`，共享时复制）注入。
- **测试（4 项新增，关键帧全取 2 的幂分数 ⇒ 期望值精确无舍入）**：
  采样排序 / 钳制 / 区间线性 / 单 key / 空 keys / 非有限 t 的精确值
  断言；并列 key 的「并列点取前者、区间左端取后者」确定性行为；
  窗口半开边界（start 含 / end 不含 / 畸形 / 未声明 / NaN）；三件套
  经 `update_animations` 的四帧推进逐帧断言（事件触发帧 = 窗口首帧、
  曲线值逐位、末端钳制）。既有 50 项动画测试零改动全过（17 处
  `AnimationClip` 字面量中 8 处机械补 `windows` / `curves` 空字段、
  9 处本就走 `..Default::default()`，断言逻辑零改动）。
- **诚实限制（§8.44）**：倒放（负 speed）不触发事件；`parameter_values`
  是区间状态语义非事件流（误用即每帧整表被覆写）；窗口 / 曲线只对
  active 剪辑生效（crossfade 源冻结、附加层不产——与事件同口径）。

### Added（R73：数据并行分片——单重系统吃满多核，§8.1 半闭合）

调度器 v2 首轮（路线图 v5 第一梯队，用户裁定提前）。此前并行只到
**批次粒度**（§8.1）：一个批次内的多个无冲突系统各占一线程，但**单个**
重系统永远单线程遍历。本轮新增第三种系统形态**分片并行系统**：

- **API**：`Schedule::add_sharded_system::<T, _>(stage, access, f)`，闭包
  `Fn(&World, &[Entity]) -> Vec<(Entity, T)>`。调度器一次性收集实体列表
  （稀疏集插入序，确定性），按纯函数 `shard_count_for(实体数, 核数)`
  切成固定连续区间（`shard_range`，尾片吸收余数、相邻片首尾相接），
  每片以不相交子切片调用一次闭包；写组件 `T` **自动并入** `Access` 写集
  （分批口径覆盖实际写，消掉声明遗漏的脚枪）。
- **两段式执行模型（本轮核心设计决策，由存储模型倒逼）**：组件列的
  写锁是**列级** RwLock（`sync.rs`），分片线程并发 `get_mut` 同一列必
  冲突（实现期首轮实测炸出）——因此分片闭包**只读**世界（列级读锁
  并存），逐实体返回写缓冲；join 后调度器按**成员序 × 分片序 × 缓冲序**
  （= 实体列表序的稳定全序）串行写回。稳定全序 ⇒ 结果与分片数、线程数、
  机器核数全部无关：native 多线程与 wasm 单线程（`available_parallelism`
  兜底 1 ⇒ 分片数退化 1、内联路径）逐位一致。诚实边界写进 API 文档：
  写回段串行，**写密集、计算轻**的变换（纯拷贝级）分片收益有限，此类
  场景更适合既有整体并行系统按列拆分。
- **分片数公式**：`items ≤ 1024`（`MIN_SHARD_ITEMS`）不分片，否则
  `min(cores, items / 1024)`——每片 ≥1024 实体，摊薄线程创建的微秒级
  开销；输出只依赖 (实体数, 核数) 两入参，同一次执行内全分片取值一致。
- **批执行器统一为任务列表**：普通并行系统 1 任务、分片系统 W 任务，
  沿用原子计数器自取 + `thread::scope`；单任务/单核走内联保序路径。
  分片闭包借用的实体列表与结果槽以三路平行集合从最终位置不可变借用
  （`Box<dyn ...>` 生命周期参数化，规避省略生命周期默认 `'static`）。
- **确定性验收**：3 万实体规模走真实调度路径（真多线程、分片数随核数
  变化），逐实体 `to_bits` 与「直接按实体序应用同一变换」的参考实现
  对账；分片数/区间划分纯函数边界精确值单测（0/1024/1025/2049 × 核数
  截断）；分片系统与整体系统同批、与冲突系统异批保序（`Access` 按整
  体口径参与分批）。既有 8 项调度测试零改动全过。
- **基准（实测口径）**：criterion 新增 `whole_transform_100k` /
  `sharded_transform_100k` 对照（10 万实体，工作体 64 次 sqrt 有界收敛
  迭代）：55.5ms → 39.4ms（**1.41×**，余量被串行写回 + 列级读锁逐实体
  争用吃掉）；同机实测**纯拷贝级轻变换分片净亏**（35.3ms vs 24.0ms，
  缓冲分配 + 串行写回 + 线程创建占大头）——两个数字连同边界一起写进
  基准注释与 API 文档，不藏负收益。
- **偏离声明（常驻线程池）**：路线图原文「常驻线程池 + 数据并行分片」
  的前者 v1 不做。归因是类型系统层面而非工程惰性：常驻 worker 只能收
  `'static` 任务，而并行/分片闭包需要借用 `&World`（非 `'static`），
  安全 Rust 中非 `'static` 值无法放入任何 `'static` 容器与长命线程通信
  （channel / Mutex / Arc 全被借用检查阻断）。可行解锁路径是 `Arc<World>`
  所有权重构（App 持 `Arc<World>`、独占系统走 `Arc::get_mut`、并行任务
  捕获克隆——全 `'static`），涉及全工作区 World 所有权 API 改动，而 v1
  收益（每批省约几十微秒 spawn 开销）不抵风险，记入 §8.1 作后续评估项。

### Added（iris-exp：自由视角 / 缩放 / 角色控制——frictions ⑥ 闭合，纯游戏层）

`games/iris-exp` 此前只有动作播放（机位固定、角色钉在原地）。本轮在
`ExpPlugin` 内补齐第三人称控制装配，**引擎零改动**（`Input` 的鼠标查询
能力早已齐备——`mouse_button` / `mouse_delta` / `wheel_delta`，轨道相机
+ 缩放 + WASD 移动是纯游戏层工作，与 frictions 档案的归因一致）：

- **轨道相机**（`OrbitCamera` 资源 = yaw/pitch/dist 跨帧状态）：右键
  拖拽环绕（0.0035 rad/px，俯角钳 [−5°, 85°]）+ 滚轮指数缩放
  （`dist×=exp(−wheel·0.003)`，钳 [2.5, 18] m）+ 注视点跟随角色（胸口
  +1.5 m，注册序保证同帧读到角色新位置）。位姿几何提纯为自由函数
  `orbit_pose`（偏航后俯仰的旋转使相机前向与「eye 指向 target」解析
  一致，距离不变量 + 特殊角单测守护）；初始机位由轨道参数构造（正面
  俯 9°、dist 7.5，与旧手调机位同构观感，拖拽无跳变）。
- **角色移动**：WASD 相对相机方位（基向量由 `OrbitCamera.yaw` 解析
  求出，不读相机 Transform）、3 m/s、钳在地面 ±11 m；朝向 = `facing_yaw`
  （模型前向本地 +Z——旧注释「面向 -Z」系笔误，快照目检抓出，
  `atan2(x,z)` 解析解）+ `exp_approach` 帧率无关指数趋近（每步
  `wrap_angle` 规范状态角，残差 = 角差×`exp(−rate·Σdt)`，切分无关性
  单测守护）。
  动画播放状态随移动：移动播步行、停下定格当前帧、跳跃单次不受影响
  （播完回落按移动状态定播放）——「移动即走、停即站」，验证 `playing`
  开关的运行期切换。
- **两条硬语义**：①无输入帧（鼠标/滚轮/WASD 全静默）不写相机与角色
  `Transform`——headless / 离屏路径逐位行为与 R66b 之前一致，
  `EXP_CAM_HEAD` 诊断机位不被运行期改写，既有快照基线语义保持；
  ②由此旧测试台「无输入推进动画」失效——headless 测试与快照 example
  同轮改为注入移动键（W/S 电平按住），断言语义不变（快照帧 A 按住 S
  朝相机行走保正脸 + 迈步姿态，帧 B 松开 S 原地跳；`docs/exp.png`
  重生成）。
- **验证**：纯函数 5 项单测（orbit_pose 解析解/距离不变量、facing_yaw
  基向、shortest_angle 环绕、wrap_angle 规范、exp_approach 切分无关 +
  解析残差）；headless 新增 2 项集成（轨道相机拖拽/缩放/clamp/无输入
  恒等——不依赖资产；WASD 移动/转身/定格/跟随/clamp——依赖资产）；既有
  `determinism_bitwise` 原样通过（同输入双跑 `to_bits()` 逐位一致）。
  踩坑四则录 frictions ⑥（ResourceRef 守卫作用域、atan2 负零 ±π、
  相机跟随判定须含 WASD 电平、陈旧注释定错模型前向——验收图目检抓出）。

### Changed（iris-exp：轨道相机 Q/E 注视点高度 + 缩放范围放宽——frictions ⑥ 同轮增量）

自由视角轮（见上条 Added 基础形态）的会话前未提交增量，补记：

- **Q/E 注视点高度**：`OrbitCamera` 增 `focus_h`（Q 降 / E 按住 2.5 m/s
  线性升降，钳 [0, 4.5] m = 脚 ↔ 头顶之上留余量）；注视点 = 角色位置 +
  focus_h（`TARGET_HEIGHT` 退化为初始值）；无输入帧恒等语义同步扩为
  「鼠标 + 滚轮 + Q/E + WASD 全静默」才恒等；
- **缩放范围** [2.5, 18] → **[0.3, 60] m**：下限取相机近裁剪面
  （`z_near = 0.1`）之上留裕量——再近角色表面被近裁剪面切穿；上限取
  地面 24 m 尺度之上再留余量（全景机位）；
- **测试**：headless 增 `orbit_camera_focus_height`——E 按住 30 帧 =
  1.5 + 2.5×0.5 = 2.75 m 精确值、双端长推精确钳到 [FOCUS_H_MIN,
  FOCUS_H_MAX]、相机位姿跟 `orbit_pose` 纯函数 oracle 逐位对齐、无输入
  帧 focus_h 与相机位姿 `to_bits()` 逐位恒等。

### Fixed（测试基建：`post_render` 套件补文件内串行互斥 `GPU_SERIAL`）

R62b 轮给 shadow_v2 / transparent / decal 三个 GPU 套件补串行互斥时**漏了
`post_render.rs`**（13 项 GPU 测试全裸奔）。本轮 iris-exp 控制轮的 debug
全量门禁实锤复现：vignette 两项并行共用单适配器自旋挂死，日志 10 分钟零
推进（杀任务 → 加互斥 → 复跑 13 项 10.6 秒全过）。修法与既定处方同款：
文件内 `static GPU_SERIAL: Mutex<()>`，逐测试加锁，断言逻辑零改动；
AGENTS.md 互斥清单同步（ssr / sss / irradiance / shadow_v2 / transparent /
decal / post）。

### Fixed（R66e：数据贴图无压缩声明门——皮肤近距块状斑闭合，BC1 法线压缩病灶）

用户近距对照（Blender 原模型 vs iris-exp，Orbit 下限 0.3 m 新可达机位）：
皮肤密布**块状斑**——与 R66d ⑧ 的远距点状噪点不同形态、不同病灶
（frictions ⑨）。归因链：腿近距离屏三模式对照——**摘法线完全干净**、
摘 ORM 更糟（排除）、base 复现块斑 ⇒ 病灶在法线贴图；同图 Blender
（无压缩）渲染干净 ⇒ 引擎侧退化 = **BC1 块压缩**（R30 决策树全不透明
数据贴图命中 BC1；BC1 通道耦合端点插值对法线的伪梯度在近距 level 0
显形——R66d mip 链只救远距缩小采样）。

- **修法**：`TextureStore::declare_uncompressed` / `is_uncompressed_declared`
  声明门 = BC 决策树**前置否决**（声明句柄直走 RGBA8 + mip 链）；glTF
  加载器为法线 / ORM **自动声明**（外部资产质量优先）；
- **兼容形态**：引擎原生资产不声明 = 决策树逐位不变（内存优先）⇒ 既有
  FNV 像素基线零漂移（门禁全量复跑确认，无重锁程序）；显存估算两口径
  同步（`texture_gpu_bytes` 前置分支 + `gpu_bytes_estimate_mipped` 谓词，
  声明者按 RGBA8 链增量计）；
- **测试**：`texture.rs::uncompressed_declaration_switches_estimate_to_rgba8`
  （口径切换 + 幂等）、`gltf.rs::normal_texture_declared_uncompressed_on_load`
  （加载期声明断言）；腿近距离屏复跑块斑消失、与 Blender 观感对齐；
  `docs/exp.png` 重生成；
- **既有断言同步（本轮唯一）**：`asset_pipeline.rs` 显存口径断言原锁
  「法线归 BC1」旧决策树——加载器声明门合法改变该口径（法线 RGBA8、
  BC3 声明被无压缩声明前置否决），期望值与注释同步更新并归因；其余
  既有测试零改动全过；
- **诚实限制**（ARCHITECTURE §8.45 新增）：引擎原生数据贴图仍走 BC1
  （声明 opt-in，近距块斑机理同款——按需补声明即可，无管线改动）；
  BC5（法线专用双通道压缩）不做（体量取舍）。

### Fixed（R66d：glTF doubleSided 落地 + BC 压缩路径 mip 链——顶视角发片消失与皮肤点状噪点闭合）

用户顶视角 orbit 截图暴露两枚引擎侧既有缺口（iris-exp frictions ⑦⑧，
归因链与实验见 docs/private/iris-exp-frictions.md；均**非 R66c 资产轮
引入**——R66c 前后验收图对照同款）：

- **顶视角发片/贴片整片消失**：全仓 `doubleSided` 零命中——GLB 加载丢弃
  该字段、场景材质管线硬编码 `Cull Back`；角色 9 材质全 `doubleSided:
  true`，绕向朝内的发片在顶视角恰见背面被全剔（Blender 回导同视角完整
  ⇒ 资产无病）。修复：`MaterialDef.double_sided`（缺省 false）+ glTF 直通
  + 关卡格式 v2 可选字段（false 省略、旧档字节级稳定）；管线族扩 10 条
  `cull_mode: None` 变体（主 pass / RT / 探针捕获 × 不透明 / 透明 × 普通 /
  蒙皮）；收集期烙印进绘制项且**不进排序键**（无双面场景批划分与次序
  逐位不变）。像素级判别测试 `culling_render.rs::
  double_sided_material_renders_backface`（单面背对相机红像素恒 0 /
  双面为正）。
- **皮肤点状噪点**：三组离屏对照钉死病灶——关阴影仍在、关 `MipSettings`
  不变、**摘法线贴图完全消失**；机制 = 法线/ORM 全不透明命中 R30 BC1 决策
  树，而 R66 mip 链只覆盖 RGBA8 回退路径 ⇒ 压缩贴图恒单 mip、远距无法线
  平滑。修复：`mip_chain_bc` 逐级 RGBA8 降采样后编码 BC1/BC3 块流链
  （**止于 4×4**——wgpu 压缩 copy extent 须块宽整数倍）；`MipSettings`
  在场三路径同口径多级化、**缺席单 mip 逐位不变**（R66 同款兼容形态）；
  `gpu_bytes_estimate_mipped` 口径同步。双向阈值测试 `mip_render.rs::
  bc_path_mip_chain_suppresses_minify_aliasing`。
- **兼容与基线**：两修均为「资源/字段缺席 = 逐位不变」形态——引擎原生
  资产不声明 doubleSided、基线锁场景不带 `MipSettings` ⇒ 既有 FNV 像素
  基线零漂移（门禁全量复跑确认，无重锁程序）；culling_render 文件按
  AGENTS 铁律补 `GPU_SERIAL` 互斥（新增第四项 GPU 测试）。
- 文档同步：ARCHITECTURE §4 mip 条目与 §8.43 改写为闭合形态；frictions
  ⑦⑧ 闭合；`docs/exp.png` 重生成（黑发 + 皮肤干净，顶视角覆盖恢复由
  用户游戏内复验）。

### Fixed（iris-exp 资产：R66c GLB 重导出——头发彩虹/透明、三张 B=0 法线、耳坠群组误追踪）

用户对照 Blender 报告：GLB 往返回导后头发呈「彩虹丝 + 看似透明」。诊断链
（GLB JSON + 内嵌贴图解包 + Blender 节点图逐参数对照）钉死三处导出语义
损失，**全在资产侧、零引擎改动**（血泪细节 docs/private/04.md §6.7）：

- **头发彩虹 + 透明观感**：原始头发材质 Base Color = Mix(factor=1,
  A=发丝图集, B=纯黑)——发色本为纯黑、图集只供 alpha；导出器不追踪 Mix，
  把图集 RGB（高饱和彩丝）直写 baseColorTexture ⇒ 任何 glTF 消费者看到
  原色彩丝，叠 alpha BLEND 后发片只剩稀疏彩虹丝、「透明」是观感。烘焙
  `exp_hair_d`（纯黑 RGB + 图集 alpha）重接闭合。
- **三张法线 B=0**：原始工程法线为两通道（RG）约定，材质内以
  「invert R + Combine(B=1)」重建（头发/耳坠）或直连（身体/布料——原始
  渲染同病）；导出器不追踪重建链、直写原图 ⇒ glTF 按三通道规范解码
  z=−1、着色法线指向面内（回导深色壳状发团 + 异常高光的元凶）。烘焙
  `exp_hair_n` / `exp_ear_n` / `exp_body_n`（按原链应用 R 反转 + B=1）重接。
- **耳坠 mat_36 群组误追踪**（R66b 白布同源）：Mix MULTIPLY（色彩×信息.B）
  与 Math 自发光（遮罩×发光、原值 0）误追踪 + 金属度通道错位（原始取
  信息.R、glTF 读 B）。烘焙 `exp_ear_d` / `exp_ear_orm`（R=AO、G=粗糙、
  B=金属）+ 材质重建规范图、Emission 全断。
- **mat_16 间接引用回退**：R66b 曾烘焙「纹理作 UV」间接引用成合成图，会话
  其后回退为间接引用图（烘焙图 users=0），照导必复现；恢复烘焙图直连。

自检（GLB JSON + 内嵌贴图解包）：法线 B 通道均值全 1.0、材质 emissive 全
None、全 primitive 带 TANGENT、alphaMode 分类与动画/蒙皮完好；Blender 回导
目检与原渲染一致（黑发 + 发丝 alpha）；引擎 iris-exp 快照重生成
`docs/exp.png`，头发为黑发团、彩虹噪点消失。

### Fixed（iris-exp 资产：R66b GLB 重导出——布料纯白 / 面部不可见 / 皮肤噪点 / 挂件不跟头）

用户对照 Blender 原始渲染报告四问题。诊断链（回导 Blender 无头 EEVEE 对照 +
引擎头部特写 + 贴图解包）定位**全部为 glTF 导出语义损失，非引擎缺陷**；
修复全部在 Blender 侧（材质重接 + 贴图烘焙 + 重导出，GLB 含 TANGENT，
46.39 MB 版权资产不入库；血泪细节 docs/private/04.md §6，frictions ③④⑤
已闭合）：

- **布料纯白**：元凶 = 导出器误追踪节点群组内 Math 节点的未连线默认值
  （0.5×10），写出 `emissiveFactor=[1,1,1]` + KHR strength 9.1——引擎
  不支持 emissiveTexture 却整片应用 factor ⇒ 自发光纯白。烘焙
  `exp_cloth_d`（d×mravB 乘算 + d.alpha）+ 正确通道 `exp_cloth_orm`，
  材质重接为导出器规范图（Emission 全断）。引擎复验黑色泳衣 + 金饰 +
  蓝灰纱罩衫全部恢复。
- **皮肤密布噪点**：复合机理——导出 ORM 的 G 通道语义错位（原始
  SEPARATE_COLOR 分离的哑光粗糙度被直传原图 G≈0.23，引擎按 glTF 语义
  读成高光泽镜面）+ 全 primitive 无 TANGENT（引擎按 R42 设计用屏幕空间
  导数推 TBN，蒙皮弯曲曲面逐像素法线抖动），低粗糙度镜面 × 抖动 TBN ⇒
  密集闪点。重导出勾 `export_tangents=True` + 皮肤/面部 ORM 按原始通道
  烘焙（G = 原 R 通道）。引擎复验皮肤噪点完全消失。
- **面部五官不可见**：非贴图丢失（特写可见唇红渲染正确、回导 Blender
  五官俱全）——正常游玩距离像素太少 + 皮肤噪点淹没 + 挂件眼球位移的
  复合观感。
- **挂件不跟头**：骨骼挂接（BONE 父级）经 glTF 退化为普通节点变换且
  基准位置偏移（回导位于 −1.4）。四挂件（mesh_30/32/56/57）转头骨蒙皮
  （OBJECT 父级 + ARMATURE 修改器 + gMan Head 权重 1），含骨架缩放
  2.207 的世界空间共轭修正（S·D·S⁻¹）。蒙皮后引擎内真正跟头。

引擎侧验证（零引擎改动）：iris-exp headless 测试过、快照跨运行逐字节
一致、`docs/exp.png` 重生成。

### Added（R66：纹理 mip 链 + 各向异性过滤——`MipSettings` 资源，默认像素零变化）

全库材质纹理此前的上传路径为**单 mip**（`mip_level_count` 恒 1）且无一处
配置各向异性过滤——`games/iris-exp` 导入外部 GLB 角色实测暴露该缺口
（frictions ①）：发片材质（黑底透明 + 彩色发丝，alpha BLEND）远距重度
缩小采样时高频 alpha 逐像素翻转成噪点。混合公式核实无误（wgpu
ALPHA_BLENDING 直通 alpha 与 glTF 非预乘语义一致，不动），病灶就是
「无 mip + 无各向异性」的全局缺口。本轮修复形态与既有「资源缺席 = 关」
范式（TaaSettings / SsrSettings / OcclusionCulling）完全同款：

- **`MipSettings { anisotropy: u8 }` 资源**（`texture.rs`，Default = 8，
  入 prelude）：**资源缺席 = 与 R66 之前逐位一致**（单 mip 上传、采样器
  逐字段不变）——默认渲染像素零变化，全部既有 FNV 帧哈希锁原值通过。
  在场时 RGBA8 回退路径上传完整 CPU mip 链，采样器换三线性
  （`mipmap_filter: Linear`）+ 各向异性（u8 夹到 wgpu hal 边界 [1, 16]，
  适配器不支持时 wgpu-core 内部收敛为 1）；BC1 / BC3 压缩路径维持单 mip
  （诚实限制，§8.43）。生效时机 = 纹理首次上传（句柄值缓存不追溯）。
- **CPU 纯函数 mip 生成**（`texture.rs`：`mip_level_count` /
  `downsample_rgba8` / `mip_chain_rgba8`）：2×2 box 降采样、逐通道整数
  floor 均值（跨平台位级确定）、维数 `max(1, dim/2)` 终止于 1×1；奇数维
  末行/列不参与下一级（floor 链语义，与 GPU mip 维数公式一致），一维
  为 1 时边缘 clamp。sRGB 编码空间过滤为诚实限制（不追求线性空间）。
  链的每一级可脱离 World 精确值单测。
- **mip 感知显存估算** `TextureStore::gpu_bytes_estimate_mipped`：RGBA8
  路径补链增量（≈ 基级 +1/3），压缩路径与旧口径相同；旧
  `gpu_bytes_estimate` 签名与数值不动（既有测试零改动）。
- **GPU 验收**（新套件 `mip_render.rs`，自带文件内 `GPU_SERIAL` 互斥）：
  ①兼容——无纹理 / 1×1 纹理场景「缺席 = 在场」全帧 FNV 逐位一致
  （链与采样器改动对这两类纹理数学上不可见）；②有效性双向可判别
  （R53/R60 指标学）——512² / 4 纹素格棋盘俯视重度缩小（≈12 纹素/像素）
  ROI 亮度标准差：关 mip 实测 99.77（摩尔纹噪点）> 60 阈值、开 0.00
  （box 链均灰）< 20 阈值，两阈值独立断言；③确定性——同输入双会话
  （独立世界 + 独立渲染器）两态逐位一致；④mip 关闭帧锁 R66 文件基线
  哈希（新建，非改既有锁）。纯函数单测 4 项（含奇数尺寸边缘处理、
  1×1 终止、链长、估算口径）。验收图 `docs/mip.png`（关/开对照离屏图）。
- **iris-exp 接入**：`ExpPlugin` 插入 `MipSettings { anisotropy: 8 }`
  （headless 路径无渲染器，无害）+ 光照调参（frictions ②，intensity
  2.6→1.4、ambient 0.22/0.24/0.28→0.11/0.12/0.14，白服区域实测亮度
  ≈223→≈184，大面积过曝缓解）；`docs/exp.png` 重生成，快照跨运行
  逐字节一致（sha256 对账），mip 生效性经「资源在场/缺席」哈希差对照
  证实。`assets/exp_weiqing.glb` 缺席时全部路径照常跳过。

### Added（iris-exp：实验性项目——外部 GLB 角色 + Mixamo 重定向动作导入验证）

新建 `games/iris-exp`（最小实验环境，非完整游戏）：验证外部建模管线
（Blender 4.x 导出 GLB）产出的蒙皮角色在引擎内的渲染 / 动画 / 动作切换。
资产（`assets/exp_weiqing.glb`，32MB 版权模型）不入库，`.gitignore` 按名字
精确忽略，缺席时插件降级无角色模式（诚实告警，测试跳过）。

- **装配**：4 蒙皮网格 + 4 骨骼挂接刚性挂件 + 87 骨骨架；动画按名加载
  （`Mixamo_Walk` 循环 / `Mixamo_Jump` 单次播完自动回落步行——用非循环
  播放结束的 `playing` 落 `false` 做接线，零状态机）。
- **验证**（headless 确定性 + 离屏快照双轨）：剪辑名按名可查、87 具名
  节点在位；步行推进采样关节逐帧变化；跳跃骨盆 z 起伏 >1 m（蓄力 1.08 /
  腾空 3.36）；播完自动回落步行；同输入两次推进关节位姿 `to_bits()`
  逐位一致；离屏快照跨运行逐字节一致，验收图 `docs/exp.png`；渲染实测
  蒙皮 / 阴影 / 贴图正常，角色面向 +Z（相机已对位）。
- **暴露的引擎侧缺口**独立成档（`docs/private/iris-exp-frictions.md`，
  「现象 → 归因 → 绕行 → 建议修法」）：① alpha BLEND 发片材质噪点、
  ② 场景观感偏亮——两条均已于 R66 闭合（见上）。

## [0.1.1] - 2026-09-12

### Changed（仓库结构：六个 dogfooding 游戏 crate 迁入根级 `games/`）

`iris-game / iris-arena / iris-bastion / iris-ascent / iris-infiltration / iris-assault`
从 `crates/` 迁出到工作区根下的 `games/`，引擎 crate 与游戏 crate 在目录层面
分离（`git mv`，文件内容除文档注释中的路径提法外零改动）。纯结构变更：

- workspace `members` 与 `workspace.dependencies` 的路径同步改为 `games/...`；
- 资产解析不受影响——`iris_engine::asset_path` 的工作区根来自 **iris-engine
  自身** `CARGO_MANIFEST_DIR` 上溯两级（`crates/iris-engine` 未动），与游戏
  crate 位置无关；`iris-assault` 音频 fixture 的 `../../crates/...` 回退在新
  布局下（`games/iris-assault` 上溯两级）仍指向工作区根，无需修改；
- README 六个游戏小节、ARCHITECTURE 路径提法、iris-infiltration 文档注释
  同步更新；CHANGELOG 历史条目中的旧路径保留（历史记录不改写）。

### Fixed（测试基建：三个 GPU 套件补文件内串行互斥 `GPU_SERIAL`）

全量门禁复跑中三个 iris-render 测试套件在**同文件多测试并行**下共用单 GPU
适配器互相争用（R62b 记录过的病灶，本轮在 debug 下首次复现，症状谱更宽）：
`shadow_v2_render` 两轮自旋 10 分钟零推进、`transparent_render` 空转 18 分钟、
`decal_render` 直接段错误（0xc0000005）。归因证据：`shadow_v2_render` 串行
单跑 7 项仅 10.1 秒全过；cargo test 的测试二进制之间本就串行执行，争用只来自
同文件并行。按 R62b 既定修法给三个文件加 `static GPU_SERIAL: Mutex<()>`
（参照 `ssr_render.rs`），修复后全量 debug 门禁一次通过（1178 passed 与基线
一致）。测试代码断言逻辑零改动。

### Changed（角色建模迭代轮二：四肢塑形 + 关节铰接 + 头面重建）

上一轮把「手足」推到可用，本轮按用户给定次序补齐**四肢 → 躯体 → 关节 → 衣物 →
细节 → 头面**。三件核心：①四肢与躯干从「锥管 / 直筒」变成**带肌肉起伏与 S 曲线
的放样**（截面中心可离轴）；②肘 / 膝从「一块壳」变成**多片铰接护具**；③头盔与
面部从「一颗蛋 + 一条黑带」变成**造型 + 结构**。网格 24479/22466 →
**31065 顶点 / 28974 三角形**（仍是一张蒙皮网格、一次 draw），引擎零改动。

- **四肢塑形（新原语 limb_shaped）**：剖面里多给两个数——截面中心相对轴线的
  **左偏 / 后偏**。真实四肢的侧影全在前后方向上：股四头肌前凸（后偏 −0.006）、
  腘绳肌后鼓、腓肠肌在膝下 0.26 处最鼓（+0.008）、踝上收成跟腱、肱二头肌 /
  肱三头肌分列前后、肱桡肌在肘下鼓起。放样行数 4~6 → 7~9、分段 12/14 → 16，
  侧影因此不再是直线。裤脚（堆在靴口）的**半径与偏移都跟着腓肠肌走**，
  否则小腿会从裤脚里穿出来。
- **护膝曾被裤脚埋掉（真 bug）**：第一版「膝下裤脚」是从膝盖往下 0.19 m 的整圈
  放样（半深 0.094），比护膝（半深 0.079）还大 ⇒ **护膝整片在裤腿里，等于不
  存在**。现在裤脚只盖小腿下段（y 0.324~0.252，落在靴口滚边之上），护膝第一次
  真正露出来。
- **两圈悬空的护膝绑带（真 bug）**：ring_shell 的行只给 (y, hx, hz)，中心恒在
  x = 0 —— 旧代码没给平移，两条绑带其实悬在**两腿之间的空处**，且左右腿各生成
  一份逐位重合的几何（z-fighting）。现在改用开弧壳绑在护膝上（弧壳永远不会像
  整圈那样在小腿上悬空）。
- **关节铰接（push_joints）**：肘三件（上片 / 下片 / 肘尖凸台）+ 铆钉 ×2，
  膝五件（护膝弧壳 / 髌骨凸台 / 侧翼 ×2 / 绑带 ×2）+ 铆钉 ×2。关节件全部改走
  blend_chain（旧护膝 / 肘甲是 rigid，弯曲时与肢体脱节）；关节**内侧**（肘前）
  加两道压缩褶——外侧是拉伸面，只在内侧起皱。
- **躯体 S 曲线**：躯干 14 个截面全部允许前后偏移——臀后凸（+0.008）、腰内收
  （−0.004）、胸椎后凸（+0.002）、胸骨前凸（−0.006）。
- **衣物结构线（push_uniform）**：背部育克缝（**指数必须与躯干同为 2.6**，否则
  超椭圆的「角」比椭圆鼓，弧壳两端会陷进躯干里）、臀部后袋 ×2、下摆抽绳 ×2、
  肩部帆布徽记 ×2。
- **头部重建**：颅顶由「各层 hz ≈ hx 的蛋」改成 **hz > hx 的 FAST 型盔形**
  （前后更长、两侧更直），加三段颅顶中脊、通风窗 ×4（先在盔面截面上取点、
  再沿法向外移）、**扫掠侧导轨**（旧版是两片平贴的板，近景读作「贴纸」）+
  端头铆钉 ×4、绕盔后的盔带 + 猫眼片；颅顶与下半盔壳的 UV 从盒投改柱面投
  （球面上的盒投接缝是若干条弧线，柱面只有几条竖直缝）。
- **面部重建**：护目镜从「一整条暗色弧带」改成**镜框弧壳 + 左右镜片各一片 +
  鼻梁桥 + 侧扣 + 侧扣到头盔带的连接带**；面罩加鼻梁脊、颊部滤罐 ×2（**漆面
  金属而非橡胶**——整张脸若全是黑材质，近景就是一团黑）、下巴呼气阀、口栅 ×3。
- **材质**：面罩玻璃基色 0.020 → 0.108、粗糙度 0.055 → 0.13（金属度 1 + 近黑
  基色在没有环境反射时是**纯黑洞**，头部特写实测整张脸一团黑）；手套基色
  0.072 → 0.098；橡胶法线 0.80 → 0.55（面罩大面积上是低频抖动，bump 过大在
  近景读成波纹）。
- **预览机位**：头部与躯干特写原先都在 **+Z（背面）**——头部那张拍的是盔顶与
  后配重包，护目镜 / 面罩 / 帽檐全在取景之外，等于没检查；现在两张都改到面前，
  躯干特写退到 2.05 m（1.45 m 时步枪横在胸前挡住大半背心）。

诚实限制（未变）：图集仍是 512²（16 × 128² 分区、无 mipmap）；布料没有姿态空间
褶皱，关节弯曲仍靠权重混合与装备遮挡；头盔的柱面 UV 仍有 3 条竖直图案接缝。

### Changed（角色建模迭代轮：手足重建 + 部件法线纠向 + 材质单位复检）

上一轮把角色换成蒙皮网格后，目检的结论仍是「粗糙」，且**手足最粗**。本轮按
「手足 → 四肢 → 躯体 → 关节 → 衣物 → 细节 → 头面」的次序先验收前两项，并顺手
挖出一个**影响整身光照的真 bug**（20 个部件法线朝内）。网格 6756/12184 →
**24479 顶点 / 22466 三角形**（仍是一张网格、一次 draw），引擎零改动。

- **手重建（本轮第一优先）**：第一版的指长只有 0.030 m（真人中指 0.080）⇒ 指尖
  够不到掌底，整只手读作连指手套。现在掌长 0.104 / 指长 0.064~0.080，四指按
  **绕掌心的圆弧**屈曲（总屈曲角 1.30~1.42 rad，半径 R = L/θ，路径点落在弧上 ⇒
  指节自然连成弧），指间留缝、关节处剖面半径内收（0.90~0.93 倍）起皱、指背
  掌指关节与近节指间**各一片护片**（护片法向由切线绕弧推出，与 sweep 的截面基
  同构）；拇指两节对掌 + 掌背鱼际隆起；腕口加护套环 + 背面织带束带 + 手背掌骨
  护片。
- **战靴重建**：鞋底改成**趾→跟一体放样**（跟 30 mm 厚、足弓抬起 15 mm、前掌
  最宽、趾尖上翘），鞋帮楦体（趾低窄 → 踝高宽）、鞋头包（190° 包住趾部）、
  后跟杯（绕 Y 轴 166° 包跟腱）、靴筒（比小腿粗 6 mm，裤脚罩在靴口上）、靴口
  滚边、**鞋舌 + 鞋带 ×4 + 黄铜鞋眼 ×8**、踝束带 + 扣片、后提环、外侧护踝片。
  第一版那双「打光发灰的圆疙瘩」的两个成因（法线全场朝内 + 靴筒半宽 0.086 比
  小腿还粗）都已消除。
- **部件法线纠向（真 bug）**：放样 / 旋转体 / 超椭球的环绕方向取决于「截面基
  (right, up) 与行方向是否成右手系（right × up = 行方向）」，而这是调用方给的
  ——写反的壳在光照下整片发灰。小圆柱探针实测鞋楦三件的有向体积为 **−5.81e-4**
  （应为 +5.81e-4）；把 Assembly::add 的逐件有向体积诊断跑满整身，**抓到 20 个
  朝内部件**（鞋楦三件、双肩三角肌、双掌鱼际、头盔耳罩、脸侧滤罐、腰侧挂具、
  腕环、刀镡……），且**左右未必同病**（镜像部件行方向相反）。修法在**基元层**：
  geom::orient_outward 按首个有效面的几何统一纠向（与 arc_shell 同一策略，
  端面这类判不出朝向的面按与径向的夹角跳过），Assembly::add 保留「闭合壳有向
  体积必须为正」的不变量兜底——调用方不必再逐件核对右手系。
- **材质单位复检**：上一轮把法线贴图的量纲从「除以 UV 步长」改为「跨 2 纹素高度
  差」后，bump 系数仍是按旧量纲调的 ⇒ 近景把细粒读成大块斑纹（靴子特写像玳瑁
  皮）。11 个分区的 bump 全部减半（布料 2.00 → 0.90、皮革 1.30 → 0.55、护甲
  1.60 → 0.85 …），皮革高度场向细粒倾斜（低频折痕 0.40 → 0.22）、明暗幅度
  14% → 8%；**枪械金属改成磷化黑**（线性 0.072 → 0.035 量级）——它此前与护甲钢
  的基色距离只有 1.65，tiles_are_visually_distinct 实测判「材质退化」，同时也是
  「整身一片橄榄绿」的成因之一；手套提亮一档（0.054 → 0.070）让手指在近景读得
  出来。
- **织物细节 push_fabric**：四肢加 8 道堆褶 / 束带（上臂中段、肘下、大腿中段
  织带、膝上、膝下、小腿中段——比肢体半径大 3~6 mm 的短环，两端收口），躯干加
  立领一圈。光滑锥管的「塑料管感」由此破掉。
- **预览机位**：4 个全身 + 6 个特写（头 / 躯干 / 双手 / 双靴），**手与足各给两个
  特写**；修掉两处取景错误——「front」机位原先在 +Z 其实拍的是背面（角色面朝
  −Z），主光跟着反、正面细节全糊在阴影里；手部特写由**骨节点实测世界位置**
  推出（写死坐标必然拍空，第一版 hand_right 只拍到弹匣与空景）。

诚实限制（未变）：图集仍是 512²（16 × 128² 分区、无 mipmap）；头部与面部本轮
**未动**（排在用户给定次序的最后，下一轮处理）；布料没有姿态空间褶皱，关节弯曲
仍靠权重混合与装备遮挡。

### Changed（视觉上限轮：Iris Assault 角色与庭院重建——程序化蒙皮人形 + 程序化建筑庭院）

把「写实战斗庭院」的**内容侧**推到引擎当前能力的上限：角色从 22 个骨节点上
逐块挂的缩放立方体，换成**一张 GPU 蒙皮网格**（6756 顶点 / 12184 三角形 /
22 骨 + 1 网格节点 / 单材质 512² 程序化图集 = 单次 draw）；庭院从 15 个图元实体
换成**程序化建筑 GLB**（42980 三角形 / 9 材质）。**引擎零改动**——全部工作在本
crate 的 `geom` / `soldier` / `soldier_tex` / `scenery` / `scenery_tex` 五个新模块、
资产生成器与插件装配内完成。

- **新模块 `geom`**（程序化几何工具，纯函数 · 确定性）：超椭球（指数 2 = 椭球、
  4~8 = 圆角盒、1 = 菱形截面）、截面放样 `loft` / 直线轴 `tube`、旋转体 `lathe`；
  `Piece` / `Assembly` 承载「局部建模 → 盒投 / 柱面 UV → 刚体变换 → 蒙皮权重
  （刚性 / 沿骨链平滑混合）」装配流水线，输出带 TANGENT 与 JOINTS/WEIGHTS 的
  `MeshData`；9 项单测（逐位确定性 / 包络 / 退化三角形 / UV 落在分区内 / 权重
  归一化 / 切线正交）。
- **新模块 `soldier`**：士兵模型——身体（躯干 / 四肢放样，超椭圆截面逐段变半径、
  三角肌把上臂接到躯干）、装备（头盔穹顶（旋转体）+ 面罩 + 下颌板 + 耳罩 + 夜视座、
  护甲板 / 肩甲 / 护腕 / 肘垫 / 大腿甲 / 膝垫、弹匣包 ×3 + 侧包 ×2、腰带 + 带扣 +
  水壶 + 匕首鞘）、武器（刀：配重 / 缠柄 / 护手 + 端头 / 双刃菱形截面刀身；步枪：
  机匣 / 导轨 / 枪管 / 制退器 / 护木 / 弹匣 / 握把 / 枪托 / 光学瞄具 + 前后镜片）。
  **权重按骨链投影分配**（脊柱 / 腿 / 臂三条链，关节两侧平滑混合），装备与武器
  刚性绑定。
- **新模块 `soldier_tex`**：512² 士兵图集（16 个 128² 分区：布料 ×2 / 护甲 / 头盔
  漆面 / 皮革 / 战靴 / 刀身钢 / 枪械金属 / 缠柄 / 手套 / 面罩玻璃 / 黄铜 / 织带 /
  橡胶 / 内衬 / 帆布包），base + normal + ORM 三通道一次生成；可平铺值噪声 +
  高度场中心差分（与引擎 `brick_normal_orm` 同口径）。
- **新模块 `scenery` + `scenery_tex`**：庭院建筑——铺地（石板网格画在贴图里：
  290 块独立石板几何要 ~19k 三角形，贴图方案 2 个三角形/格）、墙体（背板 + 面板
  拆出**拱形壁龛**）、壁柱 / 柱头 / 檐口 / 雉堞、多立克石柱（方形柱础 + 收分柱身 +
  钟形柱头 + 顶板）、三层喷泉与水面、悬挂火盆（墙托 + 斜撑 + 铁链 + 铜盆 + 炭火）、
  壁龛雕像、木门与铁箍、旗帜；9 材质可平铺贴图。
- **关卡档职责分工**：`assault_courtyard.level.json` 保留**碰撞 + 灯光 + 粒子**权威
  （`level_roundtrip.rs` 零改动全过），视觉由 `assets/assault_courtyard.glb` 承担；
  插件在 GLB 装配成功后按名隐藏档里的可见图元（失败则保持图元可见 = 可玩的降级）。
- **碰撞契约与守护测试**：视觉几何不得越过静态碰撞体表面 8 cm 以上（浅浮雕式
  凸出允许），2.10 m 以上（角色头顶）自由——`geometry_stays_clear_of_player_space`
  逐顶点卡这道关，喷泉（球体）/ 石柱（0.9×0.9 盒）另有专项外形测试。**实测抓到
  两处**：对面墙用纯平移复制会把 0.6 m 厚的墙体推进场地 0.61 m（改绕 Y 轴 180°
  旋转）、壁龛雕像摆在龛口会凸出场地 14 cm（改立在龛底）。
- **敌人配色**：蒙皮后网格挂在模型根上，R48 的「逐骨节点覆写材质」路径失效——
  改为对模型根 / 枪节点的 primitive 乘暖色因子（乘法而非替换：图集已有材质层次，
  替换会把布料 / 钢刃 / 皮革压成同一块红）。
- **新工具**：`examples/soldier_preview.rs`（六机位角色特写，走真实动画姿势——绑定
  姿势的「T 字站桩」不是玩家看到的样子）、`examples/courtyard_preview.rs`（四机位建筑
  目检）、`examples/texture_dump.rs`（程序化贴图导出 PPM，隔离排查材质问题）。
- **验收图**：`docs/soldier.png`（角色六机位）、`docs/courtyard.png`（庭院四机位：
  英雄视角 / 拱形壁龛与雕像 / 喷泉 / 俯瞰）、`docs/assault.png`（游戏内战斗中段两帧，重渲）。
- **资产体积**：`assets/assault_bot.glb` 241 KB → 2.30 MB、新增
  `assets/assault_courtyard.glb` 7.25 MB；`assets/assault.ipak` 265 KB → 9.57 MB。
  诚实说明：程序化噪声贴图的 PNG 熵很高（图集三通道 ≈ 1.8 MB），要压体积的方向是
  降分辨率或离线压缩容器，不是减材质。
- **引擎摩擦（ARCHITECTURE §8.42 新增）**：`set_subtree_hidden` 依赖 `Visibility` 组件
  存储**已注册**，而 `&World` 无法注册新组件类型——「先隐藏旧图元、后创建带
  `Visibility` 的角色」的顺序下插入静默失败（只 warn），旧图元继续渲染并与新建筑
  z-fighting（实测：4 m 宽的旧砖块墙盖住整面新墙）。本 crate 的修法是让庭院根实体
  显式挂 `Visibility`（同时完成注册）。

### Added（R65：行为树 + 黑板 + 感知事件——AI 上限，路线图 v4 · W7 首轮）

敌人 AI 的游戏侧手写状态机在能力变多后必然爆炸（状态 × 转移的笛卡尔增长，
最初评估的第二个瓶颈）。R65 在 iris-core 落引擎级最小骨架，并以
iris-infiltration 守卫迁移做实战验收：**结构是纯数据、决策带记忆、通信只有
黑板、感知是纯函数 + 事件**。设计记录 ARCHITECTURE §3.11（新增）、诚实限制
§8.41（新增）。**不做任何 git 写操作；既有测试零改动全过。**

- **新模块 `iris-core/behavior`**（零渲染依赖）：`BehaviorTree` = `NodeSpec`
  嵌套规格**前序展开**成扁平 `Node` 数组（子节点只存 `NodeId` 下标——运行期
  零动态分发、遍历期零分配、结构可序列化）；节点族八种（`Sequence` /
  `Selector` / `Parallel` 成功数×失败数双阈值失败优先 / `Inverter` / `Repeat`
  / `Cooldown` / `Condition` / `Action`），叶子由游戏侧按**具名动作 id**
  （`ActionId` = `&'static str` 新类型）经 `BehaviorContext` 分发，引擎不认识
  任何游戏动作；`BehaviorState` 逐节点记忆 ⇒ 上帧 `Running` 的节点下帧**从
  记忆处恢复**（复合子游标 / 重复计数 / 剩余冷却 / 并行已终结位图）；
  `Blackboard`（`BTreeMap<String, BlackValue>`，值五型）是感知与动作之间**唯一**
  的通信通道，不做跨代理共享；`BehaviorAgent` = 树 + 运行态 + 黑板三合一组件
  （despawn 即消亡，无手工清理）；系统体 `tick_behavior_agents`（Update 阶段
  并行系统，迭代序 = 实体 id 升序）。
- **新模块 `iris-core/perception`**：纯函数 `in_view_cone`（三维距离 + 水平朝向
  半角点积）/ `has_line_of_sight`（线段 × `StaticObstacle`，复用 physics 射线）
  / `can_see` / `within_hearing`（纯距离、无视线——隔墙听得见是有意取舍）+
  `Perception` / `PerceptionTarget` 组件（视距 / 半角余弦 / 听觉半径 / 眼位
  偏移字段化）+ `perception_system`（扫描 → 差分 → `Events<PerceptionEvent>`
  Seen / Lost / Heard（双代缓冲先例）+ 运行态写回）；三份收集列表按实体 id
  升序 ⇒ 输出与 spawn 顺序无关。
- **iris-infiltration 守卫迁移（行为等价硬验收）**：守卫驱动改写为行为树
  （顶层 Selector「击晕冻结 / 感知 + 决策」，决策 Selector「逃跑 → 追逐 →
  支援 → 巡逻」）；**迁移前后全状态轨迹 FNV 哈希逐位相同**——两个场景：自动
  玩家全流程 2993 帧（`c71ed50cf49c6d8c`）与发现 → 追逐 → 放弃 → 击晕 900 帧
  （`8a0d0798f55ec8f2`），覆盖玩家运动 / 巡逻 / 警觉 / 发现 / 重寻路 / 放弃 /
  击晕冻结 / 开门 / 拾取 / 胜利全分支。
- **两个演示能力（默认关 = 能力缺席即旧行为）**：①**逃跑**（低血量 `health ≤
  FLEE_HEALTH` 或被逼近「看得见且 ≤ `FLEE_RADIUS`」→ 走到距玩家最远的巡逻
  路点，`FLEE_SECS` 粘性防视线抖动振荡）；②**呼叫增援**（发现玩家 →
  `InfiltrationEvent::GuardAlert` → 半径内其他守卫进入支援态；支援态**不进**
  `GuardState` 枚举，由黑板 `guard.support_timer` / `guard.alert_pos` 承载——
  这正是行为树相对状态机的扩展力）。轨迹与状态断言在新增
  `crates/iris-infiltration/tests/guard_behavior.rs`（7 项，含乱序 spawn / 同输入
  逐位确定性 / 树结构优先级钉住）。
- **测试**：iris-core 新增 29 项（节点语义逐条 / Running 记忆恢复 / 黑板键序
  与类型语义 / 构造期 fail-loud / 同输入双跑逐位 / ECS 乱序 spawn；感知纯函数
  解析对照（锥内 / 锥外 / 闭边界 / 遮挡 / 旋转感知 / 听觉半径）+ 差分
  Seen→Lost→再 Seen + 双代窗口 + 乱序 spawn 无关），iris-infiltration 新增
  7 项；六项门禁全绿（debug / release 全量 + native / wasm clippy）。


### Added（R64：光照探针 GI——烘焙 irradiance 探针云 + 四面体重心插值，路线图 v4 · W6）

补上**间接漫反射**的最后一块拼图：光照贴图（R52c）只服务静态几何、反射
探针 / SSR 只服务镜面，动态物体的间接漫反射此前只能吃全局 IBL 的天空辐照
（室内阴影里的角色被天空色污染成冷蓝）。R64 烘焙**irradiance 探针云**并在
运行时按位置做**四面体内重心插值**，取代无光照贴图实体的全局 env 漫反射项。

- **新模块 `iris-render/irradiance»**（纯 CPU，零 GPU 依赖）：`IrradianceProbe»
  组件 + `bake_irradiance_probes» / `install_irradiance_probes» +
  `IrradianceProbeSet» 资源 + `Tetrahedralization» 纯函数；自研**确定性
  Delaunay 四面体化**（增量 Bowyer-Watson：BFS 连通空腔 + 幽灵胞元凸包接入 +
  最胖初始单纯形 + 三线性哈希抖动 ±1e-6 + 抖动幅度阶梯重试），零第三方依赖。
- **烘焙源复用 `bake_sample»**（与光照贴图同一 BVH / 采样 / 三项口径），存
  `BakeSample::indirect»（天空含 AO + 一次反弹含色溢）⇒ 与光照贴图视觉同源是
  机制保证；**探针朝向 = 六轴平均**（无方向 RGB 标量的方向零阶平均，对均匀场
  精确）。
- **求值 = 逐实体 CPU 侧**（实体根 `GlobalTransform» 位置 → 一次插值 →
  实例载荷），**拒绝逐片元 GPU 插值**（成本差 4–5 个数量级，箱庭尺度的角色
  本来就是逐实体常数）。
- **注入载体 `InstanceRaw::probe»**（128 B/实例，顶点属性 loc14，显存 +16 B/实例）：
  xyz 存 E/π（与 `irradiance_map» 采样值同口径 ⇒ 着色器零换算）、w = 标志位；
  `standard.wgsl» 的间接漫反射项写成
  `select(env_irradiance, in.probe.rgb, in.probe.w > 0.5)»。
- **兼容铁律**：探针资源缺席 / 空集 / 实体挂光照贴图 ⇒ 载荷恒 `[0,0,0,0]»，
  `select» 返回纹理采样值的**同一串位**——全部既有 GPU FNV 基线**原值通过**
  （本轮不重锁），新增两条 R63 场景基线锁（无贴图 / 有贴图）。
- **验收图** `docs/light_probes.png»（4 排 × 2 列：①室内阴影区角色 关|开
  ②亮区|阴影区 ③与静态光照贴图的融合（近景）④探针布置可视化），由
  `examples/light_probe_snapshot.rs» 生成（内含硬断言，跨运行逐字节一致）。
  实测角色亮度 200.5 → 143.8（72%）、蓝红偏置向房间参考收敛、角色外像素变化 0。
- **诚实限制**（§8.40）：逐片元插值 / SH 方向性探针 / 可见性加权 / 磁盘持久化 /
  编辑器集成 / 动态重烘焙（拒绝清单）；几何为「真值 ± 1 µm」（抖动法固有）；
  极扁平点云可能整体退化为空结构（阶梯重试后回退全局 env）；包外无距离衰减。


### Added（R63：预积分皮肤次表面散射——材质上限，路线图 v4 · W6）

皮肤在终结线处是硬边漫反射，光在皮肉里的穿透被完全忽略。R63 给写实角色
的肤质材质一条**预积分次表面散射**通路（Penner 2011 近似）：程序化预计算
一张 (NdotL × 曲率尺度) 的漫反射 LUT，肤质材质的**直射漫反射**用它取代
几何 `NdotL` 因子——光「包裹」进终结线背光侧。**无屏空间模糊、无多 pass、
无厚度图、无透射**。设计记录 ARCHITECTURE §4.32（新增）、§4.3（同步）、
诚实限制 §8.39（新增）。**不做任何 git 写操作；全部既有 GPU FNV 基线原值
通过（本轮不重锁）；`SCENE_FORMAT_VERSION` 保持 2。**

- **积分式（单一来源 `iris_render::sss`）**：球冠法线分布的投影面积加权
  Lambert 预积分 `W(c, t) = N(c, α(t)) / D(α)`——方位角积分有闭式解，只剩
  极角一维复合 Simpson（128 步，与 20000 步参考偏差 9.0e-7）。**积分在 f64
  累加、末端落 f32**（f32 累加 129 项会攒出 1.19e-7 求和噪声，足以在
  「对 c / 对 t 单调」这条解析性质上制造亚 ulp 反向抖动）。**无 RNG、无全局
  状态**：同输入两次生成逐位同一输出（`to_bits` 断言）。
- **按 `D(α)` 归一（c = 1 为基准）**：`W(1, t) ≡ 1` ⇒ strength 只在终结线及
  背光侧加光，受光面观感与关闭时一致。代价是终结线附近**增能**（非严格
  能量守恒，Penner 2011 的既有取舍）→ 诚实清单。
- **LUT**：`Rgba16Float` 64×64 常驻纹理（渲染器初始化时一次生成上传），
  行 = 曲率、列 = NdotL，**纹素中心对齐** ⇒ 端点 W(−1) = 0 / W(1) = 1
  逐位精确（不会出现「全照面被 LUT 抹暗 1.6%」的端点截断伪影）。精度依据：
  双线性重建误差 `t ≥ 0.05` 时 ≤ 1.4e-3（低于 8-bit 量化步长）、`t → 0` 的
  Lambert 折点 ≤ 7.9e-3；选 16-bit float 而非 8-bit unorm 是因为平缓的
  终结线斜坡用 8-bit 会有 1/255 量化台阶（R42/R52c 的同类先例）。
- **作用面 = 只改直射漫反射**：实现形态是**相加的补项**
  `(1 − metallic) · albedo/π · radiance · (nol_sss − nol)`（strength 线性
  混合），**镜面项留在几何 NdotL 上**（高光仍遵守几何反射定律）；**金属无
  SSS 走权重不走分支**（metallic = 1 ⇒ 权重 = 0 ⇒ 补项恒 ±0.0 ⇒ GPU 断言
  「strength 0/1 整帧逐位一致」）；**点光同样适用**（终结线软化方向无关；
  `pl` 已含窗口衰减与 cubemap 阴影，补项自动继承）。IBL / 光照贴图间接项 /
  自发光 / AO 一概不参与。
- **实测陷阱（1 LSB 级基线漂移的归因，写进 ARCHITECTURE §8.39⑦）**：补项
  初版按 `kd = (1 − F)(1 − metallic)` 写，在辅助函数里重算了 `pbr_light` 的
  `normalize(v + l)` / `fresnel_schlick` 子表达式——**改变了编译器在
  `pbr_light` 内的 CSE / 内联决策，进而改变其 FMA 收缩选择**，实测
  `shadow_v2_render.rs` 的接触阴影对照场景出现 **3 个字节 ±1 LSB** 漂移
  （320×180×4 中的 3 个通道，全在接触阴影边缘的 PCF 临界像素；与 SSS 逻辑
  无关，strength = 0 时补项根本不执行）。去掉子表达式复用（能量权重只留
  `1 − metallic`，绝缘体漫反射权重比 `kd` 高约 4%，对近似补项在噪声量级）
  后该基线**逐位复原**——本轮**不重锁任何既有 FNV 基线**。教训：新增直射项
  时不要在辅助函数里重算 `pbr_light` 的子表达式。
- **阴影协同（乘位论证）**：补项与主体项消费**同一个已乘完阴影的辐亮度**
  ——方向光 `light_color × shadow_factor`（CSM + PCF）、点光
  `light_color × atten × shadow`（cubemap）——不存在第二条绕过阴影的通路，
  背光侧的包裹绝不「晒穿」投影阴影。点光 cubemap 阴影是硬 0 ⇒ 阴影区像素与
  strength **逐位无关**（GPU 硬断言：掩码 9206 px 内差异 0 字节）；CSM 的
  阴影暗部保留系数使直射不为 0，「贡献 = 0」的逐位断言落在点光路径上
  （偏离声明见 REPORT 与 §4.32）。
- **光照贴图协同**：SSS 补项只进直射项，与 R52c 的 `select` 间接分支正交
  ⇒ 有 / 无图集两条路径下 SSS 的几何行为一致（GPU 测试：背光侧受光像素与
  亮度零点位置对照，零点差 1 px）。
- **参数载体与兼容铁律**：`MaterialDef` 加 `subsurface_strength`（缺省 0）
  + `subsurface_scale`（缺省 0.1 m = 头 / 前臂量级）。落地前核实全仓 68 处
  `MaterialDef {` 字面量：**49 处走 `..MaterialDef::default()` 更新语法、
  15 处是函数签名 / 结构体定义、2 处（glTF 导入与场景段组装）全字段枚举**
  ——加字段编译零破坏，只需为那 2 处补缺省值。**strength = 0 时着色器的
  `if (material.sss.x > 0.0)`（uniform 条件）整段不进入、一个算术都不多做**
  ——比 `mix(a, b, 0) = a·1 + b·0` 的浮点恒等式论证更强（后者在 `a = −0.0`
  时翻符号），是 R52c `select` 先例的强化形态。
- **绑定与布局**：LUT 纹理走材质组 binding 7（追加在现有最大 binding 之后，
  R61/R62a/R62b 的「不扩 Globals」先例），采样复用 binding 6 的 ClampToEdge
  双线性采样器（不新增采样器槽位）；`MaterialParams` 扩 `vec4 sss`，
  Rust 侧 `MaterialRaw` 64 B + `size_of` 编译期断言。
- **序列化**：`MaterialSection` 加两个**可选**字段，缺省值省略（强度缺省与
  尺度缺省**各自独立**判定——强度 0 但尺度非缺省时仍写出，「关掉 SSS 不该
  丢失已调好的尺度」）⇒ v2 旧档直读零变化、重存**逐字节不变**，
  `SCENE_FORMAT_VERSION` 保持 2；材质去重键 `material_key` 同步覆盖新字段
  （漏掉会让「只差 SSS 参数」的两个材质被错误合并）。
- **测试（九 + 六）**：LUT 纯函数 9 项单测（逐位可复现 / 端点解析对照 /
  零点 `−sin α` 解析对照 / 双轴单调 / 方位角镜像对称逐位 / 二维中点法独立
  参考对照 / 重建误差上界 / 调参表数字 / 平面退化逐位等于经典 Lambert）；
  GPU 集成 6 项（R63 基线 FNV 锁 / **终结线三指标**——背光侧受光像素 0 → 68、
  亮度零点 127 → 34 对齐解析零点 24.1 的有界断言、2%..98% 过渡带 65 → 136 px
  且 ≥ 1.30× / 金属逐位零贡献 + 绝缘体对照 / **点光投影阴影内逐位无关** /
  光照贴图双路径一致 / 双会话 6 帧逐帧逐位一致）。
- **验收图**：`docs/sss.png`（5 排 × 2 列：终结线 关|开、尺度 0.05|0.10、
  尺度 0.30|LUT 本体、阴影区 关|开、人形角色（`assault_bot.glb` 皮肤化覆写）
  关|开），由 `examples/sss_snapshot.rs` 生成（内含硬断言，跨运行逐字节一致）。
- **v1 不做（诚实清单 §8.39）**：屏空间 SSS（separable SSS 模糊 pass）；
  透射 / 厚度图 / 背光穿透光晕；各向异性高光；头发 / 布料专用模型；
  逐顶点曲率烘焙；LUT 运行时热替换。

### Added（R62b：SSR 屏幕空间反射——前向管线 × 上帧颜色/深度的射线步进，路线图 v4 · W6）

探针（R62a）是静态烘焙的，反射不到后摆进场景的物体与移动的物体。SSR 在
**前向片元着色内**（`standard.wgsl` fs_main）对**上一帧**的颜色与深度做
屏幕空间射线步进：命中 ⇒ 反射屏幕内内容（含一切动态物体）；未命中 ⇒
回退探针 / env（R62a 通路原值）。设计记录 ARCHITECTURE §4.31（新增）、
§4.30（诚实清单移除 SSR，指回 §4.31）、§8.31④ / §8.37⑩（同步）；
诚实限制 §8.38（新增）。**不做任何 git 写操作；既有 GPU FNV 基线全部
原值通过（本轮不重锁）。**

- **步进算法**：世界空间反射射线 32 步线性等分（首样本半步长防自命中）
  + 命中后 6 轮二分细化（命中精度 0.375 m → ≈6 mm）。逐样本以上帧实际
  渲染矩阵投影到上帧屏幕取 UV + clip.w（线性视距），与上帧 scene_depth
  副本（同一存储口径）比较「穿越 + 厚度容差」；出屏 / 相机背后即终止。
  全程无 RNG、无导数依赖——同输入逐位可复现。
- **上帧资源（含对规格的偏离声明）**：上帧颜色 = 主 HDR 解析缓冲的帧尾
  副本（一次 `copy_texture_to_texture`，resolved 加 `COPY_SRC`）；上帧
  深度 = **复用现有单份 scene_depth 纹理、不做 ping-pong**——本引擎的
  scene_depth pass 编码在主 pass 之后，同一命令编码器按序执行 ⇒ 主 pass
  读到的必然是上一帧内容（规格预设的「单份变双份」是 1080p 下 ~16 MiB
  的纯浪费，语义与 ping-pong 逐位等价）。**不与 TAA 历史共用**——TAA
  历史是时域混合产物、非任何一帧的真实辐亮度（裁决理由进 §4.31）。
- **分叉语义（单一来源公式）**：`w_ssr = roughness_fade × edge_fade ×
  强度 × [命中]`，`roughness_fade = clamp((0.6 − r)/0.35, 0, 1)`（≤0.25
  全量 / ≥0.6 着色器早退不步进——GGX lobe 半角 ~40° 的单射线采样无意义
  且上帧颜色无预滤波 mip，粗糙面走探针/env 预滤波已是正确估计）；
  `spec_refl_w ← mix(既有镜面项, 上帧反射色 × f_amb, w_ssr)`——SSR 在
  「env vs 探针」选择之后、乘 BRDF 权重处替换，未命中 = 既有通路原串位。
  光照贴图实体的镜面项共用同一变量 ⇒ 同样可被 SSR 替换。
- **重置语义**：首帧启用 / 关→开 / 分辨率变化 / 切镜（**跟随
  `reset_taa`**——切镜让一切跨帧时域状态过期）/ 无不透明几何帧 ⇒ 该帧
  uniform `flags.x = 0` 整段跳过 ⇒ **输出与 SSR 关闭帧逐位一致**（硬
  断言）；`FrameStats::ssr_on` 观测字段（不进排序键）。
- **uniform 与绑定（R61/R62a 扩容先例）**：SSR 参数 96B（上帧矩阵 +
  标量）走 binding 13、上帧颜色 binding 11（可过滤）、上帧深度 binding
  12（非过滤，textureLoad）——追加在现有最大 binding 之后，**不扩
  `Globals` 前缀**，skybox.wgsl 并行布局陷阱不被触碰；绑定 11..13 在
  探针捕获面 / RT 相机的绑定组里恒为**零参数 + 1×1 哑纹理**（上帧语义
  只对主相机成立）。哑资源常驻（`SsrGpu`），缺席路径不分配任何大资源。
- **兼容铁律**：`SsrSettings` 缺席 / 关 = 不分配副本、不写参数、着色器
  早退，与 R62a 逐位一致。`tests/ssr_render.rs` 兼容锁：缺席帧 FNV =
  R53 之后默认链基线原值、显式关 = 缺席逐位、开启首帧 = 关闭帧逐位。
- **有效性（四项硬标准全 GPU 断言）**：①金属地面反射屏内标记落在
  **解析镜像位置**邻域（R62a 视差落点同款手法，8px 邻域 203 个标记红
  像素，关闭 = 0）；②**动态物体**（探针烘焙后标记 A→B）：SSR 反射
  跟随 B 的新镜像位置，探针路径（SSR 关）在 B 位置为 0——探针做不到、
  SSR 做得到；③SSR 强度随粗糙度**严格单调下降**（0.1/0.45/0.55/0.7
  四档、唯粗糙度变量的固定几何对照，0.7 整帧**逐位等于关闭帧**）；
  ④屏外标记 ⇒ 全部片元未命中 ⇒ 整帧与探针路径**逐位一致**。
- **确定性**：两个同输入完整会话（48 帧 ≥ 40，含相机平移与物体自体
  运动）逐帧逐位一致；切镜（reset_taa）后下一帧与关闭同姿态帧逐位
  一致、再下一帧恢复。验收图 example 跨运行 FNV 逐字节一致（debug 与
  release 同哈希）。
- **性能与显存**：480×270 室内小场景 release 帧时 0.122 → 0.347 ms
  （步进 + 帧尾副本；步进 = 参与片元各 32 次 mat4 乘 + textureLoad）；
  帧尾副本带宽 = 2×W×H×8 B（1080p ≈ 33 MiB/帧读写）；显存增量 =
  上帧颜色副本 W×H×8 B（1080p ≈ 16 MiB）+ 1×1 哑纹理与双 96B 参数。
  测试：4 项 CPU 单测（粗糙度衰减曲线 / 边缘衰减锚点 / 退化带二值）+
  7 项 GPU 集成测试（兼容锁 / 镜像落点 / 动态物体 / 粗糙度单调 /
  屏外回退 / 双会话逐位 / 切镜重置）。诚实限制 §8.38（只反射上帧屏幕
  内容 / 掠射平面假命中 / 厚度斜穿漏判 / 无预滤波 / MSAA 细节 / 一帧
  滞后 / hi-z 等拒绝清单）。
- **验收**：`docs/ssr.png`（4 排 × 2 列：金属地面 关|开 / 动态物体
  关|开 / 粗糙度梯度 关|开 / 屏外回退 关|开），
  `examples/ssr_snapshot.rs` 生成并内含硬断言。

### Added（R62a：烘焙反射探针——局部长方体影响域 + 预滤波 cubemap + 视差修正，路线图 v4 · W6）

§8.31④ 的缺口（挂光照贴图的实体失去 IBL 镜面）在探针覆盖范围内闭合，
静态箱庭的「室内/局部反射」有了通路：全局 env 只能反射天空，房间里的
金属与地面反射不到周围的墙和物。设计记录 ARCHITECTURE §4.30（新增）、
§4.24（与光照贴图的协同）、§8.31④（改写为已闭合）；诚实限制 §8.37
（新增）。**不做任何 git 写操作；既有 GPU FNV 基线全部原值通过（本轮
不重锁）。**

- **组件与影响域**：`ReflectionProbe { half_extents, falloff }`（位置取
  实体 `GlobalTransform`）。影响域是**轴对齐长方体**——选盒不选球的理由
  写进文档（室内房间是盒；视差修正的投影几何是盒；权重可逐轴分离）。
  权重 = 逐轴 `clamp((E_i − |p_i|) / falloff, 0, 1)` 的**乘积**：域内 1、
  过渡带线性降、域外 0（`falloff = 0` 退化为二值判定、不做除法）。
- **捕获与预滤波**：`Renderer::bake_reflection_probe(&world, entity)`
  （`&mut self` 入口，`reset_taa` 先例）——6 面 90° 捕获（面矩阵与点光
  阴影同源：`light::cube_face_view_proj`，含 NDC y 翻转）→ 按 GGX 预滤波
  成与 env **同档的 mip 链**（复用 `environment::encode_prefilter_chain`，
  同一 `roughness × env_params.x` 映射，mip 级数编译期锁死相等）。
  **捕获含天空**（决策点）：6 面先画天空盒再画不透明几何，产物是完整
  辐射度函数、采样侧零回退分支；代价是与 `SkySource` generation 绑定
  （换天空源需重烘），写进 §8.37。方向光阴影按**探针盒单级联拟合**
  （`probe::fit_probe_shadow`，写主帧级联槽 0 的临时借用）——封闭房间的
  地板不会在探针里「晒到太阳」；点光阴影不进捕获（诚实限制）。
- **着色分叉**（`standard.wgsl`）：域内 ⇒ 镜面走探针 + **视差修正**
  （经典 box projection：反射射线 × 盒求交、按出射点重定向采样方向，
  公式进注释 + 解析单测），域外 / 无探针 ⇒ 全局 env（现状路径）。
  探针参数走**独立缓冲 + 绑定 9**、cubemap 数组走**绑定 10**（CubeArray，
  复用 binding 6 的 Filtering 采样器）——**不扩 `Globals`**，R61 的
  skybox 并行布局陷阱完全不被触碰。
- **选择裁决**：逐片元取影响权重最大者，uniform 数组按**实体位模式升序**
  填充 ⇒ 并列裁决 = 位模式小者（全序，与查询 / 插入顺序无关，R61 选择键
  同款纪律）；预算 `MAX_REFLECTION_PROBES = 4`（超预算 warn），CPU 按
  （相机到影响域距离升序, 实体位模式升序）取一批。**不做多探针混合**。
- **光照贴图协同（闭合 §8.31④）**：`spec` 在 `select` 之后对挂图集的实体
  追加探针镜面项（间接漫反射仍走贴图）——语义「漫反射走贴图 / 镜面走
  探针」；无探针时该项不执行，R52c 语义与逐位兼容不变。
- **兼容铁律**：无组件 / 未烘焙 / 域外 ⇒ `spec` 保持 `env_spec` 的同一
  串位（`select` 纯值选择），且无探针时一次 `write_buffer` 都不发。
  `tests/probe_render.rs` 的 `probe_absent_or_unbaked_is_bitwise_identical`
  逐字节断言；既有 1028（debug）/ 1026（release）测试零改动全过、全部
  既有 GPU FNV 基线原值通过。
- **实测（本轮抓到的两个真 bug，都固化成测试）**：①捕获管线沿用
  `front_face: Ccw` 时，面矩阵的 NDC y 翻转**反转环绕方向** ⇒ 剔掉房间
  朝内的墙面、渲染外壳（症状：探针被球壳包住时整张 cubemap 变成球内壁的
  暗灰、墙面图案与自发光板全部消失）；改为 `front_face: Cw` 后恢复与主
  pass 一致的剔除语义。②「探针恰在反射体内部」是退化输入，文档化为
  使用建议（探针放在房间中央、反射体在域内偏移处）。
- **验收**：`docs/reflection_probe.png`（4 排 × 2 列：室内盒场景探针
  关|开 / 视差修正 关|开 / 粗糙度 0.1|0.8 / 光照贴图场景探针 关|开），
  `examples/reflection_probe_snapshot.rs` 生成并内含硬断言。测试：18 项
  probe 单测（权重单调性 / 视差五组特例解析解 / 选择全序 / 槽位幂等 /
  阴影拟合）+ 7 项 GPU 集成测试（兼容逐位 / 朝向镜像 / 视差解析落点 /
  粗糙度 mip / 双会话逐帧逐位 / 乱序插入选中同一探针 / 光照贴图镜面恢复）。
- **性能与显存**：探针 cubemap 128²×6×5 级 mip ≈ **1.0 MiB/探针**
  （预算 4 ⇒ 4 MiB）+ 共享捕获擦写立方体 768 KiB + 深度 384 KiB
  ≈ **5.2 MiB**（对照点光阴影预算 4 盏的 48 MiB）；采样开销 = 逐片元
  最多 4 次权重计算 + 1 次 `textureSampleLevel`（CubeArray）；烘焙为
  一次性命令缓冲（阴影 1 pass + 6 面捕获 + 30 个预滤波 mip pass）。

### Added（R61：阴影 v2——坡度缩放 bias + 法线偏移 + 点光阴影预算化多盏，路线图 v4 · W6）

R60 遗留的两条阴影限制（§8.6「点光阴影首盏」、§8.8「偏移为常数 bias」）
本轮闭合：偏移改为**逐级联 / 逐灯按纹素世界尺寸缩放**的坡度缩放 bias +
法线偏移，点光阴影从「查询序首盏」扩为**预算 4 盏 + 确定性选择键**。
设计记录 ARCHITECTURE §4.7.1（重写）、§4.7.2（新增「阴影偏移 v2」小节）、
§4.7（比较段）；诚实限制 §8.6 / §8.8 同步改写。

- **坡度缩放 bias + 法线偏移**：`bias = clamp(BIAS_MIN + 1.5 · (texel_world /
  depth_window) · tanθ, BIAS_MIN, BIAS_MAX)`，法线偏移 =
  `min(1.5 · texel_world · (1 − NdotL), 0.15 m)` 沿**几何法线**推移采样点。
  推导：一个阴影图纹素跨度内的深度变化 = `texel_world · tanθ`
  （`tanθ = tan(acos(NdotL))`，clamp 64），3×3 PCF 对角样本再乘 √2 ⇒ 系数
  取下界 √2 与实测之间的 1.5。常数项**保留为下限**（0.0015 → 0.0004，
  完全去掉会复发平地 acne），上限 0.02 防掠射极端退化为无影。级联越远
  纹素越大、窗口越宽 ⇒ 偏移量逐级联缩放（`CsmParams` 224B → 256B，新增
  `cascade_texel` / `cascade_slope_k`，CPU 由新的 `light::cascade_fit_params`
  逐级联给出；矩阵与 R29 逐位相同）。点光同两技术适配距离比较语义
  （`texel_world(d) = d · 2/512`，单采样无 PCF ⇒ 坡度系数 1.0；bias 仍以
  米存、除以 range 归一化），常数下限 0.03 m → 0.005 m。
- **效果（同场景对照，数字皆进断言）**：掠射角平面（NdotL ≈ 0.118）acne
  像素 **12883 → 0**（测试基线场景 4575 → 0）；0.07 m 薄板的接触阴影在
  常数 bias 下**整条消失**（开 / 关阴影两帧逐位相同 = peter-panning 的
  极端形态），v2 保留 2591 px 且根部亮带 **0 px**（法线偏移未让接触阴影
  浮起）。
- **点光阴影预算化多盏**：`POINT_SHADOW_BUDGET = 4`（常量而非资源——
  纹理数组层数在 `Renderer::new` 固定，运行时调整需重建纹理与 24 个面
  视图 / 绑定组）。颜色与深度都扩为 `512² × 24` 层的 `texture_2d_array`
  （采样侧 `CubeArray` 视图，层 = 槽位 × 6 + 面），每帧最多 24 个捕获
  pass；**显存 12 MiB → 48 MiB（+36 MiB）**，实测 `draw_frame`
  0.37 ms/帧（1 盏）→ 0.70 ms/帧（4 盏）。**选择键 =（相机距离升序,
  实体位模式升序）**：纯函数 `light::select_point_shadow_casters`，比较器
  为全序 ⇒ 结果只取决于灯集合、**与查询 / 插入顺序无关**（乱序插入选中
  同一批灯，单测守护）；超出预算 `log::warn`（消息改为预算语义）。单盏
  阴影场景选中结果与 v1 一致（同灯），无阴影点光走与 v1 逐位相同的路径。
- **uniform 布局（渲染内部契约，不涉公开 API）**：`PointLight` 2×vec4 →
  3×vec4（`shadow = (是否投影, cubemap 槽位, −, −)`），`Globals` 512B →
  640B，`point_shadow_params` 语义改为（是否有投影灯, bias 下限, 坡度
  系数, 法线偏移系数），range 改为逐灯 `color.w`；v1 的「投影灯交换到
  槽位 0」随之废止。`FrameStats.point_shadow_casters` 新增观测字段
  （不进任何排序键）。
- **并行布局陷阱（新看门测试）**：`skybox.wgsl` 自持一份 `Globals` 前缀
  声明，`point_lights` 长度漏同步会让天空整体错色（R61 实测踩过一次，
  三条「阴影关闭」对照组基线同时失守才定位到）——
  `pipeline::tests::skybox_globals_prefix_matches_standard_layout` 逐字段
  核对两处的字段顺序与数组长度。
- **归因 + 重锁（本轮特殊，程序完整执行）**：受影响的 GPU FNV 基线共
  3 条——`normal_render` R41 锁与 `post_render` R42 锁
  `0x609b_2457_f4d5_cd82` → **`0xe51d_b127_6079_ce39`**、
  `lightmap_render` R52b 锁 `0x3fe5_f3ff_00e0_f329` →
  **`0xf232_16a3_37fb_f385`**，均在常量注释里声明「R61 重锁 + 归因证据」。
  归因证据三条：①受影响场景的**阴影关闭对照组**在 R61 前后**逐位一致**
  （0x6a56_af90_bf4c_6686 / 0x8fbb_a702_069d_c4f5 / 0xf70f_dbd5_ec3b_06e1，
  见 `tests/shadow_v2_render.rs`）；②`Renderer::set_legacy_shadow_bias(true)`
  的对照模式**逐位复现 R60**（掠射角 / 接触阴影 / 单盏点光三场景哈希与
  改动前实跑相同）；③未受影响的锁（fxaa / occlusion / taa 三处 FNV，
  场景本无阴影）原值通过 = 改动面没有外溢的旁证。
- **验收**：①归因重锁程序完整（见上）；②消偏有效性——掠射角 acne
  4575/12883 → 0、接触阴影根部亮带 0 px 上界断言、双点光各投各影
  （单开 A 320 px / 单开 B 81 px / 双开 401 px，**401 = 320 + 81 精确
  可加 = 互不串扰**，投影灯数 1/1/2）；③确定性——阴影开启（CSM + 双点光）
  双会话 30 帧逐帧逐位一致 + 预算选择在同灯集合乱序插入下选中同一批灯
  （纯函数单测）；④验收图 `examples/shadow_v2_snapshot.rs` →
  `docs/shadow_v2.png` 四面板（掠射角 旧|新、接触阴影 旧|新、双点光
  关|开、CSM 接缝 2× 放大|暗化差分），快照跨运行逐字节一致（MD5
  4c7e47ff0f87326f37995e43e816b0e6）。测试基线 debug 1014 → **1028** /
  release 1012 → **1026**（+14 = light.rs 6 项纯函数单测 + pipeline.rs
  1 项布局看门 + `tests/shadow_v2_render.rs` 7 项 GPU 集成测试；既有测试
  除 3 条重锁常量外零改动）。

### Added（R60：TAA 时域抗锯齿——历史缓冲 + 投影抖动 + 相机重投影，路线图 v4 · W6 首轮）

R53 遗留的「历史缓冲 + 投影抖动 + 运动向量三件套与单帧链架构冲突」
（§8.32⑩）本轮解开：状态严格收敛在**链首单一 pass** + 一个帧计数器 +
一个上帧矩阵快照，链的其余部分保持无信号单帧 pass。设计记录
ARCHITECTURE §4.29（新增，六项设计裁决），诚实清单 §8.36（新增）、
§8.32⑩ 修订。

- **链位与共存**：TAA 在链首（主 HDR 解析缓冲之后、DOF/MB 之前，
  `PostInput::TaaHistory` 让下游链直读本帧历史输出）——时域累加给
  后续 pass 干净输入；与链尾 LDR 域的 FXAA 链位正交、可同开为推荐
  组合；MSAA 4x 之上叠加抖动（静止收敛 ≈ 32 等效样本 SSAA）；RT
  视口无后处理链不受影响。
- **投影抖动**：Halton(2,3) 8 点序列（`halton` / `taa_jitter` 纯整数
  radical inverse，帧号取模）±0.5 纹素；`jitter_projection` 只作用于
  **纯投影矩阵再乘 view**（row0 += jx·row3 的单元素实现依赖投影 w 行
  结构，对合成矩阵不成立；NDC 位移解析单测钉住符号与幅值）。本帧矩阵
  贯穿主 pass / 视锥剔除 / scene_depth 深度副本 / 遮挡查询代理 / MB
  **一致抖动**（遮挡保守性论证原样成立：AABB ⊇ 几何与矩阵无关，抖动
  是两者的全局刚性偏移）；CSM 光空间矩阵与 RT 相机不抖动。
- **重投影与钳制**：复用 MB 的深度副本数学（线性视距 → NDC z 解析
  重建 → 本帧 `inv_view_proj` 反投影 → 上帧实际渲染矩阵重投影）；3×3
  RGB 邻域 AABB min/max 钳制 + 固定 0.9/0.1 混合（`shaders/taa.wgsl`，
  组 0 五 binding：参数 144B + 抖动色 + 线性采样器 + 深度 + 历史）。
  **无逐物体运动向量**（拒绝清单）——动态物体鬼影由钳制压制不消除。
- **历史与重置**：Rgba16Float 双缓冲 ping-pong（`TaaTargets` 独立于
  PostScratch——后者每帧被链上 pass 覆写；随 `HdrTargets` 尺寸重建 =
  分辨率变化免费重置）。**重置 = 显式 `Renderer::reset_taa()` + 自动
  （首帧启用 / 关→开 / 分辨率变化），不做切镜阈值检测**（内容启发式，
  决策交给知道切镜时刻的宿主）；重置帧抖动置零 + 混合系数 0，输出与
  无 TAA 帧**逐位一致**（`OffscreenRenderer::renderer_mut` 新增可变
  访问器）。`FrameStats.taa_reset` 观测字段（不进排序键）。
- **兼容铁律不变**：`TaaSettings` 缺席 = 不抖动任何矩阵
  （`jitter_projection` 对零偏移原样返回）、不分配历史缓冲、不入链
  ——全部既有 GPU FNV 基线逐位不变（fxaa 场景缺席帧哈希仍锁 R53 值，
  交叉验证整条链未被扰动）。
- **确定性升级**：历史反馈使渲染有状态 ⇒ 硬要求升级为「两个同输入
  完整会话（130 帧 = 100 帧亚像素平移 + 30 帧静止）逐帧逐位一致」；
  静止收敛稳态为 Halton 8 帧循环轨道——稳态 FNV 基线锁库
  （0x5d96_20c7_3382_cbf0）+ 两周期对应相位帧残差 ≤ 2 LSB（8-bit 输出
  量化口径，非逐位不动点）。
- **有效性指标（可判别口径，8px 内部 ROI——抖动使视锥边界几何随相位
  出画/入画，与边缘爬行是两类现象）**：亚像素缓动相机 + 2px 细杆场景
  ①运动段近满幅硬翻转（帧间 |Δ|>100）关 296 / 开 **0**（归零），高
  跳变（|Δ|>75）关 1064 / 开 121（8.8 倍），最大帧间跳变 117.7 →
  91.7；低阈值（>25）TAA 变化更多（32k vs 2.6k）= 缓坡平滑移动的
  签名（「指标学」：判别选在分布高端）；②静止收敛帧边缘过渡带
  （10%–90% 窗口）215 → 782；③鬼影有界：运动方块中段拖影像素
  （|Δ|>25）< 4%、停止 80 帧衰减 < 1%（残余 = 收敛边缘软化）。
- **验收**：① 兼容——既有全部 GPU 基线测试零改动全过（含
  post/fxaa/occlusion 的 FNV 锁）；② 确定性——双会话逐帧逐位测试；
  ③ 指标——上三项；④ 验收图 `examples/taa_snapshot.rs` →
  `docs/taa.png`（上排细杆 关|开：边缘平滑 + 低频天空残差 ≤ 2 LSB
  不糊化；下排运动方块 关|开：拖影受控；跨运行 MD5 逐字节一致），
  judge 目检通过；③ 门禁六项全绿。测试基线
  debug 1005 → **1014** / release 1003 → **1012**（+9 = post.rs 4 项
  纯函数单测 + `tests/taa_render.rs` 5 项 GPU 集成测试：兼容锁 /
  显式重置逐位 / 双会话确定性 + 稳态基线 / crawling + 过渡带 /
  鬼影有界；既有测试零改动全过）。


### Added（R59：资产管线强化——动画注册表 / BC3 压缩通路 / manifest + 优先级加载，路线图 v4 · W5）

三块独立能力服务同一目标：外部资产「从导入到运行」一条龙。设计记录
ARCHITECTURE §4.28（新增）与 §7.3 R59 增量，诚实清单 §8.35（新增）、
§8.24 修订。

- **GLB 动画注册表**（`iris_render::asset::registry`）：`AnimationRegistry`
  （World 资源，`BTreeMap` 路径字典序）——`import` headless 批量导入
  （不 spawn 也能解析剪辑元数据：名字 / 时长 / 轨道数 / 事件数 + 骨架
  节点名表）；`spawn_instance` 命中即复用（未命中自动导入）；按名查
  剪辑（重名取先）。**多实例共享**：`AnimationPlayer::clips` /
  `GltfInstance::clips` 升级 `Arc<Vec<AnimationClip>>`——注册表命中
  spawn 的 N 实例共享同一份关键帧数据（引用计数，取舍见 §4.28），
  `GltfInstance` 新增 `textures: Vec<TextureHandle>`（本次 spawn 落库
  句柄，BC3 声明的桥梁）；写时复制口 `AnimationPlayer::clips_mut()`
  （`Arc::make_mut`，事件轨道注入用；iris-ascent 同步迁移）。常规
  spawn 行为不变（独占份，读路径 Deref 全兼容）。
- **BC3 编码器 + 压缩通路**（`texture_bcn` / `texture` / 渲染器）：
  `encode_bc3`（BC4 alpha 块 + BC1 色块复用 = 1 B/px）与
  `bc3_eligible`（分层规则：全不透明归 BC1、带 alpha 走 BC3，两判定
  恒互斥）；确定性全整数编码（纯色 alpha 块索引全 0 恒等还原——绕开
  端点相等的硬件穿透调色板）；**默认路径逐位不变**：BC3 只对
  `TextureStore::declare_bc3` / `add_bc3_declared` 声明过的纹理生效，
  渲染器决策树未声明分支与 R59 之前逐位一致（既有 GPU 基线零变化；
  `texture_is_bc3` 观测口、`gpu_bytes_estimate` 计 BC3 口径 1 B/px）；
  误差断言诚实口径：alpha 单像素上界 255/14 ≈ 18.2（端点即块内极值
  像素），GPU 等价渲染块内纯色棋盘格全帧平均误差 < 2/255。
- **资产 manifest + 优先级加载**（`iris_engine::manifest` +
  `async_loading`）：JSON 清单（路径 / kind / 优先级整数（越小越先）/
  bc3 标志，serde 零新依赖，中文校验报错，未知字段前向兼容）；
  `AssetLoader::request_with_priority`——出队键 =（优先级，请求序）升序
  （请求序在调用线程分配，与到达线程的先后无关），加载线程排空通道再
  弹最小键，在途作业不抢占；inline 后端共用同一队列（**契约修订**：
  读取耗时计入 poll 而非 request——同 API 同语义；同优先级仍请求序
  FIFO，既有调用方无感）；**中断 / 续载语义**：loader 无记忆，丢弃即
  在途请求作废，续载 = 消费方持已得结果重发未完成请求（幂等重读），
  两后端全流程测试钉住（线程后端确定性出队由测试闸门定序）。
- **生成工具 + 全流程验收**：`gen_r59_assets`（含内嵌 alpha 贴图 +
  法线 + idle/swing/hit 三命名动画的 GLB 与 manifest）→ `r59_snapshot`
  （manifest → 优先级异步分帧加载 → 注册表导入 → 双实例 spawn → 按名
  切动画 → `docs/asset_pipeline.png` 六面板：三姿势帧 / BC3 vs RGBA8
  对照 / 加载时序 + 显存数字（292 B → 100 B）；快照跨运行逐字节一致）。
  headless 端到端 `iris-engine/tests/asset_pipeline.rs`。
- **验收**：① headless 端到端 2 项 + 单测 registry 5 / async_loading +5 /
  manifest 5 / BC3（texture_bcn 7 + texture 1 + bc3_render GPU 2），基线
  debug 976 → **1003** / release 974 → **1001**（既有测试零改动全过——
  默认路径逐位不变的最终证据；唯 iris-ascent 事件注入一处走新的
  `clips_mut` 写时复制口）；② 离屏验收图 `docs/asset_pipeline.png`
  （`examples/r59_snapshot.rs` 产出，MD5 跨运行逐字节一致）；③ 六项
  门禁全绿（wasm32 clippy 含新代码照绿）。


### Added（R58b：编辑器 b 阶段——undo / 光源编辑 / 烘焙入口 / 导航可视化 / 视角旋转，路线图 v4 · W5）

`iris-editor` b 阶段五件事全部落在命令层（交互壳只加键位与面板行），
诚实清单 §8.34（12 条）与 edit.rs 模块文档同步。

- **undo / redo（快照栈）**：每个写操作（摆放 / 删除 / 复制 / 拖拽按下 /
  微移 / 缩放 / 光源编辑）执行前拍 `WorldSnapshot` 入栈（深度 32 环形丢弃），
  `Ctrl+Z` / `Ctrl+Y`（或 `Ctrl+Shift+Z`）进退。快照 = 可编辑实体全组件 +
  两 Store **整库克隆** + 登记表全文 + 方向光资源 + `level` 段 + 编辑器句
  柄缓存；undo 后世界 dump（名字序转储）与快照时点**逐字符一致**（多步
  序列含「undo 到头再 redo 到尾」全恢复，测试钉住）。三条硬决策：恢复 =
  **全删全建**（组件存储 swap_remove 打乱存储序）；Store 记**整库克隆**
  而非截断（undo 截掉的数据 redo 无法凭空恢复）；**挂载根边界**（含 glTF
  挂载根的世界快照栈清空）。烘焙与 NavGrid 不入栈（烘焙**清栈**——跨烘
  焙 undo 会把在用的 UV2 副本从库里换掉）。引擎侧小改：`MeshStore` /
  `MaterialStore` 补 `contents` / `set_contents`（整库克隆消费）。
- **光源参数编辑**：模式由选中态裁决——选中点光 = `[` `]` 强度、`-` `=`
  范围、`H` 色相 +30°（HSV→RGB 纯函数，饱和度 / 明度保持）、`K` 阴影开
  关；未选中 = 方向光 `[` `]` 强度、`;` `'` 环境光、`J`/`L` 偏航
  `I`/`K` 俯仰（0.05 rad 步进、限位 ±89°、归一化回写）。全部入 undo
  栈，位模式精确测试（`to_bits` 对照 + 一次 undo 复原逐位）。`L` 双语义：
  未选中 = 偏航 +，有选中 = 摆放点光（R58a 语义保留）。
- **烘焙入口（`B` = 保存 + 烘焙）**：先存档（文件 = 内容的规范事实）→
  在**规范影子世界**（fresh World + `open_level` 同款初始化序 +
  `load_scene`，与游戏加载同位布局）上跑 `bake_lightmap` → IRISLMAP 产物
  落盘（`level.bake.artifact` 引用名 / 缺席 = 档同目录 `*.ilm` 默认名 +
  状态行提示）→ 产物表项按**清单序**重映射回活动世界 `install_lightmap`
  + 登记表回补（R57 `attach` 语义 ⇒ 烘焙后保存仍写原始 mesh 段、重载重
  挂，闭环测试钉住）；烘焙后继续摆放的新实体无图集绑定走 IBL（既有混合
  语义）。状态行回报 texel 数 / 耗时 / 挂载实体数。
- **open_level 位布局修正（内容实体先行）**：实体生成序改为「瞬态资产
  句柄（非实体）→ 关卡内容实体 → 网格线 / HUD」——内容实体 0 号槽连续、
  代数 0，与 `load_scene` 规范布局一致。这是烘焙闭环的地基：a 阶段的
  「家具先行」布局使编辑器世界与任何重载世界的实体位必然分叉，R57 产物
  按 (实体位, 句柄) 引用 ⇒ 直接烘焙活动世界必产出失配产物（实测踩坑）。
- **NavGrid 可视化与重建（`N`）**：开 = 重建 + 叠加（每次开都重建），
  关 = 撤叠加、资源保留。参数优先档内 `level.nav`；缺席时
  `nav_bounds_from_statics` 纯函数从静态碰撞体 AABB 并集推 bounds + 默认
  格子参数。blocked 格半透明红薄片 + 域边框橙线（`EditorTransient`——不
  入档、不可拾取）；面板显示格子总数 / blocked 数（`is_blocked` 逐格统计）
  / 域尺寸。
- **相机视角旋转**：中键拖拽 = 绕 target 轨道（`orbit_eye` 球坐标解析式
  纯函数，俯仰钳 ±89°）；`F` = 聚焦选中实体（target = 世界 AABB 中心）。
  相机不入档语义不变。
- **引擎 bug 修正（编辑器烘焙 dogfooding 发现）**：`uv_sphere` 极点带的
  quad 剖分有一半是**零面积退化三角**（极点环顶点全重合），与
  `build_bvh`「primitives 不产出退化三角」前提矛盾——任何含球场景烘焙即
  panic。极点带改三角扇修正（视觉零差异）；`mesh.rs::sphere_topology` 与
  `lod_render::HIGH_TRIANGLES` 两个金标准按新拓扑重推导
  （(rings−2)·segments·6 + 2·segments·3）。
- **验收**：① headless 19 → **31 项** + 命令层单测 +7，基线 debug
  957 → **976** / release 955 → **974**（既有测试零改动全过，唯上述两个
  金标准常量随 uv_sphere 拓扑修正重推导）；② 离屏验收图
  `docs/editor_b.png`（2×2 拼图：点光编辑前后 / 烘焙前后 + 导航叠加 +
  面板 UNDO/BAKED/NAV 行；`examples/editor_b_snapshot.rs` 产出，帧内容
  跨运行逐字节一致——烘焙耗时墙钟被确定性状态行覆盖）；③ 六项门禁全绿
  （wasm32 clippy 含新代码照绿）。

### Added（R58a：最小编辑器 MVP——视口 / 摆放 / 拖拽 / 存取，路线图 v4 · W5）

新 crate `iris-editor`（bin，原生为主）：**能用的关卡摆放编辑器** a 阶段。
数据层 = R57 关卡档 v2（不发明新格式）；语义规范全文 = edit.rs / shell.rs
模块文档 + ARCHITECTURE §4.27，诚实清单 §8.34（9 条）。

- **架构硬要求兑现：命令层与交互壳分离**。`edit.rs` 命令层：`EditorState`
  资源 + 全部编辑操作为自由函数（`open_level` / `save_level` / `place_at` /
  `place_light_at` / `pick` / `placement_point` / `select` / `begin_drag` /
  `drag_to` / `delete_selected` / `duplicate_selected` / `nudge_selected` /
  `scale_selected` / 参数切换 + 只读查询 `editable_entities` / `describe` /
  `panel_text` / `screen_ray_at`），签名一律 `World(+ 事件参数) → 确定性
  结果`，不含窗口 / 输入 / 渲染代码；`shell.rs` 交互壳刻意薄（飞行相机
  WASD/QE/滚轮 + 输入快照翻译 + Esc，键位表在模块文档与运行期面板）。
- **a 阶段功能**：地面参考网格（±12 m、纯视觉**不入档**）；图元四态
  （cube/plane/sphere/cylinder 循环）+ 五种无纹理材质预设 + 可选静态
  碰撞体 + 点光（灯泡网格）摆放，落点 = 鼠标射线与实体 AABB / y=0 地面
  的最近交点、底面贴合 + 0.5 m 吸附；射线拾取（`MeshData::bounds` 世界
  AABB + 引擎 `ray_box`，最近命中、同距取存储序在前）+ 12 边选中框；
  左键拖拽（射线 ∩ 命中点高度平面、抓取偏移不跳）；Del 删除（级联子树）/
  Ctrl+D 复制（`_copy{n}` 后缀）；只读属性面板（8×8 ASCII 位图字体，数值
  修改走热键：方向键微移 0.1 / Alt = Y / Shift 十倍、PgUp·PgDn 等比缩放）；
  CLI 第一参数 = 关卡档路径（缺省 `assets/editor_mvp.level.json`），存在
  则 v2 直读（`level` 段经 `LevelRegistry` 快照原样保留）、Ctrl+S 保存；
  档存在但解析失败 **panic** 而非降级空世界（避免随后一次保存覆盖原档）。
- **存档往返的两条机制不变量**：① 编辑器 spawn 的网格 / 材质**同步登记
  进 `SceneAssetRegistry`**（`MeshRef::Primitive(段)` / `MaterialRef::
  default()`）——不登记则 `save_scene` 还原不出 mesh 段；② **句柄编号
  对齐**：编辑器侧同源去重（同图元段 / 同预设只入 Store 一次，`Vec` 线性
  扫描缓存——键含 f32 不可 Hash 且插入序即句柄序）+ 瞬态视觉资产先建 ⇒
  保存与重载两侧 `MeshHandle` / `MaterialHandle` 逐位相同（往返对照测试
  直接比句柄数值）。瞬态视觉（网格线 / 选中框）挂 `EditorTransient`，
  `save_level` 存前 despawn、存后按缓存句柄重建（不新占编号）；HUD 实体
  无 `Transform` 天然不入档。
- **引擎侧两处小改动**：`PrimitiveSection::to_mesh_data` 提升为 pub
  （编辑器复用同一份「段 → 网格」映射，避免两处生成参数漂移）；prelude
  补导出 `PrimitiveSection`。
- **验收**：① headless `tests/edit_headless.rs` **19 项**——9 实体（四
  图元 + 五预设 + 静态碰撞体 + 点光 + 复制体）摆放 → save → load → 世界
  dump（实体数 / 清单序 / 逐实体 Transform 位模式 / 句柄 / 组件位 / 登记
  表 BTree 序 / 两 Store 逐句柄 FNV 与位模式 / 方向光位模式 / level 段
  快照）**逐字符相等**；同档二次保存逐字节相同；assault 庭院档（R57
  产物）经编辑器开 → 编辑 → 存 → 重载往返一致；既有 924 / 922 测试
  **零改动全过**（基线 debug 924 → **957** / release 922 → **955**，新增
  33 = 命令层 14 + headless 19）；② 离屏验收图 `docs/editor_mvp.png`
  （整体：命令层摆出 24 实体测试关卡 + 网格 + 选中框 + 面板）与
  `docs/editor_viewport.png`（编辑态近景：换视角换选中），
  `examples/editor_snapshot.rs` 产出（保存 + 重载对账同一条代码路径）；
  ③ 六项门禁全绿（wasm32 clippy 含新 crate 照绿——crate 内无原生专属
  API）。

### Added（R57：关卡格式与编辑数据模型——路线图 v4 · W5 首轮）

关卡档 v2（`iris-engine::level` + `scene` 格式升级）：让箱庭关卡的
**静态世界**完全脱离游戏代码存在，为编辑器轮（R58）打数据地基。
语义规范全文 = level.rs / scene.rs 模块文档 + ARCHITECTURE §4.26，
诚实清单 §8.33。本轮**不做任何编辑器 GUI**。

- **关卡档 = 场景档的超集（不从零重造）**：顶层 `version` 字段 +
  `level` 段；`version` 缺席 = v1 纯场景档**无损直读**（迁移钩子函数表
  `MIGRATIONS` 的第一步：v1 → v2 = 加默认空 `level` 段）；高于当前
  版本拒读（`Unsupported`，pack 格式同款策略）；save 恒写当前版本、
  空 `level` 段省略。加载 / 保存仍是 `load_scene` / `save_scene`
  **单通路**。
- **`level` 段**（全可选，应用序 terrain → 实体 → nav → bake）：
  - `terrain`：heightmap 两态（`hills` = `generate_hills` 生成器全参数
    / `inline` = 行主序高度数组；**不引用外置高度场文件**——v1 无
    高度场装载器，取舍写入模块文档）+ 材质段 + `TerrainOptions` 全
    字段；派生实体（根 / chunk / 碰撞拼块）打 `LevelGenerated` 标记，
    save 跳过、下次加载整体重建（`GltfMounted` 同款模式）；引擎断言
    前提全部前置校验为 `OutOfRange`；
  - `nav`：**只存 `NavGridParams` 全字段、不存烘焙格子**——加载后由
    世界静态碰撞体（无 `RigidBody` 或 `is_static`，排除传感器）经
    `NavGrid::build` 重建入 World 资源（位图是静态几何的纯函数，
    §3.9「便宜重建」）；
  - `bake`：`BakeParams` + `BakeUvParams` 全字段 + 记录性
    `dilate_iterations` + **产物文件名引用**；产物存在则校验后
    `install_lightmap`，**缺失诚实降级为无光照贴图并告警**，损坏 /
    与关卡几何失配报错（陈旧产物是错误不是降级）。
- **光照贴图产物外置文件（IRISLMAP v1）**：`BakedLightmap` 全字段
  无损二进制（魔数 + 版本 + 图集布局表 + radiance/indirect f32 原样
  字节 + covered 位图，全小端；编码纯函数逐位确定）；
  `save_lightmap_artifact` / `load_lightmap_artifact` /
  `encode_lightmap_artifact` / `parse_lightmap_artifact` 公开。挂载前
  一致性校验（实体在世 / mesh 句柄 / 三角数 / 下标连续性——穷尽
  `install_lightmap` 的 panic 前提），挂载后 UV2 副本句柄的来源
  **回补登记表** ⇒ save 还原原始 mesh 段、重载重挂，往返闭环。
- **实体段扩展**：材质段升为**三纹理槽**（基色 / 法线 / ORM），每槽
  纹理源两态——文件路径字符串（v1 语义原样，serde untagged 直读）或
  **程序化源**（`grass` 三件套 / `brick_normal_orm` 对，引擎确定性
  生成器全参数，sRGB 按槽位）；`particles` 段 = `ParticleEmitter`
  全部配置字段（运行期状态不入档；particles.rs 的「不写场景序列化」
  v1 限制作废）；方向光段补齐 `shadow_range`（缺省省略，v1 档重存
  字节级不变）。**点光核实结论**：v1 实体段已全覆盖 `PointLight`
  四字段，无缺口。
- **同源去重（v2 加载语义）**：一次加载内同图元参数 / 同 (路径,
  srgb) 纹理 / 同参数程序化纹理 / 位级同材质**共享句柄**——「文件内
  同源 ⇒ 世界内同句柄」，assault 迁移逐位复现代码侧共享布局的机制
  前提（glTF 整实例除外，v1 语义原样）。
- **往返与浮点实测**：`load → save → load` 第二次世界与第一次**逐位
  一致**（assault 庭院档钉住：实体表 / 组件位模式 / 三大 Store 内容
  FNV / 登记表 / 资源全量对照）；serde_json f32 往返**逐位保真**
  （4112 确定性伪随机位模式 + 边界值全集 `to_bits` 对照）——「经 f64
  中转 1 ULP 漂移」实测不成立，无需自定义浮点输出策略（依据入档
  scene.rs 模块文档）。
- **assault 庭院迁移（验收①）**：`build_courtyard` 约 170 行代码生成
  整体替换为加载 `assets/assault_courtyard.level.json`（15 静态实体 +
  黄昏方向光，石板 / 砖墙写实材质以程序化纹理源入档、零二进制资产）。
  迁移前后同帧离屏渲染 **FNV-1a 逐位相等**（帧 A
  `0x1070dd6e79e0523c` / 帧 B `0x31ca9c4842076a08`，改码前捕获基线）、
  FrameStats 全等、headless parity dump（288 实体 + Store FNV +
  登记表）逐行一致；既有 29 项单测 + 8 项 headless **零改动全过**。
  粒子 RNG 以实体 bits 为种子 ⇒ 档内实体序 = 原 spawn 序是硬约束
  （新测试钉住）。墙 / 柱数据与 `logic.rs` 常量的双源一致性由
  `tests/level_roundtrip.rs` 位级对照守护（单一事实来源留 R58）。
- **API**：`NavGrid::params()` 只读访问器（往返对照出口）；prelude
  导出 level 段全部类型与产物 IO；`MaterialRef` 扩展三槽来源字段
  （`texture` 字段语义不变，既有调用零破坏）。
- **测试**：level.rs 14 项 + scene.rs 新增 2 项（scene 模块计 28 项）
  + assault `level_roundtrip.rs` 3 项；基线 debug 905 → 924 /
  release 903 → 922（新增 19，逐二进制明细见 REPORT.md）。

### Added（R53：遮挡剔除 + FXAA——写实路线 W4 收官）

硬件遮挡查询剔除（`iris-render::occlusion`）与后处理链尾部 FXAA
（`shaders/fxaa.wgsl`）。语义规范全文 = occlusion.rs 模块文档 +
ARCHITECTURE §4.25，诚实清单 §8.32。**硬约束：默认设置（两个开关
资源缺席）下全部既有 GPU 测试像素零变化**——遮挡缺席 = 查询 pass /
GPU 资源 / 命令流一个字节都不多；FXAA 缺席 = blit 直写最终目标，
链与 R52c 之后逐位一致（两条新 FNV 兼容锁 + 全部既有基线原值通过）。

- **遮挡查询两阶段（对规范草稿的 4 处偏离均有理由）**：帧 N 对
  「视锥内全部普通不透明实例」画世界 AABB 代理盒（查询盒 = 局部
  包围盒 8 角点过模型变换的轴对齐并集 ⊇ 几何，纯函数 `world_aabb`
  解析单测），每实例一个硬件遮挡查询（4096 槽封顶、超出恒可见），
  resolve → copy → 帧 N 提交后 `map_async`；帧 N+1 开头
  `poll(Poll)` + `try_recv` 消费（双帧读回环、同一时刻至多一条在途、
  **迟到即弃**——宁可漏剔不误剔），样本数 = 0 者主 pass 跳过绘制。
  偏离①：wgpu 30 遮挡查询已是核心能力（`Features` 无 `OCCLUSION_QUERY`
  位），`supports_occlusion_query()` 恒真、仅作探测点，「无能力 = 全部
  可见」由读回不新鲜的执行路径天然承担；偏离②：查询在**主 pass
  之后**复用本帧深度（草稿的「主 pass 之前 + 上一帧深度」不可行——
  主深度是 MSAA 且每帧被清，跨帧持有需额外深度预 pass），剔除决策
  仍延迟一帧；偏离③：每帧查询全部视锥实例（草稿只查「可见或未
  判定」——被剔者不再写深度，停止查询 = 遮挡者移动后永久空洞）；
  偏离④（实现级）：查询管线深度比较 **LessEqual**——可见实例的
  AABB 前面与它自己写入的深度共面等深，严格 Less 会让每个轴对齐
  几何「自遮挡误剔」（贴花 R39 同款教训的遮挡查询版）。
- **保守性 = 像素级**：遮挡查询语义是保守下界（硬件可多报、禁止
  少报），计 0 ⇒ 全部采样严格远于已写入深度 ⇒ 该实例对本帧颜色与
  深度零贡献 ⇒ 剔除不改变任何像素。剔除只作用于**主 pass**（阴影 /
  点光阴影 / 场景深度副本仍按完整实例列表——被遮挡者的投影与深度
  贡献保留，「开启与关闭逐位一致」不被阴影侧打破）；主 pass 批内按
  隐藏表切连续子段（`point_shadow_visible` 先例）。`FrameStats`
  新增 `occlusion_culled`（同时计入 `culled_instances`）。
- **开关与回退**：`OcclusionCulling { enabled }` 资源缺席 = 关
  （默认路径连 GPU 资源都不分配）；读回节拍依赖帧间 `device.poll`
  推进（60fps 事件循环天然满足；headless 背靠背连跑 draw 自动退化
  为全部可见——保守方向，测试/示例以帧间阻塞 `read_pixels` 模拟节拍）。
- **FXAA（3.11 console 质量档）**：`FxaaSettings { enabled,
  edge_threshold = 0.166, edge_threshold_min = 0.083 }` 资源，缺席/关 =
  整段跳过；开启时 blit 改写中间 LDR 纹理（与最终目标同格式同管线，
  输出字节一致）、FXAA pass 采样后写最终目标——链位 blit 之后、UI
  之前 ⇒ HUD 文字不被抹糊（不透明 UI 像素开启前后逐位一致）。
  8 邻域显示域亮度（线性样本 `pow(1/2.2)` 近似回显示空间）+ 亮度
  自适应阈值早退（**非边缘逐位透传中心采样**，8-bit sRGB 编解码
  往返恒等）+ 梯度定向、沿边缘 ±1/6 纹素两点采样、亮度带裁决取点
  或取中。实现教训：链共享 `post_sampler` 是 Nearest，会把亚纹素
  偏移量化回同纹素 ⇒ 整帧无差别透传（changed=0 实测）——FXAA 用
  专属线性采样器（中心取样权重恰 (1,0,0,0)，早退性质不破坏）。
  **TAA 不做**（历史缓冲 + 投影抖动 + 运动向量整合与单帧链架构
  冲突，列后续候选，§8.32⑩）。
- **测试与指标学**：occlusion.rs 4 项单测（`world_aabb` 恒等 /
  90° 精确 / 任意角含面中心 + 代理表一致性）；GPU 遮挡 4 项（全遮挡
  盒稳态 `occlusion_culled == 1`、instances 恰少 1、**开启前后全帧
  像素逐位一致**、首帧无结果恒可见、稳态逐帧逐位、显式关 = 缺席 =
  FNV 锁 0x0369_3e06_193a_9c14、射线隐藏盒回归）；GPU FXAA 1 项
  （缺席 = 显式关 = FNV 锁 0x24e3_8254_0222_7bbb、最大相邻跳变下降 +
  过渡带（10%–90% 窗口）变宽 + 变更占比 < 35% + 不透明 HUD 色块
  逐位不变 + 开启态逐位确定）。教训：总变差（TV）对单调重映射不变、
  判别不了平滑——首版断言假失败，改最大跳变 + 过渡带计数。
- **验收**：`examples/occlusion_fxaa_snapshot.rs` →
  `docs/occlusion_fxaa.png`（上排遮挡 关|开——逐位一致 = 保守性目检，
  半露盒/矮桌反误杀对照；下排 FXAA 关|开——斜条边缘 1–2 px 硬跳变
  → 2–3 px 缓坡、HUD 文字两侧同样锐利）。实测（release，i7-13620H，
  480×270，13 实例）：遮挡剔除 3、主 pass 三角形 116 → 80、
  `Renderer::draw` CPU 墙钟 0.125 → 0.085 ms/帧（−32%）。
- **测试计数**：debug 896 → 905（+4 occlusion.rs 单测、+4 遮挡 GPU、
  +1 FXAA GPU）、release 894 → 903。

### Added（R52c：烘焙光照 c 阶段——GPU 集成与混合语义）

写实路线「间接光」三阶段收官（第 52 轮迭代 c；语义规范全文 =
`iris-render::lightmap` 模块文档 + ARCHITECTURE §4.24 c 小节，诚实清单
§8.31）。把 R52a/b 的 CPU 产物接到渲染器：图集纹理上传 + UV2 顶点属性 +
shader 间接项替换 + 动态光叠加语义。**硬约束：未挂光照贴图的场景渲染
逐位不变**（R52b / R41 两条 FNV-1a 全帧基线锁原值通过）。

- **装配入口 `install_lightmap`**：`LightmapStore`（World 资源）收录
  `BakedLightmap` → `LightmapHandle`；`build_uv2_mesh` 逐实体把图集角点
  坐标写进 `MeshData::uv2`（新增字段，`UV2_LAYOUT` = loc13 / Float32x2 /
  stride 8 独立顶点缓冲，缺席绑常驻零垫）；同一顶点被分进不同 chart 或被
  重叠偏移移到不同位置时**按需拆分顶点**（复制位置/法线/UV/切线/蒙皮后
  追加、索引改指——拓扑不变、`triangles_drawn` 不变）；UV2 是逐实体数据
  而网格是共享资源 ⇒ **每实体一份网格副本**（换 `MeshHandle` 组件）+ 挂
  `LightmapBinding`。局部三角序号 = 全局下标在实体区间内的**秩**（收集
  逐实体连续 + `build_charts` 不丢三角 ⇒ 秩即序号，断言连续性）。
- **GPU 上传**：图集 = `Rgba16Float` 单 mip（线性 HDR **间接**辐照度）懒
  上传 + 句柄缓存（`ensure_lightmap`，照 `ensure_texture` 模式）；
  `f32_to_f16` = 本模块自带 IEEE 754 binary16 RNE 纯函数（免引 `half`
  依赖，全值域解析单测）；采样器全图集共用一份（双线性 + ClampToEdge）；
  未挂图集的材质绑常驻 1×1 黑纹理（创建即零初始化）。
- **混合语义 = 「静态 GI 补动态直射」**：图集存 `BakeSample::indirect`
  （天空 AO + 一次反弹色溢，**不含烘焙直射**——与实时方向光/点光双计会
  过曝）；片元 `间接项 = select(IBL 环境, kd_amb × albedo × E/π, mask)`，
  直接项（方向光 CSM + 点光）不受光照贴图影响；mask = 材质常量
  `MaterialParams.surface.w`（随 (材质, 图集) 绑定组缓存：挂绑定 + 网格有
  UV2 + 句柄在目录内 = 1，否则 0；网格无 UV2 / 句柄悬垂在收集期退回
  `NO_LIGHTMAP` 并告警）。**`select` 不做算术** ⇒ mask = 0 时返回值与引入
  前**同一串位**（逐位不变硬约束的实现根据；写成 `ibl×(1−m) + lm×m` 会把
  约束交给浮点恒等式）。口径对齐 IBL（其 `irradiance_map` 存 E/π）；光照
  贴图项不再乘 orm AO（遮蔽已烘进天空项，再乘是双重 AO）。
- **分批**：材质绑定组缓存键 = `(材质 << 32) | 图集槽位`（同图集共组）；
  绘制段按 (键, 图集) 切（`run_length`）；**排序键不含图集** ⇒ 无光照贴图
  场景的排序与批划分与 R52b 逐位一致。主 / 蒙皮 / 透明 / RTT 四条几何
  通路全部接 UV2 槽位（透明**普通**路径不采样光照贴图，v1 限制）。
- **dilation 填充**：chart 角点 UV 恰落在 texel 边界 ⇒ 双线性对边界两侧
  各取一半、外侧是 pad/间隙黑值 ⇒ 不处理则每条 chart 边一道半 texel 暗线
  （R52b 留给本轮目检的决定）。`dilate_atlas` = 未覆盖 texel 按 4 邻域均值
  迭代扩散（双缓冲、至多 8 轮、只写未覆盖、已覆盖逐位不变）；覆盖判据用
  新增的 `BakedLightmap::covered` 位图而**不是** `!= ZERO`（封闭暗角的真黑
  值必须保留，按零判会漏光）。验收图目检无黑线。
- **验收**：`examples/bake_lightmap_snapshot.rs`（室内盒同场景同相机，
  左原状 / 右挂载横拼）→ `docs/bake_lightmap.png`：受影响像素 99.3%、
  暗部四分位亮度 179.5 → 145.9（**81%**，墙角 AO + 半球遮蔽）、亮部四分位
  226.7 → 215.5（**95%**，直射保持）；目检墙角 / 柱根接触暗化连续无条带、
  柱旁地板带暖红色溢、无 chart 边缘黑线、无闪烁伪影。
- **实测（release，i7-13620H）**：验收场景烘焙 213 ms + 装配 1.3 ms
  （4 实体、11890 覆盖 texel、8 texel/m、256² 图集 = 512 KiB 显存）。
- **测试**：lightmap.rs 新增 9 项（f16 全值域解析 + 可表示值往返、
  dilation 四组解析 + 确定性、UV2 注入逐角点逐位 + 冲突拆分 + 拓扑不变 +
  数量不符 panic、端到端装配、UV2 ↔ 图集坐标逐位 + dilation 前后逐位 +
  墙角 AO 对照、悬垂句柄 panic）；mesh.rs 新增 3 项（UV2 字节计入内存、
  GPU 布局契约、数量不符 panic）；tests/lightmap_render.rs 新增 3 项 GPU
  集成测试（R52b 全帧 FNV 基线锁 `0x3fe5_f3ff_00e0_f329`、烘焙生效四分位
  对照 + 懒上传观测 + 两次渲染逐位、绑定但无 UV2 逐位退回 IBL）。
  计数：debug 881 → **896**、release 879 → **894**（iris-render lib
  debug 270 → 282 / release 266 → 278，新增 15 项）。
- **偏离与说明**：①规范「shader 用 UV2 直接采样 + AO 强度常量入材质」
  落实为 **mask 入材质 uniform（`surface.w`）+ 绑定组按 (材质, 图集) 缓存**
  ——UV2 已是图集坐标 ⇒ 采样零常量，per-instance 常量不需要；②规范未提
  dilation，本轮目检确认 chart 边缘黑线必须处理，故新增（R52b 文档预留的
  决定）；③透明普通路径不采样光照贴图（与视深排序分批冲突），透明蒙皮
  沿用蒙皮解析结果——规范只要求「蒙皮通路同样传 UV2」；④`BakedLightmap`
  增 `indirect` / `covered` 两个字段（上传口径与 dilation 判据的真值来源，
  用 `total()` 或 `!= ZERO` 推断都会错）。

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
