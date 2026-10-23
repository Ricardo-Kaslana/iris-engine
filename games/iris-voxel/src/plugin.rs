//! 引擎接线：资源、区块实体、输入、物理、编辑与 HUD。
//!
//! 所有玩法状态都放在资源里；区块网格句柄在启动时一次性创建，编辑后
//! 通过 `MeshStore::update` 原地替换内容，句柄不增长。

use glam::{IVec2, IVec3, Vec3};
use iris_engine::prelude::*;

use crate::atlas;
use crate::collision::{ChunkCollision, StaticBox, can_place};
use crate::logic::block::Block;
use crate::logic::inventory::Inventory;
use crate::logic::player::{
    GRAVITY_SCALE, JUMP_SPEED, PLAYER_HALF_HEIGHT, PLAYER_RADIUS, PlayerState, TERMINAL_SPEED,
    apply_look, forward, wants_sprint, wish_direction,
};
use crate::logic::raycast::pick_block;
use crate::logic::world::{
    CHUNK_COUNT, CHUNK_HEIGHT, CHUNK_SIZE, VoxelWorld, WORLD_CHUNKS, WORLD_SIZE,
};
use crate::mesh::{build_chunk_meshes, wireframe_cube_mesh};

/// 拾取与放置的最大距离（米）。
pub const REACH: f32 = 5.0;
/// 基准垂直视场角（弧度，45°）。
const BASE_FOV: f32 = std::f32::consts::FRAC_PI_4;
/// 奔跑时视场角外扩量（约 8°）：周边视野变快是速度感的主要来源。
const SPRINT_FOV_BOOST: f32 = 8.0 * std::f32::consts::PI / 180.0;
/// 视场角收敛速率（每秒）：指数逼近，约 0.15 秒跟上，不做瞬切。
const FOV_RESPONSE: f32 = 12.0;
/// 快捷栏按顺序放置的方块。
pub const HOTBAR_BLOCKS: [Block; 8] = [
    Block::Grass,
    Block::Dirt,
    Block::Stone,
    Block::Sand,
    Block::Wood,
    Block::Planks,
    Block::Glowstone,
    Block::Leaves,
];

/// 体素游戏插件。
pub struct VoxelPlugin {
    pub seed: u64,
}

impl Default for VoxelPlugin {
    fn default() -> Self {
        Self { seed: 20_261_022 }
    }
}

impl VoxelPlugin {
    pub fn new(seed: u64) -> Self {
        Self { seed }
    }

    /// 注册启动与每帧系统；窗口程序与 headless 测试共用同一条编排。
    pub fn build_schedule(&self, schedule: &mut Schedule) {
        let seed = self.seed;
        schedule.add_system(Stage::Startup, move |world: &mut World| {
            setup_resources(world, seed);
        });
        schedule.add_system(Stage::Startup, build_world_and_assets);
        schedule.add_system(Stage::Update, toggle_capture);
        schedule.add_system(Stage::Update, update_look);
        schedule.add_system(Stage::Update, update_hotbar);
        schedule.add_system(Stage::Update, step_player);
        schedule.add_system(Stage::Update, update_target_and_edits);
        schedule.add_system(Stage::Update, sync_camera);
        schedule.add_system(Stage::Update, update_hud);
    }
}

impl Plugin for VoxelPlugin {
    fn build(&self, app: &mut App) {
        self.build_schedule(&mut app.schedule);
    }
}

impl Resource for VoxelWorld {}
impl Resource for PlayerState {}
impl Resource for Inventory {}

/// 区块实体与网格句柄，下标与 `VoxelWorld::chunk_index` 对齐。
///
/// 每个区块有两套网格：不透明方块与裁切方块（树叶）。裁切实体挂
/// `Visibility`，分区为空时隐藏——多数区块没有树，隐藏掉就不产生空绘制。
#[derive(Resource)]
struct ChunkTable {
    entries: Vec<ChunkEntry>,
}

struct ChunkEntry {
    coord: IVec2,
    opaque: MeshHandle,
    cutout: MeshHandle,
    cutout_entity: Entity,
}

