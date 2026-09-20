//! 集成测试：水下 + 焦散（R94，`UnderwaterFog` + `shaders/underwater.wgsl`）。
//!
//! 七条主线（对应 ARCHITECTURE §4.53 的验收标准）：
//! 1. **缺席 / 在水上 = 逐位不变**：无资源的世界 ≡ 有资源但相机在水面之上
//!    的世界（两条缺席路径都不入链，链拓扑与 R93 逐字段相同）；
//! 2. **入水改画面**：同场景相机下到水面以下 ⇒ 全帧哈希必然不同；
//! 3. **吸收深度是活旋钮**（非默认值断言）：`absorb_depth` 2 m vs 20 m ⇒
//!    下半画面平均亮度单调（浅吸收更暗）；
//! 4. **焦散可达像素**：`caustic_strength` 0 vs 0.8 ⇒ 差分像素数过阈值
//!    （且 0 档与「不加焦散」同构）；
//! 5. **抖动可达像素**：`wobble` 0 vs 0.02 ⇒ 画面不同；
//! 6. **与体积雾同链共存**：雾 + 水下同时开启 ⇒ 与各自单开都不同、且拓扑
//!    轮转不炸（槽位读写互斥由 post.rs 不变式测试 + 本条实弹双证）；
//! 7. **确定性**：双会话逐帧逐位一致；`Time` 推进改变哈希（焦散时间项
//!    确实到达像素）。
//!
//! 环境无 GPU 适配器时优雅跳过（打印提示，测试仍算通过）。

use std::time::Duration;

use glam::Vec2;
use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Time, Transform, World};
use iris_render::prelude::*;

const SIZE: u32 = 256;

/// 新写 GPU 套件自带串行互斥（AGENTS.md 铁律）。
static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 机位：`eye_y` 决定在水上还是水下。
fn camera(eye_y: f32) -> Camera {
    Camera {
        eye: Vec3::new(0.0, eye_y, 6.0),
        target: Vec3::new(0.0, eye_y - 1.0, -20.0),
        fov_y_radians: 60f32.to_radians(),
        ..Camera::default()
    }
}

/// 场景：水面（y = 0，±24 m）+ 海床（y = −4）+ 一只亮色立柱（焦散与
/// 吸收的参照物）。`with_resource` 决定是否插 `UnderwaterFog`。
fn scene(
    eye_y: f32,
    with_resource: bool,
    water: impl FnOnce(&mut UnderwaterFog),
    fog: bool,
) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(camera(eye_y));
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.3, -1.0, -0.2).normalize(),
        color: Vec3::new(1.0, 0.97, 0.9),
        intensity: 2.4,
        ambient: Vec3::new(0.12, 0.13, 0.15),
        shadows: false,
        shadow_range: 60.0,
    });
    world.insert_resource(BloomSettings::disabled());
    if with_resource {
        let mut u = UnderwaterFog::default();
        water(&mut u);
        world.insert_resource(u);
    }
    if fog {
        world.insert_resource(FogSettings {
            enabled: true,
            density: 0.02,
            height_falloff: 0.05,
            height_offset: 0.0,
            inscatter_strength: 0.5,
            phase_g: 0.3,
            max_distance: 120.0,
        });
    }
    let bed_mesh = world
        .resource_mut::<MeshStore>()
        .add(MeshData::plane(200.0));
    let cube_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let bed_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.42, 0.4, 0.36, 1.0));
    let pillar_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.75, 0.7, 0.2, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, -4.0, -10.0),
            ..Transform::IDENTITY
        },
        bed_mesh,
        bed_mat,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(2.5, -2.0, -8.0),
            scale: Vec3::new(0.6, 4.0, 0.6),
            ..Transform::IDENTITY
        },
        cube_mesh,
        pillar_mat,
    ));
    world.spawn((WaterSurface::new(0.0, Vec2::splat(24.0)),));
    propagate_transforms(&mut world);
    world
}

