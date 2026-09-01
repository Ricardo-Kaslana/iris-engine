# Iris Engine

一个用 Rust 编写的现代 3D 游戏引擎：**跨平台 · 内存安全 · 高性能 · 低占用 · 现代化**。

```rust
use iris_engine::prelude::*;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins::new("我的游戏"))
        .add_system(Stage::Startup, setup)
        .add_system(Stage::Update, gameplay)
        .run();
}

fn setup(world: &mut World) {
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world.resource_mut::<MaterialStore>().add(MaterialDef::solid(0.9, 0.4, 0.2, 1.0));
    world.spawn((Transform::from_xyz(0.0, 0.6, 0.0), cube, mat));
}

fn gameplay(world: &mut World) {
    let dt = world.resource::<Time>().delta_secs();
    world.query::<(&mut Transform, &Spin)>(|(t, s)| {
        t.rotation = Quat::from_axis_angle(Vec3::Y, s.0 * dt) * t.rotation;
    });
}
```

## 核心特性

| 特性 | 落实方式 |
|------|----------|
| **跨平台** | wgpu 统一封装 Vulkan / DirectX 12 / Metal（WebGPU 标准 API），winit 跨平台窗口；同一份代码编译 Windows / macOS / Linux，并可交叉编译至 ARM64 与 wasm32 |
| **内存安全** | Rust 所有权系统；实体「索引 + 代数」防悬垂引用；组件与资源运行期借用检查——错误访问立刻 panic 而非未定义行为；**引擎全部代码零 `unsafe`** |
| **高性能** | 稀疏集 ECS（遍历内存连续、缓存友好）；SIMD 数学库 glam；GPU 侧自动实例化批处理；**实例缓冲跨帧持久：静态场景零上传，动态实体仅传变化段**；**视锥剔除**（包围球 × 归一化平面）；**并行系统调度**（声明组件读写集，无冲突系统多线程并发）；wgpu 流水线异步提交 |
| **低占用** | 无 GC、无反射、无运行时；组件销毁即时确定性归还内存；发布构建 `lto="thin"` + `codegen-units=1` |
| **音频** | 纯软件混音器（多声音线性叠加 + 线性插值重采样 + 软限幅，混音数学全纯函数、单测采样级断言）+ cpal 设备层（实时回调 try_lock 不阻塞，无设备 / wasm32 优雅降级为纯混音器）+ WAV 加载（手写 RIFF 解析，16-bit PCM）+ 程序化音效（正弦 / 方波 / 噪声，确定性可复现） |
| **物理与碰撞（Sphere/AABB 刚体，v1）** | 轻量刚体物理：重力（`gravity_scale`）/ 线性阻尼 / restitution 反弹（组合取双方 max）/ 近似库仑摩擦；`Collider`（静态/动态）+ `RigidBody` 组件模型（组件缺席语义：只有 Collider = 静态、只有 RigidBody = 仅受力积分）；求解管线「半隐式欧拉积分 → O(n²) 检测 → 分离/反弹/摩擦响应 → 2 次迭代位置松弛」；检测与求解核心为**纯函数**（穿透/法线/反弹可脱离世界单测），`step_physics` 为 `&World` 并行系统（Update 内求解、PostUpdate 层级传播，天然正确编排），同输入两次模拟逐采样 bit 级一致；**静态推离查询**（`resolve_static_push` / `resolve_against_statics`，不写组件的纯查询）支撑运动学角色控制器——iris-game 只取水平分量即得「墙体阻挡 + 贴墙滑行」 |
| **场景序列化（JSON 双向）** | 关卡数据与游戏代码分离：手写 JSON 场景文件（环境段：方向光 + HDR 天空源；实体段：Name / Visibility / Transform / mesh / material / point_light / collider / rigidbody，缺省值 = 引擎默认）↔ 世界实体（`load_scene` / `save_scene`）；mesh 三态（图元 cube/plane/sphere/cylinder、glTF 整实例挂载为子实体、纯逻辑实体）；`SceneAssetRegistry` 登记表驱动资源来源往返（图元参数 / glTF 与贴图路径在 load 时登记、save 时还原），非场景来源句柄优雅降级 + 告警；`load(save(load(f)))` 语义等价且二次保存逐字节一致（21 项单测） |
| **现代化** | WGSL 着色器；ECS 架构 + 插件系统；**变换层级**（父子级联 + PostUpdate 阶段传播：Update 写变换当帧即全局生效）；**实体可见性开关**（`Visibility` 组件，全部渲染 pass 与 UI 统一过滤）；**实体命名查找**（`Name` + 按名定位）；**实体挂载相机**；**glTF 2.0 资产管线**（.gltf/.glb + **节点动画 + 骨骼蒙皮**）；**PBR 材质**（Cook-Torrance 金属度/粗糙度工作流）；**IBL 环境光照**（程序化天空 / **外部 HDR 环境贴图** + 分割求和，Radiance .hdr equirectangular 手写解析）；**HDR 渲染 + ACES 色调映射 + 泛光**（Rgba16Float 中间缓冲）；**方向光阴影贴图**（2048² 深度图 + 3×3 PCF 软阴影）；**多光源**（1 方向光 + 8 点光物理衰减，首灯 cubemap 阴影）；**2D UI 叠加层**（HUD：屏幕空间矩形 + 内嵌位图字体文本，单管线单纹理单 draw，色调映射后叠加）；derive 宏；4x MSAA；固定步长逻辑 |

