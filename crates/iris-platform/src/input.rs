//! 输入状态：键盘 / 鼠标的帧内快照。
//!
//! 每帧由引擎主循环在渲染后调用 [`Input::end_frame`] 清除一次性状态
//! （按下 / 释放 / 鼠标位移 / 滚轮），「按住」状态跨帧保持。
//!
//! # 输入语义选择指南：电平（`key`）vs 边沿（`key_pressed`）
//!
//! 引擎同时提供两种查询语义，按玩法挑一边（打磨 IV 摩擦⑤的结论——
//! 语义本身合理，选择权在下游游戏）：
//!
//! - **边沿**（[`Input::key_pressed`] / [`Input::key_released`]，仅本帧
//!   为真）：适合「一次动作触发一次」的输入——菜单确认、开火、跳跃的
//!   传统单发实现。注意脚本 / 回放注入需要 release + press 两步才能
//!   造出边沿（`press` 对已按住的键不产生新的边沿）。
//! - **电平**（[`Input::key`]，按住期间恒真）：适合「状态持续成立」的
//!   输入——移动、加速，以及**带状态门控的持续动作**。典型：跳跃输入
//!   用电平 + grounded 门控（按住即「落地就跳」，自动连跳 / 蓄压手感
//!   友好，脚本注入一帧置位即可）——iris-ascent 的 `AscentInput::jump`
//!   即电平语义（logic 层 `grounded || coyote > 0` 门控决定何时真正
//!   起跳）；iris-game / iris-bastion 的移动意图同样读电平。
//!
//! 经验法则：**动作是否应该「按住就持续生效」**——是则电平（门控交给
//! 玩法状态，如 grounded / 冷却 / 弹匣），否则边沿。电平 + 门控还天然
//! 解耦了「输入采集」与「动作判定」：纯逻辑核心只收 `bool` 字段，不
//! 依赖边沿的帧序约定，headless 测试与回放注入更简单。
//!
//! # 第一人称鼠标捕获
//!
//! [`MouseCapture`] 是游戏侧发起、主循环结算的光标捕获请求。捕获生效
//! 期间指针被锁定或限制在窗口内并隐藏，[`Input`] 进入相对模式：
//! `mouse_delta` 只累积原始设备运动（winit `DeviceEvent::MouseMotion`），
//! 不再由光标位置差分产生；释放后首次光标事件只重建差分基准，不产生
//! 视角跳变。两个模式切换的入口仅由主循环调用
//! [`Input::set_relative`]，游戏系统读 [`Input::is_relative`] 即可区分
//! 位移口径。

use std::collections::HashSet;

use iris_core::Resource;
use winit::event::{ElementState, MouseButton};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::CursorGrabMode;

/// 键鼠输入状态（World 资源）。
#[derive(Default, Resource)]
pub struct Input {
    keys_held: HashSet<KeyCode>,
    keys_pressed: HashSet<KeyCode>,
    keys_released: HashSet<KeyCode>,
    mouse_held: HashSet<MouseButton>,
    mouse_pressed: HashSet<MouseButton>,
    mouse_released: HashSet<MouseButton>,
    mouse_delta: (f64, f64),
    wheel_delta: f32,
    cursor: Option<(f64, f64)>,
    last_cursor: Option<(f64, f64)>,
    /// 相对模式（捕获生效期间由主循环置位）：位移只来自原始设备运动，
    /// 光标位置差分停用。见模块文档「第一人称鼠标捕获」。
    relative: bool,
}

impl Input {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- 查询 ----

    /// 键是否处于按住状态。
    pub fn key(&self, code: KeyCode) -> bool {
        self.keys_held.contains(&code)
    }

    /// 键是否在本帧刚被按下（仅本帧为真）。
    pub fn key_pressed(&self, code: KeyCode) -> bool {
        self.keys_pressed.contains(&code)
    }

    /// 本帧是否有**任意键**刚被按下（「任意键开始」标题画面 / 跳过过场）。
    /// 语义等同对全键位遍历 [`Input::key_pressed`]——刚按下集合非空即真；
    /// 系统自动重复不重触发（与 `key_pressed` 的边沿语义一致）。
    pub fn any_key_pressed(&self) -> bool {
        !self.keys_pressed.is_empty()
    }

    /// 键是否在本帧刚被释放（仅本帧为真）。
    pub fn key_released(&self, code: KeyCode) -> bool {
        self.keys_released.contains(&code)
    }