fn timed(mut world: World, frames: u32) -> World {
    let mut time = Time::default();
    for _ in 0..frames {
        time.advance(Duration::from_millis(250));
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

/// 一个测试内复用的渲染器（**每测试一只设备**，见 `Gpu` 的说明）。
struct Gpu {
    renderer: OffscreenRenderer,
}

impl Gpu {
    /// 建设备。本套件的多个对照臂**共用一只渲染器**：各世界由同一
    /// `scene(...)` 构造，网格 / 材质句柄与内容逐一对应（只差资源），
    /// 正是 AGENTS.md「同句柄不同内容会串味」的**反面**——内容相同 ⇒ 复用
    /// 安全。不这么做的话单文件 30+ 只设备会让驱动侧适配器请求在第五六个
    /// 测试返回 `None`（实测：单独跑每个测试都过，整文件跑第 5 个开始建不出
    /// 设备——wgpu/DX12 的设备释放是延迟的，进程内不回收）。
    fn new() -> Self {
        Self {
            renderer: OffscreenRenderer::new(SIZE, SIZE).expect("环境应有 GPU 适配器"),
        }
    }
    fn render(&mut self, world: &World) -> Vec<u8> {
        for _ in 0..2 {
            self.renderer.render(world);
        }
        self.renderer.read_pixels()
    }
}

/// 画面下半（水下主导区）平均亮度。
fn lower_mean_luma(px: &[u8]) -> f64 {
    let mut sum = 0.0f64;
    let mut n = 0.0f64;
    for y in (SIZE / 2)..SIZE {
        for x in 0..SIZE {
            let i = ((y * SIZE + x) * 4) as usize;
            // u8 直接相加会在 debug 下溢出 panic（255+255 > u8::MAX）
            // ——先升 u32 再求和（R92 排障同款陷阱，测试自身也要过门禁）。
            sum += (px[i] as u32 + px[i + 1] as u32 + px[i + 2] as u32) as f64 / 3.0;
            n += 1.0;
        }
    }
    sum / n
}

fn count_changed(a: &[u8], b: &[u8]) -> usize {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .filter(|(x, y)| (0..3).any(|i| (x[i] as i32 - y[i] as i32).abs() >= 4))
        .count()
}

fn skip_if_no_gpu() -> bool {
    if OffscreenRenderer::new(8, 8).is_none() {
        eprintln!("跳过：环境无 GPU 适配器");
        return true;
    }
    false
}

#[test]
fn absent_or_above_water_is_bitwise_inert() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if skip_if_no_gpu() {
        return;
    }
    let mut gpu = Gpu::new();
    let none = gpu.render(&timed(scene(2.0, false, |_| {}, false), 4));
    let above = gpu.render(&timed(scene(2.0, true, |_| {}, false), 4));
    assert_eq!(
        fnv1a(&none),
        fnv1a(&above),
        "资源在场但相机在水面之上 ⇒ 必须与无资源逐位一致（两条件缺一不入链）"
    );
    // 判别力旁证：同一资源、相机下水 ⇒ 必须不同（否则上条等式是恒真）
    let below = gpu.render(&timed(scene(-1.5, true, |_| {}, false), 4));
    assert_ne!(fnv1a(&none), fnv1a(&below), "入水必须改变画面");
}

/// 入水必须真实改变画面，且**纯吸收档**下压暗（默认档含内散射 / Snell 窗 /
/// 焦散三处加性项，实测反而略亮——那是路径辐射 `bg·T + L_ins·(1−T)` 的正常
/// 行为，不能拿它当「吸收生效」的判据，故这里把加性项全关）。
#[test]
fn submerged_camera_changes_frame_and_absorption_darkens() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if skip_if_no_gpu() {
        return;
    }
    let mut gpu = Gpu::new();
    let dry = gpu.render(&timed(scene(-1.5, false, |_| {}, false), 4));
    let plain = |u: &mut UnderwaterFog| {
        u.sun_inscatter = 0.0;
        u.sky_window = 0.0;
        u.caustic_strength = 0.0;
        u.wobble = 0.0;
    };
    let wet = gpu.render(&timed(scene(-1.5, true, plain, false), 4));
    assert_ne!(fnv1a(&dry), fnv1a(&wet), "入水必须改变画面");
    assert!(
        count_changed(&dry, &wet) > SIZE as usize * SIZE as usize / 8,
        "水下 pass 是全屏项，差分应覆盖大片画面，实得 {}",
        count_changed(&dry, &wet)
    );
    assert!(
        lower_mean_luma(&wet) < lower_mean_luma(&dry),
        "纯吸收档必须压暗下半画面：{} < {}",
        lower_mean_luma(&wet),
        lower_mean_luma(&dry)
    );
}

