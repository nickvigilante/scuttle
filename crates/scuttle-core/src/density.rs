//! How much of each reasoning or tool block to show, following the web UI's display preferences.

use std::collections::BTreeMap;

use coder_sdk::types;
use serde::Deserialize;

/// How a block renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    Expanded,
    Summary,
    Hidden,
}

/// Which key sends a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendShortcut {
    Enter,
    ModifierEnter,
}

/// The server-stored display preferences shared with the web UI.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayPrefs {
    pub thinking: String,
    pub shell: String,
    pub diff: String,
    pub collapse_steps: bool,
    pub send_shortcut: SendShortcut,
}

impl Default for DisplayPrefs {
    fn default() -> Self {
        DisplayPrefs {
            thinking: "auto".into(),
            shell: "always_collapsed".into(),
            diff: "auto".into(),
            collapse_steps: false,
            send_shortcut: SendShortcut::Enter,
        }
    }
}

impl From<&types::CodersdkUserPreferenceSettings> for DisplayPrefs {
    fn from(s: &types::CodersdkUserPreferenceSettings) -> Self {
        let d = DisplayPrefs::default();
        DisplayPrefs {
            thinking: s
                .thinking_display_mode
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or(d.thinking),
            shell: s
                .shell_tool_display_mode
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or(d.shell),
            diff: s
                .code_diff_display_mode
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or(d.diff),
            collapse_steps: s.collapse_assistant_steps.unwrap_or(false),
            send_shortcut: match s.agent_chat_send_shortcut.as_ref().map(|v| v.as_str()) {
                Some("modifier_enter") => SendShortcut::ModifierEnter,
                _ => SendShortcut::Enter,
            },
        }
    }
}

/// The kind of block being rendered.
#[derive(Debug, Clone, Copy)]
pub enum BlockKind<'a> {
    Reasoning,
    Tool(&'a str),
}

const SHELL_TOOLS: &[&str] = &["execute", "process_output"];
const DIFF_TOOLS: &[&str] = &["write_file", "edit_files"];

fn from_mode(mode: &str) -> Density {
    match mode {
        "always_expanded" => Density::Expanded,
        _ => Density::Summary,
    }
}

/// The density for a block, before any per-block toggle the user applied this session.
pub fn density_for(
    kind: BlockKind,
    prefs: &DisplayPrefs,
    overrides: &BTreeMap<String, Density>,
) -> Density {
    match kind {
        BlockKind::Reasoning => from_mode(&prefs.thinking),
        BlockKind::Tool(name) if SHELL_TOOLS.contains(&name) => from_mode(&prefs.shell),
        BlockKind::Tool(name) if DIFF_TOOLS.contains(&name) => from_mode(&prefs.diff),
        BlockKind::Tool(name) => overrides.get(name).copied().unwrap_or(Density::Summary),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_defaults_collapse_shell_and_summarize_everything() {
        let prefs = DisplayPrefs::default();
        let none = BTreeMap::new();
        assert_eq!(
            density_for(BlockKind::Tool("execute"), &prefs, &none),
            Density::Summary
        );
        assert_eq!(
            density_for(BlockKind::Tool("edit_files"), &prefs, &none),
            Density::Summary
        );
        assert_eq!(
            density_for(BlockKind::Reasoning, &prefs, &none),
            Density::Summary
        );
    }

    #[test]
    fn server_prefs_expand_shell_and_diff_tools() {
        let prefs = DisplayPrefs {
            shell: "always_expanded".into(),
            diff: "always_expanded".into(),
            ..DisplayPrefs::default()
        };
        let none = BTreeMap::new();
        assert_eq!(
            density_for(BlockKind::Tool("process_output"), &prefs, &none),
            Density::Expanded
        );
        assert_eq!(
            density_for(BlockKind::Tool("write_file"), &prefs, &none),
            Density::Expanded
        );
    }

    #[test]
    fn local_overrides_apply_only_to_tools_the_server_does_not_cover() {
        let prefs = DisplayPrefs::default();
        let mut overrides = BTreeMap::new();
        overrides.insert("read_file".to_string(), Density::Hidden);
        overrides.insert("execute".to_string(), Density::Hidden);
        assert_eq!(
            density_for(BlockKind::Tool("read_file"), &prefs, &overrides),
            Density::Hidden
        );
        assert_eq!(
            density_for(BlockKind::Tool("execute"), &prefs, &overrides),
            Density::Summary
        );
    }

    #[test]
    fn prefs_convert_from_server_settings() {
        let settings: coder_sdk::types::CodersdkUserPreferenceSettings =
            serde_json::from_value(serde_json::json!({
                "thinking_display_mode": "always_expanded",
                "shell_tool_display_mode": "auto",
                "code_diff_display_mode": "always_collapsed",
                "collapse_assistant_steps": true,
                "agent_chat_send_shortcut": "modifier_enter"
            }))
            .unwrap();
        let prefs = DisplayPrefs::from(&settings);
        assert_eq!(prefs.thinking, "always_expanded");
        assert_eq!(prefs.send_shortcut, SendShortcut::ModifierEnter);
        assert!(prefs.collapse_steps);
    }
}
