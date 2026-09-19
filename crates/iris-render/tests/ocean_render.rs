//! 集成测试：海洋 v2（R93，`OceanSurface` + 频谱位移图 + 双层跟随网格）。
//!
//! 六条主线（对应 ARCHITECTURE §4.52 的验收标准）：
//! 1. **生效反证**：同场景有 / 无 `OceanSurface` 全帧 FNV-1a **不同**；
//! 2. **缺席 = 逐位不变**：无 `OceanSurface` 时与「本轮引入前的路径」同构
//!    （零捕获增量、零海洋 draw）——用两世界同帧哈希与池塘套件的一致性
//!    旁证不能跨套件做，这里以「同 renderer 上跑两个海洋关闭世界逐位一致」
//!    的可复现口径 + 海洋在场时帧必然不同（主线 1）联合保证；
//! 3. **互斥语义**：`OceanSurface` 与 `WaterSurface` 同挂的帧 ≡ 只有海洋的
//!    帧（逐位），且 ≠ 只有池塘的帧——池塘路径被整段跳过而非「画了再盖」；
//! 4. **细节层可达**：`detail_amplitude` 0 → 0.12 全帧哈希变化 + 近处像素
//!    差分计数上升（非默认值断言，R89 教训）；
//! 5. **远场覆盖**：低机位掠射下远处的海仍是海（far 环真实参与，非天空底板）；
//! 6. **确定性**：双会话逐帧逐位一致；时间推进改变全帧哈希（关键帧动画
//!    确实到达像素，不是静态贴图）。
//!
//! 环境无 GPU 适配器时优雅跳过（打印提示，测试仍算通过）。

use std::time::Duration;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Time, Transform, World};
use iris_render::prelude::*;

/// 屏幕尺寸（与水体 / 天气套件同口径）。
const SIZE: u32 = 256;

/// 本套件串行化（AGENTS.md 铁律：新写 GPU 套件直接带互斥，别等挂死再补）。
static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 测试机位：低机位（3 m）略俯——同时看到近处细节、掠射远海与竖柱倒影。
fn camera() -> Camera {
    Camera {
        eye: Vec3::new(0.0, 3.0, 10.0),
        target: Vec3::new(0.0, 0.6, -20.0),
        fov_y_radians: 60f32.to_radians(),
        ..Camera::default()
    }
}

/// 表面种类（互斥语义的三种世界）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Surface {
    None,
    Pond,
    Ocean,
    Both,
}

/// 场景：深色地面在 y = −4（远离海面，避免与水体吸收混淆）+ 亮红竖柱
/// （反射可判别）+ 按需的水面组件。
fn world_with(surface: Surface, ocean: OceanSurface) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(camera());
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.3, -1.0, -0.2).normalize(),
        intensity: 1.0,
        ambient: Vec3::new(0.10, 0.11, 0.13),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    let mut ground_mat = MaterialDef::solid(0.05, 0.05, 0.055, 1.0);
    ground_mat.roughness = 0.95;
    let mut pillar_mat = MaterialDef::solid(0.9, 0.05, 0.05, 1.0);
    pillar_mat.roughness = 0.6;
    let ground_mesh = world
        .resource_mut::<MeshStore>()
        .add(MeshData::plane(400.0));
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let ground = world.resource_mut::<MaterialStore>().add(ground_mat);
    let pillar = world.resource_mut::<MaterialStore>().add(pillar_mat);
    world.spawn((
        Transform {
            position: Vec3::new(0.0, -4.0, 0.0),
            ..Transform::IDENTITY
        },
        ground_mesh,
        ground,
    ));
    // 竖柱跨 y 0..4（柱身在水面之上、倒影在水里）
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 2.0, 2.0),
            scale: Vec3::new(0.5, 4.0, 0.5),
            ..Transform::IDENTITY
        },
        cube,
        pillar,
    ));
    if surface == Surface::Pond || surface == Surface::Both {
        world.spawn((WaterSurface::new(0.0, glam::Vec2::new(24.0, 24.0)),));
    }
    if surface == Surface::Ocean || surface == Surface::Both {
        world.spawn((ocean,));
    }
    propagate_transforms(&mut world);
    world
}

