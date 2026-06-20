mod audio;
mod custom_event;
mod java;
mod keycodes;
mod navigator;
mod seer2;
mod trace;
mod ui;

use custom_event::RuffleEvent;

use jni::{
    objects::{JObject, JString},
    sys::{self, jint, jobject},
    JNIEnv, JavaVM,
};
use keycodes::{android_key_event_to_ruffle_key_descriptor, key_tag_to_key_descriptor};
use std::any::Any;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::sync::{mpsc, MutexGuard};
use std::time::Duration;
use std::{
    panic,
    sync::{Arc, Mutex},
    thread,
    time::Instant,
};
use wgpu::rwh::{AndroidDisplayHandle, HasWindowHandle, RawDisplayHandle};

use android_activity::input::{InputEvent, KeyAction, MotionAction};
use android_activity::{AndroidApp, AndroidAppWaker, InputStatus, MainEvent, PollEvent};
use backtrace::Backtrace;
use jni::objects::JClass;

use audio::AAudioAudioBackend;
use url::Url;

use ruffle_common::duration::FloatDuration;
use ruffle_core::{
    backend::navigator::{FetchReason, OwnedFuture, Request},
    events::{LogicalKey, MouseButton, PlayerEvent},
    font::DefaultFont,
    tag_utils::SwfMovie,
    Player, PlayerBuilder, StageAlign, StageScaleMode, ViewportDimensions,
};
use ruffle_frontend_utils::backends::storage::DiskStorageBackend;
use ruffle_frontend_utils::content::PlayingContent;
use ruffle_frontend_utils::{
    backends::navigator::{ExternalNavigatorBackend, FutureSpawner},
    content::ContentDescriptor,
};

use crate::navigator::{AndroidNavigatorBackend, AndroidNavigatorInterface};
use crate::trace::FileLogBackend;
use crate::ui::AndroidUiBackend;
use java::JavaInterface;
use ruffle_render_wgpu::{backend::WgpuRenderBackend, target::SwapChainTarget};

/// A unique identifier for a given `Player` instance.
/// Used to track which player any currently executing future is bound to.
#[derive(Copy, Clone, Eq, PartialEq)]
struct PlayerId(i64);

impl PlayerId {
    fn new() -> Self {
        use std::sync::atomic::{AtomicI64, Ordering};

        static NEXT: AtomicI64 = AtomicI64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        assert!(id >= 0, "PlayerId overflowed!");
        Self(id)
    }
}

/// A `Player`-bound future that is currently running.
pub struct PlayerRunnable(async_task::Runnable<PlayerId>);

/// Represents a current Player and any associated state with that player,
/// which may be lost when this Player is closed (dropped)
struct ActivePlayer {
    id: PlayerId,
    player: Arc<Mutex<Player>>,
}

#[derive(Clone, Copy)]
enum RenderBackendPreference {
    Auto,
    Vulkan,
    OpenGl,
}

impl RenderBackendPreference {
    fn from_key(key: &str) -> Self {
        match key {
            "auto" => Self::Auto,
            "opengl" => Self::OpenGl,
            _ => Self::Vulkan,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Vulkan => "vulkan",
            Self::OpenGl => "opengl",
        }
    }

    fn backends(self) -> wgpu::Backends {
        match self {
            Self::Auto => wgpu::Backends::PRIMARY,
            Self::Vulkan => wgpu::Backends::VULKAN,
            Self::OpenGl => wgpu::Backends::GL,
        }
    }
}

#[derive(Clone)]
pub struct EventSender {
    sender: Sender<RuffleEvent>,
    waker: AndroidAppWaker,
}

impl EventSender {
    pub fn send(&self, event: RuffleEvent) {
        if self.sender.send(event).is_ok() {
            self.waker.wake();
        }
    }
}

/// A bare-bones executor that schedules tasks on the winit event loop.
struct AndroidExecutor {
    event_loop: EventSender,
    player_id: PlayerId,
}

impl<E: std::error::Error + 'static> FutureSpawner<E> for AndroidExecutor {
    fn spawn(&self, future: OwnedFuture<(), E>) {
        // Discard any errors.
        let future = async {
            if let Err(e) = future.await {
                tracing::error!("Async error: {}", e);
            }
        };

        let event_loop = self.event_loop.clone();
        let scheduler = move |task| {
            let event = RuffleEvent::TaskPoll(PlayerRunnable(task));
            event_loop.send(event)
        };

        let (runnable, task) = async_task::Builder::new()
            .metadata(self.player_id)
            .spawn_local(|_| future, scheduler);

        // The future should run in the background.
        task.detach();
        // Immediately schedule the future to be polled for the first time.
        runnable.schedule();
    }
}

