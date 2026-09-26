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

    pub(super) fn adjust_window_height(&mut self, delta: i32) -> Result<(), AppError> {
        let (_, m, p, c, row) = self.sizing_target()?;
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
        for (requested, expected) in [(1, 1), (513, 513), (u32::MAX, 700)] {
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