impl ChunkTable {
    /// 该区块的两套网格句柄。下标越界返回 `None`。
    fn meshes(&self, coord: IVec2) -> Option<(MeshHandle, MeshHandle)> {
        VoxelWorld::chunk_index(coord).map(|index| {
            let entry = &self.entries[index];
            (entry.opaque, entry.cutout)
        })
    }
}

/// 区块的两套网格句柄（不透明、裁切），供外部工具与 headless 测试读取。
pub fn chunk_meshes(world: &World, coord: IVec2) -> Option<(MeshHandle, MeshHandle)> {
    world
        .try_resource::<ChunkTable>()
        .and_then(|table| table.meshes(coord))
}

/// 区块裁切实体的句柄。
fn cutout_entity(world: &World, coord: IVec2) -> Option<Entity> {
    let table = world.try_resource::<ChunkTable>()?;
    let index = VoxelWorld::chunk_index(coord)?;
    Some(table.entries[index].cutout_entity)
}

/// 区块裁切实体当前是否可见。空分区会被隐藏，便于测试断言开关生效。
pub fn chunk_cutout_visible(world: &World, coord: IVec2) -> bool {
    let Some(entity) = cutout_entity(world, coord) else {
        return false;
    };
    world
        .get::<Visibility>(entity)
        .map(|visibility| visibility.visible)
        .unwrap_or(false)
}

/// 每区块碰撞壳（独立资源，便于收集时只读）。
#[derive(Resource)]
struct ChunkCollisionRes(ChunkCollision);

#[derive(Resource, Clone, Copy)]
struct SpawnPoint(Vec3);

#[derive(Resource)]
struct Highlight(Entity);

#[derive(Resource)]
struct Hotbar {
    selected: usize,
}

impl Hotbar {
    fn block(&self) -> Block {
        HOTBAR_BLOCKS[self.selected]
    }

    fn select(&mut self, index: usize) {
        self.selected = index % HOTBAR_BLOCKS.len();
    }

    fn cycle(&mut self, delta: i32) {
        self.selected =
            (self.selected as i32 + delta).rem_euclid(HOTBAR_BLOCKS.len() as i32) as usize;
    }
}

#[derive(Resource, Default)]
struct VoxelScratch {
    statics: Vec<StaticBox>,
}

#[derive(Resource)]
struct HudEntities {
    crosshair_h: Entity,
    crosshair_v: Entity,
    info: Entity,
    bag: Entity,
    panel_hotbar: Entity,
    panel_label: Entity,
    slots: Vec<Entity>,
    swatches: Vec<Entity>,
    numbers: Vec<Entity>,
    selection: Entity,
    label: Entity,
}

fn setup_resources(world: &mut World, seed: u64) {
    if !world.contains_resource::<Time>() {
        world.insert_resource(Time::default());
    }
    if !world.contains_resource::<Input>() {
        world.insert_resource(Input::new());
    }
    if !world.contains_resource::<MouseCapture>() {
        world.insert_resource(MouseCapture::new());
    }
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
        world.insert_resource(DirectionalLight {
            direction: Vec3::new(-0.45, -1.05, 1.0),
            color: Vec3::new(1.0, 0.97, 0.9),
            intensity: 1.1,
            ambient: Vec3::new(0.18, 0.2, 0.24),
            ..DirectionalLight::default()
        });
    }
    if !world.contains_resource::<Viewport>() {
        world.insert_resource(Viewport::default());
    }
    // 方块纹理开启 mip 链与各向异性过滤。掠射角下的方块面是典型的
    // 各向异性足迹：只靠三线性会在远处糊出摩尔纹，移动时整片闪烁。
    // WebGPU 规定 anisotropy > 1 时放大、缩小与 mip 过滤必须全线性，
    // 所以这里放弃最近邻放大——像素画的硬边让位给稳定画面。
    if !world.contains_resource::<MipSettings>() {
        world.insert_resource(MipSettings::default());
    }
    if !world.contains_resource::<TextureFilterSettings>() {
        world.insert_resource(TextureFilterSettings {
            mag: TextureFilter::Linear,
            min: TextureFilter::Linear,
        });
    }
    // 天空换成游戏侧生成的平色全景图（见 `sky` 模块）：参考风格是
    // 一整片低饱和淡蓝，没有太阳盘，解析大气散射在地平线附近反而会
    // 压出白带。SkySettings 保持缺省（不启用大气），接触阴影、曝光与
    // 暗角照常开启。
    if !world.contains_resource::<SkySource>() {
        world.insert_resource(crate::sky::sky_source());
    }
    if !world.contains_resource::<AoSettings>() {
        world.insert_resource(AoSettings {
            enabled: true,
            intensity: 1.35,
            radius: 0.4,
        });
    }
    if !world.contains_resource::<ExposureSettings>() {
        world.insert_resource(ExposureSettings { exposure: 1.06 });
    }
    if !world.contains_resource::<VignetteSettings>() {
        world.insert_resource(VignetteSettings { strength: 0.2 });
    }

    let world_data = VoxelWorld::generate(seed);
    let player = PlayerState::spawn();
    let collision = ChunkCollision::build(&world_data);
    world.insert_resource(world_data);
    world.insert_resource(player);
    world.insert_resource(Inventory::new());
    world.insert_resource(ChunkCollisionRes(collision));
    world.insert_resource(Hotbar { selected: 0 });
    world.insert_resource(SpawnPoint(player.position));
    world.insert_resource(VoxelScratch::default());

    if let Some(mut camera) = world.try_resource_mut::<Camera>() {
        camera.eye = player.eye();
        camera.target = camera.eye + forward(player.yaw, player.pitch);
        camera.up = Vec3::Y;
    }
}

