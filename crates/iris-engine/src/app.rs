//! App 框架：构建期注册插件与系统，`run` 进入平台主循环。
//!
//! 主循环时序（每帧）：
//! ```text
//! 事件轮换（clear_all_events：上帧事件降级为仍可读的上一代，
//!            连续两帧未消费的残留丢弃——见 iris_core::event 生命周期）
//! 窗口事件泵（输入写入 Input 资源）
//!   -> Time.begin_frame（可变 delta + 固定步长累积）
//!   -> FixedUpdate × N（确定性逻辑，最多 5 步防死亡螺旋）
//!   -> Update（可变步长逻辑，写局部变换）
//!   -> PostUpdate（收尾系统：层级传播等，每帧一次，不在 FixedUpdate 循环内）
//!   -> 渲染 + 呈现（读取传播后的 GlobalTransform）
//!   -> 输入一次性状态清零 / 检查 AppExit（读本帧产出——Update 与
//!      PostUpdate 发出的 AppExit 都能被读到，打磨 V 起）
//! ```

// 平台原因（wasm32）：手动驱动异步 GPU 初始化未来所需的任务原语只在
// wasm 分支（Runner::poll_surface_init）使用，native 下引入会产生未用告警
#[cfg(target_arch = "wasm32")]
use std::pin::Pin;
#[cfg(target_arch = "wasm32")]
use std::task::{Context, Poll, Waker};

use iris_core::prelude::{AppExit, Instant, Schedule, Stage, System, Time, World};
use iris_platform::input::Input;
use iris_platform::window::{WindowConfig, WindowHandle};
use iris_render::surface::SurfaceKit;
use winit::application::ApplicationHandler;
use winit::event::{MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};

use crate::plugin::{Plugin, Viewport};

/// 引擎应用：构建期收集插件与系统，运行期驱动主循环。
pub struct App {
    pub world: World,
    pub schedule: Schedule,
    pub(crate) window_config: WindowConfig,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            world: World::new(),
            schedule: Schedule::new(),
            window_config: WindowConfig::default(),
        }
    }

    /// 应用插件（立即生效）。
    pub fn add_plugin(mut self, plugin: impl Plugin) -> Self {
        plugin.build(&mut self);
        self
    }

    /// `add_plugin` 的复数别名（Bevy 风格，接受插件或插件组）。
    pub fn add_plugins(mut self, plugin: impl Plugin) -> Self {
        plugin.build(&mut self);
        self
    }

    /// 注册系统到指定阶段。
    pub fn add_system(mut self, stage: Stage, system: impl Into<System>) -> Self {
        self.schedule.add_system(stage, system);
        self
    }

    /// 注册并行系统（声明组件读写集，与无冲突系统并发执行）。
    pub fn add_parallel_system(
        mut self,
        stage: Stage,
        access: iris_core::prelude::Access,
        system: impl Fn(&World) + Send + Sync + 'static,
    ) -> Self {
        self.schedule.add_parallel_system(stage, access, system);
        self
    }

    /// 设置窗口配置（优先级低于 WindowPlugin / DefaultPlugins）。
    pub fn with_window(mut self, config: WindowConfig) -> Self {
        self.window_config = config;
        self
    }

    /// 构建期直接访问世界（注入初始资源等）。
    pub fn world(&mut self) -> &mut World {
        &mut self.world
    }

    /// 进入主循环（阻塞直到窗口关闭或收到 `AppExit` 事件）。
    ///
    /// 注意：必须在主线程调用（winit 的平台约束）。
    ///
    /// 平台原因（wasm32）：winit web 后端的 `run_app` 以 JS 异常 unwind
    /// 退出循环——**本函数在 web 上不会正常返回**，`run_app` 之后的代码
    /// （包括下面的 `.expect`）不会执行，因此不要在 `run()` 之后放置任何
    /// 「正常收尾」逻辑。
    pub fn run(self) {
        let event_loop = EventLoop::builder()
            .build()
            .expect("创建事件循环失败（是否在主线程调用？）");
        let mut runner = Runner {
            app: self,
            window: None,
            surface: None,
            startup_done: false,
            // 平台原因（wasm32）：GPU 初始化改走逐帧驱动的异步未来
            #[cfg(target_arch = "wasm32")]
            surface_init: None,
        };
        event_loop.run_app(&mut runner).expect("事件循环异常退出");
    }
}

