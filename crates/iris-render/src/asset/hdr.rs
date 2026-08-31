//! Radiance `.hdr`（RGBE）全景图解析：外部 equirectangular 环境贴图入口。
//!
//! 支持：
//! - `#?` 魔数（`#?RADIANCE` / `#?RGBE` 等）+ 任意头部键值行 + 空行结束；
//! - `-Y <h> +X <w>` 分辨率行（其余朝向形式 → [`HdrError::Unsupported`]）；
//! - 旧式平面扫描线（每像素 4 字节 RGBE）与新式逐通道 RLE 扫描线
//!   （扫描线以 `[2, 2, hi, lo]` 开头且 `hi:lo == width`）。
//!
//! RGBE 编码：每像素 4 字节，R/G/B 共享一个 8 位指数，值 = m/256 × 2^(e-128)
//! ——约 8 位相对精度、动态范围极宽，恰好匹配 HDR 环境值（太阳 ≫ 1、阴影 ≪ 1）。
//!
//! ```ignore
//! let image = iris_render::asset::load_hdr("assets/sunset.hdr")?;
//! world.insert_resource(iris_render::environment::SkySource::equirect_hdr(image));
//! ```

use std::fmt;
use std::path::Path;

/// 线性 RGB 全景图（行主序，每像素 3 个 f32，第 0 行 = equirect 的天顶行）。
#[derive(Clone, Debug)]
pub struct HdrImage {
    pub width: u32,
    pub height: u32,
    /// 行主序线性 RGB（长度 = width × height × 3）。
    pub rgb: Vec<f32>,
}

impl HdrImage {
    /// 构造指定尺寸的全零图。
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, rgb: vec![0.0; (width * height * 3) as usize] }
    }

    /// 读取 (x, y) 处的线性 RGB。
    pub fn pixel(&self, x: u32, y: u32) -> [f32; 3] {
        let i = ((y * self.width + x) * 3) as usize;
        [self.rgb[i], self.rgb[i + 1], self.rgb[i + 2]]
    }

    /// 写入 (x, y) 处的线性 RGB。
    pub fn set_pixel(&mut self, x: u32, y: u32, value: [f32; 3]) {
        let i = ((y * self.width + x) * 3) as usize;
        self.rgb[i..i + 3].copy_from_slice(&value);
    }
}

/// `.hdr` 解析错误。
#[derive(Debug)]
pub enum HdrError {
    Io(std::io::Error),
    /// 数据损坏：结构或长度与 Radiance 规范不符。
    Format(String),
    /// 合法 `.hdr` 但使用了本加载器不支持的特性（如其他分辨率朝向）。
    Unsupported(String),
}

impl fmt::Display for HdrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO 错误：{e}"),
            Self::Format(what) => write!(f, "Radiance 格式错误：{what}"),
            Self::Unsupported(what) => write!(f, "暂不支持的 Radiance 特性：{what}"),
        }
    }
}

impl std::error::Error for HdrError {}

impl From<std::io::Error> for HdrError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// 从文件加载 `.hdr`。
pub fn load_hdr(path: impl AsRef<Path>) -> Result<HdrImage, HdrError> {
    let bytes = std::fs::read(path)?;
    parse_hdr(&bytes)
}

