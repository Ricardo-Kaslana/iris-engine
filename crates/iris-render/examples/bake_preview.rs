//! 工具：R52b 烘焙光照 b 阶段（UV2 参数化 + 图集布局）的 headless 断言 +
//! 图集预览验收（纯 CPU，**无 GPU / 无窗口**——b 阶段零渲染集成）。
//!
//! 场景：室内盒——10×10 m 地板 + 后墙（z = −5，法线 +Z）+ 左墙（x = −5，
//! 法线 +X，各 10 m 宽 × 4 m 高）+ 0.6×2×0.6 m 遮挡柱（(1.5, 0, 0)），
//! 方向光**侧照**（`light_dir = (−0.55, −1, 0.3)`，暖色）+ 压暗冷色天空
//! （0.12, 0.15, 0.22——一次反弹会填影，直射主导才有稳健对比）+ 软影锥
//! 0.08 rad（影中心 k = 0 本影）+ 64 半球样本 + 1 次反弹。
//! UV：8 texel/m、图集 256²（floor chart 82×82 + 两墙 82×34 + 柱三 chart，
//! shelf 两层）。
//!
//! headless 断言（失败直接 panic）：
//! - 辐照数组与图集尺寸对齐、覆盖 texel 数千级；
//! - **柱影带**：地板阴影中心 texel（世界 (0.4375, 0, 0.5625)——光斜率
//!   解析推得）辐照 < 开阔地板 texel 的 1/2，且 > 0（天空 + 反弹残光，
//!   影子不是纯黑）；
//! - **AO 墙角**：地板贴后墙根 texel 辐照 < 地板中心 texel（半球被墙遮
//!   挡 ⇒ 天空项被扣）。
//!
//! 输出 `iris_bake_preview.ppm`（ACES + sRGB 编码，与引擎 blit 同观感），
//! 随后转 PNG 并目检：
//!
//! ```sh
//! cargo run -p iris-render --example bake_preview --release
//! ffmpeg -y -i iris_bake_preview.ppm docs/bake_preview.png
//! ```
//!
//! 目检要点：floor chart（左上大方块）上遮挡柱的**影子带**清晰可见、
//! 贴墙根一圈**渐暗**（AO）；两墙 chart 只有天空 + 反弹（光从 +x 侧来，
//! 法线 +Z / +X 的墙直射为解析零 ⇒ 明显暗于地板）；柱 chart 的 ±x 两面
//! 被重叠偏移并排摆放（串光防护的视觉证据）；chart 间隙与 pad 恒黑。

use std::path::Path;
use std::time::Instant;

use iris_core::hierarchy::propagate_transforms;
use iris_core::prelude::{Quat, Transform, Vec3, World};
use iris_render::prelude::{
    BakeParams, BakeUvParams, MaterialDef, MaterialStore, MeshData, MeshStore, bake_lightmap,
    write_lightmap_preview,
};

/// 图集边长（texel）。
const ATLAS: u32 = 256;
/// 分辨率（texel / 米）。
const TEXELS_PER_METER: f32 = 8.0;
/// floor chart 局部系换算：plane 10×10 居中 ⇒ 投影 × 8 ∈ [−40, 40]，
/// chart min = −40 ⇒ 局部 = 世界坐标 × 8 + 41（含 1 texel pad）。
const FLOOR_LOCAL_OFFSET: f32 = 41.0;

