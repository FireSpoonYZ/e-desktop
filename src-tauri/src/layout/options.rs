//! lane: layout-options. niri-style preset sizes, column centering, struts and named pages.
use serde::{Deserialize, Serialize};

use super::*;
use crate::config::{Config, ShortcutAction};

/// `{"proportion": 0.5}` of the viewport, or `{"fixed": 800}` logical pixels of window size.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PresetSize {
    Proportion(f64),
    Fixed(u32),
}

// Config is Eq. Validation rejects non-finite proportions and JSON cannot carry NaN.
impl Eq for PresetSize {}

impl PresetSize {
    fn valid(self) -> bool {
        match self {
            Self::Proportion(p) => p.is_finite() && p > 0.0 && p <= 1.0,
            Self::Fixed(px) => px > 0,
        }
    }

    /// Physical size inside `total`, which already excludes the outer gap. A proportion
    /// floors like integer division; a fixed size is the window itself, so its gap is added.
    pub fn pixels(self, total: u32, scale: f64, gap: u32) -> u32 {
        let size = match self {
            // The epsilon keeps 2/3 of 1200 at 800 despite the binary fraction.
            Self::Proportion(p) => (p * f64::from(total) + 1e-6).floor(),
            Self::Fixed(px) => (f64::from(px) * scale).round() + f64::from(gap),
        };
        (size.min(f64::from(total)) as u32).max(1)
    }
}

/// When focus or an ensure-visible scroll centers the focused column (niri semantics).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CenterFocusedColumn {
    #[default]
    Never,
    Always,
    /// Center only when the column does not fit on screen with the column focus came from.
    OnOverflow,
}

/// Logical pixels removed from each side of the layout area, on top of the bar and gaps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Struts {
    pub left: u32,
    pub right: u32,
    pub top: u32,
    pub bottom: u32,
}

/// A page created at startup that stays when empty. `monitor` is a monitor id or name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NamedWorkspace {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<String>,
}

pub fn default_preset_sizes() -> Vec<PresetSize> {
    vec![
        PresetSize::Proportion(1.0 / 3.0),
        PresetSize::Proportion(0.5),
        PresetSize::Proportion(2.0 / 3.0),
    ]
}

/// The configuration fields the engine and viewport computation read.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutOptions {
    pub preset_column_widths: Vec<PresetSize>,
    pub default_column_width: PresetSize,
    pub preset_window_heights: Vec<PresetSize>,
    pub center_focused_column: CenterFocusedColumn,
    pub always_center_single_column: bool,
    pub struts: Struts,
    pub workspaces: Vec<NamedWorkspace>,
}

impl From<&Config> for LayoutOptions {
    fn from(config: &Config) -> Self {
        Self {
            preset_column_widths: config.preset_column_widths.clone(),
            default_column_width: config.default_column_width,
            preset_window_heights: config.preset_window_heights.clone(),
            center_focused_column: config.center_focused_column,
            always_center_single_column: config.always_center_single_column,
            struts: config.struts,
            workspaces: config.workspaces.clone(),
        }
    }
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self::from(&Config::default())
    }
}

#[derive(Clone, Default)]
pub(super) struct LayoutState {
    options: LayoutOptions,
    /// Declared workspaces still to create once monitors are known.
    workspaces_pending: bool,
    /// Width each maximized column returns to.
    maximized: BTreeMap<ColumnId, u32>,
}

fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Reject invalid layout options; `Config::parse` reports the message.
pub fn validate(config: &Config) -> Result<(), String> {
    for (field, presets) in [
        ("presetColumnWidths", &config.preset_column_widths),
        ("presetWindowHeights", &config.preset_window_heights),
    ] {
        if presets.is_empty() || !presets.iter().all(|p| p.valid()) {
            return Err(format!(
                "{field} 须为非空数组，proportion 在 0 到 1 之间（不含 0），fixed 为正整数。"
            ));
        }
    }
    if !config.default_column_width.valid() {
        return Err(
            "defaultColumnWidth 的 proportion 须在 0 到 1 之间（不含 0），fixed 须为正整数。"
                .into(),
        );
    }
    for (index, workspace) in config.workspaces.iter().enumerate() {
        if workspace.name.trim().is_empty()
            || workspace
                .monitor
                .as_deref()
                .is_some_and(|m| m.trim().is_empty())
        {
            return Err(format!("工作区 {}：名称和显示器不能为空白。", index + 1));
        }
        if config.workspaces[..index]
            .iter()
            .any(|other| same_name(other.name.trim(), workspace.name.trim()))
        {
            return Err(format!("工作区名称重复：{}", workspace.name));
        }
    }
    for binding in &config.shortcuts {
        if matches!(&binding.action,
            ShortcutAction::PageByName { name, .. }
            | ShortcutAction::Command { command: Command::SetPageName { name } }
                if name.trim().is_empty())
        {
            return Err(format!("快捷键 {} 的页面名称不能为空白。", binding.key));
        }
    }
    Ok(())
}

