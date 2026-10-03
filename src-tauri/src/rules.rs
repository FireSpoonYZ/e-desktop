use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::model::{AppError, ErrorCode, NativeWindow};

/// Literal, case-insensitive substring conditions; omitted conditions match any window.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WindowRule {
    pub app_name: Option<String>,
    pub title: Option<String>,
    pub floating: Option<bool>,
    pub column_width: Option<u32>,
    pub monitor_id: Option<String>,
    pub page_index: Option<usize>,
    // lane: layout-options
    /// Target named page (case-insensitive); wins over `monitorId` and `pageIndex`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_name: Option<String>,
    // lane: rules-spawn-screenshot
    /// niri-style regex conditions: any entry must match (empty list: no extra condition).
    #[serde(default)]
    pub matches: Vec<WindowMatch>,
    /// A window matching any entry is excluded from this rule.
    #[serde(default)]
    pub excludes: Vec<WindowMatch>,
    /// The column fills the viewport width.
    pub open_maximized: Option<bool>,
    /// Layout fullscreen on open.
    pub open_fullscreen: Option<bool>,
    /// false: the new window does not take focus; true: it does even without native focus.
    pub open_focused: Option<bool>,
    /// Logical pixels; the tiled layout keeps the window inside these limits.
    pub min_width: Option<u32>,
    pub max_width: Option<u32>,
    pub min_height: Option<u32>,
    pub max_height: Option<u32>,
}

/// lane: rules-spawn-screenshot. A regular expression compiled when the configuration is
/// parsed, so an invalid pattern is a configuration error. Unanchored and case-sensitive
/// (use `(?i)`, `^` and `$` in the pattern itself).
#[derive(Clone, Debug)]
pub struct RulePattern(Regex);

impl PartialEq for RulePattern {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_str() == other.0.as_str()
    }
}

impl Eq for RulePattern {}

impl Serialize for RulePattern {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.as_str())
    }
}

impl<'de> Deserialize<'de> for RulePattern {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Regex::new(&text)
            .map(Self)
            .map_err(|e| serde::de::Error::custom(format!("invalid regex {text:?}: {e}")))
    }
}

/// lane: rules-spawn-screenshot. Every given field must match.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WindowMatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_name: Option<RulePattern>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<RulePattern>,
}

impl WindowMatch {
    fn hits(&self, window: &NativeWindow) -> bool {
        let found = |value: &str, pattern: &Option<RulePattern>| {
            pattern.as_ref().is_none_or(|p| p.0.is_match(value))
        };
        found(&window.app_name, &self.app_name) && found(&window.title, &self.title)
    }
}

impl WindowRule {
    pub fn validate(&self) -> Result<(), AppError> {
        let message = if [
            &self.app_name,
            &self.title,
            &self.monitor_id,
            &self.page_name,
        ]
        .into_iter()
        .any(|text| text.as_ref().is_some_and(|text| text.trim().is_empty()))
        {
            Some("Window rule match text, monitorId and pageName must not be blank")
        } else if self.column_width == Some(0) {
            Some("Window rule columnWidth must be greater than zero")
        } else if self.page_index == Some(0) {
            Some("Window rule pageIndex must be 1-based")
        } else if self
            .matches
            .iter()
            .chain(&self.excludes)
            .any(|m| m.app_name.is_none() && m.title.is_none())
        {
            Some("Window rule matches/excludes entries need appName or title")
        } else if [
            self.min_width,
            self.max_width,
            self.min_height,
            self.max_height,
        ]
        .contains(&Some(0))
        {
            Some("Window rule size limits must be greater than zero")
        } else if matches!((self.min_width, self.max_width), (Some(min), Some(max)) if min > max)
            || matches!((self.min_height, self.max_height), (Some(min), Some(max)) if min > max)
        {
            Some("Window rule minimum size must not exceed its maximum")
        } else if self.floating.is_none()
            && self.column_width.is_none()
            && self.monitor_id.is_none()
            && self.page_index.is_none()
            && self.page_name.is_none()
            && self.open_maximized.is_none()
            && self.open_fullscreen.is_none()
            && self.open_focused.is_none()
            && [
                self.min_width,
                self.max_width,
                self.min_height,
                self.max_height,
            ]
            .iter()
            .all(Option::is_none)
        {
            Some("Window rule must have at least one action")
        } else {
            None
        };
        match message {
            Some(message) => Err(AppError {
                code: ErrorCode::InvalidCommand,
                message: message.into(),
                window_id: None,
            }),
            None => Ok(()),
        }
    }

