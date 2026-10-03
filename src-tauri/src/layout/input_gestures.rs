//! Touchpad swipe commands (lane: input-gestures): the view follows the fingers, then settles
//! on a column like niri's `view_offset_gesture_end`.
use super::*;

/// The scroll a released swipe settles on, and the column it focuses. Every column offers
/// its left-aligned scroll and, when narrower than the view, its right-aligned one; the one
/// nearest `target` wins. Then, as in niri, focus moves on to the furthest fully visible
/// column in the swipe direction (`target` against the scroll `from` at release).
pub(super) fn snap_target(
    widths: &[u32],
    view: u32,
    from: i64,
    target: i64,
) -> Option<(usize, i64)> {
    let view = i64::from(view);
    let mut left = 0;
    let mut best: Option<(usize, i64)> = None;
    for (c, &width) in widths.iter().enumerate() {
        let width = i64::from(width);
        let right = (width < view).then_some(left + width - view);
        for x in [Some(left), right].into_iter().flatten() {
            let x = edges::clamp_x(widths, view as u32, x);
            if best.is_none_or(|(_, b)| (x - target).abs() < (b - target).abs()) {
                best = Some((c, x));
            }
        }
        left += width;
    }
    let (mut column, x) = best?;
    let visible = |c: usize| {
        let left = edges::edge_position(widths, x, c);
        left >= 0 && left + i64::from(widths[c]) <= view
    };
    if target >= from {
        while column + 1 < widths.len() && visible(column + 1) {
            column += 1;
        }
    } else {
        while column > 0 && visible(column - 1) {
            column -= 1;
        }
    }
    Some((column, x))
}

impl Engine {
    fn gesture_page(&self, monitor_id: &str) -> Result<Option<(usize, usize)>, AppError> {
        let m = self.monitor_index(monitor_id)?;
        if self.monitor_suspended(m) {
            return Ok(None);
        }
        let monitor = &self.snapshot.monitors[m];
        let p = monitor
            .pages
            .iter()
            .position(|p| p.id == monitor.active_page)
            .unwrap();
        Ok(Some((m, p)))
    }

    /// Follow the fingers within the relaxed scroll range; no snapping, focus stays put.
    pub(super) fn drag_viewport(&mut self, monitor_id: &str, delta: i32) -> Result<(), AppError> {
        let Some((m, p)) = self.gesture_page(monitor_id)? else {
            return Ok(());
        };
        let view = self.snapshot.monitors[m].viewport.width;
        let page = &mut self.snapshot.monitors[m].pages[p];
        page.viewport_x =
            clamp_scroll_relaxed(page, view, i64::from(page.viewport_x) + i64::from(delta));
        Ok(())
    }

    /// Settle the view after a swipe; true when a column window took focus.
    pub(super) fn snap_viewport(&mut self, monitor_id: &str, delta: i32) -> Result<bool, AppError> {
        let Some((m, p)) = self.gesture_page(monitor_id)? else {
            return Ok(false);
        };
        let view = self.snapshot.monitors[m].viewport.width;
        let page = &self.snapshot.monitors[m].pages[p];
        let from = i64::from(page.viewport_x);
        let Some((c, x)) = snap_target(&widths(page), view, from, from + i64::from(delta)) else {
            return Ok(false);
        };
        let id = self.column_focus_target(&page.columns[c]);
        self.snapshot.monitors[m].pages[p].viewport_x = coordinate(x);
        let Some(id) = id else {
            return Ok(false);
        };
        let w = self.window_index(&id)?;
        // Manager-hidden columns come back on screen. User-minimized windows are not targets.
        self.snapshot.windows[w].native.minimized = false;
        self.set_focus(&id, false)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swipe_release_snaps_to_the_nearest_column_edge_then_the_furthest_visible_column() {
        let widths = [600, 600, 600, 600];
        // Released between column 1 and 2 aligned left: nearest stop, moving right.
        assert_eq!(snap_target(&widths, 1200, 500, 650), Some((2, 600)));
        // Moving left from the same place focuses the leftmost fully visible column.
        assert_eq!(snap_target(&widths, 1200, 700, 650), Some((1, 600)));
        // A fling past the end clamps to the last stop.
        assert_eq!(snap_target(&widths, 1200, 1100, 9000), Some((3, 1200)));
        assert_eq!(snap_target(&widths, 1200, 100, -9000), Some((0, 0)));
        // Wide columns only offer their left edge.
        assert_eq!(snap_target(&[1500, 400], 1200, 0, 250), Some((0, 0)));
        assert_eq!(snap_target(&[1500, 400], 1200, 0, 600), Some((1, 700)));
        assert_eq!(snap_target(&[], 1200, 0, 50), None);
    }

    fn widened() -> Engine {
        let mut e = crate::layout::tests::engine();
        for column in &mut e.snapshot.monitors[0].pages[0].columns {
            column.width = 600;
        }
        e
    }

    #[test]
    fn drag_viewport_follows_without_focus_and_snap_focuses_the_landing_column() {
        let mut e = widened();
        assert_eq!(e.snapshot.monitors[0].pages[0].columns.len(), 3);
        let focused = e.snapshot.focused_window.clone();
        let t = e
            .dispatch(Command::DragViewport {
                monitor_id: "a".into(),
                delta: 250,
            })
            .unwrap();
        assert_eq!(e.snapshot.monitors[0].pages[0].viewport_x, 250);
        assert_eq!(e.snapshot.focused_window, focused);
        assert!(
            !t.actions
                .iter()
                .any(|a| matches!(a, NativeAction::Focus { .. }))
        );
        // Relaxed limits: one end column stays on screen.
        e.dispatch(Command::DragViewport {
            monitor_id: "a".into(),
            delta: 99_999,
        })
        .unwrap();
        assert_eq!(e.snapshot.monitors[0].pages[0].viewport_x, 1200);
        e.dispatch(Command::DragViewport {
            monitor_id: "a".into(),
            delta: -1150,
        })
        .unwrap();
        let t = e
            .dispatch(Command::SnapViewport {
                monitor_id: "a".into(),
                delta: 300,
            })
            .unwrap();
        let page = &e.snapshot.monitors[0].pages[0];
        assert_eq!(page.viewport_x, 600);
        let landing = page.columns[2].windows[0].clone();
        assert_eq!(e.snapshot.focused_window.as_ref(), Some(&landing));
        assert!(
            t.actions
                .iter()
                .any(|a| matches!(a, NativeAction::Focus { window_id } if *window_id == landing))
        );
    }

    #[test]
    fn swipes_leave_a_suspended_monitor_alone() {
        let mut e = widened();
        e.set_suspended_monitors(vec!["a".into()]);
        let focused = e.snapshot.focused_window.clone();
        for command in [
            Command::DragViewport {
                monitor_id: "a".into(),
                delta: 300,
            },
            Command::SnapViewport {
                monitor_id: "a".into(),
                delta: 900,
            },
        ] {
            e.dispatch(command).unwrap();
        }
        assert_eq!(e.snapshot.monitors[0].pages[0].viewport_x, 0);
        assert_eq!(e.snapshot.focused_window, focused);
        assert!(
            e.dispatch(Command::DragViewport {
                monitor_id: "missing".into(),
                delta: 1,
            })
            .is_err()
        );
    }
}