fn create_active_player(
    app: &AndroidApp,
    window: &ndk::native_window::NativeWindow,
    server: &seer2::HttpServer,
    event_loop: EventSender,
    android_storage_dir: &Path,
    trace_output: Option<&Path>,
    render_backend: RenderBackendPreference,
    render_scale: f64,
) -> ActivePlayer {
    let dimensions = viewport_dimensions(app, window, render_scale);
    let renderer = unsafe {
        WgpuRenderBackend::for_window_unsafe(
            wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: RawDisplayHandle::Android(AndroidDisplayHandle::new()),
                raw_window_handle: window.window_handle().unwrap().into(),
            },
            (dimensions.width, dimensions.height),
            render_backend.backends(),
            wgpu::PowerPreference::HighPerformance,
        )
        .unwrap()
    };
    let movie_url = server.movie_url();
    let movie_url_parsed = Url::parse(&movie_url).unwrap();
    let player_id = PlayerId::new();

    let future_spawner = AndroidExecutor {
        event_loop,
        player_id,
    };

    let navigator = AndroidNavigatorBackend::new(ExternalNavigatorBackend::new(
        movie_url_parsed.clone(),
        None,
        None,
        future_spawner,
        None,
        false,
        Default::default(),
        ruffle_core::backend::navigator::SocketMode::Allow,
        Rc::new(PlayingContent::DirectFile(ContentDescriptor::new_remote(
            movie_url_parsed,
        ))),
        AndroidNavigatorInterface,
    ));

    let active_player = ActivePlayer {
        id: player_id,
        player: PlayerBuilder::new()
            .with_renderer(renderer)
            .with_audio(AAudioAudioBackend::new().unwrap())
            .with_storage(Box::new(DiskStorageBackend::new(
                android_storage_dir.to_path_buf(),
            )))
            .with_ui(AndroidUiBackend::new())
            .with_navigator(navigator)
            .with_log(FileLogBackend::new(trace_output))
            .with_video(ruffle_video_software::backend::SoftwareVideoBackend::new())
            .with_letterbox(ruffle_core::config::Letterbox::On)
            .with_align(StageAlign::empty(), true)
            .with_scale_mode(StageScaleMode::ShowAll, true)
            .with_fullscreen(true)
            .build(),
    };

    {
        let mut player_lock = active_player.player.lock().unwrap();
        set_android_default_fonts(&mut player_lock);
        player_lock.fetch_root_movie(movie_url, Vec::new(), Box::new(|_| {}));
        player_lock.set_is_playing(true);
        player_lock.set_letterbox(ruffle_core::config::Letterbox::On);
        player_lock.set_viewport_dimensions(dimensions);
    }

    active_player
}

fn load_replacement_root_movie<'gc>(
    uc: &ruffle_core::context::UpdateContext<'gc>,
    movie_url: String,
) -> OwnedFuture<(), ruffle_core::loader::Error> {
    let player = uc.player_handle();

    Box::pin(async move {
        let fetch = player
            .lock()
            .unwrap()
            .fetch(Request::get(movie_url), FetchReason::LoadSwf);
        let response = fetch.await.map_err(|error| {
            player
                .lock()
                .unwrap()
                .ui()
                .display_root_movie_download_failed_message(false, error.error.to_string());
            error.error
        })?;
        let swf_url = response.url().into_owned();
        let body = response.body().await.map_err(|error| {
            player
                .lock()
                .unwrap()
                .ui()
                .display_root_movie_download_failed_message(true, error.to_string());
            error
        })?;

        let spoofed_or_swf_url = player
            .lock()
            .unwrap()
            .spoofed_url()
            .map(|url| url.to_string())
            .unwrap_or(swf_url);

        let movie = SwfMovie::from_data(&body, spoofed_or_swf_url, None).map_err(|error| {
            player
                .lock()
                .unwrap()
                .ui()
                .display_root_movie_download_failed_message(true, error.to_string());
            ruffle_core::loader::Error::InvalidSwf(error)
        })?;

        let mut player_lock = player.lock().unwrap();
        player_lock.set_is_playing(false);
        player_lock.mutate_with_update_context(|uc| {
            uc.replace_root_movie(movie);
        });
        player_lock.set_is_playing(true);
        Ok(())
    })
}

