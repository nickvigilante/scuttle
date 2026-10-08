//! Desktop notifications: the escape sequence that makes the terminal raise one when a chat's
//! turn ends while scuttle is not focused, or the bell where no sequence is known to work.
//!
//! Only terminals on an allowlist get a notification sequence, since a terminal that does not
//! parse one could print it. Everything else gets the bell, which every terminal accepts.

use std::io::Write;

use scuttle_core::alerts::{ChatAlert, Outcome};
use scuttle_core::config::NotificationMode;

/// The bell, outside any escape sequence.
pub const BEL: &str = "\x07";

/// The most characters a notification's title or body keeps, well under kitty's 2048-byte
/// payload limit.
const TEXT_MAX: usize = 100;

/// The terminals scuttle knows how to send a desktop notification to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalKind {
    /// OSC 777 `notify`, which carries a title.
    Warp,
    /// OSC 9, the only notification sequence iTerm2 parses.
    ITerm2,
    /// OSC 777 `notify`.
    Ghostty,
    /// OSC 99, which kitty shows only while the window is unfocused.
    Kitty,
    /// Any other terminal, which gets the bell.
    Other,
}

/// The terminal scuttle runs in, as the environment describes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub kind: TerminalKind,
    /// Whether scuttle runs inside tmux, which forwards a sequence to the outer terminal only
    /// in its DCS passthrough, and only while `allow-passthrough` is on.
    pub tmux: bool,
}

/// The terminal `env` describes. `TERM_PROGRAM` decides when set, except inside tmux, which
/// sets it to `tmux`; then, or when it is unset, as over SSH, the terminals' own variables
/// decide. Zellij and GNU screen do not forward notification sequences, so inside them the
/// terminal is `Other`.
pub fn detect(env: &dyn Fn(&str) -> Option<String>) -> Target {
    let get = |key: &str| env(key).filter(|v| !v.is_empty());
    let tmux = get("TMUX").is_some();
    let program = get("TERM_PROGRAM");
    let term = get("TERM").unwrap_or_default();
    let kind = if get("ZELLIJ").is_some() || get("STY").is_some() {
        TerminalKind::Other
    } else {
        match program.as_deref() {
            Some("WarpTerminal") => TerminalKind::Warp,
            Some("iTerm.app") => TerminalKind::ITerm2,
            Some("ghostty") => TerminalKind::Ghostty,
            Some(p) if p != "tmux" => TerminalKind::Other,
            _ if get("LC_TERMINAL").as_deref() == Some("iTerm2")
                || get("ITERM_SESSION_ID").is_some() =>
            {
                TerminalKind::ITerm2
            }
            _ if term == "xterm-kitty" || get("KITTY_WINDOW_ID").is_some() => TerminalKind::Kitty,
            _ if term == "xterm-ghostty" || get("GHOSTTY_RESOURCES_DIR").is_some() => {
                TerminalKind::Ghostty
            }
            _ if get("WARP_CLIENT_VERSION").is_some() => TerminalKind::Warp,
            _ => TerminalKind::Other,
        }
    };
    Target { kind, tmux }
}

/// `text` without control characters or `;`, so it can neither end a sequence early nor
/// shift into another parameter, cut to `TEXT_MAX` characters.
pub fn clean(text: &str) -> String {
    let kept: Vec<char> = text
        .chars()
        .filter(|c| !c.is_control() && *c != ';')
        .collect();
    if kept.len() <= TEXT_MAX {
        return kept.into_iter().collect();
    }
    let mut cut: String = kept[..TEXT_MAX - 1].iter().collect();
    cut.push('\u{2026}');
    cut
}

/// The notification's title and body for `alert`. The body names the chat by its title only,
/// never a URL or the session token.
pub fn message(alert: &ChatAlert) -> (String, String) {
    let words = match alert.outcome {
        Outcome::Finished => "finished",
        Outcome::NeedsAnswer => "needs an answer",
        Outcome::Failed => "failed",
    };
    ("scuttle".into(), format!("{} {words}", alert.title))
}

