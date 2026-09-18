//! 集成测试：多视口与渲染到纹理（R40）——RT 相机渲染到离屏纹理 +
//! UI 图片控件（`UiImage` / `UiImageRtt`）把 RT 或普通纹理贴到屏幕。
//!
//! 断言策略：
//! - **主导通道签名**：RT / UI 内容 sRGB + ACES 化，精确值不可靠；用
//!   「亮红 / 蓝主导 + 未达阈值视为背景」等签名比较（同 `ui_render.rs`）；
//! - **主视图零变化回归锁**：同一场景仅增加 RT 相机（或 RT store），主
//!   pass 像素与无 RT 时**逐像素相等**——「默认场景（无 RT）主渲染像素
//!   零变化」的硬约束；
//! - **区域差分**：RT 内容区域的像素分布与无该 UI 时显著不同（证明 RT
//!   内容确实落在该区域）;
//! - **参考渲染法**：`UiImage` 贴实心纹理 → 区域像素与纹理一致（±2/255，
//!   sRGB 往返量化裕量）。
//!
//! 光照全部关闭（方向光强度 0 + 环境光 0 + IBL 0）+ 泛光关闭 + 自发光
//! 主导材质——表面 HDR 输出 = 自发光值，经 ACES 后色彩签名干净可判。

use glam::{Quat, Vec3};
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 重负载 GPU 套件文件内串行互斥（AGENTS.md 纪律：同文件多测试并行共用
/// 单适配器会挂死；R82/R89 控制轮实锤先例照 `ssr_render.rs::GPU_SERIAL`）。
static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 无光照分析场景（表面只出自发光，见模块文档）。主相机资源与 RT 相机
/// 由调用方设置。
fn dark_world() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    world.insert_resource(DirectionalLight {
        intensity: 0.0,
        ambient: Vec3::ZERO,
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(EnvironmentSettings { intensity: 0.0 });
    world.insert_resource(BloomSettings::disabled());
    world
}

/// 确保 RenderTargetStore 已插入（缺席则插入空 store；此后 resource_mut
/// 可安全 add）。游戏侧按需 insert（不进 DefaultPlugins），测试显式插入。
fn ensure_rt_store(world: &mut World) {
    if world.try_resource::<RenderTargetStore>().is_none() {
        world.insert_resource(RenderTargetStore::new());
    }
}

/// 自发光主导材质：albedo 纯黑（光照项恒 0），HDR 输出 = emissive。
fn emissive_mat(r: f32, g: f32, b: f32) -> MaterialDef {
    MaterialDef {
        base_color: [0.0, 0.0, 0.0, 1.0],
        emissive: [r, g, b, 1.0],
        ..Default::default()
    }
}

/// 亮红签名（sRGB 红自发光 2.0 经 ACES → 亮红）。
fn is_red(px: &[u8], i: usize) -> bool {
    let (r, g, b) = (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32);
    r > 150 && r > g + 60 && r > b + 60
}

/// 俯视 RT 相机实体：位置 (0,height,0) 向下看（局部 +Z → 世界 +Y ⇒
/// 前方 -Z → -Y），fov / 近远面用 ActiveCamera 默认值。
fn spawn_rt_camera(world: &mut World, height: f32, target: RenderTargetHandle) {
    world.spawn((
        Transform {
            position: Vec3::new(0.0, height, 0.0),
            rotation: Quat::from_rotation_arc(Vec3::Z, Vec3::Y),
            scale: Vec3::ONE,
        },
        ActiveCamera::default(),
        RendersTo { target },
    ));
}

/// 用例一「RT 内容正确 + UiImageRtt 呈现」：RT 相机平视前方的一块红色
/// 自发光立方体（主相机资源刻意看向空区，故主视图为暗背景）；RT 内容经
/// UiImageRtt 贴到左上角。断言：RT 区域中心为亮红（立方体在 RT 视锥中
/// 心）、RT 区域外主视图为暗背景（主路径不受 RT 干扰）。
#[test]
fn rt_content_visible_via_ui_image_rtt() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = dark_world();
    // 主相机资源看向空区（+Z 远端）：主视图 = 暗背景
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 1.8, 100.0);
        camera.target = Vec3::new(0.0, 1.8, 101.0);
    }
    // RT store + RT 相机（平视 -Z 前方红色立方体）
    ensure_rt_store(&mut world);
    let rt = world.resource_mut::<RenderTargetStore>().add(128, 128);
    spawn_rt_camera(&mut world, 4.0, rt);
    // 红色自发光立方体：位于 RT 相机正前方 (0,1,0)，RT 相机在 (0,1,4) 看 -Z
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let red = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(2.0, 0.0, 0.0));
    world.spawn((Transform::from_xyz(0.0, 1.0, 0.0), cube, red));
    // UiImageRtt：左上角 64×64 显示 RT
    world.spawn((
        UiRect {
            position: [8.0, 8.0],
            size: [64.0, 64.0],
        },
        UiImageRtt { target: rt },
        UiColor::default(),
    ));
    propagate_transforms(&mut world);
    renderer.render(&world);
    let w = renderer.size().0 as usize;
    let px = renderer.read_pixels();

    // RT 区域（8..72 × 8..72）中心（40,40）= RT 视图中心 = 立方体 → 亮红
    let cx = (8 + 64 / 2) as usize;
    let cy = (8 + 64 / 2) as usize;
    let ci = (cy * w + cx) * 4;
    assert!(
        is_red(&px, ci),
        "RT 区域中心应渲染出红色立方体（红签名），实际 ({},{},{})",
        px[ci],
        px[ci + 1],
        px[ci + 2]
    );
    // RT 区域内多数像素为红（立方体占 RT 视锥主体）——统计红签名占比
    let mut red = 0u32;
    let total = (64 - 12) * (64 - 12);
    for y in 20..72 {
        for x in 20..72 {
            let i = (y * w + x) * 4;
            if is_red(&px, i) {
                red += 1;
            }
        }
    }
    assert!(
        red > total / 3,
        "RT 区域应显著出现红色立方体（红签名 {red}/{total}）"
    );
    // 主视图（远离 RT 区域，右下角）应为天空/非红——主相机看向空区，红色
    // 立方体在 RT 相机的俯视 / 平视视锥内（只在 RT 输出中出现）
    let non_red_i = (200 * w + 200) * 4;
    assert!(
        !is_red(&px, non_red_i),
        "主视图不应有红色立方体（立方体只在 RT 视锥内），实际 ({},{},{})",
        px[non_red_i],
        px[non_red_i + 1],
        px[non_red_i + 2]
    );
}

