//! The multi-line input box with sent-message history and slash completion.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_textarea::TextArea;
use scuttle_core::commands::{CommandInfo, completions};
use scuttle_core::density::SendShortcut;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerAction {
    None,
    Submit(String),
    OpenEditor,
    Interrupt,
}

pub struct Composer {
    area: TextArea<'static>,
    max_lines: u16,
    history: Vec<String>,
    history_pos: Option<usize>,
    draft: String,
}

/// Normalizes CRLF and lone CR line endings to LF, because bracketed paste in several
/// terminals delivers CR-based line endings instead of plain `\n`.
fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Characters that `ratatui_textarea::TextArea::input` binds to emacs-style editing and
/// cursor movement when Alt is held without Ctrl. Everything else typed with Alt (or
/// AltGr, which several European layouts report as Ctrl+Alt) is inserted literally
/// instead of being swallowed by the widget.
fn is_alt_reserved(c: char) -> bool {
    matches!(
        c,
        'h' | 'd' | 'f' | 'b' | 'n' | 'p' | 'v' | '<' | '>' | '[' | ']'
    )
}

impl Composer {
    pub fn new(max_lines: u16) -> Composer {
        let mut area = TextArea::default();
        area.set_placeholder_text(
            "Message the agent. Enter to send, Shift+Enter for a new line, /help for commands.",
        );
        Composer {
            area,
            max_lines: max_lines.max(1),
            history: Vec::new(),
            history_pos: None,
            draft: String::new(),
        }
    }