fn recreate_player_surface(
    app: &AndroidApp,
    active_player: &ActivePlayer,
    window: &ndk::native_window::NativeWindow,
    render_scale: f64,
    resume_playback: bool,
) -> bool {
    if window.width() <= 0 || window.height() <= 0 {
        log::warn!(
            "Skipping surface recreation for invalid window size: {} x {}",
            window.width(),
            window.height()
        );
        return false;
    }

    let raw_window_handle = match window.window_handle() {
        Ok(handle) => handle.into(),
        Err(error) => {
            log::warn!("Skipping surface recreation; window handle unavailable: {error:?}");
            return false;
        }
    };

    let mut player_lock = match active_player.player.lock() {
        Ok(player) => player,
        Err(error) => {
            log::warn!("Skipping surface recreation; player lock is poisoned: {error}");
            return false;
        }
    };

    let Some(renderer) =
        <dyn Any>::downcast_mut::<WgpuRenderBackend<SwapChainTarget>>(player_lock.renderer_mut())
    else {
        log::warn!("Skipping surface recreation; renderer backend type is unexpected");
        return false;
    };

    let result = unsafe {
        renderer.recreate_surface_unsafe(
            wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: RawDisplayHandle::Android(AndroidDisplayHandle::new()),
                raw_window_handle,
            },
            render_surface_size(window, render_scale),
        )
    };

    if let Err(error) = result {
        log::warn!("Failed to recreate render surface: {error:?}");
        return false;
    }

    player_lock.set_viewport_dimensions(viewport_dimensions(app, window, render_scale));
    if resume_playback {
        player_lock.set_is_playing(true);
    }

    true
}

fn set_audio_output(active_player: &ActivePlayer, enabled: bool) {
    let mut player_lock = match active_player.player.lock() {
        Ok(player) => player,
        Err(error) => {
            log::warn!("Skipping audio output change; player lock is poisoned: {error}");
            return;
        }
    };

    let Some(audio) = <dyn Any>::downcast_mut::<AAudioAudioBackend>(player_lock.audio_mut()) else {
        log::warn!("Skipping audio output change; audio backend type is unexpected");
        return;
    };

    if enabled {
        audio.resume_output();
    } else {
        audio.pause_output();
    }
}

