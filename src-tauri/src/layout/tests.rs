use super::*;

#[test]
fn sizing_commands_use_the_frozen_physical_pixel_ipc_contract() {
    for (command, value) in [
        (
            Command::SetColumnWidth { width: u32::MAX },
            serde_json::json!({"type":"setColumnWidth","width":u32::MAX}),
        ),
        (
            Command::AdjustColumnWidth { delta: i32::MIN },
            serde_json::json!({"type":"adjustColumnWidth","delta":i32::MIN}),
        ),
        (
            Command::AdjustWindowHeight { delta: i32::MAX },
            serde_json::json!({"type":"adjustWindowHeight","delta":i32::MAX}),
        ),
        (
            Command::ResetWindowHeights,
            serde_json::json!({"type":"resetWindowHeights"}),
        ),
    ] {
        assert_eq!(serde_json::to_value(&command).unwrap(), value);
        assert_eq!(serde_json::from_value::<Command>(value).unwrap(), command);
    }
    for value in [
        serde_json::json!({"type":"setColumnWidth","width":-1}),
        serde_json::json!({"type":"setColumnWidth","width":4294967296_u64}),
        serde_json::json!({"type":"adjustColumnWidth","delta":2147483648_i64}),
        serde_json::json!({"type":"adjustWindowHeight","delta":-2147483649_i64}),
    ] {
        assert!(serde_json::from_value::<Command>(value).is_err());
    }
}

fn stacked_engine() -> Engine {
    let mut e = engine();
    e.dispatch(Command::MoveWindow {
        direction: Direction::Right,
    })
    .unwrap();
    e.dispatch(Command::FocusWindow {
        window_id: "3".into(),
    })
    .unwrap();
    e.dispatch(Command::MoveWindow {
        direction: Direction::Left,
    })
    .unwrap();
    e.dispatch(Command::FocusWindow {
        window_id: "1".into(),
    })
    .unwrap();
    assert_eq!(
        e.snapshot.monitors[0].pages[0].columns[0].windows,
        ["2", "1", "3"]
    );
    e
}

fn assert_column_coverage(e: &Engine, actions: &[NativeAction]) {
    for monitor in &e.snapshot.monitors {
        for column in monitor.pages.iter().flat_map(|p| &p.columns) {
            let mut y = monitor.viewport.y as i64;
            for id in &column.windows {
                let rect = placement(actions, id).0;
                assert!(rect.height > 0);
                assert_eq!(rect.y as i64, y);
                assert_eq!(rect.width, column.width);
                y += rect.height as i64;
            }
            assert_eq!(
                y,
                monitor.viewport.y as i64 + monitor.viewport.height as i64
            );
        }
    }
}

fn assert_rejected_unchanged(e: &mut Engine, command: Command) {
    let before = serde_json::to_value(e.snapshot()).unwrap();
    let weights = e.height_weights.clone();
    let next_id = e.next_id;
    let page_focus = e.page_focus.clone();
    let fullscreen_restore = e.fullscreen_restore.clone();
    let placements = serde_json::to_value(e.placements().unwrap()).unwrap();
    let error = e.dispatch(command).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidCommand);
    assert!(!error.message.is_empty());
    assert_eq!(serde_json::to_value(e.snapshot()).unwrap(), before);
    assert_eq!(e.height_weights, weights);
    assert_eq!(e.next_id, next_id);
    assert_eq!(e.page_focus, page_focus);
    assert_eq!(e.fullscreen_restore, fullscreen_restore);
    assert_eq!(
        serde_json::to_value(e.placements().unwrap()).unwrap(),
        placements
    );
}

#[test]
fn sizing_rejects_paused_unfocused_floating_and_fullscreen_atomically() {
    for state in 0..4 {
        let mut e = stacked_engine();
        e.dispatch(Command::AdjustWindowHeight { delta: 100 })
            .unwrap();
        match state {
            0 => {
                e.dispatch(Command::Disable).unwrap();
            }
            1 => {
                e.dispatch(Command::AddPage {
                    monitor_id: "a".into(),
                })
                .unwrap();
            }
            2 => {
                e.dispatch(Command::ToggleFloating).unwrap();
            }
            3 => {
                e.dispatch(Command::ToggleFullscreen).unwrap();
            }
            _ => unreachable!(),
        }
        for command in [
            Command::SetColumnWidth { width: 700 },
            Command::AdjustColumnWidth { delta: -50 },
            Command::AdjustWindowHeight { delta: 50 },
            Command::ResetWindowHeights,
            Command::CycleWidth,
        ] {
            assert_rejected_unchanged(&mut e, command);
        }
    }
}