    /// 鼠标按键是否按住。
    pub fn mouse_button(&self, button: MouseButton) -> bool {
        self.mouse_held.contains(&button)
    }

    /// 鼠标按键是否在本帧刚被按下。
    pub fn mouse_button_pressed(&self, button: MouseButton) -> bool {
        self.mouse_pressed.contains(&button)
    }

    /// 鼠标按键是否在本帧刚被释放。
    pub fn mouse_button_released(&self, button: MouseButton) -> bool {
        self.mouse_released.contains(&button)
    }

    /// 本帧累计鼠标位移。未捕获时是光标位置差分的物理像素；捕获时是
    /// 原始设备运动计数（单位依设备而定，游戏侧自行乘灵敏度）。
    pub fn mouse_delta(&self) -> (f64, f64) {
        self.mouse_delta
    }

    /// 是否处于相对鼠标模式（[`MouseCapture`] 捕获生效期间为真）。
    pub fn is_relative(&self) -> bool {
        self.relative
    }

    /// 本帧累计滚轮滚动量（像素模式为像素，行模式按每行 40 归一）。
    pub fn wheel_delta(&self) -> f32 {
        self.wheel_delta
    }

    /// 光标位置（物理像素），未知时为 `None`。
    pub fn cursor_position(&self) -> Option<(f64, f64)> {
        self.cursor
    }

    // ---- 事件注入（由平台事件循环调用） ----

    /// 切换相对鼠标模式（主循环在捕获状态变化时调用；游戏系统不应
    /// 直接调用）。切换瞬间清空本帧位移与光标差分基准：捕获时指针的
    /// 瞬移不产生视角跳变，释放后首次光标事件只重建基准。
    pub fn set_relative(&mut self, relative: bool) {
        if self.relative == relative {
            return;
        }
        self.relative = relative;
        self.mouse_delta = (0.0, 0.0);
        self.last_cursor = None;
    }

    /// 便捷注入：按下键。等价 `on_key(key.into(), ElementState::Pressed)`，
    /// 测试与工具脚本无须手构 winit 事件类型（`KeyCode` 可直接传入，
    /// 经 `Into<PhysicalKey>` 转换）。
    pub fn press(&mut self, key: impl Into<PhysicalKey>) {
        self.on_key(key.into(), ElementState::Pressed);
    }

    /// 便捷注入：释放键。等价 `on_key(key.into(), ElementState::Released)`。
    /// 「按住」状态跨帧保持，换向必须显式释放（脚本化驱动时勿忘）。
    pub fn release(&mut self, key: impl Into<PhysicalKey>) {
        self.on_key(key.into(), ElementState::Released);
    }

    pub fn on_key(&mut self, physical: PhysicalKey, state: ElementState) {
        let PhysicalKey::Code(code) = physical else {
            return;
        };
        match state {
            ElementState::Pressed => {
                if self.keys_held.insert(code) {
                    self.keys_pressed.insert(code);
                }
            }
            ElementState::Released => {
                self.keys_held.remove(&code);
                self.keys_released.insert(code);
            }
        }
    }

    pub fn on_mouse_button(&mut self, button: MouseButton, state: ElementState) {
        match state {
            ElementState::Pressed => {
                if self.mouse_held.insert(button) {
                    self.mouse_pressed.insert(button);
                }
            }
            ElementState::Released => {
                self.mouse_held.remove(&button);
                self.mouse_released.insert(button);
            }
        }
    }

    /// 注入原始设备位移（winit `DeviceEvent::MouseMotion`）。主循环只在
    /// 相对模式下行调用它；headless 测试可随时注入以模拟拖拽。单位依
    /// 设备而定，通常为鼠标计数而非物理像素。
    pub fn on_mouse_motion(&mut self, delta: (f64, f64)) {
        self.mouse_delta.0 += delta.0;
        self.mouse_delta.1 += delta.1;
    }

    pub fn on_wheel(&mut self, delta: f32) {
        self.wheel_delta += delta;
    }

    pub fn on_cursor_moved(&mut self, position: (f64, f64)) {
        // 未捕获：相对位移由光标位置差分得出（与上一事件位置比较）。
        // 捕获：同一段移动已由原始运动事件计入，差分必须停用否则翻倍。
        if !self.relative {
            if let Some(last) = self.last_cursor {
                self.mouse_delta.0 += position.0 - last.0;
                self.mouse_delta.1 += position.1 - last.1;
            }
        }
        self.last_cursor = Some(position);
        self.cursor = Some(position);
    }

