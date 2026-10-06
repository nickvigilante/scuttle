//! The multi-line input box with sent-message history and slash completion.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use ratatui_textarea::{CursorMove, TextArea, WrapMode};
use scuttle_core::commands::COMMANDS;
use scuttle_core::density::SendShortcut;
use scuttle_core::skills::{self, MenuEntry, MenuKind};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

/// Columns a tab advances to, matching `TextArea`'s default tab length.
const TAB: usize = 4;

/// The most slash menu entries shown at once; Up and Down scroll through the rest.
pub const SLASH_ROWS: usize = 5;

/// A paste of at least this many lines becomes a snippet, as the web UI's `isLargePaste` says.
pub const PASTE_LINES: usize = 10;

/// A paste of at least this many characters becomes a snippet, as the web UI's
/// `isLargePaste` says.
pub const PASTE_CHARS: usize = 1000;

/// How soon pasting a snippet's text again expands its token instead of adding another.
pub const PASTE_AGAIN: Duration = Duration::from_secs(2);

/// Whether a paste of `text` is large enough to become a snippet. As in the web UI, a
/// trailing newline counts as a line, and the length is in UTF-16 code units, as JavaScript's
/// `String.length` counts it.
pub fn is_large_paste(text: &str) -> bool {
    text.split('\n').count() >= PASTE_LINES || text.encode_utf16().count() >= PASTE_CHARS
}

#[cfg(test)]
thread_local! {
    /// How many snippet tokens this test thread has built, so a test can pin that keys reuse them.
    static TOKEN_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The local time a paste arrived, with the UTC offset it was read in.
pub type PasteTime = chrono::DateTime<chrono::FixedOffset>;

/// A large paste the composer shows as a token and sends as a text file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snippet {
    pub number: u32,
    pub text: String,
    /// When the paste arrived, in local time, which names its file.
    pub pasted: PasteTime,
    /// The token, built once, since every key near the cursor looks for it.
    token: String,
}

impl Snippet {
    /// The snippet numbered `number` for `text`, pasted at `pasted`. Its token is
    /// `[Pasted text #1 +120 lines]`, or `[Pasted text #2 +1500 chars]` for a paste on one line.
    pub fn new(number: u32, text: String, pasted: PasteTime) -> Snippet {
        #[cfg(test)]
        TOKEN_BUILDS.with(|n| n.set(n.get() + 1));
        let token = match text.lines().count() {
            n if n > 1 => format!("[Pasted text #{number} +{n} lines]"),
            _ => format!("[Pasted text #{number} +{} chars]", text.chars().count()),
        };
        Snippet {
            number,
            text,
            pasted,
            token,
        }
    }

    /// What the composer shows in place of the text.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The name the text uploads under, `pasted-text-YYYY-MM-DD-HH-MM-SS.txt` in local time,
    /// as the web UI names a large paste by when it arrived.
    pub fn file_name(&self) -> String {
        self.pasted
            .format("pasted-text-%Y-%m-%d-%H-%M-%S.txt")
            .to_string()
    }
}

/// The char columns at which `token` starts on `line`.
fn token_starts(line: &str, token: &str) -> Vec<usize> {
    line.match_indices(token)
        .map(|(byte, _)| line[..byte].chars().count())
        .collect()
}

/// `line` with `token` taken out at byte `at`, leaving one space where it sat between words
/// and none at either end of the line.
fn cut_token(line: &str, at: usize, token: &str) -> String {
    let (before, after) = (&line[..at], &line[at + token.len()..]);
    let left = before.trim_end_matches([' ', '\t']);
    let right = after.trim_start_matches([' ', '\t']);
    let spaced = left.len() < before.len() || right.len() < after.len();
    if spaced && !left.is_empty() && !right.is_empty() {
        format!("{left} {right}")
    } else {
        format!("{left}{right}")
    }
}

/// What is left of a snippet token an edit cut into plain text, such as a character typed or
/// a newline inside it, or a cut over part of it. A draft that holds it is not sent, since
/// the token no longer resolves and its paste would be left out.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Remnant {
    number: u32,
    text: String,
}

/// The byte length of the longest common prefix of `a` and `b`, on a char boundary.
fn common_prefix(a: &str, b: &str) -> usize {
    a.char_indices()
        .zip(b.chars())
        .find(|((_, x), y)| x != y)
        .map_or(a.len().min(b.len()), |((i, _), _)| i)
}

/// The byte length of the longest common suffix of `a` and `b`, on a char boundary.
fn common_suffix(a: &str, b: &str) -> usize {
    a.chars()
        .rev()
        .zip(b.chars().rev())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum()
}

/// What a key does to a snippet token at the cursor.
#[derive(Clone, Copy)]
enum TokenKey {
    /// Tab on or just after a token puts its text in place.
    Expand,
    /// A key that deletes back from the cursor removes a token it would cut.
    DeleteBack,
    /// A key that deletes forward from the cursor removes a token it would cut.
    DeleteForward,
    /// A word delete back (Ctrl+W, Alt+Backspace, Alt+H) takes every token its word reaches.
    WordBack,
    /// A word delete forward (Alt+D, Alt+Delete) takes every token its word reaches.
    WordForward,
    /// Ctrl+K from inside a token deletes from the token's start.
    DeleteToEnd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerAction {
    None,
    Submit(String),
    OpenEditor,
    Interrupt,
    /// Tab on a last word that starts with `@`, carrying the path typed after the `@`.
    CompletePath(String),
    /// The send key on a draft that owes a snippet whose whole token it no longer holds,
    /// carrying the token. The draft stays, since sending it would drop the paste.
    BrokenPaste(String),
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
    /// The slash menu's entries: the commands, then any skills the app has loaded.
    menu: Vec<MenuEntry>,
    /// The highlighted slash menu entry, as an index into `slash_matches`. Any change to the
    /// text puts it back on the first match.
    slash_selected: usize,
    /// The entries that complete each command's argument, by the command's name, which the
    /// app rebuilds as their lists change.
    arguments: BTreeMap<&'static str, Vec<MenuEntry>>,
    /// The text Up or Down last moved the highlight on, until the next edit. After a command
    /// and a space alone, the send key takes the highlighted argument only then.
    highlight_moved_on: Option<String>,
    /// Every large paste of the session, numbered from 1. None is dropped, so a token that
    /// comes back through the history, a restored draft, or `$EDITOR` still resolves.
    snippets: Vec<Snippet>,
    /// The newest snippet and when it was pasted, so pasting its text again expands it.
    last_paste: Option<(u32, Instant)>,
    /// What is left of the tokens edits have cut into plain text since the last send, so
    /// deleting all of what is left counts as removing the token.
    remnants: Vec<Remnant>,
    /// The snippets the draft owes: each one whose token it has held since the last send and
    /// that no explicit removal took out. A draft that owes a snippet whose whole token it no
    /// longer holds is not sent, whatever changed the token.
    owed: BTreeSet<u32>,
    /// `owed` for the draft set aside while the history is walked.
    draft_owed: BTreeSet<u32>,
    /// How many rows the token lookups have scanned, so a test can pin that a key looks only
    /// at the cursor's row.
    #[cfg(test)]
    rows_scanned: std::cell::Cell<usize>,
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

/// Whether a slash menu entry is a command whose usage names a required `<argument>`.
fn needs_argument(entry: &MenuEntry) -> bool {
    entry.kind == MenuKind::Command
        && COMMANDS.iter().any(|c| {
            entry.names.first().map(String::as_str) == Some(c.name) && c.usage.contains('<')
        })
}

/// Whether `key` submits the composer under `shortcut`, instead of inserting a newline.
/// `enhanced` reports whether the terminal distinguishes modified Enter keys (keyboard
/// enhancement); without it, Ctrl+Enter and Super+Enter both arrive as a plain Enter, so
/// Alt+Enter, which several terminals still report distinctly, substitutes for them in
/// modifier-send mode.
pub fn is_send_key(key: KeyEvent, shortcut: SendShortcut, enhanced: bool) -> bool {
    if key.code != KeyCode::Enter {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        return shortcut == SendShortcut::ModifierEnter && !enhanced;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let sup = key.modifiers.contains(KeyModifiers::SUPER);
    match shortcut {
        SendShortcut::Enter => !shift && !ctrl && !sup,
        SendShortcut::ModifierEnter => ctrl || sup,
    }
}

/// How the hint above the composer and `/help` name the key that sends the first queued
/// message now: the key `is_send_key` accepts on an empty composer under `shortcut`. In
/// modifier mode, Super (Cmd on macOS) and Ctrl both send with keyboard enhancement, and
/// without it Alt (Option on macOS) stands in, since both then arrive as a plain Enter.
/// `mac` picks the macOS key symbols.
pub fn send_now_label(shortcut: SendShortcut, enhanced: bool, mac: bool) -> &'static str {
    match (shortcut, enhanced, mac) {
        (SendShortcut::Enter, _, _) => "↵",
        (SendShortcut::ModifierEnter, true, true) => "⌘↵",
        (SendShortcut::ModifierEnter, true, false) => "Ctrl+↵",
        (SendShortcut::ModifierEnter, false, true) => "⌥↵",
        (SendShortcut::ModifierEnter, false, false) => "Alt+↵",
    }
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
            menu: skills::menu(&[], &[], None, None),
            slash_selected: 0,
            arguments: BTreeMap::new(),
            highlight_moved_on: None,
            snippets: Vec::new(),
            last_paste: None,
            remnants: Vec::new(),
            owed: BTreeSet::new(),
            draft_owed: BTreeSet::new(),
            #[cfg(test)]
            rows_scanned: std::cell::Cell::new(0),
        };
        composer.configure();
        composer
    }