struct Runner {
    app: App,
    window: Option<WindowHandle>,
    surface: Option<SurfaceKit>,
    startup_done: bool,
    /// 平台原因（wasm32）：GPU 适配器 / 设备请求是 JS Promise，无法阻塞
    /// 等待，`SurfaceKit` 的初始化只能异步进行。这里暂存待驱动的初始化
    /// 未来，由每帧 `tick` 推进一步（Promise 在帧间由 JS 微任务队列 resolve）。
    #[cfg(target_arch = "wasm32")]
    surface_init: Option<Pin<Box<dyn std::future::Future<Output = Option<SurfaceKit>>>>>,
}

/// 驱动一帧的全部更新阶段：FixedUpdate × N（时间累积器决定步数）→
/// Update → PostUpdate。PostUpdate 每帧执行一次且不在 FixedUpdate
/// 循环内——层级传播（HierarchyPlugin）栖身于此：Update 写局部变换 →
/// PostUpdate 传播 → 渲染读取。独立成函数便于编排单测。
fn run_update_stages(schedule: &mut Schedule, world: &mut World) {
    // 固定步长逻辑
    let fixed_steps = world
        .try_resource_mut::<Time>()
        .map(|mut t| t.fixed_steps())
        .unwrap_or(0);
    for _ in 0..fixed_steps {
        schedule.run(Stage::FixedUpdate, world);
    }

    // 可变更新
    schedule.run(Stage::Update, world);

    // 后更新（每帧一次）：层级传播等收尾系统
    schedule.run(Stage::PostUpdate, world);
}

impl Runner {
    /// 每帧逻辑：更新 -> 渲染 -> 收尾。
    fn tick(&mut self, event_loop: &ActiveEventLoop) {
        // wasm：先推进一步异步 GPU 初始化（见 Runner.surface_init 注释）
        #[cfg(target_arch = "wasm32")]
        self.poll_surface_init();

        let Some(surface) = &mut self.surface else {
            return;
        };
        let world = &mut self.app.world;

        // 1. 帧首事件轮换（上帧事件降级为仍可读的上一代、更早一代丢弃
        //    ——PostUpdate 产出的事件本帧 Update 全程可读，见 event 生命周期）
        world.clear_all_events();

        // 2. 时间推进（时间资源缺失时按无时间模式运行）
        if let Some(mut time) = world.try_resource_mut::<Time>() {
            // 跨平台封装 Instant：wasm 上 std::time::Instant 运行时 panic
            time.begin_frame(Instant::now());
        }

        // 3. FixedUpdate × N -> Update -> PostUpdate（见 run_update_stages）
        run_update_stages(&mut self.app.schedule, world);

        // 4. 渲染 + 呈现
        surface.render(world);

        // 5. 帧收尾：输入一次性状态清零
        if let Some(mut input) = world.try_resource_mut::<Input>() {
            input.end_frame();
        }

        // 6. 退出检查：读本帧产出的事件（Update 与 PostUpdate 发出的
        //    AppExit 都能被读到——调序前退出检查在清空之后、恒读空）。
        //    下一帧帧首的轮换会再给一次读取机会（双代窗口），正常路径
        //    本帧即可退出。
        if world.try_events::<AppExit>().is_some_and(|e| !e.is_empty()) {
            event_loop.exit();
        }
    }

