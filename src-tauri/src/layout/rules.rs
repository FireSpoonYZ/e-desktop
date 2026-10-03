use super::*;

/// lane: rules-spawn-screenshot. Window size limits from rules, in logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct SizeLimits {
    min_width: Option<u32>,
    max_width: Option<u32>,
    min_height: Option<u32>,
    max_height: Option<u32>,
}

fn physical(logical: u32, scale: f64) -> u32 {
    (f64::from(logical) * scale)
        .round()
        .clamp(0.0, f64::from(u32::MAX)) as u32
}

/// Column width range from the rule limits of the column's windows (window size plus `gap`).
fn rule_width_bounds(
    size_limits: &BTreeMap<WindowId, SizeLimits>,
    column: &Column,
    scale: f64,
    gap: u32,
) -> (u32, u32) {
    let limits: Vec<_> = column
        .windows
        .iter()
        .filter_map(|id| size_limits.get(id))
        .collect();
    let min = limits
        .iter()
        .filter_map(|l| l.min_width)
        .map(|v| physical(v, scale).saturating_add(gap))
        .max()
        .unwrap_or(0);
    let max = limits
        .iter()
        .filter_map(|l| l.max_width)
        .map(|v| physical(v, scale).saturating_add(gap))
        .min()
        .unwrap_or(u32::MAX);
    (min, max)
}

/// Rows of `total` pixels shaped like `desired`, each inside `[lo, hi]` (lo wins over hi).
/// Rows that would leave their range are pinned to it, largest violation side first, and the
/// rest share what is left. Minimums that cannot all fit shrink proportionally.
fn constrain_heights(total: u32, desired: &[u32], lo: &[u32], hi: &[u32]) -> Vec<u32> {
    let n = desired.len();
    let lo: Vec<u64> = lo.iter().map(|&v| u64::from(v.max(1))).collect();
    let hi: Vec<u64> = (0..n).map(|i| u64::from(hi[i]).max(lo[i])).collect();
    let total = u64::from(total);
    let share = |amount: u64, weights: &[u64]| -> Vec<u64> {
        let sum = weights.iter().map(|&w| u128::from(w)).sum::<u128>().max(1);
        let (mut cumulative, mut previous) = (0u128, 0u128);
        weights
            .iter()
            .map(|&w| {
                cumulative += u128::from(w);
                let boundary = u128::from(amount) * cumulative / sum;
                let part = boundary - previous;
                previous = boundary;
                part as u64
            })
            .collect()
    };
    if lo.iter().sum::<u64>() >= total {
        return share(total, &lo)
            .into_iter()
            .map(|h| h.max(1) as u32)
            .collect();
    }
    let mut fixed: Vec<Option<u64>> = vec![None; n];
    loop {
        let free: Vec<usize> = (0..n).filter(|&i| fixed[i].is_none()).collect();
        if free.is_empty() {
            break;
        }
        let used: u64 = fixed.iter().flatten().sum();
        let weights: Vec<u64> = free.iter().map(|&i| u64::from(desired[i].max(1))).collect();
        let parts = share(total.saturating_sub(used), &weights);
        let under: u64 = free
            .iter()
            .zip(&parts)
            .map(|(&i, &p)| lo[i].saturating_sub(p))
            .sum();
        let over: u64 = free
            .iter()
            .zip(&parts)
            .map(|(&i, &p)| p.saturating_sub(hi[i]))
            .sum();
        if under == 0 && over == 0 {
            for (&i, &p) in free.iter().zip(&parts) {
                fixed[i] = Some(p);
            }
            break;
        }
        for (&i, &p) in free.iter().zip(&parts) {
            if under >= over && p < lo[i] {
                fixed[i] = Some(lo[i]);
            } else if under < over && p > hi[i] {
                fixed[i] = Some(hi[i]);
            }
        }
    }
    fixed.into_iter().map(|h| h.unwrap_or(1) as u32).collect()
}

impl Engine {
    /// Replace rules atomically. Existing window IDs keep their user-controlled layout.
    pub fn set_window_rules(&mut self, rules: Vec<WindowRule>) -> Result<(), AppError> {
        for rule in &rules {
            rule.validate()?;
        }
        self.window_rules = rules;
        Ok(())
    }

    pub(super) fn update_pending_rule_floating(&mut self, actions: &[NativeAction]) {
        for action in actions {
            // A minimized placement only hides the native window; it does not apply its rect.
            if let NativeAction::Placement {
                window_id,
                minimized: false,
                ..
            }
            | NativeAction::Restore { window_id } = action
            {
                self.pending_rule_floating.remove(window_id);
            }
        }
        self.pending_rule_floating.retain(|id| {
            self.snapshot
                .windows
                .iter()
                .any(|w| &w.native.id == id && w.floating)
                && self
                    .snapshot
                    .monitors
                    .iter()
                    .any(|m| m.pages.iter().any(|p| p.floating_windows.contains(id)))
        });
    }

