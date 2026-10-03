use serde::{Deserialize, Serialize};

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
        } else if self.floating.is_none()
            && self.column_width.is_none()
            && self.monitor_id.is_none()
            && self.page_index.is_none()
            && self.page_name.is_none()
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
        contains(&window.app_name, &self.app_name) && contains(&window.title, &self.title)
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
