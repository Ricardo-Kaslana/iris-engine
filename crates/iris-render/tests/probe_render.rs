//! 集成测试：烘焙反射探针（R62a）——局部长方体影响域 + 预滤波 cubemap +
//! 视差修正。
//!
//! 测试分层（与既有 GPU 测试同款：无 GPU 时 `eprintln!` 跳过，不 panic）：
//! 1. **兼容**：无 `ReflectionProbe` / 探针未烘焙 ⇒ 与 R61 逐位一致
//!    （同一渲染器的两个世界逐帧哈希相同——缺席语义的实现根据）；
//! 2. **有效性（朝向）**：金属球把 +X 的红板 / −X 的绿板按**镜像朝向**
//!    反射到画面的右 / 左半（写反任何一处立方体面约定都会让两侧互换）；
//! 3. **有效性（视差）**：地面（金属镜面）对天花板标记的反射落点与
//!    **解析镜像位置**逐像素对齐（关掉视差修正后落到解析的「无修正」位置，
//!    两者相距数十像素——不是「有变化」式弱断言）；
//! 4. **有效性（粗糙度 → mip）**：同一场景 roughness 0.05 / 0.9 的反射
//!    峰值饱和度下降、红色扩散面积上升（同一预滤波链的 mip 语义）；
//! 5. **确定性**：探针捕获 + 渲染的两个完整会话逐帧逐位一致；两个世界
//!    乱序插入探针时选中同一探针（权重裁决与插入序无关）；
//! 6. **光照贴图协同**（闭合 ARCHITECTURE §8.31④）：挂光照贴图的金属球
//!    在无探针时没有环境镜面（近黑），有探针时恢复为可见反射。
//!
//! **文件内 `GPU_SERIAL` 互斥串行化**：7 项测试全部走 GPU，同文件多测试
//! 并行共用单适配器会挂死（AGENTS.md 测试规约；R82 收官轮 release 全量
//! 门禁实测挂死在 `running 7 tests` 后 CPU 0% 长驻——补齐互斥后正常）。

use std::sync::Mutex;

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Entity, Transform, World};
use iris_render::prelude::*;

const SIZE: u32 = 256;

/// 文件内 GPU 互斥：同文件多测试并行共用单适配器会挂死（AGENTS.md 测试
/// 规约——症状谱：自旋挂死 / 空转死等 / 驱动侧段错误）。本文件 7 项测试
/// 全部走 GPU，**必须**串行化（与 ssr_render.rs::GPU_SERIAL 同款）。
static GPU_SERIAL: Mutex<()> = Mutex::new(());

/// FNV-1a 64 位全帧哈希（与既有 GPU 测试同款）。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 像素 (x, y) 的 RGB。
fn pixel(px: &[u8], w: u32, x: u32, y: u32) -> [i32; 3] {
    let i = ((y * w + x) * 4) as usize;
    [px[i] as i32, px[i + 1] as i32, px[i + 2] as i32]
}

/// 「明显偏红」判定：红通道压倒性高于绿蓝（阈值按发射板 (3,0,0) 的 HDR
/// 值经 ACES + sRGB 后的量级取——远高于天空 / 灰墙的色偏）。
fn is_red(p: [i32; 3]) -> bool {
    p[0] > 110 && p[0] - p[1] > 60 && p[0] - p[2] > 60
}

/// 「明显偏绿」判定（同上，绿板 (0,3,0)）。
fn is_green(p: [i32; 3]) -> bool {
    p[1] > 110 && p[1] - p[0] > 60 && p[1] - p[2] > 60
}

/// 红 / 绿像素在左右半屏的计数（`(左红, 右红, 左绿, 右绿)`）。
fn red_green_split(px: &[u8], w: u32, h: u32) -> (usize, usize, usize, usize) {
    let (mut rl, mut rr, mut gl, mut gr) = (0, 0, 0, 0);
    for y in 0..h {
        for x in 0..w {
            let p = pixel(px, w, x, y);
            if is_red(p) {
                if x < w / 2 {
                    rl += 1;
                } else {
                    rr += 1;
                }
            } else if is_green(p) {
                if x < w / 2 {
                    gl += 1;
                } else {
                    gr += 1;
                }
            }
        }
    }
    (rl, rr, gl, gr)
}

