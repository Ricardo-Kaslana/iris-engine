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
//! - **无 `resize`**：RT 尺寸创建后固定，尺寸变化需重建 store 条目（v1
//!   不做运行时 resize——监控屏分辨率一般固定，读回 / 重分配留待后续）；
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

    #[test]
    fn renders_to_carries_target() {
        let mut store = RenderTargetStore::new();
        let h = store.add(64, 64);
        let rt = RendersTo { target: h };
        assert_eq!(rt.target, h);
    }
}
