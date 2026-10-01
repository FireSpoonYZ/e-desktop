use std::{collections::HashSet, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    model::{Command, Direction, Snapshot},
    rules::WindowRule,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Layered over the built-in map by key; `unbind` removes a built-in key.
    pub shortcuts: Vec<ShortcutBinding>,
    /// false: `shortcuts` is the complete map (`[]` disables all global shortcuts).
    pub default_shortcuts: bool,
    pub window_rules: Vec<WindowRule>,
    /// Layout and overview animations; 0 disables them, maximum 1000 ms.
    pub animation_duration_ms: u32,
    /// Hovering a managed window focuses it (niri focus-follows-mouse).
    pub focus_follows_mouse: bool,
    /// Keyboard/command focus changes move the pointer into the focused window.
    pub warp_mouse_to_focus: bool,
    /// The top-left pixel of each monitor toggles the overview.
    pub hot_corners: bool,
    /// Held with left drag to move managed windows; null disables.
    pub drag_modifier: Option<DragModifier>,
    /// Logical pixels between tiled windows and around the usable edges. Maximum 256.
    pub gaps: u32,
    /// False hides every top bar and reserves no space for them.
    pub top_bar: bool,
    /// `#rrggbb` focus border for managed windows; null leaves the system color. Windows 11 only.
    pub focus_border_color: Option<String>,
    /// Windows 11 corner preference. `system` does not override DWM.
    pub window_corners: WindowCorners,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DragModifier {
    Alt,
    Super,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WindowCorners {
    System,
    Round,
    RoundSmall,
    Square,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShortcutBinding {
    pub key: String,
    pub action: ShortcutAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ShortcutAction {
    Command {
        #[serde(with = "StrictCommand")]
        command: Command,
    },
    // Empty struct variants (not unit variants) make serde reject extra fields.
    Overview {},
    Commands {},
    Quit {},
    Page {
        number: usize,
        #[serde(default)]
        move_window: bool,
    },
    RelativePage {
        delta: i32,
        #[serde(default)]
        move_window: bool,
    },
    Scroll {
        direction: HorizontalDirection,
    },
    /// Removes the built-in binding for this key; dropped before registration.
    Unbind {},
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HorizontalDirection {
    Left,
    Right,
}

// The IPC Command remains unchanged; configuration rejects misspelled command fields too.
#[derive(Serialize, Deserialize)]
#[serde(
    remote = "Command",
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum StrictCommand {
    Refresh {},
    Enable {},
    Disable {},
    FocusWindow {
        window_id: String,
    },
    FocusDirection {
        direction: Direction,
    },
    SwitchPage {
        monitor_id: String,
        page_id: String,
    },
    AddPage {
        monitor_id: String,
    },
    MoveWindowToPage {
        window_id: String,
        page_id: String,
    },
    MoveWindow {
        direction: Direction,
    },
    CycleWidth {},
    SetColumnWidth {
        width: u32,
    },
    SetWindowColumnWidth {
        window_id: String,
        width: u32,
    },
    AdjustColumnWidth {
        delta: i32,
    },
    AdjustWindowHeight {
        delta: i32,
    },
    ResetWindowHeights {},
    CenterFocused {},
    Scroll {
        monitor_id: String,
        delta: i32,
    },
    SlideColumn {
        direction: Direction,
    },
    ToggleFloating {},
    ToggleFullscreen {},
    CloseWindow {
        window_id: String,
    },
    DropWindow {
        window_id: String,
        x: i32,
        y: i32,
    },
    DragEdge {
        monitor_id: String,
        edge: u32,
        delta: i32,
    },
    DragRow {
        monitor_id: String,
        column: u32,
        edge: u32,
        delta: i32,
    },
    SetFloatingRect {
        window_id: String,
        rect: crate::model::Rect,
    },
}

impl Default for Config {
    fn default() -> Self {
        Self {
            shortcuts: vec![],
            default_shortcuts: true,
            window_rules: vec![],
            animation_duration_ms: 160,
            focus_follows_mouse: false,
            warp_mouse_to_focus: false,
            hot_corners: true,
            drag_modifier: Some(DragModifier::Alt),
            gaps: 0,
            top_bar: true,
            focus_border_color: Some("#7fc8ff".into()),
            window_corners: WindowCorners::System,
        }
    }
}

fn colorref(hex: &str) -> Option<u32> {
    let hex = hex.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let rgb = u32::from_str_radix(hex, 16).ok()?;
    Some((rgb >> 16 & 0xff) | ((rgb >> 8 & 0xff) << 8) | ((rgb & 0xff) << 16))
}

impl Config {
    /// COLORREF `0x00BBGGRR` for `focus_border_color`, if it is `#rrggbb`.
    pub fn focus_colorref(&self) -> Option<u32> {
        self.focus_border_color.as_deref().and_then(colorref)
    }

    pub fn builtin_shortcuts() -> Vec<ShortcutBinding> {
        let mut shortcuts = Vec::new();
        let mut bind = |key: String, action| shortcuts.push(ShortcutBinding { key, action });
        for (key, direction) in [
            ("H", Direction::Left),
            ("J", Direction::Down),
            ("K", Direction::Up),
            ("L", Direction::Right),
        ] {
            bind(
                format!("Control+Alt+{key}"),
                ShortcutAction::Command {
                    command: Command::FocusDirection { direction },
                },
            );
            bind(
                format!("Control+Alt+Shift+{key}"),
                ShortcutAction::Command {
                    command: Command::MoveWindow { direction },
                },
            );
        }
        for (key, command) in [
            ("R", Command::CycleWidth),
            ("C", Command::CenterFocused),
            ("F", Command::ToggleFullscreen),
            ("V", Command::ToggleFloating),
            ("Backspace", Command::Disable),
            (
                "N",
                Command::AddPage {
                    monitor_id: String::new(),
                },
            ),
        ] {
            bind(
                format!("Control+Alt+{key}"),
                ShortcutAction::Command { command },
            );
        }
        for (key, action) in [
            ("Space", ShortcutAction::Commands {}),
            ("O", ShortcutAction::Overview {}),
            ("Q", ShortcutAction::Quit {}),
        ] {
            bind(format!("Control+Alt+{key}"), action);
        }
        for (key, delta) in [("PageUp", -1), ("PageDown", 1)] {
            for (modifier, move_window) in [("", false), ("Shift+", true)] {
                bind(
                    format!("Control+Alt+{modifier}{key}"),
                    ShortcutAction::RelativePage { delta, move_window },
                );
            }
        }
        for number in 1..=9 {
            for (modifier, move_window) in [("", false), ("Shift+", true)] {
                bind(
                    format!("Control+Alt+{modifier}{number}"),
                    ShortcutAction::Page {
                        number,
                        move_window,
                    },
                );
            }
        }
        for (key, direction) in [
            ("Left", HorizontalDirection::Left),
            ("Right", HorizontalDirection::Right),
        ] {
            for modifiers in ["Control", "Control+Alt"] {
                bind(
                    format!("{modifiers}+{key}"),
                    ShortcutAction::Scroll { direction },
                );
            }
        }
        for (key, command) in [
            ("Left", Command::AdjustColumnWidth { delta: -50 }),
            ("Right", Command::AdjustColumnWidth { delta: 50 }),
            ("Up", Command::AdjustWindowHeight { delta: 50 }),
            ("Down", Command::AdjustWindowHeight { delta: -50 }),
            ("R", Command::ResetWindowHeights),
        ] {
            bind(
                format!("Control+Alt+Shift+{key}"),
                ShortcutAction::Command { command },
            );
        }
        shortcuts
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let config: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if config.animation_duration_ms > 1000 {
            return Err("animationDurationMs 须在 0 到 1000 之间。".into());
        }
        if config.gaps > 256 {
            return Err("gaps 须在 0 到 256 之间。".into());
        }
        if config
            .focus_border_color
            .as_deref()
            .is_some_and(|color| colorref(color).is_none())
        {
            return Err("focusBorderColor 须为 #rrggbb 或 null。".into());
        }
        for (index, rule) in config.window_rules.iter().enumerate() {
            rule.validate()
                .map_err(|e| format!("窗口规则 {}：{}", index + 1, e.message))?;
        }
        let mut keys = HashSet::new();
        for binding in &config.shortcuts {
            if binding.key.trim().is_empty() || !keys.insert(&binding.key) {
                return Err(format!("快捷键为空或重复：{}", binding.key));
            }
            if matches!(
                binding.action,
                ShortcutAction::Command {
                    command: Command::SetColumnWidth { width: 0 }
                }
            ) {
                return Err(format!("快捷键 {} 的列宽须大于 0。", binding.key));
            }
            if matches!(
                binding.action,
                ShortcutAction::Page { number: 0, .. }
                    | ShortcutAction::RelativePage { delta: 0, .. }
            ) {
                return Err(format!(
                    "快捷键 {} 的页面编号须从 1 开始，相对偏移不能为 0。",
                    binding.key
                ));
            }
        }
        Ok(config)
    }

    pub fn normalize_keys(
        mut self,
        normalize: impl Fn(&str) -> Result<String, String>,
    ) -> Result<Self, String> {
        let mut keys = HashSet::new();
        for binding in &mut self.shortcuts {
            binding.key = normalize(&binding.key)?;
            if !keys.insert(binding.key.clone()) {
                return Err(format!("快捷键重复：{}", binding.key));
            }
        }
        if self.default_shortcuts {
            let mut merged = Vec::new();
            for mut binding in Self::builtin_shortcuts() {
                binding.key = normalize(&binding.key)?;
                if !keys.contains(&binding.key) {
                    merged.push(binding);
                }
            }
            merged.append(&mut self.shortcuts);
            self.shortcuts = merged;
            // The map is now complete; normalizing again must not re-add unbound keys.
            self.default_shortcuts = false;
        }
        self.shortcuts
            .retain(|binding| binding.action != ShortcutAction::Unbind {});
        Ok(self)
    }
}

impl ShortcutAction {
    /// Resolve only on the controller thread, against its execution-time snapshot.
    pub fn resolve(&self, snapshot: &Snapshot) -> Option<Self> {
        let (index, delta, move_window) = match self {
            Self::Page {
                number,
                move_window,
            } => (Some(number.checked_sub(1)?), 0, *move_window),
            Self::RelativePage { delta, move_window } => (None, *delta, *move_window),
            Self::Scroll { direction } => {
                return Some(Self::Command {
                    command: Command::SlideColumn {
                        direction: match direction {
                            HorizontalDirection::Left => Direction::Left,
                            HorizontalDirection::Right => Direction::Right,
                        },
                    },
                });
            }
            _ => return Some(self.clone()),
        };
        let monitor = snapshot
            .monitors
            .iter()
            .find(|m| Some(&m.monitor.id) == snapshot.active_monitor.as_ref())
            .or(snapshot.monitors.first())?;
        let current = monitor
            .pages
            .iter()
            .position(|p| p.id == monitor.active_page)?;
        let target = index.unwrap_or_else(|| (current as i64 + i64::from(delta)).max(0) as usize);
        let page = monitor.pages.get(target)?;
        Some(Self::Command {
            command: if move_window {
                Command::MoveWindowToPage {
                    window_id: snapshot.focused_window.clone()?,
                    page_id: page.id.clone(),
                }
            } else {
                Command::SwitchPage {
                    monitor_id: monitor.monitor.id.clone(),
                    page_id: page.id.clone(),
                }
            },
        })
    }
}

/// Tracks bytes, not timestamps: atomic saves and same-size edits are both detected.
/// A failed candidate is observed once; the caller commits only after registration succeeds.
pub struct ConfigFile {
    pub path: PathBuf,
    observed: Option<Result<Option<Vec<u8>>, String>>,
}

impl ConfigFile {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            observed: None,
        }
    }

    pub fn poll(&mut self) -> Option<Result<Config, String>> {
        let contents = match std::fs::read(&self.path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        };
        if self.observed.as_ref() == Some(&contents) {
            return None;
        }
        self.observed = Some(contents.clone());
        Some(contents.and_then(|bytes| match bytes {
            Some(bytes) => Config::parse(&bytes),
            None => Ok(Config::default()),
        }))
    }
}

/// Implicit shortcuts follow the snapshot's active monitor, which stays behind an unmanaged
/// fullscreen window. Block those. A command that names an unsuspended monitor still runs.
pub fn foreground_blocks_shortcut(action: &ShortcutAction, suspended: &[String]) -> bool {
    match action {
        ShortcutAction::Quit {} | ShortcutAction::Unbind {} => false,
        ShortcutAction::Overview {}
        | ShortcutAction::Commands {}
        | ShortcutAction::Page { .. }
        | ShortcutAction::RelativePage { .. }
        | ShortcutAction::Scroll { .. } => true,
        ShortcutAction::Command { command } => !explicit_live_target(command, suspended),
    }
}

fn explicit_live_target(command: &Command, suspended: &[String]) -> bool {
    let live = |id: &str| !id.is_empty() && !suspended.iter().any(|paused| paused == id);
    match command {
        Command::Refresh | Command::Enable | Command::Disable => true,
        Command::SwitchPage { monitor_id, .. }
        | Command::AddPage { monitor_id }
        | Command::Scroll { monitor_id, .. }
        | Command::DragEdge { monitor_id, .. }
        | Command::DragRow { monitor_id, .. } => live(monitor_id),
        _ => false,
    }
}

/// Check both native foreground and the resolved host before showing any surface.
pub fn foreground_blocks_show(
    explicit: Option<&str>,
    resolved: Option<&str>,
    foreground_suspended: bool,
    suspended: &[String],
) -> bool {
    (explicit.is_none() && foreground_suspended)
        || explicit
            .or(resolved)
            .is_some_and(|id| suspended.iter().any(|paused| paused == id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{Monitor, MonitorState, Page, Rect},
        shortcuts::Shortcuts,
    };

    #[test]
    fn defaults_empty_mapping_and_strict_schema() {
        assert_eq!(Config::parse(b"{}").unwrap(), Config::default());
        assert_eq!(Config::builtin_shortcuts().len(), 48);
        assert!(
            Config::parse(br#"{"shortcuts":[]}"#)
                .unwrap()
                .shortcuts
                .is_empty()
        );
        let binding =
            r#"{"key":"Control+Alt+A","action":{"type":"command","command":{"type":"disable"}}}"#;
        assert_eq!(
            Config::parse(format!(r#"{{"shortcuts":[{binding}]}}"#).as_bytes())
                .unwrap()
                .shortcuts
                .len(),
            1
        );
        assert!(
            Config::parse(format!(r#"{{"shortcuts":[{binding},{binding}]}}"#).as_bytes()).is_err()
        );
        for text in [
            r#"{"shortcut":[]}"#,
            r#"{"shortcuts":null}"#,
            r#"{"shortcuts":[{"key":"A","unknown":true,"action":{"type":"quit"}}]}"#,
            r#"{"shortcuts":[{"key":"","action":{"type":"quit"}}]}"#,
            r#"{"shortcuts":[{"key":"A","action":{"type":"unknown"}}]}"#,
            r#"{"shortcuts":[{"key":"A","action":{"type":"quit","extra":1}}]}"#,
            r#"{"shortcuts":[{"key":"A","action":{"type":"page","number":0}}]}"#,
            r#"{"shortcuts":[{"key":"A","action":{"type":"page","number":-1}}]}"#,
            r#"{"shortcuts":[{"key":"A","action":{"type":"relativePage","delta":0}}]}"#,
            r#"{"shortcuts":[{"key":"A","action":{"type":"scroll","direction":"up"}}]}"#,
            r#"{"shortcuts":[{"key":"A","action":{"type":"page","number":1,"delta":1}}]}"#,
            r#"{"shortcuts":[{"key":"A","action":{"type":"command","command":{"type":"disable","extra":1}}}]}"#,
            r#"{"shortcuts":[{"key":"A","action":{"type":"command","command":{"type":"scroll","monitorId":"a","delta":2,"extra":1}}}]}"#,
        ] {
            assert!(Config::parse(text.as_bytes()).is_err(), "accepted: {text}");
        }
        let defaults = Config::default();
        assert_eq!(
            Config::parse(&serde_json::to_vec(&defaults).unwrap()).unwrap(),
            defaults
        );
    }

    #[test]
    fn shortcuts_layer_over_builtins_unless_disabled() {
        let resolve = |text: &str| {
            Config::parse(text.as_bytes())
                .unwrap()
                .normalize_keys(|k| Ok(k.to_owned()))
                .unwrap()
        };
        let focus_right = ShortcutAction::Command {
            command: Command::FocusDirection {
                direction: Direction::Right,
            },
        };
        let action = |config: &Config, key: &str| {
            config
                .shortcuts
                .iter()
                .find(|b| b.key == key)
                .map(|b| b.action.clone())
        };
        assert_eq!(resolve("{}").shortcuts, Config::builtin_shortcuts());
        assert_eq!(resolve(r#"{"shortcuts":[]}"#).shortcuts.len(), 48);
        let config = resolve(
            r#"{"shortcuts":[
                {"key":"Control+Alt+L","action":{"type":"unbind"}},
                {"key":"Control+Alt+Semicolon","action":{"type":"command","command":{"type":"focusDirection","direction":"right"}}},
                {"key":"Control+Alt+O","action":{"type":"quit"}}
            ]}"#,
        );
        assert_eq!(config.shortcuts.len(), 48);
        assert_eq!(action(&config, "Control+Alt+L"), None);
        assert_eq!(action(&config, "Control+Alt+Semicolon"), Some(focus_right));
        assert_eq!(
            action(&config, "Control+Alt+O"),
            Some(ShortcutAction::Quit {})
        );
        assert_eq!(action(&config, "Control+Alt+H").is_some(), true);
        // Idempotent: a normalized map is complete and keeps its unbinds.
        assert_eq!(
            config.clone().normalize_keys(|k| Ok(k.to_owned())).unwrap(),
            config
        );
        assert!(
            resolve(r#"{"defaultShortcuts":false}"#)
                .shortcuts
                .is_empty()
        );
        assert_eq!(
            resolve(
                r#"{"defaultShortcuts":false,"shortcuts":[{"key":"A","action":{"type":"quit"}}]}"#
            )
            .shortcuts
            .len(),
            1
        );
    }

    #[test]
    fn animation_duration_bounds_and_invalid_retention() {
        assert_eq!(Config::parse(b"{}").unwrap().animation_duration_ms, 160);
        let mut shortcuts = Shortcuts::new(Config::default());
        for ms in [0, 150, 1000] {
            let config =
                Config::parse(format!(r#"{{"animationDurationMs":{ms}}}"#).as_bytes()).unwrap();
            shortcuts.replace(config, |_, _| Ok(())).unwrap();
            assert_eq!(shortcuts.config.animation_duration_ms, ms);
        }
        let previous = shortcuts.config.clone();
        for value in ["1001", "-1", "1.5", "null", "4294967296", "\"160\""] {
            assert!(
                Config::parse(format!(r#"{{"animationDurationMs":{value}}}"#).as_bytes()).is_err()
            );
            assert_eq!(shortcuts.config, previous);
        }
    }

    #[test]
    fn appearance_defaults_bounds_and_colorref() {
        let config = Config::parse(b"{}").unwrap();
        assert_eq!(config.gaps, 0);
        assert!(config.top_bar);
        assert_eq!(config.focus_border_color.as_deref(), Some("#7fc8ff"));
        assert_eq!(config.window_corners, WindowCorners::System);
        assert_eq!(config.focus_colorref(), Some(0x00ff_c87f));
        let config = Config::parse(
            br#"{"gaps":0,"topBar":false,"focusBorderColor":null,"windowCorners":"roundSmall"}"#,
        )
        .unwrap();
        assert_eq!(config.focus_colorref(), None);
        assert_eq!(
            (
                config.gaps,
                config.top_bar,
                config.focus_border_color,
                config.window_corners
            ),
            (0, false, None, WindowCorners::RoundSmall)
        );
        assert_eq!(
            Config::parse(br##"{"focusBorderColor":"#ABCDEF"}"##)
                .unwrap()
                .focus_colorref(),
            Some(0x00ef_cdab)
        );
        for text in [
            r#"{"gaps":257}"#,
            r#"{"gaps":-1}"#,
            r#"{"topBar":"yes"}"#,
            r#"{"focusBorderColor":"7fc8ff"}"#,
            r##"{"focusBorderColor":"#7fc8f"}"##,
            r##"{"focusBorderColor":"#7fc8ffff"}"##,
            r#"{"windowCorners":"circle"}"#,
        ] {
            assert!(Config::parse(text.as_bytes()).is_err(), "accepted: {text}");
        }
    }

    #[test]
    fn sizing_shortcuts_share_the_ipc_contract() {
        for command in [
            Command::SetColumnWidth { width: 700 },
            Command::AdjustColumnWidth { delta: -50 },
            Command::AdjustWindowHeight { delta: 50 },
            Command::ResetWindowHeights,
        ] {
            let json = serde_json::json!({"shortcuts": [{"key": "Control+Alt+W", "action": {"type": "command", "command": command}}]});
            let config = Config::parse(&serde_json::to_vec(&json).unwrap()).unwrap();
            assert_eq!(
                config.shortcuts[0].action,
                ShortcutAction::Command { command }
            );
        }
        assert!(Config::parse(br#"{"shortcuts":[{"key":"A","action":{"type":"command","command":{"type":"adjustColumnWidth","delta":50,"width":900}}}]}"#).is_err());
    }

    #[test]
    fn window_rules_load_with_shortcuts_and_only_affect_new_windows() {
        use crate::{layout::Engine, model::*};
        let config = Config::parse(
            br#"{"shortcuts":[],"windowRules":[{"appName":"editor","columnWidth":700}]}"#,
        )
        .unwrap();
        assert!(config.shortcuts.is_empty());
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
        let window = |id: &str| NativeWindow {
            id: id.into(),
            title: "Draft".into(),
            app_name: "Editor".into(),
            process_id: 1,
            monitor_id: "a".into(),
            rect: area,
            minimized: false,
            minimized_by_manager: false,
            resizable: true,
        };
        let mut system = SystemSnapshot {
            monitors: vec![Monitor {
                id: "a".into(),
                name: "Main".into(),
                bounds: area,
                work_area: area,
                scale_factor: 1.0,
                primary: true,
            }],
            windows: vec![window("old")],
            focused_window: Some("old".into()),
        };
        let mut shortcuts = Shortcuts::new(Config::default());
        shortcuts.replace(config, |_, _| Ok(())).unwrap();
        engine
            .set_window_rules(shortcuts.config.window_rules.clone())
            .unwrap();
        assert!(engine.reconcile(system.clone()).unwrap().actions.is_empty());
        // The rule's width is kept; a lone column is not stretched to the screen.
        assert_eq!(engine.snapshot().monitors[0].pages[0].columns[0].width, 700);
        engine.dispatch(Command::Enable).unwrap();
        engine
            .dispatch(Command::AdjustColumnWidth { delta: 50 })
            .unwrap();
        let updated = Config::parse(
            br#"{"shortcuts":[],"windowRules":[{"appName":"editor","columnWidth":333}]}"#,
        )
        .unwrap();
        shortcuts
            .replace(updated, |_, _| {
                panic!("unchanged bindings must not re-register")
            })
            .unwrap();
        engine
            .set_window_rules(shortcuts.config.window_rules.clone())
            .unwrap();
        for invalid in [
            br#"{"windowRules":[{"columnWidth":0}]}"#.as_slice(),
            br#"{"windowRules":[{"pageIndex":0}]}"#.as_slice(),
            br#"{"windowRules":[{"title":"editor","unknown":true,"floating":true}]}"#.as_slice(),
            br#"{"shortcuts":[{"key":"A","action":{"type":"command","command":{"type":"setColumnWidth","width":0}}}]}"#.as_slice(),
        ] { assert!(Config::parse(invalid).is_err()); }
        system.windows.push(window("new"));
        engine.reconcile(system).unwrap();
        let columns = &engine.snapshot().monitors[0].pages[0].columns;
        assert_eq!((columns[0].width, columns[1].width), (750, 333));
        assert!(Config::parse(b"{}").unwrap().window_rules.is_empty());
    }

    #[test]
    fn content_changes_invalid_retention_and_deletion() {
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/config-tests");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("reload-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut file = ConfigFile::new(path.clone());
        let mut shortcuts = Shortcuts::new(Config::default());
        shortcuts
            .replace(file.poll().unwrap().unwrap(), |_, _| Ok(()))
            .unwrap();
        assert!(file.poll().is_none());
        std::fs::write(
            &path,
            br#"{"shortcuts":[{"key":"A","action":{"type":"overview"}}]}"#,
        )
        .unwrap();
        shortcuts
            .replace(file.poll().unwrap().unwrap(), |_, _| Ok(()))
            .unwrap();
        assert!(file.poll().is_none());
        // Same length, no sleeps or timestamp dependence.
        std::fs::write(
            &path,
            br#"{"shortcuts":[{"key":"A","action":{"type":"commands"}}]}"#,
        )
        .unwrap();
        shortcuts
            .replace(file.poll().unwrap().unwrap(), |_, _| {
                panic!("same key re-registered")
            })
            .unwrap();
        assert_eq!(shortcuts.action("A"), Some(&ShortcutAction::Commands {}));
        let last_valid = shortcuts.config.clone();
        std::fs::write(&path, b"{").unwrap();
        assert!(file.poll().unwrap().is_err());
        assert!(file.poll().is_none());
        assert_eq!(shortcuts.config, last_valid);
        std::fs::write(&path, br#"{"shortcuts":[]}"#).unwrap();
        shortcuts
            .replace(file.poll().unwrap().unwrap(), |_, _| Ok(()))
            .unwrap();
        assert!(shortcuts.config.shortcuts.is_empty());
        std::fs::remove_file(&path).unwrap();
        shortcuts
            .replace(file.poll().unwrap().unwrap(), |_, _| Ok(()))
            .unwrap();
        assert_eq!(shortcuts.config, Config::default());
        assert!(file.poll().is_none());
        // A read error is not a deletion and must not restore defaults.
        std::fs::create_dir(&path).unwrap();
        assert!(file.poll().unwrap().is_err());
        assert!(file.poll().is_none());
        std::fs::remove_dir(path).unwrap();
    }

    fn monitor(id: &str, width: u32) -> MonitorState {
        MonitorState {
            monitor: Monitor {
                id: id.into(),
                name: id.into(),
                bounds: Rect::default(),
                work_area: Rect::default(),
                scale_factor: 1.0,
                primary: false,
            },
            viewport: Rect {
                width,
                ..Rect::default()
            },
            pages: (1..=3)
                .map(|number| Page {
                    id: format!("{id}-{number}"),
                    name: String::new(),
                    columns: vec![],
                    floating_windows: vec![],
                    viewport_x: 0,
                })
                .collect(),
            active_page: format!("{id}-2"),
        }
    }

    #[test]
    fn actions_resolve_from_execution_snapshot() {
        let mut snapshot = Snapshot {
            monitors: vec![monitor("a", 900), monitor("b", 1200)],
            active_monitor: Some("b".into()),
            focused_window: Some("window".into()),
            ..Snapshot::default()
        };
        let scroll = ShortcutAction::Scroll {
            direction: HorizontalDirection::Right,
        };
        assert_eq!(
            scroll.resolve(&snapshot),
            Some(ShortcutAction::Command {
                command: Command::SlideColumn {
                    direction: Direction::Right
                }
            })
        );
        let relative = ShortcutAction::RelativePage {
            delta: 1,
            move_window: false,
        };
        assert_eq!(
            relative.resolve(&snapshot),
            Some(ShortcutAction::Command {
                command: Command::SwitchPage {
                    monitor_id: "b".into(),
                    page_id: "b-3".into()
                }
            })
        );
        let page = ShortcutAction::Page {
            number: 1,
            move_window: true,
        };
        assert_eq!(
            page.resolve(&snapshot),
            Some(ShortcutAction::Command {
                command: Command::MoveWindowToPage {
                    window_id: "window".into(),
                    page_id: "b-1".into()
                }
            })
        );
        snapshot.active_monitor = Some("a".into());
        assert_eq!(
            ShortcutAction::Scroll {
                direction: HorizontalDirection::Left
            }
            .resolve(&snapshot),
            Some(ShortcutAction::Command {
                command: Command::SlideColumn {
                    direction: Direction::Left
                }
            })
        );
        snapshot.focused_window = None;
        assert!(page.resolve(&snapshot).is_none());
        assert!(
            ShortcutAction::Page {
                number: 4,
                move_window: false
            }
            .resolve(&snapshot)
            .is_none()
        );
        snapshot.monitors.clear();
        assert!(relative.resolve(&snapshot).is_none());
        for action in [
            ShortcutAction::Overview {},
            ShortcutAction::Commands {},
            ShortcutAction::Quit {},
            ShortcutAction::Command {
                command: Command::Disable,
            },
        ] {
            assert_eq!(action.resolve(&snapshot), Some(action));
        }
    }

    #[test]
    fn a_paused_foreground_blocks_implicit_shortcuts_not_an_explicit_other_monitor() {
        let paused = ["a".to_string()];
        assert!(foreground_blocks_shortcut(&ShortcutAction::Overview {}, &paused));
        assert!(foreground_blocks_shortcut(&ShortcutAction::Commands {}, &paused));
        assert!(foreground_blocks_shortcut(
            &ShortcutAction::Scroll {
                direction: HorizontalDirection::Left,
            },
            &paused,
        ));
        assert!(foreground_blocks_shortcut(
            &ShortcutAction::Command {
                command: Command::FocusDirection {
                    direction: Direction::Left,
                },
            },
            &paused,
        ));
        assert!(!foreground_blocks_shortcut(&ShortcutAction::Quit {}, &paused));
        assert!(!foreground_blocks_shortcut(
            &ShortcutAction::Command {
                command: Command::SwitchPage {
                    monitor_id: "b".into(),
                    page_id: "b-1".into(),
                },
            },
            &paused,
        ));
        assert!(foreground_blocks_shortcut(
            &ShortcutAction::Command {
                command: Command::SwitchPage {
                    monitor_id: "a".into(),
                    page_id: "a-1".into(),
                },
            },
            &paused,
        ));
        // The actual built-in AddPage action uses an empty monitor id until routing.
        let add_page = Config::builtin_shortcuts()
            .into_iter()
            .find(|binding| matches!(&binding.action,
                ShortcutAction::Command { command: Command::AddPage { monitor_id } }
                    if monitor_id.is_empty()))
            .expect("built-in implicit AddPage shortcut");
        assert!(foreground_blocks_shortcut(&add_page.action, &paused));
        assert!(!foreground_blocks_shortcut(
            &ShortcutAction::Command { command: Command::AddPage { monitor_id: "b".into() } },
            &paused,
        ));
        assert!(!foreground_blocks_show(Some("b"), Some("b"), true, &paused));
        assert!(foreground_blocks_show(Some("a"), Some("a"), false, &paused));
        assert!(foreground_blocks_show(None, Some("b"), true, &paused));
        // Ordinary unmanaged foreground on B must not show over stale layout host A.
        assert!(foreground_blocks_show(None, Some("a"), false, &paused));
        assert!(!foreground_blocks_show(None, Some("b"), false, &paused));
    }
}
