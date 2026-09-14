//! [`ExpPlugin`]：实验项目的全部内容——场景装配（地面 / 方向光 / 相机）
//! + 外部 GLB 角色实例化 + 动作切换接线 + 轨道相机与角色移动控制。
//!
//! 与完整游戏（iris-assault 等）刻意保持最小：无逻辑状态机、无物理、
//! 无音频内容、无 UI——只保留回答「渲染 / 动画 / 切换 / 控制」四问所需
//! 的最低装配。`build_schedule` 独立暴露供 headless 测试与离屏快照复用
//! （动画推进由 `AnimationPlugin` 负责，headless 路径需自行注册——与
//! 窗口路径 `DefaultPlugins` 同一编排）。
//!
//! # 控制（frictions ⑥：自由视角 / 缩放 / 角色移动）
//!
//! 纯游戏层装配（引擎无改动）：[`OrbitCamera`] 资源记录轨道状态
//! （yaw / pitch / dist / focus_h），右键拖拽环绕 + 滚轮缩放 + Q/E 调
//! 注视点高度；WASD 相对相机移动 + 朝向平滑转身。两条硬语义：
//!
//! - **无输入帧恒等**：鼠标与 WASD 全静默的帧不写相机与角色
//!   `Transform`——headless / 离屏路径（无输入注入）与窗口静态帧的
//!   逐位行为与 R66b 之前一致，外部机位覆盖（如快照 example 的
//!   `EXP_CAM_HEAD` 诊断特写）不会被运行期系统改写。
//! - **动画播放状态随移动**：步行剪辑仅在有移动输入时播放（停下定格
//!   当前帧），跳跃单次播放不受移动影响（原地跳）；播完回落步行后同样
//!   按移动状态决定播放或定格。因此**依赖「无输入推进动画」的旧测试台
//!   需显式注入移动键**（headless 测试与快照 example 已同轮更新）。

use iris_core::prelude::{Entity, Stage};
use iris_engine::prelude::{
    Access, ActiveCamera, AnimationClip, AnimationPlayer, AnimationPlugin, App, Camera,
    DirectionalLight, HierarchyPlugin, Input, KeyCode, MaterialDef, MaterialStore, MeshData,
    MeshStore, MipSettings, MouseButton, Name, OffscreenRenderer, Plugin, Quat, Resource, Schedule,
    TextureStore, Time, Transform, Vec3, Visibility, World, asset_path, spawn_gltf_instance,
};

/// 角色 GLB 路径（`asset_path` 解析：CWD 相对优先，workspace 根相对回退）。
/// 本体 32MB 版权模型不入库（`assets/exp_weiqing.glb` 已在 .gitignore），
/// 缺席时插件降级为无角色模式（场景照常，诚实告警）。
pub const MODEL_PATH: &str = "assets/exp_weiqing.glb";
/// 步行循环剪辑名（GLB 剪辑名即契约）。
pub const WALK_CLIP: &str = "Mixamo_Walk";
/// 跳跃单次剪辑名（播完自动回落步行）。
pub const JUMP_CLIP: &str = "Mixamo_Jump";

/// 轨道相机注视点**初始**高度（世界坐标 y 偏移；角色约 4 m 高，取胸腹
/// 位）。运行期由 Q/E 连续调节（[`OrbitCamera::focus_h`]，区间
/// [FOCUS_H_MIN, FOCUS_H_MAX]）。
pub const TARGET_HEIGHT: f32 = 1.5;
/// 注视点高度下限（m）：贴地（脚部特写）。
pub const FOCUS_H_MIN: f32 = 0.0;
/// 注视点高度上限（m）：角色约 4 m 高，头顶之上留余量。
pub const FOCUS_H_MAX: f32 = 4.5;
/// 注视点高度调节速率（m/s，Q/E 按住线性升降）。
pub const FOCUS_H_RATE: f32 = 2.5;
/// 拖拽环绕灵敏度（rad / 像素；≈0.2°/px，800 px 拖满 ≈160°）。
pub const ORBIT_SENS: f32 = 0.0035;
/// 滚轮缩放系数（`dist ×= exp(−wheel·k)`；窗口路径行模式一格 = ±40，
/// 每格缩放 ≈12.7%）。
pub const WHEEL_SENS: f32 = 0.003;
/// 俯角下限（rad）：略低于水平，可仰拍；再低会钻地。
pub const PITCH_MIN: f32 = -5.0_f32.to_radians();
/// 俯角上限（rad）：接近顶视但不过顶。
pub const PITCH_MAX: f32 = 85.0_f32.to_radians();
/// 轨道距离下限（m）：贴近面部特写。下限取相机近裁剪面（`z_near=0.1`）
/// 之上留裕量——再近角色表面会被近裁剪面切穿。
pub const DIST_MIN: f32 = 0.3;
/// 轨道距离上限（m）：拉远看全景（地面 24 m 尺度之上再留余量）。
pub const DIST_MAX: f32 = 60.0;
/// 角色移动速度（m/s）。
pub const MOVE_SPEED: f32 = 3.0;
/// 朝向转身速率（1/s；帧率无关指数趋近，见 [`exp_approach`]）。
pub const TURN_RATE: f32 = 12.0;
/// 角色活动范围半宽（m）：地面 24×24，留 1 m 边防止走出地面悬空。
pub const GROUND_HALF: f32 = 11.0;

