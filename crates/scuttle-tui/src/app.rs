//! The TUI: owns UI state, turns terminal events into core messages, and draws.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use scuttle_core::app::{App, CopyTarget, Effect, Msg, Notice};
use scuttle_core::config::{self, LocalConfig};
use scuttle_core::density::SendShortcut;

use crate::activity::{SPINNER_INTERVAL, activity_line, spinner_frame};
use crate::clipboard::{Clipboard, CopyOutcome};
use crate::composer::{Composer, ComposerAction};
use crate::footer::footer_line;
use crate::help::help_lines;
use crate::links;
use crate::picker::{PickerChoice, PickerState};
use crate::selection::{Pos, Selection, selected_text};
use crate::theme::Theme;
use crate::transcript_view::{self, BlockId, HitTarget, View, Welcome};

/// How long a notice replaces the status footer when no key is pressed.
pub const NOTICE_TTL: Duration = Duration::from_secs(5);

/// Screens narrower or shorter than this keep every cell for content instead of a margin.
const MIN_PADDED: (u16, u16) = (20, 8);

/// The screen inside a one-cell margin on every side, or all of it on a tiny terminal.
fn padded(area: Rect) -> Rect {
    if area.width >= MIN_PADDED.0 && area.height >= MIN_PADDED.1 {
        area.inner(Margin::new(1, 1))
    } else {
        area
    }
}

const PLACEHOLDER_SHIFT: &str =
    "Message the agent. Enter to send, Shift+Enter for a new line, /help for commands.";
const PLACEHOLDER_ALT: &str =
    "Message the agent. Enter to send, Alt+Enter for a new line, /help for commands.";
const PLACEHOLDER_MODIFIER: &str =
    "Message the agent. Ctrl+Enter to send, Enter for a new line, /help for commands.";
const PLACEHOLDER_MODIFIER_ALT: &str =
    "Message the agent. Alt+Enter to send, Enter for a new line, /help for commands.";

/// Leaves the terminal, runs the editor, and resumes. Resume is attempted even when leaving
/// failed, but the editor only runs on a terminal that was fully left. Returns the editor's
/// outcome and the resume's outcome.
fn editor_round_trip(
    leave: impl FnOnce() -> std::io::Result<()>,
    run: impl FnOnce() -> std::io::Result<()>,
    resume: impl FnOnce() -> std::io::Result<()>,
) -> (std::io::Result<()>, std::io::Result<()>) {
    let edited = leave().and_then(|()| run());
    let resumed = resume();
    (edited, resumed)
}

/// The notice for copying `url`, called `what`, after it opened no browser.
fn copy_url_notice(what: &str, url: &str, outcome: CopyOutcome) -> Notice {
    match outcome {
        CopyOutcome::Copied => Notice::Info(format!("Copied {what}: {url}")),
        CopyOutcome::CopiedWithWarning(w) => Notice::Info(format!("Copied {what}: {url}. {w}")),
        CopyOutcome::Failed(_) => Notice::Error(format!("Could not copy {what}: {url}")),
    }
}

/// The notice for copying the chat URL after `/web` opened no browser.
fn web_copy_notice(url: &str, outcome: CopyOutcome) -> Notice {
    copy_url_notice("the chat URL", url, outcome)
}

/// The notice for copying a link that opened no browser. A link the runtime refuses to open
/// says so, since nothing else would explain why the click did not open it.
fn link_copy_notice(url: &str, outcome: CopyOutcome) -> Notice {
    let notice = copy_url_notice("the link", url, outcome);
    if links::web_link(url).is_some() {
        return notice;
    }
    let why = "Only http and https links open in a browser.";
    match notice {
        Notice::Info(text) => Notice::Info(format!("{why} {text}")),
        Notice::Error(text) => Notice::Error(format!("{why} {text}")),
    }
}

/// A left-button press in the transcript that has not been released.
#[derive(Debug, Clone, Copy)]
struct Drag {
    anchor: Pos,
    /// Whether the pointer left the anchor cell; a release without moving is a click.
    moved: bool,
    /// The transcript line kept at the top of the screen while the button is held, so new
    /// output cannot scroll the text out from under the pointer.
    top: usize,
    /// The transcript area at the press. Both ends of the drag map through it, so the
    /// activity row appearing or leaving mid-drag cannot shift the selection.
    area: Rect,
}

pub struct Tui {
    pub core: App,
    pub composer: Composer,
    pub picker: Option<PickerState>,
    pub last_copied: Option<String>,
    config: LocalConfig,
    config_path: Option<PathBuf>,
    theme: Theme,
    welcome: Welcome,
    toggles: HashSet<BlockId>,
    view: View,
    area: Rect,
    scroll_from_bottom: usize,
    last_ctrl_c: Option<Instant>,
    show_help: bool,
    /// How far the help overlay is scrolled, in lines.
    help_scroll: u16,
    /// Created on the first copy: opening the native clipboard can block on a stale display.
    clipboard: Option<Clipboard>,
    /// The footer's notice: an index into `core.notices` and when it became active.
    active_notice: Option<(usize, Instant)>,
    /// How many of `core.notices` the footer has already considered.
    notices_seen: usize,
    /// Indices into `core.notices` that arrived but were never active, oldest first.
    unshown_notices: VecDeque<usize>,
    /// Set when the screen may hold foreign output, for example after the external editor.
    needs_full_redraw: bool,
    /// Whether keyboard enhancement flags are active, which decides the send and newline keys.
    keyboard_enhanced: bool,
    /// Set when the app must quit with an error, for example when the terminal cannot be restored.
    fatal: Option<String>,
    /// When the Tui was made; the spinner frame is a function of the time since.
    epoch: Instant,
    /// Set by `tick` for a timer wakeup, where only the clock changed, so `draw_at` reuses the
    /// transcript lines instead of rebuilding them. Cleared by every draw.
    reuse_view: bool,
    /// The transcript width `view` was built for.
    view_width: u16,
    /// The transcript's `history_resets` count when `view` was built.
    view_resets: u64,
    /// How many times `draw_at` rebuilt the transcript lines; tests check timer frames reuse them.
    view_builds: usize,
    /// The highlighted text, kept after release until the next press or key.
    selection: Option<Selection>,
    drag: Option<Drag>,
}

impl Tui {
    pub fn new(
        config: LocalConfig,
        config_path: Option<PathBuf>,
        theme: Theme,
        welcome: Welcome,
    ) -> Tui {
        let mut composer = Composer::new(config.composer_max_lines);
        composer.set_gutter_style(theme.dim);
        Tui {
            core: App::new(config.busy_behavior, config.mouse),
            composer,
            picker: None,
            last_copied: None,
            config,
            config_path,
            theme,
            welcome,
            toggles: HashSet::new(),
            view: View::default(),
            area: Rect::default(),
            scroll_from_bottom: 0,
            last_ctrl_c: None,
            show_help: false,
            help_scroll: 0,
            clipboard: None,
            active_notice: None,
            notices_seen: 0,
            unshown_notices: VecDeque::new(),
            needs_full_redraw: false,
            keyboard_enhanced: true,
            fatal: None,
            epoch: Instant::now(),
            reuse_view: false,
            view_width: 0,
            view_resets: 0,
            view_builds: 0,
            selection: None,
            drag: None,
        }
    }

    /// Records whether the terminal reports modified Enter keys distinctly, which decides the
    /// composer's send and newline keys and its hint.
    pub fn set_keyboard_enhanced(&mut self, enhanced: bool) {
        self.keyboard_enhanced = enhanced;
        self.composer.set_enhanced(enhanced);
        self.refresh_placeholder();
    }

    /// Picks the composer hint from the send preference and the keyboard enhancement state.
    fn refresh_placeholder(&mut self) {
        let text = match (self.core.prefs.send_shortcut, self.keyboard_enhanced) {
            (SendShortcut::Enter, true) => PLACEHOLDER_SHIFT,
            (SendShortcut::Enter, false) => PLACEHOLDER_ALT,
            (SendShortcut::ModifierEnter, true) => PLACEHOLDER_MODIFIER,
            (SendShortcut::ModifierEnter, false) => PLACEHOLDER_MODIFIER_ALT,
        };
        if self.composer.widget().placeholder_text() != text {
            self.composer.set_placeholder(text);
        }
    }

    /// The reason the app must quit with an error, if any. Resets it.
    pub fn take_fatal(&mut self) -> Option<String> {
        self.fatal.take()
    }

