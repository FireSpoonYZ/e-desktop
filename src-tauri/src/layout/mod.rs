use std::collections::{BTreeMap, BTreeSet};

use crate::model::*;

/// Monitor/page indices plus optional column/row (None for floating windows).
type WindowLocation = (usize, usize, Option<(usize, usize)>);

/// Pure, transactional layout state. Native effects are returned to the caller, never applied here.
#[derive(Clone)]
pub struct Engine {
    snapshot: Snapshot,
    next_id: u64,
    viewports: BTreeMap<MonitorId, Rect>,
    page_focus: BTreeMap<PageId, WindowId>,
    fullscreen_restore: BTreeMap<WindowId, Rect>,
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

fn clamp_scroll(page: &Page, viewport: u32, target: i64) -> i32 {
    let Some(first) = page.columns.first() else {
        return 0;
    };
    let last = page.columns.last().unwrap();
    let extent: i64 = page.columns.iter().map(|c| c.width as i64).sum();
    // Half-column edge margins permit centering even the first/last column.
    let min = ((first.width as i64 - viewport as i64) / 2).min(0);
    let max =
        (extent - viewport as i64 + ((viewport as i64 - last.width as i64) / 2).max(0)).max(0);
    coordinate(target.clamp(min, max))
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
            fullscreen_restore: BTreeMap::new(),
        }
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
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
        let mut actions = next.placements();
        if let Some(id) = next.snapshot.focused_window.as_ref().filter(|_| {
            next.snapshot.enabled
                && next.snapshot.backend.capabilities.focus
                && self.snapshot.focused_window != next.snapshot.focused_window
                && native_focus != next.snapshot.focused_window
        }) {
            actions.push(NativeAction::Focus {
                window_id: id.clone(),
            });
        }
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
        let viewport_changed = system.monitors.iter().any(|monitor| {
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
        self.snapshot
            .monitors
            .retain(|m| monitor_ids.contains(&m.monitor.id));
        for monitor in system.monitors {
            let viewport = self
                .viewports
                .get(&monitor.id)
                .copied()
                .unwrap_or(monitor.work_area);
            if let Some(existing) = self
                .snapshot
                .monitors
                .iter_mut()
                .find(|m| m.monitor.id == monitor.id)
            {
                existing.monitor = monitor;
                existing.viewport = viewport;
            } else {
                let page = self.page();
                self.snapshot.monitors.push(MonitorState {
                    monitor,
                    active_page: page.id.clone(),
                    pages: vec![page],
                    viewport,
                });
            }
        }
        self.snapshot
            .windows
            .retain(|w| window_ids.contains(&w.native.id));
        for mut native in system.windows {
            if let Some(existing) = self
                .snapshot
                .windows
                .iter_mut()
                .find(|w| w.native.id == native.id)
            {
                // Iconic rectangles are not floating restore targets. Keep the last normal
                // geometry; fullscreen_restore separately owns the pre-fullscreen rectangle.
                if existing.floating && native.minimized {
                    native.rect = existing.native.rect;
                }
                existing.native = native;
            } else {
                let floating = !native.resizable;
                self.snapshot.windows.push(WindowState {
                    native,
                    floating,
                    fullscreen: false,
                });
            }
        }
        for monitor in &mut self.snapshot.monitors {
            for page in &mut monitor.pages {
                for column in &mut page.columns {
                    column.windows.retain(|id| window_ids.contains(id));
                }
                page.floating_windows.retain(|id| window_ids.contains(id));
            }
        }
        // Preserve logical monitor/page membership: clipped native rectangles can straddle monitors.
        for window in self.snapshot.windows.clone() {
            if self.location(&window.native.id).is_ok() {
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
        self.cleanup();
        let previous = self.snapshot.focused_window.clone();
        if let Some(id) = system.focused_window {
            let (m, p, _) = self.location(&id)?;
            // A stale native focus on a manager-hidden page must not undo a page switch.
            let native = &self.snapshot.windows[self.window_index(&id)?].native;
            if !self.snapshot.enabled
                || (!native.minimized
                    && self.snapshot.monitors[m].pages[p].id
                        == self.snapshot.monitors[m].active_page)
            {
                self.set_focus(&id, viewport_changed || previous.as_ref() != Some(&id))?;
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

    fn cleanup(&mut self) {
        for m in 0..self.snapshot.monitors.len() {
            let monitor = &mut self.snapshot.monitors[m];
            for page in &mut monitor.pages {
                for column in &mut page.columns {
                    column.width = column.width.min(monitor.viewport.width).max(1);
                }
                page.columns.retain(|c| !c.windows.is_empty());
            }
            // Keep the selected empty page and a stable empty tail, but coalesce adjacent empties.
            let last = monitor.pages.last().map(|p| p.id.clone());
            monitor.pages.retain(|p| {
                !empty(p) || p.id == monitor.active_page || Some(&p.id) == last.as_ref()
            });
            if monitor.pages.len() > 1
                && empty(monitor.pages.last().unwrap())
                && empty(&monitor.pages[monitor.pages.len() - 2])
            {
                monitor.pages.pop();
            }
            if monitor.pages.last().is_none_or(|p| !empty(p)) {
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
                    clamp_scroll(page, monitor.viewport.width, page.viewport_x as i64);
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
    }

    fn set_focus(&mut self, id: &str, ensure_visible: bool) -> Result<(), AppError> {
        let (m, p, _) = self.location(id)?;
        self.snapshot.monitors[m].active_page = self.snapshot.monitors[m].pages[p].id.clone();
        self.snapshot.active_monitor = Some(self.snapshot.monitors[m].monitor.id.clone());
        self.snapshot.focused_window = Some(id.into());
        self.page_focus
            .insert(self.snapshot.monitors[m].active_page.clone(), id.into());
        let neighbors: Vec<_> = ids(&self.snapshot.monitors[m].pages[p]).cloned().collect();
        for window in &mut self.snapshot.windows {
            if window.native.id != id && neighbors.contains(&window.native.id) {
                window.fullscreen = false;
                if let Some(rect) = self.fullscreen_restore.remove(&window.native.id) {
                    window.native.rect = rect;
                }
            }
        }
        if ensure_visible {
            self.ensure_visible(id, false)?;
        }
        Ok(())
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
        page.viewport_x = clamp_scroll(page, viewport as u32, target);
        Ok(())
    }

    pub fn dispatch(&mut self, command: Command) -> Result<Transition, AppError> {
        let mut next = self.clone();
        let actions = next.dispatch_inner(command)?;
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
            return Ok(self.placements());
        }
        if !self.snapshot.enabled {
            return Err(invalid("Window management is paused"));
        }
        let mut focus_action = false;
        match command {
            Command::FocusWindow { window_id } => {
                self.require_focus()?;
                let w = self.window_index(&window_id)?;
                self.snapshot.windows[w].native.minimized = false;
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
                                .map(|c| c.windows[w.min(c.windows.len() - 1)].clone())
                        }
                    }
                } else {
                    let windows: Vec<_> = ids(page).cloned().collect();
                    let index = windows.iter().position(|w| w == &id).unwrap();
                    let index = match direction {
                        Direction::Left | Direction::Up => index.checked_sub(1),
                        _ => Some(index + 1),
                    };
                    index.and_then(|i| windows.get(i).cloned())
                };
                if let Some(target) = target {
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
                let id = self.focused()?;
                let (m, p, column) = self.location(&id)?;
                let (c, _) =
                    column.ok_or_else(|| invalid("Floating windows have no column width"))?;
                let viewport = self.snapshot.monitors[m].viewport.width;
                let presets = [
                    (viewport / 3).max(1),
                    (viewport / 2).max(1),
                    ((viewport as u64 * 2 / 3) as u32).max(1),
                    viewport,
                ];
                let width = &mut self.snapshot.monitors[m].pages[p].columns[c].width;
                *width = presets
                    .into_iter()
                    .find(|preset| preset > width)
                    .unwrap_or(presets[0]);
                self.ensure_visible(&id, false)?;
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
                page.viewport_x = coordinate(page.viewport_x as i64 + delta as i64);
            }
            Command::ToggleFloating => {
                let id = self.focused()?;
                let w = self.window_index(&id)?;
                if self.snapshot.windows[w].floating && !self.snapshot.windows[w].native.resizable {
                    return Err(invalid("Nonresizable windows must remain floating"));
                }
                let (m, p, _) = self.location(&id)?;
                self.remove_window(&id);
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
            Command::Enable | Command::Disable | Command::Refresh => unreachable!(),
        }
        self.cleanup();
        let mut actions = self.placements();
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

    fn require_focus(&self) -> Result<(), AppError> {
        if self.snapshot.backend.capabilities.focus {
            Ok(())
        } else {
            Err(invalid("Backend does not support focus"))
        }
    }

    // ponytail: linear window lookups keep snapshot ownership simple; index IDs if large window counts matter.
    fn placements(&self) -> Vec<NativeAction> {
        if !self.snapshot.enabled {
            return vec![];
        }
        let mut actions = Vec::new();
        for monitor in &self.snapshot.monitors {
            for page in &monitor.pages {
                let active = page.id == monitor.active_page;
                let fullscreen = ids(page).find(|id| {
                    self.snapshot
                        .windows
                        .iter()
                        .any(|w| &w.native.id == *id && w.fullscreen)
                });
                let mut x = monitor.viewport.x as i64 - page.viewport_x as i64;
                for column in &page.columns {
                    let count = column.windows.len() as u64;
                    for (index, id) in column.windows.iter().enumerate() {
                        let top = monitor.viewport.height as u64 * index as u64 / count;
                        let bottom = monitor.viewport.height as u64 * (index as u64 + 1) / count;
                        let rect = Rect {
                            x: coordinate(x),
                            y: coordinate(monitor.viewport.y as i64 + top as i64),
                            width: column.width,
                            height: (bottom - top).max(1) as u32,
                        };
                        self.place(&mut actions, id, rect, monitor.viewport, active, fullscreen);
                    }
                    x += column.width as i64;
                }
                for id in &page.floating_windows {
                    let window = &self.snapshot.windows[self.window_index(id).unwrap()];
                    self.place(
                        &mut actions,
                        id,
                        window.native.rect,
                        monitor.viewport,
                        active,
                        fullscreen,
                    );
                }
            }
        }
        actions
    }

    fn place(
        &self,
        actions: &mut Vec<NativeAction>,
        id: &str,
        rect: Rect,
        viewport: Rect,
        active: bool,
        fullscreen: Option<&WindowId>,
    ) {
        let window = &self.snapshot.windows[self.window_index(id).unwrap()];
        if window.native.minimized && !window.native.minimized_by_manager {
            return;
        }
        let rect = if window.fullscreen { viewport } else { rect };
        let left = (rect.x as i64).max(viewport.x as i64);
        let top = (rect.y as i64).max(viewport.y as i64);
        let right =
            (rect.x as i64 + rect.width as i64).min(viewport.x as i64 + viewport.width as i64);
        let bottom =
            (rect.y as i64 + rect.height as i64).min(viewport.y as i64 + viewport.height as i64);
        let visible = right > left && bottom > top;
        let fully_visible = visible
            && left == rect.x as i64
            && top == rect.y as i64
            && right == rect.x as i64 + rect.width as i64
            && bottom == rect.y as i64 + rect.height as i64;
        let clipping = self.snapshot.backend.capabilities.clipping;
        // Without native clipping, hide whole edge windows rather than leaking onto another monitor.
        let minimized = !active
            || fullscreen.is_some_and(|w| w != id)
            || (!window.floating && (!visible || (!clipping && !fully_visible)));
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
