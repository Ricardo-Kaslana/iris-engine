//! # iris-voxel
//!
//! 类 Minecraft 体素沙盒的首个可玩闭环，同时是引擎的 dogfooding 项目。
//!
//! 世界固定为 8×8 个区块，每区块 16×16×48 方块并全部常驻。游戏实现
//! 确定性地形、面剔除区块网格、程序化纹理图集、天空光与方块光传播、
//! 平滑光照与逐顶点环境光遮蔽、胶囊碰撞、DDA 方块拾取、破坏与放置，
//! 以及准星与快捷栏 HUD。荧光石是当前唯一的发光方块。
//!
//! 纯玩法核心在 [`logic`] 中，只依赖 glam 与标准库；引擎资源与系统
//! 接线在 [`plugin`] 中。本轮仍不提供透明方块衰减、彩色光源、昼夜
//! 循环、存档、流式加载或生物。

pub mod atlas;
pub mod collision;
pub mod logic;
pub mod mesh;
pub mod plugin;

pub use plugin::VoxelPlugin;