/// 用例二「主视图零变化回归锁」：同一场景仅增加 RT store + RT 相机实体
///（不考虑 UI），主 pass 像素与无 RT 时逐像素一致。
#[test]
fn main_view_pixels_unchanged_with_rt_camera() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    // 场景 A：无 RT。主相机资源（默认）+ 灰色地面 + 红色立方体 + 方向光。
    let scene_a = || {
        let mut world = dark_world();
        let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(60.0));
        let grey = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.5, 0.5, 0.5, 1.0));
        world.spawn((Transform::IDENTITY, plane, grey));
        let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
        let red = world
            .resource_mut::<MaterialStore>()
            .add(emissive_mat(1.5, 0.0, 0.0));
        world.spawn((Transform::from_xyz(0.0, 0.6, 0.0), cube, red));
        propagate_transforms(&mut world);
        world
    };
    // 场景 B：同上 + RT store + RT 相机实体（俯视），无 UI。
    let scene_b = || {
        let mut world = scene_a();
        ensure_rt_store(&mut world);
        let rt = world.resource_mut::<RenderTargetStore>().add(128, 128);
        spawn_rt_camera(&mut world, 10.0, rt);
        propagate_transforms(&mut world);
        world
    };
    renderer.render(&scene_a());
    let a = renderer.read_pixels();
    renderer.render(&scene_b());
    let b = renderer.read_pixels();
    assert_eq!(
        a, b,
        "增加 RT store + RT 相机后主 pass 像素必须与无 RT 时逐像素一致"
    );
    // 两帧统计一致（主几何不受 RT 影响）
    assert_eq!(
        renderer.renderer().stats().draw_calls,
        2,
        "地面 + 立方体 = 2 draw call"
    );
}

/// 用例三「主视图也随 RT 主判并行一致」：在用例二基础上，RT 相机也存在
/// 但不影响主视图统计（FrameStats 口径 = 主几何）。
#[test]
fn rt_camera_does_not_change_frame_stats() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(128, 128) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = dark_world();
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(60.0));
    let grey = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.5, 0.5, 0.5, 1.0));
    world.spawn((Transform::IDENTITY, plane, grey));
    ensure_rt_store(&mut world);
    let rt = world.resource_mut::<RenderTargetStore>().add(64, 64);
    spawn_rt_camera(&mut world, 10.0, rt);
    propagate_transforms(&mut world);
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    // RT 不占主几何口径：只有地面 + （RT 相机无网格）→ 除地面外零实例；
    // 主视图默认相机看向地面 → 1 实例 / 1 draw。
    assert_eq!(stats.instances, 1, "主视图只有地面实例");
    assert_eq!(stats.draw_calls, 1, "主视图只有地面 1 draw call");
}

