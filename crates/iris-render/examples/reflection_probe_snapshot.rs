//! 反射探针验收图（R62a）：四组面板对照，输出 `iris_reflection_probe.ppm`
//! → `docs/reflection_probe.png`。
//!
//! 面板布局（上排 / 下排各两格，每格「关 | 开」左右并排）：
//! 1. **室内盒场景**：金属球 + 金属地面 + 彩色图案箱，探针**关|开**——
//!    开启后地面与球面出现墙面 / 图案箱的反射（全局 env 只反射天空）；
//! 2. **视差修正 关|开**：金属地面反射天花板标记，左格反射落在「无重定向」
//!    位置（错位），右格落回解析镜像位置（贴合盒几何）；
//! 3. **粗糙度 → mip**：同一探针场景 roughness 0.1 | 0.8——高粗糙度走更高
//!    mip，反射明显变糊（与全局 env 同一套预滤波链）；
//! 4. **光照贴图协同**：挂光照贴图的金属球（§8.31④：无环境镜面）在探针
//!    关|开下的对照——开启后镜面由探针补回。
//!
//! 每格都带硬断言（不通过直接 panic、不写文件）；跨运行逐字节一致
//! （同输入两次渲染的 FNV-1a 全帧哈希相同，本示例打印哈希供比对）。
//!
//! ```sh
//! cargo run -p iris-render --example reflection_probe_snapshot --release
//! ffmpeg -y -i iris_reflection_probe.ppm docs/reflection_probe.png
//! ```

use std::path::Path;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Entity, Transform, World};
use iris_render::prelude::*;

const WIDTH: u32 = 480;
const HEIGHT: u32 = 270;
const GAP: u32 = 6;
/// 面板行数（每组一行，行内两格）。
const ROWS: u32 = 4;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn pixel(px: &[u8], w: u32, x: u32, y: u32) -> [i32; 3] {
    let i = ((y * w + x) * 4) as usize;
    [px[i] as i32, px[i + 1] as i32, px[i + 2] as i32]
}

fn count_if(px: &[u8], f: impl Fn([i32; 3]) -> bool) -> usize {
    px.chunks_exact(4)
        .filter(|c| f([c[0] as i32, c[1] as i32, c[2] as i32]))
        .count()
}

fn is_red(p: [i32; 3]) -> bool {
    p[0] > 110 && p[0] - p[1] > 60 && p[0] - p[2] > 60
}

fn is_green(p: [i32; 3]) -> bool {
    p[1] > 110 && p[1] - p[0] > 60 && p[1] - p[2] > 60
}

fn base_world(camera: Camera) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(camera);
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.5, -1.0, 0.35).normalize(),
        intensity: 1.8,
        ambient: Vec3::new(0.08, 0.09, 0.12),
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    world
}

fn emissive(r: f32, g: f32, b: f32) -> MaterialDef {
    let mut mat = MaterialDef::solid(r, g, b, 1.0);
    mat.emissive = [r * 3.0, g * 3.0, b * 3.0, 1.0];
    mat
}

