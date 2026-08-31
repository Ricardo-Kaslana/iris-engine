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
| **高性能** | 稀疏集 ECS（遍历内存连续、缓存友好）；SIMD 数学库 glam；GPU 侧自动实例化批处理；**视锥剔除**（包围球 × 归一化平面）；**并行系统调度**（声明组件读写集，无冲突系统多线程并发）；wgpu 流水线异步提交 |
| **低占用** | 无 GC、无反射、无运行时；组件销毁即时确定性归还内存；发布构建 `lto="thin"` + `codegen-units=1` |
| **现代化** | WGSL 着色器；ECS 架构 + 插件系统；**变换层级**；**实体挂载相机**；**glTF 2.0 资产管线**（.gltf/.glb + **节点动画 + 骨骼蒙皮**）；**PBR 材质**（Cook-Torrance 金属度/粗糙度工作流）；**IBL 环境光照**（程序化天空 / **外部 HDR 环境贴图** + 分割求和，Radiance .hdr equirectangular 手写解析）；**HDR 渲染 + ACES 色调映射 + 泛光**（Rgba16Float 中间缓冲）；**方向光阴影贴图**（2048² 深度图 + 3×3 PCF 软阴影）；**多光源**（1 方向光 + 8 点光物理衰减，首灯 cubemap 阴影）；derive 宏；4x MSAA；固定步长逻辑 |

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

# GLB 动画验收场景（先 gen_asset 生成资产），输出两个时刻的位图：
# 蓝色立方体自转 + 浮动（GLB 容器 + AnimationPlayer 播放链路）
cargo run --example gen_asset --release
cargo run --example anim_snapshot --release

# 骨骼蒙皮验收场景（同用 gen_asset 资产），输出两个时刻的位图：
# 两段臂绑髋/肘关节，肘关节挥舞 90°（GPU LBS 蒙皮 + 蒙皮阴影）
cargo run --example skin_snapshot --release

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
├── assets/              # 示例资产（cube.gltf + cube0.bin + sunset.hdr，gen_asset 生成）
├── crates/
│   ├── iris-macros/     # 过程宏：#[derive(Component / Resource / Event)]
│   ├── iris-core/       # ECS、调度、变换层级、时间、事件
│   ├── iris-render/     # wgpu 渲染器、glTF 管线、离屏渲染
│   ├── iris-platform/   # 窗口配置、键鼠输入
│   └── iris-engine/     # App 门面、插件系统、主循环 + 示例
│       └── examples/    # cube / orbit / model / instanced / offscreen / gen_asset / gltf_snapshot / shadow_snapshot / point_shadow_snapshot / bloom_snapshot / hdr_snapshot
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

## 测试

```sh
cargo test --workspace   # 132 个测试：单元 + GPU 端到端离屏渲染
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
**并行调度（原子计数观测真并发 / 读写冲突分批保序 / 独占系统屏障）**、
glTF 资产管线（内嵌 data-URI 与外部 .bin buffer、metallic/roughness 直通、双相机路径）、
Radiance .hdr 解析器（平面 / RLE 扫描线、RGBE 指数编解码、f16 位模式转换）。

## 跨平台矩阵

| 平台 | 图形后端 | 状态 |
|------|----------|------|
| Windows x64 / ARM64 | DirectX 12 / Vulkan | ✅ 已验证（本机 DX12） |
| Linux x64 / ARM64 | Vulkan | ✅ wgpu 官方支持 |
| macOS / iOS | Metal | ✅ wgpu 官方支持 |
| Web (wasm32) | WebGPU | 🚧 wgpu 支持，窗口层适配中 |

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
- [x] 多线程系统调度（声明式读写集 + 冲突分批并行）
- [ ] wasm32 Web 目标端到端验证
- [ ] 音频与 2D 叠加层（UI）
- [ ] 实例缓冲 GPU 持久化与增量更新

## License

MIT OR Apache-2.0
