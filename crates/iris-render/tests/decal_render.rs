//! 集成测试：盒投影贴花（R39）——端盖代理几何 + 盒裁剪 + alpha 混合 +
//! 按纹理分批的贴花 pass（蒙皮之后、透明之前）。
//!
//! 断言策略（参考渲染法 + 区域分类法）：
//! - 光照全部关闭（方向光强度 0 + 环境光 0 + IBL 强度 0）、泛光关闭，
//!   表面用**自发光主导**材质（albedo 纯黑）——表面 HDR 输出 = 自发光值，
//!   贴花无光照输出 = 纹理 × tint，合成值可解析预测；
//! - 屏幕**区域断言**：世界点经与渲染器同源的 `Camera::view_proj` 投影到
//!   像素（NDC y 翻转与 blit 的 V 约定一致），盒内 / 盒外采样点分类断言；
//! - alpha 合成与 tint 用**参考渲染法**：与「单不透明平面、自发光 = 解析
//!   合成值」的参考场景逐像素比对——两侧经过完全相同的 ACES 色调映射 +
//!   sRGB 编码，±2/255 容差（8-bit 量化裕量）。
//!
//! **共面深度工程约定**：贴花端盖与被贴表面理论上重合（LessEqual 通过等
//! 深），但两侧平面经不同矩阵路径计算，末位 ulp 抖动会让部分像素随机落
//! 到表面之后。测试统一把端盖向相机侧抬 ε = 2 mm（视深差远大于 Depth32
//! Float 在该距离的精度 ~1e-7，视觉不可察觉）——这也是引擎使用方的推荐
//! 摆放（见 `decal.rs` 模块文档）。
//!
//! 每个场景变体用独立 `OffscreenRenderer`（GPU 材质 / 纹理缓存按句柄跨
//! 渲染持久 + FrameStats 互不串扰，同 `transparent_render.rs`）。

use glam::{Quat, Vec3};
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Entity, GlobalTransform, Transform, World};
use iris_render::prelude::*;

/// 七项测试各自建渲染器、共用单 GPU 适配器，并行时实测段错误
/// （0xc0000005，与 shadow_v2/transparent 的并行停顿同一病灶）——
/// 文件内互斥串行化（参照 `ssr_render.rs::GPU_SERIAL`）。
static GPU_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 端盖相对被贴表面的向相机侧抬升量（米）——共面深度的浮点裕量，
/// 见模块文档。
const CAP_EPSILON: f32 = 0.002;

/// 无光照分析场景：方向光强度 0 + 环境光 0（表面只出自发光）+ IBL 0 +
/// 关阴影 + 关泛光。相机由调用方设置。
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

/// 自发光主导材质：albedo 纯黑（光照项恒 0），HDR 输出 = emissive。
fn emissive_mat(r: f32, g: f32, b: f32) -> MaterialDef {
    MaterialDef {
        base_color: [0.0, 0.0, 0.0, 1.0],
        emissive: [r, g, b, 1.0],
        ..Default::default()
    }
}

/// alpha 混合自发光材质（玻璃板类，透明 pass 绘制）。
fn blend_mat(r: f32, g: f32, b: f32, a: f32) -> MaterialDef {
    MaterialDef {
        base_color: [0.0, 0.0, 0.0, a],
        emissive: [r, g, b, 1.0],
        alpha_mode: AlphaMode::Blend,
        ..Default::default()
    }
}

/// 绿色自发光地面（40×40，XZ 平面）。
fn spawn_green_floor(world: &mut World) {
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(0.0, 2.0, 0.0));
    world.spawn((Transform::IDENTITY, plane, mat));
}

/// 地面贴花实体：单位盒端盖贴地（局部 +Z → 世界 +Y），端盖位于地面之上
/// `CAP_EPSILON`。`decal.size` = 贴花盒尺寸。
fn spawn_floor_decal(world: &mut World, decal: Decal) -> Entity {
    let up = Quat::from_rotation_arc(Vec3::Z, Vec3::Y);
    // 盒中心 = -size.z/2（把 +Z 端盖压回 y=0 平面）+ ε（浮点裕量）
    let position = Vec3::new(0.0, -decal.size.z * 0.5 + CAP_EPSILON, 0.0);
    world.spawn((
        Transform {
            position,
            rotation: up,
            scale: Vec3::ONE,
        },
        decal,
    ))
}

