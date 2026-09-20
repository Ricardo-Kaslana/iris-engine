# R94 水下 + 焦散（路线图 v5 · 第三梯队收官役下半 · 设计定稿）

> 2026-09-20。R93 拆轮的下半（裁定 5）。闭合 §8.50⑤「相机入水无处理」。
> 主会话直接实现（R93 起的现行模式）。

## 〇、四项裁定（按推荐定稿）

**裁定 1 · 水下厚度判据 = 解析平面求交钳制，不改水 pass 的深度写入。**
候选原本是「相机入水时让水/海洋 pass 把水面视距写进 scene_depth（或新增一张
`water_depth` 掩膜）」。实测推演否决：水体 pass 已经在**同帧读** scene_depth
（R81 第六消费者），要写同一张纹理就是同 pass 读写 hazard，只能新增一张全
分辨率掩膜 + 给水体/海洋 pass 各加一个颜色附件（并把「水像素不写
scene_depth」这条既有语义翻掉 ⇒ §8.50⑥⑨ 两串锁要重裁）。
定稿走纯解析路线：水下列长 = `min(scene_depth, d_plane)`，其中
`d_plane = d · (h − eye.y) / (world_y − eye.y)`（视线向上出射时才有效，
`world_y` 由 scene_depth + `inv_view_proj` 重建，与 fog.wgsl 同一纹素中心
口径）。视线水平或向下 ⇒ 列长 = `d`。零新资源、零附件、零既有语义变动，
代价是**把波面当成平面 `h`**（浪高不参与列长，诚实声明）。

**裁定 2 · 链位 = 雾之后、DOF 之前，独立全屏 pass。**
理由：空气雾是「场景内的气溶胶」、水是「相机所在介质」，两者同开时先雾后水
在物理层次上正确；且相机在水下时 DOF / MB / 泛光都该作用于已被水吸收后的
颜色（水下亮源不该按空气中的亮度去提泛光）。
槽位机器按 R79 先例：新函数 `full_chain_slots_with_water_column(dof, mb,
vignette, fog, underwater)`，**两个既有函数一字不动**；`underwater = false`
时返回的 `slots` 与 `full_chain_slots_with_fog` 逐字段相同（拓扑等价锚），
开启时整条相机级链再轮转一格（雾占 A ⇒ 水下占 B ⇒ DOF 写对面 …）。
不变式测试照抄 fog 那两条（关闭逐字段等价 + 全组合「读槽 ≠ 写槽」「读到的
是前驱最新输出」）。

**裁定 3 · 焦散 = 水下 pass 内的屏幕空间程序化项，不进 PBR。**
候选「给 standard.wgsl 加水下焦散贡献项」被否：那要动 fs_shaded 本体并覆盖
meshlet / 蒙皮 / 发带全部前向路径（R63/R90 的 FMA 漂移面），且需要水底法线
而 post 链只有深度。定稿：由 scene_depth 重建世界 XZ，双频余弦干涉式
`caustic(p, t)`（无状态纯函数、CPU 逐式镜像 + 源码看门，R92 涟漪同族），
按列长衰减加权后加性叠到水色上。诚实声明：无朝向判定（垂直墙面也带亮斑）、
不参与光照与阴影、不被焦散照到的物体没有能量守恒。

**裁定 4 · 资源形态 = `UnderwaterFog` 资源即开关，缺席 = 整段不入链。**
不做组件（水下是相机状态不是实体属性），也不自动跟随（与 R92 `Weather`
同族：引擎不猜「用户想不想要水下」）。字段 6 个公开旋钮，每个配一条非默认
值断言（R89 铁律）：`color`（吸收色）/ `absorb_depth`（特征深度）/
`sun_inscatter`（太阳内散射强度）/ `sky_window`（Snell 窗与水面方向的补光）/
`caustic_strength` / `wobble`（屏幕空间折射抖动）。
入水判定在渲染器每帧做：资源在场 **且** 相机 `eye.y` 低于选中水面
（海洋优先、池塘次之，与 R93 互斥同序）。

## 一、范围

- 新模块 `iris-render/src/underwater.rs`：资源 + 判定纯函数 + 列长纯函数 +
  焦散纯函数 + 链槽位纯函数 + CPU↔WGSL 镜像。
- 新着色器 `shaders/underwater.wgsl`：复用 `post_depth_layout`
  （params / src / sampler / scene_depth 四槽，零新布局）。
