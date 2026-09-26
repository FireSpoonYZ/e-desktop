//! Controller-owned Scroll frames. No timers/threads survive cancellation or restoration.
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use crate::model::{NativeAction, Rect, Snapshot};

/// Last successful native plans; minimized rectangles are logical (not moved by the backend).
pub type Placements = HashMap<String, (Rect, Option<Rect>, bool)>;
const FRAME_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Default)]
pub struct ScrollAnimation {
    plan: Option<Plan>,
}

struct Plan {
    started: Instant,
    duration: Duration,
    next_frame: Instant,
    viewport: Rect,
    actions: Vec<NativeAction>,
    // Final-action index plus a successful visible frame or a still-hidden layout rectangle.
    moving: Vec<(usize, Rect)>,
}

impl ScrollAnimation {
    pub fn deadline(&self) -> Option<Instant> {
        self.plan.as_ref().map(|plan| plan.next_frame)
    }

    /// Taking the plan invalidates every remaining frame before a native Restore can run.
    pub fn cancel(&mut self) -> Vec<NativeAction> {
        self.plan.take().map_or_else(Vec::new, |plan| plan.actions)
    }

    pub fn start(
        &mut self,
        snapshot: &Snapshot,
        monitor_id: &str,
        applied: &Placements,
        actions: Vec<NativeAction>,
        duration: Duration,
        now: Instant,
    ) -> Vec<NativeAction> {
        self.cancel();
        let Some(monitor) = snapshot
            .monitors
            .iter()
            .find(|m| m.monitor.id == monitor_id)
            .filter(|_| {
                snapshot.enabled && snapshot.backend.capabilities.clipping && !duration.is_zero()
            })
        else {
            return actions;
        };
        let Some(page) = monitor.pages.iter().find(|p| p.id == monitor.active_page) else {
            return actions;
        };
        if snapshot.windows.iter().any(|w| {
            w.fullscreen
                && (page
                    .columns
                    .iter()
                    .any(|c| c.windows.contains(&w.native.id))
                    || page.floating_windows.contains(&w.native.id))
        }) {
            return actions;
        }
        let mut moving = Vec::new();
        for (index, action) in actions.iter().enumerate() {
            let NativeAction::Placement {
                window_id, rect, ..
            } = action
            else {
                continue;
            };
            if !page.columns.iter().any(|c| c.windows.contains(window_id))
                || !snapshot
                    .windows
                    .iter()
                    .any(|w| &w.native.id == window_id && !w.floating && !w.fullscreen)
            {
                continue;
            }
            let Some((from, _, minimized)) = applied.get(window_id) else {
                return actions; // No reliable starting plan: settle the whole transition.
            };
            // Controller only calls start for Scroll: page/viewport membership is unchanged.
            // An offscreen manager-minimized window can advance while hidden, but is only
            // restored at its first clipped frame. Engine omits user-minimized windows.
            if from.y != rect.y
                || from.width != rect.width
                || from.height != rect.height
                || (*minimized && clip_to_viewport(*from, monitor.viewport).is_some())
            {
                return actions;
            }
            if from.x != rect.x {
                moving.push((index, *from));
            }
        }
        if moving.is_empty() {
            return actions;
        }
        let plan = Plan {
            started: now,
            duration,
            next_frame: now + FRAME_INTERVAL.min(duration),
            viewport: monitor.viewport,
            actions,
            moving,
        };
        // Settle nonanimated placements now; focus (if any) follows final placement only.
        let initial = plan
            .actions
            .iter()
            .enumerate()
            .filter_map(|(index, action)| {
                if !matches!(action, NativeAction::Placement { .. }) {
                    return None;
                }
                Some(match plan.moving.iter().find(|(i, _)| *i == index) {
                    Some((_, from)) => frame_placement(action, *from, plan.viewport),
                    None => action.clone(),
                })
            })
            .collect();
        self.plan = Some(plan);
        initial
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
        plan.moving
            .iter()
            .map(|(index, from)| {
                let action = &plan.actions[*index];
                let NativeAction::Placement { rect, .. } = action else {
                    unreachable!()
                };
                let rect = Rect {
                    x: interpolate_x(from.x, rect.x, elapsed, plan.duration),
                    ..*rect
                };
                frame_placement(action, rect, plan.viewport)
            })
            .collect()
    }
}

