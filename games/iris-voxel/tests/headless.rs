//! headless 集成测试：不建窗口、不建 GPU，复用 `VoxelPlugin` 的完整
//! 系统链驱动真实世界。覆盖启动区块、第一人称移动与跳跃、相对鼠标
//! 视角、DDA 破坏与放置、边界区块邻接重建、HUD 选择，以及天空光与
//! 荧光石方块光的增量传播和跨区块网格重建。

use std::time::Duration;

use glam::{IVec2, IVec3, Vec3};
use iris_engine::prelude::{
    App, Camera, DirectionalLight, ElementState, Input, KeyCode, MaterialStore, MeshData,
    MeshHandle, MeshStore, MouseButton, MouseCapture, Stage, TextureStore, Time, UiText, Viewport,
};
use iris_voxel::VoxelPlugin;
use iris_voxel::atlas::tile_uv;
use iris_voxel::logic::{
    Block, CHUNK_COUNT, Inventory, PlayerState, Tile, VoxelWorld, forward, pick_block,
};
use iris_voxel::plugin::{REACH, apply_edit, chunk_cutout_visible, chunk_meshes};

struct Harness {
    app: App,
}

impl Harness {
    fn new(seed: u64) -> Self {
        let mut app = App::new();
        app.world.insert_resource(Time::default());
        app.world.insert_resource(Input::new());
        app.world.insert_resource(MouseCapture::new());
        app.world.insert_resource(MeshStore::new());
        app.world.insert_resource(MaterialStore::new());
        app.world.insert_resource(TextureStore::new());
        app.world.insert_resource(Camera::default());
        app.world.insert_resource(DirectionalLight::default());
        app.world.insert_resource(Viewport::new(1280, 720));
        VoxelPlugin::new(seed).build_schedule(&mut app.schedule);
        app.schedule.run(Stage::Startup, &mut app.world);
        Self { app }
    }

    /// 推进一帧：镜像 App::tick 的事件轮换、固定 1/60 推进与输入收尾。
    fn frame(&mut self) {
        self.app.world.clear_all_events();
        self.app
            .world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(1.0 / 60.0));
        self.app.schedule.run(Stage::Update, &mut self.app.world);
        self.app
            .schedule
            .run(Stage::PostUpdate, &mut self.app.world);
        self.app.world.resource_mut::<Input>().end_frame();
    }

    fn frames(&mut self, count: usize) {
        for _ in 0..count {
            self.frame();
        }
    }

    fn settle(&mut self) {
        self.frames(10);
    }

    fn player(&self) -> PlayerState {
        *self.app.world.resource::<PlayerState>()
    }

    fn set_player(&mut self, player: PlayerState) {
        *self.app.world.resource_mut::<PlayerState>() = player;
    }

    fn world(&self) -> iris_core::resource::ResourceRef<'_, VoxelWorld> {
        self.app.world.resource::<VoxelWorld>()
    }

    fn set_relative(&mut self) {
        self.app.world.resource_mut::<Input>().set_relative(true);
    }

    fn press(&mut self, key: KeyCode) {
        self.app.world.resource_mut::<Input>().press(key);
    }

    fn release(&mut self, key: KeyCode) {
        self.app.world.resource_mut::<Input>().release(key);
    }

    fn click(&mut self, button: MouseButton) {
        let mut input = self.app.world.resource_mut::<Input>();
        input.on_mouse_button(button, ElementState::Released);
        input.on_mouse_button(button, ElementState::Pressed);
    }

    fn look(&mut self, delta: (f64, f64)) {
        self.app
            .world
            .resource_mut::<Input>()
            .on_mouse_motion(delta);
    }

    /// 区块的不透明分区网格句柄。
    fn mesh_handle_at(&self, coord: IVec2) -> MeshHandle {
        chunk_meshes(&self.app.world, coord)
            .unwrap_or_else(|| panic!("区块 {coord:?} 没有网格实体"))
            .0
    }

    /// 区块的裁切分区（树叶）网格句柄。
    fn cutout_mesh_at(&self, coord: IVec2) -> MeshHandle {
        chunk_meshes(&self.app.world, coord)
            .unwrap_or_else(|| panic!("区块 {coord:?} 没有网格实体"))
            .1
    }

    fn revision(&self, mesh: MeshHandle) -> u64 {
        self.app
            .world
            .resource::<MeshStore>()
            .revision(mesh)
            .expect("区块网格句柄应有效")
    }

    fn hud_label(&self) -> String {
        let mut label = None;
        self.app.world.query::<&UiText>(|text| {
            if text.string.contains(" x")
                && !text.string.starts_with("BAG")
                && !text.string.starts_with("XYZ")
            {
                label = Some(text.string.clone());
            }
        });
        label.expect("HUD 应存在方块标签")
    }

    fn hud_summary(&self) -> String {
        let mut summary = None;
        self.app.world.query::<&UiText>(|text| {
            if text.string.starts_with("BAG") {
                summary = Some(text.string.clone());
            }
        });
        summary.expect("HUD 应存在背包汇总")
    }

    fn inventory_count(&self, block: Block) -> u32 {
        self.app.world.resource::<Inventory>().count(block)
    }

    fn grant(&mut self, block: Block, amount: u32) {
        self.app
            .world
            .resource_mut::<Inventory>()
            .add(block, amount);
    }
}