#[test]
fn column_widths_clamp_extreme_pixels_without_focus_or_monitor_changes() {
    let mut e = engine();
    e.dispatch(Command::FocusWindow {
        window_id: "3".into(),
    })
    .unwrap();
    let other = serde_json::to_value(&e.snapshot.monitors[1]).unwrap();
    // Sizing needs placement, not native focus capability.
    e.snapshot.backend.capabilities.focus = false;
    assert_rejected_unchanged(&mut e, Command::SetColumnWidth { width: 0 });
    for (command, width) in [
        (Command::SetColumnWidth { width: 1 }, 1),
        (Command::AdjustColumnWidth { delta: i32::MIN }, 1),
        (Command::AdjustColumnWidth { delta: i32::MAX }, 1200),
        (Command::SetColumnWidth { width: u32::MAX }, 1200),
        (Command::SetColumnWidth { width: 777 }, 777),
        (Command::AdjustColumnWidth { delta: -7 }, 770),
        (Command::AdjustColumnWidth { delta: 0 }, 770),
        (Command::CycleWidth, 800),
    ] {
        let t = e.dispatch(command).unwrap();
        let (rect, clip, minimized) = placement(&t.actions, "3");
        assert_eq!(rect.width, width);
        assert!(!minimized);
        assert_eq!(clip, None);
        assert!(rect.x >= 0 && rect.x as i64 + width as i64 <= 1200);
        assert_eq!(e.snapshot.focused_window.as_deref(), Some("3"));
        assert!(
            !t.actions
                .iter()
                .any(|a| matches!(a, NativeAction::Focus { .. }))
        );
        assert_eq!(
            serde_json::to_value(&e.snapshot.monitors[1]).unwrap(),
            other
        );
    }
    let mut native = system();
    native.focused_window = Some("3".into());
    let t = e.reconcile(native).unwrap();
    assert_eq!(placement(&t.actions, "3").0.width, 800);
}

#[test]
fn window_height_adjustments_cover_exactly_and_reset_only_the_current_column() {
    let mut e = stacked_engine();
    e.dispatch(Command::Scroll {
        monitor_id: "a".into(),
        delta: -100,
    })
    .unwrap();
    let scroll = e.snapshot.monitors[0].pages[0].viewport_x;
    for (delta, height) in [
        (101, 401),
        (-51, 350),
        (0, 350),
        (i32::MAX, 898),
        (i32::MIN, 1),
    ] {
        let t = e.dispatch(Command::AdjustWindowHeight { delta }).unwrap();
        assert_eq!(placement(&t.actions, "1").0.height, height);
        assert_column_coverage(&e, &t.actions);
        assert_eq!(e.snapshot.focused_window.as_deref(), Some("1"));
        assert_eq!(e.snapshot.monitors[0].pages[0].viewport_x, scroll);
        assert!(
            !t.actions
                .iter()
                .any(|a| matches!(a, NativeAction::Focus { .. }))
        );
    }
    let weights = e.height_weights.clone();
    e.dispatch(Command::FocusWindow {
        window_id: "4".into(),
    })
    .unwrap();
    let t = e
        .dispatch(Command::AdjustWindowHeight { delta: i32::MIN })
        .unwrap();
    assert_eq!(placement(&t.actions, "4").0.height, 900);
    e.dispatch(Command::ResetWindowHeights).unwrap();
    assert_eq!(e.height_weights, weights);
    e.dispatch(Command::FocusWindow {
        window_id: "1".into(),
    })
    .unwrap();
    let t = e.dispatch(Command::ResetWindowHeights).unwrap();
    assert!(e.height_weights.is_empty());
    for id in ["1", "2", "3"] {
        assert_eq!(placement(&t.actions, id).0.height, 300);
    }
    assert_column_coverage(&e, &t.actions);
}

#[test]
fn height_preferences_follow_reorder_split_merge_viewport_and_close() {
    let mut e = stacked_engine();
    e.dispatch(Command::AdjustWindowHeight { delta: 100 })
        .unwrap();
    let weights = e.height_weights.clone();
    let t = e
        .dispatch(Command::MoveWindow {
            direction: Direction::Up,
        })
        .unwrap();
    assert_eq!(placement(&t.actions, "1").0.height, 400);
    assert_eq!(placement(&t.actions, "1").0.y, 0);
    assert_column_coverage(&e, &t.actions);
    let t = e
        .dispatch(Command::MoveWindow {
            direction: Direction::Left,
        })
        .unwrap();
    assert_eq!(placement(&t.actions, "1").0.height, 900);
    assert_column_coverage(&e, &t.actions);
    let t = e
        .dispatch(Command::MoveWindow {
            direction: Direction::Right,
        })
        .unwrap();
    assert_eq!(placement(&t.actions, "1").0.height, 400);
    assert_column_coverage(&e, &t.actions);
    assert_eq!(e.height_weights, weights);
    let viewport = Rect {
        x: 23,
        y: 71,
        width: 1000,
        height: 601,
    };
    e.set_viewports(BTreeMap::from([("a".into(), viewport)]));
    let t = e.reconcile(system()).unwrap();
    assert_column_coverage(&e, &t.actions);
    assert!(placement(&t.actions, "1").0.height > placement(&t.actions, "2").0.height);
    assert_eq!(e.height_weights, weights);
    e.set_viewports(BTreeMap::new());
    let mut native = system();
    native.monitors[0].bounds.height = 1301;
    native.monitors[0].work_area.height = 1301;
    let t = e.reconcile(native.clone()).unwrap();
    assert_column_coverage(&e, &t.actions);
    assert!(placement(&t.actions, "1").0.height > placement(&t.actions, "2").0.height);
    e.dispatch(Command::CloseWindow {
        window_id: "2".into(),
    })
    .unwrap();
    assert_eq!(e.height_weights, weights); // Close may be cancelled.
    native.windows.retain(|w| w.id != "2");
    let t = e.reconcile(native.clone()).unwrap();
    assert!(!e.height_weights.contains_key("2"));
    assert_column_coverage(&e, &t.actions);
    assert!(placement(&t.actions, "1").0.height > placement(&t.actions, "3").0.height);
    let t = e.dispatch(Command::ResetWindowHeights).unwrap();
    assert_column_coverage(&e, &t.actions);
    let a = placement(&t.actions, "1").0.height;
    let b = placement(&t.actions, "3").0.height;
    assert_eq!(a.abs_diff(b), 1);
    native.windows.retain(|w| w.id != "1");
    native.focused_window = Some("3".into());
    let t = e.reconcile(native).unwrap();
    assert_eq!(placement(&t.actions, "3").0.height, 1301);
    assert_column_coverage(&e, &t.actions);
}