/// 俯视原点的分析相机（地面场景共用）。
fn set_topdown_camera(world: &mut World) {
    let mut camera = world.resource_mut::<Camera>();
    camera.eye = Vec3::new(0.0, 4.0, 4.0);
    camera.target = Vec3::ZERO;
}

/// 相机 view-proj（宽高比 1，256² 正方形画幅）。
fn view_proj_of(world: &World) -> glam::Mat4 {
    world.resource::<Camera>().view_proj(1.0)
}

/// 世界点 → 像素坐标（与渲染器同源的 view_proj；NDC y 翻转与 blit 的
/// V 翻转约定一致：NDC y=+1 → 第 0 行）。
fn project(world_point: Vec3, vp: glam::Mat4, w: u32, h: u32) -> (usize, usize) {
    let clip = vp * world_point.extend(1.0);
    let ndc = clip.truncate() / clip.w;
    let px = (ndc.x * 0.5 + 0.5) * w as f32;
    let py = (0.5 - ndc.y * 0.5) * h as f32;
    (px as usize, py as usize)
}

/// 像素 [r, g, b]。
fn pixel(px: &[u8], w: u32, x: usize, y: usize) -> [i32; 3] {
    let i = (y * w as usize + x) * 4;
    [px[i] as i32, px[i + 1] as i32, px[i + 2] as i32]
}

/// 纯绿分类：g 通道显著主导且其余通道接近 0（自发光 (0,2,0) 经 ACES 后
/// g ≈ 238/255，r/b ≈ 0）。
fn assert_pure_green(p: [i32; 3], what: &str) {
    assert!(
        p[1] > 150 && p[1] > p[0] + 60 && p[1] > p[2] + 60,
        "{what}：应为纯绿，实际 rgb={p:?}"
    );
}

/// 红色分类（棋盘红格：无光照输出 = (1,0,0) 经 ACES）。
fn assert_red(p: [i32; 3], what: &str) {
    assert!(
        p[0] > 150 && p[0] > p[1] + 60 && p[0] > p[2] + 60,
        "{what}：应为红色棋盘格，实际 rgb={p:?}"
    );
}

/// 白色分类（棋盘白格：输出 (1,1,1) 经 ACES ≈ 各通道 240+/255）。
fn assert_white(p: [i32; 3], what: &str) {
    assert!(
        p[0] > 150 && p[1] > 150 && p[2] > 150,
        "{what}：应为白色棋盘格，实际 rgb={p:?}"
    );
}

/// 逐通道 ±tol 比较（参考渲染法）。
fn assert_pixel_near(got: [i32; 3], want: [i32; 3], tol: i32, what: &str) {
    for (name, (g, w)) in ["r", "g", "b"].iter().zip(got.iter().zip(want.iter())) {
        assert!(
            (g - w).abs() <= tol,
            "{what}：{name} 通道 {g} 应在 {w} ±{tol} 内"
        );
    }
}

/// 红白棋盘格贴花（2×2×0.1，tint 白）投在绿色地面，相机俯视原点。
fn checker_scene() -> World {
    let mut world = dark_world();
    set_topdown_camera(&mut world);
    spawn_green_floor(&mut world);
    let checker = world
        .resource_mut::<TextureStore>()
        .add(CpuTexture::checkerboard(
            2,
            4,
            [255, 0, 0, 255],
            [255, 255, 255, 255],
        ));
    let decal = Decal {
        texture: checker,
        size: Vec3::new(2.0, 2.0, 0.1),
        tint: [1.0, 1.0, 1.0, 1.0],
    };
    spawn_floor_decal(&mut world, decal);
    propagate_transforms(&mut world);
    world
}