/// 解析内存中的 `.hdr` 字节。
pub fn parse_hdr(bytes: &[u8]) -> Result<HdrImage, HdrError> {
    let mut pos = 0usize;
    // 头部：首行 `#?` 魔数，其后任意键值行，空行结束
    let magic = next_line(bytes, &mut pos)
        .ok_or_else(|| HdrError::Format("文件为空（缺少魔数行）".into()))?;
    if !magic.starts_with(b"#?") {
        return Err(HdrError::Format(format!(
            "魔数行必须以 `#?` 开头，实际 {:?}",
            String::from_utf8_lossy(magic)
        )));
    }
    loop {
        let Some(line) = next_line(bytes, &mut pos) else {
            return Err(HdrError::Format("头部未以空行结束（缺少分辨率行）".into()));
        };
        // FORMAT= / EXPOSURE= 等头部键值对本实现无关紧要，跳过
        if line.is_empty() {
            break;
        }
    }
    // 分辨率行：-Y <h> +X <w>
    let res_line = next_line(bytes, &mut pos)
        .ok_or_else(|| HdrError::Format("缺少分辨率行".into()))?;
    let text = String::from_utf8_lossy(res_line).into_owned();
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let (height_str, width_str) = match tokens.as_slice() {
        ["-Y", h, "+X", w] => (*h, *w),
        _ => {
            return Err(HdrError::Unsupported(format!(
                "分辨率行仅支持 `-Y <h> +X <w>`，实际 {text:?}"
            )))
        }
    };
    let height: u32 = height_str
        .parse()
        .map_err(|_| HdrError::Format(format!("非法高度：{height_str}")))?;
    let width: u32 = width_str
        .parse()
        .map_err(|_| HdrError::Format(format!("非法宽度：{width_str}")))?;
    if width == 0 || height == 0 {
        return Err(HdrError::Format(format!("尺寸为零：{width}×{height}")));
    }

    let w = width as usize;
    let mut image = HdrImage::new(width, height);
    for y in 0..height {
        let scanline = decode_scanline(bytes, &mut pos, w)?;
        for x in 0..w {
            // R/G/B 三通道共享第 4 字节指数
            let value = rgbe_to_rgb(
                scanline[x],
                scanline[w + x],
                scanline[2 * w + x],
                scanline[3 * w + x],
            );
            image.set_pixel(x as u32, y, value);
        }
    }
    // 宽高乘积必须与数据量吻合：尾部只允许空白（文件末尾换行）
    if bytes[pos..].iter().any(|&b| !b.is_ascii_whitespace()) {
        return Err(HdrError::Format("扫描线数据之后存在多余字节".into()));
    }
    Ok(image)
}

/// 读取一行（不含行尾；兼容 \n 与 \r\n）。返回 None 表示数据耗尽。
fn next_line<'a>(data: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    if *pos >= data.len() {
        return None;
    }
    let rest = &data[*pos..];
    let end = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
    let mut line = &rest[..end];
    if line.last() == Some(&b'\r') {
        line = &line[..line.len() - 1];
    }
    *pos += end + if end < rest.len() { 1 } else { 0 };
    Some(line)
}

/// 解码一条扫描线为 R/G/B/E 四组宽度字节（平面排布：[R…][G…][B…][E…]）。
fn decode_scanline(data: &[u8], pos: &mut usize, width: usize) -> Result<Vec<u8>, HdrError> {
    // 新式 RLE：扫描线以 [2, 2, hi, lo] 开头且 hi:lo == width。
    // 宽度校验是新旧格式的关键判别（旧式像素恰为 [2,2,·,·] 时不得误判）。
    if data.get(*pos..*pos + 4)
        == Some(&[2u8, 2, (width >> 8) as u8, (width & 0xff) as u8])
    {
        *pos += 4;
        let mut out = Vec::with_capacity(width * 4);
        for _ in 0..4 {
            rle_channel(data, pos, width, &mut out)?;
        }
        return Ok(out);
    }
    // 旧式扫描线：每像素 4 字节 RGBE（交错）；展开为 R/G/B/E 四段平面排布，
    // 与 RLE 路径的输出布局统一，调用方按同一方式索引
    let end = *pos + width * 4;
    let slice = data
        .get(*pos..end)
        .ok_or_else(|| HdrError::Format("扫描线数据不足（文件被截断）".into()))?;
    *pos = end;
    let mut out = vec![0u8; width * 4];
    for x in 0..width {
        out[x] = slice[x * 4];
        out[width + x] = slice[x * 4 + 1];
        out[2 * width + x] = slice[x * 4 + 2];
        out[3 * width + x] = slice[x * 4 + 3];
    }
    Ok(out)
}

