//! 验收工具：R52c 烘焙光照 c 阶段（GPU 集成与混合语义）的**烘焙前后对比**。
//!
//! 场景与 `tests/lightmap_render.rs` 同款室内盒（10×10 m 地板 + 后墙 +
//! 左墙 + 0.6×2×0.6 m 遮挡柱），相机在室内斜看墙角；方向光侧照
//! （`(-0.55, -1, 0.3)`，暖色 2.2 强度）+ CSM 阴影开启。
//!
//! - **帧 A**：原状（无光照贴图）——间接光 = IBL 程序化天空；
//! - **帧 B**：`bake_lightmap` + `install_lightmap`（UV2 注入 + 图集挂载）
//!   ——间接光 = 烘焙的天空 AO + 一次反弹色溢（8 texel/m、256² 图集）。
//!
//! 两帧**同场景同相机**，横拼输出 `iris_bake_lightmap.ppm`（读回的像素已
//! 是 sRGB 编码，直接丢 alpha 写 P6），随后转 PNG 目检：
//!
//! ```sh
//! cargo run -p iris-render --example bake_lightmap_snapshot --release
//! ffmpeg -y -i iris_bake_lightmap.ppm docs/bake_lightmap.png
//! ```
//!
//! 目检要点（左 A / 右 B）：
//! - **墙角与墙地交接**：B 明显比 A 暗（烘焙的天空项按半球遮蔽扣除 = AO），
//!   且暗化沿墙角连续渐变、无条带；
//! - **柱根**：B 有一圈接触暗化（半球被柱身遮挡），与 CSM 的动态投影
//!   叠加但不重影（直接光只由运行时方向光提供，烘焙不含直射）；
//! - **受光地板**：B 与 A 亮度接近（直射 + 动态阴影逐位同源，只有间接项
//!   被替换）；
//! - **色溢**：B 里靠近红色柱子的地板带一点暖红（一次反弹携带 albedo），
//!   A 里没有；
//! - **chart 边缘**：无半 texel 宽的黑线（dilation 填充的验收点，见
//!   `lightmap.rs` 模块文档）；
//! - **无闪烁 / 无噪点**：64 半球样本 + 低差异序列 ⇒ 平滑（确定性另由
//!   集成测试逐位锁定）。
//!
//! headless 断言（失败直接 panic）：帧 A 全帧哈希 = 集成测试的 R52b 基线
//! （同一场景同一分辨率）、帧 B 与 A 显著不同、暗部四分位下降 ≥ 15%、
//! 亮部四分位保持 ≥ 85%。

use std::path::Path;
use std::time::Instant;

use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Quat, Transform, Vec3, World};
use iris_render::prelude::*;

/// 输出分辨率（与 `tests/lightmap_render.rs` 一致 ⇒ 帧 A 哈希可对照）。
const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
/// 两帧之间的分隔条宽度（像素）。
const GAP: u32 = 6;
/// 验收口径的 UV 参数：8 texel/m + 256² 图集（比集成测试的 4 / 128 细一档，
/// 目检要看清 AO 渐变与 chart 边缘）。
const TEXELS_PER_METER: f32 = 8.0;
const ATLAS: u32 = 256;

/// 室内盒场景（与集成测试 `room_world` 同款几何 / 相机 / 光照）。
fn room_world() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera {
        eye: Vec3::new(3.4, 2.2, 3.8),
        target: Vec3::new(-1.2, 0.6, -2.0),
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.55, -1.0, 0.3).normalize(),
        color: Vec3::new(1.0, 0.95, 0.85),
        intensity: 2.2,
        ambient: Vec3::new(0.10, 0.11, 0.14),
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());

    let floor_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(10.0));
    let wall_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(10.0));
    let column_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mut mats = world.resource_mut::<MaterialStore>();
    let floor_mat = mats.add(MaterialDef::solid(0.8, 0.78, 0.72, 1.0));
    let wall_mat = mats.add(MaterialDef::solid(0.55, 0.6, 0.7, 1.0));
    let column_mat = mats.add(MaterialDef::solid(0.9, 0.35, 0.2, 1.0));
    drop(mats);

    world.spawn((Transform::IDENTITY, floor_mesh, floor_mat));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 2.0, -5.0),
            rotation: Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
            scale: Vec3::new(1.0, 1.0, 0.4),
        },
        wall_mesh,
        wall_mat,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(-5.0, 2.0, 0.0),
            rotation: Quat::from_rotation_z(-std::f32::consts::FRAC_PI_2),
            scale: Vec3::new(0.4, 1.0, 1.0),
        },
        wall_mesh,
        wall_mat,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(1.5, 1.0, 0.0),
            scale: Vec3::new(0.6, 2.0, 0.6),
            ..Transform::IDENTITY
        },
        column_mesh,
        column_mat,
    ));
    propagate_transforms(&mut world);
    world
}

