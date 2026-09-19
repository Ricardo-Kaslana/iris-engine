# R89 DDGI v2 设计定稿

**轮次**：R89 · 路线图 v5 第三梯队「全局光照」2/2（R88 DDGI v1 的收官轮）
**定稿日期**：2026-09-18
**状态**：已落地（2026-09-18，R89 收官）。语义权威见
`ARCHITECTURE.md` §4.45 / §8.58 与 `iris_render::ddgi` /
`iris_editor::edit` 模块文档；实现与设计稿的偏离（8 条）记录在
`CHANGELOG.md` R89 的「偏离声明」段，本文件转为历史决策记录。
**权威关系**：本文件是 R89 的**开工前设计定稿**；实现落地后语义权威仍是
`ARCHITECTURE.md`（新增 §4.45 + §8.58，并把 §8.57⑫⑬⑰⑱ 相应条目移入实现或改写）
与 `iris_render::ddgi` / `iris_editor::edit` 模块文档，本文件转为历史决策记录。
**开工前设计定稿的上一轮留档**：`docs/ddgi_v1_design.md`（R88）。

---

## 1 范围与目标

**路线图原样范围**（`docs/private/roadmap-v5.md:119`）：
「与烘焙光照贴图 / 反射探针 / SSR 的贡献合成语义 + 编辑器探针放置入口」。

**本轮实定范围 = 四桶**：

| 桶 | 内容 |
|---|---|
| A | **合成语义五方定稿**：贴图 / env 兜底 / ambient 补光 / 反射探针捕获 / SSR |
| B | **两个既有 bug**：SSR 强度死旋钮（本轮调研新发现，26 轮未发现）、`reset_taa` 漏清跨帧时域状态（frictions ⑬） |
| C | **编辑器 DDGI 入口**：前置修正（GI 几何收集排除叠加层）+ 参数步进 + 可视化 + 入档 |
| D | **明确不做清单**（点光参与 / 天空卷积 / 差值法 / 探针实体化 / 新片元 binding） |

**桶 A 的实质产出**是一条覆盖度量的连续化（§5），它一次性解决 §8.57⑱（扇贝形
亮带）与 §8.57⑬（ambient 天空底色双计）两条诚实限制——这两条在 R88 都是
「已声明未解决」，且 `docs/ddgi.png` 里那条波浪带是实测证据。

**关键可实现性前提**：本轮**不需要任何新的片元阶段 storage 缓冲**。R88 已把
片元只读 storage 占满 4/4（binding 16/17/18/20 = WebGPU 默认
`maxStorageBuffersPerShaderStage` 上限，§8.57⑮）。覆盖度 `cov` 是着色器**内**
由已有探针表算出的量，不引入新数据流 ⇒ 不触这条上限。反过来说：**任何需要
新片元 binding 的方案（点光表、天空 SH 常量以外的数据、探针实体化后的第二
张表）在 R89 都不可实现**，这是 §11 不做清单的硬依据之一。

---

## 2 地基与复用清单（带行号，一律复用不重写）

### 2.1 R88 的 DDGI 面（`crates/iris-render/src/ddgi.rs`，3374 行）

| 复用面 | 位置 |
|---|---|
| 常量 | `DDGI_DEPTH_GRID = 8`:155、`DDGI_DEPTH_TEXELS`:161、`DDGI_DEPTH_FLOATS`:164、`DDGI_DEPTH_TABLE_BYTES`:170、`DDGI_SKY_DEPTH = 1e4`:183、`DDGI_SIGMA_REL = 0.1`:193、`DDGI_PREFILL_GRID = 2`:204、`DDGI_RAYS_PER_FACE_CHOICES = [8,16,32,64]`:211 |
| `DdgiSettings`（资源，缺席 = 关） | `:250`，校验 `validate:296`，`alpha:332` |
| 网格规划纯函数 | `DdgiLayout:346`、`axis_points:368`、`stride_for_capacity:378`、`plan_layout:406`、`layout_positions:450`、`triangles_bounds:476`、`default_bounds:496` |
| 滚动纯函数 | `advance:516`、`sh_hysteresis:549`、`depth_hysteresis:563`、`relight_fingerprint:581`、`grid_fingerprint:607` |
| 重照明 | `depth_moment_finish:631`、`neutral_depth_moment:644`、`depth_cell_of_ray:655`、`probe_relight:680` |
| 深度感知混合 | `ddgi_depth_face:751`、`ddgi_depth_texel:778`、`ddgi_depth_weight:823`、`ddgi_depth_weight_at:842`、**`ddgi_blend:874`（本轮的 CPU 镜像扩展点）** |
| 状态与观测 | `DdgiStats:928`、`DdgiGrid:954`（`probe_stamp:1022`、`structure_generation:1026`、`geometry_generation:1030`、`grid_fp:1034`、`bvh:1039`、`instance_payload`） |
| 引擎侧编排 | `crates/iris-engine/src/ddgi.rs:46/59/103`（`ddgi_bake_params` 纯函数 + Startup 插空网格 + `tick_ddgi`），注册在 `DayNightPlugin` 之后 |
| 着色器 | `standard.wgsl`：binding 19 参数语义注释 `:235-237`、`ddgi_depth_face:1031`、`ddgi_depth_texel:1064`、`ddgi_depth_weight:1107`、`ddgi_irradiance:1141`（`:1159` 是双剪枝处）、**mode 门三支 `:1286-1294`**、`ibl_env:1350-1351`、**ambient 补光行 `:1397-1400`** |
| 源码层看门先例 | `ddgi.rs:1016` 注释所指的 `wgsl_ddgi_source_matches_cpu_contract` |
| R88 GPU 锚（6 项） | `tests/ddgi_render.rs`：`absent_paths_are_bitwise_pristine:351`、`ddgi_replaces_uniform_sky_with_occlusion_aware_gi:429`、`depth_weight_suppresses_leakage_through_the_wall:526`、`rolling_convergence_matches_full_relight:685`、`two_sessions_are_bitwise_identical_over_frames:759`、`daynight_change_is_followed_within_convergence_window:822`。**注意：R88 无 FNV 绝对常量锁**（用三种缺席配置逐字节相等代替），**且没有任何合成测试** |
| R88 快照 | `examples/ddgi_snapshot.rs`：断言全部是**运行时自洽 + 面板间 `assert_ne`**（`:374` 关/开可判别、`:408` 深度项开/关可判别、`:435` 昼/夜可判别、`:455` 双会话逐位）⇒ **没有硬编码绝对值 ⇒ 本轮重生成图不需重锁程序**（§8.2） |

### 2.2 合成面对手戏