/// 轨道相机状态（游戏层资源）：yaw = 环绕方位角（0 = 相机在 +Z 侧）、
/// pitch = 俯角（0 = 水平，正值 = 相机升高俯视）、dist = 轨道距离、
/// focus_h = 注视点在角色上方的高度（Q/E 调节）。位姿几何见
/// [`orbit_pose`]。资源由插件 Startup 兜底插入；缺席时轨道系统空转
/// （相机静止——与引擎「资源缺席 = 关」语义同款）。
#[derive(Clone, Copy, Debug, Resource)]
pub struct OrbitCamera {
    /// 环绕方位角（rad）。
    pub yaw: f32,
    /// 俯角（rad）。
    pub pitch: f32,
    /// 轨道距离（m）。
    pub dist: f32,
    /// 注视点高度（m，角色本地 y 偏移；Q/E 调节，区间
    /// [FOCUS_H_MIN, FOCUS_H_MAX]）。
    pub focus_h: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        // 初始机位：正面平视稍俯（相机在 +Z 侧，GLB 角色面向 +Z——
        // 默认看到正脸），与 R66b 之前的观感同构（同俯角、同侧）。
        Self {
            yaw: 0.0,
            pitch: 9.0_f32.to_radians(),
            dist: 7.5,
            focus_h: TARGET_HEIGHT,
        }
    }
}

/// 角色控制跨帧状态：当前朝向角（rad，[`facing_yaw`] 口径）。
/// 单独成资源而非写回 `ExpEntities`（那是实体表不是状态袋）；旋转
/// 始终写作 `from_rotation_y(yaw)`，无其他旋转分量，避免从四元数
/// 反提取的万向锁与浮点往返误差。
#[derive(Clone, Copy, Debug, Resource)]
pub struct ExpCharacter {
    /// 当前朝向角（rad）。
    pub yaw: f32,
}

impl Default for ExpCharacter {
    fn default() -> Self {
        // GLB 角色面向本地 +Z（EXP_CAM_HEAD 诊断注释与旧快照正脸为证；
        // build_scene 旧注释「面向 -Z」系笔误陈留），rotation 恒等时正脸
        // 朝 +Z（初始相机侧）。
        Self { yaw: 0.0 }
    }
}

/// 场景实体表：相机 + 角色模型根（加载失败为 None）+ 两个剪辑的索引。
#[derive(Clone, Copy, Resource)]
pub struct ExpEntities {
    /// 轨道相机实体（`update_orbit_camera` 的写目标）。
    pub camera: Entity,
    /// 角色模型根实体（GLB 收编后的顶层；`None` = 资产缺席降级）。
    pub model: Option<Entity>,
    /// 步行剪辑下标。
    pub walk: usize,
    /// 跳跃剪辑下标。
    pub jump: usize,
}

/// 实验项目插件：Startup 装配场景与角色，Update 接线动作切换、角色
/// 移动与轨道相机。
pub struct ExpPlugin;

impl Default for ExpPlugin {
    fn default() -> Self {
        Self
    }
}

