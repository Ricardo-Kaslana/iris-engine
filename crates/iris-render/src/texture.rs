//! 纹理：程序化生成的 CPU 纹理目录（零外部图像解码依赖）。

use iris_core::Resource;

/// 纹理句柄，指向 `TextureStore` 中的条目。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct TextureHandle(pub u32);

/// CPU 侧 RGBA8 纹理。
#[derive(Clone, Debug)]
pub struct CpuTexture {
    pub width: u32,
    pub height: u32,
    /// RGBA8，行主序，每像素 4 字节。
    pub rgba: Vec<u8>,
}

impl CpuTexture {
    /// 1x1 纯色纹理（常用于无贴图的纯色材质）。
    pub fn solid(color: [u8; 4]) -> Self {
        Self {
            width: 1,
            height: 1,
            rgba: color.to_vec(),
        }
    }

    /// 程序化棋盘格纹理。
    pub fn checkerboard(tiles: u32, cell: u32, a: [u8; 4], b: [u8; 4]) -> Self {
        let size = tiles * cell;
        let mut rgba = Vec::with_capacity((size * size * 4) as usize);
        for y in 0..size {
            for x in 0..size {
                let c = if ((x / cell) + (y / cell)) % 2 == 0 {
                    a
                } else {
                    b
                };
                rgba.extend_from_slice(&c);
            }
        }
        Self {
            width: size,
            height: size,
            rgba,
        }
    }
}

/// CPU 侧纹理目录（World 资源）。0 号槽位固定为白色纯色纹理。
#[derive(Resource)]
pub struct TextureStore {
    textures: Vec<CpuTexture>,
}

impl Default for TextureStore {
    fn default() -> Self {
        Self {
            textures: vec![CpuTexture::solid([255, 255, 255, 255])],
        }
    }
}

impl TextureStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, texture: CpuTexture) -> TextureHandle {
        self.textures.push(texture);
        TextureHandle(self.textures.len() as u32 - 1)
    }

    pub fn get(&self, handle: TextureHandle) -> Option<&CpuTexture> {
        self.textures.get(handle.0 as usize)
    }

    pub fn len(&self) -> usize {
        self.textures.len()
    }

    pub fn is_empty(&self) -> bool {
        self.textures.is_empty()
    }

    /// CPU 源内存占用（字节）：Σ 宽×高×4（RGBA8 源数据，不含 Vec 的
    /// 分配器开销）。「低占用」卖点的观测口径之一。
    pub fn memory_bytes(&self) -> u64 {
        self.textures
            .iter()
            .map(|t| t.width as u64 * t.height as u64 * 4)
            .sum()
    }

    /// GPU 侧显存占位**估算**（非精确值：驱动可能对齐 / 复制分配，
    /// 采样器等附属对象不计）。按渲染器上传决策树逐纹理计算（R30）：
    /// - BC1 路径（`feature_bc` 为真且尺寸 4 对齐且全不透明，决策见
    ///   [`crate::texture_bcn::bc1_eligible`]）：4×4 块 8 字节 = 0.5 B/px；
    /// - 其余（含 web / 无特性 / 非对齐 / 半透明回退）：RGBA8 4 B/px。
    ///
    /// mipmap v1 无（单 mip 计）；同一 `feature_bc` 取值下与渲染器实际上传
    /// 格式一致（决策函数共用），`false` / `true` 两次调用的差即压缩节省。
    pub fn gpu_bytes_estimate(&self, feature_bc: bool) -> u64 {
        self.textures
            .iter()
            .map(|t| {
                if crate::texture_bcn::bc1_eligible(t.width, t.height, &t.rgba, feature_bc) {
                    (t.width as u64 / 4) * (t.height as u64 / 4) * 8
                } else {
                    t.width as u64 * t.height as u64 * 4
                }
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkerboard_dimensions_and_colors() {
        let tex = CpuTexture::checkerboard(2, 4, [0, 0, 0, 255], [255, 255, 255, 255]);
        assert_eq!(tex.width, 8);
        assert_eq!(tex.height, 8);
        assert_eq!(tex.rgba.len(), 8 * 8 * 4);
        // 左上角第一格为 a
        assert_eq!(&tex.rgba[0..4], &[0, 0, 0, 255]);
        // (4,0) 属于第二列 → b（像素起始字节 = (y×8+x)×4 = 16）
        let idx = 4 * 4;
        assert_eq!(&tex.rgba[idx..idx + 4], &[255, 255, 255, 255]);
    }

    #[test]
    fn store_zero_is_white_fallback() {
        let mut store = TextureStore::new();
        assert_eq!(
            store.get(TextureHandle(0)).unwrap().rgba,
            vec![255, 255, 255, 255]
        );
        let handle = store.add(CpuTexture::solid([255, 0, 0, 255]));
        assert_eq!(handle, TextureHandle(1));
        assert_eq!(store.get(handle).unwrap().rgba, vec![255, 0, 0, 255]);
        assert!(store.get(TextureHandle(99)).is_none());
    }

    /// 内存统计（R30）：memory_bytes = Σ 源 RGBA 字节；gpu_bytes_estimate
    /// 按上传决策树计——BC 特性下 4 对齐不透明纹理 0.5 B/px，其余 4 B/px；
    /// 两次估算之差即压缩节省。
    #[test]
    fn memory_stats_track_source_and_gpu_estimate() {
        let mut store = TextureStore::new();
        // 0 号兜底：1×1 纯色（非 4 对齐 → 恒走 RGBA 路径）
        assert_eq!(store.memory_bytes(), 4);
        assert_eq!(store.gpu_bytes_estimate(true), 4);
        // 8×8 棋盘（不透明、4 对齐）：特性开启走 BC1 = 2 块×2 块×8 B = 32 B
        let checker = CpuTexture::checkerboard(2, 4, [0, 0, 0, 255], [255, 255, 255, 255]);
        assert!(checker.rgba.iter().all(|&b| b == 255 || b == 0));
        store.add(checker);
        assert_eq!(store.memory_bytes(), 4 + 8 * 8 * 4);
        assert_eq!(store.gpu_bytes_estimate(true), 4 + 32);
        // 同一 store 关闭特性：全 RGBA
        assert_eq!(store.gpu_bytes_estimate(false), 4 + 8 * 8 * 4);
    }
}