    /// 平台原因（wasm32）：逐帧驱动异步 GPU 初始化未来。
    ///
    /// JS Promise 只能在本帧 Rust 代码让出控制权后由浏览器微任务队列
    /// resolve，因此这里用 noop Waker 手动 poll：未就绪则把未来放回
    /// `surface_init` 等下一帧；Promise settle 后内部状态已更新，下一次
    /// poll 即返回 Ready（无需真实唤醒通知，winit web 每帧都会请求重绘）。
    /// 就绪后走与 native `resumed` 相同的 Startup 流程。
    #[cfg(target_arch = "wasm32")]
    fn poll_surface_init(&mut self) {
        if self.surface.is_some() {
            return;
        }
        let Some(mut init) = self.surface_init.take() else {
            return;
        };
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        match init.as_mut().poll(&mut cx) {
            Poll::Ready(Some(kit)) => {
                self.surface = Some(kit);
                if !self.startup_done {
                    self.app.schedule.run(Stage::Startup, &mut self.app.world);
                    self.startup_done = true;
                }
            }
            Poll::Ready(None) => {
                // 与 native resumed 的降级路径一致；不再重试，事件循环空转
                log::error!("没有可用的 GPU 适配器，无法启动渲染");
            }
            Poll::Pending => self.surface_init = Some(init),
        }
    }
}

impl ApplicationHandler for Runner {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window = event_loop
            .create_window(self.app.window_config.to_attributes())
            .expect("创建窗口失败");
        let window: WindowHandle = std::sync::Arc::new(window);

        // 平台原因（wasm32）：SurfaceKit 初始化是 JS Promise，只能异步等待
        // （pollster::block_on 在 wasm 是忙自旋，会卡死浏览器主线程）。把
        // 初始化未来暂存到 Runner，由每帧 tick 推进一步；完成前 tick 因
        // surface 为 None 直接返回，事件循环照常运转。
        #[cfg(target_arch = "wasm32")]
        {
            self.surface_init = Some(Box::pin(SurfaceKit::with_window_async(window.clone())));
            self.window = Some(window);
            // wasm 路径到此为止（下方 native 块被 cfg 摘除，函数在此收尾）
        }

        // native：同步创建（winit 平台约束要求在主线程，直接阻塞等待 GPU 就绪）
        #[cfg(not(target_arch = "wasm32"))]
        {
            match SurfaceKit::with_window(window.clone()) {
                Some(surface) => self.surface = Some(surface),
                None => {
                    log::error!("没有可用的 GPU 适配器，无法启动渲染");
                    event_loop.exit();
                    return;
                }
            }
            self.window = Some(window);

            // Startup 阶段：GPU 就绪后执行一次，系统可安全注册网格等资源
            if !self.startup_done {
                self.app.schedule.run(Stage::Startup, &mut self.app.world);
                self.startup_done = true;
            }
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        // 失焦 / 最小化时清空输入，防止按键卡死
        if let Some(mut input) = self.app.world.try_resource_mut::<Input>() {
            input.clear_all();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        let is_main = self.window.as_ref().is_some_and(|w| w.id() == window_id);
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if is_main {
                    if let Some(surface) = &mut self.surface {
                        surface.resize(size.width, size.height);
                    }
                    // 视口资源同步（照 Input 资源的模式）：拾取等消费方
                    // 读到的宽高比始终是当前帧的实际尺寸（缺席则跳过——
                    // 未用 DefaultPlugins 的极简 App 自行负责）
                    if let Some(mut viewport) = self.app.world.try_resource_mut::<Viewport>() {
                        viewport.width = size.width;
                        viewport.height = size.height;
                    }
                }
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } if is_main => {
                if let Some(mut input) = self.app.world.try_resource_mut::<Input>() {
                    input.on_key(event.physical_key, event.state);
                }
            }
            WindowEvent::MouseInput { state, button, .. } if is_main => {
                if let Some(mut input) = self.app.world.try_resource_mut::<Input>() {
                    input.on_mouse_button(button, state);
                }
            }
            WindowEvent::MouseWheel { delta, .. } if is_main => {
                let amount = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y * 40.0,
                    MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
                };
                if let Some(mut input) = self.app.world.try_resource_mut::<Input>() {
                    input.on_wheel(amount);
                }
            }
            WindowEvent::CursorMoved { position, .. } if is_main => {
                if let Some(mut input) = self.app.world.try_resource_mut::<Input>() {
                    input.on_cursor_moved((position.x, position.y));
                }
            }
            WindowEvent::Focused(false) => {
                if let Some(mut input) = self.app.world.try_resource_mut::<Input>() {
                    input.clear_all();
                }
            }
            WindowEvent::RedrawRequested if is_main => self.tick(event_loop),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // 持续重绘：请求下一帧（VSync 由呈现层限流）
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_core::Component;
    use iris_core::Resource;
    use iris_core::prelude::{GlobalTransform, Transform, Vec3};
    use std::time::Duration;