## 快速开始

### 环境要求

- Rust 1.85+（`rustup update stable`）
- 支持 Vulkan / DX12 / Metal 的 GPU（软件回退亦可）

### 运行示例

```sh
# 旋转立方体：纹理 + 光照 + 飞行相机（WASD / Space·Shift / 滚轮 / Esc）
cargo run --example cube --release

# glTF 资产加载：先生成示例资产，再加载渲染（外部 .bin buffer + 材质）
cargo run --example gen_asset --release
cargo run --example model --release

# 行星系：变换层级（太阳→地球→月球级联旋转）+ 彩色点光
cargo run --example orbit --release

# 4096 个实例自动合批：8 种材质 → 8 个 draw call
cargo run --example instanced --release

# 无窗口离屏渲染（CI 友好），双点光照明，输出 iris_offscreen.bmp
cargo run --example offscreen --release

# glTF 资产离屏快照（验收用），输出 iris_gltf_preview.bmp
cargo run --example gltf_snapshot --release

# PBR / HDR / 阴影验收场景（离屏渲染），输出 iris_shadow_preview.bmp：
# 金属球粗糙度梯度 + 绝缘体球 + 地面软阴影 + ACES 色调映射
cargo run --example shadow_snapshot --release

# 点光 cubemap 阴影验收场景（离屏渲染），输出 iris_point_shadow.bmp：
# 暗色房间中央点光，环灯物体的阴影背离灯的方向径向拉伸（点光阴影特征）
cargo run --example point_shadow_snapshot --release

# 泛光验收场景（离屏渲染），输出 iris_bloom_preview.bmp：
# 三颗自发光彩球的 HDR 光晕（阈值提取 + 半分辨率高斯模糊）
cargo run --example bloom_snapshot --release

# 2D UI 叠加层（HUD）验收场景（离屏渲染），输出 iris_ui.bmp：
# 点光阴影 3D 场景 + 顶部位图标题 / 血条 / 右下文本（矩形 + 位图字体单批渲染）
cargo run --example ui_snapshot --release

# 音频子系统验收场景（离屏渲染混音波形），输出 iris_audio.bmp：
# 混音器编排 boom/blip/噪声三段程序化音效 → 240 根渐变波形条（481 矩形单批），
# 有真实音频设备时同步真声播放一遍
cargo run --example audio_snapshot --release

# GLB 动画验收场景（先 gen_asset 生成资产），输出两个时刻的位图：
# 蓝色立方体自转 + 浮动（GLB 容器 + AnimationPlayer 播放链路）
cargo run --example gen_asset --release
cargo run --example anim_snapshot --release

# 骨骼蒙皮验收场景（同用 gen_asset 资产），输出两个时刻的位图：
# 两段臂绑髋/肘关节，肘关节挥舞 90°（GPU LBS 蒙皮 + 蒙皮阴影）
cargo run --example skin_snapshot --release

# 收集小游戏双时刻验收图（左：开局 / 右：拾取 3 枚后），输出
# iris_game_start.bmp / iris_game_after.bmp（headless 同款驱动 + 离屏渲染）
cargo run --example game_snapshot --release

# 波次生存竞技场双时刻验收图（左：wave 2 混战 / 右：GAME OVER），输出
# docs/arena_preview.png 拼图源（headless 同款驱动 + 离屏渲染，蒙皮蠕虫
# 敌人两时刻扭动相位不同）
cargo run -p iris-engine --example arena_snapshot --release

# 物理与碰撞验收场景（离屏渲染 + headless 终态断言），输出 iris_physics_midair.bmp /
# iris_physics_rest.bmp：10 颗不同弹性的刚体球落到静态台面与地面——t=0.3s 半空 /
# t=3s 静止（球心 y>0、两两不穿透、总速度 < 2 m/s 三重断言）
cargo run -p iris-engine --example physics_snapshot --release

# 场景序列化验收（内容工作流示范）：load_scene 加载手写的 assets/arena.scene.json
# （地面 + 围墙竞技场 + 台面/圆柱 + 金属/漫反射静态球 + 悬空动态球 + 点光），
# 装 PhysicsPlugin 模拟 1.5s（动态球落到台面静置）后离屏渲染，输出 scene_snapshot.bmp
cargo run -p iris-engine --example scene_snapshot --release

# 外部 HDR 环境贴图验收场景（先 gen_asset 生成日落全景），输出 iris_hdr.bmp：
# Radiance .hdr equirectangular → IBL + 天空盒，铬球反射暖色天空与太阳
cargo run --example gen_asset --release
cargo run --example hdr_snapshot --release

# 并行调度基准：8 个互不冲突系统 串行 vs 并行加速比
cargo run -p iris-core --example bench_parallel --release

# ECS 吞吐基准
cargo run -p iris-core --example bench_ecs --release
```

