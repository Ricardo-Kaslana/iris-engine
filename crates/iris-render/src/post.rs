//! 后处理：设置资源 + 全屏 pass 串联框架 v1。
//!
//! 后处理链（编译期有序，详见 ARCHITECTURE §4.11 / §4.29）：
//!
//! ```text
//! 主 HDR 解析缓冲（几何 + 天空 + 透明 + 粒子）
//!   → TAA（可选，R60，链首，全分辨率）：投影抖动色 + 深度副本 + 历史
//!     → 3×3 邻域钳制混合 → 新 HDR（写入本帧历史槽，下游链直接读它）
//!   → 景深（DOF，可选，全分辨率）：按逐像素线性视距算 CoC → 12-tap
//!     泊松盘 HDR 平均；需要主相机深度副本（scene_depth pass，同样可选）
//!   → 运动模糊（MB，可选，全分辨率）：深度重建世界位 → 上帧相机重投影
//!     得逐像素屏幕速度 → 沿速度方向 N-tap 采样（v1 只覆盖相机运动）
//!   → 泛光（半分辨率：亮度提取 → 3×(水平/垂直) 模糊，ping-pong A/B）
//!   → 暗角（全分辨率 HDR → HDR，强度 0 时整段跳过）
//!   → blit（泛光叠加 + 曝光 + ACES 色调映射——链的固定终点）→ sRGB
//!   → FXAA（可选，R53）：LDR sRGB 域边缘平滑；开启时 blit 改写中间 LDR
//!     纹理、FXAA 再写最终目标，缺席/关闭时 blit 直写目标、链逐位不变
//! ```
//!
//! TAA（R60，详见 ARCHITECTURE §4.29）把 R53 时「三件套与单帧链架构
//! 冲突」解开：状态严格收敛在链首单一 pass（双历史纹理 ping-pong +
//! 帧计数器 + 上帧矩阵快照），链的其余部分保持无信号单帧 pass。
//! `TaaSettings` **缺席 = 关**：不抖动任何矩阵、不分配历史纹理、链
//! 命令流与 R59 逐位一致（零开销默认路径）。
//!
//! DOF / MB 排在泛光**之前**：焦外亮源与运动拖影都应在 HDR 线性域参与
//! 亮度提取（泛光），先在物理域混合、后做镜头合成，与暗角先于曝光同理。
//! 两者默认关闭（资源缺席 = 关），关闭时整段跳过、链与 R42 基线逐位
//! 一致（零开销默认路径，`tests/post_render.rs` FNV 锁定）。
//!
//! 框架的形态边界（防过度设计）：**编译期有序链 + 单一「描述 → 编码」
//! 函数**（[`encode_post_pass`]），不做运行时注册、不做 trait 对象注册表、
//! 不做拓扑排序。新增一个后处理 pass 的步骤（暗角即模板）：
//!
//! 1. 写一个 WGSL（全屏三角形顶点 + 片段入口，组 0 = uniform + 源纹理 +
//!    采样器，见 `shaders/vignette.wgsl`）；
//! 2. 在 `pipeline.rs` 用 `make_post_pipeline` 建一条管线（目标 = HDR
//!    格式，与链上其他中间 pass 同构）；
//! 3. 在 `renderer.rs` 的 `draw` 后处理段，按链序追加一个
//!    [`PostPassDesc`] 并调用 [`encode_post_pass`]；
//! 4. 若需要新的中间纹理分辨率等级，在 [`PostScale`] 加一个变体——
//!    ping-pong 分配器自动管理其纹理对。
//!
//! 需要额外纹理输入的 pass（DOF / MB 的深度副本）在 [`PostPassDesc`] 的
//! `extra_texture` 槽位携带（组 0 binding 3，布局用 `post_depth_layout`）；
//! 链位与 ping-pong 槽位分配由纯函数 [`full_chain_slots`] 单点定义。
//!
//! 中间纹理由 [`PostScratch`] 按分辨率等级分配「纹理对」并跨帧复用；
//! 泛光的槽位交替（读 A 写 B / 读 B 写 A）由纯函数 [`bloom_blur_slots`]
//! 单点定义。

// ---------- 设置资源（World 资源，渲染器 try_resource 直读） ----------

/// 泛光（Bloom）设置。作为 `World` 资源存在，未注册时使用默认值（启用）。
///
/// 泛光在 HDR 线性空间工作：亮度超过 `threshold` 的像素经半分辨率
/// 高斯模糊后，在 ACES 色调映射之前按 `strength` 叠加回画面。
#[derive(Clone, Copy, Debug, iris_core::Resource)]
pub struct BloomSettings {
    /// 是否启用泛光。
    pub enabled: bool,
    /// 叠加强度（0 = 不可见）。
    pub strength: f32,
    /// 亮度提取阈值（HDR 线性亮度；1.0 = 只有超过漫反射全白的像素参与泛光）。
    pub threshold: f32,
}

impl Default for BloomSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            strength: 0.15,
            threshold: 1.0,
        }
    }
}

impl BloomSettings {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }
}

/// 曝光控制。传感器增益：线性 HDR 颜色在 ACES 色调映射**之前**乘以
/// `exposure`（`blit.wgsl` 的 `hdr * blit_params.y`）。
///
/// **资源缺席语义**：未注册时渲染器按 1.0（中性）处理——缺席即画面
/// 与无曝光控制逐位一致；`DefaultPlugins` 因此不强制注册本资源，需要
/// 时手动 `world.insert_resource(ExposureSettings { exposure: 2.0 })`。
/// 负值按 0 处理（渲染器钳制，不翻转画面）；0.0 = 全黑（ACES(0) = 0）。
#[derive(Clone, Copy, Debug, iris_core::Resource)]
pub struct ExposureSettings {
    /// 曝光倍率（1.0 = 中性；< 1 欠曝压暗、> 1 过曝提亮，单调性由
    /// ACES 在正半轴严格单调保证）。
    pub exposure: f32,
}

impl Default for ExposureSettings {
    fn default() -> Self {
        Self { exposure: 1.0 }
    }
}

/// 暗角（vignette）设置。镜头物理遮挡的近似：画面四角按到中心的距离
/// `smoothstep` 压暗（全分辨率线性 HDR → HDR pass，见
/// `shaders/vignette.wgsl`）。
///
/// **资源缺席语义**：未注册或 `strength = 0` 时渲染器整段跳过暗角
/// pass、blit 直读主解析缓冲——与无暗角帧逐位一致（零开销默认路径）；
/// `DefaultPlugins` 不注册本资源。负值按 0 处理。
#[derive(Clone, Copy, Debug, iris_core::Resource)]
pub struct VignetteSettings {
    /// 压暗强度（0 = 关；0.8 左右为明显的电影式四角压暗，中心区域不受
    /// 影响）。
    pub strength: f32,
}

impl Default for VignetteSettings {
    fn default() -> Self {
        Self { strength: 0.0 }
    }
}

/// 景深（Depth of Field）设置。薄透镜近似的简化：聚焦平面附近的场景保持
/// 清晰，之外的像素按圆混淆圈（CoC）半径做泊松盘 HDR 平均（全分辨率
/// HDR → HDR pass，见 `shaders/dof.wgsl`）。
///
/// **资源缺席语义**：未注册或 `enabled = false` 时整段跳过 DOF pass，
/// 链与无 DOF 帧逐位一致（零开销默认路径）；`DefaultPlugins` 不注册本
/// 资源。`enabled` 为唯一开关——各项参数在关闭时不被读取。
#[derive(Clone, Copy, Debug, iris_core::Resource)]
pub struct DepthOfFieldSettings {
    /// 是否启用景深。
    pub enabled: bool,
    /// 聚焦平面距离（米）。v1 取**沿视线方向的线性视距**（view-Z），
    /// 与深度副本的存储口径一致；斜向距离（径向距离）在画面边缘略长，
    /// 是声明的近似（见 ARCHITECTURE §8）。挂了 [`Self::focus_target`]
    /// 且目标有效时本字段退为**回退值**。
    pub focus_distance: f32,
    /// 清晰带半宽（± 此范围内的像素 CoC = 0，逐位透传原色）。
    pub focus_range: f32,
    /// 最大模糊半径（纹素数）：CoC 归一化到 [0, 1] 后乘本值得实际采样
    /// 半径；12-tap 泊松盘在该半径内平均。
    pub max_blur_radius: f32,
    /// 焦点实体（打磨 VI 摩擦⑤）：`Some` 时对焦距离**每帧由渲染器解析**
    /// ——目标实体的 `GlobalTransform` 世界位经相机视图矩阵取 view-Z，与
    /// [`Self::focus_distance`] 同口径。解析点在 `Renderer::draw` 的 DOF
    /// 参数上传处（相机已解析、链头第一个全分辨率 pass 之前）：本资源是
    /// **配置而非状态**，解析结果只写进 uniform、不回写资源字段（避免
    /// 「游戏侧写的值被渲染器覆写」的所有权纠缠）。
    ///
    /// **缺席回退**：目标实体失效 / 无 `GlobalTransform` / 自身
    /// [`Visibility::HIDDEN`](crate::visibility::Visibility) → 回退
    /// [`Self::focus_distance`] 标量（不隐藏即无操作）。`GlobalTransform`
    /// 是上一帧层级传播的产物，跟随运动的目标有一帧滞后——对焦是连续
    /// 平滑量，一帧在清晰带半宽（米级）内不可感知。
    /// 纯函数解析入口见 [`resolve_focus_distance`]。
    pub focus_target: Option<iris_core::prelude::Entity>,
}

impl Default for DepthOfFieldSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            focus_distance: 10.0,
            focus_range: 2.0,
            max_blur_radius: 8.0,
            focus_target: None,
        }
    }
}

/// [`DepthOfFieldSettings::focus_target`] 的解析（纯函数，渲染器每帧
/// 调用一次——单测在此锁定语义，无需 GPU）：目标有效时返回其世界位经
/// `view` 变换后的视深（**view-Z 口径**，相机前方为正、与
/// `focus_distance` 字段一致）；`None` 目标 / 实体无 `GlobalTransform` /
/// 目标隐藏一律回退 `settings.focus_distance`。
///
/// `view` 取 [`crate::camera::view_from_global`]（与渲染器主相机同源的
/// 上一帧传播结果）。
pub fn resolve_focus_distance(
    world: &iris_core::world::World,
    settings: &DepthOfFieldSettings,
    view: &glam::Mat4,
) -> f32 {
    let Some(target) = settings.focus_target else {
        return settings.focus_distance;
    };
    let hidden = world
        .get::<crate::visibility::Visibility>(target)
        .is_some_and(|visibility| !visibility.visible);
    let position = match world.get::<iris_core::prelude::GlobalTransform>(target) {
        Some(global) if !hidden => global.position(),
        _ => return settings.focus_distance,
    };
    // 视空间 -z = 相机前方的线性视距（与透明排序 view_depth 同口径）
    let view_z = -(view * position.extend(1.0)).z;
    if view_z.is_finite() {
        view_z
    } else {
        settings.focus_distance
    }
}

/// 运动模糊（Motion Blur）设置。相机运动的帧间拖影：逐像素用当前帧
/// 深度重建世界位、经上一帧相机矩阵重投影，屏幕 UV 差即该像素速度，
/// 沿速度方向 N-tap 采样 HDR（全分辨率 pass，见 `shaders/motion_blur.wgsl`）。
///
/// **诚实边界**：v1 只覆盖**相机运动**——物体自身运动（速度缓冲 +
/// 物体级重投影）不做，静止物在相机运动帧中同样正确模糊，运动物体无
/// 拖影（见 ARCHITECTURE §8）。
///
/// **资源缺席语义**：未注册或 `enabled = false` 时整段跳过 MB pass，
/// 链与无 MB 帧逐位一致；相机静止（或首帧无上帧矩阵）时即使启用，
/// 速度恒 0、着色器原色透传，输出仍与无 MB 帧逐位一致。
#[derive(Clone, Copy, Debug, iris_core::Resource)]
pub struct MotionBlurSettings {
    /// 是否启用运动模糊。
    pub enabled: bool,
    /// 速度 → 采样 tap 数的缩放（tap 数 = clamp(速度长 × strength, 1,
    /// max_samples)；速度单位 = UV/帧）。1.0 下画面级速度（~0.1 UV/帧）
    /// 几乎不可见，写实拖影建议 32 以上。
    pub strength: f32,
    /// 方向采样 tap 数上限（速度钳制；实际采样循环恒跑满 32 槽、按
    /// tap 数掩码加权，上限只为质量/带宽调参服务）。
    pub max_samples: u32,
}

