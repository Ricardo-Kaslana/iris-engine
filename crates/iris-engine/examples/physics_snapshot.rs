//! 工具：物理与碰撞 v1 的 headless 断言 + 双时刻视觉验收（离屏渲染）。
//!
//! 用与单测**完全相同**的确定性方式驱动真实物理世界（`PhysicsPlugin` 的
//! Update 并行系统 + `Time::advance` 固定 60Hz 步长 + PostUpdate 层级传播，
//! 不建窗口），场景：地面（plane + 静态 AABB）+ 高台（cube + 静态 AABB）+
//! 10 颗彩色刚体球（restitution 0.2..0.9、半径 0.35..0.55，固定 seed 伪随机
//! 高度与水平抖动）从空中自由落下：
//!
//! - 模拟到 t = 0.3 s（半空，球群仍在下落）→ 离屏渲染左图；
//! - 继续模拟到 t = 3 s → **headless 断言终态**：全部球心 y > 0、球-球与
//!   球-静态体两两不穿透（残余 < 2e-3）、总速度 < 2.0 m/s（近似静止）——
//!   断言失败直接 panic；随后离屏渲染右图（球群落定在台面 / 地面）。
//!
//! 输出 `iris_physics_midair.bmp` / `iris_physics_rest.bmp`（1280×720 各一帧）。
//! 拼图命令（hstack 后删 BMP）：
//! ```sh
//! ffmpeg -y -i iris_physics_midair.bmp -i iris_physics_rest.bmp \
//!     -filter_complex hstack docs/physics_preview.png
//! ```
//!
//! ```sh
//! cargo run -p iris-engine --example physics_snapshot --release
//! ```

use std::time::Duration;

use iris_core::physics::{detect_pair, ColliderShape};
use iris_core::prelude::{Entity, Stage, Time, Transform, Vec3};
use iris_engine::prelude::{
    App, Camera, Collider, DirectionalLight, HierarchyPlugin, MaterialDef, MaterialStore,
    MeshData, MeshStore, OffscreenRenderer, PhysicsPlugin, Plugin, PointLight, RigidBody,
    TextureStore,
};

/// 球数量（规格：8~12）。
const BALL_COUNT: usize = 10;
/// 静止判定：全部球的速度总和上限（米/秒，10 球 ≈ 平均 0.2 m/s/球）。
const SPEED_LIMIT: f32 = 2.0;
/// 穿透判定：终态两两重叠的容差（求解器 2 次松弛的残余量级）。
const PENETRATION_TOLERANCE: f32 = 2e-3;

/// 确定性伪随机（xorshift64\*，固定 seed）：演示的「随机」高度 / 抖动
/// 完全可复现——同一次提交下断言与图像逐位一致。
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// [0, 1) 均匀样本。
    fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let bits = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        ((bits >> 40) as f32) / 16_777_216.0
    }
}

/// 物理世界测试台：资源 + PhysicsPlugin（Update）/ HierarchyPlugin（PostUpdate）
/// + 确定性 60Hz 推进，阶段序与 App::tick 完全一致（headless，无窗口 / 无 GPU）。
struct Harness {
    app: App,
    /// (球实体, 半径)。
    balls: Vec<(Entity, f32)>,
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
        app.world.insert_resource(DirectionalLight { intensity: 1.0, ..DirectionalLight::default() });
        // 暖色主点光（cubemap 阴影）：球体在台面与地面上投出柔和阴影
        app.world.spawn((
            Transform::from_xyz(-4.0, 7.5, 6.0),
            PointLight {
                color: Vec3::new(1.0, 0.85, 0.65),
                intensity: 55.0,
                range: 50.0,
                shadows: true,
            },
        ));
        // 物理（Update）+ 层级传播（PostUpdate）——与 DefaultPlugins 同序：
        // 物理在 Update 写局部变换，PostUpdate 当帧传播为 GlobalTransform
        PhysicsPlugin.build(&mut app);
        HierarchyPlugin.build(&mut app);

