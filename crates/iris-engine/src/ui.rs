//! UI 编排（R41）：承载需要引擎门面层资源的系统——`update_ui_layout`
//! （读 `Viewport` 资源，缺席兜底 1280×720）与 `UiPlugin` 插件（把 UI
//! 组件、资源与三个系统按序注册进 `DefaultPlugins`）。
//!
//! # 分层理由
//!
//! `iris-render::ui` 提供组件、纯函数与不依赖 `Viewport` 的系统
//! （`update_ui_menus`、`update_ui_focus`——它们只读 `Input` 与组件）。锚点
//! 解析需要 `Viewport`，而 `Viewport` 是引擎门面层资源（`iris-render` 不能
//! 反向引用 `iris-engine`），故 `update_ui_layout` 放在本模块。
//!
//! # 执行序（渲染收集之前）
//!
//! 三系统均注册到 `Stage::PostUpdate`，而渲染发生在 PostUpdate 之后（`App`
//! 主循环 `surface.render`），故它们在渲染收集前执行——锚定后的位置与
//! 可见性对收集 `collect_ui_vertices` / `collect_ui_image_vertices` 本帧
//! 即生效。系统间顺序（相同阶段独占系统按注册序）：`update_ui_layout`
//! （写 `UiRect.position`）→ `update_ui_menus`（写 `Visibility`）→
//! `update_ui_focus`（按最终位置与可见性排序 / 换焦 / 高亮）。

use iris_core::entity::Entity;
use iris_core::prelude::{Stage, World};
use iris_render::ui::{
    UiAnchor, UiAnchorPoint, UiClickEvent, UiFocus, UiMenuStack, UiRect, anchor_to_position,
    update_ui_focus, update_ui_menus,
};

use crate::app::App;
use crate::plugin::{Plugin, Viewport};

/// 视口缺席时的兜底尺寸（与 `Viewport::default()` 一致）。
const DEFAULT_VIEWPORT: [f32; 2] = [1280.0, 720.0];

/// 解析全部挂 [`UiAnchor`] 实体的锚点 → 写 `UiRect.position`（每帧）。
///
/// 读 `Viewport` 资源（缺席 = 1280×720 兜底），**不拉伸不缩放**
/// `UiRect.size`（v1 无父容器嵌套——诚实限制，见 ARCHITECTURE §8）。
/// 只处理挂 `UiAnchor` 的实体：未挂者位置不动（手写绝对坐标既有行为零
/// 变化）。采两阶段（收集→应用）避免查询守卫存活期间对同组件取写锁。
pub fn update_ui_layout(world: &mut World) {
    let viewport = world
        .try_resource::<Viewport>()
        .map(|v| [v.width as f32, v.height as f32])
        .unwrap_or(DEFAULT_VIEWPORT);
    // 第一阶段（只读）：收集实体 + 锚点与矩形尺寸
    let mut targets: Vec<(Entity, UiAnchorPoint, [f32; 2], [f32; 2])> = Vec::new();
    world.query::<(Entity, &UiAnchor, &UiRect)>(|(entity, anchor, rect)| {
        targets.push((entity, anchor.point, rect.size, anchor.offset));
    });
    // 第二阶段（写回）：锚点解析为绝对像素写进 UiRect.position
    for (entity, point, size, offset) in targets {
        let position = anchor_to_position(point, viewport, size, offset);
        if let Some(mut rect) = world.get_mut::<UiRect>(entity) {
            rect.position = position;
        }
    }
}