/// 场景中第一个贴花实体的世界盒矩阵（与收集路径同构：实体全局矩阵 ×
/// size 缩放）——测试据此把盒局部采样点变到世界再投影到像素。
fn decal_model(world: &World) -> glam::Mat4 {
    let mut model = None;
    world.query::<(&GlobalTransform, &Decal)>(|(gt, decal)| {
        if model.is_none() {
            model = Some(gt.matrix() * glam::Mat4::from_scale(decal.size));
        }
    });
    model.expect("场景应有贴花实体")
}

/// 场景中第一个贴花实体。
fn first_decal_entity(world: &World) -> Entity {
    let mut found = None;
    world.query::<(Entity, &Decal)>(|(e, _)| {
        if found.is_none() {
            found = Some(e);
        }
    });
    found.expect("场景应有贴花实体")
}

/// 用例一「投影正确」：棋盘格贴花投在绿色平面上（盒 Z 轴贴面）——盒内
/// 采样点呈棋盘（红 / 白格与 UV 方向精确对应），盒外纯绿。若 UV 翻转、
/// 端盖朝向错误或投影矩阵组装错误，采样点的格色 / 区域颜色必然错位。
#[test]
fn decal_projects_checkerboard_onto_plane() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let world = checker_scene();
    renderer.render(&world);
    let px = renderer.read_pixels();
    let vp = view_proj_of(&world);
    let model = decal_model(&world);

    // 盒内：UV = local.xy + 0.5。局部 (-0.25,-0.25) → uv (0.25,0.25) →
    // 纹理 (2,2)/8 → 格 (0,0) = 红；(0.25,-0.25) → 格 (1,0) = 白；
    // (-0.25,0.25) → 格 (0,1) = 白
    for (lx, ly, red) in [
        (-0.25f32, -0.25f32, true),
        (0.25, -0.25, false),
        (-0.25, 0.25, false),
    ] {
        let p = model.transform_point3(Vec3::new(lx, ly, 0.5));
        let (x, y) = project(p, vp, 256, 256);
        let got = pixel(&px, 256, x, y);
        if red {
            assert_red(got, &format!("盒内红格采样点 local=({lx},{ly})"));
        } else {
            assert_white(got, &format!("盒内白格采样点 local=({lx},{ly})"));
        }
    }
    // 盒外：local 半径 0.8（盒界 0.5 之外、仍落在 40×40 地面与画面内）
    // → 纯绿
    for (lx, ly) in [(0.8f32, 0.0f32), (-0.8, 0.0), (0.0, 0.8), (0.0, -0.8)] {
        let p = model.transform_point3(Vec3::new(lx, ly, 0.5));
        let (x, y) = project(p, vp, 256, 256);
        assert!(
            (2..254).contains(&x) && (2..254).contains(&y),
            "盒外采样点 local=({lx},{ly}) 应落在画面内（实际 {x},{y}）"
        );
        assert_pure_green(
            pixel(&px, 256, x, y),
            &format!("盒外采样点 local=({lx},{ly})"),
        );
    }
}