impl Default for MotionBlurSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            strength: 32.0,
            max_samples: 16,
        }
    }
}

/// FXAA（快速近似抗锯齿）设置（R53）。后处理链尾部的 LDR 域边缘平滑：
/// 色调映射后的画面按亮度梯度识别边缘、沿边缘方向做双点混合（单 pass
/// 片段着色器，见 `shaders/fxaa.wgsl`）。
///
/// **资源缺席或 `enabled = false` 时整段跳过**：blit 直写最终目标，与
/// 无 FXAA 帧逐位一致（零开销默认路径）；`DefaultPlugins` 不注册本资源。
/// 开启时 blit 改写一份中间 LDR 纹理（同格式），FXAA 采样后再写最终
/// 目标——UI 在 FXAA **之后**绘制，HUD 文字不被抹糊（链位理由见
/// ARCHITECTURE §4.25）。
///
/// 阈值工作在显示域（着色器把 sRGB 解码后的线性样本以 `pow(1/2.2)`
/// 近似回显示值算亮度——FXAA 原文的边缘检测假设即显示空间）：
/// `edge_threshold` / `edge_threshold_min` 取 FXAA 3.11 的典型常量。
#[derive(Clone, Copy, Debug, iris_core::Resource)]
pub struct FxaaSettings {
    /// 是否启用 FXAA。
    pub enabled: bool,
    /// 相对边缘阈值（亮度越大要求越高：暗部小对比度边缘不被抹）。
    pub edge_threshold: f32,
    /// 绝对边缘阈值下限（亮部的最小边缘亮度差）。
    pub edge_threshold_min: f32,
}

impl Default for FxaaSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            edge_threshold: 0.166,
            edge_threshold_min: 0.083,
        }
    }
}

/// SSR（屏幕空间反射）设置（R62b）。前向片元内对**上一帧**的颜色与深度
/// 做屏幕空间射线步进：命中 ⇒ 反射屏幕内内容（含动态物体与后摆进的
/// 物体）；未命中 ⇒ 回退探针 / env（R62a 通路）。设计与裁决详见
/// ARCHITECTURE §4.31。
///
/// **资源缺席语义**：未注册或 `enabled = false` 时整段关闭——不分配上帧
/// 颜色副本、不编码额外 pass、不写 SSR 参数（着色器 `count = 0` 早退），
/// 渲染与 R62a 逐位一致（零开销默认路径，全部既有 GPU FNV 基线原值）。
///
/// **重置语义**：首帧启用 / 关→开切换 / 分辨率变化 / 切镜（跟随
/// [`crate::renderer::Renderer::reset_taa`]）⇒ 上帧资源无效 ⇒ 该帧整段
/// 跳过，输出与 SSR 关闭帧逐位一致（`FrameStats::ssr_on` 观测字段）。
#[derive(Clone, Copy, Debug, iris_core::Resource)]
pub struct SsrSettings {
    /// 是否启用 SSR（`Default = false` = 关，与「缺席 = 关」语义一致）。
    pub enabled: bool,
    /// 全局强度倍率（≥ 0；1.0 = 物理权重，粗糙度 / Fresnel / 边缘衰减
    /// 全量生效）。**口径（R89 定稿）**：本项是 SSR **命中权重**的乘数、
    /// 不是亮度倍率——`> 1` 的部分被钳到 1（合成公式
    /// `mix(探针/env 镜面, ssr 反射色, w_ssr)` 的混合因子不允许越界），
    /// `0` ⇒ 与 SSR 关闭逐位一致（零反射）。R62b 起它被上传到
    /// `ssr.flags.y` 但着色器零读取（26 轮未发现的死旋钮，R89 修复）。
    pub intensity: f32,
    /// 步进最大距离（米）：反射射线的世界空间行程上限。
    pub max_distance: f32,
    /// 厚度容差（米）：穿越判定带宽——命中要求「上帧表面深度 < 射线点
    /// 深度 ≤ 上帧表面深度 + 厚度」。过小则斜穿漏判，过大则薄墙前后混淆。
    pub thickness: f32,
    /// 屏幕边缘衰减带宽（UV 比例，0.1 = 四周 10% 内随接近边缘线性衰减
    /// 到 0——命中点出屏方向淡出，避免屏幕边界处反射突然出现 / 消失）。
    pub edge_margin: f32,
    /// 粗糙度早退阈值（0..1）：粗糙度 ≥ 此值的片元不步进，直接走探针 /
    /// env（GGX lobe 半角 ~40° 的单射线采样无意义 + 上帧颜色无预滤波
    /// mip，步进纯浪费）。
    pub roughness_cutoff: f32,
}

impl Default for SsrSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            intensity: 1.0,
            max_distance: 12.0,
            thickness: 0.5,
            edge_margin: 0.1,
            roughness_cutoff: 0.6,
        }
    }
}

/// [`SsrSettings::roughness_cutoff`] 之下 SSR 强度开始衰减的粗糙度下限
/// （≤ 此值全量命中权重）。与 `standard.wgsl` 的 `SSR_ROUGH_FLOOR` 同值，
/// 着色器侧逐行镜像，两者必须同步修改。
pub const SSR_ROUGH_FLOOR: f32 = 0.25;

/// 粗糙度衰减曲线（与 `standard.wgsl` 的 `ssr_roughness_fade` 同一公式，
/// CPU 单测锁定，着色器侧逐行镜像）：
/// `clamp((cutoff − r) / (cutoff − floor), 0, 1)`——
/// `r ≤ floor` 全量 1（光滑面全量）、`r ≥ cutoff` 为 0（早退段）、中间
/// 线性下降。`floor == cutoff` 时不做除法，退化为二值判定。
pub fn ssr_roughness_fade(roughness: f32, floor: f32, cutoff: f32) -> f32 {
    if cutoff <= floor {
        return if roughness < cutoff { 1.0 } else { 0.0 };
    }
    ((cutoff - roughness) / (cutoff - floor)).clamp(0.0, 1.0)
}

/// 屏幕边缘衰减（与 `standard.wgsl` 的 `ssr_edge_fade` 同一公式，CPU
/// 单测锁定，着色器侧逐行镜像）：命中点 UV 到四边的最近距离除以带宽
/// 钳制到 [0, 1]，两轴相乘。`margin ≤ 0` 视为无衰减（恒 1）。
pub fn ssr_edge_fade(uv: [f32; 2], margin: f32) -> f32 {
    if margin <= 0.0 {
        return 1.0;
    }
    let fx = (uv[0].min(1.0 - uv[0]) / margin).clamp(0.0, 1.0);
    let fy = (uv[1].min(1.0 - uv[1]) / margin).clamp(0.0, 1.0);
    fx * fy
}

/// SSR 合成权重（R89 新增，`standard.wgsl` 的 `ssr_march` 命中支 fade
/// 表达式的**整条公式**镜像）：
///
/// ```text
/// w_ssr = clamp(roughness_fade(r, cutoff) × edge_fade(uv, margin) × intensity, 0, 1)
/// ```
///
/// R62b 的 CPU 镜像只有 roughness / edge 两个**分量**，合并权重没有镜像
/// 函数 ⇒ 着色器侧漏乘 `intensity`（`SsrSettings::intensity` 上传后 26
/// 轮零读取）没有 CPU 侧单测能抓到——本函数补上整条公式的钉子，并落实
/// AGENTS.md 新规约「每个公开旋钮至少一条非默认值断言」。
///
/// 与着色器同式同序：先乘两个衰减分量、再乘 intensity（上传侧已钳
/// `≥ 0`，此处同钳），最后整体钳 [0, 1]——`intensity = 1.0`（默认）时
/// 乘法与钳制均逐位恒等 ⇒ 全部既有像素不变；`0` ⇒ 恒 0（与 SSR 关闭
/// 一致）；`> 1` 的部分被钳掉（命中权重、不是亮度倍率）。
pub fn ssr_blend_weight(
    roughness: f32,
    uv: [f32; 2],
    intensity: f32,
    floor: f32,
    cutoff: f32,
    margin: f32,
) -> f32 {
    let fade = ssr_roughness_fade(roughness, floor, cutoff)
        * ssr_edge_fade(uv, margin)
        * intensity.max(0.0);
    fade.clamp(0.0, 1.0)
}

// ---------- GTAO 环境光遮蔽（R77，资源缺席 = 关 = 逐位不变） ----------

/// GTAO（Ground-Truth 环境光遮蔽的简化变体）设置（R77）。主 pass **之前**
/// 的独立全屏 pass：读**上一帧**主相机深度副本（scene_depth，与 SSR 同款
/// 上帧语义），按「切片水平角」法估算逐像素环境可见度，写半分辨率 R16Float
/// 可见度图；`standard.wgsl` 在主 pass 按屏幕坐标采样后乘在**间接光**
/// （IBL / 光照贴图 / 环境常量）上——与 ORM 的 ao 同靶：直接光不乘
/// （AO 描述小尺度互遮蔽的环境光损失，与 R41 既有语义一致）。
///
/// **资源缺席语义**：未注册或 `enabled = false` 时整段关闭——不分配可见
/// 度图、不编码 pass、标准着色器采样常驻 1×1 哑纹理（纹素恰为 1.0，
/// `x × 1.0` 逐位精确），渲染与 R76 逐位一致（零开销默认路径，全部既有
/// GPU FNV 基线原值）；`DefaultPlugins` 不注册本资源。
///
/// **重置语义**：可见度图依赖上一帧深度。深度副本没跑过（启用首帧 /
/// 上帧无不透明几何 / 分辨率变化重建）的帧不编码 GTAO、只把可见度图清成
/// 中性 1.0——该帧输出与 AO 关闭帧逐位一致（记账见
/// `Renderer::ao_depth_frame`）。
///
/// 算法细节与诚实限制（掠射角低估、深度断裂光晕、光照贴图叠加过暗等）
/// 见 ARCHITECTURE §4.33 与 §8。
#[derive(Clone, Copy, Debug, iris_core::Resource)]
pub struct AoSettings {
    /// 是否启用 GTAO（`Default = false` = 关，与「缺席 = 关」语义一致）。
    pub enabled: bool,
    /// 遮蔽强度指数：可见度 = `(1 − 遮蔽)^intensity`。1.0 = 线性（物理
    /// 权重）；越大角落越黑。0.0 = 恒 1（不可见，等同关闭但仍有 pass 开销）。
    pub intensity: f32,
    /// 屏幕空间搜索半径（米，世界尺度）：该距离内的深度样本参与水平角
    /// 估计，之外按 `1 − (d/r)²` 衰减到 0。0.3–1.0 为常用区间。
    pub radius: f32,
}

impl Default for AoSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            intensity: 1.0,
            radius: 0.5,
        }
    }
}

/// 每像素切片数（固定常量，非设置项——切片 × 步数决定 pass 成本与
/// 条带伪影水平，dither 旋转已压制大部分条带；调参与确定性收益不成比）。
pub const AO_SLICES: u32 = 4;

/// 每切片沿正负两方向各步进的样本数（固定常量，理由同 [`AO_SLICES`]）。
pub const AO_STEPS: u32 = 8;

/// 像素坐标 → [0, 1) 的确定性抖动值（与 `shaders/ao.wgsl` 的 `ao_dither`
/// 同一整数哈希逐行镜像，CPU 单测锁定）：整数乘法按 `wrapping` 语义，
/// WGSL 侧 u32 运算天然回绕，两侧位级一致。用途 = 按像素旋转 GTAO 切片
/// 方向（交错式 dither），把切片量化噪声打碎成不显眼的高频颗粒——
/// 纯像素坐标函数，无状态、无时域依赖，跨运行逐位一致。
pub fn ao_dither(pixel: [u32; 2]) -> f32 {
    let mut h = pixel[0].wrapping_mul(73_856_093) ^ pixel[1].wrapping_mul(19_349_663);
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    h ^= h >> 16;
    (h & 0x00FF_FFFF) as f32 / 16_777_216.0
}