/// 面板 1：室内盒场景——金属地面 + 中央金属球 + 相机身后的彩色图案板
/// （图案板在相机背后 ⇒ 画面里不可能直接看到它们，红 / 绿像素只能来自反射），
/// 探针在房间中央。
fn room_world(with_probe: bool) -> (World, Option<Entity>) {
    let mut world = base_world(Camera {
        eye: Vec3::new(1.4, 2.3, 4.2),
        target: Vec3::new(0.0, 0.4, -1.2),
        fov_y_radians: std::f32::consts::FRAC_PI_4,
        ..Camera::default()
    });
    // 地面（金属镜面）与三面墙（漫反射灰）
    let floor = world.resource_mut::<MeshStore>().add(MeshData::plane(12.0));
    let mirror = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::metal(0.75, 0.76, 0.8, 0.08));
    world.spawn((Transform::IDENTITY, floor, mirror));
    let wall = world.resource_mut::<MeshStore>().add(MeshData::plane(12.0));
    let wall_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.55, 0.58, 0.62, 1.0));
    let walls = [
        (
            Vec3::new(0.0, 2.5, -5.0),
            glam::Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
        ),
        (
            Vec3::new(-5.0, 2.5, 0.0),
            glam::Quat::from_rotation_z(-std::f32::consts::FRAC_PI_2),
        ),
        (
            Vec3::new(5.0, 2.5, 0.0),
            glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
        ),
    ];
    for (position, rotation) in walls {
        world.spawn((
            Transform {
                position,
                rotation,
                ..Transform::IDENTITY
            },
            wall,
            wall_mat,
        ));
    }
    // 中央金属球
    let sphere = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(0.9, 48, 32));
    let chrome = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::metal(0.92, 0.93, 0.95, 0.06));
    world.spawn((Transform::from_xyz(0.0, 0.9, -1.6), sphere, chrome));
    // 后墙上的红 / 绿图案板：高挂 ⇒ 只在上半屏直接可见；**下半屏（金属地面）
    // 里的红 / 绿只能来自反射**（这是本面板的断言口径）
    let slab = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let red = world
        .resource_mut::<MaterialStore>()
        .add(emissive(1.0, 0.15, 0.1));
    let green = world
        .resource_mut::<MaterialStore>()
        .add(emissive(0.15, 1.0, 0.2));
    for (x, mat) in [(-1.7f32, red), (1.7, green)] {
        world.spawn((
            Transform {
                position: Vec3::new(x, 3.0, -4.8),
                scale: Vec3::new(2.4, 1.8, 0.3),
                ..Transform::IDENTITY
            },
            slab,
            mat,
        ));
    }
    let probe = with_probe.then(|| {
        world.spawn((
            Transform::from_xyz(0.0, 1.2, 0.0),
            ReflectionProbe::new(Vec3::new(5.0, 2.5, 5.0)).with_falloff(1.0),
        ))
    });
    propagate_transforms(&mut world);
    (world, probe)
}

/// 面板 2：视差修正对照——金属地面 + 天花板标记（标记贴在探针盒顶面上）。
fn parallax_world() -> (World, Entity) {
    let mut world = base_world(Camera {
        eye: Vec3::new(0.0, 2.4, 1.6),
        target: Vec3::new(0.0, 0.2, -0.6),
        fov_y_radians: std::f32::consts::FRAC_PI_4,
        ..Camera::default()
    });
    let floor = world.resource_mut::<MeshStore>().add(MeshData::plane(12.0));
    let mirror = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::metal(0.9, 0.9, 0.92, 0.05));
    world.spawn((Transform::IDENTITY, floor, mirror));
    let slab = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let red = world
        .resource_mut::<MaterialStore>()
        .add(emissive(1.0, 0.2, 0.1));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 3.0, -1.0),
            scale: Vec3::new(0.5, 0.04, 0.5),
            ..Transform::IDENTITY
        },
        slab,
        red,
    ));
    let probe = world.spawn((
        Transform::from_xyz(0.0, 1.475, 0.0),
        ReflectionProbe::new(Vec3::new(3.0, 1.525, 3.0)).with_falloff(0.0),
    ));
    propagate_transforms(&mut world);
    (world, probe)
}

