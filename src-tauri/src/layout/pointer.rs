use super::*;

fn contains(rect: Rect, x: i64, y: i64) -> bool {
    x >= rect.x as i64
        && y >= rect.y as i64
        && x < rect.x as i64 + rect.width as i64
        && y < rect.y as i64 + rect.height as i64
}

/// Width of the strips along the left/right screen edges where a dropped window queues off
/// screen instead of joining the layout on screen.
pub fn queue_band(scale: f64, view: u32) -> i64 {
    ((48.0 * scale).round() as i64).max(i64::from(view) / 24)
}

impl Engine {
    /// Take `id` out of its column, dropping the column once empty, while the rest of the
    /// screen stays put: columns right of a removed on-screen column close the gap.
    fn detach(&mut self, id: &str) -> Result<u32, AppError> {
        let (m, p, Some((c, row))) = self.location(id)? else {
            return Err(invalid("Window has no tiled column"));
        };
        let page = &mut self.snapshot.monitors[m].pages[p];
        let width = page.columns[c].width;
        page.columns[c].windows.remove(row);
        if page.columns[c].windows.is_empty() {
            let left: i64 = page.columns[..c].iter().map(|c| i64::from(c.width)).sum();
            let x = i64::from(page.viewport_x);
            if left + i64::from(width) <= x {
                page.viewport_x = coordinate(x - i64::from(width));
            } else if left < x {
                page.viewport_x = coordinate(left);
            }
            page.columns.remove(c);
        }
        Ok(width)
    }

    /// Insert `column` just past the left or right screen edge of page `p`, leaving what is
    /// on screen in place. The columns on screen first widen to fill it if they no longer do,
    /// so the queued column really is off screen.
    pub(super) fn queue_column(&mut self, m: usize, p: usize, column: Column, right: bool) {
        let view = self.snapshot.monitors[m].viewport.width;
        let page = &mut self.snapshot.monitors[m].pages[p];
        page.columns.retain(|c| !c.windows.is_empty());
        let mut filled = widths(page);
        edges::fill(&mut filled, view);
        for (c, w) in page.columns.iter_mut().zip(&filled) {
            c.width = *w;
        }
        let mut x = edges::clamp_x(&filled, view, page.viewport_x.into());
        let (mut left, mut index) = (0i64, None);
        for (c, &w) in filled.iter().enumerate() {
            if right && left < x + i64::from(view) {
                index = Some(c + 1);
            }
            if !right && index.is_none() && left + i64::from(w) > x {
                index = Some(c);
            }
            left += i64::from(w);
        }
        if !right && !page.columns.is_empty() {
            x += i64::from(column.width);
        }
        page.columns.insert(index.unwrap_or(0), column);
        page.viewport_x = coordinate(x);
    }

    /// After a drag pushed the focused window off screen, focus the nearest window still on
    /// screen, so keyboard input never goes to a hidden window.
    pub(super) fn keep_focus_on_screen(&mut self, m: usize, p: usize) -> Result<(), AppError> {
        let Some(id) = self.snapshot.focused_window.clone() else {
            return Ok(());
        };
        let Ok((fm, fp, Some((fc, row)))) = self.location(&id) else {
            return Ok(());
        };
        if (fm, fp) != (m, p) {
            return Ok(());
        }
        let view = i64::from(self.snapshot.monitors[m].viewport.width);
        let page = &self.snapshot.monitors[m].pages[p];
        let w = widths(page);
        let x = i64::from(page.viewport_x);
        let on_screen = |c: usize| {
            let left = edges::edge_position(&w, x, c);
            left < view && left + i64::from(w[c]) > 0
        };
        if on_screen(fc) {
            return Ok(());
        }
        let mut visible = (0..w.len()).filter(|&c| on_screen(c));
        let nearest = if edges::edge_position(&w, x, fc) < 0 {
            visible.next()
        } else {
            visible.last()
        };
        let Some(c) = nearest else {
            return Ok(());
        };
        let windows = &page.columns[c].windows;
        let target = windows[row.min(windows.len() - 1)].clone();
        self.set_focus(&target, false)
    }