### 添加为依赖

```sh
cargo add iris-engine
```

## 架构总览

```mermaid
flowchart TB
    subgraph 用户代码
        S[系统 fn(&mut World)]
        P[插件 Plugin]
    end
    subgraph iris-engine 门面
        APP[App / 主循环 / 插件系统]
    end
    subgraph iris-render 渲染层
        R[Renderer 批处理] --- SK[SurfaceKit 窗口呈现]
        R --- OFF[OffscreenRenderer 离屏]
        R --- SH[standard.wgsl]
    end
    subgraph iris-platform 平台层
        W[窗口配置] --- IN[Input 键鼠状态]
    end
    subgraph iris-core 核心
        ECS[World / 稀疏集 ECS] --- SCH[Schedule 阶段调度]
        ECS --- RES[资源 / 事件] --- TIME[Time 固定步长]
    end
    APP --> S & P
    APP --> R
    APP --> W & IN
    APP --> ECS
```

详细设计见 [ARCHITECTURE.md](ARCHITECTURE.md)。

## 项目结构

```
iris-engine/
├── assets/              # 示例资产（cube.gltf + cube0.bin + sunset.hdr + arena.scene.json，gen_asset 生成；collect_arena.scene.json / arena_battle.scene.json 为手写关卡，enemy_worm.glb 由 gen_asset 生成）
├── crates/
│   ├── iris-macros/     # 过程宏：#[derive(Component / Resource / Event)]
│   ├── iris-core/       # ECS、调度、变换层级、时间、事件、物理与碰撞
│   ├── iris-render/     # wgpu 渲染器、glTF 管线、离屏渲染
│   ├── iris-platform/   # 窗口配置、键鼠输入
│   ├── iris-audio/      # 软件混音器、WAV 加载、程序化音效、cpal 设备层
│   ├── iris-engine/     # App 门面、插件系统、主循环、场景序列化 + 示例
│   │   └── examples/    # cube / orbit / model / instanced / offscreen / gen_asset / gltf_snapshot / shadow_snapshot / point_shadow_snapshot / bloom_snapshot / hdr_snapshot / ui_snapshot / audio_snapshot / skin_snapshot / game_snapshot / arena_snapshot / physics_snapshot / scene_snapshot / web（wasm 专用入口，见「Web 构建」）
│   ├── iris-game/       # 引擎下游示范游戏 I：第三人称限时收集（见「用 Iris 做的游戏」）
│   └── iris-arena/      # 引擎下游示范游戏 II：波次生存竞技场（同上）
└── ARCHITECTURE.md      # 架构设计文档
```

## 实测数据

本机实测（Windows 11 · x86_64 · DirectX 12 · release 构建），复现命令见下。

| 指标 | 数值 |
|------|------|
| ECS：10 万实体生成（2 组件/实体） | **6.0 ms** |
| ECS：双组件遍历（10 万实体/帧，含可变写回） | **251.6 µs/帧** |
| ECS 吞吐 | **≈ 4.0 亿实体组件更新/秒** |
| GPU 合批：25 实例 + 地面（全部在视锥内） | **2 个 draw call**（同网格同材质自动合并） |
| 视锥剔除：相机背后 27 实体 | **0 次提交**（包围球 × 6 平面，测试断言） |
| 示例二进制（含 wgpu 全套渲染栈） | cube.exe **7.8 MB** / offscreen.exe 7.4 MB |
| 纯 ECS 工具体积上限 | bench_ecs.exe **0.17 MB** |
| 离屏渲染一帧 1280×720（含 GPU 初始化 + BMP 落盘） | < 1 s |

```sh
# 复现
cargo run -p iris-core --example bench_ecs --release
cargo run --example offscreen --release   # 产出 iris_offscreen.bmp
ls -l target/release/examples/*.exe
```

离屏渲染实测输出（`docs/offscreen_preview.png`，缩放预览）：方向光下的旋转立方体，
左右两盏彩色点光在地面形成蓝 / 红光晕并平滑衰减——多光源、深度、MSAA 均工作。

