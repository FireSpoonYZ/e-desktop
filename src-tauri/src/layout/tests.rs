use super::*;

fn monitor(id: &str, x: i32) -> Monitor {
    Monitor {
        id: id.into(),
        name: id.into(),
        bounds: Rect {
            x,
            y: 0,
            width: 1200,
            height: 900,
        },
        work_area: Rect {
            x,
            y: 0,
            width: 1200,
            height: 900,
        },
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
    let mut engine = Engine::new(BackendStatus {
        kind: BackendKind::Windows,
        availability: BackendAvailability::Ready,
        capabilities: Capabilities {
            enumerate: true,
            placement: true,
            minimize: true,
            clipping: true,
            focus: true,
            close: true,
            ..Capabilities::default()
        },
        message: String::new(),
    });
    assert!(engine.reconcile(system()).unwrap().actions.is_empty());
    engine.dispatch(Command::Enable).unwrap();
    engine
}

fn placement(actions: &[NativeAction], id: &str) -> (Rect, Option<Rect>, bool) {
    actions
        .iter()
        .find_map(|a| match a {
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

#[test]
fn floating_page_restore_ignores_iconic_geometry_including_fullscreen() {
    for fullscreen in [false, true] {
        let mut e = engine();
        e.dispatch(Command::ToggleFloating).unwrap();
        let mut native = system();
        let normal = Rect {
            x: 80,
            y: 90,
            width: 640,
            height: 480,
        };
        native.windows[0].rect = normal;
        e.reconcile(native.clone()).unwrap();
        let original_page = e.snapshot.monitors[0].active_page.clone();
        let expected = if fullscreen {
            e.dispatch(Command::ToggleFullscreen).unwrap();
            native.windows[0].rect = e.snapshot.monitors[0].viewport;
            e.reconcile(native.clone()).unwrap();
            e.snapshot.monitors[0].viewport
        } else {
            normal
        };
        e.dispatch(Command::AddPage {
            monitor_id: "a".into(),
        })
        .unwrap();
        native.focused_window = None;
        native.windows[0].rect = Rect {
            x: -32000,
            y: -32000,
            width: 160,
            height: 30,
        };
        native.windows[0].minimized = true;
        native.windows[0].minimized_by_manager = true;
        native.windows[0].title = "Updated while minimized".into();
        for _ in 0..2 {
            e.reconcile(native.clone()).unwrap();
        }
        assert_eq!(e.snapshot.windows[0].native.rect, expected);
        assert!(e.snapshot.windows[0].native.minimized_by_manager);
        assert_eq!(
            e.snapshot.windows[0].native.title,
            "Updated while minimized"
        );
        let t = e
            .dispatch(Command::SwitchPage {
                monitor_id: "a".into(),
                page_id: original_page,
            })
            .unwrap();
        assert_eq!(placement(&t.actions, "1"), (expected, None, false));
        if fullscreen {
            let t = e.dispatch(Command::ToggleFullscreen).unwrap();
            assert_eq!(placement(&t.actions, "1"), (normal, None, false));
        }
    }
}

#[test]
fn backend_permission_refresh_enables_without_resetting_pages() {
    let ready = engine().snapshot.backend.clone();
    let mut denied = ready.clone();
    denied.availability = BackendAvailability::PermissionRequired;
    denied.capabilities = Capabilities::default();
    denied.message = "Grant native permission".into();
    let mut e = Engine::new(denied.clone());
    e.reconcile(system()).unwrap();
    let pages = serde_json::to_value(&e.snapshot.monitors).unwrap();
    assert_eq!(
        e.dispatch(Command::Enable).unwrap_err().code,
        ErrorCode::PermissionRequired
    );
    e.set_backend(ready.clone());
    assert!(!e.snapshot.enabled);
    assert_eq!(serde_json::to_value(&e.snapshot.monitors).unwrap(), pages);
    assert_eq!(
        serde_json::to_value(&e.snapshot.backend).unwrap(),
        serde_json::to_value(&ready).unwrap()
    );
    assert!(!e.dispatch(Command::Enable).unwrap().actions.is_empty());
    assert_eq!(serde_json::to_value(&e.snapshot.monitors).unwrap(), pages);
    e.dispatch(Command::AddPage {
        monitor_id: "a".into(),
    })
    .unwrap();
    e.dispatch(Command::Disable).unwrap();
    let pages = serde_json::to_value(&e.snapshot.monitors).unwrap();
    e.set_backend(denied);
    assert_eq!(
        e.dispatch(Command::Enable).unwrap_err().code,
        ErrorCode::PermissionRequired
    );
    e.set_backend(ready);
    e.dispatch(Command::Enable).unwrap();
    assert_eq!(serde_json::to_value(&e.snapshot.monitors).unwrap(), pages);
}

#[test]
fn movement_stacks_splits_and_preserves_width_order() {
    let mut e = engine();
    e.dispatch(Command::CycleWidth).unwrap();
    let width = e.snapshot.monitors[0].pages[0].columns[0].width;
    e.dispatch(Command::MoveWindow {
        direction: Direction::Right,
    })
    .unwrap();
    let page = &e.snapshot.monitors[0].pages[0];
    assert_eq!(page.columns.len(), 2);
    assert_eq!(page.columns[0].windows, ["2", "1"]);
    let t = e
        .dispatch(Command::MoveWindow {
            direction: Direction::Up,
        })
        .unwrap();
    assert_eq!(
        e.snapshot.monitors[0].pages[0].columns[0].windows,
        ["1", "2"]
    );
    assert_eq!(placement(&t.actions, "1").0.height, 450);
    assert_eq!(placement(&t.actions, "2").0.y, 450);
    e.dispatch(Command::MoveWindow {
        direction: Direction::Left,
    })
    .unwrap();
    assert_eq!(e.snapshot.monitors[0].pages[0].columns.len(), 3);
    assert_eq!(e.snapshot.monitors[0].pages[0].columns[0].windows, ["1"]);
    // Moving into a column adopts its width; enumeration does not overwrite layout widths/order.
    assert_ne!(e.snapshot.monitors[0].pages[0].columns[0].width, width);
    let before = serde_json::to_value(&e.snapshot.monitors).unwrap();
    e.reconcile(system()).unwrap();
    assert_eq!(serde_json::to_value(&e.snapshot.monitors).unwrap(), before);
}

#[test]
fn close_is_only_a_request_and_manager_minimization_is_not_destruction() {
    let mut e = engine();
    let before = serde_json::to_value(e.snapshot()).unwrap();
    let t = e
        .dispatch(Command::CloseWindow {
            window_id: "1".into(),
        })
        .unwrap();
    assert!(matches!(&t.actions[..], [NativeAction::Close { window_id }] if window_id == "1"));
    assert_eq!(serde_json::to_value(e.snapshot()).unwrap(), before);
    let mut native = system();
    native.windows[0].minimized = true;
    native.windows[0].minimized_by_manager = true;
    e.reconcile(native.clone()).unwrap();
    assert_eq!(e.snapshot.windows.len(), 4);
    native.windows.remove(0);
    native.focused_window = None;
    let t = e.reconcile(native).unwrap();
    assert!(
        matches!(t.actions.last(), Some(NativeAction::Focus { window_id }) if window_id == "2")
    );
    assert_eq!(e.snapshot.windows.len(), 3);
    assert_eq!(e.snapshot.monitors[0].pages[0].columns.len(), 2);
    assert_eq!(e.snapshot.focused_window.as_deref(), Some("2"));
    let mut native = system();
    native.windows.clear();
    native.focused_window = None;
    e.reconcile(native).unwrap();
    assert!(
        e.snapshot
            .monitors
            .iter()
            .all(|m| m.pages.len() == 1 && empty(&m.pages[0]))
    );
}

#[test]
fn focus_scroll_clipping_and_independent_monitors() {
    let mut e = engine();
    let other = serde_json::to_value(&e.snapshot.monitors[1]).unwrap();
    let t = e
        .dispatch(Command::FocusWindow {
            window_id: "3".into(),
        })
        .unwrap();
    assert_eq!(e.snapshot.monitors[0].pages[0].viewport_x, 600);
    assert_eq!(placement(&t.actions, "3").0.x, 600);
    assert!(placement(&t.actions, "1").2);
    assert_eq!(
        serde_json::to_value(&e.snapshot.monitors[1]).unwrap(),
        other
    );
    let t = e
        .dispatch(Command::Scroll {
            monitor_id: "a".into(),
            delta: -300,
        })
        .unwrap();
    assert_eq!(placement(&t.actions, "1").1.unwrap().width, 300);
    e.reconcile(system()).unwrap(); // Actual focus changes to 1; follows it into view.
    assert_eq!(e.snapshot.monitors[0].pages[0].viewport_x, 0);
    e.dispatch(Command::CenterFocused).unwrap();
    assert_eq!(e.snapshot.monitors[0].pages[0].viewport_x, -300);
    e.dispatch(Command::FocusDirection {
        direction: Direction::Right,
    })
    .unwrap();
    assert_eq!(e.snapshot.focused_window.as_deref(), Some("2"));
    assert_eq!(e.snapshot.monitors[0].pages[0].viewport_x, 0);
}

#[test]
fn pages_move_across_monitors_cleanup_and_stale_focus() {
    let mut e = engine();
    let a_first = e.snapshot.monitors[0].active_page.clone();
    let b_empty = e.snapshot.monitors[1].pages.last().unwrap().id.clone();
    e.dispatch(Command::MoveWindowToPage {
        window_id: "1".into(),
        page_id: b_empty.clone(),
    })
    .unwrap();
    assert_eq!(e.location("1").unwrap().0, 1);
    assert_eq!(e.snapshot.monitors[1].active_page, b_empty);
    assert_eq!(e.snapshot.monitors[1].pages.len(), 3);
    let t = e
        .dispatch(Command::AddPage {
            monitor_id: "b".into(),
        })
        .unwrap();
    assert!(placement(&t.actions, "1").2);
    assert_eq!(e.snapshot.focused_window, None);
    let selected_empty = e.snapshot.monitors[1].active_page.clone();
    e.reconcile(system()).unwrap(); // Stale native focus cannot reactivate the hidden page.
    assert_eq!(e.snapshot.monitors[1].active_page, selected_empty);
    assert_eq!(e.location("1").unwrap().0, 1); // Native monitor association is not logical assignment.
    e.dispatch(Command::SwitchPage {
        monitor_id: "a".into(),
        page_id: a_first.clone(),
    })
    .unwrap();
    assert_eq!(e.snapshot.focused_window.as_deref(), Some("2"));
    e.dispatch(Command::MoveWindowToPage {
        window_id: "1".into(),
        page_id: a_first,
    })
    .unwrap();
    assert_eq!(e.snapshot.monitors[1].pages.len(), 2);
}

#[test]
fn invalid_commands_are_atomic_and_disabled_never_places() {
    let mut e = engine();
    let before = serde_json::to_value(e.snapshot()).unwrap();
    let bad = [
        Command::FocusWindow {
            window_id: "gone".into(),
        },
        Command::MoveWindowToPage {
            window_id: "1".into(),
            page_id: "gone".into(),
        },
        Command::SwitchPage {
            monitor_id: "a".into(),
            page_id: e.snapshot.monitors[1].active_page.clone(),
        },
    ];
    for command in bad {
        assert!(e.dispatch(command).is_err());
        assert_eq!(serde_json::to_value(e.snapshot()).unwrap(), before);
    }
    let mut invalid_system = system();
    invalid_system.windows.push(window("1", "a"));
    assert!(e.reconcile(invalid_system).is_err());
    assert_eq!(serde_json::to_value(e.snapshot()).unwrap(), before);
    e.set_viewports(BTreeMap::from([("a".into(), Rect::default())]));
    assert!(e.reconcile(system()).is_err());
    assert_eq!(serde_json::to_value(e.snapshot()).unwrap(), before);
    e.set_viewports(BTreeMap::new());
    let t = e.dispatch(Command::Disable).unwrap();
    assert_eq!(t.actions.len(), 4);
    assert!(
        t.actions
            .iter()
            .all(|a| matches!(a, NativeAction::Restore { .. }))
    );
    assert!(e.dispatch(Command::CycleWidth).is_err());
    assert!(e.reconcile(system()).unwrap().actions.is_empty());
    assert!(e.dispatch(Command::Disable).unwrap().actions.is_empty());
    assert!(!e.dispatch(Command::Enable).unwrap().actions.is_empty());
    assert!(
        Engine::new(BackendStatus::default())
            .dispatch(Command::Enable)
            .is_err()
    );
}

#[test]
fn fullscreen_floating_reservations_and_user_minimization() {
    let mut e = engine();
    let t = e.dispatch(Command::ToggleFullscreen).unwrap();
    assert_eq!(
        placement(&t.actions, "1").0,
        e.snapshot.monitors[0].viewport
    );
    assert!(placement(&t.actions, "2").2);
    assert!(!placement(&t.actions, "4").2);
    e.dispatch(Command::ToggleFullscreen).unwrap();
    let t = e.dispatch(Command::ToggleFloating).unwrap();
    assert!(e.snapshot.windows[0].floating);
    assert_eq!(placement(&t.actions, "1").0, system().windows[0].rect);
    e.dispatch(Command::ToggleFullscreen).unwrap();
    let mut native = system();
    native.windows[0].rect = e.snapshot.monitors[0].viewport;
    e.reconcile(native).unwrap();
    let t = e.dispatch(Command::ToggleFullscreen).unwrap();
    assert_eq!(placement(&t.actions, "1").0, system().windows[0].rect);
    let target_page = e.snapshot.monitors[1].active_page.clone();
    let t = e
        .dispatch(Command::MoveWindowToPage {
            window_id: "1".into(),
            page_id: target_page,
        })
        .unwrap();
    assert_eq!(placement(&t.actions, "1").0.x, -1160);
    let original_page = e.snapshot.monitors[0].active_page.clone();
    e.dispatch(Command::MoveWindowToPage {
        window_id: "1".into(),
        page_id: original_page,
    })
    .unwrap();
    let viewport = Rect {
        x: 56,
        y: 44,
        width: 1144,
        height: 856,
    };
    e.set_viewports(BTreeMap::from([("a".into(), viewport)]));
    let mut native = system();
    native.windows[1].minimized = true;
    let t = e.reconcile(native).unwrap();
    assert_eq!(e.snapshot.monitors[0].viewport, viewport);
    assert_eq!(
        e.snapshot.monitors[0].monitor.work_area,
        system().monitors[0].work_area
    );
    assert!(
        !t.actions
            .iter()
            .any(|a| matches!(a, NativeAction::Placement { window_id, .. } if window_id == "2"))
    );
    e.dispatch(Command::ToggleFloating).unwrap();
    assert!(!e.snapshot.windows[0].floating);
}

#[test]
fn no_clipping_uses_whole_window_visibility_and_focus_recovers_edges() {
    let mut e = engine();
    e.dispatch(Command::Disable).unwrap();
    e.snapshot.backend.capabilities.clipping = false;
    e.dispatch(Command::Enable).unwrap();
    let t = e
        .dispatch(Command::Scroll {
            monitor_id: "a".into(),
            delta: 300,
        })
        .unwrap();
    assert!(placement(&t.actions, "1").2);
    assert!(!placement(&t.actions, "2").2);
    assert!(placement(&t.actions, "3").2);
    assert!(
        t.actions
            .iter()
            .all(|action| !matches!(action, NativeAction::Placement { clip: Some(_), .. }))
    );
    let t = e
        .dispatch(Command::FocusWindow {
            window_id: "3".into(),
        })
        .unwrap();
    let (rect, clip, minimized) = placement(&t.actions, "3");
    assert!(!minimized);
    assert_eq!(clip, None);
    assert!(rect.x >= 0 && rect.x as i64 + rect.width as i64 <= 1200);
    e.dispatch(Command::CycleWidth).unwrap();
    e.set_viewports(BTreeMap::from([(
        "a".into(),
        Rect {
            x: 0,
            y: 0,
            width: 400,
            height: 900,
        },
    )]));
    let mut native = system();
    native.focused_window = Some("3".into());
    let t = e.reconcile(native).unwrap();
    let (rect, clip, minimized) = placement(&t.actions, "3");
    assert_eq!(rect.width, 400);
    assert_eq!(rect.x, 0);
    assert_eq!(clip, None);
    assert!(!minimized);
    assert!(
        e.snapshot.monitors[0]
            .pages
            .iter()
            .flat_map(|page| &page.columns)
            .all(|column| column.width <= 400)
    );
    let destination = e.snapshot.monitors[0].active_page.clone();
    e.dispatch(Command::MoveWindowToPage {
        window_id: "4".into(),
        page_id: destination,
    })
    .unwrap();
    let (m, p, column) = e.location("4").unwrap();
    assert_eq!(
        e.snapshot.monitors[m].pages[p].columns[column.unwrap().0].width,
        400
    );
}