    #[derive(Component)]
    struct Ticks(u32);

    /// 事件生命周期事件类型（编排测试专用）。
    #[derive(Debug, Clone, Copy, PartialEq, iris_core::Event)]
    struct Ping(u32);

    /// 事件消费方的收件箱（并行系统不能 insert_resource，用组件承载）。
    #[derive(Component)]
    struct Inbox(Vec<u32>);

    /// 阶段编排：Update 先于 PostUpdate，且 PostUpdate 每帧恰好一次
    /// （即便 FixedUpdate 跑了多步——收尾系统不在固定步长循环内）。
    #[test]
    fn post_update_runs_once_after_update_outside_fixed_loop() {
        let mut world = World::new();
        world.insert_resource(Time::default());
        world.spawn((Ticks(0),));
        let entity = world.entities().next().unwrap();

        let mut schedule = Schedule::new();
        schedule
            .add_system(Stage::FixedUpdate, |world: &mut World| {
                let e = world.entities().next().unwrap();
                world.get_mut::<Ticks>(e).unwrap().0 += 1;
            })
            .add_system(Stage::Update, |world: &mut World| {
                let e = world.entities().next().unwrap();
                world.get_mut::<Ticks>(e).unwrap().0 += 10;
            })
            .add_system(Stage::PostUpdate, |world: &mut World| {
                let e = world.entities().next().unwrap();
                world.get_mut::<Ticks>(e).unwrap().0 += 100;
            });

        // 累积恰好 2 个固定步长的预算（Time::advance：确定性推进）
        world.resource_mut::<Time>().set_fixed_hz(60.0);
        world
            .resource_mut::<Time>()
            .advance(Duration::from_secs_f64(2.0 / 60.0));

        run_update_stages(&mut schedule, &mut world);

        assert_eq!(
            world.get::<Ticks>(entity).unwrap().0,
            2 + 10 + 100,
            "FixedUpdate ×2 + Update ×1 + PostUpdate ×1"
        );
    }

    /// 层级传播迁址 PostUpdate 后的端到端编排：Update 写父级局部变换 →
    /// PostUpdate（HierarchyPlugin 注册的 propagate_transforms）传播 →
    /// 子实体 GlobalTransform 当帧即反映新位置（不再滞后一帧）。
    #[test]
    fn hierarchy_propagation_in_post_update_sees_same_frame_writes() {
        let mut world = World::new();
        let parent = world.spawn((Transform::IDENTITY,));
        let child = world.spawn((Transform::from_xyz(1.0, 0.0, 0.0),));
        world.add_child(parent, child);

        let mut schedule = Schedule::new();
        schedule
            .add_system(Stage::Update, move |world: &mut World| {
                // 用户系统在 Update 内移动父级
                world.get_mut::<Transform>(parent).unwrap().position = Vec3::new(5.0, 0.0, 0.0);
            })
            .add_system(
                Stage::PostUpdate,
                iris_core::hierarchy::propagate_transforms,
            );

        run_update_stages(&mut schedule, &mut world);

        let child_global = world
            .get::<GlobalTransform>(child)
            .expect("传播应已为子实体建立 GlobalTransform")
            .position();
        assert!(
            (child_global - Vec3::new(6.0, 0.0, 0.0)).length() < 1e-5,
            "子实体应随父级当帧移动到 (6,0,0)，实际 {child_global:?}"
        );
    }

