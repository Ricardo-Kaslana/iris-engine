//! 2D UI 叠加层（HUD）：屏幕空间彩色矩形 + 内嵌位图字体文本。
//!
//! - **像素坐标系**：原点左上、y 向下（与窗口行序一致），由 `ui.wgsl`
//!   的顶点着色器统一翻转到 NDC；
//! - 矩形与文本共用**单一管线、单一字形图集纹理、单一 draw call**；
//! - HUD pass 位于色调映射 blit 之后，直接写 sRGB 输出目标——因此
//!   **不参与 HDR / 泛光 / 色调映射**（HUD 应是干净的界面叠加，
//!   不应被后期处理提亮或染雾）；
//! - v1 无层级、无锚点、无剪裁：组件挂在任意实体上，位置即屏幕像素
//!   （文本的排版原点复用同实体 `UiRect.position`，见 [`UiText`]）。

use std::sync::atomic::{AtomicBool, Ordering};

use iris_core::Component;
use iris_core::prelude::Entity;

use crate::rendertarget::RenderTargetHandle;
use crate::texture::TextureHandle;

/// UI 矩形：屏幕像素坐标（原点左上、y 向下）。
///
/// 挂在任意实体上，与 [`UiColor`] 配对渲染；无层级 / 无锚点（v1）。
/// **挂有 [`UiText`] 的实体上，本组件退化为文本的定位载体（不绘制
/// 矩形）**；需要面板背景时把矩形放在单独的实体上。
#[derive(Component, Clone, Debug)]
pub struct UiRect {
    /// 左上角位置（像素）。
    pub position: [f32; 2],
    /// 宽高（像素）。
    pub size: [f32; 2],
}

/// UI 文本：内嵌位图字体渲染。
///
/// 字形基准 8×8 像素，`scale` 为放大倍数（等宽步进与行高都是 8×scale）。
/// 支持 `\n` 换行。**排版原点复用同实体 `UiRect.position`**（v1 不引入
/// 独立锚点组件；该 `UiRect` 仅作定位载体、不绘制矩形，无则画在 (0,0)）。
#[derive(Component, Clone, Debug)]
pub struct UiText {
    /// 文本内容（仅 ASCII；非 ASCII 字符跳过并告警一次）。
    pub string: String,
    /// 字形放大倍数（1 = 原始 8×8 位图像素）。
    pub scale: f32,
}

/// UI 颜色：线性 RGBA（0..1）。矩形与文本共用，缺省白。
#[derive(Component, Clone, Copy, Debug)]
pub struct UiColor(pub [f32; 4]);

impl Default for UiColor {
    fn default() -> Self {
        Self([1.0, 1.0, 1.0, 1.0])
    }
}

/// UI 图片：把一张普通纹理（[`crate::texture::TextureHandle`]）以
/// [`UiRect`]（position / size，像素）定位绘制到屏幕。
///
/// 与 [`UiText`] 同款：**定位复用同实体 `UiRect`**（缺省画在 (0,0)，
/// 尺寸 0×0），`UiColor` 乘色（缺省白 = 原样显示）。绘制顺序 = 图片之下、
/// 文本之上：图片先于 UI 文本绘制（小地图底图应垫在 HUD 文字下方，
/// 「后画者在上」由收集顺序统一裁决，见模块头）。UV 恒为全屏 0..1
/// 直角采样（无抗锯齿 v1——小地图 / 像素化内容天然契合，Nearest 采样
/// 由渲染器统一使用）。图片纹理同样可能走 BC1 上传（照
/// [`crate::texture_bcn`] 决策树，见 `renderer.rs::ensure_texture`）。
#[derive(Component, Clone, Debug)]
pub struct UiImage {
    /// 要显示的普通纹理。
    pub texture: crate::texture::TextureHandle,
}

/// UI 图片（渲染目标版）：把 [`crate::rendertarget::RenderTargetHandle`]
/// 对应 RT 的颜色纹理以 [`UiRect`] 定位绘制到屏幕——小地图 / 监控屏的直接
/// 呈现。
///
/// 定位 / 乘色 / 顺序语义与 [`UiImage`] 完全一致（图片之下、文本之上）。
/// RT 的 GPU 颜色纹理由渲染器跨帧持有（`renderer.rs::GpuRenderTarget`），
/// UI pass 直接采样该纹理（Rgba8UnormSrgb）。
#[derive(Component, Clone, Debug)]
pub struct UiImageRtt {
    /// 要显示的渲染目标。
    pub target: crate::rendertarget::RenderTargetHandle,
}

