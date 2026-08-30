//! 后处理设置。

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
        Self { enabled: true, strength: 0.15, threshold: 1.0 }
    }
}

impl BloomSettings {
    pub fn disabled() -> Self {
        Self { enabled: false, ..Default::default() }
    }
}