    /// Feeds a message to the core and keeps UI state consistent with the result.
    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        let effects = self.core.update(msg);
        self.prune_live_toggles();
        self.refresh_placeholder();
        effects
    }

    /// Live-turn block ids are reused by the next turn, so expansions must not carry over.
    fn prune_live_toggles(&mut self) {
        if self.core.transcript.live.is_empty() {
            self.toggles.retain(|(owner, _)| owner.is_some());
        }
    }

    fn notice(&mut self, n: Notice) {
        self.core.notices.push(n);
    }

    /// Makes the newest notice pushed since the last call active, and expires an old one. Once
    /// no notice is active, the oldest notice that was never shown takes its turn.
    pub fn sync_notice(&mut self, now: Instant) {
        let count = self.core.notices.len();
        if count > self.notices_seen {
            self.unshown_notices.extend(self.notices_seen..count);
            self.active_notice = self.unshown_notices.pop_back().map(|i| (i, now));
        }
        self.notices_seen = count;
        if self
            .active_notice
            .is_some_and(|(_, since)| now.duration_since(since) >= NOTICE_TTL)
        {
            self.active_notice = None;
        }
        if self.active_notice.is_none() {
            self.active_notice = self.unshown_notices.pop_front().map(|i| (i, now));
        }
    }

    /// The notice the footer shows instead of the status line, if any.
    pub fn active_notice(&self) -> Option<&Notice> {
        self.active_notice
            .and_then(|(i, _)| self.core.notices.get(i))
    }

    /// When the active notice expires, so the loop can redraw then.
    pub fn notice_deadline(&self) -> Option<Instant> {
        self.active_notice.map(|(_, since)| since + NOTICE_TTL)
    }

    /// Whether the next draw must repaint every cell. Resets the request.
    pub fn take_full_redraw(&mut self) -> bool {
        std::mem::take(&mut self.needs_full_redraw)
    }

    /// When the spinner needs its next frame, or `None` while the agent is idle.
    pub fn animation_deadline(&self, now: Instant) -> Option<Instant> {
        self.core.activity().map(|_| now + SPINNER_INTERVAL)
    }

    /// Marks the next draw as a timer wakeup: nothing but the clock changed since the last one.
    pub fn tick(&mut self) {
        self.reuse_view = true;
    }

    fn copy(&mut self, text: String) {
        if let Some(outcome) = self.write_clipboard(text) {
            self.report_copy(outcome);
        }
    }

    /// Puts `text` on the clipboard, opening it on first use. Tests only record the text, so
    /// they return `None`.
    fn write_clipboard(&mut self, text: String) -> Option<CopyOutcome> {
        self.last_copied = Some(text.clone());
        if cfg!(test) {
            return None;
        }
        Some(
            self.clipboard
                .get_or_insert_with(Clipboard::new)
                .copy(&text),
        )
    }

    /// Copies a URL that opened no browser, reporting the copy with `notice`.
    fn copy_url(&mut self, url: &str, notice: impl FnOnce(CopyOutcome) -> Notice) {
        if let Some(outcome) = self.write_clipboard(url.to_owned()) {
            self.notice(notice(outcome));
        }
    }

    fn report_copy(&mut self, outcome: CopyOutcome) {
        let notice = match outcome {
            CopyOutcome::Copied => Notice::Info("Copied".into()),
            CopyOutcome::CopiedWithWarning(w) => Notice::Info(format!("Copied. {w}")),
            CopyOutcome::Failed(why) => Notice::Error(format!("Copy failed: {why}")),
        };
        self.notice(notice);
    }

    /// Handles effects the UI owns. Returns false for effects the runtime should run.
    pub fn apply_ui_effect(&mut self, effect: &Effect) -> bool {
        match effect {
            Effect::ShowPicker(kind) => self.picker = Some(PickerState::open(*kind, &self.core)),
            Effect::ShowHelp => {
                self.show_help = true;
                self.help_scroll = 0;
            }
            Effect::Copy(CopyTarget::LastMessage) => {
                let text = self
                    .core
                    .transcript
                    .messages()
                    .rev()
                    .find(|m| m.role.as_ref().map(|r| r.as_str()) == Some("assistant"))
                    .map(|m| {
                        m.content
                            .iter()
                            .filter(|p| p.type_.as_ref().map(|t| t.as_str()) == Some("text"))
                            .filter_map(|p| p.text.clone())
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    });
                match text {
                    Some(t) => self.copy(t),
                    None => self.notice(Notice::Error("Nothing to copy yet.".into())),
                }
            }
            Effect::Copy(CopyTarget::CodeBlock(n)) => {
                match n
                    .checked_sub(1)
                    .and_then(|i| self.view.last_code_blocks.get(i))
                    .cloned()
                {
                    Some(code) => self.copy(code),
                    None => self.notice(Notice::Error(format!(
                        "The last message has no code block {n}."
                    ))),
                }
            }
            Effect::SetMouse(enabled) => {
                if !cfg!(test) {
                    let _ = crate::terminal::set_mouse(*enabled);
                }
                if !*enabled {
                    self.end_drag();
                    self.selection = None;
                }
                if let Some(path) = self.config_path.as_ref()
                    && let Err(e) = config::set_mouse(path, *enabled)
                {
                    self.notice(Notice::Error(e.to_string()));
                }
                let note = if *enabled {
                    "Mouse capture on. Drag to select and copy; hold Shift (Option in iTerm2 or Terminal.app) for the terminal's own selection."
                } else {
                    "Mouse capture off."
                };
                self.notice(Notice::Info(note.into()));
            }
            Effect::SaveOrganization(id) => {
                if let Some(path) = self.config_path.as_ref()
                    && let Err(e) = config::set_organization(path, *id)
                {
                    self.notice(Notice::Error(e.to_string()));
                }
            }
            Effect::ClearView => {
                self.toggles.clear();
                self.selection = None;
                self.drag = None;
                self.scroll_from_bottom = 0;
                self.composer.reset_history_position();
            }
            Effect::CopyWebUrl(url) => self.copy_url(url, |o| web_copy_notice(url, o)),
            Effect::CopyLink(url) => self.copy_url(url, |o| link_copy_notice(url, o)),
            Effect::RestoreComposer(text) => {
                // Keep anything typed since the failed request, after the restored text.
                let current = self.composer.text();
                if current.trim().is_empty() {
                    self.composer.set_text(text);
                } else {
                    self.composer.set_text(&format!("{text}\n\n{current}"));
                }
            }
            _ => return false,
        }
        true
    }

    /// Replaces the composer text with what the editor saved, or leaves it untouched on failure.
    pub fn edit_with(
        &mut self,
        run: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let path = std::env::temp_dir().join(format!("scuttle-{}.md", uuid::Uuid::new_v4()));
        {
            use std::io::Write;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&path)?;
            if let Err(e) = file.write_all(self.composer.text().as_bytes()) {
                let _ = std::fs::remove_file(&path);
                return Err(e);
            }
        }
        let result = run(&path).and_then(|_| std::fs::read_to_string(&path));
        self.needs_full_redraw = true;
        let _ = std::fs::remove_file(&path);
        let text = result?;
        self.composer.set_text(text.trim_end_matches('\n'));
        Ok(())
    }

    /// The first screen row showing a click target that matches `pred`.
    #[cfg(test)]
    pub fn row_of(&self, pred: impl Fn(&HitTarget) -> bool) -> Option<u16> {
        let top = self.top_line();
        let hit = self.view.hits.iter().find(|h| pred(&h.target))?;
        let line = hit.lines.start.max(top);
        (line < top + self.area.height as usize).then(|| self.area.y + (line - top) as u16)
    }

    fn max_scroll(&self) -> usize {
        self.view
            .lines
            .len()
            .saturating_sub(self.area.height as usize)
    }

    /// The transcript line at the top of the screen. A held drag pins it, even past the usual
    /// bottom when the transcript grew taller mid-drag.
    fn top_line(&self) -> usize {
        match self.drag {
            Some(drag) => drag.top,
            None => self.max_scroll().saturating_sub(self.scroll_from_bottom),
        }
    }

    fn scroll_up(&mut self, lines: usize) {
        if let Some(drag) = self.drag.as_mut() {
            drag.top = drag.top.saturating_sub(lines);
            return;
        }
        self.scroll_from_bottom = (self.scroll_from_bottom + lines).min(self.max_scroll());
    }

    fn scroll_down(&mut self, lines: usize) {
        let max = self.max_scroll();
        if let Some(drag) = self.drag.as_mut() {
            if drag.top < max {
                drag.top = (drag.top + lines).min(max);
            }
            return;
        }
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(lines);
    }

    fn inside(&self, column: u16, row: u16) -> bool {
        row >= self.area.y
            && row < self.area.y + self.area.height
            && column >= self.area.x
            && column < self.area.x + self.area.width
    }

    /// The transcript cell under the pointer, clamped into the transcript. During a drag it
    /// maps through the transcript area as it was at the press.
    fn pos_at(&self, column: u16, row: u16) -> Pos {
        let area = self.drag.map_or(self.area, |d| d.area);
        let bottom = area.y + area.height.saturating_sub(1);
        let right = area.x + area.width.saturating_sub(1);
        let row = row.clamp(area.y, bottom);
        Pos {
            line: self.top_line() + (row - area.y) as usize,
            col: column.clamp(area.x, right) - area.x,
        }
    }

    /// Acts on a click on transcript row `line`: copies a code block or toggles a block.
    fn click(&mut self, line: usize) {
        let target = self
            .view
            .hits
            .iter()
            .rev()
            .find(|h| h.lines.contains(&line))
            .map(|h| h.target.clone());
        match target {
            Some(HitTarget::CopyCode(code)) => self.copy(code),
            Some(HitTarget::Toggle(id)) => {
                let was_toggled = self.toggles.remove(&id);
                if !was_toggled {
                    self.toggles.insert(id);
                }
            }
            None => {}
        }
    }

    /// The URL of the link under transcript cell `pos`, if any.
    fn link_at(&self, pos: Pos) -> Option<String> {
        self.view
            .links
            .iter()
            .find(|l| l.line == pos.line && l.cols.contains(&pos.col))
            .map(|l| l.url.clone())
    }

    pub fn handle(&mut self, event: Event) -> Vec<Effect> {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.key(key),
            Event::Paste(text) => {
                self.composer.paste(&text);
                vec![]
            }
            Event::Mouse(m) => self.mouse(m),
            _ => vec![],
        }
    }

    fn mouse(&mut self, m: MouseEvent) -> Vec<Effect> {
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.selection = None;
                self.end_drag();
                if self.core.mouse && !self.overlay_showing() && self.inside(m.column, m.row) {
                    self.drag = Some(Drag {
                        anchor: self.pos_at(m.column, m.row),
                        moved: false,
                        top: self.top_line(),
                        area: self.area,
                    });
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => self.drag_to(m.column, m.row),
            MouseEventKind::Up(MouseButton::Left) => {
                self.extend_to(m.column, m.row);
                let Some(drag) = self.drag else {
                    return vec![];
                };
                let mut effects = vec![];
                if !drag.moved {
                    // The view is still pinned at the press, so this is the link that was
                    // under the pointer then, even if the agent streamed more since.
                    match self.link_at(drag.anchor) {
                        Some(url) => effects.push(Effect::OpenLink(url)),
                        None => self.click(drag.anchor.line),
                    }
                } else if let Some(selection) = self.selection {
                    let text = selected_text(&self.view, &selection);
                    if !text.is_empty() {
                        self.copy(text);
                    }
                }
                self.end_drag();
                return effects;
            }
            MouseEventKind::ScrollUp => self.scroll_up(3),
            MouseEventKind::ScrollDown => self.scroll_down(3),
            _ => {}
        }
        vec![]
    }

    /// Extends the selection to the pointer, scrolling a line when it leaves the transcript.
    fn drag_to(&mut self, column: u16, row: u16) {
        let Some(drag) = self.drag else {
            return;
        };
        if row < drag.area.y {
            self.scroll_up(1);
        } else if row >= drag.area.y + drag.area.height {
            self.scroll_down(1);
        }
        self.extend_to(column, row);
    }

    fn extend_to(&mut self, column: u16, row: u16) {
        let head = self.pos_at(column, row);
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        drag.moved |= head != drag.anchor;
        if drag.moved {
            self.selection = Some(Selection {
                anchor: drag.anchor,
                head,
            });
        }
    }

    /// Lets go of a held drag, handing its pinned top back to the scroll position.
    fn end_drag(&mut self) {
        if let Some(drag) = self.drag.take() {
            let max = self.max_scroll();
            self.scroll_from_bottom = max - drag.top.min(max);
        }
    }

    /// Whether the help box, a picker, or the slash menu covers the transcript.
    fn overlay_showing(&self) -> bool {
        self.show_help || self.picker.is_some() || !self.composer.slash_matches().is_empty()
    }

    fn key(&mut self, key: KeyEvent) -> Vec<Effect> {
        self.active_notice = None;
        self.selection = None;
        self.end_drag();
        // Handled before the overlays, so a double Ctrl+C quits from anywhere.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            let now = Instant::now();
            if self
                .last_ctrl_c
                .is_some_and(|t| now.duration_since(t) < Duration::from_secs(2))
            {
                return vec![Effect::Quit];
            }
            self.last_ctrl_c = Some(now);
            self.show_help = false;
            self.picker = None;
            self.notice(Notice::Info("Press Ctrl+C again to quit.".into()));
            return vec![];
        }
        if self.show_help {
            let page = self.area.height.saturating_sub(2).max(1);
            match key.code {
                KeyCode::Up => self.help_scroll = self.help_scroll.saturating_sub(1),
                KeyCode::Down => self.help_scroll = self.help_scroll.saturating_add(1),
                KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(page),
                KeyCode::PageDown => self.help_scroll = self.help_scroll.saturating_add(page),
                _ => self.show_help = false,
            }
            return vec![];
        }
        if let Some(picker) = self.picker.as_mut() {
            let choice = picker.handle_key(key);
            return match choice {
                Some(PickerChoice::Cancel) => {
                    self.picker = None;
                    vec![]
                }
                Some(PickerChoice::Model(id)) => {
                    self.picker = None;
                    self.update(Msg::ModelChosen(id))
                }
                Some(PickerChoice::Workspace(ws)) => {
                    self.picker = None;
                    self.update(Msg::WorkspaceChosen(ws))
                }
                Some(PickerChoice::Effort(level)) => {
                    self.picker = None;
                    self.update(Msg::EffortChosen(level))
                }
                Some(PickerChoice::Organization(id)) => {
                    self.picker = None;
                    self.update(Msg::OrganizationChosen(id))
                }
                None => vec![],
            };
        }
        let page = self.area.height.max(1) as usize;
        match key.code {
            KeyCode::PageUp => {
                self.scroll_up(page);
                return vec![];
            }
            KeyCode::PageDown => {
                self.scroll_down(page);
                return vec![];
            }
            KeyCode::End => {
                self.scroll_from_bottom = 0;
                return vec![];
            }
            _ => {}
        }
        match self.composer.handle_key(key, self.core.prefs.send_shortcut) {
            ComposerAction::Submit(text) => {
                self.scroll_from_bottom = 0;
                self.update(Msg::Submit(text))
            }
            ComposerAction::Interrupt => self.update(Msg::Interrupt),
            ComposerAction::OpenEditor => self.open_editor(),
            ComposerAction::None => vec![],
        }
    }

    fn open_editor(&mut self) -> Vec<Effect> {
        let editor = std::env::var("EDITOR")
            .or_else(|_| std::env::var("VISUAL"))
            .ok()
            .filter(|e| !e.trim().is_empty())
            .unwrap_or_else(|| "vi".into());
        let mouse = self.core.mouse;
        let mut resumed = Ok(());
        let result = self.edit_with(|path| {
            let (edited, resume) = editor_round_trip(
                crate::terminal::leave,
                || {
                    // `$1` keeps the path out of the shell parse while still allowing
                    // `EDITOR="code -w"`.
                    let status = std::process::Command::new("sh")
                        .arg("-c")
                        .arg(format!("{editor} \"$1\""))
                        .arg("sh")
                        .arg(path)
                        .status()?;
                    if status.success() {
                        Ok(())
                    } else {
                        Err(std::io::Error::other(format!(
                            "editor exited with {status}"
                        )))
                    }
                },
                || crate::terminal::resume(mouse),
            );
            resumed = resume;
            edited
        });
        self.set_keyboard_enhanced(crate::terminal::keyboard_enhanced());
        if let Err(e) = result {
            self.notice(Notice::Error(format!("Editor failed: {e}")));
        }
        self.finish_editor(resumed)
    }

    /// Quits with an error when the terminal could not be restored after the editor, since
    /// drawing on a terminal in an unknown mode would garble the screen.
    fn finish_editor(&mut self, resumed: std::io::Result<()>) -> Vec<Effect> {
        match resumed {
            Ok(()) => vec![],
            Err(e) => {
                self.fatal = Some(format!(
                    "could not restore the terminal after the editor: {e}"
                ));
                vec![Effect::Quit]
            }
        }
    }

    pub fn draw(&mut self, f: &mut Frame) {
        self.draw_at(f, Instant::now());
    }

    /// Draws the screen as of `now`, which picks the spinner frame and expires notices.
    pub fn draw_at(&mut self, f: &mut Frame, now: Instant) {
        self.prune_live_toggles();
        self.sync_notice(now);
        let outer = padded(f.area());
        let activity = self.core.activity();
        let activity_height = u16::from(activity.is_some());
        let composer_height = self
            .composer
            .height(outer.width)
            .min(outer.height.saturating_sub(2 + activity_height).max(3));
        let [transcript, activity_row, composer, footer] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(activity_height),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .areas(outer);
        self.area = transcript;
        // Lines drawn at another width or before a history reset may now hold other text, so
        // a selection or held drag over them no longer means what it did.
        let resets = self.core.transcript.history_resets();
        let stale = self.view_width != transcript.width || self.view_resets != resets;
        let reuse = std::mem::take(&mut self.reuse_view) && !stale;
        if !reuse {
            self.view = transcript_view::build(
                &self.core,
                &self.config.density,
                &self.toggles,
                &self.welcome,
                &self.theme,
                transcript.width,
            );
            self.view_width = transcript.width;
            self.view_resets = resets;
            self.view_builds += 1;
        }
        if stale {
            self.selection = None;
            self.end_drag();
        }
        self.scroll_from_bottom = self.scroll_from_bottom.min(self.max_scroll());
        let top = self.top_line();
        let visible: Vec<Line> = self
            .view
            .lines
            .iter()
            .skip(top)
            .take(transcript.height as usize)
            .cloned()
            .collect();
        f.render_widget(Paragraph::new(visible), transcript);
        // A line's own style only covers its text, so the tint is painted across the row.
        for (row, meta) in self
            .view
            .meta
            .iter()
            .skip(top)
            .take(transcript.height as usize)
            .enumerate()
        {
            if meta.user {
                let line = Rect {
                    y: transcript.y + row as u16,
                    height: 1,
                    ..transcript
                };
                f.buffer_mut().set_style(line, self.theme.user_tint);
            }
        }
        // The markers of blocks in progress show the spinner's frame. Painting the cells keeps
        // a timer frame from rebuilding the lines, as with the tint.
        if !self.view.spinners.is_empty() {
            let frame = spinner_frame(now.saturating_duration_since(self.epoch));
            let buf = f.buffer_mut();
            for &line in &self.view.spinners {
                if let Some(row) = line
                    .checked_sub(top)
                    .filter(|row| *row < transcript.height as usize)
                    && let Some(cell) = buf.cell_mut((transcript.x, transcript.y + row as u16))
                {
                    cell.set_symbol(frame);
                }
            }
        }
        if let Some(selection) = self.selection {
            let shown = self
                .view
                .lines
                .len()
                .saturating_sub(top)
                .min(transcript.height as usize);
            for row in 0..shown {
                let line = top + row;
                if self.view.meta.get(line).is_some_and(|m| m.rule) {
                    continue;
                }
                if let Some((from, to)) = selection.columns(line, transcript.width) {
                    let cells = Rect {
                        x: transcript.x + from,
                        y: transcript.y + row as u16,
                        width: to - from,
                        height: 1,
                    };
                    f.buffer_mut().set_style(cells, self.theme.selection);
                }
            }
        }
        if let Some(activity) = activity.as_ref() {
            let elapsed = now.saturating_duration_since(self.epoch);
            f.render_widget(
                Paragraph::new(activity_line(activity, elapsed, &self.theme)),
                activity_row,
            );
        }
        let frame = Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(self.theme.dim);
        let inner = frame.inner(composer);
        f.render_widget(frame, composer);
        f.render_widget(self.composer.widget(), inner);
        f.render_widget(
            Paragraph::new(footer_line(
                &self.core,
                self.active_notice(),
                &self.theme,
                footer.width,
            )),
            footer,
        );
        let matches = self.composer.slash_matches();
        if !matches.is_empty() {
            let h = (matches.len() as u16 + 2).min(transcript.height);
            let area = Rect {
                y: transcript.y + transcript.height - h,
                height: h,
                ..transcript
            };
            let lines: Vec<Line> = matches
                .iter()
                .map(|c| {
                    Line::from(vec![
                        Span::styled(c.display_usage(), self.theme.accent),
                        Span::raw("  "),
                        Span::styled(c.description, self.theme.dim),
                    ])
                })
                .collect();
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(lines).block(Block::default().borders(Borders::ALL)),
                area,
            );
        }
        if let Some(picker) = self.picker.as_ref() {
            let h = picker.height().min(transcript.height);
            picker.render(
                f,
                Rect {
                    y: transcript.y + transcript.height - h,
                    height: h,
                    ..transcript
                },
                &self.theme,
            );
        }
        if self.show_help {
            let lines = help_lines(&self.theme, transcript.width.saturating_sub(2));
            let h = (lines.len() as u16 + 2).min(transcript.height);
            let max_scroll = (lines.len() as u16).saturating_sub(h.saturating_sub(2));
            self.help_scroll = self.help_scroll.min(max_scroll);
            let title = if max_scroll > 0 {
                " Help (Up and Down scroll, any other key closes) "
            } else {
                " Help "
            };
            let area = Rect {
                y: transcript.y,
                height: h,
                ..transcript
            };
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(lines)
                    .scroll((self.help_scroll, 0))
                    .block(Block::default().borders(Borders::ALL).title(title)),
                area,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use scuttle_core::live::LiveBlock;
    use serde_json::json;

    fn tui() -> Tui {
        Tui::new(
            scuttle_core::config::LocalConfig::default(),
            None,
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: "nick".into(),
                art: vec![],
                show: true,
            },
        )
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, mods))
    }

    fn screen(t: &mut Tui, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| t.draw(f)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn typing_and_enter_submits_through_core() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        for c in "hi".chars() {
            t.handle(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(effects.as_slice(), [Effect::CreateChat { text, .. }] if text == "hi"));
    }

    #[test]
    fn ctrl_c_twice_quits() {
        let mut t = tui();
        assert!(
            t.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL))
                .is_empty()
        );
        assert_eq!(
            t.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            vec![Effect::Quit]
        );
    }

    #[test]
    fn click_on_a_code_block_copies_it() {
        let (mut t, row) = tui_with_a_code_block();
        click(&mut t, row);
        assert_eq!(t.last_copied.as_deref(), Some("echo hi\n"));
    }

    #[test]
    fn click_hit_testing_accounts_for_the_margin_offset() {
        let mut t = tui();
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap();
        t.core.update(Msg::ChatLoaded {
            chat: Box::new(chat),
            messages: serde_json::from_value(json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "```\necho hi\n```"}]}])).unwrap(),
        });
        let shown = screen(&mut t, 60, 20);
        let rows: Vec<&str> = shown.lines().collect();
        assert!(
            rows[0].trim().is_empty(),
            "top margin row holds no content: {:?}",
            rows[0]
        );
        let code_row = rows
            .iter()
            .position(|r| r.contains("echo hi"))
            .expect("code block on screen") as u16;
        assert!(code_row > 0, "the code row sits below the top margin");
        // The margin row is not backed by any transcript line, so a click there must miss.
        click(&mut t, 0);
        assert_eq!(t.last_copied, None, "the margin row is not a hit target");
        // The code block's actual screen row, shifted down by the margin, still resolves.
        click(&mut t, code_row);
        assert_eq!(t.last_copied.as_deref(), Some("echo hi\n"));
    }

    #[test]
    fn the_slash_menu_shows_aliases() {
        let mut t = tui();
        t.composer.set_text("/q");
        let shown = screen(&mut t, 60, 16);
        assert!(shown.contains("/quit (/exit)"), "{shown}");
    }

    #[test]
    fn slash_model_opens_the_picker_and_escape_closes_it() {
        let mut t = tui();
        // `/model` only opens the picker once the model list has loaded.
        t.core.update(Msg::ModelsLoaded(
            serde_json::from_value(json!([{"id": uuid::Uuid::new_v4(), "display_name": "A", "enabled": true, "reasoning_efforts": []}]))
                .unwrap(),
        ));
        let effects = t
            .core
            .update(Msg::Command(scuttle_core::commands::Command::Model(None)));
        for e in &effects {
            t.apply_ui_effect(e);
        }
        assert!(t.picker.is_some());
        t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(t.picker.is_none());
    }

    #[test]
    fn slash_effort_opens_the_slider_and_enter_saves_the_level() {
        let mut t = tui();
        t.core.update(Msg::ModelsLoaded(
            serde_json::from_value(json!([
                {"id": uuid::Uuid::new_v4(), "display_name": "Thinker", "enabled": true, "is_default": true, "reasoning_efforts": ["low", "high"]}
            ]))
            .unwrap(),
        ));
        let open = |t: &mut Tui| {
            let effects = t.update(Msg::Submit("/effort".into()));
            for e in &effects {
                t.apply_ui_effect(e);
            }
            assert!(t.picker.is_some());
        };
        open(&mut t);
        let shown = screen(&mut t, 60, 20);
        assert!(shown.contains("low   high"), "{shown}");
        t.handle(key(KeyCode::Left, KeyModifiers::NONE));
        t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(t.picker.is_none());
        assert_eq!(t.core.selected_effort.as_deref(), Some("low"));
        open(&mut t);
        t.handle(key(KeyCode::Right, KeyModifiers::NONE));
        t.handle(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(t.picker.is_none());
        assert_eq!(
            t.core.selected_effort.as_deref(),
            Some("low"),
            "Esc keeps the effort"
        );
    }

    #[test]
    fn editor_failure_keeps_composer_text() {
        let mut t = tui();
        t.composer.set_text("keep me");
        let result = t.edit_with(|_path| Err(std::io::Error::other("editor crashed")));
        assert!(result.is_err());
        assert_eq!(t.composer.text(), "keep me");
    }

    #[test]
    fn the_screen_keeps_a_one_cell_margin() {
        let mut t = tui();
        t.core.notices.push(Notice::Info("margin check".into()));
        t.sync_notice(Instant::now());
        let (w, h) = (60u16, 16u16);
        let shown = screen(&mut t, w, h);
        let rows: Vec<&str> = shown.lines().collect();
        assert!(rows[0].trim().is_empty(), "top row: {:?}", rows[0]);
        assert!(rows[h as usize - 1].trim().is_empty(), "bottom row");
        for row in &rows {
            assert!(row.starts_with(' ') && row.ends_with(' '), "{row:?}");
        }
        assert!(
            rows[h as usize - 2].starts_with(" margin check"),
            "the footer sits inside the margin: {:?}",
            rows[h as usize - 2]
        );
    }

    #[test]
    fn a_tiny_terminal_gives_up_the_margin() {
        let mut t = tui();
        let shown = screen(&mut t, 20, 6);
        let rows: Vec<&str> = shown.lines().collect();
        assert!(
            rows[2].starts_with('─'),
            "the composer border starts at column 0: {rows:?}"
        );
        let padded = screen(&mut t, 40, 10);
        assert!(padded.lines().all(|r| r.starts_with(' ')), "{padded}");
    }

    #[test]
    fn draw_fits_small_terminals() {
        let mut t = tui();
        for (w, h) in [(40u16, 10u16), (20, 6), (120, 40)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| t.draw(f)).unwrap();
        }
    }

    #[test]
    fn restore_composer_puts_the_text_back() {
        let mut t = tui();
        assert!(t.apply_ui_effect(&Effect::RestoreComposer("lost words".into())));
        assert_eq!(t.composer.text(), "lost words");
    }

    #[test]
    fn restore_composer_keeps_what_was_typed_since() {
        let mut t = tui();
        t.composer.set_text("typed after");
        t.apply_ui_effect(&Effect::RestoreComposer("failed send".into()));
        assert_eq!(t.composer.text(), "failed send\n\ntyped after");
    }

    #[test]
    fn runtime_effects_are_not_ui_effects() {
        let mut t = tui();
        assert!(!t.apply_ui_effect(&Effect::FetchPrefs));
        assert!(!t.apply_ui_effect(&Effect::Quit));
    }

    #[test]
    fn live_turn_toggles_drop_when_the_live_turn_empties() {
        let mut t = tui();
        t.toggles.insert((None, 0));
        t.toggles.insert((Some(4), 1));
        t.core
            .transcript
            .live
            .blocks
            .push(LiveBlock::Text("streaming".into()));
        t.update(Msg::Refresh);
        assert!(t.toggles.contains(&(None, 0)), "live turn still has blocks");
        t.core.transcript.live.clear();
        t.update(Msg::Refresh);
        assert!(!t.toggles.contains(&(None, 0)));
        assert!(t.toggles.contains(&(Some(4), 1)));
    }

    #[test]
    fn a_new_notice_shows_until_a_key_press() {
        let mut t = tui();
        let now = Instant::now();
        t.core.notices.push(Notice::Info("hello there".into()));
        t.sync_notice(now);
        assert_eq!(t.active_notice(), Some(&Notice::Info("hello there".into())));
        assert!(screen(&mut t, 60, 10).contains("hello there"));
        t.handle(key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(t.active_notice(), None);
        assert!(!screen(&mut t, 60, 10).contains("hello there"));
    }

    #[test]
    fn a_notice_expires_after_five_seconds() {
        let mut t = tui();
        let now = Instant::now();
        t.core.notices.push(Notice::Error("boom".into()));
        t.sync_notice(now);
        assert_eq!(t.notice_deadline(), Some(now + NOTICE_TTL));
        t.sync_notice(now + Duration::from_secs(4));
        assert!(t.active_notice().is_some());
        t.sync_notice(now + NOTICE_TTL);
        assert_eq!(t.active_notice(), None);
        assert_eq!(t.notice_deadline(), None);
    }

    #[test]
    fn the_newest_notice_wins() {
        let mut t = tui();
        let now = Instant::now();
        t.core.notices.push(Notice::Info("first".into()));
        t.sync_notice(now);
        t.core.notices.push(Notice::Info("second".into()));
        t.core.notices.push(Notice::Info("third".into()));
        t.sync_notice(now);
        assert_eq!(t.active_notice(), Some(&Notice::Info("third".into())));
    }

    #[test]
    fn notices_that_were_never_shown_take_turns() {
        let mut t = tui();
        let now = Instant::now();
        t.core.notices.push(Notice::Info("version skew".into()));
        t.core.notices.push(Notice::Error("no organization".into()));
        t.sync_notice(now);
        assert_eq!(
            t.active_notice(),
            Some(&Notice::Error("no organization".into()))
        );
        t.sync_notice(now + NOTICE_TTL);
        assert_eq!(
            t.active_notice(),
            Some(&Notice::Info("version skew".into()))
        );
        assert_eq!(t.notice_deadline(), Some(now + NOTICE_TTL * 2));
        t.sync_notice(now + NOTICE_TTL * 2);
        assert_eq!(t.active_notice(), None);
    }

    #[test]
    fn copy_warnings_are_shown_as_info() {
        let mut t = tui();
        t.report_copy(CopyOutcome::CopiedWithWarning("check tmux".into()));
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Info("Copied. check tmux".into()))
        );
        t.report_copy(CopyOutcome::Failed("no display".into()));
        assert_eq!(
            t.core.notices.last(),
            Some(&Notice::Error("Copy failed: no display".into()))
        );
    }

    #[test]
    fn the_clipboard_is_not_created_at_startup() {
        let t = tui();
        assert!(t.clipboard.is_none());
    }

    #[test]
    fn without_keyboard_enhancement_the_placeholder_names_alt_enter() {
        let mut t = tui();
        assert!(
            t.composer
                .widget()
                .placeholder_text()
                .contains("Shift+Enter")
        );
        t.set_keyboard_enhanced(false);
        t.composer.set_text("");
        let placeholder = t.composer.widget().placeholder_text();
        assert!(
            placeholder.contains("Alt+Enter for a new line"),
            "{placeholder}"
        );
        assert!(!placeholder.contains("Shift+Enter"), "{placeholder}");
    }

    #[test]
    fn modifier_enter_without_enhancement_names_alt_enter_to_send() {
        let mut t = tui();
        t.core.prefs.send_shortcut = scuttle_core::density::SendShortcut::ModifierEnter;
        t.set_keyboard_enhanced(false);
        assert_eq!(
            t.composer.widget().placeholder_text(),
            "Message the agent. Alt+Enter to send, Enter for a new line, /help for commands."
        );
        t.set_keyboard_enhanced(true);
        assert!(
            t.composer
                .widget()
                .placeholder_text()
                .contains("Ctrl+Enter to send")
        );
    }

    #[test]
    fn loaded_prefs_update_the_placeholder() {
        let mut t = tui();
        t.set_keyboard_enhanced(false);
        let prefs = scuttle_core::density::DisplayPrefs {
            send_shortcut: scuttle_core::density::SendShortcut::ModifierEnter,
            ..Default::default()
        };
        t.update(Msg::PrefsLoaded(prefs));
        assert!(
            t.composer
                .widget()
                .placeholder_text()
                .contains("Alt+Enter to send")
        );
        // The Tui passes the enhancement state on, so Alt+Enter now sends.
        for c in "hi".chars() {
            t.handle(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::ALT));
        assert!(matches!(effects.as_slice(), [Effect::CreateChat { text, .. }] if text == "hi"));
    }

    #[test]
    fn ctrl_c_closes_the_help_box_and_a_second_press_quits() {
        let mut t = tui();
        t.apply_ui_effect(&Effect::ShowHelp);
        assert!(
            t.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL))
                .is_empty()
        );
        assert!(!t.show_help);
        assert_eq!(
            t.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            vec![Effect::Quit]
        );
    }

    #[test]
    fn help_scrolls_on_a_short_screen() {
        let mut t = tui();
        t.apply_ui_effect(&Effect::ShowHelp);
        let top = screen(&mut t, 60, 14);
        assert!(top.contains("/model"), "{top}");
        assert!(!top.contains("Ctrl+C twice"), "{top}");
        // Scroll to the bottom without depending on today's exact help length: keep pressing
        // Down until the screen stops changing, capped so a scrolling regression fails fast
        // instead of looping.
        let mut bottom = screen(&mut t, 60, 14);
        for _ in 0..200 {
            t.handle(key(KeyCode::Down, KeyModifiers::NONE));
            let next = screen(&mut t, 60, 14);
            if next == bottom {
                break;
            }
            bottom = next;
        }
        assert!(bottom.contains("Ctrl+C twice"), "{bottom}");
        assert!(t.show_help);
        t.handle(key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(!t.show_help);
        t.apply_ui_effect(&Effect::ShowHelp);
        assert!(
            screen(&mut t, 60, 14).contains("/model"),
            "reopening starts at the top"
        );
    }

    #[test]
    fn ctrl_c_closes_a_picker_and_a_second_press_quits() {
        let mut t = tui();
        t.core.update(Msg::ModelsLoaded(vec![]));
        t.apply_ui_effect(&Effect::ShowPicker(scuttle_core::app::Picker::Model));
        assert!(t.picker.is_some());
        assert!(
            t.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL))
                .is_empty()
        );
        assert!(t.picker.is_none());
        assert_eq!(
            t.handle(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            vec![Effect::Quit]
        );
    }

    fn tui_with_a_code_block() -> (Tui, u16) {
        let mut t = tui();
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap();
        t.core.update(Msg::ChatLoaded {
            chat: Box::new(chat),
            messages: serde_json::from_value(json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "```\necho hi\n```"}]}])).unwrap(),
        });
        let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
        term.draw(|f| t.draw(f)).unwrap();
        let row = t
            .row_of(|target| matches!(target, HitTarget::CopyCode(_)))
            .expect("code block on screen");
        (t, row)
    }

    fn click(t: &mut Tui, row: u16) {
        mouse(t, MouseEventKind::Down(MouseButton::Left), 2, row);
        mouse(t, MouseEventKind::Up(MouseButton::Left), 2, row);
    }

    fn mouse(t: &mut Tui, kind: MouseEventKind, column: u16, row: u16) {
        t.handle(Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }));
    }

    /// The screen column and row where `needle` first appears.
    fn find(shown: &str, needle: &str) -> (u16, u16) {
        use unicode_width::UnicodeWidthStr;
        shown
            .lines()
            .enumerate()
            .find_map(|(y, row)| {
                row.find(needle)
                    .map(|i| (row[..i].width() as u16, y as u16))
            })
            .unwrap_or_else(|| panic!("{needle:?} not on screen:\n{shown}"))
    }

    fn numbered_rows(t: &mut Tui) {
        let messages: Vec<_> = (1..=30)
            .map(|i| json!({"id": i, "role": "assistant", "content": [{"type": "text", "text": format!("row {i:02}")}]}))
            .collect();
        loaded(t, serde_json::Value::Array(messages));
    }

    fn reversed(t: &mut Tui, w: u16, h: u16, x: u16, y: u16) -> bool {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| t.draw(f)).unwrap();
        term.backend().buffer()[(x, y)]
            .modifier
            .contains(ratatui::style::Modifier::REVERSED)
    }

    /// Whether the first cell of `needle` on screen is highlighted, wherever it now is.
    fn highlighted(t: &mut Tui, w: u16, h: u16, needle: &str) -> bool {
        let (x, y) = find(&screen(t, w, h), needle);
        reversed(t, w, h, x, y)
    }

    /// Presses at `from`, drags to `to`, and releases there.
    fn drag(t: &mut Tui, from: (u16, u16), to: (u16, u16)) {
        mouse(t, MouseEventKind::Down(MouseButton::Left), from.0, from.1);
        mouse(t, MouseEventKind::Drag(MouseButton::Left), to.0, to.1);
        mouse(t, MouseEventKind::Up(MouseButton::Left), to.0, to.1);
    }

    #[test]
    fn dragging_selects_highlights_and_copies_on_release() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "alpha beta gamma"}]}]),
        );
        let shown = screen(&mut t, 60, 20);
        let (x, y) = find(&shown, "beta");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 9, y);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x + 9, y);
        assert_eq!(t.last_copied.as_deref(), Some("beta gamma"));
        assert!(reversed(&mut t, 60, 20, x, y));
        assert!(reversed(&mut t, 60, 20, x + 9, y));
        assert!(!reversed(&mut t, 60, 20, x + 10, y));
    }

    #[test]
    fn a_selection_across_wrapped_lines_copies_the_original_text() {
        let text = "one two three four five six seven eight nine ten eleven twelve";
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": text}]}]),
        );
        let shown = screen(&mut t, 24, 20);
        let (x0, y0) = find(&shown, "one two");
        let (x1, y1) = find(&shown, "twelve");
        assert!(
            y1 > y0 + 1,
            "the text wraps over at least three rows:\n{shown}"
        );
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x0, y0);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x1 + 5, y1);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x1 + 5, y1);
        assert_eq!(t.last_copied.as_deref(), Some(text));
    }

    #[test]
    fn a_selection_stays_on_its_text_when_scrolled() {
        let mut t = tui();
        numbered_rows(&mut t);
        let shown = screen(&mut t, 40, 24);
        let (x, y) = find(&shown, "row 26");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 5, y);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x + 5, y);
        assert_eq!(t.last_copied.as_deref(), Some("row 26"));
        mouse(&mut t, MouseEventKind::ScrollUp, x, y);
        let scrolled = screen(&mut t, 40, 24);
        assert_eq!(find(&scrolled, "row 26"), (x, y + 3), "{scrolled}");
        assert!(
            reversed(&mut t, 40, 24, x, y + 3),
            "the highlight moved with the text"
        );
        assert!(
            !reversed(&mut t, 40, 24, x, y),
            "and left the old screen row"
        );
    }

    #[test]
    fn streaming_during_a_drag_keeps_the_text_under_the_pointer() {
        let mut t = tui();
        numbered_rows(&mut t);
        let shown = screen(&mut t, 40, 24);
        let (x, y) = find(&shown, "row 26");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        for seq in 1..=10 {
            t.update(live_part(
                seq,
                json!({"type": "text", "text": "streamed line\n\n"}),
            ));
            let now = screen(&mut t, 40, 24);
            assert_eq!(find(&now, "row 26"), (x, y), "the text moved:\n{now}");
        }
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 5, y);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x + 5, y);
        assert_eq!(t.last_copied.as_deref(), Some("row 26"));
    }

    #[test]
    fn a_key_press_clears_the_selection() {
        let mut t = tui();
        numbered_rows(&mut t);
        let shown = screen(&mut t, 40, 24);
        let (x, y) = find(&shown, "row 26");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 5, y);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x + 5, y);
        assert!(reversed(&mut t, 40, 24, x, y));
        t.handle(key(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(!reversed(&mut t, 40, 24, x, y));
    }

    fn status(s: &str) -> Msg {
        stream(json!({"type": "status", "status": {"status": s}}))
    }

    #[test]
    fn a_drag_keeps_its_text_when_the_activity_row_appears() {
        let mut t = tui();
        numbered_rows(&mut t);
        let shown = screen(&mut t, 40, 24);
        let (x, y) = find(&shown, "row 26");
        let (bx, by) = find(&shown, "row 30");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        t.update(status("running"));
        let now = screen(&mut t, 40, 24);
        assert!(
            now.contains("Working…"),
            "the activity row appeared:\n{now}"
        );
        assert_eq!(find(&now, "row 26"), (x, y), "the text moved:\n{now}");
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 5, y);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x + 5, y);
        assert_eq!(t.last_copied.as_deref(), Some("row 26"));
        assert!(
            highlighted(&mut t, 40, 24, "row 26"),
            "the highlight is on the text"
        );

        // The press lands on the last transcript row, which the activity row then covers.
        let mut t = tui();
        numbered_rows(&mut t);
        assert_eq!(find(&screen(&mut t, 40, 24), "row 30"), (bx, by));
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), bx, by);
        t.update(status("running"));
        screen(&mut t, 40, 24);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), bx + 5, by);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), bx + 5, by);
        assert_eq!(t.last_copied.as_deref(), Some("row 30"));
    }

    #[test]
    fn a_drag_keeps_its_text_when_the_activity_row_disappears() {
        let mut t = tui();
        numbered_rows(&mut t);
        t.update(status("running"));
        let shown = screen(&mut t, 40, 24);
        assert!(shown.contains("Working…"), "{shown}");
        let (x, y) = find(&shown, "row 26");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        t.update(status("waiting"));
        let now = screen(&mut t, 40, 24);
        assert!(!now.contains("Working…"), "the activity row left:\n{now}");
        assert_eq!(find(&now, "row 26"), (x, y), "the text moved:\n{now}");
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 5, y);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x + 5, y);
        assert_eq!(t.last_copied.as_deref(), Some("row 26"));
        assert!(
            highlighted(&mut t, 40, 24, "row 26"),
            "the highlight is on the text"
        );
    }

    #[test]
    fn a_selection_across_a_rule_leaves_the_rule_out() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([
                {"id": 1, "role": "assistant", "content": [
                    {"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args": {"command": "ls"}}
                ]},
                {"id": 2, "role": "tool", "content": [
                    {"type": "tool-result", "tool_call_id": "a", "tool_name": "execute", "result": {"output": "ok"}}
                ]},
                {"id": 3, "role": "assistant", "content": [{"type": "text", "text": "All fixed."}]}
            ]),
        );
        let shown = screen(&mut t, 40, 20);
        let (x0, y0) = find(&shown, "execute");
        let (x1, y1) = find(&shown, "All fixed.");
        let (_, rule) = find(&shown, "────");
        assert!(y0 < rule && rule < y1, "{shown}");
        drag(&mut t, (x0, y0), (x1 + 9, y1));
        let copied = t.last_copied.clone().expect("copied");
        assert!(copied.starts_with("execute"), "{copied:?}");
        assert!(copied.ends_with("All fixed."), "{copied:?}");
        assert!(!copied.contains('─'), "the rule is left out: {copied:?}");
        assert!(
            !copied.contains("\n\n\n"),
            "the rule leaves one paragraph break: {copied:?}"
        );
        assert!(
            !reversed(&mut t, 40, 20, 20, rule),
            "the rule is not highlighted"
        );
    }

    #[test]
    fn a_click_acts_on_release_and_survives_a_jiggle() {
        let (mut t, row) = tui_with_a_code_block();
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), 2, row);
        assert_eq!(t.last_copied, None, "a press alone does nothing");
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), 2, row);
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), 2, row);
        assert_eq!(
            t.last_copied.as_deref(),
            Some("echo hi\n"),
            "a movement within the cell is still a click"
        );
    }

    #[test]
    fn a_click_toggles_a_tool_block() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [
                {"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args": {"command": "ls"}}
            ]}]),
        );
        screen(&mut t, 60, 20);
        let row = t
            .row_of(|target| matches!(target, HitTarget::Toggle(_)))
            .expect("tool block on screen");
        click(&mut t, row);
        assert_eq!(t.toggles.len(), 1, "the click toggled the block");
        click(&mut t, row);
        assert!(t.toggles.is_empty(), "a second click toggles it back");
    }

    #[test]
    fn an_empty_selection_copies_nothing() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "short"}]}]),
        );
        let shown = screen(&mut t, 60, 20);
        let (_, y) = find(&shown, "short");
        let notices = t.core.notices.len();
        drag(&mut t, (40, y), (50, y));
        assert_eq!(t.last_copied, None);
        assert_eq!(t.core.notices.len(), notices, "no notice");
    }

    #[test]
    fn with_the_mouse_off_nothing_is_selected() {
        let mut t = tui();
        numbered_rows(&mut t);
        let shown = screen(&mut t, 40, 24);
        let (x, y) = find(&shown, "row 26");
        t.update(Msg::Command(scuttle_core::commands::Command::Mouse));
        assert!(!t.core.mouse);
        drag(&mut t, (x, y), (x + 5, y));
        assert_eq!(t.last_copied, None);
        assert!(!reversed(&mut t, 40, 24, x, y));
    }

    #[test]
    fn turning_the_mouse_off_mid_drag_releases_the_view() {
        let mut t = tui();
        numbered_rows(&mut t);
        let shown = screen(&mut t, 40, 24);
        let (x, y) = find(&shown, "row 26");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 5, y);
        for e in t.update(Msg::Command(scuttle_core::commands::Command::Mouse)) {
            t.apply_ui_effect(&e);
        }
        mouse(&mut t, MouseEventKind::Up(MouseButton::Left), x + 5, y);
        assert_eq!(t.last_copied, None, "the drag ended with the capture");
        assert!(!reversed(&mut t, 40, 24, x, y));
    }

    #[test]
    fn clicks_are_ignored_while_an_overlay_is_showing() {
        let (mut t, row) = tui_with_a_code_block();
        t.apply_ui_effect(&Effect::ShowHelp);
        click(&mut t, row);
        assert_eq!(t.last_copied, None, "help box");
        t.show_help = false;
        t.core.update(Msg::ModelsLoaded(vec![]));
        t.apply_ui_effect(&Effect::ShowPicker(scuttle_core::app::Picker::Model));
        click(&mut t, row);
        assert_eq!(t.last_copied, None, "picker");
        t.picker = None;
        t.composer.set_text("/co");
        click(&mut t, row);
        assert_eq!(t.last_copied, None, "slash menu");
        t.composer.set_text("");
        click(&mut t, row);
        assert!(t.last_copied.is_some());
    }

    #[test]
    fn the_editor_file_is_private_new_and_removed_afterwards() {
        let mut t = tui();
        t.composer.set_text("draft");
        let mut seen = None;
        t.edit_with(|path| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(path)?.permissions().mode();
                assert_eq!(mode & 0o777, 0o600);
            }
            assert_eq!(std::fs::read_to_string(path)?, "draft");
            seen = Some(path.to_owned());
            Ok(())
        })
        .unwrap();
        assert!(!seen.unwrap().exists());
    }

    #[test]
    fn the_editor_round_trip_resumes_even_when_leaving_fails() {
        let mut ran = false;
        let mut resumed = false;
        let (edited, resume) = editor_round_trip(
            || Err(std::io::Error::other("leave failed")),
            || {
                ran = true;
                Ok(())
            },
            || {
                resumed = true;
                Ok(())
            },
        );
        assert!(edited.is_err());
        assert!(resume.is_ok());
        assert!(!ran, "the editor must not run on a half-restored terminal");
        assert!(resumed);
    }

    #[test]
    fn a_failed_resume_quits_with_an_error() {
        let mut t = tui();
        let effects = t.finish_editor(Err(std::io::Error::other("tty gone")));
        assert_eq!(effects, vec![Effect::Quit]);
        assert!(t.take_fatal().unwrap().contains("tty gone"));
        assert!(t.finish_editor(Ok(())).is_empty());
        assert_eq!(t.take_fatal(), None);
    }

    #[test]
    fn returning_from_the_editor_requests_a_full_redraw() {
        let mut t = tui();
        assert!(!t.take_full_redraw());
        t.edit_with(|path| std::fs::write(path, "edited\n"))
            .unwrap();
        assert_eq!(t.composer.text(), "edited");
        assert!(t.take_full_redraw());
        assert!(!t.take_full_redraw());
    }

    fn screen_at(t: &mut Tui, w: u16, h: u16, now: Instant) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| t.draw_at(f, now)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Loads a chat with `messages` into the Tui.
    fn loaded(t: &mut Tui, messages: serde_json::Value) {
        let chat = serde_json::from_value(json!({"id": uuid::Uuid::new_v4(), "children": [], "files": [], "mcp_server_ids": [], "inline_mcp_servers": [], "labels": {}})).unwrap();
        t.update(Msg::ChatLoaded {
            chat: Box::new(chat),
            messages: serde_json::from_value(messages).unwrap(),
        });
    }

    fn stream(v: serde_json::Value) -> Msg {
        Msg::Stream(coder_sdk::StreamEvent {
            kind: coder_sdk::StreamEventType::parse(v["type"].as_str().unwrap_or_default()),
            event: serde_json::from_value(v.clone()).ok(),
            raw: v,
        })
    }

    fn live_part(seq: i64, part: serde_json::Value) -> Msg {
        stream(
            json!({"type": "message_part", "message_part": {"history_version": 1, "generation_attempt": 1, "seq": seq, "part": part}}),
        )
    }

    #[test]
    fn the_spinner_shows_from_submit_and_animates() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        loaded(&mut t, json!([]));
        let now = Instant::now();
        assert_eq!(t.animation_deadline(now), None, "no timer while idle");
        for c in "hi".chars() {
            t.handle(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            t.animation_deadline(now),
            Some(now + crate::activity::SPINNER_INTERVAL)
        );
        let first = screen_at(&mut t, 60, 16, now);
        assert!(first.contains("Waiting for the agent…"), "{first}");
        let later = screen_at(&mut t, 60, 16, now + crate::activity::SPINNER_INTERVAL);
        assert_ne!(first, later, "the spinner advanced");
    }

    #[test]
    fn the_spinner_keeps_going_while_the_agent_streams() {
        let mut t = tui();
        loaded(&mut t, json!([]));
        t.update(stream(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        assert!(screen(&mut t, 60, 16).contains("Working…"));
        t.update(live_part(1, json!({"type": "reasoning", "text": "hmm"})));
        assert!(screen(&mut t, 60, 16).contains("Thinking…"));
        t.update(live_part(2, json!({"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args_delta": "{"})));
        assert!(screen(&mut t, 60, 16).contains("Running execute…"));
        t.update(live_part(3, json!({"type": "text", "text": "Done"})));
        assert!(screen(&mut t, 60, 16).contains("Writing…"));
        t.update(stream(
            json!({"type": "status", "status": {"status": "waiting"}}),
        ));
        let idle = screen(&mut t, 60, 16);
        assert!(
            !idle.contains("Writing…") && !idle.contains("Working…"),
            "{idle}"
        );
        assert_eq!(t.animation_deadline(Instant::now()), None);
    }

    #[test]
    fn a_spinner_tick_reuses_the_transcript_lines() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "hello"}]}]),
        );
        t.update(stream(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        let now = Instant::now();
        screen_at(&mut t, 60, 16, now);
        assert_eq!(t.view_builds, 1);
        t.tick();
        let ticked = screen_at(&mut t, 60, 16, now + crate::activity::SPINNER_INTERVAL);
        assert_eq!(
            t.view_builds, 1,
            "a timer frame must not rebuild the transcript"
        );
        assert!(
            ticked.contains("hello") && ticked.contains("Working…"),
            "{ticked}"
        );
        t.update(Msg::Refresh);
        screen_at(&mut t, 60, 16, now);
        assert_eq!(t.view_builds, 2, "a message rebuilds");
        t.tick();
        screen_at(&mut t, 70, 16, now);
        assert_eq!(t.view_builds, 3, "a resize rebuilds even on a timer frame");
    }
    #[test]
    fn the_tint_fills_the_row_on_light_and_dark_terminals() {
        use ratatui::style::Color;
        for dark in [true, false] {
            let theme = Theme::terminal(dark);
            let mut t = Tui::new(
                scuttle_core::config::LocalConfig::default(),
                None,
                theme,
                Welcome {
                    url: "https://x".into(),
                    user: "nick".into(),
                    art: vec![],
                    show: true,
                },
            );
            loaded(
                &mut t,
                json!([{"id": 1, "role": "user", "content": [{"type": "text", "text": "my question"}]}]),
            );
            let (w, h) = (50u16, 12u16);
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| t.draw(f)).unwrap();
            let buf = term.backend().buffer().clone();
            let y = (0..h)
                .find(|&y| {
                    (0..w)
                        .map(|x| buf[(x, y)].symbol())
                        .collect::<String>()
                        .contains("my question")
                })
                .unwrap();
            let tint = theme.user_tint.bg.unwrap();
            for x in 1..w - 1 {
                assert_eq!(buf[(x, y)].bg, tint, "dark={dark} x={x}");
            }
            assert_ne!(buf[(0, y)].bg, tint, "the margin stays untinted");
            let marker = (0..w).find(|&x| buf[(x, y)].symbol() == "›").unwrap();
            assert_eq!(
                buf[(marker, y)].fg,
                Color::Blue,
                "the marker keeps its color"
            );
        }
        assert_eq!(
            Theme::terminal(false).user_tint.bg,
            Some(Color::Indexed(254))
        );
        assert_eq!(
            Theme::terminal(true).user_tint.bg,
            Some(Color::Indexed(236))
        );
    }

    #[test]
    fn the_tint_covers_wrapped_rows_and_nothing_else() {
        let mut t = tui();
        let long = "wrapped words ".repeat(6);
        loaded(
            &mut t,
            json!([
                {"id": 1, "role": "user", "content": [{"type": "text", "text": long.trim()}]},
                {"id": 2, "role": "assistant", "content": [{"type": "text", "text": "Answer."}]}
            ]),
        );
        let (w, h) = (30u16, 16u16);
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| t.draw(f)).unwrap();
        let buf = term.backend().buffer().clone();
        let tint = t.theme.user_tint.bg.unwrap();
        let tinted: Vec<u16> = (0..h).filter(|&y| buf[(w / 2, y)].bg == tint).collect();
        assert!(
            tinted.len() >= 2,
            "the continuation row is tinted too: {tinted:?}"
        );
        for &y in &tinted {
            assert_ne!(buf[(0, y)].bg, tint, "left margin");
            assert_ne!(buf[(w - 1, y)].bg, tint, "right margin");
        }
        let answer = (0..h)
            .find(|&y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .contains("Answer.")
            })
            .unwrap();
        assert!(!tinted.contains(&answer));
    }

    #[test]
    fn choosing_an_organization_saves_it_to_the_config() {
        let dir = std::env::temp_dir().join(format!("scuttle-org-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        let mut t = Tui::new(
            scuttle_core::config::LocalConfig::default(),
            Some(path.clone()),
            Theme::terminal(true),
            Welcome {
                url: "https://x".into(),
                user: String::new(),
                art: vec![],
                show: true,
            },
        );
        let id = uuid::Uuid::new_v4();
        assert!(t.apply_ui_effect(&Effect::SaveOrganization(id)));
        assert_eq!(
            scuttle_core::config::load(&path).unwrap().organization,
            Some(id)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_organization_picker_choice_reaches_the_core() {
        use scuttle_core::app::OrgRef;
        let mut t = tui();
        let org = |name: &str, is_default| OrgRef {
            id: uuid::Uuid::new_v4(),
            name: name.to_lowercase(),
            display_name: name.into(),
            is_default,
            can_create_chats: true,
        };
        let (product, coder) = (org("Product", false), org("Coder", true));
        t.core.update(Msg::OrganizationsLoaded(vec![
            product.clone(),
            coder.clone(),
        ]));
        t.core.update(Msg::Started {
            org_id: coder.id,
            open_chat: None,
        });
        let effects = t
            .core
            .update(Msg::Command(scuttle_core::commands::Command::Organization(
                None,
            )));
        for e in &effects {
            assert!(t.apply_ui_effect(e), "{e:?}");
        }
        assert!(t.picker.is_some());
        t.handle(key(KeyCode::Up, KeyModifiers::NONE));
        let effects = t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(t.picker.is_none());
        assert!(
            effects.contains(&Effect::SaveOrganization(product.id)),
            "{effects:?}"
        );
        assert_eq!(t.core.org_id, Some(product.id));
    }

    #[test]
    fn slash_new_clears_the_view_and_keeps_the_draft() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "old answer"}]}]),
        );
        t.toggles.insert((Some(1), 0));
        assert!(screen(&mut t, 60, 16).contains("old answer"));
        t.composer.set_text("draft to keep");
        let effects = t.update(Msg::Command(scuttle_core::commands::Command::New));
        for e in &effects {
            t.apply_ui_effect(e);
        }
        assert!(effects.contains(&Effect::CloseStream));
        assert!(t.toggles.is_empty());
        assert_eq!(t.composer.text(), "draft to keep");
        let shown = screen(&mut t, 60, 16);
        assert!(!shown.contains("old answer"), "{shown}");
        assert!(
            shown.contains("scuttle"),
            "the welcome screen is back: {shown}"
        );
    }

    #[test]
    fn slash_new_ends_a_held_drag_and_drops_the_selection_and_scroll() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        numbered_rows(&mut t);
        let shown = screen(&mut t, 40, 24);
        let (x, y) = find(&shown, "row 26");
        drag(&mut t, (x, y), (x + 5, y));
        assert!(t.selection.is_some());
        mouse(&mut t, MouseEventKind::ScrollUp, x, y);
        assert!(t.scroll_from_bottom > 0);
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        assert!(t.drag.is_some(), "the button is held");
        let effects = t.update(Msg::Command(scuttle_core::commands::Command::New));
        for e in &effects {
            t.apply_ui_effect(e);
        }
        assert!(t.drag.is_none());
        assert!(t.selection.is_none());
        assert_eq!(t.scroll_from_bottom, 0);
        let shown = screen(&mut t, 40, 24);
        assert!(!shown.contains("row 26"), "{shown}");
    }

    #[test]
    fn a_resize_drops_the_selection_and_ends_a_held_drag() {
        let mut t = tui();
        numbered_rows(&mut t);
        let shown = screen(&mut t, 40, 24);
        let (x, y) = find(&shown, "row 26");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 5, y);
        screen(&mut t, 40, 24);
        assert!(t.selection.is_some(), "a redraw at the same width keeps it");
        assert!(t.drag.is_some());
        screen(&mut t, 50, 24);
        assert!(t.selection.is_none());
        assert!(t.drag.is_none());
    }

    #[test]
    fn a_history_reset_drops_the_selection_and_ends_a_held_drag() {
        let mut t = tui();
        numbered_rows(&mut t);
        let shown = screen(&mut t, 40, 24);
        let (x, y) = find(&shown, "row 26");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 5, y);
        t.update(stream(json!({"type": "history_reset"})));
        t.update(stream(
            json!({"type": "message", "message": {"id": 1, "role": "assistant", "content": [{"type": "text", "text": "replaced"}]}}),
        ));
        screen(&mut t, 40, 24);
        assert!(
            t.selection.is_some(),
            "the replacement is buffered, so nothing changed yet"
        );
        t.update(stream(
            json!({"type": "status", "status": {"status": "waiting"}}),
        ));
        let shown = screen(&mut t, 40, 24);
        assert!(shown.contains("replaced"), "{shown}");
        assert!(t.selection.is_none());
        assert!(t.drag.is_none());
    }

    #[test]
    fn a_web_url_that_opened_no_browser_is_copied() {
        let mut t = tui();
        let url = "https://coder.example.com/agents/x".to_owned();
        assert!(t.apply_ui_effect(&Effect::CopyWebUrl(url.clone())));
        assert_eq!(t.last_copied.as_deref(), Some(url.as_str()));
    }

    #[test]
    fn the_web_url_copy_notice_names_the_url() {
        let url = "https://coder.example.com/agents/x";
        assert_eq!(
            web_copy_notice(url, CopyOutcome::Copied),
            Notice::Info("Copied the chat URL: https://coder.example.com/agents/x".into())
        );
        assert_eq!(
            web_copy_notice(
                url,
                CopyOutcome::CopiedWithWarning("tmux may drop it".into())
            ),
            Notice::Info(
                "Copied the chat URL: https://coder.example.com/agents/x. tmux may drop it".into()
            )
        );
        assert_eq!(
            web_copy_notice(url, CopyOutcome::Failed("no terminal".into())),
            Notice::Error("Could not copy the chat URL: https://coder.example.com/agents/x".into())
        );
    }

    fn release(t: &mut Tui, column: u16, row: u16) -> Vec<Effect> {
        t.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }))
    }

    fn open_docs() -> Vec<Effect> {
        vec![Effect::OpenLink("https://coder.com/docs".into())]
    }

    #[test]
    fn a_click_on_a_link_opens_it_and_a_drag_over_it_selects_it() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "See [the docs](https://coder.com/docs) here."}]}]),
        );
        let (x, y) = find(&screen(&mut t, 60, 20), "the docs");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x + 2, y);
        assert_eq!(release(&mut t, x + 2, y), open_docs());
        assert_eq!(t.last_copied, None);
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        mouse(&mut t, MouseEventKind::Drag(MouseButton::Left), x + 7, y);
        assert!(
            release(&mut t, x + 7, y).is_empty(),
            "a drag never opens the link"
        );
        assert_eq!(t.last_copied.as_deref(), Some("the docs"));
        let (x, y) = find(&screen(&mut t, 60, 20), "here.");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        assert!(
            release(&mut t, x, y).is_empty(),
            "the text beside a link is not the link"
        );
    }

    #[test]
    fn a_click_on_the_second_row_of_a_wrapped_link_opens_it() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "Read [the very long documentation title here](https://coder.com/docs) please"}]}]),
        );
        let shown = screen(&mut t, 22, 20);
        let (x, y) = find(&shown, "documentation");
        let (_, first) = find(&shown, "Read");
        assert!(y > first, "the link wraps onto a second row:\n{shown}");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x + 3, y);
        assert_eq!(release(&mut t, x + 3, y), open_docs());
        let (x, y) = find(&shown, "please");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        assert!(release(&mut t, x, y).is_empty());
    }

    #[test]
    fn a_link_click_while_streaming_opens_the_link_under_the_press() {
        let mut t = tui();
        numbered_rows(&mut t);
        t.update(stream(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        t.update(live_part(
            1,
            json!({"type": "text", "text": "See [the docs](https://coder.com/docs)."}),
        ));
        let (x, y) = find(&screen(&mut t, 60, 16), "the docs");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        t.update(live_part(
            2,
            json!({"type": "text", "text": "\n\nMore.\n\nAnd more.\n\nStill more."}),
        ));
        let during = screen(&mut t, 60, 16);
        assert_eq!(
            find(&during, "the docs"),
            (x, y),
            "the held press pins the view:\n{during}"
        );
        assert_eq!(release(&mut t, x, y), open_docs());
    }

    #[test]
    fn link_text_in_a_code_block_copies_the_code_instead() {
        let mut t = tui();
        loaded(
            &mut t,
            json!([{"id": 1, "role": "assistant", "content": [{"type": "text", "text": "```\n[the docs](https://coder.com/docs)\n```"}]}]),
        );
        let (x, y) = find(&screen(&mut t, 60, 20), "the docs");
        mouse(&mut t, MouseEventKind::Down(MouseButton::Left), x, y);
        assert!(release(&mut t, x, y).is_empty(), "code is never a link");
        assert_eq!(
            t.last_copied.as_deref(),
            Some("[the docs](https://coder.com/docs)\n")
        );
    }

    #[test]
    fn a_link_that_opened_no_browser_is_copied() {
        let mut t = tui();
        let url = "https://coder.com/docs".to_owned();
        assert!(!t.apply_ui_effect(&Effect::OpenLink(url.clone())));
        assert!(t.apply_ui_effect(&Effect::CopyLink(url.clone())));
        assert_eq!(t.last_copied.as_deref(), Some(url.as_str()));
        assert_eq!(
            copy_url_notice("the link", &url, CopyOutcome::Copied),
            Notice::Info("Copied the link: https://coder.com/docs".into())
        );
    }

    #[test]
    fn a_link_that_is_not_a_web_link_says_why_it_was_copied() {
        assert_eq!(
            link_copy_notice("https://coder.com/docs", CopyOutcome::Copied),
            Notice::Info("Copied the link: https://coder.com/docs".into()),
            "a web link that opened no browser needs no reason"
        );
        for url in ["file:///etc/passwd", "javascript:alert(1)", "-a Calculator"] {
            assert_eq!(
                link_copy_notice(url, CopyOutcome::Copied),
                Notice::Info(format!(
                    "Only http and https links open in a browser. Copied the link: {url}"
                )),
                "{url}"
            );
        }
        assert_eq!(
            link_copy_notice("file:///x", CopyOutcome::Failed("no terminal".into())),
            Notice::Error(
                "Only http and https links open in a browser. Could not copy the link: file:///x"
                    .into()
            )
        );
    }

    /// The first transcript row containing `needle` that is not the activity row, and its
    /// first visible character.
    fn marker_of(shown: &str, needle: &str) -> char {
        shown
            .lines()
            .find(|l| l.contains(needle) && !l.contains('…'))
            .and_then(|l| l.trim_start().chars().next())
            .unwrap_or_else(|| panic!("{needle:?} not on screen:\n{shown}"))
    }

    #[test]
    fn the_thinking_line_animates_with_the_spinner() {
        let mut t = tui();
        loaded(&mut t, json!([]));
        t.update(stream(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        t.update(live_part(1, json!({"type": "reasoning", "text": "hmm"})));
        let start = t.epoch;
        let first = screen_at(&mut t, 60, 16, start);
        assert_eq!(
            marker_of(&first, " Thinking").to_string(),
            crate::activity::spinner_frame(Duration::ZERO)
        );
        t.tick();
        let later = screen_at(&mut t, 60, 16, start + crate::activity::SPINNER_INTERVAL);
        assert_eq!(
            marker_of(&later, " Thinking").to_string(),
            crate::activity::spinner_frame(crate::activity::SPINNER_INTERVAL)
        );
        assert_eq!(t.view_builds, 1, "the frame came from a reused view");
        t.update(live_part(2, json!({"type": "text", "text": "Done."})));
        let answered = screen_at(&mut t, 60, 16, start);
        assert_eq!(
            marker_of(&answered, " Thinking"),
            '∴',
            "a finished thought stops"
        );
    }

    #[test]
    fn animating_a_long_transcript_reuses_its_lines() {
        let mut t = tui();
        let messages: Vec<_> = (1..=500)
            .map(|i| json!({"id": i, "role": "assistant", "content": [{"type": "text", "text": format!("message {i} with **markdown** and `code`")}]}))
            .collect();
        loaded(&mut t, serde_json::Value::Array(messages));
        t.update(stream(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        t.update(live_part(
            1,
            json!({"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args_delta": "{\"command\": \"make\"}"}),
        ));
        let start = t.epoch;
        let first = screen_at(&mut t, 60, 16, start);
        assert_eq!(t.view_builds, 1);
        assert_eq!(
            t.view.spinners.len(),
            1,
            "one marker animates, however long the chat"
        );
        assert_eq!(
            marker_of(&first, "execute(").to_string(),
            crate::activity::spinner_frame(Duration::ZERO)
        );
        for step in 1..=3u32 {
            t.tick();
            let at = crate::activity::SPINNER_INTERVAL * step;
            let shown = screen_at(&mut t, 60, 16, start + at);
            assert_eq!(
                marker_of(&shown, "execute(").to_string(),
                crate::activity::spinner_frame(at)
            );
        }
        assert_eq!(
            t.view_builds, 1,
            "timer frames never rebuild the transcript"
        );
    }

    #[test]
    fn copying_an_animated_row_copies_its_static_marker() {
        let mut t = tui();
        loaded(&mut t, json!([]));
        t.update(stream(
            json!({"type": "status", "status": {"status": "running"}}),
        ));
        t.update(live_part(
            1,
            json!({"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args_delta": "{\"command\": \"make\"}"}),
        ));
        let start = t.epoch;
        let shown = screen_at(&mut t, 60, 16, start);
        let (x, y) = find(&shown, "execute(make)");
        assert_eq!(
            marker_of(&shown, "execute(").to_string(),
            crate::activity::spinner_frame(Duration::ZERO),
            "the marker is animating"
        );
        drag(&mut t, (x - 2, y), (x + 12, y));
        assert_eq!(t.last_copied.as_deref(), Some("◌ execute(make)"));
    }

    #[test]
    fn a_sent_message_stills_an_unfinished_call_of_the_turn_before() {
        let mut t = tui();
        t.core.update(Msg::Started {
            org_id: uuid::Uuid::new_v4(),
            open_chat: None,
        });
        loaded(
            &mut t,
            json!([
                {"id": 1, "role": "user", "content": [{"type": "text", "text": "build it"}]},
                {"id": 2, "role": "assistant", "content": [
                    {"type": "tool-call", "tool_call_id": "a", "tool_name": "execute", "args": {"command": "make"}}
                ]}
            ]),
        );
        for c in "again".chars() {
            t.handle(key(KeyCode::Char(c), KeyModifiers::NONE));
        }
        t.handle(key(KeyCode::Enter, KeyModifiers::NONE));
        let now = Instant::now();
        assert!(t.animation_deadline(now).is_some(), "the agent is awaited");
        let shown = screen_at(&mut t, 60, 16, now);
        assert!(shown.contains("Waiting for the agent…"), "{shown}");
        assert_eq!(marker_of(&shown, "execute(make)"), '◌');
        assert!(t.view.spinners.is_empty());
    }
}
