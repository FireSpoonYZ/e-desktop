use std::{
    collections::{BTreeMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use tauri::{Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use crate::{
    animation::{Placements, ScrollAnimation},
    config::{Config, ConfigFile, ShortcutAction},
    layout::Engine,
    model::{
        AppError, BackendAvailability, BackendStatus, Command, ErrorCode, NativeAction, Rect,
        Snapshot,
    },
    platform::Backend,
    rules::WindowRule,
    shortcuts::{Shortcuts, normalize_key},
};

const BAR_HEIGHT: f64 = 44.0;
const RAIL_WIDTH: f64 = 56.0;
const REFRESH_INTERVAL: Duration = Duration::from_millis(500);
type Reply = mpsc::SyncSender<Result<Snapshot, AppError>>;

#[derive(Clone, Copy)]
enum Surface {
    Overview,
    Commands,
}
impl Surface {
    fn label(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Commands => "commands",
        }
    }
    fn parse(value: &str) -> Result<Self, AppError> {
        match value {
            "overview" => Ok(Self::Overview),
            "commands" => Ok(Self::Commands),
            _ => Err(error(ErrorCode::InvalidCommand, "未知的界面入口。")),
        }
    }
}

enum Request {
    Command(Command, Option<Reply>),
    Show(Surface, Option<String>),
    Dismiss(Surface),
    Shortcut(String),
    Quit,
}

struct AppState {
    sender: mpsc::SyncSender<Request>,
    snapshot: Arc<Mutex<Snapshot>>,
    can_exit: AtomicBool,
}

fn error(code: ErrorCode, message: impl Into<String>) -> AppError {
    AppError {
        code,
        message: message.into(),
        window_id: None,
    }
}

fn send(state: &AppState, request: Request) -> Result<(), AppError> {
    state.sender.try_send(request).map_err(|_| {
        error(
            ErrorCode::BackendUnavailable,
            "窗口控制器暂时忙碌或已退出，请稍后重试。",
        )
    })
}

#[tauri::command]
fn get_snapshot(state: tauri::State<'_, AppState>) -> Snapshot {
    state
        .snapshot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

#[tauri::command]
async fn execute(
    command: Command,
    state: tauri::State<'_, AppState>,
) -> Result<Snapshot, AppError> {
    let (tx, rx) = mpsc::sync_channel(1);
    send(&state, Request::Command(command, Some(tx)))?;
    // Native calls stay on one dedicated thread, including non-Send AX objects.
    tauri::async_runtime::spawn_blocking(move || {
        rx.recv()
            .map_err(|_| error(ErrorCode::BackendUnavailable, "窗口控制器已停止。"))?
    })
    .await
    .map_err(|e| error(ErrorCode::BackendUnavailable, e.to_string()))?
}

#[tauri::command]
fn open_surface(
    surface: String,
    monitor_id: Option<String>,
    state: tauri::State<'_, AppState>,
) -> Result<(), AppError> {
    send(&state, Request::Show(Surface::parse(&surface)?, monitor_id))
}

#[tauri::command]
fn dismiss_surface(surface: String, state: tauri::State<'_, AppState>) -> Result<(), AppError> {
    send(&state, Request::Dismiss(Surface::parse(&surface)?))
}

#[tauri::command]
fn quit(state: tauri::State<'_, AppState>) -> Result<(), AppError> {
    send(&state, Request::Quit)
}

struct Controller {
    backend: Option<Backend>,
    engine: Engine,
    errors: Vec<AppError>,
    config_error: Option<AppError>,
    window_rules: Vec<WindowRule>,
    placements: Placements,
    animation: ScrollAnimation,
    animation_duration: Duration,
}

impl Controller {
    fn new() -> Self {
        let mut controller = Self {
            backend: None,
            engine: Engine::new(BackendStatus::default()),
            errors: vec![],
            config_error: None,
            window_rules: vec![],
            placements: Placements::new(),
            animation: ScrollAnimation::default(),
            animation_duration: Duration::from_millis(u64::from(
                Config::default().animation_duration_ms,
            )),
        };
        if let Err(e) = controller.connect() {
            controller.record(e);
        }
        controller
    }

    fn connect(&mut self) -> Result<(), AppError> {
        let backend = Backend::new()?;
        let mut engine = Engine::new(backend.status());
        engine.set_window_rules(self.window_rules.clone())?;
        self.engine = engine;
        self.backend = Some(backend);
        Ok(())
    }

    fn set_window_rules(&mut self, rules: Vec<WindowRule>) -> Result<(), AppError> {
        self.engine.set_window_rules(rules.clone())?;
        self.window_rules = rules;
        Ok(())
    }

    fn record(&mut self, issue: AppError) {
        if self.errors.last() != Some(&issue) {
            self.errors.push(issue);
            if self.errors.len() > 5 {
                self.errors.remove(0);
            }
        }
    }

    fn snapshot(&self, shortcuts: bool) -> Snapshot {
        let mut snapshot = self.engine.snapshot().clone();
        if let Some(backend) = &self.backend {
            snapshot.backend = backend.status();
        } else if let Some(issue) = self.errors.last() {
            snapshot.backend.availability = match issue.code {
                ErrorCode::PermissionRequired => BackendAvailability::PermissionRequired,
                ErrorCode::UnsupportedSession => BackendAvailability::UnsupportedSession,
                ErrorCode::NotImplemented => BackendAvailability::NotImplemented,
                _ => BackendAvailability::Unavailable,
            };
            snapshot.backend.message = issue.message.clone();
        }
        snapshot.backend.capabilities.global_shortcuts = shortcuts;
        snapshot.errors.extend(self.errors.clone());
        snapshot.errors.extend(self.config_error.clone());
        snapshot
    }

    fn refresh(&mut self, apply: bool) -> Result<(), AppError> {
        // Reconcile at its normal deadline, never defer it behind a stream of Scrolls.
        // Drop frames BEFORE enumeration (windows/monitors may have disappeared).
        // Tiled targets come from engine columns/viewport_x, not observed mid-frame rects;
        // reconciliation immediately applies that final layout, without stale frame writes.
        self.animation.cancel();
        let backend = self
            .backend
            .as_mut()
            .ok_or_else(|| error(ErrorCode::BackendUnavailable, "原生窗口后端尚未连接。"))?;
        let system = backend.enumerate();
        self.engine.set_backend(backend.status());
        let system = system?;
        let viewports = system
            .monitors
            .iter()
            .map(|monitor| {
                let top = (BAR_HEIGHT * monitor.scale_factor).round() as u32;
                let left = (RAIL_WIDTH * monitor.scale_factor).round() as u32;
                let area = monitor.work_area;
                (
                    monitor.id.clone(),
                    Rect {
                        x: area.x.saturating_add(left.min(area.width) as i32),
                        y: area.y.saturating_add(top.min(area.height) as i32),
                        width: area.width.saturating_sub(left).max(1),
                        height: area.height.saturating_sub(top).max(1),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        self.engine.set_viewports(viewports);
        let transition = self.engine.reconcile(system)?;
        self.placements.retain(|id, _| {
            transition
                .snapshot
                .windows
                .iter()
                .any(|w| &w.native.id == id)
        });
        if apply {
            // Observation must not steal focus from an open palette, menu or dialog.
            let actions: Vec<_> = transition
                .actions
                .into_iter()
                .filter(|a| !matches!(a, NativeAction::Focus { .. }))
                .collect();
            self.apply(&actions)?;
        }
        Ok(())
    }

    fn apply(&mut self, actions: &[NativeAction]) -> Result<(), AppError> {
        for action in actions {
            if let NativeAction::Placement {
                window_id,
                rect,
                clip,
                minimized,
            } = action
            {
                let same_plan = self.placements.get(window_id) == Some(&(*rect, *clip, *minimized));
                let native_matches = self
                    .engine
                    .snapshot()
                    .windows
                    .iter()
                    .find(|w| &w.native.id == window_id)
                    .is_some_and(|w| {
                        if *minimized {
                            w.native.minimized_by_manager
                        } else {
                            !w.native.minimized && w.native.rect == *rect
                        }
                    });
                if same_plan && native_matches {
                    continue;
                }
            }
            let result = self
                .backend
                .as_mut()
                .ok_or_else(|| error(ErrorCode::BackendUnavailable, "原生窗口后端尚未连接。"))?
                .apply(std::slice::from_ref(action));
            if let Err(issue) = result {
                // Focus/close refusal is not a reason to rearrange every other window.
                if matches!(
                    action,
                    NativeAction::Placement { .. } | NativeAction::Restore { .. }
                ) {
                    let _ = self.stop();
                }
                let _ = self.refresh(false);
                return Err(issue);
            }
            match action {
                NativeAction::Placement {
                    window_id,
                    rect,
                    clip,
                    minimized,
                } => {
                    self.placements
                        .insert(window_id.clone(), (*rect, *clip, *minimized));
                }
                NativeAction::Restore { window_id } => {
                    self.placements.remove(window_id);
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<(), AppError> {
        self.animation.cancel();
        self.placements.clear();
        // Always turn off automatic reflow, even when one native restore fails.
        let mut result = self.engine.dispatch(Command::Disable).map(|_| ());
        if let Some(backend) = self.backend.as_mut() {
            if let Err(issue) = backend.restore() {
                result = Err(issue);
            }
        }
        if let Err(issue) = &result {
            self.record(issue.clone());
        }
        result
    }

    fn command(&mut self, command: Command) -> Result<(), AppError> {
        if matches!(command, Command::Disable) {
            self.errors.clear();
            let result = self.stop();
            let _ = self.refresh(false);
            return result;
        }
        let scroll_monitor = match &command {
            Command::Scroll { monitor_id, .. } => Some(monitor_id.clone()),
            _ => None,
        };
        if scroll_monitor.is_none() && !matches!(command, Command::Refresh) {
            // Close returns no layout actions; finish the old target before it too.
            self.finish_animation()?;
        }
        if self.backend.is_none() {
            self.connect()?;
        }
        if let Some(backend) = &self.backend {
            self.engine.set_backend(backend.status());
        }
        if matches!(command, Command::Refresh) {
            self.errors.clear();
            return self.refresh(true);
        }
        if matches!(command, Command::Enable) {
            self.refresh(false)?;
        }
        let transition = match self.engine.dispatch(command) {
            Ok(transition) => transition,
            Err(issue) => {
                self.finish_animation()?;
                return Err(issue);
            }
        };
        let actions = if let Some(monitor_id) = scroll_monitor {
            self.animation.start(
                &transition.snapshot,
                &monitor_id,
                &self.placements,
                transition.actions,
                self.animation_duration,
                Instant::now(),
            )
        } else {
            transition.actions
        };
        self.apply(&actions)
    }

    fn finish_animation(&mut self) -> Result<(), AppError> {
        let actions = self.animation.cancel();
        self.apply(&actions)
    }

    fn animate(&mut self, now: Instant) -> Result<(), AppError> {
        let actions = self.animation.frame(now);
        self.apply(&actions)
    }
}

fn configure_window(window: &tauri::WebviewWindow, rect: Rect) -> Result<(), AppError> {
    window
        .set_position(PhysicalPosition::new(rect.x, rect.y))
        .and_then(|_| window.set_size(PhysicalSize::new(rect.width, rect.height)))
        .map_err(|e| error(ErrorCode::OperationDenied, e.to_string()))
}

fn control_label(surface: &str, index: usize) -> String {
    if index == 0 {
        surface.to_owned()
    } else {
        format!("{surface}-{index}")
    }
}

fn position_controls(app: &tauri::AppHandle, snapshot: &Snapshot) -> Result<(), AppError> {
    let mut labels = HashSet::new();
    for (index, monitor) in snapshot.monitors.iter().enumerate() {
        let work = monitor.monitor.work_area;
        let top = (BAR_HEIGHT * monitor.monitor.scale_factor).round() as u32;
        let left = (RAIL_WIDTH * monitor.monitor.scale_factor).round() as u32;
        for (surface, rect) in [
            (
                "topbar",
                Rect {
                    x: work.x,
                    y: work.y,
                    width: work.width,
                    height: top,
                },
            ),
            (
                "pagerail",
                Rect {
                    x: work.x,
                    y: work.y.saturating_add(top as i32),
                    width: left,
                    height: work.height.saturating_sub(top).max(1),
                },
            ),
        ] {
            let label = control_label(surface, index);
            labels.insert(label.clone());
            let window = match app.get_webview_window(&label) {
                Some(window) => window,
                None => WebviewWindowBuilder::new(
                    app,
                    &label,
                    WebviewUrl::App(
                        format!("index.html?surface={surface}&monitorIndex={index}").into(),
                    ),
                )
                .title("e-desktop")
                .decorations(false)
                .shadow(false)
                .resizable(false)
                .skip_taskbar(true)
                .focused(false)
                .visible(false)
                .build()
                .map_err(|e| error(ErrorCode::BackendUnavailable, e.to_string()))?,
            };
            configure_window(&window, rect)?;
            window
                .set_always_on_top(snapshot.enabled)
                .map_err(|e| error(ErrorCode::OperationDenied, e.to_string()))?;
            if surface == "topbar" || snapshot.enabled {
                window
                    .show()
                    .map_err(|e| error(ErrorCode::OperationDenied, e.to_string()))?;
            } else {
                let _ = window.hide();
            }
        }
    }
    for (label, window) in app.webview_windows() {
        if (label.starts_with("topbar-") || label.starts_with("pagerail-"))
            && !labels.contains(&label)
        {
            let _ = window.hide();
        }
    }
    Ok(())
}

fn show_surface(
    app: &tauri::AppHandle,
    snapshot: &Snapshot,
    surface: Surface,
    monitor_id: Option<&str>,
) -> Result<(), AppError> {
    let window = app
        .get_webview_window(surface.label())
        .ok_or_else(|| error(ErrorCode::BackendUnavailable, "控制窗口不存在。"))?;
    let monitor = match monitor_id {
        Some(id) => Some(
            snapshot
                .monitors
                .iter()
                .find(|m| m.monitor.id == id)
                .ok_or_else(|| error(ErrorCode::InvalidCommand, "目标显示器已断开。"))?,
        ),
        None => snapshot
            .monitors
            .iter()
            .find(|m| Some(&m.monitor.id) == snapshot.active_monitor.as_ref())
            .or(snapshot.monitors.first()),
    };
    if let Some(monitor) = monitor {
        let area = monitor.monitor.work_area;
        let rect = match surface {
            Surface::Overview => area,
            Surface::Commands => {
                let scale = monitor.monitor.scale_factor;
                let width = ((640.0 * scale) as u32).min(area.width);
                let height = ((480.0 * scale) as u32).min(area.height);
                Rect {
                    x: area.x + ((area.width - width) / 2) as i32,
                    y: area.y + ((area.height - height) / 3) as i32,
                    width,
                    height,
                }
            }
        };
        configure_window(&window, rect)?;
    }
    for label in ["overview", "commands"] {
        if label != surface.label() {
            if let Some(other) = app.get_webview_window(label) {
                let _ = other.hide();
            }
        }
    }
    window
        .set_always_on_top(true)
        .and_then(|_| window.show())
        .and_then(|_| window.set_focus())
        .map_err(|e| error(ErrorCode::OperationDenied, e.to_string()))?;
    app.emit_to(
        surface.label(),
        "surface-opened",
        monitor.map(|m| &m.monitor.id),
    )
    .map_err(|e| error(ErrorCode::BackendUnavailable, e.to_string()))
}

fn set_shortcut(
    app: &tauri::AppHandle,
    sender: &mpsc::SyncSender<Request>,
    key: &str,
    enabled: bool,
) -> Result<(), String> {
    let result = if enabled {
        let sender = sender.clone();
        let callback_key = key.to_owned();
        app.global_shortcut().on_shortcut(key, move |_, _, event| {
            if event.state == ShortcutState::Pressed {
                let _ = sender.try_send(Request::Shortcut(callback_key.clone()));
            }
        })
    } else {
        app.global_shortcut().unregister(key)
    };
    result.map_err(|e| {
        format!(
            "快捷键 {key} {}失败：{e}",
            if enabled { "注册" } else { "注销" }
        )
    })
}

fn reload_config(
    app: &tauri::AppHandle,
    sender: &mpsc::SyncSender<Request>,
    file: &mut ConfigFile,
    shortcuts: &mut Shortcuts,
    controller: &mut Controller,
) -> Option<Result<(), AppError>> {
    file.poll().map(|candidate| {
        candidate
            .and_then(|config| config.normalize_keys(normalize_key))
            .and_then(|config| {
                shortcuts.replace(config, |key, enabled| {
                    set_shortcut(app, sender, key, enabled)
                })
            })
            .and_then(|()| {
                controller
                    .set_window_rules(shortcuts.config.window_rules.clone())
                    .map_err(|e| e.to_string())?;
                controller.animation_duration =
                    Duration::from_millis(u64::from(shortcuts.config.animation_duration_ms));
                Ok(())
            })
            .map_err(|e| {
                error(
                    ErrorCode::InvalidCommand,
                    format!("配置 {}：{e}", file.path.display()),
                )
            })
    })
}

fn run_controller(
    app: tauri::AppHandle,
    receiver: mpsc::Receiver<Request>,
    shared: Arc<Mutex<Snapshot>>,
    sender: mpsc::SyncSender<Request>,
    config_path: std::path::PathBuf,
) {
    let mut controller = Controller::new();
    let defaults = Config::default()
        .normalize_keys(normalize_key)
        .expect("valid default shortcuts");
    let mut shortcuts = Shortcuts::new(defaults);
    let mut config_file = ConfigFile::new(config_path);
    // Install the requested map first; an invalid startup file falls back to defaults.
    if let Some(Err(issue)) = reload_config(
        &app,
        &sender,
        &mut config_file,
        &mut shortcuts,
        &mut controller,
    ) {
        controller.config_error = Some(issue);
        let defaults = shortcuts.config.clone();
        if let Err(e) = shortcuts.replace(defaults, |key, enabled| {
            set_shortcut(&app, &sender, key, enabled)
        }) {
            controller.record(error(ErrorCode::OperationDenied, e));
        }
    }
    if controller.backend.is_some() {
        if let Err(issue) = controller.refresh(false) {
            controller.record(issue);
        }
    }
    let mut previous = Vec::new();
    let mut controls = String::new();
    let mut next_refresh = Instant::now() + REFRESH_INTERVAL;
    loop {
        // A deadline, not an idle timeout: queued commands cannot starve config reloads.
        if Instant::now() >= next_refresh {
            if let Some(result) = reload_config(
                &app,
                &sender,
                &mut config_file,
                &mut shortcuts,
                &mut controller,
            ) {
                controller.config_error = result.err();
            }
            if controller.backend.is_some() {
                if let Err(issue) = controller.refresh(true) {
                    let _ = controller.stop();
                    controller.record(issue);
                }
            }
            next_refresh = Instant::now() + REFRESH_INTERVAL;
        }
        if let Err(issue) = controller.animate(Instant::now()) {
            controller.record(issue);
        }
        let snapshot = controller.snapshot(shortcuts.available());
        // Native surface positions change only with monitor geometry or enabled state.
        let geometry = serde_json::to_string(&(
            snapshot.enabled,
            snapshot
                .monitors
                .iter()
                .map(|m| &m.monitor)
                .collect::<Vec<_>>(),
        ))
        .unwrap_or_default();
        if geometry != controls {
            match position_controls(&app, &snapshot) {
                Ok(()) => controls = geometry,
                Err(issue) => controller.record(issue),
            }
        }
        let snapshot = controller.snapshot(shortcuts.available());
        let fingerprint = serde_json::to_vec(&snapshot).unwrap_or_default();
        if fingerprint != previous {
            *shared.lock().unwrap_or_else(|e| e.into_inner()) = snapshot.clone();
            let _ = app.emit("snapshot", &snapshot);
            previous = fingerprint;
        }
        let next_deadline = controller
            .animation
            .deadline()
            .map_or(next_refresh, |frame| frame.min(next_refresh));
        let request = match receiver
            .recv_timeout(next_deadline.saturating_duration_since(Instant::now()))
        {
            Ok(Request::Shortcut(key)) => {
                match shortcuts
                    .action(&key)
                    .and_then(|action| action.resolve(controller.engine.snapshot()))
                {
                    Some(ShortcutAction::Command { command }) => {
                        Ok(Request::Command(command, None))
                    }
                    Some(ShortcutAction::Overview {}) => Ok(Request::Show(Surface::Overview, None)),
                    Some(ShortcutAction::Commands {}) => Ok(Request::Show(Surface::Commands, None)),
                    Some(ShortcutAction::Quit {}) => Ok(Request::Quit),
                    _ => continue,
                }
            }
            request => request,
        };
        match request {
            Ok(Request::Command(mut command, reply)) => {
                if let Command::AddPage { monitor_id } = &mut command {
                    if monitor_id.is_empty() {
                        *monitor_id = controller
                            .engine
                            .snapshot()
                            .active_monitor
                            .clone()
                            .unwrap_or_default();
                    }
                }
                let result = controller.command(command);
                if let Err(issue) = &result {
                    controller.record(issue.clone());
                }
                let current = controller.snapshot(shortcuts.available());
                *shared.lock().unwrap_or_else(|e| e.into_inner()) = current.clone();
                let _ = app.emit("snapshot", &current);
                if let Some(reply) = reply {
                    let _ = reply.send(result.map(|_| current));
                }
            }
            Ok(Request::Shortcut(_)) => unreachable!("shortcut resolved above"),
            Ok(Request::Show(surface, monitor_id)) => {
                if let Err(issue) = controller.finish_animation() {
                    controller.record(issue);
                    continue;
                }
                if let Err(issue) = show_surface(&app, &snapshot, surface, monitor_id.as_deref()) {
                    controller.record(issue);
                }
            }
            Ok(Request::Dismiss(surface)) => {
                if let Some(window) = app.get_webview_window(surface.label()) {
                    let _ = window.hide();
                }
                let current = controller.snapshot(shortcuts.available());
                if current.enabled && current.backend.capabilities.focus {
                    if let Some(window_id) = current.focused_window {
                        if let Err(issue) = controller.command(Command::FocusWindow { window_id }) {
                            controller.record(issue);
                        }
                    }
                }
            }
            Ok(Request::Quit) => {
                if controller.stop().is_ok() {
                    app.state::<AppState>()
                        .can_exit
                        .store(true, Ordering::Release);
                    app.exit(0);
                    break;
                }
                controller.record(error(
                    ErrorCode::OperationDenied,
                    "窗口未能完整还原，已取消退出。请暂停并还原或再次退出重试。",
                ));
                if let Some(window) = app.get_webview_window("topbar") {
                    let _ = window.show().and_then(|_| window.set_focus());
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Err(issue) = controller.stop() {
                    eprintln!("窗口控制器停止时还原失败：{}", issue.message);
                }
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

pub fn run() {
    let (sender, receiver) = mpsc::sync_channel(64);
    let shared = Arc::new(Mutex::new(Snapshot::default()));
    let state = AppState {
        sender: sender.clone(),
        snapshot: shared.clone(),
        can_exit: AtomicBool::new(false),
    };
    tauri::Builder::default()
        .manage(state)
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            execute,
            open_surface,
            dismiss_surface,
            quit
        ])
        .setup(move |app| {
            let config_path = match std::env::var_os("E_DESKTOP_CONFIG") {
                Some(path) => std::path::PathBuf::from(path),
                None => app.path().app_config_dir()?.join("config.json"),
            };
            let app = app.handle().clone();
            std::thread::Builder::new()
                .name("desktop-controller".into())
                .spawn(move || run_controller(app, receiver, shared, sender, config_path))?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let state = window.state::<AppState>();
                let request = match window.label() {
                    "overview" => Request::Dismiss(Surface::Overview),
                    "commands" => Request::Dismiss(Surface::Commands),
                    _ => Request::Quit,
                };
                let _ = send(&state, request);
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to build e-desktop")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                let state = app.state::<AppState>();
                if !state.can_exit.load(Ordering::Acquire) {
                    api.prevent_exit();
                    let _ = send(&state, Request::Quit);
                }
            }
        });
}
