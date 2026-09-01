//! 集成测试：透明渲染（R23）——alpha 混合材质 + 视深排序透明 pass。
//!
//! 断言策略（参考渲染法）：透明场景与「单不透明立方体、自发光 = 解析
//! 预期的 HDR 合成值」的参考场景各渲一帧，比较中心像素。两侧经过完全
//! 相同的 ACES 色调映射 + sRGB 编码，解析 HDR 值推导如下：
//!
//! - 材质全部**自发光主导**：albedo 纯黑（漫反射 / 常量环境光项 = 0，
//!   IBL 镜面 ≈ F0(0.04) × 天空 ≈ 0.03 HDR，经 α 乘积后 ≪ 自发光 2.0），
//!   片段输出 ≈ 自发光值，混合结果可精确预测；
//! - α 混合（`src α, 1 − src α`）逐层复合：远层先画，n 层 α=0.5、
//!   自发光 E 的合成中第 k 近层的系数为 αᵏ（最近层 α¹）；
//! - 透明板背后垫**自发光 0 的不透明黑垫板**：天空不再透入（黑垫板
//!   HDR 输出 ≈ 0.03，经 α 乘积渗入 ≤ 0.004 HDR，显示域 < 1/255）。
//!
//! 容差：逐通道 ±10/255（8-bit 量化 + 上述残余 IBL 项，量级 < 2/255，
//! 10 为裕量）。顺序错误（如近→远）会得到通道互换的结果——与参考值差
//! 数十级 / 255，断言天然区分。
//!
//! **每个场景变体用独立 `OffscreenRenderer`**：渲染器的 GPU 材质缓存按
//! 句柄索引跨渲染持久（句柄即内容的既有引擎语义），同句柄不同定义的
//! 两个场景复用同一渲染器会命中首个场景的陈旧材质——独立渲染器也使
//! 帧统计（`FrameStats`）互不串扰。
//!
//! 用例五「旧场景兼容」（无 alpha_mode 字段的 JSON → Opaque）是纯 CPU
//! 单测，见 `iris-engine/src/scene.rs` 的
//! `alpha_mode_legacy_default_and_blend_roundtrip`。

use glam::Vec3;
use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Transform, World};
use iris_render::prelude::*;

/// 测试用自发光强度：ACES 压缩后各通道仍可分辨（>40/255 间隔）。
const E: f32 = 2.0;
/// 透明板混合系数。
const A: f32 = 0.5;

/// 基础世界：空目录 + 正视相机（+Z 轴上看向原点，视线 = −Z）+
/// 方向光（关阴影，消除采样不确定性）+ 泛光关闭。
fn base_world() -> World {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    world.insert_resource(TextureStore::new());
    world.insert_resource(Camera::default());
    {
        let mut camera = world.resource_mut::<Camera>();
        camera.eye = Vec3::new(0.0, 0.0, 12.0);
        camera.target = Vec3::ZERO;
    }
    world.insert_resource(DirectionalLight {
        shadows: false,
        ..DirectionalLight::default()
    });
    world.insert_resource(BloomSettings::disabled());
    world
}

/// 自发光材质：albedo 纯黑（光照项可忽略，见模块文档）+ 自发光 (r,g,b)。
/// `blend = true` 时 alpha 混合（混合系数 a），否则不透明（参考立方体用）。
fn emissive_mat(r: f32, g: f32, b: f32, a: f32, blend: bool) -> MaterialDef {
    MaterialDef {
        base_color: [0.0, 0.0, 0.0, a],
        emissive: [r, g, b, 1.0],
        alpha_mode: if blend {
            AlphaMode::Blend
        } else {
            AlphaMode::Opaque
        },
        ..Default::default()
    }
}

/// 透明板：扁立方体（正面 +Z 朝相机，背面剔除后单层混合）。
fn spawn_plate(world: &mut World, cube: MeshHandle, x: f32, z: f32, mat: MaterialHandle) {
    world.spawn((
        Transform {
            position: Vec3::new(x, 0.0, z),
            scale: Vec3::new(1.6, 1.6, 0.1),
            ..Transform::IDENTITY
        },
        cube,
        mat,
    ));
}

/// 中心像素 [r, g, b]。
fn center_pixel(px: &[u8], w: u32, h: u32) -> [i32; 3] {
    let i = ((h / 2 * w + w / 2) * 4) as usize;
    [px[i] as i32, px[i + 1] as i32, px[i + 2] as i32]
}

/// 逐通道 ±tol 比较（参考渲染法，见模块文档）。
fn assert_pixel_near(got: [i32; 3], want: [i32; 3], tol: i32, what: &str) {
    for (name, (g, w)) in ["r", "g", "b"].iter().zip(got.iter().zip(want.iter())) {
        assert!(
            (g - w).abs() <= tol,
            "{what}：{name} 通道 {g} 应在 {w} ±{tol} 内"
        );
    }
}

