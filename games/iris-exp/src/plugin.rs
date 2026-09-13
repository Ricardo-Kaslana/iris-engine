//! [`ExpPlugin`]：实验项目的全部内容——场景装配（地面 / 方向光 / 相机）
//! + 外部 GLB 角色实例化 + 动作切换接线。
//!
//! 与完整游戏（iris-assault 等）刻意保持最小：无逻辑状态机、无物理、
//! 无音频内容、无 UI——只保留回答「渲染 / 动画 / 切换」三问所需的最低
//! 装配。`build_schedule` 独立暴露供 headless 测试与离屏快照复用（动画
//! 推进由 `AnimationPlugin` 负责，headless 路径需自行注册——与窗口路径
//! `DefaultPlugins` 同一编排）。

use iris_core::prelude::{Entity, Stage};
use iris_engine::prelude::{
    Access, ActiveCamera, AnimationClip, AnimationPlayer, AnimationPlugin, App, Camera,
    DirectionalLight, HierarchyPlugin, Input, KeyCode, MaterialDef, MaterialStore, MeshData,
    MeshStore, Name, OffscreenRenderer, Plugin, Quat, Resource, Schedule, TextureStore, Transform,
    Vec3, Visibility, World, asset_path, spawn_gltf_instance,
};

/// 角色 GLB 路径（`asset_path` 解析：CWD 相对优先，workspace 根相对回退）。
/// 本体 32MB 版权模型不入库（`assets/exp_weiqing.glb` 已在 .gitignore），
/// 缺席时插件降级为无角色模式（场景照常，诚实告警）。
pub const MODEL_PATH: &str = "assets/exp_weiqing.glb";
/// 步行循环剪辑名（GLB 剪辑名即契约）。
pub const WALK_CLIP: &str = "Mixamo_Walk";
/// 跳跃单次剪辑名（播完自动回落步行）。
pub const JUMP_CLIP: &str = "Mixamo_Jump";

/// 场景实体表：角色模型根（加载失败为 None）+ 两个剪辑的索引。
#[derive(Clone, Copy, Resource)]
pub struct ExpEntities {
    /// 角色模型根实体（GLB 收编后的顶层；`None` = 资产缺席降级）。
    pub model: Option<Entity>,
    /// 步行剪辑下标。
    pub walk: usize,
    /// 跳跃剪辑下标。
    pub jump: usize,
}

/// 实验项目插件：Startup 装配场景与角色，Update 接线动作切换。
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
    pub fn build_schedule(&self, schedule: &mut Schedule) {
        schedule
            .add_system(Stage::Startup, setup_resources)
            .add_system(Stage::Startup, build_scene)
            .add_parallel_system(
                Stage::Update,
                Access::new()
                    .read::<Input>()
                    .read::<ExpEntities>()
                    .write::<AnimationPlayer>(),
                update_exp_control,
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
    if !world.contains_resource::<Camera>() {
        world.insert_resource(Camera::default());
    }
    if !world.contains_resource::<DirectionalLight>() {
        world.insert_resource(DirectionalLight::default());
    }
}

/// Startup 2：场景（地面 / 方向光 / 相机）+ 角色 GLB 装配。
///
/// 资产缺席不 panic：降级为无角色模式（场景照常 + 诚实告警）——本项目
/// 的资产是 32MB 版权模型，不在开源仓库内。
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
    if let Some(mut light) = world.try_resource_mut::<DirectionalLight>() {
        light.direction = Vec3::new(-0.45, -1.0, -0.35).normalize_or_zero();
        light.intensity = 2.6;
        light.ambient = Vec3::new(0.22, 0.24, 0.28);
        light.shadows = true;
    }

    // ---- 相机：正面平视稍俯（角色 ~4 m 高；GLB 导出后面向 -Z，
    // 相机放 -Z 侧并回转 180° 对准角色面部）----
    let camera = world.spawn((
        Name("exp/camera".to_string()),
        Transform {
            position: Vec3::new(0.0, 2.4, 7.2),
            rotation: Quat::from_rotation_x(-9.0_f32.to_radians()),
            scale: Vec3::ONE,
        },
        ActiveCamera::default(),
    ));
    let _ = camera;
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
            // 步行循环自动开播（`player()` 默认播第一个剪辑——按名改指步行）
            if let Some(walk) = walk {
                player.active = walk;
                player.looping = true;
                player.playing = true;
            }
            player.capture_rest(world);
            world.insert(model, player);
            log::info!(
                "ExpPlugin：角色已装配（{} 节点 / {} 剪辑），动作：1 步行 / 空格 跳跃",
                instance.node_entities.len(),
                clips.len()
            );
            ExpEntities {
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
                model: None,
                walk: 0,
                jump: 0,
            }
        }
    };
    world.insert_resource(entities);
}

/// Update：动作切换接线。`空格` 跳跃单次（`looping = false`，播完自动
/// 回落步行）；`1` 步行循环。角色缺席（资产降级）时整帧空转。
fn update_exp_control(world: &World) {
    let Some(ents) = world.try_resource::<ExpEntities>() else {
        return;
    };
    let Some(model) = ents.model else {
        return;
    };
    let space_pressed = world
        .try_resource::<Input>()
        .map(|input| input.key_pressed(KeyCode::Space))
        .unwrap_or(false);
    let one_pressed = world
        .try_resource::<Input>()
        .map(|input| input.key_pressed(KeyCode::Digit1))
        .unwrap_or(false);

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
        player.playing = true;
    }
}

/// 离屏渲染器便捷构造（快照 example / 手动验收用；无 GPU 返回 None）。
pub fn offscreen(width: u32, height: u32) -> Option<OffscreenRenderer> {
    OffscreenRenderer::new(width, height)
}