/// 启动后每个区块两套网格（不透明 + 裁切）、目标框与云层各一个；
/// 玩家落在平台上，相机与玩家眼睛同步。
#[test]
fn startup_builds_chunks_and_grounds_player() {
    let mut harness = Harness::new(1);
    let mut meshes = 0;
    harness.app.world.query::<&MeshHandle>(|_| meshes += 1);
    assert_eq!(
        meshes,
        CHUNK_COUNT * 2 + 2,
        "每区块两套网格 + 目标框 + 云层"
    );

    harness.settle();
    let player = harness.player();
    assert!(player.grounded, "平台应站住");
    assert!(
        (player.position.y - 15.9).abs() < 0.05,
        "平台顶层 y=14，胶囊中心应落在 15.9 附近：{}",
        player.position.y
    );
    let camera = harness.app.world.resource::<Camera>();
    assert!((camera.eye - player.eye()).length() < 1e-6);
    assert!((camera.target - camera.eye).length() > 0.9);
}

/// 按住 W 30 帧沿 -Z 前进并保持贴地。
#[test]
fn walking_moves_forward_on_plateau() {
    let mut harness = Harness::new(2);
    harness.settle();
    let start = harness.player();
    harness.press(KeyCode::KeyW);
    harness.frames(30);
    harness.release(KeyCode::KeyW);
    harness.frame();
    let end = harness.player();
    let distance = start.position.z - end.position.z;
    assert!(
        (1.8..=2.5).contains(&distance),
        "0.5 秒步行应前进约 2.15 米，实际 {distance}"
    );
    assert!(end.grounded);
    assert!((end.position.x - start.position.x).abs() < 1e-3);
}

/// 跳跃峰值约 v²/2g ≈ 1.24 米（起跳 8.5 m/s、重力 27.5 m/s²），
/// 随后落回并恢复 grounded。
#[test]
fn jump_peaks_and_lands() {
    let mut harness = Harness::new(3);
    harness.settle();
    let ground = harness.player().position.y;
    harness.press(KeyCode::Space);
    harness.frame();
    harness.release(KeyCode::Space);
    let mut peak = ground;
    for _ in 0..90 {
        harness.frame();
        peak = peak.max(harness.player().position.y);
    }
    let height = peak - ground;
    assert!(
        (1.0..=1.7).contains(&height),
        "跳跃高度应接近 1.24 米，实际 {height}"
    );
    let player = harness.player();
    assert!(player.grounded, "90 帧后应落地");
    assert!((player.position.y - ground).abs() < 0.05);
}