impl ExpPlugin {
    /// 与 `DefaultPlugins` 的编排方式一致（Startup 一次、Update 每帧）；
    /// 独立暴露供 headless 测试与离屏快照复用。**动画与层级不在此注册**：
    /// 调用方按窗口路径的编排补上 `AnimationPlugin`（Update）与
    /// `HierarchyPlugin`（PostUpdate）。
    ///
    /// Update 系统注册序即执行序（同组件 `write` 冲突由调度器串行）：
    /// `update_exp_control`（角色移动 + 动画）在前、`update_orbit_camera`
    /// 在后——相机同帧读到角色新位置，移动时注视点零滞后跟随。
    pub fn build_schedule(&self, schedule: &mut Schedule) {
        schedule
            .add_system(Stage::Startup, setup_resources)
            .add_system(Stage::Startup, build_scene)
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<Input>()
                    .read::<Time>()
                    .read::<ExpEntities>()
                    .read::<OrbitCamera>()
                    .write::<ExpCharacter>()
                    .write::<Transform>()
                    .write::<AnimationPlayer>(),
                update_exp_control,
            )
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<Input>()
                    .read::<ExpEntities>()
                    .write::<OrbitCamera>()
                    .write::<Transform>(),
                update_orbit_camera,
            );
    }
}

impl Plugin for ExpPlugin {
    fn build(&self, app: &mut App) {
        self.build_schedule(&mut app.schedule);
        // 窗口路径把动画 / 层级一并补上（headless / 离屏路径由测试台按
        // 同一编排自行注册）
        AnimationPlugin.build(app);
        HierarchyPlugin.build(app);
    }
}

/// 按名查剪辑下标（GLB 剪辑名即契约；缺席告警）。
fn clip_index(clips: &[AnimationClip], name: &str) -> Option<usize> {
    let found = clips.iter().position(|clip| clip.name == name);
    if found.is_none() {
        log::warn!(
            "ExpPlugin：GLB 缺少剪辑 {name}（现有 {:?}）",
            clips.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
        );
    }
    found
}

/// Startup 1：资源兜底插入（ResourcePlugin 语义——窗口程序里
/// DefaultPlugins 已就位，不得覆盖；headless / 离屏路径由这里补齐）。
fn setup_resources(world: &mut World) {
    if !world.contains_resource::<MeshStore>() {
        world.insert_resource(MeshStore::new());
    }
    if !world.contains_resource::<MaterialStore>() {
        world.insert_resource(MaterialStore::new());
    }
    if !world.contains_resource::<TextureStore>() {
        world.insert_resource(TextureStore::new());
    }
    // mip 链 + 各向异性过滤（R66）：外部 GLB 的发片（黑底透明 + 彩色
    // 发丝）远距缩小采样必须走 mip，否则高频 alpha 逐像素翻噪点
    //（frictions ①）。headless 路径无渲染器，资源在场无害。
    if !world.contains_resource::<MipSettings>() {
        world.insert_resource(MipSettings { anisotropy: 8 });
    }
    if !world.contains_resource::<Camera>() {
        world.insert_resource(Camera::default());
    }
    if !world.contains_resource::<DirectionalLight>() {
        world.insert_resource(DirectionalLight::default());
    }
    if !world.contains_resource::<OrbitCamera>() {
        world.insert_resource(OrbitCamera::default());
    }
    if !world.contains_resource::<ExpCharacter>() {
        world.insert_resource(ExpCharacter::default());
    }
}