/// 用例一「混合正确性」：红色自发光不透明立方体后置（z=−1.5）+ 蓝色
/// Blend（α=0.5）透明板前置（z=0）。中心像素 = 0.5×蓝 + 0.5×红 =
/// 自发光 (E/2, 0, E/2)，与「单不透明立方体自发光 (1, 0, 1)」的参考
/// 渲染比对。同时断言 FrameStats：透明计入 instances / triangles，
/// 透明普通项构成独立 draw call。混合被替换为 REPLACE、透明先于不透明
/// 绘制或透明未画都会显著偏离参考值。
#[test]
fn transparent_blends_over_opaque_geometry() {
    // 待测场景：红不透明在后 + 蓝透明在前
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = base_world();
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let red = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(E, 0.0, 0.0, 1.0, false));
    let blue = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(0.0, 0.0, E, A, true));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 0.0, -1.5),
            scale: Vec3::new(2.0, 2.0, 0.1),
            ..Transform::IDENTITY
        },
        cube,
        red,
    ));
    spawn_plate(&mut world, cube, 0.0, 0.0, blue);
    propagate_transforms(&mut world);
    renderer.render(&world);
    let got = center_pixel(&renderer.read_pixels(), 256, 256);

    // FrameStats：2 实例（不透明 + 透明）、2 draw（不透明批 + 透明 run）、
    // 24 三角形（12 × 2）——透明计入统计且不与不透明合批
    let stats = renderer.renderer().stats();
    assert_eq!(stats.instances, 2, "透明实例计入 instances");
    assert_eq!(stats.draw_calls, 2, "不透明批 + 透明 run 各一次 draw");
    assert_eq!(stats.triangles_drawn, 24, "透明三角形计入统计");

    // 参考场景（独立渲染器，见模块文档）：单不透明立方体自发光 (0.5E, 0, 0.5E)
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = base_world();
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat =
        world
            .resource_mut::<MaterialStore>()
            .add(emissive_mat(E * A, 0.0, E * A, 1.0, false));
    spawn_plate(&mut world, cube, 0.0, 0.0, mat);
    propagate_transforms(&mut world);
    renderer.render(&world);
    let reference = center_pixel(&renderer.read_pixels(), 256, 256);
    assert_pixel_near(got, reference, 10, "半透明蓝叠加在自发光红之上");
}

/// 用例二「排序正确性」（核心）：三块透明板沿视线排列——远蓝（z=−2）、
/// 中绿（z=0）、近红（z=+2），全部 α=0.5、自发光 E、不同材质（强制三项
/// 分开绘制，不互相合批）。解析合成（back-to-front）：
/// 红 over (绿 over (蓝 over 黑垫板)) = (αE, α²E, α³E) = (1.0, 0.5, 0.25)。
/// 若顺序颠倒（近→远）得到 (α³E, α²E, αE) = (0.25, 0.5, 1.0)——r/b 通道
/// 互换，参考断言必然失败；其余任何非 back-to-front 排列同理偏离。
#[test]
fn transparent_sorts_back_to_front() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = base_world();
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    // 黑垫板：自发光 0 的不透明立方体（挡住天空，见模块文档）
    let black = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(0.0, 0.0, 0.0, 1.0, false));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 0.0, -4.0),
            scale: Vec3::new(3.0, 3.0, 0.1),
            ..Transform::IDENTITY
        },
        cube,
        black,
    ));
    let blue = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(0.0, 0.0, E, A, true));
    let green = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(0.0, E, 0.0, A, true));
    let red = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(E, 0.0, 0.0, A, true));
    // 生成顺序故意与深度序相反：排序必须与查询顺序无关
    spawn_plate(&mut world, cube, 0.0, 2.0, red);
    spawn_plate(&mut world, cube, 0.0, -2.0, blue);
    spawn_plate(&mut world, cube, 0.0, 0.0, green);
    propagate_transforms(&mut world);
    renderer.render(&world);
    let got = center_pixel(&renderer.read_pixels(), 256, 256);

    // 参考（独立渲染器）：单不透明立方体自发光 = 解析合成值 (αE, α²E, α³E)
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = base_world();
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let mat = world.resource_mut::<MaterialStore>().add(emissive_mat(
        A * E,
        A * A * E,
        A * A * A * E,
        1.0,
        false,
    ));
    spawn_plate(&mut world, cube, 0.0, 2.0, mat);
    propagate_transforms(&mut world);
    renderer.render(&world);
    let reference = center_pixel(&renderer.read_pixels(), 256, 256);
    assert_pixel_near(got, reference, 10, "三块透明板按远→近排序合成");
}