/// 用例二「盒裁剪」：投影端盖四角的屏幕包围盒之外 2 像素环带零采样——
/// 环带全部纯绿（区域差分），盒内对照点呈棋盘。若几何越界（如整盒侧面
/// 渲染的边缘 texel 拉伸）或盒裁剪失效，环带必出现非绿像素。
#[test]
fn decal_clips_at_box_boundary() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let world = checker_scene();
    renderer.render(&world);
    let px = renderer.read_pixels();
    let (w, h) = renderer.size();
    let vp = view_proj_of(&world);
    let model = decal_model(&world);

    // 端盖四角的屏幕包围盒（保守外接：斜视角下投影为四边形，包围盒
    // ≥ 实际足迹，环带必在足迹之外）
    let mut min = (usize::MAX, usize::MAX);
    let mut max = (0usize, 0usize);
    for cx in [-0.5f32, 0.5] {
        for cy in [-0.5f32, 0.5] {
            let p = model.transform_point3(Vec3::new(cx, cy, 0.5));
            let (x, y) = project(p, vp, w, h);
            min = (min.0.min(x), min.1.min(y));
            max = (max.0.max(x), max.1.max(y));
        }
    }
    const BAND: usize = 2;
    // 环带：包围盒外扩 2 像素的四条边采样（步进 8 像素），必须全绿
    let x0 = min.0.saturating_sub(BAND);
    let x1 = (max.0 + BAND).min(w as usize - 1);
    let y0 = min.1.saturating_sub(BAND);
    let y1 = (max.1 + BAND).min(h as usize - 1);
    let mut ring = Vec::new();
    for x in (x0..=x1).step_by(8) {
        ring.push((x, y0));
        ring.push((x, y1));
    }
    for y in (y0..=y1).step_by(8) {
        ring.push((x0, y));
        ring.push((x1, y));
    }
    assert!(
        ring.len() >= 16,
        "环带采样点应足够覆盖（实际 {} 点）",
        ring.len()
    );
    for (x, y) in ring {
        assert_pure_green(
            pixel(&px, w, x, y),
            &format!("盒足迹外 2px 环带 ({x},{y}) 应零采样"),
        );
    }
    // 对照：盒内棋盘采样点仍然非绿（证明环带的绿色不是整个画面退化）
    let p = model.transform_point3(Vec3::new(-0.25, -0.25, 0.5));
    let (x, y) = project(p, vp, w, h);
    let got = pixel(&px, w, x, y);
    assert!(
        !(got[1] > got[0] + 60 && got[1] > got[2] + 60),
        "盒内对照点应为棋盘格而非绿色，实际 rgb={got:?}"
    );
}

/// 用例三「alpha 混合精确」：50% alpha 纯红贴花叠在自发光 (0,2,0) 绿色
/// 地面上 → 合成 = α·(1,0,0) + (1−α)·(0,2,0)（线性 HDR），与「单平面
/// 自发光 = 该合成值」的参考场景逐像素比对（±2/255）。混合系数、ACES、
/// sRGB 任一环节偏差都会超出容差。
#[test]
fn decal_alpha_blend_is_exact() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let alpha = 128.0f32 / 255.0;
    // 待测场景：α=128/255 纯红贴花 on 绿地面
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = dark_world();
    set_topdown_camera(&mut world);
    spawn_green_floor(&mut world);
    let tex = world
        .resource_mut::<TextureStore>()
        .add(CpuTexture::solid([255, 0, 0, 128]));
    let decal = Decal {
        texture: tex,
        size: Vec3::new(2.0, 2.0, 0.1),
        tint: [1.0, 1.0, 1.0, 1.0],
    };
    spawn_floor_decal(&mut world, decal);
    propagate_transforms(&mut world);
    renderer.render(&world);
    let px = renderer.read_pixels();
    let vp = view_proj_of(&world);
    let model = decal_model(&world);
    let (x, y) = project(
        model.transform_point3(Vec3::new(0.0, 0.0, 0.5)),
        vp,
        256,
        256,
    );
    let got = pixel(&px, 256, x, y);

    // 参考场景（独立渲染器）：单平面自发光 = 解析合成值
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = dark_world();
    set_topdown_camera(&mut world);
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let mat = world.resource_mut::<MaterialStore>().add(emissive_mat(
        alpha,               // α·红（r 通道）
        (1.0 - alpha) * 2.0, // (1−α)·绿（g 通道）
        0.0,                 // 蓝通道 = 0
    ));
    world.spawn((Transform::IDENTITY, plane, mat));
    propagate_transforms(&mut world);
    renderer.render(&world);
    let reference = pixel(&renderer.read_pixels(), 256, x, y);
    assert_pixel_near(got, reference, 2, "50% alpha 贴花与地面的合成应精确");
}

