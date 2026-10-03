//! lane: layout-actions. niri column, window, monitor and focus-history keyboard actions.

use super::*;

fn active_monitor(e: &Engine) -> Result<usize, AppError> {
    e.snapshot
        .active_monitor
        .as_ref()
        .and_then(|id| e.monitor_index(id).ok())
        .ok_or_else(|| invalid("No active monitor"))
}

fn active_page(e: &Engine, m: usize) -> usize {
    let monitor = &e.snapshot.monitors[m];
    monitor
        .pages
        .iter()
        .position(|p| p.id == monitor.active_page)
        .unwrap()
}

/// Focused tiled window as (id, monitor, page, column, row). Like MoveWindow, floating
/// windows have no column to act on; layout fullscreen windows still do.
fn focused_tile(e: &Engine) -> Result<(WindowId, usize, usize, usize, usize), AppError> {
    let id = e.focused()?;
    let (m, p, location) = e.location(&id)?;
    let (c, r) = location.ok_or_else(|| invalid("Floating windows do not have a tiled order"))?;
    Ok((id, m, p, c, r))
}

/// True for right. Column actions only go left or right.
fn rightward(direction: Direction) -> Result<bool, AppError> {
    match direction {
        Direction::Left => Ok(false),
        Direction::Right => Ok(true),
        _ => Err(invalid("Columns move left or right")),
    }
}

/// Index of the column next to column c on that side, if there is one.
fn beside(c: usize, right: bool, count: usize) -> Option<usize> {
    if right {
        Some(c + 1).filter(|&n| n < count)
    } else {
        c.checked_sub(1)
    }
}

/// niri output_left_of and friends: the monitor whose centre is nearest that way among those
/// overlapping the current monitor's row (left/right) or column (up/down).
fn monitor_toward(e: &Engine, m: usize, direction: Direction) -> Option<usize> {
    let span = |r: Rect, vertical: bool| {
        if vertical {
            (i64::from(r.y), i64::from(r.y) + i64::from(r.height))
        } else {
            (i64::from(r.x), i64::from(r.x) + i64::from(r.width))
        }
    };
    let current = e.snapshot.monitors[m].monitor.bounds;
    let horizontal = matches!(direction, Direction::Left | Direction::Right);
    // The overlap band runs across the direction; centres are compared along it.
    let (start, end) = span(current, horizontal);
    let (from, to) = span(current, !horizontal);
    let forward = matches!(direction, Direction::Right | Direction::Down);
    e.snapshot
        .monitors
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != m)
        .filter_map(|(i, monitor)| {
            let bounds = monitor.monitor.bounds;
            let (s, t) = span(bounds, horizontal);
            let (f, g) = span(bounds, !horizontal);
            // Doubled centres stay integral.
            let distance = if forward {
                (f + g) - (from + to)
            } else {
                (from + to) - (f + g)
            };
            (distance > 0 && s < end && start < t).then_some((distance, i))
        })
        .min()
        .map(|(_, i)| i)
}

/// Remove column c; like detach, the columns on screen keep their position.
fn take_column(e: &mut Engine, m: usize, p: usize, c: usize) -> Column {
    let page = &mut e.snapshot.monitors[m].pages[p];
    let left: i64 = page.columns[..c].iter().map(|c| i64::from(c.width)).sum();
    let width = i64::from(page.columns[c].width);
    let x = i64::from(page.viewport_x);
    if left + width <= x {
        page.viewport_x = coordinate(x - width);
    } else if left < x {
        page.viewport_x = coordinate(left);
    }
    page.columns.remove(c)
}

/// The focused window leaves its stack for a new column of the same width beside it.
fn expel(e: &mut Engine, id: &str, (m, p, c, r): (usize, usize, usize, usize), right: bool) {
    let width = e.snapshot.monitors[m].pages[p].columns[c].width;
    e.snapshot.monitors[m].pages[p].columns[c].windows.remove(r);
    let column = e.column(id.into(), width);
    let at = if right { c + 1 } else { c };
    e.snapshot.monitors[m].pages[p].columns.insert(at, column);
}

/// Focus a window that may sit on a hidden page or off screen (manager-minimized).
fn focus_hidden(e: &mut Engine, id: &str) -> Result<(), AppError> {
    let w = e.window_index(id)?;
    e.snapshot.windows[w].native.minimized = false;
    e.set_focus(id, true)
}

/// Some(actions) when FocusDirection moved focus; None at the end of the column or strip.
fn try_focus_direction(
    e: &mut Engine,
    direction: Direction,
) -> Result<Option<Vec<NativeAction>>, AppError> {
    let before = e.snapshot.focused_window.clone();
    if before.is_none() {
        return Ok(None);
    }
    let actions = e.dispatch_inner(Command::FocusDirection { direction })?;
    Ok((e.snapshot.focused_window != before).then_some(actions))
}