/// 深度副本存储值（线性视距 `clip.w`）+ 屏幕 uv → 视空间位置重建（与
/// `shaders/ao.wgsl` 的 `ao_view_pos` 同一公式，CPU 单测经
/// `glam::camera::rh::proj::directx::perspective` 正反算锁定）：DirectX
/// RH 透视下 `clip.w = −view_z`，故 `p_view = (ndc·d·(aspect·tan, tan),
/// −d)`。
pub fn ao_view_pos(uv: glam::Vec2, view_d: f32, tan_half_fov: f32, aspect: f32) -> glam::Vec3 {
    let ndc = uv * 2.0 - glam::Vec2::ONE;
    glam::Vec3::new(
        ndc.x * view_d * aspect * tan_half_fov,
        ndc.y * view_d * tan_half_fov,
        -view_d,
    )
}

/// 世界尺度搜索步的世界偏移 → 屏幕 uv 偏移（与 `shaders/ao.wgsl` 的
/// 步进换算同一公式）：视平面上「每 uv 的世界尺寸」随深度线性放大
/// （x 轴再乘宽高比），`dir` 是切片方向上的**单位**向量（世界域）。
pub fn ao_uv_offset(
    dir: glam::Vec2,
    radius: f32,
    t: f32,
    view_d: f32,
    tan_half_fov: f32,
    aspect: f32,
) -> glam::Vec2 {
    let world_per_uv = glam::Vec2::new(
        2.0 * view_d * tan_half_fov * aspect,
        2.0 * view_d * tan_half_fov,
    );
    dir * (radius * t) / world_per_uv
}

/// 单样本对水平角的贡献（与 `shaders/ao.wgsl` 的 `ao_sample` 同一公式）：
/// 样本相对**切平面**（法线 `n`）的仰角正弦 × 距离衰减。切平面下方的
/// 样本（`dot < 0`）自然给出负值，调用方取 max 即「水平线只在上方抬高」；
/// 衰减 `1 − (d/r)²` 同时把半径外样本压到 ≤ 0（可判别地退出水平角竞争）。
pub fn ao_sample_sin(view_delta: glam::Vec3, normal: glam::Vec3, radius: f32) -> f32 {
    let dist = view_delta.length();
    if dist < 1e-4 {
        return 0.0;
    }
    let falloff = ao_falloff(dist, radius);
    if falloff <= 0.0 {
        // 半径外：负值保证在 max() 中不可能抬高水平线（0 也不行——
        // 半径外样本不得与「切片内切平面上的样本」竞争）
        return -1.0;
    }
    view_delta.dot(normal) / dist * falloff
}

/// 距离衰减（与着色器同式）：`1 − (d/r)²` 钳到 [0, 1]——半径内边缘平滑
/// 过渡、半径外恰 0。`r ≤ 0` 视为无搜索（恒 0，调用方钳半径 ≥ 1e-3）。
pub fn ao_falloff(distance: f32, radius: f32) -> f32 {
    if radius <= 0.0 {
        return 0.0;
    }
    (1.0 - (distance / radius) * (distance / radius)).clamp(0.0, 1.0)
}

/// 单切片遮蔽（与着色器循环体同式）：正负两方向水平角正弦（负值钳 0）
/// 之和的一半。N 片取平均后进 [`ao_visibility`]。
pub fn ao_slice_occlusion(sin_up: f32, sin_down: f32) -> f32 {
    0.5 * (sin_up.max(0.0) + sin_down.max(0.0))
}

/// 遮蔽 → 可见度（与着色器同式）：`(1 − occ)` 钳到 [0, 1] 后按
/// `intensity` 取幂。intensity = 1 线性；物理意义 = 把强度作为对比度
/// 指数（`pow(1, x) = 1` 恒精确，未遮蔽像素在任意强度下逐位不变）。
pub fn ao_visibility(occlusion: f32, intensity: f32) -> f32 {
    (1.0 - occlusion).clamp(0.0, 1.0).powf(intensity.max(0.0))
}

// ---------- 天空 v2（R79，资源缺席 = 旧渐变天空逐位不变） ----------

/// 程序化大气散射天空设置（R79）。`skybox.wgsl` 的片元在 mode = 1 时按
/// **片元内解析单次散射**（无 LUT——确定性零基础设施的裁定，质量对照旧
/// 渐变量级提升；LUT 路线留闭合池）计算 Rayleigh + Mie 散射与 HDR 太阳盘，
/// mode = 0（资源缺席或 [`Self::atmosphere`] = false）走旧 env cubemap
/// 采样路径——与引入前逐位一致（零开销默认路径，全部既有 GPU FNV 基线
/// 原值通过）；`DefaultPlugins` 不注册本资源。
///
/// **捕获相机同样生效**：天空参数经 skybox 组 binding 7 的 uniform 进入
/// 同一着色器，反射探针看到的天空与主相机一致（同源渲染是特性不是限制；
/// 但 IBL env cubemap 仍是旧渐变内容——大气不联动反射与环境光，见
/// ARCHITECTURE §8.48）。
///
/// 算法（平面指数大气 + Kasten 风格气团近似）与诚实限制（艺术化解析近似
/// 非物理量纲、行星曲率不可测等）见 ARCHITECTURE §4.36。
#[derive(Clone, Copy, Debug, iris_core::Resource)]
pub struct SkySettings {
    /// 是否启用大气散射（`Default = false`，与「缺席 = 旧渐变天空」语义
    /// 一致）。
    pub atmosphere: bool,
    /// Rayleigh（分子瑞利散射）项强度倍率。1.0 = 校准基线；0 = 关蓝天。
    pub rayleigh_strength: f32,
    /// Mie（气溶胶米氏散射）项强度倍率（太阳周向光晕的来源）。
    pub mie_strength: f32,
    /// 太阳盘 HDR 强度（盘面在色调映射前叠加 `sun_disk_intensity ×
    /// 太阳色 × 盘内 smoothstep`；32 ≈ ACES 后近白且参与泛光）。
    pub sun_disk_intensity: f32,
    /// 地平线以下的淡出色（世界空间 ray.y < 0 的视线按仰角淡到此色）。
    pub ground_color: glam::Vec3,
}

impl Default for SkySettings {
    fn default() -> Self {
        Self {
            atmosphere: false,
            rayleigh_strength: 1.0,
            mie_strength: 1.0,
            sun_disk_intensity: 32.0,
            ground_color: glam::Vec3::new(0.05, 0.05, 0.06),
        }
    }
}

/// TAA（时域抗锯齿）设置（R60）。后处理链首的 HDR 域时域累加：主相机
/// 投影按 Halton(2,3) 8 点序列做亚像素抖动，TAA pass 用当前帧抖动色 +
/// 深度副本 + 上帧历史做相机重投影与 3×3 邻域钳制混合（详见
/// ARCHITECTURE §4.29 的六项设计裁决）。
///
/// **资源缺席语义**：未注册或 `enabled = false` 时整段跳过——不抖动任何
/// 矩阵、不分配历史纹理、链命令流与无 TAA 帧逐位一致（零开销默认路径，
/// 全部既有 GPU FNV 基线的兼容铁律）；`DefaultPlugins` 不注册本资源。
///
/// **诚实边界**（§8.36）：无逐物体 / 蒙皮运动向量——动态物体的鬼影由
/// 邻域钳制压制不消除；相机切镜需宿主显式调 [`crate::renderer::Renderer::reset_taa`]
/// （切镜阈值检测是内容启发式，拒绝清单）。
#[derive(Clone, Copy, Debug, Default, iris_core::Resource)]
pub struct TaaSettings {
    /// 是否启用 TAA（`Default = false` = 关，与「缺席 = 关」语义一致）。
    pub enabled: bool,
}

// ---------- pass 串联框架 ----------

/// 后处理中间纹理的分辨率等级（相对主 HDR 解析缓冲）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PostScale {
    /// 半分辨率：泛光等低频效果（4× 省带宽，模糊本就抹掉高频）。
    Half,
    /// 全分辨率：逐像素效果（暗角等，按需分配）。
    Full,
}

impl PostScale {
    /// 等级数（`PostScratch` 每等级一个纹理对槽位）。
    const COUNT: usize = 2;

    fn index(self) -> usize {
        match self {
            PostScale::Half => 0,
            PostScale::Full => 1,
        }
    }

    /// 相对主缓冲的尺寸换算：半分辨率下取整、全分辨率原样；两者都至少
    /// 1×1（零尺寸输入钳为 1——全屏三角形至少要一个像素的目标）。半
    /// 分辨率公式与既有泛光实现逐位一致（`(w/2).max(1)`），保证默认
    /// 渲染像素不变。
    pub(crate) fn size(self, width: u32, height: u32) -> (u32, u32) {
        match self {
            PostScale::Half => ((width / 2).max(1), (height / 2).max(1)),
            PostScale::Full => (width.max(1), height.max(1)),
        }
    }
}

/// ping-pong 槽位：链上相邻 pass 在纹理对的 A / B 两槽间交替读写。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PingPongSlot {
    /// A 槽：泛光提取与模糊链的固定输出槽（blit 固定采样）。
    A,
    /// B 槽：模糊中转槽。
    B,
}

/// 中间纹理寻址：分辨率等级 + 槽位。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PostScratchRef {
    Half(PingPongSlot),
    Full(PingPongSlot),
}

/// pass 源：主 HDR 解析缓冲、某个中间槽位，或 TAA 本帧历史输出。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PostInput {
    /// 主解析缓冲（几何 + 天空 + 透明 + 粒子的混合结果）。
    Scene,
    /// 中间纹理对的某个槽位。
    Scratch(PostScratchRef),
    /// TAA 本帧输出（当前写半边的历史对纹理）。仅 TAA 开启时由
    /// [`chain_source`] 产出——下游链的源从主解析缓冲切到这里。
    TaaHistory,
}

/// 一对同规格 HDR 中间纹理（ping-pong）。纹理跨帧复用，只在首次使用
/// 或分辨率变化时重建（见 [`PostScratch::ensure_with`]）。
pub(crate) struct PostPair {
    #[allow(dead_code)]
    a: wgpu::Texture,
    a_view: wgpu::TextureView,
    #[allow(dead_code)]
    b: wgpu::Texture,
    b_view: wgpu::TextureView,
    /// 纹理尺寸（复用判定键，见 [`PostScratch::cache_hit`]）。
    size: (u32, u32),
}

/// ping-pong 分配器：按分辨率等级各缓存一对 HDR 中间纹理，`ensure` 按
/// 需创建 / 尺寸不符时整体重建（旧纹理随之释放）。复用决策以纹理对的
/// 实际尺寸为准（单一事实来源），并抽取为纯函数 [`Self::cache_hit`]
/// 便于离线单测；GPU 侧的分配 / 复用 / 重建路径由全部既有 GPU 测试隐式
/// 覆盖（多帧渲染测试的第二帧起恒走复用路径）。
#[derive(Default)]
pub(crate) struct PostScratch {
    /// 每等级纹理对（`None` = 未分配）。
    pairs: [Option<PostPair>; PostScale::COUNT],
}

impl PostScratch {
    /// 缓存命中判定（纯函数，离线单测）：已缓存**同一尺寸**才复用；
    /// 未分配或尺寸不符一律新建并整体替换。
    fn cache_hit(cached: Option<(u32, u32)>, size: (u32, u32)) -> bool {
        cached == Some(size)
    }

    /// 确保某等级纹理对就位：命中同尺寸缓存则零开销返回；否则经工厂
    /// 新建并整体替换。决策（[`Self::cache_hit`]）与纹理替换集中于此，
    /// GPU 工厂（[`Self::ensure`]）与未来可能的替身工厂共用同一分配
    /// 语义。
    fn ensure_with(
        &mut self,
        scale: PostScale,
        width: u32,
        height: u32,
        make_pair: impl FnOnce((u32, u32)) -> PostPair,
    ) {
        let target = scale.size(width, height);
        let i = scale.index();
        let cached = self.pairs[i].as_ref().map(|pair| pair.size);
        if Self::cache_hit(cached, target) {
            return;
        }
        self.pairs[i] = Some(make_pair(target));
    }

