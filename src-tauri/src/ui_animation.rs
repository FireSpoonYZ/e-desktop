//! lane: ui-animation — per-type animation lengths and the niri-style hotkey overlay settings.
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::model::{Command, Snapshot};

/// The kinds of animation niri configures separately. Each transition uses one duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationKind {
    /// The active page of a monitor changed (old page slides out, new one in).
    WorkspaceSwitch,
    /// Horizontal view movement: scroll, slide, center and focus changes.
    ViewMovement,
    /// Windows change columns, stacks, pages, floating or layout fullscreen.
    WindowMovement,
    /// Column widths and window heights, including dragged boundaries.
    WindowResize,
    /// Windows appearing or disappearing (refresh reflow, enable, close).
    WindowOpenClose,
    /// Overview thumbnails zooming between the desktop and the overview (frontend).
    OverviewOpenClose,
}

/// `animations`: milliseconds per kind; missing or null falls back to `animationDurationMs`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Animations {
    pub workspace_switch: Option<u32>,
    pub view_movement: Option<u32>,
    pub window_movement: Option<u32>,
    pub window_resize: Option<u32>,
    pub window_open_close: Option<u32>,
    pub overview_open_close: Option<u32>,
}

impl Animations {
    fn get(&self, kind: AnimationKind) -> Option<u32> {
        match kind {
            AnimationKind::WorkspaceSwitch => self.workspace_switch,
            AnimationKind::ViewMovement => self.view_movement,
            AnimationKind::WindowMovement => self.window_movement,
            AnimationKind::WindowResize => self.window_resize,
            AnimationKind::WindowOpenClose => self.window_open_close,
            AnimationKind::OverviewOpenClose => self.overview_open_close,
        }
    }

    /// `base` is `animationDurationMs`; an unclassified transition (`None`) uses it as well.
    pub fn duration(&self, kind: Option<AnimationKind>, base: Duration) -> Duration {
        kind.and_then(|kind| self.get(kind))
            .map_or(base, |ms| Duration::from_millis(u64::from(ms)))
    }

    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("workspaceSwitch", self.workspace_switch),
            ("viewMovement", self.view_movement),
            ("windowMovement", self.window_movement),
            ("windowResize", self.window_resize),
            ("windowOpenClose", self.window_open_close),
            ("overviewOpenClose", self.overview_open_close),
        ] {
            if value.is_some_and(|ms| ms > 1000) {
                return Err(format!(
                    "animations.{name} 须在 0 到 1000 之间，或为 null。"
                ));
            }
        }
        Ok(())
    }
}

/// The animation kind a command asks for; `None` uses `animationDurationMs`. The match is
/// exhaustive so that every new command gets classified.
pub fn command_kind(command: &Command) -> Option<AnimationKind> {
    use AnimationKind::*;
    Some(match command {
        Command::Refresh | Command::Enable | Command::CloseWindow { .. } => WindowOpenClose,
        Command::FocusWindow { .. }
        | Command::FocusDirection { .. }
        | Command::Scroll { .. }
        | Command::SlideColumn { .. }
        | Command::CenterFocused
        | Command::FocusColumnFirst
        | Command::FocusColumnLast
        | Command::FocusWindowOrPage { .. }
        | Command::FocusColumnOrMonitor { .. }
        | Command::FocusMonitor { .. }
        | Command::FocusWindowPrevious
        | Command::DragViewport { .. }
        | Command::SnapViewport { .. } => ViewMovement,
        Command::SwitchPage { .. }
        | Command::AddPage { .. }
        | Command::FocusPagePrevious
        | Command::MovePageToMonitor { .. } => WorkspaceSwitch,
        Command::MoveWindow { .. }
        | Command::MoveWindowToPage { .. }
        | Command::DropWindow { .. }
        | Command::ToggleFloating
        | Command::ToggleFullscreen
        | Command::SetFloatingRect { .. }
        | Command::ConsumeOrExpelWindow { .. }
        | Command::ConsumeWindowIntoColumn
        | Command::ExpelWindowFromColumn
        | Command::MoveColumn { .. }
        | Command::MoveColumnToFirst
        | Command::MoveColumnToLast
        | Command::SwapWindow { .. }
        | Command::MoveColumnToMonitor { .. }
        | Command::MoveWindowToMonitor { .. }
        | Command::ToggleColumnTabbedDisplay => WindowMovement,
        Command::CycleWidth
        | Command::CycleWidthBack
        | Command::CycleWindowHeight
        | Command::MaximizeColumn
        | Command::SetColumnWidth { .. }
        | Command::SetWindowColumnWidth { .. }
        | Command::AdjustColumnWidth { .. }
        | Command::AdjustWindowHeight { .. }
        | Command::ResetWindowHeights
        | Command::DragEdge { .. }
        | Command::DragRow { .. } => WindowResize,
        // No layout motion of their own.
        Command::Disable
        | Command::SetPageName { .. }
        | Command::UnsetPageName
        | Command::Screenshot
        | Command::ScreenshotScreen
        | Command::ScreenshotWindow => return None,
    })
}

