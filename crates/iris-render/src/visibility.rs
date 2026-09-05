//! 实体可见性开关：渲染收集阶段的过滤组件。
//!
//! 语义约定（打磨 III 起）：
//! - **组件缺席 = 可见**——不给既有场景加任何迁移成本，默认行为零变化；
//!   挂上 [`Visibility::HIDDEN`] 即在全部渲染 pass（主 pass / 方向光阴影 /
//!   点光阴影）与 UI 叠加层中跳过该实体。dogfooding 反馈的「隐藏实体
//!   只能移出视野或销毁重建」缺口即由此闭合（见 ARCHITECTURE §8.12）。
//! - **隐藏子树原语（打磨 V）**：[`set_subtree_hidden`] 沿 `Children`
//!   递归写整棵子树的可见性（「隐藏整个角色 = 模型 + 腿链 + 网格子孙」
//!   的引擎侧闭合，§8.22⑤）；
//!   `ChildOf` 父链上行遍历**（`renderer::hidden_by_ancestor`）——不加
//!   新组件、不动 `iris_core::hierarchy` 数据；自身 `HIDDEN` 的语义与
//!   级联缺失时代完全一致，级联只补「祖先隐藏带动子树」的方向（每实
//!   体代价 O(父链深度)，典型场景 ≤ 4）。普通网格 / 蒙皮 / 透明 / 贴花
//!   （R39）四条收集路径与粒子发射器整池过滤全部生效；可见性过滤不计
//!   入 `FrameStats.culled_instances`（剔除口径不变）。

use iris_core::Component;
use iris_core::entity::Entity;
use iris_core::world::World;

/// 级联可见性的父链搜索深度上限：典型游戏层级 ≤ 4；上限只防御
/// `add_child` 未检测的构造性环（a、b 互设父级——层级传播对环同样未
/// 定义），渲染收集不得挂死，越界按可见处理（宁可多画的既有哲学）。
const VISIBILITY_ANCESTOR_MAX_DEPTH: usize = 64;

/// 子树遍历深度上限（照 [`hidden_by_ancestor`] 的深度帽模式）：只防御
/// `add_child` 未检测的构造性环（a、b 互设父级），越界截断并 warn 一次
/// ——原语不得挂死。
const SUBTREE_MAX_DEPTH: usize = 64;

/// 隐藏 / 显示整棵子树（打磨 V 原语）：把 `root` 及其全部后代的
/// [`Visibility`] 写为 `hidden` 指定的状态。
///
/// - **无 `Visibility` 的子实体会被插入**（「组件缺席 = 可见」的缺席
///   语义下，插入 `HIDDEN` 是唯一能隐藏它的方式；插入 `VISIBLE` 则是
///   把缺席语义显式化，行为不变）——插入要求该组件存储已注册（场景中
///   已有任意实体挂 `Visibility`；全场景从无该组件时插入静默跳过并
///   warn——注册新组件类型是结构性修改，`&World` 做不到）；
/// - **环防护**：显式栈迭代（深树不爆栈）+ 深度帽（越界截断告警一次）；
/// - 只读世界签名（`&World`）+ 尝试型写锁——并行系统可直接调用；
/// - `root` 已失效：no-op（不 panic，世代语义一致）。
///
/// ```ignore
/// use iris_render::visibility::set_subtree_hidden;
///
/// set_subtree_hidden(world, guard_root, true); // 击晕：整棵模型子树隐藏
/// set_subtree_hidden(world, guard_root, false); // 复活：整棵重新可见
/// ```
pub fn set_subtree_hidden(world: &World, root: Entity, hidden: bool) {
    let visibility = if hidden {
        Visibility::HIDDEN
    } else {
        Visibility::VISIBLE
    };
    let mut truncated = false;
    let mut stack = vec![(root, 0usize)];
    while let Some((entity, depth)) = stack.pop() {
        if depth > SUBTREE_MAX_DEPTH {
            truncated = true;
            continue;
        }
        match world.get_mut::<Visibility>(entity) {
            Some(mut component) => *component = visibility,
            // 无该组件：缺席 = 可见，隐藏须插入；VISIBLE 插入是显式化
            None => {
                if world.insert_registered(entity, visibility).is_none() {
                    log::warn!(
                        "set_subtree_hidden：实体 {entity:?} 无法插入 Visibility                         （组件存储未注册或实体已失效），已跳过"
                    );
                }
            }
        }
        for child in world.children(entity) {
            stack.push((child, depth + 1));
        }
    }
    if truncated {
        log::warn!(
            "set_subtree_hidden：子树深度超过 {SUBTREE_MAX_DEPTH} 层（构造性环？），             更深部分已截断"
        );
    }
}

