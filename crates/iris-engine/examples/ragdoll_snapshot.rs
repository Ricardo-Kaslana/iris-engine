//! 工具：R37 布娃娃与死亡混合 v1 的 headless 断言 + 双时刻视觉验收
//! （离屏渲染 → `docs/ragdoll.png`）。
//!
//! 用与单测**完全相同**的确定性方式驱动真实物理世界（`PhysicsPlugin` 的
//! Update 并行系统 + `Time::advance` 固定 60Hz 步长 + PostUpdate 层级传播，
//! 不建窗口），场景：平地（plane + 静态 AABB）+ 六球人形布娃娃
//! （[`RagdollDesc::humanoid`]，`spawn_ragdoll_kinematic` 生成静态肢体 +
//! 相邻 [`DistanceJoint`](iris_core::physics::DistanceJoint) 链）：
//!
//! - 模拟到 t = 0.3 s（**动画接管期**：肢体静态定格站立，纹丝不动）→
//!   离屏渲染左图；
//! - [`release_ragdoll`] 翻转动体（**物理接管期**：继承速度 + 侧向冲量
//!   踢倒）→ 继续模拟 180 步（t = 3.3 s）→ **headless 断言终态**：链上
//!   相邻肢体距离保持 rest ± 容差（链未散架）、质心显著塌落（塌地）、
//!   全部肢体落在地面附近且无穿地 / 悬浮 / 爆炸——断言失败直接 panic；
//!   随后离屏渲染右图（肢体塌堆在地面）。
//!
//! 目检要点：左 = 直立定格（kinematic 期静态体）、右 = 塌地不散架、无
//! 穿地 / 悬浮的肢体。输出两帧横拼为 `docs/ragdoll.png`（2560×720，
//! image 直接拼接，不依赖 ffmpeg）。
//!
//! ```sh
//! cargo run -p iris-engine --example ragdoll_snapshot --release
//! ```

use std::time::Duration;

use image::GenericImage;

use iris_core::physics::{ColliderShape, detect_pair};
use iris_core::prelude::{Entity, Stage, Time, Transform, Vec3};
use iris_core::ragdoll::{RagdollDesc, release_ragdoll, spawn_ragdoll_kinematic};
use iris_engine::prelude::{
    App, Camera, Collider, DirectionalLight, HierarchyPlugin, MaterialDef, MaterialStore, MeshData,
    MeshStore, OffscreenRenderer, PhysicsPlugin, Plugin, PointLight, RigidBody, TextureStore,
};

/// 释放后推进步数（规格：180 步 @ 60Hz = 3 s 物理接管期）。
const RELEASE_STEPS: usize = 180;
/// 链距容差：终态相邻肢体距离与 rest 的最大偏差（硬约束 2 轮松弛的
/// 残余量级，与单测 FINAL_TOLERANCE 同级）。
const CHAIN_TOLERANCE: f32 = 2e-2;
/// 穿地 / 悬浮判定：肢体球心相对「静置在地面」的允许偏差（求解残余
/// 穿透 + 堆叠起伏的裕度）。
const GROUND_TOLERANCE: f32 = 5e-2;
/// 静止判定：全部肢体速度总和上限（3 s 阻尼 + 摩擦后应近似静止）。
const SPEED_LIMIT: f32 = 1.5;

/// 物理世界测试台：资源 + PhysicsPlugin（Update）/ HierarchyPlugin
/// （PostUpdate）+ 确定性 60Hz 推进，阶段序与 App::tick 完全一致
/// （headless，无窗口 / 无 GPU）。
struct Harness {
    app: App,
    /// 肢体实体序 = `desc.limbs` 定义序。
    limbs: Vec<Entity>,
    desc: RagdollDesc,
    /// 出生点（肢体 offset 的基准）。
    origin: Vec3,
}

impl Harness {
    fn new() -> Self {
        let mut app = App::new();
        // DefaultPlugins 等价资源（离屏路径无窗口 / 无音频需求）
        app.world.insert_resource(Time::default());
        app.world.insert_resource(MeshStore::new());
        app.world.insert_resource(MaterialStore::new());
        app.world.insert_resource(TextureStore::new());
        app.world.insert_resource(Camera::default());
        app.world.insert_resource(DirectionalLight {
            intensity: 1.0,
            ..DirectionalLight::default()
        });
        // 暖色主点光：肢体球在地面投出柔和阴影（立体感 → 目检穿地 / 悬浮）
        app.world.spawn((
            Transform::from_xyz(-3.0, 6.0, 5.0),
            PointLight {
                color: Vec3::new(1.0, 0.9, 0.78),
                intensity: 45.0,
                range: 40.0,
                shadows: true,
            },
        ));
        // 物理（Update）+ 层级传播（PostUpdate）——与 DefaultPlugins 同序
        PhysicsPlugin.build(&mut app);
        HierarchyPlugin.build(&mut app);

        let mut harness = Self {
            app,
            limbs: Vec::new(),
            desc: RagdollDesc::humanoid(),
            origin: Vec3::ZERO,
        };
        harness.build_scene();
        harness
    }

