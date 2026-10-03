//! Controller-owned layout frames. No timers/threads survive cancellation or restoration.
//!
//! Every engine transition can animate: tiled windows interpolate position and size from
//! their last applied plan to the new target, and page switches slide the old page out and
//! the new one in vertically (niri workspaces). Frames are clipped to the monitor, so this
//! needs native clipping; other backends settle immediately.
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use crate::{
    layout::{expand_gap, half_gap},
    model::{MonitorState, NativeAction, Page, Rect, Snapshot, WindowState},
};

/// Last successful native plans; minimized rectangles are logical (not moved by the backend).
pub type Placements = HashMap<String, (Rect, Option<Rect>, bool)>;
const FRAME_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Default)]
pub struct Animation {
    plan: Option<Plan>,
    /// `start` installed this plan and `restart` has not consumed it yet.
    fresh: bool,
    /// The next `start` explicitly requests focus while fullscreen already owns foreground.
    user_focus: bool,
}

struct Plan {
    started: Instant,
    duration: Duration,
    next_frame: Instant,
    moves: Vec<Move>,
    /// A window not yet fully inside its viewport is activated after its final placement.
    deferred: Vec<NativeAction>,
    /// This activation was explicitly requested while fullscreen already owned foreground.
    user_deferred: bool,
}

/// One window of a running animation, for backends that draw frames themselves (a compositor
/// overlay per monitor, niri style) instead of moving the real window every frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Sprite {
    pub window_id: String,
    /// The current frame (screen coordinates), drawn cropped to `bounds`.
    pub rect: Rect,
    /// The monitor area the window is drawn into.
    pub bounds: Rect,
    /// The final placement when the real window may take it right away, hidden under the
    /// overlay: it stays shown and at least as visible as now. Windows that shrink out of
    /// view or end hidden keep their current state (and picture) until the animation ends.
    pub early: Option<NativeAction>,
}

fn area(r: Option<Rect>) -> u64 {
    r.map_or(0, |r| u64::from(r.width) * u64::from(r.height))
}

struct Move {
    /// The exact engine placement, applied when the animation ends or is finished early.
    action: NativeAction,
    from: Rect,
    to: Rect,
    /// The last frame produced.
    current: Rect,
    /// x, y, width, height velocities in physical pixels/second.
    initial_velocity: [f64; 4],
    velocity: [f64; 4],
    /// Monitor viewport expanded by half a gap: the area frames may draw into.
    bounds: Rect,
    /// Minimized (or never shown) since the last frame: mask before restoring it.
    hidden: bool,
}

impl Move {
    fn frame(&mut self, elapsed: Duration, duration: Duration) -> NativeAction {
        let NativeAction::Placement { window_id, .. } = &self.action else {
            unreachable!()
        };
        let from = rect_components(self.from);
        let to = rect_components(self.to);
        let values: [f64; 4] = std::array::from_fn(|i| {
            let (position, velocity) = spring(
                from[i],
                to[i],
                self.initial_velocity[i],
                elapsed.as_secs_f64(),
                duration.as_secs_f64(),
            );
            self.velocity[i] = velocity;
            position.round()
        });
        let rect = Rect {
            x: values[0].clamp(i32::MIN.into(), i32::MAX.into()) as i32,
            y: values[1].clamp(i32::MIN.into(), i32::MAX.into()) as i32,
            width: values[2].clamp(1.0, u32::MAX.into()) as u32,
            height: values[3].clamp(1.0, u32::MAX.into()) as u32,
        };
        self.current = rect;
        let clip = clip_to_viewport(rect, self.bounds);
        let minimized = clip.is_none();
        // A fully visible frame of an already shown window needs no mask; a window being
        // restored or crossing the edge is masked before it moves (no spill onto neighbours).
        let clip = clip.filter(|c| self.hidden || *c != rect);
        self.hidden = minimized;
        NativeAction::Placement {
            window_id: window_id.clone(),
            rect,
            clip,
            minimized,
        }
    }
}

impl Animation {
    pub fn deadline(&self) -> Option<Instant> {
        self.plan.as_ref().map(|plan| plan.next_frame)
    }

    /// Length of the running plan; a refresh keeps it instead of restarting the clock.
    pub fn duration(&self) -> Option<Duration> {
        self.plan.as_ref().map(|plan| plan.duration)
    }

    pub fn sprites(&self) -> Vec<Sprite> {
        let Some(plan) = &self.plan else {
            return vec![];
        };
        plan.moves
            .iter()
            .map(|m| {
                let NativeAction::Placement {
                    window_id,
                    minimized,
                    ..
                } = &m.action
                else {
                    unreachable!()
                };
                let to = clip_to_viewport(m.to, m.bounds);
                let early = !minimized
                    && (to == Some(m.to)
                        || area(to)
                            >= area(clip_to_viewport(m.from, m.bounds).filter(|_| !m.hidden)));
                Sprite {
                    window_id: window_id.clone(),
                    rect: m.current,
                    bounds: m.bounds,
                    early: early.then(|| m.action.clone()),
                }
            })
            .collect()
    }

    /// Restart the clock from the displayed frame and velocity: preparing an overlay can
    /// take a frame or two. A poll that kept the same target must not restart its motion.
    pub fn restart(&mut self, now: Instant) {
        if !self.fresh {
            return;
        }
        self.fresh = false;
        let Some(plan) = &mut self.plan else {
            return;
        };
        for m in &mut plan.moves {
            m.from = m.current;
            m.initial_velocity = m.velocity;
        }
        plan.started = now;
        plan.next_frame = now + FRAME_INTERVAL.min(plan.duration);
    }

    pub fn started_fresh(&self) -> bool {
        self.fresh
    }

    pub fn deferred_focus(&self) -> Option<&NativeAction> {
        self.plan
            .as_ref()?
            .deferred
            .iter()
            .find(|a| matches!(a, NativeAction::Focus { .. }))
    }

    pub fn deferred_focus_id(&self) -> Option<&str> {
        match self.deferred_focus() {
            Some(NativeAction::Focus { window_id }) => Some(window_id),
            _ => None,
        }
    }

    /// Preserve a new explicit activation only if fullscreen already owns foreground.
    /// An ordinary navigation request made before fullscreen begins must remain cancellable.
    pub fn note_user_focus(&mut self, foreground_paused: bool) {
        self.user_focus = foreground_paused;
    }

    pub fn clear_user_focus(&mut self) {
        self.user_focus = false;
    }

    /// Drop a focus that is still waiting. The next `start` must not put it back.
    pub fn discard_deferred_focus(&mut self) {
        if let Some(plan) = &mut self.plan {
            plan.deferred
                .retain(|a| !matches!(a, NativeAction::Focus { .. }));
            plan.user_deferred = false;
        }
    }

    /// Drop pre-fullscreen activation; keep an explicit activation requested during fullscreen.
    pub fn discard_stale_focus(&mut self) {
        if self.plan.as_ref().is_some_and(|plan| plan.user_deferred) {
            return;
        }
        self.discard_deferred_focus();
    }

