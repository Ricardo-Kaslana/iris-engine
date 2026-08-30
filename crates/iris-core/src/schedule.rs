//! 系统调度：按阶段（Startup / FixedUpdate / Update）顺序执行系统。

use crate::world::World;

/// 执行阶段。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    /// 进入主循环前执行一次（资源初始化、场景搭建）。
    Startup,
    /// 固定步长逻辑（物理、确定性模拟），每帧可能执行 0..N 次。
    FixedUpdate,
    /// 可变步长更新（输入、AI、动画），每帧一次。
    Update,
}

/// 系统：消费整个世界的一次性闭包或函数。
///
/// 函数与闭包均可：`fn(&mut World)` 自动转换。
pub struct System {
    pub name: &'static str,
    f: Box<dyn FnMut(&mut World) + Send>,
}

impl System {
    pub fn new<F>(name: &'static str, f: F) -> Self
    where
        F: FnMut(&mut World) + Send + 'static,
    {
        Self { name, f: Box::new(f) }
    }

    pub fn run(&mut self, world: &mut World) {
        (self.f)(world);
    }
}

impl<F> From<F> for System
where
    F: FnMut(&mut World) + Send + 'static,
{
    fn from(f: F) -> Self {
        Self { name: std::any::type_name::<F>(), f: Box::new(f) }
    }
}

impl std::fmt::Debug for System {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("System").field("name", &self.name).finish()
    }
}

#[derive(Default)]
pub struct Schedule {
    startup: Vec<System>,
    fixed: Vec<System>,
    update: Vec<System>,
}

impl Schedule {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册系统到指定阶段。相同阶段内按注册顺序执行。
    pub fn add_system(&mut self, stage: Stage, system: impl Into<System>) -> &mut Self {
        let system = system.into();
        log::trace!("注册系统 {} 到阶段 {stage:?}", system.name);
        match stage {
            Stage::Startup => self.startup.push(system),
            Stage::FixedUpdate => self.fixed.push(system),
            Stage::Update => self.update.push(system),
        }
        self
    }

    /// 执行某阶段全部系统。
    pub fn run(&mut self, stage: Stage, world: &mut World) {
        let systems: &mut Vec<System> = match stage {
            Stage::Startup => &mut self.startup,
            Stage::FixedUpdate => &mut self.fixed,
            Stage::Update => &mut self.update,
        };
        for system in systems {
            system.run(world);
        }
    }

    pub fn system_count(&self, stage: Stage) -> usize {
        match stage {
            Stage::Startup => self.startup.len(),
            Stage::FixedUpdate => self.fixed.len(),
            Stage::Update => self.update.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Component, Resource};

    #[derive(Component)] struct Counter(i32);

    #[test]
    fn systems_run_in_registration_order() {
        let mut world = World::new();
        let entity = world.spawn((Counter(0),));

        fn add_ten(world: &mut World) {
            let mut counter = world.get_mut::<Counter>(world.entities().next().unwrap()).unwrap();
            counter.0 += 10;
        }
        let add_hundred = |world: &mut World| {
            let mut counter = world.get_mut::<Counter>(world.entities().next().unwrap()).unwrap();
            counter.0 += 100;
        };

        let mut schedule = Schedule::new();
        schedule
            .add_system(Stage::Update, add_hundred)
            .add_system(Stage::Update, add_ten);
        assert_eq!(schedule.system_count(Stage::Update), 2);
        schedule.run(Stage::Update, &mut world);
        assert_eq!(world.get::<Counter>(entity).unwrap().0, 110);
    }

    #[test]
    fn startup_runs_once_via_explicit_call() {
        #[derive(Resource)]
        struct Stamp(u32);

        let mut world = World::new();
        let mut schedule = Schedule::new();
        schedule.add_system(Stage::Startup, |world: &mut World| {
            world.insert_resource(Stamp(0));
        });
        schedule.run(Stage::Startup, &mut world);
        assert_eq!(world.resource::<Stamp>().0, 0);
    }
}