/// Shrink `rect` by logical struts, always keeping at least one pixel.
pub fn apply_struts(rect: Rect, struts: &Struts, scale: f64) -> Rect {
    let px = |value: u32| (f64::from(value) * scale).round().min(f64::from(u32::MAX)) as u32;
    let left = px(struts.left).min(rect.width.saturating_sub(1));
    let right = px(struts.right).min(rect.width.saturating_sub(1) - left);
    let top = px(struts.top).min(rect.height.saturating_sub(1));
    let bottom = px(struts.bottom).min(rect.height.saturating_sub(1) - top);
    Rect {
        x: coordinate(i64::from(rect.x) + i64::from(left)),
        y: coordinate(i64::from(rect.y) + i64::from(top)),
        width: rect.width - left - right,
        height: rect.height - top - bottom,
    }
}

/// Next (or previous) preset after `current`. An exact match steps from that preset; any
/// other size goes to the first preset past it in the cycling direction (niri).
pub fn cycle_index(presets: &[u32], current: u32, forward: bool) -> usize {
    let len = presets.len();
    if let Some(i) = presets.iter().position(|&p| p.abs_diff(current) <= 1) {
        return if forward {
            (i + 1) % len
        } else {
            (i + len - 1) % len
        };
    }
    if forward {
        presets.iter().position(|&p| p > current).unwrap_or(0)
    } else {
        presets
            .iter()
            .rposition(|&p| p < current)
            .unwrap_or(len - 1)
    }
}

/// Scroll that reveals column `c`. `previous` is the column focus came from on this page.
pub fn view_target(
    widths: &[u32],
    view: u32,
    x: i64,
    c: usize,
    center: bool,
    previous: Option<usize>,
    options: &LayoutOptions,
) -> i64 {
    let view = i64::from(view);
    let left_of = |i: usize| widths[..i].iter().map(|&w| i64::from(w)).sum::<i64>();
    let left = left_of(c);
    let width = i64::from(widths[c]);
    let centered = left + (width - view) / 2;
    if center {
        return centered;
    }
    let fit = if left < x || width >= view {
        left
    } else if left + width > x + view {
        left + width - view
    } else {
        x
    };
    // niri: a column wider than the view is aligned, not centered.
    let centered = if width >= view { fit } else { centered };
    let centering = options.center_focused_column == CenterFocusedColumn::Always
        || (options.always_center_single_column && widths.len() <= 1);
    if centering {
        return centered;
    }
    match (options.center_focused_column, previous) {
        (CenterFocusedColumn::OnOverflow, Some(previous)) => {
            // The neighbour on the side focus came from must fit on screen with the target.
            let source = if previous > c {
                (c + 1).min(widths.len() - 1)
            } else {
                c.saturating_sub(1)
            };
            let source_left = left_of(source);
            let span = if source_left < left {
                left + width - source_left
            } else {
                source_left + i64::from(widths[source]) - left
            };
            if span <= view { fit } else { centered }
        }
        _ => fit,
    }
}

/// Execution-time resolution of a `pageByName` shortcut against the snapshot.
pub fn resolve_page_by_name(snapshot: &Snapshot, name: &str, move_window: bool) -> Option<Command> {
    let (monitor, page) = snapshot.monitors.iter().find_map(|m| {
        m.pages
            .iter()
            .find(|p| snapshot.named_pages.contains(&p.id) && same_name(&p.name, name.trim()))
            .map(|p| (m, p))
    })?;
    Some(if move_window {
        Command::MoveWindowToPage {
            window_id: snapshot.focused_window.clone()?,
            page_id: page.id.clone(),
        }
    } else {
        Command::SwitchPage {
            monitor_id: monitor.monitor.id.clone(),
            page_id: page.id.clone(),
        }
    })
}