/// 公有领域 font8x8 位图字体（Daniel Hepper 整理 / Marcel Sondaar 起源，
/// License: Public Domain），按标准表 `font8x8_basic` 誊录 ASCII 32..=127
/// 共 96 个字形。每字形 8 字节 = 自上而下 8 行的行位码，行内
/// **LSB = 最左像素**。形状由模块内单测守护（参考行比对 + 对称性）。
pub const FONT8X8_ASCII: [[u8; 8]; 96] = [
    // U+0020 ' '
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    // U+0021 '!'
    [0x18, 0x3C, 0x3C, 0x18, 0x18, 0x00, 0x18, 0x00],
    // U+0022 '"'
    [0x36, 0x36, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    // U+0023 '#'
    [0x36, 0x36, 0x7F, 0x36, 0x7F, 0x36, 0x36, 0x00],
    // U+0024 '$'
    [0x0C, 0x3E, 0x03, 0x1E, 0x30, 0x1F, 0x0C, 0x00],
    // U+0025 '%'
    [0x00, 0x63, 0x33, 0x18, 0x0C, 0x66, 0x63, 0x00],
    // U+0026 '&'
    [0x1C, 0x36, 0x1C, 0x6E, 0x3B, 0x33, 0x6E, 0x00],
    // U+0027 '\''
    [0x06, 0x06, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00],
    // U+0028 '('
    [0x18, 0x0C, 0x06, 0x06, 0x06, 0x0C, 0x18, 0x00],
    // U+0029 ')'
    [0x06, 0x0C, 0x18, 0x18, 0x18, 0x0C, 0x06, 0x00],
    // U+002A '*'
    [0x00, 0x66, 0x3C, 0xFF, 0x3C, 0x66, 0x00, 0x00],
    // U+002B '+'
    [0x00, 0x0C, 0x0C, 0x3F, 0x0C, 0x0C, 0x00, 0x00],
    // U+002C ','
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C, 0x06],
    // U+002D '-'
    [0x00, 0x00, 0x00, 0x3F, 0x00, 0x00, 0x00, 0x00],
    // U+002E '.'
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C, 0x00],
    // U+002F '/'
    [0x60, 0x30, 0x18, 0x0C, 0x06, 0x03, 0x01, 0x00],
    // U+0030 '0'
    [0x3E, 0x63, 0x73, 0x7B, 0x6F, 0x67, 0x3E, 0x00],
    // U+0031 '1'
    [0x0C, 0x0E, 0x0C, 0x0C, 0x0C, 0x0C, 0x3F, 0x00],
    // U+0032 '2'
    [0x1E, 0x33, 0x30, 0x1C, 0x06, 0x33, 0x3F, 0x00],
    // U+0033 '3'
    [0x1E, 0x33, 0x30, 0x1C, 0x30, 0x33, 0x1E, 0x00],
    // U+0034 '4'
    [0x38, 0x3C, 0x36, 0x33, 0x7F, 0x30, 0x78, 0x00],
    // U+0035 '5'
    [0x3F, 0x03, 0x1F, 0x30, 0x30, 0x33, 0x1E, 0x00],
    // U+0036 '6'
    [0x1C, 0x06, 0x03, 0x1F, 0x33, 0x33, 0x1E, 0x00],
    // U+0037 '7'
    [0x3F, 0x33, 0x30, 0x18, 0x0C, 0x0C, 0x0C, 0x00],
    // U+0038 '8'
    [0x1E, 0x33, 0x33, 0x1E, 0x33, 0x33, 0x1E, 0x00],
    // U+0039 '9'
    [0x1E, 0x33, 0x33, 0x3E, 0x30, 0x18, 0x0E, 0x00],
    // U+003A ':'
    [0x00, 0x0C, 0x0C, 0x00, 0x00, 0x0C, 0x0C, 0x00],
    // U+003B ';'
    [0x00, 0x0C, 0x0C, 0x00, 0x00, 0x0C, 0x0C, 0x06],
    // U+003C '<'
    [0x18, 0x0C, 0x06, 0x03, 0x06, 0x0C, 0x18, 0x00],
    // U+003D '='
    [0x00, 0x00, 0x3F, 0x00, 0x00, 0x3F, 0x00, 0x00],
    // U+003E '>'
    [0x06, 0x0C, 0x18, 0x30, 0x18, 0x0C, 0x06, 0x00],
    // U+003F '?'
    [0x1E, 0x33, 0x30, 0x18, 0x0C, 0x00, 0x0C, 0x00],
    // U+0040 '@'
    [0x3E, 0x63, 0x7B, 0x7B, 0x7B, 0x03, 0x1E, 0x00],
    // U+0041 'A'
    [0x0C, 0x1E, 0x33, 0x33, 0x3F, 0x33, 0x33, 0x00],
    // U+0042 'B'
    [0x3F, 0x66, 0x66, 0x3E, 0x66, 0x66, 0x3F, 0x00],
    // U+0043 'C'
    [0x3C, 0x66, 0x03, 0x03, 0x03, 0x66, 0x3C, 0x00],
    // U+0044 'D'
    [0x1F, 0x36, 0x66, 0x66, 0x66, 0x36, 0x1F, 0x00],
    // U+0045 'E'
    [0x7F, 0x46, 0x16, 0x1E, 0x16, 0x46, 0x7F, 0x00],
    // U+0046 'F'
    [0x7F, 0x46, 0x16, 0x1E, 0x16, 0x06, 0x0F, 0x00],
    // U+0047 'G'
    [0x3C, 0x66, 0x03, 0x03, 0x73, 0x66, 0x7C, 0x00],
    // U+0048 'H'
    [0x33, 0x33, 0x33, 0x3F, 0x33, 0x33, 0x33, 0x00],
    // U+0049 'I'
    [0x1E, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00],
    // U+004A 'J'
    [0x78, 0x30, 0x30, 0x30, 0x33, 0x33, 0x1E, 0x00],
    // U+004B 'K'
    [0x67, 0x66, 0x36, 0x1E, 0x36, 0x66, 0x67, 0x00],
    // U+004C 'L'
    [0x0F, 0x06, 0x06, 0x06, 0x46, 0x66, 0x7F, 0x00],
    // U+004D 'M'
    [0x63, 0x77, 0x7F, 0x7F, 0x6B, 0x63, 0x63, 0x00],
    // U+004E 'N'
    [0x63, 0x67, 0x6F, 0x7B, 0x73, 0x63, 0x63, 0x00],
    // U+004F 'O'
    [0x1C, 0x36, 0x63, 0x63, 0x63, 0x36, 0x1C, 0x00],
    // U+0050 'P'
    [0x3F, 0x66, 0x66, 0x3E, 0x06, 0x06, 0x0F, 0x00],
    // U+0051 'Q'
    [0x1E, 0x33, 0x33, 0x33, 0x3B, 0x1E, 0x38, 0x00],
    // U+0052 'R'
    [0x3F, 0x66, 0x66, 0x3E, 0x36, 0x66, 0x67, 0x00],
    // U+0053 'S'
    [0x1E, 0x33, 0x07, 0x0E, 0x38, 0x33, 0x1E, 0x00],
    // U+0054 'T'
    [0x3F, 0x2D, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00],
    // U+0055 'U'
    [0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x3F, 0x00],
    // U+0056 'V'
    [0x33, 0x33, 0x33, 0x33, 0x33, 0x1E, 0x0C, 0x00],
    // U+0057 'W'
    [0x63, 0x63, 0x63, 0x6B, 0x7F, 0x77, 0x63, 0x00],
    // U+0058 'X'
    [0x63, 0x63, 0x36, 0x1C, 0x1C, 0x36, 0x63, 0x00],
    // U+0059 'Y'
    [0x33, 0x33, 0x33, 0x1E, 0x0C, 0x0C, 0x1E, 0x00],
    // U+005A 'Z'
    [0x7F, 0x63, 0x31, 0x18, 0x4C, 0x66, 0x7F, 0x00],
    // U+005B '['
    [0x1E, 0x06, 0x06, 0x06, 0x06, 0x06, 0x1E, 0x00],
    // U+005C '\'
    [0x03, 0x06, 0x0C, 0x18, 0x30, 0x60, 0x40, 0x00],
    // U+005D ']'
    [0x1E, 0x18, 0x18, 0x18, 0x18, 0x18, 0x1E, 0x00],
    // U+005E '^'
    [0x08, 0x1C, 0x36, 0x63, 0x00, 0x00, 0x00, 0x00],
    // U+005F '_'
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF],
    // U+0060 '`'
    [0x0C, 0x0C, 0x18, 0x00, 0x00, 0x00, 0x00, 0x00],
    // U+0061 'a'
    [0x00, 0x00, 0x1E, 0x30, 0x3E, 0x33, 0x6E, 0x00],
    // U+0062 'b'
    [0x07, 0x06, 0x06, 0x3E, 0x66, 0x66, 0x3B, 0x00],
    // U+0063 'c'
    [0x00, 0x00, 0x1E, 0x33, 0x03, 0x33, 0x1E, 0x00],
    // U+0064 'd'
    [0x38, 0x30, 0x30, 0x3E, 0x33, 0x33, 0x6E, 0x00],
    // U+0065 'e'
    [0x00, 0x00, 0x1E, 0x33, 0x3F, 0x03, 0x1E, 0x00],
    // U+0066 'f'
    [0x1C, 0x36, 0x06, 0x0F, 0x06, 0x06, 0x0F, 0x00],
    // U+0067 'g'
    [0x00, 0x00, 0x6E, 0x33, 0x33, 0x3E, 0x30, 0x1F],
    // U+0068 'h'
    [0x07, 0x06, 0x36, 0x6E, 0x66, 0x66, 0x67, 0x00],
    // U+0069 'i'
    [0x0C, 0x00, 0x0E, 0x0C, 0x0C, 0x0C, 0x1E, 0x00],
    // U+006A 'j'
    [0x30, 0x00, 0x30, 0x30, 0x30, 0x33, 0x33, 0x1E],
    // U+006B 'k'
    [0x07, 0x06, 0x66, 0x36, 0x1E, 0x36, 0x67, 0x00],
    // U+006C 'l'
    [0x0E, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00],
    // U+006D 'm'
    [0x00, 0x00, 0x33, 0x7F, 0x7F, 0x6B, 0x63, 0x00],
    // U+006E 'n'
    [0x00, 0x00, 0x1F, 0x33, 0x33, 0x33, 0x33, 0x00],
    // U+006F 'o'
    [0x00, 0x00, 0x1E, 0x33, 0x33, 0x33, 0x1E, 0x00],
    // U+0070 'p'
    [0x00, 0x00, 0x3B, 0x66, 0x66, 0x3E, 0x06, 0x0F],
    // U+0071 'q'
    [0x00, 0x00, 0x6E, 0x33, 0x33, 0x3E, 0x30, 0x78],
    // U+0072 'r'
    [0x00, 0x00, 0x3B, 0x6E, 0x66, 0x06, 0x0F, 0x00],
    // U+0073 's'
    [0x00, 0x00, 0x3E, 0x03, 0x1E, 0x30, 0x1F, 0x00],
    // U+0074 't'
    [0x08, 0x0C, 0x3E, 0x0C, 0x0C, 0x2C, 0x18, 0x00],
    // U+0075 'u'
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x33, 0x6E, 0x00],
    // U+0076 'v'
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x1E, 0x0C, 0x00],
    // U+0077 'w'
    [0x00, 0x00, 0x63, 0x6B, 0x7F, 0x7F, 0x36, 0x00],
    // U+0078 'x'
    [0x00, 0x00, 0x63, 0x36, 0x1C, 0x36, 0x63, 0x00],
    // U+0079 'y'
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x3E, 0x30, 0x1F],
    // U+007A 'z'
    [0x00, 0x00, 0x3F, 0x19, 0x0C, 0x26, 0x3F, 0x00],
    // U+007B '{'
    [0x38, 0x0C, 0x0C, 0x07, 0x0C, 0x0C, 0x38, 0x00],
    // U+007C '|'
    [0x18, 0x18, 0x18, 0x00, 0x18, 0x18, 0x18, 0x00],
    // U+007D '}'
    [0x07, 0x0C, 0x0C, 0x38, 0x0C, 0x0C, 0x07, 0x00],
    // U+007E '~'
    [0x6E, 0x3B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    // U+007F DEL
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
];

