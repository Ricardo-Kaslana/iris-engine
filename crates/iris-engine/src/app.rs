//! App 框架：构建期注册插件与系统，`run` 进入平台主循环。
//!
//! 主循环时序（每帧）：
//! ```text
//! 窗口事件泵（输入写入 Input 资源）
//!   -> Time.begin_frame（可变 delta + 固定步长累积）
//!   -> FixedUpdate × N（确定性逻辑，最多 5 步防死亡螺旋）
//!   -> Update（可变步长逻辑）
//!   -> 渲染 + 呈现
//!   -> 事件清空 / 输入一次性状态清零 / 检查 AppExit
//! ```

use std::time::Instant;

use iris_core::prelude::{AppExit, Schedule, Stage, System, Time, World};
use iris_platform::input::Input;
use iris_platform::window::{WindowConfig, WindowHandle};
use iris_render::surface::SurfaceKit;
use winit::application::ApplicationHandler;
use winit::event::{MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};

use crate::plugin::Plugin;

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
    pub fn run(self) {
        let event_loop = EventLoop::builder()
            .build()
            .expect("创建事件循环失败（是否在主线程调用？）");
        let mut runner = Runner {
            app: self,
            window: None,
            surface: None,
            startup_done: false,
        };
        event_loop.run_app(&mut runner).expect("事件循环异常退出");
    }
}

struct Runner {
    app: App,
    window: Option<WindowHandle>,
    surface: Option<SurfaceKit>,
    startup_done: bool,
}

impl Runner {
    /// 每帧逻辑：更新 -> 渲染 -> 收尾。
    fn tick(&mut self, event_loop: &ActiveEventLoop) {
        let Some(surface) = &mut self.surface else {
            return;
        };
        let world = &mut self.app.world;

        // 1. 时间推进（时间资源缺失时按无时间模式运行）
        if let Some(mut time) = world.try_resource_mut::<Time>() {
            time.begin_frame(Instant::now());
        }

        // 2. 固定步长逻辑
        let fixed_steps = world
            .try_resource_mut::<Time>()
            .map(|mut t| t.fixed_steps())
            .unwrap_or(0);
        for _ in 0..fixed_steps {
            self.app.schedule.run(Stage::FixedUpdate, world);
        }

        // 3. 可变更新
        self.app.schedule.run(Stage::Update, world);

        // 4. 渲染 + 呈现
        surface.render(world);

        // 5. 帧收尾：事件清空、输入一次性状态清零
        world.clear_all_events();
        if let Some(mut input) = world.try_resource_mut::<Input>() {
            input.end_frame();
        }

        // 6. 退出检查
        if world.try_events::<AppExit>().is_some_and(|e| e.len() > 0) {
            event_loop.exit();
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
                }
            }
            WindowEvent::KeyboardInput { event, is_synthetic: false, .. } if is_main => {
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