    pub(super) fn insert_new_window(&mut self, id: &str) -> Result<(), AppError> {
        let w = self.window_index(id)?;
        let native = &self.snapshot.windows[w].native;
        let mut matched = WindowRule::default();
        for rule in self.window_rules.iter().filter(|rule| rule.matches(native)) {
            matched.floating = rule.floating.or(matched.floating);
            matched.column_width = rule.column_width.or(matched.column_width);
            matched.monitor_id = rule.monitor_id.clone().or(matched.monitor_id);
            matched.page_index = rule.page_index.or(matched.page_index);
            matched.page_name = rule.page_name.clone().or(matched.page_name); // lane: layout-options
            matched.open_maximized = rule.open_maximized.or(matched.open_maximized);
            matched.open_fullscreen = rule.open_fullscreen.or(matched.open_fullscreen);
            matched.open_focused = rule.open_focused.or(matched.open_focused);
            matched.min_width = rule.min_width.or(matched.min_width);
            matched.max_width = rule.max_width.or(matched.max_width);
            matched.min_height = rule.min_height.or(matched.min_height);
            matched.max_height = rule.max_height.or(matched.max_height);
        }
        let source = self.monitor_index(&native.monitor_id)?;
        let m = matched
            .monitor_id
            .as_ref()
            .and_then(|id| self.monitor_index(id).ok())
            .unwrap_or(source);
        let monitor = &self.snapshot.monitors[m];
        // Only address existing pages; missing targets fall back without creating empty pages.
        let p = matched
            .page_index
            .and_then(|index| index.checked_sub(1))
            .filter(|index| *index < monitor.pages.len())
            .unwrap_or_else(|| {
                monitor
                    .pages
                    .iter()
                    .position(|p| p.id == monitor.active_page)
                    .unwrap()
            });
        // lane: layout-options: an existing named page wins; a missing one falls back.
        let (m, p) = matched
            .page_name
            .as_deref()
            .and_then(|name| self.named_page(name))
            .unwrap_or((m, p));
        if !native.resizable && matched.floating != Some(true) {
            self.size_floating.insert(id.into());
        }
        self.snapshot.windows[w].floating = !native.resizable || matched.floating.unwrap_or(false);
        if self.snapshot.windows[w].floating && source != m {
            let source = self.snapshot.monitors[source].viewport;
            let target = self.snapshot.monitors[m].viewport;
            self.pending_rule_floating.insert(id.into());
            let rect = &mut self.snapshot.windows[w].native.rect;
            // Same translation/clamping as a manual cross-monitor floating move.
            rect.x = coordinate((rect.x as i64 + target.x as i64 - source.x as i64).clamp(
                target.x as i64,
                target.x as i64 + target.width.saturating_sub(rect.width) as i64,
            ));
            rect.y = coordinate((rect.y as i64 + target.y as i64 - source.y as i64).clamp(
                target.y as i64,
                target.y as i64 + target.height.saturating_sub(rect.height) as i64,
            ));
        }
        let page = &self.snapshot.monitors[m].pages[p];
        let at = self
            .page_focus
            .get(&page.id)
            .and_then(|focused| {
                page.columns
                    .iter()
                    .position(|c| c.windows.contains(focused))
            })
            .map(|c| c + 1)
            .unwrap_or(page.columns.len());
        // insert_window clamps a maximized column to the target viewport width.
        let width = if matched.open_maximized == Some(true) {
            Some(u32::MAX)
        } else {
            matched.column_width
        };
        self.insert_window(m, p, id, width)?;
        if !self.snapshot.windows[w].floating {
            let page = &mut self.snapshot.monitors[m].pages[p];
            let column = page.columns.pop().unwrap();
            page.columns.insert(at, column);
        }
        self.apply_open_rules(id, &matched);
        Ok(())
    }

    /// lane: rules-spawn-screenshot. Fullscreen, focus and size-limit effects of a new window.
    fn apply_open_rules(&mut self, id: &str, matched: &WindowRule) {
        let Ok(w) = self.window_index(id) else {
            return;
        };
        let window = &mut self.snapshot.windows[w];
        if matched.open_fullscreen == Some(true) && window.native.resizable {
            window.fullscreen = true;
            if window.floating {
                self.fullscreen_restore
                    .insert(id.into(), window.native.rect);
            }
        }
        if let Some(focus) = matched.open_focused {
            self.open_focus.insert(id.into(), focus);
        }
        let limits = SizeLimits {
            min_width: matched.min_width,
            max_width: matched.max_width,
            min_height: matched.min_height,
            max_height: matched.max_height,
        };
        if limits != SizeLimits::default() {
            self.size_limits.insert(id.into(), limits);
        }
    }

    /// lane: rules-spawn-screenshot. Settle `openFocused` for windows discovered by this
    /// reconcile. True when native focus must go back to the engine's (unchanged) focus.
    pub(super) fn apply_open_focus(&mut self, previous: Option<&str>) -> bool {
        let requests = std::mem::take(&mut self.open_focus);
        let manage = self.snapshot.enabled && self.snapshot.backend.capabilities.focus;
        let mut refocus = false;
        for (id, focus) in requests {
            let focused = self.snapshot.focused_window.as_deref() == Some(id.as_str());
            if manage
                && focus
                && !focused
                && self.location(&id).is_ok()
                && !self.window_protected(&id)
            {
                let _ = self.set_focus(&id, true);
            } else if manage && !focus && focused {
                // Native focus on the new window did not scroll (reconcile_inner); handing focus
                // back leaves the view as it was before the window opened (niri).
                if let Some(previous) = previous
                    .filter(|p| *p != id && self.location(p).is_ok() && !self.window_protected(p))
                {
                    refocus |= self.set_focus(previous, false).is_ok();
                }
            }
            // Focus stays on a window that opened unfocused: reveal it as native focus would.
            if !focus && self.snapshot.focused_window.as_deref() == Some(id.as_str()) {
                let _ = self.reveal(&id, false, previous);
            }
        }
        refocus
    }

