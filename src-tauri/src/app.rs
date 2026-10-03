use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use tauri::{Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use crate::{
    animation::{Animation, Placements},
    config::{Config, ConfigFile, ShortcutAction},
    layout::{Engine, expand_gap, half_gap, inset_gap},
    model::{
        AppError, BackendAvailability, BackendStatus, Command, ErrorCode, MonitorState,
        NativeAction, Rect, Snapshot,
    },
    platform::Backend,
    pointer::{Gesture, tiled},
    preview::{PreviewSession, PreviewSlot, PreviewStatus},
    rules::WindowRule,
    shortcuts::{Shortcuts, normalize_key},
};

#[cfg(target_os = "windows")]
use crate::platform::splitter;
#[cfg(target_os = "windows")]
mod controller_queue;
#[cfg(target_os = "windows")]
use controller_queue::{Receiver as RequestReceiver, Sender as RequestSender};
#[cfg(not(target_os = "windows"))]
type RequestSender = mpsc::SyncSender<Request>;
#[cfg(not(target_os = "windows"))]
type RequestReceiver = mpsc::Receiver<Request>;

enum ReceiveError {
    Timeout,
    Disconnected,
    #[cfg(target_os = "windows")]
    Failed(AppError),
}

fn request_queue() -> Result<(RequestSender, RequestReceiver), AppError> {
    #[cfg(target_os = "windows")]
    return controller_queue::bounded(64);
    #[cfg(not(target_os = "windows"))]
    Ok(mpsc::sync_channel(64))
}

fn receive_request(receiver: &RequestReceiver, deadline: Instant) -> Result<Request, ReceiveError> {
    #[cfg(target_os = "windows")]
    return receiver.recv_until(deadline);
    #[cfg(not(target_os = "windows"))]
    receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|issue| match issue {
            mpsc::RecvTimeoutError::Timeout => ReceiveError::Timeout,
            mpsc::RecvTimeoutError::Disconnected => ReceiveError::Disconnected,
        })
}

const BAR_HEIGHT: f64 = 36.0;
const REFRESH_INTERVAL: Duration = Duration::from_millis(500);
type Reply = mpsc::SyncSender<Result<Snapshot, AppError>>;

// Set E_DESKTOP_TRACE_IPC=1 before startup; cached on the first traced request.
// No window titles/payloads or animation-frame logging. Disabled traces do no log I/O/formatting.
struct IpcDiagnostics {
    started: Instant,
    sequence: AtomicU64,
}

fn ipc_trace_enabled(value: Option<&std::ffi::OsStr>) -> bool {
    value == Some(std::ffi::OsStr::new("1"))
}

#[derive(Clone, Copy)]
struct IpcTrace {
    diagnostics: Option<&'static IpcDiagnostics>,
    id: u64,
    operation: &'static str,
}

impl IpcTrace {
    const NONE: Self = Self {
        diagnostics: None,
        id: 0,
        operation: "",
    };

    fn begin(operation: &'static str) -> Self {
        static DIAGNOSTICS: OnceLock<Option<IpcDiagnostics>> = OnceLock::new();
        let diagnostics = DIAGNOSTICS.get_or_init(|| {
            ipc_trace_enabled(std::env::var_os("E_DESKTOP_TRACE_IPC").as_deref()).then(|| {
                IpcDiagnostics {
                    started: Instant::now(),
                    sequence: AtomicU64::new(1),
                }
            })
        });
        Self::with_diagnostics(operation, diagnostics.as_ref())
    }

    fn with_diagnostics(
        operation: &'static str,
        diagnostics: Option<&'static IpcDiagnostics>,
    ) -> Self {
        match diagnostics {
            Some(diagnostics) => Self {
                diagnostics: Some(diagnostics),
                id: diagnostics.sequence.fetch_add(1, Ordering::Relaxed),
                operation,
            },
            None => Self::NONE,
        }
    }

    fn write(self, stage: &str, output: &mut impl std::io::Write) {
        if let Some(diagnostics) = self.diagnostics {
            // Ignore logging failures: a closed stderr must not change command results.
            let _ = writeln!(
                output,
                "[ipc-trace] us={} thread={:?} request={} op={} stage={}",
                diagnostics.started.elapsed().as_micros(),
                std::thread::current().id(),
                self.id,
                self.operation,
                stage
            );
        }
    }

    fn mark(self, stage: &str) {
        if self.diagnostics.is_some() {
            self.write(stage, &mut std::io::stderr().lock());
        }
    }

    fn scope(self, enter: &'static str, exit: &'static str) -> IpcTraceScope {
        self.mark(enter);
        IpcTraceScope { trace: self, exit }
    }
}

// "exit" means the scope returned (including errors), not that native work succeeded.
struct IpcTraceScope {
    trace: IpcTrace,
    exit: &'static str,
}
impl Drop for IpcTraceScope {
    fn drop(&mut self) {
        self.trace.mark(self.exit);
    }
}

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
    Command(Command, Option<Reply>, IpcTrace),
    Show(Surface, Option<String>),
    Dismiss(Surface, IpcTrace),
    Previews(
        u64,
        Vec<PreviewSlot>,
        mpsc::SyncSender<Result<Vec<PreviewStatus>, AppError>>,
        IpcTrace,
    ),
    Shortcut(String),
    PinBar(String, bool),
    /// The mouse hook queued events; drain them with `hook::drain`.
    #[cfg(target_os = "windows")]
    Pointer,
    /// A column boundary drag ended; drain it with `splitter::drain`.
    #[cfg(target_os = "windows")]
    Edges,
    Quit,
}

struct AppState {
    sender: RequestSender,
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
    #[cfg(target_os = "windows")]
    return state.sender.try_send(request);
    #[cfg(not(target_os = "windows"))]
    state.sender.try_send(request).map_err(|_| {
        error(
            ErrorCode::BackendUnavailable,
            "窗口控制器暂时忙碌或已退出，请稍后重试。",
        )
    })
}