    /// 帧末清除一次性状态。「按住」集合、光标位置、差分基准与相对模式
    /// 保留（相对模式由主循环随捕获状态显式切换）。
    pub fn end_frame(&mut self) {
        self.keys_pressed.clear();
        self.keys_released.clear();
        self.mouse_pressed.clear();
        self.mouse_released.clear();
        self.mouse_delta = (0.0, 0.0);
        self.wheel_delta = 0.0;
    }

    /// 窗口失焦时清空全部状态（防止按键卡死、位移跳变）。相对模式
    /// 保持不变——主循环在释放捕获时显式关闭它。
    pub fn clear_all(&mut self) {
        self.keys_held.clear();
        self.keys_pressed.clear();
        self.keys_released.clear();
        self.mouse_held.clear();
        self.mouse_pressed.clear();
        self.mouse_released.clear();
        self.mouse_delta = (0.0, 0.0);
        self.wheel_delta = 0.0;
        self.last_cursor = None;
    }
}

/// 第一人称鼠标捕获请求（World 资源，`InputPlugin` / `DefaultPlugins`
/// 自动插入）。
///
/// 游戏系统调用 [`MouseCapture::request`] 表达意图，主循环在本帧渲染后
/// 结算：按 `Locked` → `Confined` 顺序尝试光标抓取，成功则隐藏指针并
/// 把 [`Input`] 切到相对模式；两者都失败（平台不支持或浏览器拒绝指针
/// 锁定）时保持未生效状态，输入留在绝对模式，游戏可据
/// [`MouseCapture::is_active`] 自行降级。重复调用 `request` 只在目标
/// 状态变化时生效；捕获被外部原因中断后（典型是浏览器强制退出指针
/// 锁定）可用 [`MouseCapture::retry`] 重新尝试。
///
/// 窗口失焦时主循环释放抓取并恢复指针显示，焦点回来后若请求仍在则
/// 重新结算；因此游戏只需表达「我现在想捕获」，不必追踪焦点。
///
/// 捕获状态在帧末结算，当帧的系统写入要到下一帧才对输入生效（约一帧
/// 延迟，肉眼不可见）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Resource)]
pub struct MouseCapture {
    requested: bool,
    active: bool,
    mode: Option<CursorGrabMode>,
    generation: u64,
}

impl MouseCapture {
    pub fn new() -> Self {
        Self::default()
    }

    /// 请求或撤销捕获。目标状态与当前请求一致时不产生新代际
    /// （避免每帧重复抓取）；`true` 表示希望捕获，是否成功读
    /// [`MouseCapture::is_active`]。
    pub fn request(&mut self, capture: bool) {
        if self.requested != capture {
            self.requested = capture;
            self.generation = self.generation.wrapping_add(1);
        }
    }

