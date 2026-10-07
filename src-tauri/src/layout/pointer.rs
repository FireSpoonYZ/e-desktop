use super::*;

fn contains(rect: Rect, x: i64, y: i64) -> bool {
    x >= rect.x as i64
        && y >= rect.y as i64
        && x < rect.x as i64 + rect.width as i64
        && y < rect.y as i64 + rect.height as i64
}

/// Width of the strips along the left/right screen edges where a drop scrolls that
/// column fully on screen (flush left or flush right) instead of inserting under the pointer.
pub fn queue_band(scale: f64, view: u32) -> i64 {
    ((48.0 * scale).round() as i64).max(i64::from(view) / 24)
}

/// Height of the strip along a monitor's top edge where a drop makes a full-width column.
pub fn top_band(scale: f64) -> i64 {
    (64.0 * scale).round() as i64
}

impl Engine {
    /// Take `id` out of its column, dropping the column once empty, while the rest of the
    /// screen stays put: columns right of a removed on-screen column close the gap.
    pub(super) fn detach(&mut self, id: &str) -> Result<u32, AppError> {
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

    /// Insert `column` just past the left or right screen edge of page `p`. Widths stay as
    /// they are. When the other columns already cover the screen, they stay put; when they
    /// do not, the view scrolls so the queued column is fully off screen, which can shift
    /// those windows together.
    pub(super) fn queue_column(&mut self, m: usize, p: usize, column: Column, right: bool) {
        let view = i64::from(self.snapshot.monitors[m].viewport.width);
        let page = &mut self.snapshot.monitors[m].pages[p];
        page.columns.retain(|c| !c.windows.is_empty());
        let mut x = i64::from(page.viewport_x);
        let mut index = if right { page.columns.len() } else { 0 };
        let mut left = 0i64;
        let mut found_left = false;
        for (c, existing) in page.columns.iter().enumerate() {
            let w = i64::from(existing.width);
            if right && left < x + view {
                index = c + 1;
            }
            if !right && !found_left && left + w > x {
                index = c;
                found_left = true;
            }
            left += w;
        }
        let width = i64::from(column.width);
        if !right && !page.columns.is_empty() {
            x += width;
        }
        page.columns.insert(index, column);
        let col_left: i64 = page.columns[..index]
            .iter()
            .map(|c| i64::from(c.width))
            .sum();
        if page.columns.len() > 1 {
            if right {
                let park = col_left - view;
                if x > park {
                    x = park;
                }
            } else if x < col_left + width {
                x = col_left + width;
            }
        }
        page.viewport_x = coordinate(x);
    }

    /// After a drag pushed the focused window off screen, focus the nearest window still on
    /// screen, so keyboard input never goes to a hidden window.
    pub(super) fn keep_focus_on_screen(&mut self, m: usize, p: usize) -> Result<(), AppError> {
        let Some(id) = self.snapshot.focused_window.clone() else {
            return Ok(());
        };
        let Ok((fm, fp, Some((fc, _)))) = self.location(&id) else {
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
        if let Some(target) = self.column_focus_target(&page.columns[c]) {
            self.set_focus(&target, false)?;
        }
        Ok(())
    }

    /// niri-style interactive move: drop onto a column's middle to stack into it (above or
    /// below the row under the pointer), onto its outer quarters or empty space for a new
    /// column, onto the strip along a screen edge to place that column fully on screen there.
    pub(super) fn drop_window(
        &mut self,
        id: &str,
        x: i32,
        y: i32,
        page_id: Option<&str>,
        viewport_x: Option<i32>,
    ) -> Result<(), AppError> {
        if viewport_x.is_some() && page_id.is_none() {
            return Err(invalid("Drop viewport requires an explicit target page"));
        }
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
        let p = if let Some(page_id) = page_id {
            let (target_m, p) = self.page_index(page_id)?;
            if target_m != m {
                return Err(invalid("Drop point is outside the target page's monitor"));
            }
            p
        } else {
            monitor
                .pages
                .iter()
                .position(|p| p.id == monitor.active_page)
                .unwrap()
        };
        let page = &monitor.pages[p];
        if viewport_x.is_some() && !contains(viewport, x, y) {
            return Err(invalid(
                "Overview drop point is outside the target viewport",
            ));
        }
        let source_view = self.snapshot.monitors[old_m].viewport.width;
        let source_width = self.snapshot.monitors[old_m].pages[old_p].columns[old_c].width;
        // Keep the screen share on another display; physical pixels lose it across different sizes.
        let width = ((u64::from(source_width) * u64::from(viewport.width)
            + u64::from(source_view) / 2)
            / u64::from(source_view))
        .clamp(1, u64::from(viewport.width)) as u32;
        let band = queue_band(monitor.monitor.scale_factor, viewport.width);
        let (vx, vw) = (i64::from(viewport.x), i64::from(viewport.width));
        if viewport_x.is_none() && (x < vx + band || x >= vx + vw - band) {
            if (m, p) != (old_m, old_p) {
                let page_id = page.id.clone();
                self.record_hotplug_move(id, &page_id);
            }
            self.detach(id)?;
            let column = self.column(id.into(), width);
            let column_id = column.id.clone();
            let right = x >= vx + vw / 2;
            // Park geometry is what `ensure_visible` already un-hides: a right park at
            // `col_left - view` scrolls to the column's right edge, a left park at
            // `col_left + width` scrolls to its left edge. Row-squeeze still parks.
            self.queue_column(m, p, column, right);
            let columns = &self.snapshot.monitors[m].pages[p].columns;
            let c = columns.iter().position(|c| c.id == column_id).unwrap();
            let pair = if right { c.checked_sub(1) } else { Some(c + 1) };
            self.fit_dropped_column(m, p, c, pair);
            return self.finish_drop_focus(id, m, p);
        }
        // Top-centre drop creates a standalone full-width column, not layout fullscreen.
        // Side queue bands take precedence at the corners. Use screen coordinates so a pinned
        // bar does not make the top edge unreachable; the resulting tile uses the usable viewport.
        let top = i64::from(monitor.monitor.bounds.y);
        let top_band = top_band(monitor.monitor.scale_factor);
        if viewport_x.is_none()
            && y >= top
            && y < (top + top_band).min(i64::from(viewport.y) + i64::from(viewport.height))
        {
            let page_id = page.id.clone();
            if (m, p) != (old_m, old_p) {
                self.record_hotplug_move(id, &page_id);
            }
            self.detach(id)?;
            let columns = &self.snapshot.monitors[m].pages[p].columns;
            let mut at = columns.len();
            let mut left =
                i64::from(viewport.x) - i64::from(self.snapshot.monitors[m].pages[p].viewport_x);
            for (c, column) in columns.iter().enumerate() {
                if x < left + i64::from(column.width) {
                    at = c;
                    break;
                }
                left += i64::from(column.width);
            }
            let column = self.column(id.into(), viewport.width);
            self.snapshot.monitors[m].pages[p]
                .columns
                .insert(at, column);
            self.snapshot.windows[w].fullscreen = false;
            self.fullscreen_restore.remove(id);
            return self.finish_drop_focus(id, m, p);
        }
        // (column index, Some(row) to stack into that column, None for a new column there).
        let mut target = (page.columns.len(), None);
        // A new column pairs with the column the pointer is over (or the last one): `after` it
        // for its right half or the empty space past the end.
        let mut after = true;
        let mut left = i64::from(viewport.x) - i64::from(viewport_x.unwrap_or(page.viewport_x));
        for (c, column) in page.columns.iter().enumerate() {
            let right = left + column.width as i64;
            if x < left {
                target = (c, None);
                after = false;
                break;
            }
            if x < right {
                let quarter = column.width as i64 / 4;
                after = x >= left + column.width as i64 / 2;
                target = if x < left + quarter {
                    (c, None)
                } else if x >= right - quarter {
                    (c + 1, None)
                } else if let Some(shown) = super::tabbed::shown_tab(column) {
                    // lane: tabbed. A new tab goes right after the shown one.
                    let at = column.windows.iter().position(|id| id == shown).unwrap();
                    (c, Some(at + 1))
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
                // Joining another stack: every row must keep its window's minimum height, else
                // the window becomes a column beside it and both keep the full height.
                let joined = !(same_page && target.0 == old_c);
                let column = &self.snapshot.monitors[m].pages[p].columns[c];
                if joined
                    && super::tabbed::shown_tab(column).is_none()
                    && !self.rows_fit(m, column)
                {
                    self.snapshot.monitors[m].pages[p].columns[c].windows.remove(row);
                    let at = if after { c + 1 } else { c };
                    let column = self.column(id.into(), width);
                    self.snapshot.monitors[m].pages[p].columns.insert(at, column);
                    let pair = if after { at - 1 } else { at + 1 };
                    self.fit_dropped_column(m, p, at, Some(pair));
                } else if joined {
                    self.fill_empty_width(m, p, c);
                }
            }
            None => {
                let column = self.column(id.into(), width);
                self.snapshot.monitors[m].pages[p].columns.insert(c, column);
                let pair = if after { c.checked_sub(1) } else { Some(c + 1) };
                self.fit_dropped_column(m, p, c, pair);
            }
        }
        self.finish_drop_focus(id, m, p)
    }

    /// A window stacked into column `c` can leave screen width no column covers (it came
    /// from the neighbouring column): the column grows to cover it.
    fn fill_empty_width(&mut self, m: usize, p: usize, c: usize) {
        let view = u64::from(self.snapshot.monitors[m].viewport.width);
        let columns = &mut self.snapshot.monitors[m].pages[p].columns;
        let total: u64 = columns.iter().map(|column| u64::from(column.width)).sum();
        if total < view {
            columns[c].width += (view - total) as u32;
        }
    }

    /// Every row of `column` on monitor `m` is at least as tall as its window's minimum.
    fn rows_fit(&self, m: usize, column: &Column) -> bool {
        let total = self.snapshot.monitors[m].viewport.height;
        self.column_heights(column, total)
            .iter()
            .zip(&column.windows)
            .all(|(&height, id)| height >= self.native_min_height(m, id))
    }

    /// Width of the new column `c` a drop made: a window that cannot be half as wide as the
    /// screen only gets the full width. Beside a full-width column `pair`, both share the
    /// screen half and half when both can, otherwise both stay full width.
    fn fit_dropped_column(&mut self, m: usize, p: usize, c: usize, pair: Option<usize>) {
        let view = self.snapshot.monitors[m].viewport.width;
        let half = view / 2;
        let columns = &self.snapshot.monitors[m].pages[p].columns;
        let fits_half = |column: &Column| self.settled_column_width(m, column, half) <= half;
        let pair = pair.filter(|&i| columns.get(i).is_some_and(|column| column.width >= view));
        let width = if !fits_half(&columns[c]) {
            view
        } else if let Some(i) = pair {
            if fits_half(&columns[i]) { half } else { view }
        } else {
            return;
        };
        let columns = &mut self.snapshot.monitors[m].pages[p].columns;
        columns[c].width = width;
        if let Some(i) = pair.filter(|_| width == half) {
            columns[i].width = half;
        }
    }

    fn finish_drop_focus(&mut self, id: &str, m: usize, p: usize) -> Result<(), AppError> {
        let page_id = self.snapshot.monitors[m].pages[p].id.clone();
        if self.snapshot.monitors[m].active_page == page_id {
            return self.set_focus(id, true);
        }
        self.ensure_visible(id, false)?;
        self.page_focus.insert(page_id, id.into());
        self.remember_column_focus(id);
        if self.snapshot.focused_window.as_deref() == Some(id) {
            self.focus_active_page();
        }
        Ok(())
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
                page_id: None,
                viewport_x: None,
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
            page_id: None,
            viewport_x: None,
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
        drop(&mut e, "1", top, 100); // Top half of row 0 in the same column.
        assert_eq!(layout(&e, 0), [vec!["1", "2"], vec!["3"]]);
        let edge = x(&e, 1) + 530;
        drop(&mut e, "1", edge, 100); // Right quarter of column 3: new column after it.
        assert_eq!(layout(&e, 0), [vec!["2"], vec!["3"], vec!["1"]]);
        let own = x(&e, 2) + 300;
        let before = layout(&e, 0);
        drop(&mut e, "1", own, 100); // Onto itself: unchanged.
        assert_eq!(layout(&e, 0), before);
        drop(&mut e, "3", -900, 450); // Monitor b holds window 4.
        assert_eq!(layout(&e, 1), [vec!["4", "3"]]);
        assert_eq!(e.snapshot.active_monitor.as_deref(), Some("b"));
        let error = e
            .dispatch(Command::DropWindow {
                page_id: None,
                viewport_x: None,
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
    fn top_drop_detaches_one_window_and_preview_matches_full_width_result() {
        let mut e = engine();
        drop(&mut e, "1", 900, 450);
        assert_eq!(layout(&e, 0)[0], ["2", "1"]);
        let command = Command::DropWindow {
            page_id: None,
            viewport_x: None,
            window_id: "1".into(),
            x: 600,
            y: 30,
        };
        let scene = e.scene(command.clone(), "a", &["1".into()]).unwrap();
        let preview = scene.tiles.iter().find(|t| t.title == "1").unwrap();
        assert_eq!(preview.label, "100%");
        assert_eq!(preview.rect.width, 1200);
        let t = e.dispatch(command).unwrap();
        let (m, p, Some((c, _))) = e.location("1").unwrap() else {
            panic!("tiled");
        };
        let column = &e.snapshot.monitors[m].pages[p].columns[c];
        assert_eq!(column.width, 1200);
        assert_eq!(column.windows, ["1"]);
        assert!(!e.snapshot.windows[e.window_index("1").unwrap()].fullscreen);
        assert!(t.actions.iter().any(|a| matches!(a,
            NativeAction::Placement { window_id, rect, minimized: false, .. }
            if window_id == "1" && rect.x == 0 && rect.width == 1200 && rect.height == 900)));
        // Top corners still queue on the side rather than taking over the screen.
        let mut e = engine();
        drop(&mut e, "1", 1190, 10);
        let (m, p, Some((c, _))) = e.location("1").unwrap() else {
            panic!("tiled");
        };
        assert_eq!(e.snapshot.monitors[m].pages[p].columns[c].width, 600);
    }

    #[test]
    fn cross_screen_independent_columns_keep_their_share_and_top_uses_target_dpi() {
        let mut e = engine();
        let mut native = SystemSnapshot {
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
                .filter(|w| w.native.id != "4")
                .map(|w| w.native.clone())
                .collect(),
            focused_window: Some("1".into()),
        };
        native.monitors[1].bounds.width = 600;
        native.monitors[1].work_area.width = 600;
        native.monitors[1].scale_factor = 1.5;
        e.reconcile(native).unwrap();
        drop(&mut e, "1", -900, 450);
        let (m, p, Some((c, _))) = e.location("1").unwrap() else {
            panic!("tiled");
        };
        assert_eq!(e.snapshot.monitors[m].pages[p].columns[c].width, 300);
        drop(&mut e, "1", 80, 450); // Outer quarter creates an independent column.
        let (m, p, Some((c, _))) = e.location("1").unwrap() else {
            panic!("tiled");
        };
        assert_eq!(e.snapshot.monitors[m].pages[p].columns[c].width, 600);
        drop(&mut e, "1", -900, 80); // 64 logical px = 96 physical px on this display.
        let (m, p, Some((c, _))) = e.location("1").unwrap() else {
            panic!("tiled");
        };
        assert_eq!(m, 1);
        assert_eq!(e.snapshot.monitors[m].pages[p].columns[c].width, 600);
    }

    #[test]
    fn drop_beside_a_full_width_column_splits_in_half_or_keeps_both_full() {
        let widths = |e: &Engine| -> Vec<(String, u32)> {
            let monitor = &e.snapshot.monitors[1];
            let page = monitor.pages.iter().find(|p| p.id == monitor.active_page).unwrap();
            page.columns.iter().map(|c| (c.windows[0].clone(), c.width)).collect()
        };
        let split = [("4".to_string(), 600), ("1".to_string(), 600)];
        let full = [("4".to_string(), 1200), ("1".to_string(), 1200)];
        // Monitor b (x -1200..0) holds window 4 alone at full width; window 1 is 50% on a.
        let setup = |mins: &[(&str, u32)]| {
            let mut e = engine();
            e.dispatch(Command::SetWindowColumnWidth {
                window_id: "4".into(),
                width: 1200,
            })
            .unwrap();
            e.set_min_widths(mins.iter().map(|(id, w)| ((*id).into(), *w)).collect());
            e
        };
        // Right quarter of the full-width column, and the strip along the right screen edge.
        for x in [-200, -10] {
            let mut e = setup(&[]);
            let command = Command::DropWindow {
                page_id: None,
                viewport_x: None,
                window_id: "1".into(),
                x,
                y: 450,
            };
            let scene = e.scene(command.clone(), "b", &["1".into()]).unwrap();
            let tiles: Vec<_> = scene
                .tiles
                .iter()
                .map(|t| (t.title.as_str(), t.rect.x, t.label.as_str()))
                .collect();
            assert_eq!(tiles, [("4", 0, "50%"), ("1", 600, "50%")], "x {x}");
            e.dispatch(command).unwrap();
            assert_eq!(widths(&e), split, "x {x}");
        }
        // Either window cannot be half as wide: both stay full width.
        for id in ["1", "4"] {
            let mut e = setup(&[(id, 700)]);
            drop(&mut e, "1", -200, 450);
            assert_eq!(widths(&e), full, "minimum on {id}");
        }
        // A minimum measured on a 150% monitor converts: 900 px there is 600 px here.
        for (min, expected) in [(900, split), (960, full)] {
            let mut e = setup(&[("1", min)]);
            let mut native = SystemSnapshot {
                monitors: e.snapshot.monitors.iter().map(|m| m.monitor.clone()).collect(),
                windows: e.snapshot.windows.iter().map(|w| w.native.clone()).collect(),
                focused_window: e.snapshot.focused_window.clone(),
            };
            native.monitors[0].scale_factor = 1.5;
            e.reconcile(native).unwrap();
            drop(&mut e, "1", -200, 450);
            assert_eq!(widths(&e), expected, "minimum {min}");
        }
    }

    #[test]
    fn stacking_needs_every_row_to_fit_its_minimum_height() {
        let columns = |e: &Engine| -> Vec<(Vec<String>, u32)> {
            let monitor = &e.snapshot.monitors[1];
            let page = monitor.pages.iter().find(|p| p.id == monitor.active_page).unwrap();
            page.columns.iter().map(|c| (c.windows.clone(), c.width)).collect()
        };
        let ids = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        // Monitor b (900 px tall) holds window 4 alone at full width.
        let setup = |heights: &[(&str, u32)]| {
            let mut e = engine();
            e.dispatch(Command::SetWindowColumnWidth {
                window_id: "4".into(),
                width: 1200,
            })
            .unwrap();
            e.set_min_heights(heights.iter().map(|(id, h)| ((*id).into(), *h)).collect());
            e
        };
        // Lower half of the column's middle: stacks below when both fit half the height.
        let mut e = setup(&[("1", 450), ("4", 450)]);
        drop(&mut e, "1", -500, 600);
        assert_eq!(columns(&e), [(ids(&["4", "1"]), 1200)]);
        // Either window too tall for its row: a column beside it, on the pointer's side.
        for (id, x, expected) in [("1", -500, ["4", "1"]), ("4", -700, ["1", "4"])] {
            let mut e = setup(&[(id, 451)]);
            let command = Command::DropWindow {
                page_id: None,
                viewport_x: None,
                window_id: "1".into(),
                x,
                y: 600,
            };
            let scene = e.scene(command.clone(), "b", &["1".into()]).unwrap();
            let labels: Vec<_> = scene.tiles.iter().map(|t| t.label.as_str()).collect();
            assert_eq!(labels, ["50%", "50%"], "minimum on {id}");
            e.dispatch(command).unwrap();
            assert_eq!(
                columns(&e),
                [(ids(&[expected[0]]), 600), (ids(&[expected[1]]), 600)],
                "minimum on {id}"
            );
        }
    }

    #[test]
    fn stacking_the_last_neighbour_widens_the_column_to_the_freed_screen() {
        // Monitor a (x 0..1200): columns 1, 2, 3 of 600 px; 1 and 2 on screen.
        let mut e = engine();
        let columns = |e: &Engine| -> Vec<(Vec<String>, u32)> {
            let page = &e.snapshot.monitors[0].pages[0];
            page.columns.iter().map(|c| (c.windows.clone(), c.width)).collect()
        };
        let ids = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        drop(&mut e, "3", 900, 600); // Column 1 still covers the other half: widths stay.
        assert_eq!(columns(&e), [(ids(&["1"]), 600), (ids(&["2", "3"]), 600)]);
        let command = Command::DropWindow {
            page_id: None,
            viewport_x: None,
            window_id: "1".into(),
            x: 900,
            y: 100,
        };
        let scene = e.scene(command.clone(), "a", &["1".into()]).unwrap();
        assert!(scene.tiles.iter().all(|t| t.rect.width == 1200 && t.label.starts_with("100% ×")));
        e.dispatch(command).unwrap();
        assert_eq!(columns(&e), [(ids(&["1", "2", "3"]), 1200)]);
        assert_eq!(e.snapshot.monitors[0].pages[0].viewport_x, 0);
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