/// 红色「饱和度峰值」= 全帧 max(r − max(g, b))：镜面反射板的锐利像该值高，
/// 高粗糙度模糊后被天空 / 环境稀释 ⇒ 峰值下降（粗糙度 → mip 的观测量）。
fn red_peak(px: &[u8]) -> i32 {
    let mut best = i32::MIN;
    for chunk in px.chunks_exact(4) {
        let (r, g, b) = (chunk[0] as i32, chunk[1] as i32, chunk[2] as i32);
        best = best.max(r - g.max(b));
    }
    best
}

/// 「带红」像素数（弱阈值）：模糊会把红色摊到更大面积 ⇒ 高粗糙度下上升。
fn reddish_count(px: &[u8]) -> usize {
    px.chunks_exact(4)
        .filter(|c| {
            let (r, g, b) = (c[0] as i32, c[1] as i32, c[2] as i32);
            r - g.max(b) > 25 && r > 70
        })
        .count()
}

/// 世界骨架：目录 + 相机 + 光照 + 关泛光（确定性亮度）。
fn base_world(camera: Camera) -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(camera);
    world.insert_resource(DirectionalLight::default());
    world.insert_resource(BloomSettings::disabled());
    world
}

/// 自发光板材质（自发光叠加在光照结果上，不受光照 / 阴影影响 ⇒ 探针捕获
/// 拿到的就是该颜色的辐射度）。
fn emissive(r: f32, g: f32, b: f32) -> MaterialDef {
    let mut mat = MaterialDef::solid(r, g, b, 1.0);
    mat.emissive = [r * 3.0, g * 3.0, b * 3.0, 1.0];
    mat
}

/// 朝向场景：金属球在原点上方，±X 各一块自发光板；相机窄视角从 +Z 看球
/// （两块板都在视锥之外 ⇒ 画面里的红 / 绿只能来自球的反射）。
fn orientation_world(with_probe: bool, roughness: f32) -> (World, Option<Entity>) {
    let mut world = base_world(Camera {
        eye: Vec3::new(0.0, 1.4, 11.0),
        target: Vec3::new(0.0, 1.0, 0.0),
        fov_y_radians: 0.42,
        ..Camera::default()
    });
    let sphere = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, 48, 32));
    let mirror = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::metal(0.95, 0.95, 0.95, roughness));
    world.spawn((Transform::from_xyz(0.0, 1.0, 0.0), sphere, mirror));

    let slab = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let red = world
        .resource_mut::<MaterialStore>()
        .add(emissive(1.0, 0.0, 0.0));
    let green = world
        .resource_mut::<MaterialStore>()
        .add(emissive(0.0, 1.0, 0.0));
    world.spawn((
        Transform {
            position: Vec3::new(5.0, 1.0, 0.0),
            scale: Vec3::new(0.2, 3.0, 3.0),
            ..Transform::IDENTITY
        },
        slab,
        red,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(-5.0, 1.0, 0.0),
            scale: Vec3::new(0.2, 3.0, 3.0),
            ..Transform::IDENTITY
        },
        slab,
        green,
    ));

    let probe = with_probe.then(|| {
        world.spawn((
            Transform::from_xyz(0.0, 1.0, 0.0),
            ReflectionProbe::new(Vec3::new(8.0, 4.0, 8.0)),
        ))
    });
    propagate_transforms(&mut world);
    (world, probe)
}