/// Startup 2：场景（地面 / 方向光 / 相机）+ 角色 GLB 装配。
///
/// 资产缺席不 panic：降级为无角色模式（场景照常 + 诚实告警）——本项目
/// 的资产是 32MB 版权模型，不在开源仓库内。相机位姿由 [`orbit_pose`]
/// 构造（初始参数 [`OrbitCamera::default`]），保证窗口首帧与轨道系统的
/// 状态口径一致（拖拽不跳变）。
fn build_scene(world: &mut World) {
    // ---- 地面：大扁立方体（哑光灰，衬托角色）----
    let ground_mesh = world.resource_mut::<MeshStore>().add(MeshData::cube());
    let ground_mat = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.32, 0.34, 0.36, 1.0));
    world.spawn((
        Name("exp/ground".to_string()),
        Transform {
            position: Vec3::new(0.0, -0.05, 0.0),
            scale: Vec3::new(24.0, 0.1, 24.0),
            ..Transform::IDENTITY
        },
        ground_mesh,
        ground_mat,
        Visibility::VISIBLE,
    ));

    // ---- 方向光：右上前方斜射，开阴影 ----
    // R66 调参（frictions ②）：intensity 2.6 + ambient 0.22 观感整体偏白
    //（白色服装大面积过曝、贴图细节被冲掉），按离屏快照逐轮目检调低至
    // 白色布料恢复层次。离屏与窗口共用 ACES 链，观感可直接对照。
    if let Some(mut light) = world.try_resource_mut::<DirectionalLight>() {
        light.direction = Vec3::new(-0.45, -1.0, -0.35).normalize_or_zero();
        light.intensity = 1.4;
        light.ambient = Vec3::new(0.11, 0.12, 0.14);
        light.shadows = true;
    }

    // ---- 相机：轨道位姿构造（注视原点上方 orbit.focus_h）----
    let orbit = *world
        .try_resource::<OrbitCamera>()
        .expect("OrbitCamera 已由 setup_resources 插入");
    let (eye, rot) = orbit_pose(
        orbit.yaw,
        orbit.pitch,
        orbit.dist,
        Vec3::new(0.0, orbit.focus_h, 0.0),
    );
    let camera = world.spawn((
        Name("exp/camera".to_string()),
        Transform {
            position: eye,
            rotation: rot,
            scale: Vec3::ONE,
        },
        ActiveCamera::default(),
    ));
    // ---- 角色 GLB：装配 + 步行循环自动开播 ----
    let path = asset_path(MODEL_PATH);
    let entities = match spawn_gltf_instance(world, &path) {
        Ok(instance) => {
            let clips = instance.clips.clone();
            let walk = clip_index(&clips, WALK_CLIP);
            let jump = clip_index(&clips, JUMP_CLIP);
            let model = instance.roots[0];
            world.insert(model, Name("exp/character".to_string()));
            world.insert(model, Visibility::VISIBLE);
            let mut player = instance.player();
            // 步行循环自动开播（`player()` 默认播第一个剪辑——按名改指步行；
            // 首帧 Update 无移动输入即定格——播放状态随移动，见模块文档）
            if let Some(walk) = walk {
                player.active = walk;
                player.looping = true;
                player.playing = true;
            }
            player.capture_rest(world);
            world.insert(model, player);
            log::info!(
                "ExpPlugin：角色已装配（{} 节点 / {} 剪辑），\
                 控制：WASD 移动 / 右键拖拽视角 / 滚轮缩放 / Q·E 注视点高度 / \
                 空格 跳跃 / 1 步行",
                instance.node_entities.len(),
                clips.len()
            );
            ExpEntities {
                camera,
                model: Some(model),
                walk: walk.unwrap_or(0),
                jump: jump.unwrap_or(0),
            }
        }
        Err(err) => {
            log::warn!(
                "ExpPlugin：角色 GLB {} 加载失败（{err}）——无角色模式\
                 （把 GLB 放到该路径后重跑即可）",
                path.display()
            );
            ExpEntities {
                camera,
                model: None,
                walk: 0,
                jump: 0,
            }
        }
    };
    world.insert_resource(entities);
}

