//! headless 集成测试：不建窗口、不建 GPU，复用 `VoxelPlugin` 的完整
//! 系统链驱动真实世界。覆盖启动区块、第一人称移动与跳跃、相对鼠标
//! 视角、DDA 破坏与放置、边界区块邻接重建与 HUD 选择。

use std::time::Duration;

use glam::{IVec2, IVec3, Vec3};
use iris_engine::prelude::{
    App, Camera, DirectionalLight, ElementState, Input, KeyCode, MaterialStore, MeshHandle,
    MeshStore, MouseButton, MouseCapture, Stage, TextureStore, Time, Transform, UiText, Viewport,
};
use iris_voxel::VoxelPlugin;
use iris_voxel::logic::{Block, CHUNK_COUNT, PlayerState, VoxelWorld, forward, pick_block};
use iris_voxel::plugin::{REACH, apply_edit};

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

    fn mesh_handle_at(&self, coord: IVec2) -> MeshHandle {
        let origin = Vec3::new((coord.x * 16) as f32, 0.0, (coord.y * 16) as f32);
        let mut found = None;
        // 目标框实体也可能落在区块原点，用「没有 Visibility」区分
        // 区块实体（目标框恒挂 Visibility）。
        self.app.world.query::<(
            &Transform,
            &MeshHandle,
            Option<&iris_engine::prelude::Visibility>,
        )>(|(transform, mesh, visibility)| {
            if transform.position == origin && visibility.is_none() {
                found = Some(*mesh);
            }
        });
        found.unwrap_or_else(|| panic!("区块 {coord:?} 没有网格实体"))
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
            if text.string.starts_with("SELECTED:") {
                label = Some(text.string.clone());
            }
        });
        label.expect("HUD 应存在选择标签")
    }
}

/// 启动后 64 个区块网格加一个目标框网格；玩家落在平台上，
/// 相机与玩家眼睛同步。
#[test]
fn startup_builds_chunks_and_grounds_player() {
    let mut harness = Harness::new(1);
    let mut meshes = 0;
    harness.app.world.query::<&MeshHandle>(|_| meshes += 1);
    assert_eq!(meshes, CHUNK_COUNT + 1, "64 个区块 + 目标框");

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

/// 跳跃峰值约 v²/2g ≈ 1.38 米，随后落回并恢复 grounded。
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
        "跳跃高度应接近 1.38 米，实际 {height}"
    );
    let player = harness.player();
    assert!(player.grounded, "90 帧后应落地");
    assert!((player.position.y - ground).abs() < 0.05);
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

    harness.press(KeyCode::Digit3);
    harness.click(MouseButton::Right);
    harness.frame();
    harness.release(KeyCode::Digit3);

    assert_eq!(
        harness.world().block(target),
        Block::Stone,
        "右键应在命中面相邻格放置选中的石头"
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

/// 快捷栏选择改变 HUD 标签。
#[test]
fn hotbar_selection_updates_hud_label() {
    let mut harness = Harness::new(8);
    harness.settle();
    assert_eq!(harness.hud_label(), "SELECTED: GRASS");
    harness.press(KeyCode::Digit3);
    harness.frame();
    harness.release(KeyCode::Digit3);
    assert_eq!(harness.hud_label(), "SELECTED: STONE");
}