    /// A fresh `TextArea` does not wrap and underlines the cursor's line, and `set_text` makes
    /// a fresh one, so both call this.
    fn configure(&mut self) {
        self.area.set_wrap_mode(WrapMode::WordOrGlyph);
        self.area.set_cursor_line_style(Style::default());
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

    /// Changes the most rows the composer grows to, as `/settings` does live.
    pub fn set_max_lines(&mut self, max: u16) {
        self.max_lines = max.max(1);
    }

    pub fn text(&self) -> String {
        self.area.lines().join("\n")
    }

    /// The last whitespace-separated word of the text, if the text ends inside it.
    fn last_word(&self) -> Option<String> {
        let text = self.text();
        if text.ends_with(char::is_whitespace) {
            return None;
        }
        text.split_whitespace().last().map(str::to_owned)
    }

    /// Replaces the last word of the text with `with`.
    pub fn replace_last_word(&mut self, with: &str) {
        let text = self.text();
        let keep = text.trim_end_matches(|c: char| !c.is_whitespace());
        self.set_text(&format!("{keep}{with}"));
    }

    pub fn set_text(&mut self, text: &str) {
        self.slash_selected = 0;
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
        // A token that comes back (the history, a restored draft, `$EDITOR`) is owed again; a
        // token the new text lost stays owed, since nothing removed it explicitly.
        self.owe_present(&text);
    }

    /// Owes every snippet whose whole token `text` holds; a blank draft owes nothing, since
    /// there is nothing to send.
    fn owe_present(&mut self, text: &str) {
        if text.trim().is_empty() {
            self.owed.clear();
            self.remnants.clear();
            return;
        }
        for s in &self.snippets {
            if text.contains(s.token()) {
                self.owed.insert(s.number);
            }
        }
    }

    /// Settles what the draft owes after an edit from `before`. An `explicit` edit, a token
    /// key or a word delete, removes the tokens it took whole; any other edit is tracked, and
    /// it removes a token only by deleting all of it, or all that is left of it.
    fn settle(&mut self, before: &str, explicit: bool) {
        let after = self.text();
        if before == after {
            return;
        }
        if explicit {
            for s in &self.snippets {
                if before.contains(s.token()) && !after.contains(s.token()) {
                    self.owed.remove(&s.number);
                }
            }
        } else {
            self.track_edit(before, &after);
        }
        self.owe_present(&after);
    }

    pub fn paste(&mut self, text: &str) {
        let before = self.text();
        self.insert_paste(text);
        self.settle(&before, false);
    }

    /// Inserts `text` at the cursor as typed.
    fn insert_paste(&mut self, text: &str) {
        self.area.insert_str(normalize_line_endings(text));
        self.slash_selected = 0;
        self.sync_gutter();
    }

    /// Inserts a bracketed paste that arrived at `now`, at local time `pasted`. A large one
    /// shows as its snippet's token, and the same text pasted again within `PASTE_AGAIN`
    /// expands that token in place.
    pub fn paste_at(&mut self, text: &str, now: Instant, pasted: PasteTime) {
        let text = normalize_line_endings(text);
        let before = self.text();
        if let Some((number, at)) = self.last_paste.take()
            && now.saturating_duration_since(at) < PASTE_AGAIN
            && self.snippet(number).is_some_and(|s| s.text == text)
            && self.expand(number)
        {
            self.settle(&before, true);
            return;
        }
        if !is_large_paste(&text) {
            self.paste(&text);
            return;
        }
        // Each snippet's file needs its own name, and the name counts seconds, so two pastes
        // in one second take the next one.
        let pasted = match self.snippets.last() {
            Some(last) if pasted.timestamp() <= last.pasted.timestamp() => {
                last.pasted + chrono::TimeDelta::seconds(1)
            }
            _ => pasted,
        };
        let snippet = Snippet::new(self.snippets.len() as u32 + 1, text, pasted);
        self.area.insert_str(snippet.token());
        self.last_paste = Some((snippet.number, now));
        self.snippets.push(snippet);
        self.slash_selected = 0;
        self.sync_gutter();
        // A token pasted inside another cuts that one.
        self.settle(&before, false);
    }

    /// Records the tokens the edit from `before` to `after` cut into plain text, and follows
    /// what is left of those an earlier edit cut. An edit is the one run of text between the
    /// two texts' common prefix and suffix. An edit that takes a whole token or a whole
    /// remnant removes it on purpose, and one that makes a token whole again, such as an
    /// undo, mends it.
    fn track_edit(&mut self, before: &str, after: &str) {
        if self.snippets.is_empty() || before == after {
            return;
        }
        let from = common_prefix(before, after);
        let suffix = common_suffix(&before[from..], &after[from..]);
        let (old_end, new_end) = (before.len() - suffix, after.len() - suffix);
        let insert = from == old_end;
        // Whether the edit reaches inside the bytes `start..end` of `before`.
        let touches = |start: usize, end: usize| {
            if insert {
                start < from && from < end
            } else {
                from < end && start < old_end
            }
        };
        let whole = |start: usize, end: usize| !insert && from <= start && end <= old_end;
        // What `start..end` of `before` became in `after`.
        let reshaped = |start: usize, end: usize| {
            let to = if end > old_end {
                end - old_end + new_end
            } else {
                new_end
            };
            after[start.min(from)..to].to_owned()
        };
        let mut remnants = Vec::new();
        let mut removed = Vec::new();
        for r in std::mem::take(&mut self.remnants) {
            let hit = before
                .match_indices(r.text.as_str())
                .map(|(start, piece)| (start, start + piece.len()))
                .find(|&(start, end)| touches(start, end));
            match hit {
                None => remnants.push(r),
                Some((start, end)) if whole(start, end) => {
                    removed.push(r.number);
                }
                Some((start, end)) => remnants.push(Remnant {
                    number: r.number,
                    text: reshaped(start, end),
                }),
            }
        }
        for snippet in &self.snippets {
            let token = snippet.token();
            // A token still there as often as before was not cut, whichever copy the common
            // prefix and suffix put the edit in.
            if after.matches(token).count() >= before.matches(token).count() {
                continue;
            }
            for (start, _) in before.match_indices(token) {
                let end = start + token.len();
                if whole(start, end) {
                    removed.push(snippet.number);
                } else if touches(start, end) {
                    remnants.push(Remnant {
                        number: snippet.number,
                        text: reshaped(start, end),
                    });
                }
            }
        }
        remnants.retain(|r| {
            !r.text.trim().is_empty() && self.snippet(r.number).is_none_or(|s| s.token() != r.text)
        });
        remnants.dedup();
        // A snippet goes once nothing of it is left: no whole token and no remnant.
        for number in removed {
            let token = self.snippet(number).map(|s| s.token().to_owned());
            if token.is_some_and(|t| !after.contains(&t))
                && !remnants.iter().any(|r| r.number == number)
            {
                self.owed.remove(&number);
            }
        }
        self.remnants = remnants;
    }

    /// The token of the first snippet the draft owes whose whole token `text` no longer
    /// holds, so sending `text` would leave that paste out.
    fn broken_token(&self, text: &str) -> Option<String> {
        self.owed
            .iter()
            .filter_map(|&n| self.snippet(n))
            .find(|s| !text.contains(s.token()))
            .map(|s| s.token().to_owned())
    }

    fn snippet(&self, number: u32) -> Option<&Snippet> {
        self.snippets.iter().find(|s| s.number == number)
    }

    /// The snippet tokens on `row`, in order: the first char column, the length in chars, and
    /// the snippet's number. Matching is by text, so a token resolves wherever it comes back
    /// (the history, a restored draft, `$EDITOR`). Two side effects follow: a snippet whose
    /// own text holds another live token makes that text a live token once expanded, and a
    /// draft recalled after a failed send carries a paste the core already put back as a
    /// chip, which `Msg::AttachPaste` then takes only once.
    fn row_tokens(&self, row: usize) -> Vec<(usize, usize, u32)> {
        #[cfg(test)]
        self.rows_scanned.set(self.rows_scanned.get() + 1);
        let Some(line) = self.area.lines().get(row) else {
            return Vec::new();
        };
        let mut found: Vec<(usize, usize, u32)> = self
            .snippets
            .iter()
            .flat_map(|s| {
                let len = s.token().chars().count();
                token_starts(line, s.token())
                    .into_iter()
                    .map(move |start| (start, len, s.number))
            })
            .collect();
        found.sort();
        found
    }

    /// Applies `key` when it acts on a snippet token at the cursor: Tab expands the token, and
    /// a key that would delete part of one deletes all of it, so no half token is ever sent
    /// as typed text with its paste dropped. Whether the key was taken.
    fn token_key(&mut self, key: KeyEvent) -> bool {
        if self.snippets.is_empty() {
            return false;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // These mirror ratatui-textarea's own delete bindings; Ctrl+Alt is AltGr, a character.
        let kind = match key.code {
            KeyCode::Tab => TokenKey::Expand,
            KeyCode::Backspace if alt && !ctrl => TokenKey::WordBack,
            KeyCode::Backspace => TokenKey::DeleteBack,
            KeyCode::Char('w') if ctrl && !alt => TokenKey::WordBack,
            KeyCode::Char('h') if ctrl && !alt => TokenKey::DeleteBack,
            KeyCode::Char('h') if alt && !ctrl => TokenKey::WordBack,
            KeyCode::Delete if alt && !ctrl => TokenKey::WordForward,
            KeyCode::Delete => TokenKey::DeleteForward,
            KeyCode::Char('d') if alt && !ctrl => TokenKey::WordForward,
            KeyCode::Char('d') if ctrl && !alt => TokenKey::DeleteForward,
            KeyCode::Char('k') if ctrl && !alt => TokenKey::DeleteToEnd,
            _ => return false,
        };
        if matches!(kind, TokenKey::WordBack | TokenKey::WordForward) {
            return self.word_delete(key);
        }
        let cursor = self.area.cursor();
        let (row, col) = (cursor.0, cursor.1);
        let hit = self
            .row_tokens(row)
            .into_iter()
            .find(|&(start, len, _)| match kind {
                TokenKey::Expand => (start..=start + len).contains(&col),
                TokenKey::DeleteBack => start < col && col <= start + len,
                TokenKey::DeleteForward => start <= col && col < start + len,
                TokenKey::DeleteToEnd => start < col && col < start + len,
                TokenKey::WordBack | TokenKey::WordForward => false,
            });
        let Some((start, len, number)) = hit else {
            return false;
        };
        match kind {
            TokenKey::Expand => self.expand_token(row, start, len, number),
            TokenKey::DeleteBack | TokenKey::DeleteForward => self.replace(row, start, len, ""),
            TokenKey::WordBack | TokenKey::WordForward => false,
            TokenKey::DeleteToEnd => {
                if let Ok(start) = u16::try_from(start)
                    && let Ok(row) = u16::try_from(row)
                {
                    self.area.move_cursor(CursorMove::Jump(row, start));
                }
                self.area.delete_line_by_end();
                true
            }
        }
    }

    /// Applies the word delete `key` on a row that holds a token, widened to every token the
    /// deleted word reaches into, so the token goes whole, with the whitespace between it and
    /// the cursor. Whether the key was taken; a selection, or a row without a token, is left
    /// to the widget.
    fn word_delete(&mut self, key: KeyEvent) -> bool {
        if self.area.selection_range().is_some() {
            return false;
        }
        let row = self.area.cursor().0;
        let tokens = self.row_tokens(row);
        if tokens.is_empty() {
            return false;
        }
        let chars = |c: &Composer| c.area.lines()[row].chars().count();
        let (rows, width) = (self.area.lines().len(), chars(self));
        self.area.input(key);
        // A delete across a line break joins two lines and cuts no token.
        if self.area.lines().len() != rows {
            return true;
        }
        // The word went from `from` to `to`; the cursor sits at `from` either way.
        let from = self.area.cursor().1;
        let to = from + width.saturating_sub(chars(self));
        let (start, end) = tokens
            .iter()
            .filter(|&&(start, len, _)| start < to && from < start + len)
            .fold((from, to), |(lo, hi), &(start, len, _)| {
                (lo.min(start), hi.max(start + len))
            });
        // Widened, the delete is redone as one edit, so one undo brings every token back.
        if (start, end) != (from, to)
            && let (Ok(row), Ok(at)) = (u16::try_from(row), u16::try_from(start))
        {
            self.area.undo();
            self.area.move_cursor(CursorMove::Jump(row, at));
            self.area.delete_str(end - start);
        }
        true
    }

    /// Replaces the first token of snippet `number` with its text; whether there was one.
    fn expand(&mut self, number: u32) -> bool {
        let rows = self.area.lines().len();
        let hit = (0..rows).find_map(|row| {
            self.row_tokens(row)
                .into_iter()
                .find(|t| t.2 == number)
                .map(|(start, len, _)| (row, start, len))
        });
        match hit {
            Some((row, start, len)) => self.expand_token(row, start, len, number),
            None => false,
        }
    }

    /// Replaces the `len`-char token of snippet `number` at column `start` of `row` with the
    /// snippet's text.
    fn expand_token(&mut self, row: usize, start: usize, len: usize, number: u32) -> bool {
        let Some(text) = self.snippet(number).map(|s| s.text.clone()) else {
            return false;
        };
        self.replace(row, start, len, &text)
    }

    /// Puts `with` in place of the `len` chars from column `start` of `row`, leaving the cursor
    /// after it.
    fn replace(&mut self, row: usize, start: usize, len: usize, with: &str) -> bool {
        let (Ok(row), Ok(start)) = (u16::try_from(row), u16::try_from(start)) else {
            return false;
        };
        self.area.move_cursor(CursorMove::Jump(row, start));
        self.area.delete_str(len);
        self.area.insert_str(with);
        self.slash_selected = 0;
        self.sync_gutter();
        true
    }

    /// The text with every snippet token replaced by its text, as Ctrl+O copies it.
    pub fn expanded_text(&self) -> String {
        self.expand_tokens(&self.text())
    }

    /// `text` with every snippet token replaced by its snippet's text, for a command's
    /// argument, which is not a message and so carries no file.
    pub fn expand_tokens(&self, text: &str) -> String {
        self.snippets
            .iter()
            .fold(text.to_owned(), |t, s| t.replace(s.token(), &s.text))
    }

    /// `text` without its snippet tokens, trimmed, and the snippets they stand for, each once,
    /// in the order they first appear, for a message that sends them as text files. A token
    /// between words leaves one space, and a line that held only tokens goes, along with a
    /// blank line it would leave doubled.
    pub fn split_snippets(&self, text: &str) -> (String, Vec<Snippet>) {
        let mut found: Vec<(usize, &Snippet)> = self
            .snippets
            .iter()
            .filter_map(|s| text.find(s.token()).map(|at| (at, s)))
            .collect();
        found.sort_by_key(|(at, _)| *at);
        let mut kept: Vec<String> = Vec::new();
        let mut skip_blank = false;
        for line in text.split('\n') {
            let mut line = line.to_owned();
            let mut cut = false;
            for (_, s) in &found {
                while let Some(at) = line.find(s.token()) {
                    line = cut_token(&line, at, s.token());
                    cut = true;
                }
            }
            if cut && line.trim().is_empty() {
                skip_blank = kept.last().is_some_and(|l| l.trim().is_empty());
                continue;
            }
            if std::mem::take(&mut skip_blank) && line.trim().is_empty() {
                continue;
            }
            kept.push(line);
        }
        (
            kept.join("\n").trim().to_owned(),
            found.into_iter().map(|(_, s)| s.clone()).collect(),
        )
    }

    /// Leaves history browsing without touching the text being typed.
    pub fn reset_history_position(&mut self) {
        self.history_pos = None;
        self.draft.clear();
        self.draft_owed.clear();
    }

    /// The rows the text takes at `width` columns (the composer's full inner width, gutter
    /// included), never more than `max_lines`, plus the rules above and below it.
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

    /// Replaces the slash menu, which the app rebuilds when skills or the user load.
    pub fn set_menu(&mut self, menu: Vec<MenuEntry>) {
        self.menu = menu;
    }

    /// Replaces the entries that complete `command`'s argument, keeping the highlight on the
    /// same entry, by value, while it is still listed.
    pub fn set_arguments(&mut self, command: &'static str, entries: Vec<MenuEntry>) {
        let held = self
            .slash_matches()
            .get(self.slash_selected)
            .map(|e| e.insert.clone())
            .filter(|insert| !insert.is_empty());
        self.arguments.insert(command, entries);
        let found =
            held.and_then(|held| self.slash_matches().iter().position(|e| e.insert == held));
        if let Some(i) = found {
            self.slash_selected = i;
        } else {
            // The entry the highlight was on is gone, so the send key must not take whichever
            // entry now sits first as if it had been picked.
            self.slash_selected = 0;
            self.highlight_moved_on = None;
        }
    }

    /// The slash menu entries the text matches: while it is a single word starting with `/`,
    /// the commands and skills; after a command whose argument the menu completes and a
    /// space, that command's argument entries.
    pub fn slash_matches(&self) -> Vec<&MenuEntry> {
        let text = self.text();
        if let Some(query) = skills::argument_query(&text) {
            return self
                .arguments
                .get(query.command)
                .map(|entries| skills::argument_matches(entries, query.partial))
                .unwrap_or_default();
        }
        if !text.starts_with('/') || text.contains(char::is_whitespace) {
            return Vec::new();
        }
        skills::matches(&self.menu, &text)
    }

    /// The highlighted slash menu entry, as an index into `slash_matches`.
    pub fn slash_selected(&self) -> usize {
        self.slash_selected
    }

    /// The slash menu entry Tab and Enter take: the highlighted one, else the first that
    /// inserts anything; a note line, such as "Loading skills…", inserts nothing.
    fn menu_choice(&self) -> Option<&MenuEntry> {
        let matches = self.slash_matches();
        matches
            .get(self.slash_selected)
            .copied()
            .filter(|e| !e.insert.is_empty())
            .or_else(|| matches.iter().copied().find(|e| !e.insert.is_empty()))
    }

    /// What Tab puts in the composer for the menu's choice: the entry's insert, or after a
    /// command and a space, the command as typed with the entry's argument after it.
    fn completion(&self) -> Option<String> {
        let insert = self.menu_choice()?.insert.clone();
        let text = self.text();
        Some(match skills::argument_query(&text) {
            Some(query) => format!("{} {insert}", query.typed),
            None => insert,
        })
    }

    /// Moves the highlight one entry up or down, past any note line, and stays put when no
    /// entry that inserts something lies that way.
    fn move_highlight(&mut self, down: bool) {
        let matches = self.slash_matches();
        let selectable = |i: &usize| !matches[*i].insert.is_empty();
        let next = if down {
            (self.slash_selected + 1..matches.len()).find(selectable)
        } else {
            (0..self.slash_selected).rev().find(selectable)
        };
        if let Some(next) = next {
            self.slash_selected = next;
            self.highlight_moved_on = Some(self.text());
        }
    }

    /// Whether Up and Down move the slash menu's highlight: the menu shows, and they are not
    /// walking the sent-message history, which may pass through a sent command.
    fn menu_keys(&self) -> bool {
        self.history_pos.is_none() && !self.slash_matches().is_empty()
    }

    fn submit(&mut self) -> ComposerAction {
        let text = self.text();
        if text.trim().is_empty() {
            return ComposerAction::None;
        }
        if let Some(token) = self.broken_token(&text) {
            return ComposerAction::BrokenPaste(token);
        }
        self.history.push(text.clone());
        self.history_pos = None;
        self.draft.clear();
        self.draft_owed.clear();
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
                self.draft_owed = std::mem::take(&mut self.owed);
                Some(self.history.len() - 1)
            }
            (Some(0), true) => Some(0),
            (Some(i), true) => Some(i - 1),
            (Some(i), false) if i + 1 < self.history.len() => Some(i + 1),
            (Some(_), false) => None,
            (None, false) => return,
        };
        self.history_pos = next;
        // A sent message owes what it holds; the draft gets back what it owed.
        self.owed = match next {
            Some(_) => BTreeSet::new(),
            None => std::mem::take(&mut self.draft_owed),
        };
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

    /// Whether the cursor is at the end of its line, as it always is on an empty composer.
    pub fn at_line_end(&self) -> bool {
        let cursor = self.area.cursor();
        self.area
            .lines()
            .get(cursor.0)
            .is_none_or(|line| cursor.1 >= line.chars().count())
    }

    pub fn handle_key(&mut self, key: KeyEvent, shortcut: SendShortcut) -> ComposerAction {
        let before = self.text();
        // A snippet token takes Tab, which inside a message's text has no other use (it
        // completes only a lone `/command` or an `@path` last word), and every delete key that
        // would cut it. Those keys take whole tokens, so only the other keys are tracked.
        let (action, tracked) = if self.token_key(key) {
            (ComposerAction::None, false)
        } else {
            (self.key_action(key, shortcut), true)
        };
        if self.text() != before {
            self.slash_selected = 0;
            self.highlight_moved_on = None;
            self.settle(&before, !tracked);
        }
        self.sync_gutter();
        action
    }

    fn key_action(&mut self, key: KeyEvent, shortcut: SendShortcut) -> ComposerAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let sup = key.modifiers.contains(KeyModifiers::SUPER);
        match key.code {
            KeyCode::Enter => {
                if is_send_key(key, shortcut, self.enhanced) {
                    // After a command and a space, the send key runs the command with the
                    // highlighted argument once part of one is typed or the highlight moved;
                    // with nothing typed after the space, it runs the command bare, as before.
                    let text = self.text();
                    if let Some(query) = skills::argument_query(&text) {
                        let picked = !query.partial.is_empty()
                            || self.highlight_moved_on.as_deref() == Some(text.as_str());
                        if picked && let Some(done) = self.completion() {
                            self.set_text(&done);
                        }
                        return self.submit();
                    }
                    // On a bare command prefix, the send key takes the menu's entry: a
                    // command that needs an argument completes as Tab does, anything else runs.
                    if let Some((insert, needs_argument)) = self
                        .menu_choice()
                        .map(|e| (e.insert.clone(), needs_argument(e)))
                    {
                        if needs_argument {
                            self.set_text(&insert);
                            return ComposerAction::None;
                        }
                        self.set_text(insert.trim_end());
                    }
                    return self.submit();
                }
                self.area.insert_newline();
            }
            KeyCode::Char('j') if ctrl => self.area.insert_newline(),
            KeyCode::Char('g') if ctrl => return ComposerAction::OpenEditor,
            KeyCode::Esc if !self.slash_matches().is_empty() => self.set_text(""),
            KeyCode::Esc => return ComposerAction::Interrupt,
            KeyCode::Tab => {
                if let Some(done) = self.completion() {
                    self.set_text(&done);
                } else if let Some(path) = self
                    .last_word()
                    .and_then(|w| w.strip_prefix('@').map(str::to_owned))
                {
                    return ComposerAction::CompletePath(path);
                }
            }
            KeyCode::Up if self.menu_keys() => self.move_highlight(false),
            KeyCode::Down if self.menu_keys() => self.move_highlight(true),
            KeyCode::Up if self.area.screen_cursor().row == 0 => self.recall(true),
            KeyCode::Down if self.history_pos.is_some() && self.on_last_row() => self.recall(false),
            // AltGr on several European layouts reports as Ctrl+Alt; treat it as a literal
            // character rather than the widget's few Ctrl+Alt navigation bindings.
            KeyCode::Char(c) if ctrl && alt => self.area.insert_char(c),
            // TextArea::input only inserts a Char when Alt is not held, and otherwise
            // treats Alt+<letter> as an emacs-style editing or movement shortcut. Insert
            // directly unless `c` is one of those reserved shortcut keys.
            KeyCode::Char(c) if alt && !ctrl && !is_alt_reserved(c) => self.area.insert_char(c),
            // Cmd+Left and Cmd+Right arrive as Super+Left and Super+Right; the widget drops
            // Super and would move one character, so they go to the ends of the line here,
            // selecting on the way with Shift, as Shift+Home and Shift+End do.
            KeyCode::Left | KeyCode::Right if sup => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    if !self.area.is_selecting() {
                        self.area.start_selection();
                    }
                } else {
                    self.area.cancel_selection();
                }
                self.area.move_cursor(if key.code == KeyCode::Left {
                    CursorMove::Head
                } else {
                    CursorMove::End
                });
            }
            // The widget reads Ctrl+Home and Ctrl+End as Home and End, so the ends of the
            // whole message are reached here.
            KeyCode::Home if ctrl => {
                self.area.move_cursor(CursorMove::Top);
                self.area.move_cursor(CursorMove::Head);
            }
            KeyCode::End if ctrl => {
                self.area.move_cursor(CursorMove::Bottom);
                self.area.move_cursor(CursorMove::End);
            }
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
    fn is_send_key_excludes_super_in_enter_mode_and_includes_it_in_modifier_mode() {
        assert!(!is_send_key(
            key(KeyCode::Enter, KeyModifiers::SUPER),
            SendShortcut::Enter,
            true
        ));
        assert!(is_send_key(
            key(KeyCode::Enter, KeyModifiers::SUPER),
            SendShortcut::ModifierEnter,
            true
        ));
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
    fn tab_on_an_at_word_asks_for_path_completion() {
        let mut c = Composer::new(10);
        type_str(&mut c, "look at @src/ma");
        assert_eq!(
            c.handle_key(key(KeyCode::Tab, KeyModifiers::NONE), SendShortcut::Enter),
            ComposerAction::CompletePath("src/ma".into())
        );
        c.replace_last_word("@src/main.rs");
        assert_eq!(c.text(), "look at @src/main.rs");
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
    fn no_line_is_underlined_while_typing() {
        use ratatui::style::Modifier;
        let underlined = |c: &Composer| {
            let mut term = Terminal::new(TestBackend::new(30, 3)).unwrap();
            term.draw(|f| f.render_widget(c.widget(), f.area()))
                .unwrap();
            let buf = term.backend().buffer().clone();
            (0..3u16)
                .flat_map(|y| (0..30u16).map(move |x| (x, y)))
                .filter(|&at| buf[at].modifier.contains(Modifier::UNDERLINED))
                .count()
        };
        let mut c = Composer::new(10);
        type_str(&mut c, "the line being typed");
        assert_eq!(underlined(&c), 0);
        c.set_text("a fresh text area\nwith two lines");
        assert_eq!(
            underlined(&c),
            0,
            "set_text builds a new TextArea, and it is cleared too"
        );
        assert_eq!(c.widget().cursor_line_style(), Style::default());
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

    fn press(c: &mut Composer, code: KeyCode) -> ComposerAction {
        c.handle_key(key(code, KeyModifiers::NONE), SendShortcut::Enter)
    }

    #[test]
    fn up_and_down_move_through_the_slash_menu_and_tab_takes_the_highlighted_entry() {
        let mut c = Composer::new(10);
        type_str(&mut c, "/");
        assert_eq!(c.slash_selected(), 0);
        press(&mut c, KeyCode::Down);
        press(&mut c, KeyCode::Down);
        assert_eq!(c.slash_selected(), 2);
        press(&mut c, KeyCode::Up);
        assert_eq!(c.slash_selected(), 1);
        press(&mut c, KeyCode::Up);
        press(&mut c, KeyCode::Up);
        assert_eq!(c.slash_selected(), 0, "Up stops at the first entry");
        let last = c.slash_matches().len() - 1;
        for _ in 0..last + 3 {
            press(&mut c, KeyCode::Down);
        }
        assert_eq!(c.slash_selected(), last, "Down stops at the last entry");
        press(&mut c, KeyCode::Up);
        let wanted = c.slash_matches()[last - 1].insert.clone();
        press(&mut c, KeyCode::Tab);
        assert_eq!(c.text(), wanted);
    }

    #[test]
    fn narrowing_the_slash_filter_after_scrolling_starts_over_at_the_first_match() {
        let mut c = Composer::new(10);
        type_str(&mut c, "/");
        for _ in 0..8 {
            press(&mut c, KeyCode::Down);
        }
        assert_eq!(c.slash_selected(), 8);
        type_str(&mut c, "c");
        assert_eq!(c.slash_selected(), 0, "typing starts the highlight over");
        assert!(c.slash_matches().len() < 8);
        press(&mut c, KeyCode::Tab);
        assert_eq!(c.text(), "/chats ");
    }

    #[test]
    fn up_and_down_keep_walking_the_history_through_a_sent_slash_command() {
        let mut c = Composer::new(10);
        for sent in ["first", "/model", "last"] {
            type_str(&mut c, sent);
            press(&mut c, KeyCode::Enter);
        }
        press(&mut c, KeyCode::Up);
        assert_eq!(c.text(), "last");
        press(&mut c, KeyCode::Up);
        assert_eq!(c.text(), "/model");
        assert!(!c.slash_matches().is_empty(), "the menu shows for it");
        press(&mut c, KeyCode::Up);
        assert_eq!(
            c.text(),
            "first",
            "Up keeps recalling instead of moving the menu"
        );
    }

    #[test]
    fn enter_on_a_command_prefix_runs_the_highlighted_entry() {
        let mut c = Composer::new(10);
        type_str(&mut c, "/mo");
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit("/model".into())
        );
        assert_eq!(c.text(), "");
    }

    #[test]
    fn enter_on_a_command_that_needs_an_argument_completes_it() {
        let mut c = Composer::new(10);
        type_str(&mut c, "/attach");
        assert_eq!(press(&mut c, KeyCode::Enter), ComposerAction::None);
        assert_eq!(c.text(), "/attach ");
    }

    #[test]
    fn enter_after_a_space_sends_the_text_unchanged() {
        let mut c = Composer::new(10);
        type_str(&mut c, "/effort high");
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit("/effort high".into())
        );
    }

    #[test]
    fn enter_runs_the_entry_up_and_down_highlighted() {
        let mut c = Composer::new(10);
        type_str(&mut c, "/co");
        press(&mut c, KeyCode::Down);
        let wanted = c.slash_matches()[1].insert.trim_end().to_owned();
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit(wanted)
        );
    }

