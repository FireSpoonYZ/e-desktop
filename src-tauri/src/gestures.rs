//! Global wheel and touchpad gestures (lane: input-gestures). Pure logic: the Windows mouse
//! hook and raw-input window feed it, the controller turns its steps into layout commands.
//! Touchpad positions are fractions of the pad width on both axes.
use std::collections::VecDeque;

use crate::{
    config::WheelModifier,
    model::{Command, Direction, MonitorState, Snapshot},
};

/// One wheel notch (Win32 WHEEL_DELTA). High-resolution wheels and touchpads send less.
pub const WHEEL_NOTCH: i32 = 120;
/// niri binds `Mod+WheelScroll*` with `cooldown-ms=150`.
pub const WHEEL_COOLDOWN_MS: u32 = 150;
/// A partial high-resolution sum this old no longer counts toward the next notch.
const WHEEL_IDLE_MS: u32 = 500;

/// Exact modifiers: other held keys (Shift aside) leave the wheel to the application.
pub fn wheel_modifier_held(modifier: WheelModifier, control: bool, alt: bool, win: bool) -> bool {
    match modifier {
        WheelModifier::Super => win && !control && !alt,
        WheelModifier::Alt => alt && !control && !win,
        WheelModifier::ControlAlt => control && alt && !win,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelStep {
    /// Relative page on the monitor under the pointer: -1 above, 1 below.
    Page(i32),
    /// niri focus-column-left/right.
    Column(Direction),
}

/// Sums wheel deltas to whole notches, one step per notch at most once per cooldown.
#[derive(Debug, Default)]
pub struct WheelTracker {
    sum: i32,
    horizontal: bool,
    last_event: Option<u32>,
    last_step: Option<u32>,
}

impl WheelTracker {
    /// `delta` > 0 is wheel up, or right for `horizontal`. `time` is the wrapping input
    /// timestamp in milliseconds.
    pub fn feed(&mut self, delta: i32, horizontal: bool, time: u32) -> Option<WheelStep> {
        let stale = self
            .last_event
            .is_none_or(|last| time.wrapping_sub(last) > WHEEL_IDLE_MS);
        if stale || horizontal != self.horizontal || (self.sum > 0) != (delta > 0) {
            self.sum = 0;
        }
        self.horizontal = horizontal;
        self.last_event = Some(time);
        self.sum = self.sum.saturating_add(delta);
        if self.sum.abs() < WHEEL_NOTCH {
            return None;
        }
        let up = self.sum > 0;
        self.sum = 0;
        if self
            .last_step
            .is_some_and(|last| time.wrapping_sub(last) < WHEEL_COOLDOWN_MS)
        {
            return None;
        }
        self.last_step = Some(time);
        Some(match (horizontal, up) {
            (false, true) => WheelStep::Page(-1),
            (false, false) => WheelStep::Page(1),
            (true, true) => WheelStep::Column(Direction::Right),
            (true, false) => WheelStep::Column(Direction::Left),
        })
    }
}

pub fn monitor_at(snapshot: &Snapshot, x: i32, y: i32) -> Option<&MonitorState> {
    let (x, y) = (i64::from(x), i64::from(y));
    snapshot.monitors.iter().find(|m| {
        let b = m.monitor.bounds;
        x >= i64::from(b.x)
            && y >= i64::from(b.y)
            && x < i64::from(b.x) + i64::from(b.width)
            && y < i64::from(b.y) + i64::from(b.height)
    })
}

/// The neighbouring page of the monitor's active page; none past either end (no wrapping).
pub fn relative_page(monitor: &MonitorState, delta: i32) -> Option<Command> {
    let current = monitor
        .pages
        .iter()
        .position(|p| p.id == monitor.active_page)?;
    let target = current.checked_add_signed(delta as isize)?;
    Some(Command::SwitchPage {
        monitor_id: monitor.monitor.id.clone(),
        page_id: monitor.pages.get(target)?.id.clone(),
    })
}

/// Commands for a wheel step over the monitor under the pointer. A column step there first
/// activates that monitor's page when focus is on another monitor.
pub fn wheel_commands(snapshot: &Snapshot, x: i32, y: i32, step: WheelStep) -> Vec<Command> {
    let Some(monitor) = monitor_at(snapshot, x, y) else {
        return vec![];
    };
    let direction = match step {
        WheelStep::Page(delta) => return relative_page(monitor, delta).into_iter().collect(),
        WheelStep::Column(direction) => direction,
    };
    let id = &monitor.monitor.id;
    let page = monitor.pages.iter().find(|p| p.id == monitor.active_page);
    if page.is_none_or(|p| p.columns.is_empty() && p.floating_windows.is_empty()) {
        return vec![];
    }
    let mut commands = vec![];
    if snapshot.active_monitor.as_ref() != Some(id) || snapshot.focused_window.is_none() {
        commands.push(Command::SwitchPage {
            monitor_id: id.clone(),
            page_id: monitor.active_page.clone(),
        });
    }
    commands.push(Command::FocusDirection { direction });
    commands
}

/// A touchpad contact in pad widths from the pad's top-left corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Contact {
    pub id: u32,
    pub x: f64,
    pub y: f64,
}

/// One finger collection of a HID report.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slot {
    pub tip: bool,
    pub contact: Contact,
}

