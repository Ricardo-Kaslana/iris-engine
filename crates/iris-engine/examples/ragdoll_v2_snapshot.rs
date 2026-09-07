//! 工具：R50 真人形 ragdoll v2（胶囊肢 + 摆动关节限位 + 角速度翻滚）的
//! headless 断言 + 双时刻视觉验收（离屏渲染 → `docs/ragdoll_v2.png`）。
//!
//! 用与单测**完全相同**的确定性方式驱动真实物理世界（`PhysicsPlugin` 的
//! Update 并行系统 + `Time::advance` 固定 60Hz 步长 + PostUpdate 层级传播，
//! 不建窗口）。场景：地面（顶面 y = 0）+ 1 m 高的台阶平台 + 站在平台上的
//! 十肢胶囊人形（[`RagdollDesc::humanoid_v2`]，`spawn_ragdoll_kinematic`
//! 生成静态肢体 + 8 条 [`SwingJoint`] 自动接线 + 1 条头的 `DistanceJoint`）：
//!
//! - 动画接管期 12 步（肢体静态定格站立）后 [`release_ragdoll`]：继承速度
//!   加侧向冲量踢下台阶 + **角速度注入**（绕链根肩线前扑，见
//!   `iris_core::ragdoll` 的翻倒转轴论证）；
//! - 模拟 28 步（t ≈ 0.47 s，**腾空帧**：已越过台缘、躯体前扑离地、四肢
//!   在限位内甩开）→ 离屏渲染左图；
//! - 继续 200 步（t ≈ 3.7 s，**摊倒帧**：落地翻滚后摊平）→ **headless
//!   断言终态**：关节锚距保持 rest（不散架）、摆角全程 ≤ 限位（不反折）、
//!   肢体不穿地、质心塌落、速度收敛、**四肢胶囊不再竖直悬空**（多数肢体
//!   轴接近水平 = 摊倒而非插在地上）、躯干躺平——断言失败直接 panic；
//!   随后离屏渲染右图。
//!
//! 目检要点：左 = 腾空前扑（四肢胶囊**沿肢体方向**、不是竖直插着）、
//! 关节在限位内；右 = 摊倒在地（躯干躺平、四肢摊开、无肢体竖直悬空、
//! 无关节反折、无穿地）。输出两帧横拼为 `docs/ragdoll_v2.png`
//! （2560×720，image 直接拼接，不依赖 ffmpeg）。
//!
//! ```sh
//! cargo run -p iris-engine --example ragdoll_v2_snapshot --release
//! ```

use std::time::Duration;

use image::GenericImage;

use iris_engine::prelude::{
    App, Camera, Collider, DirectionalLight, Entity, HierarchyPlugin, MaterialDef, MaterialHandle,
    MaterialStore, MeshData, MeshStore, OffscreenRenderer, PhysicsPlugin, Plugin, PointLight, Quat,
    RagdollDesc, RagdollLimbDef, RigidBody, Stage, SwingJoint, TextureStore, Time, Transform, Vec3,
    release_ragdoll, spawn_ragdoll_kinematic,
};

/// 释放后到「腾空帧」的步数（28 步 ≈ 0.47 s：已越过台缘、尚未落地）。
const AIRBORNE_STEPS: usize = 28;
/// 腾空帧到「摊倒帧」的步数（200 步 ≈ 3.3 s：落地 + 翻滚 + 阻尼停下）。
const SETTLE_STEPS: usize = 200;
/// 台阶平台顶面高度（出生点 y）。
const PLATFORM_TOP: f32 = 1.0;
/// 肢体的角阻尼（= `RAGDOLL_LIMB_ANGULAR_DAMPING`，打印渐近转角用）。
const ANGULAR_DAMPING: f32 = 1.5;