    fn build_scene(&mut self) {
        let sphere_mesh = self
            .app
            .world
            .resource_mut::<MeshStore>()
            .add(MeshData::uv_sphere(1.0, 32, 16));
        let plane_mesh = self
            .app
            .world
            .resource_mut::<MeshStore>()
            .add(MeshData::plane(60.0));

        // ---- 地面：视觉 plane + 物理静态 AABB（顶面 y = 0）----
        let mat_ground = self
            .app
            .world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.17, 0.17, 0.19, 1.0));
        self.app
            .world
            .spawn((Transform::IDENTITY, plane_mesh, mat_ground));
        self.app.world.spawn((
            Transform::from_xyz(0.0, -1.0, 0.0),
            Collider::static_box(Vec3::new(30.0, 1.0, 30.0)),
        ));

        // ---- 六球人形：静态肢体（动画接管期）+ 视觉球（半径各异）----
        // 头 / 躯干 / 双手 / 双脚各自配色，塌地后的姿态一目了然
        let limb_colors: [[f32; 3]; 6] = [
            [0.90, 0.76, 0.62], // 头（肤色）
            [0.25, 0.45, 0.80], // 躯干（蓝）
            [0.85, 0.30, 0.25], // 左手（红）
            [0.85, 0.55, 0.20], // 右手（橙）
            [0.30, 0.30, 0.34], // 左脚（深灰）
            [0.30, 0.30, 0.34], // 右脚（深灰）
        ];
        // 死亡混合第一阶段：按死亡姿势生成静态肢体（出生点 = 原点，
        // humanoid 的脚底贴地）。运行时实体再挂视觉组件（mesh + 材质 +
        // 半径缩放）。
        let limbs = spawn_ragdoll_kinematic(&mut self.app.world, self.origin, &self.desc);
        for (index, (&entity, color)) in limbs.iter().zip(&limb_colors).enumerate() {
            let def = &self.desc.limbs[index];
            let mat = self
                .app
                .world
                .resource_mut::<MaterialStore>()
                .add(MaterialDef::solid(color[0], color[1], color[2], 1.0));
            self.app.world.insert(entity, sphere_mesh);
            self.app.world.insert(entity, mat);
            let mut transform = *self
                .app
                .world
                .get::<Transform>(entity)
                .expect("肢体应有 Transform");
            transform.scale = Vec3::splat(def.radius);
            self.app.world.insert(entity, transform);
            self.limbs.push(entity);
        }

        {
            let mut camera = self.app.world.resource_mut::<Camera>();
            camera.eye = Vec3::new(3.4, 2.4, 5.2);
            camera.target = Vec3::new(0.3, 0.7, 0.0);
        }
    }

    /// 确定性推进一帧：advance(1/60) → Update（物理）→ PostUpdate（层级传播）。
    fn frame(&mut self) {
        self.app
            .world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(1.0 / 60.0));
        self.app.schedule.run(Stage::Update, &mut self.app.world);
        self.app
            .schedule
            .run(Stage::PostUpdate, &mut self.app.world);
    }

    fn frames(&mut self, count: usize) {
        for _ in 0..count {
            self.frame();
        }
    }

    /// 终态（位置，速度）快照（断言与报告共用）。
    fn limb_states(&self) -> Vec<(Vec3, Vec3)> {
        self.limbs
            .iter()
            .map(|&entity| {
                let position = self
                    .app
                    .world
                    .get::<Transform>(entity)
                    .expect("肢体应有 Transform")
                    .position;
                let velocity = self
                    .app
                    .world
                    .get::<RigidBody>(entity)
                    .expect("肢体应有 RigidBody")
                    .velocity;
                (position, velocity)
            })
            .collect()
    }
}