/// 用例三「深度只测不写」：
/// (a) 近透明板 A（红）之后的不透明蓝立方体与远透明板 B（绿）仍参与
///     合成——后方不透明物透过 A 可见、B 不因 A 被裁（A 若是 REPLACE
///     混合或 B 被裁掉都会偏离参考值）；
/// (b) 两块**等深**共面透明板（红 / 绿）+ 黑垫板：深度写入关闭时两块都
///     通过深度测试（比较对象是不透明垫板的更远深度）；若透明写深度，
///     后画的那块在 Less 测试下被先画板的深度裁掉——红绿两通道必有一个
///     塌到垫板亮度（≈ 26/255）。断言两通道都显著亮于该值（与两板先后
///     无关，断言不依赖并列深度时的实体序）。
#[test]
fn transparent_depth_test_on_write_off() {
    // (a) 三层合成：红(近 z=1) over 绿(z=-1) over 不透明蓝(z=-3)
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = base_world();
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let blue_opaque = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(0.0, 0.0, E, 1.0, false));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 0.0, -3.0),
            scale: Vec3::new(3.0, 3.0, 0.1),
            ..Transform::IDENTITY
        },
        cube,
        blue_opaque,
    ));
    let green = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(0.0, E, 0.0, A, true));
    let red = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(E, 0.0, 0.0, A, true));
    spawn_plate(&mut world, cube, 0.0, 1.0, red);
    spawn_plate(&mut world, cube, 0.0, -1.0, green);
    propagate_transforms(&mut world);
    renderer.render(&world);
    let got = center_pixel(&renderer.read_pixels(), 256, 256);

    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = base_world();
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    // 参考自发光 = 两层透明板对不透明蓝垫板的解析合成：
    // r = αE（红自身），g = (1−α)·αE（绿被红衰减），b = (1−α)²·E
    // （不透明垫板被两层板衰减——垫板是不透明的，无 α 自身项，与
    // 用例二的三层全透明公式不同）
    let mat = world.resource_mut::<MaterialStore>().add(emissive_mat(
        A * E,
        (1.0 - A) * A * E,
        (1.0 - A) * (1.0 - A) * E,
        1.0,
        false,
    ));
    spawn_plate(&mut world, cube, 0.0, 1.0, mat);
    propagate_transforms(&mut world);
    renderer.render(&world);
    let reference = center_pixel(&renderer.read_pixels(), 256, 256);
    assert_pixel_near(
        got,
        reference,
        10,
        "透过近板应见到远板与后方不透明几何的合成",
    );

    // (b) 共面等深透明对：两块都必须出现在合成结果里
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = base_world();
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let black = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(0.0, 0.0, 0.0, 1.0, false));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 0.0, -4.0),
            scale: Vec3::new(3.0, 3.0, 0.1),
            ..Transform::IDENTITY
        },
        cube,
        black,
    ));
    let red = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(E, 0.0, 0.0, A, true));
    let green = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(0.0, E, 0.0, A, true));
    // 同 z=0、横向略偏移使两板部分重叠（中心像素两板都覆盖）
    spawn_plate(&mut world, cube, -0.15, 0.0, red);
    spawn_plate(&mut world, cube, 0.15, 0.0, green);
    propagate_transforms(&mut world);
    renderer.render(&world);
    let [r, g, b] = center_pixel(&renderer.read_pixels(), 256, 256);
    // 两板都在：次序无论先后，被叠在下面的通道 = α²E = 0.5 HDR
    // （sRGB ≈ 203）；若透明写深度，后画板被裁，其一通道塌到垫板
    // 亮度 ≈ 26/255。阈值取 170 居中区分两条带。
    assert!(
        r > 170 && g > 170,
        "共面透明对两块都应通过深度测试参与混合（深度写入必须关闭），实际 r={r} g={g} b={b}"
    );
}

