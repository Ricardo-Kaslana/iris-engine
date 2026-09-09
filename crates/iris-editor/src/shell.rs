//! 交互壳（R58a）：窗口 / 输入 / 相机 → 编辑命令层。刻意做**薄**——本模块
//! 不含任何编辑语义，只把设备事件换算成 [`crate::edit`] 的参数。
//!
//! # 键位表（a 阶段固定，无自定义——自定义在拒绝清单内）
//!
//! | 输入 | 命令 |
//! |---|---|
//! | 鼠标左键单击 | 拾取并选中（未命中 = 清空选择） |
//! | 鼠标左键按住拖动 | 拖拽选中实体（射线 ∩ 命中点高度平面，x / z 吸附） |
//! | 鼠标右键单击 | 在命中点摆放当前图元 |
//! | `P` | 同右键（键盘等价，光标位置照样生效） |
//! | `L` | 在命中点摆放点光 |
//! | `Tab` | 循环切换图元（cube → plane → sphere → cylinder） |
//! | `R` | 循环切换材质预设 |
//! | `C` | 开关「摆放时挂静态碰撞体」 |
//! | `G` | 开关网格吸附（0.5 m） |
//! | `Delete` | 删除选中（级联子树） |
//! | `Ctrl+D` | 复制选中（原地，名字加 `_copy{n}` 后缀） |
//! | `Ctrl+S` | 保存关卡档 |
//! | 方向键 | 微移选中 0.1 m（左/右 = ∓X，上/下 = ∓Z）；`Alt`+上/下 = ±Y；`Shift` = 十倍 |
//! | `PgUp` / `PgDn` | 缩放选中 ×1.1 / ÷1.1 |
//! | `W` `A` `S` `D` | 飞行相机水平移动（相机偏航系） |
//! | `Q` / `E` | 飞行相机降 / 升 |
//! | `Shift`（配合 WASD / QE） | 飞行加速 ×3 |
//! | 滚轮 | 变焦（视场角） |
//! | `Esc` | 退出 |
//!
//! `Ctrl` 按住时相机不移动（避免 `Ctrl+S` / `Ctrl+D` 期间镜头漂移）。
//!
//! # 相机
//!
//! 用 [`Camera`] **资源**（不挂 `ActiveCamera` 实体——那会把相机变成有
//! `Transform` 的实体、进而被 `save_scene` 写进档里，见 [`crate::edit`]
//! 模块文档诚实清单第 8 条）。渲染器在无 `ActiveCamera` 实体时回退
//! `Camera` 资源。a 阶段**无视角旋转**（规格给的飞行相机 = 平移 + 变焦，
//! 朝向固定为 [`crate::edit::open_level`] 写入的默认斜俯视角）。
//!
//! # 平台
//!
//! 无原生专属 API：输入 / 窗口 / 时间全部经引擎资源（[`Input`] /
//! [`Viewport`] / [`Time`]）；`std::env::args` 与 `std::fs` 在 wasm32 上
//! 编译通过、运行期不可用（与引擎资产管线的平台约定一致）——wasm32
//! clippy 门禁照绿，编辑器本体是原生工具。资源缺席时各系统 no-op（不
//! panic），于是 headless / 离屏路径可以只注册本插件的调度而不建窗口。

use std::path::{Path, PathBuf};

use glam::Vec3;
use iris_engine::prelude::*;

use crate::edit::{self, EditorState, NUDGE_STEP, SCALE_STEP};

/// 飞行相机速度（米 / 秒，未加速时）。
pub const FLY_SPEED: f32 = 6.0;
/// `Shift` 加速倍率。
pub const FLY_BOOST: f32 = 3.0;
/// 滚轮 → 视场角增量系数（与引擎示例 `cube.rs` 同值）。
pub const ZOOM_FACTOR: f32 = -0.0015;

/// 编辑器插件：Startup 开关卡 + Update 三个系统（相机 → 输入 → 退出）。
///
/// 系统编排经 [`EditorPlugin::build_schedule`] 暴露，供 headless 测试与
/// 离屏快照在不建窗口 / GPU 的前提下复用同一条推进路径（六个 dogfooding
/// 游戏同款范式）。**UI / 层级 / 渲染等不在此注册**：窗口路径由
/// `DefaultPlugins` 提供；离屏路径由调用方按需补 `UiPlugin`（属性面板
/// 锚定）与 `HierarchyPlugin`（全局变换传播）。
#[derive(Clone, Debug)]
pub struct EditorPlugin {
    /// 关卡档路径（CLI 参数或缺省 [`edit::DEFAULT_LEVEL_ENTRY`]）。
    pub level_path: PathBuf,
}

