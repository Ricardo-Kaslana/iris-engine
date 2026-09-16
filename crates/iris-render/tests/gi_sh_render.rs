//! 集成测试：GI 探针三阶 SH 逐片元化（R83，`IrradianceProbeSet` 带 SH ⇒
//! binding 18/19 + `standard.wgsl` 的 `gi_sh_irradiance` 支）。
//!
//! 五条主线（与 ARCHITECTURE §4.39 的验收标准一一对应）：
//! 1. **缺席 = 逐位不变（硬约束）**：探针集资源缺席 / 空集 / 单探针退化集
//!    （三者都 ⇒ mode = 0 + 载荷恒零）三次渲染 read_pixels **逐字节相等**
//!    ——新增 binding 18/19 的哑缓冲与 mode 均匀门没有扰动既有路径；
//! 2. **空间梯度（§8.40① 的头条断言）**：一个 4 m 高的箱体横跨「红区探针」
//!    与「绿区探针」，SH 模式下箱底偏红、箱顶偏绿，而同一探针云的 v1 逐实体
//!    模式（无 SH）上下**同值**（跨模式同像素作差 ⇒ 镜面项 / 天空项抵消，
//!    剩下的只有间接漫反射项的差异）；
//! 3. **方向性（§8.40②）**：同一探针内「常量场」vs「+Y 亮 / −Y 暗的 L1 场」
//!    两次渲染，球体上半部显著变亮、下半部显著变暗——v1 的无方向标量（六轴
//!    平均）表达不了这个信号（同 L0 ⇒ v1 模式两次渲染逐位一致）；
//! 4. **真实烘焙贯通**：`bake_irradiance_probes_sh` 烘出的两层异色板场景
//!    （红地板 / 绿天花板），球体顶部偏绿、底部偏红（CPU 侧
//!    `sh_irradiance_at` 对照，两条独立实现互证）；
//! 5. **确定性**：同场景双会话逐字节一致；改单个探针的 SH / 位置 ⇒ 全帧
//!    哈希变化（防「恒等假通过」）。
//!
//! 环境无 GPU 适配器时优雅跳过（打印提示，测试仍算通过）。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 屏幕尺寸（测试统一口径）。
const SIZE: u32 = 256;

/// 本套件串行化（ssr_render 同款纪律）：同文件多测试并行共用单适配器会
/// 挂死——文件内互斥锁把并发度压到 1。
static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 公共相机：正对场景中心（+Z 方向看 −Z），采样点的法线都朝向相机。
fn scene_camera() -> Camera {
    Camera {
        eye: Vec3::new(0.0, 2.0, 7.0),
        target: Vec3::new(0.0, 2.0, 0.0),
        fov_y_radians: std::f32::consts::FRAC_PI_3,
        ..Camera::default()
    }
}