![离屏渲染输出](docs/offscreen_preview.png)

glTF 资产管线实测（`docs/gltf_preview.png`）：从 `assets/cube.gltf`（外部 .bin
buffer + POSITION/NORMAL/TEXCOORD + 45° 旋转节点）加载渲染，材质因子与点光正确。

![glTF 资产渲染](docs/gltf_preview.png)

方向光阴影实测（`docs/shadow_preview.png`）：立方体与球体向地面投射软阴影，
PCF 边缘自然、无 acne 条纹；`DirectionalLight.shadows = false` 可整帧关闭。

![方向光阴影](docs/shadow_preview.png)

点光 cubemap 阴影实测（`docs/point_shadow_preview.png`）：暗色房间中央点光
（512² × 6 面 R32Float，存归一化线性距离），环灯方块/球的阴影从根部
**背离点光方向径向拉伸**——放射状阴影是点光区别于方向光平行阴影的视觉特征；
`PointLight.shadows = true` 时查询顺序中的第一盏点光投影（v1 限定）。

![点光 cubemap 阴影](docs/point_shadow_preview.png)

PBR + HDR + IBL 实测（`docs/pbr_preview.png`）：Cook-Torrance 金属度/粗糙度工作流——
金 / 铜 / 银 / 钢四种金属球（粗糙度 0.05 → 0.55 递增）+ 绝缘体红球，方向光软阴影，
程序化天空背景（inv_view_proj 射线采样），IBL 环境光照（天空立方体贴图：
漫反射辐照度卷积 + GGX 预滤波 + BRDF LUT 分割求和），金属球上半球反射蓝天、
下半球反射地面；线性 Rgba16Float HDR 渲染经 ACES 色调映射输出 sRGB。

![PBR + HDR 渲染](docs/pbr_preview.png)

泛光实测（`docs/bloom_preview.png`）：HDR 亮度提取（阈值 1.0，软膝）→ 半分辨率
高斯模糊（三轮分离迭代）→ ACES 前按强度叠加。发光体溢出柔和彩色光晕，
暗部与地面保持干净；`BloomSettings` 资源可调强度 / 阈值或整帧关闭。

![泛光后处理](docs/bloom_preview.png)

glTF 动画实测（`docs/anim_preview.png`，左 t=0 / 右 t=1）：`anim_cube.glb`
（GLB 容器）中的蓝色立方体按嵌入的自转 + 浮动剪辑运动——
`AnimationPlayer` 组件驱动节点 `Transform`，层级传播当帧更新世界变换。

![glTF 动画](docs/anim_preview.png)

骨骼蒙皮实测（`docs/skin_preview.png`，左 t=0 / 右 t=1.5）：两段臂分别绑定
髋 / 肘关节，肘关节旋转 90° 时上段**绕肘点**扫成水平——关节矩阵
`J = inv(蒙皮全局) × global(joint) × ibm` 每帧算出后经 storage buffer
进顶点着色器做 4 关节线性混合（LBS）；蒙皮阴影管线复用同一份矩阵，
阴影随骨骼同步变形。

![骨骼蒙皮](docs/skin_preview.png)

外部 HDR 环境贴图实测（`docs/hdr_preview.png`）：程序化生成的日落全景
`assets/sunset.hdr`（1024×512 equirectangular Radiance 文件，手写 RGBE 解析器
加载）经 `SkySource` 资源挂入世界，渲染器按 generation 失效自动重烘 IBL——
暖色天空与亮度 40 的太阳盘成为天空盒背景与泛光源，金属铬球反射出天顶蓝、
暖色地平线与太阳高光，地面承接 HDR 环境的漫反射辐射。

![外部 HDR 环境贴图](docs/hdr_preview.png)

2D UI 叠加层实测（`docs/ui_preview.png`）：点光阴影 3D 场景之上叠加 HUD——
顶部 `IRIS ENGINE` 位图标题、左上血条（深底 + 红色前景两个矩形实体）、
右下青色占位文本。UI pass 位于色调映射 blit 之后：矩形与文本合批为
**一个 draw call**、直接写 sRGB 目标——文字与色块干净锐利，不受泛光 /
ACES 影响；屏幕像素坐标系（原点左上、y 向下），字体为内嵌公有领域
font8x8（ASCII 96 字形，等宽 8×scale 步进，逐亮位展开、形状由单测守护）。

![2D UI 叠加层](docs/ui_preview.png)

