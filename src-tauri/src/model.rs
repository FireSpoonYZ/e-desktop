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
    /// lane: tabbed. Absent in older snapshots means normal.
    #[serde(default)]
    pub display: ColumnDisplay,
    /// lane: tabbed. The one window a tabbed column shows; the engine keeps it in sync with
    /// the column's focus memory. None for normal columns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_tab: Option<WindowId>,
}

/// lane: tabbed. niri column display: stacked rows, or tabs showing one window at a time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ColumnDisplay {
    #[default]
    Normal,
    Tabbed,
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
    /// Base layout animation length (`animationDurationMs`); 0 disables animations.
    pub animation_duration_ms: u32,
    /// Monitors paused while a foreign window covers the full display.
    /// Not layout fullscreen, not a maximized work-area window, and not a
    /// rectangle this manager itself just placed. Empty when nothing is covered.
    #[serde(default)]
    pub suspended_monitors: Vec<MonitorId>,
    // lane: layout-options
    /// Pages carrying a user or configured name in `Page::name`; they persist while empty.
    #[serde(default)]
    pub named_pages: Vec<PageId>,
    /// lane: ui-animation — overview open/close length after per-kind overrides; 0 disables.
    #[serde(default)]
    pub overview_animation_ms: u32,
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
    /// Slide the active monitor's view left/right until the next column there is fully on
    /// screen and focus it; at the end of the strip, focus the neighbouring column instead.
    SlideColumn {
        direction: Direction,
    },
    ToggleFloating,
    ToggleFullscreen,
    CloseWindow {
        window_id: WindowId,
    },
    /// Pointer drop at a physical screen point: a tiled window joins the column under it,
    /// or becomes a new column when dropped on a column's outer quarter or empty space. Near
    /// the left/right screen edge it becomes a column scrolled fully on screen on that side.
    DropWindow {
        window_id: WindowId,
        x: i32,
        y: i32,
        /// Explicit overview target; absent uses the page active under the screen point.
        /// A background target does not activate that page.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        page_id: Option<PageId>,
        /// Overview hit-test scroll, only with page_id; bypasses desktop edge/top drop bands.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        viewport_x: Option<i32>,
    },
    /// Drag the boundary left of column `edge` on the monitor's active page by `delta`
    /// physical pixels (`edge` = column count is the right end). Neighbours give or take the
    /// width, cut columns slide, and the edge snaps onto screen edges without auto-filling.
    DragEdge {
        monitor_id: MonitorId,
        edge: u32,
        delta: i32,
    },
    /// Drag the boundary above row `edge` of column `column` on the monitor's active page by
    /// `delta` physical pixels. The two rows trade height and the boundary snaps to the middle.
    /// Dropped on the top/bottom edge, the rows it passed leave the stack and queue as one
    /// column just off screen, on the side of the screen half the column is in.
    DragRow {
        monitor_id: MonitorId,
        column: u32,
        edge: u32,
        delta: i32,
    },
    /// Physical rectangle for a floating window (pointer move/resize).
    SetFloatingRect {
        window_id: WindowId,
        rect: Rect,
    },
    // lane: layout-actions
    /// niri consume-or-expel-window-left/right: a stacked window leaves for a new column on
    /// that side; a window alone in its column joins the bottom of the neighbouring column.
    ConsumeOrExpelWindow {
        direction: Direction,
    },
    /// The first window of the column to the right joins the bottom of the focused column.
    ConsumeWindowIntoColumn,
    /// The focused window leaves its stack for a new column to its right.
    ExpelWindowFromColumn,
    /// Swap the focused column with its left/right neighbour.
    MoveColumn {
        direction: Direction,
    },
    MoveColumnToFirst,
    MoveColumnToLast,
    /// Swap the focused window with the remembered window of the left/right column.
    SwapWindow {
        direction: Direction,
    },
    FocusColumnFirst,
    FocusColumnLast,
    /// Focus up/down in the column; past its top/bottom, switch to the previous/next page.
    FocusWindowOrPage {
        direction: Direction,
    },
    /// Focus left/right; past the end of the strip, focus the monitor on that side.
    FocusColumnOrMonitor {
        direction: Direction,
    },
    /// Activate the nearest monitor that way and focus its active page's remembered window.
    FocusMonitor {
        direction: Direction,
    },
    /// Move the focused column (a floating window alone) to that monitor's active page.
    MoveColumnToMonitor {
        direction: Direction,
    },
    MoveWindowToMonitor {
        direction: Direction,
    },
    /// Move the active monitor's active page to the monitor on that side (niri
    /// move-workspace-to-monitor).
    MovePageToMonitor {
        direction: Direction,
    },
    /// Focus the previously focused managed window, across pages and monitors.
    FocusWindowPrevious,
    /// Return the active monitor to the page it showed before.
    FocusPagePrevious,
    // lane: layout-options
    /// Cycle the focused column backwards through `presetColumnWidths`.
    CycleWidthBack,
    /// Set the focused window's height to the next `presetWindowHeights` entry; the rest of
    /// its column shares the remaining height.
    CycleWindowHeight,
    /// Toggle the focused column between the full viewport width and its previous width.
    MaximizeColumn,
    /// Name the active page of the active monitor; the name moves off any other page.
    SetPageName {
        name: String,
    },
    UnsetPageName,
    /// lane: tabbed. Switch the focused window's column between normal and tabbed display.
    ToggleColumnTabbedDisplay,
    // lane: input-gestures
    /// Touchpad swipe step: move the monitor's active page view by `delta` physical pixels,
    /// without snapping to columns or changing focus.
    DragViewport {
        monitor_id: MonitorId,
        delta: i32,
    },
    /// Touchpad swipe release: settle the view on the column edge nearest its position plus
    /// `delta` (the projected fling) and focus that column.
    SnapViewport {
        monitor_id: MonitorId,
        delta: i32,
    },
    // lane: rules-spawn-screenshot
    /// Controller-only: freeze the screen and pick a region; PNG file plus clipboard.
    Screenshot,
    /// Controller-only: the active monitor.
    ScreenshotScreen,
    /// Controller-only: the foreground window (the layout focus behind our own surfaces).
    ScreenshotWindow,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