fn build_world_and_assets(world: &mut World) {
    let atlas_texture = atlas::build_atlas();
    let atlas_handle = world
        .resource_mut::<TextureStore>()
        .add_uncompressed_declared(atlas_texture);
    // 经典体素观感：无光照材质 + 逐顶点光照与明暗。天空光、方块光与
    // AO 都由游戏侧烘进顶点色，材质保持 unlit，避免 Lit 材质在掠射角
    // 出现镜面高光。
    let material = world.resource_mut::<MaterialStore>().add(
        MaterialDef {
            texture: atlas_handle,
            roughness: 0.92,
            ..Default::default()
        }
        .with_vertex_color(VertexColorMode::Multiply)
        .with_pixel_snap()
        .with_unlit(),
    );
    // 树叶材质与主材质逐字段相同，只多一条 alpha 测试：叶片贴图里挖了
    // 孔洞，`base.a < 0.5` 的片元在着色前丢弃。Mask 仍走不透明分区，
    // 深度写入与批处理键不变。
    let leaves_material = world.resource_mut::<MaterialStore>().add(
        MaterialDef {
            texture: atlas_handle,
            roughness: 0.92,
            ..Default::default()
        }
        .with_vertex_color(VertexColorMode::Multiply)
        .with_pixel_snap()
        .with_unlit()
        // 阈值取 0.35 而不是 0.5：线性过滤会把缺口边缘的 alpha 平均到
        // 一半，0.5 的阈值会让每个 2×2 缺口缩成一两道细缝。压低阈值后
        // 缺口的实际尺寸与贴图接近。
        .with_alpha_mask(0.35),
    );

    let mut entries = Vec::with_capacity(CHUNK_COUNT);
    for cz in 0..WORLD_CHUNKS {
        for cx in 0..WORLD_CHUNKS {
            let coord = IVec2::new(cx as i32, cz as i32);
            let meshes = {
                let world_data = world.resource::<VoxelWorld>();
                build_chunk_meshes(&world_data, coord)
            };
            let cutout_visible = !meshes.cutout.indices.is_empty();
            let (opaque, cutout) = {
                let mut store = world.resource_mut::<MeshStore>();
                (store.add(meshes.opaque), store.add(meshes.cutout))
            };
            world.spawn((
                Transform::from_xyz((cx * CHUNK_SIZE) as f32, 0.0, (cz * CHUNK_SIZE) as f32),
                opaque,
                material,
            ));
            let cutout_entity = world.spawn((
                Transform::from_xyz((cx * CHUNK_SIZE) as f32, 0.0, (cz * CHUNK_SIZE) as f32),
                cutout,
                leaves_material,
                if cutout_visible {
                    Visibility::VISIBLE
                } else {
                    Visibility::HIDDEN
                },
            ));
            entries.push(ChunkEntry {
                coord,
                opaque,
                cutout,
                cutout_entity,
            });
        }
    }
    debug_assert!(
        entries
            .iter()
            .enumerate()
            .all(|(index, entry)| VoxelWorld::chunk_index(entry.coord) == Some(index)),
        "区块表下标必须与世界区块下标对齐"
    );
    world.insert_resource(ChunkTable { entries });

    // 方块云：参考风格里最醒目的一层。几十个盒子合并成一张网格，
    // unlit 近白色，不参与阴影。
    let seed = world.resource::<VoxelWorld>().seed;
    let cloud_mesh = world
        .resource_mut::<MeshStore>()
        .add(crate::clouds::cloud_mesh(seed));
    let cloud_material = world.resource_mut::<MaterialStore>().add(
        MaterialDef {
            base_color: [0.98, 0.96, 0.92, 1.0],
            // unlit 输出 albedo + emissive；云要比天空更亮才不会读成灰块。
            emissive: [0.34, 0.33, 0.31, 1.0],
            ..Default::default()
        }
        .with_unlit(),
    );
    world.spawn((
        Transform::IDENTITY,
        cloud_mesh,
        cloud_material,
        Visibility::VISIBLE,
    ));

    let highlight_mesh = world.resource_mut::<MeshStore>().add(wireframe_cube_mesh());
    let highlight_material = world
        .resource_mut::<MaterialStore>()
        .add(MaterialDef::solid(0.04, 0.04, 0.05, 1.0).with_unlit());
    let highlight = world.spawn((
        Transform::IDENTITY,
        highlight_mesh,
        highlight_material,
        Visibility::HIDDEN,
    ));
    world.insert_resource(Highlight(highlight));

    build_hud(world);
}

