//! iris-render CPU 侧基准（criterion，R33）。
//!
//! 全部为 **GPU 无关的纯 CPU 路径**（不构造设备即可测）：
//!
//! - `frustum_extract_and_test`：Gribb-Hartmann 平面提取 + 1000 次球 /
//!   AABB 混合相交测试——每帧剔除的第一段固定成本；
//! - `bc1_encode_256`：256² 纹理 BC1 编码（上传期一次性成本，决定大
//!   纹理加载的 CPU 停顿上界）；
//! - `instance_diff_10k`：1 万实例 10% 变化的增量上传分段（帧级比对
//!   的 CPU 开销，决定「静态场景零上传」决策的代价）；
//! - `collect_instances_5k`：真实 World + MeshStore 下 5k 立方体的
//!   收集 + 视锥剔除（最接近帧预算的指标——`Renderer::draw` 每帧
//!   GPU 编码前的全部 CPU 收集工作）。
//!
//! ```sh
//! cargo bench -p iris-render
//! ```

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use glam::{Mat4, Vec3};
use iris_core::prelude::{GlobalTransform, Transform};
use iris_render::prelude::{
    CpuCollector, Frustum, InstanceRaw, MaterialHandle, MaterialStore, MeshData, MeshHandle,
    MeshStore, diff_changed_ranges, encode_bc1,
};

/// 确定性 LCG（与既有测试同款，不引依赖）。
struct Lcg(u64);

impl Lcg {
    fn next_f32(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// 相机视图投影（look_at 原点、透视 60°，与引擎 z ∈ [0,1] 约定一致）。
fn view_proj() -> Mat4 {
    let view =
        glam::camera::rh::view::look_at_mat4(Vec3::new(0.0, 12.0, 18.0), Vec3::ZERO, Vec3::Y);
    let proj = glam::camera::rh::proj::directx::perspective(
        std::f32::consts::FRAC_PI_3,
        16.0 / 9.0,
        0.1,
        100.0,
    );
    proj * view
}

/// 256² 程序化纹理（棋盘 + 渐变混合，编码路径覆盖「端点搜索 + 索引
/// 匹配」的全部分支，纯色块会让搜索退化成最优情形、数字虚高）。
fn texture_rgba256() -> Vec<u8> {
    const SIZE: u32 = 256;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let checker = ((x / 8) + (y / 8)) % 2 == 0;
            let r = if checker { x } else { y } as u8;
            let g = ((x ^ y) & 0xff) as u8;
            let b = ((x + y) & 0xff) as u8;
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    rgba
}

/// 5k 立方体场景：100 × 50 网格铺开（部分在视锥外 → 剔除路径真实参与）。
fn collect_world() -> (iris_core::world::World, MeshStore, MaterialStore) {
    let mut world = iris_core::world::World::new();
    let mut meshes = MeshStore::new();
    let mut materials = MaterialStore::new();
    let mesh = meshes.add(MeshData::cube());
    let material = materials.add(iris_render::prelude::MaterialDef::solid(0.8, 0.4, 0.2, 1.0));
    for i in 0..5_000u32 {
        let x = (i % 100) as f32 * 2.0 - 100.0;
        let z = (i / 100) as f32 * 2.0 - 50.0;
        let t = Transform::from_xyz(x, 0.0, z);
        world.spawn((
            GlobalTransform::from_transform(t),
            MeshHandle(mesh.0),
            MaterialHandle(material.0),
        ));
    }
    (world, meshes, materials)
}

fn bench_render_cpu(c: &mut Criterion) {
    // ---- 视锥：提取 + 1000 次混合相交测试 ----
    let vp = view_proj();
    let mut rng = Lcg(0x5EED_2026);
    // 1000 个混合目标的确定性布局：球心 / AABB 散布在相机前方场域
    let spheres: Vec<(Vec3, f32)> = (0..1000)
        .map(|_| {
            (
                Vec3::new(
                    rng.next_f32() * 60.0 - 30.0,
                    rng.next_f32() * 20.0 - 5.0,
                    -rng.next_f32() * 40.0,
                ),
                0.3 + rng.next_f32(),
            )
        })
        .collect();
    let aabbs: Vec<(Vec3, Vec3)> = (0..1000)
        .map(|_| {
            let center = Vec3::new(
                rng.next_f32() * 60.0 - 30.0,
                rng.next_f32() * 20.0 - 5.0,
                -rng.next_f32() * 40.0,
            );
            let half = Vec3::splat(0.3 + rng.next_f32());
            (center - half, center + half)
        })
        .collect();
    c.bench_function("frustum_extract_and_test", |b| {
        b.iter(|| {
            let frustum = Frustum::from_view_proj(black_box(&vp));
            let mut visible = 0usize;
            for i in 0..1000 {
                // 球 / AABB 交替：两种剔除测试的热度均衡
                if i % 2 == 0 {
                    let (center, radius) = spheres[i / 2];
                    visible += frustum.intersects_sphere(center, radius) as usize;
                } else {
                    let (min, max) = aabbs[i / 2];
                    visible += frustum.intersects_aabb(min, max) as usize;
                }
            }
            black_box(visible)
        })
    });

    // ---- BC1：256² 程序化纹理编码（输入 262144 字节）----
    let rgba = texture_rgba256();
    c.bench_function("bc1_encode_256", |b| {
        b.iter(|| black_box(encode_bc1(black_box(&rgba), 256, 256)))
    });

    // ---- 实例增量比对：1 万实例、每第 10 个变化（10%）----
    let prev: Vec<InstanceRaw> = (0..10_000u32)
        .map(|i| InstanceRaw::from_matrix(Mat4::from_translation(Vec3::new(i as f32, 0.0, 0.0))))
        .collect();
    let mut next = prev.clone();
    for (i, raw) in next.iter_mut().enumerate() {
        if i % 10 == 0 {
            *raw = InstanceRaw::from_matrix(Mat4::from_translation(Vec3::new(
                i as f32,
                1.0 + (i % 7) as f32 * 0.1,
                0.0,
            )));
        }
    }
    c.bench_function("instance_diff_10k", |b| {
        b.iter(|| black_box(diff_changed_ranges(black_box(&prev), black_box(&next))))
    });

    // ---- 帧收集：真实 World + MeshStore，5k 立方体收集 + 剔除 ----
    let (world, meshes, materials) = collect_world();
    let vp = view_proj();
    let frustum = Frustum::from_view_proj(&vp);
    let eye = Vec3::new(0.0, 12.0, 18.0);
    let view = glam::camera::rh::view::look_at_mat4(eye, Vec3::ZERO, Vec3::Y);
    let mut collector = CpuCollector::new();
    // 预热一次（scratch 缓冲扩容，与渲染器帧间复用语义一致）
    let warm = collector.collect(&world, &meshes, &materials, &view, eye, &frustum);
    assert!(
        warm.visible > 0 && warm.culled > 0,
        "5k 场景应同时含可见与被剔实例（实际 {warm:?}）"
    );
    c.bench_function("collect_instances_5k", |b| {
        b.iter(|| {
            collector.collect(
                black_box(&world),
                black_box(&meshes),
                black_box(&materials),
                black_box(&view),
                black_box(eye),
                black_box(&frustum),
            )
        })
    });
}

criterion_group!(benches, bench_render_cpu);
criterion_main!(benches);