    /// GPU 工厂封装：按 HDR 中间缓冲规格（[`crate::pipeline::HDR_FORMAT`]，
    /// RENDER_ATTACHMENT | TEXTURE_BINDING）建一对纹理。
    pub(crate) fn ensure(
        &mut self,
        gpu: &crate::context::Gpu,
        scale: PostScale,
        width: u32,
        height: u32,
    ) {
        let device = &gpu.device;
        let (a_label, b_label) = match scale {
            PostScale::Half => ("iris.post.half.a", "iris.post.half.b"),
            PostScale::Full => ("iris.post.full.a", "iris.post.full.b"),
        };
        self.ensure_with(scale, width, height, |(w, h)| {
            let make_texture = |label: &'static str| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: crate::pipeline::HDR_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
            };
            let a = make_texture(a_label);
            let a_view = a.create_view(&wgpu::TextureViewDescriptor::default());
            let b = make_texture(b_label);
            let b_view = b.create_view(&wgpu::TextureViewDescriptor::default());
            PostPair {
                a,
                a_view,
                b,
                b_view,
                size: (w, h),
            }
        });
    }

    /// 某等级纹理对的两个槽位视图（未分配返回 `None`——调用方先 ensure）。
    pub(crate) fn pair_views(
        &self,
        scale: PostScale,
    ) -> Option<(&wgpu::TextureView, &wgpu::TextureView)> {
        let pair = self.pairs[scale.index()].as_ref()?;
        Some((&pair.a_view, &pair.b_view))
    }

    /// 按寻址解析单个槽位视图。
    fn view(&self, reference: PostScratchRef) -> Option<&wgpu::TextureView> {
        let (scale, slot) = match reference {
            PostScratchRef::Half(slot) => (PostScale::Half, slot),
            PostScratchRef::Full(slot) => (PostScale::Full, slot),
        };
        let pair = self.pairs[scale.index()].as_ref()?;
        Some(match slot {
            PingPongSlot::A => &pair.a_view,
            PingPongSlot::B => &pair.b_view,
        })
    }
}

/// 一帧后处理编码期的视图解析表：主解析缓冲 + 分配器（借用）+ 可选的
/// TAA 本帧输出视图。[`encode_post_pass`] 据此把 [`PostInput`] /
/// [`PostScratchRef`] 解析成实际 `TextureView`。
pub(crate) struct PostViews<'a> {
    scene: &'a wgpu::TextureView,
    scratch: &'a PostScratch,
    /// TAA 本帧输出（当前写半边的历史纹理视图）；TAA 关闭时 `None`
    /// （此时 [`PostInput::TaaHistory`] 不应出现在任何 pass 描述里）。
    taa_history: Option<&'a wgpu::TextureView>,
}

impl<'a> PostViews<'a> {
    pub(crate) fn new(
        scene: &'a wgpu::TextureView,
        scratch: &'a PostScratch,
        taa_history: Option<&'a wgpu::TextureView>,
    ) -> Self {
        Self {
            scene,
            scratch,
            taa_history,
        }
    }

    fn input(&self, input: PostInput) -> Option<&wgpu::TextureView> {
        match input {
            PostInput::Scene => Some(self.scene),
            PostInput::Scratch(reference) => self.scratch.view(reference),
            PostInput::TaaHistory => self.taa_history,
        }
    }
}

/// 一个全屏后处理 pass 的编码描述：管线 + 组 0 绑定要素 + 源 + 目标槽位。
/// 由 [`encode_post_pass`] 统一「描述 → 编码」，begin_render_pass /
/// set_pipeline / set_bind_group / draw 的样板全链只此一份。
#[derive(Clone, Copy)]
pub(crate) struct PostPassDesc<'a> {
    /// pass 调试标签（如 `"iris.pass.bloom_threshold"`）。
    pub label: &'static str,
    /// 全屏三角形管线（目标 = HDR 格式，见 `pipeline.rs` 的
    /// `make_post_pipeline`）。
    pub pipeline: &'a wgpu::RenderPipeline,
    /// 组 0 布局：uniform + 源纹理 + 采样器（链上全部中间 pass 同构）。
    pub layout: &'a wgpu::BindGroupLayout,
    /// 参数 uniform 缓冲（字节布局由 pass 的 WGSL 自行解释：泛光 32B
    /// `BlurParams` / 暗角 16B `PostParamsRaw`）。
    pub params: &'a wgpu::Buffer,
    /// 源视图。
    pub src: PostInput,
    /// 目标槽位（ping-pong 分配器管理）。
    pub dst: PostScratchRef,
    /// 组 0 binding 3 的可选附加纹理：DOF / MB 采样主相机深度副本
    /// （R32Float 线性视距，`textureLoad` 读取，非过滤）。`None` = 不绑
    /// （泛光 / 暗角等常规 pass；布局须与管线一致——`post_layout` 无此
    /// 槽，带附加纹理的管线用 `post_depth_layout`）。
    pub extra_texture: Option<&'a wgpu::TextureView>,
}

/// 描述 → 编码：全屏后处理 pass 的唯一编码路径（泛光三段与暗角都走
/// 此处）。目标槽位以清屏黑开始（全屏三角形覆写整帧，Clear 即「本
/// pass 独占输出」）；绑定组按描述现建（轻量句柄，数量 = 链上 pass 数
/// ≤ 8/帧——重资源纹理与 uniform 缓冲才值得跨帧缓存）。源或目标未
/// 分配时 panic：链拓扑契约是「先 [`PostScratch::ensure`] 再编码」。
pub(crate) fn encode_post_pass(
    encoder: &mut wgpu::CommandEncoder,
    device: &wgpu::Device,
    views: &PostViews<'_>,
    sampler: &wgpu::Sampler,
    desc: PostPassDesc<'_>,
) {
    let dst_view = views.scratch.view(desc.dst).unwrap_or_else(|| {
        panic!(
            "后处理 pass「{}」目标槽位未分配（应先 PostScratch::ensure 再编码）",
            desc.label
        )
    });
    let src_view = views
        .input(desc.src)
        .unwrap_or_else(|| panic!("后处理 pass「{}」源视图未分配（链拓扑错误）", desc.label));
    let mut entries = vec![
        wgpu::BindGroupEntry {
            binding: 0,
            resource: desc.params.as_entire_binding(),
        },
        wgpu::BindGroupEntry {
            binding: 1,
            resource: wgpu::BindingResource::TextureView(src_view),
        },
        wgpu::BindGroupEntry {
            binding: 2,
            resource: wgpu::BindingResource::Sampler(sampler),
        },
    ];
    if let Some(extra) = desc.extra_texture {
        entries.push(wgpu::BindGroupEntry {
            binding: 3,
            resource: wgpu::BindingResource::TextureView(extra),
        });
    }
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(desc.label),
        layout: desc.layout,
        entries: &entries,
    });
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(desc.label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: dst_view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_pipeline(desc.pipeline);
    pass.set_bind_group(0, &bind_group, &[]);
    pass.draw(0..3, 0..1);
}

/// TAA pass（R60）的编码描述。与 [`PostPassDesc`] 分立的原因：TAA 是链上
/// 唯一**有状态**的 pass——读上帧历史 + 写本帧历史槽 + 三路输入纹理
/// （抖动色 / 深度副本 / 历史），组 0 需要 5 个 binding（框架的
/// `post_layout` / `post_depth_layout` 上限是 4）；编码样板仍收拢在
/// [`encode_taa_pass`] 单点，不散落 renderer。
pub(crate) struct TaaPassDesc<'a> {
    /// pass 调试标签（`"iris.pass.taa"`）。
    pub label: &'static str,
    /// 全屏三角形管线（目标 = HDR 格式，`pipeline.rs` 的 `taa_pipeline`）。
    pub pipeline: &'a wgpu::RenderPipeline,
    /// 组 0 布局：uniform + 抖动色 + 采样器 + 深度副本（binding 3，非
    /// 过滤）+ 历史（binding 4，过滤），见 `taa_layout`。
    pub layout: &'a wgpu::BindGroupLayout,
    /// 参数 uniform（144B `TaaParamsRaw`：本帧逆视图投影 + 上帧视图投影
    /// + 标量）。
    pub params: &'a wgpu::Buffer,
    /// 线性采样器（历史重投影采样在亚纹素位置，Nearest 会量化回同纹素；
    /// 链共享 `post_sampler` 是 Nearest，TAA 用专属线性档——FXAA 同款
    /// 教训）。
    pub sampler: &'a wgpu::Sampler,
    /// 本帧抖动色源（主 HDR 解析缓冲）。
    pub src: &'a wgpu::TextureView,
    /// 主相机深度副本（R32Float 线性视距，`textureLoad` 读取）。
    pub depth: &'a wgpu::TextureView,
    /// 上帧历史（读半边）。
    pub history: &'a wgpu::TextureView,
    /// 本帧历史槽（写半边；下游链随即以 [`PostInput::TaaHistory`] 读它）。
    pub dst: &'a wgpu::TextureView,
}

/// TAA pass 的描述 → 编码（有状态 pass 的唯一编码路径）。目标以清屏黑
/// 开始（全屏三角形覆写整帧）。与 [`encode_post_pass`] 的差别仅在绑定
/// 条目清单（5 binding）与目标视图（历史对写半边）。
pub(crate) fn encode_taa_pass(
    encoder: &mut wgpu::CommandEncoder,
    device: &wgpu::Device,
    desc: TaaPassDesc<'_>,
) {
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(desc.label),
        layout: desc.layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: desc.params.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(desc.src),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(desc.sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(desc.depth),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(desc.history),
            },
        ],
    });
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(desc.label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: desc.dst,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_pipeline(desc.pipeline);
    pass.set_bind_group(0, &bind_group, &[]);
    pass.draw(0..3, 0..1);
}

/// 泛光模糊迭代轮数（每轮 = 水平 + 垂直两个 pass；3 轮在模糊半径与
/// 带宽开销之间取中，为既有质量基线）。
pub(crate) const BLOOM_BLUR_ROUNDS: usize = 3;

/// 泛光模糊链的 ping-pong 槽位序列：每轮「水平（读 A 写 B）→ 垂直
/// （读 B 写 A）」，任意轮数下输出恒回 A 槽——提取 pass 固定写 A、
/// blit 固定采样 A，链拓扑由本函数单点定义，改变轮数不破坏约定。
/// 纯函数，槽位交替逻辑的离线单测见模块尾。
pub(crate) fn bloom_blur_slots(rounds: usize) -> Vec<(PingPongSlot, PingPongSlot)> {
    (0..rounds)
        .flat_map(|_| {
            [
                (PingPongSlot::A, PingPongSlot::B),
                (PingPongSlot::B, PingPongSlot::A),
            ]
        })
        .collect()
}

/// 相机级后处理（DOF / MB）与暗角在同一全分辨率 ping-pong 对上的槽位
/// 分配结果。`None` = 该环节不在链上（源回退到主解析缓冲）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FullChainSlots {
    /// DOF 的目标槽位（源恒主解析缓冲；`None` = DOF 不入链）。
    pub dof_dst: Option<PingPongSlot>,
    /// MB 的源槽位（`None` = 源为主解析缓冲——DOF 关闭时）。
    pub mb_src: Option<PingPongSlot>,
    /// MB 的目标槽位（`None` = MB 不入链）。
    pub mb_dst: Option<PingPongSlot>,
    /// 链头：泛光与暗角读的槽位（`None` = 主解析缓冲）。
    pub head: Option<PingPongSlot>,
    /// 暗角的目标槽位（`None` = 暗角不入链）。
    pub vignette_dst: Option<PingPongSlot>,
    /// blit 的 HDR 源槽位（`None` = 直读主解析缓冲——链上无任何入链
    /// 的全分辨率 pass）。
    pub blit_hdr: Option<PingPongSlot>,
}