/// 查询 ASCII 字形的行位码（每字节一行，LSB = 最左像素）。
/// ASCII 32..=127 之外的字符返回 `None`（v1 非 ASCII 字符跳过并告警一次）。
pub fn glyph_bits(ch: u8) -> Option<&'static [u8; 8]> {
    if (32..=127).contains(&ch) {
        Some(&FONT8X8_ASCII[(ch - 32) as usize])
    } else {
        None
    }
}

/// 字形/白块的边长（像素）。
pub const GLYPH_SIZE: u32 = 8;
/// 图集列数。
pub const ATLAS_COLS: u32 = 16;
/// 图集行数：1 块纯白（矩形复用）+ 96 个字形 = 97 块。
pub const ATLAS_ROWS: u32 = 7;
/// 图集宽度（像素）。
pub const ATLAS_WIDTH: u32 = ATLAS_COLS * GLYPH_SIZE;
/// 图集高度（像素）。
pub const ATLAS_HEIGHT: u32 = ATLAS_ROWS * GLYPH_SIZE;

/// 纯白块的单元格序号（矩形与所有 UI 复用，单管线单纹理单 draw）。
pub const WHITE_CELL: u32 = 0;

/// ASCII 字符在图集中的单元格序号（字形紧随白块之后依次排布）。
pub fn glyph_cell(ch: u8) -> u32 {
    WHITE_CELL + 1 + (ch - 32) as u32
}