    /// lane: rules-spawn-screenshot. Clamp tiled column widths to the rule limits of their
    /// windows (window size plus the gap). Native minimum widths and the viewport still win.
    pub(super) fn apply_size_limits(&mut self) {
        let windows = &self.snapshot.windows;
        self.size_limits
            .retain(|id, _| windows.iter().any(|w| &w.native.id == id));
        if self.size_limits.is_empty() {
            return;
        }
        let gaps = self.snapshot.gaps;
        for monitor in &mut self.snapshot.monitors {
            let scale = monitor.monitor.scale_factor;
            let gap = 2 * half_gap(gaps, scale);
            for column in monitor.pages.iter_mut().flat_map(|p| &mut p.columns) {
                let (min, max) = rule_width_bounds(&self.size_limits, column, scale, gap);
                column.width = column.width.min(max).max(min);
            }
        }
    }

    /// The width `cleanup` settles `column` of monitor `m` at when it asks for `width`: rule
    /// limits (`apply_size_limits`), then native minimum widths and the viewport.
    pub(super) fn settled_column_width(&self, m: usize, column: &Column, width: u32) -> u32 {
        let monitor = &self.snapshot.monitors[m];
        let scale = monitor.monitor.scale_factor;
        let gap = 2 * half_gap(self.snapshot.gaps, scale);
        let (min, max) = rule_width_bounds(&self.size_limits, column, scale, gap);
        let native = column
            .windows
            .iter()
            .filter_map(|id| self.min_widths.get(id))
            .max()
            .map_or(0, |w| w.saturating_add(gap));
        width
            .min(max)
            .max(min)
            .max(native)
            .min(monitor.viewport.width)
            .max(1)
    }

    /// Settle the column widths of page `p` on monitor `m` now, so a scroll computed before
    /// the next `cleanup` already sees the final widths.
    pub(super) fn settle_column_widths(&mut self, m: usize, p: usize) {
        let widths: Vec<u32> = self.snapshot.monitors[m].pages[p]
            .columns
            .iter()
            .map(|column| self.settled_column_width(m, column, column.width))
            .collect();
        let columns = &mut self.snapshot.monitors[m].pages[p].columns;
        for (column, width) in columns.iter_mut().zip(widths) {
            column.width = width;
        }
    }

    /// Height of tab `id` in a tabbed column, `available` below the indicator: only that
    /// window's rule limits apply, and the space below a capped tab stays empty.
    pub(super) fn limit_tab_height(&self, id: &str, available: u32, scale: f64) -> u32 {
        let Some(limits) = self.size_limits.get(id) else {
            return available;
        };
        let max = limits
            .max_height
            .map_or(available, |v| physical(v, scale).min(available));
        let min = limits
            .min_height
            .map_or(0, |v| physical(v, scale).min(available));
        max.max(min).max(1)
    }