/// 自由落体按体素重力加速：0.2 秒后末速约 5.5 m/s、落下约 0.6 米，
/// 标准重力同期只有 2.0 m/s 与 0.2 米。
#[test]
fn falling_accelerates_at_voxel_gravity() {
    let mut harness = Harness::new(4);
    harness.settle();
    let mut airborne = harness.player();
    airborne.position += Vec3::Y * 12.0;
    airborne.velocity = Vec3::ZERO;
    airborne.grounded = false;
    harness.set_player(airborne);

    harness.frames(12);
    let player = harness.player();
    assert!(
        player.velocity.y < -5.0,
        "0.2 秒后应明显加速下落，实际 {}",
        player.velocity.y
    );
    assert!(
        player.velocity.y > -6.2,
        "不该超过逐帧积分的量级，实际 {}",
        player.velocity.y
    );
    let fallen = airborne.position.y - player.position.y;
    assert!(
        (0.5..=0.75).contains(&fallen),
        "0.2 秒应落下约 0.6 米，实际 {fallen}"
    );
    assert!(!player.grounded, "还在空中");
}

/// 按住 Shift + W 进入奔跑：0.5 秒比行走多走约三成，松开前进键立刻退出。
#[test]
fn sprint_outruns_walking_and_stops_on_release() {
    let mut walk = Harness::new(5);
    walk.settle();
    let walk_start = walk.player();
    walk.press(KeyCode::KeyW);
    walk.frames(30);
    walk.release(KeyCode::KeyW);
    walk.frame();
    let walked = walk_start.position.z - walk.player().position.z;

    let mut run = Harness::new(5);
    run.settle();
    let run_start = run.player();
    run.press(KeyCode::KeyW);
    run.press(KeyCode::ShiftLeft);
    run.frames(30);
    let sprinting = run.player();
    assert!(sprinting.sprinting, "按住 Shift 与 W 应进入奔跑");
    run.release(KeyCode::KeyW);
    run.release(KeyCode::ShiftLeft);
    run.frame();
    let end = run.player();
    let ran = run_start.position.z - end.position.z;
    assert!(ran > walked + 0.5, "奔跑应明显快于行走：{ran} 对 {walked}");
    assert!(
        (2.6..=3.0).contains(&ran),
        "奔跑 0.5 秒应前进约 2.8 米，实际 {ran}"
    );
    assert!(!end.sprinting, "松开前进键应退回行走");
}

/// 奔跑要求前进输入：只按 Shift + A 横移仍按行走速度。
#[test]
fn sprint_requires_forward_input() {
    let mut harness = Harness::new(6);
    harness.settle();
    let start = harness.player();
    harness.press(KeyCode::KeyA);
    harness.press(KeyCode::ShiftLeft);
    harness.frames(30);
    let player = harness.player();
    assert!(!player.sprinting, "横移不进入奔跑");
    let strafed = (player.position.x - start.position.x).abs();
    assert!(
        (1.8..=2.5).contains(&strafed),
        "横移应保持行走速度，实际 {strafed}"
    );
    harness.release(KeyCode::KeyA);
    harness.release(KeyCode::ShiftLeft);
}

/// 奔跑时视场角外扩，松开后平滑回到基准角。
#[test]
fn sprint_widens_fov_and_settles_back() {
    let mut harness = Harness::new(7);
    harness.settle();
    let base = harness.app.world.resource::<Camera>().fov_y_radians;
    harness.press(KeyCode::KeyW);
    harness.press(KeyCode::ShiftLeft);
    harness.frames(30);
    let wide = harness.app.world.resource::<Camera>().fov_y_radians;
    assert!(wide > base + 0.1, "奔跑应外扩视场角：{wide} 对 {base}");

    harness.release(KeyCode::ShiftLeft);
    harness.frames(30);
    let settled = harness.app.world.resource::<Camera>().fov_y_radians;
    assert!(
        (settled - base).abs() < 0.01,
        "松开后应回到基准角：{settled} 对 {base}"
    );
    harness.release(KeyCode::KeyW);
}