/// 隔离场景：直射光强度 0 + 环境常量近零 ⇒ 画面里几乎全部照明都来自
/// 间接漫反射项（GI 探针）与 env 镜面项；跨模式同像素作差把后者抵消。
fn bare_world() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(scene_camera());
    world.insert_resource(DirectionalLight {
        direction: Vec3::new(-0.4, -1.0, -0.3).normalize(),
        intensity: 0.0,
        ambient: Vec3::new(0.01, 0.01, 0.012),
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    world
}

/// 灰白粗糙材质（metallic = 0 ⇒ kd_amb 接近 1；roughness = 1 ⇒ 镜面项取
/// 预滤波最高 mip，跨采样点变化最小）。
fn gray_material(world: &mut World) -> MaterialHandle {
    let mut mat = MaterialDef::solid(0.82, 0.82, 0.84, 1.0);
    mat.roughness = 1.0;
    mat.metallic = 0.0;
    world.resource_mut::<MaterialStore>().add(mat)
}

/// 「常量场」SH 系数：`E/π(n) ≡ c`（只有 L0 项，d_0 = c / Y_0）。
fn const_sh(c: Vec3) -> ShCoeffs {
    let mut out = [[0.0f32; 3]; SH_COEFFS];
    out[0] = [c.x / SH_Y0, c.y / SH_Y0, c.z / SH_Y0];
    out
}

/// 「常量 + y 向偶极」SH 系数：`E/π(n) = a + b·n_y`
/// （d_0 = a / Y_0、d_1 = b / Y_1——L1 的序是 y, z, x ⇒ y 项是 k = 1）。
fn dipole_sh(a: Vec3, b: Vec3) -> ShCoeffs {
    let mut out = const_sh(a);
    out[1] = [b.x / SH_Y1, b.y / SH_Y1, b.z / SH_Y1];
    out
}

/// 世界坐标 → 像素坐标（与渲染器同一条相机路径；NDC y = +1 对应第 0 行）。
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

fn pixel(px: &[u8], w: u32, x: u32, y: u32) -> [i32; 3] {
    let i = ((y * w + x) * 4) as usize;
    [px[i] as i32, px[i + 1] as i32, px[i + 2] as i32]
}

fn luminance(p: [i32; 3]) -> i32 {
    p[0] + p[1] + p[2]
}

/// 全帧像素 FNV-1a 64（兼容锁哈希，与既有 GPU 测试同款）。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

fn sample_at(px: &[u8], at: Vec3) -> [i32; 3] {
    let (fx, fy) = project_to_pixel(&scene_camera(), at, SIZE, SIZE);
    let (x, y) = (fx as u32, fy as u32);
    assert!(x < SIZE && y < SIZE, "采样点 ({fx},{fy}) 越界");
    pixel(px, SIZE, x, y)
}

// ---------------------------------------------------------------------------
// 1. 缺席 = 逐位不变
// ---------------------------------------------------------------------------

/// 硬约束：三种「mode = 0 且载荷恒零」的世界（资源缺席 / 空集 / 单探针
/// 退化集）必须渲染出**逐字节相同**的画面——新增的 binding 18/19 哑缓冲与
/// mode 均匀门对既有路径零扰动。
///
/// 注：「带 SH 的集 + mode = 0」不构成逐位对照对——SH 集的 loc14 载荷口径
/// 本身换了（零阶 SH 混合而不是四面体插值，见 irradiance.rs 的
/// `instance_payload`），这是设计而非回归；既有 v1 FNV 基线由
/// `tests/irradiance_render.rs` 原值看门。
#[test]
fn absent_empty_and_degenerate_sets_are_bitwise_identical() {
    let _gpu = GPU_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let build = |kind: u32| -> World {
        let mut world = bare_world();
        let mat = gray_material(&mut world);
        let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
        world.spawn((
            Transform {
                position: Vec3::new(0.0, 2.0, 0.0),
                scale: Vec3::new(1.0, 4.0, 1.0),
                ..Transform::IDENTITY
            },
            cube,
            mat,
        ));
        match kind {
            0 => {} // 资源缺席
            1 => {
                world.insert_resource(IrradianceProbeSet::new());
            }
            _ => {
                // 单探针：四面体化为空 ⇒ 载荷恒 [0; 4]（v1 语义）
                world.insert_resource(IrradianceProbeSet::from_probes(
                    vec![Vec3::new(0.0, 1.0, 0.0)],
                    vec![Vec3::splat(2.0)],
                ));
            }
        }
        propagate_transforms(&mut world);
        world
    };

    let Some(mut a) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut hashes = Vec::new();
    for kind in 0..3u32 {
        let mut renderer = if kind == 0 {
            a
        } else {
            OffscreenRenderer::new(SIZE, SIZE).expect("渲染器")
        };
        renderer.render(&build(kind));
        let px = renderer.read_pixels();
        assert_eq!(px.len(), (SIZE * SIZE * 4) as usize);
        hashes.push(fnv1a(&px));
        a = renderer;
    }
    assert_eq!(hashes[0], hashes[1], "空集必须与资源缺席逐位一致");
    assert_eq!(hashes[1], hashes[2], "退化单探针集必须与空集逐位一致");

    // 防恒等假通过：装上带 SH 的集（mode = 1）后画面必须改变
    let mut world_sh = build(0);
    world_sh.insert_resource(IrradianceProbeSet::from_probes_sh(
        vec![Vec3::new(0.0, 1.0, 0.0)],
        vec![Vec3::splat(2.0)],
        vec![const_sh(Vec3::new(0.9, 0.2, 0.15))],
        vec![f32::INFINITY],
    ));
    a.render(&world_sh);
    assert_ne!(
        fnv1a(&a.read_pixels()),
        hashes[0],
        "带 SH 的探针集（mode = 1）必须改变画面"
    );
}

// ---------------------------------------------------------------------------
// 2. 空间梯度（§8.40① 的头条断言）
// ---------------------------------------------------------------------------

/// 四探针云（三个「红区」低探针 + 一个「绿区」高探针，构成合法四面体）与
/// 一个 4 m 高的箱体（y ∈ [0, 4]，横跨两区）。采样点在箱体正面（z = +0.51）：
/// 箱底 (0, 0.4, ·) 只被红区探针覆盖、箱顶 (0, 3.6, ·) 只被绿区探针覆盖
/// （半径 1.8 ⇒ 到对侧探针 > 3 m，权重恒 0）。
fn gradient_positions() -> Vec<Vec3> {
    vec![
        Vec3::new(-1.2, 0.5, -1.2),
        Vec3::new(1.2, 0.5, -1.2),
        Vec3::new(0.0, 0.5, 1.2),
        Vec3::new(0.0, 3.5, 0.0),
    ]
}

/// 箱底 / 箱顶采样点（正面 z = 0.51，法线 +Z ⇒ 常量场 SH 下方向无关）。
const BOX_BOTTOM: Vec3 = Vec3::new(0.0, 0.4, 0.51);
const BOX_TOP: Vec3 = Vec3::new(0.0, 3.6, 0.51);

/// 空间梯度：SH 模式下箱底偏红、箱顶偏绿；v1 模式（同一探针云、无 SH）上下
/// **同值**。断言用「跨模式同像素作差」——镜面项 / 天空项 / 色调映射在两次
/// 渲染里逐像素相同，作差后只剩间接漫反射项的变化（阈值依据见断言处）。
#[test]
fn per_fragment_sh_gives_the_box_a_spatial_gradient() {
    let _gpu = GPU_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let positions = gradient_positions();
    let radii = vec![1.8f32; 4];
    // CPU 侧口径检查（先证权重设计成立，再看 GPU）：箱底只被红探针覆盖、
    // 箱顶只被绿探针覆盖。
    let red = Vec3::new(1.0, 0.05, 0.05);
    let green = Vec3::new(0.05, 1.0, 0.1);
    let sh = vec![const_sh(red), const_sh(red), const_sh(red), const_sh(green)];
    let cpu_bottom = sh_blend(&positions, &radii, &sh, BOX_BOTTOM, Vec3::Z).unwrap();
    let cpu_top = sh_blend(&positions, &radii, &sh, BOX_TOP, Vec3::Z).unwrap();
    assert!(
        (cpu_bottom - red).abs().max_element() < 1e-5,
        "箱底的 CPU 求值必须 = 红探针值：{cpu_bottom:?}"
    );
    assert!(
        (cpu_top - green).abs().max_element() < 1e-5,
        "箱顶的 CPU 求值必须 = 绿探针值：{cpu_top:?}"
    );

    let build = |with_sh: bool| -> World {
        let mut world = bare_world();
        let mat = gray_material(&mut world);
        let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
        world.spawn((
            Transform {
                position: Vec3::new(0.0, 2.0, 0.0),
                scale: Vec3::new(1.0, 4.0, 1.0),
                ..Transform::IDENTITY
            },
            cube,
            mat,
        ));
        // v1 值 = 两区平均（无方向标量）；两个世界的探针位置 / 半径同构
        let values = vec![Vec3::splat(0.5 * std::f32::consts::PI); 4];
        let set = if with_sh {
            IrradianceProbeSet::from_probes_sh(positions.clone(), values, sh.clone(), radii.clone())
        } else {
            IrradianceProbeSet::from_probes(positions.clone(), values)
        };
        world.insert_resource(set);
        propagate_transforms(&mut world);
        world
    };

    let Some(mut renderer_sh) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    renderer_sh.render(&build(true));
    let px_sh = renderer_sh.read_pixels();
    let mut renderer_v1 = OffscreenRenderer::new(SIZE, SIZE).expect("v1 渲染器");
    renderer_v1.render(&build(false));
    let px_v1 = renderer_v1.read_pixels();

    let (bot_sh, top_sh) = (sample_at(&px_sh, BOX_BOTTOM), sample_at(&px_sh, BOX_TOP));
    let (bot_v1, top_v1) = (sample_at(&px_v1, BOX_BOTTOM), sample_at(&px_v1, BOX_TOP));
    // r−g 色相指标（红区为正、绿区为负）
    let hue = |p: [i32; 3]| p[0] - p[1];
    println!(
        "SH：箱底 {bot_sh:?}（r−g={}）箱顶 {top_sh:?}（r−g={}）",
        hue(bot_sh),
        hue(top_sh)
    );
    println!(
        "v1：箱底 {bot_v1:?}（r−g={}）箱顶 {top_v1:?}（r−g={}）",
        hue(bot_v1),
        hue(top_v1)
    );
    // 绝对判据：SH 模式下箱底红主导、箱顶绿主导
    assert!(hue(bot_sh) > 20, "SH 模式箱底必须红主导：{bot_sh:?}");
    assert!(hue(top_sh) < -20, "SH 模式箱顶必须绿主导：{top_sh:?}");
    // 跨模式作差（镜面 / 天空项抵消）：箱底往红走、箱顶往绿走。
    // 阈值 40 的依据 = 两侧 E/π 差 (1.0 − 0.05) × albedo 0.82 × kd_amb ≈ 0.7
    // 的间接项差，经 ACES 色调映射到 8-bit 后远超该量级（实测百级）。
    let d_bottom = hue(bot_sh) - hue(bot_v1);
    let d_top = hue(top_sh) - hue(top_v1);
    println!("跨模式差：箱底 {d_bottom}，箱顶 {d_top}");
    assert!(d_bottom > 40, "箱底必须因 SH 显著变红：{d_bottom}");
    assert!(d_top < -40, "箱顶必须因 SH 显著变绿：{d_top}");
    assert!(
        d_bottom - d_top > 120,
        "上下必须显著分离（v1 逐实体给不出这个梯度）：{d_bottom} vs {d_top}"
    );
    // v1 自身上下同值（同一份逐实体载荷 ⇒ 差异只来自镜面 / 天空的方向性）
    assert!(
        (hue(top_v1) - hue(bot_v1)).abs() < 40,
        "v1 模式上下色相必须接近（逐实体常数）：{top_v1:?} vs {bot_v1:?}"
    );
}

// ---------------------------------------------------------------------------
// 3. 方向性（§8.40②）
// ---------------------------------------------------------------------------

/// 方向性：同一探针云、同一 L0（⇒ v1 模式与「常量场 SH」模式逐位同源），
/// 只把 L1 的 y 向偶极打开 ⇒ 球体上半部（法线朝亮面）显著变亮、下半部
/// （法线朝暗面）显著变暗。v1 的无方向标量表达不了这个信号。
#[test]
fn directional_sh_separates_lit_and_unlit_normals_inside_one_probe() {
    let _gpu = GPU_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // 四探针（合法四面体）+ 半径 ∞ ⇒ 全场等权混合，混合值 = 单探针值
    let positions = vec![
        Vec3::new(-2.0, 0.5, -2.0),
        Vec3::new(2.0, 0.5, -2.0),
        Vec3::new(0.0, 0.5, 2.0),
        Vec3::new(0.0, 3.5, 0.0),
    ];
    let radii = vec![f32::INFINITY; 4];
    // E/π(n) = 0.5 + 0.45·n_y：球顶（n_y ≈ +0.87）≈ 0.89、球底（≈ −0.87）
    // ≈ 0.11，常量场对照 = 0.5（两者 L0 相同 ⇒ v1 值同口径）
    let base = Vec3::splat(0.5);
    let dipole = Vec3::splat(0.45);
    let flat = positions.iter().map(|_| const_sh(base)).collect::<Vec<_>>();
    let directed = positions
        .iter()
        .map(|_| dipole_sh(base, dipole))
        .collect::<Vec<_>>();

    // 球面上的两个采样点：绕球心赤道镜像对称（⇒ 镜面项 / nov 对称，
    // 唯一差异是法线的 y 分量）。**纬度必须 ≤ 45°**：更靠极的点法线带 −Z
    // 分量 ⇒ 背对相机被剔除，那个像素其实是射线在球面**前侧**的入口点，
    // CPU 对照就接不上（第一版用 60° 栽在这里）
    let center = Vec3::new(0.0, 2.0, 0.0);
    let tilt = 45.0f32.to_radians();
    let offset = Vec3::new(0.0, 1.5 * tilt.sin(), 1.5 * tilt.cos());
    let top_point = center + offset;
    let bottom_point = center - offset;
    let top_normal = offset.normalize();
    let bottom_normal = -top_normal;
    // CPU 侧口径：两次求值的比 = (0.5 + 0.45·n_y) / (0.5 − 0.45·n_y)
    let cpu_flat = sh_blend(&positions, &radii, &flat, top_point, top_normal).unwrap();
    let cpu_top = sh_blend(&positions, &radii, &directed, top_point, top_normal).unwrap();
    let cpu_bottom = sh_blend(&positions, &radii, &directed, bottom_point, bottom_normal).unwrap();
    assert!(
        (cpu_flat.x - 0.5).abs() < 1e-5 && (cpu_flat - cpu_flat.x).length() < 1e-5,
        "常量场求值必须 = 0.5：{cpu_flat:?}"
    );
    let want_top = 0.5 + 0.45 * top_normal.y;
    let want_bottom = 0.5 + 0.45 * bottom_normal.y;
    assert!(
        (cpu_top.x - want_top).abs() < 1e-4,
        "朝亮面求值必须 = 0.5 + 0.45·n_y：{} vs {want_top}",
        cpu_top.x
    );
    assert!(
        (cpu_bottom.x - want_bottom).abs() < 1e-4,
        "朝暗面求值必须 = 0.5 − 0.45·|n_y|：{} vs {want_bottom}",
        cpu_bottom.x
    );
    assert!(
        cpu_top.x / cpu_bottom.x > 4.0,
        "亮面 / 暗面比必须可判别：{} / {}",
        cpu_top.x,
        cpu_bottom.x
    );

    let build = |sh: Option<Vec<ShCoeffs>>| -> World {
        let mut world = bare_world();
        let mat = gray_material(&mut world);
        let sphere = world
            .resource_mut::<MeshStore>()
            .add(MeshData::uv_sphere(1.5, 48, 32));
        world.spawn((
            Transform {
                position: center,
                ..Transform::IDENTITY
            },
            sphere,
            mat,
        ));
        let values = vec![base * std::f32::consts::PI; 4];
        let set = match sh {
            Some(coeffs) => {
                IrradianceProbeSet::from_probes_sh(positions.clone(), values, coeffs, radii.clone())
            }
            None => IrradianceProbeSet::from_probes(positions.clone(), values),
        };
        world.insert_resource(set);
        propagate_transforms(&mut world);
        world
    };

    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    // 三轮：v1（无 SH）| 常量场 SH | 方向性 SH
    renderer.render(&build(None));
    let px_v1 = renderer.read_pixels();
    let mut renderer_flat = OffscreenRenderer::new(SIZE, SIZE).expect("常量场渲染器");
    renderer_flat.render(&build(Some(flat)));
    let px_flat = renderer_flat.read_pixels();
    let mut renderer_dir = OffscreenRenderer::new(SIZE, SIZE).expect("方向性渲染器");
    renderer_dir.render(&build(Some(directed)));
    let px_dir = renderer_dir.read_pixels();

    let lum_at = |px: &[u8], at: Vec3| luminance(sample_at(px, at));
    let (top_v1, bottom_v1) = (lum_at(&px_v1, top_point), lum_at(&px_v1, bottom_point));
    let (top_flat, bottom_flat) = (lum_at(&px_flat, top_point), lum_at(&px_flat, bottom_point));
    let (top_dir, bottom_dir) = (lum_at(&px_dir, top_point), lum_at(&px_dir, bottom_point));
    println!(
        "v1     上 {top_v1} 下 {bottom_v1}（差 {}）",
        top_v1 - bottom_v1
    );
    println!(
        "常量场 上 {top_flat} 下 {bottom_flat}（差 {}）",
        top_flat - bottom_flat
    );
    println!(
        "方向性 上 {top_dir} 下 {bottom_dir}（差 {}）",
        top_dir - bottom_dir
    );

    // v1 与常量场：同 L0 ⇒ 上下差都是「镜面 / 天空的方向性残差」，量级小
    assert!(
        (top_v1 - bottom_v1).abs() < 24,
        "v1 模式上下必须近乎同值（无方向标量）：{top_v1} vs {bottom_v1}"
    );
    // 方向性 SH：上半部（法线朝亮面）显著亮于常量场，下半部显著暗于常量场。
    // 阈值 24 的依据 = E/π 从 0.5 到 0.89 / 0.11 的 ±0.39 变化（相对 ±78%），
    // 乘 albedo 0.82 × kd_amb ≈ 0.9 后经 ACES 到 8-bit，远超 24（实测百级）。
    let d_top = top_dir - top_flat;
    let d_bottom = bottom_dir - bottom_flat;
    println!("对常量场的差：上 {d_top}，下 {d_bottom}");
    assert!(d_top > 24, "朝亮面的采样点必须显著变亮：{d_top}");
    assert!(d_bottom < -24, "朝暗面的采样点必须显著变暗：{d_bottom}");
    assert!(
        d_top - d_bottom > 80,
        "同一探针内的方向性分离必须可判别：{d_top} vs {d_bottom}"
    );
}

// ---------------------------------------------------------------------------
// 4. 真实烘焙贯通（红地板 / 绿天花板）
// ---------------------------------------------------------------------------

/// 两层异色板场景的烘焙参数：`sky_color` 抬亮（封闭双层板里天花板把地板
/// 遮成纯天空照明 ⇒ 两板的辐射率各 = `albedo × sky_color`，天空太暗会把
/// 8-bit 输出压进量化噪声里）。
fn bake_params() -> BakeParams {
    BakeParams {
        light_dir: Vec3::new(-0.4, -1.0, 0.2).normalize(),
        light_color: Vec3::new(1.0, 0.95, 0.9) * 2.0,
        sky_color: Vec3::splat(0.6),
        shadow_softness: 0.0,
        sky_samples: 32,
        bounces: 1,
    }
}

/// 红地板（y = −0.1）+ 绿天花板（y = 4.1）+ 球心 (0, 2, 0) 的球体 +
/// 四探针实体（合法四面体，两层高度；**未烘焙**——调用方决定烘 SH 还是 v1）。
/// 两次调用产出同构世界（句柄分配序一致 ⇒ 可跨会话对照）。
fn two_tone_scene() -> World {
    let mut world = bare_world();
    let slab = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let red = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.95, 0.1, 0.08, 1.0));
    let green = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.1, 0.95, 0.15, 1.0));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, -0.1, 0.0),
            scale: Vec3::new(24.0, 0.2, 24.0),
            ..Transform::IDENTITY
        },
        slab,
        red,
    ));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 4.1, 0.0),
            scale: Vec3::new(24.0, 0.2, 24.0),
            ..Transform::IDENTITY
        },
        slab,
        green,
    ));
    // 球体（动态物体：不进烘焙 ⇒ 吃探针 GI）
    let mat = gray_material(&mut world);
    let sphere = world
        .resource_mut::<MeshStore>()
        .add(MeshData::uv_sphere(1.2, 48, 32));
    world.spawn((
        Transform {
            position: SPHERE_CENTER,
            ..Transform::IDENTITY
        },
        sphere,
        mat,
    ));
    for p in probe_positions() {
        world.spawn((Transform::from_xyz(p.x, p.y, p.z), IrradianceProbe));
    }
    propagate_transforms(&mut world);
    world
}