    /// niri-style interactive move: drop onto a column's middle to stack into it (above or
    /// below the row under the pointer), onto its outer quarters or empty space for a new
    /// column, onto the strip along a screen edge to queue it just off screen there.
    pub(super) fn drop_window(&mut self, id: &str, x: i32, y: i32) -> Result<(), AppError> {
        let w = self.window_index(id)?;
        if self.snapshot.windows[w].floating {
            return Err(invalid("Floating windows move with setFloatingRect"));
        }
        let (x, y) = (x as i64, y as i64);
        let m = self
            .snapshot
            .monitors
            .iter()
            .position(|m| contains(m.monitor.bounds, x, y))
            .ok_or_else(|| invalid("Drop point is outside every monitor"))?;
        let (old_m, old_p, location) = self.location(id)?;
        let (old_c, old_row) = location.ok_or_else(|| invalid("Window has no tiled column"))?;
        let monitor = &self.snapshot.monitors[m];
        let viewport = monitor.viewport;
        let p = monitor
            .pages
            .iter()
            .position(|p| p.id == monitor.active_page)
            .unwrap();
        let page = &monitor.pages[p];
        let band = queue_band(monitor.monitor.scale_factor, viewport.width);
        let (vx, vw) = (i64::from(viewport.x), i64::from(viewport.width));
        if x < vx + band || x >= vx + vw - band {
            if (m, p) != (old_m, old_p) {
                let page_id = page.id.clone();
                self.record_hotplug_move(id, &page_id);
            }
            let width = self.detach(id)?.min(viewport.width);
            let column = self.column(id.into(), width);
            self.queue_column(m, p, column, x >= vx + vw / 2);
            return self.keep_focus_on_screen(m, p);
        }
        // (column index, Some(row) to stack into that column, None for a new column there).
        let mut target = (page.columns.len(), None);
        let mut left = viewport.x as i64 - page.viewport_x as i64;
        for (c, column) in page.columns.iter().enumerate() {
            let right = left + column.width as i64;
            if x < left {
                target = (c, None);
                break;
            }
            if x < right {
                let quarter = column.width as i64 / 4;
                target = if x < left + quarter {
                    (c, None)
                } else if x >= right - quarter {
                    (c + 1, None)
                } else {
                    let mut top = viewport.y as i64;
                    let mut row = column.windows.len();
                    for (r, height) in self
                        .column_heights(column, viewport.height)
                        .into_iter()
                        .enumerate()
                    {
                        if y < top + height as i64 / 2 {
                            row = r;
                            break;
                        }
                        top += height as i64;
                    }
                    (c, Some(row))
                };
                break;
            }
            left = right;
        }
        let same_page = (m, p) == (old_m, old_p);
        let source = &self.snapshot.monitors[old_m].pages[old_p].columns[old_c];
        let width = source.width;
        let alone = source.windows.len() == 1;
        if same_page && target.0 == old_c && (target.1.is_some() && alone) {
            return Ok(()); // Dropped back onto its own single-window column.
        }
        let page_id = self.snapshot.monitors[m].pages[p].id.clone();
        if !same_page {
            self.record_hotplug_move(id, &page_id);
        }
        let columns = &mut self.snapshot.monitors[old_m].pages[old_p].columns;
        columns[old_c].windows.remove(old_row);
        if alone {
            columns.remove(old_c);
        }
        let (mut c, row) = target;
        if same_page && alone && old_c < c {
            c -= 1;
        }
        match row {
            Some(mut row) => {
                if same_page && target.0 == old_c && old_row < row {
                    row -= 1;
                }
                self.snapshot.monitors[m].pages[p].columns[c]
                    .windows
                    .insert(row, id.into());
            }
            None => {
                let column = self.column(id.into(), width.min(viewport.width));
                self.snapshot.monitors[m].pages[p].columns.insert(c, column);
            }
        }
        self.set_focus(id, true)
    }

    /// A tiled window the user dragged by its own title bar (the system move loop): once the
    /// loop ends, drop it where it now is, as if moved with the modifier drag. Also covers a
    /// window carried onto another monitor (its size may change with the monitor's scaling).
    /// `planned` is its last applied placement. `None` when it was not moved.
    pub fn adopt_native_move(
        &mut self,
        id: &str,
        planned: Rect,
    ) -> Result<Option<Transition>, AppError> {
        let w = self.window_index(id)?;
        let window = &self.snapshot.windows[w];
        let native = window.native.clone();
        if window.floating || window.fullscreen || native.minimized {
            return Ok(None);
        }
        let (m, _, Some(_)) = self.location(id)? else {
            return Ok(None);
        };
        let other_monitor = native.monitor_id != self.snapshot.monitors[m].monitor.id;
        let moved = (native.rect.width, native.rect.height) == (planned.width, planned.height)
            && (native.rect.x, native.rect.y) != (planned.x, planned.y);
        if !(other_monitor || moved) {
            return Ok(None);
        }
        let x = native.rect.x as i64 + native.rect.width as i64 / 2;
        let y = native.rect.y as i64 + native.rect.height as i64 / 2;
        let clamp = |v: i64| v.clamp(i32::MIN.into(), i32::MAX.into()) as i32;
        Ok(self
            .dispatch(Command::DropWindow {
                window_id: id.into(),
                x: clamp(x),
                y: clamp(y),
            })
            .ok())
    }