/// 解码一条 RLE 通道游程：count > 128 为重复游程（下一字节重复 count-128 次），
/// 否则为 count 字节字面量。通道解码长度必须恰为 width。
fn rle_channel(data: &[u8], pos: &mut usize, width: usize, out: &mut Vec<u8>) -> Result<(), HdrError> {
    let start = out.len();
    while out.len() - start < width {
        let Some(&count) = data.get(*pos) else {
            return Err(HdrError::Format("RLE 数据被截断".into()));
        };
        *pos += 1;
        if count > 128 {
            let Some(&value) = data.get(*pos) else {
                return Err(HdrError::Format("RLE 重复游程被截断".into()));
            };
            *pos += 1;
            out.extend(std::iter::repeat_n(value, (count - 128) as usize));
        } else {
            if count == 0 {
                // 0 长度字面量会原地打转，规范上也是非法编码
                return Err(HdrError::Format("RLE 字面量游程长度为 0".into()));
            }
            let end = *pos + count as usize;
            let literal = data
                .get(*pos..end)
                .ok_or_else(|| HdrError::Format("RLE 字面量游程被截断".into()))?;
            *pos = end;
            out.extend_from_slice(literal);
        }
    }
    if out.len() - start != width {
        return Err(HdrError::Format("RLE 游程长度超出扫描线宽度".into()));
    }
    Ok(())
}

/// RGBE 4 字节 → 线性 RGB。指数 0 表示全零（真正的黑，区分于最小指数）。
fn rgbe_to_rgb(r: u8, g: u8, b: u8, e: u8) -> [f32; 3] {
    if e == 0 {
        return [0.0; 3];
    }
    // m/256 × 2^(e-128) = m × 2^(e-136)
    let scale = (2f32).powi(e as i32 - 136);
    [r as f32 * scale, g as f32 * scale, b as f32 * scale]
}

/// 把线性 RGB 全景图编码为 Radiance `.hdr` 字节（旧式平面扫描线）。
/// 供示例生成器与测试使用；文件体积敏感时可自行改写为 RLE。
pub fn encode_hdr(image: &HdrImage) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        64 + image.width as usize * image.height as usize * 4,
    );
    out.extend_from_slice(b"#?RADIANCE\n");
    out.extend_from_slice(b"FORMAT=32-bit_rle_rgbe\n");
    out.push(b'\n');
    out.extend_from_slice(format!("-Y {} +X {}\n", image.height, image.width).as_bytes());
    for y in 0..image.height {
        for x in 0..image.width {
            let [r, g, b] = image.pixel(x, y);
            out.extend_from_slice(&rgbe_from(r, g, b));
        }
    }
    out
}

/// 线性 RGB → RGBE 4 字节（Radiance rgbe.c 的标准编码）：
/// 以最大通道定共享指数，尾数截断到 8 位。
fn rgbe_from(r: f32, g: f32, b: f32) -> [u8; 4] {
    let max = r.max(g).max(b);
    if !(max > 0.0) {
        return [0, 0, 0, 0]; // 0 / NaN / 负值兜底为编码黑
    }
    let (frac, exp) = frexp(max);
    let scale = frac * 256.0 / max;
    // 截断；浮点舍入可能恰好触及 256.0，钳制防御
    let byte = |v: f32| (v * scale).min(255.0).max(0.0) as u8;
    [byte(r), byte(g), byte(b), (exp + 128) as u8]
}