音频子系统实测（`docs/audio_preview.png`）：`AudioMixer` 离线编排三段程序化
音效——55Hz 低频 boom（t=0，起始强峰）、880Hz 短 blip（t=0.4，与 boom 尾部
线性叠加出更高峰）、噪声 burst（t=0.7，后段尖峰）——混音到 2 秒 48kHz 立体声
缓冲，逐 400 帧取峰值归一化，映射为 240 根青→橙渐变垂直条（暗色双层底条 +
波形条，复用 UI 矩形系统：481 个 UiRect 实体合批为**一个 draw call**）。
音量编排使全程峰值 0.95 不触软限幅——包络三段清晰、无爆音削顶的平顶；
混音数学（音量 / 叠加 / 循环 / 重采样 / 限幅）全部为确定性纯函数单测覆盖，
有真实音频设备时示例同步真声播放同一编排（无设备自动降级为纯混音器）。

![音频子系统](docs/audio_preview.png)

物理与碰撞实测（`docs/physics_preview.png`，左 t=0.3s / 右 t=3s，同一世界 headless
驱动到两个时刻各自离屏渲染）：10 颗彩色刚体球（restitution 0.2..0.9、半径
0.35..0.55）从 3.5~6.5 m 落向静态高台与地面——左图球群半空中（点光软阴影随球
移动），右图球群落定在台面顶与地面上、无穿插；同一运行内嵌 headless 终态断言
（全部球心 y > 0、球-球与球-静态体残余穿透 ≤ 2e-3、总速度 0.998 < 2.0 m/s 近似
静止）。求解器核心为纯函数（检测 = 穿透/法线、求解 = 分离/反弹/摩擦），17 项
单测覆盖球-球 / 球-AABB / AABB-AABB 几何、自由落体解析解、e² 回弹高度、静止
不抖、摩擦与阻尼衰减、静态体不动、逐采样确定性等。

![物理与碰撞](docs/physics_preview.png)

场景序列化实测（`docs/scene_preview.png`）：手写 JSON 关卡文件
`assets/arena.scene.json`（环境段方向光 + 12 个实体：地面、四围墙、中央台面、
台上圆柱、金属 / 漫反射两颗静态球、悬空动态球、点光、隐藏的逻辑标记实体）经
`load_scene` 整体载入世界——示例代码零场景搭建；装上 `PhysicsPlugin` 确定性
模拟 1.5 s 后离屏渲染：所有物体在位、材质区分清晰（金属球带高光反射、漫反射
球哑光）、方向光 + 点光阴影正常；动态球精确落到台面静息高度（球心 y = 1.500、
速度 0.033 m/s，headless 断言）。mesh 来源（图元参数）由 `SceneAssetRegistry`
登记表在 load 时登记、save 时还原，`load(save(load(f)))` 与 `load(f)` 实体
组件语义等价且二次保存逐字节一致（往返不变量单测守护）。

![场景序列化](docs/scene_preview.png)

## 用 Iris 做的游戏

引擎的「真实用户」验证——只用公开 API（`iris-engine` 门面 + glam）拼完整游戏，
**引擎侧零改动**。两作递进：Iris Collect 验证「能不能拼出来」，Iris Arena
压榨「蒙皮动画 × 对象池 × 弹道 × 多游戏状态」的高强度真实用法。

### I. Iris Collect（`crates/iris-game`）——第三人称限时收集

**Dogfooding I**——10 枚金币、60 秒倒计时、围墙竞技场、跳跃拾取、HUD、
程序化音效。
纯逻辑状态机（`logic.rs`，只依赖 glam、零引擎依赖）与插件接线（`GamePlugin`）
分离：**关卡来自 JSON 场景文件**——Startup 经 `load_scene` 载入
`assets/collect_arena.scene.json`（微弱日光环境段 + 地面、四面围墙（静态
Collider，内缘 ±17.5）、三根障碍柱（压在金币环路线上制造绕行）、中央点光），
玩家 / 金币 / HUD 为代码生成，相机作为玩家子实体（层级跟随验证
`ActiveCamera`）；Update 把 `Input` + `Time` 翻译进逻辑（移动积分 / 拾取 /
胜负全部可单测的纯函数）后，用物理库的**静态推离查询**实现运动学角色控制
器——玩家球体对场景静态体求推离、只回馈水平分量：墙体把玩家挡在 17.05
（= 内缘 17.5 − 半径 0.45，远早于 ±18 的逻辑钳制安全网），推离只沿法线 →
切向速度天然保留 = **贴墙滑行**；随后把结果写回实体 / HUD / `Audio`——
金币自发光供泛光、点光 cubemap 阴影、位图字体 HUD 全部来自引擎既有能力。
同一套插件系统链经 `GamePlugin::build_schedule` 在无窗口无 GPU 的 headless
测试与离屏快照中复用（测试经 `GamePlugin::new` 注入临时目录的 fixture 关卡，
全部测试跑在真实场景加载路径上）。这次实践撞到的 7 条 API 缺口（可见性开关 /
阶段排序 / 并行发事件 / 按名查实体 / Input 注入 / Audio headless /
Time·UI·图元小项）已在后续「引擎 API 打磨」轮全部修复，游戏内的绕行代码
同步替换为新 API；角色控制器也已由物理查询实现（清单见 ARCHITECTURE.md §8.12）。

