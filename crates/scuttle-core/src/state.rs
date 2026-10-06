//! scuttle's own local state, such as whether a one-time tip was shown. It is not
//! configuration: the user never edits it, and scuttle writes it as it goes.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::{ConfigError, Existing, write_config};

/// What scuttle remembers between runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LocalState {
    /// Whether the welcome screen already suggested a Nerd Font.
    pub nerd_font_tip_shown: bool,
}

/// `$XDG_STATE_HOME/scuttle/state.toml`, else `$HOME/.local/state/scuttle/state.toml`. A
/// relative or empty `XDG_STATE_HOME` is ignored, as the XDG Base Directory spec requires.
pub fn state_path(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let base = env("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".local").join("state")))?;
    Some(base.join("scuttle").join("state.toml"))
}

/// The state at `path`. A missing, unreadable, or damaged file reads as nothing remembered,
/// since forgetting a tip costs only one more showing.
pub fn load(path: &Path) -> LocalState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

/// Remembers that the Nerd Font tip was shown, keeping any other keys and comments the file
/// holds. A damaged file is replaced, losing whatever else it held. The write is atomic and
/// readable only by its owner.
pub fn record_nerd_font_tip(path: &Path) -> Result<(), ConfigError> {
    let mut doc: toml_edit::DocumentMut = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or_default();
    doc["nerd_font_tip_shown"] = toml_edit::value(true);
    write_config(path, &doc.to_string(), Existing::Replace)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_state() -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("scuttle-state-{}", uuid::Uuid::new_v4()));
        let path = dir.join("scuttle/state.toml");
        (dir, path)
    }

    #[test]
    fn the_state_path_prefers_xdg_then_home() {
        let xdg = |k: &str| match k {
            "XDG_STATE_HOME" => Some("/x".to_string()),
            "HOME" => Some("/h".to_string()),
            _ => None,
        };
        assert_eq!(
            state_path(&xdg),
            Some(PathBuf::from("/x/scuttle/state.toml"))
        );
        let home = |k: &str| (k == "HOME").then(|| "/h".to_string());
        assert_eq!(
            state_path(&home),
            Some(PathBuf::from("/h/.local/state/scuttle/state.toml"))
        );
        assert_eq!(state_path(&|_: &str| None), None);
    }

    #[test]
    fn a_relative_xdg_state_home_is_ignored() {
        for value in ["state", ""] {
            let env = |k: &str| match k {
                "XDG_STATE_HOME" => Some(value.to_string()),
                "HOME" => Some("/h".to_string()),
                _ => None,
            };
            assert_eq!(
                state_path(&env),
                Some(PathBuf::from("/h/.local/state/scuttle/state.toml")),
                "XDG_STATE_HOME={value:?}"
            );
        }
    }

    #[test]
    fn recording_the_tip_keeps_other_keys_and_is_owner_only() {
        let (dir, path) = temp_state();
        assert!(!load(&path).nerd_font_tip_shown, "nothing remembered yet");
        record_nerd_font_tip(&path).unwrap();
        assert!(load(&path).nerd_font_tip_shown);
        std::fs::write(&path, "# kept\nlater_key = 1\n").unwrap();
        record_nerd_font_tip(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# kept"), "{text}");
        assert!(text.contains("later_key = 1"), "{text}");
        assert!(load(&path).nerd_font_tip_shown, "{text}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_or_damaged_state_file_remembers_nothing_and_a_record_replaces_it() {
        let (dir, path) = temp_state();
        assert_eq!(load(&path), LocalState::default(), "missing");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "nerd_font_tip_shown = [not toml").unwrap();
        assert_eq!(load(&path), LocalState::default(), "damaged");
        std::fs::write(&path, "nerd_font_tip_shown = \"yes\"\n").unwrap();
        assert_eq!(load(&path), LocalState::default(), "the wrong type");
        record_nerd_font_tip(&path).unwrap();
        assert!(
            load(&path).nerd_font_tip_shown,
            "a record replaces a damaged file"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