fn interpolate_x(from: i32, to: i32, elapsed: Duration, duration: Duration) -> i32 {
    if duration.is_zero() || elapsed >= duration {
        return to;
    }
    let t = elapsed.as_secs_f64() / duration.as_secs_f64();
    let eased = t * t * (3.0 - 2.0 * t);
    // Subtract in i64: MIN -> MAX spans more than an i32. Endpoints are exact above.
    (f64::from(from) + (i64::from(to) - i64::from(from)) as f64 * eased).round() as i32
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

fn frame_placement(action: &NativeAction, rect: Rect, viewport: Rect) -> NativeAction {
    let NativeAction::Placement { window_id, .. } = action else {
        unreachable!()
    };
    let clip = clip_to_viewport(rect, viewport);
    // Always mask before a visible animated move, even on a fully contained frame.
    // Native backends can then restore/move edge windows without a transient spill.
    NativeAction::Placement {
        window_id: window_id.clone(),
        rect,
        clip,
        minimized: clip.is_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{layout::Engine, model::*};

    fn fixture() -> (Engine, Placements) {
        let mut engine = Engine::new(BackendStatus {
            availability: BackendAvailability::Ready,
            capabilities: Capabilities {
                enumerate: true,
                placement: true,
                minimize: true,
                clipping: true,
                ..Capabilities::default()
            },
            ..BackendStatus::default()
        });
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

    fn scroll(engine: &mut Engine, delta: i32) -> Vec<NativeAction> {
        engine
            .dispatch(Command::Scroll {
                monitor_id: "m".into(),
                delta,
            })
            .unwrap()
            .actions
    }

    #[test]
    fn extreme_interpolation_is_monotone_and_converges_exactly() {
        let duration = Duration::from_millis(150);
        for (from, to) in [(i32::MIN, i32::MAX), (i32::MAX, i32::MIN), (-1, 0)] {
            let mut previous = from;
            for ms in 0..=150 {
                let x = interpolate_x(from, to, Duration::from_millis(ms), duration);
                assert!((from.min(to)..=from.max(to)).contains(&x));
                assert!(if from <= to {
                    x >= previous
                } else {
                    x <= previous
                });
                previous = x;
            }
            assert_eq!(previous, to);
            assert_eq!(interpolate_x(from, to, Duration::ZERO, Duration::ZERO), to);
        }
    }

    #[test]
    fn frames_clip_edges_retarget_applied_position_and_cancel_before_restore() {
        let (mut engine, mut applied) = fixture();
        let mut animation = ScrollAnimation::default();
        let now = Instant::now();
        let duration = Duration::from_millis(160);
        let target = scroll(&mut engine, 400);
        let initial = animation.start(engine.snapshot(), "m", &applied, target, duration, now);
        remember(&mut applied, &initial);
        assert_eq!(animation.deadline(), Some(now + FRAME_INTERVAL));
        assert!(animation.frame(now + Duration::from_millis(15)).is_empty());
        let middle = animation.frame(now + Duration::from_millis(80));
        for action in &middle {
            if let NativeAction::Placement {
                rect,
                clip,
                minimized,
                ..
            } = action
            {
                if !minimized {
                    let clip = clip.unwrap();
                    assert!(clip.x >= -1000 && i64::from(clip.x) + i64::from(clip.width) <= 0);
                    assert!(
                        clip.x >= rect.x
                            && i64::from(clip.x) + i64::from(clip.width)
                                <= i64::from(rect.x) + i64::from(rect.width)
                    );
                }
            }
        }
        remember(&mut applied, &middle);
        let from = applied["0"].0;
        assert_eq!(from.x, -1200);
        let target = scroll(&mut engine, -300);
        let initial = animation.start(
            engine.snapshot(),
            "m",
            &applied,
            target,
            duration,
            now + Duration::from_millis(80),
        );
        remember(&mut applied, &initial);
        assert_eq!(applied["0"].0, from);
        remember(
            &mut applied,
            &animation.frame(now + Duration::from_millis(160)),
        );
        assert_eq!(applied["0"].0.x, -1150);
        animation.cancel();
        let restore = engine.dispatch(Command::Disable).unwrap();
        assert!(
            restore
                .actions
                .iter()
                .all(|a| matches!(a, NativeAction::Restore { .. }))
        );
        assert!(animation.deadline().is_none());
        assert!(animation.frame(now + Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn reconcile_discards_frames_without_turning_observations_into_layout_targets() {
        let (mut engine, mut applied) = fixture();
        let mut animation = ScrollAnimation::default();
        let now = Instant::now();
        let target = scroll(&mut engine, 400);
        animation.start(
            engine.snapshot(),
            "m",
            &applied,
            target.clone(),
            Duration::from_millis(1000),
            now,
        );
        remember(
            &mut applied,
            &animation.frame(now + Duration::from_millis(500)),
        );
        let snapshot = engine.snapshot();
        let system = SystemSnapshot {
            monitors: snapshot
                .monitors
                .iter()
                .map(|m| m.monitor.clone())
                .collect(),
            windows: snapshot
                .windows
                .iter()
                .map(|w| {
                    let mut native = w.native.clone();
                    let (rect, _, minimized) = applied[&native.id];
                    native.rect = rect;
                    native.minimized = minimized;
                    native.minimized_by_manager = minimized;
                    native
                })
                .collect(),
            focused_window: snapshot.focused_window.clone(),
        };
        animation.cancel(); // Same ordering as Controller::refresh: before enumerate/reconcile.
        let reconciled = engine.reconcile(system).unwrap();
        assert_eq!(
            serde_json::to_value(reconciled.actions).unwrap(),
            serde_json::to_value(target).unwrap()
        );
        assert!(animation.frame(now + Duration::from_secs(2)).is_empty());
    }

    #[test]
    fn focus_waits_for_final_placement_and_fullscreen_does_not_animate() {
        let (mut engine, applied) = fixture();
        let mut target = scroll(&mut engine, 300);
        target.push(NativeAction::Focus {
            window_id: "1".into(),
        });
        let mut animation = ScrollAnimation::default();
        let now = Instant::now();
        let duration = Duration::from_millis(160);
        let initial = animation.start(
            engine.snapshot(),
            "m",
            &applied,
            target.clone(),
            duration,
            now,
        );
        assert!(
            initial
                .iter()
                .all(|a| matches!(a, NativeAction::Placement { .. }))
        );
        assert!(
            animation
                .frame(now + Duration::from_millis(80))
                .iter()
                .all(|a| matches!(a, NativeAction::Placement { .. }))
        );
        assert!(matches!(
            animation.frame(now + duration).last(),
            Some(NativeAction::Focus { .. })
        ));
        let mut snapshot = engine.snapshot().clone();
        snapshot.windows[0].fullscreen = true;
        let immediate = animation.start(&snapshot, "m", &applied, target.clone(), duration, now);
        assert_eq!(
            serde_json::to_value(immediate).unwrap(),
            serde_json::to_value(target).unwrap()
        );
        assert!(animation.deadline().is_none());
    }

    #[test]
    fn entrants_from_both_edges_redirect_and_cancel_before_or_after_becoming_visible() {
        for reverse in [false, true] {
            for elapsed_ms in [32, 128] {
                let (mut engine, mut applied) = fixture();
                if reverse {
                    remember(&mut applied, &scroll(&mut engine, 1000));
                }
                let entrant = if reverse { "0" } else { "3" };
                assert!(applied[entrant].2);
                let mut animation = ScrollAnimation::default();
                let now = Instant::now();
                let duration = Duration::from_millis(160);
                let target = scroll(&mut engine, if reverse { -700 } else { 700 });
                let initial =
                    animation.start(engine.snapshot(), "m", &applied, target, duration, now);
                remember(&mut applied, &initial);
                for ms in (16..=elapsed_ms).step_by(16) {
                    let frame = animation.frame(now + Duration::from_millis(ms));
                    for action in &frame {
                        if let NativeAction::Placement {
                            rect,
                            clip,
                            minimized,
                            ..
                        } = action
                        {
                            assert_eq!(
                                *clip,
                                clip_to_viewport(*rect, engine.snapshot().monitors[0].viewport)
                            );
                            assert_eq!(*minimized, clip.is_none());
                        }
                    }
                    remember(&mut applied, &frame);
                }
                let source = applied[entrant];
                assert_eq!(source.2, elapsed_ms == 32);
                let target = scroll(&mut engine, if reverse { -50 } else { 50 });
                let initial = animation.start(
                    engine.snapshot(),
                    "m",
                    &applied,
                    target,
                    duration,
                    now + Duration::from_millis(elapsed_ms),
                );
                remember(&mut applied, &initial);
                assert_eq!(applied[entrant], source);
                animation.cancel();
                engine.dispatch(Command::Disable).unwrap();
                assert!(animation.frame(now + Duration::from_secs(2)).is_empty());
            }
        }
    }

    #[test]
    fn untrusted_starts_settle_immediately_and_user_minimized_windows_are_untouched() {
        let (mut engine, applied) = fixture();
        let target = scroll(&mut engine, 300);
        let mut animation = ScrollAnimation::default();
        for case in 0..3 {
            let mut unreliable = applied.clone();
            match case {
                0 => {
                    unreliable.remove("0");
                }
                1 => unreliable.get_mut("0").unwrap().2 = true, // Hidden but inside viewport.
                _ => unreliable.get_mut("0").unwrap().0.y += 1,
            }
            let immediate = animation.start(
                engine.snapshot(),
                "m",
                &unreliable,
                target.clone(),
                Duration::from_millis(160),
                Instant::now(),
            );
            assert_eq!(
                serde_json::to_value(immediate).unwrap(),
                serde_json::to_value(&target).unwrap()
            );
            assert!(animation.deadline().is_none());
        }
        let snapshot = engine.snapshot();
        let mut system = SystemSnapshot {
            monitors: snapshot
                .monitors
                .iter()
                .map(|m| m.monitor.clone())
                .collect(),
            windows: snapshot.windows.iter().map(|w| w.native.clone()).collect(),
            focused_window: snapshot.focused_window.clone(),
        };
        system.windows[2].minimized = true;
        system.windows[2].minimized_by_manager = false;
        engine.reconcile(system).unwrap();
        let target = scroll(&mut engine, 300);
        let now = Instant::now();
        let initial = animation.start(
            engine.snapshot(),
            "m",
            &applied,
            target,
            Duration::from_millis(160),
            now,
        );
        for frame in [
            initial,
            animation.frame(now + Duration::from_millis(80)),
            animation.frame(now + Duration::from_millis(160)),
        ] {
            assert!(!frame.iter().any(
                |a| matches!(a, NativeAction::Placement { window_id, .. } if window_id == "2")
            ));
        }
    }

    #[test]
    fn final_actions_are_exact_hidden_windows_recover_and_no_clipping_is_immediate() {
        for (clipping, ms) in [(true, 160), (false, 160), (true, 0)] {
            let (mut engine, mut applied) = fixture();
            let target = scroll(&mut engine, 600);
            let mut snapshot = engine.snapshot().clone();
            snapshot.backend.capabilities.clipping = clipping;
            let mut animation = ScrollAnimation::default();
            let now = Instant::now();
            let initial = animation.start(
                &snapshot,
                "m",
                &applied,
                target.clone(),
                Duration::from_millis(ms),
                now,
            );
            if !clipping || ms == 0 {
                assert_eq!(
                    serde_json::to_value(initial).unwrap(),
                    serde_json::to_value(target).unwrap()
                );
                assert!(animation.deadline().is_none());
            } else {
                remember(&mut applied, &initial);
                assert!(applied["2"].2); // Entrant stays hidden until its first intersecting frame.
                remember(
                    &mut applied,
                    &animation.frame(now + Duration::from_millis(80)),
                );
                assert!(!applied["2"].2);
                let final_frame = animation.frame(now + Duration::from_millis(ms));
                assert_eq!(
                    serde_json::to_value(&final_frame).unwrap(),
                    serde_json::to_value(target).unwrap()
                );
                remember(&mut applied, &final_frame);
                assert!(applied["0"].2); // Leaving window ends minimized, never permanently hidden.
                assert!(animation.frame(now + Duration::from_secs(1)).is_empty());
            }
        }
    }
}