/// The desktop notification sequence for `kind`, or `None` for a terminal that gets the bell.
/// `title` and `body` are cleaned here.
pub fn sequence(kind: TerminalKind, title: &str, body: &str) -> Option<String> {
    let (title, body) = (clean(title), clean(body));
    match kind {
        TerminalKind::Warp | TerminalKind::Ghostty => {
            Some(format!("\x1b]777;notify;{title};{body}\x07"))
        }
        // OSC 9 has no title, and a body starting with digits and `;` would read as a ConEmu
        // command, so the body starts with the title.
        TerminalKind::ITerm2 => Some(format!("\x1b]9;{title}: {body}\x07")),
        // The title chunk says more follows; the body chunk with the same id ends it.
        TerminalKind::Kitty => Some(format!(
            "\x1b]99;i=scuttle:d=0:o=unfocused;{title}\x1b\\\x1b]99;i=scuttle:p=body:o=unfocused;{body}\x1b\\"
        )),
        TerminalKind::Other => None,
    }
}

/// `sequence` in tmux's DCS passthrough, with every `ESC` doubled, as `osc52_sequence` does.
pub fn tmux_wrap(sequence: &str) -> String {
    format!("\x1bPtmux;{}\x1b\\", sequence.replace('\x1b', "\x1b\x1b"))
}

/// Whether an alert goes out at all: `notifications` is not `off`, and scuttle is not known to
/// be focused. `focused` is `None` until the terminal reports focus, and then an alert goes out,
/// since a terminal that never reports it would otherwise get none.
pub fn should_notify(mode: NotificationMode, focused: Option<bool>) -> bool {
    mode != NotificationMode::Off && focused != Some(true)
}

/// Asks tmux whether the pane forwards passthrough sequences: `allow-passthrough` is `on` or
/// `all`, with inherited values. A failure to run tmux counts as off.
fn query_tmux_passthrough() -> bool {
    crate::runtime::child_command("tmux")
        .args(["show-options", "-Apv", "allow-passthrough"])
        .output()
        .map(|o| matches!(String::from_utf8_lossy(&o.stdout).trim(), "on" | "all"))
        .unwrap_or(false)
}

/// Writes notifications to the terminal, between draws, as the window title is written.
pub struct Notifier {
    target: Target,
    /// The cached answer of `query`, asked on the first notification inside tmux.
    passthrough: Option<bool>,
    query: Box<dyn FnMut() -> bool>,
    writer: Box<dyn Write>,
}

impl Notifier {
    /// A notifier for the terminal the process environment describes, writing to stdout.
    pub fn new() -> Notifier {
        Notifier::with(
            detect(&|k| std::env::var(k).ok()),
            Box::new(query_tmux_passthrough),
            Box::new(std::io::stdout()),
        )
    }

    /// A notifier for `target` that asks `query` whether tmux passthrough is on and writes to
    /// `writer`.
    pub fn with(
        target: Target,
        query: Box<dyn FnMut() -> bool>,
        writer: Box<dyn Write>,
    ) -> Notifier {
        Notifier {
            target,
            passthrough: None,
            query,
            writer,
        }
    }

    fn passthrough(&mut self) -> bool {
        *self.passthrough.get_or_insert_with(&mut self.query)
    }

    /// What `alert` writes under `mode`: nothing when off, the bell when `bell` or when the
    /// terminal has no allowlisted sequence or tmux would drop it, else the sequence.
    pub fn bytes(&mut self, mode: NotificationMode, alert: &ChatAlert) -> Option<String> {
        match mode {
            NotificationMode::Off => None,
            NotificationMode::Bell => Some(BEL.into()),
            NotificationMode::Desktop => {
                let (title, body) = message(alert);
                Some(match sequence(self.target.kind, &title, &body) {
                    None => BEL.into(),
                    Some(seq) if !self.target.tmux => seq,
                    Some(seq) if self.passthrough() => tmux_wrap(&seq),
                    Some(_) => BEL.into(),
                })
            }
        }
    }