/// 相对鼠标位移精确换算为 yaw / pitch，俯仰超限被钳制。
#[test]
fn relative_mouse_look_updates_angles() {
    let mut harness = Harness::new(4);
    harness.settle();
    harness.set_relative();
    harness.look((100.0, -50.0));
    harness.frame();
    let player = harness.player();
    assert!((player.yaw - (-0.25)).abs() < 1e-6, "yaw={}", player.yaw);
    assert!(
        (player.pitch - 0.125).abs() < 1e-6,
        "pitch={}",
        player.pitch
    );

    harness.look((0.0, -1_000_000.0));
    harness.frame();
    assert!((harness.player().pitch - iris_voxel::logic::PITCH_LIMIT).abs() < 1e-6);
}

/// 俯视左键破坏脚下方块，目标区块网格修订号前进。
#[test]
fn breaking_block_updates_world_and_mesh() {
    let mut harness = Harness::new(5);
    harness.settle();
    harness.set_relative();
    let mut player = harness.player();
    player.pitch = -std::f32::consts::FRAC_PI_2;
    harness.set_player(player);
    harness.frame();

    let target = IVec3::new(64, 14, 64);
    assert!(harness.world().block(target).is_solid());
    let mesh = harness.mesh_handle_at(IVec2::new(4, 4));
    let before = harness.revision(mesh);
    harness.click(MouseButton::Left);
    harness.frame();

    assert_eq!(
        harness.world().block(target),
        Block::Air,
        "脚下方块应被挖掉"
    );
    assert!(harness.revision(mesh) > before, "目标区块网格应重建");
    assert_eq!(
        harness.inventory_count(Block::Dirt),
        1,
        "挖掉草方块应得到一块泥土"
    );
    assert_eq!(harness.hud_summary(), "BAG DIRT 1");
}

/// 选择石头后右键在命中面上放置；放置目标与玩家不重叠。
#[test]
fn placing_block_writes_selected_material() {
    let mut harness = Harness::new(6);
    harness.settle();
    harness.set_relative();
    let mut player = harness.player();
    player.yaw = 0.0;
    player.pitch = -std::f32::consts::FRAC_PI_4;
    harness.set_player(player);
    harness.frame();

    let player = harness.player();
    let hit = pick_block(
        &harness.world(),
        player.eye(),
        forward(player.yaw, player.pitch),
        REACH,
    )
    .expect("俯视前方应命中平台");
    let target = hit.block + hit.normal;
    assert_eq!(harness.world().block(target), Block::Air);

    harness.grant(Block::Stone, 1);
    harness.press(KeyCode::Digit3);
    harness.click(MouseButton::Right);
    harness.frame();
    harness.release(KeyCode::Digit3);

    assert_eq!(
        harness.world().block(target),
        Block::Stone,
        "右键应在命中面相邻格放置选中的石头"
    );
    assert_eq!(
        harness.inventory_count(Block::Stone),
        0,
        "放置应消耗一格石头"
    );
}

/// 背包没有对应方块时右键不放置。
#[test]
fn placing_with_empty_inventory_is_refused() {
    let mut harness = Harness::new(12);
    harness.settle();
    harness.set_relative();
    let mut player = harness.player();
    player.yaw = 0.0;
    player.pitch = -std::f32::consts::FRAC_PI_4;
    harness.set_player(player);
    harness.frame();

    let player = harness.player();
    let hit = pick_block(
        &harness.world(),
        player.eye(),
        forward(player.yaw, player.pitch),
        REACH,
    )
    .expect("俯视前方应命中平台");
    let target = hit.block + hit.normal;
    assert_eq!(harness.inventory_count(Block::Stone), 0);

    harness.press(KeyCode::Digit3);
    harness.click(MouseButton::Right);
    harness.frame();
    harness.release(KeyCode::Digit3);

    assert_eq!(
        harness.world().block(target),
        Block::Air,
        "背包为空时不应凭空放置方块"
    );
}