    pub fn matches(&self, window: &NativeWindow) -> bool {
        let contains = |value: &str, condition: &Option<String>| {
            condition
                .as_ref()
                .is_none_or(|text| value.to_lowercase().contains(&text.to_lowercase()))
        };
        contains(&window.app_name, &self.app_name)
            && contains(&window.title, &self.title)
            && (self.matches.is_empty() || self.matches.iter().any(|m| m.hits(window)))
            && !self.excludes.iter().any(|m| m.hits(window))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rect;

    #[test]
    fn window_rule_serde_and_validation() {
        let rule: WindowRule = serde_json::from_str(r#"{"floating":false}"#).unwrap();
        assert_eq!(
            rule,
            WindowRule {
                floating: Some(false),
                ..WindowRule::default()
            }
        );
        rule.validate().unwrap();
        let rule: WindowRule = serde_json::from_str(
            r#"{"appName":"Editor","title":"Draft","columnWidth":320,"monitorId":"b","pageIndex":2}"#,
        ).unwrap();
        rule.validate().unwrap();
        assert_eq!(serde_json::to_value(&rule).unwrap()["columnWidth"], 320);
        assert_eq!(serde_json::to_value(&rule).unwrap()["appName"], "Editor");
        for json in [
            r#"{}"#,
            r#"{"appName":"editor"}"#,
            r#"{"appName":" \t", "floating":true}"#,
            r#"{"title":"\n", "floating":true}"#,
            r#"{"monitorId":"  "}"#,
            r#"{"columnWidth":0}"#,
            r#"{"pageIndex":0}"#,
        ] {
            assert_eq!(
                serde_json::from_str::<WindowRule>(json)
                    .unwrap()
                    .validate()
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidCommand
            );
        }
        for json in [
            r#"{"unknown":true,"floating":true}"#,
            r#"{"app_name":"editor","floating":true}"#,
            r#"{"columnWidth":-1}"#,
            r#"{"pageIndex":1.5}"#,
            r#"{"floating":"true"}"#,
        ] {
            assert!(serde_json::from_str::<WindowRule>(json).is_err(), "{json}");
        }
    }

    #[test]
    fn window_rule_matches_literal_substrings_with_and_conditions() {
        let mut window = NativeWindow {
            id: "1".into(),
            title: "DRAFT [a.*] — École".into(),
            app_name: "My EDITOR".into(),
            process_id: 1,
            monitor_id: "a".into(),
            rect: Rect::default(),
            minimized: false,
            minimized_by_manager: false,
            resizable: true,
        };
        let mut rule = WindowRule {
            app_name: Some("editor".into()),
            title: Some("[a.*] — école".into()),
            floating: Some(true),
            ..WindowRule::default()
        };
        assert!(rule.matches(&window));
        window.app_name = "browser".into();
        assert!(!rule.matches(&window));
        rule.app_name = None;
        assert!(rule.matches(&window));
        window.title = "aZZ — école".into();
        assert!(!rule.matches(&window));
        rule.title = None;
        assert!(rule.matches(&window));
    }
}

// lane: rules-spawn-screenshot
#[cfg(test)]
mod regex_rule_tests {
    use super::*;
    use crate::model::Rect;

    fn window(app_name: &str, title: &str) -> NativeWindow {
        NativeWindow {
            id: "1".into(),
            title: title.into(),
            app_name: app_name.into(),
            process_id: 1,
            monitor_id: "a".into(),
            rect: Rect::default(),
            minimized: false,
            minimized_by_manager: false,
            resizable: true,
        }
    }

    fn rule(json: &str) -> WindowRule {
        let rule: WindowRule = serde_json::from_str(json).unwrap();
        rule.validate().unwrap();
        rule
    }

    #[test]
    fn any_match_and_no_exclude_with_legacy_substrings_still_required() {
        let browser = rule(
            r#"{"matches":[{"appName":"^firefox$"},{"title":"(?i)picture-in-picture"}],
                "excludes":[{"title":"Private"}],"floating":true}"#,
        );
        assert!(browser.matches(&window("firefox", "Mozilla Firefox")));
        assert!(browser.matches(&window("chrome", "Picture-in-Picture")));
        // Unanchored regex search, case-sensitive unless the pattern says otherwise.
        assert!(!browser.matches(&window("Firefox", "x")));
        assert!(!browser.matches(&window("firefox-esr", "x")));
        assert!(!browser.matches(&window("firefox", "Private Browsing")));
        // Fields inside one entry are AND-ed; regex metacharacters are live.
        let editor = rule(
            r#"{"appName":"code","matches":[{"appName":"Co.e","title":"\\.rs$"}],"openFocused":false}"#,
        );
        assert!(editor.matches(&window("Code", "main.rs")));
        assert!(!editor.matches(&window("Code", "main.rst")));
        assert!(
            !editor.matches(&window("Coze", "main.rs")),
            "legacy substring still applies"
        );
        // Only excludes: every other window.
        let others = rule(r#"{"excludes":[{"appName":"^term"}],"maxWidth":900}"#);
        assert!(others.matches(&window("editor", "")));
        assert!(!others.matches(&window("terminal", "")));
    }

    #[test]
    fn regexes_compile_at_parse_time_and_new_fields_are_validated() {
        let error =
            serde_json::from_str::<WindowRule>(r#"{"matches":[{"title":"a("}],"floating":true}"#)
                .unwrap_err()
                .to_string();
        assert!(error.contains("invalid regex"), "{error}");
        for json in [
            r#"{"matches":[{"app_name":"x"}],"floating":true}"#,
            r#"{"matches":{"title":"x"},"floating":true}"#,
            r#"{"minWidth":-1}"#,
            r#"{"openFocused":"no"}"#,
        ] {
            assert!(serde_json::from_str::<WindowRule>(json).is_err(), "{json}");
        }
        for json in [
            r#"{"matches":[{}],"floating":true}"#,
            r#"{"excludes":[{}],"floating":true}"#,
            r#"{"matches":[{"title":"x"}]}"#,
            r#"{"minWidth":0}"#,
            r#"{"maxHeight":0}"#,
            r#"{"minWidth":500,"maxWidth":400}"#,
            r#"{"minHeight":500,"maxHeight":400}"#,
        ] {
            let rule: WindowRule = serde_json::from_str(json).unwrap();
            assert_eq!(
                rule.validate().unwrap_err().code,
                ErrorCode::InvalidCommand,
                "{json}"
            );
        }
        for json in [
            r#"{"openMaximized":true}"#,
            r#"{"openFullscreen":false}"#,
            r#"{"openFocused":false}"#,
            r#"{"minWidth":400,"maxWidth":400}"#,
            r#"{"minHeight":1}"#,
        ] {
            rule(json);
        }
        // Patterns serialize back to their source and compare by it.
        let parsed = rule(r#"{"matches":[{"title":"^a+$"}],"openMaximized":true}"#);
        let value = serde_json::to_value(&parsed).unwrap();
        assert_eq!(value["matches"], serde_json::json!([{"title": "^a+$"}]));
        assert_eq!(serde_json::from_value::<WindowRule>(value).unwrap(), parsed);
        assert_ne!(
            rule(r#"{"matches":[{"title":"^a$"}],"openMaximized":true}"#),
            parsed
        );
    }
}
