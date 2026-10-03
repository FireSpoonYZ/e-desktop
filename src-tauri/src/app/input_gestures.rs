//! Wheel and touchpad gestures on the controller thread (lane: input-gestures).
use tauri::Manager;

use super::{Controller, Surface};
use crate::{
    config::Config,
    gestures::{self, Swipe, WheelTracker},
    model::{AppError, Command},
    platform::{Backend, hook::Raw},
};

#[derive(Default)]
pub(super) struct InputGestures {
    wheel: WheelTracker,
    /// Monitor a touchpad drag scrolls, and the sub-pixel motion not applied yet.
    swipe: Option<(String, f64)>,
}

impl Controller {
    /// The hook leaves the wheel to applications while a surface is open.
    pub(super) fn sync_input_gestures(&self, config: &Config, surface_open: bool) {
        let enabled = self.engine.snapshot().enabled;
        crate::platform::input_gestures::configure(
            config.wheel_modifier.filter(|_| enabled && !surface_open),
            config.touchpad_gesture_fingers.filter(|_| enabled),
        );
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
            Raw::Swipe {
                x,
                y,
                swipe: Swipe::Drag(dx),
            } => {
                if self.input_gestures.swipe.is_none() {
                    if self.input_gesture_blocked(app, x, y) {
                        return Ok(());
                    }
                    let Some(monitor) = gestures::monitor_at(self.engine.snapshot(), x, y) else {
                        return Ok(());
                    };
                    self.input_gestures.swipe = Some((monitor.monitor.id.clone(), 0.0));
                }
                let (monitor_id, rest) = self.input_gestures.swipe.clone().unwrap();
                let Some(width) = self.swipe_width(&monitor_id) else {
                    self.input_gestures.swipe = None;
                    return Ok(());
                };
                let pixels = rest + gestures::swipe_pixels(dx, width);
                let delta = pixels.trunc();
                self.input_gestures.swipe = Some((monitor_id.clone(), pixels - delta));
                if delta != 0.0 {
                    let delta = delta.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
                    self.run(Command::DragViewport { monitor_id, delta }, false)?;
                }
            }
            Raw::Swipe {
                swipe: Swipe::Release(velocity),
                ..
            } => {
                let Some((monitor_id, _)) = self.input_gestures.swipe.take() else {
                    return Ok(());
                };
                let Some(width) = self.swipe_width(&monitor_id) else {
                    return Ok(());
                };
                let fling = gestures::swipe_pixels(gestures::release_distance(velocity), width);
                let delta = fling
                    .round()
                    .clamp(f64::from(i32::MIN), f64::from(i32::MAX))
                    as i32;
                self.command(Command::SnapViewport { monitor_id, delta })?;
            }
            Raw::Swipe {
                x,
                y,
                swipe: Swipe::Page(delta),
            } => {
                if self.input_gesture_blocked(app, x, y) {
                    return Ok(());
                }
                let command = gestures::monitor_at(self.engine.snapshot(), x, y)
                    .and_then(|monitor| gestures::relative_page(monitor, delta));
                if let Some(command) = command {
                    self.command(command)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