    /// Frames a retarget may start from. Windows in the running plan contribute the displayed
    /// frame even when the compositor has parked the HWND at its target or left the old one.
    /// Anything else must still match the enumerated native rect, so a user resize outside the
    /// animation settles at its target instead of easing from a stale plan.
    pub fn retarget_from(&self, placements: &Placements, windows: &[WindowState]) -> Placements {
        placements
            .iter()
            .filter(|(id, (rect, _, hidden))| {
                self.animating(id)
                    || windows.iter().any(|w| {
                        &w.native.id == *id
                            && if *hidden {
                                w.native.minimized_by_manager
                            } else {
                                !w.native.minimized && w.native.rect == *rect
                            }
                    })
            })
            .map(|(id, plan)| (id.clone(), *plan))
            .collect()
    }

    fn animating(&self, id: &str) -> bool {
        self.plan.as_ref().is_some_and(|plan| {
            plan.moves.iter().any(|m| {
                matches!(&m.action, NativeAction::Placement { window_id, .. } if window_id == id)
            })
        })
    }

    /// Taking the plan invalidates every remaining frame before a native Restore can run.
    /// Returns the exact final actions that were still pending.
    pub fn cancel(&mut self) -> Vec<NativeAction> {
        self.fresh = false;
        self.plan.take().map_or_else(Vec::new, |plan| {
            plan.moves
                .into_iter()
                .map(|m| m.action)
                .chain(plan.deferred)
                .collect()
        })
    }

    /// Replaces any running plan, starting from `applied` (the last frame actually placed),
    /// so a new command retargets mid-flight windows smoothly. Returns what to apply now.
    pub fn start(
        &mut self,
        prev: &Snapshot,
        next: &Snapshot,
        applied: &Placements,
        mut actions: Vec<NativeAction>,
        duration: Duration,
        now: Instant,
    ) -> Vec<NativeAction> {
        let user = std::mem::take(&mut self.user_focus);
        // Enumeration during a slide often produces the same targets. Keep its clock and
        // frames instead of cancelling and restarting the easing curve at every refresh.
        let keep = self.plan.as_ref().is_some_and(|plan| {
            next.enabled
                && next.backend.capabilities.clipping
                && plan.duration == duration
                && plan.moves.iter().all(|m| {
                    let NativeAction::Placement { window_id, .. } = &m.action else {
                        return false;
                    };
                    actions.contains(&m.action)
                        && locate(next, window_id).is_some_and(|(monitor, _)| {
                            expand_gap(
                                monitor.viewport,
                                half_gap(next.gaps, monitor.monitor.scale_factor),
                            ) == m.bounds
                        })
                })
                && actions
                    .iter()
                    .filter(|a| matches!(a, NativeAction::Focus { .. }))
                    .all(|a| plan.deferred.contains(a))
        });
        if keep {
            if user {
                let requested = actions.iter().find_map(|a| match a {
                    NativeAction::Focus { window_id } => Some(window_id.clone()),
                    _ => None,
                });
                if let Some(plan) = self.plan.as_mut() {
                    if requested.as_ref().is_some_and(|id| {
                        plan.deferred.iter().any(|a| {
                            matches!(a, NativeAction::Focus { window_id } if window_id == id)
                        })
                    }) {
                        plan.user_deferred = true;
                    }
                }
            }
            self.fresh = false;
            let plan = self.plan.as_ref().unwrap();
            return actions
                .into_iter()
                .filter(|a| {
                    !plan.moves.iter().any(|m| &m.action == a) && !plan.deferred.contains(a)
                })
                .collect();
        }
        // A focus still waiting in the old plan must survive a retarget that brings none.
        let is_focus = |a: &NativeAction| matches!(a, NativeAction::Focus { .. });
        let incoming_focus = actions.iter().find_map(|a| match a {
            NativeAction::Focus { window_id } => Some(window_id.clone()),
            _ => None,
        });
        let old_user_focus = self.plan.as_ref().and_then(|plan| {
            plan.user_deferred.then(|| {
                plan.deferred.iter().find_map(|a| match a {
                    NativeAction::Focus { window_id } => Some(window_id.clone()),
                    _ => None,
                })
            })?
        });
        let velocities: HashMap<_, _> = self.plan.as_ref().map_or_else(HashMap::new, |plan| {
            plan.moves
                .iter()
                .map(|m| {
                    let NativeAction::Placement { window_id, .. } = &m.action else {
                        unreachable!()
                    };
                    (window_id.clone(), (m.current, m.bounds, m.velocity))
                })
                .collect()
        });
        let carried = self.cancel().into_iter().rev().find(is_focus);
        if let Some(focus) = carried.filter(|_| !actions.iter().any(is_focus)) {
            actions.push(focus);
        }
        if !next.enabled || !next.backend.capabilities.clipping || duration.is_zero() {
            return actions;
        }
        let mut moves = Vec::new();
        let mut now_actions = Vec::new();
        for action in actions {
            let planned = match &action {
                NativeAction::Placement {
                    window_id,
                    rect,
                    minimized,
                    ..
                } => plan_move(prev, next, applied, window_id, *rect, *minimized),
                _ => None,
            };
            match planned {
                Some((from, to, bounds, hidden)) => {
                    let NativeAction::Placement {
                        window_id,
                        minimized,
                        ..
                    } = &action
                    else {
                        unreachable!()
                    };
                    // Carry only the velocity of this displayed frame in the same viewport.
                    let velocity = velocities
                        .get(window_id)
                        .filter(|(current, old_bounds, _)| {
                            *current == from && *old_bounds == bounds
                        })
                        .map_or([0.0; 4], |(_, _, velocity)| *velocity);
                    if from == to && hidden == *minimized && velocity == [0.0; 4] {
                        now_actions.push(action);
                        continue;
                    }
                    let mut m = Move {
                        action,
                        from,
                        to,
                        current: from,
                        initial_velocity: velocity,
                        velocity,
                        bounds,
                        hidden,
                    };
                    now_actions.push(m.frame(Duration::ZERO, duration));
                    moves.push(m);
                }
                None => now_actions.push(action),
            }
        }
        if moves.is_empty() {
            return now_actions;
        }
        let mut deferred = Vec::new();
        now_actions.retain(|action| {
            let NativeAction::Focus { window_id } = action else {
                return true;
            };
            let hidden = moves.iter().any(|m| {
                matches!(&m.action, NativeAction::Placement { window_id: id, .. } if id == window_id)
                    && (m.hidden || clip_to_viewport(m.from, m.bounds) != Some(m.from))
            });
            if hidden {
                deferred.push(action.clone());
            }
            !hidden
        });
        self.plan = Some(Plan {
            started: now,
            duration,
            next_frame: now + FRAME_INTERVAL.min(duration),
            moves,
            user_deferred: deferred.iter().any(|a| match a {
                NativeAction::Focus { window_id } => {
                    incoming_focus.as_ref() == Some(window_id) && user
                        || old_user_focus.as_ref() == Some(window_id)
                }
                _ => false,
            }),
            deferred,
        });
        self.fresh = true;
        now_actions
    }

