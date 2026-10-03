//! Layout kept across restarts (lane: persistence-ipc). The saved model structures are matched
//! back onto the monitors and windows enumerated after the restart.
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use super::*;

pub const LAYOUT_STATE_VERSION: u32 = 1;
/// Quiet period after the last layout change before the state file is written.
pub const SAVE_DELAY: Duration = Duration::from_secs(1);

/// Contents of `layout-state.json`. Model structs are stored whole, so fields added to them
/// are saved and restored without changes here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutState {
    pub version: u32,
    pub monitors: Vec<MonitorState>,
    pub windows: Vec<WindowState>,
    pub focused_window: Option<WindowId>,
    pub active_monitor: Option<MonitorId>,
    /// Stack height shares keyed by saved window id.
    #[serde(default)]
    pub height_weights: BTreeMap<WindowId, u32>,
}

impl LayoutState {
    /// Rejects malformed files and other versions instead of guessing at their layout.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        match value.get("version").and_then(serde_json::Value::as_u64) {
            Some(version) if version == u64::from(LAYOUT_STATE_VERSION) => {}
            Some(version) => {
                return Err(format!(
                    "版本 {version} 不受支持（需要 {LAYOUT_STATE_VERSION}）"
                ));
            }
            None => return Err("缺少版本号".into()),
        }
        serde_json::from_value(value).map_err(|e| e.to_string())
    }
}

/// `layout-state.json` next to the configuration file.
pub fn state_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name("layout-state.json")
}

type Tier<T> = fn(&T, &T) -> bool;

/// Saved index -> current index. Tiers run strictest first; every saved and every current
/// entry is used at most once, earlier entries first.
fn assign<T>(saved: &[&T], current: &[&T], tiers: &[Tier<T>]) -> Vec<Option<usize>> {
    let mut result = vec![None; saved.len()];
    let mut used = vec![false; current.len()];
    for tier in tiers {
        for (s, entry) in saved.iter().enumerate() {
            if result[s].is_some() {
                continue;
            }
            if let Some(c) = (0..current.len()).find(|&c| !used[c] && tier(entry, current[c])) {
                used[c] = true;
                result[s] = Some(c);
            }
        }
    }
    result
}

/// Device id, then device name, then the remaining monitors in order.
const MONITOR_TIERS: [Tier<Monitor>; 3] = [
    |s, c| s.id == c.id,
    |s, c| !s.name.is_empty() && s.name == c.name,
    |_, _| true,
];

/// Window ids are session cookies, so the exact tier only hits within one process. Then the
/// still running process (pid + executable) with its title, the executable with the title,
/// the running process alone, and finally the executable alone. The backends expose no
/// window class; `appName` is the executable on Windows and WM_CLASS on X11.
const WINDOW_TIERS: [Tier<NativeWindow>; 5] = [
    |s, c| s.id == c.id && s.process_id == c.process_id && s.app_name == c.app_name,
    |s, c| {
        !s.app_name.is_empty()
            && s.process_id == c.process_id
            && s.app_name == c.app_name
            && s.title == c.title
    },
    |s, c| !s.app_name.is_empty() && s.app_name == c.app_name && s.title == c.title,
    |s, c| !s.app_name.is_empty() && s.process_id == c.process_id && s.app_name == c.app_name,
    |s, c| !s.app_name.is_empty() && s.app_name == c.app_name,
];

impl Engine {
    pub fn layout_state(&self) -> LayoutState {
        LayoutState {
            version: LAYOUT_STATE_VERSION,
            monitors: self.snapshot.monitors.clone(),
            windows: self.snapshot.windows.clone(),
            focused_window: self.snapshot.focused_window.clone(),
            active_monitor: self.snapshot.active_monitor.clone(),
            height_weights: self.height_weights.clone(),
        }
    }

    /// Rebuild the saved pages on matched monitors with the matched windows. Windows on those
    /// monitors that match nothing go through the new-window rules; other monitors are kept.
    /// Returns how many windows were put back. Transactional like `dispatch`.
    pub fn restore_layout(&mut self, saved: &LayoutState) -> Result<usize, AppError> {
        let mut next = self.clone();
        let restored = next.restore_inner(saved)?;
        *self = next;
        Ok(restored)
    }