#[tokio::main]
async fn run(app: AndroidApp) {
    let mut last_frame_time = Instant::now();
    let mut next_frame_time = Some(Instant::now());
    let mut fps_frame_count = 0_u32;
    let mut fps_last_time = Instant::now();
    let mut quit = false;
    let (sender, receiver) = mpsc::channel::<RuffleEvent>();
    let mut native_window: Option<ndk::native_window::NativeWindow> = None;
    let mut playerbox: Option<ActivePlayer> = None;
    let mut app_resumed = true;
    let sender = EventSender {
        sender,
        waker: app.create_waker(),
    };

    log::info!("Starting event loop...");
    let trace_output;
    let android_storage_dir;
    let android_app_data_dir;
    let render_backend;
    let render_scale;

    unsafe {
        let vm = JavaVM::from_raw(app.vm_as_ptr() as *mut sys::JavaVM).expect("JVM must exist");
        let activity = JObject::from_raw(app.activity_as_ptr() as jobject);
        let mut jni_env = vm.get_env().unwrap();
        trace_output = JavaInterface::get_trace_output(&mut jni_env, &activity);
        android_storage_dir = JavaInterface::get_android_data_storage_dir(&mut jni_env, &activity);
        android_app_data_dir = JavaInterface::get_android_app_data_dir(&mut jni_env, &activity);
        render_backend = RenderBackendPreference::from_key(&JavaInterface::get_render_backend(
            &mut jni_env,
            &activity,
        ));
        render_scale =
            sanitize_render_scale(JavaInterface::get_render_scale(&mut jni_env, &activity));
        let _ = jni_env.set_rust_field(activity, "eventLoopHandle", sender.clone());
    }
    log::info!("Render backend preference: {}", render_backend.key());
    log::info!("Render resolution scale: {:.2}", render_scale);

    let load_failure_notifier: seer2::LoadFailureNotifier =
        Arc::new(|message| show_load_failure(&message));
    let seer2_server = match seer2::HttpServer::start(
        android_app_data_dir,
        Some(load_failure_notifier),
    ) {
        Ok(server) => Some(server),
        Err(err) => {
            let message = format!(
                "\u{6e38}\u{620f}\u{52a0}\u{8f7d}\u{5931}\u{8d25}\u{ff0c}\u{8bf7}\u{68c0}\u{67e5}\u{7f51}\u{7edc}\u{540e}\u{91cd}\u{8bd5}\u{3002}\n{}",
                err
            );
            log::error!("Failed to start Seer2 local server: {}", err);
            show_load_failure(&message);
            None
        }
    };
    if let Some(server) = seer2_server.as_ref() {
        start_server_metrics_overlay(server.metrics());
    }

    while !quit {
        let mut needs_redraw = false;
        let can_tick = playerbox.is_some();
        let poll_timeout = if can_tick {
            next_frame_time
                .and_then(|next| next.checked_duration_since(last_frame_time))
                .unwrap_or_else(|| Duration::from_millis(100))
        } else {
            Duration::from_millis(100)
        };
        app.poll_events(Some(poll_timeout), |event| {
            match event {
                PollEvent::Main(event) => match event {
                    MainEvent::Destroy => {
                        if let Some(player) = playerbox.as_ref() {
                            if let Ok(mut player_lock) = player.player.lock() {
                                player_lock.flush_shared_objects();
                            }
                        }
                        quit = true;
                    }
                    MainEvent::WindowResized { .. } => {
                        if let Some(player) = playerbox.as_ref() {
                            if let Some(window) = native_window.as_ref() {
                                if let Ok(mut player_lock) = player.player.lock() {
                                    log::info!(
                                        "WindowResized: {} x {}",
                                        window.width(),
                                        window.height()
                                    );
                                    let dimensions =
                                        viewport_dimensions(&app, window, render_scale);
                                    player_lock.set_viewport_dimensions(dimensions);
                                    needs_redraw = true;
                                }
                            } else {
                                log::warn!("Ignoring WindowResized without a native window");
                            }
                        }
                    }
                    MainEvent::Resume { .. } => {
                        app_resumed = true;
                        last_frame_time = Instant::now();
                        next_frame_time = Some(last_frame_time);
                        if let Some(player) = playerbox.as_ref() {
                            if let Some(window) = native_window.as_ref() {
                                needs_redraw |= recreate_player_surface(
                                    &app,
                                    player,
                                    window,
                                    render_scale,
                                    false,
                                );
                                set_audio_output(player, true);
                            }
                        }
                    }
                    MainEvent::Pause | MainEvent::Stop => {
                        app_resumed = false;
                        if let Some(player) = playerbox.as_ref() {
                            set_audio_output(player, false);
                        }
                    }
                    MainEvent::InitWindow { .. } => {
                        native_window = app.native_window();
                        let Some(window) = native_window.as_ref() else {
                            log::warn!("Ignoring InitWindow because native_window is unavailable");
                            return;
                        };
                        let dimensions = viewport_dimensions(&app, window, render_scale);
                        log::info!(
                            "Init window: {} x {} -> render {} x {} (is existing: {})",
                            window.width(),
                            window.height(),
                            dimensions.width,
                            dimensions.height,
                            playerbox.is_some()
                        );

                        if let Some(activeplayer) = &playerbox {
                            last_frame_time = Instant::now();
                            next_frame_time = Some(last_frame_time);
                            needs_redraw |= recreate_player_surface(
                                &app,
                                activeplayer,
                                window,
                                render_scale,
                                app_resumed,
                            );
                            if app_resumed {
                                set_audio_output(activeplayer, true);
                            }
                        } else if let Some(seer2_server) = seer2_server.as_ref() {
                            playerbox = Some(create_active_player(
                                &app,
                                window,
                                seer2_server,
                                sender.clone(),
                                &android_storage_dir,
                                trace_output.as_deref(),
                                render_backend,
                                render_scale,
                            ));
                            last_frame_time = Instant::now();
                            next_frame_time = Some(Instant::now());

                            log::info!("MOVIE STARTED");
                        } else {
                            log::warn!("Seer2 local server is unavailable; player not started");
                        }
                    }
                    MainEvent::TerminateWindow { .. } => {
                        if let Some(player) = playerbox.as_ref() {
                            set_audio_output(player, false);
                        }
                        native_window = None;
                    }
                    MainEvent::InputAvailable => {
                        if let Ok(mut inputs) = app.input_events_iter() {
                            while inputs.next(|input| match input {
                                InputEvent::MotionEvent(event) => {
                                    let Some(window) = native_window.as_ref() else {
                                        return InputStatus::Unhandled;
                                    };
                                    let pointer = event.pointer_index();
                                    let pointer = event.pointer_at_index(pointer);
                                    let coords: (i32, i32) = get_loc_in_window();
                                    let mut x = pointer.x() as f64 - coords.0 as f64;
                                    let mut y = pointer.y() as f64 - coords.1 as f64;
                                    let Ok(view_size) = get_view_size() else {
                                        return InputStatus::Unhandled;
                                    };
                                    let (render_width, render_height) =
                                        render_surface_size(window, render_scale);
                                    x = x * render_width as f64 / view_size.0 as f64;
                                    y = y * render_height as f64 / view_size.1 as f64;
                                    let ruffle_event = match event.action() {
                                        MotionAction::Down
                                        | MotionAction::PointerDown
                                        | MotionAction::ButtonPress => {
                                            PlayerEvent::MouseDown {
                                                x,
                                                y,
                                                button: MouseButton::Left, // TODO
                                                index: None,               // TODO
                                            }
                                        }
                                        MotionAction::Up
                                        | MotionAction::PointerUp
                                        | MotionAction::ButtonRelease => {
                                            PlayerEvent::MouseUp {
                                                x,
                                                y,
                                                button: MouseButton::Left, // TODO
                                            }
                                        }
                                        MotionAction::Move => PlayerEvent::MouseMove { x, y },
                                        _ => return InputStatus::Unhandled,
                                    };

                                    if let Some(player) = playerbox.as_ref() {
                                        player.player.lock().unwrap().handle_event(ruffle_event);
                                    }

                                    InputStatus::Handled
                                }
                                InputEvent::KeyEvent(event) => {
                                    if let Some(player) = playerbox.as_ref() {
                                        let Some(key_descriptor) =
                                            android_key_event_to_ruffle_key_descriptor(event)
                                        else {
                                            return InputStatus::Unhandled;
                                        };
                                        let down;
                                        let ruffle_event = match event.action() {
                                            KeyAction::Down => {
                                                down = true;
                                                PlayerEvent::KeyDown {
                                                    key: key_descriptor,
                                                }
                                            }
                                            KeyAction::Up => {
                                                down = false;
                                                PlayerEvent::KeyUp {
                                                    key: key_descriptor,
                                                }
                                            }
                                            _ => return InputStatus::Unhandled,
                                        };
                                        player.player.lock().unwrap().handle_event(ruffle_event);

                                        // TODO: Use `KeyEvent.unicode_char` when it's available:
                                        // https://github.com/rust-mobile/android-activity/issues/183
                                        if down {
                                            if let LogicalKey::Character(c) =
                                                key_descriptor.logical_key
                                            {
                                                let event = PlayerEvent::TextInput { codepoint: c };
                                                player.player.lock().unwrap().handle_event(event);
                                            }
                                        };

                                        needs_redraw = true;
                                    }

                                    InputStatus::Handled
                                }
                                _ => InputStatus::Unhandled,
                            }) {}
                        }
                    }
                    _ => {} // Something else happened but it's probably not important for now.
                },
                PollEvent::Wake => {} // A task tried to wake us, we'll recv it below
                PollEvent::Timeout => {} // No events happened, we'll tick as normal below
                _ => {}               // Unknown future event
            }
        });

        match receiver.try_recv() {
            Err(_) => {}
            Ok(RuffleEvent::TaskPoll(task)) => {
                // Only run the task if it matches our current player;
                // otherwise it is stale, and should be cancelled (which
                // happens implicitly on drop).
                if let Some(player) = playerbox.as_ref() {
                    if *task.0.metadata() == player.id {
                        task.0.run();
                    }
                }
            }
            Ok(RuffleEvent::VirtualKeyEvent {
                down,
                key_descriptor,
            }) => {
                if let Some(player) = playerbox.as_ref() {
                    let event = if down {
                        PlayerEvent::KeyDown {
                            key: key_descriptor,
                        }
                    } else {
                        PlayerEvent::KeyUp {
                            key: key_descriptor,
                        }
                    };
                    player.player.lock().unwrap().handle_event(event);

                    if down {
                        // TODO: Add shift/capslock and pass in uppercase characters accordingly
                        if let LogicalKey::Character(c) = key_descriptor.logical_key {
                            let event = PlayerEvent::TextInput { codepoint: c };
                            player.player.lock().unwrap().handle_event(event);
                        }
                    }
                }
            }
            Ok(RuffleEvent::TextInput(text)) => {
                if let Some(player) = playerbox.as_ref() {
                    let mut player = player.player.lock().unwrap();
                    for codepoint in text.chars() {
                        player.handle_event(PlayerEvent::TextInput { codepoint });
                    }
                }
            }
            Ok(RuffleEvent::RunContextMenuCallback(index)) => {
                if let Some(player) = playerbox.as_ref() {
                    player
                        .player
                        .lock()
                        .unwrap()
                        .run_context_menu_callback(index);
                }
            }
            Ok(RuffleEvent::ClearContextMenu) => {
                if let Some(player) = playerbox.as_ref() {
                    player.player.lock().unwrap().clear_custom_menu_items();
                }
            }
            Ok(RuffleEvent::RequestContextMenu) => {
                if let Some(player) = playerbox.as_ref() {
                    log::warn!("preparing context menu!");
                    let items = player.player.lock().unwrap().prepare_context_menu();
                    let (jvm, activity) = get_jvm().unwrap();
                    let mut env = jvm.attach_current_thread().unwrap();
                    JavaInterface::show_context_menu(&mut env, &activity, &items);
                }
            }
            Ok(RuffleEvent::ReloadMovie) => {
                if let (Some(player), Some(server)) = (playerbox.as_ref(), seer2_server.as_ref()) {
                    log::info!("Replacing root Flash movie");
                    let movie_url = server.movie_url();
                    player
                        .player
                        .lock()
                        .unwrap()
                        .mutate_with_update_context(|uc| {
                            let future = load_replacement_root_movie(uc, movie_url);
                            uc.navigator.spawn_future(future);
                        });
                    needs_redraw = true;
                } else {
                    log::warn!("Ignoring Flash reload request before player is ready");
                }
            }
        }

        let new_time = Instant::now();
        let dt = new_time.duration_since(last_frame_time).as_micros();
        let can_tick = playerbox.is_some();
        let can_render = app_resumed && native_window.is_some();
        if can_tick && dt > 0 {
            last_frame_time = new_time;
            if let Some(player) = playerbox.as_ref() {
                if let Ok(mut player) = player.player.lock() {
                    player.tick(FloatDuration::from_millis(dt as f64 / 1000.0));
                    next_frame_time = Some(new_time + player.time_til_next_frame());
                    needs_redraw = player.needs_render();
                    let audio =
                        <dyn Any>::downcast_mut::<AAudioAudioBackend>(player.audio_mut()).unwrap();
                    audio.recreate_stream_if_needed();
                }
            } else {
                next_frame_time = None;
            }
        }

        let mut rendered_frame = false;
        if can_render && needs_redraw {
            if let Some(player) = playerbox.as_ref() {
                if let Ok(mut player) = player.player.lock() {
                    player.render();
                    rendered_frame = true;
                }
            }
        }
        if rendered_frame {
            fps_frame_count += 1;
        }
        let fps_elapsed = new_time.duration_since(fps_last_time);
        if fps_elapsed >= Duration::from_secs(1) {
            let fps = fps_frame_count as f64 / fps_elapsed.as_secs_f64();
            update_fps(&format!("FPS:{fps:.0}"));
            fps_frame_count = 0;
            fps_last_time = new_time;
        }
    }

    unsafe {
        let vm = JavaVM::from_raw(app.vm_as_ptr() as *mut sys::JavaVM).expect("JVM must exist");
        let activity = JObject::from_raw(app.activity_as_ptr() as jobject);
        // Ensure that we take the EventSender back, or we'll leak it
        let _: Result<EventSender, _> = vm
            .get_env()
            .unwrap()
            .take_rust_field(activity, "eventLoopHandle");
    }
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn Java_rs_ruffle_PlayerActivity_commitText(
    mut env: JNIEnv,
    this: JObject,
    text: JString,
) {
    let text: String = env
        .get_string(&text)
        .expect("Couldn't get java string!")
        .into();

    if text.is_empty() {
        return;
    }

    let event_loop: MutexGuard<EventSender> = env.get_rust_field(this, "eventLoopHandle").unwrap();
    event_loop.send(RuffleEvent::TextInput(text));
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn Java_rs_ruffle_PlayerActivity_keydown(
    mut env: JNIEnv,
    this: JObject,
    key_tag: JString,
) {
    let tag: String = env
        .get_string(&key_tag)
        .expect("Couldn't get java string!")
        .into();

    let event_loop: MutexGuard<EventSender> = env.get_rust_field(this, "eventLoopHandle").unwrap();
    if let Some(desc) = key_tag_to_key_descriptor(&tag) {
        event_loop.send(RuffleEvent::VirtualKeyEvent {
            down: true,
            key_descriptor: desc,
        });
    }
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn Java_rs_ruffle_PlayerActivity_keyup(
    mut env: JNIEnv,
    this: JObject,
    key_tag: JString,
) {
    let tag: String = env
        .get_string(&key_tag)
        .expect("Couldn't get java string!")
        .into();

    let event_loop: MutexGuard<EventSender> = env.get_rust_field(this, "eventLoopHandle").unwrap();
    if let Some(desc) = key_tag_to_key_descriptor(&tag) {
        event_loop.send(RuffleEvent::VirtualKeyEvent {
            down: false,
            key_descriptor: desc,
        });
    }
}

pub fn get_jvm<'a>() -> Result<(jni::JavaVM, JObject<'a>), Box<dyn std::error::Error>> {
    // Create a VM for executing Java calls
    let context = ndk_context::android_context();
    let activity = unsafe { JObject::from_raw(context.context().cast()) };
    let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) }?;

    Ok((vm, activity))
}

