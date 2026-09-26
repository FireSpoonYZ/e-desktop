use std::collections::BTreeSet;

use crate::config::{Config, ShortcutAction};

/// Callbacks carry only canonical keys; actions are read after a mapping is committed.
pub struct Shortcuts {
    pub config: Config,
    registered: BTreeSet<String>,
}

impl Shortcuts {
    pub fn new(defaults: Config) -> Self {
        Self {
            config: defaults,
            registered: BTreeSet::new(),
        }
    }

    pub fn available(&self) -> bool {
        !self.config.shortcuts.is_empty() && self.keys() == self.registered
    }

    pub fn action(&self, key: &str) -> Option<&ShortcutAction> {
        self.config
            .shortcuts
            .iter()
            .find(|binding| binding.key == key)
            .map(|binding| &binding.action)
    }

    fn keys(&self) -> BTreeSet<String> {
        self.config
            .shortcuts
            .iter()
            .map(|binding| binding.key.clone())
            .collect()
    }

    /// Add first, retain common registrations, then remove obsolete ones. On failure,
    /// restore the old set best-effort and report every rollback failure as well.
    pub fn replace(
        &mut self,
        candidate: Config,
        mut register: impl FnMut(&str, bool) -> Result<(), String>,
    ) -> Result<(), String> {
        let wanted: BTreeSet<_> = candidate
            .shortcuts
            .iter()
            .map(|binding| binding.key.clone())
            .collect();
        let additions: Vec<_> = wanted.difference(&self.registered).cloned().collect();
        let removals: Vec<_> = self.registered.difference(&wanted).cloned().collect();
        let mut failure = None;
        for (keys, enabled) in [(additions, true), (removals, false)] {
            for key in keys {
                match register(&key, enabled) {
                    Ok(()) => {
                        if enabled {
                            self.registered.insert(key);
                        } else {
                            self.registered.remove(&key);
                        }
                    }
                    Err(e) => {
                        failure = Some(e);
                        break;
                    }
                }
            }
            if failure.is_some() {
                break;
            }
        }
        if let Some(mut issue) = failure {
            let old = self.keys();
            let restore: Vec<_> = old.difference(&self.registered).cloned().collect();
            let discard: Vec<_> = self.registered.difference(&old).cloned().collect();
            for (keys, enabled) in [(restore, true), (discard, false)] {
                for key in keys {
                    match register(&key, enabled) {
                        Ok(()) => {
                            if enabled {
                                self.registered.insert(key);
                            } else {
                                self.registered.remove(&key);
                            }
                        }
                        Err(e) => {
                            issue.push_str(&format!("；回滚失败：{e}"));
                        }
                    }
                }
            }
            return Err(format!("{issue}；保留上次有效快捷键配置。"));
        }
        self.config = candidate;
        Ok(())
    }
}

#[cfg(feature = "desktop")]
pub fn normalize_key(key: &str) -> Result<String, String> {
    use tauri_plugin_global_shortcut::{Modifiers, Shortcut};
    let invalid = |e: String| format!("快捷键 {key:?} 无效：{e}");
    let shortcut: Shortcut = key.parse().map_err(|e| invalid(format!("{e}")))?;
    // The plugin accepts repeated modifiers; reject these accidental combinations.
    let tokens: Vec<_> = key.split('+').collect();
    let mut modifiers = Modifiers::empty();
    for token in &tokens[..tokens.len() - 1] {
        let part: Shortcut = format!("{}+A", token.trim())
            .parse()
            .map_err(|e| invalid(format!("{e}")))?;
        if modifiers.intersects(part.mods) {
            return Err(invalid("修饰键重复".into()));
        }
        modifiers |= part.mods;
    }
    Ok(shortcut.into_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ShortcutBinding;

    fn config(keys: &[&str]) -> Config {
        Config {
            shortcuts: keys
                .iter()
                .map(|key| ShortcutBinding {
                    key: (*key).into(),
                    action: ShortcutAction::Overview {},
                })
                .collect(),
            ..Config::default()
        }
    }

    #[test]
    fn replacement_conflict_and_rollback_preserve_old_mapping() {
        let mut shortcuts = Shortcuts::new(config(&["A", "B"]));
        let mut actual = BTreeSet::new();
        shortcuts
            .replace(config(&["A", "B"]), |key, on| {
                assert!(on);
                actual.insert(key.to_owned());
                Ok(())
            })
            .unwrap();
        let old = actual.clone();
        let result = shortcuts.replace(config(&["C", "D"]), |key, on| {
            if key == "D" {
                return Err("D conflict".into());
            }
            if on {
                actual.insert(key.to_owned());
            } else {
                actual.remove(key);
            }
            Ok(())
        });
        assert!(result.unwrap_err().contains("D conflict"));
        assert_eq!(actual, old);
        assert_eq!(shortcuts.config, config(&["A", "B"]));
        assert!(shortcuts.available());
        let result = shortcuts.replace(config(&["C"]), |key, on| {
            if key == "B" && !on {
                return Err("B unregister failed".into());
            }
            if on {
                actual.insert(key.to_owned());
            } else {
                actual.remove(key);
            }
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(actual, old);
        assert!(shortcuts.available());
        let mut changed_action = config(&["A", "B"]);
        changed_action.shortcuts[0].action = ShortcutAction::Quit {};
        shortcuts
            .replace(changed_action, |_, _| {
                panic!("common keys must not re-register")
            })
            .unwrap();
        assert_eq!(shortcuts.action("A"), Some(&ShortcutAction::Quit {}));
        shortcuts
            .replace(config(&[]), |key, on| {
                assert!(!on);
                actual.remove(key);
                Ok(())
            })
            .unwrap();
        assert!(actual.is_empty());
        assert!(!shortcuts.available());
    }

    #[test]
    fn rollback_failures_are_reported_and_uncommitted_keys_are_inert() {
        let mut shortcuts = Shortcuts::new(config(&["A", "B"]));
        shortcuts
            .replace(config(&["A", "B"]), |_, _| Ok(()))
            .unwrap();
        let result = shortcuts
            .replace(config(&["C"]), |key, on| {
                if (key == "A" && on) || ((key == "B" || key == "C") && !on) {
                    Err(format!("{key} failed"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert!(result.contains("回滚失败"));
        assert!(!shortcuts.available());
        assert_eq!(shortcuts.action("C"), None);
        assert_eq!(shortcuts.config, config(&["A", "B"]));
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn native_key_validation_and_alias_duplicates() {
        for key in [
            "",
            "Control",
            "Control++A",
            "Control+A+B",
            "A+Control",
            "Control+Ctrl+A",
            "Control+NotAKey",
        ] {
            assert!(normalize_key(key).is_err(), "{key}");
        }
        assert_eq!(
            normalize_key("ctrl+alt+a").unwrap(),
            normalize_key("Alt+Control+KeyA").unwrap()
        );
        assert!(
            config(&["Ctrl+A", "Control+KeyA"])
                .normalize_keys(normalize_key)
                .is_err()
        );
        let defaults = Config::default().normalize_keys(normalize_key).unwrap();
        assert_eq!(defaults.shortcuts.len(), 46);
    }
}
