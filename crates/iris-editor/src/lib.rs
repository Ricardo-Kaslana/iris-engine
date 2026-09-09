//! # iris-editor
//!
//! Iris **关卡摆放编辑器**（R58a 最小编辑器 MVP）：W5 内容产能的核心工具。
//! 数据层是 R57 的关卡档 v2（[`iris_engine::scene`] / [`iris_engine::level`]）
//! ——编辑器不发明新格式，它只是那份格式的一个**写者**。
//!
//! # 分层（架构硬要求：命令层与交互壳分离）
//!
//! - [`edit`]：**编辑命令层**。编辑器状态（选择 / 摆放类型 / 吸附开关）+
//!   全部编辑操作（摆放 / 拾取 / 拖拽 / 删除 / 复制 / 微移 / 缩放）+ 瞬态
//!   视觉（地面参考网格、选中框）+ 只读属性面板文本 + 关卡档存取。全部是
//!   `World + EditorState + 事件参数 → 确定性结果` 的自由函数，**不含任何
//!   窗口 / 输入 / 渲染代码**，headless 直接单测（`tests/edit_headless.rs`
//!   = 验收①的载体）。
//! - [`shell`]：**交互壳**。winit 窗口（经引擎 `DefaultPlugins`）+ 飞行相机 +
//!   输入翻译（鼠标像素 → 射线、键位 → 命令层调用）。刻意薄；键位表在
//!   该模块文档里。
//!
//! # 运行
//!
//! ```sh
//! cargo run -p iris-editor --release                       # 缺省档 assets/editor_mvp.level.json
//! cargo run -p iris-editor --release -- 我的关卡.level.json  # CLI 指定档
//! ```
//!
//! 档不存在 = 从空世界新建（`Ctrl+S` 落盘）；档存在但解析失败 = **panic**
//! 而非降级为空世界（避免随后一次保存覆盖原档，见 [`shell::open_editor`]）。
//!
//! # a 阶段边界（= 验收标准）
//!
//! 做的：视口（飞行相机 + 地面参考网格）、图元 / 点光摆放、射线拾取、
//! 地面拖拽 + 网格吸附、删除 / 复制、只读属性面板 + 热键数值修改、关卡档
//! 存取。
//!
//! **拒绝清单**（想做任何一项都先停下写偏离汇报）：多选 / 框选、undo /
//! redo、光源参数编辑面板与方向光编辑、烘焙入口 / 导航可视化、GLB 摆放与
//! 资产浏览器、地形雕刻、材质编辑器、文本输入框、快捷键自定义、主题样式
//! ——全部 b 阶段及以后。完整诚实清单见 [`edit`] 模块文档尾部（9 条）。
//!
//! # 平台
//!
//! 原生为主（编辑器是工具，不是运行时内容）；crate 内无原生专属 API，
//! wasm32 编译目标检查照绿（文件读写与 CLI 参数在 web 运行期不可用，与
//! 引擎资产管线的平台约定一致）。

pub mod edit;
pub mod shell;

pub use edit::{
    DEFAULT_LEVEL_ENTRY, DEFAULT_SNAP, EditorState, EditorTransient, EntityInfo, GIZMO_EDGE_COUNT,
    GRID_EXTENT, GRID_STEP, HUD_NAME, LIGHT_BULB, MATERIAL_PRESETS, MaterialPreset, OpenReport,
    PlaceKind, ascii_only, begin_drag, clear_transient, collider_for, cycle_kind, cycle_preset,
    delete_selected, describe, drag_to, duplicate_selected, editable_entities, end_drag,
    gizmo_edge_transforms, local_aabb_to_world, nudge_selected, open_level, panel_text, pick,
    pick_with_hit, place_at, place_from_ray, place_light_at, place_light_from_ray, placement_point,
    preset_name, rebuild_gizmo, rebuild_grid, refresh_hud, refresh_visuals, save_level,
    scale_selected, screen_ray_at, select, snap_value, spawn_hud, toggle_collider, toggle_snap,
    update_gizmo, world_aabb_of,
};
pub use shell::{EditorPlugin, FLY_BOOST, FLY_SPEED, ZOOM_FACTOR, fly_camera, open_editor};
