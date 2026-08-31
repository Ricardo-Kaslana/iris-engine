//! 实体命名：按名定位实体的调试与工具辅助。
//!
//! dogfooding 反馈（见 ARCHITECTURE §8.12）：跨系统定位实体（HUD 刷新、
//! 金币动画等）此前只能自建「实体句柄索引」资源。[`Name`] 组件 +
//! [`World::entity_named`] 提供最低限度的按名查找；高频访问仍建议沿用
//! 句柄资源（线性扫描是 O(n)）。

use crate::Component;
use crate::entity::Entity;
use crate::world::World;

/// 实体名。同名实体合法，查找取生成序首个。
///
/// ```ignore
/// let hud = world.spawn((Name("hud/score".into()), UiText { .. }));
/// world.entity_named("hud/score");
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Component)]
pub struct Name(pub String);

impl From<&str> for Name {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<String> for Name {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl World {
    /// 按名查找实体，返回**生成序首个**同名实体；未命中返回 `None`。
    ///
    /// 线性扫描全量 `Name` 组件（O(n)），适用于调试、编辑器与测试这类
    /// 低频查找——每帧调用的热路径请改用实体句柄资源。
    pub fn entity_named(&self, name: &str) -> Option<Entity> {
        let mut found = None;
        self.query::<(crate::prelude::Entity, &Name)>(|(entity, candidate)| {
            if found.is_none() && candidate.0 == name {
                found = Some(entity);
            }
        });
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Component;

    #[derive(Component)] struct Tag;

    #[test]
    fn entity_named_hits_misses_and_takes_first() {
        let mut world = World::new();
        let first = world.spawn((Name("coin".into()), Tag));
        let _second = world.spawn((Name("coin".into()), Tag));
        let other = world.spawn((Name("wall".into()), Tag));

        // 命中：同名取生成序首个
        assert_eq!(world.entity_named("coin"), Some(first));
        // 其余名字照常命中
        assert_eq!(world.entity_named("wall"), Some(other));
        // 未命中
        assert_eq!(world.entity_named("camera"), None);
    }

    #[test]
    fn entity_named_ignores_unnamed_entities() {
        let mut world = World::new();
        world.spawn((Tag,));
        assert_eq!(world.entity_named("anything"), None, "无 Name 组件的世界应未命中");
        let named = world.spawn((Name::from("player"),));
        assert_eq!(world.entity_named("player"), Some(named));
    }
}
