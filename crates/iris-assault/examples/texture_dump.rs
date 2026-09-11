//! 工具：把程序化贴图导出为 PPM（目检材质本身，不经过渲染）。
//!
//! ```sh
//! cargo run -p iris-assault --example texture_dump --release
//! ```

use iris_assault::scenery_tex::{Surface, surface_texture};
use iris_assault::soldier_tex;
use iris_engine::prelude::CpuTexture;

fn write_ppm(path: &str, texture: &CpuTexture) {
    let mut bytes = format!("P6\n{} {}\n255\n", texture.width, texture.height).into_bytes();
    for pixel in texture.rgba.chunks_exact(4) {
        bytes.extend_from_slice(&pixel[..3]);
    }
    std::fs::write(path, bytes).expect("写 PPM");
    println!("{path}: {}×{}", texture.width, texture.height);
}

fn main() {
    for surface in Surface::ALL {
        let set = surface_texture(surface);
        let name = format!("tex_{surface:?}");
        write_ppm(&format!("{name}_base.ppm"), &set.base);
        if let Some(normal) = &set.normal {
            write_ppm(&format!("{name}_normal.ppm"), normal);
        }
        if let Some(orm) = &set.orm {
            write_ppm(&format!("{name}_orm.ppm"), orm);
        }
    }
    let atlas = soldier_tex::soldier_atlas(soldier_tex::ATLAS_SEED);
    write_ppm("tex_soldier_base.ppm", &atlas.base);
    write_ppm("tex_soldier_normal.ppm", &atlas.normal);
}