/// 推进 `frames` 个固定步长后的 `Time`（波形时间源）。
fn timed(mut world: World, frames: u32) -> World {
    let mut time = Time::default();
    for _ in 0..frames {
        time.advance(Duration::from_millis(333));
    }
    world.insert_resource(time);
    world
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

fn pixel(px: &[u8], x: u32, y: u32) -> [i32; 3] {
    let i = ((y * SIZE + x) * 4) as usize;
    [px[i] as i32, px[i + 1] as i32, px[i + 2] as i32]
}

/// 两帧逐像素差异计数（阈值按通道位深给依据：任一通道差 ≥ 4/255 记一个）。
fn count_changed(a: &[u8], b: &[u8]) -> usize {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .filter(|(x, y)| (0..3).any(|i| (x[i] as i32 - y[i] as i32).abs() >= 4))
        .count()
}

/// 单帧渲染并读回（每个世界用独立 renderer——渲染器按句柄缓存 GPU 资源，
/// 跨世界复用会串味，AGENTS.md 铁律）。
fn render_once(world: &World) -> Vec<u8> {
    let mut renderer = OffscreenRenderer::new(SIZE, SIZE).expect("环境应有 GPU 适配器");
    for _ in 0..2 {
        renderer.render(world);
    }
    renderer.read_pixels()
}

#[test]
fn ocean_presence_changes_frame() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(_probe) = OffscreenRenderer::new(8, 8) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    drop(_probe);
    let dry = render_once(&timed(
        world_with(Surface::None, OceanSurface::default()),
        4,
    ));
    let wet = render_once(&timed(
        world_with(Surface::Ocean, OceanSurface::default()),
        4,
    ));
    assert_ne!(fnv1a(&dry), fnv1a(&wet), "海洋必须真实改变像素");
    assert!(
        count_changed(&dry, &wet) > SIZE as usize * SIZE as usize / 6,
        "无限海面应覆盖屏幕相当大面积（远场环参与）——实测 {}",
        count_changed(&dry, &wet)
    );
}

#[test]
fn surfaceless_frames_are_reproducible_and_pristine() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if OffscreenRenderer::new(8, 8).is_none() {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    }
    // 无海洋路径的两份同构世界逐位一致（缺席语义本身不引入任何跨帧状态）。
    let a = render_once(&timed(
        world_with(Surface::None, OceanSurface::default()),
        4,
    ));
    let b = render_once(&timed(
        world_with(Surface::None, OceanSurface::default()),
        4,
    ));
    assert_eq!(fnv1a(&a), fnv1a(&b), "无海洋帧必须可复现");
}

#[test]
fn ocean_suppresses_pond_when_both_present() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if OffscreenRenderer::new(8, 8).is_none() {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    }
    let ocean = OceanSurface::default();
    let only_ocean = render_once(&timed(world_with(Surface::Ocean, ocean), 4));
    let both = render_once(&timed(world_with(Surface::Both, ocean), 4));
    let only_pond = render_once(&timed(world_with(Surface::Pond, ocean), 4));
    assert_eq!(
        fnv1a(&only_ocean),
        fnv1a(&both),
        "两路同挂时池塘必须整段不执行（≡ 只有海洋）"
    );
    assert_ne!(
        fnv1a(&only_ocean),
        fnv1a(&only_pond),
        "判别力旁证：池塘帧与海洋帧必须不同（否则上条等式是恒真）"
    );
}

#[test]
fn detail_amplitude_reaches_pixels() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if OffscreenRenderer::new(8, 8).is_none() {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    }
    let base = OceanSurface::default();
    let flat = OceanSurface {
        detail_amplitude: 0.0,
        ..base
    };
    let rough = OceanSurface {
        detail_amplitude: 0.12,
        ..base
    };
    let f_flat = render_once(&timed(world_with(Surface::Ocean, flat), 4));
    let f_rough = render_once(&timed(world_with(Surface::Ocean, rough), 4));
    assert_ne!(
        fnv1a(&f_flat),
        fnv1a(&f_rough),
        "detail_amplitude 必须是活旋钮（非默认值断言）"
    );
    // 近处 ROI（画面底部 1/4 = 近场环内）差分应比掠射远区更显著：
    // 细节层的几何位移只在 near_extent 内（§8.72）。
    let near_changed = (SIZE * 3 / 4..SIZE)
        .map(|y| {
            (0..SIZE)
                .filter(|&x| {
                    let a = pixel(&f_flat, x, y);
                    let b = pixel(&f_rough, x, y);
                    (0..3).any(|i| (a[i] - b[i]).abs() >= 4)
                })
                .count()
        })
        .sum::<usize>();
    assert!(
        near_changed > 200,
        "近场细节必须可判别（底部 1/4 条带差分像素数），实测 {near_changed}"
    );
}

