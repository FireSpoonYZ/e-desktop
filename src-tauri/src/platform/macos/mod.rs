//! Local AX window control, not a compositor. Create/use/drop Backend on one OS worker.
mod ffi;
mod geometry;
mod screens;

use crate::model::*;
use ffi::*;
use std::collections::BTreeMap;

fn error(code: ErrorCode, message: impl Into<String>, id: Option<&str>) -> AppError {
    AppError {
        code,
        message: message.into(),
        window_id: id.map(str::to_owned),
    }
}
fn ax_error(code: i32, operation: &str, id: Option<&str>) -> AppError {
    error(
        match code {
            -25202 => ErrorCode::WindowGone,
            -25211 => ErrorCode::PermissionRequired,
            _ => ErrorCode::OperationDenied,
        },
        format!("{operation}: Accessibility error {code}"),
        id,
    )
}
#[derive(Clone, Copy)]
struct Original {
    frame: Frame,
    minimized: bool,
}
struct Window {
    element: Owned,
    app: Owned,
    pid: i32,
    original: Option<Original>,
    minimized_by_manager: bool,
}
/// Contains non-Send CF/AX references; integration owns this on a dedicated thread.
pub struct Backend {
    windows: BTreeMap<WindowId, Window>,
    next_id: u64,
    monitors: Vec<Monitor>,
    screen_error: Option<AppError>,
}
impl Backend {
    pub fn new() -> Result<Self, AppError> {
        let mut backend = Self {
            windows: BTreeMap::new(),
            next_id: 1,
            monitors: Vec::new(),
            screen_error: None,
        };
        // Reading displays is harmless; never prompt, enable, or move a window here.
        backend.refresh_screens();
        Ok(backend)
    }
    fn refresh_screens(&mut self) {
        match screens::monitors() {
            Ok(monitors) => {
                self.monitors = monitors;
                self.screen_error = None;
            }
            Err(error) => {
                self.monitors.clear();
                self.screen_error = Some(error);
            }
        }
    }
    pub fn status(&self) -> BackendStatus {
        let trusted = unsafe { AXIsProcessTrusted() != 0 };
        let ready = trusted && self.screen_error.is_none();
        BackendStatus {
            kind: BackendKind::MacOs,
            availability: if !trusted {
                BackendAvailability::PermissionRequired
            } else if self
                .screen_error
                .as_ref()
                .is_some_and(|e| e.code == ErrorCode::UnsupportedSession)
            {
                BackendAvailability::UnsupportedSession
            } else if !ready {
                BackendAvailability::Unavailable
            } else {
                BackendAvailability::Ready
            },
            capabilities: Capabilities {
                enumerate: ready,
                placement: ready,
                focus: ready,
                close: ready,
                minimize: ready,
                ..Capabilities::default()
            },
            message: if !trusted {
                "Grant this running app Accessibility access in System Settings > Privacy & Security > Accessibility, then refresh. The backend never changes security settings.".into()
            } else if let Some(error) = &self.screen_error {
                error.message.clone()
            } else {
                "AX controls standard movable/resizable windows on uniform-scale displays. No arbitrary clipping, compositor isolation, native fullscreen, global shortcuts or focus-follow-pointer. Other Spaces and application-specific AX refusals may limit control.".into()
            },
        }
    }
    fn permission(&self) -> Result<(), AppError> {
        if unsafe { AXIsProcessTrusted() } == 0 {
            return Err(error(
                ErrorCode::PermissionRequired,
                self.status().message,
                None,
            ));
        }
        Ok(())
    }
    pub fn enumerate(&mut self) -> Result<SystemSnapshot, AppError> {
        self.permission()?;
        self.refresh_screens();
        if let Some(error) = &self.screen_error {
            return Err(error.clone());
        }
        let scale = self.monitors[0].scale_factor;
        let pids = process_ids()?;
        let own_pid = std::process::id() as i32;
        let mut result = SystemSnapshot {
            monitors: self.monitors.clone(),
            ..SystemSnapshot::default()
        };
        let mut seen = std::collections::BTreeSet::new();
        for pid in pids.into_iter().filter(|pid| *pid > 0) {
            let Some(app) = (unsafe { Owned::from_create(AXUIElementCreateApplication(pid)) })
            else {
                continue;
            };
            unsafe {
                AXUIElementSetMessagingTimeout(app.0, 0.5);
            }
            let list = match attribute(app.0, "AXWindows") {
                Ok(list) => list,
                Err(code) => {
                    if code != -25202
                        && self
                            .windows
                            .values()
                            .any(|w| w.pid == pid && w.original.is_some())
                    {
                        return Err(ax_error(
                            code,
                            "Enumerating managed application windows",
                            None,
                        ));
                    }
                    continue;
                }
            };
            if unsafe { CFGetTypeID(list.0) != CFArrayGetTypeID() } {
                continue;
            }
            let app_name = attribute(app.0, "AXTitle")
                .ok()
                .and_then(|v| text(v.0))
                .unwrap_or_else(|| format!("Process {pid}"));
            for index in 0..unsafe { CFArrayGetCount(list.0) } {
                let element = unsafe { CFArrayGetValueAtIndex(list.0, index) };
                unsafe {
                    AXUIElementSetMessagingTimeout(element, 0.5);
                }
                if pid == own_pid && !attribute(element, "AXTitle").ok().and_then(|v| text(v.0))
                    .is_some_and(|title| super::own_terminal_title(&title)) { continue; }
                let known_id = self
                    .windows
                    .iter()
                    .find(|(_, w)| w.pid == pid && unsafe { CFEqual(w.element.0, element) != 0 })
                    .map(|(id, _)| id.clone());
                if known_id.is_none() {
                    let subrole = attribute(element, "AXSubrole").ok().and_then(|v| text(v.0));
                    if subrole.as_deref() != Some("AXStandardWindow")
                        || !settable(element, "AXPosition")
                        || !settable(element, "AXSize")
                    {
                        continue;
                    }
                }
                // Native fullscreen is outside this backend's ownership.
                if boolean(element, "AXFullScreen").unwrap_or(false) {
                    continue;
                }
                let frame = match geometry(element) {
                    Ok(frame) => frame,
                    Err(code) => {
                        if let Some(id) = &known_id {
                            return Err(ax_error(code, "Reading managed geometry", Some(id)));
                        }
                        continue;
                    }
                };
                let Some(rect) = geometry::physical(frame, scale) else {
                    continue;
                };
                let minimized = match boolean(element, "AXMinimized") {
                    Ok(value) => value,
                    Err(code) => {
                        if let Some(id) = &known_id {
                            return Err(ax_error(code, "Reading minimized state", Some(id)));
                        }
                        continue;
                    }
                };
                let id = if let Some(id) = known_id {
                    id
                } else {
                    let id = format!("mac-window-{}", self.next_id);
                    self.next_id += 1;
                    self.windows.insert(
                        id.clone(),
                        Window {
                            element: unsafe { Owned::retained(element) },
                            app: unsafe { Owned::retained(app.0) },
                            pid,
                            original: None,
                            minimized_by_manager: false,
                        },
                    );
                    id
                };
                seen.insert(id.clone());
                let window = self.windows.get_mut(&id).expect("inserted AX window");
                if !minimized {
                    window.minimized_by_manager = false;
                }
                let monitor = self
                    .monitors
                    .iter()
                    .max_by_key(|m| geometry::overlap(rect, m.bounds))
                    .expect("validated displays");
                result.windows.push(NativeWindow {
                    id,
                    title: attribute(element, "AXTitle")
                        .ok()
                        .and_then(|v| text(v.0))
                        .unwrap_or_default(),
                    app_name: app_name.clone(),
                    process_id: pid as u32,
                    monitor_id: monitor.id.clone(),
                    rect,
                    minimized,
                    minimized_by_manager: window.minimized_by_manager,
                    resizable: settable(element, "AXSize"),
                });
            }
        }
        // Absence from AXWindows (Spaces, minimization, app stalls) alone is not closure.
        let missing: Vec<_> = self
            .windows
            .keys()
            .filter(|id| !seen.contains(*id))
            .cloned()
            .collect();
        for id in missing {
            let window = &self.windows[&id];
            match geometry(window.element.0) {
                Err(-25202) => {
                    self.windows.remove(&id);
                }
                Err(code) if window.original.is_some() => {
                    return Err(ax_error(code, "Checking retained window", Some(&id)));
                }
                Ok(frame) if window.original.is_some() => {
                    let rect = geometry::physical(frame, scale).ok_or_else(|| {
                        error(
                            ErrorCode::OperationDenied,
                            "Invalid retained window geometry",
                            Some(&id),
                        )
                    })?;
                    let minimized = boolean(window.element.0, "AXMinimized")
                        .map_err(|c| ax_error(c, "Reading retained minimized state", Some(&id)))?;
                    let monitor = self
                        .monitors
                        .iter()
                        .max_by_key(|m| geometry::overlap(rect, m.bounds))
                        .expect("validated displays");
                    result.windows.push(NativeWindow {
                        id: id.clone(),
                        title: attribute(window.element.0, "AXTitle")
                            .ok()
                            .and_then(|v| text(v.0))
                            .unwrap_or_default(),
                        app_name: attribute(window.app.0, "AXTitle")
                            .ok()
                            .and_then(|v| text(v.0))
                            .unwrap_or_else(|| format!("Process {}", window.pid)),
                        process_id: window.pid as u32,
                        monitor_id: monitor.id.clone(),
                        rect,
                        minimized,
                        minimized_by_manager: minimized && window.minimized_by_manager,
                        resizable: settable(window.element.0, "AXSize"),
                    });
                }
                _ => {}
            }
        }
        if let Some(system) = unsafe { Owned::from_create(AXUIElementCreateSystemWide()) } {
            if let Ok(app) = attribute(system.0, "AXFocusedApplication") {
                if let Ok(focused) = attribute(app.0, "AXFocusedWindow") {
                    result.focused_window = result
                        .windows
                        .iter()
                        .find(|w| unsafe { CFEqual(self.windows[&w.id].element.0, focused.0) != 0 })
                        .map(|w| w.id.clone());
                }
            }
        }
        Ok(result)
    }
    pub fn apply(&mut self, actions: &[NativeAction]) -> Result<(), AppError> {
        self.permission()?;
        // Recheck topology before every batch; never use stale uniform-scale assumptions.
        self.refresh_screens();
        if let Some(error) = &self.screen_error {
            return Err(error.clone());
        }
        for action in actions {
            match action {
                NativeAction::Restore { window_id } => self.restore_one(window_id)?,
                NativeAction::Placement {
                    window_id,
                    rect,
                    clip,
                    minimized,
                } => {
                    if clip.is_some() {
                        return Err(error(
                            ErrorCode::NotImplemented,
                            "macOS AX cannot clip arbitrary windows",
                            Some(window_id),
                        ));
                    }
                    if rect.width == 0 || rect.height == 0 {
                        return Err(error(
                            ErrorCode::InvalidCommand,
                            "Window dimensions must be nonzero",
                            Some(window_id),
                        ));
                    }
                    let scale = self.monitors[0].scale_factor;
                    let window = self.window(window_id)?;
                    remember(window).map_err(|c| {
                        ax_error(c, "Saving original window state", Some(window_id))
                    })?;
                    if *minimized {
                        set_boolean(window.element.0, "AXMinimized", true)
                            .map_err(|c| ax_error(c, "Minimizing", Some(window_id)))?;
                        window.minimized_by_manager = !window.original.expect("saved").minimized;
                    } else {
                        set_boolean(window.element.0, "AXMinimized", false).map_err(|c| {
                            ax_error(c, "Restoring from minimized", Some(window_id))
                        })?;
                        window.minimized_by_manager = false;
                        set_geometry(window.element.0, geometry::logical(*rect, scale))
                            .map_err(|c| ax_error(c, "Setting window geometry", Some(window_id)))?;
                        let actual = geometry(window.element.0).map_err(|c| {
                            ax_error(c, "Checking window geometry", Some(window_id))
                        })?;
                        if geometry::physical(actual, scale) != Some(*rect) {
                            return Err(error(
                                ErrorCode::OperationDenied,
                                "Application constrained or deferred the requested geometry; refresh actual state",
                                Some(window_id),
                            ));
                        }
                    }
                }
                NativeAction::Focus { window_id } => {
                    let window = self.window(window_id)?;
                    remember(window).map_err(|c| {
                        ax_error(c, "Saving original window state", Some(window_id))
                    })?;
                    set_boolean(window.element.0, "AXMinimized", false)
                        .map_err(|c| ax_error(c, "Unminimizing focused window", Some(window_id)))?;
                    window.minimized_by_manager = false;
                    set_boolean(window.app.0, "AXFrontmost", true)
                        .map_err(|c| ax_error(c, "Activating application", Some(window_id)))?;
                    set_boolean(window.element.0, "AXMain", true)
                        .map_err(|c| ax_error(c, "Making window main", Some(window_id)))?;
                    ffi::action(window.element.0, "AXRaise")
                        .map_err(|c| ax_error(c, "Raising window", Some(window_id)))?;
                }
                NativeAction::Close { window_id } => {
                    let window = self.window(window_id)?;
                    let button = attribute(window.element.0, "AXCloseButton")
                        .map_err(|c| ax_error(c, "Finding close button", Some(window_id)))?;
                    ffi::action(button.0, "AXPress")
                        .map_err(|c| ax_error(c, "Pressing close button", Some(window_id)))?;
                    // A save dialog can cancel close: retain state until actual AX invalidation.
                }
            }
        }
        Ok(())
    }
    fn window(&mut self, id: &str) -> Result<&mut Window, AppError> {
        let window = self.windows.get_mut(id).ok_or_else(|| {
            error(
                ErrorCode::WindowGone,
                "Unknown AX session window ID; refresh",
                Some(id),
            )
        })?;
        let mut pid = 0;
        ffi::result(unsafe { AXUIElementGetPid(window.element.0, &mut pid) })
            .map_err(|c| ax_error(c, "Validating AX window", Some(id)))?;
        if pid != window.pid {
            return Err(error(
                ErrorCode::WindowGone,
                "AX window process changed",
                Some(id),
            ));
        }
        if boolean(window.element.0, "AXFullScreen").unwrap_or(false) {
            return Err(error(
                ErrorCode::OperationDenied,
                "Exit native macOS fullscreen before managing this window",
                Some(id),
            ));
        }
        Ok(window)
    }
    fn restore_one(&mut self, id: &str) -> Result<(), AppError> {
        let Some(window) = self.windows.get_mut(id) else {
            return Err(error(
                ErrorCode::WindowGone,
                "Unknown AX window ID",
                Some(id),
            ));
        };
        let Some(original) = window.original else {
            return Ok(());
        };
        if matches!(geometry(window.element.0), Err(-25202)) {
            self.windows.remove(id);
            return Ok(());
        }
        if boolean(window.element.0, "AXFullScreen").unwrap_or(false) {
            return Err(error(
                ErrorCode::OperationDenied,
                "Exit native fullscreen before restoring managed window",
                Some(id),
            ));
        }
        // Keep the original on any failure so a subsequent restore can retry.
        let unminimize = set_boolean(window.element.0, "AXMinimized", false);
        let placement = set_geometry(window.element.0, original.frame);
        let minimized = set_boolean(window.element.0, "AXMinimized", original.minimized);
        unminimize
            .and(placement)
            .and(minimized)
            .map_err(|c| ax_error(c, "Restoring original window state", Some(id)))?;
        let actual = geometry(window.element.0)
            .map_err(|c| ax_error(c, "Verifying restored geometry", Some(id)))?;
        if actual != original.frame
            || boolean(window.element.0, "AXMinimized")
                .map_err(|c| ax_error(c, "Verifying restored minimization", Some(id)))?
                != original.minimized
        {
            return Err(error(
                ErrorCode::OperationDenied,
                "Application constrained or deferred restoration; original state retained for retry",
                Some(id),
            ));
        }
        window.original = None;
        window.minimized_by_manager = false;
        Ok(())
    }
    pub fn restore(&mut self) -> Result<(), AppError> {
        // Native logical originals remain meaningful even after a DPI change.
        let ids: Vec<_> = self
            .windows
            .iter()
            .filter(|(_, w)| w.original.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        if ids.is_empty() {
            return Ok(());
        }
        self.permission()?;
        let mut errors = Vec::new();
        for id in ids {
            if let Err(error) = self.restore_one(&id) {
                errors.push(error);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            let first = &errors[0];
            Err(error(
                first.code,
                errors
                    .iter()
                    .map(|e| {
                        format!(
                            "{}: {}",
                            e.window_id.as_deref().unwrap_or("window"),
                            e.message
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; "),
                first.window_id.as_deref(),
            ))
        }
    }
}
fn remember(window: &mut Window) -> Result<(), i32> {
    if window.original.is_none() {
        window.original = Some(Original {
            frame: geometry(window.element.0)?,
            minimized: boolean(window.element.0, "AXMinimized")?,
        });
    }
    Ok(())
}
fn process_ids() -> Result<Vec<i32>, AppError> {
    let count = unsafe { proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return Err(error(
            ErrorCode::BackendUnavailable,
            "proc_listallpids failed",
            None,
        ));
    }
    let mut pids = vec![0i32; count as usize + 1024];
    let actual = unsafe {
        proc_listallpids(
            pids.as_mut_ptr().cast(),
            (pids.len() * std::mem::size_of::<i32>()) as i32,
        )
    };
    if actual <= 0 || actual as usize >= pids.len() {
        return Err(error(
            ErrorCode::BackendUnavailable,
            "Process enumeration failed or changed too quickly; refresh",
            None,
        ));
    }
    pids.truncate(actual as usize);
    Ok(pids)
}