fn show_load_failure(message: &str) {
    match get_jvm() {
        Ok((jvm, activity)) => match jvm.attach_current_thread() {
            Ok(mut env) => JavaInterface::show_load_failure(&mut env, &activity, message),
            Err(err) => log::error!("Failed to attach JVM for load failure dialog: {}", err),
        },
        Err(err) => log::error!("Failed to get JVM for load failure dialog: {}", err),
    }
}

fn start_server_metrics_overlay(metrics: Arc<seer2::CacheMetrics>) {
    thread::spawn(move || {
        let mut last = String::new();
        loop {
            let text = metrics.snapshot_text();
            if text != last {
                update_server_metrics(&text);
                last = text;
            }
            thread::sleep(Duration::from_millis(500));
        }
    });
}

fn update_server_metrics(text: &str) {
    match get_jvm() {
        Ok((jvm, activity)) => match jvm.attach_current_thread() {
            Ok(mut env) => JavaInterface::update_server_metrics(&mut env, &activity, text),
            Err(err) => log::error!("Failed to attach JVM for server metrics: {}", err),
        },
        Err(err) => log::error!("Failed to get JVM for server metrics: {}", err),
    }
}

fn update_fps(text: &str) {
    match get_jvm() {
        Ok((jvm, activity)) => match jvm.attach_current_thread() {
            Ok(mut env) => JavaInterface::update_fps(&mut env, &activity, text),
            Err(err) => log::error!("Failed to attach JVM for FPS overlay: {}", err),
        },
        Err(err) => log::error!("Failed to get JVM for FPS overlay: {}", err),
    }
}