/// 层级可见性级联：沿 `ChildOf` 父链上行，任一祖先挂
/// [`Visibility::HIDDEN`] 即隐藏——语义即「任一祖先隐藏 ⇒ 子树全隐藏」。
/// **渲染收集期上行遍历**实现：不加新组件、不动 `hierarchy` 数据；自身
/// `HIDDEN` 的检查由调用方完成（本函数只补祖先链）。复杂度
/// O(父链深度)，典型场景 ≤ 4。
pub(crate) fn hidden_by_ancestor(world: &World, mut entity: Entity) -> bool {
    for _ in 0..VISIBILITY_ANCESTOR_MAX_DEPTH {
        let Some(parent) = world.parent(entity) else {
            return false;
        };
        if world.get::<Visibility>(parent).is_some_and(|v| !v.visible) {
            return true;
        }
        entity = parent;
    }
    false
}

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
    use iris_core::prelude::Transform;

    /// 三层树全翻（隐藏与恢复双向）；子树外实体不受影响。
    #[test]
    fn subtree_hidden_flips_three_levels_and_back() {
        let mut world = World::new();
        let root = world.spawn((Transform::IDENTITY, Visibility::VISIBLE));
        let child = world.spawn((Transform::IDENTITY, Visibility::VISIBLE));
        let grandchild = world.spawn((Transform::IDENTITY, Visibility::VISIBLE));
        world.add_child(root, child);
        world.add_child(child, grandchild);
        let outside = world.spawn((Transform::IDENTITY, Visibility::VISIBLE));

        set_subtree_hidden(&world, root, true);
        for entity in [root, child, grandchild] {
            assert_eq!(
                *world.get::<Visibility>(entity).unwrap(),
                Visibility::HIDDEN,
                "子树实体 {entity:?} 应被隐藏"
            );
        }
        assert_eq!(
            *world.get::<Visibility>(outside).unwrap(),
            Visibility::VISIBLE,
            "子树外实体不受影响"
        );

        set_subtree_hidden(&world, root, false);
        for entity in [root, child, grandchild] {
            assert_eq!(
                *world.get::<Visibility>(entity).unwrap(),
                Visibility::VISIBLE,
                "恢复后子树实体 {entity:?} 应可见"
            );
        }
    }

    /// 无 Visibility 的子实体被插入 HIDDEN（缺席语义下隐藏它的唯一方式）。
    #[test]
    fn missing_visibility_components_get_inserted() {
        let mut world = World::new();
        let root = world.spawn((Transform::IDENTITY, Visibility::VISIBLE));
        // 子孙都无 Visibility（缺席 = 可见）
        let child = world.spawn((Transform::IDENTITY,));
        let grandchild = world.spawn((Transform::IDENTITY,));
        world.add_child(root, child);
        world.add_child(child, grandchild);

        set_subtree_hidden(&world, root, true);
        assert_eq!(
            *world.get::<Visibility>(grandchild).unwrap(),
            Visibility::HIDDEN,
            "无 Visibility 的孙实体应被插入 HIDDEN"
        );
        // 叶子 = 空子树：幂等（重复调用无变化、不 panic）
        set_subtree_hidden(&world, grandchild, true);
        set_subtree_hidden(&world, grandchild, true);
        assert_eq!(
            *world.get::<Visibility>(grandchild).unwrap(),
            Visibility::HIDDEN
        );
    }

    /// 构造性环（a、b 互设父级）不得挂死：深度帽截断、全部到场实体翻转。
    #[test]
    fn cyclic_subtree_terminates_via_depth_cap() {
        let mut world = World::new();
        let a = world.spawn((Transform::IDENTITY, Visibility::VISIBLE));
        let b = world.spawn((Transform::IDENTITY, Visibility::VISIBLE));
        world.add_child(a, b);
        world.add_child(b, a); // 构造性环（add_child 未检测的非法装配）
        set_subtree_hidden(&world, a, true);
        assert_eq!(*world.get::<Visibility>(a).unwrap(), Visibility::HIDDEN);
        assert_eq!(*world.get::<Visibility>(b).unwrap(), Visibility::HIDDEN);
    }

    #[test]
    fn constants_and_default() {
        // 常量断言放进 const 块：编译期求值（clippy::assertions_on_constants）
        const { assert!(Visibility::VISIBLE.visible) };
        const { assert!(!Visibility::HIDDEN.visible) };
        assert_eq!(Visibility::default(), Visibility::VISIBLE, "默认可见");
    }
}
