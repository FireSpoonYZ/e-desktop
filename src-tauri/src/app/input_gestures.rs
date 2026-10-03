//! Wheel and touchpad gestures on the controller thread (lane: input-gestures).
use tauri::Manager;

use super::{Controller, Surface};
use crate::{
    config::Config,
    gestures::{self, Swipe, WheelTracker},
    model::{AppError, Command},
    platform::{Backend, hook::Raw},
};

/// A touchpad swipe in progress. Its monitor and admission are bound when it begins (the
/// first horizontal `Drag` or `PageBegin`); later steps neither re-check nor retarget.
#[derive(Debug, Clone, PartialEq)]
enum SwipeSession {
    /// Not admitted when it began, or cancelled since: the rest of it does nothing.
    Blocked,
    /// Horizontal: the monitor whose view follows the fingers, and the sub-pixel motion not
    /// applied yet.
    Drag { monitor_id: String, rest: f64 },
    /// Vertical: the monitor whose page changes when it ends.
    Page { monitor_id: String },
}

#[derive(Default)]
pub(super) struct InputGestures {
    wheel: WheelTracker,
    swipe: Option<SwipeSession>,
    /// Touchpad finger count last configured (None: off). A change restarts the recognizer,
    /// which sends no release for the swipe in progress.
    fingers: Option<u8>,
}

impl Controller {
    /// The hook leaves the wheel to applications while a surface is open.
    pub(super) fn sync_input_gestures(&mut self, config: &Config, surface_open: bool) {
        let enabled = self.engine.snapshot().enabled;
        let fingers = config.touchpad_gesture_fingers.filter(|_| enabled);
        if fingers != self.input_gestures.fingers {
            self.input_gestures.fingers = fingers;
            self.input_gestures.swipe = None;
        }
        crate::platform::input_gestures::configure(
            config.wheel_modifier.filter(|_| enabled && !surface_open),
            fingers,
        );
    }

    /// The swipe in progress does nothing more until it ends (lane: rules-spawn-screenshot).
    pub(super) fn block_swipe(&mut self) {
        if self.input_gestures.swipe.is_some() {
            self.input_gestures.swipe = Some(SwipeSession::Blocked);
        }
    }

    /// Focus-follows-mouse rules: no open surface, pointer drag, foreground menu or dialog,
    /// and nothing over a display a foreign fullscreen window paused.
    fn input_gesture_blocked(&self, app: &tauri::AppHandle, x: i32, y: i32) -> bool {
        let surface_open = [Surface::Overview, Surface::Commands].iter().any(|s| {
            app.get_webview_window(s.label())
                .and_then(|w| w.is_visible().ok())
                .unwrap_or(false)
        });
        surface_open
            || !self.engine.snapshot().enabled
            || self.gesture.is_some()
            || self.window_drag
            || self.point_on_suspended(x, y)
            || !self
                .backend
                .as_ref()
                .is_some_and(Backend::pointer_focus_allowed)
    }

    fn swipe_width(&self, monitor_id: &str) -> Option<u32> {
        self.engine
            .snapshot()
            .monitors
            .iter()
            .find(|m| m.monitor.id == monitor_id)
            .map(|m| m.viewport.width)
    }