| 路径 | 事实位置 |
|---|---|
| 光照贴图掩码 | CPU 裁决 `renderer.rs:2276-2295`（`resolve_lightmap` 四条退回）、写进材质绑定组 `:11050`（`mask = contains_key`）入 `surface.w` `:11059-11064`、缓存键 `material_key:11012`；着色器 `select(ibl_env, ibl_lightmap, material.surface.w > 0.5)`（`standard.wgsl:1355` 区段） |
| **图集实体不发探针标志位** | `renderer.rs:2308-2311`：`let probe_payload = \|position, lightmap\| { if lightmap != NO_LIGHTMAP { return [0.0; 4]; } …`；注释 `:2298`「**光照贴图优先**…CPU 侧就不再发标志位（两道保险）」；口径声明另见 `irradiance.rs:162-164`；DDGI 分支在同一守卫之后（`:2312-2316`）⇒ **图集实体同样吃不到 DDGI** |
| 贴图承载的内容 | `bake.rs:733-736`：bounce 项 = `albedo × (direct_irradiance(命中点) + π·sky_color)/π` ⇒ **方向光的弹射已经在贴图里**；`BakeSample::indirect() = sky + bounce`（`:695-697`），上传这份（`:970`）⇒ 与 DDGI 承载同一份天空 + 一次弹射 ⇒ **相加必双计** |
| 反射探针镜面链 | `standard.wgsl:1326`（`spec = env_spec`）→ `:1327`（`best_w > 0`）→ `:1337`（探针 cubemap 采样）→ `:1343`（`spec_refl_w = spec * (f_amb·brdf.x + brdf.y)`）→ `:1344-1348`（SSR 覆写）→ `:1350-1351`（入 `ibl_env`）；图集掩码替换 `let ibl = select(ibl_env, ibl_lightmap, material.surface.w > 0.5)` 在 `:1366`；图集实体的探针镜面补项 `:1372-1377`（条件 `surface.w > 0.5 && best_w > 0`） |
| **捕获相机恒 mode 0** | 绑定构造 `renderer.rs:3500-3530`：GI 绑 `gi_sh.off_probes_buf/off_params_buf`（`:3516-3517`）、**DDGI 绑 `ddgi.off_depth_buf`（`:3518`）**、SSR/AO 哑（`:3521-3524`）；注释 `:3496-3499`「捕获不做探针自反馈 / 不做屏幕空间反射 / 不渲染锥光与第 9+ 盏点光」；立场文档 `:1855-1863`、`:2303-2305` |
| SSR 上帧颜色 | 帧尾副本 `renderer.rs:8787-8805`（源 `targets.resolved` = 主解析缓冲 MSAA 解析 + 天空 + 透明 + 粒子的最终结果，注释 `:8781-8783`）；后处理链恒写 `PingPongSlot::A/B`（`post.rs:981-1004`）⇒ **副本含全部 GI、不含后处理** |
| `env_params.y`（IBL 强度） | 只乘两处：`standard.wgsl:1351`（`ibl_env` 整体）与 `:1376`（图集实体镜面补项）；`EnvironmentSettings` = `environment.rs:26-34`（默认 1.0，缺席 = 1.0：`renderer.rs:4773-4776`、`:6356-6359`）；**关卡格式不序列化它**（`scene.rs:1388-1391` 的 environment 段只有 `directional_light` / `sky_source`），各游戏代码插入（如 `games/iris-assault/src/plugin.rs:332-333`） |
| `globals.ambient` | CPU 写 `renderer.rs:6370`（主帧）/ `:4821`（捕获面）；源 `DirectionalLight.ambient`（`light.rs:221`，默认 `splat(0.14)` `:240`），序列化 `scene.rs:408/427`；消费 `standard.wgsl:1397-1400`，**不乘 `env_params.y`**；既有论述 `ARCHITECTURE.md:2640`（「不是接管美术补光 `ambient` 项」）、`:2783`、`:4815-4816` |
| 协同语义文档 | §8.31④ = `ARCHITECTURE.md:5765-5772`；R83 正交论述 §4.39 = `:4290-4292`（着色器同声明 `standard.wgsl:1283-1284`）；R88 现状 §8.57⑫ = `:6335-6338` |
| 合成测试锚 | `tests/lightmap_render.rs:31` `R52B_BASELINE_HASH = 0xf232_16a3_37fb_f385`；`tests/irradiance_render.rs:29/31` 两把锁 + `probes_do_not_touch_lightmapped_entities`（消息：「只挂光照贴图的场景装探针后必须逐位不变」）；`tests/probe_render.rs:723` `probe_restores_specular_on_lightmapped_entities`（协同语义唯一定点）；`tests/ssr_render.rs:36` `R53_FXAA_OFF_BASELINE = 0x24e3_8254_0222_7bbb` + `:846` `ssr_follows_reset_taa_skip_frame_is_bitwise_pristine` |

### 2.3 编辑器面（`crates/iris-editor/src/`，命令层 3391 行 + 交互壳 545 行）

**范式事实**：**没有命令对象**（无 `struct + apply/undo`），全部是自由函数
`pub fn xxx(world: &mut World, …) -> Option<T>`；undo 靠**快照栈**而非逆操作。

| 复用面 | 位置 |
|---|---|
| undo 骨架 | `WorldSnapshot:1400`（字段含 `directional: Option<DirectionalLight>:1413`）、`capture_snapshot:1423`、`push_undo:1491`、`restore_snapshot:1600`、`undo:1541`、`redo:1571`、`UNDO_LIMIT = 32`、`clear_undo_history:1516`（幂等重跑类不入栈） |
| **光源编辑模板**（本轮的直抄对象） | `LightEditMode{Directional,PointLight,Inert}:1686` + 模式裁决 `light_edit_mode:1696`；步进函数体固定四步（`push_undo` → 改资源/组件 → `set_status` → `refresh_hud`）：`light_intensity_step:1776`、`light_range_step:1814`、`light_hue_step:1825`、`light_shadow_toggle:1836`、`sun_ambient_step:1848`、`sun_direction_step:1870`；摆放 `place_light_at:821` / `place_light_from_ray:866` |
| 烘焙入口模板 | `BakeReport:1922` + `bake_and_install:1945`（参数取 `LevelRegistry.section.bake`，缺席回落默认；先 `save_level` → 建**规范影子世界**重跑 `load_scene` → `bake_lightmap` → 产物落盘 → 按清单序重映射回活动世界 → `clear_undo_history`） |
| 可视化先例 | **无独立 pass、无专用 binding**——叠加层就是走主 pass 的普通实体 + `EditorTransient:452` 标记 + `TransientAssets:461`（1 个 cube 当线网格 + 6 个材质），`build_transient_assets:2449`（注释：**必须在内容之前创建**，「句柄编号对齐」不变量）；NavGrid 叠加 `build_nav_viz:2247`（拉扁 cube + 半透明红 `AlphaMode::Blend:2476`，两阶段收集→spawn）、地面网格 `rebuild_grid:2521` / `spawn_line:2564`、选中框 `rebuild_gizmo:2586` + 纯函数 `gizmo_edge_transforms:2648` + 原位更新 `update_gizmo:2615`、点光灯泡 `LIGHT_BULB:233` + `bulb_def:411`；过滤 `editable_entities:968`；存前 despawn `save_level:627`；HUD `spawn_hud:2918` / `panel_text:2826` |
| 交互壳 | `EditorPlugin:83` + `build_schedule:111`；两阶段输入 `FrameInput:204` → `read_input:265`（纯读）→ `apply_input:342`（按固定优先级翻译成命令层调用）；键位表 `shell.rs:6-40`；拾取 `screen_ray_at:2989`（用 `Viewport`+`Camera` **资源**，刻意不挂 `ActiveCamera` 实体）、`pick:992` / `pick_with_hit:986` / `pick_ray:997`、`placement_point:1022`；**无 tool/brush 概念**，最接近的是 `PlaceKind:290` 四态 Tab 循环；拖拽 `begin_drag:1089` / `drag_to:1108` / `end_drag:1150`（拖拽中禁 undo） |
| headless 测试台架 | `tests/edit_headless.rs`：`temp_dir:53`（pid + 调用序计数，R63 教训）、世界指纹 `dump_world:137` / `dump_world_sorted:146` / `store_digest:328`；apply→undo 逐字符复原的样板 `point_light_edits_undo_bit_exact:1551`（含 `undo_depth==5` 断言）、`directional_light_edits_undo_bit_exact:1621`、`undo_redo_multi_step_sequence_with_full_recovery:1333`；模式裁决 `light_edit_mode_dispatch:1679`；烘焙闭环 `bake_closed_loop_roundtrip:1718`；导航端到端 `nav_rebuild_and_viz_end_to_end:1863`；「无 `EditorState` 则 no-op」契约 `commands_are_inert_without_editor_state:1204` / `b_stage_commands_are_inert_without_editor_state:1519`；不入档 `transient_visuals_never_reach_the_file:557` |
| **既有拒绝清单（本轮必须尊重）** | `edit.rs:150-154` 已把「bake / nav 参数的**数值编辑面板**」列入拒绝——理由：`LevelRegistry` 的**加载时快照**语义（`level.rs:123`）需升级为「现读 + 回写」，属 §8.33② 语义变更、单独一轮评审；`edit.rs:169` 面板是只读 8×8 ASCII 位图字体、**无文本输入框**；`edit.rs:155` Nav 叠加「手动重建、不自动刷新」是已声明的诚实第 4 条 |