#[test]
fn sizing_handles_maximum_viewport_dimensions_without_overflow() {
    let mut e = stacked_engine();
    let mut native = system();
    let area = Rect {
        x: i32::MIN,
        y: i32::MIN,
        width: u32::MAX,
        height: u32::MAX,
    };
    native.monitors[0].bounds = area;
    native.monitors[0].work_area = area;
    e.reconcile(native).unwrap();
    e.dispatch(Command::SetColumnWidth { width: u32::MAX })
        .unwrap();
    // A lone column always fills the viewport, whatever the requested width.
    for delta in [i32::MAX, i32::MIN, i32::MIN, i32::MAX] {
        let t = e.dispatch(Command::AdjustColumnWidth { delta }).unwrap();
        assert_eq!(placement(&t.actions, "1").0.width, u32::MAX);
    }
    for delta in [i32::MAX, i32::MIN, i32::MIN, i32::MAX] {
        let t = e.dispatch(Command::AdjustWindowHeight { delta }).unwrap();
        assert_column_coverage(&e, &t.actions);
    }
}

#[test]
fn height_preferences_survive_minimum_viewport_and_give_new_siblings_a_fair_share() {
    let mut e = stacked_engine();
    e.dispatch(Command::AdjustWindowHeight { delta: 100 })
        .unwrap();
    let weights = e.height_weights.clone();
    e.set_viewports(BTreeMap::from([(
        "a".into(),
        Rect {
            x: 0,
            y: 0,
            width: 1200,
            height: 3,
        },
    )]));
    let t = e.reconcile(system()).unwrap();
    assert_column_coverage(&e, &t.actions);
    e.dispatch(Command::AdjustWindowHeight { delta: i32::MAX })
        .unwrap();
    assert_eq!(e.height_weights, weights);
    e.set_viewports(BTreeMap::new());
    let mut native = system();
    native.windows.push(window("5", "a"));
    let t = e.reconcile(native).unwrap();
    assert_eq!(placement(&t.actions, "1").0.height, 400);
    e.dispatch(Command::FocusWindow {
        window_id: "5".into(),
    })
    .unwrap();
    let t = e
        .dispatch(Command::MoveWindow {
            direction: Direction::Left,
        })
        .unwrap();
    assert_column_coverage(&e, &t.actions);
    assert!((224..=226).contains(&placement(&t.actions, "5").0.height));
    assert_eq!(e.height_weights, weights);
}

#[test]
fn insufficient_height_rejects_reconcile_and_merge_without_partial_changes() {
    let mut e = stacked_engine();
    e.dispatch(Command::AdjustWindowHeight { delta: 100 })
        .unwrap();
    let before = serde_json::to_value(e.snapshot()).unwrap();
    let weights = e.height_weights.clone();
    e.set_viewports(BTreeMap::from([(
        "a".into(),
        Rect {
            x: 0,
            y: 0,
            width: 1200,
            height: 2,
        },
    )]));
    assert_eq!(
        e.reconcile(system()).unwrap_err().code,
        ErrorCode::InvalidCommand
    );
    assert_eq!(serde_json::to_value(e.snapshot()).unwrap(), before);
    assert_eq!(e.height_weights, weights);
    e.dispatch(Command::Disable).unwrap();
    let before = serde_json::to_value(e.snapshot()).unwrap();
    assert_eq!(
        e.reconcile(system()).unwrap_err().code,
        ErrorCode::InvalidCommand
    );
    assert_eq!(serde_json::to_value(e.snapshot()).unwrap(), before);
    let mut e = engine();
    e.set_viewports(BTreeMap::from([(
        "a".into(),
        Rect {
            x: 0,
            y: 0,
            width: 1200,
            height: 2,
        },
    )]));
    e.reconcile(system()).unwrap();
    let t = e
        .dispatch(Command::MoveWindow {
            direction: Direction::Right,
        })
        .unwrap();
    assert_column_coverage(&e, &t.actions);
    let t = e
        .dispatch(Command::AdjustWindowHeight { delta: i32::MAX })
        .unwrap();
    assert_column_coverage(&e, &t.actions);
    assert_eq!(placement(&t.actions, "1").0.height, 1);
    e.dispatch(Command::FocusWindow {
        window_id: "3".into(),
    })
    .unwrap();
    assert_rejected_unchanged(
        &mut e,
        Command::MoveWindow {
            direction: Direction::Left,
        },
    );
}

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

