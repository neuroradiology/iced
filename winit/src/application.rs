use crate::{
    conversion,
    input::{keyboard, mouse},
    window, Cache, Clipboard, Command, Debug, Element, Event, Executor, Mode,
    MouseCursor, Proxy, Runtime, Settings, Size, Subscription, UserInterface,
};

/// An interactive, native cross-platform application.
///
/// This trait is the main entrypoint of Iced. Once implemented, you can run
/// your GUI application by simply calling [`run`](#method.run). It will run in
/// its own window.
///
/// An [`Application`](trait.Application.html) can execute asynchronous actions
/// by returning a [`Command`](struct.Command.html) in some of its methods.
pub trait Application: Sized {
    /// The graphics backend to use to draw the [`Application`].
    ///
    /// [`Application`]: trait.Application.html
    type Backend: window::Backend;

    /// The [`Executor`] that will run commands and subscriptions.
    ///
    /// [`Executor`]: trait.Executor.html
    type Executor: Executor;

    /// The type of __messages__ your [`Application`] will produce.
    ///
    /// [`Application`]: trait.Application.html
    type Message: std::fmt::Debug + Send;

    /// Initializes the [`Application`].
    ///
    /// Here is where you should return the initial state of your app.
    ///
    /// Additionally, you can return a [`Command`](struct.Command.html) if you
    /// need to perform some async action in the background on startup. This is
    /// useful if you want to load state from a file, perform an initial HTTP
    /// request, etc.
    ///
    /// [`Application`]: trait.Application.html
    fn new() -> (Self, Command<Self::Message>);

    /// Returns the current title of the [`Application`].
    ///
    /// This title can be dynamic! The runtime will automatically update the
    /// title of your application when necessary.
    ///
    /// [`Application`]: trait.Application.html
    fn title(&self) -> String;

    /// Handles a __message__ and updates the state of the [`Application`].
    ///
    /// This is where you define your __update logic__. All the __messages__,
    /// produced by either user interactions or commands, will be handled by
    /// this method.
    ///
    /// Any [`Command`] returned will be executed immediately in the background.
    ///
    /// [`Application`]: trait.Application.html
    /// [`Command`]: struct.Command.html
    fn update(&mut self, message: Self::Message) -> Command<Self::Message>;

    /// Returns the event `Subscription` for the current state of the
    /// application.
    ///
    /// The messages produced by the `Subscription` will be handled by
    /// [`update`](#tymethod.update).
    ///
    /// A `Subscription` will be kept alive as long as you keep returning it!
    fn subscription(&self) -> Subscription<Self::Message>;

    /// Returns the widgets to display in the [`Application`].
    ///
    /// These widgets can produce __messages__ based on user interaction.
    ///
    /// [`Application`]: trait.Application.html
    fn view(
        &mut self,
    ) -> Element<'_, Self::Message, <Self::Backend as window::Backend>::Renderer>;

    /// Returns the current [`Application`] mode.
    ///
    /// The runtime will automatically transition your application if a new mode
    /// is returned.
    ///
    /// By default, an application will run in windowed mode.
    ///
    /// [`Application`]: trait.Application.html
    fn mode(&self) -> Mode {
        Mode::Windowed
    }