    pub fn frame(&mut self, now: Instant) -> Vec<NativeAction> {
        let Some(plan) = self.plan.as_mut() else {
            return vec![];
        };
        if now < plan.next_frame {
            return vec![];
        }
        let elapsed = now.saturating_duration_since(plan.started);
        if elapsed >= plan.duration {
            return self.cancel(); // Exact engine actions, not rounded interpolated endpoints.
        }
        // Stay on the original cadence; skip missed ticks rather than drifting or catching up.
        let tick = elapsed.as_nanos() / FRAME_INTERVAL.as_nanos() + 1;
        plan.next_frame =
            (plan.started + FRAME_INTERVAL * tick as u32).min(plan.started + plan.duration);
        plan.moves
            .iter_mut()
            .map(|m| m.frame(elapsed, plan.duration))
            .collect()
    }
}

/// While a deferred activation is in flight, ignore the foreground we left and any focus that
/// is not a managed window. A different managed window is a real click and must reach reconcile.
pub fn suppress_observed_focus(
    deferred_target: Option<&str>,
    enumerated: Option<&str>,
    ignored_foreground: Option<&str>,
    enumerated_is_managed: bool,
) -> bool {
    let Some(target) = deferred_target else {
        return false;
    };
    !enumerated_is_managed
        || enumerated.is_none_or(|id| id == target || ignored_foreground == Some(id))
}

/// Remember the foreground a deferred slide left behind. Cleared once that activation is gone.
/// Only the plan that first defers records it. `None` is a real foreground (nothing focused), not
/// "no anchor yet"; a later retarget must not fill that with the layout target.
pub fn note_ignored_foreground(
    ignored: &mut Option<String>,
    fresh: bool,
    already_deferred: bool,
    deferred_target: Option<&str>,
    observed_foreground: Option<String>,
) {
    if deferred_target.is_none() {
        *ignored = None;
        return;
    }
    if fresh && !already_deferred {
        *ignored = observed_foreground;
    }
}

/// One refresh of a running plan: displayed frames, same-target clock, deferred focus.
/// Returns actions to present and whether `restart` should run (new plan only).
pub fn poll_refresh(
    animation: &mut Animation,
    placements: &Placements,
    prev: &Snapshot,
    next: &Snapshot,
    actions: Vec<NativeAction>,
    enumerated_focus: Option<&str>,
    ignored_foreground: Option<&str>,
    duration: Duration,
    now: Instant,
) -> (Vec<NativeAction>, bool) {
    let pending = animation.deferred_focus().cloned();
    let target = animation.deferred_focus_id();
    let adopt = target.is_some_and(|target| {
        enumerated_focus.is_some_and(|id| {
            id != target
                && ignored_foreground != Some(id)
                && next.focused_window.as_deref() == Some(id)
        })
    });
    if adopt {
        // Native focus is already the clicked window. Drop the slide's activation so the
        // finishing frame cannot steal it back; do not emit another Focus (a palette or
        // dialog must not be overridden either).
        animation.discard_deferred_focus();
    }
    let mut actions: Vec<_> = actions
        .into_iter()
        .filter(|a| !matches!(a, NativeAction::Focus { .. }))
        .collect();
    if let Some(focus) = pending.filter(|_| !adopt) {
        actions.push(focus);
    }
    let from = animation.retarget_from(placements, &next.windows);
    let actions = animation.start(prev, next, &from, actions, duration, now);
    (actions, animation.started_fresh())
}

/// Tiled windows only; floating and layout fullscreen windows settle immediately.
fn locate<'a>(snapshot: &'a Snapshot, id: &str) -> Option<(&'a MonitorState, &'a Page)> {
    if snapshot
        .windows
        .iter()
        .any(|w| w.native.id == id && (w.floating || w.fullscreen))
    {
        return None;
    }
    snapshot.monitors.iter().find_map(|m| {
        m.pages
            .iter()
            .find(|p| p.columns.iter().any(|c| c.windows.iter().any(|w| w == id)))
            .map(|p| (m, p))
    })
}

/// (from, to, bounds, hidden) for an animated window; None applies the target immediately.
fn plan_move(
    prev: &Snapshot,
    next: &Snapshot,
    applied: &Placements,
    id: &str,
    rect: Rect,
    minimized: bool,
) -> Option<(Rect, Rect, Rect, bool)> {
    let (monitor, page) = locate(next, id)?;
    let (old_monitor, old_page) = locate(prev, id)?;
    if old_monitor.monitor.id != monitor.monitor.id {
        return None; // Crossing monitors (and DPI) mid-animation is not worth the risk.
    }
    let &(from, _, from_hidden) = applied.get(id)?;
    let bounds = expand_gap(
        monitor.viewport,
        half_gap(next.gaps, monitor.monitor.scale_factor),
    );
    let visible = |r: Rect| clip_to_viewport(r, bounds).is_some();
    let (was_active, is_active) = (&old_monitor.active_page, &monitor.active_page);
    let shown_before = &old_page.id == was_active;
    let shown_after = &page.id == is_active;
    if shown_before && shown_after {
        // Hidden inside the viewport (e.g. behind a fullscreen neighbour) is not a start frame.
        if (from_hidden && visible(from)) || (minimized && visible(rect)) {
            return None;
        }
        return Some((from, rect, bounds, from_hidden));
    }
    // Page switch: later pages sit below (niri), so moving down slides everything up.
    let position = |id: &String| {
        monitor
            .pages
            .iter()
            .position(|p| &p.id == id)
            .or_else(|| old_monitor.pages.iter().position(|p| &p.id == id))
    };
    let dy = match (position(was_active), position(is_active)) {
        (Some(a), Some(b)) if a < b => i64::from(bounds.height),
        (Some(a), Some(b)) if a > b => -i64::from(bounds.height),
        _ => return None,
    };
    if shown_after && &old_page.id == is_active && !minimized {
        return Some((shift(rect, dy), rect, bounds, true)); // Entering from below/above.
    }
    if shown_before && &page.id == was_active && !from_hidden {
        return Some((from, shift(rect, -dy), bounds, false)); // Leaving; ends minimized.
    }
    None
}

fn shift(rect: Rect, dy: i64) -> Rect {
    Rect {
        y: (i64::from(rect.y) + dy).clamp(i32::MIN.into(), i32::MAX.into()) as i32,
        ..rect
    }
}

fn rect_components(rect: Rect) -> [f64; 4] {
    [
        rect.x.into(),
        rect.y.into(),
        rect.width.into(),
        rect.height.into(),
    ]
}

/// Analytic critically damped spring, with a small Hermite tail correction so the configured
/// duration still ends exactly at the target, at rest. No integration step depends on cadence.
/// Ported to `ui/src/overview/zoom.ts` for the overview zoom; keep both in step.
pub(crate) fn spring(from: f64, to: f64, velocity: f64, elapsed: f64, duration: f64) -> (f64, f64) {
    if elapsed <= 0.0 {
        return (from, velocity);
    }
    if elapsed >= duration {
        return (to, 0.0);
    }
    let omega = 6.0 / duration;
    let displacement = from - to;
    let coefficient = velocity + omega * displacement;
    let decay = (-omega * elapsed).exp();
    let end_decay = (-omega * duration).exp();
    let residual = (displacement + coefficient * duration) * end_decay;
    let end_velocity = (velocity - omega * coefficient * duration) * end_decay;
    let t = elapsed / duration;
    let position = to + (displacement + coefficient * elapsed) * decay
        - residual * (3.0 * t * t - 2.0 * t * t * t)
        - duration * end_velocity * (t * t * t - t * t);
    let velocity = (velocity - omega * coefficient * elapsed) * decay
        - residual * (6.0 * t - 6.0 * t * t) / duration
        - end_velocity * (3.0 * t * t - 2.0 * t);
    (position, velocity)
}