    /// Writes `alert`'s notification under `mode`.
    pub fn notify(&mut self, mode: NotificationMode, alert: &ChatAlert) -> std::io::Result<()> {
        let Some(bytes) = self.bytes(mode, alert) else {
            return Ok(());
        };
        self.writer.write_all(bytes.as_bytes())?;
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    fn kind(pairs: &[(&str, &str)]) -> TerminalKind {
        detect(&env(pairs)).kind
    }

    #[test]
    fn the_terminal_comes_from_the_injected_environment() {
        use TerminalKind::*;
        assert_eq!(kind(&[("TERM_PROGRAM", "WarpTerminal")]), Warp);
        assert_eq!(kind(&[("TERM_PROGRAM", "iTerm.app")]), ITerm2);
        assert_eq!(kind(&[("TERM_PROGRAM", "ghostty")]), Ghostty);
        assert_eq!(kind(&[("TERM", "xterm-kitty")]), Kitty);
        assert_eq!(kind(&[("KITTY_WINDOW_ID", "1")]), Kitty);
        assert_eq!(kind(&[("TERM", "xterm-ghostty")]), Ghostty);
        assert_eq!(kind(&[("LC_TERMINAL", "iTerm2")]), ITerm2, "over SSH");
        assert_eq!(kind(&[]), Other);
        assert_eq!(kind(&[("TERM", "linux")]), Other);
        assert_eq!(kind(&[("TERM_PROGRAM", "Apple_Terminal")]), Other);
        assert_eq!(kind(&[("TERM_PROGRAM", "WezTerm")]), Other);
        assert_eq!(
            kind(&[("TERM_PROGRAM", "vscode"), ("ITERM_SESSION_ID", "w0")]),
            Other,
            "a named terminal wins over another's leftover variables"
        );
        assert_eq!(
            kind(&[("TERM_PROGRAM", "WarpTerminal"), ("ZELLIJ", "0")]),
            Other,
            "Zellij forwards nothing"
        );
        assert_eq!(
            kind(&[("TERM_PROGRAM", ""), ("TERM", "xterm-kitty")]),
            Kitty
        );
    }

    #[test]
    fn inside_tmux_the_outer_terminal_comes_from_its_own_variables() {
        let target = detect(&env(&[
            ("TMUX", "/tmp/tmux-501/default,1,0"),
            ("TERM_PROGRAM", "tmux"),
            ("WARP_CLIENT_VERSION", "v0.2026"),
        ]));
        assert_eq!(
            target,
            Target {
                kind: TerminalKind::Warp,
                tmux: true
            }
        );
        assert!(!detect(&env(&[("TERM_PROGRAM", "ghostty")])).tmux);
    }

    #[test]
    fn each_terminal_gets_its_own_sequence() {
        assert_eq!(
            sequence(TerminalKind::Warp, "scuttle", "Fix finished").as_deref(),
            Some("\x1b]777;notify;scuttle;Fix finished\x07")
        );
        assert_eq!(
            sequence(TerminalKind::Ghostty, "scuttle", "Fix failed").as_deref(),
            Some("\x1b]777;notify;scuttle;Fix failed\x07")
        );
        assert_eq!(
            sequence(TerminalKind::ITerm2, "scuttle", "42 finished").as_deref(),
            Some("\x1b]9;scuttle: 42 finished\x07"),
            "the body never starts with digits"
        );
        assert_eq!(
            sequence(TerminalKind::Kitty, "scuttle", "Fix finished").as_deref(),
            Some(
                "\x1b]99;i=scuttle:d=0:o=unfocused;scuttle\x1b\\\x1b]99;i=scuttle:p=body:o=unfocused;Fix finished\x1b\\"
            )
        );
        assert_eq!(
            sequence(TerminalKind::Other, "scuttle", "Fix finished"),
            None
        );
    }

    #[test]
    fn titles_lose_semicolons_and_control_characters_and_are_capped() {
        assert_eq!(clean("a;b\x1b]9;c\x07d\u{9c}e\nf"), "ab]9cdef");
        let long = "x".repeat(500);
        let cut = clean(&long);
        assert_eq!(cut.chars().count(), TEXT_MAX);
        assert!(cut.ends_with('\u{2026}'));
        assert_eq!(
            sequence(
                TerminalKind::Warp,
                "s;t",
                "evil\x07\x1b]0;pwned\x1b\\ finished"
            )
            .as_deref(),
            Some("\x1b]777;notify;st;evil]0pwned\\ finished\x07")
        );
    }

    #[test]
    fn tmux_passthrough_doubles_every_escape() {
        assert_eq!(
            tmux_wrap("\x1b]9;hi\x07"),
            "\x1bPtmux;\x1b\x1b]9;hi\x07\x1b\\"
        );
    }

    #[test]
    fn only_an_unfocused_or_unknown_focus_notifies_and_off_never_does() {
        use NotificationMode::*;
        assert!(should_notify(Desktop, None));
        assert!(should_notify(Desktop, Some(false)));
        assert!(!should_notify(Desktop, Some(true)));
        assert!(should_notify(Bell, Some(false)));
        assert!(!should_notify(Bell, Some(true)));
        assert!(!should_notify(Off, Some(false)));
        assert!(!should_notify(Off, None));
    }

    fn alert(outcome: Outcome) -> ChatAlert {
        ChatAlert {
            chat_id: uuid::Uuid::nil(),
            title: "Fix the swagger annotations".into(),
            outcome,
            open: false,
        }
    }

    #[test]
    fn the_body_says_how_the_chat_ended() {
        assert_eq!(
            message(&alert(Outcome::Finished)),
            (
                "scuttle".into(),
                "Fix the swagger annotations finished".into()
            )
        );
        assert_eq!(
            message(&alert(Outcome::NeedsAnswer)).1,
            "Fix the swagger annotations needs an answer"
        );
        assert_eq!(
            message(&alert(Outcome::Failed)).1,
            "Fix the swagger annotations failed"
        );
    }

    /// A writer the test can read back after the notifier owns it.
    #[derive(Clone, Default)]
    struct Shared(Rc<RefCell<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn notifier(
        kind: TerminalKind,
        tmux: bool,
        passthrough: bool,
    ) -> (Notifier, Shared, Rc<RefCell<u32>>) {
        let out = Shared::default();
        let asked = Rc::new(RefCell::new(0));
        let counter = asked.clone();
        let n = Notifier::with(
            Target { kind, tmux },
            Box::new(move || {
                *counter.borrow_mut() += 1;
                passthrough
            }),
            Box::new(out.clone()),
        );
        (n, out, asked)
    }

    fn written(out: &Shared) -> String {
        String::from_utf8(std::mem::take(&mut *out.0.borrow_mut())).unwrap()
    }

    #[test]
    fn off_writes_nothing_and_bell_writes_only_the_bell() {
        for kind in [
            TerminalKind::Warp,
            TerminalKind::ITerm2,
            TerminalKind::Ghostty,
            TerminalKind::Kitty,
            TerminalKind::Other,
        ] {
            let (mut n, out, _) = notifier(kind, false, true);
            n.notify(NotificationMode::Off, &alert(Outcome::Finished))
                .unwrap();
            assert_eq!(written(&out), "", "{kind:?}");
            n.notify(NotificationMode::Bell, &alert(Outcome::Failed))
                .unwrap();
            assert_eq!(written(&out), BEL, "{kind:?}");
        }
    }

    #[test]
    fn an_unknown_terminal_gets_only_the_bell() {
        let (mut n, out, _) = notifier(TerminalKind::Other, false, true);
        n.notify(NotificationMode::Desktop, &alert(Outcome::Finished))
            .unwrap();
        assert_eq!(written(&out), BEL);
    }

    #[test]
    fn desktop_writes_the_terminal_sequence() {
        let (mut n, out, asked) = notifier(TerminalKind::Warp, false, true);
        n.notify(NotificationMode::Desktop, &alert(Outcome::NeedsAnswer))
            .unwrap();
        assert_eq!(
            written(&out),
            "\x1b]777;notify;scuttle;Fix the swagger annotations needs an answer\x07"
        );
        assert_eq!(*asked.borrow(), 0, "tmux is asked only inside tmux");
    }

    #[test]
    fn tmux_wraps_the_sequence_only_with_passthrough_and_asks_once() {
        let (mut n, out, asked) = notifier(TerminalKind::ITerm2, true, true);
        n.notify(NotificationMode::Desktop, &alert(Outcome::Finished))
            .unwrap();
        n.notify(NotificationMode::Desktop, &alert(Outcome::Finished))
            .unwrap();
        let once = "\x1bPtmux;\x1b\x1b]9;scuttle: Fix the swagger annotations finished\x07\x1b\\";
        assert_eq!(written(&out), format!("{once}{once}"));
        assert_eq!(*asked.borrow(), 1);

        let (mut n, out, _) = notifier(TerminalKind::Kitty, true, false);
        n.notify(NotificationMode::Desktop, &alert(Outcome::Finished))
            .unwrap();
        assert_eq!(written(&out), BEL, "without passthrough tmux gets the bell");
    }
}