    pub(super) fn set_floating_rect(&mut self, id: &str, rect: Rect) -> Result<(), AppError> {
        let w = self.window_index(id)?;
        let window = &mut self.snapshot.windows[w];
        if !window.floating || window.fullscreen {
            return Err(invalid(
                "Only floating, non-fullscreen windows take a free rectangle",
            ));
        }
        if rect.width == 0 || rect.height == 0 {
            return Err(invalid("Floating rectangle must be nonempty"));
        }
        window.native.rect = rect;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::engine;
    use super::*;

    fn drop(e: &mut Engine, id: &str, x: i32, y: i32) {
        e.dispatch(Command::DropWindow {
            window_id: id.into(),
            x,
            y,
        })
        .unwrap();
    }

    fn layout(e: &Engine, m: usize) -> Vec<Vec<String>> {
        let monitor = &e.snapshot.monitors[m];
        let page = monitor
            .pages
            .iter()
            .find(|p| p.id == monitor.active_page)
            .unwrap();
        page.columns.iter().map(|c| c.windows.clone()).collect()
    }

    #[test]
    fn drops_stack_split_and_cross_monitors() {
        // Monitor a (x 0..1200): columns 1, 2, 3 of 600 px; viewport starts at column 1.
        let mut e = engine();
        drop(&mut e, "1", 900, 450); // Middle of column 2: stack below or above by y.
        assert_eq!(layout(&e, 0), [vec!["2", "1"], vec!["3"]]);
        assert_eq!(e.snapshot.focused_window.as_deref(), Some("1"));
        let x = |e: &Engine, c: usize| {
            let page = &e.snapshot.monitors[0].pages[0];
            -page.viewport_x
                + page.columns[..c]
                    .iter()
                    .map(|c| c.width as i32)
                    .sum::<i32>()
        };
        let top = x(&e, 0) + 300;
        drop(&mut e, "1", top, 10); // Top half of row 0 in the same column.
        assert_eq!(layout(&e, 0), [vec!["1", "2"], vec!["3"]]);
        let edge = x(&e, 1) + 530;
        drop(&mut e, "1", edge, 10); // Right quarter of column 3: new column after it.
        assert_eq!(layout(&e, 0), [vec!["2"], vec!["3"], vec!["1"]]);
        let own = x(&e, 2) + 300;
        let before = layout(&e, 0);
        drop(&mut e, "1", own, 10); // Onto itself: unchanged.
        assert_eq!(layout(&e, 0), before);
        drop(&mut e, "3", -900, 450); // Monitor b holds window 4.
        assert_eq!(layout(&e, 1), [vec!["4", "3"]]);
        assert_eq!(e.snapshot.active_monitor.as_deref(), Some("b"));
        let error = e
            .dispatch(Command::DropWindow {
                window_id: "3".into(),
                x: 99_999,
                y: 0,
            })
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidCommand);
    }

    #[test]
    fn title_bar_drag_joins_the_layout_where_dropped_even_across_monitors() {
        let mut e = engine();
        let plan = |e: &Engine, id: &str| {
            e.placements()
                .unwrap()
                .into_iter()
                .find_map(|a| match a {
                    NativeAction::Placement {
                        window_id, rect, ..
                    } if window_id == id => Some(rect),
                    _ => None,
                })
                .unwrap()
        };
        let observe = |e: &mut Engine, id: &str, monitor: &str, rect: Rect| {
            let mut system = SystemSnapshot {
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
                focused_window: e.snapshot.focused_window.clone(),
            };
            let w = system.windows.iter_mut().find(|w| w.id == id).unwrap();
            (w.monitor_id, w.rect) = (monitor.into(), rect);
            e.reconcile(system).unwrap();
        };
        // Window 4 lives alone on monitor b (x < 0); it is dragged onto monitor a.
        let planned = plan(&e, "4");
        let dropped = Rect {
            x: 100,
            y: 50,
            ..planned
        };
        observe(&mut e, "4", "a", dropped);
        assert!(e.adopt_native_move("4", planned).unwrap().is_some());
        assert_eq!(e.location("4").unwrap().0, 0);
        assert_eq!(e.snapshot.focused_window.as_deref(), Some("4"));
        // A window left where it was placed is not a move.
        let planned = plan(&e, "1");
        observe(&mut e, "1", "a", planned);
        assert!(e.adopt_native_move("1", planned).unwrap().is_none());
    }

    #[test]
    fn floating_rect_only_moves_floating_windows() {
        let mut e = engine();
        let rect = Rect {
            x: 10,
            y: 20,
            width: 300,
            height: 200,
        };
        let command = Command::SetFloatingRect {
            window_id: "1".into(),
            rect,
        };
        assert!(e.dispatch(command.clone()).is_err());
        e.dispatch(Command::ToggleFloating).unwrap();
        let t = e.dispatch(command).unwrap();
        assert!(t.actions.iter().any(|a| matches!(a,
            NativeAction::Placement { window_id, rect: r, .. } if window_id == "1" && *r == rect)));
    }
}