/// HUD 配色对齐 BLOCKWILD：墨绿玻璃面板、米白标题、灰绿正文、薄荷绿高亮。
/// UI 管线按线性 RGBA 消费，下列数值是目标 sRGB 色的线性等价。
const HUD_GLASS: [f32; 4] = [0.0044, 0.0085, 0.0066, 0.90];
const HUD_SLOT: [f32; 4] = [0.0100, 0.0153, 0.0116, 0.90];
const HUD_PAPER: [f32; 4] = [0.912, 0.869, 0.729, 1.0];
const HUD_MUTED: [f32; 4] = [0.507, 0.587, 0.492, 1.0];
const HUD_MINT: [f32; 4] = [0.380, 0.711, 0.477, 1.0];

fn build_hud(world: &mut World) {
    // 左上信息卡：标题、坐标与背包摘要。
    world.spawn((
        UiRect {
            position: [16.0, 16.0],
            size: [340.0, 100.0],
        },
        UiColor(HUD_GLASS),
    ));
    world.spawn((
        UiRect {
            position: [28.0, 24.0],
            size: [0.0, 0.0],
        },
        UiText {
            string: String::from("IRIS VOXEL"),
            scale: 2.2,
        },
        UiColor(HUD_PAPER),
    ));
    let info = world.spawn((
        UiRect {
            position: [28.0, 54.0],
            size: [0.0, 0.0],
        },
        UiText {
            string: String::from("XYZ 0 0 0"),
            scale: 1.15,
        },
        UiColor(HUD_MUTED),
    ));
    let bag = world.spawn((
        UiRect {
            position: [28.0, 76.0],
            size: [0.0, 0.0],
        },
        UiText {
            string: String::from("BAG EMPTY"),
            scale: 1.15,
        },
        UiColor(HUD_MUTED),
    ));

    // 快捷栏：玻璃底板 + 深色槽位 + 彩色方块块 + 序号。
    let panel_hotbar = world.spawn((
        UiRect {
            position: [0.0, 0.0],
            size: [0.0, 0.0],
        },
        UiColor(HUD_GLASS),
    ));
    let panel_label = world.spawn((
        UiRect {
            position: [0.0, 0.0],
            size: [0.0, 0.0],
        },
        UiColor(HUD_GLASS),
    ));
    let selection = world.spawn((
        UiRect {
            position: [0.0, 0.0],
            size: [46.0, 46.0],
        },
        UiColor(HUD_MINT),
    ));
    let mut slots = Vec::with_capacity(HOTBAR_BLOCKS.len());
    let mut swatches = Vec::with_capacity(HOTBAR_BLOCKS.len());
    let mut numbers = Vec::with_capacity(HOTBAR_BLOCKS.len());
    for (index, block) in HOTBAR_BLOCKS.into_iter().enumerate() {
        slots.push(world.spawn((
            UiRect {
                position: [0.0, 0.0],
                size: [40.0, 40.0],
            },
            UiColor(HUD_SLOT),
        )));
        swatches.push(world.spawn((
            UiRect {
                position: [0.0, 0.0],
                size: [28.0, 28.0],
            },
            UiColor(block.ui_color()),
        )));
        numbers.push(world.spawn((
            UiRect {
                position: [0.0, 0.0],
                size: [0.0, 0.0],
            },
            UiText {
                string: (index + 1).to_string(),
                scale: 1.0,
            },
            UiColor(HUD_MUTED),
        )));
    }
    let crosshair_h = world.spawn((
        UiRect {
            position: [0.0, 0.0],
            size: [12.0, 2.0],
        },
        UiColor([1.0, 1.0, 1.0, 0.9]),
    ));
    let crosshair_v = world.spawn((
        UiRect {
            position: [0.0, 0.0],
            size: [2.0, 12.0],
        },
        UiColor([1.0, 1.0, 1.0, 0.9]),
    ));
    let label = world.spawn((
        UiRect {
            position: [0.0, 0.0],
            size: [0.0, 0.0],
        },
        UiText {
            string: format!("{} x0", HOTBAR_BLOCKS[0].name()),
            scale: 1.6,
        },
        UiColor(HUD_PAPER),
    ));
    world.insert_resource(HudEntities {
        crosshair_h,
        crosshair_v,
        info,
        bag,
        panel_hotbar,
        panel_label,
        slots,
        swatches,
        numbers,
        selection,
        label,
    });
}