/// 用例四「透明不投影」：只有透明投射物 + 不透明受影地面——开 / 关阴影
/// 两帧差分应几乎为零（透明项不进阴影贴图，地面无影）；对照同场景不透明
/// 版，差分必须出现成片变暗（证明阴影管线本身在该场景有效，透明版
/// 「无影」不是阴影失效的假阴性）。四个变体各自独立渲染器（材质句柄
/// 在透明 / 不透明版下定义不同，见模块文档）。
#[test]
fn transparent_geometry_casts_no_shadow() {
    let render = |alpha_mode: AlphaMode, shadows: bool| {
        // 闭包返回 Option，无 GPU 适配器经 `?` 提前返回 None（调用方优雅跳过）
        let mut renderer = OffscreenRenderer::new(256, 256)?;
        let mut world = World::new();
        world.insert_resource(MeshStore::new());
        world.insert_resource(MaterialStore::new());
        world.insert_resource(TextureStore::new());
        world.insert_resource(Camera::default());
        world.insert_resource(DirectionalLight {
            shadows,
            ..DirectionalLight::default()
        });
        world.insert_resource(BloomSettings::disabled());
        let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
        // 悬浮投射物：不透明版 / 透明版（同色，α=0.5）
        let mat = world.resource_mut::<MaterialStore>().add(MaterialDef {
            base_color: [0.9, 0.3, 0.1, 0.5],
            alpha_mode,
            ..Default::default()
        });
        world.spawn((Transform::from_xyz(0.0, 1.5, 0.0), cube, mat));
        // 受影地面
        let plane = world.resource_mut::<MeshStore>().add(MeshData::plane(40.0));
        let ground = world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.5, 0.5, 0.5, 1.0));
        world.spawn((Transform::IDENTITY, plane, ground));
        propagate_transforms(&mut world);
        renderer.render(&world);
        Some(renderer.read_pixels())
    };

    // 透明版：开 / 关阴影差分 ≈ 0（透明不进阴影 pass）
    let (Some(blend_on), Some(blend_off)) = (
        render(AlphaMode::Blend, true),
        render(AlphaMode::Blend, false),
    ) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut blend_diff = 0u32;
    for i in (0..blend_on.len()).step_by(4) {
        let sum_on = blend_on[i] as i32 + blend_on[i + 1] as i32 + blend_on[i + 2] as i32;
        let sum_off = blend_off[i] as i32 + blend_off[i + 1] as i32 + blend_off[i + 2] as i32;
        if (sum_on - sum_off).abs() > 12 {
            blend_diff += 1;
        }
    }
    assert!(
        blend_diff <= 4,
        "透明投射物不应投下阴影（开/关阴影差分应≈0，MSAA 边缘容差 4），实际 {blend_diff} 像素"
    );

    // 不透明对照版：差分必须成片（阴影管线在该场景正常工作）
    let (Some(opaque_on), Some(opaque_off)) = (
        render(AlphaMode::Opaque, true),
        render(AlphaMode::Opaque, false),
    ) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut opaque_diff = 0u32;
    for i in (0..opaque_on.len()).step_by(4) {
        let sum_on = opaque_on[i] as i32 + opaque_on[i + 1] as i32 + opaque_on[i + 2] as i32;
        let sum_off = opaque_off[i] as i32 + opaque_off[i + 1] as i32 + opaque_off[i + 2] as i32;
        if (sum_on - sum_off).abs() > 12 {
            opaque_diff += 1;
        }
    }
    assert!(
        opaque_diff as f32 > 256.0 * 256.0 * 0.005,
        "不透明对照版应有成片阴影差分（排除阴影管线失效的假阴性），实际 {opaque_diff} 像素"
    );
}

/// 补充：深度序中相邻同 (网格, 材质) 键的透明项合并为一个 run 批——
/// 4 块同材质透明板 + 1 块不透明垫板 = 2 draw call（1 不透明批 + 1 透明
/// run），5 实例 60 三角形全部计入统计。
#[test]
fn transparent_run_batches_same_key() {
    let Some(mut renderer) = OffscreenRenderer::new(256, 256) else {
        eprintln!("跳过：环境无 GPU 适配器");
        return;
    };
    let mut world = base_world();
    let cube = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let black = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(0.0, 0.0, 0.0, 1.0, false));
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 0.0, -6.0),
            scale: Vec3::new(3.0, 3.0, 0.1),
            ..Transform::IDENTITY
        },
        cube,
        black,
    ));
    let glass = world
        .resource_mut::<MaterialStore>()
        .add(emissive_mat(0.0, 0.0, E, A, true));
    for z in [-2.0f32, -1.0, 1.0, 2.0] {
        spawn_plate(&mut world, cube, 0.0, z, glass);
    }
    propagate_transforms(&mut world);
    renderer.render(&world);
    let stats = renderer.renderer().stats();
    assert_eq!(stats.instances, 5, "垫板 + 4 透明板共 5 实例");
    assert_eq!(stats.draw_calls, 2, "同键透明项应合并为单个 run 批");
    assert_eq!(stats.triangles_drawn, 60, "5 × 12 三角形全部计入");
    // 中心像素非黑：透明板确实画了（4 层 α=0.5 蓝叠在黑垫板上）
    let [r, g, b] = center_pixel(&renderer.read_pixels(), 256, 256);
    assert!(
        b > 120 && b > r + 40,
        "蓝色透明板应可见，实际 r={r} g={g} b={b}"
    );
}