impl Engine {
    /// Some(actions) when the command finished through another command or skipped a
    /// suspended monitor; None continues with the shared cleanup and placement tail.
    pub(super) fn layout_action(
        &mut self,
        command: Command,
        focus: &mut bool,
    ) -> Result<Option<Vec<NativeAction>>, AppError> {
        // As command_hits_suspended: leave a paused monitor and its windows alone.
        if self
            .snapshot
            .focused_window
            .as_deref()
            .is_some_and(|id| self.window_protected(id))
            || self
                .snapshot
                .active_monitor
                .as_deref()
                .is_some_and(|id| self.hits_suspended(id))
        {
            return Ok(Some(vec![]));
        }
        match command {
            Command::ConsumeOrExpelWindow { direction } => {
                let right = rightward(direction)?;
                let (id, m, p, c, r) = focused_tile(self)?;
                let columns = &mut self.snapshot.monitors[m].pages[p].columns;
                if columns[c].windows.len() > 1 {
                    expel(self, &id, (m, p, c, r), right);
                } else if let Some(target) = beside(c, right, columns.len()) {
                    columns[c].windows.remove(r);
                    columns[target].windows.push(id.clone());
                }
                self.cleanup();
                self.ensure_visible(&id, false)?;
            }
            Command::ConsumeWindowIntoColumn => {
                let (id, m, p, c, _) = focused_tile(self)?;
                let columns = &mut self.snapshot.monitors[m].pages[p].columns;
                if c + 1 < columns.len() {
                    let first = columns[c + 1].windows.remove(0);
                    columns[c].windows.push(first);
                }
                self.cleanup();
                self.ensure_visible(&id, false)?;
            }
            Command::ExpelWindowFromColumn => {
                let (id, m, p, c, r) = focused_tile(self)?;
                if self.snapshot.monitors[m].pages[p].columns[c].windows.len() > 1 {
                    expel(self, &id, (m, p, c, r), true);
                }
                self.ensure_visible(&id, false)?;
            }
            Command::MoveColumn { direction } => {
                let right = rightward(direction)?;
                let (id, m, p, c, _) = focused_tile(self)?;
                let columns = &mut self.snapshot.monitors[m].pages[p].columns;
                if let Some(target) = beside(c, right, columns.len()) {
                    columns.swap(c, target);
                }
                self.ensure_visible(&id, false)?;
            }
            Command::MoveColumnToFirst | Command::MoveColumnToLast => {
                let (id, m, p, c, _) = focused_tile(self)?;
                let columns = &mut self.snapshot.monitors[m].pages[p].columns;
                let column = columns.remove(c);
                if command == Command::MoveColumnToFirst {
                    columns.insert(0, column);
                } else {
                    columns.push(column);
                }
                self.ensure_visible(&id, false)?;
            }
            Command::SwapWindow { direction } => {
                let right = rightward(direction)?;
                let (id, m, p, c, r) = focused_tile(self)?;
                let page = &self.snapshot.monitors[m].pages[p];
                let Some(t) = beside(c, right, page.columns.len()) else {
                    return Ok(None);
                };
                let Some(other) = self.column_focus_target(&page.columns[t]) else {
                    return Ok(None);
                };
                let row = page.columns[t].windows.iter().position(|w| w == &other);
                let page = &mut self.snapshot.monitors[m].pages[p];
                page.columns[c].windows[r] = other.clone();
                page.columns[t].windows[row.unwrap()] = id.clone();
                // Focus follows the window; its old column remembers the window it received.
                self.column_focus.insert(page.columns[c].id.clone(), other);
                self.remember_column_focus(&id);
                self.ensure_visible(&id, false)?;
            }
            Command::FocusColumnFirst | Command::FocusColumnLast => {
                self.require_focus()?;
                let m = active_monitor(self)?;
                let columns = &self.snapshot.monitors[m].pages[active_page(self, m)].columns;
                let column = if command == Command::FocusColumnFirst {
                    columns.first()
                } else {
                    columns.last()
                };
                if let Some(target) = column.and_then(|c| self.column_focus_target(c)) {
                    focus_hidden(self, &target)?;
                    *focus = true;
                }
            }
            Command::FocusWindowOrPage { direction } => {
                let delta = match direction {
                    Direction::Up => -1,
                    Direction::Down => 1,
                    _ => return Err(invalid("Focus moves to a page up or down")),
                };
                if let Some(actions) = try_focus_direction(self, direction)? {
                    return Ok(Some(actions));
                }
                let m = active_monitor(self)?;
                let target = active_page(self, m) as i64 + delta;
                let monitor = &self.snapshot.monitors[m];
                if let Some(page) = usize::try_from(target)
                    .ok()
                    .and_then(|p| monitor.pages.get(p))
                {
                    let command = Command::SwitchPage {
                        monitor_id: monitor.monitor.id.clone(),
                        page_id: page.id.clone(),
                    };
                    return self.dispatch_inner(command).map(Some);
                }
            }
            Command::FocusColumnOrMonitor { direction } => {
                rightward(direction)?;
                if let Some(actions) = try_focus_direction(self, direction)? {
                    return Ok(Some(actions));
                }
                return self
                    .dispatch_inner(Command::FocusMonitor { direction })
                    .map(Some);
            }
            Command::FocusMonitor { direction } => {
                let m = active_monitor(self)?;
                let Some(t) = monitor_toward(self, m, direction) else {
                    return Ok(None);
                };
                if self.monitor_suspended(t) {
                    return Ok(Some(vec![]));
                }
                let before = self.snapshot.focused_window.clone();
                self.snapshot.active_monitor = Some(self.snapshot.monitors[t].monitor.id.clone());
                self.focus_active_page();
                if self.snapshot.focused_window.is_none() {
                    // Native focus cannot follow onto an empty page; do not let it pull back.
                    self.stale_native_focus = before;
                }
                *focus = true;
            }
            Command::MoveWindowToMonitor { direction } => {
                let id = self.focused()?;
                let (m, _, _) = self.location(&id)?;
                let Some(t) = monitor_toward(self, m, direction) else {
                    return Ok(None);
                };
                if self.monitor_suspended(t) {
                    return Ok(Some(vec![]));
                }
                let page_id = self.snapshot.monitors[t].active_page.clone();
                let command = Command::MoveWindowToPage {
                    window_id: id,
                    page_id,
                };
                return self.dispatch_inner(command).map(Some);
            }
            Command::MoveColumnToMonitor { direction } => {
                let id = self.focused()?;
                let (m, p, location) = self.location(&id)?;
                let Some((c, _)) = location else {
                    return self
                        .dispatch_inner(Command::MoveWindowToMonitor { direction })
                        .map(Some);
                };
                let Some(t) = monitor_toward(self, m, direction) else {
                    return Ok(None);
                };
                if self.monitor_suspended(t) {
                    return Ok(Some(vec![]));
                }
                let tp = active_page(self, t);
                let page_id = self.snapshot.monitors[t].pages[tp].id.clone();
                let mut column = take_column(self, m, p, c);
                // A fresh identity: a parked minimized window may still restore the old one.
                let old_id = std::mem::replace(&mut column.id, self.id("column"));
                self.carry_maximized_column(&old_id, &mut column, m, t); // lane: layout-options
                for window in &column.windows {
                    self.record_hotplug_move(window, &page_id);
                }
                // niri inserts right of the target workspace's active column.
                let at = self
                    .page_focus
                    .get(&page_id)
                    .and_then(|w| self.location(w).ok())
                    .and_then(|(tm, tpp, location)| location.filter(|_| (tm, tpp) == (t, tp)))
                    .map(|(c, _)| c + 1);
                let columns = &mut self.snapshot.monitors[t].pages[tp].columns;
                columns.insert(at.unwrap_or(columns.len()), column);
                self.set_focus(&id, true)?;
                *focus = true;
            }
            Command::MovePageToMonitor { direction } => {
                let m = active_monitor(self)?;
                let p = active_page(self, m);
                let Some(t) = monitor_toward(self, m, direction) else {
                    return Ok(None);
                };
                if self.monitor_suspended(t) || empty(&self.snapshot.monitors[m].pages[p]) {
                    return Ok(Some(vec![]));
                }
                let origins = self.floating_origins();
                let source = &mut self.snapshot.monitors[m];
                let page = source.pages.remove(p);
                // A non-empty page always leaves at least the empty tail behind.
                source.active_page = source.pages[p.min(source.pages.len() - 1)].id.clone();
                // niri inserts after the target's active workspace, before the empty tail.
                let tail = usize::from(self.snapshot.monitors[t].pages.last().is_some_and(empty));
                let at =
                    (active_page(self, t) + 1).min(self.snapshot.monitors[t].pages.len() - tail);
                let page_id = page.id.clone();
                let windows: Vec<WindowId> = ids(&page).cloned().collect();
                let target = &mut self.snapshot.monitors[t];
                target.active_page = page_id.clone();
                target.pages.insert(at, page);
                self.snapshot.active_monitor = Some(target.monitor.id.clone());
                for window in &windows {
                    self.record_hotplug_move(window, &page_id);
                }
                self.rebase_floating(origins);
                self.focus_active_page();
                *focus = true;
            }
            Command::FocusWindowPrevious => {
                self.require_focus()?;
                let target = self
                    .focus_history
                    .iter()
                    .rev()
                    .find(|id| {
                        self.navigable(id)
                            && self.location(id).is_ok()
                            && !self.window_protected(id)
                    })
                    .cloned();
                if let Some(target) = target {
                    focus_hidden(self, &target)?;
                    *focus = true;
                }
            }
            Command::FocusPagePrevious => {
                let m = active_monitor(self)?;
                let monitor = &self.snapshot.monitors[m];
                if let Some(page_id) = self
                    .previous_pages
                    .get(&monitor.monitor.id)
                    .filter(|id| monitor.pages.iter().any(|p| &p.id == *id))
                    .filter(|id| **id != monitor.active_page)
                {
                    let command = Command::SwitchPage {
                        monitor_id: monitor.monitor.id.clone(),
                        page_id: page_id.clone(),
                    };
                    return self.dispatch_inner(command).map(Some);
                }
            }
            _ => unreachable!("not a layout-actions command"),
        }
        Ok(None)
    }