/// 单元格的 UV 区块（含半纹素内缩防图集渗色），返回 `(uv_min, uv_max)`。
/// 采样器为 Nearest，内缩进一步弱化浮点取整导致的邻块渗色。
fn cell_uv(cell: u32) -> ([f32; 2], [f32; 2]) {
    let col = cell % ATLAS_COLS;
    let row = cell / ATLAS_COLS;
    let x = (col * GLYPH_SIZE) as f32;
    let y = (row * GLYPH_SIZE) as f32;
    let w = ATLAS_WIDTH as f32;
    let h = ATLAS_HEIGHT as f32;
    (
        [(x + 0.5) / w, (y + 0.5) / h],
        [
            (x + GLYPH_SIZE as f32 - 0.5) / w,
            (y + GLYPH_SIZE as f32 - 0.5) / h,
        ],
    )
}

/// 字形某行位 texel 中心的 UV。文本按亮位逐位展开成 scale×scale 的
/// 实心小 quad，四角共用该 UV（恒定采样 = 最强的「半纹素内缩」，
/// 恒不会触邻块；位图逐位缩放因此逐像素精确）。
fn bit_uv(ch: u8, col: u32, row: u32) -> [f32; 2] {
    let cell = glyph_cell(ch);
    let x = (cell % ATLAS_COLS * GLYPH_SIZE + col) as f32;
    let y = (cell / ATLAS_COLS * GLYPH_SIZE + row) as f32;
    [
        (x + 0.5) / ATLAS_WIDTH as f32,
        (y + 0.5) / ATLAS_HEIGHT as f32,
    ]
}

/// 构建图集 CPU 像素（RGBA8，行主序）：0 号格纯白不透明（矩形复用），
/// 字形格白色 + alpha = 行位码（位点亮 → 不透明）。
pub(crate) fn build_atlas_rgba() -> Vec<u8> {
    let mut rgba = vec![0u8; (ATLAS_WIDTH * ATLAS_HEIGHT * 4) as usize];
    let mut put = |x: u32, y: u32, pixel: [u8; 4]| {
        let i = ((y * ATLAS_WIDTH + x) * 4) as usize;
        rgba[i..i + 4].copy_from_slice(&pixel);
    };
    for y in 0..GLYPH_SIZE {
        for x in 0..GLYPH_SIZE {
            put(x, y, [255, 255, 255, 255]);
        }
    }
    for (glyph, rows) in FONT8X8_ASCII.iter().enumerate() {
        let cell = WHITE_CELL + 1 + glyph as u32;
        let base_x = (cell % ATLAS_COLS) * GLYPH_SIZE;
        let base_y = (cell / ATLAS_COLS) * GLYPH_SIZE;
        for (row, bits) in rows.iter().enumerate() {
            for col in 0..GLYPH_SIZE {
                let lit = (bits >> col) & 1 == 1;
                put(
                    base_x + col,
                    base_y + row as u32,
                    [255, 255, 255, if lit { 255 } else { 0 }],
                );
            }
        }
    }
    rgba
}

/// UI 顶点：像素位置 + 图集 UV + 线性 RGBA 顶点色（32 字节，POD）。
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct UiVertex {
    /// 屏幕像素坐标（原点左上、y 向下）。
    pub pos: [f32; 2],
    /// 图集 UV。
    pub uv: [f32; 2],
    /// 线性 RGBA 顶点色。
    pub color: [f32; 4],
}

impl UiVertex {
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<UiVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[
            wgpu::VertexAttribute {
                offset: 0,
                shader_location: 0,
                format: wgpu::VertexFormat::Float32x2,
            },
            wgpu::VertexAttribute {
                offset: 8,
                shader_location: 1,
                format: wgpu::VertexFormat::Float32x2,
            },
            wgpu::VertexAttribute {
                offset: 16,
                shader_location: 2,
                format: wgpu::VertexFormat::Float32x4,
            },
        ],
    };
}

/// 追加一个轴对齐 quad（两三角形、6 顶点）：`min`/`max` 为像素对角，
/// `uv_min`/`uv_max` 为对应 UV（恒定采样时两者相等）。
fn push_quad(
    out: &mut Vec<UiVertex>,
    min: [f32; 2],
    max: [f32; 2],
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    color: [f32; 4],
) {
    let vertex = |pos: [f32; 2], uv: [f32; 2]| UiVertex { pos, uv, color };
    let tl = vertex(min, uv_min);
    let tr = vertex([max[0], min[1]], [uv_max[0], uv_min[1]]);
    let br = vertex(max, uv_max);
    let bl = vertex([min[0], max[1]], [uv_min[0], uv_max[1]]);
    out.extend_from_slice(&[tl, tr, br, tl, br, bl]);
}

/// 矩形展开为 6 顶点：UV 取纯白块区块，颜色即顶点色。
pub fn push_rect_vertices(out: &mut Vec<UiVertex>, rect: &UiRect, color: UiColor) {
    let (uv_min, uv_max) = cell_uv(WHITE_CELL);
    let min = rect.position;
    let max = [
        rect.position[0] + rect.size[0],
        rect.position[1] + rect.size[1],
    ];
    push_quad(out, min, max, uv_min, uv_max, color.0);
}