/// 兼容硬约束：无组件 / 组件在场但**未烘焙**，渲染结果逐位一致（同一渲染器
/// 的两个世界——句柄布局相同，故可共用一个渲染器）。
#[test]
fn probe_absent_or_unbaked_is_bitwise_identical() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let (without, _) = orientation_world(false, 0.05);
    renderer.render(&without);
    let px_without = renderer.read_pixels();

    // 组件在场但从未烘焙：探针登记表未命中 ⇒ 不进 uniform ⇒ 同一串位
    let (unbaked, probe) = orientation_world(true, 0.05);
    assert!(probe.is_some(), "场景应挂上探针组件");
    renderer.render(&unbaked);
    let px_unbaked = renderer.read_pixels();

    assert_eq!(
        fnv1a(&px_without),
        fnv1a(&px_unbaked),
        "未烘焙的探针必须与没有探针逐位一致（哈希 {:#018x} vs {:#018x}）",
        fnv1a(&px_without),
        fnv1a(&px_unbaked)
    );
    assert_eq!(px_without, px_unbaked, "逐字节一致（不止哈希）");
    assert_eq!(
        renderer.renderer().stats().reflection_probes,
        0,
        "未烘焙探针不计入生效探针数"
    );
}

/// 有效性③：金属球把 ±X 的自发光板按镜像朝向反射到画面的右 / 左半——
/// 反射内容存在的强断言 + 朝向断言（写反立方体面约定即两侧互换）。
#[test]
fn probe_reflects_directional_markers_in_mirrored_orientation() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    // 关探针：球只反射天空 ⇒ 画面里没有任何红 / 绿
    let (plain, _) = orientation_world(false, 0.05);
    renderer.render(&plain);
    let px_plain = renderer.read_pixels();
    let (rl, rr, gl, gr) = red_green_split(&px_plain, SIZE, SIZE);
    assert_eq!(
        (rl, rr, gl, gr),
        (0, 0, 0, 0),
        "无探针时金属球只反射天空，不应出现红 / 绿反射（实际 {rl}/{rr}/{gl}/{gr}）"
    );

    // 开探针：+X 的红板出现在画面右半、−X 的绿板出现在画面左半
    let (world, probe) = orientation_world(true, 0.05);
    let probe = probe.expect("场景应挂上探针组件");
    assert!(
        renderer.renderer_mut().bake_reflection_probe(&world, probe),
        "探针烘焙应成功"
    );
    renderer.render(&world);
    let px_probe = renderer.read_pixels();
    assert_eq!(
        renderer.renderer().stats().reflection_probes,
        1,
        "已烘焙探针应在本帧生效"
    );
    let (rl, rr, gl, gr) = red_green_split(&px_probe, SIZE, SIZE);
    assert!(rr > 30, "球的右半应反射出 +X 的红板（实际右半红像素 {rr}）");
    assert_eq!(rl, 0, "红板在 +X ⇒ 左半不应出现红色（实际 {rl}）");
    assert!(gl > 30, "球的左半应反射出 −X 的绿板（实际左半绿像素 {gl}）");
    assert_eq!(gr, 0, "绿板在 −X ⇒ 右半不应出现绿色（实际 {gr}）");

    // 探针确实改变了画面（不是「没生效也不报错」）
    assert_ne!(fnv1a(&px_plain), fnv1a(&px_probe), "开探针后画面必须变化");
}

/// 有效性④：粗糙度 → mip。同一预滤波链下 roughness 0.05 的反射锐利、
/// 0.9 的被模糊：峰值饱和度下降、带红像素面积上升。
#[test]
fn probe_roughness_selects_blurrier_mip() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    // 两个粗糙度是**内容不同**的世界（同句柄布局）⇒ 必须各用一个渲染器：
    // 渲染器按句柄值缓存 GPU 材质，共用会把第一次的材质带给第二次（R53 教训）。
    let render = |roughness: f32| -> Option<Vec<u8>> {
        let mut renderer = OffscreenRenderer::new(SIZE, SIZE)?;
        let (world, probe) = orientation_world(true, roughness);
        let probe = probe.expect("场景应挂上探针组件");
        assert!(
            renderer.renderer_mut().bake_reflection_probe(&world, probe),
            "探针烘焙应成功"
        );
        renderer.render(&world);
        Some(renderer.read_pixels())
    };
    let Some(sharp) = render(0.05) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let blurry = render(0.9).expect("第二次渲染");

    let peak_sharp = red_peak(&sharp);
    let peak_blurry = red_peak(&blurry);
    let area_sharp = reddish_count(&sharp);
    let area_blurry = reddish_count(&blurry);
    println!(
        "粗糙度 → mip：峰值 {peak_sharp} → {peak_blurry}，带红面积 {area_sharp} → {area_blurry}"
    );
    assert!(
        peak_sharp > 60,
        "粗糙度 0.05 的镜面反射应有高饱和红（实际峰值 {peak_sharp}）"
    );
    assert!(
        peak_sharp - peak_blurry > 20,
        "粗糙度 0.9 走更高 mip ⇒ 峰值必须明显下降（{peak_sharp} → {peak_blurry}）"
    );
    assert!(
        area_blurry > area_sharp,
        "模糊会把红色摊到更大面积（{area_sharp} → {area_blurry}）"
    );
}

