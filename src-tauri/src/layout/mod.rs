use std::collections::{BTreeMap, BTreeSet};

use crate::model::*;
use crate::rules::WindowRule;

pub mod edges;
mod monitors;
mod pointer;
pub use pointer::top_band;
mod rules;
pub mod scene;

mod sizing;

/// Monitor/page indices plus optional column/row (None for floating windows).
type WindowLocation = (usize, usize, Option<(usize, usize)>);

/// Tiled window the user minimized. It is out of the active strip, but the slot is enough to
/// put it back in the same page, column and row when it returns, including out of order.
#[derive(Clone)]
struct MinimizedSlot {
    id: WindowId,
    page_id: PageId,
    column_id: ColumnId,
    width: u32,
    /// Same-column windows that were above this one, nearest first.
    above: Vec<WindowId>,
    /// Columns that were to the left, nearest first.
    left: Vec<ColumnId>,
}

/// Pure, transactional layout state. Native effects are returned to the caller, never applied here.
#[derive(Clone)]
pub struct Engine {
    snapshot: Snapshot,
    next_id: u64,
    viewports: BTreeMap<MonitorId, Rect>,
    page_focus: BTreeMap<PageId, WindowId>,
    /// Session-only focus history keyed by stable column and window identities.
    column_focus: BTreeMap<ColumnId, WindowId>,
    fullscreen_restore: BTreeMap<WindowId, Rect>,
    /// Relative shares of space above each tiled window's one-pixel minimum.
    height_weights: BTreeMap<WindowId, u32>,
    /// User-minimized tiled windows, kept out of columns until restored.
    minimized_slots: Vec<MinimizedSlot>,
    window_rules: Vec<WindowRule>,
    pending_rule_floating: BTreeSet<WindowId>,
    /// Floating only because they were not resizable when discovered (Chromium drops the
    /// resize frame while fullscreen); they tile once they become resizable.
    size_floating: BTreeSet<WindowId>,
    /// Minimum widths windows enforce natively; their columns are never narrower (niri).
    min_widths: BTreeMap<WindowId, u32>,
    disconnected_monitors: BTreeMap<MonitorId, monitors::DisconnectedMonitor>,
    monitor_order: Vec<MonitorId>,
    /// Explicit moves into borrowed pages stay on their chosen host when the owner returns.
    hotplug_pinned: BTreeMap<WindowId, MonitorId>,
    outputs_suspended: bool,
    /// Managed windows detected covering some monitor, wherever their column lives.
    covering_windows: BTreeSet<WindowId>,
}

fn invalid(message: &str) -> AppError {
    AppError {
        code: ErrorCode::InvalidCommand,
        message: message.into(),
        window_id: None,
    }
}

fn empty(page: &Page) -> bool {
    page.columns.is_empty() && page.floating_windows.is_empty()
}

fn ids(page: &Page) -> impl Iterator<Item = &WindowId> {
    page.columns
        .iter()
        .flat_map(|column| column.windows.iter())
        .chain(page.floating_windows.iter())
}