/// 全分辨率链的槽位分配（纯函数，单测见模块尾）：DOF 写 A、MB 写 B，
/// 暗角写在链头**对面**槽位（避免同 pass 读写同一纹理——wgpu 资源互斥），
/// blit 源 = 暗角开启取暗角输出、否则取链头。拓扑铁律与 `bloom_blur_slots`
/// 同款由测试守护。
pub(crate) fn full_chain_slots(dof_on: bool, mb_on: bool, vignette_on: bool) -> FullChainSlots {
    let dof_dst = dof_on.then_some(PingPongSlot::A);
    let (mb_src, mb_dst) = if mb_on {
        (dof_dst, Some(PingPongSlot::B))
    } else {
        (None, None)
    };
    let head = if mb_on { mb_dst } else { dof_dst };
    // 暗角目标：与既有行为一致默认写 A；链头占 A 时写对面槽位（避免
    // 同 pass 读写同一纹理——wgpu 资源互斥校验）
    let vignette_dst = vignette_on.then_some(match head {
        Some(PingPongSlot::A) => PingPongSlot::B,
        _ => PingPongSlot::A,
    });
    let blit_hdr = if vignette_on { vignette_dst } else { head };
    FullChainSlots {
        dof_dst,
        mb_src,
        mb_dst,
        head,
        vignette_dst,
        blit_hdr,
    }
}

/// 体积雾入链后的全链槽位（R79）。`slots` = 下游链（DOF/MB/暗角/blit）
/// 实际消费的槽位表——雾关闭时与 [`full_chain_slots`] 的返回**逐字段相同**
/// （渲染器走旧路径，命令流与 R78 逐位一致）；雾开启时整体轮转（见
/// [`full_chain_slots_with_fog`]）。`fog_dst` = 雾 pass 的目标槽位
/// （`None` = 雾不入链）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FogChainSlots {
    /// 下游链槽位（雾关 = 既有拓扑原样；雾开 = 轮转后的拓扑）。
    pub slots: FullChainSlots,
    /// 雾 pass 目标槽位（恒全分辨率 A 槽；`None` = 雾不入链）。
    pub fog_dst: Option<PingPongSlot>,
}

/// 全分辨率链的槽位分配（含体积雾，R79；纯函数，单测见模块尾）。
///
/// **旧函数零改动的红线**：[`full_chain_slots`] 及其既有单测保持原样，
/// 本函数单点定义雾开启后的新拓扑——雾槽 = 全分辨率 A 槽，插在链头
/// （链源 → 雾 → 其余原序）。槽位只有 A/B 一对，雾占据 A 的「场景级」
/// 位置后整条相机级链顺移一格（A→B→A 轮转）：
///
/// - 雾：链源（主解析缓冲 / TAA 输出）→ **A**；
/// - DOF：雾输出（A）→ **B**；
/// - MB：源 = DOF 输出（B，DOF 开）或雾输出（A，DOF 关），目标恒写
///   源的**对面**槽（ping-pong 铁律）；
/// - 链头 = 最后入链 pass 的输出（全关时 = 雾输出）；暗角写链头对面槽
///   （与旧规则同式），blit 源 = 暗角输出（暗角开）或链头。
///
/// 每种组合下「pass 读的槽位 ≠ 自己写的槽位」且「读到的槽位内容是该
/// pass 链上前驱的最新输出」——拓扑不变量由新增单测守护。
pub(crate) fn full_chain_slots_with_fog(
    dof_on: bool,
    mb_on: bool,
    vignette_on: bool,
    fog_on: bool,
) -> FogChainSlots {
    let base = full_chain_slots(dof_on, mb_on, vignette_on);
    if !fog_on {
        return FogChainSlots {
            slots: base,
            fog_dst: None,
        };
    }
    let fog_dst = Some(PingPongSlot::A);
    let dof_dst = dof_on.then_some(PingPongSlot::B);
    // MB：源 = DOF 输出（DOF 开）或雾输出（DOF 关），写对面槽
    let (mb_src, mb_dst) = if mb_on {
        match dof_dst {
            Some(PingPongSlot::B) => (Some(PingPongSlot::B), Some(PingPongSlot::A)),
            _ => (Some(PingPongSlot::A), Some(PingPongSlot::B)),
        }
    } else {
        (None, None)
    };
    let head = if mb_on {
        mb_dst
    } else if dof_on {
        dof_dst
    } else {
        fog_dst
    };
    let vignette_dst = vignette_on.then_some(match head {
        Some(PingPongSlot::A) => PingPongSlot::B,
        _ => PingPongSlot::A,
    });
    let blit_hdr = if vignette_on { vignette_dst } else { head };
    FogChainSlots {
        slots: FullChainSlots {
            dof_dst,
            mb_src,
            mb_dst,
            head,
            vignette_dst,
            blit_hdr,
        },
        fog_dst,
    }
}

/// 水下 pass 入链后的全链槽位（R94）。`slots` = 下游链实际消费的槽位表，
/// `fog_dst` / `uw_dst` = 雾与水下 pass 各自的目标槽（`None` = 不入链）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ColumnChainSlots {
    /// 下游链槽位（水下关 = 既有拓扑原样；开 = 再多轮转一格）。
    pub slots: FullChainSlots,
    /// 雾 pass 目标槽（恒 A；`None` = 雾不入链）。
    pub fog_dst: Option<PingPongSlot>,
    /// 水下 pass 目标槽（雾开时 = B、雾关时 = A；`None` = 水下不入链）。
    pub uw_dst: Option<PingPongSlot>,
}

/// 槽位的对面（ping-pong 铁律：pass 不得读写同一纹理）。
fn opposite_slot(slot: Option<PingPongSlot>) -> PingPongSlot {
    match slot {
        Some(PingPongSlot::A) => PingPongSlot::B,
        _ => PingPongSlot::A,
    }
}

/// 全分辨率链的槽位分配（含体积雾 + 水下，R94；纯函数，单测见模块尾）。
///
/// **两个既有函数零改动的红线**（[`full_chain_slots`] / [`full_chain_slots_with_fog`]
/// 及其全部单测原样保留）：本函数单点定义「链头前插几级」后的新拓扑——链上
/// 顺序恒为 `链源 → 雾? → 水下? → DOF? → MB? → 暗角? → blit`，每一级写入
/// 都落在前驱槽位的**对面**（A→B→A 轮转）。
///
/// 水下关闭时**直接委托**既有函数（逐字段相同的保证由构造给出，不靠测试
/// 兜底）：`uw_on = false` ⇒ 返回值 ≡ [`full_chain_slots_with_fog`]；
/// 雾与水下都关 ⇒ ≡ [`full_chain_slots`]。
pub(crate) fn full_chain_slots_with_water_column(
    dof_on: bool,
    mb_on: bool,
    vignette_on: bool,
    fog_on: bool,
    uw_on: bool,
) -> ColumnChainSlots {
    if !uw_on {
        let f = full_chain_slots_with_fog(dof_on, mb_on, vignette_on, fog_on);
        return ColumnChainSlots {
            slots: f.slots,
            fog_dst: f.fog_dst,
            uw_dst: None,
        };
    }
    // 水下开启 ⇒ 链头至少有一级前插，轮转起点恒 A（与雾的规则同构）。
    let fog_dst = fog_on.then_some(PingPongSlot::A);
    let uw_dst = Some(opposite_slot(fog_dst));
    let pre = uw_dst;
    let dof_dst = dof_on.then_some(opposite_slot(pre));
    let mb_src = if mb_on {
        if dof_on { dof_dst } else { pre }
    } else {
        None
    };
    let mb_dst = mb_on.then_some(opposite_slot(mb_src));
    let head = if mb_on {
        mb_dst
    } else if dof_on {
        dof_dst
    } else {
        pre
    };
    let vignette_dst = vignette_on.then_some(opposite_slot(head));
    let blit_hdr = if vignette_on { vignette_dst } else { head };
    ColumnChainSlots {
        slots: FullChainSlots {
            dof_dst,
            mb_src,
            mb_dst,
            head,
            vignette_dst,
            blit_hdr,
        },
        fog_dst,
        uw_dst,
    }
}

// ---------- TAA 纯函数（R60，离线单测见模块尾） ----------

/// TAA 抖动序列的点数（Halton(2,3) 前 8 项；帧号模 8 索引）。
pub const TAA_JITTER_POINTS: u64 = 8;

/// Halton 低差异序列（radical inverse）：`index` 的 `base` 进制位序反转。
/// `index = 0` 恒 0，从 1 起用（1/2, 1/3, …）。纯整数运算，确定性零依赖。
pub fn halton(index: u32, base: u32) -> f32 {
    let mut f = 1.0f32;
    let mut r = 0.0f32;
    let mut i = index;
    while i > 0 {
        f /= base as f32;
        r += f * (i % base) as f32;
        i /= base;
    }
    r
}

/// 第 `frame` 个 TAA 帧的亚像素抖动偏移（**纹素单位**，x/y 各自
/// (Halton − 0.5) ∈ ±0.5，像素中心对称分布）。调用方换算到 NDC：
/// `offset_px × (2/宽, 2/高)`。确定性：同帧号恒同偏移。
pub fn taa_jitter(frame: u64) -> [f32; 2] {
    let i = (frame % TAA_JITTER_POINTS) as u32 + 1;
    [halton(i, 2) - 0.5, halton(i, 3) - 0.5]
}

/// 把 NDC 偏移加进投影矩阵：`clip.x' = clip.x + jx·clip.w`（row0 +=
/// jx·row3，y 同理）。**必须作用于纯投影矩阵、再乘 view**——实现只更新
/// [row0][col2] 一个元素，前提是 row3 = (0,0,±1,0)（透视投影的 w 行）；
/// 对已合成 view 的矩阵该前提不成立，行运算也不再是纯 NDC 平移。
/// 零偏移**原样返回**（不碰矩阵任何位——TAA 关闭路径逐位一致的保证）。
///
/// 元素下标备忘（glam 列主序，`axis` = 列）：row0 += jx·row3 展开后只有
/// [row0][col2] 一个元素非零变化，系数取 [row3][col2] = `z_axis.w`
/// （透视矩阵的 w 行 = (0,0,±1,0)，RH 即 −1）。NDC 位移有解析单测。
///
/// 抖动只作用于主相机投影（主 pass / scene_depth 副本 / 遮挡查询代理
/// 与视锥剔除共用同一矩阵故一致抖动；CSM 光空间矩阵独立不抖动，
/// ARCHITECTURE §4.29 裁决 2）。
pub fn jitter_projection(proj: glam::Mat4, offset_ndc: glam::Vec2) -> glam::Mat4 {
    if offset_ndc == glam::Vec2::ZERO {
        return proj;
    }
    let mut m = proj;
    m.z_axis.x += offset_ndc.x * proj.z_axis.w;
    m.z_axis.y += offset_ndc.y * proj.z_axis.w;
    m
}

