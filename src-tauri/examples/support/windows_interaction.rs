//! Helpers included by windows_smoke; actions remain restricted to its fixture PID.
use super::*;
use e_desktop::animation::{Animation, Placements};
use e_desktop::preview::{PreviewSlot, PreviewState};
use std::time::{Duration, Instant};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub fn preview_checks(
    backend: &mut Backend,
    original: &SystemSnapshot,
    evidence: &mut Vec<serde_json::Value>,
) -> Result<()> {
    // Own-process destination, never an arbitrary external HWND.
    struct Destination(windows_sys::Win32::Foundation::HWND);
    impl Drop for Destination {
        fn drop(&mut self) {
            unsafe {
                DestroyWindow(self.0);
            }
        }
    }
    let class: Vec<_> = "STATIC\0".encode_utf16().collect();
    let title: Vec<_> = "e-desktop preview smoke\0".encode_utf16().collect();
    let area = original
        .monitors
        .iter()
        .find(|m| m.primary)
        .unwrap()
        .work_area;
    let destination = Destination(unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            area.x + 40,
            area.y + 70,
            500,
            300,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    });
    assert!(!destination.0.is_null());
    let slot = PreviewSlot {
        window_id: original.windows[0].id.clone(),
        rect: Rect {
            x: 10,
            y: 10,
            width: 300,
            height: 180,
        },
        clip: Rect {
            x: 30,
            y: 20,
            width: 270,
            height: 150,
        },
    };
    assert!(backend.previews_available());
    let statuses = backend.sync_previews(destination.0 as usize, &[slot.clone()])?;
    assert_eq!(statuses[0].state, PreviewState::Ready);
    let duplicate = backend.sync_previews(destination.0 as usize, &[slot.clone(), slot.clone()]);
    assert!(duplicate.is_err());
    assert_eq!(
        backend.sync_previews(destination.0 as usize, &[slot.clone()])?[0].state,
        PreviewState::Ready
    );
    let mut missing = slot.clone();
    missing.window_id = "not-registered".into();
    assert_eq!(
        backend.sync_previews(destination.0 as usize, &[missing])?[0].state,
        PreviewState::SourceGone
    );
    let mut hidden = slot.clone();
    hidden.clip.width = 0;
    assert_eq!(
        backend.sync_previews(destination.0 as usize, &[hidden])?[0].state,
        PreviewState::Hidden
    );
    backend.clear_previews();
    assert_eq!(
        backend.sync_previews(destination.0 as usize, &[slot.clone()])?[0].state,
        PreviewState::Ready
    );
    backend.restore()?;
    let pid = original.windows[0].process_id;
    for initial in &original.windows {
        let system = observe(backend, pid)?;
        let actual = system.windows.iter().find(|w| w.id == initial.id).unwrap();
        assert_eq!(
            actual.rect, initial.rect,
            "previews must not change source geometry"
        );
        assert_eq!(actual.minimized, initial.minimized);
    }
    unsafe {
        ShowWindow(destination.0, SW_HIDE);
    }
    assert!(
        backend
            .sync_previews(destination.0 as usize, &[slot])
            .is_err()
    );
    evidence.push(
        serde_json::json!({"dwmPreview": {"statuses": statuses, "duplicateRejected": true,
        "unknownSourceRejected": true, "offscreenHidden": true, "clearReregisterRestore": true,
        "hiddenDestinationRejected": true, "sourceGeometryUnchanged": true,
        "visualPixelsVerified": false }}),
    );
    Ok(())
}

