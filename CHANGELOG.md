# Changelog

本文件记录 Iris 引擎的所有重要变更。格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本遵守 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

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