    pub fn widget(&self) -> &TextArea<'static> {
        &self.area
    }

    /// Replaces the hint shown while the composer is empty.
    pub fn set_placeholder(&mut self, text: &str) {
        self.area.set_placeholder_text(text);
    }

    pub fn text(&self) -> String {
        self.area.lines().join("\n")
    }

    pub fn set_text(&mut self, text: &str) {
        let text = normalize_line_endings(text);
        let lines: Vec<String> = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(str::to_owned).collect()
        };
        let placeholder = self.area.placeholder_text().to_owned();
        self.area = TextArea::new(lines);
        self.area.set_placeholder_text(placeholder);
        self.area.move_cursor(ratatui_textarea::CursorMove::Bottom);
        self.area.move_cursor(ratatui_textarea::CursorMove::End);
    }

    pub fn paste(&mut self, text: &str) {
        self.area.insert_str(normalize_line_endings(text));
    }

    /// Content lines plus the border, never more than `max_lines` of content.
    pub fn height(&self) -> u16 {
        (self.area.lines().len() as u16).clamp(1, self.max_lines) + 2
    }

    pub fn slash_matches(&self) -> Vec<&'static CommandInfo> {
        let text = self.text();
        if !text.starts_with('/') || text.contains(char::is_whitespace) {
            return Vec::new();
        }
        completions(&text)
    }

    fn submit(&mut self) -> ComposerAction {
        let text = self.text();
        if text.trim().is_empty() {
            return ComposerAction::None;
        }
        self.history.push(text.clone());
        self.history_pos = None;
        self.draft.clear();
        self.set_text("");
        ComposerAction::Submit(text)
    }

    fn recall(&mut self, older: bool) {
        if self.history.is_empty() {
            return;
        }
        let next = match (self.history_pos, older) {
            (None, true) => {
                self.draft = self.text();
                Some(self.history.len() - 1)
            }
            (Some(0), true) => Some(0),
            (Some(i), true) => Some(i - 1),
            (Some(i), false) if i + 1 < self.history.len() => Some(i + 1),
            (Some(_), false) => None,
            (None, false) => return,
        };
        self.history_pos = next;
        let text = match next {
            Some(i) => self.history[i].clone(),
            None => std::mem::take(&mut self.draft),
        };
        self.set_text(&text);
    }

    pub fn handle_key(&mut self, key: KeyEvent, shortcut: SendShortcut) -> ComposerAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let sup = key.modifiers.contains(KeyModifiers::SUPER);
        match key.code {
            KeyCode::Enter if alt => self.area.insert_newline(),
            KeyCode::Enter => {
                let send = match shortcut {
                    SendShortcut::Enter => !shift && !ctrl && !sup,
                    SendShortcut::ModifierEnter => ctrl || sup,
                };
                if send {
                    return self.submit();
                }
                self.area.insert_newline();
            }
            KeyCode::Char('j') if ctrl => self.area.insert_newline(),
            KeyCode::Char('g') if ctrl => return ComposerAction::OpenEditor,
            KeyCode::Esc => return ComposerAction::Interrupt,
            KeyCode::Tab => {
                if let Some(first) = self.slash_matches().first() {
                    self.set_text(&format!("{} ", first.name));
                }
            }
            KeyCode::Up if self.area.cursor().0 == 0 => self.recall(true),
            KeyCode::Down
                if self.area.cursor().0 + 1 >= self.area.lines().len()
                    && self.history_pos.is_some() =>
            {
                self.recall(false)
            }
            // AltGr on several European layouts reports as Ctrl+Alt; treat it as a literal
            // character rather than the widget's few Ctrl+Alt navigation bindings.
            KeyCode::Char(c) if ctrl && alt => self.area.insert_char(c),
            // TextArea::input only inserts a Char when Alt is not held, and otherwise
            // treats Alt+<letter> as an emacs-style editing or movement shortcut. Insert
            // directly unless `c` is one of those reserved shortcut keys.
            KeyCode::Char(c) if alt && !ctrl && !is_alt_reserved(c) => self.area.insert_char(c),
            _ => {
                self.area.input(key);
            }
        }
        ComposerAction::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn type_str(c: &mut Composer, s: &str) {
        for ch in s.chars() {
            c.handle_key(
                key(KeyCode::Char(ch), KeyModifiers::NONE),
                SendShortcut::Enter,
            );
        }
    }

    #[test]
    fn enter_sends_and_shift_enter_inserts_a_newline() {
        let mut c = Composer::new(10);
        type_str(&mut c, "a");
        assert_eq!(
            c.handle_key(
                key(KeyCode::Enter, KeyModifiers::SHIFT),
                SendShortcut::Enter
            ),
            ComposerAction::None
        );
        type_str(&mut c, "b");
        assert_eq!(
            c.handle_key(key(KeyCode::Enter, KeyModifiers::NONE), SendShortcut::Enter),
            ComposerAction::Submit("a\nb".into())
        );
        assert_eq!(c.text(), "");
    }

    #[test]
    fn modifier_enter_mode_swaps_the_keys() {
        let mut c = Composer::new(10);
        type_str(&mut c, "a");
        assert_eq!(
            c.handle_key(
                key(KeyCode::Enter, KeyModifiers::NONE),
                SendShortcut::ModifierEnter
            ),
            ComposerAction::None
        );
        assert_eq!(
            c.handle_key(
                key(KeyCode::Enter, KeyModifiers::CONTROL),
                SendShortcut::ModifierEnter
            ),
            ComposerAction::Submit("a\n".into())
        );
    }

    #[test]
    fn alt_enter_and_ctrl_j_always_insert_newlines() {
        let mut c = Composer::new(10);
        type_str(&mut c, "a");
        c.handle_key(key(KeyCode::Enter, KeyModifiers::ALT), SendShortcut::Enter);
        c.handle_key(
            key(KeyCode::Char('j'), KeyModifiers::CONTROL),
            SendShortcut::Enter,
        );
        assert_eq!(c.text(), "a\n\n");
    }

    #[test]
    fn empty_enter_does_not_submit() {
        let mut c = Composer::new(10);
        assert_eq!(
            c.handle_key(key(KeyCode::Enter, KeyModifiers::NONE), SendShortcut::Enter),
            ComposerAction::None
        );
    }

    #[test]
    fn history_recalls_previous_messages_and_restores_the_draft() {
        let mut c = Composer::new(10);
        type_str(&mut c, "first");
        c.handle_key(key(KeyCode::Enter, KeyModifiers::NONE), SendShortcut::Enter);
        type_str(&mut c, "second");
        c.handle_key(key(KeyCode::Enter, KeyModifiers::NONE), SendShortcut::Enter);
        type_str(&mut c, "draft");
        c.handle_key(key(KeyCode::Up, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(c.text(), "second");
        c.handle_key(key(KeyCode::Up, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(c.text(), "first");
        c.handle_key(key(KeyCode::Down, KeyModifiers::NONE), SendShortcut::Enter);
        c.handle_key(key(KeyCode::Down, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(c.text(), "draft");
    }

    #[test]
    fn tab_completes_the_first_slash_match() {
        let mut c = Composer::new(10);
        type_str(&mut c, "/co");
        assert_eq!(c.slash_matches().len(), 2);
        c.handle_key(key(KeyCode::Tab, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(c.text(), "/compact ");
    }

    #[test]
    fn paste_is_verbatim_and_ctrl_g_opens_the_editor() {
        let mut c = Composer::new(10);
        c.paste("line one\nline two");
        assert_eq!(c.text(), "line one\nline two");
        assert_eq!(
            c.handle_key(
                key(KeyCode::Char('g'), KeyModifiers::CONTROL),
                SendShortcut::Enter
            ),
            ComposerAction::OpenEditor
        );
        assert_eq!(
            c.handle_key(key(KeyCode::Esc, KeyModifiers::NONE), SendShortcut::Enter),
            ComposerAction::Interrupt
        );
    }

    #[test]
    fn height_grows_with_content_up_to_the_limit() {
        let mut c = Composer::new(3);
        assert_eq!(c.height(), 3);
        c.paste("1\n2\n3\n4\n5");
        assert_eq!(c.height(), 5);
    }

    #[test]
    fn paste_and_set_text_normalize_crlf_and_cr_line_endings() {
        let mut c = Composer::new(10);
        c.paste("a\r\nb\rc");
        assert_eq!(c.text(), "a\nb\nc");
        c.set_text("a\r\nb\rc");
        assert_eq!(c.text(), "a\nb\nc");
    }

    #[test]
    fn alt_and_altgr_characters_insert_directly() {
        let mut c = Composer::new(10);
        c.handle_key(
            key(KeyCode::Char('s'), KeyModifiers::ALT),
            SendShortcut::Enter,
        );
        assert_eq!(c.text(), "s");
    }

    #[test]
    fn ctrl_alt_characters_insert_directly() {
        let mut c = Composer::new(10);
        c.handle_key(
            key(
                KeyCode::Char('@'),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            ),
            SendShortcut::Enter,
        );
        assert_eq!(c.text(), "@");
    }

    #[test]
    fn alt_reserved_emacs_keys_still_move_instead_of_inserting() {
        let mut c = Composer::new(10);
        type_str(&mut c, "ab");
        assert_eq!(c.widget().cursor(), (0, 2));
        c.handle_key(
            key(KeyCode::Char('b'), KeyModifiers::ALT),
            SendShortcut::Enter,
        );
        assert_eq!(c.text(), "ab");
        assert_eq!(c.widget().cursor(), (0, 0));
    }

    #[test]
    fn alt_characters_outside_the_widgets_reserved_set_insert_directly() {
        let mut c = Composer::new(10);
        c.handle_key(
            key(KeyCode::Char('w'), KeyModifiers::ALT),
            SendShortcut::Enter,
        );
        c.handle_key(
            key(KeyCode::Char('e'), KeyModifiers::ALT),
            SendShortcut::Enter,
        );
        c.handle_key(
            key(KeyCode::Char('a'), KeyModifiers::ALT),
            SendShortcut::Enter,
        );
        assert_eq!(c.text(), "wea");
    }

    #[test]
    fn history_recall_handles_multi_line_entries() {
        let mut c = Composer::new(10);
        type_str(&mut c, "l1");
        c.handle_key(
            key(KeyCode::Enter, KeyModifiers::SHIFT),
            SendShortcut::Enter,
        );
        type_str(&mut c, "l2");
        c.handle_key(key(KeyCode::Enter, KeyModifiers::NONE), SendShortcut::Enter);
        type_str(&mut c, "x");
        c.handle_key(key(KeyCode::Up, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(c.text(), "l1\nl2");
        c.handle_key(key(KeyCode::Up, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(c.text(), "l1\nl2");
        c.handle_key(key(KeyCode::Down, KeyModifiers::NONE), SendShortcut::Enter);
        c.handle_key(key(KeyCode::Down, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(c.text(), "x");
    }
}