/// 确定性③：探针捕获 + 渲染的两个完整会话逐帧逐位一致。
#[test]
fn probe_two_sessions_are_frame_by_frame_bitwise() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    const FRAMES: u32 = 12;
    let run = || -> Option<Vec<u64>> {
        let mut renderer = OffscreenRenderer::new(SIZE, SIZE)?;
        let mut hashes = Vec::new();
        for frame in 0..FRAMES {
            let (mut world, probe) = orientation_world(true, 0.05);
            let probe = probe.expect("场景应挂上探针组件");
            assert!(renderer.renderer_mut().bake_reflection_probe(&world, probe));
            // 逐帧平移相机（探针位置不变 ⇒ 只有视角在动）
            {
                let mut camera = world.resource_mut::<Camera>();
                camera.eye.x = -0.6 + frame as f32 * 0.1;
            }
            propagate_transforms(&mut world);
            renderer.render(&world);
            hashes.push(fnv1a(&renderer.read_pixels()));
        }
        Some(hashes)
    };
    let Some(a) = run() else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let b = run().expect("第二次会话");
    assert_eq!(a.len(), FRAMES as usize);
    assert_eq!(
        a, b,
        "探针捕获 + 渲染的两个同输入会话必须逐帧逐位一致（确定性）"
    );
    println!(
        "探针双会话确定性：{FRAMES} 帧逐位一致（末帧 {:#018x}）",
        a[FRAMES as usize - 1]
    );
}

/// 确定性③（选择裁决）：两个世界**乱序插入**探针时选中同一探针。
///
/// 场景里两个探针重叠：A 覆盖球（权重 1），B 的过渡带盖住球（权重 < 1）。
/// 世界 1 先插 A 后插 B、世界 2 反之 —— 实体位模式随之互换，但**权重最大
/// 者恒为 A** ⇒ 两帧逐位一致（若裁决依赖插入序，世界 2 可能选中 B，
/// 而 B 的捕获位置不同 ⇒ 画面必然不同）。会话 3 只烘焙 B 作为对照：
/// 它必须与「两个都烘焙」的画面不同，证明选择确实起了作用。
#[test]
fn probe_selection_is_independent_of_insertion_order() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let scene = |first_a: bool, bake: [bool; 2]| -> Option<(Vec<u8>, u32)> {
        let mut renderer = OffscreenRenderer::new(SIZE, SIZE)?;
        let (mut world, _) = orientation_world(false, 0.05);
        let probe_a = || {
            (
                Transform::from_xyz(0.0, 1.0, 0.0),
                ReflectionProbe::new(Vec3::new(8.0, 4.0, 8.0)),
            )
        };
        let probe_b = || {
            (
                Transform::from_xyz(3.0, 1.0, 0.0),
                ReflectionProbe::new(Vec3::new(4.0, 4.0, 8.0)).with_falloff(3.0),
            )
        };
        let (ea, eb) = if first_a {
            (world.spawn(probe_a()), world.spawn(probe_b()))
        } else {
            let b = world.spawn(probe_b());
            let a = world.spawn(probe_a());
            (a, b)
        };
        propagate_transforms(&mut world);
        if bake[0] {
            assert!(renderer.renderer_mut().bake_reflection_probe(&world, ea));
        }
        if bake[1] {
            assert!(renderer.renderer_mut().bake_reflection_probe(&world, eb));
        }
        renderer.render(&world);
        let active = renderer.renderer().stats().reflection_probes;
        Some((renderer.read_pixels(), active))
    };
    let Some((both_ab, active_ab)) = scene(true, [true, true]) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let (both_ba, active_ba) = scene(false, [true, true]).expect("第二次会话");
    assert_eq!(active_ab, 2, "两个已烘焙探针都在预算内 ⇒ 都应上传");
    assert_eq!(active_ba, 2);
    assert_eq!(
        both_ab, both_ba,
        "乱序插入的两个世界必须选中同一探针（权重裁决与插入序无关）"
    );
    // 对照：只烘焙 B（捕获位置不同）⇒ 画面必须与「A、B 都烘焙」不同
    let (only_b, active_b) = scene(false, [false, true]).expect("第三次会话");
    assert_eq!(active_b, 1);
    assert_ne!(
        fnv1a(&both_ab),
        fnv1a(&only_b),
        "权重更大的 A 生效时画面应与只烘焙 B 不同（证明选择裁决确实生效）"
    );
}