- `pipeline.rs`：`underwater_pipeline`（`make_post_depth_pipeline` 同族）。
- `renderer.rs`：`UnderwaterParamsRaw` + 参数上传 + 编码（雾后 DOF 前）+
  `scene_or_taa` 源切换 + FrameStats。
- `iris-engine/scene.rs`：环境段 `underwater`（往返 + 退化拒绝 + 旧档零变化）。
- 验收图 `docs/underwater.png` 三面板：水下看海床（焦散 + 吸收）| 资源关闭
  对照（= 引入前逐位）| 水下仰望水面（验证平面钳制列长让 Snell 窗不被吞）。
- 文档：ARCHITECTURE §4.53 + §8.73、README、CHANGELOG、AGENTS.md。

## 二、不在本轮（明确拒绝清单）

水下体积光柱 / god rays（需屏幕空间深度重建的第二遍或 raymarch，成本另档）；
水下音效与滤波（iris-audio 侧，另轮）；浮力体入水的溅射 / 水面扰动（R92
无落地交互的既有边界）；相机穿越水面的过渡帧（不插值，硬切换 + 声明）；
水像素写 scene_depth（裁定 1 否决）；水下折射的逐像素真实光追（`wobble`
是屏幕空间近似）。

## 三、兼容铁律

`UnderwaterFog` 缺席 ⇒ 不建 pipeline 之外的任何资源、零 `write_buffer`、
零 pass、链槽位拓扑逐字段等价 R93 现状 ⇒ 像素与命令流逐位不变。既有全部
像素锁（water / weather / post / fog / oit / shadow_v2 / sss / normal /
transparent / fur / ocean）必须**原值通过**；`post_render.rs` 的 13 项与
`sky_fog_render.rs` 是本轮最可能被牵动的两套（链拓扑相邻），零改动是硬标准。


---

## 四、实现回写（同日，主会话实现）

四项裁定全部按定稿落地，零范围裁剪。实现期新增的两处认知（都不在定稿里，
是撞上才知道的）：

1. **散射项必须带水色着色**。定稿写的 `L_ins = sun_color × strength × (1−T)`
   在验收图上把整幅洗白（太阳辐照 3.0 × 0.35 ≈ 0.84 的白覆盖）。物理上水
   分子散射后的光带着水的颜色，故改为 `water_color × sun_color × …`，散射
   亮度自然落到 0.1 量级；三个亮度旋钮（`sun_inscatter` / `sky_window` /
   `caustic_strength`）随之重标量级。焦散**保持不带水色**——它是透过波面
   聚焦的日光，落在水底上是太阳色。
2. **吸收单调性只在纯吸收档成立**。GPU 测试首稿拿默认档断言「absorb_depth
   越小越暗」，实测反向（219.7 vs 217.4）。归因：路径辐射
   `bg·T + L_ins·(1−T)` 的第二项随消光**增大**而抬亮，把吸收的变暗吃掉——
   这是物理正确的行为，不是 bug，但用它当吸收判据就是错的指标。改为把三处
   加性项全关后再断言，并补一条「换吸收色必须体现在 R−B 符号翻转」的色相
   判据（单看亮度会漏掉「色相没生效」这一类错）。

另外两处工具性教训：单文件 30+ 只 `OffscreenRenderer` 会让驱动侧在第五六个
测试返回 `None`（每个测试单独跑都过，整文件跑挂）⇒ 改成一测试一只设备，
各对照臂复用（世界由同一构造函数生成、句柄与内容一一对应，不触发 AGENTS.md
「同句柄不同内容串味」的前提）；亮度求和 `px[i]+px[i+1]+px[i+2]` 是 u8
相加，debug 下溢出 panic ⇒ 先升 u32。

WGSL 侧一条格式约束：`clippy::doc_lazy_continuation` 会把以 `+` 开头的文档
行当成 markdown 列表续行报错；`doc` 注释里避免行首 `+`。

验收：`docs/underwater.png` 三面板目检通过（青绿水体 + 网状焦散落在海床上
并随距离衰减 + 从下方可见波面）；焦散开 / 关海床条带差分 2498 px；
基线 debug 1792 / release 1789（+16）；post 13 / sky_fog 5 / water 4 /
ocean 6 / weather 6 既有像素锁原值通过。