    fn restore_inner(&mut self, saved: &LayoutState) -> Result<usize, AppError> {
        let monitors = assign(
            &saved
                .monitors
                .iter()
                .map(|m| &m.monitor)
                .collect::<Vec<_>>(),
            &self
                .snapshot
                .monitors
                .iter()
                .map(|m| &m.monitor)
                .collect::<Vec<_>>(),
            &MONITOR_TIERS,
        );
        // Only windows on a page of a matched monitor have somewhere to go back to.
        let mut saved_viewport = BTreeMap::new();
        for (monitor, target) in saved.monitors.iter().zip(&monitors) {
            if target.is_some() {
                for id in monitor.pages.iter().flat_map(ids) {
                    saved_viewport.entry(id.clone()).or_insert(monitor.viewport);
                }
            }
        }
        let placed: Vec<&WindowState> = saved
            .windows
            .iter()
            .filter(|w| saved_viewport.contains_key(&w.native.id))
            .collect();
        let current: Vec<&NativeWindow> = self.snapshot.windows.iter().map(|w| &w.native).collect();
        let map: BTreeMap<WindowId, WindowId> = placed
            .iter()
            .zip(assign(
                &placed.iter().map(|w| &w.native).collect::<Vec<_>>(),
                &current,
                &WINDOW_TIERS,
            ))
            .filter_map(|(w, c)| Some((w.native.id.clone(), current[c?].id.clone())))
            .collect();
        if map.is_empty() {
            return Ok(0);
        }
        let moved: BTreeSet<WindowId> = map.values().cloned().collect();
        for id in &moved {
            self.remove_window(id);
        }
        self.minimized_slots
            .retain(|slot| !moved.contains(&slot.id));
        let mut displaced = vec![];
        let mut seen = BTreeSet::new();
        for (saved_monitor, target) in saved.monitors.iter().zip(&monitors) {
            let Some(m) = *target else {
                continue;
            };
            displaced.extend(
                self.snapshot.monitors[m]
                    .pages
                    .iter()
                    .flat_map(ids)
                    .cloned(),
            );
            let mut pages = vec![];
            let mut active = String::new();
            for saved_page in &saved_monitor.pages {
                let mut page = saved_page.clone();
                page.id = self.id("page");
                if saved_page.id == saved_monitor.active_page {
                    active = page.id.clone();
                }
                let mut floating = vec![];
                for column in &mut page.columns {
                    column.id = self.id("column");
                    let mut tiled = vec![];
                    for id in column.windows.iter().filter_map(|id| map.get(id)) {
                        if !seen.insert(id.clone()) {
                            continue;
                        }
                        // A window that lost its resize frame can only float.
                        if self.snapshot.windows[self.window_index(id)?]
                            .native
                            .resizable
                        {
                            tiled.push(id.clone());
                        } else {
                            floating.push(id.clone());
                        }
                    }
                    column.windows = tiled;
                }
                page.columns.retain(|column| !column.windows.is_empty());
                for id in saved_page
                    .floating_windows
                    .iter()
                    .filter_map(|id| map.get(id))
                {
                    if seen.insert(id.clone()) {
                        floating.push(id.clone());
                    }
                }
                page.floating_windows = floating;
                pages.push(page);
            }
            let monitor = &mut self.snapshot.monitors[m];
            monitor.pages = pages;
            monitor.active_page = active;
        }
        let mut origins = BTreeMap::new();
        for window in placed {
            let Some(id) = map.get(&window.native.id) else {
                continue;
            };
            let floating = matches!(self.location(id)?, (_, _, None));
            let w = self.window_index(id)?;
            let state = &mut self.snapshot.windows[w];
            state.floating = floating;
            state.fullscreen = window.fullscreen && !floating;
            self.fullscreen_restore.remove(id);
            if floating && window.floating {
                state.native.rect = window.native.rect;
                origins.insert(
                    id.clone(),
                    (saved_viewport[&window.native.id], window.native.rect),
                );
                // Keep the saved rectangle until it is placed, like a rule move.
                self.pending_rule_floating.insert(id.clone());
            } else if floating {
                self.size_floating.insert(id.clone());
            } else {
                self.size_floating.remove(id);
            }
        }
        for (saved_id, weight) in &saved.height_weights {
            if let Some(id) = map.get(saved_id) {
                self.height_weights.insert(id.clone(), *weight);
            }
        }
        self.rebase_floating(origins);
        // Valid active pages before new-window rules look them up.
        self.cleanup();
        for id in displaced {
            if self.location(&id).is_err() && self.minimized_slots.iter().all(|s| s.id != id) {
                self.insert_new_window(&id)?;
            }
        }
        if let Some(m) = saved
            .active_monitor
            .as_ref()
            .and_then(|id| saved.monitors.iter().position(|m| &m.monitor.id == id))
            .and_then(|s| monitors[s])
        {
            self.snapshot.active_monitor = Some(self.snapshot.monitors[m].monitor.id.clone());
        }
        self.cleanup();
        match saved
            .focused_window
            .as_ref()
            .and_then(|id| map.get(id))
            .filter(|id| self.navigable(id) && self.location(id).is_ok())
        {
            Some(id) => self.set_focus(&id.clone(), false)?,
            None => {
                self.snapshot.focused_window = None;
                self.focus_active_page();
            }
        }
        self.cleanup();
        Ok(map.len())
    }
}