        let mut harness = Self { app, balls: Vec::new() };
        harness.build_scene();
        harness
    }

    fn build_scene(&mut self) {
        let sphere_mesh =
            self.app.world.resource_mut::<MeshStore>().add(MeshData::uv_sphere(1.0, 32, 16));
        let cube_mesh = self.app.world.resource_mut::<MeshStore>().add(MeshData::cube());
        let plane_mesh = self.app.world.resource_mut::<MeshStore>().add(MeshData::plane(60.0));

        // ---- 地面：视觉 plane + 物理静态 AABB（顶面 y = 0）----
        let mat_ground = self
            .app
            .world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.17, 0.17, 0.19, 1.0));
        self.app.world.spawn((Transform::IDENTITY, plane_mesh, mat_ground));
        self.app.world.spawn((
            Transform::from_xyz(0.0, -1.0, 0.0),
            Collider::static_box(Vec3::new(30.0, 1.0, 30.0)),
        ));

        // ---- 高台：视觉 cube（缩放即尺寸）+ 物理静态 AABB（顶面 y = 2）----
        let table_half = Vec3::new(3.0, 1.0, 3.0);
        let table_pos = Vec3::new(2.0, 1.0, 0.0);
        let mat_table = self
            .app
            .world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.42, 0.3, 0.22, 1.0));
        self.app.world.spawn((
            Transform { position: table_pos, scale: table_half, ..Transform::IDENTITY },
            cube_mesh,
            mat_table,
        ));
        self.app
            .world
            .spawn((Transform::from_translation(table_pos), Collider::static_box(table_half)));

        // ---- 10 颗刚体球：restitution 0.2..0.9 均布，高度与抖动伪随机 ----
        // 2 行 × 5 列的确定性网格 + 小抖动：水平间距 2.2 保证初始互不重叠
        let mut rng = Rng::new(0x1A2B_3C4D_5E6F_7081);
        let colors: [[f32; 3]; BALL_COUNT] = [
            [0.92, 0.28, 0.22],
            [0.95, 0.62, 0.14],
            [0.95, 0.86, 0.25],
            [0.45, 0.78, 0.26],
            [0.22, 0.72, 0.45],
            [0.22, 0.68, 0.78],
            [0.26, 0.48, 0.88],
            [0.52, 0.34, 0.85],
            [0.82, 0.30, 0.62],
            [0.88, 0.88, 0.92],
        ];
        for i in 0..BALL_COUNT {
            let x = -4.4 + (i % 5) as f32 * 2.2 + (rng.next_f32() - 0.5) * 0.8;
            let z = (if i < 5 { -1.1 } else { 1.1 }) + (rng.next_f32() - 0.5) * 0.8;
            let y = 3.5 + rng.next_f32() * 3.0;
            let radius = 0.35 + rng.next_f32() * 0.2;
            let restitution = 0.2 + 0.7 * (i as f32 / (BALL_COUNT - 1) as f32);
            let color = colors[i];
            let mat = self
                .app
                .world
                .resource_mut::<MaterialStore>()
                .add(MaterialDef::solid(color[0], color[1], color[2], 1.0));
            let entity = self.app.world.spawn((
                Transform {
                    position: Vec3::new(x, y, z),
                    scale: Vec3::splat(radius),
                    ..Transform::IDENTITY
                },
                sphere_mesh,
                mat,
                Collider::dynamic_sphere(radius, restitution),
                // 线性阻尼 1.5 + 摩擦 0.6：3 秒内把弹跳能量耗散到近似静止
                RigidBody { friction: 0.6, linear_damping: 1.5, ..RigidBody::default() },
            ));
            self.balls.push((entity, radius));
        }

        {
            let mut camera = self.app.world.resource_mut::<Camera>();
            camera.eye = Vec3::new(10.5, 6.0, 13.0);
            camera.target = Vec3::new(0.5, 1.6, 0.0);
        }
    }

    /// 确定性推进一帧：advance(1/60) → Update（物理）→ PostUpdate（层级传播）。
    fn frame(&mut self) {
        self.app
            .world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(1.0 / 60.0));
        self.app.schedule.run(Stage::Update, &mut self.app.world);
        self.app.schedule.run(Stage::PostUpdate, &mut self.app.world);
    }

    fn frames(&mut self, count: usize) {
        for _ in 0..count {
            self.frame();
        }
    }

    /// 终态球心位置 + 半径（断言与写图共用）。
    fn ball_states(&self) -> Vec<(Vec3, f32)> {
        self.balls
            .iter()
            .map(|&(entity, radius)| {
                (self.app.world.get::<Transform>(entity).expect("球应有 Transform").position, radius)
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

    // ---- 时刻一：t = 0.3 s（18 步）——球群半空中 ----
    harness.frames(18);
    renderer.render(&harness.app.world);
    renderer.save_bmp("iris_physics_midair.bmp").expect("写 BMP");

    // ---- 推进到 t = 3 s（共 180 步 @ 60Hz）----
    harness.frames(162);

    // ---- headless 终态断言（与离屏渲染同一世界状态）----
    let mut total_speed = 0.0f32;
    for &(entity, _) in &harness.balls {
        let velocity =
            harness.app.world.get::<RigidBody>(entity).expect("球应有 RigidBody").velocity;
        total_speed += velocity.length();
    }
    let states = harness.ball_states();
    for (i, &(position, _)) in states.iter().enumerate() {
        assert!(
            position.y > 0.0,
            "球 {i} 应停在地面之上，实际 y = {}",
            position.y
        );
    }
    // 球-球两两不穿透
    for (i, &(pa, ra)) in states.iter().enumerate() {
        for &(pb, rb) in states.iter().skip(i + 1) {
            let overlap = ra + rb - pa.distance(pb);
            assert!(
                overlap <= PENETRATION_TOLERANCE,
                "球 {i} 与另一球残余穿透 {overlap} 超容差 {PENETRATION_TOLERANCE}"
            );
        }
        // 球-静态体不穿透（地面 AABB 顶面 y = 0、台面 AABB 顶面 y = 2）
        let ground = (
            Vec3::new(0.0, -1.0, 0.0),
            ColliderShape::Box { half_extents: Vec3::new(30.0, 1.0, 30.0) },
        );
        let table =
            (Vec3::new(2.0, 1.0, 0.0), ColliderShape::Box { half_extents: Vec3::new(3.0, 1.0, 3.0) });
        for (pos, shape) in [ground, table] {
            if let Some(contact) =
                detect_pair(pa, &ColliderShape::Sphere { radius: ra }, pos, &shape)
            {
                assert!(
                    contact.penetration <= PENETRATION_TOLERANCE,
                    "球 {i} 与静态体残余穿透 {} 超容差 {PENETRATION_TOLERANCE}",
                    contact.penetration
                );
            }
        }
    }
    assert!(
        total_speed < SPEED_LIMIT,
        "3 秒后应近似静止：总速度 {total_speed} ≥ 阈值 {SPEED_LIMIT}"
    );
    println!(
        "终态断言通过：{BALL_COUNT} 球全部 y > 0、两两残余穿透 ≤ {PENETRATION_TOLERANCE}、总速度 {total_speed:.3} < {SPEED_LIMIT} m/s"
    );

    // ---- 时刻二：t = 3 s（静止群像）----
    renderer.render(&harness.app.world);
    renderer.save_bmp("iris_physics_rest.bmp").expect("写 BMP");

    println!("已输出 iris_physics_midair.bmp / iris_physics_rest.bmp");
    println!("拼图：ffmpeg -y -i iris_physics_midair.bmp -i iris_physics_rest.bmp -filter_complex hstack docs/physics_preview.png");
}