### 2.4 序列化面

`SCENE_FORMAT_VERSION: u32 = 2` 在 **`scene.rs:341`**（不在 `level.rs`）；容器
`SceneDoc:347-355`；迁移表 `MIGRATIONS:363` / `migrate_v1_to_v2:367`。可选段
先例 `LevelSection:159-167`（三子段全 `#[serde(default, skip_serializing_if =
"Option::is_none")]`，`is_empty():171` ⇒ 整段省略，**版本号不 bump**）；子段
内写法 `NavSection:441`、`BakeSection:574`（逐字段 `#[serde(default = "…")]`、
`artifact: Option<String>` 缺席 = 只存参数不挂产物）；字段级先例
`MaterialSection.double_sided:657`、`subsurface_strength/scale:662/668`；字节
不变测试 `scene.rs:2757/2822/2689`、`level.rs:1707/1795`、
`edit_headless.rs:1040/1087/530`。**`EntitySection:461-481` 九个可选字段
（name/visible/transform/mesh/material/point_light/particles/collider/
rigidbody）无探针字段 ⇒ `IrradianceProbe` 现不可序列化**；反射探针的既有拒绝
在 `level.rs:131`（「不入档——由宿主运行时挂载并调 `bake_reflection_probe`」）。

---

## 3 设计决策定稿

### 3.1 用户裁定（2026-09-18，用户为项目唯一决策人）

| # | 议题 | 裁定 | 落地 |
|---|---|---|---|
| 1 | R89 本轮切多大 | **合成语义 + 编辑器入口 + 两个 bug**（四桶全做） | §1 / §6 / §7 |
| 2 | DDGI × 烘焙光照贴图 | **维持互斥，升级为显式裁决**（不做差值法） | §4.1 |
| 3 | 扇贝带 ⑱ 与 ambient 双计 ⑬ | **用同一个覆盖度 `cov` 一起修** | §5 |
| 4 | 编辑器「探针放置入口」做到哪层 | **参数步进 + 可视化 + 入档**（不实体化、不做数值面板） | §7 |

### 3.2 主会话定稿（附理由）

| # | 决策 | 理由 |
|---|---|---|
| 5 | 覆盖度定义取「**到探针球面的带内深度**」`max_i clamp((r_i − d_i)/(r_i·0.25), 0, 1)`，而非 `Σw` 归一化 | `Σw` 类定义在网格**内部**就给不出 1（单探针半程处窗权仅 0.56），会把已经正确的像素削弱掉；取「最近探针的球内深度」可保证半径内 75% 区域 `cov ≡ 1` ⇒ 改动面被结构性地限制在外壳 |
| 6 | `cov` 无量纲（分子分母同含 `r_i`） | 尺度无关 ⇒ 换 spacing 不改羽化带宽；若用绝对带宽（米），粗网格的羽化带会吃掉整个探针 |
| 7 | ambient 衰减写成 `× (1.0 − cov)`，**ambient 那行的语句不动**（只在 mode 2 分支给 `cov` 赋值，mode ≤ 1 时 `cov` 恒 0） | `× 1.0` 在 f32 对任何有限值逐位恒等；且**同族的无条件乘法已有先例**——R77 的 `ao_ss` 就是靠「哑纹理纹素恒 1.0」实现零分支恒等（`standard.wgsl` GTAO 段），不是靠 `select` 的纯值选择 |
| 8 | 混合写成 `mix(env, gi, cov)` 而非新增 `select` | `mix(x,y,1.0) = x·0 + y·1 = y` 精确 ⇒ `cov = 1` 区域逐位等于 R88；连续过渡本来就是目标，`select` 只能给硬断界 |
| 9 | 差值法（贴图静态 + DDGI 动态增量）**明确拒绝** | 需要①持久化基线探针表、②每探针二次重照明对照、③负增量钳位、④「贴图烘焙参数与 DDGI 网格参数严格同源」的脆弱隐性契约。收益是「同空间两套 GI 混用」，而这是美术流程可以自行规避的选择（一个关卡一套 GI） |
| 10 | 捕获相机**维持 mode 0**，但把「捕获吃 DDGI 的 L0 载荷」从「未修订」改写成显式裁决 + 测试锚 | R88 的 `DdgiGrid::instance_payload` 已经接上了，**缺的只是断言**；让捕获走 mode 2 会引入「捕获相机看墙后」的新语义与探针自反馈风险，而反射里的间接光是低频量 |
| 11 | SSR × DDGI **零代码改动**，只补论证与锚 | SSR 读的上帧颜色 = 主解析缓冲帧尾副本（含全部 GI）⇒ 一致性天然成立（§2.2 事实） |
| 12 | GI 几何收集排除用**新 marker 组件 `BakeExcluded`**，而非给 `collect_bake_triangles` 加排除谓词参数 | 该函数是 lightmap / 探针 / DDGI **三个消费者**共用的纯 `&World` 函数，加参数要三处穿线且 DDGI 的 `tick` 拿不到排除集；marker 组件的「缺席 = 收录」语义与仓库既有口径（`Visibility` 缺席 = 可见）一致，且编辑器一处打标全覆盖 |
| 13 | 编辑器入口**不碰数值输入面板**，全部走键位步进 | `edit.rs:150-154` 的既有拒绝（牵连 §8.33② 的 `LevelRegistry` 语义变更评审）；步进形态已有六个光源函数作模板，且 `rays_per_face` 本来就是四档枚举 |
| 14 | `WorldSnapshot` 必须加 `ddgi: Option<DdgiSettings>` 的 capture/restore | 快照现在只含 `DirectionalLight` + `LevelSection`；不加则 undo 不还原 DDGI 参数 = 静默的撤销栈谎言 |
| 15 | DDGI 参数入档走 `LevelSection.ddgi`，**不 bump 版本** | 三处先例（nav/bake 子段 + `MaterialSection` 字段级）；`LevelRegistry` 是加载时快照，所以「资源 → 段」的回写只能发生在 `save_level`（照 `DirectionalLight` 的现有写法） |
| 16 | SSR 强度修复取 `clamp(fade × flags.y, 0, 1)` | 强度 0 ⇒ 与「SSR 关」逐位一致（物理正确：零反射）；钳上界防过冲（混合因子 > 1 会把 `spec_refl_w` 反号甩出去）；**全仓无人设过非默认强度 ⇒ 既有基线零风险**（§6.1） |
| 17 | SSR 修复**必须同时补 CPU 镜像 `ssr_blend_weight`** | 现有镜像只有 `ssr_roughness_fade` / `ssr_edge_fade` 两个**分量**（`post.rs:331/341`），合并权重没有镜像函数 ⇒ 这正是强度漏乘能藏 26 轮的结构性原因。补合并项的镜像 + 单测，等于把整条公式钉在 CPU 侧 |
| 18 | `reset_taa` 补清 `ao_depth_frame`，`prev_view_proj` 待实现期核实后再定 | 前者是 frictions ⑬ 实锤且修法明确（一行 + 断言）；后者牵涉运动模糊首帧语义（清了是否等价「无历史」需实测），不预设结论 |
| 19 | 点光/聚光/emissive 参与重照明**不进 R89** | 与 R88 的滚动模型正面冲突：光源一动即全表标脏 ⇒ 1.7 ms/帧 + 2.1 s 收敛，火把照明要 2 秒才起效。解它需要「局部子集重照明 + 固定归约序」专章，与 §8.57⑪（并行化）天然同轮 ⇒ 建议独立立项「DDGI 性能与动态光源」 |