/// 面板 3：粗糙度对照——金属球反射两侧自发光板（相机窄视角，板在视锥外）。
fn roughness_world(roughness: f32) -> (World, Entity) {
    let mut world = base_world(Camera {
        eye: Vec3::new(0.0, 1.4, 11.0),
        target: Vec3::new(0.0, 1.0, 0.0),
        fov_y_radians: 0.42,
        ..Camera::default()
    });
    // 方向光压暗：太阳高光会盖过「反射被摊开」的度量（本面板只关心探针 mip）
    world.insert_resource(DirectionalLight {
        intensity: 0.3,
        ambient: Vec3::new(0.04, 0.04, 0.05),
        ..DirectionalLight::default()
    });
    let sphere = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, 48, 32));
    let mirror = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::metal(0.95, 0.95, 0.95, roughness));
    world.spawn((Transform::from_xyz(0.0, 1.0, -1.5), sphere, mirror));
    let slab = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let red = world
        .resource_mut::<MaterialStore>()
        .add(emissive(1.0, 0.0, 0.0));
    let green = world
        .resource_mut::<MaterialStore>()
        .add(emissive(0.0, 1.0, 0.0));
    // ×9：宽银幕（16:9）下水平半视角 ≈ 20.7°，板必须远在视锥外（否则度量
    // 会被「直接看到的板」淹没，粗糙度对比失效）
    for (x, mat) in [(9.0f32, red), (-9.0, green)] {
        world.spawn((
            Transform {
                position: Vec3::new(x, 1.0, -1.5),
                scale: Vec3::new(0.2, 3.0, 3.0),
                ..Transform::IDENTITY
            },
            slab,
            mat,
        ));
    }
    let probe = world.spawn((
        Transform::from_xyz(0.0, 1.0, 0.0),
        ReflectionProbe::new(Vec3::new(8.0, 4.0, 8.0)),
    ));
    propagate_transforms(&mut world);
    (world, probe)
}

/// 面板 4：光照贴图协同——真烘焙的室内场景，金属球挂光照贴图（无环境镜面）。
fn lightmap_world(with_probe: bool) -> (World, Option<Entity>) {
    let mut world = base_world(Camera {
        eye: Vec3::new(0.0, 1.6, 9.0),
        target: Vec3::new(0.0, 1.0, -0.5),
        fov_y_radians: 0.55,
        ..Camera::default()
    });
    let floor = world.resource_mut::<MeshStore>().add(MeshData::plane(10.0));
    let floor_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.8, 0.78, 0.72, 1.0));
    world.spawn((Transform::IDENTITY, floor, floor_mat));
    let wall = world.resource_mut::<MeshStore>().add(MeshData::plane(10.0));
    let wall_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.55, 0.6, 0.7, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 2.0, -5.0),
            rotation: glam::Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
            ..Transform::IDENTITY
        },
        wall,
        wall_mat,
    ));
    let sphere = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, 48, 32));
    let mirror = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::metal(0.95, 0.95, 0.95, 0.05));
    world.spawn((Transform::from_xyz(0.0, 1.0, -2.5), sphere, mirror));
    let slab = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let red = world
        .resource_mut::<MaterialStore>()
        .add(emissive(1.0, 0.15, 0.1));
    let green = world
        .resource_mut::<MaterialStore>()
        .add(emissive(0.15, 1.0, 0.2));
    for (x, mat) in [(5.6f32, red), (-5.6, green)] {
        world.spawn((
            Transform {
                position: Vec3::new(x, 1.4, -2.5),
                scale: Vec3::new(0.2, 1.8, 1.8),
                ..Transform::IDENTITY
            },
            slab,
            mat,
        ));
    }
    let probe = with_probe.then(|| {
        world.spawn((
            Transform::from_xyz(0.0, 1.0, 0.0),
            ReflectionProbe::new(Vec3::new(6.0, 3.0, 6.0)),
        ))
    });
    propagate_transforms(&mut world);
    let baked = bake_lightmap(
        &world,
        &BakeParams {
            light_dir: Vec3::new(-0.5, -1.0, 0.35),
            light_color: Vec3::new(1.0, 0.95, 0.85) * 1.8,
            sky_color: Vec3::new(0.12, 0.15, 0.22),
            shadow_softness: 0.08,
            sky_samples: 96,
            bounces: 1,
        },
        &BakeUvParams {
            texels_per_meter: 8.0,
            atlas_size: 256,
        },
    );
    let report = install_lightmap(&mut world, baked);
    println!(
        "  光照贴图：{} 个实体、新增 {} 顶点（图集 {:?}）",
        report.entities, report.vertices_added, report.lightmap
    );
    propagate_transforms(&mut world);
    (world, probe)
}