![收集小游戏双时刻验收](docs/game_preview.png)

实测（`docs/game_preview.png`，左：开局 / 右：拾取 3 枚后，同一世界用 headless
方式驱动到两个时刻各自离屏渲染）：左图玩家居中、围墙与障碍柱在位、金币环
带泛光、SCORE 0/10；右图 SCORE 3/10、TIME 开始倒数、金币减少、玩家与子实体
相机已随输入移动——场景文件关卡、层级跟随、点光阴影、泛光、HUD 在真实游戏
负载下完整工作，玩家与墙体 / 障碍柱无穿插。

```sh
cargo run -p iris-game --release   # 玩游戏（WASD 移动 / Space 跳跃）
cargo run --example game_snapshot --release   # 双时刻离屏验收图
```

### II. Iris Arena（`crates/iris-arena`）——波次生存竞技场

**Dogfooding II**——俯视 3/4 视角竞技场：WASD 移动（屏幕相对 + 贴墙滑行）、
**Shift 冲刺**（0.15 s 高速 + 0.25 s 无敌帧）、**自动开火**（每 0.25 s 朝
14 m 内最近敌人发射弹道，无敌人不开火）。蒙皮蠕虫敌人（`enemy_worm.glb`，
三段箱体关节链扭动循环）从场边刷新成波次（第 n 波 3 + 2n 只、追击速度逐波
+0.15）；接触掉 1 血 + 击退 + 连击清零；弹道命中 -1 HP + 小击退。清空一波
→ 3 s 间歇 → 下一波；玩家 5 HP 归零 GAME OVER，R 直接重开。连击：击杀 +1、
受伤或 3 s 无击杀清零，得分 = 25 × 连击数。

相比 Iris Collect 高强度压榨的引擎面：**蒙皮动画 × 对象池**——启动时一次
spawn 15 个蒙皮蠕虫实例（`spawn_gltf_instance` + `AnimationPlayer` 自动
循环扭动）与 40 个自发光弹道小球，全部 `Visibility::HIDDEN` 待命，波次间
按逻辑侧稳定 id 取用 / 归还槽位，**零运行期 spawn / despawn**（池不超发由
单测守护）；**根实体移动 × 蒙皮子树**——插件只写 glTF 根的
Transform，关节子树随层级传播跟随、动画逐帧覆写关节旋转，两者在同一实体
树上叠加；纯逻辑核心（`logic.rs`，只 glam、零引擎依赖、17 项单测含逐采样
确定性）内置本地 XZ 静态推离（与引擎 `resolve_static_push` 水平投影语义
近似、刻意解耦），插件层在 Startup 把场景 `Collider` 一次性转换为逻辑形状
（Update 每帧零查询）；多游戏状态 Title / Playing / GameOver + 波次间歇；
音效由逻辑侧单调事件计数器的前后差分触发（射击 / 命中 / 受伤 / 开波双音，
全部程序化生成）。headless 集成测试（8 项）复用与窗口程序同一编排的插件
系统链，覆盖池取用 / 归还 / 跨波复用、动画推进、HUD 刷新、弹道池封顶、
终局重开闭环与混战 draw calls 记录性断言（13 敌 + 20 弹 + 场景 + HUD 一帧
**25 次 draw call**）；本次实践撞到的 API 摩擦（AnimationPlugin 未入
prelude / 资产路径工作目录相对 / 蒙皮实例关节树不随根移动等）见
ARCHITECTURE.md §8.13。

![波次生存竞技场双时刻验收](docs/arena_preview.png)

实测（`docs/arena_preview.png`，左：wave 2 混战 / 右：GAME OVER，同一世界
用 headless 方式驱动到两个时刻各自离屏渲染）：左图 5 只蒙皮蠕虫从场边
围拢玩家、自动开火弹道在飞、HUD（WAVE 2 / SCORE / HP 条）完整；右图蠕虫
围拢倒地的玩家、HP 条清空、GAME OVER 横幅——两时刻相隔真实推进的帧，
蒙皮扭动相位肉眼可见地不同（动画在跑）。玩家与障碍柱 / 墙体无穿插。

```sh
cargo run -p iris-arena --release   # 玩游戏（WASD 移动 / Shift 冲刺 / 自动开火）
cargo run -p iris-engine --example arena_snapshot --release   # 双时刻离屏验收图
```

## 测试

```sh
cargo test --workspace   # 292 个测试：单元 + GPU 端到端离屏渲染
```