/// 关节锚距容差：摆动关节是**层级投影**（修正全给子端），每轮末尾约束
/// 精确满足 ⇒ 残差是 f32 舍入量级（单测锁定 1e-4）；示例放宽到 1e-3。
const ANCHOR_TOLERANCE: f32 = 1e-3;
/// 摆角超限容差（同上量级）。
const ANGLE_TOLERANCE: f32 = 1e-3;
/// 穿地容差：与 `examples/ragdoll_snapshot.rs` 同值（2 轮位置松弛的残余
/// 穿透 + 关节把肢体往地面拽的量级）。
const GROUND_TOLERANCE: f32 = 5e-2;
/// 终态静止判定：全部肢体速度总和上限。
const SPEED_LIMIT: f32 = 1.5;
/// 「四肢不再竖直悬空」判定：轴与竖直方向夹角 > 45°（|axis.y| < cos45°）
/// 的胶囊肢至少要有这么多个（9 条胶囊肢里的多数）。
const NON_UPRIGHT_LIMBS: usize = 6;

/// 物理世界测试台：资源 + PhysicsPlugin（Update）/ HierarchyPlugin
/// （PostUpdate）+ 确定性 60Hz 推进，阶段序与 App::tick 完全一致
/// （headless，无窗口 / 无 GPU）。
struct Harness {
    app: App,
    /// 肢体实体序 = `desc.limbs` 定义序（`limbs[0]` = 链根躯干）。
    limbs: Vec<Entity>,
    desc: RagdollDesc,
    /// 每条摆动链接的关节参数快照 `(子端, 父端, 关节)`。
    wired: Vec<(usize, usize, SwingJoint)>,
    /// 出生点（肢体 offset 的基准）= 台阶平台顶面。
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
        // 暖色主点光：肢体在地面投出柔和阴影（立体感 → 目检穿地 / 悬空）
        app.world.spawn((
            Transform::from_xyz(-3.0, 6.0, 5.0),
            PointLight {
                color: Vec3::new(1.0, 0.9, 0.78),
                intensity: 55.0,
                range: 45.0,
                shadows: true,
            },
        ));
        // 物理（Update）+ 层级传播（PostUpdate）——与 DefaultPlugins 同序
        PhysicsPlugin.build(&mut app);
        HierarchyPlugin.build(&mut app);

