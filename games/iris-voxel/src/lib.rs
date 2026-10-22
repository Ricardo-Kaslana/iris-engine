//! # iris-voxel
//!
//! 类 Minecraft 体素沙盒的首个可玩闭环，同时是引擎的 dogfooding 项目。
//!
//! 世界固定为 8×8 个区块，每区块 16×16×48 方块并全部常驻。游戏实现
//! 确定性地形、面剔除区块网格、程序化纹理图集、胶囊碰撞、DDA 方块
//! 拾取、破坏与放置，以及准星与快捷栏 HUD。
//!
//! 纯玩法核心在 [`logic`] 中，只依赖 glam 与标准库；引擎资源与系统
//! 接线在 [`plugin`] 中。本轮不提供体素光照传播、存档、流式加载或
//! 生物。

pub mod atlas;
pub mod collision;
pub mod logic;
pub mod mesh;
pub mod plugin;

pub use plugin::VoxelPlugin;