#[test]
fn absorb_depth_knob_is_monotone() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if skip_if_no_gpu() {
        return;
    }
    // **隔离混淆源**：吸收档必须关掉内散射 / Snell 窗 / 焦散 / 抖动——
    // 否则「越浑越暗」不成立（首稿实测反例：absorb_depth 1.5 m 的画面反而
    // 比 40 m 亮 2 级，因为路径辐射 `L = bg·T + inscatter·(1−T)` 的第二次
    // 项随消光增大而抬亮，把吸收的变暗完全吃掉——这是物理正确的行为，
    // 不是 bug，但用它当吸收判据就是错的指标）。
    let mut gpu = Gpu::new();
    let clear = gpu.render(&timed(
        scene(
            -1.5,
            true,
            |u| {
                u.absorb_depth = 40.0;
                u.sun_inscatter = 0.0;
                u.sky_window = 0.0;
                u.caustic_strength = 0.0;
                u.wobble = 0.0;
            },
            false,
        ),
        4,
    ));
    let murky = gpu.render(&timed(
        scene(
            -1.5,
            true,
            |u| {
                u.absorb_depth = 1.5;
                u.sun_inscatter = 0.0;
                u.sky_window = 0.0;
                u.caustic_strength = 0.0;
                u.wobble = 0.0;
            },
            false,
        ),
        4,
    ));
    let clear_luma = lower_mean_luma(&clear);
    let murky_luma = lower_mean_luma(&murky);
    assert!(
        murky_luma < clear_luma,
        "纯吸收档下特征深度越小（越浑）画面越暗：{murky_luma} < {clear_luma}"
    );
    assert_ne!(fnv1a(&clear), fnv1a(&murky), "absorb_depth 须是活旋钮");
    // 吸收色通道的方向性：蓝水（蓝高红低）与绿水（绿高红低）必须在
    // R−B 符号上翻转——单看亮度会漏掉「色相没生效」这一类错。
    let blue = gpu.render(&timed(
        scene(
            -1.5,
            true,
            |u| {
                u.absorb_depth = 2.0;
                u.color = Vec3::new(0.01, 0.05, 0.30);
                u.sun_inscatter = 0.0;
                u.sky_window = 0.0;
                u.caustic_strength = 0.0;
                u.wobble = 0.0;
            },
            false,
        ),
        4,
    ));
    let green = gpu.render(&timed(
        scene(
            -1.5,
            true,
            |u| {
                u.absorb_depth = 2.0;
                u.color = Vec3::new(0.05, 0.30, 0.02);
                u.sun_inscatter = 0.0;
                u.sky_window = 0.0;
                u.caustic_strength = 0.0;
                u.wobble = 0.0;
            },
            false,
        ),
        4,
    ));
    let rb = |px: &[u8]| -> f64 {
        let i = ((SIZE * 3 / 4 * SIZE + SIZE / 2) * 4) as usize;
        (px[i] as f64) - (px[i + 2] as f64)
    };
    assert!(
        rb(&blue) < rb(&green),
        "吸收色必须体现在 R−B 差上（蓝水更负）：{} vs {}",
        rb(&blue),
        rb(&green)
    );
}

#[test]
fn caustics_and_wobble_reach_pixels() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if skip_if_no_gpu() {
        return;
    }
    let mut gpu = Gpu::new();
    let base = gpu.render(&timed(
        scene(
            -1.5,
            true,
            |u| {
                u.caustic_strength = 0.0;
                u.wobble = 0.0;
            },
            false,
        ),
        4,
    ));
    let caustic = gpu.render(&timed(
        scene(-1.5, true, |u| u.caustic_strength = 1.2, false),
        4,
    ));
    let wob = gpu.render(&timed(
        scene(
            -1.5,
            true,
            |u| {
                u.caustic_strength = 0.0;
                u.wobble = 0.03;
            },
            false,
        ),
        4,
    ));
    assert_ne!(fnv1a(&base), fnv1a(&caustic), "焦散强度须到达像素");
    assert!(
        count_changed(&base, &caustic) > 500,
        "焦散是空间变化的亮筋，差分像素数须可判，实得 {}",
        count_changed(&base, &caustic)
    );
    assert!(
        lower_mean_luma(&caustic) >= lower_mean_luma(&base) - 1e-6,
        "焦散是加性项 ⇒ 只能抬亮，不能压暗"
    );
    assert_ne!(fnv1a(&base), fnv1a(&wob), "抖动幅度须到达像素");
}

#[test]
fn fog_and_underwater_coexist_in_chain() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if skip_if_no_gpu() {
        return;
    }
    let mut gpu = Gpu::new();
    let fog_only = gpu.render(&timed(scene(-1.5, false, |_| {}, true), 4));
    let uw_only = gpu.render(&timed(scene(-1.5, true, |_| {}, false), 4));
    let both = gpu.render(&timed(scene(-1.5, true, |_| {}, true), 4));
    // 三级链（链源 → 雾 → 水下 → …）下三者互不相同 ⇒ 槽位轮转没有把某一级
    // 的输出读成自己的输入（同槽读写会直接 wgpu 校验失败或画面错乱）。
    let (a, b, c) = (fnv1a(&fog_only), fnv1a(&uw_only), fnv1a(&both));
    assert_ne!(a, b, "雾与水下是两个不同的效果");
    assert_ne!(a, c, "两者同开须不同于只开雾");
    assert_ne!(b, c, "两者同开须不同于只开水下");
}

#[test]
fn underwater_is_bitwise_deterministic_and_time_moves_it() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    if skip_if_no_gpu() {
        return;
    }
    let mut r1 = OffscreenRenderer::new(SIZE, SIZE).unwrap();
    let mut r2 = OffscreenRenderer::new(SIZE, SIZE).unwrap();
    let mut hashes = Vec::new();
    for frame in 0..5u32 {
        let world = timed(scene(-1.5, true, |_| {}, false), frame + 1);
        r1.render(&world);
        r2.render(&world);
        let a = r1.read_pixels();
        let b = r2.read_pixels();
        assert_eq!(fnv1a(&a), fnv1a(&b), "第 {frame} 帧双会话必须逐位一致");
        hashes.push(fnv1a(&a));
    }
    assert!(
        hashes.windows(2).any(|w| w[0] != w[1]),
        "时间推进必须改变画面（焦散 / 抖动的时间项没到达像素即为假绿）"
    );
}