/// 左键首次请求鼠标捕获；已请求但未生效时重试（覆盖浏览器强制退出
/// 指针锁定）。Esc 释放捕获，未捕获时退出。
fn toggle_capture(world: &mut World) {
    let (left, escape) = {
        let Some(input) = world.try_resource::<Input>() else {
            return;
        };
        (
            input.mouse_button_pressed(MouseButton::Left),
            input.key_pressed(KeyCode::Escape),
        )
    };
    if left {
        if let Some(mut capture) = world.try_resource_mut::<MouseCapture>() {
            if capture.is_requested() {
                if !capture.is_active() {
                    capture.retry();
                }
            } else {
                capture.request(true);
            }
        }
    }
    if escape {
        let requested = world
            .try_resource::<MouseCapture>()
            .is_some_and(|capture| capture.is_requested());
        if requested {
            if let Some(mut capture) = world.try_resource_mut::<MouseCapture>() {
                capture.request(false);
            }
        } else {
            world.send_event(AppExit);
        }
    }
}

fn update_look(world: &mut World) {
    let delta = {
        let Some(input) = world.try_resource::<Input>() else {
            return;
        };
        if !input.is_relative() {
            return;
        }
        input.mouse_delta()
    };
    if delta == (0.0, 0.0) {
        return;
    }
    let mut player = world.resource_mut::<PlayerState>();
    let (yaw, pitch) = apply_look(player.yaw, player.pitch, delta);
    player.yaw = yaw;
    player.pitch = pitch;
}

fn update_hotbar(world: &mut World) {
    let (picked, wheel) = {
        let Some(input) = world.try_resource::<Input>() else {
            return;
        };
        let keys = [
            KeyCode::Digit1,
            KeyCode::Digit2,
            KeyCode::Digit3,
            KeyCode::Digit4,
            KeyCode::Digit5,
            KeyCode::Digit6,
            KeyCode::Digit7,
            KeyCode::Digit8,
        ];
        let picked = keys.iter().position(|key| input.key_pressed(*key));
        (picked, input.wheel_delta())
    };
    let mut hotbar = world.resource_mut::<Hotbar>();
    if let Some(index) = picked {
        hotbar.select(index);
    } else if wheel != 0.0 {
        hotbar.cycle(if wheel > 0.0 { -1 } else { 1 });
    }
}