/// 用例四「tint」：乘色与整体 alpha 缩放生效——白纹理 × tint
/// (0.5, 1.0, 0.5, 0.5) 叠在绿地面，合成 = α'·(tint.rgb) + (1−α')·(0,2,0)，
/// α' = (128/255)×0.5。单点断言（±2/255，参考渲染法）。
#[test]
fn decal_tint_multiplies_color_and_alpha() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let alpha = (128.0f32 / 255.0) * 0.5; // tex.a × tint.a
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = dark_world();
    set_topdown_camera(&mut world);
    spawn_green_floor(&mut world);
    let tex = world
        .resource_mut::<TextureStore>()
        .add(CpuTexture::solid([255, 255, 255, 128]));
    let decal = Decal {
        texture: tex,
        size: Vec3::new(2.0, 2.0, 0.1),
        tint: [0.5, 1.0, 0.5, 0.5],
    };
    spawn_floor_decal(&mut world, decal);
    propagate_transforms(&mut world);
    renderer.render(&world);
    let px = renderer.read_pixels();
    let vp = view_proj_of(&world);
    let model = decal_model(&world);
    let (x, y) = project(
        model.transform_point3(Vec3::new(0.0, 0.0, 0.5)),
        vp,
        256,
        256,
    );
    let got = pixel(&px, 256, x, y);

    // 解析合成：src.rgb = tex.rgb × tint.rgb，src.a = tex.a × tint.a
    let want = [alpha * 0.5, alpha * 1.0 + (1.0 - alpha) * 2.0, alpha * 0.5];
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = dark_world();
    set_topdown_camera(&mut world);
    let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(want[0], want[1], want[2]));
    world.spawn((Transform::IDENTITY, plane, mat));
    propagate_transforms(&mut world);
    renderer.render(&world);
    let reference = pixel(&renderer.read_pixels(), 256, x, y);
    assert_pixel_near(got, reference, 2, "tint 乘色与整体 alpha 应精确生效");
}

/// 用例五「绘制序」：贴花（α=0.5 红）在墙上、半透明玻璃板（α=0.5 绿）
/// 再叠在前方 → 玻璃合成在**贴花混合结果**之上。与「单不透明墙、自发光
/// = 解析合成 glass ∘ decal ∘ wall」的参考场景比对。若贴花 pass 错排在
/// 透明之后，合成变为 decal ∘ glass ∘ wall（r 通道 0.5 vs 0.25），断言必挂。
#[test]
fn decal_draws_before_transparent_geometry() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let a = 0.5f32;
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = dark_world();
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 0.0, 5.0);
        camera.target = Vec3::ZERO;
    }
    // 蓝色自发光墙（不透明立方体薄板），前面 z = -2.95
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let wall_mat = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(0.0, 0.0, 2.0));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 0.0, -3.0),
            scale: Vec3::new(3.0, 3.0, 0.1),
            ..Transform::IDENTITY
        },
        cube,
        wall_mat,
    ));
    // 墙面贴花：局部 +Z → 世界 +Z（面向相机），端盖 = 墙面 + ε
    let tex = world
        .resource_mut::<TextureStore>()
        .add(CpuTexture::solid([255, 0, 0, 128]));
    let decal = Decal {
        texture: tex,
        size: Vec3::new(1.0, 1.0, 0.05),
        tint: [1.0, 1.0, 1.0, 1.0],
    };
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 0.0, -2.95 - decal.size.z * 0.5 + CAP_EPSILON),
            scale: Vec3::ONE,
            ..Transform::IDENTITY
        },
        decal,
    ));
    // 玻璃板（透明 pass，深度只测不写）在贴花前方 z = 0
    let glass = world
        .resource_mut::<MaterialStore>()
        .add(blend_mat(0.0, 2.0, 0.0, a));
    world.spawn((
        Transform {
            position: Vec3::ZERO,
            scale: Vec3::new(3.0, 3.0, 0.1),
            ..Transform::IDENTITY
        },
        cube,
        glass,
    ));
    propagate_transforms(&mut world);
    renderer.render(&world);
    let px = renderer.read_pixels();
    let got = pixel(&px, 256, 128, 128);

    // 参考：单不透明墙，自发光 = glass ∘ decal ∘ wall 的解析合成
    let after_decal = [a * 1.0 + (1.0 - a) * 0.0, 0.0, a * 0.0 + (1.0 - a) * 2.0];
    let composite = [
        a * 0.0 + (1.0 - a) * after_decal[0],
        a * 2.0 + (1.0 - a) * after_decal[1],
        a * 0.0 + (1.0 - a) * after_decal[2],
    ];
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = dark_world();
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 0.0, 5.0);
        camera.target = Vec3::ZERO;
    }
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world.resource_mut::<MaterialStore>().add(emissive_mat(
        composite[0],
        composite[1],
        composite[2],
    ));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 0.0, -3.0),
            scale: Vec3::new(3.0, 3.0, 0.1),
            ..Transform::IDENTITY
        },
        cube,
        mat,
    ));
    propagate_transforms(&mut world);
    renderer.render(&world);
    let reference = pixel(&renderer.read_pixels(), 256, 128, 128);
    assert_pixel_near(
        got,
        reference,
        10,
        "玻璃应合成在贴花之上（贴花在透明 pass 之前）",
    );
}

