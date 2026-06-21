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
    sys::{self, jboolean, jint, jobject},
    JNIEnv, JavaVM,
};
use keycodes::{android_key_event_to_ruffle_key_descriptor, key_tag_to_key_descriptor};
use std::any::Any;
use std::cell::Cell;
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
    events::{LogicalKey, MouseButton, PlayerEvent, TextControlCode},
    external::{ExternalInterfaceProvider, Value as ExternalValue},
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
use ruffle_render::quality::StageQuality;
use ruffle_render_wgpu::{backend::WgpuRenderBackend, target::SwapChainTarget};

thread_local! {
    #[allow(clippy::missing_const_for_thread_local)]
    static SUPPRESS_PANIC_CALLBACK: Cell<bool> = const { Cell::new(false) };
}

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

#[derive(Clone, Copy, PartialEq, Eq)]
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

fn stage_quality_from_key(key: &str) -> StageQuality {
    match key {
        "low" => StageQuality::Low,
        "medium" => StageQuality::Medium,
        "best" => StageQuality::Best,
        _ => StageQuality::High,
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

fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

fn catch_recoverable_panic<F, R>(f: F) -> Result<R, Box<dyn Any + Send>>
where
    F: FnOnce() -> R,
{
    SUPPRESS_PANIC_CALLBACK.with(|flag| {
        let previous = flag.replace(true);
        let result = panic::catch_unwind(panic::AssertUnwindSafe(f));
        flag.set(previous);
        result
    })
}

struct AndroidExternalInterfaceProvider;

impl ExternalInterfaceProvider for AndroidExternalInterfaceProvider {
    fn call_method(
        &self,
        _context: &mut ruffle_core::context::UpdateContext<'_>,
        name: &str,
        args: &[ExternalValue],
    ) -> ExternalValue {
        let args_json = external_values_to_json(args);
        let url = find_external_url(name, args);
        log::info!("ExternalInterface.call: {name} {args_json}");
        handle_external_interface_call(name, &args_json, url.as_deref());
        ExternalValue::Undefined
    }

    fn on_callback_available(&self, name: &str) {
        log::info!("ExternalInterface callback registered: {name}");
    }

    fn get_id(&self) -> Option<String> {
        Some("ruffle_android".to_string())
    }
}

fn external_values_to_json(values: &[ExternalValue]) -> String {
    let mut output = String::from("[");
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push_str(&external_value_to_json(value));
    }
    output.push(']');
    output
}

fn external_value_to_json(value: &ExternalValue) -> String {
    match value {
        ExternalValue::Undefined | ExternalValue::Null => "null".to_string(),
        ExternalValue::Bool(value) => value.to_string(),
        ExternalValue::Number(value) => value.to_string(),
        ExternalValue::String(value) => json_quote(value),
        ExternalValue::List(values) => external_values_to_json(values),
        ExternalValue::Object(values) => {
            let mut output = String::from("{");
            for (index, (key, value)) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&json_quote(key));
                output.push(':');
                output.push_str(&external_value_to_json(value));
            }
            output.push('}');
            output
        }
    }
}

fn json_quote(value: &str) -> String {
    let mut output = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character => output.push(character),
        }
    }
    output.push('"');
    output
}

fn find_external_url(name: &str, args: &[ExternalValue]) -> Option<String> {
    find_url_in_text(name).or_else(|| args.iter().find_map(find_external_value_url))
}

fn find_external_value_url(value: &ExternalValue) -> Option<String> {
    match value {
        ExternalValue::String(value) => find_url_in_text(value),
        ExternalValue::List(values) => values.iter().find_map(find_external_value_url),
        ExternalValue::Object(values) => values.iter().find_map(|(key, value)| {
            find_url_in_text(key).or_else(|| find_external_value_url(value))
        }),
        _ => None,
    }
}

fn find_url_in_text(text: &str) -> Option<String> {
    ["https://", "http://"].iter().find_map(|scheme| {
        let start = text.find(scheme)?;
        let tail = &text[start..];
        let end = tail
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '"' | '\'' | '<' | '>' | '[' | ']')
            })
            .unwrap_or(tail.len());
        let url = tail[..end]
            .trim_end_matches([',', ';', ')', '}'])
            .to_string();
        if url.is_empty() {
            None
        } else {
            Some(url)
        }
    })
}

