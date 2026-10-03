//! Modifier + left drag of a floating window: it follows the pointer, turned into ordinary
//! layout commands. Tiled windows are previewed and dropped by the drag overlay instead.
//! Physical screen pixels.
use crate::model::{Command, Rect, Snapshot, WindowId};

pub struct Gesture {
    pub window_id: WindowId,
    start: (i64, i64),
    rect: Rect,
}

/// A window in a column of the layout (not floating, not layout fullscreen).
pub fn tiled(snapshot: &Snapshot, window_id: &str) -> bool {
    snapshot
        .windows
        .iter()
        .any(|w| w.native.id == window_id && !w.floating && !w.fullscreen)
        && snapshot
            .monitors
            .iter()
            .flat_map(|m| &m.pages)
            .flat_map(|p| &p.columns)
            .any(|c| c.windows.iter().any(|w| w == window_id))
}

impl Gesture {
    /// Only floating, non-fullscreen windows follow the pointer.
    pub fn start(snapshot: &Snapshot, window_id: &str, x: i32, y: i32) -> Option<Self> {
        let window = snapshot
            .windows
            .iter()
            .find(|w| w.native.id == window_id && w.floating && !w.fullscreen)?;
        Some(Self {
            window_id: window_id.into(),
            start: (x as i64, y as i64),
            rect: window.native.rect,
        })
    }

    /// The command for the pointer at (x, y).
    pub fn update(&mut self, x: i32, y: i32, _release: bool) -> Vec<Command> {
        let clamp = |v: i64| v.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        vec![Command::SetFloatingRect {
            window_id: self.window_id.clone(),
            rect: Rect {
                x: clamp(self.rect.x as i64 + x as i64 - self.start.0),
                y: clamp(self.rect.y as i64 + y as i64 - self.start.1),
                ..self.rect
            },
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    fn snapshot(floating: bool) -> Snapshot {
        let rect = Rect {
            x: 100,
            y: 100,
            width: 400,
            height: 300,
        };
        let mut snapshot = Snapshot::default();
        snapshot.windows.push(WindowState {
            native: NativeWindow {
                id: "w".into(),
                title: String::new(),
                app_name: String::new(),
                process_id: 1,
                monitor_id: "m".into(),
                rect,
                minimized: false,
                minimized_by_manager: false,
                resizable: true,
            },
            floating,
            fullscreen: false,
        });
        snapshot
    }

    #[test]
    fn floating_windows_follow_and_tiled_windows_are_left_to_the_overlay() {
        let mut g = Gesture::start(&snapshot(true), "w", 150, 150).unwrap();
        assert!(matches!(&g.update(170, 140, false)[..],
            [Command::SetFloatingRect { rect, .. }] if (rect.x, rect.y, rect.width) == (120, 90, 400)));
        let mut tiled_snapshot = snapshot(false);
        assert!(!tiled(&tiled_snapshot, "w"), "no column holds it yet");
        tiled_snapshot.monitors.push(MonitorState {
            monitor: Monitor {
                id: "m".into(),
                name: String::new(),
                bounds: Rect::default(),
                work_area: Rect::default(),
                scale_factor: 1.0,
                primary: true,
            },
            pages: vec![Page {
                id: "p".into(),
                name: String::new(),
                columns: vec![Column {
                    id: "c".into(),
                    width: 400,
                    windows: vec!["w".into()],
                    display: ColumnDisplay::Normal,
                    active_tab: None,
                }],
                floating_windows: vec![],
                viewport_x: 0,
            }],
            active_page: "p".into(),
            viewport: Rect::default(),
        });
        assert!(tiled(&tiled_snapshot, "w"));
        assert!(Gesture::start(&tiled_snapshot, "w", 150, 150).is_none());
    }
}
