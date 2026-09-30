//! The multi-line input box with sent-message history and slash completion.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use ratatui_textarea::{CursorMove, TextArea, WrapMode};
use scuttle_core::commands::{CommandInfo, completions};
use scuttle_core::density::SendShortcut;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

/// Columns a tab advances to, matching `TextArea`'s default tab length.
const TAB: usize = 4;

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
    /// Whether the terminal reports modified Enter keys distinctly (keyboard enhancement).
    enhanced: bool,
    /// Style of the line-number gutter shown while the text has more than one line.
    gutter: Style,
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

/// The display width after `text`, starting at column `width`, with tabs advancing to the next
/// stop. This matches `display_width_to` in ratatui-textarea 0.9.2's `wrap.rs`.
fn width_to(text: &str, mut width: usize) -> usize {
    for c in text.chars() {
        if c == '\t' {
            width += TAB - (width % TAB);
        } else {
            width += c.width().unwrap_or(0);
        }
    }
    width
}

/// Rows a word too wide for one row takes when split at grapheme boundaries, following
/// `split_range_by_grapheme_width` in ratatui-textarea 0.9.2.
fn glyph_rows(text: &str, width: usize) -> usize {
    let mut rows = 0;
    let mut used = 0;
    let mut open = false;
    for g in text.graphemes(true) {
        let mut w = width_to(g, used) - used;
        if open && used + w > width {
            rows += 1;
            used = 0;
            w = width_to(g, 0);
        }
        used += w;
        open = true;
        if used > width {
            rows += 1;
            used = 0;
            open = false;
        }
    }
    rows + usize::from(open)
}

/// Rows `line` takes at `width` columns under `WrapMode::WordOrGlyph`. ratatui-textarea
/// computes this in `wrap_word_chunks` but does not export it, so this mirrors that function;
/// `height_counts_the_rows_the_widget_draws` pins the two together.
fn wrapped_rows(line: &str, width: usize) -> usize {
    let width = width.max(1);
    let chunks: Vec<&str> = line.split_word_bounds().collect();
    let mut rows = 0;
    let mut used = 0;
    let mut open = false;
    let mut i = 0;
    while i < chunks.len() {
        let w = width_to(chunks[i], used) - used;
        if used + w <= width {
            used += w;
            open = true;
            i += 1;
            continue;
        }
        if open {
            rows += 1;
            used = 0;
            open = false;
            continue;
        }
        rows += glyph_rows(chunks[i], width);
        used = 0;
        i += 1;
    }
    (rows + usize::from(open)).max(1)
}

impl Composer {
    pub fn new(max_lines: u16) -> Composer {
        let mut area = TextArea::default();
        area.set_placeholder_text(
            "Message the agent. Enter to send, Shift+Enter for a new line, /help for commands.",
        );
        let mut composer = Composer {
            area,
            max_lines: max_lines.max(1),
            history: Vec::new(),
            history_pos: None,
            draft: String::new(),
            enhanced: true,
            gutter: Style::default(),
        };
        composer.configure();
        composer
    }

    /// A fresh `TextArea` does not wrap, and `set_text` makes a fresh one, so both call this.
    fn configure(&mut self) {
        self.area.set_wrap_mode(WrapMode::WordOrGlyph);
        self.sync_gutter();
    }

    /// Shows line numbers only while there is more than one line. The widget itself leaves the
    /// gutter blank on the continuation rows of a wrapped line.
    fn sync_gutter(&mut self) {
        let multi = self.area.lines().len() > 1;
        match (multi, self.area.line_number_style()) {
            (true, Some(style)) if style == self.gutter => {}
            (true, _) => self.area.set_line_number_style(self.gutter),
            (false, Some(_)) => self.area.remove_line_number(),
            (false, None) => {}
        }
    }

    /// Sets the line-number gutter's style.
    pub fn set_gutter_style(&mut self, style: Style) {
        self.gutter = style;
        self.sync_gutter();
    }

