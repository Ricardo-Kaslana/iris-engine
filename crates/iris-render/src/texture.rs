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
}