fn step_player(world: &mut World) {
    let Some(dt) = world.try_resource::<Time>().map(|time| time.delta_secs()) else {
        return;
    };
    if dt <= 0.0 {
        return;
    }
    let Some(mut player) = world.try_resource::<PlayerState>().map(|player| *player) else {
        return;
    };
    let (forward_axis, right_axis, jump, sprint_held) = {
        let Some(input) = world.try_resource::<Input>() else {
            return;
        };
        (
            axis(input.key(KeyCode::KeyW), input.key(KeyCode::KeyS)),
            axis(input.key(KeyCode::KeyD), input.key(KeyCode::KeyA)),
            input.key(KeyCode::Space),
            input.key(KeyCode::ShiftLeft) || input.key(KeyCode::ShiftRight),
        )
    };

    player.sprinting = wants_sprint(forward_axis, sprint_held);
    let direction = wish_direction(player.yaw, forward_axis, right_axis);
    let speed = player.speed();
    player.velocity.x = direction.x * speed;
    player.velocity.z = direction.z * speed;
    if jump && player.grounded {
        player.velocity.y = JUMP_SPEED;
    }
    // 下落上限：重力倍率把落地速度推得很快，长距离坠落要有个上限，
    // 否则单帧位移过大、落地观感也会失真。
    if player.velocity.y < -TERMINAL_SPEED {
        player.velocity.y = -TERMINAL_SPEED;
    }

    let params = CharacterParams {
        radius: PLAYER_RADIUS,
        half_height: PLAYER_HALF_HEIGHT,
        step_height: 0.3,
        slope_limit_radians: std::f32::consts::FRAC_PI_4,
        snap_distance: 0.05,
        // 体素世界的下落要「重」：引擎按标准 9.81 积分，游戏侧按倍率放大。
        gravity_scale: GRAVITY_SCALE,
    };
    let output = {
        let mut scratch = world.resource_mut::<VoxelScratch>();
        {
            let collision = world.resource::<ChunkCollisionRes>();
            collision.0.gather(player.position, &mut scratch.statics);
        }
        move_and_slide_statics(
            player.position,
            player.velocity,
            dt,
            &params,
            &scratch.statics,
        )
    };
    player.position = output.position;
    player.velocity = output.velocity;
    player.grounded = output.grounded;

    if player.position.y < -8.0 {
        let spawn = world.resource::<SpawnPoint>().0;
        player.position = spawn;
        player.velocity = Vec3::ZERO;
        player.grounded = false;
    }
    *world.resource_mut::<PlayerState>() = player;
}

fn axis(positive: bool, negative: bool) -> f32 {
    match (positive, negative) {
        (true, false) => 1.0,
        (false, true) => -1.0,
        _ => 0.0,
    }
}

fn update_target_and_edits(world: &mut World) {
    let player = *world.resource::<PlayerState>();
    let (left, right, relative) = {
        let Some(input) = world.try_resource::<Input>() else {
            return;
        };
        (
            input.mouse_button_pressed(MouseButton::Left),
            input.mouse_button_pressed(MouseButton::Right),
            input.is_relative(),
        )
    };
    let hit = {
        let world_data = world.resource::<VoxelWorld>();
        pick_block(
            &world_data,
            player.eye(),
            forward(player.yaw, player.pitch),
            REACH,
        )
    };
    let highlight = world.resource::<Highlight>().0;
    if let Some(hit) = hit {
        if let Some(mut transform) = world.get_mut::<Transform>(highlight) {
            transform.position = Vec3::new(
                hit.block.x as f32 + 0.5,
                hit.block.y as f32 + 0.5,
                hit.block.z as f32 + 0.5,
            );
        }
        if let Some(mut visibility) = world.get_mut::<Visibility>(highlight) {
            visibility.visible = true;
        }
    } else if let Some(mut visibility) = world.get_mut::<Visibility>(highlight) {
        visibility.visible = false;
    }

    if !relative {
        return;
    }
    if left {
        if let Some(hit) = hit {
            let broken = world.resource::<VoxelWorld>().block(hit.block);
            if broken.is_breakable() && apply_edit(world, hit.block, Block::Air) {
                if let Some(drop) = broken.drop_item() {
                    world.resource_mut::<Inventory>().add(drop, 1);
                }
            }
        }
    }
    if right {
        if let Some(hit) = hit {
            let target = hit.block + hit.normal;
            let selected = world.resource::<Hotbar>().block();
            let placeable = {
                let world_data = world.resource::<VoxelWorld>();
                can_place(&world_data, &player, target)
            };
            if placeable {
                let consumed = world.resource_mut::<Inventory>().take(selected, 1);
                if consumed && !apply_edit(world, target, selected) {
                    // 世界拒绝写入时退还这一格，避免背包凭空少一块。
                    world.resource_mut::<Inventory>().add(selected, 1);
                }
            }
        }
    }
}

