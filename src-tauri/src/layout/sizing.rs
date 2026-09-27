use super::*;

/// Reserve one pixel per window, then apportion the rest with cumulative rounding.
/// u128 keeps even u32::MAX viewports/weights safe; the last boundary is exactly total.
fn distribute(total: u32, weights: &[u32]) -> Vec<u32> {
    if weights.is_empty() {
        return vec![];
    }
    let extra = total as u128 - weights.len() as u128;
    let sum: u128 = weights.iter().map(|&w| w as u128).sum();
    let divisor = if sum == 0 { weights.len() as u128 } else { sum };
    let mut cumulative = 0;
    let mut previous = 0;
    weights
        .iter()
        .map(|&weight| {
            cumulative += if sum == 0 { 1 } else { weight as u128 };
            let boundary = extra * cumulative / divisor;
            let height = (boundary - previous + 1) as u32;
            previous = boundary;
            height
        })
        .collect()
}

impl Engine {
    pub(super) fn sizing_target(&self) -> Result<(WindowId, usize, usize, usize, usize), AppError> {
        self.window_sizing_target(&self.focused()?)
    }

    fn window_sizing_target(
        &self,
        id: &str,
    ) -> Result<(WindowId, usize, usize, usize, usize), AppError> {
        let window = &self.snapshot.windows[self.window_index(id)?];
        if window.floating || window.fullscreen {
            return Err(invalid(
                "Sizing requires a tiled window outside layout fullscreen",
            ));
        }
        let (m, p, column) = self.location(id)?;
        let (c, row) = column.ok_or_else(|| invalid("Window has no tiled column"))?;
        Ok((id.into(), m, p, c, row))
    }

    pub(super) fn resize_column(&mut self, value: i64, relative: bool) -> Result<(), AppError> {
        let id = self.focused()?;
        self.resize_window_column(&id, value, relative)?;
        self.ensure_visible(&id, false)
    }

    pub(super) fn resize_window_column(
        &mut self,
        id: &str,
        value: i64,
        relative: bool,
    ) -> Result<(), AppError> {
        let (_, m, p, c, _) = self.window_sizing_target(id)?;
        let viewport = self.snapshot.monitors[m].viewport.width;
        let width = &mut self.snapshot.monitors[m].pages[p].columns[c].width;
        let target = if relative {
            *width as i64 + value
        } else {
            value
        };
        *width = target.clamp(1, viewport as i64) as u32;
        Ok(())
    }

    pub(super) fn column_heights(&self, column: &Column, total: u32) -> Vec<u32> {
        let known: Vec<_> = column
            .windows
            .iter()
            .filter_map(|id| self.height_weights.get(id).copied())
            .collect();
        // Newly joined windows get an average share, not a near-zero default weight.
        let default = if known.is_empty() {
            1
        } else {
            (known.iter().map(|&w| w as u64).sum::<u64>() / known.len() as u64).max(1) as u32
        };
        let weights: Vec<_> = column
            .windows
            .iter()
            .map(|id| self.height_weights.get(id).copied().unwrap_or(default))
            .collect();
        distribute(total, &weights)
    }

    /// `applied` holds rects the backend actually placed (non-minimized). A tiled window whose
    /// native size now differs was resized by the user through its own border: keep that size
    /// instead of snapping it back. Transactional; `None` when nothing changed.
    pub fn adopt_native_sizes(
        &mut self,
        applied: &[(WindowId, Rect)],
    ) -> Result<Option<Transition>, AppError> {
        if !self.snapshot.enabled {
            return Ok(None);
        }
        let mut next = self.clone();
        let mut changed = false;
        for (id, rect) in applied {
            let Ok(w) = next.window_index(id) else {
                continue;
            };
            let native = next.snapshot.windows[w].native.clone();
            // Floating/fullscreen windows are rejected here; floating ones already keep native rects.
            let Ok((_, m, p, c, _)) = next.window_sizing_target(id) else {
                continue;
            };
            // A window the OS moved to another monitor (hotplug, DPI) was not resized by hand.
            if native.minimized || native.monitor_id != next.snapshot.monitors[m].monitor.id {
                continue;
            }
            let dw = native.rect.width as i64 - rect.width as i64;
            let dh = native.rect.height as i64 - rect.height as i64;
            if dw != 0 {
                let before = next.snapshot.monitors[m].pages[p].columns[c].width as i64;
                next.resize_window_column(id, dw, true)?;
                let monitor = &mut next.snapshot.monitors[m];
                let page = &mut monitor.pages[p];
                let grown = page.columns[c].width as i64 - before;
                // Left edge dragged: grow leftward, keeping the right edge where the user left it.
                if native.rect.x != rect.x && grown != 0 {
                    page.viewport_x =
                        clamp_scroll(page, monitor.viewport.width, page.viewport_x as i64 + grown);
                }
                changed |= grown != 0;
            }
            if dh != 0 {
                let before = next.height_weights.clone();
                next.adjust_height_of(id, dh.clamp(i32::MIN as i64, i32::MAX as i64) as i32)?;
                changed |= next.height_weights != before;
            }
        }
        if !changed {
            return Ok(None);
        }
        let actions = next.placements()?;
        *self = next;
        Ok(Some(self.transition(actions)))
    }