fn main() {
    env_logger::init();
    let Some(mut renderer) = OffscreenRenderer::new(1280, 720) else {
        eprintln!("无 GPU");
        std::process::exit(1);
    };

    let mut harness = Harness::new();

    // ---- 时刻一：t = 0.3 s（18 步）——动画接管期，肢体静态站立 ----
    harness.frames(18);
    let states = harness.limb_states();
    for (index, &(position, _)) in states.iter().enumerate() {
        let want = harness.origin + harness.desc.limbs[index].offset;
        assert_eq!(
            position, want,
            "动画接管期肢体 {index} 应 bit 级保持生成姿势（静态体不积分）"
        );
    }
    renderer.render(&harness.app.world);
    let left = renderer.read_pixels();

    // ---- 死亡混合第二阶段：翻转动体 + 侧向冲量踢倒，跑 180 步 ----
    release_ragdoll(
        &harness.app.world,
        &harness.limbs,
        Vec3::new(0.3, 0.0, 0.0),
        Some((Vec3::X, 3.0)),
    );
    harness.frames(RELEASE_STEPS);

    // ---- headless 终态断言（与离屏渲染同一世界状态）----
    let states = harness.limb_states();
    let (positions, velocities): (Vec<Vec3>, Vec<Vec3>) = states.into_iter().unzip();

    // 链未散架：相邻肢体距离保持 rest ± 容差（DistanceJoint 硬约束生效）
    for &(a, b) in &harness.desc.links {
        let rest = (harness.origin + harness.desc.limbs[b].offset)
            .distance(harness.origin + harness.desc.limbs[a].offset);
        let dist = positions[a].distance(positions[b]);
        assert!(
            (dist - rest).abs() <= CHAIN_TOLERANCE,
            "链接 ({a}, {b}) 距离 {dist} 应保持 rest {rest} ± {CHAIN_TOLERANCE}（链未散架）"
        );
    }

    // 塌地：质心显著低于直立姿势（humanoid 直立质心 y ≈ 0.8）
    let com = positions.iter().sum::<Vec3>() / positions.len() as f32;
    assert!(
        com.y < 0.55,
        "终态质心 y = {} 应显著塌落（肢体塌堆在地面）",
        com.y
    );
    // 无悬浮：全部肢体落在地面附近（直立头部 y = 1.55，塌地后必 < 1.0）
    for (index, &position) in positions.iter().enumerate() {
        assert!(
            position.y < 1.0,
            "肢体 {index} 应塌在地面附近（无悬浮），实际 y = {}",
            position.y
        );
        // 无穿地：球-地残余穿透 ≤ 容差（静置球心 y ≥ 半径 − 容差）
        let contact = detect_pair(
            position,
            &ColliderShape::Sphere {
                radius: harness.desc.limbs[index].radius,
            },
            Vec3::new(0.0, -1.0, 0.0),
            &ColliderShape::Box {
                half_extents: Vec3::new(30.0, 1.0, 30.0),
            },
        );
        if let Some(contact) = contact {
            assert!(
                contact.penetration <= GROUND_TOLERANCE,
                "肢体 {index} 穿地 {} 超容差 {GROUND_TOLERANCE}",
                contact.penetration
            );
        }
    }
    // 无爆炸：3 s 阻尼 + 摩擦后近似静止
    let total_speed: f32 = velocities.iter().map(|v| v.length()).sum();
    assert!(
        total_speed < SPEED_LIMIT,
        "3 秒后应近似静止：总速度 {total_speed} ≥ 阈值 {SPEED_LIMIT}"
    );
    println!(
        "终态断言通过：链距 rest ± {CHAIN_TOLERANCE}、质心 y = {:.3}（塌地）、总速度 {total_speed:.3} < {SPEED_LIMIT} m/s",
        com.y
    );

    // ---- 时刻二：物理接管 3 s 后（塌地群像）----
    renderer.render(&harness.app.world);
    let right = renderer.read_pixels();

    // ---- 左右横拼 → docs/ragdoll.png（image 直接拼接，不依赖 ffmpeg）----
    let (w, h) = renderer.size();
    let mut canvas = image::RgbaImage::new(w * 2, h);
    let left = image::RgbaImage::from_raw(w, h, left).expect("左图像素缓冲尺寸匹配");
    let right = image::RgbaImage::from_raw(w, h, right).expect("右图像素缓冲尺寸匹配");
    canvas.copy_from(&left, 0, 0).expect("左图拼入画布");
    canvas.copy_from(&right, w, 0).expect("右图拼入画布");
    canvas
        .save("docs/ragdoll.png")
        .expect("写 docs/ragdoll.png（从仓库根目录运行）");
    println!("已输出 docs/ragdoll.png（{}x{}）", w * 2, h);
}