// ---------------------------------------------------------------------------
// 视差修正：解析镜像位置对照
// ---------------------------------------------------------------------------

/// 视差场景参数（相机 / 探针 / 标记的解析位置都由这些常量导出）。
struct ParallaxScene {
    camera: Camera,
    /// 探针影响域中心与半边长（y 向下多留 0.05 m 使地面严格在域内：
    /// 域边界权重为 0，地面若恰在边界上就完全不吃探针）。
    probe_center: Vec3,
    probe_half: Vec3,
    /// 天花板标记（自发光板）中心。
    marker: Vec3,
}

fn parallax_scene() -> ParallaxScene {
    ParallaxScene {
        camera: Camera {
            eye: Vec3::new(0.0, 2.4, 1.6),
            target: Vec3::new(0.0, 0.2, -0.6),
            fov_y_radians: std::f32::consts::FRAC_PI_4,
            ..Camera::default()
        },
        probe_center: Vec3::new(0.0, 1.475, 0.0),
        probe_half: Vec3::new(3.0, 1.525, 3.0),
        marker: Vec3::new(0.0, 3.0, -1.0),
    }
}

/// 世界坐标 → 像素坐标（与渲染器同一条相机路径：`view_matrix` +
/// DirectX 约定的透视投影；NDC y = +1 对应贴图第 0 行）。
fn project_to_pixel(camera: &Camera, p: Vec3, w: u32, h: u32) -> (f32, f32) {
    let aspect = w as f32 / h as f32;
    let view = camera.view_matrix();
    let proj = glam::camera::rh::proj::directx::perspective(
        camera.fov_y_radians,
        aspect,
        camera.z_near,
        camera.z_far,
    );
    let clip = proj * view * p.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    (
        (ndc.x * 0.5 + 0.5) * w as f32,
        (0.5 - ndc.y * 0.5) * h as f32,
    )
}

/// 解析镜像位置：标记关于地面（y = 0）的镜像点与相机连线与地面的交点——
/// 完美的平面镜上，标记的像就落在这里。盒投影对视差修正后的采样方向
/// **恰为该点指向标记的方向**（标记在盒面上、射线在盒内不被遮挡），
/// 因此「开视差」的反射落点必须与它逐像素对齐。
fn analytic_mirror_pixel(scene: &ParallaxScene, w: u32, h: u32) -> (f32, f32) {
    let mirrored = Vec3::new(scene.marker.x, -scene.marker.y, scene.marker.z);
    let eye = scene.camera.eye;
    let dir = mirrored - eye;
    let t = -eye.y / dir.y;
    let hit = eye + dir * t;
    assert!(
        (hit.y).abs() < 1e-4,
        "镜像交点必须落在地面上（y = 0），实际 {hit:?}"
    );
    project_to_pixel(&scene.camera, hit, w, h)
}

