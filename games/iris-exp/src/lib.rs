//! # iris-exp
//!
//! Iris 引擎**实验性项目**：外部 GLB 角色（Mixamo 重定向动作）导入验证。
//! 不是完整游戏——是一个最小实弹环境，回答三个问题：
//!
//! 1. **渲染**：外部建模管线（Blender + glTF 导出）产出的蒙皮角色能否被
//!    引擎原样渲染（材质 / 贴图 / 多 primitive 网格）；
//! 2. **动画**：GLB 内嵌的多个动作剪辑（`Mixamo_Walk` / `Mixamo_Jump`）
//!    能否按名加载并正确驱动 87 骨蒙皮（采样关键帧 + 线性插值）；
//! 3. **切换**：运行期在剪辑间切换（步行循环 ↔ 跳跃单次 → 回落步行）。
//!
//! 资产来源与经验全记录见 `docs/private/04.md`（Mixamo 动画重定向到
//! 自定义骨架的完整管线与九个坑）。GLB 本体（32MB，版权模型）不入库——
//! `assets/exp_weiqing.glb` 已在 `.gitignore`，缺席时插件降级
//! 为无角色模式（相机 + 地面照常，诚实告警）。
//!
//! 结构（游戏 crate 标准范式，本作逻辑极薄故不设 logic.rs）：
//! - [`plugin`]：[`ExpPlugin`]——场景 / 角色装配 + 动作切换接线，
//!   [`ExpPlugin::build_schedule`](plugin::ExpPlugin::build_schedule)
//!   供 headless 测试与离屏快照复用（动画推进由 `AnimationPlugin` 负责，
//!   headless 路径需自行注册——与窗口路径 `DefaultPlugins` 同一编排）。
//!
//! ```ignore
//! use iris_engine::prelude::*;
//! use iris_exp::ExpPlugin;
//!
//! App::new()
//!     .add_plugins(DefaultPlugins::new("Iris Exp"))
//!     .add_plugin(ExpPlugin::default())
//!     .run();
//! ```
//!
//! 操作：`1` 步行循环、`空格` 跳跃单次（结束自动回落步行）。

pub mod plugin;

pub use plugin::{ExpEntities, ExpPlugin, JUMP_CLIP, MODEL_PATH, WALK_CLIP};
