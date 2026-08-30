//! 变换层级：父子关系与全局变换传播。
//!
//! ```ignore
//! let sun = world.spawn((Transform::IDENTITY,));
//! let earth = world.spawn((Transform::from_xyz(3.0, 0.0, 0.0),));
//! world.add_child(sun, earth);
//!
//! // 每帧由 HierarchyPlugin 自动调用：
//! propagate_transforms(&mut world);
//! // 此后渲染器读取 GlobalTransform（父级旋转会带动子级的世界位置）
//! ```
//!
//! 语义约定：
//! - `Transform` 是**本地**变换（相对父级）；`GlobalTransform` 是**世界**变换，只读，
//!   每帧由 `propagate_transforms` 重建，手动修改会在下一帧被覆盖。
//! - 父实体被销毁后，子实体自动「晋升」为根（其 `GlobalTransform` 保持上一帧值再被重建），
//!   需要级联销毁请用 `despawn_recursive`。

use glam::{Mat4, Vec3};

use crate::Component;
use crate::entity::Entity;
use crate::transform::Transform;
use crate::world::World;

/// 「我的父实体」组件。由 [`World::add_child`] 维护，勿手动插入。
#[derive(Clone, Copy, Debug, PartialEq, Component)]
pub struct ChildOf(pub Entity);

/// 「我的子实体列表」组件。由 [`World::add_child`] 维护，勿手动插入。
#[derive(Clone, Debug, Default, Component)]
pub struct Children(pub Vec<Entity>);

/// 世界空间变换（只读快照）。由层级系统从 `Transform` 链复合而来。
#[derive(Clone, Copy, Debug, PartialEq, Component)]
pub struct GlobalTransform(Mat4);

impl GlobalTransform {
    pub fn from_matrix(matrix: Mat4) -> Self {
        Self(matrix)
    }

    pub fn from_transform(transform: Transform) -> Self {
        Self(transform.matrix())
    }

    /// 世界空间模型矩阵。
    pub fn matrix(&self) -> Mat4 {
        self.0
    }

    /// 世界空间位置（矩阵平移分量）。
    pub fn position(&self) -> Vec3 {
        self.0.w_axis.truncate()
    }
}

impl World {
    /// 建立父子关系。子实体已有父时先解除旧关系。
    ///
    /// # Panics
    /// 父或子实体已失效、或把实体设为自己的父级时 panic。
    pub fn add_child(&mut self, parent: Entity, child: Entity) {
        assert!(self.contains(parent), "父实体 {parent:?} 不存在");
        assert!(self.contains(child), "子实体 {child:?} 不存在");
        assert!(parent != child, "实体不能是自己的父级");

        // 解除旧父关系
        if let Some(ChildOf(old)) = self.remove_component::<ChildOf>(child) {
            self.remove_from_children_list(old, child);
        }
        self.insert(child, ChildOf(parent));
        if self.get::<Children>(parent).is_some() {
            let mut list = self.get_mut::<Children>(parent).expect("刚刚确认存在");
            list.0.push(child);
        } else {
            self.insert(parent, Children(vec![child]));
        }
    }

    /// 解除子实体与其父的关系（子成为根实体）。
    pub fn remove_from_parent(&mut self, child: Entity) {
        if let Some(ChildOf(old)) = self.remove_component::<ChildOf>(child) {
            self.remove_from_children_list(old, child);
        }
    }

    fn remove_from_children_list(&mut self, parent: Entity, child: Entity) {
        if let Some(mut list) = self.get_mut::<Children>(parent) {
            list.0.retain(|&c| c != child);
        }
    }

    /// 父实体（若存在且存活）。
    pub fn parent(&self, child: Entity) -> Option<Entity> {
        let ChildOf(parent) = *self.get::<ChildOf>(child)?;
        self.contains(parent).then_some(parent)
    }

    /// 子实体列表（无子时为空）。
    pub fn children(&self, parent: Entity) -> Vec<Entity> {
        self.get::<Children>(parent).map(|c| c.0.clone()).unwrap_or_default()
    }

    /// 级联销毁：销毁实体及其全部后代。
    pub fn despawn_recursive(&mut self, entity: Entity) -> bool {
        if !self.contains(entity) {
            return false;
        }
        for child in self.children(entity) {
            self.despawn_recursive(child);
        }
        self.remove_from_parent(entity);
        self.despawn(entity)
    }
}