        let mut harness = Self {
            app,
            limbs: Vec::new(),
            desc: RagdollDesc::humanoid_v2(),
            wired: Vec::new(),
            origin: Vec3::new(0.9, PLATFORM_TOP, 0.0),
        };
        harness.build_scene();
        harness
    }

    fn build_scene(&mut self) {
        let plane_mesh = self
            .app
            .world
            .resource_mut::<MeshStore>()
            .add(MeshData::plane(60.0));
        let platform_mesh = self
            .app
            .world
            .resource_mut::<MeshStore>()
            .add(MeshData::cube());

        // ---- 地面：视觉 plane + 物理静态 AABB（顶面 y = 0）----
        let mat_ground = self
            .app
            .world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.16, 0.16, 0.18, 1.0));
        self.app
            .world
            .spawn((Transform::IDENTITY, plane_mesh, mat_ground));
        self.app.world.spawn((
            Transform::from_xyz(0.0, -1.0, 0.0),
            Collider::static_box(Vec3::new(30.0, 1.0, 30.0)),
        ));

        // ---- 台阶平台：顶面 y = 1.0（ragdoll 从这里被踢下去）----
        let mat_platform = self
            .app
            .world
            .resource_mut::<MaterialStore>()
            .add(MaterialDef::solid(0.30, 0.29, 0.27, 1.0));
        self.app.world.spawn((
            Transform {
                position: Vec3::new(0.9, PLATFORM_TOP * 0.5, 0.0),
                scale: Vec3::new(2.4, PLATFORM_TOP, 2.4),
                ..Transform::IDENTITY
            },
            platform_mesh,
            mat_platform,
        ));
        self.app.world.spawn((
            Transform::from_xyz(0.9, PLATFORM_TOP * 0.5, 0.0),
            Collider::static_box(Vec3::new(1.2, PLATFORM_TOP * 0.5, 1.2)),
        ));

        // ---- 十肢胶囊人形：静态肢体（动画接管期）+ 胶囊视觉 ----
        // 配色分区（头 / 躯干 / 双臂 / 双腿），摊倒后的姿态一目了然
        let limb_colors: [[f32; 3]; 10] = [
            [0.25, 0.45, 0.80], // 0 躯干（蓝）
            [0.90, 0.76, 0.62], // 1 头（肤色）
            [0.85, 0.30, 0.25], // 2 左上臂（红）
            [0.85, 0.55, 0.20], // 3 右上臂（橙）
            [0.95, 0.45, 0.40], // 4 左前臂（浅红）
            [0.95, 0.70, 0.35], // 5 右前臂（浅橙）
            [0.32, 0.34, 0.38], // 6 左大腿（深灰蓝）
            [0.32, 0.34, 0.38], // 7 右大腿
            [0.22, 0.23, 0.26], // 8 左小腿（更深）
            [0.22, 0.23, 0.26], // 9 右小腿
        ];
        let limbs = spawn_ragdoll_kinematic(&mut self.app.world, self.origin, &self.desc);
        for (index, (&entity, color)) in limbs.iter().zip(&limb_colors).enumerate() {
            let def = self.desc.limbs[index];
            let mat = self
                .app
                .world
                .resource_mut::<MaterialStore>()
                .add(MaterialDef::solid(color[0], color[1], color[2], 1.0));
            self.attach_limb_visual(entity, &def, mat);
            self.limbs.push(entity);
        }
        // 摆动关节参数快照（断言用；生成时已自动接线）
        for link in &self.desc.links {
            if link.swing_limit.is_none() {
                continue;
            }
            let joint = *self
                .app
                .world
                .get::<SwingJoint>(limbs[link.a])
                .expect("Some 链接应已接线摆动关节");
            self.wired.push((link.a, link.b, joint));
        }
        assert_eq!(
            self.wired.len(),
            8,
            "v2 人形应有 8 条摆动链接（肩 ×2 / 肘 ×2 / 胯 ×2 / 膝 ×2）"
        );

        {
            let mut camera = self.app.world.resource_mut::<Camera>();
            camera.eye = Vec3::new(5.6, 2.6, 6.0);
            camera.target = Vec3::new(2.3, 0.4, 0.0);
        }
    }

    /// 肢体的胶囊视觉：圆柱段（半径 = 肢体半径、高 = 2 × 轴半长，本地轴
    /// 沿 +Y = 胶囊轴）挂在肢体实体上（跟随物理旋转），两端帽球作**子实体**
    /// 挂在本地 (0, ±轴半长, 0)（层级传播在 PostUpdate）。球肢直接挂球网格。
    /// 刻意不加渲染端新原语（`MeshData::capsule` 之类）——本轮范围是物理。
    fn attach_limb_visual(&mut self, entity: Entity, def: &RagdollLimbDef, mat: MaterialHandle) {
        let capsule = def.half_length > def.radius;
        let half_axis = (def.half_length - def.radius).max(0.0);
        let mesh = if capsule {
            self.app
                .world
                .resource_mut::<MeshStore>()
                .add(MeshData::cylinder(def.radius, half_axis * 2.0, 20))
        } else {
            self.app
                .world
                .resource_mut::<MeshStore>()
                .add(MeshData::uv_sphere(def.radius, 24, 16))
        };
        self.app.world.insert(entity, mesh);
        self.app.world.insert(entity, mat);
        if !capsule {
            return; // 球肢：一个球就是全部视觉
        }
        let cap_mesh = self
            .app
            .world
            .resource_mut::<MeshStore>()
            .add(MeshData::uv_sphere(def.radius, 20, 12));
        for sign in [1.0f32, -1.0] {
            let cap = self.app.world.spawn((
                Transform::from_xyz(0.0, sign * half_axis, 0.0),
                cap_mesh,
                mat,
            ));
            self.app.world.add_child(entity, cap);
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

    fn position(&self, index: usize) -> Vec3 {
        self.app
            .world
            .get::<Transform>(self.limbs[index])
            .expect("肢体应有 Transform")
            .position
    }

    fn rotation(&self, index: usize) -> Quat {
        self.app
            .world
            .get::<Transform>(self.limbs[index])
            .expect("肢体应有 Transform")
            .rotation
    }

    fn velocity(&self, index: usize) -> Vec3 {
        self.app
            .world
            .get::<RigidBody>(self.limbs[index])
            .expect("肢体应有 RigidBody")
            .velocity
    }

    /// 肢体最低点（胶囊 = 中心 − |轴 y 分量|·轴半长 − 半径；球 = 中心 − 半径）。
    fn lowest_point(&self, index: usize) -> f32 {
        let def = &self.desc.limbs[index];
        let position = self.position(index);
        if def.half_length > def.radius {
            let axis_y = (self.rotation(index) * Vec3::Y).y.abs();
            position.y - axis_y * (def.half_length - def.radius) - def.radius
        } else {
            position.y - def.radius
        }
    }

    /// 全部肢体位置的质心。
    fn com(&self) -> Vec3 {
        let sum: Vec3 = (0..self.limbs.len())
            .map(|index| self.position(index))
            .sum();
        sum / self.limbs.len() as f32
    }

    /// 关节断言：每条摆动链接的锚距保持 rest、摆角不超限位（锥轴随父端
    /// 旋转），并把最大偏差累计出来供终态报告。
    fn check_joints(&self, max_deviation: &mut f32, max_excess: &mut f32) {
        for &(a, b, ref joint) in &self.wired {
            let anchor_a = self.position(a) + self.rotation(a) * joint.anchor_a;
            let anchor_b = self.position(b) + self.rotation(b) * joint.anchor_b;
            let delta = anchor_b - anchor_a;
            let rest = joint.rest_offset.length();
            let deviation = (delta.length() - rest).abs();
            assert!(
                deviation <= ANCHOR_TOLERANCE,
                "链接 ({a}, {b}) 锚距 {} 偏离 rest {rest} 达 {deviation} > {ANCHOR_TOLERANCE}（散架）",
                delta.length()
            );
            *max_deviation = (*max_deviation).max(deviation);

            let axis = self.rotation(b) * joint.rest_offset.normalize_or_zero();
            let angle = (delta.normalize().dot(axis)).clamp(-1.0, 1.0).acos();
            let excess = angle - joint.swing_limit_radians;
            assert!(
                excess <= ANGLE_TOLERANCE,
                "关节 ({a}, {b}) 摆角 {angle} rad 超限位 {} rad 达 {excess}（反折）",
                joint.swing_limit_radians
            );
            *max_excess = (*max_excess).max(excess);
        }
    }
}

fn main() {
    env_logger::init();
    let Some(mut renderer) = OffscreenRenderer::new(1280, 720) else {
        eprintln!("无 GPU");
        std::process::exit(1);
    };

    let mut harness = Harness::new();

    // ---- 动画接管期：12 步静态定格（肢体应 bit 级保持生成姿势）----
    let spawn_pose: Vec<Vec3> = (0..harness.limbs.len())
        .map(|index| harness.position(index))
        .collect();
    harness.frames(12);
    for (index, &want) in spawn_pose.iter().enumerate() {
        assert_eq!(
            harness.position(index),
            want,
            "动画接管期肢体 {index} 应 bit 级保持生成姿势（静态体不积分）"
        );
    }

    // ---- 死亡混合第二阶段：踢下台阶 + 前扑翻倒 ----
    release_ragdoll(
        &harness.app.world,
        &harness.limbs,
        Vec3::new(0.4, 0.0, 0.0),
        Some((Vec3::X, 3.0)),
    );
    let spin = harness
        .app
        .world
        .get::<RigidBody>(harness.limbs[0])
        .expect("肢体应有 RigidBody")
        .angular_velocity;
    println!(
        "注入角速度 ω = {spin:?} rad/s（绕链根肩线 ⇒ 前扑 / 后仰，不侧倒；\
         渐近总转角 = |ω| / 角阻尼 ≈ {:.2} rad ≈ {:.0}°）",
        spin.length() / ANGULAR_DAMPING,
        spin.length() / ANGULAR_DAMPING * 180.0 / std::f32::consts::PI
    );

    // ---- 腾空帧 ----
    let mut max_angle_excess = 0.0f32;
    let mut max_anchor_deviation = 0.0f32;
    harness.frames(AIRBORNE_STEPS);
    harness.check_joints(&mut max_anchor_deviation, &mut max_angle_excess);
    println!(
        "腾空帧（t ≈ {:.2} s）：质心 {:?}、躯干轴 {:?}、躯干 ω {:?}",
        AIRBORNE_STEPS as f32 / 60.0,
        harness.com(),
        harness.rotation(0) * Vec3::Y,
        harness
            .app
            .world
            .get::<RigidBody>(harness.limbs[0])
            .expect("肢体应有 RigidBody")
            .angular_velocity
    );
    renderer.render(&harness.app.world);
    let left = renderer.read_pixels();

    // ---- 摊倒帧（逐步断言关节 / 穿地 / 速度有界）----
    for _ in 0..SETTLE_STEPS {
        harness.frame();
        harness.check_joints(&mut max_anchor_deviation, &mut max_angle_excess);
        for index in 0..harness.limbs.len() {
            let lowest = harness.lowest_point(index);
            assert!(
                lowest >= -GROUND_TOLERANCE,
                "肢体 {index} 不应穿地（最低点 {lowest}，容差 {GROUND_TOLERANCE}）"
            );
            let speed = harness.velocity(index).length();
            assert!(speed < 25.0, "肢体 {index} 速度 {speed} 应有界（无爆炸）");
        }
    }

    // ---- headless 终态断言（与离屏渲染同一世界状态）----
    assert!(
        max_anchor_deviation <= ANCHOR_TOLERANCE,
        "全程关节锚距偏差 {max_anchor_deviation} 应 ≤ {ANCHOR_TOLERANCE}（链未散架）"
    );
    assert!(
        max_angle_excess <= ANGLE_TOLERANCE,
        "全程摆角超限 {max_angle_excess} rad 应 ≤ {ANGLE_TOLERANCE}（关节未反折）"
    );

    let final_com = harness.com();
    assert!(
        final_com.y < PLATFORM_TOP * 0.5,
        "终态质心 y = {} 应显著塌落到地面附近（台阶高 {PLATFORM_TOP}）",
        final_com.y
    );
    let total_speed: f32 = (0..harness.limbs.len())
        .map(|index| harness.velocity(index).length())
        .sum();
    assert!(
        total_speed < SPEED_LIMIT,
        "{SETTLE_STEPS} 步后应趋于静止：总速度 {total_speed} ≥ {SPEED_LIMIT}"
    );

    // 四肢不再竖直悬空：多数胶囊肢的轴接近水平（摊倒而非插在地上）
    let non_upright = (0..harness.limbs.len())
        .filter(|&index| {
            let def = &harness.desc.limbs[index];
            def.half_length > def.radius
        })
        .filter(|&index| {
            (harness.rotation(index) * Vec3::Y).y.abs() < std::f32::consts::FRAC_1_SQRT_2
        })
        .count();
    assert!(
        non_upright >= NON_UPRIGHT_LIMBS,
        "摊倒后应有 ≥ {NON_UPRIGHT_LIMBS} 条胶囊肢的轴接近水平（不竖直悬空），实际 {non_upright}"
    );

    // 躯干躺平：轴接近水平（前扑翻倒的落点）
    let torso_axis = harness.rotation(0) * Vec3::Y;
    assert!(
        torso_axis.y.abs() < 0.35,
        "终态躯干轴应接近水平（躺平），实际 {torso_axis:?}"
    );

    println!(
        "终态断言通过：锚距偏差 ≤ {max_anchor_deviation:.2e}、摆角超限 ≤ {max_angle_excess:.2e} rad、\
         质心 y = {:.3}（塌地）、总速度 {total_speed:.3} < {SPEED_LIMIT}、水平肢体 {non_upright}/9",
        final_com.y
    );

    renderer.render(&harness.app.world);
    let right = renderer.read_pixels();

    // ---- 左右横拼 → docs/ragdoll_v2.png（image 直接拼接，不依赖 ffmpeg）----
    let (w, h) = renderer.size();
    let mut canvas = image::RgbaImage::new(w * 2, h);
    let left = image::RgbaImage::from_raw(w, h, left).expect("左图像素缓冲尺寸匹配");
    let right = image::RgbaImage::from_raw(w, h, right).expect("右图像素缓冲尺寸匹配");
    canvas.copy_from(&left, 0, 0).expect("左图拼入画布");
    canvas.copy_from(&right, w, 0).expect("右图拼入画布");
    canvas
        .save("docs/ragdoll_v2.png")
        .expect("写 docs/ragdoll_v2.png（从仓库根目录运行）");
    println!("已输出 docs/ragdoll_v2.png（{}x{}）", w * 2, h);
}