/// 无视差修正时的解析落点：片元的反射方向必须**等于探针中心指向标记的
/// 方向**（此时采样方向不被重定向）。对地面片元 `F = (x, 0, z)`，从相机
/// `C` 出发的视线方向镜像到 +Y 后为 `(F.x − C.x, C.y, F.z − C.z)`，
/// 令其正比于 `m = marker − probe_center`，解出 F。
fn analytic_unprojected_pixel(scene: &ParallaxScene, w: u32, h: u32) -> (f32, f32) {
    let c = scene.camera.eye;
    let m = scene.marker - scene.probe_center;
    // (F.x − C.x, C.y, F.z − C.z) ∝ m ⇒ 以 y 分量定比例（m.y ≠ 0）
    assert!(m.y.abs() > 1e-3, "标记必须在探针中心上方");
    let k = c.y / m.y;
    let f = Vec3::new(c.x + m.x * k, 0.0, c.z + m.z * k);
    project_to_pixel(&scene.camera, f, w, h)
}

/// 视差场景：金属地面 + 天花板自发光标记（探针盒的顶面恰在天花板高度）。
fn parallax_world(scene: &ParallaxScene) -> (World, Entity) {
    let mut world = base_world(scene.camera.clone());
    let floor = world.resource_mut::<MeshStore>().add(MeshData::plane(12.0));
    let mirror = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::metal(0.9, 0.9, 0.9, 0.05));
    world.spawn((Transform::IDENTITY, floor, mirror));

    // 标记：小薄板贴在探针盒顶面（盒面几何 ⇒ 盒投影精确；板做小是为了让
    // 反射斑块紧凑——斑块越小，「落点在哪」的位置断言鉴别力越强）
    let slab = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let red = world
        .resource_mut::<MaterialStore>()
        .add(emissive(1.0, 0.0, 0.0));
    world.spawn((
        Transform {
            position: scene.marker,
            scale: Vec3::new(0.22, 0.04, 0.22),
            ..Transform::IDENTITY
        },
        slab,
        red,
    ));
    let probe = world.spawn((
        Transform::from_translation(scene.probe_center),
        // falloff = 0：地面在盒内 0.05 m 处，二值权重下恒为 1
        ReflectionProbe::new(scene.probe_half).with_falloff(0.0),
    ));
    propagate_transforms(&mut world);
    (world, probe)
}

/// 以 `center` 为圆心、半径 `radius` 内的「标记红」像素数。
///
/// 判定用 r ≫ g,b（自发光板经 ACES 后的强红）而非 argmax：镜面地面上还有
/// **太阳高光**（同一探针采样路径上的另一个红偏亮斑），argmax 会被它抢走；
/// 位置断言因此改问「解析落点附近有没有成片的标记红、另一处有没有」。
fn red_pixels_near(px: &[u8], w: u32, h: u32, center: (f32, f32), radius: f32) -> usize {
    let mut count = 0;
    let x0 = (center.0 - radius).max(0.0) as u32;
    let x1 = ((center.0 + radius) as u32).min(w - 1);
    let y0 = (center.1 - radius).max(0.0) as u32;
    let y1 = ((center.1 + radius) as u32).min(h - 1);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let dx = x as f32 - center.0;
            let dy = y as f32 - center.1;
            if dx * dx + dy * dy > radius * radius {
                continue;
            }
            let p = pixel(px, w, x, y);
            if p[0] > 150 && p[0] - p[1] > 80 && p[0] - p[2] > 80 {
                count += 1;
            }
        }
    }
    count
}

fn pixel_distance(a: (f32, f32), b: (u32, u32)) -> f32 {
    ((a.0 - b.0 as f32).powi(2) + (a.1 - b.1 as f32).powi(2)).sqrt()
}