fn sanitize_render_scale(scale: f32) -> f64 {
    if scale.is_finite() {
        f64::from(scale).clamp(0.25, 1.0)
    } else {
        1.0
    }
}

fn viewport_dimensions(
    app: &AndroidApp,
    window: &ndk::native_window::NativeWindow,
    render_scale: f64,
) -> ViewportDimensions {
    let (width, height) = render_surface_size(window, render_scale);
    let scale_factor = app
        .config()
        .density()
        .map(|dpi| dpi as f64 / 160.0)
        .unwrap_or(1.0);

    ViewportDimensions {
        width,
        height,
        scale_factor: scale_factor * render_scale,
    }
}

fn render_surface_size(window: &ndk::native_window::NativeWindow, render_scale: f64) -> (u32, u32) {
    (
        scaled_window_dimension(window.width(), render_scale),
        scaled_window_dimension(window.height(), render_scale),
    )
}

fn scaled_window_dimension(value: i32, render_scale: f64) -> u32 {
    ((value.max(1) as f64 * render_scale).round() as u32).max(1)
}

fn set_android_default_fonts(player: &mut Player) {
    let names = vec![
        "Android CJK".to_string(),
        "宋体".to_string(),
        "SimSun".to_string(),
        "Arial".to_string(),
    ];
    player.set_default_font(DefaultFont::Sans, names.clone());
    player.set_default_font(DefaultFont::Serif, names.clone());
    player.set_default_font(DefaultFont::Typewriter, names.clone());
    player.set_default_font(DefaultFont::JapaneseGothic, names.clone());
    player.set_default_font(DefaultFont::JapaneseGothicMono, names.clone());
    player.set_default_font(DefaultFont::JapaneseMincho, names);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn Java_rs_ruffle_PlayerActivity_requestContextMenu(
    mut env: JNIEnv,
    this: JObject,
) {
    let event_loop: MutexGuard<EventSender> = env.get_rust_field(this, "eventLoopHandle").unwrap();
    event_loop.send(RuffleEvent::RequestContextMenu);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn Java_rs_ruffle_PlayerActivity_runContextMenuCallback(
    mut env: JNIEnv,
    this: JObject,
    index: jint,
) {
    let event_loop: MutexGuard<EventSender> = env.get_rust_field(this, "eventLoopHandle").unwrap();
    event_loop.send(RuffleEvent::RunContextMenuCallback(index as usize));
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn Java_rs_ruffle_PlayerActivity_clearContextMenu(
    mut env: JNIEnv,
    this: JObject,
) {
    let event_loop: MutexGuard<EventSender> = env.get_rust_field(this, "eventLoopHandle").unwrap();
    event_loop.send(RuffleEvent::ClearContextMenu);
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn Java_rs_ruffle_PlayerActivity_reloadGame(mut env: JNIEnv, this: JObject) {
    let event_loop: Result<MutexGuard<EventSender>, _> =
        env.get_rust_field(this, "eventLoopHandle");
    match event_loop {
        Ok(event_loop) => event_loop.send(RuffleEvent::ReloadMovie),
        Err(error) => log::warn!("Ignoring reload request before event loop is ready: {error:?}"),
    }
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn Java_rs_ruffle_PlayerActivity_nativeInit(
    mut env: JNIEnv,
    class: JClass,
    crash_callback: JObject,
) {
    let crash_callback = env.new_global_ref(crash_callback).unwrap();
    let jvm = env.get_java_vm().unwrap();

    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("ruffle")
            .with_filter(
                android_logger::FilterBuilder::new()
                    .parse("warn,ruffle=info")
                    .build(),
            ),
    );

    panic::set_hook(Box::new(move |info| {
        let backtrace = Backtrace::new();
        let thread = thread::current();
        let thread = thread.name().unwrap_or("<unnamed>");
        let message = match info.payload().downcast_ref::<&'static str>() {
            Some(s) => *s,
            None => match info.payload().downcast_ref::<String>() {
                Some(s) => &**s,
                None => "Box<Any>",
            },
        };

        let full = match info.location() {
            Some(location) => format!(
                "thread '{}' panicked at '{}': {}:{}\n{:?}",
                thread,
                message,
                location.file(),
                location.line(),
                backtrace
            ),
            None => format!(
                "thread '{}' panicked at '{}'\n{:?}",
                thread, message, backtrace
            ),
        };
        log::error!(target: "panic","{}", full);

        let mut env = jvm.attach_current_thread().unwrap();
        if env.exception_check().unwrap() {
            // There's a pending exception, java will discover this on their own
        } else {
            let java_message = env.new_string(full).unwrap();
            let crash_callback = env.new_global_ref(&crash_callback).unwrap();
            env.call_method(
                crash_callback,
                "onCrash",
                "(Ljava/lang/String;)V",
                &[(&java_message).into()],
            )
            .unwrap();
        }
    }));

    JavaInterface::init(&mut env, &class)
}

fn get_loc_in_window() -> (i32, i32) {
    let (jvm, activity) = get_jvm().unwrap();
    let mut env = jvm.attach_current_thread().unwrap();

    // no worky :(
    //ndk_glue::native_activity().show_soft_input(true);

    JavaInterface::get_loc_in_window(&mut env, &activity)
}

fn get_view_size() -> Result<(i32, i32), Box<dyn std::error::Error>> {
    let (jvm, activity) = get_jvm()?;
    let mut env = jvm.attach_current_thread()?;

    let width = JavaInterface::get_surface_width(&mut env, &activity);
    let height = JavaInterface::get_surface_height(&mut env, &activity);

    Ok((width, height))
}

#[no_mangle]
fn android_main(app: AndroidApp) {
    log::info!("Starting android_main...");
    run(app);
}