/// 四探针布局（三个低探针 + 一个高探针 ⇒ 非共面，四面体化合法）。
fn probe_positions() -> [Vec3; 4] {
    [
        Vec3::new(-1.6, 1.2, -1.6),
        Vec3::new(1.6, 1.2, -1.6),
        Vec3::new(0.0, 1.2, 1.6),
        Vec3::new(0.0, 3.0, 0.0),
    ]
}

const SPHERE_CENTER: Vec3 = Vec3::new(0.0, 2.0, 0.0);

/// 真实烘焙贯通：`bake_irradiance_probes_sh` 产出的系数上 GPU 后，球体顶部
/// （朝绿天花板）偏绿、底部（朝红地板）偏红；并与 CPU 侧 `sh_irradiance_at`
/// 对照（两条独立实现互证——GPU 求值必须落在 CPU 求值的同侧），再与同一世界
/// 烘 v1 集（无 SH）对照（方向性必须消失）。
#[test]
fn baked_sh_probes_light_the_sphere_from_the_real_bake() {
    let _gpu = GPU_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // 球面上的两个采样点：绕球心赤道镜像对称（⇒ 镜面项 / nov 对称，
    // 唯一差异是法线的 y 分量）。**纬度必须 ≤ 45°**：更靠极的点法线带 −Z
    // 分量 ⇒ 背对相机被剔除，那个像素其实是射线在球面**前侧**的入口点，
    // CPU 对照就接不上（第一版用 60° 栽在这里）
    let tilt = 45.0f32.to_radians();
    let offset = Vec3::new(0.0, 1.2 * tilt.sin(), 1.2 * tilt.cos());
    let top_point = SPHERE_CENTER + offset;
    let bottom_point = SPHERE_CENTER - offset;
    let top_normal = offset.normalize();
    let bottom_normal = -top_normal;

    // ---- SH 世界 ----
    let mut world = two_tone_scene();
    let baked = bake_irradiance_probes_sh(&world, &bake_params());
    assert!(baked.has_sh(), "烘焙必须产出 SH");
    assert_eq!(baked.len(), 4);
    let report = install_irradiance_probes(&mut world, baked);
    assert!(report.sh, "装配报告必须记录 SH");
    assert!(!report.degenerate, "四探针（非共面）不得退化");
    let set = world.resource::<IrradianceProbeSet>().clone();
    // 半径 = 最近邻 × 1.5（自动派生；本布局最近邻 ≈ 3.2 m）
    for (i, r) in set.radii().iter().enumerate() {
        assert!(
            r.is_finite() && *r > 2.0,
            "探针 {i} 的自动半径必须有限且覆盖球体：{r}"
        );
    }

    // CPU 对照：球顶 / 球底的 E/π
    let cpu_top = set.sh_irradiance_at(top_point, top_normal).unwrap();
    let cpu_bottom = set.sh_irradiance_at(bottom_point, bottom_normal).unwrap();
    println!("CPU 球顶 {cpu_top:?} / 球底 {cpu_bottom:?}");
    assert!(
        cpu_top.y - cpu_top.x > 0.0,
        "CPU：球顶（朝绿天花板）必须偏绿：{cpu_top:?}"
    );
    assert!(
        cpu_bottom.x - cpu_bottom.y > 0.0,
        "CPU：球底（朝红地板）必须偏红：{cpu_bottom:?}"
    );
    let cpu_sep = (cpu_top.y - cpu_top.x) - (cpu_bottom.y - cpu_bottom.x);
    assert!(cpu_sep > 0.05, "CPU：上下色相分离必须可判别：{cpu_sep}");

    let Some(mut renderer) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    renderer.render(&world);
    let px = renderer.read_pixels();
    let top = sample_at(&px, top_point);
    let bottom = sample_at(&px, bottom_point);
    println!("GPU 球顶 {top:?} / 球底 {bottom:?}");
    // GPU 与 CPU 同侧：球顶偏绿、球底偏红（绝对量级受色调映射 / 镜面项影响，
    // 只断言符号与可判别性——量级由 CPU 对照与面板 2/3 的差值断言把守）
    assert!(
        top[1] - top[0] > 4,
        "GPU：球顶必须偏绿（与 CPU 同侧）：{top:?}"
    );
    assert!(
        bottom[0] - bottom[1] > 4,
        "GPU：球底必须偏红（与 CPU 同侧）：{bottom:?}"
    );
    let gpu_sep = (top[1] - top[0]) - (bottom[1] - bottom[0]);
    assert!(
        gpu_sep > 16,
        "GPU：上下色相分离必须可判别（CPU 分离 {cpu_sep}）：{gpu_sep}"
    );

    // ---- v1 对照世界（同一布局，烘不带 SH 的集）----
    let mut world_v1 = two_tone_scene();
    let v1_set = bake_irradiance_probes(&world_v1, &bake_params());
    assert!(!v1_set.has_sh());
    install_irradiance_probes(&mut world_v1, v1_set);
    let mut renderer_v1 = OffscreenRenderer::new(SIZE, SIZE).expect("v1 渲染器");
    renderer_v1.render(&world_v1);
    let px_v1 = renderer_v1.read_pixels();
    let top_v1 = sample_at(&px_v1, top_point);
    let bottom_v1 = sample_at(&px_v1, bottom_point);
    let sep_v1 = (top_v1[1] - top_v1[0]) - (bottom_v1[1] - bottom_v1[0]);
    println!("v1 球顶 {top_v1:?} / 球底 {bottom_v1:?}（分离 {sep_v1}）");
    assert!(
        sep_v1.abs() * 3 < gpu_sep,
        "v1 的方向性分离必须显著小于 SH（{sep_v1} vs {gpu_sep}）"
    );
    assert_ne!(
        fnv1a(&px),
        fnv1a(&px_v1),
        "SH 与 v1 两条路径必须给出不同画面（防恒等假通过）"
    );
}