/// 有效性①：视差修正的反射落点与**解析镜像位置**对齐；关掉修正后落到
/// 解析的「无重定向」位置（两者相距数十像素）。
#[test]
fn probe_parallax_matches_analytic_mirror_position() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let scene = parallax_scene();
    let (world, probe) = parallax_world(&scene);
    assert!(
        renderer.renderer_mut().bake_reflection_probe(&world, probe),
        "视差场景探针烘焙应成功"
    );

    let mirror_px = analytic_mirror_pixel(&scene, SIZE, SIZE);
    let unprojected_px = analytic_unprojected_pixel(&scene, SIZE, SIZE);
    println!(
        "解析落点：镜像 ({:.1}, {:.1})，无视差 ({:.1}, {:.1})",
        mirror_px.0, mirror_px.1, unprojected_px.0, unprojected_px.1
    );
    assert!(
        pixel_distance(mirror_px, (SIZE / 2, SIZE / 2)) < 200.0
            && pixel_distance(unprojected_px, (SIZE / 2, SIZE / 2)) < 200.0,
        "两个解析落点都应在画面内（否则断言无意义）"
    );

    // ---- 视差修正开（生产路径）----
    renderer.render(&world);
    let px_on = renderer.read_pixels();
    let on_at_mirror = red_pixels_near(&px_on, SIZE, SIZE, mirror_px, 8.0);
    let on_at_unprojected = red_pixels_near(&px_on, SIZE, SIZE, unprojected_px, 8.0);
    println!("视差开：镜像落点邻域红色 {on_at_mirror}，无重定向落点邻域 {on_at_unprojected}");
    assert!(
        on_at_mirror >= 15,
        "视差修正后标记的反射应落在解析镜像位置附近（8 像素邻域内 {on_at_mirror} 个标记红像素）"
    );
    assert_eq!(
        on_at_unprojected, 0,
        "视差修正后反射不应落在「无重定向」解析位置（实际 {on_at_unprojected}）"
    );

    // ---- 视差修正关（对照模式）----
    renderer.renderer_mut().set_legacy_probe_parallax(true);
    renderer.render(&world);
    let px_off = renderer.read_pixels();
    renderer.renderer_mut().set_legacy_probe_parallax(false);
    let off_at_mirror = red_pixels_near(&px_off, SIZE, SIZE, mirror_px, 8.0);
    let off_at_unprojected = red_pixels_near(&px_off, SIZE, SIZE, unprojected_px, 8.0);
    println!("视差关：镜像落点邻域红色 {off_at_mirror}，无重定向落点邻域 {off_at_unprojected}");
    assert_eq!(
        off_at_mirror, 0,
        "关掉视差修正后反射不应落在镜像位置（实际 {off_at_mirror}）"
    );
    assert!(
        off_at_unprojected >= 15,
        "关掉修正后反射应落在解析的无重定向位置附近（实际 {off_at_unprojected}）"
    );

    // 两个落点相距足够远 ⇒ 上述位置断言有鉴别力（不是同一片区域）
    let separation = ((mirror_px.0 - unprojected_px.0).powi(2)
        + (mirror_px.1 - unprojected_px.1).powi(2))
    .sqrt();
    println!("两个解析落点相距 {separation:.1} 像素");
    assert!(
        separation > 30.0,
        "解析的两个落点必须显著分离（实际 {separation:.1} 像素），否则断言无鉴别力"
    );
}

// ---------------------------------------------------------------------------
// 光照贴图协同（闭合 ARCHITECTURE §8.31④）
// ---------------------------------------------------------------------------