/// Debounced writer of the state file and the single restore attempt of this process.
pub struct LayoutStateFile {
    pub path: PathBuf,
    latest: Vec<u8>,
    due: Option<Instant>,
    restore_attempted: bool,
}

impl LayoutStateFile {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            latest: vec![],
            due: None,
            restore_attempted: false,
        }
    }

    /// Call after every controller step. Only a managed layout is saved, `SAVE_DELAY` after
    /// its last change; once paused, a pending change is written at once.
    pub fn observe(&mut self, engine: &Engine, now: Instant) -> Result<(), String> {
        if !engine.snapshot().enabled {
            return self.flush();
        }
        let bytes = serde_json::to_vec(&engine.layout_state()).map_err(|e| e.to_string())?;
        if bytes != self.latest {
            self.latest = bytes;
            self.due = Some(now + SAVE_DELAY);
        }
        if self.due.is_some_and(|due| now >= due) {
            return self.flush();
        }
        Ok(())
    }

    /// Write a pending change now (pause and quit).
    pub fn flush(&mut self) -> Result<(), String> {
        if self.due.take().is_none() {
            return Ok(());
        }
        let write = || {
            if let Some(directory) = self.path.parent() {
                std::fs::create_dir_all(directory)?;
            }
            // Replace atomically so a crash never leaves a truncated file behind.
            let temporary = self.path.with_extension("json.tmp");
            std::fs::write(&temporary, &self.latest)?;
            std::fs::rename(&temporary, &self.path)
        };
        write().map_err(|e: std::io::Error| e.to_string())
    }

    /// Restore once per process, on the first enable that sees monitors. A missing file is not
    /// an error; a malformed or other-version file is reported and ignored.
    pub fn restore(&mut self, engine: &mut Engine, allowed: bool) -> Result<usize, String> {
        if self.restore_attempted
            || engine.snapshot().enabled
            || engine.snapshot().monitors.is_empty()
        {
            return Ok(0);
        }
        self.restore_attempted = true;
        if !allowed {
            return Ok(0);
        }
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.to_string()),
        };
        let state = LayoutState::parse(&bytes)?;
        engine.restore_layout(&state).map_err(|e| e.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(id: &str, name: &str, x: i32) -> Monitor {
        let area = Rect {
            x,
            y: 0,
            width: 1200,
            height: 900,
        };
        Monitor {
            id: id.into(),
            name: name.into(),
            bounds: area,
            work_area: area,
            scale_factor: 1.0,
            primary: x == 0,
        }
    }

    fn window(id: &str, app: &str, title: &str, pid: u32, monitor_id: &str) -> NativeWindow {
        NativeWindow {
            id: id.into(),
            title: title.into(),
            app_name: app.into(),
            process_id: pid,
            monitor_id: monitor_id.into(),
            rect: Rect {
                x: 40,
                y: 40,
                width: 500,
                height: 400,
            },
            minimized: false,
            minimized_by_manager: false,
            resizable: true,
        }
    }

    fn engine(system: SystemSnapshot) -> Engine {
        let mut engine = Engine::new(BackendStatus {
            availability: BackendAvailability::Ready,
            capabilities: Capabilities {
                enumerate: true,
                placement: true,
                minimize: true,
                clipping: true,
                focus: true,
                close: true,
                ..Capabilities::default()
            },
            ..BackendStatus::default()
        });
        engine.reconcile(system).unwrap();
        engine
    }

    fn page_windows(engine: &Engine, monitor: usize) -> Vec<Vec<Vec<String>>> {
        engine.snapshot().monitors[monitor]
            .pages
            .iter()
            .map(|page| page.columns.iter().map(|c| c.windows.clone()).collect())
            .collect()
    }

    /// Session one: editor+terminal stacked, browser in its own column on page 1, a floating
    /// calculator, and mail on page 2. Focus on the terminal.
    fn saved_session() -> LayoutState {
        let windows = vec![
            window("w1", "editor.exe", "main.rs", 10, "m1"),
            window("w2", "terminal.exe", "shell", 11, "m1"),
            window("w3", "browser.exe", "Docs", 12, "m1"),
            window("w4", "calc.exe", "Calculator", 13, "m1"),
            window("w5", "mail.exe", "Inbox", 14, "m1"),
        ];
        let mut e = engine(SystemSnapshot {
            monitors: vec![monitor("m1", r"\\.\DISPLAY1", 0)],
            windows,
            focused_window: Some("w2".into()),
        });
        e.dispatch(Command::Enable).unwrap();
        e.dispatch(Command::MoveWindow {
            direction: Direction::Left,
        })
        .unwrap();
        e.dispatch(Command::FocusWindow {
            window_id: "w4".into(),
        })
        .unwrap();
        e.dispatch(Command::ToggleFloating).unwrap();
        e.dispatch(Command::SetFloatingRect {
            window_id: "w4".into(),
            rect: Rect {
                x: 300,
                y: 200,
                width: 320,
                height: 480,
            },
        })
        .unwrap();
        let page2 = e.snapshot().monitors[0].pages[1].id.clone();
        e.dispatch(Command::MoveWindowToPage {
            window_id: "w5".into(),
            page_id: page2,
        })
        .unwrap();
        e.dispatch(Command::SetWindowColumnWidth {
            window_id: "w3".into(),
            width: 700,
        })
        .unwrap();
        e.dispatch(Command::FocusWindow {
            window_id: "w2".into(),
        })
        .unwrap();
        e.dispatch(Command::AdjustWindowHeight { delta: 100 })
            .unwrap();
        assert_eq!(
            page_windows(&e, 0),
            vec![
                vec![vec!["w1".to_string(), "w2".into()], vec!["w3".into()]],
                vec![vec!["w5".into()]],
                vec![],
            ]
        );
        let state = e.layout_state();
        // Round trip through the file format.
        LayoutState::parse(&serde_json::to_vec(&state).unwrap()).unwrap()
    }

    #[test]
    fn restore_rebuilds_pages_columns_floating_focus_and_heights_after_restart() {
        let saved = saved_session();
        // New process: new session ids, new monitor handle, same device name, other order.
        let mut e = engine(SystemSnapshot {
            monitors: vec![monitor("h9", r"\\.\DISPLAY1", 0)],
            windows: vec![
                window("n1", "mail.exe", "Inbox", 24, "h9"),
                window("n2", "calc.exe", "Calculator", 23, "h9"),
                window("n3", "browser.exe", "Other tab", 22, "h9"),
                window("n4", "terminal.exe", "shell", 21, "h9"),
                window("n5", "editor.exe", "main.rs", 20, "h9"),
                window("n6", "new.exe", "Unsaved", 25, "h9"),
            ],
            focused_window: None,
        });
        assert_eq!(e.restore_layout(&saved).unwrap(), 5);
        assert_eq!(
            page_windows(&e, 0),
            vec![
                vec![
                    vec!["n5".to_string(), "n4".into()],
                    vec!["n3".into()],
                    vec!["n6".into()],
                ],
                vec![vec!["n1".into()]],
                vec![],
            ]
        );
        let snapshot = e.snapshot();
        let page = &snapshot.monitors[0].pages[0];
        assert_eq!(page.columns[1].width, 700);
        assert_eq!(page.floating_windows, ["n2"]);
        let calc = snapshot
            .windows
            .iter()
            .find(|w| w.native.id == "n2")
            .unwrap();
        assert!(calc.floating);
        assert_eq!(
            calc.native.rect,
            Rect {
                x: 300,
                y: 200,
                width: 320,
                height: 480
            }
        );
        assert_eq!(snapshot.focused_window.as_deref(), Some("n4"));
        assert_eq!(snapshot.monitors[0].active_page, page.id);
        assert_eq!(
            e.height_weights.get("n4"),
            saved.height_weights.get("w2"),
            "stack heights follow the window"
        );
        assert!(e.height_weights.contains_key("n4"));
        // Enabling places the saved floating rectangle, not the enumerated one.
        let actions = e.dispatch(Command::Enable).unwrap().actions;
        assert!(actions.iter().any(|a| matches!(a,
            NativeAction::Placement { window_id, rect, .. }
                if window_id == "n2" && rect.x == 300 && rect.height == 480)));
    }

    #[test]
    fn window_matching_prefers_live_process_and_title_and_uses_each_window_once() {
        let saved = [
            window("w1", "app.exe", "A", 5, "m"),
            window("w2", "app.exe", "B", 5, "m"),
            window("w3", "app.exe", "C", 6, "m"),
            window("w4", "", "", 0, "m"),
        ];
        let current = [
            window("x1", "app.exe", "B", 5, "m"),
            window("x2", "app.exe", "A", 7, "m"),
            window("x3", "app.exe", "Z", 5, "m"),
            window("x4", "", "", 0, "m"),
        ];
        let saved: Vec<_> = saved.iter().collect();
        let current_refs: Vec<_> = current.iter().collect();
        let result = assign(&saved, &current_refs, &WINDOW_TIERS);
        // w2 keeps its live process and title; w1 prefers its title over pid alone;
        // w3 takes the last app.exe window; an anonymous window never matches loosely.
        assert_eq!(result, vec![Some(1), Some(0), Some(2), None]);
        // The same session id with the same live process is exact, whatever the title.
        let same = [window("w1", "app.exe", "renamed", 5, "m")];
        let result = assign(&[saved[0]], &same.iter().collect::<Vec<_>>(), &WINDOW_TIERS);
        assert_eq!(result, vec![Some(0)]);
    }

    #[test]
    fn monitors_match_by_id_then_name_then_remaining_order() {
        let saved = [
            monitor("a", "DISPLAY1", 0),
            monitor("b", "DISPLAY2", 1200),
            monitor("c", "DISPLAY3", 2400),
        ];
        let current = [
            monitor("z", "DISPLAY9", 0),
            monitor("y", "DISPLAY2", 1200),
            monitor("a", "DISPLAY7", 2400),
        ];
        let result = assign(
            &saved.iter().collect::<Vec<_>>(),
            &current.iter().collect::<Vec<_>>(),
            &MONITOR_TIERS,
        );
        assert_eq!(result, vec![Some(2), Some(1), Some(0)]);
        let fewer = [monitor("q", "DISPLAY5", 0)];
        let result = assign(
            &saved.iter().collect::<Vec<_>>(),
            &fewer.iter().collect::<Vec<_>>(),
            &MONITOR_TIERS,
        );
        assert_eq!(result, vec![Some(0), None, None]);
    }

    #[test]
    fn unmatched_monitor_layout_and_nonresizable_windows_stay_valid() {
        let saved = saved_session();
        let mut fixed = window("n1", "editor.exe", "main.rs", 20, "b");
        fixed.resizable = false;
        let mut e = engine(SystemSnapshot {
            monitors: vec![monitor("a", "OTHER", -1200), monitor("b", "", 0)],
            windows: vec![
                fixed,
                window("n2", "terminal.exe", "shell", 21, "b"),
                window("n3", "stay.exe", "Left alone", 22, "a"),
            ],
            focused_window: None,
        });
        let before_a = page_windows(&e, 0);
        // The saved monitor falls back to the first current monitor in order: "a".
        assert_eq!(e.restore_layout(&saved).unwrap(), 2);
        let page = &e.snapshot().monitors[0].pages[0];
        assert_eq!(page.columns.len(), 2, "terminal plus the displaced window");
        assert_eq!(page.columns[0].windows, ["n2"]);
        assert_eq!(page.columns[1].windows, ["n3"]);
        assert_eq!(page.floating_windows, ["n1"], "lost resize frame floats");
        assert!(
            e.snapshot()
                .windows
                .iter()
                .find(|w| w.native.id == "n1")
                .unwrap()
                .floating
        );
        assert_ne!(page_windows(&e, 0), before_a);
        // Nothing to match leaves the engine untouched.
        let mut empty = engine(SystemSnapshot {
            monitors: vec![monitor("a", "", 0)],
            windows: vec![window("k", "unknown.exe", "?", 1, "a")],
            focused_window: None,
        });
        let before = serde_json::to_value(empty.snapshot()).unwrap();
        assert_eq!(empty.restore_layout(&saved).unwrap(), 0);
        assert_eq!(serde_json::to_value(empty.snapshot()).unwrap(), before);
    }

    #[test]
    fn state_file_rejects_corrupt_and_other_versions() {
        let saved = saved_session();
        let mut value = serde_json::to_value(&saved).unwrap();
        assert!(LayoutState::parse(&serde_json::to_vec(&value).unwrap()).is_ok());
        value["version"] = 2.into();
        let error = LayoutState::parse(&serde_json::to_vec(&value).unwrap()).unwrap_err();
        assert!(error.contains('2'), "{error}");
        for text in [&b"{"[..], b"[]", br#"{"version":1}"#, br#"{"monitors":[]}"#] {
            assert!(LayoutState::parse(text).is_err());
        }
        assert_eq!(
            state_path(Path::new("dir/config.json")),
            Path::new("dir/layout-state.json")
        );
    }

    #[test]
    fn saves_are_debounced_flushed_on_pause_and_restored_once() {
        let directory =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/layout-state-tests");
        let path = directory.join(format!("state-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut file = LayoutStateFile::new(path.clone());
        let system = SystemSnapshot {
            monitors: vec![monitor("m", "", 0)],
            windows: vec![
                window("a", "one.exe", "One", 1, "m"),
                window("b", "two.exe", "Two", 2, "m"),
            ],
            focused_window: Some("a".into()),
        };
        let mut e = engine(system.clone());
        let start = Instant::now();
        // Paused layouts are not saved.
        file.observe(&e, start).unwrap();
        assert!(!path.exists());
        e.dispatch(Command::Enable).unwrap();
        file.observe(&e, start).unwrap();
        file.observe(&e, start + SAVE_DELAY / 2).unwrap();
        assert!(!path.exists(), "waits for the quiet period");
        file.observe(&e, start + SAVE_DELAY).unwrap();
        assert!(path.exists());
        e.dispatch(Command::MoveWindow {
            direction: Direction::Right,
        })
        .unwrap();
        file.observe(&e, start + SAVE_DELAY).unwrap();
        e.dispatch(Command::Disable).unwrap();
        file.observe(&e, start + SAVE_DELAY).unwrap();
        let written = LayoutState::parse(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written.monitors[0].pages[0].columns[0].windows, ["b", "a"]);

        let mut restarted = engine(system);
        let mut fresh = LayoutStateFile::new(path.clone());
        assert_eq!(fresh.restore(&mut restarted, true).unwrap(), 2);
        assert_eq!(
            page_windows(&restarted, 0)[0],
            vec![vec!["b".to_string(), "a".into()]]
        );
        assert_eq!(fresh.restore(&mut restarted, true).unwrap(), 0, "only once");

        std::fs::write(&path, b"{\"version\":99}").unwrap();
        let mut corrupt = LayoutStateFile::new(path.clone());
        let mut untouched = engine(SystemSnapshot {
            monitors: vec![monitor("m", "", 0)],
            windows: vec![],
            focused_window: None,
        });
        assert!(corrupt.restore(&mut untouched, true).is_err());
        std::fs::remove_file(&path).unwrap();
        let mut missing = LayoutStateFile::new(path);
        assert_eq!(missing.restore(&mut untouched, true).unwrap(), 0);
    }
}