pub fn animation_checks(
    backend: &mut Backend,
    original: &SystemSnapshot,
    evidence: &mut Vec<serde_json::Value>,
) -> Result<()> {
    let mut engine = Engine::new(backend.status());
    engine.reconcile(original.clone())?;
    let enabled = engine.dispatch(Command::Enable)?;
    let mut applied = Placements::new();
    let ids: Vec<_> = original.windows.iter().map(|w| w.id.clone()).collect();
    let pid = original.windows[0].process_id;
    let monitor = original.windows[0].monitor_id.clone();
    let remember = |applied: &mut Placements, actions: &[NativeAction]| {
        for action in actions {
            if let NativeAction::Placement {
                window_id,
                rect,
                clip,
                minimized,
            } = action
            {
                assert!(ids.contains(window_id));
                applied.insert(window_id.clone(), (*rect, *clip, *minimized));
            }
        }
    };
    backend.apply(&enabled.actions)?;
    remember(&mut applied, &enabled.actions);
    let focus = engine.snapshot().focused_window.clone();
    let sized = engine.dispatch(Command::SetWindowColumnWidth {
        window_id: ids[1].clone(),
        width: 650,
    })?;
    assert_eq!(sized.snapshot.focused_window, focus);
    assert!(
        !sized
            .actions
            .iter()
            .any(|a| matches!(a, NativeAction::Focus { .. }))
    );
    backend.apply(&sized.actions)?;
    remember(&mut applied, &sized.actions);
    assert_eq!(
        observe(backend, pid)?
            .windows
            .iter()
            .find(|w| w.id == ids[1])
            .unwrap()
            .rect
            .width,
        650
    );
    let mut animation = Animation::default();
    let mut visible_frames = 0;
    // Redirect mid-flight, then enter from the opposite edge, and cancel before restore.
    // Scrolling snaps to column edges; always head toward the side that still has room.
    for (step, partial) in [(600, true), (450, false), (700, false), (700, true)] {
        let prev = engine.snapshot().clone();
        let at_start = prev
            .monitors
            .iter()
            .find(|m| m.monitor.id == monitor)
            .and_then(|m| m.pages.iter().find(|p| p.id == m.active_page))
            .is_some_and(|p| p.viewport_x == 0);
        let delta = if at_start { step } else { -step };
        let transition = engine.dispatch(Command::Scroll {
            monitor_id: monitor.clone(),
            delta,
        })?;
        let now = Instant::now();
        let initial = animation.start(
            &prev,
            engine.snapshot(),
            &applied,
            transition.actions,
            Duration::from_millis(160),
            now,
        );
        assert!(
            animation.deadline().is_some(),
            "scroll {delta} did not move"
        );
        backend.apply(&initial)?;
        remember(&mut applied, &initial);
        for ms in (16..=if partial { 80 } else { 160 }).step_by(16) {
            std::thread::sleep(Duration::from_millis(16));
            let frame = animation.frame(now + Duration::from_millis(ms));
            backend.apply(&frame)?;
            remember(&mut applied, &frame);
            let actual = observe(backend, pid)?;
            for action in frame {
                if let NativeAction::Placement {
                    window_id,
                    rect,
                    minimized,
                    clip,
                } = action
                {
                    let native = actual.windows.iter().find(|w| w.id == window_id).unwrap();
                    assert_eq!(native.minimized, minimized);
                    if !minimized {
                        assert_eq!(native.rect, rect);
                        // Frames drop the mask only when the whole window is on its monitor.
                        let viewport = engine
                            .snapshot()
                            .monitors
                            .iter()
                            .find(|m| m.monitor.id == monitor)
                            .unwrap()
                            .viewport;
                        let inside = rect.x >= viewport.x
                            && i64::from(rect.x) + i64::from(rect.width)
                                <= i64::from(viewport.x) + i64::from(viewport.width);
                        assert!(clip.is_some() || inside);
                        visible_frames += 1;
                    }
                }
            }
        }
    }
    animation.cancel();
    let disabled = engine.dispatch(Command::Disable)?;
    backend.apply(&disabled.actions)?;
    backend.restore()?;
    assert!(
        animation
            .frame(Instant::now() + Duration::from_secs(2))
            .is_empty()
    );
    let actual = observe(backend, pid)?;
    for initial in &original.windows {
        let restored = actual.windows.iter().find(|w| w.id == initial.id).unwrap();
        assert_eq!(restored.rect, initial.rect);
        assert_eq!(restored.minimized, initial.minimized);
    }
    evidence.push(
        serde_json::json!({"nativeScrollAnimation": {"verifiedVisibleFrames": visible_frames,
        "redirectAndEntry": true, "disableRestored": true, "targetedColumnWidth": 650,
        "realTimeCadenceMeasured": false }}),
    );
    Ok(())
}