impl Engine {
    /// Replace the layout options. Changed workspace declarations are created at the next
    /// cleanup that has monitors; pages already carrying those names are reused.
    pub fn set_layout_options(&mut self, options: LayoutOptions) {
        let state = &mut self.layout_options;
        state.workspaces_pending |= options.workspaces != state.options.workspaces;
        state.options = options;
    }

    pub fn layout_options(&self) -> &LayoutOptions {
        &self.layout_options.options
    }

    fn preset_pixels(&self, presets: &[PresetSize], m: usize, total: u32) -> Vec<u32> {
        let scale = self.snapshot.monitors[m].monitor.scale_factor;
        let gap = 2 * half_gap(self.snapshot.gaps, scale);
        presets
            .iter()
            .map(|p| p.pixels(total, scale, gap))
            .collect()
    }

    /// Width of a new column on monitor `m` without a rule width.
    pub(super) fn default_column_width(&self, m: usize) -> u32 {
        let total = self.snapshot.monitors[m].viewport.width;
        let preset = [self.layout_options.options.default_column_width];
        self.preset_pixels(&preset, m, total)[0]
    }

    pub(super) fn cycle_column_width(&mut self, forward: bool) -> Result<(), AppError> {
        let (id, m, p, c, _) = self.sizing_target()?;
        let total = self.snapshot.monitors[m].viewport.width;
        let presets =
            self.preset_pixels(&self.layout_options.options.preset_column_widths, m, total);
        let width = &mut self.snapshot.monitors[m].pages[p].columns[c].width;
        *width = presets[cycle_index(&presets, *width, forward)];
        self.ensure_visible(&id, false)
    }

    /// Set the focused window to the next preset height; the rest of its column shares
    /// the remaining height. A lone window always fills its column.
    pub(super) fn cycle_window_height(&mut self) -> Result<(), AppError> {
        let (_, m, p, c, row) = self.sizing_target()?;
        let monitor = &self.snapshot.monitors[m];
        let column = &monitor.pages[p].columns[c];
        if column.windows.len() == 1 {
            return Ok(());
        }
        let total = monitor.viewport.height;
        let current = self.column_heights(column, total)[row];
        let presets =
            self.preset_pixels(&self.layout_options.options.preset_window_heights, m, total);
        let target = presets[cycle_index(&presets, current, true)];
        let delta = (i64::from(target) - i64::from(current)) as i32;
        self.adjust_window_height(delta)
    }

    /// Toggle the focused column between the viewport width and the width it had before.
    pub(super) fn toggle_maximize_column(&mut self) -> Result<(), AppError> {
        let (id, m, p, c, _) = self.sizing_target()?;
        let full = self.snapshot.monitors[m].viewport.width;
        let default = self.default_column_width(m);
        let snapshot = &self.snapshot;
        self.layout_options.maximized.retain(|column, _| {
            snapshot
                .monitors
                .iter()
                .flat_map(|m| &m.pages)
                .any(|p| p.columns.iter().any(|c| &c.id == column))
        });
        let column = &mut self.snapshot.monitors[m].pages[p].columns[c];
        let restore = self.layout_options.maximized.remove(&column.id);
        column.width = if column.width == full {
            // Already full width without a saved width (e.g. set by hand): use the default.
            restore.unwrap_or(default).min(full)
        } else {
            self.layout_options
                .maximized
                .insert(column.id.clone(), column.width);
            full
        };
        self.ensure_visible(&id, false)
    }

    fn active_page_position(&self) -> Result<(usize, usize), AppError> {
        let m = self
            .snapshot
            .active_monitor
            .as_ref()
            .and_then(|id| self.monitor_index(id).ok())
            .ok_or_else(|| invalid("No active monitor"))?;
        let monitor = &self.snapshot.monitors[m];
        let p = monitor
            .pages
            .iter()
            .position(|p| p.id == monitor.active_page)
            .unwrap();
        Ok((m, p))
    }