struct ActivePlayerConfig<'a> {
    movie_url: &'a str,
    event_loop: EventSender,
    android_storage_dir: &'a Path,
    trace_output: Option<&'a Path>,
    render_backend: RenderBackendPreference,
    render_scale: f64,
    stage_quality: StageQuality,
}

fn create_active_player(
    app: &AndroidApp,
    window: &ndk::native_window::NativeWindow,
    config: ActivePlayerConfig<'_>,
) -> Result<ActivePlayer, String> {
    let ActivePlayerConfig {
        movie_url,
        event_loop,
        android_storage_dir,
        trace_output,
        render_backend,
        render_scale,
        stage_quality,
    } = config;
    let dimensions = viewport_dimensions_for_backend(app, window, render_backend, render_scale);
    let renderer = create_render_backend(window, dimensions, render_backend)?;
    let movie_url = movie_url.to_owned();
    let movie_url_parsed =
        Url::parse(&movie_url).map_err(|error| format!("Invalid movie URL: {error}"))?;
    let player_id = PlayerId::new();

    let future_spawner = AndroidExecutor {
        event_loop,
        player_id,
    };

    let navigator =
        AndroidNavigatorBackend::new(ExternalNavigatorBackend::new_with_environment_proxies(
            movie_url_parsed.clone(),
            None,
            None,
            future_spawner,
            None,
            false,
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
            .with_audio(
                AAudioAudioBackend::new()
                    .map_err(|error| format!("Failed to initialize audio: {error}"))?,
            )
            .with_storage(Box::new(DiskStorageBackend::new(
                android_storage_dir.to_path_buf(),
            )))
            .with_ui(AndroidUiBackend::new())
            .with_navigator(navigator)
            .with_external_interface(Box::new(AndroidExternalInterfaceProvider))
            .with_log(FileLogBackend::new(trace_output))
            .with_video(ruffle_video_software::backend::SoftwareVideoBackend::new())
            .with_letterbox(ruffle_core::config::Letterbox::On)
            .with_quality(stage_quality)
            .with_align(StageAlign::empty(), true)
            .with_scale_mode(StageScaleMode::ShowAll, true)
            .with_fullscreen(true)
            .build(),
    };

    {
        let mut player_lock = active_player
            .player
            .lock()
            .map_err(|error| format!("Failed to lock player during startup: {error}"))?;
        set_android_default_fonts(&mut player_lock);
        player_lock.fetch_root_movie(movie_url, Vec::new(), Box::new(|_| {}));
        player_lock.set_is_playing(true);
        player_lock.set_letterbox(ruffle_core::config::Letterbox::On);
        player_lock.set_quality(stage_quality);
        player_lock.set_viewport_dimensions(dimensions);
    }

    Ok(active_player)
}

fn create_render_backend(
    window: &ndk::native_window::NativeWindow,
    dimensions: ViewportDimensions,
    render_backend: RenderBackendPreference,
) -> Result<WgpuRenderBackend<SwapChainTarget>, String> {
    let mut last_error = String::from("unknown error");

    for attempt in 1..=5 {
        let raw_window_handle = match window.window_handle() {
            Ok(handle) => handle.into(),
            Err(error) => {
                last_error = format!("Window handle unavailable: {error:?}");
                break;
            }
        };

        let result = catch_recoverable_panic(|| unsafe {
            WgpuRenderBackend::for_window_unsafe(
                wgpu::SurfaceTargetUnsafe::RawHandle {
                    raw_display_handle: RawDisplayHandle::Android(AndroidDisplayHandle::new()),
                    raw_window_handle,
                },
                (dimensions.width, dimensions.height),
                render_backend.backends(),
                wgpu::PowerPreference::HighPerformance,
            )
        });

        match result {
            Ok(Ok(renderer)) => return Ok(renderer),
            Ok(Err(error)) => {
                last_error = format!("{error:?}");
                log::warn!("Render surface creation attempt {attempt} failed: {last_error}");
            }
            Err(payload) => {
                last_error = format!(
                    "panic while creating render surface: {}",
                    panic_payload_message(payload.as_ref())
                );
                log::warn!("Render surface creation attempt {attempt} panicked: {last_error}");
            }
        }

        thread::sleep(Duration::from_millis(120));
    }

    Err(last_error)
}

fn load_replacement_root_movie<'gc>(
    uc: &ruffle_core::context::UpdateContext<'gc>,
    movie_url: String,
) -> OwnedFuture<(), ruffle_core::loader::Error> {
    let player = uc.player_handle();

    Box::pin(async move {
        let fetch = match player.lock() {
            Ok(player) => player.fetch(Request::get(movie_url), FetchReason::LoadSwf),
            Err(error) => {
                log::warn!("Skipping Flash reload; player lock is poisoned: {error}");
                return Ok(());
            }
        };
        let response = fetch.await.map_err(|error| {
            if let Ok(player) = player.lock() {
                player
                    .ui()
                    .display_root_movie_download_failed_message(false, error.error.to_string());
            }
            error.error
        })?;
        let swf_url = response.url().into_owned();
        let body = response.body().await.inspect_err(|error| {
            if let Ok(player) = player.lock() {
                player
                    .ui()
                    .display_root_movie_download_failed_message(true, error.to_string());
            }
        })?;

        let spoofed_or_swf_url = match player.lock() {
            Ok(player) => player
                .spoofed_url()
                .map(|url| url.to_string())
                .unwrap_or(swf_url),
            Err(error) => {
                log::warn!("Using fetched URL during reload; player lock is poisoned: {error}");
                swf_url
            }
        };

        let movie = SwfMovie::from_data(&body, spoofed_or_swf_url, None).map_err(|error| {
            if let Ok(player) = player.lock() {
                player
                    .ui()
                    .display_root_movie_download_failed_message(true, error.to_string());
            }
            ruffle_core::loader::Error::InvalidSwf(error)
        })?;

        let Ok(mut player_lock) = player.lock() else {
            log::warn!("Skipping Flash reload apply; player lock is poisoned");
            return Ok(());
        };
        player_lock.set_is_playing(false);
        player_lock.mutate_with_update_context(|uc| {
            uc.replace_root_movie(movie);
        });
        player_lock.set_is_playing(true);
        set_no_movie_background_visible(false);
        Ok(())
    })
}