---

## 4 桶 A：合成语义定稿（逐条）

### 4.1 A1 · DDGI × 光照贴图 = 互斥，升级为显式裁决

**现状**：图集实体的 `probe_payload` 恒 `[0.0; 4]`（`renderer.rs:2308-2311`），
DDGI 分支在同一守卫之后 ⇒ 挂图集实体**完全吃不到 DDGI**。物理上这是对的：
贴图已含 `sky + bounce`、且 bounce 里带方向光弹射（`bake.rs:733-736`），与
DDGI 承载同一份贡献，相加即双计。

**定稿**：
1. 语义不变（零像素改动），但**从「未修订」升格为「已裁决」**：§8.57⑫ 移入
   实现，ARCHITECTURE 新增 §4.45 写清「同一空间两套静态/动态 GI 二选一，
   挂图集 = 贴图赢」的裁决与理由（含裁定 2 拒绝差值法的依据）。
2. **补判别性测试锚**（R88 缺）：`ddgi_does_not_touch_lightmapped_entities_bitwise`
   ——同一场景「DDGI 在场」与「DDGI 缺席」两跑，挂图集实体所在区域逐位相同。
   现有 `probes_do_not_touch_lightmapped_entities` 只测了 R63 时代的烘焙集。
3. 编辑器 `bake_and_install:1945` 路径：若 `DdgiGrid` 非空，`BakeReport` 增
   一条状态提示（「本关卡同时挂了 DDGI 网格与烘焙贴图，二者对同一空间互斥」）
   ——**只是提示，不做自动裁剪**。

### 4.2 A2 / A3 = 覆盖度连续化（见 §5，本轮核心算法）

### 4.3 A4 · DDGI × 反射探针捕获

**定稿**：捕获恒 mode 0（维持 R88 立场与 `renderer.rs:3518` 的哑绑定），但把
「捕获画面的间接漫反射来自 `DdgiGrid::instance_payload`（零阶径向混合，**不含
深度项**）」写成显式裁决，并补锚
`probe_capture_uses_ddgi_l0_payload`——断言：DDGI 表改变 ⇒ 捕获画面改变，
而 `IrradianceProbeSet` 被忽略（互斥）⇒ 烘焙集改变 ⇒ 捕获画面不变。
**新增诚实限制**：反射画面里的间接漫反射无方向性、无遮挡感知（低频量，可接受）。

### 4.4 A5 · DDGI × SSR 与切镜

**定稿**：零代码改动 + 两条断言。
1. `ssr_reflects_ddgi_gi_bitwise`：DDGI 在场时 SSR 命中色随 GI 表变化（证据链
   = 帧尾副本源 `targets.resolved`）。
2. `reset_taa_does_not_disturb_ddgi`：`reset_taa()` 后 `DdgiGrid` 的表内容、
   `probe_stamp`、`structure_generation`、`ddgi_upload` 记账**全部逐位不变**
   ——探针域状态与镜头无关，这是**正确的不清**（与 B2 里「该清而没清」的
   `ao_depth_frame` 形成对照，两条一起构成「时域状态失效面」的完整台账）。

---

## 5 覆盖度连续化（本轮核心算法，修 §8.57⑱ + §8.57⑬）

### 5.1 问题

- **⑱ 扇贝带**：`Σw ≤ ε` 的兜底是 env 常量（R88 决策 11），而探针影响半径是
  **有限球** ⇒ 越过某条等值线整片切到 env，边界是「半径球面并集」的截面 ⇒
  扇贝形亮带。`docs/ddgi.png` 天花板与墙交接处那条波浪带就是它（探针层高
  1.3、半径 2.4 ⇒ 只够到 y = 3.7，墙顶 y = 4 以上无人接管）。
- **⑬ ambient 双计**：`sky_color` 默认取 `DirectionalLight.ambient`，而
  ambient 同时是 `standard.wgsl:1397-1400` 常量补光项的来源 ⇒ 同一旋钮两处
  消费、天空底色双计。

两者是同一个量的两面：「**这片像素有多少比例的间接光来自探针**」。

### 5.2 定义

```text
cover_i = clamp((r_i − d_i) / (r_i × DDGI_COVER_FADE), 0, 1)      // 球面外 0，深入带内 1
cov     = max_i cover_i                                            // 固定序循环，与 Σw 同遍
```

`DDGI_COVER_FADE = 0.25` ⇒ 半径最外 25% 是羽化带，`d ≤ 0.75·r_i` 时 `cov ≡ 1`。

**为什么不用 `Σw` 归一化**（决策 5）：`Σw` 在网格内部就达不到 1（单探针半程
处窗权 = `(1 − 0.25)²` = 0.5625），会把 R88 已经正确的像素削弱；`max_i` 的
「带内深度」定义把改动面**结构性地限制在外壳**。

### 5.3 两个消费点

```wgsl
// ① env ↔ GI 交叉混合（修 ⑱）：把硬 select 换成连续 mix
let ddgi = ddgi_irradiance(in.world_pos, n);        // 出口扩成 (gi, cov)
irradiance = select(env_irradiance, mix(env_irradiance, ddgi.xyz, ddgi.w),
                    in.probe.w > 0.5);
// ② ambient 补光衰减（修 ⑬）：ambient 行的语句不动，只多乘一个 (1 − cov)
color = color + ibl_final * ao_ss
      + mix(albedo, f_env, metallic) * globals.ambient.rgb * ao * ao_ss * (1.0 - ddgi_cov);
```

`ddgi_cov` 在 mode ≤ 1 分支恒 `0.0`（不进任何算术）⇒ `× (1.0 − 0.0) = × 1.0`
逐位恒等（决策 7，先例 = R77 `ao_ss` 无条件乘 1.0）。

