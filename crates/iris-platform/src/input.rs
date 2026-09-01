//! 输入状态：键盘 / 鼠标的帧内快照。
//!
//! 每帧由引擎主循环在渲染后调用 [`Input::end_frame`] 清除一次性状态
//! （按下 / 释放 / 鼠标位移 / 滚轮），「按住」状态跨帧保持。

use std::collections::HashSet;

use iris_core::Resource;
use winit::event::{ElementState, MouseButton};
use winit::keyboard::{KeyCode, PhysicalKey};

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

    /// 本帧累计鼠标位移（物理像素）。
    pub fn mouse_delta(&self) -> (f64, f64) {
        self.mouse_delta
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

    pub fn on_mouse_motion(&mut self, delta: (f64, f64)) {
        self.mouse_delta.0 += delta.0;
        self.mouse_delta.1 += delta.1;
    }

    pub fn on_wheel(&mut self, delta: f32) {
        self.wheel_delta += delta;
    }

    pub fn on_cursor_moved(&mut self, position: (f64, f64)) {
        // 相对位移由光标位置差分得出（与上一事件位置比较）
        if let Some(last) = self.last_cursor {
            self.mouse_delta.0 += position.0 - last.0;
            self.mouse_delta.1 += position.1 - last.1;
        }
        self.last_cursor = Some(position);
        self.cursor = Some(position);
    }

    /// 帧末清除一次性状态。「按住」集合、光标位置与差分基准保留。
    pub fn end_frame(&mut self) {
        self.keys_pressed.clear();
        self.keys_released.clear();
        self.mouse_pressed.clear();
        self.mouse_released.clear();
        self.mouse_delta = (0.0, 0.0);
        self.wheel_delta = 0.0;
    }

    /// 窗口失焦时清空全部状态（防止按键卡死、位移跳变）。
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
        assert!(input.key(KeyCode::Enter), "按住状态跨帧保持（不参与 any-key）");

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
}
