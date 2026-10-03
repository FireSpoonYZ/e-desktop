//! lane: rules-spawn-screenshot. Screenshot commands on the controller thread: resolve the
//! target here, then capture, save and copy on the backend's screenshot thread.
use super::*;
use crate::screenshot::Kind;

/// Our own transient surfaces on screen; each leaves before the capture.
#[cfg(target_os = "windows")]
fn surfaces_to_dismiss(overview: bool, commands: bool, hotkeys: bool) -> Vec<Surface> {
    [
        (Surface::Overview, overview),
        (Surface::Commands, commands),
        (Surface::Hotkeys, hotkeys),
    ]
    .into_iter()
    .filter_map(|(surface, shown)| shown.then_some(surface))
    .collect()
}

impl Controller {
    pub(super) fn screenshot(
        &mut self,
        app: &tauri::AppHandle,
        sender: &RequestSender,
        kind: Kind,
        path: Option<String>,
    ) -> Result<(), AppError> {
        #[cfg(target_os = "windows")]
        {
            use crate::platform::screenshot::{Target, start};
            let snapshot = self.engine.snapshot();
            let target = match kind {
                Kind::Region => Target::Region,
                Kind::Screen => Target::Screen(
                    snapshot
                        .monitors
                        .iter()
                        .find(|m| Some(&m.monitor.id) == snapshot.active_monitor.as_ref())
                        .or(snapshot.monitors.first())
                        .map(|m| m.monitor.bounds)
                        .ok_or_else(|| {
                            error(ErrorCode::BackendUnavailable, "没有可截取的显示器。")
                        })?,
                ),
                Kind::Window => Target::Window(
                    self.backend
                        .as_ref()
                        .ok_or_else(|| {
                            error(ErrorCode::BackendUnavailable, "原生窗口后端尚未连接。")
                        })?
                        .screenshot_window(snapshot.focused_window.as_deref())?,
                ),
            };
            // An open palette, overview or hotkey overlay leaves the screen first; Dismiss
            // hands focus back (the overlay only when it holds the focus).
            let hotkeys = app
                .get_webview_window(Surface::Hotkeys.label())
                .and_then(|w| w.is_visible().ok())
                .unwrap_or(false);
            let dismiss = surfaces_to_dismiss(
                self.overview_host.is_some(),
                self.commands_host.is_some(),
                hotkeys,
            );
            for &surface in &dismiss {
                let _ = sender.try_send(Request::Dismiss(surface, IpcTrace::NONE));
            }
            let sender = sender.clone();
            start(target, path, !dismiss.is_empty(), move |issue| {
                let _ = sender.try_send(Request::Issue(issue));
            })?;
            self.cancel_pointer_sessions();
            Ok(())
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (app, sender, kind, path);
            Err(crate::screenshot::unsupported())
        }
    }

    /// The screenshot takes the pointer: drop the floating-window drag, the tiled-window drop
    /// preview and the touchpad swipe in progress. Their releases still arrive and find nothing.
    #[cfg(target_os = "windows")]
    fn cancel_pointer_sessions(&mut self) {
        self.gesture = None;
        if std::mem::take(&mut self.window_drag) {
            crate::platform::splitter::end_window(None, false);
        }
        self.block_swipe();
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;
    use crate::{
        app::tests::{offline_controller, two_monitor_engine},
        gestures::Swipe,
    };

    #[test]
    fn every_transient_surface_including_the_hotkey_overlay_leaves_before_a_capture() {
        let labels = |overview, commands, hotkeys| {
            surfaces_to_dismiss(overview, commands, hotkeys)
                .iter()
                .map(|surface| surface.label())
                .collect::<Vec<_>>()
        };
        assert!(labels(false, false, false).is_empty());
        assert_eq!(labels(false, false, true), ["hotkeys"]);
        assert_eq!(
            labels(true, true, true),
            ["overview", "commands", "hotkeys"]
        );
    }

    #[test]
    fn a_screenshot_cancels_drags_and_swipes_so_their_late_releases_do_nothing() {
        let mut engine = two_monitor_engine();
        engine.dispatch(Command::ToggleFloating).unwrap();
        let mut controller = offline_controller(engine);
        // Modifier drag of the floating window, a tiled-window drop preview and a touchpad
        // swipe are all under way when the screenshot starts.
        controller.gesture = Gesture::start(controller.engine.snapshot(), "wa", 10, 10);
        assert!(controller.gesture.is_some());
        controller.window_drag = true;
        let open = |_: &Controller| false;
        let step = controller.swipe_command(500, 400, Swipe::Drag(0.1), open);
        assert!(matches!(step, Some((Command::DragViewport { .. }, false))));
        controller.cancel_pointer_sessions();
        assert!(
            controller.gesture.is_none(),
            "the floating window no longer follows moves"
        );
        assert!(
            !controller.window_drag,
            "the drop preview ended without a drop"
        );
        // The rest of the swipe, and its release after the picker closed, do nothing.
        assert_eq!(
            controller.swipe_command(500, 400, Swipe::Drag(0.1), open),
            None
        );
        assert_eq!(
            controller.swipe_command(500, 400, Swipe::Release(2.0), open),
            None
        );
        // The next swipe works again.
        let step = controller.swipe_command(1500, 400, Swipe::Drag(0.1), open);
        assert!(
            matches!(step, Some((Command::DragViewport { monitor_id, .. }, false)) if monitor_id == "b")
        );
        // Nothing in progress: nothing gets blocked.
        let mut idle = offline_controller(two_monitor_engine());
        idle.cancel_pointer_sessions();
        let step = idle.swipe_command(500, 400, Swipe::Drag(0.1), open);
        assert!(matches!(step, Some((Command::DragViewport { .. }, false))));
    }
}