    pub(super) fn adjust_window_height(&mut self, delta: i32) -> Result<(), AppError> {
        self.adjust_height_of(&self.focused()?, delta)
    }

    fn adjust_height_of(&mut self, id: &str, delta: i32) -> Result<(), AppError> {
        let (_, m, p, c, row) = self.window_sizing_target(id)?;
        let monitor = &self.snapshot.monitors[m];
        let column = &monitor.pages[p].columns[c];
        if delta == 0 || column.windows.len() == 1 {
            return Ok(());
        }
        let total = monitor.viewport.height;
        let heights = self.column_heights(column, total);
        let target = (heights[row] as i64 + delta as i64)
            .clamp(1, total as i64 - column.windows.len() as i64 + 1) as u32;
        if target == heights[row] {
            return Ok(());
        }
        let weights: Vec<_> = heights
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != row)
            .map(|(_, &h)| h - 1)
            .collect();
        let mut others = distribute(total - target, &weights).into_iter();
        for (i, id) in column.windows.iter().enumerate() {
            let height = if i == row {
                target
            } else {
                others.next().unwrap()
            };
            self.height_weights.insert(id.clone(), height - 1);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Engine {
        let mut engine = Engine::new(BackendStatus {
            availability: BackendAvailability::Ready,
            capabilities: Capabilities {
                enumerate: true,
                placement: true,
                minimize: true,
                focus: true,
                ..Capabilities::default()
            },
            ..BackendStatus::default()
        });
        let rect = Rect {
            width: 1000,
            height: 800,
            ..Rect::default()
        };
        engine
            .reconcile(SystemSnapshot {
                monitors: ["a", "b"]
                    .map(|id| Monitor {
                        id: id.into(),
                        name: id.into(),
                        bounds: rect,
                        work_area: rect,
                        scale_factor: 1.0,
                        primary: id == "a",
                    })
                    .into(),
                windows: ["a", "b"]
                    .map(|id| NativeWindow {
                        id: id.into(),
                        monitor_id: id.into(),
                        title: id.into(),
                        app_name: "test".into(),
                        process_id: 1,
                        rect,
                        minimized: false,
                        minimized_by_manager: false,
                        resizable: true,
                    })
                    .into(),
                focused_window: Some("a".into()),
            })
            .unwrap();
        engine.dispatch(Command::Enable).unwrap();
        engine
    }

    fn width(window_id: &str, width: u32) -> Command {
        Command::SetWindowColumnWidth {
            window_id: window_id.into(),
            width,
        }
    }

    #[test]
    fn targeted_width_preserves_focus_monitor_background_page_and_clamps_to_target_viewport() {
        let mut e = engine();
        let tail = e.snapshot.monitors[1].pages.last().unwrap().id.clone();
        e.dispatch(Command::MoveWindowToPage {
            window_id: "b".into(),
            page_id: tail,
        })
        .unwrap();
        e.dispatch(Command::SwitchPage {
            monitor_id: "b".into(),
            page_id: e.snapshot.monitors[1].pages.last().unwrap().id.clone(),
        })
        .unwrap();
        e.dispatch(Command::FocusWindow {
            window_id: "a".into(),
        })
        .unwrap();
        e.snapshot.monitors[1].viewport.width = 700;
        e.snapshot.backend.capabilities.focus = false;
        // b is alone on its page, so it always fills the 700px viewport.
        for (requested, expected) in [(1, 700), (513, 700), (u32::MAX, 700)] {
            let before = e.snapshot.clone();
            let page_focus = e.page_focus.clone();
            let t = e.dispatch(width("b", requested)).unwrap();
            assert_eq!(t.snapshot.focused_window, before.focused_window);
            assert_eq!(t.snapshot.active_monitor, before.active_monitor);
            assert_eq!(e.page_focus, page_focus);
            for (after, before) in t.snapshot.monitors.iter().zip(&before.monitors) {
                assert_eq!(after.active_page, before.active_page);
            }
            assert!(
                !t.actions
                    .iter()
                    .any(|action| matches!(action, NativeAction::Focus { .. }))
            );
            let (_, m, p, c, _) = e.window_sizing_target("b").unwrap();
            assert_eq!(e.snapshot.monitors[m].pages[p].columns[c].width, expected);
            assert!(t.actions.iter().any(|action| matches!(action,
                NativeAction::Placement { window_id, minimized: true, rect, .. }
                if window_id == "b" && rect.width == expected)));
        }
        e.snapshot.focused_window = None;
        e.dispatch(width("b", 300)).unwrap();
        assert_eq!(e.snapshot.focused_window, None);
    }

    #[test]
    fn border_resized_tiled_window_keeps_its_size() {
        let mut e = engine();
        // Stack b under a in one column so height is adjustable too.
        let page = e.snapshot.monitors[0].active_page.clone();
        e.dispatch(Command::MoveWindowToPage {
            window_id: "b".into(),
            page_id: page,
        })
        .unwrap();
        e.dispatch(Command::FocusWindow {
            window_id: "b".into(),
        })
        .unwrap();
        if e.window_sizing_target("b").unwrap().3 != e.window_sizing_target("a").unwrap().3 {
            e.dispatch(Command::MoveWindow {
                direction: Direction::Left,
            })
            .unwrap();
        }
        // A second column keeps the page wider than the screen, so widths can change
        // (a lone column always fills the viewport).
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
        let mut extra = system.windows[0].clone();
        (extra.id, extra.title, extra.monitor_id) = ("c".into(), "c".into(), "a".into());
        system.windows.push(extra);
        e.reconcile(system).unwrap();
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
        // The backend placed both windows at `applied`; then `a` reports `resized`.
        let observe = |e: &mut Engine, resized: Rect, applied: &[&str]| {
            let applied: Vec<_> = applied
                .iter()
                .map(|id| (id.to_string(), plan(e, id)))
                .collect();
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
            for w in &mut system.windows {
                w.rect = if w.id == "a" { resized } else { plan(e, &w.id) };
            }
            e.reconcile(system).unwrap();
            e.adopt_native_sizes(&applied).unwrap()
        };
        let (_, m, p, c, _) = e.window_sizing_target("a").unwrap();
        let column = |e: &Engine| e.snapshot.monitors[m].pages[p].columns[c].width;
        let width = column(&e);
        let before = plan(&e, "a");

        // Right/bottom edges dragged: 100 narrower, 50 taller.
        let resized = Rect {
            width: before.width - 100,
            height: before.height + 50,
            ..before
        };
        // A placement the backend never applied (refused, not yet placed) is not a user resize.
        assert!(observe(&mut e, resized, &["b"]).is_none());
        assert_eq!(column(&e), width);
        let t = observe(&mut e, resized, &["a", "b"]).unwrap();
        assert_eq!(column(&e), width - 100);
        assert_eq!(plan(&e, "a").height, before.height + 50);
        assert!(t.actions.iter().any(|a| matches!(a,
            NativeAction::Placement { window_id, rect, .. } if window_id == "a" && *rect == resized)));
        // Stable: the next refresh at the adopted size changes nothing.
        let after = plan(&e, "a");
        assert!(observe(&mut e, after, &["a", "b"]).is_none());

        // Left edge dragged 60px right: the column narrows and the view keeps its right edge
        // unless the scroll limit clamps it.
        let viewport_x = e.snapshot.monitors[m].pages[p].viewport_x;
        let left = Rect {
            x: after.x + 60,
            width: after.width - 60,
            ..after
        };
        observe(&mut e, left, &["a", "b"]).unwrap();
        assert_eq!(column(&e), width - 160);
        let page = &e.snapshot.monitors[m].pages[p];
        assert_eq!(
            page.viewport_x,
            clamp_scroll(
                page,
                e.snapshot.monitors[m].viewport.width,
                viewport_x as i64 - 60
            )
        );
    }
    #[test]
    fn targeted_width_rejects_invalid_targets_and_pause_transactionally() {
        for state in 0..5 {
            let mut e = engine();
            match state {
                0 => {
                    e.dispatch(Command::Disable).unwrap();
                }
                1 => {
                    e.dispatch(Command::ToggleFloating).unwrap();
                }
                2 => {
                    e.dispatch(Command::ToggleFullscreen).unwrap();
                }
                _ => {}
            }
            let before = serde_json::to_value(e.snapshot()).unwrap();
            let command = match state {
                3 => width("gone", 300),
                4 => width("a", 0),
                _ => width("a", 300),
            };
            let error = e.dispatch(command).unwrap_err();
            assert_eq!(
                error.code,
                if state == 3 {
                    ErrorCode::WindowGone
                } else {
                    ErrorCode::InvalidCommand
                }
            );
            assert_eq!(serde_json::to_value(e.snapshot()).unwrap(), before);
        }
    }

    #[test]
    fn targeted_width_has_frozen_json_and_strict_config_contract() {
        let value =
            serde_json::json!({"type":"setWindowColumnWidth","windowId":"b","width":u32::MAX});
        assert_eq!(serde_json::to_value(width("b", u32::MAX)).unwrap(), value);
        assert_eq!(
            serde_json::from_value::<Command>(value.clone()).unwrap(),
            width("b", u32::MAX)
        );
        let binding =
            serde_json::json!({"key":"Control+Alt+W","action":{"type":"command","command":value}});
        assert!(serde_json::from_value::<crate::config::ShortcutBinding>(binding.clone()).is_ok());
        let mut invalid_binding = binding;
        invalid_binding["action"]["command"]["unexpected"] = true.into();
        assert!(serde_json::from_value::<crate::config::ShortcutBinding>(invalid_binding).is_err());
        for invalid_width in [
            serde_json::json!(-1),
            serde_json::json!(4294967296_u64),
            serde_json::json!(1.5),
        ] {
            let mut invalid = value.clone();
            invalid["width"] = invalid_width;
            assert!(serde_json::from_value::<Command>(invalid).is_err());
        }
    }
}
