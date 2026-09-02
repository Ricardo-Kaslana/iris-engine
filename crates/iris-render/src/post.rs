//! 后处理：设置资源 + 全屏 pass 串联框架 v1。
//!
//! 后处理链（编译期有序，详见 ARCHITECTURE §4.11）：
//!
//! ```text
//! 主 HDR 解析缓冲（几何 + 天空 + 透明 + 粒子）
//!   → 泛光（半分辨率：亮度提取 → 3×(水平/垂直) 模糊，ping-pong A/B）
//!   → 暗角（全分辨率 HDR → HDR，强度 0 时整段跳过）
//!   → blit（泛光叠加 + 曝光 + ACES 色调映射——链的固定终点）→ sRGB
//! ```
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
                resource: wgpu::BindingResource::TextureView(src_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
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
}
