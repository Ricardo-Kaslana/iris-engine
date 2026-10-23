//! 平色天空（BLOCKWILD 风格）。
//!
//! 参考项目的天空是一整片低饱和的淡蓝：几乎看不到渐变，没有太阳盘，
//! 只在地平线附近轻微发白。引擎的程序化天空带太阳盘与明显的天顶蓝，
//! 和这种「纸片天空」相去较远；这里直接生成一张 256×128 的等距柱状
//! 线性 HDR 图，经 `SkySource::equirect_hdr` 换入。
//!
//! 图像约定与引擎一致：第 0 行 = +Y 天顶。天空是经度无关的，每行一个
//! 颜色，因此不会出现接缝；地平线以下淡到灰绿，避免俯视时看到纯黑。

use iris_engine::prelude::{HdrImage, SkySource};

/// 天顶色（线性 RGB）。目标 sRGB 约 (168, 202, 222)。
pub const SKY_ZENITH: [f32; 3] = [0.27, 0.48, 0.75];
/// 地平线色（线性 RGB）。目标 sRGB 约 (190, 214, 228)。
pub const SKY_HORIZON: [f32; 3] = [0.45, 0.62, 0.82];
/// 地平线以下的淡出色，只用于兜底视线。
pub const SKY_GROUND: [f32; 3] = [0.24, 0.30, 0.24];

const SKY_WIDTH: u32 = 256;
const SKY_HEIGHT: u32 = 128;

fn smoothstep(low: f32, high: f32, value: f32) -> f32 {
    let t = ((value - low) / (high - low)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// 生成天空全景图。行方向做天顶到地平线的平滑过渡，列方向恒定。
pub fn sky_image() -> HdrImage {
    let mut image = HdrImage::new(SKY_WIDTH, SKY_HEIGHT);
    for y in 0..SKY_HEIGHT {
        // 仰角：第 0 行 = 天顶（+1），最后一行 = 天底（-1）。
        let elevation = 1.0 - 2.0 * (y as f32 + 0.5) / SKY_HEIGHT as f32;
        let color = if elevation >= 0.0 {
            // 前 30% 仰角走完大部分过渡，再往上缓慢加深，接近参考图的
            // 「整片淡蓝、只在地平线附近泛白」。
            let t = smoothstep(0.0, 0.30, elevation);
            mix(SKY_HORIZON, SKY_ZENITH, t * 0.8 + elevation * 0.2)
        } else {
            let t = smoothstep(0.0, 0.25, -elevation);
            mix(SKY_HORIZON, SKY_GROUND, t)
        };
        for x in 0..SKY_WIDTH {
            image.set_pixel(x, y, color);
        }
    }
    image
}

/// 换入引擎的天空源资源。`equirect_hdr` 自带 generation，渲染器会重烘 IBL。
pub fn sky_source() -> SkySource {
    SkySource::equirect_hdr(sky_image())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sky_is_longitude_invariant() {
        let image = sky_image();
        for y in 0..SKY_HEIGHT {
            let first = image.pixel(0, y);
            for x in [1, 17, SKY_WIDTH - 1] {
                assert_eq!(image.pixel(x, y), first, "同一行颜色应一致");
            }
        }
    }

    #[test]
    fn zenith_is_deeper_than_horizon() {
        let image = sky_image();
        let zenith = image.pixel(0, 0);
        let horizon = image.pixel(0, SKY_HEIGHT / 2);
        let zenith_blue = zenith[2] - zenith[0];
        let horizon_blue = horizon[2] - horizon[0];
        assert!(
            zenith_blue > horizon_blue,
            "天顶的蓝色成分应比地平线更强：{zenith:?} vs {horizon:?}"
        );
        assert!(
            zenith.iter().sum::<f32>() < horizon.iter().sum::<f32>(),
            "地平线整体应更亮"
        );
    }

    #[test]
    fn below_horizon_fades_to_ground_without_black() {
        let image = sky_image();
        let bottom = image.pixel(0, SKY_HEIGHT - 1);
        assert!(bottom.iter().all(|channel| *channel > 0.2));
    }

    #[test]
    fn sky_image_is_deterministic() {
        assert_eq!(sky_image().rgb, sky_image().rgb);
    }
}