impl Default for EditorPlugin {
    fn default() -> Self {
        Self {
            level_path: PathBuf::from(edit::DEFAULT_LEVEL_ENTRY),
        }
    }
}

impl EditorPlugin {
    /// 从 CLI 参数构造：第一个位置参数 = 关卡档路径，缺席 = 缺省路径。
    /// 相对路径经 [`asset_path`] 解析（`cargo run` 与 `cargo test` 的 cwd
    /// 不同，见 iris-engine::assets 文档）。
    pub fn from_args() -> Self {
        let path = std::env::args()
            .nth(1)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(edit::DEFAULT_LEVEL_ENTRY));
        Self {
            level_path: asset_path(path),
        }
    }

    /// 注册编辑器的全部系统（Startup 一次 + Update 每帧三个）。
    pub fn build_schedule(&self, schedule: &mut Schedule) {
        let level_path = self.level_path.clone();
        schedule
            .add_system(Stage::Startup, move |world: &mut World| {
                open_editor(world, &level_path);
            })
            .add_system(Stage::Update, fly_camera)
            .add_system(Stage::Update, edit_input)
            .add_system(Stage::Update, quit_on_escape);
    }
}

impl Plugin for EditorPlugin {
    fn build(&self, app: &mut App) {
        self.build_schedule(&mut app.schedule);
    }
}

/// Startup：打开关卡档。
///
/// 档**存在但加载失败**时 panic（而非降级为空世界）：编辑器随后一次
/// `Ctrl+S` 就会把原档覆盖掉，静默降级等于给用户一个数据丢失陷阱。
/// 档不存在是正常路径（从空世界开始新建）。与 iris-assault 的关卡档
/// 加载同款处置。
pub fn open_editor(world: &mut World, level_path: &Path) {
    edit::open_level(world, level_path).unwrap_or_else(|err| {
        panic!(
            "关卡档 {} 加载失败：{err}（编辑器不降级为空世界——避免随后一次保存覆盖原档）",
            level_path.display()
        )
    });
}

/// Update 1：飞行相机（WASD 平移在相机偏航系、Q / E 升降、滚轮变焦）。
///
/// `Ctrl` 按住时不平移（`Ctrl+S` / `Ctrl+D` 期间镜头不漂）；`Input` /
/// `Time` / `Camera` 任一缺席则 no-op（headless 复用不 panic）。
pub fn fly_camera(world: &mut World) {
    let (move_vec, wheel) = {
        let (Some(input), Some(time), Some(camera)) = (
            world.try_resource::<Input>(),
            world.try_resource::<Time>(),
            world.try_resource::<Camera>(),
        ) else {
            return;
        };
        let wheel = input.wheel_delta();
        let ctrl = input.key(KeyCode::ControlLeft) || input.key(KeyCode::ControlRight);
        if ctrl {
            (Vec3::ZERO, wheel)
        } else {
            let mut wish = Vec3::ZERO;
            if input.key(KeyCode::KeyW) {
                wish.z += 1.0;
            }
            if input.key(KeyCode::KeyS) {
                wish.z -= 1.0;
            }
            if input.key(KeyCode::KeyD) {
                wish.x += 1.0;
            }
            if input.key(KeyCode::KeyA) {
                wish.x -= 1.0;
            }
            if input.key(KeyCode::KeyE) {
                wish.y += 1.0;
            }
            if input.key(KeyCode::KeyQ) {
                wish.y -= 1.0;
            }
            let boost = if input.key(KeyCode::ShiftLeft) || input.key(KeyCode::ShiftRight) {
                FLY_BOOST
            } else {
                1.0
            };
            // 水平移动在相机偏航系（朝向的 XZ 投影），升降恒沿世界 Y
            let view = camera.target - camera.eye;
            let forward = Vec3::new(view.x, 0.0, view.z).normalize_or_zero();
            let right = forward.cross(Vec3::Y);
            let delta = right * wish.x + Vec3::Y * wish.y + forward * wish.z;
            (
                delta.normalize_or_zero() * FLY_SPEED * boost * time.delta_secs(),
                wheel,
            )
        }
    };
    let mut camera = world.resource_mut::<Camera>();
    camera.fly(move_vec);
    camera.zoom(wheel * ZOOM_FACTOR);
}