/// 边界编辑会同时重建自身与相邻区块；其余区块不受影响。
#[test]
fn border_edit_remeshes_neighbour_chunks() {
    let mut harness = Harness::new(7);
    harness.settle();
    let own = harness.mesh_handle_at(IVec2::new(0, 0));
    let east = harness.mesh_handle_at(IVec2::new(1, 0));
    let south = harness.mesh_handle_at(IVec2::new(0, 1));
    let far = harness.mesh_handle_at(IVec2::new(5, 5));
    let (own_before, east_before, south_before, far_before) = (
        harness.revision(own),
        harness.revision(east),
        harness.revision(south),
        harness.revision(far),
    );

    assert!(apply_edit(
        &mut harness.app.world,
        IVec3::new(15, 40, 15),
        Block::Stone
    ));

    assert!(harness.revision(own) > own_before);
    assert!(harness.revision(east) > east_before);
    assert!(harness.revision(south) > south_before);
    assert_eq!(harness.revision(far), far_before, "远处区块不应重建");
}

/// 启动后平台上方空气直接露天，天空光为 15；实心格与地下没有方块光。
#[test]
fn startup_world_has_sky_light() {
    let mut harness = Harness::new(9);
    harness.settle();
    let world = harness.world();
    assert_eq!(world.light().sky(IVec3::new(64, 15, 64)), 15);
    assert_eq!(world.light().sky(IVec3::new(64, 14, 64)), 0);
    assert_eq!(world.light().block_light(IVec3::new(64, 15, 64)), 0);
}

/// 在跨区块位置放荧光石：附近方块光为 14，光照波及的区块网格全部
/// 重建，远处区块不受影响；拆掉光源后方块光归零。
#[test]
fn glowstone_relights_surroundings_and_remeshes_cross_chunk() {
    let mut harness = Harness::new(10);
    harness.settle();
    let source = IVec3::new(65, 40, 65);
    let chunks = [
        IVec2::new(3, 3),
        IVec2::new(3, 4),
        IVec2::new(4, 3),
        IVec2::new(4, 4),
        IVec2::new(4, 5),
        IVec2::new(5, 4),
    ];
    let handles: Vec<MeshHandle> = chunks
        .iter()
        .map(|&coord| harness.mesh_handle_at(coord))
        .collect();
    let before: Vec<u64> = handles
        .iter()
        .map(|&handle| harness.revision(handle))
        .collect();
    let far = harness.mesh_handle_at(IVec2::new(6, 4));
    let far_before = harness.revision(far);
    let diagonal = harness.mesh_handle_at(IVec2::new(5, 5));
    let diagonal_before = harness.revision(diagonal);

    assert!(apply_edit(&mut harness.app.world, source, Block::Glowstone));
    assert_eq!(harness.world().light().block_light(source), 15);
    for offset in [IVec3::new(0, -1, 0), IVec3::X, IVec3::Z] {
        assert_eq!(
            harness.world().light().block_light(source + offset),
            14,
            "光源邻格 {offset:?} 应为 14"
        );
    }
    for (handle, before) in handles.iter().zip(before) {
        assert!(
            harness.revision(*handle) > before,
            "光照波及的区块应重建网格"
        );
    }
    assert_eq!(harness.revision(far), far_before, "远处区块不应重建");
    assert_eq!(
        harness.revision(diagonal),
        diagonal_before,
        "光照 14 格未到达对角区块"
    );

    assert!(apply_edit(&mut harness.app.world, source, Block::Air));
    assert_eq!(harness.world().light().block_light(source + IVec3::X), 0);
}

/// 快捷栏选择改变 HUD 标签，标签同时显示背包数量。
#[test]
fn hotbar_selection_updates_hud_label() {
    let mut harness = Harness::new(8);
    harness.settle();
    assert_eq!(harness.hud_label(), "GRASS x0");
    assert_eq!(harness.hud_summary(), "BAG EMPTY");
    harness.grant(Block::Stone, 5);
    harness.press(KeyCode::Digit3);
    harness.frame();
    harness.release(KeyCode::Digit3);
    assert_eq!(harness.hud_label(), "STONE x5");
    assert_eq!(harness.hud_summary(), "BAG STONE 5");
    harness.press(KeyCode::Digit7);
    harness.frame();
    harness.release(KeyCode::Digit7);
    assert_eq!(harness.hud_label(), "GLOWSTONE x0");
    harness.press(KeyCode::Digit8);
    harness.frame();
    harness.release(KeyCode::Digit8);
    assert_eq!(harness.hud_label(), "LEAVES x0");
}