/// UI 插件：注册 UI 资源（[`UiFocus`] / [`UiMenuStack`]）、
/// [`UiClickEvent`] 事件与锚定 / 菜单 / 焦点三系统（执行序见模块头）。
#[derive(Default)]
pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.world.insert_resource(UiFocus::default());
        app.world.insert_resource(UiMenuStack::default());
        app.world.register_event::<UiClickEvent>();
        app.schedule.add_system(Stage::PostUpdate, update_ui_layout);
        app.schedule.add_system(Stage::PostUpdate, update_ui_menus);
        app.schedule.add_system(Stage::PostUpdate, update_ui_focus);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_platform::input::Input;
    use iris_render::ui::{Focusable, UiButton, UiColor, UiMenu};
    use iris_render::visibility::Visibility;

    /// 锚定系统端到端：挂 UiAnchor 的实体被解析为正确绝对位置；未挂者
    /// 位置不动（零变化）；缺席 Viewport 兜底 1280×720。
    #[test]
    fn update_ui_layout_resolves_anchors_and_honors_viewport() {
        let mut world = World::new();
        let anchored = world.spawn((
            UiRect {
                position: [999.0, 999.0],
                size: [100.0, 40.0],
            },
            UiColor::default(),
            UiAnchor {
                point: UiAnchorPoint::Center,
                offset: [0.0, 0.0],
            },
        ));
        let fixed = world.spawn((
            UiRect {
                position: [12.0, 34.0],
                size: [8.0, 8.0],
            },
            UiColor::default(),
        ));

        // Viewport 缺席 → 兜底 1280×720
        update_ui_layout(&mut world);
        assert_eq!(
            world.get::<UiRect>(anchored).unwrap().position,
            [590.0, 340.0],
            "缺省视口 1280×720 下 Center 左上角应为 (590,340)"
        );
        assert_eq!(
            world.get::<UiRect>(fixed).unwrap().position,
            [12.0, 34.0],
            "未挂 UiAnchor 的实体位置不得被改写"
        );

        // 改 Viewport 尺寸 → 改跑一帧 → 元素中心恒定（不漂移）
        world.insert_resource(Viewport::new(800, 600));
        update_ui_layout(&mut world);
        let pos = world.get::<UiRect>(anchored).unwrap().position;
        assert_eq!(
            pos,
            [350.0, 280.0],
            "800×600 下 Center 左上角应为 (350,280)"
        );
    }

    /// 三系统端到端（菜单全景）：layout → menus → focus，focus 落在可见
    /// 菜单首位并被高亮；栈顶菜单实体可见。
    #[test]
    fn ui_systems_end_to_end_menu_focus_flow() {
        let mut world = World::new();
        world.insert_resource(Viewport::new(1280, 720));
        world.insert_resource(Input::new());
        world.insert_resource(UiFocus::default());
        let mut stack = UiMenuStack::default();
        stack.push_menu("pause");
        world.insert_resource(stack);

        let a = spawn_button(&mut world, -48.0);
        let b = spawn_button(&mut world, 0.0);
        let c = spawn_button(&mut world, 48.0);

        update_ui_layout(&mut world);
        update_ui_menus(&mut world);

        for btn in [a, b, c] {
            assert!(
                world.get::<Visibility>(btn).unwrap().visible,
                "栈顶菜单的按钮应可见"
            );
        }

        // 初始无焦点 → 按 ↓ → 落位首位（offset -48 的 a，Y 最小）
        world
            .resource_mut::<Input>()
            .press(iris_platform::prelude::KeyCode::ArrowDown);
        update_ui_focus(&mut world);
        assert_eq!(
            world.resource::<UiFocus>().current,
            Some(a),
            "Y 最小的按钮应获焦"
        );
        let hl = world.resource::<UiFocus>().highlight_color;
        assert_eq!(world.get::<UiColor>(a).unwrap().0, hl, "焦点按钮应高亮");
    }

    fn spawn_button(world: &mut World, offset: f32) -> Entity {
        world.spawn((
            UiRect {
                position: [0.0, 0.0],
                size: [120.0, 40.0],
            },
            UiColor([0.3, 0.3, 0.35, 1.0]),
            Focusable,
            UiButton {
                label: format!("btn{offset:?}"),
            },
            UiMenu("pause".into()),
            UiAnchor {
                point: UiAnchorPoint::Center,
                offset: [0.0, offset],
            },
        ))
    }
}
