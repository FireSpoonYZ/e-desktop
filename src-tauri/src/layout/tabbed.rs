//! lane: tabbed. niri tabbed column display: a tabbed column shows only its active window,
//! at full column height below a tab indicator strip. The other tabs are hidden with the same
//! manager minimization that hides off-screen columns and background pages, so pause and quit
//! restore them like any other managed window. A column with one window lays out normally.
use super::*;

/// Logical pixels reserved above a tabbed column's window for the tab indicator.
pub const TAB_INDICATOR_HEIGHT: f64 = 12.0;

/// The tab indicator of one tabbed column shown on its monitor (physical screen pixels).
#[derive(Debug, Clone, PartialEq)]
pub struct TabBar {
    pub monitor_id: MonitorId,
    /// The whole strip; it extends past the screen edge with a partly visible column.
    pub rect: Rect,
    /// The part of `rect` on screen.
    pub visible: Rect,
    pub tabs: Vec<WindowId>,
    pub active: usize,
    pub scale: f64,
}

/// The window a tabbed column shows when it has several; None lays the column out normally.
pub(super) fn shown_tab(column: &Column) -> Option<&WindowId> {
    if column.display != ColumnDisplay::Tabbed || column.windows.len() < 2 {
        return None;
    }
    column
        .active_tab
        .as_ref()
        .filter(|id| column.windows.contains(id))
        .or(column.windows.first())
}

/// Split a column's tile into the indicator strip on top and the window below it.
pub(super) fn split_tab_bar(tile: Rect, scale: f64) -> (Rect, Rect) {
    let bar =
        ((TAB_INDICATOR_HEIGHT * scale).round().max(1.0) as u32).min(tile.height.saturating_sub(1));
    (
        Rect {
            height: bar,
            ..tile
        },
        Rect {
            y: tile.y.saturating_add(bar as i32),
            height: tile.height - bar,
            ..tile
        },
    )
}

/// Index of the tab under screen column `x`: the strip is split into equal segments.
pub fn tab_at(bar: &TabBar, x: i32) -> Option<usize> {
    let offset = i64::from(x) - i64::from(bar.rect.x);
    let width = i64::from(bar.rect.width);
    if bar.tabs.is_empty() || offset < 0 || offset >= width {
        return None;
    }
    Some((offset * bar.tabs.len() as i64 / width) as usize)
}

fn intersection(a: Rect, b: Rect) -> Option<Rect> {
    let left = i64::from(a.x).max(i64::from(b.x));
    let top = i64::from(a.y).max(i64::from(b.y));
    let right = (i64::from(a.x) + i64::from(a.width)).min(i64::from(b.x) + i64::from(b.width));
    let bottom = (i64::from(a.y) + i64::from(a.height)).min(i64::from(b.y) + i64::from(b.height));
    (left < right && top < bottom).then(|| Rect {
        x: coordinate(left),
        y: coordinate(top),
        width: (right - left) as u32,
        height: (bottom - top) as u32,
    })
}

/// Indicators for every tabbed column whose active window is placed on screen, on unpaused
/// monitors' active pages without a layout fullscreen window. Empty while paused.
pub fn tab_bars(engine: &Engine) -> Vec<TabBar> {
    let snapshot = engine.snapshot();
    let Ok(actions) = engine.placements() else {
        return vec![];
    };
    let shown: BTreeSet<&WindowId> = actions
        .iter()
        .filter_map(|action| match action {
            NativeAction::Placement {
                window_id,
                minimized: false,
                ..
            } => Some(window_id),
            _ => None,
        })
        .collect();
    let mut bars = Vec::new();
    for monitor in &snapshot.monitors {
        let Some(page) = monitor.pages.iter().find(|p| p.id == monitor.active_page) else {
            continue;
        };
        if ids(page).any(|id| {
            snapshot
                .windows
                .iter()
                .any(|w| &w.native.id == id && w.fullscreen)
        }) {
            continue;
        }
        let scale = monitor.monitor.scale_factor;
        let half = half_gap(snapshot.gaps, scale);
        let outer = expand_gap(monitor.viewport, half);
        let mut x = i64::from(monitor.viewport.x) - i64::from(page.viewport_x);
        for column in &page.columns {
            if let Some(active) = shown_tab(column).filter(|id| shown.contains(id)) {
                let tile = inset_gap(
                    Rect {
                        x: coordinate(x),
                        y: monitor.viewport.y,
                        width: column.width,
                        height: monitor.viewport.height,
                    },
                    half,
                );
                let (rect, _) = split_tab_bar(tile, scale);
                if let Some(visible) = intersection(rect, outer) {
                    bars.push(TabBar {
                        monitor_id: monitor.monitor.id.clone(),
                        rect,
                        visible,
                        tabs: column.windows.clone(),
                        active: column.windows.iter().position(|id| id == active).unwrap(),
                        scale,
                    });
                }
            }
            x += i64::from(column.width);
        }
    }
    bars
}