/// 写入一个方块并重建受影响区块的网格与碰撞壳。返回世界是否改变。
///
/// 几何脏区块重建网格与碰撞，光照脏区块只重建网格；纯光照变化不会
/// 改动碰撞壳。公开给 headless 测试复用边界编辑与邻区块重建路径。
pub fn apply_edit(world: &mut World, pos: IVec3, block: Block) -> bool {
    let outcome = {
        let mut world_data = world.resource_mut::<VoxelWorld>();
        world_data.set_block(pos, block)
    };
    if !outcome.changed {
        return false;
    }
    remesh_chunks(world, &outcome.dirty, &outcome.light_dirty);
    true
}

/// 重建一组区块的网格与碰撞壳。`MeshStore::update` 原地替换内容，
/// 区块实体与句柄保持不变。
pub fn remesh_chunks(world: &mut World, geometry_dirty: &[IVec2], light_dirty: &[IVec2]) {
    let mut mesh_dirty = geometry_dirty.to_vec();
    for &coord in light_dirty {
        if !mesh_dirty.contains(&coord) {
            mesh_dirty.push(coord);
        }
    }
    for &coord in &mesh_dirty {
        let meshes = {
            let world_data = world.resource::<VoxelWorld>();
            build_chunk_meshes(&world_data, coord)
        };
        let Some((opaque, cutout)) = chunk_meshes(world, coord) else {
            continue;
        };
        let cutout_visible = !meshes.cutout.indices.is_empty();
        {
            let mut store = world.resource_mut::<MeshStore>();
            store.update(opaque, meshes.opaque);
            store.update(cutout, meshes.cutout);
        }
        if let Some(entity) = cutout_entity(world, coord) {
            if let Some(mut visibility) = world.get_mut::<Visibility>(entity) {
                visibility.visible = cutout_visible;
            }
        }
    }
    for &coord in geometry_dirty {
        let world_data = world.resource::<VoxelWorld>();
        world
            .resource_mut::<ChunkCollisionRes>()
            .0
            .rebuild(&world_data, coord);
    }
}

fn sync_camera(world: &mut World) {
    let player = *world.resource::<PlayerState>();
    let dt = world
        .try_resource::<Time>()
        .map(|time| time.delta_secs())
        .unwrap_or(1.0 / 60.0);
    let mut camera = world.resource_mut::<Camera>();
    camera.eye = player.eye();
    camera.target = camera.eye + forward(player.yaw, player.pitch);
    camera.up = Vec3::Y;
    // 奔跑时视场角外扩：速度感主要来自周边视野的流动，而不是屏幕中心。
    // 指数收敛，松开 Shift 后平滑回到基准角。
    let target = BASE_FOV
        + if player.sprinting {
            SPRINT_FOV_BOOST
        } else {
            0.0
        };
    let blend = 1.0 - (-FOV_RESPONSE * dt).exp();
    camera.fov_y_radians += (target - camera.fov_y_radians) * blend;
}