    /// 强制重新结算当前请求：用于捕获被平台外部中断后（web 指针锁定
    /// 被浏览器退出）重新尝试，同样适用于切换全屏 / 窗口模式后。
    /// 未请求捕获时调用只会重新结算一次释放状态，不会进入捕获。
    pub fn retry(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// 是否希望捕获（主循环据此结算）。
    pub fn is_requested(&self) -> bool {
        self.requested
    }

    /// 捕获是否实际生效（光标已抓取且 [`Input`] 处于相对模式）。
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// 实际生效的抓取模式；未捕获或平台不支持时为 `None`。
    pub fn grab_mode(&self) -> Option<CursorGrabMode> {
        self.mode
    }

    /// 请求代际（主循环据此判断是否需要重新结算）。
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// 主循环写回实际生效状态（游戏系统不应调用）。`None` 表示释放或
    /// 抓取失败。
    pub fn set_applied(&mut self, mode: Option<CursorGrabMode>) {
        self.active = mode.is_some();
        self.mode = mode;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::PhysicalKey;

    fn key(code: KeyCode) -> PhysicalKey {
        PhysicalKey::Code(code)
    }

    #[test]
    fn press_hold_release_lifecycle() {
        let mut input = Input::new();
        input.on_key(key(KeyCode::KeyW), ElementState::Pressed);
        assert!(input.key_pressed(KeyCode::KeyW));
        assert!(input.key(KeyCode::KeyW), "按下的当帧同时处于按住状态");

        input.end_frame();
        assert!(!input.key_pressed(KeyCode::KeyW), "按下状态只保留一帧");
        assert!(input.key(KeyCode::KeyW), "按住状态跨帧保持");

        input.on_key(key(KeyCode::KeyW), ElementState::Released);
        assert!(input.key_released(KeyCode::KeyW));
        assert!(!input.key(KeyCode::KeyW));
        input.end_frame();
        assert!(!input.key_released(KeyCode::KeyW));
    }

    #[test]
    fn repeat_press_does_not_retrigger() {
        let mut input = Input::new();
        input.on_key(key(KeyCode::Space), ElementState::Pressed);
        input.end_frame();
        // 系统自动重复会再次派发 Pressed，不应视为「刚按下」
        input.on_key(key(KeyCode::Space), ElementState::Pressed);
        assert!(!input.key_pressed(KeyCode::Space));
        assert!(input.key(KeyCode::Space));
    }

    /// any-key 查询：本帧任意键刚按下为真；下一帧（end_frame 清除一次性
    /// 状态）为假；释放动作本身不产生按下边沿，仍为假。
    #[test]
    fn any_key_pressed_tracks_frame_edges() {
        let mut input = Input::new();
        assert!(!input.any_key_pressed(), "空闲帧不应有任意键按下");
        input.press(KeyCode::Enter);
        assert!(input.any_key_pressed(), "本帧刚按下应为真");
        // 同帧再多按一键：仍为真（集合语义）
        input.press(KeyCode::KeyX);
        assert!(input.any_key_pressed());

        input.end_frame();
        assert!(!input.any_key_pressed(), "下一帧按下边沿已清除");
        assert!(
            input.key(KeyCode::Enter),
            "按住状态跨帧保持（不参与 any-key）"
        );

        input.release(KeyCode::Enter);
        assert!(!input.any_key_pressed(), "释放不是按下：any-key 仍为假");
        assert!(input.key_released(KeyCode::Enter));
    }

    /// press / release 便捷注入：与 on_key 语义一致，但无须手构
    /// ElementState / PhysicalKey（KeyCode 经 Into<PhysicalKey> 直通）。
    #[test]
    fn press_and_release_drive_key_state() {
        let mut input = Input::new();
        input.press(KeyCode::KeyD);
        assert!(input.key(KeyCode::KeyD), "press 后应处于按住状态");
        assert!(input.key_pressed(KeyCode::KeyD), "press 当帧视为刚按下");

        input.end_frame();
        assert!(input.key(KeyCode::KeyD), "按住状态跨帧保持");
        assert!(!input.key_pressed(KeyCode::KeyD));

        // 换向必须显式释放（按住状态不会自行消失）
        input.release(KeyCode::KeyD);
        assert!(!input.key(KeyCode::KeyD), "release 后不应再按住");
        assert!(input.key_released(KeyCode::KeyD));

        // 也接受显式 PhysicalKey（与 on_key 的参数形态互通）
        input.press(PhysicalKey::Code(KeyCode::KeyA));
        assert!(input.key(KeyCode::KeyA));
    }

    #[test]
    fn mouse_motion_accumulates_within_frame() {
        let mut input = Input::new();
        // 首个事件只建立差分基准，不产生位移
        input.on_cursor_moved((100.0, 100.0));
        input.on_cursor_moved((101.5, 98.0));
        input.on_cursor_moved((102.0, 99.0));
        assert_eq!(input.mouse_delta(), (2.0, -1.0));
        input.end_frame();
        assert_eq!(input.mouse_delta(), (0.0, 0.0));
        // 帧间连续：下一帧从上一事件位置继续差分
        input.on_cursor_moved((103.0, 99.0));
        assert_eq!(input.mouse_delta(), (1.0, 0.0));
    }

    #[test]
    fn focus_loss_clears_everything() {
        let mut input = Input::new();
        input.on_key(key(KeyCode::KeyA), ElementState::Pressed);
        input.on_mouse_button(MouseButton::Left, ElementState::Pressed);
        input.on_cursor_moved((50.0, 50.0));
        input.clear_all();
        assert!(!input.key(KeyCode::KeyA));
        assert!(!input.mouse_button(MouseButton::Left));
        // 失焦后重新移动鼠标：从新位置重新建立基准，不产生跳变
        input.on_cursor_moved((200.0, 200.0));
        assert_eq!(input.mouse_delta(), (0.0, 0.0));
    }

    /// 相对模式：光标位置照常记录，但不折算位移；位移只来自原始运动
    /// 事件，避免同一段移动被差分与原始输入计两次。
    #[test]
    fn relative_mode_uses_raw_motion_not_cursor_deltas() {
        let mut input = Input::new();
        input.set_relative(true);
        input.on_cursor_moved((100.0, 100.0));
        input.on_cursor_moved((160.0, 130.0));
        assert_eq!(
            input.mouse_delta(),
            (0.0, 0.0),
            "捕获期间光标位置不再折算位移"
        );
        assert_eq!(
            input.cursor_position(),
            Some((160.0, 130.0)),
            "光标位置照常记录"
        );

        input.on_mouse_motion((6.0, -4.0));
        input.on_mouse_motion((-2.0, 1.0));
        assert_eq!(input.mouse_delta(), (4.0, -3.0), "原始运动逐事件累加");
        input.end_frame();
        assert_eq!(input.mouse_delta(), (0.0, 0.0));
    }

    /// 退出相对模式：本帧待消费位移清空，光标差分基准重建
    /// （释放捕获后指针的瞬移不产生视角跳变）。
    #[test]
    fn leaving_relative_mode_rebuilds_cursor_baseline() {
        let mut input = Input::new();
        input.set_relative(true);
        input.on_cursor_moved((100.0, 100.0));
        input.on_mouse_motion((8.0, 2.0));
        input.set_relative(false);
        assert_eq!(input.mouse_delta(), (0.0, 0.0), "模式切换清空待消费位移");

        input.on_cursor_moved((400.0, 400.0));
        assert_eq!(input.mouse_delta(), (0.0, 0.0), "首次光标事件只重建基准");
        input.on_cursor_moved((410.0, 404.0));
        assert_eq!(input.mouse_delta(), (10.0, 4.0), "之后恢复光标差分");
    }

    /// 进入相对模式：未消费的光标差分与旧基准一并清空，捕获瞬间的
    /// 指针归位不计入视角。
    #[test]
    fn entering_relative_mode_clears_pending_delta() {
        let mut input = Input::new();
        input.on_cursor_moved((100.0, 100.0));
        input.on_cursor_moved((110.0, 105.0));
        assert_eq!(input.mouse_delta(), (10.0, 5.0));

        input.set_relative(true);
        assert_eq!(input.mouse_delta(), (0.0, 0.0));
        input.on_cursor_moved((900.0, 900.0));
        assert_eq!(input.mouse_delta(), (0.0, 0.0), "捕获后光标事件不产生位移");
        input.on_mouse_motion((3.0, 3.0));
        assert_eq!(input.mouse_delta(), (3.0, 3.0));
    }

    /// 失焦清空不改变相对模式：主循环先释放捕获再清空输入，顺序由
    /// App 负责；Input 自身不擅自退出捕获口径。
    #[test]
    fn focus_clear_keeps_relative_mode() {
        let mut input = Input::new();
        input.set_relative(true);
        input.on_mouse_motion((5.0, 5.0));
        input.clear_all();
        assert!(input.is_relative());
        assert_eq!(input.mouse_delta(), (0.0, 0.0));
    }

    /// 捕获请求的代际语义：目标状态变化才换代；重复请求幂等；
    /// `retry` 强制换代以重新结算；`set_applied` 只写生效态。
    #[test]
    fn mouse_capture_request_lifecycle() {
        let mut capture = MouseCapture::new();
        assert!(!capture.is_requested());
        assert!(!capture.is_active());
        assert_eq!(capture.grab_mode(), None);
        let base = capture.generation();

        capture.request(true);
        assert!(capture.is_requested());
        assert!(!capture.is_active(), "请求不等于生效");
        assert_eq!(capture.generation(), base + 1);

        capture.request(true);
        assert_eq!(capture.generation(), base + 1, "重复请求不换代");

        capture.retry();
        assert_eq!(capture.generation(), base + 2, "retry 强制重新结算");

        capture.set_applied(Some(CursorGrabMode::Locked));
        assert!(capture.is_active());
        assert_eq!(capture.grab_mode(), Some(CursorGrabMode::Locked));

        capture.set_applied(None);
        assert!(!capture.is_active(), "抓取失败或释放后不生效");
        assert_eq!(capture.grab_mode(), None);

        capture.request(false);
        assert!(!capture.is_requested());
        assert_eq!(capture.generation(), base + 3);
    }
}