fn clip_to_viewport(rect: Rect, viewport: Rect) -> Option<Rect> {
    let left = i64::from(rect.x).max(i64::from(viewport.x));
    let top = i64::from(rect.y).max(i64::from(viewport.y));
    let right = (i64::from(rect.x) + i64::from(rect.width))
        .min(i64::from(viewport.x) + i64::from(viewport.width));
    let bottom = (i64::from(rect.y) + i64::from(rect.height))
        .min(i64::from(viewport.y) + i64::from(viewport.height));
    (left < right && top < bottom).then(|| Rect {
        x: left as i32,
        y: top as i32,
        width: (right - left) as u32,
        height: (bottom - top) as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{layout::Engine, model::*};

    const MS: fn(u64) -> Duration = Duration::from_millis;

    fn fixture() -> (Engine, Placements) {
        let mut engine = Engine::new(BackendStatus {
            availability: BackendAvailability::Ready,
            capabilities: Capabilities {
                enumerate: true,
                placement: true,
                minimize: true,
                focus: true,
                clipping: true,
                ..Capabilities::default()
            },
            ..BackendStatus::default()
        });
        engine.set_gaps(0);
        let area = Rect {
            x: -1000,
            y: 0,
            width: 1000,
            height: 800,
        };
        engine
            .reconcile(SystemSnapshot {
                monitors: vec![Monitor {
                    id: "m".into(),
                    name: "m".into(),
                    bounds: area,
                    work_area: area,
                    scale_factor: 1.0,
                    primary: true,
                }],
                windows: (0..4)
                    .map(|i| NativeWindow {
                        id: i.to_string(),
                        title: String::new(),
                        app_name: String::new(),
                        process_id: 1,
                        monitor_id: "m".into(),
                        rect: area,
                        minimized: false,
                        minimized_by_manager: false,
                        resizable: true,
                    })
                    .collect(),
                focused_window: Some("0".into()),
            })
            .unwrap();
        let actions = engine.dispatch(Command::Enable).unwrap().actions;
        let mut applied = Placements::new();
        remember(&mut applied, &actions);
        (engine, applied)
    }

    fn remember(applied: &mut Placements, actions: &[NativeAction]) {
        for action in actions {
            if let NativeAction::Placement {
                window_id,
                rect,
                clip,
                minimized,
            } = action
            {
                applied.insert(window_id.clone(), (*rect, *clip, *minimized));
            }
        }
    }

    /// Dispatch and start an animation from the currently applied frames.
    fn run(
        engine: &mut Engine,
        animation: &mut Animation,
        applied: &mut Placements,
        command: Command,
        now: Instant,
    ) -> Vec<NativeAction> {
        let prev = engine.snapshot().clone();
        let target = engine.dispatch(command).unwrap().actions;
        let initial = animation.start(&prev, engine.snapshot(), applied, target, MS(160), now);
        remember(applied, &initial);
        initial
    }

    fn json(value: impl serde::Serialize) -> serde_json::Value {
        serde_json::to_value(value).unwrap()
    }

    fn scroll(delta: i32) -> Command {
        Command::Scroll {
            monitor_id: "m".into(),
            delta,
        }
    }

    fn assert_frames_stay_on_monitor(frame: &[NativeAction]) {
        for action in frame {
            if let NativeAction::Placement {
                rect,
                clip,
                minimized,
                ..
            } = action
            {
                let visible = clip.unwrap_or(*rect);
                assert_eq!(*minimized, clip_to_viewport(*rect, VIEWPORT).is_none());
                if !minimized {
                    assert_eq!(clip_to_viewport(visible, VIEWPORT), Some(visible));
                }
            }
        }
    }
    const VIEWPORT: Rect = Rect {
        x: -1000,
        y: 0,
        width: 1000,
        height: 800,
    };

    #[test]
    fn spring_is_monotone_from_rest_and_stops_exactly() {
        for (from, to) in [(f64::from(i32::MIN), f64::from(i32::MAX)), (5.0, -3.0)] {
            let mut previous = from;
            for step in 0..=100 {
                let (x, velocity) = spring(from, to, 0.0, f64::from(step) / 100.0, 1.0);
                assert!(if from <= to {
                    x >= previous
                } else {
                    x <= previous
                });
                assert!(x.is_finite() && velocity.is_finite());
                previous = x;
            }
            assert_eq!(previous, to);
            assert_eq!(spring(from, to, 0.0, 0.0, 1.0), (from, 0.0));
            assert_eq!(spring(from, to, 0.0, 1.0, 1.0), (to, 0.0));
            let (near_end, speed) = spring(from, to, 0.0, 1.0 - 1e-8, 1.0);
            assert!((near_end - to).abs() < 1e-5);
            assert!(speed.abs() < (to - from).abs() * 1e-6);
        }
    }

    #[test]
    fn retarget_carries_displayed_velocity_even_on_reversal_and_restart() {
        let (mut engine, mut applied) = fixture();
        let mut animation = Animation::default();
        let now = Instant::now();
        run(&mut engine, &mut animation, &mut applied, scroll(400), now);
        remember(&mut applied, &animation.frame(now + MS(48)));
        let old = &animation.plan.as_ref().unwrap().moves[0];
        let displayed = old.current;
        let velocity = old.velocity;
        assert!(velocity[0].abs() > 1.0);
        run(
            &mut engine,
            &mut animation,
            &mut applied,
            scroll(-300),
            now + MS(55),
        );
        let retarget = &animation.plan.as_ref().unwrap().moves[0];
        assert_eq!(retarget.current, displayed);
        assert_eq!(retarget.velocity, velocity);
        assert_eq!(retarget.initial_velocity, velocity);
        // Even preparation delays preserve the displayed frame's tangent, not extrapolated time.
        animation.restart(now + MS(70));
        let retarget = &animation.plan.as_ref().unwrap().moves[0];
        assert_eq!(retarget.from, displayed);
        assert_eq!(retarget.initial_velocity, velocity);
        let epsilon = 1e-7;
        for (i, (&from, &to)) in rect_components(retarget.from)
            .iter()
            .zip(&rect_components(retarget.to))
            .enumerate()
        {
            let (position, speed) = spring(from, to, velocity[i], epsilon, 0.160);
            assert!(((position - from) / epsilon - velocity[i]).abs() < 0.1);
            assert!((speed - velocity[i]).abs() < 0.1);
        }
        remember(&mut applied, &animation.frame(now + MS(230)));
        assert!(animation.deadline().is_none());
        run(
            &mut engine,
            &mut animation,
            &mut applied,
            scroll(100),
            now + MS(240),
        );
        assert!(
            animation
                .plan
                .as_ref()
                .unwrap()
                .moves
                .iter()
                .all(|m| m.velocity == [0.0; 4])
        );
    }

    #[test]
    fn retarget_at_displayed_position_still_brakes_existing_motion() {
        let (mut engine, mut applied) = fixture();
        let mut animation = Animation::default();
        let now = Instant::now();
        run(&mut engine, &mut animation, &mut applied, scroll(400), now);
        remember(&mut applied, &animation.frame(now + MS(48)));
        let old = animation.plan.as_ref().unwrap().moves.iter()
            .find(|m| matches!(&m.action, NativeAction::Placement { window_id, .. } if window_id == "0"))
            .unwrap();
        let velocity = old.velocity;
        let displayed = old.current;
        let target = NativeAction::Placement {
            window_id: "0".into(),
            rect: displayed,
            clip: None,
            minimized: false,
        };
        let snapshot = engine.snapshot();
        animation.start(
            snapshot,
            snapshot,
            &applied,
            vec![target.clone()],
            MS(160),
            now + MS(48),
        );
        let movement = &animation.plan.as_ref().unwrap().moves[0];
        assert_eq!(movement.from, movement.to);
        assert_eq!(movement.velocity, velocity);
        assert!(velocity[0].abs() > 1.0);
        assert_eq!(animation.frame(now + MS(208)), vec![target]);
    }

    #[test]
    fn irregular_frames_use_elapsed_time_skip_ticks_and_finish_after_a_long_gap() {
        let (mut engine, mut applied) = fixture();
        let prev = engine.snapshot().clone();
        let target = engine.dispatch(scroll(400)).unwrap().actions;
        let now = Instant::now();
        let mut regular = Animation::default();
        let mut sparse = Animation::default();
        for animation in [&mut regular, &mut sparse] {
            animation.start(
                &prev,
                engine.snapshot(),
                &applied,
                target.clone(),
                MS(160),
                now,
            );
        }
        for at in [16, 32, 48, 64, 80] {
            regular.frame(now + MS(at));
        }
        sparse.frame(now + MS(37));
        assert_eq!(sparse.deadline(), Some(now + MS(48)));
        assert!(sparse.frame(now + MS(40)).is_empty());
        assert_eq!(
            json(regular.frame(now + MS(97))),
            json(sparse.frame(now + MS(97)))
        );
        assert_eq!(sparse.deadline(), Some(now + MS(112)));
        remember(&mut applied, &sparse.frame(now + Duration::from_secs(30)));
        assert_eq!(
            json(sparse.frame(now + Duration::from_secs(31))),
            json(Vec::<NativeAction>::new())
        );
        assert!(sparse.deadline().is_none());
        for action in target {
            if let NativeAction::Placement {
                window_id,
                rect,
                clip,
                minimized,
            } = action
            {
                assert_eq!(applied[&window_id], (rect, clip, minimized));
            }
        }
    }

    #[test]
    fn zero_duration_and_disabled_retarget_drop_momentum_and_pending_frames() {
        for disabled in [false, true] {
            let (mut engine, mut applied) = fixture();
            let mut animation = Animation::default();
            let now = Instant::now();
            run(&mut engine, &mut animation, &mut applied, scroll(400), now);
            remember(&mut applied, &animation.frame(now + MS(48)));
            let prev = engine.snapshot().clone();
            let target = engine.dispatch(scroll(-300)).unwrap().actions;
            let mut next = engine.snapshot().clone();
            next.enabled = !disabled;
            let duration = if disabled { MS(160) } else { Duration::ZERO };
            let actions = animation.start(
                &prev,
                &next,
                &applied,
                target.clone(),
                duration,
                now + MS(48),
            );
            assert_eq!(json(actions), json(target));
            assert!(animation.deadline().is_none());
            assert!(animation.frame(now + MS(1000)).is_empty());
        }
    }

    #[test]
    fn scroll_frames_clip_retarget_and_end_exactly() {
        let (mut engine, mut applied) = fixture();
        let mut animation = Animation::default();
        let now = Instant::now();
        run(&mut engine, &mut animation, &mut applied, scroll(400), now);
        assert_eq!(animation.deadline(), Some(now + FRAME_INTERVAL));
        assert!(animation.frame(now + MS(15)).is_empty());
        let middle = animation.frame(now + MS(80));
        assert!(!middle.is_empty());
        assert_frames_stay_on_monitor(&middle);
        remember(&mut applied, &middle);
        let mid = applied["0"].0;
        // Retarget from the frame actually on screen, not from the old target.
        let initial = run(
            &mut engine,
            &mut animation,
            &mut applied,
            scroll(-300),
            now + MS(80),
        );
        assert!(initial.iter().any(
            |a| matches!(a, NativeAction::Placement { window_id, rect, .. } if window_id == "0" && *rect == mid)
        ));
        remember(&mut applied, &animation.frame(now + MS(240)));
        assert!(animation.deadline().is_none());
        // The last frame is the exact engine plan for every window.
        let plan = engine.clone().dispatch(scroll(0)).unwrap().actions;
        for action in plan {
            if let NativeAction::Placement {
                window_id,
                rect,
                minimized,
                ..
            } = action
            {
                assert_eq!(
                    (applied[&window_id].0, applied[&window_id].2),
                    (rect, minimized)
                );
            }
        }
        animation.cancel();
        assert!(
            engine
                .dispatch(Command::Disable)
                .unwrap()
                .actions
                .iter()
                .all(|a| matches!(a, NativeAction::Restore { .. }))
        );
        assert!(animation.frame(now + Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn width_changes_animate_size_and_neighbours() {
        let (mut engine, mut applied) = fixture();
        let mut animation = Animation::default();
        let now = Instant::now();
        let before = applied["0"].0;
        run(
            &mut engine,
            &mut animation,
            &mut applied,
            Command::AdjustColumnWidth { delta: 200 },
            now,
        );
        let middle = animation.frame(now + MS(80));
        remember(&mut applied, &middle);
        let width = applied["0"].0.width;
        assert!(width > before.width && width < before.width + 200);
        remember(&mut applied, &animation.frame(now + MS(160)));
        assert_eq!(applied["0"].0.width, before.width + 200);
    }

    #[test]
    fn focus_waits_only_while_its_window_is_offscreen() {
        let (mut engine, mut applied) = fixture();
        let mut animation = Animation::default();
        let now = Instant::now();
        // Window 3 starts offscreen: its focus waits for the final placement.
        let initial = run(
            &mut engine,
            &mut animation,
            &mut applied,
            Command::FocusWindow {
                window_id: "3".into(),
            },
            now,
        );
        assert!(
            !initial
                .iter()
                .any(|a| matches!(a, NativeAction::Focus { .. }))
        );
        // A retarget without its own focus (Scroll) keeps the pending focus.
        run(
            &mut engine,
            &mut animation,
            &mut applied,
            scroll(10),
            now + MS(32),
        );
        assert!(matches!(
            animation.frame(now + MS(400)).last(),
            Some(NativeAction::Focus { window_id }) if window_id == "3"
        ));
        // Window 3 is now visible: focusing a visible neighbour is immediate.
        let initial = run(
            &mut engine,
            &mut animation,
            &mut applied,
            Command::FocusDirection {
                direction: Direction::Left,
            },
            now + MS(500),
        );
        let focused = engine.snapshot().focused_window.clone().unwrap();
        let (rect, _, hidden) = applied[&focused];
        assert_eq!(
            initial
                .iter()
                .any(|a| matches!(a, NativeAction::Focus { .. })),
            !hidden && clip_to_viewport(rect, VIEWPORT) == Some(rect)
        );
    }

    #[test]
    fn page_switch_slides_old_page_out_and_new_page_in() {
        let (mut engine, mut applied) = fixture();
        let mut animation = Animation::default();
        let now = Instant::now();
        let tail = engine.snapshot().monitors[0]
            .pages
            .last()
            .unwrap()
            .id
            .clone();
        let first = engine.snapshot().monitors[0].pages[0].id.clone();
        // Move window 3 to page 2 without animation noise, then switch pages.
        let actions = engine
            .dispatch(Command::MoveWindowToPage {
                window_id: "3".into(),
                page_id: tail.clone(),
            })
            .unwrap()
            .actions;
        remember(&mut applied, &actions);
        if engine.snapshot().monitors[0].active_page != first {
            let actions = engine
                .dispatch(Command::SwitchPage {
                    monitor_id: "m".into(),
                    page_id: first.clone(),
                })
                .unwrap()
                .actions;
            remember(&mut applied, &actions);
        }
        let leaving = applied["0"];
        assert!(!leaving.2 && applied["3"].2);
        run(
            &mut engine,
            &mut animation,
            &mut applied,
            Command::SwitchPage {
                monitor_id: "m".into(),
                page_id: tail,
            },
            now,
        );
        let middle = animation.frame(now + MS(80));
        assert_frames_stay_on_monitor(&middle);
        remember(&mut applied, &middle);
        assert!(applied["0"].0.y < leaving.0.y, "old page moves up");
        let entering = applied["3"].0;
        let last = animation.frame(now + MS(160));
        remember(&mut applied, &last);
        assert!(entering.y > applied["3"].0.y, "new page comes from below");
        assert!(applied["0"].2, "old page ends minimized");
        assert!(!applied["3"].2);
    }

    #[test]
    fn no_clipping_zero_duration_and_unknown_starts_settle_immediately() {
        for case in 0..3 {
            let (mut engine, mut applied) = fixture();
            let prev = engine.snapshot().clone();
            let target = engine.dispatch(scroll(600)).unwrap().actions;
            let mut next = engine.snapshot().clone();
            let mut duration = MS(160);
            match case {
                0 => next.backend.capabilities.clipping = false,
                1 => duration = Duration::ZERO,
                _ => applied.clear(),
            }
            let mut animation = Animation::default();
            let initial = animation.start(
                &prev,
                &next,
                &applied,
                target.clone(),
                duration,
                Instant::now(),
            );
            assert_eq!(json(initial), json(target));
            assert!(animation.deadline().is_none());
        }
    }
    #[test]
    fn discarding_deferred_focus_keeps_the_placement_clock() {
        let (mut engine, mut applied) = fixture();
        let mut animation = Animation::default();
        let now = Instant::now();
        let prev = engine.snapshot().clone();
        let target = engine
            .dispatch(Command::FocusWindow {
                window_id: "3".into(),
            })
            .unwrap()
            .actions;
        let initial = animation.start(&prev, engine.snapshot(), &applied, target.clone(), MS(160), now);
        remember(&mut applied, &initial);
        assert!(animation.deferred_focus_id().is_some());
        let deadline = animation.deadline();
        assert!(deadline.is_some());
        animation.discard_deferred_focus();
        assert!(animation.deferred_focus_id().is_none());
        assert_eq!(animation.deadline(), deadline);
        let placements: Vec<_> = target
            .into_iter()
            .filter(|action| !matches!(action, NativeAction::Focus { .. }))
            .collect();
        let (actions, fresh) = poll_refresh(
            &mut animation,
            &applied,
            &prev,
            engine.snapshot(),
            placements,
            None,
            None,
            MS(160),
            now + MS(40),
        );
        assert!(!fresh);
        assert!(actions.iter().all(|action| !matches!(action, NativeAction::Focus { .. })));
        assert_eq!(animation.deadline(), deadline);
    }

    #[test]
    fn a_paused_foreground_drops_only_the_focus_that_was_already_waiting() {
        let (mut engine, mut applied) = fixture();
        let mut animation = Animation::default();
        let now = Instant::now();
        // Controller::run records foreground context before dispatch/start. A user command
        // made before fullscreen begins must not gain a permanent activation exemption.
        animation.note_user_focus(false);
        let prev = engine.snapshot().clone();
        let target = engine
            .dispatch(Command::FocusWindow {
                window_id: "3".into(),
            })
            .unwrap()
            .actions;
        let initial = animation.start(
            &prev,
            engine.snapshot(),
            &applied,
            target.clone(),
            MS(160),
            now,
        );
        remember(&mut applied, &initial);
        assert_eq!(animation.deferred_focus_id(), Some("3"));
        let deadline = animation.deadline();
        animation.discard_stale_focus();
        assert!(animation.deferred_focus_id().is_none());
        let placements: Vec<_> = target
            .iter()
            .filter(|action| !matches!(action, NativeAction::Focus { .. }))
            .cloned()
            .collect();
        let (actions, fresh) = poll_refresh(
            &mut animation,
            &applied,
            &prev,
            engine.snapshot(),
            placements.clone(),
            None,
            None,
            MS(160),
            now + MS(40),
        );
        assert!(!fresh);
        assert!(actions.iter().all(|action| !matches!(action, NativeAction::Focus { .. })));
        assert_eq!(animation.deferred_focus_id(), None);
        assert_eq!(animation.deadline(), deadline);

        animation.note_user_focus(true);
        let again = animation.start(
            &prev,
            engine.snapshot(),
            &applied,
            target.clone(),
            MS(160),
            now + MS(48),
        );
        remember(&mut applied, &again);
        assert_eq!(animation.deferred_focus_id(), Some("3"));
        animation.discard_stale_focus();
        assert_eq!(
            animation.deferred_focus_id(),
            Some("3"),
            "explicit focus on the live monitor stays"
        );
        let (actions, _) = poll_refresh(
            &mut animation,
            &applied,
            &prev,
            engine.snapshot(),
            placements,
            None,
            None,
            MS(160),
            now + MS(64),
        );
        assert!(actions.iter().all(|action| !matches!(action, NativeAction::Focus { .. })));
        assert_eq!(animation.deferred_focus_id(), Some("3"));
    }

    #[test]
    fn polling_preserves_the_slide_clock_and_deferred_focus() {
        let (mut engine, mut applied) = fixture();
        let mut animation = Animation::default();
        let now = Instant::now();
        run(
            &mut engine,
            &mut animation,
            &mut applied,
            Command::FocusWindow {
                window_id: "3".into(),
            },
            now,
        );
        assert!(
            matches!(animation.deferred_focus(), Some(NativeAction::Focus { window_id }) if window_id == "3")
        );
        let frame = animation.frame(now + MS(32));
        remember(&mut applied, &frame);
        let deadline = animation.deadline();
        let prev = engine.snapshot().clone();
        let observed = SystemSnapshot {
            monitors: prev.monitors.iter().map(|m| m.monitor.clone()).collect(),
            windows: prev.windows.iter().map(|w| w.native.clone()).collect(),
            focused_window: None, // Controller ignores stale focus while activation is deferred.
        };
        let mut transition = engine.reconcile(observed).unwrap();
        transition
            .actions
            .push(animation.deferred_focus().unwrap().clone());
        let initial = animation.start(
            &prev,
            &transition.snapshot,
            &applied,
            transition.actions,
            MS(160),
            now + MS(40),
        );
        assert!(initial.is_empty());
        assert_eq!(animation.deadline(), deadline);
        let final_frame = animation.frame(now + MS(160));
        assert!(
            matches!(final_frame.last(), Some(NativeAction::Focus { window_id }) if window_id == "3")
        );
        assert!(animation.deadline().is_none());
    }
    #[test]
    fn entering_partial_window_is_not_raised_over_the_neighbouring_monitor() {
        let (mut engine, mut applied) = fixture();
        let mut animation = Animation::default();
        let now = Instant::now();
        run(&mut engine, &mut animation, &mut applied, scroll(400), now);
        let frame = animation.frame(now + MS(32));
        remember(&mut applied, &frame);
        let (rect, _, hidden) = applied["2"];
        assert!(!hidden);
        assert_ne!(clip_to_viewport(rect, VIEWPORT), Some(rect));
        let initial = run(
            &mut engine,
            &mut animation,
            &mut applied,
            Command::FocusWindow {
                window_id: "2".into(),
            },
            now + MS(32),
        );
        assert!(
            !initial
                .iter()
                .any(|a| matches!(a, NativeAction::Focus { .. }))
        );
        assert!(
            matches!(animation.deferred_focus(), Some(NativeAction::Focus { window_id }) if window_id == "2")
        );
        assert!(
            matches!(animation.frame(now + MS(192)).last(), Some(NativeAction::Focus { window_id }) if window_id == "2")
        );
    }

    fn placement_actions(actions: &[NativeAction]) -> Vec<NativeAction> {
        actions
            .iter()
            .filter(|a| matches!(a, NativeAction::Placement { .. }))
            .cloned()
            .collect()
    }

    #[test]
    fn same_target_poll_keeps_the_clock_when_present_restarts() {
        let (mut engine, mut applied) = fixture();
        let mut animation = Animation::default();
        let now = Instant::now();
        let prev = engine.snapshot().clone();
        let target = engine
            .dispatch(Command::FocusWindow {
                window_id: "3".into(),
            })
            .unwrap()
            .actions;
        let initial = animation.start(
            &prev,
            engine.snapshot(),
            &applied,
            target.clone(),
            MS(160),
            now,
        );
        remember(&mut applied, &initial);
        assert!(animation.started_fresh());
        animation.restart(now + MS(20));
        assert_eq!(
            animation.deadline(),
            Some(now + MS(20) + FRAME_INTERVAL.min(MS(160)))
        );
        assert!(!animation.started_fresh());
        let running = animation.deadline();
        animation.restart(now + MS(30));
        assert_eq!(animation.deadline(), running, "restart is one-shot");

        remember(&mut applied, &animation.frame(now + MS(52)));
        let deadline = animation.deadline();
        let native = engine
            .snapshot()
            .windows
            .iter()
            .find(|w| w.native.id == "0")
            .unwrap()
            .native
            .rect;
        assert_ne!(applied["0"].0, native);
        let snapshot = engine.snapshot().clone();
        let (_actions, fresh) = poll_refresh(
            &mut animation,
            &applied,
            &snapshot,
            &snapshot,
            placement_actions(&target),
            Some("0"),
            Some("0"),
            MS(160),
            now + MS(60),
        );
        assert!(!fresh);
        assert_eq!(animation.deadline(), deadline);
        assert_eq!(animation.deferred_focus_id(), Some("3"));
        animation.restart(now + MS(60));
        assert_eq!(animation.deadline(), deadline);
    }

    #[test]
    fn retarget_starts_from_displayed_frames_when_native_rects_differ() {
        let (mut engine, mut applied) = fixture();
        let mut animation = Animation::default();
        let now = Instant::now();
        run(&mut engine, &mut animation, &mut applied, scroll(400), now);
        remember(&mut applied, &animation.frame(now + MS(80)));
        let mid = applied["0"].0;
        let native = engine
            .snapshot()
            .windows
            .iter()
            .find(|w| w.native.id == "0")
            .unwrap()
            .native
            .rect;
        assert_ne!(mid, native);
        let prev = engine.snapshot().clone();
        let target = engine.dispatch(scroll(-300)).unwrap().actions;
        let end = target.iter().find_map(|a| match a {
            NativeAction::Placement {
                window_id, rect, ..
            } if window_id == "0" => Some(*rect),
            _ => None,
        });
        assert_ne!(Some(mid), end);
        let (initial, fresh) = poll_refresh(
            &mut animation,
            &applied,
            &prev,
            engine.snapshot(),
            target,
            None,
            None,
            MS(160),
            now + MS(80),
        );
        assert!(fresh);
        assert!(initial.iter().any(|a| {
            matches!(a, NativeAction::Placement { window_id, rect, .. } if window_id == "0" && *rect == mid)
        }));
    }

    #[test]
    fn user_resize_outside_animation_is_not_a_displayed_frame() {
        let (mut engine, applied) = fixture();
        let mut mismatched = engine.snapshot().windows.clone();
        for window in &mut mismatched {
            window.native.rect.x += 40;
        }
        let from = Animation::default().retarget_from(&applied, &mismatched);
        assert!(from.is_empty());
        let mut matched = engine.snapshot().windows.clone();
        for window in &mut matched {
            if let Some((rect, _, hidden)) = applied.get(&window.native.id) {
                window.native.rect = *rect;
                window.native.minimized = *hidden;
                window.native.minimized_by_manager = *hidden;
            }
        }
        assert_eq!(
            Animation::default().retarget_from(&applied, &matched).len(),
            applied.len()
        );
        let prev = engine.snapshot().clone();
        let target = engine.dispatch(scroll(300)).unwrap().actions;
        let mut animation = Animation::default();
        let initial = animation.start(
            &prev,
            engine.snapshot(),
            &from,
            target.clone(),
            MS(160),
            Instant::now(),
        );
        assert_eq!(json(initial), json(target));
        assert!(animation.deadline().is_none());
    }

    #[test]
    fn stale_foreground_keeps_deferred_focus_and_a_click_replaces_it() {
        let (mut engine, mut applied) = fixture();
        let mut animation = Animation::default();
        let now = Instant::now();
        let prev = engine.snapshot().clone();
        let target = engine
            .dispatch(Command::FocusWindow {
                window_id: "3".into(),
            })
            .unwrap()
            .actions;
        let initial = animation.start(
            &prev,
            engine.snapshot(),
            &applied,
            target.clone(),
            MS(160),
            now,
        );
        remember(&mut applied, &initial);
        remember(&mut applied, &animation.frame(now + MS(32)));
        let deadline = animation.deadline();
        let snapshot = engine.snapshot().clone();
        let actions = placement_actions(&target);
        let (_kept, fresh) = poll_refresh(
            &mut animation,
            &applied,
            &snapshot,
            &snapshot,
            actions.clone(),
            Some("0"),
            Some("0"),
            MS(160),
            now + MS(40),
        );
        assert!(!fresh);
        assert_eq!(animation.deadline(), deadline);
        assert_eq!(animation.deferred_focus_id(), Some("3"));
        animation.restart(now + MS(40));
        assert_eq!(animation.deadline(), deadline);

        // Enumerated focus changed, but reconcile left the slide target in place.
        let (_kept, fresh) = poll_refresh(
            &mut animation,
            &applied,
            &snapshot,
            &snapshot,
            actions.clone(),
            Some("1"),
            Some("0"),
            MS(160),
            now + MS(48),
        );
        assert!(!fresh);
        assert_eq!(animation.deferred_focus_id(), Some("3"));

        let mut clicked = snapshot.clone();
        clicked.focused_window = Some("1".into());
        let (replaced, fresh) = poll_refresh(
            &mut animation,
            &applied,
            &snapshot,
            &clicked,
            actions,
            Some("1"),
            Some("0"),
            MS(160),
            now + MS(56),
        );
        assert!(!fresh);
        assert_eq!(animation.deadline(), deadline);
        assert!(animation.deferred_focus_id().is_none());
        assert!(
            !replaced
                .iter()
                .any(|a| matches!(a, NativeAction::Focus { window_id } if window_id == "3"))
        );
        assert!(
            !animation
                .frame(now + MS(400))
                .iter()
                .any(|a| matches!(a, NativeAction::Focus { window_id } if window_id == "3"))
        );
    }

    #[test]
    fn deferred_focus_ignores_only_the_foreground_it_left() {
        assert!(!suppress_observed_focus(None, Some("0"), Some("0"), true));
        assert!(suppress_observed_focus(
            Some("3"),
            Some("0"),
            Some("0"),
            true
        ));
        assert!(suppress_observed_focus(
            Some("3"),
            Some("3"),
            Some("0"),
            true
        ));
        assert!(suppress_observed_focus(Some("3"), None, Some("0"), true));
        assert!(suppress_observed_focus(
            Some("3"),
            Some("1"),
            Some("0"),
            false
        ));
        assert!(!suppress_observed_focus(
            Some("3"),
            Some("1"),
            Some("0"),
            true
        ));
        assert!(!suppress_observed_focus(Some("3"), Some("0"), None, true));

        let mut ignored = None;
        note_ignored_foreground(&mut ignored, true, false, Some("3"), Some("0".into()));
        assert_eq!(ignored.as_deref(), Some("0"));
        note_ignored_foreground(&mut ignored, true, true, Some("4"), Some("3".into()));
        assert_eq!(
            ignored.as_deref(),
            Some("0"),
            "retarget keeps the original foreground"
        );
        note_ignored_foreground(&mut ignored, false, true, Some("4"), Some("9".into()));
        assert_eq!(ignored.as_deref(), Some("0"));
        note_ignored_foreground(&mut ignored, true, true, None, Some("3".into()));
        assert_eq!(ignored, None);
    }

    #[test]
    fn native_foreground_anchors_a_deferred_slide_not_the_layout_focus() {
        // Layout focus is "0". Native foreground is already "1". The command defers "3".
        let (mut engine, mut applied) = fixture();
        let mut animation = Animation::default();
        let now = Instant::now();
        assert_eq!(engine.snapshot().focused_window.as_deref(), Some("0"));
        let already_deferred = animation.deferred_focus_id().is_some();
        let prev = engine.snapshot().clone();
        let target = engine
            .dispatch(Command::FocusWindow {
                window_id: "3".into(),
            })
            .unwrap()
            .actions;
        let initial = animation.start(
            &prev,
            engine.snapshot(),
            &applied,
            target.clone(),
            MS(160),
            now,
        );
        remember(&mut applied, &initial);
        let mut ignored = None;
        note_ignored_foreground(
            &mut ignored,
            animation.started_fresh(),
            already_deferred,
            animation.deferred_focus_id(),
            Some("1".into()),
        );
        assert_eq!(ignored.as_deref(), Some("1"));
        assert_ne!(
            ignored.as_deref(),
            engine.snapshot().focused_window.as_deref()
        );
        assert_eq!(animation.deferred_focus_id(), Some("3"));

        remember(&mut applied, &animation.frame(now + MS(32)));
        let deadline = animation.deadline();
        let snapshot = engine.snapshot().clone();
        let actions = placement_actions(&target);
        for at in [MS(40), MS(48)] {
            let (_kept, fresh) = poll_refresh(
                &mut animation,
                &applied,
                &snapshot,
                &snapshot,
                actions.clone(),
                Some("1"),
                ignored.as_deref(),
                MS(160),
                now + at,
            );
            assert!(!fresh);
            assert_eq!(animation.deadline(), deadline);
            assert_eq!(animation.deferred_focus_id(), Some("3"));
        }
        animation.restart(now + MS(48));
        assert_eq!(animation.deadline(), deadline);

        // A later real activation still cancels the deferred slide.
        let mut clicked = snapshot.clone();
        clicked.focused_window = Some("2".into());
        let (replaced, _fresh) = poll_refresh(
            &mut animation,
            &applied,
            &snapshot,
            &clicked,
            actions,
            Some("2"),
            ignored.as_deref(),
            MS(160),
            now + MS(56),
        );
        assert!(animation.deferred_focus_id().is_none());
        assert!(
            !replaced
                .iter()
                .any(|a| matches!(a, NativeAction::Focus { window_id } if window_id == "3"))
        );
        assert!(
            !animation
                .frame(now + MS(400))
                .iter()
                .any(|a| matches!(a, NativeAction::Focus { window_id } if window_id == "3"))
        );

        // No managed foreground is itself the anchor. A fresh retarget must not replace it
        // with the layout target.
        let mut animation = Animation::default();
        let (mut engine, mut applied) = fixture();
        let already_deferred = animation.deferred_focus_id().is_some();
        let prev = engine.snapshot().clone();
        let target = engine
            .dispatch(Command::FocusWindow {
                window_id: "3".into(),
            })
            .unwrap()
            .actions;
        let initial = animation.start(&prev, engine.snapshot(), &applied, target, MS(160), now);
        remember(&mut applied, &initial);
        let mut ignored = None;
        note_ignored_foreground(
            &mut ignored,
            animation.started_fresh(),
            already_deferred,
            animation.deferred_focus_id(),
            None,
        );
        assert!(ignored.is_none());
        let already_deferred = animation.deferred_focus_id().is_some();
        assert!(already_deferred);
        let prev = engine.snapshot().clone();
        let retarget = engine.dispatch(scroll(-200)).unwrap().actions;
        animation.start(
            &prev,
            engine.snapshot(),
            &applied,
            retarget,
            MS(160),
            now + MS(16),
        );
        assert!(animation.started_fresh(), "retarget installs a new plan");
        assert_eq!(animation.deferred_focus_id(), Some("3"));
        note_ignored_foreground(
            &mut ignored,
            animation.started_fresh(),
            already_deferred,
            animation.deferred_focus_id(),
            Some(engine.snapshot().focused_window.clone().unwrap()),
        );
        assert!(ignored.is_none());
    }
}