/// Update：动作切换 + WASD 角色移动 + 朝向转身。`空格` 跳跃单次
/// （`looping = false`，播完自动回落步行）；`1` 步行循环；步行动画的
/// 播放状态随移动输入（停下定格当前帧）。无移动输入的帧不写角色
/// `Transform`（恒等）；角色缺席（资产降级）时整帧空转。
fn update_exp_control(world: &World) {
    let Some(ents) = world.try_resource::<ExpEntities>() else {
        return;
    };
    let Some(model) = ents.model else {
        return;
    };
    let Some(input) = world.try_resource::<Input>() else {
        return;
    };
    let dt = world
        .try_resource::<Time>()
        .map(|t| t.delta_secs())
        .unwrap_or(0.0);
    // 移动方向以相机方位为参照（读 OrbitCamera 状态而非相机 Transform：
    // 本帧拖拽后的最新值，且与渲染无关、headless 可注入断言）
    let cam_yaw = world
        .try_resource::<OrbitCamera>()
        .map(|o| o.yaw)
        .unwrap_or(0.0);

    // ---- WASD 意图方向（相机系：W = 屏幕里侧，D = 屏幕右侧）----
    let forward = Vec3::new(-cam_yaw.sin(), 0.0, -cam_yaw.cos());
    let right = Vec3::new(cam_yaw.cos(), 0.0, -cam_yaw.sin());
    let mut dir = Vec3::ZERO;
    if input.key(KeyCode::KeyW) {
        dir += forward;
    }
    if input.key(KeyCode::KeyS) {
        dir -= forward;
    }
    if input.key(KeyCode::KeyD) {
        dir += right;
    }
    if input.key(KeyCode::KeyA) {
        dir -= right;
    }
    let moving = dir != Vec3::ZERO;
    let dir = if moving { dir.normalize_or_zero() } else { dir };

    let space_pressed = input.key_pressed(KeyCode::Space);
    let one_pressed = input.key_pressed(KeyCode::Digit1);

    let Some(mut player) = world.get_mut::<AnimationPlayer>(model) else {
        return;
    };
    // 跳跃播完（非循环到尾）自动回落步行
    if player.active == ents.jump
        && player.active != ents.walk
        && (!player.playing || player.time >= player.clips[player.active].duration)
    {
        player.active = ents.walk;
        player.time = 0.0;
        player.looping = true;
        player.playing = true;
    }
    if space_pressed && player.active != ents.jump {
        player.active = ents.jump;
        player.time = 0.0;
        player.looping = false;
        player.playing = true;
    } else if one_pressed && player.active != ents.walk {
        player.active = ents.walk;
        player.time = 0.0;
        player.looping = true;
        player.playing = moving;
    }
    // 步行播放状态随移动（停下定格当前帧；跳跃播放不受影响——原地跳）
    if player.active == ents.walk {
        player.playing = moving;
    }

    // ---- 位置积分 + 朝向转身（有移动输入才写，无输入恒等）----
    if moving {
        let Some(mut t) = world.get_mut::<Transform>(model) else {
            return;
        };
        let mut pos = t.position + dir * (MOVE_SPEED * dt);
        pos.x = pos.x.clamp(-GROUND_HALF, GROUND_HALF);
        pos.z = pos.z.clamp(-GROUND_HALF, GROUND_HALF);
        t.position = pos;
        if let Some(mut ch) = world.try_resource_mut::<ExpCharacter>() {
            ch.yaw = wrap_angle(exp_approach(ch.yaw, facing_yaw(dir), TURN_RATE, dt));
        }
        let yaw = world
            .try_resource::<ExpCharacter>()
            .map(|c| c.yaw)
            .unwrap_or(0.0);
        t.rotation = Quat::from_rotation_y(yaw);
    }
}

/// WASD 任一处于按住状态（移动意图与相机「角色移动帧跟随」判定共用
/// 的键位表——两处必须同步增删）。
fn wasd_held(input: &Input) -> bool {
    input.key(KeyCode::KeyW)
        || input.key(KeyCode::KeyS)
        || input.key(KeyCode::KeyA)
        || input.key(KeyCode::KeyD)
}

/// Update：右键拖拽轨道环绕 + 滚轮缩放 + Q/E 调注视点高度 + 注视点跟随
/// 角色。鼠标、滚轮、Q/E 与 WASD 全静默的帧直接返回（不写相机
/// `Transform`、不写资源——恒等帧保护外部机位覆盖与 headless 逐位稳定）；
/// WASD 按住帧照写（注视点跟随移动中的角色，钳制边界上写的是逐位恒等
/// 值，无副作用）。相机实体缺席（理论上不发生，实体表自有即建）时仅
/// 状态更新不写位姿。
fn update_orbit_camera(world: &World) {
    let Some(ents) = world.try_resource::<ExpEntities>() else {
        return;
    };
    let Some(input) = world.try_resource::<Input>() else {
        return;
    };
    let dragging = input.mouse_button(MouseButton::Right);
    let (dx, dy) = input.mouse_delta();
    let wheel = input.wheel_delta();
    let focus_key_held = input.key(KeyCode::KeyQ) || input.key(KeyCode::KeyE);
    if !dragging && dx == 0.0 && dy == 0.0 && wheel == 0.0 && !focus_key_held && !wasd_held(&input)
    {
        return;
    }
    // 轨道状态读取圈进作用域（try_resource 守卫活到块尾——在守卫持有
    // 期间 try_resource_mut 同一资源会触发 already borrowed panic）
    let (mut yaw, mut pitch, mut dist, mut focus_h) = {
        let Some(orbit) = world.try_resource::<OrbitCamera>() else {
            return;
        };
        (orbit.yaw, orbit.pitch, orbit.dist, orbit.focus_h)
    };
    if dragging {
        // 右移 = 相机向右环绕（yaw 增）；上移 = 相机升高俯视（pitch 增）
        yaw += dx as f32 * ORBIT_SENS;
        pitch = (pitch - dy as f32 * ORBIT_SENS).clamp(PITCH_MIN, PITCH_MAX);
    }
    if wheel != 0.0 {
        // 上滚（wheel > 0）= 拉近
        dist *= (-wheel * WHEEL_SENS).exp();
    }
    dist = dist.clamp(DIST_MIN, DIST_MAX);
    if let Some(dt) = world.try_resource::<Time>().map(|t| t.delta_secs()) {
        // Q 降 / E 升（注视点朝脚部 / 头顶移动；相机同向跟随）
        if input.key(KeyCode::KeyQ) {
            focus_h -= FOCUS_H_RATE * dt;
        }
        if input.key(KeyCode::KeyE) {
            focus_h += FOCUS_H_RATE * dt;
        }
    }
    focus_h = focus_h.clamp(FOCUS_H_MIN, FOCUS_H_MAX);

    // ---- 注视点：角色当前位置 + focus_h 高度（同帧新值——注册序在
    // 移动系统之后）----
    let focus = match ents.model {
        Some(m) => world
            .get::<Transform>(m)
            .map(|t| t.position)
            .unwrap_or(Vec3::ZERO),
        None => Vec3::ZERO,
    } + Vec3::new(0.0, focus_h, 0.0);
    let (eye, rot) = orbit_pose(yaw, pitch, dist, focus);

    if let Some(mut t) = world.get_mut::<Transform>(ents.camera) {
        t.position = eye;
        t.rotation = rot;
    }
    if let Some(mut o) = world.try_resource_mut::<OrbitCamera>() {
        o.yaw = yaw;
        o.pitch = pitch;
        o.dist = dist;
        o.focus_h = focus_h;
    }
}