/// 用例六「Visibility 过滤与层级级联」：`HIDDEN` 贴花不出现（自身隐藏与
/// 祖先级联两种路径），可见性过滤不计入 `culled_instances`；可见对照组
/// 的贴花正常入画并计入统计。
#[test]
fn hidden_decals_are_skipped_with_ancestor_cascade() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    // --- 自身 HIDDEN：贴花不出现、不计剔除 ---
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = checker_scene();
    world.insert(first_decal_entity(&world), Visibility::HIDDEN);
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(stats.instances, 1, "隐藏贴花不进收集（只剩地面）");
    assert_eq!(stats.draw_calls, 1, "无贴花批 draw call");
    assert_eq!(stats.triangles_drawn, 2, "只有地面 2 三角形");
    assert_eq!(stats.culled_instances, 0, "可见性过滤不算剔除");
    let px = renderer.read_pixels();
    let vp = view_proj_of(&world);
    let model = decal_model(&world);
    let (x, y) = project(
        model.transform_point3(Vec3::new(-0.25, -0.25, 0.5)),
        vp,
        256,
        256,
    );
    assert_pure_green(pixel(&px, 256, x, y), "隐藏贴花的盒内采样点应为纯绿");

    // --- 祖先级联：贴花挂到 HIDDEN 根下 → 子树全隐藏 ---
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = checker_scene();
    let rig = world.spawn((Transform::IDENTITY, Visibility::HIDDEN));
    world.add_child(rig, first_decal_entity(&world));
    propagate_transforms(&mut world);
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(stats.instances, 1, "祖先隐藏 ⇒ 贴花整树隐藏");
    assert_eq!(stats.draw_calls, 1);
    let px = renderer.read_pixels();
    let vp = view_proj_of(&world);
    let model = decal_model(&world);
    let (x, y) = project(
        model.transform_point3(Vec3::new(0.25, -0.25, 0.5)),
        vp,
        256,
        256,
    );
    assert_pure_green(pixel(&px, 256, x, y), "级联隐藏的盒内采样点应为纯绿");

    // --- 对照组：根恢复可见 → 贴花入画并计入统计 ---
    world.insert(rig, Visibility::VISIBLE);
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(stats.instances, 2, "地面 + 贴花");
    assert_eq!(stats.draw_calls, 2, "地面批 + 贴花批各 1");
    assert_eq!(stats.triangles_drawn, 6, "地面 2 + 贴花端盖 4 三角形");
    let px = renderer.read_pixels();
    let (x, y) = project(
        model.transform_point3(Vec3::new(-0.25, -0.25, 0.5)),
        vp,
        256,
        256,
    );
    assert_red(pixel(&px, 256, x, y), "恢复可见后红格应出现");
}