**逻辑自洽性**：`cov → 1` 时吃 GI（含 DDGI 的天光项）、ambient 归零；
`cov → 0` 时吃 env IBL（`irradiance_map` 天空，被 `env_params.y` 缩放）+
ambient 全额 = R88 原状。两条天空路径不同时全额在场 ⇒ 无双计、无真空。

### 5.4 CPU 镜像与看门

`ddgi_blend`（`ddgi.rs:874`）出口同步扩为 `(Vec3, f32, bool)` 或新增
`ddgi_coverage(position, positions, radii) -> f32` 纯函数；WGSL 侧
`DDGI_COVER_FADE` 常数进**源码层看门单测**（照
`wgsl_ddgi_source_matches_cpu_contract`）。可判别性单测：球心 `cov = 1`、
`d = r` 恰在球面 `cov = 0`、`d = 0.75r` 恰达 1（边界精确命中）、`d > r` 为 0、
`r = 0`（退化探针）不产生 NaN、多探针取 max 与顺序无关。

### 5.5 附带收益与残留

- 收益：`bounds` 配置错误的容忍度上升（覆盖不到处是羽化而非断界）；
  §8.57⑱ 从「诚实修法：把 bounds 抬到盖住所有表面」的**作者责任**变成引擎
  自动兜底。
- 残留（改写而非删除 ⑱）：覆盖度是**几何并集**意义上的，不判断可见性 ⇒
  墙外区域的 `cov` 可能仍为 1（靠深度项抑制，而深度项是概率上界）；羽化带
  宽度是常量 25%，不随探针密度自适应。

---

## 6 桶 B：两个既有 bug

### 6.1 B1 · SSR 强度旋钮从未生效（26 轮未发现）

**实锤链**：`SsrSettings::intensity` 定义 `post.rs:293`（文档「全局强度倍率；
1.0 = 物理权重」）→ 上传 `renderer.rs:7332` 区段 `flags[1] =
ssr_cfg.intensity.max(0.0)` → **`standard.wgsl` 全文对 `ssr.flags.y` 零读取点**
（只有 `:777` 读 `.z`/`.w`、`:1344` 读 `.x`）→ 与 `:775-779` 的实现
`fade = ssr_roughness_fade(...) * ssr_edge_fade(...)` 不符，模块文档里的公式
`w_ssr = roughness_fade × edge_fade × 强度 × [命中]` 少了「强度」。

**为什么零风险**：全仓每一处 `SsrSettings` 构造都写 `..Default::default()`
（`tests/ssr_render.rs:66`、`examples/ssr_snapshot.rs:56`、`:274` 显式关、
各游戏同）⇒ `intensity` 恒为默认 **1.0**，**从来没有一个场景、一个测试、一张
验收图设过它** ⇒ 乘进去是逐位恒等，`docs/ssr.png` 与 `R53_FXAA_OFF_BASELINE`
都不会动。

**修法**：`standard.wgsl:777` → `fade = clamp(roughness_fade × edge_fade ×
ssr.flags.y, 0.0, 1.0)`（决策 16）。配套：①新增 CPU 镜像
`pub fn ssr_blend_weight(roughness, uv, intensity, floor, cutoff, margin) -> f32`
（决策 17）+ 单测（含「非默认强度」的判别性断言：强度 0 ⇒ 与关闭逐位一致、
强度 2 ⇒ 钳到 1）；②`SsrSettings.intensity` 文档补「> 1 被钳到 1（本项是
命中权重，不是亮度倍率）」；③AGENTS.md 测试规约沉淀一条——
**每个公开旋钮至少要有一条非默认值断言**（分量镜像测试会漏掉整条公式的缺项）。

**诚实声明**：这是 R62b 的实现缺陷，不是 DDGI 引入的；R89 顺带闭合并在
CHANGELOG 的 Fixed 段单列。

### 6.2 B2 · `reset_taa` 漏清跨帧时域状态（frictions ⑬）

**实态核正**：定义在 `renderer.rs:4556-4564`（frictions ⑬ 引用的 3071 行号
**已漂移**）。实际只动两个字段：`taa_reset_pending = true`（`:4557`）、
`ssr_prev_valid = false`（`:4563`）。跨帧时域状态全清单（供台账）：
`occ_frame:641`、`prev_view_proj:646`、`taa_frame:650`、`taa_reset_pending:654`、
`taa_was_on:657`、`ssr_prev_valid:664`、`ssr_was_on:667`、`ssr_uniform_live:671`、
`ao_depth_frame:698`、`ssr.prev_color:970`/`prev_size:972`、`ddgi_upload:684`、
`meshlet_frame:748`。

**定稿**：
1. `reset_taa` 内补 `self.ao_depth_frame = None` ⇒ 切镜首帧 GTAO 即中性 1.0、
   第二帧起正确（frictions ⑬ 的建议修法）。断言 = reset 后首帧
   `FrameStats.ao_on == false` + 两帧定格残影像素断言（借 iris-exp 的定格法）。
2. `prev_view_proj` **不在设计期预设结论**：实现期核实「清空后运动模糊首帧」
   是否等价于「无历史」语义，等价才清、不等价则写进 §8 并给理由。
3. `ddgi_upload` **确认不该清**（§4.4 的 `reset_taa_does_not_disturb_ddgi` 钉住）。
4. iris-exp 侧撤除「连渲三帧」绕行（实验项目可动），并修
   `docs/private/iris-exp-frictions.md` ⑬ 的过时行号与状态标记。
5. 默认路径零影响：不调 `reset_taa` 的宿主逐位不变；调了但没有 GTAO 的宿主也
   逐位不变（`ao_depth_frame` 本就是 `None`）。

---

## 7 桶 C：编辑器 DDGI 入口

### 7.1 C0 · 前置修正：GI 几何收集排除叠加层（实锤缺口，必须先行）

**问题**：`collect_bake_triangles`（`bake.rs:830`，查询体 `:833-859`）只排除
蒙皮 / 不可见 / 非静态刚体，**完全不认 `EditorTransient`** ⇒ 编辑器叠加层
（NavGrid 红片、地面网格线、选中框、点光灯泡）会进 DDGI 的 BVH（挡光）并撑大
`default_bounds`。`tick_ddgi` 跑在**活动世界**上 ⇒ 一旦编辑器开 DDGI，
叠加层会自我污染 GI（尤其 NavGrid：blocked 红片正铺在探针所在的空间里）。
光照贴图今天没踩到，只因为编辑器烘焙走规范影子世界（`bake_and_install:1945`）。

**定稿**：新增 marker 组件 **`BakeExcluded`**（落 `iris_render::bake`，
`#[derive(Component)]` 单元结构体，与 `IrradianceProbe`/`EditorTransient` 同
形态），语义 = 「该实体的几何不进入 lightmap / 辐照探针 / DDGI **共用**的三角
收集」；**缺席语义 = 收录**（既有场景逐位不变，无该组件 ⇒ 与 R52a 一致）；
`collect_bake_triangles` 的查询元组加 `Option<&BakeExcluded>` 并在闭包首行
排除（查询元组现 7 项 = 上限 ⇒ 需把 `Option<&Collider>` 与
`Option<&RigidBody>` 合并处理或改用两阶段收集，**实现期核实**）。编辑器
`build_transient_assets:2449` 产物与 `build_nav_viz:2247` 等叠加层 spawn 时
统一挂该组件。