pub(super) fn engine() -> Engine {
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
    engine.set_gaps(0);
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
    // Scrolling settles on a column edge: window 1 is back in full, unclipped.
    assert_eq!(e.snapshot.monitors[0].pages[0].viewport_x, 0);
    assert_eq!(
        placement(&t.actions, "1"),
        (placement(&t.actions, "1").0, None, false)
    );
    e.reconcile(system()).unwrap(); // Actual focus changes to 1; follows it into view.
    assert_eq!(e.snapshot.monitors[0].pages[0].viewport_x, 0);
    e.dispatch(Command::CenterFocused).unwrap();
    // The strip never leaves empty space, so centering the first column is clamped.
    assert_eq!(e.snapshot.monitors[0].pages[0].viewport_x, 0);
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
    // Scroll snaps to the next edge: columns 2 and 3 fill the screen.
    assert!(placement(&t.actions, "1").2);
    assert!(!placement(&t.actions, "2").2);
    assert!(!placement(&t.actions, "3").2);
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

fn hotplug_system(outputs: &[&str], closed: &[&str]) -> SystemSnapshot {
    let mut native = system();
    native.monitors = outputs
        .iter()
        .map(|id| {
            monitor(
                id,
                match *id {
                    "a" => 0,
                    "b" => -1200,
                    _ => 1200,
                },
            )
        })
        .collect();
    native.windows.retain(|w| !closed.contains(&w.id.as_str()));
    for window in &mut native.windows {
        if !outputs.contains(&window.monitor_id.as_str()) {
            window.monitor_id = outputs[0].into();
        }
    }
    native.focused_window = None;
    native
}

fn assert_hotplug_consistent(e: &Engine) {
    let mut assigned = BTreeSet::new();
    for monitor in &e.snapshot.monitors {
        assert!(monitor.pages.iter().any(|p| p.id == monitor.active_page));
        assert!(empty(monitor.pages.last().unwrap()));
        for page in &monitor.pages {
            for id in ids(page) {
                assert!(assigned.insert(id.clone()), "duplicate {id}");
                assert!(e.window_index(id).is_ok());
            }
            assert_eq!(
                page.viewport_x,
                clamp_scroll(page, monitor.viewport.width, page.viewport_x as i64)
            );
            for column in &page.columns {
                assert!(!column.windows.is_empty());
                assert!((1..=monitor.viewport.width).contains(&column.width));
            }
        }
    }
    assert_eq!(
        assigned,
        e.snapshot
            .windows
            .iter()
            .map(|w| w.native.id.clone())
            .collect()
    );
    if let Some(id) = &e.snapshot.focused_window {
        let (m, p, _) = e.location(id).unwrap();
        assert_eq!(
            e.snapshot.active_monitor.as_ref(),
            Some(&e.snapshot.monitors[m].monitor.id)
        );
        assert_eq!(
            e.snapshot.monitors[m].active_page,
            e.snapshot.monitors[m].pages[p].id
        );
    }
}

#[test]
fn hotplug_roundtrip_preserves_pages_columns_floating_scroll_and_background_focus() {
    let mut e = stacked_engine();
    e.dispatch(Command::FocusWindow {
        window_id: "3".into(),
    })
    .unwrap();
    let tail = e.snapshot.monitors[0].pages.last().unwrap().id.clone();
    e.dispatch(Command::MoveWindowToPage {
        window_id: "3".into(),
        page_id: tail,
    })
    .unwrap();
    e.dispatch(Command::ToggleFloating).unwrap();
    e.dispatch(Command::FocusWindow {
        window_id: "1".into(),
    })
    .unwrap();
    e.dispatch(Command::AdjustWindowHeight { delta: 100 })
        .unwrap();
    e.dispatch(Command::CenterFocused).unwrap();
    e.dispatch(Command::FocusWindow {
        window_id: "4".into(),
    })
    .unwrap();
    let before = serde_json::to_value(&e.snapshot.monitors[0].pages).unwrap();
    let weights = e.height_weights.clone();
    for _ in 0..2 {
        let t = e.reconcile(hotplug_system(&["b"], &[])).unwrap();
        assert_eq!(e.snapshot.focused_window.as_deref(), Some("4"));
        assert!(placement(&t.actions, "1").2);
        assert!(placement(&t.actions, "3").2);
        assert!(
            !t.actions
                .iter()
                .any(|a| matches!(a, NativeAction::Focus { .. }))
        );
        assert_hotplug_consistent(&e);
        let t = e.reconcile(hotplug_system(&["b", "a"], &[])).unwrap();
        assert_eq!(e.snapshot.monitors[0].monitor.id, "a");
        assert_eq!(
            serde_json::to_value(&e.snapshot.monitors[0].pages).unwrap(),
            before
        );
        assert_eq!(e.height_weights, weights);
        assert_eq!(e.snapshot.focused_window.as_deref(), Some("4"));
        assert!(
            !t.actions
                .iter()
                .any(|a| matches!(a, NativeAction::Focus { .. }))
        );
        assert_hotplug_consistent(&e);
    }
}

#[test]
fn hotplug_focused_page_follows_migration_without_native_refocus() {
    let mut e = engine();
    e.dispatch(Command::FocusWindow {
        window_id: "4".into(),
    })
    .unwrap();
    let page = e.snapshot.monitors[1].active_page.clone();
    for outputs in [vec!["a"], vec!["b", "a"], vec!["b"], vec!["a", "b"]] {
        let t = e.reconcile(hotplug_system(&outputs, &[])).unwrap();
        let (m, p, _) = e.location("4").unwrap();
        assert_eq!(e.snapshot.monitors[m].pages[p].id, page);
        assert!(!placement(&t.actions, "4").2);
        assert!(
            !t.actions
                .iter()
                .any(|a| matches!(a, NativeAction::Focus { .. }))
        );
        assert_hotplug_consistent(&e);
    }
}

#[test]
fn hotplug_focused_edge_column_stays_visible_on_an_unchanged_smaller_receiver() {
    let mut e = engine();
    e.set_viewports(BTreeMap::from([(
        "b".into(),
        Rect {
            x: -1200,
            y: 0,
            width: 400,
            height: 900,
        },
    )]));
    e.reconcile(system()).unwrap();
    e.dispatch(Command::FocusWindow {
        window_id: "3".into(),
    })
    .unwrap();
    let t = e.reconcile(hotplug_system(&["b"], &[])).unwrap();
    assert!(!placement(&t.actions, "3").2);
    assert_eq!(placement(&t.actions, "3").0.width, 400);
    assert!(
        !t.actions
            .iter()
            .any(|a| matches!(a, NativeAction::Focus { .. }))
    );
    assert_hotplug_consistent(&e);
}

#[test]
fn hotplug_manual_moves_in_and_out_of_borrowed_pages_are_not_reclaimed() {
    let mut e = engine();
    let borrowed = e.snapshot.monitors[1].active_page.clone();
    e.reconcile(hotplug_system(&["a"], &[])).unwrap();
    e.dispatch(Command::MoveWindowToPage {
        window_id: "1".into(),
        page_id: borrowed.clone(),
    })
    .unwrap();
    let destination = e.snapshot.monitors[0].pages.last().unwrap().id.clone();
    e.dispatch(Command::MoveWindowToPage {
        window_id: "4".into(),
        page_id: destination,
    })
    .unwrap();
    // Even moving back into the original page is now explicit host ownership.
    e.dispatch(Command::MoveWindowToPage {
        window_id: "4".into(),
        page_id: borrowed,
    })
    .unwrap();
    e.reconcile(hotplug_system(&["b", "a"], &[])).unwrap();
    for id in ["1", "4"] {
        let (m, _, _) = e.location(id).unwrap();
        assert_eq!(e.snapshot.monitors[m].monitor.id, "a");
    }
    assert_hotplug_consistent(&e);
}

#[test]
fn hotplug_multiple_disconnects_reconnect_order_and_closure_preserve_first_owner() {
    for reconnect in [vec!["b", "a", "c"], vec!["c", "a", "b"]] {
        let mut e = engine();
        let a_page = e.snapshot.monitors[0].pages[0].id.clone();
        let b_page = e.snapshot.monitors[1].pages[0].id.clone();
        e.reconcile(hotplug_system(&["a", "b", "c"], &[])).unwrap();
        e.reconcile(hotplug_system(&["a", "c"], &[])).unwrap();
        // Pin 1 to a, even though its page originally came from b.
        e.dispatch(Command::MoveWindowToPage {
            window_id: "1".into(),
            page_id: b_page.clone(),
        })
        .unwrap();
        e.reconcile(hotplug_system(&["c"], &["2"])).unwrap();
        assert_hotplug_consistent(&e);
        // Restore b first while a is still absent: the pinned fragment must later return to a.
        e.reconcile(hotplug_system(&["b", "c"], &["2"])).unwrap();
        e.reconcile(hotplug_system(&reconnect, &["2"])).unwrap();
        for (id, owner) in [("1", "a"), ("3", "a"), ("4", "b")] {
            let (m, _, _) = e.location(id).unwrap();
            assert_eq!(e.snapshot.monitors[m].monitor.id, owner);
        }
        let (m, p, _) = e.location("3").unwrap();
        assert_eq!(e.snapshot.monitors[m].pages[p].id, a_page);
        let (m, p, _) = e.location("4").unwrap();
        assert_eq!(e.snapshot.monitors[m].pages[p].id, b_page);
        assert!(e.window_index("2").is_err());
        assert!(e.disconnected_monitors.is_empty());
        assert_hotplug_consistent(&e);
        let pages = serde_json::to_value(&e.snapshot.monitors).unwrap();
        e.reconcile(hotplug_system(&["c", "b", "a"], &["2"]))
            .unwrap();
        assert_eq!(serde_json::to_value(&e.snapshot.monitors).unwrap(), pages);
    }
}

#[test]
fn hotplug_empty_outputs_suspend_actions_and_commands_until_valid_enumeration() {
    let mut e = engine();
    let before = serde_json::to_value(e.snapshot()).unwrap();
    for _ in 0..2 {
        let t = e.reconcile(SystemSnapshot::default()).unwrap();
        assert!(t.actions.is_empty());
        assert!(e.outputs_suspended);
        assert_eq!(serde_json::to_value(e.snapshot()).unwrap(), before);
    }
    for command in [
        Command::Enable,
        Command::CycleWidth,
        Command::MoveWindowToPage {
            window_id: "1".into(),
            page_id: e.snapshot.monitors[1].active_page.clone(),
        },
    ] {
        assert_rejected_unchanged(&mut e, command);
    }
    assert!(e.dispatch(Command::Refresh).unwrap().actions.is_empty());
    assert!(
        e.reconcile(SystemSnapshot {
            windows: vec![window("1", "a")],
            ..SystemSnapshot::default()
        })
        .is_err()
    );
    assert!(e.outputs_suspended);
    e.reconcile(hotplug_system(&["a", "b"], &["2"])).unwrap();
    assert!(!e.outputs_suspended);
    assert!(e.window_index("2").is_err());
    assert_hotplug_consistent(&e);
    e.reconcile(SystemSnapshot::default()).unwrap();
    let t = e.dispatch(Command::Disable).unwrap();
    assert_eq!(t.actions.len(), 3);
    assert!(
        t.actions
            .iter()
            .all(|a| matches!(a, NativeAction::Restore { .. }))
    );
    assert!(e.dispatch(Command::Enable).is_err());
    assert!(
        e.reconcile(hotplug_system(&["b", "a"], &["2"]))
            .unwrap()
            .actions
            .is_empty()
    );
    assert!(!e.snapshot.enabled);
}

#[test]
fn hotplug_empty_monitor_restores_stable_page_and_closed_ids_never_resurrect() {
    let mut e = engine();
    e.reconcile(hotplug_system(&["a", "b"], &["4"])).unwrap();
    let empty_page = e.snapshot.monitors[1].active_page.clone();
    e.reconcile(hotplug_system(&["a"], &["4"])).unwrap();
    e.reconcile(hotplug_system(&["b", "a"], &["4"])).unwrap();
    assert_eq!(e.snapshot.monitors[1].pages.len(), 1);
    assert_eq!(e.snapshot.monitors[1].active_page, empty_page);
    assert!(e.window_index("4").is_err());
    e.reconcile(hotplug_system(&["a"], &["4"])).unwrap();
    e.reconcile(hotplug_system(&["a"], &["1", "2", "3", "4"]))
        .unwrap();
    e.reconcile(hotplug_system(&["b", "a"], &["1", "2", "3", "4"]))
        .unwrap();
    assert!(e.snapshot.windows.is_empty());
    assert!(e.snapshot.monitors.iter().all(|m| m.pages.len() == 1));
    assert_hotplug_consistent(&e);
}

#[test]
fn hotplug_mixed_dpi_shrink_clamps_floating_fullscreen_restore_and_pending_geometry() {
    let mut e = engine();
    e.dispatch(Command::Disable).unwrap();
    e.set_window_rules(vec![WindowRule {
        floating: Some(true),
        monitor_id: Some("b".into()),
        page_index: Some(2),
        ..WindowRule::default()
    }])
    .unwrap();
    let mut native = system();
    native.windows.push(window("new", "a"));
    e.reconcile(native).unwrap();
    assert!(e.pending_rule_floating.contains("new"));
    let mut small = hotplug_system(&["a"], &[]);
    small.monitors[0].work_area.width = 300;
    small.monitors[0].work_area.height = 200;
    small.monitors[0].scale_factor = 2.0;
    small.windows.push(window("new", "a"));
    for _ in 0..2 {
        assert!(e.reconcile(small.clone()).unwrap().actions.is_empty());
        assert!(e.pending_rule_floating.contains("new"));
        assert_eq!(
            e.snapshot.windows[e.window_index("new").unwrap()]
                .native
                .rect,
            Rect {
                x: 0,
                y: 0,
                width: 300,
                height: 200
            }
        );
        assert_hotplug_consistent(&e);
    }
    e.dispatch(Command::Enable).unwrap();
    assert!(e.pending_rule_floating.contains("new"));
    let (m, p, _) = e.location("new").unwrap();
    let page_id = e.snapshot.monitors[m].pages[p].id.clone();
    let t = e
        .dispatch(Command::SwitchPage {
            monitor_id: "a".into(),
            page_id,
        })
        .unwrap();
    assert!(!placement(&t.actions, "new").2);
    assert!(!e.pending_rule_floating.contains("new"));
    e.dispatch(Command::ToggleFullscreen).unwrap();
    let mut native = hotplug_system(&["b", "a"], &[]);
    native.monitors[0].work_area.width = 250;
    native.monitors[0].work_area.height = 150;
    native.monitors[0].scale_factor = 1.25;
    native.windows.push(window("new", "a"));
    let t = e.reconcile(native).unwrap();
    assert_eq!(
        placement(&t.actions, "new").0,
        e.snapshot.monitors[1].viewport
    );
    let t = e.dispatch(Command::ToggleFullscreen).unwrap();
    let rect = placement(&t.actions, "new").0;
    assert_eq!(
        rect,
        Rect {
            x: -1200,
            y: 0,
            width: 250,
            height: 150
        }
    );
    assert_hotplug_consistent(&e);
}

#[test]
fn hotplug_closed_borrowed_window_id_is_not_revived_and_reuse_is_new_discovery() {
    let mut e = engine();
    e.reconcile(hotplug_system(&["a"], &[])).unwrap();
    e.dispatch(Command::FocusWindow {
        window_id: "4".into(),
    })
    .unwrap();
    e.reconcile(hotplug_system(&["a"], &["4"])).unwrap();
    assert!(e.window_index("4").is_err());
    assert_ne!(e.snapshot.focused_window.as_deref(), Some("4"));
    e.reconcile(hotplug_system(&["b", "a"], &["4"])).unwrap();
    assert!(e.window_index("4").is_err());
    e.set_window_rules(vec![WindowRule {
        floating: Some(true),
        ..WindowRule::default()
    }])
    .unwrap();
    e.reconcile(hotplug_system(&["b", "a"], &[])).unwrap();
    assert!(e.snapshot.windows[e.window_index("4").unwrap()].floating);
    assert_hotplug_consistent(&e);
}

#[test]
fn hotplug_reconcile_failure_keeps_recovery_metadata_transactional() {
    let mut e = stacked_engine();
    e.reconcile(hotplug_system(&["b"], &[])).unwrap();
    let before = serde_json::to_value(e.snapshot()).unwrap();
    let saved = e.disconnected_monitors.len();
    let order = e.monitor_order.clone();
    let next_id = e.next_id;
    let mut invalid = hotplug_system(&["a", "b"], &[]);
    invalid.monitors[0].work_area.height = 2;
    assert!(e.reconcile(invalid).is_err());
    assert_eq!(serde_json::to_value(e.snapshot()).unwrap(), before);
    assert_eq!(e.disconnected_monitors.len(), saved);
    assert_eq!(e.monitor_order, order);
    assert_eq!(e.next_id, next_id);
    e.reconcile(hotplug_system(&["b", "a"], &[])).unwrap();
    assert!(e.disconnected_monitors.is_empty());
    assert_eq!(e.location("1").unwrap().0, 0);
    assert_hotplug_consistent(&e);
}

#[test]
fn hotplug_restoring_the_hosts_active_page_allows_simultaneous_new_windows() {
    let mut e = engine();
    e.reconcile(hotplug_system(&["a"], &[])).unwrap();
    e.dispatch(Command::FocusWindow {
        window_id: "4".into(),
    })
    .unwrap();
    let mut native = hotplug_system(&["b", "a"], &[]);
    native.windows.push(window("new", "a"));
    e.reconcile(native).unwrap();
    assert_eq!(
        e.snapshot.monitors[e.location("new").unwrap().0].monitor.id,
        "a"
    );
    assert_eq!(
        e.snapshot.monitors[e.location("4").unwrap().0].monitor.id,
        "b"
    );
    assert_hotplug_consistent(&e);
}

#[test]
fn gaps_pad_adjacent_columns_and_clip_to_the_outer_viewport() {
    let mut e = Engine::new(BackendStatus {
        availability: BackendAvailability::Ready,
        capabilities: Capabilities {
            enumerate: true,
            placement: true,
            minimize: true,
            clipping: true,
            focus: true,
            ..Capabilities::default()
        },
        ..BackendStatus::default()
    });
    let half = half_gap(16, 1.0);
    assert_eq!(half, 8);
    e.set_gaps(16);
    let outer = Rect {
        x: 0,
        y: 0,
        width: 1200,
        height: 900,
    };
    let inner = inset_gap(outer, half);
    assert_eq!(expand_gap(inner, half), outer);
    e.set_viewports(BTreeMap::from([("a".into(), inner)]));
    let mut native = system();
    native.monitors.truncate(1);
    native.windows.truncate(3);
    e.reconcile(native).unwrap();
    let t = e.dispatch(Command::Enable).unwrap();
    let (left, left_clip, minimized) = placement(&t.actions, "1");
    let (right, _, _) = placement(&t.actions, "2");
    let gap = (half * 2) as i32;
    assert!(!minimized && left_clip.is_none());
    assert_eq!(left.x - outer.x, gap);
    assert_eq!(left.y - outer.y, gap);
    assert_eq!(right.x - (left.x + left.width as i32), gap);
    assert_eq!(
        outer.x + outer.width as i32 - (right.x + right.width as i32),
        gap
    );
    assert_eq!(
        outer.y + outer.height as i32 - (left.y + left.height as i32),
        gap
    );
    let t = e.dispatch(Command::ToggleFullscreen).unwrap();
    assert_eq!(placement(&t.actions, "1").0, outer);
    e.dispatch(Command::ToggleFullscreen).unwrap();
    // Show columns 2 and 3, then let column 1 peek in from the left edge (beyond the snap).
    e.dispatch(Command::Scroll {
        monitor_id: "a".into(),
        delta: 40,
    })
    .unwrap();
    let t = e
        .dispatch(Command::DragEdge {
            monitor_id: "a".into(),
            edge: 1,
            delta: 200,
        })
        .unwrap();
    let (rect, clip, minimized) = placement(&t.actions, "1");
    let clip = clip.unwrap();
    assert!(!minimized && rect.x < outer.x && clip.x == outer.x && clip.x < inner.x);
    assert_eq!(
        i64::from(clip.x) + i64::from(clip.width),
        i64::from(rect.x) + i64::from(rect.width)
    );
    assert_eq!((clip.y, clip.height), (rect.y, rect.height));
}

#[test]
fn clipped_edge_windows_hide_when_the_neighbouring_monitor_shows_nothing() {
    let mut e = engine();
    // Column "1" peeks 300 px in at a's left edge; the rest lies on monitor b.
    e.snapshot.monitors[0].pages[0].viewport_x = 300;
    let (_, clip, minimized) = placement(&e.placements().unwrap(), "1");
    assert!(!minimized && clip.is_some(), "b's own window covers the cut-off part");
    let page = e.snapshot.monitors[0].pages[0].id.clone();
    e.dispatch(Command::MoveWindowToPage {
        window_id: "4".into(),
        page_id: page,
    })
    .unwrap();
    e.snapshot.monitors[0].pages[0].viewport_x = 300;
    let actions = e.placements().unwrap();
    assert!(placement(&actions, "1").2, "nothing on b would hide it");
    // The right edge borders no monitor: that peek stays.
    let (_, clip, minimized) = placement(&actions, "3");
    assert!(!minimized && clip.is_some());
}

#[test]
fn closing_the_focused_window_focuses_its_layout_neighbour() {
    let mut e = engine();
    e.dispatch(Command::FocusWindow {
        window_id: "2".into(),
    })
    .unwrap();
    // Windows activates a window on the other monitor once "2" is gone.
    let mut native = system();
    native.windows.retain(|w| w.id != "2");
    native.focused_window = Some("4".into());
    let t = e.reconcile(native.clone()).unwrap();
    assert_eq!(e.snapshot.focused_window.as_deref(), Some("3"));
    assert!(matches!(t.actions.last(), Some(NativeAction::Focus { window_id }) if window_id == "3"));
    // The last column falls back to its left neighbour; a stack keeps focus in the column.
    native.windows.retain(|w| w.id != "3");
    native.focused_window = None;
    e.reconcile(native).unwrap();
    assert_eq!(e.snapshot.focused_window.as_deref(), Some("1"));
    let mut e = stacked_engine();
    e.dispatch(Command::FocusWindow {
        window_id: "1".into(),
    })
    .unwrap();
    let mut native = system();
    native.windows.retain(|w| w.id != "1");
    native.focused_window = None;
    e.reconcile(native).unwrap();
    assert_eq!(e.snapshot.focused_window.as_deref(), Some("3"));
}

#[test]
fn slide_column_reveals_and_focuses_the_next_column() {
    // Monitor a: columns 1, 2, 3 of 600 px on a 1200 px view; 1 and 2 are visible.
    let mut e = engine();
    let slide = |e: &mut Engine, direction| {
        e.dispatch(Command::SlideColumn { direction }).unwrap();
        let page = &e.snapshot.monitors[0].pages[0];
        (page.viewport_x, e.snapshot.focused_window.clone().unwrap())
    };
    assert_eq!(slide(&mut e, Direction::Right), (600, "3".into()));
    // Nothing further right: focus moves like focusDirection (already at the last column).
    assert_eq!(slide(&mut e, Direction::Right), (600, "3".into()));
    assert_eq!(slide(&mut e, Direction::Left), (0, "1".into()));
    // Everything to the left is visible: focus steps left through visible columns.
    e.dispatch(Command::FocusWindow {
        window_id: "2".into(),
    })
    .unwrap();
    assert_eq!(slide(&mut e, Direction::Left), (0, "1".into()));
}

fn active_layout(e: &Engine, m: usize) -> (Vec<Vec<String>>, i32) {
    let monitor = &e.snapshot.monitors[m];
    let page = monitor
        .pages
        .iter()
        .find(|p| p.id == monitor.active_page)
        .unwrap();
    (
        page.columns.iter().map(|c| c.windows.clone()).collect(),
        page.viewport_x,
    )
}

#[test]
fn drops_on_a_screen_edge_queue_the_window_off_screen() {
    // Monitor a (x 0..1200): columns 1, 2, 3 of 600 px; 1 and 2 on screen.
    let mut e = engine();
    let drop = |e: &mut Engine, id: &str, x: i32| {
        e.dispatch(Command::DropWindow {
            window_id: id.into(),
            x,
            y: 450,
        })
        .unwrap()
    };
    let t = drop(&mut e, "2", 1190);
    assert_eq!(active_layout(&e, 0), (vec![vec!["1".into()], vec!["3".into()], vec!["2".into()]], 0));
    assert!(placement(&t.actions, "2").2, "queued past the right edge");
    // The focused window queued on the left: focus stays on screen.
    let t = drop(&mut e, "1", 10);
    assert_eq!(active_layout(&e, 0).0, [["1"], ["3"], ["2"]]);
    assert_eq!(active_layout(&e, 0).1, 600);
    assert!(placement(&t.actions, "1").2);
    assert_eq!(e.snapshot.focused_window.as_deref(), Some("3"));
    // Onto another monitor's edge: a lone column there widens to keep the queue off screen.
    drop(&mut e, "3", -10);
    assert_eq!(active_layout(&e, 1), (vec![vec!["4".into()], vec!["3".into()]], 0));
    assert_eq!(e.snapshot.monitors[1].pages[0].columns[0].width, 1200);
}

#[test]
fn edge_squeeze_moves_focus_to_the_window_that_took_the_screen() {
    let mut e = engine();
    let t = e
        .dispatch(Command::DragEdge {
            monitor_id: "a".into(),
            edge: 1,
            delta: -590,
        })
        .unwrap();
    let page = &e.snapshot.monitors[0].pages[0];
    assert_eq!(page.columns[1].width, 1200);
    assert_eq!((page.columns[0].width, page.viewport_x), (600, 600));
    assert_eq!(e.snapshot.focused_window.as_deref(), Some("2"));
    assert!(matches!(t.actions.last(), Some(NativeAction::Focus { window_id }) if window_id == "2"));
}

#[test]
fn row_boundaries_resize_snap_and_squeeze_rows_out_sideways() {
    let row = |edge, delta| Command::DragRow {
        monitor_id: "a".into(),
        column: 0,
        edge,
        delta,
    };
    let heights = |e: &Engine| {
        let actions = e.placements().unwrap();
        e.snapshot.monitors[0].pages[0].columns[0]
            .windows
            .iter()
            .map(|id| placement(&actions, id).0.height)
            .collect::<Vec<_>>()
    };
    // One column [2, 1, 3] filling the 1200x900 screen, rows of 300.
    let mut e = stacked_engine();
    e.dispatch(row(1, 60)).unwrap();
    assert_eq!(heights(&e), [360, 240, 300]);
    e.dispatch(row(1, 80)).unwrap(); // 440 snaps to the middle.
    assert_eq!(heights(&e), [450, 150, 300]);
    e.dispatch(row(2, 290)).unwrap(); // Onto the bottom edge: 3 leaves the stack.
    assert_eq!(heights(&e), [450, 450]);
    // The column fills the screen, so its centre is not in the left half: queued right.
    assert_eq!(active_layout(&e, 0), (vec![vec!["2".into(), "1".into()], vec!["3".into()]], 0));
    // A column in the left half squeezes its rows out to the left.
    let mut e = engine();
    e.dispatch(Command::MoveWindow {
        direction: Direction::Right,
    })
    .unwrap();
    assert_eq!(active_layout(&e, 0).0, [vec!["2", "1"], vec!["3"]]);
    e.dispatch(row(1, -440)).unwrap();
    assert_eq!(active_layout(&e, 0), (vec![vec!["2".into()], vec!["1".into()], vec!["3".into()]], 600));
    assert_eq!(e.snapshot.focused_window.as_deref(), Some("1"));
    assert!(e.dispatch(row(1, 10)).is_err(), "a single row has no boundary");
}