    /// Named pages persist while empty. A name moves off any other page that had it.
    pub(super) fn set_page_name(&mut self, name: &str) -> Result<(), AppError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(invalid("Page name must not be blank"));
        }
        let (m, p) = self.active_page_position()?;
        if let Some((other_m, other_p)) = self.named_page(name) {
            let other = self.snapshot.monitors[other_m].pages[other_p].id.clone();
            self.snapshot.named_pages.retain(|id| id != &other);
        }
        let page = &mut self.snapshot.monitors[m].pages[p];
        page.name = name.into();
        if !self.snapshot.named_pages.contains(&page.id) {
            self.snapshot.named_pages.push(page.id.clone());
        }
        Ok(())
    }

    pub(super) fn unset_page_name(&mut self) -> Result<(), AppError> {
        let (m, p) = self.active_page_position()?;
        let id = self.snapshot.monitors[m].pages[p].id.clone();
        self.snapshot.named_pages.retain(|named| named != &id);
        Ok(())
    }

    /// Monitor/page indices of the page carrying `name` (case-insensitive).
    pub(super) fn named_page(&self, name: &str) -> Option<(usize, usize)> {
        let name = name.trim();
        self.snapshot
            .monitors
            .iter()
            .enumerate()
            .find_map(|(m, monitor)| {
                monitor
                    .pages
                    .iter()
                    .position(|page| {
                        self.snapshot.named_pages.contains(&page.id) && same_name(&page.name, name)
                    })
                    .map(|p| (m, p))
            })
    }

    /// Create declared workspaces that no page carries yet, ahead of the dynamic pages and in
    /// declaration order. A missing target monitor falls back to the primary one.
    pub(super) fn ensure_named_pages(&mut self) {
        if !self.layout_options.workspaces_pending || self.snapshot.monitors.is_empty() {
            return;
        }
        self.layout_options.workspaces_pending = false;
        for workspace in self.layout_options.options.workspaces.clone() {
            let name = workspace.name.trim();
            if self.named_page(name).is_some() {
                continue;
            }
            let monitors = &self.snapshot.monitors;
            let m = workspace
                .monitor
                .as_deref()
                .and_then(|target| {
                    monitors
                        .iter()
                        .position(|m| m.monitor.id == target || same_name(&m.monitor.name, target))
                })
                .or_else(|| monitors.iter().position(|m| m.monitor.primary))
                .unwrap_or(0);
            let mut page = self.page();
            page.name = name.into();
            self.snapshot.named_pages.push(page.id.clone());
            let named = &self.snapshot.named_pages;
            let pages = &mut self.snapshot.monitors[m].pages;
            let at = pages.iter().take_while(|p| named.contains(&p.id)).count();
            pages.insert(at, page);
        }
    }

    /// Scroll so the column of `id` is visible. `previous` is the window focus moved from.
    pub(super) fn reveal(
        &mut self,
        id: &str,
        center: bool,
        previous: Option<&str>,
    ) -> Result<(), AppError> {
        let (m, p, column) = self.location(id)?;
        let Some((c, _)) = column else {
            return Ok(());
        };
        let previous = previous.and_then(|w| match self.location(w) {
            Ok((pm, pp, Some((pc, _)))) if (pm, pp) == (m, p) && pc != c => Some(pc),
            _ => None,
        });
        let viewport = self.snapshot.monitors[m].viewport.width;
        let options = &self.layout_options.options;
        let page = &mut self.snapshot.monitors[m].pages[p];
        let target = view_target(
            &widths(page),
            viewport,
            page.viewport_x.into(),
            c,
            center,
            previous,
            options,
        );
        // Keep an already visible column in place, including explicit centering/queue gaps.
        page.viewport_x = clamp_scroll_relaxed(page, viewport, target);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(json: &str) -> LayoutOptions {
        LayoutOptions::from(&Config::parse(json.as_bytes()).unwrap())
    }

    fn engine(config: &str, windows: &[&str]) -> Engine {
        let mut e = Engine::new(BackendStatus {
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
        e.set_layout_options(options(config));
        e.reconcile(system(windows)).unwrap();
        e.dispatch(Command::Enable).unwrap();
        e
    }

    fn system(windows: &[&str]) -> SystemSnapshot {
        let area = Rect {
            x: 0,
            y: 0,
            width: 1200,
            height: 900,
        };
        SystemSnapshot {
            monitors: ["a", "b"]
                .into_iter()
                .enumerate()
                .map(|(i, id)| Monitor {
                    id: id.into(),
                    name: format!("Screen {id}"),
                    bounds: Rect {
                        x: i as i32 * 1200,
                        ..area
                    },
                    work_area: Rect {
                        x: i as i32 * 1200,
                        ..area
                    },
                    scale_factor: 1.0,
                    primary: i == 0,
                })
                .collect(),
            windows: windows
                .iter()
                .map(|id| NativeWindow {
                    id: (*id).into(),
                    title: (*id).into(),
                    app_name: "test".into(),
                    process_id: 1,
                    monitor_id: "a".into(),
                    rect: area,
                    minimized: false,
                    minimized_by_manager: false,
                    resizable: true,
                })
                .collect(),
            focused_window: windows.first().map(|id| (*id).into()),
        }
    }

    fn column_width(e: &Engine, id: &str) -> u32 {
        let (m, p, column) = e.location(id).unwrap();
        e.snapshot.monitors[m].pages[p].columns[column.unwrap().0].width
    }

    fn page_names(e: &Engine, m: usize) -> Vec<(String, bool)> {
        e.snapshot.monitors[m]
            .pages
            .iter()
            .map(|p| (p.name.clone(), e.snapshot.named_pages.contains(&p.id)))
            .collect()
    }

    #[test]
    fn config_defaults_round_trip_and_rejects_invalid_values() {
        let config = Config::parse(b"{}").unwrap();
        assert_eq!(config.preset_column_widths, default_preset_sizes());
        assert_eq!(config.preset_window_heights, default_preset_sizes());
        assert_eq!(config.default_column_width, PresetSize::Proportion(0.5));
        assert_eq!(config.center_focused_column, CenterFocusedColumn::Never);
        assert!(!config.always_center_single_column);
        assert_eq!(config.struts, Struts::default());
        assert!(config.workspaces.is_empty());
        let config = Config::parse(
            br#"{"presetColumnWidths":[{"fixed":640},{"proportion":1}],
                "defaultColumnWidth":{"fixed":800},
                "presetWindowHeights":[{"proportion":0.25}],
                "centerFocusedColumn":"onOverflow","alwaysCenterSingleColumn":true,
                "struts":{"left":10,"top":4},
                "workspaces":[{"name":"chat","monitor":"b"},{"name":"web"}]}"#,
        )
        .unwrap();
        assert_eq!(
            config.preset_column_widths,
            [PresetSize::Fixed(640), PresetSize::Proportion(1.0)]
        );
        assert_eq!(
            config.center_focused_column,
            CenterFocusedColumn::OnOverflow
        );
        assert_eq!(
            config.struts,
            Struts {
                left: 10,
                top: 4,
                ..Struts::default()
            }
        );
        assert_eq!(config.workspaces[0].monitor.as_deref(), Some("b"));
        assert_eq!(
            Config::parse(&serde_json::to_vec(&config).unwrap()).unwrap(),
            config
        );
        for text in [
            r#"{"presetColumnWidths":[]}"#,
            r#"{"presetColumnWidths":[{"proportion":0}]}"#,
            r#"{"presetColumnWidths":[{"proportion":1.5}]}"#,
            r#"{"presetColumnWidths":[{"proportion":-0.5}]}"#,
            r#"{"presetColumnWidths":[{"fixed":0}]}"#,
            r#"{"presetColumnWidths":[{"fixed":-5}]}"#,
            r#"{"presetColumnWidths":[{"proportion":0.5,"fixed":5}]}"#,
            r#"{"presetColumnWidths":[{"percent":50}]}"#,
            r#"{"presetWindowHeights":[{"proportion":2}]}"#,
            r#"{"presetWindowHeights":[]}"#,
            r#"{"defaultColumnWidth":{"proportion":0}}"#,
            r#"{"defaultColumnWidth":{"fixed":0}}"#,
            r#"{"centerFocusedColumn":"sometimes"}"#,
            r#"{"alwaysCenterSingleColumn":1}"#,
            r#"{"struts":{"left":-1}}"#,
            r#"{"struts":{"middle":1}}"#,
            r#"{"workspaces":[{"name":" "}]}"#,
            r#"{"workspaces":[{"name":"a","monitor":""}]}"#,
            r#"{"workspaces":[{"name":"Chat"},{"name":"chat"}]}"#,
            r#"{"workspaces":[{"name":"a","output":"b"}]}"#,
            r#"{"shortcuts":[{"key":"A","action":{"type":"pageByName","name":" "}}]}"#,
            r#"{"shortcuts":[{"key":"A","action":{"type":"command","command":{"type":"setPageName","name":""}}}]}"#,
        ] {
            assert!(Config::parse(text.as_bytes()).is_err(), "accepted: {text}");
        }
    }

    #[test]
    fn preset_pixels_cycle_and_struts() {
        assert_eq!(PresetSize::Proportion(1.0 / 3.0).pixels(1000, 1.0, 0), 333);
        assert_eq!(PresetSize::Proportion(2.0 / 3.0).pixels(1200, 1.0, 0), 800);
        assert_eq!(PresetSize::Fixed(400).pixels(2000, 1.5, 16), 616);
        assert_eq!(PresetSize::Fixed(4000).pixels(1000, 1.0, 0), 1000);
        let presets = [400, 600, 800];
        assert_eq!(cycle_index(&presets, 600, true), 2);
        assert_eq!(cycle_index(&presets, 800, true), 0);
        assert_eq!(cycle_index(&presets, 601, true), 2);
        assert_eq!(cycle_index(&presets, 500, true), 1);
        assert_eq!(cycle_index(&presets, 900, true), 0);
        assert_eq!(cycle_index(&presets, 400, false), 2);
        assert_eq!(cycle_index(&presets, 500, false), 0);
        assert_eq!(cycle_index(&presets, 300, false), 2);
        let rect = Rect {
            x: -100,
            y: 36,
            width: 1000,
            height: 700,
        };
        let struts = Struts {
            left: 10,
            right: 20,
            top: 5,
            bottom: 0,
        };
        assert_eq!(
            apply_struts(rect, &struts, 2.0),
            Rect {
                x: -80,
                y: 46,
                width: 940,
                height: 690,
            }
        );
        let huge = Struts {
            left: u32::MAX,
            right: u32::MAX,
            top: u32::MAX,
            bottom: 9,
        };
        let squeezed = apply_struts(rect, &huge, 1.0);
        assert_eq!((squeezed.width, squeezed.height), (1, 1));
        assert_eq!(apply_struts(rect, &Struts::default(), 1.5), rect);
    }

    #[test]
    fn centering_modes_follow_niri() {
        let widths = [600, 600, 600, 600];
        let never = LayoutOptions::default();
        // Never: minimal scroll to bring column 2 in from the left edge.
        assert_eq!(
            view_target(&widths, 1000, 0, 2, false, Some(1), &never),
            800
        );
        let always = LayoutOptions {
            center_focused_column: CenterFocusedColumn::Always,
            ..LayoutOptions::default()
        };
        assert_eq!(view_target(&widths, 1000, 0, 2, false, None, &always), 1000);
        // Columns at least as wide as the view are aligned, never centered.
        assert_eq!(
            view_target(&[1200, 1200], 1000, 0, 1, false, None, &always),
            1200
        );
        let overflow = LayoutOptions {
            center_focused_column: CenterFocusedColumn::OnOverflow,
            ..LayoutOptions::default()
        };
        // 600 + 600 > 1000: the target does not fit with the source neighbour, so center.
        assert_eq!(
            view_target(&widths, 1000, 0, 2, false, Some(1), &overflow),
            1000
        );
        // Without a previous column it only fits.
        assert_eq!(
            view_target(&widths, 1000, 0, 2, false, None, &overflow),
            800
        );
        let narrow = [300, 300, 300, 300, 300];
        assert_eq!(
            view_target(&narrow, 1000, 0, 3, false, Some(2), &overflow),
            200
        );
        assert_eq!(
            view_target(&narrow, 1000, 600, 1, false, Some(2), &overflow),
            300
        );
        let single = LayoutOptions {
            always_center_single_column: true,
            ..LayoutOptions::default()
        };
        assert_eq!(view_target(&[400], 1000, 0, 0, false, None, &single), -300);
        assert_eq!(
            view_target(&[400, 400], 1000, 0, 0, false, None, &single),
            0
        );
    }

    #[test]
    fn preset_widths_default_width_and_cycle_back_drive_columns() {
        let mut e = engine(
            r#"{"presetColumnWidths":[{"fixed":300},{"proportion":0.75}],"defaultColumnWidth":{"proportion":0.25}}"#,
            &["1", "2"],
        );
        assert_eq!(column_width(&e, "1"), 300);
        e.dispatch(Command::CycleWidth).unwrap();
        assert_eq!(column_width(&e, "1"), 900);
        e.dispatch(Command::CycleWidth).unwrap();
        assert_eq!(column_width(&e, "1"), 300);
        e.dispatch(Command::CycleWidthBack).unwrap();
        assert_eq!(column_width(&e, "1"), 900);
        // A rule width still wins over the default width.
        e.set_window_rules(vec![
            serde_json::from_str(r#"{"title":"3","columnWidth":555}"#).unwrap(),
        ])
        .unwrap();
        let mut native = system(&["1", "2", "3", "4"]);
        native.focused_window = Some("1".into());
        e.reconcile(native).unwrap();
        assert_eq!(column_width(&e, "3"), 555);
        assert_eq!(column_width(&e, "4"), 300);
    }

    #[test]
    fn maximize_column_toggles_full_width_and_restores() {
        let mut e = engine("{}", &["1", "2"]);
        assert_eq!(column_width(&e, "1"), 600);
        e.dispatch(Command::SetColumnWidth { width: 450 }).unwrap();
        let t = e.dispatch(Command::MaximizeColumn).unwrap();
        assert_eq!(column_width(&e, "1"), 1200);
        assert!(!e.snapshot.windows[e.window_index("1").unwrap()].fullscreen);
        assert!(t.actions.iter().any(|a| matches!(a,
            NativeAction::Placement { window_id, rect, minimized: false, .. }
                if window_id == "1" && rect.width == 1200)));
        e.dispatch(Command::MaximizeColumn).unwrap();
        assert_eq!(column_width(&e, "1"), 450);
        // Already full width by hand: the toggle falls back to the default width.
        e.dispatch(Command::SetColumnWidth { width: 1200 }).unwrap();
        e.dispatch(Command::MaximizeColumn).unwrap();
        assert_eq!(column_width(&e, "1"), 600);
        e.dispatch(Command::ToggleFloating).unwrap();
        assert!(e.dispatch(Command::MaximizeColumn).is_err());
    }

    #[test]
    fn cycle_window_height_sets_preset_and_shares_the_rest() {
        let mut e = engine("{}", &["1", "2", "3"]);
        let page = e.snapshot.monitors[0].active_page.clone();
        for id in ["2", "3"] {
            e.dispatch(Command::FocusWindow {
                window_id: id.into(),
            })
            .unwrap();
            e.dispatch(Command::MoveWindow {
                direction: Direction::Left,
            })
            .unwrap();
        }
        let heights = |e: &Engine| {
            let column = &e.snapshot.monitors[0].pages[0].columns[0];
            assert_eq!(column.windows, ["1", "2", "3"]);
            e.column_heights(column, 900)
        };
        assert_eq!(e.snapshot.monitors[0].active_page, page);
        assert_eq!(heights(&e), [300, 300, 300]);
        // Focus is on "3": the exact 1/3 match steps to 1/2.
        e.dispatch(Command::CycleWindowHeight).unwrap();
        assert_eq!(heights(&e), [225, 225, 450]);
        e.dispatch(Command::CycleWindowHeight).unwrap();
        assert_eq!(heights(&e), [150, 150, 600]);
        e.dispatch(Command::CycleWindowHeight).unwrap();
        assert_eq!(heights(&e), [300, 300, 300]);
        // A lone window keeps the whole column.
        let mut lone = engine("{}", &["1"]);
        lone.dispatch(Command::CycleWindowHeight).unwrap();
        assert!(lone.height_weights.is_empty());
    }

    #[test]
    fn declared_workspaces_persist_and_names_can_be_set_and_unset() {
        let mut e = engine(
            r#"{"workspaces":[{"name":"chat","monitor":"Screen B"},{"name":"web"},{"name":"mail","monitor":"gone"}]}"#,
            &["1"],
        );
        assert_eq!(
            page_names(&e, 0),
            [
                ("web".into(), true),
                ("mail".into(), true),
                ("Desktop 3".into(), false),
                ("Desktop 4".into(), false)
            ]
        );
        assert_eq!(
            page_names(&e, 1),
            [("chat".into(), true), ("Desktop 2".into(), false)]
        );
        // Windows present at startup stay on the active dynamic page.
        assert_eq!(e.location("1").unwrap().1, 2);
        // Empty named pages survive cleanup; refreshes do not recreate or duplicate them.
        e.reconcile(system(&["1"])).unwrap();
        assert_eq!(e.snapshot.monitors[0].pages.len(), 4);
        // Resolution: switch to a named page, or move the focused window there.
        let snapshot = e.snapshot().clone();
        let chat = snapshot.monitors[1].pages[0].id.clone();
        let action = ShortcutAction::PageByName {
            name: "CHAT".into(),
            move_window: false,
        };
        assert_eq!(
            action.resolve(&snapshot),
            Some(ShortcutAction::Command {
                command: Command::SwitchPage {
                    monitor_id: "b".into(),
                    page_id: chat.clone(),
                }
            })
        );
        let moved = ShortcutAction::PageByName {
            name: "chat".into(),
            move_window: true,
        }
        .resolve(&snapshot);
        let Some(ShortcutAction::Command { command }) = moved else {
            panic!("pageByName move did not resolve");
        };
        e.dispatch(command).unwrap();
        assert_eq!(e.location("1").unwrap().0, 1);
        assert!(
            ShortcutAction::PageByName {
                name: "nope".into(),
                move_window: false
            }
            .resolve(&snapshot)
            .is_none()
        );
        // SetPageName names the active page and takes the name from any other page.
        e.dispatch(Command::SetPageName {
            name: " Web ".into(),
        })
        .unwrap();
        assert_eq!(page_names(&e, 1)[0], ("Web".into(), true));
        // Unnamed, empty and inactive, the former "web" page on monitor a is discarded.
        assert_eq!(
            page_names(&e, 0),
            [("mail".into(), true), ("Desktop 2".into(), false)]
        );
        assert!(
            e.dispatch(Command::SetPageName { name: " ".into() })
                .is_err()
        );
        e.dispatch(Command::UnsetPageName).unwrap();
        assert_eq!(page_names(&e, 1)[0], ("Desktop 1".into(), false));
    }

    #[test]
    fn window_rule_page_name_routes_new_windows_to_named_pages() {
        let mut e = engine(r#"{"workspaces":[{"name":"chat","monitor":"b"}]}"#, &["1"]);
        e.set_window_rules(vec![
            serde_json::from_str(r#"{"title":"new","monitorId":"a","pageName":"Chat"}"#).unwrap(),
            serde_json::from_str(r#"{"title":"other","pageName":"missing"}"#).unwrap(),
        ])
        .unwrap();
        e.reconcile(system(&["1", "new", "other"])).unwrap();
        let (m, p, _) = e.location("new").unwrap();
        assert_eq!(e.snapshot.monitors[m].pages[p].name, "chat");
        assert_eq!(e.snapshot.focused_window.as_deref(), Some("1"));
        // A missing name falls back like a missing page index.
        let (m, p, _) = e.location("other").unwrap();
        assert_eq!(e.location("1").unwrap().0, m);
        assert_eq!(e.location("1").unwrap().1, p);
    }

    #[test]
    fn focus_changes_center_columns_per_option() {
        let mut e = engine(r#"{"centerFocusedColumn":"always"}"#, &["1", "2", "3"]);
        e.dispatch(Command::FocusWindow {
            window_id: "2".into(),
        })
        .unwrap();
        let x = |e: &Engine| e.snapshot.monitors[0].pages[0].viewport_x;
        // New windows open right of the focused one: the strip is 1, 3, 2 here.
        let (_, _, column) = e.location("2").unwrap();
        let left = column.unwrap().0 as i32 * 600;
        assert_eq!(x(&e), left - 300);
        let mut single = engine(r#"{"alwaysCenterSingleColumn":true}"#, &["1"]);
        single
            .dispatch(Command::FocusWindow {
                window_id: "1".into(),
            })
            .unwrap();
        assert_eq!(x(&single), -300);
    }
}