// ---------------------------------------------------------------------------
// 5. 确定性
// ---------------------------------------------------------------------------

/// 确定性：同场景双会话逐字节一致；改单个探针的 SH 或位置 ⇒ 全帧哈希改变
/// （防「恒等假通过」——逐片元支真的在读探针表）。
#[test]
fn sh_path_is_deterministic_and_probe_edits_change_the_frame() {
    let _gpu = GPU_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let positions = gradient_positions();
    let radii = vec![1.8f32; 4];
    let base_sh = || {
        vec![
            const_sh(Vec3::new(1.0, 0.1, 0.1)),
            const_sh(Vec3::new(1.0, 0.1, 0.1)),
            const_sh(Vec3::new(1.0, 0.1, 0.1)),
            const_sh(Vec3::new(0.1, 1.0, 0.2)),
        ]
    };
    let build = |sh: Vec<ShCoeffs>, pos: Vec<Vec3>| -> World {
        let mut world = bare_world();
        let mat = gray_material(&mut world);
        let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
        world.spawn((
            Transform {
                position: Vec3::new(0.0, 2.0, 0.0),
                scale: Vec3::new(1.0, 4.0, 1.0),
                ..Transform::IDENTITY
            },
            cube,
            mat,
        ));
        world.insert_resource(IrradianceProbeSet::from_probes_sh(
            pos,
            vec![Vec3::splat(1.5); 4],
            sh,
            radii.clone(),
        ));
        propagate_transforms(&mut world);
        world
    };

    let Some(mut a) = OffscreenRenderer::new(SIZE, SIZE) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut b = OffscreenRenderer::new(SIZE, SIZE).expect("第二会话渲染器");
    let world_a = build(base_sh(), positions.clone());
    let world_b = build(base_sh(), positions.clone());
    a.render(&world_a);
    b.render(&world_b);
    let hash_a = fnv1a(&a.read_pixels());
    let hash_b = fnv1a(&b.read_pixels());
    assert_eq!(hash_a, hash_b, "同输入两条会话必须逐位一致");

    // 改单个探针（第 4 个）的 SH ⇒ 箱顶像素变、全帧哈希变
    let mut edited = base_sh();
    edited[3] = const_sh(Vec3::new(0.2, 0.3, 1.0));
    a.render(&build(edited, positions.clone()));
    let px_edited = a.read_pixels();
    assert_ne!(fnv1a(&px_edited), hash_a, "改单探针 SH 必须改变全帧哈希");
    // 复跑未改的世界取箱顶基准（同 build 函数 ⇒ 句柄同构，可复用渲染器 a）
    a.render(&world_a);
    let top_before = sample_at(&a.read_pixels(), BOX_TOP);
    assert_ne!(
        sample_at(&px_edited, BOX_TOP),
        top_before,
        "改箱顶探针的 SH 必须改变箱顶像素"
    );

    // 移动探针位置 ⇒ 全帧哈希变（位置进 storage，不是只读系数）
    let mut moved = positions.clone();
    moved[3] = Vec3::new(0.0, 2.2, 0.0);
    a.render(&build(base_sh(), moved));
    assert_ne!(fnv1a(&a.read_pixels()), hash_a, "移动探针必须改变全帧哈希");
}