/// 文本展开为顶点：逐字形扫描行位码，每个**亮位**生成一个
/// scale×scale 像素的实心小 quad（UV 恒定采样该位 texel 中心）。
/// 空格 / 未知字形不产生顶点但**仍前进步进**（等宽 8×scale）；
/// `\n` 行高 8×scale。非 ASCII 字符跳过并告警一次。
pub fn push_text_vertices(
    out: &mut Vec<UiVertex>,
    text: &UiText,
    origin: [f32; 2],
    color: UiColor,
) {
    static NON_ASCII_WARNED: AtomicBool = AtomicBool::new(false);
    let step = GLYPH_SIZE as f32 * text.scale;
    if step <= 0.0 || step.is_nan() {
        return; // scale <= 0 / NaN：无可绘制内容
    }
    let [mut pen_x, mut pen_y] = origin;
    for ch in text.string.chars() {
        if ch == '\n' {
            pen_x = origin[0];
            pen_y += step;
            continue;
        }
        let bits = if ch.is_ascii() {
            glyph_bits(ch as u8)
        } else {
            if !NON_ASCII_WARNED.swap(true, Ordering::Relaxed) {
                log::warn!(
                    "UI 文本包含非 ASCII 字符 U+{:04X}，v1 位图字体仅支持 ASCII，已跳过",
                    ch as u32
                );
            }
            None
        };
        if let Some(rows) = bits {
            let ch = ch as u8;
            for (row, line_bits) in rows.iter().enumerate() {
                for col in 0..GLYPH_SIZE {
                    if (line_bits >> col) & 1 == 1 {
                        let px = pen_x + col as f32 * text.scale;
                        let py = pen_y + row as f32 * text.scale;
                        let uv = bit_uv(ch, col, row as u32);
                        push_quad(
                            out,
                            [px, py],
                            [px + text.scale, py + text.scale],
                            uv,
                            uv,
                            color.0,
                        );
                    }
                }
            }
        }
        pen_x += step;
    }
}

/// 测量文本宽度（像素）：8 × scale × 最长行的字符数（`\n` 分行取最大）。
///
/// 与 [`push_text_vertices`] 的等宽步进同源——空格与非 ASCII 字符
/// （跳过绘制但前进步进）同样计入宽度，因此测量结果与实际墨迹包围盒
/// 对单行纯 ASCII 精确一致、对含不可绘制字符的行是保守上界。
/// 供右对齐 / 居中等手工排版使用（HUD 布局不必再手算 8 × scale × 字符数）。
pub fn text_width(text: &UiText) -> f32 {
    let step = GLYPH_SIZE as f32 * text.scale;
    text.string
        .split('\n')
        .map(|line| line.chars().count() as f32 * step)
        .fold(0.0, f32::max)
}

/// 从 World 只读收集全部 UI 顶点（渲染器每帧调用）。
/// 矩形先收集、文本后收集：alpha 混合下文本叠于矩形之上（血条底/前景
/// 这类多层矩形由实体生成顺序决定层叠）。
/// [`crate::visibility::Visibility`] 过滤：挂 `HIDDEN` 的实体（无论矩形
/// 还是文本）不产生顶点；组件缺席视为可见（默认行为零变化）。
pub(crate) fn collect_ui_vertices(world: &iris_core::world::World, out: &mut Vec<UiVertex>) {
    use crate::visibility::Visibility;
    world.query::<(Entity, &UiRect, &UiColor, Option<&Visibility>)>(
        |(entity, rect, color, visibility)| {
            // 挂有 UiText / UiImage / UiImageRtt 的实体上，UiRect 仅充当定位
            // / 尺寸载体（文本排版原点、图片像素盒），不绘制矩形——否则缺省
            // 白矩形会盖住文字 / 图片；需要面板背景时单独生成一个矩形实体。
            if visibility.is_some_and(|v| !v.visible) {
                return;
            }
            if world.get::<UiText>(entity).is_none()
                && world.get::<UiImage>(entity).is_none()
                && world.get::<UiImageRtt>(entity).is_none()
            {
                push_rect_vertices(out, rect, *color);
            }
        },
    );
    world.query::<(Entity, &UiText, &UiColor, Option<&Visibility>)>(
        |(entity, text, color, visibility)| {
            if visibility.is_some_and(|v| !v.visible) {
                return;
            }
            let origin = world
                .get::<UiRect>(entity)
                .map(|rect| rect.position)
                .unwrap_or([0.0, 0.0]);
            push_text_vertices(out, text, origin, *color);
        },
    );
}

/// 图片 quad 的 6 顶点：UV 全屏 0..1（把整张图片纹理映射到
/// [`UiRect`] 像素盒），颜色 = 乘色。
pub fn push_image_vertices(out: &mut Vec<UiVertex>, rect: &UiRect, color: UiColor) {
    let min = rect.position;
    let max = [
        rect.position[0] + rect.size[0],
        rect.position[1] + rect.size[1],
    ];
    push_quad(out, min, max, [0.0, 0.0], [1.0, 1.0], color.0);
}

/// 渲染器收集到的单张 UI 图片：绘制源（普通纹理 / 渲染目标）+ 固定
/// 6 顶点（图片恒为单位 quad，见 [`push_image_vertices`]）。
pub(crate) struct UiImageItem {
    /// 绘制源的纹理（普通纹理）——`None` 则 `rtt` 出参。
    pub texture: Option<TextureHandle>,
    /// 绘制源的渲染目标——`None` 则 `texture` 出参。
    pub rtt: Option<RenderTargetHandle>,
    /// 该图片的 6 顶点（独立于 atlas 顶点缓冲，避免混批绑定冲突）。
    pub vertices: [UiVertex; 6],
}

