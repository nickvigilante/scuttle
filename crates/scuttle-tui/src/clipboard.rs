//! Copying text: the native clipboard locally, OSC 52 over SSH, and tmux passthrough.

use std::io::Write;

use base64::Engine;

pub const OSC52_MAX_BYTES: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyOutcome {
    Copied,
    /// Copied, but the terminal may not have delivered it (tmux without `allow-passthrough`).
    CopiedWithWarning(String),
    Failed(String),
}

/// The OSC 52 escape sequence for `text`, wrapped in a tmux DCS passthrough when `in_tmux` is set.
pub fn osc52_sequence(text: &str, in_tmux: bool) -> Option<String> {
    if text.len() > OSC52_MAX_BYTES {
        return None;
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(text);
    let seq = format!("\x1b]52;c;{b64}\x07");
    Some(if in_tmux {
        format!("\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b"))
    } else {
        seq
    })
}

/// Whether an OSC 52 write inside tmux should use the DCS passthrough form (`true`) or the plain
/// form (`false`, when tmux's `set-clipboard` is `"on"` and will forward it itself).
fn tmux_sequence_choice(set_clipboard_on: bool) -> bool {
    !set_clipboard_on
}

/// Queries tmux's global `set-clipboard` option. Treats a failure to run tmux as not `"on"`.
fn query_tmux_set_clipboard_on() -> bool {
    std::process::Command::new("tmux")
        .args(["show", "-gv", "set-clipboard"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "on")
        .unwrap_or(false)
}

pub struct Clipboard {
    native: Option<arboard::Clipboard>,
    in_tmux: bool,
    over_ssh: bool,
    /// Cached result of the tmux `set-clipboard` query, read lazily on the first OSC 52 copy.
    tmux_set_clipboard_on: Option<bool>,
    /// Set once the passthrough form has warned that it may not reach the clipboard.
    warned_tmux: bool,
    writer: Box<dyn Write>,
}

impl Clipboard {
    pub fn new() -> Clipboard {
        let over_ssh =
            std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
        Clipboard {
            // X11 and some Wayland compositors drop the clipboard when the owner exits, so keep this alive.
            native: if over_ssh {
                None
            } else {
                arboard::Clipboard::new().ok()
            },
            in_tmux: std::env::var_os("TMUX").is_some(),
            over_ssh,
            tmux_set_clipboard_on: None,
            warned_tmux: false,
            writer: Box::new(std::io::stdout()),
        }
    }

    fn tmux_clipboard_on(&mut self) -> bool {
        if let Some(on) = self.tmux_set_clipboard_on {
            return on;
        }
        let on = query_tmux_set_clipboard_on();
        self.tmux_set_clipboard_on = Some(on);
        on
    }

    fn osc52(&mut self, text: &str) -> CopyOutcome {
        let in_tmux_wrap = if self.in_tmux {
            tmux_sequence_choice(self.tmux_clipboard_on())
        } else {
            false
        };
        let Some(seq) = osc52_sequence(text, in_tmux_wrap) else {
            return CopyOutcome::Failed(format!(
                "too large to copy over the terminal ({} bytes, limit {OSC52_MAX_BYTES})",
                text.len()
            ));
        };
        if self
            .writer
            .write_all(seq.as_bytes())
            .and_then(|_| self.writer.flush())
            .is_err()
        {
            return CopyOutcome::Failed("could not write to the terminal".into());
        }
        if in_tmux_wrap && !self.warned_tmux {
            self.warned_tmux = true;
            return CopyOutcome::CopiedWithWarning(
                "tmux set-clipboard is not on, so this reaches your clipboard only if allow-passthrough is on".into(),
            );
        }
        CopyOutcome::Copied
    }

    pub fn copy(&mut self, text: &str) -> CopyOutcome {
        if !self.over_ssh
            && let Some(native) = self.native.as_mut()
            && native.set_text(text.to_owned()).is_ok()
        {
            return CopyOutcome::Copied;
        }
        self.osc52(text)
    }
}

impl Default for Clipboard {
    fn default() -> Clipboard {
        Clipboard::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_osc52_and_tmux_passthrough() {
        assert_eq!(
            osc52_sequence("hi", false).as_deref(),
            Some("\x1b]52;c;aGk=\x07")
        );
        assert_eq!(
            osc52_sequence("hi", true).as_deref(),
            Some("\x1bPtmux;\x1b\x1b]52;c;aGk=\x07\x1b\\")
        );
    }

    #[test]
    fn refuses_payloads_over_the_limit() {
        let big = "x".repeat(OSC52_MAX_BYTES + 1);
        assert!(osc52_sequence(&big, false).is_none());
        assert!(osc52_sequence(&"x".repeat(OSC52_MAX_BYTES), false).is_some());
    }

    #[test]
    fn tmux_sequence_choice_on_uses_plain_form() {
        assert!(!tmux_sequence_choice(true));
    }

    #[test]
    fn tmux_sequence_choice_anything_else_uses_passthrough() {
        assert!(tmux_sequence_choice(false));
    }

    fn test_clipboard(set_clipboard_on: bool) -> Clipboard {
        Clipboard {
            native: None,
            in_tmux: true,
            over_ssh: false,
            tmux_set_clipboard_on: Some(set_clipboard_on),
            warned_tmux: false,
            writer: Box::new(Vec::new()),
        }
    }

    #[test]
    fn tmux_set_clipboard_on_copies_plain_without_warning() {
        let mut clipboard = test_clipboard(true);
        assert_eq!(clipboard.copy("hi"), CopyOutcome::Copied);
        assert_eq!(clipboard.copy("hi"), CopyOutcome::Copied);
    }

    #[test]
    fn tmux_set_clipboard_off_warns_once_then_copies() {
        let mut clipboard = test_clipboard(false);
        assert_eq!(
            clipboard.copy("hi"),
            CopyOutcome::CopiedWithWarning(
                "tmux set-clipboard is not on, so this reaches your clipboard only if allow-passthrough is on"
                    .into()
            )
        );
        assert_eq!(clipboard.copy("hi"), CopyOutcome::Copied);
    }

    #[test]
    fn oversized_payload_over_ssh_fails_without_warning() {
        let mut clipboard = test_clipboard(false);
        let big = "x".repeat(OSC52_MAX_BYTES + 1);
        assert_eq!(
            clipboard.copy(&big),
            CopyOutcome::Failed(format!(
                "too large to copy over the terminal ({} bytes, limit {OSC52_MAX_BYTES})",
                big.len()
            ))
        );
    }
}
