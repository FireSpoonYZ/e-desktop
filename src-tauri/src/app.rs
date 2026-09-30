use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
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
    animation::{Animation, Placements},
    config::{Config, ConfigFile, ShortcutAction},
    layout::{Engine, expand_gap, half_gap, inset_gap},
    model::{
        AppError, BackendAvailability, BackendStatus, Command, ErrorCode, NativeAction, Rect,
        Snapshot,
    },
    platform::Backend,
    pointer::{Gesture, tiled},
    preview::{PreviewSession, PreviewSlot, PreviewStatus},
    rules::WindowRule,
    shortcuts::{Shortcuts, normalize_key},
};

#[cfg(target_os = "windows")]
use crate::platform::splitter;

const BAR_HEIGHT: f64 = 36.0;
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
    Previews(
        u64,
        Vec<PreviewSlot>,
        mpsc::SyncSender<Result<Vec<PreviewStatus>, AppError>>,
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
async fn sync_previews(
    window: tauri::WebviewWindow,
    session: u64,
    slots: Vec<PreviewSlot>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<PreviewStatus>, AppError> {
    if window.label() != "overview" {
        return Err(error(
            ErrorCode::InvalidCommand,
            "实时预览仅供概览窗口使用。",
        ));
    }
    let (tx, rx) = mpsc::sync_channel(1);
    send(&state, Request::Previews(session, slots, tx))?;
    tauri::async_runtime::spawn_blocking(move || {
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
    /// Refresh within this delay: a released native move/resize, or windows that appeared or
    /// disappeared (their neighbours close the gap right away instead of at the next poll).
    refresh_soon: Option<Duration>,
    /// Whether bars can auto-hide (needs the pointer hook).
    autohide: bool,
    pinned: BTreeSet<String>,
    revealed: BTreeSet<String>,
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
            refused: Placements::new(),
            animation: Animation::default(),
            preview_session: PreviewSession::default(),
            gesture: None,
            window_drag: false,
            hovered: None,
            in_corner: false,
            top_bar: Config::default().top_bar,
            native_drag: None,
            refresh_soon: None,
            autohide: false,
            pinned: BTreeSet::new(),
            revealed: BTreeSet::new(),
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
        let backend = Backend::new()?;
        let mut engine = Engine::new(backend.status());
        engine.set_window_rules(self.window_rules.clone())?;
        engine.set_gaps(gaps);
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
            .filter(|id| self.bar_pinned(id) || self.revealed.contains(id))
            .collect()
    }

    fn refresh(&mut self, apply: bool) -> Result<(), AppError> {
        // Keep a running slide when polling finds the same layout. A focus waiting for its
        // target to arrive must survive reconciliation without adopting the stale foreground.
        let pending_focus = self.animation.deferred_focus().cloned();
        let backend = self
            .backend
            .as_mut()
            .ok_or_else(|| error(ErrorCode::BackendUnavailable, "原生窗口后端尚未连接。"))?;
        let system = backend.enumerate();
        self.engine.set_backend(backend.status());
        let mut system = system?;
        if pending_focus.is_some() {
            // The old foreground window is not a new activation: the target is still
            // arriving. Preserve layout focus until the deferred activation is applied.
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
            if let Some((planned, id)) =
                dragged.and_then(|id| Some((self.placements.get(&id).filter(|p| !p.2)?.0, id)))
            {
                let adopted = match self.engine.adopt_native_move(&id, planned)? {
                    Some(moved) => Some(moved),
                    None => self.engine.adopt_native_sizes(&[(id, planned)])?,
                };
                if let Some(adopted) = adopted {
                    transition = adopted;
                }
            }
            // Observation must not steal focus from an open palette, menu or dialog.
            let actions: Vec<_> = transition
                .actions
                .into_iter()
                .filter(|a| !matches!(a, NativeAction::Focus { .. }))
                .chain(pending_focus)
                .collect();
            // Opened/closed windows push neighbours: animate those from where they are on
            // screen. A window the user moved or resized itself starts at its target.
            let windows = &transition.snapshot.windows;
            let on_screen: Placements = self
                .placements
                .iter()
                .filter(|(id, (rect, _, hidden))| {
                    windows.iter().any(|w| {
                        &w.native.id == *id
                            && if *hidden {
                                w.native.minimized_by_manager
                            } else {
                                !w.native.minimized && w.native.rect == *rect
                            }
                    })
                })
                .map(|(id, plan)| (id.clone(), *plan))
                .collect();
            let actions = self.animation.start(
                &prev,
                &transition.snapshot,
                &on_screen,
                actions,
                self.animation_duration,
                Instant::now(),
            );
            self.present(actions, true)?;
        } else {
            self.animation.cancel();
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
        let prev = self.engine.snapshot().clone();
        let transition = match self.engine.dispatch(command) {
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
        self.present(actions, animate && layout)
    }

    /// Apply animation output. On Windows the compositor overlay draws the frames (cropped to
    /// each monitor, as in niri) while the real windows move once; elsewhere, or if the
    /// overlay fails, every frame moves the real windows.
    fn present(&mut self, actions: Vec<NativeAction>, started: bool) -> Result<(), AppError> {
        #[cfg(target_os = "windows")]
        if self.animation.deadline().is_some() && self.backend.is_some() {
            let sprites = self.sprites();
            let animated: HashSet<String> = self
                .animation
                .sprites()
                .into_iter()
                .map(|s| s.window_id)
                .collect();
            let (frames, rest): (Vec<_>, Vec<_>) = actions.into_iter().partition(|a| {
                matches!(a, NativeAction::Placement { window_id, .. } if animated.contains(window_id))
            });
            let backend = self.backend.as_mut().unwrap();
            match backend.compose(&sprites) {
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
        let snapshot = self.engine.snapshot();
        let areas: Vec<Rect> = sprites.iter().map(|s| s.bounds).collect();
        for monitor in &snapshot.monitors {
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
            let hwnd = window
                .hwnd()
                .map_err(|e| error(ErrorCode::BackendUnavailable, e.to_string()))?;
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
        let actions = self.animation.cancel();
        self.apply(&actions)
    }

    fn animate(&mut self, now: Instant) -> Result<(), AppError> {
        let actions = self.animation.frame(now);
        let result = self.present(actions, false);
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
            if let (Some(backend), Some((rect, clip, false))) =
                (&self.backend, self.placements.get(id).copied())
            {
                backend.warp_pointer(clip.unwrap_or(rect));
            }
        }
        #[cfg(not(target_os = "windows"))]
        let _ = before;
    }
}

#[cfg(target_os = "windows")]
impl Controller {
    fn refresh_within(&mut self, delay: Duration) {
        self.refresh_soon = Some(self.refresh_soon.map_or(delay, |d| d.min(delay)));
    }

    fn sync_pointer(&self, config: &Config) {
        use crate::platform::hook;
        let enabled = self.engine.snapshot().enabled;
        hook::configure(hook::Settings {
            targets: match (&self.backend, config.drag_modifier) {
                (Some(backend), Some(_)) if enabled => backend.pointer_targets(),
                _ => HashSet::new(),
            },
            managed: match &self.backend {
                Some(backend) if enabled => backend.pointer_targets(),
                _ => HashSet::new(),
            },
            modifier: config.drag_modifier,
            moves: (enabled && config.focus_follows_mouse) || !self.revealed.is_empty(),
            zones: self
                .engine
                .snapshot()
                .monitors
                .iter()
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
            Raw::MoveSize { hwnd, start } => {
                let snapshot = self.engine.snapshot();
                if !start {
                    splitter::end_window(None, snapshot.enabled);
                } else if let Some(id) = self
                    .backend
                    .as_ref()
                    .and_then(|b| b.window_for(hwnd))
                    .filter(|id| snapshot.enabled && tiled(snapshot, id))
                {
                    splitter::begin_window(id, Some(hwnd));
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
                    splitter::begin_window(id, None);
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
                let snapshot = self.engine.snapshot();
                let corner = snapshot
                    .monitors
                    .iter()
                    .find(|m| (m.monitor.bounds.x, m.monitor.bounds.y) == (x, y))
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
                            Request::Dismiss(Surface::Overview)
                        } else {
                            Request::Show(Surface::Overview, Some(monitor_id))
                        }));
                    }
                }
                if !config.focus_follows_mouse || pressed || !can_focus {
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

fn show_surface(
    app: &tauri::AppHandle,
    snapshot: &Snapshot,
    surface: Surface,
    monitor_id: Option<&str>,
    preview_session: Option<u64>,
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
        serde_json::json!({ "monitorId": monitor.map(|m| &m.monitor.id), "previewSession": preview_session }),
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
                controller.engine.set_gaps(shortcuts.config.gaps);
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
            controller.sync_pointer(&shortcuts.config);
            let surface_open = [Surface::Overview, Surface::Commands].iter().any(|s| {
                app.get_webview_window(s.label())
                    .and_then(|w| w.is_visible().ok())
                    .unwrap_or(false)
            });
            controller.sync_edges(surface_open);
            // Drags preview against the current layout (including row heights).
            splitter::publish(controller.engine.clone());
            controller.sync_decorations(&shortcuts.config);
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
                let before = controller.engine.snapshot().focused_window.clone();
                let result = controller.command(command);
                if let Err(issue) = &result {
                    controller.record(issue.clone());
                } else if shortcuts.config.warp_mouse_to_focus {
                    controller.warp_to_focus(before.as_ref());
                }
                let current = controller.snapshot(shortcuts.available());
                *shared.lock().unwrap_or_else(|e| e.into_inner()) = current.clone();
                let _ = app.emit("snapshot", &current);
                if let Some(reply) = reply {
                    let _ = reply.send(result.map(|_| current));
                }
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
                if let Err(issue) = controller.finish_animation() {
                    controller.record(issue);
                    continue;
                }
                controller.clear_previews();
                let available =
                    matches!(surface, Surface::Overview) && controller.preview_capable();
                let session = controller.preview_session.begin(available);
                if let Err(issue) =
                    show_surface(&app, &snapshot, surface, monitor_id.as_deref(), session)
                {
                    controller.clear_previews();
                    controller.record(issue);
                }
            }
            Ok(Request::Previews(session, slots, reply)) => {
                let result = controller.sync_previews(&app, session, &slots);
                let _ = reply.send(result);
            }
            Ok(Request::Dismiss(surface)) => {
                if matches!(surface, Surface::Overview) {
                    controller.clear_previews();
                }
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
            if matches!(event, tauri::WindowEvent::Destroyed) && window.label() == "overview" {
                let _ = send(
                    &window.state::<AppState>(),
                    Request::Dismiss(Surface::Overview),
                );
            }
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
