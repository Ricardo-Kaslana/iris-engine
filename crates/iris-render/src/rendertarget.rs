//! 渲染目标（R40）：把场景渲染到离屏纹理（小地图 / 监控屏播放器的基础）。
//!
//! 本模块只保存**CPU 侧描述**（尺寸）+ 句柄——与 `TextureStore` 的「CPU
//! 目录 + GPU 懒上传」模式对齐。GPU 纹理 / 中间缓冲 / 绑定组由
//! [`crate::renderer::Renderer`] 按句柄惰性创建并跨帧持有（见
//! `renderer.rs` 的 `GpuRenderTarget`），store 侧零 GPU 依赖。
//!
//! ## 用法
//!
//! ```ignore
//! // 游戏侧按需插入（不进 DefaultPlugins——缺席 = 无 RT 相机，主路径零变化）
//! world.insert_resource(RenderTargetStore::new());
//! let minimap = world.resource_mut::<RenderTargetStore>().add(200, 200);
//! // 相机实体：挂 ActiveCamera + RendersTo → 每帧渲染一次到对应 RT
//! world.spawn((Transform::from_xyz(0.0, 30.0, 0.0), ActiveCamera::default(), RendersTo { target: minimap }));
//! ```
//!
//! ## v1 限制（诚实清单，见 ARCHITECTURE §8.20）
//!
//! - **尺寸变化 = 下一帧整体重建**：[`RenderTargetStore::resize`] 改 CPU
//!   描述后，渲染器按句柄比对尺寸、失配重建 GPU 资源（重建帧旧内容即刻
//!   作废，读回 / 增量重分配仍不做）；
//! - **多 RT 相机数量上限 2**：超出部分 `log::warn` 跳过（小地图 + 监控屏
//!   两个典型用例，多视口编排是后续路线图）；
//! - 0 号句柄无特殊保留（与 `TextureStore` 的白色 0 号不同）：任何句柄都
//!   是对应尺寸的合法 RT，调用方自行用 `RendersTo` 关联。

use iris_core::Resource;

/// 渲染目标句柄，指向 [`RenderTargetStore`] 中的条目。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct RenderTargetHandle(pub u32);

/// CPU 侧渲染目标描述（尺寸；GPU 资源由渲染器惰性创建并跨帧持有）。
///
/// 尺寸以像素计，与 [`crate::camera`] 的宽高比输入同口径（window 程序取
/// `Viewport` 资源，离屏 / headless 直接以期望像素写）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderTargetSpec {
    /// 宽（像素，≥ 1）。
    pub width: u32,
    /// 高（像素，≥ 1）。
    pub height: u32,
}

/// CPU 侧渲染目标目录（World 资源）。
///
/// **不随 `DefaultPlugins` 强制注册**：游戏侧需要多视口时按需
/// `insert_resource(RenderTargetStore::new())`；资源缺席 = 世界中无 RT
/// 相机，渲染器走既有单视口主路径（字节零变化）。
#[derive(Resource, Default)]
pub struct RenderTargetStore {
    targets: Vec<RenderTargetSpec>,
}

impl RenderTargetStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一个渲染目标，返回其句柄（宽度 / 高度下限 1）。
    pub fn add(&mut self, width: u32, height: u32) -> RenderTargetHandle {
        self.targets.push(RenderTargetSpec {
            width: width.max(1),
            height: height.max(1),
        });
        RenderTargetHandle(self.targets.len() as u32 - 1)
    }

    /// 按句柄取 CPU 描述。
    pub fn get(&self, handle: RenderTargetHandle) -> Option<&RenderTargetSpec> {
        self.targets.get(handle.0 as usize)
    }

    /// 运行时改尺寸（闭合 §8.20「无 resize」条）：更新 CPU 侧描述并返回
    /// `true`；句柄无效返回 `false`（不新建条目——建条目是 `add` 的语义）。
    /// 宽高下限 1（钳制口径与 [`RenderTargetStore::add`] 一致）。
    ///
    /// GPU 侧生效方式：渲染器每帧按句柄比对缓存资源尺寸与本描述，失配则
    /// **整体重建**（旧纹理 / 缓冲随缓存条目释放；RT 实例缓冲本就每帧
    /// 全量上传，无增量损失）。同尺寸重复调用是良性空操作（缓存比对
    /// 相等 ⇒ GPU 零动作）。
    pub fn resize(&mut self, handle: RenderTargetHandle, width: u32, height: u32) -> bool {
        let Some(spec) = self.targets.get_mut(handle.0 as usize) else {
            return false;
        };
        spec.width = width.max(1);
        spec.height = height.max(1);
        true
    }

    pub fn len(&self) -> usize {
        self.targets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }
}

/// 把实体挂载相机输出到指定渲染目标（小地图 / 监控屏）。
///
/// 挂载实体**须同时挂 [`crate::camera::ActiveCamera`]**（本组件不包含相机
/// 参数，仅声明输出目标）。渲染器收集全部 `(ActiveCamera + RendersTo)`
/// 实体，各自渲染一次到对应 [`RenderTargetHandle`]；主视图 = 无
/// `RendersTo` 的相机（见 `renderer.rs` 相机解析）。
///
/// **缺席语义**：未挂本组件的相机 = 主视图相机（默认路径，行为零变化）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, iris_core::Component)]
pub struct RendersTo {
    pub target: RenderTargetHandle,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_adds_sized_targets_with_dense_handles() {
        let mut store = RenderTargetStore::new();
        assert!(store.is_empty());
        let a = store.add(200, 200);
        assert_eq!(a, RenderTargetHandle(0));
        let b = store.add(320, 180);
        assert_eq!(b, RenderTargetHandle(1));
        assert_eq!(store.len(), 2);
        assert_eq!(store.get(a).unwrap().width, 200);
        assert_eq!(store.get(a).unwrap().height, 200);
        assert_eq!(store.get(b).unwrap().width, 320);
        assert_eq!(store.get(b).unwrap().height, 180);
        assert!(store.get(RenderTargetHandle(99)).is_none());
    }

    #[test]
    fn store_clamps_nonpositive_dimensions_to_one() {
        let mut store = RenderTargetStore::new();
        let h = store.add(0, 0);
        let spec = store.get(h).unwrap();
        assert_eq!((spec.width, spec.height), (1, 1), "零尺寸钳制到 1×1");
    }

    /// `resize`（§8.20 闭合）：有效句柄改尺寸返回 true 并即时反映到
    /// `get`；无效句柄 false 且**不新建条目**；零/负尺寸钳到 1（与 `add`
    /// 同口径）；`len` 全程不变。
    #[test]
    fn resize_updates_existing_entries_and_rejects_invalid_handles() {
        let mut store = RenderTargetStore::new();
        let a = store.add(200, 200);
        assert!(store.resize(a, 320, 180), "有效句柄应成功");
        assert_eq!(
            store.get(a),
            Some(&RenderTargetSpec {
                width: 320,
                height: 180
            })
        );
        assert!(
            !store.resize(RenderTargetHandle(99), 64, 64),
            "无效句柄必须 false"
        );
        assert_eq!(store.len(), 1, "resize 不得新建条目");
        assert!(store.resize(a, 0, 0));
        assert_eq!(
            store.get(a).unwrap(),
            &RenderTargetSpec {
                width: 1,
                height: 1
            },
            "钳制口径与 add 一致"
        );
    }

    #[test]
    fn renders_to_carries_target() {
        let mut store = RenderTargetStore::new();
        let h = store.add(64, 64);
        let rt = RendersTo { target: h };
        assert_eq!(rt.target, h);
    }
}