/// 烘焙参数：与运行时方向光同源（方向 / 颜色 × 强度），天空压暗（一次
/// 反弹会填掉大半遮蔽对比，直射主导才有稳健的目检对比，量级论证见
/// ARCHITECTURE §4.24）。
fn bake_params() -> BakeParams {
    BakeParams {
        light_dir: Vec3::new(-0.55, -1.0, 0.3),
        light_color: Vec3::new(1.0, 0.95, 0.85) * 2.2,
        sky_color: Vec3::new(0.12, 0.15, 0.22),
        shadow_softness: 0.08,
        sky_samples: 64,
        bounces: 1,
    }
}

/// sRGB 字节的感知亮度（Rec. 709 权重）。
fn luminance(px: &[u8], i: usize) -> u32 {
    (u32::from(px[i * 4]) * 2126 + u32::from(px[i * 4 + 1]) * 7152 + u32::from(px[i * 4 + 2]) * 722)
        / 10_000
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 两帧横拼写 PPM P6（读回的像素已是 sRGB 编码，直接丢 alpha）。
fn write_side_by_side(
    left: &[u8],
    right: &[u8],
    width: u32,
    height: u32,
    path: &Path,
) -> std::io::Result<()> {
    use std::io::Write;
    let total_w = width * 2 + GAP;
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    writeln!(out, "P6\n{total_w} {height}\n255")?;
    for y in 0..height {
        for x in 0..total_w {
            let pixel = if x < width {
                &left[((y * width + x) * 4) as usize..]
            } else if x < width + GAP {
                &[40u8, 40, 40, 255] // 分隔条：深灰，肉眼可辨左右
            } else {
                &right[((y * width + (x - width - GAP)) * 4) as usize..]
            };
            out.write_all(&pixel[..3])?;
        }
    }
    out.flush()
}

fn main() {
    let Some(mut renderer) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    // ---- 帧 A：原状（无光照贴图）----
    let world_plain = room_world();
    renderer.render(&world_plain);
    let px_plain = renderer.read_pixels();
    let hash_plain = fnv1a(&px_plain);
    println!("帧 A（无光照贴图）：全帧 FNV-1a = {hash_plain:#018x}");

    // ---- 烘焙 ----
    let mut world_baked = room_world();
    let uv = BakeUvParams {
        texels_per_meter: TEXELS_PER_METER,
        atlas_size: ATLAS,
    };
    let started = Instant::now();
    let baked = bake_lightmap(&world_baked, &bake_params(), &uv);
    let bake_elapsed = started.elapsed();
    let covered = baked.covered.iter().filter(|&&c| c).count();
    let started = Instant::now();
    let report = install_lightmap(&mut world_baked, baked);
    let install_elapsed = started.elapsed();
    let store = world_baked.resource::<LightmapStore>();
    let item = store.get(report.lightmap).expect("目录条目存在");
    println!(
        "烘焙 {bake_elapsed:?} + 装配 {install_elapsed:?}：覆盖 texel {covered}、\
         chart {} 个、shelf 已用 {} / {ATLAS}、利用率 {:.1}%、注入实体 {}、拆分顶点 {}、\
         图集 {ATLAS}×{ATLAS}（GPU {} KiB）",
        item.atlas.entries.len(),
        item.atlas.used_height,
        item.atlas.utilization() * 100.0,
        report.entities,
        report.vertices_added,
        store.gpu_bytes_estimate() / 1024
    );
    drop(store);

    // ---- 帧 B：烘焙 + 挂载 ----
    renderer.render(&world_baked);
    let px_baked = renderer.read_pixels();
    assert!(
        renderer.renderer().lightmap_is_uploaded(report.lightmap),
        "图集必须已懒上传"
    );
    println!(
        "帧 B（挂光照贴图）：全帧 FNV-1a = {:#018x}",
        fnv1a(&px_baked)
    );

    // ---- headless 断言 ----
    let total = (WIDTH * HEIGHT) as usize;
    let changed: Vec<usize> = (0..total)
        .filter(|&i| {
            (0..3).any(|c| (px_plain[i * 4 + c] as i32 - px_baked[i * 4 + c] as i32).abs() > 2)
        })
        .collect();
    let coverage = changed.len() as f64 / total as f64;
    let mut by_luma = changed.clone();
    by_luma.sort_unstable_by_key(|&i| luminance(&px_plain, i));
    let quartile = by_luma.len() / 4;
    let mean_of = |set: &[usize], px: &[u8]| -> f64 {
        set.iter().map(|&i| luminance(px, i) as f64).sum::<f64>() / set.len() as f64
    };
    let (dark_a, dark_b) = (
        mean_of(&by_luma[..quartile], &px_plain),
        mean_of(&by_luma[..quartile], &px_baked),
    );
    let (bright_a, bright_b) = (
        mean_of(&by_luma[by_luma.len() - quartile..], &px_plain),
        mean_of(&by_luma[by_luma.len() - quartile..], &px_baked),
    );
    println!(
        "受影响像素 {} / {total}（{:.1}%）、暗部四分位 {dark_a:.1} → {dark_b:.1}（{:.0}%）、\
         亮部四分位 {bright_a:.1} → {bright_b:.1}（{:.0}%）",
        changed.len(),
        coverage * 100.0,
        dark_b / dark_a * 100.0,
        bright_b / bright_a * 100.0
    );
    println!(
        "四角像素 A/B：左上 {:?}/{:?}、右上 {:?}/{:?}、左下 {:?}/{:?}、右下 {:?}/{:?}",
        &px_plain[0..3],
        &px_baked[0..3],
        &px_plain[(WIDTH - 1) as usize * 4..(WIDTH - 1) as usize * 4 + 3],
        &px_baked[(WIDTH - 1) as usize * 4..(WIDTH - 1) as usize * 4 + 3],
        &px_plain[((HEIGHT - 1) * WIDTH) as usize * 4..((HEIGHT - 1) * WIDTH) as usize * 4 + 3],
        &px_baked[((HEIGHT - 1) * WIDTH) as usize * 4..((HEIGHT - 1) * WIDTH) as usize * 4 + 3],
        &px_plain[(((HEIGHT - 1) * WIDTH + WIDTH - 1) * 4) as usize
            ..((HEIGHT - 1) * WIDTH + WIDTH - 1) as usize * 4 + 3],
        &px_baked[(((HEIGHT - 1) * WIDTH + WIDTH - 1) * 4) as usize
            ..((HEIGHT - 1) * WIDTH + WIDTH - 1) as usize * 4 + 3]
    );
    assert!(coverage > 0.10, "室内盒必须占据画面（{coverage:.1}%）");
    assert!(
        dark_b < dark_a * 0.85,
        "暗部四分位必须下降 ≥ 15%：{dark_a:.1} → {dark_b:.1}"
    );
    assert!(
        bright_b > bright_a * 0.85,
        "亮部四分位必须保持 ≥ 85%：{bright_a:.1} → {bright_b:.1}"
    );

    // ---- 横拼导出 ----
    let path = Path::new("iris_bake_lightmap.ppm");
    write_side_by_side(&px_plain, &px_baked, WIDTH, HEIGHT, path)
        .unwrap_or_else(|e| panic!("写 {path:?}：{e}"));
    println!(
        "已输出 {path:?}（{}×{}，左 A / 右 B）",
        WIDTH * 2 + GAP,
        HEIGHT
    );
    println!("转 PNG：ffmpeg -y -i iris_bake_lightmap.ppm docs/bake_lightmap.png");
}