集成测试在真实 GPU 上离屏渲染并校验像素（无 GPU 环境自动跳过），覆盖：
着色器编译、深度测试、MSAA、实例化合批、视锥剔除、多光源照明、
**方向光阴影（开/关整帧差分：成片变暗、无变亮、大部分不变）**、
**点光 cubemap 阴影（面矩阵全方向覆盖 + 偏置遮挡物阴影落在解析预测位置 +
开/关整帧差分 + 空跑不污染）**、
**泛光（开/关整帧差分：光晕变亮、无变暗、远场不变）**、
**IBL（铬球反射蓝天：蓝主导像素对比 + 整帧变亮）**、
**glTF 动画（旋转方块离屏渲染：正对 → 侧对红色像素锐减）**、
**骨骼蒙皮（肘关节动画驱动顶点位移：最左像素左移 ≥30px + 蒙皮阴影成片变暗）**、
**外部 HDR 环境贴图（红/蓝合成全景：画面上下半球主导通道 + 铬球反射上半球）**、
**2D UI 叠加层（矩形像素精确落地无溢出 + 文本亮像素命中逐位解析预测 ±25% +
HUD 区域不透 3D 且 HUD 外区域与无 UI 渲染逐像素一致）**、
**并行调度（原子计数观测真并发 / 读写冲突分批保序 / 独占系统屏障）**、
**实例缓冲增量上传（静态场景连续帧零上传且像素一致 + 单实体移动仅传变化段）**、
glTF 资产管线（内嵌 data-URI 与外部 .bin buffer、metallic/roughness 直通、双相机路径）、
Radiance .hdr 解析器（平面 / RLE 扫描线、RGBE 指数编解码、f16 位模式转换）、
**音频混音核心（音量缩放与即时生效、多声音线性叠加可分解、播完自动移除、
循环回绕、stop 静音与 SoundId 世代防悬垂、通道上/下混、重采样过零点保持、
软限幅边界）**、WAV 解析器（16-bit PCM 值映射、帧交错保留、奇数长度未知块
跳过、坏魔数 / 非 16-bit / 非 PCM / 声道数报错）与程序化音效（正弦 / 方波
对照解析解逐采样 1e-5 容差、噪声固定 seed 可复现、首尾防爆音包络）、
**headless 游戏集成（iris-game：不建窗口不建 GPU，`GamePlugin` 完整系统链 +
`Time::advance` 确定性推进 + `Input::press / release` 脚本化按键注入 +
临时 fixture 场景文件注入驱动真实游戏世界——静止 / 移动积分与朝向 / 单次
跳跃弧线 / 相机子实体层级跟随 / 脚本路线拾取与计分 / 金币可见性隐藏 /
HUD 文本刷新 / 全收集胜利与终局冻结 / 超时落败 /
**墙体 Collider 阻挡（玩家稳定贴墙 17.05，小于钳制上限 18——是物理而非
clamp 在挡）/ 贴墙滑行（法向被挡、切向持续滑行）**）、
**headless 竞技场集成（iris-arena：同款驱动 + 双池预热断言 + 蒙皮动画推进 +
任意键开局取池 + 波次间池槽位复用（第 2 波复用第 1 波全部 5 槽）+ HUD
四要素刷新 + 弹道池封顶与槽位回收 + GAME OVER → R 重开闭环（旧 id 全释放、
事件计数复位）+ 混战 draw calls < 300 记录性断言（实测 25））**、
**物理与碰撞（球-球 / 球-AABB（顶面 / 侧向 / 角点 / 盒内退出）/ AABB-AABB
接触几何与反弹 / 摩擦均值的纯函数断言；自由落体对照 y(t)=h-½gt² 的 60Hz 离散
容差、gravity_scale 缩放、e=0.8 回弹顶点 ≈ 初高×0.64 且逐次衰减、e=0 落地静止
穿透 <1e-3 不抖、摩擦切向衰减对比、线性阻尼对照 v₀e^(-λt)、静态体 bit 级不动、
双世界逐采样 bit 级确定性、7 球堆叠两两不穿透、Time 缺席零步长；
静态推离查询：单墙推离量精确且只沿法线（切向保留）/ 正交双墙顺序累加 /
空静态表恒等 / 世界封装跳过动态体并遵循静态缺席语义）**、
**场景序列化（最小场景与字段缺省值 = 引擎默认、未知字段容错、坏 JSON /
缺 entities / 缺文件 → 错误枚举各归其位；实体段逐字段进组件（Name / Visibility /
Transform 四元数 / 材质数值 / 点光 / 碰撞体 / 刚体）、静态体缺席语义、
glTF 引用挂载子实体树 + GltfMounted 标记、GLB 容器引用、方向光环境资源
（缺席不改）、sky_source 合成 HDR → EquirectHdr + 路径登记、材质贴图文件
载入 + 登记；往返：丰富场景（全组件 + glTF + 贴图）save → load → save
二次保存逐字节一致且逐实体语义等价、glTF 内部实体不泄漏进场景文件、
非场景来源 mesh / 材质优雅降级告警、默认材质段省略）**。