/// 渲染一帧（可选烘焙探针）并读回像素。
fn render_panel(
    renderer: &mut OffscreenRenderer,
    world: &World,
    probe: Option<Entity>,
    legacy_parallax: bool,
) -> Vec<u8> {
    renderer
        .renderer_mut()
        .set_legacy_probe_parallax(legacy_parallax);
    if let Some(entity) = probe {
        assert!(
            renderer.renderer_mut().bake_reflection_probe(world, entity),
            "探针烘焙应成功"
        );
    }
    renderer.render(world);
    let px = renderer.read_pixels();
    println!(
        "  探针 {}｜视差 {}｜生效探针 {}｜哈希 {:#018x}",
        if probe.is_some() { "开" } else { "关" },
        if legacy_parallax { "关" } else { "开" },
        renderer.renderer().stats().reflection_probes,
        fnv1a(&px)
    );
    px
}

/// 写 4 排 × 2 列的 PPM P6（与既有验收图示例同款面板拼装）。
fn write_grid(path: &Path, panels: &[(Vec<u8>, Vec<u8>)]) -> std::io::Result<()> {
    use std::io::Write;
    let total_w = WIDTH * 2 + GAP;
    let total_h = HEIGHT * ROWS + GAP * (ROWS - 1);
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    writeln!(out, "P6\n{total_w} {total_h}\n255")?;
    let mut row = vec![0u8; (total_w * 3) as usize];
    for (r, (left, right)) in panels.iter().enumerate() {
        if r > 0 {
            for _ in 0..GAP {
                out.write_all(&vec![90u8; (total_w * 3) as usize])?;
            }
        }
        for y in 0..HEIGHT {
            for x in 0..(total_w as usize) {
                let pixel: &[u8] = if (x as u32) < WIDTH {
                    &left[((y * WIDTH + x as u32) * 4) as usize..]
                } else if (x as u32) < WIDTH + GAP {
                    &[40, 40, 40, 255]
                } else {
                    let rx = x - WIDTH as usize - GAP as usize;
                    &right[((y * WIDTH + rx as u32) * 4) as usize..]
                };
                row[x * 3..x * 3 + 3].copy_from_slice(&pixel[..3]);
            }
            out.write_all(&row)?;
        }
    }
    out.flush()
}