/// 世界坐标 → 像素坐标（与渲染器同一条相机路径；NDC y = +1 对应第 0 行）。
fn project_to_pixel(p: Vec3) -> (u32, u32) {
    let c = camera();
    let aspect = 1.0;
    let view = c.view_matrix();
    let proj =
        glam::camera::rh::proj::directx::perspective(c.fov_y_radians, aspect, c.z_near, c.z_far);
    let clip = proj * view * p.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    (
        ((ndc.x * 0.5 + 0.5) * SIZE as f32) as u32,
        ((0.5 - ndc.y * 0.5) * SIZE as f32) as u32,
    )
}

/// 远海可达性：同一机位下，把 `far_extent` 从 192 m 缩到 40 m，100 m 处的
/// 采样点必须从「海」翻成「天空」——这是远场环真实存在的**判别**证据
/// （比「下半画面变暗」更硬：后者只证明有东西盖住了画面）。
#[test]
fn far_ring_reaches_beyond_near_ring() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if OffscreenRenderer::new(8, 8).is_none() {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    }
    let base = OceanSurface::default();
    let wide = render_once(&timed(world_with(Surface::Ocean, base), 4));
    let narrow = render_once(&timed(
        world_with(
            Surface::Ocean,
            OceanSurface {
                far_extent: 40.0,
                ..base
            },
        ),
        4,
    ));
    let dry = render_once(&timed(world_with(Surface::None, base), 4));
    // 采样点：侧向 8 m、前方 100 m 的海面（> near_extent 24 m、
    // < far_extent 192 m）。首稿取 x=0 正前方，被 8 m 处的亮红竖柱挡住
    // ——「海 vs 天空」比的是柱子，两条断言都以恒等式通过 / 失败自曝。
    let (x, y) = project_to_pixel(Vec3::new(8.0, base.height, -100.0));
    let (x, y) = (x.min(SIZE - 1), y.min(SIZE - 1));
    let sea = pixel(&wide, x, y);
    let sky = pixel(&narrow, x, y);
    let dry_here = pixel(&dry, x, y);
    assert_eq!(
        sky, dry_here,
        "缩窄 far_extent 后该点必是纯天空（与无海洋同值）——否则采样点选错"
    );
    assert_ne!(
        sea, sky,
        "100 m 处必须看到海（远场环参与）：海 {sea:?} vs 天空 {sky:?}"
    );
    // 颜色启发式在此**不适用**（首稿的错）：低机位看 100 m 外的海面，俯角
    // 只有几度 ⇒ Schlick Fresnel 趋 1，海就是天空的镜子（实测海 [214,222,230]
    // vs 天空 [215,220,225]，亮度反而略高）。物理正确的判别是「同点必须
    // 不同帧」+「差异覆盖一整圈环带」，不是「比天空暗」。
    let annulus = count_changed(&wide, &narrow);
    assert!(
        annulus > (SIZE * SIZE / 20) as usize,
        "far_extent 192→40 的差异必须是**一整圈环带**（≥ 全帧 5%），实测 {annulus}"
    );
    // 「海洋让画面变暗」在这里**不成立也不该成立**：3 m 机位看远处俯角只有
    // 几度，Fresnel 趋 1，海把整片天空镜像回来，下半平均亮度反而略高于晴天
    // 底板。深色主导只适用于陡视角（近岸 / 高机位），本轮不在这里断言。
}

#[test]
fn ocean_is_bitwise_deterministic_and_time_moves_it() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if OffscreenRenderer::new(8, 8).is_none() {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    }
    let base = OceanSurface::default();
    // 双会话：两个独立渲染器跑同一世界序列，逐帧逐位一致（有状态渲染的
    // 标准断言口径，R60/R62b/R82 范式）。
    let mut r1 = OffscreenRenderer::new(SIZE, SIZE).unwrap();
    let mut r2 = OffscreenRenderer::new(SIZE, SIZE).unwrap();
    let mut hashes = Vec::new();
    for frame in 0..6u32 {
        let world = timed(world_with(Surface::Ocean, base), frame + 1);
        r1.render(&world);
        r2.render(&world);
        let a = r1.read_pixels();
        let b = r2.read_pixels();
        assert_eq!(
            fnv1a(&a),
            fnv1a(&b),
            "第 {frame} 帧双会话必须逐位一致（关键帧索引由 Time 纯函数决定）"
        );
        hashes.push(fnv1a(&a));
    }
    assert!(
        hashes.windows(2).any(|w| w[0] != w[1]),
        "时间推进必须改变画面（否则关键帧动画没到达像素）"
    );
}