    #[test]
    fn enter_runs_a_skill_from_the_menu() {
        let mut c = Composer::new(10);
        let skill = skills::Skill {
            name: "deploy-docs".into(),
            description: "Ship the docs".into(),
        };
        c.set_menu(skills::menu(&[skill], &[], None, None));
        type_str(&mut c, "/deploy");
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit("/deploy-docs".into())
        );
    }

    #[test]
    fn enter_on_the_exact_name_of_a_skill_runs_that_skill_not_a_longer_command() {
        let mut c = Composer::new(10);
        let skill = |name: &str| skills::Skill {
            name: name.into(),
            description: String::new(),
        };
        c.set_menu(skills::menu(
            &[skill("plan"), skill("newsletter")],
            &[],
            None,
            None,
        ));
        type_str(&mut c, "/plan");
        assert_eq!(c.slash_matches()[c.slash_selected()].label, "/plan");
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit("/plan".into()),
            "a skill whose name prefixes /plan-mode"
        );
        type_str(&mut c, "/new");
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit("/new".into()),
            "a command whose name prefixes the newsletter skill"
        );
    }

    #[test]
    fn the_skills_note_row_is_never_highlighted_or_taken() {
        let mut c = Composer::new(10);
        let skill = skills::Skill {
            name: "deploy".into(),
            description: String::new(),
        };
        c.set_menu(skills::menu(
            &[skill],
            &[],
            None,
            Some("Skills are unavailable: boom"),
        ));
        type_str(&mut c, "/");
        let matches = c.slash_matches().len();
        assert_eq!(
            c.slash_matches()[matches - 1].kind,
            MenuKind::Note,
            "the note is the last row"
        );
        for _ in 0..matches + 3 {
            press(&mut c, KeyCode::Down);
        }
        assert_eq!(
            c.slash_selected(),
            matches - 2,
            "Down stops on the last entry that inserts something"
        );
        press(&mut c, KeyCode::Tab);
        assert_eq!(c.text(), "/deploy ");
        c.set_text("/");
        for _ in 0..matches + 3 {
            press(&mut c, KeyCode::Down);
        }
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit("/deploy".into())
        );
    }

    #[test]
    fn a_slash_typed_during_a_history_walk_leaves_up_and_down_on_the_history() {
        let mut c = Composer::new(10);
        for sent in ["first", "second"] {
            type_str(&mut c, sent);
            press(&mut c, KeyCode::Enter);
        }
        press(&mut c, KeyCode::Up);
        assert_eq!(c.text(), "second");
        for _ in 0.."second".len() {
            press(&mut c, KeyCode::Backspace);
        }
        type_str(&mut c, "/");
        assert!(!c.slash_matches().is_empty(), "the menu shows for it");
        press(&mut c, KeyCode::Down);
        assert_eq!(c.slash_selected(), 0, "Down does not move the menu");
        assert_eq!(c.text(), "", "Down walks back to the empty draft");
        type_str(&mut c, "/");
        press(&mut c, KeyCode::Down);
        assert_eq!(c.slash_selected(), 1, "after the walk, Down moves the menu");
    }

    #[test]
    fn the_send_now_label_names_a_key_that_sends() {
        let cases = [
            (SendShortcut::Enter, true, true, "↵", KeyModifiers::NONE),
            (SendShortcut::Enter, true, false, "↵", KeyModifiers::NONE),
            (SendShortcut::Enter, false, true, "↵", KeyModifiers::NONE),
            (SendShortcut::Enter, false, false, "↵", KeyModifiers::NONE),
            (
                SendShortcut::ModifierEnter,
                true,
                true,
                "⌘↵",
                KeyModifiers::SUPER,
            ),
            (
                SendShortcut::ModifierEnter,
                true,
                false,
                "Ctrl+↵",
                KeyModifiers::CONTROL,
            ),
            (
                SendShortcut::ModifierEnter,
                false,
                true,
                "⌥↵",
                KeyModifiers::ALT,
            ),
            (
                SendShortcut::ModifierEnter,
                false,
                false,
                "Alt+↵",
                KeyModifiers::ALT,
            ),
        ];
        for (shortcut, enhanced, mac, label, mods) in cases {
            assert_eq!(
                send_now_label(shortcut, enhanced, mac),
                label,
                "{shortcut:?}, enhanced {enhanced}, macOS {mac}"
            );
            assert!(
                is_send_key(key(KeyCode::Enter, mods), shortcut, enhanced),
                "{label} must be a send key under {shortcut:?}, enhanced {enhanced}"
            );
        }
    }

    /// When the tests' pastes arrive: 1,700,000,000 seconds after the Unix epoch, in UTC.
    fn pasted() -> PasteTime {
        chrono::DateTime::from_timestamp(1_700_000_000, 0)
            .unwrap()
            .fixed_offset()
    }

    /// A paste that becomes a snippet: twelve numbered lines.
    fn big() -> String {
        (1..=12).map(|i| format!("log line {i}\n")).collect()
    }

    #[test]
    fn a_large_paste_becomes_a_token_and_a_small_one_goes_in_as_typed() {
        let now = Instant::now();
        let mut c = Composer::new(10);
        c.paste_at("two\nlines", now, pasted());
        assert_eq!(c.text(), "two\nlines");
        c.set_text("");
        c.paste_at(&big(), now, pasted());
        assert_eq!(c.text(), "[Pasted text #1 +12 lines]");
        c.paste_at(&"x".repeat(PASTE_CHARS), now + PASTE_AGAIN, pasted());
        assert_eq!(
            c.text(),
            "[Pasted text #1 +12 lines][Pasted text #2 +1000 chars]"
        );
        assert_eq!(c.height(80), 3, "the tokens take one row");
        assert!(
            is_large_paste(&"a\n".repeat(9)),
            "nine newlines make ten lines, as in the web UI"
        );
        assert!(!is_large_paste(&"a\n".repeat(8)));
        assert!(!is_large_paste(&"x".repeat(PASTE_CHARS - 1)));
    }

    #[test]
    fn the_same_paste_again_right_away_expands_its_token_and_later_adds_another() {
        let now = Instant::now();
        let mut c = Composer::new(10);
        c.paste_at(&big(), now, pasted());
        c.paste_at(&big(), now + Duration::from_millis(500), pasted());
        assert_eq!(c.text(), big(), "the second paste expanded the first");
        let mut c = Composer::new(10);
        c.paste_at(&big(), now, pasted());
        c.paste_at(&big(), now + PASTE_AGAIN, pasted());
        assert_eq!(
            c.text(),
            "[Pasted text #1 +12 lines][Pasted text #2 +12 lines]"
        );
    }

    #[test]
    fn tab_on_a_token_expands_it_and_backspace_after_it_removes_it() {
        let now = Instant::now();
        let mut c = Composer::new(10);
        type_str(&mut c, "see ");
        c.paste_at(&big(), now, pasted());
        type_str(&mut c, " please");
        assert_eq!(press(&mut c, KeyCode::Tab), ComposerAction::None);
        assert_eq!(
            c.text(),
            "see [Pasted text #1 +12 lines] please",
            "Tab away from the token does nothing"
        );
        for _ in 0.." please".len() {
            press(&mut c, KeyCode::Left);
        }
        press(&mut c, KeyCode::Tab);
        assert_eq!(c.text(), format!("see {} please", big()));
        let mut c = Composer::new(10);
        type_str(&mut c, "see ");
        c.paste_at(&big(), now, pasted());
        press(&mut c, KeyCode::Backspace);
        assert_eq!(c.text(), "see ", "one Backspace removes the whole token");
        press(&mut c, KeyCode::Backspace);
        assert_eq!(c.text(), "see");
    }

    #[test]
    fn a_sent_draft_splits_into_text_and_snippets_and_a_command_takes_them_inline() {
        let now = Instant::now();
        let mut c = Composer::new(10);
        type_str(&mut c, "compare ");
        c.paste_at(&big(), now, pasted());
        type_str(&mut c, " and ");
        c.paste_at("short\nlog", now, pasted());
        type_str(&mut c, " ");
        c.paste_at(&"y".repeat(PASTE_CHARS), now, pasted());
        let (message, snippets) = c.split_snippets(&c.text());
        assert_eq!(message, "compare and short\nlog");
        assert_eq!(
            snippets.iter().map(Snippet::file_name).collect::<Vec<_>>(),
            [
                "pasted-text-2023-11-14-22-13-20.txt",
                "pasted-text-2023-11-14-22-13-21.txt"
            ]
        );
        assert_eq!(snippets[0].text, big());
        assert_eq!(
            c.expand_tokens("/title [Pasted text #2 +1000 chars]"),
            format!("/title {}", "y".repeat(PASTE_CHARS))
        );
        assert_eq!(
            c.expanded_text(),
            format!(
                "compare {} and short\nlog {}",
                big(),
                "y".repeat(PASTE_CHARS)
            )
        );
    }

    #[test]
    fn a_token_still_resolves_after_a_history_recall_and_set_text() {
        let now = Instant::now();
        let mut c = Composer::new(10);
        type_str(&mut c, "see ");
        c.paste_at(&big(), now, pasted());
        assert!(matches!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit(_)
        ));
        press(&mut c, KeyCode::Up);
        assert_eq!(c.text(), "see [Pasted text #1 +12 lines]");
        assert_eq!(c.expanded_text(), format!("see {}", big()));
        c.set_text("edited in $EDITOR [Pasted text #1 +12 lines]");
        assert_eq!(c.split_snippets(&c.text()).1.len(), 1);
    }

    /// "see " and a token, with the cursor just after the token.
    fn see_token(now: Instant) -> Composer {
        let mut c = Composer::new(10);
        type_str(&mut c, "see ");
        c.paste_at(&big(), now, pasted());
        c
    }

    /// Asserts the draft lost the whole token, never part of it, so no broken token can be
    /// sent as typed text while its paste is dropped.
    fn assert_whole_token_gone(c: &Composer, expected: &str) {
        assert_eq!(c.text(), expected);
        assert!(
            !c.text().contains("Pasted") && !c.text().contains(']'),
            "a piece of the token is left: {:?}",
            c.text()
        );
    }

    #[test]
    fn ctrl_w_after_a_token_removes_the_whole_token() {
        let mut c = see_token(Instant::now());
        c.handle_key(
            key(KeyCode::Char('w'), KeyModifiers::CONTROL),
            SendShortcut::Enter,
        );
        assert_whole_token_gone(&c, "see ");
    }

    #[test]
    fn alt_backspace_after_a_token_removes_the_whole_token() {
        let mut c = see_token(Instant::now());
        c.handle_key(
            key(KeyCode::Backspace, KeyModifiers::ALT),
            SendShortcut::Enter,
        );
        assert_whole_token_gone(&c, "see ");
    }

    #[test]
    fn delete_at_a_tokens_start_removes_the_whole_token() {
        let mut c = see_token(Instant::now());
        type_str(&mut c, " please");
        let token_and_tail = "[Pasted text #1 +12 lines] please".chars().count();
        for _ in 0..token_and_tail {
            press(&mut c, KeyCode::Left);
        }
        press(&mut c, KeyCode::Delete);
        assert_whole_token_gone(&c, "see  please");
    }

    #[test]
    fn ctrl_d_ctrl_h_and_ctrl_k_never_cut_a_token_either() {
        let mut c = see_token(Instant::now());
        c.handle_key(
            key(KeyCode::Char('h'), KeyModifiers::CONTROL),
            SendShortcut::Enter,
        );
        assert_whole_token_gone(&c, "see ");
        let mut c = see_token(Instant::now());
        press(&mut c, KeyCode::Home);
        for _ in 0.."see ".len() {
            press(&mut c, KeyCode::Right);
        }
        c.handle_key(
            key(KeyCode::Char('d'), KeyModifiers::CONTROL),
            SendShortcut::Enter,
        );
        assert_whole_token_gone(&c, "see ");
        let mut c = see_token(Instant::now());
        for _ in 0..5 {
            press(&mut c, KeyCode::Left);
        }
        c.handle_key(
            key(KeyCode::Char('k'), KeyModifiers::CONTROL),
            SendShortcut::Enter,
        );
        assert_whole_token_gone(&c, "see ");
    }

    #[test]
    fn a_key_reuses_the_built_tokens_and_scans_only_the_cursors_row() {
        let now = Instant::now();
        let mut c = Composer::new(10);
        for i in 0..5u32 {
            c.paste_at(&big(), now + PASTE_AGAIN * (i + 1), pasted());
        }
        let rows = vec!["log line"; 2000].join("\n");
        c.set_text(&format!("{}\n{rows}", c.text()));
        TOKEN_BUILDS.with(|n| n.set(0));
        c.rows_scanned.set(0);
        press(&mut c, KeyCode::Backspace);
        assert_eq!(c.rows_scanned.get(), 1, "Backspace looked at one row");
        press(&mut c, KeyCode::Tab);
        assert_eq!(c.rows_scanned.get(), 2, "Tab looked at one row");
        assert_eq!(TOKEN_BUILDS.with(|n| n.get()), 0, "no token was rebuilt");
        assert!(c.text().ends_with("log lin"));
    }

    #[test]
    fn the_character_threshold_counts_utf16_code_units_as_the_web_ui_does() {
        assert!(
            is_large_paste(&"😀".repeat(PASTE_CHARS / 2)),
            "an emoji counts two, as in JavaScript"
        );
        assert!(!is_large_paste(&"é".repeat(PASTE_CHARS - 1)));
    }

    #[test]
    fn a_removed_token_leaves_no_doubled_space_or_blank_line() {
        let now = Instant::now();
        let mut c = Composer::new(10);
        c.set_text("intro\n\n");
        c.paste_at(&big(), now, pasted());
        c.paste("\n\noutro and ");
        c.paste_at(&"z".repeat(PASTE_CHARS), now + PASTE_AGAIN, pasted());
        type_str(&mut c, "  end");
        let (message, snippets) = c.split_snippets(&c.text());
        assert_eq!(message, "intro\n\noutro and end");
        assert_eq!(snippets.len(), 2);
    }

    /// The cursor's line and column.
    fn cursor(c: &Composer) -> (usize, usize) {
        let at = c.widget().cursor();
        (at.0, at.1)
    }

    #[test]
    fn home_end_and_cmd_arrows_move_to_the_ends_of_the_line() {
        let mut c = Composer::new(10);
        c.set_text("first line\nsecond line");
        let moves = [
            (key(KeyCode::Home, KeyModifiers::NONE), (1, 0)),
            (key(KeyCode::End, KeyModifiers::NONE), (1, 11)),
            (key(KeyCode::Left, KeyModifiers::SUPER), (1, 0)),
            (key(KeyCode::Right, KeyModifiers::SUPER), (1, 11)),
            (key(KeyCode::Char('a'), KeyModifiers::CONTROL), (1, 0)),
            (key(KeyCode::Char('e'), KeyModifiers::CONTROL), (1, 11)),
            (key(KeyCode::Home, KeyModifiers::CONTROL), (0, 0)),
            (key(KeyCode::End, KeyModifiers::NONE), (0, 10)),
            (key(KeyCode::End, KeyModifiers::CONTROL), (1, 11)),
            (key(KeyCode::Left, KeyModifiers::SUPER), (1, 0)),
            (key(KeyCode::Home, KeyModifiers::CONTROL), (0, 0)),
        ];
        for (k, want) in moves {
            c.handle_key(k, SendShortcut::Enter);
            assert_eq!(cursor(&c), want, "after {k:?}");
        }
        assert_eq!(c.text(), "first line\nsecond line", "no key typed anything");
    }

    #[test]
    fn at_line_end_follows_the_cursor_line() {
        let mut c = Composer::new(10);
        assert!(c.at_line_end(), "an empty composer");
        c.set_text("one\ntwo");
        assert!(c.at_line_end());
        press(&mut c, KeyCode::Left);
        assert!(!c.at_line_end());
        press(&mut c, KeyCode::End);
        assert!(c.at_line_end());
    }

    #[test]
    fn line_keys_stop_at_a_tokens_edge_so_a_delete_there_takes_the_whole_token() {
        let now = Instant::now();
        let mut c = Composer::new(10);
        c.paste_at(&big(), now, pasted());
        type_str(&mut c, " mid ");
        c.paste_at(&"x".repeat(PASTE_CHARS), now + PASTE_AGAIN, pasted());
        let line = "[Pasted text #1 +12 lines] mid [Pasted text #2 +1000 chars]";
        assert_eq!(c.text(), line);
        for (k, col) in [
            (key(KeyCode::Left, KeyModifiers::SUPER), 0),
            (key(KeyCode::End, KeyModifiers::NONE), line.chars().count()),
            (key(KeyCode::Home, KeyModifiers::CONTROL), 0),
            (
                key(KeyCode::End, KeyModifiers::CONTROL),
                line.chars().count(),
            ),
        ] {
            c.handle_key(k, SendShortcut::Enter);
            assert_eq!(cursor(&c), (0, col), "after {k:?}");
        }
        press(&mut c, KeyCode::Backspace);
        assert!(
            !c.text().contains("#2"),
            "Backspace took the whole token: {:?}",
            c.text()
        );
        c.handle_key(key(KeyCode::Left, KeyModifiers::SUPER), SendShortcut::Enter);
        press(&mut c, KeyCode::Delete);
        assert!(
            !c.text().contains("#1"),
            "Delete took the whole token: {:?}",
            c.text()
        );
        assert_eq!(c.text().trim(), "mid");
    }

    const TOKEN: &str = "[Pasted text #1 +12 lines]";

    #[test]
    fn a_word_delete_back_that_reaches_a_token_takes_all_of_it() {
        let keys = [
            key(KeyCode::Char('w'), KeyModifiers::CONTROL),
            key(KeyCode::Backspace, KeyModifiers::ALT),
            key(KeyCode::Char('h'), KeyModifiers::ALT),
        ];
        // Right after the token, then across one space, then across several.
        for k in keys {
            for tail in ["", " ", "   "] {
                let mut c = see_token(Instant::now());
                type_str(&mut c, tail);
                c.handle_key(k, SendShortcut::Enter);
                assert_eq!(c.text(), "see ", "{k:?} with {tail:?} after the token");
            }
        }
    }

    #[test]
    fn a_word_delete_forward_that_reaches_a_token_takes_all_of_it() {
        let keys = [
            key(KeyCode::Char('d'), KeyModifiers::ALT),
            key(KeyCode::Delete, KeyModifiers::ALT),
        ];
        let len = TOKEN.chars().count();
        // At the token's start, then one space before it, then several.
        for k in keys {
            for (gap, expected) in [("", "see "), (" ", "see"), ("   ", "see")] {
                let mut c = Composer::new(10);
                type_str(&mut c, &format!("see{gap}"));
                if gap.is_empty() {
                    type_str(&mut c, " ");
                }
                c.paste_at(&big(), Instant::now(), pasted());
                let lefts = len + if gap.is_empty() { 0 } else { gap.len() };
                for _ in 0..lefts {
                    press(&mut c, KeyCode::Left);
                }
                c.handle_key(k, SendShortcut::Enter);
                assert_eq!(c.text(), expected, "{k:?} with {gap:?} before the token");
            }
        }
    }

    #[test]
    fn a_word_delete_away_from_a_token_deletes_only_the_word() {
        let mut c = see_token(Instant::now());
        type_str(&mut c, " please");
        c.handle_key(
            key(KeyCode::Char('w'), KeyModifiers::CONTROL),
            SendShortcut::Enter,
        );
        assert_eq!(c.text(), format!("see {TOKEN} "));
    }

    /// Moves the cursor five columns into the token after "see ".
    fn into_token(c: &mut Composer) {
        for _ in 0..5 {
            press(c, KeyCode::Left);
        }
    }

    #[test]
    fn a_draft_whose_token_was_edited_into_plain_text_is_never_sent() {
        type Edit = fn(&mut Composer);
        let edits: [(&str, Edit); 3] = [
            ("typing inside it", |c| {
                into_token(c);
                type_str(c, "x");
            }),
            ("a newline inside it", |c| {
                into_token(c);
                c.handle_key(
                    key(KeyCode::Char('j'), KeyModifiers::CONTROL),
                    SendShortcut::Enter,
                );
            }),
            ("a cut over part of it", |c| {
                for _ in 0..5 {
                    c.handle_key(key(KeyCode::Left, KeyModifiers::SHIFT), SendShortcut::Enter);
                }
                c.handle_key(
                    key(KeyCode::Char('x'), KeyModifiers::CONTROL),
                    SendShortcut::Enter,
                );
            }),
        ];
        for (what, edit) in edits {
            let mut c = see_token(Instant::now());
            edit(&mut c);
            let draft = c.text();
            assert!(!draft.contains(TOKEN), "{what} broke the token: {draft:?}");
            assert_eq!(
                press(&mut c, KeyCode::Enter),
                ComposerAction::BrokenPaste(TOKEN.into()),
                "{what}"
            );
            assert_eq!(c.text(), draft, "the draft stays after {what}");
            type_str(&mut c, "y");
            assert_eq!(
                press(&mut c, KeyCode::Enter),
                ComposerAction::BrokenPaste(TOKEN.into()),
                "a further edit to what is left still refuses, after {what}"
            );
        }
    }

    #[test]
    fn undoing_the_edit_or_deleting_the_rest_of_the_token_lets_the_draft_send() {
        let mut c = see_token(Instant::now());
        into_token(&mut c);
        type_str(&mut c, "x");
        c.handle_key(
            key(KeyCode::Char('u'), KeyModifiers::CONTROL),
            SendShortcut::Enter,
        );
        assert_eq!(c.text(), format!("see {TOKEN}"));
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit(format!("see {TOKEN}"))
        );

        let mut c = see_token(Instant::now());
        into_token(&mut c);
        type_str(&mut c, "x");
        press(&mut c, KeyCode::End);
        for _ in 0..TOKEN.chars().count() + 1 {
            c.handle_key(key(KeyCode::Left, KeyModifiers::SHIFT), SendShortcut::Enter);
        }
        press(&mut c, KeyCode::Backspace);
        assert_eq!(c.text(), "see ");
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit("see ".into())
        );

        let mut c = see_token(Instant::now());
        for _ in 0..TOKEN.chars().count() {
            c.handle_key(key(KeyCode::Left, KeyModifiers::SHIFT), SendShortcut::Enter);
        }
        c.handle_key(
            key(KeyCode::Char('x'), KeyModifiers::CONTROL),
            SendShortcut::Enter,
        );
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit("see ".into()),
            "a cut of the whole token removes it on purpose"
        );
    }

    #[test]
    fn a_paste_inside_a_token_breaks_it_and_the_send_is_refused() {
        let mut c = see_token(Instant::now());
        into_token(&mut c);
        c.paste("oops");
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::BrokenPaste(TOKEN.into())
        );
    }

    #[test]
    fn shift_cmd_arrows_select_to_the_ends_of_the_line() {
        let mut c = Composer::new(10);
        c.set_text("first line\nsecond line");
        let shift_sup = KeyModifiers::SHIFT | KeyModifiers::SUPER;
        c.handle_key(key(KeyCode::Left, shift_sup), SendShortcut::Enter);
        assert_eq!(c.widget().selection_range(), Some(((1, 0), (1, 11))));
        c.handle_key(key(KeyCode::Right, shift_sup), SendShortcut::Enter);
        assert_eq!(
            c.widget().selection_range(),
            Some(((1, 11), (1, 11))),
            "back where it began, with nothing selected"
        );
        c.handle_key(key(KeyCode::Left, shift_sup), SendShortcut::Enter);
        c.handle_key(
            key(KeyCode::Right, KeyModifiers::SUPER),
            SendShortcut::Enter,
        );
        assert_eq!(
            c.widget().selection_range(),
            None,
            "without Shift, the move drops the selection"
        );
        assert_eq!(cursor(&c), (1, 11));
    }

    /// Asserts the send key on `c` either refuses, naming `token`, or sends a draft that holds
    /// `token` whole, so its paste goes with it.
    fn never_sent_without(c: &mut Composer, token: &str, what: &str) -> ComposerAction {
        let action = press(c, KeyCode::Enter);
        match &action {
            ComposerAction::BrokenPaste(named) => assert_eq!(named, token, "{what}"),
            ComposerAction::Submit(text) => {
                assert!(
                    text.contains(token),
                    "{what} sent without its paste: {text:?}"
                )
            }
            other => panic!("{what}: {other:?}"),
        }
        action
    }

    #[test]
    fn a_word_delete_then_an_undo_never_sends_a_part_of_the_token() {
        let mut c = see_token(Instant::now());
        c.handle_key(
            key(KeyCode::Char('w'), KeyModifiers::CONTROL),
            SendShortcut::Enter,
        );
        assert_eq!(c.text(), "see ");
        c.handle_key(
            key(KeyCode::Char('u'), KeyModifiers::CONTROL),
            SendShortcut::Enter,
        );
        assert_eq!(
            c.text(),
            format!("see {TOKEN}"),
            "one undo brings the whole token back"
        );
        never_sent_without(&mut c, TOKEN, "Ctrl+W then Ctrl+U");
        // Typing after the token first, so the undo has an edit of its own to take back.
        let mut c = see_token(Instant::now());
        type_str(&mut c, " ");
        c.handle_key(
            key(KeyCode::Char('w'), KeyModifiers::CONTROL),
            SendShortcut::Enter,
        );
        for _ in 0..2 {
            c.handle_key(
                key(KeyCode::Char('u'), KeyModifiers::CONTROL),
                SendShortcut::Enter,
            );
        }
        never_sent_without(&mut c, TOKEN, "Ctrl+W then two Ctrl+U");
    }

    #[test]
    fn a_word_delete_into_a_remnant_beside_a_whole_token_is_refused() {
        let now = Instant::now();
        let mut c = Composer::new(10);
        c.paste_at(&big(), now, pasted());
        type_str(&mut c, " ");
        c.paste_at(&"x".repeat(PASTE_CHARS), now + PASTE_AGAIN, pasted());
        let second = "[Pasted text #2 +1000 chars]";
        for _ in 0..3 {
            press(&mut c, KeyCode::Left);
        }
        type_str(&mut c, "q");
        press(&mut c, KeyCode::End);
        c.handle_key(
            key(KeyCode::Char('w'), KeyModifiers::CONTROL),
            SendShortcut::Enter,
        );
        assert!(c.text().starts_with(TOKEN), "{:?}", c.text());
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::BrokenPaste(second.into())
        );
    }

    #[test]
    fn a_token_changed_in_the_editor_is_refused() {
        let mut c = see_token(Instant::now());
        c.set_text("see [Pasted text #1 +12 lines");
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::BrokenPaste(TOKEN.into())
        );
        c.set_text("see");
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::BrokenPaste(TOKEN.into()),
            "a token the editor took out whole is still not an explicit removal"
        );
        c.set_text("");
        type_str(&mut c, "new message");
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit("new message".into()),
            "a cleared draft holds no paste"
        );
    }

    #[test]
    fn a_recalled_message_sends_and_the_draft_keeps_its_paste_when_it_comes_back() {
        let mut c = Composer::new(10);
        type_str(&mut c, "earlier");
        press(&mut c, KeyCode::Enter);
        let mut draft = see_token_on(c);
        press(&mut draft, KeyCode::Up);
        assert_eq!(draft.text(), "earlier");
        assert_eq!(
            press(&mut draft, KeyCode::Enter),
            ComposerAction::Submit("earlier".into()),
            "the draft's paste does not hold up another message"
        );
        let mut c = Composer::new(10);
        type_str(&mut c, "earlier");
        press(&mut c, KeyCode::Enter);
        let mut c = see_token_on(c);
        c.set_text("see [Pasted text #1 +12 lines");
        press(&mut c, KeyCode::Up);
        press(&mut c, KeyCode::Down);
        assert_eq!(c.text(), "see [Pasted text #1 +12 lines");
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::BrokenPaste(TOKEN.into()),
            "the draft that comes back still owes its paste"
        );
    }

    /// `c` with "see " and a token typed after what it already sent.
    fn see_token_on(mut c: Composer) -> Composer {
        type_str(&mut c, "see ");
        c.paste_at(&big(), Instant::now(), pasted());
        c
    }

    /// Gives the composer `/workspace`'s argument entries for `names`, then `none`.
    fn workspaces(c: &mut Composer, names: &[&str]) {
        let mut entries: Vec<MenuEntry> = names
            .iter()
            .map(|n| skills::argument(n, "running · Docker"))
            .collect();
        entries.push(skills::argument("none", "Detach the workspace"));
        c.set_arguments("/workspace", entries);
    }

    fn menu_labels(c: &Composer) -> Vec<String> {
        c.slash_matches().iter().map(|e| e.label.clone()).collect()
    }

    #[test]
    fn a_command_and_a_space_list_its_argument_names() {
        let mut c = Composer::new(10);
        workspaces(&mut c, &["dev-20", "build3", "dev-2"]);
        type_str(&mut c, "/workspace ");
        assert_eq!(menu_labels(&c), ["dev-20", "build3", "dev-2", "none"]);
        type_str(&mut c, "dev-2");
        assert_eq!(menu_labels(&c), ["dev-2", "dev-20"], "the exact name first");
        press(&mut c, KeyCode::Tab);
        assert_eq!(c.text(), "/workspace dev-2");
        c.set_text("/ws bu");
        assert_eq!(menu_labels(&c), ["build3"], "/ws lists the same names");
        press(&mut c, KeyCode::Tab);
        assert_eq!(
            c.text(),
            "/ws build3",
            "Tab keeps the name typed for the command"
        );
        c.set_text("/ws zzz");
        assert!(c.slash_matches().is_empty(), "no match: no menu");
        c.set_text("/workspacex d");
        assert!(c.slash_matches().is_empty());
        c.set_text("/workspacex");
        assert!(
            c.slash_matches().is_empty(),
            "no command starts with /workspacex"
        );
        c.set_text("/workspace dev-2");
        type_str(&mut c, " ");
        assert!(
            c.slash_matches().is_empty(),
            "a space after the name closes the menu"
        );
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit("/workspace dev-2 ".into())
        );
    }

    #[test]
    fn the_send_key_takes_the_highlighted_argument_once_one_is_typed_or_picked() {
        let mut c = Composer::new(10);
        workspaces(&mut c, &["dev", "build"]);
        type_str(&mut c, "/workspace d");
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit("/workspace dev".into())
        );
        type_str(&mut c, "/ws ");
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit("/ws ".into()),
            "nothing after the space runs /workspace bare, as before"
        );
        type_str(&mut c, "/ws ");
        press(&mut c, KeyCode::Down);
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit("/ws build".into())
        );
        type_str(&mut c, "/ws ");
        press(&mut c, KeyCode::Down);
        press(&mut c, KeyCode::Up);
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit("/ws dev".into()),
            "a highlight moved back to the first name still picks it"
        );
        type_str(&mut c, "/ws ");
        press(&mut c, KeyCode::Down);
        type_str(&mut c, "d");
        press(&mut c, KeyCode::Backspace);
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit("/ws ".into()),
            "an edit since the move forgets it"
        );
        c.set_arguments(
            "/workspace",
            vec![
                skills::argument("none", "Detach the workspace"),
                skills::note_entry("Loading workspaces…"),
            ],
        );
        type_str(&mut c, "/workspace de");
        assert_eq!(menu_labels(&c), ["Loading workspaces…"]);
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit("/workspace de".into()),
            "a note is never sent"
        );
    }

    #[test]
    fn esc_closes_the_argument_menu_as_it_closes_the_command_menu() {
        let mut c = Composer::new(10);
        workspaces(&mut c, &["dev"]);
        type_str(&mut c, "/ws d");
        assert_eq!(press(&mut c, KeyCode::Esc), ComposerAction::None);
        assert!(c.slash_matches().is_empty());
        assert_eq!(c.text(), "");
    }

    #[test]
    fn new_argument_entries_keep_the_highlight_on_the_same_name() {
        let mut c = Composer::new(10);
        workspaces(&mut c, &["alpha", "beta", "gamma"]);
        type_str(&mut c, "/workspace ");
        press(&mut c, KeyCode::Down);
        workspaces(&mut c, &["delta", "gamma", "alpha", "beta"]);
        assert_eq!(c.slash_matches()[c.slash_selected()].label, "beta");
        workspaces(&mut c, &["delta", "gamma"]);
        assert_eq!(
            c.slash_selected(),
            0,
            "a name that left puts the highlight on the first"
        );
    }

    #[test]
    fn a_moved_highlight_whose_name_leaves_the_list_no_longer_picks_one() {
        let mut c = Composer::new(10);
        workspaces(&mut c, &["alpha", "beta", "gamma"]);
        type_str(&mut c, "/workspace ");
        press(&mut c, KeyCode::Down);
        workspaces(&mut c, &["alpha", "gamma"]);
        assert_eq!(
            press(&mut c, KeyCode::Enter),
            ComposerAction::Submit("/workspace ".into()),
            "the picked name is gone, so the send key runs /workspace bare"
        );
    }
}