/// 从 World 只读收集全部 UI 图片顶点（渲染器每帧调用；图片在下、文本在
/// 上的顺序由「先收集图片、再收集矩形/文本」的调用序保证——收集顺序即
/// 绘制顺序，后画者在上）。
///
/// [`crate::visibility::Visibility`] 过滤：挂 `HIDDEN` 的实体不产生顶点；
/// 组件缺席视为可见（默认行为零变化）。定位复用同实体 [`UiRect`]（缺省
/// 画在 (0,0)），乘色 = [`UiColor`]（缺省白）。
pub(crate) fn collect_ui_image_vertices(
    world: &iris_core::world::World,
    out: &mut Vec<UiImageItem>,
) {
    use crate::visibility::Visibility;
    world.query::<(Entity, &UiImage, &UiRect, &UiColor, Option<&Visibility>)>(
        |(entity, image, rect, color, visibility)| {
            if visibility.is_some_and(|v| !v.visible) {
                return;
            }
            let _ = entity;
            let mut tmp = Vec::with_capacity(6);
            push_image_vertices(&mut tmp, rect, *color);
            out.push(UiImageItem {
                texture: Some(image.texture),
                rtt: None,
                vertices: tmp.try_into().expect("图片 quad 恒 6 顶点"),
            });
        },
    );
    world.query::<(Entity, &UiImageRtt, &UiRect, &UiColor, Option<&Visibility>)>(
        |(entity, image, rect, color, visibility)| {
            if visibility.is_some_and(|v| !v.visible) {
                return;
            }
            let _ = entity;
            let mut tmp = Vec::with_capacity(6);
            push_image_vertices(&mut tmp, rect, *color);
            out.push(UiImageItem {
                texture: None,
                rtt: Some(image.target),
                vertices: tmp.try_into().expect("图片 quad 恒 6 顶点"),
            });
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn popcount(rows: &[u8; 8]) -> u32 {
        rows.iter().map(|b| b.count_ones()).sum()
    }

    fn col_sums(rows: &[u8; 8]) -> [u32; 8] {
        let mut sums = [0u32; 8];
        for (col, sum) in sums.iter_mut().enumerate() {
            for row in rows {
                *sum += ((row >> col) & 1) as u32;
            }
        }
        sums
    }

    #[test]
    fn rect_is_position_carrier_for_text_entities() {
        // 文本实体上的 UiRect 只作定位载体（不绘制矩形），
        // 纯矩形实体照常绘制
        let mut world = iris_core::world::World::new();
        world.spawn((
            UiRect {
                position: [0.0, 0.0],
                size: [10.0, 10.0],
            },
            UiColor::default(),
        ));
        world.spawn((
            UiRect {
                position: [20.0, 0.0],
                size: [100.0, 100.0],
            },
            UiText {
                string: "A".into(),
                scale: 1.0,
            },
            UiColor::default(),
        ));
        let mut out = Vec::new();
        collect_ui_vertices(&world, &mut out);
        let text_verts = popcount(glyph_bits(b'A').unwrap()) as usize * 6;
        assert_eq!(
            out.len(),
            6 + text_verts,
            "应只有 1 个矩形（6 顶点）+ 文本顶点；文本实体的 UiRect 不应绘制"
        );
        // 定位生效：文本顶点从 x=20 起
        let text_min_x = out[6..].iter().map(|v| v.pos[0]).fold(f32::MAX, f32::min);
        assert_eq!(text_min_x, 20.0);
    }

    #[test]
    fn font_table_covers_printable_ascii_only() {
        for ch in 32u8..=127 {
            assert!(glyph_bits(ch).is_some(), "ASCII {ch} 应可查");
        }
        for ch in [0u8, 31, 128, 255] {
            assert!(glyph_bits(ch).is_none(), "非可打印 ASCII {ch} 应返回 None");
        }
    }

    #[test]
    fn space_and_del_are_blank() {
        for ch in [32u8, 127] {
            let bits = glyph_bits(ch).unwrap();
            assert!(bits.iter().all(|&b| b == 0), "字形 {ch} 应全零");
        }
    }

    #[test]
    fn reference_shapes_guard_transcription() {
        // 与公有领域 font8x8 参考表逐字节比对（誊录守护）
        assert_eq!(
            glyph_bits(b'A').unwrap(),
            &[0x0C, 0x1E, 0x33, 0x33, 0x3F, 0x33, 0x33, 0x00]
        );
        assert_eq!(
            glyph_bits(b'0').unwrap(),
            &[0x3E, 0x63, 0x73, 0x7B, 0x6F, 0x67, 0x3E, 0x00]
        );
        let lit = popcount(glyph_bits(b'A').unwrap());
        assert!(
            (8..=40).contains(&lit),
            "'A' 亮像素数应在 8..=40，实际 {lit}"
        );
    }

    #[test]
    fn capital_a_mirror_symmetric_within_ink_bbox() {
        // 列和在墨迹包围盒内必须镜像对称。比「第 0/7 列相等」更强：
        // font8x8 字形在 8px 格内左对齐，最右列恒空——直接比较 0/7 列
        // 对真实参考数据恒假，对称守护须以墨迹边界为轴
        let sums = col_sums(glyph_bits(b'A').unwrap());
        let first = (0..8).find(|&c| sums[c] > 0).unwrap();
        let last = (0..8).rev().find(|&c| sums[c] > 0).unwrap();
        assert_eq!(first, 0, "'A' 墨迹应顶格从第 0 列开始");
        let mut c = first;
        while c < last - c {
            assert_eq!(sums[c], sums[last - c], "'A' 列 {c} 与镜像列应相等");
            c += 1;
        }
    }

    #[test]
    fn i_and_l_have_ink() {
        assert!(popcount(glyph_bits(b'I').unwrap()) >= 1);
        assert!(popcount(glyph_bits(b'l').unwrap()) >= 1);
    }

    #[test]
    fn atlas_pixels_match_bit_codes() {
        let rgba = build_atlas_rgba();
        assert_eq!(rgba.len(), (ATLAS_WIDTH * ATLAS_HEIGHT * 4) as usize);
        // 0 号格 = 纯白不透明块（矩形复用）
        for y in 0..GLYPH_SIZE {
            for x in 0..GLYPH_SIZE {
                let i = ((y * ATLAS_WIDTH + x) * 4) as usize;
                assert_eq!(&rgba[i..i + 4], &[255, 255, 255, 255], "白块 ({x},{y})");
            }
        }
        // 'A' 单元格：RGB 恒白，alpha 逐位等于行位码
        let cell = glyph_cell(b'A');
        let base_x = (cell % ATLAS_COLS * GLYPH_SIZE) as usize;
        let base_y = (cell / ATLAS_COLS * GLYPH_SIZE) as usize;
        for (row, bits) in glyph_bits(b'A').unwrap().iter().enumerate() {
            for col in 0..8usize {
                let i = ((base_y + row) * ATLAS_WIDTH as usize + base_x + col) * 4;
                assert_eq!(rgba[i], 255, "字形 texel 应为白色 ({row},{col})");
                assert_eq!(
                    rgba[i + 3],
                    if (bits >> col) & 1 == 1 { 255 } else { 0 },
                    "alpha 应等于行位码 ({row},{col})"
                );
            }
        }
    }

    #[test]
    fn rect_expands_to_six_vertices() {
        let mut out = Vec::new();
        push_rect_vertices(
            &mut out,
            &UiRect {
                position: [10.0, 20.0],
                size: [30.0, 40.0],
            },
            UiColor([1.0, 0.0, 0.0, 1.0]),
        );
        assert_eq!(out.len(), 6, "矩形 = 2 三角形 6 顶点");
        let max_x = out.iter().map(|v| v.pos[0]).fold(f32::MIN, f32::max);
        let max_y = out.iter().map(|v| v.pos[1]).fold(f32::MIN, f32::max);
        assert_eq!((max_x, max_y), (40.0, 60.0), "右下角 = position + size");
        assert!(out.iter().all(|v| v.color == [1.0, 0.0, 0.0, 1.0]));
        // 全部顶点采样白块（恒白 alpha=1）
        assert!(out.iter().all(|v| v.uv[0] <= 1.0 && v.uv[1] <= 1.0));
    }

    #[test]
    fn text_expands_lit_bits_to_quads() {
        // "A" scale 1：每个亮位一个 1×1 像素 quad，顶点数 = popcount × 6
        let mut out = Vec::new();
        push_text_vertices(
            &mut out,
            &UiText {
                string: "A".into(),
                scale: 1.0,
            },
            [0.0, 0.0],
            UiColor::default(),
        );
        assert_eq!(out.len(), popcount(glyph_bits(b'A').unwrap()) as usize * 6);
        assert!(
            out.iter()
                .all(|v| (0.0..=8.0).contains(&v.pos[0]) && (0.0..=8.0).contains(&v.pos[1]))
        );

        // scale 2：位数不变、盒放大到 16×16
        let mut scaled = Vec::new();
        push_text_vertices(
            &mut scaled,
            &UiText {
                string: "A".into(),
                scale: 2.0,
            },
            [0.0, 0.0],
            UiColor::default(),
        );
        assert_eq!(scaled.len(), out.len(), "位数与 scale 无关");
        assert!(scaled.iter().all(|v| v.pos[0] <= 16.0 && v.pos[1] <= 16.0));

        // 原点平移生效
        let mut moved = Vec::new();
        push_text_vertices(
            &mut moved,
            &UiText {
                string: "A".into(),
                scale: 1.0,
            },
            [5.0, 7.0],
            UiColor::default(),
        );
        let min_x = moved.iter().map(|v| v.pos[0]).fold(f32::MAX, f32::min);
        let min_y = moved.iter().map(|v| v.pos[1]).fold(f32::MAX, f32::min);
        assert_eq!((min_x, min_y), (5.0, 7.0));
    }

    #[test]
    fn hidden_ui_entities_produce_no_vertices() {
        // Visibility::HIDDEN 的矩形与文本均不进入收集结果；
        // 组件缺席 = 可见（默认行为零变化）
        let mut world = iris_core::world::World::new();
        use crate::visibility::Visibility;
        world.spawn((
            UiRect {
                position: [0.0, 0.0],
                size: [10.0, 10.0],
            },
            UiColor::default(),
            Visibility::HIDDEN,
        ));
        world.spawn((
            UiRect {
                position: [20.0, 0.0],
                size: [0.0, 0.0],
            },
            UiText {
                string: "HIDE".into(),
                scale: 1.0,
            },
            UiColor::default(),
            Visibility::HIDDEN,
        ));
        let visible_rect = world.spawn((
            UiRect {
                position: [40.0, 0.0],
                size: [8.0, 8.0],
            },
            UiColor::default(),
        ));
        let _visible_text = world.spawn((
            UiRect {
                position: [60.0, 0.0],
                size: [0.0, 0.0],
            },
            UiText {
                string: "A".into(),
                scale: 1.0,
            },
            UiColor::default(),
        ));
        // 显式 VISIBLE 与缺席等价
        world.insert(visible_rect, Visibility::VISIBLE);

        let mut out = Vec::new();
        collect_ui_vertices(&world, &mut out);
        let text_verts = popcount(glyph_bits(b'A').unwrap()) as usize * 6;
        assert_eq!(
            out.len(),
            6 + text_verts,
            "只有可见矩形（6 顶点）+ 可见文本；隐藏的矩形与文本不应产生顶点"
        );
        let min_x = out.iter().map(|v| v.pos[0]).fold(f32::MAX, f32::min);
        assert!(
            min_x >= 40.0,
            "隐藏实体（x=0/20）不应出现，最小 x 应为 40，实际 {min_x}"
        );
    }

    #[test]
    fn text_width_matches_monospace_layout() {
        let measure = |string: &str, scale: f32| {
            text_width(&UiText {
                string: string.into(),
                scale,
            })
        };
        // 单行：8 × scale × 字符数
        assert_eq!(measure("TIME 60", 3.0), 7.0 * 8.0 * 3.0);
        assert_eq!(measure("", 3.0), 0.0, "空文本宽度为 0");
        // 多行取最长行
        assert_eq!(measure("AB\nABCD", 1.0), 4.0 * 8.0);
        assert_eq!(measure("ABCDE\nAB", 2.0), 5.0 * 8.0 * 2.0);
        // 不可绘制字符（空格 / 非 ASCII）同样前进步进，计入宽度
        assert_eq!(measure("  A", 1.0), 3.0 * 8.0);
        assert_eq!(measure("éA", 1.0), 2.0 * 8.0);
        // 右对齐用例：x = 屏宽 - 宽度 - 边距后，布局盒右缘恰好落在
        // 右边距上，且全部墨迹都在布局盒内（等宽步进 ≥ 墨迹包围盒）
        let text = UiText {
            string: "TIME 59".into(),
            scale: 3.0,
        };
        let width = text_width(&text);
        let origin_x = 1280.0 - width - 24.0;
        assert_eq!(origin_x + width, 1280.0 - 24.0, "布局盒右缘应落在右边距");
        let mut out = Vec::new();
        push_text_vertices(&mut out, &text, [origin_x, 0.0], UiColor::default());
        let max_x = out.iter().map(|v| v.pos[0]).fold(f32::MIN, f32::max);
        assert!(
            max_x <= 1280.0 - 24.0,
            "墨迹不得越过布局盒右缘（实际 {max_x}）"
        );
    }

    #[test]
    fn text_advances_monospace_with_newline_and_skips() {
        // 空格不产生顶点但仍前进步进：两个空格后 'A' 从 x=16 起
        let mut out = Vec::new();
        push_text_vertices(
            &mut out,
            &UiText {
                string: "  A".into(),
                scale: 1.0,
            },
            [0.0, 0.0],
            UiColor::default(),
        );
        let min_x = out.iter().map(|v| v.pos[0]).fold(f32::MAX, f32::min);
        assert_eq!(min_x, 16.0);

        // \n 行高 = 8×scale：scale 2 时第二行 pen_y = 16，'A' 最低亮行为
        // 第 6 行（末行全零），故最低顶点 y = 16 + 6×2 + 2 = 30
        let mut lines = Vec::new();
        push_text_vertices(
            &mut lines,
            &UiText {
                string: "A\nA".into(),
                scale: 2.0,
            },
            [0.0, 0.0],
            UiColor::default(),
        );
        let max_y = lines.iter().map(|v| v.pos[1]).fold(f32::MIN, f32::max);
        assert_eq!(max_y, 30.0);

        // 非 ASCII 跳过但仍前进：'é' 占 1 步，'A' 从 x=8 起
        let mut unicode = Vec::new();
        push_text_vertices(
            &mut unicode,
            &UiText {
                string: "éA".into(),
                scale: 1.0,
            },
            [0.0, 0.0],
            UiColor::default(),
        );
        let min_x = unicode.iter().map(|v| v.pos[0]).fold(f32::MAX, f32::min);
        assert_eq!(min_x, 8.0);

        // scale 0：无可绘制内容
        let mut empty = Vec::new();
        push_text_vertices(
            &mut empty,
            &UiText {
                string: "A".into(),
                scale: 0.0,
            },
            [0.0, 0.0],
            UiColor::default(),
        );
        assert!(empty.is_empty());
    }

    /// 图片 quad：6 顶点、全屏 UV（0..1 跨整个纹理）、像素盒 = position+size、
    /// 乘色恒为顶点色。
    #[test]
    fn image_vertices_use_fullscreen_uv() {
        let mut out = Vec::new();
        push_image_vertices(
            &mut out,
            &UiRect {
                position: [10.0, 20.0],
                size: [30.0, 40.0],
            },
            UiColor([1.0, 0.0, 0.0, 1.0]),
        );
        assert_eq!(out.len(), 6, "图片 quad = 2 三角形 6 顶点");
        let max_x = out.iter().map(|v| v.pos[0]).fold(f32::MIN, f32::max);
        let max_y = out.iter().map(|v| v.pos[1]).fold(f32::MIN, f32::max);
        assert_eq!((max_x, max_y), (40.0, 60.0), "图片盒 = position + size");
        // UV 恒为全屏 0..1（整个纹理映射到图片盒）
        assert!(out.iter().any(|v| v.uv == [0.0, 0.0]));
        assert!(out.iter().any(|v| v.uv == [1.0, 1.0]));
        assert!(
            out.iter()
                .all(|v| (0.0..=1.0).contains(&v.uv[0]) && (0.0..=1.0).contains(&v.uv[1])),
            "图片 UV 应全部落在 0..1"
        );
        assert!(out.iter().all(|v| v.color == [1.0, 0.0, 0.0, 1.0]));
    }

    /// 图片收集：UiImage / UiImageRtt 实体各产生一个带源标记的 6 顶点项；
    /// 挂 `Visibility::HIDDEN` 的实体不产生顶点；挂图片的实体其 `UiRect`
    /// 不再被矩形收集路径当作普通矩形绘制（否则缺省白矩形盖住图片）。
    #[test]
    fn collect_image_vertices_groups_by_source_and_honors_visibility() {
        use crate::rendertarget::RenderTargetHandle;
        use crate::visibility::Visibility;
        let mut world = iris_core::world::World::new();
        let tex = TextureHandle(3);
        world.spawn((
            UiRect {
                position: [0.0, 0.0],
                size: [8.0, 8.0],
            },
            UiImage { texture: tex },
            UiColor::default(),
        ));
        world.spawn((
            UiRect {
                position: [20.0, 0.0],
                size: [8.0, 8.0],
            },
            UiImageRtt {
                target: RenderTargetHandle(5),
            },
            UiColor::default(),
            Visibility::HIDDEN,
        ));
        let mut out = Vec::new();
        collect_ui_image_vertices(&world, &mut out);
        assert_eq!(out.len(), 1, "HIDDEN 的图片实体不应产生顶点");
        assert_eq!(out[0].texture, Some(tex));
        assert_eq!(out[0].rtt, None);
        // 图片实体的 UiRect 不被矩形收集路径当作普通矩形（否则白矩形盖图）
        let mut rects = Vec::new();
        collect_ui_vertices(&world, &mut rects);
        assert!(
            rects.is_empty(),
            "挂图片的实体其 UiRect 只作定位载体，不应产生矩形顶点"
        );
    }
}