/// 轨道位姿几何（纯函数）：绕 `target` 的球坐标 → 相机（世界位）。
///
/// `offset = dist·(cos(pitch)·sin(yaw), sin(pitch), cos(pitch)·cos(yaw))`，
/// 旋转 = `from_rotation_y(yaw) × from_rotation_x(−pitch)`（先俯仰后绕
/// 世界 Y 偏航）——相机前向 `R × (0,0,−1)` 与「从 eye 指向 target」方向
/// 精确一致（解析可证，单测守护）。pitch 为俯角口径（正值 = 相机高于
/// 注视点）。
pub fn orbit_pose(yaw: f32, pitch: f32, dist: f32, target: Vec3) -> (Vec3, Quat) {
    let (sp, cp) = pitch.sin_cos();
    let (sy, cy) = yaw.sin_cos();
    let offset = Vec3::new(dist * cp * sy, dist * sp, dist * cp * cy);
    (
        target + offset,
        Quat::from_rotation_y(yaw) * Quat::from_rotation_x(-pitch),
    )
}

/// 移动方向 → 角色朝向角（纯函数）：模型前向为本地 **+Z**（GLB 导出口径，
/// 离屏快照正脸与 `EXP_CAM_HEAD` 诊断机位双侧证实——initial rotation
/// 恒等时 +Z 侧相机看到正脸），旋转 `from_rotation_y(yaw)` 须把 `(0,0,1)`
/// 转到 `dir`，解析解 `yaw = atan2(dir.x, dir.z)`。值域 [−π, π]，面向
/// +Z 恰为 0（与初始朝向一致，S 向移动不触发转身）。
pub fn facing_yaw(dir: Vec3) -> f32 {
    dir.x.atan2(dir.z)
}

/// 最短角差（纯函数）：`to − from` 归一到 (−π, π]，供朝向环绕转身
/// （跨 ±π 不绕远路）。
pub fn shortest_angle(from: f32, to: f32) -> f32 {
    let d = (to - from + core::f32::consts::PI) % core::f32::consts::TAU;
    let d = if d < 0.0 {
        d + core::f32::consts::TAU
    } else {
        d
    };
    d - core::f32::consts::PI
}

/// 角度规范到 [−π, π)（纯函数）：转身状态的规范表示——连续同向转身
/// 会让 `exp_approach` 的状态角沿等价旋转漂出 ±π（如 −π → −2π），旋转
/// 语义不变但状态值漂移；每步规范一次即锚定。边界 ±π 的去向由
/// `.round()` 的远离半取整决定（±π 互转，旋转等价无碍）。
pub fn wrap_angle(a: f32) -> f32 {
    a - core::f32::consts::TAU * (a / core::f32::consts::TAU).round()
}