fn recreate_player_surface(
    app: &AndroidApp,
    active_player: &ActivePlayer,
    window: &ndk::native_window::NativeWindow,
    render_backend: RenderBackendPreference,
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

    let result = catch_recoverable_panic(|| unsafe {
        renderer.recreate_surface_unsafe(
            wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: RawDisplayHandle::Android(AndroidDisplayHandle::new()),
                raw_window_handle,
            },
            render_surface_size_for_backend(window, render_backend, render_scale),
        )
    });

    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            log::warn!("Failed to recreate render surface: {error:?}");
            return false;
        }
        Err(payload) => {
            log::warn!(
                "Panic while recreating render surface: {}",
                panic_payload_message(payload.as_ref())
            );
            return false;
        }
    }

    player_lock.set_viewport_dimensions(viewport_dimensions_for_backend(
        app,
        window,
        render_backend,
        render_scale,
    ));
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
    let swf_uri;
    let android_storage_dir;
    let android_app_data_dir;
    let render_backend;
    let render_scale;
    let mut stage_quality;
    let mut hover_click_mode = false;

    unsafe {
        let vm = JavaVM::from_raw(app.vm_as_ptr() as *mut sys::JavaVM).expect("JVM must exist");
        let activity = JObject::from_raw(app.activity_as_ptr() as jobject);
        let mut jni_env = vm.get_env().unwrap();
        trace_output = JavaInterface::get_trace_output(&mut jni_env, &activity);
        swf_uri = JavaInterface::get_swf_uri(&mut jni_env, &activity);
        android_storage_dir = JavaInterface::get_android_data_storage_dir(&mut jni_env, &activity);
        android_app_data_dir = JavaInterface::get_android_app_data_dir(&mut jni_env, &activity);
        render_backend = RenderBackendPreference::from_key(&JavaInterface::get_render_backend(
            &mut jni_env,
            &activity,
        ));
        render_scale =
            sanitize_render_scale(JavaInterface::get_render_scale(&mut jni_env, &activity));
        stage_quality =
            stage_quality_from_key(&JavaInterface::get_stage_quality(&mut jni_env, &activity));
        let _ = jni_env.set_rust_field(activity, "eventLoopHandle", sender.clone());
    }
    log::info!("Render backend preference: {}", render_backend.key());
    log::info!("Render resolution scale: {:.2}", render_scale);
    if render_backend == RenderBackendPreference::OpenGl && render_scale < 1.0 {
        log::warn!(
            "OpenGL ES backend requires native Android surface size; resolution scale will be ignored"
        );
    }
    log::info!("Stage quality: {stage_quality}");

    let external_movie_url = swf_uri.filter(|uri| !uri.is_empty());
    if let Some(uri) = external_movie_url.as_ref() {
        log::info!("Loading movie from Android intent: {uri}");
    }
    let seer2_server = if external_movie_url.is_some() {
        None
    } else {
        let load_failure_notifier: seer2::LoadFailureNotifier =
            Arc::new(|message| show_load_failure(&message));
        match seer2::HttpServer::start(android_app_data_dir, Some(load_failure_notifier)) {
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
        }
    };
    let root_movie_url =
        external_movie_url.or_else(|| seer2_server.as_ref().map(|server| server.movie_url()));
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
                                    let dimensions = viewport_dimensions_for_backend(
                                        &app,
                                        window,
                                        render_backend,
                                        render_scale,
                                    );
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
                                    render_backend,
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
                        let dimensions = viewport_dimensions_for_backend(
                            &app,
                            window,
                            render_backend,
                            render_scale,
                        );
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
                                render_backend,
                                render_scale,
                                app_resumed,
                            );
                            if app_resumed {
                                set_audio_output(activeplayer, true);
                            }
                        } else if let Some(movie_url) = root_movie_url.as_ref() {
                            match create_active_player(
                                &app,
                                window,
                                ActivePlayerConfig {
                                    movie_url,
                                    event_loop: sender.clone(),
                                    android_storage_dir: &android_storage_dir,
                                    trace_output: trace_output.as_deref(),
                                    render_backend,
                                    render_scale,
                                    stage_quality,
                                },
                            ) {
                                Ok(active_player) => {
                                    playerbox = Some(active_player);
                                    last_frame_time = Instant::now();
                                    next_frame_time = Some(Instant::now());

                                    log::info!("MOVIE STARTED");
                                }
                                Err(error) => {
                                    log::error!("Failed to create player: {error}");
                                    show_load_failure(&format!(
                                        "游戏渲染初始化失败，请切换渲染器后重试。\n{error}"
                                    ));
                                }
                            }
                        } else {
                            log::warn!("Root movie URL is unavailable; player not started");
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
                                        render_surface_size_for_backend(
                                            window,
                                            render_backend,
                                            render_scale,
                                        );
                                    x = x * render_width as f64 / view_size.0 as f64;
                                    y = y * render_height as f64 / view_size.1 as f64;
                                    let ruffle_event = match event.action() {
                                        MotionAction::Down
                                        | MotionAction::PointerDown
                                        | MotionAction::ButtonPress
                                            if hover_click_mode =>
                                        {
                                            PlayerEvent::MouseMove { x, y }
                                        }
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
                                        | MotionAction::ButtonRelease
                                            if hover_click_mode =>
                                        {
                                            PlayerEvent::MouseMove { x, y }
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
                                        if let Ok(mut player) = player.player.lock() {
                                            player.handle_event(ruffle_event);
                                        }
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
                                        if let Ok(mut player_lock) = player.player.lock() {
                                            player_lock.handle_event(ruffle_event);

                                            // TODO: Use `KeyEvent.unicode_char` when it's available:
                                            // https://github.com/rust-mobile/android-activity/issues/183
                                            if down {
                                                if let LogicalKey::Character(c) =
                                                    key_descriptor.logical_key
                                                {
                                                    let event =
                                                        PlayerEvent::TextInput { codepoint: c };
                                                    player_lock.handle_event(event);
                                                }
                                            }
                                        }

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

        for _ in 0..256 {
            let event = match receiver.try_recv() {
                Ok(event) => event,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            };

            match event {
                RuffleEvent::TaskPoll(task) => {
                    // Only run the task if it matches our current player;
                    // otherwise it is stale, and should be cancelled (which
                    // happens implicitly on drop).
                    if let Some(player) = playerbox.as_ref() {
                        if *task.0.metadata() == player.id {
                            task.0.run();
                        }
                    }
                }
                RuffleEvent::VirtualKeyEvent {
                    down,
                    key_descriptor,
                } => {
                    if let Some(player) = playerbox.as_ref() {
                        let Ok(mut player_lock) = player.player.lock() else {
                            log::warn!("Skipping virtual key event; player lock is poisoned");
                            continue;
                        };
                        let event = if down {
                            PlayerEvent::KeyDown {
                                key: key_descriptor,
                            }
                        } else {
                            PlayerEvent::KeyUp {
                                key: key_descriptor,
                            }
                        };
                        player_lock.handle_event(event);

                        if down {
                            // TODO: Add shift/capslock and pass in uppercase characters accordingly
                            if let LogicalKey::Character(c) = key_descriptor.logical_key {
                                let event = PlayerEvent::TextInput { codepoint: c };
                                player_lock.handle_event(event);
                            }
                        }
                    }
                }
                RuffleEvent::TextInput(text) => {
                    if let Some(player) = playerbox.as_ref() {
                        if let Ok(mut player) = player.player.lock() {
                            for codepoint in text.chars() {
                                player.handle_event(PlayerEvent::TextInput { codepoint });
                            }
                        } else {
                            log::warn!("Skipping text input; player lock is poisoned");
                        }
                    }
                }
                RuffleEvent::TextControl { code, repeat_count } => {
                    if let Some(player) = playerbox.as_ref() {
                        if let Ok(mut player) = player.player.lock() {
                            for _ in 0..repeat_count.max(1) {
                                if matches!(code, TextControlCode::Backspace) {
                                    if let Some(key) = key_tag_to_key_descriptor("BACKSPACE") {
                                        player.handle_event(PlayerEvent::KeyDown { key });
                                        player.handle_event(PlayerEvent::KeyUp { key });
                                    }
                                }
                                player.handle_event(PlayerEvent::TextControl { code });
                            }
                        } else {
                            log::warn!("Skipping text control input; player lock is poisoned");
                        }
                    }
                }
                RuffleEvent::SetStageQuality(quality) => {
                    stage_quality = quality;
                    if let Some(player) = playerbox.as_ref() {
                        if let Ok(mut player) = player.player.lock() {
                            player.set_quality(quality);
                            needs_redraw = true;
                        } else {
                            log::warn!("Skipping stage quality change; player lock is poisoned");
                        }
                    }
                }
                RuffleEvent::RunContextMenuCallback(index) => {
                    if let Some(player) = playerbox.as_ref() {
                        if let Ok(mut player) = player.player.lock() {
                            player.run_context_menu_callback(index);
                        } else {
                            log::warn!("Skipping context menu callback; player lock is poisoned");
                        }
                    }
                }
                RuffleEvent::ClearContextMenu => {
                    if let Some(player) = playerbox.as_ref() {
                        if let Ok(mut player) = player.player.lock() {
                            player.clear_custom_menu_items();
                        } else {
                            log::warn!("Skipping context menu clear; player lock is poisoned");
                        }
                    }
                }
                RuffleEvent::RequestContextMenu => {
                    if let Some(player) = playerbox.as_ref() {
                        log::warn!("preparing context menu!");
                        let items = match player.player.lock() {
                            Ok(mut player) => player.prepare_context_menu(),
                            Err(error) => {
                                log::warn!(
                                    "Skipping context menu; player lock is poisoned: {error}"
                                );
                                continue;
                            }
                        };
                        match get_jvm() {
                            Ok((jvm, activity)) => match jvm.attach_current_thread() {
                                Ok(mut env) => {
                                    JavaInterface::show_context_menu(&mut env, &activity, &items)
                                }
                                Err(error) => {
                                    log::warn!("Skipping context menu; JVM attach failed: {error}")
                                }
                            },
                            Err(error) => {
                                log::warn!("Skipping context menu; JVM unavailable: {error}")
                            }
                        }
                    }
                }
                RuffleEvent::ReloadMovie => {
                    if let (Some(player), Some(movie_url)) =
                        (playerbox.as_ref(), root_movie_url.as_ref())
                    {
                        log::info!("Replacing root Flash movie");
                        if let Ok(mut player) = player.player.lock() {
                            player.mutate_with_update_context(|uc| {
                                let future = load_replacement_root_movie(uc, movie_url.to_owned());
                                uc.navigator.spawn_future(future);
                            });
                            needs_redraw = true;
                        } else {
                            log::warn!("Ignoring Flash reload request; player lock is poisoned");
                        }
                    } else {
                        log::warn!("Ignoring Flash reload request before player is ready");
                    }
                }
                RuffleEvent::ExternalInterfaceCallback { name, payload } => {
                    if let Some(player) = playerbox.as_ref() {
                        match player.player.lock() {
                            Ok(mut player) => {
                                log::info!(
                                    "Calling ExternalInterface callback {name} with {payload}"
                                );
                                let _ = player.call_internal_interface(
                                    &name,
                                    [ExternalValue::String(payload)],
                                );
                            }
                            Err(error) => {
                                log::warn!(
                                    "Skipping ExternalInterface callback; player lock is poisoned: {error}"
                                );
                            }
                        }
                    } else {
                        log::warn!("Ignoring ExternalInterface callback before player is ready");
                    }
                }
                RuffleEvent::SetHoverClickMode(enabled) => {
                    hover_click_mode = enabled;
                    log::info!("Hover click mode: {enabled}");
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
                    if let Some(audio) =
                        <dyn Any>::downcast_mut::<AAudioAudioBackend>(player.audio_mut())
                    {
                        audio.recreate_stream_if_needed();
                    } else {
                        log::warn!("Skipping audio stream maintenance; audio backend mismatch");
                    }
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
    let text: String = match env.get_string(&text) {
        Ok(text) => text.into(),
        Err(error) => {
            log::warn!("Ignoring IME text; failed to read Java string: {error}");
            return;
        }
    };

    if text.is_empty() {
        return;
    }

    let event_loop: Result<MutexGuard<EventSender>, _> =
        env.get_rust_field(this, "eventLoopHandle");
    match event_loop {
        Ok(event_loop) => event_loop.send(RuffleEvent::TextInput(text)),
        Err(error) => log::warn!("Ignoring IME text before event loop is ready: {error:?}"),
    }
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn Java_rs_ruffle_PlayerActivity_deleteBackward(
    mut env: JNIEnv,
    this: JObject,
    repeat_count: jint,
) {
    let repeat_count = repeat_count.clamp(1, 8) as u32;
    let event_loop: Result<MutexGuard<EventSender>, _> =
        env.get_rust_field(this, "eventLoopHandle");
    match event_loop {
        Ok(event_loop) => event_loop.send(RuffleEvent::TextControl {
            code: TextControlCode::Backspace,
            repeat_count,
        }),
        Err(error) => log::warn!("Ignoring backspace before event loop is ready: {error:?}"),
    }
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn Java_rs_ruffle_PlayerActivity_setStageQuality(
    mut env: JNIEnv,
    this: JObject,
    key: JString,
) {
    let key: String = match env.get_string(&key) {
        Ok(key) => key.into(),
        Err(error) => {
            log::warn!("Ignoring stage quality change; failed to read Java string: {error}");
            return;
        }
    };

    let event_loop: Result<MutexGuard<EventSender>, _> =
        env.get_rust_field(this, "eventLoopHandle");
    match event_loop {
        Ok(event_loop) => {
            event_loop.send(RuffleEvent::SetStageQuality(stage_quality_from_key(&key)))
        }
        Err(error) => log::warn!("Ignoring stage quality change before event loop: {error:?}"),
    }
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn Java_rs_ruffle_PlayerActivity_keydown(
    mut env: JNIEnv,
    this: JObject,
    key_tag: JString,
) {
    let tag: String = match env.get_string(&key_tag) {
        Ok(tag) => tag.into(),
        Err(error) => {
            log::warn!("Ignoring keydown; failed to read Java string: {error}");
            return;
        }
    };

    let event_loop: Result<MutexGuard<EventSender>, _> =
        env.get_rust_field(this, "eventLoopHandle");
    match event_loop {
        Ok(event_loop) => {
            if let Some(desc) = key_tag_to_key_descriptor(&tag) {
                event_loop.send(RuffleEvent::VirtualKeyEvent {
                    down: true,
                    key_descriptor: desc,
                });
            }
        }
        Err(error) => log::warn!("Ignoring keydown before event loop is ready: {error:?}"),
    }
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn Java_rs_ruffle_PlayerActivity_keyup(
    mut env: JNIEnv,
    this: JObject,
    key_tag: JString,
) {
    let tag: String = match env.get_string(&key_tag) {
        Ok(tag) => tag.into(),
        Err(error) => {
            log::warn!("Ignoring keyup; failed to read Java string: {error}");
            return;
        }
    };

    let event_loop: Result<MutexGuard<EventSender>, _> =
        env.get_rust_field(this, "eventLoopHandle");
    match event_loop {
        Ok(event_loop) => {
            if let Some(desc) = key_tag_to_key_descriptor(&tag) {
                event_loop.send(RuffleEvent::VirtualKeyEvent {
                    down: false,
                    key_descriptor: desc,
                });
            }
        }
        Err(error) => log::warn!("Ignoring keyup before event loop is ready: {error:?}"),
    }
}

pub fn get_jvm<'a>() -> Result<(jni::JavaVM, JObject<'a>), Box<dyn std::error::Error>> {
    // Create a VM for executing Java calls
    let context = panic::catch_unwind(ndk_context::android_context).map_err(|payload| {
        let message = format!(
            "android context is not initialized: {}",
            panic_payload_message(payload.as_ref())
        );
        std::io::Error::other(message)
    })?;
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

pub(crate) fn open_web_login_url(url: &str) {
    match get_jvm() {
        Ok((jvm, activity)) => match jvm.attach_current_thread() {
            Ok(mut env) => JavaInterface::open_web_login(&mut env, &activity, url),
            Err(err) => log::error!("Failed to attach JVM for web login: {}", err),
        },
        Err(err) => log::error!("Failed to get JVM for web login: {}", err),
    }
}

fn handle_external_interface_call(name: &str, args: &str, url: Option<&str>) {
    match get_jvm() {
        Ok((jvm, activity)) => match jvm.attach_current_thread() {
            Ok(mut env) => {
                JavaInterface::handle_external_interface_call(&mut env, &activity, name, args, url)
            }
            Err(err) => log::error!("Failed to attach JVM for ExternalInterface: {}", err),
        },
        Err(err) => log::error!("Failed to get JVM for ExternalInterface: {}", err),
    }
}

fn set_no_movie_background_visible(visible: bool) {
    match get_jvm() {
        Ok((jvm, activity)) => match jvm.attach_current_thread() {
            Ok(mut env) => {
                JavaInterface::set_no_movie_background_visible(&mut env, &activity, visible)
            }
            Err(err) => log::error!("Failed to attach JVM for no-movie background: {}", err),
        },
        Err(err) => log::error!("Failed to get JVM for no-movie background: {}", err),
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
            Err(err) => log::debug!("Failed to attach JVM for server metrics: {}", err),
        },
        Err(err) => log::debug!("Failed to get JVM for server metrics: {}", err),
    }
}

fn update_fps(text: &str) {
    match get_jvm() {
        Ok((jvm, activity)) => match jvm.attach_current_thread() {
            Ok(mut env) => JavaInterface::update_fps(&mut env, &activity, text),
            Err(err) => log::debug!("Failed to attach JVM for FPS overlay: {}", err),
        },
        Err(err) => log::debug!("Failed to get JVM for FPS overlay: {}", err),
    }
}

fn sanitize_render_scale(scale: f32) -> f64 {
    if scale.is_finite() {
        f64::from(scale).clamp(0.25, 1.0)
    } else {
        1.0
    }
}

fn effective_render_scale(render_backend: RenderBackendPreference, render_scale: f64) -> f64 {
    match render_backend {
        RenderBackendPreference::OpenGl => 1.0,
        RenderBackendPreference::Auto | RenderBackendPreference::Vulkan => render_scale,
    }
}

fn viewport_dimensions_for_backend(
    app: &AndroidApp,
    window: &ndk::native_window::NativeWindow,
    render_backend: RenderBackendPreference,
    render_scale: f64,
) -> ViewportDimensions {
    viewport_dimensions(
        app,
        window,
        effective_render_scale(render_backend, render_scale),
    )
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

fn render_surface_size_for_backend(
    window: &ndk::native_window::NativeWindow,
    render_backend: RenderBackendPreference,
    render_scale: f64,
) -> (u32, u32) {
    render_surface_size(window, effective_render_scale(render_backend, render_scale))
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
    let event_loop: Result<MutexGuard<EventSender>, _> =
        env.get_rust_field(this, "eventLoopHandle");
    match event_loop {
        Ok(event_loop) => event_loop.send(RuffleEvent::RequestContextMenu),
        Err(error) => log::warn!("Ignoring context menu request before event loop: {error:?}"),
    }
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn Java_rs_ruffle_PlayerActivity_runContextMenuCallback(
    mut env: JNIEnv,
    this: JObject,
    index: jint,
) {
    let event_loop: Result<MutexGuard<EventSender>, _> =
        env.get_rust_field(this, "eventLoopHandle");
    match event_loop {
        Ok(event_loop) => event_loop.send(RuffleEvent::RunContextMenuCallback(index as usize)),
        Err(error) => log::warn!("Ignoring context menu callback before event loop: {error:?}"),
    }
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn Java_rs_ruffle_PlayerActivity_clearContextMenu(
    mut env: JNIEnv,
    this: JObject,
) {
    let event_loop: Result<MutexGuard<EventSender>, _> =
        env.get_rust_field(this, "eventLoopHandle");
    match event_loop {
        Ok(event_loop) => event_loop.send(RuffleEvent::ClearContextMenu),
        Err(error) => log::warn!("Ignoring context menu clear before event loop: {error:?}"),
    }
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
pub unsafe extern "C" fn Java_rs_ruffle_PlayerActivity_setHoverClickMode(
    mut env: JNIEnv,
    this: JObject,
    enabled: jboolean,
) {
    let event_loop: Result<MutexGuard<EventSender>, _> =
        env.get_rust_field(this, "eventLoopHandle");
    match event_loop {
        Ok(event_loop) => event_loop.send(RuffleEvent::SetHoverClickMode(enabled != 0)),
        Err(error) => {
            log::warn!("Ignoring hover click mode change before event loop is ready: {error:?}");
        }
    }
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn Java_rs_ruffle_PlayerActivity_externalInterfaceCallback(
    mut env: JNIEnv,
    this: JObject,
    name: JString,
    payload: JString,
) {
    let callback_name = match env.get_string(&name) {
        Ok(name) => name.to_string_lossy().to_string(),
        Err(error) => {
            log::warn!("Ignoring ExternalInterface callback with invalid name: {error}");
            return;
        }
    };
    let payload = match env.get_string(&payload) {
        Ok(payload) => payload.to_string_lossy().to_string(),
        Err(error) => {
            log::warn!("Ignoring ExternalInterface callback with invalid payload: {error}");
            return;
        }
    };
    let event_loop: Result<MutexGuard<EventSender>, _> =
        env.get_rust_field(this, "eventLoopHandle");
    match event_loop {
        Ok(event_loop) => event_loop.send(RuffleEvent::ExternalInterfaceCallback {
            name: callback_name,
            payload,
        }),
        Err(error) => {
            log::warn!("Ignoring ExternalInterface callback before event loop is ready: {error:?}");
        }
    }
}

#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn Java_rs_ruffle_PlayerActivity_nativeInit(
    mut env: JNIEnv,
    class: JClass,
    crash_callback: JObject,
) {
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

    let crash_callback = match env.new_global_ref(crash_callback) {
        Ok(callback) => callback,
        Err(error) => {
            log::error!("Failed to create crash callback global ref: {error}");
            JavaInterface::init(&mut env, &class);
            return;
        }
    };
    let jvm = match env.get_java_vm() {
        Ok(jvm) => jvm,
        Err(error) => {
            log::error!("Failed to get JavaVM for crash hook: {error}");
            JavaInterface::init(&mut env, &class);
            return;
        }
    };

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

        if SUPPRESS_PANIC_CALLBACK.with(Cell::get) {
            log::warn!("Suppressing crash callback for recoverable native panic");
            return;
        }

        let Ok(mut env) = jvm.attach_current_thread() else {
            log::error!("Failed to attach JVM while reporting native panic");
            return;
        };
        match env.exception_check() {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                log::error!("Failed to check pending Java exception: {error}");
                return;
            }
        }
        let Ok(java_message) = env.new_string(full) else {
            log::error!("Failed to allocate native panic message string");
            return;
        };
        if let Err(error) = env.call_method(
            crash_callback.as_obj(),
            "onCrash",
            "(Ljava/lang/String;)V",
            &[(&java_message).into()],
        ) {
            log::error!("Failed to call native panic callback: {error}");
        }
    }));

    JavaInterface::init(&mut env, &class)
}

fn get_loc_in_window() -> (i32, i32) {
    match get_jvm() {
        Ok((jvm, activity)) => match jvm.attach_current_thread() {
            Ok(mut env) => JavaInterface::get_loc_in_window(&mut env, &activity),
            Err(error) => {
                log::warn!("Failed to attach JVM for input coordinates: {error}");
                (0, 0)
            }
        },
        Err(error) => {
            log::warn!("Failed to get JVM for input coordinates: {error}");
            (0, 0)
        }
    }
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
