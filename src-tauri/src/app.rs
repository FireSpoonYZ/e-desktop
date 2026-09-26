use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use tauri::{Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use crate::{
    layout::Engine,
    model::{
        AppError, BackendAvailability, BackendStatus, Command, Direction, ErrorCode, NativeAction,
        Rect, Snapshot,
    },
    platform::Backend,
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
    Page {
        index: Option<usize>,
        delta: i32,
        move_window: bool,
    },
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
    placements: HashMap<String, (Rect, Option<Rect>, bool)>,
}

impl Controller {
    fn new() -> Self {
        let mut controller = Self {
            backend: None,
            engine: Engine::new(BackendStatus::default()),
            errors: vec![],
            placements: HashMap::new(),
        };
        if let Err(e) = controller.connect() {
            controller.record(e);
        }
        controller
    }

    fn connect(&mut self) -> Result<(), AppError> {
        let backend = Backend::new()?;
        self.engine = Engine::new(backend.status());
        self.backend = Some(backend);
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
        snapshot
    }

    fn refresh(&mut self, apply: bool) -> Result<(), AppError> {
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
        let transition = self.engine.dispatch(command)?;
        self.apply(&transition.actions)
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

fn register_shortcuts(
    app: &tauri::AppHandle,
    sender: &mpsc::SyncSender<Request>,
) -> (bool, Vec<AppError>) {
    let mut failures = vec![];
    let mut register = |key: &str, command: Option<Command>, surface: Option<Surface>| {
        let sender = sender.clone();
        if let Err(e) = app.global_shortcut().on_shortcut(key, move |_, _, event| {
            if event.state != ShortcutState::Pressed {
                return;
            }
            let request = if let Some(surface) = surface {
                Request::Show(surface, None)
            } else if let Some(command) = &command {
                Request::Command(command.clone(), None)
            } else {
                Request::Quit
            };
            let _ = sender.try_send(request);
        }) {
            failures.push(error(
                ErrorCode::OperationDenied,
                format!("快捷键 {key} 注册失败：{e}"),
            ));
        }
    };
    for (key, direction) in [
        ("H", Direction::Left),
        ("J", Direction::Down),
        ("K", Direction::Up),
        ("L", Direction::Right),
    ] {
        register(
            &format!("Control+Alt+{key}"),
            Some(Command::FocusDirection { direction }),
            None,
        );
        register(
            &format!("Control+Alt+Shift+{key}"),
            Some(Command::MoveWindow { direction }),
            None,
        );
    }
    for (key, command) in [
        ("Control+Alt+R", Command::CycleWidth),
        ("Control+Alt+C", Command::CenterFocused),
        ("Control+Alt+F", Command::ToggleFullscreen),
        ("Control+Alt+V", Command::ToggleFloating),
        ("Control+Alt+Backspace", Command::Disable),
    ] {
        register(key, Some(command), None);
    }
    register("Control+Alt+Space", None, Some(Surface::Commands));
    register("Control+Alt+O", None, Some(Surface::Overview));
    register("Control+Alt+Q", None, None);
    register(
        "Control+Alt+N",
        Some(Command::AddPage {
            monitor_id: String::new(),
        }),
        None,
    );
    let mut pages = vec![
        ("Control+Alt+PageUp".to_owned(), None, -1, false),
        ("Control+Alt+PageDown".to_owned(), None, 1, false),
        ("Control+Alt+Shift+PageUp".to_owned(), None, -1, true),
        ("Control+Alt+Shift+PageDown".to_owned(), None, 1, true),
    ];
    for index in 0..9 {
        pages.push((format!("Control+Alt+{}", index + 1), Some(index), 0, false));
        pages.push((
            format!("Control+Alt+Shift+{}", index + 1),
            Some(index),
            0,
            true,
        ));
    }
    for (key, index, delta, move_window) in pages {
        let sender = sender.clone();
        if let Err(e) = app
            .global_shortcut()
            .on_shortcut(key.as_str(), move |_, _, event| {
                if event.state == ShortcutState::Pressed {
                    let _ = sender.try_send(Request::Page {
                        index,
                        delta,
                        move_window,
                    });
                }
            })
        {
            failures.push(error(
                ErrorCode::OperationDenied,
                format!("快捷键 {key} 注册失败：{e}"),
            ));
        }
    }
    (failures.is_empty(), failures)
}

fn page_command(
    snapshot: &Snapshot,
    index: Option<usize>,
    delta: i32,
    move_window: bool,
) -> Option<Command> {
    let monitor = snapshot
        .monitors
        .iter()
        .find(|m| Some(&m.monitor.id) == snapshot.active_monitor.as_ref())
        .or(snapshot.monitors.first())?;
    let current = monitor
        .pages
        .iter()
        .position(|p| p.id == monitor.active_page)?;
    let target = index.unwrap_or_else(|| (current as i64 + i64::from(delta)).max(0) as usize);
    let page = monitor.pages.get(target)?;
    if move_window {
        Some(Command::MoveWindowToPage {
            window_id: snapshot.focused_window.clone()?,
            page_id: page.id.clone(),
        })
    } else {
        Some(Command::SwitchPage {
            monitor_id: monitor.monitor.id.clone(),
            page_id: page.id.clone(),
        })
    }
}

fn run_controller(
    app: tauri::AppHandle,
    receiver: mpsc::Receiver<Request>,
    shared: Arc<Mutex<Snapshot>>,
    shortcuts: bool,
    startup_errors: Vec<AppError>,
) {
    let mut controller = Controller::new();
    for issue in startup_errors {
        controller.record(issue);
    }
    if controller.backend.is_some() {
        if let Err(issue) = controller.refresh(false) {
            controller.record(issue);
        }
    }
    let mut previous = Vec::new();
    let mut controls = String::new();
    loop {
        let snapshot = controller.snapshot(shortcuts);
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
        let snapshot = controller.snapshot(shortcuts);
        let fingerprint = serde_json::to_vec(&snapshot).unwrap_or_default();
        if fingerprint != previous {
            *shared.lock().unwrap_or_else(|e| e.into_inner()) = snapshot.clone();
            let _ = app.emit("snapshot", &snapshot);
            previous = fingerprint;
        }
        match receiver.recv_timeout(REFRESH_INTERVAL) {
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
                let current = controller.snapshot(shortcuts);
                *shared.lock().unwrap_or_else(|e| e.into_inner()) = current.clone();
                let _ = app.emit("snapshot", &current);
                if let Some(reply) = reply {
                    let _ = reply.send(result.map(|_| current));
                }
            }
            Ok(Request::Page {
                index,
                delta,
                move_window,
            }) => {
                if let Some(command) =
                    page_command(controller.engine.snapshot(), index, delta, move_window)
                {
                    if let Err(issue) = controller.command(command) {
                        controller.record(issue);
                    }
                }
            }
            Ok(Request::Show(surface, monitor_id)) => {
                if let Err(issue) = show_surface(&app, &snapshot, surface, monitor_id.as_deref()) {
                    controller.record(issue);
                }
            }
            Ok(Request::Dismiss(surface)) => {
                if let Some(window) = app.get_webview_window(surface.label()) {
                    let _ = window.hide();
                }
                let current = controller.snapshot(shortcuts);
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
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if controller.backend.is_some() {
                    if let Err(issue) = controller.refresh(true) {
                        let _ = controller.stop();
                        controller.record(issue);
                    }
                }
            }
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
            let (shortcuts, errors) = register_shortcuts(app.handle(), &sender);
            let app = app.handle().clone();
            std::thread::Builder::new()
                .name("desktop-controller".into())
                .spawn(move || run_controller(app, receiver, shared, shortcuts, errors))?;
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
