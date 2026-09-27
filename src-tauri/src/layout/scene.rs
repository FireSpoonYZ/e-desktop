//! Drag previews: the layout a command would leave on one monitor, as boxes to paint. The
//! overlay runs the same engine command a release applies, so the preview is what you get.
use super::*;

#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    /// Screen area the preview covers: the monitor's usable viewport.
    pub area: Rect,
    pub scale: f64,
    pub tiles: Vec<Tile>,
    /// Windows just off screen afterwards that were on screen before (squeezed out) or are
    /// being dragged (queued).
    pub marks: Vec<Mark>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    /// Relative to `Scene::area`.
    pub rect: Rect,
    pub title: String,
    /// Share of the screen: width, plus height for stacked windows.
    pub label: String,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub right: bool,
    pub title: String,
    pub active: bool,
}

fn percent(part: i64, whole: u32) -> i64 {
    (part * 100 + i64::from(whole) / 2) / i64::from(whole.max(1))
}

impl Engine {
    /// The tiled layout of `monitor_id`'s active page after `command`; `active` windows are
    /// highlighted. None when the command would be rejected.
    pub fn scene(&self, command: Command, monitor_id: &str, active: &[WindowId]) -> Option<Scene> {
        let shown: BTreeSet<WindowId> = self
            .placements()
            .ok()?
            .into_iter()
            .filter_map(|a| match a {
                NativeAction::Placement {
                    window_id,
                    minimized: false,
                    ..
                } => Some(window_id),
                _ => None,
            })
            .collect();
        let mut next = self.clone();
        let actions = next.dispatch(command).ok()?.actions;
        let monitor = next
            .snapshot
            .monitors
            .iter()
            .find(|m| m.monitor.id == monitor_id)?;
        let page = monitor.pages.iter().find(|p| p.id == monitor.active_page)?;
        let area = monitor.viewport;
        let view = i64::from(area.width);
        let title = |id: &str| {
            next.snapshot
                .windows
                .iter()
                .find(|w| w.native.id == id)
                .map(|w| {
                    if w.native.title.is_empty() {
                        w.native.app_name.clone()
                    } else {
                        w.native.title.clone()
                    }
                })
                .unwrap_or_default()
        };
        let mut scene = Scene {
            area,
            scale: monitor.monitor.scale_factor,
            tiles: vec![],
            marks: vec![],
        };
        let mut left = -i64::from(page.viewport_x);
        for column in &page.columns {
            let width = i64::from(column.width);
            let visible = (left + width).min(view) - left.max(0);
            let heights = next.column_heights(column, area.height);
            for (id, height) in column.windows.iter().zip(heights) {
                let Some((rect, clip, minimized)) = actions.iter().find_map(|a| match a {
                    NativeAction::Placement {
                        window_id,
                        rect,
                        clip,
                        minimized,
                    } if window_id == id => Some((*rect, *clip, *minimized)),
                    _ => None,
                }) else {
                    continue;
                };
                let active = active.contains(id);
                if minimized {
                    if active || shown.contains(id) {
                        scene.marks.push(Mark {
                            right: left + width / 2 >= view / 2,
                            title: title(id),
                            active,
                        });
                    }
                    continue;
                }
                let r = clip.unwrap_or(rect);
                let w = percent(visible, area.width);
                let label = if column.windows.len() > 1 {
                    format!("{w}% × {}%", percent(height.into(), area.height))
                } else {
                    format!("{w}%")
                };
                scene.tiles.push(Tile {
                    rect: Rect {
                        x: coordinate(i64::from(r.x) - i64::from(area.x)),
                        y: coordinate(i64::from(r.y) - i64::from(area.y)),
                        ..r
                    },
                    title: title(id),
                    label,
                    active,
                });
            }
            left += width;
        }
        Some(scene)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::engine;
    use super::*;

    #[test]
    fn scene_shows_the_result_and_marks_windows_pushed_off_screen() {
        let e = engine();
        // Monitor a: columns 1, 2, 3 of 600 px; 1 and 2 on screen.
        let scene = e
            .scene(
                Command::DropWindow {
                    window_id: "2".into(),
                    x: 1190,
                    y: 450,
                },
                "a",
                &["2".into()],
            )
            .unwrap();
        let tiles: Vec<_> = scene
            .tiles
            .iter()
            .map(|t| (t.title.as_str(), t.rect.x, t.label.as_str(), t.active))
            .collect();
        assert_eq!(tiles, [("1", 0, "50%", false), ("3", 600, "50%", false)]);
        assert_eq!(
            scene.marks,
            [Mark {
                right: true,
                title: "2".into(),
                active: true
            }]
        );
        // The engine itself is untouched; a rejected command has no scene.
        assert_eq!(e.snapshot().monitors[0].pages[0].columns[1].windows, ["2"]);
        assert!(
            e.scene(
                Command::DropWindow {
                    window_id: "2".into(),
                    x: 99_999,
                    y: 0,
                },
                "a",
                &[],
            )
            .is_none()
        );
    }
}
