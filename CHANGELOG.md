# Changelog

本文件记录 Iris 引擎的所有重要变更。格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本遵守 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

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