/// 链源切换（TAA 开启时下游链的源从主解析缓冲切到本帧历史输出）。
/// `head` = 全分辨率链的链头槽位（`None` = DOF/MB 均未入链）；TAA 开启
/// 且链头为空时源 = TAA 输出。blit 的源同理（`blit_hdr` 槽位）。纯函数
/// 单点定义，单测锁定「TAA 关 = 拓扑与 R59 逐位一致」。
pub(crate) fn chain_source(head: Option<PingPongSlot>, taa_on: bool) -> PostInput {
    match head {
        Some(slot) => PostInput::Scratch(PostScratchRef::Full(slot)),
        None if taa_on => PostInput::TaaHistory,
        None => PostInput::Scene,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{Mat4, Vec2, Vec3};
    use iris_core::hierarchy::propagate_transforms;
    use iris_core::prelude::{Entity, GlobalTransform, Transform, World};

    // ---- 焦点实体解析（打磨 VI 摩擦⑤）----

    /// 相机：原点平移 (0, 0, 6)、恒等旋转 ⇒ 注视 −Z（引擎前方），
    /// view 矩阵与渲染器主相机同源（[`crate::camera::view_from_global`]）。
    fn camera_view() -> Mat4 {
        let global = GlobalTransform::from_transform(Transform::from_xyz(0.0, 0.0, 6.0));
        crate::camera::view_from_global(&global)
    }

    /// 焦点目标实体（传播出 GlobalTransform 的常规层级实体）。
    fn target(world: &mut World, position: Vec3) -> Entity {
        let entity = world.spawn((Transform {
            position,
            ..Transform::IDENTITY
        },));
        propagate_transforms(world);
        entity
    }

    fn settings(focus_distance: f32, focus_target: Option<Entity>) -> DepthOfFieldSettings {
        DepthOfFieldSettings {
            enabled: true,
            focus_distance,
            focus_target,
            ..Default::default()
        }
    }

    /// 无焦点实体（None）→ 逐位回退标量：既有语义零改动。
    #[test]
    fn focus_distance_without_target_is_the_scalar() {
        let world = World::new();
        assert_eq!(
            resolve_focus_distance(&world, &settings(7.5, None), &camera_view()),
            7.5
        );
    }

    /// 焦点实体移动 → 对焦距离逐值跟随（相机 z = 6、目标 z = 1 ⇒ 5.0；
    /// 目标移到 z = −4 ⇒ 10.0）。口径是 **view-Z**：偏轴目标 (3, 2, 1)
    /// 仍解析为 5.0（径向距离会是 √38 ≈ 6.164）——与 `focus_distance`
    /// 字段文档声明的近似一致。
    #[test]
    fn focus_target_distance_follows_entity() {
        let mut world = World::new();
        let view = camera_view();
        let entity = target(&mut world, Vec3::new(0.0, 0.0, 1.0));
        let resolved = resolve_focus_distance(&world, &settings(1.0, Some(entity)), &view);
        assert!(
            (resolved - 5.0).abs() <= 1e-5,
            "目标在相机前方 5 m，对焦距离应为 5.0，实际 {resolved}"
        );
        // 目标移动：写 Transform 后重新传播，解析值即时跟随
        *world.get_mut::<Transform>(entity).unwrap() = Transform::from_xyz(0.0, 0.0, -4.0);
        propagate_transforms(&mut world);
        let moved = resolve_focus_distance(&world, &settings(1.0, Some(entity)), &view);
        assert!(
            (moved - 10.0).abs() <= 1e-5,
            "目标移到 10 m，对焦距离应为 10.0，实际 {moved}"
        );
        // view-Z 口径（偏轴不放大）
        let off_axis = target(&mut world, Vec3::new(3.0, 2.0, 1.0));
        let depth = resolve_focus_distance(&world, &settings(1.0, Some(off_axis)), &view);
        assert!(
            (depth - 5.0).abs() <= 1e-5,
            "偏轴距只按视轴投影计（径向为 6.164），实际 {depth}"
        );
    }

    /// 缺席回退两条：目标实体无 `GlobalTransform`（未入层级的裸实体）、
    /// 目标自身 `Visibility::HIDDEN` → 一律回退标量。
    #[test]
    fn focus_target_absent_or_hidden_falls_back_to_scalar() {
        let mut world = World::new();
        let bare = world.spawn((crate::visibility::Visibility::VISIBLE,));
        assert_eq!(
            resolve_focus_distance(&world, &settings(4.0, Some(bare)), &camera_view()),
            4.0,
            "无 GlobalTransform 的目标应回退标量"
        );
        let entity = target(&mut world, Vec3::new(0.0, 0.0, 1.0));
        world.insert(entity, crate::visibility::Visibility::HIDDEN);
        assert_eq!(
            resolve_focus_distance(&world, &settings(4.0, Some(entity)), &camera_view()),
            4.0,
            "隐藏的焦点实体应回退标量"
        );
    }

    #[test]
    fn half_scale_halves_and_clamps_to_one() {
        assert_eq!(
            PostScale::Half.size(0, 0),
            (1, 1),
            "零尺寸输入钳为 1×1（全屏三角形至少要一个像素的目标）"
        );
        assert_eq!(PostScale::Half.size(1, 1), (1, 1), "小于 2 的尺寸不产生 0");
        assert_eq!(
            PostScale::Half.size(7, 5),
            (3, 2),
            "半分辨率下取整（与既有泛光公式一致，保证默认渲染像素不变）"
        );
        assert_eq!(PostScale::Half.size(256, 256), (128, 128));
    }

    #[test]
    fn full_scale_keeps_size_and_clamps_to_one() {
        assert_eq!(PostScale::Full.size(0, 0), (1, 1), "零尺寸输入同样钳为 1×1");
        assert_eq!(PostScale::Full.size(640, 360), (640, 360), "全分辨率原样");
    }

    #[test]
    fn cache_reuse_requires_exact_size_match() {
        assert!(!PostScratch::cache_hit(None, (3, 2)), "未分配必须新建");
        assert!(
            PostScratch::cache_hit(Some((3, 2)), (3, 2)),
            "同尺寸命中即复用（跨帧零分配路径）"
        );
        assert!(
            !PostScratch::cache_hit(Some((3, 2)), (4, 2)),
            "宽度不符整体重建"
        );
        assert!(
            !PostScratch::cache_hit(Some((3, 2)), (3, 3)),
            "高度不符整体重建"
        );
    }

    #[test]
    fn bloom_slots_alternate_and_land_in_a() {
        let slots = bloom_blur_slots(BLOOM_BLUR_ROUNDS);
        assert_eq!(
            slots.len(),
            BLOOM_BLUR_ROUNDS * 2,
            "每轮恰为水平 + 垂直两个 pass"
        );
        for (src, dst) in &slots {
            assert_ne!(src, dst, "pass 不得读写同一槽位（ping-pong 铁律）");
        }
        for pair in slots.windows(2) {
            assert_eq!(
                pair[0].1, pair[1].0,
                "链必须连续：上一 pass 的输出槽 = 下一 pass 的输入槽"
            );
        }
        assert_eq!(
            slots.last().map(|(_, dst)| *dst),
            Some(PingPongSlot::A),
            "输出恒回 A 槽（blit 固定采样 A，改轮数不破坏约定）"
        );
        assert!(
            bloom_blur_slots(0).is_empty(),
            "零轮 = 空链（调用方整段跳过）"
        );
    }

    /// 全分辨率链槽位分配：全关 = 全 `None`（与 R42 基线完全相同的命令
    /// 流——逐位一致默认路径的拓扑前提）。
    #[test]
    fn full_chain_slots_all_off_is_pristine() {
        let slots = full_chain_slots(false, false, false);
        assert_eq!(
            slots,
            FullChainSlots {
                dof_dst: None,
                mb_src: None,
                mb_dst: None,
                head: None,
                vignette_dst: None,
                blit_hdr: None,
            },
            "DOF/MB/暗角全关时链必须与引入前完全一致"
        );
    }

    /// 8 种开关组合的拓扑不变量：
    /// 1. 任一 pass 的源槽位 ≠ 目标槽位（ping-pong 铁律）；
    /// 2. 链连续：DOF → MB → (泛光/暗角读链头) → blit；
    /// 3. blit 源 = 暗角输出（暗角开）或链头（暗角关）；
    /// 4. 暗角开且链头占 A 时暗角写 B（同 pass 读写互斥）。
    #[test]
    fn full_chain_slots_invariants_for_all_combinations() {
        for dof_on in [false, true] {
            for mb_on in [false, true] {
                for vignette_on in [false, true] {
                    let s = full_chain_slots(dof_on, mb_on, vignette_on);
                    let label = format!("dof={dof_on} mb={mb_on} vig={vignette_on}");
                    // 逐 pass 源/目标互斥
                    if let (Some(dst), Some(src)) = (s.mb_dst, s.mb_src) {
                        assert_ne!(dst, src, "MB 读写同槽：{label}");
                    }
                    if let Some(dst) = s.vignette_dst {
                        assert_ne!(
                            Some(dst),
                            s.head,
                            "暗角与链头同槽（同 pass 读写冲突）：{label}"
                        );
                    }
                    // 链连续：MB 入链时源 = DOF 输出槽（DOF 关则主解析缓冲）
                    if mb_on {
                        assert_eq!(
                            s.mb_src, s.dof_dst,
                            "MB 源必须取 DOF 输出槽（DOF 关则同为 None）"
                        );
                    }
                    assert_eq!(
                        s.head,
                        if mb_on { s.mb_dst } else { s.dof_dst },
                        "链头 = MB（开）或 DOF（开）的输出，都关则 None：{label}"
                    );
                    // blit 源约定
                    assert_eq!(
                        s.blit_hdr,
                        if vignette_on { s.vignette_dst } else { s.head },
                        "blit 源 = 暗角输出（暗角开）或链头：{label}"
                    );
                    // 槽位编码约定：DOF 恒 A、MB 恒 B
                    assert_eq!(s.dof_dst.map(|_| ()), dof_on.then_some(()), "{label}");
                    if dof_on {
                        assert_eq!(s.dof_dst, Some(PingPongSlot::A), "{label}");
                    }
                    if mb_on {
                        assert_eq!(s.mb_dst, Some(PingPongSlot::B), "{label}");
                    }
                    // 默认路径逐位一致的特例：无相机级效果时暗角行为不变
                    if !dof_on && !mb_on {
                        assert_eq!(
                            s.vignette_dst,
                            vignette_on.then_some(PingPongSlot::A),
                            "无相机级效果时暗角目标必须保持既有 A 槽：{label}"
                        );
                        assert_eq!(
                            s.blit_hdr,
                            vignette_on.then_some(PingPongSlot::A),
                            "无相机级效果时 blit 源必须保持既有约定：{label}"
                        );
                    }
                }
            }
        }
    }

    // ---- TAA 纯函数（R60） ----

    /// Halton 解析值对照（radical inverse 的手算锚点）。
    #[test]
    fn halton_matches_radical_inverse() {
        assert_eq!(halton(0, 2), 0.0, "index 0 恒 0");
        assert_eq!(halton(1, 2), 0.5, "1/2");
        assert_eq!(halton(2, 2), 0.25, "1/4");
        assert_eq!(halton(3, 2), 0.75, "3/4");
        assert_eq!(halton(4, 2), 0.125, "1/8");
        assert!((halton(1, 3) - 1.0 / 3.0).abs() <= 1e-7, "1/3");
        assert!((halton(2, 3) - 2.0 / 3.0).abs() <= 1e-7, "2/3");
        assert!((halton(3, 3) - 1.0 / 9.0).abs() <= 1e-7, "1/9");
        assert!(
            (halton(5, 3) - 7.0 / 9.0).abs() <= 1e-7,
            "5 = 12₃ → 21 反转 = 2/3+1/9"
        );
    }

    /// 抖动序列：8 点各自落回 ±0.5 纹素、两两不同、同帧号确定性复现、
    /// 帧号模 8 回绕。
    #[test]
    fn taa_jitter_sequence_properties() {
        let mut seen: Vec<((u32, u32), u64)> = Vec::new();
        for frame in 0..TAA_JITTER_POINTS {
            let j = taa_jitter(frame);
            assert!(
                j[0].abs() <= 0.5 && j[1].abs() <= 0.5,
                "抖动偏移必须 within ±0.5 纹素，实际 {j:?}"
            );
            let key = (j[0].to_bits(), j[1].to_bits());
            assert!(
                !seen.iter().any(|(k, _)| *k == key),
                "8 点必须两两不同：{frame} → {j:?}"
            );
            seen.push((key, frame));
            assert_eq!(j, taa_jitter(frame + TAA_JITTER_POINTS), "模 8 回绕");
        }
        assert_eq!(seen.len() as u64, TAA_JITTER_POINTS);
    }

    /// jitter_projection：零偏移**逐位恒等**（TAA 关闭路径的兼容保证）；
    /// 非零偏移使投影 NDC 精确位移 (jx, jy)（DirectX RH 透视，视空间点
    /// (0,0,−5) 的基线 NDC = (0,0)）。
    #[test]
    fn jitter_projection_ndc_shift() {
        let proj =
            glam::camera::rh::proj::directx::perspective(60f32.to_radians(), 1.0, 0.1, 100.0);
        let zero = jitter_projection(proj, Vec2::ZERO);
        assert_eq!(
            zero.to_cols_array(),
            proj.to_cols_array(),
            "零偏移必须逐位恒等（不能出现 −0.0 位翻转）"
        );
        let point = Vec3::new(0.0, 0.0, -5.0);
        let clip = proj * point.extend(1.0);
        let base_ndc = Vec2::new(clip.x, clip.y) / clip.w;
        assert!(
            base_ndc.length() <= 1e-6,
            "视轴点基线 NDC 应为 (0,0)，实际 {base_ndc}"
        );
        let j = Vec2::new(0.0012, -0.0008);
        let clip_j = jitter_projection(proj, j) * point.extend(1.0);
        let ndc_j = Vec2::new(clip_j.x, clip_j.y) / clip_j.w;
        assert!(
            (ndc_j - j).length() <= 1e-6,
            "抖动后 NDC 位移必须精确等于偏移，实际 {ndc_j} vs {j}"
        );
        // 偏轴点同样整体平移（刚体 NDC 偏移，不缩放）
        let off_axis = Vec3::new(0.7, -0.4, -5.0);
        let c0 = proj * off_axis.extend(1.0);
        let c1 = jitter_projection(proj, j) * off_axis.extend(1.0);
        let d0 = Vec2::new(c0.x, c0.y) / c0.w;
        let d1 = Vec2::new(c1.x, c1.y) / c1.w;
        assert!((d1 - d0 - j).length() <= 1e-6, "偏轴点位移也应等于偏移");
    }

    /// 链源切换三态：有链头槽位时 TAA 状态无关（槽位优先）；链头为空
    /// 且 TAA 开 → TAA 输出；全关 → 主解析缓冲（R59 拓扑原样）。
    #[test]
    fn chain_source_resolves_taa_head_and_scene() {
        assert_eq!(
            chain_source(Some(PingPongSlot::A), false),
            PostInput::Scratch(PostScratchRef::Full(PingPongSlot::A))
        );
        assert_eq!(
            chain_source(Some(PingPongSlot::B), true),
            PostInput::Scratch(PostScratchRef::Full(PingPongSlot::B)),
            "有链头时 TAA 不改变源（槽位优先）"
        );
        assert_eq!(chain_source(None, true), PostInput::TaaHistory);
        assert_eq!(chain_source(None, false), PostInput::Scene);
    }

    // ---- SSR 纯函数（R62b，standard.wgsl 同名函数的 CPU 侧锚点） ----

    /// 粗糙度衰减曲线三段：下限以下全量 1、上限以上 0、中间线性单调
    /// 下降（中点恰 0.5）。早退阈值以上为 0 是「粗糙面不走步进」的
    /// 权重侧保证（步进侧由着色器分支跳过）。
    #[test]
    fn ssr_roughness_fade_is_linear_between_floor_and_cutoff() {
        let floor = SSR_ROUGH_FLOOR;
        let cutoff = 0.6;
        assert_eq!(ssr_roughness_fade(0.0, floor, cutoff), 1.0, "完全镜面全量");
        assert_eq!(
            ssr_roughness_fade(floor, floor, cutoff),
            1.0,
            "下限处恰全量"
        );
        assert_eq!(
            ssr_roughness_fade(cutoff, floor, cutoff),
            0.0,
            "上限处恰归零"
        );
        assert_eq!(ssr_roughness_fade(1.0, floor, cutoff), 0.0, "上限以上恒 0");
        assert_eq!(
            ssr_roughness_fade((floor + cutoff) * 0.5, floor, cutoff),
            0.5,
            "中点线性中值"
        );
        // 单调不增：带内严格下降
        let mut prev = 1.0;
        let mut r = floor;
        while r <= cutoff {
            let v = ssr_roughness_fade(r, floor, cutoff);
            assert!(v <= prev, "衰减必须单调不增：r = {r}");
            assert!(v < prev || r <= floor, "带内必须严格下降：r = {r}");
            prev = v;
            r += 0.01;
        }
    }

    /// 衰减带退化为二值（floor == cutoff）：阈值以下 1、以上 0，不产生
    /// 除零。着色器分支同构。
    #[test]
    fn ssr_roughness_fade_degenerate_band_is_binary() {
        assert_eq!(ssr_roughness_fade(0.3, 0.5, 0.5), 1.0, "阈值以下全量");
        assert_eq!(ssr_roughness_fade(0.5, 0.5, 0.5), 0.0, "阈值处归零");
        assert_eq!(ssr_roughness_fade(0.7, 0.5, 0.5), 0.0, "阈值以上归零");
    }

    /// 边缘衰减：屏中心恒 1、边线恰 0、带宽内线性、两轴相乘、负带宽 =
    /// 无衰减。出屏方向淡出在 [0,1] 之外由着色器步进的出屏 break 兜底
    /// （本函数只对 [0,1] 内的命中点定义）。
    #[test]
    fn ssr_edge_fade_anchors_and_axes() {
        assert_eq!(ssr_edge_fade([0.5, 0.5], 0.1), 1.0, "屏中心无衰减");
        assert_eq!(ssr_edge_fade([0.0, 0.5], 0.1), 0.0, "边线衰减到 0");
        assert_eq!(ssr_edge_fade([0.5, 1.0], 0.1), 0.0);
        assert_eq!(ssr_edge_fade([0.05, 0.5], 0.1), 0.5, "半带宽处线性中值");
        assert_eq!(
            ssr_edge_fade([0.05, 0.05], 0.1),
            0.25,
            "两轴相乘（角区衰减更快）"
        );
        assert_eq!(ssr_edge_fade([0.05, 0.5], 0.0), 1.0, "负 / 零带宽 = 无衰减");
        assert_eq!(ssr_edge_fade([0.5, 0.5], 0.1), 1.0);
        // 带宽外（距边 ≥ margin）恒全量
        assert_eq!(ssr_edge_fade([0.11, 0.5], 0.1), 1.0);
    }

    /// 合成权重（R89 新镜像，B1 修复的钉子）：默认强度 1.0 逐位等于
    /// 「两分量之积」（既有像素不变的依据）；强度 0 ⇒ 恒 0（与 SSR 关闭
    /// 一致）；强度 2 ⇒ 钳到 1（命中权重不是亮度倍率）；中间线性。
    /// **非默认强度断言是本测试的主体**——分量镜像测试抓不住整条公式的
    /// 缺项，这正是强度死旋钮藏了 26 轮的结构性原因（AGENTS.md 测试规约
    /// 「每个公开旋钮至少一条非默认值断言」的出处）。
    #[test]
    fn ssr_blend_weight_follows_full_formula_with_nondefault_intensity() {
        let (floor, cutoff, margin) = (SSR_ROUGH_FLOOR, 0.6f32, 0.1f32);
        // 全量场景：粗糙度取下限、屏中心 ⇒ 两分量恒 1，权重 = intensity
        let full =
            |intensity: f32| ssr_blend_weight(0.2, [0.5, 0.5], intensity, floor, cutoff, margin);
        assert_eq!(
            full(1.0).to_bits(),
            (ssr_roughness_fade(0.2, floor, cutoff) * ssr_edge_fade([0.5, 0.5], margin)).to_bits(),
            "默认强度 1.0 ⇒ 与「分量之积」逐位一致（既有像素不变的依据）"
        );
        assert_eq!(full(0.0), 0.0, "强度 0 ⇒ 恒 0（与 SSR 关闭逐位一致）");
        assert_eq!(full(2.0), 1.0, "强度 2 ⇒ 钳到 1（命中权重、不是亮度倍率）");
        assert_eq!(full(-1.0), 0.0, "负强度 ⇒ 上传侧语义（max(0)）⇒ 0");
        // 中间线性：全量分量下权重 = intensity（钳制前）
        assert_eq!(full(0.5).to_bits(), (0.5f32).to_bits());
        // 半量场景：粗糙度取 (floor+cutoff)/2 ⇒ roughness 分量 0.5，
        // 权重 = 0.5 × intensity（linear 区间不触钳制）
        let mid_rough = (floor + cutoff) * 0.5;
        let w = ssr_blend_weight(mid_rough, [0.5, 0.5], 1.0, floor, cutoff, margin);
        assert_eq!(w.to_bits(), (0.5f32).to_bits(), "半粗糙 ⇒ 半权重");
        assert_eq!(
            ssr_blend_weight(mid_rough, [0.5, 0.5], 1.2, floor, cutoff, margin).to_bits(),
            (0.6f32).to_bits(),
            "0.5 × 1.2 = 0.6（未触钳制的放行区）"
        );
        assert_eq!(
            ssr_blend_weight(mid_rough, [0.5, 0.5], 3.0, floor, cutoff, margin),
            1.0,
            "0.5 × 3.0 = 1.5 ⇒ 钳到 1"
        );
    }

    // ---- GTAO 纯函数（R77，ao.wgsl 同名函数的 CPU 侧锚点） ----

    /// 抖动哈希：确定性复现、值域 [0, 1)、不同像素大体给出不同值、
    /// 对坐标离群值稳定（u32 全域无 panic）。
    #[test]
    fn ao_dither_is_deterministic_in_range_and_varied() {
        for px in [[0u32, 0], [1, 1], [640, 360], [u32::MAX, u32::MAX]] {
            let d = ao_dither(px);
            assert!(
                (0.0..1.0).contains(&d),
                "抖动值必须在 [0,1)，实际 {d:?} @ {px:?}"
            );
            assert_eq!(ao_dither(px), d, "同像素必须确定性复现 @ {px:?}");
        }
        // 分散性（统计口径）：1024 个像素 → 8-bit 量化桶期望命中
        // 256·(1 − (255/256)^1024) ≈ 251 桶（生日碰撞下期望约 5 桶为空，
        // 「全 256 桶命中 / 逐像素唯一」是统计上不可达的伪标准）。
        let mut seen = std::collections::HashSet::new();
        for y in 0..32u32 {
            for x in 0..32u32 {
                seen.insert((ao_dither([x, y]) * 256.0) as u32);
            }
        }
        assert!(
            seen.len() >= 200,
            "1024 个像素的量化抖动应高度分散，实际只有 {} 桶",
            seen.len()
        );
    }

    /// 重建公式与 glam 的 DirectX RH 透视**正反算互锁**：视空间点经投影
    /// 得 (uv, d)，重建必须还原原点（1e-6 相对容差——f32 在 d·tan 链上的
    /// 舍入）。这条测试是「重建与光栅化同口径」的解析证明：ao.wgsl 的
    /// 步进换算全建立在这个公式上。
    #[test]
    fn ao_view_pos_roundtrips_through_directx_perspective() {
        let fov = 60f32.to_radians();
        let aspect = 16.0 / 9.0;
        let proj = glam::camera::rh::proj::directx::perspective(fov, aspect, 0.1, 100.0);
        let tan_half = (fov * 0.5).tan();
        for view_p in [
            Vec3::new(0.0, 0.0, -5.0),
            Vec3::new(1.7, -2.3, -8.0),
            Vec3::new(-3.1, 4.2, -20.0),
        ] {
            let clip = proj * view_p.extend(1.0);
            let d = clip.w;
            let uv = (Vec2::new(clip.x, clip.y) / clip.w) * 0.5 + Vec2::splat(0.5);
            let rebuilt = ao_view_pos(uv, d, tan_half, aspect);
            let err = (rebuilt - view_p).length();
            assert!(
                err <= 1e-4 * view_p.length().max(1.0),
                "重建必须还原视空间点：{view_p} → {rebuilt}（误差 {err}）"
            );
        }
    }

    /// 步进换算：沿切片方向（世界域单位向量）按 `radius·t` 取世界偏移，
    /// 换算成 uv 偏移后以**同一深度**重建，落点与中心的水平距离必须恰为
    /// `radius·t`（方向分量按 dir 定义）——世界尺度搜索半径的解析证明。
    #[test]
    fn ao_uv_offset_preserves_world_radius() {
        let fov = 90f32.to_radians();
        let tan_half = (fov * 0.5).tan();
        let aspect = 1.0;
        let d = 10.0f32;
        let radius = 2.0f32;
        for t in [0.125f32, 0.5, 1.0] {
            let dir = Vec2::new(1.0, 0.0); // 世界 x 方向
            let off = ao_uv_offset(dir, radius, t, d, tan_half, aspect);
            let center = ao_view_pos(Vec2::splat(0.5), d, tan_half, aspect);
            let sample = ao_view_pos(Vec2::splat(0.5) + off, d, tan_half, aspect);
            let delta = sample - center;
            assert!(
                (delta.x - radius * t).abs() <= 1e-3,
                "世界偏移必须等于 radius·t：t={t} 实际 {}",
                delta.x
            );
            assert!(
                delta.y.abs() <= 1e-4,
                "x 方向步进不得串到 y（实际 {}）",
                delta.y
            );
            assert!(delta.z.abs() <= 1e-4, "同深度平面内步进 z 不变");
        }
    }

    /// 距离衰减锚点：距离 0 全量、半半径 0.75、半径处恰 0、半径外恒 0、
    /// 非正半径 = 无搜索恒 0。
    #[test]
    fn ao_falloff_anchors() {
        assert_eq!(ao_falloff(0.0, 0.5), 1.0, "距离 0 全量");
        assert_eq!(ao_falloff(0.25, 0.5), 0.75, "半半径 1 − ¼");
        assert_eq!(ao_falloff(0.5, 0.5), 0.0, "半径处恰归零");
        assert_eq!(ao_falloff(1.0, 0.5), 0.0, "半径外恒 0");
        assert_eq!(ao_falloff(0.1, 0.0), 0.0, "零半径 = 无搜索");
        assert_eq!(ao_falloff(0.1, -1.0), 0.0, "负半径 = 无搜索");
    }

    /// 单样本仰角正弦：切平面上的样本贡献 0；沿法线上方距离 δ 的样本
    /// 贡献 `sinθ·falloff`（解析值对照）；切平面下方为负；半径外给 -1
    /// （可判别地退出水平角竞争）；重合点退化 0。
    #[test]
    fn ao_sample_sin_anchors() {
        let n = Vec3::Y; // 切平面 = xz 平面
        let p = Vec3::ZERO;
        // 切平面内样本：dot(vv, n) = 0
        let flat = ao_sample_sin(Vec3::new(0.3, 0.0, -0.1), n, 1.0);
        assert_eq!(flat, 0.0, "切平面上的样本不遮蔽");
        // 法线上方 30° 仰角、距离 0.5 的样本：sin = 0.5，falloff = 0.75
        let raised = ao_sample_sin(Vec3::new(0.25 * 3f32.sqrt(), 0.25, 0.0), n, 1.0);
        let expected = 0.5 * 0.75;
        assert!(
            (raised - expected).abs() <= 1e-6,
            "仰角正弦 × 衰减应为 {expected}，实际 {raised}"
        );
        // 切平面下方：负值（max 竞争中恒输给 0 水平线）
        let below = ao_sample_sin(Vec3::new(0.0, -0.25, 0.0), n, 1.0);
        assert!(below < 0.0, "切平面下方样本必为负，实际 {below}");
        // 半径外：-1（不得抬高水平线）
        assert_eq!(
            ao_sample_sin(Vec3::new(0.0, 2.0, 0.0), n, 1.0),
            -1.0,
            "半径外样本给 -1"
        );
        // 重合点：0（除零保护）
        assert_eq!(ao_sample_sin(p - p, n, 1.0), 0.0, "重合点退化为 0");
    }

    /// 切片遮蔽与可见度曲线：锚点 + 单调性。未遮蔽像素（occ = 0）在任意
    /// 强度下可见度**逐位恰 1**（`pow(1, x) = 1` 的精确性是「AO 开启但
    /// 无遮蔽的像素不变」的根据）；强度指数单调；负强度按 0 处理。
    #[test]
    fn ao_visibility_curve_and_slice_anchors() {
        assert_eq!(ao_slice_occlusion(0.0, 0.0), 0.0);
        assert_eq!(ao_slice_occlusion(1.0, 0.0), 0.5);
        assert_eq!(ao_slice_occlusion(-1.0, 0.4), 0.2, "负水平角钳 0");
        // 可见度锚点
        assert_eq!(ao_visibility(0.0, 1.0), 1.0);
        assert_eq!(ao_visibility(0.0, 2.5), 1.0, "未遮蔽在任意强度下恰 1");
        assert_eq!(ao_visibility(1.0, 1.0), 0.0, "全遮蔽可见度 0");
        assert_eq!(ao_visibility(1.5, 1.0), 0.0, "超界遮蔽钳 0");
        let half = ao_visibility(0.5, 1.0);
        assert!((half - 0.5).abs() <= 1e-6);
        assert!(
            (ao_visibility(0.5, 2.0) - 0.25).abs() <= 1e-6,
            "强度 = 指数：0.5² = 0.25"
        );
        assert_eq!(
            ao_visibility(0.5, -1.0),
            1.0,
            "负强度按 0 处理（无遮蔽效果）"
        );
        // 遮蔽增大 → 可见度单调不增
        let mut prev = 1.0;
        for i in 0..=20 {
            let occ = i as f32 / 20.0;
            let v = ao_visibility(occ, 1.5);
            assert!(v <= prev, "可见度必须单调不增：occ = {occ}");
            prev = v;
        }
    }

    // ---- 体积雾链拓扑（R79） ----

    /// 雾关闭 = 既有拓扑**逐字段相同**（渲染器走旧路径的根据——命令流
    /// 与 R78 逐位一致）+ 雾槽恒 `None`。
    /// 水下入链后「水下关闭」必须与既有链拓扑**逐字段**相同（全部 8 种
    /// dof / mb / vignette 组合 × 雾开 / 关）——渲染器据此在「无水下架」时
    /// 走与 R93 完全相同的命令流。
    #[test]
    fn column_chain_slots_off_matches_fog_only_exactly() {
        for fog_on in [false, true] {
            for dof_on in [false, true] {
                for mb_on in [false, true] {
                    for vignette_on in [false, true] {
                        let legacy = full_chain_slots_with_fog(dof_on, mb_on, vignette_on, fog_on);
                        let got = full_chain_slots_with_water_column(
                            dof_on,
                            mb_on,
                            vignette_on,
                            fog_on,
                            false,
                        );
                        assert_eq!(
                            got,
                            ColumnChainSlots {
                                slots: legacy.slots,
                                fog_dst: legacy.fog_dst,
                                uw_dst: None,
                            },
                            "水下关闭必须逐字段等价雾档拓扑：fog={fog_on}                          dof={dof_on} mb={mb_on} vig={vignette_on}"
                        );
                    }
                }
            }
        }
    }

    /// 水下开启的全部 32 种组合的拓扑不变量：
    /// 1. 水下槽恒在雾槽的对面（雾开 ⇒ B、雾关 ⇒ A）；
    /// 2. 每一级 pass 读的槽位 ≠ 自己写的槽位（wgpu 同 pass 读写互斥）；
    /// 3. 每一级读到的是链上前驱的**最新**输出（源链不断裂）；
    /// 4. blit 源 = 暗角输出（暗角开）或链头（否则），且链头恒为最后一个
    ///    入链 pass 的目标。
    #[test]
    fn column_chain_slots_invariants_for_all_combinations() {
        for fog_on in [false, true] {
            for uw in [false, true] {
                for dof_on in [false, true] {
                    for mb_on in [false, true] {
                        for vignette_on in [false, true] {
                            let c = full_chain_slots_with_water_column(
                                dof_on,
                                mb_on,
                                vignette_on,
                                fog_on,
                                uw,
                            );
                            if uw {
                                assert_eq!(
                                    c.uw_dst,
                                    Some(opposite_slot(c.fog_dst)),
                                    "水下槽须在雾的对面：fog={fog_on}"
                                );
                            }
                            // 2 / 3：逐级「读槽 ≠ 写槽」且读的是前驱输出
                            if let (Some(fog), Some(col)) = (c.fog_dst, c.uw_dst) {
                                assert_ne!(fog, col, "雾与水下不得同槽");
                            }
                            if let (Some(src), Some(dst)) = (c.slots.dof_dst, c.uw_dst) {
                                if uw {
                                    assert_ne!(src, dst, "DOF 读槽须≠写槽");
                                }
                            }
                            if let (Some(src), Some(dst)) = (c.slots.mb_src, c.slots.mb_dst) {
                                assert_ne!(src, dst, "MB 读槽须≠写槽");
                            }
                            if let (Some(src), Some(dst)) = (c.slots.head, c.slots.vignette_dst) {
                                assert_ne!(src, dst, "暗角读槽须≠写槽");
                            }
                            // MB 的源 = 链上 DOF（开）或水下 / 雾（DOF 关）的输出
                            if mb_on {
                                let want = if dof_on {
                                    c.slots.dof_dst
                                } else {
                                    c.uw_dst.or(c.fog_dst)
                                };
                                assert_eq!(
                                    c.slots.mb_src, want,
                                    "MB 源须是前驱输出：fog={fog_on} uw={uw} dof={dof_on}"
                                );
                            }
                            // 4：链头 = 最后入链 pass 的目标
                            let want_head = if mb_on {
                                c.slots.mb_dst
                            } else if dof_on {
                                c.slots.dof_dst
                            } else {
                                c.uw_dst.or(c.fog_dst)
                            };
                            assert_eq!(c.slots.head, want_head, "链头判定漂移");
                            let want_blit = if vignette_on {
                                c.slots.vignette_dst
                            } else {
                                c.slots.head
                            };
                            assert_eq!(c.slots.blit_hdr, want_blit, "blit 源判定漂移");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn fog_chain_slots_off_matches_legacy_exactly() {
        for dof_on in [false, true] {
            for mb_on in [false, true] {
                for vignette_on in [false, true] {
                    let legacy = full_chain_slots(dof_on, mb_on, vignette_on);
                    let with_fog = full_chain_slots_with_fog(dof_on, mb_on, vignette_on, false);
                    assert_eq!(
                        with_fog,
                        FogChainSlots {
                            slots: legacy,
                            fog_dst: None,
                        },
                        "雾关闭必须与既有 full_chain_slots 逐字段相同：dof={dof_on} mb={mb_on} vig={vignette_on}"
                    );
                }
            }
        }
    }

    /// 雾开启的全部 8 种组合的拓扑不变量：
    /// 1. 雾槽恒全分辨率 A；
    /// 2. 任一 pass 的源槽位 ≠ 目标槽位（ping-pong 铁律）；
    /// 3. 链连续：链源 → 雾(A) → DOF(B) → MB(对面槽) → (泛光/暗角读链头)
    ///    → blit——每个下游 pass 的源槽位内容 = 其链上前驱的最新输出；
    /// 4. 暗角写链头对面槽；blit 源 = 暗角输出（暗角开）或链头。
    #[test]
    fn fog_chain_slots_invariants_for_all_combinations() {
        for dof_on in [false, true] {
            for mb_on in [false, true] {
                for vignette_on in [false, true] {
                    let s = full_chain_slots_with_fog(dof_on, mb_on, vignette_on, true);
                    let label = format!("dof={dof_on} mb={mb_on} vig={vignette_on}");
                    assert_eq!(
                        s.fog_dst,
                        Some(PingPongSlot::A),
                        "雾槽恒 A（场景级位置）：{label}"
                    );
                    // 雾读链源（主解析缓冲 / TAA 输出，不在 A/B 槽）、写 A
                    // ——DOF 的源必须是雾输出
                    if let Some(dof_dst) = s.slots.dof_dst {
                        assert_eq!(s.fog_dst, Some(PingPongSlot::A), "雾槽恒 A：{label}");
                        assert_ne!(dof_dst, PingPongSlot::A, "DOF 读写同槽：{label}");
                        assert_eq!(dof_dst, PingPongSlot::B, "DOF 槽位约定 = B：{label}");
                    }
                    // MB：源 = DOF 输出（DOF 开）或雾输出（DOF 关），目标
                    // 恒为源的对面槽
                    if let Some(mb_dst) = s.slots.mb_dst {
                        let mb_src = s.slots.mb_src.expect("MB 入链必有源槽位");
                        assert_ne!(mb_dst, mb_src, "MB 读写同槽：{label}");
                        let expected_src = if dof_on {
                            PingPongSlot::B
                        } else {
                            PingPongSlot::A
                        };
                        assert_eq!(mb_src, expected_src, "MB 源 = DOF 输出 / 雾输出：{label}");
                    }
                    // 链头 = 最后入链 pass 的输出
                    let expected_head = if mb_on {
                        s.slots.mb_dst
                    } else if dof_on {
                        s.slots.dof_dst
                    } else {
                        s.fog_dst
                    };
                    assert_eq!(s.slots.head, expected_head, "链头定义：{label}");
                    // 暗角写链头对面槽（同 pass 读写互斥）
                    if let Some(dst) = s.slots.vignette_dst {
                        assert_ne!(
                            Some(dst),
                            s.slots.head,
                            "暗角与链头同槽（同 pass 读写冲突）：{label}"
                        );
                    }
                    assert_eq!(
                        s.slots.blit_hdr,
                        if vignette_on {
                            s.slots.vignette_dst
                        } else {
                            s.slots.head
                        },
                        "blit 源 = 暗角输出（暗角开）或链头：{label}"
                    );
                }
            }
        }
    }
}