/// Groups precision-touchpad reports into frames. In hybrid mode a frame spans several
/// reports: the first carries the frame's contact count, the rest carry 0.
#[derive(Debug, Default)]
pub struct Frames {
    expected: usize,
    received: usize,
    contacts: Vec<Contact>,
}

impl Frames {
    /// The touching contacts once the report completes a frame.
    pub fn report(&mut self, count: u32, slots: &[Slot]) -> Option<Vec<Contact>> {
        if count > 0 {
            self.expected = count as usize;
            self.received = 0;
            self.contacts.clear();
        }
        if self.received >= self.expected {
            return None;
        }
        for slot in slots.iter().take(self.expected - self.received) {
            self.received += 1;
            if slot.tip {
                self.contacts.push(slot.contact);
            }
        }
        (self.received == self.expected).then(|| {
            self.expected = 0;
            self.received = 0;
            std::mem::take(&mut self.contacts)
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Swipe {
    /// Horizontal finger motion since the last step, in pad widths (positive: right).
    Drag(f64),
    /// The horizontal swipe ended; finger velocity in pad widths per second.
    Release(f64),
    /// The swipe locked onto the vertical axis: it begins here (the first `Drag` begins a
    /// horizontal one).
    PageBegin,
    /// The vertical swipe ended: 1 the page below (fingers moved up), -1 above, 0 short of the
    /// threshold.
    Page(i32),
    /// The recognizer restarted mid-swipe (touchpad settings changed, management paused); no
    /// release follows for the swipe in progress.
    Cancel,
}

/// Motion that decides a swipe's axis.
const LOCK_DISTANCE: f64 = 0.04;
/// Vertical travel that switches a page on release.
const PAGE_DISTANCE: f64 = 0.12;
/// Samples that make up the release velocity.
const VELOCITY_WINDOW_MS: u64 = 100;
/// Pad-width swipe in viewport widths: a comfortable half-pad swipe moves about one column.
pub const SWIPE_VIEWPORTS_PER_PAD: f64 = 1.5;

/// Distance a released swipe keeps going with niri's touchpad deceleration (0.997 per ms).
pub fn release_distance(velocity: f64) -> f64 {
    -velocity / (1000.0 * 0.997f64.ln())
}

/// Viewport pixels for finger motion: content follows the fingers.
pub fn swipe_pixels(fingers: f64, viewport_width: u32) -> f64 {
    -fingers * SWIPE_VIEWPORTS_PER_PAD * f64::from(viewport_width)
}

#[derive(Debug)]
enum Phase {
    Idle,
    /// Fingers down, axis not decided yet.
    Pending {
        start: (f64, f64),
    },
    Horizontal {
        last: f64,
        samples: VecDeque<(u64, f64)>,
    },
    Vertical {
        start: f64,
        last: f64,
    },
    /// Too many fingers touched; wait until the pad is empty.
    Ignored,
}

/// Swipe recognizer for exactly `fingers` contacts.
#[derive(Debug)]
pub struct Touchpad {
    fingers: usize,
    phase: Phase,
}

impl Touchpad {
    pub fn new(fingers: u8) -> Self {
        Self {
            fingers: fingers.into(),
            phase: Phase::Idle,
        }
    }

    /// The touching contacts of one frame at `time` milliseconds.
    pub fn frame(&mut self, contacts: &[Contact], time: u64) -> Option<Swipe> {
        let count = contacts.len();
        if count != self.fingers {
            let ended = self.end(time);
            let ignored = matches!(self.phase, Phase::Ignored);
            self.phase = if count > self.fingers || (ignored && count > 0) {
                Phase::Ignored
            } else {
                Phase::Idle
            };
            return ended;
        }
        let n = count as f64;
        let x = contacts.iter().map(|c| c.x).sum::<f64>() / n;
        let y = contacts.iter().map(|c| c.y).sum::<f64>() / n;
        match &mut self.phase {
            Phase::Ignored => None,
            Phase::Idle => {
                self.phase = Phase::Pending { start: (x, y) };
                None
            }
            Phase::Pending { start } => {
                let (dx, dy) = (x - start.0, y - start.1);
                if dx.abs().max(dy.abs()) < LOCK_DISTANCE {
                    None
                } else if dx.abs() >= dy.abs() {
                    self.phase = Phase::Horizontal {
                        last: x,
                        samples: VecDeque::from([(time, x)]),
                    };
                    Some(Swipe::Drag(dx))
                } else {
                    self.phase = Phase::Vertical {
                        start: start.1,
                        last: y,
                    };
                    Some(Swipe::PageBegin)
                }
            }
            Phase::Horizontal { last, samples } => {
                let dx = x - *last;
                *last = x;
                samples.push_back((time, x));
                while samples
                    .front()
                    .is_some_and(|(t, _)| time.saturating_sub(*t) > VELOCITY_WINDOW_MS)
                {
                    samples.pop_front();
                }
                (dx != 0.0).then_some(Swipe::Drag(dx))
            }
            Phase::Vertical { last, .. } => {
                *last = y;
                None
            }
        }
    }

    fn end(&self, time: u64) -> Option<Swipe> {
        match &self.phase {
            Phase::Horizontal { samples, .. } => {
                let recent: Vec<_> = samples
                    .iter()
                    .filter(|(t, _)| time.saturating_sub(*t) <= VELOCITY_WINDOW_MS)
                    .collect();
                let velocity = match (recent.first(), recent.last()) {
                    (Some((t0, x0)), Some((t1, x1))) if t1 > t0 => {
                        (x1 - x0) * 1000.0 / (t1 - t0) as f64
                    }
                    _ => 0.0,
                };
                Some(Swipe::Release(velocity))
            }
            Phase::Vertical { start, last } => {
                Some(Swipe::Page(if (last - start).abs() < PAGE_DISTANCE {
                    0
                } else if last < start {
                    1
                } else {
                    -1
                }))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Config,
        model::{Column, Monitor, Page, Rect},
    };

    #[test]
    fn wheel_modifiers_are_exact_and_configurable() {
        use WheelModifier::*;
        assert!(wheel_modifier_held(Super, false, false, true));
        assert!(
            !wheel_modifier_held(Super, true, false, true),
            "Ctrl+Win is not Win"
        );
        assert!(wheel_modifier_held(Alt, false, true, false));
        assert!(!wheel_modifier_held(Alt, true, true, false));
        assert!(wheel_modifier_held(ControlAlt, true, true, false));
        assert!(!wheel_modifier_held(ControlAlt, false, true, false));
        assert!(!wheel_modifier_held(ControlAlt, true, true, true));

        let defaults = Config::parse(b"{}").unwrap();
        assert_eq!(defaults.wheel_modifier, Some(Super));
        assert_eq!(defaults.touchpad_gesture_fingers, None);
        let config =
            Config::parse(br#"{"wheelModifier":"controlAlt","touchpadGestureFingers":4}"#).unwrap();
        assert_eq!(
            (config.wheel_modifier, config.touchpad_gesture_fingers),
            (Some(ControlAlt), Some(4))
        );
        assert_eq!(
            Config::parse(br#"{"wheelModifier":null}"#)
                .unwrap()
                .wheel_modifier,
            None
        );
        for text in [
            r#"{"wheelModifier":"shift"}"#,
            r#"{"touchpadGestureFingers":2}"#,
            r#"{"touchpadGestureFingers":5}"#,
            r#"{"touchpadGestureFingers":"3"}"#,
        ] {
            assert!(Config::parse(text.as_bytes()).is_err(), "accepted: {text}");
        }
    }

    #[test]
    fn wheel_notches_step_with_a_cooldown_and_high_resolution_deltas_accumulate() {
        let mut wheel = WheelTracker::default();
        assert_eq!(wheel.feed(120, false, 1000), Some(WheelStep::Page(-1)));
        assert_eq!(wheel.feed(-120, false, 1100), None, "within the cooldown");
        assert_eq!(wheel.feed(-120, false, 1150), Some(WheelStep::Page(1)));
        // High resolution: 40 + 40 + 40 makes one notch; reversing resets the sum.
        assert_eq!(wheel.feed(40, true, 2000), None);
        assert_eq!(wheel.feed(40, true, 2010), None);
        assert_eq!(
            wheel.feed(40, true, 2020),
            Some(WheelStep::Column(Direction::Right))
        );
        assert_eq!(wheel.feed(-100, true, 2200), None);
        assert_eq!(wheel.feed(60, true, 2210), None);
        assert_eq!(wheel.feed(50, true, 2220), None);
        assert_eq!(
            wheel.feed(-120, true, 2400),
            Some(WheelStep::Column(Direction::Left))
        );
        // A stale partial sum does not complete a later notch.
        assert_eq!(wheel.feed(100, false, 3000), None);
        assert_eq!(wheel.feed(100, false, 4000), None);
        // Input timestamps wrap.
        let mut wheel = WheelTracker::default();
        assert!(wheel.feed(120, false, u32::MAX - 10).is_some());
        assert!(wheel.feed(120, false, 20).is_none());
        assert!(wheel.feed(120, false, 200).is_some());
    }

    fn monitor(id: &str, x: i32, pages: usize, active: usize) -> MonitorState {
        let bounds = Rect {
            x,
            y: 0,
            width: 1000,
            height: 800,
        };
        MonitorState {
            monitor: Monitor {
                id: id.into(),
                name: id.into(),
                bounds,
                work_area: bounds,
                scale_factor: 1.0,
                primary: x == 0,
            },
            pages: (0..pages)
                .map(|n| Page {
                    id: format!("{id}{n}"),
                    name: String::new(),
                    columns: vec![Column {
                        id: format!("c{id}{n}"),
                        width: 500,
                        windows: vec![format!("w{id}{n}")],
                        display: Default::default(),
                        active_tab: None,
                    }],
                    floating_windows: vec![],
                    viewport_x: 0,
                })
                .collect(),
            active_page: format!("{id}{active}"),
            viewport: bounds,
        }
    }

    #[test]
    fn wheel_steps_act_on_the_monitor_under_the_pointer() {
        let mut snapshot = Snapshot {
            monitors: vec![monitor("a", 0, 3, 1), monitor("b", 1000, 2, 0)],
            active_monitor: Some("a".into()),
            focused_window: Some("wa1".into()),
            ..Snapshot::default()
        };
        let switch = |m: &str, p: &str| Command::SwitchPage {
            monitor_id: m.into(),
            page_id: p.into(),
        };
        assert_eq!(
            wheel_commands(&snapshot, 10, 10, WheelStep::Page(-1)),
            [switch("a", "a0")]
        );
        assert_eq!(
            wheel_commands(&snapshot, 1500, 10, WheelStep::Page(1)),
            [switch("b", "b1")]
        );
        assert!(wheel_commands(&snapshot, 1500, 10, WheelStep::Page(-1)).is_empty());
        assert!(wheel_commands(&snapshot, 5000, 10, WheelStep::Page(1)).is_empty());
        let right = Command::FocusDirection {
            direction: Direction::Right,
        };
        assert_eq!(
            wheel_commands(&snapshot, 10, 10, WheelStep::Column(Direction::Right)),
            [right.clone()]
        );
        assert_eq!(
            wheel_commands(&snapshot, 1500, 10, WheelStep::Column(Direction::Right)),
            [switch("b", "b0"), right.clone()]
        );
        snapshot.monitors[1].pages[0].columns.clear();
        assert!(
            wheel_commands(&snapshot, 1500, 10, WheelStep::Column(Direction::Right)).is_empty()
        );
    }

    fn slots(contacts: &[(u32, f64, f64)]) -> Vec<Slot> {
        contacts
            .iter()
            .map(|&(id, x, y)| Slot {
                tip: true,
                contact: Contact { id, x, y },
            })
            .collect()
    }

    #[test]
    fn hybrid_reports_assemble_into_frames() {
        let mut frames = Frames::default();
        assert_eq!(
            frames.report(0, &slots(&[(1, 0.1, 0.1)])),
            None,
            "no frame open"
        );
        // Three contacts, two per report: the second report's extra slot is padding.
        assert_eq!(
            frames.report(3, &slots(&[(1, 0.1, 0.1), (2, 0.2, 0.1)])),
            None
        );
        let frame = frames
            .report(0, &slots(&[(3, 0.3, 0.1), (9, 0.9, 0.9)]))
            .unwrap();
        assert_eq!(frame.iter().map(|c| c.id).collect::<Vec<_>>(), [1, 2, 3]);
        // Parallel mode, with a lifted contact reported once with its tip up.
        let mut lifted = slots(&[(1, 0.1, 0.1), (2, 0.2, 0.1)]);
        lifted[1].tip = false;
        assert_eq!(frames.report(2, &lifted).unwrap().len(), 1);
        assert_eq!(frames.report(0, &lifted), None);
    }

    fn fingers(n: usize, x: f64, y: f64) -> Vec<Contact> {
        (0..n)
            .map(|i| Contact {
                id: i as u32,
                x: x + i as f64 * 0.05,
                y,
            })
            .collect()
    }

    #[test]
    fn horizontal_swipe_drags_then_releases_with_velocity() {
        let mut pad = Touchpad::new(3);
        // Fingers land one by one; only the third starts the swipe.
        assert_eq!(pad.frame(&fingers(1, 0.2, 0.5), 0), None);
        assert_eq!(pad.frame(&fingers(2, 0.2, 0.5), 8), None);
        assert_eq!(pad.frame(&fingers(3, 0.2, 0.5), 16), None);
        assert_eq!(
            pad.frame(&fingers(3, 0.22, 0.5), 24),
            None,
            "below the lock"
        );
        let Some(Swipe::Drag(dx)) = pad.frame(&fingers(3, 0.25, 0.51), 32) else {
            panic!("horizontal lock");
        };
        assert!((dx - 0.05).abs() < 1e-9);
        let mut total = dx;
        for (i, t) in (40..=120).step_by(8).enumerate() {
            let x = 0.25 + 0.01 * (i + 1) as f64;
            if let Some(Swipe::Drag(dx)) = pad.frame(&fingers(3, x, 0.5), t) {
                total += dx;
            }
        }
        assert!(
            (total - 0.16).abs() < 1e-9,
            "every movement is reported once"
        );
        // One finger lifts: release with about 0.01 pad widths per 8 ms.
        let Some(Swipe::Release(v)) = pad.frame(&fingers(2, 0.4, 0.5), 128) else {
            panic!("release");
        };
        assert!((v - 1.25).abs() < 1e-6, "velocity {v}");
        assert_eq!(pad.frame(&fingers(1, 0.4, 0.5), 136), None);
        assert_eq!(pad.frame(&[], 144), None);
        // A swipe that stopped before lifting releases without velocity.
        pad.frame(&fingers(3, 0.2, 0.5), 1000);
        pad.frame(&fingers(3, 0.3, 0.5), 1008);
        pad.frame(&fingers(3, 0.3, 0.5), 1200);
        assert_eq!(pad.frame(&[], 1400), Some(Swipe::Release(0.0)));
        assert!(release_distance(1.0) > 0.3 && release_distance(1.0) < 0.34);
        assert_eq!(swipe_pixels(0.5, 1000), -750.0);
    }

    #[test]
    fn vertical_swipe_switches_pages_on_release_past_the_threshold() {
        let mut pad = Touchpad::new(4);
        pad.frame(&fingers(4, 0.3, 0.6), 0);
        assert_eq!(
            pad.frame(&fingers(4, 0.31, 0.5), 8),
            Some(Swipe::PageBegin),
            "vertical lock begins the swipe"
        );
        assert_eq!(
            pad.frame(&fingers(4, 0.31, 0.4), 16),
            None,
            "no drag on the vertical axis"
        );
        assert_eq!(pad.frame(&fingers(3, 0.31, 0.4), 24), Some(Swipe::Page(1)));
        pad.frame(&[], 32);
        pad.frame(&fingers(4, 0.3, 0.3), 100);
        assert_eq!(
            pad.frame(&fingers(4, 0.3, 0.45), 108),
            Some(Swipe::PageBegin)
        );
        assert_eq!(pad.frame(&[], 116), Some(Swipe::Page(-1)));
        // Short of the threshold: the swipe still ends, without a page.
        pad.frame(&fingers(4, 0.3, 0.3), 200);
        assert_eq!(
            pad.frame(&fingers(4, 0.3, 0.36), 208),
            Some(Swipe::PageBegin)
        );
        assert_eq!(pad.frame(&[], 216), Some(Swipe::Page(0)));
        // Below the axis lock nothing began, so nothing ends.
        pad.frame(&fingers(4, 0.3, 0.3), 300);
        pad.frame(&fingers(4, 0.3, 0.32), 308);
        assert_eq!(pad.frame(&[], 316), None);
    }

    #[test]
    fn other_finger_counts_are_ignored_until_the_pad_is_clear() {
        let mut pad = Touchpad::new(3);
        // Four fingers: no three-finger swipe while they lift one by one.
        pad.frame(&fingers(3, 0.2, 0.5), 0);
        assert_eq!(pad.frame(&fingers(4, 0.2, 0.5), 8), None);
        assert_eq!(pad.frame(&fingers(3, 0.2, 0.5), 16), None);
        assert_eq!(pad.frame(&fingers(3, 0.5, 0.5), 24), None);
        assert_eq!(pad.frame(&fingers(2, 0.5, 0.5), 32), None);
        assert_eq!(pad.frame(&[], 40), None);
        // A swipe interrupted by an extra finger still releases.
        pad.frame(&fingers(3, 0.2, 0.5), 100);
        assert!(matches!(
            pad.frame(&fingers(3, 0.3, 0.5), 108),
            Some(Swipe::Drag(_))
        ));
        assert!(matches!(
            pad.frame(&fingers(4, 0.3, 0.5), 116),
            Some(Swipe::Release(_))
        ));
        assert_eq!(pad.frame(&fingers(3, 0.5, 0.5), 124), None);
        // Two-finger scrolling never starts a three-finger swipe.
        pad.frame(&[], 200);
        assert_eq!(pad.frame(&fingers(2, 0.2, 0.5), 208), None);
        assert_eq!(pad.frame(&fingers(2, 0.6, 0.5), 216), None);
        assert_eq!(pad.frame(&[], 224), None);
    }
}