/// 用例七「零开销默认路径」：无贴花场景的 FrameStats 与 R38 基线语义
/// 完全一致（贴花 pass 整体跳过、不产生任何额外 draw / 实例 / 三角形 /
/// 剔除计数）；加一枚贴花后统计恰好 +1 批 / +1 实例 / +4 三角形。
#[test]
fn scene_without_decals_matches_pre_decal_stats() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = dark_world();
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 3.0, 6.0);
        camera.target = Vec3::ZERO;
    }
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(1.0, 1.0, 1.0));
    world.spawn((Transform::from_xyz(-1.0, 0.0, 0.0), cube, mat));
    world.spawn((Transform::from_xyz(1.0, 0.0, 0.0), cube, mat));
    propagate_transforms(&mut world);
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    // R38 基线：2 同网格同材质实例 = 1 批 1 draw、24 三角形、零剔除
    assert_eq!(stats.draw_calls, 1, "无贴花场景 draw_calls 应与 R38 一致");
    assert_eq!(stats.instances, 2);
    assert_eq!(stats.triangles_drawn, 24);
    assert_eq!(stats.culled_instances, 0);
    assert!(
        stats.instance_upload_bytes > 0,
        "几何实例上传路径不受贴花 pass 影响"
    );

    // 对照：同一场景 + 1 贴花 → 恰好 +1 draw / +1 实例 / +4 三角形
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = dark_world();
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 3.0, 6.0);
        camera.target = Vec3::ZERO;
    }
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(1.0, 1.0, 1.0));
    world.spawn((Transform::from_xyz(-1.0, 0.0, 0.0), cube, mat));
    world.spawn((Transform::from_xyz(1.0, 0.0, 0.0), cube, mat));
    // 挂在立方体前面的地面贴花
    let tex = world
        .resource_mut::<TextureStore>()
        .add(CpuTexture::solid([255, 0, 0, 255]));
    let decal = Decal {
        texture: tex,
        size: Vec3::new(1.0, 1.0, 0.05),
        tint: [1.0, 1.0, 1.0, 1.0],
    };
    world.spawn((
        Transform {
            position: Vec3::new(0.0, -decal.size.z * 0.5 + CAP_EPSILON, 1.5),
            rotation: Quat::from_rotation_arc(Vec3::Z, Vec3::Y),
            scale: Vec3::ONE,
        },
        decal,
    ));
    propagate_transforms(&mut world);
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(stats.draw_calls, 2, "不透明批 + 贴花批");
    assert_eq!(stats.instances, 3, "2 立方体 + 1 贴花");
    assert_eq!(stats.triangles_drawn, 28, "2 × 12 + 贴花端盖 4 三角形");
}

// ---------------------------------------------------------------------------
// C2 · 贴花受光（DecalLighting，闭合 §8.19 直射项）
// ---------------------------------------------------------------------------

/// FNV-1a 64 全帧哈希（同款工具函数见 `lightmap_render.rs`）。
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn lit_scene(lighting: bool, intensity: f32, ambient: f32) -> World {
    let mut world = checker_scene();
    {
        let mut light = world.resource_mut::<DirectionalLight>();
        light.direction = Vec3::new(0.0, -1.0, 0.0);
        light.color = Vec3::ONE;
        light.intensity = intensity;
        light.ambient = Vec3::splat(ambient);
    }
    if lighting {
        world.insert_resource(DecalLighting);
    }
    world
}

/// 中央 32×32 像素 ROI 的 RGB 通道和（棋盘贴花占据俯视画面中心）。
fn center_roi_sum(px: &[u8], w: usize) -> u64 {
    let mut sum = 0u64;
    let c = w / 2;
    for y in (c - 16)..(c + 16) {
        for x in (c - 16)..(c + 16) {
            let i = (y * w + x) * 4;
            sum += u64::from(px[i]) + u64::from(px[i + 1]) + u64::from(px[i + 2]);
        }
    }
    sum
}