## 跨平台矩阵

| 平台 | 图形后端 | 状态 |
|------|----------|------|
| Windows x64 / ARM64 | DirectX 12 / Vulkan | ✅ 已验证（本机 DX12） |
| Linux x64 / ARM64 | Vulkan | ✅ wgpu 官方支持 |
| macOS / iOS | Metal | ✅ wgpu 官方支持 |
| Web (wasm32) | WebGPU | ✅ **端到端验证通过**：全 workspace 编译零警告 + 真实浏览器渲染确认（点光场景 + HUD 完整成像，见 `docs/web_preview.png` 与下方「Web 构建」） |

### Web 构建（wasm32-unknown-unknown）

需要 WebGPU 浏览器（Chrome / Edge 113+）；wgpu 30 在 wasm 下只编译 WebGPU 后端。

```sh
rustup target add wasm32-unknown-unknown
# wasm-bindgen-cli 版本必须与 Cargo.lock 中 wasm-bindgen 一致（先 grep 查询）
cargo install wasm-bindgen-cli --version 0.2.127 --locked
cargo build --example web --release --target wasm32-unknown-unknown
wasm-bindgen --out-dir web-demo --target web     target/wasm32-unknown-unknown/release/examples/web.wasm
# web-demo/index.html 已随构建脚本就位；起静态服务器访问：
npx serve web-demo    # 或 python -m http.server -d web-demo
```

平台要点（详见 ARCHITECTURE.md §6）：wasm 上 `Instant` 由 `performance.now()` 封装
（std 版本运行时 panic）；无 std 线程，并行系统自动内联顺序执行；GPU 适配器请求
是 JS Promise，引擎在事件循环内逐帧异步驱动（同步阻塞会卡死页面）；`env_logger`
编译通过但无输出（v1 未接 console log）；canvas 由 winit 自动附加到 `<body>`；
音频设备层不可用（cpal 不支持 wasm32，`AudioOutput::device()` 恒 `None`），
混音核心照常编译、`Audio` 资源自动降级为纯混音器。

## 路线图

- [x] 变换层级（父子级联）与全局变换传播
- [x] 多光源（点光数组）与视锥剔除
- [x] glTF 2.0 资产管线（网格 / 材质 / 纹理 / 节点层级）
- [x] 实体挂载相机（层级 × 相机）
- [x] 方向光阴影贴图（正交深度 pass + PCF）
- [x] PBR 材质模型（Cook-Torrance）与 HDR / ACES 色调映射
- [x] 泛光（Bloom）后处理
- [x] IBL 环境光照（程序化天空 + 分割求和）
- [x] 天空盒背景 pass（射线采样，与 IBL 同源）
- [x] GLB 二进制容器与 glTF 节点动画（AnimationPlayer）
- [x] 骨骼蒙皮动画（GPU LBS + 蒙皮阴影）
- [x] 外部 HDR 资产加载（equirectangular）
- [x] 点光阴影（cubemap，首灯 6 面归一化距离）
- [x] 2D UI 叠加层（HUD：屏幕空间矩形 + 位图字体文本，单批单 draw）
- [x] 多线程系统调度（声明式读写集 + 冲突分批并行）
- [x] wasm32 Web 目标端到端验证（全 workspace 编译 + Instant/线程/异步 GPU 初始化平台封装 + 真实浏览器渲染确认）
- [x] 音频子系统（纯软件混音器 + cpal 设备层 + WAV 加载 + 程序化音效生成，wasm32 降级为纯混音器）
- [x] 实例缓冲 GPU 持久化与增量更新（帧级比对：静态场景零上传，动态实体仅传变化段）
- [x] 物理与碰撞 v1（Sphere/AABB 刚体：积分 / 检测 / 响应 / 迭代松弛，纯函数确定性求解器）
- [x] 场景序列化（JSON 双向：环境段 + 实体段 ↔ 世界实体，登记表驱动的资源来源往返）

### Web 端到端实测

`docs/web_preview.png`：真实浏览器（WebGPU）中加载 wasm 版引擎的截图——
位图字体标题 "IRIS WEB"、血条 HUD、四个彩色方块 + 中央点光径向照明 +
天空盒背景完整成像。整条链路（wasm 模块实例化 → winit canvas →
WebGPU 适配器/设备 → 全渲染栈 → HUD 叠加）在浏览器主线程上逐帧运行。

## License

MIT OR Apache-2.0