/// 一帧的输入事实（只读快照）：把「读设备」与「改世界」分成两阶段，
/// 避免 `Input` 守卫与命令层的 `&mut World` 借用打架。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct FrameInput {
    /// 光标像素（物理像素，原点左上；`None` = 本帧无光标位置）。
    cursor: Option<(f64, f64)>,
    left_pressed: bool,
    left_held: bool,
    left_released: bool,
    right_pressed: bool,
    save: bool,
    duplicate: bool,
    delete: bool,
    cycle_kind: bool,
    cycle_preset: bool,
    toggle_collider: bool,
    toggle_snap: bool,
    place: bool,
    place_light: bool,
    /// 微移（单位轴 × 带符号距离，已含 Shift 十倍）。
    nudge: Option<(Vec3, f32)>,
    /// 缩放倍率（PgUp / PgDn）。
    scale: Option<f32>,
}

/// Update 2：输入翻译 → 命令层。编辑器状态缺席（Startup 未跑）则 no-op。
pub fn edit_input(world: &mut World) {
    if world.try_resource::<EditorState>().is_none() {
        return;
    }
    let frame = read_input(world);
    apply_input(world, &frame);
    edit::refresh_hud(world);
}

/// 读一帧的输入事实（纯读，不改世界）。
fn read_input(world: &World) -> FrameInput {
    let Some(input) = world.try_resource::<Input>() else {
        return FrameInput::default();
    };
    let ctrl = input.key(KeyCode::ControlLeft) || input.key(KeyCode::ControlRight);
    let shift = input.key(KeyCode::ShiftLeft) || input.key(KeyCode::ShiftRight);
    let alt = input.key(KeyCode::AltLeft) || input.key(KeyCode::AltRight);
    let step = if shift { NUDGE_STEP * 10.0 } else { NUDGE_STEP };
    // 方向键优先级固定（左 → 右 → 上 → 下）：同帧多键按下时结果确定
    let nudge = if input.key_pressed(KeyCode::ArrowLeft) {
        Some((Vec3::X, -step))
    } else if input.key_pressed(KeyCode::ArrowRight) {
        Some((Vec3::X, step))
    } else if input.key_pressed(KeyCode::ArrowUp) {
        if alt {
            Some((Vec3::Y, step))
        } else {
            Some((Vec3::Z, -step))
        }
    } else if input.key_pressed(KeyCode::ArrowDown) {
        if alt {
            Some((Vec3::Y, -step))
        } else {
            Some((Vec3::Z, step))
        }
    } else {
        None
    };
    let scale = if input.key_pressed(KeyCode::PageUp) {
        Some(SCALE_STEP)
    } else if input.key_pressed(KeyCode::PageDown) {
        Some(1.0 / SCALE_STEP)
    } else {
        None
    };
    FrameInput {
        cursor: input.cursor_position(),
        left_pressed: input.mouse_button_pressed(MouseButton::Left),
        left_held: input.mouse_button(MouseButton::Left),
        left_released: input.mouse_button_released(MouseButton::Left),
        right_pressed: input.mouse_button_pressed(MouseButton::Right),
        save: ctrl && input.key_pressed(KeyCode::KeyS),
        duplicate: ctrl && input.key_pressed(KeyCode::KeyD),
        delete: input.key_pressed(KeyCode::Delete),
        cycle_kind: input.key_pressed(KeyCode::Tab),
        cycle_preset: input.key_pressed(KeyCode::KeyR),
        toggle_collider: input.key_pressed(KeyCode::KeyC),
        toggle_snap: input.key_pressed(KeyCode::KeyG),
        place: input.key_pressed(KeyCode::KeyP),
        place_light: input.key_pressed(KeyCode::KeyL),
        nudge,
        scale,
    }
}