/// C2 受光双锚：①**中性光照退化锚**——`ambient ≡ 1、intensity = 0` 时
/// 光照因子恒 (1,1,1)，乘 1.0 为 IEEE 恒等 ⇒ 「资源在场」与「资源缺席」
/// 全帧逐位一致（在场但无害的严格证据）；②**判别锚**——顶光 intensity 2
/// 时受光帧必须与关闭态哈希不同，且贴花 ROI 亮度上升（地面是自发光
/// 材质不受光照影响 ⇒ 差异只能来自贴花的乘性因子）。
#[test]
fn decal_lighting_neutral_anchor_and_sun_discriminability() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(128, 128) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    // ① 中性锚：off（intensity 0 / ambient 1）vs on（同光照 + 资源）
    renderer.render(&lit_scene(false, 0.0, 1.0));
    let neutral_off = renderer.read_pixels();
    renderer.render(&lit_scene(true, 0.0, 1.0));
    let neutral_on = renderer.read_pixels();
    assert_eq!(
        fnv1a(&neutral_off),
        fnv1a(&neutral_on),
        "因子 ≡ 1 时受光开必须与关逐位一致（乘 1.0 恒等退化锚）"
    );
    // ② 判别锚：顶光满额（N=+Y 朝相机，dot(N,L)=1 ⇒ 因子 2.0）
    renderer.render(&lit_scene(false, 2.0, 0.0));
    let sun_off = renderer.read_pixels();
    renderer.render(&lit_scene(true, 2.0, 0.0));
    let sun_on = renderer.read_pixels();
    assert_ne!(
        fnv1a(&sun_off),
        fnv1a(&sun_on),
        "受光开 / 关在直射光下必须可判别（哈希相同 = 因子没接进像素）"
    );
    let (w, _) = renderer.size();
    let w = w as usize;
    assert!(
        center_roi_sum(&sun_on, w) > center_roi_sum(&sun_off, w),
        "×2 光照因子的贴花 ROI 必须更亮：{} > {}",
        center_roi_sum(&sun_on, w),
        center_roi_sum(&sun_off, w)
    );
}

/// 贴花纹理动态更新：同句柄换色后必须重建贴花绑定组，画面与新建世界
/// 逐字节一致，且绑定组缓存不累积。
#[test]
fn decal_texture_update_at_same_handle_rebuilds_bind_group() {
    let _gpu = GPU_SERIAL.lock().unwrap();
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };

    let mut world = dark_world();
    set_topdown_camera(&mut world);
    spawn_green_floor(&mut world);
    let texture = world
        .resource_mut::<TextureStore>()
        .add(CpuTexture::solid([255, 0, 0, 255]));
    let decal = Decal {
        texture,
        size: Vec3::new(2.0, 2.0, 0.1),
        tint: [1.0, 1.0, 1.0, 1.0],
    };
    spawn_floor_decal(&mut world, decal);
    propagate_transforms(&mut world);

    renderer.render(&world);
    let red = renderer.read_pixels();
    assert_eq!(
        renderer.renderer().resource_cache_stats().decal_bind_groups,
        1,
        "首帧应有一个贴花纹理绑定组"
    );

    assert!(
        world
            .resource_mut::<TextureStore>()
            .update(texture, CpuTexture::solid([0, 0, 255, 255])),
        "存活贴花纹理更新必须命中"
    );
    renderer.render(&world);
    let blue = renderer.read_pixels();
    assert_eq!(
        renderer.renderer().resource_cache_stats().decal_bind_groups,
        1,
        "纹理更新后绑定组重建，不得累积"
    );
    assert!(red != blue, "贴花红蓝换色必须可判别");

    let mut fresh = dark_world();
    set_topdown_camera(&mut fresh);
    spawn_green_floor(&mut fresh);
    let fresh_texture = fresh
        .resource_mut::<TextureStore>()
        .add(CpuTexture::solid([0, 0, 255, 255]));
    spawn_floor_decal(
        &mut fresh,
        Decal {
            texture: fresh_texture,
            size: Vec3::new(2.0, 2.0, 0.1),
            tint: [1.0, 1.0, 1.0, 1.0],
        },
    );
    propagate_transforms(&mut fresh);
    let mut fresh_renderer = OffscreenRenderer::new(256, 256).expect("GPU 已在场");
    fresh_renderer.render(&fresh);
    assert!(
        blue == fresh_renderer.read_pixels(),
        "贴花纹理更新后的画面必须与新建世界逐字节一致"
    );
}
