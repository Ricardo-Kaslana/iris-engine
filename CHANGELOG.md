# Changelog

本文件记录 Iris 引擎的所有重要变更。格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本遵守 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

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
