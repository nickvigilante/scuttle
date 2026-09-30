//! The `/help` overlay: every slash command with its usage, then every key.

use ratatui::text::{Line, Span};
use scuttle_core::commands::COMMANDS;
use unicode_width::UnicodeWidthStr;

use crate::theme::Theme;
use crate::wrap::wrap_lines;

/// One key binding as `/help` shows it.
pub struct KeyInfo {
    pub keys: &'static str,
    pub action: &'static str,
}

/// Every key the TUI handles, in the order `/help` lists them.
pub const KEYS: &[KeyInfo] = &[
    KeyInfo {
        keys: "Enter",
        action: "Send, or add a line, per your Coder send-key preference",
    },
    KeyInfo {
        keys: "Shift+Enter, Alt+Enter, Ctrl+J",
        action: "Add a line (Alt+Enter sends when Ctrl+Enter is the send key and the terminal cannot tell them apart)",
    },
    KeyInfo {
        keys: "Esc",
        action: "Interrupt the agent, or close the slash menu",
    },
    KeyInfo {
        keys: "Ctrl+G",
        action: "Edit the message in $EDITOR",
    },
    KeyInfo {
        keys: "Up, Down",
        action: "Recall sent messages",
    },
    KeyInfo {
        keys: "Tab",
        action: "Complete a slash command",
    },
    KeyInfo {
        keys: "Left, Right",
        action: "Move the /effort slider; Enter saves, Esc cancels",
    },
    KeyInfo {
        keys: "PageUp, PageDown, wheel",
        action: "Scroll the transcript",
    },
    KeyInfo {
        keys: "End",
        action: "Jump to the latest message",
    },
    KeyInfo {
        keys: "Click",
        action: "Open a link, expand a tool call or thinking, or copy a code block",
    },
    KeyInfo {
        keys: "Drag",
        action: "Select text and copy it on release",
    },
    KeyInfo {
        keys: "Ctrl+C twice",
        action: "Quit",
    },
];

/// The help text, wrapped to `width` so a narrow screen scrolls instead of cutting lines off.
pub fn help_lines(theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let usage_width = COMMANDS
        .iter()
        .map(|c| c.display_usage().width())
        .max()
        .unwrap_or(0);
    let keys_width = KEYS.iter().map(|k| k.keys.width()).max().unwrap_or(0);
    let mut lines = vec![Line::from(Span::styled("Commands", theme.accent))];
    lines.extend(COMMANDS.iter().map(|c| {
        Line::from(vec![
            Span::styled(format!("{:<usage_width$}", c.display_usage()), theme.accent),
            Span::raw("  "),
            Span::raw(c.description),
        ])
    }));
    lines.push(Line::default());
    lines.push(Line::from(Span::styled("Keys", theme.accent)));
    lines.extend(KEYS.iter().map(|k| {
        Line::from(vec![
            Span::styled(format!("{:<keys_width$}", k.keys), theme.accent),
            Span::raw("  "),
            Span::raw(k.action),
        ])
    }));
    wrap_lines(&lines, width)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn help_lists_every_command_and_key() {
        let lines = text(&help_lines(&Theme::terminal(true), 200));
        for c in COMMANDS {
            assert!(
                lines
                    .iter()
                    .any(|l| l.contains(&c.display_usage()) && l.contains(c.description)),
                "{} missing from {lines:?}",
                c.usage
            );
        }
        for k in KEYS {
            assert!(
                lines
                    .iter()
                    .any(|l| l.contains(k.keys) && l.contains(k.action)),
                "{} missing",
                k.keys
            );
        }
    }

    #[test]
    fn help_shows_aliases_on_the_command_line() {
        let lines = text(&help_lines(&Theme::terminal(true), 200));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("/quit (/exit)") && l.contains("Exit scuttle"))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("/organization [name] (/org)"))
        );
    }

    #[test]
    fn help_wraps_to_narrow_widths() {
        let lines = help_lines(&Theme::terminal(true), 30);
        assert!(lines.iter().all(|l| l.width() <= 30));
    }
}