**注意**：`IrradianceProbe` 组件实体本身带网格吗？——它是 marker（`irradiance.rs:290`），
不入收集；无需处理。

### 7.2 C1 · 编辑命令层（直抄光源编辑模板）

```rust
pub enum DdgiEditMode { Inert, Grid, Budget, Debug }
pub fn ddgi_edit_mode(world: &World) -> DdgiEditMode;      // 照 light_edit_mode:1696
pub fn ddgi_spacing_step(world: &mut World, dir: i32) -> Option<f32>;
pub fn ddgi_bounds_translate(world: &mut World, axis: Axis, dir: i32) -> Option<(Vec3, Vec3)>;
pub fn ddgi_probes_per_frame_step(world: &mut World, dir: i32) -> Option<usize>;
pub fn ddgi_rays_face_step(world: &mut World) -> Option<u32>;   // 四档循环（DDGI_RAYS_PER_FACE_CHOICES）
pub fn ddgi_hysteresis_step(world: &mut World, dir: i32) -> Option<f32>;
pub fn ddgi_overlay_toggle(world: &mut World) -> bool;
```

函数体固定四步 `push_undo → 改 DdgiSettings 资源 → set_status → refresh_hud`；
「无 `EditorState` ⇒ no-op」契约必须守住（照 `commands_are_inert_without_
editor_state:1204`）。**`DdgiSettings` 缺席时**步进命令的处理定稿为：
第一条命令插入默认资源并 `set_status` 提示「DDGI 已启用（默认参数）」
——把「启用 DDGI」也纳入可撤销的一步，而不是要求宿主先手改代码。

**`WorldSnapshot` 必改**：加 `ddgi: Option<DdgiSettings>`（照 `directional:1413`），
`capture_snapshot:1423` 与 `restore_snapshot:1600` 同步。

**参数变化后的重建语义**：`spacing` / `bounds` 变 ⇒ `grid_fingerprint` 变 ⇒
现有 `DdgiGrid::tick` 已会重规划网格并全表标脏（`structure_generation` 递增 ⇒
全量重上传），**编辑器零额外接线**；这一步要在测试里钉住（「改 spacing ⇒
`structure_generation` 前进 + 表重填」）。

### 7.3 C2 · 可视化（transient 实体走主 pass，零新 pass / 零新 binding）

- 探针标记：每探针一个小球（复用 `MeshData` 的球生成，注意 R58b 的
  `uv_sphere` 极点退化三角坑），`EditorTransient` + **`BakeExcluded`** 双标记
  （叠加层自己不进 GI）。
- 状态分档着色：脏（待重照明）/ 粗填 / 精修已稳 ⇒ 让 `⌈n/k⌉` 收敛过程肉眼
  可读。数据源 = `DdgiGrid::dirty()` + `probe_stamp()`（只读，零成本）。
- 影响半径球壳线框（可选，默认关——256 个球壳的线是画面噪声）：只在
  「选中单个探针」时显示，避免与 NavGrid 叠加混成不可读。
- **维持「手动重建、不自动刷新」的既有立场**（`edit.rs:155` 诚实第 4 条）：
  叠加层在参数步进与 toggle 时重建，不做每帧跟随——理由与 Nav 同源（每帧
  despawn/spawn 数百实体的成本与确定性都不划算）。
- 上限：`MAX_IRRADIANCE_PROBES` = 256 球 ⇒ 编辑器实体数与 NavGrid 同量级，可接受。

### 7.4 C3 · 入档

`LevelSection` 加子段：

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub ddgi: Option<DdgiSection>,        // 缺席 = 不启用 DDGI（能力不启用）
```

`DdgiSection` 逐字段 `#[serde(default = "…")]`（对齐 `DdgiSettings` 的字段与
默认值：spacing / bounds / probes_per_frame / rays_per_face / hysteresis /
sky_color / bounces），`geometry_generation` **不入档**（运行期记账，不是作者
意图）。**`SCENE_FORMAT_VERSION` 保持 2、不 bump**（先例三处）。加载侧：
`apply_level` 把段翻译成 `DdgiSettings` 资源插入（缺席 ⇒ 不插 ⇒ DDGI 关）。
保存侧：`save_level` 从活动 `DdgiSettings` 资源回写段（照 `DirectionalLight`
的现有处理）。

**拒绝探针持久化**（§8.57⑭ 维持）：表内容（SH / 深度矩 / 脏位 / cursor）不入档
——重算成本已实测可控（粗填 ≈3.3 ms），且入档会引入「表与几何失配」的校验面。

**新增测试**：`ddgi_section_survives_editor_roundtrip`、
`level_without_ddgi_section_is_byte_identical`（旧档不动）、
`second_save_is_byte_identical` 既有测试须原值通过（证明「无 DDGI 段 ⇒ 不写段」）。

---

## 8 兼容硬约束（红线）

### 8.1 零改动面

1. `DdgiSettings` / `DdgiGrid` 缺席或空 ⇒ 全部既有 FNV 基线**原值通过**
   （R83 mode 0/1、lightmap、反射探针、SSR、AO、R88 三种缺席配置）。
2. WGSL mode 0 / mode 1 分支**语句逐条不动**；`cov` 在两支恒 0 且不进任何算术。
3. ambient 补光行（`standard.wgsl:1397-1400`）**结构不动**，只追加
   `* (1.0 − ddgi_cov)` 乘法（`ddgi_cov = 0.0` 时逐位恒等）。
4. 反射探针捕获绑定面不动（`renderer.rs:3500-3530` 的哑绑定原样）。
5. **不扩 `Globals`、不新增片元 storage binding**（4/4 已满，§8.57⑮）；
   `skybox.wgsl` 并行布局看门单测不动。
6. `BakeExcluded` 缺席 = 收录 ⇒ 任何不带该组件的既有场景，
   `collect_bake_triangles` 输出逐位不变（既有 lightmap / 探针基线的间接保证）。
7. 零 unsafe；无 rand、无全局可变状态；新 GPU 测试仍走 `tests/ddgi_render.rs`
   的 `GPU_SERIAL` 互斥（若开新文件必须自带互斥锁，AGENTS.md 铁律）。
8. wasm32 双 target 零警告（本轮无 compute、无 ray query，纯 CPU + 着色器算术）。

### 8.2 合法的像素变化（必须归因，不许静默）

以下三处**会**改变「DDGI 在场」场景的像素，属预期语义修订：

1. **A2 覆盖度混合**：仅影响 `cov < 1` 的外壳区域。
   证据 = 「`cov = 1` 区域与 R88 逐位相同」的分区断言 + DDGI-off 对照组新旧
   代码逐位一致。
2. **A3 ambient 衰减**：影响所有 `cov > 0` 的像素（幅度 = `ambient × cov`）。
   证据 = 同上两条 + `cov` 的解析单测。
3. **B1 SSR 强度**：全仓 `intensity` 恒 1.0 ⇒ **实际零像素变化**（§6.1 已证）。

