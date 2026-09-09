//! # iris-editor
//!
//! Iris **关卡摆放编辑器**（R58a 最小编辑器 MVP → R58b undo / 光源编辑 /
//! 烘焙入口 / 导航可视化 / 视角旋转）：W5 内容产能的核心工具。数据层是
//! R57 的关卡档 v2（[`iris_engine::scene`] / [`iris_engine::level`]）
//! ——编辑器不发明新格式，它只是那份格式的一个**写者**。
//!
//! # 分层（架构硬要求：命令层与交互壳分离）
//!
//! - [`edit`]：**编辑命令层**。编辑器状态（选择 / 摆放类型 / 吸附开关 /
//!   undo 栈）+ 全部编辑操作（摆放 / 拾取 / 拖拽 / 删除 / 复制 / 微移 /
//!   缩放 / 光源参数 / 烘焙 / 导航 / 相机轨道）+ 瞬态视觉（地面参考网格、
//!   选中框、Nav 可视化）+ 只读属性面板文本 + 关卡档存取。全部是
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
//! # b 阶段增量（R58b，在 a 阶段之上）
//!
//! 做的：**undo / redo**（快照栈，深度 32，dump 逐字符一致测试钉住）、
//! **光源参数编辑**（点光四键 / 方向光五键，选中态定模式，位模式精确）、
//! **烘焙入口 B**（档内 `level.bake` 参数 → bake → 产物落盘 → 挂载 →
//! 登记表回补）、**NavGrid 可视化 N**（档内参数或静态 AABB 并集自动
//! bounds、blocked 红片 + 域边框）、**视角旋转**（中键轨道 + F 聚焦）。
//!
//! **拒绝清单**（想做任何一项都先停下写偏离汇报）：多选 / 框选、GLB 摆放
//! 与资产浏览器、地形雕刻、材质编辑器、文本输入框、快捷键自定义、主题、
//! **bake / nav 参数的数值编辑面板**（档内参数只读使用——参数编辑需要
//! `LevelRegistry` 快照语义升级为现读 + 回写，属 §8.33② 语义变更，单独
//! 一轮评审）、精确三角形拾取、动画预览。完整诚实清单见 [`edit`] 模块
//! 文档尾部（12 条）。
//!
//! # 平台
//!
//! 原生为主（编辑器是工具，不是运行时内容）；crate 内无原生专属 API，
//! wasm32 编译目标检查照绿（文件读写与 CLI 参数在 web 运行期不可用，与
//! 引擎资产管线的平台约定一致）。

pub mod edit;
pub mod shell;

pub use edit::{
    BakeReport, DEFAULT_LEVEL_ENTRY, DEFAULT_SNAP, EditorState, EditorTransient, EntityInfo,
    GIZMO_EDGE_COUNT, GRID_EXTENT, GRID_STEP, HUD_NAME, HUE_STEP_DEG, LIGHT_BULB,
    LIGHT_INTENSITY_MIN, LIGHT_INTENSITY_STEP, LIGHT_RANGE_MIN, LIGHT_RANGE_STEP, LightEditMode,
    MATERIAL_PRESETS, MaterialPreset, NavReport, ORBIT_PITCH_LIMIT, ORBIT_SENSITIVITY, OpenReport,
    PlaceKind, SUN_AMBIENT_STEP, SUN_DIR_STEP, SUN_INTENSITY_STEP, SUN_PITCH_LIMIT, UNDO_LIMIT,
    ascii_only, bake_and_install, begin_drag, clear_transient, collider_for, cycle_kind,
    cycle_preset, delete_selected, describe, drag_to, duplicate_selected, editable_entities,
    end_drag, focus_selected, gizmo_edge_transforms, hsv_to_rgb, light_edit_mode, light_hue_step,
    light_intensity_step, light_range_step, light_shadow_toggle, local_aabb_to_world,
    nav_blocked_count, nav_bounds_from_statics, nudge_selected, open_level, orbit_camera,
    orbit_eye, panel_text, pick, pick_with_hit, place_at, place_from_ray, place_light_at,
    place_light_from_ray, placement_point, preset_name, rebuild_gizmo, rebuild_grid, rebuild_nav,
    rebuild_nav_viz, redo, redo_depth, refresh_hud, refresh_visuals, rgb_to_hsv, rotate_hue,
    rotate_light_direction, save_level, scale_selected, screen_ray_at, select, snap_value,
    spawn_hud, sun_ambient_step, sun_direction_step, toggle_collider, toggle_nav_viz, toggle_snap,
    undo, undo_depth, update_gizmo, world_aabb_of,
};
pub use shell::{EditorPlugin, FLY_BOOST, FLY_SPEED, ZOOM_FACTOR, fly_camera, open_editor};