    pub(super) fn input_gesture(
        &mut self,
        app: &tauri::AppHandle,
        raw: Raw,
    ) -> Result<(), AppError> {
        match raw {
            Raw::Wheel {
                x,
                y,
                delta,
                horizontal,
                time,
            } => {
                let Some(step) = self.input_gestures.wheel.feed(delta, horizontal, time) else {
                    return Ok(());
                };
                if self.input_gesture_blocked(app, x, y) {
                    return Ok(());
                }
                for command in gestures::wheel_commands(self.engine.snapshot(), x, y, step) {
                    self.command(command)?;
                }
            }
            Raw::Swipe { x, y, swipe } => {
                let step = self.swipe_command(x, y, swipe, |c| c.input_gesture_blocked(app, x, y));
                if let Some((command, animate)) = step {
                    self.run(command, animate)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The session a swipe beginning with the pointer at (x, y) binds.
    fn begin_swipe(
        &self,
        x: i32,
        y: i32,
        blocked: impl FnOnce(&Self) -> bool,
        bind: impl FnOnce(String) -> SwipeSession,
    ) -> SwipeSession {
        if blocked(self) {
            return SwipeSession::Blocked;
        }
        gestures::monitor_at(self.engine.snapshot(), x, y)
            .map_or(SwipeSession::Blocked, |m| bind(m.monitor.id.clone()))
    }

    /// The command for one touchpad swipe step and whether it animates. `blocked` is asked
    /// only when a swipe begins.
    pub(super) fn swipe_command(
        &mut self,
        x: i32,
        y: i32,
        swipe: Swipe,
        blocked: impl FnOnce(&Self) -> bool,
    ) -> Option<(Command, bool)> {
        let clamp = |v: f64| v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
        match swipe {
            Swipe::Drag(dx) => {
                if self.input_gestures.swipe.is_none() {
                    let session =
                        self.begin_swipe(x, y, blocked, |monitor_id| SwipeSession::Drag {
                            monitor_id,
                            rest: 0.0,
                        });
                    self.input_gestures.swipe = Some(session);
                }
                let Some(SwipeSession::Drag { monitor_id, rest }) =
                    self.input_gestures.swipe.clone()
                else {
                    return None;
                };
                let Some(width) = self.swipe_width(&monitor_id) else {
                    self.input_gestures.swipe = Some(SwipeSession::Blocked);
                    return None;
                };
                let pixels = rest + gestures::swipe_pixels(dx, width);
                let delta = pixels.trunc();
                self.input_gestures.swipe = Some(SwipeSession::Drag {
                    monitor_id: monitor_id.clone(),
                    rest: pixels - delta,
                });
                (delta != 0.0).then(|| {
                    let delta = clamp(delta);
                    (Command::DragViewport { monitor_id, delta }, false)
                })
            }
            Swipe::Release(velocity) => {
                let Some(SwipeSession::Drag { monitor_id, .. }) = self.input_gestures.swipe.take()
                else {
                    return None;
                };
                let width = self.swipe_width(&monitor_id)?;
                let fling = gestures::swipe_pixels(gestures::release_distance(velocity), width);
                let delta = clamp(fling.round());
                Some((Command::SnapViewport { monitor_id, delta }, true))
            }
            Swipe::PageBegin => {
                let session = self.begin_swipe(x, y, blocked, |monitor_id| SwipeSession::Page {
                    monitor_id,
                });
                self.input_gestures.swipe = Some(session);
                None
            }
            Swipe::Page(delta) => {
                let Some(SwipeSession::Page { monitor_id }) = self.input_gestures.swipe.take()
                else {
                    return None;
                };
                if delta == 0 {
                    return None;
                }
                let snapshot = self.engine.snapshot();
                let monitor = snapshot
                    .monitors
                    .iter()
                    .find(|m| m.monitor.id == monitor_id)?;
                gestures::relative_page(monitor, delta).map(|command| (command, true))
            }
            Swipe::Cancel => {
                self.input_gestures.swipe = None;
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{offline_controller, two_monitor_engine};

    const OPEN: fn(&Controller) -> bool = |_| false;
    const SHUT: fn(&Controller) -> bool = |_| true;

    fn never_asked(_: &Controller) -> bool {
        panic!("admission is decided when the swipe begins")
    }

    fn page_target(step: Option<(Command, bool)>) -> Option<String> {
        match step {
            Some((Command::SwitchPage { monitor_id, .. }, true)) => Some(monitor_id),
            other => panic!("expected an animated page switch, got {other:?}"),
        }
    }

    fn drag_target(step: Option<(Command, bool)>) -> String {
        match step {
            Some((Command::DragViewport { monitor_id, .. }, false)) => monitor_id,
            other => panic!("expected a viewport drag, got {other:?}"),
        }
    }

    #[test]
    fn vertical_swipe_binds_monitor_and_admission_when_it_begins() {
        let mut c = offline_controller(two_monitor_engine());
        // Begun over A; the pointer ends over B: A still switches.
        assert_eq!(c.swipe_command(500, 400, Swipe::PageBegin, OPEN), None);
        let step = c.swipe_command(1500, 400, Swipe::Page(1), never_asked);
        assert_eq!(page_target(step).as_deref(), Some("a"));
        // Begun while blocked (an open surface, say): ending after it closed does nothing.
        assert_eq!(c.swipe_command(500, 400, Swipe::PageBegin, SHUT), None);
        assert_eq!(c.swipe_command(500, 400, Swipe::Page(1), never_asked), None);
        // Short of the threshold the swipe still ends, so the next one binds afresh.
        c.swipe_command(500, 400, Swipe::PageBegin, OPEN);
        assert_eq!(c.swipe_command(500, 400, Swipe::Page(0), never_asked), None);
        c.swipe_command(1500, 400, Swipe::PageBegin, OPEN);
        let step = c.swipe_command(500, 400, Swipe::Page(1), never_asked);
        assert_eq!(page_target(step).as_deref(), Some("b"));
        // An end without a beginning (it was dropped) switches nothing.
        assert_eq!(c.swipe_command(500, 400, Swipe::Page(1), never_asked), None);
    }

    #[test]
    fn horizontal_swipe_blocked_at_its_start_stays_blocked_until_it_ends() {
        let mut c = offline_controller(two_monitor_engine());
        assert_eq!(c.swipe_command(500, 400, Swipe::Drag(0.1), SHUT), None);
        assert_eq!(
            c.swipe_command(500, 400, Swipe::Drag(0.1), never_asked),
            None
        );
        assert_eq!(
            c.swipe_command(500, 400, Swipe::Release(1.0), never_asked),
            None
        );
        // The next swipe is checked again and follows the pointer's monitor at its start.
        let step = c.swipe_command(1500, 400, Swipe::Drag(0.1), OPEN);
        assert_eq!(drag_target(step), "b");
        let step = c.swipe_command(500, 400, Swipe::Drag(0.1), never_asked);
        assert_eq!(drag_target(step), "b");
        match c.swipe_command(500, 400, Swipe::Release(0.0), never_asked) {
            Some((Command::SnapViewport { monitor_id, .. }, true)) => assert_eq!(monitor_id, "b"),
            other => panic!("expected a snap, got {other:?}"),
        }
    }

    #[test]
    fn pausing_or_reconfiguring_the_touchpad_ends_the_controller_swipe() {
        let mut c = offline_controller(two_monitor_engine());
        let fingers = |n: Option<u8>| Config {
            touchpad_gesture_fingers: n,
            ..Config::default()
        };
        c.sync_input_gestures(&fingers(Some(3)), false);
        assert_eq!(
            drag_target(c.swipe_command(500, 400, Swipe::Drag(0.1), OPEN)),
            "a"
        );
        // An unchanged configuration keeps the swipe going.
        c.sync_input_gestures(&fingers(Some(3)), true);
        assert_eq!(
            drag_target(c.swipe_command(500, 400, Swipe::Drag(0.1), never_asked)),
            "a"
        );
        // Paused mid-swipe (the recognizer restarts and never releases it), then resumed:
        // the next swipe starts over B and is checked again.
        c.engine.dispatch(Command::Disable).unwrap();
        c.sync_input_gestures(&fingers(Some(3)), false);
        c.engine.dispatch(Command::Enable).unwrap();
        c.sync_input_gestures(&fingers(Some(3)), false);
        assert_eq!(
            drag_target(c.swipe_command(1500, 400, Swipe::Drag(0.1), OPEN)),
            "b"
        );
        // Switching gestures off and on again does the same.
        c.sync_input_gestures(&fingers(None), false);
        c.sync_input_gestures(&fingers(Some(4)), false);
        assert_eq!(
            drag_target(c.swipe_command(500, 400, Swipe::Drag(0.1), OPEN)),
            "a"
        );
        // The recognizer's own Cancel ends it too: its release never comes.
        assert_eq!(c.swipe_command(0, 0, Swipe::Cancel, never_asked), None);
        assert_eq!(
            c.swipe_command(500, 400, Swipe::Release(1.0), never_asked),
            None
        );
        assert_eq!(
            drag_target(c.swipe_command(1500, 400, Swipe::Drag(0.1), OPEN)),
            "b"
        );
    }
}