fn coordinate(value: i64) -> i32 {
    value.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

/// One side of the logical gap, in physical pixels. The visual gap is twice this.
pub fn half_gap(gaps: u32, scale: f64) -> u32 {
    let px = f64::from(gaps) * scale / 2.0;
    if !px.is_finite() || px <= 0.0 {
        0
    } else {
        px.round() as u32
    }
}

/// Inset every side. Stays inside `rect`; a side smaller than `2 * half` keeps at least 1px.
pub fn inset_gap(rect: Rect, half: u32) -> Rect {
    // ponytail: clamp so the result stays inside `rect`. Full `half` when the monitor is larger than the gap.
    let hx = half.min(rect.width.saturating_sub(1) / 2);
    let hy = half.min(rect.height.saturating_sub(1) / 2);
    Rect {
        x: rect.x.saturating_add(hx as i32),
        y: rect.y.saturating_add(hy as i32),
        width: rect.width - hx * 2,
        height: rect.height - hy * 2,
    }
}

/// Grow every side by `half` (the viewport before the gap inset).
pub fn expand_gap(rect: Rect, half: u32) -> Rect {
    let half = i64::from(half);
    Rect {
        x: coordinate(i64::from(rect.x) - half),
        y: coordinate(i64::from(rect.y) - half),
        width: (i64::from(rect.width) + half * 2).clamp(0, i64::from(u32::MAX)) as u32,
        height: (i64::from(rect.height) + half * 2).clamp(0, i64::from(u32::MAX)) as u32,
    }
}

fn overlaps(a: Rect, b: Rect) -> bool {
    (a.x as i64) < b.x as i64 + b.width as i64
        && (b.x as i64) < a.x as i64 + a.width as i64
        && (a.y as i64) < b.y as i64 + b.height as i64
        && (b.y as i64) < a.y as i64 + a.height as i64
}

fn widths(page: &Page) -> Vec<u32> {
    page.columns.iter().map(|c| c.width).collect()
}

/// Default scroll: no gap before the first column or after the last.
fn clamp_scroll(page: &Page, viewport: u32, target: i64) -> i32 {
    coordinate(edges::clamp_x(&widths(page), viewport, target))
}

/// Also allows centering gaps and an off-screen queue.
fn clamp_scroll_relaxed(page: &Page, viewport: u32, target: i64) -> i32 {
    coordinate(edges::clamp_x_relaxed(&widths(page), viewport, target))
}

fn user_minimized(window: &WindowState) -> bool {
    window.native.minimized && !window.native.minimized_by_manager
}

/// Index just after the nearest predecessor still present; otherwise the start.
fn insert_after<T: PartialEq>(existing: &[T], predecessors: &[T]) -> usize {
    predecessors
        .iter()
        .find_map(|pred| existing.iter().position(|id| id == pred).map(|i| i + 1))
        .unwrap_or(0)
}

/// Present ids plus parked ones, using each parked id's nearest-first predecessors.
fn logical_ids(present: &[String], parked: &[(String, Vec<String>)]) -> Vec<String> {
    let mut order = present.to_vec();
    let mut pending: Vec<_> = parked
        .iter()
        .filter(|(id, _)| !order.iter().any(|have| have == id))
        .cloned()
        .collect();
    while !pending.is_empty() {
        let start = pending.len();
        let batch = std::mem::take(&mut pending);
        for (id, preds) in batch {
            if let Some(pred) = preds
                .iter()
                .find(|pred| order.iter().any(|have| have == *pred))
            {
                let at = order.iter().position(|have| have == pred).unwrap();
                order.insert(at + 1, id);
            } else if preds.iter().any(|pred| {
                parked.iter().any(|(other, _)| other == pred)
                    && !order.iter().any(|have| have == pred)
            }) {
                pending.push((id, preds));
            } else {
                order.insert(0, id);
            }
        }
        if pending.len() == start {
            for (id, _) in pending {
                order.push(id);
            }
            break;
        }
    }
    order
}

/// Snap distance for boundary drags: 1/12 of the viewport width, at least 64 logical pixels,
/// so an edge released anywhere near the screen edge clearly lands on it.
pub fn snap_distance(scale: f64, view: u32) -> i64 {
    ((64.0 * scale).round() as i64).max(i64::from(view) / 12)
}

impl Engine {
    pub fn new(backend: BackendStatus) -> Self {
        Self {
            snapshot: Snapshot {
                backend,
                ..Snapshot::default()
            },
            next_id: 0,
            viewports: BTreeMap::new(),
            page_focus: BTreeMap::new(),
            column_focus: BTreeMap::new(),
            fullscreen_restore: BTreeMap::new(),
            height_weights: BTreeMap::new(),
            minimized_slots: vec![],
            window_rules: vec![],
            pending_rule_floating: BTreeSet::new(),
            size_floating: BTreeSet::new(),
            min_widths: BTreeMap::new(),
            disconnected_monitors: BTreeMap::new(),
            monitor_order: vec![],
            hotplug_pinned: BTreeMap::new(),
            outputs_suspended: false,
            covering_windows: BTreeSet::new(),
        }
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// Replace the per-monitor full-display pause. Empty resumes every monitor.
    pub fn set_suspended_monitors(&mut self, ids: Vec<MonitorId>) {
        self.snapshot.suspended_monitors = ids;
    }

    /// Ids from the last enumerate, not from column membership.
    pub fn set_covering_windows(&mut self, ids: Vec<WindowId>) {
        self.covering_windows = ids.into_iter().collect();
    }

    pub(crate) fn window_on_suspended(&self, id: &str) -> bool {
        self.location(id)
            .ok()
            .is_some_and(|(m, _, _)| self.monitor_suspended(m))
    }

    /// Logical suspension or a managed window detected covering another display.
    pub(crate) fn window_protected(&self, id: &str) -> bool {
        self.covering_windows.contains(id) || self.window_on_suspended(id)
    }

    fn monitor_suspended(&self, index: usize) -> bool {
        let id = &self.snapshot.monitors[index].monitor.id;
        self.snapshot.suspended_monitors.iter().any(|s| s == id)
    }

    fn hits_suspended(&self, monitor_id: &str) -> bool {
        self.snapshot
            .suspended_monitors
            .iter()
            .any(|id| id == monitor_id)
    }

    /// Commands that would move a suspended monitor, or pull a window onto/off it.
    fn command_hits_suspended(&self, command: &Command) -> bool {
        let win = |id: &str| self.window_protected(id);
        let mon = |id: &str| self.hits_suspended(id);
        let point = |x: i32, y: i32| {
            let (x, y) = (i64::from(x), i64::from(y));
            self.snapshot.monitors.iter().any(|m| {
                let b = m.monitor.bounds;
                mon(&m.monitor.id)
                    && x >= i64::from(b.x)
                    && y >= i64::from(b.y)
                    && x < i64::from(b.x) + i64::from(b.width)
                    && y < i64::from(b.y) + i64::from(b.height)
            })
        };
        match command {
            Command::DragEdge { monitor_id, .. }
            | Command::DragRow { monitor_id, .. }
            | Command::Scroll { monitor_id, .. }
            | Command::SwitchPage { monitor_id, .. }
            | Command::AddPage { monitor_id } => mon(monitor_id),
            Command::SlideColumn { .. } => self.snapshot.active_monitor.as_deref().is_some_and(mon),
            Command::DropWindow {
                window_id, x, y, ..
            } => point(*x, *y) || win(window_id),
            Command::MoveWindowToPage { window_id, page_id } => {
                win(window_id)
                    || self.snapshot.monitors.iter().any(|m| {
                        mon(&m.monitor.id) && m.pages.iter().any(|p| &p.id == page_id)
                    })
            }
            Command::FocusWindow { window_id }
            | Command::SetWindowColumnWidth { window_id, .. }
            | Command::SetFloatingRect { window_id, .. }
            | Command::CloseWindow { window_id } => win(window_id),
            Command::FocusDirection { .. }
            | Command::MoveWindow { .. }
            | Command::CycleWidth
            | Command::SetColumnWidth { .. }
            | Command::AdjustColumnWidth { .. }
            | Command::AdjustWindowHeight { .. }
            | Command::ResetWindowHeights
            | Command::CenterFocused
            | Command::ToggleFloating
            | Command::ToggleFullscreen => {
                self.snapshot.focused_window.as_deref().is_some_and(win)
            }
            _ => false,
        }
    }

    /// Refresh native availability/capabilities without discarding the user's layout.
    pub fn set_backend(&mut self, backend: BackendStatus) {
        self.snapshot.backend = backend;
    }

    /// Integration supplies physical control-window reservations before the next reconciliation.
    /// Native monitor work areas remain unchanged; missing overrides use the native work area.
    pub fn set_viewports(&mut self, viewports: BTreeMap<MonitorId, Rect>) {
        self.viewports = viewports;
    }

    pub fn set_min_widths(&mut self, min_widths: BTreeMap<WindowId, u32>) {
        self.min_widths = min_widths;
    }

    pub fn set_gaps(&mut self, gaps: u32) {
        self.snapshot.gaps = gaps;
    }

    fn id(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}-{}", self.next_id)
    }

    fn page(&mut self) -> Page {
        let id = self.id("page");
        Page {
            name: "Desktop".into(),
            id,
            columns: vec![],
            floating_windows: vec![],
            viewport_x: 0,
        }
    }

    fn column(&mut self, window: WindowId, width: u32) -> Column {
        Column {
            id: self.id("column"),
            width: width.max(1),
            windows: vec![window],
        }
    }

    fn window_index(&self, id: &str) -> Result<usize, AppError> {
        self.snapshot
            .windows
            .iter()
            .position(|w| w.native.id == id)
            .ok_or_else(|| AppError {
                code: ErrorCode::WindowGone,
                message: format!("Unknown window: {id}"),
                window_id: Some(id.into()),
            })
    }

    fn monitor_index(&self, id: &str) -> Result<usize, AppError> {
        self.snapshot
            .monitors
            .iter()
            .position(|m| m.monitor.id == id)
            .ok_or_else(|| invalid("Unknown monitor"))
    }

    fn page_index(&self, id: &str) -> Result<(usize, usize), AppError> {
        self.snapshot
            .monitors
            .iter()
            .enumerate()
            .find_map(|(m, monitor)| {
                monitor
                    .pages
                    .iter()
                    .position(|page| page.id == id)
                    .map(|p| (m, p))
            })
            .ok_or_else(|| invalid("Unknown page"))
    }

    fn location(&self, id: &str) -> Result<WindowLocation, AppError> {
        self.window_index(id)?;
        for (m, monitor) in self.snapshot.monitors.iter().enumerate() {
            for (p, page) in monitor.pages.iter().enumerate() {
                for (c, column) in page.columns.iter().enumerate() {
                    if let Some(w) = column.windows.iter().position(|w| w == id) {
                        return Ok((m, p, Some((c, w))));
                    }
                }
                if page.floating_windows.iter().any(|w| w == id) {
                    return Ok((m, p, None));
                }
            }
        }
        Err(invalid("Window has no monitor/page"))
    }

    fn focused(&self) -> Result<String, AppError> {
        self.snapshot
            .focused_window
            .clone()
            .ok_or_else(|| invalid("No focused window"))
    }

    fn transition(&self, actions: Vec<NativeAction>) -> Transition {
        Transition {
            snapshot: self.snapshot.clone(),
            actions,
        }
    }

    pub fn reconcile(&mut self, system: SystemSnapshot) -> Result<Transition, AppError> {
        let mut next = self.clone();
        let native_focus = system.focused_window.clone();
        next.reconcile_inner(system)?;
        if next.outputs_suspended {
            *self = next;
            return Ok(self.transition(vec![]));
        }
        let mut actions = next.placements()?;
        if let Some(id) = next.snapshot.focused_window.as_ref().filter(|_| {
            next.snapshot.enabled
                && next.snapshot.backend.capabilities.focus
                && self.snapshot.focused_window != next.snapshot.focused_window
                && native_focus != next.snapshot.focused_window
        }) {
            if !next.window_protected(id) {
                actions.push(NativeAction::Focus {
                    window_id: id.clone(),
                });
            }
        }
        next.update_pending_rule_floating(&actions);
        *self = next;
        Ok(self.transition(actions))
    }

    fn reconcile_inner(&mut self, system: SystemSnapshot) -> Result<(), AppError> {
        let mut monitor_ids = BTreeSet::new();
        for monitor in &system.monitors {
            if monitor.id.is_empty()
                || !monitor_ids.insert(monitor.id.clone())
                || monitor.work_area.width == 0
                || monitor.work_area.height == 0
                || !monitor.scale_factor.is_finite()
                || monitor.scale_factor <= 0.0
            {
                return Err(invalid("Invalid or duplicate monitor"));
            }
            if let Some(viewport) = self.viewports.get(&monitor.id) {
                let area = monitor.work_area;
                if viewport.width == 0
                    || viewport.height == 0
                    || viewport.x < area.x
                    || viewport.y < area.y
                    || viewport.x as i64 + viewport.width as i64 > area.x as i64 + area.width as i64
                    || viewport.y as i64 + viewport.height as i64
                        > area.y as i64 + area.height as i64
                {
                    return Err(invalid(
                        "Viewport must be a nonempty subset of the monitor work area",
                    ));
                }
            }
        }
        let mut window_ids = BTreeSet::new();
        for window in &system.windows {
            if window.id.is_empty()
                || !window_ids.insert(window.id.clone())
                || !monitor_ids.contains(&window.monitor_id)
            {
                return Err(invalid("Invalid window ID or monitor assignment"));
            }
        }
        if system
            .focused_window
            .as_ref()
            .is_some_and(|id| !window_ids.contains(id))
        {
            return Err(invalid("Focused window is absent from enumeration"));
        }
        // Empty enumeration cannot distinguish closed windows from temporarily unavailable outputs.
        // Validate the SystemSnapshot contract first; keep identities until outputs return.
        if system.monitors.is_empty() {
            self.outputs_suspended = true;
            return Ok(());
        }
        self.outputs_suspended = false;
        // Closing the focused window hands focus to its layout neighbour (niri), not to
        // whatever window the OS activates next (often on another monitor).
        let successor = self
            .snapshot
            .focused_window
            .as_ref()
            .filter(|id| self.snapshot.enabled && !window_ids.contains(*id))
            .and_then(|id| self.neighbours(id).ok())
            .and_then(|candidates| candidates.into_iter().find(|id| window_ids.contains(id)));
        let floating_origins = self.floating_origins();
        let viewport_changed = system.monitors.len() != self.snapshot.monitors.len()
            || system.monitors.iter().any(|monitor| {
                let viewport = self
                    .viewports
                    .get(&monitor.id)
                    .copied()
                    .unwrap_or(monitor.work_area);
                self.snapshot
                    .monitors
                    .iter()
                    .find(|m| m.monitor.id == monitor.id)
                    .is_none_or(|m| m.viewport != viewport)
            });
        self.fullscreen_restore
            .retain(|id, _| window_ids.contains(id));
        self.reconcile_monitors(system.monitors, &window_ids);
        self.snapshot
            .windows
            .retain(|w| window_ids.contains(&w.native.id));
        // Restoring a borrowed active page can leave its host without a valid insertion target.
        self.cleanup();
        self.update_pending_rule_floating(&[]);
        let mut new_windows = BTreeSet::new();
        for mut native in system.windows {
            if let Some(existing) = self
                .snapshot
                .windows
                .iter_mut()
                .find(|w| w.native.id == native.id)
            {
                // Keep normal geometry for iconic windows and rule moves not yet placed visibly.
                // fullscreen_restore separately owns the pre-fullscreen rectangle.
                if existing.floating
                    && (native.minimized || self.pending_rule_floating.contains(&native.id))
                {
                    native.rect = existing.native.rect;
                }
                existing.native = native;
            } else {
                new_windows.insert(native.id.clone());
                let floating = !native.resizable;
                self.snapshot.windows.push(WindowState {
                    native,
                    floating,
                    fullscreen: false,
                });
            }
        }
        self.size_floating.retain(|id| window_ids.contains(id));
        for id in self.size_floating.clone() {
            let w = self.window_index(&id)?;
            if !self.snapshot.windows[w].native.resizable {
                continue;
            }
            self.size_floating.remove(&id);
            if let Ok((m, p, None)) = self.location(&id) {
                self.remove_window(&id);
                self.snapshot.windows[w].floating = false;
                self.insert_window(m, p, &id, None)?;
            }
        }
        // Preserve logical monitor/page membership: clipped native rectangles can straddle monitors.
        for window in self.snapshot.windows.clone() {
            if self.location(&window.native.id).is_ok() {
                continue;
            }
            // Parked user-minimized windows are restored by cleanup, not reinserted at the tail.
            if self
                .minimized_slots
                .iter()
                .any(|slot| slot.id == window.native.id)
            {
                continue;
            }
            if new_windows.contains(&window.native.id) {
                self.insert_new_window(&window.native.id)?;
                continue;
            }
            let m = self.monitor_index(&window.native.monitor_id)?;
            let p = self.snapshot.monitors[m]
                .pages
                .iter()
                .position(|p| p.id == self.snapshot.monitors[m].active_page)
                .unwrap();
            self.insert_window(m, p, &window.native.id, None)?;
        }
        self.rebase_floating(floating_origins);
        self.cleanup();
        self.repair_hotplug_focus();
        if viewport_changed {
            if let Some(id) = self
                .snapshot
                .focused_window
                .clone()
                .filter(|id| window_ids.contains(id) && !self.window_protected(id))
            {
                self.ensure_visible(&id, false)?;
            }
        }
        let previous = self.snapshot.focused_window.clone();
        if let Some(id) = successor.filter(|id| {
            self.location(id).is_ok() && !self.window_protected(id)
        }) {
            self.set_focus(&id, true)?;
        } else if let Some(id) = system.focused_window {
            // A user-minimized window has left the columns; native focus must not error or wake it.
            // Focus on a suspended monitor must not scroll that page back into the covering window.
            if let Ok((m, p, _)) = self.location(&id) {
                let native = &self.snapshot.windows[self.window_index(&id)?].native;
                if !self.window_protected(&id)
                    && (!self.snapshot.enabled || !native.minimized)
                    && self.snapshot.monitors[m].pages[p].id
                        == self.snapshot.monitors[m].active_page
                {
                    self.set_focus(&id, viewport_changed || previous.as_ref() != Some(&id))?;
                }
            }
        }
        if self
            .snapshot
            .focused_window
            .as_ref()
            .is_some_and(|id| !window_ids.contains(id))
        {
            self.snapshot.focused_window = None;
            self.focus_active_page();
        }
        if self
            .snapshot
            .active_monitor
            .as_ref()
            .is_none_or(|id| !monitor_ids.contains(id))
        {
            self.snapshot.active_monitor = self
                .snapshot
                .monitors
                .iter()
                .find(|m| m.monitor.primary)
                .or(self.snapshot.monitors.first())
                .map(|m| m.monitor.id.clone());
        }
        Ok(())
    }

    fn insert_window(
        &mut self,
        m: usize,
        p: usize,
        id: &str,
        width: Option<u32>,
    ) -> Result<(), AppError> {
        let w = self.window_index(id)?;
        if self.snapshot.windows[w].floating {
            self.snapshot.monitors[m].pages[p]
                .floating_windows
                .push(id.into());
        } else {
            let viewport_width = self.snapshot.monitors[m].viewport.width;
            let width = width
                .unwrap_or((viewport_width / 2).max(1))
                .min(viewport_width);
            let column = self.column(id.into(), width);
            self.snapshot.monitors[m].pages[p].columns.push(column);
        }
        Ok(())
    }

    fn remove_window(&mut self, id: &str) {
        for monitor in &mut self.snapshot.monitors {
            for page in &mut monitor.pages {
                for column in &mut page.columns {
                    column.windows.retain(|w| w != id);
                }
                page.floating_windows.retain(|w| w != id);
            }
        }
    }

    /// User-minimized tiled windows leave the active strip. Their slot keeps the page,
    /// column, row and width so a later restore (in any order) lands where they were.
    /// Manager-hidden windows stay in their columns.
    fn sync_minimized(&mut self) {
        let mut keep = Vec::new();
        let mut ready = Vec::new();
        for slot in std::mem::take(&mut self.minimized_slots) {
            match self
                .snapshot
                .windows
                .iter()
                .find(|w| w.native.id == slot.id)
            {
                Some(w) if w.floating => ready.push(slot),
                Some(w) if user_minimized(w) => keep.push(slot),
                Some(_) => ready.push(slot),
                None => {}
            }
        }
        self.minimized_slots = keep;
        for slot in ready {
            self.restore_slot(slot);
        }
        let focused = self.snapshot.focused_window.clone();
        let parked: Vec<WindowId> = self
            .snapshot
            .windows
            .iter()
            .filter(|w| {
                !w.floating
                    && user_minimized(w)
                    && self
                        .minimized_slots
                        .iter()
                        .all(|slot| slot.id != w.native.id)
                    && matches!(self.location(&w.native.id), Ok((_, _, Some(_))))
            })
            .map(|w| w.native.id.clone())
            .collect();
        for id in &parked {
            self.park_window(id);
        }
        for monitor in &mut self.snapshot.monitors {
            for page in &mut monitor.pages {
                page.columns.retain(|c| !c.windows.is_empty());
            }
        }
        if focused.as_ref().is_some_and(|id| {
            self.snapshot
                .windows
                .iter()
                .any(|w| w.native.id == *id && user_minimized(w))
        }) {
            self.snapshot.focused_window = None;
            self.focus_active_page();
        }
    }

    fn park_window(&mut self, id: &str) {
        let Ok((m, p, Some((c, row)))) = self.location(id) else {
            return;
        };
        let page_id = self.snapshot.monitors[m].pages[p].id.clone();
        let column_id = self.snapshot.monitors[m].pages[p].columns[c].id.clone();
        let width = self.snapshot.monitors[m].pages[p].columns[c].width;
        let rows = logical_ids(
            &self.snapshot.monitors[m].pages[p].columns[c].windows,
            &self
                .minimized_slots
                .iter()
                .filter(|slot| slot.page_id == page_id && slot.column_id == column_id)
                .map(|slot| (slot.id.clone(), slot.above.clone()))
                .collect::<Vec<_>>(),
        );
        let columns = logical_ids(
            &self.snapshot.monitors[m].pages[p]
                .columns
                .iter()
                .map(|column| column.id.clone())
                .collect::<Vec<_>>(),
            &self.column_preds(&page_id),
        );
        let above = rows[..rows.iter().position(|window| window == id).unwrap_or(row)]
            .iter()
            .rev()
            .cloned()
            .collect();
        let left = columns[..columns
            .iter()
            .position(|column| column == &column_id)
            .unwrap_or(c)]
            .iter()
            .rev()
            .cloned()
            .collect();
        self.minimized_slots.push(MinimizedSlot {
            id: id.into(),
            page_id,
            column_id,
            width,
            above,
            left,
        });
        // Use the same detach geometry as a drag: removing a column left of the viewport
        // must not move the remaining visible windows.
        let _ = self.detach(id);
    }

    fn column_preds(&self, page_id: &str) -> Vec<(ColumnId, Vec<ColumnId>)> {
        let mut seen = BTreeSet::new();
        self.minimized_slots
            .iter()
            .filter(|slot| slot.page_id == page_id && seen.insert(slot.column_id.clone()))
            .map(|slot| (slot.column_id.clone(), slot.left.clone()))
            .collect()
    }

    fn restore_slot(&mut self, slot: MinimizedSlot) {
        if self.location(&slot.id).is_ok() {
            return;
        }
        let Some((m, p)) = self.page_pos(&slot.page_id) else {
            let Ok(m) = self
                .window_index(&slot.id)
                .and_then(|w| self.monitor_index(&self.snapshot.windows[w].native.monitor_id))
            else {
                return;
            };
            let p = self.snapshot.monitors[m]
                .pages
                .iter()
                .position(|page| page.id == self.snapshot.monitors[m].active_page)
                .unwrap();
            let _ = self.insert_window(m, p, &slot.id, Some(slot.width));
            return;
        };
        if self.snapshot.windows[self.window_index(&slot.id).unwrap()].floating {
            let floating = &mut self.snapshot.monitors[m].pages[p].floating_windows;
            if !floating.iter().any(|id| id == &slot.id) {
                floating.push(slot.id);
            }
            return;
        }
        let column_ids: Vec<ColumnId> = self.snapshot.monitors[m].pages[p]
            .columns
            .iter()
            .map(|column| column.id.clone())
            .collect();
        let existing = column_ids.iter().position(|id| id == &slot.column_id);
        let index = insert_after(&column_ids, &slot.left);
        let page = &mut self.snapshot.monitors[m].pages[p];
        if let Some(c) = existing {
            let row = insert_after(&page.columns[c].windows, &slot.above);
            page.columns[c].windows.insert(row, slot.id);
            return;
        }
        let left: i64 = page.columns[..index]
            .iter()
            .map(|c| i64::from(c.width))
            .sum();
        if !page.columns.is_empty() && left <= i64::from(page.viewport_x) {
            page.viewport_x = coordinate(i64::from(page.viewport_x) + i64::from(slot.width));
        }
        page.columns.insert(
            index,
            Column {
                id: slot.column_id,
                width: slot.width.max(1),
                windows: vec![slot.id],
            },
        );
    }

    fn page_pos(&self, id: &str) -> Option<(usize, usize)> {
        self.snapshot
            .monitors
            .iter()
            .enumerate()
            .find_map(|(m, monitor)| {
                monitor
                    .pages
                    .iter()
                    .position(|page| page.id == id)
                    .map(|p| (m, p))
            })
    }

    fn navigable(&self, id: &str) -> bool {
        self.snapshot
            .windows
            .iter()
            .any(|w| w.native.id == id && (!w.native.minimized || w.native.minimized_by_manager))
    }

    fn cleanup(&mut self) {
        self.sync_minimized();
        let parked: BTreeSet<PageId> = self
            .minimized_slots
            .iter()
            .map(|slot| slot.page_id.clone())
            .collect();
        let gaps = self.snapshot.gaps;
        for m in 0..self.snapshot.monitors.len() {
            let monitor = &mut self.snapshot.monitors[m];
            let gap = 2 * half_gap(gaps, monitor.monitor.scale_factor);
            for page in &mut monitor.pages {
                for column in &mut page.columns {
                    let min = column
                        .windows
                        .iter()
                        .filter_map(|id| self.min_widths.get(id))
                        .max()
                        .map_or(0, |w| w + gap);
                    column.width = column.width.max(min).min(monitor.viewport.width).max(1);
                }
                page.columns.retain(|c| !c.windows.is_empty());
            }
            // A page whose tiled windows are all user-minimized is not a disposable empty tail.
            let disposable = |page: &Page| empty(page) && !parked.contains(&page.id);
            let last = monitor.pages.last().map(|p| p.id.clone());
            monitor.pages.retain(|p| {
                !disposable(p) || p.id == monitor.active_page || Some(&p.id) == last.as_ref()
            });
            if monitor.pages.len() > 1
                && disposable(monitor.pages.last().unwrap())
                && disposable(&monitor.pages[monitor.pages.len() - 2])
            {
                monitor.pages.pop();
            }
            if monitor.pages.last().is_none_or(|p| !disposable(p)) {
                let page = self.page();
                self.snapshot.monitors[m].pages.push(page);
            }
            let monitor = &mut self.snapshot.monitors[m];
            if !monitor.pages.iter().any(|p| p.id == monitor.active_page) {
                monitor.active_page = monitor.pages[0].id.clone();
            }
            for (i, page) in monitor.pages.iter_mut().enumerate() {
                page.name = format!("Desktop {}", i + 1);
                page.viewport_x =
                    clamp_scroll_relaxed(page, monitor.viewport.width, page.viewport_x as i64);
            }
        }
        let valid: BTreeMap<_, BTreeSet<_>> = self
            .snapshot
            .monitors
            .iter()
            .flat_map(|m| m.pages.iter())
            .map(|p| (p.id.clone(), ids(p).cloned().collect()))
            .collect();
        self.page_focus
            .retain(|page, window| valid.get(page).is_some_and(|ids| ids.contains(window)));
        self.height_weights
            .retain(|id, _| self.snapshot.windows.iter().any(|w| &w.native.id == id));
        let snapshot = &self.snapshot;
        self.column_focus.retain(|column, window| {
            snapshot
                .monitors
                .iter()
                .flat_map(|m| &m.pages)
                .flat_map(|p| &p.columns)
                .any(|c| &c.id == column && c.windows.contains(window))
                && snapshot.windows.iter().any(|w| {
                    &w.native.id == window && (!w.native.minimized || w.native.minimized_by_manager)
                })
        });
        if let Some(id) = self.snapshot.focused_window.clone() {
            self.remember_column_focus(&id);
        }
    }

    fn remember_column_focus(&mut self, id: &str) {
        if self.navigable(id) {
            if let Ok((m, p, Some((c, _)))) = self.location(id) {
                self.column_focus.insert(
                    self.snapshot.monitors[m].pages[p].columns[c].id.clone(),
                    id.into(),
                );
            }
        }
    }

    fn column_focus_target(&self, column: &Column) -> Option<WindowId> {
        self.column_focus
            .get(&column.id)
            .filter(|id| column.windows.contains(id) && self.navigable(id))
            .or_else(|| column.windows.iter().find(|id| self.navigable(id)))
            .cloned()
    }

    fn set_focus(&mut self, id: &str, ensure_visible: bool) -> Result<(), AppError> {
        let (m, p, _) = self.location(id)?;
        self.snapshot.monitors[m].active_page = self.snapshot.monitors[m].pages[p].id.clone();
        self.snapshot.active_monitor = Some(self.snapshot.monitors[m].monitor.id.clone());
        self.snapshot.focused_window = Some(id.into());
        self.remember_column_focus(id);
        self.page_focus
            .insert(self.snapshot.monitors[m].active_page.clone(), id.into());
        // Floating focus leaves a tiled layout fullscreen in place; the floating window covers it.
        let floating = self.snapshot.windows[self.window_index(id)?].floating;
        if !floating {
            let neighbors: Vec<_> = ids(&self.snapshot.monitors[m].pages[p]).cloned().collect();
            for window in &mut self.snapshot.windows {
                if window.native.id != id && neighbors.contains(&window.native.id) {
                    window.fullscreen = false;
                    if let Some(rect) = self.fullscreen_restore.remove(&window.native.id) {
                        window.native.rect = rect;
                    }
                }
            }
        }
        // A suspended monitor keeps its scroll; revealing would reflow under the covering window.
        if ensure_visible && !self.monitor_suspended(m) {
            self.ensure_visible(id, false)?;
        }
        Ok(())
    }

    /// Windows that inherit focus when `id` closes, best first: the rest of its column (below,
    /// then above), then the columns to its right, then to its left.
    fn neighbours(&self, id: &str) -> Result<Vec<WindowId>, AppError> {
        let (m, p, location) = self.location(id)?;
        let page = &self.snapshot.monitors[m].pages[p];
        let Some((c, r)) = location else {
            return Ok(ids(page).filter(|w| *w != id).cloned().collect());
        };
        let column = &page.columns[c].windows;
        let mut out: Vec<WindowId> = column[r + 1..]
            .iter()
            .chain(column[..r].iter().rev())
            .cloned()
            .collect();
        for c in (c + 1..page.columns.len()).chain((0..c).rev()) {
            if let Some(id) = self.column_focus_target(&page.columns[c]) {
                out.push(id);
            }
        }
        Ok(out)
    }

    fn focus_active_page(&mut self) {
        let Some(m) = self
            .snapshot
            .active_monitor
            .as_ref()
            .and_then(|id| self.monitor_index(id).ok())
        else {
            return;
        };
        let monitor = &self.snapshot.monitors[m];
        let page = monitor
            .pages
            .iter()
            .find(|p| p.id == monitor.active_page)
            .unwrap();
        let id = self
            .page_focus
            .get(&page.id)
            .filter(|id| {
                ids(page).any(|w| w == *id)
                    && self.snapshot.windows.iter().any(|w| {
                        &w.native.id == *id
                            && (!w.native.minimized || w.native.minimized_by_manager)
                    })
            })
            .cloned()
            .or_else(|| {
                ids(page)
                    .find(|id| {
                        self.snapshot.windows.iter().any(|w| {
                            &w.native.id == *id
                                && (!w.native.minimized || w.native.minimized_by_manager)
                        })
                    })
                    .cloned()
            });
        self.snapshot.focused_window = None;
        if let Some(id) = id {
            let _ = self.set_focus(&id, true);
        }
    }

    fn ensure_visible(&mut self, id: &str, center: bool) -> Result<(), AppError> {
        let (m, p, column) = self.location(id)?;
        let Some((c, _)) = column else {
            return Ok(());
        };
        let viewport = self.snapshot.monitors[m].viewport.width as i64;
        let page = &mut self.snapshot.monitors[m].pages[p];
        let left: i64 = page.columns[..c].iter().map(|c| c.width as i64).sum();
        let width = page.columns[c].width as i64;
        let x = page.viewport_x as i64;
        let target = if center {
            left + (width - viewport) / 2
        } else if left < x || width >= viewport {
            left
        } else if left + width > x + viewport {
            left + width - viewport
        } else {
            x
        };
        // Keep an already visible column in place, including explicit centering/queue gaps.
        page.viewport_x = clamp_scroll_relaxed(page, viewport as u32, target);
        Ok(())
    }

    pub fn dispatch(&mut self, command: Command) -> Result<Transition, AppError> {
        let mut next = self.clone();
        let actions = next.dispatch_inner(command)?;
        next.update_pending_rule_floating(&actions);
        *self = next;
        Ok(self.transition(actions))
    }

    fn dispatch_inner(&mut self, command: Command) -> Result<Vec<NativeAction>, AppError> {
        if command == Command::Refresh {
            return Ok(vec![]);
        }
        if command == Command::Disable {
            if !self.snapshot.enabled {
                return Ok(vec![]);
            }
            self.snapshot.enabled = false;
            return Ok(self
                .snapshot
                .windows
                .iter()
                .map(|w| NativeAction::Restore {
                    window_id: w.native.id.clone(),
                })
                .collect());
        }
        if self.outputs_suspended {
            return Err(invalid(
                "No outputs available; waiting for monitor enumeration",
            ));
        }
        if command == Command::Enable {
            let backend = &self.snapshot.backend;
            if backend.availability != BackendAvailability::Ready {
                let code = match backend.availability {
                    BackendAvailability::PermissionRequired => ErrorCode::PermissionRequired,
                    BackendAvailability::UnsupportedSession => ErrorCode::UnsupportedSession,
                    BackendAvailability::NotImplemented => ErrorCode::NotImplemented,
                    _ => ErrorCode::BackendUnavailable,
                };
                return Err(AppError {
                    code,
                    message: backend.message.clone(),
                    window_id: None,
                });
            }
            let caps = &backend.capabilities;
            if !caps.enumerate || !caps.placement || !caps.minimize {
                return Err(AppError {
                    code: ErrorCode::OperationDenied,
                    message: "Management requires enumeration, placement and minimization".into(),
                    window_id: None,
                });
            }
            self.snapshot.enabled = true;
            if let Some(id) = self.snapshot.focused_window.clone() {
                self.ensure_visible(&id, false)?;
            }
            return self.placements();
        }
        if !self.snapshot.enabled {
            return Err(invalid("Window management is paused"));
        }
        if self.command_hits_suspended(&command) {
            return Ok(vec![]);
        }
        let mut focus_action = false;
        match command {
            Command::FocusWindow { window_id } => {
                self.require_focus()?;
                let w = self.window_index(&window_id)?;
                self.snapshot.windows[w].native.minimized = false;
                self.sync_minimized();
                self.set_focus(&window_id, true)?;
                focus_action = true;
            }
            Command::FocusDirection { direction } => {
                self.require_focus()?;
                let id = self.focused()?;
                let (m, p, location) = self.location(&id)?;
                let page = &self.snapshot.monitors[m].pages[p];
                let target = if let Some((c, w)) = location {
                    match direction {
                        Direction::Up => {
                            w.checked_sub(1).map(|w| page.columns[c].windows[w].clone())
                        }
                        Direction::Down => page.columns[c].windows.get(w + 1).cloned(),
                        Direction::Left | Direction::Right => {
                            let c = if direction == Direction::Left {
                                c.checked_sub(1)
                            } else {
                                Some(c + 1)
                            };
                            c.and_then(|c| page.columns.get(c))
                                .and_then(|c| self.column_focus_target(c))
                        }
                    }
                } else {
                    let windows: Vec<_> =
                        ids(page).filter(|w| self.navigable(w)).cloned().collect();
                    let index = windows.iter().position(|w| w == &id).unwrap();
                    let index = match direction {
                        Direction::Left | Direction::Up => index.checked_sub(1),
                        _ => Some(index + 1),
                    };
                    index.and_then(|i| windows.get(i).cloned())
                };
                if let Some(target) = target.filter(|id| self.navigable(id)) {
                    let w = self.window_index(&target)?;
                    self.snapshot.windows[w].native.minimized = false;
                    self.set_focus(&target, true)?;
                    focus_action = true;
                }
            }
            Command::SwitchPage {
                monitor_id,
                page_id,
            } => {
                let m = self.monitor_index(&monitor_id)?;
                let (target, _) = self.page_index(&page_id)?;
                if m != target {
                    return Err(invalid("Page belongs to a different monitor"));
                }
                self.snapshot.monitors[m].active_page = page_id;
                self.snapshot.active_monitor = Some(monitor_id);
                self.focus_active_page();
                focus_action = true;
            }
            Command::AddPage { monitor_id } => {
                let m = self.monitor_index(&monitor_id)?;
                // Dynamic desktops already have an empty tail; select it rather than accumulating empties.
                let page = self.snapshot.monitors[m].pages.last().unwrap().id.clone();
                self.snapshot.monitors[m].active_page = page;
                self.snapshot.active_monitor = Some(monitor_id);
                self.focus_active_page();
            }
            Command::MoveWindowToPage { window_id, page_id } => {
                let (m, p) = self.page_index(&page_id)?;
                let (old_m, old_p, column) = self.location(&window_id)?;
                if (m, p) == (old_m, old_p) {
                    return Ok(vec![]);
                }
                self.record_hotplug_move(&window_id, &page_id);
                let width = column
                    .map(|(c, _)| self.snapshot.monitors[old_m].pages[old_p].columns[c].width);
                if m != old_m && column.is_none() {
                    let source = self.snapshot.monitors[old_m].viewport;
                    let target = self.snapshot.monitors[m].viewport;
                    let w = self.window_index(&window_id)?;
                    let translate = |rect: &mut Rect| {
                        rect.x =
                            coordinate((rect.x as i64 + target.x as i64 - source.x as i64).clamp(
                                target.x as i64,
                                target.x as i64 + target.width.saturating_sub(rect.width) as i64,
                            ));
                        rect.y =
                            coordinate((rect.y as i64 + target.y as i64 - source.y as i64).clamp(
                                target.y as i64,
                                target.y as i64 + target.height.saturating_sub(rect.height) as i64,
                            ));
                    };
                    translate(&mut self.snapshot.windows[w].native.rect);
                    if let Some(rect) = self.fullscreen_restore.get_mut(&window_id) {
                        translate(rect);
                    }
                }
                self.remove_window(&window_id);
                self.insert_window(m, p, &window_id, width)?;
                self.set_focus(&window_id, true)?;
                focus_action = true;
            }
            Command::MoveWindow { direction } => {
                let id = self.focused()?;
                let (m, p, location) = self.location(&id)?;
                let (c, w) = location
                    .ok_or_else(|| invalid("Floating windows do not have a tiled order"))?;
                let page = &mut self.snapshot.monitors[m].pages[p];
                match direction {
                    Direction::Up if w > 0 => page.columns[c].windows.swap(w, w - 1),
                    Direction::Down if w + 1 < page.columns[c].windows.len() => {
                        page.columns[c].windows.swap(w, w + 1)
                    }
                    Direction::Left | Direction::Right => {
                        let target = if direction == Direction::Left {
                            c.checked_sub(1)
                        } else if c + 1 < page.columns.len() {
                            Some(c + 1)
                        } else {
                            None
                        };
                        if let Some(target) = target {
                            page.columns[c].windows.remove(w);
                            page.columns[target].windows.push(id.clone());
                        } else if page.columns[c].windows.len() > 1 {
                            let width = page.columns[c].width;
                            page.columns[c].windows.remove(w);
                            let column = self.column(id.clone(), width);
                            self.snapshot.monitors[m].pages[p].columns.insert(
                                if direction == Direction::Left {
                                    c
                                } else {
                                    c + 1
                                },
                                column,
                            );
                        }
                    }
                    _ => {}
                }
                self.cleanup();
                self.ensure_visible(&id, false)?;
            }
            Command::CycleWidth => {
                let (id, m, p, c, _) = self.sizing_target()?;
                let viewport = self.snapshot.monitors[m].viewport.width;
                // niri preset-column-widths: 1/3, 1/2, 2/3; full width stays on ToggleFullscreen.
                let presets = [
                    (viewport / 3).max(1),
                    (viewport / 2).max(1),
                    ((viewport as u64 * 2 / 3) as u32).max(1),
                ];
                let width = &mut self.snapshot.monitors[m].pages[p].columns[c].width;
                *width = presets
                    .into_iter()
                    .find(|preset| preset > width)
                    .unwrap_or(presets[0]);
                self.ensure_visible(&id, false)?;
            }
            Command::SetColumnWidth { width } => {
                if width == 0 {
                    return Err(invalid("Column width must be positive"));
                }
                self.resize_column(width as i64, false)?;
            }
            Command::SetWindowColumnWidth { window_id, width } => {
                if width == 0 {
                    return Err(invalid("Column width must be positive"));
                }
                self.resize_window_column(&window_id, width as i64, false)?;
            }
            Command::AdjustColumnWidth { delta } => {
                self.resize_column(delta as i64, true)?;
            }
            Command::AdjustWindowHeight { delta } => self.adjust_window_height(delta)?,
            Command::ResetWindowHeights => {
                let (_, m, p, c, _) = self.sizing_target()?;
                for id in &self.snapshot.monitors[m].pages[p].columns[c].windows {
                    self.height_weights.remove(id);
                }
            }
            Command::CenterFocused => {
                let id = self.focused()?;
                self.ensure_visible(&id, true)?;
            }
            Command::Scroll { monitor_id, delta } => {
                let m = self.monitor_index(&monitor_id)?;
                let monitor = &mut self.snapshot.monitors[m];
                let page = monitor
                    .pages
                    .iter_mut()
                    .find(|p| p.id == monitor.active_page)
                    .unwrap();
                page.viewport_x = coordinate(edges::snap_scroll(
                    &widths(page),
                    monitor.viewport.width,
                    page.viewport_x as i64,
                    delta as i64,
                ));
            }
            Command::SlideColumn { direction } => {
                self.require_focus()?;
                if !self.slide_column(direction)? {
                    if self.snapshot.focused_window.is_none() {
                        return Ok(vec![]);
                    }
                    return self.dispatch_inner(Command::FocusDirection { direction });
                }
                focus_action = true;
            }
            Command::DragEdge {
                monitor_id,
                edge,
                delta,
            } => {
                let m = self.monitor_index(&monitor_id)?;
                let monitor = &mut self.snapshot.monitors[m];
                let view = monitor.viewport.width;
                let snap = snap_distance(monitor.monitor.scale_factor, view);
                let gap = 2 * half_gap(self.snapshot.gaps, monitor.monitor.scale_factor);
                let page = monitor
                    .pages
                    .iter_mut()
                    .find(|p| p.id == monitor.active_page)
                    .unwrap();
                if edge as usize > page.columns.len() {
                    return Err(invalid("Column boundary does not exist"));
                }
                let edge = edge as usize;
                let original_widths = widths(page);
                let original_x = i64::from(page.viewport_x);
                let (mut widths, mut x) = edges::drag_edge(
                    &original_widths,
                    view,
                    original_x,
                    edge,
                    delta as i64,
                    snap,
                );
                if delta != 0 && edge > 0 && edge < widths.len() {
                    let half = view / 2;
                    let left_full = edges::edge_position(&original_widths, original_x, edge - 1) >= 0;
                    let right_full = edges::edge_position(&original_widths, original_x, edge + 1)
                        <= i64::from(view);
                    let clipped = match (left_full, right_full) {
                        (true, false) if widths[edge - 1] == half => Some(edge),
                        (false, true) if widths[edge] == half => Some(edge - 1),
                        _ => None,
                    };
                    if let Some(clipped) = clipped {
                        // At the middle snap, bring the clipped neighbour fully on screen too,
                        // but only if both columns' real minimums allow the half-width split.
                        let can_split = page.columns[edge - 1..=edge].iter().all(|column| {
                            column
                                .windows
                                .iter()
                                .filter_map(|id| self.min_widths.get(id))
                                .max()
                                .map_or(0, |width| width + gap)
                                <= half
                        });
                        if widths[clipped] > half
                            && edges::edge_position(&widths, x, edge) == i64::from(half)
                            && can_split
                        {
                            widths[clipped] = half;
                            x = edges::edge_position(&widths, 0, edge) - i64::from(half);
                            x = edges::clamp_x_relaxed(&widths, view, x);
                        }
                    }
                }
                for (column, width) in page.columns.iter_mut().zip(widths) {
                    column.width = width;
                }
                page.viewport_x = coordinate(x);
                let p = self.snapshot.monitors[m]
                    .pages
                    .iter()
                    .position(|p| p.id == self.snapshot.monitors[m].active_page)
                    .unwrap();
                let before = self.snapshot.focused_window.clone();
                self.keep_focus_on_screen(m, p)?;
                focus_action = self.snapshot.focused_window != before;
            }
            Command::DragRow {
                monitor_id,
                column,
                edge,
                delta,
            } => {
                let m = self.monitor_index(&monitor_id)?;
                let before = self.snapshot.focused_window.clone();
                self.drag_row(m, column as usize, edge as usize, delta.into())?;
                focus_action = self.snapshot.focused_window != before;
            }
            Command::ToggleFloating => {
                let id = self.focused()?;
                let w = self.window_index(&id)?;
                if self.snapshot.windows[w].floating && !self.snapshot.windows[w].native.resizable {
                    return Err(invalid("Nonresizable windows must remain floating"));
                }
                let (m, p, _) = self.location(&id)?;
                self.remove_window(&id);
                self.size_floating.remove(&id);
                self.snapshot.windows[w].floating = !self.snapshot.windows[w].floating;
                self.snapshot.windows[w].fullscreen = false;
                if let Some(rect) = self.fullscreen_restore.remove(&id) {
                    self.snapshot.windows[w].native.rect = rect;
                }
                self.insert_window(m, p, &id, None)?;
                self.ensure_visible(&id, false)?;
            }
            Command::ToggleFullscreen => {
                let id = self.focused()?;
                let w = self.window_index(&id)?;
                if !self.snapshot.windows[w].native.resizable {
                    return Err(invalid("Nonresizable windows cannot fill the viewport"));
                }
                let window = &mut self.snapshot.windows[w];
                window.fullscreen = !window.fullscreen;
                if window.fullscreen && window.floating {
                    self.fullscreen_restore
                        .insert(id.clone(), window.native.rect);
                } else if let Some(rect) = self.fullscreen_restore.remove(&id) {
                    window.native.rect = rect;
                }
                self.ensure_visible(&id, false)?;
            }
            Command::CloseWindow { window_id } => {
                self.window_index(&window_id)?;
                if !self.snapshot.backend.capabilities.close {
                    return Err(invalid("Backend does not support graceful close"));
                }
                // Keep membership until enumeration confirms native destruction (close can be cancelled).
                return Ok(vec![NativeAction::Close { window_id }]);
            }
            Command::DropWindow {
                window_id,
                x,
                y,
                page_id,
                viewport_x,
            } => {
                let before = self.snapshot.focused_window.clone();
                self.drop_window(&window_id, x, y, page_id.as_deref(), viewport_x)?;
                focus_action = self.snapshot.focused_window.as_deref() == Some(&window_id)
                    || self.snapshot.focused_window != before;
            }
            Command::SetFloatingRect { window_id, rect } => {
                self.set_floating_rect(&window_id, rect)?;
            }
            Command::Enable | Command::Disable | Command::Refresh => unreachable!(),
        }
        self.cleanup();
        let mut actions = self.placements()?;
        if let Some(id) = self
            .snapshot
            .focused_window
            .as_ref()
            .filter(|_| focus_action && self.snapshot.backend.capabilities.focus)
        {
            actions.push(NativeAction::Focus {
                window_id: id.clone(),
            });
        }
        Ok(actions)
    }

    /// Scroll the active page until the next column toward `direction` is fully visible and
    /// focus its last focused window. False when every column there is visible.
    fn slide_column(&mut self, direction: Direction) -> Result<bool, AppError> {
        let right = match direction {
            Direction::Left => false,
            Direction::Right => true,
            _ => return Err(invalid("Columns slide left or right")),
        };
        let m = self
            .snapshot
            .active_monitor
            .as_ref()
            .and_then(|id| self.monitor_index(id).ok())
            .ok_or_else(|| invalid("No active monitor"))?;
        let monitor = &self.snapshot.monitors[m];
        let view = monitor.viewport.width as i64;
        let p = monitor
            .pages
            .iter()
            .position(|p| p.id == monitor.active_page)
            .unwrap();
        let page = &monitor.pages[p];
        let x = page.viewport_x as i64;
        let (mut left, mut target) = (0i64, None);
        for (c, column) in page.columns.iter().enumerate() {
            let width = column.width as i64;
            if right && left + width > x + view {
                target = Some((c, if width >= view { left } else { left + width - view }));
                break;
            }
            if !right && left < x {
                target = Some((c, left));
            }
            left += width;
        }
        let Some((c, scroll)) = target else {
            return Ok(false);
        };
        let id = self.column_focus_target(&page.columns[c]);
        let page = &mut self.snapshot.monitors[m].pages[p];
        page.viewport_x = clamp_scroll(page, view as u32, scroll);
        let Some(id) = id else {
            return Ok(true);
        };
        let w = self.window_index(&id)?;
        // Manager-hidden columns come back on screen. User-minimized windows are not targets.
        self.snapshot.windows[w].native.minimized = false;
        self.set_focus(&id, false)?;
        Ok(true)
    }

    fn require_focus(&self) -> Result<(), AppError> {
        if self.snapshot.backend.capabilities.focus {
            Ok(())
        } else {
            Err(invalid("Backend does not support focus"))
        }
    }

    // ponytail: linear window lookups keep snapshot ownership simple; index IDs if large window counts matter.
    fn placements(&self) -> Result<Vec<NativeAction>, AppError> {
        // Reject impossible geometry transactionally, including viewport changes while paused.
        for monitor in &self.snapshot.monitors {
            if monitor
                .pages
                .iter()
                .flat_map(|p| &p.columns)
                .any(|c| c.windows.len() as u64 > monitor.viewport.height as u64)
            {
                return Err(invalid(
                    "Viewport height is smaller than the tiled window count",
                ));
            }
        }
        if !self.snapshot.enabled {
            return Ok(vec![]);
        }
        let mut actions = Vec::new();
        for monitor in &self.snapshot.monitors {
            // Leave the covering window and the tiles under it where they are.
            if self
                .snapshot
                .suspended_monitors
                .iter()
                .any(|id| id == &monitor.monitor.id)
            {
                continue;
            }
            for page in &monitor.pages {
                let active = page.id == monitor.active_page;
                let fullscreen = ids(page).find(|id| {
                    self.snapshot
                        .windows
                        .iter()
                        .any(|w| &w.native.id == *id && w.fullscreen)
                });
                let half = half_gap(self.snapshot.gaps, monitor.monitor.scale_factor);
                let outer = expand_gap(monitor.viewport, half);
                let uncovered = self.uncovered_monitors(&monitor.monitor.id);
                let mut x = monitor.viewport.x as i64 - page.viewport_x as i64;
                for column in &page.columns {
                    let heights = self.column_heights(column, monitor.viewport.height);
                    let mut y = monitor.viewport.y as i64;
                    for (id, height) in column.windows.iter().zip(heights) {
                        let rect = inset_gap(
                            Rect {
                                x: coordinate(x),
                                y: coordinate(y),
                                width: column.width,
                                height,
                            },
                            half,
                        );
                        self.place(&mut actions, id, rect, outer, &uncovered, active, fullscreen);
                        y += height as i64;
                    }
                    x += column.width as i64;
                }
                for id in &page.floating_windows {
                    let window = &self.snapshot.windows[self.window_index(id).unwrap()];
                    self.place(
                        &mut actions,
                        id,
                        window.native.rect,
                        outer,
                        &[],
                        active,
                        fullscreen,
                    );
                }
            }
        }
        Ok(actions)
    }

    /// Bounds of other monitors whose active page shows no tiled windows. Composition content
    /// ignores window regions, so a clipped window relies on the neighbouring monitor's own
    /// windows to hide the part cut off at the edge (the backend keeps it below them).
    fn uncovered_monitors(&self, own: &str) -> Vec<Rect> {
        self.snapshot
            .monitors
            .iter()
            .filter(|m| {
                m.monitor.id != own
                    && m.pages
                        .iter()
                        .find(|p| p.id == m.active_page)
                        .is_none_or(|p| p.columns.is_empty())
            })
            .map(|m| m.monitor.bounds)
            .collect()
    }

    fn place(
        &self,
        actions: &mut Vec<NativeAction>,
        id: &str,
        rect: Rect,
        bounds: Rect,
        uncovered: &[Rect],
        active: bool,
        fullscreen: Option<&WindowId>,
    ) {
        let window = &self.snapshot.windows[self.window_index(id).unwrap()];
        if self.covering_windows.contains(id)
            || (window.native.minimized && !window.native.minimized_by_manager)
        {
            return;
        }
        // `bounds` is the viewport expanded by half a gap: fullscreen has no gap, and
        // scrolling may draw tiled windows into that margin instead of clipping there.
        let rect = if window.fullscreen { bounds } else { rect };
        let left = (rect.x as i64).max(bounds.x as i64);
        let top = (rect.y as i64).max(bounds.y as i64);
        let right = (rect.x as i64 + rect.width as i64).min(bounds.x as i64 + bounds.width as i64);
        let bottom =
            (rect.y as i64 + rect.height as i64).min(bounds.y as i64 + bounds.height as i64);
        let visible = right > left && bottom > top;
        let fully_visible = visible
            && left == rect.x as i64
            && top == rect.y as i64
            && right == rect.x as i64 + rect.width as i64
            && bottom == rect.y as i64 + rect.height as i64;
        let clipping = self.snapshot.backend.capabilities.clipping;
        // A monitor without tiled windows cannot cover the part cut off at this edge.
        let spills = !fully_visible && uncovered.iter().any(|b| overlaps(rect, *b));
        // Without native clipping, hide whole edge windows rather than leaking onto another monitor.
        // A floating window stays up over layout fullscreen instead of being covered by it.
        let minimized = !active
            || (fullscreen.is_some_and(|w| w != id) && !window.floating)
            || (!window.floating && (!visible || spills || (!clipping && !fully_visible)));
        let clip = if clipping && !window.floating && !minimized && visible && !fully_visible {
            Some(Rect {
                x: coordinate(left),
                y: coordinate(top),
                width: (right - left) as u32,
                height: (bottom - top) as u32,
            })
        } else {
            None
        };
        actions.push(NativeAction::Placement {
            window_id: id.into(),
            rect,
            clip,
            minimized,
        });
    }
}

#[cfg(test)]
mod tests;