/// 把一帧的输入事实翻译成命令层调用（顺序即优先级：存取 → 删除 / 复制 →
/// 摆放 → 拾取 / 拖拽 → 数值微调 → 参数切换）。
fn apply_input(world: &mut World, frame: &FrameInput) {
    if frame.save {
        // 错误已由命令层双通道报告（log::error + 面板状态行），壳不重复处理
        let _ = edit::save_level(world);
    }
    if frame.duplicate {
        edit::duplicate_selected(world);
    }
    if frame.delete {
        edit::delete_selected(world);
    }
    // 摆放与拾取都需要光标射线；无光标位置（无窗口 / 未进视口）则跳过
    if let Some((px, py)) = frame.cursor {
        if let Some((origin, dir)) = edit::screen_ray_at(world, px, py) {
            if frame.right_pressed || frame.place {
                edit::place_from_ray(world, origin, dir);
            }
            if frame.place_light {
                edit::place_light_from_ray(world, origin, dir);
            }
            if frame.left_pressed && edit::begin_drag(world, origin, dir).is_none() {
                // 点空白处 = 清空选择（拾取未命中）
                edit::select(world, None);
            }
            if frame.left_held {
                edit::drag_to(world, origin, dir);
            }
        }
    }
    if frame.left_released {
        edit::end_drag(world);
    }
    if let Some((axis, amount)) = frame.nudge {
        edit::nudge_selected(world, axis, amount);
    }
    if let Some(factor) = frame.scale {
        edit::scale_selected(world, factor);
    }
    if frame.cycle_kind {
        edit::cycle_kind(world);
    }
    if frame.cycle_preset {
        edit::cycle_preset(world);
    }
    if frame.toggle_collider {
        edit::toggle_collider(world);
    }
    if frame.toggle_snap {
        edit::toggle_snap(world);
    }
}

/// Update 3：`Esc` 退出（引擎事件，窗口层消费）。
pub fn quit_on_escape(world: &mut World) {
    if world
        .try_resource::<Input>()
        .is_some_and(|input| input.key_pressed(KeyCode::Escape))
    {
        world.send_event(AppExit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 无输入 / 无相机资源时两个系统都 no-op（headless 复用不 panic）。
    #[test]
    fn systems_noop_without_resources() {
        let mut world = World::new();
        fly_camera(&mut world);
        edit_input(&mut world);
        quit_on_escape(&mut world);
        assert!(
            world.try_resource::<EditorState>().is_none(),
            "壳不创建编辑器状态（那是 open_level 的职责）"
        );
    }

    /// 相机平移在偏航系：默认斜俯视角下按 W 应沿朝向的 XZ 投影前进
    /// （y 不变），按 E 应纯升。数值对照 = 手工解算（1e-5 容差，
    /// 归一化引入的浮点误差）。
    #[test]
    fn fly_camera_moves_in_yaw_frame() {
        let mut world = World::new();
        world.insert_resource(Time::default());
        world.insert_resource(Input::new());
        world.insert_resource(Camera {
            eye: Vec3::new(9.0, 9.5, 12.0),
            target: Vec3::new(0.0, 0.6, 0.0),
            up: Vec3::Y,
            ..Camera::default()
        });
        world
            .resource_mut::<Time>()
            .advance(std::time::Duration::from_secs_f64(0.25));
        world.resource_mut::<Input>().press(KeyCode::KeyE);
        fly_camera(&mut world);
        let camera = world.resource::<Camera>();
        assert!(
            (camera.eye.y - 11.0).abs() < 1e-5,
            "E 升：9.5 + {} m/s × 0.25 s（= Time 的 max_delta 钳制上限）= 11.0，实际 {}",
            FLY_SPEED,
            camera.eye.y
        );
        assert!((camera.eye.x - 9.0).abs() < 1e-5, "升降不动水平分量");
    }

    /// `Ctrl` 按住时不平移（保存 / 复制期间镜头不漂）。
    #[test]
    fn fly_camera_freezes_while_ctrl_held() {
        let mut world = World::new();
        world.insert_resource(Time::default());
        world.insert_resource(Input::new());
        world.insert_resource(Camera::default());
        world
            .resource_mut::<Time>()
            .advance(std::time::Duration::from_secs_f64(1.0));
        {
            let mut input = world.resource_mut::<Input>();
            input.press(KeyCode::ControlLeft);
            input.press(KeyCode::KeyW);
        }
        let before = world.resource::<Camera>().eye;
        fly_camera(&mut world);
        assert_eq!(world.resource::<Camera>().eye, before, "Ctrl 按住不动");
    }

    /// 缺省插件路径 = 缺省关卡档（CLI 未给参数时）。
    #[test]
    fn default_plugin_uses_default_entry() {
        assert_eq!(
            EditorPlugin::default().level_path,
            PathBuf::from(edit::DEFAULT_LEVEL_ENTRY)
        );
    }
}
