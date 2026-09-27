//! Modifier + left drag: floating windows follow the pointer, tiled windows are inserted where
//! dropped. Turned into ordinary layout commands. Physical screen pixels.
use crate::model::{Command, Rect, Snapshot, WindowId};

pub struct Gesture {
    pub window_id: WindowId,
    floating: bool,
    start: (i64, i64),
    rect: Rect,
}

impl Gesture {
    pub fn start(snapshot: &Snapshot, window_id: &str, x: i32, y: i32) -> Option<Self> {
        let window = snapshot.windows.iter().find(|w| w.native.id == window_id)?;
        let tiled = snapshot
            .monitors
            .iter()
            .flat_map(|m| &m.pages)
            .flat_map(|p| &p.columns)
            .any(|c| c.windows.iter().any(|w| w == window_id));
        if window.fullscreen || !(window.floating || tiled) {
            return None;
        }
        Some(Self {
            window_id: window_id.into(),
            floating: window.floating,
            start: (x as i64, y as i64),
            rect: window.native.rect,
        })
    }

    /// Commands for the pointer at (x, y); `release` ends the gesture.
    pub fn update(&mut self, x: i32, y: i32, release: bool) -> Vec<Command> {
        let window_id = self.window_id.clone();
        if !self.floating {
            return if release {
                vec![Command::DropWindow { window_id, x, y }]
            } else {
                vec![]
            };
        }
        let clamp = |v: i64| v.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        vec![Command::SetFloatingRect {
            window_id,
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
    fn gestures_map_to_layout_commands() {
        let mut g = Gesture::start(&snapshot(true), "w", 150, 150).unwrap();
        assert!(matches!(&g.update(170, 140, false)[..],
            [Command::SetFloatingRect { rect, .. }] if (rect.x, rect.y, rect.width) == (120, 90, 400)));
        // Tiled windows need a column; this snapshot has none.
        assert!(Gesture::start(&snapshot(false), "w", 0, 0).is_none());
        let mut tiled = snapshot(false);
        tiled.monitors.push(MonitorState {
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
                }],
                floating_windows: vec![],
                viewport_x: 0,
            }],
            active_page: "p".into(),
            viewport: Rect::default(),
        });
        let mut g = Gesture::start(&tiled, "w", 150, 150).unwrap();
        assert!(g.update(900, 150, false).is_empty());
        assert!(matches!(
            &g.update(900, 160, true)[..],
            [Command::DropWindow { x: 900, y: 160, .. }]
        ));
    }
}