#[tauri::command]
fn get_snapshot(state: tauri::State<'_, AppState>) -> Snapshot {
    let trace = IpcTrace::begin("get_snapshot");
    let _ipc = trace.scope("ipc.enter", "ipc.exit");
    let _read = trace.scope("snapshot.read.enter", "snapshot.read.exit");
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
    let trace = IpcTrace::begin(if matches!(command, Command::DropWindow { .. }) {
        "execute.dropWindow"
    } else {
        "execute"
    });
    let _ipc = trace.scope("ipc.enter", "ipc.exit");
    let (tx, rx) = mpsc::sync_channel(1);
    trace.mark("enqueue.enter");
    send(&state, Request::Command(command, Some(tx), trace))?;
    trace.mark("enqueue.accepted");
    // Native calls stay on one dedicated thread, including non-Send AX objects.
    tauri::async_runtime::spawn_blocking(move || {
        let _wait = trace.scope("reply.wait.enter", "reply.wait.exit");
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
    let trace = IpcTrace::begin("dismiss_surface");
    let _ipc = trace.scope("ipc.enter", "ipc.exit");
    trace.mark("enqueue.enter");
    send(&state, Request::Dismiss(Surface::parse(&surface)?, trace))?;
    trace.mark("enqueue.accepted");
    Ok(())
}

#[tauri::command]
async fn sync_previews(
    window: tauri::WebviewWindow,
    session: u64,
    slots: Vec<PreviewSlot>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<PreviewStatus>, AppError> {
    let trace = IpcTrace::begin("sync_previews");
    let _ipc = trace.scope("ipc.enter", "ipc.exit");
    if window.label() != "overview" {
        return Err(error(
            ErrorCode::InvalidCommand,
            "实时预览仅供概览窗口使用。",
        ));
    }
    let (tx, rx) = mpsc::sync_channel(1);
    trace.mark("enqueue.enter");
    send(&state, Request::Previews(session, slots, tx, trace))?;
    trace.mark("enqueue.accepted");
    tauri::async_runtime::spawn_blocking(move || {
        let _wait = trace.scope("reply.wait.enter", "reply.wait.exit");
        rx.recv()
            .map_err(|_| error(ErrorCode::BackendUnavailable, "窗口控制器已停止。"))?
    })
    .await
    .map_err(|e| error(ErrorCode::BackendUnavailable, e.to_string()))?
}

#[tauri::command]
fn set_bar_pinned(
    monitor_id: String,
    pinned: bool,
    state: tauri::State<'_, AppState>,
) -> Result<(), AppError> {
    send(&state, Request::PinBar(monitor_id, pinned))
}

#[tauri::command]
fn quit(state: tauri::State<'_, AppState>) -> Result<(), AppError> {
    send(&state, Request::Quit)
}

struct Controller {
    trace: IpcTrace,
    backend: Option<Backend>,
    engine: Engine,
    errors: Vec<AppError>,
    config_error: Option<AppError>,
    window_rules: Vec<WindowRule>,
    placements: Placements,
    /// Plans a window refused (e.g. minimum size); not retried until the plan changes.
    refused: Placements,
    animation: Animation,
    animation_duration: Duration,
    preview_session: PreviewSession,
    gesture: Option<Gesture>,
    /// A tiled window is being dragged with the modifier (the splitter thread previews it).
    window_drag: bool,
    /// Last window under the pointer: focus-follows-mouse acts on entering a window.
    hovered: Option<String>,
    in_corner: bool,
    /// False hides every top bar and reserves no space.
    top_bar: bool,
    /// Managed window in the system move/size loop at the last refresh.
    native_drag: Option<String>,
    /// Known title-bar move: Snap/DPI size changes are not manual border resizes.
    native_move: Option<String>,
    /// Native foreground when a deferred slide began. Later polls of that window are not clicks.
    /// `None` means nothing managed was in front, not that the anchor is unset.
    ignored_foreground: Option<String>,
    /// Refresh within this delay: a released native move/resize, or windows that appeared or
    /// disappeared (their neighbours close the gap right away instead of at the next poll).
    refresh_soon: Option<Duration>,
    /// Whether bars can auto-hide (needs the pointer hook).
    autohide: bool,
    pinned: BTreeSet<String>,
    revealed: BTreeSet<String>,
    /// Monitor the overview or commands surface was opened on.
    overview_host: Option<String>,
    commands_host: Option<String>,
}

impl Controller {
    fn new() -> Self {
        let mut controller = Self {
            trace: IpcTrace::NONE,
            backend: None,
            engine: Engine::new(BackendStatus::default()),
            errors: vec![],
            config_error: None,
            window_rules: vec![],
            placements: Placements::new(),
            refused: Placements::new(),
            animation: Animation::default(),
            preview_session: PreviewSession::default(),
            gesture: None,
            window_drag: false,
            hovered: None,
            in_corner: false,
            top_bar: Config::default().top_bar,
            native_drag: None,
            native_move: None,
            ignored_foreground: None,
            refresh_soon: None,
            autohide: false,
            pinned: BTreeSet::new(),
            revealed: BTreeSet::new(),
            overview_host: None,
            commands_host: None,
            animation_duration: Duration::from_millis(u64::from(
                Config::default().animation_duration_ms,
            )),
        };
        if let Err(e) = controller.connect() {
            controller.record(e);
        }
        controller.engine.set_gaps(Config::default().gaps);
        controller
    }

    fn connect(&mut self) -> Result<(), AppError> {
        let gaps = self.engine.snapshot().gaps;
        let layout_options = self.engine.layout_options().clone(); // lane: layout-options
        let backend = Backend::new()?;
        let mut engine = Engine::new(backend.status());
        engine.set_window_rules(self.window_rules.clone())?;
        engine.set_gaps(gaps);
        engine.set_layout_options(layout_options);
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
        snapshot.bars_autohide = self.autohide;
        snapshot.animation_duration_ms = self.animation_duration.as_millis() as u32;
        snapshot.pinned_bars = snapshot
            .monitors
            .iter()
            .map(|m| m.monitor.id.clone())
            .filter(|id| self.bar_pinned(id))
            .collect();
        snapshot
    }

    fn bar_pinned(&self, monitor_id: &str) -> bool {
        self.top_bar && (!self.autohide || self.pinned.contains(monitor_id))
    }

    /// Bars shown right now: pinned ones plus those revealed by the pointer.
    fn visible_bars(&self) -> BTreeSet<String> {
        if !self.top_bar {
            return BTreeSet::new();
        }
        self.engine
            .snapshot()
            .monitors
            .iter()
            .map(|m| m.monitor.id.clone())
            .filter(|id| {
                !self
                    .engine
                    .snapshot()
                    .suspended_monitors
                    .iter()
                    .any(|s| s == id)
                    && (self.bar_pinned(id) || self.revealed.contains(id))
            })
            .collect()
    }

    fn refresh(&mut self, apply: bool) -> Result<(), AppError> {
        // Keep a running slide when polling finds the same layout. A focus waiting for its
        // target to arrive must survive reconciliation without adopting the stale foreground.
        let deferred_focus = self.animation.deferred_focus_id().map(str::to_owned);
        let backend = self
            .backend
            .as_mut()
            .ok_or_else(|| error(ErrorCode::BackendUnavailable, "原生窗口后端尚未连接。"))?;
        let system = backend.enumerate();
        self.engine.set_backend(backend.status());
        #[cfg(target_os = "windows")]
        self.engine.set_min_widths(backend.min_widths());
        let mut system = system?;
        #[cfg(target_os = "windows")]
        {
            self.engine
                .set_suspended_monitors(backend.full_display_monitors());
            self.engine
                .set_covering_windows(backend.covering_windows());
        }
        let enumerated = system.focused_window.clone();
        let managed = enumerated
            .as_ref()
            .is_some_and(|id| system.windows.iter().any(|w| &w.id == id));
        if crate::animation::suppress_observed_focus(
            deferred_focus.as_deref(),
            enumerated.as_deref(),
            self.ignored_foreground.as_deref(),
            managed,
        ) {
            // The foreground left behind (or an unmanaged palette) is not a new activation.
            // A different managed window is a click: leave it for reconcile.
            system.focused_window = None;
        }
        let gaps = self.engine.snapshot().gaps;
        let viewports = system
            .monitors
            .iter()
            .map(|monitor| {
                // Only a pinned bar reserves space; an auto-hidden one overlays windows.
                let top = if self.bar_pinned(&monitor.id) {
                    bar_rect(monitor).height
                } else {
                    0
                };
                let area = monitor.work_area;
                let usable = Rect {
                    x: area.x,
                    y: area.y.saturating_add(top.min(area.height) as i32),
                    width: area.width.max(1),
                    height: area.height.saturating_sub(top).max(1),
                };
                // lane: layout-options: struts shrink the area inside the bar, before gaps.
                let usable = crate::layout::options::apply_struts(
                    usable,
                    &self.engine.layout_options().struts,
                    monitor.scale_factor,
                );
                (
                    monitor.id.clone(),
                    inset_gap(usable, half_gap(gaps, monitor.scale_factor)),
                )
            })
            .collect::<BTreeMap<_, _>>();
        self.engine.set_viewports(viewports);
        let prev = self.engine.snapshot().clone();
        let mut transition = self.engine.reconcile(system)?;
        self.revealed.retain(|id| {
            transition
                .snapshot
                .monitors
                .iter()
                .any(|m| &m.monitor.id == id)
        });
        let alive = |id: &String| {
            transition
                .snapshot
                .windows
                .iter()
                .any(|w| &w.native.id == id)
        };
        self.placements.retain(|id, _| alive(id));
        self.refused.retain(|id, _| alive(id));
        // While the user drags a window's own border or title bar, leave everything alone;
        // the first refresh after the drag adopts the new size or position.
        #[cfg(target_os = "windows")]
        let dragging = self.backend.as_ref().and_then(Backend::move_size_window);
        #[cfg(not(target_os = "windows"))]
        let dragging: Option<String> = None;
        let dragged = match dragging {
            Some(id) => {
                self.animation.cancel();
                self.ignored_foreground = None;
                self.native_drag = Some(id);
                return Ok(());
            }
            None => self.native_drag.take(),
        };
        if apply {
            // A tiled window dragged by its title bar joins the layout where it was dropped,
            // including on another monitor.
            // A window resized through its own border keeps the new size. Only the window the
            // user dragged counts: apps also resize themselves (e.g. after moving to a monitor
            // with other scaling), and those changes are placed back into the layout.
            if let Some((planned, id)) = dragged
                .filter(|id| !self.engine.window_protected(id))
                .and_then(|id| Some((self.placements.get(&id).filter(|p| !p.2)?.0, id)))
            {
                let adopted = match self.engine.adopt_native_move(&id, planned)? {
                    Some(moved) => Some(moved),
                    None if self.native_move.as_ref() == Some(&id) => None,
                    None => self.engine.adopt_native_sizes(&[(id, planned)])?,
                };
                if let Some(adopted) = adopted {
                    transition = adopted;
                }
            }
            // Opened/closed windows push neighbours. Animating windows start from the
            // displayed frame; a native resize outside the plan still has to match.
            // The plan already in flight must not be reinserted onto a paused foreground.
            // A focus the user requested since then is marked and stays.
            #[cfg(target_os = "windows")]
            if self
                .backend
                .as_ref()
                .is_some_and(Backend::foreground_on_paused)
            {
                self.animation.discard_stale_focus();
            }
            let (actions, fresh) = crate::animation::poll_refresh(
                &mut self.animation,
                &self.placements,
                &prev,
                &transition.snapshot,
                transition.actions,
                enumerated.as_deref(),
                self.ignored_foreground.as_deref(),
                self.animation_duration,
                Instant::now(),
            );
            self.note_ignored(fresh, deferred_focus.is_some(), None);
            self.present(actions, fresh)?;
        } else {
            self.animation.cancel();
            self.ignored_foreground = None;
        }
        Ok(())
    }

    fn note_ignored(&mut self, fresh: bool, already_deferred: bool, observed: Option<String>) {
        let deferred = self.animation.deferred_focus_id().map(str::to_owned);
        crate::animation::note_ignored_foreground(
            &mut self.ignored_foreground,
            fresh,
            already_deferred,
            deferred.as_deref(),
            observed,
        );
    }

    /// Foreground before a shortcut installs a plan. Not a full enumeration: one HWND lookup.
    /// Non-Windows backends do not defer slides (no clipping); layout focus is enough there.
    fn observed_foreground(&self) -> Option<String> {
        #[cfg(target_os = "windows")]
        {
            let hwnd =
                unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
            if hwnd.is_null() {
                return None;
            }
            return self
                .backend
                .as_ref()
                .and_then(|backend| backend.window_for(hwnd as usize));
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.engine.snapshot().focused_window.clone()
        }
    }

    fn apply(&mut self, actions: &[NativeAction]) -> Result<(), AppError> {
        let _apply = self.trace.scope("apply.enter", "apply.exit");
        for action in actions {
            // Keep keyboard input on the open surface; Engine focus still updates.
            // Dismiss clears its host before reissuing FocusWindow for the handoff.
            if matches!(action, NativeAction::Focus { .. })
                && (self.overview_host.is_some() || self.commands_host.is_some())
            {
                continue;
            }
            if matches!(
                action,
                NativeAction::Placement { window_id, .. } | NativeAction::Focus { window_id }
                    if self.engine.window_protected(window_id)
            ) {
                continue;
            }
            if let NativeAction::Placement {
                window_id,
                rect,
                clip,
                minimized,
            } = action
            {
                let plan = (*rect, *clip, *minimized);
                if self.refused.get(window_id) == Some(&plan) {
                    continue;
                }
                let same_plan = self.placements.get(window_id) == Some(&plan);
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
                // One window refusing its slot (minimum size, layered clip, ...) must not
                // end the whole session: the backend already restored that window, so
                // report it, leave it untouched until its plan changes, and keep going.
                if let (
                    NativeAction::Placement {
                        window_id,
                        rect,
                        clip,
                        minimized,
                    },
                    ErrorCode::OperationDenied,
                ) = (action, &issue.code)
                {
                    self.placements.remove(window_id);
                    self.refused
                        .insert(window_id.clone(), (*rect, *clip, *minimized));
                    self.record(issue);
                    continue;
                }
                // Windows may refuse foreground activation (foreground lock). That must not end
                // the session; the next enumeration adopts the window actually focused.
                if matches!(
                    (action, &issue.code),
                    (NativeAction::Focus { .. }, ErrorCode::OperationDenied)
                ) {
                    continue;
                }
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
                    self.refused.remove(window_id);
                }
                NativeAction::Restore { window_id } => {
                    self.placements.remove(window_id);
                    self.refused.remove(window_id);
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<(), AppError> {
        self.clear_previews();
        self.animation.cancel();
        self.ignored_foreground = None;
        self.placements.clear();
        self.refused.clear();
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
        self.run(command, true)
    }

    /// `animate: false` for commands a pointer drag issues on every move.
    fn run(&mut self, command: Command, animate: bool) -> Result<(), AppError> {
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
            self.refused.clear();
            return self.refresh(true);
        }
        if matches!(command, Command::Enable) {
            self.refresh(false)?;
        }
        let foreground_paused = self.foreground_on_paused_display();
        self.animation.note_user_focus(foreground_paused);
        let already_deferred = self.animation.deferred_focus_id().is_some();
        let observed = self.observed_foreground();
        let prev = self.engine.snapshot().clone();
        self.trace.mark("dispatch.enter");
        let result = self.engine.dispatch(command);
        self.trace.mark("dispatch.exit");
        let transition = match result {
            Ok(transition) => transition,
            Err(issue) => {
                self.finish_animation()?;
                return Err(issue);
            }
        };
        let layout = transition
            .actions
            .iter()
            .any(|a| matches!(a, NativeAction::Placement { .. }));
        let actions = if animate && layout {
            // Retargets a running animation from the frames currently on screen.
            self.animation.start(
                &prev,
                &transition.snapshot,
                &self.placements,
                transition.actions,
                self.animation_duration,
                Instant::now(),
            )
        } else {
            // Close returns no layout actions; finish the old target before it too.
            self.finish_animation()?;
            transition.actions
        };
        let fresh = self.animation.started_fresh();
        self.note_ignored(fresh, already_deferred, observed);
        self.present(actions, fresh)
    }

    /// Apply animation output. On Windows the compositor overlay draws the frames (cropped to
    /// each monitor, as in niri) while the real windows move once; elsewhere, or if the
    /// overlay fails, every frame moves the real windows.
    fn present(&mut self, actions: Vec<NativeAction>, started: bool) -> Result<(), AppError> {
        let _present = self.trace.scope("present.enter", "present.exit");
        #[cfg(target_os = "windows")]
        if self.animation.deadline().is_some() && self.backend.is_some() {
            let sprites = self.sprites();
            let animated: HashSet<String> = self
                .animation
                .sprites()
                .into_iter()
                .map(|s| s.window_id)
                .collect();
            let actions: Vec<_> = actions
                .into_iter()
                .filter(|a| {
                    !matches!(
                        a,
                        NativeAction::Placement { window_id, .. } | NativeAction::Focus { window_id }
                            if self.engine.window_protected(window_id)
                    )
                })
                .collect();
            let (frames, rest): (Vec<_>, Vec<_>) = actions.into_iter().partition(|a| {
                matches!(a, NativeAction::Placement { window_id, .. } if animated.contains(window_id))
            });
            let backend = self.backend.as_mut().unwrap();
            self.trace.mark("compose.enter");
            let result = backend.compose(&sprites);
            self.trace.mark("compose.exit");
            match result {
                Ok(()) => {
                    // The frames on screen: a retarget continues from them.
                    for frame in frames {
                        if let NativeAction::Placement {
                            window_id,
                            rect,
                            clip,
                            minimized,
                        } = frame
                        {
                            self.placements.insert(window_id, (rect, clip, minimized));
                        }
                    }
                    if started {
                        self.animation.restart(Instant::now());
                    }
                    return self.apply(&rest);
                }
                Err(issue) => {
                    backend.compose_end();
                    self.record(issue);
                    return self.apply(&frames.into_iter().chain(rest).collect::<Vec<_>>());
                }
            }
        }
        let _ = started;
        self.apply(&actions)
    }

    /// Everything the overlay of an animating monitor must draw: the animated windows, then
    /// the other windows shown on that monitor (they would vanish under it otherwise), with
    /// floating windows last, on top.
    #[cfg(target_os = "windows")]
    fn sprites(&self) -> Vec<crate::animation::Sprite> {
        let mut sprites = self.animation.sprites();
        sprites.retain(|sprite| !self.engine.window_protected(&sprite.window_id));
        let snapshot = self.engine.snapshot();
        let areas: Vec<Rect> = sprites.iter().map(|s| s.bounds).collect();
        for monitor in snapshot.monitors.iter().filter(|monitor| {
            !snapshot
                .suspended_monitors
                .iter()
                .any(|id| id == &monitor.monitor.id)
        }) {
            let bounds = expand_gap(
                monitor.viewport,
                half_gap(snapshot.gaps, monitor.monitor.scale_factor),
            );
            let Some(page) = monitor
                .pages
                .iter()
                .find(|p| p.id == monitor.active_page && areas.contains(&bounds))
            else {
                continue;
            };
            let tiled = page.columns.iter().flat_map(|c| c.windows.iter());
            for id in tiled.chain(page.floating_windows.iter()) {
                if sprites.iter().any(|s| &s.window_id == id) {
                    continue;
                }
                let rect = match self.placements.get(id) {
                    Some((rect, _, false)) => *rect,
                    Some(_) => continue,
                    None => match snapshot.windows.iter().find(|w| &w.native.id == id) {
                        Some(w) if !w.native.minimized => w.native.rect,
                        _ => continue,
                    },
                };
                sprites.push(crate::animation::Sprite {
                    window_id: id.clone(),
                    rect,
                    bounds,
                    early: None,
                });
            }
        }
        sprites
    }

    /// Uncover the monitors once no animation runs (finished, finished early or dropped).
    fn settle(&mut self) {
        #[cfg(target_os = "windows")]
        if self.animation.deadline().is_none() {
            if let Some(backend) = self.backend.as_mut() {
                backend.compose_end();
            }
        }
    }

    fn clear_previews(&mut self) {
        let _clear = self
            .trace
            .scope("previews.clear.enter", "previews.clear.exit");
        self.preview_session.end();
        #[cfg(target_os = "windows")]
        if let Some(backend) = &mut self.backend {
            backend.clear_previews();
        }
    }

    fn preview_capable(&self) -> bool {
        #[cfg(target_os = "windows")]
        return self
            .backend
            .as_ref()
            .is_some_and(Backend::previews_available);
        #[cfg(not(target_os = "windows"))]
        false
    }

    fn sync_previews(
        &mut self,
        app: &tauri::AppHandle,
        session: u64,
        slots: &[PreviewSlot],
    ) -> Result<Vec<PreviewStatus>, AppError> {
        // An old cleanup/request must not clear or repopulate a newly opened overview.
        if !self.preview_session.accepts(session) {
            return Ok(vec![]);
        }
        if slots.len() > self.engine.snapshot().windows.len() {
            return Err(error(ErrorCode::InvalidCommand, "预览窗口数量无效。"));
        }
        #[cfg(target_os = "windows")]
        {
            let window = app
                .get_webview_window("overview")
                .ok_or_else(|| error(ErrorCode::BackendUnavailable, "概览窗口不存在。"))?;
            self.trace.mark("previews.hwnd.enter");
            let hwnd = window
                .hwnd()
                .map_err(|e| error(ErrorCode::BackendUnavailable, e.to_string()))?;
            self.trace.mark("previews.hwnd.exit");
            let _native = self
                .trace
                .scope("previews.native.enter", "previews.native.exit");
            self.backend
                .as_mut()
                .ok_or_else(|| error(ErrorCode::BackendUnavailable, "原生窗口后端尚未连接。"))?
                .sync_previews(hwnd.0 as usize, slots)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = app;
            Ok(vec![])
        }
    }

    fn finish_animation(&mut self) -> Result<(), AppError> {
        // Preserve normal activation when finishing early; only fullscreen makes it stale.
        if self.foreground_on_paused_display() {
            self.animation.discard_stale_focus();
        }
        self.animation.clear_user_focus();
        let actions = self.animation.cancel();
        self.ignored_foreground = None;
        self.apply(&actions)
    }

    fn animate(&mut self, now: Instant) -> Result<(), AppError> {
        // Frame end applies the plan's deferred focus. Only the pre-pause one is stale.
        #[cfg(target_os = "windows")]
        if self
            .backend
            .as_ref()
            .is_some_and(Backend::foreground_on_paused)
        {
            self.animation.discard_stale_focus();
        }
        let actions = self.animation.frame(now);
        // Queued input may wake the loop before a frame is due; do not redraw its overlay.
        if actions.is_empty() {
            return Ok(());
        }
        let result = self.present(actions, false);
        if self.animation.deferred_focus_id().is_none() {
            self.ignored_foreground = None;
        }
        self.settle();
        result
    }

    /// niri warp-mouse-to-focus: after a non-pointer focus change, move the pointer inside.
    fn warp_to_focus(&self, before: Option<&String>) {
        #[cfg(target_os = "windows")]
        {
            let snapshot = self.engine.snapshot();
            let Some(id) = snapshot
                .focused_window
                .as_ref()
                .filter(|id| snapshot.enabled && Some(*id) != before)
            else {
                return;
            };
            if self.engine.window_protected(id) {
                return;
            }
            if let (Some(backend), Some((rect, clip, false))) =
                (&self.backend, self.placements.get(id).copied())
            {
                backend.warp_pointer(clip.unwrap_or(rect));
            }
        }
        #[cfg(not(target_os = "windows"))]
        let _ = before;
    }

    fn note_surface(&mut self, surface: Surface, host: Option<String>) {
        *match surface {
            Surface::Overview => &mut self.overview_host,
            Surface::Commands => &mut self.commands_host,
        } = host;
    }

    fn foreground_on_paused_display(&self) -> bool {
        #[cfg(target_os = "windows")]
        {
            return self
                .backend
                .as_ref()
                .is_some_and(Backend::foreground_on_paused);
        }
        #[cfg(not(target_os = "windows"))]
        false
    }

    /// Hide a surface whose host display is paused. Previews die only with the overview.
    fn conceal_suspended_surfaces(&mut self, app: &tauri::AppHandle) {
        let suspended = self.engine.snapshot().suspended_monitors.clone();
        for surface in [Surface::Overview, Surface::Commands] {
            let host = match surface {
                Surface::Overview => self.overview_host.clone(),
                Surface::Commands => self.commands_host.clone(),
            };
            let Some(id) = host else { continue };
            if !suspended.iter().any(|paused| paused == &id) {
                continue;
            }
            if let Some(window) = app.get_webview_window(surface.label()) {
                if window.is_visible().unwrap_or(false) {
                    let _ = window.hide();
                    if matches!(surface, Surface::Overview) {
                        self.clear_previews();
                    }
                }
            }
            self.note_surface(surface, None);
        }
    }
}

/// Hook events still arrive through an input-blocking overlay. Never interpret parked HWNDs
/// as the drawn target; releases and lifetime events must keep draining normally.
#[cfg(target_os = "windows")]
fn compositor_blocks_native_pointer(composing: bool, raw: &crate::platform::hook::Raw) -> bool {
    use crate::platform::hook::Raw;
    composing && matches!(raw, Raw::Grab { .. } | Raw::Move { .. } | Raw::MoveSize { start: true, .. })
}

#[cfg(target_os = "windows")]
impl Controller {
    fn refresh_within(&mut self, delay: Duration) {
        self.refresh_soon = Some(self.refresh_soon.map_or(delay, |d| d.min(delay)));
    }

    fn sync_pointer(&self, config: &Config) {
        use crate::platform::hook;
        let snapshot = self.engine.snapshot();
        let enabled = snapshot.enabled;
        let suspended = |id: &str| snapshot.suspended_monitors.iter().any(|s| s == id);
        hook::configure(hook::Settings {
            targets: match (&self.backend, config.drag_modifier) {
                (Some(backend), Some(_)) if enabled && !backend.composing() => backend
                    .pointer_targets()
                    .into_iter()
                    .filter(|hwnd| {
                        backend
                            .window_for(*hwnd)
                            .is_none_or(|id| !self.engine.window_protected(&id))
                    })
                    .collect(),
                _ => HashSet::new(),
            },
            managed: match &self.backend {
                Some(backend) if enabled => backend.pointer_targets(),
                _ => HashSet::new(),
            },
            modifier: config.drag_modifier,
            moves: (enabled && config.focus_follows_mouse) || !self.revealed.is_empty(),
            zones: snapshot
                .monitors
                .iter()
                .filter(|m| !suspended(&m.monitor.id))
                .flat_map(|m| {
                    let b = m.monitor.bounds;
                    let corner = Rect {
                        width: 1,
                        height: 1,
                        ..b
                    };
                    let edge = Rect { height: 1, ..b };
                    [
                        (enabled && config.hot_corners).then_some(corner),
                        (self.top_bar && !self.bar_pinned(&m.monitor.id)).then_some(edge),
                    ]
                })
                .flatten()
                .collect(),
            suspended: snapshot
                .monitors
                .iter()
                .filter(|m| suspended(&m.monitor.id))
                .map(|m| m.monitor.bounds)
                .collect(),
        });
    }

    /// Drag strips on every active page, hidden while a surface is open.
    fn sync_edges(&self, surface_open: bool) {
        splitter::configure(if surface_open {
            vec![]
        } else {
            splitter::strips(&self.engine)
        });
    }

    fn sync_decorations(&mut self, config: &Config) {
        let Some(backend) = self.backend.as_mut() else {
            return;
        };
        let snapshot = self.engine.snapshot();
        let focused = snapshot
            .enabled
            .then_some(snapshot.focused_window.as_deref())
            .flatten();
        let (border, corners) = decoration_style(config);
        backend.set_decorations(focused, border, corners);
    }

    fn point_on_suspended(&self, x: i32, y: i32) -> bool {
        let snapshot = self.engine.snapshot();
        let (x, y) = (i64::from(x), i64::from(y));
        snapshot.monitors.iter().any(|m| {
            snapshot
                .suspended_monitors
                .iter()
                .any(|id| id == &m.monitor.id)
                && x >= i64::from(m.monitor.bounds.x)
                && y >= i64::from(m.monitor.bounds.y)
                && x < i64::from(m.monitor.bounds.x) + i64::from(m.monitor.bounds.width)
                && y < i64::from(m.monitor.bounds.y) + i64::from(m.monitor.bounds.height)
        })
    }

    /// Reveal an unpinned bar at its monitor's top edge; hide it once the pointer leaves it.
    fn update_bars(&mut self, x: i32, y: i32, corner_active: bool) {
        if !self.top_bar {
            self.revealed.clear();
            return;
        }
        let (x, y) = (x as i64, y as i64);
        let inside = |r: Rect| {
            x >= r.x as i64
                && y >= r.y as i64
                && x < r.x as i64 + r.width as i64
                && y < r.y as i64 + r.height as i64
        };
        for m in &self.engine.snapshot().monitors {
            let id = &m.monitor.id;
            if self
                .engine
                .snapshot()
                .suspended_monitors
                .iter()
                .any(|s| s == id)
            {
                self.revealed.remove(id);
                continue;
            }
            if self.bar_pinned(id) {
                continue;
            }
            let b = m.monitor.bounds;
            let edge = inside(Rect { height: 1, ..b }) && !(corner_active && x == b.x as i64);
            if edge {
                self.revealed.insert(id.clone());
            } else if !inside(bar_rect(&m.monitor)) {
                self.revealed.remove(id);
            }
        }
    }

    /// Returns an overview toggle request when the pointer enters a hot corner.
    fn pointer(&mut self, app: &tauri::AppHandle, config: &Config) -> Option<Request> {
        let mut request = None;
        for raw in crate::platform::hook::drain() {
            match self.pointer_event(app, config, raw) {
                Ok(Some(next)) => request = Some(next),
                Ok(None) => {}
                Err(issue) => {
                    self.gesture = None;
                    self.record(issue);
                }
            }
        }
        request
    }

    fn pointer_event(
        &mut self,
        app: &tauri::AppHandle,
        config: &Config,
        raw: crate::platform::hook::Raw,
    ) -> Result<Option<Request>, AppError> {
        use crate::platform::hook::Raw;
        let blocked = compositor_blocks_native_pointer(
            self.backend.as_ref().is_some_and(Backend::composing),
            &raw,
        );
        if blocked && !matches!(raw, Raw::Move { .. }) {
            return Ok(None);
        }
        match raw {
            Raw::Up => {
                // The hook runs before the dragged window's own loop sees the release.
                if let Some(id) = self.backend.as_ref().and_then(Backend::move_size_window) {
                    self.native_drag = Some(id);
                    self.refresh_within(Duration::from_millis(80));
                }
                return Ok(None);
            }
            Raw::Windows => {
                self.refresh_within(Duration::from_millis(30));
                return Ok(None);
            }
            // A title bar drag of a tiled window previews where it will land.
            Raw::MoveSize {
                hwnd,
                start,
                moving,
            } => {
                let snapshot = self.engine.snapshot();
                if !start {
                    splitter::end_window(None, snapshot.enabled);
                } else if let Some(id) = self
                    .backend
                    .as_ref()
                    .and_then(|b| b.window_for(hwnd))
                    .filter(|id| snapshot.enabled && tiled(snapshot, id))
                {
                    self.native_move = (moving == Some(true)).then(|| id.clone());
                    splitter::begin_window(id, Some(hwnd), moving);
                }
                return Ok(None);
            }
            _ => {}
        }
        let snapshot = self.engine.snapshot();
        let enabled = snapshot.enabled;
        if let (Raw::Release { x, y }, true) = (&raw, self.window_drag) {
            self.window_drag = false;
            splitter::end_window(Some((*x, *y)), enabled);
            return Ok(None);
        }
        if !enabled {
            self.gesture = None;
            if let Raw::Move { x, y, .. } = raw {
                self.update_bars(x, y, false);
            }
            return Ok(None);
        }
        let can_focus = snapshot.backend.capabilities.focus;
        let focused = snapshot.focused_window.clone();
        match raw {
            Raw::Grab { hwnd, x, y } => {
                let Some(id) = self.backend.as_ref().and_then(|b| b.window_for(hwnd)) else {
                    return Ok(None);
                };
                self.gesture = Gesture::start(snapshot, &id, x, y);
                if self.gesture.is_some() {
                    if can_focus && focused.as_ref() != Some(&id) {
                        self.command(Command::FocusWindow { window_id: id })?;
                    }
                } else if tiled(snapshot, &id) {
                    // Tiled windows stay put; the preview shows where the release drops them.
                    self.window_drag = true;
                    splitter::begin_window(id, None, None);
                }
            }
            Raw::Up | Raw::Windows | Raw::MoveSize { .. } => unreachable!("handled above"),
            Raw::Release { x, y } => {
                if let Some(mut gesture) = self.gesture.take() {
                    for command in gesture.update(x, y, true) {
                        self.command(command)?;
                    }
                }
            }
            Raw::Move { x, y, pressed } => {
                if let Some(gesture) = self.gesture.as_mut() {
                    for command in gesture.update(x, y, false) {
                        self.run(command, false)?;
                    }
                    return Ok(None);
                }
                // The drag overlay follows the pointer itself; no hover focus or hot corner.
                if self.window_drag {
                    return Ok(None);
                }
                self.update_bars(x, y, config.hot_corners);
                if self.point_on_suspended(x, y) {
                    return Ok(None);
                }
                let snapshot = self.engine.snapshot();
                let corner = snapshot
                    .monitors
                    .iter()
                    .find(|m| {
                        (m.monitor.bounds.x, m.monitor.bounds.y) == (x, y)
                            && !snapshot.suspended_monitors.iter().any(|id| id == &m.monitor.id)
                    })
                    .filter(|_| config.hot_corners && !pressed)
                    .map(|m| m.monitor.id.clone());
                if corner.is_some() != self.in_corner {
                    self.in_corner = corner.is_some();
                    if let Some(monitor_id) = corner {
                        let open = app
                            .get_webview_window(Surface::Overview.label())
                            .and_then(|w| w.is_visible().ok())
                            .unwrap_or(false);
                        return Ok(Some(if open {
                            Request::Dismiss(
                                Surface::Overview,
                                IpcTrace::begin("native.dismiss_surface"),
                            )
                        } else {
                            Request::Show(Surface::Overview, Some(monitor_id))
                        }));
                    }
                }
                if !config.focus_follows_mouse || pressed || !can_focus || blocked {
                    if blocked {
                        self.hovered = None;
                    }
                    return Ok(None);
                }
                let Some(backend) = &self.backend else {
                    return Ok(None);
                };
                let hovered = backend.window_at(x, y);
                if hovered == self.hovered {
                    return Ok(None);
                }
                self.hovered = hovered.clone();
                let surface_open = [Surface::Overview, Surface::Commands].iter().any(|s| {
                    app.get_webview_window(s.label())
                        .and_then(|w| w.is_visible().ok())
                        .unwrap_or(false)
                });
                if let Some(id) = hovered.filter(|id| Some(id) != focused.as_ref()) {
                    if !surface_open && backend.pointer_focus_allowed() {
                        self.command(Command::FocusWindow { window_id: id })?;
                    }
                }
            }
        }
        Ok(None)
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

#[cfg(target_os = "windows")]
fn decoration_style(config: &Config) -> (Option<u32>, Option<i32>) {
    use windows_sys::Win32::Graphics::Dwm::{DWMWCP_DONOTROUND, DWMWCP_ROUND, DWMWCP_ROUNDSMALL};
    let corners = match config.window_corners {
        crate::config::WindowCorners::System => None,
        crate::config::WindowCorners::Round => Some(DWMWCP_ROUND),
        crate::config::WindowCorners::RoundSmall => Some(DWMWCP_ROUNDSMALL),
        crate::config::WindowCorners::Square => Some(DWMWCP_DONOTROUND),
    };
    (config.focus_colorref(), corners)
}

fn bar_rect(monitor: &crate::model::Monitor) -> Rect {
    let work = monitor.work_area;
    let top = (BAR_HEIGHT * monitor.scale_factor).round() as u32;
    Rect {
        height: top.min(work.height),
        ..work
    }
}

/// Show without activation: revealing a bar must not take focus from the user's window.
fn show_bar(window: &tauri::WebviewWindow, visible: bool, on_top: bool) -> Result<(), AppError> {
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::*;
        let hwnd = window
            .hwnd()
            .map_err(|e| error(ErrorCode::BackendUnavailable, e.to_string()))?
            .0 as windows_sys::Win32::Foundation::HWND;
        unsafe {
            if visible {
                let order = if on_top { HWND_TOPMOST } else { HWND_NOTOPMOST };
                SetWindowPos(
                    hwnd,
                    order,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
                ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            } else {
                ShowWindow(hwnd, SW_HIDE);
            }
        }
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    window
        .set_always_on_top(on_top)
        .and_then(|_| {
            if visible {
                window.show()
            } else {
                window.hide()
            }
        })
        .map_err(|e| error(ErrorCode::OperationDenied, e.to_string()))
}

fn position_controls(
    app: &tauri::AppHandle,
    snapshot: &Snapshot,
    visible: &BTreeSet<String>,
) -> Result<(), AppError> {
    let mut labels = HashSet::new();
    for (index, monitor) in snapshot.monitors.iter().enumerate() {
        let label = control_label("topbar", index);
        labels.insert(label.clone());
        let window = match app.get_webview_window(&label) {
            Some(window) => window,
            None => WebviewWindowBuilder::new(
                app,
                &label,
                WebviewUrl::App(format!("index.html?surface=topbar&monitorIndex={index}").into()),
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
        configure_window(&window, bar_rect(&monitor.monitor))?;
        let id = &monitor.monitor.id;
        // A revealed (unpinned) bar overlays tiled windows, so it must stay on top.
        let pinned = snapshot.pinned_bars.contains(id);
        show_bar(&window, visible.contains(id), snapshot.enabled || !pinned)?;
    }
    for (label, window) in app.webview_windows() {
        if label.starts_with("topbar-") && !labels.contains(&label) {
            let _ = window.hide();
        }
    }
    Ok(())
}

fn surface_monitor<'a>(
    snapshot: &'a Snapshot,
    monitor_id: Option<&str>,
) -> Result<Option<&'a MonitorState>, AppError> {
    match monitor_id {
        Some(id) => snapshot
            .monitors
            .iter()
            .find(|m| m.monitor.id == id)
            .map(Some)
            .ok_or_else(|| error(ErrorCode::InvalidCommand, "目标显示器已断开。")),
        None => Ok(snapshot
            .monitors
            .iter()
            .find(|m| Some(&m.monitor.id) == snapshot.active_monitor.as_ref())
            .or(snapshot.monitors.first())),
    }
}

fn show_surface(
    app: &tauri::AppHandle,
    monitor: Option<&MonitorState>,
    surface: Surface,
    preview_session: Option<u64>,
) -> Result<Option<String>, AppError> {
    let window = app
        .get_webview_window(surface.label())
        .ok_or_else(|| error(ErrorCode::BackendUnavailable, "控制窗口不存在。"))?;
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
    let host = monitor.map(|m| m.monitor.id.clone());
    app.emit_to(
        surface.label(),
        "surface-opened",
        serde_json::json!({ "monitorId": host, "previewSession": preview_session }),
    )
    .map_err(|e| error(ErrorCode::BackendUnavailable, e.to_string()))?;
    Ok(host)
}

fn set_shortcut(
    app: &tauri::AppHandle,
    sender: &RequestSender,
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
    sender: &RequestSender,
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
                controller.engine.set_gaps(shortcuts.config.gaps);
                controller
                    .engine
                    .set_layout_options((&shortcuts.config).into()); // lane: layout-options
                controller.top_bar = shortcuts.config.top_bar;
                if !controller.top_bar {
                    controller.revealed.clear();
                }
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
    receiver: RequestReceiver,
    shared: Arc<Mutex<Snapshot>>,
    sender: RequestSender,
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
    #[cfg(target_os = "windows")]
    let _hook = {
        let sender = sender.clone();
        crate::platform::hook::start(Box::new(move || {
            let _ = sender.try_send(Request::Pointer);
        }))
        .map_err(|issue| controller.record(issue))
        .ok()
    };
    #[cfg(target_os = "windows")]
    {
        controller.autohide = _hook.is_some();
    }
    #[cfg(target_os = "windows")]
    let _splitter = {
        let sender = sender.clone();
        crate::platform::splitter::start(Box::new(move || {
            let _ = sender.try_send(Request::Edges);
        }))
        .map_err(|issue| controller.record(issue))
        .ok()
    };
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
        controller.settle();
        let snapshot = controller.snapshot(shortcuts.available());
        // Native surface positions change only with monitor geometry or enabled state.
        let visible = controller.visible_bars();
        let geometry = serde_json::to_string(&(
            snapshot.enabled,
            &visible,
            &snapshot.pinned_bars,
            snapshot
                .monitors
                .iter()
                .map(|m| &m.monitor)
                .collect::<Vec<_>>(),
        ))
        .unwrap_or_default();
        if geometry != controls {
            match position_controls(&app, &snapshot, &visible) {
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
        #[cfg(target_os = "windows")]
        {
            controller.conceal_suspended_surfaces(&app);
            controller.sync_pointer(&shortcuts.config);
            let surface_open = [Surface::Overview, Surface::Commands].iter().any(|s| {
                app.get_webview_window(s.label())
                    .and_then(|w| w.is_visible().ok())
                    .unwrap_or(false)
            });
            // Publish pause state before SYNC can inspect the configured strips.
            splitter::publish(controller.engine.clone());
            controller.sync_edges(surface_open);
            controller.sync_decorations(&shortcuts.config);
        }
        let next_deadline = controller
            .animation
            .deadline()
            .map_or(next_refresh, |frame| frame.min(next_refresh));
        let request = match receive_request(&receiver, next_deadline) {
            Ok(Request::Shortcut(key)) => {
                let Some(action) = shortcuts.action(&key).cloned() else {
                    continue;
                };
                let suspended = controller.engine.snapshot().suspended_monitors.clone();
                if controller.foreground_on_paused_display()
                    && crate::config::foreground_blocks_shortcut(&action, &suspended)
                {
                    continue;
                }
                match action.resolve(controller.engine.snapshot()) {
                    Some(ShortcutAction::Command { command }) => Ok(Request::Command(
                        command,
                        None,
                        IpcTrace::begin("shortcut.command"),
                    )),
                    Some(ShortcutAction::Overview {}) => Ok(Request::Show(Surface::Overview, None)),
                    Some(ShortcutAction::Commands {}) => Ok(Request::Show(Surface::Commands, None)),
                    Some(ShortcutAction::Quit {}) => Ok(Request::Quit),
                    _ => continue,
                }
            }
            request => request,
        };
        match request {
            Ok(Request::Command(mut command, reply, trace)) => {
                let _request = trace.scope("controller.enter", "controller.exit");
                controller.trace = trace;
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
                let before = controller.engine.snapshot().focused_window.clone();
                let result = controller.command(command);
                if let Err(issue) = &result {
                    controller.record(issue.clone());
                } else if shortcuts.config.warp_mouse_to_focus {
                    controller.warp_to_focus(before.as_ref());
                }
                trace.mark("snapshot.publish.enter");
                let current = controller.snapshot(shortcuts.available());
                *shared.lock().unwrap_or_else(|e| e.into_inner()) = current.clone();
                let _ = app.emit("snapshot", &current);
                trace.mark("snapshot.publish.exit");
                if let Some(reply) = reply {
                    trace.mark("reply.send.enter");
                    trace.mark(if reply.send(result.map(|_| current)).is_ok() {
                        "reply.sent"
                    } else {
                        "reply.disconnected"
                    });
                }
                controller.trace = IpcTrace::NONE;
            }
            Ok(Request::Shortcut(_)) => unreachable!("shortcut resolved above"),
            Ok(Request::PinBar(monitor_id, pinned)) => {
                // Unpinning under the pointer keeps the bar until the pointer leaves it.
                if pinned {
                    controller.revealed.remove(&monitor_id);
                    controller.pinned.insert(monitor_id);
                } else {
                    controller.pinned.remove(&monitor_id);
                    controller.revealed.insert(monitor_id);
                }
                // Pinned bars reserve their height; reflow the tiled windows now.
                if controller.backend.is_some() {
                    if let Err(issue) = controller.refresh(true) {
                        controller.record(issue);
                    }
                }
            }
            #[cfg(target_os = "windows")]
            Ok(Request::Edges) => {
                for drop in splitter::drain() {
                    // A window carried by its title bar is already where it was dropped:
                    // place it straight into its slot and skip adopting the native move.
                    if let Some(id) = &drop.carried {
                        controller.native_drag = None;
                        controller.native_move = None;
                        controller.placements.remove(id);
                    }
                    // Moved windows slide into place; boundary drags resize once, unanimated.
                    let animate = matches!(drop.command, Command::DropWindow { .. });
                    if let Err(issue) = controller.run(drop.command, animate) {
                        controller.record(issue);
                    }
                }
            }
            #[cfg(target_os = "windows")]
            Ok(Request::Pointer) => {
                if let Some(request) = controller.pointer(&app, &shortcuts.config) {
                    let _ = sender.try_send(request);
                }
                if let Some(delay) = controller.refresh_soon.take() {
                    // Give the released window's loop (or a closing app) a moment first.
                    next_refresh = next_refresh.min(Instant::now() + delay);
                }
            }
            Ok(Request::Show(surface, monitor_id)) => {
                let host = match surface_monitor(&snapshot, monitor_id.as_deref()) {
                    Ok(host) => host,
                    Err(issue) => {
                        controller.record(issue);
                        continue;
                    }
                };
                let suspended = controller.engine.snapshot().suspended_monitors.clone();
                if crate::config::foreground_blocks_show(
                    monitor_id.as_deref(),
                    host.map(|m| m.monitor.id.as_str()),
                    controller.foreground_on_paused_display(),
                    &suspended,
                ) {
                    continue;
                }
                if let Err(issue) = controller.finish_animation() {
                    controller.record(issue);
                    continue;
                }
                controller.clear_previews();
                let available =
                    matches!(surface, Surface::Overview) && controller.preview_capable();
                let session = controller.preview_session.begin(available);
                match show_surface(&app, host, surface, session) {
                    Ok(host) => {
                        controller.note_surface(surface, host);
                        let other = match surface {
                            Surface::Overview => Surface::Commands,
                            Surface::Commands => Surface::Overview,
                        };
                        controller.note_surface(other, None);
                    }
                    Err(issue) => {
                        controller.clear_previews();
                        controller.record(issue);
                    }
                }
            }
            Ok(Request::Previews(session, slots, reply, trace)) => {
                let _request = trace.scope("controller.enter", "controller.exit");
                controller.trace = trace;
                let result = controller.sync_previews(&app, session, &slots);
                trace.mark("reply.send.enter");
                trace.mark(if reply.send(result).is_ok() {
                    "reply.sent"
                } else {
                    "reply.disconnected"
                });
                controller.trace = IpcTrace::NONE;
            }
            Ok(Request::Dismiss(surface, trace)) => {
                let _request = trace.scope("controller.enter", "controller.exit");
                controller.trace = trace;
                controller.note_surface(surface, None);
                if matches!(surface, Surface::Overview) {
                    controller.clear_previews();
                }
                if let Some(window) = app.get_webview_window(surface.label()) {
                    let _hide = trace.scope("surface.hide.enter", "surface.hide.exit");
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
                controller.trace = IpcTrace::NONE;
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
            Err(ReceiveError::Disconnected) => {
                if let Err(issue) = controller.stop() {
                    eprintln!("窗口控制器停止时还原失败：{}", issue.message);
                }
                break;
            }
            Err(ReceiveError::Timeout) => {}
            #[cfg(target_os = "windows")]
            Err(ReceiveError::Failed(issue)) => {
                controller.record(issue);
                if let Err(issue) = controller.stop() {
                    eprintln!("控制器消息等待失败后还原失败：{}", issue.message);
                }
                let current = controller.snapshot(shortcuts.available());
                *shared.lock().unwrap_or_else(|e| e.into_inner()) = current.clone();
                let _ = app.emit("snapshot", &current);
                break;
            }
        }
    }
}

pub fn run() {
    let (sender, receiver) = request_queue().expect("failed to create desktop controller queue");
    let shared = Arc::new(Mutex::new(Snapshot::default()));
    let state = AppState {
        sender: sender.clone(),
        snapshot: shared.clone(),
        can_exit: AtomicBool::new(false),
    };
    tauri::Builder::default()
        .manage(state)
        .manage(crate::terminal::TerminalHost::default())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            crate::terminal::terminal_endpoint,
            crate::terminal::open_terminal_window,
            get_snapshot,
            execute,
            open_surface,
            dismiss_surface,
            sync_previews,
            set_bar_pinned,
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
            if window.label().starts_with("terminal-") {
                if matches!(event, tauri::WindowEvent::Destroyed) { crate::terminal::unregister(window); }
                return; // Closing a view detaches; only explicit terminal.close kills a shell.
            }
            if matches!(event, tauri::WindowEvent::Destroyed) && window.label() == "overview" {
                let _ = send(
                    &window.state::<AppState>(),
                    Request::Dismiss(Surface::Overview, IpcTrace::begin("native.dismiss_surface")),
                );
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let state = window.state::<AppState>();
                let request = match window.label() {
                    "overview" => Request::Dismiss(
                        Surface::Overview,
                        IpcTrace::begin("native.dismiss_surface"),
                    ),
                    "commands" => Request::Dismiss(
                        Surface::Commands,
                        IpcTrace::begin("native.dismiss_surface"),
                    ),
                    _ => Request::Quit,
                };
                let _ = send(&state, request);
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to build e-desktop")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) { app.state::<crate::terminal::TerminalHost>().shutdown(); }
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                let state = app.state::<AppState>();
                if !state.can_exit.load(Ordering::Acquire) {
                    api.prevent_exit();
                    let _ = send(&state, Request::Quit);
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Monitor;

    #[test]
    fn open_surfaces_hold_native_focus_through_drop_resize_and_dismiss_handoff() {
        use crate::model::{Capabilities, NativeWindow, SystemSnapshot};
        let area = Rect { x: 0, y: 0, width: 1000, height: 800 };
        let mut engine = Engine::new(BackendStatus {
            availability: BackendAvailability::Ready,
            capabilities: Capabilities {
                enumerate: true, placement: true, focus: true, minimize: true, clipping: true,
                ..Capabilities::default()
            },
            ..BackendStatus::default()
        });
        engine.reconcile(SystemSnapshot {
            monitors: vec![Monitor {
                id: "m".into(), name: String::new(), bounds: area, work_area: area,
                scale_factor: 1.0, primary: true,
            }],
            windows: ["source", "target"].into_iter().map(|id| NativeWindow {
                id: id.into(), title: String::new(), app_name: String::new(),
                process_id: 1, monitor_id: "m".into(), rect: area,
                minimized: false, minimized_by_manager: false, resizable: true,
            }).collect(),
            focused_window: Some("source".into()),
        }).unwrap();
        engine.dispatch(Command::Enable).unwrap();
        let mut actions = vec![];
        for id in ["source", "target"] {
            actions = engine.dispatch(Command::SetWindowColumnWidth {
                window_id: id.into(), width: 500,
            }).unwrap().actions;
        }
        // No real Backend is created: any unfiltered native action reports unavailable.
        let mut controller = Controller {
            trace: IpcTrace::NONE, backend: None, engine, errors: vec![],
            config_error: None, window_rules: vec![], placements: Placements::new(),
            refused: Placements::new(), animation: Animation::default(),
            animation_duration: Duration::from_millis(160),
            preview_session: PreviewSession::default(), gesture: None, window_drag: false,
            hovered: None, in_corner: false, top_bar: true, native_drag: None,
            native_move: None, ignored_foreground: None, refresh_soon: None,
            autohide: false, pinned: BTreeSet::new(), revealed: BTreeSet::new(),
            overview_host: None, commands_host: None,
        };
        controller.note_surface(Surface::Overview, Some("m".into()));
        let before = controller.engine.snapshot().clone();
        let page_id = before.monitors[0].active_page.clone();
        let drop = controller.engine.dispatch(Command::DropWindow {
            window_id: "source".into(), x: 750, y: 600,
            page_id: Some(page_id), viewport_x: Some(0),
        }).unwrap();
        assert_eq!(drop.snapshot.focused_window.as_deref(), Some("source"));
        assert!(drop.snapshot.monitors[0].pages[0].columns[0].windows.contains(&"target".into()));
        let focus: Vec<_> = drop.actions.iter()
            .filter(|action| matches!(action, NativeAction::Focus { .. })).cloned().collect();
        assert_eq!(focus, vec![NativeAction::Focus { window_id: "source".into() }]);
        controller.apply(&focus).unwrap();

        // A focus emitted by a later animation frame uses the same apply gate.
        let mut applied = Placements::new();
        for action in actions {
            if let NativeAction::Placement { window_id, rect, clip, minimized } = action {
                applied.insert(window_id, (rect, clip, minimized));
            }
        }
        applied.get_mut("source").unwrap().0.x = 1100;
        let now = Instant::now();
        controller.animation.start(&before, &drop.snapshot, &applied, drop.actions,
            controller.animation_duration, now);
        assert_eq!(controller.animation.deferred_focus_id(), Some("source"));
        let final_focus: Vec<_> = controller.animation.frame(now + controller.animation_duration)
            .into_iter().filter(|action| matches!(action, NativeAction::Focus { .. })).collect();
        assert_eq!(final_focus, focus);
        controller.apply(&final_focus).unwrap();

        for command in [
            Command::SetWindowColumnWidth { window_id: "target".into(), width: 650 },
            Command::SetColumnWidth { width: 700 },
        ] {
            let resized = controller.engine.dispatch(command).unwrap();
            assert_eq!(resized.snapshot.focused_window.as_deref(), Some("source"));
            assert!(!resized.actions.iter().any(|action| matches!(action, NativeAction::Focus { .. })));
        }
        let enter = controller.engine.dispatch(Command::FocusWindow {
            window_id: "target".into(),
        }).unwrap();
        let focus: Vec<_> = enter.actions.into_iter()
            .filter(|action| matches!(action, NativeAction::Focus { .. })).collect();
        assert_eq!(controller.engine.snapshot().focused_window.as_deref(), Some("target"));
        controller.apply(&focus).unwrap(); // Card selection changes logical focus first.
        controller.note_surface(Surface::Overview, None);
        let handoff = controller.engine.dispatch(Command::FocusWindow {
            window_id: controller.engine.snapshot().focused_window.clone().unwrap(),
        }).unwrap();
        let focus: Vec<_> = handoff.actions.into_iter()
            .filter(|action| matches!(action, NativeAction::Focus { .. })).collect();
        assert_eq!(focus, vec![NativeAction::Focus { window_id: "target".into() }]);
        assert_eq!(controller.apply(&focus).unwrap_err().code, ErrorCode::BackendUnavailable);

        controller.note_surface(Surface::Commands, Some("m".into()));
        controller.apply(&focus).unwrap();
        controller.note_surface(Surface::Overview, None); // Another surface still owns input.
        controller.apply(&focus).unwrap();
        controller.note_surface(Surface::Commands, None);
        assert_eq!(controller.apply(&focus).unwrap_err().code, ErrorCode::BackendUnavailable);
        // Placement is never suppressed by the surface focus gate.
        controller.note_surface(Surface::Overview, Some("m".into()));
        assert_eq!(controller.apply(&[NativeAction::Placement {
            window_id: "target".into(), rect: area, clip: None, minimized: false,
        }]).unwrap_err().code, ErrorCode::BackendUnavailable);
    }

    #[test]
    fn ipc_trace_is_opt_in_correlated_and_tolerates_logging_failure() {
        use std::ffi::OsStr;
        assert!(ipc_trace_enabled(Some(OsStr::new("1"))));
        for value in [None, Some(""), Some("0"), Some("true")] {
            assert!(!ipc_trace_enabled(value.map(OsStr::new)));
        }
        let mut output = Vec::new();
        let off = IpcTrace::with_diagnostics("execute.dropWindow", None);
        off.write("ipc.enter", &mut output);
        assert!(output.is_empty());
        assert_eq!(off.id, 0);

        static DIAGNOSTICS: OnceLock<IpcDiagnostics> = OnceLock::new();
        let diagnostics = DIAGNOSTICS.get_or_init(|| IpcDiagnostics {
            started: Instant::now(),
            sequence: AtomicU64::new(1),
        });
        let trace = IpcTrace::with_diagnostics("execute.dropWindow", Some(diagnostics));
        let next = IpcTrace::with_diagnostics("get_snapshot", Some(diagnostics));
        assert_eq!(next.id, trace.id + 1);
        let (sender, receiver) = request_queue().unwrap();
        let state = AppState {
            sender, snapshot: Arc::new(Mutex::new(Snapshot::default())),
            can_exit: AtomicBool::new(false),
        };
        send(&state, Request::Command(Command::Refresh, None, trace)).unwrap();
        let request = receive_request(&receiver, Instant::now())
            .unwrap_or_else(|_| panic!("request missing"));
        let Request::Command(Command::Refresh, None, received) = request else {
            panic!("unexpected request");
        };
        assert_eq!(received.id, trace.id);
        assert_eq!(received.operation, trace.operation);
        received.write("enqueue.accepted", &mut output);
        trace.write("controller.enter", &mut output);
        next.write("ipc.enter", &mut output);
        let lines = String::from_utf8(output).unwrap();
        let lines: Vec<_> = lines.lines().collect();
        assert_eq!(lines.len(), 3);
        for (line, id) in lines.iter().zip([trace.id, trace.id, next.id]) {
            assert!(line.starts_with("[ipc-trace] us="));
            assert!(line.contains(" thread=ThreadId("));
            assert!(line.contains(&format!(" request={id} ")));
        }
        assert!(lines[0].ends_with("op=execute.dropWindow stage=enqueue.accepted"));
        assert!(lines[1].ends_with("op=execute.dropWindow stage=controller.enter"));
        assert!(lines[2].ends_with("op=get_snapshot stage=ipc.enter"));
        // std's empty slice writer returns an error; tracing must not propagate/panic.
        trace.write("ipc.exit", &mut &mut [][..]);
        let process_trace = IpcTrace::begin("test");
        assert_eq!(process_trace.diagnostics.is_some(),
            ipc_trace_enabled(std::env::var_os("E_DESKTOP_TRACE_IPC").as_deref()));
        let scope = process_trace.scope("test.enter", "test.exit");
        assert_eq!(scope.trace.id, process_trace.id);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn compositor_pointer_gate_blocks_starts_but_keeps_release_and_cleanup_events() {
        use crate::platform::hook::Raw;
        for raw in [
            Raw::Grab { hwnd: 1, x: 0, y: 0 },
            Raw::Move { x: 0, y: 0, pressed: false },
            Raw::MoveSize { hwnd: 1, start: true, moving: Some(true) },
        ] {
            assert!(compositor_blocks_native_pointer(true, &raw));
            assert!(!compositor_blocks_native_pointer(false, &raw));
        }
        for raw in [
            Raw::Release { x: 0, y: 0 }, Raw::Up, Raw::Windows,
            Raw::MoveSize { hwnd: 1, start: false, moving: None },
        ] {
            assert!(!compositor_blocks_native_pointer(true, &raw));
        }
    }

    #[test]
    fn surface_gate_checks_the_selected_host_before_showing() {
        let mut snapshot = Snapshot {
            monitors: ["a", "b", "c"]
                .into_iter()
                .map(|id| MonitorState {
                    monitor: Monitor {
                        id: id.into(), name: id.into(), bounds: Rect::default(),
                        work_area: Rect::default(), scale_factor: 1.0, primary: id == "a",
                    },
                    pages: vec![], active_page: String::new(), viewport: Rect::default(),
                })
                .collect(),
            active_monitor: Some("a".into()),
            suspended_monitors: vec!["a".into()],
            ..Snapshot::default()
        };
        let blocked = |snapshot: &Snapshot, requested: Option<&str>, foreground_paused| {
            let host = surface_monitor(snapshot, requested).unwrap();
            crate::config::foreground_blocks_show(
                requested, host.map(|m| m.monitor.id.as_str()), foreground_paused,
                &snapshot.suspended_monitors,
            )
        };
        // Native foreground moved to an ordinary unmanaged B window; layout still names A.
        assert!(blocked(&snapshot, None, false));
        assert!(!blocked(&snapshot, Some("b"), true));
        assert!(blocked(&snapshot, Some("a"), false));
        assert!(surface_monitor(&snapshot, Some("missing")).is_err());
        // The first-monitor fallback must obey the same pause gate.
        snapshot.active_monitor = Some("disconnected".into());
        assert!(blocked(&snapshot, None, false));
        snapshot.active_monitor = Some("b".into());
        assert!(!blocked(&snapshot, None, false));
        assert!(blocked(&snapshot, None, true));
    }
}