/// 带光照贴图的室内场景：地面 + 后墙挂光照贴图（真实烘焙 + `install_lightmap`），
/// 金属球悬在探针影响域内，±X 各一块自发光板（板上图案进入探针捕获）。
fn lightmap_probe_world(with_probe: bool) -> (World, Option<Entity>) {
    let mut world = base_world(Camera {
        eye: Vec3::new(0.0, 1.5, 10.0),
        target: Vec3::new(0.0, 1.0, 0.0),
        fov_y_radians: 0.5,
        ..Camera::default()
    });
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.4, -1.0, 0.2).normalize(),
        intensity: 1.6,
        ..DirectionalLight::default()
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
    // 金属球（挂光照贴图后**没有**环境镜面 —— §8.31④ 的缺口）
    let sphere = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.0, 48, 32));
    let mirror = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::metal(0.95, 0.95, 0.95, 0.05));
    world.spawn((Transform::from_xyz(0.0, 1.0, 0.0), sphere, mirror));
    // 两侧自发光板：探针开启时球面应出现它们的反射
    let slab = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let red = world
        .resource_mut::<MaterialStore>()
        .add(emissive(1.0, 0.0, 0.0));
    let green = world
        .resource_mut::<MaterialStore>()
        .add(emissive(0.0, 1.0, 0.0));
    for (x, mat) in [(4.0f32, red), (-4.0, green)] {
        world.spawn((
            Transform {
                position: Vec3::new(x, 1.5, 0.0),
                scale: Vec3::new(0.2, 2.0, 2.0),
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

    // 真烘焙 + 装配（口径与 `tests/lightmap_render.rs` 同源：128² 图集、
    // 天空压暗、64 半球样本、1 次反弹）
    let baked = bake_lightmap(
        &world,
        &BakeParams {
            light_dir: Vec3::new(-0.4, -1.0, 0.2),
            light_color: Vec3::new(1.0, 0.95, 0.85) * 1.6,
            sky_color: Vec3::new(0.12, 0.15, 0.22),
            shadow_softness: 0.08,
            sky_samples: 64,
            bounces: 1,
        },
        &BakeUvParams {
            texels_per_meter: 4.0,
            atlas_size: 128,
        },
    );
    let report = install_lightmap(&mut world, baked);
    assert!(
        report.entities >= 3,
        "地面 / 墙 / 球都应进入光照贴图（实际 {} 个实体）",
        report.entities
    );
    propagate_transforms(&mut world);
    (world, probe)
}

/// 有效性④（协同）：挂光照贴图的金属球在无探针时**没有环境镜面**（近黑，
/// §8.31④ 的缺口），探针在场时镜面由探针补回（两侧自发光板的反射出现）。
#[test]
fn probe_restores_specular_on_lightmapped_entities() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let (plain, _) = lightmap_probe_world(false);
    renderer.render(&plain);
    let px_plain = renderer.read_pixels();
    let (rl, rr, gl, gr) = red_green_split(&px_plain, SIZE, SIZE);
    assert_eq!(
        (rl, rr, gl, gr),
        (0, 0, 0, 0),
        "光照贴图实体无环境镜面 ⇒ 画面里不应有板的反射（实际 {rl}/{rr}/{gl}/{gr}）"
    );

    // 探针在另一世界（内容不同 ⇒ 换渲染器，避免句柄缓存串味）
    let (world, probe) = lightmap_probe_world(true);
    let mut probe_renderer = OffscreenRenderer::new(SIZE, SIZE).expect("第二个渲染器");
    let probe = probe.expect("场景应挂上探针组件");
    assert!(
        probe_renderer
            .renderer_mut()
            .bake_reflection_probe(&world, probe),
        "探针烘焙应成功"
    );
    probe_renderer.render(&world);
    let px_probe = probe_renderer.read_pixels();
    let (rl, rr, gl, gr) = red_green_split(&px_probe, SIZE, SIZE);
    println!("光照贴图 + 探针：rl={rl} rr={rr} gl={gl} gr={gr}");
    assert!(
        rr > 10,
        "探针应把镜面项补回给光照贴图实体（右半红像素 {rr}）"
    );
    assert!(
        gl > 10,
        "探针应把镜面项补回给光照贴图实体（左半绿像素 {gl}）"
    );
    assert_eq!(rl, 0, "红板在 +X ⇒ 左半不应出现红色（实际 {rl}）");
    assert_eq!(gr, 0, "绿板在 −X ⇒ 右半不应出现绿色（实际 {gr}）");
    // 光照贴图路径本身仍然生效（画面与无光照贴图的版本不同——
    // 只做弱对照：探针帧与无图集帧在球外区域不同即可）
    assert_ne!(fnv1a(&px_plain), fnv1a(&px_probe), "开探针后画面必须变化");
}