    /// Runs the [`Application`].
    ///
    /// This method will take control of the current thread and __will NOT
    /// return__.
    ///
    /// It should probably be that last thing you call in your `main` function.
    ///
    /// [`Application`]: trait.Application.html
    fn run(
        settings: Settings,
        backend_settings: <Self::Backend as window::Backend>::Settings,
    ) where
        Self: 'static,
    {
        use window::Backend as _;
        use winit::{
            event::{self, WindowEvent},
            event_loop::{ControlFlow, EventLoop},
            window::WindowBuilder,
        };

        let mut debug = Debug::new();

        debug.startup_started();
        let event_loop = EventLoop::with_user_event();
        let mut external_messages = Vec::new();

        let mut runtime = {
            let executor = Self::Executor::new().expect("Create executor");

            Runtime::new(executor, Proxy::new(event_loop.create_proxy()))
        };

        let (mut application, init_command) = Self::new();
        runtime.spawn(init_command);

        let subscription = application.subscription();
        runtime.track(subscription);

        let mut title = application.title();
        let mut mode = application.mode();

        let window = {
            let mut window_builder = WindowBuilder::new();

            let (width, height) = settings.window.size;

            window_builder = window_builder
                .with_title(&title)
                .with_inner_size(winit::dpi::LogicalSize { width, height })
                .with_resizable(settings.window.resizable)
                .with_decorations(settings.window.decorations)
                .with_fullscreen(conversion::fullscreen(
                    event_loop.primary_monitor(),
                    mode,
                ));

            #[cfg(target_os = "windows")]
            {
                use winit::platform::windows::WindowBuilderExtWindows;

                if let Some(parent) = settings.window.platform_specific.parent {
                    window_builder = window_builder.with_parent_window(parent);
                }
            }

            window_builder.build(&event_loop).expect("Open window")
        };

        let mut size = Size::new(window.inner_size(), window.scale_factor());
        let mut resized = false;

        let clipboard = Clipboard::new(&window);
        let (mut backend, mut renderer) = Self::Backend::new(backend_settings);

        let surface = backend.create_surface(&window);

        let mut swap_chain = {
            let physical_size = size.physical();

            backend.create_swap_chain(
                &surface,
                physical_size.width,
                physical_size.height,
            )
        };

        let user_interface = build_user_interface(
            &mut application,
            Cache::default(),
            &mut renderer,
            size.logical(),
            &mut debug,
        );

        debug.draw_started();
        let mut primitive = user_interface.draw(&mut renderer);
        debug.draw_finished();

        let mut cache = Some(user_interface.into_cache());
        let mut events = Vec::new();
        let mut mouse_cursor = MouseCursor::OutOfBounds;
        let mut modifiers = winit::event::ModifiersState::default();
        debug.startup_finished();

        window.request_redraw();

        event_loop.run(move |event, _, control_flow| match event {
            event::Event::MainEventsCleared => {
                if events.is_empty() && external_messages.is_empty() && !resized
                {
                    return;
                }

                // TODO: We should be able to keep a user interface alive
                // between events once we remove state references.
                //
                // This will allow us to rebuild it only when a message is
                // handled.
                let mut user_interface = build_user_interface(
                    &mut application,
                    cache.take().unwrap(),
                    &mut renderer,
                    size.logical(),
                    &mut debug,
                );

                debug.event_processing_started();
                events
                    .iter()
                    .cloned()
                    .for_each(|event| runtime.broadcast(event));

                let mut messages = user_interface.update(
                    &renderer,
                    clipboard
                        .as_ref()
                        .map(|c| c as &dyn iced_native::Clipboard),
                    events.drain(..),
                );
                messages.extend(external_messages.drain(..));
                debug.event_processing_finished();

                if messages.is_empty() {
                    debug.draw_started();
                    primitive = user_interface.draw(&mut renderer);
                    debug.draw_finished();

                    cache = Some(user_interface.into_cache());
                } else {
                    // When there are messages, we are forced to rebuild twice
                    // for now :^)
                    let temp_cache = user_interface.into_cache();

                    for message in messages {
                        log::debug!("Updating");

                        debug.log_message(&message);

                        debug.update_started();
                        let command =
                            runtime.enter(|| application.update(message));
                        runtime.spawn(command);
                        debug.update_finished();
                    }

                    let subscription =
                        runtime.enter(|| application.subscription());
                    runtime.track(subscription);

                    // Update window title
                    let new_title = application.title();

                    if title != new_title {
                        window.set_title(&new_title);

                        title = new_title;
                    }

                    // Update window mode
                    let new_mode = application.mode();

                    if mode != new_mode {
                        window.set_fullscreen(conversion::fullscreen(
                            window.current_monitor(),
                            new_mode,
                        ));

                        mode = new_mode;
                    }

                    let user_interface = build_user_interface(
                        &mut application,
                        temp_cache,
                        &mut renderer,
                        size.logical(),
                        &mut debug,
                    );

                    debug.draw_started();
                    primitive = user_interface.draw(&mut renderer);
                    debug.draw_finished();

                    cache = Some(user_interface.into_cache());
                }

                window.request_redraw();
            }
            event::Event::UserEvent(message) => {
                external_messages.push(message);
            }
            event::Event::RedrawRequested(_) => {
                debug.render_started();

                if resized {
                    let physical_size = size.physical();

                    swap_chain = backend.create_swap_chain(
                        &surface,
                        physical_size.width,
                        physical_size.height,
                    );

                    resized = false;
                }

                let new_mouse_cursor = backend.draw(
                    &mut renderer,
                    &mut swap_chain,
                    &primitive,
                    size.scale_factor(),
                    &debug.overlay(),
                );

                debug.render_finished();

                if new_mouse_cursor != mouse_cursor {
                    window.set_cursor_icon(conversion::mouse_cursor(
                        new_mouse_cursor,
                    ));

                    mouse_cursor = new_mouse_cursor;
                }

                // TODO: Handle animations!
                // Maybe we can use `ControlFlow::WaitUntil` for this.
            }
            event::Event::WindowEvent {
                event: window_event,
                ..
            } => match window_event {
                WindowEvent::Resized(new_size) => {
                    size = Size::new(new_size, size.scale_factor());

                    events.push(Event::Window(window::Event::Resized {
                        width: size.logical().width.round() as u32,
                        height: size.logical().height.round() as u32,
                    }));

                    resized = true;
                }
                WindowEvent::CloseRequested => {
                    *control_flow = ControlFlow::Exit;
                }
                WindowEvent::CursorMoved { position, .. } => {
                    let position =
                        position.to_logical::<f64>(size.scale_factor());

                    events.push(Event::Mouse(mouse::Event::CursorMoved {
                        x: position.x as f32,
                        y: position.y as f32,
                    }));
                }
                WindowEvent::MouseInput { button, state, .. } => {
                    events.push(Event::Mouse(mouse::Event::Input {
                        button: conversion::mouse_button(button),
                        state: conversion::button_state(state),
                    }));
                }
                WindowEvent::MouseWheel { delta, .. } => match delta {
                    winit::event::MouseScrollDelta::LineDelta(
                        delta_x,
                        delta_y,
                    ) => {
                        events.push(Event::Mouse(
                            mouse::Event::WheelScrolled {
                                delta: mouse::ScrollDelta::Lines {
                                    x: delta_x,
                                    y: delta_y,
                                },
                            },
                        ));
                    }
                    winit::event::MouseScrollDelta::PixelDelta(position) => {
                        events.push(Event::Mouse(
                            mouse::Event::WheelScrolled {
                                delta: mouse::ScrollDelta::Pixels {
                                    x: position.x as f32,
                                    y: position.y as f32,
                                },
                            },
                        ));
                    }
                },
                WindowEvent::ReceivedCharacter(c)
                    if !is_private_use_character(c) =>
                {
                    events.push(Event::Keyboard(
                        keyboard::Event::CharacterReceived(c),
                    ));
                }
                WindowEvent::KeyboardInput {
                    input:
                        winit::event::KeyboardInput {
                            virtual_keycode: Some(virtual_keycode),
                            state,
                            ..
                        },
                    ..
                } => {
                    match (virtual_keycode, state) {
                        (
                            winit::event::VirtualKeyCode::F12,
                            winit::event::ElementState::Pressed,
                        ) => debug.toggle(),
                        _ => {}
                    }

                    events.push(Event::Keyboard(keyboard::Event::Input {
                        key_code: conversion::key_code(virtual_keycode),
                        state: conversion::button_state(state),
                        modifiers: conversion::modifiers_state(modifiers),
                    }));
                }
                WindowEvent::HoveredFile(path) => {
                    events
                        .push(Event::Window(window::Event::FileHovered(path)));
                }
                WindowEvent::DroppedFile(path) => {
                    events
                        .push(Event::Window(window::Event::FileDropped(path)));
                }
                WindowEvent::HoveredFileCancelled => {
                    events.push(Event::Window(window::Event::FilesHoveredLeft));
                }
                WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                    size = Size::new(size.physical(), scale_factor);
                }
                _ => {}
            },
            event::Event::DeviceEvent {
                event: event::DeviceEvent::ModifiersChanged(new_modifiers),
                ..
            } => {
                modifiers = new_modifiers;
            }
            _ => {
                *control_flow = ControlFlow::Wait;
            }
        })
    }
}

fn build_user_interface<'a, A: Application>(
    application: &'a mut A,
    cache: Cache,
    renderer: &mut <A::Backend as window::Backend>::Renderer,
    size: winit::dpi::LogicalSize<f64>,
    debug: &mut Debug,
) -> UserInterface<'a, A::Message, <A::Backend as window::Backend>::Renderer> {
    debug.view_started();
    let view = application.view();
    debug.view_finished();

    debug.layout_started();
    let user_interface = UserInterface::build(
        view,
        iced_native::Size::new(
            size.width.round() as f32,
            size.height.round() as f32,
        ),
        cache,
        renderer,
    );
    debug.layout_finished();

    user_interface
}

// As defined in: http://www.unicode.org/faq/private_use.html
fn is_private_use_character(c: char) -> bool {
    match c {
        '\u{E000}'..='\u{F8FF}'
        | '\u{F0000}'..='\u{FFFFD}'
        | '\u{100000}'..='\u{10FFFD}' => true,
        _ => false,
    }
}