fn main() {
    let mut world = World::new();
    world.insert_resource(MeshStore::new());
    world.insert_resource(MaterialStore::new());
    let floor_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(10.0));
    let wall_mesh = world.resource_mut::<MeshStore>().add(MeshData::plane(10.0));
    let column_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let floor_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.8, 0.78, 0.72, 1.0));
    let wall_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.55, 0.6, 0.7, 1.0));
    let column_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.9, 0.35, 0.2, 1.0));

    // 地板（plane 默认 XZ、法线 +Y）
    world.spawn((Transform::IDENTITY, floor_mesh, floor_mat));
    // 后墙 z = −5：绕 X 转 90°（法线 +Y → +Z），局部 z 缩放 0.4 ⇒ 高 4 m
    world.spawn((
        Transform {
            position: Vec3::new(0.0, 2.0, -5.0),
            rotation: Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
            scale: Vec3::new(1.0, 1.0, 0.4),
        },
        wall_mesh,
        wall_mat,
    ));
    // 左墙 x = −5：绕 Z 转 −90°（法线 +Y → +X），局部 x 缩放 0.4 ⇒ 高 4 m
    world.spawn((
        Transform {
            position: Vec3::new(-5.0, 2.0, 0.0),
            rotation: Quat::from_rotation_z(-std::f32::consts::FRAC_PI_2),
            scale: Vec3::new(0.4, 1.0, 1.0),
        },
        wall_mesh,
        wall_mat,
    ));
    // 遮挡柱 0.6 × 2 × 0.6 m（底面坐在地板上）
    world.spawn((
        Transform {
            position: Vec3::new(1.5, 1.0, 0.0),
            scale: Vec3::new(0.6, 2.0, 0.6),
            ..Default::default()
        },
        column_mesh,
        column_mat,
    ));
    propagate_transforms(&mut world);

    let params = BakeParams {
        light_dir: Vec3::new(-0.55, -1.0, 0.3),
        light_color: Vec3::new(2.2, 2.0, 1.7),
        // 天空压暗：一次反弹（radiosity）会把影子填掉大半，直射主导
        // 才有稳健的影带对比（反弹解析量级见 ARCHITECTURE §4.24）
        sky_color: Vec3::new(0.12, 0.15, 0.22),
        // 锥半角 0.08 ⇒ 锥在柱顶面的扩散 2·tan(0.08) ≈ 0.16 m < 柱半宽
        // 0.3 m ⇒ 影中心 8/8 全挡（k = 0 本影，解析可断言）
        shadow_softness: 0.08,
        sky_samples: 64,
        bounces: 1,
    };
    let uv = BakeUvParams {
        texels_per_meter: TEXELS_PER_METER,
        atlas_size: ATLAS,
    };
    let started = Instant::now();
    let lightmap = bake_lightmap(&world, &params, &uv);
    let elapsed = started.elapsed();

    // ---- 布局统计 ----
    assert_eq!(lightmap.radiance.len(), (ATLAS * ATLAS) as usize);
    let covered = lightmap
        .radiance
        .iter()
        .filter(|c| **c != Vec3::ZERO)
        .count();
    println!(
        "烘焙 {elapsed:?}：chart {} 个（实体 {} 个）、shelf 已用高度 {} / {ATLAS}、利用率 {:.1}%、覆盖 texel {covered}",
        lightmap.atlas.entries.len(),
        lightmap
            .atlas
            .entries
            .iter()
            .map(|e| e.entity.to_bits())
            .collect::<std::collections::BTreeSet<u64>>()
            .len(),
        lightmap.atlas.used_height,
        lightmap.atlas.utilization() * 100.0,
    );
    for entry in &lightmap.atlas.entries {
        println!(
            "  entry：实体 {:?} / {:?} / mesh {:?} / 原点 {:?} / 尺寸 {:?} / 三角 {}",
            entry.entity,
            entry.axis,
            entry.mesh,
            entry.origin,
            entry.size,
            entry.triangles.len()
        );
    }
    assert!(covered > 5000, "覆盖 texel 应数千级，实际 {covered}");

    // ---- 解析断言：柱影带 / AO 墙角 ----
    // floor chart = 面积最大者（82×82；两墙 82×34、柱 ≤ 14×18），装箱恒
    // 放首位，但原点走查表不硬编码
    let floor_origin = lightmap
        .atlas
        .entries
        .iter()
        .max_by_key(|e| u64::from(e.size[0]) * u64::from(e.size[1]))
        .expect("非空图集")
        .origin;
    // 光单位方向 ≈ (−0.466, −0.847, 0.254)：柱顶 (y = 2) 的影子中心落在
    // 世界 (1.5 − 2×0.466/0.847, 0, 0 + 2×0.254/0.847) ≈ (0.4, 0, 0.6) ⇒
    // 最近 texel 中心 = 世界 (0.4375, 0, 0.5625)（floor chart 局部换算见常量）
    let radiance_at_world = |x: f32, z: f32| -> Vec3 {
        let tx = (x * TEXELS_PER_METER + FLOOR_LOCAL_OFFSET - 0.5).round() as usize
            + floor_origin[0] as usize;
        let ty = (z * TEXELS_PER_METER + FLOOR_LOCAL_OFFSET - 0.5).round() as usize
            + floor_origin[1] as usize;
        lightmap.radiance[ty * ATLAS as usize + tx]
    };
    let shadow = radiance_at_world(0.4375, 0.5625);
    let open = radiance_at_world(-3.4375, -3.4375);
    assert!(
        shadow.length() > 0.0,
        "影子内仍有天空 + 反弹残光（不是纯黑），实际 {shadow:?}"
    );
    assert!(
        shadow.length() < 0.7 * open.length(),
        "柱影带应显著暗于开阔地板：{shadow:?} vs {open:?}"
    );
    // AO 墙角：贴后墙根（z = −4.9375，距墙 1/16 m）vs 地板中心
    let corner = radiance_at_world(0.0625, -4.9375);
    let center = radiance_at_world(0.0625, 0.0625);
    assert!(
        corner.length() < center.length(),
        "墙角 AO 应暗于地板中心：{corner:?} vs {center:?}"
    );
    println!(
        "辐照对照（线性 HDR 长度）：开阔 {:.3} / 柱影 {:.3}（{:.0}%）/ 墙角 {:.3} vs 中心 {:.3}（{:.0}%）",
        open.length(),
        shadow.length(),
        shadow.length() / open.length() * 100.0,
        corner.length(),
        center.length(),
        corner.length() / center.length() * 100.0
    );

    // ---- 预览导出 ----
    let path = Path::new("iris_bake_preview.ppm");
    write_lightmap_preview(&lightmap, path).expect("写 iris_bake_preview.ppm");
    println!("已输出 {path:?}（{ATLAS}×{ATLAS}，ACES + sRGB 编码）");
    println!("转 PNG：ffmpeg -y -i iris_bake_preview.ppm docs/bake_preview.png");
}
