//! 引擎接线：资源、区块实体、输入、物理、编辑与 HUD。
//!
//! 所有玩法状态都放在资源里；区块网格句柄在启动时一次性创建，编辑后
//! 通过 `MeshStore::update` 原地替换内容，句柄不增长。

use glam::{IVec2, IVec3, Vec3};
use iris_engine::prelude::*;

use crate::atlas;
use crate::collision::{ChunkCollision, StaticBox, can_place};
use crate::logic::block::Block;
use crate::logic::player::{
    JUMP_SPEED, PLAYER_HALF_HEIGHT, PLAYER_RADIUS, PlayerState, WALK_SPEED, apply_look, forward,
    wish_direction,
};
use crate::logic::raycast::pick_block;
use crate::logic::world::{
    CHUNK_COUNT, CHUNK_HEIGHT, CHUNK_SIZE, VoxelWorld, WORLD_CHUNKS, WORLD_SIZE,
};
use crate::mesh::{build_chunk_mesh, wireframe_cube_mesh};

/// 拾取与放置的最大距离（米）。
pub const REACH: f32 = 5.0;
/// 快捷栏按顺序放置的方块。
pub const HOTBAR_BLOCKS: [Block; 7] = [
    Block::Grass,
    Block::Dirt,
    Block::Stone,
    Block::Sand,
    Block::Wood,
    Block::Planks,
    Block::Glowstone,
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

/// 区块实体与网格句柄，下标与 `VoxelWorld::chunk_index` 对齐。
#[derive(Resource)]
struct ChunkTable {
    entries: Vec<ChunkEntry>,
}

struct ChunkEntry {
    coord: IVec2,
    mesh: MeshHandle,
}

impl ChunkTable {
    fn mesh(&self, coord: IVec2) -> Option<MeshHandle> {
        VoxelWorld::chunk_index(coord).map(|index| self.entries[index].mesh)
    }
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
    slots: Vec<Entity>,
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
        world.insert_resource(DirectionalLight::default());
    }
    if !world.contains_resource::<Viewport>() {
        world.insert_resource(Viewport::default());
    }
    // 方块纹理是高频率像素画；开启 mip 链与各向异性过滤，抑制远处
    // 单 mip 采样产生的闪烁。
    if !world.contains_resource::<MipSettings>() {
        world.insert_resource(MipSettings::default());
    }

    let world_data = VoxelWorld::generate(seed);
    let player = PlayerState::spawn();
    let collision = ChunkCollision::build(&world_data);
    world.insert_resource(world_data);
    world.insert_resource(player);
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
        .with_unlit(),
    );

    let mut entries = Vec::with_capacity(CHUNK_COUNT);
    for cz in 0..WORLD_CHUNKS {
        for cx in 0..WORLD_CHUNKS {
            let coord = IVec2::new(cx as i32, cz as i32);
            let data = {
                let world_data = world.resource::<VoxelWorld>();
                build_chunk_mesh(&world_data, coord)
            };
            let mesh = world.resource_mut::<MeshStore>().add(data);
            world.spawn((
                Transform::from_xyz((cx * CHUNK_SIZE) as f32, 0.0, (cz * CHUNK_SIZE) as f32),
                mesh,
                material,
            ));
            entries.push(ChunkEntry { coord, mesh });
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

fn build_hud(world: &mut World) {
    let selection = world.spawn((
        UiRect {
            position: [0.0, 0.0],
            size: [46.0, 46.0],
        },
        UiColor([1.0, 1.0, 1.0, 0.95]),
    ));
    let mut slots = Vec::with_capacity(HOTBAR_BLOCKS.len());
    for block in HOTBAR_BLOCKS {
        slots.push(world.spawn((
            UiRect {
                position: [0.0, 0.0],
                size: [40.0, 40.0],
            },
            UiColor(block.ui_color()),
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
            string: format!("SELECTED: {}", HOTBAR_BLOCKS[0].name()),
            scale: 2.0,
        },
        UiColor([1.0, 1.0, 1.0, 1.0]),
    ));
    world.insert_resource(HudEntities {
        crosshair_h,
        crosshair_v,
        slots,
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
    let (forward_axis, right_axis, jump) = {
        let Some(input) = world.try_resource::<Input>() else {
            return;
        };
        (
            axis(input.key(KeyCode::KeyW), input.key(KeyCode::KeyS)),
            axis(input.key(KeyCode::KeyD), input.key(KeyCode::KeyA)),
            input.key(KeyCode::Space),
        )
    };

    let direction = wish_direction(player.yaw, forward_axis, right_axis);
    player.velocity.x = direction.x * WALK_SPEED;
    player.velocity.z = direction.z * WALK_SPEED;
    if jump && player.grounded {
        player.velocity.y = JUMP_SPEED;
    }

    let params = CharacterParams {
        radius: PLAYER_RADIUS,
        half_height: PLAYER_HALF_HEIGHT,
        step_height: 0.3,
        slope_limit_radians: std::f32::consts::FRAC_PI_4,
        snap_distance: 0.05,
        gravity_scale: 1.0,
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
            let breakable = {
                let world_data = world.resource::<VoxelWorld>();
                world_data.block(hit.block).is_breakable()
            };
            if breakable {
                apply_edit(world, hit.block, Block::Air);
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
                apply_edit(world, target, selected);
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
        let data = {
            let world_data = world.resource::<VoxelWorld>();
            build_chunk_mesh(&world_data, coord)
        };
        if let Some(handle) = world.resource::<ChunkTable>().mesh(coord) {
            world.resource_mut::<MeshStore>().update(handle, data);
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
    let mut camera = world.resource_mut::<Camera>();
    camera.eye = player.eye();
    camera.target = camera.eye + forward(player.yaw, player.pitch);
    camera.up = Vec3::Y;
}

fn update_hud(world: &mut World) {
    let (viewport_w, viewport_h) = world
        .try_resource::<Viewport>()
        .map(|viewport| (viewport.width as f32, viewport.height as f32))
        .unwrap_or((1280.0, 720.0));
    let (selected, name) = {
        let hotbar = world.resource::<Hotbar>();
        (hotbar.selected, hotbar.block().name())
    };
    let (crosshair_h, crosshair_v, selection, label, slots) = {
        let hud = world.resource::<HudEntities>();
        (
            hud.crosshair_h,
            hud.crosshair_v,
            hud.selection,
            hud.label,
            hud.slots.clone(),
        )
    };

    let center_x = (viewport_w * 0.5).floor();
    let center_y = (viewport_h * 0.5).floor();
    set_rect(world, crosshair_h, center_x - 6.0, center_y - 1.0);
    set_rect(world, crosshair_v, center_x - 1.0, center_y - 6.0);

    let slot = 40.0;
    let gap = 6.0;
    let total = slots.len() as f32 * slot + (slots.len() as f32 - 1.0) * gap;
    let start_x = ((viewport_w - total) * 0.5).floor();
    let start_y = (viewport_h - slot - 24.0).floor();
    for (index, entity) in slots.into_iter().enumerate() {
        set_rect(
            world,
            entity,
            start_x + index as f32 * (slot + gap),
            start_y,
        );
    }
    set_rect(
        world,
        selection,
        start_x + selected as f32 * (slot + gap) - 3.0,
        start_y - 3.0,
    );

    let text = format!("SELECTED: {name}");
    let scale = 2.0;
    let text_width = text.chars().count() as f32 * 8.0 * scale;
    let text_height = 8.0 * scale;
    let text_x = ((viewport_w - text_width) * 0.5).floor();
    let text_y = start_y - text_height - 8.0;
    if let Some(mut rect) = world.get_mut::<UiRect>(label) {
        rect.position = [text_x, text_y];
        rect.size = [text_width, text_height];
    }
    if let Some(mut ui_text) = world.get_mut::<UiText>(label) {
        if ui_text.string != text {
            ui_text.string = text;
        }
    }
}

fn set_rect(world: &mut World, entity: Entity, x: f32, y: f32) {
    if let Some(mut rect) = world.get_mut::<UiRect>(entity) {
        rect.position = [x, y];
    }
}

/// 世界尺寸诊断用常量（测试与外部工具可读）。
pub const WORLD_BLOCK_COUNT: usize = WORLD_SIZE * WORLD_SIZE * CHUNK_HEIGHT;