    /// lane: rules-spawn-screenshot. Row heights of `column` kept inside rule height limits.
    pub(super) fn limit_heights(&self, column: &Column, heights: Vec<u32>) -> Vec<u32> {
        if !column.windows.iter().any(|id| {
            self.size_limits
                .get(id)
                .is_some_and(|l| l.min_height.is_some() || l.max_height.is_some())
        }) {
            return heights;
        }
        let Some(scale) = self
            .snapshot
            .monitors
            .iter()
            .find(|m| {
                m.pages
                    .iter()
                    .any(|p| p.columns.iter().any(|c| c.id == column.id))
            })
            .map(|m| m.monitor.scale_factor)
        else {
            return heights;
        };
        let gap = 2 * half_gap(self.snapshot.gaps, scale);
        let limit = |id: &WindowId, pick: fn(&SizeLimits) -> Option<u32>| {
            self.size_limits
                .get(id)
                .and_then(pick)
                .map(|v| physical(v, scale).saturating_add(gap))
        };
        let lo: Vec<u32> = column
            .windows
            .iter()
            .map(|id| limit(id, |l| l.min_height).unwrap_or(1))
            .collect();
        let hi: Vec<u32> = column
            .windows
            .iter()
            .map(|id| limit(id, |l| l.max_height).unwrap_or(u32::MAX))
            .collect();
        let total = heights.iter().map(|&h| u64::from(h)).sum::<u64>();
        constrain_heights(total.min(u64::from(u32::MAX)) as u32, &heights, &lo, &hi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(id: &str, x: i32) -> Monitor {
        let area = Rect {
            x,
            y: 0,
            width: 1200,
            height: 900,
        };
        Monitor {
            id: id.into(),
            name: id.into(),
            bounds: area,
            work_area: area,
            scale_factor: 1.0,
            primary: x == 0,
        }
    }

    fn window(id: &str, monitor_id: &str) -> NativeWindow {
        NativeWindow {
            id: id.into(),
            title: id.into(),
            app_name: "test".into(),
            process_id: 1,
            monitor_id: monitor_id.into(),
            rect: Rect {
                x: 40,
                y: 40,
                width: 500,
                height: 400,
            },
            minimized: false,
            minimized_by_manager: false,
            resizable: true,
        }
    }

    fn system() -> SystemSnapshot {
        SystemSnapshot {
            monitors: vec![monitor("a", 0), monitor("b", -1200)],
            windows: vec![
                window("1", "a"),
                window("2", "a"),
                window("3", "a"),
                window("4", "b"),
            ],
            focused_window: Some("1".into()),
        }
    }

    fn engine() -> Engine {
        let mut e = Engine::new(BackendStatus {
            kind: BackendKind::Windows,
            availability: BackendAvailability::Ready,
            capabilities: Capabilities {
                enumerate: true,
                placement: true,
                minimize: true,
                clipping: true,
                focus: true,
                ..Capabilities::default()
            },
            message: String::new(),
        });
        e.reconcile(system()).unwrap();
        e.dispatch(Command::Enable).unwrap();
        e
    }

    fn placement(actions: &[NativeAction], id: &str) -> (Rect, Option<Rect>, bool) {
        actions
            .iter()
            .find_map(|action| match action {
                NativeAction::Placement {
                    window_id,
                    rect,
                    clip,
                    minimized,
                } if window_id == id => Some((*rect, *clip, *minimized)),
                _ => None,
            })
            .unwrap()
    }

    fn rule(json: &str) -> WindowRule {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn first_discovery_merges_matching_actions_in_order_and_leaves_nonmatches_alone() {
        let mut e = engine();
        e.set_window_rules(vec![
            rule(r#"{"title":"new","columnWidth":444,"monitorId":"a","pageIndex":1}"#),
            rule(r#"{"appName":"test","title":"NEW","floating":true,"columnWidth":333,"monitorId":"b"}"#),
            rule(r#"{"title":"new","floating":false,"pageIndex":2}"#),
            rule(r#"{"title":"unmatched","columnWidth":999}"#),
        ]).unwrap();
        let mut native = system();
        native
            .windows
            .extend([window("new", "a"), window("plain", "a")]);
        let t = e.reconcile(native).unwrap();
        let (m, p, column) = e.location("new").unwrap();
        assert_eq!((m, p), (1, 1));
        assert_eq!(
            e.snapshot.monitors[m].pages[p].columns[column.unwrap().0].width,
            333
        );
        assert!(!e.snapshot.windows[e.window_index("new").unwrap()].floating);
        assert!(placement(&t.actions, "new").2);
        let (m, p, column) = e.location("plain").unwrap();
        assert_eq!((m, p), (0, 0));
        assert_eq!(
            e.snapshot.monitors[m].pages[p].columns[column.unwrap().0].width,
            600
        );
    }

    #[test]
    fn reloading_and_title_changes_do_not_override_manual_layout_or_old_nonmatches() {
        let mut e = engine();
        e.set_window_rules(vec![rule(
            r#"{"title":"new","columnWidth":333,"monitorId":"b","pageIndex":2}"#,
        )])
        .unwrap();
        let mut native = system();
        native.windows.push(window("new", "a"));
        e.reconcile(native.clone()).unwrap();
        let page = e.snapshot.monitors[0].active_page.clone();
        e.dispatch(Command::MoveWindowToPage {
            window_id: "new".into(),
            page_id: page,
        })
        .unwrap();
        e.dispatch(Command::CycleWidth).unwrap();
        let before = serde_json::to_value(&e.snapshot.monitors).unwrap();
        e.set_window_rules(vec![rule(
            r#"{"floating":false,"columnWidth":12,"monitorId":"b","pageIndex":2}"#,
        )])
        .unwrap();
        native.focused_window = Some("new".into());
        native.windows[0].title = "new".into();
        native.windows.last_mut().unwrap().title = "changed".into();
        e.reconcile(native.clone()).unwrap();
        assert_eq!(serde_json::to_value(&e.snapshot.monitors).unwrap(), before);
        e.dispatch(Command::ToggleFloating).unwrap();
        let before = serde_json::to_value(&e.snapshot.monitors).unwrap();
        e.reconcile(native.clone()).unwrap();
        assert_eq!(serde_json::to_value(&e.snapshot.monitors).unwrap(), before);
        assert!(e.snapshot.windows[e.window_index("new").unwrap()].floating);
        e.set_window_rules(vec![]).unwrap();
        e.reconcile(native).unwrap();
        assert_eq!(serde_json::to_value(&e.snapshot.monitors).unwrap(), before);
    }

    #[test]
    fn invalid_setter_is_atomic_and_does_not_change_snapshot() {
        let mut e = engine();
        let rules = vec![rule(r#"{"floating":true}"#)];
        e.set_window_rules(rules.clone()).unwrap();
        let before = serde_json::to_value(e.snapshot()).unwrap();
        for invalid in [
            r#"{}"#,
            r#"{"columnWidth":0}"#,
            r#"{"pageIndex":0}"#,
            r#"{"monitorId":" "}"#,
            r#"{"title":" ","floating":false}"#,
        ] {
            assert!(
                e.set_window_rules(vec![rule(r#"{"floating":false}"#), rule(invalid)])
                    .is_err()
            );
            assert_eq!(e.window_rules, rules);
            assert_eq!(serde_json::to_value(e.snapshot()).unwrap(), before);
        }
        let mut native = system();
        native.windows.push(window("new", "a"));
        e.reconcile(native).unwrap();
        assert!(e.snapshot.windows[e.window_index("new").unwrap()].floating);
    }

    #[test]
    fn cross_monitor_background_routing_never_steals_focus_including_two_paused_refreshes() {
        for enabled in [false, true] {
            let mut e = engine();
            if !enabled {
                e.dispatch(Command::Disable).unwrap();
            }
            let active_pages: Vec<_> = e
                .snapshot
                .monitors
                .iter()
                .map(|m| m.active_page.clone())
                .collect();
            e.set_window_rules(vec![rule(r#"{"monitorId":"b","pageIndex":2}"#)])
                .unwrap();
            let mut native = system();
            native.windows.push(window("new", "a"));
            native.focused_window = Some("new".into());
            for _ in 0..2 {
                let t = e.reconcile(native.clone()).unwrap();
                assert_eq!(e.location("new").unwrap().0, 1);
                assert_eq!(e.location("new").unwrap().1, 1);
                assert_eq!(
                    e.snapshot
                        .monitors
                        .iter()
                        .map(|m| m.active_page.clone())
                        .collect::<Vec<_>>(),
                    active_pages
                );
                assert_eq!(e.snapshot.focused_window.as_deref(), Some("1"));
                assert_eq!(e.snapshot.active_monitor.as_deref(), Some("a"));
                assert!(!t.actions.iter().any(|action| matches!(action, NativeAction::Focus { window_id } if window_id == "new")));
                if enabled {
                    assert!(placement(&t.actions, "new").2);
                } else {
                    assert!(t.actions.is_empty());
                }
            }
        }
    }

    #[test]
    fn missing_monitor_and_out_of_range_page_fall_back_without_creating_requested_pages() {
        for monitor_id in [None, Some("missing"), Some("b")] {
            let mut e = engine();
            e.dispatch(Command::AddPage {
                monitor_id: "a".into(),
            })
            .unwrap();
            let mut native = system();
            // b existed before reconciliation but is now disconnected.
            native.monitors.retain(|m| m.id == "a");
            native.windows.retain(|w| w.monitor_id == "a");
            native.windows.push(window("new", "a"));
            native.focused_window = None;
            let active = e.snapshot.monitors[0].active_page.clone();
            e.set_window_rules(vec![WindowRule {
                monitor_id: monitor_id.map(str::to_owned),
                page_index: Some(usize::MAX),
                ..WindowRule::default()
            }])
            .unwrap();
            e.reconcile(native).unwrap();
            let (m, p, _) = e.location("new").unwrap();
            assert_eq!(m, 0);
            assert_eq!(e.snapshot.monitors[m].pages[p].id, active);
            assert_eq!(e.snapshot.monitors[m].pages.len(), 3); // Existing pages plus normal empty tail.
        }
    }

    #[test]
    fn valid_target_monitor_uses_its_active_page_and_width_is_clamped_to_its_viewport() {
        let mut e = engine();
        e.dispatch(Command::AddPage {
            monitor_id: "b".into(),
        })
        .unwrap();
        let active = e.snapshot.monitors[1].active_page.clone();
        e.set_viewports(BTreeMap::from([(
            "b".into(),
            Rect {
                x: -1100,
                y: 50,
                width: 700,
                height: 800,
            },
        )]));
        e.set_window_rules(vec![rule(
            r#"{"monitorId":"b","pageIndex":99,"columnWidth":4294967295}"#,
        )])
        .unwrap();
        let mut native = system();
        native.windows.push(window("new", "a"));
        native.focused_window = None;
        let t = e.reconcile(native).unwrap();
        let (m, p, column) = e.location("new").unwrap();
        assert_eq!(m, 1);
        assert_eq!(e.snapshot.monitors[m].pages[p].id, active);
        assert_eq!(
            e.snapshot.monitors[m].pages[p].columns[column.unwrap().0].width,
            700
        );
        assert_eq!(placement(&t.actions, "new").0.width, 700);
    }

    #[test]
    fn nonresizable_stays_floating_and_cross_monitor_float_is_translated_and_clamped() {
        for resizable in [false, true] {
            let mut e = engine();
            e.set_window_rules(vec![WindowRule {
                floating: Some(resizable),
                monitor_id: Some("b".into()),
                column_width: Some(42),
                ..WindowRule::default()
            }])
            .unwrap();
            let mut native = system();
            let mut new = window("new", "a");
            new.resizable = resizable;
            new.rect.x = 1100;
            new.rect.y = 850;
            native.windows.push(new);
            let t = e.reconcile(native).unwrap();
            assert_eq!(e.location("new").unwrap(), (1, 0, None));
            assert_eq!(
                placement(&t.actions, "new").0,
                Rect {
                    x: -500,
                    y: 500,
                    width: 500,
                    height: 400
                }
            );
            assert!(!placement(&t.actions, "new").2);
        }
    }

    #[test]
    fn initial_rules_work_while_paused_and_page_indices_do_not_create_pages() {
        let mut e = Engine::new(BackendStatus::default());
        e.set_window_rules(vec![rule(r#"{"columnWidth":1,"pageIndex":2}"#)])
            .unwrap();
        let t = e
            .reconcile(SystemSnapshot {
                monitors: vec![monitor("a", 0)],
                windows: vec![window("new", "a")],
                focused_window: Some("new".into()),
            })
            .unwrap();
        assert!(t.actions.is_empty());
        assert!(!e.snapshot.enabled);
        let (m, p, column) = e.location("new").unwrap();
        assert_eq!((m, p), (0, 0));
        assert_eq!(
            e.snapshot.monitors[m].pages[p].columns[column.unwrap().0].width,
            1
        );
        assert_eq!(e.snapshot.monitors[m].pages.len(), 2);
    }

    #[test]
    fn paused_cross_monitor_floating_keeps_rule_rect_until_visible_placement() {
        for page_index in [1, 2] {
            let mut e = engine();
            e.dispatch(Command::Disable).unwrap();
            e.set_window_rules(vec![WindowRule {
                floating: Some(true),
                monitor_id: Some("b".into()),
                page_index: Some(page_index),
                ..WindowRule::default()
            }])
            .unwrap();
            let mut native = system();
            native.windows.push(window("new", "a"));
            native.focused_window = Some("new".into());
            let expected = Rect {
                x: -1160,
                y: 40,
                width: 500,
                height: 400,
            };
            for _ in 0..2 {
                assert!(e.reconcile(native.clone()).unwrap().actions.is_empty());
                assert_eq!(
                    e.snapshot.windows[e.window_index("new").unwrap()]
                        .native
                        .rect,
                    expected
                );
            }
            // Reloads still do not reroute an existing pending window.
            e.set_window_rules(vec![rule(r#"{"floating":false,"monitorId":"a"}"#)])
                .unwrap();
            let t = e.dispatch(Command::Enable).unwrap();
            assert_eq!(
                placement(&t.actions, "new"),
                (expected, None, page_index == 2)
            );
            if page_index == 2 {
                // A minimized Placement does not move the native rectangle on Windows.
                native.windows.last_mut().unwrap().minimized = true;
                native.windows.last_mut().unwrap().minimized_by_manager = true;
                e.reconcile(native.clone()).unwrap();
                // Even an external restore on the old monitor cannot consume the pending move.
                native.windows.last_mut().unwrap().minimized = false;
                native.windows.last_mut().unwrap().minimized_by_manager = false;
                let t = e.reconcile(native.clone()).unwrap();
                assert_eq!(placement(&t.actions, "new"), (expected, None, true));
                let page = e.snapshot.monitors[1].pages[1].id.clone();
                let t = e
                    .dispatch(Command::SwitchPage {
                        monitor_id: "b".into(),
                        page_id: page,
                    })
                    .unwrap();
                assert_eq!(placement(&t.actions, "new"), (expected, None, false));
            }
            // After the first visible placement, ordinary native/user geometry wins again.
            let moved = Rect {
                x: -900,
                y: 120,
                width: 420,
                height: 300,
            };
            let new = native.windows.last_mut().unwrap();
            new.monitor_id = "b".into();
            new.rect = moved;
            let t = e.reconcile(native).unwrap();
            assert_eq!(placement(&t.actions, "new"), (moved, None, false));
        }
    }

    #[test]
    fn pending_rule_geometry_is_released_on_restore_removal_disconnect_and_manual_move() {
        for event in ["restore", "remove", "disconnect", "manual"] {
            let mut e = engine();
            e.dispatch(Command::Disable).unwrap();
            e.set_window_rules(vec![rule(
                r#"{"floating":true,"monitorId":"b","pageIndex":2}"#,
            )])
            .unwrap();
            let mut native = system();
            native.windows.push(window("new", "a"));
            native.focused_window = None;
            e.reconcile(native.clone()).unwrap();
            assert!(e.pending_rule_floating.contains("new"));
            match event {
                "restore" => {
                    e.dispatch(Command::Enable).unwrap();
                    assert!(e.pending_rule_floating.contains("new"));
                    let t = e.dispatch(Command::Disable).unwrap();
                    assert!(t.actions.iter().any(
                        |a| matches!(a, NativeAction::Restore { window_id } if window_id == "new")
                    ));
                }
                "remove" => {
                    native.windows.pop();
                }
                "disconnect" => {
                    native.monitors.retain(|m| m.id != "b");
                    native.windows.retain(|w| w.id != "4");
                }
                "manual" => {
                    e.dispatch(Command::Enable).unwrap();
                    let page = e.snapshot.monitors[0].active_page.clone();
                    let t = e
                        .dispatch(Command::MoveWindowToPage {
                            window_id: "new".into(),
                            page_id: page,
                        })
                        .unwrap();
                    assert_eq!(
                        placement(&t.actions, "new"),
                        (window("new", "a").rect, None, false)
                    );
                }
                _ => unreachable!(),
            }
            e.reconcile(native.clone()).unwrap();
            if event == "disconnect" {
                // The borrowed background page is retained; its translated move is still pending.
                assert!(e.pending_rule_floating.contains("new"));
                let (m, p, _) = e.location("new").unwrap();
                assert_ne!(
                    e.snapshot.monitors[m].pages[p].id,
                    e.snapshot.monitors[m].active_page
                );
            } else {
                assert!(e.pending_rule_floating.is_empty(), "{event}");
            }
            if event != "remove" {
                assert_eq!(
                    e.snapshot.windows[e.window_index("new").unwrap()]
                        .native
                        .rect,
                    window("new", "a").rect,
                    "{event}"
                );
            } else {
                e.set_window_rules(vec![]).unwrap();
                native.windows.push(window("new", "a"));
                e.reconcile(native).unwrap();
                assert!(!e.snapshot.windows[e.window_index("new").unwrap()].floating);
            }
        }
    }

    #[test]
    fn disconnected_logical_monitor_does_not_reapply_rules_to_existing_window() {
        let mut e = engine();
        e.set_window_rules(vec![rule(r#"{"floating":true,"columnWidth":7}"#)])
            .unwrap();
        let mut native = system();
        native.monitors.retain(|m| m.id == "a");
        native.windows[3].monitor_id = "a".into();
        e.reconcile(native).unwrap();
        let (m, p, column) = e.location("4").unwrap();
        assert_eq!(m, 0);
        assert_eq!(
            e.snapshot.monitors[m].pages[p].columns[column.unwrap().0].width,
            600 // Existing window: not the rule's 7, and not stretched to the viewport.
        );
        assert!(!e.snapshot.windows[e.window_index("4").unwrap()].floating);
    }
}

// lane: rules-spawn-screenshot
#[cfg(test)]
mod open_rule_tests {
    use super::*;

    fn native(id: &str) -> NativeWindow {
        NativeWindow {
            id: id.into(),
            title: format!("title {id}"),
            app_name: "app".into(),
            process_id: 1,
            monitor_id: "a".into(),
            rect: Rect {
                x: 40,
                y: 40,
                width: 500,
                height: 400,
            },
            minimized: false,
            minimized_by_manager: false,
            resizable: true,
        }
    }

    fn system(scale: f64, ids: &[&str], focused: &str) -> SystemSnapshot {
        let area = Rect {
            x: 0,
            y: 0,
            width: 1200,
            height: 900,
        };
        SystemSnapshot {
            monitors: vec![Monitor {
                id: "a".into(),
                name: "a".into(),
                bounds: area,
                work_area: area,
                scale_factor: scale,
                primary: true,
            }],
            windows: ids.iter().map(|id| native(id)).collect(),
            focused_window: Some(focused.into()),
        }
    }

    /// Enabled engine with window "1" focused and `rule` applying to later windows.
    fn engine(scale: f64, rule: &str) -> Engine {
        let mut e = Engine::new(BackendStatus {
            kind: BackendKind::Windows,
            availability: BackendAvailability::Ready,
            capabilities: Capabilities {
                enumerate: true,
                placement: true,
                minimize: true,
                clipping: true,
                focus: true,
                ..Capabilities::default()
            },
            message: String::new(),
        });
        e.reconcile(system(scale, &["1"], "1")).unwrap();
        e.dispatch(Command::Enable).unwrap();
        e.set_window_rules(vec![serde_json::from_str(rule).unwrap()])
            .unwrap();
        e
    }

    fn width(e: &Engine, id: &str) -> u32 {
        let (m, p, column) = e.location(id).unwrap();
        e.snapshot.monitors[m].pages[p].columns[column.unwrap().0].width
    }

    fn focus_actions(t: &Transition) -> Vec<&str> {
        t.actions
            .iter()
            .filter_map(|a| match a {
                NativeAction::Focus { window_id } => Some(window_id.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn open_maximized_fills_the_viewport_and_open_fullscreen_sets_layout_fullscreen() {
        let mut e = engine(
            1.0,
            r#"{"matches":[{"title":"2$"}],"columnWidth":300,"openMaximized":true}"#,
        );
        e.reconcile(system(1.0, &["1", "2"], "2")).unwrap();
        assert_eq!((width(&e, "1"), width(&e, "2")), (600, 1200));
        let mut e = engine(1.0, r#"{"title":"2","openFullscreen":true}"#);
        e.reconcile(system(1.0, &["1", "2"], "2")).unwrap();
        let fullscreen: Vec<_> = e.snapshot.windows.iter().map(|w| w.fullscreen).collect();
        assert_eq!(fullscreen, [false, true]);
    }

    #[test]
    fn open_focused_false_returns_focus_once_and_true_takes_it() {
        let mut e = engine(1.0, r#"{"title":"2","openFocused":false}"#);
        let t = e.reconcile(system(1.0, &["1", "2"], "2")).unwrap();
        assert_eq!(e.snapshot.focused_window.as_deref(), Some("1"));
        assert_eq!(focus_actions(&t), ["1"]);
        let t = e.reconcile(system(1.0, &["1", "2"], "1")).unwrap();
        assert!(focus_actions(&t).is_empty());
        // Later native focus on the window is a user choice and is adopted.
        e.reconcile(system(1.0, &["1", "2"], "2")).unwrap();
        assert_eq!(e.snapshot.focused_window.as_deref(), Some("2"));

        let mut e = engine(1.0, r#"{"title":"2","openFocused":true}"#);
        let t = e.reconcile(system(1.0, &["1", "2"], "1")).unwrap();
        assert_eq!(e.snapshot.focused_window.as_deref(), Some("2"));
        assert_eq!(focus_actions(&t), ["2"]);
    }

    #[test]
    fn open_focused_false_keeps_the_view_on_the_previous_window() {
        let rule = r#"{"title":"2","openFocused":false,"columnWidth":600}"#;
        let mut e = engine(1.0, rule);
        e.dispatch(Command::SetColumnWidth { width: 1200 }).unwrap();
        // The new window opens right of "1" and natively takes focus.
        let t = e.reconcile(system(1.0, &["1", "2"], "2")).unwrap();
        assert_eq!(e.snapshot.focused_window.as_deref(), Some("1"));
        assert_eq!(focus_actions(&t), ["1"]);
        assert_eq!(e.snapshot.monitors[0].pages[0].viewport_x, 0);
        let placed = |id: &str| {
            t.actions
                .iter()
                .find_map(|a| match a {
                    NativeAction::Placement {
                        window_id,
                        rect,
                        clip,
                        minimized,
                    } if window_id == id => Some((rect.x, rect.width, *clip, *minimized)),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(placed("1"), (0, 1200, None, false));
        assert!(placed("2").3);

        // When focus cannot be handed back, the new window keeps it and is revealed.
        let mut e = engine(1.0, rule);
        e.dispatch(Command::SetColumnWidth { width: 1200 }).unwrap();
        e.snapshot.backend.capabilities.focus = false;
        e.reconcile(system(1.0, &["1", "2"], "2")).unwrap();
        assert_eq!(e.snapshot.focused_window.as_deref(), Some("2"));
        assert_eq!(e.snapshot.monitors[0].pages[0].viewport_x, 600);
    }

    #[test]
    fn tabbed_column_applies_the_shown_tab_height_limits() {
        let mut e = engine(1.0, r#"{"title":"2","maxHeight":200}"#);
        e.reconcile(system(1.0, &["1", "2"], "2")).unwrap();
        e.dispatch(Command::MoveWindow {
            direction: Direction::Left,
        })
        .unwrap();
        let t = e.dispatch(Command::ToggleColumnTabbedDisplay).unwrap();
        let rect = |t: &Transition, id: &str| {
            t.actions
                .iter()
                .find_map(|a| match a {
                    NativeAction::Placement {
                        window_id,
                        rect,
                        minimized,
                        ..
                    } if window_id == id => Some((*rect, *minimized)),
                    _ => None,
                })
                .unwrap()
        };
        // 900 tall, 12 px indicator: the capped tab stays at 200 and the rest stays empty.
        let (shown, minimized) = rect(&t, "2");
        assert!(!minimized);
        assert_eq!((shown.y, shown.height), (12, 200));
        // The unlimited tab still fills the column once shown.
        let t = e
            .dispatch(Command::FocusDirection {
                direction: Direction::Up,
            })
            .unwrap();
        let (shown, minimized) = rect(&t, "1");
        assert!(!minimized);
        assert_eq!((shown.y, shown.height), (12, 888));
        assert!(rect(&t, "2").1);
    }

    #[test]
    fn width_limits_are_logical_pixels_and_survive_commands_and_rule_reloads() {
        let mut e = engine(2.0, r#"{"title":"2","minWidth":350,"maxWidth":400}"#);
        e.reconcile(system(2.0, &["1", "2"], "2")).unwrap();
        // Default half viewport 600 is raised to 350 logical = 700 physical.
        assert_eq!(width(&e, "2"), 700);
        e.set_window_rules(vec![]).unwrap();
        e.dispatch(Command::SetColumnWidth { width: 1200 }).unwrap();
        assert_eq!(width(&e, "2"), 800);
        e.dispatch(Command::SetColumnWidth { width: 100 }).unwrap();
        assert_eq!(width(&e, "2"), 700);
        e.set_gaps(10);
        e.dispatch(Command::SetColumnWidth { width: 1200 }).unwrap();
        // The limit is the window; the column adds its two half gaps (10 logical at scale 2).
        assert_eq!(width(&e, "2"), 820);
        assert_eq!(width(&e, "1"), 600);
    }

    #[test]
    fn height_limits_shape_stacked_rows() {
        let mut e = engine(1.0, r#"{"title":"2","maxHeight":200}"#);
        e.reconcile(system(1.0, &["1", "2"], "2")).unwrap();
        e.dispatch(Command::MoveWindow {
            direction: Direction::Left,
        })
        .unwrap();
        let (m, p, column) = e.location("2").unwrap();
        let column = &e.snapshot.monitors[m].pages[p].columns[column.unwrap().0];
        assert_eq!(column.windows, ["1", "2"]);
        assert_eq!(e.column_heights(column, 900), [700, 200]);
        // Growing the limited row is held at its maximum.
        e.dispatch(Command::AdjustWindowHeight { delta: 300 })
            .unwrap();
        let (m, p, column) = e.location("2").unwrap();
        let column = &e.snapshot.monitors[m].pages[p].columns[column.unwrap().0];
        assert_eq!(e.column_heights(column, 900), [700, 200]);
    }

    #[test]
    fn constrained_heights_pin_violations_and_share_the_rest() {
        let max = u32::MAX;
        assert_eq!(
            constrain_heights(900, &[450, 450], &[1, 1], &[max, max]),
            [450, 450]
        );
        assert_eq!(
            constrain_heights(900, &[450, 450], &[600, 1], &[max, max]),
            [600, 300]
        );
        assert_eq!(
            constrain_heights(900, &[300, 300, 300], &[1, 1, 1], &[100, max, max]),
            [100, 400, 400]
        );
        // Minimum wins over a smaller maximum; all at maximum leaves space unused.
        assert_eq!(
            constrain_heights(900, &[450, 450], &[500, 1], &[200, 100]),
            [500, 100]
        );
        // Minimums that cannot fit shrink in proportion.
        assert_eq!(
            constrain_heights(900, &[450, 450], &[600, 1200], &[max, max]),
            [300, 600]
        );
    }
}
