//! A one-line text editor the core owns, for renames, titles, and free-text answers.
//!
//! The cursor moves by extended grapheme cluster, not by `char`, so Left, Right, Backspace, and
//! Delete never split a multi-codepoint cluster such as a combining accent or an emoji with a
//! skin-tone modifier.

use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    Char(char),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    Submit,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOutcome {
    Editing,
    /// The trimmed text.
    Submitted(String),
    Cancelled,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineEdit {
    text: String,
    /// The cursor position in extended grapheme clusters.
    cursor: usize,
}

impl LineEdit {
    /// An editor holding `text` with the cursor at its end.
    pub fn new(text: &str) -> LineEdit {
        LineEdit {
            text: text.to_owned(),
            cursor: text.graphemes(true).count(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The byte offset of grapheme boundary `at`, or the end of the text past the last one.
    fn byte(&self, at: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .nth(at)
            .map_or(self.text.len(), |(i, _)| i)
    }

    pub fn apply(&mut self, edit: Edit) -> EditOutcome {
        let len = self.text.graphemes(true).count();
        match edit {
            Edit::Char(c) => {
                let at = self.byte(self.cursor);
                self.text.insert(at, c);
                // A joining character (a combining mark, an emoji modifier, a ZWJ, a flag
                // half) merges with what is already there into one grapheme cluster, so the
                // cursor is recomputed from the text rather than bumped by one; otherwise it
                // would land past the cluster it just joined instead of right after it.
                let end = at + c.len_utf8();
                self.cursor = self.text[..end].graphemes(true).count();
            }
            Edit::Backspace if self.cursor > 0 => {
                let start = self.byte(self.cursor - 1);
                let end = self.byte(self.cursor);
                self.text.replace_range(start..end, "");
                self.cursor -= 1;
            }
            Edit::Delete if self.cursor < len => {
                let start = self.byte(self.cursor);
                let end = self.byte(self.cursor + 1);
                self.text.replace_range(start..end, "");
            }
            Edit::Left => self.cursor = self.cursor.saturating_sub(1),
            Edit::Right => self.cursor = (self.cursor + 1).min(len),
            Edit::Home => self.cursor = 0,
            Edit::End => self.cursor = len,
            Edit::Submit => return EditOutcome::Submitted(self.text.trim().to_owned()),
            Edit::Cancel => return EditOutcome::Cancelled,
            Edit::Backspace | Edit::Delete => {}
        }
        EditOutcome::Editing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_happen_at_the_cursor_across_multibyte_characters() {
        let mut e = LineEdit::new("héllo");
        assert_eq!(e.cursor(), 5);
        e.apply(Edit::Left);
        e.apply(Edit::Left);
        e.apply(Edit::Backspace);
        assert_eq!(e.text(), "hélo");
        assert_eq!(e.cursor(), 2);
        e.apply(Edit::Char('ł'));
        assert_eq!(e.text(), "héłlo");
        e.apply(Edit::Home);
        e.apply(Edit::Delete);
        assert_eq!(e.text(), "éłlo");
        e.apply(Edit::Right);
        e.apply(Edit::End);
        assert_eq!(e.cursor(), 4);
    }

    #[test]
    fn submit_trims_and_cancel_discards() {
        let mut e = LineEdit::new("  Fix the watch test ");
        assert_eq!(
            e.apply(Edit::Submit),
            EditOutcome::Submitted("Fix the watch test".into())
        );
        assert_eq!(e.apply(Edit::Cancel), EditOutcome::Cancelled);
        assert_eq!(e.apply(Edit::Char('x')), EditOutcome::Editing);
    }

    #[test]
    fn edits_never_split_a_grapheme_cluster() {
        // "e" plus a combining acute accent (U+0301) forms one grapheme, "é"; a thumbs-up
        // plus a skin-tone modifier (U+1F3FD) forms another. Each is two code points.
        let combining_e = "e\u{0301}";
        let thumbs_up = "\u{1F44D}\u{1F3FD}";
        let joined = format!("{combining_e}{thumbs_up}");

        let mut e = LineEdit::new(&joined);
        assert_eq!(e.cursor(), 2, "two grapheme clusters, not four code points");
        e.apply(Edit::Left);
        assert_eq!(e.cursor(), 1, "left moves by a whole cluster");
        e.apply(Edit::Backspace);
        assert_eq!(
            e.text(),
            thumbs_up,
            "backspace removed the whole combining-accent cluster, not just the accent"
        );

        let mut e = LineEdit::new(&joined);
        e.apply(Edit::Home);
        e.apply(Edit::Delete);
        assert_eq!(
            e.text(),
            thumbs_up,
            "delete removed the whole combining-accent cluster, not just the base letter"
        );
        e.apply(Edit::Delete);
        assert_eq!(
            e.text(),
            "",
            "delete removed the whole emoji-modifier cluster, not just one code point"
        );
    }

    #[test]
    fn typing_a_joining_character_keeps_the_cursor_right_after_its_cluster() {
        let mut e = LineEdit::new("ab");
        e.apply(Edit::Left);
        assert_eq!(e.cursor(), 1);
        e.apply(Edit::Char('\u{0301}'));
        e.apply(Edit::Char('x'));
        assert_eq!(
            e.text(),
            "a\u{0301}xb",
            "the combining accent joined the preceding letter instead of standing alone"
        );
        assert_eq!(
            e.cursor(),
            2,
            "the cursor sits after 'x', not two codepoints past the accent"
        );

        let mut e = LineEdit::new("");
        e.apply(Edit::Char('\u{1F44D}'));
        e.apply(Edit::Char('\u{1F3FD}'));
        assert_eq!(
            e.cursor(),
            1,
            "the skin-tone modifier joined the thumbs-up into one cluster"
        );
        e.apply(Edit::Backspace);
        assert_eq!(
            e.text(),
            "",
            "backspace removed the whole joined cluster, not just the modifier"
        );
    }
}