    /// 事件生命周期编排（打磨 V）：PostUpdate 产出的事件，下一帧 Update
    /// 全程可读，可读窗口 = 产出帧 + 下一帧（不重复、不累积）。时序镜像
    /// `Runner::tick`：帧首 `clear_all_events` 轮换 → Update → PostUpdate。
    /// 发送方按帧计数产出可区分的值，且第 3 帧起停止发送——用于断言
    /// 「窗口关闭后事件不再出现」。
    #[test]
    fn post_update_events_are_readable_next_frame_exactly_once() {
        #[derive(Resource)]
        struct FrameCounter(u32);

        let mut world = World::new();
        world.spawn((Inbox(vec![]),));
        world.insert_resource(FrameCounter(0));
        world.register_event::<Ping>(); // Update 首帧即 drain：显式预注册
        let entity = world.entities().next().unwrap();

        let mut schedule = Schedule::new();
        schedule
            .add_system(Stage::Update, |world: &mut World| {
                let e = world.entities().next().unwrap();
                let received: Vec<u32> = world.events_mut::<Ping>().drain().map(|p| p.0).collect();
                world.get_mut::<Inbox>(e).unwrap().0.extend(received);
            })
            .add_system(Stage::PostUpdate, |world: &mut World| {
                let mut counter = world.resource_mut::<FrameCounter>();
                let value = counter.0;
                counter.0 += 1;
                drop(counter);
                if value < 2 {
                    world.send_event(Ping(value));
                }
            });

        // 帧 1：帧首轮换（缓冲为空）→ Update 读到 0 → PostUpdate 产出 0
        world.clear_all_events();
        schedule.run(Stage::Update, &mut world);
        schedule.run(Stage::PostUpdate, &mut world);
        assert!(
            world.get::<Inbox>(entity).unwrap().0.is_empty(),
            "产出帧内 Update 先于 PostUpdate，读不到"
        );
        // 帧 2：帧首轮换（上帧事件降级为仍可读的上一代）→ Update 读到 0
        world.clear_all_events();
        schedule.run(Stage::Update, &mut world);
        schedule.run(Stage::PostUpdate, &mut world);
        assert_eq!(
            world.get::<Inbox>(entity).unwrap().0,
            vec![0],
            "上帧 PostUpdate 产出的事件应在本帧 Update 读到"
        );
        // 帧 3：上帧（帧 2）PostUpdate 产出的 1 → Update 读到；帧 3 的
        // PostUpdate 起停止发送
        world.clear_all_events();
        schedule.run(Stage::Update, &mut world);
        schedule.run(Stage::PostUpdate, &mut world);
        assert_eq!(
            world.get::<Inbox>(entity).unwrap().0,
            vec![0, 1],
            "每帧 PostUpdate 事件恰好投递一次"
        );
        // 帧 4：帧 3 未产出事件、更早的 0/1 均已 drain——Update 读到空
        //（可读窗口关闭，事件不重复投递、不无限累积）
        world.clear_all_events();
        schedule.run(Stage::Update, &mut world);
        assert_eq!(
            world.get::<Inbox>(entity).unwrap().0,
            vec![0, 1],
            "事件可读窗口 = 产出帧 + 下一帧，之后不再出现"
        );
    }

    /// 事件缓冲的轮换语义经 World 装箱路径派发正确的锚点（编排测试的
    /// 单元级对照，与 iris_core::event 的单测互为镜像——验证
    /// `ErasedEvents::rotate` 对 `Box<dyn ErasedEvents>` 的派发）。
    #[test]
    fn world_event_rotation_dispatches_through_erased_trait() {
        let mut world = World::new();
        world.send_event(Ping(1));
        world.clear_all_events();
        let received: Vec<Ping> = world.events::<Ping>().iter().copied().collect();
        assert_eq!(received, vec![Ping(1)], "装箱轮换后上帧事件应仍可读");
    }
}
