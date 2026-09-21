//! # iris-exp
//!
//! Iris 引擎**实验性项目**：外部 GLB 角色（Mixamo 重定向动作）导入验证。
//! 不是完整游戏——是一个最小实弹环境，回答四个问题：
//!
//! 1. **渲染**：外部建模管线（Blender + glTF 导出）产出的蒙皮角色能否被
//!    引擎原样渲染（材质 / 贴图 / 多 primitive 网格）；
//! 2. **动画**：GLB 内嵌的多个动作剪辑（`Mixamo_Idle` / `Mixamo_Walk` /
//!    `Mixamo_Jump`）能否按名加载并正确驱动 87 骨蒙皮（采样关键帧 +
//!    线性插值）；
//! 3. **切换**：运行期在剪辑间带 crossfade 过渡切换——三状态机（Idle
//!    与 Walk 循环、Jump 单次）驱动，跳跃播完由引擎条件转移按 `moving`
//!    参数自动回落（静止回 Idle、移动中直落 Walk）；
//! 4. **控制**：自由视角与角色移动（右键拖拽轨道相机 + 滚轮缩放 +
//!    WASD 相对相机移动——纯游戏层装配，frictions ⑥）。
//!
//! 资产来源与经验全记录见 `docs/private/04.md`（Mixamo 动画重定向到
//! 自定义骨架的完整管线与九个坑）。GLB 本体（32MB，版权模型）不入库——
//! `assets/exp_weiqing.glb` 已在 `.gitignore`，缺席时插件降级
//! 为无角色模式（相机 + 地面照常，诚实告警）。
//!
//! 结构（游戏 crate 标准范式，本作逻辑极薄故不设 logic.rs）：
//! - [`plugin`]：[`ExpPlugin`]——场景 / 角色装配 + 动作切换 + 控制
//!   接线，[`ExpPlugin::build_schedule`](plugin::ExpPlugin::build_schedule)
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
//! 操作：`WASD` 移动（相对相机）、右键拖拽环绕视角、滚轮缩放、
//! `Q`/`E` 调注视点高度（脚 ↔ 头顶）、`空格` 跳跃单次（结束回落常态）。
//! 常态动作是 `Mixamo_Idle`——停止且无进行中单次动作时循环播放，移动
//! 即以 0.15 s crossfade 切步行循环，全部动作切换带过渡混合（停回 Idle、
//! 走切 Walk 全自动，无手动切换键）。
//!
//! 渲染装配（引擎能力 opt-in 资源的游戏层显式启用）：CSM 阴影 +
//! mip/各向异性（R66）+ GTAO（R77）+ TAA + 泛光（默认档）+ 暗角；
//! 运动模糊装配就位但暂关（腾空段拖影在定格图读作残影，窗口游玩可开）；
//! SSR 与遮挡剔除按场景特性不开（无反射面 / 24 m 小场景），
//! 理由见 [`plugin::ExpPlugin`] 装配注释。

pub mod plugin;

pub use plugin::{
    ExpCharacter, ExpEntities, ExpPlugin, IDLE_CLIP, JUMP_CLIP, MODEL_PATH, OrbitCamera, WALK_CLIP,
};