fn main() {
    let Some(mut renderer) = OffscreenRenderer::new(WIDTH, HEIGHT) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut panels: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

    // ---- 面板 1：室内盒场景 探针 关 | 开 ----
    println!("面板 1：室内盒场景 探针 关|开");
    let (world, _) = room_world(false);
    let panel1_left = render_panel(&mut renderer, &world, None, false);
    let (world, probe) = room_world(true);
    let panel1_right = render_panel(&mut renderer, &world, probe, false);
    // 断言只看**下半屏（地面）**：图案板挂在上半屏直接可见，下半屏里的红只能
    // 来自金属地面的反射 ⇒ 关探针时必然为 0。
    let lower_red = |px: &[u8]| {
        let mut n = 0;
        for y in HEIGHT / 2..HEIGHT {
            for x in 0..WIDTH {
                if is_red(pixel(px, WIDTH, x, y)) {
                    n += 1;
                }
            }
        }
        n
    };
    let (lr_left, lr_right) = (lower_red(&panel1_left), lower_red(&panel1_right));
    let changed = panel1_left
        .iter()
        .zip(&panel1_right)
        .filter(|(a, b)| a != b)
        .count();
    println!("  地面（下半屏）红色反射像素：关 {lr_left} → 开 {lr_right}｜变更字节 {changed}");
    assert_eq!(
        lr_left, 0,
        "无探针时金属地面只反射天空 ⇒ 下半屏不应有图案板的红（实际 {lr_left}）"
    );
    assert!(
        lr_right > 300,
        "探针开启后金属地面应反射出墙上图案（下半屏红 {lr_right}）"
    );
    assert!(
        changed * 20 > (WIDTH * HEIGHT) as usize,
        "开启探针必须显著改变画面（变更字节 {changed}）"
    );
    panels.push((panel1_left, panel1_right));

    // ---- 面板 2：视差修正 关 | 开 ----
    println!("面板 2：视差修正 关|开");
    let (world, probe) = parallax_world();
    let panel2_left = render_panel(&mut renderer, &world, Some(probe), true);
    let panel2_right = render_panel(&mut renderer, &world, Some(probe), false);
    let diff = panel2_left
        .iter()
        .zip(&panel2_right)
        .filter(|(a, b)| a != b)
        .count();
    println!("  视差开关的差异字节：{diff}");
    assert!(diff > 200, "视差修正必须改变反射落点（差异字节 {diff}）");
    panels.push((panel2_left, panel2_right));

    // ---- 面板 3：粗糙度 0.1 | 0.8 ----
    println!("面板 3：粗糙度 0.1 | 0.8");
    let (world, probe) = roughness_world(0.1);
    let mut rough_renderer = OffscreenRenderer::new(WIDTH, HEIGHT).expect("第二个渲染器");
    let panel3_left = render_panel(&mut rough_renderer, &world, Some(probe), false);
    let (world, probe) = roughness_world(0.8);
    let mut rough_renderer2 = OffscreenRenderer::new(WIDTH, HEIGHT).expect("第三个渲染器");
    let panel3_right = render_panel(&mut rough_renderer2, &world, Some(probe), false);
    // 度量用 `峰值下降 + 弱红面积上升`：模糊把镜面反射摊开——饱和的反射核
    // 变淡（峰值降），边缘扩散（弱红面积升）。两档粗糙度只差探针 mip。
    let reddish = |px: &[u8]| count_if(px, |p| p[0] - p[1].max(p[2]) > 25 && p[0] > 70);
    let strong = |px: &[u8]| count_if(px, |p| p[0] > 120 && p[0] - p[1] > 70 && p[0] - p[2] > 70);
    let peak = |px: &[u8]| {
        px.chunks_exact(4)
            .map(|c| c[0] as i32 - (c[1] as i32).max(c[2] as i32))
            .max()
            .unwrap_or(0)
    };
    let (area_left, area_right) = (reddish(&panel3_left), reddish(&panel3_right));
    let (peak_left, peak_right) = (peak(&panel3_left), peak(&panel3_right));
    println!(
        "  弱红面积 0.1 → {area_left}，0.8 → {area_right}｜强红 {} → {}｜峰值 {peak_left} → {peak_right}",
        strong(&panel3_left),
        strong(&panel3_right),
    );
    assert!(
        peak_left - peak_right > 40,
        "高粗糙度走更高 mip ⇒ 反射峰值必须明显下降（{peak_left} → {peak_right}）"
    );
    assert!(
        area_right > area_left,
        "模糊把反射摊开 ⇒ 弱红面积应上升（{area_left} → {area_right}）"
    );
    assert_ne!(
        fnv1a(&panel3_left),
        fnv1a(&panel3_right),
        "两档粗糙度必须给出不同画面"
    );
    panels.push((panel3_left, panel3_right));

    // ---- 面板 4：光照贴图协同 探针 关 | 开 ----
    println!("面板 4：光照贴图场景 探针 关|开");
    let (world, _) = lightmap_world(false);
    let mut lm_renderer = OffscreenRenderer::new(WIDTH, HEIGHT).expect("第四个渲染器");
    let panel4_left = render_panel(&mut lm_renderer, &world, None, false);
    let (world, probe) = lightmap_world(true);
    let mut lm_renderer2 = OffscreenRenderer::new(WIDTH, HEIGHT).expect("第五个渲染器");
    let panel4_right = render_panel(&mut lm_renderer2, &world, probe, false);
    let (red_l, green_l) = (
        count_if(&panel4_left, is_red),
        count_if(&panel4_left, is_green),
    );
    let (red_r, green_r) = (
        count_if(&panel4_right, is_red),
        count_if(&panel4_right, is_green),
    );
    println!("  红/绿反射像素：关 {red_l}/{green_l} → 开 {red_r}/{green_r}");
    assert_eq!(
        (red_l, green_l),
        (0, 0),
        "无探针时光照贴图实体没有环境镜面 ⇒ 画面里不应有图案板的反射（实际 {red_l}/{green_l}）"
    );
    assert!(
        red_r + green_r >= 20,
        "探针应把镜面项补回给挂光照贴图的金属球（红 {red_r}、绿 {green_r}）"
    );
    panels.push((panel4_left, panel4_right));

    // ---- 性能：烘焙耗时 / 采样开销 / 显存（只打印，不影响验收图）----
    {
        use std::time::Instant;
        let (world_on, probe) = room_world(true);
        let probe = probe.expect("探针");
        let (world_off, _) = room_world(false);
        let mut perf = OffscreenRenderer::new(WIDTH, HEIGHT).expect("性能渲染器");
        // 预热（网格 / 材质 / 纹理懒上传与实例缓冲）
        for _ in 0..5 {
            perf.render(&world_on);
        }
        let _ = perf.read_pixels();

        // 烘焙耗时：命令编码 + 提交 + **GPU 执行完毕**（poll 等待）
        let t0 = Instant::now();
        assert!(perf.renderer_mut().bake_reflection_probe(&world_on, probe));
        let _ = perf.gpu().device.poll(wgpu::PollType::wait_indefinitely());
        let bake_ms = t0.elapsed().as_secs_f64() * 1000.0;

        // 采样开销：同一渲染器、同一相机，探针关（世界不挂组件）vs 开（域内片元
        // 多算 ≤4 次权重 + 1 次 CubeArray 采样）。**逐帧 poll 等待 GPU 完成**——
        // 只计 CPU 编码会被命令缓冲排空掩盖（实测同场景波动 40%+），两侧同款
        // 处理故差值可比。
        let frame_ms = |renderer: &mut OffscreenRenderer, world: &World| {
            for _ in 0..10 {
                renderer.draw_frame(world);
            }
            let _ = renderer
                .gpu()
                .device
                .poll(wgpu::PollType::wait_indefinitely());
            let frames = 120;
            let t = Instant::now();
            for _ in 0..frames {
                renderer.draw_frame(world);
                let _ = renderer
                    .gpu()
                    .device
                    .poll(wgpu::PollType::wait_indefinitely());
            }
            t.elapsed().as_secs_f64() * 1000.0 / frames as f64
        };
        let ms_off = frame_ms(&mut perf, &world_off);
        let ms_on = frame_ms(&mut perf, &world_on);

        // 显存：逐探针 cubemap mip 链 + 共享擦写立方体 + 捕获深度
        let side = (PROBE_MAP_SIZE * PROBE_MAP_SIZE * 6 * 8) as f64;
        let mip_chain = side * (1.0 + 0.25 + 0.0625 + 0.015625 + 0.00390625);
        let raw = side;
        let depth = (PROBE_MAP_SIZE * PROBE_MAP_SIZE * 6 * 4) as f64;
        println!(
            "性能：烘焙 {bake_ms:.2} ms/探针｜帧时 {ms_off:.3} → {ms_on:.3} ms（{WIDTH}×{HEIGHT}，\
             {:.1}% 开销）｜显存 {:.2} MiB = {} 探针 × {:.2} MiB mip 链 + 擦写 {:.2} MiB + 深度 {:.2} MiB",
            (ms_on - ms_off) / ms_off * 100.0,
            (mip_chain * MAX_REFLECTION_PROBES as f64 + raw + depth) / (1024.0 * 1024.0),
            MAX_REFLECTION_PROBES,
            mip_chain / (1024.0 * 1024.0),
            raw / (1024.0 * 1024.0),
            depth / (1024.0 * 1024.0),
        );
    }

    let path = Path::new("iris_reflection_probe.ppm");
    write_grid(path, &panels).expect("写验收图");
    println!(
        "输出 {}（4 排 × 2 列：①室内探针 关|开 ②视差 关|开 ③粗糙度 0.1|0.8 ④光照贴图 关|开，{}×{}）",
        path.display(),
        WIDTH * 2 + GAP,
        HEIGHT * ROWS + GAP * (ROWS - 1)
    );
    println!("转 PNG：ffmpeg -y -i iris_reflection_probe.ppm docs/reflection_probe.png");
}