/// 层级传播：把每个实体的 `Transform` 沿父链复合为 `GlobalTransform`。
///
/// 引擎主循环每帧在用户 Update 系统之前调用（`HierarchyPlugin` 注册）。
/// 根实体（无父或父已失效）的全局变换等于其本地变换。
pub fn propagate_transforms(world: &mut World) {
    // 1. 为新实体补齐 GlobalTransform（有父的实体稍后被传播覆盖）
    let mut pending: Vec<Entity> = Vec::new();
    world.query::<(Entity, &Transform)>(|(entity, _)| pending.push(entity));
    for entity in pending {
        if world.get::<GlobalTransform>(entity).is_none() {
            let local = *world.get::<Transform>(entity).expect("查询已确认存在");
            world.insert(entity, GlobalTransform::from_transform(local));
        }
    }

    // 2. 收集根：有 Transform 且（无父 或 父已失效）
    let mut roots: Vec<Entity> = Vec::new();
    world.query::<(Entity, &Transform)>(|(entity, _)| roots.push(entity));
    roots.retain(|entity| match world.get::<ChildOf>(*entity) {
        Some(parent) => !world.contains(parent.0),
        None => true,
    });

    // 3. 深度优先传播（游戏场景层级浅，递归深度可控）
    for root in roots {
        propagate(world, root, None);
    }
}

fn propagate(world: &mut World, entity: Entity, parent_matrix: Option<Mat4>) {
    let local = world
        .get::<Transform>(entity)
        .map(|t| *t)
        .expect("根收集已确认存在 Transform");
    let matrix = match parent_matrix {
        Some(parent) => parent * local.matrix(),
        None => local.matrix(),
    };
    world.insert(entity, GlobalTransform::from_matrix(matrix));

    for child in world.children(entity) {
        if world.contains(child) {
            propagate(world, child, Some(matrix));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::Vec3;

    #[test]
    fn add_child_links_both_sides() {
        let mut world = World::new();
        let parent = world.spawn((Transform::IDENTITY,));
        let child = world.spawn((Transform::IDENTITY,));
        world.add_child(parent, child);

        assert_eq!(world.parent(child), Some(parent));
        assert_eq!(world.children(parent), vec![child]);
    }

    #[test]
    fn reparent_moves_child() {
        let mut world = World::new();
        let a = world.spawn((Transform::IDENTITY,));
        let b = world.spawn((Transform::IDENTITY,));
        let child = world.spawn((Transform::IDENTITY,));
        world.add_child(a, child);
        world.add_child(b, child);

        assert_eq!(world.parent(child), Some(b));
        assert!(world.children(a).is_empty(), "旧父的子列表应移除该实体");
        assert_eq!(world.children(b), vec![child]);
    }

    #[test]
    fn propagation_composes_parent_chain() {
        let mut world = World::new();
        let root = world.spawn((Transform::from_xyz(1.0, 0.0, 0.0),));
        let mid = world.spawn((Transform::from_xyz(1.0, 0.0, 0.0),));
        let leaf = world.spawn((Transform::from_xyz(1.0, 0.0, 0.0),));
        world.add_child(root, mid);
        world.add_child(mid, leaf);

        propagate_transforms(&mut world);

        assert_eq!(world.get::<GlobalTransform>(root).unwrap().position(), Vec3::X);
        assert_eq!(world.get::<GlobalTransform>(mid).unwrap().position(), Vec3::new(2.0, 0.0, 0.0));
        assert_eq!(
            world.get::<GlobalTransform>(leaf).unwrap().position(),
            Vec3::new(3.0, 0.0, 0.0)
        );
    }

    #[test]
    fn parent_rotation_carries_child() {

        let mut world = World::new();
        // 父绕 Y 转 180°，子本地位于 +X → 世界位于 -X
        let root = world.spawn((Transform::from_rotation_y(std::f32::consts::PI),));
        let child = world.spawn((Transform::from_xyz(1.0, 0.0, 0.0),));
        world.add_child(root, child);

        propagate_transforms(&mut world);
        let pos = world.get::<GlobalTransform>(child).unwrap().position();
        assert!(pos.x < 0.0 && pos.x > -1.01 && pos.x < -0.99, "子应被父旋转带到 -X：{pos:?}");
    }

    #[test]
    fn orphaned_child_promotes_to_root() {
        let mut world = World::new();
        let parent = world.spawn((Transform::from_xyz(5.0, 0.0, 0.0),));
        let child = world.spawn((Transform::IDENTITY,));
        world.add_child(parent, child);

        world.despawn(parent); // 非级联销毁：子变孤儿
        propagate_transforms(&mut world);

        // 孤儿按根重建：全局 == 本地
        assert_eq!(world.get::<GlobalTransform>(child).unwrap().position(), Vec3::ZERO);
    }

    #[test]
    fn despawn_recursive_removes_subtree() {
        let mut world = World::new();
        let root = world.spawn((Transform::IDENTITY,));
        let a = world.spawn((Transform::IDENTITY,));
        let b = world.spawn((Transform::IDENTITY,));
        world.add_child(root, a);
        world.add_child(a, b);

        assert!(world.despawn_recursive(root));
        assert!(!world.contains(a));
        assert!(!world.contains(b));
        assert_eq!(world.entity_count(), 0);
    }

    #[test]
    fn removal_detaches_child() {
        let mut world = World::new();
        let parent = world.spawn((Transform::IDENTITY,));
        let child = world.spawn((Transform::IDENTITY,));
        world.add_child(parent, child);

        world.remove_from_parent(child);
        assert_eq!(world.parent(child), None);
        assert!(world.children(parent).is_empty());
    }
}
