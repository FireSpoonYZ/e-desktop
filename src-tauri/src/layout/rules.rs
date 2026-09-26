use super::*;

impl Engine {
    /// Replace rules atomically. Existing window IDs keep their user-controlled layout.
    pub fn set_window_rules(&mut self, rules: Vec<WindowRule>) -> Result<(), AppError> {
        for rule in &rules {
            rule.validate()?;
        }
        self.window_rules = rules;
        Ok(())
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
        self.snapshot.windows[w].floating = !native.resizable || matched.floating.unwrap_or(false);
        if self.snapshot.windows[w].floating && source != m {
            let source = self.snapshot.monitors[source].viewport;
            let target = self.snapshot.monitors[m].viewport;
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
        self.insert_window(m, p, id, matched.column_width)
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
            600
        );
        assert!(!e.snapshot.windows[e.window_index("4").unwrap()].floating);
    }
}