/// 帧率无关指数趋近（纯函数）：每帧朝 `target` 走「当前角差 ×
/// `1 − exp(−rate·dt)`」。残差比例逐帧连乘 = `exp(−rate·Σdt)`（指数
/// 衰减解析解），同一段真实时间任意切分收敛轨迹一致（浮点舍入内），
/// `rate ≤ 0` 视为直接贴合。
pub fn exp_approach(cur: f32, target: f32, rate: f32, dt: f32) -> f32 {
    if rate <= 0.0 {
        return target;
    }
    let k = 1.0 - (-rate * dt).exp();
    cur + shortest_angle(cur, target) * k
}

/// 离屏渲染器便捷构造（快照 example / 手动验收用；无 GPU 返回 None）。
pub fn offscreen(width: u32, height: u32) -> Option<OffscreenRenderer> {
    OffscreenRenderer::new(width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-5;

    fn assert_close(a: Vec3, b: Vec3, what: &str) {
        assert!(
            a.abs_diff_eq(b, EPS),
            "{what}：期望 {b:?}，实际 {a:?}（差 {:?}）",
            a - b
        );
    }

    #[test]
    fn orbit_pose_analytic() {
        // 水平正对（yaw=0, pitch=0）：相机在 target +Z 方向 dist 处，恒等旋转
        let (eye, rot) = orbit_pose(0.0, 0.0, 5.0, Vec3::new(1.0, 2.0, 3.0));
        assert_close(eye, Vec3::new(1.0, 2.0, 8.0), "水平正对 eye");
        assert!(
            rot.abs_diff_eq(Quat::IDENTITY, EPS),
            "水平正对旋转应恒等，实际 {rot:?}"
        );
        // 正上方俯拍（pitch=90°）：eye 在 target 正上方，前向朝下
        let (eye, rot) = orbit_pose(0.0, core::f32::consts::FRAC_PI_2, 4.0, Vec3::ZERO);
        assert_close(eye, Vec3::new(0.0, 4.0, 0.0), "正上方 eye");
        let fwd = rot * Vec3::NEG_Z;
        assert_close(fwd, Vec3::NEG_Y, "正上方相机前向应朝下");
        // 侧位 90°（yaw=90°）：相机绕到 +X 侧
        let (eye, _) = orbit_pose(core::f32::consts::FRAC_PI_2, 0.0, 5.0, Vec3::ZERO);
        assert_close(eye, Vec3::new(5.0, 0.0, 0.0), "侧位 90° eye");
        // 不变量：|eye − target| = dist（任意角度）
        for &(yaw, pitch) in &[(0.3_f32, 0.2_f32), (-2.1, 1.2), (5.0, -0.05)] {
            let (eye, _) = orbit_pose(yaw, pitch, 7.25, Vec3::new(0.5, 1.0, -0.5));
            assert!(
                (eye.distance(Vec3::new(0.5, 1.0, -0.5)) - 7.25).abs() < EPS,
                "轨道距离不变量被破坏（yaw={yaw} pitch={pitch}）"
            );
        }
        // 初始机位（OrbitCamera::default）：与 R66b 旧机位同构观感——
        // 相机在 +Z 侧俯视 9°，注视点在 target 上方
        let o = OrbitCamera::default();
        let (eye, rot) = orbit_pose(o.yaw, o.pitch, o.dist, Vec3::new(0.0, TARGET_HEIGHT, 0.0));
        assert_close(
            eye,
            Vec3::new(
                0.0,
                1.5 + 7.5 * 9f32.to_radians().sin(),
                7.5 * 9f32.to_radians().cos(),
            ),
            "初始机位 eye",
        );
        assert_close(
            rot * Vec3::NEG_Z,
            Vec3::new(0.0, -9f32.to_radians().sin(), -9f32.to_radians().cos()),
            "初始机位前向（俯 9°）",
        );
    }

    #[test]
    fn facing_yaw_cardinal() {
        let pi = core::f32::consts::PI;
        // 模型原生面向 +Z：dir = +Z 无需转身（S 向移动不触发转身）
        assert!(
            facing_yaw(Vec3::Z).abs() < EPS,
            "面向 +Z 应为 yaw=0，实际 {}",
            facing_yaw(Vec3::Z)
        );
        // 面向 -Z：±π 皆合法（同一旋转；atan2 对 (−0.0, 负数) 返回 −π，
        // IEEE 负零语义决定符号，跨平台确定）
        assert!(
            (facing_yaw(Vec3::NEG_Z).abs() - pi).abs() < EPS,
            "面向 -Z 应为 |yaw|=π，实际 {}",
            facing_yaw(Vec3::NEG_Z)
        );
        assert!(
            (facing_yaw(Vec3::X) - core::f32::consts::FRAC_PI_2).abs() < EPS,
            "面向 +X 应为 yaw=+π/2（from_rotation_y(π/2) 把 +Z 转到 +X）"
        );
        assert!(
            (facing_yaw(Vec3::NEG_X) + core::f32::consts::FRAC_PI_2).abs() < EPS,
            "面向 -X 应为 yaw=-π/2"
        );
    }

    #[test]
    fn wrap_angle_canonical() {
        let pi = core::f32::consts::PI;
        // 主值区内恒等
        assert!((wrap_angle(1.234) - 1.234).abs() < EPS);
        // 漂出 ±π 的等价角拉回主值区（连续同向转身的漂移场景）
        assert!((wrap_angle(-2.0 * pi).abs()) < EPS, "-2π 应规范到 0");
        assert!(
            (wrap_angle(3.0 * pi).abs() - pi).abs() < EPS,
            "3π 应规范到 ±π（round 远离半取整实取 −π，等价）"
        );
        assert!(wrap_angle(-5.0).abs() < pi, "-5 rad 应规范进 [−π, π)");
        // 边界 ±π 互转（旋转等价）
        assert!((wrap_angle(-pi).abs() - pi).abs() < EPS);
    }

    #[test]
    fn shortest_angle_wrap() {
        let pi = core::f32::consts::PI;
        // 跨 ±π 取短弧：0 → 350° 应为 -10° 而非 +350°
        assert!(
            (shortest_angle(0.0, 350.0_f32.to_radians()) + 10.0_f32.to_radians()).abs() < EPS,
            "350° 应取 -10° 短弧"
        );
        assert!(
            (shortest_angle(350.0_f32.to_radians(), 0.0) - 10.0_f32.to_radians()).abs() < EPS,
            "反向同样取短弧"
        );
        // 同角差为 0
        assert!(shortest_angle(1.234, 1.234).abs() < EPS);
        // 半圈边界（±π）取值有定义（幅度恒 π）
        assert!((shortest_angle(0.0, -pi).abs() - pi).abs() < EPS);
    }

    #[test]
    fn exp_approach_frame_rate_independent() {
        let pi = core::f32::consts::PI;
        // 同一段真实时间（1/20 s）不同切分（3×1/60 s vs 1×1/20 s vs
        // 10×1/200 s）收敛一致（指数衰减解析解，容差 = 浮点舍入）
        let run = |steps: usize, dt: f32| -> f32 {
            let mut cur = 0.0_f32;
            for _ in 0..steps {
                cur = exp_approach(cur, pi, TURN_RATE, dt);
            }
            cur
        };
        let a = run(3, 1.0 / 60.0);
        let b = run(1, 1.0 / 20.0);
        let c = run(10, 1.0 / 200.0);
        assert!(
            (a - b).abs() < 1e-6 && (a - c).abs() < 1e-6,
            "切分无关性被破坏：{a} vs {b} vs {c}"
        );
        // 单步解析解：残差 = 角差 × exp(−rate·dt)（取非边界目标角 2.0 rad
        // ——边界 ±π 处短弧方向本就双向等长，无符号约定）
        let t0 = 2.0_f32;
        let cur = exp_approach(0.0, t0, TURN_RATE, 1.0 / 60.0);
        let expect = t0 * (-TURN_RATE * 1.0 / 60.0).exp();
        assert!(
            ((t0 - cur) - expect).abs() < 1e-6,
            "单步残差应等于解析解：期望 {expect}，实际残差 {}",
            t0 - cur
        );
        // rate ≤ 0 直接贴合；朝向环绕（0 → -π+ε）走短弧
        assert_eq!(exp_approach(0.3, 2.0, 0.0, 1.0 / 60.0), 2.0);
        let wrapped = exp_approach(0.0, -pi + 0.01, TURN_RATE, 1.0 / 60.0);
        assert!(wrapped < 0.0, "目标在 -π 附近应走负向短弧，实际 {wrapped}");
    }
}