    /// Native focus reported to reconcile. While nothing is focused after FocusMonitor moved
    /// onto an empty page, the window left behind natively is not a new activation.
    pub(super) fn fresh_native_focus(&mut self, native: Option<WindowId>) -> Option<WindowId> {
        let stale = native.is_some() && native == self.stale_native_focus;
        if stale && self.snapshot.focused_window.is_none() {
            return None;
        }
        if native.is_some() {
            self.stale_native_focus = None;
        }
        native
    }

    /// Record the focus and page changes of one transaction, starting from before.
    pub(super) fn track_layout_history(&mut self, before: &Snapshot) {
        let focused = self.snapshot.focused_window.clone();
        if let Some(previous) = before
            .focused_window
            .as_ref()
            .filter(|id| focused.as_ref() != Some(*id))
        {
            self.focus_history.retain(|id| id != previous);
            self.focus_history.push(previous.clone());
        }
        let windows = &self.snapshot.windows;
        self.focus_history.retain(|id| {
            Some(id) != focused.as_ref() && windows.iter().any(|w| &w.native.id == id)
        });
        for old in &before.monitors {
            if let Some(now) = self
                .snapshot
                .monitors
                .iter()
                .find(|m| m.monitor.id == old.monitor.id && m.active_page != old.active_page)
            {
                self.previous_pages
                    .insert(now.monitor.id.clone(), old.active_page.clone());
            }
        }
        let monitors = &self.snapshot.monitors;
        self.previous_pages.retain(|monitor, page| {
            monitors
                .iter()
                .any(|m| &m.monitor.id == monitor && m.pages.iter().any(|p| &p.id == page))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::engine;
    use super::*;
    use crate::config::{Config, ShortcutAction};

    fn run(e: &mut Engine, command: Command) -> Transition {
        e.dispatch(command).unwrap()
    }

    fn focus(e: &mut Engine, id: &str) {
        run(
            e,
            Command::FocusWindow {
                window_id: id.into(),
            },
        );
    }

    /// Windows of each column on monitor m's active page.
    fn columns(e: &Engine, m: usize) -> Vec<Vec<&str>> {
        let monitor = &e.snapshot.monitors[m];
        monitor.pages[active_page(e, m)]
            .columns
            .iter()
            .map(|c| c.windows.iter().map(String::as_str).collect())
            .collect()
    }

    fn focused(e: &Engine) -> Option<&str> {
        e.snapshot.focused_window.as_deref()
    }

    fn active(e: &Engine) -> &str {
        e.snapshot.active_monitor.as_deref().unwrap()
    }

    fn focus_action(t: &Transition) -> Option<&str> {
        t.actions.iter().find_map(|a| match a {
            NativeAction::Focus { window_id } => Some(window_id.as_str()),
            _ => None,
        })
    }

    fn placed_x(t: &Transition, id: &str) -> i32 {
        t.actions
            .iter()
            .find_map(|a| match a {
                NativeAction::Placement {
                    window_id, rect, ..
                } if window_id == id => Some(rect.x),
                _ => None,
            })
            .unwrap()
    }

    /// Enumeration of the engine's own monitors and windows with the given native focus.
    fn native(e: &Engine, focused: &str) -> SystemSnapshot {
        SystemSnapshot {
            monitors: e
                .snapshot
                .monitors
                .iter()
                .map(|m| m.monitor.clone())
                .collect(),
            windows: e
                .snapshot
                .windows
                .iter()
                .map(|w| w.native.clone())
                .collect(),
            focused_window: Some(focused.into()),
        }
    }

    fn left() -> Direction {
        Direction::Left
    }

    fn right() -> Direction {
        Direction::Right
    }

    /// Monitor a (x 0, primary): columns 1, 2, 3 of 600 px. Monitor b (x -1200): column 4.
    /// Focus 1, then 1 joins column 2: a = [[2, 1], [3]].
    fn stacked() -> Engine {
        let mut e = engine();
        run(&mut e, Command::ConsumeOrExpelWindow { direction: right() });
        assert_eq!(columns(&e, 0), [vec!["2", "1"], vec!["3"]]);
        e
    }

    #[test]
    fn consume_or_expel_joins_a_neighbour_or_splits_a_stack() {
        let mut e = stacked();
        assert_eq!(focused(&e), Some("1"));
        run(&mut e, Command::ConsumeOrExpelWindow { direction: right() });
        assert_eq!(columns(&e, 0), [vec!["2"], vec!["1"], vec!["3"]]);
        assert_eq!(e.snapshot.monitors[0].pages[0].columns[1].width, 600);
        run(&mut e, Command::ConsumeOrExpelWindow { direction: left() });
        assert_eq!(columns(&e, 0), [vec!["2", "1"], vec!["3"]]);
        run(&mut e, Command::ConsumeOrExpelWindow { direction: left() });
        assert_eq!(columns(&e, 0), [vec!["1"], vec!["2"], vec!["3"]]);
        // Alone at the strip's left end: nothing to join.
        run(&mut e, Command::ConsumeOrExpelWindow { direction: left() });
        assert_eq!(columns(&e, 0), [vec!["1"], vec!["2"], vec!["3"]]);
        assert_eq!(focused(&e), Some("1"));
        let error = e
            .dispatch(Command::ConsumeOrExpelWindow {
                direction: Direction::Up,
            })
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidCommand);
    }

    #[test]
    fn consume_into_and_expel_from_the_focused_column() {
        let mut e = engine();
        run(&mut e, Command::ConsumeWindowIntoColumn);
        assert_eq!(columns(&e, 0), [vec!["1", "2"], vec!["3"]]);
        run(&mut e, Command::ConsumeWindowIntoColumn);
        assert_eq!(columns(&e, 0), [vec!["1", "2", "3"]]);
        run(&mut e, Command::ConsumeWindowIntoColumn);
        assert_eq!(columns(&e, 0), [vec!["1", "2", "3"]]);
        run(&mut e, Command::ExpelWindowFromColumn);
        assert_eq!(columns(&e, 0), [vec!["2", "3"], vec!["1"]]);
        run(&mut e, Command::ExpelWindowFromColumn);
        assert_eq!(columns(&e, 0), [vec!["2", "3"], vec!["1"]]);
        assert_eq!(focused(&e), Some("1"));
    }

    #[test]
    fn columns_move_by_one_or_to_either_end_and_stay_visible() {
        let mut e = engine();
        let id = e.snapshot.monitors[0].pages[0].columns[0].id.clone();
        run(&mut e, Command::MoveColumn { direction: left() });
        assert_eq!(columns(&e, 0), [vec!["1"], vec!["2"], vec!["3"]]);
        run(&mut e, Command::MoveColumn { direction: right() });
        assert_eq!(columns(&e, 0), [vec!["2"], vec!["1"], vec!["3"]]);
        let t = run(&mut e, Command::MoveColumnToLast);
        assert_eq!(columns(&e, 0), [vec!["2"], vec!["3"], vec!["1"]]);
        assert_eq!(e.snapshot.monitors[0].pages[0].viewport_x, 600);
        assert_eq!(placed_x(&t, "1"), 600);
        assert_eq!(e.snapshot.monitors[0].pages[0].columns[2].id, id);
        run(&mut e, Command::MoveColumnToFirst);
        assert_eq!(columns(&e, 0), [vec!["1"], vec!["2"], vec!["3"]]);
        assert_eq!(e.snapshot.monitors[0].pages[0].viewport_x, 0);
        assert_eq!(focused(&e), Some("1"));
    }

    #[test]
    fn swap_window_trades_places_with_the_remembered_window_and_focus_follows() {
        let mut e = stacked();
        focus(&mut e, "2");
        focus(&mut e, "3");
        run(&mut e, Command::SwapWindow { direction: left() });
        assert_eq!(columns(&e, 0), [vec!["3", "1"], vec!["2"]]);
        assert_eq!(focused(&e), Some("3"));
        // The column 3 left now remembers 2, so focusing back right lands on 2.
        run(&mut e, Command::FocusDirection { direction: right() });
        assert_eq!(focused(&e), Some("2"));
        run(&mut e, Command::SwapWindow { direction: right() });
        assert_eq!(columns(&e, 0), [vec!["3", "1"], vec!["2"]]);
    }

    #[test]
    fn layout_moves_reject_floating_windows_like_move_window() {
        let mut e = stacked();
        run(&mut e, Command::ToggleFloating);
        for command in [
            Command::ConsumeOrExpelWindow { direction: left() },
            Command::ConsumeWindowIntoColumn,
            Command::ExpelWindowFromColumn,
            Command::MoveColumn { direction: right() },
            Command::MoveColumnToFirst,
            Command::MoveColumnToLast,
            Command::SwapWindow { direction: right() },
        ] {
            let before = serde_json::to_value(e.snapshot()).unwrap();
            assert_eq!(
                e.dispatch(command).unwrap_err().code,
                ErrorCode::InvalidCommand
            );
            assert_eq!(serde_json::to_value(e.snapshot()).unwrap(), before);
        }
        // A layout fullscreen tile still has a column to move, as with MoveWindow.
        run(&mut e, Command::ToggleFloating);
        run(&mut e, Command::ToggleFullscreen);
        run(&mut e, Command::MoveColumnToFirst);
        assert_eq!(columns(&e, 0)[0], ["1"]);
    }

    #[test]
    fn focus_first_and_last_column_scroll_into_view() {
        let mut e = engine();
        let t = run(&mut e, Command::FocusColumnLast);
        assert_eq!((focused(&e), focus_action(&t)), (Some("3"), Some("3")));
        assert_eq!(e.snapshot.monitors[0].pages[0].viewport_x, 600);
        run(&mut e, Command::FocusColumnFirst);
        assert_eq!(focused(&e), Some("1"));
        assert_eq!(e.snapshot.monitors[0].pages[0].viewport_x, 0);
    }

    #[test]
    fn focus_window_or_page_crosses_to_the_neighbouring_page_at_column_ends() {
        let mut e = stacked();
        let first = e.snapshot.monitors[0].active_page.clone();
        let up = || Command::FocusWindowOrPage {
            direction: Direction::Up,
        };
        let down = || Command::FocusWindowOrPage {
            direction: Direction::Down,
        };
        run(&mut e, up());
        assert_eq!(focused(&e), Some("2"));
        run(&mut e, up());
        assert_eq!(
            (focused(&e), &e.snapshot.monitors[0].active_page),
            (Some("2"), &first)
        );
        run(&mut e, down());
        run(&mut e, down());
        assert_ne!(e.snapshot.monitors[0].active_page, first);
        assert_eq!(focused(&e), None);
        run(&mut e, up());
        assert_eq!(
            (focused(&e), &e.snapshot.monitors[0].active_page),
            (Some("1"), &first)
        );
        assert!(
            e.dispatch(Command::FocusWindowOrPage { direction: left() })
                .is_err()
        );
    }

    #[test]
    fn focus_column_or_monitor_leaves_the_strip_for_the_neighbouring_monitor() {
        let mut e = engine();
        run(&mut e, Command::FocusColumnOrMonitor { direction: right() });
        assert_eq!(focused(&e), Some("2"));
        run(&mut e, Command::FocusColumnOrMonitor { direction: left() });
        let t = run(&mut e, Command::FocusColumnOrMonitor { direction: left() });
        assert_eq!(
            (focused(&e), active(&e), focus_action(&t)),
            (Some("4"), "b", Some("4"))
        );
        run(&mut e, Command::FocusColumnOrMonitor { direction: right() });
        assert_eq!((focused(&e), active(&e)), (Some("1"), "a"));
    }

    #[test]
    fn focus_monitor_follows_geometry_and_activates_empty_pages() {
        let mut e = engine();
        run(&mut e, Command::FocusMonitor { direction: right() });
        assert_eq!(active(&e), "a");
        run(&mut e, Command::FocusMonitor { direction: left() });
        assert_eq!((focused(&e), active(&e)), (Some("4"), "b"));
        run(
            &mut e,
            Command::FocusMonitor {
                direction: Direction::Up,
            },
        );
        assert_eq!(active(&e), "b");
        // Monitor b stacked above a: only up/down reach it.
        e.snapshot.monitors[1].monitor.bounds.x = 300;
        e.snapshot.monitors[1].monitor.bounds.y = -900;
        run(
            &mut e,
            Command::FocusMonitor {
                direction: Direction::Down,
            },
        );
        assert_eq!((focused(&e), active(&e)), (Some("1"), "a"));
        run(&mut e, Command::FocusMonitor { direction: left() });
        assert_eq!(active(&e), "a");
        run(
            &mut e,
            Command::FocusMonitor {
                direction: Direction::Up,
            },
        );
        assert_eq!(active(&e), "b");
        // An empty page still becomes the active monitor; stale native focus does not undo it.
        let tail = e.snapshot.monitors[1].pages.last().unwrap().id.clone();
        run(
            &mut e,
            Command::SwitchPage {
                monitor_id: "b".into(),
                page_id: tail,
            },
        );
        focus(&mut e, "1");
        let t = run(
            &mut e,
            Command::FocusMonitor {
                direction: Direction::Up,
            },
        );
        assert_eq!(
            (focused(&e), active(&e), focus_action(&t)),
            (None, "b", None)
        );
        e.reconcile(native(&e, "1")).unwrap();
        assert_eq!((focused(&e), active(&e)), (None, "b"));
        e.reconcile(native(&e, "2")).unwrap();
        assert_eq!((focused(&e), active(&e)), (Some("2"), "a"));
    }

    #[test]
    fn columns_and_windows_move_to_the_neighbouring_monitor_with_focus() {
        let mut e = stacked();
        let id = e.snapshot.monitors[0].pages[0].columns[0].id.clone();
        let t = run(&mut e, Command::MoveColumnToMonitor { direction: left() });
        assert_eq!(columns(&e, 1), [vec!["4"], vec!["2", "1"]]);
        assert_eq!(columns(&e, 0), [vec!["3"]]);
        assert_ne!(e.snapshot.monitors[1].pages[0].columns[1].id, id);
        assert_eq!(
            (focused(&e), active(&e), focus_action(&t)),
            (Some("1"), "b", Some("1"))
        );
        // Back onto a, right of the column a last focused.
        run(&mut e, Command::MoveColumnToMonitor { direction: right() });
        assert_eq!(columns(&e, 0), [vec!["3"], vec!["2", "1"]]);
        run(&mut e, Command::MoveColumnToMonitor { direction: right() });
        assert_eq!(columns(&e, 0), [vec!["3"], vec!["2", "1"]]);
        run(&mut e, Command::MoveWindowToMonitor { direction: left() });
        assert_eq!(columns(&e, 1), [vec!["4"], vec!["1"]]);
        assert_eq!((focused(&e), active(&e)), (Some("1"), "b"));
        // A floating window moves alone, also through MoveColumnToMonitor.
        run(&mut e, Command::ToggleFloating);
        run(&mut e, Command::MoveColumnToMonitor { direction: right() });
        assert_eq!(e.location("1").unwrap(), (0, 0, None));
        assert_eq!((focused(&e), active(&e)), (Some("1"), "a"));
    }

    #[test]
    fn moved_maximized_column_keeps_its_restore_width_and_fills_the_target() {
        let mut e = engine();
        // Monitor a shows 1000 px, monitor b its whole 1200 px work area.
        let viewport = Rect {
            width: 1000,
            ..e.snapshot.monitors[0].viewport
        };
        e.set_viewports(BTreeMap::from([("a".into(), viewport)]));
        e.reconcile(native(&e, "1")).unwrap();
        let width = |e: &Engine| {
            let (m, p, column) = e.location("1").unwrap();
            e.snapshot.monitors[m].pages[p].columns[column.unwrap().0].width
        };
        run(&mut e, Command::SetColumnWidth { width: 450 });
        run(&mut e, Command::MaximizeColumn);
        assert_eq!(width(&e), 1000);
        run(&mut e, Command::MoveColumnToMonitor { direction: left() });
        assert_eq!((e.location("1").unwrap().0, width(&e)), (1, 1200));
        run(&mut e, Command::MaximizeColumn);
        assert_eq!(width(&e), 450);
        // Back onto the narrower monitor, still maximized and still restoring 450.
        run(&mut e, Command::MaximizeColumn);
        run(&mut e, Command::MoveColumnToMonitor { direction: right() });
        assert_eq!((e.location("1").unwrap().0, width(&e)), (0, 1000));
        run(&mut e, Command::MaximizeColumn);
        assert_eq!(width(&e), 450);
        // A column that is not maximized moves at its own width.
        run(&mut e, Command::MoveColumnToMonitor { direction: left() });
        assert_eq!(width(&e), 450);
        // MovePageToMonitor keeps column identities, so the restore width stays attached.
        run(&mut e, Command::MaximizeColumn);
        run(&mut e, Command::MovePageToMonitor { direction: right() });
        assert_eq!((e.location("1").unwrap().0, width(&e)), (0, 1000));
        run(&mut e, Command::MaximizeColumn);
        assert_eq!(width(&e), 450);
    }

    #[test]
    fn move_page_to_monitor_carries_the_page_and_its_focus() {
        let mut e = engine();
        let page = e.snapshot.monitors[0].active_page.clone();
        let t = run(&mut e, Command::MovePageToMonitor { direction: left() });
        let b = &e.snapshot.monitors[1];
        assert_eq!(b.pages.len(), 3);
        assert_eq!((&b.pages[1].id, &b.active_page), (&page, &page));
        assert_eq!(columns(&e, 1), [vec!["1"], vec!["2"], vec!["3"]]);
        assert_eq!((focused(&e), active(&e)), (Some("1"), "b"));
        assert_eq!(placed_x(&t, "1"), -1200);
        let a = &e.snapshot.monitors[0];
        assert!(a.pages.len() == 1 && empty(&a.pages[0]) && a.active_page == a.pages[0].id);
        // An empty page has nothing to carry.
        run(&mut e, Command::FocusMonitor { direction: right() });
        let before = serde_json::to_value(&e.snapshot.monitors).unwrap();
        run(&mut e, Command::MovePageToMonitor { direction: left() });
        assert_eq!(serde_json::to_value(&e.snapshot.monitors).unwrap(), before);
    }

    #[test]
    fn focus_window_previous_crosses_pages_and_monitors_and_forgets_closed_windows() {
        let mut e = engine();
        focus(&mut e, "3");
        run(&mut e, Command::FocusWindowPrevious);
        assert_eq!(focused(&e), Some("1"));
        run(&mut e, Command::FocusWindowPrevious);
        assert_eq!(focused(&e), Some("3"));
        focus(&mut e, "4");
        let t = run(&mut e, Command::FocusWindowPrevious);
        assert_eq!(
            (focused(&e), active(&e), focus_action(&t)),
            (Some("3"), "a", Some("3"))
        );
        // From an empty page back to the window of a hidden page.
        run(
            &mut e,
            Command::AddPage {
                monitor_id: "a".into(),
            },
        );
        assert_eq!(focused(&e), None);
        run(&mut e, Command::FocusWindowPrevious);
        assert_eq!(focused(&e), Some("3"));
        // History is now [1, 4]; once 4 closes, previous skips to 1.
        let mut system = native(&e, "3");
        system.windows.retain(|w| w.id != "4");
        e.reconcile(system).unwrap();
        assert_eq!(e.focus_history, ["1"]);
        run(&mut e, Command::FocusWindowPrevious);
        assert_eq!(focused(&e), Some("1"));
    }

    #[test]
    fn focus_page_previous_toggles_the_last_two_pages_of_the_active_monitor() {
        let mut e = engine();
        let first = e.snapshot.monitors[0].active_page.clone();
        run(&mut e, Command::FocusPagePrevious);
        assert_eq!(e.snapshot.monitors[0].active_page, first);
        run(
            &mut e,
            Command::AddPage {
                monitor_id: "a".into(),
            },
        );
        let second = e.snapshot.monitors[0].active_page.clone();
        run(&mut e, Command::FocusPagePrevious);
        assert_eq!(
            (&e.snapshot.monitors[0].active_page, focused(&e)),
            (&first, Some("1"))
        );
        run(&mut e, Command::FocusPagePrevious);
        assert_eq!(e.snapshot.monitors[0].active_page, second);
        // Monitor b keeps its own page history.
        run(&mut e, Command::FocusMonitor { direction: left() });
        run(&mut e, Command::FocusPagePrevious);
        assert_eq!(active(&e), "b");
        assert_eq!(e.snapshot.monitors[0].active_page, second);
    }

    #[test]
    fn builtin_shortcuts_and_config_parse_the_new_commands() {
        let builtin = Config::builtin_shortcuts();
        let bound = |key: &str| {
            builtin
                .iter()
                .find(|b| b.key == format!("Control+Alt+{key}"))
                .map(|b| b.action.clone())
        };
        assert_eq!(
            bound("Shift+Period"),
            Some(ShortcutAction::Command {
                command: Command::MoveColumnToMonitor { direction: right() }
            })
        );
        assert_eq!(
            bound("Backquote"),
            Some(ShortcutAction::Command {
                command: Command::FocusWindowPrevious
            })
        );
        for command in [
            Command::ConsumeOrExpelWindow { direction: left() },
            Command::ConsumeWindowIntoColumn,
            Command::ExpelWindowFromColumn,
            Command::MoveColumn { direction: right() },
            Command::MoveColumnToFirst,
            Command::MoveColumnToLast,
            Command::SwapWindow { direction: left() },
            Command::FocusColumnFirst,
            Command::FocusColumnLast,
            Command::FocusWindowOrPage {
                direction: Direction::Down,
            },
            Command::FocusColumnOrMonitor { direction: right() },
            Command::FocusMonitor {
                direction: Direction::Up,
            },
            Command::MoveColumnToMonitor { direction: left() },
            Command::MoveWindowToMonitor { direction: right() },
            Command::MovePageToMonitor { direction: left() },
            Command::FocusWindowPrevious,
            Command::FocusPagePrevious,
        ] {
            let json = serde_json::json!({"shortcuts": [{"key": "A", "action": {"type": "command", "command": command}}]});
            let config = Config::parse(&serde_json::to_vec(&json).unwrap()).unwrap();
            assert_eq!(
                config.shortcuts[0].action,
                ShortcutAction::Command { command }
            );
        }
        assert_eq!(
            serde_json::from_str::<Command>(r#"{"type":"focusMonitor","direction":"left"}"#)
                .unwrap(),
            Command::FocusMonitor { direction: left() }
        );
        assert!(Config::parse(br#"{"shortcuts":[{"key":"A","action":{"type":"command","command":{"type":"focusWindowPrevious","extra":1}}}]}"#).is_err());
    }
}