    pub fn widget(&self) -> &TextArea<'static> {
        &self.area
    }

    /// Records whether keyboard enhancement is active. Without it, terminals report Ctrl+Enter
    /// and Cmd+Enter as a plain Enter, so the `ModifierEnter` preference sends with Alt+Enter.
    pub fn set_enhanced(&mut self, enhanced: bool) {
        self.enhanced = enhanced;
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
        self.configure();
        self.area.move_cursor(ratatui_textarea::CursorMove::Bottom);
        self.area.move_cursor(ratatui_textarea::CursorMove::End);
    }

    pub fn paste(&mut self, text: &str) {
        self.area.insert_str(normalize_line_endings(text));
        self.sync_gutter();
    }

    /// Leaves history browsing without touching the text being typed.
    pub fn reset_history_position(&mut self) {
        self.history_pos = None;
        self.draft.clear();
    }

    /// The rows the text takes at `width` columns (the composer's full inner width, gutter
    /// included), never more than `max_lines`, plus the border.
    pub fn height(&self, width: u16) -> u16 {
        let lines = self.area.lines();
        let gutter = if lines.len() > 1 {
            lines.len().to_string().len() + 2
        } else {
            0
        };
        let text_width = usize::from(width).saturating_sub(gutter).max(1);
        let rows: usize = lines.iter().map(|l| wrapped_rows(l, text_width)).sum();
        rows.clamp(1, usize::from(self.max_lines)) as u16 + 2
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

    /// Whether the cursor is on the last drawn row, the last wrapped row of the last line.
    /// `TextArea` exports no row count, so this compares against the row of the text's end.
    fn on_last_row(&mut self) -> bool {
        let cursor = self.area.cursor();
        if cursor.0 + 1 < self.area.lines().len() {
            return false;
        }
        // `Jump` takes u16 coordinates; past that, fall back to the logical last line.
        let (Ok(row), Ok(col)) = (u16::try_from(cursor.0), u16::try_from(cursor.1)) else {
            return true;
        };
        let here = self.area.screen_cursor().row;
        self.area.move_cursor(CursorMove::End);
        let last = self.area.screen_cursor().row;
        self.area.move_cursor(CursorMove::Jump(row, col));
        here == last
    }

    pub fn handle_key(&mut self, key: KeyEvent, shortcut: SendShortcut) -> ComposerAction {
        let action = self.key_action(key, shortcut);
        self.sync_gutter();
        action
    }

    fn key_action(&mut self, key: KeyEvent, shortcut: SendShortcut) -> ComposerAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let sup = key.modifiers.contains(KeyModifiers::SUPER);
        match key.code {
            KeyCode::Enter if alt => {
                if shortcut == SendShortcut::ModifierEnter && !self.enhanced {
                    return self.submit();
                }
                self.area.insert_newline();
            }
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
            KeyCode::Esc if !self.slash_matches().is_empty() => self.set_text(""),
            KeyCode::Esc => return ComposerAction::Interrupt,
            KeyCode::Tab => {
                if let Some(first) = self.slash_matches().first() {
                    self.set_text(&format!("{} ", first.name));
                }
            }
            KeyCode::Up if self.area.screen_cursor().row == 0 => self.recall(true),
            KeyCode::Down if self.history_pos.is_some() && self.on_last_row() => self.recall(false),
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
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui_textarea::WrapMode;

    fn render(c: &Composer, width: u16, height: u16) -> Vec<String> {
        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|f| f.render_widget(c.widget(), f.area()))
            .unwrap();
        let buf = term.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect()
            })
            .collect()
    }

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
    fn modifier_enter_without_enhancement_sends_with_alt_enter() {
        let mut c = Composer::new(10);
        c.set_enhanced(false);
        type_str(&mut c, "a");
        // Without enhancement, Ctrl+Enter arrives as a plain Enter, which adds a line.
        assert_eq!(
            c.handle_key(
                key(KeyCode::Enter, KeyModifiers::NONE),
                SendShortcut::ModifierEnter
            ),
            ComposerAction::None
        );
        type_str(&mut c, "b");
        assert_eq!(
            c.handle_key(
                key(KeyCode::Enter, KeyModifiers::ALT),
                SendShortcut::ModifierEnter
            ),
            ComposerAction::Submit("a\nb".into())
        );
    }

    #[test]
    fn enter_mode_without_enhancement_sends_with_enter_and_alt_enter_adds_a_line() {
        let mut c = Composer::new(10);
        c.set_enhanced(false);
        type_str(&mut c, "a");
        assert_eq!(
            c.handle_key(key(KeyCode::Enter, KeyModifiers::ALT), SendShortcut::Enter),
            ComposerAction::None
        );
        type_str(&mut c, "b");
        assert_eq!(
            c.handle_key(key(KeyCode::Enter, KeyModifiers::NONE), SendShortcut::Enter),
            ComposerAction::Submit("a\nb".into())
        );
    }

    #[test]
    fn modifier_enter_with_enhancement_keeps_alt_enter_as_a_newline() {
        let mut c = Composer::new(10);
        type_str(&mut c, "a");
        assert_eq!(
            c.handle_key(
                key(KeyCode::Enter, KeyModifiers::ALT),
                SendShortcut::ModifierEnter
            ),
            ComposerAction::None
        );
        assert_eq!(c.text(), "a\n");
    }

    #[test]
    fn escape_with_the_slash_menu_showing_clears_instead_of_interrupting() {
        let mut c = Composer::new(10);
        type_str(&mut c, "/co");
        assert!(!c.slash_matches().is_empty());
        assert_eq!(
            c.handle_key(key(KeyCode::Esc, KeyModifiers::NONE), SendShortcut::Enter),
            ComposerAction::None
        );
        assert_eq!(c.text(), "");
        assert_eq!(
            c.handle_key(key(KeyCode::Esc, KeyModifiers::NONE), SendShortcut::Enter),
            ComposerAction::Interrupt
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
    fn resetting_the_history_position_keeps_the_text() {
        let mut c = Composer::new(10);
        type_str(&mut c, "sent");
        c.handle_key(key(KeyCode::Enter, KeyModifiers::NONE), SendShortcut::Enter);
        c.handle_key(key(KeyCode::Up, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(c.text(), "sent");
        c.reset_history_position();
        c.handle_key(key(KeyCode::Down, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(
            c.text(),
            "sent",
            "Down no longer walks back to the old draft"
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
    fn tab_completes_an_alias_to_its_command() {
        let mut c = Composer::new(10);
        type_str(&mut c, "/ex");
        assert_eq!(c.slash_matches().len(), 1);
        c.handle_key(key(KeyCode::Tab, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(c.text(), "/quit ");
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
        assert_eq!(c.height(80), 3);
        c.paste("1\n2\n3\n4\n5");
        assert_eq!(c.height(80), 5);
    }

    #[test]
    fn long_lines_wrap_and_count_toward_the_height() {
        let mut c = Composer::new(10);
        c.set_text("the quick brown fox jumps over the lazy dog");
        assert_eq!(c.widget().wrap_mode(), WrapMode::WordOrGlyph);
        assert_eq!(c.height(12), 4 + 2);
        assert_eq!(c.height(80), 1 + 2);
    }

    #[test]
    fn height_counts_the_rows_the_widget_draws() {
        let cases = [
            ("the quick brown fox jumps over the lazy dog", 12u16),
            ("averyveryverylongwordthatmustsplitsomewhere", 10),
            ("中文字符测试中文字符测试", 9),
            ("emoji 👩‍💻 and words 👩‍💻👩‍💻 more", 7),
            ("tab\tseparated\tvalues and more", 8),
            ("two lines\nthe second one wraps around", 10),
        ];
        for (text, width) in cases {
            let mut c = Composer::new(50);
            c.set_text(text);
            render(&c, width, 40);
            // `set_text` leaves the cursor at the end, so its screen row is the last row.
            let drawn = c.widget().screen_cursor().row + 1;
            assert_eq!(
                usize::from(c.height(width) - 2),
                drawn,
                "{text:?} at width {width}"
            );
        }
    }

    #[test]
    fn line_numbers_show_only_for_more_than_one_line() {
        let mut c = Composer::new(10);
        type_str(&mut c, "one");
        assert_eq!(c.widget().line_number_style(), None);
        c.handle_key(
            key(KeyCode::Enter, KeyModifiers::SHIFT),
            SendShortcut::Enter,
        );
        assert!(c.widget().line_number_style().is_some());
        c.handle_key(
            key(KeyCode::Backspace, KeyModifiers::NONE),
            SendShortcut::Enter,
        );
        assert_eq!(c.widget().line_number_style(), None);
        c.set_text("a\nb");
        assert!(c.widget().line_number_style().is_some());
        assert_eq!(c.widget().wrap_mode(), WrapMode::WordOrGlyph);
        c.paste("\nc");
        assert!(c.widget().line_number_style().is_some());
    }

    #[test]
    fn wrapped_rows_get_a_blank_gutter_and_wide_characters_fit() {
        let mut c = Composer::new(10);
        c.set_text("中文字符测试中文字符测试\nsecond");
        let width = 13;
        let rows = render(&c, width, 10);
        // A 3-cell gutter leaves 10 columns, five ideographs, so line 1 takes three rows.
        assert_eq!(c.height(width), 4 + 2);
        assert!(rows[0].starts_with(" 1 "), "{rows:?}");
        assert!(
            rows[1].starts_with("   ") && rows[2].starts_with("   "),
            "{rows:?}"
        );
        assert!(rows[3].starts_with(" 2 "), "{rows:?}");
        let text: String = rows[..3]
            .iter()
            .map(|r| {
                r.chars()
                    .skip(3)
                    .filter(|ch| *ch != ' ')
                    .collect::<String>()
            })
            .collect();
        assert_eq!(text, "中文字符测试中文字符测试", "nothing was cut off");
    }

    #[test]
    fn history_recall_uses_visual_rows_of_a_wrapped_line() {
        let mut c = Composer::new(10);
        type_str(&mut c, "first");
        c.handle_key(key(KeyCode::Enter, KeyModifiers::NONE), SendShortcut::Enter);
        // At 8 columns this one logical line draws as "alpha ", "beta ", "gamma".
        type_str(&mut c, "alpha beta gamma");
        render(&c, 8, 10);
        assert_eq!(c.widget().screen_cursor().row, 2);
        c.handle_key(key(KeyCode::Up, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(
            c.text(),
            "alpha beta gamma",
            "Up on a lower row moves the cursor"
        );
        assert_eq!(c.widget().screen_cursor().row, 1);
        c.handle_key(key(KeyCode::Up, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(c.text(), "alpha beta gamma");
        assert_eq!(c.widget().screen_cursor().row, 0);
        c.handle_key(key(KeyCode::Up, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(c.text(), "first", "Up on the first row recalls");

        // Recall the wrapped line itself, then check that Down only recalls from its last row.
        let mut c = Composer::new(10);
        type_str(&mut c, "alpha beta gamma");
        c.handle_key(key(KeyCode::Enter, KeyModifiers::NONE), SendShortcut::Enter);
        type_str(&mut c, "x");
        c.handle_key(key(KeyCode::Up, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(c.text(), "alpha beta gamma");
        render(&c, 8, 10);
        c.handle_key(key(KeyCode::Up, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(c.widget().screen_cursor().row, 1);
        c.handle_key(key(KeyCode::Down, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(
            c.text(),
            "alpha beta gamma",
            "Down above the last row moves the cursor"
        );
        assert_eq!(c.widget().screen_cursor().row, 2);
        c.handle_key(key(KeyCode::Down, KeyModifiers::NONE), SendShortcut::Enter);
        assert_eq!(c.text(), "x", "Down on the last row restores the draft");
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