/// A transition that changes any monitor's active page is a workspace switch, whatever caused
/// it (following a moved window, focusing a window on another page, a click during refresh).
pub fn transition_kind(
    prev: &Snapshot,
    next: &Snapshot,
    kind: Option<AnimationKind>,
) -> Option<AnimationKind> {
    let switched = next.monitors.iter().any(|monitor| {
        prev.monitors.iter().any(|old| {
            old.monitor.id == monitor.monitor.id && old.active_page != monitor.active_page
        })
    });
    if switched {
        Some(AnimationKind::WorkspaceSwitch)
    } else {
        kind
    }
}

/// `hotkeyOverlay`: niri shows the overlay once at startup; here, when tiling is first enabled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct HotkeyOverlay {
    pub skip_at_startup: bool,
}

/// Whether to show the overlay now. `pending` is true until tiling is first enabled.
pub fn hotkeys_at_first_enable(pending: &mut bool, enabled: bool, config: HotkeyOverlay) -> bool {
    if !*pending || !enabled {
        return false;
    }
    *pending = false;
    !config.skip_at_startup
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        animation::{Animation, Placements},
        config::{Config, ShortcutAction},
        layout::Engine,
        model::*,
    };
    use std::time::Instant;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn animations_parse_with_null_fallback_and_bounds() {
        let config = Config::parse(b"{}").unwrap();
        assert_eq!(config.animations, Animations::default());
        assert_eq!(config.hotkey_overlay, HotkeyOverlay::default());
        let config = Config::parse(
            br#"{"animationDurationMs":200,"animations":{"workspaceSwitch":300,"viewMovement":null,"windowResize":0,"overviewOpenClose":1000}}"#,
        )
        .unwrap();
        let base = MS(u64::from(config.animation_duration_ms));
        let animations = config.animations;
        assert_eq!(
            animations.duration(Some(AnimationKind::WorkspaceSwitch), base),
            MS(300)
        );
        assert_eq!(
            animations.duration(Some(AnimationKind::ViewMovement), base),
            MS(200)
        );
        assert_eq!(
            animations.duration(Some(AnimationKind::WindowMovement), base),
            MS(200)
        );
        assert_eq!(
            animations.duration(Some(AnimationKind::WindowResize), base),
            MS(0)
        );
        assert_eq!(
            animations.duration(Some(AnimationKind::OverviewOpenClose), base),
            MS(1000)
        );
        assert_eq!(animations.duration(None, base), MS(200));
        // A disabled base still lets one kind animate.
        let config =
            Config::parse(br#"{"animationDurationMs":0,"animations":{"windowOpenClose":120}}"#)
                .unwrap();
        assert_eq!(
            config
                .animations
                .duration(Some(AnimationKind::WindowOpenClose), MS(0)),
            MS(120)
        );
        assert_eq!(
            config
                .animations
                .duration(Some(AnimationKind::ViewMovement), MS(0)),
            MS(0)
        );
        for text in [
            r#"{"animations":{"viewMovement":1001}}"#,
            r#"{"animations":{"viewMovement":-1}}"#,
            r#"{"animations":{"viewMovement":"160"}}"#,
            r#"{"animations":{"windowClose":100}}"#,
            r#"{"animations":null}"#,
            r#"{"hotkeyOverlay":{"skipAtStartup":"yes"}}"#,
            r#"{"hotkeyOverlay":{"skip":true}}"#,
        ] {
            assert!(Config::parse(text.as_bytes()).is_err(), "accepted: {text}");
        }
        let config = Config::parse(br#"{"hotkeyOverlay":{"skipAtStartup":true}}"#).unwrap();
        assert!(config.hotkey_overlay.skip_at_startup);
        let defaults = Config::default();
        assert_eq!(
            Config::parse(&serde_json::to_vec(&defaults).unwrap()).unwrap(),
            defaults
        );
    }

    #[test]
    fn commands_pick_their_animation_kind() {
        for (command, kind) in [
            (
                Command::Scroll {
                    monitor_id: "m".into(),
                    delta: 10,
                },
                AnimationKind::ViewMovement,
            ),
            (
                Command::FocusDirection {
                    direction: Direction::Left,
                },
                AnimationKind::ViewMovement,
            ),
            (
                Command::MoveWindow {
                    direction: Direction::Left,
                },
                AnimationKind::WindowMovement,
            ),
            (Command::ToggleFullscreen, AnimationKind::WindowMovement),
            (
                Command::DragEdge {
                    monitor_id: "m".into(),
                    edge: 1,
                    delta: 5,
                },
                AnimationKind::WindowResize,
            ),
            (
                Command::DragRow {
                    monitor_id: "m".into(),
                    column: 0,
                    edge: 1,
                    delta: 5,
                },
                AnimationKind::WindowResize,
            ),
            (Command::CycleWidth, AnimationKind::WindowResize),
            (
                Command::SwitchPage {
                    monitor_id: "m".into(),
                    page_id: "p".into(),
                },
                AnimationKind::WorkspaceSwitch,
            ),
            (Command::Refresh, AnimationKind::WindowOpenClose),
        ] {
            assert_eq!(command_kind(&command), Some(kind), "{command:?}");
        }
        assert_eq!(command_kind(&Command::Disable), None);
    }

    #[test]
    fn newer_layout_commands_use_their_documented_kind_not_the_base_duration() {
        let (left, right) = (Direction::Left, Direction::Right);
        let m = || "m".to_string();
        let cases = [
            (
                Command::MoveColumn { direction: left },
                AnimationKind::WindowMovement,
            ),
            (Command::MoveColumnToFirst, AnimationKind::WindowMovement),
            (Command::MoveColumnToLast, AnimationKind::WindowMovement),
            (
                Command::ConsumeOrExpelWindow { direction: right },
                AnimationKind::WindowMovement,
            ),
            (
                Command::ConsumeWindowIntoColumn,
                AnimationKind::WindowMovement,
            ),
            (
                Command::ExpelWindowFromColumn,
                AnimationKind::WindowMovement,
            ),
            (
                Command::SwapWindow { direction: left },
                AnimationKind::WindowMovement,
            ),
            (
                Command::MoveColumnToMonitor { direction: right },
                AnimationKind::WindowMovement,
            ),
            (
                Command::MoveWindowToMonitor { direction: right },
                AnimationKind::WindowMovement,
            ),
            (
                Command::ToggleColumnTabbedDisplay,
                AnimationKind::WindowMovement,
            ),
            (Command::MaximizeColumn, AnimationKind::WindowResize),
            (Command::CycleWindowHeight, AnimationKind::WindowResize),
            (Command::CycleWidthBack, AnimationKind::WindowResize),
            (Command::FocusColumnFirst, AnimationKind::ViewMovement),
            (Command::FocusColumnLast, AnimationKind::ViewMovement),
            (
                Command::FocusWindowOrPage {
                    direction: Direction::Down,
                },
                AnimationKind::ViewMovement,
            ),
            (
                Command::FocusColumnOrMonitor { direction: left },
                AnimationKind::ViewMovement,
            ),
            (
                Command::FocusMonitor { direction: right },
                AnimationKind::ViewMovement,
            ),
            (Command::FocusWindowPrevious, AnimationKind::ViewMovement),
            (
                Command::DragViewport {
                    monitor_id: m(),
                    delta: 5,
                },
                AnimationKind::ViewMovement,
            ),
            (
                Command::SnapViewport {
                    monitor_id: m(),
                    delta: 0,
                },
                AnimationKind::ViewMovement,
            ),
            (Command::FocusPagePrevious, AnimationKind::WorkspaceSwitch),
            (
                Command::MovePageToMonitor { direction: left },
                AnimationKind::WorkspaceSwitch,
            ),
        ];
        // Base 0 with one override per kind: an unclassified command would not animate.
        let animations = Animations {
            workspace_switch: Some(250),
            view_movement: Some(150),
            window_movement: Some(300),
            window_resize: Some(200),
            ..Animations::default()
        };
        for (command, kind) in cases {
            let classified = command_kind(&command);
            assert_eq!(classified, Some(kind), "{command:?}");
            assert_ne!(animations.duration(classified, MS(0)), MS(0), "{command:?}");
        }
        for command in [
            Command::SetPageName { name: "web".into() },
            Command::UnsetPageName,
            Command::Screenshot,
            Command::ScreenshotScreen,
            Command::ScreenshotWindow,
        ] {
            assert_eq!(command_kind(&command), None, "{command:?}");
        }
    }

    fn engine() -> Engine {
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
        let area = Rect {
            x: 0,
            y: 0,
            width: 1200,
            height: 900,
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
                windows: ["a", "b"]
                    .into_iter()
                    .map(|id| NativeWindow {
                        id: id.into(),
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
                focused_window: Some("a".into()),
            })
            .unwrap();
        engine
    }

    fn placements(actions: &[NativeAction]) -> Placements {
        actions
            .iter()
            .filter_map(|action| match action {
                NativeAction::Placement {
                    window_id,
                    rect,
                    clip,
                    minimized,
                } => Some((window_id.clone(), (*rect, *clip, *minimized))),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_page_change_is_a_workspace_switch_and_a_released_boundary_animates() {
        let mut engine = engine();
        let mut applied = Placements::new();
        let mut run = |engine: &mut Engine, command| {
            let transition = engine.dispatch(command).unwrap();
            applied.extend(placements(&transition.actions));
            transition
        };
        run(&mut engine, Command::Enable);
        for direction in [Direction::Left, Direction::Right] {
            run(&mut engine, Command::SetColumnWidth { width: 600 });
            engine.dispatch(Command::FocusDirection { direction }).ok();
        }
        let prev = engine.snapshot().clone();
        let added = run(
            &mut engine,
            Command::AddPage {
                monitor_id: "m".into(),
            },
        );
        assert_eq!(
            transition_kind(&prev, &added.snapshot, Some(AnimationKind::ViewMovement)),
            Some(AnimationKind::WorkspaceSwitch)
        );
        let page = prev.monitors[0].active_page.clone();
        run(
            &mut engine,
            Command::SwitchPage {
                monitor_id: "m".into(),
                page_id: page,
            },
        );
        let prev = engine.snapshot().clone();
        let drag = Command::DragEdge {
            monitor_id: "m".into(),
            edge: 1,
            delta: 120,
        };
        let kind = command_kind(&drag);
        let released = engine.dispatch(drag).unwrap();
        let kind = transition_kind(&prev, &released.snapshot, kind);
        assert_eq!(kind, Some(AnimationKind::WindowResize));
        let animations = Animations {
            window_resize: Some(250),
            ..Animations::default()
        };
        let duration = animations.duration(kind, MS(160));
        assert_eq!(duration, MS(250));
        let mut animation = Animation::default();
        let now = Instant::now();
        animation.start(
            &prev,
            &released.snapshot,
            &applied,
            released.actions,
            duration,
            now,
        );
        assert!(
            animation.deadline().is_some(),
            "boundary release must animate"
        );
        assert_eq!(animation.duration(), Some(MS(250)));
    }

    #[test]
    fn hotkey_overlay_shows_once_when_tiling_is_first_enabled() {
        let mut pending = true;
        let show = HotkeyOverlay::default();
        assert!(!hotkeys_at_first_enable(&mut pending, false, show));
        assert!(hotkeys_at_first_enable(&mut pending, true, show));
        assert!(!hotkeys_at_first_enable(&mut pending, true, show));
        let mut pending = true;
        let skip = HotkeyOverlay {
            skip_at_startup: true,
        };
        assert!(!hotkeys_at_first_enable(&mut pending, true, skip));
        assert!(!pending);
    }

    #[test]
    fn hotkey_overlay_binding_serializes_for_the_frontend() {
        let binding = Config::builtin_shortcuts()
            .into_iter()
            .find(|binding| binding.key == "Control+Alt+Slash")
            .unwrap();
        assert_eq!(binding.action, ShortcutAction::HotkeyOverlay {});
        assert_eq!(
            serde_json::to_value(&binding.action).unwrap(),
            serde_json::json!({"type": "hotkeyOverlay"})
        );
        let focus = Config::builtin_shortcuts().into_iter().next().unwrap();
        assert_eq!(
            serde_json::to_value(&focus).unwrap(),
            serde_json::json!({"key": "Control+Alt+H", "action": {"type": "command", "command": {"type": "focusDirection", "direction": "left"}}})
        );
    }

    #[test]
    fn spring_samples_match_the_frontend_port() {
        // ui/tests/overview-zoom.test.mjs checks the same values against springProgress.
        let progress = |t: f64| crate::animation::spring(0.0, 1.0, 0.0, t, 1.0).0;
        assert_eq!(progress(0.0), 0.0);
        assert_eq!(progress(1.0), 1.0);
        assert!(
            (progress(0.5) - 0.820_682).abs() < 1e-5,
            "{}",
            progress(0.5)
        );
        assert!(
            (progress(0.25) - 0.449_069).abs() < 1e-5,
            "{}",
            progress(0.25)
        );
        // Duration-independent shape: a 400 ms spring at 200 ms equals the unit curve at 0.5.
        let scaled = crate::animation::spring(100.0, 300.0, 0.0, 0.2, 0.4).0;
        assert!((scaled - (100.0 + 200.0 * progress(0.5))).abs() < 1e-9);
    }
}
