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
    model::{MonitorState, NativeAction, Page, Rect, Snapshot},
};

/// Last successful native plans; minimized rectangles are logical (not moved by the backend).
pub type Placements = HashMap<String, (Rect, Option<Rect>, bool)>;
const FRAME_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Default)]
pub struct Animation {
    plan: Option<Plan>,
}

struct Plan {
    started: Instant,
    duration: Duration,
    next_frame: Instant,
    moves: Vec<Move>,
    /// Focus on a window that is still offscreen waits for its final placement.
    deferred: Vec<NativeAction>,
}

struct Move {
    /// The exact engine placement, applied when the animation ends or is finished early.
    action: NativeAction,
    from: Rect,
    to: Rect,
    /// Monitor viewport expanded by half a gap: the area frames may draw into.
    bounds: Rect,
    /// Minimized (or never shown) since the last frame: mask before restoring it.
    hidden: bool,
}

impl Move {
    fn frame(&mut self, t: f64) -> NativeAction {
        let NativeAction::Placement { window_id, .. } = &self.action else {
            unreachable!()
        };
        let rect = lerp_rect(self.from, self.to, t);
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

    /// Taking the plan invalidates every remaining frame before a native Restore can run.
    /// Returns the exact final actions that were still pending.
    pub fn cancel(&mut self) -> Vec<NativeAction> {
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
        // A focus still waiting in the old plan must survive a retarget that brings none.
        let is_focus = |a: &NativeAction| matches!(a, NativeAction::Focus { .. });
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
                    let mut m = Move {
                        action,
                        from,
                        to,
                        bounds,
                        hidden,
                    };
                    now_actions.push(m.frame(0.0));
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
                    && m.hidden
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
            deferred,
        });
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
        plan.next_frame = (now + FRAME_INTERVAL).min(plan.started + plan.duration);
        let t = ease(elapsed.as_secs_f64() / plan.duration.as_secs_f64());
        plan.moves.iter_mut().map(|m| m.frame(t)).collect()
    }
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
        if (from_hidden && visible(from))
            || (minimized && visible(rect))
            || (from == rect && from_hidden == minimized)
        {
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

/// niri's default ease-out cubic.
fn ease(t: f64) -> f64 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

fn lerp(from: i64, to: i64, t: f64) -> i64 {
    // i64 differences: MIN -> MAX spans more than an i32. Exact endpoints at t = 0 and 1.
    from + ((to - from) as f64 * t).round() as i64
}

fn lerp_rect(from: Rect, to: Rect, t: f64) -> Rect {
    let size = |a: u32, b: u32| lerp(a.into(), b.into(), t).clamp(1, u32::MAX.into()) as u32;
    let pos =
        |a: i32, b: i32| lerp(a.into(), b.into(), t).clamp(i32::MIN.into(), i32::MAX.into()) as i32;
    Rect {
        x: pos(from.x, to.x),
        y: pos(from.y, to.y),
        width: size(from.width, to.width),
        height: size(from.height, to.height),
    }
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
    fn interpolation_is_monotone_and_exact_at_the_ends() {
        for (from, to) in [(i64::from(i32::MIN), i64::from(i32::MAX)), (5, -3)] {
            let mut previous = from;
            for step in 0..=100 {
                let x = lerp(from, to, ease(f64::from(step) / 100.0));
                assert!(if from <= to {
                    x >= previous
                } else {
                    x <= previous
                });
                previous = x;
            }
            assert_eq!(previous, to);
            assert_eq!(lerp(from, to, ease(0.0)), from);
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
        let hidden = applied[&focused].2;
        assert_eq!(
            initial
                .iter()
                .any(|a| matches!(a, NativeAction::Focus { .. })),
            !hidden
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
}