/// 生成的世界内容确实进入渲染网格：裁切分区里有树叶槽位，不透明分区
/// 里有矿石槽位，说明分区、图集与实体接线对新增方块生效。
#[test]
fn generated_trees_and_ores_reach_chunk_meshes() {
    let harness = Harness::new(11);
    let mut leaves = false;
    let mut ores = false;
    for cz in 0..8 {
        for cx in 0..8 {
            let coord = IVec2::new(cx, cz);
            let (opaque, cutout) =
                chunk_meshes(&harness.app.world, coord).expect("区块应有网格句柄");
            let store = harness.app.world.resource::<MeshStore>();
            let cutout_mesh = store.get(cutout).expect("裁切网格句柄应有效");
            leaves |= has_tile(cutout_mesh, Tile::Leaves);
            let opaque_mesh = store.get(opaque).expect("不透明网格句柄应有效");
            ores |= [Tile::CoalOre, Tile::IronOre, Tile::GoldOre]
                .into_iter()
                .any(|tile| has_tile(opaque_mesh, tile));
        }
    }
    assert!(leaves, "裁切分区里应出现树叶面");
    assert!(ores, "不透明分区里应出现矿石面");
}

fn has_tile(mesh: &MeshData, tile: Tile) -> bool {
    let corner = tile_uv(tile, 0.0, 0.0);
    mesh.vertices.iter().any(|vertex| {
        (vertex.uv[0] - corner[0]).abs() < 1e-6 && (vertex.uv[1] - corner[1]).abs() < 1e-6
    })
}

/// 每个区块的裁切实体可见性都与分区内容一致：空分区隐藏，不产生绘制。
#[test]
fn chunk_cutout_visibility_follows_mesh_content() {
    let harness = Harness::new(13);
    let mut hidden = 0;
    for cz in 0..8 {
        for cx in 0..8 {
            let coord = IVec2::new(cx, cz);
            let (_, cutout) = chunk_meshes(&harness.app.world, coord).expect("区块应有网格");
            let empty = harness
                .app
                .world
                .resource::<MeshStore>()
                .get(cutout)
                .expect("裁切网格句柄应有效")
                .indices
                .is_empty();
            assert_eq!(
                chunk_cutout_visible(&harness.app.world, coord),
                !empty,
                "区块 {coord:?} 的可见性与网格内容不一致"
            );
            if empty {
                hidden += 1;
            }
        }
    }
    println!("空裁切分区 {hidden} 个");
}

/// 在树冠之外的空中补一片树叶：裁切分区多出六面，句柄原地更新。
#[test]
fn placing_leaves_grows_the_cutout_mesh() {
    let mut harness = Harness::new(14);
    harness.settle();
    let coord = IVec2::new(3, 3);
    let spot = IVec3::new(56, 40, 56);
    assert_eq!(harness.world().block(spot), Block::Air);
    let cutout = harness.cutout_mesh_at(coord);
    let before = harness.revision(cutout);
    let faces_before = harness
        .app
        .world
        .resource::<MeshStore>()
        .get(cutout)
        .expect("裁切网格句柄应有效")
        .indices
        .len()
        / 6;

    assert!(apply_edit(&mut harness.app.world, spot, Block::Leaves));
    assert!(harness.revision(cutout) > before, "裁切网格应重建");
    let mesh = harness.app.world.resource::<MeshStore>();
    let mesh = mesh.get(cutout).expect("裁切网格句柄应有效");
    assert_eq!(
        mesh.indices.len() / 6,
        faces_before + 6,
        "孤立叶块应新增六个面"
    );
    assert!(has_tile(mesh, Tile::Leaves));
    // 叶格自己也有天空光：15 减去「距离 1 + 透光代价 1」= 13。石头格是 0。
    assert_eq!(harness.world().light().sky(spot), 13, "叶格的天空光应为 13");
}