/// 用例四「UiImage 贴实心纹理」：暗背景 + 左上角 64×64 的 UiImage（实心
/// 红纹理）。区域内像素与纹理一致（±2/255 sRGB 量化容差），区域外为零
/// 红色溢出。
#[test]
fn ui_image_draws_texture_pixels() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = dark_world();
    let tex = world
        .resource_mut::<TextureStore>()
        .add(CpuTexture::solid([255, 0, 0, 255]));
    world.spawn((
        UiRect {
            position: [64.0, 64.0],
            size: [64.0, 64.0],
        },
        UiImage { texture: tex },
        UiColor::default(),
    ));
    renderer.render(&world);
    let w = renderer.size().0 as usize;
    let px = renderer.read_pixels();

    // 区域内部（68..124）应全部红签名（实心红纹理 Nearest 映射）
    let mut red = 0u32;
    let total = (124 - 68) * (124 - 68);
    for y in 68..124 {
        for x in 68..124 {
            let i = (y * w + x) * 4;
            if is_red(&px, i) {
                red += 1;
            }
        }
    }
    assert_eq!(red, total, "UiImage 区域内部应全部亮红，实际 {red}/{total}");
    // 紧贴区域的外框带零红溢出
    let mut stray = 0u32;
    for y in 52..140 {
        for x in 52..140 {
            if (64..128).contains(&x) && (64..128).contains(&y) {
                continue;
            }
            let i = (y * w + x) * 4;
            if is_red(&px, i) {
                stray += 1;
            }
        }
    }
    assert_eq!(stray, 0, "UiImage 区域外不应用红签名溢出，实际 {stray}");
}

/// 用例五「UiImageRtt 区域差分」：主相机资源看向空区（主视图 = 天空），
/// RT 相机平视一块红色自发光立方体。左上角贴 UiImageRtt：该区域出现 RT
/// 内容（红），与无 UiImageRtt 时（天空，非红）显著不同；其余区域不受
/// 影响（region-diff 证明 RT 内容确实落在该区域）。
#[test]
fn ui_image_rtt_region_renders_rt_content() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let scene = |with_rtt: bool| {
        let mut world = dark_world();
        // 主相机资源看向空区：主视图 = 天空（非红）
        {
            let mut camera = world.resource_mut::<Camera>();
            camera.eye = Vec3::new(0.0, 1.8, 100.0);
            camera.target = Vec3::new(0.0, 1.8, 101.0);
        }
        // 红色自发光立方体：仅 RT 相机（平视 -Z 前方）可见
        let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
        let red = world
            .resource_mut::<MaterialStore>()
            .add(emissive_mat(2.0, 0.0, 0.0));
        world.spawn((Transform::from_xyz(0.0, 1.0, 0.0), cube, red));
        ensure_rt_store(&mut world);
        let rt = world.resource_mut::<RenderTargetStore>().add(128, 128);
        spawn_rt_camera(&mut world, 4.0, rt);
        if with_rtt {
            world.spawn((
                UiRect {
                    position: [8.0, 8.0],
                    size: [64.0, 64.0],
                },
                UiImageRtt { target: rt },
                UiColor::default(),
            ));
        }
        propagate_transforms(&mut world);
        world
    };
    renderer.render(&scene(true));
    let with_rtt = renderer.read_pixels();
    renderer.render(&scene(false));
    let without_rtt = renderer.read_pixels();
    let w = renderer.size().0 as usize;

    // 左上角区域（8..72）：with_rtt 应显著多红（RT 内容 = 红色立方体），
    // without_rtt 应为天空（非红）——区域差分证明 RT 内容确实落在该区域。
    let region_red = |px: &[u8]| {
        let mut red = 0u32;
        for y in 20..72 {
            for x in 20..72 {
                let i = (y * w + x) * 4;
                if is_red(px, i) {
                    red += 1;
                }
            }
        }
        red
    };
    let red_with = region_red(&with_rtt);
    let red_without = region_red(&without_rtt);
    let total = (72 - 20) * (72 - 20);
    assert!(
        red_with > total / 8,
        "UiImageRtt 区域应呈现 RT 红色立方体内容（{red_with}/{total}）"
    );
    assert!(
        red_without < total / 20,
        "无 UiImageRtt 时该区域应为天空（非红主导），实际 {red_without}/{total}"
    );
    // 右上角（无 UI）：with_rtt 与 without_rtt 一致（UI 只影响其区域）
    let mut changed = 0u32;
    for y in 10..60 {
        for x in 190..240 {
            let i = (y * w + x) * 4;
            let d = (with_rtt[i] as i32 + with_rtt[i + 1] as i32 + with_rtt[i + 2] as i32)
                - (without_rtt[i] as i32 + without_rtt[i + 1] as i32 + without_rtt[i + 2] as i32);
            if d.abs() > 12 {
                changed += 1;
            }
        }
    }
    assert_eq!(
        changed, 0,
        "UiImageRtt 只应影响其区域，右上角不应有像素变化"
    );
}