/// v = frac × 2^exp（frac ∈ [0.5, 1)）。调用方保证 v 有限且 > 0。
fn frexp(v: f32) -> (f32, i32) {
    let bits = v.to_bits();
    let raw_exp = ((bits >> 23) & 0xff) as i32;
    if raw_exp == 0 {
        // f32 非规格化（|v| < 2^-126）：放大 2^32 后重取（环境图值域不会触达，仅防御）
        let (frac, exp) = frexp(v * 4_294_967_296.0);
        return (frac, exp - 32);
    }
    (f32::from_bits((bits & 0x807f_ffff) | (126 << 23)), raw_exp - 126)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 旧式平面 2×2：四像素四种典型编码（白、编码黑、任意值、全零指数）。
    #[test]
    fn parse_flat_scanlines() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 2 +X 2\n");
        // (1,1,1)：max=1 → scale=128 → [128,128,128,129]
        bytes.extend_from_slice(&[128, 128, 128, 129]);
        // (0,0,0)：指数 0 = 编码黑
        bytes.extend_from_slice(&[255, 255, 255, 0]);
        // (4,0.5,0)：max=4 → scale=32 → [128, 16, 0, 131]
        bytes.extend_from_slice(&[128, 16, 0, 131]);
        // 小值 (0.25, 0.25, 0.25)：max=0.25 → frexp=(0.5,-1) → scale=512 → [128,·,·,127]
        bytes.extend_from_slice(&[128, 128, 128, 127]);

        let image = parse_hdr(&bytes).expect("解析应成功");
        assert_eq!((image.width, image.height), (2, 2));
        let px = |x, y| image.pixel(x, y);
        for c in px(0, 0) {
            assert!((c - 1.0).abs() < 1e-5, "白像素应为 1.0，实际 {c}");
        }
        for c in px(1, 0) {
            assert_eq!(c, 0.0, "指数 0 应解码为全零");
        }
        assert!((px(0, 1)[0] - 4.0).abs() < 1e-5);
        assert!((px(0, 1)[1] - 0.5).abs() < 1e-5);
        assert_eq!(px(0, 1)[2], 0.0);
        for c in px(1, 1) {
            assert!((c - 0.25).abs() < 1e-5);
        }
    }

    /// 新式 RLE 4×2：每通道 [重复游程 + 字面量] 混合，并带文件尾换行。
    #[test]
    fn parse_rle_scanlines() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"#?RGBE\n\n-Y 2 +X 4\n");
        // 扫描线头 [2, 2, hi, lo]，宽 4 → [2,2,0,4]
        bytes.extend_from_slice(&[2, 2, 0, 4]);
        // R 通道：重复游程 [132, 128] → count 132 > 128 → 128 重复 132-128 = 4 次
        bytes.extend_from_slice(&[132, 128]);
        // G 通道：字面量 4 字节 [0,0,0,0]（count=4 ≤ 128 → 字面量）
        bytes.extend_from_slice(&[4, 0, 0, 0, 0]);
        // B 通道：重复游程 1 个 200 + 字面量 3 个 0
        bytes.extend_from_slice(&[129, 200, 3, 0, 0, 0]);
        // E 通道：字面量 [129, 129, 129, 129]
        bytes.extend_from_slice(&[4, 129, 129, 129, 129]);
        // 第二条扫描线：与第一条相同
        bytes.extend_from_slice(&[2, 2, 0, 4]);
        bytes.extend_from_slice(&[132, 128, 4, 0, 0, 0, 0, 129, 200, 3, 0, 0, 0, 4, 129, 129, 129, 129]);
        bytes.push(b'\n');

        let image = parse_hdr(&bytes).expect("解析应成功");
        assert_eq!((image.width, image.height), (4, 2));
        for y in 0..2u32 {
            for x in 0..4u32 {
                let [r, g, b] = image.pixel(x, y);
                assert!((r - 128.0 * 2f32.powi(129 - 136)).abs() < 1e-5, "R({x},{y})={r}");
                assert_eq!(g, 0.0);
                // B 通道混合编码：x=0 来自重复游程，x=1..4 来自字面量补零
                if x == 0 {
                    assert!((b - 200.0 * 2f32.powi(129 - 136)).abs() < 1e-5, "B({x},{y})={b}");
                } else {
                    assert_eq!(b, 0.0, "B({x},{y})={b}");
                }
            }
        }
    }

    /// 编码 → 解码往返：可精确表示的值必须无损恢复（1e-5 容差）。
    /// 注意 RGBE 尾数只有 8 位：小通道相对最大通道的比值低于 1/256 时
    /// 会被截断为 0，测试值需避开该量化边界。
    #[test]
    fn encode_decode_roundtrip() {
        let mut image = HdrImage::new(3, 1);
        image.set_pixel(0, 0, [1.0, 0.0, 0.0]);
        image.set_pixel(1, 0, [0.25, 0.5, 1.0]);
        image.set_pixel(2, 0, [40.0, 0.25, 0.0]);
        let parsed = parse_hdr(&encode_hdr(&image)).expect("往返解析应成功");
        assert_eq!((parsed.width, parsed.height), (3, 1));
        for x in 0..3u32 {
            let a = image.pixel(x, 0);
            let b = parsed.pixel(x, 0);
            for c in 0..3 {
                assert!((a[c] - b[c]).abs() < 1e-5, "通道 {c} 往返失真：{} vs {}", a[c], b[c]);
            }
        }
        // (1,0,0) 的 RGBE 字节应为 Radiance 标准编码 [128, 0, 0, 129]
        assert_eq!(rgbe_from(1.0, 0.0, 0.0), [128, 0, 0, 129]);
    }

    /// RGBE 指数数学：e == 0 全零；m × 2^(e-136) 与 m/256 × 2^(e-128) 等价。
    #[test]
    fn rgbe_exponent_math() {
        assert_eq!(rgbe_to_rgb(0, 0, 0, 0), [0.0; 3]);
        // (1,0,0) 编码 [128,0,0,129] → 128/256 × 2^1 = 1.0
        let [r, g, b] = rgbe_to_rgb(128, 0, 0, 129);
        assert!((r - 1.0).abs() < 1e-5 && g == 0.0 && b == 0.0);
        // 亮度 40（示例太阳）的编码字节：frexp(40)=(0.625,6) → scale=4 → m=160, e=134
        let [r, _, _] = rgbe_to_rgb(160, 0, 0, 134);
        assert!((r - 40.0).abs() < 1e-4, "40.0 往返失真：{r}");
    }

    #[test]
    fn bad_magic_is_format_error() {
        let bytes = b"PNM garbage\n\n-Y 1 +X 1\n\x00\x00\x00\x00" as &[u8];
        assert!(matches!(parse_hdr(bytes), Err(HdrError::Format(_))));
        assert!(parse_hdr(b"").is_err());
    }

    #[test]
    fn bad_resolution_line_is_unsupported() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"#?RADIANCE\n\n+X 4 -Y 2\n");
        assert!(matches!(parse_hdr(&bytes), Err(HdrError::Unsupported(_))));
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"#?RADIANCE\n\n-Y abc +X 4\n");
        assert!(matches!(parse_hdr(&bytes), Err(HdrError::Format(_))));
    }

    /// RLE 通道解码长度必须恰为 width：重复游程越界报 Format。
    #[test]
    fn rle_overshoot_is_format_error() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"#?RADIANCE\n\n-Y 1 +X 4\n");
        bytes.extend_from_slice(&[2, 2, 0, 4]);
        // R 通道：重复游程 5 次 128（宽只有 4）
        bytes.extend_from_slice(&[133, 128]);
        bytes.extend_from_slice(&[4, 0, 0, 0, 0]); // G
        bytes.extend_from_slice(&[4, 0, 0, 0, 0]); // B
        bytes.extend_from_slice(&[4, 129, 129, 129, 129]); // E
        assert!(matches!(parse_hdr(&bytes), Err(HdrError::Format(_))));
    }

    /// 尾部多余字节（宽高乘积与数据量不符）报 Format。
    #[test]
    fn trailing_garbage_is_format_error() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"#?RADIANCE\n\n-Y 1 +X 1\n");
        bytes.extend_from_slice(&[128, 0, 0, 129, 99]);
        assert!(matches!(parse_hdr(&bytes), Err(HdrError::Format(_))));
    }

    /// load_hdr 走临时文件（fixture 写入 std::env::temp_dir() 子目录）。
    #[test]
    fn load_hdr_roundtrip_via_file() {
        let mut image = HdrImage::new(2, 1);
        image.set_pixel(0, 0, [2.0, 1.0, 0.5]);
        image.set_pixel(1, 0, [0.0, 0.0, 0.0]);
        let dir = std::env::temp_dir().join("iris_hdr_tests");
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let path = dir.join("iris_hdr_unit.hdr");
        std::fs::write(&path, encode_hdr(&image)).expect("写入临时 .hdr");

        let loaded = load_hdr(&path).expect("加载临时 .hdr");
        let a = image.pixel(0, 0);
        let b = loaded.pixel(0, 0);
        for c in 0..3 {
            assert!((a[c] - b[c]).abs() < 1e-5);
        }
        assert_eq!(loaded.pixel(1, 0), [0.0; 3]);
        let _ = std::fs::remove_file(&path);
    }

    /// HdrError 实现 Display + Error（风格与 GltfError 一致）。
    #[test]
    fn error_display() {
        let e = HdrError::Format("测试".into());
        assert!(e.to_string().contains("测试"));
        let _: &dyn std::error::Error = &e;
    }
}