impl Engine {
    pub fn set_default_column_display(&mut self, display: ColumnDisplay) {
        self.default_column_display = display;
    }

    pub fn default_column_display(&self) -> ColumnDisplay {
        self.default_column_display
    }

    /// A window hidden behind the active tab of its tabbed column.
    pub fn hidden_tab(&self, id: &str) -> bool {
        let Ok((m, p, Some((c, _)))) = self.location(id) else {
            return false;
        };
        shown_tab(&self.snapshot.monitors[m].pages[p].columns[c]).is_some_and(|tab| tab != id)
    }

    /// Tabbed columns show their remembered focus (or first navigable window); normal
    /// columns carry no active tab.
    pub(super) fn sync_active_tabs(&mut self) {
        for m in 0..self.snapshot.monitors.len() {
            for p in 0..self.snapshot.monitors[m].pages.len() {
                for c in 0..self.snapshot.monitors[m].pages[p].columns.len() {
                    let column = &self.snapshot.monitors[m].pages[p].columns[c];
                    let active = match column.display {
                        ColumnDisplay::Tabbed => self.column_focus_target(column),
                        ColumnDisplay::Normal => None,
                    };
                    self.snapshot.monitors[m].pages[p].columns[c].active_tab = active;
                }
            }
        }
    }

    pub(super) fn toggle_column_tabbed_display(&mut self) -> Result<(), AppError> {
        let id = self.focused()?;
        let (m, p, location) = self.location(&id)?;
        let (c, _) = location.ok_or_else(|| invalid("Floating windows have no column"))?;
        let column = &mut self.snapshot.monitors[m].pages[p].columns[c];
        column.display = match column.display {
            ColumnDisplay::Normal => ColumnDisplay::Tabbed,
            ColumnDisplay::Tabbed => ColumnDisplay::Normal,
        };
        self.remember_column_focus(&id);
        self.sync_active_tabs();
        self.ensure_visible(&id, false)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::engine;
    use super::*;

    fn layout(e: &Engine) -> Vec<Vec<String>> {
        e.snapshot.monitors[0].pages[0]
            .columns
            .iter()
            .map(|c| c.windows.clone())
            .collect()
    }

    fn placement(t: &Transition, id: &str) -> (Rect, Option<Rect>, bool) {
        t.actions
            .iter()
            .find_map(|a| match a {
                NativeAction::Placement {
                    window_id,
                    rect,
                    clip,
                    minimized,
                } if window_id == id => Some((*rect, *clip, *minimized)),
                _ => None,
            })
            .unwrap()
    }

    fn focus(e: &mut Engine, id: &str) -> Transition {
        e.dispatch(Command::FocusWindow {
            window_id: id.into(),
        })
        .unwrap()
    }

    fn move_window(e: &mut Engine, direction: Direction) -> Transition {
        e.dispatch(Command::MoveWindow { direction }).unwrap()
    }

    /// Monitor a (1200x900): column [1, 2] (600 px) then [3]; 1 focused, column tabbed.
    fn tabbed() -> Engine {
        let mut e = engine();
        focus(&mut e, "2");
        move_window(&mut e, Direction::Left);
        focus(&mut e, "1");
        assert_eq!(layout(&e), [vec!["1", "2"], vec!["3"]]);
        e.dispatch(Command::ToggleColumnTabbedDisplay).unwrap();
        e
    }

    #[test]
    fn tabbed_column_shows_only_the_active_window_below_the_indicator() {
        let mut e = tabbed();
        let column = &e.snapshot.monitors[0].pages[0].columns[0];
        assert_eq!(column.display, ColumnDisplay::Tabbed);
        assert_eq!(column.active_tab.as_deref(), Some("1"));
        let t = e.dispatch(Command::Refresh).unwrap();
        assert!(t.actions.is_empty());
        let t = e.dispatch(Command::CenterFocused).unwrap();
        let bar = (TAB_INDICATOR_HEIGHT).round() as u32;
        let (shown, _, hidden) = placement(&t, "1");
        assert!(!hidden);
        assert_eq!(
            (shown.y, shown.height, shown.width),
            (bar as i32, 900 - bar, 600)
        );
        let (rect, clip, hidden) = placement(&t, "2");
        assert!(hidden && clip.is_none());
        assert_eq!(
            rect, shown,
            "a hidden tab is restored straight into the shown slot"
        );
        assert!(e.hidden_tab("2") && !e.hidden_tab("1") && !e.hidden_tab("3"));
        let bars = tab_bars(&e);
        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].tabs, ["1", "2"]);
        assert_eq!(bars[0].active, 0);
        assert_eq!(
            bars[0].rect,
            Rect {
                x: shown.x,
                y: 0,
                width: 600,
                height: bar
            }
        );
        assert_eq!(tab_at(&bars[0], shown.x + 299), Some(0));
        assert_eq!(tab_at(&bars[0], shown.x + 300), Some(1));
        assert_eq!(tab_at(&bars[0], shown.x + 600), None);
        // Toggling back stacks the rows again with no indicator.
        let t = e.dispatch(Command::ToggleColumnTabbedDisplay).unwrap();
        assert!(!placement(&t, "2").2);
        assert_eq!(placement(&t, "1").0.height, 450);
        assert!(tab_bars(&e).is_empty());
        assert_eq!(e.snapshot.monitors[0].pages[0].columns[0].active_tab, None);
    }

    #[test]
    fn focus_up_down_switches_tabs_and_left_right_returns_to_the_active_tab() {
        let mut e = tabbed();
        let t = e
            .dispatch(Command::FocusDirection {
                direction: Direction::Down,
            })
            .unwrap();
        assert_eq!(e.snapshot.focused_window.as_deref(), Some("2"));
        assert!(placement(&t, "1").2 && !placement(&t, "2").2);
        assert!(t.actions.contains(&NativeAction::Focus {
            window_id: "2".into()
        }));
        e.dispatch(Command::FocusDirection {
            direction: Direction::Right,
        })
        .unwrap();
        assert_eq!(e.snapshot.focused_window.as_deref(), Some("3"));
        // The tabbed column keeps showing tab 2 while another column has focus.
        assert!(e.hidden_tab("1"));
        e.dispatch(Command::FocusDirection {
            direction: Direction::Left,
        })
        .unwrap();
        assert_eq!(e.snapshot.focused_window.as_deref(), Some("2"));
        e.dispatch(Command::FocusDirection {
            direction: Direction::Up,
        })
        .unwrap();
        assert_eq!(e.snapshot.focused_window.as_deref(), Some("1"));
        assert!(e.hidden_tab("2"));
    }

    #[test]
    fn windows_joining_a_tabbed_column_become_the_active_tab() {
        let mut e = tabbed();
        focus(&mut e, "3");
        let t = move_window(&mut e, Direction::Left);
        assert_eq!(layout(&e), [vec!["1", "2", "3"]]);
        assert_eq!(
            e.snapshot.monitors[0].pages[0].columns[0]
                .active_tab
                .as_deref(),
            Some("3")
        );
        assert!(!placement(&t, "3").2 && placement(&t, "1").2 && placement(&t, "2").2);
        // Leaving again: the rest of the column stays tabbed and shows a remaining tab.
        move_window(&mut e, Direction::Right);
        assert_eq!(layout(&e), [vec!["1", "2"], vec!["3"]]);
        let column = &e.snapshot.monitors[0].pages[0].columns[0];
        assert_eq!(column.display, ColumnDisplay::Tabbed);
        assert!(column.active_tab.is_some());
        // The expelled window got a fresh (default normal) column.
        assert_eq!(
            e.snapshot.monitors[0].pages[0].columns[1].display,
            ColumnDisplay::Normal
        );
    }

    #[test]
    fn a_drop_into_a_tabbed_column_inserts_after_the_active_tab_and_activates() {
        let mut e = tabbed();
        focus(&mut e, "2");
        let page = &e.snapshot.monitors[0].pages[0];
        let left = -page.viewport_x;
        // Middle of the tabbed column, near its top: y does not pick a row in tabs.
        e.dispatch(Command::DropWindow {
            window_id: "3".into(),
            x: left + 300,
            y: 850,
            page_id: None,
            viewport_x: None,
        })
        .unwrap();
        assert_eq!(layout(&e), [vec!["1", "2", "3"]]);
        assert_eq!(e.snapshot.focused_window.as_deref(), Some("3"));
        assert!(e.hidden_tab("1") && e.hidden_tab("2") && !e.hidden_tab("3"));
        // Row boundaries do not exist in a tabbed column.
        assert!(
            e.dispatch(Command::DragRow {
                monitor_id: "a".into(),
                column: 0,
                edge: 1,
                delta: 50,
            })
            .is_err()
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn tabbed_columns_have_no_row_splitter_strips() {
        let mut e = tabbed();
        let rows = |e: &Engine| {
            crate::platform::splitter::strips(e)
                .iter()
                .filter(|s| s.column.is_some())
                .count()
        };
        assert_eq!(rows(&e), 0);
        e.dispatch(Command::ToggleColumnTabbedDisplay).unwrap();
        assert_eq!(rows(&e), 1);
    }

    #[test]
    fn single_window_tabbed_column_lays_out_normally() {
        let mut e = engine();
        focus(&mut e, "3");
        e.dispatch(Command::ToggleColumnTabbedDisplay).unwrap();
        let t = e.dispatch(Command::CenterFocused).unwrap();
        let (rect, _, hidden) = placement(&t, "3");
        assert!(!hidden);
        assert_eq!((rect.y, rect.height), (0, 900));
        assert!(tab_bars(&e).is_empty());
        assert!(!e.hidden_tab("3"));
    }

    #[test]
    fn pause_restores_hidden_tabs_and_default_display_applies_to_new_columns() {
        let mut e = tabbed();
        let t = e.dispatch(Command::Disable).unwrap();
        for id in ["1", "2"] {
            assert!(t.actions.contains(&NativeAction::Restore {
                window_id: id.into()
            }));
        }
        assert!(tab_bars(&e).is_empty());
        let mut e = engine();
        e.set_default_column_display(ColumnDisplay::Tabbed);
        focus(&mut e, "3");
        move_window(&mut e, Direction::Left);
        // Merging keeps the existing column's display; a new column takes the default.
        assert_eq!(
            e.snapshot.monitors[0].pages[0].columns[1].display,
            ColumnDisplay::Normal
        );
        move_window(&mut e, Direction::Right);
        let column = e.snapshot.monitors[0].pages[0].columns.last().unwrap();
        assert_eq!(
            (column.display, column.windows.clone()),
            (ColumnDisplay::Tabbed, vec!["3".to_string()])
        );
    }

    #[test]
    fn column_display_and_config_are_backward_compatible() {
        let column: Column =
            serde_json::from_str(r#"{"id":"c","width":10,"windows":["w"]}"#).unwrap();
        assert_eq!(
            (column.display, column.active_tab.as_deref()),
            (ColumnDisplay::Normal, None)
        );
        let value = serde_json::to_value(&column).unwrap();
        assert_eq!(value["display"], "normal");
        assert!(value.get("activeTab").is_none());
        assert_eq!(
            serde_json::to_value(Command::ToggleColumnTabbedDisplay).unwrap(),
            serde_json::json!({"type": "toggleColumnTabbedDisplay"})
        );
        use crate::config::{Config, ShortcutAction};
        assert_eq!(
            Config::parse(b"{}").unwrap().default_column_display,
            ColumnDisplay::Normal
        );
        assert_eq!(
            Config::parse(br#"{"defaultColumnDisplay":"tabbed"}"#)
                .unwrap()
                .default_column_display,
            ColumnDisplay::Tabbed
        );
        assert!(Config::parse(br#"{"defaultColumnDisplay":"stacked"}"#).is_err());
        let binding = Config::builtin_shortcuts()
            .into_iter()
            .find(|b| b.key == "Control+Alt+W")
            .unwrap();
        assert_eq!(
            binding.action,
            ShortcutAction::Command {
                command: Command::ToggleColumnTabbedDisplay
            }
        );
        assert!(Config::parse(br#"{"shortcuts":[{"key":"A","action":{"type":"command","command":{"type":"toggleColumnTabbedDisplay"}}}]}"#).is_ok());
        assert!(Config::parse(br#"{"shortcuts":[{"key":"A","action":{"type":"command","command":{"type":"toggleColumnTabbedDisplay","extra":1}}}]}"#).is_err());
    }
}