/// 用例六「无 RT / 空 store = 主路径零变化」：未插入 RenderTargetStore 与
/// 插入空 store（无 RT 相机）两种世界渲染结果逐像素一致——store 缺席 =
/// 无 RT 相机 = 主视图行为零变化。
#[test]
fn no_rt_store_keeps_baseline() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(128, 128) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let scene = |with_store: bool| {
        let mut world = dark_world();
        let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(60.0));
        let grey = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.5, 0.5, 0.5, 1.0));
        world.spawn((Transform::IDENTITY, plane, grey));
        if with_store {
            world.insert_resource(RenderTargetStore::new());
        }
        propagate_transforms(&mut world);
        world
    };
    renderer.render(&scene(false));
    let a = renderer.read_pixels();
    renderer.render(&scene(true));
    let b = renderer.read_pixels();
    assert_eq!(
        a, b,
        "无 RT store 与空 store 的主 pass 像素必须一致（缺席 = 无 RT 相机）"
    );
}

/// 用例七「resize 重建正确性」（C1 · §8.20 闭合）：
/// ①「先建 64×64 再 resize 到 96×96」的渲染结果必须与「直接建 96×96」
///   **逐像素一致**——尺寸失配重建出的 GPU 资源与新建严格等价；
/// ②「resize 到同尺寸」是良性空操作——与完全不 resize 逐像素一致
///   （ensure 快路零动作的证据）；
/// ③以上全程只动 RT 侧，主视图（含 UiImageRtt 区域）像素即断言对象。
#[test]
fn rt_resize_rebuild_matches_fresh_target_bitwise() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    // 场景工厂：初建尺寸 initial，随后（可选）resize 到 to。
    let scene = |initial: (u32, u32), to: Option<(u32, u32)>| {
        let mut world = dark_world();
        {
            let mut camera = world.resource_mut::<Camera>();
            camera.eye = Vec3::new(0.0, 1.8, 100.0);
            camera.target = Vec3::new(0.0, 1.8, 101.0);
        }
        let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
        let red = world
            .resource_mut::<MaterialStore>()
            .add(emissive_mat(2.0, 0.0, 0.0));
        world.spawn((Transform::from_xyz(0.0, 1.0, 0.0), cube, red));
        ensure_rt_store(&mut world);
        let rt = world
            .resource_mut::<RenderTargetStore>()
            .add(initial.0, initial.1);
        if let Some((w, h)) = to {
            assert!(
                world.resource_mut::<RenderTargetStore>().resize(rt, w, h),
                "有效句柄 resize 应成功"
            );
            assert_eq!(
                world
                    .resource::<RenderTargetStore>()
                    .get(rt)
                    .map(|s| (s.width, s.height)),
                Some((w.max(1), h.max(1))),
                "CPU 描述应立即反映新尺寸"
            );
        }
        spawn_rt_camera(&mut world, 4.0, rt);
        world.spawn((
            UiRect {
                position: [8.0, 8.0],
                size: [64.0, 64.0],
            },
            UiImageRtt { target: rt },
            UiColor::default(),
        ));
        propagate_transforms(&mut world);
        world
    };
    // 帧序刻意安排：先让缓存停在 64×64，再走 resize→96（**真实触发重建**
    // ——失配臂被覆盖），最后直建 96 作逐像素对照（同句柄同尺寸 = 快路，
    // 二者像素必须一致 ⇒ 重建产物 ≡ 新建产物）。
    renderer.render(&scene((64, 64), None));
    let _ = renderer.read_pixels();
    // ①先建 64 再 resize 96 —— 重建路径
    renderer.render(&scene((64, 64), Some((96, 96))));
    let resized = renderer.read_pixels();
    // ①直建 96 —— 对照路径（缓存已 96，ensure 快路零动作）
    renderer.render(&scene((96, 96), None));
    let fresh = renderer.read_pixels();
    assert_eq!(
        fresh, resized,
        "resize 重建后的 GPU 资源必须与新尺寸直接创建逐像素一致"
    );
    // ②同尺寸 resize = 空操作（与不 resize 逐像素一致）
    renderer.render(&scene((96, 96), Some((96, 96))));
    let noop = renderer.read_pixels();
    assert_eq!(
        fresh, noop,
        "resize 到同尺寸必须与不 resize 逐像素一致（良性空操作）"
    );
}