因为 R88 的 `docs/ddgi.png` 与 `tests/ddgi_render.rs` **都没有绝对常量锁**
（前者运行时自洽 + 面板间 `assert_ne`，后者用三种缺席配置相等代替），
所以第 1、2 条**不走完整的「归因 + 重锁」程序**，但必须：①改动前先捕获全帧
哈希留对照；②重生成 `docs/ddgi.png` 并**目检扇贝带消失**；③确认三条
`assert_ne`（关/开、深度项开/关、昼/夜）在新图下仍成立；④在 CHANGELOG 显式
写「R89 修订了 DDGI 在场场景的像素（覆盖度羽化 + ambient 去双计）」，
不得含糊为「无基线影响」。

---

## 9 确定性论证

| 环节 | 论据 |
|---|---|
| `cov` 求值 | 与 `Σw` 同一遍固定序循环取 `max`（`f32::max` 全序、无迭代序依赖）；CPU 镜像同式同序 |
| 编辑器步进 | 全部是对 `DdgiSettings` 字段的确定性算术（乘除档表、加法平移），无 RNG、无迭代序 |
| 网格重规划 | 复用 R88 的 `plan_layout` / `stride_for_capacity`（整数二分，无浮点搜索）⇒ 参数变化 ⇒ 逐位确定的新网格 |
| 可视化 | 叠加层实体的 spawn 序 = 探针索引升序（固定）；不进入任何排序键；且被 `BakeExcluded` 挡在 GI 收集外 ⇒ **不反作用于模拟或 GI** |
| 序列化 | 段字段按固定结构体序写出；`bounds` 的六个 f32 走既有 `level.rs:1795` 逐位往返测试口径 |
| 时域 | 双会话逐帧逐位（R88 的 48 帧锚扩到含 `cov` 后的新值）；SSR×DDGI 与 reset 不动 DDGI 两条锚 |

**新增等价锚**：`DDGI_COVER_FADE → 0`（羽化带宽趋 0）⇒ `cov` 退化为 R88 的
`Σw > ε` 硬判据 ⇒ 表与像素逐位回到 R88（本轮改动面的极限自检）。

---

## 10 成本与风险

| 项 | 估计 |
|---|---|
| 着色器改动 | `ddgi_irradiance` 出口 +1 分量、mode 2 分支 2 行、ambient 行 1 乘、`ssr` `:777` 1 乘 ⇒ 单文件 < 30 行 |
| CPU 改动 | `ddgi.rs` 新纯函数 + 镜像；`bake.rs` 收集过滤 1 处；`post.rs` 新镜像函数；`renderer.rs` reset_taa 1 行 + 台账 |
| 编辑器改动 | 新命令段（~250 行）+ 可视化（~200 行，照 `build_nav_viz` 体量）+ `WorldSnapshot` 3 处 + 键位表 |
| 序列化 | `level.rs` 新子段（~120 行含校验）+ `scene.rs` 无改动（段在 `LevelSection` 内） |
| 运行时开销 | `cov` 每片元 ≈ 5 条指令（一次除法 + clamp + max），落在已有的双剪枝循环内；可视化仅编辑器模式；**游戏侧稳态成本仍为 0** |
| 主要风险 | ①`collect_bake_triangles` 查询元组撞 7 项上限（需两阶段改造，`bake.rs:831` 注释已预警）；②ambient 衰减幅度是主观观感量，可能需要在验收图上判读后再定 `cov` 是否只衰减一部分（备选 `× (1 − cov × DDGI_AMBIENT_DEDUPE)`，`DEDUPE` 默认 1.0）；③编辑器叠加层与 NavGrid 叠加同时开的可读性；④`prev_view_proj` 的清理语义需实测 |
| 工作量 | 中偏大（四桶）；若 ① 的两阶段改造牵连出烘焙重构，可把 C0 拆成前置小轮 |

---

## 11 不做清单（明确留下轮）

- 点光 / 聚光 / emissive 参与重照明（决策 19，建议与 §8.57⑪ 并行化合成
  「DDGI 性能与动态光源」专轮）
- `SkySource` 天空卷积（§8.57①：R79 大气与 DDGI 天空底色不同源，日落暖橙进不了
  间接光）
- 差值法 static + dynamic GI（裁定 2 / 决策 9）
- 探针实体化（`DdgiProbe` 组件、逐个编辑）
- 任何新片元阶段 storage 缓冲（4/4 满）——需要它就得先立项「申请 wgpu 特性
  上限 or 把某路改纹理」
- 着色器全表线性扫描的空间加速（§8.57⑯：探针八叉树 / 3×3×3 邻域查表）
- 深度表双线性插值与 relocation（§8.57⑥⑦⑧）
- 探针表持久化（§8.57⑭）
- 编辑器数值输入面板（`edit.rs:150-154` 既有拒绝，牵连 §8.33② 评审）

---

## 12 诚实限制修订

**从 §8.57 移入实现 / 改写**：
- ⑫（合成语义未修订）→ 移入实现，指向 §4.45 的五方裁决。
- ⑬（ambient 双计）→ 移入实现，改写为「`cov` 衰减已去双计；`cov` 不判可见性
  ⇒ 墙外区域 `cov` 仍可能为 1（靠深度项抑制）」。
- ⑱（扇贝带）→ 改写为 §5.5 的残留形态（羽化替代断界，带宽固定 25%）。
- ⑰（探针不落实体 ⇒「编辑器里看不见、也不能逐个调」）→ **只关闭前半截、
  改写不删除**：C2 的可视化让网格看得见（§7.3），但探针仍不落实体、不能
  逐个调整——后半截留给拒绝清单里的「探针实体化」与「排除格笔刷」（§11）。

**§8.58 新增条目（R89 的诚实边界，收官时落档）**——草案：
①`cov` 是几何并集量，不判断可见性；②羽化带宽固定 25%、不随探针密度自适应；
③ambient 衰减与 `cov` 共用一个量 ⇒ 无法单独调「去双计强度」；④图集实体在
DDGI 在场时完全不吃动态 GI（无局部混用，裁定 2 的代价）；⑤捕获 / RT 相机的
间接漫反射仍是零阶、无遮挡感知；⑥编辑器叠加层「手动重建不自动刷新」（沿用
既有立场）+ 与 Nav 叠加同开时可读性下降；⑦DDGI 参数入档但探针表不入档 ⇒
换机/换几何后首次启用要重走粗填 + 滚动收敛；⑧SSR 强度是**命中权重钳位**而
非亮度倍率（> 1 无效）——口径本轮定稿并写进文档；⑨`BakeExcluded` 是显式
声明式排除，不做几何类型自动判定（透明 / 粒子 / 贴花仍会进收集，与 R52a 同
立场）。

---

## 13 验收标准

### 13.1 CPU 单测（预计新增 20–26 项，全解析解 / 逐位）

`ddgi_coverage` 边界解析解（球心 1 / `d = r` 恰 0 / `d = 0.75r` 恰 1 / 球外 0 /
`r = 0` 无 NaN / 多探针 max 与序无关）· `cov → 1` 时 `mix` 恒等（浮点逐位）·
`DDGI_COVER_FADE → 0` ⇒ 退化回 R88 硬判据 · WGSL 源码层看门（新常数
`DDGI_COVER_FADE`）· `ssr_blend_weight` 镜像（含非默认强度：0 ⇒ 关、2 ⇒ 钳 1、
半程线性）· `BakeExcluded` 收集排除（同世界带/不带 ⇒ 三角集差异可判别）·
每条编辑器步进命令 apply→undo **逐字符复原**（照 `point_light_edits_undo_bit_exact:1551`
形态，含 `undo_depth` 断言）· `DdgiEditMode` 模式裁决分发 ·
「无 `EditorState` ⇒ no-op」契约 · `DdgiSection` 逐字段往返逐字节 + 旧档无段
逐字节不变 · 参数步进 ⇒ `structure_generation` 前进 + 全表重填 ·
`WorldSnapshot` 含 `DdgiSettings` 的复原 · 叠加层不入档（`transient_visuals_
never_reach_the_file` 同款）。

