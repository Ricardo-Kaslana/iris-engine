//! 后处理：设置资源 + 全屏 pass 串联框架 v1。
//!
//! 后处理链（编译期有序，详见 ARCHITECTURE §4.11）：
//!
//! ```text
//! 主 HDR 解析缓冲（几何 + 天空 + 透明 + 粒子）
//!   → 景深（DOF，可选，全分辨率）：按逐像素线性视距算 CoC → 12-tap
//!     泊松盘 HDR 平均；需要主相机深度副本（scene_depth pass，同样可选）
//!   → 运动模糊（MB，可选，全分辨率）：深度重建世界位 → 上帧相机重投影
//!     得逐像素屏幕速度 → 沿速度方向 N-tap 采样（v1 只覆盖相机运动）
//!   → 泛光（半分辨率：亮度提取 → 3×(水平/垂直) 模糊，ping-pong A/B）
//!   → 暗角（全分辨率 HDR → HDR，强度 0 时整段跳过）
//!   → blit（泛光叠加 + 曝光 + ACES 色调映射——链的固定终点）→ sRGB
//! ```
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
    /// 是声明的近似（见 ARCHITECTURE §8）。
    pub focus_distance: f32,
    /// 清晰带半宽（± 此范围内的像素 CoC = 0，逐位透传原色）。
    pub focus_range: f32,
    /// 最大模糊半径（纹素数）：CoC 归一化到 [0, 1] 后乘本值得实际采样
    /// 半径；12-tap 泊松盘在该半径内平均。
    pub max_blur_radius: f32,
}

impl Default for DepthOfFieldSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            focus_distance: 10.0,
            focus_range: 2.0,
            max_blur_radius: 8.0,
        }
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

/// pass 源：主 HDR 解析缓冲或某个中间槽位。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PostInput {
    /// 主解析缓冲（几何 + 天空 + 透明 + 粒子的混合结果）。
    Scene,
    /// 中间纹理对的某个槽位。
    Scratch(PostScratchRef),
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

/// 一帧后处理编码期的视图解析表：主解析缓冲 + 分配器（借用）。
/// [`encode_post_pass`] 据此把 [`PostInput`] / [`PostScratchRef`] 解析成
/// 实际 `TextureView`。
pub(crate) struct PostViews<'a> {
    scene: &'a wgpu::TextureView,
    scratch: &'a PostScratch,
}

impl<'a> PostViews<'a> {
    pub(crate) fn new(scene: &'a wgpu::TextureView, scratch: &'a PostScratch) -> Self {
        Self { scene, scratch }
    }

    fn input(&self, input: PostInput) -> Option<&wgpu::TextureView> {
        match input {
            PostInput::Scene => Some(self.scene),
            PostInput::Scratch(reference) => self.scratch.view(reference),
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