fn update_hud(world: &mut World) {
    let (viewport_w, viewport_h) = world
        .try_resource::<Viewport>()
        .map(|viewport| (viewport.width as f32, viewport.height as f32))
        .unwrap_or((1280.0, 720.0));
    let (selected, name, count, position) = {
        let hotbar = world.resource::<Hotbar>();
        let block = hotbar.block();
        let count = world.resource::<Inventory>().count(block);
        let position = world.resource::<PlayerState>().position;
        (hotbar.selected, block.name(), count, position)
    };
    let (
        crosshair_h,
        crosshair_v,
        info_entity,
        bag_entity,
        panel_hotbar,
        panel_label,
        selection,
        label,
        slots,
        swatches,
        numbers,
    ) = {
        let hud = world.resource::<HudEntities>();
        (
            hud.crosshair_h,
            hud.crosshair_v,
            hud.info,
            hud.bag,
            hud.panel_hotbar,
            hud.panel_label,
            hud.selection,
            hud.label,
            hud.slots.clone(),
            hud.swatches.clone(),
            hud.numbers.clone(),
        )
    };

    let center_x = (viewport_w * 0.5).floor();
    let center_y = (viewport_h * 0.5).floor();
    set_rect(world, crosshair_h, center_x - 6.0, center_y - 1.0);
    set_rect(world, crosshair_v, center_x - 1.0, center_y - 6.0);

    // 快捷栏：玻璃底板、薄荷绿选中框、深色槽位、彩色方块块与序号。
    let slot = 40.0;
    let gap = 6.0;
    let total = slots.len() as f32 * slot + (slots.len() as f32 - 1.0) * gap;
    let start_x = ((viewport_w - total) * 0.5).floor();
    let start_y = (viewport_h - slot - 30.0).floor();
    set_size_and_pos(
        world,
        panel_hotbar,
        start_x - 10.0,
        start_y - 10.0,
        total + 20.0,
        60.0,
    );
    for (index, entity) in slots.into_iter().enumerate() {
        let x = start_x + index as f32 * (slot + gap);
        set_rect(world, entity, x, start_y);
    }
    for (index, entity) in swatches.into_iter().enumerate() {
        let x = start_x + index as f32 * (slot + gap) + 6.0;
        set_rect(world, entity, x, start_y + 6.0);
    }
    for (index, entity) in numbers.into_iter().enumerate() {
        let x = start_x + index as f32 * (slot + gap) + 4.0;
        set_rect(world, entity, x, start_y + 3.0);
    }
    set_size_and_pos(
        world,
        selection,
        start_x + selected as f32 * (slot + gap) - 3.0,
        start_y - 3.0,
        46.0,
        46.0,
    );

    // 方块名显示在快捷栏上方的玻璃小条里。
    let label_text = format!("{name} x{count}");
    let label_scale = 1.6;
    let label_width = label_text.chars().count() as f32 * 8.0 * label_scale;
    let label_height = 8.0 * label_scale;
    let pill_w = label_width + 24.0;
    let pill_h = label_height + 12.0;
    let pill_x = ((viewport_w - pill_w) * 0.5).floor();
    let pill_y = start_y - pill_h - 10.0;
    set_size_and_pos(world, panel_label, pill_x, pill_y, pill_w, pill_h);
    set_size_and_pos(
        world,
        label,
        pill_x + 12.0,
        pill_y + 6.0,
        label_width,
        label_height,
    );
    set_text(world, label, &label_text);

    // 左上信息卡：坐标与背包摘要（最多三项，超出用省略号）。
    let info_text = format!(
        "XYZ {} {} {}",
        position.x.floor() as i32,
        position.y.floor() as i32,
        position.z.floor() as i32
    );
    set_text(world, info_entity, &info_text);
    let bag_text = {
        let inventory = world.resource::<Inventory>();
        let mut text = String::from("BAG");
        let mut shown = 0;
        for (block, amount) in inventory.entries() {
            if shown == 3 {
                text.push_str(" ...");
                break;
            }
            text.push_str(&format!(" {} {}", block.name(), amount));
            shown += 1;
        }
        if shown == 0 {
            text.push_str(" EMPTY");
        }
        text
    };
    set_text(world, bag_entity, &bag_text);
}

fn set_rect(world: &mut World, entity: Entity, x: f32, y: f32) {
    if let Some(mut rect) = world.get_mut::<UiRect>(entity) {
        rect.position = [x, y];
    }
}

fn set_size_and_pos(world: &mut World, entity: Entity, x: f32, y: f32, w: f32, h: f32) {
    if let Some(mut rect) = world.get_mut::<UiRect>(entity) {
        rect.position = [x, y];
        rect.size = [w, h];
    }
}

fn set_text(world: &mut World, entity: Entity, text: &str) {
    if let Some(mut ui_text) = world.get_mut::<UiText>(entity) {
        if ui_text.string != text {
            ui_text.string = text.to_string();
        }
    }
}

/// 世界尺寸诊断用常量（测试与外部工具可读）。
pub const WORLD_BLOCK_COUNT: usize = WORLD_SIZE * WORLD_SIZE * CHUNK_HEIGHT;