### 13.2 GPU 集成（预计新增 6–9 项，`GPU_SERIAL`）

1. 既有全部 FNV 基线零改动原值通过（含 R88 三种缺席配置）。
2. **`cov = 1` 分区逐位等于 R88**（本轮改动面不外溢的直接证据）。
3. DDGI-off 对照组：新旧代码逐位一致（A2/A3 的归因证据）。
4. `ddgi_does_not_touch_lightmapped_entities_bitwise`（A1）。
5. `probe_capture_uses_ddgi_l0_payload`（A4：DDGI 表变 ⇒ 捕获变；烘焙集变 ⇒ 捕获不变）。
6. `ssr_reflects_ddgi_gi_bitwise`（A5）。
7. `reset_taa_does_not_disturb_ddgi`（A5/B2）+ `reset_taa` 后首帧
   `FrameStats.ao_on == false` 与残影像素断言（B2）。
8. SSR 强度非默认值的像素可判别（B1，与 `intensity = 1.0` 帧差异 > 阈值）。
9. 双会话逐帧逐位（48 帧，R88 锚扩到新混合式）。

### 13.3 快照与目检

重生成 `docs/ddgi.png`（或新增 `docs/ddgi_v2.png`，二选一在实现期定：
建议**新增**一张 2×2 = 「R88 现状（硬断界）| R89 羽化 | ambient 去双计前后 |
编辑器叠加视图」，让修复效果可对比）+ **主会话目检扇贝带消失** +
三条 `assert_ne` 可判别性仍成立 + 跨运行逐字节一致。
编辑器侧新增 `docs/ddgi_editor.png`（叠加层可视化 + HUD 状态行）。

### 13.4 文档

`README.md` 特性条目 · `ARCHITECTURE.md` 新增 §4.45 + §8.58，§8.57⑫⑬⑱ 改写 ·
`CHANGELOG.md`（Added 桶 A/C、**Fixed 桶 B 单列两条**、像素修订声明）·
`AGENTS.md` 基线数字（现 debug 1527 / release 1524）+ 测试规约新增一条
「每个公开旋钮至少一条非默认值断言」（B1 的沉淀）·
`docs/private/iris-exp-frictions.md` ⑬ 标记闭合 + 行号订正 ·
本文件 §状态 改为「已落地，语义权威见 ARCHITECTURE §4.45 / §8.58」。

---

## 14 落点文件清单

| 文件 | 动作 |
|---|---|
| `crates/iris-render/src/ddgi.rs` | `ddgi_coverage` 新纯函数 + `ddgi_blend` 出口扩 `cov` + `DDGI_COVER_FADE` 常数 + 看门与单测 |
| `crates/iris-render/shaders/standard.wgsl` | `ddgi_irradiance` 出口扩分量、mode 2 分支 `mix`、ambient 行 `× (1 − cov)`、SSR `:777` 乘 `flags.y` 并钳 |
| `crates/iris-render/src/ssr.rs`（**不存在** ⇒ 落 `post.rs`） | `ssr_blend_weight` 新 CPU 镜像 + `intensity` 文档口径 |
| `crates/iris-render/src/post.rs` | `SsrSettings::intensity` 文档、`pub use ssr_blend_weight`、prelude（`lib.rs:247` 导出位） |
| `crates/iris-render/src/renderer.rs` | `reset_taa` 补 `ao_depth_frame = None`（+ `prev_view_proj` 视核实结果）、`probe_payload` 段注释加 A1 裁决 |
| `crates/iris-render/src/bake.rs` | `BakeExcluded` marker + `collect_bake_triangles` 过滤（注意查询元组 7 项上限） |
| `crates/iris-render/src/lib.rs` | prelude 导出 `BakeExcluded` / `DDGI_COVER_FADE` / `ssr_blend_weight` |
| `crates/iris-editor/src/edit.rs` | DDGI 编辑段（模式裁决 + 6 条步进命令）、`WorldSnapshot.ddgi` 字段与 capture/restore 同步、`build_ddgi_overlay` 可视化、`BakeReport` 的贴图×DDGI 提示 |
| `crates/iris-editor/src/shell.rs` | 键位表新增 DDGI 模式与步进键 |
| `crates/iris-engine/src/level.rs` | `LevelSection.ddgi` + `DdgiSection`（校验 + `apply` / 回写） |
| `crates/iris-render/tests/ddgi_render.rs` | 合成锚 + 覆盖度分区锚 + 双会话锚 |
| `crates/iris-editor/tests/edit_headless.rs` | 步进命令 undo 复原 + 段往返逐字节 + no-op 契约 |
| `crates/iris-render/examples/ddgi_snapshot.rs`（或新 `ddgi_v2_snapshot.rs`） | 重生成 / 新增验收图 |

---

## 附：需在 REPORT.md / CHANGELOG 声明的偏离

1. **`irradiance` 的 env 兜底从 `select` 改为 `mix`**（mode 2 分支）——理由：
   连续覆盖度混合是消灭 §8.57⑱ 断界的唯一形态；`cov = 1` 时逐位等于原
   `select` 结果（§5.3）。
2. **`globals.ambient` 补光项在 DDGI 在场时被 `cov` 衰减**——这是对 R52c/R63
   既有的「ambient 是独立美术补光预算，探针不接管」（`ARCHITECTURE.md:2640`）
   的**部分推翻**：裁定 3 让 `sky_color` 与 ambient 同源之后，"不接管" 就
   不再自洽（会双计）。新口径 = 「ambient 与 DDGI 天光项同源 ⇒ 按覆盖度
   分摊，不叠加」。理由与影响必须显式写进 CHANGELOG，不能并入 Added。
3. **`collect_bake_triangles` 新增组件依赖**（查询元组扩项）——若为此把收集改成
   两阶段重构，属实现级结构调整，需声明是否影响 R52a/R64 的既有逐位口径。
4. **SSR 权重公式补上强度项**——修复而非改义；但因它动的是**既有已发布特性**
   的像素路径（尽管全仓无人用过非默认强度），必须在 CHANGELOG 的 Fixed 段
   单列，并声明「既有 `docs/ssr.png` 与 SSR 基线逐位不变」的依据
   （`intensity` 恒 1.0）。
5. **`reset_taa` 语义从「清 TAA + SSR」扩到「清一切跨帧时域状态」**——同时
   声明 DDGI 是**故意不清**的那一类（探针域状态与镜头无关），避免被误读为
   漏项。
6. **编辑器 DDGI 入口不含数值输入面板、不含探针实体化**——尊重
   `edit.rs:150-154` 与 `edit.rs:169` 的既有拒绝；「放置入口」定稿为
   参数步进 + 可视化 + 入档三件套，与路线图字面「探针放置入口」的差别
   （不能逐个放/删探针）须显式声明，并把「排除格」笔刷列为 R89+ 候选。
