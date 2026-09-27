use serde::{Deserialize, Serialize};

pub type MonitorId = String;
pub type PageId = String;
pub type ColumnId = String;
pub type WindowId = String;

/// Physical screen pixels, including negative origins on secondary monitors.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Monitor {
    pub id: MonitorId,
    pub name: String,
    pub bounds: Rect,
    pub work_area: Rect,
    pub scale_factor: f64,
    pub primary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWindow {
    pub id: WindowId,
    pub title: String,
    pub app_name: String,
    pub process_id: u32,
    pub monitor_id: MonitorId,
    pub rect: Rect,
    pub minimized: bool,
    /// Must remain enumerable even when the manager minimized this window.
    pub minimized_by_manager: bool,
    pub resizable: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemSnapshot {
    pub monitors: Vec<Monitor>,
    pub windows: Vec<NativeWindow>,
    pub focused_window: Option<WindowId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowState {
    pub native: NativeWindow,
    pub floating: bool,
    /// Layout fullscreen fills the monitor work area; not OS-native fullscreen.
    pub fullscreen: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    pub id: ColumnId,
    pub width: u32,
    /// Top-to-bottom order within this column.
    pub windows: Vec<WindowId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub id: PageId,
    pub name: String,
    /// Left-to-right order; pages are not OS virtual desktops.
    pub columns: Vec<Column>,
    pub floating_windows: Vec<WindowId>,
    pub viewport_x: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorState {
    pub monitor: Monitor,
    pub pages: Vec<Page>,
    pub active_page: PageId,
    /// Usable physical area after the top bar and the gap inset.
    pub viewport: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BackendKind {
    Windows,
    X11,
    MacOs,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BackendAvailability {
    Ready,
    NotImplemented,
    PermissionRequired,
    UnsupportedSession,
    Unavailable,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub enumerate: bool,
    pub placement: bool,
    pub focus: bool,
    pub close: bool,
    pub minimize: bool,
    pub clipping: bool,
    pub global_shortcuts: bool,
    pub focus_follows_pointer: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendStatus {
    pub kind: BackendKind,
    pub availability: BackendAvailability,
    pub capabilities: Capabilities,
    pub message: String,
}

impl Default for BackendStatus {
    fn default() -> Self {
        Self {
            kind: BackendKind::Unsupported,
            availability: BackendAvailability::NotImplemented,
            capabilities: Capabilities::default(),
            message: "Native backend is not implemented in this baseline.".into(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub enabled: bool,
    pub backend: BackendStatus,
    pub monitors: Vec<MonitorState>,
    pub windows: Vec<WindowState>,
    pub focused_window: Option<WindowId>,
    pub active_monitor: Option<MonitorId>,
    pub errors: Vec<AppError>,
    /// Monitors whose control bar stays visible and reserves space; others auto-hide.
    pub pinned_bars: Vec<MonitorId>,
    /// False when the platform cannot reveal bars from the pointer (all bars stay pinned).
    pub bars_autohide: bool,
    /// Logical pixels between tiled windows and the screen edges (niri gaps).
    pub gaps: u32,
    /// Layout and overview animation length; 0 disables animations.
    pub animation_duration_ms: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// IPC: { "type": "moveWindowToPage", "windowId": "...", "pageId": "..." }.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Command {
    Refresh,
    Enable,
    Disable,
    FocusWindow {
        window_id: WindowId,
    },
    FocusDirection {
        direction: Direction,
    },
    SwitchPage {
        monitor_id: MonitorId,
        page_id: PageId,
    },
    AddPage {
        monitor_id: MonitorId,
    },
    MoveWindowToPage {
        window_id: WindowId,
        page_id: PageId,
    },
    MoveWindow {
        direction: Direction,
    },
    CycleWidth,
    /// Column dimensions and deltas are physical pixels, not logical/DPI-scaled units.
    SetColumnWidth {
        width: u32,
    },
    SetWindowColumnWidth {
        window_id: WindowId,
        width: u32,
    },
    AdjustColumnWidth {
        delta: i32,
    },
    AdjustWindowHeight {
        delta: i32,
    },
    ResetWindowHeights,
    CenterFocused,
    Scroll {
        monitor_id: MonitorId,
        delta: i32,
    },
    ToggleFloating,
    ToggleFullscreen,
    CloseWindow {
        window_id: WindowId,
    },
    /// Pointer drop at a physical screen point: a tiled window joins the column under it,
    /// or becomes a new column when dropped on a column's outer quarter or empty space.
    DropWindow {
        window_id: WindowId,
        x: i32,
        y: i32,
    },
    /// Drag the boundary left of column `edge` on the monitor's active page by `delta`
    /// physical pixels (`edge` = column count is the right end). Neighbours give or take the
    /// width, cut columns slide, the edge snaps onto screen edges; the page stays filled.
    DragEdge {
        monitor_id: MonitorId,
        edge: u32,
        delta: i32,
    },
    /// Physical rectangle for a floating window (pointer move/resize).
    SetFloatingRect {
        window_id: WindowId,
        rect: Rect,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
    NotImplemented,
    UnsupportedSession,
    BackendUnavailable,
    PermissionRequired,
    WindowGone,
    OperationDenied,
    InvalidCommand,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
    pub window_id: Option<WindowId>,
}

impl AppError {
    pub fn not_implemented(component: &str) -> Self {
        Self {
            code: ErrorCode::NotImplemented,
            message: format!("{component} is not implemented in this baseline."),
            window_id: None,
        }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AppError {}

/// Backend retains original placement/region/state before the first mutation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum NativeAction {
    /// clip is in screen pixels; None removes only manager-owned clipping.
    /// Fully offscreen windows are minimized, never made irretrievably hidden.
    Placement {
        window_id: WindowId,
        rect: Rect,
        clip: Option<Rect>,
        minimized: bool,
    },
    Focus {
        window_id: WindowId,
    },
    /// Graceful window close only; never terminate a process.
    Close {
        window_id: WindowId,
    },
    Restore {
        window_id: WindowId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transition {
    pub snapshot: Snapshot,
    pub actions: Vec<NativeAction>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_contract_and_safe_baseline() {
        let command: Command =
            serde_json::from_str(r#"{"type":"moveWindowToPage","windowId":"w1","pageId":"p2"}"#)
                .unwrap();
        assert_eq!(
            command,
            Command::MoveWindowToPage {
                window_id: "w1".into(),
                page_id: "p2".into()
            }
        );
        assert_eq!(serde_json::to_value(command).unwrap()["windowId"], "w1");
        assert!(serde_json::from_str::<Command>(r#"{"type":"invented"}"#).is_err());
        let snapshot = serde_json::to_value(Snapshot::default()).unwrap();
        assert_eq!(snapshot["enabled"], false);
        assert_eq!(snapshot["backend"]["availability"], "notImplemented");
        assert_eq!(snapshot["focusedWindow"], serde_json::Value::Null);
        assert_eq!(snapshot["backend"]["capabilities"]["placement"], false);
        assert_eq!(snapshot["gaps"], 0);
    }
}
