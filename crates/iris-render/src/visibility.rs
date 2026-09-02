//! 实体可见性开关：渲染收集阶段的过滤组件。
//!
//! 语义约定（打磨 III 起）：
//! - **组件缺席 = 可见**——不给既有场景加任何迁移成本，默认行为零变化；
//!   挂上 [`Visibility::HIDDEN`] 即在全部渲染 pass（主 pass / 方向光阴影 /
//!   点光阴影）与 UI 叠加层中跳过该实体。dogfooding 反馈的「隐藏实体
//!   只能移出视野或销毁重建」缺口即由此闭合（见 ARCHITECTURE §8.12）。
//! - **层级级联：任一祖先隐藏 ⇒ 子树全隐藏**。实现为**渲染收集期沿
//!   `ChildOf` 父链上行遍历**（`renderer::hidden_by_ancestor`）——不加
//!   新组件、不动 `iris_core::hierarchy` 数据；自身 `HIDDEN` 的语义与
//!   级联缺失时代完全一致，级联只补「祖先隐藏带动子树」的方向（每实
//!   体代价 O(父链深度)，典型场景 ≤ 4）。普通网格 / 蒙皮 / 透明三条
//!   收集路径与粒子发射器整池过滤全部生效；可见性过滤不计入
//!   `FrameStats.culled_instances`（剔除口径不变）。

use iris_core::Component;

/// 实体可见性（渲染过滤组件）。缺席 = 可见；祖先 `HIDDEN` ⇒ 子树全隐藏
/// （模块文档的层级级联语义）。
///
/// ```ignore
/// world.insert(coin, Visibility::HIDDEN); // 已收集金币：所有 pass 跳过
/// world.insert(coin, Visibility::VISIBLE); // 重新出现（组件保留，切换开销为零）
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Component)]
pub struct Visibility {
    pub visible: bool,
}

impl Visibility {
    /// 可见（渲染收集包含该实体）。
    pub const VISIBLE: Self = Self { visible: true };
    /// 隐藏（渲染收集跳过该实体：主 pass、全部阴影 pass 与 UI 均不绘制）。
    pub const HIDDEN: Self = Self { visible: false };
}

impl Default for Visibility {
    fn default() -> Self {
        Self::VISIBLE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_and_default() {
        // 常量断言放进 const 块：编译期求值（clippy::assertions_on_constants）
        const { assert!(Visibility::VISIBLE.visible) };
        const { assert!(!Visibility::HIDDEN.visible) };
        assert_eq!(Visibility::default(), Visibility::VISIBLE, "默认可见");
    }
}
